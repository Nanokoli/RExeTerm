//! Per-Client executor registry. Network I/O always uses cloned endpoints.
use super::endpoint::ExecutorEndpoint;
use crate::protocol::{tool_output, ToolResult};
use anyhow::{anyhow, Result};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone)]
pub(super) struct ExecutorRegistry {
    inner: Arc<Mutex<ClientState>>,
}
struct ClientState {
    default_executor: Option<String>,
    executors: BTreeMap<String, ExecutorEndpoint>,
}

impl ExecutorRegistry {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ClientState {
                default_executor: None,
                executors: BTreeMap::new(),
            })),
        }
    }
    pub(super) async fn insert(&self, endpoint: ExecutorEndpoint) {
        let mut state = self.inner.lock().await;
        let id = endpoint.id().to_string();
        state.default_executor.get_or_insert_with(|| id.clone());
        state.executors.insert(id, endpoint);
    }
    pub(super) async fn remove(&self, id: &str) -> Option<ExecutorEndpoint> {
        let mut state = self.inner.lock().await;
        let endpoint = state.executors.remove(id);
        if state.default_executor.as_deref() == Some(id) {
            state.default_executor = state.executors.keys().next().cloned();
        }
        endpoint
    }
    pub(super) async fn drain(&self) -> Vec<ExecutorEndpoint> {
        let mut state = self.inner.lock().await;
        state.default_executor = None;
        std::mem::take(&mut state.executors).into_values().collect()
    }
    pub(super) async fn set_default(&self, id: &str) -> Result<()> {
        let mut state = self.inner.lock().await;
        if !state.executors.contains_key(id) {
            return Err(anyhow!("executor not found: {id}"));
        }
        state.default_executor = Some(id.to_string());
        Ok(())
    }
    pub(super) async fn default_executor(&self) -> String {
        self.inner
            .lock()
            .await
            .default_executor
            .clone()
            .unwrap_or_default()
    }
    pub(super) async fn select(&self, requested: Option<&str>) -> Result<ExecutorEndpoint> {
        let state = self.inner.lock().await;
        let id = requested
            .or(state.default_executor.as_deref())
            .ok_or_else(|| anyhow!("no executor selected; connect to a remote executor first"))?;
        state
            .executors
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("executor not found: {id}"))
    }
    pub(super) async fn list_result(&self) -> ToolResult {
        let state = self.inner.lock().await;
        let executors: Vec<_> = state
            .executors
            .values()
            .map(ExecutorEndpoint::summary)
            .collect();
        let value = json!({"default": state.default_executor, "executors": executors});
        ToolResult {
            metadata: value.clone(),
            output: tool_output(serde_json::to_string_pretty(&value).unwrap_or_default()),
        }
    }
}
