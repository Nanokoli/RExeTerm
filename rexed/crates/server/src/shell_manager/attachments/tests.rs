#![cfg(unix)]
use super::*;
use remote_executor_terminal::CommandSpec;
use std::time::Duration;

fn attach(manager: &ShellManager, id: &str, token: u64) -> TerminalAttachment {
    let (tx, _rx) = mpsc::unbounded_channel();
    manager
        .register_client("main", id.into(), token, 80, 24, tx, "test".into(), None)
        .unwrap()
}

#[tokio::test]
async fn only_current_controller_writes_and_stale_attachment_cannot_regain_control() {
    let root = tempfile::tempdir().unwrap();
    let received = root.path().join("received");
    let command = format!(
        "read line; printf '%s' \"$line\" > '{}'; sleep 30",
        received.display()
    );
    let manager = ShellManager::default_shell(80, 24);
    let session = manager
        .create_pty(
            "main",
            CommandSpec::new("sh").args(["-c", &command]),
            None,
            None,
        )
        .unwrap();
    let first = attach(&manager, "view", 1);
    let second = attach(&manager, "view", 2);
    assert_ne!(first.id, second.id);
    second.request_control().unwrap();
    first.write(b"must-not-arrive\n").unwrap();
    second.write(b"accepted\n").unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while std::fs::read_to_string(&received).unwrap_or_default() != "accepted" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    manager.remove_client("main", &second.id, 2);
    assert_eq!(
        manager.controller_id("main").as_deref(),
        Some(first.id.as_str())
    );
    let replacement = attach(&manager, &second.id, 3);
    replacement.request_control().unwrap();
    replacement.resize(110, 32).unwrap();
    assert!(second.request_control().is_err());
    assert!(second.resize(5, 5).is_err());
    manager.remove_client("main", &second.id, 2);
    assert_eq!(
        manager.controller_id("main").as_deref(),
        Some(replacement.id.as_str())
    );
    assert_eq!(session.summary().cols, 110);
    manager.close_pty_clients("main");
    assert!(replacement.request_control().is_err());
    assert!(manager.controller_id("main").is_none());
    let fresh = attach(&manager, &first.id, 4);
    manager.remove_client("main", &first.id, 1);
    assert!(first.request_control().is_err());
    fresh.request_control().unwrap();
    assert_eq!(manager.client_details("main").len(), 1);
    assert_eq!(
        manager.controller_id("main").as_deref(),
        Some(fresh.id.as_str())
    );
    manager.close_pty_clients("main");
    session.kill().unwrap();
}

#[tokio::test]
async fn terminal_and_exbash_claims_share_the_same_lock_and_controller() {
    let manager = ShellManager::default_shell(80, 24);
    let session = manager
        .create_pty("main", CommandSpec::new("cat"), None, None)
        .unwrap();
    let root = attach(&manager, "0", 1);
    let collision = attach(&manager, "0", 2);
    assert_eq!(root.id, "0");
    assert_ne!(collision.id, "0");
    manager.lock_pty("main").unwrap();
    assert!(collision.request_control().is_err());
    assert!(manager
        .claim_and_write("main", "rec:main", None, b"denied\n")
        .is_err());
    assert_eq!(manager.controller_id("main").as_deref(), Some("0"));
    root.request_control().unwrap();
    manager.unlock_pty("main");
    manager
        .claim_and_write("main", "rec:main", None, b"accepted\n")
        .unwrap();
    assert_eq!(manager.controller_id("main").as_deref(), Some("rec:main"));
    collision.request_control().unwrap();
    assert_eq!(
        manager.controller_id("main").as_deref(),
        Some(collision.id.as_str())
    );
    manager.lock_pty("main").unwrap();
    manager.close_pty_clients("main");
    let fresh = attach(&manager, "viewer", 3);
    assert!(manager.is_locked("main"));
    assert!(fresh.request_control().is_err());
    assert!(manager
        .claim_and_write("main", "rec:main", None, b"denied\n")
        .is_err());
    manager.remove_pty("main");
    assert!(fresh.request_control().is_err());
    session.kill().unwrap();
}
