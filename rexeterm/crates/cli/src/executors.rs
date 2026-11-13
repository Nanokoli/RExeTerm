//! Saved executor entries live in the project:
//! `<cwd>/.rexeterm/config-dir/executors/<id>.json`.
//!
//! They are client data: adding, editing or removing one never needs a host, and
//! the host only sees the entry when `executor connect` hands it over.
use crate::preferences;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

fn directory() -> Result<PathBuf> {
    Ok(preferences::directory()?.join("executors"))
}

fn path(id: &str) -> Result<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
        bail!("invalid executor id: {id}");
    }
    Ok(directory()?.join(format!("{id}.json")))
}

pub fn read(id: &str) -> Result<Option<Value>> {
    let path = path(id)?;
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    Ok(Some(serde_json::from_str(&text).with_context(|| {
        format!("cannot parse {}", path.display())
    })?))
}

/// Read one entry, or explain how to create it.
pub fn load(id: &str) -> Result<Value> {
    read(id)?.with_context(|| format!("executor {id} is not saved; run executor host add {id}"))
}

fn write(id: &str, entry: &Value) -> Result<PathBuf> {
    let path = path(id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_vec_pretty(entry)?)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(path)
}

pub fn save(id: &str, entry: Value) -> Result<Value> {
    let path = write(id, &entry)?;
    Ok(json!({"executor": entry, "path": path.display().to_string()}))
}

pub fn list() -> Result<Value> {
    let directory = directory()?;
    let mut entries = Vec::new();
    if directory.is_dir() {
        for item in fs::read_dir(&directory)? {
            let path = item?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                entries.push(value);
            }
        }
    }
    entries.sort_by(|left, right| {
        left["id"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["id"].as_str().unwrap_or_default())
    });
    Ok(json!({"executors": entries}))
}

pub fn edit(id: &str, patch: Value) -> Result<Value> {
    let mut entry = load(id)?;
    let (Some(target), Some(source)) = (entry.as_object_mut(), patch.as_object()) else {
        bail!("the patch must be a JSON object");
    };
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
    target.insert("id".into(), json!(id));
    write(id, &entry)?;
    Ok(json!({"executor": entry}))
}

pub fn remove(id: &str) -> Result<Value> {
    let path = path(id)?;
    if path.is_file() {
        fs::remove_file(&path)
            .with_context(|| format!("cannot remove {}", path.display()))?;
    }
    Ok(json!({"removed": id}))
}
