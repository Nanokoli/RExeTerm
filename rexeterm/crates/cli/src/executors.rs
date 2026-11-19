//! Project executor entries, independent credential links, and live inventory decoding.
use crate::{ipc_client::IpcClient, preferences};
use anyhow::{ensure, Context, Result};
use rexeterm_appcore::{client::ConnectExecutorOptions, config};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn directory() -> Result<PathBuf> {
    Ok(preferences::directory()?.join("executors"))
}

fn valid_name(name: &str) -> Result<()> {
    ensure!(
        config::portable_name(name),
        "use a portable name of up to 100 letters, digits, dots, hyphens or underscores (no reserved device names)"
    );
    Ok(())
}

pub fn validate_alias(id: &str) -> Result<()> {
    valid_name(id).context("invalid live executor ID")?;
    ensure!(id != "local", "the live executor ID local is reserved");
    Ok(())
}

fn entry_path(id: &str) -> Result<PathBuf> {
    valid_name(id).context("invalid executor ID")?;
    Ok(config::named_executor_directory(&preferences::directory()?, id).join("executor.json"))
}

fn legacy_path(id: &str) -> Result<PathBuf> {
    ensure!(
        !id.is_empty() && !id.contains(['/', '\\', ':']) && !id.contains(".."),
        "invalid executor ID"
    );
    Ok(directory()?.join(format!("{id}.json")))
}

fn read_entry(path: &Path) -> Result<Option<Value>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot read executor entry {}", path.display()))
        }
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "invalid JSON in executor entry {} at {}:{}",
            path.display(),
            error.line(),
            error.column()
        )
    })?;
    ensure!(
        value["id"].as_str().is_some_and(|id| !id.is_empty()),
        "executor entry {} needs an id",
        path.display()
    );
    Ok(Some(value))
}

fn read(id: &str) -> Result<Option<(Value, PathBuf)>> {
    let mut paths = Vec::new();
    if let Ok(path) = entry_path(id) {
        paths.push(path);
    }
    paths.push(legacy_path(id)?);
    for path in paths {
        if let Some(value) = read_entry(&path)? {
            ensure!(
                value["id"] == id,
                "executor ID does not match {}",
                path.display()
            );
            return Ok(Some((value, path)));
        }
    }
    Ok(None)
}

fn load(id: &str) -> Result<(Value, PathBuf)> {
    read(id)?.with_context(|| {
        format!("executor {id} is not saved; use -e {id} executor host add <URL> --root <DIR>")
    })
}

fn public_entry(entry: &Value) -> Value {
    let mut view = json!({});
    for key in ["id", "name", "url", "root"] {
        if let Some(value) = entry[key].as_str() {
            if key != "url" || !value.contains(['@', '?', '#', '\r', '\n']) {
                view[key] = json!(value);
            }
        }
    }
    view["hasProfile"] = json!(entry["connectionProfile"].is_string());
    view
}

fn absolute_profile(entry: &mut Value, base: &Path) -> Result<()> {
    if let Some(value) = entry.get_mut("connectionProfile").filter(|v| !v.is_null()) {
        let path = value
            .as_str()
            .filter(|path| !path.trim().is_empty())
            .context("connectionProfile must be a nonempty file path")?;
        *value = json!(base.join(path));
    }
    Ok(())
}

pub fn save(id: &str, mut entry: Value) -> Result<Value> {
    let path = entry_path(id)?;
    ensure!(
        read(id)?.is_none(),
        "executor {id} is already saved; use executor host edit"
    );
    absolute_profile(&mut entry, &std::env::current_dir()?)?;
    config::write_json(&path, &entry)?;
    Ok(json!({"executor":public_entry(&entry),"path":path}))
}

pub fn list() -> Result<Vec<Value>> {
    let base = directory()?;
    let entries = match fs::read_dir(&base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut rows = std::collections::BTreeMap::new();
    for item in entries {
        let item = item?;
        let path = item.path();
        let is_current = item.file_type()?.is_dir();
        let path = if is_current {
            path.join("executor.json")
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            path
        } else {
            continue;
        };
        if let Some(entry) = read_entry(&path)? {
            let id = entry["id"]
                .as_str()
                .context("executor id missing")?
                .to_owned();
            if is_current || !rows.contains_key(&id) {
                let mut row = public_entry(&entry);
                row["path"] = json!(path);
                rows.insert(id, row);
            }
        }
    }
    Ok(rows.into_values().collect())
}

pub fn list_saved() -> Result<Value> {
    Ok(json!({"directory":directory()?,"executors":list()?}))
}

pub fn edit(id: &str, mut patch: Value) -> Result<Value> {
    let (mut entry, previous) = load(id)?;
    ensure!(patch.is_object(), "the patch must be a JSON object");
    for key in ["authentication", "tls"] {
        ensure!(
            patch[key].is_null(),
            "set credentials in a private profile, then use credential link and --credential"
        );
    }
    let base = previous
        .parent()
        .context("executor entry needs a parent directory")?;
    absolute_profile(&mut entry, base)?;
    config::resolve_tls_paths(&mut entry, base)?;
    absolute_profile(&mut patch, &std::env::current_dir()?)?;
    let target = entry
        .as_object_mut()
        .context("executor entry must be an object")?;
    for (key, value) in patch
        .as_object()
        .context("the patch must be a JSON object")?
    {
        if value.is_null() {
            target.remove(key);
        } else {
            target.insert(key.clone(), value.clone());
        }
    }
    target.insert("id".into(), json!(id));
    let path = entry_path(id).unwrap_or(previous);
    config::write_json(&path, &entry)?;
    Ok(json!({"executor":public_entry(&entry),"path":path}))
}

pub fn remove(id: &str) -> Result<Value> {
    // A legacy file must not reappear if a newer entry shadowed it.
    for path in [Some(legacy_path(id)?), entry_path(id).ok()]
        .into_iter()
        .flatten()
    {
        if let Some(entry) = read_entry(&path)? {
            ensure!(
                entry["id"] == id,
                "executor ID does not match {}",
                path.display()
            );
        }
        remove_file(&path)?;
    }
    Ok(json!({"removed":id}))
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
    }
}

fn credentials_directory() -> Result<PathBuf> {
    Ok(preferences::directory()?.join("credentials"))
}

fn credential_path(name: &str) -> Result<PathBuf> {
    valid_name(name).context("invalid credential name")?;
    Ok(credentials_directory()?.join(format!("{name}.json")))
}

pub fn list_credentials() -> Result<Value> {
    let directory = credentials_directory()?;
    let mut entries = Vec::new();
    match fs::read_dir(&directory) {
        Ok(items) => {
            for item in items {
                let path = item?.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    if let Some(name) = path.file_stem().and_then(|name| name.to_str()) {
                        let source = fs::read_link(&path).ok();
                        entries.push(json!({"name":name,"path":path,"available":path.is_file(),"source":source}));
                    }
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    entries.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({"directory":directory,"credentials":entries}))
}

pub fn link_credential(name: &str, source: &Path) -> Result<Value> {
    let path = credential_path(name)?;
    let source = std::path::absolute(source)?;
    config::load_connection_profile(&source)?;
    config::private_directory(&credentials_directory()?)?;
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(&source, &path);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(&source, &path);
    result.with_context(|| format!("cannot link credential {name} at {}", path.display()))?;
    Ok(json!({"name":name,"path":path,"linked":true}))
}

pub fn remove_credential(name: &str) -> Result<Value> {
    let path = credential_path(name)?;
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_symlink(),
        "{} is a profile file; remove it manually if intended",
        path.display()
    );
    remove_file(&path)?;
    Ok(json!({"removed":name}))
}

/// A named credential contributes only login and TLS values. The saved entry
/// supplies the address and the default live ID (the hostname from the CLI).
pub fn connection(id: &str, credential: Option<&str>) -> Result<Value> {
    let (mut entry, path) = load(id)?;
    let base = path
        .parent()
        .context("executor entry needs a parent directory")?;
    let mut params = if let Some(name) = credential {
        let selected = credential_path(name)?;
        let profile = config::load_connection_profile(&selected).with_context(|| {
            format!("cannot use credential {name}; see credential list or credential link")
        })?;
        ensure!(
            !profile["authentication"].is_null() || !profile["tls"].is_null(),
            "credential {name} has no authentication or tls options"
        );
        let fields = ["username", "authentication", "tls"];
        let target = entry
            .as_object_mut()
            .context("executor entry must be an object")?;
        for key in fields {
            target.remove(key);
            if let Some(value) = profile.get(key) {
                target.insert(key.to_owned(), value.clone());
            }
        }
        entry
    } else {
        absolute_profile(&mut entry, base)?;
        config::resolve_tls_paths(&mut entry, base)?;
        expand_profile(entry)?
    };
    params["id"] = json!(id);
    validate_connection(params)
        .with_context(|| format!("cannot connect using executor entry {}", path.display()))
}

pub fn options_file(path: &Path, explicit_id: Option<&str>) -> Result<Value> {
    let mut params = config::load_connection_profile(path)?;
    // The options file may be a full profile from HostNet; its own metadata is
    // not a second profile reference.
    if let Some(id) = explicit_id {
        params["id"] = json!(id);
    }
    validate_connection(params)
        .with_context(|| format!("invalid connection options in {}", path.display()))
}

fn expand_profile(entry: Value) -> Result<Value> {
    let Some(path) = entry["connectionProfile"].as_str() else {
        return Ok(entry);
    };
    let mut profile = config::load_connection_profile(Path::new(path))?;
    let fields = profile
        .as_object_mut()
        .context("profile must be an object")?;
    for (key, value) in entry
        .as_object()
        .context("executor entry must be an object")?
    {
        if !value.is_null() {
            fields.insert(key.clone(), value.clone());
        }
    }
    Ok(profile)
}

fn validate_connection(mut params: Value) -> Result<Value> {
    for key in ["id", "url"] {
        ensure!(
            params[key]
                .as_str()
                .is_some_and(|value| !value.trim().is_empty()),
            "connection options require a nonempty {key} (use -e to supply id)"
        );
    }
    let fields = params
        .as_object_mut()
        .context("connection options must be an object")?;
    fields.remove("connectionProfile");
    if !params["authentication"].is_null() {
        ensure!(
            params["username"]
                .as_str()
                .is_some_and(|user| !user.trim().is_empty()),
            "authentication requires a username in the selected profile"
        );
    }
    // Use the SDK input contract without echoing malformed values (which may be secrets).
    let parsed: ConnectExecutorOptions = serde_json::from_value(params.clone())
        .map_err(|_| anyhow::anyhow!("check the SDK options fields: username, authentication, tls, links, channels and labels"))?;
    for url in std::iter::once(&parsed.url).chain(&parsed.links) {
        ensure!(
            !url.contains(['@', '?', '#', '\r', '\n']),
            "connection URLs cannot contain credentials, queries or fragments"
        );
        ensure!(
            parsed.authentication.is_none()
                || url.starts_with("tls://")
                || url.starts_with("wss://"),
            "authentication requires tls:// or wss:// links"
        );
    }
    if let Some(tls) = &parsed.tls {
        ensure!(
            tls.certificate_file.is_some() == tls.private_key_file.is_some(),
            "tls.certificateFile and tls.privateKeyFile must be configured together"
        );
        for (field, path) in [
            ("caFile", &tls.ca_file),
            ("certificateFile", &tls.certificate_file),
            ("privateKeyFile", &tls.private_key_file),
        ] {
            if let Some(path) = path {
                ensure!(
                    path.is_file(),
                    "tls.{field} is missing or not a file: {}",
                    path.display()
                );
            }
        }
    }
    if matches!(
        parsed.authentication,
        Some(
            rexeterm_appcore::client::auth::AuthenticationOptions::Certificate
                | rexeterm_appcore::client::auth::AuthenticationOptions::Ca
        )
    ) {
        ensure!(
            parsed.tls.is_some_and(|tls| tls.certificate_file.is_some()),
            "certificate authentication requires tls.certificateFile and tls.privateKeyFile"
        );
    }
    Ok(params)
}

#[derive(Default)]
pub struct Inventory {
    pub default: Option<String>,
    pub executors: Vec<Value>,
}

impl Inventory {
    pub async fn live(client: &IpcClient) -> Result<Self> {
        let reply = client
            .call("rpc", json!({"tool":"list_executor","params":{}}))
            .await?;
        let fields = reply
            .get("metadata")
            .filter(|value| value.get("executors").is_some())
            .unwrap_or(&reply);
        Ok(Self {
            default: fields["default"].as_str().map(str::to_owned),
            executors: fields["executors"]
                .as_array()
                .cloned()
                .context("unexpected list_executor result; expected metadata.executors")?,
        })
    }
    pub fn find(&self, id: &str) -> Option<&Value> {
        self.executors.iter().find(|executor| executor["id"] == id)
    }
    pub fn with_saved(mut self) -> Result<Vec<Value>> {
        for executor in &mut self.executors {
            executor["connected"] = json!(true);
        }
        for mut entry in list()? {
            if let Some(id) = entry["id"].as_str() {
                if self.find(id).is_none() {
                    entry["connected"] = json!(false);
                    self.executors.push(entry);
                }
            }
        }
        Ok(self.executors)
    }
}
