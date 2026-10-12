use crate::{
    error::Error,
    runner::{self, Observation, Signals},
};
use nix::unistd::{AccessFlags, access};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

mod podman;

struct ObservationBudget {
    deadline: Instant,
    remaining: usize,
}

impl ObservationBudget {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(5),
            remaining: 64 * 1024,
        }
    }

    fn observe(
        &mut self,
        command: &mut Command,
        signals: Option<&Signals>,
    ) -> Result<Observation, runner::ProbeFailure> {
        let timeout = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or(runner::ProbeFailure::Timeout)?;
        let result = runner::observe_with_limits(command, signals, timeout, self.remaining);
        match &result {
            Ok(output) => self.remaining -= output.stdout.len() + output.stderr.len(),
            Err(_) => {
                self.remaining = 0;
                self.deadline = Instant::now();
            }
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Docker,
    Podman,
}

impl std::fmt::Display for EngineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        })
    }
}

pub struct ResolvedEngine {
    pub kind: EngineKind,
    pub executable: PathBuf,
    pub version: String,
    pub selection_reason: &'static str,
}

pub fn discover(path: Option<&std::ffi::OsStr>) -> Result<PathBuf, Error> {
    discover_kind(EngineKind::Docker, path)
}

fn discover_kind(kind: EngineKind, path: Option<&std::ffi::OsStr>) -> Result<PathBuf, Error> {
    let (label, action) = match kind {
        EngineKind::Docker => ("Docker", "locate Docker"),
        EngineKind::Podman => ("Podman", "locate Podman"),
    };
    let mut denied = None;
    for directory in path.into_iter().flat_map(std::env::split_paths) {
        let candidate = directory.join(kind.to_string());
        match std::fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => {
                if access(&candidate, AccessFlags::X_OK).is_ok() {
                    return std::path::absolute(&candidate).map_err(|error| {
                        Error::operational(
                            action,
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
            action,
            path.display().to_string(),
            "executable access denied",
            format!("Fix {label} CLI/PATH permissions without elevating Larainit"),
        ))
    } else {
        Err(Error::operational(
            action,
            "PATH",
            format!("{label} executable absent"),
            format!(
                "Install/configure {label} independently and make {kind} available on PATH; executable presence is not readiness"
            ),
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

pub fn assess_preference(
    preference: crate::config::EnginePreference,
    signals: Option<&Signals>,
) -> Result<ResolvedEngine, Error> {
    // No last-successful preference is persisted; execution failures never reselect.
    select_ready(preference, |kind| match kind {
        EngineKind::Docker => assess(signals),
        EngineKind::Podman => uncertified_podman(signals),
    })
}

pub(crate) fn select_ready(
    preference: crate::config::EnginePreference,
    mut observe: impl FnMut(EngineKind) -> Result<ResolvedEngine, Error>,
) -> Result<ResolvedEngine, Error> {
    if let Some(kind) = preference.explicit_kind() {
        let mut selected = observe(kind)?;
        selected.selection_reason = "explicit preference; no fallback";
        return Ok(selected);
    }
    match observe(EngineKind::Docker) {
        Ok(mut selected) => {
            selected.selection_reason = "auto: healthy Docker preferred";
            Ok(selected)
        }
        Err(docker) => match observe(EngineKind::Podman) {
            Ok(mut selected) => {
                selected.selection_reason =
                    "auto: Docker readiness failed; supported local rootless Podman selected";
                Ok(selected)
            }
            Err(podman) => Err(Error::operational(
                "select engine",
                "auto",
                format!("Docker: {docker}; Podman: {podman}"),
                "Configure a healthy engine independently; no engine was started or repaired",
            )),
        },
    }
}

fn uncertified_podman(signals: Option<&Signals>) -> Result<ResolvedEngine, Error> {
    let budget = ObservationBudget::new();
    for variable in ["CONTAINER_HOST", "CONTAINER_CONNECTION"] {
        if std::env::var_os(variable).is_some() {
            return Err(Error::operational(
                "check Podman mode",
                variable,
                "remote mode indicator is present (including empty values); unsupported/not checked",
                "Use a supported local rootless environment; Larainit will not clear connection settings or contact a remote engine",
            ));
        }
    }
    if nix::unistd::geteuid().is_root() {
        return Err(Error::operational(
            "check Podman mode",
            "effective user",
            "rootful Podman is unsupported/not checked",
            "Use a separately prepared non-root fixture; no elevation or rootful fallback is attempted",
        ));
    }
    let executable = discover_kind(EngineKind::Podman, std::env::var_os("PATH").as_deref())?;
    podman::assess(executable, signals, budget)
}

pub fn assess(signals: Option<&Signals>) -> Result<ResolvedEngine, Error> {
    let mut budget = ObservationBudget::new();
    let executable = discover(std::env::var_os("PATH").as_deref())?;
    let mut process = command(&executable);
    process.args(["version", "--format", "{{json .Server}}"]);
    let output = budget.observe(&mut process, signals).map_err(|cause| Error::operational(
        "check Docker server", executable.display().to_string(), cause, "Check Docker CLI access and the configured context/endpoint; Larainit will not start or repair it"))?;
    let version = server_version(&output).map_err(|cause| Error::operational(
        "check Docker server", executable.display().to_string(), cause, "Check the configured Docker server and permissions; client presence alone is insufficient"))?;
    Ok(ResolvedEngine {
        kind: EngineKind::Docker,
        executable,
        version,
        selection_reason: "Docker readiness observation",
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
    fn candidate_observations_share_capture_and_time_budgets() {
        let fixture = crate::test_support::Fixture::new();
        let command = || {
            let mut command = fixture.docker();
            command.args(["version", "--format", "{{json .Server}}"]);
            command
        };
        fixture.write("version-stdout", "a".repeat(24 * 1024));
        let mut budget = ObservationBudget::new();
        assert!(budget.observe(&mut command(), None).is_ok());
        assert!(budget.observe(&mut command(), None).is_ok());
        assert!(matches!(
            budget.observe(&mut command(), None),
            Err(runner::ProbeFailure::Overflow)
        ));
        let calls = fixture.calls().len();
        assert!(budget.observe(&mut command(), None).is_err());
        assert_eq!(fixture.calls().len(), calls);
        fixture.write("version-stdout", "small");
        let mut budget = ObservationBudget::new();
        assert!(budget.observe(&mut command(), None).is_ok());
        budget.deadline = std::time::Instant::now();
        let calls = fixture.calls().len();
        assert!(matches!(
            budget.observe(&mut command(), None),
            Err(runner::ProbeFailure::Timeout)
        ));
        assert_eq!(
            fixture.calls().len(),
            calls,
            "candidate deadline was reset per subprocess"
        );
    }

    #[test]
    fn selection_matrix_is_readiness_only_docker_first_and_strict() {
        use crate::config::EnginePreference;
        for preference in [
            EnginePreference::Auto,
            EnginePreference::Docker,
            EnginePreference::Podman,
        ] {
            for docker_ready in [false, true] {
                for podman_ready in [false, true] {
                    let mut calls = Vec::new();
                    let result = select_ready(preference, |kind| {
                        calls.push(kind);
                        let ready = match kind {
                            EngineKind::Docker => docker_ready,
                            EngineKind::Podman => podman_ready,
                        };
                        if ready {
                            Ok(ResolvedEngine {
                                kind,
                                executable: kind.to_string().into(),
                                version: "mock".into(),
                                selection_reason: "",
                            })
                        } else {
                            Err(Error::operational(
                                "observe readiness",
                                kind.to_string(),
                                "unavailable",
                                "operator setup required",
                            ))
                        }
                    });
                    let expected = match preference {
                        EnginePreference::Auto if docker_ready => Some(EngineKind::Docker),
                        EnginePreference::Auto if podman_ready => Some(EngineKind::Podman),
                        EnginePreference::Docker if docker_ready => Some(EngineKind::Docker),
                        EnginePreference::Podman if podman_ready => Some(EngineKind::Podman),
                        _ => None,
                    };
                    assert_eq!(result.as_ref().ok().map(|engine| engine.kind), expected);
                    let expected_calls = match preference {
                        EnginePreference::Auto if !docker_ready => {
                            vec![EngineKind::Docker, EngineKind::Podman]
                        }
                        EnginePreference::Auto | EnginePreference::Docker => {
                            vec![EngineKind::Docker]
                        }
                        EnginePreference::Podman => vec![EngineKind::Podman],
                    };
                    assert_eq!(calls, expected_calls);
                    if let Ok(engine) = result {
                        assert!(!engine.selection_reason.is_empty());
                    }
                }
            }
        }
    }

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
