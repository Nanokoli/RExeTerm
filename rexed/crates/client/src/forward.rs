//! Client-owned local TCP listeners forwarding through independent stream links.
use crate::{
    endpoint::ExecutorEndpoint,
    protocol::{forward::StreamTarget, tool_output, ToolResult},
    transport::{ChannelPool, Purpose},
};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::{
    net::TcpListener,
    sync::Semaphore,
    task::{JoinHandle, JoinSet},
    time::{timeout, Duration},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForwardOptions {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub mode: String,
    #[serde(default)]
    pub listen: Option<String>,
    #[serde(default)]
    pub target: Option<StreamTarget>,
    #[serde(default, rename = "forwardID")]
    pub id: Option<String>,
    #[serde(default, rename = "maxConnections")]
    pub max_connections: Option<usize>,
}
struct Entry {
    executor: String,
    metadata: crate::protocol::management::ChannelMetadata,
    listen: String,
    target: StreamTarget,
    active: Arc<AtomicUsize>,
    error: Arc<Mutex<Option<String>>>,
    task: JoinHandle<()>,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<String, Entry>,
    next: u64,
}
impl Drop for State {
    fn drop(&mut self) {
        for entry in self.entries.values() {
            entry.task.abort();
        }
    }
}
#[derive(Clone, Default)]
pub(super) struct ForwardRegistry(Arc<Mutex<State>>);
impl ForwardRegistry {
    pub(super) async fn stop_executor(&self, executor: &str) {
        let entries: Vec<_> = {
            let mut state = self.0.lock().unwrap();
            let ids: Vec<_> = state
                .entries
                .iter()
                .filter(|(_, v)| v.executor == executor)
                .map(|(k, _)| k.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| state.entries.remove(&id))
                .collect()
        };
        for entry in entries {
            entry.task.abort();
            let _ = entry.task.await;
        }
    }
    pub(super) async fn shutdown(&self) {
        let entries = std::mem::take(&mut self.0.lock().unwrap().entries);
        for entry in entries.values() {
            entry.task.abort();
        }
        for entry in entries.into_values() {
            let _ = entry.task.await;
        }
    }
    pub(super) async fn start(
        &self,
        endpoint: ExecutorEndpoint,
        options: ForwardOptions,
    ) -> Result<ToolResult> {
        let metadata = crate::protocol::management::ChannelMetadata {
            title: options.title.unwrap_or_else(|| "未命名".into()),
            description: options.description.unwrap_or_default(),
        }
        .normalized();
        metadata.validate().map_err(anyhow::Error::msg)?;
        let pool = endpoint.stream_pool()?;
        let target = options
            .target
            .ok_or_else(|| anyhow!("target is required"))?;
        if target.kind.trim().is_empty() || target.address.trim().is_empty() {
            bail!("target kind and address are required");
        }
        let listen = options.listen.unwrap_or_else(|| "127.0.0.1:0".into());
        let listen = listen.strip_prefix("tcp://").unwrap_or(&listen);
        let capacity = options.max_connections.unwrap_or(pool.limits().forward);
        if capacity == 0 || capacity > 1024 {
            bail!("maxConnections must be between 1 and 1024");
        }
        let listener = TcpListener::bind(listen).await?;
        let address = format!("tcp://{}", listener.local_addr()?);
        let active = Arc::new(AtomicUsize::new(0));
        let error = Arc::new(Mutex::new(None));
        let task = spawn_listener(
            listener,
            ForwardChannel {
                pool,
                session: endpoint.session.clone(),
                metadata: metadata.clone(),
            },
            target.clone(),
            capacity,
            active.clone(),
            error.clone(),
        );
        let mut state = self.0.lock().unwrap();
        state.next += 1;
        let id = format!("forward-{}", state.next);
        let response = json!({"forwardID":id,"executor":endpoint.id(),"listen":address,"target":target,"maxConnections":capacity,"title":metadata.title,"description":metadata.description});
        state.entries.insert(
            id,
            Entry {
                executor: endpoint.id().into(),
                metadata,
                listen: address,
                target,
                active,
                error,
                task,
            },
        );
        result(response)
    }
    pub(super) fn list(&self) -> Result<ToolResult> {
        let state = self.0.lock().unwrap();
        let entries:Vec<_>=state.entries.iter().map(|(id,entry)|json!({"forwardID":id,"executor":entry.executor,"listen":entry.listen,"target":entry.target,"activeConnections":entry.active.load(Ordering::Relaxed),"lastError":*entry.error.lock().unwrap(),"running":!entry.task.is_finished(),"title":entry.metadata.title,"description":entry.metadata.description})).collect();
        result(json!({"forwards":entries}))
    }
    pub(super) async fn stop(&self, id: &str) -> Result<ToolResult> {
        let entry = self
            .0
            .lock()
            .unwrap()
            .entries
            .remove(id)
            .ok_or_else(|| anyhow!("forward not found: {id}"))?;
        entry.task.abort();
        let _ = entry.task.await;
        result(json!({"forwardID":id,"stopped":true}))
    }
}
fn result(metadata: serde_json::Value) -> Result<ToolResult> {
    Ok(ToolResult {
        output: tool_output(serde_json::to_string_pretty(&metadata)?),
        metadata,
    })
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
struct ForwardChannel {
    pool: ChannelPool,
    session: Option<Arc<super::session::ClientSession>>,
    metadata: crate::protocol::management::ChannelMetadata,
}
fn spawn_listener(
    listener: TcpListener,
    channel: ForwardChannel,
    target: StreamTarget,
    capacity: usize,
    active: Arc<AtomicUsize>,
    error: Arc<Mutex<Option<String>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let slots = Arc::new(Semaphore::new(capacity));
        let mut sessions = JoinSet::new();
        loop {
            while sessions.try_join_next().is_some() {}
            let Ok(slot) = slots.clone().acquire_owned().await else {
                break;
            };
            let (stream, _) = match listener.accept().await {
                Ok(value) => value,
                Err(e) => {
                    *error.lock().unwrap() = Some(e.to_string());
                    break;
                }
            };
            let pool = channel.pool.clone();
            let target = target.clone();
            let session = channel.session.clone();
            let metadata = channel.metadata.clone();
            let error = error.clone();
            let active = active.clone();
            sessions.spawn(async move {
                let _slot = slot;
                active.fetch_add(1, Ordering::Relaxed);
                let _active = Active(active);
                let result = async {
                    let (link, lease) = timeout(Duration::from_secs(30), async {
                        let (mut link, lease) = pool.connect(Purpose::Forward).await?;
                        if let Some(session) = session {
                            session
                                .bind_named(
                                    &mut link,
                                    if metadata.title != "未命名"
                                        || !metadata.description.is_empty()
                                    {
                                        Some(metadata)
                                    } else {
                                        None
                                    },
                                )
                                .await?;
                        }
                        remote_executor_data::open_stream(&mut link, target).await?;
                        Ok::<_, anyhow::Error>((link, lease))
                    })
                    .await
                    .map_err(|_| anyhow!("forward channel open timed out"))??;
                    // Keep link and channel permits until both directions finish.
                    let _lease = lease;
                    remote_executor_data::bridge(stream, link).await?;
                    Ok::<_, anyhow::Error>(())
                }
                .await;
                if let Err(e) = result {
                    *error.lock().unwrap() = Some(e.to_string());
                }
            });
        }
    })
}
