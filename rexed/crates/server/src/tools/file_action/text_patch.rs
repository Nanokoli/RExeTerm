//! Text patch parsing and application. No filesystem or tool-context dependencies.
use anyhow::{anyhow, Context, Result};

#[derive(Clone, Debug)]
pub(super) struct TextShape {
    bom: bool,
    line_ending: &'static str,
    pub(super) text: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum LinePatchOp {
    Delete {
        start: usize,
        end: usize,
    },
    Move {
        start: usize,
        end: usize,
        after: LineInsertTarget,
    },
    Append {
        after: LineInsertTarget,
        lines: Vec<String>,
    },
    Replace {
        line: usize,
        text: String,
    },
}

#[derive(Clone, Debug)]
struct PendingLineInsertion {
    boundary: usize,
    order: usize,
    kind: PendingLineInsertionKind,
}

#[derive(Clone, Debug)]
enum PendingLineInsertionKind {
    Literal(Vec<String>),
    Move { start: usize, end: usize },
}

#[derive(Clone, Debug)]
struct LineInsertion {
    order: usize,
    lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineRemoval {
    Delete,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineInsertTarget {
    Start,
    After(usize),
    End,
}

impl TextShape {
    pub(super) fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if let Some((offset, byte)) = binary_byte(&bytes) {
            return Err(anyhow!(
                "Cannot patch binary file (binary byte at offset {}: 0x{:02X})",
                offset,
                byte
            ));
        }
        let raw = String::from_utf8(bytes).context("Cannot patch non-UTF-8 text file")?;
        let line_ending = detect_line_ending(&raw);
        let (bom, raw) = raw
            .strip_prefix('\u{FEFF}')
            .map(|text| (true, text.to_string()))
            .unwrap_or((false, raw));
        Ok(Self {
            bom,
            line_ending,
            text: normalize_to_lf(&raw),
        })
    }

    pub(super) fn encode(&self, text: &str) -> Vec<u8> {
        let mut raw = restore_line_endings(text, self.line_ending);
        if self.bom {
            raw.insert(0, '\u{FEFF}');
        }
        raw.into_bytes()
    }
}
pub(super) fn apply_line_text_patch(before_text: &str, patch_text: &str) -> Result<String> {
    if patch_text
        .lines()
        .any(|line| line.trim() == "*** Begin Patch")
    {
        return Err(anyhow!("old patch envelope format is not supported"));
    }
    let ops = parse_line_patch(patch_text)?;
    if ops.is_empty() {
        return Err(anyhow!(
            "patchText did not contain any line patch instructions"
        ));
    }
    apply_line_patch(before_text, &ops)
}

fn parse_line_patch(patch_text: &str) -> Result<Vec<LinePatchOp>> {
    let lines: Vec<&str> = patch_text
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect();
    let mut ops = Vec::new();
    let mut idx = 0usize;
    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();
        idx += 1;
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("***DELETE***") {
            let (start, end) = parse_line_range(rest.trim())
                .with_context(|| format!("invalid DELETE instruction: `{line}`"))?;
            ops.push(LinePatchOp::Delete { start, end });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("***MOVE***") {
            let (range, after) = rest
                .trim()
                .split_once(',')
                .ok_or_else(|| anyhow!("MOVE must be `***MOVE*** start-end,startline`"))?;
            let (start, end) = parse_line_range(range.trim())
                .with_context(|| format!("invalid MOVE range: `{line}`"))?;
            let after = parse_line_insert_target(after.trim())
                .with_context(|| format!("invalid MOVE target: `{line}`"))?;
            ops.push(LinePatchOp::Move { start, end, after });
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("***APPEND_HEAD***") {
            let after = parse_line_insert_target(rest.trim())
                .with_context(|| format!("invalid APPEND_HEAD target: `{line}`"))?;
            let mut block = Vec::new();
            let mut found_end = false;
            while idx < lines.len() {
                let item = lines[idx];
                idx += 1;
                if item.trim() == "***APPEND_END***" {
                    found_end = true;
                    break;
                }
                block.push(item.to_string());
            }
            if !found_end {
                return Err(anyhow!("APPEND_HEAD block is missing ***APPEND_END***"));
            }
            ops.push(LinePatchOp::Append {
                after,
                lines: block,
            });
            continue;
        }
        if trimmed == "***APPEND_END***" {
            return Err(anyhow!("APPEND_END without APPEND_HEAD"));
        }
        if let Some((number, text)) = line.split_once(':') {
            let line_number = parse_line_number(number.trim(), false)
                .with_context(|| format!("invalid replace instruction: `{line}`"))?;
            ops.push(LinePatchOp::Replace {
                line: line_number,
                text: text.to_string(),
            });
            continue;
        }
        return Err(anyhow!(
            "invalid patchText instruction `{line}`; expected `***DELETE*** start-end`, `***MOVE*** start-end,startline`, `***APPEND_HEAD*** startline ... ***APPEND_END***`, or `n:new text`"
        ));
    }
    Ok(ops)
}

fn parse_line_range(value: &str) -> Result<(usize, usize)> {
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| anyhow!("line range must be `start-end`"))?;
    let start = parse_line_number(start.trim(), false)?;
    let end = parse_line_number(end.trim(), false)?;
    if start > end {
        return Err(anyhow!("line range start {start} is after end {end}"));
    }
    Ok((start, end))
}

fn parse_line_number(value: &str, allow_zero: bool) -> Result<usize> {
    let number = value
        .parse::<usize>()
        .with_context(|| format!("expected a positive line number, got `{value}`"))?;
    if number == 0 && !allow_zero {
        return Err(anyhow!("line number must be >= 1"));
    }
    Ok(number)
}

fn parse_line_insert_target(value: &str) -> Result<LineInsertTarget> {
    if value == "-1" {
        return Ok(LineInsertTarget::End);
    }
    let number = parse_line_number(value, true)?;
    if number == 0 {
        Ok(LineInsertTarget::Start)
    } else {
        Ok(LineInsertTarget::After(number))
    }
}

fn apply_line_patch(before_text: &str, ops: &[LinePatchOp]) -> Result<String> {
    let had_final_newline = before_text.ends_with('\n');
    let lines: Vec<String> = if before_text.is_empty() {
        Vec::new()
    } else {
        before_text
            .split_terminator('\n')
            .map(str::to_string)
            .collect()
    };
    let line_count = lines.len();
    let mut removals = vec![None; line_count];
    let mut replacements = vec![None; line_count];
    let mut pending_insertions = Vec::new();

    for (order, op) in ops.iter().enumerate() {
        match op {
            LinePatchOp::Delete { start, end } => {
                ensure_line_range(&lines, *start, *end, "DELETE")?;
                mark_line_removal(&mut removals, *start, *end, LineRemoval::Delete)?;
            }
            LinePatchOp::Move { start, end, after } => {
                ensure_line_range(&lines, *start, *end, "MOVE")?;
                if matches!(after, LineInsertTarget::After(line) if (*start..=*end).contains(line))
                {
                    return Err(anyhow!(
                        "MOVE target line {} is inside moved range {start}-{end}",
                        after.label()
                    ));
                }
                let boundary = after.index(line_count, "MOVE target")?;
                mark_line_removal(&mut removals, *start, *end, LineRemoval::Move)?;
                pending_insertions.push(PendingLineInsertion {
                    boundary,
                    order,
                    kind: PendingLineInsertionKind::Move {
                        start: *start,
                        end: *end,
                    },
                });
            }
            LinePatchOp::Append {
                after,
                lines: block,
            } => {
                let boundary = after.index(line_count, "APPEND_HEAD target")?;
                pending_insertions.push(PendingLineInsertion {
                    boundary,
                    order,
                    kind: PendingLineInsertionKind::Literal(block.clone()),
                });
            }
            LinePatchOp::Replace { line, text } => {
                ensure_line_range(&lines, *line, *line, "replace")?;
                let replacement = &mut replacements[line - 1];
                if replacement.is_some() {
                    return Err(anyhow!("replace line {line} is targeted more than once"));
                }
                *replacement = Some(text.clone());
            }
        }
    }

    for (idx, replacement) in replacements.iter().enumerate() {
        if replacement.is_some() && removals[idx] == Some(LineRemoval::Delete) {
            return Err(anyhow!("replace line {} overlaps deleted line", idx + 1));
        }
    }

    let insertions = resolve_line_insertions(&lines, &replacements, pending_insertions);
    let mut output_lines = Vec::new();
    push_line_insertions(&mut output_lines, &insertions[0]);
    for line_idx in 0..line_count {
        if removals[line_idx].is_none() {
            output_lines.push(line_text(&lines, &replacements, line_idx + 1));
        }
        push_line_insertions(&mut output_lines, &insertions[line_idx + 1]);
    }

    let mut out = output_lines.join("\n");
    if had_final_newline || !output_lines.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

fn mark_line_removal(
    removals: &mut [Option<LineRemoval>],
    start: usize,
    end: usize,
    removal: LineRemoval,
) -> Result<()> {
    for line in start..=end {
        if let Some(existing) = removals[line - 1] {
            return Err(anyhow!(
                "{} line range {start}-{end} overlaps {} line {line}",
                removal.label(),
                existing.label()
            ));
        }
    }
    for line in start..=end {
        removals[line - 1] = Some(removal);
    }
    Ok(())
}

fn resolve_line_insertions(
    lines: &[String],
    replacements: &[Option<String>],
    pending_insertions: Vec<PendingLineInsertion>,
) -> Vec<Vec<LineInsertion>> {
    let mut insertions: Vec<Vec<LineInsertion>> = (0..=lines.len()).map(|_| Vec::new()).collect();
    for pending in pending_insertions {
        let lines = match pending.kind {
            PendingLineInsertionKind::Literal(lines) => lines,
            PendingLineInsertionKind::Move { start, end } => (start..=end)
                .map(|line| line_text(lines, replacements, line))
                .collect(),
        };
        insertions[pending.boundary].push(LineInsertion {
            order: pending.order,
            lines,
        });
    }
    for boundary in &mut insertions {
        boundary.sort_by_key(|insertion| insertion.order);
    }
    insertions
}

fn push_line_insertions(output_lines: &mut Vec<String>, insertions: &[LineInsertion]) {
    for insertion in insertions {
        output_lines.extend(insertion.lines.iter().cloned());
    }
}

fn line_text(lines: &[String], replacements: &[Option<String>], line: usize) -> String {
    replacements[line - 1]
        .as_ref()
        .cloned()
        .unwrap_or_else(|| lines[line - 1].clone())
}

impl LineInsertTarget {
    fn index(self, line_count: usize, label: &str) -> Result<usize> {
        match self {
            LineInsertTarget::Start => Ok(0),
            LineInsertTarget::End => Ok(line_count),
            LineInsertTarget::After(line) => {
                if line > line_count {
                    return Err(anyhow!(
                        "{label} line {line} is out of range for {line_count} line(s)"
                    ));
                }
                Ok(line)
            }
        }
    }

    fn label(self) -> String {
        match self {
            LineInsertTarget::Start => "0".to_string(),
            LineInsertTarget::End => "-1".to_string(),
            LineInsertTarget::After(line) => line.to_string(),
        }
    }
}

impl LineRemoval {
    fn label(self) -> &'static str {
        match self {
            LineRemoval::Delete => "DELETE",
            LineRemoval::Move => "MOVE",
        }
    }
}

fn ensure_line_range(lines: &[String], start: usize, end: usize, label: &str) -> Result<()> {
    if start == 0 || end == 0 || start > end || end > lines.len() {
        return Err(anyhow!(
            "{label} line range {start}-{end} is out of range for {} line(s)",
            lines.len()
        ));
    }
    Ok(())
}
fn detect_line_ending(text: &str) -> &'static str {
    let crlf = text.find("\r\n");
    let lf = text.find('\n');
    match (crlf, lf) {
        (Some(crlf), Some(lf)) if crlf <= lf => "\r\n",
        (Some(_), None) => "\r\n",
        _ => "\n",
    }
}

fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(text: &str, line_ending: &str) -> String {
    if line_ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}
fn binary_byte(bytes: &[u8]) -> Option<(usize, u8)> {
    bytes
        .iter()
        .take(4096)
        .enumerate()
        .find_map(|(idx, byte)| (*byte == 0).then_some((idx, *byte)))
}
