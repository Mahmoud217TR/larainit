use crate::{
    engine::{self, Docker},
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
}

pub fn inspect(
    docker: &Docker,
    reference: &str,
    signals: Option<&Signals>,
) -> Result<Image, Error> {
    validate_reference(reference, &crate::config::Source::Default)?;
    let mut command = engine::command(&docker.executable);
    command.args([
        "image",
        "inspect",
        "--format",
        "{{json .}}",
        "--",
        reference,
    ]);
    let output = runner::observe(&mut command, signals).map_err(|cause| error(reference, cause))?;
    parse(reference, &output)
}

pub fn error(reference: &str, cause: impl std::fmt::Display) -> Error {
    Error::operational(
        "inspect local image",
        format!("Docker effective context, image {reference}"),
        cause,
        format!(
            "Prepare a compatible local image in the effective Docker context. From the Larainit source checkout, manually run docker build -t {reference:?} .; standalone users should follow README image preparation. Larainit never pulls or builds images"
        ),
    )
}

fn parse(reference: &str, output: &Observation) -> Result<Image, Error> {
    if !output.status.success() {
        let cause = engine::failed_response(output);
        // Only an explicit released Docker CLI missing-image error establishes absence.
        let missing = String::from_utf8_lossy(&output.stderr).contains("No such image:");
        return Err(error(
            reference,
            if missing {
                format!("local image absent: {cause}")
            } else {
                format!("image inspection failed: {cause}")
            },
        ));
    }
    let mut image: Image = serde_json::from_slice(&output.stdout)
        .map_err(|_| error(reference, "malformed or incomplete image JSON"))?;
    if !valid_id(&image.id) {
        return Err(error(reference, "missing/invalid immutable image ID"));
    }
    if image.os != "linux" || image.architecture != "amd64" {
        return Err(error(
            reference,
            format!(
                "incompatible image platform {}/{}; require linux/amd64",
                image.os, image.architecture
            ),
        ));
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
            "Specify a usable pre-existing local Docker image",
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
    fn local_image_requires_immutable_identity_and_platform() {
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
