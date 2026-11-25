//! Saved connection configs combine a host, a credential and an optional upstream config.
use super::{
    credential_path, directory, read_entry, valid_name, validate_alias, CREDENTIAL_FIELDS,
};
use crate::preferences;
use anyhow::{ensure, Context, Result};
use rexeterm_appcore::config;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

const MAX_HOPS: usize = 8;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    id: String,
    executor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential: Option<String>,
    start_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    via: Option<String>,
}

impl Config {
    fn parse(input: Value) -> Result<Self> {
        let row: Self = serde_json::from_value(input).map_err(|_| {
            anyhow::anyhow!("expected id, executor and startPath, with optional credential and via")
        })?;
        validate_alias(&row.id)?;
        valid_name(&row.executor).context("invalid executor name")?;
        if let Some(name) = &row.credential {
            valid_name(name).context("invalid credential name")?;
        }
        if let Some(via) = &row.via {
            validate_alias(via).context("invalid jump config ID")?;
        }
        let p = row.start_path.as_bytes();
        let absolute = matches!(p, [b'/', ..] | [b'\\', b'\\', ..])
            || matches!(p, [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic());
        ensure!(
            absolute && !p.contains(&0),
            "startPath must be an absolute POSIX or Windows path"
        );
        Ok(row)
    }
}

fn entries(base: &Path) -> Result<Vec<fs::DirEntry>> {
    match fs::read_dir(base) {
        Ok(entries) => Ok(entries.collect::<std::io::Result<_>>()?),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn config_directory() -> Result<PathBuf> {
    Ok(preferences::directory()?.join("configs"))
}

fn config_path(id: &str) -> Result<PathBuf> {
    validate_alias(id)?;
    Ok(config_directory()?.join(format!("{id}.json")))
}

fn read_config(path: &Path, id: &str) -> Result<Option<Config>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("cannot read config {id}")),
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid JSON in config {id}"))?;
    let row = Config::parse(value).with_context(|| format!("invalid config {id}"))?;
    ensure!(row.id == id, "config ID must match its filename");
    Ok(Some(row))
}

fn write_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("missing config directory")?;
    config::private_directory(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    config::private_file(file.path())?;
    serde_json::to_writer_pretty(file.as_file_mut(), value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|e| e.error)
        .with_context(|| format!("could not create {}", path.display()))?;
    Ok(())
}

pub fn import_credential(name: &str, source: &Path) -> Result<Value> {
    let path = credential_path(name)?;
    ensure!(source.is_absolute(), "profile path must be absolute");
    let profile = config::load_connection_profile(source)?;
    ensure!(
        !profile["authentication"].is_null() || !profile["tls"].is_null(),
        "the profile must provide authentication or TLS options"
    );
    let stored: serde_json::Map<_, _> = CREDENTIAL_FIELDS
        .into_iter()
        .filter_map(|key| {
            profile
                .get(key)
                .filter(|v| !v.is_null())
                .map(|v| (key.into(), v.clone()))
        })
        .collect();
    write_new(&path, &stored)?;
    Ok(json!({"name":name,"available":true}))
}

fn chain<'a>(
    target: &'a Config,
    mut lookup: impl FnMut(&str) -> Option<&'a Config>,
) -> Result<Vec<&'a Config>> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    let mut row = target;
    loop {
        ensure!(
            seen.insert(&row.id),
            "jump chain contains a cycle at {}",
            row.id
        );
        ordered.push(row);
        ensure!(
            ordered.len() <= MAX_HOPS,
            "jump chain supports up to {MAX_HOPS} hosts"
        );
        let Some(via) = &row.via else { break };
        row = lookup(via).with_context(|| format!("jump config {via} is missing or invalid"))?;
    }
    ordered.reverse();
    Ok(ordered)
}

fn participating_configs(target: Config) -> Result<BTreeMap<String, Config>> {
    let mut current = target;
    let mut result = BTreeMap::new();
    loop {
        let via = current.via.clone();
        result.insert(current.id.clone(), current);
        let Some(via) = via else { break };
        if result.contains_key(&via) {
            break;
        }
        ensure!(
            result.len() < MAX_HOPS,
            "jump chain supports up to {MAX_HOPS} hosts"
        );
        current = read_config(&config_path(&via)?, &via)?
            .with_context(|| format!("jump config {via} is missing"))?;
    }
    Ok(result)
}

pub fn add(input: Value) -> Result<Value> {
    let row = Config::parse(input)?;
    let id = row.id.clone();
    let records = participating_configs(row)?;
    let row = &records[&id];
    for hop in chain(row, |via| records.get(via))? {
        super::load(&hop.executor)?;
        if let Some(name) = &hop.credential {
            ensure!(
                credential_path(name)?.is_file(),
                "credential {name} is missing"
            );
        }
    }
    write_new(&config_path(&id)?, row)?;
    Ok(serde_json::to_value(row)?)
}

fn legacy(entry: &Value) -> bool {
    [
        "root",
        "connectionProfile",
        "username",
        "authentication",
        "tls",
    ]
    .iter()
    .any(|key| !entry[*key].is_null())
}

/// Build SDK options in execution order. The config ID is the final live alias,
/// independent from every saved host and from other configs targeting that host.
pub fn connection(id: &str, legacy_credential: Option<&str>) -> Result<Vec<Value>> {
    let Some(row) = read_config(&config_path(id)?, id)? else {
        let (entry, _) = super::load(id)?;
        ensure!(
            legacy(&entry),
            "no connection config {id}; add a config first"
        );
        return Ok(vec![super::connection(id, legacy_credential)?]);
    };
    let records = participating_configs(row)?;
    let mut options: Vec<_> = chain(&records[id], |via| records.get(via))?
        .into_iter()
        .map(|row| connection_options(row).with_context(|| format!("cannot use config {}", row.id)))
        .collect::<Result<_>>()?;
    if options.len() == 1 {
        options[0]["id"] = json!(id);
    }
    Ok(options)
}

fn connection_options(row: &Config) -> Result<Value> {
    let (entry, _) = super::load(&row.executor)?;
    let mut options = json!({"id":row.executor,"url":entry["url"]});
    for field in ["links", "channels"] {
        if let Some(value) = entry.get(field).filter(|value| !value.is_null()) {
            options[field] = value.clone();
        }
    }
    if let Some(name) = &row.credential {
        let profile = config::load_connection_profile(&credential_path(name)?)
            .with_context(|| format!("cannot use credential {name}"))?;
        ensure!(
            !profile["authentication"].is_null() || !profile["tls"].is_null(),
            "credential {name} has no authentication or TLS options"
        );
        for key in CREDENTIAL_FIELDS {
            if let Some(value) = profile.get(key) {
                options[key] = value.clone();
            }
        }
    }
    super::validate_connection(options)
}

pub fn inventory() -> Result<Value> {
    let mut host_failures = Vec::new();
    let mut config_failures = Vec::new();
    let mut hosts = BTreeMap::new();
    let mut configs = BTreeMap::new();
    let mut invalid_ids = BTreeSet::new();
    for item in entries(&directory()?)? {
        let path = item.path();
        let current = item.file_type()?.is_dir();
        let file = if current {
            path.join("executor.json")
        } else if path.extension().is_some_and(|ext| ext == "json") {
            path
        } else {
            continue;
        };
        match read_entry(&file) {
            Ok(Some(entry)) => {
                let id = entry["id"]
                    .as_str()
                    .context("executor ID missing")?
                    .to_owned();
                if current || !hosts.contains_key(&id) {
                    hosts.insert(id, entry);
                }
            }
            Ok(None) => {}
            Err(_) => host_failures.push(item.file_name().to_string_lossy().into_owned()),
        }
    }
    for item in entries(&config_directory()?)? {
        let path = item.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if let Some(id) = path.file_stem().and_then(|name| name.to_str()) {
                match read_config(&path, id) {
                    Ok(Some(row)) => {
                        configs.insert(id.to_owned(), row);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        invalid_ids.insert(id.to_owned());
                        config_failures.push(item.file_name().to_string_lossy().into_owned());
                    }
                }
            }
        }
    }
    let credential_inventory = super::list_credentials()?;
    let credentials: BTreeMap<_, _> = credential_inventory["credentials"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row["name"]
                .as_str()
                .map(|name| (name, row["available"].as_bool().unwrap_or(false)))
        })
        .collect();
    let view_configs = configs
        .values()
        .map(|row| {
            let mut view = json!(row);
            match chain(row, |via| configs.get(via)) {
                Ok(route) => {
                    view["route"] = json!(route.iter().map(|hop| &hop.id).collect::<Vec<_>>());
                    for hop in route {
                        let issue = if !hosts.contains_key(&hop.executor) {
                            Some(format!("Missing executor {}", hop.executor))
                        } else {
                            hop.credential
                                .as_deref()
                                .filter(|name| credentials.get(*name) != Some(&true))
                                .map(|name| format!("Missing credential {name}"))
                        };
                        if let Some(issue) = issue {
                            view["error"] = json!(issue);
                            break;
                        }
                    }
                }
                Err(error) => view["error"] = json!(error.to_string()),
            }
            view
        })
        .chain(
            hosts
                .iter()
                .filter(|(id, entry)| {
                    !configs.contains_key(*id) && !invalid_ids.contains(*id) && legacy(entry)
                })
                .map(|(id, entry)| {
                    json!({
                        "id": id, "executor": id, "startPath": entry["root"].as_str(),
                        "legacy": true, "route": [id],
                    })
                }),
        )
        .collect::<Vec<_>>();
    let view_hosts = hosts
        .values()
        .map(|entry| {
            let mut view = super::public_entry(entry);
            if let Some(fields) = view.as_object_mut() {
                fields.remove("root");
                fields.remove("hasProfile");
            }
            view
        })
        .collect::<Vec<_>>();
    host_failures.sort();
    config_failures.sort();
    Ok(json!({
        "executors": view_hosts,
        "credentials": credentials.iter().map(|(name, available)| json!({"name":name,"available":available})).collect::<Vec<_>>(),
        "configs": view_configs,
        "failures": {"executors":host_failures,"configs":config_failures},
    }))
}
