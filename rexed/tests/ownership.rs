use remote_executor::{
    client::{
        management::ManagementClient,
        new_client_credential,
        transport::{Connector, DefaultConnector, Frame, Link},
    },
    protocol::{
        file::{FileMessage, FileRequest},
        forward::{StreamMessage, StreamTarget},
        management::*,
        terminal::{AdminText, ClientText, ServerText},
        ExecutorRequest, ExecutorResponse,
    },
    server::{management::start_management_listener, start_executor, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::{
    io::AsyncReadExt,
    net::TcpListener,
    time::{sleep, timeout},
};
async fn exchange(link: &mut Link, message: &SessionMessage) -> SessionMessage {
    link.send(Frame::Text(serde_json::to_string(message).unwrap()))
        .await
        .unwrap();
    let Some(Frame::Text(text)) = link.recv().await.unwrap() else {
        panic!("session reply missing");
    };
    serde_json::from_str(&text).unwrap()
}
async fn open(url: &str, identity: &ClientCredential) -> (SessionOwner, SessionCredential) {
    let mut link = DefaultConnector.connect(url).await.unwrap();
    let reply = exchange(
        &mut link,
        &SessionMessage::OpenClient {
            client: ClientHello::default(),
            credential: identity.clone(),
        },
    )
    .await;
    let SessionMessage::ClientOpened { owner, credential } = reply else {
        panic!("client open failed");
    };
    (owner, credential)
}
async fn bound(
    url: &str,
    credential: &SessionCredential,
    metadata: Option<ChannelMetadata>,
) -> Link {
    let mut link = DefaultConnector.connect(url).await.unwrap();
    assert!(matches!(
        exchange(
            &mut link,
            &SessionMessage::Bind {
                credential: credential.clone(),
                metadata
            }
        )
        .await,
        SessionMessage::Ok
    ));
    link
}
async fn call(link: &mut Link, params: Value) -> ExecutorResponse {
    link.send(Frame::Text(
        serde_json::to_string(&ExecutorRequest {
            id: json!(1),
            method: "exbash".into(),
            params,
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let Some(Frame::Text(text)) = link.recv().await.unwrap() else {
        panic!("tool reply missing");
    };
    serde_json::from_str(&text).unwrap()
}
async fn start(link: &mut Link) -> String {
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -Command 'Start-Sleep -Seconds 30'"
    } else {
        "sh -c 'sleep 30'"
    };
    let reply = call(
        link,
        json!({"mode":"run","command":command,"read_timeout":0}),
    )
    .await;
    assert!(reply.ok, "{reply:?}");
    reply.result.unwrap()["metadata"]["asyncID"]
        .as_str()
        .unwrap()
        .into()
}
async fn inventory(link: &mut Link) -> Value {
    link.send(Frame::Text(
        json!({"id":2,"tool":"client_resources","params":{}}).to_string(),
    ))
    .await
    .unwrap();
    let Some(Frame::Text(text)) = link.recv().await.unwrap() else {
        panic!("inventory reply");
    };
    let reply: ExecutorResponse = serde_json::from_str(&text).unwrap();
    assert!(reply.ok, "{reply:?}");
    reply.result.unwrap()["metadata"].clone()
}
async fn close(url: &str, credential: SessionCredential) {
    let mut link = DefaultConnector.connect(url).await.unwrap();
    assert!(matches!(
        exchange(&mut link, &SessionMessage::Close { credential }).await,
        SessionMessage::Ok
    ));
}
#[tokio::test]
async fn same_username_cannot_list_or_control_foreign_resources_and_reconnect_rebuilds_own_index() {
    let manager = ShellManager::default_shell(80, 24);
    let executor = Executor::local("ownership");
    let url = start_executor("tcp://127.0.0.1:0", executor.clone(), manager.clone())
        .await
        .unwrap();
    let admin_guard = start_management_listener("tcp://127.0.0.1:0", executor, manager).unwrap();
    let mut admin = ManagementClient::connect(admin_guard.address())
        .await
        .unwrap();
    let identity = new_client_credential();
    let (a, ac) = open(&url, &identity).await;
    let (b, bc) = open(&url, &new_client_credential()).await;
    assert_eq!(a.username, b.username);
    assert_ne!(a.client_id, b.client_id);
    let mut alink = bound(&url, &ac, None).await;
    let mut blink = bound(&url, &bc, None).await;
    let aid = start(&mut alink).await;
    let bid = start(&mut blink).await;
    let listed = call(&mut alink, json!({"mode":"list"})).await;
    assert!(listed.ok);
    assert!(listed.result.as_ref().unwrap().to_string().contains(&aid));
    assert!(!listed.result.unwrap().to_string().contains(&bid));
    for mode in ["list", "attach", "stop", "remove"] {
        let reply = call(
            &mut alink,
            json!({"mode":mode,"asyncID":bid,"read_timeout":0,"text":"malicious"}),
        )
        .await;
        assert!(!reply.ok, "foreign {mode} accepted");
    }
    let mut pty = bound(&url, &ac, None).await;
    pty.send(Frame::Text(
        serde_json::to_string(&ClientText::Hello {
            id: "intruder".into(),
            pty: bid.clone(),
            cols: 80,
            rows: 24,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let Some(Frame::Text(text)) = pty.recv().await.unwrap() else {
        panic!("missing read-only PTY metadata");
    };
    assert!(matches!(
        serde_json::from_str(&text).unwrap(),
        ServerText::Meta {
            read_only: true,
            ..
        }
    ));
    pty.close().await.unwrap();
    let mut old_admin = bound(&url, &ac, None).await;
    old_admin
        .send(Frame::Text(
            serde_json::to_string(&AdminText::List).unwrap(),
        ))
        .await
        .unwrap();
    let Some(Frame::Text(text)) = old_admin.recv().await.unwrap() else {
        panic!("list reply");
    };
    let ServerText::Sessions { sessions } = serde_json::from_str(&text).unwrap() else {
        panic!("list sessions");
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].pty, aid);
    let snapshot = admin.snapshot().await.unwrap();
    assert_eq!(snapshot.clients.len(), 2);
    assert_eq!(snapshot.programs.len(), 2);
    let (resumed, resumed_credential) = open(&url, &identity).await;
    assert_eq!(a.client_id, resumed.client_id);
    assert_eq!(a.session_id, resumed.session_id);
    let mut recovered = bound(&url, &resumed_credential, None).await;
    let index = inventory(&mut recovered).await;
    assert_eq!(index["programs"][0]["programId"], aid);
    assert_eq!(index["programs"].as_array().unwrap().len(), 1);
    let mut existing = bound(&url, &ac, None).await;
    assert_eq!(inventory(&mut existing).await["clientId"], a.client_id);
    let mut forged = identity.clone();
    forged.secret = "0".repeat(64);
    let mut attacker = DefaultConnector.connect(&url).await.unwrap();
    assert!(matches!(
        exchange(
            &mut attacker,
            &SessionMessage::OpenClient {
                client: ClientHello::default(),
                credential: forged
            }
        )
        .await,
        SessionMessage::Error { .. }
    ));
    close(&url, resumed_credential).await;
    assert!(call(&mut blink, json!({"mode":"list"}))
        .await
        .result
        .unwrap()
        .to_string()
        .contains(&bid));
    close(&url, bc).await;
}
#[tokio::test]
async fn unbound_info_cannot_be_used_as_a_business_or_global_management_bypass() {
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor("tcp://127.0.0.1:0", Executor::local("anonymous"), manager)
        .await
        .unwrap();
    let mut link = DefaultConnector.connect(&url).await.unwrap();
    link.send(Frame::Text(
        json!({"id":1,"tool":"_executor_info","params":{}}).to_string(),
    ))
    .await
    .unwrap();
    let Some(Frame::Text(text)) = link.recv().await.unwrap() else {
        panic!("info reply");
    };
    assert!(serde_json::from_str::<ExecutorResponse>(&text).unwrap().ok);
    assert!(!call(&mut link, json!({"mode":"list"})).await.ok);
    for message in [
        serde_json::to_string(&AdminText::List).unwrap(),
        serde_json::to_string(&StreamMessage::Open {
            target: StreamTarget {
                kind: "tcp".into(),
                address: "127.0.0.1:1".into(),
            },
        })
        .unwrap(),
        serde_json::to_string(&FileMessage::Open {
            request: FileRequest::Download {
                path: "anything".into(),
                directory: None,
                offset: 0,
            },
        })
        .unwrap(),
    ] {
        let mut link = DefaultConnector.connect(&url).await.unwrap();
        link.send(Frame::Text(message)).await.unwrap();
        let Some(Frame::Text(text)) = link.recv().await.unwrap() else {
            panic!("missing denial");
        };
        assert!(text.contains("client session required"), "{text}");
    }
}
#[tokio::test]
async fn timeout_stops_tasks_and_all_data_channels_even_with_live_links() {
    let root = tempfile::tempdir().unwrap();
    let upload = root.path().join("upload");
    std::fs::write(&upload, b"original").unwrap();
    let download = root.path().join("large");
    std::fs::File::create(&download)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    let manager = ShellManager::default_shell(80, 24);
    let executor = Executor::local("expiry")
        .with_client_disconnect_timeout(Duration::from_millis(1200))
        .unwrap();
    let url = start_executor("tcp://127.0.0.1:0", executor.clone(), manager.clone())
        .await
        .unwrap();
    let admin_guard = start_management_listener("tcp://127.0.0.1:0", executor, manager).unwrap();
    let mut admin = ManagementClient::connect(admin_guard.address())
        .await
        .unwrap();
    let identity = new_client_credential();
    let (a, ac) = open(&url, &identity).await;
    let (_, bc) = open(&url, &new_client_credential()).await;
    let mut persistent = bound(&url, &ac, None).await;
    let aid = start(&mut persistent).await;
    let mut healthy = bound(&url, &bc, None).await;
    let bid = start(&mut healthy).await;
    let mut pty = bound(&url, &ac, None).await;
    pty.send(Frame::Text(
        serde_json::to_string(&ClientText::Hello {
            id: "own".into(),
            pty: aid.clone(),
            cols: 80,
            rows: 24,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    // Wait for registration before expiry.
    timeout(Duration::from_secs(3),async { loop { if matches!(pty.recv().await.unwrap(),Some(Frame::Text(text)) if matches!(serde_json::from_str::<ServerText>(&text),Ok(ServerText::Meta {..}))) {break;} } }).await.unwrap();
    let labels = ChannelMetadata {
        title: "中".repeat(40),
        description: "文".repeat(200),
    };
    let mut file = bound(&url, &ac, Some(labels.clone())).await;
    file.send(Frame::Text(
        serde_json::to_string(&FileMessage::Open {
            request: FileRequest::Upload {
                path: upload.clone(),
                directory: None,
                size: 100,
                overwrite: true,
                sha256: None,
            },
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let Some(Frame::Text(ready)) = file.recv().await.unwrap() else {
        panic!("file ready");
    };
    assert!(matches!(
        serde_json::from_str(&ready).unwrap(),
        FileMessage::Ready { .. }
    ));
    file.send(Frame::Binary(b"partial".to_vec())).await.unwrap();
    let mut downloading = bound(&url, &ac, None).await;
    downloading
        .send(Frame::Text(
            serde_json::to_string(&FileMessage::Open {
                request: FileRequest::Download {
                    path: download,
                    directory: None,
                    offset: 0,
                },
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    let Some(Frame::Text(_)) = downloading.recv().await.unwrap() else {
        panic!("download ready");
    };
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut forwarding = bound(
        &url,
        &ac,
        Some(ChannelMetadata {
            title: "数据库".into(),
            description: "转发说明".into(),
        }),
    )
    .await;
    forwarding
        .send(Frame::Text(
            serde_json::to_string(&StreamMessage::Open {
                target: StreamTarget {
                    kind: "tcp".into(),
                    address: target.local_addr().unwrap().to_string(),
                },
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    let (mut remote, _) = target.accept().await.unwrap();
    assert!(matches!(
        forwarding.recv().await.unwrap(),
        Some(Frame::Text(_))
    ));
    let index = inventory(&mut persistent).await;
    assert_eq!(index["channels"].as_array().unwrap().len(), 4);
    assert!(index["channels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["metadata"]["title"] == labels.title));
    let snapshot = timeout(Duration::from_secs(6), async {
        loop {
            let mut ping = DefaultConnector.connect(&url).await.unwrap();
            assert!(matches!(
                exchange(
                    &mut ping,
                    &SessionMessage::Ping {
                        credential: bc.clone()
                    }
                )
                .await,
                SessionMessage::Ok
            ));
            let s = admin.snapshot().await.unwrap();
            if s.clients.len() == 1
                && s.channels.is_empty()
                && !s.programs.iter().any(|p| p.program_id == aid)
            {
                break s;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert!(snapshot
        .programs
        .iter()
        .any(|p| p.program_id == bid && p.state == "running"));
    assert!(snapshot
        .programs
        .iter()
        .filter(|p| p.program_id == aid)
        .all(|p| p.attached_clients.is_empty()));
    assert_eq!(std::fs::read(&upload).unwrap(), b"original");
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        2,
        "staging file leaked"
    );
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(2), remote.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(timeout(Duration::from_secs(2), persistent.recv())
        .await
        .unwrap()
        .unwrap()
        .is_none());
    let mut expired = DefaultConnector.connect(&url).await.unwrap();
    assert!(matches!(
        exchange(
            &mut expired,
            &SessionMessage::Bind {
                credential: ac,
                metadata: None
            }
        )
        .await,
        SessionMessage::Error { .. }
    ));
    // The durable identity can open a new session, but the old running resources are gone.
    let (resumed, credential) = open(&url, &identity).await;
    assert_eq!(resumed.client_id, a.client_id);
    close(&url, credential).await;
    close(&url, bc).await;
}
#[tokio::test]
async fn channel_metadata_limits_count_unicode_characters() {
    assert!(ChannelMetadata::default().validate().is_ok());
    assert!(ChannelMetadata {
        title: "中".repeat(40),
        description: "文".repeat(200)
    }
    .validate()
    .is_ok());
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor("tcp://127.0.0.1:0", Executor::local("labels"), manager)
        .await
        .unwrap();
    let (_, credential) = open(&url, &new_client_credential()).await;
    for metadata in [
        ChannelMetadata {
            title: "中".repeat(41),
            description: String::new(),
        },
        ChannelMetadata {
            title: "ok".into(),
            description: "文".repeat(201),
        },
    ] {
        let mut link = DefaultConnector.connect(&url).await.unwrap();
        assert!(matches!(
            exchange(
                &mut link,
                &SessionMessage::Bind {
                    credential: credential.clone(),
                    metadata: Some(metadata)
                }
            )
            .await,
            SessionMessage::Error { .. }
        ));
    }
    close(&url, credential).await;
}

#[tokio::test]
async fn task_metadata_survives_both_io_lifecycles_without_expanding_client_inventory() {
    let manager = ShellManager::default_shell(80, 24);
    let executor = Executor::local("task-metadata");
    let url = start_executor("tcp://127.0.0.1:0", executor.clone(), manager.clone())
        .await
        .unwrap();
    let admin_guard = start_management_listener("tcp://127.0.0.1:0", executor, manager).unwrap();
    let mut admin = ManagementClient::connect(admin_guard.address())
        .await
        .unwrap();
    let (_, a) = open(&url, &new_client_credential()).await;
    let (_, b) = open(&url, &new_client_credential()).await;
    let mut own = bound(&url, &a, None).await;
    let mut other = bound(&url, &b, None).await;
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -Command 'Start-Sleep -Seconds 30'"
    } else {
        "sleep 30"
    };
    let title = "中🦀".repeat(20);
    let description = "文🦀".repeat(100);
    for io in ["stdio", "pty"] {
        let response = call(&mut own, json!({"mode":"run","io":io,"command":command,"read_timeout":0,"title":title,"description":description})).await;
        assert!(response.ok, "{response:?}");
        let task = response.result.unwrap()["metadata"].clone();
        let id = task["asyncID"].as_str().unwrap();
        assert_eq!(task["title"], title);
        assert_eq!(task["description"], description);
        let listed = call(&mut own, json!({"mode":"list"})).await.result.unwrap();
        assert!(listed["metadata"]["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["asyncID"] == id && t["title"] == title && t["description"] == description));
        assert_eq!(inventory(&mut other).await["programs"], json!([]));
        for mode in ["status", "stop", "remove"] {
            assert!(
                !call(&mut other, json!({"mode":mode,"asyncID":id})).await.ok,
                "foreign {mode}"
            );
        }
        let foreign_list = call(&mut other, json!({"mode":"list","asyncID":id})).await;
        if foreign_list.ok {
            assert_eq!(foreign_list.result.unwrap()["metadata"]["runs"], json!([]));
        }
        let index = inventory(&mut own).await;
        assert!(index["programs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["programId"] == id
                && p["title"] == title
                && p["description"] == description));
        let snapshot = admin.snapshot().await.unwrap();
        let program = snapshot
            .programs
            .iter()
            .find(|p| p.program_id == id)
            .unwrap();
        assert_eq!(program.title, title);
        assert_eq!(program.description, description);
        if io == "pty" {
            let viewed = call(
                &mut other,
                json!({"mode":"attach","asyncID":id,"read_timeout":0}),
            )
            .await;
            assert!(viewed.ok, "{viewed:?}");
            assert_eq!(viewed.result.unwrap()["metadata"]["title"], title);
        } else {
            assert!(
                !call(
                    &mut other,
                    json!({"mode":"attach","asyncID":id,"read_timeout":0})
                )
                .await
                .ok
            );
        }
        for mode in ["status", "attach", "stop", "attach"] {
            let response = call(&mut own, json!({"mode":mode,"asyncID":id,"read_timeout":0})).await;
            assert!(response.ok, "{response:?}");
            let metadata = &response.result.unwrap()["metadata"];
            assert_eq!(metadata["title"], title, "{mode}: {metadata}");
            assert_eq!(metadata["description"], description, "{mode}: {metadata}");
        }
        assert!(
            call(&mut own, json!({"mode":"remove","asyncID":id}))
                .await
                .ok
        );
        assert!(
            !call(&mut own, json!({"mode":"status","asyncID":id}))
                .await
                .ok
        );
        assert_eq!(inventory(&mut own).await["programs"], json!([]));
    }
    for io in ["stdio", "pty"] {
        for labels in [
            json!({}),
            json!({"title":" \t","description":""}),
            json!({"title":"短任务","description":"完成结果"}),
        ] {
            let mut params =
                json!({"mode":"shell","io":io,"command":"echo done","read_timeout":10000});
            params
                .as_object_mut()
                .unwrap()
                .extend(labels.as_object().unwrap().clone());
            let response = call(&mut own, params).await;
            assert!(response.ok, "{response:?}");
            let result = response.result.unwrap();
            assert_eq!(result["metadata"]["state"], "stopped");
            assert_eq!(
                result["metadata"]["title"],
                if labels["title"] == "短任务" {
                    "短任务"
                } else {
                    "未命名"
                }
            );
            assert_eq!(
                result["metadata"]["description"],
                labels["description"].as_str().unwrap_or_default()
            );
        }
        for invalid in [
            json!({"title":"中".repeat(41)}),
            json!({"title":" ".repeat(41)}),
            json!({"description":"文".repeat(201)}),
        ] {
            let mut params = json!({"mode":"run","io":io,"command":command,"read_timeout":0});
            params
                .as_object_mut()
                .unwrap()
                .extend(invalid.as_object().unwrap().clone());
            let response = call(&mut own, params).await;
            assert!(!response.ok, "{response:?}");
            assert!(response.error.unwrap().contains("40 characters"));
        }
        assert_eq!(inventory(&mut own).await["programs"], json!([]));
    }
    close(&url, a).await;
    close(&url, b).await;
}
