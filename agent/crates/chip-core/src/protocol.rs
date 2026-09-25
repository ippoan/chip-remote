//! WebSocket messages between the Worker (`HubDO`) and the agent. See docs/PROTOCOL.md.

use serde::{Deserialize, Serialize};

/// Chip as sent by the Worker. Only the fields the agent needs; the rest is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WireChip {
    pub task_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub tldr: String,
    #[serde(default)]
    pub status: String,
}

/// Worker → agent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    #[serde(rename = "hello")]
    Hello {
        #[serde(default)]
        chips: Vec<WireChip>,
    },
    #[serde(rename = "chip.new")]
    ChipNew { chip: WireChip },
    #[serde(rename = "chip.withdrawn")]
    ChipWithdrawn { task_id: String },
    #[serde(rename = "action")]
    Action {
        request_id: String,
        task_id: String,
        action: Action,
        #[serde(default)]
        title: String,
        #[serde(default)]
        tldr: String,
    },
    #[serde(rename = "ping")]
    Ping,
    /// Forward compatibility: unknown types are logged and ignored.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Start,
    Dismiss,
}

/// agent → Worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    #[serde(rename = "chip.located")]
    ChipLocated { task_id: String },
    #[serde(rename = "chip.not_found")]
    ChipNotFound { task_id: String },
    #[serde(rename = "action.result")]
    ActionResult {
        request_id: String,
        task_id: String,
        ok: bool,
        error: Option<String>,
    },
    #[serde(rename = "pong")]
    Pong,
}

/// Close code the Worker uses when a newer agent connection replaces this one.
pub const CLOSE_REPLACED: u16 = 4000;

impl ServerMsg {
    pub fn parse(text: &str) -> Result<ServerMsg, serde_json::Error> {
        serde_json::from_str(text)
    }
}

impl ClientMsg {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ClientMsg is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_messages() {
        let m = ServerMsg::parse(r#"{"type":"hello","chips":[{"task_id":"task_1","title":"t","tldr":"d","status":"located_pending","located":false,"extra":1}]}"#).unwrap();
        assert_eq!(
            m,
            ServerMsg::Hello {
                chips: vec![WireChip {
                    task_id: "task_1".into(),
                    title: "t".into(),
                    tldr: "d".into(),
                    status: "located_pending".into()
                }]
            }
        );
        let m = ServerMsg::parse(r#"{"type":"action","request_id":"r","task_id":"task_1","action":"dismiss","title":"t","tldr":"d"}"#).unwrap();
        assert!(matches!(
            m,
            ServerMsg::Action {
                action: Action::Dismiss,
                ..
            }
        ));
        assert_eq!(
            ServerMsg::parse(r#"{"type":"ping"}"#).unwrap(),
            ServerMsg::Ping
        );
        assert_eq!(
            ServerMsg::parse(r#"{"type":"chip.withdrawn","task_id":"x"}"#).unwrap(),
            ServerMsg::ChipWithdrawn {
                task_id: "x".into()
            }
        );
        assert_eq!(
            ServerMsg::parse(r#"{"type":"future.thing","a":1}"#).unwrap(),
            ServerMsg::Unknown
        );
    }

    #[test]
    fn serializes_client_messages() {
        assert_eq!(
            ClientMsg::ChipLocated {
                task_id: "t".into()
            }
            .to_json(),
            r#"{"type":"chip.located","task_id":"t"}"#
        );
        assert_eq!(
            ClientMsg::ActionResult {
                request_id: "r".into(),
                task_id: "t".into(),
                ok: false,
                error: Some("chip_not_found".into())
            }
            .to_json(),
            r#"{"type":"action.result","request_id":"r","task_id":"t","ok":false,"error":"chip_not_found"}"#
        );
        assert_eq!(ClientMsg::Pong.to_json(), r#"{"type":"pong"}"#);
    }
}
