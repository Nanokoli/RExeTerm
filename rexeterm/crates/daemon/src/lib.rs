//! In-process application host. AppCore owns state; Daemon owns startup and lifetime.
#[cfg(feature = "local")]
use crate::{local::LocalExecutor, server::SettingsStore};
#[cfg(feature = "local")]
pub use remote_executor_server as server;
pub use rexeterm_appcore::{client, protocol};
use rexeterm_appcore::{AppCore, AppCoreOptions};
#[cfg(feature = "local")]
pub mod local;
#[cfg(not(feature = "local"))]
use anyhow::anyhow;
use anyhow::Result;
use std::path::PathBuf;

#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "local"), derive(Default))]
pub struct DaemonOptions {
    /// Settings for the optional local executor; ignored when it is disabled.
    pub settings_path: Option<PathBuf>,
    /// Client-owned shell settings; None keeps an isolated in-memory store.
    pub client_settings_path: Option<PathBuf>,
    pub local_executor: bool,
    pub identity: crate::protocol::management::ClientHello,
}
#[cfg(feature = "local")]
impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            settings_path: None,
            client_settings_path: None,
            local_executor: true,
            identity: Default::default(),
        }
    }
}

#[derive(Clone)]
pub struct Daemon {
    instance: AppCore,
    #[cfg(feature = "local")]
    local: Option<LocalExecutor>,
}
impl Daemon {
    pub async fn new() -> Result<Self> {
        Self::new_with_options(DaemonOptions::default()).await
    }
    pub async fn new_with_settings(settings_path: Option<PathBuf>) -> Result<Self> {
        Self::new_with_options(DaemonOptions {
            settings_path,
            ..DaemonOptions::default()
        })
        .await
    }
    pub async fn new_with_options(options: DaemonOptions) -> Result<Self> {
        options.identity.validate().map_err(anyhow::Error::msg)?;
        let instance_options = AppCoreOptions {
            client_settings_path: options.client_settings_path,
            identity: options.identity,
        };
        let instance = AppCore::new_with_options(instance_options).await?;
        if options.local_executor {
            #[cfg(feature = "local")]
            {
                let local = LocalExecutor::start(SettingsStore::load(options.settings_path)?)?;
                let instance = instance
                    .with_initial_executor(local.connection_options())
                    .await?;
                return Ok(Self {
                    instance,
                    local: Some(local),
                });
            }
            #[cfg(not(feature = "local"))]
            {
                return Err(anyhow!(
                    "local executor support is not compiled in; enable the local feature"
                ));
            }
        }
        Ok(Self {
            instance,
            #[cfg(feature = "local")]
            local: None,
        })
    }
    #[cfg(feature = "local")]
    pub async fn new_with_settings_store(settings: SettingsStore) -> Result<Self> {
        let instance = AppCore::new_with_options(AppCoreOptions::default()).await?;
        let local = LocalExecutor::start(settings)?;
        let instance = instance
            .with_initial_executor(local.connection_options())
            .await?;
        Ok(Self {
            instance,
            local: Some(local),
        })
    }
    /// Stop accepting adapter work before calling this method.
    pub async fn shutdown(&self) {
        self.instance.shutdown().await;
    }
    pub fn core(&self) -> &AppCore {
        &self.instance
    }
    #[cfg(feature = "local")]
    pub fn local_executor(&self) -> Option<&LocalExecutor> {
        self.local.as_ref()
    }
}
