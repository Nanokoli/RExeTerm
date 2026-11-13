use crate::{from_stream, Connector, DefaultConnector, Frame, MAX_FRAME_BYTES};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{duplex, AsyncWriteExt},
    net::TcpListener,
    time::{timeout, Duration},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

fn header(kind: u8, len: u32) -> Vec<u8> {
    let mut bytes = b"REX1".to_vec();
    bytes.push(kind);
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes
}

#[tokio::test]
async fn stream_frames_roundtrip_preserve_message_boundaries_and_close() {
    let (one, two) = duplex(32);
    let mut reader = from_stream(two);
    let mut writer = from_stream(one);
    let task = tokio::spawn(async move {
        writer.send(Frame::Text("中文\ntext".into())).await.unwrap();
        writer.send(Frame::Binary(vec![])).await.unwrap();
        writer
            .send(Frame::Binary(vec![0, 255, 1, 2]))
            .await
            .unwrap();
        writer.close().await.unwrap();
        assert!(writer.send(Frame::Text("late".into())).await.is_err());
    });
    assert_eq!(
        reader.recv().await.unwrap(),
        Some(Frame::Text("中文\ntext".into()))
    );
    assert_eq!(reader.recv().await.unwrap(), Some(Frame::Binary(vec![])));
    assert_eq!(
        reader.recv().await.unwrap(),
        Some(Frame::Binary(vec![0, 255, 1, 2]))
    );
    assert_eq!(reader.recv().await.unwrap(), None);
    assert_eq!(reader.recv().await.unwrap(), None);
    task.await.unwrap();
}

#[tokio::test]
async fn cancelling_a_partial_read_preserves_header_and_payload() {
    let (mut bytes, stream) = duplex(64);
    let mut link = from_stream(stream);
    let frame = header(1, 6);
    bytes.write_all(&frame[..4]).await.unwrap();
    assert!(timeout(Duration::from_millis(5), link.recv())
        .await
        .is_err());
    bytes.write_all(&frame[4..]).await.unwrap();
    bytes.write_all(b"abc").await.unwrap();
    assert!(timeout(Duration::from_millis(5), link.recv())
        .await
        .is_err());
    bytes.write_all(b"def").await.unwrap();
    assert_eq!(
        link.recv().await.unwrap(),
        Some(Frame::Text("abcdef".into()))
    );
}

#[tokio::test]
async fn malformed_or_oversized_frames_are_rejected_before_reading_body() {
    for bytes in [
        header(1, MAX_FRAME_BYTES as u32 + 1),
        header(7, 0),
        header(3, 1),
        b"BAD!\x01\0\0\0\0".to_vec(),
    ] {
        let (mut sender, stream) = duplex(64);
        sender.write_all(&bytes).await.unwrap();
        let mut link = from_stream(stream);
        assert!(timeout(Duration::from_secs(1), link.recv())
            .await
            .unwrap()
            .is_err());
        assert_eq!(link.recv().await.unwrap(), None);
    }
}

#[tokio::test]
async fn truncation_and_invalid_text_are_errors_not_clean_eof() {
    let mut truncated_body = header(2, 5);
    truncated_body.extend_from_slice(b"ab");
    let mut bad_text = header(1, 1);
    bad_text.push(255);
    for bytes in [b"REX".to_vec(), truncated_body, bad_text] {
        let (mut sender, stream) = duplex(64);
        sender.write_all(&bytes).await.unwrap();
        sender.shutdown().await.unwrap();
        assert!(from_stream(stream).recv().await.is_err());
    }
}

#[tokio::test]
async fn websocket_control_frames_are_handled_without_data_layer_help() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        ws.send(Message::Ping(vec![1, 2, 3].into())).await.unwrap();
        ws.send(Message::Text("payload".into())).await.unwrap();
        loop {
            if let Message::Pong(bytes) = ws.next().await.unwrap().unwrap() {
                assert_eq!(bytes.as_ref(), &[1, 2, 3]);
                break;
            }
        }
    });
    let mut link = DefaultConnector.connect(&url).await.unwrap();
    assert_eq!(
        link.recv().await.unwrap(),
        Some(Frame::Text("payload".into()))
    );
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}
