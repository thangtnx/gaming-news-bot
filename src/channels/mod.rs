pub mod discord;
pub mod zalo;

use crate::sources::CodeItem;
use anyhow::Result;
use discord::DiscordChannel;
use zalo::ZaloChannel;

/// Normalized incoming message from any chat platform.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub chat_id: String,
    pub is_group: bool,
    pub auto_subscribe: bool,
    pub text: Option<String>,
    pub reply_context: Option<String>,
}

/// Abstraction for any messaging platform (Zalo, Telegram, Discord, etc.).
pub trait ChatChannel: Send + Sync {
    fn name(&self) -> &'static str;
    fn poll_messages(&self) -> Result<Vec<IncomingMessage>>;
    fn send_message(&self, chat_id: &str, text: &str) -> Result<()>;
    fn reply_message(
        &self,
        chat_id: &str,
        _reply_context: Option<&str>,
        text: &str,
    ) -> Result<()> {
        self.send_message(chat_id, text)
    }
    fn send_codes(
        &self,
        chat_id: &str,
        _header: &str,
        _items: &[CodeItem],
        fallback_text: &str,
    ) -> Result<()> {
        self.send_message(chat_id, fallback_text)
    }
    fn reply_codes(
        &self,
        chat_id: &str,
        reply_context: Option<&str>,
        _header: &str,
        _items: &[CodeItem],
        fallback_text: &str,
    ) -> Result<()> {
        self.reply_message(chat_id, reply_context, fallback_text)
    }
}

pub enum Channel {
    Zalo(ZaloChannel),
    Discord(DiscordChannel),
}

impl ChatChannel for Channel {
    fn name(&self) -> &'static str {
        match self {
            Self::Zalo(c) => c.name(),
            Self::Discord(c) => c.name(),
        }
    }

    fn poll_messages(&self) -> Result<Vec<IncomingMessage>> {
        match self {
            Self::Zalo(c) => c.poll_messages(),
            Self::Discord(c) => c.poll_messages(),
        }
    }

    fn send_message(&self, chat_id: &str, text: &str) -> Result<()> {
        match self {
            Self::Zalo(c) => c.send_message(chat_id, text),
            Self::Discord(c) => c.send_message(chat_id, text),
        }
    }

    fn reply_message(
        &self,
        chat_id: &str,
        reply_context: Option<&str>,
        text: &str,
    ) -> Result<()> {
        match self {
            Self::Zalo(c) => c.reply_message(chat_id, reply_context, text),
            Self::Discord(c) => c.reply_message(chat_id, reply_context, text),
        }
    }

    fn send_codes(
        &self,
        chat_id: &str,
        header: &str,
        items: &[CodeItem],
        fallback_text: &str,
    ) -> Result<()> {
        match self {
            Self::Zalo(c) => c.send_codes(chat_id, header, items, fallback_text),
            Self::Discord(c) => c.send_codes(chat_id, header, items, fallback_text),
        }
    }

    fn reply_codes(
        &self,
        chat_id: &str,
        reply_context: Option<&str>,
        header: &str,
        items: &[CodeItem],
        fallback_text: &str,
    ) -> Result<()> {
        match self {
            Self::Zalo(c) => c.reply_codes(chat_id, reply_context, header, items, fallback_text),
            Self::Discord(c) => {
                c.reply_codes(chat_id, reply_context, header, items, fallback_text)
            }
        }
    }
}
