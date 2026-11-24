use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Formats a platform-scoped subscriber identifier while keeping bare Zalo IDs backward-compatible.
pub fn format_subscriber_id(platform: &str, chat_id: &str) -> String {
    if platform == "zalo" {
        chat_id.to_string()
    } else {
        format!("{platform}:{chat_id}")
    }
}

/// Parses a stored subscriber identifier into `(platform, chat_id)`, defaulting unprefixed IDs to `"zalo"`.
pub fn parse_subscriber_id(subscriber_id: &str) -> (&str, &str) {
    if let Some(id) = subscriber_id.strip_prefix("discord:") {
        ("discord", id)
    } else if let Some(id) = subscriber_id.strip_prefix("zalo:") {
        ("zalo", id)
    } else {
        ("zalo", subscriber_id)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedData {
    subscribers: BTreeSet<String>,
    #[serde(default)]
    game_subscribers: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    unsubscribed_groups: BTreeSet<String>,
    sent_codes: BTreeSet<String>,
}

#[derive(Debug)]
pub struct BotState {
    path: PathBuf,
    data: PersistedData,
}

impl BotState {
    /// Loads persisted state from `path`, or initializes empty state if the file does not exist.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let data = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read state file {}", path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("Failed to parse state file {}", path.display()))?
        } else {
            PersistedData::default()
        };

        Ok(Self { path, data })
    }

    pub fn subscribe(&mut self, chat_id: &str) -> Result<bool> {
        self.data.unsubscribed_groups.remove(chat_id);
        let inserted = self.data.subscribers.insert(chat_id.to_string());
        self.save()?;
        Ok(inserted)
    }

    /// Auto-subscribes a group chat unless it explicitly ran `/unsubscribe`.
    pub fn auto_subscribe_group(&mut self, chat_id: &str) -> Result<bool> {
        if self.data.unsubscribed_groups.contains(chat_id) {
            return Ok(false);
        }
        let inserted = self.data.subscribers.insert(chat_id.to_string());
        if inserted {
            self.save()?;
        }
        Ok(inserted)
    }

    pub fn unsubscribe(&mut self, chat_id: &str, is_group: bool) -> Result<bool> {
        let mut removed = self.data.subscribers.remove(chat_id);
        for set in self.data.game_subscribers.values_mut() {
            if set.remove(chat_id) {
                removed = true;
            }
        }
        if is_group {
            self.data.unsubscribed_groups.insert(chat_id.to_string());
        }
        if removed || is_group {
            self.save()?;
        }
        Ok(removed)
    }

    /// Subscribes `chat_id` to notifications for a specific `game_id`.
    pub fn subscribe_game(&mut self, game_id: &str, chat_id: &str) -> Result<bool> {
        let game = game_id.to_lowercase();
        if self.data.subscribers.contains(chat_id)
            || self
                .data
                .game_subscribers
                .get(&game)
                .is_some_and(|set| set.contains(chat_id))
        {
            return Ok(false);
        }
        self.data.unsubscribed_groups.remove(chat_id);
        self.data
            .game_subscribers
            .entry(game)
            .or_default()
            .insert(chat_id.to_string());
        self.save()?;
        Ok(true)
    }

    /// Unsubscribes `chat_id` from a specific `game_id`, splitting a global subscription if needed.
    pub fn unsubscribe_game(
        &mut self,
        game_id: &str,
        chat_id: &str,
        is_group: bool,
        all_game_ids: &[&str],
    ) -> Result<bool> {
        let game = game_id.to_lowercase();
        if self.data.subscribers.remove(chat_id) {
            for &other in all_game_ids {
                let other_lower = other.to_lowercase();
                if other_lower != game {
                    self.data
                        .game_subscribers
                        .entry(other_lower)
                        .or_default()
                        .insert(chat_id.to_string());
                }
            }
            if is_group {
                self.data.unsubscribed_groups.insert(chat_id.to_string());
            }
            self.save()?;
            return Ok(true);
        }

        let removed = self
            .data
            .game_subscribers
            .get_mut(&game)
            .is_some_and(|set| set.remove(chat_id));
        if is_group {
            self.data.unsubscribed_groups.insert(chat_id.to_string());
        }
        if removed || is_group {
            self.save()?;
        }
        Ok(removed)
    }

    #[allow(dead_code)]
    pub fn subscribers(&self) -> Vec<String> {
        self.data.subscribers.iter().cloned().collect()
    }

    /// Returns sorted deduplicated subscribers receiving notifications for `game_id`.
    pub fn subscribers_for_game(&self, game_id: &str) -> Vec<String> {
        let game = game_id.to_lowercase();
        let mut combined = self.data.subscribers.clone();
        if let Some(per_game) = self.data.game_subscribers.get(&game) {
            combined.extend(per_game.iter().cloned());
        }
        combined.into_iter().collect()
    }

    pub fn is_code_sent(&self, code: &str) -> bool {
        self.data.sent_codes.contains(code)
    }

    pub fn mark_codes_sent(&mut self, codes: &[String]) -> Result<()> {
        let mut changed = false;
        for code in codes {
            if self.data.sent_codes.insert(code.clone()) {
                changed = true;
            }
        }
        if changed {
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create state directory {}", parent.display()))?;
        }
        let tmp_path = self.path.with_extension("json.tmp");
        let serialized = serde_json::to_string_pretty(&self.data)?;
        fs::write(&tmp_path, serialized)
            .with_context(|| format!("Failed to write temp state {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &self.path)
            .with_context(|| format!("Failed to replace state file {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn persists_subscribers_and_sent_codes() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("gaming_news_bot_test_{nanos}"));
        let state_file = temp_dir.join("state.json");

        {
            let mut state = BotState::load_or_create(&state_file).unwrap();
            assert!(state.subscribe("chat_1").unwrap());
            assert!(!state.subscribe("chat_1").unwrap());
            state.mark_codes_sent(&["CODE1".to_string()]).unwrap();
        }

        let reloaded = BotState::load_or_create(&state_file).unwrap();
        assert_eq!(reloaded.subscribers(), vec!["chat_1".to_string()]);
        assert!(reloaded.is_code_sent("CODE1"));
        assert!(!reloaded.is_code_sent("CODE2"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn auto_subscribes_group_unless_unsubscribed() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("gaming_news_bot_group_{nanos}"));
        let state_file = temp_dir.join("state.json");

        let mut state = BotState::load_or_create(&state_file).unwrap();
        assert!(state.auto_subscribe_group("zgr-123").unwrap());
        assert!(!state.auto_subscribe_group("zgr-123").unwrap());
        assert!(state.unsubscribe("zgr-123", true).unwrap());
        assert!(!state.auto_subscribe_group("zgr-123").unwrap());
        assert!(state.subscribe("zgr-123").unwrap());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn formats_and_parses_platform_subscriber_ids() {
        assert_eq!(format_subscriber_id("zalo", "chat_1"), "chat_1");
        assert_eq!(format_subscriber_id("zalo", "zgr-123"), "zgr-123");
        assert_eq!(
            format_subscriber_id("discord", "1234567890"),
            "discord:1234567890"
        );

        assert_eq!(parse_subscriber_id("chat_1"), ("zalo", "chat_1"));
        assert_eq!(parse_subscriber_id("zgr-123"), ("zalo", "zgr-123"));
        assert_eq!(parse_subscriber_id("zalo:chat_1"), ("zalo", "chat_1"));
        assert_eq!(
            parse_subscriber_id("discord:1234567890"),
            ("discord", "1234567890")
        );
    }

    #[test]
    fn manages_per_game_and_global_subscriptions() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("gaming_news_bot_games_{nanos}"));
        let state_file = temp_dir.join("state.json");

        let mut state = BotState::load_or_create(&state_file).unwrap();
        let games = ["gi", "hsr"];

        // Per-game subscribe and deduplication
        assert!(state.subscribe_game("gi", "discord:111").unwrap());
        assert!(!state.subscribe_game("gi", "discord:111").unwrap());
        assert_eq!(state.subscribers_for_game("gi"), vec!["discord:111"]);
        assert!(state.subscribers_for_game("hsr").is_empty());

        // Global subscriber receives both and cannot re-subscribe per-game redundantly
        assert!(state.subscribe("chat_global").unwrap());
        assert!(!state.subscribe_game("gi", "chat_global").unwrap());
        assert_eq!(
            state.subscribers_for_game("gi"),
            vec!["chat_global", "discord:111"]
        );

        // Unsubscribing global subscriber from one game keeps them on remaining games
        assert!(state.unsubscribe_game("gi", "chat_global", false, &games).unwrap());
        assert_eq!(state.subscribers_for_game("gi"), vec!["discord:111"]);
        assert_eq!(state.subscribers_for_game("hsr"), vec!["chat_global"]);

        // Global unsubscribe clears both global and per-game subscriptions
        assert!(state.subscribe_game("hsr", "discord:111").unwrap());
        assert!(state.unsubscribe("discord:111", false).unwrap());
        assert!(state.subscribers_for_game("gi").is_empty());
        assert_eq!(state.subscribers_for_game("hsr"), vec!["chat_global"]);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
