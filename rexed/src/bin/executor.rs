use anyhow::Result;
use clap::{Parser, Subcommand};
use remote_executor::server::auth::{
    hash_password, Authentication, ConfigDirectory, Permissions, UserConfig,
};
use remote_executor::server::pty::CommandSpec;
use remote_executor::server::{
    start_executor, Executor, ExecutorInfo, SettingsStore, ShellManager,
};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "rexed", subcommand_negates_reqs = true)]
struct Args {
    #[arg(long, required = true)]
    id: Option<String>,
    /// Configuration and user policy root; defaults to executable-dir/rexed-config.
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,
    /// Explicit development-only plaintext/no-login listener.
    #[arg(long)]
    insecure_no_auth: bool,
    #[command(subcommand)]
    command: Option<Command>,

    /// WebSocket address, tcp://host:port, or unix:///path/to/socket.
    #[arg(long, default_value = "tls://127.0.0.1:0")]
    listen: String,

    /// Override server.json adminListen. Otherwise use the platform default; omitted network ports use the RE port + 1.
    #[arg(long)]
    admin_listen: Option<String>,

    #[arg(long)]
    system: Option<String>,

    #[arg(long)]
    device: Option<String>,

    #[arg(long, default_value = "main")]
    pty: String,
    #[arg(long)]
    pty_program: Option<String>,

    #[arg(long)]
    settings: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a TLS identity without replacing existing configuration.
    Init {
        #[arg(long)]
        server_name: Vec<String>,
    },
    /// Create or update one RE user. Relative certificate paths are user-directory relative.
    User {
        username: String,
        #[arg(long)]
        password_stdin: bool,
        #[arg(long)]
        certificate: Vec<PathBuf>,
        #[arg(long)]
        ca: Vec<PathBuf>,
        #[arg(long)]
        ca_identity: Option<String>,
        #[arg(long, value_delimiter = ',')]
        allow: Vec<String>,
        #[arg(long)]
        disabled: bool,
    },
}
fn configure(directory: &ConfigDirectory, command: Command) -> Result<()> {
    match command {
        Command::Init { server_name } => directory.initialize(server_name)?,
        Command::User {
            username,
            password_stdin,
            certificate,
            ca,
            ca_identity,
            allow,
            disabled,
        } => {
            directory.initialize(Vec::new())?;
            let mut user = match directory.load_user(&username) {
                Ok(user) => user,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    UserConfig::default()
                }
                Err(error) => return Err(error),
            };
            if password_stdin {
                let mut password = String::new();
                std::io::stdin().take(1027).read_to_string(&mut password)?;
                if password.ends_with('\n') {
                    password.pop();
                    if password.ends_with('\r') {
                        password.pop();
                    }
                }
                user.password_hash = Some(hash_password(&password)?);
            }
            if !certificate.is_empty() {
                user.certificates = certificate;
            }
            if !ca.is_empty() {
                user.certificate_authorities = ca;
            }
            if ca_identity.is_some() {
                user.ca_identity = ca_identity;
            }
            user.disabled = disabled;
            if !allow.is_empty() {
                let mut permissions = Permissions::default();
                for name in allow {
                    match name.as_str() {
                        "execute" => permissions.execute = true,
                        "files-read" => permissions.files_read = true,
                        "files-write" => permissions.files_write = true,
                        "pty-view" => permissions.pty_view = true,
                        "pty-control" => permissions.pty_control = true,
                        "forward" => permissions.forward = true,
                        "management" => permissions.management = true,
                        "none" => {}
                        _ => anyhow::bail!("unknown permission: {name}"),
                    }
                }
                user.permissions = permissions;
            }
            directory.save_user(&username, &user)?;
        }
    }
    eprintln!("configuration={}", directory.path().display());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let directory = ConfigDirectory::open(args.config_dir)?;
    if let Some(command) = args.command {
        return configure(&directory, command);
    }
    directory.initialize(Vec::new())?;
    let id = args.id.ok_or_else(|| anyhow::anyhow!("--id is required"))?;
    let server = directory.server()?;
    let admin_listen = args.admin_listen.or(server.admin_listen);
    let settings = SettingsStore::load(Some(
        args.settings
            .unwrap_or_else(|| directory.path().join("settings.json")),
    ))?;
    let executor_id = id.clone();
    let events =
        remote_executor::server::events::EventLog::standalone(executor_id.clone(), server.logging)?;
    if let Some(path) = events.directory() {
        eprintln!("logs={}", path.display());
    }
    events.emit("server.started", None, Default::default());
    let mut executor = Executor::new(ExecutorInfo {
        id,
        system: args
            .system
            .or_else(|| Some(std::env::consts::OS.to_string())),
        device: args.device.or_else(|| std::env::var("HOSTNAME").ok()),
        labels: BTreeMap::new(),
    })
    .with_settings_store(settings.clone())
    .with_event_log(events.clone());
    if !args.insecure_no_auth {
        executor = executor.with_authentication(Authentication::load(&directory)?);
    }
    let manager = ShellManager::default_shell(80, 24).with_event_log(events.clone());
    if let Some(program) = args.pty_program {
        manager.create_pty(args.pty.clone(), CommandSpec::new(program), None, None)?;
    }
    let actual = start_executor(args.listen, executor.clone(), manager.clone()).await?;
    let admin_target = remote_executor::server::management::resolve_management_target(
        admin_listen.as_deref(),
        &executor_id,
        &actual,
    )?;
    let admin = remote_executor::server::management::start_management_listener(
        &admin_target,
        executor,
        manager,
    )?;
    eprintln!("management={}", admin.address());
    eprintln!("{actual} pty={}", args.pty);

    tokio::signal::ctrl_c().await?;
    events.emit("server.stopped", None, Default::default());
    events.flush()?;
    Ok(())
}
