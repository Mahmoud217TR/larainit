#![allow(dead_code)] // Shared fixture helpers are used incrementally by story tests.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub struct Fixture {
    pub root: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let mut random = [0; 16];
        use std::io::Read;
        fs::File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut random)
            .unwrap();
        let name: String = random.iter().map(|v| format!("{v:02x}")).collect();
        let root = std::env::temp_dir().join(format!("larainit-test-{name}"));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self { root }
    }

    pub fn write(&self, name: &str, value: impl AsRef<[u8]>) {
        fs::write(self.root.join(name), value).unwrap();
    }

    pub fn docker(&self) -> Command {
        let mut command = Command::new(mock_binary());
        command.env("MOCK_ROOT", &self.root);
        command
    }

    pub fn calls(&self) -> Vec<Vec<OsString>> {
        let bytes = fs::read(self.root.join("argv.bin")).unwrap_or_default();
        let mut bytes = bytes.as_slice();
        let mut result = Vec::new();
        while !bytes.is_empty() {
            let len = length(&mut bytes);
            let (frame, rest) = bytes.split_at(len);
            bytes = rest;
            let mut frame = frame;
            let mut values = Vec::new();
            while !frame.is_empty() {
                let len = length(&mut frame);
                let (value, rest) = frame.split_at(len);
                values.push(OsString::from_vec(value.to_vec()));
                frame = rest;
            }
            result.push(values);
        }
        result
    }

    pub fn install_docker(&self) -> PathBuf {
        let bin = self.root.join("bin");
        fs::create_dir(&bin).unwrap();
        fs::copy(mock_binary(), bin.join("docker")).unwrap();
        bin
    }

    pub fn larainit(&self) -> Command {
        let binary = match option_env!("CARGO_BIN_EXE_larainit") {
            Some(binary) => binary,
            None => panic!(
                "larainit() is only used by integration tests, which supply the Cargo binary path"
            ),
        };
        let mut command = Command::new(binary);
        command
            .current_dir(&self.root)
            .env("MOCK_ROOT", &self.root)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env_remove("LARAINIT_ENGINE")
            .env_remove("LARAINIT_IMAGE");
        command
    }

    pub fn real_larainit(&self) -> Command {
        let docker_config = std::env::var_os("DOCKER_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    std::env::var_os("HOME").expect("real Docker needs HOME or DOCKER_CONFIG"),
                )
                .join(".docker")
            });
        let image =
            std::env::var("LARAINIT_TEST_IMAGE").unwrap_or_else(|_| "larainit:latest".into());
        let mut command = self.larainit();
        command
            .env_remove("MOCK_ROOT")
            .env("DOCKER_CONFIG", docker_config)
            .args(["--engine", "docker", "--image", &image]);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn length(bytes: &mut &[u8]) -> usize {
    let (head, rest) = bytes.split_at(8);
    *bytes = rest;
    u64::from_le_bytes(head.try_into().unwrap()) as usize
}

pub fn mock_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let fixture = Fixture::new();
        let binary = fixture.root.join("docker");
        let status = Command::new("rustc")
            .args(["--edition=2024", "-Dwarnings"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/mock-engine.rs"))
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap();
        assert!(status.success(), "native mock compilation failed");
        // Keep the binary for this test process; all scenario directories are independently removed.
        std::mem::forget(fixture);
        binary
    })
}

pub fn read_text(path: impl AsRef<Path>) -> io::Result<String> {
    fs::read_to_string(path)
}
