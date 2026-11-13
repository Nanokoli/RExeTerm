//! One bounded queue and writer thread; rotation never runs on an executor IO task.
use super::LogConfig;
use crate::auth::{private_directory, private_file};
use anyhow::{anyhow, ensure, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    thread::JoinHandle,
};
enum Message {
    Record(Vec<u8>),
    Flush(SyncSender<std::result::Result<(), String>>),
}
pub(super) struct FileLog {
    directory: PathBuf,
    sender: Option<SyncSender<Message>>,
    worker: Option<JoinHandle<()>>,
    lost: Arc<AtomicU64>,
    warned: AtomicBool,
    max_record: usize,
}
impl FileLog {
    pub(super) fn open(executor: &str, config: &LogConfig) -> Result<Self> {
        let exe = std::env::current_exe()?;
        let base = exe
            .parent()
            .ok_or_else(|| anyhow!("executable has no parent directory"))?;
        let root = base.join(
            config
                .directory
                .as_deref()
                .unwrap_or_else(|| Path::new(".rexed-logs")),
        );
        private_directory(&root)?;
        let directory = root.join(format!("{:x}", Sha256::digest(executor.as_bytes())));
        private_directory(&directory)?;
        let lock_path = directory.join("writer.lock");
        ensure!(!lock_path.is_symlink(), "audit lock must not be a symlink");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        private_file(&lock_path)?;
        lock.try_lock()
            .map_err(|e| anyhow!("audit directory is already in use: {e}"))?;
        let writer = Rotating::open(
            directory.clone(),
            config.max_file_bytes,
            config.max_files,
            lock,
        )?;
        let (sender, receiver) = mpsc::sync_channel(256);
        let lost = Arc::new(AtomicU64::new(0));
        let pending = lost.clone();
        let executor = executor.to_owned();
        let worker = std::thread::Builder::new()
            .name("rexed-audit".into())
            .spawn(move || {
                let mut writer = writer;
                let mut error = None;
                loop {
                    let message = receiver.recv_timeout(std::time::Duration::from_millis(100));
                    // Report overflow even when no further record arrives before flush/shutdown.
                    if let Err(e) = writer.report_lost(&pending, &executor) {
                        if error.is_none() {
                            eprintln!("rexed audit write failed: {e}");
                        }
                        error = Some(e.to_string());
                    }
                    match message {
                        Ok(Message::Record(line)) => {
                            if let Err(e) = writer.write(&line) {
                                pending.fetch_add(1, Ordering::Relaxed);
                                if error.is_none() {
                                    eprintln!("rexed audit write failed: {e}");
                                }
                                error = Some(e.to_string());
                            }
                        }
                        Ok(Message::Flush(reply)) => {
                            let result = writer
                                .flush()
                                .map_err(|e| e.to_string())
                                .and_then(|()| error.take().map_or(Ok(()), Err));
                            let _ = reply.send(result);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Err(e) = writer.flush() {
                                if error.is_none() {
                                    eprintln!("rexed audit flush failed: {e}");
                                }
                                error = Some(e.to_string());
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            if let Err(e) = writer.flush() {
                                eprintln!("rexed audit final flush failed: {e}");
                            }
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            directory,
            sender: Some(sender),
            worker: Some(worker),
            lost,
            warned: AtomicBool::new(false),
            max_record: config.max_file_bytes.min(65536) as usize,
        })
    }
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }
    pub(super) fn write(&self, mut record: Value) {
        let Ok(mut bytes) = serde_json::to_vec(&record) else {
            return;
        };
        if bytes.len() + 1 > self.max_record {
            let size = bytes.len();
            record["details"] = json!({"operationId":record["details"]["operationId"],"tool":record["details"]["tool"],"recordTruncated":true,"originalBytes":size});
            bytes = serde_json::to_vec(&record).unwrap();
        }
        bytes.push(b'\n');
        if self
            .sender
            .as_ref()
            .unwrap()
            .try_send(Message::Record(bytes))
            .is_err()
        {
            self.lost.fetch_add(1, Ordering::Relaxed);
            if !self.warned.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "rexed audit queue is full; lost records will be reported as logging.dropped"
                );
            }
        }
    }
    pub(super) fn flush(&self) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .unwrap()
            .send(Message::Flush(tx))
            .map_err(|_| anyhow!("audit writer stopped"))?;
        rx.recv()
            .map_err(|_| anyhow!("audit writer stopped"))?
            .map_err(anyhow::Error::msg)
    }
}
impl Drop for FileLog {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
struct Rotating {
    directory: PathBuf,
    writer: Option<BufWriter<File>>,
    bytes: u64,
    max_bytes: u64,
    max_files: usize,
    _lock: File,
}
impl Rotating {
    fn open(directory: PathBuf, max_bytes: u64, max_files: usize, lock: File) -> Result<Self> {
        let mut s = Self {
            directory,
            writer: None,
            bytes: 0,
            max_bytes,
            max_files,
            _lock: lock,
        };
        s.reopen()?;
        Ok(s)
    }
    fn reopen(&mut self) -> Result<()> {
        let path = self.directory.join("audit.jsonl");
        ensure!(!path.is_symlink(), "audit file must not be a symlink");
        let file = OpenOptions::new().append(true).create(true).open(&path)?;
        private_file(&path)?;
        self.bytes = file.metadata()?.len();
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }
    fn write(&mut self, line: &[u8]) -> Result<()> {
        if self.writer.is_none() {
            self.reopen()?;
        }
        if self.bytes > 0 && self.bytes.saturating_add(line.len() as u64) > self.max_bytes {
            self.flush()?;
            self.writer.take();
            let oldest = self.directory.join(if self.max_files == 1 {
                "audit.jsonl".to_owned()
            } else {
                format!("audit.{}.jsonl", self.max_files - 1)
            });
            if oldest.exists() {
                std::fs::remove_file(oldest)?;
            }
            for i in (1..self.max_files - 1).rev() {
                let a = self.directory.join(format!("audit.{i}.jsonl"));
                if a.exists() {
                    std::fs::rename(a, self.directory.join(format!("audit.{}.jsonl", i + 1)))?;
                }
            }
            if self.max_files > 1 {
                std::fs::rename(
                    self.directory.join("audit.jsonl"),
                    self.directory.join("audit.1.jsonl"),
                )?;
            }
            self.reopen()?;
        }
        self.writer.as_mut().unwrap().write_all(line)?;
        self.bytes = self.bytes.saturating_add(line.len() as u64);
        Ok(())
    }
    fn report_lost(&mut self, pending: &AtomicU64, executor: &str) -> Result<()> {
        let count = pending.swap(0, Ordering::Relaxed);
        if count == 0 {
            return Ok(());
        }
        let mut notice = serde_json::to_vec(
            &json!({"timestampMs":crate::registry::now_ms(),"executorId":executor,"event":"logging.dropped","details":{"records":count}}),
        )?;
        notice.push(b'\n');
        if let Err(e) = self.write(&notice) {
            pending.fetch_add(count, Ordering::Relaxed);
            return Err(e);
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if let Some(writer) = &mut self.writer {
            writer.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flush_reports_overflow_without_needing_another_record() {
        let temp = tempfile::tempdir().unwrap();
        let log = FileLog::open(
            "overflow",
            &LogConfig {
                directory: Some(temp.path().join("logs")),
                ..Default::default()
            },
        )
        .unwrap();
        // Deterministically simulate the producer's overflow counter; no timing race.
        log.lost.fetch_add(7, Ordering::Relaxed);
        log.flush().unwrap();
        let text = std::fs::read_to_string(log.directory.join("audit.jsonl")).unwrap();
        let record: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(record["event"], "logging.dropped");
        assert_eq!(record["details"]["records"], 7);
    }
}
