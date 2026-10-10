#[path = "support/packages.rs"]
mod packages;
mod support;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use support::Fixture;

#[test]
fn cache_permissions_mounts_and_ephemeral_configuration_are_explicit() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    assert!(
        fixture
            .larainit()
            .env("PATH", &bin)
            .arg("composer")
            .output()
            .unwrap()
            .status
            .success()
    );
    let root = fixture.root.join("cache/larainit");
    for directory in [&root, &root.join("composer"), &root.join("npm")] {
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    fs::set_permissions(root.join("composer"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        fixture
            .larainit()
            .env("PATH", bin)
            .arg("npm")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::metadata(root.join("composer"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let calls = fixture.calls();
    let run = &calls[2];
    for value in [
        "HOME=/tmp",
        "COMPOSER_HOME=/tmp/composer",
        "COMPOSER_CACHE_DIR=/cache/composer",
        "npm_config_cache=/cache/npm",
    ] {
        assert!(run.contains(&value.into()));
    }
    assert_eq!(
        run.iter().filter(|argument| *argument == "--mount").count(),
        3
    );
    assert!(
        !run.iter().any(|argument| argument
            .to_str()
            .is_some_and(|value| value.contains("/opt/composer")
                || value.contains("auth.json")
                || value.contains(".npmrc")))
    );
}

#[test]
fn invalid_existing_cache_and_parent_permissions_are_never_bypassed_or_repaired() {
    for mode in ["file", "denied", "readonly"] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fs::create_dir_all(fixture.root.join("cache/larainit")).unwrap();
        let path = fixture.root.join("cache/larainit/composer");
        if mode == "file" {
            fs::write(&path, b"user file").unwrap();
        } else {
            fs::create_dir(&path).unwrap();
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if mode == "denied" { 0 } else { 0o500 }),
            )
            .unwrap();
        }
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .arg("php")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(path.to_str().unwrap()));
        assert!(!fixture.calls().iter().any(|call| call[0] == "run"));
        if mode == "file" {
            assert_eq!(fs::read(&path).unwrap(), b"user file");
        } else {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}

#[test]
#[ignore = "requires prepared real Docker and explicit package downloads; run --ignored --nocapture"]
fn fresh_locked_projects_reuse_actual_download_artifacts() {
    let fixture = Fixture::new();
    let first = fixture.root.join("first");
    let second = fixture.root.join("second");
    packages::manifests(&first);
    packages::manifests(&second);
    packages::run(&fixture, &first, packages::COMPOSER_INSTALL);
    packages::run(&fixture, &first, packages::NPM_INSTALL);
    packages::locks(&first, &second);
    assert!(!second.join("vendor").exists() && !second.join("node_modules").exists());
    let composer = fixture.root.join("cache/larainit/composer/files");
    let npm = fixture.root.join("cache/larainit/npm/_cacache/content-v2");
    let composer_before = packages::files(&composer);
    let npm_before = packages::files(&npm);
    assert!(!composer_before.is_empty() && !npm_before.is_empty());
    let output = packages::run(&fixture, &second, packages::COMPOSER_INSTALL);
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        log.contains("from cache") && log.contains("psr/log"),
        "no actual Composer cache hit:\n{log}"
    );
    let output = packages::run(&fixture, &second, packages::NPM_CI);
    assert!(String::from_utf8_lossy(&output.stdout).contains("added 1 package"));
    assert_eq!(packages::files(&composer), composer_before);
    assert_eq!(packages::files(&npm), npm_before);
    for path in composer_before.keys().chain(npm_before.keys()) {
        assert_eq!(
            fs::metadata(path).unwrap().uid(),
            nix::unistd::getuid().as_raw()
        );
    }
    assert!(
        second.join("vendor/psr/log").is_dir() && second.join("node_modules/is-number").is_dir()
    );
    let installer = packages::run(&fixture, &second, &["laravel", "--version"]);
    assert!(String::from_utf8_lossy(&installer.stdout).contains("5.32.0"));
    println!(
        "psr/log 3.0.2 Composer cache-hit excerpt: {}",
        log.lines()
            .filter(|line| line.contains("from cache") || line.contains("/cache/composer/files"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!(
        "is-number 7.0.0 installed offline in a fresh locked project; {} Composer and {} npm artifact files retained unchanged and owned by UID {}",
        composer_before.len(),
        npm_before.len(),
        nix::unistd::getuid()
    );
}
