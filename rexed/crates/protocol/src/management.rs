//! RE-owned client sessions and the independent, read-only operations endpoint.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientHello {
    pub username: String,
    pub client_name: String,
    pub client_version: String,
}
impl Default for ClientHello {
    fn default() -> Self {
        Self {
            username: "default".into(),
            client_name: "rexe-client".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}
impl ClientHello {
    pub fn validate(&self) -> Result<(), &'static str> {
        for value in [&self.username, &self.client_name, &self.client_version] {
            if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
                return Err(
                    "client identity fields must contain 1..128 bytes without control characters",
                );
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionOwner {
    #[serde(default)]
    pub client_id: String,
    pub session_id: String,
    pub username: String,
    pub client_name: String,
    pub client_version: String,
}
impl SessionOwner {
    pub fn resource_owner_id(&self) -> &str {
        if self.client_id.is_empty() {
            &self.session_id
        } else {
            &self.client_id
        }
    }
}
/// Possession credential for a persistent client identity. Never include in logs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCredential {
    pub client_id: String,
    pub secret: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceMetadata {
    #[serde(default = "unnamed")]
    pub title: String,
    #[serde(default)]
    pub description: String,
}
pub(crate) fn unnamed() -> String {
    "未命名".into()
}
/// Display metadata shared by tasks, file transfers and forwards.
pub type TaskMetadata = ResourceMetadata;
pub type ChannelMetadata = ResourceMetadata;

impl Default for ResourceMetadata {
    fn default() -> Self {
        Self {
            title: unnamed(),
            description: String::new(),
        }
    }
}
impl ResourceMetadata {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.title.chars().count() > 40 || self.description.chars().count() > 200 {
            return Err(
                "title must be at most 40 characters and description at most 200 characters",
            );
        }
        Ok(())
    }
    pub fn normalized(mut self) -> Self {
        if self.title.trim().is_empty() {
            self.title = unnamed();
        }
        self
    }
}
// Deliberately no Debug: session binding secrets must not enter diagnostics.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCredential {
    pub session_id: String,
    pub secret: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum SessionMessage {
    #[serde(rename = "re.session.v1.open")]
    Open { client: ClientHello },
    #[serde(rename = "re.client.v1.open")]
    OpenClient {
        client: ClientHello,
        credential: ClientCredential,
    },
    #[serde(rename = "re.client.v1.opened")]
    ClientOpened {
        owner: SessionOwner,
        credential: SessionCredential,
    },
    #[serde(rename = "re.session.v1.opened")]
    Opened {
        owner: SessionOwner,
        credential: SessionCredential,
    },
    #[serde(rename = "re.session.v1.bind")]
    Bind {
        credential: SessionCredential,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<ChannelMetadata>,
    },
    #[serde(rename = "re.session.v1.ping")]
    Ping { credential: SessionCredential },
    #[serde(rename = "re.session.v1.close")]
    Close { credential: SessionCredential },
    #[serde(rename = "re.session.v1.ok")]
    Ok,
    #[serde(rename = "re.session.v1.error")]
    Error { message: String },
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficSummary {
    pub received_bytes: u64,
    pub sent_bytes: u64,
    pub receive_bytes_per_second: u64,
    pub send_bytes_per_second: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSessionSummary {
    pub active_links: usize,
    pub traffic: TrafficSummary,
    pub traffic_complete: bool,
    pub owner: SessionOwner,
    pub peer: String,
    pub transport: String,
    pub created_at_ms: u64,
    pub last_seen_at_ms: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Pty,
    File,
    Forward,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSummary {
    pub channel_id: String,
    #[serde(default)]
    pub metadata: ChannelMetadata,
    pub kind: ChannelKind,
    pub owner: Option<SessionOwner>,
    pub peer: String,
    pub transport: String,
    pub target: String,
    pub created_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedClient {
    pub id: String,
    pub peer: String,
    pub owner: Option<SessionOwner>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramSummary {
    pub program_id: String,
    #[serde(default = "unnamed")]
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub pid: Option<u32>,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub created_at_ms: u64,
    pub created_by: Option<SessionOwner>,
    pub state: String,
    pub exit_code: Option<Value>,
    pub controller: Option<String>,
    pub controller_session: Option<SessionOwner>,
    pub attached_clients: Vec<AttachedClient>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementSnapshot {
    pub executor_id: String,
    pub observed_at_ms: u64,
    pub clients: Vec<ClientSessionSummary>,
    pub channels: Vec<ChannelSummary>,
    pub programs: Vec<ProgramSummary>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ManagementMessage {
    #[serde(rename = "re.admin.v1.snapshot")]
    Snapshot { id: u64 },
    #[serde(rename = "re.admin.v1.result")]
    Result {
        id: u64,
        snapshot: ManagementSnapshot,
    },
    #[serde(rename = "re.admin.v1.error")]
    Error { message: String },
}
/// Shared server/client default. No TCP listener is implicitly opened.
pub fn default_management_target(executor_id: &str) -> std::io::Result<String> {
    if executor_id.is_empty()
        || executor_id.len() > 48
        || !executor_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        || matches!(executor_id, "." | "..")
    {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "default socket requires a 1..48 character executor id using letters, digits, '-', '_' or '.'; specify --admin-listen/--target for other ids"));
    }
    #[cfg(unix)]
    {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".cache"))
            })
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "set XDG_RUNTIME_DIR/HOME or an explicit management target",
                )
            })?;
        Ok(format!(
            "unix://{}",
            directory
                .join("rexed")
                .join(format!("{executor_id}.admin.sock"))
                .display()
        ))
    }
    #[cfg(not(unix))]
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "default Unix socket is unavailable; specify a TCP/WS management target",
    ))
}
