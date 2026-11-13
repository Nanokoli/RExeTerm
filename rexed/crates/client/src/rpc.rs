//! Tool request/response data and client-side wait budgets, independent of links.
use crate::protocol::{is_soft_timeout_method, ExecutorRequest, ExecutorResponse};
use crate::transport::{Connector, Frame};
use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio::time::{timeout, Duration};

pub(super) const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;
const EXBASH_TIMEOUT_BUFFER_MS: u64 = 5_000;
pub(super) async fn call_link(
    connector: &dyn Connector,
    url: &str,
    request: ExecutorRequest,
    call_timeout_ms: Option<u64>,
) -> Result<ExecutorResponse> {
    let request_id = request.id.clone();
    if let Some(call_timeout_ms) = call_timeout_ms {
        match timeout(
            Duration::from_millis(call_timeout_ms),
            call_link_inner(connector, url, request),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow!(
                "executor call timed out after {call_timeout_ms}ms for request {request_id}"
            )),
        }
    } else {
        call_link_inner(connector, url, request).await
    }
}

async fn call_link_inner(
    connector: &dyn Connector,
    url: &str,
    request: ExecutorRequest,
) -> Result<ExecutorResponse> {
    let mut link = connector.connect(url).await?;
    let response = exchange(&mut link, request).await;
    retire(link, None);
    response
}
async fn exchange(
    link: &mut crate::transport::Link,
    request: ExecutorRequest,
) -> Result<ExecutorResponse> {
    let request_id = request.id.clone();
    link.send(Frame::Text(serde_json::to_string(&request)?))
        .await?;
    if let Some(frame) = link.recv().await? {
        match frame {
            Frame::Text(text) => {
                let response = serde_json::from_str::<ExecutorResponse>(&text)?;
                return Ok(response);
            }
            Frame::Close => {}
            Frame::Binary(_) => return Err(anyhow!("unexpected binary frame in tool response")),
        }
    }
    Err(anyhow!(
        "executor closed before responding to request {request_id}"
    ))
}

pub(super) fn call_timeout_ms_for(request: &ExecutorRequest) -> Option<u64> {
    if request.method == "exbash" {
        let read_timeout = request
            .params
            .get("read_timeout")
            .and_then(Value::as_u64)
            .unwrap_or(10_000);
        return Some(read_timeout.saturating_add(EXBASH_TIMEOUT_BUFFER_MS));
    }
    if is_soft_timeout_method(&request.method) {
        let search_timeout = request
            .params
            .get("timeout")
            .and_then(Value::as_i64)
            .unwrap_or(10_000);
        if search_timeout == -1 {
            return None;
        }
        return Some((search_timeout.max(0) as u64).saturating_add(EXBASH_TIMEOUT_BUFFER_MS));
    }

    Some(DEFAULT_CALL_TIMEOUT_MS)
}

pub(super) async fn call_pooled(
    pool: &crate::transport::ChannelPool,
    session: Option<&super::session::ClientSession>,
    request: ExecutorRequest,
    timeout_ms: Option<u64>,
) -> Result<ExecutorResponse> {
    use crate::transport::Purpose;
    let purpose = match request.method.as_str() {
        "read" | "stat" | "glob" | "rg" => Purpose::Query,
        "exbash"
            if !matches!(
                request.params.get("mode").and_then(Value::as_str),
                Some("stop" | "remove" | "list")
            ) =>
        {
            Purpose::Pty
        }
        _ => Purpose::Control,
    };
    let request_id = request.id.clone();
    let call = async {
        let (mut link, lease) = pool.connect(purpose).await?;
        if let Some(session) = session {
            session.bind(&mut link).await?;
        }
        let response = exchange(&mut link, request).await;
        retire(link, Some(lease));
        response
    };
    if let Some(ms) = timeout_ms {
        timeout(Duration::from_millis(ms), call)
            .await
            .map_err(|_| anyhow!("executor call timed out after {ms}ms for request {request_id}"))?
    } else {
        call.await
    }
}

fn retire(mut link: crate::transport::Link, lease: Option<crate::transport::ChannelLease>) {
    tokio::spawn(async move {
        let _lease = lease;
        let _ = timeout(Duration::from_secs(1), link.close()).await;
    });
}
