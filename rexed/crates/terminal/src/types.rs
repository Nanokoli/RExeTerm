use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn envs(
        mut self,
        env: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        self.env.extend(
            env.into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        self
    }

    pub fn argv(&self) -> Vec<String> {
        let mut argv = vec![self.program.clone()];
        argv.extend(self.args.clone());
        argv
    }

    pub fn cwd_ref(&self) -> Option<&PathBuf> {
        self.cwd.as_ref()
    }

    pub fn env_ref(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    pub(crate) fn effective_cwd(&self) -> Option<PathBuf> {
        self.cwd.clone().or_else(|| std::env::current_dir().ok())
    }
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub pty: String,
    pub command: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub process_id: Option<u32>,
    pub created_at: u64,
    pub exit_code: Option<u32>,
    pub output_history_bytes: usize,
    pub output_history_limit: usize,
    pub output_total_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct SessionDetail {
    pub pty: String,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub process_id: Option<u32>,
    pub created_at: u64,
    pub cols: u16,
    pub rows: u16,
    pub output_history_bytes: usize,
    pub output_history_limit: usize,
    pub output_total_bytes: u64,
    pub exit_code: Option<u32>,
    pub ended_at: Option<u64>,
}

pub fn path_text(path: Option<&Path>) -> Option<String> {
    path.map(|path| path.to_string_lossy().into_owned())
}
