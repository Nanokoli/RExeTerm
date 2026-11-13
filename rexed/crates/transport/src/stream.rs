//! REX1 framing for reliable ordered byte streams, including serial drivers.
use crate::{Frame, FrameReader, FrameWriter, Link, LinkFuture, MAX_FRAME_BYTES};
use anyhow::{anyhow, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
const HEADER_BYTES: usize = 9;

/// The client opens/configures the device. This adapter only frames its bytes.
pub fn from_stream<S>(stream: S) -> Link
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let meter = std::sync::Arc::new(crate::TrafficMeter::default());
    let (read, write) = tokio::io::split(crate::CountedIo::new(stream, meter.clone()));
    Link::new(
        StreamWriter {
            write,
            closed: false,
        },
        StreamReader {
            read,
            header: [0; HEADER_BYTES],
            header_read: 0,
            body: Vec::new(),
            body_read: 0,
            closed: false,
        },
    )
    .with_traffic(meter)
}
struct StreamWriter<W> {
    write: W,
    closed: bool,
}
impl<W: AsyncWrite + Unpin + Send> FrameWriter for StreamWriter<W> {
    fn send(&mut self, frame: Frame) -> LinkFuture<'_, ()> {
        Box::pin(async move {
            if self.closed {
                bail!("link is closed");
            }
            if frame.len() > MAX_FRAME_BYTES {
                bail!("frame exceeds {MAX_FRAME_BYTES} bytes");
            }
            let (kind, body): (u8, &[u8]) = match &frame {
                Frame::Text(text) => (1, text.as_bytes()),
                Frame::Binary(bytes) => (2, bytes),
                Frame::Close => (3, &[]),
            };
            let mut header = [0; HEADER_BYTES];
            header[..4].copy_from_slice(b"REX1");
            header[4] = kind;
            header[5..].copy_from_slice(&(body.len() as u32).to_be_bytes());
            // A failed/cancelled write must not be retried on this byte stream.
            self.closed = true;
            self.write.write_all(&header).await?;
            self.write.write_all(body).await?;
            // All frame bytes are queued now. Cancelling a flush may still
            // need retransmission, but appending a Close frame is safe. A
            // cancellation in either write_all above keeps the link poisoned.
            self.closed = kind == 3;
            self.write.flush().await?;
            if kind == 3 {
                self.write.shutdown().await?;
            } else {
                self.closed = false;
            }
            Ok(())
        })
    }
}
struct StreamReader<R> {
    read: R,
    header: [u8; HEADER_BYTES],
    header_read: usize,
    body: Vec<u8>,
    body_read: usize,
    closed: bool,
}
impl<R: AsyncRead + Unpin + Send> FrameReader for StreamReader<R> {
    fn recv(&mut self) -> LinkFuture<'_, Option<Frame>> {
        Box::pin(async move {
            if self.closed {
                return Ok(None);
            }
            while self.header_read < HEADER_BYTES {
                let count = self.read.read(&mut self.header[self.header_read..]).await?;
                if count == 0 {
                    self.closed = true;
                    if self.header_read == 0 {
                        return Ok(None);
                    }
                    bail!("truncated REX1 header");
                }
                self.header_read += count;
            }
            if self.body.is_empty() && self.body_read == 0 {
                self.closed = true;
                if &self.header[..4] != b"REX1" {
                    bail!("invalid REX1 frame magic/version");
                }
                let kind = self.header[4];
                if !(1..=3).contains(&kind) {
                    bail!("invalid REX1 frame kind: {kind}");
                }
                let size = u32::from_be_bytes(self.header[5..].try_into().unwrap()) as usize;
                if size > MAX_FRAME_BYTES {
                    bail!("frame exceeds {MAX_FRAME_BYTES} bytes");
                }
                if kind == 3 {
                    if size != 0 {
                        bail!("close frame must be empty");
                    }
                    return Ok(None);
                }
                self.body.resize(size, 0);
                self.closed = false;
            }
            while self.body_read < self.body.len() {
                let count = self.read.read(&mut self.body[self.body_read..]).await?;
                if count == 0 {
                    self.closed = true;
                    bail!("truncated REX1 payload");
                }
                self.body_read += count;
            }
            let body = std::mem::take(&mut self.body);
            self.header_read = 0;
            self.body_read = 0;
            let frame = match self.header[4] {
                1 => match String::from_utf8(body) {
                    Ok(text) => Frame::Text(text),
                    Err(_) => {
                        self.closed = true;
                        return Err(anyhow!("invalid UTF-8 in text frame"));
                    }
                },
                _ => Frame::Binary(body),
            };
            Ok(Some(frame))
        })
    }
}
