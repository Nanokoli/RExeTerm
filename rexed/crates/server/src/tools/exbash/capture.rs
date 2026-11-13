//! A request reply is bounded independently of live raw PTY streaming.
#[derive(Default)]
pub(super) struct OutputCapture {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}
pub(super) const CAPTURE_LIMIT: usize = 1024 * 1024;
impl OutputCapture {
    pub fn extend(&mut self, bytes: Vec<u8>) {
        let keep = bytes
            .len()
            .min(CAPTURE_LIMIT.saturating_sub(self.bytes.len()));
        self.truncated |= keep < bytes.len();
        self.bytes.extend_from_slice(&bytes[..keep]);
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}
