//! Binary patch parsing and application using offsets from the original bytes.
use anyhow::{anyhow, Context, Result};

#[derive(Clone, Debug)]
struct BinaryHunk {
    anchor: BinaryAnchor,
    bytes: Vec<u8>,
    order: usize,
}

#[derive(Clone, Debug)]
enum BinaryAnchor {
    Replace {
        offset: usize,
        len: usize,
    },
    Delete {
        offset: usize,
        len: usize,
    },
    Append {
        target: BinaryAppendTarget,
        len: usize,
    },
}

#[derive(Clone, Debug)]
enum BinaryAppendTarget {
    Offset(usize),
    End,
}

#[derive(Clone, Debug)]
struct BinaryOperation {
    start: usize,
    end: usize,
    replacement: Vec<u8>,
    order: usize,
    additions: usize,
    deletions: usize,
}

#[derive(Clone, Debug)]
pub(super) struct BinaryPatchApplied {
    pub(super) bytes: Vec<u8>,
    pub(super) additions: usize,
    pub(super) deletions: usize,
}

pub(super) fn apply_patch(bytes: &[u8], patch_text: &str) -> Result<BinaryPatchApplied> {
    let hunks = parse_binary_patch(patch_text)?;
    apply_binary_hunks(bytes, &hunks)
}
fn parse_binary_patch(patch_text: &str) -> Result<Vec<BinaryHunk>> {
    if patch_text
        .lines()
        .any(|line| line.trim() == "*** Begin Patch")
    {
        return Err(anyhow!(
            "old patch envelope format is not supported; pass filePath separately and use binary patchText"
        ));
    }

    let mut hunks = Vec::new();
    for (order, raw) in patch_text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("***DELETE***") {
            let (offset, len) = parse_binary_offset_len(rest.trim())?;
            hunks.push(BinaryHunk {
                anchor: BinaryAnchor::Delete { offset, len },
                bytes: Vec::new(),
                order,
            });
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("***APPEND***") {
            let (span, hex) = split_binary_body(rest.trim(), line)?;
            let (target, len) = parse_binary_append_span(span)?;
            let bytes = decode_hex(hex)?;
            ensure_binary_body_len(bytes.len(), len, line)?;
            hunks.push(BinaryHunk {
                anchor: BinaryAnchor::Append { target, len },
                bytes,
                order,
            });
            continue;
        }

        if let Some((span, hex)) = trimmed.split_once(':') {
            let (offset, len) = parse_binary_offset_len(span.trim())?;
            let bytes = decode_hex(hex)?;
            ensure_binary_body_len(bytes.len(), len, line)?;
            hunks.push(BinaryHunk {
                anchor: BinaryAnchor::Replace { offset, len },
                bytes,
                order,
            });
            continue;
        }

        return Err(anyhow!(
            "invalid binary patch instruction `{line}`; expected `***DELETE*** offset-len`, `***APPEND*** offset-len:HEX`, or `offset-len:HEX`"
        ));
    }
    if hunks.is_empty() {
        return Err(anyhow!("patchText did not contain any hunks"));
    }
    Ok(hunks)
}

fn parse_byte_offset(value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .with_context(|| format!("invalid byte offset `{value}`"))
}

fn parse_byte_len(value: &str) -> Result<usize> {
    let len = value
        .parse::<usize>()
        .with_context(|| format!("invalid byte length `{value}`"))?;
    if len == 0 {
        return Err(anyhow!("byte length must be greater than 0"));
    }
    Ok(len)
}

fn parse_binary_offset_len(value: &str) -> Result<(usize, usize)> {
    let (offset, len) = value
        .rsplit_once('-')
        .ok_or_else(|| anyhow!("binary span must be `offset-len`"))?;
    Ok((
        parse_byte_offset(offset.trim())?,
        parse_byte_len(len.trim())?,
    ))
}

fn parse_binary_append_span(value: &str) -> Result<(BinaryAppendTarget, usize)> {
    let (offset, len) = value
        .rsplit_once('-')
        .ok_or_else(|| anyhow!("binary append span must be `offset-len`"))?;
    let target = if offset.trim() == "-1" {
        BinaryAppendTarget::End
    } else {
        BinaryAppendTarget::Offset(parse_byte_offset(offset.trim())?)
    };
    Ok((target, parse_byte_len(len.trim())?))
}

fn split_binary_body<'a>(value: &'a str, line: &str) -> Result<(&'a str, &'a str)> {
    value
        .split_once(':')
        .map(|(span, body)| (span.trim(), body))
        .ok_or_else(|| anyhow!("binary patch instruction `{line}` is missing `:HEX`"))
}

fn ensure_binary_body_len(actual: usize, expected: usize, line: &str) -> Result<()> {
    if actual != expected {
        return Err(anyhow!(
            "binary patch instruction `{line}` declares {expected} byte(s) but contains {actual} byte(s)"
        ));
    }
    Ok(())
}

fn apply_binary_hunks(bytes: &[u8], hunks: &[BinaryHunk]) -> Result<BinaryPatchApplied> {
    let mut ops = hunks
        .iter()
        .map(|hunk| hunk_to_binary_operation(hunk, bytes.len()))
        .collect::<Result<Vec<_>>>()?;
    ops.sort_by_key(|op| (op.start, op.end > op.start, op.order));

    let mut output = Vec::new();
    let mut cursor = 0usize;
    let additions = ops.iter().map(|op| op.additions).sum();
    let deletions = ops.iter().map(|op| op.deletions).sum();
    for op in ops {
        if op.start < cursor {
            return Err(anyhow!(
                "binary patch hunks overlap or target already replaced bytes"
            ));
        }
        output.extend_from_slice(&bytes[cursor..op.start]);
        output.extend(op.replacement);
        cursor = op.end;
    }
    output.extend_from_slice(&bytes[cursor..]);
    Ok(BinaryPatchApplied {
        bytes: output,
        additions,
        deletions,
    })
}

fn hunk_to_binary_operation(hunk: &BinaryHunk, total: usize) -> Result<BinaryOperation> {
    let (start, end, additions, deletions) = match hunk.anchor {
        BinaryAnchor::Replace { offset, len } => {
            let end = byte_range_end(offset, len, total)?;
            (offset, end, len, len)
        }
        BinaryAnchor::Delete { offset, len } => {
            let end = byte_range_end(offset, len, total)?;
            (offset, end, 0, len)
        }
        BinaryAnchor::Append {
            target: BinaryAppendTarget::Offset(offset),
            len,
        } => {
            ensure_append_offset(offset, total)?;
            (offset, offset, len, 0)
        }
        BinaryAnchor::Append {
            target: BinaryAppendTarget::End,
            len,
        } => (total, total, len, 0),
    };

    Ok(BinaryOperation {
        start,
        end,
        replacement: hunk.bytes.clone(),
        order: hunk.order,
        additions,
        deletions,
    })
}

fn byte_range_end(offset: usize, len: usize, total: usize) -> Result<usize> {
    offset
        .checked_add(len)
        .filter(|end| *end <= total)
        .ok_or_else(|| {
            anyhow!(
                "byte range {}..{} is out of range for this file ({total} bytes)",
                offset,
                offset.saturating_add(len)
            )
        })
}

fn ensure_append_offset(offset: usize, total: usize) -> Result<()> {
    if offset > total {
        return Err(anyhow!(
            "append offset {offset} is out of range for this file ({total} bytes); use offset 0 for the start or -1 for the end"
        ));
    }
    Ok(())
}
pub(super) fn decode_hex(text: &str) -> Result<Vec<u8>> {
    let compact = text
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.strip_prefix("0x")
                .or_else(|| part.strip_prefix("0X"))
                .unwrap_or(part)
        })
        .collect::<Vec<_>>()
        .join("");
    if compact.len() % 2 != 0 {
        return Err(anyhow!(
            "hex byte content must contain an even number of digits"
        ));
    }
    (0..compact.len())
        .step_by(2)
        .map(|idx| {
            u8::from_str_radix(&compact[idx..idx + 2], 16)
                .map_err(|err| anyhow!("invalid hex byte at digit {idx}: {err}"))
        })
        .collect()
}
