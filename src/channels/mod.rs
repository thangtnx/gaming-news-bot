pub mod zalo;

use anyhow::Result;
use zalo::ZaloChannel;

/// Normalized incoming message from any chat platform.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub chat_id: String,
    pub is_group: bool,
    pub text: Option<String>,
}

/// Abstraction for any messaging platform (Zalo, Telegram, Discord, etc.).
pub trait ChatChannel: Send + Sync {
    fn poll_messages(&self) -> Result<Vec<IncomingMessage>>;
    fn send_message(&self, chat_id: &str, text: &str) -> Result<()>;
}

pub enum Channel {
    Zalo(ZaloChannel),
}

impl ChatChannel for Channel {
    fn poll_messages(&self) -> Result<Vec<IncomingMessage>> {
        match self {
            Self::Zalo(c) => c.poll_messages(),
        }
    }

    fn send_message(&self, chat_id: &str, text: &str) -> Result<()> {
        match self {
            Self::Zalo(c) => c.send_message(chat_id, text),
        }
    }
}
