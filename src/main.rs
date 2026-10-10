mod cache;
mod cli;
mod config;
mod doctor;
mod engine;
mod error;
mod image;
mod runner;

use std::io::IsTerminal;
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
        let docker = engine::assess(Some(&signals))?;
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
