//! Attach text/file decoding and decoded byte limits.
use super::options::ExbashOptions;
use crate::context::ToolContext;
use anyhow::{anyhow, Result};

pub(super) async fn input_data(
    options: &ExbashOptions,
    ctx: &ToolContext,
) -> Result<(Vec<u8>, &'static str)> {
    if options.text_input().is_some() && options.file_path_input().is_some() {
        return Err(anyhow!("Provide only one of text or filePath for attach."));
    }
    if let Some(text) = options.text_input() {
        let text = unescaper::unescape(text)
            .map_err(|err| anyhow!("failed to parse attach text escapes: {err}"))?;
        let bytes = text.into_bytes();
        validate_input_bytes("text", bytes.len())?;
        return Ok((bytes, "text"));
    }
    if let Some(path) = options.file_path_input() {
        let bytes = tokio::fs::read(ctx.resolve(path)).await?;
        validate_input_bytes("file input", bytes.len())?;
        return Ok((bytes, "file"));
    }
    Ok((Vec::new(), "attach"))
}

fn validate_input_bytes(name: &str, len: usize) -> Result<()> {
    let limit = ExbashOptions::INPUT_BYTES_LIMIT;
    if len > limit {
        return Err(anyhow!("{name} exceeds {limit} bytes ({len} bytes)"));
    }
    Ok(())
}
