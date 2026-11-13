//! Local directory opt-out. This check precedes registry/config/network access.
use anyhow::Result;
use std::{fmt, path::Path};
pub const MARKER: &str = ".rexeterm.disable";
#[derive(Debug)]
pub struct Disabled;
impl fmt::Display for Disabled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Exit due to file [.rexeterm.disable]. Run in your project root, or set cwd to it, instead of the shared workspace.")
    }
}
impl std::error::Error for Disabled {}
pub fn check(directory: &Path) -> Result<()> {
    match std::fs::symlink_metadata(directory.join(MARKER)) {
        Ok(_) => Err(Disabled.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub fn set(directory: &Path, disabled: bool) -> Result<serde_json::Value> {
    let marker = directory.join(MARKER);
    if disabled {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    } else {
        match std::fs::remove_file(&marker) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(serde_json::json!({"directory":directory,"disabled":disabled,"marker":MARKER}))
}
