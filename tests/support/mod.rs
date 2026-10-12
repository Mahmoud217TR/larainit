#![allow(dead_code)] // Shared fixture helpers are used incrementally by story tests.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
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
        let path = self.root.join(name);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent().unwrap())
            .unwrap();
        use std::io::Write;
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .write_all(value.as_ref())
            .unwrap();
    }

    pub fn docker(&self) -> Command {
        let mut command = Command::new(mock_binary());
        command.env("MOCK_ROOT", &self.root);
        command
    }

    pub fn calls(&self) -> Vec<Vec<OsString>> {
        self.engine_calls("docker")
    }

    pub fn engine_calls(&self, engine: &str) -> Vec<Vec<OsString>> {
        let root = match engine {
            "docker" => self.root.clone(),
            "podman" | "sqlite3" => self.root.join(engine),
            _ => panic!("unsupported fixture executable"),
        };
        let bytes = fs::read(root.join("argv.bin")).unwrap_or_default();
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
        // No writable executable descriptor for parallel child spawns to inherit.
        fs::hard_link(mock_binary(), bin.join("docker")).unwrap();
        bin
    }

    pub fn install_engine(&self, engine: &str) -> PathBuf {
        assert!(matches!(engine, "podman" | "sqlite3"));
        let bin = self.root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(self.root.join(engine)).unwrap();
        fs::hard_link(mock_binary(), bin.join(engine)).unwrap();
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
        let image =
            std::env::var("LARAINIT_TEST_IMAGE").unwrap_or_else(|_| "larainit:latest".into());
        self.real_larainit_with_image(&image)
    }

    pub fn real_larainit_with_image(&self, image: &str) -> Command {
        let docker_config = std::env::var_os("DOCKER_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    std::env::var_os("HOME").expect("real Docker needs HOME or DOCKER_CONFIG"),
                )
                .join(".docker")
            });
        let mut command = self.larainit();
        command
            .env_remove("MOCK_ROOT")
            .env("DOCKER_CONFIG", docker_config)
            .args(["--engine", "docker", "--image", image]);
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
