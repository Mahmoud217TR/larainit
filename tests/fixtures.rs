mod support;

use std::io::Write;
use std::process::Stdio;
use support::Fixture;

#[test]
fn native_fixture_records_arguments_and_simulates_observations_and_failures() {
    let fixture = Fixture::new();
    let result = fixture
        .docker()
        .args(["version", "--format", "{{json .Server}}"])
        .env("DOCKER_CONTEXT", "isolated-test")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(
        serde_json::from_slice::<serde_json::Value>(&result.stdout)
            .unwrap()
            .is_object()
    );
    assert_eq!(
        fixture.calls()[0],
        ["version", "--format", "{{json .Server}}"]
    );
    fixture.write("inspect-mode", "exit=17");
    fixture.write("inspect-stderr", "test denial");
    let result = fixture
        .docker()
        .args([
            "image",
            "inspect",
            "--format",
            "{{json .}}",
            "--",
            "image with spaces",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(17));
    assert_eq!(result.stderr, b"test denial");
    assert_eq!(fixture.calls()[1][5], "image with spaces");
    let result = fixture.docker().arg("build").output().unwrap();
    assert_eq!(result.status.code(), Some(99));
    assert!(fixture.root.join("unexpected-command").exists());
}

#[test]
fn fixture_preserves_input_empty_and_native_values_and_creates_cidfile() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    fixture.write("run-mode", "stdin");
    let cid = fixture.root.join("cid");
    let raw = std::ffi::OsString::from_vec(vec![0xff, b' ']);
    let mut child = fixture
        .docker()
        .arg("run")
        .arg("--cidfile")
        .arg(&cid)
        .arg("")
        .arg(&raw)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"input\n").unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"input\n");
    assert_eq!(support::read_text(cid).unwrap(), "a".repeat(64));
    let calls = fixture.calls();
    assert_eq!(calls[0][3], "");
    assert_eq!(calls[0][4], raw);
}
