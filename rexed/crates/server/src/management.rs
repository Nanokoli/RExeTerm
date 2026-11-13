//! Independent operations listener. It accepts snapshots, never executor tools/data.
use crate::{
    protocol::management::*,
    registry::now_ms,
    resilient::{bind_tcp, ResilientTcpListener},
    transport::{accept_websocket_at, from_stream, Frame, Link},
    Executor, ShellManager,
};
use anyhow::{anyhow, bail, Result};
#[cfg(unix)]
use std::path::PathBuf;
use tokio::{
    sync::Semaphore,
    task::{JoinHandle, JoinSet},
    time::{timeout, Duration},
};

pub struct ManagementListener {
    address: String,
    task: JoinHandle<()>,
}
impl ManagementListener {
    pub fn address(&self) -> &str {
        &self.address
    }
}
impl Drop for ManagementListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Normalize an independent address. Only omitted network ports use RE port + 1.
pub fn resolve_management_target(
    spec: Option<&str>,
    executor_id: &str,
    business: &str,
) -> Result<String> {
    let Some(spec) = spec else {
        return Ok(default_management_target(executor_id)?);
    };
    if spec.starts_with("unix://") {
        if spec.len() == 7 {
            bail!("Unix socket path is empty")
        }
        return Ok(spec.into());
    }
    let (scheme, rest) = spec
        .split_once("://")
        .ok_or_else(|| anyhow!("management target needs unix://, tcp:// or ws://"))?;
    if !matches!(scheme, "tcp" | "ws" | "tls" | "wss") {
        bail!("unsupported management transport: {scheme}")
    }
    let (authority, path) = rest
        .split_once('/')
        .map(|(a, p)| (a, format!("/{p}")))
        .unwrap_or((rest, String::new()));
    if authority.is_empty()
        || authority.contains(['@', '?', '#'])
        || path.contains(['?', '#'])
        || (matches!(scheme, "tcp" | "tls") && !path.is_empty())
    {
        bail!("invalid management target")
    }
    let has_port = if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or_else(|| anyhow!("invalid IPv6 address"))?;
        !authority[end + 1..].is_empty()
    } else {
        authority.contains(':')
    };
    let authority = if has_port {
        authority.into()
    } else {
        let business = business
            .split_once("://")
            .map(|(_, r)| r)
            .unwrap_or(business)
            .split('/')
            .next()
            .unwrap_or_default();
        let port: u16 = business
            .rsplit_once(':')
            .ok_or_else(|| {
                anyhow!("specify a management port when RE uses a non-network listener")
            })?
            .1
            .parse()?;
        let port = port
            .checked_add(1)
            .ok_or_else(|| anyhow!("RE port + 1 overflows; specify management port explicitly"))?;
        format!("{authority}:{port}")
    };
    // Validate explicit ports as well. Binding resolves hostnames separately.
    let _: u16 = authority
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("management port missing"))?
        .1
        .parse()?;
    Ok(format!("{scheme}://{authority}{path}"))
}

pub fn start_management_listener(
    target: &str,
    executor: Executor,
    manager: ShellManager,
) -> Result<ManagementListener> {
    #[cfg(unix)]
    if let Some(path) = target.strip_prefix("unix://") {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let path = std::path::absolute(path)?;
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)?;
            }
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let cleanup = SocketCleanup {
            path: path.clone(),
            dev: metadata.dev(),
            ino: metadata.ino(),
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let address = format!("unix://{}", path.display());
        let task = tokio::spawn(async move {
            let _cleanup = cleanup;
            let slots = std::sync::Arc::new(Semaphore::new(128));
            let mut tasks = JoinSet::new();
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("rexed Unix management accept failed: {error}");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };
                while tasks.try_join_next().is_some() {}
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let executor = executor.clone();
                let manager = manager.clone();
                tasks.spawn(async move {
                    let _slot = slot;
                    let _ =
                        serve_management_link_trusted(from_stream(stream), executor, manager).await;
                });
            }
        });
        return Ok(ManagementListener { address, task });
    }
    let (scheme, rest) = target
        .split_once("://")
        .ok_or_else(|| anyhow!("management target missing scheme"))?;
    let ws = matches!(scheme, "ws" | "wss");
    let secure = matches!(scheme, "tls" | "wss");
    anyhow::ensure!(
        matches!(scheme, "tcp" | "ws" | "tls" | "wss"),
        "unsupported management transport"
    );
    anyhow::ensure!(
        executor.authentication.is_none() || secure,
        "network management requires tls:// or wss:// when authentication is enabled"
    );
    let tls = if secure {
        Some(
            executor
                .authentication
                .as_ref()
                .ok_or_else(|| anyhow!("TLS management requires authentication configuration"))?
                .tls_config(),
        )
    } else {
        None
    };
    let (address, path) = rest
        .split_once('/')
        .map(|(a, p)| (a, format!("/{p}")))
        .unwrap_or((rest, "/".into()));
    if !ws && path != "/" {
        bail!("TCP management target cannot contain a path")
    }
    let listener = bind_tcp(address)?;
    let actual = format!(
        "{}://{}{}",
        scheme,
        listener.local_addr()?,
        if ws { path.as_str() } else { "" }
    );
    let mut listener = ResilientTcpListener::new(listener, executor.registry.events.clone())?;
    let task = tokio::spawn(async move {
        let slots = std::sync::Arc::new(Semaphore::new(128));
        let mut tasks = JoinSet::new();
        loop {
            let (stream, peer) = listener.accept().await;
            while tasks.try_join_next().is_some() {}
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let executor = executor.clone();
            let manager = manager.clone();
            let path = path.clone();
            let tls = tls.clone();
            tasks.spawn(async move {
                let _slot = slot;
                let _ = async {
                    stream.set_nodelay(true)?;
                    let link = if let Some(tls) = tls {
                        timeout(
                            Duration::from_secs(10),
                            crate::transport::tls::accept_tls(
                                stream,
                                tls,
                                ws.then_some(path.clone()),
                            ),
                        )
                        .await
                        .map_err(anyhow::Error::from)
                        .and_then(|result| result)
                    } else if ws {
                        timeout(Duration::from_secs(10), accept_websocket_at(stream, path))
                            .await
                            .map_err(anyhow::Error::from)
                            .and_then(|result| result)
                    } else {
                        Ok(from_stream(stream))
                    };
                    let link = executor.registry.events.handshake(
                        link,
                        &peer.to_string(),
                        "management",
                    )?;
                    serve_management_link_on(link, executor, manager, &peer.to_string()).await
                }
                .await as Result<()>;
            });
        }
    });
    Ok(ManagementListener {
        address: actual,
        task,
    })
}

pub async fn serve_management_link(
    link: Link,
    executor: Executor,
    manager: ShellManager,
) -> Result<()> {
    serve_management_link_on(link, executor, manager, "injected").await
}
async fn serve_management_link_on(
    mut link: Link,
    executor: Executor,
    manager: ShellManager,
    peer: &str,
) -> Result<()> {
    if let Some(authentication) = &executor.authentication {
        let principal = authentication
            .authenticate(&mut link, &executor.registry.events, peer, "management")
            .await?;
        if !principal.permissions.management {
            executor.registry.events.emit("authentication.authorization_denied", None,
                serde_json::json!({"username":principal.username,"peer":peer,"operation":"management","error":"management permission denied"}));
            // Wait for the first request so the peer can finish its send before we close.
            // Closing immediately after auth races snapshot(), especially on TLS links.
            let _ = timeout(Duration::from_secs(10), link.recv()).await;
            let _ = timeout(
                Duration::from_secs(1),
                link.send(Frame::Text(serde_json::to_string(
                    &ManagementMessage::Error {
                        message: "management permission denied".into(),
                    },
                )?)),
            )
            .await;
            bail!("management permission denied");
        }
    }
    serve_management_link_trusted(link, executor, manager).await
}
async fn serve_management_link_trusted(
    mut link: Link,
    executor: Executor,
    manager: ShellManager,
) -> Result<()> {
    loop {
        // An idle operations connection may stay open; only each request has a size/response budget.
        let Some(frame) = link.recv().await? else {
            return Ok(());
        };
        if matches!(frame, Frame::Close) {
            return Ok(());
        }
        let request = match frame {
            Frame::Text(text) if text.len() <= 4096 => serde_json::from_str(&text).ok(),
            _ => None,
        };
        let reply = match request {
            Some(ManagementMessage::Snapshot { id }) => {
                let (clients, channels) = executor.registry.snapshot();
                ManagementMessage::Result {
                    id,
                    snapshot: ManagementSnapshot {
                        executor_id: executor.info().id.clone(),
                        observed_at_ms: now_ms(),
                        clients,
                        channels,
                        programs: manager.program_summaries(),
                    },
                }
            }
            _ => {
                link.send(Frame::Text(serde_json::to_string(
                    &ManagementMessage::Error {
                        message:
                            "expected re.admin.v1.snapshot; business requests are not accepted here"
                                .into(),
                    },
                )?))
                .await?;
                return Ok(());
            }
        };
        timeout(
            Duration::from_secs(10),
            link.send(Frame::Text(serde_json::to_string(&reply)?)),
        )
        .await??;
    }
}
#[cfg(unix)]
struct SocketCleanup {
    path: PathBuf,
    dev: u64,
    ino: u64,
}
#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.dev && m.ino() == self.ino)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
