use rexeterm::{
    app::desktop::{dispatch, Desktop, Host, Workspace},
    server::{start_executor, Executor, ShellManager},
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[cfg(unix)]
fn terminal_data(packet: &[u8]) -> Vec<&[u8]> {
    assert_eq!(&packet[..4], b"RTP1");
    assert!(packet[4] <= 1);
    let mut offset = 5;
    let mut output = vec![];
    while offset < packet.len() {
        let kind = packet[offset];
        let length =
            u32::from_le_bytes(packet[offset + 1..offset + 5].try_into().unwrap()) as usize;
        let payload = &packet[offset + 5..offset + 5 + length];
        match kind {
            0 => output.push(payload),
            1 => {
                serde_json::from_slice::<Value>(payload).unwrap();
            }
            _ => panic!("unknown binary record"),
        }
        offset += 5 + length;
    }
    output
}

async fn setup(root: &std::path::Path) -> Arc<Desktop> {
    let url = start_executor(
        "tcp://127.0.0.1:0",
        Executor::local("desktop-test"),
        ShellManager::default_shell(80, 24),
    )
    .await
    .unwrap();
    let desktop = Desktop::new(
        root.join("config/workspace.json"),
        Workspace {
            version: 1,
            last_host: None,
            hosts: vec![Host {
                username: None,
                connection_profile: None,
                management: None,
                id: "test".into(),
                name: "测试 Host".into(),
                url,
                root: root.to_string_lossy().into_owned(),
            }],
        },
    )
    .await
    .unwrap();
    dispatch(&desktop, "connect", json!({"host":"test"}))
        .await
        .unwrap();
    desktop
}

async fn completed(desktop: &Arc<Desktop>, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let jobs = desktop.transfers();
            let job = jobs
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["id"] == id)
                .unwrap();
            if job["status"] != "running" {
                return job.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn desktop_browses_previews_searches_and_persists_real_hosts() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("中文目录")).unwrap();
    std::fs::write(
        root.path().join("hello.rs"),
        "// unique_desktop_token\nfn main() {}\n",
    )
    .unwrap();
    std::fs::write(root.path().join("binary.bin"), [0, 159, 255, 0]).unwrap();
    let desktop = setup(root.path()).await;
    let path = root.path().to_string_lossy();
    let listing = dispatch(&desktop, "browse", json!({"host":"test","path":path}))
        .await
        .unwrap();
    assert!(listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["name"] == "中文目录" && e["directory"] == true));
    let preview = dispatch(
        &desktop,
        "preview",
        json!({"host":"test","path":root.path().join("hello.rs")}),
    )
    .await
    .unwrap();
    assert!(preview["content"]
        .as_str()
        .unwrap()
        .contains("unique_desktop_token"));
    let binary = dispatch(
        &desktop,
        "preview",
        json!({"host":"test","path":root.path().join("binary.bin")}),
    )
    .await
    .unwrap();
    assert_eq!(binary["mode"], "binary");
    for (query, content) in [("hello", false), ("unique_desktop_token", true)] {
        let result = dispatch(
            &desktop,
            "search",
            json!({"host":"test","path":path,"query":query,"content":content}),
        )
        .await
        .unwrap();
        assert_eq!(result["entries"].as_array().unwrap().len(), 1, "{result}");
        assert!(result["entries"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("hello.rs"));
    }
    assert!(dispatch(
        &desktop,
        "search",
        json!({"host":"test","path":path,"query":"[","content":true})
    )
    .await
    .is_err());
    let saved = desktop.workspace().await.unwrap();
    let duplicate: Host = serde_json::from_value(saved["saved"]["hosts"][0].clone()).unwrap();
    assert!(desktop.save_host(duplicate).is_err());
    desktop.shutdown().await.unwrap();
    assert!(dispatch(&desktop, "workspace", json!({})).await.is_err());
    let restored = Desktop::new(
        root.path().join("config/workspace.json"),
        Workspace {
            version: 1,
            hosts: vec![],
            last_host: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(restored.workspace().await.unwrap()["saved"], saved["saved"]);
    restored.shutdown().await.unwrap();
}

#[tokio::test]
async fn desktop_transfers_real_bytes_and_tcp_forward_is_owned_by_app() {
    let root = tempfile::tempdir().unwrap();
    let desktop = setup(root.path()).await;
    let payload: Vec<u8> = (0..131_200).map(|n| (n % 251) as u8).collect();
    let source = root.path().join("source.bin");
    let remote = root.path().join("remote.bin");
    let copy = root.path().join("copy.bin");
    std::fs::write(&source, &payload).unwrap();
    for (mode, local) in [("upload", &source), ("download", &copy)] {
        let started = dispatch(
            &desktop,
            "start_transfer",
            json!({"host":"test","options":{"mode":mode,"localPath":local,"targetPath":remote}}),
        )
        .await
        .unwrap();
        let job = completed(&desktop, started["id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "done", "{job}");
        assert_eq!(job["bytes"], payload.len() as u64);
        assert!(job["rate"].as_f64().unwrap() > 0.0);
        assert!(job["durationMs"].is_u64());
    }
    assert_eq!(std::fs::read(copy).unwrap(), payload);
    let collision = dispatch(
        &desktop,
        "start_transfer",
        json!({"host":"test","options":{"mode":"upload","localPath":source,"targetPath":remote}}),
    )
    .await
    .unwrap();
    assert_eq!(
        completed(&desktop, collision["id"].as_str().unwrap()).await["status"],
        "error"
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let echo = tokio::spawn(async move {
        let (mut peer, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4];
        peer.read_exact(&mut bytes).await.unwrap();
        peer.write_all(&bytes).await.unwrap();
    });
    dispatch(
        &desktop,
        "start_forward",
        json!({"host":"test","listen":"127.0.0.1:0","target":address.to_string()}),
    )
    .await
    .unwrap();
    let forwards = dispatch(&desktop, "forwards", json!({})).await.unwrap();
    let forward = &forwards["metadata"]["forwards"][0];
    let mut peer = TcpStream::connect(
        forward["listen"]
            .as_str()
            .unwrap()
            .trim_start_matches("tcp://"),
    )
    .await
    .unwrap();
    peer.write_all(b"ping").await.unwrap();
    let mut received = [0; 4];
    peer.read_exact(&mut received).await.unwrap();
    assert_eq!(&received, b"ping");
    drop(peer);
    echo.await.unwrap();
    desktop.shutdown().await.unwrap();
    assert!(TcpStream::connect(
        forward["listen"]
            .as_str()
            .unwrap()
            .trim_start_matches("tcp://")
    )
    .await
    .is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_terminal_stream_handles_input_resize_and_detach_without_stopping_task() {
    let root = tempfile::tempdir().unwrap();
    let desktop = setup(root.path()).await;
    let run = dispatch(&desktop,"run",json!({"host":"test","path":root.path(),"title":"构建终端","description":"交互测试","command":"read line; printf 'PTY_ECHO:%s\\n' \"$line\"; sleep 30"})).await.unwrap();
    let pty = run["metadata"]["asyncID"].as_str().unwrap();
    assert_eq!(run["metadata"]["title"], "构建终端");
    assert_eq!(run["metadata"]["description"], "交互测试");
    let first = dispatch(&desktop, "terminal_open", json!({"host":"test","pty":pty}))
        .await
        .unwrap();
    let session = dispatch(
        &desktop,
        "terminal_open",
        json!({"host":"test","pty":pty,"cols":90,"rows":25}),
    )
    .await
    .unwrap();
    assert!(desktop
        .terminal_poll(first["id"].as_str().unwrap())
        .await
        .is_err());
    assert!(dispatch(
        &desktop,
        "terminal_input",
        json!({"id":first["id"],"data":"stale\r"})
    )
    .await
    .is_err());
    dispatch(&desktop, "terminal_close", json!({"id":first["id"]}))
        .await
        .unwrap();
    dispatch(
        &desktop,
        "terminal_resize",
        json!({"id":session["id"],"cols":110,"rows":32}),
    )
    .await
    .unwrap();
    dispatch(
        &desktop,
        "terminal_input",
        json!({"id":session["id"],"data":"preview_input\r"}),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut output = vec![];
        loop {
            let packet = desktop
                .terminal_poll(session["id"].as_str().unwrap())
                .await
                .unwrap();
            for data in terminal_data(&packet) {
                output.extend_from_slice(data);
            }
            if String::from_utf8_lossy(&output).contains("PTY_ECHO:preview_input") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    dispatch(&desktop, "terminal_close", json!({"id":session["id"]}))
        .await
        .unwrap();
    let tasks = dispatch(&desktop, "tasks", json!({"host":"test"}))
        .await
        .unwrap();
    assert!(tasks["metadata"]["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["asyncID"] == pty
            && r["state"] == "running"
            && r["title"] == "构建终端"
            && r["description"] == "交互测试"));
    dispatch(&desktop, "stop_task", json!({"host":"test","id":pty}))
        .await
        .unwrap();
    let tasks = dispatch(&desktop, "tasks", json!({"host":"test"}))
        .await
        .unwrap();
    assert!(
        tasks["metadata"]["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["asyncID"] == pty
                && t["state"] == "stopped"
                && t["title"] == "构建终端"
                && t["description"] == "交互测试"),
        "GUI history must come from the application after the remote task is retired"
    );
    desktop.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_input_progresses_while_preview_output_is_backpressured() {
    let root = tempfile::tempdir().unwrap();
    let desktop = setup(root.path()).await;
    let ready = root.path().join("burst-ready");
    let received = root.path().join("input-received");
    let command = format!(
        "read start; head -c 2097152 /dev/zero; printf ready > '{}'; read line; printf '%s' \"$line\" > '{}'; printf END_STREAM; sleep 30",
        ready.display(), received.display()
    );
    let run = dispatch(
        &desktop,
        "run",
        json!({"host":"test","path":root.path(),"command":command}),
    )
    .await
    .unwrap();
    let pty = run["metadata"]["asyncID"].as_str().unwrap();
    let session = dispatch(&desktop, "terminal_open", json!({"host":"test","pty":pty}))
        .await
        .unwrap();
    dispatch(
        &desktop,
        "terminal_input",
        json!({"id":session["id"],"data":"start\r"}),
    )
    .await
    .unwrap();
    // Do not poll: the bounded output queue must fill before sending input.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    dispatch(
        &desktop,
        "terminal_input",
        json!({"id":session["id"],"data":"responsive\r"}),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while std::fs::read_to_string(&received).unwrap_or_default() != "responsive" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("output backpressure stalled keyboard input");
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut zeros = 0;
        let mut tail = String::new();
        loop {
            let packet = desktop
                .terminal_poll(session["id"].as_str().unwrap())
                .await
                .unwrap();
            let mut bytes = 0;
            for data in terminal_data(&packet) {
                bytes += data.len();
                zeros += data.iter().filter(|&&b| b == 0).count();
                tail.push_str(&String::from_utf8_lossy(data));
                if tail.len() > 128 {
                    tail = tail[tail.len() - 128..].to_string();
                }
            }
            assert!(bytes <= 256 * 1024 + 32768, "unbounded IPC batch");
            if tail.contains("END_STREAM") {
                assert_eq!(zeros, 2097152);
                break;
            }
        }
    })
    .await
    .unwrap();
    dispatch(&desktop, "terminal_close", json!({"id":session["id"]}))
        .await
        .unwrap();
    dispatch(&desktop, "stop_task", json!({"host":"test","id":pty}))
        .await
        .unwrap();
    desktop.shutdown().await.unwrap();
}

#[tokio::test]
async fn tasks_are_scoped_to_this_client_and_shared_with_its_http_adapter() {
    use rexeterm::{app::McpHttpOptions, protocol::ExecutorRequest};
    #[path = "support/mcp_http.rs"]
    mod wire;
    let root = tempfile::tempdir().unwrap();
    let first = setup(root.path()).await;
    let saved: Workspace =
        serde_json::from_value(first.workspace().await.unwrap()["saved"].clone()).unwrap();
    let second = Desktop::new(root.path().join("other/workspace.json"), saved)
        .await
        .unwrap();
    second.connect("test").await.unwrap();
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -Command \"Start-Sleep -Seconds 30\""
    } else {
        "sleep 30"
    };
    let args = json!({"host":"test","path":root.path(),"command":command});
    let mine = dispatch(&first, "run", args.clone()).await.unwrap();
    let foreign = dispatch(&second, "run", args).await.unwrap();
    let clone = first.app_host().core().clone();
    let cloned = clone
        .handle(ExecutorRequest {
            id: json!(1),
            method: "exbash".into(),
            params: json!({"mode":"shell","command":command,"read_timeout":0}),
            directory: Some(root.path().into()),
            executor: Some("test".into()),
            tool_timeout_ms: None,
        })
        .await;
    assert!(cloned.ok, "{cloned:?}");
    let cloned_id = cloned.result.unwrap()["metadata"]["asyncID"].clone();
    let address = first
        .app_host()
        .enable_mcp_http(McpHttpOptions::default())
        .await
        .unwrap();
    let session = wire::init(address).await;
    let via_http = wire::tool(
        address,
        &session,
        "exbash",
        json!({"mode":"shell","command":command,"read_timeout":0}),
    )
    .await;
    let http_id = via_http["metadata"]["asyncID"].clone();
    assert!(http_id.is_string(), "{via_http}");
    let own = dispatch(&first, "tasks", json!({"host":"test"}))
        .await
        .unwrap();
    let rows = own["metadata"]["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{own}");
    for id in [&mine["metadata"]["asyncID"], &cloned_id, &http_id] {
        assert!(rows.iter().any(|row| &row["asyncID"] == id));
    }
    assert!(!own
        .to_string()
        .contains(foreign["metadata"]["asyncID"].as_str().unwrap()));
    let theirs = dispatch(&second, "tasks", json!({"host":"test"}))
        .await
        .unwrap();
    assert_eq!(theirs["metadata"]["runs"].as_array().unwrap().len(), 1);
    assert_eq!(
        theirs["metadata"]["runs"][0]["asyncID"],
        foreign["metadata"]["asyncID"]
    );
    for row in rows {
        dispatch(
            &first,
            "stop_task",
            json!({"host":"test","id":row["asyncID"]}),
        )
        .await
        .unwrap();
    }
    dispatch(
        &second,
        "stop_task",
        json!({"host":"test","id":foreign["metadata"]["asyncID"]}),
    )
    .await
    .unwrap();
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn management_is_independent_authenticated_global_and_reauthenticates_after_configuration() {
    use rexeterm::server::{
        auth::{hash_password, Authentication, ConfigDirectory, Permissions, UserConfig},
        management::start_management_listener,
    };
    let root = tempfile::tempdir().unwrap();
    let config = ConfigDirectory::open(Some(root.path().join("rexed"))).unwrap();
    config.initialize(Vec::new()).unwrap();
    for (username, permissions) in [
        (
            "viewer",
            Permissions {
                execute: true,
                pty_view: true,
                pty_control: true,
                ..Default::default()
            },
        ),
        ("admin", Permissions::all()),
    ] {
        config
            .save_user(
                username,
                &UserConfig {
                    password_hash: Some(hash_password(&format!("{username}-secret")).unwrap()),
                    permissions,
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let executor = Executor::local("gui-management-test")
        .with_authentication(Authentication::load(&config).unwrap());
    let manager = ShellManager::default_shell(80, 24);
    let url = start_executor("tls://127.0.0.1:0", executor.clone(), manager.clone())
        .await
        .unwrap();
    let listener = start_management_listener("wss://127.0.0.1:0/ops", executor, manager).unwrap();
    let mut profiles = vec![];
    for username in ["viewer", "admin"] {
        let path = root.path().join(format!("{username}.json"));
        std::fs::write(&path,serde_json::to_vec(&json!({"username":username,"authentication":{"method":"password","password":format!("{username}-secret")},"tls":{"caFile":config.path().join("server-cert.pem")}})).unwrap()).unwrap();
        profiles.push(path);
    }
    let host:Host=serde_json::from_value(json!({"id":"node","name":"Node","root":root.path(),"url":url,"username":"viewer","connectionProfile":profiles[0],"management":{"target":listener.address()}})).unwrap();
    let desktop = Desktop::new(
        root.path().join("first/workspace.json"),
        Workspace {
            version: 1,
            hosts: vec![host.clone()],
            last_host: None,
        },
    )
    .await
    .unwrap();
    let other = Desktop::new(
        root.path().join("second/workspace.json"),
        Workspace {
            version: 1,
            hosts: vec![host],
            last_host: None,
        },
    )
    .await
    .unwrap();
    // The operations endpoint does not require an active business connection.
    let denied = dispatch(&desktop, "management_snapshot", json!({"host":"node"}))
        .await
        .unwrap();
    assert_eq!(denied["status"], "denied", "{denied}");
    assert!(denied["snapshot"].is_null());
    desktop.connect("node").await.unwrap();
    other.connect("node").await.unwrap();
    let command = if cfg!(windows) {
        "powershell.exe -NoProfile -Command \"Start-Sleep -Seconds 30\""
    } else {
        "sleep 30"
    };
    let mine = dispatch(
        &desktop,
        "run",
        json!({"host":"node","path":root.path(),"command":command,"title":"mine"}),
    )
    .await
    .unwrap();
    let theirs = dispatch(
        &other,
        "run",
        json!({"host":"node","path":root.path(),"command":command,"title":"theirs"}),
    )
    .await
    .unwrap();
    let mine_id = &mine["metadata"]["asyncID"];
    let other_id = &theirs["metadata"]["asyncID"];
    assert!(
        mine_id.is_string() && other_id.is_string(),
        "{mine} {theirs}"
    );
    let terminal = dispatch(
        &desktop,
        "terminal_open",
        json!({"host":"node","pty":mine_id}),
    )
    .await
    .unwrap();
    let configured=dispatch(&desktop,"management_configure",json!({"host":"node","settings":{"target":listener.address(),"connectionProfile":profiles[1]}})).await.unwrap();
    assert_eq!(configured["runtime"]["default"], "node");
    let snapshot = dispatch(&desktop, "management_snapshot", json!({"host":"node"}))
        .await
        .unwrap();
    assert_eq!(snapshot["status"], "connected", "{snapshot}");
    assert_eq!(snapshot["snapshot"]["executorId"], "gui-management-test");
    assert_eq!(
        snapshot["snapshot"]["clients"].as_array().unwrap().len(),
        2,
        "management must not register a business client"
    );
    for id in [mine_id, other_id] {
        assert!(snapshot["snapshot"]["programs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| &p["programId"] == id));
    }
    assert!(snapshot["snapshot"]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["kind"] == "pty" && &c["target"] == mine_id));
    let own = dispatch(&desktop, "tasks", json!({"host":"node"}))
        .await
        .unwrap();
    assert_eq!(own["metadata"]["runs"].as_array().unwrap().len(), 1);
    assert_eq!(&own["metadata"]["runs"][0]["asyncID"], mine_id);
    // A separate Host can read a different server without changing the business default target.
    let second = start_management_listener(
        "tcp://127.0.0.1:0",
        Executor::local("second-ops"),
        ShellManager::default_shell(80, 24),
    )
    .unwrap();
    let second_host:Host=serde_json::from_value(json!({"id":"ops-only","name":"Ops only","root":"/","url":"tcp://127.0.0.1:1","management":{"target":second.address()}})).unwrap();
    desktop.save_host(second_host).unwrap();
    let second_snapshot = dispatch(&desktop, "management_snapshot", json!({"host":"ops-only"}))
        .await
        .unwrap();
    assert_eq!(second_snapshot["status"], "connected", "{second_snapshot}");
    assert_eq!(second_snapshot["snapshot"]["executorId"], "second-ops");
    assert_eq!(
        desktop.workspace().await.unwrap()["runtime"]["default"],
        "node"
    );
    // Switching away from admin credentials must discard the privileged connection.
    dispatch(
        &desktop,
        "management_configure",
        json!({"host":"node","settings":{"target":listener.address()}}),
    )
    .await
    .unwrap();
    let denied = dispatch(&desktop, "management_snapshot", json!({"host":"node"}))
        .await
        .unwrap();
    assert_eq!(denied["status"], "denied", "{denied}");
    assert!(denied["snapshot"].is_null() && !denied.to_string().contains("secret"));
    let tasks = dispatch(&desktop, "tasks", json!({"host":"node"}))
        .await
        .unwrap();
    assert!(tasks["metadata"]["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| &t["asyncID"] == mine_id && t["state"] == "running"));
    // Unavailable operations must not return the last global snapshot or disturb tasks.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let missing = format!("tcp://{}", closed.local_addr().unwrap());
    drop(closed);
    dispatch(
        &desktop,
        "management_configure",
        json!({"host":"node","settings":{"target":missing}}),
    )
    .await
    .unwrap();
    let unavailable = dispatch(&desktop, "management_snapshot", json!({"host":"node"}))
        .await
        .unwrap();
    assert_eq!(unavailable["status"], "unavailable", "{unavailable}");
    assert!(unavailable["snapshot"].is_null());
    dispatch(&desktop, "terminal_close", json!({"id":terminal["id"]}))
        .await
        .unwrap();
    dispatch(&desktop, "stop_task", json!({"host":"node","id":mine_id}))
        .await
        .unwrap();
    dispatch(&other, "stop_task", json!({"host":"node","id":other_id}))
        .await
        .unwrap();
    desktop.shutdown().await.unwrap();
    other.shutdown().await.unwrap();
    assert!(
        dispatch(&desktop, "management_snapshot", json!({"host":"ops-only"}))
            .await
            .is_err()
    );
}
