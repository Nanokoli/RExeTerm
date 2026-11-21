//! Directory opt-out for the calling directory and daemon storage.
use anyhow::Result;
use std::{
    fmt, fs, io,
    path::{Component, Path, PathBuf},
};
pub const MARKER: &str = ".rexeterm.disable";
#[derive(Debug)]
pub enum Disabled {
    CurrentDirectory,
    Storage {
        marker: PathBuf,
        option: &'static str,
    },
}
impl fmt::Display for Disabled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory => f.write_str("Exit due to file [.rexeterm.disable]. Run in your project root, or set cwd to it, instead of the shared workspace."),
            Self::Storage { marker, option } => write!(
                f,
                "Exit due to file [{}]. Choose a project directory for {option}.",
                marker.display()
            ),
        }
    }
}
impl std::error::Error for Disabled {}
pub fn check(directory: &Path) -> Result<()> {
    if has_marker(&directory.join(MARKER))? {
        return Err(Disabled::CurrentDirectory.into());
    }
    Ok(())
}
fn has_marker(marker: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(marker) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn check_registry(path: &Path) -> Result<()> {
    check_storage(path, "--registry-dir")
}

pub fn check_state(path: &Path) -> Result<()> {
    check_storage(path, "--config-dir")
}

fn check_storage(path: &Path, option: &'static str) -> Result<()> {
    let absolute = std::path::absolute(path)?;
    let resolved = resolve_storage(&absolute)?;
    check_storage_markers(&resolved, option)?;
    if absolute != resolved {
        check_storage_markers(&absolute, option)?;
    }
    Ok(())
}

fn check_storage_markers(path: &Path, option: &'static str) -> Result<()> {
    // A marker covers this directory and storage directly below it, including
    // deeper paths within its RexETerm directories. Other subprojects are independent.
    let mut inside_rexeterm = false;
    for (depth, directory) in path.ancestors().enumerate() {
        if depth < 2 || inside_rexeterm {
            let marker = directory.join(MARKER);
            if has_marker(&marker)? {
                return Err(Disabled::Storage { marker, option }.into());
            }
        }
        inside_rexeterm = matches!(
            directory.file_name().and_then(|name| name.to_str()),
            Some(crate::PROJECT_DIR | crate::REGISTRY_DIR | rexeterm_config::DIRECTORY)
        );
    }
    Ok(())
}

fn resolve_storage(absolute: &Path) -> Result<PathBuf> {
    // Resolve aliases even if the requested storage hasn't been created yet.
    for prefix in absolute.ancestors() {
        match fs::canonicalize(prefix) {
            Ok(mut resolved) => {
                for component in absolute.strip_prefix(prefix)?.components() {
                    if component == Component::ParentDir {
                        resolved.pop();
                    } else {
                        resolved.push(component.as_os_str());
                    }
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "cannot resolve storage directory").into())
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
