use super::{ChatChannel, IncomingMessage};
use crate::sources::CodeItem;
use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Error as WsError, Message, WebSocket};
use ureq::{Agent, AgentBuilder};

const USER_AGENT: &str = "DiscordBot (https://github.com/gaming-news-bot, 0.1.0)";
const DEFAULT_GATEWAY_URL: &str = "wss://gateway.discord.gg";
const INTENTS_WITH_MESSAGE_CONTENT: u64 = 37377;
const INTENTS_WITHOUT_MESSAGE_CONTENT: u64 = 4609;
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 41_250;
const MESSAGE_FLAG_SUPPRESS_EMBEDS: u32 = 1 << 2;

pub struct DiscordChannel {
    agent: Agent,
    api_base: String,
    token: String,
    max_message_chars: usize,
    reconnect_delay: Duration,
    embed_color: u32,
    max_codes_per_embed: usize,
    tx: Mutex<mpsc::Sender<IncomingMessage>>,
    rx: Mutex<mpsc::Receiver<IncomingMessage>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscordBotInfo {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GatewayBotResponse {
    url: String,
}

#[derive(Debug, Serialize)]
struct SlashCommandSpec<'a> {
    name: &'a str,
    description: &'a str,
    #[serde(rename = "type")]
    kind: u8,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    options: Vec<SlashCommandOptionSpec<'a>>,
}

#[derive(Debug, Serialize)]
struct SlashCommandOptionSpec<'a> {
    name: &'a str,
    description: &'a str,
    #[serde(rename = "type")]
    kind: u8,
}

#[derive(Debug, Serialize)]
struct MessageContentBody<'a> {
    content: &'a str,
    flags: u32,
}

impl<'a> MessageContentBody<'a> {
    fn new(content: &'a str) -> Self {
        Self {
            content,
            flags: MESSAGE_FLAG_SUPPRESS_EMBEDS,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EmbedMessageBody {
    content: &'static str,
    embeds: Vec<DiscordEmbed>,
}

#[derive(Debug, Serialize)]
struct DiscordEmbed {
    description: String,
    color: u32,
}

#[derive(Debug, Serialize)]
struct InteractionCallbackBody {
    #[serde(rename = "type")]
    kind: u8,
}

#[derive(Debug, Serialize)]
struct IdentifyPayload<'a> {
    op: u8,
    d: IdentifyData<'a>,
}

#[derive(Debug, Serialize)]
struct IdentifyData<'a> {
    token: &'a str,
    intents: u64,
    properties: IdentifyProperties<'a>,
}

#[derive(Debug, Serialize)]
struct IdentifyProperties<'a> {
    os: &'a str,
    browser: &'a str,
    device: &'a str,
}

#[derive(Debug, Serialize)]
struct HeartbeatPayload {
    op: u8,
    d: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GatewayEvent {
    op: u8,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
    #[serde(default)]
    d: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct HelloData {
    heartbeat_interval: u64,
}

#[derive(Debug, Deserialize)]
struct DiscordMessageData {
    channel_id: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    content: String,
    #[serde(default)]
    author: Option<DiscordAuthor>,
}

#[derive(Debug, Deserialize)]
struct DiscordAuthor {
    #[serde(default)]
    bot: bool,
}

#[derive(Debug, Deserialize)]
struct DiscordInteractionData {
    id: String,
    token: String,
    #[serde(rename = "type")]
    kind: u8,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    data: Option<ApplicationCommandData>,
}

#[derive(Debug, Deserialize)]
struct ApplicationCommandData {
    name: String,
    #[serde(default)]
    options: Vec<ApplicationCommandOption>,
}

#[derive(Debug, Deserialize)]
struct ApplicationCommandOption {
    name: String,
    #[serde(rename = "type")]
    kind: u8,
}

impl DiscordChannel {
    /// Creates a new Discord REST + Gateway client without connecting to the Gateway yet.
    pub fn new(
        api_base: impl Into<String>,
        token: impl Into<String>,
        max_message_chars: usize,
        reconnect_delay_secs: u64,
        embed_color: u32,
        max_codes_per_embed: usize,
    ) -> Result<Self> {
        let tls = Arc::new(
            native_tls::TlsConnector::new().context("Failed to initialize Windows TLS connector")?,
        );
        let agent = AgentBuilder::new()
            .tls_connector(tls)
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .build();
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            agent,
            api_base: api_base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            max_message_chars: max_message_chars.max(1),
            reconnect_delay: Duration::from_secs(reconnect_delay_secs.max(1)),
            embed_color,
            max_codes_per_embed: max_codes_per_embed.clamp(1, 25),
            tx: Mutex::new(tx),
            rx: Mutex::new(rx),
        })
    }

    fn send_webhook_batches<T: Serialize>(
        &self,
        app_id: &str,
        interaction_token: &str,
        batches: &[T],
    ) -> Result<()> {
        let Some((first, rest)) = batches.split_first() else {
            return Ok(());
        };

        self.rest_send_json(
            "editInteractionReply",
            "PATCH",
            &format!("webhooks/{app_id}/{interaction_token}/messages/@original"),
            first,
        )?;

        for batch in rest {
            self.rest_send_json(
                "followupInteractionReply",
                "POST",
                &format!("webhooks/{app_id}/{interaction_token}"),
                batch,
            )?;
        }
        Ok(())
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.api_base, path.trim_start_matches('/'))
    }

    fn redact_token(&self, msg: String) -> String {
        redact_secret(&self.token, msg)
    }

    fn map_ureq_err(&self, label: &str, err: ureq::Error) -> anyhow::Error {
        match err {
            ureq::Error::Status(status, resp) => {
                let body = resp.into_string().unwrap_or_default();
                anyhow!(
                    "Discord {label} failed (HTTP {status}): {}",
                    self.redact_token(body)
                )
            }
            other => anyhow!(
                "Failed to call Discord {label}: {}",
                self.redact_token(other.to_string())
            ),
        }
    }

    fn rest_get_json<T: DeserializeOwned>(&self, label: &str, path: &str) -> Result<T> {
        let resp = self
            .agent
            .get(&self.endpoint(path))
            .set("Authorization", &format!("Bot {}", self.token))
            .set("User-Agent", USER_AGENT)
            .call()
            .map_err(|err| self.map_ureq_err(label, err))?;

        resp.into_json()
            .with_context(|| format!("Failed to decode Discord {label} JSON"))
    }

    fn rest_send_json<B: Serialize>(
        &self,
        label: &str,
        method: &str,
        path: &str,
        body: &B,
    ) -> Result<()> {
        self.agent
            .request(method, &self.endpoint(path))
            .set("Authorization", &format!("Bot {}", self.token))
            .set("User-Agent", USER_AGENT)
            .send_json(body)
            .map_err(|err| self.map_ureq_err(label, err))?;
        Ok(())
    }

    /// Verifies the Discord bot token and fetches the bot user's profile via `GET /users/@me`.
    pub fn get_me(&self) -> Result<DiscordBotInfo> {
        self.rest_get_json("getMe", "users/@me")
    }

    /// Registers global Slash Commands for the Discord bot application.
    pub fn register_commands(&self, application_id: &str, game_ids: &[&str]) -> Result<()> {
        let mut commands = vec![
            SlashCommandSpec {
                name: "start",
                description: "Xem hướng dẫn sử dụng bot",
                kind: 1,
                options: Vec::new(),
            },
            SlashCommandSpec {
                name: "help",
                description: "Xem hướng dẫn và danh sách lệnh",
                kind: 1,
                options: Vec::new(),
            },
            SlashCommandSpec {
                name: "subscribe",
                description: "Đăng ký nhận thông báo mới",
                kind: 1,
                options: Vec::new(),
            },
            SlashCommandSpec {
                name: "unsubscribe",
                description: "Hủy đăng ký nhận thông báo",
                kind: 1,
                options: Vec::new(),
            },
            SlashCommandSpec {
                name: "codes",
                description: "Xem danh sách giftcode còn hiệu lực (tất cả game)",
                kind: 1,
                options: Vec::new(),
            },
        ];

        let mut seen = BTreeSet::new();
        for &game_id in game_ids {
            if seen.insert(game_id) {
                commands.push(SlashCommandSpec {
                    name: game_id,
                    description: "Lệnh cho game này",
                    kind: 1,
                    options: vec![
                        SlashCommandOptionSpec {
                            name: "codes",
                            description: "Xem danh sách giftcode còn hiệu lực",
                            kind: 1,
                        },
                        SlashCommandOptionSpec {
                            name: "subscribe",
                            description: "Đăng ký nhận thông báo cho game này",
                            kind: 1,
                        },
                        SlashCommandOptionSpec {
                            name: "unsubscribe",
                            description: "Hủy đăng ký nhận thông báo cho game này",
                            kind: 1,
                        },
                    ],
                });
            }
        }

        self.rest_send_json(
            "registerCommands",
            "PUT",
            &format!("applications/{application_id}/commands"),
            &commands,
        )
    }

    /// Starts the background Discord Gateway v10 WebSocket thread.
    pub fn start_gateway(&self, application_id: String) {
        let Ok(tx_guard) = self.tx.lock() else {
            error!("Discord channel sender mutex poisoned");
            return;
        };
        let tx = tx_guard.clone();
        drop(tx_guard);

        let agent = self.agent.clone();
        let api_base = self.api_base.clone();
        let token = self.token.clone();
        let reconnect_delay = self.reconnect_delay;

        thread::spawn(move || {
            run_gateway_loop(GatewayContext {
                agent,
                api_base,
                token,
                application_id,
                reconnect_delay,
                tx,
            });
        });
    }
}

impl ChatChannel for DiscordChannel {
    fn name(&self) -> &'static str {
        "discord"
    }

    fn poll_messages(&self) -> Result<Vec<IncomingMessage>> {
        let guard = self
            .rx
            .lock()
            .map_err(|_| anyhow!("Discord receiver mutex poisoned"))?;

        match guard.recv_timeout(Duration::from_secs(30)) {
            Ok(first) => {
                let mut messages = vec![first];
                while let Ok(next) = guard.try_recv() {
                    messages.push(next);
                }
                Ok(messages)
            }
            Err(RecvTimeoutError::Timeout) => Ok(Vec::new()),
            Err(RecvTimeoutError::Disconnected) => {
                Err(anyhow!("Discord Gateway message channel disconnected"))
            }
        }
    }

    fn send_message(&self, chat_id: &str, text: &str) -> Result<()> {
        for chunk in split_discord_message(text, self.max_message_chars) {
            self.rest_send_json(
                "sendMessage",
                "POST",
                &format!("channels/{chat_id}/messages"),
                &MessageContentBody::new(&chunk),
            )?;
        }
        Ok(())
    }

    fn reply_message(
        &self,
        chat_id: &str,
        reply_context: Option<&str>,
        text: &str,
    ) -> Result<()> {
        let Some(ctx) = reply_context else {
            return self.send_message(chat_id, text);
        };
        let Some((app_id, interaction_token)) = ctx.split_once('/') else {
            return self.send_message(chat_id, text);
        };

        let chunks = split_discord_message(text, self.max_message_chars);
        let bodies: Vec<MessageContentBody<'_>> = chunks
            .iter()
            .map(|chunk| MessageContentBody::new(chunk))
            .collect();
        self.send_webhook_batches(app_id, interaction_token, &bodies)
    }

    fn send_codes(
        &self,
        chat_id: &str,
        header: &str,
        items: &[CodeItem],
        fallback_text: &str,
    ) -> Result<()> {
        if items.is_empty() {
            return self.send_message(chat_id, fallback_text);
        }
        let batches = build_code_embed_batches(
            header,
            items,
            self.embed_color,
            self.max_codes_per_embed,
        );
        for batch in &batches {
            self.rest_send_json(
                "sendCodesEmbed",
                "POST",
                &format!("channels/{chat_id}/messages"),
                batch,
            )?;
        }
        Ok(())
    }

    fn reply_codes(
        &self,
        chat_id: &str,
        reply_context: Option<&str>,
        header: &str,
        items: &[CodeItem],
        fallback_text: &str,
    ) -> Result<()> {
        if items.is_empty() {
            return self.reply_message(chat_id, reply_context, fallback_text);
        }
        let Some(ctx) = reply_context else {
            return self.send_codes(chat_id, header, items, fallback_text);
        };
        let Some((app_id, interaction_token)) = ctx.split_once('/') else {
            return self.send_codes(chat_id, header, items, fallback_text);
        };

        let batches = build_code_embed_batches(
            header,
            items,
            self.embed_color,
            self.max_codes_per_embed,
        );
        self.send_webhook_batches(app_id, interaction_token, &batches)
    }
}

struct GatewayContext {
    agent: Agent,
    api_base: String,
    token: String,
    application_id: String,
    reconnect_delay: Duration,
    tx: mpsc::Sender<IncomingMessage>,
}

fn run_gateway_loop(ctx: GatewayContext) {
    let mut intents = INTENTS_WITH_MESSAGE_CONTENT;

    loop {
        let ws_url = fetch_gateway_ws_url(&ctx);
        info!(url = %ws_url, intents, "Connecting to Discord Gateway");

        match connect(ws_url.as_str()) {
            Ok((mut socket, _)) => {
                configure_socket_timeout(&socket, Duration::from_secs(1));
                let outcome = run_gateway_session(&ctx, &mut socket, intents);
                if matches!(outcome, SessionOutcome::DisallowedIntents)
                    && intents != INTENTS_WITHOUT_MESSAGE_CONTENT
                {
                    warn!(
                        "Discord rejected Message Content Intent (close code 4014). Falling back to Slash Commands only; enable Message Content Intent in the Discord Developer Portal to also handle plain-text commands."
                    );
                    intents = INTENTS_WITHOUT_MESSAGE_CONTENT;
                }
            }
            Err(err) => {
                warn!(
                    "Discord Gateway connection failed: {}",
                    redact_secret(&ctx.token, err.to_string())
                );
            }
        }

        thread::sleep(ctx.reconnect_delay);
    }
}

enum SessionOutcome {
    Reconnect,
    DisallowedIntents,
}

fn run_gateway_session(
    ctx: &GatewayContext,
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    intents: u64,
) -> SessionOutcome {
    let mut seq: Option<u64> = None;
    let mut heartbeat_interval: Option<Duration> = None;
    let mut next_heartbeat: Option<Instant> = None;
    let mut identified = false;

    loop {
        if let (Some(interval), Some(deadline)) = (heartbeat_interval, next_heartbeat) {
            if Instant::now() >= deadline {
                if let Err(err) = send_heartbeat(socket, seq) {
                    warn!(
                        "Failed to send Discord Gateway heartbeat: {}",
                        redact_secret(&ctx.token, err.to_string())
                    );
                    return SessionOutcome::Reconnect;
                }
                next_heartbeat = Some(Instant::now() + interval);
            }
        }

        match socket.read() {
            Ok(Message::Text(raw)) => {
                let event: GatewayEvent = match serde_json::from_str(&raw) {
                    Ok(ev) => ev,
                    Err(err) => {
                        warn!("Failed to parse Discord Gateway event: {err}");
                        continue;
                    }
                };

                if let Some(s) = event.s {
                    seq = Some(s);
                }

                match event.op {
                    10 => {
                        let interval_ms = event
                            .d
                            .and_then(|d| serde_json::from_value::<HelloData>(d).ok())
                            .map(|h| h.heartbeat_interval.max(1000))
                            .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_MS);
                        let interval = Duration::from_millis(interval_ms);
                        heartbeat_interval = Some(interval);
                        next_heartbeat = Some(Instant::now() + interval / 2);

                        if !identified {
                            if let Err(err) = send_identify(socket, &ctx.token, intents) {
                                warn!(
                                    "Failed to send Discord Gateway Identify: {}",
                                    redact_secret(&ctx.token, err.to_string())
                                );
                                return SessionOutcome::Reconnect;
                            }
                            identified = true;
                        }
                    }
                    1 => {
                        if let Err(err) = send_heartbeat(socket, seq) {
                            warn!(
                                "Failed to reply to Discord Gateway heartbeat request: {}",
                                redact_secret(&ctx.token, err.to_string())
                            );
                            return SessionOutcome::Reconnect;
                        }
                        if let Some(interval) = heartbeat_interval {
                            next_heartbeat = Some(Instant::now() + interval);
                        }
                    }
                    7 => {
                        info!("Discord Gateway requested reconnect (op 7)");
                        return SessionOutcome::Reconnect;
                    }
                    9 => {
                        warn!("Discord Gateway invalid session (op 9), reconnecting");
                        return SessionOutcome::Reconnect;
                    }
                    0 => {
                        handle_dispatch_event(ctx, event.t.as_deref(), event.d);
                    }
                    _ => {}
                }
            }
            Ok(Message::Ping(payload)) => {
                if let Err(err) = socket.send(Message::Pong(payload)) {
                    warn!(
                        "Failed to send Discord Gateway Pong: {}",
                        redact_secret(&ctx.token, err.to_string())
                    );
                    return SessionOutcome::Reconnect;
                }
            }
            Ok(Message::Close(frame)) => {
                let code = frame.as_ref().map(|f| u16::from(f.code));
                let reason = frame
                    .as_ref()
                    .map(|f| f.reason.to_string())
                    .unwrap_or_default();
                warn!(close_code = ?code, reason = %reason, "Discord Gateway closed connection");
                if code == Some(4014) {
                    return SessionOutcome::DisallowedIntents;
                }
                return SessionOutcome::Reconnect;
            }
            Ok(_) => {}
            Err(WsError::Io(err))
                if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(err) => {
                warn!(
                    "Discord Gateway read error: {}",
                    redact_secret(&ctx.token, err.to_string())
                );
                return SessionOutcome::Reconnect;
            }
        }
    }
}

fn handle_dispatch_event(
    ctx: &GatewayContext,
    event_name: Option<&str>,
    data: Option<serde_json::Value>,
) {
    let Some(data) = data else {
        return;
    };

    match event_name {
        Some("READY") => {
            info!("Discord Gateway READY");
        }
        Some("MESSAGE_CREATE") => {
            let Ok(msg) = serde_json::from_value::<DiscordMessageData>(data) else {
                return;
            };
            if msg.author.as_ref().is_some_and(|author| author.bot) {
                return;
            }
            if msg.content.trim().is_empty() {
                return;
            }
            let incoming = IncomingMessage {
                chat_id: msg.channel_id,
                is_group: msg.guild_id.is_some(),
                auto_subscribe: false,
                text: Some(msg.content),
                reply_context: None,
            };
            if ctx.tx.send(incoming).is_err() {
                warn!("Failed to queue Discord MESSAGE_CREATE event");
            }
        }
        Some("INTERACTION_CREATE") => {
            let Ok(interaction) = serde_json::from_value::<DiscordInteractionData>(data) else {
                return;
            };
            if interaction.kind != 2 {
                return;
            }
            let (Some(channel_id), Some(cmd)) = (interaction.channel_id, interaction.data) else {
                return;
            };

            if let Err(err) = defer_interaction(ctx, &interaction.id, &interaction.token) {
                warn!(
                    "Failed to acknowledge Discord interaction {}: {}",
                    interaction.id,
                    redact_secret(&ctx.token, err)
                );
                return;
            }

            let incoming = IncomingMessage {
                chat_id: channel_id,
                is_group: interaction.guild_id.is_some(),
                auto_subscribe: false,
                text: Some(format_interaction_command(&cmd)),
                reply_context: Some(format!("{}/{}", ctx.application_id, interaction.token)),
            };
            if ctx.tx.send(incoming).is_err() {
                warn!("Failed to queue Discord INTERACTION_CREATE event");
            }
        }
        _ => {}
    }
}

fn format_interaction_command(data: &ApplicationCommandData) -> String {
    if let Some(sub) = data.options.iter().find(|opt| opt.kind == 1) {
        format!("/{}:{}", data.name, sub.name)
    } else {
        format!("/{}", data.name)
    }
}

fn defer_interaction(
    ctx: &GatewayContext,
    interaction_id: &str,
    interaction_token: &str,
) -> std::result::Result<(), String> {
    let url = format!(
        "{}/interactions/{interaction_id}/{interaction_token}/callback",
        ctx.api_base
    );
    ctx.agent
        .post(&url)
        .set("Authorization", &format!("Bot {}", ctx.token))
        .set("User-Agent", USER_AGENT)
        .send_json(&InteractionCallbackBody { kind: 5 })
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn fetch_gateway_ws_url(ctx: &GatewayContext) -> String {
    let endpoint = format!("{}/gateway/bot", ctx.api_base);
    let response = ctx
        .agent
        .get(&endpoint)
        .set("Authorization", &format!("Bot {}", ctx.token))
        .set("User-Agent", USER_AGENT)
        .call();

    match response {
        Ok(resp) => match resp.into_json::<GatewayBotResponse>() {
            Ok(body) => gateway_ws_url(&body.url),
            Err(err) => {
                warn!("Failed to decode Discord /gateway/bot response: {err}");
                gateway_ws_url(DEFAULT_GATEWAY_URL)
            }
        },
        Err(err) => {
            warn!(
                "Failed to fetch Discord /gateway/bot URL: {}",
                redact_secret(&ctx.token, err.to_string())
            );
            gateway_ws_url(DEFAULT_GATEWAY_URL)
        }
    }
}

fn gateway_ws_url(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.contains('?') {
        trimmed.to_string()
    } else {
        format!("{trimmed}/?v=10&encoding=json")
    }
}

fn configure_socket_timeout(socket: &WebSocket<MaybeTlsStream<TcpStream>>, timeout: Duration) {
    let result = match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(timeout)),
        MaybeTlsStream::NativeTls(tls) => tls.get_ref().set_read_timeout(Some(timeout)),
        _ => Ok(()),
    };
    if let Err(err) = result {
        warn!("Failed to configure Discord WebSocket read timeout: {err}");
    }
}

fn send_identify(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    token: &str,
    intents: u64,
) -> Result<()> {
    let payload = IdentifyPayload {
        op: 2,
        d: IdentifyData {
            token,
            intents,
            properties: IdentifyProperties {
                os: std::env::consts::OS,
                browser: "gaming-news-bot",
                device: "gaming-news-bot",
            },
        },
    };
    let json = serde_json::to_string(&payload)?;
    socket.send(Message::Text(json))?;
    Ok(())
}

fn send_heartbeat(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    seq: Option<u64>,
) -> Result<()> {
    let json = serde_json::to_string(&HeartbeatPayload { op: 1, d: seq })?;
    socket.send(Message::Text(json))?;
    Ok(())
}

fn redact_secret(secret: &str, msg: String) -> String {
    if secret.is_empty() {
        msg
    } else {
        msg.replace(secret, "<REDACTED>")
    }
}

/// Splits text into Discord-sized chunks by Unicode scalar count, preferring newline boundaries.
pub fn split_discord_message(text: &str, max_chars: usize) -> Vec<String> {
    let limit = max_chars.max(1);
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for line in text.split('\n') {
        let line_len = line.chars().count();
        if line_len > limit {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
            let mut slice = String::new();
            let mut slice_len = 0usize;
            for ch in line.chars() {
                if slice_len == limit {
                    chunks.push(std::mem::take(&mut slice));
                    slice_len = 0;
                }
                slice.push(ch);
                slice_len += 1;
            }
            if !slice.is_empty() {
                current = slice;
                current_len = slice_len;
            }
            continue;
        }

        let needed = if current.is_empty() {
            line_len
        } else {
            line_len + 1
        };
        if current_len + needed > limit {
            chunks.push(std::mem::take(&mut current));
            current.push_str(line);
            current_len = line_len;
        } else {
            if !current.is_empty() {
                current.push('\n');
                current_len += 1;
            }
            current.push_str(line);
            current_len += line_len;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

const MAX_EMBED_DESCRIPTION_CHARS: usize = 4096;

/// Formats active or newly discovered giftcode items into Discord Rich Embeds with Markdown.
pub fn build_code_embed_batches(
    header: &str,
    items: &[CodeItem],
    color: u32,
    max_per_embed: usize,
) -> Vec<EmbedMessageBody> {
    let per_batch = max_per_embed.clamp(1, 25);
    let chunks: Vec<&[CodeItem]> = items.chunks(per_batch).collect();
    let total = chunks.len();

    chunks
        .into_iter()
        .enumerate()
        .map(|(idx, group)| {
            let heading = if total > 1 {
                format!("## {header} ({}/{total})", idx + 1)
            } else {
                format!("## {header}")
            };

            let mut sections = vec![heading];
            let mut game_groups: Vec<(&str, Vec<&CodeItem>)> = Vec::new();
            for item in group {
                if let Some((_, list)) = game_groups
                    .iter_mut()
                    .find(|(game, _)| *game == item.game.as_str())
                {
                    list.push(item);
                } else {
                    game_groups.push((item.game.as_str(), vec![item]));
                }
            }

            for (game, game_items) in game_groups {
                let mut entries = Vec::with_capacity(game_items.len());
                for item in game_items {
                    let reward = if item.reward.is_empty() {
                        "Không rõ"
                    } else {
                        item.reward.as_str()
                    };
                    entries.push(format!(
                        "- 🔑 **`{}`** *({})* — 🔗 [Đổi ngay]({})\n  - 💎 **Quà:** {}\n  - ⏳ **Hạn:** `{}`",
                        item.code, item.server, item.redeem_url, reward, item.expiry
                    ));
                }
                sections.push(format!("### 🎮 {game}\n{}", entries.join("\n\n")));
            }

            let mut description = sections.join("\n\n");
            if description.chars().count() > MAX_EMBED_DESCRIPTION_CHARS {
                let mut truncated: String = description
                    .chars()
                    .take(MAX_EMBED_DESCRIPTION_CHARS - 3)
                    .collect();
                truncated.push_str("...");
                description = truncated;
            }

            EmbedMessageBody {
                content: "",
                embeds: vec![DiscordEmbed { description, color }],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_multiline_messages_under_limit() {
        let input = "line1\nline2\nline3\nline4";
        let chunks = split_discord_message(input, 12);
        assert_eq!(chunks, vec!["line1\nline2", "line3\nline4"]);
    }

    #[test]
    fn splits_long_unicode_line_by_char_count() {
        let input = "🎁 Tiếng Việt dài quá giới hạn";
        let chunks = split_discord_message(input, 8);
        assert!(chunks.iter().all(|c| c.chars().count() <= 8));
        assert_eq!(chunks.concat(), input);
    }

    #[test]
    fn keeps_short_message_intact() {
        let chunks = split_discord_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn redacts_bot_token_in_error_strings() {
        let secret = "MTIz.abc.xyz";
        let redacted = redact_secret(secret, format!("error using token {secret}"));
        assert!(!redacted.contains(secret));
        assert!(redacted.contains("<REDACTED>"));
        assert_eq!(redact_secret(secret, "clean message".to_string()), "clean message");
    }

    #[test]
    fn formats_gateway_ws_url_safely() {
        assert_eq!(
            gateway_ws_url("wss://gateway.discord.gg/"),
            "wss://gateway.discord.gg/?v=10&encoding=json"
        );
        assert_eq!(
            gateway_ws_url("wss://gateway.discord.gg/?v=10&encoding=json"),
            "wss://gateway.discord.gg/?v=10&encoding=json"
        );
    }

    #[test]
    fn suppresses_link_embeds_in_message_payload() {
        let payload = serde_json::to_value(MessageContentBody::new(
            "https://genshin.hoyoverse.com/gift?code=TEST",
        ))
        .unwrap();
        assert_eq!(payload["flags"], 4);
        assert_eq!(
            payload["content"],
            "https://genshin.hoyoverse.com/gift?code=TEST"
        );
    }

    #[test]
    fn formats_top_level_and_game_subcommand_interactions() {
        let top_level = ApplicationCommandData {
            name: "codes".to_string(),
            options: Vec::new(),
        };
        assert_eq!(format_interaction_command(&top_level), "/codes");

        for (sub_name, expected) in [
            ("codes", "/gi:codes"),
            ("subscribe", "/gi:subscribe"),
            ("unsubscribe", "/gi:unsubscribe"),
        ] {
            let subcommand = ApplicationCommandData {
                name: "gi".to_string(),
                options: vec![ApplicationCommandOption {
                    name: sub_name.to_string(),
                    kind: 1,
                }],
            };
            assert_eq!(format_interaction_command(&subcommand), expected);
        }
    }

    #[test]
    fn builds_code_embeds_with_short_links_and_link_buttons() {
        let items: Vec<CodeItem> = (1..=7)
            .map(|i| CodeItem {
                game: "Genshin Impact".to_string(),
                code: format!("CODE{i}"),
                server: "G".to_string(),
                reward: if i == 1 {
                    String::new()
                } else {
                    "Primogem × 60".to_string()
                },
                expiry: "unknown".to_string(),
                redeem_url: format!("https://genshin.hoyoverse.com/gift?code=CODE{i}"),
            })
            .collect();

        let batches = build_code_embed_batches("📜 Giftcode:", &items, 5_793_266, 6);
        assert_eq!(batches.len(), 2);

        let first = serde_json::to_value(&batches[0]).unwrap();
        assert!(first.get("flags").is_none());
        assert!(first.get("components").is_none());
        let desc = first["embeds"][0]["description"].as_str().unwrap();
        assert!(desc.contains("## 📜 Giftcode: (1/2)"));
        assert!(desc.contains("### 🎮 Genshin Impact"));
        assert!(desc.contains(
            "- 🔑 **`CODE1`** *(G)* — 🔗 [Đổi ngay](https://genshin.hoyoverse.com/gift?code=CODE1)"
        ));
        assert!(desc.contains("  - 💎 **Quà:** Không rõ"));
        assert!(desc.contains("  - ⏳ **Hạn:** `unknown`"));
        assert!(!desc.contains("nút bên dưới"));

        let second = serde_json::to_value(&batches[1]).unwrap();
        let desc2 = second["embeds"][0]["description"].as_str().unwrap();
        assert!(desc2.contains("## 📜 Giftcode: (2/2)"));
        assert!(desc2.contains("CODE7"));
    }
}
