//! Startup settings types shared with the protocol data model.
pub use crate::protocol::shell::{default_profiles, ReSettings, ShellProfile, ShellSettings};
pub(super) fn auto_string() -> String {
    "auto".to_string()
}
