use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, SanType};
use remote_executor::{
    client::{
        auth::{AuthenticatedConnector, AuthenticationOptions},
        management::ManagementClient,
        transport::{Connector, Frame, TlsClientOptions, TlsConnector},
        Client, ConnectExecutorOptions, FileTransferOptions,
    },
    protocol::{
        auth::AuthMessage,
        management::{ClientHello, SessionMessage},
        ExecutorRequest,
    },
    server::{
        auth::{hash_password, Authentication, ConfigDirectory, Permissions, UserConfig},
        start_executor, Executor, ShellManager,
    },
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::time::timeout;

struct Fixture {
    _root: tempfile::TempDir,
    directory: ConfigDirectory,
    executor: Executor,
    manager: ShellManager,
}
fn client_certificate(
    root: &Path,
    name: &str,
    identity: &str,
    issuer: Option<(&rcgen::Certificate, &KeyPair)>,
) -> PathBuf {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ClientAuth);
    params
        .subject_alt_names
        .push(SanType::URI(identity.try_into().unwrap()));
    let key = KeyPair::generate().unwrap();
    let cert = match issuer {
        Some((cert, issuer_key)) => params.signed_by(&key, cert, issuer_key).unwrap(),
        None => params.self_signed(&key).unwrap(),
    };
    let path = root.join(format!("{name}.pem"));
    std::fs::write(&path, cert.pem()).unwrap();
    std::fs::write(root.join(format!("{name}-key.pem")), key.serialize_pem()).unwrap();
    path
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let directory = ConfigDirectory::open(Some(root.path().join("config"))).unwrap();
    directory.initialize(Vec::new()).unwrap();
    directory
        .save_user(
            "alice",
            &UserConfig {
                password_hash: Some(hash_password("alice-secret").unwrap()),
                permissions: Permissions::all(),
                ..Default::default()
            },
        )
        .unwrap();
    directory
        .save_user(
            "bob",
            &UserConfig {
                password_hash: Some(hash_password("bob-secret").unwrap()),
                permissions: Permissions {
                    files_read: true,
                    pty_view: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    directory
        .save_user(
            "viewer",
            &UserConfig {
                password_hash: Some(hash_password("viewer-secret").unwrap()),
                permissions: Permissions {
                    execute: true,
                    pty_view: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    let cert = client_certificate(root.path(), "pinned", "urn:rexed:user:pinned", None);
    directory
        .save_user(
            "pinned",
            &UserConfig {
                certificates: vec![cert],
                permissions: Permissions::all(),
                ..Default::default()
            },
        )
        .unwrap();
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["test-ca".into()]).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = params.self_signed(&key).unwrap();
    let ca_path = root.path().join("ca.pem");
    std::fs::write(&ca_path, ca.pem()).unwrap();
    client_certificate(
        root.path(),
        "ca-user",
        "urn:rexed:user:carol",
        Some((&ca, &key)),
    );
    directory
        .save_user(
            "carol",
            &UserConfig {
                certificate_authorities: vec![ca_path.clone()],
                permissions: Permissions::all(),
                ..Default::default()
            },
        )
        .unwrap();
    directory
        .save_user(
            "dave",
            &UserConfig {
                certificate_authorities: vec![ca_path],
                permissions: Permissions::all(),
                ..Default::default()
            },
        )
        .unwrap();
    let executor =
        Executor::local("auth-test").with_authentication(Authentication::load(&directory).unwrap());
    Fixture {
        _root: root,
        directory,
        executor,
        manager: ShellManager::default_shell(80, 24),
    }
}
fn tls(f: &Fixture, certificate: Option<&str>) -> TlsClientOptions {
    TlsClientOptions {
        ca_file: Some(f.directory.path().join("server-cert.pem")),
        certificate_file: certificate.map(|name| f._root.path().join(format!("{name}.pem"))),
        private_key_file: certificate.map(|name| f._root.path().join(format!("{name}-key.pem"))),
        server_name: None,
    }
}
fn options(
    f: &Fixture,
    url: &str,
    username: &str,
    auth: Value,
    certificate: Option<&str>,
) -> ConnectExecutorOptions {
    let mut options: ConnectExecutorOptions = serde_json::from_value(
        json!({"id":"node","url":url,"username":username,"authentication":auth}),
    )
    .unwrap();
    options.tls = Some(tls(f, certificate));
    options
}
async fn call(
    client: &Client,
    tool: &str,
    params: Value,
) -> remote_executor::protocol::ExecutorResponse {
    client
        .handle(ExecutorRequest {
            id: json!(1),
            method: tool.into(),
            params,
            executor: None,
            directory: None,
            tool_timeout_ms: None,
        })
        .await
}
async fn text(link: &mut remote_executor::client::transport::Link) -> String {
    match timeout(Duration::from_secs(5), link.recv())
        .await
        .unwrap()
        .unwrap()
    {
        Some(Frame::Text(s)) => s,
        other => panic!("unexpected {other:?}"),
    }
}
#[tokio::test]
async fn tls_and_wss_authenticate_password_pin_and_ca_with_strict_identity() {
    let f = fixture();
    for scheme in ["tls", "wss"] {
        let url = start_executor(
            format!("{scheme}://127.0.0.1:0"),
            f.executor.clone(),
            f.manager.clone(),
        )
        .await
        .unwrap();
        for (username, auth, certificate) in [
            (
                "alice",
                json!({"method":"password","password":"alice-secret"}),
                None,
            ),
            ("pinned", json!({"method":"certificate"}), Some("pinned")),
            ("carol", json!({"method":"ca"}), Some("ca-user")),
        ] {
            let client = Client::default();
            client
                .connect_to_executor(options(&f, &url, username, auth, certificate))
                .await
                .unwrap();
            assert!(
                call(&client, "stat", json!({"filePath":f._root.path()}))
                    .await
                    .ok
            );
            // A second physical lane uses the authenticated ticket, not anonymous info discovery.
            assert!(call(&client, "client_resources", json!({})).await.ok);
            client.shutdown().await;
        }
        for (username, auth, certificate) in [
            (
                "alice",
                json!({"method":"password","password":"wrong"}),
                None,
            ),
            (
                "missing",
                json!({"method":"password","password":"alice-secret"}),
                None,
            ),
            ("bob", json!({"method":"certificate"}), Some("pinned")),
            ("dave", json!({"method":"ca"}), Some("ca-user")),
        ] {
            assert!(Client::default()
                .connect_to_executor(options(&f, &url, username, auth, certificate))
                .await
                .is_err());
        }
        let mut wrong_name = options(
            &f,
            &url,
            "alice",
            json!({"method":"password","password":"alice-secret"}),
            None,
        );
        wrong_name.tls.as_mut().unwrap().server_name = Some("wrong.invalid".into());
        assert!(Client::default()
            .connect_to_executor(wrong_name)
            .await
            .is_err());
        let mut untrusted = options(
            &f,
            &url,
            "alice",
            json!({"method":"password","password":"alice-secret"}),
            None,
        );
        untrusted.tls = None;
        assert!(Client::default()
            .connect_to_executor(untrusted)
            .await
            .is_err());
    }
}
#[tokio::test]
async fn every_link_requires_login_and_bound_session_username_cannot_be_spoofed() {
    let f = fixture();
    let url = start_executor("tls://127.0.0.1:0", f.executor.clone(), f.manager.clone())
        .await
        .unwrap();
    let raw: Arc<dyn Connector> = Arc::new(TlsConnector::new(tls(&f, None)).unwrap());
    for business in [
        json!({"tool":"_executor_info","id":1}),
        json!({"type":"re.session.v1.bind","credential":{"sessionId":"fake","secret":"fake"}}),
        json!({"type":"hello","id":"x","pty":"known","cols":80,"rows":24}),
        json!({"type":"re.file.v2.open"}),
        json!({"type":"re.stream.v1.open"}),
    ] {
        let mut link = raw.connect(&url).await.unwrap();
        link.send(Frame::Text(business.to_string())).await.unwrap();
        assert!(matches!(
            serde_json::from_str::<AuthMessage>(&text(&mut link).await).unwrap(),
            AuthMessage::Error { .. }
        ));
    }
    let alice = AuthenticatedConnector::new(
        raw.clone(),
        "alice".into(),
        AuthenticationOptions::Password {
            password: "alice-secret".into(),
        },
    );
    let mut link = alice.connect(&url).await.unwrap();
    link.send(Frame::Text(
        serde_json::to_string(&SessionMessage::Open {
            client: ClientHello {
                username: "alice".into(),
                client_name: "raw".into(),
                client_version: "test".into(),
            },
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let SessionMessage::Opened { credential, .. } =
        serde_json::from_str(&text(&mut link).await).unwrap()
    else {
        panic!("session not opened");
    };
    let bob = AuthenticatedConnector::new(
        raw.clone(),
        "bob".into(),
        AuthenticationOptions::Password {
            password: "bob-secret".into(),
        },
    );
    for message in [
        SessionMessage::Bind {
            credential: credential.clone(),
            metadata: None,
        },
        SessionMessage::Ping {
            credential: credential.clone(),
        },
        SessionMessage::Close {
            credential: credential.clone(),
        },
    ] {
        let mut link = bob.connect(&url).await.unwrap();
        link.send(Frame::Text(serde_json::to_string(&message).unwrap()))
            .await
            .unwrap();
        assert!(!matches!(
            timeout(Duration::from_secs(5), link.recv()).await.unwrap(),
            Ok(Some(Frame::Text(_)))
        ));
    }
    // Bob's failed Close must not close Alice's session.
    let mut link = alice.connect(&url).await.unwrap();
    link.send(Frame::Text(
        serde_json::to_string(&SessionMessage::Ping { credential }).unwrap(),
    ))
    .await
    .unwrap();
    assert!(matches!(
        serde_json::from_str::<SessionMessage>(&text(&mut link).await).unwrap(),
        SessionMessage::Ok
    ));
    let admin = remote_executor::server::management::start_management_listener(
        "tls://127.0.0.1:0",
        f.executor.clone(),
        f.manager.clone(),
    )
    .unwrap();
    let mut denied = ManagementClient::with_connector(Arc::new(bob), admin.address())
        .await
        .unwrap();
    let error = denied.snapshot().await.unwrap_err();
    assert!(
        error.to_string().contains("management permission denied"),
        "{error:#}"
    );
    let mut allowed = ManagementClient::with_connector(Arc::new(alice), admin.address())
        .await
        .unwrap();
    assert!(!allowed.snapshot().await.unwrap().clients.is_empty());
}
#[tokio::test]
async fn permissions_gate_tools_file_and_forward_data_paths() {
    let f = fixture();
    let url = start_executor("tls://127.0.0.1:0", f.executor.clone(), f.manager.clone())
        .await
        .unwrap();
    let client = Client::default();
    client
        .connect_to_executor(options(
            &f,
            &url,
            "bob",
            json!({"method":"password","password":"bob-secret"}),
            None,
        ))
        .await
        .unwrap();
    assert!(
        !call(
            &client,
            "exbash",
            json!({"mode":"run","command":"not-allowed"})
        )
        .await
        .ok
    );
    assert!(
        !call(
            &client,
            "FileAction",
            json!({"mode":"delete","filePath":f._root.path().join("must-exist")})
        )
        .await
        .ok
    );
    let source = f._root.path().join("source");
    std::fs::write(&source, b"permission-check").unwrap();
    for (mode, succeeds) in [("download", true), ("upload", false)] {
        let local = if succeeds {
            f._root.path().join("download")
        } else {
            source.clone()
        };
        let options: FileTransferOptions = serde_json::from_value(
            json!({"mode":mode,"localPath":local,"targetPath":source,"overwrite":false}),
        )
        .unwrap();
        assert_eq!(
            client
                .transfer_file(None, options, None, None)
                .await
                .is_ok(),
            succeeds
        );
    }
    assert_eq!(
        std::fs::read(f._root.path().join("download")).unwrap(),
        b"permission-check"
    );
    let connector = AuthenticatedConnector::new(
        Arc::new(TlsConnector::new(tls(&f, None)).unwrap()),
        "bob".into(),
        AuthenticationOptions::Password {
            password: "bob-secret".into(),
        },
    );
    let mut link = connector.connect(&url).await.unwrap();
    link.send(Frame::Text(
        serde_json::to_string(&SessionMessage::Open {
            client: ClientHello {
                username: "bob".into(),
                client_name: "raw".into(),
                client_version: "test".into(),
            },
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let SessionMessage::Opened { credential, .. } =
        serde_json::from_str(&text(&mut link).await).unwrap()
    else {
        panic!("session not opened")
    };
    let mut link = connector.connect(&url).await.unwrap();
    link.send(Frame::Text(
        serde_json::to_string(&SessionMessage::Bind {
            credential,
            metadata: None,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    assert!(matches!(
        serde_json::from_str::<SessionMessage>(&text(&mut link).await).unwrap(),
        SessionMessage::Ok
    ));
    link.send(Frame::Text(
        json!({"type":"re.stream.v1.open","target":{"kind":"tcp","address":"127.0.0.1:9"}})
            .to_string(),
    ))
    .await
    .unwrap();
    assert!(text(&mut link).await.contains("permission denied"));
    client.shutdown().await;
}
#[tokio::test]
async fn a_user_without_pty_control_can_only_view_even_its_own_task() {
    use remote_executor::protocol::terminal::{ClientText, ServerText};
    let f = fixture();
    let url = start_executor("tls://127.0.0.1:0", f.executor.clone(), f.manager.clone())
        .await
        .unwrap();
    let client = Client::default();
    client
        .connect_to_executor(options(
            &f,
            &url,
            "viewer",
            json!({"method":"password","password":"viewer-secret"}),
            None,
        ))
        .await
        .unwrap();
    let command = if cfg!(windows) {
        "Start-Sleep -Seconds 30"
    } else {
        "sleep 30"
    };
    let response = call(
        &client,
        "exbash",
        json!({"mode":"shell","command":command,"read_timeout":0}),
    )
    .await;
    assert!(response.ok, "{response:?}");
    let result = response.result.unwrap();
    let id = result["metadata"]["asyncID"].as_str().unwrap();
    let mut terminal = client
        .open_terminal(None, id, "read-only-owner", 80, 24)
        .await
        .unwrap();
    let meta: ServerText = serde_json::from_str(&text(terminal.link()).await).unwrap();
    assert!(matches!(meta,ServerText::Meta {read_only:true,role,..} if role=="Viewer"));
    for frame in [
        Frame::Binary(b"denied\n".to_vec()),
        Frame::Text(serde_json::to_string(&ClientText::RequestControl).unwrap()),
        Frame::Text(
            serde_json::to_string(&ClientText::Resize {
                cols: 120,
                rows: 40,
            })
            .unwrap(),
        ),
    ] {
        terminal.link().send(frame).await.unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerText>(&text(terminal.link()).await).unwrap(),
            ServerText::Error { .. }
        ));
    }
    assert!(f.manager.detail(id).unwrap().controller.is_none());
    client.shutdown().await;
}
#[test]
fn private_config_uses_safe_user_names_and_hashed_passwords() {
    let f = fixture();
    for name in ["../escape", "a/b", "", "CON", "LPT1", "a.b"] {
        assert!(f.directory.user_directory(name).is_err());
    }
    let config =
        std::fs::read_to_string(f.directory.path().join("users/alice/config.json")).unwrap();
    assert!(config.contains("$argon2id$") && !config.contains("alice-secret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f.directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(f.directory.path().join("server-key.pem"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn users_created_after_start_are_loaded_for_password_pin_and_ca_without_restarting() {
    let root = tempfile::tempdir().unwrap();
    let directory = ConfigDirectory::open(Some(root.path().join("config"))).unwrap();
    directory.initialize(Vec::new()).unwrap();
    let broken = directory.user_directory("broken").unwrap();
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("config.json"), b"not valid json").unwrap();
    // Unrelated malformed users do not prevent startup: users are not scanned.
    let executor =
        Executor::local("lazy-auth").with_authentication(Authentication::load(&directory).unwrap());
    let f = Fixture {
        _root: root,
        directory,
        executor,
        manager: ShellManager::default_shell(80, 24),
    };
    let url = start_executor("tls://127.0.0.1:0", f.executor.clone(), f.manager.clone())
        .await
        .unwrap();
    f.directory
        .save_user(
            "latepass",
            &UserConfig {
                password_hash: Some(hash_password("late-secret").unwrap()),
                permissions: Permissions::all(),
                ..Default::default()
            },
        )
        .unwrap();
    let pinned = client_certificate(f._root.path(), "latepin", "urn:rexed:user:latepin", None);
    f.directory
        .save_user(
            "latepin",
            &UserConfig {
                certificates: vec![pinned],
                permissions: Permissions::all(),
                ..Default::default()
            },
        )
        .unwrap();
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["late-ca".into()]).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = params.self_signed(&key).unwrap();
    let ca_path = f._root.path().join("late-ca.pem");
    std::fs::write(&ca_path, ca.pem()).unwrap();
    client_certificate(
        f._root.path(),
        "lateca",
        "urn:rexed:user:lateca",
        Some((&ca, &key)),
    );
    f.directory
        .save_user(
            "lateca",
            &UserConfig {
                certificate_authorities: vec![ca_path],
                permissions: Permissions::all(),
                ..Default::default()
            },
        )
        .unwrap();
    // This valid leaf has the expected URI, but neither the pinned key nor trusted issuer.
    client_certificate(f._root.path(), "untrusted", "urn:rexed:user:lateca", None);
    for (name, auth, cert) in [
        (
            "latepass",
            json!({"method":"password","password":"late-secret"}),
            None,
        ),
        ("latepin", json!({"method":"certificate"}), Some("latepin")),
        ("lateca", json!({"method":"ca"}), Some("lateca")),
    ] {
        let client = Client::default();
        client
            .connect_to_executor(options(&f, &url, name, auth, cert))
            .await
            .unwrap();
        assert!(
            call(&client, "stat", json!({"filePath":f._root.path()}))
                .await
                .ok
        );
        client.shutdown().await;
    }
    for (name, auth, cert) in [
        (
            "latepin",
            json!({"method":"certificate"}),
            Some("untrusted"),
        ),
        ("latepin", json!({"method":"certificate"}), None),
        ("lateca", json!({"method":"ca"}), Some("untrusted")),
        (
            "broken",
            json!({"method":"password","password":"late-secret"}),
            None,
        ),
    ] {
        assert!(Client::default()
            .connect_to_executor(options(&f, &url, name, auth, cert))
            .await
            .is_err());
    }
}
