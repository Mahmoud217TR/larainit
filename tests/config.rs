mod support;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use support::Fixture;

fn configured(text: &str) -> (Fixture, std::path::PathBuf) {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("config/larainit/config.toml", text);
    (fixture, bin)
}

#[test]
fn both_settings_obey_cli_environment_file_default_precedence() {
    for (cli, env, file, expected) in [
        (
            Some("cli:tag"),
            Some("env:tag"),
            Some("file:tag"),
            "cli:tag",
        ),
        (None, Some("env:tag"), Some("file:tag"), "env:tag"),
        (None, None, Some("file:tag"), "file:tag"),
        (None, None, None, "larainit:latest"),
    ] {
        let text = file
            .map(|image| format!("image='{image}'"))
            .unwrap_or_default();
        let (fixture, bin) = configured(&text);
        let mut command = fixture.larainit();
        command.env("PATH", bin);
        if let Some(image) = cli {
            command.args(["--image", image]);
        }
        if let Some(image) = env {
            command.env("LARAINIT_IMAGE", image);
        }
        assert!(command.arg("php").output().unwrap().status.success());
        assert_eq!(fixture.calls()[1][5], expected);
    }
    let (fixture, bin) = configured("engine='podman'");
    assert!(
        fixture
            .larainit()
            .env("PATH", &bin)
            .env("LARAINIT_ENGINE", "docker")
            .arg("php")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        fixture
            .larainit()
            .env("PATH", &bin)
            .env("LARAINIT_ENGINE", "podman")
            .args(["--engine", "auto", "php"])
            .output()
            .unwrap()
            .status
            .success()
    );
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
}

#[test]
fn schema_read_errors_and_effective_invalid_values_do_not_fall_back() {
    for text in [
        "unknown='secret value'",
        "engine=42",
        "image=[]",
        "engine='",
    ] {
        let (fixture, bin) = configured(text);
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["--engine", "docker", "--image", "cli:tag", "php"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(fixture.calls().is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret value"));
    }
    let (fixture, bin) = configured("engine='auto'");
    fs::set_permissions(
        fixture.root.join("config/larainit/config.toml"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .args(["--engine", "docker", "php"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(fixture.calls().is_empty());
    for (key, value) in [
        ("LARAINIT_ENGINE", ""),
        ("LARAINIT_ENGINE", "Docker"),
        ("LARAINIT_ENGINE", "podman"),
        ("LARAINIT_IMAGE", ""),
        ("LARAINIT_IMAGE", "-option"),
        ("LARAINIT_IMAGE", "bad\nvalue"),
    ] {
        let (fixture, bin) = configured("engine='auto'\nimage='file:tag'");
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .env(key, value)
            .arg("php")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(fixture.calls().is_empty());
    }
}

#[test]
fn xdg_and_home_resolution_writes_only_execution_caches() {
    for base in ["", "relative"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        assert!(
            fixture
                .larainit()
                .env("PATH", bin)
                .env("XDG_CONFIG_HOME", base)
                .env("XDG_CACHE_HOME", base)
                .arg("php")
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(fixture.root.join(".cache/larainit/composer").is_dir());
        assert!(!fixture.root.join(".config").exists());
    }
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    assert!(
        fixture
            .larainit()
            .env("PATH", &bin)
            .env_remove("HOME")
            .arg("php")
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .env_remove("HOME")
        .env("XDG_CONFIG_HOME", "relative")
        .arg("php")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!fixture.root.join("config").exists());
}

#[test]
fn invalid_saved_values_are_rejected_even_when_overridden() {
    for text in [
        "engine='unknown'",
        "engine=''",
        "image=''",
        "image='-option'",
    ] {
        let (fixture, bin) = configured(text);
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["--engine", "docker", "--image", "valid:tag", "php"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{text}");
        assert!(fixture.calls().is_empty());
        assert!(!fixture.root.join("cache").exists());
    }
}

#[test]
fn unimplemented_explicit_podman_never_selects_or_executes_docker() {
    for command in ["php", "doctor"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["--engine", "podman", command])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        if command == "doctor" {
            assert_eq!(fixture.calls().len(), 1);
            assert_eq!(
                fixture.calls()[0][0],
                "version",
                "unused Docker observation must not execute or inspect images"
            );
        } else {
            assert!(fixture.calls().is_empty());
        }
        assert!(!fixture.root.join("cache").exists());
    }
}

#[test]
fn engine_reads_report_precedence_without_writes() {
    for (cli, environment, saved, expected, source) in [
        (
            Some("docker"),
            Some("podman"),
            Some("auto"),
            "docker",
            "CLI",
        ),
        (
            None,
            Some("docker"),
            Some("podman"),
            "docker",
            "environment LARAINIT_ENGINE",
        ),
        (None, None, Some("docker"), "docker", "config"),
        (None, None, None, "auto", "default"),
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        let path = fixture.root.join("config/larainit/config.toml");
        if let Some(saved) = saved {
            fixture.write(
                "config/larainit/config.toml",
                format!("engine='{saved}'\nimage='custom:tag'"),
            );
        }
        let before = fs::read(&path).ok();
        let mut command = fixture.larainit();
        command.env("PATH", bin);
        if let Some(cli) = cli {
            command.args(["--engine", cli]);
        }
        if let Some(environment) = environment {
            command.env("LARAINIT_ENGINE", environment);
        }
        let output = command.arg("engine").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&format!("effective preference: {expected}")),
            "{stdout}"
        );
        assert!(stdout.contains(source), "{stdout}");
        assert_eq!(fs::read(path).ok(), before);
        if saved.is_none() {
            assert!(!fixture.root.join("config").exists());
        }
        assert!(!fixture.root.join("cache").exists());
    }
}

#[test]
fn setters_save_the_argument_and_preserve_recognized_image() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    for (preference, status) in [("docker", 0), ("podman", 1), ("auto", 0)] {
        let calls_before = fixture.calls().len();
        let output = fixture
            .larainit()
            .env("PATH", &bin)
            .env("LARAINIT_ENGINE", "docker")
            .args(["--engine", "auto", "engine", preference])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(status));
        let text = fs::read_to_string(fixture.root.join("config/larainit/config.toml")).unwrap();
        let raw: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(raw["engine"].as_str(), Some(preference));
        assert!(raw.get("image").is_none());
        if preference == "podman" {
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("preference saved; execution unavailable")
            );
            assert_eq!(
                fixture.calls().len(),
                calls_before,
                "saved Podman choice probed Docker"
            );
        }
    }
    fixture.write(
        "config/larainit/config.toml",
        "image='registry.example/日本語:tag'\nengine='auto'",
    );
    for _ in 0..2 {
        assert!(
            fixture
                .larainit()
                .env("PATH", &bin)
                .args(["--image", "transient:tag", "engine", "docker"])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let raw: toml::Value = toml::from_str(
        &fs::read_to_string(fixture.root.join("config/larainit/config.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(raw["image"].as_str(), Some("registry.example/日本語:tag"));
    assert!(!fixture.root.join("cache").exists());
}

#[test]
fn durable_save_survives_unavailable_docker_and_readiness_failure() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("version-mode", "exit=1");
    fixture.write("version-stderr", "permission denied");
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .args(["engine", "docker"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("preference saved; execution unavailable")
    );
    let raw: toml::Value = toml::from_str(
        &fs::read_to_string(fixture.root.join("config/larainit/config.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(raw["engine"].as_str(), Some("docker"));
    assert_eq!(fixture.calls().len(), 1);
    assert!(!fixture.root.join("cache").exists());
}
