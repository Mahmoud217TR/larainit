use crate::{
    cli::{BuildOptions, Overrides},
    config, engine,
    error::{Error, Kind},
    image, runner,
};
use nix::unistd::{AccessFlags, access};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

struct Request {
    context: PathBuf,
    file: PathBuf,
    tag: String,
}

fn invalid(cause: impl std::fmt::Display) -> Error {
    let mut error = Error::operational(
        "validate build input",
        "explicit local source",
        cause,
        "Example: larainit build --context /path/to/source --tag larainit:latest",
    );
    error.kind = Kind::Usage;
    error
}

fn local_path(path: &Path) -> Result<(), Error> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty()
        || bytes == b"-"
        || bytes.contains(&0)
        || path.components().any(|part| part == Component::ParentDir)
        || path.to_str().is_some_and(|value| {
            value.contains("://")
                || (!path.is_absolute()
                    && value
                        .split_once(':')
                        .is_some_and(|(host, _)| host.contains('@')))
        })
    {
        return Err(invalid(
            "No URL/Git/stdin context, parent traversal or NUL path is accepted",
        ));
    }
    Ok(())
}

fn component(value: &str) -> bool {
    let bytes = value.as_bytes();
    let alnum = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        while index < bytes.len() && alnum(bytes[index]) {
            index += 1;
        }
        if start == index {
            return false;
        }
        if index == bytes.len() {
            return true;
        }
        match bytes[index] {
            b'.' => index += 1,
            b'_' => {
                index += 1;
                if bytes.get(index) == Some(&b'_') {
                    index += 1;
                }
            }
            b'-' => {
                while bytes.get(index) == Some(&b'-') {
                    index += 1;
                }
            }
            _ => return false,
        }
    }
    false
}

fn named_tag(value: &str) -> bool {
    if value.is_empty()
        || value.contains('@')
        || image::valid_id(value)
        || image::valid_container_id(value)
    {
        return false;
    }
    let (repository, tag) = match value.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => (repo, Some(tag)),
        _ => (value, None),
    };
    if tag.is_some_and(|tag| {
        tag.is_empty()
            || tag.len() > 128
            || !tag.as_bytes()[0].is_ascii_alphanumeric() && tag.as_bytes()[0] != b'_'
            || !tag
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    }) {
        return false;
    }
    let mut parts = repository.split('/').peekable();
    let Some(first) = parts.next() else {
        return false;
    };
    let has_domain = parts.peek().is_some()
        && (first.contains('.')
            || first.contains(':')
            || first == "localhost"
            || first.bytes().any(|byte| byte.is_ascii_uppercase()));
    let remote = if has_domain {
        repository.split_once('/').unwrap().1
    } else {
        repository
    };
    // Docker's normalized remote name includes library/ for unqualified official images.
    let library_prefix =
        !remote.contains('/') && (!has_domain || matches!(first, "docker.io" | "index.docker.io"));
    if remote.len() + if library_prefix { 8 } else { 0 } > 255 {
        return false;
    }
    if has_domain {
        if let Some(rest) = first.strip_prefix('[') {
            let Some((address, suffix)) = rest.split_once(']') else {
                return false;
            };
            if address.parse::<std::net::Ipv6Addr>().is_err()
                || (!suffix.is_empty()
                    && !suffix.strip_prefix(':').is_some_and(|port| {
                        !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
                    }))
            {
                return false;
            }
            return parts.all(component);
        }
        let (host, port) = first
            .rsplit_once(':')
            .map_or((first, None), |(host, port)| (host, Some(port)));
        if port
            .is_some_and(|port| port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()))
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.as_bytes()[0].is_ascii_alphanumeric()
                    && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return false;
        }
    } else if !component(first) {
        return false;
    }
    parts.all(component)
}

impl Request {
    fn validate(options: &BuildOptions) -> Result<Self, Error> {
        local_path(&options.context)?;
        let tag = options.tag.as_deref().unwrap_or("larainit:latest");
        if !named_tag(tag) {
            return Err(invalid(
                "build destination must be a writable named image tag, not a digest/option",
            ));
        }
        let resource = |path: &Path, cause: String| {
            Error::operational(
                "validate local build resource",
                format!("{path:?}"),
                cause,
                "Supply a readable local context directory and regular in-context Dockerfile; no source is fetched or repaired",
            )
        };
        let context = options
            .context
            .canonicalize()
            .map_err(|cause| resource(&options.context, cause.to_string()))?;
        if !context.is_dir() || access(&context, AccessFlags::R_OK | AccessFlags::X_OK).is_err() {
            return Err(resource(
                &context,
                "context is not a readable/searchable directory".into(),
            ));
        }
        let requested = options.file.as_deref().unwrap_or(Path::new("Dockerfile"));
        local_path(requested)?;
        let file = if requested.is_absolute() {
            requested.to_owned()
        } else {
            context.join(requested)
        };
        let file = file
            .canonicalize()
            .map_err(|cause| resource(&file, cause.to_string()))?;
        if !file.starts_with(&context) {
            return Err(invalid("build file resolves outside the context"));
        }
        if !file.is_file() || access(&file, AccessFlags::R_OK).is_err() {
            return Err(resource(
                &file,
                "build file is not a readable regular file".into(),
            ));
        }
        Ok(Self {
            context,
            file,
            tag: tag.to_owned(),
        })
    }
}

pub fn run(options: &BuildOptions, overrides: &Overrides) -> Result<i32, Error> {
    let request = Request::validate(options)?;
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(invalid("build execution currently requires Linux x86_64"));
    }
    let settings = config::resolve(overrides)?;
    let signals = runner::Signals::register()?;
    let result = (|| {
        let selected = engine::assess_preference(settings.engine, Some(&signals))?;
        let temporary = std::env::temp_dir()
            .canonicalize()
            .map_err(|cause| invalid(cause.to_string()))?;
        if temporary.starts_with(&request.context) {
            return Err(invalid(
                "private IID scratch must be outside the build context; use an external temporary directory",
            ));
        }
        let scratch = runner::Scratch::new()?;
        let iidfile = scratch.iidfile();
        let scratch_root = iidfile
            .parent()
            .ok_or_else(|| invalid("missing private scratch parent"))?
            .canonicalize()
            .map_err(|cause| invalid(cause.to_string()))?;
        if scratch_root.starts_with(&request.context) {
            return Err(invalid(
                "private IID scratch must be outside the build context; use an external temporary directory",
            ));
        }
        let mut command = engine::command(&selected.executable);
        command
            .arg("build")
            .arg("--iidfile")
            .arg(&iidfile)
            .arg("--file")
            .arg(&request.file)
            .arg("--tag")
            .arg(&request.tag)
            .arg("--")
            .arg(&request.context);
        let status = runner::stream_build(&mut command, &signals)?;
        if status != 0 {
            return Ok(status);
        }
        if let Some(signal) = signals.signal() {
            return Ok(128 + signal as i32);
        }
        let verify_error = |cause: String| {
            Error::operational(
                "verify explicit build",
                format!("{} selected store, tag {}", selected.kind, request.tag),
                format!("build completed; verification failed: {cause}"),
                "Check the builder output/store and tag independently; artifacts may remain. No repair, rollback or fallback was attempted",
            )
        };
        let (iid, iid_handle) = scratch
            .image_id(selected.kind)
            .map_err(|cause| verify_error(cause.to_string()))?;
        let iid_snapshot = iid_handle
            .metadata()
            .map_err(|cause| verify_error(cause.to_string()))?;
        let immutable = image::inspect(&selected, &iid, Some(&signals))
            .map_err(|cause| verify_error(cause.to_string()))?;
        let tagged = image::inspect(&selected, &request.tag, Some(&signals))
            .map_err(|cause| verify_error(cause.to_string()))?;
        if immutable.id != iid || tagged.id != iid {
            return Err(verify_error(
                "own IID, immutable result and target tag do not agree".into(),
            ));
        }
        let (current_iid, current_handle) = scratch
            .image_id(selected.kind)
            .map_err(|cause| verify_error(cause.to_string()))?;
        let current = current_handle
            .metadata()
            .map_err(|cause| verify_error(cause.to_string()))?;
        use std::os::unix::fs::MetadataExt;
        // Keep the original descriptor alive through this comparison, preventing inode reuse.
        if current_iid != iid
            || current.dev() != iid_snapshot.dev()
            || current.ino() != iid_snapshot.ino()
            || current.len() != iid_snapshot.len()
            || current.mtime() != iid_snapshot.mtime()
            || current.mtime_nsec() != iid_snapshot.mtime_nsec()
            || current.ctime() != iid_snapshot.ctime()
            || current.ctime_nsec() != iid_snapshot.ctime_nsec()
        {
            return Err(verify_error("IID changed during build verification".into()));
        }
        if let Some(signal) = signals.signal() {
            return Ok(128 + signal as i32);
        }
        writeln!(
            std::io::stdout().lock(),
            "verified build: {} {} {}\nRun: larainit --engine {} --image {:?} php -v",
            selected.kind,
            request.tag,
            iid,
            selected.kind,
            request.tag
        )
        .map_err(|cause| verify_error(cause.to_string()))?;
        Ok(0)
    })();
    if result.is_err() {
        if let Some(signal) = signals.signal() {
            return Ok(128 + signal as i32);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_grammar_covers_named_repositories_not_digests_or_options() {
        for value in [
            "larainit",
            "larainit:latest",
            "repo/a__b:TAG_1",
            "repo/a--b",
            "localhost:5000/a/b:v1",
            "registry.example/a.b",
            "REGISTRY/repo:DEV",
            "UPPER/repo",
            "[::1]:5000/repo:dev",
        ] {
            assert!(named_tag(value), "{value}");
        }
        for value in [
            "",
            "-tag",
            "registry/UPPER/repo",
            "repo@sha256:abc",
            "repo:",
            "repo:bad tag",
            "repo/.bad",
            "repo/a___b",
            "repo//name",
            "repo/a..b",
            "host:/repo",
            "[bad]/repo",
            "repo:💥",
        ] {
            assert!(!named_tag(value), "{value}");
        }
        assert!(!named_tag(&format!("sha256:{}", "a".repeat(64))));
        assert!(!named_tag(&"a".repeat(64)));
        assert!(named_tag(&format!("{}:tag", "a".repeat(64))));
        assert!(named_tag(&format!("repo:{}", "a".repeat(128))));
        assert!(!named_tag(&format!("repo:{}", "a".repeat(129))));
        assert!(local_path(Path::new("user@example.com:repo")).is_err());
    }

    #[test]
    fn destination_length_counts_normalized_remote_name_not_registry_host() {
        assert!(named_tag(&format!(
            "registry.example/{}:tag",
            "b".repeat(255)
        )));
        assert!(!named_tag(&format!(
            "registry.example/{}:tag",
            "b".repeat(256)
        )));
        assert!(named_tag(&format!("namespace/{}:tag", "b".repeat(245))));
        assert!(!named_tag(&format!("namespace/{}:tag", "b".repeat(246))));
        for host in ["", "docker.io/", "index.docker.io/"] {
            assert!(named_tag(&format!("{host}{}:tag", "b".repeat(247))));
            assert!(
                !named_tag(&format!("{host}{}:tag", "b".repeat(248))),
                "default library/ prefix counts toward the 255-byte remote name"
            );
        }
        assert!(named_tag(&format!("REGISTRY/{}:tag", "b".repeat(255))));
        assert!(named_tag(&format!("[::1]:5000/{}:tag", "b".repeat(255))));
    }
}
