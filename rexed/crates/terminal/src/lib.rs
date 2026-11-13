//! Native PTY runtime owned by RemoteExecutor; no network protocol dependencies.
//! Imported from pty-t core; see ../ORIGIN.md for source provenance.
mod manager;
mod types;

pub mod session;
pub mod state;

pub use manager::{default_shell, PtyManager};
pub use types::{CommandSpec, SessionDetail, SessionSummary, TermSize};
