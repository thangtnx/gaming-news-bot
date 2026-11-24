use super::{ChatChannel, IncomingMessage};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use ureq::{Agent, AgentBuilder};

#[derive(Debug, Clone)]
pub struct ZaloChannel {
    agent: Agent,
    api_base: String,
    token: String,
    poll_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ZaloBotInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub username: String,
}

#[derive(Debug, Deserialize)]
struct GetMeResponse {
    ok: bool,
    result: Option<ZaloBotInfo>,
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetUpdatesResponse {
    ok: bool,
    result: Option<UpdatePayload>,
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum UpdatePayload {
    Single(ZaloUpdate),
    Batch(Vec<ZaloUpdate>),
}

#[derive(Debug, Clone, Deserialize)]
struct ZaloUpdate {
    message: Option<ZaloMessage>,
}

#[derive(Debug, Clone, Deserialize)]
struct ZaloMessage {
    chat: ZaloChat,
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ZaloChat {
    id: String,
    #[serde(default)]
    chat_type: String,
}

#[derive(Debug, Serialize)]
struct GetUpdatesRequest {
    timeout: String,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest<'a> {
    chat_id: &'a str,
    text: &'a str,
}

#[derive(Debug, Deserialize)]
struct SendMessageResponse {
    ok: bool,
    #[serde(default)]
    error_code: i64,
    #[serde(default)]
    description: Option<String>,
}

impl ZaloChannel {
    /// Creates a new Zalo Bot API channel client.
    pub fn new(
        api_base: impl Into<String>,
        token: impl Into<String>,
        poll_timeout_secs: u64,
    ) -> Result<Self> {
        let tls = Arc::new(
            native_tls::TlsConnector::new().context("Failed to initialize Windows TLS connector")?,
        );
        let agent = AgentBuilder::new()
            .tls_connector(tls)
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build();
        Ok(Self {
            agent,
            api_base: api_base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            poll_timeout_secs,
        })
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base, self.token, method)
    }

    fn redact_token(&self, msg: String) -> String {
        if self.token.is_empty() {
            msg
        } else {
            msg.replace(&self.token, "<REDACTED>")
        }
    }

    /// Verifies the bot token and retrieves bot profile info via `getMe`.
    pub fn get_me(&self) -> Result<ZaloBotInfo> {
        let body: GetMeResponse = self
            .agent
            .get(&self.endpoint("getMe"))
            .call()
            .map_err(|e| {
                anyhow!(
                    "Failed to call Zalo getMe: {}",
                    self.redact_token(e.to_string())
                )
            })?
            .into_json()
            .context("Failed to decode Zalo getMe JSON")?;

        if !body.ok {
            return Err(anyhow!(
                "Zalo getMe failed (code {}): {}",
                body.error_code,
                body.description.unwrap_or_default()
            ));
        }

        body.result
            .ok_or_else(|| anyhow!("Zalo getMe returned ok=true without result"))
    }
}

impl ChatChannel for ZaloChannel {
    fn poll_messages(&self) -> Result<Vec<IncomingMessage>> {
        let body: GetUpdatesResponse = self
            .agent
            .post(&self.endpoint("getUpdates"))
            .timeout(Duration::from_secs(self.poll_timeout_secs + 15))
            .send_json(&GetUpdatesRequest {
                timeout: self.poll_timeout_secs.to_string(),
            })
            .map_err(|e| {
                anyhow!(
                    "Failed to call Zalo getUpdates: {}",
                    self.redact_token(e.to_string())
                )
            })?
            .into_json()
            .context("Failed to decode Zalo getUpdates JSON")?;

        if !body.ok {
            if body.error_code == 408 {
                return Ok(Vec::new());
            }
            return Err(anyhow!(
                "Zalo getUpdates error (code {}): {}",
                body.error_code,
                body.description.unwrap_or_default()
            ));
        }

        let updates = match body.result {
            None => Vec::new(),
            Some(UpdatePayload::Single(u)) => vec![u],
            Some(UpdatePayload::Batch(list)) => list,
        };

        Ok(updates
            .into_iter()
            .filter_map(|u| u.message)
            .map(|msg| IncomingMessage {
                is_group: msg.chat.chat_type.eq_ignore_ascii_case("GROUP")
                    || msg.chat.id.starts_with("zgr-"),
                chat_id: msg.chat.id,
                text: msg.text,
            })
            .collect())
    }

    fn send_message(&self, chat_id: &str, text: &str) -> Result<()> {
        let body: SendMessageResponse = self
            .agent
            .post(&self.endpoint("sendMessage"))
            .send_json(&SendMessageRequest { chat_id, text })
            .map_err(|e| {
                anyhow!(
                    "Failed to call Zalo sendMessage: {}",
                    self.redact_token(e.to_string())
                )
            })?
            .into_json()
            .context("Failed to decode Zalo sendMessage JSON")?;

        if !body.ok {
            return Err(anyhow!(
                "Zalo sendMessage failed (code {}): {}",
                body.error_code,
                body.description.unwrap_or_default()
            ));
        }
        Ok(())
    }
}
