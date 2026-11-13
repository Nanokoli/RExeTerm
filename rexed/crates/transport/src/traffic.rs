//! Byte counters below framing. No client, user or application protocol lives here.
use std::{
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll},
    time::Instant,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
#[derive(Clone, Copy, Default, Debug)]
pub struct TrafficSnapshot {
    pub received_bytes: u64,
    pub sent_bytes: u64,
    pub receive_bytes_per_second: u64,
    pub send_bytes_per_second: u64,
}
#[derive(Clone, Copy, Default)]
struct Bucket {
    tick: u64,
    received: u64,
    sent: u64,
}
#[derive(Default)]
struct State {
    received: u64,
    sent: u64,
    buckets: [Bucket; 10],
    parent: Option<Arc<TrafficAggregate>>,
}
#[derive(Default)]
pub struct TrafficMeter(Mutex<State>);
#[derive(Default)]
pub struct TrafficAggregate(TrafficMeter);
impl TrafficAggregate {
    pub fn snapshot(&self) -> TrafficSnapshot {
        self.0.snapshot()
    }
}
fn tick() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 / 100
}
impl State {
    fn add(&mut self, at: u64, received: u64, sent: u64) {
        self.received = self.received.saturating_add(received);
        self.sent = self.sent.saturating_add(sent);
        let b = &mut self.buckets[(at % 10) as usize];
        if b.tick != at {
            *b = Bucket {
                tick: at,
                ..Default::default()
            };
        }
        b.received = b.received.saturating_add(received);
        b.sent = b.sent.saturating_add(sent);
    }
    fn snapshot_at(&self, now: u64) -> TrafficSnapshot {
        let mut result = TrafficSnapshot {
            received_bytes: self.received,
            sent_bytes: self.sent,
            ..Default::default()
        };
        for b in self.buckets {
            if b.tick <= now && now - b.tick < 10 {
                result.receive_bytes_per_second =
                    result.receive_bytes_per_second.saturating_add(b.received);
                result.send_bytes_per_second = result.send_bytes_per_second.saturating_add(b.sent);
            }
        }
        result
    }
}
impl TrafficMeter {
    pub fn received(&self, bytes: usize) {
        self.record(bytes as u64, 0)
    }
    pub fn sent(&self, bytes: usize) {
        self.record(0, bytes as u64)
    }
    fn record(&self, received: u64, sent: u64) {
        if received == 0 && sent == 0 {
            return;
        }
        let parent = {
            let mut s = self.0.lock().unwrap();
            s.add(tick(), received, sent);
            s.parent.clone()
        };
        if let Some(parent) = parent {
            parent.0.record(received, sent)
        }
    }
    /// Route all past and future IO into an aggregate. Aggregates cannot themselves be linked.
    pub fn attach_to(&self, parent: Arc<TrafficAggregate>) -> bool {
        let mut s = self.0.lock().unwrap();
        if s.parent.is_some() {
            return false;
        }
        let mut p = parent.0 .0.lock().unwrap();
        if p.parent.is_some() {
            return false;
        }
        p.received = p.received.saturating_add(s.received);
        p.sent = p.sent.saturating_add(s.sent);
        let now = tick();
        for b in s.buckets {
            if now.saturating_sub(b.tick) < 10 {
                let dst = &mut p.buckets[(b.tick % 10) as usize];
                if dst.tick != b.tick {
                    *dst = Bucket {
                        tick: b.tick,
                        ..Default::default()
                    }
                }
                dst.received = dst.received.saturating_add(b.received);
                dst.sent = dst.sent.saturating_add(b.sent);
            }
        }
        drop(p);
        s.parent = Some(parent);
        true
    }
    pub fn snapshot(&self) -> TrafficSnapshot {
        self.0.lock().unwrap().snapshot_at(tick())
    }
}
pub struct CountedIo<S> {
    inner: S,
    meter: Arc<TrafficMeter>,
}
impl<S> CountedIo<S> {
    pub fn new(inner: S, meter: Arc<TrafficMeter>) -> Self {
        Self { inner, meter }
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for CountedIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        this.meter.received(buf.filled().len() - before);
        result
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for CountedIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &result {
            this.meter.sent(*n)
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{from_stream, Frame};
    use tokio::io::AsyncReadExt;
    #[tokio::test]
    async fn stream_meter_counts_rex1_headers_and_payload_at_the_io_boundary() {
        let (a, mut b) = tokio::io::duplex(1024);
        let mut link = from_stream(a);
        let meter = link.traffic().unwrap();
        link.send(Frame::Binary(vec![7; 31])).await.unwrap();
        let mut wire = [0; 40];
        b.read_exact(&mut wire).await.unwrap();
        assert_eq!(&wire[..4], b"REX1");
        assert_eq!(meter.snapshot().sent_bytes, 40);
    }
    #[tokio::test]
    async fn websocket_meter_includes_handshake_frames_and_automatic_pong() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (io, _) = listener.accept().await.unwrap();
            let mut link = crate::accept_websocket(io).await.unwrap();
            let meter = link.traffic().unwrap();
            let before = meter.snapshot();
            assert!(
                before.received_bytes > 0 && before.sent_bytes > 0,
                "HTTP upgrade IO"
            );
            assert_eq!(link.recv().await.unwrap(), Some(Frame::Text("abc".into())));
            link.send(Frame::Text("reply".into())).await.unwrap();
            let after = meter.snapshot();
            // Client masking adds four bytes: ping(2+4+3) and text(2+4+3).
            assert_eq!(after.received_bytes - before.received_bytes, 18);
            // Server pong(2+3) and text(2+5).
            assert_eq!(after.sent_bytes - before.sent_bytes, 12);
        });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{address}"))
            .await
            .unwrap();
        ws.send(Message::Ping(vec![1, 2, 3].into())).await.unwrap();
        ws.send(Message::Text("abc".into())).await.unwrap();
        assert!(matches!(
            ws.next().await.unwrap().unwrap(),
            Message::Pong(_)
        ));
        assert!(matches!(
            ws.next().await.unwrap().unwrap(),
            Message::Text(_)
        ));
        server.await.unwrap();
    }
    #[test]
    fn aggregate_includes_all_links_and_keeps_totals_after_they_close() {
        let total = Arc::new(TrafficAggregate::default());
        let a = TrafficMeter::default();
        let b = TrafficMeter::default();
        a.received(10);
        assert!(a.attach_to(total.clone()));
        assert!(b.attach_to(total.clone()));
        a.sent(20);
        b.received(30);
        assert!(!a.attach_to(total.clone()));
        drop(a);
        drop(b);
        let s = total.snapshot();
        assert_eq!((s.received_bytes, s.sent_bytes), (40, 20));
        assert_eq!(s.receive_bytes_per_second, 40);
    }
    #[test]
    fn idle_rate_expires_but_total_bytes_are_retained() {
        let mut state = State::default();
        state.add(100, 100, 200);
        assert_eq!(state.snapshot_at(109).receive_bytes_per_second, 100);
        let s = state.snapshot_at(110);
        assert_eq!((s.received_bytes, s.sent_bytes), (100, 200));
        assert_eq!(
            (s.receive_bytes_per_second, s.send_bytes_per_second),
            (0, 0)
        );
        state.add(110, 5, 7);
        let s = state.snapshot_at(110);
        assert_eq!((s.received_bytes, s.sent_bytes), (105, 207));
        assert_eq!(
            (s.receive_bytes_per_second, s.send_bytes_per_second),
            (5, 7)
        );
    }
}
