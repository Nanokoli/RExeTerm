//! GUI composition. Business services share Application; operations use independent links.
mod management;
use crate::{AppHost, DaemonOptions};
#[cfg(not(feature = "mcp-http"))]
use anyhow::anyhow;
use anyhow::Result;
use rexeterm_application::Application;
pub use rexeterm_application::{friendly_path, join_remote, Host, ManagementSettings, Workspace};
#[cfg(feature = "mcp-http")]
use serde_json::json;
use serde_json::Value;
use std::{ops::Deref, path::PathBuf, sync::Arc};
pub struct Desktop {
    host: AppHost,
    application: Arc<Application>,
    management: management::ManagementReaders,
}
impl Deref for Desktop {
    type Target = Arc<Application>;
    fn deref(&self) -> &Self::Target {
        &self.application
    }
}
impl Desktop {
    pub async fn new(path: PathBuf, defaults: Workspace) -> Result<Arc<Self>> {
        let host = AppHost::new(DaemonOptions {
            local_executor: false,
            client_settings_path: Some(path.with_file_name("preferences.json")),
            identity: rexeterm_appcore::protocol::management::ClientHello {
                client_name: "RexETerm App".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .await?;
        let application = Application::new(host.core().clone(), path, defaults).await?;
        Ok(Arc::new(Self {
            host,
            application,
            management: Default::default(),
        }))
    }
    pub fn app_host(&self) -> &AppHost {
        &self.host
    }
    pub fn application(&self) -> Arc<Application> {
        self.application.clone()
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.management.shutdown().await;
        self.application.shutdown().await?;
        self.host.shutdown().await
    }
}
pub async fn dispatch(service: &Arc<Desktop>, action: &str, args: Value) -> Result<Value> {
    match action {
        "management_snapshot" => {
            let host = service.application.host(
                args["host"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 Host"))?,
            )?;
            service.management.read(&host).await
        }
        "management_configure" => {
            let id = args["host"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("缺少 Host"))?;
            let settings: ManagementSettings = serde_json::from_value(args["settings"].clone())?;
            if let Some(target) = settings.target.as_deref().filter(|s| !s.trim().is_empty()) {
                management::validate_target(target.trim())?;
            }
            service.application.set_management(id, settings).await?;
            service.management.forget(id);
            service.application.workspace().await
        }
        "management_disconnect" => {
            if let Some(id) = args["host"].as_str() {
                service.management.forget(id);
            }
            Ok(serde_json::json!({"ok":true}))
        }
        #[cfg(feature = "mcp-http")]
        "mcp_status" => Ok(
            json!({"address":service.host.mcp_http_address().await.map(|a|format!("http://{a}/mcp"))}),
        ),
        #[cfg(feature = "mcp-http")]
        "mcp_toggle" => {
            if args["enabled"].as_bool().unwrap_or(false) {
                let listen = args["listen"]
                    .as_str()
                    .unwrap_or("127.0.0.1:8080")
                    .parse()?;
                let address = service
                    .host
                    .enable_mcp_http(crate::McpHttpOptions {
                        listen,
                        ..Default::default()
                    })
                    .await?;
                Ok(json!({"address":format!("http://{address}/mcp")}))
            } else {
                service.host.disable_mcp_http().await?;
                Ok(json!({"address":null}))
            }
        }
        #[cfg(not(feature = "mcp-http"))]
        "mcp_status" | "mcp_toggle" => Err(anyhow!("MCP HTTP 未启用")),
        _ => rexeterm_application::dispatch(&service.application, action, args).await,
    }
}
