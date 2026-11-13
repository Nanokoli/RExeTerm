//! Native PTY runtime owned by RemoteExecutor; no network protocol dependencies.
mod manager;
mod types;

pub mod session;
pub mod state;

pub use manager::{default_shell, PtyManager};
pub use types::{CommandSpec, SessionDetail, SessionSummary, TermSize};
