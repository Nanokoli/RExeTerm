//! Bounded file data exchange. No addresses, sockets or HTTP routes live here.
use anyhow::{anyhow, bail, Result};
use remote_executor_protocol::file::FileMessage;
use remote_executor_transport::{Frame, Link, LinkFuture};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{timeout, Duration};
pub const CHUNK_BYTES: usize = 64 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub trait TransferObserver: Send + Sync {
    /// Absolute bytes and expected total. Errors cancel this transfer.
    fn progress(&self, bytes: u64, total: u64) -> LinkFuture<'_, ()>;
}
#[derive(Debug)]
pub struct ContentDigest {
    pub bytes: u64,
    pub sha256: String,
}
fn digest(hasher: Sha256, bytes: u64) -> ContentDigest {
    ContentDigest {
        bytes,
        sha256: format!("sha256:{:x}", hasher.finalize()),
    }
}
pub async fn send_message(link: &mut Link, message: &FileMessage) -> Result<()> {
    send_frame(link, Frame::Text(serde_json::to_string(message)?)).await
}
async fn send_frame(link: &mut Link, frame: Frame) -> Result<()> {
    timeout(IDLE_TIMEOUT, link.send(frame))
        .await
        .map_err(|_| anyhow!("file send timed out"))?
}
pub async fn receive_frame(link: &mut Link) -> Result<Frame> {
    timeout(IDLE_TIMEOUT, link.recv())
        .await
        .map_err(|_| anyhow!("file receive timed out"))??
        .ok_or_else(|| anyhow!("file link closed before completion"))
}
pub fn decode_message(frame: Frame) -> Result<FileMessage> {
    let Frame::Text(text) = frame else {
        bail!("expected file control message")
    };
    let message: FileMessage = serde_json::from_str(&text)?;
    if let FileMessage::Error { message } = message {
        bail!("remote file transfer: {message}")
    }
    Ok(message)
}
pub async fn receive_message(link: &mut Link) -> Result<FileMessage> {
    decode_message(receive_frame(link).await?)
}
async fn observe(observer: Option<&dyn TransferObserver>, bytes: u64, total: u64) -> Result<()> {
    if let Some(observer) = observer {
        observer.progress(bytes, total).await?;
    }
    Ok(())
}
pub async fn send_content<R: AsyncRead + Unpin>(
    link: &mut Link,
    reader: &mut R,
    size: u64,
    observer: Option<&dyn TransferObserver>,
) -> Result<ContentDigest> {
    let mut buffer = vec![0; CHUNK_BYTES];
    let mut bytes = 0;
    let mut hasher = Sha256::new();
    observe(observer, 0, size).await?;
    while bytes < size {
        let limit = (size - bytes).min(CHUNK_BYTES as u64) as usize;
        let n = reader.read(&mut buffer[..limit]).await?;
        if n == 0 {
            bail!("file source ended after {bytes} bytes; expected {size}")
        }
        send_frame(link, Frame::Binary(buffer[..n].to_vec())).await?;
        hasher.update(&buffer[..n]);
        bytes += n as u64;
        observe(observer, bytes, size).await?;
    }
    let result = digest(hasher, bytes);
    send_message(
        link,
        &FileMessage::End {
            bytes,
            sha256: result.sha256.clone(),
        },
    )
    .await?;
    Ok(result)
}
pub async fn receive_content<W: AsyncWrite + Unpin>(
    link: &mut Link,
    writer: &mut W,
    size: u64,
    observer: Option<&dyn TransferObserver>,
) -> Result<ContentDigest> {
    let mut bytes = 0u64;
    let mut hasher = Sha256::new();
    observe(observer, 0, size).await?;
    loop {
        match receive_frame(link).await? {
            Frame::Binary(chunk) => {
                if chunk.is_empty() || chunk.len() > CHUNK_BYTES {
                    bail!("invalid file chunk length")
                }
                if chunk.len() as u64 > size.saturating_sub(bytes) {
                    bail!("file exceeds declared size {size}")
                }
                writer.write_all(&chunk).await?;
                hasher.update(&chunk);
                bytes += chunk.len() as u64;
                observe(observer, bytes, size).await?;
            }
            frame => match decode_message(frame)? {
                FileMessage::End {
                    bytes: expected,
                    sha256,
                } => {
                    let actual = digest(hasher, bytes);
                    if bytes != size || expected != bytes {
                        bail!("file size mismatch: received {bytes}; expected {size}")
                    }
                    if actual.sha256 != sha256 {
                        bail!("file sha256 mismatch")
                    }
                    writer.flush().await?;
                    return Ok(actual);
                }
                _ => bail!("unexpected file message during content transfer"),
            },
        }
    }
}
