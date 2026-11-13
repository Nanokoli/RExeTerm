use crate::{Connector, DefaultConnector, Frame, KcpAcceptor};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::{
    net::UdpSocket,
    time::{timeout, Duration},
};

#[tokio::test]
async fn reliable_udp_recovers_from_dropped_packets() {
    let mut server = KcpAcceptor::bind("127.0.0.1:0").await.unwrap();
    let upstream = server.local_addr().unwrap();
    let proxy = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let dropped = Arc::new(AtomicUsize::new(0));
    let counter = dropped.clone();
    let proxy_task = tokio::spawn(async move {
        let mut client = None;
        let mut sequence = 0usize;
        let mut buffer = vec![0; 65536];
        loop {
            let (n, peer) = proxy.recv_from(&mut buffer).await.unwrap();
            sequence += 1;
            let target = if peer == upstream {
                match client {
                    Some(peer) => peer,
                    None => continue,
                }
            } else {
                client = Some(peer);
                upstream
            };
            if sequence.is_multiple_of(13) {
                counter.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            proxy.send_to(&buffer[..n], target).await.unwrap();
        }
    });
    let payload: Vec<u8> = (0..128 * 1024).map(|n| (n % 251) as u8).collect();
    let expected = payload.clone();
    let server_task = tokio::spawn(async move {
        let (mut link, _) = server.accept().await.unwrap();
        let frame = link.recv().await.unwrap().unwrap();
        assert_eq!(frame, Frame::Binary(expected));
        link.send(frame).await.unwrap();
        link.close().await.unwrap();
    });
    let start = Instant::now();
    timeout(Duration::from_secs(25), async {
        let mut link = DefaultConnector
            .connect(&format!("kcp://{proxy_address}"))
            .await
            .unwrap();
        link.send(Frame::Binary(payload.clone())).await.unwrap();
        assert_eq!(link.recv().await.unwrap(), Some(Frame::Binary(payload)));
        assert!(link.recv().await.unwrap().is_none());
        server_task.await.unwrap();
    })
    .await
    .unwrap();
    assert!(dropped.load(Ordering::Relaxed) > 0);
    eprintln!(
        "KCP trial: 131072 bytes each direction; dropped_datagrams={}; elapsed_ms={}",
        dropped.load(Ordering::Relaxed),
        start.elapsed().as_millis()
    );
    proxy_task.abort();
    let _ = proxy_task.await;
}
