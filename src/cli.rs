use clap::{
    Arg, Command,
    builder::{
        NonEmptyStringValueParser, OsStringValueParser, PathBufValueParser, TypedValueParser,
    },
    error::ErrorKind,
};
use std::ffi::OsString;
use std::path::PathBuf;

const ENGINE_VALUES: [&str; 3] = ["auto", "docker", "podman"];

#[derive(Debug, PartialEq, Eq)]
pub struct BuildOptions {
    pub context: PathBuf,
    pub file: Option<PathBuf>,
    pub tag: Option<String>,
    pub engine: Option<String>,
}

#[derive(Debug, Default)]
pub struct Overrides {
    pub engine: Option<String>,
    pub image: Option<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Doctor,
    Engine(Option<String>),
    Build(BuildOptions),
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
        .arg(Arg::new("engine").long("engine").value_parser(ENGINE_VALUES))
        .arg(Arg::new("image").long("image").value_name("IMAGE").value_parser(OsStringValueParser::new()))
        .arg(Arg::new("command").value_name("COMMAND [ARG...]").num_args(1..)
            .trailing_var_arg(true).value_parser(OsStringValueParser::new()))
        .after_help("Global options precede COMMAND. new runs laravel new; doctor inspects prerequisites.\nengine [auto|docker|podman] inspects or saves a preference.\nbuild --context PATH builds an explicitly supplied local context.\nWithout COMMAND, Bash requires all standard streams to be terminals.\nconfig is reserved and not yet supported.")
}

fn builtin(name: &'static str) -> Command {
    let command = Command::new(name).bin_name(format!("larainit {name}"));
    match name {
        "engine" => command.arg(Arg::new("preference").value_parser(ENGINE_VALUES)),
        "build" => command
            .arg(
                Arg::new("context")
                    .long("context")
                    .required(true)
                    .value_name("PATH")
                    .value_parser(path_parser()),
            )
            .arg(
                Arg::new("file")
                    .long("file")
                    .value_name("PATH")
                    .value_parser(path_parser()),
            )
            .arg(
                Arg::new("tag")
                    .long("tag")
                    .value_name("IMAGE")
                    .value_parser(NonEmptyStringValueParser::new()),
            )
            .arg(
                Arg::new("engine")
                    .long("engine")
                    .value_parser(ENGINE_VALUES),
            )
            .after_help("Example: larainit build --context /path/to/source"),
        _ => command,
    }
}

fn path_parser() -> impl TypedValueParser<Value = PathBuf> {
    PathBufValueParser::new().try_map(|path| {
        if path.as_os_str().is_empty() {
            Err("path cannot be empty")
        } else {
            Ok(path)
        }
    })
}

pub fn parse(
    args: impl IntoIterator<Item = OsString>,
    all_terminal: bool,
) -> Result<Invocation, clap::Error> {
    let matches = command().try_get_matches_from(args)?;
    let mut overrides = Overrides {
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
                builtin("doctor").try_get_matches_from([program].into_iter().chain(tail))?;
                Action::Doctor
            }
            Some("engine") => {
                let matches =
                    builtin("engine").try_get_matches_from([program].into_iter().chain(tail))?;
                Action::Engine(matches.get_one::<String>("preference").cloned())
            }
            Some("build") => {
                let matches = builtin("build")
                    .try_get_matches_from([program].into_iter().chain(tail))
                    .map_err(|error| {
                        if error.exit_code() == 0 {
                            error
                        } else {
                            clap::Error::raw(
                                error.kind(),
                                format!(
                                    "{error}\nExample: larainit build --context /path/to/source\n"
                                ),
                            )
                        }
                    })?;
                Action::Build(BuildOptions {
                    context: matches
                        .get_one::<PathBuf>("context")
                        .expect("required context")
                        .clone(),
                    file: matches.get_one::<PathBuf>("file").cloned(),
                    tag: matches.get_one::<String>("tag").cloned(),
                    engine: matches.get_one::<String>("engine").cloned(),
                })
            }
            Some("config") => Action::Reserved("config".to_owned()),
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
    if let Action::Build(options) = &action {
        if let Some(engine) = &options.engine {
            overrides.engine = Some(engine.clone());
        }
    }
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
            vec!["larainit", "--engine", "unknown"],
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
        assert_eq!(
            parsed(&["larainit", "config"]).action,
            Action::Reserved("config".to_owned())
        );
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

    #[test]
    fn builtin_requests_preserve_native_paths_and_build_override_precedence() {
        assert_eq!(parsed(&["larainit", "engine"]).action, Action::Engine(None));
        for preference in ENGINE_VALUES {
            assert_eq!(
                parsed(&["larainit", "engine", preference]).action,
                Action::Engine(Some(preference.into()))
            );
        }
        let invocation = parsed(&[
            "larainit",
            "--engine",
            "docker",
            "--image",
            "selected:tag",
            "build",
            "--context",
            "日本語 context",
            "--file",
            "a b/Dockerfile",
            "--tag",
            "custom:tag",
            "--engine",
            "podman",
        ]);
        assert_eq!(invocation.overrides.engine.as_deref(), Some("podman"));
        assert_eq!(
            invocation.overrides.image.as_deref(),
            Some(std::ffi::OsStr::new("selected:tag"))
        );
        assert_eq!(
            invocation.action,
            Action::Build(BuildOptions {
                context: "日本語 context".into(),
                file: Some("a b/Dockerfile".into()),
                tag: Some("custom:tag".into()),
                engine: Some("podman".into()),
            })
        );
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![0xff]);
        let invocation = parse(
            [
                "larainit".into(),
                "build".into(),
                "--context".into(),
                raw.clone(),
                "--file".into(),
                raw.clone(),
            ],
            false,
        )
        .unwrap();
        assert_eq!(
            invocation.action,
            Action::Build(BuildOptions {
                context: raw.clone().into(),
                file: Some(raw.into()),
                tag: None,
                engine: None,
            })
        );
    }
}
