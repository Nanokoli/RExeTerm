//! Generic stream data over any framed link. Drivers and physical link framing
//! remain outside this crate; TCP is the default stream target implementation.
pub mod file;
pub mod staged_file;
use anyhow::{anyhow, bail, Result};
use remote_executor_protocol::forward::{StreamMessage, StreamTarget};
use remote_executor_transport::{Frame, Link, LinkFuture};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub trait ByteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ByteStream for T {}
pub trait StreamOpener: Send + Sync {
    fn open<'a>(&'a self, target: &'a StreamTarget) -> LinkFuture<'a, Box<dyn ByteStream>>;
}
#[derive(Default)]
pub struct NativeStreamOpener;
impl StreamOpener for NativeStreamOpener {
    fn open<'a>(&'a self, target: &'a StreamTarget) -> LinkFuture<'a, Box<dyn ByteStream>> {
        Box::pin(async move {
            match target.kind.as_str() {
                "tcp" => Ok(
                    Box::new(tokio::net::TcpStream::connect(&target.address).await?)
                        as Box<dyn ByteStream>,
                ),
                #[cfg(unix)]
                "unix" => Ok(
                    Box::new(tokio::net::UnixStream::connect(&target.address).await?)
                        as Box<dyn ByteStream>,
                ),
                other => {
                    bail!("unsupported stream target `{other}`; inject a StreamOpener for devices")
                }
            }
        })
    }
}
fn message(value: &StreamMessage) -> Result<Frame> {
    Ok(Frame::Text(serde_json::to_string(value)?))
}
/// Negotiate a remote target before passing local application bytes.
pub async fn open_stream(link: &mut Link, target: StreamTarget) -> Result<()> {
    link.send(message(&StreamMessage::Open { target })?).await?;
    match link.recv().await? {
        Some(Frame::Text(text)) => match serde_json::from_str(&text)? {
            StreamMessage::Ready => Ok(()),
            StreamMessage::Error { message } => Err(anyhow!(message)),
            _ => bail!("unexpected stream handshake response"),
        },
        _ => bail!("stream closed during handshake"),
    }
}
pub async fn serve_stream(
    mut link: Link,
    target: StreamTarget,
    opener: &dyn StreamOpener,
) -> Result<()> {
    let opened =
        tokio::time::timeout(std::time::Duration::from_secs(30), opener.open(&target)).await;
    let stream = match opened {
        Ok(Ok(stream)) => stream,
        error => {
            let error = match error {
                Ok(Err(e)) => e.to_string(),
                _ => "stream target open timed out".into(),
            };
            link.send(message(&StreamMessage::Error { message: error })?)
                .await?;
            return Ok(());
        }
    };
    link.send(message(&StreamMessage::Ready)?).await?;
    bridge(stream, link).await?;
    Ok(())
}
/// Full duplex with bounded memory, backpressure and independent EOF in each
/// direction. Each stream stays on one link so bytes never require reordering.
pub async fn bridge<S: ByteStream>(stream: S, link: Link) -> Result<(u64, u64)> {
    let (mut read, mut write) = tokio::io::split(stream);
    let (mut sender, mut receiver) = link.split();
    let send = async {
        let mut buffer = vec![0; 64 * 1024];
        let mut total = 0;
        loop {
            let n = read.read(&mut buffer).await?;
            if n == 0 {
                sender.send(message(&StreamMessage::Eof)?).await?;
                return Ok::<_, anyhow::Error>(total);
            }
            sender.send(Frame::Binary(buffer[..n].to_vec())).await?;
            total += n as u64;
        }
    };
    let recv = async {
        let mut total = 0;
        loop {
            match receiver.recv().await? {
                Some(Frame::Binary(bytes)) => {
                    write.write_all(&bytes).await?;
                    total += bytes.len() as u64;
                }
                Some(Frame::Text(text)) => match serde_json::from_str(&text)? {
                    StreamMessage::Eof => {
                        write.shutdown().await?;
                        return Ok::<_, anyhow::Error>(total);
                    }
                    StreamMessage::Error { message } => bail!("remote stream: {message}"),
                    _ => bail!("unexpected stream data message"),
                },
                _ => bail!("stream link closed before EOF"),
            }
        }
    };
    tokio::try_join!(send, recv)
}
