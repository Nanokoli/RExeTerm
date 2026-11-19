//! One binary output reader and one virtual screen per daemon-owned PTY.
use super::{
    screen::{Screen, MAX_COLS, MAX_ROWS},
    screenshot, OUTPUT_BUFFER_BYTES,
};
use anyhow::{anyhow, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rexeterm_appcore::{protocol::terminal::ServerText, transport::Frame};
use rexeterm_application::Application;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    ops::Deref,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{watch, OwnedSemaphorePermit, Semaphore},
    time::{timeout, Instant},
};

const STORED_SCREENS: usize = 128;
const ACTIVE_STREAMS: usize = 64;
const SCREEN_TTL: Duration = Duration::from_secs(1800);
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_QUIET: Duration = Duration::from_millis(120);
type Key = (String, String);

pub(super) struct Terminals {
    entries: Mutex<HashMap<Key, Arc<Task>>>,
    slots: Arc<Semaphore>,
    png_slots: Arc<Semaphore>,
}

impl Default for Terminals {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
            slots: Arc::new(Semaphore::new(ACTIVE_STREAMS)),
            png_slots: Arc::new(Semaphore::new(2)),
        }
    }
}

struct Task {
    state: Mutex<TerminalState>,
    changed: watch::Sender<()>,
    cancel: watch::Sender<bool>,
}

struct TerminalState {
    metadata: Value,
    screen: Screen,
    bytes: VecDeque<u8>,
    end: u64,
    last_output: Option<Instant>,
    ready: bool,
    finished: Option<Instant>,
    capture: Capture,
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Capture {
    FromStart,
    LegacyInitial,
    LegacyTranscript,
    LiveOnly,
}

#[derive(Clone, Copy)]
enum View {
    Start,
    Screen,
    Replay(Option<u64>),
}

impl Terminals {
    fn slot(&self) -> Result<OwnedSemaphorePermit> {
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow!("daemon PTY stream capacity reached"))
    }

    pub fn reap(&self) {
        self.entries.lock().unwrap().retain(|_, task| {
            task.state
                .lock()
                .unwrap()
                .finished
                .is_none_or(|stop| stop.elapsed() < SCREEN_TTL)
        });
    }

    pub fn clear(&self, host: &str, id: &str) {
        if let Some(task) = self.get_and_remove(host, id) {
            task.cancel.send_replace(true);
        }
    }

    fn get_and_remove(&self, host: &str, id: &str) -> Option<Arc<Task>> {
        self.entries
            .lock()
            .unwrap()
            .remove(&(host.into(), id.into()))
    }

    fn get(&self, host: &str, id: &str) -> Option<Arc<Task>> {
        self.entries
            .lock()
            .unwrap()
            .get(&(host.into(), id.into()))
            .cloned()
    }

    pub fn note_exit(&self, host: &str, id: &str, metadata: &Value) {
        if let Some(task) = self.get(host, id) {
            task.note_exit(metadata);
        }
    }

    pub async fn start(
        &self,
        app: &Application,
        host: &str,
        mut params: Value,
        directory: Option<&str>,
        sinks: &Value,
    ) -> Result<Value> {
        for (key, limit) in [("cols", MAX_COLS), ("rows", MAX_ROWS)] {
            if let Some(size) = params.get(key) {
                ensure!(
                    size.as_u64()
                        .is_some_and(|size| (1..=u64::from(limit)).contains(&size)),
                    "PTY {key} must be between 1 and {limit}"
                );
            }
        }
        let slot = self.slot()?;
        let wait = read_wait(&params);
        if app
            .core()
            .selected_executor(Some(host))
            .await?
            .pty_output_stream
        {
            params["streamOutput"] = json!(true);
        }
        let reply = app
            .rpc_unrendered(Some(host), "exbash", params, directory)
            .await?;
        let id = reply["metadata"]["asyncID"]
            .as_str()
            .context("missing PTY task ID")?;
        let capture = if reply["metadata"]["ptyOutputStream"] == true {
            Capture::FromStart
        } else if reply["metadata"]["state"] == "stopped" {
            Capture::LegacyTranscript
        } else {
            Capture::LegacyInitial
        };
        let task = self.register(app, host, id, &reply, capture, slot)?;
        task.wait_ready().await;
        if capture == Capture::LegacyInitial
            && reply["output"]["text"].as_str().is_none_or(str::is_empty)
        {
            task.wait_snapshot(wait, 0, false).await;
        }
        let view = if sinks["tui"] == true || sinks["png"] == true {
            View::Screen
        } else {
            View::Start
        };
        if capture == Capture::FromStart {
            task.wait_snapshot(wait, 0, matches!(view, View::Start))
                .await;
        }
        self.snapshot(task, view, sinks, &reply).await
    }

    pub async fn read(
        &self,
        app: &Application,
        host: &str,
        id: &str,
        args: &Value,
    ) -> Result<Value> {
        let task = self.find_or_observe(app, host, id).await?;
        task.wait_ready().await;
        let view = if args["from_start"] == true {
            View::Replay(None)
        } else if let Some(since) = args["since"].as_u64() {
            View::Replay(Some(since))
        } else {
            View::Screen
        };
        let after = match view {
            View::Replay(Some(offset)) => offset,
            _ => 0,
        };
        let wait = Duration::from_millis(args["wait_ms"].as_u64().unwrap_or(0).min(10000));
        let rendering = args["terminal"]["tui"] == true || args["terminal"]["png"] == true;
        if rendering {
            task.wait_snapshot(wait, after, false).await;
        } else {
            task.wait(wait, |state| state.end > after || state.finished.is_some())
                .await;
        }
        app.mark_task_read(host, id);
        self.snapshot(task, view, &args["terminal"], &Value::Null)
            .await
    }

    pub async fn input(
        &self,
        app: &Application,
        host: &str,
        mut params: Value,
        directory: Option<&str>,
        sinks: &Value,
    ) -> Result<Value> {
        let id = params["asyncID"]
            .as_str()
            .context("missing PTY task ID")?
            .to_owned();
        let task = self.find_or_observe(app, host, &id).await?;
        task.wait_ready().await;
        let wait = read_wait(&params);
        let before = task.state.lock().unwrap().end;
        // Input stays on the existing exbash path (including remote files and
        // control policy); only the binary stream feeds the screen.
        params["read_timeout"] = json!(0);
        let reply = match app
            .rpc_unrendered(Some(host), "exbash", params, directory)
            .await
        {
            Ok(reply) => {
                task.note_exit(&reply["metadata"]);
                reply
            }
            Err(error) if error.to_string() == "control lost: someone attached: unknown" => {
                // Older REs can report control loss when the just-written key
                // exits the application. Confirm exit before accepting the input.
                task.wait_finished(Duration::from_millis(300)).await;
                if task.state.lock().unwrap().metadata["state"] != "stopped" {
                    return Err(error);
                }
                Value::Null
            }
            Err(error) => return Err(error),
        };
        task.wait_snapshot(wait, before, false).await;
        self.snapshot(task, View::Screen, sinks, &reply).await
    }

    async fn find_or_observe(&self, app: &Application, host: &str, id: &str) -> Result<Arc<Task>> {
        if let Some(task) = self.get(host, id) {
            return Ok(task);
        }
        let slot = self.slot()?;
        // Tasks started elsewhere have no screen to recover; attach live and
        // describe the resulting snapshot as partial.
        let reply = app
            .rpc_unrendered(
                Some(host),
                "exbash",
                json!({"mode":"attach","asyncID":id,"read_timeout":0}),
                None,
            )
            .await?;
        self.register(app, host, id, &reply, Capture::LiveOnly, slot)
    }

    fn register(
        &self,
        app: &Application,
        host: &str,
        id: &str,
        reply: &Value,
        capture: Capture,
        slot: OwnedSemaphorePermit,
    ) -> Result<Arc<Task>> {
        let key = (host.to_owned(), id.to_owned());
        let mut entries = self.entries.lock().unwrap();
        if let Some(task) = entries.get(&key) {
            return Ok(task.clone());
        }
        if entries.len() >= STORED_SCREENS {
            let oldest = entries
                .iter()
                .filter_map(|(key, task)| task.state.lock().unwrap().finished.map(|end| (key, end)))
                .min_by_key(|(_, end)| *end)
                .map(|(key, _)| key.clone())
                .context("daemon PTY screen capacity reached")?;
            entries.remove(&oldest);
        }
        let mut metadata = reply["metadata"].clone();
        let size = &metadata["terminalOutput"];
        let cols = size["cols"]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(80);
        let rows = size["rows"]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(24);
        for key in ["ptyOutputStream", "output", "rawPretty", "wrote", "source"] {
            if let Some(fields) = metadata.as_object_mut() {
                fields.remove(key);
            }
        }
        if metadata["state"].is_null() {
            metadata["state"] = json!("running");
        }
        metadata["asyncID"] = json!(id);
        metadata["host"] = json!(host);
        let no_stream = capture != Capture::FromStart && metadata["state"] == "stopped";
        let mut state = TerminalState {
            metadata,
            screen: Screen::new(cols, rows),
            bytes: VecDeque::new(),
            end: 0,
            last_output: None,
            ready: no_stream,
            finished: no_stream.then(Instant::now),
            capture,
            error: None,
        };
        if capture != Capture::FromStart {
            state.feed(
                reply["output"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
        if no_stream {
            state.screen.finish();
        }
        let task = Arc::new(Task {
            state: Mutex::new(state),
            changed: watch::channel(()).0,
            cancel: watch::channel(false).0,
        });
        entries.insert(key.clone(), task.clone());
        if !no_stream {
            let client = app.core().deref().clone();
            let task = task.clone();
            let mut cancel = task.cancel.subscribe();
            tokio::spawn(async move {
                let _slot = slot;
                let result = tokio::select! {
                    biased;
                    _ = cancel.changed() => Ok(()),
                    result = observe(task.clone(), client, key, capture, (cols, rows)) => result,
                };
                task.finish(result);
            });
        }
        Ok(task)
    }

    async fn snapshot(
        &self,
        task: Arc<Task>,
        view: View,
        sinks: &Value,
        reply: &Value,
    ) -> Result<Value> {
        let raw = sinks["raw"] == true;
        let png = sinks["png"] == true;
        let tui = sinks["tui"] == true;
        // Limit allocation and rasterization even with simultaneous IPC callers.
        let png_slot = if png {
            Some(self.png_slots.clone().acquire_owned().await?)
        } else {
            None
        };
        let (mut meta, mut output, viewport) = {
            let state = task.state.lock().unwrap();
            let (cols, rows) = state.screen.size();
            let mut meta = state.metadata.clone();
            let view = match view {
                View::Start if meta["state"] == "stopped" && state.finished.is_some() => {
                    View::Replay(None)
                }
                View::Start => View::Screen,
                view => view,
            };
            let is_screen = matches!(view, View::Screen);
            let start = state.end - state.bytes.len() as u64;
            let from = match view {
                View::Replay(Some(since)) => since.clamp(start, state.end),
                _ => start,
            };
            let viewport = (is_screen || tui || png).then(|| state.screen.viewport());
            let tail = (raw || !is_screen).then(|| {
                state
                    .bytes
                    .iter()
                    .skip((from - start) as usize)
                    .copied()
                    .collect::<Vec<_>>()
            });
            let text = if is_screen {
                viewport.as_ref().unwrap().text()
            } else {
                String::from_utf8_lossy(tail.as_deref().unwrap()).into_owned()
            };
            let mut output = json!({"text":text});
            if raw {
                output["rawBase64"] = json!(BASE64.encode(tail.as_deref().unwrap()));
            }
            if tui {
                output["ansi"] = json!(viewport.as_ref().unwrap().ansi());
            }
            meta["terminalOutput"] = json!({
                "format": if is_screen {"client-screen-v1"} else {"raw-pty-v1"},
                "view": if is_screen {"screen"} else {"stream"},
                "cols": cols,
                "rows": rows,
                "capture": match state.capture {
                    Capture::FromStart => "from-start",
                    Capture::LegacyInitial => "legacy-initial-plus-live",
                    Capture::LegacyTranscript => "legacy-transcript",
                    Capture::LiveOnly => "live-only",
                },
            });
            if let Some(error) = &state.error {
                meta["terminalOutput"]["error"] = json!(error);
            }
            meta["detached"] = json!(state.finished.is_none());
            meta["from"] = json!(from);
            meta["next_offset"] = json!(state.end);
            meta["buffered_bytes"] = json!(state.bytes.len());
            meta["dropped_bytes"] = json!(start);
            meta["outputTruncated"] = json!(start > 0 || meta["outputTruncated"] == true);
            (meta, output, viewport)
        };
        for key in ["wrote", "source"] {
            if let Some(value) = reply["metadata"].get(key) {
                meta[key] = value.clone();
            }
        }
        if let Some(message) = reply["output"]["message"]
            .as_str()
            .filter(|msg| !msg.is_empty())
        {
            output["message"] = json!(message);
        }
        if let Some(slot) = png_slot {
            let png = tokio::task::spawn_blocking(move || {
                let _slot = slot;
                let png = screenshot::screen_png(&viewport.unwrap())?;
                ensure!(
                    png.len() <= 3 * 1024 * 1024,
                    "PTY screenshot exceeds the IPC budget"
                );
                Ok::<_, anyhow::Error>(BASE64.encode(png))
            })
            .await??;
            output["pngBase64"] = json!(png);
        }
        Ok(json!({"metadata":meta,"output":output}))
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        for task in self.entries.get_mut().unwrap().values() {
            task.cancel.send_replace(true);
        }
    }
}

fn read_wait(params: &Value) -> Duration {
    Duration::from_millis(params["read_timeout"].as_u64().unwrap_or(10000).min(10000))
}

impl TerminalState {
    fn feed(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.end += data.len() as u64;
        self.last_output = Some(Instant::now());
        self.screen.feed(data);
        if data.len() >= OUTPUT_BUFFER_BYTES {
            self.bytes.clear();
            self.bytes.extend(&data[data.len() - OUTPUT_BUFFER_BYTES..]);
        } else {
            let overflow = (self.bytes.len() + data.len()).saturating_sub(OUTPUT_BUFFER_BYTES);
            self.bytes.drain(..overflow);
            self.bytes.extend(data);
        }
    }
}

impl Task {
    async fn wait_ready(&self) {
        self.wait(OPEN_TIMEOUT * 2 + Duration::from_millis(100), |state| {
            state.ready || state.finished.is_some()
        })
        .await;
    }

    async fn wait_finished(&self, wait: Duration) {
        self.wait(wait, |state| state.finished.is_some()).await;
    }

    async fn wait_snapshot(&self, wait: Duration, after: u64, prefer_exit: bool) {
        if prefer_exit {
            self.wait_finished(wait).await;
            return;
        }
        let mut changed = self.changed.subscribe();
        let deadline = Instant::now() + wait;
        loop {
            let wake = {
                let state = self.state.lock().unwrap();
                if state.finished.is_some() {
                    return;
                }
                if state.end > after && state.metadata["state"] != "stopped" {
                    state
                        .last_output
                        .map_or(deadline, |last| deadline.min(last + FRAME_QUIET))
                } else {
                    deadline
                }
            };
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(wake) => return,
                result = changed.changed() => if result.is_err() { return; }
            }
        }
    }

    async fn wait(&self, wait: Duration, condition: impl Fn(&TerminalState) -> bool) {
        let mut changed = self.changed.subscribe();
        let _ = timeout(wait, async {
            while !condition(&self.state.lock().unwrap()) {
                if changed.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
    }

    fn note_exit(&self, metadata: &Value) {
        if metadata["state"] == "stopped" {
            let mut state = self.state.lock().unwrap();
            state.metadata["state"] = json!("stopped");
            for key in ["endedAt", "exitCode"] {
                if let Some(value) = metadata.get(key) {
                    state.metadata[key] = value.clone();
                }
            }
        }
    }

    fn finish(&self, result: Result<()>) {
        let mut state = self.state.lock().unwrap();
        state.screen.finish();
        state.finished = Some(Instant::now());
        state.error = result.err().map(|error| format!("{error:#}"));
        if state.metadata["state"] != "stopped" {
            state.metadata["state"] = json!("unknown");
            if state.error.is_none() {
                state.error = Some("PTY output stream closed without an exit status".into());
            }
        }
        self.changed.send_replace(());
    }
}

async fn observe(
    task: Arc<Task>,
    client: rexeterm_appcore::client::Client,
    (host, id): Key,
    capture: Capture,
    (cols, rows): (u16, u16),
) -> Result<()> {
    let mut connection = timeout(OPEN_TIMEOUT, async {
        if capture == Capture::FromStart {
            client.observe_terminal(Some(&host), &id).await
        } else {
            client
                .open_terminal(Some(&host), &id, "rexetermd", cols, rows)
                .await
        }
    })
    .await
    .context("opening PTY output stream timed out")??;
    let link = connection.link();
    let mut first = true;
    loop {
        let frame = if first {
            first = false;
            timeout(OPEN_TIMEOUT, link.recv())
                .await
                .context("PTY output stream did not acknowledge attach")??
        } else {
            link.recv().await?
        };
        match frame {
            Some(Frame::Binary(data)) => {
                task.state.lock().unwrap().feed(&data);
                task.changed.send_replace(());
            }
            Some(Frame::Text(text)) => match serde_json::from_str::<ServerText>(&text)? {
                ServerText::Meta {
                    cols,
                    rows,
                    exit_code,
                    ..
                } => {
                    let mut state = task.state.lock().unwrap();
                    state.screen.resize(cols, rows);
                    if let Some(code) = exit_code {
                        state.metadata["state"] = json!("stopped");
                        if state.metadata["exitCode"].is_null() {
                            state.metadata["exitCode"] = json!(code);
                        }
                    }
                    state.ready = true;
                    task.changed.send_replace(());
                }
                ServerText::Error { message } => return Err(anyhow!("{message}")),
                _ => {}
            },
            Some(Frame::Close) | None => break,
        }
    }
    Ok(())
}
