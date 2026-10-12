mod support;

use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use support::Fixture;

fn prepared_base() -> String {
    let base = std::env::var("LARAINIT_TEST_IMAGE")
        .expect("explicit build activation requires prepared LARAINIT_TEST_IMAGE; no passing skip");
    assert!(!base.is_empty() && !base.starts_with('-'));
    assert!(
        base.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/:._-@[]".contains(&byte)),
        "base must be a single Dockerfile image reference, never injected source"
    );
    base
}

fn real_docker() -> std::path::PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("docker"))
        .find(|path| path.is_file())
        .expect("prepared Docker CLI required")
        .canonicalize()
        .unwrap()
}

#[test]
#[ignore = "explicit real Docker builds with cancellation; requires prepared image, no passing skip"]
fn real_build_cancellation_never_verifies_or_requests_container_cleanup() {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    let base = prepared_base();
    let docker = real_docker();
    for signal in [Signal::SIGINT, Signal::SIGTERM] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("forward-docker", docker.as_os_str().as_encoded_bytes());
        fixture.write("build-mode", "forward");
        let config = b"engine='auto'\nimage='saved:untouched'\n";
        fixture.write("config/larainit/config.toml", config);
        let suffix = fixture.root.file_name().unwrap().to_str().unwrap();
        let marker = format!("ready-{suffix}");
        fixture.write(
            "source/Dockerfile",
            format!("FROM {base}\nRUN echo {marker}; sleep 60\n"),
        );
        let runtime = fixture.root.join("runtime");
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let tag = format!("larainit-build-check:{suffix}-cancel");
        let stdout = fixture.root.join("build.stdout");
        let stderr = fixture.root.join("build.stderr");
        let mut child = fixture
            .real_larainit()
            .env("PATH", bin)
            .env("MOCK_ROOT", &fixture.root)
            .env("TMPDIR", &runtime)
            .env("BUILDKIT_PROGRESS", "plain")
            .args([
                "build",
                "--engine",
                "docker",
                "--context",
                "source",
                "--tag",
                &tag,
            ])
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let ready = loop {
            let text = fs::read_to_string(&stderr).unwrap();
            if text
                .lines()
                .any(|line| line.contains(&marker) && !line.contains("RUN "))
            {
                break true;
            }
            if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        kill(Pid::from_raw(child.id() as i32), signal).ok();
        let deadline = Instant::now() + Duration::from_secs(18);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("real builder cancellation timed out; tag {tag} may remain");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(
            ready,
            "builder did not reach uncached RUN: {}",
            fs::read_to_string(&stderr).unwrap()
        );
        assert!(!status.success());
        assert!(
            !fs::read_to_string(&stdout)
                .unwrap()
                .contains("verified build:")
        );
        let calls = fixture.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][0], "version");
        assert_eq!(calls[1][0], "build");
        assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
        assert_eq!(
            fs::read(fixture.root.join("config/larainit/config.toml")).unwrap(),
            config
        );
        assert!(!fixture.root.join("cache").exists());
        let inspect = Command::new(&docker)
            .args(["image", "inspect", "--", &tag])
            .output()
            .unwrap();
        assert!(!inspect.status.success());
        eprintln!(
            "real uncached build cancelled with {signal:?}: status {status}, no verified result or container cleanup"
        );
    }
}

#[test]
#[ignore = "explicit real Docker build and test-owned tag race; retains own image/layers"]
fn real_build_rejects_tag_replacement_after_successful_native_build() {
    let base = prepared_base();
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    let docker = real_docker();
    fixture.write("forward-docker", docker.as_os_str().as_encoded_bytes());
    fixture.write("build-mode", "forward");
    let config = b"engine='auto'\nimage='saved:untouched'\n";
    fixture.write("config/larainit/config.toml", config);
    let suffix = fixture.root.file_name().unwrap().to_str().unwrap();
    let tag = format!("larainit-build-check:{suffix}-race");
    fixture.write(
        "source/Dockerfile",
        format!("FROM {base}\nLABEL larainit.race={suffix}\n"),
    );
    fixture.write("retag-build-reference", &tag);
    fixture.write("retag-before-build-inspect", &base);
    let runtime = fixture.root.join("runtime");
    fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
    let output = fixture
        .real_larainit()
        .env("PATH", bin)
        .env("MOCK_ROOT", &fixture.root)
        .env("TMPDIR", &runtime)
        .args([
            "build",
            "--engine",
            "docker",
            "--context",
            "source",
            "--tag",
            &tag,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("verified build:"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("own IID, immutable result and target tag do not agree"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fixture.calls();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0][0], "version");
    assert_eq!(calls[1][0], "build");
    assert_eq!(calls[2][0], "image");
    assert_eq!(calls[3].last().unwrap(), tag.as_str());
    let iid = calls[2].last().unwrap();
    let inspect = Command::new(&docker)
        .args([
            std::ffi::OsStr::new("image"),
            std::ffi::OsStr::new("inspect"),
            std::ffi::OsStr::new("--"),
            iid,
        ])
        .output()
        .unwrap();
    assert!(
        inspect.status.success(),
        "wrapper must not roll back its successfully built image"
    );
    eprintln!(
        "real tag-race verified refusal: target {tag}, retained built IID {}",
        iid.to_string_lossy()
    );
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    assert_eq!(
        fs::read(fixture.root.join("config/larainit/config.toml")).unwrap(),
        config
    );
    assert!(!fixture.root.join("cache").exists());
}

#[test]
#[ignore = "explicit local image builds on authorized prepared Docker; retains tagged result/layers"]
fn explicit_docker_build_preserves_settings_and_honors_native_ignore_rules() {
    let base = prepared_base();
    let fixture = Fixture::new();
    // Operator-selected existing image; this does not pull or prepare it.
    let inspect = Command::new("docker")
        .args(["image", "inspect", "--format", "{{json .}}", "--", &base])
        .output()
        .unwrap();
    assert!(
        inspect.status.success(),
        "prepared local base required: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let base_record: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(base_record["Os"], "linux");
    assert_eq!(base_record["Architecture"], "amd64");
    let base_id = base_record["Id"].as_str().unwrap();

    let config = b"engine = \"auto\"\nimage = \"saved/example:untouched\"\n";
    fixture.write("config/larainit/config.toml", config);
    fixture.write("source a,b 日本語/payload/visible", "native-build-marker");
    fixture.write("source a,b 日本語/payload/excluded", "must-not-enter-image");
    fixture.write("source a,b 日本語/.dockerignore", "payload/excluded\n");
    fixture.write("source a,b 日本語/docker/Build file", format!(
        "FROM {base}\nCOPY payload/ /larainit-build-check/\nRUN chmod -R a+rX /larainit-build-check\n"));
    let runtime = fixture.root.join("runtime");
    fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
    let suffix = fixture.root.file_name().unwrap().to_str().unwrap();
    let tag = format!("larainit-build-check:{suffix}");
    // Keep artifacts and report their exact identity, never prune shared builder state.
    eprintln!("operator build fixture tag (artifacts may remain): {tag}");
    let output = fixture
        .real_larainit()
        .env("TMPDIR", &runtime)
        .args([
            "build",
            "--engine",
            "docker",
            "--context",
            "source a,b 日本語",
            "--file",
            "docker/Build file",
            "--tag",
            &tag,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let inspected = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", "--", &tag])
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let id = String::from_utf8(inspected.stdout).unwrap();
    let id = id.trim();
    assert!(
        id.starts_with("sha256:")
            && id.len() == 71
            && id[7..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_ne!(id, base_id, "fixture must produce its own image");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("verified build: docker {tag} {id}"))
    );
    eprintln!("operator build fixture verified ID: {id}");
    assert_eq!(
        fs::read(fixture.root.join("config/larainit/config.toml")).unwrap(),
        config
    );
    assert!(
        !fixture.root.join("cache").exists(),
        "build must not prepare runtime caches"
    );
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);

    let run = fixture.real_larainit_with_image(&tag).env("TMPDIR", &runtime)
        .args(["php", "-r", "if (file_exists('/larainit-build-check/excluded')) exit(42); echo file_get_contents('/larainit-build-check/visible');"])
        .output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"native-build-marker");
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);

    // The native builder, not Larainit, decides that copying an ignored source fails.
    fixture.write(
        "source a,b 日本語/docker/Build file",
        format!("FROM {base}\nCOPY payload/excluded /excluded\n"),
    );
    let failed_tag = format!("larainit-build-check:{suffix}-failed");
    let failed = fixture
        .real_larainit()
        .env("TMPDIR", &runtime)
        .args([
            "build",
            "--engine",
            "docker",
            "--context",
            "source a,b 日本語",
            "--file",
            "docker/Build file",
            "--tag",
            &failed_tag,
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stdout).contains("verified build:"));
    assert_eq!(
        fs::read(fixture.root.join("config/larainit/config.toml")).unwrap(),
        config
    );
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    let preserved = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", "--", &tag])
        .output()
        .unwrap();
    assert!(preserved.status.success());
    assert_eq!(String::from_utf8(preserved.stdout).unwrap().trim(), id);
    let absent = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", "--", &failed_tag])
        .output()
        .unwrap();
    assert!(
        !absent.status.success(),
        "failed builder unexpectedly created target tag"
    );
    assert!(
        String::from_utf8_lossy(&absent.stderr).contains("No such image:"),
        "tag absence not proven: {}",
        String::from_utf8_lossy(&absent.stderr)
    );
}
