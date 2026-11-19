//! One bounded card per invocation; payload text lives in ___ blocks, never in fences.
pub use crate::commands::Sink;
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::Value;
use std::{
    fmt::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Where payload blocks go: inline text, a file, or both.
#[derive(Clone, Debug)]
pub struct Options {
    pub sinks: Vec<Sink>,
    pub out_dir: Option<PathBuf>,
    /// Inline bodies longer than this are cut; selected file sinks keep the payload.
    pub text_limit: usize,
    /// Exact destination for `-raw`; overrides the generated name.
    pub raw_path: Option<PathBuf>,
    /// Exact destination for `-png`.
    pub png_path: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            sinks: vec![Sink::Text],
            out_dir: None,
            text_limit: 64 * 1024,
            raw_path: None,
            png_path: None,
        }
    }
}

impl Options {
    fn inline(&self) -> bool {
        self.sinks.iter().any(|sink| matches!(sink, Sink::Text))
    }
}

fn safe(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && !matches!(c, '\n' | '\t' | '\r') {
                '�'
            } else {
                c
            }
        })
        .collect()
}
/// Payload block: `___` / kind / `___` / text / `___`.
/// `_` renders as a thematic break but can never become a setext underline, so the
/// kind line stays a paragraph. The delimiter grows when the payload has `___` lines.
fn block(text: &str, output: &mut String) {
    let text = safe(&text.replace("\r\n", "\n"));
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let longest = text
        .lines()
        .map(|line| line.trim_end().chars().take_while(|c| *c == '_').count())
        .max()
        .unwrap_or(0);
    let marker = "_".repeat((longest + 1).max(3));
    let _ = writeln!(output, "{marker}\n{text}\n{marker}\n");
}
/// Field names are identifiers: keep them verbatim so results stay copy-pasteable.
fn key_label(text: &str) -> String {
    safe(text).replace(['\n', '\t'], " ")
}
/// Values keep the escapes that could break emphasis, code spans or tables.
fn label(text: &str) -> String {
    safe(text)
        .replace(['\n', '\t'], " ")
        .chars()
        .flat_map(|c| {
            if "`*[]|\\".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}
/// Inline code for a single-line value; picks a backtick run the value cannot close.
fn code(text: &str) -> String {
    let text = safe(text);
    if text.contains('\n') {
        return text;
    }
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let marker = "`".repeat((longest + 1).max(1));
    format!("{marker}{text}{marker}")
}

/// Cut a body at a char boundary so the card never shows half a UTF-8 sequence.
fn truncate(body: &str, limit: usize) -> &str {
    if body.len() <= limit {
        return body;
    }
    let mut end = limit;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    &body[..end]
}

fn terminal_size(meta: &Value) -> (u64, u64) {
    let cols = meta["terminalOutput"]["cols"].as_u64().unwrap_or(80);
    let rows = meta["terminalOutput"]["rows"].as_u64().unwrap_or(24);
    (cols, rows)
}

fn write_sink(
    body: &[u8],
    field: &str,
    ext: &str,
    meta: &Value,
    options: &Options,
) -> Result<PathBuf> {
    let explicit = match ext {
        "bin" => options.raw_path.as_ref(),
        "png" => options.png_path.as_ref(),
        _ => None,
    };
    if let Some(path) = explicit {
        if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create output directory {}", parent.display()))?;
        }
        std::fs::write(path, body)
            .with_context(|| format!("cannot write output sink {}", path.display()))?;
        return Ok(path.clone());
    }
    let dir = options
        .out_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(".rexeterm-out"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create output directory {}", dir.display()))?;
    let (cols, rows) = terminal_size(meta);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    for attempt in 0.. {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let path = dir.join(format!("{stamp}-{cols}x{rows}-{field}{suffix}.{ext}"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match file {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(body)
                    .with_context(|| format!("cannot write output sink {}", path.display()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot open output sink {}", path.display()))
            }
        }
    }
    unreachable!("unbounded suffix generator")
}

fn emit_payload(
    field: &str,
    body: &str,
    io: &str,
    meta: &Value,
    options: &Options,
    out: &mut String,
) -> Result<()> {
    if options.inline() {
        let shown = truncate(body, options.text_limit);
        let _ = writeln!(out, ">mode: {}", mode(io, false, meta, body));
        let _ = writeln!(out, "# {io}-{field}\n");
        block(shown, out);
        if shown.len() < body.len() {
            let _ = writeln!(
                out,
                ">**truncated**: {} of {} bytes",
                shown.len(),
                body.len()
            );
        }
    }
    if options.sinks.contains(&Sink::File) {
        let path = write_sink(body.as_bytes(), field, "txt", meta, options)?;
        let _ = writeln!(out, ">mode: {io}-text-file");
        if !options.inline() {
            let _ = writeln!(out, "# {io}-{field}\n");
        }
        let _ = writeln!(out, ">**file**: {}", code(&path.display().to_string()));
        out.push('\n');
    }
    Ok(())
}

/// Container headings always start a new block, so lists above them stay intact.
fn heading(out: &mut String, depth: usize, name: &str) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    let _ = writeln!(out, "{} {}\n", "#".repeat((depth + 2).min(6)), name);
}
/// Task card: `>` title, `>>` description, `>>>` command and path,
/// followed by identity, exit state and payload blocks.
fn task_card(meta: &Value, output: &Value, options: &Options, out: &mut String) -> Result<()> {
    let text = |key: &str| meta[key].as_str().unwrap_or_default().to_string();
    let title = match text("title") {
        title if title.is_empty() => "未命名".to_string(),
        title => title,
    };
    let description = match text("description") {
        description if description.is_empty() => "—".to_string(),
        description => description,
    };
    // cmd is the shell the executor picked (bash -lc, powershell.exe -Command, ...),
    // not the command line: argv[0] plus its leading flags.
    let argv: Vec<String> = meta["argv"]
        .as_array()
        .map(|argv| {
            argv.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut shell: Vec<&str> = Vec::new();
    for (index, part) in argv.iter().enumerate() {
        if index == 0 || part.starts_with('-') {
            shell.push(part);
        } else {
            break;
        }
    }
    let _ = writeln!(out, ">{title}");
    let _ = writeln!(out, ">>{description}");
    let mut context = false;
    for (label, value) in [("cmd", shell.join(" ")), ("path", text("cwd"))] {
        if !value.is_empty() {
            let _ = writeln!(out, ">>>**{label}:**{}", code(&value));
            context = true;
        }
    }
    if context {
        out.push('\n');
    }
    let _ = writeln!(out, ">asyncID: {}", code(&text("asyncID")));
    if let Some(pid) = meta.get("pid").filter(|pid| !pid.is_null()) {
        let _ = writeln!(out, ">pid: {pid}");
    }
    match meta.get("exitCode") {
        None | Some(Value::Null) => {
            let state = meta["state"]
                .as_str()
                .filter(|state| !state.is_empty())
                .unwrap_or("running");
            let _ = writeln!(out, ">{}", label(state));
        }
        Some(exit) => {
            let _ = writeln!(out, ">exitCode: {exit}");
        }
    }
    out.push('\n');

    // The label already says what the block is, so the block itself stays bare.
    let io = match text("io") {
        io if !io.is_empty() => io,
        _ if meta.get("terminalOutput").is_some() || !text("asyncID").starts_with("stdio-") => {
            "pty".to_string()
        }
        _ => "stdio".to_string(),
    };
    let body = |key: &str| {
        output[key]
            .as_str()
            .or_else(|| meta[key].as_str())
            .filter(|body| !body.is_empty())
            .map(str::to_string)
    };
    let emit = |field: &str, body: &str, out: &mut String| {
        emit_payload(field, body, &io, meta, options, out).with_context(|| {
            format!(
                "cannot save output for asyncID {}",
                meta["asyncID"].as_str().unwrap_or_default()
            )
        })
    };
    if !text("command").is_empty() {
        emit("command", &text("command"), out)?;
    }
    for key in ["stdout", "stderr", "text", "message", "info", "file"] {
        if let Some(body) = body(key) {
            emit(key, &body, out)?;
        }
    }
    if io == "pty" {
        for (sink, key, ext, mode) in [
            (Sink::Raw, "rawBase64", "bin", "pty-raw-file"),
            (Sink::Png, "pngBase64", "png", "pty-png-file"),
        ] {
            if options.sinks.contains(&sink) {
                let data = output[key].as_str()
                    .with_context(|| format!("rexetermd did not return {mode} for asyncID {}; verify the bound daemon is up to date", text("asyncID")))?;
                let path = write_sink(&BASE64.decode(data)?, "text", ext, meta, options)?;
                let _ = writeln!(
                    out,
                    ">mode: {mode}\n\n>**file**: {}\n",
                    code(&path.display().to_string())
                );
            }
        }
        let terminal = &meta["terminalOutput"];
        if let Some(error) = terminal["error"].as_str() {
            let _ = writeln!(out, ">**PTY stream**: {}\n", label(error));
        } else if terminal["capture"] == "legacy-initial-plus-live" {
            out.push_str(
                ">**PTY capture**: This executor can miss output between start and attach.\n\n",
            );
        } else if terminal["capture"] == "live-only" {
            out.push_str(">**PTY capture**: Output starts at attach; earlier screen contents are unavailable.\n\n");
        }
    }
    let mut footer: Vec<(&str, String)> = Vec::new();
    for key in [
        "startedAt",
        "endedAt",
        "outputLimitBytes",
        "from",
        "next_offset",
        "dropped_bytes",
    ] {
        if let Some(value) = meta.get(key).filter(|value| !value.is_null()) {
            footer.push((
                key,
                match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                },
            ));
        }
    }
    if !footer.is_empty() {
        for (key, value) in &footer {
            let _ = writeln!(out, ">**{key}**: {value}");
        }
        out.push('\n');
    }
    Ok(())
}

fn mode(io: &str, file: bool, meta: &Value, body: &str) -> String {
    let render = if body.starts_with('\u{89}') {
        "png"
    } else if io == "pty" {
        match meta["terminalOutput"]["format"]
            .as_str()
            .unwrap_or_default()
        {
            "client-screen-v1" => "tui",
            _ => "raw",
        }
    } else {
        "text"
    };
    match (io, file) {
        // stdio keeps two kinds only: inline text or a file reference.
        ("stdio", true) => "stdio-file".to_string(),
        ("stdio", false) => "stdio-text".to_string(),
        // pty keeps the render kind and marks a file reference with a -file suffix.
        (_, true) => format!("{io}-{render}-file"),
        (_, false) => format!("{io}-{render}"),
    }
}

fn fields(value: &Value, depth: usize, options: &Options, out: &mut String) -> Result<()> {
    // Tool payloads have their own text representation; do not print duplicate JSON text.
    if let Some(obj) = value.as_object() {
        if obj.contains_key("metadata") && obj.contains_key("output") {
            if obj["metadata"]
                .get("asyncID")
                .and_then(Value::as_str)
                .is_some()
            {
                return task_card(&obj["metadata"], &obj["output"], options, out);
            }
            fields(&obj["metadata"], depth, options, out)?;
            let output = &obj["output"];
            for key in ["message", "text", "info"] {
                if let Some(text) = output[key].as_str().filter(|s| !s.is_empty()) {
                    if key == "text"
                        && serde_json::from_str::<Value>(text).is_ok_and(|v| v == obj["metadata"])
                    {
                        continue;
                    }
                    let _ = writeln!(out, "- **{}**:\n", key_label(key));
                    block(text, out);
                }
            }
            return Ok(());
        }
        if obj.is_empty() {
            out.push_str("(empty)\n\n");
        }
        // Order: the executed argv first, then plain keys, then nested blocks and
        // multi-line payloads, so a card reads top-down instead of alphabetically.
        let mut blocks: Vec<(&String, &Value)> = Vec::new();
        if let Some(argv) = obj.get("argv") {
            heading(out, depth, "argv");
            fields(argv, depth + 1, options, out)?;
        }
        for (key, value) in obj {
            if key == "argv" {
                continue;
            }
            match value {
                Value::Object(_) | Value::Array(_) => blocks.push((key, value)),
                Value::String(text) if text.contains('\n') => blocks.push((key, value)),
                scalar => {
                    let shown = match scalar {
                        Value::String(s) if s.is_empty() => "—".to_string(),
                        Value::String(s) => label(s),
                        other => other.to_string(),
                    };
                    let _ = writeln!(out, "- **{}**: {}", key_label(key), shown);
                }
            }
        }
        for (key, value) in blocks {
            let name = key_label(key);
            match value {
                Value::String(text) => {
                    let _ = writeln!(out, "- **{}**:\n", name);
                    block(text, out);
                }
                _ => {
                    heading(out, depth, &name);
                    fields(value, depth + 1, options, out)?;
                }
            }
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
    } else if let Some(rows) = value.as_array() {
        if rows.is_empty() {
            out.push_str("(empty)\n\n");
        } else if rows.iter().all(|row| !row.is_array() && !row.is_object()) {
            let text = rows
                .iter()
                .map(|row| match row {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            block(&text, out);
        } else {
            for (index, row) in rows.iter().enumerate() {
                let name = ["name", "id", "asyncID", "host", "title"]
                    .iter()
                    .find_map(|key| row[*key].as_str())
                    .unwrap_or_default();
                let suffix = if name.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", label(name))
                };
                heading(out, depth, &format!("Item {}{}", index + 1, suffix));
                fields(row, depth + 1, options, out)?;
            }
        }
    } else if let Some(text) = value.as_str() {
        block(text, out);
    } else {
        let _ = writeln!(out, "{value}\n");
    }
    Ok(())
}
pub fn render(value: &Value) -> String {
    render_with(value, &Options::default()).expect("inline rendering does not write files")
}

pub fn render_with(value: &Value, options: &Options) -> Result<String> {
    let mut out = String::from("# Result\n\n");
    fields(value, 0, options, &mut out)?;
    Ok(out)
}
pub fn error(message: &str) -> String {
    let mut out = String::from("# Error\n\n");
    block(message, &mut out);
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn field_names_stay_verbatim_and_empty_scalars_show_a_dash() {
        let out = render(&json!({"buffered_bytes":6,"next_offset":6,"description":""}));
        assert!(out.contains("- **buffered_bytes**: 6"), "{out}");
        assert!(out.contains("- **next_offset**: 6"), "{out}");
        assert!(out.contains("- **description**: —"), "{out}");
        assert!(!out.contains("<details>"), "{out}");
        assert!(!out.contains('\\'), "{out}");
    }

    #[test]
    fn task_card_reports_the_exit_code_once_the_task_stopped() {
        let out = render(&json!({
            "metadata": {
                "asyncID": "t",
                "state": "stopped",
                "exitCode": 3,
                "pid": 7,
                "endedAt": 9,
                "detached": false,
                "totalOutput": 5,
            },
            "output": {},
        }));
        assert!(out.contains(">pid: 7"), "{out}");
        assert!(out.contains(">exitCode: 3"), "{out}");
        assert!(!out.contains("running"), "{out}");
    }

    #[test]
    fn scalar_arrays_render_as_one_block_without_item_headings() {
        let out = render(&json!({"argv":["C:/x.exe","--wait","5"]}));
        assert!(out.contains("___\nC:/x.exe\n--wait\n5\n___"), "{out}");
        assert!(!out.contains("Item 1"), "{out}");
    }

    #[test]
    fn object_arrays_label_each_item_with_its_identifier() {
        let out = render(&json!([{"name":"alpha","port":1},{"name":"beta","port":2}]));
        assert!(out.contains("Item 1 — alpha"), "{out}");
        assert!(out.contains("Item 2 — beta"), "{out}");
    }

    #[test]
    fn keeps_remote_text_inside_a_block_and_strips_terminal_controls() {
        let text = "hello\n___\n# forged\n\x1b[31m中";
        let output = render(
            &json!({"metadata":{"asyncID":"task-1","state":"running"},"output":{"text":text,"info":"live only"}}),
        );
        assert!(output.starts_with("# Result\n\n"));
        assert!(output.contains("____\nhello\n___\n# forged\n�[31m中\n____"));
        assert!(!output.contains('\x1b'));
        assert_eq!(output.matches("hello").count(), 1);
        assert!(output.contains("task-1") && output.contains("live only"));
    }
}
