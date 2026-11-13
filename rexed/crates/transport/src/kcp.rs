//! Experimental reliable UDP stream backend, reusing REX1 framing.
use crate::{from_stream, Link, Purpose};
use anyhow::{anyhow, Result};
use tokio_kcp::{KcpConfig, KcpListener, KcpNoDelayConfig, KcpStream};
pub fn kcp_config(purpose: Purpose) -> KcpConfig {
    let interactive = matches!(purpose, Purpose::Control | Purpose::Pty);
    KcpConfig {
        stream: true,
        nodelay: if interactive {
            KcpNoDelayConfig {
                nodelay: true,
                interval: 10,
                resend: 2,
                nc: false,
            }
        } else {
            KcpNoDelayConfig::normal()
        },
        flush_write: interactive,
        flush_acks_input: true,
        ..KcpConfig::default()
    }
}
pub async fn connect_kcp(address: &str, purpose: Purpose) -> Result<Link> {
    let peer = tokio::net::lookup_host(address)
        .await?
        .next()
        .ok_or_else(|| anyhow!("KCP address resolved to no peers"))?;
    Ok(from_stream(ReliableKcpIo::new(
        KcpStream::connect(&kcp_config(purpose), peer).await?,
    )))
}
pub struct KcpAcceptor(KcpListener);
impl KcpAcceptor {
    pub async fn bind(address: &str) -> Result<Self> {
        Ok(Self(
            KcpListener::bind(kcp_config(Purpose::Pty), address).await?,
        ))
    }
    pub fn local_addr(&self) -> Result<std::net::SocketAddr> {
        Ok(self.0.local_addr()?)
    }
    pub async fn accept(&mut self) -> Result<(Link, String)> {
        let (stream, peer) = self.0.accept().await?;
        Ok((from_stream(ReliableKcpIo::new(stream)), peer.to_string()))
    }
}

// tokio_kcp's flush only queues UDP packets and its Drop stops retransmission.
// Wait for ACKs before reporting a frame flushed, so a completed final frame
// survives closing the stream. This conservative policy is experimental.
struct ReliableKcpIo {
    stream: KcpStream,
    delay: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
    flushing_since: Option<std::time::Instant>,
}
impl ReliableKcpIo {
    fn new(stream: KcpStream) -> Self {
        Self {
            stream,
            delay: None,
            flushing_since: None,
        }
    }
}
impl tokio::io::AsyncRead for ReliableKcpIo {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl tokio::io::AsyncWrite for ReliableKcpIo {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::{future::Future, task::Poll};
        match std::pin::Pin::new(&mut self.stream).poll_flush(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        if self.stream.session().kcp_socket().lock().can_close() {
            self.delay = None;
            self.flushing_since = None;
            return Poll::Ready(Ok(()));
        }
        if self
            .flushing_since
            .get_or_insert_with(std::time::Instant::now)
            .elapsed()
            > std::time::Duration::from_secs(30)
        {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "KCP acknowledgement timed out",
            )));
        }
        let delay = self.delay.get_or_insert_with(|| {
            Box::pin(tokio::time::sleep(std::time::Duration::from_millis(5)))
        });
        if delay.as_mut().poll(cx).is_ready() {
            self.delay = None;
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}
