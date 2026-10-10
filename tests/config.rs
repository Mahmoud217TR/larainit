mod support;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use support::Fixture;

fn configured(text: &str) -> (Fixture, std::path::PathBuf) {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fs::create_dir_all(fixture.root.join("config/larainit")).unwrap();
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
