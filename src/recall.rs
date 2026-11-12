use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use serde_json::{Value, json};

use crate::model::{
    ChatMessage, SearchHit, SessionMutation, SessionSummary, SourceKind, TranscriptFind,
    classify_source, format_relative_age, session_meta_line, short_model_badge, source_badge,
};
use crate::palette::now_epoch_secs;

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

pub fn bind_sidebar_row(object: &glib::Object) {
    let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
    let value = item
        .item()
        .and_downcast::<glib::BoxedAnyObject>()
        .expect("sidebar item");
    let row_item = value.borrow::<SidebarRowItem>();
    let widget = match &*row_item {
        SidebarRowItem::Local(session) => build_local_session_row(session),
        SidebarRowItem::RemoteHit { hit, .. } => build_remote_hit_row(hit),
    };
    item.set_child(Some(&widget));
}

fn build_local_session_row(session: &SessionSummary) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class("session-row");
    if session.pinned {
        row.add_css_class("session-row-pinned");
    }
    let title = gtk::Label::new(Some(if session.title.is_empty() {
        "Untitled conversation"
    } else {
        &session.title
    }));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("session-title");
    row.append(&title);
    if !session.preview.is_empty() {
        let preview = gtk::Label::new(Some(&session.preview));
        preview.set_xalign(0.0);
        preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview.add_css_class("session-preview");
        row.append(&preview);
    }
    row.append(&build_local_meta_row(session));
    row.set_tooltip_text(Some(&local_session_tooltip(session)));
    row
}

fn build_local_meta_row(session: &SessionSummary) -> gtk::Box {
    let meta_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if session.pinned {
        let pin = gtk::Label::new(Some("★ PIN"));
        pin.add_css_class("source-badge");
        pin.add_css_class("pin-badge");
        meta_row.append(&pin);
    }
    let badge = gtk::Label::new(Some(&source_badge(&session.source)));
    badge.add_css_class("source-badge");
    badge.add_css_class(match classify_source(&session.source) {
        SourceKind::Automated => "source-auto",
        SourceKind::Interactive => "source-chat",
    });
    meta_row.append(&badge);
    if session.is_branch {
        let fork = gtk::Label::new(Some("⑂ FORK"));
        fork.add_css_class("source-badge");
        fork.add_css_class("fork-badge");
        meta_row.append(&fork);
    }
    let meta = gtk::Label::new(Some(&session_meta_line(session, now_epoch_secs())));
    meta.set_xalign(0.0);
    meta.set_hexpand(true);
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
    meta.add_css_class("session-meta");
    meta_row.append(&meta);
    meta_row
}

fn local_session_tooltip(session: &SessionSummary) -> String {
    match &session.read_only_reason {
        Some(reason) => format!("Conversation ID: {}\n{}", session.stored_id, reason),
        None => format!("Conversation ID: {}", session.stored_id),
    }
}

fn build_remote_hit_row(hit: &SearchHit) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class("session-row");
    row.add_css_class("session-row-fts");
    let title = gtk::Label::new(Some(&hit.title));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("session-title");
    row.append(&title);
    if !hit.snippet_markup.is_empty() {
        let snippet = gtk::Label::new(None);
        snippet.set_use_markup(true);
        snippet.set_markup(&hit.snippet_markup);
        snippet.set_xalign(0.0);
        snippet.set_ellipsize(gtk::pango::EllipsizeMode::End);
        snippet.add_css_class("session-preview");
        row.append(&snippet);
    }
    row.append(&build_remote_meta_row(hit));
    row.set_tooltip_text(Some(&format!(
        "Deep server match · ID: {}\nClick to open and jump to match",
        hit.stored_id
    )));
    row
}

fn build_remote_meta_row(hit: &SearchHit) -> gtk::Box {
    let meta_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let fts = gtk::Label::new(Some("FTS5"));
    fts.add_css_class("source-badge");
    fts.add_css_class("fts-badge");
    meta_row.append(&fts);
    if !hit.role.is_empty() {
        let role = gtk::Label::new(Some(&hit.role.to_uppercase()));
        role.add_css_class("source-badge");
        meta_row.append(&role);
    }
    let mut parts = vec![format!("{} msgs", hit.message_count)];
    let model = short_model_badge(&hit.model);
    if !model.is_empty() {
        parts.push(model);
    }
    if let Some(age) = format_relative_age(&hit.updated_at, now_epoch_secs()) {
        parts.push(age);
    }
    let meta = gtk::Label::new(Some(&parts.join(" · ")));
    meta.set_xalign(0.0);
    meta.set_hexpand(true);
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
    meta.add_css_class("session-meta");
    meta_row.append(&meta);
    meta_row
}

pub struct TranscriptFindBar {
    pub root: gtk::Box,
    pub entry: gtk::SearchEntry,
    pub status: gtk::Label,
    pub load_earlier: gtk::Button,
    pub prev_btn: gtk::Button,
    pub next_btn: gtk::Button,
    pub close_btn: gtk::Button,
    updating: Cell<bool>,
}

impl TranscriptFindBar {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("transcript-find-bar");
        root.set_margin_start(16);
        root.set_margin_end(16);
        root.set_margin_top(8);
        root.set_margin_bottom(4);
        root.set_visible(false);

        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some("Find in transcript… (Enter next · Shift+Enter prev)"));
        entry.set_hexpand(true);
        entry.add_css_class("transcript-find-entry");
        entry.update_property(&[gtk::accessible::Property::Label("Find in transcript")]);

        let status = gtk::Label::new(Some("Type to find"));
        status.add_css_class("find-status-badge");

        let load_earlier = gtk::Button::with_label("Search earlier pages");
        load_earlier.add_css_class("flat");
        load_earlier.add_css_class("lineage-btn");
        load_earlier.set_visible(false);

        let prev_btn = find_icon_button("go-up-symbolic", "Previous match (Shift+Enter)");
        let next_btn = find_icon_button("go-down-symbolic", "Next match (Enter)");
        let close_btn = find_icon_button("window-close-symbolic", "Close find bar (Esc)");

        root.append(&entry);
        root.append(&status);
        root.append(&load_earlier);
        root.append(&prev_btn);
        root.append(&next_btn);
        root.append(&close_btn);

        Rc::new(Self {
            root,
            entry,
            status,
            load_earlier,
            prev_btn,
            next_btn,
            close_btn,
            updating: Cell::new(false),
        })
    }

    pub fn sync(&self, find: &TranscriptFind, history_before: usize, history_loading: bool) {
        self.root.set_visible(find.open);
        if !find.open {
            return;
        }
        self.updating.set(true);
        if self.entry.text().as_str() != find.query {
            self.entry.set_text(&find.query);
            self.entry.set_position(-1);
        }
        self.updating.set(false);
        self.status.set_text(&find.status_label());
        let has_hits = !find.matches.is_empty();
        self.prev_btn.set_sensitive(has_hits);
        self.next_btn.set_sensitive(has_hits);
        let can_page = !find.query.trim().is_empty() && history_before > 0;
        self.load_earlier.set_visible(can_page);
        self.load_earlier.set_sensitive(!history_loading);
    }

    pub fn focus_entry(&self) {
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    pub fn connect(
        self: &Rc<Self>,
        on_query: impl Fn(String) + 'static,
        on_step: impl Fn(isize) + 'static,
        on_load_earlier: impl Fn() + 'static,
        on_close: impl Fn() + 'static,
    ) {
        let step_cb: Rc<dyn Fn(isize)> = Rc::new(on_step);
        let close_cb: Rc<dyn Fn()> = Rc::new(on_close);
        let weak = Rc::downgrade(self);
        self.entry.connect_search_changed(move |entry| {
            if let Some(bar) = weak.upgrade()
                && !bar.updating.get()
            {
                on_query(entry.text().to_string());
            }
        });
        wire_find_buttons(self, &step_cb, on_load_earlier, &close_cb);
        wire_find_keys(self, step_cb, close_cb);
    }
}

fn wire_find_buttons(
    bar: &TranscriptFindBar,
    step_cb: &Rc<dyn Fn(isize)>,
    on_load_earlier: impl Fn() + 'static,
    close_cb: &Rc<dyn Fn()>,
) {
    let prev = Rc::clone(step_cb);
    bar.prev_btn.connect_clicked(move |_| prev(-1));
    let next = Rc::clone(step_cb);
    bar.next_btn.connect_clicked(move |_| next(1));
    let enter = Rc::clone(step_cb);
    bar.entry.connect_activate(move |_| enter(1));
    let esc = Rc::clone(close_cb);
    bar.entry.connect_stop_search(move |_| esc());
    let close = Rc::clone(close_cb);
    bar.close_btn.connect_clicked(move |_| close());
    bar.load_earlier.connect_clicked(move |_| on_load_earlier());
}

fn wire_find_keys(bar: &TranscriptFindBar, step_cb: Rc<dyn Fn(isize)>, close_cb: Rc<dyn Fn()>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gdk::Key::Escape {
            close_cb();
            return glib::Propagation::Stop;
        }
        if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
            && modifiers.contains(gdk::ModifierType::SHIFT_MASK)
        {
            step_cb(-1);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    bar.entry.add_controller(keys);
}

fn find_icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let btn = gtk::Button::from_icon_name(icon);
    btn.add_css_class("flat");
    btn.set_tooltip_text(Some(tooltip));
    btn.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    btn
}

pub fn present_rename_dialog(
    window: &impl IsA<gtk::Widget>,
    current_title: &str,
    on_commit: impl Fn(String) + 'static,
) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let heading = gtk::Label::new(Some("Rename conversation"));
    heading.set_xalign(0.0);
    heading.add_css_class("inspector-section-title");
    root.append(&heading);

    let entry = gtk::Entry::new();
    entry.set_text(current_title.trim());
    entry.set_placeholder_text(Some("Conversation title"));
    entry.update_property(&[gtk::accessible::Property::Label("Conversation title")]);
    root.append(&entry);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("flat");
    let save = gtk::Button::with_label("Rename");
    save.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&save);
    root.append(&actions);

    let dialog = adw::Dialog::builder()
        .title("Rename Conversation")
        .content_width(420)
        .follows_content_size(true)
        .child(&root)
        .build();

    wire_rename_dialog(&dialog, &entry, &cancel, &save, on_commit);
    dialog.present(Some(window));
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn wire_rename_dialog(
    dialog: &adw::Dialog,
    entry: &gtk::Entry,
    cancel: &gtk::Button,
    save: &gtk::Button,
    on_commit: impl Fn(String) + 'static,
) {
    let commit: Rc<dyn Fn(String)> = Rc::new(on_commit);
    let dlg_cancel = dialog.clone();
    cancel.connect_clicked(move |_| {
        let _ = dlg_cancel.close();
    });
    let dlg_save = dialog.clone();
    let entry_save = entry.clone();
    let cb_save = Rc::clone(&commit);
    save.connect_clicked(move |_| submit_rename(&dlg_save, &entry_save, &cb_save));

    let dlg_enter = dialog.clone();
    let cb_enter = commit;
    entry.connect_activate(move |e| submit_rename(&dlg_enter, e, &cb_enter));
}

fn submit_rename(dialog: &adw::Dialog, entry: &gtk::Entry, commit: &Rc<dyn Fn(String)>) {
    let next = entry.text().trim().to_string();
    if !next.is_empty() {
        let _ = dialog.close();
        commit(next);
    }
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
