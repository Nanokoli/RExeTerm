//! Bounded independent channel classes over one or more physical targets.
use crate::{Connector, Link};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Copy, Debug)]
pub enum Purpose {
    Control,
    Query,
    Pty,
    File,
    Forward,
}
impl Purpose {
    fn index(self) -> usize {
        self as usize
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelLimits {
    pub control: usize,
    pub query: usize,
    pub pty: usize,
    pub file: usize,
    pub forward: usize,
}
impl Default for ChannelLimits {
    fn default() -> Self {
        Self {
            control: 2,
            query: 4,
            pty: 4,
            file: 4,
            forward: 8,
        }
    }
}
impl ChannelLimits {
    fn values(&self) -> [usize; 5] {
        [self.control, self.query, self.pty, self.file, self.forward]
    }
    pub fn validate(&self) -> Result<()> {
        if self.values().iter().any(|n| *n == 0 || *n > 1024) {
            bail!("channel limits must be between 1 and 1024");
        }
        Ok(())
    }
}
struct Route {
    target: String,
    slots: Arc<Semaphore>,
}
struct State {
    connector: Arc<dyn Connector>,
    routes: Vec<Route>,
    lanes: [Arc<Semaphore>; 5],
    next: AtomicUsize,
    available: Arc<Notify>,
    limits: ChannelLimits,
}
#[derive(Clone)]
pub struct ChannelPool(Arc<State>);
/// Keep the lease alive while using its link, including after splitting it.
pub struct ChannelLease {
    lane: Option<OwnedSemaphorePermit>,
    route: Option<OwnedSemaphorePermit>,
    available: Arc<Notify>,
}
impl Drop for ChannelLease {
    fn drop(&mut self) {
        // Notify after releasing permits, otherwise a waking task can observe
        // no capacity and miss the eventual release.
        self.lane.take();
        self.route.take();
        self.available.notify_waiters();
    }
}
impl ChannelPool {
    pub fn new(
        connector: Arc<dyn Connector>,
        targets: Vec<String>,
        limits: ChannelLimits,
    ) -> Result<Self> {
        limits.validate()?;
        if targets.is_empty() {
            bail!("at least one transport target is required");
        }
        let mut routes = Vec::new();
        for target in targets {
            if routes.iter().any(|route: &Route| route.target == target) {
                continue;
            }
            let capacity = connector.max_parallel_links(&target);
            if capacity == 0 || capacity > 65536 {
                bail!("invalid connector capacity: {capacity}");
            }
            routes.push(Route {
                target,
                slots: Arc::new(Semaphore::new(capacity)),
            });
        }
        Ok(Self(Arc::new(State {
            connector,
            routes,
            lanes: limits.values().map(|n| Arc::new(Semaphore::new(n))),
            next: AtomicUsize::new(0),
            available: Arc::new(Notify::new()),
            limits,
        })))
    }
    pub fn limits(&self) -> &ChannelLimits {
        &self.0.limits
    }
    pub fn targets(&self) -> Vec<&str> {
        self.0.routes.iter().map(|r| r.target.as_str()).collect()
    }
    pub async fn connect(&self, purpose: Purpose) -> Result<(Link, ChannelLease)> {
        let mut lane = Some(
            self.0.lanes[purpose.index()]
                .clone()
                .acquire_owned()
                .await?,
        );
        loop {
            let notified = self.0.available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let start = self.0.next.fetch_add(1, Ordering::Relaxed);
            let mut errors = Vec::new();
            for offset in 0..self.0.routes.len() {
                let route = &self.0.routes[start.wrapping_add(offset) % self.0.routes.len()];
                let Ok(slot) = route.slots.clone().try_acquire_owned() else {
                    continue;
                };
                let mut lease = ChannelLease {
                    lane: lane.take(),
                    route: Some(slot),
                    available: self.0.available.clone(),
                };
                match self.0.connector.connect_for(&route.target, purpose).await {
                    Ok(link) => return Ok((link, lease)),
                    Err(error) => {
                        lane = lease.lane.take();
                        errors.push(format!("{}: {error}", route.target));
                    }
                }
            }
            // Only connection establishment can fail over; sent application
            // requests are never replayed by this scheduler.
            if !errors.is_empty() {
                return Err(anyhow!("all available links failed: {}", errors.join("; ")));
            }
            notified.await;
        }
    }
}
