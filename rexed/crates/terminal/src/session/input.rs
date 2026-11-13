//! Native input bytes and terminal status reply configuration.
use super::Session;
use anyhow::Result;
impl Session {
    pub fn write_input(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }
    /// Enable the Windows headless terminal status reply. The embedding service
    /// disables this while an interactive terminal supplies its own replies.
    pub fn set_auto_status_response(&self, enabled: bool) {
        self.auto_status_response
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }
}
