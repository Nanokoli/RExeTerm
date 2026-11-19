//! RexETerm appcore owns RE client instances and application preferences.
pub use rexe_client as client;
pub use rexe_client::{data, protocol, transport};
pub use rexeterm_config as config;
mod history;
mod identity;
pub mod preferences;
mod terminal_output;

use anyhow::Result;
use client::{Client, ConnectExecutorOptions};
use preferences::ClientSettingsStore;
use protocol::{management::ClientHello, ExecutorRequest, ExecutorResponse};
use serde_json::{json, Value};
use std::{ops::Deref, path::PathBuf, sync::Arc};

#[derive(Clone, Debug, Default)]
pub struct AppCoreOptions {
    pub client_settings_path: Option<PathBuf>,
    pub identity: ClientHello,
}

/// The common application instance used by MCP, CLI and the full user app.
/// Clones share the RE client and preference snapshots.
#[derive(Clone, Default)]
pub struct AppCore {
    client: Client,
    preferences: ClientSettingsStore,
    history: Arc<history::TaskHistory>,
}
impl AppCore {
    /// In-memory preferences and an empty RE client; no local execution resources.
    pub async fn new() -> Result<Self> {
        Ok(Self::default())
    }
    pub async fn new_with_options(options: AppCoreOptions) -> Result<Self> {
        options.identity.validate().map_err(anyhow::Error::msg)?;
        let preferences = match options.client_settings_path {
            Some(path) => ClientSettingsStore::load(path)?,
            None => ClientSettingsStore::default(),
        };
        Ok(Self {
            client: Client::new().await?.with_identity(options.identity)?,
            preferences: ClientSettingsStore::default(),
            history: Default::default(),
        }
        .with_settings_store(preferences))
    }
    pub fn from_client(client: Client) -> Self {
        Self {
            client,
            preferences: ClientSettingsStore::default(),
            history: Default::default(),
        }
    }
    pub fn with_settings_store(mut self, preferences: ClientSettingsStore) -> Self {
        if let Some(path) = preferences.path() {
            self.history = history::TaskHistory::load(path.with_extension("task-history.json"));
            self.client =
                self.client
                    .with_credential_store(Arc::new(identity::FileCredentialStore::new(
                        path.with_extension("identities.json"),
                    )));
        }
        self.preferences = preferences;
        self
    }
    pub fn with_identity(mut self, identity: ClientHello) -> Result<Self> {
        self.client = self.client.with_identity(identity)?;
        Ok(self)
    }
    pub fn with_connector(mut self, connector: Arc<dyn transport::Connector>) -> Self {
        self.client = self.client.with_connector(connector);
        self
    }
    pub async fn with_initial_executor(mut self, endpoint: ConnectExecutorOptions) -> Result<Self> {
        self.client = self.client.with_initial_endpoint(endpoint).await?;
        Ok(self)
    }
    /// Direct access bypasses RexETerm preference handling.
    pub fn re_client(&self) -> &Client {
        &self.client
    }
    pub fn mark_task_read(&self, executor: &str, id: &str) {
        self.history.mark_read(executor, id);
    }

    /// Executor a host-less request would use; the directory preference layer
    /// needs it because folders belong to an executor.
    pub async fn default_executor(&self) -> String {
        self.client.default_executor().await
    }

    pub fn preferences(&self) -> &ClientSettingsStore {
        &self.preferences
    }
    pub fn task_history(&self) -> Value {
        self.history.snapshot()
    }
    pub fn remove_task_history(&self, executor: &str, id: &str) -> Result<()> {
        self.history.remove(executor, id)
    }
    pub async fn shutdown(&self) {
        self.history.stop();
        self.client.shutdown().await;
    }
    pub async fn handle(&self, request: ExecutorRequest) -> ExecutorResponse {
        let exbash = request.method == "exbash";
        let mut response = self.handle_unrendered(request).await;
        if exbash {
            terminal_output::render(&mut response);
        }
        response
    }

    /// Run the same preparation and task tracking without projecting PTY bytes.
    /// The CLI host maintains its own screen across output updates.
    pub async fn handle_unrendered(&self, mut request: ExecutorRequest) -> ExecutorResponse {
        if request.method == "connect_to_executor" {
            let saved = (|| -> Result<()> {
                let Some(path) = self.preferences.path() else {
                    return Ok(());
                };
                let id = request.params["id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("executor id required"))?;
                let root = path.parent().unwrap_or_else(|| std::path::Path::new("."));
                let directory = config::executor_directory(root, id);
                let credentials = config::credential_directory(root, id);
                let mut profile = request.params["connectionProfile"]
                    .as_str()
                    .map(PathBuf::from);
                config::resolve_tls_paths(&mut request.params, &std::env::current_dir()?)?;
                // GUI/HTTP saved hosts name an external profile; only direct SDK
                // options with inline credentials need their own private file.
                if profile.is_none()
                    && (!request.params["authentication"].is_null()
                        || !request.params["tls"].is_null())
                {
                    let user = request.params["username"].as_str().unwrap_or("default");
                    let target =
                        config::user_directory(&credentials, user).join("authentication.json");
                    config::write_json(
                        &target,
                        &json!({
                            "username":request.params["username"],
                            "authentication":request.params["authentication"],
                            "tls":request.params["tls"],
                        }),
                    )?;
                    profile = Some(target);
                }
                let mut public = request.params.clone();
                let fields = public.as_object_mut().expect("params with id is an object");
                for key in ["authentication", "tls", "connectionProfile"] {
                    fields.remove(key);
                }
                public["userConfig"] = json!(profile);
                config::write_json(&directory.join("connection.json"), &public)
            })();
            if let Err(error) = saved {
                return ExecutorResponse::err(request.id, Some("client".into()), error.to_string());
            }
        }
        if request.method == "task_history" {
            return ExecutorResponse::ok(
                request.id,
                Some("client".into()),
                json!({"metadata":self.history.snapshot(), "output":protocol::tool_output("")}),
            );
        }
        if request.method == "exbash"
            && matches!(request.params["mode"].as_str(), Some("run" | "shell"))
            && !self.history.can_start()
        {
            return ExecutorResponse::err(
                request.id,
                Some("client".into()),
                "application task capacity reached",
            );
        }
        if matches!(
            request.method.as_str(),
            "list_shells" | "set_default_shell" | "request_reload"
        ) {
            let target = match &request.executor {
                Some(id) => id.clone(),
                None => self.client.default_executor().await,
            };
            let directory = request
                .directory
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned());
            return match self.preferences.handle_tool(
                &request.method,
                request.params,
                &target,
                directory.as_deref(),
            ) {
                Ok(result) => {
                    ExecutorResponse::ok(request.id, Some("client".into()), json!(result))
                }
                Err(error) => {
                    ExecutorResponse::err(request.id, Some("client".into()), error.to_string())
                }
            };
        }
        if request.method == "exbash"
            && request.params.get("mode").and_then(Value::as_str) == Some("shell")
            // Clients that own their preferences send the finished command.
            && request.params.get("shellCommand").is_none()
        {
            let prepare = async {
                let selected = self.client.selected_executor(request.executor.as_deref()).await?;
                if !selected.client_shell_command {
                    anyhow::bail!("executor {} does not support clientShellCommand; upgrade the executor to use client shell preferences", selected.info.id);
                }
                let directory = request.directory.as_ref().map(|p| p.to_string_lossy().into_owned());
                let command = self.preferences.shell_command(&selected.info.id, directory.as_deref(), request.params.get("shell").and_then(Value::as_str), selected.info.system.as_deref(), request.params.get("command").and_then(Value::as_str).unwrap_or_default())?;
                Ok::<_, anyhow::Error>((selected.info.id, command))
            }.await;
            match prepare {
                Ok((target, command)) => {
                    request.executor = Some(target);
                    request.params["shellCommand"] = json!(command);
                }
                Err(error) => {
                    return ExecutorResponse::err(
                        request.id,
                        Some("client".into()),
                        error.to_string(),
                    )
                }
            }
        }
        let exbash = request.method == "exbash";
        let track = exbash
            && matches!(
                request.params["mode"].as_str(),
                Some("run" | "shell" | "stop" | "status" | "attach")
            );
        let track_target = match &request.executor {
            Some(target) => target.clone(),
            None => self.client.default_executor().await,
        };
        let response = self.client.handle(request).await;
        if track && response.ok {
            if let Some(result) = &response.result {
                self.history.observe(
                    self.client.clone(),
                    track_target,
                    result["metadata"].clone(),
                );
            }
        }
        response
    }
}
impl Deref for AppCore {
    type Target = Client;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
