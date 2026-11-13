//! Application preferences live outside this SDK.
//! ```compile_fail
//! use rexe_client::settings::ClientSettingsStore;
//! ```
//! ```compile_fail
//! use rexe_client::ClientInstance;
//! ```
//! RE capability client: connections, sessions, tools, PTY, files and forwarding.
//! Application preferences and adapters belong to RexETerm.
//!
//! Local execution APIs belong to the application composition layer:
//! ```compile_fail
//! let client = rexe_client::Client::default();
//! client.subscribe_local_exit_code("task");
//! ```
//! ```compile_fail
//! let client = rexe_client::Client::default();
//! client.local_exbash_run_detail("task");
//! ```
pub mod auth;
mod endpoint;
mod identity;
pub use identity::{new_client_credential, ClientCredentialStore};
mod forward;
pub mod management;
mod session;
pub use remote_executor_data as data;
mod options;
mod rpc;
mod state;
pub mod terminal;
mod transfer;
pub use remote_executor_transport as transport;

use anyhow::{anyhow, Result};
use endpoint::ExecutorEndpoint;

pub use options::{
    ConnectExecutorOptions, ExecutorSelection, FileTransferMode, FileTransferOptions,
    SetDefaultExecutorOptions,
};
pub use protocol::file::FileTransferResult;
use protocol::{is_write_method, ExecutorRequest, ExecutorResponse, ToolResult};
pub use remote_executor_protocol as protocol;
use serde_json::{json, Value};
use state::ExecutorRegistry;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Client {
    connector: Arc<dyn transport::Connector>,
    credentials: Arc<dyn ClientCredentialStore>,
    identity: protocol::management::ClientHello,
    forwards: forward::ForwardRegistry,
    state: ExecutorRegistry,
    write_lock: Arc<Mutex<()>>,
}

impl Client {
    /// Create an empty router without opening sockets or starting local processes.
    pub async fn new() -> Result<Self> {
        Ok(Self::default())
    }

    /// Connect an initial endpoint and make it the default. This also lets an
    /// application composition layer register its reserved `local` endpoint.
    pub async fn with_initial_executor(options: ConnectExecutorOptions) -> Result<Self> {
        let client = Self::default();
        client.register_executor(options).await?;
        Ok(client)
    }

    /// Add an initial endpoint during application composition, including `local`.
    pub async fn with_initial_endpoint(self, options: ConnectExecutorOptions) -> Result<Self> {
        self.register_executor(options).await?;
        Ok(self)
    }

    /// Configure the RE-owned user label and client identity for new endpoints.
    pub fn with_identity(mut self, identity: protocol::management::ClientHello) -> Result<Self> {
        identity.validate().map_err(|e| anyhow!(e))?;
        self.identity = identity;
        Ok(self)
    }
    pub async fn with_initial_executor_and_identity(
        options: ConnectExecutorOptions,
        identity: protocol::management::ClientHello,
    ) -> Result<Self> {
        let client = Self::default().with_identity(identity)?;
        client.register_executor(options).await?;
        Ok(client)
    }

    /// Supply stable identity credentials without giving the SDK preference/file ownership.
    pub fn with_credential_store(mut self, store: Arc<dyn ClientCredentialStore>) -> Self {
        self.credentials = store;
        self
    }
    /// Use a custom connector for serial devices, local pipes or other links.
    pub fn with_connector(mut self, connector: Arc<dyn transport::Connector>) -> Self {
        self.connector = connector;
        self
    }

    /// Stop forwarding and best-effort close all RE sessions before shutting down Tokio.
    /// Call after in-flight requests finish. All Client clones share this reset.
    pub async fn shutdown(&self) {
        let endpoints = self.state.drain().await;
        self.forwards.shutdown().await;
        let mut tasks = tokio::task::JoinSet::new();
        for endpoint in endpoints {
            if let Some(session) = endpoint.session {
                tasks.spawn(async move {
                    let _ = session.close().await;
                });
            }
        }
        while tasks.join_next().await.is_some() {}
    }
    pub async fn handle(&self, request: ExecutorRequest) -> ExecutorResponse {
        if is_write_method(&request.method) {
            let id = request.id.clone();
            let Ok(_guard) = self.write_lock.try_lock() else {
                return self.err(id, "another write operation is already running");
            };
            return self.handle_inner(request).await;
        }

        self.handle_inner(request).await
    }

    async fn handle_inner(&self, request: ExecutorRequest) -> ExecutorResponse {
        if is_list_executor(&request.method) {
            return self.ok(request.id, self.state.list_result().await);
        }
        if is_connect_executor(&request.method) {
            return self.connect_executor_response(request).await;
        }
        if is_set_default_executor(&request.method) {
            return self.set_default_response(request).await;
        }
        if request.method == "forward" {
            let id = request.id.clone();
            let result = async {
                let options: forward::ForwardOptions = serde_json::from_value(request.params)?;
                match options.mode.as_str() {
                    "list" => self.forwards.list(),
                    "stop" => {
                        self.forwards
                            .stop(
                                options
                                    .id
                                    .as_deref()
                                    .ok_or_else(|| anyhow!("forwardID is required"))?,
                            )
                            .await
                    }
                    "start" => {
                        let endpoint = self.state.select(request.executor.as_deref()).await?;
                        self.forwards.start(endpoint, options).await
                    }
                    _ => Err(anyhow!("forward mode must be start, list or stop")),
                }
            }
            .await;
            return match result {
                Ok(value) => self.ok(id, value),
                Err(error) => self.err(id, error.to_string()),
            };
        }
        if is_file_transfer(&request.method) {
            return self.file_transfer_response(request).await;
        }

        let request_id = request.id.clone();
        let selected = match self.state.select(request.executor.as_deref()).await {
            Ok(endpoint) => endpoint,
            Err(err) => return self.err(request_id, err.to_string()),
        };
        selected.call(self.connector.as_ref(), request).await
    }

    /// Resolve a target snapshot so application layers can prepare requests.
    pub async fn selected_executor(&self, executor: Option<&str>) -> Result<ExecutorSelection> {
        let endpoint = self.state.select(executor).await?;
        Ok(ExecutorSelection {
            info: endpoint.info().clone(),
            client_shell_command: endpoint.supports_client_shell_command(),
        })
    }

    pub async fn connect_to_executor(&self, options: ConnectExecutorOptions) -> Result<()> {
        if options.id == "local" {
            return Err(anyhow!("local executor is reserved"));
        }
        self.register_executor(options).await
    }

    async fn register_executor(&self, mut options: ConnectExecutorOptions) -> Result<()> {
        options
            .username
            .get_or_insert_with(|| self.identity.username.clone());
        options
            .client_name
            .get_or_insert_with(|| self.identity.client_name.clone());
        if options.id.trim().is_empty() {
            return Err(anyhow!("executor id is required"));
        }
        let connector: Arc<dyn transport::Connector> = match options.tls.clone() {
            Some(tls) => Arc::new(transport::TlsConnector::new(tls)?),
            None => self.connector.clone(),
        };
        let connector: Arc<dyn transport::Connector> = match options.authentication.clone() {
            Some(authentication) => Arc::new(auth::AuthenticatedConnector::new(
                connector,
                options.username.clone().expect("username initialized"),
                authentication,
            )),
            None => connector,
        };
        let mut endpoint = ExecutorEndpoint::remote(options, self.identity.client_version.clone());
        endpoint.refresh_info(connector.as_ref()).await?;
        endpoint
            .configure_links(connector, self.credentials.as_ref())
            .await?;
        self.state.insert(endpoint).await;
        Ok(())
    }

    pub async fn disconnect_executor(&self, id: &str) -> Result<()> {
        let endpoint = self
            .state
            .remove(id)
            .await
            .ok_or_else(|| anyhow!("executor not found: {id}"))?;
        self.forwards.stop_executor(id).await;
        if let Some(session) = endpoint.session {
            session.close().await?;
        }
        Ok(())
    }
    pub async fn set_default_executor(&self, id: &str) -> Result<()> {
        self.state.set_default(id).await
    }

    /// Returns an empty string until an initial or remote endpoint is registered.
    pub async fn default_executor(&self) -> String {
        self.state.default_executor().await
    }

    async fn connect_executor_response(&self, request: ExecutorRequest) -> ExecutorResponse {
        let id = request.id.clone();
        match serde_json::from_value::<ConnectExecutorOptions>(request.params) {
            Ok(options) => match self.connect_to_executor(options).await {
                Ok(()) => self.ok(id, self.state.list_result().await),
                Err(err) => self.err(id, err.to_string()),
            },
            Err(err) => self.err(id, format!("bad connect_to_executor params: {err}")),
        }
    }

    async fn set_default_response(&self, request: ExecutorRequest) -> ExecutorResponse {
        let id = request.id.clone();
        match serde_json::from_value::<SetDefaultExecutorOptions>(request.params) {
            Ok(options) => match self.set_default_executor(&options.id).await {
                Ok(()) => self.ok(id, self.state.list_result().await),
                Err(err) => self.err(id, err.to_string()),
            },
            Err(err) => self.err(id, format!("bad set_default_executor params: {err}")),
        }
    }

    async fn file_transfer_response(&self, request: ExecutorRequest) -> ExecutorResponse {
        let id = request.id.clone();
        let options = match serde_json::from_value::<FileTransferOptions>(request.params) {
            Ok(options) => options,
            Err(err) => return self.err(id, format!("bad file_transfer params: {err}")),
        };
        let selected = match self.state.select(request.executor.as_deref()).await {
            Ok(endpoint) => endpoint,
            Err(err) => return self.err(id, err.to_string()),
        };
        let mut metadata = json!({"mode":options.mode,"localPath":options.local_path,"targetPath":options.target_path,"title":options.metadata.title,"description":options.metadata.description,"protocol":protocol::file::FILE_PROTOCOL});
        match selected
            .transfer_file(options, request.directory.as_deref(), None)
            .await
        {
            Ok(result) => {
                let object = metadata
                    .as_object_mut()
                    .expect("transfer metadata is an object");
                object.insert("bytes".into(), json!(result.bytes));
                object.insert("sha256".into(), json!(result.sha256));
                object.insert("file".into(), result.file);
                self.ok(
                    id,
                    ToolResult {
                        output: protocol::tool_output(format!(
                            "Transferred {} bytes",
                            result.bytes
                        )),
                        metadata,
                    },
                )
            }
            Err(err) => self.err(id, err.to_string()),
        }
    }

    /// Transfer bytes through the selected endpoint's File lane. Hosts can
    /// supply progress observation and cancel by dropping this future.
    pub async fn transfer_file(
        &self,
        executor: Option<&str>,
        options: FileTransferOptions,
        directory: Option<&std::path::Path>,
        observer: Option<&dyn data::file::TransferObserver>,
    ) -> Result<FileTransferResult> {
        self.state
            .select(executor)
            .await?
            .transfer_file(options, directory, observer)
            .await
    }

    fn ok(&self, id: Value, result: ToolResult) -> ExecutorResponse {
        ExecutorResponse::ok(id, Some("client".to_string()), json!(result))
    }

    fn err(&self, id: Value, error: impl Into<String>) -> ExecutorResponse {
        ExecutorResponse::err(id, Some("client".to_string()), error)
    }
}
fn is_list_executor(method: &str) -> bool {
    method == "list_executor"
}

fn is_connect_executor(method: &str) -> bool {
    method == "connect_to_executor"
}

fn is_set_default_executor(method: &str) -> bool {
    method == "set_default_executor"
}

fn is_file_transfer(method: &str) -> bool {
    method == "file_transfer"
}

impl Default for Client {
    fn default() -> Self {
        Self {
            connector: Arc::new(transport::DefaultConnector),
            credentials: Arc::new(identity::MemoryCredentialStore::default()),
            identity: protocol::management::ClientHello::default(),
            forwards: forward::ForwardRegistry::default(),
            state: ExecutorRegistry::new(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }
}
