mod support;
use std::io::Write;
use std::process::Stdio;
use support::Fixture;

#[test]
fn streams_and_all_numeric_engine_statuses_pass_through() {
    for status in [0, 42, 125, 126, 127] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("run-mode", format!("exit={status}"));
        fixture.write("run-stdout", "out\n");
        fixture.write("run-stderr", "mount/tool failure detail\n");
        let output = fixture
            .larainit()
            .env("PATH", bin)
            .args(["php", "-v"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(status));
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"mount/tool failure detail\n");
        assert!(!fixture.calls()[2].contains(&"-t".into()));
    }
}

#[test]
fn piped_bash_and_eof_do_not_require_a_terminal() {
    for input in [b"echo script\n".as_slice(), b"".as_slice()] {
        let fixture = Fixture::new();
        let bin = fixture.install_docker();
        fixture.write("run-mode", "stdin");
        let mut child = fixture
            .larainit()
            .env("PATH", bin)
            .arg("bash")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, input);
        let calls = fixture.calls();
        assert!(calls[2].contains(&"-i".into()));
        assert!(!calls[2].contains(&"-t".into()));
    }
}
