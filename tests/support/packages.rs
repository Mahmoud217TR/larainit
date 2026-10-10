#![allow(dead_code)] // Reuse and concurrency suites share only the helpers each needs.
use super::Fixture;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Output;

pub const COMPOSER_INSTALL: &[&str] = &[
    "composer",
    "install",
    "--no-interaction",
    "--prefer-dist",
    "--no-scripts",
    "--no-plugins",
    "-vvv",
];
pub const NPM_INSTALL: &[&str] = &[
    "npm",
    "install",
    "--ignore-scripts",
    "--no-audit",
    "--no-fund",
    "--loglevel=http",
];
pub const NPM_CI: &[&str] = &[
    "npm",
    "ci",
    "--ignore-scripts",
    "--no-audit",
    "--no-fund",
    "--offline",
    "--loglevel=http",
];

pub fn manifests(project: &Path) {
    fs::create_dir(project).unwrap();
    fs::write(
        project.join("composer.json"),
        br#"{"name":"larainit/cache-fixture","require":{"psr/log":"3.0.2"}}"#,
    )
    .unwrap();
    fs::write(project.join("package.json"), br#"{"name":"larainit-cache-fixture","version":"1.0.0","private":true,"dependencies":{"is-number":"7.0.0"}}"#).unwrap();
}

pub fn run(fixture: &Fixture, project: &Path, args: &[&str]) -> Output {
    let output = fixture
        .real_larainit()
        .current_dir(project)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?} failed: {}\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub fn locks(source: &Path, destination: &Path) {
    for file in ["composer.lock", "package-lock.json"] {
        fs::copy(source.join(file), destination.join(file)).unwrap();
    }
}

pub fn files(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.extend(files(&path));
            } else {
                result.insert(path.clone(), fs::read(path).unwrap());
            }
        }
    }
    result
}
