use super::{EngineKind, ObservationBudget, ResolvedEngine};
use crate::{
    error::Error,
    runner::{Observation, Signals},
};
use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

// Podman v5.7.0 sqlite_state{,_internal}.go: schema 1, default DB at graphRoot/db.sql.
const DB_QUERY: &str = "SELECT ID, SchemaVersion, OS, StaticDir, TmpDir, GraphRoot, RunRoot, GraphDriver, VolumeDir FROM DBConfig;";

// Live fixture and distribution-startup certification is incomplete. No runtime bypass.
const UBUNTU_PROFILE_CERTIFIED: bool = false;

// Ubuntu 26.04 amd64 candidate, not a version-only allowlist. See docs/podman-profile.md.
const AUDITED_FILES: &[(&str, &str)] = &[
    (
        "/usr/bin/podman",
        "6627da0abfd3b0189498089319f5759c7b132bb6a51130bf8ff43d7f102d9673",
    ),
    (
        "/usr/bin/crun",
        "0e6d2727a3bcb013652543f6c8124a24cdea863ee0a99038c55b3591c0d8c126",
    ),
    (
        "/usr/bin/sqlite3",
        "5a910e452929c124d23d6b90a95fc963c97aa4aea7170894f7f4567deeb5f510",
    ),
    (
        "/usr/bin/bwrap",
        "523da3e7399044be5163aee6f57a77a6bef7454376e28f0a0627920bae1b76b6",
    ),
    (
        "/usr/bin/conmon",
        "856d10b5842ae5dd7f4cf73a60d0f1a764bdcd0cfadc447adb0b0ef41a935412",
    ),
    (
        "/usr/bin/catatonit",
        "402bf3651d96326c61076f60d16d7a2e775f73c3c7884ed2a76d13fb7399a0bd",
    ),
    (
        "/usr/share/containers/containers.conf",
        "027a1d569a50acaf4d75be396781113f3510903af8783cf1cd14add601295dd5",
    ),
    (
        "/usr/share/containers/storage.conf",
        "43af65f53ddf8031c6595bc5a9129753e3b3e48381fd577a9e51498c8bb15775",
    ),
    (
        "/usr/share/containers/seccomp.json",
        "886ae167646b7e5db381ecf7c31e6de720a8e8da15cf3202fe1f67f424af2b75",
    ),
    (
        "/etc/dpkg/dpkg.cfg",
        "fead43b89af3ea5691c48f32d7fe1ba0f7ab229fb5d230f612d76fe8e6f5a015",
    ),
];

// Candidate installed helper fingerprints; live execution/provenance certification remains closed.
const INFO_HELPERS: &[(&str, &str)] = &[
    (
        "/usr/lib/podman/netavark",
        "53fc9fe0ff1d80159713d76ec609984ce38ed390cc1729847aa55f4e6a8e9e2f",
    ),
    (
        "/usr/lib/podman/aardvark-dns",
        "f5de7cbaf10481acf86f8bd039425eeaed7e34cb4e1c62b68d354264565cc1c5",
    ),
    (
        "/usr/bin/pasta",
        "a10789852407f65160c4abfac002efe511b254e6ab2cbfc658e807575db44832",
    ),
    (
        "/usr/bin/pasta.avx2",
        "e02708849808156f0c7f92f39b7d970b01f2d5c392fc559eeb294dbda72abed7",
    ),
    (
        "/usr/bin/slirp4netns",
        "20581c54ee53ae32e908c9b318481e5a71b72a13f850ce41722e402cb524b325",
    ),
    (
        "/usr/bin/dpkg",
        "972003a11f3ae0f5b2556dce1d2c2721fb5119818b9bbef1124293024fdb6517",
    ),
    (
        "/usr/bin/dpkg-query",
        "82a19acac53907f83faca7d6494289fe2d074514cf1b09933114635415c2e876",
    ),
];
const PATH_INFO_HELPERS: &[&str] = &["pasta", "slirp4netns", "dpkg-query"];

struct PreparedProfile {
    database: PathBuf,
    sqlite: PathBuf,
    bubblewrap: PathBuf,
    graph_root: PathBuf,
    run_root: PathBuf,
    static_dir: PathBuf,
    tmp_dir: PathBuf,
    volume_dir: PathBuf,
    crun: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct DbConfig {
    #[serde(rename = "ID")]
    id: u32,
    schema_version: u32,
    #[serde(rename = "OS")]
    os: String,
    static_dir: PathBuf,
    tmp_dir: PathBuf,
    graph_root: PathBuf,
    run_root: PathBuf,
    graph_driver: String,
    volume_dir: PathBuf,
}

// Only the fields used by storage's startup recovery; do not duplicate its entire schema.
#[derive(Deserialize)]
struct StorageRecord {
    id: String,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    flags: HashMap<String, serde_json::Value>,
    #[serde(flatten)]
    other: HashMap<String, serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
struct Info {
    host: Host,
    store: Store,
    version: Version,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Host {
    arch: String,
    os: String,
    service_is_remote: bool,
    security: Security,
    database_backend: String,
    oci_runtime: Runtime,
    distribution: Distribution,
    cgroup_manager: String,
    cgroup_version: String,
    conmon: HelperPath,
    network_backend: String,
    network_backend_info: NetworkHelpers,
    pasta: Program,
    slirp4netns: Program,
}

#[derive(Deserialize)]
struct HelperPath {
    path: PathBuf,
}

#[derive(Deserialize)]
struct NetworkHelpers {
    backend: String,
    path: PathBuf,
    dns: HelperPath,
}

#[derive(Deserialize)]
struct Program {
    executable: PathBuf,
}

#[derive(Deserialize)]
struct Distribution {
    distribution: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Security {
    rootless: bool,
    selinux_enabled: bool,
    apparmor_enabled: bool,
    seccomp_enabled: bool,
    seccomp_profile_path: PathBuf,
}

#[derive(Deserialize)]
struct Runtime {
    name: String,
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Store {
    graph_root: PathBuf,
    run_root: PathBuf,
    volume_path: PathBuf,
    graph_driver_name: String,
    transient_store: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Version {
    version: String,
    os: String,
    os_arch: String,
}

impl PreparedProfile {
    fn metadata_command(&self) -> Command {
        // SQLite -readonly alone can initialize or update WAL shared-memory files.
        let mut command = Command::new(&self.bubblewrap);
        command
            .env_clear()
            .args([
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-user",
                "--unshare-pid",
                "--unshare-net",
                "--die-with-parent",
                "--new-session",
                "--cap-drop",
                "ALL",
                "--disable-userns",
                "--",
            ])
            .arg(&self.sqlite)
            .args(["-init", "/dev/null", "-readonly", "-json"])
            .arg(&self.database)
            .arg(DB_QUERY);
        command
    }

    fn validate_database(&self, bytes: &[u8]) -> Result<(), &'static str> {
        let rows: Vec<DbConfig> = serde_json::from_slice(bytes)
            .map_err(|_| "malformed or incomplete stored Podman configuration")?;
        let [row] = rows.as_slice() else {
            return Err("exactly one initialized DBConfig row is required");
        };
        if row.id != 1
            || row.schema_version != 1
            || row.os != "linux"
            || row.graph_driver != "overlay"
            || row.static_dir != self.static_dir
            || row.tmp_dir != self.tmp_dir
            || row.graph_root != self.graph_root
            || row.run_root != self.run_root
            || row.volume_dir != self.volume_dir
        {
            return Err("stored Podman schema/configuration disagrees with the audited profile");
        }
        Ok(())
    }

    fn validate_info(&self, bytes: &[u8]) -> Result<String, &'static str> {
        let info: Info = serde_json::from_slice(bytes)
            .map_err(|_| "malformed or incomplete Podman info JSON")?;
        if info.host.service_is_remote
            || !info.host.security.rootless
            || info.host.security.selinux_enabled
            || info.host.security.apparmor_enabled
            || !info.host.security.seccomp_enabled
            || info.host.security.seccomp_profile_path
                != Path::new("/usr/share/containers/seccomp.json")
            || info.host.arch != "amd64"
            || info.host.os != "linux"
            || info.host.distribution.distribution != "ubuntu"
            || info.host.distribution.version != "26.04"
            || info.host.cgroup_manager != "systemd"
            || info.host.cgroup_version != "v2"
            || info.host.conmon.path != Path::new("/usr/bin/conmon")
            || info.host.network_backend != "netavark"
            || info.host.network_backend_info.backend != "netavark"
            || info.host.network_backend_info.path != Path::new("/usr/lib/podman/netavark")
            || info.host.network_backend_info.dns.path != Path::new("/usr/lib/podman/aardvark-dns")
            || info.host.pasta.executable != Path::new("/usr/bin/pasta")
            || info.host.slirp4netns.executable != Path::new("/usr/bin/slirp4netns")
            || info.host.database_backend != "sqlite"
            || info.host.oci_runtime.name != "crun"
            || info.host.oci_runtime.path != self.crun
            || info.store.graph_driver_name != "overlay"
            || info.store.transient_store
            || info.store.graph_root != self.graph_root
            || info.store.run_root != self.run_root
            || info.store.volume_path != self.volume_dir
            || info.version.version != "5.7.0"
            || info.version.os != "linux"
            || info.version.os_arch != "linux/amd64"
        {
            return Err(
                "Podman info contradicts the audited local rootless platform/runtime/store profile",
            );
        }
        Ok(info.version.version)
    }

    fn observe(
        &self,
        metadata: &mut Command,
        info: &mut Command,
        signals: Option<&Signals>,
        budget: &mut ObservationBudget,
        recheck: impl FnOnce() -> io::Result<()>,
    ) -> Result<String, Error> {
        let output = budget.observe(metadata, signals).map_err(|cause| {
            Error::operational(
                "read Podman metadata",
                "audited existing state",
                cause,
                "No repair or alternative reader is attempted",
            )
        })?;
        successful(&output)?;
        self.validate_database(&output.stdout).map_err(|cause| {
            Error::operational(
                "validate Podman metadata",
                "audited existing state",
                cause,
                "No Podman probe is authorized by mismatched metadata",
            )
        })?;
        // ponytail: readiness is point-in-time; pin namespaces/configuration if concurrent mutation must be supported.
        // Recheck after metadata, before the engine can refresh.
        recheck().map_err(profile_error)?;
        let output = budget.observe(info, signals).map_err(|cause| {
            Error::operational(
                "observe prepared Podman",
                "audited existing state",
                cause,
                "No initialization, repair or fallback is attempted",
            )
        })?;
        successful(&output)?;
        self.validate_info(&output.stdout).map_err(|cause| {
            Error::operational(
                "validate Podman readiness",
                "audited existing state",
                cause,
                "No run/build is authorized by contradictory info",
            )
        })
    }

    fn check_boot(&self) -> io::Result<()> {
        let boot = read_existing(Path::new("/proc/sys/kernel/random/boot_id"), 0, 128)?;
        let cached = read_existing(
            &self.tmp_dir.join("alive"),
            nix::unistd::geteuid().as_raw(),
            128,
        )?;
        require(
            current_boot(&boot, &cached),
            "missing, empty or stale Podman boot marker",
        )
    }

    fn check_startup(&self) -> io::Result<()> {
        self.check_boot()?;
        self.check_overlay()?;
        self.check_storage_metadata()?;
        let uid = nix::unistd::geteuid().as_raw();
        for name in ["exits", "persist"] {
            required(&self.tmp_dir.join(name), uid, true)?;
        }
        require(
            read_existing(&self.graph_root.join("defaultNetworkBackend"), uid, 64)? == b"netavark",
            "network backend is uninitialized, unknown or requires migration",
        )?;
        required(&self.graph_root.join("networks/netavark.lock"), uid, false)?;
        required(&self.run_root.join("networks/rootless-netns"), uid, true)?;
        // Match common/pkg/config/systemd.go: unreadable journals select the file logger.
        if !(Path::new("/run/systemd/system").is_dir()
            && (readable_journal(Path::new("/run/log/journal"))?
                || readable_journal(Path::new("/var/log/journal"))?))
        {
            required(&self.tmp_dir.join("events/events.log"), uid, false)?;
        }
        Ok(())
    }

    fn check_namespace_view(&self) -> io::Result<()> {
        let uid = nix::unistd::geteuid().as_raw();
        let gid = nix::unistd::getegid().as_raw();
        let pidfile = self.tmp_dir.join("pause.pid");
        let pause_bytes = read_existing(&pidfile, uid, 32)?;
        let pause = pause_pid(&pause_bytes)?;
        let proc = PathBuf::from(format!("/proc/{pause}"));
        let process = required(&proc, uid, true)?;
        require(
            mapped_namespace(&read_existing(&proc.join("uid_map"), uid, 4096)?, uid)
                && mapped_namespace(&read_existing(&proc.join("gid_map"), uid, 4096)?, gid),
            "existing rootless namespace is absent or has unaudited mappings",
        )?;
        // Only these kernel magic links are followed. This never calls setns.
        let user = File::open(proc.join("ns/user"))?;
        let mounts = File::open(proc.join("ns/mnt"))?;
        let root = File::open(proc.join("root"))?;
        check_root_view(&root)?;
        let hook_marker = Path::new("/etc/containers/podman_preexec_hooks.txt");
        absent(hook_marker, 0)?;
        require(
            existing_from(root.try_clone()?, hook_marker, 0, false)?.is_none(),
            "Podman pre-exec hooks are enabled in the pause filesystem view",
        )?;
        require(
            !same_file(&user, &File::open("/proc/self/ns/user")?)?
                && !same_file(&mounts, &File::open("/proc/self/ns/mnt")?)?,
            "pause process must have distinct rootless user and mount namespaces",
        )?;
        let caller = PathBuf::from(format!("/proc/{}", std::process::id()));
        let home = PathBuf::from(
            std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is required"))?,
        );
        // Include configuration/helper search roots and absent overrides, not only existing files.
        let paths = [
            home.as_path(),
            &self.graph_root,
            &self.run_root,
            &self.tmp_dir,
            Path::new("/usr"),
            Path::new("/etc"),
            Path::new("/var"),
            Path::new("/sys"),
            Path::new("/dev"),
            Path::new("/run"),
            Path::new("/proc"),
        ];
        let path_dirs = std::env::split_paths(
            &std::env::var_os("PATH").ok_or_else(|| io::Error::other("PATH is required"))?,
        )
        .map(|path| match std::fs::canonicalize(&path) {
            Ok(path) => Ok(path),
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(path),
            Err(cause) => Err(cause),
        })
        .collect::<io::Result<Vec<_>>>()?;
        let paths = paths
            .into_iter()
            .chain(path_dirs.iter().map(PathBuf::as_path))
            .collect::<Vec<_>>();
        let caller_mounts = read_existing(&caller.join("mountinfo"), uid, 1024 * 1024)?;
        let pause_mounts = read_existing(&proc.join("mountinfo"), uid, 1024 * 1024)?;
        check_mount_views(&caller_mounts, &pause_mounts, &paths)?;
        for path in [
            &self.graph_root,
            &self.run_root,
            &self.static_dir,
            &self.tmp_dir,
            &self.volume_dir,
        ] {
            check_path_view(&root, path, uid, true)?;
        }
        check_path_view(&root, &self.database, uid, false)?;
        for (path, _) in AUDITED_FILES.iter().chain(INFO_HELPERS) {
            check_path_view(&root, Path::new(path), 0, false)?;
        }
        let overlay = self.graph_root.join("overlay");
        check_path_view(&root, &overlay, uid, true)?;
        let overlay = existing_from(root.try_clone()?, &overlay, uid, true)?
            .ok_or_else(|| io::Error::other("pause overlay path is absent"))?;
        check_overlay_backing_filesystem(nix::sys::statfs::fstatfs(&overlay)?.filesystem_type())?;
        require(
            read_existing(&pidfile, uid, 32)? == pause_bytes
                && same_file(&process, &required(&proc, uid, true)?)?
                && same_file(&user, &File::open(proc.join("ns/user"))?)?
                && same_file(&mounts, &File::open(proc.join("ns/mnt"))?)?
                && same_file(&root, &File::open(proc.join("root"))?)?
                && read_existing(&caller.join("mountinfo"), uid, 1024 * 1024)? == caller_mounts
                && read_existing(&proc.join("mountinfo"), uid, 1024 * 1024)? == pause_mounts,
            "pause process, root or mount view changed during passive inspection",
        )
    }

    fn check_storage_metadata(&self) -> io::Result<()> {
        let uid = nix::unistd::geteuid().as_raw();
        for files in [
            &[
                "overlay-layers/layers.json",
                "overlay-layers/volatile-layers.json",
            ][..],
            &["overlay-images/images.json"][..],
            &[
                "overlay-containers/containers.json",
                "overlay-containers/volatile-containers.json",
            ][..],
        ] {
            let mut ids = HashSet::new();
            let mut names = HashSet::new();
            for name in files {
                let path = self.graph_root.join(name);
                if existing(&path, uid, false)?.is_none() {
                    continue;
                }
                // ponytail: 1 MiB per metadata file; audit larger stores before raising this ceiling.
                let bytes = read_existing(&path, uid, 1024 * 1024)?;
                if bytes.is_empty() {
                    continue;
                }
                let records: Option<Vec<StorageRecord>> =
                    serde_json::from_slice(&bytes).map_err(|_| {
                        io::Error::other("malformed or ambiguous storage recovery metadata")
                    })?;
                for record in records.unwrap_or_default() {
                    // Go folds struct-field names, including Unicode. Require canonical recovery keys.
                    require(
                        record.other.keys().all(|key| {
                            key.is_ascii()
                                && !["id", "names", "flags"]
                                    .iter()
                                    .any(|field| key.eq_ignore_ascii_case(field))
                        }),
                        "noncanonical storage recovery field",
                    )?;
                    require(
                        !record.id.is_empty() && ids.insert(record.id),
                        "missing or conflicting storage record ID",
                    )?;
                    require(
                        record
                            .flags
                            .get("incomplete")
                            .is_none_or(|flag| flag == &serde_json::Value::Bool(false)),
                        "incomplete or unknown storage record must not be recovered",
                    )?;
                    for name in record.names {
                        require(
                            !name.is_empty() && names.insert(name),
                            "empty or conflicting storage name must not be repaired",
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn check_overlay(&self) -> io::Result<()> {
        let uid = nix::unistd::geteuid().as_raw();
        let overlay = self.graph_root.join("overlay");
        check_overlay_backing_filesystem(
            nix::sys::statfs::fstatfs(&required(&overlay, uid, true)?)?.filesystem_type(),
        )?;
        let flag = read_existing(&overlay.join(".has-mount-program"), uid, 64)?;
        require(
            flag == b"false",
            "native overlay requires an existing false mount-program marker",
        )?;
        required(&overlay.join("l"), uid, true)?;
        let cache = self.run_root.join("overlay");
        // storage/overlay Init and Status otherwise probe mounts and record new state.
        for (feature, supported) in [
            ("overlay", true),
            ("native-diff()", true),
            ("metacopy()", false),
            ("volatile", true),
            ("idmapped-lower-dir", false),
        ] {
            require(
                read_existing(&cache.join(format!("{feature}-{supported}")), uid, 8192)?.is_empty(),
                "overlay feature cache differs from the candidate profile",
            )?;
            absent(&cache.join(format!("{feature}-{}", !supported)), uid)?;
        }
        for path in [
            "storage.lock",
            "userns.lock",
            "overlay-images/images.lock",
            "overlay-containers/containers.lock",
            "overlay-layers/layers.lock",
        ] {
            required(&self.graph_root.join(path), uid, false)?;
        }
        for path in ["overlay-containers", "overlay-locks"] {
            required(&self.run_root.join(path), uid, true)?;
        }
        required(
            &self.run_root.join("overlay-layers/mountpoints.lock"),
            uid,
            false,
        )?;
        // layerStore.load recovers deletion leftovers as well as overlay staging.
        for path in [
            overlay.join("staging"),
            overlay.join("tempdirs"),
            self.graph_root.join("overlay-layers/tmp"),
        ] {
            if existing(&path, uid, true)?.is_some() {
                require(
                    std::fs::read_dir(path)?.next().is_none(),
                    "pending storage temporary state must not be recovered by a readiness probe",
                )?;
            }
        }
        Ok(())
    }
}

fn check_overlay_backing_filesystem(filesystem: nix::sys::statfs::FsType) -> io::Result<()> {
    // overlay.Init calls quota.NewControl on XFS even with no configured quota.
    require(
        filesystem != nix::sys::statfs::FsType(nix::libc::XFS_SUPER_MAGIC),
        "XFS overlay quota initialization is outside the prepared profile",
    )
}

fn same_file(a: &File, b: &File) -> io::Result<bool> {
    let (a, b) = (a.metadata()?, b.metadata()?);
    Ok((a.dev(), a.ino()) == (b.dev(), b.ino()))
}

fn pause_pid(bytes: &[u8]) -> io::Result<u32> {
    let pid = std::str::from_utf8(bytes)
        .map_err(io::Error::other)?
        .parse::<u32>()
        .map_err(io::Error::other)?;
    // The fallback Go join uses Atoi without trimming before a failure can remove pause.pid.
    require(
        pid > 1 && bytes == pid.to_string().as_bytes(),
        "invalid or noncanonical rootless pause PID",
    )?;
    Ok(pid)
}

fn check_root_view(root: &File) -> io::Result<()> {
    require(
        root.metadata()?.is_dir() && same_file(root, &File::open("/")?)?,
        "pause filesystem root differs from the caller root",
    )
}

fn check_path_view(root: &File, path: &Path, owner: u32, directory: bool) -> io::Result<()> {
    let caller = required(path, owner, directory)?;
    let paused = existing_from(root.try_clone()?, path, owner, directory)?
        .ok_or_else(|| io::Error::other(format!("{}: pause view is absent", path.display())))?;
    require(
        same_file(&caller, &paused)?,
        &format!("{}: pause path identity differs", path.display()),
    )
}

fn mount_path(field: &str) -> io::Result<PathBuf> {
    let mut bytes = field.as_bytes();
    let mut decoded = Vec::new();
    while let Some((&byte, rest)) = bytes.split_first() {
        if byte == b'\\' {
            let escaped = match bytes.get(..4) {
                Some(b"\\040") => b' ',
                Some(b"\\011") => b'\t',
                Some(b"\\012") => b'\n',
                Some(b"\\134") => b'\\',
                _ => return Err(io::Error::other("unknown mountinfo path escape")),
            };
            decoded.push(escaped);
            bytes = &bytes[4..];
        } else {
            decoded.push(byte);
            bytes = rest;
        }
    }
    let path = PathBuf::from(OsString::from_vec(decoded));
    require(
        path.is_absolute()
            && !field.contains('\0')
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
            && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str(),
        "invalid or noncanonical mountinfo path",
    )?;
    Ok(path)
}

fn check_mount_root(field: &str, filesystem: &str) -> io::Result<()> {
    if filesystem != "nsfs" {
        return mount_path(field).map(|_| ());
    }
    // nsfs.show_path emits a namespace object name, not an absolute filesystem path.
    let (kind, inode) = field
        .split_once(":[")
        .and_then(|(kind, rest)| rest.strip_suffix(']').map(|inode| (kind, inode)))
        .ok_or_else(|| io::Error::other("malformed nsfs root identity"))?;
    require(
        ["cgroup", "ipc", "mnt", "net", "pid", "time", "user", "uts"].contains(&kind)
            && inode
                .parse::<u64>()
                .is_ok_and(|id| id > 0 && id.to_string() == inode),
        "unknown or noncanonical nsfs root identity",
    )
}

fn mount_view(
    bytes: &[u8],
    paths: &[&Path],
    pause: bool,
) -> io::Result<HashMap<PathBuf, Vec<String>>> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let mut view = HashMap::new();
    let mut ids = HashSet::new();
    let mut binfmt = Vec::new();
    for line in text.lines() {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let separator = fields
            .iter()
            .position(|field| *field == "-")
            .ok_or_else(|| io::Error::other("missing mountinfo separator"))?;
        require(
            separator >= 6 && fields.len() == separator + 4,
            "malformed mountinfo row",
        )?;
        let id = fields[0].parse::<u64>().map_err(io::Error::other)?;
        let parent = fields[1].parse::<u64>().map_err(io::Error::other)?;
        require(
            id > 0
                && parent > 0
                && ids.insert(id)
                && fields[2]
                    .split_once(':')
                    .is_some_and(|(a, b)| a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok()),
            "duplicate or invalid mountinfo ID or device",
        )?;
        check_mount_root(fields[3], fields[separator + 1]).map_err(|cause| {
            io::Error::other(format!(
                "mount {id}: root {:?} ({}): {cause}",
                fields[3],
                fields[separator + 1]
            ))
        })?;
        let point = mount_path(fields[4]).map_err(|cause| {
            io::Error::other(format!("mount {id}: mountpoint {:?}: {cause}", fields[4]))
        })?;
        let relevant = paths
            .iter()
            .any(|path| path.starts_with(&point) || point.starts_with(path));
        require(
            !relevant
                || point == Path::new("/proc/sys/fs/binfmt_misc")
                || !point.starts_with("/proc/sys/fs/binfmt_misc"),
            "nested mount below binfmt_misc is outside the prepared profile",
        )?;
        for field in &fields[6..separator] {
            if *field == "unbindable" {
                require(!pause || !relevant, "unbindable pause mount is unsupported")?;
                continue;
            }
            let (kind, id) = field
                .split_once(':')
                .ok_or_else(|| io::Error::other("unknown mountinfo propagation field"))?;
            require(
                ["shared", "master", "propagate_from"].contains(&kind)
                    && id.parse::<u64>().is_ok_and(|id| id > 0),
                "unknown mountinfo propagation field",
            )?;
            require(
                !pause || !relevant || kind != "shared",
                "shared pause mount may propagate setup or cleanup",
            )?;
        }
        if relevant {
            let signature = fields[2..6]
                .iter()
                .chain(&fields[separator + 1..])
                .map(|field| (*field).to_owned())
                .collect::<Vec<_>>();
            if point == Path::new("/proc/sys/fs/binfmt_misc") {
                binfmt.push((id, parent, signature));
                continue;
            }
            require(
                view.insert(point.clone(), signature).is_none(),
                &format!(
                    "{}: stacked or duplicate relevant mount is unsupported",
                    point.display()
                ),
            )?;
        }
    }
    if !binfmt.is_empty() {
        // Linux mountinfo represents the already-triggered systemd automount as a parent chain.
        // Retain both signatures; don't pick a row by enumeration order or maximum mount ID.
        if binfmt.len() == 2 && binfmt[0].2[4] == "binfmt_misc" {
            binfmt.swap(0, 1);
        }
        let signature = match binfmt.as_slice() {
            [(id, parent, row)] if id != parent && row[1] == "/" && row[4] == "binfmt_misc" => {
                row.clone()
            }
            [(id, parent, base), (top_id, top_parent, top)]
                if parent != id
                    && parent != top_id
                    && top_parent == id
                    && base[1] == "/"
                    && top[1] == "/"
                    && base[4] == "autofs"
                    && top[4] == "binfmt_misc" =>
            {
                [base.as_slice(), top.as_slice()].concat()
            }
            _ => {
                return Err(io::Error::other(
                    "/proc/sys/fs/binfmt_misc: untriggered automount or ambiguous/unsupported mount stack",
                ));
            }
        };
        view.insert(PathBuf::from("/proc/sys/fs/binfmt_misc"), signature);
    }
    require(
        view.contains_key(Path::new("/")),
        "mountinfo lacks a visible filesystem root",
    )?;
    Ok(view)
}

fn check_mount_views(caller: &[u8], paused: &[u8], paths: &[&Path]) -> io::Result<()> {
    let caller = mount_view(caller, paths, false)
        .map_err(|cause| io::Error::other(format!("caller mount view: {cause}")))?;
    let paused = mount_view(paused, paths, true)
        .map_err(|cause| io::Error::other(format!("pause mount view: {cause}")))?;
    if let Some(path) = caller
        .keys()
        .chain(paused.keys())
        .find(|path| caller.get(*path) != paused.get(*path))
    {
        return Err(io::Error::other(format!(
            "caller and pause mount views differ at {path:?}"
        )));
    }
    Ok(())
}

fn profile_error(cause: impl std::fmt::Display) -> Error {
    Error::operational(
        "check Podman profile",
        "audited Ubuntu 26.04 rootless/crun/SQLite profile",
        format!("unsupported/not checked: {cause}"),
        "Use the independently prepared certified-profile requirements in docs/podman-profile.md; no installation, refresh, repair or alternative profile is attempted",
    )
}

fn require(ok: bool, cause: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(cause))
    }
}

// Follow the config module's ownership/no-follow policy without allowing creation.
// Each component is opened relative to the previously checked directory descriptor.
fn existing(path: &Path, owner: u32, directory: bool) -> io::Result<Option<File>> {
    let root = File::from(open(
        "/",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    existing_from(root, path, owner, directory)
}

fn existing_from(
    mut file: File,
    path: &Path,
    owner: u32,
    directory: bool,
) -> io::Result<Option<File>> {
    require(path.is_absolute(), "profile path must be absolute")?;
    let names = path
        .components()
        .filter(|c| *c != Component::RootDir)
        .collect::<Vec<_>>();
    for (index, component) in names.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(io::Error::other("unsupported profile path traversal"));
        };
        let ancestor = file.metadata()?;
        require(
            ancestor.is_dir()
                && (ancestor.uid() == 0 || ancestor.uid() == owner)
                && (ancestor.mode() & 0o022 == 0
                    || (ancestor.uid() == 0 && ancestor.mode() & 0o1000 != 0))
                && ancestor.nlink() > 0,
            "unsafe profile ancestor",
        )?;
        let last = index + 1 == names.len();
        let mut flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK;
        if !last || directory {
            flags |= OFlag::O_DIRECTORY;
        }
        file = match openat(&file, *name, flags, Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(nix::errno::Errno::ENOENT) => return Ok(None),
            Err(cause) => return Err(cause.into()),
        };
    }
    let metadata = file.metadata()?;
    require(
        metadata.uid() == owner
            && metadata.mode() & 0o022 == 0
            && metadata.nlink() > 0
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            },
        "unsafe profile type, owner or permissions",
    )?;
    Ok(Some(file))
}

fn required(path: &Path, owner: u32, directory: bool) -> io::Result<File> {
    existing(path, owner, directory)
        .map_err(|cause| io::Error::other(format!("{}: {cause}", path.display())))?
        .ok_or_else(|| {
            io::Error::other(format!(
                "{}: required prepared path is absent",
                path.display()
            ))
        })
}

fn read_existing(path: &Path, owner: u32, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    required(path, owner, false)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= limit,
        "profile metadata exceeds its read limit",
    )?;
    Ok(bytes)
}

fn absent(path: &Path, owner: u32) -> io::Result<()> {
    require(
        existing(path, owner, false)
            .map_err(|cause| io::Error::other(format!("{}: {cause}", path.display())))?
            .is_none(),
        &format!("{}: unaudited override is present", path.display()),
    )
}

fn refuse_helpers(directory: &Path, names: &[&str], owner: u32) -> io::Result<()> {
    for name in names {
        absent(&directory.join(name), owner)?;
    }
    Ok(())
}

// Permit only an exact package-managed link through checked parent directories.
// The target is independently ownership-checked and fingerprinted by the caller.
fn packaged_link(path: &Path, value: &Path, owner: u32) -> io::Result<()> {
    required(
        path.parent()
            .ok_or_else(|| io::Error::other("package link has no parent"))?,
        owner,
        true,
    )?;
    let metadata = std::fs::symlink_metadata(path)?;
    require(
        metadata.is_symlink() && metadata.uid() == owner && std::fs::read_link(path)? == value,
        &format!("{}: unaudited package-managed link", path.display()),
    )
}

fn readable_journal(path: &Path) -> io::Result<bool> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(cause)
            if matches!(
                cause.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(false);
        }
        Err(cause) => return Err(cause),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() && std::fs::read_dir(entry.path()).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn current_boot(boot: &[u8], cached: &[u8]) -> bool {
    boot.len() == 37
        && boot[36] == b'\n'
        && boot[..36].iter().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                *b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        && boot == cached
}

fn supported_environment(key: &std::ffi::OsStr) -> bool {
    let key = key.as_encoded_bytes();
    ![
        b"CONTAINER".as_slice(),
        b"_CONTAINERS_",
        b"PODMAN_",
        b"STORAGE_",
        b"LD_",
        b"SQLITE_",
        b"BWRAP_",
        b"DPKG_",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
        && ![b"GCONV_PATH".as_slice(), b"GLIBC_TUNABLES", b"container"].contains(&key)
}

fn mapped_namespace(bytes: &[u8], host: u32) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let rows = text
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(str::parse::<u32>)
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>();
    matches!(rows, Ok(rows) if rows.len() == 2 && rows[0] == [0, host, 1]
        && rows[1].len() == 3 && rows[1][0] == 1 && rows[1][1] >= 65536 && rows[1][2] == 65536)
}

fn prepared_profile(executable: &Path) -> io::Result<PreparedProfile> {
    require(
        cfg!(all(target_os = "linux", target_arch = "x86_64"))
            && executable == Path::new("/usr/bin/podman"),
        "executable/platform has no audited profile",
    )?;
    let uid = nix::unistd::geteuid().as_raw();
    let gid = nix::unistd::getegid().as_raw();
    require(
        uid != 0 && nix::unistd::getuid().as_raw() == uid && nix::unistd::getgid().as_raw() == gid,
        "rootful or changed process identity is unsupported",
    )?;
    for (key, _) in std::env::vars_os() {
        require(
            supported_environment(&key),
            "engine, storage, loader or package-query environment override is present",
        )?;
    }
    let home = PathBuf::from(
        std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is required"))?,
    );
    let passwd = read_existing(Path::new("/etc/passwd"), 0, 64 * 1024)?;
    let passwd = std::str::from_utf8(&passwd).map_err(io::Error::other)?;
    let account = passwd
        .lines()
        .map(|line| line.split(':').collect::<Vec<_>>())
        .find(|fields| fields.len() == 7 && fields[2].parse::<u32>() == Ok(uid))
        .ok_or_else(|| io::Error::other("a local account is required"))?;
    require(
        Path::new(account[5]) == home
            && std::env::var_os("USER").is_none_or(|name| name == account[0]),
        "HOME/USER disagrees with the local account",
    )?;
    required(&home, uid, true)?;
    let runtime = PathBuf::from(format!("/run/user/{uid}"));
    for (key, path) in [
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_RUNTIME_DIR", runtime.clone()),
    ] {
        require(
            std::env::var_os(key).is_none_or(|value| Path::new(&value) == path)
                && (key != "XDG_RUNTIME_DIR" || std::env::var_os(key).is_some()),
            "XDG profile override or missing runtime directory",
        )?;
    }
    require(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS")
            .is_none_or(|value| value == format!("unix:path={}/bus", runtime.display()).as_str()),
        "unaudited session bus address",
    )?;
    let os_release = read_existing(Path::new("/usr/lib/os-release"), 0, 4096)?;
    let os_release = std::str::from_utf8(&os_release).map_err(io::Error::other)?;
    require(
        os_release.lines().any(|line| line == "ID=ubuntu")
            && os_release
                .lines()
                .any(|line| line == "VERSION_ID=\"26.04\"")
            && std::fs::canonicalize("/etc/os-release")? == Path::new("/usr/lib/os-release"),
        "unaudited distribution",
    )?;
    let kernel = read_existing(Path::new("/proc/sys/kernel/osrelease"), 0, 256)?;
    require(
        !String::from_utf8_lossy(&kernel)
            .to_ascii_lowercase()
            .contains("microsoft"),
        "WSL is not audited",
    )?;
    read_existing(Path::new("/sys/fs/cgroup/cgroup.controllers"), 0, 4096)?;
    required(Path::new("/run/systemd/system"), 0, true)?;
    absent(Path::new("/sys/fs/selinux/enforce"), 0)?;
    for path in ["/run/.containerenv", "/.dockerenv"] {
        absent(Path::new(path), 0)?;
    }
    for path in [
        "/etc/containers/containers.conf",
        "/etc/containers/containers.conf.d",
        "/etc/containers/containers.rootless.conf",
        "/etc/containers/containers.rootless.conf.d",
        "/etc/containers/storage.conf",
        "/etc/containers/seccomp.json",
        "/etc/containers/mounts.conf",
        "/usr/share/containers/mounts.conf",
        "/etc/containers/oci/hooks.d",
        "/usr/share/containers/oci/hooks.d",
    ] {
        absent(Path::new(path), 0)?;
    }
    for name in [
        "containers.conf",
        "containers.conf.d",
        "storage.conf",
        "mounts.conf",
        "podman-connections.json",
    ] {
        absent(&home.join(".config/containers").join(name), uid)?;
    }
    absent(&home.join(".dpkg.cfg"), uid)?;
    let dpkg_dropins = Path::new("/etc/dpkg/dpkg.cfg.d");
    if existing(dpkg_dropins, 0, true)?.is_some() {
        require(
            std::fs::read_dir(dpkg_dropins)?.next().is_none(),
            "package-query configuration drop-ins are unaudited",
        )?;
    }
    // version.Package uses absolute paths and can fall through to every packager.
    for path in [
        "/usr/bin/rpm",
        "/usr/bin/dlocate",
        "/usr/bin/pacman",
        "/usr/bin/qfile",
        "/usr/bin/equery",
        "/usr/local/sbin/pkg",
    ] {
        absent(Path::new(path), 0)?;
    }
    packaged_link(Path::new("/sbin"), Path::new("usr/sbin"), 0)?;
    absent(Path::new("/usr/sbin/apk"), 0)?;
    // Ubuntu installs ordinary helpers here; only conmon would override the pinned binary.
    for path in [
        "/usr/libexec/podman/conmon",
        "/usr/local/libexec/podman",
        "/usr/local/lib/podman",
        "/usr/lib/podman/conmon",
    ] {
        absent(Path::new(path), 0)?;
    }
    packaged_link(
        Path::new("/usr/libexec/podman/catatonit"),
        Path::new("../../bin/catatonit"),
        0,
    )?;
    refuse_helpers(
        Path::new("/usr/libexec/podman"),
        &["netavark", "aardvark-dns", "pasta", "slirp4netns"],
        0,
    )?;
    refuse_helpers(Path::new("/usr/lib/podman"), &["pasta", "slirp4netns"], 0)?;
    // Package-version queries and optional helpers must not resolve into user-writable PATH entries.
    for path in std::env::split_paths(
        &std::env::var_os("PATH").ok_or_else(|| io::Error::other("PATH is required"))?,
    ) {
        require(path.is_absolute(), "relative PATH entry is not audited")?;
        match std::fs::canonicalize(&path) {
            Ok(path) => {
                required(&path, 0, true)?;
                if path != Path::new("/usr/bin") {
                    refuse_helpers(&path, PATH_INFO_HELPERS, 0)?;
                }
            }
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {
                absent(&path.join("unused"), 0)?;
            }
            Err(cause) => return Err(cause),
        }
    }
    let graph = home.join(".local/share/containers/storage");
    let profile = PreparedProfile {
        database: graph.join("db.sql"),
        sqlite: "/usr/bin/sqlite3".into(),
        bubblewrap: "/usr/bin/bwrap".into(),
        static_dir: graph.join("libpod"),
        tmp_dir: runtime.join("libpod/tmp"),
        volume_dir: graph.join("volumes"),
        graph_root: graph,
        run_root: runtime.join("containers"),
        crun: "/usr/bin/crun".into(),
    };
    for path in [
        &profile.graph_root,
        &profile.run_root,
        &profile.static_dir,
        &profile.tmp_dir,
        &profile.volume_dir,
    ] {
        required(path, uid, true)?;
    }
    let mut header = [0; 16];
    required(&profile.database, uid, false)?.read_exact(&mut header)?;
    require(
        &header == b"SQLite format 3\0",
        "database is not an existing SQLite store",
    )?;
    absent(&profile.static_dir.join("bolt_state.db"), uid)?;
    profile.check_startup()?;
    required(&profile.tmp_dir.join("alive.lck"), uid, false)?;
    require(
        required(
            Path::new(&format!("/dev/shm/libpod_rootless_lock_{uid}")),
            uid,
            false,
        )?
        .metadata()?
        .len()
            > 0,
        "rootless lock state is not initialized",
    )?;
    profile.check_namespace_view()?;
    Ok(profile)
}

fn certified_profile(
    executable: &Path,
    signals: Option<&Signals>,
    budget: &mut ObservationBudget,
) -> Result<PreparedProfile, Error> {
    if !UBUNTU_PROFILE_CERTIFIED {
        return Err(profile_error(
            "no certified prepared-storage profile is available; Ubuntu 26.04/Podman 5.7.0 candidate certification is pending",
        ));
    }
    candidate_profile(executable, signals, budget)
}

fn candidate_profile(
    executable: &Path,
    signals: Option<&Signals>,
    budget: &mut ObservationBudget,
) -> Result<PreparedProfile, Error> {
    let profile = prepared_profile(executable).map_err(profile_error)?;
    // Ubuntu 26.04's Rust coreutils uses this exact root-controlled package link.
    // Other symlinks remain unsupported; invoke the original name for argv[0].
    let hash_path = Path::new("/usr/bin/sha256sum");
    let hash_target = if std::fs::symlink_metadata(hash_path)
        .map_err(profile_error)?
        .is_symlink()
    {
        packaged_link(
            hash_path,
            Path::new("../lib/cargo/bin/coreutils/sha256sum"),
            0,
        )
        .map_err(profile_error)?;
        Path::new("/usr/lib/cargo/bin/coreutils/sha256sum")
    } else {
        hash_path
    };
    let hash_file = required(hash_target, 0, false).map_err(profile_error)?;
    require(
        hash_file.metadata().map_err(profile_error)?.mode() & 0o6000 == 0,
        "setuid/setgid fingerprint helper is unsupported",
    )
    .map_err(profile_error)?;
    for (path, _) in AUDITED_FILES.iter().chain(INFO_HELPERS) {
        let file = required(Path::new(path), 0, false).map_err(profile_error)?;
        require(
            file.metadata().map_err(profile_error)?.mode() & 0o6000 == 0,
            "setuid/setgid audited files are unsupported",
        )
        .map_err(profile_error)?;
        if INFO_HELPERS.iter().any(|(helper, _)| helper == path) {
            require(
                file.metadata().map_err(profile_error)?.mode() & 0o111 != 0,
                "audited info helper is not executable",
            )
            .map_err(profile_error)?;
        }
    }
    let mut hashes = Command::new(hash_path);
    hashes.env_clear().args(
        AUDITED_FILES
            .iter()
            .chain(INFO_HELPERS)
            .map(|(path, _)| path),
    );
    let output = budget
        .observe(&mut hashes, signals)
        .map_err(profile_error)?;
    let expected = AUDITED_FILES
        .iter()
        .chain(INFO_HELPERS)
        .map(|(path, hash)| format!("{hash}  {path}\n"))
        .collect::<String>();
    if !output.status.success() || output.stdout != expected.as_bytes() {
        return Err(profile_error(
            "binary/configuration fingerprint differs from the audited packages",
        ));
    }
    Ok(profile)
}

fn successful(output: &Observation) -> Result<(), Error> {
    if output.status.success() {
        return Ok(());
    }
    // Never echo metadata or connection output, which may contain private paths/credentials.
    Err(Error::operational(
        "observe Podman profile",
        "selected local profile",
        format!(
            "observation returned status {}",
            crate::error::child_status(output.status)
        ),
        "Check the prepared profile independently; no initialization, repair or fallback is attempted",
    ))
}

pub(super) fn assess(
    executable: PathBuf,
    signals: Option<&Signals>,
    mut budget: ObservationBudget,
) -> Result<ResolvedEngine, Error> {
    let profile = certified_profile(&executable, signals, &mut budget)?;
    let mut command = Command::new(&executable);
    command.args(["info", "--format", "json"]);
    let version = profile.observe(
        &mut profile.metadata_command(),
        &mut command,
        signals,
        &mut budget,
        || {
            profile.check_startup()?;
            profile.check_namespace_view()
        },
    )?;
    Ok(ResolvedEngine {
        kind: EngineKind::Podman,
        executable,
        version,
        selection_reason: "certified prepared rootless Podman readiness observation",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    #[ignore = "explicit dedicated-account audit: LARAINIT_TEST_PODMAN_PROFILE=1; metadata-only, never executes Podman"]
    fn real_ubuntu_candidate_metadata_only() {
        assert_eq!(
            std::env::var("LARAINIT_TEST_PODMAN_PROFILE").as_deref(),
            Ok("1"),
            "explicit fixture authorization is required; no passing skip"
        );
        let signals = Signals::register().unwrap();
        let mut budget = ObservationBudget::new();
        let profile =
            candidate_profile(Path::new("/usr/bin/podman"), Some(&signals), &mut budget).unwrap();
        let snapshot = || {
            let uid = nix::unistd::geteuid().as_raw();
            let mut names = std::fs::read_dir(&profile.graph_root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            names.sort();
            let files = [
                "db.sql",
                "db.sql-wal",
                "db.sql-shm",
                "db.sql-journal",
                "overlay-layers/layers.json",
                "overlay-layers/volatile-layers.json",
                "overlay-images/images.json",
                "overlay-containers/containers.json",
                "overlay-containers/volatile-containers.json",
            ]
            .map(|name| {
                existing(&profile.graph_root.join(name), uid, false)
                    .unwrap()
                    .map(|file| {
                        let metadata = file.metadata().unwrap();
                        let mut bytes = Vec::new();
                        file.take(16 * 1024 * 1024 + 1)
                            .read_to_end(&mut bytes)
                            .unwrap();
                        assert!(
                            bytes.len() <= 16 * 1024 * 1024,
                            "fixture snapshot exceeds audit limit"
                        );
                        (
                            bytes,
                            metadata.dev(),
                            metadata.ino(),
                            metadata.mtime(),
                            metadata.mtime_nsec(),
                            metadata.ctime(),
                            metadata.ctime_nsec(),
                        )
                    })
            });
            (names, files)
        };
        let before = snapshot();
        let output = budget.observe(&mut profile.metadata_command(), Some(&signals));
        let startup = profile
            .check_startup()
            .and_then(|()| profile.check_namespace_view());
        assert!(
            before == snapshot(),
            "metadata reader/startup guard changed database/storage metadata or graph-root entries"
        );
        startup.unwrap();
        let output = output.unwrap();
        successful(&output).unwrap();
        profile.validate_database(&output.stdout).unwrap();
        println!(
            "Candidate paths, namespaces, fingerprints and stored metadata validated; database/storage metadata bytes/identity/mtime/ctime unchanged. No Podman process was executed; production certification remains closed."
        );
    }

    #[test]
    fn installed_executable_cannot_bypass_pending_certification() {
        let mut budget = ObservationBudget::new();
        let result = certified_profile(Path::new("/usr/bin/podman"), None, &mut budget);
        let Err(error) = result else {
            panic!("uncertified installed binary was authorized");
        };
        assert!(error.to_string().contains("certification is pending"));
    }

    fn profile() -> PreparedProfile {
        PreparedProfile {
            database: "/graph/db.sql".into(),
            sqlite: "/usr/bin/sqlite3".into(),
            bubblewrap: "/usr/bin/bwrap".into(),
            graph_root: "/graph".into(),
            run_root: "/run".into(),
            static_dir: "/graph/libpod".into(),
            tmp_dir: "/run/libpod/tmp".into(),
            volume_dir: "/graph/volumes".into(),
            crun: "/usr/bin/crun".into(),
        }
    }

    #[test]
    fn namespace_mount_views_refuse_propagation_remaps_and_ambiguous_rows() {
        let caller = b"33 2 259:8 / / rw,relatime shared:1 - ext4 /dev/disk rw\n32 33 0:28 / /run rw shared:5 - tmpfs tmpfs rw\n";
        let paused = b"620 619 259:8 / / rw,relatime master:1 - ext4 /dev/disk rw\n626 620 0:28 / /run rw master:5 - tmpfs tmpfs rw\n";
        let paths = [Path::new("/home/test/overlay"), Path::new("/run/user/1001")];
        assert!(check_mount_views(caller, paused, &paths).is_ok());
        // IDs/order/propagation may differ; mount roots, sources, flags and topology may not.
        for bad in [
            String::from_utf8_lossy(paused).replace("master:1", "shared:1"),
            String::from_utf8_lossy(paused).replace("master:5", "shared:5 master:5"),
            String::from_utf8_lossy(paused).replace("master:5", "unrecognized:5"),
            String::from_utf8_lossy(paused).replace("master:5", "master:no-number"),
            String::from_utf8_lossy(paused).replace("259:8", "259:9"),
            String::from_utf8_lossy(paused).replace(" / / rw,", " /other / rw,"),
            String::from_utf8_lossy(paused).replace("/dev/disk", "/dev/other"),
            String::from_utf8_lossy(paused).replace("rw,relatime", "ro,relatime"),
            String::from_utf8_lossy(paused).replace("ext4", "xfs"),
            String::from_utf8_lossy(paused).replace(" /run ", " /elsewhere "),
            format!(
                "{}620 619 259:8 / / rw,relatime - ext4 /dev/disk rw\n",
                String::from_utf8_lossy(paused)
            ),
            format!(
                "{}700 620 0:77 / /home/test/overlay rw - tmpfs tmpfs rw\n",
                String::from_utf8_lossy(paused)
            ),
            "620 619 0:28 / /run rw master:5 - tmpfs tmpfs rw\n".into(),
            "".into(),
            "malformed".into(),
        ] {
            assert!(
                check_mount_views(caller, bad.as_bytes(), &paths).is_err(),
                "{bad}"
            );
        }
        let unrelated = b"700 620 0:77 / /unrelated rw shared:7 - tmpfs tmpfs rw\n";
        assert!(
            check_mount_views(caller, &[paused.as_slice(), unrelated].concat(), &paths).is_ok()
        );
        let escaped = b"800 620 0:77 / /home/test/space\\040name rw - tmpfs tmpfs rw\n";
        let mounted = [paused.as_slice(), escaped].concat();
        assert!(
            check_mount_views(
                &mounted,
                &mounted,
                &[Path::new("/home/test/space name/store")]
            )
            .is_ok()
        );
        assert!(
            check_mount_views(
                paused,
                &mounted,
                &[Path::new("/home/test/space name/store")]
            )
            .is_err()
        );
        for bad in [b"\\041".as_slice(), b"\\000", b"\\999", b"\\", b"relative"] {
            let row = [
                b"800 620 0:77 / ".as_slice(),
                bad,
                b" rw - tmpfs tmpfs rw\n",
            ]
            .concat();
            assert!(check_mount_views(&row, &row, &paths).is_err());
        }
    }

    #[test]
    fn pause_pid_requires_canonical_existing_process_number() {
        assert_eq!(pause_pid(b"109516").unwrap(), 109516);
        for bytes in [
            b"".as_slice(),
            b"0",
            b"1",
            b"-1",
            b"+109516",
            b"0109516",
            b"109516\n",
            b" 109516",
            b"109516 ",
            b"109516\0",
            b"4294967296",
            b"not-a-pid",
        ] {
            assert!(pause_pid(bytes).is_err(), "{bytes:?}");
        }
    }

    #[test]
    fn namespace_path_view_uses_readonly_descriptor_walk_and_refuses_remaps() {
        let fixture = crate::test_support::Fixture::new();
        let uid = nix::unistd::geteuid().as_raw();
        fixture.write("view/store", b"preserve metadata");
        let path = fixture.root.join("view/store");
        let before = std::fs::metadata(&path).unwrap();
        let root = File::open("/").unwrap();
        check_root_view(&root).unwrap();
        check_path_view(&root, &path, uid, false).unwrap();
        let different_root = File::open(&fixture.root).unwrap();
        assert!(check_root_view(&different_root).is_err());
        assert!(check_path_view(&different_root, &path, uid, false).is_err());
        fixture.write(
            &format!("remap{}", path.display()),
            b"different namespace file",
        );
        let remapped_root = File::open(fixture.root.join("remap")).unwrap();
        let error = check_path_view(&remapped_root, &path, uid, false).unwrap_err();
        assert!(error.to_string().contains("identity differs"));
        assert!(check_path_view(&root, &fixture.root.join("missing"), uid, false).is_err());
        assert!(!fixture.root.join("missing").exists());
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(
            (before.ino(), before.mtime_nsec(), before.ctime_nsec()),
            (after.ino(), after.mtime_nsec(), after.ctime_nsec())
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve metadata");
    }

    #[test]
    fn namespace_mount_views_accept_nsfs_roots_without_loosening_filesystem_paths() {
        let root = "620 619 259:8 / / rw,relatime master:1 - ext4 /dev/disk rw\n";
        let paths = [Path::new("/run")];
        for kind in ["cgroup", "ipc", "mnt", "net", "pid", "time", "user", "uts"] {
            let mounted = format!(
                "{root}1106 32 0:5 {kind}:[4026533483] /run/docker/netns/example rw master:669 - nsfs nsfs rw\n"
            );
            assert!(
                check_mount_views(mounted.as_bytes(), mounted.as_bytes(), &paths).is_ok(),
                "{kind}"
            );
            let mismatch = check_mount_views(
                mounted.as_bytes(),
                mounted.replace("4026533483", "4026533484").as_bytes(),
                &paths,
            )
            .unwrap_err();
            assert!(mismatch.to_string().contains("/run/docker/netns/example"));
            assert!(
                check_mount_views(
                    mounted.as_bytes(),
                    mounted.replace("nsfs nsfs", "ext4 /dev/disk").as_bytes(),
                    &paths
                )
                .is_err()
            );
            // Namespace object names are never mountpoint paths.
            assert!(
                check_mount_views(
                    mounted.as_bytes(),
                    mounted
                        .replace("/run/docker/netns/example", "net:[4026533483]")
                        .as_bytes(),
                    &paths
                )
                .is_err()
            );
        }
        for name in [
            "net:[0]",
            "net:[+1]",
            "net:[01]",
            "net:[-1]",
            "net:[18446744073709551616]",
            "net:[1",
            "net:1]",
            "net:[1]extra",
            "other:[1]",
            "net:[a]",
            "net:[1]/../x",
            "/",
            "relative",
        ] {
            let mounted = format!(
                "{root}1106 32 0:5 {name} /run/docker/netns/example rw master:669 - nsfs nsfs rw\n"
            );
            assert!(
                check_mount_views(mounted.as_bytes(), mounted.as_bytes(), &paths).is_err(),
                "{name}"
            );
        }
        for name in ["relative", "../escape", "/../escape", "/x/./y", "/x//y"] {
            let mounted = format!("{root}1106 32 8:1 {name} /run/example rw - ext4 /dev/disk rw\n");
            assert!(
                check_mount_views(mounted.as_bytes(), mounted.as_bytes(), &paths).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn namespace_mount_views_accept_only_the_prepared_binfmt_automount_stack() {
        let caller_root = "33 2 259:8 / / rw,relatime shared:1 - ext4 /dev/disk rw\n";
        let pause_root = "620 619 259:8 / / rw,relatime master:1 - ext4 /dev/disk rw\n";
        let caller_stack = "40 29 0:34 / /proc/sys/fs/binfmt_misc rw,relatime shared:13 - autofs systemd-1 rw,fd=36,pgrp=1,timeout=0,minproto=5,maxproto=5,direct,pipe_ino=24436\n172 40 0:46 / /proc/sys/fs/binfmt_misc rw,nosuid,nodev,noexec,relatime shared:139 - binfmt_misc binfmt_misc rw\n";
        let pause_stack = caller_stack
            .replace("40 29 ", "640 629 ")
            .replace("172 40 ", "772 640 ")
            .replace("shared:", "master:");
        let caller = format!("{caller_root}{caller_stack}");
        let paused = format!("{pause_root}{pause_stack}");
        let paths = [Path::new("/proc")];
        assert!(check_mount_views(caller.as_bytes(), paused.as_bytes(), &paths).is_ok());
        let reversed = format!(
            "{pause_root}{}\n{}\n",
            pause_stack.lines().nth(1).unwrap(),
            pause_stack.lines().next().unwrap()
        );
        assert!(check_mount_views(caller.as_bytes(), reversed.as_bytes(), &paths).is_ok());
        let lower_top_id = paused.replace("772 640 ", "120 640 ");
        assert!(check_mount_views(caller.as_bytes(), lower_top_id.as_bytes(), &paths).is_ok());
        let single = format!("{pause_root}{}\n", pause_stack.lines().nth(1).unwrap());
        assert!(check_mount_views(single.as_bytes(), single.as_bytes(), &paths).is_ok());
        for bad in [
            format!("{pause_root}{}\n", pause_stack.lines().next().unwrap()),
            format!("{paused}{}\n", pause_stack.lines().nth(1).unwrap()),
            paused.replace("772 640 ", "772 629 "),
            paused.replace("640 629 ", "640 772 "),
            paused.replace("772 640 ", "640 640 "),
            paused.replace("binfmt_misc binfmt_misc", "tmpfs tmpfs"),
            paused.replace("autofs systemd-1", "tmpfs tmpfs"),
            paused.replace("master:139", "shared:139"),
            paused.replace("master:13", "shared:13"),
            paused.replace("0:46", "0:47"),
            paused.replace("fd=36", "fd=37"),
            paused.replace("0:34 / ", "0:34 /elsewhere "),
            format!(
                "{paused}999 772 0:47 / /proc/sys/fs/binfmt_misc rw - binfmt_misc binfmt_misc rw\n"
            ),
            format!("{paused}999 772 0:47 / /proc/sys/fs/binfmt_misc/status rw - tmpfs tmpfs rw\n"),
        ] {
            assert!(
                check_mount_views(caller.as_bytes(), bad.as_bytes(), &paths).is_err(),
                "{bad}"
            );
        }
        for path in ["/", "/home/test/overlay", "/proc/other"] {
            let stacked = paused.replace("/proc/sys/fs/binfmt_misc", path);
            let relevant = [Path::new(path)];
            assert!(
                check_mount_views(stacked.as_bytes(), stacked.as_bytes(), &relevant).is_err(),
                "{path}"
            );
        }
        assert!(check_mount_views(caller.as_bytes(), single.as_bytes(), &paths).is_err());
    }

    fn database_json(profile: &PreparedProfile) -> Value {
        json!([{"ID":1,"SchemaVersion":1,"OS":"linux", "StaticDir":profile.static_dir,
            "TmpDir":profile.tmp_dir,"GraphRoot":profile.graph_root,"RunRoot":profile.run_root,
            "GraphDriver":"overlay","VolumeDir":profile.volume_dir}])
    }

    #[test]
    fn operator_reported_info_passes_existing_typed_validator() {
        let mut candidate = profile();
        candidate.graph_root = "/home/larainit-podman-test/.local/share/containers/storage".into();
        candidate.run_root = "/run/user/1001/containers".into();
        candidate.volume_dir = candidate.graph_root.join("volumes");
        // Required-field projection of operator output, not raw-byte or engine acceptance evidence.
        let reported = include_bytes!("../../tests/fixtures/podman-ubuntu-reported-info.json");
        assert_eq!(candidate.validate_info(reported).unwrap(), "5.7.0");
    }

    fn info_json(profile: &PreparedProfile) -> Value {
        json!({"host":{"arch":"amd64","os":"linux","serviceIsRemote":false,
            "security":{"rootless":true,"selinuxEnabled":false,"apparmorEnabled":false,"seccompEnabled":true,
                "seccompProfilePath":"/usr/share/containers/seccomp.json"},"databaseBackend":"sqlite",
            "distribution":{"distribution":"ubuntu","version":"26.04"},"cgroupManager":"systemd","cgroupVersion":"v2",
            "conmon":{"path":"/usr/bin/conmon"},
            "networkBackend":"netavark",
            "networkBackendInfo":{"backend":"netavark","path":"/usr/lib/podman/netavark",
                "dns":{"path":"/usr/lib/podman/aardvark-dns"}},
            "pasta":{"executable":"/usr/bin/pasta"},
            "slirp4netns":{"executable":"/usr/bin/slirp4netns"},
            "ociRuntime":{"name":"crun","path":profile.crun}},
            "store":{"graphRoot":profile.graph_root,"runRoot":profile.run_root,"volumePath":profile.volume_dir,
                "graphDriverName":"overlay","transientStore":false},
            "version":{"Version":"5.7.0","Os":"linux","OsArch":"linux/amd64"}})
    }

    fn prepare_overlay(fixture: &crate::test_support::Fixture, profile: &mut PreparedProfile) {
        use std::os::unix::fs::DirBuilderExt;
        profile.graph_root = fixture.root.join("graph");
        profile.run_root = fixture.root.join("run");
        fixture.write("graph/overlay/.has-mount-program", b"false");
        for name in [
            "overlay-true",
            "native-diff()-true",
            "metacopy()-false",
            "volatile-true",
            "idmapped-lower-dir-false",
        ] {
            fixture.write(&format!("run/overlay/{name}"), b"");
        }
        for name in [
            "storage.lock",
            "userns.lock",
            "overlay-images/images.lock",
            "overlay-containers/containers.lock",
            "overlay-layers/layers.lock",
        ] {
            fixture.write(&format!("graph/{name}"), b"");
        }
        fixture.write("run/overlay-layers/mountpoints.lock", b"");
        for path in [
            "graph/overlay/l",
            "run/overlay-containers",
            "run/overlay-locks",
        ] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(fixture.root.join(path))
                .unwrap();
        }
    }

    fn prepare_startup(fixture: &crate::test_support::Fixture, profile: &mut PreparedProfile) {
        use std::os::unix::fs::DirBuilderExt;
        prepare_overlay(fixture, profile);
        profile.tmp_dir = fixture.root.join("runtime");
        fixture.write(
            "runtime/alive",
            std::fs::read("/proc/sys/kernel/random/boot_id").unwrap(),
        );
        fixture.write("graph/defaultNetworkBackend", b"netavark");
        fixture.write("graph/networks/netavark.lock", b"");
        fixture.write("runtime/events/events.log", b"");
        for path in [
            "runtime/exits",
            "runtime/persist",
            "run/networks/rootless-netns",
        ] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(fixture.root.join(path))
                .unwrap();
        }
    }

    #[test]
    fn metadata_argv_disables_startup_and_never_interpolates_native_paths() {
        let mut profile = profile();
        use std::os::unix::ffi::OsStringExt;
        let native = std::ffi::OsString::from_vec(b"/private/a ' ; bad\xff.db".to_vec());
        profile.database = native.clone().into();
        let command = profile.metadata_command();
        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/usr/bin/bwrap")
        );
        let args: Vec<_> = command.get_args().map(std::ffi::OsString::from).collect();
        assert_eq!(
            &args[..14],
            [
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-user",
                "--unshare-pid",
                "--unshare-net",
                "--die-with-parent",
                "--new-session",
                "--cap-drop",
                "ALL",
                "--disable-userns",
                "--"
            ]
            .map(std::ffi::OsString::from)
        );
        assert_eq!(args[14], profile.sqlite.as_os_str());
        assert_eq!(
            &args[15..19],
            ["-init", "/dev/null", "-readonly", "-json"].map(std::ffi::OsString::from)
        );
        assert_eq!(args[19], native);
        assert_eq!(args[20], DB_QUERY);
        assert_eq!(args.len(), 21);
        assert!(!DB_QUERY.contains("immutable") && !DB_QUERY.contains("PRAGMA"));
    }

    #[test]
    fn overlay_backing_filesystem_refuses_xfs_quota_setup() {
        use nix::sys::statfs::{FsType, fstatfs};
        assert!(check_overlay_backing_filesystem(FsType(nix::libc::XFS_SUPER_MAGIC)).is_err());
        assert!(check_overlay_backing_filesystem(FsType(nix::libc::EXT4_SUPER_MAGIC)).is_ok());
        let fixture = crate::test_support::Fixture::new();
        let mut profile = profile();
        prepare_overlay(&fixture, &mut profile);
        let overlay = profile.graph_root.join("overlay");
        let directory = required(&overlay, nix::unistd::geteuid().as_raw(), true).unwrap();
        let before = directory.metadata().unwrap();
        let filesystem = fstatfs(&directory).unwrap().filesystem_type();
        assert_eq!(
            profile.check_overlay().is_ok(),
            filesystem != FsType(nix::libc::XFS_SUPER_MAGIC),
        );
        let after = directory.metadata().unwrap();
        assert_eq!(
            (
                before.ino(),
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            ),
            (
                after.ino(),
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            ),
        );
        assert!(!overlay.join("backingFsBlockDev").exists());
    }

    #[test]
    #[ignore = "requires explicit LARAINIT_TEST_SQLITE and working Bubblewrap/user namespaces; metadata-only, not Podman certification"]
    fn real_metadata_sandbox_preserves_live_wal_and_refuses_missing_state() {
        let fixture = crate::test_support::Fixture::new();
        let mut profile = profile();
        profile.sqlite = std::env::var_os("LARAINIT_TEST_SQLITE")
            .expect("explicit metadata audit requires LARAINIT_TEST_SQLITE; no passing skip")
            .into();
        assert!(profile.sqlite.is_absolute());
        profile.database = fixture.root.join("metadata a ' ; 日本語.sql");
        fixture.write(".sqliterc", ".print forbidden-startup\n");
        let marker = fixture.root.join("prepared");
        let mut writer = Command::new("python3")
            .args(["-c", r#"
import json, pathlib, sqlite3, sys, time
connection = sqlite3.connect(sys.argv[1])
connection.execute('PRAGMA journal_mode=WAL')
connection.execute('PRAGMA wal_autocheckpoint=0')
connection.execute('CREATE TABLE DBConfig (ID, SchemaVersion, OS, StaticDir, TmpDir, GraphRoot, RunRoot, GraphDriver, VolumeDir)')
row = json.loads(sys.argv[3])[0]
connection.execute('INSERT INTO DBConfig VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)',
                   tuple(row[key] for key in ('ID', 'SchemaVersion', 'OS', 'StaticDir', 'TmpDir', 'GraphRoot', 'RunRoot', 'GraphDriver', 'VolumeDir')))
connection.commit()
pathlib.Path(sys.argv[2]).write_text('ready')
time.sleep(30)
"#])
            .arg(&profile.database).arg(&marker).arg(database_json(&profile).to_string())
            .spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() {
            if std::time::Instant::now() >= deadline || writer.try_wait().unwrap().is_some() {
                let _ = writer.kill();
                let _ = writer.wait();
                panic!("test-owned SQLite fixture was not prepared");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let snapshot = |directory: &Path| {
            let mut files: Vec<_> = std::fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.is_file())
                .map(|path| {
                    (
                        path.file_name().unwrap().to_owned(),
                        std::fs::read(&path).unwrap(),
                        std::fs::metadata(&path).unwrap().modified().unwrap(),
                    )
                })
                .collect();
            files.sort_by(|a, b| a.0.cmp(&b.0));
            files
        };
        let observations = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let original = profile.database.clone();
            let mut observations = Vec::new();
            for case in ["live", "missing-shm", "absent"] {
                let directory = fixture.root.join(case);
                std::fs::create_dir(&directory).unwrap();
                if case == "live" {
                    profile.database = original.clone();
                } else {
                    profile.database = directory.join("db.sql");
                    if case == "missing-shm" {
                        std::fs::copy(&original, &profile.database).unwrap();
                        std::fs::copy(
                            format!("{}-wal", original.display()),
                            directory.join("db.sql-wal"),
                        )
                        .unwrap();
                    }
                }
                let parent = profile.database.parent().unwrap();
                let before = snapshot(parent);
                let output = crate::runner::observe(
                    profile.metadata_command().env("HOME", &fixture.root),
                    None,
                );
                observations.push((case, before == snapshot(parent), output));
            }
            observations
        }));
        let _ = writer.kill();
        writer.wait().unwrap();
        for (case, unchanged, output) in observations.unwrap() {
            assert!(
                unchanged,
                "{case}: metadata read changed fixture bytes/mtime or created files"
            );
            let output = output.unwrap();
            assert_eq!(output.status.success(), case == "live", "{case}");
            if case == "live" {
                assert!(
                    output.stderr.is_empty(),
                    "metadata reader emitted startup errors"
                );
                profile.validate_database(&output.stdout).unwrap();
            }
        }
    }

    #[test]
    fn mocked_prepared_observation_orders_metadata_before_info_and_shares_capture() {
        for state in [
            "valid",
            "bad-metadata",
            "metadata-failure",
            "metadata-flood",
            "metadata-hang",
            "info-failure",
            "info-hang",
            "bad-info",
            "combined-overflow",
            "stale-boot",
            "cold-overlay",
            "cold-startup",
            "pending-layer-deletion",
            "pending-overlay-deletion",
            "incomplete-layer",
            "conflicting-storage-names",
            "ambiguous-storage-flags",
            "missing-pause",
        ] {
            let fixture = crate::test_support::Fixture::new();
            let bin = fixture.install_engine("podman");
            fixture.install_engine("sqlite3");
            let mut profile = profile();
            profile.sqlite = bin.join("sqlite3");
            prepare_startup(&fixture, &mut profile);
            if state == "cold-overlay" {
                std::fs::remove_file(profile.run_root.join("overlay/volatile-true")).unwrap();
            }
            if state == "cold-startup" {
                std::fs::remove_file(profile.graph_root.join("defaultNetworkBackend")).unwrap();
            }
            if state == "pending-layer-deletion" {
                fixture.write("graph/overlay-layers/tmp/temporary-1", b"preserve");
            }
            if state == "pending-overlay-deletion" {
                fixture.write("graph/overlay/tempdirs/temporary-1", b"preserve");
            }
            if state == "incomplete-layer" {
                fixture.write(
                    "graph/overlay-layers/volatile-layers.json",
                    br#"[{"id":"1","flags":{"incomplete":true}}]"#,
                );
            }
            if state == "conflicting-storage-names" {
                fixture.write(
                    "graph/overlay-containers/containers.json",
                    br#"[{"id":"1","names":["shared"]}]"#,
                );
                fixture.write(
                    "graph/overlay-containers/volatile-containers.json",
                    br#"[{"id":"2","names":["shared"]}]"#,
                );
            }
            if state == "ambiguous-storage-flags" {
                fixture.write(
                    "graph/overlay-layers/layers.json",
                    br#"[{"id":"1","flags":{"incomplete":true},"flags":{}}]"#,
                );
            }
            if state == "stale-boot" {
                fixture.write("runtime/alive", b"stale");
            }
            let mut metadata_json = database_json(&profile).to_string();
            let mut info_json = info_json(&profile).to_string();
            if state == "bad-metadata" {
                metadata_json = "[]".into();
            }
            if state == "metadata-failure" {
                fixture.write("sqlite3/metadata-mode", "exit=1");
            }
            if state == "metadata-flood" {
                fixture.write("sqlite3/metadata-mode", "flood");
            }
            if state == "metadata-hang" {
                fixture.write("sqlite3/metadata-mode", "hang");
            }
            if state == "info-failure" {
                fixture.write("podman/info-mode", "exit=125");
            }
            if state == "info-hang" {
                fixture.write("podman/info-mode", "hang");
            }
            if state == "bad-info" {
                info_json = "{}".into();
            }
            if state == "combined-overflow" {
                metadata_json.push_str(&" ".repeat(45 * 1024));
                info_json.push_str(&" ".repeat(25 * 1024));
            }
            fixture.write("sqlite3/metadata-stdout", metadata_json);
            fixture.write("podman/info-stdout", info_json);
            // This seam tests ordering/budgets; mock call recording cannot write in the sandbox.
            let mut metadata = Command::new(&profile.sqlite);
            metadata
                .args(["-init", "/dev/null", "-readonly", "-json"])
                .arg(&profile.database)
                .arg(DB_QUERY)
                .env("MOCK_ROOT", &fixture.root);
            let mut info = Command::new(bin.join("podman"));
            info.args(["info", "--format", "json"])
                .env("MOCK_ROOT", &fixture.root);
            let start = std::time::Instant::now();
            let result = profile.observe(
                &mut metadata,
                &mut info,
                None,
                &mut ObservationBudget::new(),
                || {
                    profile.check_startup()?;
                    if state == "missing-pause" {
                        profile.check_namespace_view()?;
                    }
                    Ok(())
                },
            );
            if state == "valid" {
                assert_eq!(result.unwrap(), "5.7.0");
            } else {
                assert!(result.is_err(), "{state}");
            }
            assert!(
                start.elapsed() < std::time::Duration::from_secs(8),
                "candidate exceeded total bound: {state}"
            );
            assert_eq!(fixture.engine_calls("sqlite3").len(), 1);
            assert_eq!(
                fixture.engine_calls("podman").len(),
                usize::from(matches!(
                    state,
                    "valid" | "combined-overflow" | "info-failure" | "info-hang" | "bad-info"
                ))
            );
        }
    }

    #[test]
    fn stored_schema_and_all_effective_paths_must_agree() {
        let profile = profile();
        let valid = database_json(&profile);
        assert!(
            profile
                .validate_database(&serde_json::to_vec(&valid).unwrap())
                .is_ok()
        );
        for (key, value) in [
            ("ID", json!(0)),
            ("SchemaVersion", json!(0)),
            ("SchemaVersion", json!(2)),
            ("OS", json!("windows")),
            ("GraphDriver", json!("vfs")),
            ("StaticDir", json!("/other")),
            ("TmpDir", json!("/other")),
            ("GraphRoot", json!("/other")),
            ("RunRoot", json!("/other")),
            ("VolumeDir", json!("/other")),
        ] {
            let mut invalid = valid.clone();
            invalid[0][key] = value;
            assert!(
                profile
                    .validate_database(&serde_json::to_vec(&invalid).unwrap())
                    .is_err(),
                "{key}"
            );
        }
        for invalid in [
            json!([]),
            json!([valid[0], valid[0]]),
            json!({}),
            json!([{}]),
        ] {
            assert!(
                profile
                    .validate_database(&serde_json::to_vec(&invalid).unwrap())
                    .is_err()
            );
        }
        let duplicate = valid.to_string().replace(
            "\"SchemaVersion\":1",
            "\"SchemaVersion\":1,\"SchemaVersion\":1",
        );
        assert!(profile.validate_database(duplicate.as_bytes()).is_err());
    }

    #[test]
    fn every_required_metadata_and_info_field_refuses_missing_null_and_wrong_types() {
        let profile = profile();
        let database = database_json(&profile);
        for key in database[0].as_object().unwrap().keys() {
            for value in [None, Some(Value::Null), Some(json!({})), Some(json!([]))] {
                let mut invalid = database.clone();
                if let Some(value) = value {
                    invalid[0][key] = value;
                } else {
                    invalid[0].as_object_mut().unwrap().remove(key);
                }
                assert!(
                    profile
                        .validate_database(&serde_json::to_vec(&invalid).unwrap())
                        .is_err(),
                    "metadata {key}"
                );
            }
        }
        let info = info_json(&profile);
        for pointer in [
            "/host/arch",
            "/host/os",
            "/host/serviceIsRemote",
            "/host/databaseBackend",
            "/host/security/rootless",
            "/host/security/selinuxEnabled",
            "/host/security/apparmorEnabled",
            "/host/security/seccompEnabled",
            "/host/security/seccompProfilePath",
            "/host/distribution/distribution",
            "/host/distribution/version",
            "/host/cgroupManager",
            "/host/cgroupVersion",
            "/host/conmon/path",
            "/host/networkBackend",
            "/host/networkBackendInfo/backend",
            "/host/networkBackendInfo/path",
            "/host/networkBackendInfo/dns/path",
            "/host/pasta/executable",
            "/host/slirp4netns/executable",
            "/host/ociRuntime/name",
            "/host/ociRuntime/path",
            "/store/graphRoot",
            "/store/runRoot",
            "/store/volumePath",
            "/store/graphDriverName",
            "/store/transientStore",
            "/version/Version",
            "/version/Os",
            "/version/OsArch",
        ] {
            for value in [None, Some(Value::Null), Some(json!({})), Some(json!([]))] {
                let mut invalid = info.clone();
                if let Some(value) = value {
                    *invalid.pointer_mut(pointer).unwrap() = value;
                } else {
                    let (parent, key) = pointer.rsplit_once('/').unwrap();
                    invalid
                        .pointer_mut(parent)
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .remove(key);
                }
                assert!(
                    profile
                        .validate_info(&serde_json::to_vec(&invalid).unwrap())
                        .is_err(),
                    "info {pointer}"
                );
            }
        }
    }

    #[test]
    fn info_requires_explicit_local_rootless_platform_runtime_and_store_evidence() {
        let profile = profile();
        let valid = info_json(&profile);
        assert!(
            profile
                .validate_info(&serde_json::to_vec(&valid).unwrap())
                .is_ok()
        );
        for (pointer, value) in [
            ("/host/serviceIsRemote", json!(true)),
            ("/host/security/rootless", json!(false)),
            ("/host/security/selinuxEnabled", json!(true)),
            ("/host/security/apparmorEnabled", json!(true)),
            ("/host/security/seccompEnabled", json!(false)),
            (
                "/host/security/seccompProfilePath",
                json!("/etc/containers/seccomp.json"),
            ),
            ("/host/security/seccompProfilePath", json!("")),
            ("/host/security/seccompProfilePath", json!("unconfined")),
            ("/host/distribution/distribution", json!("debian")),
            ("/host/distribution/version", json!("24.04")),
            ("/host/cgroupManager", json!("cgroupfs")),
            ("/host/cgroupVersion", json!("v1")),
            ("/host/conmon/path", json!("/other/conmon")),
            ("/host/networkBackend", json!("cni")),
            ("/host/networkBackendInfo/backend", json!("cni")),
            ("/host/networkBackendInfo/path", json!("/other/netavark")),
            (
                "/host/networkBackendInfo/dns/path",
                json!("/other/aardvark-dns"),
            ),
            ("/host/pasta/executable", json!("/other/pasta")),
            ("/host/slirp4netns/executable", json!("/other/slirp4netns")),
            ("/host/arch", json!("arm64")),
            ("/host/os", json!("windows")),
            ("/host/databaseBackend", json!("boltdb")),
            ("/host/ociRuntime/name", json!("runc")),
            ("/host/ociRuntime/path", json!("/other")),
            ("/store/graphRoot", json!("/other")),
            ("/store/runRoot", json!("/other")),
            ("/store/volumePath", json!("/other")),
            ("/store/graphDriverName", json!("vfs")),
            ("/store/transientStore", json!(true)),
            ("/version/Version", json!("5.8.2")),
            ("/version/OsArch", json!("linux/arm64")),
            ("/version/Os", json!("windows")),
        ] {
            let mut invalid = valid.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(
                profile
                    .validate_info(&serde_json::to_vec(&invalid).unwrap())
                    .is_err(),
                "{pointer}"
            );
        }
        let mut missing = valid.clone();
        missing["host"]
            .as_object_mut()
            .unwrap()
            .remove("serviceIsRemote");
        assert!(
            profile
                .validate_info(&serde_json::to_vec(&missing).unwrap())
                .is_err()
        );
        let duplicate = valid.to_string().replace(
            "\"serviceIsRemote\":false",
            "\"serviceIsRemote\":false,\"serviceIsRemote\":false",
        );
        assert!(profile.validate_info(duplicate.as_bytes()).is_err());
    }

    #[test]
    fn profile_boundaries_reject_overrides_unsafe_paths_stale_boot_and_host_namespaces() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = crate::test_support::Fixture::new();
        let uid = nix::unistd::geteuid().as_raw();
        fixture.write("safe", b"metadata");
        let safe = fixture.root.join("safe");
        assert_eq!(read_existing(&safe, uid, 8).unwrap(), b"metadata");
        assert!(read_existing(&safe, uid, 7).is_err());
        symlink(&safe, fixture.root.join("linked")).unwrap();
        assert!(read_existing(&fixture.root.join("linked"), uid, 8).is_err());
        symlink(&fixture.root, fixture.root.join("linked-parent")).unwrap();
        assert!(read_existing(&fixture.root.join("linked-parent/safe"), uid, 8).is_err());
        std::fs::set_permissions(&safe, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read_existing(&safe, uid, 8).is_err());
        assert!(absent(&fixture.root.join("absent"), uid).is_ok());
        assert!(absent(&fixture.root.join("linked"), uid).is_err());
        let boot = b"12345678-1234-1234-1234-123456789abc\n";
        assert!(current_boot(boot, boot));
        for cached in [
            b"".as_slice(),
            b"present",
            b"87654321-1234-1234-1234-123456789abc\n",
        ] {
            assert!(!current_boot(boot, cached));
        }
        assert!(!current_boot(b"not-a-boot-id\n", b"not-a-boot-id\n"));
        assert!(mapped_namespace(b"0 1001 1\n1 165536 65536\n", 1001));
        for map in [
            b"0 0 4294967295\n".as_slice(),
            b"0 1002 1\n1 165536 65536\n",
            b"0 1001 1\n1 165536 1\n",
            b"invalid",
        ] {
            assert!(!mapped_namespace(map, 1001));
        }
        for key in [
            "CONTAINERS_CONF",
            "CONTAINERS_CONF_OVERRIDE",
            "CONTAINERS_STORAGE_CONF",
            "CONTAINERS_HELPER_BINARY_DIR",
            "CONTAINER_HOST",
            "CONTAINER_CONNECTION",
            "_CONTAINERS_USERNS_CONFIGURED",
            "STORAGE_DRIVER",
            "STORAGE_OPTS",
            "LD_PRELOAD",
            "PODMAN_CONNECTIONS_CONF",
            "SQLITE_TMPDIR",
            "BWRAP_ARGS",
            "DPKG_ROOT",
            "DPKG_ADMINDIR",
            "DPKG_FORCE",
            "DPKG_DEBUG",
            "container",
        ] {
            assert!(!supported_environment(std::ffi::OsStr::new(key)), "{key}");
        }
        for key in [
            "HOME",
            "USER",
            "PATH",
            "TMPDIR",
            "XDG_CACHE_HOME",
            "LARAINIT_TEST_PODMAN_IMAGE",
        ] {
            assert!(supported_environment(std::ffi::OsStr::new(key)), "{key}");
        }
    }

    #[test]
    fn packaged_helper_directory_accepts_exact_link_but_not_conmon_override() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = crate::test_support::Fixture::new();
        let uid = nix::unistd::geteuid().as_raw();
        fixture.write("usr/bin/catatonit", b"audited target");
        fixture.write(
            "usr/libexec/podman/rootlessport",
            b"ordinary packaged helper",
        );
        let directory = fixture.root.join("usr/libexec/podman");
        let link = directory.join("catatonit");
        let value = Path::new("../../bin/catatonit");
        symlink(value, &link).unwrap();
        assert!(required(&directory, uid, true).is_ok());
        assert!(absent(&directory.join("conmon"), uid).is_ok());
        assert!(packaged_link(&link, value, uid).is_ok());
        assert!(packaged_link(&link, Path::new("/other/catatonit"), uid).is_err());
        assert!(packaged_link(&link, value, uid.wrapping_add(1)).is_err());
        fixture.write("usr/libexec/podman/conmon", b"override");
        assert!(absent(&directory.join("conmon"), uid).is_err());
        assert!(packaged_link(&directory.join("rootlessport"), value, uid).is_err());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(packaged_link(&link, value, uid).is_err());
    }

    #[test]
    fn helper_search_refuses_nonexecutables_and_links_without_running_or_removing_them() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        for name in [
            "netavark",
            "aardvark-dns",
            "pasta",
            "slirp4netns",
            "dpkg-query",
        ] {
            let fixture = crate::test_support::Fixture::new();
            let uid = nix::unistd::geteuid().as_raw();
            let directory = fixture.root.join("helpers");
            let names = [name];
            assert!(refuse_helpers(&directory, &names, uid).is_ok());
            assert!(!directory.exists(), "check created the search directory");
            fixture.write(
                "helpers/ordinary-other-helper",
                b"preserve unrelated helper",
            );
            assert!(refuse_helpers(&directory, &names, uid).is_ok());
            fixture.write(&format!("helpers/{name}"), b"unaudited helper");
            let path = directory.join(name);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let before = std::fs::metadata(&path).unwrap();
            assert!(refuse_helpers(&directory, &names, uid).is_err());
            let after = std::fs::metadata(&path).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"unaudited helper");
            assert_eq!(
                (
                    before.ino(),
                    before.mtime(),
                    before.mtime_nsec(),
                    before.ctime(),
                    before.ctime_nsec()
                ),
                (
                    after.ino(),
                    after.mtime(),
                    after.mtime_nsec(),
                    after.ctime(),
                    after.ctime_nsec()
                )
            );
            std::fs::remove_file(&path).unwrap();
            symlink("ordinary-other-helper", &path).unwrap();
            assert!(refuse_helpers(&directory, &names, uid).is_err());
            assert_eq!(
                std::fs::read_link(&path).unwrap(),
                Path::new("ordinary-other-helper")
            );
            assert_eq!(
                std::fs::read(directory.join("ordinary-other-helper")).unwrap(),
                b"preserve unrelated helper"
            );
        }
    }

    #[test]
    fn overlay_readiness_never_creates_markers_or_recovers_staging() {
        use std::os::unix::fs::DirBuilderExt;
        for state in ["warm", "missing", "fuse", "contradictory", "staging"] {
            let fixture = crate::test_support::Fixture::new();
            let mut profile = profile();
            prepare_overlay(&fixture, &mut profile);
            let flag = profile.graph_root.join("overlay/.has-mount-program");
            match state {
                "missing" => std::fs::remove_file(&flag).unwrap(),
                "fuse" => fixture.write("graph/overlay/.has-mount-program", b"true"),
                "contradictory" => fixture.write("run/overlay/volatile-false", b""),
                "staging" => fixture.write("graph/overlay/staging/unrelated", b"preserve"),
                _ => {}
            }
            let before = std::fs::read(&flag).ok();
            assert_eq!(profile.check_overlay().is_ok(), state == "warm", "{state}");
            assert_eq!(std::fs::read(&flag).ok(), before);
            if state == "staging" {
                assert_eq!(
                    std::fs::read(profile.graph_root.join("overlay/staging/unrelated")).unwrap(),
                    b"preserve"
                );
            }
        }
        for name in ["overlay/staging", "overlay/tempdirs", "overlay-layers/tmp"] {
            let fixture = crate::test_support::Fixture::new();
            let mut profile = profile();
            prepare_overlay(&fixture, &mut profile);
            let path = profile.graph_root.join(name);
            assert!(profile.check_overlay().is_ok());
            assert!(!path.exists(), "readiness created {name}");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            assert!(profile.check_overlay().is_ok(), "empty {name}");
            for entry in ["temporary-1", "lock-1", "unrelated"] {
                let sentinel = path.join(entry);
                std::fs::write(&sentinel, b"preserve").unwrap();
                assert!(profile.check_overlay().is_err(), "{name}/{entry}");
                assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");
                std::fs::remove_file(sentinel).unwrap();
            }
            std::fs::remove_dir(&path).unwrap();
            std::os::unix::fs::symlink(&fixture.root, &path).unwrap();
            assert!(profile.check_overlay().is_err(), "linked {name}");
        }
    }

    #[test]
    fn startup_readiness_never_creates_missing_paths_or_migrates_network_backend() {
        for (name, directory) in [
            ("runtime/exits", true),
            ("runtime/persist", true),
            ("graph/defaultNetworkBackend", false),
            ("graph/networks/netavark.lock", false),
            ("run/networks/rootless-netns", true),
            ("graph/overlay-layers/layers.lock", false),
            ("run/overlay-layers/mountpoints.lock", false),
        ] {
            let fixture = crate::test_support::Fixture::new();
            let mut profile = profile();
            prepare_startup(&fixture, &mut profile);
            assert!(profile.check_startup().is_ok());
            let path = fixture.root.join(name);
            if directory {
                std::fs::remove_dir(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
            assert!(profile.check_startup().is_err(), "{name}");
            assert!(!path.exists(), "startup check repaired {name}");
        }
        let fixture = crate::test_support::Fixture::new();
        let mut profile = profile();
        prepare_startup(&fixture, &mut profile);
        for backend in [b"cni".as_slice(), b"", b"netavark\n", b"unknown"] {
            fixture.write("graph/defaultNetworkBackend", backend);
            assert!(profile.check_startup().is_err());
            assert_eq!(
                std::fs::read(profile.graph_root.join("defaultNetworkBackend")).unwrap(),
                backend
            );
        }
    }

    #[test]
    fn storage_recovery_metadata_is_bounded_read_only_and_checks_volatile_records() {
        for name in [
            "overlay-layers/layers.json",
            "overlay-layers/volatile-layers.json",
            "overlay-images/images.json",
            "overlay-containers/containers.json",
            "overlay-containers/volatile-containers.json",
        ] {
            let fixture = crate::test_support::Fixture::new();
            let mut profile = profile();
            prepare_startup(&fixture, &mut profile);
            let path = profile.graph_root.join(name);
            assert!(profile.check_storage_metadata().is_ok());
            assert!(!path.exists(), "check created {name}");
            let snapshot = || {
                let bytes = std::fs::read(&path).unwrap();
                let metadata = std::fs::metadata(&path).unwrap();
                (
                    bytes,
                    metadata.ino(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            };
            for (bytes, valid) in [
                (b"".as_slice(), true),
                (b"null", true),
                (b"[]", true),
                (br#"[{"id":"1"},{"id":"2","names":["unique"],"flags":{"incomplete":false},"metadata":"opaque"}]"#, true),
                (br#"[{"id":"1","names":["caf\u00e9"],"flags":{"other":{"incomplete":true}},"big-data-names":["manifest"]}]"#, true),
                (b"{}", false),
                (b"[null]", false),
                (b"[{}]", false),
                (br#"[{"id":""}]"#, false),
                (br#"[{"id":null}]"#, false),
                (br#"[{"id":"1"},{"id":"1"}]"#, false),
                (br#"[{"id":"1","id":"2"}]"#, false),
                (br#"[{"id":"1","ID":"2"}]"#, false),
                (br#"[{"id":"1","names":null}]"#, false),
                (br#"[{"id":"1","names":[1]}]"#, false),
                (br#"[{"id":"1","names":[""]}]"#, false),
                (br#"[{"id":"1","names":["a","a"]}]"#, false),
                (br#"[{"id":"1","names":["a"]},{"id":"2","names":["a"]}]"#, false),
                (br#"[{"id":"1","names":[],"Names":["a"]}]"#, false),
                (br#"[{"id":"1","names":[],"names":["a"]}]"#, false),
                (br#"[{"id":"1","flags":{"incomplete":true}}]"#, false),
                (br#"[{"id":"1","flags":{"incomplete":"false"}}]"#, false),
                (br#"[{"id":"1","flags":null}]"#, false),
                (br#"[{"id":"1","Flags":{"incomplete":true}}]"#, false),
                (br#"[{"id":"1","flags":{"incomplete":true},"flags":{}}]"#, false),
                (br#"[{"id":"1","flag\u017f":{"incomplete":true}}]"#, false),
                (br#"[{"id":"1"}] trailing"#, false),
            ] {
                fixture.write(&format!("graph/{name}"), bytes);
                let before = snapshot();
                assert_eq!(profile.check_storage_metadata().is_ok(), valid, "{name}: {}", String::from_utf8_lossy(bytes));
                assert_eq!(snapshot(), before, "check changed {name}");
            }
            fixture.write(&format!("graph/{name}"), " ".repeat(1024 * 1024 + 1));
            let before = snapshot();
            assert!(profile.check_storage_metadata().is_err());
            assert_eq!(snapshot(), before);
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&fixture.root, &path).unwrap();
            assert!(profile.check_storage_metadata().is_err(), "linked {name}");
        }
        for (permanent, volatile) in [
            (
                "overlay-layers/layers.json",
                "overlay-layers/volatile-layers.json",
            ),
            (
                "overlay-containers/containers.json",
                "overlay-containers/volatile-containers.json",
            ),
        ] {
            let fixture = crate::test_support::Fixture::new();
            let mut profile = profile();
            prepare_startup(&fixture, &mut profile);
            fixture.write(
                &format!("graph/{permanent}"),
                br#"[{"id":"1","names":["shared"]}]"#,
            );
            for bytes in [
                br#"[{"id":"2","names":["shared"]}]"#.as_slice(),
                br#"[{"id":"1","names":["different"]}]"#,
            ] {
                fixture.write(&format!("graph/{volatile}"), bytes);
                assert!(profile.check_storage_metadata().is_err());
                assert_eq!(
                    std::fs::read(profile.graph_root.join(volatile)).unwrap(),
                    bytes
                );
            }
        }
        let fixture = crate::test_support::Fixture::new();
        let mut profile = profile();
        prepare_startup(&fixture, &mut profile);
        for name in [
            "overlay-layers/layers.json",
            "overlay-images/images.json",
            "overlay-containers/containers.json",
        ] {
            fixture.write(
                &format!("graph/{name}"),
                br#"[{"id":"1","names":["same-name-different-store"]}]"#,
            );
        }
        assert!(profile.check_storage_metadata().is_ok());
    }

    #[test]
    fn journal_detection_requires_a_readable_directory_not_a_file_or_link() {
        use std::os::unix::fs::symlink;
        let fixture = crate::test_support::Fixture::new();
        let root = fixture.root.join("journal");
        assert!(!readable_journal(&root).unwrap());
        fixture.write("journal/file", b"not a journal directory");
        assert!(!readable_journal(&root).unwrap());
        symlink(&fixture.root, root.join("linked-directory")).unwrap();
        assert!(!readable_journal(&root).unwrap());
        fixture.write("journal/machine/file", b"journal entry");
        assert!(readable_journal(&root).unwrap());
    }
}
