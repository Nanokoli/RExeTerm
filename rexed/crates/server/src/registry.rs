//! Client identities are primary keys; session credentials only bind physical links.
use crate::protocol::management::*;
use anyhow::{anyhow, bail, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
struct Session {
    summary: ClientSessionSummary,
    secret: String,
    expires: Instant,
    cancelled: watch::Sender<bool>,
    traffic: Arc<crate::transport::TrafficAggregate>,
    active_links: usize,
    traffic_complete: bool,
}
struct State {
    clients: BTreeMap<String, Session>,
    sessions: BTreeMap<String, String>,
    channels: BTreeMap<String, ChannelSummary>,
    disconnect_timeout: Duration,
    manager: Option<crate::shell_manager::ShellManager>,
    reaper_started: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            clients: Default::default(),
            sessions: Default::default(),
            channels: Default::default(),
            disconnect_timeout: Duration::from_secs(60),
            manager: None,
            reaper_started: false,
        }
    }
}
#[derive(Clone, Default)]
pub(crate) struct Registry {
    state: Arc<Mutex<State>>,
    pub(crate) events: crate::events::EventLog,
}
impl State {
    fn prune(&mut self, events: &crate::events::EventLog) {
        if let Some(manager) = &self.manager {
            manager.reap_completed();
        }
        let expired: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, s)| s.expires <= Instant::now())
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            let session = self.clients.remove(&id).unwrap();
            self.sessions.remove(&session.summary.owner.session_id);
            session.cancelled.send_replace(true);
            // Finish cleanup before this identity can resume with a fresh lease.
            if let Some(manager) = &self.manager {
                manager.expire_client(&id);
            }
            events.emit(
                "client.disconnected",
                Some(&session.summary.owner),
                serde_json::json!({"reason":"lease_ended","peer":session.summary.peer,"transport":session.summary.transport}),
            );
        }
    }
}
impl Registry {
    pub(crate) fn set_disconnect_timeout(&self, timeout: Duration) {
        self.state.lock().unwrap().disconnect_timeout = timeout;
    }
    pub(crate) fn maintain(&self, manager: crate::shell_manager::ShellManager) {
        let mut state = self.state.lock().unwrap();
        state.manager = Some(manager);
        if state.reaper_started {
            return;
        }
        state.reaper_started = true;
        let weak = Arc::downgrade(&self.state);
        let events = self.events.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let Some(state) = weak.upgrade() else {
                    break;
                };
                state.lock().unwrap().prune(&events);
            }
        });
    }
    pub(crate) fn open(
        &self,
        hello: ClientHello,
        peer: String,
        transport: String,
    ) -> Result<(SessionOwner, SessionCredential)> {
        let secret = format!(
            "{:032x}{:032x}",
            rand::random::<u128>(),
            rand::random::<u128>()
        );
        let client_id = format!("client-{:x}", Sha256::digest(secret.as_bytes()));
        self.open_client(
            hello,
            ClientCredential { client_id, secret },
            peer,
            transport,
        )
    }
    pub(crate) fn open_client(
        &self,
        hello: ClientHello,
        credential: ClientCredential,
        peer: String,
        transport: String,
    ) -> Result<(SessionOwner, SessionCredential)> {
        hello.validate().map_err(|e| anyhow!(e))?;
        if credential.secret.len() != 64
            || !credential.secret.bytes().all(|b| b.is_ascii_hexdigit())
            || credential.client_id
                != format!("client-{:x}", Sha256::digest(credential.secret.as_bytes()))
        {
            bail!("invalid client identity credential");
        }
        let mut state = self.state.lock().unwrap();
        state.prune(&self.events);
        let client_id = credential.client_id;
        if let Some(existing) = state.clients.get(&client_id) {
            if existing.summary.owner.username != hello.username {
                bail!("cannot change username while resuming a client");
            }
        } else if state.clients.len() >= 1024 {
            bail!("client session capacity reached");
        }
        let (id, secret) = state
            .clients
            .get(&client_id)
            .map(|s| (s.summary.owner.session_id.clone(), s.secret.clone()))
            .unwrap_or_else(|| {
                (
                    format!("session-{:032x}", rand::random::<u128>()),
                    format!(
                        "{:032x}{:032x}",
                        rand::random::<u128>(),
                        rand::random::<u128>()
                    ),
                )
            });
        let owner = SessionOwner {
            client_id: client_id.clone(),
            session_id: id.clone(),
            username: hello.username,
            client_name: hello.client_name,
            client_version: hello.client_version,
        };
        let stamp = now_ms();
        let expires = Instant::now() + state.disconnect_timeout;
        let old = state.clients.remove(&client_id);
        let resumed = old.is_some();
        let mut session = match old {
            Some(mut session) => {
                state.sessions.remove(&session.summary.owner.session_id);
                session.summary.owner = owner.clone();
                session.summary.peer = peer;
                session.summary.transport = transport;
                session.summary.last_seen_at_ms = stamp;
                session.secret = secret.clone();
                session.expires = expires;
                session
            }
            None => Session {
                summary: ClientSessionSummary {
                    owner: owner.clone(),
                    active_links: 0,
                    traffic: Default::default(),
                    traffic_complete: true,
                    peer,
                    transport,
                    created_at_ms: stamp,
                    last_seen_at_ms: stamp,
                },
                secret: secret.clone(),
                expires,
                cancelled: watch::channel(false).0,
                traffic: Arc::new(crate::transport::TrafficAggregate::default()),
                active_links: 0,
                traffic_complete: true,
            },
        };
        session.summary.owner = owner.clone();
        self.events.emit(
            if resumed {
                "client.resumed"
            } else {
                "client.connected"
            },
            Some(&owner),
            serde_json::json!({"peer":session.summary.peer,"transport":session.summary.transport}),
        );
        state.sessions.insert(id.clone(), client_id.clone());
        state.clients.insert(client_id, session);
        Ok((
            owner,
            SessionCredential {
                session_id: id,
                secret,
            },
        ))
    }
    pub(crate) fn prune(&self) {
        self.state.lock().unwrap().prune(&self.events);
    }
    pub(crate) fn bind(&self, credential: &SessionCredential) -> Result<SessionOwner> {
        let mut state = self.state.lock().unwrap();
        state.prune(&self.events);
        let client_id = state
            .sessions
            .get(&credential.session_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown or expired client session"))?;
        let timeout = state.disconnect_timeout;
        let session = state
            .clients
            .get_mut(&client_id)
            .filter(|s| s.secret == credential.secret)
            .ok_or_else(|| anyhow!("unknown or expired client session"))?;
        session.expires = Instant::now() + timeout;
        session.summary.last_seen_at_ms = now_ms();
        Ok(session.summary.owner.clone())
    }
    pub(crate) fn cancellation(&self, owner: &SessionOwner) -> Result<watch::Receiver<bool>> {
        let mut state = self.state.lock().unwrap();
        state.prune(&self.events);
        let session = state
            .clients
            .get(&owner.client_id)
            .filter(|s| s.summary.owner.session_id == owner.session_id)
            .ok_or_else(|| anyhow!("unknown or expired client session"))?;
        Ok(session.cancelled.subscribe())
    }
    pub(crate) fn close(&self, credential: &SessionCredential) -> Result<()> {
        let owner = self.bind(credential)?;
        self.events.emit(
            "client.close_requested",
            Some(&owner),
            serde_json::json!({}),
        );
        let mut state = self.state.lock().unwrap();
        if let Some(session) = state.clients.get_mut(&owner.client_id) {
            session.expires = Instant::now();
        }
        state.prune(&self.events);
        Ok(())
    }
    pub(crate) fn track_link(
        &self,
        owner: &SessionOwner,
        traffic: Option<Arc<crate::transport::TrafficMeter>>,
    ) -> Option<LinkGuard> {
        let mut state = self.state.lock().unwrap();
        let session = state.clients.get_mut(&owner.client_id)?;
        session.active_links += 1;
        match traffic {
            Some(meter) => session.traffic_complete &= meter.attach_to(session.traffic.clone()),
            None => session.traffic_complete = false,
        }
        Some(LinkGuard {
            registry: self.clone(),
            id: owner.client_id.clone(),
            traffic: session.traffic.clone(),
        })
    }
    pub(crate) fn channel(
        &self,
        kind: ChannelKind,
        owner: Option<SessionOwner>,
        peer: &str,
        transport: &str,
        target: String,
        metadata: ChannelMetadata,
    ) -> ChannelGuard {
        let id = format!("channel-{:032x}", rand::random::<u128>());
        let summary = ChannelSummary {
            channel_id: id.clone(),
            kind,
            owner,
            peer: peer.into(),
            transport: transport.into(),
            target,
            metadata: metadata.normalized(),
            created_at_ms: now_ms(),
        };
        self.events.emit("channel.opened", summary.owner.as_ref(), serde_json::json!({"channelId":id,"kind":kind,"peer":peer,"transport":transport,"metadata":summary.metadata,"target":summary.target}));
        self.state
            .lock()
            .unwrap()
            .channels
            .insert(id.clone(), summary);
        ChannelGuard {
            registry: self.clone(),
            id,
        }
    }
    pub(crate) fn snapshot(&self) -> (Vec<ClientSessionSummary>, Vec<ChannelSummary>) {
        let mut state = self.state.lock().unwrap();
        state.prune(&self.events);
        (
            state
                .clients
                .values()
                .map(|s| {
                    let mut row = s.summary.clone();
                    let t = s.traffic.snapshot();
                    row.active_links = s.active_links;
                    row.traffic_complete = s.traffic_complete;
                    row.traffic = TrafficSummary {
                        received_bytes: t.received_bytes,
                        sent_bytes: t.sent_bytes,
                        receive_bytes_per_second: t.receive_bytes_per_second,
                        send_bytes_per_second: t.send_bytes_per_second,
                    };
                    row
                })
                .collect(),
            state.channels.values().cloned().collect(),
        )
    }
}
pub(crate) struct LinkGuard {
    registry: Registry,
    id: String,
    traffic: Arc<crate::transport::TrafficAggregate>,
}
impl Drop for LinkGuard {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock().unwrap();
        if let Some(client) = state
            .clients
            .get_mut(&self.id)
            .filter(|c| Arc::ptr_eq(&c.traffic, &self.traffic))
        {
            client.active_links = client.active_links.saturating_sub(1);
        }
    }
}
pub(crate) struct ChannelGuard {
    registry: Registry,
    id: String,
}
impl Drop for ChannelGuard {
    fn drop(&mut self) {
        if let Some(c) = self
            .registry
            .state
            .lock()
            .unwrap()
            .channels
            .remove(&self.id)
        {
            self.registry.events.emit(
                "channel.closed",
                c.owner.as_ref(),
                serde_json::json!({"channelId":c.channel_id,"kind":c.kind,"target":c.target}),
            );
        }
    }
}
