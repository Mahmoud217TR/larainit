mod support;
use std::fs;
use support::Fixture;

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
    ] {
        assert!(report.contains(field), "missing {field}: {report}");
    }
    assert!(output.stderr.is_empty());
    assert!(!fixture.root.join("cache").exists());
    assert!(!fixture.root.join("config").exists());
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
                fs::create_dir_all(fixture.root.join("config/larainit")).unwrap();
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
