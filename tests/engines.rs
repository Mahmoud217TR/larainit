mod support;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::time::{Duration, Instant};
use support::Fixture;

#[test]
#[ignore = "operator creates one test-only group-owned file using container root; run with prepared rootful non-remapped Docker"]
fn supplementary_group_only_file_is_writable_without_ownership_repair() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();
    assert_ne!(
        uid, 0,
        "ownership verification requires an ordinary host user"
    );
    let supplementary = nix::unistd::getgroups()
        .unwrap()
        .into_iter()
        .map(|group| group.as_raw())
        .find(|group| *group != gid)
        .expect("a real supplementary group is required for this requested test");
    let image = std::env::var("LARAINIT_TEST_IMAGE").expect("set prepared LARAINIT_TEST_IMAGE");
    let mount = format!("type=bind,src={},dst=/fixture", fixture.root.display());
    // Explicit operator fixture setup, confined to a newly allocated test directory.
    // Larainit never requests container root or changes ownership itself.
    let setup = std::process::Command::new("docker").args(["run","--rm","--pull=never","--user","0","--mount",&mount,"--entrypoint","bash","--",&image,"-c",
        &format!("printf original > /fixture/group-only; chown 424242:{supplementary} /fixture/group-only; chmod 660 /fixture/group-only")]).output().unwrap();
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let path = fixture.root.join("group-only");
    let original = fs::metadata(&path).unwrap();
    assert_eq!(original.uid(), 424242);
    assert_eq!(original.gid(), supplementary);
    let without_groups = std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "--pull=never",
            "--user",
            &format!("{uid}:{gid}"),
            "--mount",
            &mount,
            "--entrypoint",
            "bash",
            "--",
            &image,
            "-c",
            "printf denied >> /fixture/group-only",
        ])
        .output()
        .unwrap();
    assert!(
        !without_groups.status.success(),
        "test fixture did not require supplementary group access"
    );
    let output = fixture.real_larainit().args(["php","-r","exit(file_put_contents('group-only',' native-group-write', FILE_APPEND) === false ? 1 : 0);"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(path).unwrap(), b"original native-group-write");
    let after = fs::metadata(fixture.root.join("group-only")).unwrap();
    assert_eq!(
        (after.uid(), after.gid(), after.mode()),
        (original.uid(), original.gid(), original.mode())
    );
}

fn operator_image(base: &str, tag: &str, marker: &str) -> String {
    let mut child = std::process::Command::new("docker")
        .args(["build", "--quiet", "-t", tag, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Explicit test-operator preparation, never a Larainit startup path.
    child.stdin.take().unwrap().write_all(format!("FROM {base}\nENV LARAINIT_TEST_MARKER={marker}\nLABEL larainit.operator-test={tag}\n").as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = std::process::Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", "--", tag])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
#[ignore = "explicit operator builds disposable local images; run with --ignored --test-threads=1"]
fn real_image_retag_removal_and_context_failure_never_provision_or_fall_back() {
    let base = std::env::var("LARAINIT_TEST_IMAGE")
        .expect("set a prepared LARAINIT_TEST_IMAGE for operator race fixtures");
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    let suffix = fixture.root.file_name().unwrap().to_str().unwrap();
    let selected = format!("larainit-race:{suffix}-selected");
    let replacement = format!("larainit-race:{suffix}-replacement");
    let original_id = operator_image(&base, &selected, "original");
    let replacement_id = operator_image(&base, &replacement, "replacement");
    let docker = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("docker"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    fixture.write("forward-docker", docker.to_str().unwrap());
    fixture.write("retag-before-run", &replacement_id);
    fixture.write("retag-reference", &selected);
    let docker_config = std::env::var_os("DOCKER_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".docker")
        });
    let output = fixture
        .larainit()
        .env("DOCKER_CONFIG", &docker_config)
        .env("PATH", &bin)
        .args([
            "--image",
            &selected,
            "php",
            "-r",
            "echo getenv('LARAINIT_TEST_MARKER');",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"original");
    let calls = fixture.calls();
    let run = calls.iter().find(|call| call[0] == "run").unwrap();
    assert!(run.contains(&original_id.clone().into()));
    assert!(!run.contains(&selected.clone().into()));
    fs::remove_file(fixture.root.join("retag-before-run")).unwrap();
    let removal_tag = format!("larainit-race:{suffix}-remove");
    let removal_id = operator_image(&base, &removal_tag, "remove");
    fixture.write("remove-before-run", &removal_id);
    let output = fixture
        .larainit()
        .env("DOCKER_CONFIG", &docker_config)
        .env("PATH", &bin)
        .args(["--image", &removal_tag, "php", "-v"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&output.stderr).contains("No such image"));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&removal_tag),
        "image-removal failures must retain the requested reference"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("manual"),
        "image-removal failures need a non-provisioning remedy"
    );
    assert!(
        fixture
            .calls()
            .iter()
            .filter(|call| call[0] == "run")
            .all(|call| call.contains(&"--pull=never".into()))
    );
    assert!(!fixture.root.join("unexpected-command").exists());
    let isolated = Fixture::new();
    let output = isolated
        .real_larainit()
        .env("DOCKER_CONTEXT", format!("larainit-nonexistent-{suffix}"))
        .arg("php")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!isolated.root.join("cache").exists());
    // Remove only identities built by this test; never normal operator images or broad filters.
    for id in [&original_id, &replacement_id] {
        assert!(
            std::process::Command::new("docker")
                .args(["image", "rm", "--force", "--", id])
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
#[ignore = "requires real prepared Docker; explicitly run with --ignored --test-threads=1"]
fn real_arguments_special_mounts_streams_eof_and_statuses() {
    let fixture = Fixture::new();
    let workspace = fixture.root.join("workspace a,b\"q:c 日本語");
    fs::create_dir(&workspace).unwrap();
    let program = workspace.join("args with spaces");
    fs::write(
        &program,
        b"#!/usr/local/bin/php\n<?php echo json_encode(array_slice($argv,1));\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let corpus = [
        "",
        "a b",
        "it's",
        "\"quoted\"",
        "日本語",
        "$HOME;*",
        "--",
        "--help",
        "--version",
        "--engine",
        "=",
        "--x=a=b",
    ];
    let output = fixture
        .real_larainit()
        .current_dir(&workspace)
        .arg("/workspace/args with spaces")
        .args(corpus)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&output.stdout).unwrap(),
        corpus
    );
    let link = fixture.root.join("linked project");
    std::os::unix::fs::symlink(&workspace, &link).unwrap();
    let output = fixture
        .real_larainit()
        .current_dir(&link)
        .args(["php", "-r", "echo file_get_contents('args with spaces');"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, fs::read(&program).unwrap());
    let output = fixture
        .real_larainit()
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
        let mut child = fixture
            .real_larainit()
            .args(["php", "-r", "echo stream_get_contents(STDIN);"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, input);
    }
    let output = fixture
        .real_larainit()
        .args(["php", "-r", "echo stream_get_contents(STDIN);"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let output = fixture
        .real_larainit()
        .arg("larainit-deliberately-absent-program")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(127));
    fs::write(
        fixture.root.join("not-executable"),
        b"no execute permission",
    )
    .unwrap();
    let output = fixture
        .real_larainit()
        .arg("/workspace/not-executable")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(126));
}

#[test]
#[ignore = "requires real prepared Docker; explicitly run with --ignored --test-threads=1"]
fn real_signals_reach_command_preserve_status_and_remove_identified_container() {
    for signal in [
        nix::sys::signal::Signal::SIGINT,
        nix::sys::signal::Signal::SIGTERM,
    ] {
        let fixture = Fixture::new();
        let runtime = fixture.root.join("runtime");
        fs::create_dir(&runtime).unwrap();
        let mut child = fixture
            .real_larainit()
            .env("TMPDIR", &runtime)
            .args([
                "bash",
                "-c",
                "trap 'exit 42' INT TERM; touch ready; while :; do sleep 0.1; done",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !fixture.root.join("ready").exists() {
            assert!(start.elapsed() < Duration::from_secs(30));
            assert!(child.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        let scratch = fs::read_dir(&runtime)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let id = fs::read_to_string(scratch.join("cid")).unwrap();
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id() as i32), signal).unwrap();
        let start = Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > Duration::from_secs(20) {
                child.kill().unwrap();
                panic!("real interruption exceeded bound; container {id} may remain");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output = std::process::Command::new("docker")
            .args(["container", "inspect", "--", id.trim()])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "identified test container remained"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("No such"));
        assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    }
}
