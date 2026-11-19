//! One-time transfer of an in-flight PTY subscription. There is no replay buffer.
use super::ShellManager;
use crate::protocol::management::SessionOwner;
use anyhow::{anyhow, Result};
use remote_executor_terminal::{
    session::{OutputSubscription, Session},
    TermSize,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

const HANDOFF_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct PendingOutput {
    pub session: Arc<Session>,
    pub output: OutputSubscription,
    pub size: TermSize,
}

#[derive(Clone)]
pub(super) struct PendingStreams {
    slots: Arc<Semaphore>,
    streams: Arc<Mutex<HashMap<String, Reservation>>>,
}

struct Reservation {
    expires: Instant,
    _slot: OwnedSemaphorePermit,
    stream: PendingOutput,
}

impl Default for PendingStreams {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(16)),
            streams: Default::default(),
        }
    }
}

impl PendingStreams {
    pub(super) fn remove(&self, id: &str) -> Option<PendingOutput> {
        let reservation = self.streams.lock().unwrap().remove(id)?;
        (reservation.expires > Instant::now()).then_some(reservation.stream)
    }

    pub(super) fn prune(&self) {
        let now = Instant::now();
        self.streams
            .lock()
            .unwrap()
            .retain(|_, entry| entry.expires > now);
    }
}

impl ShellManager {
    pub(crate) fn reserve_output_slot(&self) -> Result<OwnedSemaphorePermit> {
        self.pending_output.prune();
        self.pending_output
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow!("PTY output handoff capacity reached"))
    }

    pub(crate) fn reserve_output(
        &self,
        id: &str,
        slot: OwnedSemaphorePermit,
        pending: PendingOutput,
    ) {
        self.pending_output.streams.lock().unwrap().insert(
            id.into(),
            Reservation {
                expires: Instant::now() + HANDOFF_TIMEOUT,
                _slot: slot,
                stream: pending,
            },
        );
    }

    pub(crate) fn take_reserved_output(
        &self,
        id: &str,
        owner: Option<&SessionOwner>,
    ) -> Result<PendingOutput> {
        self.require_owner(id, owner)?;
        self.pending_output
            .remove(id)
            .ok_or_else(|| anyhow!("initial PTY output subscription was already taken or expired"))
    }
}
