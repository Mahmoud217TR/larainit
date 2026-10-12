mod support;

use std::fs;
use support::Fixture;

#[test]
fn remote_indicators_including_present_empty_values_never_probe() {
    for key in ["CONTAINER_HOST", "CONTAINER_CONNECTION"] {
        for value in ["", "ssh://private-token@remote.example", "named-connection"] {
            let fixture = Fixture::new();
            let bin = fixture.install_docker();
            fixture.install_engine("podman");
            fixture.install_engine("sqlite3");
            let output = fixture
                .larainit()
                .env("PATH", bin)
                .env_remove("CONTAINER_HOST")
                .env_remove("CONTAINER_CONNECTION")
                .env(key, value)
                .args(["--engine", "podman", "engine"])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("remote") && stderr.contains(key),
                "{stderr}"
            );
            assert!(!stderr.contains("private-token"));
            assert!(fixture.calls().is_empty());
            assert!(fixture.engine_calls("podman").is_empty());
            assert!(fixture.engine_calls("sqlite3").is_empty());
            assert!(!fixture.root.join("config").exists());
            assert!(!fixture.root.join("cache").exists());
        }
    }
}

#[test]
fn executable_database_and_runtime_markers_are_not_certification() {
    // No engine command is authorized by executable/database existence alone.
    for state in [
        "cold",
        "database",
        "markers",
        "wal",
        "configuration",
        "stored-custom-paths",
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.install_engine("podman");
        fixture.install_engine("sqlite3");
        match state {
            "cold" => {}
            "database" | "markers" | "wal" => {
                fixture.write(
                    ".local/share/containers/storage/db.sql",
                    b"SQLite format 3\0not verified",
                );
                if state != "database" {
                    fixture.write("runtime/libpod/tmp/alive", "present");
                }
                if state == "wal" {
                    fixture.write(
                        ".local/share/containers/storage/db.sql-wal",
                        "unverified WAL",
                    );
                }
            }
            "configuration" => {
                fixture.write(
                    "config/containers/containers.conf.d/custom.conf",
                    "[engine]\nremote=true",
                );
            }
            "stored-custom-paths" => {
                fixture.write(
                    ".local/share/containers/storage/db.sql",
                    "unverified stored custom paths",
                );
            }
            _ => unreachable!(),
        }
        let path = fixture.root.join(".local/share/containers/storage/db.sql");
        let before = fs::read(&path).ok();
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .env("XDG_RUNTIME_DIR", fixture.root.join("runtime"))
            .env_remove("CONTAINER_HOST")
            .env_remove("CONTAINER_CONNECTION")
            .args(["--engine", "podman", "engine"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{state}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("not checked"), "{stderr}");
        if nix::unistd::geteuid().is_root() {
            assert!(stderr.contains("rootful"), "{stderr}");
        } else {
            assert!(stderr.contains("certified"), "{stderr}");
        }
        assert!(fixture.engine_calls("podman").is_empty());
        assert!(fixture.engine_calls("sqlite3").is_empty());
        assert!(fixture.calls().is_empty());
        assert_eq!(fs::read(path).ok(), before);
        assert!(!fixture.root.join("cache").exists());
    }
}

#[test]
fn unused_remote_podman_indicators_do_not_block_healthy_docker() {
    for preference in ["auto", "docker"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.install_engine("podman");
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .env("CONTAINER_HOST", "")
            .args(["--engine", preference, "engine"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("selected engine: docker"));
        assert!(stdout.contains("selection reason:"));
        assert_eq!(fixture.calls().len(), 1);
        assert!(fixture.engine_calls("podman").is_empty());
    }
}
