//! Text, directory and binary reads, including pagination and presentation.
use super::stamp::{file_hash_code, file_stamp_for_metadata, FileStamp};
use crate::{
    context::ToolContext,
    protocol::{tool_output_full, ToolResult},
};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};
const DEFAULT_READ_LIMIT: usize = 2000;
const BINARY_READ_LIMIT: usize = 128;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
fn read_binary_file(
    path: &Path,
    file: FileStamp,
    offset: usize,
    limit: usize,
    hash_code: Option<String>,
) -> Result<ToolResult> {
    let bytes = fs::read(path)?;
    if offset > bytes.len() {
        return Err(anyhow!(
            "Offset {offset} is out of range for this file ({} bytes)",
            bytes.len()
        ));
    }

    let limit = limit.clamp(1, BINARY_READ_LIMIT);
    let end = (offset + limit).min(bytes.len());
    let slice = &bytes[offset..end];
    let truncated = end < bytes.len();
    let hex = hexdump(slice, offset);

    let info = if truncated {
        format!(
            "Showing bytes {offset}-{} of {}. Use offset={end} to continue.",
            end.saturating_sub(1),
            bytes.len()
        )
    } else {
        format!("total {} bytes", bytes.len())
    };
    let message = "";
    let mut metadata = json!({ "file": file });
    if let Some(hash_code) = hash_code {
        metadata["hashCode"] = json!(hash_code);
    }

    Ok(ToolResult {
        metadata,
        output: tool_output_full(message, hex, info),
    })
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReadOptions {
    #[serde(rename = "filePath")]
    pub file_path: PathBuf,
    #[serde(default)]
    pub mode: Option<ReadMode>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default, rename = "hashCheckMode")]
    pub hash_check_mode: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReadMode {
    Text,
    Binary,
}
pub fn read_path(options: ReadOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let filepath = ctx.resolve(&options.file_path);
    let stat = fs::metadata(&filepath)
        .with_context(|| format!("File not found: {}", filepath.display()))?;
    let file = file_stamp_for_metadata(&filepath, &stat)?;
    let hash_code = if options.hash_check_mode {
        if !stat.is_file() {
            return Err(anyhow!(
                "hashCheckMode only supports files: {}",
                filepath.display()
            ));
        }
        Some(file_hash_code(&filepath)?)
    } else {
        None
    };
    let mode = options.mode.unwrap_or(ReadMode::Text);
    if stat.is_dir() {
        if mode == ReadMode::Binary {
            return Err(anyhow!(
                "binary read mode only supports files: {}",
                filepath.display()
            ));
        }
        return read_dir(
            &filepath,
            file,
            options.offset.unwrap_or(1),
            options.limit.unwrap_or(DEFAULT_READ_LIMIT),
        );
    }
    if mode == ReadMode::Binary {
        return read_binary_file(
            &filepath,
            file,
            options.offset.unwrap_or(0),
            options.limit.unwrap_or(BINARY_READ_LIMIT),
            hash_code,
        );
    }
    read_file(
        &filepath,
        file,
        options.offset.unwrap_or(1),
        options.limit.unwrap_or(DEFAULT_READ_LIMIT),
        hash_code,
    )
}
fn read_dir(path: &Path, file: FileStamp, offset: usize, limit: usize) -> Result<ToolResult> {
    if offset < 1 {
        return Err(anyhow!("offset must be greater than or equal to 1"));
    }
    let mut entries = fs::read_dir(path)?
        .map(|entry| {
            let entry = entry?;
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() {
                name.push('/');
            }
            Ok(name)
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort();

    let start = offset - 1;
    let limit = limit.max(1);
    let sliced = entries
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let total_entries = entries.len();
    let truncated = start + sliced.len() < total_entries;
    let next_offset = truncated.then_some(offset + sliced.len());
    let output = sliced.join("\n");

    let info = if truncated {
        format!(
            "Showing entries {offset}-{} of {total_entries}. Use offset={} to continue.",
            offset + sliced.len() - 1,
            next_offset.unwrap_or(offset)
        )
    } else {
        format!("total {total_entries} entries")
    };
    let message = "";
    Ok(ToolResult {
        metadata: json!({ "file": file }),
        output: tool_output_full(message, output, info),
    })
}

fn read_file(
    path: &Path,
    file: FileStamp,
    offset: usize,
    limit: usize,
    hash_code: Option<String>,
) -> Result<ToolResult> {
    if offset < 1 {
        return Err(anyhow!("offset must be greater than or equal to 1"));
    }
    let bytes = fs::read(path)?;
    if let Some((offset, byte)) = binary_byte(&bytes) {
        return Err(anyhow!(
            "Cannot read binary file: {} (binary byte at offset {}: 0x{:02X})",
            path.display(),
            offset,
            byte
        ));
    }

    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() < offset && !(lines.is_empty() && offset == 1) {
        return Err(anyhow!(
            "Offset {offset} is out of range for this file ({} lines)",
            lines.len()
        ));
    }

    let start = offset - 1;
    let limit = limit.max(1);
    let raw = lines
        .iter()
        .skip(start)
        .take(limit)
        .map(|line| truncate_line(line))
        .collect::<Vec<_>>();
    let last = offset + raw.len().saturating_sub(1);
    let truncated = start + raw.len() < lines.len();
    let output = raw
        .iter()
        .enumerate()
        .map(|(idx, line)| format!("{}: {line}", offset + idx))
        .collect::<Vec<_>>()
        .join("\n");
    let next_offset = truncated.then_some(last + 1);

    let info = if truncated {
        format!(
            "Showing lines {offset}-{last} of {}. Use offset={} to continue.",
            lines.len(),
            next_offset.unwrap_or(last + 1)
        )
    } else {
        format!("total {} lines", lines.len())
    };
    let message = "";
    let mut metadata = json!({ "file": file });
    if let Some(hash_code) = hash_code {
        metadata["hashCode"] = json!(hash_code);
    }

    Ok(ToolResult {
        metadata,
        output: tool_output_full(message, output, info),
    })
}
fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_LENGTH {
        return line.to_string();
    }
    format!("{}{}", &line[..MAX_LINE_LENGTH], MAX_LINE_SUFFIX)
}

fn binary_byte(bytes: &[u8]) -> Option<(usize, u8)> {
    bytes
        .iter()
        .take(4096)
        .enumerate()
        .find_map(|(idx, byte)| (*byte == 0).then_some((idx, *byte)))
}

fn hexdump(bytes: &[u8], base: usize) -> String {
    bytes
        .chunks(16)
        .enumerate()
        .map(|(row, chunk)| {
            let offset = base + row * 16;
            let hex = (0..16)
                .map(|idx| {
                    chunk
                        .get(idx)
                        .map(|byte| format!("{byte:02X}"))
                        .unwrap_or_else(|| "  ".to_string())
                })
                .collect::<Vec<_>>();
            let ascii = chunk
                .iter()
                .map(|byte| {
                    if byte.is_ascii_graphic() || *byte == b' ' {
                        *byte as char
                    } else {
                        '.'
                    }
                })
                .collect::<String>();
            format!("{offset:08X}  {}  |{}|", hex.join(" "), ascii)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
