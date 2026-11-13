//! Bounded task outcomes belong to the application instance, not to rexed.
use crate::{client::Client, protocol::ExecutorRequest};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::time::{sleep, Duration};
#[derive(Default)]
pub(crate) struct TaskHistory {
    entries: Mutex<BTreeMap<String, Value>>,
    path: Option<PathBuf>,
    error: Mutex<Option<String>>,
    tasks: Mutex<BTreeMap<String, tokio::task::AbortHandle>>,
    stopped: AtomicBool,
}
impl Drop for TaskHistory {
    fn drop(&mut self) {
        for task in std::mem::take(self.tasks.get_mut().unwrap()).into_values() {
            task.abort();
        }
    }
}
impl TaskHistory {
    pub fn load(path: PathBuf) -> Arc<Self> {
        let (mut entries, error) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<BTreeMap<String, Value>>(&bytes) {
                Ok(v) => (v, None),
                Err(e) => (BTreeMap::new(), Some(e.to_string())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BTreeMap::new(), None),
            Err(e) => (BTreeMap::new(), Some(e.to_string())),
        };
        for entry in entries.values_mut() {
            if entry["state"] == "running" {
                entry["state"] = json!("unknown");
                entry["reason"] = json!("应用重启，未收到最终结果");
            }
        }
        while entries.len() > 200 {
            let key = entries.keys().next().unwrap().clone();
            entries.remove(&key);
        }
        Arc::new(Self {
            entries: Mutex::new(entries),
            path: Some(path),
            error: Mutex::new(error),
            tasks: Default::default(),
            stopped: Default::default(),
        })
    }
    pub fn remove(&self, executor: &str, id: &str) -> anyhow::Result<()> {
        let key = format!("{executor}\0{id}");
        {
            let entries = self.entries.lock().unwrap();
            anyhow::ensure!(
                entries.get(&key).is_some_and(|v| v["state"] != "running"),
                "task is running or history is absent"
            );
        }
        if let Some(task) = self.tasks.lock().unwrap().remove(&key) {
            task.abort();
        }
        let mut entries = self.entries.lock().unwrap();
        entries.remove(&key);
        self.persist(&entries);
        Ok(())
    }
    /// Remember that a client has seen this task's output.
    pub fn mark_read(&self, executor: &str, id: &str) {
        let key = format!("{executor}\0{id}");
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get_mut(&key) {
            if entry["read"].as_bool() != Some(true) {
                entry["read"] = json!(true);
                self.persist(&entries);
            }
        }
    }
    pub fn snapshot(&self) -> Value {
        json!({"tasks":self.entries.lock().unwrap().values().collect::<Vec<_>>(),"persistenceError":self.error.lock().unwrap().clone()})
    }
    fn update(&self, key: &str, entry: Value) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(key.into(), entry);
        while entries.len() > 200 {
            let old = entries
                .iter()
                .filter(|(_, v)| v["state"] != "running")
                .min_by_key(|(_, v)| v["observedAt"].as_u64().unwrap_or_default())
                .map(|(k, _)| k.clone());
            if let Some(old) = old {
                entries.remove(&old);
            } else {
                break;
            }
        }
        self.persist(&entries);
    }
    fn persist(&self, entries: &BTreeMap<String, Value>) {
        if let Some(path) = &self.path {
            let result = (|| -> anyhow::Result<()> {
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| std::path::Path::new("."));
                std::fs::create_dir_all(parent)?;
                let mut file = tempfile::NamedTempFile::new_in(parent)?;
                serde_json::to_writer_pretty(&mut file, entries)?;
                file.write_all(b"\n")?;
                file.as_file().sync_all()?;
                file.persist(path)?;
                Ok(())
            })();
            if let Err(e) = result {
                *self.error.lock().unwrap() = Some(e.to_string());
            }
        }
    }
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        for task in std::mem::take(&mut *self.tasks.lock().unwrap()).into_values() {
            task.abort();
        }
    }
    pub fn can_start(&self) -> bool {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter(|v| v["state"] == "running")
            .count()
            < 256
    }
    pub fn observe(self: &Arc<Self>, client: Client, executor: String, metadata: Value) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let Some(id) = metadata["asyncID"].as_str().map(str::to_owned) else {
            return;
        };
        let key = format!("{executor}\0{id}");
        if metadata["state"] != "stopped"
            && self
                .entries
                .lock()
                .unwrap()
                .get(&key)
                .is_some_and(|v| v["state"] == "running")
        {
            return;
        }
        let mut entry = metadata;
        if entry["io"] == "stdio" {
            if let Some(object) = entry.as_object_mut() {
                object.remove("stdout");
                object.remove("stderr");
            }
        }
        entry["host"] = json!(executor);
        entry["observedAt"] = json!(now_ms());
        self.update(&key, entry.clone());
        if entry["state"] == "stopped" {
            if let Some(task) = self.tasks.lock().unwrap().remove(&key) {
                task.abort();
            }
            return;
        }
        let weak = Arc::downgrade(self);
        let task_key = key.clone();
        let task = tokio::spawn(async move {
            let mut failures = 0;
            loop {
                sleep(Duration::from_secs(2)).await;
                if weak
                    .upgrade()
                    .is_none_or(|h| h.stopped.load(Ordering::SeqCst))
                {
                    break;
                }
                let result = client.handle(request(&executor, "status", &id)).await;
                if result.ok {
                    failures = 0;
                    let metadata = result.result.unwrap_or(Value::Null)["metadata"].clone();
                    if metadata["state"] == "stopped" {
                        entry = metadata;
                        entry["host"] = json!(executor);
                        entry["observedAt"] = json!(now_ms());
                        if let Some(history) = weak.upgrade() {
                            history.update(&key, entry);
                        }
                        // Consume only a confirmed terminal receipt; never remove an active task.
                        let _ = client.handle(request(&executor, "remove", &id)).await;
                        break;
                    }
                } else {
                    failures += 1;
                    if failures >= 3 {
                        entry["state"] = json!("unknown");
                        entry["reason"] = json!("未收到最终结果，远端任务不可查询");
                        entry["observedAt"] = json!(now_ms());
                        if let Some(history) = weak.upgrade() {
                            history.update(&key, entry);
                        }
                        break;
                    }
                }
            }
        });
        let mut tasks = self.tasks.lock().unwrap();
        tasks.retain(|_, t| !t.is_finished());
        tasks.insert(task_key, task.abort_handle());
    }
}
fn request(executor: &str, mode: &str, id: &str) -> ExecutorRequest {
    ExecutorRequest {
        id: json!("task-history"),
        method: "exbash".into(),
        params: json!({"mode":mode,"asyncID":id}),
        executor: Some(executor.into()),
        directory: None,
        tool_timeout_ms: Some(5000),
    }
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
