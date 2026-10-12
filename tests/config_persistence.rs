mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::{Duration, Instant};
use support::Fixture;

#[test]
fn saves_are_private_and_cooperating_writers_preserve_the_image() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    assert!(
        fixture
            .larainit()
            .env("PATH", &bin)
            .args(["engine", "docker"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let dir = fixture.root.join("config/larainit");
    let path = dir.join("config.toml");
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fixture.write(
        "config/larainit/config.toml",
        "engine='docker'\nimage='preserved:tag'",
    );
    let mut children = Vec::new();
    for preference in ["auto", "docker", "auto", "docker"] {
        children.push(
            fixture
                .larainit()
                .env("PATH", &bin)
                .args(["engine", preference])
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let raw: toml::Value = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(raw["image"].as_str(), Some("preserved:tag"));
    assert_eq!(fs::read_dir(dir).unwrap().count(), 1);
}

#[test]
fn unsafe_targets_and_ancestors_are_never_repaired_or_overwritten() {
    for state in [
        "symlink",
        "hardlink",
        "fifo",
        "writable-file",
        "writable-parent",
        "linked-parent",
        "malformed",
        "unknown",
        "unreadable",
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write(
            "config/larainit/config.toml",
            "engine='docker'\nimage='old:tag'",
        );
        let dir = fixture.root.join("config/larainit");
        let path = dir.join("config.toml");
        let old = fs::read(&path).unwrap();
        fixture.write("unrelated", &old);
        match state {
            "symlink" => {
                fs::remove_file(&path).unwrap();
                symlink(fixture.root.join("unrelated"), &path).unwrap();
            }
            "hardlink" => {
                fs::remove_file(&path).unwrap();
                fs::hard_link(fixture.root.join("unrelated"), &path).unwrap();
            }
            "fifo" => {
                fs::remove_file(&path).unwrap();
                nix::unistd::mkfifo(
                    &path,
                    nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
                )
                .unwrap();
            }
            "writable-file" => {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap()
            }
            "writable-parent" => {
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap()
            }
            "linked-parent" => {
                fs::rename(&dir, fixture.root.join("moved")).unwrap();
                symlink(fixture.root.join("moved"), &dir).unwrap();
            }
            "malformed" => fs::write(&path, "engine='").unwrap(),
            "unknown" => fs::write(&path, "unknown='secret'").unwrap(),
            "unreadable" => fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap(),
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&path).unwrap();
        let bytes_before = if before.is_file() || before.file_type().is_symlink() {
            fs::read(&path).ok()
        } else {
            None
        };
        let start = Instant::now();
        let output = fixture
            .larainit()
            .env("PATH", &bin)
            .args(["engine", "auto"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{state}");
        assert!(start.elapsed() < Duration::from_secs(3), "{state}");
        assert_eq!(
            fs::symlink_metadata(&path).unwrap().permissions().mode(),
            before.permissions().mode()
        );
        assert_eq!(fs::read(fixture.root.join("unrelated")).unwrap(), old);
        if before.is_file() || before.file_type().is_symlink() {
            assert_eq!(fs::read(&path).ok(), bytes_before, "{state}");
        }
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .arg("engine")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "unsafe read: {state}");
        assert!(fixture.calls().is_empty(), "{state}");
    }
}

#[test]
fn lock_wait_is_cancellable_and_never_commits() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("config/larainit/config.toml", "engine='docker'");
    let dir = fixture.root.join("config/larainit");
    let lock = nix::fcntl::Flock::lock(
        fs::File::open(&dir).unwrap(),
        nix::fcntl::FlockArg::LockExclusiveNonblock,
    )
    .unwrap();
    let old = fs::read(dir.join("config.toml")).unwrap();
    let mut child = fixture
        .larainit()
        .env("PATH", bin)
        .args(["engine", "auto"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        child.try_wait().unwrap().is_none(),
        "setter did not wait for the stable directory lock"
    );
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(143));
    drop(lock);
    assert_eq!(fs::read(dir.join("config.toml")).unwrap(), old);
    assert!(fixture.calls().is_empty());
}
