use super::*;
use crate::{
    tools::exbash::{exbash, ExbashOptions},
    ShellManager,
};
fn owner(id: &str) -> SessionOwner {
    SessionOwner {
        client_id: id.into(),
        session_id: format!("session-{id}"),
        username: "same-user".into(),
        client_name: "test".into(),
        client_version: "1".into(),
    }
}
fn context(manager: &ShellManager, id: &str) -> ToolContext {
    ToolContext::new(None)
        .with_shell_manager(manager.clone())
        .with_client_owner(Some(owner(id)))
}
async fn call(ctx: &ToolContext, params: Value) -> ToolResult {
    exbash(
        serde_json::from_value::<ExbashOptions>(params).unwrap(),
        ctx,
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn pipes_keep_streams_separate_and_collect_the_exit_tail() {
    let manager = ShellManager::default_shell(80, 24);
    let ctx = context(&manager, "a");
    let command = if cfg!(windows) {
        "[Console]::Out.Write('out'); [Console]::Error.Write('err'); exit 7"
    } else {
        "test ! -t 0 && test ! -t 1 && test ! -t 2 || exit 8; printf out; printf err >&2; exit 7"
    };
    let result = call(
        &ctx,
        json!({"mode":"shell","io":"stdio","command":command,"read_timeout":10000}),
    )
    .await;
    assert_eq!(result.metadata["state"], "stopped");
    assert_eq!(result.metadata["exitCode"], 7);
    assert_eq!(result.metadata["stdout"], "out");
    assert_eq!(result.metadata["stderr"], "err");
    assert!(result.metadata.get("terminalOutput").is_none());
    assert!(manager.core().list().is_empty());
    assert_eq!(manager.stdio.list(Some(&owner("a"))).len(), 0);
}
#[tokio::test]
async fn detached_tasks_accept_input_eof_and_enforce_client_ownership() {
    let manager = ShellManager::default_shell(80, 24);
    let a = context(&manager, "a");
    let b = context(&manager, "b");
    let command = if cfg!(windows) {
        "$x=[Console]::ReadLine(); [Console]::Out.Write('got:'+$x)"
    } else {
        "IFS= read -r line; printf 'got:%s' \"$line\""
    };
    let started = call(
        &a,
        json!({"mode":"shell","io":"stdio","command":command,"keepStdin":true,"read_timeout":0}),
    )
    .await;
    let id = started.metadata["asyncID"].as_str().unwrap();
    assert!(manager.stdio.list(Some(&owner("b"))).is_empty());
    assert_eq!(manager.program_summaries().len(), 1);
    let illegal = serde_json::from_value::<ExbashOptions>(
        json!({"mode":"attach","asyncID":id,"text":"bad\\n"}),
    )
    .unwrap();
    assert!(exbash(illegal, &b).await.is_err());
    let result = call(
        &a,
        json!({"mode":"attach","asyncID":id,"text":"literal\\\\path\\n","read_timeout":10000}),
    )
    .await;
    assert_eq!(result.metadata["stdout"], "got:literal\\path");
    assert_eq!(result.metadata["exitCode"], 0);
    call(&a, json!({"mode":"remove","asyncID":id})).await;
    assert!(manager.stdio.get(id, Some(&owner("a"))).is_err());
    let command = if cfg!(windows) {
        "[Console]::Out.Write([Console]::In.ReadToEnd())"
    } else {
        "cat"
    };
    let result = call(
        &a,
        json!({"mode":"shell","io":"stdio","command":command,"stdin":"seed","read_timeout":10000}),
    )
    .await;
    assert_eq!(result.metadata["stdout"], "seed");
}
#[cfg(unix)]
#[tokio::test]
async fn deadline_and_lease_stop_descendants_and_capture_is_bounded() {
    let manager = ShellManager::default_shell(80, 24);
    let mut ctx = context(&manager, "a");
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("escaped");
    let command = format!("(sleep 1; touch '{}') & wait", marker.display());
    let result = call(
        &ctx,
        json!({"mode":"shell","io":"stdio","command":command,"timeout":50,"read_timeout":2000}),
    )
    .await;
    assert_eq!(result.metadata["timedOut"], true);
    let result=call(&ctx,json!({"mode":"shell","io":"stdio","command":"head -c 2097152 /dev/zero","read_timeout":2000})).await;
    assert!(result.metadata["stdout"].as_str().unwrap().len() <= LIMIT);
    assert_eq!(result.metadata["outputTruncated"], true);
    let (cancel, receiver) = watch::channel(false);
    ctx.cancellation = Some(receiver);
    let result = call(
        &ctx,
        json!({"mode":"shell","io":"stdio","command":"sleep 60","read_timeout":0}),
    )
    .await;
    let id = result.metadata["asyncID"].as_str().unwrap();
    cancel.send_replace(true);
    let task = manager.stdio.get(id, Some(&owner("a"))).unwrap();
    let mut finished = task.finished.subscribe();
    if !*finished.borrow_and_update() {
        tokio::time::timeout(Duration::from_secs(2), finished.changed())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(task.status.lock().unwrap()["stopped"], true);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(!marker.exists());
}
