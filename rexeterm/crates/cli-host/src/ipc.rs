//! Native local IPC: versioned length-prefixed control messages. No HTTP or TCP fallback.
use anyhow::{bail, ensure, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub const MAX_REQUEST: usize = 256 * 1024;
pub const MAX_RESPONSE: usize = 8 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"RXTI\x01\0\0\0";
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub service_id: String,
    pub token: String,
    pub operation: String,
    pub args: Value,
    pub key: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Response {
    pub service_id: String,
    pub result: Option<Value>,
    pub error: Option<String>,
}
pub async fn write<T: Serialize, W: AsyncWrite + Unpin + ?Sized>(
    io: &mut W,
    value: &T,
    limit: usize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= limit, "IPC message exceeds {limit} bytes");
    io.write_all(MAGIC).await?;
    io.write_u32_le(bytes.len().try_into()?).await?;
    io.write_all(&bytes).await?;
    io.flush().await?;
    Ok(())
}
pub async fn read<T: DeserializeOwned, R: AsyncRead + Unpin + ?Sized>(
    io: &mut R,
    limit: usize,
) -> Result<T> {
    let mut magic = [0; 8];
    io.read_exact(&mut magic).await?;
    ensure!(&magic == MAGIC, "unsupported RexETerm IPC protocol/version");
    let length = io.read_u32_le().await? as usize;
    ensure!(
        length > 0 && length <= limit,
        "invalid IPC message length: {length}"
    );
    let mut bytes = vec![0; length];
    io.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub type LocalStream = Box<dyn Stream>;
pub async fn connect(target: &str) -> Result<LocalStream> {
    #[cfg(unix)]
    if let Some(path) = target.strip_prefix("unix://") {
        let (address, _directory) = unix_address(std::path::Path::new(path))?;
        return Ok(Box::new(tokio::net::UnixStream::connect(address).await?));
    }
    #[cfg(windows)]
    if let Some(name) = target.strip_prefix("pipe://") {
        let path = pipe_path(name)?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&path) {
                Ok(pipe) => return Ok(Box::new(pipe)),
                Err(e)
                    if e.raw_os_error() == Some(231) && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    bail!("unsupported local IPC target: {target}; use a Unix socket or local Windows named pipe")
}
pub fn pipe_path(name: &str) -> Result<String> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid local pipe name"
    );
    Ok(format!(r"\\.\pipe\{name}"))
}
/// Keep the socket physically beside the executable even with a long Linux install path.
#[cfg(unix)]
pub fn unix_address(path: &std::path::Path) -> Result<(std::path::PathBuf, Option<std::fs::File>)> {
    use std::os::unix::ffi::OsStrExt;
    if path.as_os_str().as_bytes().len() < 104 {
        return Ok((path.to_owned(), None));
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let directory = std::fs::File::open(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("socket needs parent"))?,
        )?;
        let address = std::path::PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
            .join(
                path.file_name()
                    .ok_or_else(|| anyhow::anyhow!("socket needs filename"))?,
            );
        ensure!(
            address.as_os_str().as_bytes().len() < 104,
            "socket filename is too long"
        );
        Ok((address, Some(directory)))
    }
    #[cfg(not(target_os = "linux"))]
    bail!("socket path is too long; move the executable or use a shorter --registry-dir")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn fragmented_messages_keep_utf8_and_bound_allocations() {
        let (mut a, mut b) = tokio::io::duplex(7);
        let task = tokio::spawn(async move {
            write(&mut a, &json!({"text":"中文\u{0}🙂"}), MAX_REQUEST)
                .await
                .unwrap();
        });
        let value: Value = read(&mut b, MAX_REQUEST).await.unwrap();
        assert_eq!(value["text"], "中文\u{0}🙂");
        task.await.unwrap();
        let mut invalid = Vec::from(MAGIC.as_slice());
        invalid.extend(u32::MAX.to_le_bytes());
        assert!(read::<Value, _>(&mut invalid.as_slice(), MAX_REQUEST)
            .await
            .unwrap_err()
            .to_string()
            .contains("length"));
        assert!(read::<Value, _>(&mut b"GET /api".as_slice(), MAX_REQUEST)
            .await
            .is_err());
    }
    #[tokio::test]
    async fn network_targets_and_remote_pipe_names_are_rejected() {
        for target in [
            "http://127.0.0.1:1",
            "tcp://127.0.0.1:1",
            "pipe://server/path",
        ] {
            assert!(connect(target).await.is_err());
        }
        assert!(pipe_path(r"server\name").is_err());
        assert_eq!(pipe_path("rexeterm-abc").unwrap(), r"\\.\pipe\rexeterm-abc");
    }
}
