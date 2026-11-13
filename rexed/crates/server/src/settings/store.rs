//! Immutable startup defaults for standalone terminals and direct legacy calls.
//! Client preferences, directory overrides and reloads belong to Client.
use super::{
    schema::{auto_string, default_profiles, ReSettings},
    shell::{resolve_profile_name, ShellResolution, ShellResolver},
};
use anyhow::{anyhow, Context, Result};
use remote_executor_terminal::CommandSpec;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
const SETTINGS_FILE: &str = ".re-setting.json";
#[derive(Clone)]
pub struct SettingsStore {
    path: PathBuf,
    root: PathBuf,
    settings: Arc<ReSettings>,
}
impl SettingsStore {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = absolutize(path.unwrap_or_else(default_settings_path))?;
        let settings: ReSettings = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("failed to parse startup settings {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ReSettings::default(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read startup settings {}", path.display()))
            }
        };
        let store = Self::from_settings(path, settings);
        for (field, value) in [
            ("default", &store.settings.shells.default),
            ("interactive", &store.settings.shells.interactive),
        ] {
            let name = resolve_profile_name(value);
            if !store.settings.shells.profiles.contains_key(&name) {
                return Err(anyhow!(
                    "shells.{field} references missing profile `{name}` in {}",
                    store.path.display()
                ));
            }
        }
        Ok(store)
    }
    pub fn load_default_lossy() -> Self {
        Self::load(None)
            .unwrap_or_else(|_| Self::from_settings(default_settings_path(), ReSettings::default()))
    }
    pub fn from_settings(path: PathBuf, mut settings: ReSettings) -> Self {
        let path = absolutize(path).unwrap_or_else(|_| PathBuf::from(SETTINGS_FILE));
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        for (name, profile) in default_profiles() {
            settings.shells.profiles.entry(name).or_insert(profile);
        }
        if settings.shells.default.trim().is_empty() {
            settings.shells.default = auto_string();
        }
        if settings.shells.interactive.trim().is_empty() {
            settings.shells.interactive = auto_string();
        }
        Self {
            path,
            root,
            settings: Arc::new(settings),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn settings(&self) -> Result<ReSettings> {
        Ok((*self.settings).clone())
    }
    pub fn command_spec(
        &self,
        shell: Option<&str>,
        command: &str,
        cwd: &Path,
    ) -> Result<CommandSpec> {
        let resolution =
            self.resolve_shell(shell.unwrap_or_default(), Some(command), false, Some(cwd))?;
        Ok(CommandSpec::new(resolution.program)
            .args(resolution.args.iter().map(String::as_str))
            .cwd(cwd.to_path_buf()))
    }
    pub fn interactive_command_spec(&self) -> Result<CommandSpec> {
        let resolution = self.resolve_shell("", None, true, None)?;
        Ok(CommandSpec::new(resolution.program).args(resolution.args.iter().map(String::as_str)))
    }
    fn resolve_shell(
        &self,
        requested: &str,
        command: Option<&str>,
        interactive: bool,
        search_root: Option<&Path>,
    ) -> Result<ShellResolution> {
        ShellResolver {
            settings: &self.settings.shells,
            path: &self.path,
            root: &self.root,
        }
        .resolve(requested, command, interactive, search_root)
    }
}
fn default_settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(SETTINGS_FILE)))
        .unwrap_or_else(|| PathBuf::from(SETTINGS_FILE))
}
fn absolutize(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
