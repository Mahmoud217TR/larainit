use crate::{
    config::{self, PathState},
    error::Error,
};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub struct Caches {
    pub composer: PathBuf,
    pub npm: PathBuf,
}

impl Caches {
    pub fn at(root: &Path) -> Self {
        Self {
            composer: root.join("composer"),
            npm: root.join("npm"),
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        for path in [&self.composer, &self.npm] {
            crate::runner::mount(path, "/cache")?;
            if let PathState::Inaccessible(cause) = config::observe_directory(path) {
                return Err(failure(path, cause));
            }
        }
        Ok(())
    }

    pub fn prepare(&self) -> Result<(), Error> {
        for path in [&self.composer, &self.npm] {
            prepare_directory(path)?;
        }
        Ok(())
    }
}

fn failure(path: &Path, cause: impl std::fmt::Display) -> Error {
    Error::operational(
        "prepare download cache",
        path.display().to_string(),
        cause,
        "Choose an accessible writable XDG_CACHE_HOME; no cache bypass, elevation or existing-permission repair is performed",
    )
}

fn prepare_directory(path: &Path) -> Result<(), Error> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|cause| failure(path, cause))?;
    match config::observe_directory(path) {
        PathState::Usable => Ok(()),
        state => Err(failure(
            path,
            format!("not usable after preparation: {state:?}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn creates_private_download_caches_without_repairing_existing_permissions() {
        let root = std::env::temp_dir().join(format!("larainit-cache-unit-{}", std::process::id()));
        assert!(!root.exists());
        let caches = Caches::at(&root);
        caches.validate().unwrap();
        assert!(!root.exists());
        caches.prepare().unwrap();
        for path in [&root, &caches.composer, &caches.npm] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::set_permissions(&caches.composer, fs::Permissions::from_mode(0o755)).unwrap();
        caches.prepare().unwrap();
        assert_eq!(
            fs::metadata(&caches.composer).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::write(root.join("file"), b"existing user file").unwrap();
        assert!(prepare_directory(&root.join("file")).is_err());
        assert_eq!(fs::read(root.join("file")).unwrap(), b"existing user file");
        fs::remove_dir_all(root).unwrap();
    }
}
