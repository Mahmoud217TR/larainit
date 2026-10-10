use crate::image::validate_reference as validate_image;
use crate::{cli::Overrides, error::Error};
use nix::unistd::{AccessFlags, access};
use serde::Deserialize;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub engine: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Auto,
    Docker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Cli,
    Environment(&'static str),
    Config(PathBuf),
    Default,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cli => f.write_str("CLI"),
            Self::Environment(name) => write!(f, "environment {name}"),
            Self::Config(path) => write!(f, "config {}", path.display()),
            Self::Default => f.write_str("default"),
        }
    }
}

#[derive(Debug)]
pub struct Settings {
    pub engine: Engine,
    pub image: String,
    pub engine_source: Source,
    pub image_source: Source,
    pub config_path: PathBuf,
    pub config_present: bool,
    pub cache_root: Result<PathBuf, Error>,
}

pub fn xdg_path(
    base: Option<&OsStr>,
    home: Option<&OsStr>,
    fallback: &str,
    variable: &'static str,
) -> Result<PathBuf, Error> {
    if let Some(base) = base.filter(|v| Path::new(v).is_absolute()) {
        return Ok(PathBuf::from(base).join("larainit"));
    }
    let home = home.filter(|v| Path::new(v).is_absolute()).ok_or_else(|| {
        Error::operational(
            "resolve user path",
            variable,
            "no absolute XDG base or HOME",
            format!("Set an absolute {variable} or HOME"),
        )
    })?;
    Ok(PathBuf::from(home).join(fallback).join("larainit"))
}

pub fn config_path() -> Result<PathBuf, Error> {
    Ok(xdg_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        ".config",
        "XDG_CONFIG_HOME",
    )?
    .join("config.toml"))
}

pub fn cache_root() -> Result<PathBuf, Error> {
    xdg_path(
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        ".cache",
        "XDG_CACHE_HOME",
    )
}

pub fn read(path: &Path) -> Result<(RawConfig, bool), Error> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((RawConfig::default(), false));
        }
        Err(error) => {
            return Err(Error::operational(
                "inspect configuration",
                path.display().to_string(),
                error,
                "Check the configuration path and permissions",
            ));
        }
        Ok(_) => {}
    }
    let text = fs::read_to_string(path).map_err(|error| {
        Error::operational(
            "read configuration",
            path.display().to_string(),
            error,
            "Fix file access and UTF-8 TOML syntax",
        )
    })?;
    let raw = toml::from_str(&text).map_err(|error: toml::de::Error| {
        Error::operational(
            "parse configuration",
            path.display().to_string(),
            error.message(),
            "Use only optional string engine and image keys",
        )
    })?;
    Ok((raw, true))
}

fn pick(
    cli: Option<OsString>,
    environment: Option<OsString>,
    variable: &'static str,
    file: Option<&str>,
    path: &Path,
    default: &str,
) -> (OsString, Source) {
    if let Some(value) = cli {
        return (value, Source::Cli);
    }
    if let Some(value) = environment {
        return (value, Source::Environment(variable));
    }
    if let Some(value) = file {
        return (value.into(), Source::Config(path.into()));
    }
    (default.into(), Source::Default)
}

pub fn resolve(overrides: &Overrides) -> Result<Settings, Error> {
    let path = config_path()?;
    let (raw, config_present) = read(&path)?;
    let (engine, engine_source) = pick(
        overrides.engine.as_ref().map(OsString::from),
        std::env::var_os("LARAINIT_ENGINE"),
        "LARAINIT_ENGINE",
        raw.engine.as_deref(),
        &path,
        "auto",
    );
    let engine = match engine.to_str() {
        Some("auto") => Engine::Auto,
        Some("docker") => Engine::Docker,
        _ => {
            return Err(Error::operational(
                "resolve engine",
                engine_source.to_string(),
                "unsupported engine value (Podman is deferred)",
                "Use auto or docker",
            ));
        }
    };
    let (image, image_source) = pick(
        overrides.image.clone(),
        std::env::var_os("LARAINIT_IMAGE"),
        "LARAINIT_IMAGE",
        raw.image.as_deref(),
        &path,
        "larainit:latest",
    );
    let image = image.into_string().map_err(|_| {
        Error::operational(
            "resolve image",
            image_source.to_string(),
            "image reference is not UTF-8",
            "Use a valid local Docker image reference",
        )
    })?;
    validate_image(&image, &image_source)?;
    Ok(Settings {
        engine,
        image,
        engine_source,
        image_source,
        config_path: path,
        config_present,
        cache_root: cache_root(),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum PathState {
    Usable,
    Preparable(PathBuf),
    Inaccessible(String),
}

/// Read-only evidence, never a guarantee of future writes.
pub fn observe_directory(path: &Path) -> PathState {
    let mut ancestor = path;
    loop {
        match fs::metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => {
                if let Err(error) = access(
                    ancestor,
                    AccessFlags::R_OK | AccessFlags::W_OK | AccessFlags::X_OK,
                ) {
                    return PathState::Inaccessible(format!("{}: {error}", ancestor.display()));
                }
                return if ancestor == path {
                    PathState::Usable
                } else {
                    PathState::Preparable(ancestor.into())
                };
            }
            Ok(_) => {
                return PathState::Inaccessible(format!(
                    "{} is not a directory",
                    ancestor.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match ancestor.parent() {
                Some(parent) => ancestor = parent,
                None => return PathState::Inaccessible("no accessible ancestor".into()),
            },
            Err(error) => {
                return PathState::Inaccessible(format!("{}: {error}", ancestor.display()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_schema_and_xdg_fallbacks() {
        let raw: RawConfig = toml::from_str("engine='docker'\nimage='custom:tag'").unwrap();
        assert_eq!(raw.engine.as_deref(), Some("docker"));
        for text in ["unknown='x'", "engine=42", "image=[]", "engine='"] {
            assert!(toml::from_str::<RawConfig>(text).is_err());
        }
        assert!(toml::from_str::<RawConfig>("").unwrap().engine.is_none());
        for base in [None, Some(OsStr::new("")), Some(OsStr::new("relative"))] {
            assert_eq!(
                xdg_path(
                    base,
                    Some(OsStr::new("/home/me")),
                    ".config",
                    "XDG_CONFIG_HOME"
                )
                .unwrap(),
                Path::new("/home/me/.config/larainit")
            );
            assert!(xdg_path(base, None, ".config", "XDG_CONFIG_HOME").is_err());
        }
        assert_eq!(
            xdg_path(
                Some(OsStr::new("/absolute")),
                None,
                ".cache",
                "XDG_CACHE_HOME"
            )
            .unwrap(),
            Path::new("/absolute/larainit")
        );
    }

    #[test]
    fn precedence_preserves_empty_explicit_overrides() {
        let path = Path::new("/config");
        assert_eq!(
            pick(
                Some("cli".into()),
                Some("env".into()),
                "VAR",
                Some("file"),
                path,
                "default"
            ),
            ("cli".into(), Source::Cli)
        );
        assert_eq!(
            pick(None, Some("".into()), "VAR", Some("file"), path, "default"),
            ("".into(), Source::Environment("VAR"))
        );
        assert_eq!(
            pick(None, None, "VAR", Some("file"), path, "default").1,
            Source::Config(path.into())
        );
        assert_eq!(
            pick(None, None, "VAR", None, path, "default").1,
            Source::Default
        );
        for value in ["", " ", "-bad", "bad\nvalue"] {
            assert!(validate_image(value, &Source::Default).is_err());
        }
        assert!(validate_image("localhost/tool:tag", &Source::Default).is_ok());
    }

    #[test]
    fn observes_without_creating_paths() {
        let root = std::env::temp_dir();
        let path = root
            .join(format!("larainit-observe-{}", std::process::id()))
            .join("child");
        assert!(matches!(observe_directory(&path), PathState::Preparable(_)));
        assert!(!path.exists());
        assert!(matches!(
            observe_directory(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("Cargo.toml")
                    .as_path()
            ),
            PathState::Inaccessible(_)
        ));
    }
}
