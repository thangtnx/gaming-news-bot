#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod channels;
mod config;
mod sources;
mod state;

use anyhow::{Context, Result};
use channels::zalo::ZaloChannel;
use channels::{Channel, ChatChannel};
use config::AppConfig;
use sources::genshin::GenshinFandomSource;
use sources::{CodeItem, GameSource, NewsSource};
use state::BotState;
use std::env;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use ureq::AgentBuilder;

fn main() -> Result<()> {
    ensure_project_workdir();
    let _ = dotenvy::dotenv();

    init_logging()?;

    let config = AppConfig::load("config.toml")?;
    let token = env::var("ZALO_BOT_TOKEN")
        .context("Missing ZALO_BOT_TOKEN environment variable (set it in .env)")?;

    let state = Arc::new(Mutex::new(BotState::load_or_create(&config.bot.state_path)?));

    let zalo = ZaloChannel::new(
        &config.bot.zalo_api_base,
        token,
        config.bot.zalo_poll_timeout_secs,
    )?;
    let me = zalo
        .get_me()
        .context("Failed to connect to Zalo Bot API (verify ZALO_BOT_TOKEN)")?;
    info!(
        bot_id = %me.id,
        bot_name = %me.first_name,
        username = %me.username,
        "Connected to Zalo Bot API successfully"
    );
    let channel = Arc::new(Channel::Zalo(zalo));

    let tls = Arc::new(
        native_tls::TlsConnector::new().context("Failed to initialize Windows TLS connector")?,
    );
    let http = AgentBuilder::new()
        .tls_connector(tls)
        .timeout_connect(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build();

    let sources: Arc<Vec<GameSource>> = Arc::new(vec![GameSource::Genshin(
        GenshinFandomSource::new(
            http,
            &config.genshin.fandom_api_url,
            config.genshin.allowed_servers.clone(),
            &config.messages.redeem_url,
        ),
    )]);

    info!(
        source_count = sources.len(),
        "Starting Gaming News Bot"
    );

    let mut handles = Vec::with_capacity(sources.len() + 1);
    for idx in 0..sources.len() {
        let config = config.clone();
        let state = Arc::clone(&state);
        let channel = Arc::clone(&channel);
        let sources = Arc::clone(&sources);
        handles.push(thread::spawn(move || {
            run_source_poller(config, state, channel, &sources[idx]);
        }));
    }

    let updates_handle = {
        let config = config.clone();
        let state = Arc::clone(&state);
        let channel = Arc::clone(&channel);
        let sources = Arc::clone(&sources);
        thread::spawn(move || {
            run_channel_loop(config, state, channel, sources);
        })
    };
    handles.push(updates_handle);

    for handle in handles {
        let _ = handle.join();
    }
    Ok(())
}

fn init_logging() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if cfg!(debug_assertions) {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        fs::create_dir_all("data").context("Failed to create data directory for logs")?;
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open("data/bot.log")
            .context("Failed to open data/bot.log")?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Mutex::new(log_file))
            .init();
    }
    Ok(())
}

/// Ensures relative paths (`config.toml`, `.env`, `data/state.json`) resolve even when launched from System32.
fn ensure_project_workdir() {
    if PathBuf::from("config.toml").exists() {
        return;
    }
    if let Ok(exe) = env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            if ancestor.join("config.toml").exists() {
                let _ = env::set_current_dir(ancestor);
                return;
            }
        }
    }
}

fn run_source_poller(
    config: AppConfig,
    state: Arc<Mutex<BotState>>,
    channel: Arc<Channel>,
    source: &GameSource,
) {
    let kind = source.kind();
    let interval = config.intervals.for_kind(kind);
    info!(
        source = %source.name(),
        kind = kind.as_str(),
        poll_interval_secs = interval.as_secs(),
        "Started source poller"
    );
    loop {
        if let Err(err) = check_and_notify_source(&config, &state, &*channel, source) {
            error!(source = %source.name(), "Failed during source check: {err:#}");
        }
        thread::sleep(interval);
    }
}

fn collect_all_active_items(sources: &[GameSource]) -> Result<Vec<CodeItem>> {
    let mut all_items = Vec::new();
    let mut last_err = None;

    for source in sources {
        match source.fetch_active_items() {
            Ok(mut items) => all_items.append(&mut items),
            Err(err) => {
                warn!(source = %source.name(), "Source fetch failed: {err:#}");
                last_err = Some(err);
            }
        }
    }

    if all_items.is_empty() {
        if let Some(err) = last_err {
            return Err(err);
        }
    }

    Ok(all_items)
}

fn check_and_notify_source(
    config: &AppConfig,
    state: &Arc<Mutex<BotState>>,
    channel: &Channel,
    source: &GameSource,
) -> Result<()> {
    let items = source.fetch_active_items()?;

    let (new_items, subscribers) = {
        let guard = state
            .lock()
            .map_err(|_| anyhow::anyhow!("State mutex poisoned"))?;
        let new_items: Vec<CodeItem> = items
            .into_iter()
            .filter(|item| !guard.is_code_sent(&item.code) && !guard.is_code_sent(&item.dedup_key()))
            .collect();
        let subscribers = guard.subscribers();
        (new_items, subscribers)
    };

    if new_items.is_empty() {
        info!("Hourly check complete: no new items.");
        return Ok(());
    }

    if subscribers.is_empty() {
        info!(
            count = new_items.len(),
            "Found new items, but there are no subscribers yet."
        );
        return Ok(());
    }

    let message = format_codes_message(&config.messages.new_codes_header, &new_items);

    let mut any_sent = false;
    for chat_id in &subscribers {
        match channel.send_message(chat_id, &message) {
            Ok(()) => {
                any_sent = true;
                info!(chat_id = %chat_id, count = new_items.len(), "Sent new items");
            }
            Err(err) => {
                warn!(chat_id = %chat_id, "Failed to send message to subscriber: {err:#}");
            }
        }
    }

    if any_sent {
        let keys: Vec<String> = new_items.into_iter().map(|i| i.dedup_key()).collect();
        let mut guard = state
            .lock()
            .map_err(|_| anyhow::anyhow!("State mutex poisoned"))?;
        guard.mark_codes_sent(&keys)?;
    }

    Ok(())
}

fn run_channel_loop(
    config: AppConfig,
    state: Arc<Mutex<BotState>>,
    channel: Arc<Channel>,
    sources: Arc<Vec<GameSource>>,
) {
    loop {
        match channel.poll_messages() {
            Ok(messages) => {
                for msg in messages {
                    if msg.is_group {
                        if let Ok(mut guard) = state.lock() {
                            if let Ok(true) = guard.auto_subscribe_group(&msg.chat_id) {
                                info!(chat_id = %msg.chat_id, "Auto-subscribed group chat");
                            }
                        }
                    }

                    let Some(text) = msg.text.as_deref() else {
                        continue;
                    };
                    if let Err(err) = handle_command(
                        &msg.chat_id,
                        msg.is_group,
                        text,
                        &config,
                        &state,
                        &*channel,
                        &sources,
                    ) {
                        warn!(chat_id = %msg.chat_id, "Error handling command: {err:#}");
                    }
                }
            }
            Err(err) => {
                warn!("Channel poll failed: {err:#}");
                thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

fn handle_command(
    chat_id: &str,
    is_group: bool,
    raw_text: &str,
    config: &AppConfig,
    state: &Arc<Mutex<BotState>>,
    channel: &Channel,
    sources: &[GameSource],
) -> Result<()> {
    let Some(cmd) = extract_command(raw_text) else {
        return Ok(());
    };

    let reply = match cmd.as_str() {
        "/start" | "/help" => config.messages.welcome.clone(),
        "/subscribe" => {
            let inserted = state
                .lock()
                .map_err(|_| anyhow::anyhow!("State mutex poisoned"))?
                .subscribe(chat_id)?;
            if inserted {
                config.messages.subscribed.clone()
            } else {
                config.messages.already_subscribed.clone()
            }
        }
        "/unsubscribe" => {
            let removed = state
                .lock()
                .map_err(|_| anyhow::anyhow!("State mutex poisoned"))?
                .unsubscribe(chat_id, is_group)?;
            if removed {
                config.messages.unsubscribed.clone()
            } else {
                config.messages.not_subscribed.clone()
            }
        }
        "/codes" => match collect_all_active_items(sources) {
            Ok(items) if items.is_empty() => config.messages.no_active_codes.clone(),
            Ok(items) => format_codes_message(&config.messages.active_codes_header, &items),
            Err(err) => {
                error!("Failed to fetch items for /codes command: {err:#}");
                config.messages.fetch_error.clone()
            }
        },
        _ => return Ok(()),
    };

    channel.send_message(chat_id, &reply)
}

/// Extracts the slash command even when preceded by `@mention` in a group chat.
fn extract_command(raw_text: &str) -> Option<String> {
    raw_text
        .split_whitespace()
        .find(|token| token.starts_with('/'))
        .map(|token| token.split('@').next().unwrap_or(token).to_lowercase())
}

fn format_codes_message(header: &str, items: &[CodeItem]) -> String {
    let mut blocks = Vec::with_capacity(items.len() + 2);
    blocks.push(header.to_string());

    for item in items {
        let reward = if item.reward.is_empty() {
            "Không rõ".to_string()
        } else {
            item.reward.clone()
        };

        blocks.push(format!(
            "━━━━━━━━━━━━━━━━━━━━\n\
             🎮 {} | 🔑 {} ({})\n\
             💎 Quà: {}\n\
             ⏳ Hạn: {}\n\
             🔗 Đổi ngay: {}",
            item.game, item.code, item.server, reward, item.expiry, item.redeem_url
        ));
    }

    blocks.push("━━━━━━━━━━━━━━━━━━━━".to_string());
    blocks.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_code_cards_with_direct_redeem_link() {
        let items = vec![CodeItem {
            game: "Genshin Impact".to_string(),
            code: "POYMANSXF877".to_string(),
            server: "G".to_string(),
            reward: "Primogem × 20, Mora × 10,000".to_string(),
            expiry: "unknown".to_string(),
            redeem_url: "https://genshin.hoyoverse.com/en/gift?code=POYMANSXF877".to_string(),
        }];

        let msg = format_codes_message("🎁 Code mới:", &items);

        assert!(msg.contains("🎮 Genshin Impact | 🔑 POYMANSXF877 (G)"));
        assert!(
            msg.contains("🔗 Đổi ngay: https://genshin.hoyoverse.com/en/gift?code=POYMANSXF877")
        );
    }

    #[test]
    fn extracts_slash_commands_with_or_without_group_mention() {
        assert_eq!(extract_command("/codes").as_deref(), Some("/codes"));
        assert_eq!(extract_command("/codes@MyBot").as_deref(), Some("/codes"));
        assert_eq!(
            extract_command("@GenshinBot /subscribe").as_deref(),
            Some("/subscribe")
        );
        assert_eq!(
            extract_command("@Genshin Bot /codes").as_deref(),
            Some("/codes")
        );
        assert_eq!(extract_command("hello world"), None);
    }
}
