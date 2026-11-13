//! Executor identity/capability discovery and logical tool calls.
use super::{
    options::ConnectExecutorOptions,
    rpc::{call_link, call_timeout_ms_for, DEFAULT_CALL_TIMEOUT_MS},
};
use crate::protocol::{ExecutorInfo, ExecutorRequest, ExecutorResponse};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

#[derive(Clone)]
pub(super) struct ExecutorEndpoint {
    info: ExecutorInfo,
    url: String,
    protocol: Option<String>,
    version: Option<String>,
    file_transfer: bool,
    client_shell_command: bool,
    file_transfer_protocol: Option<String>,
    remote_id: Option<String>,
    extra_links: Vec<String>,
    limits: crate::transport::ChannelLimits,
    pool: Option<crate::transport::ChannelPool>,
    stream_forward: bool,
    client_sessions: bool,
    client_isolation: bool,
    persistent_identity: bool,
    channel_metadata: bool,
    identity: crate::protocol::management::ClientHello,
    pub(super) session: Option<std::sync::Arc<super::session::ClientSession>>,
}

impl ExecutorEndpoint {
    pub(super) fn info(&self) -> &ExecutorInfo {
        &self.info
    }
    pub(super) async fn refresh_info(
        &mut self,
        connector: &dyn crate::transport::Connector,
    ) -> Result<()> {
        let request = ExecutorRequest {
            id: json!("_executor_info"),
            method: "_executor_info".to_string(),
            params: json!({}),
            directory: None,
            executor: None,
            tool_timeout_ms: None,
        };
        let response =
            call_link(connector, &self.url, request, Some(DEFAULT_CALL_TIMEOUT_MS)).await?;
        if !response.ok {
            return Err(anyhow!(response
                .error
                .unwrap_or_else(|| "_executor_info failed".to_string())));
        }
        let result = response.result.unwrap_or(Value::Null);
        let metadata = result.get("metadata").unwrap_or(&result);
        let version = metadata
            .get("version")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("executor info missing version"))?;
        let capabilities = metadata
            .get("capabilities")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("executor info missing capabilities"))?;
        self.remote_id = metadata
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.client_sessions = capabilities
            .get("clientSessions")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.client_isolation = capabilities
            .get("clientIsolation")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.persistent_identity = capabilities
            .get("persistentClientIdentity")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.channel_metadata = capabilities
            .get("channelMetadata")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.stream_forward = capabilities
            .get("streamForward")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.protocol = metadata
            .get("protocol")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.version = Some(version.to_string());
        if self.info.system.is_none() {
            self.info.system = metadata
                .get("system")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if self.info.device.is_none() {
            self.info.device = metadata
                .get("device")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if self.info.labels.is_empty() {
            if let Some(labels) = metadata.get("labels").and_then(Value::as_object) {
                self.info.labels = labels
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect();
            }
        }
        self.client_shell_command = capabilities
            .get("clientShellCommand")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.file_transfer = metadata
            .pointer("/capabilities/fileTransfer")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.file_transfer_protocol = metadata
            .get("fileTransferProtocol")
            .and_then(Value::as_str)
            .map(str::to_string);
        if self.file_transfer && !capabilities.contains_key("fileTransfer") {
            return Err(anyhow!("executor info missing fileTransfer capability"));
        }
        Ok(())
    }

    pub(super) async fn call(
        &self,
        _connector: &dyn crate::transport::Connector,
        mut request: ExecutorRequest,
    ) -> ExecutorResponse {
        let request_id = request.id.clone();
        if matches!(request.method.as_str(), "exbash" | "client_resources")
            && !self.client_isolation
        {
            return ExecutorResponse::err(
                request_id,
                Some(self.info.id.clone()),
                "executor does not support clientIsolation; upgrade rexed",
            );
        }
        let call_timeout_ms = call_timeout_ms_for(&request);
        request.executor = None;
        match super::rpc::call_pooled(
            self.pool.as_ref().expect("registered endpoint has a pool"),
            self.session.as_deref(),
            request,
            call_timeout_ms,
        )
        .await
        {
            Ok(mut response) => {
                response
                    .executor
                    .get_or_insert_with(|| self.info.id.clone());
                response
            }
            Err(err) => ExecutorResponse::err(
                request_id,
                Some(self.info.id.clone()),
                format!("executor {} call failed: {err}", self.info.id),
            ),
        }
    }
}

impl ExecutorEndpoint {
    pub(super) fn remote(options: ConnectExecutorOptions, client_version: String) -> Self {
        Self {
            info: ExecutorInfo {
                id: options.id,
                system: options.system,
                device: options.device,
                labels: options.labels,
            },
            url: normalize_url(&options.url),
            extra_links: options.links.iter().map(|url| normalize_url(url)).collect(),
            limits: options.channels,
            pool: None,
            remote_id: None,
            stream_forward: false,
            client_sessions: false,
            client_isolation: false,
            persistent_identity: false,
            channel_metadata: false,
            identity: crate::protocol::management::ClientHello {
                username: options.username.unwrap_or_else(|| "default".into()),
                client_name: options.client_name.unwrap_or_else(|| "client".into()),
                client_version,
            },
            session: None,
            protocol: None,
            version: None,
            file_transfer: false,
            client_shell_command: false,
            file_transfer_protocol: None,
        }
    }
    pub(super) async fn configure_links(
        &mut self,
        connector: std::sync::Arc<dyn crate::transport::Connector>,
        credentials: &dyn super::ClientCredentialStore,
    ) -> Result<()> {
        self.limits.validate()?;
        let mut targets = vec![self.url.clone()];
        for url in &self.extra_links {
            let mut other = self.clone();
            other.url = url.clone();
            other.refresh_info(connector.as_ref()).await?;
            if self.remote_id.is_none()
                || self.remote_id != other.remote_id
                || self.info.system != other.info.system
            {
                return Err(anyhow!(
                    "additional link does not identify the same executor: {url}"
                ));
            }
            if self.client_shell_command != other.client_shell_command
                || self.stream_forward != other.stream_forward
                || self.file_transfer_protocol != other.file_transfer_protocol
                || self.file_transfer != other.file_transfer
                || self.client_sessions != other.client_sessions
                || self.client_isolation != other.client_isolation
                || self.persistent_identity != other.persistent_identity
                || self.channel_metadata != other.channel_metadata
            {
                return Err(anyhow!("additional link capabilities differ: {url}"));
            }
            targets.push(url.clone());
        }
        self.pool = Some(crate::transport::ChannelPool::new(
            connector,
            targets,
            self.limits.clone(),
        )?);
        if self.client_sessions {
            self.session = Some(
                super::session::ClientSession::open(
                    self.pool.as_ref().unwrap().clone(),
                    self.identity.clone(),
                    if self.persistent_identity {
                        Some(credentials.load_or_create_for(
                            &self.info.id,
                            &serde_json::to_string(&(
                                &self.url,
                                &self.remote_id,
                                &self.identity.username,
                            ))?,
                        )?)
                    } else {
                        None
                    },
                    self.channel_metadata,
                )
                .await?,
            );
        }
        Ok(())
    }
    pub(super) fn stream_pool_for_terminal(&self) -> Result<crate::transport::ChannelPool> {
        self.pool
            .clone()
            .ok_or_else(|| anyhow!("executor has no channel pool"))
    }
    pub(super) fn stream_pool(&self) -> Result<crate::transport::ChannelPool> {
        if !self.stream_forward {
            return Err(anyhow!(
                "executor does not support streamForward; upgrade rexed"
            ));
        }
        self.pool
            .clone()
            .ok_or_else(|| anyhow!("executor has no channel pool"))
    }
    pub(super) fn file_pool(&self) -> Result<crate::transport::ChannelPool> {
        if !self.supports_file_transfer()
            || self.file_transfer_protocol.as_deref() != Some(crate::protocol::file::FILE_PROTOCOL)
        {
            return Err(anyhow!(
                "executor does not support unified file transfer re.file.v2; upgrade rexed"
            ));
        }
        self.pool
            .clone()
            .ok_or_else(|| anyhow!("executor has no channel pool"))
    }
    pub(super) fn supports_client_shell_command(&self) -> bool {
        self.client_shell_command
    }

    pub(super) fn id(&self) -> &str {
        &self.info.id
    }
    pub(super) fn supports_file_transfer(&self) -> bool {
        self.file_transfer
    }
    pub(super) fn summary(&self) -> Value {
        json!({
            "id": self.info.id,
            "system": self.info.system,
            "device": self.info.device,
            "labels": self.info.labels,
            "url": self.url,
            "protocol": self.protocol,
            "version": self.version,
            "fileTransfer": self.file_transfer,
            "clientShellCommand": self.client_shell_command,
            "fileTransferProtocol": self.file_transfer_protocol,
            "links": self.pool.as_ref().map(|pool|pool.targets()),
            "channels": self.limits,
            "streamForward": self.stream_forward,
            "clientSession": self.session.as_ref().map(|s|&s.owner),
            "clientIsolation": self.client_isolation,
        })
    }
}
fn normalize_url(url: &str) -> String {
    if url.contains("://") {
        url.to_string()
    } else {
        format!("ws://{url}")
    }
}
