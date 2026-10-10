#[path = "support/packages.rs"]
mod packages;
mod support;
use std::fs;
use std::process::Stdio;
use support::Fixture;

#[test]
fn independent_invocations_share_caches_not_scratch_names_or_workspaces() {
    let first = Fixture::new();
    let second = Fixture::new();
    let bin_first = first.install_docker();
    let bin_second = second.install_docker();
    let shared = first.root.join("shared-cache");
    let a = first
        .larainit()
        .env("PATH", bin_first)
        .env("XDG_CACHE_HOME", &shared)
        .arg("composer")
        .spawn()
        .unwrap();
    let b = second
        .larainit()
        .env("PATH", bin_second)
        .env("XDG_CACHE_HOME", &shared)
        .arg("npm")
        .spawn()
        .unwrap();
    assert!(a.wait_with_output().unwrap().status.success());
    assert!(b.wait_with_output().unwrap().status.success());
    let a = first.calls();
    let b = second.calls();
    let name = |run: &[std::ffi::OsString]| {
        run[run.iter().position(|arg| arg == "--name").unwrap() + 1].clone()
    };
    assert_ne!(name(&a[2]), name(&b[2]));
    for calls in [a, b] {
        assert!(calls[2].iter().any(|value| {
            value
                .to_str()
                .is_some_and(|value| value.contains(shared.to_str().unwrap()))
        }));
    }
}

#[test]
#[ignore = "requires real Docker and explicit downloads; run --ignored --test-threads=1 --nocapture"]
fn actual_simultaneous_projects_use_package_manager_cache_coordination_only() {
    let fixture = Fixture::new();
    let seed = fixture.root.join("seed");
    packages::manifests(&seed);
    packages::run(&fixture, &seed, packages::COMPOSER_INSTALL);
    packages::run(&fixture, &seed, packages::NPM_INSTALL);
    let projects = [
        fixture.root.join("project-a"),
        fixture.root.join("project-b"),
    ];
    for (index, project) in projects.iter().enumerate() {
        packages::manifests(project);
        packages::locks(&seed, project);
        fs::write(project.join("sentinel"), index.to_string()).unwrap();
    }
    let shared = fixture.root.join("simultaneous-cache");
    let runtime = fixture.root.join("runtime");
    fs::create_dir(&runtime).unwrap();
    let mut children = Vec::new();
    let start = std::time::Instant::now();
    for project in &projects {
        // All four native clients are launched before waiting; distinct projects and package paths.
        for args in [packages::COMPOSER_INSTALL, packages::NPM_INSTALL] {
            let child = fixture
                .real_larainit()
                .current_dir(project)
                .env("XDG_CACHE_HOME", &shared)
                .env("TMPDIR", &runtime)
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            children.push((project.clone(), args[0], child));
        }
    }
    for (project, manager, child) in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{manager} in {} failed: {}",
            project.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for (index, project) in projects.iter().enumerate() {
        assert_eq!(
            fs::read_to_string(project.join("sentinel")).unwrap(),
            index.to_string()
        );
        assert!(
            project.join("vendor/psr/log").is_dir()
                && project.join("node_modules/is-number").is_dir()
        );
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(project.join("vendor/autoload.php"))
                .unwrap()
                .uid(),
            nix::unistd::getuid().as_raw()
        );
    }
    assert_eq!(fs::read_dir(runtime).unwrap().count(), 0);
    assert!(!packages::files(&shared.join("larainit/composer/files")).is_empty());
    assert!(!packages::files(&shared.join("larainit/npm/_cacache/content-v2")).is_empty());
    println!(
        "Four real native Composer/npm invocations spawned before waiting across two independent projects, all status 0 in {:.3}s; shared download caches and separated host-owned installs",
        start.elapsed().as_secs_f64()
    );
    for mode in ["file", "permission", "readonly"] {
        let denied = fixture.root.join(format!("denied-{mode}"));
        if mode == "file" {
            fs::write(&denied, b"existing file").unwrap();
        } else if mode == "permission" {
            use std::os::unix::fs::PermissionsExt;
            fs::create_dir(&denied).unwrap();
            fs::set_permissions(&denied, fs::Permissions::from_mode(0o500)).unwrap();
        }
        let target = if mode == "readonly" {
            std::path::Path::new("/sys").to_owned()
        } else {
            denied.clone()
        };
        let output = fixture
            .real_larainit()
            .env("XDG_CACHE_HOME", &target)
            .arg("php")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(target.to_str().unwrap()));
        if mode == "permission" {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&denied).unwrap().permissions().mode() & 0o777,
                0o500
            );
            fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
        }
        if mode == "file" {
            assert_eq!(fs::read(&denied).unwrap(), b"existing file");
        }
    }
}
