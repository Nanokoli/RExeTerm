use anyhow::Result;
use clap::Parser;
use rexeterm::{
    admin_view::{self, Page},
    client::management::{default_management_target, ManagementClient},
    protocol::management::{ChannelKind, ManagementSnapshot},
};
use std::io::IsTerminal;
#[path = "rexctl/ui.rs"]
mod ui;
#[derive(Parser)]
#[command(
    name = "rexctl",
    about = "Five operations pages over the independent rexed management endpoint"
)]
struct Args {
    #[arg(long)]
    target: Option<String>,
    /// Private JSON with username, authentication and tls for network operations.
    #[arg(long)]
    connection_profile: Option<std::path::PathBuf>,
    #[arg(long, default_value = "local")]
    id: String,
    #[arg(value_enum)]
    page: Option<Page>,
    #[arg(long)]
    username: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long, conflicts_with = "tui")]
    json: bool,
    #[arg(long, conflicts_with = "tui")]
    watch: bool,
    #[arg(long, conflicts_with = "no_tui")]
    tui: bool,
    #[arg(long)]
    no_tui: bool,
    #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u64).range(1..=3600))]
    interval: u64,
}
fn select(s: &mut ManagementSnapshot, args: &Args) {
    if args.username.is_none() && args.session.is_none() {
        return;
    }
    let matches = |o: Option<&rexeterm::protocol::management::SessionOwner>| {
        o.is_some_and(|o| {
            args.username.as_deref().is_none_or(|u| u == o.username)
                && args.session.as_deref().is_none_or(|id| id == o.session_id)
        })
    };
    s.clients.retain(|c| matches(Some(&c.owner)));
    s.channels.retain(|c| matches(c.owner.as_ref()));
    s.programs.retain(|p| matches(p.created_by.as_ref()));
}
fn print(s: &ManagementSnapshot, args: &Args) -> Result<()> {
    if args.json {
        let value = match args.page {
            None => serde_json::to_value(s)?,
            Some(Page::Clients) => serde_json::to_value(&s.clients)?,
            Some(Page::Programs) => serde_json::to_value(&s.programs)?,
            Some(page) => {
                let kind = match page {
                    Page::Pty => ChannelKind::Pty,
                    Page::Files => ChannelKind::File,
                    _ => ChannelKind::Forward,
                };
                serde_json::to_value(
                    s.channels
                        .iter()
                        .filter(|c| c.kind == kind)
                        .collect::<Vec<_>>(),
                )?
            }
        };
        println!("{}", serde_json::to_string(&value)?);
    } else {
        print!(
            "{}",
            admin_view::render(s, args.page.unwrap_or(Page::Clients))
        );
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let target = args
        .target
        .clone()
        .map(Ok)
        .unwrap_or_else(|| default_management_target(&args.id))?;
    let client = if let Some(path) = &args.connection_profile {
        use rexeterm::client::{
            auth::{AuthenticatedConnector, AuthenticationOptions},
            transport::{Connector, DefaultConnector, TlsClientOptions, TlsConnector},
        };
        use std::sync::Arc;
        let profile: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let tls = profile
            .get("tls")
            .cloned()
            .map(serde_json::from_value::<TlsClientOptions>)
            .transpose()?;
        let mut connector: Arc<dyn Connector> = match tls {
            Some(tls) => Arc::new(TlsConnector::new(tls)?),
            None => Arc::new(DefaultConnector),
        };
        if let Some(auth) = profile.get("authentication") {
            let username = profile["username"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("connection profile requires username"))?;
            connector = Arc::new(AuthenticatedConnector::new(
                connector,
                username.into(),
                serde_json::from_value::<AuthenticationOptions>(auth.clone())?,
            ));
        }
        ManagementClient::with_connector(connector, &target).await?
    } else {
        ManagementClient::connect(&target).await?
    };
    if args.tui
        || (!args.no_tui
            && !args.json
            && !args.watch
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal())
    {
        return ui::run(client, args).await;
    }
    let mut client = client;
    loop {
        let mut snapshot = client.snapshot().await?;
        select(&mut snapshot, &args);
        print(&snapshot, &args)?;
        if !args.watch {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(args.interval)).await;
    }
    Ok(())
}
