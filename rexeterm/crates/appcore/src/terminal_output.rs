//! On-demand text presentation for MCP/CLI. GUI streams go directly to xterm.
use crate::protocol::ExecutorResponse;
use serde_json::{json, Value};

pub(super) fn render(response: &mut ExecutorResponse) {
    if !response.ok {
        return;
    }
    let Some(result) = response.result.as_mut() else {
        return;
    };
    let terminal = &result["metadata"]["terminalOutput"];
    if terminal["format"] != "raw-pty-v1" {
        return;
    }
    // Completed run output remains a transcript, not a last-screen projection.
    if terminal["view"] == "stream" {
        return;
    }
    let cols = terminal["cols"].as_u64().unwrap_or(80).clamp(1, 4096) as u16;
    let rows = terminal["rows"].as_u64().unwrap_or(24).clamp(1, 4096) as u16;
    let text = result["output"]["text"].as_str().unwrap_or_default();
    let mut terminal = avt::Terminal::new(cols as usize, rows as usize).expect("terminal size is non-zero");
    terminal.feed(text);
    let mut lines: Vec<_> = terminal
        .screen()
        .visible_lines()
        .take(rows as usize)
        .map(|line| line.text().trim_end().to_string())
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let plain = lines.join("\n");
    result["output"]["text"] = json!(plain);
    if result["metadata"].get("output").is_some() {
        result["metadata"]["output"] = json!(plain);
    }
    if result["metadata"].get("rawPretty").is_some() {
        result["metadata"]["rawPretty"] = json!(terminal.dump());
    }
    result["metadata"]["terminalOutput"]["format"] = Value::String("client-screen-v1".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_is_rendered_on_client_and_foreign_results_are_left_intact() {
        let mut response = ExecutorResponse::ok(
            json!(1),
            None,
            json!({"metadata":{"terminalOutput":{"format":"raw-pty-v1","cols":80,"rows":24},"rawPretty":""},"output":{"text":"old\r\x1b[2K\x1b[31mnew\x1b[0m\r\n中文"}}),
        );
        render(&mut response);
        let result = response.result.unwrap();
        assert_eq!(result["output"]["text"], "new\n中文");
        assert!(result["metadata"]["rawPretty"]
            .as_str()
            .unwrap()
            .contains("new"));
        let mut plain =
            ExecutorResponse::ok(json!(1), None, json!({"output":{"text":"untouched\rtext"}}));
        render(&mut plain);
        assert_eq!(plain.result.unwrap()["output"]["text"], "untouched\rtext");
    }


    #[test]
    fn render_keeps_mapped_box_characters() {
        let mut response = ExecutorResponse::ok(
            json!(1),
            None,
            json!({"metadata":{"terminalOutput":{"format":"raw-pty-v1","cols":20,"rows":4,"view":"screen"}},"output":{"text":"\x1b(0lqqk\x1b(B\r\n"}}),
        );
        render(&mut response);
        let text = response.result.unwrap()["output"]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains('┌'), "{text:?}");
    }
}
