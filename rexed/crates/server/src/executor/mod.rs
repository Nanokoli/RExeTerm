//! Executor API and request orchestration.
mod data;
mod dispatch;
mod listeners;
#[cfg(test)]
mod test;

use crate::{
    context::ToolContext,
    protocol::{
        is_write_method, tool_output, ExecutorInfo, ExecutorRequest, ExecutorResponse, ToolResult,
    },
    settings::SettingsStore,
    shell_manager::ShellManager,
};
pub use data::serve_executor_link;
pub use dispatch::dispatch_tool;
use dispatch::{apply_soft_timeout_param, dispatch_with_timeout, effective_tool_timeout_ms};
pub use listeners::{start_executor, start_executor_listener, start_shared_executor_ws};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Clone)]
pub struct Executor {
    info: ExecutorInfo,
    pub(crate) authentication: Option<Arc<crate::auth::Authentication>>,
    pub(crate) registry: crate::registry::Registry,
    file_slots: Arc<tokio::sync::Semaphore>,
    stream_opener: Arc<dyn crate::data::StreamOpener>,
    stream_slots: Arc<tokio::sync::Semaphore>,
    shell_manager: Option<ShellManager>,
    settings: SettingsStore,
    write_lock: Arc<AsyncMutex<()>>,
}

impl Executor {
    pub fn new(info: ExecutorInfo) -> Self {
        Self {
            info,
            authentication: None,
            registry: crate::registry::Registry::default(),
            file_slots: Arc::new(tokio::sync::Semaphore::new(32)),
            stream_opener: Arc::new(crate::data::NativeStreamOpener),
            stream_slots: Arc::new(tokio::sync::Semaphore::new(32)),
            shell_manager: None,
            settings: SettingsStore::load_default_lossy(),
            write_lock: Arc::new(AsyncMutex::new(())),
        }
    }

    pub fn local(id: impl Into<String>) -> Self {
        Self::new(ExecutorInfo {
            id: id.into(),
            system: Some(std::env::consts::OS.to_string()),
            device: std::env::var("HOSTNAME").ok(),
            labels: BTreeMap::new(),
        })
        .with_shell_manager(ShellManager::default_shell(80, 24))
    }

    pub fn with_authentication(mut self, authentication: Arc<crate::auth::Authentication>) -> Self {
        self.authentication = Some(authentication);
        self
    }
    pub fn info(&self) -> &ExecutorInfo {
        &self.info
    }

    pub fn with_event_log(mut self, events: crate::events::EventLog) -> Self {
        self.registry.events = events;
        self
    }
    /// Revoke expired leases, close data links and stop their client-owned tasks.
    pub fn prune_client_sessions(&self) {
        self.registry.prune();
    }
    pub fn with_shell_manager(mut self, shell_manager: ShellManager) -> Self {
        self.shell_manager = Some(shell_manager);
        self
    }

    pub fn with_client_disconnect_timeout(
        self,
        timeout: std::time::Duration,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !timeout.is_zero() && timeout <= std::time::Duration::from_secs(u32::MAX.into()),
            "client disconnect timeout must be positive and fit u32 seconds"
        );
        self.registry.set_disconnect_timeout(timeout);
        Ok(self)
    }
    pub fn with_settings_store(mut self, settings_store: SettingsStore) -> Self {
        if let Ok(settings) = settings_store.settings() {
            self.registry
                .set_disconnect_timeout(std::time::Duration::from_secs(
                    settings.client_disconnect_timeout_seconds.get().into(),
                ));
        }
        self.settings = settings_store;
        self
    }

    pub fn with_stream_opener(mut self, opener: Arc<dyn crate::data::StreamOpener>) -> Self {
        self.stream_opener = opener;
        self
    }
    pub async fn handle(&self, request: ExecutorRequest) -> ExecutorResponse {
        self.handle_owned(request, None, None).await
    }
    pub(crate) async fn handle_owned(
        &self,
        request: ExecutorRequest,
        owner: Option<crate::protocol::management::SessionOwner>,
        peer: Option<&str>,
    ) -> ExecutorResponse {
        let mut audit = self.registry.events.request(&request, owner.as_ref(), peer);
        let response = self.execute_owned(request, owner).await;
        audit.finish(&response);
        response
    }
    async fn execute_owned(
        &self,
        request: ExecutorRequest,
        owner: Option<crate::protocol::management::SessionOwner>,
    ) -> ExecutorResponse {
        let id = request.id.clone();
        let method = request.method.clone();
        if method == "_executor_info" {
            return ExecutorResponse::ok(
                id,
                Some(self.info.id.clone()),
                serde_json::json!(self.executor_info_result()),
            );
        }
        if let Some(authentication) = &self.authentication {
            let permissions = match owner.as_ref() {
                Some(owner) => authentication.permissions(&owner.username).await.ok(),
                None => None,
            };
            let allowed = permissions.is_some_and(|p| p.allows_tool(&method, &request.params));
            if !allowed {
                return ExecutorResponse::err(id, Some(self.info.id.clone()), "permission denied");
            }
        }
        let cancellation = match owner
            .as_ref()
            .map(|o| self.registry.cancellation(o))
            .transpose()
        {
            Ok(value) => value,
            Err(error) => {
                return ExecutorResponse::err(id, Some(self.info.id.clone()), error.to_string())
            }
        };
        if method == "client_resources" {
            let Some(owner) = owner.as_ref() else {
                return ExecutorResponse::err(
                    id,
                    Some(self.info.id.clone()),
                    "client session required",
                );
            };
            let (_, channels) = self.registry.snapshot();
            let channels: Vec<_> = channels
                .into_iter()
                .filter(|c| {
                    c.owner
                        .as_ref()
                        .is_some_and(|o| o.resource_owner_id() == owner.resource_owner_id())
                })
                .collect();
            let programs: Vec<_> = self
                .shell_manager
                .as_ref()
                .map(|m| m.program_summaries())
                .unwrap_or_default()
                .into_iter()
                .filter(|p| {
                    p.created_by
                        .as_ref()
                        .is_some_and(|o| o.resource_owner_id() == owner.resource_owner_id())
                })
                .collect();
            let value = serde_json::json!({"clientId":owner.client_id,"programs":programs,"channels":channels});
            return ExecutorResponse::ok(
                id,
                Some(self.info.id.clone()),
                serde_json::json!(ToolResult {
                    metadata: value.clone(),
                    output: tool_output(value.to_string())
                }),
            );
        }
        let directory = request.directory.clone();
        let timeout_ms = effective_tool_timeout_ms(request.tool_timeout_ms);
        let params = apply_soft_timeout_param(&method, request.params, request.tool_timeout_ms);
        let client_prepared = method == "exbash" && params.get("shellCommand").is_some();
        let mut ctx = ToolContext::new(directory.clone()).with_client_owner(owner);
        ctx.cancellation = cancellation;
        if method == "exbash" && !client_prepared {
            ctx = ctx.with_settings_store(self.settings.clone());
        }
        if let Some(shell_manager) = &self.shell_manager {
            ctx = ctx.with_shell_manager(shell_manager.clone());
        }
        let result = if is_write_method(&method) {
            let _guard = self.write_lock.lock().await;
            dispatch_with_timeout(&method, params, &ctx, timeout_ms).await
        } else {
            dispatch_with_timeout(&method, params, &ctx, timeout_ms).await
        };

        match result {
            Ok(output) => {
                ExecutorResponse::ok(id, Some(self.info.id.clone()), serde_json::json!(output))
            }
            Err(err) => ExecutorResponse::err(id, Some(self.info.id.clone()), err.to_string()),
        }
    }

    fn executor_info_result(&self) -> ToolResult {
        let value = serde_json::json!({
            "id": self.info.id,
            "protocol": "remote-executor",
            "version": env!("CARGO_PKG_VERSION"),
            "system": self.info.system,
            "device": self.info.device,
            "labels": self.info.labels,
            "capabilities": {
                "stdioExecution": true,
                "fileTransfer": true,
                "clientShellCommand": true,
                "streamForward": true,
                "clientSessions": true,
                "clientIsolation": true,
                "persistentClientIdentity": true,
                "channelMetadata": true,
            },
            "fileTransferProtocol": crate::protocol::file::FILE_PROTOCOL,
        });
        ToolResult {
            metadata: value.clone(),
            output: tool_output(serde_json::to_string_pretty(&value).unwrap_or_default()),
        }
    }
}
