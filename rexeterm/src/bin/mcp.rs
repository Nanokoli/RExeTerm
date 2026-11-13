use anyhow::Result;
use clap::Parser;
use rexeterm::daemon::{Daemon, DaemonOptions};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(about = "RexETerm ordinary MCP stdio, with an embedded daemon")]
struct Args {
    #[arg(long)]
    settings: Option<PathBuf>,
    #[arg(long)]
    client_settings: Option<PathBuf>,
    #[arg(long)]
    no_local: bool,
    #[arg(long, default_value = "default")]
    re_user: String,
    #[arg(long, default_value = "rexeterm-mcp")]
    client_name: String,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let daemon = Daemon::new_with_options(DaemonOptions {
        settings_path: args.settings,
        client_settings_path: Some(
            args.client_settings
                .unwrap_or(rexeterm::appcore::preferences::ClientSettingsStore::default_path()?),
        ),
        local_executor: cfg!(feature = "local") && !args.no_local,
        identity: rexeterm::protocol::management::ClientHello {
            username: args.re_user,
            client_name: args.client_name,
            ..Default::default()
        },
    })
    .await?;
    let result = rexeterm::mcp::run_mcp_stdio_with_core(daemon.core().clone()).await;
    daemon.shutdown().await;
    result
}
