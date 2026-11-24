#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod channels;
mod config;
mod sources;
mod state;

use anyhow::{anyhow, Context, Result};
use channels::discord::DiscordChannel;
use channels::zalo::ZaloChannel;
use channels::{Channel, ChatChannel};
use config::AppConfig;
use sources::genshin::GenshinFandomSource;
use sources::{CodeItem, GameSource, NewsSource, NotificationKind};
use state::{format_subscriber_id, parse_subscriber_id, BotState};
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
    let state = Arc::new(Mutex::new(BotState::load_or_create(&config.bot.state_path)?));

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
    let game_ids: Vec<&str> = sources.iter().map(NewsSource::game_id).collect();

    let mut channels: Vec<Arc<Channel>> = Vec::new();

    if let Some(zalo_token) = read_env_token("ZALO_BOT_TOKEN") {
        let zalo = ZaloChannel::new(
            &config.bot.zalo_api_base,
            zalo_token,
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
        channels.push(Arc::new(Channel::Zalo(zalo)));
    }

    if let Some(discord_token) = read_env_token("DISCORD_BOT_TOKEN") {
        let discord = DiscordChannel::new(
            &config.discord.api_base,
            discord_token,
            config.discord.max_message_chars,
            config.discord.reconnect_delay_secs,
            config.discord.embed_color,
            config.discord.max_codes_per_embed,
        )?;
        let me = discord
            .get_me()
            .context("Failed to connect to Discord API (verify DISCORD_BOT_TOKEN)")?;
        info!(
            bot_id = %me.id,
            username = %me.username,
            display_name = ?me.global_name,
            "Connected to Discord API successfully"
        );
        if let Err(err) = discord.register_commands(&me.id, &game_ids) {
            warn!("Failed to register Discord slash commands: {err:#}");
        } else {
            info!("Registered Discord global slash commands");
        }
        discord.start_gateway(me.id);
        channels.push(Arc::new(Channel::Discord(discord)));
    }

    if channels.is_empty() {
        return Err(anyhow!(
            "No messaging platform configured. Set ZALO_BOT_TOKEN and/or DISCORD_BOT_TOKEN in .env"
        ));
    }

    let channels = Arc::new(channels);

    info!(
        source_count = sources.len(),
        channel_count = channels.len(),
        "Starting Gaming News Bot"
    );

    let mut handles = Vec::with_capacity(sources.len() + channels.len());
    for idx in 0..sources.len() {
        let config = config.clone();
        let state = Arc::clone(&state);
        let channels = Arc::clone(&channels);
        let sources = Arc::clone(&sources);
        handles.push(thread::spawn(move || {
            run_source_poller(config, state, channels, &sources[idx]);
        }));
    }

    for channel in channels.iter() {
        let config = config.clone();
        let state = Arc::clone(&state);
        let channel = Arc::clone(channel);
        let sources = Arc::clone(&sources);
        handles.push(thread::spawn(move || {
            run_channel_loop(config, state, channel, sources);
        }));
    }

    for handle in handles {
        let _ = handle.join();
    }
    Ok(())
}

fn read_env_token(var_name: &str) -> Option<String> {
    env::var(var_name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
    channels: Arc<Vec<Arc<Channel>>>,
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
        if let Err(err) = check_and_notify_source(&config, &state, &channels, source) {
            error!(source = %source.name(), "Failed during source check: {err:#}");
        }
        thread::sleep(interval);
    }
}

fn collect_active_items(sources: &[GameSource], game_filter: Option<&str>) -> Result<Vec<CodeItem>> {
    let mut all_items = Vec::new();
    let mut last_err = None;

    for source in sources {
        if source.kind() != NotificationKind::Giftcodes {
            continue;
        }
        if let Some(game) = game_filter {
            if !source.game_id().eq_ignore_ascii_case(game) {
                continue;
            }
        }
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
    channels: &[Arc<Channel>],
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
        let subscribers = guard.subscribers_for_game(source.game_id());
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
    for subscriber_id in &subscribers {
        let (platform, chat_id) = parse_subscriber_id(subscriber_id);
        let Some(channel) = channels.iter().find(|ch| ch.name() == platform) else {
            warn!(
                subscriber_id = %subscriber_id,
                platform,
                "Skipping subscriber because channel is not enabled"
            );
            continue;
        };
        match channel.send_codes(
            chat_id,
            &config.messages.new_codes_header,
            &new_items,
            &message,
        ) {
            Ok(()) => {
                any_sent = true;
                info!(platform, chat_id = %chat_id, count = new_items.len(), "Sent new items");
            }
            Err(err) => {
                warn!(
                    platform,
                    chat_id = %chat_id,
                    "Failed to send message to subscriber: {err:#}"
                );
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
                    let sub_id = format_subscriber_id(channel.name(), &msg.chat_id);
                    if msg.auto_subscribe {
                        if let Ok(mut guard) = state.lock() {
                            if let Ok(true) = guard.auto_subscribe_group(&sub_id) {
                                info!(
                                    platform = channel.name(),
                                    chat_id = %msg.chat_id,
                                    "Auto-subscribed group chat"
                                );
                            }
                        }
                    }

                    let Some(text) = msg.text.as_deref() else {
                        continue;
                    };
                    if let Err(err) = handle_command(
                        &msg.chat_id,
                        &sub_id,
                        msg.is_group,
                        msg.reply_context.as_deref(),
                        text,
                        &config,
                        &state,
                        channel.as_ref(),
                        &sources,
                    ) {
                        warn!(
                            platform = channel.name(),
                            chat_id = %msg.chat_id,
                            "Error handling command: {err:#}"
                        );
                    }
                }
            }
            Err(err) => {
                warn!(platform = channel.name(), "Channel poll failed: {err:#}");
                thread::sleep(Duration::from_secs(5));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    chat_id: &str,
    sub_id: &str,
    is_group: bool,
    reply_context: Option<&str>,
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
        "/start" | "/help" => {
            if channel.name() == "discord" && !config.discord.welcome.trim().is_empty() {
                config.discord.welcome.clone()
            } else {
                config.messages.welcome.clone()
            }
        }
        "/subscribe" => {
            let inserted = state
                .lock()
                .map_err(|_| anyhow::anyhow!("State mutex poisoned"))?
                .subscribe(sub_id)?;
            bool_reply(
                inserted,
                &config.messages.subscribed,
                &config.messages.already_subscribed,
            )
        }
        "/unsubscribe" => {
            let removed = state
                .lock()
                .map_err(|_| anyhow::anyhow!("State mutex poisoned"))?
                .unsubscribe(sub_id, is_group)?;
            bool_reply(
                removed,
                &config.messages.unsubscribed,
                &config.messages.not_subscribed,
            )
        }
        "/codes" => {
            return reply_codes_command(chat_id, reply_context, sources, None, config, channel);
        }
        _ => {
            let Some((game_id, action)) = cmd.strip_prefix('/').and_then(|s| s.split_once(':'))
            else {
                return Ok(());
            };
            let has_game = sources
                .iter()
                .any(|source| source.game_id().eq_ignore_ascii_case(game_id));
            if !has_game {
                config.messages.unknown_game.clone()
            } else {
                match action {
                    "codes" => {
                        let has_codes = sources.iter().any(|source| {
                            source.kind() == NotificationKind::Giftcodes
                                && source.game_id().eq_ignore_ascii_case(game_id)
                        });
                        if !has_codes {
                            config.messages.no_active_codes.clone()
                        } else {
                            return reply_codes_command(
                                chat_id,
                                reply_context,
                                sources,
                                Some(game_id),
                                config,
                                channel,
                            );
                        }
                    }
                    "subscribe" => {
                        let inserted = state
                            .lock()
                            .map_err(|_| anyhow::anyhow!("State mutex poisoned"))?
                            .subscribe_game(game_id, sub_id)?;
                        bool_reply(
                            inserted,
                            &config.messages.subscribed,
                            &config.messages.already_subscribed,
                        )
                    }
                    "unsubscribe" => {
                        let all_game_ids: Vec<&str> =
                            sources.iter().map(NewsSource::game_id).collect();
                        let removed = state
                            .lock()
                            .map_err(|_| anyhow::anyhow!("State mutex poisoned"))?
                            .unsubscribe_game(game_id, sub_id, is_group, &all_game_ids)?;
                        bool_reply(
                            removed,
                            &config.messages.unsubscribed,
                            &config.messages.not_subscribed,
                        )
                    }
                    _ => return Ok(()),
                }
            }
        }
    };

    channel.reply_message(chat_id, reply_context, &reply)
}

fn bool_reply(flag: bool, on_true: &str, on_false: &str) -> String {
    if flag { on_true } else { on_false }.to_string()
}

fn reply_codes_command(
    chat_id: &str,
    reply_context: Option<&str>,
    sources: &[GameSource],
    game_filter: Option<&str>,
    config: &AppConfig,
    channel: &Channel,
) -> Result<()> {
    match collect_active_items(sources, game_filter) {
        Ok(items) if items.is_empty() => {
            channel.reply_message(chat_id, reply_context, &config.messages.no_active_codes)
        }
        Ok(items) => {
            let fallback = format_codes_message(&config.messages.active_codes_header, &items);
            channel.reply_codes(
                chat_id,
                reply_context,
                &config.messages.active_codes_header,
                &items,
                &fallback,
            )
        }
        Err(err) => {
            error!("Failed to fetch items for codes command: {err:#}");
            channel.reply_message(chat_id, reply_context, &config.messages.fetch_error)
        }
    }
}

fn normalize_action(action: &str) -> &str {
    match action {
        "sub" | "subscribe" => "subscribe",
        "unsub" | "unsubscribe" => "unsubscribe",
        other => other,
    }
}

/// Extracts slash commands (including `/sub`, `/unsub`, `/<game>:codes|sub|unsub`, or `/<game> <action>`), even after `@mention`.
fn extract_command(raw_text: &str) -> Option<String> {
    let mut tokens = raw_text.split_whitespace();
    let first = tokens.find(|token| token.starts_with('/'))?;
    let cmd = first.split('@').next().unwrap_or(first).to_lowercase();

    if let Some((game, action)) = cmd.strip_prefix('/').and_then(|s| s.split_once(':')) {
        return Some(format!("/{game}:{}", normalize_action(action)));
    }

    match cmd.as_str() {
        "/sub" => return Some("/subscribe".to_string()),
        "/unsub" => return Some("/unsubscribe".to_string()),
        "/start" | "/help" | "/subscribe" | "/unsubscribe" | "/codes" => return Some(cmd),
        _ => {}
    }

    if let Some(sub) = tokens.next() {
        let sub_lower = sub.to_lowercase();
        let normalized = normalize_action(&sub_lower);
        if matches!(normalized, "codes" | "subscribe" | "unsubscribe") {
            return Some(format!("{cmd}:{normalized}"));
        }
    }
    Some(cmd)
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
        assert_eq!(extract_command("/sub").as_deref(), Some("/subscribe"));
        assert_eq!(extract_command("/unsub").as_deref(), Some("/unsubscribe"));
        assert_eq!(
            extract_command("/gi:codes").as_deref(),
            Some("/gi:codes")
        );
        assert_eq!(
            extract_command("/gi codes").as_deref(),
            Some("/gi:codes")
        );
        assert_eq!(
            extract_command("/gi:subscribe").as_deref(),
            Some("/gi:subscribe")
        );
        assert_eq!(
            extract_command("/gi:sub").as_deref(),
            Some("/gi:subscribe")
        );
        assert_eq!(
            extract_command("/gi sub").as_deref(),
            Some("/gi:subscribe")
        );
        assert_eq!(
            extract_command("/gi:unsubscribe").as_deref(),
            Some("/gi:unsubscribe")
        );
        assert_eq!(
            extract_command("/gi:unsub").as_deref(),
            Some("/gi:unsubscribe")
        );
        assert_eq!(
            extract_command("/gi unsub").as_deref(),
            Some("/gi:unsubscribe")
        );
        assert_eq!(
            extract_command("/gi:codes@MyBot").as_deref(),
            Some("/gi:codes")
        );
        assert_eq!(
            extract_command("@Bot /gi:codes").as_deref(),
            Some("/gi:codes")
        );
        assert_eq!(extract_command("hello world"), None);
    }
}
