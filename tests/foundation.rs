mod support;

use std::fs;
use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};
use support::Fixture;

#[test]
fn local_routes_do_not_load_configuration_probe_or_write() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("config/larainit")).unwrap();
    fixture.write("config/larainit/config.toml", "unknown = 'private value'");
    for (args, status) in [
        (vec!["--help"], 0),
        (vec!["--version"], 0),
        (vec!["--engine", "podman", "php"], 2),
        (vec!["--unknown"], 2),
        (vec!["doctor", "extra"], 2),
        (vec![], 2),
        (vec!["engine"], 1),
        (vec!["build"], 1),
        (vec!["config"], 1),
    ] {
        let output = fixture
            .larainit()
            .args(args)
            .env("PATH", "")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(status));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private value"));
    }
    assert!(fixture.calls().is_empty());
    assert!(!fixture.root.join("cache").exists());
}

#[test]
fn prerequisites_and_transport_fail_without_cache_preparation() {
    for mode in ["version-failure", "image-failure", "bad-byte"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        if mode == "version-failure" {
            fixture.write("version-mode", "exit=1");
            fixture.write("version-stderr", "permission denied");
        } else if mode == "image-failure" {
            fixture.write("inspect-mode", "exit=1");
            fixture.write("inspect-stderr", "Error: No such image: test:tag");
        }
        let mut command = fixture.larainit();
        command
            .env("PATH", bin)
            .args(["--image", "test:tag", "php"]);
        if mode == "bad-byte" {
            use std::os::unix::ffi::OsStringExt;
            command.arg(std::ffi::OsString::from_vec(vec![0xff]));
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(!fixture.root.join("cache").exists());
        assert!(!fixture.calls().iter().any(|call| call[0] == "run"));
        assert!(!fixture.root.join("unexpected-command").exists());
    }
}

#[test]
fn safe_execution_inherits_streams_status_and_cleans_only_private_scratch() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    let runtime = fixture.root.join("runtime");
    fs::create_dir(&runtime).unwrap();
    fixture.write("run-mode", "exit=42");
    fixture.write("run-stdout", "out\n");
    fixture.write("run-stderr", "err\n");
    let output = fixture
        .larainit()
        .env("PATH", &bin)
        .env("TMPDIR", &runtime)
        .args(["php", "", "a b", "--help"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(output.stdout, b"out\n");
    assert_eq!(output.stderr, b"err\n");
    let calls = fixture.calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[3][0], "stop");
    assert_eq!(calls[4][0], "rm");
    assert_eq!(calls[3].last(), calls[4].last());
    let run = &calls[2];
    assert_eq!(run[0], "run");
    assert!(run.contains(&"--pull=never".into()));
    assert!(!run.contains(&"-t".into()));
    assert_eq!(run.iter().filter(|arg| *arg == "--mount").count(), 3);
    assert_eq!(&run[run.len() - 3..], &["", "a b", "--help"]);
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    assert!(fixture.root.join("cache/larainit/composer").is_dir());
    assert!(fixture.root.join("cache/larainit/npm").is_dir());
    fixture.write("run-mode", "stdin");
    fixture.write("run-stdout", "");
    fixture.write("run-stderr", "");
    let mut child = fixture
        .larainit()
        .env("PATH", bin)
        .arg("bash")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"piped input\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.stdout, b"piped input\n");
    assert!(output.status.success());
}

#[test]
fn interruption_targets_captured_id_and_preserves_returned_status() {
    for signal in [
        nix::sys::signal::Signal::SIGINT,
        nix::sys::signal::Signal::SIGTERM,
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("run-mode", "running");
        let mut child = fixture
            .larainit()
            .env("PATH", bin)
            .arg("php")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.root.join("run-started").exists() {
            assert!(Instant::now() < deadline, "fake run did not start");
            assert!(
                child.try_wait().unwrap().is_none(),
                "wrapper exited before fake run"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id() as i32), signal).unwrap();
        let start = Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                child.kill().unwrap();
                panic!("interruption was not bounded");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(128 + signal as i32));
        let calls = fixture.calls();
        let stop = calls.iter().find(|call| call[0] == "stop").unwrap();
        assert_eq!(stop.last().unwrap(), std::ffi::OsStr::new(&"a".repeat(64)));
        assert!(!fixture.root.join("unexpected-command").exists());
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
