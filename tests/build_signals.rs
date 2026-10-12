mod support;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use std::{
    process::Stdio,
    time::{Duration, Instant},
};
use support::Fixture;

#[test]
fn build_output_is_streamed_beyond_probe_capture_limits() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("source/Dockerfile", "FROM scratch\n");
    fixture.write("build-mode", "exit=41");
    fixture.write("build-stdout", "o".repeat(128 * 1024));
    fixture.write("build-stderr", "e".repeat(128 * 1024));
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .args(["build", "--context", "source"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(41));
    assert_eq!(output.stdout.len(), 128 * 1024);
    assert_eq!(output.stderr.len(), 128 * 1024);
    assert!(!fixture.calls().iter().any(|call| call[0] == "image"));
}

#[test]
fn long_build_has_no_probe_timeout_and_cancellation_never_authorizes_container_cleanup() {
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("source/Dockerfile", "FROM scratch\n");
        fixture.write("build-mode", "running");
        fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
        let mut child = fixture
            .larainit()
            .env("PATH", bin)
            .args(["build", "--context", "source"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.root.join("build-started").exists() {
            assert!(child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_secs(6));
        assert!(
            child.try_wait().unwrap().is_none(),
            "build incorrectly used observation timeout"
        );
        kill(Pid::from_raw(child.id() as i32), signal).unwrap();
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > Duration::from_secs(10) {
                child.kill().unwrap();
                panic!("build cancellation exceeded bound");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(128 + signal as i32));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("verified build"));
        let calls = fixture.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][0], "version");
        assert_eq!(calls[1][0], "build");
        let iid = calls[1].iter().position(|arg| arg == "--iidfile").unwrap();
        assert!(!std::path::Path::new(&calls[1][iid + 1]).exists());
        assert!(!fixture.root.join("config").exists());
        assert!(!fixture.root.join("cache").exists());
    }
}

#[test]
fn cancellation_retains_leader_until_descendant_escalation_and_preserves_unrelated_processes() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("source/Dockerfile", "FROM scratch\n");
    fixture.write("build-mode", "running-descendant");
    fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
    let mut sentinel = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let mut child = fixture
        .larainit()
        .env("PATH", bin)
        .args(["build", "--context", "source"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fixture.root.join("build-descendant-pid").exists() {
        assert!(child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let leader: u32 = std::fs::read_to_string(fixture.root.join("build-leader-pid"))
        .unwrap()
        .parse()
        .unwrap();
    let descendant: u32 = std::fs::read_to_string(fixture.root.join("build-descendant-pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let state = |pid| {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, tail)| tail.as_bytes()[0]))
    };
    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
    let start = Instant::now();
    while state(leader) != Some(b'Z') {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "client leader was reaped before final group signal"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        state(descendant).is_some_and(|state| state != b'Z'),
        "descendant must ignore graceful signal"
    );
    assert!(sentinel.try_wait().unwrap().is_none());
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            sentinel.kill().unwrap();
            sentinel.wait().unwrap();
            panic!("build cancellation exceeded bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let sentinel_alive = sentinel.try_wait().unwrap().is_none();
    if sentinel_alive {
        sentinel.kill().unwrap();
        sentinel.wait().unwrap();
    }
    assert!(sentinel_alive);
    assert_eq!(output.status.code(), Some(143));
    assert!(
        matches!(state(descendant), None | Some(b'Z')),
        "owned descendant survived group escalation"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("verified build"));
    assert_eq!(fixture.calls().len(), 2);
}

#[test]
fn cancellation_preserves_custom_status_but_never_reports_zero_as_verified_success() {
    for (mode, expected) in [("signal-exit-42", 42), ("signal-exit-0", 143)] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("source/Dockerfile", "FROM scratch\n");
        fixture.write("build-mode", mode);
        fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
        fixture.write(
            "config/larainit/config.toml",
            "engine='docker'\nimage='saved:tag'\n",
        );
        let before = std::fs::read(fixture.root.join("config/larainit/config.toml")).unwrap();
        let mut child = fixture
            .larainit()
            .env("PATH", bin)
            .args(["build", "--context", "source"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.root.join("build-trap-ready").exists() {
            assert!(child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > Duration::from_secs(10) {
                child.kill().unwrap();
                panic!("build cancellation exceeded bound");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(expected));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("verified build"));
        assert_eq!(fixture.calls().len(), 2);
        assert_eq!(
            std::fs::read(fixture.root.join("config/larainit/config.toml")).unwrap(),
            before
        );
        let calls = fixture.calls();
        let index = calls[1]
            .iter()
            .position(|value| value == "--iidfile")
            .unwrap();
        assert!(!std::path::Path::new(&calls[1][index + 1]).exists());
    }
}
