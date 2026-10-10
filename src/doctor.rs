use crate::{
    cli::Overrides,
    config::{self, Engine, PathState},
    engine,
    error::Error,
    image, runner,
};
use std::io::Write;
use std::path::Path;

#[derive(Default)]
struct Report {
    lines: Vec<String>,
    failures: Vec<String>,
}

impl Report {
    fn add(&mut self, state: &str, label: &str, detail: impl std::fmt::Display) {
        self.lines.push(format!("[{state}] {label}: {detail}"));
    }

    fn fail(&mut self, label: &str, cause: &Error) {
        self.add("failure", label, cause);
        self.failures.push(cause.to_string());
    }

    fn directory(&mut self, label: &str, path: &Path) {
        if let Err(cause) = runner::mount(path, "/inspection-only") {
            self.fail(label, &cause);
            return;
        }
        match config::observe_directory(path) {
            PathState::Usable => self.add("success", label, format!("{} (read-only permission evidence)", path.display())),
            PathState::Preparable(ancestor) => self.add("info", label, format!("{} absent/preparable; existing ancestor {}", path.display(), ancestor.display())),
            PathState::Inaccessible(cause) => self.fail(label, &Error::operational("inspect path", path.display().to_string(), cause, "Choose an accessible directory; doctor creates no write-test files or directories")),
        }
    }
}

pub fn run(overrides: &Overrides) -> Result<i32, Error> {
    let mut report = Report::default();
    report.add("info","platform/version",format!("{} {} larainit {}; verification: see README recorded Ubuntu/Docker environment; this host/mode is not automatically verified", std::env::consts::OS,std::env::consts::ARCH,env!("CARGO_PKG_VERSION")));
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        report.fail(
            "platform",
            &Error::operational(
                "check platform",
                std::env::consts::OS,
                "outside the initial Linux x86_64 target",
                "Use the documented target; other platforms are unverified",
            ),
        );
    }
    match config::config_path() {
        Ok(path) => report.add("info", "configuration path", path.display()),
        Err(cause) => report.fail("configuration path", &cause),
    }
    let settings = config::resolve(overrides);
    match &settings {
        Ok(settings) => {
            report.add(
                "info",
                "configuration file",
                format!(
                    "{} ({})",
                    settings.config_path.display(),
                    if settings.config_present {
                        "present; parsed"
                    } else {
                        "not present; optional"
                    }
                ),
            );
            report.add(
                "success",
                "effective engine",
                format!(
                    "{} -> Docker (source: {})",
                    match settings.engine {
                        Engine::Auto => "auto",
                        Engine::Docker => "docker",
                    },
                    settings.engine_source
                ),
            );
            report.add(
                "success",
                "effective image",
                format!("{} (source: {})", settings.image, settings.image_source),
            );
        }
        Err(cause) => report.fail("configuration/effective settings", cause),
    }
    // Only shared bounded observation supervision is used, never tool/container lifecycle setup.
    let signals = runner::Signals::register()?;
    match engine::discover(std::env::var_os("PATH").as_deref()) {
        Ok(path) => report.add("success", "Docker executable", path.display()),
        Err(cause) => report.fail("Docker executable", &cause),
    }
    if let Ok(settings) = &settings {
        match engine::assess(Some(&signals)) {
            Ok(docker) => {
                report.add(
                    "success",
                    "Docker server",
                    format!("{} (effective configured endpoint/context)", docker.version),
                );
                match image::inspect(&docker, &settings.image, Some(&signals)) {
                    Ok(image) => report.add(
                        "success",
                        "local image",
                        format!(
                            "{}: {} {}/{}",
                            settings.image, image.id, image.os, image.architecture
                        ),
                    ),
                    Err(cause) => report.fail("local image", &cause),
                }
            }
            Err(cause) => {
                report.fail("Docker server", &cause);
                report.add("not checked", "local image", "Docker server is unavailable");
            }
        }
    } else {
        report.add(
            "not checked",
            "Docker server",
            "effective settings could not be resolved; no engine probe is authorized",
        );
        report.add(
            "not checked",
            "local image",
            "effective settings are unavailable",
        );
    }
    match runner::workspace() {
        Ok(path) => report.directory("workspace", &path),
        Err(cause) => report.fail("workspace", &cause),
    }
    match config::cache_root() {
        Ok(root) => {
            report.directory("Composer cache", &root.join("composer"));
            report.directory("npm cache", &root.join("npm"));
        }
        Err(cause) => {
            report.fail("cache root", &cause);
            report.add(
                "not checked",
                "Composer cache",
                "cache root could not be resolved",
            );
            report.add(
                "not checked",
                "npm cache",
                "cache root could not be resolved",
            );
        }
    }
    report.add("info","inspection boundary","normal Docker-owned bookkeeping and user-configured socket activation may occur; no explicit service management, containers, image provisioning or user-state writes are requested. Permission observations do not guarantee future writes");
    let mut stdout = std::io::stdout().lock();
    for line in report.lines {
        writeln!(stdout, "{line}").map_err(|cause| {
            Error::operational(
                "write doctor report",
                "stdout",
                cause,
                "Check the output destination",
            )
        })?;
    }
    for failure in &report.failures {
        crate::error::diagnostic(failure);
    }
    Ok(i32::from(!report.failures.is_empty()))
}
