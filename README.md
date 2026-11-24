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
- **Per-notification-type intervals:** Each `NotificationKind` (`giftcodes`, `news`, etc.) has its own check interval under `[intervals]` in `config.toml`, and the bot only sends alerts when new unseen items appear.
- **Background execution on Windows:** Release builds hide the console window (`windows_subsystem = "windows"`), write logs to `data/bot.log`, and support automatic logon startup via Windows Task Scheduler.

## Commands
- `/start` or `/help`: Show usage instructions
- `/subscribe`: Subscribe to new code/news notifications
- `/unsubscribe`: Unsubscribe from notifications
- `/codes`: Fetch and display currently active codes

## Setup & Usage

1. Create `.env` from the template:
   ```powershell
   Copy-Item .env.example .env
   ```
   Set your messaging platform token in `.env` (e.g., `ZALO_BOT_TOKEN`).

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
