use crate::image::validate_reference as validate_image;
use crate::{cli::Overrides, error::Error};
use nix::unistd::{AccessFlags, access};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use nix::fcntl::{Flock, FlockArg, OFlag, openat, renameat};
use nix::sys::stat::{Mode, mkdirat};

#[cfg(test)]
use crate::test_support;

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub engine: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnginePreference {
    Auto,
    Docker,
    Podman,
}

impl EnginePreference {
    pub fn parse(value: &OsStr, source: &Source) -> Result<Self, Error> {
        match value.to_str() {
            Some("auto") => Ok(Self::Auto),
            Some("docker") => Ok(Self::Docker),
            Some("podman") => Ok(Self::Podman),
            _ => Err(Error::operational(
                "resolve engine",
                source.to_string(),
                "unsupported engine value",
                "Use auto, docker or podman",
            )),
        }
    }

    pub fn explicit_kind(self) -> Option<crate::engine::EngineKind> {
        match self {
            Self::Auto => None,
            Self::Docker => Some(crate::engine::EngineKind::Docker),
            Self::Podman => Some(crate::engine::EngineKind::Podman),
        }
    }
}

impl std::fmt::Display for EnginePreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Docker => "docker",
            Self::Podman => "podman",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Cli,
    Environment(&'static str),
    Config(PathBuf),
    Default,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cli => f.write_str("CLI"),
            Self::Environment(name) => write!(f, "environment {name}"),
            Self::Config(path) => write!(f, "config {}", path.display()),
            Self::Default => f.write_str("default"),
        }
    }
}

#[derive(Debug)]
pub struct Settings {
    pub engine: EnginePreference,
    pub saved_engine: Option<EnginePreference>,
    pub image: String,
    pub engine_source: Source,
    pub image_source: Source,
    pub config_path: PathBuf,
    pub config_present: bool,
    pub cache_root: Result<PathBuf, Error>,
}

pub fn xdg_path(
    base: Option<&OsStr>,
    home: Option<&OsStr>,
    fallback: &str,
    variable: &'static str,
) -> Result<PathBuf, Error> {
    if let Some(base) = base.filter(|v| Path::new(v).is_absolute()) {
        return Ok(PathBuf::from(base).join("larainit"));
    }
    let home = home.filter(|v| Path::new(v).is_absolute()).ok_or_else(|| {
        Error::operational(
            "resolve user path",
            variable,
            "no absolute XDG base or HOME",
            format!("Set an absolute {variable} or HOME"),
        )
    })?;
    Ok(PathBuf::from(home).join(fallback).join("larainit"))
}

pub fn config_path() -> Result<PathBuf, Error> {
    Ok(xdg_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        ".config",
        "XDG_CONFIG_HOME",
    )?
    .join("config.toml"))
}

pub fn cache_root() -> Result<PathBuf, Error> {
    xdg_path(
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        ".cache",
        "XDG_CACHE_HOME",
    )
}

pub fn read(path: &Path) -> Result<(RawConfig, bool), Error> {
    let result = (|| {
        let Some(directory) = ConfigDirectory::open(path, false)? else {
            return Ok((RawConfig::default(), false));
        };
        let snapshot = snapshot(&directory.file, directory.target.as_os_str())?;
        directory.verify()?;
        match snapshot {
            Some(snapshot) => Ok((decode(&snapshot.bytes, path)?, true)),
            None => Ok((RawConfig::default(), false)),
        }
    })();
    result.map_err(|cause: io::Error| config_error(path, cause))
}

fn config_error(path: &Path, cause: impl std::fmt::Display) -> Error {
    Error::operational(
        "access configuration",
        path.display().to_string(),
        cause,
        "Use safe owner-controlled regular files/directories on a supported local filesystem; Larainit does not repair permissions or links",
    )
}

fn decode(bytes: &[u8], path: &Path) -> io::Result<RawConfig> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| io::Error::other("configuration is not UTF-8"))?;
    let raw: RawConfig = toml::from_str(text)
        .map_err(|error: toml::de::Error| {
            Error::operational(
                "parse configuration",
                path.display().to_string(),
                error.message(),
                "Use only optional string engine and image keys",
            )
        })
        .map_err(|error| io::Error::other(error.to_string()))?;
    // Optional engine/image strings; unknown keys rejected; all present values
    // validated even when an override wins.
    let source = Source::Config(path.into());
    if let Some(engine) = &raw.engine {
        EnginePreference::parse(OsStr::new(engine), &source)
            .map_err(|error| io::Error::other(error.to_string()))?;
    }
    if let Some(image) = &raw.image {
        validate_image(image, &source).map_err(|error| io::Error::other(error.to_string()))?;
    }
    Ok(raw)
}

fn require(condition: bool, message: &'static str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn same_identity(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn directory_metadata(file: &File) -> io::Result<Metadata> {
    let metadata = file.metadata()?;
    let uid = nix::unistd::geteuid().as_raw();
    require(
        metadata.is_dir() && (metadata.uid() == 0 || metadata.uid() == uid),
        "unsafe configuration directory type/owner",
    )?;
    // Root-owned sticky ancestors (e.g. /tmp) protect the next owned component.
    require(
        metadata.mode() & 0o022 == 0 || (metadata.uid() == 0 && metadata.mode() & 0o1000 != 0),
        "configuration ancestor is writable by other users",
    )?;
    require(metadata.nlink() > 0, "configuration directory was unlinked")?;
    Ok(metadata)
}

struct ConfigDirectory {
    file: File,
    target: OsString,
    ancestors: Vec<(File, OsString, Metadata)>,
}

impl ConfigDirectory {
    fn open(path: &Path, create: bool) -> io::Result<Option<Self>> {
        require(path.is_absolute(), "configuration path must be absolute")?;
        let target = path
            .file_name()
            .ok_or_else(|| io::Error::other("configuration filename required"))?
            .to_owned();
        let mut file = File::from(nix::fcntl::open(
            "/",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        directory_metadata(&file)?;
        let mut ancestors = Vec::new();
        for component in path
            .parent()
            .ok_or_else(|| io::Error::other("configuration parent required"))?
            .components()
        {
            let name = match component {
                Component::RootDir | Component::CurDir => continue,
                Component::Normal(name) => name,
                _ => {
                    return Err(io::Error::other(
                        "parent traversal in configuration path is unsupported",
                    ));
                }
            };
            let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
            let next = match openat(&file, name, flags, Mode::empty()) {
                Ok(next) => File::from(next),
                Err(nix::errno::Errno::ENOENT) if create => {
                    supported_filesystem(&file)?;
                    match mkdirat(&file, name, Mode::S_IRWXU) {
                        Ok(()) => file.sync_all()?,
                        Err(nix::errno::Errno::EEXIST) => {}
                        Err(cause) => return Err(cause.into()),
                    }
                    File::from(openat(&file, name, flags, Mode::empty())?)
                }
                Err(nix::errno::Errno::ENOENT) => return Ok(None),
                Err(cause) => return Err(cause.into()),
            };
            let metadata = directory_metadata(&next)?;
            ancestors.push((file, name.to_owned(), metadata));
            file = next;
        }
        require(
            file.metadata()?.uid() == nix::unistd::geteuid().as_raw(),
            "application config directory must belong to the current user",
        )?;
        require(
            file.metadata()?.mode() & 0o022 == 0,
            "application config directory is writable by other users",
        )?;
        let directory = Self {
            file,
            target,
            ancestors,
        };
        directory.verify()?;
        Ok(Some(directory))
    }

    fn verify(&self) -> io::Result<()> {
        for (parent, name, original) in &self.ancestors {
            directory_metadata(parent)?;
            let current = File::from(openat(
                parent,
                name.as_os_str(),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?);
            let metadata = directory_metadata(&current)?;
            require(
                same_identity(original, &metadata)
                    && original.uid() == metadata.uid()
                    && original.mode() == metadata.mode(),
                "configuration ancestor changed during access",
            )?;
        }
        directory_metadata(&self.file)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    identity: (u64, u64, u32, u32, u64, u64, i64, i64, i64, i64),
    bytes: Vec<u8>,
}

fn file_identity(metadata: &Metadata) -> (u64, u64, u32, u32, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.uid(),
        metadata.mode(),
        metadata.nlink(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn snapshot(directory: &File, name: &OsStr) -> io::Result<Option<Snapshot>> {
    let file = match openat(
        directory,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => File::from(file),
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(cause) => return Err(cause.into()),
    };
    let metadata = file.metadata()?;
    require(
        metadata.is_file()
            && metadata.uid() == nix::unistd::geteuid().as_raw()
            && metadata.nlink() == 1,
        "configuration must be an owned regular file with one link",
    )?;
    require(
        metadata.mode() & 0o022 == 0 && metadata.mode() & 0o400 != 0,
        "unsafe configuration file permissions",
    )?;
    let mut bytes = Vec::new();
    // ponytail: 64 KiB config ceiling; revisit only if recognized settings grow.
    (&file).take(65537).read_to_end(&mut bytes)?;
    require(bytes.len() <= 65536, "configuration exceeds 64 KiB")?;
    require(
        file_identity(&metadata) == file_identity(&file.metadata()?),
        "configuration changed while reading",
    )?;
    let named = File::from(openat(
        directory,
        name,
        OFlag::O_PATH | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    require(
        file_identity(&metadata) == file_identity(&named.metadata()?),
        "configuration name changed while reading",
    )?;
    Ok(Some(Snapshot {
        identity: file_identity(&metadata),
        bytes,
    }))
}

#[derive(Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    DurablySaved,
    CommittedDurabilityUnconfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveStage {
    Locked,
    BeforeWrite,
    BeforeFileSync,
    FileSynced,
    BeforeRename,
    Renamed,
}

struct ConfigTemp<'a> {
    directory: &'a File,
    file: File,
    name: String,
    metadata: Metadata,
    renamed: bool,
}

impl Drop for ConfigTemp<'_> {
    fn drop(&mut self) {
        if self.renamed {
            return;
        }
        // A replaced temporary name is not ours to delete.
        if let Ok(fd) = openat(
            self.directory,
            self.name.as_str(),
            OFlag::O_PATH | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            if File::from(fd)
                .metadata()
                .is_ok_and(|current| same_identity(&self.metadata, &current))
            {
                let _ = nix::unistd::unlinkat(
                    self.directory,
                    self.name.as_str(),
                    nix::unistd::UnlinkatFlags::NoRemoveDir,
                );
            }
        }
    }
}

pub fn save(
    path: &Path,
    preference: EnginePreference,
    signals: &crate::runner::Signals,
) -> Result<SaveOutcome, Error> {
    save_transaction(path, preference, Some(signals), |_, _, _| Ok(()))
        .map_err(|cause| config_error(path, cause))
}

fn supported_filesystem(directory: &File) -> io::Result<()> {
    // Network/FUSE/unknown filesystem lock/durability semantics are unsupported.
    let filesystem = nix::sys::statfs::fstatfs(directory)?.filesystem_type().0;
    require(
        [
            nix::libc::EXT4_SUPER_MAGIC,
            nix::libc::XFS_SUPER_MAGIC,
            nix::libc::BTRFS_SUPER_MAGIC,
            nix::libc::TMPFS_MAGIC,
            nix::libc::OVERLAYFS_SUPER_MAGIC,
        ]
        .contains(&filesystem),
        "unsupported configuration filesystem guarantees",
    )
}

fn save_transaction(
    path: &Path,
    preference: EnginePreference,
    signals: Option<&crate::runner::Signals>,
    mut step: impl FnMut(SaveStage, &File, Option<&str>) -> io::Result<()>,
) -> io::Result<SaveOutcome> {
    let directory = ConfigDirectory::open(path, true)?
        .ok_or_else(|| io::Error::other("configuration directory absent"))?;
    supported_filesystem(&directory.file)?;
    let mut lock_file = directory.file.try_clone()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let _lock = loop {
        if signals.is_some_and(|signals| signals.signal().is_some()) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "preference save cancelled before commit",
            ));
        }
        match Flock::lock(lock_file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => break lock,
            Err((file, nix::errno::Errno::EWOULDBLOCK)) if Instant::now() < deadline => {
                lock_file = file;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err((_, cause)) => return Err(cause.into()),
        }
    };
    directory.verify()?;
    step(SaveStage::Locked, &directory.file, None)?;
    let original = snapshot(&directory.file, &directory.target)?;
    let mut raw = original
        .as_ref()
        .map(|snapshot| decode(&snapshot.bytes, path))
        .transpose()?
        .unwrap_or_default();
    raw.engine = Some(preference.to_string());
    let bytes = toml::to_string(&raw)
        .map_err(|_| io::Error::other("serialize recognized configuration values"))?
        .into_bytes();
    let mut random = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let name = format!(
        ".config-{}.tmp",
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let file = File::from(openat(
        &directory.file,
        name.as_str(),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::S_IRUSR | Mode::S_IWUSR,
    )?);
    let metadata = file.metadata()?;
    let mut temp = ConfigTemp {
        directory: &directory.file,
        file,
        name,
        metadata,
        renamed: false,
    };
    require(
        temp.metadata.mode() & 0o777 == 0o600 && temp.metadata.nlink() == 1,
        "private configuration temporary file required",
    )?;
    step(SaveStage::BeforeWrite, &directory.file, Some(&temp.name))?;
    temp.file.write_all(&bytes)?;
    step(SaveStage::BeforeFileSync, &directory.file, Some(&temp.name))?;
    temp.file.sync_all()?;
    step(SaveStage::FileSynced, &directory.file, Some(&temp.name))?;
    directory.verify()?;
    require(
        snapshot(&directory.file, &directory.target)? == original,
        "configuration changed externally before commit",
    )?;
    let written = snapshot(&directory.file, OsStr::new(&temp.name))?
        .ok_or_else(|| io::Error::other("configuration temporary file disappeared"))?;
    require(
        written.identity.0 == temp.metadata.dev()
            && written.identity.1 == temp.metadata.ino()
            && written.bytes == bytes
            && written.identity.3 & 0o777 == 0o600,
        "configuration temporary file was replaced or changed",
    )?;
    if signals.is_some_and(|signals| signals.signal().is_some()) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "preference save cancelled before commit",
        ));
    }
    step(SaveStage::BeforeRename, &directory.file, Some(&temp.name))?;
    // Commit point. The final race with an uncoordinated external writer is excluded.
    renameat(
        &directory.file,
        temp.name.as_str(),
        &directory.file,
        directory.target.as_os_str(),
    )?;
    temp.renamed = true;
    if step(SaveStage::Renamed, &directory.file, None)
        .and_then(|_| directory.file.sync_all())
        .is_err()
    {
        return Ok(SaveOutcome::CommittedDurabilityUnconfirmed);
    }
    Ok(SaveOutcome::DurablySaved)
}

fn pick(
    cli: Option<OsString>,
    environment: Option<OsString>,
    variable: &'static str,
    file: Option<&str>,
    path: &Path,
    default: &str,
) -> (OsString, Source) {
    if let Some(value) = cli {
        return (value, Source::Cli);
    }
    if let Some(value) = environment {
        return (value, Source::Environment(variable));
    }
    if let Some(value) = file {
        return (value.into(), Source::Config(path.into()));
    }
    (default.into(), Source::Default)
}

pub fn resolve(overrides: &Overrides) -> Result<Settings, Error> {
    let path = config_path()?;
    let (raw, config_present) = read(&path)?;
    let saved_engine = raw
        .engine
        .as_deref()
        .map(|value| EnginePreference::parse(OsStr::new(value), &Source::Config(path.clone())))
        .transpose()?;
    let (engine, engine_source) = pick(
        overrides.engine.as_ref().map(OsString::from),
        std::env::var_os("LARAINIT_ENGINE"),
        "LARAINIT_ENGINE",
        raw.engine.as_deref(),
        &path,
        "auto",
    );
    let engine = EnginePreference::parse(&engine, &engine_source)?;
    let (image, image_source) = pick(
        overrides.image.clone(),
        std::env::var_os("LARAINIT_IMAGE"),
        "LARAINIT_IMAGE",
        raw.image.as_deref(),
        &path,
        "larainit:latest",
    );
    let image = image.into_string().map_err(|_| {
        Error::operational(
            "resolve image",
            image_source.to_string(),
            "image reference is not UTF-8",
            "Use a valid local Docker image reference",
        )
    })?;
    validate_image(&image, &image_source)?;
    Ok(Settings {
        engine,
        saved_engine,
        image,
        engine_source,
        image_source,
        config_path: path,
        config_present,
        cache_root: cache_root(),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum PathState {
    Usable,
    Preparable(PathBuf),
    Inaccessible(String),
}

/// Read-only evidence, never a guarantee of future writes.
pub fn observe_directory(path: &Path) -> PathState {
    let mut ancestor = path;
    loop {
        match fs::metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => {
                if let Err(error) = access(
                    ancestor,
                    AccessFlags::R_OK | AccessFlags::W_OK | AccessFlags::X_OK,
                ) {
                    return PathState::Inaccessible(format!("{}: {error}", ancestor.display()));
                }
                return if ancestor == path {
                    PathState::Usable
                } else {
                    PathState::Preparable(ancestor.into())
                };
            }
            Ok(_) => {
                return PathState::Inaccessible(format!(
                    "{} is not a directory",
                    ancestor.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match ancestor.parent() {
                Some(parent) => ancestor = parent,
                None => return PathState::Inaccessible("no accessible ancestor".into()),
            },
            Err(error) => {
                return PathState::Inaccessible(format!("{}: {error}", ancestor.display()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save_fixture() -> (test_support::Fixture, PathBuf) {
        let fixture = test_support::Fixture::new();
        fixture.write("config/config.toml", "engine='docker'\nimage='old:tag'");
        let path = fixture.root.join("config/config.toml");
        (fixture, path)
    }

    #[test]
    fn save_faults_preserve_precommit_data_but_never_roll_back_a_commit() {
        for stage in [
            SaveStage::BeforeWrite,
            SaveStage::BeforeFileSync,
            SaveStage::BeforeRename,
            SaveStage::Renamed,
        ] {
            let (_fixture, path) = save_fixture();
            let old = fs::read(&path).unwrap();
            let result =
                save_transaction(&path, EnginePreference::Podman, None, |current, _, _| {
                    if current == stage {
                        Err(io::Error::other("injected filesystem failure"))
                    } else {
                        Ok(())
                    }
                });
            if stage == SaveStage::Renamed {
                assert_eq!(result.unwrap(), SaveOutcome::CommittedDurabilityUnconfirmed);
                assert_eq!(read(&path).unwrap().0.engine.as_deref(), Some("podman"));
            } else {
                assert!(result.is_err());
                assert_eq!(fs::read(&path).unwrap(), old);
            }
            assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        }
    }

    #[test]
    fn external_target_edits_and_parent_substitution_fail_before_commit() {
        for change in ["replace", "edit", "parent"] {
            let (fixture, path) = save_fixture();
            let old = fs::read(&path).unwrap();
            let result = save_transaction(&path, EnginePreference::Podman, None, |stage, _, _| {
                if stage == SaveStage::FileSynced {
                    match change {
                        "replace" => {
                            fs::remove_file(&path)?;
                            fs::write(&path, "engine='auto'")?;
                        }
                        "edit" => fs::write(&path, "engine='auto'")?,
                        "parent" => {
                            fs::rename(path.parent().unwrap(), fixture.root.join("moved"))?;
                            fs::create_dir(path.parent().unwrap())?;
                            fs::write(&path, "engine='auto'")?;
                        }
                        _ => unreachable!(),
                    }
                }
                Ok(())
            });
            assert!(result.is_err(), "{change}");
            assert_eq!(fs::read(&path).unwrap(), b"engine='auto'");
            if change == "parent" {
                assert_eq!(
                    fs::read(fixture.root.join("moved/config.toml")).unwrap(),
                    old
                );
                assert_eq!(fs::read_dir(fixture.root.join("moved")).unwrap().count(), 1);
            }
        }
    }

    #[test]
    fn substituted_temporary_name_is_not_committed_or_deleted() {
        let (_fixture, path) = save_fixture();
        let old = fs::read(&path).unwrap();
        let mut replacement = None;
        let result = save_transaction(&path, EnginePreference::Podman, None, |stage, _, name| {
            if stage == SaveStage::FileSynced {
                let temp = path.parent().unwrap().join(name.unwrap());
                fs::remove_file(&temp)?;
                fs::write(&temp, b"unrelated data")?;
                replacement = Some(temp);
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), old);
        assert_eq!(fs::read(replacement.unwrap()).unwrap(), b"unrelated data");
    }

    #[test]
    fn transaction_reads_the_latest_recognized_image_after_locking() {
        let (_fixture, path) = save_fixture();
        let outcome = save_transaction(&path, EnginePreference::Auto, None, |stage, _, _| {
            if stage == SaveStage::Locked {
                fs::write(&path, "image='latest:tag'")?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(outcome, SaveOutcome::DurablySaved);
        let raw = read(&path).unwrap().0;
        assert_eq!(raw.engine.as_deref(), Some("auto"));
        assert_eq!(raw.image.as_deref(), Some("latest:tag"));
    }

    #[test]
    fn strict_schema_and_xdg_fallbacks() {
        let raw: RawConfig = toml::from_str("engine='docker'\nimage='custom:tag'").unwrap();
        assert_eq!(raw.engine.as_deref(), Some("docker"));
        for text in ["unknown='x'", "engine=42", "image=[]", "engine='"] {
            assert!(toml::from_str::<RawConfig>(text).is_err());
        }
        assert!(toml::from_str::<RawConfig>("").unwrap().engine.is_none());
        for base in [None, Some(OsStr::new("")), Some(OsStr::new("relative"))] {
            assert_eq!(
                xdg_path(
                    base,
                    Some(OsStr::new("/home/me")),
                    ".config",
                    "XDG_CONFIG_HOME"
                )
                .unwrap(),
                Path::new("/home/me/.config/larainit")
            );
            assert!(xdg_path(base, None, ".config", "XDG_CONFIG_HOME").is_err());
        }
        assert_eq!(
            xdg_path(
                Some(OsStr::new("/absolute")),
                None,
                ".cache",
                "XDG_CACHE_HOME"
            )
            .unwrap(),
            Path::new("/absolute/larainit")
        );
    }

    #[test]
    fn recognized_values_serialize_without_handwritten_escaping() {
        for engine in [None, Some("auto"), Some("docker"), Some("podman")] {
            let raw = RawConfig {
                engine: engine.map(str::to_owned),
                image: Some("registry.example/日本語\\\"image:tag".into()),
            };
            let text = toml::to_string(&raw).unwrap();
            let decoded: RawConfig = toml::from_str(&text).unwrap();
            assert_eq!(decoded.engine, raw.engine);
            assert_eq!(decoded.image, raw.image);
        }
        let text = toml::to_string(&RawConfig::default()).unwrap();
        assert!(toml::from_str::<RawConfig>(&text).unwrap().image.is_none());
    }

    #[test]
    fn precedence_preserves_empty_explicit_overrides() {
        let path = Path::new("/config");
        assert_eq!(
            pick(
                Some("cli".into()),
                Some("env".into()),
                "VAR",
                Some("file"),
                path,
                "default"
            ),
            ("cli".into(), Source::Cli)
        );
        assert_eq!(
            pick(None, Some("".into()), "VAR", Some("file"), path, "default"),
            ("".into(), Source::Environment("VAR"))
        );
        assert_eq!(
            pick(None, None, "VAR", Some("file"), path, "default").1,
            Source::Config(path.into())
        );
        assert_eq!(
            pick(None, None, "VAR", None, path, "default").1,
            Source::Default
        );
        for value in ["", " ", "-bad", "bad\nvalue"] {
            assert!(validate_image(value, &Source::Default).is_err());
        }
        assert!(validate_image("localhost/tool:tag", &Source::Default).is_ok());
    }

    #[test]
    fn observes_without_creating_paths() {
        let root = std::env::temp_dir();
        let path = root
            .join(format!("larainit-observe-{}", std::process::id()))
            .join("child");
        assert!(matches!(observe_directory(&path), PathState::Preparable(_)));
        assert!(!path.exists());
        assert!(matches!(
            observe_directory(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("Cargo.toml")
                    .as_path()
            ),
            PathState::Inaccessible(_)
        ));
    }
}
