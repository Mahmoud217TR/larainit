mod support;

use std::process::Command;
use support::Fixture;

#[test]
fn independent_stores_and_readiness() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.install_engine("podman");
    fixture.write(
        "podman/info-stdout",
        b"{\"host\":{\"security\":{\"rootless\":true}}}",
    );
    fixture.write("podman/inspect-stdout", b"[{\"Id\":\"podman-only\"}]");
    let mut docker = fixture.docker();
    assert!(
        docker
            .args(["version", "--format", "{{json .Server}}"])
            .status()
            .unwrap()
            .success()
    );
    let mut podman = Command::new(bin.join("podman"));
    podman.env("MOCK_ROOT", &fixture.root);
    assert!(
        podman
            .args(["info", "--format", "json"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = Command::new(bin.join("podman"))
        .env("MOCK_ROOT", &fixture.root)
        .args(["image", "inspect", "--format", "{{json .}}", "--", "local"])
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"[{\"Id\":\"podman-only\"}]");
    assert_eq!(fixture.calls().len(), 1);
    assert_eq!(fixture.engine_calls("podman").len(), 2);
    assert!(!fixture.root.join("invoked-podman").exists());
}

#[test]
fn explicit_build_identity_and_metadata_output() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.install_engine("sqlite3");
    let iid = fixture.root.join("own-iid");
    fixture.write("build-iid", format!("sha256:{}", "b".repeat(64)));
    assert!(
        fixture
            .docker()
            .arg("build")
            .arg("--iidfile")
            .arg(&iid)
            .arg(".")
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(iid).unwrap(),
        format!("sha256:{}", "b".repeat(64))
    );
    fixture.write("sqlite3/metadata-stdout", "[{\"initialized\":1}]");
    let output = Command::new(bin.join("sqlite3"))
        .env("MOCK_ROOT", &fixture.root)
        .args([
            "-init",
            "/dev/null",
            "-readonly",
            "-json",
            "existing.db",
            "SELECT 1;",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"[{\"initialized\":1}]");
    assert_eq!(fixture.engine_calls("sqlite3").len(), 1);
}

#[test]
fn unconfigured_operations_fail_closed() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    std::fs::hard_link(bin.join("docker"), bin.join("podman")).unwrap();
    let output = Command::new(bin.join("podman"))
        .env("MOCK_ROOT", &fixture.root)
        .args(["info", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(99));
    assert!(fixture.root.join("invoked-podman").exists());
    assert_eq!(
        fixture
            .docker()
            .args(["build", "."])
            .output()
            .unwrap()
            .status
            .code(),
        Some(99)
    );
    assert_eq!(
        fixture
            .docker()
            .arg("prune")
            .output()
            .unwrap()
            .status
            .code(),
        Some(99)
    );
}

#[test]
fn podman_failures_and_cleanup_are_independent() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.install_engine("podman");
    fixture.write("podman/info-mode", "malformed");
    let command = || {
        let mut command = Command::new(bin.join("podman"));
        command.env("MOCK_ROOT", &fixture.root);
        command
    };
    assert_eq!(
        command()
            .args(["info", "--format", "json"])
            .output()
            .unwrap()
            .stdout,
        b"not JSON"
    );
    fixture.write("podman/info-mode", "exit=1");
    fixture.write("podman/info-stderr", "permission denied");
    let output = command()
        .args(["info", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, b"permission denied");
    let id = "a".repeat(64);
    assert!(
        command()
            .args(["kill", "--signal", "SIGINT", "--", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("podman/stopped-status")).unwrap(),
        "130"
    );
    assert!(
        command()
            .args(["rm", "--force", "--", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!fixture.root.join("stopped-status").exists());
    fixture.write("podman/info-mode", "hang");
    let mut child = command()
        .args(["info", "--format", "json"])
        .spawn()
        .unwrap();
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
}
