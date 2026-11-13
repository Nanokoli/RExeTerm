//! Data dispatch and session binding over established physical links.
use super::Executor;
use crate::{
    protocol::{
        management::{ChannelKind, ChannelMetadata, SessionMessage, SessionOwner},
        ExecutorRequest, ExecutorResponse,
    },
    shell_manager::ShellManager,
    transport::{Frame, Link},
};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use tokio::time::{timeout, Duration};
pub async fn serve_executor_link(
    link: Link,
    peer: String,
    executor: Executor,
    manager: ShellManager,
) -> Result<()> {
    serve_executor_link_on(link, peer, "injected", executor, manager).await
}
pub(super) async fn serve_executor_link_on(
    mut link: Link,
    peer: String,
    transport: &str,
    executor: Executor,
    manager: ShellManager,
) -> Result<()> {
    executor.registry.maintain(manager.clone());
    let executor = executor.with_shell_manager(manager.clone());
    let mut link_event=executor.registry.events.span("link.opened","link.closed",None,serde_json::json!({"linkId":format!("link-{:032x}",rand::random::<u128>()),"peer":peer,"transport":transport}));
    let principal = match &executor.authentication {
        Some(authentication) => Some(
            authentication
                .authenticate(&mut link, &executor.registry.events, &peer, transport)
                .await?,
        ),
        None => None,
    };
    let Some(Frame::Text(mut text)) = timeout(Duration::from_secs(30), link.recv()).await?? else {
        return Ok(());
    };
    let mut owner: Option<SessionOwner> = None;
    let mut _link_guard = None;
    let mut metadata = ChannelMetadata::default();
    if let Ok(message) = serde_json::from_str::<SessionMessage>(&text) {
        let mut bound_owner = None;
        let mut keep_data = false;
        let mut closing = None;
        let result = match message {
            SessionMessage::Open { mut client } => {
                if let Some(p) = &principal {
                    anyhow::ensure!(
                        p.username == client.username,
                        "authenticated username mismatch"
                    );
                    client.username.clone_from(&p.username);
                }
                executor
                    .registry
                    .open(client, peer.clone(), transport.into())
                    .map(|(o, credential)| {
                        bound_owner = Some(o.clone());
                        SessionMessage::Opened {
                            owner: o,
                            credential,
                        }
                    })
            }
            SessionMessage::OpenClient {
                mut client,
                credential,
            } => {
                if let Some(p) = &principal {
                    anyhow::ensure!(
                        p.username == client.username,
                        "authenticated username mismatch"
                    );
                    client.username.clone_from(&p.username);
                }
                executor
                    .registry
                    .open_client(client, credential, peer.clone(), transport.into())
                    .map(|(o, credential)| {
                        bound_owner = Some(o.clone());
                        SessionMessage::ClientOpened {
                            owner: o,
                            credential,
                        }
                    })
            }
            SessionMessage::Bind {
                credential,
                metadata: requested,
            } => (|| {
                if let Some(requested) = requested {
                    requested.validate().map_err(|e| anyhow!(e))?;
                    metadata = requested.normalized();
                }
                executor.registry.bind(&credential)
            })()
            .map(|o| {
                bound_owner = Some(o);
                keep_data = true;
                SessionMessage::Ok
            }),
            SessionMessage::Ping { credential } => executor.registry.bind(&credential).map(|o| {
                bound_owner = Some(o);
                SessionMessage::Ok
            }),
            SessionMessage::Close { credential } => executor.registry.bind(&credential).map(|o| {
                bound_owner = Some(o);
                closing = Some(credential);
                SessionMessage::Ok
            }),
            _ => Err(anyhow!("expected session open/bind/ping/close")),
        };
        if let (Some(p), Some(o)) = (&principal, &bound_owner) {
            anyhow::ensure!(p.username == o.username, "authenticated username mismatch");
        }
        if let Some(o) = &bound_owner {
            link_event.bind(o.clone());
            _link_guard = executor.registry.track_link(o, link.traffic());
        }
        if let Some(credential) = closing {
            executor.registry.close(&credential)?;
        }
        let failed = result.is_err();
        let reply = result.unwrap_or_else(|e| SessionMessage::Error {
            message: e.to_string(),
        });
        link.send(Frame::Text(serde_json::to_string(&reply)?))
            .await?;
        if failed || !keep_data {
            return Ok(());
        }
        owner = bound_owner;
    }
    let mut cancellation = owner
        .as_ref()
        .map(|o| executor.registry.cancellation(o))
        .transpose()?;
    let business = async {
        if owner.is_some() {
            let Some(Frame::Text(next)) = timeout(Duration::from_secs(30), link.recv()).await??
            else {
                return Ok(());
            };
            text = next;
        }
        if owner.is_none()
            && !matches!(serde_json::from_str::<ExecutorRequest>(&text), Ok(r) if r.method == "_executor_info")
        {
            reject_unbound(&mut link, &text).await?;
            return Ok(());
        }
        serve_data(
            link, text, peer, transport, executor, manager, owner, metadata,
        )
        .await
    };
    tokio::pin!(business);
    if let Some(cancelled) = cancellation.as_mut() {
        if *cancelled.borrow() {
            return Ok(());
        }
        tokio::select! { biased; _ = cancelled.changed() => Ok(()), result = &mut business => result }
    } else {
        business.await
    }
}
#[allow(clippy::too_many_arguments)]
async fn serve_data(
    mut link: Link,
    text: String,
    peer: String,
    transport: &str,
    executor: Executor,
    manager: ShellManager,
    owner: Option<SessionOwner>,
    metadata: ChannelMetadata,
) -> Result<()> {
    let permissions = match (&executor.authentication, owner.as_ref()) {
        (Some(auth), Some(owner)) => auth.permissions(&owner.username).await?,
        (Some(_), None) => crate::auth::Permissions::default(),
        _ => crate::auth::Permissions::all(),
    };
    if let Ok(crate::protocol::file::FileMessage::Open { request }) = serde_json::from_str(&text) {
        let target = match &request {
            crate::protocol::file::FileRequest::Download { path, .. }
            | crate::protocol::file::FileRequest::Upload { path, .. } => path.display().to_string(),
        };
        let mut activity=executor.registry.events.span("file.started","file.finished",owner.clone(),serde_json::json!({"path":target,"direction":match &request {crate::protocol::file::FileRequest::Upload{..}=>"upload",_=>"download"},"peer":peer,"transport":transport}));
        let allowed = match request {
            crate::protocol::file::FileRequest::Download { .. } => permissions.files_read,
            crate::protocol::file::FileRequest::Upload { .. } => permissions.files_write,
        };
        if !allowed {
            activity.set("error", serde_json::json!("permission denied"));
            activity.finish("error");
            crate::data::file::send_message(
                &mut link,
                &crate::protocol::file::FileMessage::Error {
                    message: "permission denied".into(),
                },
            )
            .await?;
            return Ok(());
        }
        let Ok(_slot) = executor.file_slots.clone().try_acquire_owned() else {
            activity.set("error", serde_json::json!("file transfer capacity reached"));
            activity.finish("error");
            crate::data::file::send_message(
                &mut link,
                &crate::protocol::file::FileMessage::Error {
                    message: "file transfer capacity reached".into(),
                },
            )
            .await?;
            return Ok(());
        };
        let target = match &request {
            crate::protocol::file::FileRequest::Download { path, .. }
            | crate::protocol::file::FileRequest::Upload { path, .. } => path.display().to_string(),
        };
        let _channel = executor.registry.channel(
            ChannelKind::File,
            owner.clone(),
            &peer,
            transport,
            target.clone(),
            metadata.clone(),
        );

        return crate::file_transfer::serve_file_transfer(
            link,
            request,
            executor.write_lock.clone(),
            activity,
        )
        .await;
    }
    if let Ok(crate::protocol::forward::StreamMessage::Open { target }) =
        serde_json::from_str(&text)
    {
        let mut activity=executor.registry.events.span("forward.started","forward.finished",owner.clone(),serde_json::json!({"target":format!("{}://{}",target.kind,target.address),"peer":peer,"transport":transport}));
        if !permissions.forward {
            activity.set("error", serde_json::json!("permission denied"));
            activity.finish("error");
            link.send(Frame::Text(serde_json::to_string(
                &crate::protocol::forward::StreamMessage::Error {
                    message: "permission denied".into(),
                },
            )?))
            .await?;
            return Ok(());
        }
        let _slot = executor.stream_slots.clone().acquire_owned().await?;
        let _channel = executor.registry.channel(
            ChannelKind::Forward,
            owner.clone(),
            &peer,
            transport,
            format!("{}://{}", target.kind, target.address),
            metadata.clone(),
        );
        let result = crate::data::serve_stream(link, target, executor.stream_opener.as_ref()).await;
        if let Err(error) = &result {
            activity.set("error", serde_json::json!(error.to_string()));
        }
        activity.finish(if result.is_ok() { "ok" } else { "error" });
        return result;
    }
    if serde_json::from_str::<ExecutorRequest>(&text).is_ok() {
        send_executor_response(
            &mut link,
            &executor,
            &text,
            owner.clone(),
            &peer,
            transport,
            &metadata,
        )
        .await?;
        while let Some(frame) = link.recv().await? {
            match frame {
                Frame::Text(text) => {
                    send_executor_response(
                        &mut link,
                        &executor,
                        &text,
                        owner.clone(),
                        &peer,
                        transport,
                        &metadata,
                    )
                    .await?
                }
                Frame::Close => break,
                Frame::Binary(_) => bail!("unexpected binary frame in tool connection"),
            }
        }
        return Ok(());
    }
    let is_pty = matches!(
        serde_json::from_str::<crate::protocol::terminal::ClientText>(&text),
        Ok(crate::protocol::terminal::ClientText::Hello { .. })
    );
    let _channel = is_pty.then(|| {
        executor.registry.channel(
            ChannelKind::Pty,
            owner.clone(),
            &peer,
            transport,
            serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("pty").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default(),
            metadata.clone(),
        )
    });
    if !permissions.pty_view {
        link.send(Frame::Text(serde_json::to_string(
            &crate::protocol::terminal::ServerText::Error {
                message: "permission denied".into(),
            },
        )?))
        .await?;
        return Ok(());
    }
    let (sender, receiver) = link.split();
    crate::terminal::handle_first_text_with_policy(
        text,
        sender,
        receiver,
        peer,
        manager,
        owner,
        !permissions.pty_control,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
async fn send_executor_response(
    link: &mut Link,
    executor: &Executor,
    text: &str,
    owner: Option<SessionOwner>,
    peer: &str,
    transport: &str,
    metadata: &ChannelMetadata,
) -> Result<()> {
    let response = match serde_json::from_str::<ExecutorRequest>(text) {
        Ok(request) if owner.is_none() && request.method != "_executor_info" => {
            ExecutorResponse::err(request.id, None, "client session required")
        }
        Ok(request) => {
            let _channel = (request.method == "exbash"
                && request.params.get("mode").and_then(Value::as_str) == Some("attach")
                && !request
                    .params
                    .get("asyncID")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("stdio-")))
            .then(|| {
                executor.registry.channel(
                    ChannelKind::Pty,
                    owner.clone(),
                    peer,
                    transport,
                    request
                        .params
                        .get("asyncID")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    metadata.clone(),
                )
            });
            executor.handle_owned(request, owner, Some(peer)).await
        }
        Err(err) => ExecutorResponse::err(Value::Null, None, format!("invalid request: {err}")),
    };
    link.send(Frame::Text(serde_json::to_string(&response)?))
        .await
}

async fn reject_unbound(link: &mut Link, text: &str) -> Result<()> {
    let message = "client session required".to_string();
    let response = if let Ok(request) = serde_json::from_str::<ExecutorRequest>(text) {
        serde_json::to_string(&ExecutorResponse::err(request.id, None, message))?
    } else if serde_json::from_str::<crate::protocol::file::FileMessage>(text).is_ok() {
        serde_json::to_string(&crate::protocol::file::FileMessage::Error { message })?
    } else if serde_json::from_str::<crate::protocol::forward::StreamMessage>(text).is_ok() {
        serde_json::to_string(&crate::protocol::forward::StreamMessage::Error { message })?
    } else {
        serde_json::to_string(&crate::protocol::terminal::ServerText::Error { message })?
    };
    link.send(Frame::Text(response)).await
}
