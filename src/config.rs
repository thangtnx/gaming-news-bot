use crate::sources::NotificationKind;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub bot: BotConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
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

const DEFAULT_DISCORD_WELCOME: &str = "\
# 🎮 Gaming News Bot
-# Cập nhật Giftcode & tin tức game tự động

### 📌 Lệnh chung (tất cả game)
- `/subscribe` *(`/sub`)* — Đăng ký nhận thông báo mới
- `/unsubscribe` *(`/unsub`)* — Hủy đăng ký nhận thông báo
- `/codes` — Xem giftcode đang còn hiệu lực

### 🎯 Lệnh theo từng game
- **Genshin Impact (`gi`)**
  - `/gi codes` — Xem code Genshin còn hiệu lực
  - `/gi subscribe` — Đăng ký nhận code Genshin
  - `/gi unsubscribe` — Hủy đăng ký Genshin

-# Mẹo: Gõ `/` trong khung chat để chọn nhanh lệnh có sẵn.";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiscordConfig {
    pub api_base: String,
    pub max_message_chars: usize,
    pub reconnect_delay_secs: u64,
    pub embed_color: u32,
    pub max_codes_per_embed: usize,
    pub welcome: String,
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            api_base: "https://discord.com/api/v10".to_string(),
            max_message_chars: 2000,
            reconnect_delay_secs: 5,
            embed_color: 5_793_266,
            max_codes_per_embed: 10,
            welcome: DEFAULT_DISCORD_WELCOME.to_string(),
        }
    }
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
    #[serde(default = "default_unknown_game")]
    pub unknown_game: String,
    pub new_codes_header: String,
    pub active_codes_header: String,
    pub redeem_url: String,
}

fn default_unknown_game() -> String {
    "Không tìm thấy game này. Dùng /help để xem các lệnh đang hỗ trợ.".to_string()
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
