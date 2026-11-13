//! Generic ordered stream contract. UDP datagrams need a separate contract.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamTarget {
    pub kind: String,
    pub address: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum StreamMessage {
    #[serde(rename = "re.stream.v1.open")]
    Open { target: StreamTarget },
    #[serde(rename = "re.stream.v1.ready")]
    Ready,
    #[serde(rename = "re.stream.v1.eof")]
    Eof,
    #[serde(rename = "re.stream.v1.error")]
    Error { message: String },
}
