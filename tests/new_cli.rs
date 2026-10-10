mod support;

use support::Fixture;

#[test]
fn new_maps_to_the_actual_installer_in_the_invocation_directory() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .args([
            "new",
            "my app",
            "",
            "--",
            "--help",
            "--engine",
            "it's 日本語",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    let run = calls.iter().find(|call| call[0] == "run").unwrap();
    let entrypoint = run
        .iter()
        .position(|value| value == "--entrypoint")
        .unwrap();
    assert_eq!(run[entrypoint + 1], "laravel");
    assert_eq!(
        &run[entrypoint + 4..],
        &[
            "new",
            "my app",
            "",
            "--",
            "--help",
            "--engine",
            "it's 日本語"
        ]
    );
    let workspace = fixture.root.canonicalize().unwrap();
    assert!(run.iter().any(|value| {
        value.to_str().is_some_and(|value| {
            value.contains(workspace.to_str().unwrap()) && value.ends_with("dst=/workspace")
        })
    }));
    assert!(!run.iter().any(|value| value == "create-project"));
    assert!(!fixture.root.join("unexpected-command").exists());
}

#[test]
fn missing_and_uninspectable_images_require_manual_preparation_not_provisioning() {
    for stderr in [
        "Error response from daemon: No such image: missing:tag",
        "permission denied",
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("inspect-mode", "exit=1");
        fixture.write("inspect-stderr", stderr);
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["--image", "missing:tag", "new", "my-app"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        for expected in [
            "missing:tag",
            "Docker",
            "context",
            "manually",
            "docker build",
            "source checkout",
            stderr,
        ] {
            assert!(error.contains(expected), "missing {expected:?}: {error}");
        }
        let calls = fixture.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][0], "version");
        assert_eq!(calls[1][0], "image");
        assert!(!fixture.root.join("cache").exists());
        assert!(!fixture.root.join("unexpected-command").exists());
    }
}
