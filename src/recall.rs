use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::model::{ChatMessage, SearchHit, SessionMutation, SessionSummary};

pub fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[derive(Clone, Debug)]
pub enum SidebarRowItem {
    Local(SessionSummary),
    RemoteHit { hit: SearchHit, query: String },
}

impl SidebarRowItem {
    pub fn stored_id(&self) -> &str {
        match self {
            Self::Local(session) => &session.stored_id,
            Self::RemoteHit { hit, .. } => &hit.stored_id,
        }
    }

    pub fn search_query(&self) -> Option<&str> {
        match self {
            Self::Local(_) => None,
            Self::RemoteHit { query, .. } => Some(query.as_str()),
        }
    }
}

pub fn clean_session_preview(raw: &str, title: &str) -> String {
    let collapsed = collapse_inline_text(raw);
    let stripped = strip_cron_boilerplate(&collapsed);
    if stripped.is_empty() || stripped.eq_ignore_ascii_case(title.trim()) {
        return String::new();
    }
    truncate_ellipsis(stripped, 74)
}

fn strip_cron_boilerplate(text: &str) -> &str {
    if text.starts_with("[IMPORTANT: You are running as a scheduled") {
        return "";
    }
    text.trim_start_matches(['#', '>', '*', '-', ' ']).trim()
}

pub fn format_fts_snippet(raw: &str) -> (String, String) {
    let cleaned = clean_snippet_wrappers(raw);
    if cleaned.is_empty() {
        return (String::new(), String::new());
    }
    let segments = split_fts_markers(&cleaned);
    let windowed = window_segments(&segments, 118);
    render_segments(&windowed)
}

fn clean_snippet_wrappers(raw: &str) -> String {
    let unescaped = raw
        .replace("\\n", " ")
        .replace("\\r", " ")
        .replace("\\t", " ")
        .replace("\\\"", "\"");
    let trimmed = unescaped.trim();
    let unwrapped = strip_json_envelope(trimmed);
    collapse_inline_text(unwrapped)
}

fn strip_json_envelope(text: &str) -> &str {
    for prefix in [
        "{\"output\": \"",
        "{\"output\":\"",
        "{\"content\": \"",
        "{\"result\": \"",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return rest.trim_end_matches("\"}").trim();
        }
    }
    text
}

fn split_fts_markers(text: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find(">>>") {
        push_segment(&mut out, false, &rest[..open]);
        let after_open = &rest[open + 3..];
        if let Some(close) = after_open.find("<<<") {
            push_segment(&mut out, true, &after_open[..close]);
            rest = &after_open[close + 3..];
        } else {
            push_segment(&mut out, false, after_open);
            return out;
        }
    }
    push_segment(&mut out, false, rest);
    out
}

fn push_segment(out: &mut Vec<(bool, String)>, highlighted: bool, text: &str) {
    if !text.is_empty() {
        out.push((highlighted, text.to_string()));
    }
}

fn window_segments(segments: &[(bool, String)], max_chars: usize) -> Vec<(bool, String)> {
    let total_chars: usize = segments.iter().map(|(_, s)| s.chars().count()).sum();
    if total_chars <= max_chars {
        return segments.to_vec();
    }
    let first_hit_pos = chars_before_first_hit(segments);
    let start = first_hit_pos.saturating_sub(28);
    slice_segments_by_chars(segments, start, max_chars, total_chars)
}

fn chars_before_first_hit(segments: &[(bool, String)]) -> usize {
    let mut offset = 0usize;
    for (is_hit, text) in segments {
        if *is_hit {
            return offset;
        }
        offset += text.chars().count();
    }
    0
}

fn slice_segments_by_chars(
    segments: &[(bool, String)],
    start: usize,
    max_chars: usize,
    total_chars: usize,
) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    if start > 0 {
        out.push((false, "…".into()));
    }
    let end = start + max_chars;
    let mut cursor = 0usize;
    for (is_hit, text) in segments {
        let seg_len = text.chars().count();
        let seg_end = cursor + seg_len;
        if seg_end > start && cursor < end {
            let take_from = start.saturating_sub(cursor);
            let take_len = (end.min(seg_end)) - (cursor + take_from);
            let sub: String = text.chars().skip(take_from).take(take_len).collect();
            push_segment(&mut out, *is_hit, &sub);
        }
        cursor = seg_end;
    }
    if end < total_chars {
        out.push((false, "…".into()));
    }
    out
}

fn render_segments(segments: &[(bool, String)]) -> (String, String) {
    let mut plain = String::new();
    let mut markup = String::new();
    for (is_hit, text) in segments {
        plain.push_str(text);
        let escaped = escape_pango(text);
        if *is_hit {
            markup.push_str("<span bgcolor=\"#5c4818\" fgcolor=\"#ffe9a6\"><b>");
            markup.push_str(&escaped);
            markup.push_str("</b></span>");
        } else {
            markup.push_str(&escaped);
        }
    }
    (plain, markup)
}

pub fn escape_pango(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

pub fn find_transcript_matches(messages: &[ChatMessage], query: &str) -> Vec<usize> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let needle = trimmed.to_lowercase();
    let exact: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| msg.text.to_lowercase().contains(&needle).then_some(idx))
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    find_token_matches(messages, &needle)
}

fn find_token_matches(messages: &[ChatMessage], lowered_query: &str) -> Vec<usize> {
    let tokens: Vec<&str> = lowered_query
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .collect();
    if tokens.len() < 2 {
        return Vec::new();
    }
    messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| {
            let hay = msg.text.to_lowercase();
            tokens.iter().all(|tok| hay.contains(tok)).then_some(idx)
        })
        .collect()
}

pub fn mutation_patch_body(mutation: &SessionMutation) -> Value {
    match mutation {
        SessionMutation::Rename(title) => json!({ "title": title.trim() }),
        SessionMutation::SetPinned(pinned) => json!({ "pinned": pinned }),
        SessionMutation::Archive => json!({ "archived": true }),
    }
}

pub fn parse_search_results(body: &Value, secrets: &[String]) -> Result<Vec<SearchHit>, String> {
    let rows = body
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "Search results were not usable.".to_string())?;
    let mut hits = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(hit) = parse_search_hit(row, secrets) {
            hits.push(hit);
        }
    }
    Ok(hits)
}

fn parse_search_hit(row: &Value, secrets: &[String]) -> Option<SearchHit> {
    if row
        .get("archived")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let stored_id = row
        .get("id")
        .or_else(|| row.get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let raw_snippet = redact_with_secrets(
        row.get("snippet").and_then(Value::as_str).unwrap_or(""),
        secrets,
    );
    let (snippet_plain, snippet_markup) = format_fts_snippet(&raw_snippet);
    let raw_title = redact_with_secrets(
        row.get("title").and_then(Value::as_str).unwrap_or(""),
        secrets,
    );
    Some(SearchHit {
        stored_id,
        title: non_empty_or(&raw_title, "Untitled conversation"),
        source: row_trimmed(row, "source"),
        model: row_trimmed(row, "model"),
        role: row_trimmed(row, "role"),
        snippet_plain,
        snippet_markup,
        updated_at: extract_hit_epoch(row),
        message_count: row
            .get("message_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        pinned: row.get("pinned").and_then(Value::as_bool).unwrap_or(false),
        parent_session_id: non_empty_opt(&row_trimmed(row, "parent_session_id")),
    })
}

fn row_trimmed(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn non_empty_or(raw: &str, fallback: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        fallback.into()
    } else {
        trimmed.to_string()
    }
}

fn non_empty_opt(raw: &str) -> Option<String> {
    (!raw.is_empty()).then(|| raw.to_string())
}

fn extract_hit_epoch(row: &Value) -> String {
    for key in ["last_active", "updated_at", "started_at"] {
        if let Some(secs) = row.get(key).and_then(Value::as_f64)
            && secs.is_finite()
            && secs > 0.0
        {
            return (secs as i64).to_string();
        }
    }
    String::new()
}

fn redact_with_secrets(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if secret.len() >= 3 {
            out = out.replace(secret, "[REDACTED]");
        }
    }
    out
}

fn collapse_inline_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(160));
    let mut space = false;
    for ch in raw.chars() {
        if ch.is_control() && !ch.is_whitespace() {
            continue;
        }
        if ch.is_whitespace() {
            if !space && !out.is_empty() {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(ch);
            space = false;
        }
    }
    out.trim().to_string()
}

fn truncate_ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let prefix: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", prefix.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Role;

    #[test]
    fn preview_strips_cron_boilerplate_and_duplicate_titles() {
        assert_eq!(
            clean_session_preview(
                "[IMPORTANT: You are running as a scheduled cron job — autonomous]",
                "daily-backup"
            ),
            ""
        );
        assert_eq!(
            clean_session_preview("Hyprland keybinds", "hyprland keybinds"),
            ""
        );
        assert_eq!(
            clean_session_preview("##   Configure   waybar\nmodules  ", "Other title"),
            "Configure waybar modules"
        );
    }

    #[test]
    fn fts_snippet_escapes_xml_and_highlights_markers_safely() {
        let raw = "{\"output\": \"<tag> & >>>tailscale<<< status\\nok\"}";
        let (plain, markup) = format_fts_snippet(raw);
        assert_eq!(plain, "<tag> & tailscale status ok");
        assert!(markup.contains("&lt;tag&gt; &amp; "));
        assert!(markup.contains("<b>tailscale</b>"));
        assert!(!markup.contains(">>>"));
    }

    #[test]
    fn long_fts_snippet_windows_around_first_hit_without_breaking_markup() {
        let prefix = "alpha ".repeat(25);
        let suffix = " omega".repeat(25);
        let raw = format!("{prefix}>>>needle<<<{suffix}");
        let (plain, markup) = format_fts_snippet(&raw);
        assert!(plain.starts_with('…'));
        assert!(plain.ends_with('…'));
        assert!(plain.contains("needle"));
        assert!(markup.contains("<b>needle</b>"));
    }

    #[test]
    fn transcript_matches_exact_substring_and_multi_word_fallback() {
        let msgs = vec![
            ChatMessage {
                id: "1".into(),
                role: Role::User,
                text: "How do I configure systemd timers on Arch?".into(),
            },
            ChatMessage {
                id: "2".into(),
                role: Role::Assistant,
                text: "Create a .timer unit in ~/.config/systemd/user/.".into(),
            },
        ];
        assert_eq!(find_transcript_matches(&msgs, "systemd timers"), vec![0]);
        assert_eq!(find_transcript_matches(&msgs, "systemd unit"), vec![1]);
        assert!(find_transcript_matches(&msgs, "   ").is_empty());
    }

    #[test]
    fn search_results_skip_archived_and_redact_secrets() {
        let body = json!({
            "results": [
                {
                    "id": "s-live",
                    "title": "Token supersecret123 check",
                    "source": "tui",
                    "model": "glm-5.1",
                    "role": "assistant",
                    "snippet": "Verified >>>supersecret123<<< ok",
                    "last_active": 1776000100.5,
                    "message_count": 6,
                    "pinned": true,
                    "archived": false
                },
                {
                    "id": "s-arch",
                    "title": "Old",
                    "archived": true,
                    "snippet": ">>>old<<<"
                }
            ]
        });
        let hits = parse_search_results(&body, &["supersecret123".into()]).expect("hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].stored_id, "s-live");
        assert!(hits[0].pinned);
        assert!(!hits[0].title.contains("supersecret123"));
        assert!(!hits[0].snippet_plain.contains("supersecret123"));
        assert!(hits[0].snippet_markup.contains("[REDACTED]"));
    }

    #[test]
    fn mutation_patch_payloads_match_gateway_contract() {
        assert_eq!(
            mutation_patch_body(&SessionMutation::Rename("  Clean title  ".into())),
            json!({"title": "Clean title"})
        );
        assert_eq!(
            mutation_patch_body(&SessionMutation::SetPinned(true)),
            json!({"pinned": true})
        );
        assert_eq!(
            mutation_patch_body(&SessionMutation::Archive),
            json!({"archived": true})
        );
    }
}
