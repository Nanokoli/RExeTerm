//! File mutations and hash preconditions. Patch engines do not perform I/O.
mod binary_patch;
mod options;
mod result;
#[cfg(test)]
mod test;
mod text_patch;

use crate::{context::ToolContext, protocol::ToolResult, tools::fs::hash_bytes};
use anyhow::{anyhow, Context, Result};
use binary_patch::decode_hex;
pub use options::{FileActionMode, FileActionOptions, PatchMode};
pub use result::PatchFile;
use result::{
    action_file, binary_patch_file, delete_diff, diff_binary, diff_text, patch_file, rename_file,
    result_from_file, result_from_file_with_diff,
};
use std::{fs, path::Path};
use text_patch::{apply_line_text_patch, TextShape};
pub async fn file_action(options: FileActionOptions, ctx: &ToolContext) -> Result<ToolResult> {
    let target = ctx.resolve(&options.file_path);
    match options.mode {
        FileActionMode::Patch => patch_file_action(ctx, &target, &options).await,
        FileActionMode::Create => create_file_action(ctx, &target, &options).await,
        FileActionMode::Delete => delete_file_action(ctx, &target, &options).await,
        FileActionMode::Rename => rename_file_action(ctx, &target, &options).await,
    }
}

async fn patch_file_action(
    ctx: &ToolContext,
    target: &Path,
    options: &FileActionOptions,
) -> Result<ToolResult> {
    let patch_text = options
        .patch_text
        .as_deref()
        .ok_or_else(|| anyhow!("patchText is required for mode=patch"))?;
    if patch_text.trim().is_empty() {
        return Err(anyhow!("patchText is required for mode=patch"));
    }
    let before_bytes = read_existing_with_hash_check(target, options)?;
    match options.patch_mode {
        PatchMode::Text => apply_text_patch(ctx, target, before_bytes, patch_text, options).await,
        PatchMode::Binary => {
            apply_binary_patch(ctx, target, before_bytes, patch_text, options).await
        }
    }
}

async fn create_file_action(
    ctx: &ToolContext,
    target: &Path,
    options: &FileActionOptions,
) -> Result<ToolResult> {
    if target.exists() {
        return Err(anyhow!("cannot create existing file: {}", target.display()));
    }
    let content = options
        .content
        .as_deref()
        .ok_or_else(|| anyhow!("content is required for mode=create"))?;
    let bytes = match options.patch_mode {
        PatchMode::Text => content.as_bytes().to_vec(),
        PatchMode::Binary => decode_hex(content)?,
    };
    fs::write(target, &bytes)
        .with_context(|| format!("failed to create file {}", target.display()))?;
    let file = action_file(ctx, target, "create", bytes.len(), 0);
    let diff = match options.patch_mode {
        PatchMode::Text => diff_text(target, "", content),
        PatchMode::Binary => diff_binary(target, &[], &bytes),
    };
    Ok(result_from_file_with_diff(
        file,
        options.hash_check_mode.then_some(hash_bytes(&bytes)),
        Some(diff),
    ))
}

async fn delete_file_action(
    ctx: &ToolContext,
    target: &Path,
    options: &FileActionOptions,
) -> Result<ToolResult> {
    let before = read_existing_with_hash_check(target, options)?;
    let diff = delete_diff(target, &before, options.patch_mode);
    fs::remove_file(target)
        .with_context(|| format!("failed to delete file {}", target.display()))?;
    let file = action_file(ctx, target, "delete", 0, before.len());
    Ok(result_from_file_with_diff(file, None, Some(diff)))
}

async fn rename_file_action(
    ctx: &ToolContext,
    target: &Path,
    options: &FileActionOptions,
) -> Result<ToolResult> {
    let before = read_existing_with_hash_check(target, options)?;
    let new_path = options
        .new_file_path
        .as_ref()
        .ok_or_else(|| anyhow!("newFilePath is required for mode=rename"))?;
    let new_path = ctx.resolve(new_path);
    if new_path.exists() {
        return Err(anyhow!(
            "cannot rename over existing file: {}",
            new_path.display()
        ));
    }
    fs::rename(target, &new_path).with_context(|| {
        format!(
            "failed to rename file {} to {}",
            target.display(),
            new_path.display()
        )
    })?;
    let file = rename_file(ctx, target, &new_path);
    Ok(result_from_file(
        file,
        options.hash_check_mode.then_some(hash_bytes(&before)),
    ))
}

async fn apply_text_patch(
    ctx: &ToolContext,
    target: &Path,
    before_bytes: Vec<u8>,
    patch_text: &str,
    options: &FileActionOptions,
) -> Result<ToolResult> {
    let shape = TextShape::from_bytes(before_bytes)?;
    let after_text = apply_line_text_patch(&shape.text, patch_text)?;
    let after_bytes = shape.encode(&after_text);
    fs::write(target, &after_bytes)
        .with_context(|| format!("failed to write patch target {}", target.display()))?;

    let after_hash = hash_bytes(&after_bytes);
    let diff = diff_text(target, &shape.text, &after_text);
    let file = patch_file(ctx, target, &diff);
    Ok(result_from_file_with_diff(
        file,
        options.hash_check_mode.then_some(after_hash),
        Some(diff),
    ))
}

async fn apply_binary_patch(
    ctx: &ToolContext,
    target: &Path,
    before_bytes: Vec<u8>,
    patch_text: &str,
    options: &FileActionOptions,
) -> Result<ToolResult> {
    let applied = binary_patch::apply_patch(&before_bytes, patch_text)?;
    let after_bytes = applied.bytes;
    fs::write(target, &after_bytes)
        .with_context(|| format!("failed to write patch target {}", target.display()))?;

    let after_hash = hash_bytes(&after_bytes);
    let file = binary_patch_file(ctx, target, applied.additions, applied.deletions);
    let diff = diff_binary(target, &before_bytes, &after_bytes);
    Ok(result_from_file_with_diff(
        file,
        options.hash_check_mode.then_some(after_hash),
        Some(diff),
    ))
}
fn read_existing_with_hash_check(path: &Path, options: &FileActionOptions) -> Result<Vec<u8>> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read target file {}", path.display()))?;
    if options.hash_check_mode {
        let expected = normalize_hash_code(options.hash_code.as_deref().unwrap_or_default())?;
        let current = hash_bytes(&bytes);
        if expected != current {
            return Err(anyhow!(
                "hash mismatch for {}: expected {}, current {}; re-read and retry",
                path.display(),
                expected,
                current
            ));
        }
    }
    Ok(bytes)
}
fn normalize_hash_code(value: &str) -> Result<String> {
    let trimmed = value.trim();
    let digest = trimmed.strip_prefix("sha256:").unwrap_or(trimmed);
    if digest.len() != 64 || !digest.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "hashCode must be a full SHA-256 digest, optionally prefixed with sha256:"
        ));
    }
    Ok(format!("sha256:{}", digest.to_ascii_lowercase()))
}
