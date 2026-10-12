#[path = "support/packages.rs"]
mod packages;
mod support;

use std::{collections::BTreeSet, fs, os::unix::fs::MetadataExt, process::Command};
use support::Fixture;

fn prepared_fixture() -> (Fixture, String) {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        panic!("rootless profile requires Linux x86_64");
    }
    assert!(
        !nix::unistd::geteuid().is_root(),
        "rootless evidence requires a non-root account"
    );
    let image = std::env::var("LARAINIT_TEST_PODMAN_IMAGE").expect(
        "explicitly activated Podman test requires LARAINIT_TEST_PODMAN_IMAGE; no passing skip",
    );
    assert!(!image.trim().is_empty() && !image.starts_with('-'));
    let fixture = Fixture::new();
    // Never invoke Podman directly before Larainit's safe certified-profile gate passes.
    // Supplying an image never bypasses the production prepared-profile checks.
    let ready = native(&fixture, &image).arg("engine").output().unwrap();
    assert!(
        ready.status.success(),
        "certified prepared profile is required before any workload or store snapshot: {}",
        String::from_utf8_lossy(&ready.stderr)
    );
    (fixture, image)
}

fn native(fixture: &Fixture, image: &str) -> Command {
    // Preserve the authorized account's HOME/XDG config and engine runtime settings.
    // Only downloads/workspace are isolated; changing HOME would select a different Podman profile.
    let mut command = Command::new(env!("CARGO_BIN_EXE_larainit"));
    command
        .current_dir(&fixture.root)
        .env_remove("MOCK_ROOT")
        .env("XDG_CACHE_HOME", fixture.root.join("cache"))
        .args(["--engine", "podman", "--image", image]);
    command
}

fn wait_owned_wrapper(
    child: &mut std::process::Child,
    limit: std::time::Duration,
) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = child.kill();
    let reap_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < reap_deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!(
        "test-owned wrapper exceeded its deadline; forced wrapper termination is not engine disappearance evidence; inspect invocation manually"
    );
}

#[test]
fn owned_wrapper_deadline_forces_reap_but_never_counts_timeout_as_success() {
    use std::time::{Duration, Instant};
    let mut exited = Command::new("/bin/sh")
        .args(["-c", "exit 42"])
        .spawn()
        .unwrap();
    assert_eq!(
        wait_owned_wrapper(&mut exited, Duration::from_secs(2)).code(),
        Some(42)
    );
    let mut stuck = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    let start = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_owned_wrapper(&mut stuck, Duration::ZERO)
    }));
    assert!(result.is_err(), "timeout returned a passing process status");
    assert!(
        stuck.try_wait().unwrap().is_some(),
        "test-owned child was not reaped"
    );
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
#[ignore = "requires authorized certified prepared rootless profile/image; run --ignored --test-threads=1"]
fn certified_rootless_native_arguments_streams_eof_statuses_and_signals() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use std::{
        io::Write,
        process::Stdio,
        time::{Duration, Instant},
    };
    let (fixture, image) = prepared_fixture();
    let workspace = fixture.root.join("project a,b\" 日本語:");
    fs::create_dir(&workspace).unwrap();
    fs::write(
        workspace.join("args.php"),
        b"<?php echo json_encode(array_slice($argv, 1));",
    )
    .unwrap();
    let tail = [
        "",
        "a b",
        "日本語",
        "it's",
        "\"quoted\"",
        "$HOME;*",
        "--",
        "-n",
        "--x=a=b",
    ];
    let output = native(&fixture, &image)
        .current_dir(&workspace)
        .args(["php", "/workspace/args.php"])
        .args(tail)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native arguments: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&output.stdout).unwrap(),
        tail
    );
    let linked = fixture.root.join("linked workspace");
    std::os::unix::fs::symlink(&workspace, &linked).unwrap();
    let output = native(&fixture, &image)
        .current_dir(linked)
        .args(["php", "-r", "echo file_get_contents('args.php');"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, fs::read(workspace.join("args.php")).unwrap());
    let output = native(&fixture, &image)
        .args([
            "php",
            "-r",
            "fwrite(STDOUT,\"out\\n\"); fwrite(STDERR,\"err\\n\"); exit(42);",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(output.stdout, b"out\n");
    assert_eq!(output.stderr, b"err\n");
    for input in [b"input\n".as_slice(), b"".as_slice()] {
        let mut child = native(&fixture, &image)
            .args(["php", "-r", "echo stream_get_contents(STDIN);"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, input);
    }
    let output = native(&fixture, &image)
        .args(["php", "-r", "echo stream_get_contents(STDIN);"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success() && output.stdout.is_empty());
    let output = native(&fixture, &image)
        .arg("larainit-deliberately-absent-program")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(127));
    fixture.write("not-executable", "no execute permission");
    let output = native(&fixture, &image)
        .arg("/workspace/not-executable")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(126));

    for (index, requested) in [Signal::SIGINT, Signal::SIGTERM].into_iter().enumerate() {
        let path = fixture.root.join(format!("signal-{index}"));
        fs::create_dir(&path).unwrap();
        let runtime = path.join("runtime");
        fs::create_dir(&runtime).unwrap();
        let mut child = native(&fixture, &image)
            .current_dir(&path)
            .env("TMPDIR", &runtime)
            .args([
                "bash",
                "-c",
                "trap 'exit 42' INT TERM; touch ready; while :; do sleep 0.1; done",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(path.join("stderr.log")).unwrap())
            .spawn()
            .unwrap();
        let ready_deadline = Instant::now() + Duration::from_secs(30);
        let cid = loop {
            let directories: Vec<_> = fs::read_dir(&runtime)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            if path.join("ready").exists() && directories.len() == 1 {
                if let Ok(bytes) = fs::read(directories[0].join("cid")) {
                    let id = String::from_utf8(bytes).unwrap();
                    let id = id.strip_suffix('\n').unwrap_or(&id).to_owned();
                    assert!(
                        id.len() == 64
                            && id
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                        "full captured CID required"
                    );
                    break id;
                }
            }
            if Instant::now() >= ready_deadline {
                kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).unwrap();
                wait_owned_wrapper(&mut child, Duration::from_secs(18));
                panic!(
                    "signal fixture never became ready; cleanup requested only through its wrapper"
                );
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "wrapper exited before readiness"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        kill(Pid::from_raw(child.id() as i32), requested).unwrap();
        let status = wait_owned_wrapper(&mut child, Duration::from_secs(18));
        assert_eq!(status.code(), Some(42), "requested signal {requested:?}");
        let mut exists = Command::new("podman")
            .args(["container", "exists", "--", &cid])
            .spawn()
            .unwrap();
        let absent = wait_owned_wrapper(&mut exists, Duration::from_secs(5));
        assert_eq!(
            absent.code(),
            Some(1),
            "exact invocation disappearance required; access errors are not absence"
        );
        assert_eq!(fs::read_dir(runtime).unwrap().count(), 0);
    }
}

#[test]
#[ignore = "requires authorized dedicated non-root account, certified prepared profile/image and package downloads; run --ignored --test-threads=1"]
fn certified_rootless_installer_tools_host_edits_and_cache_ownership() {
    let sentinel = std::env::var("LARAINIT_TEST_PODMAN_SENTINEL_CID").expect(
        "explicitly activated rootless workflow requires LARAINIT_TEST_PODMAN_SENTINEL_CID; no passing skip",
    );
    assert!(
        sentinel.len() == 64
            && sentinel
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "sentinel must be an operator-supplied full CID, never a guessed name"
    );
    let (fixture, image) = prepared_fixture();
    let sentinel_state = || {
        let output = Command::new("podman")
            .args(["container", "inspect", "--", &sentinel])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "authorized running sentinel is required"
        );
        let objects: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(objects.len(), 1);
        let object = &objects[0];
        assert_eq!(object["Id"].as_str(), Some(sentinel.as_str()));
        assert_eq!(object["State"]["Running"].as_bool(), Some(true));
        assert_eq!(object["State"]["Status"].as_str(), Some("running"));
        assert!(object["State"]["Pid"].as_u64().is_some_and(|pid| pid > 0));
        assert!(
            object["State"]["StartedAt"]
                .as_str()
                .is_some_and(|start| !start.is_empty())
        );
        (
            object["Image"].clone(),
            object["State"]["Pid"].clone(),
            object["State"]["StartedAt"].clone(),
        )
    };
    let sentinel_before = sentinel_state();
    let containers = || {
        let output = Command::new("podman")
            .args(["ps", "--all", "--no-trunc", "--format", "{{.ID}}"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "container observation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    };
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
        "actual installer failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let project = fixture.root.join("my-app");
    assert!(project.join("artisan").is_file());
    assert!(
        fs::read_to_string(project.join("composer.json"))
            .unwrap()
            .contains("laravel/framework")
    );
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(project.join(".env"))
        .unwrap()
        .write_all(b"\n# rootless host edit\n")
        .unwrap();
    for arguments in [
        vec!["php", "artisan", "--version"],
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
            "{arguments:?}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(project.join("public/build/manifest.json").is_file());

    // Seed actual downloads and locks, then launch independent clients against the same cache.
    let seed = fixture.root.join("seed");
    packages::manifests(&seed);
    for args in [packages::COMPOSER_INSTALL, packages::NPM_INSTALL] {
        let output = native(&fixture, &image)
            .current_dir(&seed)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cache seed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let composer_artifacts = fixture.root.join("cache/larainit/composer/files");
    let npm_artifacts = fixture.root.join("cache/larainit/npm/_cacache/content-v2");
    let composer_before = packages::files(&composer_artifacts);
    let npm_before = packages::files(&npm_artifacts);
    assert!(!composer_before.is_empty() && !npm_before.is_empty());
    let runtime = fixture.root.join("runtime");
    fs::create_dir(&runtime).unwrap();
    let projects = [fixture.root.join("reuse-a"), fixture.root.join("reuse-b")];
    let mut children = Vec::new();
    for (index, path) in projects.iter().enumerate() {
        packages::manifests(path);
        packages::locks(&seed, path);
        fs::write(path.join("sentinel"), index.to_string()).unwrap();
        for args in [packages::COMPOSER_INSTALL, packages::NPM_CI] {
            // File-backed output prevents a verbose client blocking behind another client's wait.
            let log = fixture.root.join(format!("reuse-{index}-{}.log", args[0]));
            let stdout = fs::File::create(&log).unwrap();
            let stderr = stdout.try_clone().unwrap();
            let child = native(&fixture, &image)
                .current_dir(path)
                .env("TMPDIR", &runtime)
                .args(args)
                .stdout(stdout)
                .stderr(stderr)
                .spawn()
                .unwrap();
            children.push((child, log));
        }
    }
    for (mut child, log) in children {
        let status = child.wait().unwrap();
        let output = fs::read_to_string(&log).unwrap();
        assert!(status.success(), "concurrent cache reuse: {output}");
        if log
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("composer")
        {
            assert!(
                output.contains("from cache") && output.contains("psr/log"),
                "actual Composer cache hit required: {output}"
            );
        } else {
            assert!(
                output.contains("added 1 package"),
                "offline npm install required: {output}"
            );
        }
    }
    for (index, path) in projects.iter().enumerate() {
        assert_eq!(
            fs::read_to_string(path.join("sentinel")).unwrap(),
            index.to_string()
        );
        assert!(path.join("vendor/psr/log").is_dir());
        assert!(path.join("node_modules/is-number").is_dir());
    }
    assert_eq!(
        fs::read_dir(runtime).unwrap().count(),
        0,
        "invocation scratch remained"
    );
    assert_eq!(packages::files(&composer_artifacts), composer_before);
    assert_eq!(packages::files(&npm_artifacts), npm_before);
    let installer = native(&fixture, &image)
        .args(["laravel", "--version"])
        .output()
        .unwrap();
    assert!(
        installer.status.success()
            && String::from_utf8_lossy(&installer.stdout).contains("Laravel Installer")
    );
    let uid = nix::unistd::getuid().as_raw();
    let mut pending = vec![project, seed, fixture.root.join("cache/larainit")];
    pending.extend(projects);
    let mut cached_files = 0;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert_eq!(metadata.uid(), uid, "host ownership: {}", path.display());
        if metadata.is_dir() {
            pending.extend(
                fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        } else if metadata.is_file() && path.starts_with(fixture.root.join("cache")) {
            cached_files += 1;
        }
    }
    assert!(
        cached_files > 0,
        "actual download artifacts are required, not just cache directories"
    );
    assert_eq!(
        containers(),
        before,
        "test workloads leaked or unrelated container IDs changed; no bulk cleanup"
    );
    assert_eq!(
        sentinel_state(),
        sentinel_before,
        "unrelated sentinel was stopped, restarted or replaced"
    );
}

#[test]
#[ignore = "requires certified rootless/crun fixture and explicitly supplied supplementary-group-only writable file; run --ignored --test-threads=1"]
fn certified_crun_keeps_supplementary_group_access_without_ownership_repair() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("LARAINIT_TEST_PODMAN_GROUP_FILE").expect(
        "explicitly activated group test requires LARAINIT_TEST_PODMAN_GROUP_FILE; no passing skip",
    );
    let path = std::path::PathBuf::from(path);
    assert!(
        path.is_absolute(),
        "authorized fixture path must be absolute"
    );
    let metadata = fs::symlink_metadata(&path).unwrap();
    assert!(
        metadata.is_file(),
        "group fixture must be a regular file, not a symlink"
    );
    assert_ne!(
        metadata.uid(),
        nix::unistd::getuid().as_raw(),
        "owner permission must not authorize this test"
    );
    assert_ne!(
        metadata.gid(),
        nix::unistd::getgid().as_raw(),
        "primary group must not authorize this test"
    );
    assert!(
        nix::unistd::getgroups()
            .unwrap()
            .iter()
            .any(|gid| gid.as_raw() == metadata.gid())
    );
    assert_eq!(
        metadata.permissions().mode() & 0o022,
        0o020,
        "group write required, other write forbidden"
    );
    let (fixture, image) = prepared_fixture();
    let before = fs::read(&path).unwrap();
    let output = native(&fixture, &image).current_dir(path.parent().unwrap())
        .args(["php", "-r", "if (file_put_contents($argv[1], \"\\n# larainit crun group test\\n\", FILE_APPEND) === false) exit(42);"])
        .arg(path.file_name().unwrap()).output().unwrap();
    assert!(
        output.status.success(),
        "supplementary group write failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after = fs::read(&path).unwrap();
    assert_eq!(
        after,
        [before.as_slice(), b"\n# larainit crun group test\n"].concat()
    );
    let after_metadata = fs::symlink_metadata(path).unwrap();
    assert_eq!(
        (
            after_metadata.dev(),
            after_metadata.ino(),
            after_metadata.uid(),
            after_metadata.gid(),
            after_metadata.mode()
        ),
        (
            metadata.dev(),
            metadata.ino(),
            metadata.uid(),
            metadata.gid(),
            metadata.mode()
        )
    );
}
