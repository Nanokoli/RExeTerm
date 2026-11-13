//! Terminal data messages and PTY adaptation, independent of physical links.
use crate::protocol::terminal::{AdminText, ClientText, ServerText};
use crate::shell_manager::{ShellManager, TerminalEvent};
use crate::transport::{Frame, Receiver, Sender};
use anyhow::{anyhow, Result};
use tokio::sync::mpsc;

struct TerminalClient {
    manager: ShellManager,
    sender: Sender,
    receiver: Receiver,
    peer_addr: String,
    id: String,
    pty: String,
    cols: u16,
    rows: u16,
    owner: Option<crate::protocol::management::SessionOwner>,
    force_read_only: bool,
}

pub(crate) async fn handle_first_text_with_policy(
    first_text: String,
    mut sender: Sender,
    receiver: Receiver,
    peer_addr: String,
    manager: ShellManager,
    owner: Option<crate::protocol::management::SessionOwner>,
    force_read_only: bool,
) -> Result<()> {
    if let Ok(admin) = serde_json::from_str::<AdminText>(&first_text) {
        let response = handle_admin(&manager, admin, owner.as_ref());
        send_response(&mut sender, response).await?;
        return Ok(());
    }

    let Ok(ClientText::Hello {
        id,
        pty,
        cols,
        rows,
    }) = serde_json::from_str::<ClientText>(&first_text)
    else {
        send_response(
            &mut sender,
            Ok(ServerText::Error {
                message: "expected ptyt hello or read-only admin request".to_string(),
            }),
        )
        .await?;
        return Ok(());
    };

    if let Err(error) = manager.terminal_read_only(&pty, owner.as_ref()) {
        send_response(&mut sender, Err(error)).await?;
        return Ok(());
    }
    handle_terminal_client(TerminalClient {
        manager,
        sender,
        receiver,
        peer_addr,
        id,
        pty,
        cols,
        rows,
        owner,
        force_read_only,
    })
    .await
}

async fn handle_terminal_client(client: TerminalClient) -> Result<()> {
    let TerminalClient {
        manager,
        sender,
        mut receiver,
        peer_addr,
        id,
        pty,
        cols,
        rows,
        owner,
        force_read_only,
    } = client;
    let Some(session) = manager.core().session(&pty) else {
        return Err(anyhow!("pty {pty} does not exist"));
    };

    let token = rand_token();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let attachment = manager.register_client_with_policy(
        &pty,
        id,
        token,
        cols,
        rows,
        event_tx,
        peer_addr,
        owner,
        force_read_only,
    )?;
    let id = attachment.id.clone();
    // Live-only fanout: no history or screen replay on connection.
    // The writer and reader run independently so output cannot block input.
    let (tx, mut errors) = mpsc::unbounded_channel::<Frame>();
    let mut output = session.subscribe_output();
    manager.broadcast_meta(&pty);
    let mut writer_task = tokio::spawn(async move {
        let mut sender = sender;
        loop {
            let frame = tokio::select! {
                biased;
                event = event_rx.recv() => match event {
                    Some(TerminalEvent::Metadata { id, pty, role, read_only, cols, rows, exit_code }) =>
                        Frame::Text(serde_json::to_string(&ServerText::Meta { id, pty, role, read_only, cols, rows, exit_code })?),
                    Some(TerminalEvent::Closed) | None => break,
                },
                error = errors.recv() => match error { Some(frame) => frame, None => break },
                bytes = output.recv() => match bytes {
                    Ok(bytes) => Frame::Binary(bytes),
                    Err(_) if output.is_closed() => break,
                    Err(error) => {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), send_response(&mut sender, Err(error))).await;
                        break;
                    }
                },
            };
            tokio::time::timeout(std::time::Duration::from_secs(5), sender.send(frame)).await??;
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), sender.send(Frame::Close))
            .await;
        Ok::<_, anyhow::Error>(())
    });
    let _cleanup = TerminalCleanup {
        manager: manager.clone(),
        id: id.clone(),
        pty: pty.clone(),
        token,
        tasks: vec![writer_task.abort_handle()],
    };
    let mut result = Ok(());
    loop {
        let msg = tokio::select! {
            _ = &mut writer_task => break,
            msg = receiver.recv() => match msg { Ok(Some(msg)) => msg, _ => break },
        };
        if matches!(msg, Frame::Close) {
            break;
        }
        result = handle_client_message(&manager, &attachment, &tx, &pty, &id, msg).await;
        if result.is_err() {
            break;
        }
    }

    result
}

struct TerminalCleanup {
    manager: ShellManager,
    id: String,
    pty: String,
    token: u64,
    tasks: Vec<tokio::task::AbortHandle>,
}
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        self.manager.remove_client(&self.pty, &self.id, self.token);
        self.manager.broadcast_meta(&self.pty);
        for task in &self.tasks {
            task.abort();
        }
    }
}
fn handle_admin(
    manager: &ShellManager,
    msg: AdminText,
    owner: Option<&crate::protocol::management::SessionOwner>,
) -> Result<ServerText> {
    match msg {
        AdminText::List => Ok(ServerText::Sessions {
            sessions: manager
                .list()
                .into_iter()
                .filter(|s| owner.is_none_or(|o| manager.is_owned_by(&s.pty, o)))
                .collect(),
        }),
        AdminText::Detail { pty } => {
            manager.require_owner(&pty, owner)?;
            Ok(ServerText::Session {
                session: manager.detail(&pty)?,
            })
        }
        _ => Ok(ServerText::Error {
            message: "remote admin mutation is disabled".to_string(),
        }),
    }
}

async fn handle_client_message(
    manager: &ShellManager,
    attachment: &super::shell_manager::attachments::TerminalAttachment,
    tx: &mpsc::UnboundedSender<Frame>,
    pty: &str,
    id: &str,
    msg: Frame,
) -> Result<()> {
    match msg {
        Frame::Binary(data) => {
            if let Err(error) = attachment.write(&data) {
                send_error_tx(tx, &error.to_string());
            }
            Ok(())
        }
        Frame::Text(text) => match serde_json::from_str::<ClientText>(&text) {
            Ok(ClientText::Resize { cols, rows }) => {
                let result = attachment.resize(cols, rows);
                if result.is_ok() {
                    manager.broadcast_meta(pty);
                }
                if let Err(error) = result {
                    send_error_tx(tx, &error.to_string());
                }
                Ok(())
            }
            Ok(ClientText::RequestControl) => {
                let owner = manager
                    .attached_clients(pty)
                    .into_iter()
                    .find(|c| c.id == id)
                    .and_then(|c| c.owner);
                manager.events.emit(
                    "pty.control_requested",
                    owner.as_ref(),
                    serde_json::json!({"programId":pty,"controller":id}),
                );
                match attachment.request_control() {
                    Ok(()) => {
                        manager.broadcast_meta(pty);
                        Ok(())
                    }
                    Err(error) => {
                        send_error_tx(tx, &error.to_string());
                        Ok(())
                    }
                }
            }
            Ok(ClientText::Hello { .. }) => Ok(()),
            Err(err) => {
                send_error_tx(tx, &format!("bad client message: {err}"));
                Ok(())
            }
        },
        Frame::Close => Ok(()),
    }
}

async fn send_response(sender: &mut Sender, response: Result<ServerText>) -> Result<()> {
    let msg = match response {
        Ok(msg) => msg,
        Err(err) => ServerText::Error {
            message: err.to_string(),
        },
    };
    sender
        .send(Frame::Text(serde_json::to_string(&msg)?))
        .await?;
    Ok(())
}

fn send_error_tx(tx: &mpsc::UnboundedSender<Frame>, message: &str) {
    let msg = ServerText::Error {
        message: message.to_string(),
    };
    if let Ok(text) = serde_json::to_string(&msg) {
        let _ = tx.send(Frame::Text(text));
    }
}

fn rand_token() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
    NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
}
