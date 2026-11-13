//! Shell profile/candidate resolution, independent of store locks and tool calls.
use super::schema::{ShellProfile, ShellSettings};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
const COMMAND_PLACEHOLDER: &str = "{command}";
#[derive(Clone, Debug, Serialize)]
pub struct ShellResolution {
    pub requested: String,
    pub profile: String,
    pub program: String,
    pub args: Vec<String>,
    #[serde(rename = "settingsPath")]
    pub settings_path: String,
}

pub(super) struct ShellResolver<'a> {
    pub(super) settings: &'a ShellSettings,
    pub(super) path: &'a Path,
    pub(super) root: &'a Path,
}

impl ShellResolver<'_> {
    pub(super) fn resolve(
        &self,
        requested: &str,
        command: Option<&str>,
        interactive: bool,
        search_root: Option<&Path>,
    ) -> Result<ShellResolution> {
        let requested = requested.trim();
        let configured = if requested.is_empty() {
            if interactive && !self.settings.interactive.trim().is_empty() {
                self.settings.interactive.as_str()
            } else {
                self.settings.default.as_str()
            }
        } else {
            requested
        };
        let profile_name = resolve_profile_name(configured);
        let profile = self.settings.profiles.get(&profile_name).ok_or_else(|| {
            anyhow!(
                "shell profile `{profile_name}` is not configured in {}",
                self.path.display()
            )
        })?;
        let program = self.resolve_candidate(&profile_name, profile, search_root)?;
        let args = if interactive {
            profile.interactive_args.clone()
        } else {
            expand_command_args(&profile.command_args, command.unwrap_or_default())
        };
        Ok(ShellResolution {
            requested: if requested.is_empty() {
                configured.to_string()
            } else {
                requested.to_string()
            },
            profile: profile_name,
            program,
            args,
            settings_path: self.path.to_string_lossy().into_owned(),
        })
    }

    fn resolve_candidate(
        &self,
        profile_name: &str,
        profile: &ShellProfile,
        search_root: Option<&Path>,
    ) -> Result<String> {
        for candidate in &profile.candidates {
            if let Some(search_root) = search_root {
                if let Some(program) = resolve_program(candidate, search_root) {
                    return Ok(program);
                }
            }
            if let Some(program) = resolve_program(candidate, self.root) {
                return Ok(program);
            }
        }
        Err(anyhow!(
            "no candidate found for shell profile `{profile_name}`; checked: {}",
            profile.candidates.join(", ")
        ))
    }
}
pub(super) fn resolve_profile_name(name: &str) -> String {
    if name.trim().is_empty() || name.eq_ignore_ascii_case("auto") {
        if cfg!(windows) {
            "powershell".to_string()
        } else {
            "bash".to_string()
        }
    } else {
        name.to_string()
    }
}

fn expand_command_args(args: &[String], command: &str) -> Vec<String> {
    if args.iter().any(|arg| arg.contains(COMMAND_PLACEHOLDER)) {
        args.iter()
            .map(|arg| arg.replace(COMMAND_PLACEHOLDER, command))
            .collect()
    } else {
        let mut args = args.to_vec();
        args.push(command.to_string());
        args
    }
}

pub(crate) fn resolve_program(candidate: &str, root: &Path) -> Option<String> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return None;
    }
    let path = PathBuf::from(candidate);
    if path.is_absolute() || has_path_separator(candidate) {
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        return path.exists().then(|| path.to_string_lossy().into_owned());
    }
    find_in_path(candidate).map(|_| candidate.to_string())
}

fn has_path_separator(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for name in executable_names(program) {
            let path = dir.join(name);
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

fn executable_names(program: &str) -> Vec<String> {
    if !cfg!(windows) || Path::new(program).extension().is_some() {
        return vec![program.to_string()];
    }
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut names = vec![program.to_string()];
    names.extend(
        pathext
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(|ext| format!("{program}{ext}")),
    );
    names
}
