//! Bounded server audit records; standalone stdout only reports client connectivity.
mod file;
mod request;
#[cfg(test)]
mod tests;
use crate::protocol::management::SessionOwner;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogDetail {
    Summary,
    #[default]
    Operations,
    Results,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogCategory {
    Server,
    Authentication,
    Clients,
    Files,
    Search,
    Commands,
    Channels,
    Requests,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct LogConfig {
    pub enabled: bool,
    pub directory: Option<PathBuf>,
    pub detail: LogDetail,
    pub categories: Option<Vec<LogCategory>>,
    pub max_file_bytes: u64,
    pub max_files: usize,
    pub max_text_bytes: usize,
}
impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            directory: None,
            detail: LogDetail::Operations,
            categories: None,
            max_file_bytes: 16 * 1024 * 1024,
            max_files: 8,
            max_text_bytes: 4096,
        }
    }
}
impl LogConfig {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (4096..=1024 * 1024 * 1024).contains(&self.max_file_bytes),
            "logging.maxFileBytes must be 4096..1073741824"
        );
        anyhow::ensure!(
            (1..=128).contains(&self.max_files),
            "logging.maxFiles must be 1..128"
        );
        anyhow::ensure!(
            (256..=65536).contains(&self.max_text_bytes),
            "logging.maxTextBytes must be 256..65536"
        );
        Ok(())
    }
}
#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    file_read_requests: u64,
    file_write_requests: u64,
    searches: u64,
    commands: u64,
    failed_requests: u64,
    uploads: u64,
    downloads: u64,
    channels_opened: u64,
}
struct SessionAudit {
    peer: String,
    counts: Counts,
    ended: Option<u64>,
}
struct Output {
    executor_id: String,
    console: Mutex<Box<dyn Write + Send>>,
    connection_only: bool,
    file: Option<file::FileLog>,
    config: LogConfig,
    sessions: Mutex<BTreeMap<String, SessionAudit>>,
}
#[derive(Clone, Default)]
pub struct EventLog(Option<Arc<Output>>);
impl EventLog {
    pub fn with_writer(
        executor_id: impl Into<String>,
        writer: impl Write + Send + 'static,
    ) -> Self {
        Self(Some(Arc::new(Output {
            executor_id: executor_id.into(),
            console: Mutex::new(Box::new(writer)),
            connection_only: false,
            file: None,
            config: Default::default(),
            sessions: Default::default(),
        })))
    }
    pub fn stdout(executor_id: impl Into<String>) -> Self {
        let mut log = Self::with_writer(executor_id, std::io::stdout());
        Arc::get_mut(log.0.as_mut().unwrap())
            .unwrap()
            .connection_only = true;
        log
    }
    pub fn standalone(executor_id: impl Into<String>, config: LogConfig) -> anyhow::Result<Self> {
        config.validate()?;
        let executor_id = executor_id.into();
        let sink = if config.enabled {
            Some(file::FileLog::open(&executor_id, &config)?)
        } else {
            None
        };
        Ok(Self(Some(Arc::new(Output {
            executor_id,
            console: Mutex::new(Box::new(std::io::stdout())),
            connection_only: true,
            file: sink,
            config,
            sessions: Default::default(),
        }))))
    }
    pub(crate) fn handshake<T>(
        &self,
        result: anyhow::Result<T>,
        peer: &str,
        transport: &str,
    ) -> anyhow::Result<T> {
        if let Err(error) = &result {
            self.emit(
                "link.handshake_failed",
                None,
                json!({"peer":peer,"transport":transport,"error":error.to_string()}),
            );
        }
        result
    }
    pub fn directory(&self) -> Option<PathBuf> {
        self.0
            .as_ref()?
            .file
            .as_ref()
            .map(|s| s.directory().to_owned())
    }
    pub fn flush(&self) -> anyhow::Result<()> {
        if let Some(sink) = self.0.as_ref().and_then(|o| o.file.as_ref()) {
            sink.flush()?;
        }
        Ok(())
    }
    pub fn emit(&self, event: &str, owner: Option<&SessionOwner>, mut details: Value) {
        let Some(output) = &self.0 else { return };
        let stamp = crate::registry::now_ms();
        let mut peer = details["peer"].as_str().map(str::to_owned);
        let mut counts = None;
        if let Some(owner) = owner {
            let mut sessions = output.sessions.lock().unwrap();
            if matches!(event, "client.connected" | "client.resumed") {
                sessions.retain(|_, s| s.ended.is_none_or(|t| stamp.saturating_sub(t) < 60000));
                if sessions.len() >= 2048 {
                    if let Some(key) = sessions
                        .iter()
                        .filter(|(_, s)| s.ended.is_some())
                        .min_by_key(|(_, s)| s.ended)
                        .map(|(k, _)| k.clone())
                    {
                        sessions.remove(&key);
                    }
                }
                if sessions.len() < 2048 || sessions.contains_key(&owner.session_id) {
                    let s =
                        sessions
                            .entry(owner.session_id.clone())
                            .or_insert_with(|| SessionAudit {
                                peer: String::new(),
                                counts: Default::default(),
                                ended: None,
                            });
                    if let Some(p) = &peer {
                        s.peer = p.clone();
                    }
                    s.ended = None;
                }
            }
            if let Some(s) = sessions.get_mut(&owner.session_id) {
                if peer.is_none() {
                    peer = Some(s.peer.clone());
                }
                if event == "request.started" {
                    let tool = details["tool"].as_str().unwrap_or_default();
                    match tool {
                        "read" => s.counts.file_read_requests += 1,
                        "FileAction" => s.counts.file_write_requests += 1,
                        "rg" | "glob" => s.counts.searches += 1,
                        "exbash" if matches!(details["mode"].as_str(), Some("shell" | "run")) => {
                            s.counts.commands += 1
                        }
                        _ => {}
                    }
                }
                if event == "request.finished" && details["ok"] == false {
                    s.counts.failed_requests += 1;
                }
                if event == "file.started" {
                    if details["direction"] == "upload" {
                        s.counts.uploads += 1;
                    } else {
                        s.counts.downloads += 1;
                    }
                }
                if event == "channel.opened" {
                    s.counts.channels_opened += 1;
                }
                if event == "client.disconnected" {
                    s.ended = Some(stamp);
                }
                counts = Some(s.counts.clone());
            }
        }
        let ip = peer
            .as_deref()
            .and_then(|p| p.parse::<std::net::SocketAddr>().ok())
            .map(|p| p.ip().to_string());
        let connectivity = matches!(
            event,
            "client.connected" | "client.resumed" | "client.disconnected"
        );
        if output.connection_only && connectivity {
            let record = json!({"timestampMs":stamp,"event":event,"executorId":output.executor_id,"username":owner.map(|o|&o.username),"clientName":owner.map(|o|&o.client_name),"clientId":owner.map(|o|&o.client_id),"sessionId":owner.map(|o|&o.session_id),"ip":ip,"peer":peer});
            write_console(output, &record);
        }
        if output
            .config
            .categories
            .as_ref()
            .is_some_and(|v| !v.contains(&category(event, &details)))
        {
            return;
        }
        if output.config.detail == LogDetail::Summary {
            if event == "request.started" {
                return;
            }
            if let Some(d) = details.as_object_mut() {
                for key in [
                    "params",
                    "command",
                    "title",
                    "description",
                    "program",
                    "cwd",
                    "path",
                    "target",
                    "query",
                    "output",
                    "result",
                    "args",
                    "directory",
                    "stderr",
                ] {
                    d.remove(key);
                }
            }
        }
        let details = bounded(details, output.config.max_text_bytes, 0);
        let record = json!({"timestampMs":stamp,"executorId":output.executor_id,"event":event,"owner":owner,"peer":peer,"ip":ip,"counts":counts,"details":details});
        if !output.connection_only {
            write_console(output, &record);
        }
        if let Some(sink) = &output.file {
            sink.write(record);
        }
    }
    pub(crate) fn span(
        &self,
        start: &str,
        end: &'static str,
        owner: Option<SessionOwner>,
        details: Value,
    ) -> EventSpan {
        self.emit(start, owner.as_ref(), details.clone());
        EventSpan {
            log: self.clone(),
            end,
            owner,
            details,
            finished: false,
        }
    }
    pub(crate) fn detail(&self) -> LogDetail {
        self.0
            .as_ref()
            .map(|o| o.config.detail)
            .unwrap_or(LogDetail::Summary)
    }
}
fn write_console(output: &Output, record: &Value) {
    if let Ok(mut line) = serde_json::to_vec(record) {
        line.push(b'\n');
        let mut writer = output.console.lock().unwrap();
        let _ = writer.write_all(&line);
        let _ = writer.flush();
    }
}
fn category(event: &str, details: &Value) -> LogCategory {
    if event.starts_with("authentication.") {
        return LogCategory::Authentication;
    }
    if event.starts_with("client.") {
        return LogCategory::Clients;
    }
    if event.starts_with("file.") {
        return LogCategory::Files;
    }
    if event.starts_with("program.") {
        return LogCategory::Commands;
    }
    if event.starts_with("channel.")
        || event.starts_with("pty.")
        || event.starts_with("link.")
        || event.starts_with("forward.")
    {
        return LogCategory::Channels;
    }
    if event.starts_with("request.") {
        return match details["tool"].as_str() {
            Some("read" | "stat" | "FileAction") => LogCategory::Files,
            Some("rg" | "glob") => LogCategory::Search,
            Some("exbash") => LogCategory::Commands,
            _ => LogCategory::Requests,
        };
    }
    LogCategory::Server
}
fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated; {} bytes]", &text[..end], text.len())
}
fn bounded(value: Value, limit: usize, depth: usize) -> Value {
    if depth > 5 {
        return json!("[depth limit]");
    }
    match value {
        Value::String(s) => json!(clip(&s, limit)),
        Value::Array(v) => {
            let truncated = v.len() > 32;
            let mut a: Vec<_> = v
                .into_iter()
                .take(32)
                .map(|v| bounded(v, limit, depth + 1))
                .collect();
            if truncated {
                a.push(json!({"truncated":true}));
            }
            Value::Array(a)
        }
        Value::Object(v) => Value::Object(
            v.into_iter()
                .take(48)
                .map(|(k, v)| (k, bounded(v, limit, depth + 1)))
                .collect(),
        ),
        other => other,
    }
}
pub(crate) struct EventSpan {
    log: EventLog,
    end: &'static str,
    owner: Option<SessionOwner>,
    details: Value,
    finished: bool,
}
impl EventSpan {
    pub(crate) fn bind(&mut self, owner: SessionOwner) {
        self.owner = Some(owner);
        self.log
            .emit("link.bound", self.owner.as_ref(), self.details.clone());
    }
    pub(crate) fn set(&mut self, key: &str, value: Value) {
        self.details[key] = value;
    }
    pub(crate) fn finish(&mut self, outcome: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut d = self.details.clone();
        d["outcome"] = json!(outcome);
        self.log.emit(self.end, self.owner.as_ref(), d);
    }
}
impl Drop for EventSpan {
    fn drop(&mut self) {
        self.finish("closed");
    }
}
