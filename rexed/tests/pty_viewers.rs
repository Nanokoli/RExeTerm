#![cfg(unix)]
use remote_executor::{
    client::{terminal::TerminalConnection, transport::Frame, Client, ConnectExecutorOptions},
    protocol::{
        terminal::{ClientText, ServerText},
        ExecutorRequest,
    },
    server::{start_executor, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::{sleep, timeout};

async fn client(url: &str, username: &str) -> Client {
    let client = Client::new().await.unwrap();
    client
        .connect_to_executor(ConnectExecutorOptions {
            authentication: None,
            tls: None,
            id: "node".into(),
            url: url.into(),
            username: Some(username.into()),
            client_name: None,
            links: vec![],
            channels: Default::default(),
            system: None,
            device: None,
            labels: BTreeMap::new(),
        })
        .await
        .unwrap();
    client
}
async fn call(client: &Client, params: Value) -> remote_executor::protocol::ExecutorResponse {
    client
        .handle(ExecutorRequest {
            id: json!(1),
            method: "exbash".into(),
            params,
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .await
}
async fn meta(connection: &mut TerminalConnection) -> ServerText {
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Frame::Text(text)) = connection.link().recv().await.unwrap() {
                let value: ServerText = serde_json::from_str(&text).unwrap();
                if matches!(value, ServerText::Meta { .. } | ServerText::Error { .. }) {
                    return value;
                }
            }
        }
    })
    .await
    .unwrap()
}
async fn reject(connection: &mut TerminalConnection, frame: Frame) {
    connection.link().send(frame).await.unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Frame::Text(text)) = connection.link().recv().await.unwrap() {
                if let ServerText::Error { message } = serde_json::from_str(&text).unwrap() {
                    assert!(message.contains("read-only"), "{message}");
                    return;
                }
            }
        }
    })
    .await
    .unwrap();
}
async fn until_bytes(connection: &mut TerminalConnection, marker: &[u8]) -> Vec<u8> {
    timeout(Duration::from_secs(5), async {
        let mut bytes = vec![];
        loop {
            match connection.link().recv().await.unwrap() {
                Some(Frame::Binary(chunk)) => bytes.extend(chunk),
                Some(Frame::Text(text)) => assert!(
                    !matches!(
                        serde_json::from_str::<ServerText>(&text).unwrap(),
                        ServerText::Error { .. }
                    ),
                    "{text}"
                ),
                _ => panic!("PTY closed before marker"),
            }
            if let Some(end) = bytes.windows(marker.len()).position(|w| w == marker) {
                bytes.truncate(end + marker.len());
                return bytes;
            }
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn known_same_user_pty_is_shared_read_only_without_expanding_lists_or_promoting_viewers() {
    for protocol in ["tcp", "ws"] {
        let manager = ShellManager::default_shell(80, 24);
        let url = start_executor(
            format!("{protocol}://127.0.0.1:0"),
            Executor::local("viewers"),
            manager.clone(),
        )
        .await
        .unwrap();
        let a = client(&url, "alice").await;
        let b = client(&url, "alice").await;
        let c = client(&url, "alice").await;
        let other = client(&url, "bob").await;
        let started = call(
            &a,
            json!({"mode":"run","command":"sh -c 'stty -echo; printf READY; cat'","read_timeout":1000}),
        )
        .await;
        assert!(started.ok, "{started:?}");
        assert!(
            started.result.as_ref().unwrap()["output"]["text"]
                .as_str()
                .unwrap()
                .contains("READY"),
            "cat must disable terminal echo before input is sent"
        );
        let id = started.result.unwrap()["metadata"]["asyncID"]
            .as_str()
            .unwrap()
            .to_string();
        for spectator in [&b, &c, &other] {
            assert_eq!(
                call(spectator, json!({"mode":"list"}))
                    .await
                    .result
                    .unwrap()["metadata"]["runs"],
                json!([])
            );
        }
        // Even an attachment ID of "0" never bypasses the read-only policy.
        let mut bv = b.open_terminal(None, &id, "0", 5, 5).await.unwrap();
        let mut cv = c
            .open_terminal(None, &id, "viewer-c", 200, 100)
            .await
            .unwrap();
        assert!(
            matches!(meta(&mut bv).await,ServerText::Meta {role,read_only:true,cols:80,rows:24,..} if role=="Viewer")
        );
        assert!(
            matches!(meta(&mut cv).await,ServerText::Meta {role,read_only:true,..} if role=="Viewer")
        );
        assert!(manager.detail(&id).unwrap().controller.is_none());
        let mut denied = other.open_terminal(None, &id, "bob", 80, 24).await.unwrap();
        assert!(matches!(meta(&mut denied).await, ServerText::Error { .. }));
        for viewer in [&mut bv, &mut cv] {
            reject(
                viewer,
                Frame::Text(serde_json::to_string(&ClientText::RequestControl).unwrap()),
            )
            .await;
            reject(
                viewer,
                Frame::Text(
                    serde_json::to_string(&ClientText::Resize { cols: 1, rows: 1 }).unwrap(),
                ),
            )
            .await;
            reject(viewer, Frame::Binary(b"must-not-arrive\n".to_vec())).await;
        }
        assert_eq!(
            (
                manager.detail(&id).unwrap().cols,
                manager.detail(&id).unwrap().rows
            ),
            (80, 24)
        );
        for mode in ["stop", "remove"] {
            assert!(!call(&b, json!({"mode":mode,"asyncID":id})).await.ok);
        }
        assert!(
            !call(
                &b,
                json!({"mode":"attach","asyncID":id,"text":"denied","read_timeout":0})
            )
            .await
            .ok
        );
        let read = call(&b, json!({"mode":"attach","asyncID":id,"read_timeout":0})).await;
        assert!(read.ok, "{read:?}");
        let mut controller = a.open_terminal(None, &id, "owner", 100, 30).await.unwrap();
        assert!(
            matches!(meta(&mut controller).await,ServerText::Meta {role,read_only:false,..} if role=="Controller")
        );
        let payload = "\x1b[31m共享中文\x1b[0m\n".repeat(100);
        controller
            .link()
            .send(Frame::Binary(format!("{payload}END-SHARED\n").into_bytes()))
            .await
            .unwrap();
        let (bo, co, ao) = tokio::join!(
            until_bytes(&mut bv, b"END-SHARED\r\n"),
            until_bytes(&mut cv, b"END-SHARED\r\n"),
            until_bytes(&mut controller, b"END-SHARED\r\n")
        );
        assert_eq!(bo, co);
        assert_eq!(bo, ao);
        assert!(!String::from_utf8_lossy(&bo).contains("must-not-arrive"));
        assert!(bo.windows(5).any(|w| w == b"\x1b[31m"));
        controller.link().close().await.unwrap();
        timeout(Duration::from_secs(5), async {
            while manager.detail(&id).unwrap().controller.is_some() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        reject(
            &mut bv,
            Frame::Text(serde_json::to_string(&ClientText::RequestControl).unwrap()),
        )
        .await;
        assert!(manager.detail(&id).unwrap().controller.is_none());
        assert!(call(&a, json!({"mode":"remove","asyncID":id})).await.ok);
        a.shutdown().await;
        b.shutdown().await;
        c.shutdown().await;
        other.shutdown().await;
    }
}
