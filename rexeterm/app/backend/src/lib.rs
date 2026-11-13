//! Full App backend. The GUI binds to this host; AppCore remains the shared state owner.
use anyhow::Result;
pub use rexeterm_appcore::{config, AppCore};
pub use rexeterm_daemon::{Daemon, DaemonOptions};
use tokio::sync::Mutex;
pub mod desktop;

#[cfg(feature = "mcp-http")]
pub use rexeterm_mcp_http_adapt::McpHttpOptions;

#[derive(Default)]
struct HostState {
    stopped: bool,
    #[cfg(feature = "mcp-http")]
    http: Option<rexeterm_mcp_http_adapt::McpHttpServer>,
}

/// Owns the daemon and serializes App listener changes with application shutdown.
/// MCP HTTP is initially off and uses the same AppCore as the UI.
pub struct AppHost {
    daemon: Daemon,
    state: Mutex<HostState>,
}

impl AppHost {
    pub async fn new(options: DaemonOptions) -> Result<Self> {
        Ok(Self {
            daemon: Daemon::new_with_options(options).await?,
            state: Mutex::new(HostState::default()),
        })
    }

    pub fn core(&self) -> &AppCore {
        self.daemon.core()
    }

    pub fn daemon(&self) -> &Daemon {
        &self.daemon
    }

    #[cfg(feature = "mcp-http")]
    pub async fn enable_mcp_http(&self, options: McpHttpOptions) -> Result<std::net::SocketAddr> {
        let mut state = self.state.lock().await;
        anyhow::ensure!(!state.stopped, "application host has shut down");
        anyhow::ensure!(state.http.is_none(), "MCP HTTP is already enabled");
        let server = rexeterm_mcp_http_adapt::start_mcp_http(self.core().clone(), options).await?;
        let address = server.address();
        state.http = Some(server);
        Ok(address)
    }

    #[cfg(feature = "mcp-http")]
    pub async fn disable_mcp_http(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        if let Some(server) = state.http.take() {
            server.shutdown().await?;
        }
        Ok(())
    }

    #[cfg(feature = "mcp-http")]
    pub async fn mcp_http_address(&self) -> Option<std::net::SocketAddr> {
        self.state.lock().await.http.as_ref().map(|s| s.address())
    }

    /// Stop adapter work before clearing the shared RE state.
    pub async fn shutdown(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        state.stopped = true;
        #[cfg(feature = "mcp-http")]
        let result = match state.http.take() {
            Some(server) => server.shutdown().await,
            None => Ok(()),
        };
        self.daemon.shutdown().await;
        #[cfg(feature = "mcp-http")]
        result?;
        Ok(())
    }
}
