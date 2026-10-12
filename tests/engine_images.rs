mod support;

use support::Fixture;

#[test]
fn selected_docker_identity_is_the_only_run_target_and_never_reselected() {
    for failure in ["none", "image", "workspace", "run"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.install_engine("podman");
        let id = format!("sha256:{}", "d".repeat(64));
        fixture.write(
            "inspect-stdout",
            format!(r#"{{"Id":"{id}","Os":"linux","Architecture":"amd64"}}"#),
        );
        fixture.write("podman/inspect-stdout", format!(r#"[{{"Id":"{}","Os":"linux","Architecture":"amd64","ManifestType":"application/vnd.oci.image.manifest.v1+json"}}]"#, "e".repeat(64)));
        let mut command = fixture.larainit();
        match failure {
            "image" => {
                fixture.write("inspect-mode", "exit=1");
                fixture.write("inspect-stderr", "No such image: app:tag");
            }
            "workspace" => {
                let path = fixture.root.join("unrepresentable\nworkspace");
                std::fs::create_dir(&path).unwrap();
                command.current_dir(path);
            }
            "run" => fixture.write("run-mode", "exit=125"),
            _ => {}
        }
        let output = command
            .env("PATH", bin)
            .args(["--image", "app:tag", "php", "", "--help"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(match failure {
                "none" => 0,
                "run" => 125,
                _ => 1,
            })
        );
        let calls = fixture.calls();
        assert_eq!(calls.iter().filter(|call| call[0] == "version").count(), 1);
        assert_eq!(calls.iter().filter(|call| call[0] == "image").count(), 1);
        assert!(fixture.engine_calls("podman").is_empty());
        let run = calls.iter().find(|call| call[0] == "run");
        if matches!(failure, "none" | "run") {
            let run = run.unwrap();
            assert!(run.iter().any(|arg| arg == "--pull=never"));
            let entrypoint = run.iter().position(|arg| arg == "--entrypoint").unwrap();
            assert_eq!(run[entrypoint + 3], id.as_str());
            assert!(!run.iter().any(|arg| arg == "app:tag"));
        } else {
            assert!(run.is_none());
            assert!(!fixture.root.join("cache").exists());
        }
        assert!(
            !calls
                .iter()
                .any(|call| matches!(call[0].to_str(), Some("pull" | "build" | "login")))
        );
    }
}

#[test]
fn retag_or_removal_after_inspection_never_executes_a_replacement_or_searches_another_store() {
    for change in ["retag", "remove"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.install_engine("podman");
        let original = format!("sha256:{}", "d".repeat(64));
        let replacement = format!("sha256:{}", "e".repeat(64));
        let object = |id: &str| format!(r#"{{"Id":"{id}","Os":"linux","Architecture":"amd64"}}"#);
        fixture.write("inspect-stdout", object(&original));
        fixture.write("inspect-next-stdout", object(&replacement));
        fixture.write("podman/inspect-stdout", format!(r#"[{{"Id":"{}","Os":"linux","Architecture":"amd64","ManifestType":"application/vnd.oci.image.manifest.v1+json"}}]"#, "e".repeat(64)));
        fixture.write("run-expected-image", &original);
        if change == "remove" {
            fixture.write("run-removed-image", &original);
        }
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["--image", "app:tag", "php"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if change == "remove" { 125 } else { 0 })
        );
        let calls = fixture.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0][0], "version");
        assert_eq!(calls[1][0], "image");
        assert_eq!(calls[2][0], "run");
        let index = calls[2]
            .iter()
            .position(|arg| arg == "--entrypoint")
            .unwrap();
        assert_eq!(calls[2][index + 3], original.as_str());
        assert!(calls[2].iter().any(|arg| arg == "--pull=never"));
        assert!(fixture.engine_calls("podman").is_empty());
        assert!(!fixture.root.join("unexpected-command").exists());
        if change == "remove" {
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("build --engine docker --context"), "{error}");
        }
    }
}
