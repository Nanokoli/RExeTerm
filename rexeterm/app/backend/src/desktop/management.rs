//! Read-only operations connections; no business Client, session or default-target mutations.
use anyhow::{anyhow, ensure, Result};
use rexeterm_appcore::{
    client::{
        auth::{AuthenticatedConnector, AuthenticationOptions},
        management::ManagementClient,
    },
    transport::{Connector, DefaultConnector, TlsClientOptions, TlsConnector},
};
use rexeterm_application::Host;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

type Slot = Arc<AsyncMutex<Option<Connection>>>;
struct Connection {
    target: String,
    // Deliberately no Debug/Serialize: this value may contain private credentials.
    profile: Value,
    client: ManagementClient,
}
#[derive(Default)]
pub(super) struct ManagementReaders {
    slots: Mutex<HashMap<String, (Instant, Slot)>>,
    stopping: AtomicBool,
}
impl ManagementReaders {
    fn slot(&self, host: &str) -> Result<Slot> {
        let mut slots = self.slots.lock().unwrap();
        ensure!(!self.stopping.load(Ordering::SeqCst), "应用正在退出");
        if !slots.contains_key(host) && slots.len() >= 16 {
            let oldest = slots
                .iter()
                .filter(|(_, (_, slot))| Arc::strong_count(slot) == 1)
                .min_by_key(|(_, (used, _))| *used)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                slots.remove(&id);
            }
            ensure!(slots.len() < 16, "运维请求繁忙，请稍后重试");
        }
        let (used, slot) = slots
            .entry(host.into())
            .or_insert_with(|| (Instant::now(), Default::default()));
        *used = Instant::now();
        Ok(slot.clone())
    }
    pub(super) fn forget(&self, id: &str) {
        self.slots.lock().unwrap().remove(id);
    }
    pub(super) async fn read(&self, host: &Host) -> Result<Value> {
        ensure!(!self.stopping.load(Ordering::SeqCst), "应用正在退出");
        let target = match target(host) {
            Ok(target) => target,
            Err(error) => {
                return Ok(
                    json!({"host":host.id,"status":"unconfigured","message":error.to_string(),"snapshot":null,"target":null}),
                )
            }
        };
        let profile = match profile(host) {
            Ok(profile) => profile,
            Err(_) => {
                return Ok(
                    json!({"host":host.id,"target":target,"status":"invalid_configuration","message":"无法读取运维认证配置，请检查配置文件及 username、authentication、tls 字段。","snapshot":null}),
                )
            }
        };
        let slot = self.slot(&host.id)?;
        let mut stored = slot.lock().await;
        ensure!(!self.stopping.load(Ordering::SeqCst), "应用正在退出");
        // Take ownership across awaits: cancellation drops the poisoned link, rather than caching it.
        let previous = stored
            .take()
            .filter(|c| c.target == target && c.profile == profile);
        let result = tokio::time::timeout(Duration::from_secs(12), async {
            let mut connection = match previous {
                Some(connection) => connection,
                None => Connection {
                    target: target.clone(),
                    profile: profile.clone(),
                    client: ManagementClient::with_connector(connector(&profile)?, &target).await?,
                },
            };
            let snapshot = connection.client.snapshot().await?;
            Ok::<_, anyhow::Error>((connection, snapshot))
        })
        .await
        .map_err(|_| anyhow!("运维连接或读取超时"))
        .and_then(|result| result);
        ensure!(!self.stopping.load(Ordering::SeqCst), "应用正在退出");
        match result {
            Ok((connection, snapshot)) => {
                *stored = Some(connection);
                Ok(json!({"host":host.id,"target":target,"status":"connected","snapshot":snapshot}))
            }
            Err(error) => {
                let message = format!("{error:#}");
                let denied = [
                    "management permission denied",
                    "authentication failed",
                    "Permission denied",
                ]
                .iter()
                .any(|needle| message.contains(needle));
                // SDK/network errors never include the profile contents or password.
                Ok(
                    json!({"host":host.id,"target":target,"status":if denied {"denied"} else {"unavailable"},"message":message,"snapshot":null}),
                )
            }
        }
    }
    pub(super) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let slots = std::mem::take(&mut *self.slots.lock().unwrap());
        for (_, (_, slot)) in slots {
            slot.lock().await.take();
        }
    }
}

pub(super) fn validate_target(target: &str) -> Result<()> {
    ensure!(
        target.len() <= 4096 && !target.chars().any(char::is_control),
        "运维地址无效"
    );
    if let Some(path) = target.strip_prefix("unix://") {
        ensure!(
            path.starts_with('/') && path.len() > 1,
            "请输入运维 socket 的绝对路径"
        );
        return Ok(());
    }
    let url = Url::parse(target).map_err(|_| anyhow!("运维地址无效"))?;
    ensure!(
        matches!(url.scheme(), "tcp" | "tls" | "ws" | "wss")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "运维地址仅支持 tcp、tls、ws、wss 或 unix，认证请使用配置文件"
    );
    ensure!(
        url.port_or_known_default().is_some_and(|p| p != 0),
        "运维端口无效"
    );
    ensure!(
        !matches!(url.scheme(), "tcp" | "tls") || matches!(url.path(), "" | "/"),
        "TCP/TLS 运维地址不能包含路径"
    );
    Ok(())
}
fn target(host: &Host) -> Result<String> {
    if let Some(target) = host.management.as_ref().and_then(|m| m.target.as_deref()) {
        validate_target(target)?;
        return Ok(target.into());
    }
    let mut url = Url::parse(&host.url).map_err(|_| anyhow!("请填写此 Host 的运维地址"))?;
    ensure!(
        matches!(url.scheme(), "tcp" | "tls" | "ws" | "wss"),
        "请填写此 Host 的运维 socket 或网络地址"
    );
    let port = url
        .port_or_known_default()
        .and_then(|p| p.checked_add(1))
        .ok_or_else(|| anyhow!("无法推导运维端口，请单独设置"))?;
    url.set_port(Some(port))
        .map_err(|_| anyhow!("无法推导运维端口"))?;
    let target = url.to_string();
    validate_target(&target)?;
    Ok(target)
}
fn profile(host: &Host) -> Result<Value> {
    let custom = host
        .management
        .as_ref()
        .and_then(|m| m.connection_profile.as_ref());
    let mut value = match custom.or(host.connection_profile.as_ref()) {
        Some(path) => serde_json::from_slice::<Value>(&std::fs::read(path)?)?,
        None => json!({}),
    };
    ensure!(value.is_object(), "invalid management profile");
    if custom.is_none() {
        if let Some(username) = &host.username {
            value["username"] = json!(username);
        }
    }
    Ok(value)
}
fn connector(profile: &Value) -> Result<Arc<dyn Connector>> {
    let tls = profile
        .get("tls")
        .filter(|v| !v.is_null())
        .cloned()
        .map(serde_json::from_value::<TlsClientOptions>)
        .transpose()?;
    let mut connector: Arc<dyn Connector> = match tls {
        Some(tls) => Arc::new(TlsConnector::new(tls)?),
        None => Arc::new(DefaultConnector),
    };
    if let Some(auth) = profile.get("authentication").filter(|v| !v.is_null()) {
        let username = profile["username"]
            .as_str()
            .ok_or_else(|| anyhow!("运维认证配置缺少 username"))?;
        connector = Arc::new(AuthenticatedConnector::new(
            connector,
            username.into(),
            serde_json::from_value::<AuthenticationOptions>(auth.clone())?,
        ));
    }
    Ok(connector)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suggested_address_preserves_transport_ipv6_and_websocket_path() {
        for (business, expected) in [
            ("tls://[::1]:9001", "tls://[::1]:9002"),
            (
                "wss://example.test:9001/admin",
                "wss://example.test:9002/admin",
            ),
            ("ws://example.test/re", "ws://example.test:81/re"),
        ] {
            let host: Host =
                serde_json::from_value(json!({"id":"n","name":"N","root":"/","url":business}))
                    .unwrap();
            assert_eq!(target(&host).unwrap(), expected);
        }
        for business in ["tls://host:65535", "unix:///run/re.sock"] {
            let host: Host =
                serde_json::from_value(json!({"id":"n","name":"N","root":"/","url":business}))
                    .unwrap();
            assert!(target(&host).is_err());
        }
        for invalid in [
            "tls://u:password@host:2",
            "tcp://host:0",
            "tls://host:2/path",
            "https://host:3",
            "unix://relative",
            "wss://host/path?token=secret",
        ] {
            assert!(validate_target(invalid).is_err(), "{invalid}");
        }
    }
}
