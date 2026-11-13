//! Compatible terminal wire messages owned by RE; imported from pty-t protocol.
//! Source: Yeamika/pty-t at 145981f2308b7ce8ddd41abc63407d2e3a9e8b24.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientText {
    Hello {
        id: String,
        pty: String,
        cols: u16,
        rows: u16,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    RequestControl,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdminText {
    Create {
        pty: String,
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        cols: Option<u16>,
        rows: Option<u16>,
    },
    List,
    Detail {
        pty: String,
    },
    Control {
        pty: String,
        id: String,
    },
    ResizePty {
        pty: String,
        cols: u16,
        rows: u16,
    },
    Send {
        pty: String,
        data: String,
    },
    Kill {
        pty: String,
    },
    HistoryLimit {
        pty: String,
        bytes: usize,
    },
    Listen {
        addr: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientSummary {
    pub id: String,
    pub peer_addr: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub pty: String,
    #[serde(default = "super::management::unnamed")]
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub command: Vec<String>,
    pub controller: Option<String>,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub process_id: Option<u32>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub exit_code: Option<u32>,
    #[serde(default)]
    pub output_history_bytes: usize,
    #[serde(default)]
    pub output_history_limit: usize,
    #[serde(default)]
    pub output_total_bytes: u64,
    pub clients: Vec<String>,
    pub client_details: Vec<ClientSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDetail {
    pub pty: String,
    #[serde(default = "super::management::unnamed")]
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
    pub process_id: Option<u32>,
    pub created_at: u64,
    pub controller: Option<String>,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub output_history_bytes: usize,
    #[serde(default)]
    pub output_history_limit: usize,
    #[serde(default)]
    pub output_total_bytes: u64,
    pub clients: Vec<String>,
    pub client_details: Vec<ClientSummary>,
    pub exit_code: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerText {
    Meta {
        id: String,
        pty: String,
        role: String,
        #[serde(default)]
        read_only: bool,
        cols: u16,
        rows: u16,
        #[serde(default)]
        exit_code: Option<u32>,
    },
    Error {
        message: String,
    },
    Info {
        message: String,
    },
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    Session {
        session: SessionDetail,
    },
}
