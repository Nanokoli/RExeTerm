//! Mutation metadata and diff presentation.
use super::{options::PatchMode, text_patch::TextShape};
use crate::{
    context::ToolContext,
    protocol::{tool_output, ToolResult},
};
use diffy::create_patch as diffy_create_patch;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, Serialize)]
pub struct PatchFile {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "relativePath")]
    pub relative_path: String,
    #[serde(rename = "newFilePath", skip_serializing_if = "Option::is_none")]
    pub new_file_path: Option<String>,
    #[serde(rename = "newRelativePath", skip_serializing_if = "Option::is_none")]
    pub new_relative_path: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub additions: usize,
    pub deletions: usize,
}
pub(super) fn result_from_file(file: PatchFile, hash_code: Option<String>) -> ToolResult {
    result_from_file_with_diff(file, hash_code, None)
}

pub(super) fn result_from_file_with_diff(
    file: PatchFile,
    hash_code: Option<String>,
    diff: Option<String>,
) -> ToolResult {
    let mut output = match file.kind.as_str() {
        "create" => format!("Success. Created file:\nC {}", file.relative_path),
        "delete" => format!("Success. Deleted file:\nD {}", file.relative_path),
        "rename" => format!(
            "Success. Renamed file:\nR {} -> {}",
            file.relative_path,
            file.new_relative_path.as_deref().unwrap_or_default()
        ),
        _ => format!("Success. Updated file:\nM {}", file.relative_path),
    };
    if let Some(hash_code) = &hash_code {
        output.push_str(&format!("\nhashCode: {hash_code}"));
    }

    let mut metadata = json!({ "file": file, "diagnostics": {} });
    if let Some(hash_code) = hash_code {
        metadata["hashCode"] = Value::String(hash_code);
    }
    if let Some(diff) = diff.filter(|value| !value.trim().is_empty()) {
        metadata["diff"] = Value::String(diff);
    }

    ToolResult {
        metadata,
        output: tool_output(output),
    }
}
pub(super) fn binary_patch_file(
    ctx: &ToolContext,
    path: &Path,
    additions: usize,
    deletions: usize,
) -> PatchFile {
    PatchFile {
        file_path: path.to_string_lossy().into_owned(),
        relative_path: ctx.title(path),
        new_file_path: None,
        new_relative_path: None,
        kind: "binary-update".to_string(),
        additions,
        deletions,
    }
}
pub(super) fn patch_file(ctx: &ToolContext, path: &Path, diff: &str) -> PatchFile {
    let additions = count_diff_lines(diff, '+');
    let deletions = count_diff_lines(diff, '-');
    PatchFile {
        file_path: path.to_string_lossy().into_owned(),
        relative_path: ctx.title(path),
        kind: "update".to_string(),
        new_file_path: None,
        new_relative_path: None,
        additions,
        deletions,
    }
}

pub(super) fn action_file(
    ctx: &ToolContext,
    path: &Path,
    kind: &str,
    additions: usize,
    deletions: usize,
) -> PatchFile {
    PatchFile {
        file_path: path.to_string_lossy().into_owned(),
        relative_path: ctx.title(path),
        new_file_path: None,
        new_relative_path: None,
        kind: kind.to_string(),
        additions,
        deletions,
    }
}

pub(super) fn rename_file(ctx: &ToolContext, from: &Path, to: &Path) -> PatchFile {
    PatchFile {
        file_path: from.to_string_lossy().into_owned(),
        relative_path: ctx.title(from),
        new_file_path: Some(to.to_string_lossy().into_owned()),
        new_relative_path: Some(ctx.title(to)),
        kind: "rename".to_string(),
        additions: 0,
        deletions: 0,
    }
}
pub(super) fn diff_text(path: &Path, before: &str, after: &str) -> String {
    let diff = diffy_create_patch(before, after).to_string();
    diff.replacen("--- original", &format!("--- {}", path.display()), 1)
        .replacen("+++ modified", &format!("+++ {}", path.display()), 1)
}

pub(super) fn diff_binary(path: &Path, before: &[u8], after: &[u8]) -> String {
    diff_text(path, &binary_diff_text(before), &binary_diff_text(after))
}

pub(super) fn delete_diff(path: &Path, before: &[u8], mode: PatchMode) -> String {
    if mode == PatchMode::Binary {
        return diff_binary(path, before, &[]);
    }
    TextShape::from_bytes(before.to_vec())
        .map(|shape| diff_text(path, &shape.text, ""))
        .unwrap_or_else(|_| diff_binary(path, before, &[]))
}

fn binary_diff_text(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(empty)\n".to_string();
    }
    let mut lines = bytes
        .chunks(16)
        .enumerate()
        .map(|(idx, chunk)| {
            let hex = chunk
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{:08X}: {hex}", idx * 16)
        })
        .collect::<Vec<_>>()
        .join("\n");
    lines.push('\n');
    lines
}

fn count_diff_lines(diff: &str, marker: char) -> usize {
    diff.lines()
        .filter(|line| line.starts_with(marker))
        .filter(|line| !line.starts_with("+++") && !line.starts_with("---"))
        .count()
}
