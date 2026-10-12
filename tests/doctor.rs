mod support;
use support::Fixture;

#[test]
fn diagnostic_precedence_reasons_and_strict_failures_do_not_mutate_saved_settings() {
    for (cli, environment, effective, source, status) in [
        (None, None, "podman", "config", 1),
        (
            None,
            Some("docker"),
            "docker",
            "environment LARAINIT_ENGINE",
            0,
        ),
        (Some("auto"), Some("podman"), "auto", "CLI", 0),
        (Some("podman"), Some("docker"), "podman", "CLI", 1),
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.install_engine("podman");
        let saved = "engine='podman'\nimage='saved:tag'\n";
        fixture.write("config/larainit/config.toml", saved);
        let mut command = fixture.larainit();
        command.env("PATH", bin);
        if let Some(value) = cli {
            command.args(["--engine", value]);
        }
        if let Some(value) = environment {
            command.env("LARAINIT_ENGINE", value);
        }
        let output = command.arg("doctor").output().unwrap();
        assert_eq!(output.status.code(), Some(status));
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(report.contains("saved engine: podman"), "{report}");
        assert!(
            report.contains(&format!("effective engine: {effective} (source: {source}")),
            "{report}"
        );
        assert!(report.contains("saved:tag"), "{report}");
        if status == 0 {
            assert!(
                report.contains(if effective == "auto" {
                    "auto: healthy Docker preferred"
                } else {
                    "explicit preference; no fallback"
                }),
                "{report}"
            );
            assert!(report.contains("[success] local image"));
            assert_eq!(fixture.calls().len(), 2);
        } else {
            assert!(report.contains("[failure] selected engine"));
            assert!(report.contains("[not checked] local image"));
            assert_eq!(
                fixture.calls().len(),
                1,
                "Docker candidate is informational, never a strict fallback"
            );
        }
        assert!(fixture.engine_calls("podman").is_empty());
        assert_eq!(
            std::fs::read(fixture.root.join("config/larainit/config.toml")).unwrap(),
            saved.as_bytes()
        );
        assert!(!fixture.root.join("cache").exists());
    }
}

#[test]
fn healthy_report_is_complete_informational_absence_is_not_failure() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .env("LARAINIT_IMAGE", "environment:tag")
        .args(["--engine", "docker", "doctor"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).unwrap();
    for field in [
        "linux",
        "x86_64",
        "0.2.0",
        "verification",
        "Docker executable",
        "Docker server",
        "environment:tag",
        "LARAINIT_IMAGE",
        "CLI",
        "config.toml",
        "workspace",
        "Composer cache",
        "npm cache",
        "absent/preparable",
        "not present",
        "bookkeeping",
        "socket activation",
        "Podman",
        "selected engine",
    ] {
        assert!(report.contains(field), "missing {field}: {report}");
    }
    assert!(output.stderr.is_empty());
    assert!(!fixture.root.join("cache").exists());
    assert!(!fixture.root.join("config").exists());
}

#[test]
fn unused_podman_failure_is_informational_and_never_launches_a_probe() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.install_engine("podman");
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .env("CONTAINER_HOST", "")
        .arg("doctor")
        .output()
        .unwrap();
    assert!(output.status.success());
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("[info] Podman"), "{report}");
    assert!(report.contains("selected engine: docker"), "{report}");
    assert!(fixture.engine_calls("podman").is_empty());
    assert_eq!(fixture.calls().len(), 2);
    assert!(!fixture.root.join("cache").exists());
}

#[test]
fn failures_continue_independent_checks_and_label_dependents_not_checked() {
    for failure in ["server", "image", "config", "cache"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        match failure {
            "server" => {
                fixture.write("version-mode", "exit=1");
                fixture.write("version-stderr", "permission denied");
            }
            "image" => {
                fixture.write("inspect-mode", "exit=1");
                fixture.write("inspect-stderr", "No such image: larainit:latest");
            }
            "config" => {
                fixture.write("config/larainit/config.toml", "unknown='private value'");
            }
            "cache" => fixture.write("cache", "not a directory"),
            _ => unreachable!(),
        }
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .arg("doctor")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let report = String::from_utf8(output.stdout).unwrap();
        assert!(
            report.contains("workspace")
                && report.contains("Composer cache")
                && report.contains("Docker executable")
        );
        assert!(!report.contains("private value"));
        if failure == "server" || failure == "config" {
            assert!(report.contains("not checked"));
        }
        if failure == "config" {
            assert!(fixture.calls().is_empty());
        }
    }
}
