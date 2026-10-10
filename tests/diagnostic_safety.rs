mod support;
use std::fs;
use support::Fixture;

#[test]
fn doctor_requests_only_bounded_observations_and_never_prepares_user_paths() {
    for mode in ["", "exit=1", "hang", "flood"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("version-mode", mode);
        fixture.write("workspace-sentinel", "unchanged");
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .arg("doctor")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if mode.is_empty() { 0 } else { 1 })
        );
        for call in fixture.calls() {
            assert!(call[0] == "version" || call[0] == "image");
        }
        assert!(!fixture.root.join("unexpected-command").exists());
        assert!(!fixture.root.join("config").exists());
        assert!(!fixture.root.join("cache").exists());
        assert_eq!(
            fs::read(fixture.root.join("workspace-sentinel")).unwrap(),
            b"unchanged"
        );
    }
}

#[test]
#[ignore = "requires explicitly prepared real Docker; run --ignored --test-threads=1"]
fn real_doctor_has_no_workload_or_user_file_effects_in_healthy_and_broken_contexts() {
    for broken in [false, true] {
        let fixture = Fixture::new();
        fixture.write("sentinel", "untouched");
        let docker_config = fixture.root.join("docker-config");
        let plugins = fixture.root.join("plugins");
        fs::create_dir(&docker_config).unwrap();
        fs::create_dir(&plugins).unwrap();
        fs::copy(support::mock_binary(), plugins.join("docker-sentinel")).unwrap();
        fs::write(
            docker_config.join("config.json"),
            serde_json::to_vec(&serde_json::json!({"cliPluginsExtraDirs":[plugins]})).unwrap(),
        )
        .unwrap();
        let config_before = fs::read(docker_config.join("config.json")).unwrap();
        let images_before = std::process::Command::new("docker")
            .args(["image", "ls", "--quiet", "--no-trunc"])
            .output()
            .unwrap();
        assert!(images_before.status.success());
        let before = std::process::Command::new("docker")
            .args([
                "ps",
                "--all",
                "--filter",
                "name=^/larainit-",
                "--format",
                "{{.ID}}",
            ])
            .output()
            .unwrap();
        assert!(before.status.success());
        let mut command = fixture.real_larainit();
        command
            .env("DOCKER_CONFIG", &docker_config)
            .env("MOCK_ROOT", &fixture.root);
        if broken {
            command.env(
                "DOCKER_CONTEXT",
                "larainit-intentionally-nonexistent-doctor-context",
            );
        }
        let output = command.arg("doctor").output().unwrap();
        assert_eq!(output.status.code(), Some(if broken { 1 } else { 0 }));
        assert!(!fixture.root.join("cache").exists());
        assert!(!fixture.root.join("config").exists());
        assert_eq!(
            fs::read(fixture.root.join("sentinel")).unwrap(),
            b"untouched"
        );
        let after = std::process::Command::new("docker")
            .args([
                "ps",
                "--all",
                "--filter",
                "name=^/larainit-",
                "--format",
                "{{.ID}}",
            ])
            .output()
            .unwrap();
        assert_eq!(before.stdout, after.stdout);
        let images_after = std::process::Command::new("docker")
            .args(["image", "ls", "--quiet", "--no-trunc"])
            .output()
            .unwrap();
        assert!(images_after.status.success());
        let identities = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            identities(&images_before.stdout),
            identities(&images_after.stdout)
        );
        assert_eq!(
            fs::read(docker_config.join("config.json")).unwrap(),
            config_before
        );
        assert!(
            fixture.calls().is_empty(),
            "arbitrary plugin metadata was invoked"
        );
        assert!(!fixture.root.join("unexpected-command").exists());
        let report = String::from_utf8(output.stdout).unwrap();
        assert!(report.contains("bookkeeping") && report.contains("socket activation"));
    }
}
