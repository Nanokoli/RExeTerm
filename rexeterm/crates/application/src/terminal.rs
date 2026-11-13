//! One visible PTY preview, owned by the desktop rather than by a window redraw.
use anyhow::{anyhow, ensure, Result};
use rexeterm_appcore::{client::Client, protocol::terminal::ClientText, transport::Frame};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::{
    sync::{mpsc, watch, Mutex},
    task::JoinHandle,
    time::{timeout, Duration},
};

#[derive(Default)]
pub(crate) struct TerminalPreview {
    state: Mutex<Option<Session>>,
    stopped: std::sync::atomic::AtomicBool,
}
struct Session {
    id: String,
    commands: mpsc::Sender<Frame>,
    output: Arc<Mutex<mpsc::Receiver<Event>>>,
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}

enum Event {
    Data(Vec<u8>),
    Metadata(Value),
}

const MAX_CHUNK_BYTES: usize = 32768;
const MAX_POLL_BYTES: usize = 256 * 1024;

impl TerminalPreview {
    pub async fn open(
        &self,
        client: &Client,
        host: &str,
        pty: &str,
        cols: u16,
        rows: u16,
    ) -> Result<Value> {
        let mut state = self.state.lock().await;
        ensure!(
            !self.stopped.load(std::sync::atomic::Ordering::SeqCst),
            "应用正在退出"
        );
        if let Some(session) = state.take() {
            stop(session).await;
        }
        let id = format!("desktop-{}", uuid_suffix());
        let mut connection = client
            .open_terminal(Some(host), pty, &id, cols.clamp(2, 500), rows.clamp(2, 200))
            .await?;
        let (commands, mut input) = mpsc::channel::<Frame>(32);
        let (output, received) = mpsc::channel(64);
        let (cancel, mut cancelled) = watch::channel(false);
        let task = tokio::spawn(async move {
            let (writer, reader) = connection.link().halves();
            let send_input = async {
                while let Some(command) = input.recv().await {
                    timeout(Duration::from_secs(5), writer.send(command)).await??;
                }
                Ok::<_, anyhow::Error>(())
            };
            let receive_output = async {
                loop {
                    let event = match reader.recv().await? {
                        Some(Frame::Binary(bytes)) => {
                            // Keep output as bytes through the bounded binary IPC batch.
                            for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
                                output.send(Event::Data(chunk.to_vec())).await?;
                            }
                            continue;
                        }
                        Some(Frame::Text(text)) => Event::Metadata(serde_json::from_str(&text)?),
                        Some(Frame::Close) | None => break,
                    };
                    output.send(event).await?;
                }
                Ok::<_, anyhow::Error>(())
            };
            // Backpressure from the renderer must not stop keyboard input.
            // Ending either direction drops the other before closing the link.
            let result = tokio::select! {
                biased;
                _ = cancelled.changed() => Ok(()),
                result = send_input => result,
                result = receive_output => result,
            };
            if let Err(error) = result {
                let _ = output.try_send(Event::Metadata(
                    json!({"type":"error","message":error.to_string()}),
                ));
            }
            let _ = timeout(Duration::from_secs(1), connection.link().close()).await;
        });
        *state = Some(Session {
            id: id.clone(),
            commands,
            output: Arc::new(Mutex::new(received)),
            cancel,
            task,
        });
        Ok(json!({"id":id,"host":host,"pty":pty}))
    }

    pub async fn send(&self, id: &str, frame: Frame) -> Result<()> {
        let commands = {
            let state = self.state.lock().await;
            let session = state.as_ref().ok_or_else(|| anyhow!("终端已关闭"))?;
            ensure!(session.id == id, "终端会话已切换");
            session.commands.clone()
        };
        timeout(Duration::from_secs(2), commands.send(frame))
            .await?
            .map_err(|_| anyhow!("终端通道已结束"))
    }

    pub async fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<()> {
        self.send(
            id,
            Frame::Text(serde_json::to_string(&ClientText::Resize {
                cols: cols.clamp(2, 500),
                rows: rows.clamp(2, 200),
            })?),
        )
        .await
    }

    pub async fn poll(&self, id: &str) -> Result<Vec<u8>> {
        let output = {
            let state = self.state.lock().await;
            let session = state.as_ref().ok_or_else(|| anyhow!("终端已关闭"))?;
            ensure!(session.id == id, "终端会话已切换");
            session.output.clone()
        };
        // Wait for output without holding the session lock: input, resize,
        // switching previews and shutdown can proceed during an idle poll.
        let mut output = output.lock().await;
        let first = timeout(Duration::from_millis(100), output.recv()).await;
        let mut packet = b"RTP1\0".to_vec();
        let mut data = Vec::new();
        let mut bytes = 0;
        if let Ok(Some(first)) = first {
            let mut event = first;
            for index in 0..64 {
                match event {
                    Event::Data(chunk) => {
                        bytes += chunk.len();
                        data.extend(chunk);
                    }
                    Event::Metadata(value) => {
                        flush_data(&mut packet, &mut data)?;
                        append_record(&mut packet, 1, &serde_json::to_vec(&value)?)?;
                    }
                }
                if bytes >= MAX_POLL_BYTES || index == 63 {
                    break;
                }
                event = match output.try_recv() {
                    Ok(next) => next,
                    Err(_) => break,
                };
            }
        }
        flush_data(&mut packet, &mut data)?;
        packet[4] = u8::from(output.is_closed() && output.is_empty());
        Ok(packet)
    }

    pub async fn shutdown(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.close(None).await;
    }

    pub async fn close(&self, id: Option<&str>) {
        let mut state = self.state.lock().await;
        if id.is_none() || state.as_ref().is_some_and(|s| Some(s.id.as_str()) == id) {
            if let Some(session) = state.take() {
                stop(session).await;
            }
        }
    }
}
fn flush_data(packet: &mut Vec<u8>, data: &mut Vec<u8>) -> Result<()> {
    if !data.is_empty() {
        append_record(packet, 0, data)?;
        data.clear();
    }
    Ok(())
}
fn append_record(packet: &mut Vec<u8>, kind: u8, payload: &[u8]) -> Result<()> {
    let length = u32::try_from(payload.len())?;
    packet.push(kind);
    packet.extend_from_slice(&length.to_le_bytes());
    packet.extend_from_slice(payload);
    Ok(())
}
async fn stop(mut session: Session) {
    session.cancel.send_replace(true);
    if timeout(Duration::from_secs(2), &mut session.task)
        .await
        .is_err()
    {
        session.task.abort();
        let _ = session.task.await;
    }
}
fn uuid_suffix() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binary_poll_preserves_all_bytes_metadata_order_and_final_close() {
        let preview = TerminalPreview::default();
        let (commands, _) = mpsc::channel(1);
        let (send, output) = mpsc::channel(4);
        let (cancel, _) = watch::channel(false);
        let payload: Vec<u8> = (0..=255).collect();
        send.send(Event::Data(payload[..100].to_vec()))
            .await
            .unwrap();
        send.send(Event::Data(payload[100..].to_vec()))
            .await
            .unwrap();
        send.send(Event::Metadata(json!({"type":"meta","role":"viewer"})))
            .await
            .unwrap();
        send.send(Event::Data(b"\x1b[31m\0\xff".to_vec()))
            .await
            .unwrap();
        drop(send);
        *preview.state.lock().await = Some(Session {
            id: "test".into(),
            commands,
            output: Arc::new(Mutex::new(output)),
            cancel,
            task: tokio::spawn(async {}),
        });
        assert!(preview.poll("stale").await.is_err());
        let packet = preview.poll("test").await.unwrap();
        assert_eq!(&packet[..5], b"RTP1\x01");
        assert_eq!(&packet[5..10], &[0, 0, 1, 0, 0]);
        assert_eq!(&packet[10..266], payload);
        assert_eq!(packet[266], 1);
        let length = u32::from_le_bytes(packet[267..271].try_into().unwrap()) as usize;
        assert_eq!(
            serde_json::from_slice::<Value>(&packet[271..271 + length]).unwrap(),
            json!({"type":"meta","role":"viewer"})
        );
        assert_eq!(&packet[271 + length..], b"\0\x07\0\0\0\x1b[31m\0\xff");
        assert_eq!(preview.poll("test").await.unwrap(), b"RTP1\x01");
        preview.shutdown().await;
    }
}
