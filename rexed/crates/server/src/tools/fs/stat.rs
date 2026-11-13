//! File metadata query and its text presentation.
use super::stamp::{file_stamp, FileKind, FileStamp};
use crate::{
    context::ToolContext,
    protocol::{tool_output, ToolResult},
};
use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
pub struct StatOptions {
    #[serde(rename = "filePath")]
    pub file_path: PathBuf,
}
pub fn stat_path(options: StatOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let filepath = ctx.resolve(&options.file_path);
    let file = file_stamp(&filepath)?;
    let output = format_file_stamp(&file);
    Ok(ToolResult {
        metadata: json!({ "file": file }),
        output: tool_output(output),
    })
}

fn format_file_stamp(file: &FileStamp) -> String {
    let kind = match file.kind {
        FileKind::File => "file",
        FileKind::Directory => "directory",
        FileKind::Missing => "missing",
        FileKind::Other => "other",
    };
    let mut lines = vec![
        format!("kind: {kind}"),
        format!("canonicalPath: {}", file.canonical_path),
        format!("fileKey: {}", file.file_key),
    ];
    if let Some(size) = file.size {
        lines.push(format!("size: {size}"));
    }
    if let Some(mtime_ms) = file.mtime_ms {
        lines.push(format!("mtimeMs: {mtime_ms}"));
    }
    lines.join("\n")
}
