//! Runtime-owned session summaries, independent of wire messages.
use super::{Session, SessionDetail, SessionSummary};
use crate::types::path_text;

impl Session {
    pub fn summary(&self) -> SessionSummary {
        let size = *self.size.lock().unwrap();
        SessionSummary {
            pty: self.name.clone(),
            command: self.command.argv(),
            cols: size.cols,
            rows: size.rows,
            process_id: self.process_id(),
            created_at: self.created_at,
            exit_code: self.try_exit_code().ok().flatten(),
            output_history_bytes: 0,
            output_history_limit: 0,
            output_total_bytes: self.output_total_bytes(),
        }
    }

    pub fn detail(&self) -> SessionDetail {
        let summary = self.summary();
        SessionDetail {
            pty: summary.pty,
            command: summary.command,
            cwd: path_text(self.cwd.as_deref()),
            env: self.command.env.clone(),
            process_id: summary.process_id,
            created_at: self.created_at,
            cols: summary.cols,
            rows: summary.rows,
            output_history_bytes: summary.output_history_bytes,
            output_history_limit: summary.output_history_limit,
            output_total_bytes: summary.output_total_bytes,
            exit_code: summary.exit_code,
            ended_at: self.exit.lock().unwrap().ended_at,
        }
    }

    pub fn summary_line(&self) -> String {
        let summary = self.summary();
        format!(
            "pty={} cmd={} size={}x{}",
            summary.pty,
            summary.command.join(" "),
            summary.cols,
            summary.rows
        )
    }
}
