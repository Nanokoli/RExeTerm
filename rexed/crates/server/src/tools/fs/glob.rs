//! File discovery, cooperative deadlines, ordering and result limits.
use crate::{
    context::ToolContext,
    protocol::{tool_output, ToolResult},
    tools::soft_timeout::SoftTimeout,
};
use anyhow::{anyhow, Result};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::json;
use std::{
    cmp::Reverse,
    fs,
    path::{Path, PathBuf},
};
const GLOB_LIMIT: usize = 100;

#[derive(Clone, Debug, Deserialize)]
pub struct GlobOptions {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub timeout: Option<i64>,
}
pub fn glob_paths(options: GlobOptions, ctx: &ToolContext) -> Result<ToolResult> {
    if options.pattern.is_empty() {
        return Err(anyhow!("pattern is required"));
    }

    let search = options
        .path
        .as_ref()
        .map(|path| ctx.resolve(path))
        .unwrap_or_else(|| ctx.directory.clone());
    let globset = build_globset(std::slice::from_ref(&options.pattern))?;
    let mut files = Vec::new();
    let mut truncated = false;
    let mut deadline = SoftTimeout::from_millis(options.timeout)?;
    let mut files_walked = 0usize;

    for entry in WalkBuilder::new(&search).hidden(false).build() {
        if deadline.expired() {
            break;
        }
        let path = entry?.into_path();
        if !path.is_file() {
            continue;
        }
        files_walked += 1;
        let relative = path.strip_prefix(&search).unwrap_or(&path);
        if !globset.is_match(relative) && !globset.is_match(&path) {
            continue;
        }
        if files.len() >= GLOB_LIMIT {
            truncated = true;
            break;
        }
        let mtime = mtime_ms(&path);
        files.push((path, mtime));
    }

    files.sort_by_key(|file| Reverse(file.1));
    let mut output = Vec::new();
    if files.is_empty() {
        output.push("No files found".to_string());
    } else {
        output.extend(
            files
                .iter()
                .map(|(path, _)| path.to_string_lossy().into_owned()),
        );
        if truncated {
            output.push(String::new());
            output.push(format!(
                "(Results are truncated: showing first {GLOB_LIMIT} results. Consider using a more specific path or pattern.)"
            ));
        }
    }

    Ok(ToolResult {
        metadata: json!({
            "count": files.len(),
            "truncated": truncated,
            "filesWalked": files_walked,
            "timedOut": deadline.timed_out()
        }),
        output: tool_output(output.join("\n")),
    })
}
fn build_globset(globs: &[String]) -> Result<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(Glob::new(glob)?);
    }
    Ok(builder.build()?)
}

fn mtime_ms(path: &Path) -> u128 {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
