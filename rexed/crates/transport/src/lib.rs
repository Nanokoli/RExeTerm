//! Link framing and adapters. Payloads are opaque to this crate: it knows
//! nothing about tools, JSON, terminal commands, files or executor identity.
mod connector;
pub mod tls;
pub use tls::{LinkSecurity, TlsClientOptions, TlsConnector};
mod traffic;
pub use traffic::{CountedIo, TrafficAggregate, TrafficMeter, TrafficSnapshot};
#[cfg(feature = "kcp")]
mod kcp;
#[cfg(feature = "kcp")]
pub use kcp::{connect_kcp, kcp_config, KcpAcceptor};
mod pool;
pub use pool::{ChannelLease, ChannelLimits, ChannelPool, Purpose};
mod stream;
mod websocket;

use anyhow::Result;
pub use connector::{Connector, DefaultConnector};
use std::{future::Future, pin::Pin};
pub use stream::from_stream;
pub use websocket::{accept_websocket, accept_websocket_at, from_websocket};

pub type LinkFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
    Close,
}
impl Frame {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Binary(bytes) => bytes.len(),
            Self::Close => 0,
        }
    }
}

/// An ordered message source. Calls must preserve state if cancelled.
pub trait FrameReader: Send {
    fn recv(&mut self) -> LinkFuture<'_, Option<Frame>>;
}
/// An ordered message destination. A cancelled/failed send can leave a partial
/// frame on stream links; close that link rather than retrying on it.
pub trait FrameWriter: Send {
    fn send(&mut self, frame: Frame) -> LinkFuture<'_, ()>;
}

pub struct Receiver(Box<dyn FrameReader>);
impl Receiver {
    pub async fn recv(&mut self) -> Result<Option<Frame>> {
        self.0.recv().await
    }
}
pub struct Sender(Option<Box<dyn FrameWriter>>);
impl Sender {
    pub async fn send(&mut self, frame: Frame) -> Result<()> {
        self.0.as_mut().expect("sender is live").send(frame).await
    }
    pub async fn close(&mut self) -> Result<()> {
        self.send(Frame::Close).await
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        if let (Some(mut writer), Ok(runtime)) =
            (self.0.take(), tokio::runtime::Handle::try_current())
        {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    writer.send(Frame::Close),
                )
                .await;
            });
        }
    }
}

pub struct Link {
    security: LinkSecurity,
    traffic: Option<std::sync::Arc<TrafficMeter>>,
    sender: Sender,
    receiver: Receiver,
}
impl Link {
    pub fn new(sender: impl FrameWriter + 'static, receiver: impl FrameReader + 'static) -> Self {
        Self {
            traffic: None,
            security: LinkSecurity::default(),
            sender: Sender(Some(Box::new(sender))),
            receiver: Receiver(Box::new(receiver)),
        }
    }
    pub fn from_halves(sender: Sender, receiver: Receiver) -> Self {
        Self {
            sender,
            receiver,
            traffic: None,
            security: LinkSecurity::default(),
        }
    }
    /// Custom adapters can expose counters collected beneath their framing.
    pub fn with_traffic(mut self, traffic: std::sync::Arc<TrafficMeter>) -> Self {
        self.traffic = Some(traffic);
        self
    }
    pub fn with_security(mut self, security: LinkSecurity) -> Self {
        self.security = security;
        self
    }
    pub fn security(&self) -> &LinkSecurity {
        &self.security
    }
    pub fn traffic(&self) -> Option<std::sync::Arc<TrafficMeter>> {
        self.traffic.clone()
    }
    pub fn split(self) -> (Sender, Receiver) {
        (self.sender, self.receiver)
    }
    /// Borrow both directions independently while the owner retains the link
    /// and its channel/session guards.
    pub fn halves(&mut self) -> (&mut Sender, &mut Receiver) {
        (&mut self.sender, &mut self.receiver)
    }
    pub async fn send(&mut self, frame: Frame) -> Result<()> {
        self.sender.send(frame).await
    }
    pub async fn recv(&mut self) -> Result<Option<Frame>> {
        self.receiver.recv().await
    }
    pub async fn close(&mut self) -> Result<()> {
        self.sender.close().await
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod pool_test;

#[cfg(all(test, feature = "kcp"))]
mod kcp_test;
