use serde_json::Value;

use crate::model::format_compact_tokens;

pub const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;
pub const MAX_STAGED_IMAGES: usize = 6;
pub const SLASH_DECK_LIMIT: usize = 7;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceOption {
    pub root: String,
    pub label: String,
    pub sessions: usize,
    pub is_default: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceCatalog {
    pub options: Vec<WorkspaceOption>,
    pub default_cwd: String,
    pub home: String,
    pub loading: bool,
    pub error: Option<String>,
}

impl WorkspaceCatalog {
    pub fn search(&self, query: &str) -> Vec<usize> {
        let tokens = search_tokens(query);
        if tokens.is_empty() {
            return (0..self.options.len()).collect();
        }
        let mut ranked: Vec<(usize, usize)> = self
            .options
            .iter()
            .enumerate()
            .filter_map(|(idx, opt)| {
                let key = normalize_text(&format!("{} {}", opt.label, opt.root));
                score_all_tokens(&key, &tokens).map(|score| (score, idx))
            })
            .collect();
        ranked.sort_by_key(|&(score, idx)| (score, idx));
        ranked.into_iter().map(|(_, idx)| idx).collect()
    }

    pub fn caption_for(&self, active_cwd: Option<&str>, branch: &str) -> (String, String) {
        let effective = active_cwd
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(self.default_cwd.as_str());
        if effective.is_empty() {
            return ("Default workspace".into(), fallback_branch_line(branch));
        }
        let label = self
            .options
            .iter()
            .find(|opt| opt.root == effective)
            .map(|opt| opt.label.clone())
            .unwrap_or_else(|| path_basename(effective));
        let sub = format_path_and_branch(effective, &self.home, branch);
        (label, sub)
    }

    pub fn resolve_query(&self, query: &str) -> Option<Option<String>> {
        let clean = query.trim();
        if clean.eq_ignore_ascii_case("default") || clean == "~" {
            return Some(None);
        }
        let first = *self.search(clean).first()?;
        let opt = self.options.get(first)?;
        Some(if opt.is_default {
            None
        } else {
            Some(opt.root.clone())
        })
    }
}

pub fn short_path_display(path: &str, home: &str) -> String {
    let clean = path.trim();
    let home_clean = home.trim().trim_end_matches('/');
    if !home_clean.is_empty() {
        if clean == home_clean {
            return "~".into();
        }
        if let Some(rest) = clean.strip_prefix(home_clean)
            && rest.starts_with('/')
        {
            return format!("~{rest}");
        }
    }
    clean.to_string()
}

fn format_path_and_branch(path: &str, home: &str, branch: &str) -> String {
    let short = short_path_display(path, home);
    let clean_branch = branch.trim();
    if clean_branch.is_empty() {
        short
    } else {
        format!("{short} · ⎇ {clean_branch}")
    }
}

fn fallback_branch_line(branch: &str) -> String {
    let clean = branch.trim();
    if clean.is_empty() {
        "server default".into()
    } else {
        format!("⎇ {clean}")
    }
}

fn path_basename(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

pub fn parse_workspaces(body: &Value, secrets: &[String]) -> Result<WorkspaceCatalog, String> {
    let obj = body
        .as_object()
        .ok_or_else(|| "Workspace catalog was not usable.".to_string())?;
    let default_cwd = json_str(body, "default_cwd").to_string();
    let home = json_str(body, "home").to_string();
    let mut options = Vec::new();
    if let Some(repos) = obj.get("repos").and_then(Value::as_array) {
        append_workspace_rows(&mut options, repos, &default_cwd, secrets);
    }
    if let Some(projects) = obj.get("projects").and_then(Value::as_array) {
        append_workspace_rows(&mut options, projects, &default_cwd, secrets);
    }
    ensure_default_workspace(&mut options, &default_cwd);
    Ok(WorkspaceCatalog {
        options,
        default_cwd,
        home,
        loading: false,
        error: None,
    })
}

fn append_workspace_rows(
    out: &mut Vec<WorkspaceOption>,
    rows: &[Value],
    default_cwd: &str,
    secrets: &[String],
) {
    for row in rows {
        let Some(opt) = parse_workspace_row(row, default_cwd, secrets) else {
            continue;
        };
        if !out.iter().any(|existing| existing.root == opt.root) {
            out.push(opt);
        }
    }
}

fn parse_workspace_row(
    row: &Value,
    default_cwd: &str,
    secrets: &[String],
) -> Option<WorkspaceOption> {
    if let Some(raw_path) = row.as_str().map(str::trim).filter(|s| !s.is_empty()) {
        let root = raw_path.to_string();
        return Some(WorkspaceOption {
            label: path_basename(&root),
            is_default: !default_cwd.is_empty() && root == default_cwd,
            root,
            sessions: 0,
        });
    }
    let root = first_json_str(row, &["root", "path", "cwd"])?;
    let label = first_json_str(row, &["label", "name"])
        .map(|s| redact_text(&s, secrets))
        .unwrap_or_else(|| path_basename(&root));
    let sessions = row.get("sessions").and_then(Value::as_u64).unwrap_or(0) as usize;
    Some(WorkspaceOption {
        is_default: !default_cwd.is_empty() && root == default_cwd,
        root,
        label,
        sessions,
    })
}

fn ensure_default_workspace(options: &mut Vec<WorkspaceOption>, default_cwd: &str) {
    if default_cwd.is_empty() {
        return;
    }
    if let Some(pos) = options.iter().position(|opt| opt.root == default_cwd) {
        options[pos].is_default = true;
        if pos > 0 {
            let item = options.remove(pos);
            options.insert(0, item);
        }
        return;
    }
    options.insert(
        0,
        WorkspaceOption {
            root: default_cwd.to_string(),
            label: format!("{} (default)", path_basename(default_cwd)),
            sessions: 0,
            is_default: true,
        },
    );
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub category: String,
    pub usage: u64,
}

pub fn parse_skills(body: &Value, secrets: &[String]) -> Result<Vec<SkillSummary>, String> {
    let rows = body
        .as_array()
        .or_else(|| body.get("skills").and_then(Value::as_array))
        .ok_or_else(|| "Skills catalog was not usable.".to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(skill) = parse_skill_row(row, secrets) {
            out.push(skill);
        }
    }
    Ok(out)
}

fn parse_skill_row(row: &Value, secrets: &[String]) -> Option<SkillSummary> {
    if row.get("enabled").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    let name = first_json_str(row, &["name", "slug"])?;
    if name.chars().any(char::is_whitespace) {
        return None;
    }
    let description = redact_text(json_str(row, "description"), secrets);
    let category = redact_text(json_str(row, "category"), secrets);
    let usage = row.get("usage").and_then(Value::as_u64).unwrap_or(0);
    Some(SkillSummary {
        name,
        description: collapse_single_line(&description, 110),
        category,
        usage,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCandidate {
    pub token: String,
    pub insert_text: String,
    pub badge: String,
    pub description: String,
    pub is_skill: bool,
    pub zero_arg_execute: bool,
}

pub fn active_slash_prefix(draft: &str) -> Option<&str> {
    let rest = draft.strip_prefix('/')?;
    if rest.contains('\n') || rest.chars().any(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

pub fn built_in_slash_candidates() -> Vec<SlashCandidate> {
    let specs = [
        (
            "/btw",
            "/btw ",
            "SIDE-ASK",
            false,
            "Ask a snapshot question without saving to history",
        ),
        (
            "/compress",
            "/compress",
            "COMPACT",
            true,
            "Compact older turns on the server to free context",
        ),
        (
            "/attach",
            "/attach",
            "IMAGE",
            true,
            "Attach an image to stage with your next prompt (Ctrl+U)",
        ),
        (
            "/workspace",
            "/workspace ",
            "PROJECT",
            true,
            "Choose or switch remote project directory (Ctrl+Shift+W)",
        ),
        (
            "/branch",
            "/branch ",
            "TIMELINE",
            false,
            "Fork conversation into an independent writable branch",
        ),
        (
            "/undo",
            "/undo",
            "REWIND",
            true,
            "Drop the last user turn and reply from server history",
        ),
        (
            "/retry",
            "/retry",
            "REWIND",
            true,
            "Rewind last turn and load your prompt into the composer",
        ),
        (
            "/regen",
            "/regen",
            "REWIND",
            true,
            "Rewind last turn and immediately regenerate the reply",
        ),
        (
            "/find",
            "/find ",
            "SEARCH",
            true,
            "Search messages inside the current transcript (Ctrl+Shift+F)",
        ),
        (
            "/pin",
            "/pin",
            "CURATE",
            true,
            "Pin or unpin this conversation in the sidebar (Ctrl+P)",
        ),
        (
            "/rename",
            "/rename ",
            "CURATE",
            false,
            "Set a custom server-persisted title (/rename <title>)",
        ),
        (
            "/archive",
            "/archive",
            "CURATE",
            true,
            "Archive this conversation from active lists (Ctrl+Shift+E)",
        ),
    ];
    specs
        .into_iter()
        .map(
            |(token, insert_text, badge, zero_arg, desc)| SlashCandidate {
                token: token.into(),
                insert_text: insert_text.into(),
                badge: badge.into(),
                description: desc.into(),
                is_skill: false,
                zero_arg_execute: zero_arg,
            },
        )
        .collect()
}

pub fn rank_slash_candidates(
    draft: &str,
    skills: &[SkillSummary],
    limit: usize,
) -> Vec<SlashCandidate> {
    let Some(raw_prefix) = active_slash_prefix(draft) else {
        return Vec::new();
    };
    if limit == 0 {
        return Vec::new();
    }
    let needle = raw_prefix.to_ascii_lowercase();
    if needle.is_empty() {
        return default_slash_candidates(skills, limit);
    }
    score_slash_candidates(&needle, skills, limit)
}

fn default_slash_candidates(skills: &[SkillSummary], limit: usize) -> Vec<SlashCandidate> {
    let mut out = built_in_slash_candidates();
    let mut top_skills: Vec<&SkillSummary> = skills.iter().collect();
    top_skills.sort_by(|a, b| b.usage.cmp(&a.usage).then_with(|| a.name.cmp(&b.name)));
    let take_builtins = limit.min(5).min(out.len());
    out.truncate(take_builtins);
    for skill in top_skills.into_iter().take(limit.saturating_sub(out.len())) {
        out.push(skill_to_candidate(skill));
    }
    out
}

fn score_slash_candidates(
    needle: &str,
    skills: &[SkillSummary],
    limit: usize,
) -> Vec<SlashCandidate> {
    let mut scored: Vec<(usize, u8, std::cmp::Reverse<u64>, usize, SlashCandidate)> = Vec::new();
    for cand in built_in_slash_candidates() {
        let name = cand.token.trim_start_matches('/').to_ascii_lowercase();
        if let Some(score) = slash_match_score(&name, &cand.badge, &cand.description, needle) {
            scored.push((score, 0, std::cmp::Reverse(0), cand.token.len(), cand));
        }
    }
    for skill in skills {
        let name = skill.name.to_ascii_lowercase();
        if let Some(score) = slash_match_score(&name, &skill.category, &skill.description, needle) {
            let cand = skill_to_candidate(skill);
            scored.push((
                score,
                1,
                std::cmp::Reverse(skill.usage),
                cand.token.len(),
                cand,
            ));
        }
    }
    scored.sort_by(|a, b| (a.0, a.1, a.2, a.3, &a.4.token).cmp(&(b.0, b.1, b.2, b.3, &b.4.token)));
    scored.into_iter().take(limit).map(|row| row.4).collect()
}

fn slash_match_score(name: &str, category: &str, description: &str, needle: &str) -> Option<usize> {
    if name == needle {
        return Some(0);
    }
    if name.starts_with(needle) {
        return Some(1);
    }
    if let Some(pos) = name.find(needle) {
        return Some(3 + pos);
    }
    if category.to_ascii_lowercase().contains(needle)
        || description.to_ascii_lowercase().contains(needle)
    {
        return Some(25);
    }
    subsequence_gaps(name, needle).map(|gaps| 40 + gaps)
}

fn skill_to_candidate(skill: &SkillSummary) -> SlashCandidate {
    let badge = if skill.category.trim().is_empty() {
        "SKILL".into()
    } else {
        skill
            .category
            .trim()
            .chars()
            .take(14)
            .collect::<String>()
            .to_uppercase()
    };
    let description = if skill.description.is_empty() {
        format!("Invoke {} server skill", skill.name)
    } else {
        skill.description.clone()
    };
    SlashCandidate {
        token: format!("/{}", skill.name),
        insert_text: format!("/{} ", skill.name),
        badge,
        description,
        is_skill: true,
        zero_arg_execute: false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedImage {
    pub id: u64,
    pub filename: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

impl StagedImage {
    pub fn from_bytes(id: u64, raw_filename: &str, bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.is_empty() {
            return Err("Selected image is empty.".into());
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err("Image exceeds the 15 MB upload limit.".into());
        }
        let Some((mime, ext)) = infer_image_mime(&bytes) else {
            return Err("Unsupported image format (use PNG, JPEG, WebP, or GIF).".into());
        };
        Ok(Self {
            id,
            filename: sanitize_image_filename(raw_filename, id, ext),
            mime_type: mime.to_string(),
            bytes,
        })
    }

    pub fn from_file_bytes(id: u64, name: &str, bytes: Vec<u8>) -> Result<Self, String> {
        if infer_image_mime(&bytes).is_some() {
            return Self::from_bytes(id, name, bytes);
        }
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(format!("{name}: file is empty or exceeds 15 MB."));
        }
        let text = if bytes.starts_with(b"%PDF-") {
            pdf_extract::extract_text_from_mem(&bytes)
                .map_err(|_| format!("{name}: could not extract PDF text."))?
        } else {
            String::from_utf8(bytes).map_err(|_| format!("{name}: unsupported binary file."))?
        };
        if text.trim().is_empty() || text.contains('\0') {
            return Err(format!("{name}: no readable text found."));
        }
        if text.len() > 1_000_000 {
            return Err(format!("{name}: extracted text exceeds 1 MB."));
        }
        Ok(Self {
            id,
            filename: sanitize_image_filename(name, id, "txt"),
            mime_type: "text/plain".into(),
            bytes: text.into_bytes(),
        })
    }

    pub fn is_image(&self) -> bool {
        self.mime_type.starts_with("image/")
    }

    pub fn data_url(&self) -> String {
        let encoded = base64_encode(&self.bytes);
        format!("data:{};base64,{encoded}", self.mime_type)
    }

    pub fn size_label(&self) -> String {
        format_byte_size(self.bytes.len())
    }
}

pub fn infer_image_mime(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(("image/png", "png"));
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(("image/jpeg", "jpg"));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(("image/gif", "gif"));
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(("image/webp", "webp"));
    }
    None
}

fn sanitize_image_filename(raw: &str, id: u64, ext: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect::<String>();
    let trimmed = base.trim_matches('.');
    if trimmed.is_empty() {
        return format!("attachment-{id}.{ext}");
    }
    if trimmed.contains('.') {
        trimmed.to_string()
    } else {
        format!("{trimmed}.{ext}")
    }
}

pub fn format_byte_size(len: usize) -> String {
    if len < 1_024 {
        format!("{len} B")
    } else if len < 1_024 * 1_024 {
        format!("{:.1} KB", len as f64 / 1_024.0)
    } else {
        format!("{:.1} MB", len as f64 / (1_024.0 * 1_024.0))
    }
}

pub fn format_optimistic_prompt(text: &str, attachments: &[StagedImage]) -> String {
    let clean = text.trim();
    if attachments.is_empty() {
        return clean.to_string();
    }
    let chips: Vec<String> = attachments
        .iter()
        .map(|img| {
            format!(
                "{} {} ({})",
                if img.is_image() { "🖼" } else { "📄" },
                img.filename,
                img.size_label()
            )
        })
        .collect();
    let header = chips.join(" · ");
    if clean.is_empty() {
        header
    } else {
        format!("{header}\n\n{clean}")
    }
}

pub fn effective_wire_prompt(text: &str, has_attachments: bool) -> String {
    let clean = text.trim();
    if clean.is_empty() && has_attachments {
        "Please inspect the attached files.".into()
    } else {
        clean.to_string()
    }
}

pub fn derive_multimodal_launch_title(prompt: &str, attachments: &[StagedImage]) -> String {
    if !prompt.trim().is_empty() {
        return crate::model::derive_launch_title(prompt);
    }
    if let Some(first) = attachments.first() {
        return crate::model::derive_launch_title(&format!(
            "{}: {}",
            if first.is_image() { "Image" } else { "File" },
            first.filename
        ));
    }
    "New conversation".into()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompressSummary {
    pub removed: usize,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub noop: bool,
    pub headline: String,
}

impl CompressSummary {
    pub fn status_line(&self) -> String {
        if !self.headline.is_empty() {
            return self.headline.clone();
        }
        if self.noop || self.removed == 0 {
            return "Context is already compact — no older turns needed compression.".into();
        }
        if self.before_tokens > 0 && self.after_tokens > 0 {
            return format!(
                "Compacted {} messages ({} → {} tokens).",
                self.removed,
                format_compact_tokens(self.before_tokens),
                format_compact_tokens(self.after_tokens)
            );
        }
        format!("Compacted {} messages into a summary.", self.removed)
    }
}

pub fn parse_compress_summary(result: &Value, secrets: &[String]) -> CompressSummary {
    let removed = result.get("removed").and_then(Value::as_u64).unwrap_or(0) as usize;
    let before_tokens = result
        .get("before_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let after_tokens = result
        .get("after_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let summary_obj = result.get("summary");
    let noop = summary_noop(summary_obj, removed);
    let headline = summary_headline(summary_obj, secrets);
    CompressSummary {
        removed,
        before_tokens,
        after_tokens,
        noop,
        headline,
    }
}

fn summary_noop(summary_obj: Option<&Value>, removed: usize) -> bool {
    let Some(obj) = summary_obj else {
        return removed == 0;
    };
    obj.get("noop").and_then(Value::as_bool).unwrap_or(false)
        || obj.get("aborted").and_then(Value::as_bool).unwrap_or(false)
        || obj
            .get("refused_would_grow")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || removed == 0
}

fn summary_headline(summary_obj: Option<&Value>, secrets: &[String]) -> String {
    let Some(obj) = summary_obj else {
        return String::new();
    };
    let head = redact_text(json_str(obj, "headline"), secrets);
    let tokens = redact_text(json_str(obj, "token_line"), secrets);
    match (head.is_empty(), tokens.is_empty()) {
        (true, true) => String::new(),
        (false, true) => head,
        (true, false) => tokens,
        (false, false) => format!("{head} ({tokens})"),
    }
}

fn json_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
}

fn first_json_str(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        let s = json_str(value, k);
        (!s.is_empty()).then(|| s.to_string())
    })
}

fn redact_text(raw: &str, secrets: &[String]) -> String {
    let mut out = raw.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, "[REDACTED]");
        }
    }
    out
}

fn collapse_single_line(raw: &str, max_chars: usize) -> String {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= max_chars {
        return joined;
    }
    let prefix: String = joined.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", prefix.trim_end())
}

fn normalize_text(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect()
}

fn search_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(normalize_text)
        .flat_map(|s| s.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .filter(|t| !t.is_empty())
        .collect()
}

fn score_all_tokens(haystack: &str, tokens: &[String]) -> Option<usize> {
    let mut total = 0usize;
    for token in tokens {
        total += single_token_score(haystack, token)?;
    }
    Some(total)
}

fn single_token_score(haystack: &str, needle: &str) -> Option<usize> {
    if let Some(pos) = haystack.find(needle) {
        let word_start = pos == 0 || haystack.as_bytes()[pos - 1] == b' ';
        return Some(if word_start { 0 } else { 2 });
    }
    subsequence_gaps(haystack, needle).map(|gaps| 12 + gaps)
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        push_base64_chunk(&mut out, chunk, TABLE);
    }
    out
}

fn push_base64_chunk(out: &mut String, chunk: &[u8], table: &[u8]) {
    let b0 = chunk[0] as u32;
    let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
    let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
    let n = (b0 << 16) | (b1 << 8) | b2;
    out.push(table[((n >> 18) & 63) as usize] as char);
    out.push(table[((n >> 12) & 63) as usize] as char);
    if chunk.len() > 1 {
        out.push(table[((n >> 6) & 63) as usize] as char);
    } else {
        out.push('=');
    }
    if chunk.len() > 2 {
        out.push(table[(n & 63) as usize] as char);
    } else {
        out.push('=');
    }
}

fn subsequence_gaps(haystack: &str, needle: &str) -> Option<usize> {
    let mut chars = needle.chars();
    let mut expected = chars.next()?;
    let mut gaps = 0usize;
    for ch in haystack.chars() {
        if ch == expected {
            match chars.next() {
                Some(next) => expected = next,
                None => return Some(gaps),
            }
        } else {
            gaps += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TINY_PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
    ];
    const TINY_JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
    const TINY_GIF: &[u8] = b"GIF89a\x01\x00\x01\x00";
    const TINY_WEBP: &[u8] = b"RIFF\x1a\x00\x00\x00WEBP";

    #[test]
    fn parses_workspaces_and_promotes_default_cwd() {
        let payload = json!({
            "default_cwd": "/home/alice/.hermes",
            "home": "/home/alice",
            "repos": [
                {"root": "/home/alice/projects/hermes-native", "label": "hermes-native", "sessions": 4},
                {"root": "/home/alice/.hermes", "label": "hermes-home", "sessions": 12}
            ],
            "projects": ["/home/alice/projects/extra"]
        });
        let catalog = parse_workspaces(&payload, &["ali".to_string()]).expect("valid workspaces");
        assert_eq!(catalog.options.len(), 3);
        assert!(catalog.options[0].is_default);
        assert_eq!(catalog.options[0].root, "/home/alice/.hermes");
        assert_eq!(
            catalog.caption_for(Some("/home/alice/projects/hermes-native"), "main"),
            (
                "hermes-native".into(),
                "~/projects/hermes-native · ⎇ main".into()
            )
        );
        assert_eq!(
            catalog.resolve_query("native"),
            Some(Some("/home/alice/projects/hermes-native".into()))
        );
        assert_eq!(catalog.resolve_query("default"), Some(None));
    }

    #[test]
    fn parses_skills_and_skips_disabled_or_malformed_rows() {
        let payload = json!([
            {"name": "b-plan", "description": "Plan work carefully", "category": "buck-workflow", "enabled": true, "usage": 9},
            {"name": "disabled-skill", "description": "Hidden", "category": "misc", "enabled": false},
            {"name": "bad name", "description": "Whitespace", "enabled": true},
            {"name": "rust-gtk", "description": "GTK4 helper", "category": "", "usage": 2}
        ]);
        let skills = parse_skills(&payload, &[]).expect("skills array");
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "b-plan");
        assert_eq!(skills[1].name, "rust-gtk");
    }

    #[test]
    fn slash_prefix_hides_after_whitespace_or_newline() {
        assert_eq!(active_slash_prefix("/"), Some(""));
        assert_eq!(active_slash_prefix("/b-pl"), Some("b-pl"));
        assert_eq!(active_slash_prefix("/b-plan "), None);
        assert_eq!(active_slash_prefix("/btw hello"), None);
        assert_eq!(active_slash_prefix("plain text"), None);
        assert_eq!(active_slash_prefix("/foo\nbar"), None);
    }

    #[test]
    fn ranks_slash_builtins_and_server_skills_intelligently() {
        let skills = vec![
            SkillSummary {
                name: "b-plan".into(),
                description: "Strategic implementation plan".into(),
                category: "buck-workflow".into(),
                usage: 12,
            },
            SkillSummary {
                name: "b-review".into(),
                description: "Thorough code review".into(),
                category: "buck-workflow".into(),
                usage: 5,
            },
        ];
        let root = rank_slash_candidates("/", &skills, 7);
        assert_eq!(root.len(), 7);
        assert!(!root[0].is_skill);
        assert!(root.iter().any(|c| c.token == "/b-plan"));

        let plan_hits = rank_slash_candidates("/b-pl", &skills, 5);
        assert_eq!(plan_hits[0].token, "/b-plan");
        assert_eq!(plan_hits[0].insert_text, "/b-plan ");
        assert!(plan_hits[0].is_skill);
        assert!(!plan_hits[0].zero_arg_execute);

        let comp_hits = rank_slash_candidates("/comp", &skills, 5);
        assert_eq!(comp_hits[0].token, "/compress");
        assert!(comp_hits[0].zero_arg_execute);
    }

    #[test]
    fn validates_staged_images_and_encodes_data_urls() {
        let png =
            StagedImage::from_bytes(1, "/tmp/my shot!.png", TINY_PNG.to_vec()).expect("valid png");
        assert_eq!(png.mime_type, "image/png");
        assert_eq!(png.filename, "myshot.png");
        assert!(
            png.data_url()
                .starts_with("data:image/png;base64,iVBORw0KGgo")
        );

        assert!(
            StagedImage::from_bytes(2, "pic", TINY_JPEG.to_vec())
                .is_ok_and(|img| img.filename == "pic.jpg" && img.mime_type == "image/jpeg")
        );
        assert!(
            StagedImage::from_bytes(3, "", TINY_GIF.to_vec())
                .is_ok_and(|img| img.filename == "attachment-3.gif")
        );
        assert!(
            StagedImage::from_bytes(4, "clip.webp", TINY_WEBP.to_vec())
                .is_ok_and(|img| img.mime_type == "image/webp")
        );
        assert!(StagedImage::from_bytes(5, "empty.png", Vec::new()).is_err());
        assert!(StagedImage::from_bytes(6, "fake.png", b"not an image".to_vec()).is_err());
    }

    #[test]
    fn stages_text_and_rejects_unreadable_documents() {
        let file =
            StagedImage::from_file_bytes(1, "notes.txt", b"Important context".to_vec()).unwrap();
        assert!(!file.is_image());
        assert_eq!(file.bytes, b"Important context");
        assert!(StagedImage::from_file_bytes(2, "binary.bin", vec![0xff, 0xfe]).is_err());
        assert!(StagedImage::from_file_bytes(3, "bad.pdf", b"%PDF-invalid".to_vec()).is_err());
        assert!(StagedImage::from_file_bytes(4, "empty.txt", b"   ".to_vec()).is_err());
    }

    #[test]
    fn formats_multimodal_prompts_and_byte_sizes() {
        let img = StagedImage::from_bytes(1, "shot.png", TINY_PNG.to_vec()).unwrap();
        assert_eq!(format_byte_size(512), "512 B");
        assert_eq!(format_byte_size(2048), "2.0 KB");
        assert_eq!(format_byte_size(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(
            format_optimistic_prompt("Fix alignment", std::slice::from_ref(&img)),
            "🖼 shot.png (12 B)\n\nFix alignment"
        );
        assert_eq!(
            effective_wire_prompt("   ", true),
            "Please inspect the attached files."
        );
        assert_eq!(effective_wire_prompt("Hello", true), "Hello");
        assert_eq!(
            derive_multimodal_launch_title("", std::slice::from_ref(&img)),
            "Image: shot.png"
        );
    }

    #[test]
    fn parses_compress_summaries_for_active_and_noop_replies() {
        let compacted = parse_compress_summary(
            &json!({
                "status": "compressed",
                "removed": 6,
                "before_tokens": 18400,
                "after_tokens": 3200,
                "summary": {"noop": false, "headline": "Compacted 6 older messages", "token_line": "18.4k -> 3.2k"}
            }),
            &[],
        );
        assert!(!compacted.noop);
        assert_eq!(compacted.removed, 6);
        assert_eq!(
            compacted.status_line(),
            "Compacted 6 older messages (18.4k -> 3.2k)"
        );

        let noop = parse_compress_summary(
            &json!({
                "status": "compressed",
                "removed": 0,
                "summary": {"noop": true}
            }),
            &[],
        );
        assert!(noop.noop);
        assert!(noop.status_line().contains("already compact"));
    }
}
