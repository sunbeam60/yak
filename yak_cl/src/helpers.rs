use std::io::Write;

use yak::{OpenMode, YakDefault as Yak, YakError};

use crate::error::{CliError, CliResult};

/// Default block index width: 4 bytes (32-bit block addresses)
pub const DEFAULT_BLOCK_INDEX_WIDTH: u8 = 4;
/// Default block size shift: 12 (2^12 = 4096-byte blocks)
pub const DEFAULT_BLOCK_SIZE_SHIFT: u8 = 12;

/// Environment variable for dev/test convenience.
/// If set, its value is used as the password instead of prompting interactively.
/// NOT intended for production use.
const PASSWORD_ENV_VAR: &str = "YAK_PASSWORD";

/// Validate that `args` has exactly `expected` elements.
pub fn expect_args(args: &[String], expected: usize, usage: &str) -> Result<(), CliError> {
    if args.len() != expected {
        Err(CliError::new(format!("Usage: {}", usage)))
    } else {
        Ok(())
    }
}

/// Validate that `args.len()` is within [min, max].
pub fn expect_args_range(
    args: &[String],
    min: usize,
    max: usize,
    usage: &str,
) -> Result<(), CliError> {
    if args.len() < min || args.len() > max {
        Err(CliError::new(format!("Usage: {}", usage)))
    } else {
        Ok(())
    }
}

/// Resolve a password for opening an encrypted file.
///
/// 1. If `YAK_PASSWORD` env var is set, use its value.
/// 2. Otherwise, prompt interactively via rpassword.
pub fn resolve_password() -> Result<String, CliError> {
    if let Ok(pw) = std::env::var(PASSWORD_ENV_VAR) {
        return Ok(pw);
    }
    eprint!("Password: ");
    std::io::stderr().flush().ok();
    rpassword::read_password().map_err(|e| CliError::new(format!("Error reading password: {}", e)))
}

/// Resolve a password for creating an encrypted file.
///
/// 1. If `YAK_PASSWORD` env var is set, use its value (no confirmation).
/// 2. Otherwise, prompt interactively twice for confirmation.
pub fn resolve_password_create() -> Result<String, CliError> {
    if let Ok(pw) = std::env::var(PASSWORD_ENV_VAR) {
        return Ok(pw);
    }
    eprint!("Password: ");
    std::io::stderr().flush().ok();
    let pw1 = rpassword::read_password()
        .map_err(|e| CliError::new(format!("Error reading password: {}", e)))?;
    eprint!("Confirm password: ");
    std::io::stderr().flush().ok();
    let pw2 = rpassword::read_password()
        .map_err(|e| CliError::new(format!("Error reading password: {}", e)))?;
    if pw1 != pw2 {
        return Err(CliError::new("Passwords do not match"));
    }
    Ok(pw1)
}

/// Open an existing Yak file. If the file is encrypted, automatically
/// resolves the password (via env var or interactive prompt) and retries.
pub fn open_yak(path: &str, mode: OpenMode) -> Result<Yak, CliError> {
    match Yak::open(path, mode) {
        Ok(yak) => Ok(yak),
        Err(YakError::EncryptionRequired(_)) => {
            // File is encrypted — resolve password and retry
            let password = resolve_password()?;
            Yak::open_encrypted(path, mode, password.as_bytes())
                .map_err(|e| CliError::new(format!("Error opening encrypted Yak file: {}", e)))
        }
        Err(e) => Err(CliError::new(format!("Error opening Yak file: {}", e))),
    }
}

/// Close a Yak file.
pub fn close_yak(yak: Yak) -> CliResult {
    yak.close()
        .map_err(|e| CliError::new(format!("Error closing Yak file: {}", e)))
}

/// Open a stream, read its entire contents, close the stream.
pub fn read_stream_fully(sfs: &Yak, stream_path: &str) -> Result<Vec<u8>, CliError> {
    let handle = sfs
        .open_stream(stream_path, OpenMode::Read)
        .map_err(|e| CliError::new(format!("Error opening stream '{}': {}", stream_path, e)))?;

    let length = match sfs.stream_length(&handle) {
        Ok(l) => l as usize,
        Err(e) => {
            let _ = sfs.close_stream(handle);
            return Err(CliError::new(format!("Error getting stream length: {}", e)));
        }
    };

    let mut buffer = vec![0u8; length];
    match sfs.read(&handle, &mut buffer) {
        Ok(n) => buffer.truncate(n),
        Err(e) => {
            let _ = sfs.close_stream(handle);
            return Err(CliError::new(format!("Error reading stream: {}", e)));
        }
    }

    sfs.close_stream(handle)
        .map_err(|e| CliError::new(format!("Error closing stream: {}", e)))?;

    Ok(buffer)
}
