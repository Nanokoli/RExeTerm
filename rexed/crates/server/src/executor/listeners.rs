//! Physical listener adapters. Decoding tool and terminal data lives in data.rs.
use super::{data::serve_executor_link_on, Executor};
use crate::{
    resilient::{bind_tcp, ResilientTcpListener},
    shell_manager::ShellManager,
    transport::{accept_websocket, accept_websocket_at, from_stream},
};
use anyhow::{bail, Result};

/// WebSocket over HTTP is the default for unqualified addresses. TCP and Unix socket
/// listeners use REX1 framing; serial/hardware adapters use serve_executor_link.
pub fn start_executor_listener(
    target: impl Into<String>,
    executor: Executor,
    manager: ShellManager,
) -> Result<String> {
    executor.registry.maintain(manager.clone());
    let target = target.into();
    if target.starts_with("tls://") || target.starts_with("wss://") {
        let (scheme, rest) = target.split_once("://").unwrap();
        let (address, path) = rest
            .split_once('/')
            .map(|(a, p)| (a, format!("/{p}")))
            .unwrap_or((rest, "/".into()));
        anyhow::ensure!(
            scheme != "tls" || path == "/",
            "TLS stream listener cannot contain a path"
        );
        let authentication = executor
            .authentication
            .clone()
            .ok_or_else(|| anyhow::anyhow!("TLS listener requires authentication configuration"))?;
        let listener = bind_tcp(address)?;
        let actual = format!(
            "{scheme}://{}{}",
            listener.local_addr()?,
            if scheme == "wss" { &path } else { "" }
        );
        let websocket = scheme == "wss";
        let mut listener = ResilientTcpListener::new(listener, executor.registry.events.clone())?;
        tokio::spawn(async move {
            loop {
                let (stream, peer) = listener.accept().await;
                let Ok(slot) = authentication.connections.clone().try_acquire_owned() else {
                    continue;
                };
                let executor = executor.clone();
                let manager = manager.clone();
                let auth = authentication.clone();
                let path = path.clone();
                tokio::spawn(async move {
                    let _slot = slot;
                    report(
                        async {
                            stream.set_nodelay(true)?;
                            let link = tokio::time::timeout(
                                std::time::Duration::from_secs(10),
                                crate::transport::tls::accept_tls(
                                    stream,
                                    auth.tls_config(),
                                    websocket.then_some(path),
                                ),
                            )
                            .await
                            .map_err(anyhow::Error::from)
                            .and_then(|result| result);
                            let link = executor.registry.events.handshake(
                                link,
                                &peer.to_string(),
                                if websocket { "wss" } else { "tls" },
                            )?;
                            serve_executor_link_on(
                                link,
                                peer.to_string(),
                                if websocket { "wss" } else { "tls" },
                                executor,
                                manager,
                            )
                            .await
                        }
                        .await,
                    );
                });
            }
        });
        return Ok(actual);
    }
    anyhow::ensure!(
        executor.authentication.is_none(),
        "authenticated listeners require tls:// or wss://"
    );
    if let Some(address) = target.strip_prefix("tcp://") {
        let listener = bind_tcp(address)?;
        let address = format!("tcp://{}", listener.local_addr()?);
        let mut listener = ResilientTcpListener::new(listener, executor.registry.events.clone())?;
        tokio::spawn(async move {
            loop {
                let (stream, peer) = listener.accept().await;
                if let Err(error) = stream.set_nodelay(true) {
                    report(Err(error.into()));
                    continue;
                }
                let executor = executor.clone();
                let manager = manager.clone();
                tokio::spawn(async move {
                    report(
                        serve_executor_link_on(
                            from_stream(stream),
                            peer.to_string(),
                            "tcp",
                            executor,
                            manager,
                        )
                        .await,
                    );
                });
            }
        });
        return Ok(address);
    }
    #[cfg(unix)]
    if let Some(path) = target.strip_prefix("unix://") {
        // Do not unlink an existing socket: it may belong to another service.
        let listener = tokio::net::UnixListener::bind(path)?;
        let address = format!("unix://{}", std::path::absolute(path)?.display());
        tokio::spawn(async move {
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("rexed Unix accept failed: {error}");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }
                };
                let executor = executor.clone();
                let manager = manager.clone();
                let peer = peer
                    .as_pathname()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "unix:anonymous".into());
                tokio::spawn(async move {
                    report(
                        serve_executor_link_on(
                            from_stream(stream),
                            peer,
                            "unix",
                            executor,
                            manager,
                        )
                        .await,
                    );
                });
            }
        });
        return Ok(address);
    }
    let address = target.strip_prefix("ws://").unwrap_or(&target);
    if let Some((address, path)) = address.split_once('/') {
        if address.contains("://") {
            bail!("unsupported listener target: {target}")
        }
        let path = format!("/{path}");
        let bound =
            start_shared_executor_ws_at(address.into(), executor, manager, Some(path.clone()))?;
        return Ok(format!("ws://{bound}{path}"));
    }
    if address.contains("://") {
        bail!("unsupported listener target: {target}");
    }
    Ok(format!(
        "ws://{}",
        start_shared_executor_ws(address, executor, manager)?
    ))
}

/// Existing WebSocket API: returns host:port for clients that construct URLs.
pub fn start_shared_executor_ws(
    addr: impl Into<String>,
    executor: Executor,
    manager: ShellManager,
) -> Result<String> {
    start_shared_executor_ws_at(addr.into(), executor, manager, None)
}
fn start_shared_executor_ws_at(
    addr: String,
    executor: Executor,
    manager: ShellManager,
    path: Option<String>,
) -> Result<String> {
    let listener = bind_tcp(&addr)?;
    let address = listener.local_addr()?.to_string();
    let mut listener = ResilientTcpListener::new(listener, executor.registry.events.clone())?;
    tokio::spawn(async move {
        loop {
            let (stream, peer) = listener.accept().await;
            if let Err(error) = stream.set_nodelay(true) {
                report(Err(error.into()));
                continue;
            }
            let executor = executor.clone();
            let manager = manager.clone();
            let path = path.clone();
            tokio::spawn(async move {
                let result = async {
                    let handshake =
                        tokio::time::timeout(std::time::Duration::from_secs(10), async {
                            match path {
                                Some(path) => accept_websocket_at(stream, path).await,
                                None => accept_websocket(stream).await,
                            }
                        })
                        .await
                        .map_err(anyhow::Error::from)
                        .and_then(|result| result);
                    let link =
                        executor
                            .registry
                            .events
                            .handshake(handshake, &peer.to_string(), "ws")?;
                    serve_executor_link_on(link, peer.to_string(), "ws", executor, manager).await
                }
                .await;
                report(result);
            });
        }
    });
    Ok(address)
}
fn report(result: Result<()>) {
    if let Err(error) = result {
        let message = error.to_string();
        if !message.contains("Connection reset without closing handshake")
            && !message.contains("connection reset by peer")
            && !message.contains("Broken pipe")
        {
            eprintln!("rexed connection error: {error:#}");
        }
    }
}

/// Async entry point adds the optional KCP listener to the standard listeners.
pub async fn start_executor(
    target: impl Into<String>,
    executor: Executor,
    manager: ShellManager,
) -> Result<String> {
    executor.registry.maintain(manager.clone());
    let target = target.into();
    #[cfg(feature = "kcp")]
    if let Some(address) = target.strip_prefix("kcp://") {
        let mut acceptor = crate::transport::KcpAcceptor::bind(address).await?;
        let actual = format!("kcp://{}", acceptor.local_addr()?);
        tokio::spawn(async move {
            loop {
                let (link, peer) = match acceptor.accept().await {
                    Ok(value) => value,
                    Err(error) => {
                        eprintln!("rexed KCP accept failed: {error}");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }
                };
                let executor = executor.clone();
                let manager = manager.clone();
                tokio::spawn(async move {
                    report(serve_executor_link_on(link, peer, "kcp", executor, manager).await);
                });
            }
        });
        return Ok(actual);
    }
    start_executor_listener(target, executor, manager)
}
