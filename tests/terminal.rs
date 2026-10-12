mod support;

use std::{fs, path::PathBuf, process::Command};
use support::Fixture;

#[test]
fn successful_detach_cleans_only_captured_container_and_restores_terminal() {
    check_detach("normal", true);
}

#[test]
fn successful_detach_without_cid_never_guesses_cleanup_target() {
    check_detach("missing-cid", false);
}

#[test]
fn successful_detach_with_malformed_cid_never_guesses_cleanup_target() {
    check_detach("bad-cid", false);
}

fn check_detach(cid_behavior: &str, authorized: bool) {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.install_engine("podman");
    fixture.write("run-mode", "detach");
    fixture.write("run-cid-behavior", cid_behavior);
    fixture.write("unrelated-sentinel", "untouched");
    let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("python3"))
        .find(|path| path.is_file())
        .expect("developer PTY tests require Python 3")
        .canonicalize()
        .unwrap();
    let harness = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/pty-check.py");
    let output = Command::new(python)
        .args([
            "-c",
            r#"
import importlib.util, pathlib, sys
spec = importlib.util.spec_from_file_location('pty_check', sys.argv[1])
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)
harness.root, harness.binary, harness.image = pathlib.Path(sys.argv[2]), sys.argv[3], 'test:tag'
harness.engine, harness.explicit_engine, harness.results = 'docker', True, []
authorized = sys.argv[4] == 'normal'
result, _ = harness.run('detach', ['bash'], b'\x10\x11', capture_id=authorized)
assert result['status'] == 0, result
assert result['terminal_restored'] and result['foreground_restored'], result
assert ('container_id' in result) == authorized, result
if authorized:
    assert result['container_id'] == 'a' * 64, result
"#,
        ])
        .arg(harness)
        .arg(&fixture.root)
        .arg(env!("CARGO_BIN_EXE_larainit"))
        .arg(cid_behavior)
        .env("PATH", bin)
        .env("MOCK_ROOT", &fixture.root)
        .env_remove("LARAINIT_ENGINE")
        .env_remove("LARAINIT_IMAGE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(fixture.root.join("detach-received")).unwrap(),
        b"\x10\x11"
    );
    let calls = fixture.calls();
    assert_eq!(calls.len(), if authorized { 5 } else { 3 }, "{calls:?}");
    assert_eq!(calls[2][0], "run");
    assert!(calls[2].contains(&"-t".into()));
    if authorized {
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
    assert!(fixture.engine_calls("podman").is_empty());
    assert_eq!(
        fs::read(fixture.root.join("unrelated-sentinel")).unwrap(),
        b"untouched"
    );
    assert!(!fixture.root.join("unexpected-command").exists());
    assert_eq!(
        fs::read_dir(fixture.root.join("detach/runtime"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn failed_terminal_handoff_cleans_only_captured_container_and_restores_terminal() {
    check_handoff("normal", true);
}

#[test]
fn failed_terminal_handoff_without_cid_never_guesses_cleanup_target() {
    check_handoff("missing-cid", false);
}

#[test]
fn failed_terminal_handoff_with_malformed_cid_never_guesses_cleanup_target() {
    check_handoff("bad-cid", false);
}

fn check_handoff(cid_behavior: &str, authorized: bool) {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.install_engine("podman");
    fixture.write("run-mode", "running");
    fixture.write("run-cid-behavior", cid_behavior);
    fixture.write("unrelated-sentinel", "untouched");
    // Test-only libc fault injection; no Larainit runtime flag or real engine is involved.
    fixture.write(
        "handoff.c",
        r#"
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>
int tcsetpgrp(int fd, pid_t group) {
    static int injected;
    if (injected++) return ioctl(fd, TIOCSPGRP, &group);
    const char *root = getenv("MOCK_ROOT");
    char marker[4096];
    int length = root ? snprintf(marker, sizeof marker, "%s/run-started", root) : -1;
    if (length > 0 && length < (int)sizeof marker) {
        const struct timespec pause = {0, 10000000};
        for (int attempt = 0; attempt < 200 && access(marker, F_OK) != 0; attempt++) {
            nanosleep(&pause, NULL);
        }
    }
    errno = ENOTTY;
    return -1;
}
"#,
    );
    let library = fixture.root.join("handoff.so");
    let compiled = Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .arg(fixture.root.join("handoff.c"))
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let python = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("python3"))
        .find(|path| path.is_file())
        .expect("developer PTY tests require Python 3")
        .canonicalize()
        .unwrap();
    let harness = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/pty-check.py");
    let output = Command::new(python)
        .args([
            "-c",
            r#"
import importlib.util, os, pathlib, sys
os.umask(0o002)
spec = importlib.util.spec_from_file_location('pty_check', sys.argv[1])
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)
harness.root, harness.binary, harness.image = pathlib.Path(sys.argv[2]), sys.argv[3], 'test:tag'
harness.engine, harness.explicit_engine, harness.results = 'docker', True, []
result, _ = harness.run('handoff', ['php'])
assert result['status'] == 1, result
assert result['terminal_restored'] and result['foreground_restored'], result
log = (harness.root / 'handoff' / 'terminal.log').read_text()
assert 'manage terminal' in log, log
assert 'restore terminal' not in log, log
"#,
        ])
        .arg(harness)
        .arg(&fixture.root)
        .arg(env!("CARGO_BIN_EXE_larainit"))
        .env("PATH", bin)
        .env("MOCK_ROOT", &fixture.root)
        .env("LD_PRELOAD", library)
        .env_remove("LARAINIT_ENGINE")
        .env_remove("LARAINIT_IMAGE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    assert_eq!(calls.len(), if authorized { 5 } else { 3 }, "{calls:?}");
    assert_eq!(calls[2][0], "run");
    if authorized {
        assert_eq!(calls[3][0], "stop");
        assert_eq!(calls[4][0], "rm");
    }
    for call in &calls[3..] {
        assert_eq!(
            call.last().unwrap(),
            &std::ffi::OsString::from("a".repeat(64))
        );
    }
    assert!(fixture.engine_calls("podman").is_empty());
    assert_eq!(
        fs::read(fixture.root.join("unrelated-sentinel")).unwrap(),
        b"untouched"
    );
    assert!(!fixture.root.join("unexpected-command").exists());
    assert_eq!(
        fs::read_dir(fixture.root.join("handoff/runtime"))
            .unwrap()
            .count(),
        0
    );
}
