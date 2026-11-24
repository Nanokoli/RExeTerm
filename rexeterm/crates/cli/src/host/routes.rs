//! A jump chain owns ordinary online executors and local TCP forwards.
use crate::executors::validate_alias;
use anyhow::{anyhow, ensure, Context, Result};
use rexeterm_appcore::{client::ConnectExecutorOptions, config};
use rexeterm_application::Application;
use serde_json::{json, Value};
use std::net::SocketAddr;
use tokio::time::{timeout_at, Duration, Instant};
use url::Url;

const HOP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct Routes {
    active: Vec<Route>,
}

struct Route {
    executors: Vec<String>,
    forwards: Vec<String>,
    connected: usize,
}

impl Routes {
    pub(super) async fn connect(
        &mut self,
        app: &Application,
        id: &str,
        hops: Value,
    ) -> Result<Value> {
        validate_alias(id)?;
        let hops: Vec<ConnectExecutorOptions> = serde_json::from_value(hops)
            .map_err(|_| anyhow!("invalid SDK connection options in jump chain"))?;
        ensure!(hops.len() >= 2, "a jump chain requires at least two hops");

        let last = hops.len() - 1;
        let mut executors = Vec::with_capacity(hops.len());
        let mut targets = Vec::with_capacity(last);
        for (index, hop) in hops.iter().enumerate() {
            ensure!(
                config::portable_name(&hop.id),
                "invalid saved hostname at hop {}",
                index + 1
            );
            executors.push(if index == last {
                id.to_owned()
            } else {
                format!("{id}_{}_node{}", hop.id, index + 1)
            });
            if index != 0 {
                ensure!(
                    hop.links.is_empty(),
                    "hop {} ({}) must have a single URL; extra links cannot use its TCP forward",
                    index + 1,
                    hop.id
                );
                targets.push(
                    TcpTarget::parse(&hop.url).with_context(|| {
                        format!("invalid URL for hop {} ({})", index + 1, hop.id)
                    })?,
                );
            }
        }
        let online = app.rpc(None, "list_executor", json!({}), None).await?;
        let inventory = online["metadata"]["executors"]
            .as_array()
            .context("missing online executor inventory")?;
        for executor in &executors {
            ensure!(
                !inventory.iter().any(|entry| entry["id"] == *executor),
                "live executor ID {executor} is already connected; choose a different alias or disconnect it first"
            );
        }
        let forwards = executors[..last]
            .iter()
            .enumerate()
            .map(|(index, executor)| format!("{executor}_Forward_node{}", index + 2))
            .collect();
        self.active.push(Route {
            executors,
            forwards,
            connected: 0,
        });
        let route = self.active.last_mut().unwrap();
        // Track the in-progress route as well, so shutdown can release a partially built chain.
        if let Err(error) = route
            .establish(app, hops, targets, online["metadata"]["default"].is_null())
            .await
        {
            let cleanup = self.close(app, self.active.len() - 1).await;
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(anyhow!("{error:#}; {cleanup:#}")),
            };
        }
        Ok(json!({"id":id,"connected":true,"executors":route.executors,"forwards":route.forwards}))
    }

    pub(super) fn ensure_unmanaged(&self, id: &str) -> Result<()> {
        if let Some(route) = self.active.iter().find(|route| route.contains(id)) {
            anyhow::bail!(
                "executor {id} belongs to jump chain {}; disconnect the chain first",
                route.id()
            );
        }
        Ok(())
    }

    pub(super) async fn disconnect(&mut self, app: &Application, id: &str) -> Result<()> {
        if let Some(index) = self.active.iter().position(|route| route.contains(id)) {
            self.close(app, index).await
        } else {
            app.core().disconnect_executor(id).await
        }
    }

    pub(super) async fn stop_forward(
        &mut self,
        app: &Application,
        id: &str,
    ) -> Result<Option<Value>> {
        let Some(index) = self
            .active
            .iter()
            .position(|route| route.forwards.iter().any(|forward| forward == id))
        else {
            return Ok(None);
        };
        let chain = self.active[index].id().to_owned();
        self.close(app, index).await?;
        Ok(Some(json!({"forwardID":id,"stopped":true,"chain":chain})))
    }

    async fn close(&mut self, app: &Application, index: usize) -> Result<()> {
        let result = self.active[index].close(app).await;
        self.active.remove(index);
        result
    }

    pub(super) async fn shutdown(&mut self, app: &Application) {
        while !self.active.is_empty() {
            let _ = self.close(app, self.active.len() - 1).await;
        }
    }
}

impl Route {
    fn id(&self) -> &str {
        self.executors.last().unwrap()
    }

    fn contains(&self, id: &str) -> bool {
        self.executors.iter().any(|executor| executor == id)
    }

    async fn establish(
        &mut self,
        app: &Application,
        hops: Vec<ConnectExecutorOptions>,
        targets: Vec<TcpTarget>,
        select_final: bool,
    ) -> Result<()> {
        let mut targets = targets.into_iter();
        for (index, mut hop) in hops.into_iter().enumerate() {
            let hostname = std::mem::replace(&mut hop.id, self.executors[index].clone());
            let context = || format!("cannot connect hop {} ({hostname})", index + 1);
            let deadline = Instant::now() + HOP_TIMEOUT;
            let result = timeout_at(deadline, async {
                if index != 0 {
                    let target = targets.next().unwrap();
                    let forward = &self.forwards[index - 1];
                    let reply = app.rpc(
                        Some(&self.executors[index - 1]),
                        "forward",
                        json!({"mode":"start","forwardID":forward,"listen":"127.0.0.1:0","target":{"kind":"tcp","address":target.address}}),
                        None,
                    ).await.with_context(|| format!("cannot start forward {forward} to hop {} ({hostname})", index + 1))?;
                    let listen = reply["metadata"]["listen"]
                        .as_str()
                        .context("forward did not return a listen address")?;
                    target.apply(&mut hop, listen).with_context(context)?;
                }
                app.core().connect_to_executor(hop).await.with_context(context)?;
                self.connected += 1;
                Ok::<_, anyhow::Error>(())
            }).await;
            match result {
                Ok(result) if Instant::now() <= deadline => result?,
                _ => anyhow::bail!(
                    "hop {} ({hostname}) timed out after {} seconds",
                    index + 1,
                    HOP_TIMEOUT.as_secs()
                ),
            }
        }
        if select_final {
            app.core().set_default_executor(self.id()).await?;
        }
        Ok(())
    }

    async fn close(&mut self, app: &Application) -> Result<()> {
        let mut errors = Vec::new();
        while self.connected != 0 {
            let id = &self.executors[self.connected - 1];
            // The SDK stops an executor's listeners on disconnect. Keep upstream
            // listeners alive until the downstream sessions have been closed.
            if let Err(error) = app.core().disconnect_executor(id).await {
                errors.push(format!("{id}: {error:#}"));
            }
            self.connected -= 1;
        }
        ensure!(
            errors.is_empty(),
            "removed jump chain {}; could not close RE sessions: {}",
            self.id(),
            errors.join("; ")
        );
        Ok(())
    }
}

struct TcpTarget {
    url: Url,
    address: String,
}

impl TcpTarget {
    fn parse(raw: &str) -> Result<Self> {
        let url = if raw.contains("://") {
            Url::parse(raw)
        } else {
            // Same default as a direct SDK connection.
            Url::parse(&format!("ws://{raw}"))
        }?;
        ensure!(
            matches!(url.scheme(), "tcp" | "tls" | "ws" | "wss"),
            "jump targets require a tcp://, tls://, ws:// or wss:// URL"
        );
        ensure!(
            !raw.contains(['@', '?', '#', '\r', '\n']),
            "connection URLs cannot contain credentials, queries or fragments"
        );
        ensure!(
            matches!(url.scheme(), "ws" | "wss") || matches!(url.path(), "" | "/"),
            "TCP and TLS stream URLs cannot contain a path"
        );
        let host = url
            .host_str()
            .filter(|host| !host.is_empty())
            .context("missing URL host")?;
        let port = url
            .port_or_known_default()
            .filter(|&port| port != 0)
            .context("missing or invalid URL port")?;
        Ok(Self {
            address: format!("{host}:{port}"),
            url,
        })
    }

    fn apply(mut self, hop: &mut ConnectExecutorOptions, listen: &str) -> Result<()> {
        let addr: SocketAddr = listen
            .strip_prefix("tcp://")
            .context("expected a TCP forward listen address")?
            .parse()?;
        if matches!(self.url.scheme(), "tls" | "wss") {
            let original_host = self
                .url
                .host_str()
                .context("missing TLS host")?
                .trim_matches(['[', ']']);
            // Retain verification of the remote endpoint while dialing the local listener.
            hop.tls
                .get_or_insert_with(Default::default)
                .server_name
                .get_or_insert_with(|| original_host.to_owned());
        }
        hop.url = if matches!(self.url.scheme(), "ws" | "wss") {
            self.url
                .set_ip_host(addr.ip())
                .map_err(|_| anyhow!("cannot set forward host"))?;
            self.url
                .set_port(Some(addr.port()))
                .map_err(|_| anyhow!("cannot set forward port"))?;
            self.url.into()
        } else {
            format!("{}://{addr}", self.url.scheme())
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rexeterm_appcore::{
        protocol::ExecutorResponse,
        transport::{from_stream, Connector, Frame, Link, LinkFuture},
        AppCore,
    };
    use rexeterm_application::Workspace;
    use std::sync::Arc;

    struct SlowDiscovery;

    impl Connector for SlowDiscovery {
        fn connect<'a>(&'a self, _: &'a str) -> LinkFuture<'a, Link> {
            Box::pin(async move {
                // Each discovery satisfies the SDK's 30s request deadline.
                tokio::time::sleep(Duration::from_secs(25)).await;
                let (client, server) = tokio::io::duplex(4096);
                tokio::spawn(async move {
                    let mut link = from_stream(server);
                    link.recv().await.unwrap().unwrap();
                    let reply = ExecutorResponse::ok(
                        json!("_executor_info"),
                        None,
                        json!({
                            "id":"test-rexed","version":"test","capabilities":{"streamForward":true}
                        }),
                    );
                    link.send(Frame::Text(serde_json::to_string(&reply).unwrap()))
                        .await
                        .unwrap();
                });
                Ok(from_stream(client))
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_jump_chain_has_a_separate_sixty_second_budget_for_each_hop() {
        let dir = tempfile::tempdir().unwrap();
        let app = Application::new(
            AppCore::default().with_connector(Arc::new(SlowDiscovery)),
            dir.path().join("workspace.json"),
            Workspace {
                version: 1,
                hosts: vec![],
                last_host: None,
            },
        )
        .await
        .unwrap();
        let mut routes = Routes::default();
        let mut hops = json!([
            {"id":"Entry","url":"tcp://primary:45957","links":["tcp://secondary:45957"]},
            {"id":"Target","url":"tcp://target:45957"},
        ]);

        // 50s for Entry + 25s for Target succeeds despite taking more than 60s overall.
        let start = Instant::now();
        routes.connect(&app, "working", hops.clone()).await.unwrap();
        assert!(start.elapsed() >= Duration::from_secs(75));

        // Three discoveries on the same hop would take 75s; abort that chain at 60s.
        hops[0]["links"] = json!(["tcp://secondary:45957", "tcp://third:45957"]);
        let start = Instant::now();
        let err = routes.connect(&app, "slow", hops).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("hop 1 (Entry) timed out after 60 seconds"),
            "{err:#}"
        );
        assert!((Duration::from_secs(60)..Duration::from_secs(61)).contains(&start.elapsed()));

        let online = app
            .rpc(None, "list_executor", json!({}), None)
            .await
            .unwrap();
        let ids: Vec<_> = online["metadata"]["executors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["working", "working_Entry_node1"]);
        let forwards = app
            .rpc(None, "forward", json!({"mode":"list"}), None)
            .await
            .unwrap();
        let forwards = forwards["metadata"]["forwards"].as_array().unwrap();
        assert_eq!(forwards.len(), 1);
        assert_eq!(
            forwards[0]["forwardID"],
            "working_Entry_node1_Forward_node2"
        );
        routes.disconnect(&app, "working").await.unwrap();
    }
}
