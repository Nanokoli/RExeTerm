//! Authentication is separate from persistent client/resource identity.
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthMessage {
    #[serde(rename = "re.auth.v1.login")]
    Login {
        username: String,
        #[serde(flatten)]
        credential: AuthCredential,
    },
    #[serde(rename = "re.auth.v1.authenticated")]
    Authenticated { username: String, ticket: String },
    #[serde(rename = "re.auth.v1.error")]
    Error { message: String },
}
// Deliberately no Debug: passwords and tickets must never appear in request logs.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AuthCredential {
    Password { password: String },
    Certificate,
    Ca,
    Ticket { ticket: String },
}
