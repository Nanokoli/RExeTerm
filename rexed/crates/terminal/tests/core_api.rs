#[cfg(unix)]
#[test]
fn command_spec_sets_cwd_env_and_exposes_exit_code() {
    use remote_executor_terminal::session::CommandSpec;
    use remote_executor_terminal::PtyManager;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let cwd = std::env::temp_dir().join(format!("re-terminal-api-{suffix}"));
    fs::create_dir_all(&cwd).unwrap();

    let command = CommandSpec::new("sh")
        .args([
            "-lc",
            r#"test "$SHELL_MANAGER_TEST" = ok && test "$(pwd)" = "$EXPECTED_CWD"; exit 7"#,
        ])
        .cwd(&cwd)
        .env("SHELL_MANAGER_TEST", "ok")
        .env("EXPECTED_CWD", cwd.to_string_lossy());
    let manager = PtyManager::default_shell(80, 24);
    manager.create_pty("main", command, None, None).unwrap();
    let detail = manager.detail("main").unwrap();

    assert_eq!(detail.pty, "main");
    assert_eq!(detail.cwd.as_deref(), Some(cwd.to_string_lossy().as_ref()));
    assert_eq!(detail.env.get("SHELL_MANAGER_TEST").unwrap(), "ok");
    assert!(detail.process_id.is_some());
    assert!(detail.created_at > 0);

    assert_eq!(manager.wait_exit_code("main").unwrap(), 7);
    let _ = fs::remove_dir_all(cwd);
}

#[cfg(unix)]
#[tokio::test]
async fn wait_exit_code_timeout_does_not_block_later_kill() {
    use remote_executor_terminal::session::CommandSpec;
    use remote_executor_terminal::PtyManager;
    use std::time::Duration;

    let manager = PtyManager::default_shell(80, 24);
    manager
        .create_pty(
            "main",
            CommandSpec::new("sh").args(["-lc", "sleep 5"]),
            None,
            None,
        )
        .unwrap();

    assert_eq!(
        manager
            .wait_exit_code_timeout("main", Duration::from_millis(20))
            .await
            .unwrap(),
        None
    );
    manager.kill_pty("main").unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn subscribe_exit_code_reports_process_exit() {
    use remote_executor_terminal::session::CommandSpec;
    use remote_executor_terminal::PtyManager;
    use std::time::Duration;

    let manager = PtyManager::default_shell(80, 24);
    manager
        .create_pty(
            "main",
            CommandSpec::new("sh").args(["-lc", "exit 9"]),
            None,
            None,
        )
        .unwrap();
    let mut rx = manager.subscribe_exit_code("main").unwrap();

    let code = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code, 9);
    assert_eq!(manager.try_exit_code("main").unwrap(), Some(9));
}

#[cfg(unix)]
#[tokio::test]
async fn live_output_has_no_replay_and_slow_viewers_do_not_block_others() {
    use remote_executor_terminal::{CommandSpec, PtyManager};
    let manager = PtyManager::default_shell(80, 24);
    let session = manager
        .create_pty("raw", CommandSpec::new("cat"), None, None)
        .unwrap();
    session.on_pty_output(b"past");
    let mut fast = session.subscribe_output();
    let mut slow = session.subscribe_output();
    assert!(fast.try_recv().unwrap().is_none());
    let bytes = b"\x1b[31m\xff\0";
    session.on_pty_output(bytes);
    assert_eq!(fast.recv().await.unwrap(), bytes);
    assert_eq!(slow.recv().await.unwrap(), bytes);
    // Thousands of tiny reads fit the byte budget and coalesce in the slow queue.
    for _ in 0..20_000 {
        session.on_pty_output(b"x");
        assert_eq!(fast.recv().await.unwrap(), b"x");
    }
    assert_eq!(slow.recv().await.unwrap(), vec![b'x'; 20_000]);
    let block = vec![b'z'; 65536];
    for _ in 0..=remote_executor_terminal::session::LIVE_OUTPUT_LIMIT / block.len() {
        session.on_pty_output(&block);
        assert_eq!(fast.recv().await.unwrap(), block);
    }
    assert!(slow.try_recv().unwrap_err().to_string().contains("lagged"));
    assert!(session.subscribe_output().try_recv().unwrap().is_none());
    assert_eq!(
        session.output_total_bytes(),
        (4 + bytes.len()
            + 20_000
            + remote_executor_terminal::session::LIVE_OUTPUT_LIMIT
            + block.len()) as u64
    );
    assert_eq!(manager.detail("raw").unwrap().output_history_bytes, 0);
    manager.kill_pty("raw").unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn subscribe_at_creation_captures_fast_command_output() {
    use remote_executor_terminal::{CommandSpec, PtyManager};
    let manager = PtyManager::default_shell(80, 24);
    let (_, mut output) = manager
        .create_pty_with_output(
            "fast",
            CommandSpec::new("sh").args(["-c", "printf first-byte"]),
            None,
            None,
        )
        .unwrap();
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), output.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bytes, b"first-byte");
    assert_eq!(manager.wait_exit_code("fast").unwrap(), 0);
}
