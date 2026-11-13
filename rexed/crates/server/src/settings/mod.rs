//! Executor startup defaults and local executable resolution.
mod schema;
mod shell;
mod store;
pub use schema::{ReSettings, ShellProfile, ShellSettings};
pub use shell::ShellResolution;
pub use store::SettingsStore;

pub(crate) use shell::resolve_program;
