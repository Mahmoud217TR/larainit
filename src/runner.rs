use crate::error::{Error, diagnostic};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 64 * 1024;
const PROBE_DEADLINE: Duration = Duration::from_secs(5);
const REAP_DEADLINE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);

#[cfg(test)]
use crate::test_support;

pub struct Signals {
    received: Arc<AtomicUsize>,
    registrations: Vec<signal_hook::SigId>,
}

impl Signals {
    pub fn register() -> Result<Self, Error> {
        let mut signals = Self {
            received: Arc::new(AtomicUsize::new(0)),
            registrations: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            let id = signal_hook::flag::register_usize(
                signal,
                Arc::clone(&signals.received),
                signal as usize,
            )
            .map_err(|cause| {
                Error::operational(
                    "register interruption handler",
                    "SIGINT/SIGTERM",
                    cause,
                    "Check operating-system signal support",
                )
            })?;
            signals.registrations.push(id);
        }
        Ok(signals)
    }

    pub fn signal(&self) -> Option<Signal> {
        match self.received.load(Ordering::Relaxed) {
            2 => Some(Signal::SIGINT),
            15 => Some(Signal::SIGTERM),
            _ => None,
        }
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.registrations.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

#[derive(Debug)]
pub enum ProbeFailure {
    Launch(io::Error),
    Denied(io::Error),
    Io(io::Error),
    Overflow,
    Timeout,
    Interrupted(Signal),
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Launch(error) => write!(f, "cannot launch observation client: {error}"),
            Self::Denied(error) => write!(f, "observation access denied: {error}"),
            Self::Io(error) => write!(f, "observation I/O failed: {error}"),
            Self::Overflow => write!(
                f,
                "observation exceeded its combined capture budget (at most 64 KiB)"
            ),
            Self::Timeout => write!(f, "observation exceeded its deadline"),
            Self::Interrupted(signal) => write!(f, "observation interrupted by {signal}"),
        }
    }
}

#[derive(Debug)]
pub struct Observation {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn nonblocking(pipe: &impl AsFd) -> Result<(), ProbeFailure> {
    let current = fcntl(pipe, FcntlArg::F_GETFL).map_err(|error| ProbeFailure::Io(error.into()))?;
    fcntl(
        pipe,
        FcntlArg::F_SETFL(OFlag::from_bits_retain(current) | OFlag::O_NONBLOCK),
    )
    .map_err(|error| ProbeFailure::Io(error.into()))?;
    Ok(())
}

fn drain(
    pipe: &mut impl Read,
    output: &mut Vec<u8>,
    eof: &mut bool,
    total: &mut usize,
    limit: usize,
) -> Result<(), ProbeFailure> {
    if *eof {
        return Ok(());
    }
    let mut buffer = [0; 4096];
    match pipe.read(&mut buffer) {
        Ok(0) => *eof = true,
        Ok(size) => {
            if size > limit.saturating_sub(*total) {
                return Err(ProbeFailure::Overflow);
            }
            *total += size;
            output.extend_from_slice(&buffer[..size]);
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) => {}
        Err(error) => return Err(ProbeFailure::Io(error)),
    }
    Ok(())
}

pub fn terminate_client(child: &mut Child) -> Result<ExitStatus, io::Error> {
    // The process group belongs to this freshly spawned client, never the wrapper's group.
    let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
    let _ = child.kill();
    let deadline = Instant::now() + REAP_DEADLINE;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "client could not be reaped within two seconds; cleanup is best effort",
            ));
        }
        thread::sleep(POLL);
    }
}

fn finish_interrupted_client(child: &mut Child, signal: Signal) -> Result<ExitStatus, io::Error> {
    let start = Instant::now();
    let mut signaled = false;
    let mut killed = false;
    loop {
        // Retain even an exited leader until descendants have received the final group signal.
        if killed {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
        }
        let elapsed = start.elapsed();
        if elapsed >= REAP_DEADLINE {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "interrupted client could not be reaped within two seconds",
            ));
        }
        // Let the stopped container's real status reach the client before forcing termination.
        if !signaled && elapsed >= Duration::from_millis(100) {
            let _ = killpg(Pid::from_raw(child.id() as i32), signal);
            signaled = true;
        }
        if !killed && elapsed >= Duration::from_secs(1) {
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            let _ = child.kill();
            killed = true;
        }
        thread::sleep(POLL);
    }
}

pub fn observe(
    command: &mut Command,
    signals: Option<&Signals>,
) -> Result<Observation, ProbeFailure> {
    observe_with_deadline(command, signals, PROBE_DEADLINE)
}

pub fn observe_with_deadline(
    command: &mut Command,
    signals: Option<&Signals>,
    timeout: Duration,
) -> Result<Observation, ProbeFailure> {
    observe_with_limits(command, signals, timeout, OUTPUT_LIMIT)
}

pub fn observe_with_limits(
    command: &mut Command,
    signals: Option<&Signals>,
    timeout: Duration,
    output_limit: usize,
) -> Result<Observation, ProbeFailure> {
    let output_limit = output_limit.min(OUTPUT_LIMIT);
    if let Some(signal) = signals.and_then(Signals::signal) {
        return Err(ProbeFailure::Interrupted(signal));
    }
    if timeout.is_zero() {
        return Err(ProbeFailure::Timeout);
    }
    if output_limit == 0 {
        return Err(ProbeFailure::Overflow);
    }
    let deadline = Instant::now() + timeout;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            ProbeFailure::Denied(error)
        } else {
            ProbeFailure::Launch(error)
        }
    })?;
    let result = (|| {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProbeFailure::Io(io::Error::other("stdout pipe unavailable")))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| ProbeFailure::Io(io::Error::other("stderr pipe unavailable")))?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let mut observation = (Vec::new(), Vec::new());
        let (mut out_eof, mut err_eof, mut total) = (false, false, 0);
        loop {
            if let Some(signal) = signals.and_then(Signals::signal) {
                return Err(ProbeFailure::Interrupted(signal));
            }
            if Instant::now() >= deadline {
                return Err(ProbeFailure::Timeout);
            }
            drain(
                &mut stdout,
                &mut observation.0,
                &mut out_eof,
                &mut total,
                output_limit,
            )?;
            drain(
                &mut stderr,
                &mut observation.1,
                &mut err_eof,
                &mut total,
                output_limit,
            )?;
            // Keep the leader unreaped while retained pipes may still require group cleanup.
            // Reaping earlier releases its PID for reuse by an unrelated process group.
            if out_eof && err_eof {
                if let Some(status) = child.try_wait().map_err(ProbeFailure::Io)? {
                    return Ok(Observation {
                        status,
                        stdout: observation.0,
                        stderr: observation.1,
                    });
                }
            }
            thread::sleep(POLL);
        }
    })();
    // Pipes have been closed even when a descendant retained their write ends.
    if result.is_err() {
        if let Err(error) = terminate_client(&mut child) {
            diagnostic(format_args!("observation cleanup: {error}"));
        }
    }
    result
}

#[derive(Debug)]
pub struct Identity {
    pub uid: u32,
    pub gid: u32,
    pub groups: Vec<u32>,
}

impl Identity {
    pub fn current() -> Result<Self, Error> {
        let groups = nix::unistd::getgroups().map_err(|cause| {
            Error::operational(
                "read supplementary groups",
                "invoking user",
                cause,
                "Check operating-system group access",
            )
        })?;
        Ok(Self {
            uid: nix::unistd::getuid().as_raw(),
            gid: nix::unistd::getgid().as_raw(),
            groups: groups
                .into_iter()
                .map(|gid| gid.as_raw())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        })
    }
}

pub fn workspace() -> Result<PathBuf, Error> {
    let original = std::env::current_dir().map_err(|cause| {
        Error::operational(
            "resolve workspace",
            "invocation directory",
            cause,
            "Invoke from an accessible project directory",
        )
    })?;
    let path = original.canonicalize().map_err(|cause| {
        Error::operational(
            "resolve workspace",
            original.display().to_string(),
            cause,
            "Check project directory access",
        )
    })?;
    match crate::config::observe_directory(&path) {
        crate::config::PathState::Usable => {
            mount(&path, "/workspace")?;
            Ok(path)
        }
        state => Err(Error::operational(
            "inspect workspace",
            path.display().to_string(),
            format!("directory is not usable: {state:?}"),
            "Use an accessible writable project directory; no ownership or labeling changes are made",
        )),
    }
}

pub fn mount(path: &Path, target: &str) -> Result<OsString, Error> {
    let source = path.to_str().ok_or_else(|| {
        Error::operational(
            "encode bind mount",
            format!("{:?}", path.as_os_str()),
            "non-UTF-8 path cannot be transported losslessly",
            "Use a UTF-8 path; Larainit does not rewrite paths",
        )
    })?;
    if !path.is_absolute() || source.contains(['\r', '\n', '\0']) || source.trim() != source {
        return Err(Error::operational(
            "encode bind mount",
            format!("{path:?}"),
            "relative, CR/LF/NUL or edge-whitespace path cannot be transported safely",
            "Use an absolute representable path; no path is rewritten",
        ));
    }
    Ok(format!(
        "type=bind,\"src={}\",dst={target}",
        source.replace('"', "\"\"")
    )
    .into())
}

pub fn validate_tool(program: &OsStr, args: &[OsString]) -> Result<(), Error> {
    if program.is_empty()
        || std::iter::once(program)
            .chain(args.iter().map(OsString::as_os_str))
            .any(|value| value.to_str().is_none_or(|value| value.contains('\0')))
    {
        return Err(Error::operational(
            "encode tool command",
            "Docker JSON transport",
            "empty executable or non-UTF-8/NUL executable/argument cannot be transported losslessly",
            "Use representable UTF-8 arguments; command values are not printed or rewritten",
        ));
    }
    Ok(())
}

pub struct RunInput<'a> {
    pub workspace: &'a Path,
    pub composer: &'a Path,
    pub npm: &'a Path,
    pub identity: &'a Identity,
    pub image_id: &'a str,
    pub program: &'a OsStr,
    pub args: &'a [OsString],
    pub terminal: bool,
    pub name: &'a str,
    pub cidfile: &'a Path,
}

fn run_arguments_for(
    kind: crate::engine::EngineKind,
    input: &RunInput<'_>,
) -> Result<Vec<OsString>, Error> {
    use crate::engine::EngineKind;
    validate_tool(input.program, input.args)?;
    if kind == EngineKind::Podman && input.identity.uid == 0 {
        return Err(Error::operational(
            "build Podman run",
            "invoking identity",
            "rootful execution is unsupported",
            "Use the certified non-root profile; no rootful fallback is attempted",
        ));
    }
    if !crate::image::valid_id(input.image_id) {
        return Err(Error::operational(
            "build selected-engine run",
            "local image",
            "immutable image ID required",
            "Inspect a compatible pre-existing local image",
        ));
    }
    let mut args: Vec<OsString> = [
        "run",
        "--rm",
        "--init",
        "--pull=never",
        "--sig-proxy=false",
        "--name",
        input.name,
        "--cidfile",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(input.cidfile.into());
    args.extend([
        "--stop-timeout".into(),
        "5".into(),
        "--user".into(),
        format!("{}:{}", input.identity.uid, input.identity.gid).into(),
    ]);
    match kind {
        EngineKind::Docker => {
            for group in input
                .identity
                .groups
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
            {
                args.extend(["--group-add".into(), group.to_string().into()]);
            }
        }
        EngineKind::Podman => {
            // Reached only after the prepared-profile gate certifies local rootless crun.
            args.extend([
                "--userns=keep-id".into(),
                "--group-add".into(),
                "keep-groups".into(),
            ]);
        }
    }
    for (path, target) in [
        (input.workspace, "/workspace"),
        (input.composer, "/cache/composer"),
        (input.npm, "/cache/npm"),
    ] {
        args.extend(["--mount".into(), mount(path, target)?]);
    }
    args.extend(["--workdir".into(), "/workspace".into()]);
    for value in [
        "HOME=/tmp",
        "XDG_CONFIG_HOME=/tmp/.config",
        "COMPOSER_HOME=/tmp/composer",
        "COMPOSER_CACHE_DIR=/cache/composer",
        "npm_config_cache=/cache/npm",
    ] {
        args.extend(["--env".into(), value.into()]);
    }
    args.push("-i".into());
    if input.terminal {
        args.push("-t".into());
    }
    args.extend([
        "--entrypoint".into(),
        input.program.into(),
        "--".into(),
        input.image_id.into(),
    ]);
    args.extend_from_slice(input.args);
    Ok(args)
}

pub(crate) struct Scratch {
    root: PathBuf,
    name: String,
    directory: std::fs::File,
}

impl Scratch {
    pub(crate) fn new() -> Result<Self, Error> {
        use std::os::unix::fs::DirBuilderExt;
        let mut random = [0; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|cause| {
                Error::operational(
                    "allocate invocation identity",
                    "OS randomness",
                    cause,
                    "Check operating-system randomness access",
                )
            })?;
        let name = format!(
            "larainit-{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let root = std::path::absolute(std::env::temp_dir())
            .map_err(|cause| {
                Error::operational(
                    "resolve invocation scratch",
                    "temporary directory",
                    cause,
                    "Use an accessible temporary directory",
                )
            })?
            .join(&name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|cause| {
                Error::operational(
                    "create invocation scratch",
                    root.display().to_string(),
                    cause,
                    "Use an accessible temporary directory; an existing path is never reused",
                )
            })?;
        use std::os::unix::fs::OpenOptionsExt;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(&root)
            .map_err(|cause| {
                Error::operational(
                    "open invocation scratch",
                    root.display().to_string(),
                    cause,
                    "Use an accessible temporary directory",
                )
            })?;
        Ok(Self {
            root,
            name,
            directory,
        })
    }

    fn cidfile(&self) -> PathBuf {
        self.root.join("cid")
    }

    pub(crate) fn iidfile(&self) -> PathBuf {
        self.root.join("iid")
    }

    pub(crate) fn image_id(
        &self,
        kind: crate::engine::EngineKind,
    ) -> Result<(String, std::fs::File), Error> {
        use std::os::unix::fs::MetadataExt;
        let failure = |cause: String| {
            Error::operational(
                "read private build IID",
                "invocation scratch",
                cause,
                "No container cleanup or image rollback is authorized by an IID",
            )
        };
        let directory = self
            .directory
            .metadata()
            .map_err(|cause| failure(cause.to_string()))?;
        let path =
            std::fs::symlink_metadata(&self.root).map_err(|cause| failure(cause.to_string()))?;
        if !path.is_dir() || path.dev() != directory.dev() || path.ino() != directory.ino() {
            return Err(failure("build scratch path was replaced".into()));
        }
        let file = nix::fcntl::openat(
            &self.directory,
            "iid",
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(|cause| failure(cause.to_string()))?;
        let metadata = file
            .metadata()
            .map_err(|cause| failure(cause.to_string()))?;
        if !metadata.is_file()
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.nlink() != 1
        {
            return Err(failure(
                "IID must be an owned, single-link regular file".into(),
            ));
        }
        let mut bytes = Vec::new();
        (&file)
            .take(80)
            .read_to_end(&mut bytes)
            .map_err(|cause| failure(cause.to_string()))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| failure("non-UTF-8 IID".into()))?;
        let id = text.strip_suffix('\n').unwrap_or(text);
        if crate::image::valid_id(id) {
            return Ok((id.to_owned(), file));
        }
        if kind == crate::engine::EngineKind::Podman && crate::image::valid_container_id(id) {
            return Ok((format!("sha256:{id}"), file));
        }
        Err(failure("missing or invalid full immutable IID".into()))
    }

    fn container_id(&self) -> Result<Option<String>, Error> {
        let file = match nix::fcntl::openat(
            &self.directory,
            "cid",
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(io::Error::from)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(cause) => {
                return Err(Error::operational(
                    "read invocation container ID",
                    self.cidfile().display().to_string(),
                    cause,
                    "Cleanup is best effort; do not remove containers by guessed names",
                ));
            }
        };
        if !file
            .metadata()
            .map_err(|cause| {
                Error::operational(
                    "inspect invocation cidfile",
                    self.cidfile().display().to_string(),
                    cause,
                    "Cleanup is best effort",
                )
            })?
            .is_file()
        {
            return Err(Error::operational(
                "validate invocation cidfile",
                self.cidfile().display().to_string(),
                "not a regular file",
                "No container cleanup target is authorized",
            ));
        }
        let mut bytes = Vec::new();
        file.take(66).read_to_end(&mut bytes).map_err(|cause| {
            Error::operational(
                "read invocation container ID",
                self.cidfile().display().to_string(),
                cause,
                "Check only this invocation's container; cleanup is best effort",
            )
        })?;
        let id = std::str::from_utf8(&bytes)
            .ok()
            .map(|text| text.strip_suffix('\n').unwrap_or(text));
        match id.filter(|id| crate::image::valid_container_id(id)) {
            Some(id) => Ok(Some(id.to_owned())),
            None => Err(Error::operational(
                "validate invocation container ID",
                self.cidfile().display().to_string(),
                "cidfile does not contain a full container ID",
                "No cleanup target is authorized; inspect this invocation manually without bulk deletion",
            )),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if let Err(error) = nix::unistd::unlinkat(
            &self.directory,
            "iid",
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        ) {
            if error != nix::errno::Errno::ENOENT {
                diagnostic(format_args!("remove private iidfile: {error}"));
            }
        }
        match nix::unistd::unlinkat(
            &self.directory,
            "cid",
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        )
        .map_err(io::Error::from)
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => diagnostic(format_args!("remove invocation cidfile: {error}")),
        }
        // ponytail: check/removal is not atomic; hostile same-user swaps need a protected parent.
        if !matches!(
            (self.directory.metadata(), std::fs::symlink_metadata(&self.root)),
            (Ok(original), Ok(current)) if current.is_dir()
                && current.dev() == original.dev() && current.ino() == original.ino()
        ) {
            diagnostic(format_args!(
                "invocation scratch path {} was replaced or is inaccessible; directory not removed",
                self.root.display()
            ));
            return;
        }
        if let Err(error) = std::fs::remove_dir(&self.root) {
            diagnostic(format_args!(
                "remove invocation scratch {}: {error}",
                self.root.display()
            ));
        }
    }
}

struct Terminal {
    settings: nix::sys::termios::Termios,
    foreground: Option<Pid>,
    old_mask: Option<nix::sys::signal::SigSet>,
}

impl Terminal {
    fn capture() -> Result<Option<Self>, Error> {
        use std::io::IsTerminal;
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return Ok(None);
        }
        let settings = nix::sys::termios::tcgetattr(&stdin).map_err(terminal_error)?;
        let foreground = match nix::unistd::tcgetpgrp(&stdin) {
            Ok(group) => {
                if group != nix::unistd::getpgrp() {
                    return Err(terminal_error(
                        "Larainit is not the foreground terminal job; use fg",
                    ));
                }
                Some(group)
            }
            Err(nix::errno::Errno::ENOTTY) => None,
            Err(cause) => return Err(terminal_error(cause)),
        };
        Ok(Some(Self {
            settings,
            foreground,
            old_mask: None,
        }))
    }

    fn handoff(&mut self, child: &Child) -> Result<(), Error> {
        if self.foreground.is_none() {
            return Ok(());
        }
        use nix::sys::signal::{SigSet, SigmaskHow, pthread_sigmask};
        let mut set = SigSet::empty();
        set.add(Signal::SIGTTOU);
        let mut old = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&set), Some(&mut old))
            .map_err(terminal_error)?;
        self.old_mask = Some(old);
        nix::unistd::tcsetpgrp(io::stdin(), Pid::from_raw(child.id() as i32))
            .map_err(terminal_error)?;
        // A fast client may have attempted to read before the terminal handoff.
        let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGCONT);
        Ok(())
    }
}

fn terminal_error(cause: impl std::fmt::Display) -> Error {
    Error::operational(
        "manage terminal",
        "stdin",
        cause,
        "Use a foreground terminal job or pipe input explicitly",
    )
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(group) = self.foreground {
            if let Err(cause) = nix::unistd::tcsetpgrp(io::stdin(), group) {
                diagnostic(format_args!("restore terminal foreground: {cause}"));
            }
        }
        if let Err(cause) = nix::sys::termios::tcsetattr(
            io::stdin(),
            nix::sys::termios::SetArg::TCSANOW,
            &self.settings,
        ) {
            diagnostic(format_args!("restore terminal settings: {cause}"));
        }
        if let Some(mask) = self.old_mask {
            if let Err(cause) = nix::sys::signal::pthread_sigmask(
                nix::sys::signal::SigmaskHow::SIG_SETMASK,
                Some(&mask),
                None,
            ) {
                diagnostic(format_args!("restore terminal signal mask: {cause}"));
            }
        }
    }
}

fn cleanup(docker: &crate::engine::ResolvedEngine, scratch: &Scratch, signal: Signal) {
    use crate::engine::EngineKind;
    let id = match scratch.container_id() {
        Ok(Some(id)) => id,
        Ok(None) => {
            diagnostic(format_args!(
                "interruption cleanup not possible: invocation container ID absent/late; container may remain. Inspect invocation {} manually; no guessed-name or bulk removal is attempted",
                scratch.name
            ));
            return;
        }
        Err(cause) => {
            diagnostic(cause);
            return;
        }
    };
    let mut stop = crate::engine::command(&docker.executable);
    let requested = if signal == Signal::SIGINT {
        "INT"
    } else {
        "TERM"
    };
    let timeout = match docker.kind {
        EngineKind::Docker => {
            stop.args([
                "stop",
                "--signal",
                &format!("SIG{requested}"),
                "--timeout",
                "5",
                "--",
                &id,
            ]);
            Duration::from_secs(8)
        }
        EngineKind::Podman => {
            stop.args(["kill", "--signal", requested, "--", &id]);
            Duration::from_secs(3)
        }
    };
    let stopped = observe_with_deadline(&mut stop, None, timeout);
    if let Ok(output) = &stopped {
        if String::from_utf8_lossy(&output.stderr).contains("No such container:") {
            return;
        }
        if docker.kind == EngineKind::Podman && output.status.success() {
            // No status probe/reap: keep the client leader reserved for later group signals.
            // kill + reap (5s), grace (5s), rm + reap (5s), client reap (2s) <= 17s.
            thread::sleep(Duration::from_secs(5));
        }
    }
    let mut remove = crate::engine::command(&docker.executable);
    match docker.kind {
        EngineKind::Docker => {
            remove.args(["rm", "--force", "--volumes", "--", &id]);
        }
        EngineKind::Podman => {
            remove.args(["rm", "--force", "--time", "0", "--", &id]);
        }
    }
    match observe_with_deadline(&mut remove, None, Duration::from_secs(3)) {
        Ok(output)
            if output.status.success()
                || String::from_utf8_lossy(&output.stderr).contains("No such container:") => {}
        Ok(output) => diagnostic(format_args!(
            "interruption cleanup failed for container {id}: {}; remove only that ID manually when {} is reachable",
            crate::engine::failed_response(&output),
            docker.kind
        )),
        Err(cause) => diagnostic(format_args!(
            "interruption cleanup failed for container {id}: {cause}; remove only that ID manually when {} is reachable",
            docker.kind
        )),
    }
}

pub(crate) fn stream_build(command: &mut Command, signals: &Signals) -> Result<i32, Error> {
    let failure = |cause: String| {
        Error::operational(
            "supervise explicit build client",
            "selected engine",
            cause,
            "Engine artifacts may remain; no container/image cleanup, rollback or fallback is attempted",
        )
    };
    if let Some(signal) = signals.signal() {
        return Ok(128 + signal as i32);
    }
    let mut tty = Terminal::capture()?;
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|cause| failure(cause.to_string()))?;
    if let Some(tty) = tty.as_mut() {
        if let Err(cause) = tty.handoff(&child) {
            let _ = terminate_client(&mut child);
            return Err(cause);
        }
    }
    loop {
        if let Some(signal) = signals.signal() {
            let _ = killpg(Pid::from_raw(child.id() as i32), signal);
            // Keep the leader unreaped until the final group signal; descendants may survive it.
            thread::sleep(Duration::from_secs(5));
            let status =
                terminate_client(&mut child).map_err(|cause| failure(cause.to_string()))?;
            let status = crate::error::child_status(status);
            return Ok(if status == 0 {
                128 + signal as i32
            } else {
                status
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(crate::error::child_status(status)),
            Ok(None) => thread::sleep(POLL),
            Err(cause) => {
                let _ = terminate_client(&mut child);
                return Err(failure(cause.to_string()));
            }
        }
    }
}

pub fn execute(
    docker: &crate::engine::ResolvedEngine,
    image: &crate::image::Image,
    workspace: &Path,
    caches: &crate::cache::Caches,
    tool: (&OsStr, &[OsString]),
    terminal: bool,
    signals: &Signals,
) -> Result<i32, Error> {
    let (program, args) = tool;
    let identity = Identity::current()?;
    validate_tool(program, args)?;
    caches.validate()?;
    let scratch = Scratch::new()?;
    let cidfile = scratch.cidfile();
    let arguments = run_arguments_for(
        docker.kind,
        &RunInput {
            workspace,
            composer: &caches.composer,
            npm: &caches.npm,
            identity: &identity,
            image_id: &image.id,
            program,
            args,
            terminal,
            name: &scratch.name,
            cidfile: &cidfile,
        },
    )?;
    let mut tty = Terminal::capture()?;
    if let Some(signal) = signals.signal() {
        return Ok(128 + signal as i32);
    }
    caches.prepare()?;
    if let Some(signal) = signals.signal() {
        return Ok(128 + signal as i32);
    }
    let mut command = crate::engine::command(&docker.executable);
    command
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0);
    let mut child = command.spawn().map_err(|cause| {
        Error::operational(
            "launch toolchain container",
            format!("{} selected store", docker.kind),
            cause,
            "Check selected-engine CLI access and workspace/cache mounts; no fallback is attempted",
        )
    })?;
    if let Some(tty) = tty.as_mut() {
        if let Err(cause) = tty.handoff(&child) {
            cleanup(docker, &scratch, Signal::SIGTERM);
            let _ = terminate_client(&mut child);
            return Err(cause);
        }
    }
    loop {
        if let Some(signal) = signals.signal() {
            cleanup(docker, &scratch, signal);
            let status = finish_interrupted_client(&mut child, signal).map_err(|cause| {
                Error::operational(
                    "reap interrupted engine client",
                    "invocation only",
                    cause,
                    "Cleanup is best effort; check this invocation manually",
                )
            })?;
            return Ok(crate::error::child_status(status));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.code() == Some(125) && matches!(scratch.container_id(), Ok(None)) {
                    diagnostic(crate::image::error_for(
                        docker.kind,
                        &image.reference,
                        format!(
                            "{} execution returned 125 before an invocation container ID was available (inspected {}); see original engine stderr and check image availability and mount permissions",
                            docker.kind, image.id
                        ),
                    ));
                }
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status
                    .signal()
                    .and_then(|value| Signal::try_from(value).ok())
                {
                    cleanup(docker, &scratch, signal);
                } else if terminal || !status.success() {
                    // Detaching returns success without stopping the container. Numeric client
                    // failures can also leave a created container; --rm only runs after exit.
                    if matches!(scratch.container_id(), Ok(Some(_))) {
                        cleanup(docker, &scratch, Signal::SIGTERM);
                    }
                }
                return Ok(crate::error::child_status(status));
            }
            Ok(None) => thread::sleep(POLL),
            Err(cause) => {
                cleanup(docker, &scratch, Signal::SIGTERM);
                let _ = terminate_client(&mut child);
                return Err(Error::operational(
                    "supervise engine client",
                    "invocation only",
                    cause,
                    "Check this invocation manually; cleanup is best effort",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_capture_limit_and_zero_budget_fail_closed() {
        let fixture = test_support::Fixture::new();
        fixture.write("version-stdout", "x".repeat(64));
        assert!(matches!(
            observe_with_limits(&mut version(&fixture), None, Duration::from_secs(1), 32),
            Err(ProbeFailure::Overflow)
        ));
        let calls = fixture.calls().len();
        assert!(matches!(
            observe_with_limits(&mut version(&fixture), None, Duration::ZERO, 32),
            Err(ProbeFailure::Timeout)
        ));
        assert!(matches!(
            observe_with_limits(&mut version(&fixture), None, Duration::from_secs(1), 0),
            Err(ProbeFailure::Overflow)
        ));
        assert_eq!(
            fixture.calls().len(),
            calls,
            "exhausted observation launched a process"
        );
        fixture.write("version-stdout", "x".repeat(128 * 1024));
        assert!(matches!(
            observe_with_limits(
                &mut version(&fixture),
                None,
                Duration::from_secs(1),
                usize::MAX
            ),
            Err(ProbeFailure::Overflow)
        ));
    }

    #[test]
    fn csv_paths_and_native_transport() {
        assert_eq!(
            mount(Path::new("/home/me/a,b\"q:c"), "/workspace").unwrap(),
            "type=bind,\"src=/home/me/a,b\"\"q:c\",dst=/workspace"
        );
        for path in ["relative", "/tmp/new\nline", "/tmp/trailing "] {
            assert!(mount(Path::new(path), "/workspace").is_err());
        }
        use std::os::unix::ffi::OsStringExt;
        assert!(
            mount(
                Path::new(&OsString::from_vec(vec![b'/', 0xff])),
                "/workspace"
            )
            .is_err()
        );
        assert!(validate_tool(OsStr::new("php"), &["a b".into(), "".into()]).is_ok());
        assert!(validate_tool(OsStr::new("php"), &[OsString::from_vec(vec![0xff])]).is_err());
    }

    #[test]
    fn safe_run_shape_and_distinct_groups() {
        let identity = Identity {
            uid: 1000,
            gid: 1001,
            groups: vec![42, 42, 1001],
        };
        let id = format!("sha256:{}", "a".repeat(64));
        let args = vec![OsString::from(""), OsString::from("--help")];
        let mut input = RunInput {
            workspace: Path::new("/project"),
            composer: Path::new("/cache/composer"),
            npm: Path::new("/cache/npm"),
            identity: &identity,
            image_id: &id,
            program: OsStr::new("/path with space/php"),
            args: &args,
            terminal: false,
            name: "unique",
            cidfile: Path::new("/private/cid"),
        };
        let result = run_arguments_for(crate::engine::EngineKind::Docker, &input).unwrap();
        assert!(result.contains(&"--pull=never".into()));
        assert!(result.contains(&"--rm".into()));
        assert!(result.contains(&"--init".into()));
        assert!(result.contains(&"-i".into()));
        assert!(!result.contains(&"-t".into()));
        assert_eq!(result.iter().filter(|v| *v == "--mount").count(), 3);
        assert_eq!(result.iter().filter(|v| *v == "--group-add").count(), 2);
        let tail = result.iter().position(|v| v == "--entrypoint").unwrap();
        let expected: Vec<OsString> = [
            "--entrypoint",
            "/path with space/php",
            "--",
            &id,
            "",
            "--help",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        assert_eq!(&result[tail..], expected);
        input.terminal = true;
        assert!(
            run_arguments_for(crate::engine::EngineKind::Docker, &input)
                .unwrap()
                .contains(&"-t".into())
        );
    }

    #[test]
    fn podman_run_uses_keep_id_and_exclusive_keep_groups_without_repairs() {
        let identity = Identity {
            uid: 1000,
            gid: 1001,
            groups: vec![42, 42, 1001],
        };
        let id = format!("sha256:{}", "b".repeat(64));
        let tail = [
            OsString::from(""),
            OsString::from("--runtime"),
            OsString::from("日本語"),
        ];
        let mut input = RunInput {
            workspace: Path::new("/project"),
            composer: Path::new("/cache/composer"),
            npm: Path::new("/cache/npm"),
            identity: &identity,
            image_id: &id,
            program: OsStr::new("php"),
            args: &tail,
            terminal: false,
            name: "unique",
            cidfile: Path::new("/private/cid"),
        };
        let arguments = run_arguments_for(crate::engine::EngineKind::Podman, &input).unwrap();
        let end = arguments
            .iter()
            .position(|value| value == "--entrypoint")
            .unwrap();
        let options = &arguments[..end];
        assert!(options.contains(&"--userns=keep-id".into()));
        assert_eq!(
            options
                .iter()
                .filter(|value| *value == "--group-add")
                .count(),
            1
        );
        let group = options
            .iter()
            .position(|value| value == "--group-add")
            .unwrap();
        assert_eq!(options[group + 1], "keep-groups");
        let user = options.iter().position(|value| value == "--user").unwrap();
        assert_eq!(options[user + 1], "1000:1001");
        assert!(options.contains(&"--pull=never".into()));
        for forbidden in [
            "--runtime",
            "--privileged",
            "--security-opt",
            "--uidmap",
            "--gidmap",
            "--group-add=42",
        ] {
            assert!(!options.iter().any(|value| value == forbidden));
        }
        assert_eq!(arguments[end + 3], id.as_str());
        assert_eq!(&arguments[end + 4..], &tail);
        assert!(!options.contains(&"-t".into()));
        input.terminal = true;
        assert!(
            run_arguments_for(crate::engine::EngineKind::Podman, &input)
                .unwrap()
                .contains(&"-t".into())
        );
        let root = Identity {
            uid: 0,
            gid: 0,
            groups: vec![],
        };
        input.identity = &root;
        assert!(run_arguments_for(crate::engine::EngineKind::Podman, &input).is_err());
        assert!(run_arguments_for(crate::engine::EngineKind::Docker, &input).is_ok());
    }

    #[test]
    fn podman_cleanup_uses_only_captured_cid_and_requested_signal() {
        for (cid, signal, failure) in [
            (Some("a".repeat(64)), Signal::SIGINT, ""),
            (Some("a".repeat(64)), Signal::SIGTERM, "exit=1"),
            (Some("a".repeat(64)), Signal::SIGTERM, "hang"),
            (Some("invalid".into()), Signal::SIGTERM, ""),
            (None, Signal::SIGTERM, ""),
        ] {
            let fixture = test_support::Fixture::new();
            let bin = fixture.install_engine("podman");
            // Immutable hard-linked mock derives its private root from bin/podman.
            let selected = crate::engine::ResolvedEngine {
                kind: crate::engine::EngineKind::Podman,
                executable: bin.join("podman"),
                version: "mock".into(),
                selection_reason: "test only",
            };
            fixture.write("podman/cleanup-mode", failure);
            let scratch = Scratch::new().unwrap();
            if let Some(cid) = &cid {
                std::fs::write(scratch.cidfile(), cid).unwrap();
            }
            let start = Instant::now();
            cleanup(&selected, &scratch, signal);
            assert!(start.elapsed() < Duration::from_secs(16));
            let calls = fixture.engine_calls("podman");
            if cid.as_deref() != Some(&"a".repeat(64)) {
                assert!(calls.is_empty());
                continue;
            }
            assert_eq!(calls.len(), 2, "mode {failure:?}, calls {calls:?}");
            let requested = if signal == Signal::SIGINT {
                "INT"
            } else {
                "TERM"
            };
            let id = "a".repeat(64);
            assert_eq!(
                calls[0],
                ["kill", "--signal", requested, "--", &id].map(OsString::from)
            );
            assert_eq!(
                calls[1],
                ["rm", "--force", "--time", "0", "--", &id].map(OsString::from)
            );
            assert!(fixture.calls().is_empty());
        }
    }

    #[test]
    fn selected_engine_lifecycle_cleans_created_failures_and_detach_without_changing_status() {
        use crate::engine::{EngineKind, ResolvedEngine};
        for kind in [EngineKind::Docker, EngineKind::Podman] {
            for (mode, terminal, expected, cleanup_expected) in [
                ("exit=42", false, 42, true),
                ("exit=125", false, 125, true),
                ("", true, 0, true),
                ("", false, 0, false),
                ("missing-cid", false, 125, false),
                ("bad-cid", false, 42, false),
                ("removed-image", false, 125, false),
                ("created-exit=125", false, 125, true),
            ] {
                let fixture = test_support::Fixture::new();
                let name = kind.to_string();
                let bin = if kind == EngineKind::Docker {
                    fixture.install_docker()
                } else {
                    fixture.install_engine(&name)
                };
                let prefix = if kind == EngineKind::Podman {
                    "podman/"
                } else {
                    ""
                };
                if matches!(mode, "missing-cid" | "bad-cid") {
                    fixture.write(&format!("{prefix}run-cid-behavior"), mode);
                    fixture.write(&format!("{prefix}run-mode"), format!("exit={expected}"));
                } else {
                    fixture.write(&format!("{prefix}run-mode"), mode);
                }
                let original = format!("sha256:{}", "a".repeat(64));
                fixture.write(&format!("{prefix}run-expected-image"), &original);
                if mode == "removed-image" {
                    fixture.write(&format!("{prefix}run-removed-image"), &original);
                }
                // A vanished container avoids artificial grace sleeps and tests auto-removal.
                if mode != "created-exit=125" {
                    fixture.write(&format!("{prefix}cleanup-mode"), "exit=1");
                    fixture.write(
                        &format!("{prefix}cleanup-stderr"),
                        "No such container: already removed",
                    );
                }
                fixture.write("unrelated-container", "untouched");
                let selected = ResolvedEngine {
                    kind,
                    executable: bin.join(&name),
                    version: "mock".into(),
                    selection_reason: "test only",
                };
                let replacement = serde_json::json!({"Id": if kind == EngineKind::Podman { "b".repeat(64) } else { format!("sha256:{}", "b".repeat(64)) }, "Os": "linux", "Architecture": "amd64", "ManifestType": "application/vnd.oci.image.manifest.v1+json"});
                fixture.write(
                    &format!("{prefix}inspect-next-stdout"),
                    if kind == EngineKind::Podman {
                        serde_json::json!([replacement]).to_string()
                    } else {
                        replacement.to_string()
                    },
                );
                let image = crate::image::inspect(&selected, "app:tag", None).unwrap();
                let caches = crate::cache::Caches::at(&fixture.root.join("cache"));
                let signals = Signals::register().unwrap();
                let start = Instant::now();
                let status = execute(
                    &selected,
                    &image,
                    &fixture.root,
                    &caches,
                    (OsStr::new("php"), &[]),
                    terminal,
                    &signals,
                )
                .unwrap();
                assert_eq!(status, expected, "{kind} {mode}");
                assert!(start.elapsed() < Duration::from_secs(3));
                let calls = fixture.engine_calls(&name);
                assert_eq!(
                    calls.len(),
                    if mode == "created-exit=125" {
                        4
                    } else if cleanup_expected {
                        3
                    } else {
                        2
                    },
                    "{kind} {mode}: {calls:?}"
                );
                assert_eq!(calls[0][0], "image");
                assert_eq!(calls[1][0], "run");
                assert!(calls[1].iter().any(|arg| arg == "--pull=never"));
                if cleanup_expected {
                    assert_eq!(
                        calls[2][0],
                        if kind == EngineKind::Podman {
                            "kill"
                        } else {
                            "stop"
                        }
                    );
                    assert_eq!(calls[2].last().unwrap(), &OsString::from("a".repeat(64)));
                }
                if mode == "created-exit=125" {
                    assert_eq!(calls[3][0], "rm");
                    assert_eq!(calls[3].last().unwrap(), &OsString::from("a".repeat(64)));
                    assert_eq!(
                        std::fs::read(fixture.root.join(format!("{prefix}container-state")))
                            .unwrap(),
                        b"removed"
                    );
                    assert!(!fixture.root.join(format!("{prefix}run-started")).exists());
                }
                assert_eq!(
                    std::fs::read(fixture.root.join("unrelated-container")).unwrap(),
                    b"untouched"
                );
                assert!(!fixture.root.join("unexpected-command").exists());
            }
        }
    }

    fn version(fixture: &test_support::Fixture) -> Command {
        let mut command = fixture.docker();
        command.args(["version", "--format", "{{json .Server}}"]);
        command
    }

    #[test]
    fn inspected_engine_mount_failures_never_launch_or_search_another_store() {
        use crate::engine::{EngineKind, ResolvedEngine};
        for kind in [EngineKind::Docker, EngineKind::Podman] {
            for failure in ["workspace", "cache"] {
                let fixture = test_support::Fixture::new();
                let bin = fixture.install_docker();
                fixture.install_engine("podman");
                let name = kind.to_string();
                let other = if kind == EngineKind::Podman {
                    "docker"
                } else {
                    "podman"
                };
                let selected = ResolvedEngine {
                    kind,
                    executable: bin.join(&name),
                    version: "mock".into(),
                    selection_reason: "test only",
                };
                let image = crate::image::inspect(&selected, "same-tag", None).unwrap();
                let caches = crate::cache::Caches::at(&fixture.root.join("cache"));
                let workspace = if failure == "workspace" {
                    let path = fixture.root.join("unrepresentable\nworkspace");
                    std::fs::create_dir(&path).unwrap();
                    path
                } else {
                    fixture.write("cache/composer", "existing user file");
                    fixture.root.clone()
                };
                let result = execute(
                    &selected,
                    &image,
                    &workspace,
                    &caches,
                    (OsStr::new("php"), &[]),
                    false,
                    &Signals::register().unwrap(),
                );
                assert!(result.is_err(), "{kind} {failure}");
                let calls = fixture.engine_calls(&name);
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0][0], "image");
                assert!(fixture.engine_calls(other).is_empty());
                assert!(!caches.npm.exists());
                if failure == "workspace" {
                    assert!(!fixture.root.join("cache").exists());
                } else {
                    assert_eq!(
                        std::fs::read(&caches.composer).unwrap(),
                        b"existing user file"
                    );
                }
                assert!(!fixture.root.join("unexpected-command").exists());
            }
        }
    }

    #[test]
    fn bounds_hangs_floods_and_retained_pipes() {
        for mode in ["hang", "flood", "retain-pipes"] {
            let fixture = test_support::Fixture::new();
            fixture.write("version-mode", mode);
            let start = Instant::now();
            let result =
                observe_with_deadline(&mut version(&fixture), None, Duration::from_millis(500));
            if mode == "flood" {
                assert!(matches!(result, Err(ProbeFailure::Overflow)), "{result:?}");
            } else {
                assert!(matches!(result, Err(ProbeFailure::Timeout)), "{result:?}");
            }
            assert!(start.elapsed() < Duration::from_secs(3));
        }
    }

    #[test]
    fn interrupted_client_reaps_only_after_its_final_descendant_group_signal() {
        let fixture = test_support::Fixture::new();
        let marker = fixture.root.join("descendant-pid");
        let mut child = Command::new("/bin/sh")
            .args(["-c", "trap 'exit 42' TERM; /bin/sh -c 'trap \"\" INT TERM; echo $$ > \"$1\"; exec /bin/sleep 2' sh \"$1\" & while [ ! -s \"$1\" ]; do /bin/sleep 0.01; done; exit 42", "test-owned-client"])
            .arg(&marker).process_group(0).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().unwrap();
        let start = Instant::now();
        while std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            assert!(start.elapsed() < Duration::from_secs(1));
            thread::sleep(POLL);
        }
        let descendant: u32 = std::fs::read_to_string(marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let status = finish_interrupted_client(&mut child, Signal::SIGTERM).unwrap();
        let gone_deadline = Instant::now() + Duration::from_millis(100);
        let state = loop {
            let state = std::fs::read_to_string(format!("/proc/{descendant}/stat"))
                .ok()
                .and_then(|stat| stat.rsplit_once(") ").map(|(_, tail)| tail.as_bytes()[0]));
            if matches!(state, None | Some(b'Z')) || Instant::now() >= gone_deadline {
                break state;
            }
            thread::sleep(POLL);
        };
        assert_eq!(crate::error::child_status(status), 42);
        assert!(
            matches!(state, None | Some(b'Z')),
            "signal-ignoring descendant survived because the leader was reaped too early"
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn null_input_separate_output_status_and_launch_failures() {
        let fixture = test_support::Fixture::new();
        fixture.write("run-mode", "stdin");
        let mut command = fixture.docker();
        command.arg("run");
        assert!(observe(&mut command, None).unwrap().stdout.is_empty());
        fixture.write("version-mode", "exit=42");
        fixture.write("version-stdout", "out\n");
        fixture.write("version-stderr", "err\n");
        let output = observe(&mut version(&fixture), None).unwrap();
        assert_eq!(output.status.code(), Some(42));
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
        assert!(matches!(
            observe(&mut Command::new(fixture.root.join("absent")), None),
            Err(ProbeFailure::Launch(_))
        ));
        use std::os::unix::fs::PermissionsExt;
        fixture.write("denied", "not executable");
        std::fs::set_permissions(
            fixture.root.join("denied"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(matches!(
            observe(&mut Command::new(fixture.root.join("denied")), None),
            Err(ProbeFailure::Denied(_))
        ));
    }

    #[test]
    fn default_probe_deadline_is_five_seconds() {
        let fixture = test_support::Fixture::new();
        fixture.write("version-mode", "hang");
        let start = Instant::now();
        assert!(matches!(
            observe(&mut version(&fixture), None),
            Err(ProbeFailure::Timeout)
        ));
        assert!(start.elapsed() >= Duration::from_secs(5));
        assert!(start.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn cleanup_refuses_unsafe_cid_files_for_both_selected_adapters() {
        use crate::engine::{EngineKind, ResolvedEngine};
        for kind in [EngineKind::Docker, EngineKind::Podman] {
            for (case, bytes, valid) in [
                ("empty", vec![], false),
                ("short", vec![b'a'; 63], false),
                ("uppercase", vec![b'A'; 64], false),
                ("oversized", vec![b'a'; 4096], false),
                (
                    "prefix",
                    format!("sha256:{}", "a".repeat(64)).into_bytes(),
                    false,
                ),
                (
                    "whitespace",
                    format!("{} ", "a".repeat(64)).into_bytes(),
                    false,
                ),
                (
                    "extra-newline",
                    format!("{}\n\n", "a".repeat(64)).into_bytes(),
                    false,
                ),
                ("nul", [vec![b'a'; 64], vec![0]].concat(), false),
                ("non-utf8", vec![0xff; 64], false),
                ("symlink", vec![], false),
                ("directory", vec![], false),
                ("fifo", vec![], false),
                ("valid", vec![b'a'; 64], true),
                (
                    "valid-newline",
                    format!("{}\n", "a".repeat(64)).into_bytes(),
                    true,
                ),
            ] {
                let fixture = test_support::Fixture::new();
                let bin = fixture.install_docker();
                fixture.install_engine("podman");
                let name = kind.to_string();
                let prefix = if kind == EngineKind::Podman {
                    "podman/"
                } else {
                    ""
                };
                fixture.write(&format!("{prefix}cleanup-mode"), "exit=1");
                fixture.write(
                    &format!("{prefix}cleanup-stderr"),
                    "No such container: already removed",
                );
                fixture.write("unrelated-cid", "b".repeat(64));
                let selected = ResolvedEngine {
                    kind,
                    executable: bin.join(&name),
                    version: "mock".into(),
                    selection_reason: "test only",
                };
                let scratch = Scratch::new().unwrap();
                match case {
                    "symlink" => std::os::unix::fs::symlink(
                        fixture.root.join("unrelated-cid"),
                        scratch.cidfile(),
                    )
                    .unwrap(),
                    "directory" => std::fs::create_dir(scratch.cidfile()).unwrap(),
                    "fifo" => assert!(
                        Command::new("/usr/bin/mkfifo")
                            .arg(scratch.cidfile())
                            .status()
                            .unwrap()
                            .success()
                    ),
                    _ => std::fs::write(scratch.cidfile(), bytes).unwrap(),
                }
                let start = Instant::now();
                cleanup(&selected, &scratch, Signal::SIGTERM);
                assert!(start.elapsed() < Duration::from_secs(3), "{kind} {case}");
                let calls = fixture.engine_calls(&name);
                assert_eq!(calls.len(), usize::from(valid), "{kind} {case}: {calls:?}");
                if valid {
                    assert_eq!(calls[0].last().unwrap(), &OsString::from("a".repeat(64)));
                }
                assert!(
                    fixture
                        .engine_calls(if kind == EngineKind::Docker {
                            "podman"
                        } else {
                            "docker"
                        })
                        .is_empty()
                );
                assert_eq!(
                    std::fs::read(fixture.root.join("unrelated-cid")).unwrap(),
                    vec![b'b'; 64]
                );
                assert!(!fixture.root.join("unexpected-command").exists());
                if case == "directory" {
                    std::fs::remove_dir(scratch.cidfile()).unwrap(); // Test-owned nonregular input.
                }
                let root = scratch.root.clone();
                drop(scratch);
                assert!(!root.exists());
            }
        }
    }

    #[test]
    fn scratch_drop_never_removes_a_replacement_directory() {
        let fixture = test_support::Fixture::new();
        let scratch = Scratch::new().unwrap();
        let root = scratch.root.clone();
        std::fs::rename(&root, fixture.root.join("original-scratch")).unwrap();
        std::fs::create_dir(&root).unwrap();
        drop(scratch);
        let preserved = root.is_dir();
        if preserved {
            std::fs::remove_dir(&root).unwrap(); // Only this test's replacement directory.
        }
        assert!(
            preserved,
            "scratch cleanup removed a directory it did not create"
        );
    }

    #[test]
    fn private_identity_authorizes_only_a_valid_captured_container() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new().unwrap();
        let root = scratch.root.clone();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(scratch.name.len(), "larainit-".len() + 32);
        assert!(scratch.container_id().unwrap().is_none());
        std::fs::write(scratch.cidfile(), "guess").unwrap();
        assert!(scratch.container_id().is_err());
        std::fs::write(scratch.cidfile(), "a".repeat(64)).unwrap();
        assert_eq!(scratch.container_id().unwrap(), Some("a".repeat(64)));
        drop(scratch);
        assert!(!root.exists());
    }

    #[test]
    fn observation_keeps_leader_pid_reserved_until_capture_cleanup() {
        let fixture = test_support::Fixture::new();
        fixture.write("version-mode", "retain-pipes");
        thread::scope(|scope| {
            let observation = scope.spawn(|| {
                observe_with_deadline(&mut version(&fixture), None, Duration::from_secs(1))
            });
            let deadline = Instant::now() + Duration::from_millis(800);
            let pidfile = fixture.root.join("retained-leader-pid");
            while !pidfile.exists() {
                assert!(Instant::now() < deadline);
                thread::sleep(POLL);
            }
            let pid = std::fs::read_to_string(pidfile).unwrap();
            thread::sleep(Duration::from_millis(150));
            let reserved = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            let result = observation.join().unwrap();
            assert!(matches!(result, Err(ProbeFailure::Timeout)));
            assert!(
                reserved.is_ok(),
                "probe reaped its leader while it still needed to signal the group; PID could be reused"
            );
        });
    }

    #[test]
    fn replaced_scratch_parent_cannot_redirect_identity_or_delete_another_file() {
        let scratch = Scratch::new().unwrap();
        let original = scratch.root.clone();
        let moved = original.with_extension("moved");
        let other = test_support::Fixture::new();
        std::fs::write(scratch.cidfile(), "a".repeat(64)).unwrap();
        other.write("cid", "b".repeat(64));
        std::fs::rename(&original, &moved).unwrap();
        std::os::unix::fs::symlink(&other.root, &original).unwrap();
        let identity = scratch.container_id().unwrap();
        drop(scratch);
        let other_preserved = std::fs::read(other.root.join("cid")).ok();
        std::fs::remove_file(original).unwrap();
        std::fs::remove_dir_all(moved).unwrap();
        assert_eq!(
            identity,
            Some("a".repeat(64)),
            "parent substitution redirected cleanup authorization"
        );
        assert_eq!(
            other_preserved,
            Some("b".repeat(64).into_bytes()),
            "scratch cleanup followed a replaced parent and removed unrelated data"
        );
    }
}
