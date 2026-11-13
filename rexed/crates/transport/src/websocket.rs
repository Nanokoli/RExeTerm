//! WebSocket encapsulation; control frames stay below the data layer.
use crate::{Frame, FrameReader, FrameWriter, Link, LinkFuture, MAX_FRAME_BYTES};
use anyhow::{bail, Result};
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::Mutex,
};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

pub async fn accept_websocket<S>(stream: S) -> Result<Link>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let meter = Arc::new(crate::TrafficMeter::default());
    let stream = crate::CountedIo::new(stream, meter.clone());
    Ok(from_websocket(accept_async(stream).await?).with_traffic(meter))
}
// tungstenite fixes the callback error type to an HTTP response.
#[allow(clippy::result_large_err)]
pub async fn accept_websocket_at<S>(stream: S, path: String) -> Result<Link>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let meter = Arc::new(crate::TrafficMeter::default());
    let stream = crate::CountedIo::new(stream, meter.clone());
    let ws = tokio_tungstenite::accept_hdr_async(
        stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            if request.uri().path() == path {
                Ok(response)
            } else {
                Err(tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(404)
                    .body(Some("management path not found".into()))
                    .unwrap())
            }
        },
    )
    .await?;
    Ok(from_websocket(ws).with_traffic(meter))
}
pub fn from_websocket<S>(ws: WebSocketStream<S>) -> Link
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (write, read) = ws.split();
    let write = Arc::new(Mutex::new(write));
    Link::new(
        WsWriter {
            write: write.clone(),
        },
        WsReader { read, write },
    )
}
type Write<S> = Arc<Mutex<SplitSink<WebSocketStream<S>, Message>>>;
struct WsWriter<S> {
    write: Write<S>,
}
struct WsReader<S> {
    read: SplitStream<WebSocketStream<S>>,
    write: Write<S>,
}
impl<S: AsyncRead + AsyncWrite + Unpin + Send> FrameWriter for WsWriter<S> {
    fn send(&mut self, frame: Frame) -> LinkFuture<'_, ()> {
        Box::pin(async move {
            if frame.len() > MAX_FRAME_BYTES {
                bail!("frame exceeds {MAX_FRAME_BYTES} bytes");
            }
            let message = match frame {
                Frame::Text(text) => Message::Text(text.into()),
                Frame::Binary(bytes) => Message::Binary(bytes.into()),
                Frame::Close => Message::Close(None),
            };
            self.write.lock().await.send(message).await?;
            Ok(())
        })
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin + Send> FrameReader for WsReader<S> {
    fn recv(&mut self) -> LinkFuture<'_, Option<Frame>> {
        Box::pin(async move {
            while let Some(message) = self.read.next().await {
                let frame = match message? {
                    Message::Text(text) => Frame::Text(text.to_string()),
                    Message::Binary(bytes) => Frame::Binary(bytes.to_vec()),
                    // tungstenite queues its automatic pong while reading; flush it
                    // through the shared sink, even if the data layer sends nothing.
                    Message::Ping(_) => {
                        self.write.lock().await.flush().await?;
                        continue;
                    }
                    Message::Close(_) => {
                        let _ = self.write.lock().await.flush().await;
                        return Ok(None);
                    }
                    Message::Pong(_) | Message::Frame(_) => continue,
                };
                if frame.len() > MAX_FRAME_BYTES {
                    bail!("frame exceeds {MAX_FRAME_BYTES} bytes");
                }
                return Ok(Some(frame));
            }
            Ok(None)
        })
    }
}
