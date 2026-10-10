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
#[path = "../tests/support/mod.rs"]
mod test_support;

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
            Self::Launch(error) => write!(f, "cannot launch Docker client: {error}"),
            Self::Denied(error) => write!(f, "Docker access denied: {error}"),
            Self::Io(error) => write!(f, "Docker observation I/O failed: {error}"),
            Self::Overflow => write!(
                f,
                "Docker observation exceeded the combined 64-KiB output limit"
            ),
            Self::Timeout => write!(f, "Docker observation exceeded its deadline"),
            Self::Interrupted(signal) => write!(f, "Docker observation interrupted by {signal}"),
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
) -> Result<(), ProbeFailure> {
    if *eof {
        return Ok(());
    }
    let mut buffer = [0; 4096];
    match pipe.read(&mut buffer) {
        Ok(0) => *eof = true,
        Ok(size) => {
            if *total + size > OUTPUT_LIMIT {
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
        if let Some(status) = child.try_wait()? {
            return Ok(status);
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
        let mut status = None;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(signal) = signals.and_then(Signals::signal) {
                return Err(ProbeFailure::Interrupted(signal));
            }
            if Instant::now() >= deadline {
                return Err(ProbeFailure::Timeout);
            }
            drain(&mut stdout, &mut observation.0, &mut out_eof, &mut total)?;
            drain(&mut stderr, &mut observation.1, &mut err_eof, &mut total)?;
            if status.is_none() {
                status = child.try_wait().map_err(ProbeFailure::Io)?;
            }
            if let Some(status) = status {
                if out_eof && err_eof {
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

pub fn run_arguments(input: &RunInput<'_>) -> Result<Vec<OsString>, Error> {
    validate_tool(input.program, input.args)?;
    if !crate::image::valid_id(input.image_id) {
        return Err(Error::operational(
            "build Docker run",
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
    for group in input
        .identity
        .groups
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
    {
        args.extend(["--group-add".into(), group.to_string().into()]);
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

struct Scratch {
    root: PathBuf,
    name: String,
}

impl Scratch {
    fn new() -> Result<Self, Error> {
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
        Ok(Self { root, name })
    }

    fn cidfile(&self) -> PathBuf {
        self.root.join("cid")
    }

    fn container_id(&self) -> Result<Option<String>, Error> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(self.cidfile())
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
        match std::fs::remove_file(self.cidfile()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => diagnostic(format_args!("remove invocation cidfile: {error}")),
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

fn cleanup(docker: &crate::engine::Docker, scratch: &Scratch, signal: Signal) {
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
    stop.args([
        "stop",
        "--signal",
        &format!(
            "SIG{}",
            if signal == Signal::SIGINT {
                "INT"
            } else {
                "TERM"
            }
        ),
        "--timeout",
        "5",
        "--",
        &id,
    ]);
    let stopped = observe_with_deadline(&mut stop, None, Duration::from_secs(8));
    if let Ok(output) = &stopped {
        if output.status.success()
            || String::from_utf8_lossy(&output.stderr).contains("No such container:")
        {
            return;
        }
    }
    let mut remove = crate::engine::command(&docker.executable);
    remove.args(["rm", "--force", "--volumes", "--", &id]);
    match observe_with_deadline(&mut remove, None, Duration::from_secs(3)) {
        Ok(output)
            if output.status.success()
                || String::from_utf8_lossy(&output.stderr).contains("No such container:") => {}
        Ok(output) => diagnostic(format_args!(
            "interruption cleanup failed for container {id}: {}; remove only that ID manually when Docker is reachable",
            crate::engine::failed_response(&output)
        )),
        Err(cause) => diagnostic(format_args!(
            "interruption cleanup failed for container {id}: {cause}; remove only that ID manually when Docker is reachable"
        )),
    }
}

pub fn execute(
    docker: &crate::engine::Docker,
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
    let arguments = run_arguments(&RunInput {
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
    })?;
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
            "Docker effective context",
            cause,
            "Check Docker CLI access and the workspace/cache mounts; no fallback is attempted",
        )
    })?;
    if let Some(tty) = tty.as_mut() {
        if let Err(cause) = tty.handoff(&child) {
            let _ = terminate_client(&mut child);
            return Err(cause);
        }
    }
    loop {
        if let Some(signal) = signals.signal() {
            cleanup(docker, &scratch, signal);
            let status = finish_interrupted_client(&mut child, signal).map_err(|cause| {
                Error::operational(
                    "reap interrupted Docker client",
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
                    diagnostic(crate::image::error(
                        &image.reference,
                        format!(
                            "Docker execution returned 125 before an invocation container ID was available (inspected {}); see original Docker stderr and check image availability and mount permissions",
                            image.id
                        ),
                    ));
                }
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status
                    .signal()
                    .and_then(|value| Signal::try_from(value).ok())
                {
                    cleanup(docker, &scratch, signal);
                }
                return Ok(crate::error::child_status(status));
            }
            Ok(None) => thread::sleep(POLL),
            Err(cause) => {
                cleanup(docker, &scratch, Signal::SIGTERM);
                let _ = terminate_client(&mut child);
                return Err(Error::operational(
                    "supervise Docker client",
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
        let result = run_arguments(&input).unwrap();
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
        assert!(run_arguments(&input).unwrap().contains(&"-t".into()));
    }

    fn version(fixture: &test_support::Fixture) -> Command {
        let mut command = fixture.docker();
        command.args(["version", "--format", "{{json .Server}}"]);
        command
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
}
