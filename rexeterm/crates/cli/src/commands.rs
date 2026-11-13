//! Agent commands only build host requests; all RE state stays in the bound host.
use crate::{
    ipc_client::IpcClient,
    preferences,
    services::{self, DaemonArgs, ServiceCommand},
};
use anyhow::{ensure, Context, Result};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use rexeterm_cli_host::Registry;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Long names also accepted with a single dash, as in `-cmd 'ls -la'`.
pub const LONG_OPTIONS: &[&str] = &[
    "cmd",
    "title",
    "desc",
    "description",
    "text",
    "file",
    "raw",
    "png",
    "stop",
    "status",
    "read",
    "remove",
    "input",
    "input-file",
    "rcwd",
    "stdio",
    "pty",
    "direct",
    "stdin",
    "keep-stdin",
    "task-ttl",
    "timeout",
    "attach",
    "transfer",
    "executor",
    "service-id",
    "registry-dir",
    "out-dir",
    "sink",
    "text-limit",
    "host",
    "options-file",
    "layers",
    "input",
    "output",
    "overwrite",
    "binary",
    "hash-code",
    "pattern",
    "path",
    "root",
    "glob",
    "ignore-case",
    "max-count",
    "offset",
    "limit",
    "local",
    "default",
    "program",
    "command-arg",
    "interactive-arg",
];
/// Rewrite `-cmd` to `--cmd`, so the single-dash spelling works everywhere.
pub fn normalize_argv<I, T>(args: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    args.into_iter()
        .map(Into::into)
        .map(|arg| {
            let text = arg.to_string_lossy();
            let Some(name) = text.strip_prefix('-') else {
                return arg;
            };
            if name.starts_with('-') || name.len() < 2 {
                return arg;
            }
            let (head, tail) = match name.find('=') {
                Some(index) => (&name[..index], &name[index..]),
                None => (name, ""),
            };
            if LONG_OPTIONS.contains(&head) {
                return std::ffi::OsString::from(format!("--{head}{tail}"));
            }
            arg
        })
        .collect()
}

#[derive(Parser)]
#[command(
    name = "rexeterm",
    version,
    about = "Agent CLI: one Markdown result, then exit",
    after_help = "Current directory opt-out: .rexeterm.disable blocks operations. Use disable to create it and enable to remove it.",
    disable_help_subcommand = true
)]
pub struct Args {
    /// Executor every command acts on, for example `-e local`.
    #[arg(
        short = 'e',
        long = "executor",
        global = true,
        help_heading = "Global options",
        display_order = 898
    )]
    pub executor: Option<String>,
    /// Host UUID64; otherwise take the machine binding written beside the executable.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        display_order = 900
    )]
    service_id: Option<String>,
    /// Where service bindings live; defaults to the platform registry directory.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        display_order = 901
    )]
    registry_dir: Option<PathBuf>,
    /// Whole invocation deadline; timing out does not undo a host operation.
    #[arg(long, global=true, default_value_t=30000, value_parser=clap::value_parser!(u64).range(1..=600000), help_heading = "Global options", display_order = 902)]
    pub timeout_ms: u64,
    /// Payload sinks; repeat or comma-separate: text, file, raw, png, tui.
    #[arg(
        long,
        global = true,
        value_delimiter = ',',
        default_value = "text",
        help_heading = "Global options",
        display_order = 903
    )]
    pub sink: Vec<Sink>,
    /// Directory for sink files; defaults to ./.rexeterm-out in the current directory.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        display_order = 904
    )]
    pub out_dir: Option<PathBuf>,
    /// Inline text is cut after this many bytes; the full body still goes to file.
    #[arg(
        long,
        global = true,
        default_value_t = 65536,
        help_heading = "Global options",
        display_order = 905
    )]
    pub text_limit: usize,
    /// Run a command without naming `run`: rexeterm -e local -cmd 'ls -la'.
    #[arg(long = "cmd", value_name = "COMMAND")]
    pub cmd: Option<String>,
    /// Shell profile for the bare form; see `run -s`.
    #[arg(short = 's', long = "shell")]
    pub shell: Option<String>,
    /// Forward ID: show it, or stop it with --stop.
    #[arg(
        short = 'F',
        long = "forward",
        value_name = "FORWARDID",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub forward: Option<String>,
    /// Transfer ID: show its progress, or cancel it with --stop.
    #[arg(
        short = 'T',
        long = "transfer",
        value_name = "TRANSFERID",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub transfer: Option<String>,
    /// Task ID to act on, or nothing to list tasks by state.
    #[arg(
        short = 'A',
        long = "attach",
        value_name = "ASYNCID",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub attach: Option<String>,
    /// Stop the task named by -A.
    #[arg(long = "stop")]
    pub stop_task: bool,
    /// Send literal text to the task named by -A.
    #[arg(long = "text", value_name = "TEXT")]
    pub task_text: Option<String>,
    /// Send a remote file's bytes to the task named by -A.
    #[arg(long = "file", value_name = "PATH")]
    pub task_file: Option<String>,
    /// Show the task's state without reading its output.
    #[arg(long = "status")]
    pub task_status: bool,
    /// Read the task's buffered output.
    #[arg(long = "read")]
    pub task_read: bool,
    /// Remove the task from the host and from its history.
    #[arg(long = "remove")]
    pub task_remove: bool,
    /// Send literal text to the task named by -A.
    #[arg(long = "input", value_name = "TEXT")]
    pub task_input: Option<String>,
    /// Send a remote file's bytes to the task named by -A.
    #[arg(long = "input-file", value_name = "PATH")]
    pub task_input_file: Option<String>,
    /// stdin written before the command runs (stdio only).
    #[arg(long)]
    pub stdin: Option<String>,
    /// Keep stdin open after the initial input.
    #[arg(long = "keep-stdin")]
    pub keep_stdin: bool,
    /// Remote working directory for the bare run form.
    #[arg(long = "rcwd", value_name = "DIR")]
    pub rcwd: Option<String>,
    /// Skip the shell: run the command as an executable with arguments.
    #[arg(long)]
    pub direct: bool,
    /// Task lifetime in milliseconds, independent of this CLI call.
    #[arg(long = "task-ttl", value_name = "MS")]
    pub task_ttl: Option<u64>,
    /// Write the raw pty byte stream to exactly this file.
    #[arg(long = "raw", value_name = "FILE")]
    pub raw: Option<PathBuf>,
    /// Write the png screenshot to exactly this file.
    #[arg(long = "png", value_name = "FILE")]
    pub png: Option<PathBuf>,
    /// Title for the bare run form.
    #[arg(long = "title", value_name = "TEXT")]
    pub title: Option<String>,
    /// Description for the bare run form.
    #[arg(short = 'd', long = "desc", alias = "description", value_name = "TEXT")]
    pub description: Option<String>,
    /// Stay attached this many milliseconds; 0 reads what is buffered now.
    #[arg(
        short = 't',
        long = "timeout",
        value_name = "MS",
        default_value_t = 0,
        value_parser = clap::value_parser!(u64).range(0..=10000)
    )]
    pub wait_ms: u64,
    /// Capture stdout/stderr separately (bare form default).
    #[arg(long, conflicts_with = "pty")]
    pub stdio: bool,
    /// Allocate a terminal (bare form), optionally `<COLSxROWS>`.
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "80x24",
        value_name = "COLSxROWS"
    )]
    pub pty: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

/// Where a payload block can go. A single call may pick several.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Sink {
    /// Inline in the card (truncated at --text-limit).
    Text,
    /// Whole body in a .txt file.
    File,
    /// Raw pty stream in a .bin file.
    Raw,
    /// Screenshot in a .png file.
    Png,
}
#[derive(Subcommand)]
enum Command {
    /// Run this binary as the persistent rexetermd host (native local IPC).
    #[command(long_flag = "daemon", visible_alias = "deamon")]
    /// Add .rexeterm.disable in the current directory; no host is needed.
    Disable,
    /// Remove .rexeterm.disable from the current directory; no host is needed.
    Enable,
    /// Start, stop or inspect the local rexetermd service.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Show this invocation's resolved service, executor and sinks.
    Info,
    /// List what the bound host supports.
    Capabilities,
    /// Manage executors: live connections and the saved entries they use.
    Executor {
        #[command(subcommand)]
        command: ExecutorCommand,
    },
    /// Remote file operations.
    File {
        #[command(subcommand)]
        command: FileCommand,
    },
    /// Read or write host preferences.
    Preferences {
        #[command(subcommand)]
        command: PreferenceCommand,
    },
    /// Native IPC operation. JSON arguments are accepted; output is Markdown.
    Api {
        /// Operation name, for example `list_executor`.
        operation: String,
        #[arg(long, conflicts_with = "input")]
        body: Option<String>,
        /// Read JSON arguments from a local file.
        #[arg(long)]
        input: Option<PathBuf>,
        /// Save the JSON result to a new local file.
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
}
/// What the invocation does: a subcommand, or the bare-flag form that replaced
/// `run`, `task`, `transfer`, `forward` and `resources`.
enum Action {
    Info,
    Capabilities,
    Executor(ExecutorCommand),
    File(FileCommand),
    Preferences(PreferenceCommand),
    Api {
        operation: String,
        body: Option<String>,
        input: Option<PathBuf>,
        output: Option<PathBuf>,
        idempotency_key: Option<String>,
    },
    Run(RunArgs),
    Task(TaskCommand),
    Transfer(TransferCommand),
    Forward(ForwardCommand),
}

impl Action {
    /// Whether this action talks to an executor and needs one resolved.
    fn needs_executor(&self) -> bool {
        match self {
            Action::Info | Action::Capabilities => false,
            Action::Executor(command) => command.needs_executor(),
            Action::Preferences { .. } => false,
            Action::Api { .. } => false,
            Action::Run(_) | Action::Task(_) | Action::File(_) => true,
            Action::Transfer(_) | Action::Forward(_) => true,
        }
    }

    /// One executor for the whole invocation: `rexeterm -e local <command>`.
    fn set_executor(&mut self, id: &str) {
        match self {
            Action::Executor(command) => command.set_executor(id),
            Action::File(command) => command.set_executor(id),
            Action::Preferences(command) => command.set_executor(id),
            Action::Run(args) => args.host = id.to_owned(),
            Action::Task(command) => command.set_executor(id),
            Action::Transfer(command) => command.set_executor(id),
            Action::Forward(command) => command.set_executor(id),
            _ => {}
        }
    }
}
impl Args {
    /// The host role: `rexeterm service runasdaemon`, started by this CLI.
    pub fn daemon(&self) -> Option<(DaemonArgs, Option<String>, Option<PathBuf>)> {
        match self.command.as_ref() {
            Some(Command::Service {
                command: ServiceCommand::Runasdaemon(options),
            }) => Some((
                options.clone(),
                self.service_id.clone(),
                self.registry_dir.clone(),
            )),
            _ => None,
        }
    }
}
impl TaskCommand {
    fn set_executor(&mut self, id: &str) {
        match self {
            TaskCommand::List { host }
            | TaskCommand::Status { host, .. }
            | TaskCommand::CloseStdin { host, .. }
            | TaskCommand::Stop { host, .. }
            | TaskCommand::Remove { host, .. }
            | TaskCommand::Forget { host, .. } => *host = id.to_owned(),
            TaskCommand::History { host } => *host = Some(id.to_owned()),
            TaskCommand::Run(args) => args.host = id.to_owned(),
            TaskCommand::Read(args) | TaskCommand::InputFile { task: args, .. } => {
                args.host = id.to_owned()
            }
            TaskCommand::Input { task, .. } => task.host = id.to_owned(),
        }
    }
}
impl FileCommand {
    fn set_executor(&mut self, id: &str) {
        match self {
            FileCommand::Read(args) => args.host = id.to_owned(),
            FileCommand::Ls { host, .. }
            | FileCommand::Stat { host, .. }
            | FileCommand::Glob { host, .. }
            | FileCommand::Rg { host, .. } => *host = id.to_owned(),
            FileCommand::Create { target, .. }
            | FileCommand::Delete(target)
            | FileCommand::Rename { target, .. }
            | FileCommand::Patch { target, .. }
            | FileCommand::Upload { target, .. }
            | FileCommand::Download { target, .. } => target.host = id.to_owned(),
        }
    }
}
impl TransferCommand {
    fn set_executor(&mut self, id: &str) {
        if let TransferCommand::Create { host, .. } | TransferCommand::HostFile { host, .. } = self
        {
            *host = id.to_owned();
        }
    }
}
impl ForwardCommand {
    fn set_executor(&mut self, id: &str) {
        match self {
            ForwardCommand::List { host }
            | ForwardCommand::Start { host, .. }
            | ForwardCommand::Stop { host, .. } => *host = id.to_owned(),
        }
    }
}
impl PreferenceCommand {
    fn set_executor(&mut self, id: &str) {
        match self {
            PreferenceCommand::Shells { host, .. } => *host = Some(id.to_owned()),
            PreferenceCommand::SetShell { host, .. } => *host = id.to_owned(),
            PreferenceCommand::Shell { command } => command.set_executor(id),
            _ => {}
        }
    }
}
impl ShellCommand {
    fn set_executor(&mut self, id: &str) {
        match self {
            ShellCommand::List { host, .. } | ShellCommand::Default { host, .. } => {
                *host = Some(id.to_owned())
            }
            ShellCommand::Set { host, .. } => *host = Some(id.to_owned()),
        }
    }
}
#[derive(Subcommand)]
enum HostCommand {
    /// List saved hosts.
    List,
    /// Show one saved host.
    Show {
        /// Executor name.
        #[arg(skip)]
        host: String,
    },
    /// Save a new host.
    Add {
        /// Executor name to save.
        #[arg(skip)]
        host: String,
        /// Host URL, for example tls://host:45957.
        url: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        root: String,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        connection_profile: Option<PathBuf>,
    },
    /// Patch saved host fields using a JSON object (credentials belong in a private profile).
    Edit {
        /// Executor name.
        #[arg(skip)]
        host: String,
        /// JSON object with the fields to patch.
        #[arg(long)]
        options: String,
    },
    /// Delete a saved host.
    Remove {
        /// Executor name.
        #[arg(skip)]
        host: String,
    },
    /// Connect to this saved host.
    Connect {
        /// Executor name.
        #[arg(skip)]
        host: String,
    },
    /// Disconnect from this saved host.
    Disconnect {
        /// Executor name.
        #[arg(skip)]
        host: String,
    },
    /// Show connection state for this host.
    Status {
        /// Executor name.
        #[arg(skip)]
        host: String,
    },
}
#[derive(Subcommand)]
enum ExecutorCommand {
    /// List live executors and the saved entries behind them.
    List,
    /// Show one executor's connection state.
    Status {
        #[arg(skip)]
        host: String,
    },
    /// Connect the executor named by `-e`.
    Connect {
        /// Private SDK options file (TLS/auth/multiple links); its `id` wins over `-e`.
        #[arg(long)]
        options_file: Option<PathBuf>,
        #[arg(skip)]
        host: String,
    },
    /// Drop the live connection to the executor named by `-e`.
    Disconnect {
        #[arg(skip)]
        host: String,
    },
    /// Saved executor entries in `.rexeterm/config-dir` (client data, no host needed).
    Host {
        #[command(subcommand)]
        command: HostEntryCommand,
    },
}

#[derive(Subcommand)]
enum HostEntryCommand {
    /// List saved entries.
    List,
    /// Save a new entry.
    Add {
        #[arg(skip)]
        host: String,
        /// Host URL, for example tls://host:45957.
        url: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        root: String,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        connection_profile: Option<PathBuf>,
    },
    /// Patch saved fields with a JSON object.
    Edit {
        #[arg(skip)]
        host: String,
        /// JSON object with the fields to patch.
        #[arg(long)]
        options: String,
    },
    /// Delete a saved entry.
    Remove {
        #[arg(skip)]
        host: String,
    },
}

impl ExecutorCommand {
    /// Connection commands need `-e`; local entry commands only use it as the id.
    fn needs_executor(&self) -> bool {
        matches!(self, ExecutorCommand::Status { .. })
    }

    fn set_executor(&mut self, id: &str) {
        match self {
            ExecutorCommand::Status { host }
            | ExecutorCommand::Connect { host, .. }
            | ExecutorCommand::Disconnect { host } => *host = id.to_owned(),
            ExecutorCommand::Host { command } => match command {
                HostEntryCommand::Add { host, .. }
                | HostEntryCommand::Edit { host, .. }
                | HostEntryCommand::Remove { host } => *host = id.to_owned(),
                HostEntryCommand::List => {}
            },
            ExecutorCommand::List => {}
        }
    }
}

#[derive(Clone, ValueEnum)]
enum TaskIo {
    Stdio,
    Pty,
}
#[derive(ClapArgs)]
struct RunArgs {
    /// Executor name; `-e` fills this in.
    #[arg(skip)]
    host: String,
    /// Command line handed to the shell unchanged.
    #[arg(long = "cmd", value_name = "COMMAND")]
    cmd: Option<String>,
    /// Same as `-cmd`, kept for positional callers.
    command: Option<String>,
    /// Shell profile from `shell list`; omitted means the executor's own default
    /// (`powershell` on Windows, `bash` elsewhere).
    #[arg(short = 's', long = "shell", conflicts_with = "direct")]
    shell: Option<String>,
    /// Capture stdout/stderr separately (default).
    #[arg(long, conflicts_with = "pty")]
    stdio: bool,
    /// Allocate a terminal; columns x rows may be given, for example `--pty 100x30`.
    #[arg(
        long,
        num_args = 0..=1,
        default_missing_value = "80x24",
        value_name = "COLSxROWS"
    )]
    pty: Option<String>,
    /// stdin written before the command runs (stdio only).
    #[arg(long)]
    stdin: Option<String>,
    /// Keep stdin open after the initial input.
    #[arg(long)]
    keep_stdin: bool,
    /// Remote working directory.
    #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
    cwd: Option<String>,
    /// Skip the shell: run the command as an executable with arguments.
    #[arg(long)]
    direct: bool,
    /// Task description shown in the card.
    #[arg(short = 'd', long = "desc", alias = "description")]
    description: Option<String>,
    /// Task title (up to 40 Unicode characters); defaults to 未命名.
    #[arg(long = "title")]
    title: Option<String>,
    /// Detect output for this many milliseconds; 0 detaches immediately.
    #[arg(
        short = 't',
        long = "timeout",
        value_name = "MS",
        default_value_t = 0,
        value_parser = clap::value_parser!(u64).range(0..=10000)
    )]
    wait_ms: u64,
    /// Task lifetime in milliseconds; independent of this CLI call.
    #[arg(long = "task-ttl", value_name = "MS")]
    task_timeout_ms: Option<u64>,
}
impl RunArgs {
    fn io(&self) -> TaskIo {
        if self.pty.is_some() {
            TaskIo::Pty
        } else {
            TaskIo::Stdio
        }
    }
    /// `--pty 100x30` (or bare `--pty`) as (cols, rows).
    fn pty_size(&self) -> Option<(u16, u16)> {
        let (cols, rows) = self.pty.as_deref()?.split_once('x')?;
        Some((cols.trim().parse().ok()?, rows.trim().parse().ok()?))
    }
    fn command(&self) -> Result<String> {
        self.cmd
            .clone()
            .or_else(|| self.command.clone())
            .context("pass the command with -cmd '<command>'")
    }
}
#[derive(ClapArgs)]
struct ReadTaskArgs {
    /// Executor name that owns the task.
    #[arg(skip)]
    host: String,
    /// Task ID from a previous `run`.
    task: String,
    /// How long to wait for new output, in milliseconds.
    #[arg(long, default_value_t=1000, value_parser=clap::value_parser!(u64).range(0..=10000))]
    wait_ms: u64,
    /// Replay the whole client-side buffer instead of resuming after the last read.
    #[arg(long)]
    from_start: bool,
    /// Resume at a byte offset reported by a previous read.
    #[arg(long)]
    since: Option<u64>,
}
#[derive(Subcommand)]
enum TaskCommand {
    /// List tasks the host tracks for one executor.
    List {
        /// Executor name.
        #[arg(skip)]
        host: String,
    },
    /// Start a task; same flags as the top-level `run`.
    Run(RunArgs),
    /// Show one task's state without reading output.
    Status {
        /// Executor name.
        #[arg(skip)]
        host: String,
        /// Task ID.
        task: String,
    },
    /// Read buffered task output; resumes after the last read cursor.
    Read(ReadTaskArgs),
    /// Send literal text (not escape sequences). Does not read stdin or require a TTY.
    Input {
        #[command(flatten)]
        task: ReadTaskArgs,
        text: String,
    },
    /// Send bytes from a remote file (RE's 4 KiB input limit applies).
    InputFile {
        #[command(flatten)]
        task: ReadTaskArgs,
        path: String,
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
    },
    /// Close the task's stdin without stopping it.
    CloseStdin {
        /// Executor name.
        #[arg(skip)]
        host: String,
        /// Task ID.
        task: String,
    },
    /// Stop a running task.
    Stop {
        /// Executor name.
        #[arg(skip)]
        host: String,
        /// Task ID.
        task: String,
    },
    /// Remove a server task, then remove any completed outcome from host history.
    Remove {
        /// Executor name.
        #[arg(skip)]
        host: String,
        /// Task ID.
        task: String,
    },
    /// Delete only the host's completed outcome record.
    Forget {
        /// Executor name.
        #[arg(skip)]
        host: String,
        /// Task ID.
        task: String,
    },
    /// List completed outcomes the host still keeps.
    History {
        /// Executor name; all executors when omitted.
        #[arg(skip)]
        host: Option<String>,
    },
}
#[derive(ClapArgs)]
struct ReadArgs {
    /// Executor name.
    #[arg(skip)]
    host: String,
    /// Remote path.
    path: String,
    #[arg(long)]
    binary: bool,
    /// Text/directory offset is 1-based; binary offset is 0-based.
    #[arg(long)]
    offset: Option<u64>,
    #[arg(long)]
    limit: Option<u64>,
    #[arg(long)]
    hash: bool,
    #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
    cwd: Option<String>,
}
enum FileVerb {
    Create {
        target: FileTarget,
        content: Option<String>,
        input: Option<PathBuf>,
    },
    Delete(FileTarget),
    Rename {
        target: FileTarget,
        destination: String,
    },
    Patch {
        target: FileTarget,
        input: PathBuf,
        binary: bool,
        hash_code: Option<String>,
    },
}
#[derive(Subcommand)]
enum FileCommand {
    /// Read a remote file as text or binary, with offset and limit.
    Read(ReadArgs),
    /// List one folder (defaults to the working directory).
    Ls {
        /// Folder to list.
        path: Option<String>,
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
        #[arg(skip)]
        host: String,
    },
    /// Stat a remote path.
    Stat {
        /// Remote path.
        path: String,
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
        #[arg(skip)]
        host: String,
    },
    /// Match remote paths by glob inside a folder.
    Glob {
        /// Glob pattern, for example `**/*.rs`.
        pattern: String,
        /// Folder to search; defaults to the working directory.
        folder: Option<String>,
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
        #[arg(skip)]
        host: String,
    },
    /// Search remote file contents with a regex inside a folder.
    Rg {
        /// Regular expression.
        pattern: String,
        /// Folder to search; defaults to the working directory.
        folder: Option<String>,
        /// Case-insensitive match.
        #[arg(long)]
        ignore_case: bool,
        /// Maximum matches per file.
        #[arg(long)]
        max_count: Option<u32>,
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
        #[arg(skip)]
        host: String,
    },
    /// Create a remote text file from literal text or a local UTF-8 file.
    Create {
        #[command(flatten)]
        target: FileTarget,
        #[arg(long, required_unless_present = "input", conflicts_with = "input")]
        content: Option<String>,
        #[arg(long)]
        input: Option<PathBuf>,
    },
    /// Delete a remote file or empty directory.
    Delete(FileTarget),
    /// Rename or move a remote path.
    Rename {
        #[command(flatten)]
        target: FileTarget,
        /// New remote path.
        destination: String,
    },
    /// Apply RE's text/binary patch format from a local UTF-8 patch file.
    Patch {
        #[command(flatten)]
        target: FileTarget,
        /// Local patch file.
        input: PathBuf,
        #[arg(long)]
        binary: bool,
        #[arg(long)]
        hash_code: Option<String>,
    },
    /// Copy a local file to a remote path.
    Upload {
        #[command(flatten)]
        target: FileTarget,
        /// Local source path.
        source: PathBuf,
        #[arg(long)]
        overwrite: bool,
        #[command(flatten)]
        metadata: Metadata,
    },
    /// Copy a remote file to a local path.
    Download {
        #[command(flatten)]
        target: FileTarget,
        /// Local destination path.
        output: PathBuf,
        #[arg(long)]
        overwrite: bool,
        #[command(flatten)]
        metadata: Metadata,
    },
}
#[derive(ClapArgs)]
struct FileTarget {
    /// Executor name.
    #[arg(skip)]
    host: String,
    /// Remote path.
    path: String,
    #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
    cwd: Option<String>,
}
#[derive(ClapArgs, Default)]
struct Metadata {
    /// Transfer or task title (up to 40 Unicode characters).
    #[arg(long, default_value = "未命名")]
    title: String,
    /// Free-form description.
    #[arg(long, default_value = "")]
    description: String,
    #[arg(long)]
    sha256: Option<String>,
}
#[derive(Clone, ValueEnum)]
enum Direction {
    Upload,
    Download,
}
#[derive(Subcommand)]
enum TransferCommand {
    List,
    Show {
        id: String,
    },
    Cancel {
        id: String,
    },
    /// Create a staged transfer and return its ID immediately. Upload content is a separate command.
    Create {
        #[arg(skip)]
        host: String,
        #[arg(value_enum)]
        mode: Direction,
        path: String,
        #[arg(long)]
        size: Option<u64>,
        #[arg(long)]
        overwrite: bool,
        #[command(flatten)]
        metadata: Metadata,
    },
    Put {
        id: String,
        input: PathBuf,
    },
    Get {
        id: String,
        output: PathBuf,
        #[arg(long)]
        overwrite: bool,
    },
    /// Direct SDK transfer: local path belongs to the host; supports resume offsets.
    HostFile {
        #[arg(skip)]
        host: String,
        #[arg(value_enum)]
        mode: Direction,
        local_path: PathBuf,
        target_path: String,
        #[arg(long)]
        offset: Option<u64>,
        #[arg(long)]
        overwrite: bool,
        #[command(flatten)]
        metadata: Metadata,
    },
}
#[derive(Subcommand)]
enum ForwardCommand {
    List {
        #[arg(skip)]
        host: String,
    },
    Start {
        #[arg(skip)]
        host: String,
        listen: String,
        target: String,
        #[arg(long)]
        max_connections: Option<usize>,
        #[arg(long, default_value = "未命名")]
        title: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    Stop {
        #[arg(skip)]
        host: String,
        id: String,
    },
}
#[derive(Subcommand)]
enum PreferenceCommand {
    /// Show the stored client preferences as JSON.
    Show,
    /// Shell profiles and defaults used by `run --shell`.
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
    Shells {
        #[arg(skip)]
        host: Option<String>,
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
    },
    /// Point an executor at an existing shell profile.
    SetShell {
        /// Executor name.
        #[arg(skip)]
        host: String,
        /// Profile name from `preferences shell list`.
        shell: String,
        /// Remote working directory the setting applies to.
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
    },
    /// Re-read preferences.json from disk.
    Reload,
}
#[derive(Subcommand)]
enum ShellCommand {
    /// List effective shell profiles (built-in + stored) and the defaults.
    List {
        /// Executor name; global profiles when omitted.
        #[arg(skip)]
        host: Option<String>,
        /// Remote working directory; use the executor's profile when omitted.
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
        /// Show the stored global / executor / directory layers next to the result.
        #[arg(long)]
        layers: bool,
    },
    /// Store a shell profile in rexeterm preferences.
    Set {
        /// Profile name used by `run --shell`.
        name: String,
        /// Program to run, for example python3 or C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe.
        #[arg(long)]
        program: String,
        /// Argument before the command; repeat to build the full argument list. Defaults to -c {command}.
        #[arg(long = "command-arg", allow_hyphen_values = true)]
        command_args: Vec<String>,
        /// Argument used for interactive sessions; repeat as needed.
        #[arg(long = "interactive-arg", allow_hyphen_values = true)]
        interactive_args: Vec<String>,
        /// Executor name; store the profile globally when omitted.
        #[arg(skip)]
        host: Option<String>,
        /// Remote working directory the profile applies to.
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
        /// Also make this profile the default shell.
        #[arg(long)]
        default: bool,
    },
    /// Choose the default shell for a scope: global, executor, or directory.
    Default {
        /// Profile name from `preferences shell list`.
        name: String,
        /// Executor name; the global default when omitted.
        #[arg(skip)]
        host: Option<String>,
        /// Remote working directory; needs --host, sets the directory layer.
        #[arg(long = "rcwd", id = "rcwd", value_name = "DIR")]
        cwd: Option<String>,
    },
}
fn parse_object(text: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(text)?;
    ensure!(value.is_object(), "options must be a JSON object");
    Ok(value)
}
/// Directory used for the per-folder preference layer: `--rcwd` if given,
/// otherwise the working directory this CLI runs in.
fn preference_directory(explicit: Option<&str>) -> Option<String> {
    explicit.map(str::to_string).or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    })
}

async fn rpc(
    client: &IpcClient,
    host: Option<&str>,
    tool: &str,
    params: Value,
    cwd: Option<&str>,
) -> Result<Value> {
    // `directory` only feeds the per-folder preference layer, so falling back to
    // the directory this invocation runs in matches user expectations.
    let directory = preference_directory(cwd);
    client
        .call(
            "rpc",
            json!({"tool":tool,"executor":host,"params":params,"directory":directory}),
        )
        .await
}
fn optional(params: &mut Value, key: &str, value: impl Into<Value>) {
    let value = value.into();
    if !value.is_null() {
        params[key] = value;
    }
}
async fn run(client: &IpcClient, args: RunArgs) -> Result<Value> {
    let command = args.command()?;
    let pty_size = args.pty_size();
    let mut params = json!({"mode":if args.direct {"run"} else {"shell"},"command":command,"read_timeout":args.wait_ms,"io":match args.io() {TaskIo::Stdio=>"stdio",TaskIo::Pty=>"pty"}});
    if args.stdin.is_some() || args.keep_stdin {
        ensure!(
            matches!(args.io(), TaskIo::Stdio),
            "stdin/keep-stdin require --stdio"
        );
        optional(&mut params, "stdin", args.stdin);
        params["keepStdin"] = json!(args.keep_stdin);
    }
    if let Some((cols, rows)) = pty_size {
        params["cols"] = json!(cols);
        params["rows"] = json!(rows);
    }
    optional(&mut params, "shell", args.shell.clone());
    if !args.direct {
        // The CLI owns preferences, so it resolves the shell command itself and
        // the host receives a ready-to-run command instead of a profile name.
        let store = preferences::store()?;
        let facts = preferences::executor_facts(client, &args.host).await?;
        ensure!(
            facts.client_shell_command,
            "executor {} does not support clientShellCommand; upgrade the executor to use client shell preferences",
            args.host
        );
        let directory = preference_directory(args.cwd.as_deref());
        let command = store.shell_command(
            &args.host,
            directory.as_deref(),
            args.shell.as_deref(),
            facts.system.as_deref(),
            &command,
        )?;
        params["shellCommand"] = serde_json::to_value(command)?;
    }
    optional(&mut params, "title", args.title);
    optional(&mut params, "description", args.description);
    optional(&mut params, "timeout", args.task_timeout_ms);
    let mut result = rpc(
        client,
        Some(&args.host),
        "exbash",
        params,
        args.cwd.as_deref(),
    )
    .await?;
    result["metadata"]["outputObservation"] = json!(OUTPUT_OBSERVATION_LIVE);
    Ok(result)
}
async fn task_command(client: &IpcClient, command: TaskCommand) -> Result<Value> {
    match command {
        TaskCommand::List { host } => client.call("tasks.list", json!({"host":host})).await,
        TaskCommand::Run(args) => run(client, args).await,
        TaskCommand::Status { host, task } => {
            client
                .call("tasks.status", json!({"host":host,"id":task}))
                .await
        }
        TaskCommand::Read(task) => read_task(client, task).await,
        TaskCommand::Input { task, text } => {
            attach(client, task, Some(text.replace('\\', "\\\\")), None, None).await
        }
        TaskCommand::InputFile { task, path, cwd } => {
            attach(client, task, None, Some(path), cwd.as_deref()).await
        }
        TaskCommand::CloseStdin { host, task } => rpc(
            client,
            Some(&host),
            "exbash",
            json!({"mode":"attach","asyncID":task,"io":"stdio","closeStdin":true,"read_timeout":0}),
            None,
        )
        .await,
        TaskCommand::Stop { host, task } => {
            client
                .call("tasks.stop", json!({"host":host,"id":task}))
                .await
        }
        TaskCommand::Forget { host, task } => {
            client
                .call("tasks.forget", json!({"host":host,"id":task}))
                .await
        }
        TaskCommand::Remove { host, task } => {
            let status = client
                .call("tasks.status", json!({"host":host,"id":task}))
                .await?;
            if status["state"] == "running" {
                client
                    .call("tasks.stop", json!({"host":host,"id":task}))
                    .await?;
            }
            if let Err(error) = rpc(
                client,
                Some(&host),
                "exbash",
                json!({"mode":"remove","asyncID":task}),
                None,
            )
            .await
            {
                ensure!(
                    error.to_string().to_lowercase().contains("not found"),
                    "{error}"
                );
            }
            client
                .call("tasks.forget", json!({"host":host,"id":task}))
                .await
        }
        TaskCommand::History { host } => {
            let value = rpc(client, None, "task_history", json!({}), None).await?;
            let mut history = value["metadata"].clone();
            if let (Some(host), Some(tasks)) = (host, history["tasks"].as_array_mut()) {
                tasks.retain(|t| t["host"] == host);
            }
            Ok(history)
        }
    }
}
/// Shared note for reply metadata; the client buffer, not the host, keeps history.
const OUTPUT_OBSERVATION_LIVE: &str =
    "Live output for this call; task read serves the client-side buffer.";
const OUTPUT_OBSERVATION_BUFFERED: &str =
    "Client buffer keeps the most recent 256 KiB; reads resume after the last cursor.";
async fn read_task(client: &IpcClient, task: ReadTaskArgs) -> Result<Value> {
    let mut params = json!({
        "host": task.host,
        "id": task.task,
        "wait_ms": task.wait_ms,
        "from_start": task.from_start,
    });
    if let Some(since) = task.since {
        params["since"] = json!(since);
    }
    let mut result = client.call("tasks.read", params).await?;
    result["metadata"]["outputObservation"] = json!(OUTPUT_OBSERVATION_BUFFERED);
    Ok(result)
}
async fn attach(
    client: &IpcClient,
    task: ReadTaskArgs,
    text: Option<String>,
    file: Option<String>,
    cwd: Option<&str>,
) -> Result<Value> {
    let mut params = json!({"mode":"attach","asyncID":task.task,"read_timeout":task.wait_ms});
    optional(&mut params, "text", text);
    optional(&mut params, "filePath", file);
    let mut result = rpc(client, Some(&task.host), "exbash", params, cwd).await?;
    result["metadata"]["outputObservation"] = json!(OUTPUT_OBSERVATION_LIVE);
    Ok(result)
}
async fn read(client: &IpcClient, args: ReadArgs) -> Result<Value> {
    let mut params = json!({"filePath":args.path,"mode":if args.binary {"binary"} else {"text"},"hashCheckMode":args.hash});
    optional(&mut params, "offset", args.offset);
    optional(&mut params, "limit", args.limit);
    rpc(
        client,
        Some(&args.host),
        "read",
        params,
        args.cwd.as_deref(),
    )
    .await
}
async fn stat(client: &IpcClient, host: &str, path: &str, cwd: Option<&str>) -> Result<Value> {
    rpc(client, Some(host), "stat", json!({"filePath":path}), cwd).await
}
/// `file upload`: local file to remote path in one hop.
async fn upload(
    client: &IpcClient,
    target: FileTarget,
    source: PathBuf,
    overwrite: bool,
    metadata: Metadata,
) -> Result<Value> {
    let mut params = transfer_body(
        Direction::Upload,
        target.path.clone(),
        None,
        overwrite,
        metadata,
    );
    params.as_object_mut().unwrap().remove("path");
    params["targetPath"] = json!(target.path);
    params["localPath"] = json!(std::fs::canonicalize(&source)?);
    rpc(
        client,
        Some(&target.host),
        "file_transfer",
        params,
        target.cwd.as_deref(),
    )
    .await
}
/// `file download`: remote path to a local file in one hop.
async fn download(
    client: &IpcClient,
    target: FileTarget,
    output: PathBuf,
    overwrite: bool,
    metadata: Metadata,
) -> Result<Value> {
    let mut params = transfer_body(
        Direction::Download,
        target.path.clone(),
        None,
        overwrite,
        metadata,
    );
    params.as_object_mut().unwrap().remove("path");
    params["targetPath"] = json!(target.path);
    params["localPath"] = json!(output);
    rpc(
        client,
        Some(&target.host),
        "file_transfer",
        params,
        target.cwd.as_deref(),
    )
    .await
}
async fn file_command(client: &IpcClient, command: FileCommand) -> Result<Value> {
    match command {
        FileCommand::Read(args) => read(client, args).await,
        FileCommand::Ls { path, cwd, host } => {
            let mut params = json!({"pattern":"*"});
            optional(&mut params, "path", path);
            rpc(client, Some(&host), "glob", params, cwd.as_deref()).await
        }
        FileCommand::Stat { host, path, cwd } => stat(client, &host, &path, cwd.as_deref()).await,
        FileCommand::Glob {
            host,
            pattern,
            folder,
            cwd,
        } => {
            let mut params = json!({"pattern":pattern});
            optional(&mut params, "path", folder);
            rpc(client, Some(&host), "glob", params, cwd.as_deref()).await
        }
        FileCommand::Rg {
            host,
            pattern,
            folder,
            ignore_case,
            max_count,
            cwd,
        } => {
            let mut params = json!({"pattern":pattern,"case_sensitive":!ignore_case});
            optional(&mut params, "max_count", max_count);
            optional(&mut params, "root", folder);
            rpc(client, Some(&host), "rg", params, cwd.as_deref()).await
        }
        FileCommand::Create {
            target,
            content,
            input,
        } => {
            file_verb(
                client,
                FileVerb::Create {
                    target,
                    content,
                    input,
                },
            )
            .await
        }
        FileCommand::Delete(target) => file_verb(client, FileVerb::Delete(target)).await,
        FileCommand::Rename {
            target,
            destination,
        } => {
            file_verb(
                client,
                FileVerb::Rename {
                    target,
                    destination,
                },
            )
            .await
        }
        FileCommand::Patch {
            target,
            input,
            binary,
            hash_code,
        } => {
            file_verb(
                client,
                FileVerb::Patch {
                    target,
                    input,
                    binary,
                    hash_code,
                },
            )
            .await
        }
        FileCommand::Upload {
            target,
            source,
            overwrite,
            metadata,
        } => upload(client, target, source, overwrite, metadata).await,
        FileCommand::Download {
            target,
            output,
            overwrite,
            metadata,
        } => download(client, target, output, overwrite, metadata).await,
    }
}

async fn file_verb(client: &IpcClient, verb: FileVerb) -> Result<Value> {
    let (target, mut params) = match verb {
        FileVerb::Create {
            target,
            content,
            input,
        } => (
            target,
            json!({"mode":"create","content": match input {Some(p)=>tokio::fs::read_to_string(p).await?,None=>content.unwrap_or_default()}}),
        ),
        FileVerb::Delete(target) => (target, json!({"mode":"delete"})),
        FileVerb::Rename {
            target,
            destination,
        } => (target, json!({"mode":"rename","newFilePath":destination})),
        FileVerb::Patch {
            target,
            input,
            binary,
            hash_code,
        } => {
            let mut params = json!({"mode":"patch","patchText":tokio::fs::read_to_string(input).await?,"patchMode":if binary {"binary"} else {"text"},"hashCheckMode":hash_code.is_some()});
            optional(&mut params, "hashCode", hash_code);
            (target, params)
        }
    };
    let cwd = target.cwd.clone();
    params["filePath"] = json!(target.path);
    rpc(
        client,
        Some(&target.host),
        "FileAction",
        params,
        cwd.as_deref(),
    )
    .await
}
fn transfer_body(
    mode: Direction,
    path: String,
    size: Option<u64>,
    overwrite: bool,
    metadata: Metadata,
) -> Value {
    let mut body = json!({"mode":match mode {Direction::Upload=>"upload",Direction::Download=>"download"},"path":path,"overwrite":overwrite,"title":metadata.title,"description":metadata.description});
    optional(&mut body, "size", size);
    optional(&mut body, "sha256", metadata.sha256);
    body
}
async fn transfer_command(client: &IpcClient, command: TransferCommand) -> Result<Value> {
    match command {
        TransferCommand::List => client.call("transfers.list", json!({})).await,
        TransferCommand::Show { id } => client.call("transfers.show", json!({"id":id})).await,
        TransferCommand::Cancel { id } => client.call("transfers.cancel", json!({"id":id})).await,
        TransferCommand::Create {
            host,
            mode,
            path,
            size,
            overwrite,
            metadata,
        } => {
            client
                .call(
                    "transfers.create",
                    json!({"host":host,"options":transfer_body(mode,path,size,overwrite,metadata)}),
                )
                .await
        }
        TransferCommand::Put { id, input } => {
            client
                .call(
                    "transfers.put",
                    json!({"id":id,"localPath":std::fs::canonicalize(input)?}),
                )
                .await
        }
        TransferCommand::Get {
            id,
            output,
            overwrite,
        } => client.save_content(&id, output, overwrite).await,
        TransferCommand::HostFile {
            host,
            mode,
            local_path,
            target_path,
            offset,
            overwrite,
            metadata,
        } => {
            let mut params = transfer_body(mode, target_path.clone(), None, overwrite, metadata);
            params.as_object_mut().unwrap().remove("path");
            params["targetPath"] = json!(target_path);
            params["localPath"] = json!(local_path);
            optional(&mut params, "offset", offset);
            rpc(client, Some(&host), "file_transfer", params, None).await
        }
    }
}

pub async fn execute(mut args: Args) -> Result<Value> {
    let cwd = std::env::current_dir()?;
    let command = args.command.take();
    // Directory opt-out is answered before anything else.
    match &command {
        Some(Command::Disable) => return crate::directory::set(&cwd, true),
        Some(Command::Enable) => return crate::directory::set(&cwd, false),
        _ => crate::directory::check(&cwd)?,
    }
    let registry = Registry::open(args.registry_dir.clone())?;
    let command = match command {
        Some(Command::Service { command }) => {
            return services::execute(&registry, command, args.service_id.as_deref(), &cwd).await
        }
        other => other,
    };
    let action = match command {
        Some(Command::Info) => Action::Info,
        Some(Command::Capabilities) => Action::Capabilities,
        Some(Command::Executor { command }) => Action::Executor(command),
        Some(Command::File { command }) => Action::File(command),
        Some(Command::Preferences { command }) => Action::Preferences(command),
        Some(Command::Api {
            operation,
            body,
            input,
            output,
            idempotency_key,
        }) => Action::Api {
            operation,
            body,
            input,
            output,
            idempotency_key,
        },
        Some(_) => unreachable!("service and directory commands return before this"),
        None => bare_action(&args).context("name a command, or pass -cmd '<command>'")?,
    };
    // No `service create` needed: with no binding the host is started here.
    let id = match services::selected(&registry, args.service_id.as_deref(), &cwd) {
        Ok(id) => id,
        Err(error) if args.service_id.is_none() => {
            let _ = error;
            services::ensure(&registry, &cwd).await?
        }
        Err(error) => return Err(error),
    };
    let client = services::connect(&registry, &id).await?;
    let mut action = action;
    // `-e` names the executor; otherwise the host's default is used, and only
    // for actions that actually talk to one.
    match args.executor.clone() {
        Some(executor) => action.set_executor(&executor),
        None if action.needs_executor() => {
            let executor = default_executor(&client).await?;
            action.set_executor(&executor);
        }
        None => {}
    }
    execute_remote(&client, action).await
}

async fn default_executor(client: &IpcClient) -> Result<String> {
    let value = client
        .call("rpc", json!({"tool":"list_executor","params":{}}))
        .await?;
    value["metadata"]["default"]
        .as_str()
        .or_else(|| value["default"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("no executor is connected; run connect first"))
}
fn bare_action(args: &Args) -> Option<Action> {
    bare_forward(args)
        .or_else(|| bare_transfer(args))
        .or_else(|| bare_task(args))
        .or_else(|| bare_run(args))
}

/// `-F 8080:10.0.0.1:80` starts a forward; `-F` alone lists; `-F <id> -stop` stops.
fn bare_forward(args: &Args) -> Option<Action> {
    let value = args.forward.clone()?;
    let host = args.executor.clone().unwrap_or_default();
    if args.stop_task {
        return Some(Action::Forward(ForwardCommand::Stop { host, id: value }));
    }
    // `8080:10.0.0.1:80` listens on port 8080 of this machine; a full listen
    // address (`0.0.0.0:8080:10.0.0.1:80`) is accepted as well.
    let parts: Vec<&str> = value.split(':').collect();
    let parsed = match parts.as_slice() {
        [port, host_name, target] if port.chars().all(|c| c.is_ascii_digit()) => {
            Some((format!("127.0.0.1:{port}"), format!("{host_name}:{target}")))
        }
        [listen_host, port, host_name, target] => Some((
            format!("{listen_host}:{port}"),
            format!("{host_name}:{target}"),
        )),
        _ => None,
    };
    let Some((listen, target)) = parsed else {
        return Some(Action::Forward(ForwardCommand::List { host }));
    };
    Some(Action::Forward(ForwardCommand::Start {
        host,
        listen,
        target,
        max_connections: None,
        title: args.title.clone().unwrap_or_else(|| "未命名".to_owned()),
        description: args.description.clone().unwrap_or_default(),
    }))
}

fn bare_transfer(args: &Args) -> Option<Action> {
    let id = args.transfer.clone()?;
    let command = if args.stop_task {
        TransferCommand::Cancel { id }
    } else if id.is_empty() {
        TransferCommand::List
    } else {
        TransferCommand::Show { id }
    };
    Some(Action::Transfer(command))
}

fn bare_task(args: &Args) -> Option<Action> {
    let id = args.attach.clone()?;
    let host = args.executor.clone().unwrap_or_default();
    if id.is_empty() {
        return Some(Action::Task(TaskCommand::List { host }));
    }
    if args.stop_task {
        return Some(Action::Task(TaskCommand::Stop { host, task: id }));
    }
    if args.task_status {
        return Some(Action::Task(TaskCommand::Status { host, task: id }));
    }
    if args.task_remove {
        return Some(Action::Task(TaskCommand::Remove { host, task: id }));
    }
    // A fresh process has no read cursor, so a bare  asks for the
    // whole buffered transcript; otherwise it would always come back empty.
    let read = || ReadTaskArgs {
        host: host.clone(),
        task: id.clone(),
        wait_ms: args.wait_ms,
        from_start: args.task_read,
        since: None,
    };
    if args.task_read {
        return Some(Action::Task(TaskCommand::Read(read())));
    }
    if let Some(text) = args.task_input.clone().or_else(|| args.task_text.clone()) {
        return Some(Action::Task(TaskCommand::Input {
            task: read(),
            text: text.replace('\\', "\\\\"),
        }));
    }
    if let Some(path) = args
        .task_input_file
        .clone()
        .or_else(|| args.task_file.clone())
    {
        return Some(Action::Task(TaskCommand::InputFile {
            task: read(),
            path,
            cwd: None,
        }));
    }
    Some(Action::Task(TaskCommand::Read(read())))
}

fn bare_run(args: &Args) -> Option<Action> {
    let cmd = args.cmd.clone()?;
    Some(Action::Run(RunArgs {
        host: args.executor.clone().unwrap_or_default(),
        cmd: Some(cmd),
        command: None,
        shell: args.shell.clone(),
        stdio: args.stdio,
        pty: args.pty.clone(),
        stdin: args.stdin.clone(),
        keep_stdin: args.keep_stdin,
        cwd: args.rcwd.clone(),
        direct: args.direct,
        description: args.description.clone(),
        title: args.title.clone(),
        task_timeout_ms: args.task_ttl,
        wait_ms: args.wait_ms,
    }))
}

/// `-A` ordering: what needs attention first, what is done and seen last.
fn task_rank(task: &Value) -> u8 {
    let stopped = task["state"].as_str() == Some("stopped");
    let read = task["read"].as_bool() == Some(true);
    match (stopped, read) {
        (true, false) => 0,
        (false, _) => 1,
        (true, true) => 2,
    }
}
async fn execute_remote(client: &IpcClient, action: Action) -> Result<Value> {
    match action {
        Action::Info => client.call("info", json!({})).await,
        Action::Capabilities => client.call("capabilities", json!({})).await,
        Action::Executor(command) => executor_command(client, command).await,
        Action::Task(TaskCommand::List { host }) => {
            let value = task_command(client, TaskCommand::List { host }).await?;
            let mut tasks: Vec<Value> = ["active", "completed"]
                .iter()
                .filter_map(|key| value[key].as_array().cloned())
                .flatten()
                .collect();
            tasks.sort_by_key(task_rank);
            Ok(json!({"tasks": tasks}))
        }
        Action::Run(args) => run(client, args).await,
        Action::Task(command) => task_command(client, command).await,
        Action::File(command) => file_command(client, command).await,
        Action::Transfer(command) => transfer_command(client, command).await,
        Action::Forward(command) => match command {
            ForwardCommand::List { host } => {
                client.call("forwards.list", json!({"host":host})).await
            }
            ForwardCommand::Start {
                host,
                listen,
                target,
                max_connections,
                title,
                description,
            } => {
                let mut body = json!({"listen":listen,"target":{"kind":"tcp","address":target},"title":title,"description":description});
                optional(&mut body, "maxConnections", max_connections);
                client
                    .call("forwards.start", json!({"host":host,"options":body}))
                    .await
            }
            ForwardCommand::Stop { host, id } => {
                client
                    .call("forwards.stop", json!({"host":host,"id":id}))
                    .await
            }
        },
        Action::Preferences(command) => preferences_command(client, command).await,
        Action::Api {
            operation,
            body,
            input,
            output,
            idempotency_key,
        } => {
            let args = match (body, input) {
                (Some(text), _) => parse_object(&text)?,
                (_, Some(path)) => parse_object(&tokio::fs::read_to_string(path).await?)?,
                _ => json!({}),
            };
            let value = client
                .call_with_key(&operation, args, idempotency_key)
                .await?;
            if let Some(path) = output {
                use tokio::io::AsyncWriteExt;
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .await?;
                file.write_all(&serde_json::to_vec_pretty(&value)?).await?;
                file.sync_all().await?;
                Ok(json!({"output":path,"saved":true}))
            } else {
                Ok(value)
            }
        }
    }
}

/// Preferences are client data: they are read and written in this process.
async fn preferences_command(client: &IpcClient, command: PreferenceCommand) -> Result<Value> {
    match command {
        PreferenceCommand::Show => Ok(serde_json::to_value(preferences::store()?.settings())?),
        PreferenceCommand::Shells { host, cwd } => {
            preferences::shells(host.as_deref().unwrap_or_default(), cwd.as_deref())
        }
        PreferenceCommand::SetShell { host, shell, cwd } => {
            preferences::set_default(&host, cwd.as_deref(), &shell)
        }
        PreferenceCommand::Shell { command } => match command {
            ShellCommand::List { host, cwd, layers } => {
                let executor = host.as_deref().unwrap_or_default();
                let cwd = cwd.as_deref();
                if layers {
                    preferences::layers(executor, cwd)
                } else {
                    preferences::shells(executor, cwd)
                }
            }
            ShellCommand::Set {
                name,
                program,
                command_args,
                interactive_args,
                host,
                cwd,
                default,
            } => preferences::set_profile(
                host.as_deref().unwrap_or_default(),
                cwd.as_deref(),
                &name,
                &program,
                command_args,
                interactive_args,
                default,
            ),
            ShellCommand::Default { name, host, cwd } => {
                preferences::set_default(host.as_deref().unwrap_or_default(), cwd.as_deref(), &name)
            }
        },
        PreferenceCommand::Reload => client.call("preferences.reload", json!({})).await,
    }
}

/// Live executor commands talk to the host; `executor host …` edits client files.
async fn executor_command(client: &IpcClient, command: ExecutorCommand) -> Result<Value> {
    match command {
        ExecutorCommand::List => {
            let live = rpc(client, None, "list_executor", json!({}), None).await?;
            let mut executors = live["executors"].as_array().cloned().unwrap_or_default();
            let saved = crate::executors::list()?;
            // A saved entry that is not connected still shows up, marked as such.
            for entry in saved["executors"].as_array().cloned().unwrap_or_default() {
                let id = entry["id"].as_str().unwrap_or_default();
                let connected = executors.iter().any(|e| e["id"].as_str() == Some(id));
                if !connected {
                    let mut entry = entry;
                    entry["connected"] = json!(false);
                    executors.push(entry);
                }
            }
            Ok(json!({"executors": executors, "default": live["default"]}))
        }
        ExecutorCommand::Status { host } => {
            host_command(client, HostCommand::Status { host }).await
        }
        ExecutorCommand::Connect { options_file, host } => match options_file {
            Some(file) => {
                let params = parse_object(&tokio::fs::read_to_string(file).await?)?;
                // SDK connection configuration contains secrets; never echo it into output.
                let id = params["id"].as_str().map(str::to_owned);
                rpc(client, id.as_deref(), "connect_to_executor", params, None).await
            }
            None => {
                ensure!(
                    !host.is_empty(),
                    "name the executor to connect: -e <EXECUTOR>"
                );
                let entry = crate::executors::load(&host)?;
                rpc(client, Some(&host), "connect_to_executor", entry, None).await
            }
        },
        ExecutorCommand::Disconnect { host } => {
            host_command(client, HostCommand::Disconnect { host }).await
        }
        ExecutorCommand::Host { command } => match command {
            HostEntryCommand::List => crate::executors::list(),
            HostEntryCommand::Add {
                host,
                url,
                name,
                root,
                username,
                connection_profile,
            } => {
                ensure!(!host.is_empty(), "name the executor: -e <EXECUTOR>");
                crate::executors::save(
                    &host,
                    json!({
                        "id": host,
                        "name": name.unwrap_or_else(|| host.clone()),
                        "url": url,
                        "root": root,
                        "username": username,
                        "connectionProfile": connection_profile,
                    }),
                )
            }
            HostEntryCommand::Edit { host, options } => {
                ensure!(!host.is_empty(), "name the executor: -e <EXECUTOR>");
                crate::executors::edit(&host, parse_object(&options)?)
            }
            HostEntryCommand::Remove { host } => {
                ensure!(!host.is_empty(), "name the executor: -e <EXECUTOR>");
                crate::executors::remove(&host)
            }
        },
    }
}

async fn host_command(client: &IpcClient, command: HostCommand) -> Result<Value> {
    match command {
        HostCommand::List=>client.call("hosts.list",json!({})).await,
        HostCommand::Show {host}=>client.call("hosts.show",json!({"host":host})).await,
        HostCommand::Add {host,url,name,root,username,connection_profile}=>client.call("hosts.save",json!({"id":host,"name":name.unwrap_or_else(||host.clone()),"url":url,"root":root,"username":username,"connectionProfile":connection_profile})).await,
        HostCommand::Edit {host,options}=>client.call("hosts.edit",json!({"host":host,"options":parse_object(&options)?})).await,
        HostCommand::Remove {host}=>client.call("hosts.remove",json!({"host":host})).await,
        HostCommand::Connect {host}=>client.call("hosts.connect",json!({"host":host})).await,
        HostCommand::Disconnect {host}=>client.call("hosts.disconnect",json!({"host":host})).await,
        HostCommand::Status {host}=>client.call("hosts.status",json!({"host":host})).await,
    }
}
