//! Bounded framing shared by the JSONL application adapters.
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_IN_FLIGHT: usize = 32;

pub async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            break;
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        if count > MAX_MESSAGE_BYTES.saturating_sub(bytes.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "JSONL message exceeds 16 MiB",
            ));
        }
        let complete = available[count - 1] == b'\n';
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if complete {
            break;
        }
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_an_unterminated_oversized_line() {
        let data = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut input = &data[..];
        assert_eq!(
            read_line(&mut input).await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
    #[tokio::test]
    async fn preserves_crlf_unicode_and_final_unterminated_message() {
        let mut input = "你好\r\nlast".as_bytes();
        assert_eq!(read_line(&mut input).await.unwrap().unwrap(), "你好\r\n");
        assert_eq!(read_line(&mut input).await.unwrap().unwrap(), "last");
        assert!(read_line(&mut input).await.unwrap().is_none());
    }
}
