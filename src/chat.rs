use serde::{Deserialize, Serialize};

pub const HISTORY_LIMIT: usize = 200;
pub const MAX_MESSAGE_CHARS: usize = 4_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDirection {
    Incoming,
    Outgoing,
}

/// Public conversation data. iLink credentials and user IDs never enter this model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatMessage {
    pub id: String,
    pub account_id: String,
    pub direction: MessageDirection,
    pub text: String,
    pub created_at_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessageInput {
    pub text: String,
}

impl SendMessageInput {
    pub fn validate(self) -> Result<String, String> {
        let text = self.text.trim().to_owned();
        if text.is_empty() || text.chars().count() > MAX_MESSAGE_CHARS {
            return Err("text must contain between 1 and 4000 characters".to_owned());
        }
        Ok(text)
    }
}

#[derive(Serialize)]
pub struct MessageHistory {
    pub account_id: String,
    pub messages: Vec<ChatMessage>,
    pub limit: usize,
}

#[derive(Serialize)]
pub struct SendMessageResponse {
    pub message: ChatMessage,
    /// An upstream acceptance is still a success if the local history write fails.
    pub history_saved: bool,
}
