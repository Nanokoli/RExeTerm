//! Execution-side API. Client and application adapters are separate crates.
//!
//! ```compile_fail
//! use remote_executor_server::Client;
//! ```
pub mod auth;
mod context;
pub mod events;
mod executor;
mod file_transfer;
pub mod management;
mod registry;
mod resilient;
pub use remote_executor_data as data;
pub use remote_executor_protocol as protocol;
mod settings;
mod shell_manager;
mod stdio_tasks;
mod terminal;
mod tools;
pub use remote_executor_transport as transport;

pub use context::ToolContext;
pub use executor::{
    dispatch_tool, serve_executor_link, start_executor, start_executor_listener,
    start_shared_executor_ws, Executor,
};
pub use protocol::terminal as terminal_protocol;
pub use protocol::{
    tool_output, tool_output_full, tool_output_with_info, ExecutorInfo, ExecutorRequest,
    ExecutorResponse, ToolResult,
};
pub use remote_executor_terminal as pty;
pub use settings::{ReSettings, SettingsStore, ShellProfile, ShellResolution, ShellSettings};
pub use shell_manager::ShellManager;
pub use tools::exbash::{exbash, exbash_run_detail, ExbashOptions, ExbashOutput};
pub use tools::file_action::{
    file_action, FileActionMode, FileActionOptions, PatchFile, PatchMode,
};
pub use tools::fs::{
    file_hash_code, file_stamp, glob_paths, hash_bytes, read_path, stat_path, FileKind, FileStamp,
    GlobOptions, ReadMode, ReadOptions, StatOptions,
};
pub use tools::rg::{rg_matches, rg_search, RgExecutor, RgMatch, RgOptions, RgOutput};

use anyhow::Result;
use std::path::PathBuf;

#[derive(Clone)]
pub struct RemoteExecutor {
    shell: ShellManager,
    rg: RgExecutor,
}

impl RemoteExecutor {
    pub fn new(shell: ShellManager, rg: RgExecutor) -> Self {
        Self { shell, rg }
    }

    pub fn default_shell(root: impl Into<PathBuf>, cols: u16, rows: u16) -> Self {
        Self {
            shell: ShellManager::default_shell(cols, rows),
            rg: RgExecutor::new(root),
        }
    }

    pub fn shell(&self) -> &ShellManager {
        &self.shell
    }

    pub fn rg(&self) -> &RgExecutor {
        &self.rg
    }

    pub async fn search(&self, options: RgOptions) -> Result<RgOutput> {
        self.rg.search(options).await
    }

    pub fn stat(&self, file_path: impl Into<PathBuf>) -> Result<FileStamp> {
        file_stamp(&file_path.into())
    }
}
