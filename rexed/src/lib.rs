//! RE capability and execution roles. Application policy is in RexETerm.
pub use remote_executor_protocol as protocol;
#[cfg(feature = "server")]
pub use remote_executor_server as server;
#[cfg(feature = "client")]
pub use rexe_client as client;
