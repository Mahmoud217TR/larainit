mod support;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::process::Stdio;
use std::time::{Duration, Instant};
use support::Fixture;

fn interrupted(
    fixture: &Fixture,
    bin: &std::path::Path,
    before_cid: bool,
    limit: Duration,
) -> std::process::Output {
    let mut child = fixture
        .larainit()
        .env("PATH", bin)
        .arg("php")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ready = if before_cid {
            fixture.calls().iter().any(|call| call[0] == "run")
        } else {
            fixture.root.join("run-started").exists()
        };
        if ready {
            break;
        }
        assert!(Instant::now() < deadline);
        assert!(child.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(10));
    }
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > limit {
            child.kill().unwrap();
            panic!("interruption exceeded test bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn created_but_never_started_failure_requires_valid_cid_before_removal() {
    for behavior in ["normal", "missing-cid", "bad-cid"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.install_engine("podman");
        fixture.write("run-mode", "created-exit=125");
        fixture.write("run-cid-behavior", behavior);
        fixture.write("unrelated-container", "untouched");
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["--engine", "docker", "php"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&output.stderr).contains("workload never started"));
        let calls = fixture.calls();
        assert_eq!(
            calls.len(),
            if behavior == "normal" { 5 } else { 3 },
            "{calls:?}"
        );
        assert_eq!(calls[2][0], "run");
        if behavior == "normal" {
            let id = "a".repeat(64);
            assert_eq!(
                calls[3],
                ["stop", "--signal", "SIGTERM", "--timeout", "5", "--", &id]
                    .map(std::ffi::OsString::from)
            );
            assert_eq!(
                calls[4],
                ["rm", "--force", "--volumes", "--", &id].map(std::ffi::OsString::from)
            );
        }
        assert_eq!(
            std::fs::read(fixture.root.join("container-state")).unwrap(),
            if behavior == "normal" {
                b"removed".as_slice()
            } else {
                b"created".as_slice()
            }
        );
        assert!(!fixture.root.join("run-started").exists());
        assert!(fixture.engine_calls("podman").is_empty());
        assert_eq!(
            std::fs::read(fixture.root.join("unrelated-container")).unwrap(),
            b"untouched"
        );
        assert!(!fixture.root.join("unexpected-command").exists());
    }
}

#[test]
fn missing_malformed_and_late_identity_never_authorize_guessed_cleanup() {
    for mode in ["running-missing-cid", "running-bad-cid", "running-late-cid"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("run-mode", mode);
        let output = interrupted(
            &fixture,
            &bin,
            mode.ends_with("late-cid"),
            Duration::from_secs(4),
        );
        assert_eq!(output.status.code(), Some(143));
        let calls = fixture.calls();
        assert!(
            !calls
                .iter()
                .any(|call| call[0] == "stop" || call[0] == "rm")
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("cleanup"));
        assert!(!fixture.root.join("unexpected-command").exists());
    }
}

#[test]
fn cleanup_is_id_scoped_bounded_and_does_not_replace_status() {
    for mode in ["", "exit=1", "hang"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("run-mode", "running");
        fixture.write("cleanup-mode", mode);
        fixture.write("cleanup-stderr", "engine access failed");
        fixture.write("requested-status", "42");
        let output = interrupted(&fixture, &bin, false, Duration::from_secs(18));
        assert_eq!(
            output.status.code(),
            Some(if mode.is_empty() { 42 } else { 143 })
        );
        for call in fixture
            .calls()
            .iter()
            .filter(|call| call[0] == "stop" || call[0] == "rm")
        {
            assert_eq!(call.last().unwrap(), std::ffi::OsStr::new(&"a".repeat(64)));
            assert!(call.contains(&"--".into()));
        }
        if !mode.is_empty() {
            assert!(String::from_utf8_lossy(&output.stderr).contains("cleanup failed"));
        }
        assert!(!fixture.root.join("unexpected-command").exists());
    }
}

#[test]
fn auto_removal_and_name_collisions_never_broaden_cleanup() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("run-mode", "running");
    fixture.write("cleanup-mode", "exit=1");
    fixture.write(
        "cleanup-stderr",
        "Error: No such container: already removed",
    );
    assert_eq!(
        interrupted(&fixture, &bin, false, Duration::from_secs(4))
            .status
            .code(),
        Some(143)
    );
    assert!(!fixture.calls().iter().any(|call| call[0] == "rm"));
    fixture.write("run-mode", "exit=125");
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .arg("php")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert_eq!(
        fixture
            .calls()
            .iter()
            .filter(|call| call[0] == "stop")
            .count(),
        2
    );
    assert!(!fixture.calls().iter().any(|call| call[0] == "rm"));
}
