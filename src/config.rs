use crate::sources::NotificationKind;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub bot: BotConfig,
    pub intervals: IntervalsConfig,
    pub genshin: GenshinConfig,
    pub messages: MessagesConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BotConfig {
    pub zalo_api_base: String,
    pub zalo_poll_timeout_secs: u64,
    pub state_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntervalsConfig {
    pub giftcodes_secs: u64,
    pub news_secs: u64,
}

impl IntervalsConfig {
    pub fn for_kind(&self, kind: NotificationKind) -> Duration {
        let secs = match kind {
            NotificationKind::Giftcodes => self.giftcodes_secs,
            NotificationKind::News => self.news_secs,
        };
        Duration::from_secs(secs)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenshinConfig {
    pub fandom_api_url: String,
    pub allowed_servers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesConfig {
    pub welcome: String,
    pub subscribed: String,
    pub already_subscribed: String,
    pub unsubscribed: String,
    pub not_subscribed: String,
    pub no_active_codes: String,
    pub fetch_error: String,
    pub new_codes_header: String,
    pub active_codes_header: String,
    pub redeem_url: String,
}

impl AppConfig {
    /// Loads application configuration from the given TOML file path.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file at {}", path.display()))?;
        toml::from_str(&raw)
            .with_context(|| format!("Failed to parse TOML config at {}", path.display()))
    }
}
