//! Native PTY allocation and shared session state.
mod input;
mod metadata;
mod output;
pub use output::{OutputSubscription, LIVE_OUTPUT_LIMIT};
mod process;

use anyhow::Result;
use output::OutputState;
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

pub use crate::types::{CommandSpec, SessionDetail, SessionSummary, TermSize};

pub const DEFAULT_OUTPUT_HISTORY_LIMIT: usize = 0;

struct ExitState {
    code: Option<u32>,
    ended_at: Option<u64>,
    subscribers: Vec<mpsc::UnboundedSender<u32>>,
}

pub struct Session {
    name: String,
    command: CommandSpec,
    cwd: Option<std::path::PathBuf>,
    created_at: u64,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
    child_killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    process_id: Option<u32>,
    exit: Mutex<ExitState>,
    exit_signal: Condvar,
    auto_status_response: std::sync::atomic::AtomicBool,
    output: OutputState,
    output_eof: std::sync::atomic::AtomicBool,
    output_finished: tokio::sync::Notify,
    size: Mutex<TermSize>,
}

impl Session {
    pub fn new(name: String, command: CommandSpec, cols: u16, rows: u16) -> Result<Arc<Self>> {
        Ok(Self::new_with_output(name, command, cols, rows)?.0)
    }
    pub(crate) fn new_with_output(
        name: String,
        command: CommandSpec,
        cols: u16,
        rows: u16,
    ) -> Result<(Arc<Self>, OutputSubscription)> {
        let (cols, rows) = clamp_size(cols, rows);
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(&command.program);
        for arg in &command.args {
            cmd.arg(arg.as_str());
        }
        if let Some(cwd) = &command.cwd {
            cmd.cwd(cwd);
        }
        for (key, value) in &command.env {
            cmd.env(key, value);
        }
        cmd.env("TERM", "xterm-256color");

        let child = pair.slave.spawn_command(cmd)?;
        let process_id = child.process_id();
        let child_killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let session = Arc::new(Self {
            name,
            cwd: command.effective_cwd(),
            command,
            created_at: now_millis(),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(Some(child)),
            child_killer: Mutex::new(child_killer),
            process_id,
            exit: Mutex::new(ExitState {
                code: None,
                ended_at: None,
                subscribers: Vec::new(),
            }),
            exit_signal: Condvar::new(),
            auto_status_response: std::sync::atomic::AtomicBool::new(true),
            output: OutputState::default(),
            output_eof: std::sync::atomic::AtomicBool::new(false),
            output_finished: tokio::sync::Notify::new(),
            size: Mutex::new(TermSize { cols, rows }),
        });

        let output = session.subscribe_output();
        let weak = Arc::downgrade(&session);
        thread::spawn(move || {
            // A read returns as soon as bytes are available; this is capacity,
            // not a batching delay for interactive input.
            let mut buf = [0u8; 65536];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };

                let Some(session) = weak.upgrade() else {
                    break;
                };
                session.on_pty_output(&buf[..n]);
                if cfg!(windows)
                    && session
                        .auto_status_response
                        .load(std::sync::atomic::Ordering::Relaxed)
                    && buf[..n].windows(4).any(|window| window == b"\x1b[6n")
                {
                    let _ = session.write_input(b"\x1b[1;1R");
                }
            }
            if let Some(session) = weak.upgrade() {
                session
                    .output_eof
                    .store(true, std::sync::atomic::Ordering::Release);
                session.output_finished.notify_one();
            }
        });

        let weak = Arc::downgrade(&session);
        thread::spawn(move || {
            let Some(session) = weak.upgrade() else {
                return;
            };
            let status = {
                let mut child = session.child.lock().unwrap();
                let Some(child) = child.as_mut() else {
                    return;
                };
                child.wait()
            };
            match status {
                Ok(status) => {
                    session.set_exit_code(status.exit_code());
                }
                Err(err) => {
                    eprintln!("pty {} wait error: {err}", session.name);
                }
            }
        });

        Ok((session, output))
    }
}

fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(1), rows.max(1))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
