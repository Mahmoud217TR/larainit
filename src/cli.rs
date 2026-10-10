use clap::{Arg, Command, builder::OsStringValueParser, error::ErrorKind};
use std::ffi::OsString;

#[derive(Debug, Default)]
pub struct Overrides {
    pub engine: Option<String>,
    pub image: Option<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Doctor,
    Reserved(String),
    Tool {
        program: OsString,
        args: Vec<OsString>,
    },
}

#[derive(Debug)]
pub struct Invocation {
    pub action: Action,
    pub overrides: Overrides,
}

pub fn command() -> Command {
    Command::new("larainit")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Run Laravel and toolchain commands in disposable Docker containers")
        .dont_delimit_trailing_values(true)
        .arg(Arg::new("engine").long("engine").value_parser(["auto", "docker"]))
        .arg(Arg::new("image").long("image").value_name("IMAGE").value_parser(OsStringValueParser::new()))
        .arg(Arg::new("command").value_name("COMMAND [ARG...]").num_args(1..)
            .trailing_var_arg(true).value_parser(OsStringValueParser::new()))
        .after_help("Global options precede COMMAND. new runs laravel new; doctor inspects prerequisites.\nWithout COMMAND, Bash requires all standard streams to be terminals.\nengine, build, and config are reserved and not yet supported.")
}

pub fn parse(
    args: impl IntoIterator<Item = OsString>,
    all_terminal: bool,
) -> Result<Invocation, clap::Error> {
    let matches = command().try_get_matches_from(args)?;
    let overrides = Overrides {
        engine: matches.get_one::<String>("engine").cloned(),
        image: matches.get_one::<OsString>("image").cloned(),
    };
    let mut tail: Vec<OsString> = matches
        .get_many::<OsString>("command")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    let action = if tail.is_empty() {
        if !all_terminal {
            return Err(command().error(ErrorKind::MissingRequiredArgument,
                "bare invocation requires terminal stdin/stdout/stderr; supply a command, for example larainit bash"));
        }
        Action::Tool {
            program: "bash".into(),
            args: Vec::new(),
        }
    } else {
        let program = tail.remove(0);
        if program.is_empty() {
            return Err(command().error(ErrorKind::InvalidValue, "the executable cannot be empty"));
        }
        match program.to_str() {
            Some("doctor") => {
                if !tail.is_empty() {
                    return Err(command().error(
                        ErrorKind::UnknownArgument,
                        "doctor accepts no positional arguments",
                    ));
                }
                Action::Doctor
            }
            Some(name @ ("engine" | "build" | "config")) => Action::Reserved(name.to_owned()),
            Some("new") => {
                tail.insert(0, "new".into());
                Action::Tool {
                    program: "laravel".into(),
                    args: tail,
                }
            }
            _ => Action::Tool {
                program,
                args: tail,
            },
        }
    };
    Ok(Invocation { action, overrides })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Invocation {
        parse(args.iter().map(OsString::from), false).unwrap()
    }

    #[test]
    fn command_tail_is_native_and_exact() {
        let tail = [
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
        ];
        for prefix in [vec!["larainit", "php"], vec!["larainit", "--", "php"]] {
            let mut values = prefix;
            values.extend(tail);
            assert_eq!(
                parsed(&values).action,
                Action::Tool {
                    program: "php".into(),
                    args: tail.iter().map(OsString::from).collect(),
                }
            );
        }
        assert_eq!(
            parsed(&["larainit", "new", "my app"]).action,
            Action::Tool {
                program: "laravel".into(),
                args: vec!["new".into(), "my app".into()],
            }
        );
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![0xff]);
        let result = parse(["larainit".into(), raw.clone(), raw.clone()], false).unwrap();
        assert_eq!(
            result.action,
            Action::Tool {
                program: raw.clone(),
                args: vec![raw]
            }
        );
    }

    #[test]
    fn globals_local_commands_and_reserved_routes() {
        for args in [
            vec!["larainit"],
            vec!["larainit", "--unknown"],
            vec!["larainit", "--engine"],
            vec!["larainit", "--engine", "podman"],
            vec!["larainit", "--image", "a", "--image", "b"],
            vec!["larainit", "doctor", "extra"],
        ] {
            assert_eq!(
                parse(args.iter().map(OsString::from), false)
                    .unwrap_err()
                    .exit_code(),
                2
            );
        }
        for flag in ["--help", "--version"] {
            assert_eq!(
                parse(["larainit".into(), flag.into()], false)
                    .unwrap_err()
                    .exit_code(),
                0
            );
        }
        for name in ["engine", "build", "config"] {
            assert_eq!(
                parsed(&["larainit", name]).action,
                Action::Reserved(name.to_owned())
            );
        }
        assert_eq!(
            parse(["larainit".into()], true).unwrap().action,
            Action::Tool {
                program: "bash".into(),
                args: vec![]
            }
        );
        let result = parsed(&[
            "larainit", "--engine", "docker", "--image", "test:tag", "php",
        ]);
        assert_eq!(result.overrides.engine.as_deref(), Some("docker"));
        assert_eq!(
            result.overrides.image.as_deref(),
            Some(std::ffi::OsStr::new("test:tag"))
        );
    }
}
