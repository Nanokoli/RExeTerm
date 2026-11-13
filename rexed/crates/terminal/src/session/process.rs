//! Resize, process termination and cached exit notifications.
use super::{clamp_size, Session, TermSize};
use anyhow::Result;
use portable_pty::PtySize;
use tokio::sync::mpsc;

impl Session {
    pub fn subscribe_exit_code(&self) -> mpsc::UnboundedReceiver<u32> {
        let (tx, rx) = mpsc::unbounded_channel();
        let code = {
            let mut exit = self.exit.lock().unwrap();
            if let Some(code) = exit.code {
                Some(code)
            } else {
                exit.subscribers.push(tx);
                return rx;
            }
        };
        if let Some(code) = code {
            let _ = tx.send(code);
        }
        rx
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let (cols, rows) = clamp_size(cols, rows);
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        self.master.lock().unwrap().resize(size)?;
        *self.size.lock().unwrap() = TermSize { cols, rows };
        Ok(())
    }

    pub fn kill(&self) -> Result<()> {
        self.child_killer.lock().unwrap().kill()?;
        Ok(())
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    pub fn try_exit_code(&self) -> Result<Option<u32>> {
        Ok(self.exit.lock().unwrap().code)
    }

    pub fn wait_exit_code(&self) -> Result<u32> {
        let mut exit = self.exit.lock().unwrap();
        loop {
            if let Some(code) = exit.code {
                return Ok(code);
            }
            exit = self.exit_signal.wait(exit).unwrap();
        }
    }

    pub(super) fn set_exit_code(&self, code: u32) {
        let subscribers = {
            let mut exit = self.exit.lock().unwrap();
            if exit.code.is_some() {
                return;
            }
            exit.code = Some(code);
            exit.ended_at = Some(super::now_millis());
            std::mem::take(&mut exit.subscribers)
        };
        self.exit_signal.notify_all();
        for tx in subscribers {
            let _ = tx.send(code);
        }
    }
}
