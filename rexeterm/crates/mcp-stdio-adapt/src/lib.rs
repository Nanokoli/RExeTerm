//! Ordinary MCP over stdio. The embedding host supplies the state core.
use anyhow::Result;
use rexeterm_appcore::AppCore as Client;
use rexeterm_mcp_common::error_response;
pub use rexeterm_mcp_common::handle_mcp_message;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

pub async fn run_mcp_stdio_with_core(client: Client) -> Result<()> {
    run_mcp_stdio_io_with_core(
        client,
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
}

pub async fn run_mcp_stdio_io_with_core<R, W>(client: Client, reader: R, writer: W) -> Result<()>
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
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(message) => handle_mcp_message(&client, message).await,
                Err(err) => Some(error_response(
                    Value::Null,
                    -32700,
                    format!("parse error: {err}"),
                )),
            };
            write_mcp_response(stdout, response).await
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

async fn write_mcp_response<W>(
    stdout: Arc<Mutex<BufWriter<W>>>,
    response: Option<Value>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let Some(response) = response else {
        return Ok(());
    };
    let mut stdout = stdout.lock().await;
    stdout
        .write_all(serde_json::to_string(&response)?.as_bytes())
        .await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}
