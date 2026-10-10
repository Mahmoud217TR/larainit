mod support;
use std::fs;
use std::time::{Duration, Instant};
use support::Fixture;

#[test]
fn readiness_matrix_has_no_podman_or_context_fallback() {
    for engine in ["auto", "docker"] {
        for mode in [
            "healthy",
            "absent",
            "exit=1",
            "malformed",
            "hang",
            "flood",
            "retain-pipes",
        ] {
            let fixture = Fixture::new();
            let bin = fixture.install_docker();
            fs::copy(bin.join("docker"), bin.join("podman")).unwrap();
            if mode == "absent" {
                fs::remove_file(bin.join("docker")).unwrap();
            } else if mode != "healthy" {
                fixture.write("version-mode", mode);
            }
            if mode == "exit=1" {
                fixture.write("version-stderr", "permission denied");
            }
            let start = Instant::now();
            let output = fixture
                .larainit()
                .env("PATH", bin)
                .args(["--engine", engine, "php"])
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(if mode == "healthy" { 0 } else { 1 })
            );
            assert!(start.elapsed() < Duration::from_secs(8));
            assert!(!fixture.root.join("invoked-podman").exists());
            assert!(!fixture.root.join("unexpected-command").exists());
            if mode != "healthy" {
                assert!(!fixture.root.join("cache").exists());
            }
        }
    }
}

#[test]
fn null_client_only_and_malformed_responses_are_not_readiness() {
    for response in [
        "null",
        "{}",
        "{\"Client\":{\"Version\":\"test\"}}",
        "not JSON",
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("version-stdout", response);
        assert_eq!(
            fixture
                .larainit()
                .env("PATH", bin)
                .arg("php")
                .output()
                .unwrap()
                .status
                .code(),
            Some(1)
        );
        assert_eq!(fixture.calls().len(), 1);
    }
}

#[test]
fn executable_removed_after_discovery_does_not_fall_back() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("version-mode", "remove-self");
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .arg("php")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fixture.calls().len(), 1);
    assert!(!fixture.root.join("cache").exists());
}

#[test]
fn image_identity_platform_and_run_failure_never_reselect_or_provision() {
    for response in [
        "null".into(),
        "{}".into(),
        format!(
            r#"{{"Id":"sha256:{}","Os":"linux","Architecture":"arm64"}}"#,
            "a".repeat(64)
        ),
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("inspect-stdout", response);
        assert_eq!(
            fixture
                .larainit()
                .env("PATH", bin)
                .arg("php")
                .output()
                .unwrap()
                .status
                .code(),
            Some(1)
        );
        assert_eq!(fixture.calls().len(), 2);
        assert!(!fixture.root.join("cache").exists());
    }
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("version-stdout", r#"{"Version":"test","FutureField":true}"#);
    fixture.write("run-mode", "exit=125");
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .env("DOCKER_CONTEXT", "kept-context")
        .env("DOCKER_HOST", "unix:///kept-endpoint")
        .env("DOCKER_CONFIG", fixture.root.join("docker-config"))
        .args(["--image", "custom:tag", "php"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    let calls = fixture.calls();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[3][0], "stop");
    assert_eq!(calls[4][0], "rm");
    assert_eq!(calls[3].last(), calls[4].last());
    assert_eq!(calls[1][5], "custom:tag");
    assert!(calls[2].contains(&format!("sha256:{}", "a".repeat(64)).into()));
    assert!(!calls[2].contains(&"custom:tag".into()));
    assert!(calls[2].contains(&"--pull=never".into()));
    let environment = fs::read(fixture.root.join("environment.bin")).unwrap();
    for expected in [
        "kept-context",
        "unix:///kept-endpoint",
        "DOCKER_CLI_HOOKS",
        "DOCKER_CLI_HINTS",
        "false",
    ] {
        assert!(
            environment
                .windows(expected.len())
                .any(|value| value == expected.as_bytes())
        );
    }
}
