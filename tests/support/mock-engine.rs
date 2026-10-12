use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::process::{self, Command};
use std::thread;
use std::time::Duration;

fn record(values: impl Iterator<Item = impl AsRef<std::ffi::OsStr>>) -> Vec<u8> {
    let mut result = Vec::new();
    for value in values {
        let bytes = value.as_ref().as_bytes();
        result.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        result.extend_from_slice(bytes);
    }
    result
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|v| v == "--retain-pipe") {
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
    let executable = env::current_exe()?;
    let name = executable.file_name().ok_or("missing executable name")?;
    let mut root = match env::var_os("MOCK_ROOT") {
        Some(root) => std::path::PathBuf::from(root),
        None if executable.parent().and_then(|path| path.file_name())
            == Some(std::ffi::OsStr::new("bin")) =>
        {
            executable
                .parent()
                .and_then(|path| path.parent())
                .ok_or("mock fixture root missing")?
                .to_owned()
        }
        None => return Err("MOCK_ROOT required outside an installed fixture".into()),
    };
    if name == "podman" && !root.join("podman").is_dir() {
        fs::write(root.join("invoked-podman"), b"forbidden fallback")?;
        process::exit(99);
    }
    if name == "podman" || name == "sqlite3" {
        root = root.join(name);
    }
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("argv.bin"))?;
    let values = record(args.iter());
    let mut frame = (values.len() as u64).to_le_bytes().to_vec();
    frame.extend_from_slice(&values);
    log.write_all(&frame)?;
    let keys = [
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_CLI_HOOKS",
        "DOCKER_CLI_HINTS",
    ];
    fs::write(
        root.join("environment.bin"),
        record(
            keys.into_iter()
                .flat_map(|key| [Some(key.into()), env::var_os(key)].into_iter().flatten()),
        ),
    )?;

    let operation = match args.first().and_then(|v| v.to_str()) {
        Some("-init")
            if name == "sqlite3"
                && args.len() == 6
                && args[1] == "/dev/null"
                && args[2] == "-readonly"
                && args[3] == "-json" =>
        {
            "metadata"
        }
        Some("info")
            if name == "podman"
                && args.len() == 3
                && args[1] == "--format"
                && args[2] == "json" =>
        {
            "info"
        }
        Some("version")
            if args.len() == 3 && args[1] == "--format" && args[2] == "{{json .Server}}" =>
        {
            "version"
        }
        Some("image")
            if name == "podman" && args.len() == 4 && args[1] == "inspect" && args[2] == "--" =>
        {
            "inspect"
        }
        Some("image")
            if args.len() == 6
                && args[1] == "inspect"
                && args[2] == "--format"
                && args[3] == "{{json .}}"
                && args[4] == "--" =>
        {
            "inspect"
        }
        Some("run") => "run",
        Some("build") => "build",
        Some("stop") | Some("rm") | Some("kill") => "cleanup",
        _ => {
            fs::write(
                root.join("unexpected-command"),
                b"unexpected engine request",
            )?;
            eprintln!("unexpected mock engine command");
            process::exit(99);
        }
    };
    if operation == "inspect"
        && fs::read_to_string(root.join("inspect-missing-ref"))
            .ok()
            .is_some_and(|reference| {
                args.last().and_then(|arg| arg.to_str()) == Some(reference.as_str())
            })
    {
        eprintln!("No such image: test-owned removed reference");
        process::exit(1);
    }
    if matches!(operation, "info" | "metadata" | "build")
        && !root.join(format!("{operation}-mode")).exists()
        && !root.join(format!("{operation}-stdout")).exists()
        && !(operation == "build" && root.join("build-iid").exists())
    {
        fs::write(root.join("unexpected-command"), b"unconfigured operation")?;
        process::exit(99);
    }
    let mode = fs::read_to_string(root.join(format!("{operation}-mode"))).unwrap_or_default();
    let mode = mode.trim();
    let behavior = mode.strip_prefix("running-").unwrap_or(mode);
    if let Ok(docker) = fs::read_to_string(root.join("forward-docker")) {
        if operation == "inspect" {
            if let Ok(reference) = fs::read_to_string(root.join("retag-build-reference")) {
                if args.last().is_some_and(|arg| arg == reference.trim()) {
                    let replacement = fs::read_to_string(root.join("retag-before-build-inspect"))?;
                    if !Command::new(docker.trim())
                        .args(["image", "tag", replacement.trim(), reference.trim()])
                        .status()?
                        .success()
                    {
                        return Err("operator fixture could not retag its own build result".into());
                    }
                    fs::remove_file(root.join("retag-build-reference"))?;
                }
            }
        }
        if operation == "run" && mode == "create-only" {
            let mut create = args.clone();
            create[0] = "create".into();
            create.retain(|argument| argument != "--sig-proxy=false");
            let status = Command::new(docker.trim()).args(create).status()?;
            if !status.success() {
                process::exit(status.code().unwrap_or(1));
            }
            fs::write(
                root.join("run-started"),
                b"created, deliberately not started",
            )?;
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        if operation == "run" {
            if let Ok(id) = fs::read_to_string(root.join("remove-before-run")) {
                let status = Command::new(docker.trim())
                    .args(["image", "rm", "--force", "--", id.trim()])
                    .status()?;
                if !status.success() {
                    return Err("operator fixture could not remove its disposable image".into());
                }
            }
            if let Ok(id) = fs::read_to_string(root.join("retag-before-run")) {
                let reference = fs::read_to_string(root.join("retag-reference"))?;
                let status = Command::new(docker.trim())
                    .args(["image", "tag", id.trim(), reference.trim()])
                    .status()?;
                if !status.success() {
                    return Err("operator fixture could not retag its disposable image".into());
                }
            }
        }
        let status = Command::new(docker.trim()).args(&args).status()?;
        use std::os::unix::process::ExitStatusExt;
        process::exit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        );
    }
    if mode == "remove-self" {
        fs::remove_file(env::current_exe()?)?;
    }
    if mode == "hang" {
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
    if mode == "flood" {
        loop {
            io::stdout().write_all(&[b'o'; 4096])?;
            io::stderr().write_all(&[b'e'; 4096])?;
        }
    }
    if mode == "retain-pipes" {
        fs::write(root.join("retained-leader-pid"), process::id().to_string())?;
        let _child = Command::new(env::current_exe()?)
            .arg("--retain-pipe")
            .spawn()?;
    }
    if operation == "build" {
        if let Some(index) = args.iter().position(|v| v == "--iidfile") {
            if let Ok(id) = fs::read(root.join("build-iid")) {
                let path = args.get(index + 1).ok_or("missing iidfile value")?;
                fs::write(root.join("build-iid-path"), path.as_bytes())?;
                match mode {
                    "iid-symlink" | "iid-hardlink" => {
                        fs::write(root.join("build-target"), id)?;
                        if mode == "iid-symlink" {
                            std::os::unix::fs::symlink(root.join("build-target"), path)?;
                        } else {
                            fs::hard_link(root.join("build-target"), path)?;
                        }
                    }
                    "iid-fifo" => {
                        let status = Command::new("/usr/bin/mkfifo").arg(path).status()?;
                        if !status.success() {
                            return Err("mock FIFO setup failed".into());
                        }
                    }
                    _ => fs::write(path, id)?,
                }
            }
        }
        fs::write(root.join("build-started"), b"ready")?;
        if mode == "signal-exit-0" || mode == "signal-exit-42" {
            use std::os::unix::process::CommandExt;
            let script = if mode == "signal-exit-0" {
                "trap 'exit 0' INT TERM; : > \"$1\"; while :; do /bin/sleep 0.1; done"
            } else {
                "trap 'exit 42' INT TERM; : > \"$1\"; while :; do /bin/sleep 0.1; done"
            };
            fs::write(root.join("build-leader-pid"), process::id().to_string())?;
            let error = Command::new("/bin/sh")
                .args(["-c", script, "test-owned-build-client"])
                .arg(root.join("build-trap-ready"))
                .exec();
            return Err(error.into());
        }
        if mode == "running-descendant" {
            fs::write(root.join("build-leader-pid"), process::id().to_string())?;
            let _descendant = Command::new("/bin/sh")
                .args([
                    "-c",
                    "trap '' INT TERM; echo $$ > \"$1\"; exec /bin/sleep 60",
                    "test-owned-descendant",
                ])
                .arg(root.join("build-descendant-pid"))
                .stdin(process::Stdio::null())
                .stdout(process::Stdio::null())
                .stderr(process::Stdio::null())
                .spawn()?;
        }
        if mode == "running" || mode == "running-descendant" {
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
    }
    if operation == "inspect" {
        if let Ok(mutation) = fs::read_to_string(root.join("inspect-iid-mutation")) {
            use std::os::unix::ffi::OsStringExt;
            let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(fs::read(
                root.join("build-iid-path"),
            )?));
            let same = format!("sha256:{}", "a".repeat(64));
            let other = format!("sha256:{}", "b".repeat(64));
            match mutation.as_str() {
                "replace-same" | "replace-other" => {
                    let replacement = path.with_extension("replacement");
                    fs::write(
                        &replacement,
                        if mutation == "replace-same" {
                            same
                        } else {
                            other
                        },
                    )?;
                    fs::rename(replacement, path)?;
                }
                "edit-same" => fs::write(path, same)?,
                "edit-other" => fs::write(path, other)?,
                "remove" => fs::remove_file(path)?,
                _ => return Err("unknown IID mutation".into()),
            }
            fs::remove_file(root.join("inspect-iid-mutation"))?;
        }
    }
    if operation == "run" {
        let cid_behavior = fs::read_to_string(root.join("run-cid-behavior"))
            .unwrap_or_else(|_| behavior.to_owned());
        let behavior = cid_behavior.as_str();
        if let Some(index) = args.iter().position(|arg| arg == "--entrypoint") {
            let image = args.get(index + 3).ok_or("missing run image")?;
            if let Ok(expected) = fs::read_to_string(root.join("run-expected-image")) {
                if image != std::ffi::OsStr::new(&expected) {
                    fs::write(
                        root.join("unexpected-command"),
                        b"replacement image executed",
                    )?;
                    process::exit(99);
                }
            }
            if fs::read_to_string(root.join("run-removed-image"))
                .ok()
                .is_some_and(|removed| image == std::ffi::OsStr::new(&removed))
            {
                eprintln!("No such image: inspected immutable image was removed");
                process::exit(125);
            }
        }
        if behavior == "late-cid" {
            thread::sleep(Duration::from_millis(150));
        }
        if behavior != "missing-cid" {
            if let Some(index) = args.iter().position(|v| v == "--cidfile") {
                let path = args.get(index + 1).ok_or("missing cidfile value")?;
                let id = if behavior == "bad-cid" {
                    "invalid".to_owned()
                } else {
                    "a".repeat(64)
                };
                fs::write(path, id)?;
            }
        }
        if let Some(status) = mode.strip_prefix("created-exit=") {
            fs::write(root.join("container-state"), b"created")?;
            eprintln!("mock container created but workload never started");
            process::exit(status.parse()?);
        }
        if mode == "stdin" {
            let mut data = Vec::new();
            io::stdin().read_to_end(&mut data)?;
            io::stdout().write_all(&data)?;
        }
        fs::write(root.join("run-started"), b"ready")?;
        if mode == "detach" {
            // Change only this test-owned PTY; leave restoration to the native wrapper.
            if !Command::new("/bin/stty")
                .args(["raw", "-echo"])
                .status()?
                .success()
            {
                return Err("mock detach requires a terminal".into());
            }
            fs::write("ready", b"ready")?;
            let mut keys = [0; 2];
            io::stdin().read_exact(&mut keys)?;
            fs::write(root.join("detach-received"), keys)?;
            if keys != [0x10, 0x11] {
                return Err("unexpected mock detach keys".into());
            }
        }
        if mode == "running" || mode.starts_with("running-") {
            loop {
                if let Ok(status) = fs::read_to_string(root.join("stopped-status")) {
                    process::exit(status.trim().parse()?);
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
    if operation == "cleanup" && mode.is_empty() {
        if args.last().and_then(|value| value.to_str()) != Some(&"a".repeat(64)) {
            fs::write(root.join("unexpected-command"), b"unauthorized cleanup ID")?;
            process::exit(99);
        }
        if fs::read(root.join("container-state")).ok().as_deref() == Some(b"created") {
            if args[0] == "rm" {
                fs::write(root.join("container-state"), b"removed")?;
                return Ok(());
            }
            eprintln!("container exists but is not running");
            process::exit(1);
        }
        let status = if args.iter().any(|value| value == "SIGINT" || value == "INT") {
            130
        } else {
            143
        };
        let status = fs::read_to_string(root.join("requested-status"))
            .unwrap_or_else(|_| status.to_string());
        fs::write(root.join("stopped-status"), status)?;
    }
    let default = match operation {
        "version" => {
            "{\"Version\":\"test-server\",\"Os\":\"linux\",\"Arch\":\"amd64\"}\n".to_owned()
        }
        "inspect" => {
            let object = format!(
                "{{\"Id\":\"sha256:{}\",\"Os\":\"linux\",\"Architecture\":\"amd64\"}}",
                "a".repeat(64)
            );
            if name == "podman" {
                format!(
                    "[{{\"Id\":\"{}\",\"Os\":\"linux\",\"Architecture\":\"amd64\",\"ManifestType\":\"application/vnd.oci.image.manifest.v1+json\"}}]\n",
                    "a".repeat(64)
                )
            } else {
                format!("{object}\n")
            }
        }
        _ => String::new(),
    };
    let output_name = if operation == "inspect"
        && fs::read_to_string(root.join("inspect-tag"))
            .ok()
            .is_some_and(|tag| args.last().and_then(|arg| arg.to_str()) == Some(tag.as_str()))
    {
        "inspect-tag-stdout".to_owned()
    } else {
        format!("{operation}-stdout")
    };
    let stdout = fs::read(root.join(output_name)).unwrap_or_else(|_| {
        if mode == "malformed" {
            b"not JSON".to_vec()
        } else {
            default.into_bytes()
        }
    });
    io::stdout().write_all(&stdout)?;
    if operation == "inspect" {
        if let Ok(next) = fs::read(root.join("inspect-next-stdout")) {
            fs::write(root.join("inspect-stdout"), next)?;
            fs::remove_file(root.join("inspect-next-stdout"))?;
        }
    }
    let stderr = fs::read(root.join(format!("{operation}-stderr"))).unwrap_or_default();
    io::stderr().write_all(&stderr)?;
    if let Some(status) = mode.strip_prefix("exit=") {
        process::exit(status.parse()?);
    }
    Ok(())
}
