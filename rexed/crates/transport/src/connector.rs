use crate::{from_stream, from_websocket, Link, LinkFuture};
use anyhow::bail;
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async_with_config;

/// Inject a connector for custom hardware or another physical link. URLs stay
/// opaque above this boundary; opening/authenticating a link belongs here.
pub trait Connector: Send + Sync {
    /// Custom physical devices default to a single active link.
    fn max_parallel_links(&self, _target: &str) -> usize {
        1
    }
    fn connect_for<'a>(
        &'a self,
        target: &'a str,
        _purpose: crate::Purpose,
    ) -> LinkFuture<'a, Link> {
        self.connect(target)
    }
    fn connect<'a>(&'a self, target: &'a str) -> LinkFuture<'a, Link>;
}
#[derive(Default)]
pub struct DefaultConnector;
impl Connector for DefaultConnector {
    fn max_parallel_links(&self, _target: &str) -> usize {
        32
    }
    fn connect_for<'a>(&'a self, target: &'a str, purpose: crate::Purpose) -> LinkFuture<'a, Link> {
        Box::pin(async move {
            #[cfg(feature = "kcp")]
            if let Some(address) = target.strip_prefix("kcp://") {
                return crate::connect_kcp(address, purpose).await;
            }
            let _ = purpose;
            self.connect(target).await
        })
    }
    fn connect<'a>(&'a self, target: &'a str) -> LinkFuture<'a, Link> {
        Box::pin(async move {
            #[cfg(feature = "kcp")]
            if let Some(address) = target.strip_prefix("kcp://") {
                return crate::connect_kcp(address, crate::Purpose::Query).await;
            }
            if target.starts_with("tls://") || target.starts_with("wss://") {
                return crate::TlsConnector::new(crate::TlsClientOptions::default())?
                    .connect(target)
                    .await;
            }
            if target.starts_with("ws://") {
                let (ws, _) = connect_async_with_config(target, None, true).await?;
                return Ok(from_websocket(ws));
            }
            if let Some(address) = target.strip_prefix("tcp://") {
                let stream = TcpStream::connect(address).await?;
                stream.set_nodelay(true)?;
                return Ok(from_stream(stream));
            }
            #[cfg(unix)]
            if let Some(path) = target.strip_prefix("unix://") {
                return Ok(from_stream(tokio::net::UnixStream::connect(path).await?));
            }
            bail!("unsupported transport target: {target}; inject a Connector for custom links")
        })
    }
}
