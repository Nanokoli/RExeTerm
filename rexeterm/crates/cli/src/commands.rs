//! Agent commands only build host requests; all RE state stays in the bound host.
use crate::{
    executors::{self, Inventory},
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
    "tui",
    "since",
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
    "credential",
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
/// Expand single-dash options and command shortcuts before parsing.
pub fn normalize_argv<I, T>(args: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString>,
{
    let mut args = args.into_iter().map(Into::into).peekable();
    let mut normalized: Vec<_> = args.next().into_iter().collect();
    while let Some(arg) = args.next() {
        if arg == "--" {
            normalized.push(arg);
            normalized.extend(args);
            break;
        }
        // Shell profile arguments may contain a literal `-ec`.
        if arg == "-ec"
            && !matches!(
                normalized.last().and_then(|previous| previous.to_str()),
                Some("--command-arg" | "--interactive-arg")
            )
        {
            normalized.extend(["executor", "connect"].map(std::ffi::OsString::from));
            continue;
        }
        let list = match (arg.to_str(), args.peek().and_then(|next| next.to_str())) {
            (Some("-e" | "--executor"), Some("-list_local")) => {
                Some(&["executor", "host", "list"][..])
            }
            (Some("-e" | "--executor"), Some("-list_online")) => {
                Some(&["executor", "list", "--online"][..])
            }
            (Some("-cr" | "--credential"), Some("-list_local")) => {
                Some(&["credential", "list"][..])
            }
            _ => None,
        };
        if let Some(command) = list {
            args.next();
            normalized.extend(command.iter().copied().map(std::ffi::OsString::from));
        } else {
            let text = arg.to_string_lossy();
            if let Some(name) = text
                .strip_prefix('-')
                .filter(|name| !name.starts_with('-') && name.len() >= 2)
            {
                let (head, tail) = match name.find('=') {
                    Some(index) => (&name[..index], &name[index..]),
                    None => (name, ""),
                };
                if head == "cr" || LONG_OPTIONS.contains(&head) {
                    let head = if head == "cr" { "credential" } else { head };
                    normalized.push(format!("--{head}{tail}").into());
                    continue;
                }
            }
            normalized.push(arg);
        }
    }
    normalized
}

#[derive(Parser)]
#[command(
    name = "rexeterm",
    version,
    about = "Agent CLI for local and remote executors; results are Markdown by default",
    after_help = "Start with `info` or `service list`. For a new project, use `service create` (add --local for the built-in executor).\nList saved executors with `-e -list_local`, connections with `-e -list_online`, and credential links with `-cr -list_local`.\nConnect with `-ec CREDENTIAL@HOSTNAME [as LIVE_ID]` (short for `executor connect`). Separate hops with `:`; the last hostname is the default live ID.\nForward with `-f PORT:TARGET_HOST@ONLINE_EXECUTOR:TARGET_PORT`; `-f` lists all forwards, `-f ID --stop` stops one. No -e needed.\nRun with `-e ID --cmd COMMAND --rcwd DIR -t 1000`. If still running, use `-e ID -A ASYNCID -t 1000`; check exitCode.\nFor a PTY, start with `--pty 80x24`. Use `-A ASYNCID --tui` or `--png FILE` for the current screen, `--text TEXT` to send keys, or `--read --raw FILE` to save retained bytes.\nSelect the local directory with cd or REXETERM_CWD; --rcwd is remote.\n.rexeterm.disable blocks calls in its directory, and blocks using its registry or starting a daemon with its state directory even from another cwd.",
    disable_help_subcommand = true
)]
pub struct Args {
    /// Executor ID, for example `-e local`; otherwise tasks use the connected default.
    #[arg(
        short = 'e',
        long = "executor",
        global = true,
        help_heading = "Global options",
        display_order = 898
    )]
    pub executor: Option<String>,
    /// Named profile for `executor connect` (also `-cr NAME`); see `credential list`.
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        help_heading = "Global options"
    )]
    credential: Option<String>,
    /// Service UUID64; otherwise read RexETermHostUUID.txt from the current directory.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        display_order = 900
    )]
    service_id: Option<String>,
    /// Service runtime registry; defaults to .rexeterm-socket beside this executable.
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
    /// Output sinks; repeat or comma-separate: text, file, raw, png, tui.
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
    /// Limit inline text in bytes; request --sink file to also save the payload.
    #[arg(
        long,
        global = true,
        default_value_t = 65536,
        help_heading = "Global options",
        display_order = 905
    )]
    pub text_limit: usize,
    /// Run a command (up to 4 KiB); upload longer scripts with `file upload`.
    #[arg(long = "cmd", value_name = "COMMAND")]
    pub cmd: Option<String>,
    /// Shell profile name; use `preferences shell list` to see available profiles.
    #[arg(short = 's', long = "shell")]
    pub shell: Option<String>,
    /// List forwards, start [LISTEN_HOST:]PORT:TARGET_HOST@ONLINE_EXECUTOR:TARGET_PORT, or stop ID with --stop.
    #[arg(
        short = 'F',
        visible_short_alias = 'f',
        long = "forward",
        value_name = "SPEC_OR_ID",
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
    /// Show the current PTY screen or new stdio output; omit the ID to list tasks.
    #[arg(
        short = 'A',
        long = "attach",
        value_name = "ASYNCID",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub attach: Option<String>,
    /// Stop the named -A task or -F forward, or cancel the named -T transfer.
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
    /// Replay retained task output from the beginning instead of advancing the cursor.
    #[arg(long = "read")]
    pub task_read: bool,
    /// Replay retained task bytes starting at an offset from a previous read.
    #[arg(long, requires = "attach", conflicts_with_all = ["task_read", "task_input", "task_input_file", "task_text", "task_file", "stop_task", "task_status", "task_remove"])]
    pub since: Option<u64>,
    /// Remove the task from the host and from its history.
    #[arg(long = "remove")]
    pub task_remove: bool,
    /// Send literal text to the task named by -A.
    #[arg(long = "input", value_name = "TEXT")]
    pub task_input: Option<String>,
    /// Send a remote file's bytes to the task named by -A.
    #[arg(long = "input-file", value_name = "PATH")]
    pub task_input_file: Option<String>,
    /// Initial stdin (stdio only, up to 4 KiB).
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
    /// Save retained PTY bytes, including terminal escape sequences, to FILE.
    #[arg(long = "raw", value_name = "FILE")]
    pub raw: Option<PathBuf>,
    /// Write a PNG of the daemon's current PTY screen to FILE.
    #[arg(long = "png", value_name = "FILE")]
    pub png: Option<PathBuf>,
    /// Print a color snapshot of the daemon's PTY screen; send keys with -A ID --text.
    #[arg(long, global = true, help_heading = "Global options")]
    pub tui: bool,
    /// Task or forward title (up to 40 Unicode characters).
    #[arg(long = "title", value_name = "TEXT")]
    pub title: Option<String>,
    /// Task or forward description (up to 200 Unicode characters).
    #[arg(short = 'd', long = "desc", alias = "description", value_name = "TEXT")]
    pub description: Option<String>,
    /// Wait for output for 0–10000 ms; 0 returns immediately. Use -A to continue.
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
    /// Allocate a terminal; default 80x24, maximum 300x120.
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
    /// Retained PTY bytes in a .bin file.
    Raw,
    /// Current PTY screen in a .png image.
    Png,
    /// Print the current PTY screen with color and no Markdown card.
    Tui,
}

impl Args {
    pub fn selected_sinks(&self) -> Vec<Sink> {
        let mut sinks = self.sink.clone();
        for (selected, sink) in [
            (self.raw.is_some(), Sink::Raw),
            (self.png.is_some(), Sink::Png),
            (self.tui, Sink::Tui),
        ] {
            if selected && !sinks.contains(&sink) {
                sinks.push(sink);
            }
        }
        sinks
    }
}
#[derive(Subcommand)]
enum Command {
    /// Run this binary as the persistent rexetermd host (native local IPC).
    #[command(long_flag = "daemon", visible_alias = "deamon")]
    Daemon(DaemonArgs),
    /// Disable calls here and use of this directory's daemon storage; no host is needed.
    Disable,
    /// Remove .rexeterm.disable from the current directory; no host is needed.
    Enable,
    /// Start, stop or inspect the local rexetermd service.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Show cwd, binding, selected service and live executor without starting a daemon.
    Info,
    /// List what the bound host supports.
    Capabilities,
    /// Manage executors: live connections and the saved entries they use.
    Executor {
        #[command(subcommand)]
        command: ExecutorCommand,
    },
    /// Manage named links to private authentication profiles; no daemon needed.
    Credential {
        #[command(subcommand)]
        command: CredentialCommand,
    },
    /// Remote file operations.
    File {
        #[command(subcommand)]
        command: FileCommand,
    },
    /// Read or write this directory's client preferences; no daemon needed.
    Preferences {
        #[command(subcommand)]
        command: PreferenceCommand,
    },
    /// Call native IPC, e.g. `api rpc --body '{"tool":"list_executor","params":{}}'`.
    Api {
        /// IPC operation name, for example `info` or `rpc`.
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
    Capabilities,
    Disconnect(String),
    Connect {
        params: Value,
        alias: bool,
    },
    ConnectChain {
        hops: Vec<Value>,
        id: String,
    },
    File(FileCommand),
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
            Action::Capabilities
            | Action::Disconnect(_)
            | Action::Connect { .. }
            | Action::ConnectChain { .. }
            | Action::Forward(_)
            | Action::Api { .. } => false,
            Action::Run(_) | Action::Task(_) | Action::File(_) => true,
            Action::Transfer(_) => true,
        }
    }

    /// One executor for the whole invocation: `rexeterm -e local <command>`.
    fn set_executor(&mut self, id: &str) {
        match self {
            Action::File(command) => command.set_executor(id),
            Action::Run(args) => args.host = id.to_owned(),
            Action::Task(command) => command.set_executor(id),
            Action::Transfer(command) => command.set_executor(id),
            _ => {}
        }
    }
}
impl Args {
    /// The host role: public foreground daemon or the internal detached child.
    pub fn daemon(&self) -> Option<(DaemonArgs, Option<String>, Option<PathBuf>)> {
        match self.command.as_ref() {
            Some(Command::Daemon(options))
            | Some(Command::Service {
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
enum ExecutorCommand {
    /// List active connections and saved entries without starting a daemon.
    List {
        /// Show live connections only (also `-e -list_online`).
        #[arg(long)]
        online: bool,
    },
    /// Show one executor's connection state.
    Status {
        #[arg(skip)]
        host: String,
    },
    /// Connect to one saved host or a colon-separated chain; `as LIVE_ID` names the final connection.
    #[command(
        after_help = "Examples:\n  rexeterm -ec rb5-codex@RB5\n  rexeterm executor connect rb5-codex@RB5 as rb5_codex_exe\n  rexeterm --timeout-ms 210000 -ec Sk-a@HostA:Sk-b@HostB:Sk-c@Host-C as Cfba\n  rexeterm -e RB5 -cr rb5-codex executor connect\nEach hop uses its own saved hostname and credential. Without `as`, the last hostname is the live ID.\nA jump chain has up to 60 seconds per hop. Set --timeout-ms to wait longer for the whole invocation.\nEach jump needs RE TCP forwarding. See connections with `-e -list_online` and their forwards with `-f`."
    )]
    Connect {
        /// Connect using one private SDK JSON file with id and url; -e overrides id.
        #[arg(long, conflicts_with = "selection")]
        options_file: Option<PathBuf>,
        /// Select CREDENTIAL@HOSTNAME, with optional :CREDENTIAL@HOSTNAME for each additional hop.
        #[arg(value_name = "SELECTION")]
        selection: Option<String>,
        #[arg(value_parser = ["as"], requires = "alias", value_name = "as")]
        as_keyword: Option<String>,
        /// Name to use in the live inventory and subsequent -e calls.
        #[arg(requires = "as_keyword", value_name = "LIVE_ID")]
        alias: Option<String>,
        /// Name to use in the live inventory (alternative to positional `as LIVE_ID`).
        #[arg(long = "as", conflicts_with = "alias", value_name = "LIVE_ID")]
        live_id: Option<String>,
    },
    /// Drop the connection named by `-e`, including its jump chain if it belongs to one.
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
        /// Optional private profile path; prefer -cr at connect time.
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
    fn set_executor(&mut self, id: &str) {
        match self {
            ExecutorCommand::Status { host } | ExecutorCommand::Disconnect { host } => {
                *host = id.to_owned()
            }
            ExecutorCommand::Host { command } => match command {
                HostEntryCommand::Add { host, .. }
                | HostEntryCommand::Edit { host, .. }
                | HostEntryCommand::Remove { host } => *host = id.to_owned(),
                HostEntryCommand::List => {}
            },
            ExecutorCommand::List { .. } | ExecutorCommand::Connect { .. } => {}
        }
    }
}

#[derive(Subcommand)]
enum CredentialCommand {
    /// List credential names and paths, without reading any secrets.
    List,
    /// Create a symbolic link to a private profile JSON file.
    Link { name: String, profile: PathBuf },
    /// Remove the link, leaving the source intact.
    Unlink { name: String },
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
    /// Transfer description (up to 200 Unicode characters).
    #[arg(long, default_value = "")]
    description: String,
    /// Expected SHA-256: 64 hex digits, with or without the sha256: prefix.
    #[arg(long, value_parser = rexeterm_appcore::protocol::file::normalize_sha256)]
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
enum ForwardCommand {
    List,
    Start {
        host: String,
        listen: String,
        target: String,
        title: String,
        description: String,
    },
    Stop {
        id: String,
    },
}
#[derive(Subcommand)]
enum PreferenceCommand {
    /// Show the stored client preferences.
    Show,
    /// Shell profiles and defaults used by `--cmd` and `--shell`.
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
        /// Profile name used by `--cmd` with `-s` or `--shell`.
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
        /// Remote working directory; use with `-e ID` to set its default shell.
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
    // Remote paths must never depend on the invoking machine's filesystem.
    client
        .call(
            "rpc",
            json!({"tool":tool,"executor":host,"params":params,"directory":cwd}),
        )
        .await
}
fn optional(params: &mut Value, key: &str, value: impl Into<Value>) {
    let value = value.into();
    if !value.is_null() {
        params[key] = value;
    }
}
async fn run(client: &IpcClient, args: RunArgs, terminal: &Value) -> Result<Value> {
    if matches!(args.io(), TaskIo::Stdio) {
        require_pty_sinks(terminal)?;
    }
    let command = args.command()?;
    ensure!(
        command.len() <= 4096,
        "command exceeds 4096 bytes; upload a script with file upload and run it by path"
    );
    ensure!(
        args.stdin.as_ref().is_none_or(|text| text.len() <= 4096),
        "initial stdin exceeds 4096 bytes; upload a file for larger input"
    );
    ensure!(
        args.title
            .as_ref()
            .is_none_or(|text| text.chars().count() <= 40),
        "title exceeds 40 Unicode characters"
    );
    ensure!(
        args.description
            .as_ref()
            .is_none_or(|text| text.chars().count() <= 200),
        "description exceeds 200 Unicode characters"
    );
    let pty_size = args.pty_size();
    ensure!(
        args.pty.is_none() || pty_size.is_some(),
        "PTY size must be COLSxROWS"
    );
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
    let mut result = task_rpc(client, &args.host, params, args.cwd.as_deref(), terminal).await?;
    output_observation(&mut result, OUTPUT_OBSERVATION_LIVE);
    Ok(result)
}
async fn task_command(client: &IpcClient, command: TaskCommand, terminal: &Value) -> Result<Value> {
    match command {
        TaskCommand::List { host } => client.call("tasks.list", json!({"host":host})).await,
        TaskCommand::Run(args) => run(client, args, terminal).await,
        TaskCommand::Status { host, task } => {
            client
                .call("tasks.status", json!({"host":host,"id":task}))
                .await
        }
        TaskCommand::Read(task) => read_task(client, task, terminal).await,
        TaskCommand::Input { task, text } => {
            attach(
                client,
                task,
                Some(text.replace('\\', "\\\\")),
                None,
                None,
                terminal,
            )
            .await
        }
        TaskCommand::InputFile { task, path, cwd } => {
            attach(client, task, None, Some(path), cwd.as_deref(), terminal).await
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
/// The daemon retains a bounded output buffer per executor and task.
const OUTPUT_OBSERVATION_LIVE: &str =
    "Output for this call; use -A ASYNCID to continue from the daemon's cursor.";
const OUTPUT_OBSERVATION_BUFFERED: &str =
    "Daemon buffer retains up to 256 KiB; -A ASYNCID --read replays retained output.";
fn output_observation(result: &mut Value, fallback: &str) {
    result["metadata"]["outputObservation"] = json!(match result["metadata"]["terminalOutput"]
        ["view"]
        .as_str()
    {
        Some("screen") => "Current PTY screen maintained by rexetermd.",
        Some("stream") => OUTPUT_OBSERVATION_BUFFERED,
        _ => fallback,
    });
}

fn require_pty_sinks(terminal: &Value) -> Result<()> {
    ensure!(
        ["raw", "png", "tui"]
            .iter()
            .all(|key| terminal[*key] != true),
        "raw, PNG and TUI output require a PTY; start the task with --pty"
    );
    Ok(())
}

async fn task_rpc(
    client: &IpcClient,
    host: &str,
    params: Value,
    cwd: Option<&str>,
    terminal: &Value,
) -> Result<Value> {
    client.call("rpc", json!({"tool":"exbash","executor":host,"params":params,"directory":cwd,"terminal":terminal})).await
}

async fn read_task(client: &IpcClient, task: ReadTaskArgs, terminal: &Value) -> Result<Value> {
    if task.task.starts_with("stdio-") {
        require_pty_sinks(terminal)?;
    }
    let mut params = json!({
        "host": task.host,
        "id": task.task,
        "wait_ms": task.wait_ms,
        "from_start": task.from_start,
        "terminal": terminal,
    });
    if let Some(since) = task.since {
        params["since"] = json!(since);
    }
    let mut result = client.call("tasks.read", params).await?;
    output_observation(&mut result, OUTPUT_OBSERVATION_BUFFERED);
    Ok(result)
}
async fn attach(
    client: &IpcClient,
    task: ReadTaskArgs,
    text: Option<String>,
    file: Option<String>,
    cwd: Option<&str>,
    terminal: &Value,
) -> Result<Value> {
    if task.task.starts_with("stdio-") {
        require_pty_sinks(terminal)?;
    }
    let mut params = json!({"mode":"attach","asyncID":task.task,"read_timeout":task.wait_ms});
    optional(&mut params, "text", text);
    optional(&mut params, "filePath", file);
    let mut result = task_rpc(client, &task.host, params, cwd, terminal).await?;
    output_observation(&mut result, OUTPUT_OBSERVATION_LIVE);
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
    params["localPath"] = json!(std::path::absolute(output)?);
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
    let sinks = args.selected_sinks();
    let terminal = json!({"raw":sinks.contains(&Sink::Raw),"png":sinks.contains(&Sink::Png),"tui":sinks.contains(&Sink::Tui)});
    // The opt-out always precedes registry, profile and network access.
    match &command {
        Some(Command::Disable) => return crate::directory::set(&cwd, true),
        Some(Command::Enable) => return crate::directory::set(&cwd, false),
        _ => crate::directory::check(&cwd)?,
    }
    ensure!(
        terminal["tui"] != true
            || (command.is_none()
                && (args.cmd.is_some()
                    || args.attach.as_ref().is_some_and(|id| !id.is_empty()
                        && !args.task_status
                        && !args.stop_task
                        && !args.task_remove))),
        "--tui requires a PTY command or -A ASYNCID"
    );
    ensure!(
        args.credential.is_none()
            || matches!(
                &command,
                Some(Command::Executor {
                    command: ExecutorCommand::Connect { .. }
                })
            ),
        "-cr/--credential applies to executor connect"
    );
    let explicit = args.service_id.as_deref();
    let action = match command {
        Some(
            Command::Info
            | Command::Service {
                command: ServiceCommand::Info,
            },
        ) => {
            return invocation_info(&args, &cwd).await;
        }
        Some(Command::Service { command }) => {
            let registry = if matches!(command, ServiceCommand::Create(_)) {
                Registry::open(args.registry_dir)?
            } else {
                Registry::inspect(args.registry_dir)?
            };
            return services::execute(&registry, command, explicit, &cwd).await;
        }
        Some(Command::Capabilities) => Action::Capabilities,
        Some(Command::Executor { mut command }) => {
            if let Some(id) = args.executor.as_deref() {
                command.set_executor(id);
            }
            match command {
                ExecutorCommand::Host { command } => return host_entry_command(command),
                ExecutorCommand::List { .. } | ExecutorCommand::Status { .. } => {
                    return executor_diagnostics(&args, &cwd, command).await;
                }
                ExecutorCommand::Connect {
                    options_file,
                    selection,
                    as_keyword,
                    alias,
                    live_id,
                } => {
                    debug_assert_eq!(as_keyword.is_some(), alias.is_some());
                    connect_action(
                        options_file,
                        selection.as_deref(),
                        args.executor.as_deref(),
                        args.credential.as_deref(),
                        alias.or(live_id),
                    )?
                }
                ExecutorCommand::Disconnect { host } => {
                    ensure!(
                        !host.is_empty(),
                        "choose the executor to disconnect: -e ID executor disconnect"
                    );
                    Action::Disconnect(host)
                }
            }
        }
        Some(Command::Credential { command }) => return credential_command(command),
        Some(Command::File { command }) => Action::File(command),
        Some(Command::Preferences { mut command }) => {
            if let Some(id) = args.executor.as_deref() {
                command.set_executor(id);
            }
            return preferences_command(command);
        }
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
        Some(_) => unreachable!("daemon and directory commands return before this"),
        None => {
            bare_action(&args)?.context("name a command, or pass --cmd '<command>'; see --help")?
        }
    };
    let registry = Registry::inspect(args.registry_dir)?;
    let id = services::required_id(&cwd, explicit)?;
    let client = if matches!(action, Action::Capabilities | Action::Api { .. }) {
        services::client(&registry.resolve(&id)?)?
    } else {
        services::connect(&registry, &id).await?
    };
    let mut action = action;
    // Forward actions select their own exit; tasks and files can use the daemon default.
    match args.executor {
        Some(executor) => action.set_executor(&executor),
        None if action.needs_executor() => {
            let executor = Inventory::live(&client).await?.default
                .context("no executor is connected; use -e ID executor connect -cr NAME or start a service with --local")?;
            action.set_executor(&executor);
        }
        None => {}
    }
    execute_remote(&client, action, &terminal).await
}

fn connect_action(
    options_file: Option<PathBuf>,
    selection: Option<&str>,
    host: Option<&str>,
    credential: Option<&str>,
    alias: Option<String>,
) -> Result<Action> {
    if let Some(id) = &alias {
        executors::validate_alias(id)?;
    }
    let mut params = if let Some(path) = options_file {
        ensure!(credential.is_none(), "choose --options-file or -cr NAME");
        executors::options_file(&path, host)?
    } else if let Some(selection) = selection {
        ensure!(
            host.is_none() && credential.is_none(),
            "choose CREDENTIAL@HOSTNAME or -e HOSTNAME -cr NAME"
        );
        let mut hops = Vec::new();
        for (index, hop) in selection.split(':').enumerate() {
            let (credential, host) = hop
                .split_once('@')
                .filter(|(credential, host)| {
                    !credential.is_empty() && !host.is_empty() && !host.contains('@')
                })
                .with_context(|| {
                    format!(
                        "invalid hop {}; use CREDENTIAL@HOSTNAME for each hop",
                        index + 1
                    )
                })?;
            hops.push(
                executors::connection(host, Some(credential))
                    .with_context(|| format!("cannot prepare hop {} ({host})", index + 1))?,
            );
        }
        if hops.len() > 1 {
            let id =
                alias.unwrap_or_else(|| hops.last().unwrap()["id"].as_str().unwrap().to_owned());
            executors::validate_alias(&id)?;
            return Ok(Action::ConnectChain { hops, id });
        }
        hops.pop().unwrap()
    } else {
        let host = host.filter(|host| !host.is_empty()).context("choose a saved executor: executor connect CREDENTIAL@HOSTNAME [as LIVE_ID], or use -e HOSTNAME -cr NAME")?;
        executors::connection(host, credential)?
    };
    if let Some(id) = &alias {
        params["id"] = json!(id);
    }
    Ok(Action::Connect {
        params,
        alias: alias.is_some(),
    })
}

async fn invocation_info(args: &Args, cwd: &std::path::Path) -> Result<Value> {
    let registry = Registry::inspect(args.registry_dir.clone())?;
    let (mut info, client) = services::inspect(&registry, cwd, args.service_id.as_deref()).await?;
    let inventory = match client.as_ref() {
        Some(client) => Inventory::live(client).await?,
        None => Inventory::default(),
    };
    let selected = args.executor.as_deref().or(inventory.default.as_deref());
    let actual = selected.and_then(|id| inventory.find(id));
    info["executorRequested"] = json!(args.executor);
    info["defaultExecutor"] = json!(inventory.default);
    info["executor"] = json!(actual.and_then(|entry| entry["id"].as_str()));
    info["executorConnected"] = json!(actual.is_some());
    info["executable"] = json!(std::env::current_exe()?);
    info["version"] = json!(env!("CARGO_PKG_VERSION"));
    info["buildId"] = json!(option_env!("REXETERM_BUILD_ID"));
    info["clientConfigDir"] = json!(preferences::directory()?);
    let sinks = args.selected_sinks();
    info["sinks"] = json!(sinks
        .iter()
        .filter_map(|sink| sink.to_possible_value())
        .map(|sink| sink.get_name().to_owned())
        .collect::<Vec<_>>());
    info["outDir"] = json!(cwd.join(
        args.out_dir
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new(".rexeterm-out"))
    ));
    info["textLimit"] = json!(args.text_limit);
    info["rawPath"] = json!(args.raw.as_ref().map(|path| cwd.join(path)));
    info["pngPath"] = json!(args.png.as_ref().map(|path| cwd.join(path)));
    Ok(info)
}

async fn executor_diagnostics(
    args: &Args,
    cwd: &std::path::Path,
    command: ExecutorCommand,
) -> Result<Value> {
    let registry = Registry::inspect(args.registry_dir.clone())?;
    let (mut result, client) =
        services::inspect(&registry, cwd, args.service_id.as_deref()).await?;
    let inventory = match client.as_ref() {
        Some(client) => Inventory::live(client).await?,
        None => Inventory::default(),
    };
    match command {
        ExecutorCommand::List { online } => {
            result["default"] = json!(inventory.default);
            result["executors"] = json!(if online {
                inventory.executors
            } else {
                inventory.with_saved()?
            });
        }
        ExecutorCommand::Status { host } => {
            let id = if host.is_empty() {
                inventory
                    .default
                    .as_deref()
                    .context("no default executor; specify -e ID executor status")?
            } else {
                &host
            };
            result["id"] = json!(id);
            result["connected"] = json!(inventory.find(id).is_some());
            result["executor"] = json!(inventory.find(id));
        }
        _ => unreachable!("expected a read-only executor diagnostic"),
    }
    Ok(result)
}

fn bare_action(args: &Args) -> Result<Option<Action>> {
    Ok(bare_forward(args)?
        .or_else(|| bare_transfer(args))
        .or_else(|| bare_task(args))
        .or_else(|| bare_run(args)))
}

/// Omit the forward operand to list, or pass a listener and target to start.
fn bare_forward(args: &Args) -> Result<Option<Action>> {
    let Some(value) = args.forward.as_deref() else {
        return Ok(None);
    };
    if args.stop_task {
        ensure!(!value.is_empty(), "name the forward ID: -f ID --stop");
        return Ok(Some(Action::Forward(ForwardCommand::Stop {
            id: value.to_owned(),
        })));
    }
    if value.is_empty() {
        return Ok(Some(Action::Forward(ForwardCommand::List)));
    }
    const USAGE: &str =
        "invalid forward; use -f [LISTEN_HOST:]PORT:TARGET_HOST@ONLINE_EXECUTOR:TARGET_PORT";
    let mut in_brackets = false;
    let parts: Vec<_> = value
        .split(|c| match c {
            '[' | ']' => {
                in_brackets = c == '[';
                false
            }
            ':' => !in_brackets,
            _ => false,
        })
        .collect();
    let (listen_host, listen_port, target, target_port) = match parts.as_slice() {
        [port, target, target_port] => ("127.0.0.1", *port, *target, *target_port),
        [host, port, target, target_port] => (*host, *port, *target, *target_port),
        _ => anyhow::bail!(USAGE),
    };
    let (target_host, host) = target.split_once('@').context(USAGE)?;
    let listen_port = listen_port.parse::<u16>().context(USAGE)?;
    let target_port = target_port.parse::<u16>().context(USAGE)?;
    ensure!(
        valid_forward_host(listen_host)
            && valid_forward_host(target_host)
            && !host.trim().is_empty()
            && !host.contains(['@', '[', ']'])
            && target_port != 0,
        USAGE
    );
    Ok(Some(Action::Forward(ForwardCommand::Start {
        host: host.to_owned(),
        listen: format!("{listen_host}:{listen_port}"),
        target: format!("{target_host}:{target_port}"),
        title: args.title.clone().unwrap_or_else(|| "未命名".to_owned()),
        description: args.description.clone().unwrap_or_default(),
    })))
}

fn valid_forward_host(host: &str) -> bool {
    if let Some(addr) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return addr.parse::<std::net::Ipv6Addr>().is_ok();
    }
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
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
    let read = || ReadTaskArgs {
        host: host.clone(),
        task: id.clone(),
        wait_ms: args.wait_ms,
        from_start: args.task_read,
        since: args.since,
    };
    if args.task_read || args.since.is_some() {
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
async fn execute_remote(client: &IpcClient, action: Action, terminal: &Value) -> Result<Value> {
    match action {
        Action::Capabilities => client.call("capabilities", json!({})).await,
        Action::Connect { params, alias } => {
            let id = params["id"]
                .as_str()
                .context("expected executor id")?
                .to_owned();
            if alias {
                ensure!(Inventory::live(client).await?.find(&id).is_none(), "live executor ID {id} is already connected; choose a different alias or disconnect it first");
            }
            rpc(client, Some(&id), "connect_to_executor", params, None).await
        }
        Action::ConnectChain { hops, id } => {
            client
                .call("hosts.connect_chain", json!({"id":id,"hops":hops}))
                .await
        }
        Action::Disconnect(host) => client.call("hosts.disconnect", json!({"host":host})).await,
        Action::Task(TaskCommand::List { host }) => {
            let value = task_command(client, TaskCommand::List { host }, terminal).await?;
            let mut tasks: Vec<Value> = ["active", "completed"]
                .iter()
                .filter_map(|key| value[key].as_array().cloned())
                .flatten()
                .collect();
            tasks.sort_by_key(task_rank);
            Ok(json!({"tasks": tasks}))
        }
        Action::Run(args) => run(client, args, terminal).await,
        Action::Task(command) => task_command(client, command, terminal).await,
        Action::File(command) => file_command(client, command).await,
        Action::Transfer(command) => transfer_command(client, command).await,
        Action::Forward(command) => match command {
            ForwardCommand::List => client.call("forwards.list", json!({})).await,
            ForwardCommand::Start {
                host,
                listen,
                target,
                title,
                description,
            } => {
                let body = json!({"listen":listen,"target":{"kind":"tcp","address":target},"title":title,"description":description});
                client
                    .call("forwards.start", json!({"host":host,"options":body}))
                    .await
            }
            ForwardCommand::Stop { id } => client.call("forwards.stop", json!({"id":id})).await,
        },
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
            let value = if operation == "list_executor" {
                client
                    .call_with_key(
                        "rpc",
                        json!({"tool":"list_executor","params":args}),
                        idempotency_key,
                    )
                    .await?
            } else {
                client
                    .call_with_key(&operation, args, idempotency_key)
                    .await?
            };
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
fn preferences_command(command: PreferenceCommand) -> Result<Value> {
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
        PreferenceCommand::Reload => {
            Ok(json!({"reloaded":true,"preferences":preferences::store()?.settings()}))
        }
    }
}

fn credential_command(command: CredentialCommand) -> Result<Value> {
    match command {
        CredentialCommand::List => executors::list_credentials(),
        CredentialCommand::Link { name, profile } => executors::link_credential(&name, &profile),
        CredentialCommand::Unlink { name } => executors::remove_credential(&name),
    }
}

fn host_entry_command(command: HostEntryCommand) -> Result<Value> {
    match command {
        HostEntryCommand::List => executors::list_saved(),
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
    }
}
