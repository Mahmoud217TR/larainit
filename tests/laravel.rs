mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::Command;
use support::Fixture;

fn docker_config() -> PathBuf {
    std::env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                std::env::var_os("HOME").expect("Docker config needs HOME or DOCKER_CONFIG"),
            )
            .join(".docker")
        })
}

fn containers() -> BTreeSet<String> {
    let output = Command::new("docker")
        .args([
            "ps",
            "--all",
            "--filter",
            "name=^/larainit-",
            "--format",
            "{{.ID}}",
        ])
        .env("DOCKER_CLI_HOOKS", "false")
        .env("DOCKER_CLI_HINTS", "false")
        .output()
        .expect("real Docker CLI is required");
    assert!(
        output.status.success(),
        "real Docker container observation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn native(fixture: &Fixture, image: &str) -> Command {
    let mut command = fixture.larainit();
    // Keep the operator's effective Docker context despite isolated tool HOME/config/cache paths.
    command
        .env_remove("MOCK_ROOT")
        .env("DOCKER_CONFIG", docker_config())
        .args(["--engine", "docker", "--image", image]);
    command
}

#[test]
#[ignore = "requires an explicitly prepared real Docker image; run cargo test --test laravel -- --ignored --nocapture"]
fn actual_installer_creates_host_writable_project_and_removes_container() {
    let os = fs::read_to_string("/etc/os-release").expect("recorded Ubuntu environment required");
    assert!(
        os.lines().any(|line| line == "ID=ubuntu"),
        "Ubuntu execution evidence is required, not inferred from compilation"
    );
    let image = std::env::var("LARAINIT_TEST_IMAGE").unwrap_or_else(|_| "larainit:latest".into());
    let fixture = Fixture::new();
    let before = containers();
    let output = native(&fixture, &image)
        .args([
            "new",
            "my-app",
            "--database=sqlite",
            "--phpunit",
            "--no-node",
            "--no-boost",
            "--no-interaction",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "actual Laravel Installer failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let project = fixture.root.join("my-app");
    assert!(project.join("artisan").is_file());
    let composer = fs::read_to_string(project.join("composer.json")).unwrap();
    assert!(composer.contains("laravel/framework"));
    let env = project.join(".env");
    assert_eq!(
        fs::metadata(&env).unwrap().uid(),
        nix::unistd::getuid().as_raw()
    );
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&env)
        .unwrap()
        .write_all(b"\n# host edit verification\n")
        .unwrap();
    let output = native(&fixture, &image)
        .current_dir(&project)
        .args(["php", "artisan", "--version"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Artisan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Laravel Framework"));
    for arguments in [
        vec!["composer", "install", "--no-interaction"],
        vec!["npm", "install", "--no-audit", "--no-fund"],
        vec!["npm", "run", "build"],
    ] {
        let output = native(&fixture, &image)
            .current_dir(&project)
            .args(&arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(project.join("public/build/manifest.json").is_file());
    assert_eq!(
        containers().difference(&before).count(),
        0,
        "normally completed test containers remained"
    );
    println!(
        "Actual installer/Artisan, invoking UID {}, host edits, normal cleanup passed for local image {image}; actual Ubuntu release:\n{os}",
        nix::unistd::getuid().as_raw()
    );
}
