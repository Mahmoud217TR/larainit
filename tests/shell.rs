mod support;
use std::io::Write;
use std::process::Stdio;
use support::Fixture;

#[test]
fn bare_nonterminal_and_reserved_names_are_local_failures() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    for (arguments, status) in [
        (vec![], 2),
        (vec!["engine"], 1),
        (vec!["build"], 1),
        (vec!["config"], 1),
    ] {
        let output = fixture
            .larainit()
            .env("PATH", &bin)
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(status));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(if status == 2 {
            "supply a command"
        } else {
            "not yet supported"
        }));
    }
    assert!(fixture.calls().is_empty());
    assert!(!fixture.root.join("cache").exists());
}

#[test]
fn explicit_shell_and_external_help_are_not_local_flags() {
    let fixture = Fixture::new();
    let bin = fixture.install_docker();
    fixture.write("run-mode", "stdin");
    let mut child = fixture
        .larainit()
        .env("PATH", &bin)
        .arg("bash")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"shell script\n")
        .unwrap();
    assert_eq!(child.wait_with_output().unwrap().stdout, b"shell script\n");
    for flag in ["--help", "--version"] {
        assert!(
            fixture
                .larainit()
                .env("PATH", &bin)
                .args(["php", flag])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let calls = fixture.calls();
    assert_eq!(calls.iter().filter(|call| call[0] == "run").count(), 3);
    for flag in ["--help", "--version"] {
        assert!(
            calls
                .iter()
                .any(|call| call.last().is_some_and(|last| last == flag))
        );
    }
}
