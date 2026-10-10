use std::fmt;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Usage,
    Operational,
}

#[derive(Debug)]
pub struct Error {
    pub kind: Kind,
    pub operation: &'static str,
    pub context: String,
    pub cause: String,
    pub remedy: String,
}

impl Error {
    pub fn operational(
        operation: &'static str,
        context: impl Into<String>,
        cause: impl fmt::Display,
        remedy: impl Into<String>,
    ) -> Self {
        Self {
            kind: Kind::Operational,
            operation,
            context: context.into(),
            cause: cause.to_string(),
            remedy: remedy.into(),
        }
    }

    pub fn status(&self) -> i32 {
        match self.kind {
            Kind::Usage => 2,
            Kind::Operational => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}): {}. {}",
            self.operation, self.context, self.cause, self.remedy
        )
    }
}

impl std::error::Error for Error {}

pub fn diagnostic(message: impl fmt::Display) {
    use std::io::Write;
    // A closed stderr is not permission to panic or replace a real child status.
    let _ = writeln!(std::io::stderr().lock(), "larainit: {message}");
}

/// Numeric child codes pass through; signal-only status maps to 128 + signal.
pub fn child_status(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_and_actionable_errors() {
        for code in [0, 42, 125, 126, 127] {
            assert_eq!(child_status(ExitStatus::from_raw(code << 8)), code);
        }
        for signal in [2, 9, 15] {
            assert_eq!(child_status(ExitStatus::from_raw(signal)), 128 + signal);
        }
        let mut error = Error::operational(
            "inspect image",
            "Docker image test:tag",
            "access denied",
            "Check Docker access",
        );
        assert_eq!(error.status(), 1);
        assert!(error.to_string().contains("access denied"));
        assert!(error.to_string().contains("Check Docker access"));
        error.kind = Kind::Usage;
        assert_eq!(error.status(), 2);
    }
}
