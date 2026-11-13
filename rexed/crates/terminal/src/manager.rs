use crate::session::{OutputSubscription, Session};
use crate::state::ServerState;
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::{CommandSpec, SessionDetail, SessionSummary, TermSize};

#[derive(Clone)]
pub struct PtyManager {
    state: Arc<ServerState>,
}

impl PtyManager {
    pub fn new(default_command: CommandSpec, default_size: TermSize) -> Self {
        Self {
            state: Arc::new(ServerState::new(default_command, default_size)),
        }
    }

    pub fn default_shell(cols: u16, rows: u16) -> Self {
        Self::new(CommandSpec::new(default_shell()), TermSize { cols, rows })
    }

    pub fn state(&self) -> Arc<ServerState> {
        self.state.clone()
    }

    pub fn create_pty(
        &self,
        name: impl Into<String>,
        command: CommandSpec,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<Arc<Session>> {
        self.state.create_session(name.into(), command, cols, rows)
    }

    pub fn create_pty_with_output(
        &self,
        name: impl Into<String>,
        command: CommandSpec,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<(Arc<Session>, OutputSubscription)> {
        self.state
            .create_session_with_output(name.into(), command, cols, rows)
    }

    pub fn create_bash(&self, name: impl Into<String>) -> Result<Arc<Session>> {
        self.create_pty(name, CommandSpec::new(default_shell()), None, None)
    }

    pub fn session(&self, name: &str) -> Option<Arc<Session>> {
        self.state.session(name)
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        self.state.summaries()
    }

    pub fn detail(&self, pty: &str) -> Result<SessionDetail> {
        self.state.detail(pty)
    }

    pub fn resize_pty(&self, pty: &str, cols: u16, rows: u16) -> Result<()> {
        self.state.require_session(pty)?.resize(cols, rows)
    }

    pub fn send_to_pty(&self, pty: &str, data: &[u8]) -> Result<()> {
        self.state.require_session(pty)?.write_input(data)
    }

    pub fn subscribe_output(&self, pty: &str) -> Result<OutputSubscription> {
        Ok(self.state.require_session(pty)?.subscribe_output())
    }

    pub fn subscribe_exit_code(&self, pty: &str) -> Result<mpsc::UnboundedReceiver<u32>> {
        Ok(self.state.require_session(pty)?.subscribe_exit_code())
    }

    pub fn process_id(&self, pty: &str) -> Result<Option<u32>> {
        Ok(self.state.require_session(pty)?.process_id())
    }

    pub fn try_exit_code(&self, pty: &str) -> Result<Option<u32>> {
        Ok(self.state.detail(pty)?.exit_code)
    }

    pub fn wait_exit_code(&self, pty: &str) -> Result<u32> {
        self.state.require_session(pty)?.wait_exit_code()
    }

    pub async fn wait_exit_code_timeout(
        &self,
        pty: &str,
        timeout: Duration,
    ) -> Result<Option<u32>> {
        let session = self.state.require_session(pty)?;
        if let Some(code) = session.try_exit_code()? {
            return Ok(Some(code));
        }
        let mut rx = session.subscribe_exit_code();
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Some(code)) => Ok(Some(code)),
            Ok(None) => session.try_exit_code(),
            Err(_) => Ok(None),
        }
    }

    pub fn kill_pty(&self, pty: &str) -> Result<()> {
        self.state.require_removed_session(pty)?.kill()
    }
}

pub fn default_shell() -> String {
    if cfg!(windows) {
        "powershell.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string())
    }
}
