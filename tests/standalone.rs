mod support;
use std::fs;
use std::process::Command;
use support::Fixture;

#[test]
fn copied_and_symlinked_native_binary_has_source_independent_local_assistance() {
    let fixture = Fixture::new();
    let bin = fixture.root.join("isolated bin");
    fs::create_dir(&bin).unwrap();
    let placed = bin.join("larainit");
    fs::copy(env!("CARGO_BIN_EXE_larainit"), &placed).unwrap();
    let link = fixture.root.join("linked-native");
    std::os::unix::fs::symlink(&placed, &link).unwrap();
    fixture.write("config.toml", "unknown='invalid configuration'");
    for binary in [&placed, &link] {
        for flag in ["--help", "--version"] {
            let output = Command::new(binary)
                .arg(flag)
                .current_dir(&fixture.root)
                .env("PATH", &bin)
                .env_remove("HOME")
                .env("XDG_CONFIG_HOME", "relative")
                .env("XDG_CACHE_HOME", "relative")
                .env("LARAINIT_ENGINE", "podman")
                .output()
                .unwrap();
            assert!(output.status.success());
            if flag == "--version" {
                assert_eq!(
                    output.stdout,
                    format!("larainit {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
                );
            }
            assert!(output.stderr.is_empty());
        }
    }
    assert!(!fixture.root.join("relative").exists());
    assert_eq!(fs::read_dir(bin).unwrap().count(), 1);
}

#[test]
#[ignore = "requires real prepared Docker; run cargo test --release --test standalone -- --ignored"]
fn placed_binary_uses_unrelated_symlinked_workspace_with_docker_only_path() {
    let fixture = Fixture::new();
    let bin = fixture.root.join("bin");
    fs::create_dir(&bin).unwrap();
    let placed = bin.join("larainit");
    fs::copy(env!("CARGO_BIN_EXE_larainit"), &placed).unwrap();
    let docker = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("docker"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    std::os::unix::fs::symlink(docker, bin.join("docker")).unwrap();
    let workspace = fixture.root.join("unrelated project 日本語");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("sentinel"), b"actual invocation workspace").unwrap();
    let link = fixture.root.join("linked project");
    std::os::unix::fs::symlink(&workspace, &link).unwrap();
    let configured = fixture.real_larainit();
    let mut command = Command::new(placed);
    command
        .envs(
            configured
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .args(configured.get_args())
        .env("PATH", bin)
        .env_remove("MOCK_ROOT")
        .current_dir(link)
        .args([
            "php",
            "-r",
            "echo file_get_contents('sentinel'); file_put_contents('native-write','host-owned');",
        ]);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"actual invocation workspace");
    assert_eq!(
        fs::read(workspace.join("native-write")).unwrap(),
        b"host-owned"
    );
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::metadata(workspace.join("native-write")).unwrap().uid(),
        nix::unistd::getuid().as_raw()
    );
}
