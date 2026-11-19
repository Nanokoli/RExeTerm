//! Executor-scoped client configuration. Workspace only indexes IDs and selection.
use super::{Host, ManagementSettings, Workspace};
use anyhow::{ensure, Context, Result};
use rexeterm_appcore::config;
use serde_json::{json, Value};
use std::path::Path;
fn root(path: &Path) -> Result<&Path> {
    path.parent()
        .ok_or_else(|| anyhow::anyhow!("workspace has no parent"))
}
pub(super) fn read(path: &Path, defaults: Workspace) -> Result<Workspace> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(defaults),
        Err(error) => return Err(error.into()),
    };
    let value: Value = serde_json::from_slice(&bytes).context("invalid workspace")?;
    if value.get("hosts").is_some() {
        return Ok(serde_json::from_value(value)?);
    }
    ensure!(value["version"] == 1, "unsupported workspace version");
    let ids = value["hostIds"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("workspace hostIds missing"))?;
    let mut hosts = Vec::new();
    for id in ids {
        let id = id
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid workspace host ID"))?;
        let host: Host = serde_json::from_slice(&std::fs::read(
            config::executor_directory(root(path)?, id).join("executor.json"),
        )?)?;
        ensure!(host.id == id, "executor config ID mismatch");
        hosts.push(host);
    }
    Ok(Workspace {
        version: 1,
        hosts,
        last_host: value["lastHost"].as_str().map(str::to_owned),
    })
}
pub(super) fn prepare(mut host: Host) -> Result<Host> {
    if let Some(source) = host.connection_profile.as_mut() {
        *source = std::path::absolute(&*source)?;
        config::load_connection_profile(source)?;
    }
    if let Some(settings) = host.management.take() {
        host.management = Some(prepare_management(settings)?);
    }
    Ok(host)
}

pub(super) fn prepare_management(mut settings: ManagementSettings) -> Result<ManagementSettings> {
    settings.target = settings
        .target
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    if let Some(source) = settings.connection_profile.as_mut() {
        *source = std::path::absolute(&*source)?;
        config::load_connection_profile(source).context("invalid management connection profile")?;
    }
    Ok(settings)
}
pub(super) fn write(path: &Path, state: &Workspace) -> Result<()> {
    let root = root(path)?;
    for host in &state.hosts {
        config::write_json(
            &config::executor_directory(root, &host.id).join("executor.json"),
            host,
        )?;
    }
    config::write_json(
        path,
        &json!({"version":state.version,"lastHost":state.last_host,"hostIds":state.hosts.iter().map(|host|&host.id).collect::<Vec<_>>()}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_external_profile_and_round_trips_executor_index() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("config");
        std::fs::write(temp.path().join("certificate.pem"), b"test-key").unwrap();
        let source = temp.path().join("source.json");
        std::fs::write(&source,serde_json::to_vec(&json!({"username":"alice","authentication":{"method":"password","password":"private-value"},"tls":{"caFile":"certificate.pem"}})).unwrap()).unwrap();
        let path = root.join("workspace.json");
        let host = prepare(Host {
            id: "node".into(),
            name: "Node".into(),
            url: "tls://127.0.0.1:1".into(),
            root: "/".into(),
            username: Some("alice".into()),
            connection_profile: Some(source.clone()),
            management: None,
        })
        .unwrap();
        assert_eq!(host.connection_profile.as_ref(), Some(&source));
        assert!(!root.exists());
        let auth = config::load_connection_profile(&source).unwrap();
        assert_eq!(
            std::fs::read(auth["tls"]["caFile"].as_str().unwrap()).unwrap(),
            b"test-key"
        );
        write(
            &path,
            &Workspace {
                version: 1,
                hosts: vec![host],
                last_host: Some("node".into()),
            },
        )
        .unwrap();
        let index = std::fs::read_to_string(&path).unwrap();
        assert!(!index.contains("private-value") && !index.contains("connectionProfile"));
        assert!(source.is_file());
        let loaded = read(
            &path,
            Workspace {
                version: 1,
                hosts: vec![],
                last_host: None,
            },
        )
        .unwrap();
        assert_eq!(loaded.hosts[0].id, "node");
        assert_eq!(loaded.last_host.as_deref(), Some("node"));
    }
    #[test]
    fn management_credentials_are_isolated_and_persisted_with_host() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config/workspace.json");
        let legacy: Host = serde_json::from_value(
            json!({"id":"node","name":"Node","url":"tcp://127.0.0.1:1","root":"/"}),
        )
        .unwrap();
        assert!(legacy.management.is_none());
        let business = temp.path().join("business.json");
        let admin = temp.path().join("admin.json");
        for (file, password, ca) in [
            (&business, "business-secret", "business-ca"),
            (&admin, "admin-secret", "admin-ca"),
        ] {
            std::fs::write(temp.path().join(ca), ca).unwrap();
            std::fs::write(file, serde_json::to_vec(&json!({"username":"alice","authentication":{"method":"password","password":password},"tls":{"caFile":ca}})).unwrap()).unwrap();
        }
        let mut host = legacy;
        host.connection_profile = Some(business);
        host.management = Some(ManagementSettings {
            target: Some("  tls://127.0.0.1:2  ".into()),
            connection_profile: Some(admin),
        });
        let host = prepare(host).unwrap();
        let business_path = host.connection_profile.as_ref().unwrap();
        let admin_path = host
            .management
            .as_ref()
            .unwrap()
            .connection_profile
            .as_ref()
            .unwrap();
        assert_ne!(business_path, admin_path);
        assert_eq!(admin_path, &temp.path().join("admin.json"));
        assert_eq!(business_path, &temp.path().join("business.json"));
        for (file, password, ca) in [
            (business_path, "business-secret", "business-ca"),
            (admin_path, "admin-secret", "admin-ca"),
        ] {
            let value = config::load_connection_profile(file).unwrap();
            assert_eq!(value["authentication"]["password"], password);
            assert_eq!(
                std::fs::read_to_string(value["tls"]["caFile"].as_str().unwrap()).unwrap(),
                ca
            );
        }
        write(
            &path,
            &Workspace {
                version: 1,
                hosts: vec![host],
                last_host: None,
            },
        )
        .unwrap();
        let loaded = read(
            &path,
            Workspace {
                version: 1,
                hosts: vec![],
                last_host: None,
            },
        )
        .unwrap();
        assert_eq!(
            loaded.hosts[0]
                .management
                .as_ref()
                .unwrap()
                .target
                .as_deref(),
            Some("tls://127.0.0.1:2")
        );
        assert!(loaded.hosts[0]
            .management
            .as_ref()
            .unwrap()
            .connection_profile
            .as_ref()
            .unwrap()
            .is_file());
        let index = std::fs::read_to_string(path).unwrap();
        assert!(!index.contains("secret") && !index.contains("management"));
    }
}
