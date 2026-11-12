use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::Value;

use crate::model::format_compact_tokens;

pub const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;
pub const MAX_STAGED_IMAGES: usize = 6;
const SLASH_DECK_LIMIT: usize = 7;

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

    pub fn data_url(&self) -> String {
        let encoded = glib::base64_encode(&self.bytes);
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
        .map(|img| format!("🖼 {} ({})", img.filename, img.size_label()))
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
        "Please inspect the attached image.".into()
    } else {
        clean.to_string()
    }
}

pub fn derive_multimodal_launch_title(prompt: &str, attachments: &[StagedImage]) -> String {
    if !prompt.trim().is_empty() {
        return crate::model::derive_launch_title(prompt);
    }
    if let Some(first) = attachments.first() {
        return crate::model::derive_launch_title(&format!("Image: {}", first.filename));
    }
    "New conversation".into()
}

pub fn clipboard_has_image_only(formats: &gdk::ContentFormats) -> bool {
    let mimes = formats.mime_types();
    if mimes.iter().any(|m| m.starts_with("text/plain")) {
        return false;
    }
    formats.contains_type(gdk::Texture::static_type())
        || mimes.iter().any(|m| m.starts_with("image/"))
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

// --- GTK4 Widgets ---

#[derive(Clone)]
struct WorkspaceRowItem {
    option: WorkspaceOption,
    short_path: String,
    active: bool,
}

#[derive(Clone)]
pub struct WorkspacePicker {
    pub button: gtk::MenuButton,
    label: gtk::Label,
    sublabel: gtk::Label,
    popover: gtk::Popover,
    search: gtk::SearchEntry,
    status: gtk::Label,
    results: gio::ListStore,
    selection: gtk::SingleSelection,
    list: gtk::ListView,
    catalog: Rc<RefCell<WorkspaceCatalog>>,
    active_cwd: Rc<RefCell<Option<String>>>,
}

impl WorkspacePicker {
    pub fn new() -> Rc<Self> {
        let (button, label, sublabel) = build_workspace_trigger();
        let (popover, search, status, results, selection, list) = build_workspace_popover();
        button.set_popover(Some(&popover));
        let picker = Rc::new(Self {
            button,
            label,
            sublabel,
            popover,
            search,
            status,
            results,
            selection,
            list,
            catalog: Rc::new(RefCell::new(WorkspaceCatalog::default())),
            active_cwd: Rc::new(RefCell::new(None)),
        });
        picker.wire_internal_navigation();
        picker
    }

    pub fn connect_selected(&self, on_pick: impl Fn(Option<String>) + 'static) {
        let cb: Rc<dyn Fn(Option<String>)> = Rc::new(on_pick);
        let cb_enter = Rc::clone(&cb);
        let picker_enter = self.clone();
        self.search.connect_activate(move |_| {
            picker_enter.commit_selection(cb_enter.as_ref());
        });
        let cb_list = Rc::clone(&cb);
        let picker_list = self.clone();
        self.list.connect_activate(move |_, _| {
            picker_list.commit_selection(cb_list.as_ref());
        });
    }

    pub fn connect_closed(&self, on_close: impl Fn() + 'static) {
        self.search.connect_stop_search(move |_| on_close());
    }

    pub fn open(&self) {
        if self.button.is_sensitive() {
            self.button.popup();
        }
    }

    pub fn close(&self) {
        self.popover.popdown();
    }

    pub fn sync(
        &self,
        catalog: &WorkspaceCatalog,
        active_cwd: Option<&str>,
        branch: &str,
        has_open_chat: bool,
        sensitive: bool,
    ) {
        *self.catalog.borrow_mut() = catalog.clone();
        *self.active_cwd.borrow_mut() = active_cwd.map(str::to_string);
        let (primary, secondary) = catalog.caption_for(active_cwd, branch);
        self.label.set_text(&primary);
        self.sublabel.set_text(&secondary);
        self.button.set_sensitive(sensitive);
        self.button.set_tooltip_text(Some(&workspace_tooltip(
            &primary,
            &secondary,
            has_open_chat,
        )));
        if self.popover.is_visible() {
            self.rebuild_rows();
        }
    }

    fn wire_internal_navigation(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(p) = weak.upgrade() {
                p.rebuild_rows();
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(p) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            p.handle_arrow_key(key)
        });
        self.search.add_controller(keys);
        let weak = Rc::downgrade(self);
        self.popover.connect_show(move |_| {
            if let Some(p) = weak.upgrade() {
                p.search.set_text("");
                p.rebuild_rows();
                p.search.grab_focus();
            }
        });
    }

    fn handle_arrow_key(&self, key: gdk::Key) -> glib::Propagation {
        let delta = match key {
            gdk::Key::Down | gdk::Key::KP_Down => 1i64,
            gdk::Key::Up | gdk::Key::KP_Up => -1i64,
            _ => return glib::Propagation::Proceed,
        };
        let count = i64::from(self.selection.n_items());
        if count > 0 {
            let cur = self.selection.selected();
            let base = if cur == gtk::INVALID_LIST_POSITION {
                0
            } else {
                i64::from(cur)
            };
            let next = (base + delta).clamp(0, count - 1) as u32;
            self.selection.set_selected(next);
            self.list.scroll_to(next, gtk::ListScrollFlags::NONE, None);
        }
        glib::Propagation::Stop
    }

    fn rebuild_rows(&self) {
        let catalog = self.catalog.borrow();
        let active = self.active_cwd.borrow();
        let effective_active = active
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(catalog.default_cwd.as_str());
        let query = self.search.text().to_string();
        let rows: Vec<glib::BoxedAnyObject> = catalog
            .search(&query)
            .into_iter()
            .filter_map(|idx| catalog.options.get(idx))
            .map(|opt| {
                glib::BoxedAnyObject::new(WorkspaceRowItem {
                    option: opt.clone(),
                    short_path: short_path_display(&opt.root, &catalog.home),
                    active: opt.root == effective_active,
                })
            })
            .collect();
        let status_text = workspace_status_message(&catalog, rows.is_empty());
        self.results.splice(0, self.results.n_items(), &rows);
        if !rows.is_empty() {
            self.selection.set_selected(0);
        }
        self.status.set_text(&status_text);
        self.status.set_visible(!status_text.is_empty());
    }

    fn commit_selection(&self, on_pick: &dyn Fn(Option<String>)) {
        let Some(item) = self
            .selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
        else {
            return;
        };
        let chosen = {
            let row = item.borrow::<WorkspaceRowItem>();
            if row.option.is_default {
                None
            } else {
                Some(row.option.root.clone())
            }
        };
        self.popover.popdown();
        on_pick(chosen);
    }
}

fn workspace_tooltip(primary: &str, secondary: &str, has_open_chat: bool) -> String {
    let hint = if has_open_chat {
        "Pick a remote project directory to start a new chat there (Ctrl+Shift+W)"
    } else {
        "Choose the remote project directory for new conversations (Ctrl+Shift+W)"
    };
    format!("{primary} · {secondary}\n{hint}")
}

fn workspace_status_message(catalog: &WorkspaceCatalog, empty_results: bool) -> String {
    if let Some(err) = &catalog.error {
        return err.clone();
    }
    if catalog.loading {
        return "Scanning remote workspaces…".into();
    }
    if catalog.options.is_empty() {
        return "No remote project directories reported.".into();
    }
    if empty_results {
        return "No workspace matches that search.".into();
    }
    String::new()
}

fn build_workspace_trigger() -> (gtk::MenuButton, gtk::Label, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let icon = gtk::Image::from_icon_name("folder-symbolic");
    icon.add_css_class("workspace-icon");
    row.append(&icon);

    let text_col = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let label = gtk::Label::new(Some("Default workspace"));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(24);
    label.add_css_class("model-caption");
    let sublabel = gtk::Label::new(Some("server default"));
    sublabel.set_xalign(0.0);
    sublabel.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    sublabel.set_max_width_chars(28);
    sublabel.add_css_class("model-provider");
    text_col.append(&label);
    text_col.append(&sublabel);
    row.append(&text_col);

    let button = gtk::MenuButton::new();
    button.set_child(Some(&row));
    button.add_css_class("flat");
    button.add_css_class("model-trigger");
    button.add_css_class("workspace-trigger");
    button.update_property(&[gtk::accessible::Property::Label("Workspace directory")]);
    (button, label, sublabel)
}

#[allow(clippy::type_complexity)]
fn build_workspace_popover() -> (
    gtk::Popover,
    gtk::SearchEntry,
    gtk::Label,
    gio::ListStore,
    gtk::SingleSelection,
    gtk::ListView,
) {
    let popover = gtk::Popover::new();
    popover.add_css_class("model-popover");
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    panel.set_margin_top(10);
    panel.set_margin_bottom(10);
    panel.set_margin_start(10);
    panel.set_margin_end(10);
    panel.set_width_request(380);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search remote git repos & workspaces…"));
    panel.append(&search);

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("picker-status");
    panel.append(&status);

    let results = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(results.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_bind(|_, obj| bind_workspace_row(obj));
    let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
    list.set_single_click_activate(true);
    list.add_css_class("model-list");
    let scroll = gtk::ScrolledWindow::builder()
        .min_content_height(180)
        .max_content_height(300)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    panel.append(&scroll);
    popover.set_child(Some(&panel));
    (popover, search, status, results, selection, list)
}

fn bind_workspace_row(object: &glib::Object) {
    let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
    let boxed = item
        .item()
        .and_downcast::<glib::BoxedAnyObject>()
        .expect("workspace row");
    let data = boxed.borrow::<WorkspaceRowItem>();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("model-row");
    let col = gtk::Box::new(gtk::Orientation::Vertical, 1);
    col.set_hexpand(true);
    let title = gtk::Label::new(Some(&data.option.label));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("model-name");
    let path = gtk::Label::new(Some(&data.short_path));
    path.set_xalign(0.0);
    path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    path.add_css_class("model-provider");
    col.append(&title);
    col.append(&path);
    row.append(&col);
    if data.option.sessions > 0 {
        let count = gtk::Label::new(Some(&format!("{} chats", data.option.sessions)));
        count.add_css_class("source-badge");
        count.set_valign(gtk::Align::Center);
        row.append(&count);
    }
    if data.active {
        row.add_css_class("model-current");
        row.append(&gtk::Image::from_icon_name("object-select-symbolic"));
    }
    item.set_child(Some(&row));
}

type SlashPickCallback = Rc<RefCell<Option<Box<dyn Fn(SlashCandidate)>>>>;

#[derive(Clone)]
pub struct SlashDeck {
    pub revealer: gtk::Revealer,
    rows_box: gtk::Box,
    meta_label: gtk::Label,
    candidates: Rc<RefCell<Vec<SlashCandidate>>>,
    selected: Rc<Cell<usize>>,
    dismissed_for: Rc<RefCell<Option<String>>>,
    last_stamp: Rc<RefCell<(String, usize)>>,
    on_pick: SlashPickCallback,
}

impl SlashDeck {
    pub fn new() -> Rc<Self> {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 4);
        outer.add_css_class("slash-deck");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let kicker = gtk::Label::new(Some("COMMANDS & SKILLS"));
        kicker.set_xalign(0.0);
        kicker.set_hexpand(true);
        kicker.add_css_class("inspector-kicker");
        let meta_label = gtk::Label::new(Some("Tab complete · ↑↓ navigate · Esc hide"));
        meta_label.add_css_class("composer-hint");
        top.append(&kicker);
        top.append(&meta_label);
        let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        outer.append(&top);
        outer.append(&rows_box);

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(110)
            .reveal_child(false)
            .visible(false)
            .child(&outer)
            .build();

        Rc::new(Self {
            revealer,
            rows_box,
            meta_label,
            candidates: Rc::new(RefCell::new(Vec::new())),
            selected: Rc::new(Cell::new(0)),
            dismissed_for: Rc::new(RefCell::new(None)),
            last_stamp: Rc::new(RefCell::new((String::new(), usize::MAX))),
            on_pick: Rc::new(RefCell::new(None)),
        })
    }

    pub fn connect_activated(&self, handler: impl Fn(SlashCandidate) + 'static) {
        *self.on_pick.borrow_mut() = Some(Box::new(handler));
    }

    pub fn is_open(&self) -> bool {
        self.revealer.is_visible() && !self.candidates.borrow().is_empty()
    }

    pub fn dismiss(&self, current_draft: &str) {
        *self.dismissed_for.borrow_mut() = Some(current_draft.to_string());
        self.hide();
    }

    pub fn hide(&self) {
        self.revealer.set_reveal_child(false);
        self.revealer.set_visible(false);
    }

    pub fn selected_candidate(&self) -> Option<SlashCandidate> {
        let items = self.candidates.borrow();
        items.get(self.selected.get()).cloned()
    }

    pub fn step(&self, delta: isize) -> bool {
        let len = self.candidates.borrow().len();
        if !self.is_open() || len == 0 {
            return false;
        }
        let next = (self.selected.get() as isize + delta).rem_euclid(len as isize) as usize;
        self.selected.set(next);
        self.update_row_highlights();
        true
    }

    pub fn sync(&self, draft: &str, skills: &[SkillSummary], editable: bool) {
        if !editable || active_slash_prefix(draft).is_none() {
            *self.dismissed_for.borrow_mut() = None;
            self.hide();
            return;
        }
        if self.dismissed_for.borrow().as_deref() == Some(draft) {
            self.hide();
            return;
        }
        let stamp = (draft.to_string(), skills.len());
        if *self.last_stamp.borrow() != stamp {
            *self.last_stamp.borrow_mut() = stamp;
            let ranked = rank_slash_candidates(draft, skills, SLASH_DECK_LIMIT);
            self.selected.set(0);
            *self.candidates.borrow_mut() = ranked;
            self.rebuild_rows(skills.len());
        }
        let has_rows = !self.candidates.borrow().is_empty();
        self.revealer.set_visible(has_rows);
        self.revealer.set_reveal_child(has_rows);
    }

    fn rebuild_rows(&self, total_skills: usize) {
        while let Some(child) = self.rows_box.first_child() {
            self.rows_box.remove(&child);
        }
        let hint = if total_skills > 0 {
            format!("{total_skills} skills · Tab complete · Enter run · Esc hide")
        } else {
            "Tab complete · Enter run · Esc hide".into()
        };
        self.meta_label.set_text(&hint);
        let active_idx = self.selected.get();
        for (idx, cand) in self.candidates.borrow().iter().enumerate() {
            let row = build_slash_row(cand, idx == active_idx, &self.on_pick);
            self.rows_box.append(&row);
        }
    }

    fn update_row_highlights(&self) {
        let active_idx = self.selected.get();
        let mut idx = 0usize;
        let mut child = self.rows_box.first_child();
        while let Some(widget) = child {
            if idx == active_idx {
                widget.add_css_class("slash-row-active");
            } else {
                widget.remove_css_class("slash-row-active");
            }
            idx += 1;
            child = widget.next_sibling();
        }
    }
}

fn build_slash_row(
    cand: &SlashCandidate,
    active: bool,
    on_pick: &SlashPickCallback,
) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let token = gtk::Label::new(Some(&cand.token));
    token.set_xalign(0.0);
    token.add_css_class("slash-token");
    let badge = gtk::Label::new(Some(&cand.badge));
    badge.add_css_class("source-badge");
    if cand.is_skill {
        badge.add_css_class("slash-skill-badge");
    }
    let desc = gtk::Label::new(Some(&cand.description));
    desc.set_xalign(0.0);
    desc.set_hexpand(true);
    desc.set_ellipsize(gtk::pango::EllipsizeMode::End);
    desc.add_css_class("slash-desc");
    row.append(&token);
    row.append(&badge);
    row.append(&desc);

    let button = gtk::Button::new();
    button.set_child(Some(&row));
    button.add_css_class("flat");
    button.add_css_class("slash-row");
    if active {
        button.add_css_class("slash-row-active");
    }
    let chosen = cand.clone();
    let cb = Rc::clone(on_pick);
    button.connect_clicked(move |_| {
        if let Some(handler) = cb.borrow().as_ref() {
            handler(chosen.clone());
        }
    });
    button
}

type RemoveAttachmentCallback = Rc<RefCell<Option<Box<dyn Fn(u64)>>>>;

#[derive(Clone)]
pub struct AttachmentStrip {
    pub root: gtk::Box,
    pub attach_btn: gtk::Button,
    rendered_ids: Rc<RefCell<Vec<u64>>>,
    rendered_editable: Rc<Cell<bool>>,
    on_remove: RemoveAttachmentCallback,
}

impl AttachmentStrip {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("attachment-strip");
        root.set_visible(false);

        let btn_inner = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        btn_inner.append(&gtk::Image::from_icon_name("insert-image-symbolic"));
        let btn_label = gtk::Label::new(Some("Image"));
        btn_label.add_css_class("palette-trigger-label");
        btn_inner.append(&btn_label);
        let attach_btn = gtk::Button::new();
        attach_btn.set_child(Some(&btn_inner));
        attach_btn.add_css_class("flat");
        attach_btn.add_css_class("attach-trigger");
        attach_btn.set_tooltip_text(Some(
            "Attach image · Ctrl+U, paste screenshot (Ctrl+V), or drag & drop",
        ));
        attach_btn.update_property(&[gtk::accessible::Property::Label("Attach image")]);

        Rc::new(Self {
            root,
            attach_btn,
            rendered_ids: Rc::new(RefCell::new(Vec::new())),
            rendered_editable: Rc::new(Cell::new(true)),
            on_remove: Rc::new(RefCell::new(None)),
        })
    }

    pub fn connect_remove(&self, handler: impl Fn(u64) + 'static) {
        *self.on_remove.borrow_mut() = Some(Box::new(handler));
    }

    pub fn sync(&self, items: &[StagedImage], can_stage: bool) {
        self.attach_btn
            .set_sensitive(can_stage && items.len() < MAX_STAGED_IMAGES);
        let ids: Vec<u64> = items.iter().map(|i| i.id).collect();
        if *self.rendered_ids.borrow() == ids && self.rendered_editable.get() == can_stage {
            self.root.set_visible(!items.is_empty());
            return;
        }
        *self.rendered_ids.borrow_mut() = ids;
        self.rendered_editable.set(can_stage);
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        for img in items {
            self.root
                .append(&build_attachment_pill(img, can_stage, &self.on_remove));
        }
        self.root.set_visible(!items.is_empty());
    }
}

fn build_attachment_pill(
    img: &StagedImage,
    editable: bool,
    on_remove: &RemoveAttachmentCallback,
) -> gtk::Box {
    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    pill.add_css_class("attachment-pill");
    if let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from(&img.bytes)) {
        let pic = gtk::Picture::for_paintable(&texture);
        pic.set_size_request(32, 32);
        pic.set_content_fit(gtk::ContentFit::Cover);
        pic.add_css_class("attachment-thumb");
        pill.append(&pic);
    } else {
        pill.append(&gtk::Image::from_icon_name("image-x-generic-symbolic"));
    }
    let meta = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let name = gtk::Label::new(Some(&img.filename));
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_max_width_chars(18);
    name.add_css_class("attachment-name");
    let size = gtk::Label::new(Some(&img.size_label()));
    size.set_xalign(0.0);
    size.add_css_class("model-provider");
    meta.append(&name);
    meta.append(&size);
    pill.append(&meta);

    let remove = gtk::Button::from_icon_name("window-close-symbolic");
    remove.add_css_class("flat");
    remove.add_css_class("circular");
    remove.add_css_class("attachment-remove");
    remove.set_tooltip_text(Some("Remove attached image"));
    remove.set_sensitive(editable);
    let id = img.id;
    let cb = Rc::clone(on_remove);
    remove.connect_clicked(move |_| {
        if let Some(handler) = cb.borrow().as_ref() {
            handler(id);
        }
    });
    pill.append(&remove);
    pill
}

pub fn read_local_image_files(
    files: &[gio::File],
    mut next_id: impl FnMut() -> u64,
) -> (Vec<StagedImage>, Option<String>) {
    let mut staged = Vec::new();
    let mut first_err = None;
    for file in files {
        let Some(path) = file.path() else {
            first_err.get_or_insert_with(|| "Only local image files can be attached.".into());
            continue;
        };
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.png");
        match std::fs::read(&path) {
            Ok(bytes) => match StagedImage::from_bytes(next_id(), filename, bytes) {
                Ok(image) => staged.push(image),
                Err(err) => {
                    first_err.get_or_insert(err);
                }
            },
            Err(_) => {
                first_err.get_or_insert_with(|| format!("Could not read {}.", filename));
            }
        }
    }
    (staged, first_err)
}

pub fn spawn_clipboard_image_paste(
    clipboard: gdk::Clipboard,
    on_result: impl FnOnce(Result<Vec<u8>, String>) + 'static,
) {
    glib::MainContext::default().spawn_local(async move {
        match clipboard.read_texture_future().await {
            Ok(Some(texture)) => {
                let png = texture.save_to_png_bytes();
                on_result(Ok(png.to_vec()));
            }
            Ok(None) => on_result(Err("Clipboard did not contain a readable image.".into())),
            Err(_) => on_result(Err(
                "Could not read image from the Wayland clipboard.".into()
            )),
        }
    });
}

pub fn spawn_image_file_dialog(
    window: &adw::ApplicationWindow,
    on_files: impl FnOnce(Vec<gio::File>) + 'static,
) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images (PNG, JPEG, WebP, GIF)"));
    for mime in ["image/png", "image/jpeg", "image/webp", "image/gif"] {
        filter.add_mime_type(mime);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("Attach image")
        .filters(&filters)
        .default_filter(&filter)
        .build();
    let win = window.clone();
    glib::MainContext::default().spawn_local(async move {
        let Ok(model) = dialog.open_multiple_future(Some(&win)).await else {
            return;
        };
        let mut files = Vec::new();
        for idx in 0..model.n_items() {
            if let Some(file) = model.item(idx).and_downcast::<gio::File>() {
                files.push(file);
            }
        }
        if !files.is_empty() {
            on_files(files);
        }
    });
}

pub fn install_image_drop_target(composer: &gtk::Box, on_files: impl Fn(Vec<gio::File>) + 'static) {
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let comp_enter = composer.clone();
    drop.connect_enter(move |_, _, _| {
        comp_enter.add_css_class("composer-drop-active");
        gdk::DragAction::COPY
    });
    let comp_leave = composer.clone();
    drop.connect_leave(move |_| {
        comp_leave.remove_css_class("composer-drop-active");
    });
    let comp_drop = composer.clone();
    drop.connect_drop(move |_, value, _, _| {
        comp_drop.remove_css_class("composer-drop-active");
        let Ok(file_list) = value.get::<gdk::FileList>() else {
            return false;
        };
        let files = file_list.files();
        if files.is_empty() {
            return false;
        }
        on_files(files);
        true
    });
    composer.add_controller(drop);
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
            "Please inspect the attached image."
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

    const VALID_1X1_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc,
        0xcf, 0xc0, 0x50, 0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn reads_local_image_files_and_reports_invalid_inputs() {
        let dir = std::env::temp_dir().join(format!("hn-studio-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let good = dir.join("good.png");
        let bad = dir.join("bad.png");
        let missing = dir.join("missing.png");
        std::fs::write(&good, VALID_1X1_PNG).expect("write good png");
        std::fs::write(&bad, b"plain text").expect("write bad file");

        let mut seq = 10u64;
        let files = vec![
            gio::File::for_path(&good),
            gio::File::for_path(&bad),
            gio::File::for_path(&missing),
            gio::File::for_uri("https://example.invalid/remote.png"),
        ];
        let (staged, first_err) = read_local_image_files(&files, || {
            seq += 1;
            seq
        });
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].filename, "good.png");
        assert!(first_err.is_some());
    }

    #[test]
    fn gtk_workspace_picker_syncs_searches_and_commits() {
        crate::with_gtk_test_thread(|| {
            let picker = WorkspacePicker::new();
            let chosen = Rc::new(RefCell::new(Vec::new()));
            let chosen_cap = Rc::clone(&chosen);
            picker.connect_selected(move |cwd| chosen_cap.borrow_mut().push(cwd));
            picker.connect_closed(|| {});

            assert!(!workspace_status_message(&WorkspaceCatalog::default(), true).is_empty());
            let catalog = parse_workspaces(
                &json!({
                    "default_cwd": "/home/alice/.hermes",
                    "home": "/home/alice",
                    "repos": [
                        {"root": "/home/alice/projects/hermes-native", "label": "hermes-native", "sessions": 3}
                    ]
                }),
                &[],
            )
            .expect("catalog");
            picker.sync(
                &catalog,
                Some("/home/alice/projects/hermes-native"),
                "main",
                true,
                true,
            );
            picker.rebuild_rows();
            assert_eq!(picker.results.n_items(), 2);
            picker.handle_arrow_key(gdk::Key::Down);
            picker.commit_selection(&|picked| chosen.borrow_mut().push(picked));
            assert_eq!(
                chosen.borrow().as_slice(),
                &[Some("/home/alice/projects/hermes-native".to_string())]
            );
            picker.search.set_text("zzzz-no-match");
            picker.rebuild_rows();
            assert_eq!(picker.results.n_items(), 0);
            picker.close();
        });
    }

    #[test]
    fn gtk_slash_deck_and_attachment_strip_lifecycle() {
        crate::with_gtk_test_thread(|| {
            let deck = SlashDeck::new();
            let picked = Rc::new(RefCell::new(Vec::new()));
            let picked_cap = Rc::clone(&picked);
            deck.connect_activated(move |cand| picked_cap.borrow_mut().push(cand.token));
            let skills = vec![SkillSummary {
                name: "b-plan".into(),
                description: "Plan".into(),
                category: "buck".into(),
                usage: 4,
            }];
            deck.sync("/b-", &skills, true);
            assert!(deck.is_open());
            assert!(deck.step(1));
            assert!(deck.selected_candidate().is_some());
            if let Some(btn) = deck.rows_box.first_child().and_downcast::<gtk::Button>() {
                btn.emit_clicked();
            }
            assert_eq!(picked.borrow().as_slice(), &["/b-plan".to_string()]);
            deck.dismiss("/b-");
            assert!(!deck.is_open());

            let strip = AttachmentStrip::new();
            let removed = Rc::new(RefCell::new(Vec::new()));
            let removed_cap = Rc::clone(&removed);
            strip.connect_remove(move |id| removed_cap.borrow_mut().push(id));
            let valid_img = StagedImage::from_bytes(1, "dot.png", VALID_1X1_PNG.to_vec()).unwrap();
            let fallback_img = StagedImage::from_bytes(2, "anim.gif", TINY_GIF.to_vec()).unwrap();
            strip.sync(&[valid_img, fallback_img], true);
            assert!(strip.root.is_visible());
            if let Some(pill) = strip.root.first_child()
                && let Some(btn) = pill.last_child().and_downcast::<gtk::Button>()
            {
                btn.emit_clicked();
            }
            assert_eq!(removed.borrow().as_slice(), &[1]);

            let composer = gtk::Box::new(gtk::Orientation::Vertical, 4);
            install_image_drop_target(&composer, |_| {});
        });
    }
}
