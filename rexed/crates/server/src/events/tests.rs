use super::*;
use crate::protocol::{ExecutorRequest, ExecutorResponse};
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn owner() -> SessionOwner {
    SessionOwner {
        username: "alice".into(),
        client_name: "editor".into(),
        client_id: "client-a".into(),
        session_id: "session-a".into(),
        client_version: "test".into(),
    }
}
fn captured(config: LogConfig) -> (EventLog, Capture) {
    let capture = Capture::default();
    let mut log = EventLog::standalone("audit-tests", config).unwrap();
    Arc::get_mut(log.0.as_mut().unwrap()).unwrap().console = Mutex::new(Box::new(capture.clone()));
    (log, capture)
}
fn rows(log: &EventLog) -> Vec<Value> {
    log.flush().unwrap();
    std::fs::read_to_string(log.directory().unwrap().join("audit.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
#[test]
fn stdout_is_connectivity_only_and_operations_keep_identity_counts_and_no_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let (log, capture) = captured(LogConfig {
        directory: Some(temp.path().join("logs")),
        ..Default::default()
    });
    let who = owner();
    log.emit(
        "client.connected",
        Some(&who),
        json!({"peer":"127.0.0.1:3210"}),
    );
    let request = ExecutorRequest {
        id: json!(3),
        method: "read".into(),
        params: json!({"filePath":"/remote/example.txt","password":"never-log-password","secret":"never-log-secret","content":"never-log-body"}),
        directory: None,
        executor: None,
        tool_timeout_ms: None,
    };
    log.request(&request, Some(&who), Some("127.0.0.1:4321"))
        .finish(&ExecutorResponse::ok(
            json!(3),
            None,
            json!({"metadata":{"lineCount":2},"output":{"text":"private-file-content"}}),
        ));
    log.emit(
        "client.disconnected",
        Some(&who),
        json!({"reason":"lease_ended"}),
    );
    let records = rows(&log);
    let text = serde_json::to_string(&records).unwrap();
    for forbidden in [
        "never-log-password",
        "never-log-secret",
        "never-log-body",
        "private-file-content",
    ] {
        assert!(!text.contains(forbidden));
    }
    let operation = records
        .iter()
        .find(|v| v["event"] == "request.finished")
        .unwrap();
    assert_eq!(operation["owner"]["username"], "alice");
    assert_eq!(operation["ip"], "127.0.0.1");
    assert_eq!(operation["peer"], "127.0.0.1:4321");
    assert_eq!(operation["counts"]["fileReadRequests"], 1);
    assert_eq!(
        operation["details"]["params"]["filePath"],
        "/remote/example.txt"
    );
    let stdout = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["username"], "alice");
    assert_eq!(lines[0]["clientName"], "editor");
    assert_eq!(lines[1]["ip"], "127.0.0.1");
    assert!(!stdout.contains("example.txt"));
    assert!(!stdout.contains("request.finished"));
}
#[test]
fn summary_and_category_filters_preserve_connection_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let (log, capture) = captured(LogConfig {
        directory: Some(temp.path().join("logs")),
        detail: LogDetail::Summary,
        categories: Some(vec![LogCategory::Files]),
        ..Default::default()
    });
    let who = owner();
    log.emit("client.connected", Some(&who), json!({"peer":"[::1]:33"}));
    let request = ExecutorRequest {
        id: json!(1),
        method: "read".into(),
        params: json!({"filePath":"not-in-summary"}),
        directory: None,
        executor: None,
        tool_timeout_ms: None,
    };
    log.request(&request, Some(&who), None)
        .finish(&ExecutorResponse::err(json!(1), None, "missing file"));
    let records = rows(&log);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["event"], "request.finished");
    assert!(records[0]["details"].get("params").is_none());
    assert_eq!(records[0]["counts"]["failedRequests"], 1);
    let stdout = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(stdout.trim()).unwrap()["ip"],
        "::1"
    );
}
#[test]
fn result_text_is_bounded_and_rotation_has_a_fixed_disk_budget() {
    let temp = tempfile::tempdir().unwrap();
    let config = LogConfig {
        directory: Some(temp.path().join("logs")),
        detail: LogDetail::Results,
        max_file_bytes: 4096,
        max_files: 3,
        max_text_bytes: 256,
        ..Default::default()
    };
    let (log, _) = captured(config.clone());
    assert!(
        EventLog::standalone("audit-tests", config).is_err(),
        "concurrent writers must not rotate each other's files"
    );
    let request = ExecutorRequest {
        id: json!(1),
        method: "exbash".into(),
        params: json!({"mode":"run","command":"echo example"}),
        directory: None,
        executor: None,
        tool_timeout_ms: None,
    };
    log.request(&request, None, None)
        .finish(&ExecutorResponse::ok(
            json!(1),
            None,
            json!({"metadata":{"exitCode":0},"output":{"text":"汉".repeat(3000)}}),
        ));
    let initial = rows(&log);
    let result = initial
        .iter()
        .find(|v| v["event"] == "request.finished")
        .unwrap();
    assert!(result["details"]["output"]
        .as_str()
        .unwrap()
        .contains("truncated"));
    assert_eq!(result["details"]["result"]["exitCode"], 0);
    for index in 0..30 {
        log.emit(
            "server.test",
            None,
            json!({"index":index,"padding":"x".repeat(1000)}),
        );
    }
    log.flush().unwrap();
    let mut files = 0;
    for entry in std::fs::read_dir(log.directory().unwrap()).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|v| v == "jsonl") {
            files += 1;
            assert!(std::fs::metadata(&p).unwrap().len() <= 4096);
            for line in std::fs::read_to_string(p).unwrap().lines() {
                serde_json::from_str::<Value>(line).unwrap();
            }
        }
    }
    assert_eq!(files, 3);
    assert!(rows(&log).iter().any(|v| v["details"]["index"] == 29));
}
#[test]
fn file_logging_can_be_disabled_without_suppressing_connections() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("not-created");
    let (log, capture) = captured(LogConfig {
        enabled: false,
        directory: Some(root.clone()),
        ..Default::default()
    });
    log.emit(
        "client.connected",
        Some(&owner()),
        json!({"peer":"127.0.0.1:1"}),
    );
    log.flush().unwrap();
    assert!(!root.exists());
    assert!(!capture.0.lock().unwrap().is_empty());
}

#[test]
fn summary_omits_task_labels_while_operations_keep_them() {
    for detail in [LogDetail::Summary, LogDetail::Operations] {
        let temp = tempfile::tempdir().unwrap();
        let (log, capture) = captured(LogConfig {
            directory: Some(temp.path().join("logs")),
            detail,
            ..Default::default()
        });
        log.emit("program.created", Some(&owner()), json!({"programId":"task-1","title":"task-label","description":"task-description","command":"private-command"}));
        let records = rows(&log);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["details"]["programId"], "task-1");
        for (key, value) in [("title", "task-label"), ("description", "task-description")] {
            if detail == LogDetail::Summary {
                assert!(records[0]["details"].get(key).is_none());
            } else {
                assert_eq!(records[0]["details"][key], value);
            }
        }
        assert!(capture.0.lock().unwrap().is_empty());
    }
}
