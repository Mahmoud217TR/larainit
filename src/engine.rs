use crate::{
    error::Error,
    runner::{self, Observation, Signals},
};
use nix::unistd::{AccessFlags, access};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Docker {
    pub executable: PathBuf,
    pub version: String,
}

pub fn discover(path: Option<&std::ffi::OsStr>) -> Result<PathBuf, Error> {
    let mut denied = None;
    for directory in path.into_iter().flat_map(std::env::split_paths) {
        let candidate = directory.join("docker");
        match std::fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => {
                if access(&candidate, AccessFlags::X_OK).is_ok() {
                    return std::path::absolute(&candidate).map_err(|error| {
                        Error::operational(
                            "locate Docker",
                            candidate.display().to_string(),
                            error,
                            "Check the invocation directory and PATH",
                        )
                    });
                }
                denied = Some(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                denied = Some(candidate)
            }
            _ => {}
        }
    }
    if let Some(path) = denied {
        Err(Error::operational(
            "locate Docker",
            path.display().to_string(),
            "executable access denied",
            "Fix Docker CLI/PATH permissions without elevating Larainit",
        ))
    } else {
        Err(Error::operational(
            "locate Docker",
            "PATH",
            "Docker executable absent",
            "Install/configure Docker independently and make docker available on PATH; no engine fallback is used",
        ))
    }
}

pub fn command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env("DOCKER_CLI_HOOKS", "false")
        .env("DOCKER_CLI_HINTS", "false");
    command
}

pub fn assess(signals: Option<&Signals>) -> Result<Docker, Error> {
    let executable = discover(std::env::var_os("PATH").as_deref())?;
    let mut process = command(&executable);
    process.args(["version", "--format", "{{json .Server}}"]);
    let output = runner::observe(&mut process, signals).map_err(|cause| Error::operational(
        "check Docker server", executable.display().to_string(), cause, "Check Docker CLI access and the configured context/endpoint; Larainit will not start or repair it"))?;
    let version = server_version(&output).map_err(|cause| Error::operational(
        "check Docker server", executable.display().to_string(), cause, "Check the configured Docker server and permissions; client presence alone is insufficient"))?;
    Ok(Docker {
        executable,
        version,
    })
}

fn server_version(output: &Observation) -> Result<String, String> {
    if !output.status.success() {
        return Err(failed_response(output));
    }
    let response: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| "malformed Docker server JSON".to_owned())?;
    response
        .as_object()
        .and_then(|server| server.get("Version"))
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            "missing non-null Docker server object/version (not client-only output)".to_owned()
        })
}

pub fn failed_response(output: &Observation) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let denial = if stderr.to_ascii_lowercase().contains("permission denied") {
        "access denied"
    } else {
        "failed response"
    };
    format!(
        "{denial} (status {}): {}",
        crate::error::child_status(output.status),
        stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn readiness_requires_successful_server_json() {
        for data in [
            "null",
            "{}",
            "{\"Client\":{\"Version\":\"x\"}}",
            "not JSON",
            "{\"Version\":\"\"}",
        ] {
            let output = Observation {
                status: std::process::ExitStatus::from_raw(0),
                stdout: data.into(),
                stderr: vec![],
            };
            assert!(server_version(&output).is_err());
        }
        let mut output = Observation {
            status: std::process::ExitStatus::from_raw(0),
            stdout: br#"{"Version":"test","Extra":true}"#.to_vec(),
            stderr: vec![],
        };
        assert_eq!(server_version(&output).unwrap(), "test");
        output.status = std::process::ExitStatus::from_raw(1 << 8);
        output.stderr = b"permission denied".to_vec();
        assert!(
            server_version(&output)
                .unwrap_err()
                .contains("access denied")
        );
        assert!(discover(None).unwrap_err().to_string().contains("absent"));
    }
}
