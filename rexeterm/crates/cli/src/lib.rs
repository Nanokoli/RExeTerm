#[cfg(feature = "admin")]
pub use remote_executor_protocol as protocol;
#[cfg(feature = "admin")]
pub mod admin_view;
#[cfg(feature = "ipc-client")]
pub mod ipc_client;
#[cfg(feature = "stdio")]
pub mod stdio;

#[cfg(feature = "ipc-client")]
pub mod commands;
#[cfg(feature = "ipc-client")]
pub mod executors;
#[cfg(feature = "ipc-client")]
pub mod markdown;
#[cfg(feature = "ipc-client")]
pub mod preferences;
#[cfg(feature = "ipc-client")]
pub mod screenshot;
#[cfg(feature = "ipc-client")]
pub mod services;

pub use rexeterm_cli_host::directory;
