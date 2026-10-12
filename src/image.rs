use crate::{
    engine::{self, EngineKind, ResolvedEngine},
    error::Error,
    runner::{self, Observation, Signals},
};

#[derive(Debug, serde::Deserialize)]
pub struct Image {
    #[serde(skip)]
    pub reference: String,
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Os")]
    pub os: String,
    #[serde(rename = "Architecture")]
    pub architecture: String,
    #[serde(rename = "ManifestType", default)]
    manifest_type: Option<String>,
}

pub fn inspect(
    selected: &ResolvedEngine,
    reference: &str,
    signals: Option<&Signals>,
) -> Result<Image, Error> {
    validate_reference(reference, &crate::config::Source::Default)?;
    let mut command = inspection_command(selected, reference);
    let output = runner::observe(&mut command, signals)
        .map_err(|cause| error_for(selected.kind, reference, cause))?;
    parse_for(selected.kind, reference, &output)
}

fn inspection_command(selected: &ResolvedEngine, reference: &str) -> std::process::Command {
    let mut command = engine::command(&selected.executable);
    command.args(["image", "inspect"]);
    if selected.kind == EngineKind::Docker {
        command.args(["--format", "{{json .}}"]);
    }
    command.args(["--", reference]);
    command
}

pub(crate) fn error_for(kind: EngineKind, reference: &str, cause: impl std::fmt::Display) -> Error {
    let store = match kind {
        EngineKind::Docker => "Docker effective context",
        EngineKind::Podman => "Podman selected local rootless store",
    };
    Error::operational(
        "inspect local image",
        format!("{store}, image {reference}"),
        cause,
        format!(
            "Prepare a compatible local image in the {store}. Use larainit build --engine {kind} --context PATH --tag {reference:?} with explicit local source; from the source checkout, manually run {kind} build -t {reference:?} .; standalone users should follow README image preparation. Ordinary execution never implicitly pulls or builds images"
        ),
    )
}

fn parse_for(kind: EngineKind, reference: &str, output: &Observation) -> Result<Image, Error> {
    let error = |cause: String| error_for(kind, reference, cause);
    if !output.status.success() {
        let cause = engine::failed_response(output);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let missing = match kind {
            EngineKind::Docker => stderr.contains("No such image:"),
            EngineKind::Podman => stderr.contains("image not known"),
        };
        return Err(error(if missing {
            format!("local image absent: {cause}")
        } else {
            format!("image inspection failed: {cause}")
        }));
    }
    let mut image: Image = match kind {
        EngineKind::Docker => serde_json::from_slice(&output.stdout),
        EngineKind::Podman => {
            serde_json::from_slice::<Vec<Image>>(&output.stdout).and_then(|mut images| {
                if images.len() != 1 {
                    return Err(serde::de::Error::custom(
                        "exactly one local image is required",
                    ));
                }
                Ok(images.remove(0))
            })
        }
    }
    .map_err(|_| error("malformed or incomplete image JSON".into()))?;
    if kind == EngineKind::Podman {
        if !valid_container_id(&image.id) {
            return Err(error("missing/invalid full Podman image ID".into()));
        }
        if !matches!(
            image.manifest_type.as_deref(),
            Some(
                "application/vnd.oci.image.manifest.v1+json"
                    | "application/vnd.docker.distribution.manifest.v2+json"
            )
        ) {
            return Err(error("unsupported or missing image manifest format".into()));
        }
        image.id = format!("sha256:{}", image.id);
    }
    if !valid_id(&image.id) {
        return Err(error("missing/invalid immutable image ID".into()));
    }
    if image.os != "linux" || image.architecture != "amd64" {
        return Err(error(format!(
            "incompatible image platform {}/{}; require linux/amd64",
            image.os, image.architecture
        )));
    }
    image.reference = reference.to_owned();
    Ok(image)
}

pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("sha256:").is_some_and(valid_container_id)
}

pub fn validate_reference(image: &str, source: &crate::config::Source) -> Result<(), Error> {
    if image.trim().is_empty() || image.starts_with('-') || image.chars().any(char::is_control) {
        let mut error = Error::operational(
            "resolve image",
            source.to_string(),
            "blank, control-character or option-like image reference",
            "Specify a usable pre-existing local image in the selected engine store",
        );
        if *source == crate::config::Source::Cli {
            error.kind = crate::error::Kind::Usage;
        }
        return Err(error);
    }
    Ok(())
}

pub fn valid_container_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn podman_inspection_is_local_single_image_and_normalizes_only_its_id() {
        let fixture = crate::test_support::Fixture::new();
        let bin = fixture.install_engine("podman");
        let selected = ResolvedEngine {
            kind: engine::EngineKind::Podman,
            executable: bin.join("podman"),
            version: "mock".into(),
            selection_reason: "test only",
        };
        let id = "b".repeat(64);
        let good = format!(
            r#"[{{"Id":"{id}","Os":"linux","Architecture":"amd64","ManifestType":"application/vnd.oci.image.manifest.v1+json","Digest":"sha256:{}"}}]"#,
            "c".repeat(64)
        );
        let output = |stdout: String| Observation {
            status: std::process::ExitStatus::from_raw(0),
            stdout: stdout.into(),
            stderr: vec![],
        };
        assert_eq!(
            parse_for(selected.kind, "app:tag", &output(good.clone()))
                .unwrap()
                .id,
            format!("sha256:{id}")
        );
        for invalid in [
            "[]".into(),
            "[{},{}]".into(),
            good[1..good.len() - 1].into(),
            good.replace(&id, "abc"),
            good.replace(&id, &"B".repeat(64)),
            good.replace("amd64", "arm64"),
            good.replace("linux", "windows"),
            good.replace(
                "application/vnd.oci.image.manifest.v1+json",
                "application/vnd.oci.image.index.v1+json",
            ),
            good.replace("\"ManifestType\":", "\"Unknown\":"),
            good.replace("\"Id\":", "\"Id\":\"bad\",\"Id\":"),
        ] {
            assert!(parse_for(selected.kind, "app:tag", &output(invalid)).is_err());
        }
        let docker_manifest = good.replace(
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
        );
        assert!(parse_for(selected.kind, "app:tag", &output(docker_manifest)).is_ok());
        let args: Vec<_> = inspection_command(&selected, "app:tag")
            .get_args()
            .map(std::ffi::OsString::from)
            .collect();
        assert_eq!(
            args,
            ["image", "inspect", "--", "app:tag"].map(std::ffi::OsString::from)
        );
        for (kind, label) in [
            (engine::EngineKind::Docker, "docker"),
            (engine::EngineKind::Podman, "podman"),
        ] {
            let failure = error_for(kind, "app:tag", "permission denied").to_string();
            assert!(
                failure.contains(&format!("build --engine {label} --context")),
                "{failure}"
            );
            assert!(
                failure.contains("never implicitly pulls or builds"),
                "{failure}"
            );
        }
        assert!(fixture.engine_calls("podman").is_empty());
    }

    #[test]
    fn independent_mock_stores_never_search_the_other_engine() {
        for kind in [EngineKind::Docker, EngineKind::Podman] {
            for failure in ["none", "missing", "denied"] {
                let fixture = crate::test_support::Fixture::new();
                let bin = fixture.install_docker();
                fixture.install_engine("podman");
                let (name, prefix, response) = match kind {
                    EngineKind::Docker => (
                        "docker",
                        "",
                        format!(
                            r#"{{"Id":"sha256:{}","Os":"linux","Architecture":"amd64"}}"#,
                            "a".repeat(64)
                        ),
                    ),
                    EngineKind::Podman => (
                        "podman",
                        "podman/",
                        format!(
                            r#"[{{"Id":"{}","Os":"linux","Architecture":"amd64","ManifestType":"application/vnd.oci.image.manifest.v1+json"}}]"#,
                            "b".repeat(64)
                        ),
                    ),
                };
                fixture.write(&format!("{prefix}inspect-stdout"), response);
                if failure != "none" {
                    fixture.write(&format!("{prefix}inspect-mode"), "exit=1");
                    fixture.write(
                        &format!("{prefix}inspect-stderr"),
                        match (kind, failure) {
                            (_, "denied") => "permission denied",
                            (EngineKind::Docker, _) => "No such image: app:tag",
                            (EngineKind::Podman, _) => "app:tag: image not known",
                        },
                    );
                }
                let selected = ResolvedEngine {
                    kind,
                    executable: bin.join(name),
                    version: "mock".into(),
                    selection_reason: "test only",
                };
                let mut command = inspection_command(&selected, "app:tag");
                command.env("MOCK_ROOT", &fixture.root);
                let output = runner::observe(&mut command, None).unwrap();
                let result = parse_for(kind, "app:tag", &output);
                if failure == "none" {
                    let expected = if kind == EngineKind::Docker { "a" } else { "b" };
                    assert_eq!(
                        result.unwrap().id,
                        format!("sha256:{}", expected.repeat(64))
                    );
                } else {
                    let error = result.unwrap_err().to_string();
                    assert!(
                        error.contains(if failure == "missing" {
                            "image absent"
                        } else {
                            "inspection failed"
                        }),
                        "{error}"
                    );
                    assert!(
                        error.contains(&format!("build --engine {name} --context")),
                        "{error}"
                    );
                }
                assert_eq!(fixture.engine_calls(name).len(), 1);
                let other = if kind == EngineKind::Docker {
                    "podman"
                } else {
                    "docker"
                };
                assert!(fixture.engine_calls(other).is_empty());
                assert!(!fixture.root.join("cache").exists());
            }
        }
    }

    #[test]
    fn selected_store_rejects_incomplete_or_ambiguous_identity_without_provisioning() {
        use serde_json::{Value, json};
        for kind in [EngineKind::Docker, EngineKind::Podman] {
            let id = if kind == EngineKind::Docker {
                format!("sha256:{}", "a".repeat(64))
            } else {
                "a".repeat(64)
            };
            let valid = json!({"Id": id, "Os": "linux", "Architecture": "amd64", "ManifestType": "application/vnd.oci.image.manifest.v1+json"});
            let encode = |value: Value| {
                if kind == EngineKind::Docker {
                    value.to_string()
                } else {
                    json!([value]).to_string()
                }
            };
            let mut responses = vec![
                "null".into(),
                "[]".into(),
                "not JSON".into(),
                encode(
                    json!({"Digest": format!("sha256:{}", "a".repeat(64)), "Os": "linux", "Architecture": "amd64"}),
                ),
            ];
            responses.push(if kind == EngineKind::Docker {
                json!([valid]).to_string()
            } else {
                valid.to_string()
            });
            let mut wrong_id_shape = valid.clone();
            wrong_id_shape["Id"] = json!(if kind == EngineKind::Docker {
                "a".repeat(64)
            } else {
                format!("sha256:{}", "a".repeat(64))
            });
            responses.push(encode(wrong_id_shape));
            responses.push(
                encode(valid.clone())
                    .replace("\"Os\":\"linux\"", "\"Os\":\"linux\",\"Os\":\"linux\""),
            );
            for key in ["Id", "Os", "Architecture"] {
                for replacement in [None, Some(Value::Null), Some(json!([])), Some(json!({}))] {
                    let mut invalid = valid.clone();
                    if let Some(value) = replacement {
                        invalid[key] = value;
                    } else {
                        invalid.as_object_mut().unwrap().remove(key);
                    }
                    responses.push(encode(invalid));
                }
            }
            for id in [
                "abc".into(),
                "A".repeat(64),
                format!("sha256:{}", "a".repeat(63)),
                format!("sha256:sha256:{}", "a".repeat(64)),
            ] {
                let mut invalid = valid.clone();
                invalid["Id"] = json!(id);
                responses.push(encode(invalid));
            }
            if kind == EngineKind::Podman {
                responses.push(json!([valid, valid]).to_string());
                for format in [
                    Value::Null,
                    json!("application/vnd.oci.image.index.v1+json"),
                    json!("application/vnd.docker.distribution.manifest.list.v2+json"),
                    json!("application/vnd.docker.distribution.manifest.v1+json"),
                ] {
                    let mut invalid = valid.clone();
                    invalid["ManifestType"] = format;
                    responses.push(encode(invalid));
                }
            }
            for response in responses {
                let fixture = crate::test_support::Fixture::new();
                let bin = fixture.install_docker();
                fixture.install_engine("podman");
                let (name, prefix, other) = if kind == EngineKind::Podman {
                    ("podman", "podman/", "docker")
                } else {
                    ("docker", "", "podman")
                };
                fixture.write(&format!("{prefix}inspect-stdout"), response);
                let selected = ResolvedEngine {
                    kind,
                    executable: bin.join(name),
                    version: "mock".into(),
                    selection_reason: "test only",
                };
                let error = inspect(&selected, "short-name", None)
                    .unwrap_err()
                    .to_string();
                assert!(
                    error.contains(&format!("build --engine {name} --context")),
                    "{error}"
                );
                assert_eq!(fixture.engine_calls(name).len(), 1);
                assert!(fixture.engine_calls(other).is_empty());
                assert!(!fixture.root.join("cache").exists());
                assert!(!fixture.root.join("config").exists());
                assert!(!fixture.root.join("unexpected-command").exists());
            }
        }
    }

    #[test]
    fn local_image_requires_immutable_identity_and_platform() {
        let parse = |reference: &str, output: &Observation| {
            parse_for(EngineKind::Docker, reference, output)
        };
        let id = format!("sha256:{}", "a".repeat(64));
        let good = format!(r#"{{"Id":"{id}","Os":"linux","Architecture":"amd64","Extra":1}}"#);
        let mut output = Observation {
            status: std::process::ExitStatus::from_raw(0),
            stdout: good.clone().into(),
            stderr: vec![],
        };
        assert_eq!(parse("test:tag", &output).unwrap().id, id);
        for data in [
            "null".to_owned(),
            "{}".to_owned(),
            good.replace("amd64", "arm64"),
            good.replace("linux", "windows"),
            good.replace(&id, "bad"),
        ] {
            output.stdout = data.into();
            assert!(parse("test:tag", &output).is_err());
        }
        output.status = std::process::ExitStatus::from_raw(1 << 8);
        output.stderr = b"permission denied".to_vec();
        assert!(
            parse("test:tag", &output)
                .unwrap_err()
                .to_string()
                .contains("inspection failed")
        );
        output.stderr = b"Error: No such image: test:tag".to_vec();
        assert!(
            parse("test:tag", &output)
                .unwrap_err()
                .to_string()
                .contains("image absent")
        );
        assert!(!valid_container_id("a"));
        assert!(!valid_container_id(&"G".repeat(64)));
    }
}
