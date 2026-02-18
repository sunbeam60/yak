use std::fmt;

/// CLI error type — carries a human-readable message printed once in main().
pub struct CliError {
    pub message: String,
}

impl CliError {
    pub fn new(msg: impl Into<String>) -> Self {
        CliError {
            message: msg.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<yak::YakError> for CliError {
    fn from(e: yak::YakError) -> Self {
        CliError {
            message: e.to_string(),
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError {
            message: e.to_string(),
        }
    }
}

/// All command functions return this type.
pub type CliResult = Result<(), CliError>;
