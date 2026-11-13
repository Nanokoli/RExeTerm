//! Concurrent JSONL stdio adapter over the Client API.
use anyhow::Result;
use rexeterm_appcore::{
    protocol::{ExecutorRequest, ExecutorResponse},
    AppCore as Client,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::{
    io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    sync::Mutex,
    task::JoinSet,
};
pub type StdioRequest = ExecutorRequest;
pub type StdioResponse = ExecutorResponse;
pub async fn run_stdio_with_core(client: Client) -> Result<()> {
    run_stdio_io_with_core(
        client,
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
}

pub async fn run_stdio_io_with_core<R, W>(client: Client, reader: R, writer: W) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = reader;
    let stdout = Arc::new(Mutex::new(BufWriter::new(writer)));
    let mut tasks = JoinSet::new();

    loop {
        if tasks.len() >= rexeterm_adapter_common::MAX_IN_FLIGHT {
            if let Some(result) = tasks.join_next().await {
                result??;
            }
        }
        let Some(line) = rexeterm_adapter_common::read_line(&mut reader).await? else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }

        let client = client.clone();
        let stdout = stdout.clone();
        tasks.spawn(async move {
            let response = match serde_json::from_str::<StdioRequest>(&line) {
                Ok(request) => client.handle(request).await,
                Err(err) => ExecutorResponse::err(
                    Value::Null,
                    Some("client".to_string()),
                    format!("invalid request: {err}"),
                ),
            };
            write_stdio_response(stdout, response).await
        });

        while let Some(result) = tasks.try_join_next() {
            result??;
        }
    }

    while let Some(result) = tasks.join_next().await {
        result??;
    }

    Ok(())
}

async fn write_stdio_response<W>(
    stdout: Arc<Mutex<BufWriter<W>>>,
    response: StdioResponse,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let text = serde_json::to_string(&response)?;
    let mut stdout = stdout.lock().await;
    stdout.write_all(text.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}
