//! RexETerm compositions: full App host or ordinary MCP stdio with an embedded daemon.
pub use remote_executor_protocol as protocol;
#[cfg(feature = "server")]
pub use remote_executor_server as server;
pub use rexe_client as client;
#[cfg(feature = "app")]
pub use rexeterm_app as app;
pub use rexeterm_appcore as appcore;
#[cfg(feature = "admin")]
pub use rexeterm_cli::admin_view;
#[cfg(feature = "stdio")]
pub use rexeterm_cli::stdio;
pub use rexeterm_daemon as daemon;
#[cfg(feature = "local")]
pub use rexeterm_daemon::local;
#[cfg(feature = "mcp-http")]
pub use rexeterm_mcp_http_adapt as mcp_http;
#[cfg(feature = "mcp-stdio")]
pub use rexeterm_mcp_stdio_adapt as mcp;

#[cfg(feature = "http")]
pub use rexeterm_http_adapt as http;
