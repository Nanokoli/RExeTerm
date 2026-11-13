use crate::protocol::shell::{default_profiles, ShellProfile, ShellSettings};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientSettings {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub shells: ShellSettings,
    #[serde(default)]
    pub executors: BTreeMap<String, ExecutorSettings>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutorSettings {
    #[serde(default)]
    pub shells: ShellOverrides,
    #[serde(default)]
    pub directories: BTreeMap<String, ShellOverrides>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShellOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, ShellProfile>,
}
impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            version: version(),
            shells: ShellSettings::default(),
            executors: BTreeMap::new(),
        }
    }
}
fn version() -> u32 {
    1
}
impl ClientSettings {
    pub(super) fn effective(&self, executor: &str, directory: Option<&str>) -> ShellSettings {
        let mut shells = self.shells.clone();
        for (name, profile) in default_profiles() {
            shells.profiles.entry(name).or_insert(profile);
        }
        if let Some(target) = self.executors.get(executor) {
            apply(&mut shells, &target.shells);
            if let Some(overlay) = directory.and_then(|key| target.directories.get(key)) {
                apply(&mut shells, overlay);
            }
        }
        shells
    }
    pub(super) fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(anyhow!(
                "unsupported client settings version: {}",
                self.version
            ));
        }
        validate_shells(&self.effective("", None))?;
        for (id, target) in &self.executors {
            validate_shells(&self.effective(id, None))?;
            for directory in target.directories.keys() {
                validate_shells(&self.effective(id, Some(directory)))?;
            }
        }
        Ok(())
    }
}
fn apply(shells: &mut ShellSettings, overlay: &ShellOverrides) {
    if let Some(value) = &overlay.default {
        shells.default = value.clone();
    }
    if let Some(value) = &overlay.interactive {
        shells.interactive = value.clone();
    }
    shells.profiles.extend(overlay.profiles.clone());
}
fn validate_shells(shells: &ShellSettings) -> Result<()> {
    for name in [&shells.default, &shells.interactive] {
        if name.trim().is_empty()
            || (!name.eq_ignore_ascii_case("auto") && !shells.profiles.contains_key(name))
        {
            return Err(anyhow!(
                "shell profile `{name}` is not configured in client settings"
            ));
        }
    }
    for (name, profile) in &shells.profiles {
        if profile.candidates.is_empty()
            || profile
                .candidates
                .iter()
                .any(|candidate| candidate.trim().is_empty())
        {
            return Err(anyhow!(
                "shell profile `{name}` requires nonempty candidates"
            ));
        }
    }
    Ok(())
}
