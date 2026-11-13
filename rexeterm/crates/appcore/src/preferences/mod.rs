//! Client-owned shell preferences. Directory keys describe executor paths;
//! no executor directory or remote settings file is accessed here.
mod schema;
mod store;
mod tools;
pub use schema::{ClientSettings, ExecutorSettings, ShellOverrides};
pub use store::ClientSettingsStore;

#[cfg(test)]
mod test;
