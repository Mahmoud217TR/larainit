mod support;
use std::ffi::OsString;
use support::Fixture;

#[test]
fn command_boundaries_and_argument_corpus_are_exact() {
    for executable in ["php", "/path with spaces/executable", r#"["program"]"#] {
        for delimiter in [false, true] {
            let fixture = Fixture::new();
            let bin = fixture.install_docker();
            let corpus = [
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
                "--image",
                "=",
                "--x=a=b",
                "$(touch injected)",
            ];
            let mut command = fixture.larainit();
            command.env("PATH", bin);
            if delimiter {
                command.arg("--");
            }
            let output = command.arg(executable).args(corpus).output().unwrap();
            assert!(output.status.success());
            let calls = fixture.calls();
            let run = &calls[2];
            let index = run.iter().position(|arg| arg == "--entrypoint").unwrap();
            assert_eq!(run[index + 1], executable);
            assert_eq!(&run[index + 4..], corpus.map(OsString::from));
            assert!(!fixture.root.join("injected").exists());
        }
    }
}

#[test]
fn invalid_global_invocations_never_contact_an_engine() {
    for args in [
        vec!["--unknown", "php"],
        vec!["--engine"],
        vec!["--engine", "docker", "--engine", "auto", "php"],
        vec!["--image", "a", "--image", "b", "php"],
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        assert_eq!(
            fixture
                .larainit()
                .env("PATH", bin)
                .args(args)
                .output()
                .unwrap()
                .status
                .code(),
            Some(2)
        );
        assert!(fixture.calls().is_empty());
    }
}

#[test]
fn unrepresentable_transport_is_not_replaced_or_logged() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    let output = fixture
        .larainit()
        .env("PATH", bin)
        .arg("php")
        .arg(OsString::from_vec(vec![0xff]))
        .arg("private-token-value")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("losslessly"));
    assert!(!stderr.contains("private-token-value"));
    assert!(!fixture.root.join("cache").exists());
}

#[test]
fn builtin_grammar_errors_are_local_usage_failures() {
    for args in [
        vec!["engine", "unknown"],
        vec!["engine", "docker", "extra"],
        vec!["engine", "--engine", "docker"],
        vec!["build"],
        vec!["build", "--context"],
        vec!["build", "--context", ""],
        vec!["build", "--context", ".", "--file", ""],
        vec!["build", "--context", ".", "--tag", ""],
        vec!["build", "--context", ".", "--engine", "unknown"],
        vec!["build", "--context", ".", "--unknown"],
        vec!["build", "--context", ".", "php"],
        vec!["doctor", "extra"],
    ] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("config/larainit/config.toml", "unknown = 'private-value'");
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("private-value"));
        if args[0] == "build" {
            assert!(stderr.contains("larainit build --context"), "{stderr}");
        }
        assert!(fixture.calls().is_empty());
        assert!(!fixture.root.join("cache").exists());
    }
}

#[test]
fn builtin_help_is_local_and_config_stays_reserved() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("config/larainit/config.toml", "unknown = 'private-value'");
    for args in [
        vec!["engine", "--help"],
        vec!["build", "--help"],
        vec!["doctor", "--help"],
        vec!["--engine", "podman", "build", "--help"],
    ] {
        let output = fixture
            .larainit()
            .env("PATH", &bin)
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    }
    assert_eq!(
        fixture
            .larainit()
            .env("PATH", bin)
            .arg("config")
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert!(fixture.calls().is_empty());
    assert!(!fixture.root.join("cache").exists());
}
