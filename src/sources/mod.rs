pub mod genshin;

use anyhow::Result;
use genshin::GenshinFandomSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Giftcodes,
    #[allow(dead_code)]
    News,
}

impl NotificationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Giftcodes => "giftcodes",
            Self::News => "news",
        }
    }
}

/// A single news/giftcode item produced by a game source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeItem {
    pub game: String,
    pub code: String,
    pub server: String,
    pub reward: String,
    pub expiry: String,
    pub redeem_url: String,
}

impl CodeItem {
    /// Unique deduplication key across games.
    pub fn dedup_key(&self) -> String {
        format!("{}:{}", self.game, self.code)
    }
}

/// Abstraction for any game news or promotional code source.
pub trait NewsSource: Send + Sync {
    fn name(&self) -> &str;
    fn kind(&self) -> NotificationKind;
    fn fetch_active_items(&self) -> Result<Vec<CodeItem>>;
}

pub enum GameSource {
    Genshin(GenshinFandomSource),
}

impl NewsSource for GameSource {
    fn name(&self) -> &str {
        match self {
            Self::Genshin(s) => s.name(),
        }
    }

    fn kind(&self) -> NotificationKind {
        match self {
            Self::Genshin(s) => s.kind(),
        }
    }

    fn fetch_active_items(&self) -> Result<Vec<CodeItem>> {
        match self {
            Self::Genshin(s) => s.fetch_active_items(),
        }
    }
}
