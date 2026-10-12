mod support;
use support::Fixture;

#[test]
fn bare_image_identifier_is_not_a_named_build_destination() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("source/Dockerfile", "FROM scratch\n");
    fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .args(["build", "--context", "source", "--tag"])
        .arg("a".repeat(64))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(fixture.calls().is_empty());
    assert!(!fixture.root.join("cache").exists());
    assert!(!fixture.root.join("config").exists());
}

#[test]
fn uppercase_registry_host_is_preserved_but_repository_paths_remain_lowercase() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("source/Dockerfile", "FROM scratch\n");
    fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
    let output = fixture
        .larainit()
        .env("PATH", &bin)
        .args(["build", "--context", "source", "--tag", "REGISTRY/repo:DEV"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    let build = calls.iter().find(|call| call[0] == "build").unwrap();
    let index = build.iter().position(|arg| arg == "--tag").unwrap();
    assert_eq!(build[index + 1], "REGISTRY/repo:DEV");
    let count = calls.len();
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .args([
            "build",
            "--context",
            "source",
            "--tag",
            "registry/UPPER/repo",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fixture.calls().len(), count);
}

#[test]
fn invalid_build_inputs_fail_before_any_engine_or_state_change() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("source/Dockerfile", "FROM scratch\n");
    fixture.write("outside", "FROM scratch\n");
    for (args, status) in [
        (vec!["--context", "https://example.com/source"], 2),
        (vec!["--context", "git@example.com:repo"], 2),
        (vec!["--context", "ssh://user@example.com/repo"], 2),
        (vec!["--context", "-"], 2),
        (vec!["--context", "source/.."], 2),
        (vec!["--context", "absent"], 1),
        (vec!["--context", "outside"], 1),
        (vec!["--context", "source", "--file", "../outside"], 2),
        (vec!["--context", "source", "--file", "missing"], 1),
        (vec!["--context", "source", "--file", "-"], 2),
        (
            vec![
                "--context",
                "source",
                "--file",
                "https://example.com/Dockerfile",
            ],
            2,
        ),
        (vec!["--context", "source", "--tag", "repo@sha256:abc"], 2),
        (
            vec!["--context", "source", "--tag", "registry/UPPER/repo:tag"],
            2,
        ),
        (vec!["--context", "source", "--tag", "repo:bad tag"], 2),
        (vec!["--context", "source", "--tag=-option"], 2),
    ] {
        let output = fixture
            .larainit()
            .env("PATH", &bin)
            .arg("build")
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(status),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(fixture.calls().is_empty());
        assert!(!fixture.root.join("cache").exists());
        assert!(!fixture.root.join("config").exists());
    }
}

#[test]
fn build_verifies_own_iid_and_tag_without_changing_image_preferences() {
    for mode in [
        "success",
        "missing",
        "malformed",
        "mismatch",
        "tag-mismatch",
        "tag-removal",
        "image-removal",
        "engine-failure",
        "iid-symlink",
        "iid-hardlink",
        "iid-fifo",
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.install_engine("podman");
        fixture.write("source/Dockerfile", "FROM scratch\n");
        fixture.write(
            "config/larainit/config.toml",
            "engine = 'docker'\nimage = 'saved:tag'\n",
        );
        let id = format!("sha256:{}", "a".repeat(64));
        fixture.write(
            "build-mode",
            if mode == "engine-failure" {
                "exit=37"
            } else if mode.starts_with("iid-") {
                mode
            } else {
                ""
            },
        );
        if mode != "missing" {
            fixture.write("build-iid", if mode == "malformed" { "bad" } else { &id });
        }
        if mode == "mismatch" {
            fixture.write(
                "inspect-stdout",
                format!(
                    r#"{{"Id":"sha256:{}","Os":"linux","Architecture":"amd64"}}"#,
                    "b".repeat(64)
                ),
            );
        }
        if mode == "tag-mismatch" {
            fixture.write("inspect-tag", "custom:tag");
            fixture.write(
                "inspect-tag-stdout",
                format!(
                    r#"{{"Id":"sha256:{}","Os":"linux","Architecture":"amd64"}}"#,
                    "b".repeat(64)
                ),
            );
        }
        if mode == "tag-removal" {
            fixture.write("inspect-missing-ref", "custom:tag");
        }
        if mode == "image-removal" {
            fixture.write("inspect-missing-ref", &id);
        }
        let before = std::fs::read(fixture.root.join("config/larainit/config.toml")).unwrap();
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args([
                "--image",
                "override:tag",
                "build",
                "--context",
                "source",
                "--tag",
                "custom:tag",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(match mode {
                "success" => 0,
                "engine-failure" => 37,
                _ => 1,
            }),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = fixture.calls();
        let build = calls
            .iter()
            .find(|call| call[0] == "build")
            .expect("build launched");
        let tag = build.iter().position(|value| value == "--tag").unwrap();
        assert_eq!(build[tag + 1], "custom:tag");
        let iid = build.iter().position(|value| value == "--iidfile").unwrap();
        let iid_path = std::path::Path::new(&build[iid + 1]);
        assert!(!iid_path.starts_with(fixture.root.join("source")));
        assert!(!iid_path.exists());
        assert!(!calls.iter().any(|call| matches!(
            call[0].to_str(),
            Some("run" | "pull" | "rm" | "kill" | "stop" | "prune")
        )));
        assert_eq!(
            std::fs::read(fixture.root.join("config/larainit/config.toml")).unwrap(),
            before
        );
        assert!(!fixture.root.join("cache").exists());
        assert!(fixture.engine_calls("podman").is_empty());
        if mode == "success" {
            assert!(String::from_utf8_lossy(&output.stdout).contains("verified build"));
        } else {
            assert!(!String::from_utf8_lossy(&output.stdout).contains("verified build"));
        }
    }
}

#[test]
fn native_paths_default_destination_and_canonical_file_containment() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    let context = fixture
        .root
        .join(std::ffi::OsString::from_vec(b"source ' , \xff".to_vec()));
    std::fs::create_dir(&context).unwrap();
    std::fs::write(context.join("Dockerfile"), "FROM scratch\n").unwrap();
    symlink("Dockerfile", context.join("inside")).unwrap();
    fixture.write("outside", "FROM scratch\n");
    symlink(fixture.root.join("outside"), context.join("escape")).unwrap();
    fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
    let alias = fixture.root.join("alias");
    symlink(&context, &alias).unwrap();
    for file in [
        None,
        Some(context.join("Dockerfile")),
        Some("inside".into()),
    ] {
        let mut command = fixture.larainit();
        command
            .env("PATH", &bin)
            .args(["--image", "not-the-destination:tag", "build", "--context"])
            .arg(&alias);
        if let Some(file) = file {
            command.arg("--file").arg(file);
        }
        assert!(command.output().unwrap().status.success());
        let calls = fixture.calls();
        let build = calls.iter().rev().find(|call| call[0] == "build").unwrap();
        assert_eq!(build.last().unwrap(), context.as_os_str());
        let index = build.iter().position(|value| value == "--file").unwrap();
        assert_eq!(build[index + 1], context.join("Dockerfile").as_os_str());
        let index = build.iter().position(|value| value == "--tag").unwrap();
        assert_eq!(build[index + 1], "larainit:latest");
    }
    let count = fixture.calls().len();
    for file in [context.join("escape"), fixture.root.join("outside")] {
        let output = fixture
            .larainit()
            .env("PATH", &bin)
            .args(["build", "--context"])
            .arg(&context)
            .arg("--file")
            .arg(file)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(fixture.calls().len(), count);
    }
    let output = fixture
        .larainit()
        .env("PATH", &bin)
        .env("TMPDIR", &context)
        .args(["build", "--context"])
        .arg(&context)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fixture.calls().len(),
        count + 1,
        "scratch-in-context refusal must launch only readiness"
    );
}

#[test]
fn iid_replacement_during_inspection_never_reports_verified_success() {
    for mutation in [
        "replace-same",
        "replace-other",
        "edit-same",
        "edit-other",
        "remove",
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("source/Dockerfile", "FROM scratch\n");
        fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
        fixture.write("inspect-iid-mutation", mutation);
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["build", "--context", "source"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{mutation}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("build completed; verification failed")
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("verified build"));
        assert!(
            !fixture
                .calls()
                .iter()
                .any(|call| matches!(call[0].to_str(), Some("rm" | "prune" | "kill" | "stop")))
        );
    }
}

#[test]
fn build_local_engine_is_strict_and_wins_over_global_environment_and_saved_preferences() {
    for local in ["docker", "podman"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.install_engine("podman");
        fixture.write("source/Dockerfile", "FROM scratch\n");
        fixture.write(
            "config/larainit/config.toml",
            "engine='podman'\nimage='saved:tag'\n",
        );
        fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
        let before = std::fs::read(fixture.root.join("config/larainit/config.toml")).unwrap();
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .env("LARAINIT_ENGINE", "podman")
            .args([
                "--engine",
                "docker",
                "build",
                "--context",
                "source",
                "--engine",
                local,
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if local == "docker" { 0 } else { 1 })
        );
        if local == "docker" {
            assert!(fixture.calls().iter().any(|call| call[0] == "build"));
        } else {
            assert!(fixture.calls().is_empty());
        }
        assert!(
            fixture.engine_calls("podman").is_empty(),
            "uncertified builds must not initialize Podman"
        );
        assert_eq!(
            std::fs::read(fixture.root.join("config/larainit/config.toml")).unwrap(),
            before
        );
    }
}

#[test]
fn unreadable_context_file_and_nonregular_inputs_never_launch_an_engine() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("source/Dockerfile", "FROM scratch\n");
    // The root case is explicitly not evidence of unreadability: root can bypass mode bits.
    if nix::unistd::geteuid().is_root() {
        panic!("this permission regression requires a non-root test account, not a passing skip");
    }
    let context = fixture.root.join("source");
    for path in [&context, &context.join("Dockerfile")] {
        let permissions = std::fs::metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let output = fixture
            .larainit()
            .env("PATH", &bin)
            .args(["build", "--context", "source"])
            .output()
            .unwrap();
        std::fs::set_permissions(path, permissions).unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(fixture.calls().is_empty());
    }
    std::fs::create_dir(context.join("directory-file")).unwrap();
    nix::unistd::mkfifo(
        &context.join("fifo-file"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    for file in ["directory-file", "fifo-file"] {
        let start = std::time::Instant::now();
        let output = fixture
            .larainit()
            .env("PATH", &bin)
            .args(["build", "--context", "source", "--file", file])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "nonregular source blocked validation"
        );
        assert!(fixture.calls().is_empty());
    }
    assert!(!fixture.root.join("cache").exists());
    assert!(!fixture.root.join("config").exists());
}

#[test]
fn build_path_corpus_is_native_and_exact_with_option_delimiters() {
    for name in [
        "--",
        "a b",
        "日本語",
        "it's",
        "\"quoted\"",
        "a,b",
        "$HOME;*",
        "line\nbreak",
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        let context = fixture.root.join(name);
        std::fs::create_dir(&context).unwrap();
        std::fs::write(context.join("--"), "FROM scratch\n").unwrap();
        fixture.write("build-iid", format!("sha256:{}", "a".repeat(64)));
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["build", "--file=--", "--context"])
            .arg(&context)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = fixture.calls();
        let build = calls.iter().find(|call| call[0] == "build").unwrap();
        assert_eq!(build.len(), 9);
        assert_eq!(build[0], "build");
        assert_eq!(build[1], "--iidfile");
        assert_eq!(build[3], "--file");
        assert_eq!(build[4], context.join("--").as_os_str());
        assert_eq!(build[5], "--tag");
        assert_eq!(build[6], "larainit:latest");
        assert_eq!(build[7], "--");
        assert_eq!(build[8], context.as_os_str());
        assert_eq!(
            calls.len(),
            4,
            "one readiness, one build, two selected-store inspections"
        );
        assert!(!fixture.root.join("cache").exists());
        assert!(!fixture.root.join("config").exists());
    }
}
