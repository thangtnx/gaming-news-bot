use super::{CodeItem, NewsSource, NotificationKind};
use anyhow::{anyhow, Context, Result};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use ureq::Agent;

pub struct GenshinFandomSource {
    agent: Agent,
    api_url: String,
    allowed_servers: Vec<String>,
    redeem_url: String,
}

impl GenshinFandomSource {
    pub fn new(
        agent: Agent,
        api_url: impl Into<String>,
        allowed_servers: Vec<String>,
        redeem_url: impl Into<String>,
    ) -> Self {
        Self {
            agent,
            api_url: api_url.into(),
            allowed_servers,
            redeem_url: redeem_url.into(),
        }
    }
}

impl NewsSource for GenshinFandomSource {
    fn game_id(&self) -> &str {
        "gi"
    }

    fn name(&self) -> &str {
        "Genshin Impact"
    }

    fn kind(&self) -> NotificationKind {
        NotificationKind::Giftcodes
    }

    fn fetch_active_items(&self) -> Result<Vec<CodeItem>> {
        let payload: MediaWikiResponse = self
            .agent
            .get(&self.api_url)
            .set("User-Agent", "gaming-news-bot/0.1 (Rust)")
            .call()
            .context("Failed to request Fandom API")?
            .into_json()
            .context("Failed to decode Fandom JSON")?;

        let page = payload
            .query
            .pages
            .into_values()
            .next()
            .ok_or_else(|| anyhow!("No page found in Fandom API response"))?;

        let revisions = page
            .revisions
            .ok_or_else(|| anyhow!("No revisions found in Fandom API response"))?;
        let wikitext = &revisions
            .first()
            .ok_or_else(|| anyhow!("Empty revisions list in Fandom API response"))?
            .slots
            .main
            .content;

        let today_utc = Utc::now().date_naive();
        Ok(parse_active_codes(
            wikitext,
            &self.allowed_servers,
            &self.redeem_url,
            today_utc,
        ))
    }
}

#[derive(Debug, Deserialize)]
struct MediaWikiResponse {
    query: MediaWikiQuery,
}

#[derive(Debug, Deserialize)]
struct MediaWikiQuery {
    pages: HashMap<String, MediaWikiPage>,
}

#[derive(Debug, Deserialize)]
struct MediaWikiPage {
    revisions: Option<Vec<MediaWikiRevision>>,
}

#[derive(Debug, Deserialize)]
struct MediaWikiRevision {
    slots: MediaWikiSlots,
}

#[derive(Debug, Deserialize)]
struct MediaWikiSlots {
    main: MediaWikiMainSlot,
}

#[derive(Debug, Deserialize)]
struct MediaWikiMainSlot {
    #[serde(rename = "*")]
    content: String,
}

/// Parses `==Active Codes==` wikitext and filters by server, `notacode`, and UTC expiry date.
pub fn parse_active_codes(
    wikitext: &str,
    allowed_servers: &[String],
    redeem_url: &str,
    today_utc: NaiveDate,
) -> Vec<CodeItem> {
    let Some(section) = extract_active_section(wikitext) else {
        return Vec::new();
    };
    let cleaned = strip_html_comments(section);
    let blocks = extract_code_row_blocks(&cleaned);

    let mut results = Vec::new();
    for block in blocks {
        if let Some(item) = parse_code_row(&block, allowed_servers, redeem_url, today_utc) {
            results.push(item);
        }
    }
    results
}

fn extract_active_section(wikitext: &str) -> Option<&str> {
    let start_marker = "==Active Codes==";
    let start_idx = wikitext.find(start_marker)? + start_marker.len();
    let rest = &wikitext[start_idx..];

    let end_idx = rest
        .find("{{Code Row/Footer}}")
        .or_else(|| rest.find("\n=="))
        .unwrap_or(rest.len());

    Some(&rest[..end_idx])
}

fn strip_html_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 4..];
        if let Some(end) = after_start.find("-->") {
            rest = &after_start[end + 3..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

fn extract_code_row_blocks(input: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut search_from = 0;

    while let Some(rel_start) = input[search_from..].find("{{Code Row") {
        let start = search_from + rel_start;
        let after_name = &input[start + "{{Code Row".len()..];
        if after_name.starts_with('/') {
            search_from = start + "{{Code Row".len();
            continue;
        }

        if let Some(end) = find_matching_template_end(input, start) {
            blocks.push(input[start..end].to_string());
            search_from = end;
        } else {
            break;
        }
    }

    blocks
}

fn find_matching_template_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut i = start;
    let mut depth = 0usize;

    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            depth = depth.saturating_sub(1);
            i += 2;
            if depth == 0 {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

fn parse_code_row(
    block: &str,
    allowed_servers: &[String],
    redeem_url: &str,
    today_utc: NaiveDate,
) -> Option<CodeItem> {
    let inner = block
        .strip_prefix("{{")
        .and_then(|s| s.strip_suffix("}}"))?;

    let parts = split_top_level_pipes(inner);
    if parts.is_empty() {
        return None;
    }

    let mut positional = Vec::new();
    let mut notacode = false;

    for part in parts.into_iter().skip(1) {
        let trimmed = part.trim();
        if let Some((key, val)) = split_named_param(trimmed) {
            if key.eq_ignore_ascii_case("notacode") && val.eq_ignore_ascii_case("yes") {
                notacode = true;
            }
        } else {
            positional.push(trimmed.to_string());
        }
    }

    if notacode {
        return None;
    }

    let code = positional.first()?.trim().to_string();
    let server = positional.get(1)?.trim().to_string();
    if code.is_empty() || server.is_empty() {
        return None;
    }

    let server_allowed = allowed_servers
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&server));
    if !server_allowed {
        return None;
    }

    let reward_raw = positional.get(2).map(String::as_str).unwrap_or("");
    let expiry = positional
        .get(4)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    if is_expired(&expiry, today_utc) {
        return None;
    }

    let first_code = code.split(';').next().unwrap_or(&code).trim();
    let direct_url = format!("{}?code={first_code}", redeem_url.trim_end_matches('/'));

    Some(CodeItem {
        game: "Genshin Impact".to_string(),
        code,
        server,
        reward: format_reward(reward_raw),
        expiry: if expiry.is_empty() {
            "unknown".to_string()
        } else {
            expiry
        },
        redeem_url: direct_url,
    })
}

fn split_top_level_pipes(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;

    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '{' && chars[i + 1] == '{' {
            brace_depth += 1;
            current.push_str("{{");
            i += 2;
        } else if i + 1 < chars.len() && chars[i] == '}' && chars[i + 1] == '}' {
            brace_depth = brace_depth.saturating_sub(1);
            current.push_str("}}");
            i += 2;
        } else if i + 1 < chars.len() && chars[i] == '[' && chars[i + 1] == '[' {
            bracket_depth += 1;
            current.push_str("[[");
            i += 2;
        } else if i + 1 < chars.len() && chars[i] == ']' && chars[i + 1] == ']' {
            bracket_depth = bracket_depth.saturating_sub(1);
            current.push_str("]]");
            i += 2;
        } else if chars[i] == '|' && brace_depth == 0 && bracket_depth == 0 {
            parts.push(current);
            current = String::new();
            i += 1;
        } else {
            current.push(chars[i]);
            i += 1;
        }
    }
    parts.push(current);
    parts
}

fn split_named_param(param: &str) -> Option<(&str, &str)> {
    let eq_idx = param.find('=')?;
    let key = param[..eq_idx].trim();
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Some((key, param[eq_idx + 1..].trim()))
    } else {
        None
    }
}

fn is_expired(expiry: &str, today_utc: NaiveDate) -> bool {
    let trimmed = expiry.trim();
    if trimmed.len() >= 10 {
        if let Ok(date) = NaiveDate::parse_from_str(&trimmed[..10], "%Y-%m-%d") {
            return date < today_utc;
        }
    }
    false
}

fn format_reward(raw: &str) -> String {
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| item.replace('*', " × "))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_fandom_active_codes_and_filters_asia() {
        let sample = r#"
==Active Codes==
{{Code Row/Header}}<!--
Example comment:
{{Code Row|FAKECODE|G|Primogem*100|2020-01-01|unknown}}
-->
{{Code Row<!--
    -->|POYMANSXF877|G<!--
    -->|Primogem*20;Geode of Replication*160<!--
    -->|2026-11-20|unknown<!--
-->}}
{{Code Row<!--
    -->|ArchonsAnswers|G<!--
    -->|Primogem*40;Hero's Wit*3;Mora*30,000<!--
    -->|2026-11-15|2026-11-26|
       |ref=<ref>X: [https://x.com/GenshinImpact/status/1]</ref><!--
-->}}
{{Code Row<!--
   -->|YuanShen|CN<!--
   -->|Primogem*50;Hero's Wit*3<!--
   -->|2020-11-10|indef<!--
-->}}
{{Code Row|EXPIREDCODE|SEA|Primogem*60|2026-11-01|2026-11-20}}
{{Code Row|NOTACODEURL|A|notacode=yes|Primogem*60|2026-11-01|unknown}}
{{Code Row|SEAONLYCODE|SEA|Primogem*30|2026-11-24|indef}}
{{Code Row/Footer}}
"#;

        let today = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        let allowed = vec!["SEA".to_string(), "G".to_string(), "A".to_string()];
        let codes = parse_active_codes(
            sample,
            &allowed,
            "https://genshin.hoyoverse.com/en/gift",
            today,
        );

        assert_eq!(codes.len(), 3);
        assert_eq!(codes[0].code, "POYMANSXF877");
        assert_eq!(
            codes[0].reward,
            "Primogem × 20, Geode of Replication × 160"
        );
        assert_eq!(
            codes[0].redeem_url,
            "https://genshin.hoyoverse.com/en/gift?code=POYMANSXF877"
        );
        assert_eq!(codes[1].code, "ArchonsAnswers");
        assert_eq!(codes[1].expiry, "2026-11-26");
        assert_eq!(codes[2].code, "SEAONLYCODE");
        assert_eq!(codes[2].server, "SEA");
    }
}
