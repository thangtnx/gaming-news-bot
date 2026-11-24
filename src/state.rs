use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedData {
    subscribers: BTreeSet<String>,
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
        let removed = self.data.subscribers.remove(chat_id);
        if is_group {
            self.data.unsubscribed_groups.insert(chat_id.to_string());
        }
        if removed || is_group {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn subscribers(&self) -> Vec<String> {
        self.data.subscribers.iter().cloned().collect()
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
}
