use super::schema::ClientSettings;
use crate::protocol::shell::{ShellCommand, ShellSettings};
use anyhow::{anyhow, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct ClientSettingsStore {
    path: Option<PathBuf>,
    state: Arc<Mutex<State>>,
}
struct State {
    settings: ClientSettings,
    reload_error: Option<String>,
}
impl Default for ClientSettingsStore {
    fn default() -> Self {
        Self::memory(ClientSettings::default()).expect("built-in client settings are valid")
    }
}
impl ClientSettingsStore {
    pub fn memory(settings: ClientSettings) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            path: None,
            state: Arc::new(Mutex::new(State {
                settings,
                reload_error: None,
            })),
        })
    }
    pub fn load(path: PathBuf) -> Result<Self> {
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        };
        let settings = read(&path)?;
        if path.is_file() && !settings.executors.is_empty() {
            write(&path, &settings)?;
        }
        Ok(Self {
            path: Some(path),
            state: Arc::new(Mutex::new(State {
                settings,
                reload_error: None,
            })),
        })
    }
    pub fn default_path() -> Result<PathBuf> {
        Ok(crate::config::beside_executable()?.join("preferences.json"))
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub fn settings(&self) -> ClientSettings {
        self.state.lock().unwrap().settings.clone()
    }
    pub fn effective(&self, executor: &str, directory: Option<&str>) -> ShellSettings {
        self.state
            .lock()
            .unwrap()
            .settings
            .effective(executor, directory)
    }
    pub fn reload_error(&self) -> Option<String> {
        self.state.lock().unwrap().reload_error.clone()
    }
    pub fn reload(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let Some(path) = &self.path else {
            state.reload_error = None;
            return Ok(());
        };
        match read(path) {
            Ok(settings) => {
                state.settings = settings;
                state.reload_error = None;
                Ok(())
            }
            Err(error) => {
                state.reload_error = Some(error.to_string());
                Err(error)
            }
        }
    }
    pub fn set_default_shell(
        &self,
        executor: &str,
        directory: Option<&str>,
        shell: &str,
    ) -> Result<()> {
        let shell = shell.trim();
        if shell.is_empty() {
            return Err(anyhow!("shell is required"));
        }
        let mut state = self.state.lock().unwrap();
        let mut next = state.settings.clone();
        if executor.is_empty() {
            if directory.is_some() {
                return Err(anyhow!(
                    "an executor ID is required for directory shell settings"
                ));
            }
            next.shells.default = shell.to_string();
        } else {
            let target = next.executors.entry(executor.to_string()).or_default();
            let overlay = if let Some(directory) = directory {
                target.directories.entry(directory.to_string()).or_default()
            } else {
                &mut target.shells
            };
            overlay.default = Some(shell.to_string());
        }
        next.validate()?;
        if let Some(path) = &self.path {
            write(path, &next)?;
        }
        state.settings = next;
        state.reload_error = None;
        Ok(())
    }
    /// Add or replace a shell profile; `executor` empty means the global layer.
    pub fn set_shell_profile(
        &self,
        executor: &str,
        directory: Option<&str>,
        name: &str,
        program: &str,
        command_args: Vec<String>,
        interactive_args: Vec<String>,
    ) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow!("shell profile name is required"));
        }
        let program = program.trim();
        if program.is_empty() {
            return Err(anyhow!("shell profile `{name}` needs a program"));
        }
        let profile = crate::protocol::shell::ShellProfile {
            candidates: vec![program.to_string()],
            command_args,
            interactive_args,
        };
        let mut state = self.state.lock().unwrap();
        let mut next = state.settings.clone();
        if executor.is_empty() {
            if directory.is_some() {
                return Err(anyhow!(
                    "an executor ID is required for directory shell settings"
                ));
            }
            next.shells.profiles.insert(name.to_string(), profile);
        } else {
            let target = next.executors.entry(executor.to_string()).or_default();
            let overlay = if let Some(directory) = directory {
                target.directories.entry(directory.to_string()).or_default()
            } else {
                &mut target.shells
            };
            overlay.profiles.insert(name.to_string(), profile);
        }
        next.validate()?;
        if let Some(path) = &self.path {
            write(path, &next)?;
        }
        state.settings = next;
        state.reload_error = None;
        Ok(())
    }

    pub fn shell_command(
        &self,
        executor: &str,
        directory: Option<&str>,
        requested: Option<&str>,
        system: Option<&str>,
        command: &str,
    ) -> Result<ShellCommand> {
        let shells = self.effective(executor, directory);
        let selected = requested
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&shells.default);
        let name = if selected.eq_ignore_ascii_case("auto") {
            match system {
                Some("windows") => "powershell",
                Some(_) => "bash",
                None => {
                    return Err(anyhow!(
                        "executor system is unknown; choose an explicit shell profile"
                    ))
                }
            }
        } else {
            selected
        };
        let profile = shells.profiles.get(name).ok_or_else(|| {
            anyhow!("shell profile `{name}` is not configured in client settings")
        })?;
        let mut args: Vec<_> = profile
            .command_args
            .iter()
            .map(|arg| arg.replace("{command}", command))
            .collect();
        if !profile
            .command_args
            .iter()
            .any(|arg| arg.contains("{command}"))
        {
            args.push(command.to_string());
        }
        Ok(ShellCommand {
            profile: name.to_string(),
            candidates: profile.candidates.clone(),
            args,
        })
    }
}
fn read(path: &Path) -> Result<ClientSettings> {
    let mut settings: ClientSettings = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse client settings {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ClientSettings::default(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read client settings {}", path.display()))
        }
    };
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let directories = root.join("executors");
    if directories.is_dir() {
        for directory in fs::read_dir(directories)? {
            let file = directory?.path().join("preferences.json");
            if !file.is_file() {
                continue;
            }
            let document: serde_json::Value = serde_json::from_slice(&fs::read(&file)?)?;
            let id = document["executorId"]
                .as_str()
                .ok_or_else(|| anyhow!("executor preferences missing executorId"))?;
            anyhow::ensure!(
                file == crate::config::executor_directory(root, id).join("preferences.json"),
                "executor preferences path mismatch"
            );
            settings.executors.insert(
                id.into(),
                serde_json::from_value(document["settings"].clone())?,
            );
        }
    }
    settings.validate()?;
    Ok(settings)
}
fn write(path: &Path, settings: &ClientSettings) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    anyhow::ensure!(!path.is_dir(), "client settings path is a directory");
    for (id, target) in &settings.executors {
        let directory = crate::config::executor_directory(parent, id);
        crate::config::write_json(
            &directory.join("preferences.json"),
            &serde_json::json!({"executorId":id,"settings":target}),
        )?;
    }
    let mut global = settings.clone();
    global.executors.clear();
    crate::config::write_json(path, &global)
}
