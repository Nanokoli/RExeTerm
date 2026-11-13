//! Optional local execution environment, composed separately from remote routing.
use crate::{
    client::ConnectExecutorOptions,
    protocol::ExecutorInfo,
    server::{
        exbash_run_detail, pty::TermSize, start_shared_executor_ws, Executor, SettingsStore,
        ShellManager,
    },
};
use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct LocalExecutor {
    info: ExecutorInfo,
    url: String,
    shell_manager: ShellManager,
}

impl LocalExecutor {
    pub fn start(settings: SettingsStore) -> Result<Self> {
        let command = settings.interactive_command_spec()?;
        let shell_manager = ShellManager::new(command.clone(), TermSize { cols: 80, rows: 24 });
        shell_manager.create_pty("main", command, None, None)?;
        let executor = Executor::local("local")
            .with_shell_manager(shell_manager.clone())
            .with_settings_store(settings);
        let info = executor.info().clone();
        let address = start_shared_executor_ws("127.0.0.1:0", executor, shell_manager.clone())?;
        Ok(Self {
            info,
            url: format!("ws://{address}"),
            shell_manager,
        })
    }

    pub fn connection_options(&self) -> ConnectExecutorOptions {
        ConnectExecutorOptions {
            authentication: None,
            tls: None,
            username: None,
            client_name: None,
            links: Vec::new(),
            channels: Default::default(),
            id: self.info.id.clone(),
            url: self.url.clone(),
            system: self.info.system.clone(),
            device: self.info.device.clone(),
            labels: self.info.labels.clone(),
        }
    }

    pub fn subscribe_exit_code(&self, async_id: &str) -> Result<mpsc::UnboundedReceiver<u32>> {
        self.shell_manager.subscribe_exit_code(async_id)
    }

    pub fn run_detail(&self, async_id: &str) -> Result<Value> {
        exbash_run_detail(&self.shell_manager, async_id)
    }
}
