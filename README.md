# Gaming News Bot (Rust)

A modular notification and giftcode bot written entirely in **Rust**, designed to support **multiple games** and **multiple messaging platforms**.

## Architecture

```text
src/
├── sources/          # Game data sources (NewsSource trait & NotificationKind)
│   ├── mod.rs        # CodeItem, NotificationKind, and NewsSource definitions
│   └── genshin.rs    # Genshin Impact giftcode source (Fandom API)
├── channels/         # Messaging platforms (ChatChannel trait)
│   ├── mod.rs        # IncomingMessage and ChatChannel definitions
│   ├── discord.rs    # Discord REST + Gateway v10 integration (Slash & text commands)
│   └── zalo.rs       # Zalo Bot Platform integration (getMe, getUpdates, sendMessage)
├── config.rs         # TOML configuration loader (config.toml)
├── state.rs          # Persistent subscribers & sent-item tracking (data/state.json)
└── main.rs           # Per-source polling loops and chat command router
```

## Features
- **Game source (`sources/genshin.rs`):** Fetches active giftcodes from the Genshin Impact Fandom API, filters Asia-compatible servers (`SEA`, `G`, `A`), skips `notacode=yes` entries, and validates UTC expiration dates.
- **Messaging channel (`channels/zalo.rs`):**
  - Uses `getUpdates` (long polling); no public domain, port forwarding, or HTTPS required.
  - Supports both **direct messages** and **group chats** (handles `@mention` command prefixes and auto-subscribes groups on first interaction).
- **Messaging channel (`channels/discord.rs`):**
  - Connects via Discord Gateway v10 (`tungstenite` + `native-tls`) and REST API v10; runs concurrently with Zalo when both tokens are configured in `.env`.
  - Automatically registers global Slash Commands (`/start`, `/help`, `/subscribe`, `/unsubscribe`, `/codes`, and per-game subcommands such as `/gi codes`, `/gi subscribe`, `/gi unsubscribe`) and also handles plain-text commands (when **Message Content Intent** is enabled).
  - Splits messages exceeding Discord's 2000-character limit cleanly along line boundaries.
- **Per-notification-type intervals:** Each `NotificationKind` (`giftcodes`, `news`, etc.) has its own check interval under `[intervals]` in `config.toml`, and the bot only sends alerts when new unseen items appear.
- **Background execution on Windows:** Release builds hide the console window (`windows_subsystem = "windows"`), write logs to `data/bot.log`, and support automatic logon startup via Windows Task Scheduler.

## Commands
- `/start` or `/help`: Show usage instructions
- `/subscribe` (or `/sub`): Subscribe to new code/news notifications for all games
- `/unsubscribe` (or `/unsub`): Unsubscribe from all notifications
- `/codes`: Fetch and display currently active codes across all games
- `/<game>:codes`, `/<game>:subscribe` (`/sub`), `/<game>:unsubscribe` (`/unsub`): Per-game commands (e.g., `/gi:codes`, `/gi:sub`, `/gi:unsub`; on Discord use `/gi codes`, `/gi subscribe`, `/gi unsubscribe`)

## Setup & Usage

1. Create `.env` from the template:
   ```powershell
   Copy-Item .env.example .env
   ```
   Set your messaging platform token(s) in `.env` (`ZALO_BOT_TOKEN` and/or `DISCORD_BOT_TOKEN`).

   **Discord quick setup:**
   - Create an application and bot in the [Discord Developer Portal](https://discord.com/developers/applications).
   - Copy the Bot Token into `DISCORD_BOT_TOKEN` in `.env`, and optionally enable **Message Content Intent** under **Bot -> Privileged Gateway Intents** (Slash Commands work even without it).
   - Invite the bot to your server via **OAuth2 -> URL Generator** with scopes `bot` + `applications.commands` and permissions `View Channels`, `Send Messages`, and `Read Message History`.

2. Run in debug mode (prints logs to the terminal):
   ```powershell
   cargo run
   ```

3. Build the release binary (hidden console) and register Windows auto-start (defaults to the package name in `Cargo.toml`; override with `-TaskName`):
   ```powershell
   cargo build --release
   pwsh .\scripts\install-autostart.ps1
   Start-ScheduledTask -TaskName "gaming-news-bot"
   ```

4. Tail logs while running in the background:
   ```powershell
   Get-Content .\data\bot.log -Tail 30 -Wait
   ```

## Extending with New Games or Platforms
- **Add a new game or notification source:** Create `src/sources/<game>.rs` implementing `NewsSource`, add a variant to `GameSource` in `src/sources/mod.rs`, and register it in `src/main.rs`.
- **Add a new messaging platform (Telegram, Discord, Slack, etc.):** Create `src/channels/<platform>.rs` implementing `ChatChannel` (`poll_messages` & `send_message`), add a variant to `Channel` in `src/channels/mod.rs`, and wire it in `src/main.rs`.

## Configuration
Polling intervals per notification type (`[intervals]`: `giftcodes_secs`, `news_secs`), server filters, and outgoing message templates are configured in [config.toml](config.toml).
