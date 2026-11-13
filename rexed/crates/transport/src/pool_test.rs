use crate::{from_stream, ChannelLimits, ChannelPool, Connector, Frame, Link, LinkFuture, Purpose};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::{
    io::duplex,
    sync::Notify,
    time::{timeout, Duration},
};
struct TestConnector {
    calls: AtomicUsize,
    block_first: bool,
    hold: std::sync::Mutex<Vec<tokio::io::DuplexStream>>,
}
impl Connector for TestConnector {
    fn max_parallel_links(&self, _: &str) -> usize {
        if self.block_first {
            1
        } else {
            8
        }
    }
    fn connect<'a>(&'a self, _: &'a str) -> LinkFuture<'a, Link> {
        Box::pin(async move {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.block_first && n == 0 {
                Notify::new().notified().await;
            }
            let (a, b) = duplex(128);
            self.hold.lock().unwrap().push(b);
            Ok(from_stream(a))
        })
    }
}
fn connector(block_first: bool) -> Arc<TestConnector> {
    Arc::new(TestConnector {
        calls: AtomicUsize::new(0),
        block_first,
        hold: Default::default(),
    })
}
#[tokio::test]
async fn busy_query_lane_does_not_block_control_or_other_links() {
    let pool = ChannelPool::new(
        connector(false),
        vec!["one".into(), "two".into()],
        ChannelLimits {
            query: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let (_link, lease) = pool.connect(Purpose::Query).await.unwrap();
    assert!(
        timeout(Duration::from_millis(10), pool.connect(Purpose::Query))
            .await
            .is_err()
    );
    let (_control, _permit) = timeout(Duration::from_secs(1), pool.connect(Purpose::Control))
        .await
        .unwrap()
        .unwrap();
    drop(lease);
    assert!(
        timeout(Duration::from_secs(1), pool.connect(Purpose::Query))
            .await
            .unwrap()
            .is_ok()
    );
}
#[tokio::test]
async fn cancelled_connection_attempt_releases_physical_capacity() {
    let pool =
        ChannelPool::new(connector(true), vec!["serial".into()], Default::default()).unwrap();
    assert!(
        timeout(Duration::from_millis(10), pool.connect(Purpose::Query))
            .await
            .is_err()
    );
    let (mut link, _lease) = timeout(Duration::from_secs(1), pool.connect(Purpose::Control))
        .await
        .unwrap()
        .unwrap();
    link.send(Frame::Text("works".into())).await.unwrap();
}
#[tokio::test]
async fn dropping_a_lease_wakes_waiting_physical_channel() {
    let pool =
        ChannelPool::new(connector(true), vec!["serial".into()], Default::default()).unwrap();
    // Consume the deliberately stalled first attempt.
    let _ = timeout(Duration::from_millis(5), pool.connect(Purpose::Query)).await;
    let (_link, lease) = pool.connect(Purpose::Query).await.unwrap();
    let waiting = {
        let pool = pool.clone();
        tokio::spawn(async move { pool.connect(Purpose::Control).await })
    };
    tokio::task::yield_now().await;
    drop(lease);
    timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
