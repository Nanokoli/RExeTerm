//! Allowlisted request parameters and completion summaries, never credentials or input bytes.
use super::{EventLog, LogDetail};
use crate::protocol::{management::SessionOwner, ExecutorRequest, ExecutorResponse};
use serde_json::{json, Value};
use std::time::Instant;
pub(crate) struct RequestAudit {
    log: EventLog,
    owner: Option<SessionOwner>,
    details: Value,
    started: Instant,
    finished: bool,
}
impl EventLog {
    pub(crate) fn request(
        &self,
        request: &ExecutorRequest,
        owner: Option<&SessionOwner>,
        peer: Option<&str>,
    ) -> RequestAudit {
        let mut details = json!({"operationId":format!("op-{:032x}",rand::random::<u128>()),"requestId":request.id,"tool":request.method,"mode":request.params.get("mode"),"directory":request.directory,"peer":peer});
        if self.detail() != LogDetail::Summary {
            let keys: &[&str] = match request.method.as_str() {
                "read" | "stat" => &["filePath", "mode", "offset", "limit", "hashCheckMode"],
                "FileAction" => &[
                    "filePath",
                    "newFilePath",
                    "mode",
                    "patchMode",
                    "hashCheckMode",
                ],
                "glob" => &["pattern", "path", "timeout"],
                "rg" => &[
                    "pattern",
                    "root",
                    "path",
                    "glob",
                    "ignoreCase",
                    "maxCount",
                    "timeout",
                    "fixedStrings",
                    "filesOnly",
                ],
                "exbash" => &[
                    "mode",
                    "command",
                    "io",
                    "asyncID",
                    "shell",
                    "read_timeout",
                    "timeout",
                    "title",
                    "description",
                ],
                _ => &[],
            };
            let mut params = serde_json::Map::new();
            for key in keys {
                if let Some(v) = request.params.get(*key) {
                    params.insert((*key).into(), v.clone());
                }
            }
            details["params"] = Value::Object(params);
        }
        self.emit("request.started", owner, details.clone());
        RequestAudit {
            log: self.clone(),
            owner: owner.cloned(),
            details,
            started: Instant::now(),
            finished: false,
        }
    }
}
impl RequestAudit {
    pub(crate) fn finish(&mut self, response: &ExecutorResponse) {
        self.finished = true;
        let mut details = self.details.clone();
        details["ok"] = json!(response.ok);
        details["durationMs"] = json!(self.started.elapsed().as_millis());
        details["error"] = json!(response.error);
        if let Some(result) = &response.result {
            let mut metadata = serde_json::Map::new();
            for key in [
                "asyncID",
                "state",
                "pid",
                "exitCode",
                "startedAt",
                "endedAt",
                "totalOutput",
                "stdoutBytes",
                "stderrBytes",
                "filePath",
                "size",
                "kind",
                "bytes",
                "hashCode",
                "lineCount",
                "matches",
                "filesWalked",
                "code",
                "timedOut",
                "removed",
                "stopped",
                "created",
                "deleted",
                "renamed",
                "outputTruncated",
            ] {
                if let Some(v) = result["metadata"].get(key) {
                    metadata.insert(key.into(), v.clone());
                }
            }
            details["result"] = Value::Object(metadata);
            if self.log.detail() == LogDetail::Results {
                // Bound the string before cloning; large file/terminal results are never queued whole.
                if let Some(text) = result["output"]["text"].as_str() {
                    let limit = self
                        .log
                        .0
                        .as_ref()
                        .map(|o| o.config.max_text_bytes)
                        .unwrap_or(4096);
                    details["output"] = json!(super::clip(text, limit));
                }
                if let Some(text) = result["metadata"]["stderr"].as_str() {
                    let limit = self
                        .log
                        .0
                        .as_ref()
                        .map(|o| o.config.max_text_bytes)
                        .unwrap_or(4096);
                    details["stderr"] = json!(super::clip(text, limit));
                }
            }
        }
        self.log
            .emit("request.finished", self.owner.as_ref(), details);
    }
}
impl Drop for RequestAudit {
    fn drop(&mut self) {
        if !self.finished {
            let mut d = self.details.clone();
            d["outcome"] = json!("cancelled");
            d["durationMs"] = json!(self.started.elapsed().as_millis());
            self.log.emit("request.finished", self.owner.as_ref(), d);
        }
    }
}
