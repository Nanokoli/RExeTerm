//! Live-only PTY fanout with byte-bounded, coalesced queues for each subscriber.
use super::Session;
use anyhow::{anyhow, Result};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
};
use tokio::sync::Notify;

const OUTPUT_CHUNK_BYTES: usize = 64 * 1024;
/// In-flight bytes for one viewer. Consumed data is never retained for replay.
pub const LIVE_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub(super) struct OutputState {
    total_bytes: AtomicU64,
    closed: std::sync::atomic::AtomicBool,
    subscribers: Mutex<Vec<Weak<OutputQueue>>>,
}
#[derive(Default)]
struct Pending {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    lagged: bool,
    closed: bool,
}
#[derive(Default)]
struct OutputQueue {
    pending: Mutex<Pending>,
    ready: Notify,
}
/// A live subscription owns only its unread, bounded send queue.
/// New subscriptions start empty; slow viewers never block the PTY reader.
pub struct OutputSubscription {
    queue: Arc<OutputQueue>,
}
impl OutputQueue {
    fn push(&self, mut bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut pending = self.pending.lock().unwrap();
        if pending.lagged || pending.closed {
            return;
        }
        let wake = pending.bytes == 0;
        if bytes.len() > LIVE_OUTPUT_LIMIT - pending.bytes {
            pending.lagged = true;
            pending.chunks.clear();
            pending.bytes = 0;
            drop(pending);
            self.ready.notify_one();
            return;
        }
        pending.bytes += bytes.len();
        while !bytes.is_empty() {
            if pending
                .chunks
                .back()
                .is_none_or(|chunk| chunk.len() == OUTPUT_CHUNK_BYTES)
            {
                pending
                    .chunks
                    .push_back(Vec::with_capacity(OUTPUT_CHUNK_BYTES));
            }
            let chunk = pending.chunks.back_mut().unwrap();
            let count = bytes.len().min(OUTPUT_CHUNK_BYTES - chunk.len());
            chunk.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
        }
        drop(pending);
        // Wake on the empty -> nonempty transition, not for every small read.
        if wake {
            self.ready.notify_one();
        }
    }
    fn pop(&self) -> Result<Option<Vec<u8>>> {
        let mut pending = self.pending.lock().unwrap();
        if pending.lagged {
            return Err(anyhow!("PTY live output unavailable: viewer lagged beyond {LIVE_OUTPUT_LIMIT} in-flight bytes"));
        }
        if let Some(chunk) = pending.chunks.pop_front() {
            pending.bytes -= chunk.len();
            return Ok(Some(chunk));
        }
        if pending.closed {
            return Err(anyhow!("PTY live output closed"));
        }
        Ok(None)
    }
    fn close(&self) {
        self.pending.lock().unwrap().closed = true;
        self.ready.notify_one();
    }
}
impl OutputSubscription {
    pub fn is_closed(&self) -> bool {
        let pending = self.queue.pending.lock().unwrap();
        pending.closed && pending.bytes == 0 && !pending.lagged
    }
    pub fn try_recv(&mut self) -> Result<Option<Vec<u8>>> {
        self.queue.pop()
    }
    pub async fn recv(&mut self) -> Result<Vec<u8>> {
        loop {
            // Check after creating the notification so cancellation or an
            // arriving byte between pop and await cannot lose a wakeup.
            let ready = self.queue.ready.notified();
            if let Some(chunk) = self.queue.pop()? {
                return Ok(chunk);
            }
            ready.await;
        }
    }
}
impl Drop for OutputState {
    fn drop(&mut self) {
        for queue in self
            .subscribers
            .get_mut()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            queue.close();
        }
    }
}
impl Session {
    pub async fn wait_output_eof(&self) {
        loop {
            let notified = self.output_finished.notified();
            if self.output_eof.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    /// Existing subscribers drain their tail before observing closure.
    pub fn close_output(&self) {
        self.output.closed.store(true, Ordering::Release);
        for queue in self
            .output
            .subscribers
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            queue.close();
        }
    }
    pub fn on_pty_output(&self, data: &[u8]) {
        self.output
            .total_bytes
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        let mut subscribers = self.output.subscribers.lock().unwrap();
        subscribers.retain(|weak| {
            if let Some(queue) = weak.upgrade() {
                queue.push(data);
                true
            } else {
                false
            }
        });
    }
    pub fn subscribe_output(&self) -> OutputSubscription {
        let queue = Arc::new(OutputQueue::default());
        let mut subscribers = self.output.subscribers.lock().unwrap();
        subscribers.retain(|weak| weak.strong_count() != 0);
        subscribers.push(Arc::downgrade(&queue));
        if self.output.closed.load(Ordering::Acquire) {
            queue.close();
        }
        OutputSubscription { queue }
    }
    pub fn output_total_bytes(&self) -> u64 {
        self.output.total_bytes.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_wait_does_not_lose_subsequent_binary_output() {
        let queue = Arc::new(OutputQueue::default());
        let mut output = OutputSubscription {
            queue: queue.clone(),
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), output.recv())
                .await
                .is_err()
        );
        queue.push(b"\0\xff\x1b[31m");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), output.recv())
                .await
                .unwrap()
                .unwrap(),
            b"\0\xff\x1b[31m"
        );
        queue.push(b"tail");
        queue.close();
        assert_eq!(output.recv().await.unwrap(), b"tail");
        assert!(output.recv().await.is_err());
    }
}
