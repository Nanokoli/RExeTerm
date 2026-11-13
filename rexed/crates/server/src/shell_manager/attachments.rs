//! RE attachment identities and serialized control/input policy.
use super::{ShellManager, TerminalEvent};
use crate::protocol::{
    management::{AttachedClient, SessionOwner},
    terminal::ClientSummary,
};
use anyhow::{anyhow, ensure, Result};
use remote_executor_terminal::{session::Session, TermSize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

struct ConnectedClient {
    token: u64,
    read_only: bool,
    size: TermSize,
    tx: mpsc::UnboundedSender<TerminalEvent>,
    peer_addr: String,
    owner: Option<SessionOwner>,
}
struct Controller {
    id: String,
    owner: Option<SessionOwner>,
}
pub(super) struct AttachmentState {
    session: Arc<Session>,
    clients: HashMap<String, ConnectedClient>,
    controller: Option<Controller>,
    locked: bool,
    closed: bool,
}
pub(crate) struct TerminalAttachment {
    state: Arc<Mutex<AttachmentState>>,
    pub id: String,
    token: u64,
}
impl TerminalAttachment {
    pub fn write(&self, data: &[u8]) -> Result<()> {
        let state = self.state.lock().unwrap();
        ensure!(
            !state.clients.get(&self.id).is_some_and(|c| c.read_only),
            "PTY is read-only for this client"
        );
        if !state.current(&self.id, self.token) || state.controller_id() != Some(&self.id) {
            return Ok(());
        }
        // Control changes and authorization stay serialized through the actual write.
        state.session.write_input(data)
    }
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            state.current(&self.id, self.token),
            "terminal attachment expired"
        );
        ensure!(
            !state.clients[&self.id].read_only,
            "PTY is read-only for this client"
        );
        let size = TermSize {
            cols: cols.max(1),
            rows: rows.max(1),
        };
        if state.controller_id() == Some(&self.id) {
            state.session.resize(size.cols, size.rows)?;
        }
        state.clients.get_mut(&self.id).unwrap().size = size;
        Ok(())
    }
    pub fn request_control(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            state.current(&self.id, self.token),
            "terminal attachment expired"
        );
        ensure!(
            !state.clients[&self.id].read_only,
            "PTY is read-only for this client"
        );
        ensure!(!state.locked || self.id == "0", "pty is locked to user 0");
        let client = &state.clients[&self.id];
        let owner = client.owner.clone();
        state.session.resize(client.size.cols, client.size.rows)?;
        state.controller = Some(Controller {
            id: self.id.clone(),
            owner,
        });
        Ok(())
    }
}
impl AttachmentState {
    fn new(session: Arc<Session>) -> Self {
        Self {
            session,
            clients: HashMap::new(),
            controller: None,
            locked: false,
            closed: false,
        }
    }
    fn current(&self, id: &str, token: u64) -> bool {
        !self.closed && self.clients.get(id).is_some_and(|c| c.token == token)
    }
    fn controller_id(&self) -> Option<&String> {
        self.controller.as_ref().map(|c| &c.id)
    }
    fn disconnect_all(&mut self) {
        for client in self.clients.drain().map(|(_, c)| c) {
            let _ = client.tx.send(TerminalEvent::Closed);
        }
        self.controller = self.locked.then(|| Controller {
            id: "0".into(),
            owner: None,
        });
        self.session.set_auto_status_response(true);
    }
}
impl ShellManager {
    fn attachment_state(&self, pty: &str) -> Result<Arc<Mutex<AttachmentState>>> {
        let mut states = self.attachments.lock().unwrap();
        if let Some(state) = states.get(pty) {
            return Ok(state.clone());
        }
        let session = self
            .core
            .session(pty)
            .ok_or_else(|| anyhow!("pty {pty} does not exist"))?;
        let state = Arc::new(Mutex::new(AttachmentState::new(session)));
        states.insert(pty.into(), state.clone());
        Ok(state)
    }
    fn existing_attachments(&self, pty: &str) -> Option<Arc<Mutex<AttachmentState>>> {
        self.attachments.lock().unwrap().get(pty).cloned()
    }
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_client(
        &self,
        pty: &str,
        requested: String,
        token: u64,
        cols: u16,
        rows: u16,
        tx: mpsc::UnboundedSender<TerminalEvent>,
        peer_addr: String,
        owner: Option<SessionOwner>,
    ) -> Result<TerminalAttachment> {
        self.register_client_with_policy(
            pty, requested, token, cols, rows, tx, peer_addr, owner, false,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn register_client_with_policy(
        &self,
        pty: &str,
        requested: String,
        token: u64,
        cols: u16,
        rows: u16,
        tx: mpsc::UnboundedSender<TerminalEvent>,
        peer_addr: String,
        owner: Option<SessionOwner>,
        force_read_only: bool,
    ) -> Result<TerminalAttachment> {
        let read_only = self.terminal_read_only(pty, owner.as_ref())? || force_read_only;
        let state = self.attachment_state(pty)?;
        let id = {
            let mut state = state.lock().unwrap();
            ensure!(!state.closed, "terminal attachments closed");
            let id = allocate_client_id(&state.clients, &requested);
            let size = TermSize {
                cols: cols.max(1),
                rows: rows.max(1),
            };
            if !read_only && (state.controller.is_none() || state.controller_id() == Some(&id)) {
                state.session.resize(size.cols, size.rows)?;
                state.controller = Some(Controller {
                    id: id.clone(),
                    owner: owner.clone(),
                });
            }
            state.clients.insert(
                id.clone(),
                ConnectedClient {
                    token,
                    read_only,
                    size,
                    tx,
                    peer_addr,
                    owner,
                },
            );
            state
                .session
                .set_auto_status_response(!state.clients.values().any(|c| !c.read_only));
            id
        };
        self.start_exit_watcher(pty);
        Ok(TerminalAttachment { state, id, token })
    }
    pub(crate) fn remove_client(&self, pty: &str, id: &str, token: u64) {
        let Some(state) = self.existing_attachments(pty) else {
            return;
        };
        let mut state = state.lock().unwrap();
        if !state.current(id, token) {
            return;
        }
        state.clients.remove(id);
        if state.controller_id().is_some_and(|c| c == id) {
            // A deterministic successor makes simultaneous views predictable.
            let next = state
                .clients
                .iter()
                .filter(|(_, c)| !c.read_only)
                .map(|(id, _)| id)
                .min()
                .cloned();
            state.controller = next.map(|id| {
                let client = &state.clients[&id];
                let _ = state.session.resize(client.size.cols, client.size.rows);
                Controller {
                    id,
                    owner: client.owner.clone(),
                }
            });
        }
        state
            .session
            .set_auto_status_response(!state.clients.values().any(|c| !c.read_only));
    }
    pub fn close_pty_clients(&self, pty: &str) {
        let Some(state) = self.existing_attachments(pty) else {
            return;
        };
        state.lock().unwrap().disconnect_all();
    }
    pub(super) fn remove_pty_runtime(&self, pty: &str) -> bool {
        // Prevent a concurrent attach from installing state for a removed PTY.
        let mut states = self.attachments.lock().unwrap();
        if let Some(state) = states.remove(pty) {
            let mut state = state.lock().unwrap();
            state.closed = true;
            state.disconnect_all();
        }
        let removed = self.core.state().remove_session(pty).is_some();
        drop(states);
        removed
    }
    pub(crate) fn controller_id(&self, pty: &str) -> Option<String> {
        self.existing_attachments(pty)
            .and_then(|s| s.lock().unwrap().controller_id().cloned())
    }
    pub(crate) fn controller_owner(&self, pty: &str) -> Option<SessionOwner> {
        self.existing_attachments(pty).and_then(|s| {
            s.lock()
                .unwrap()
                .controller
                .as_ref()
                .and_then(|c| c.owner.clone())
        })
    }
    pub(crate) fn claim_and_write(
        &self,
        pty: &str,
        id: &str,
        owner: Option<SessionOwner>,
        data: &[u8],
    ) -> Result<()> {
        self.require_owner(pty, owner.as_ref())?;
        let state = self.attachment_state(pty)?;
        let mut state = state.lock().unwrap();
        ensure!(!state.closed, "terminal attachments closed");
        ensure!(!state.locked || id == "0", "pty is locked to user 0");
        state.controller = Some(Controller {
            id: id.into(),
            owner,
        });
        state.session.write_input(data)
    }
    pub fn lock_pty(&self, pty: &str) -> Result<()> {
        let state = self.attachment_state(pty)?;
        let mut state = state.lock().unwrap();
        state.controller = Some(Controller {
            id: "0".into(),
            owner: None,
        });
        state.locked = true;
        Ok(())
    }
    pub fn unlock_pty(&self, pty: &str) {
        if let Some(state) = self.existing_attachments(pty) {
            state.lock().unwrap().locked = false;
        }
    }
    pub fn is_locked(&self, pty: &str) -> bool {
        self.existing_attachments(pty)
            .is_some_and(|s| s.lock().unwrap().locked)
    }
    pub(crate) fn attached_clients(&self, pty: &str) -> Vec<AttachedClient> {
        let Some(state) = self.existing_attachments(pty) else {
            return vec![];
        };
        let state = state.lock().unwrap();
        let mut clients: Vec<_> = state
            .clients
            .iter()
            .map(|(id, c)| AttachedClient {
                id: id.clone(),
                peer: c.peer_addr.clone(),
                owner: c.owner.clone(),
            })
            .collect();
        clients.sort_by(|a, b| a.id.cmp(&b.id));
        clients
    }
    pub(crate) fn client_details(&self, pty: &str) -> Vec<ClientSummary> {
        self.attached_clients(pty)
            .into_iter()
            .map(|c| ClientSummary {
                id: c.id,
                peer_addr: c.peer,
            })
            .collect()
    }
    pub(super) fn metadata_clients(
        &self,
        pty: &str,
    ) -> Vec<(String, bool, mpsc::UnboundedSender<TerminalEvent>)> {
        self.existing_attachments(pty)
            .map(|s| {
                s.lock()
                    .unwrap()
                    .clients
                    .iter()
                    .map(|(id, c)| (id.clone(), c.read_only, c.tx.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}
fn allocate_client_id(clients: &HashMap<String, ConnectedClient>, requested: &str) -> String {
    let requested = requested.trim();
    if !requested.is_empty() && !clients.contains_key(requested) {
        return requested.into();
    }
    loop {
        let id = format!("client-{:016x}", rand::random::<u64>());
        if !clients.contains_key(&id) {
            return id;
        }
    }
}

#[cfg(test)]
mod tests;
