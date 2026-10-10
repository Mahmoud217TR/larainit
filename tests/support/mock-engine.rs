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
    let root = std::path::PathBuf::from(env::var_os("MOCK_ROOT").ok_or("MOCK_ROOT required")?);
    if env::current_exe()?
        .file_name()
        .is_some_and(|name| name == "podman")
    {
        fs::write(root.join("invoked-podman"), b"forbidden fallback")?;
        process::exit(99);
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
        Some("version")
            if args.len() == 3 && args[1] == "--format" && args[2] == "{{json .Server}}" =>
        {
            "version"
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
        Some("stop") | Some("rm") => "cleanup",
        _ => {
            fs::write(
                root.join("unexpected-command"),
                b"unexpected engine request",
            )?;
            eprintln!("unexpected mock engine command");
            process::exit(99);
        }
    };
    let mode = fs::read_to_string(root.join(format!("{operation}-mode"))).unwrap_or_default();
    let mode = mode.trim();
    let behavior = mode.strip_prefix("running-").unwrap_or(mode);
    if let Ok(docker) = fs::read_to_string(root.join("forward-docker")) {
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
    if operation == "run" {
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
        if mode == "stdin" {
            let mut data = Vec::new();
            io::stdin().read_to_end(&mut data)?;
            io::stdout().write_all(&data)?;
        }
        fs::write(root.join("run-started"), b"ready")?;
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
        let status = if args.iter().any(|value| value == "SIGINT") {
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
        "inspect" => format!(
            "{{\"Id\":\"sha256:{}\",\"Os\":\"linux\",\"Architecture\":\"amd64\"}}\n",
            "a".repeat(64)
        ),
        _ => String::new(),
    };
    let stdout = fs::read(root.join(format!("{operation}-stdout"))).unwrap_or_else(|_| {
        if mode == "malformed" {
            b"not JSON".to_vec()
        } else {
            default.into_bytes()
        }
    });
    io::stdout().write_all(&stdout)?;
    let stderr = fs::read(root.join(format!("{operation}-stderr"))).unwrap_or_default();
    io::stderr().write_all(&stderr)?;
    if let Some(status) = mode.strip_prefix("exit=") {
        process::exit(status.parse()?);
    }
    Ok(())
}
