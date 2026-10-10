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
