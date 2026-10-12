mod build;
mod cache;
mod cli;
mod config;
mod doctor;
mod engine;
mod error;
mod image;
mod runner;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let terminal = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal();
    let invocation = match cli::parse(std::env::args_os(), terminal) {
        Ok(invocation) => invocation,
        Err(error) => {
            let status = error.exit_code();
            if let Err(cause) = error.print() {
                error::diagnostic(format_args!("write local assistance: {cause}"));
                return ExitCode::from(if status == 0 { 1 } else { status as u8 });
            }
            return ExitCode::from(status as u8);
        }
    };
    let result = match invocation.action {
        cli::Action::Reserved(name) => Err(error::Error::operational(
            "route command",
            name,
            "not yet supported",
            "Use new, doctor, bash, or an explicit tool command",
        )),
        cli::Action::Engine(value) => engine_command(&invocation.overrides, value.as_deref()),
        cli::Action::Build(options) => build::run(&options, &invocation.overrides),
        cli::Action::Doctor => doctor::run(&invocation.overrides),
        cli::Action::Tool { program, args } => {
            run_tool(&invocation.overrides, &program, &args, terminal)
        }
    };
    match result {
        Ok(status) => ExitCode::from(status as u8),
        Err(error) => {
            error::diagnostic(&error);
            ExitCode::from(error.status() as u8)
        }
    }
}

fn engine_command(overrides: &cli::Overrides, value: Option<&str>) -> Result<i32, error::Error> {
    let signals = runner::Signals::register()?;
    let result = (|| {
        let (preference, saved, report) = if let Some(value) = value {
            let preference =
                config::EnginePreference::parse(std::ffi::OsStr::new(value), &config::Source::Cli)?;
            match config::save(&config::config_path()?, preference, &signals)? {
                config::SaveOutcome::CommittedDurabilityUnconfirmed => {
                    error::diagnostic(error::Error::operational(
                        "save engine preference",
                        "config",
                        "saved, durability unconfirmed",
                        "The new preference is visible; no rollback or readiness probe was attempted",
                    ));
                    // Committed sync failure is status 1, even if a signal interrupted sync.
                    return Ok(1);
                }
                config::SaveOutcome::DurablySaved => {}
            }
            (
                preference,
                true,
                format!("preference saved: {preference}\n"),
            )
        } else {
            let settings = config::resolve(overrides)?;
            (
                settings.engine,
                false,
                format!(
                    "saved preference: {}\neffective preference: {} (source: {})\n",
                    settings
                        .saved_engine
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "not set (auto default)".into()),
                    settings.engine,
                    settings.engine_source
                ),
            )
        };
        std::io::stdout()
            .lock()
            .write_all(report.as_bytes())
            .map_err(|cause| {
                error::Error::operational(
                    "write engine report",
                    "stdout",
                    cause,
                    "Check output access; a completed save is not rolled back",
                )
            })?;
        let docker = engine::assess_preference(preference, Some(&signals)).map_err(|cause| {
            if saved {
                error::Error::operational(
                    "report saved engine readiness",
                    preference.to_string(),
                    format!("preference saved; execution unavailable: {cause}"),
                    "The saved preference was not rolled back; configure that engine independently",
                )
            } else {
                cause
            }
        })?;
        writeln!(
            std::io::stdout().lock(),
            "selected engine: {} ({})\nselection reason: {}",
            docker.kind,
            docker.version,
            docker.selection_reason
        )
        .map_err(|cause| {
            error::Error::operational(
                "write engine readiness",
                "stdout",
                cause,
                "Check output access; a completed save is not rolled back",
            )
        })?;
        Ok(0)
    })();
    if result.is_err() {
        if let Some(signal) = signals.signal() {
            return Ok(128 + signal as i32);
        }
    }
    result
}

fn run_tool(
    overrides: &cli::Overrides,
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    terminal: bool,
) -> Result<i32, error::Error> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err(error::Error::operational(
            "check execution platform",
            std::env::consts::OS,
            "only Linux x86_64 is the initial verification target",
            "Use the documented target; other platforms are unverified",
        ));
    }
    let settings = config::resolve(overrides)?;
    let signals = runner::Signals::register()?;
    let result = (|| {
        let docker = engine::assess_preference(settings.engine, Some(&signals))?;
        let image = image::inspect(&docker, &settings.image, Some(&signals))?;
        let workspace = runner::workspace()?;
        runner::validate_tool(program, args)?;
        let caches = cache::Caches::at(&settings.cache_root?);
        runner::execute(
            &docker,
            &image,
            &workspace,
            &caches,
            (program, args),
            terminal,
            &signals,
        )
    })();
    if result.is_err() {
        if let Some(signal) = signals.signal() {
            return Ok(128 + signal as i32);
        }
    }
    result
}
