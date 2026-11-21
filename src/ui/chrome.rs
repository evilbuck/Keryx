//! GPUI surfaces for the studio, search, and timeline tools.
//!
//! The widgets are new. The decisions they trigger live on [`crate::desk::Desk`].

use gpui::{
    ClipboardItem, Context, FontWeight, KeyDownEvent, MouseButton, SharedString, div, prelude::*,
    px, rgb,
};

use crate::inspector;
use crate::model::{
    InspectorTab, PaletteAction, SessionFilter, SideQuestionStatus, build_outline,
    build_palette_entries, session_meta_line, source_badge,
};
use crate::recall::now_epoch_secs;
use crate::studio::{active_slash_prefix, rank_slash_candidates};

use super::{Focus, Root, text_button};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Sheet {
    #[default]
    None,
    Palette,
    Rename,
    Attach,
    Workspace,
}

pub fn header_tools(_root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(tool_button("find-chat", "Find", cx, |root, _, cx| {
            root.desk.open_find(None);
            root.pending_focus = Some(Focus::Find);
            cx.notify();
        }))
        .child(tool_button("palette", "Commands", cx, |root, _, cx| {
            root.toggle_palette();
            cx.notify();
        }))
        .child(tool_button("deck", "Flight Deck", cx, |root, _, cx| {
            root.desk.toggle_inspector();
            cx.notify();
        }))
        .child(tool_button("pin-chat", "Pin", cx, |root, _, cx| {
            root.desk.toggle_pin();
            cx.notify();
        }))
}

fn tool_button(
    id: &'static str,
    label: &'static str,
    cx: &mut Context<Root>,
    action: impl Fn(&mut Root, &gpui::MouseDownEvent, &mut Context<Root>) + 'static,
) -> gpui::Stateful<gpui::Div> {
    text_button(
        id,
        label,
        true,
        false,
        cx.listener(move |root, event: &gpui::MouseDownEvent, _, cx| action(root, event, cx)),
    )
}

pub fn filter_row(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let current = root.desk.state.session_filter;
    div()
        .flex()
        .gap(px(6.))
        .child(filter_chip(
            "filter-all",
            "All",
            current == SessionFilter::All,
            SessionFilter::All,
            cx,
        ))
        .child(filter_chip(
            "filter-chats",
            "Chats",
            current == SessionFilter::Chats,
            SessionFilter::Chats,
            cx,
        ))
        .child(filter_chip(
            "filter-auto",
            "Auto",
            current == SessionFilter::Auto,
            SessionFilter::Auto,
            cx,
        ))
        .child(filter_chip(
            "filter-pinned",
            "Pinned",
            current == SessionFilter::Pinned,
            SessionFilter::Pinned,
            cx,
        ))
}

fn filter_chip(
    id: &'static str,
    label: &'static str,
    active: bool,
    filter: SessionFilter,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    text_button(
        id,
        label,
        true,
        active,
        cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
            root.desk.set_filter(filter);
            cx.notify();
        }),
    )
}

pub fn session_search(root: &mut Root, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    let query = root.desk.state.session_query.clone();
    let label = if query.is_empty() {
        "Search all conversations".to_string()
    } else {
        query
    };
    div()
        .id("session-search")
        .px(px(8.))
        .py(px(6.))
        .rounded(px(8.))
        .bg(rgb(0x2c2c34))
        .text_size(px(12.))
        .text_color(rgb(0xc8c8d0))
        .track_focus(&root.session_focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, _: &gpui::MouseDownEvent, window, cx| {
                root.session_focus.focus(window);
                cx.notify();
            }),
        )
        .child(label)
}

pub fn session_caption(session: &crate::model::SessionSummary, read_only: bool) -> String {
    let mut line = session_meta_line(session, now_epoch_secs());
    let badge = source_badge(&session.source);
    if !badge.is_empty() {
        line = format!("{badge} · {line}");
    }
    if session.pinned {
        line = format!("★ {line}");
    }
    if read_only && !line.contains("read-only") {
        line = format!("{line} · read-only");
    }
    line
}

pub fn find_bar(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let find = &root.desk.state.transcript_find;
    let count = if find.matches.is_empty() {
        "No matches".to_string()
    } else {
        format!("{} / {}", find.active_pos + 1, find.matches.len())
    };
    let query = if find.query.is_empty() {
        "Find in transcript".to_string()
    } else {
        find.query.clone()
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .py(px(6.))
        .bg(rgb(0x24242c))
        .child(
            div()
                .id("find-query")
                .flex_1()
                .track_focus(&root.find_focus)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|root, _: &gpui::MouseDownEvent, window, cx| {
                        root.find_focus.focus(window);
                        cx.notify();
                    }),
                )
                .child(query),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(0x9a9aa3))
                .child(count),
        )
        .child(tool_button("find-prev", "Prev", cx, |root, _, cx| {
            root.desk.step_find(-1);
            cx.notify();
        }))
        .child(tool_button("find-next", "Next", cx, |root, _, cx| {
            root.desk.step_find(1);
            cx.notify();
        }))
        .child(tool_button("find-close", "Close", cx, |root, _, cx| {
            root.desk.close_find();
            cx.notify();
        }))
}

pub fn slash_row(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let mut row = div().flex().flex_col().gap(px(4.));
    for (index, candidate) in slash_candidates(root).into_iter().enumerate() {
        let token = candidate.token.clone();
        let insert = candidate.insert_text.clone();
        let execute = candidate.zero_arg_execute;
        let selected = index == root.slash_index;
        row = row.child(
            div()
                .id(SharedString::new(format!("slash-{token}")))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(8.))
                .bg(if selected {
                    rgb(0x2a3348)
                } else {
                    rgb(0x1c1c22)
                })
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                        root.accept_slash(&insert, execute);
                        cx.notify();
                    }),
                )
                .child(format!("{token}  {}", candidate.description)),
        );
    }
    row
}

pub fn slash_open(root: &Root) -> bool {
    !slash_candidates(root).is_empty()
}

fn slash_candidates(root: &Root) -> Vec<crate::studio::SlashCandidate> {
    if root.slash_hidden {
        return Vec::new();
    }
    let draft = root.desk.draft();
    let Some(prefix) = active_slash_prefix(&draft) else {
        return Vec::new();
    };
    let token = slash_token(prefix);
    rank_slash_candidates(
        &token,
        &root.desk.state.skills,
        crate::studio::SLASH_DECK_LIMIT,
    )
}

fn slash_token(prefix: &str) -> String {
    if prefix.is_empty() {
        "/".to_string()
    } else {
        format!("/{prefix}")
    }
}

pub fn attachment_row(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let mut row = div().flex().gap(px(6.)).flex_wrap();
    for item in root.desk.state.active_attachments().to_vec() {
        let id = item.id;
        row = row.child(text_button(
            format!("attach-{id}"),
            format!("{} · remove", item.filename),
            true,
            false,
            cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.remove_attachment(id);
                cx.notify();
            }),
        ));
    }
    row.child(tool_button("add-file", "Attach", cx, |root, _, cx| {
        root.open_sheet(Sheet::Attach);
        cx.notify();
    }))
}

pub fn workspace_button(root: &mut Root, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    let caption = root
        .desk
        .state
        .effective_active_cwd()
        .unwrap_or("workspace");
    text_button(
        "workspace",
        caption,
        true,
        false,
        cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
            root.open_sheet(Sheet::Workspace);
            cx.notify();
        }),
    )
}

pub fn inspector(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let tab = root.desk.state.inspector_tab;
    div()
        .w(px(320.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(12.))
        .bg(rgb(0x1e1e24))
        .border_l_1()
        .border_color(rgb(0x34343c))
        .child(inspector_tabs(tab, cx))
        .child(inspector_body(root, tab, cx).flex_1())
}

fn inspector_tabs(tab: InspectorTab, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .gap(px(6.))
        .child(tab_button(
            "tab-side",
            "Side-Ask",
            tab == InspectorTab::SideAsk,
            InspectorTab::SideAsk,
            cx,
        ))
        .child(tab_button(
            "tab-trace",
            "Trace",
            tab == InspectorTab::Trace,
            InspectorTab::Trace,
            cx,
        ))
        .child(tab_button(
            "tab-map",
            "Map",
            tab == InspectorTab::Outline,
            InspectorTab::Outline,
            cx,
        ))
        .child(tool_button("deck-close", "Close", cx, |root, _, cx| {
            root.desk.toggle_inspector();
            cx.notify();
        }))
}

fn tab_button(
    id: &'static str,
    label: &'static str,
    active: bool,
    tab: InspectorTab,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    text_button(
        id,
        label,
        true,
        active,
        cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
            root.desk.open_inspector(tab);
            cx.notify();
        }),
    )
}

fn inspector_body(root: &mut Root, tab: InspectorTab, cx: &mut Context<Root>) -> gpui::Div {
    match tab {
        InspectorTab::SideAsk => side_page(root, cx),
        InspectorTab::Trace => trace_page(root, cx),
        InspectorTab::Outline => map_page(root, cx),
    }
}

fn side_page(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let questions = root.desk.side_questions();
    let mut page = div().flex().flex_col().gap(px(8.));
    if questions.is_empty() {
        page = page.child("Ask a side question with /btw. It stays off the transcript.");
    }
    for item in questions {
        page = page.child(side_card(&item));
    }
    page.child(side_entry(root, cx))
}

fn side_card(item: &crate::model::SideQuestion) -> gpui::Div {
    let status = match item.status {
        SideQuestionStatus::Pending => "pending",
        SideQuestionStatus::Answered => "answered",
        SideQuestionStatus::Failed => "failed",
    };
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(8.))
        .rounded(px(8.))
        .bg(rgb(0x24242c))
        .child(format!("{status} · {}", item.question))
        .when(!item.answer.is_empty(), |card| {
            card.child(item.answer.clone())
        })
}

fn side_entry(root: &mut Root, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    let text = if root.side_text.is_empty() {
        "Side question".to_string()
    } else {
        root.side_text.clone()
    };
    div()
        .id("side-entry")
        .px(px(8.))
        .py(px(6.))
        .rounded(px(8.))
        .bg(rgb(0x2c2c34))
        .track_focus(&root.side_focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, _: &gpui::MouseDownEvent, window, cx| {
                root.side_focus.focus(window);
                cx.notify();
            }),
        )
        .child(text)
}

fn trace_page(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let compress = root.desk.can_compress();
    let telemetry = root.desk.telemetry();
    let reasoning = root.desk.reasoning_text();
    let tools = root.desk.tool_traces();
    let mut page = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(telemetry_block(&telemetry));
    if !reasoning.is_empty() {
        page = page.child(reasoning);
    }
    for tool in &tools {
        let (badge, _) = inspector::tool_badge_spec(tool);
        page = page.child(format!(
            "{badge} {} · {}",
            tool.name,
            inspector::tool_subtitle_text(tool)
        ));
    }
    page.child(text_button(
        "compress",
        "Compact context",
        compress,
        true,
        cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
            root.desk.compress_current();
            cx.notify();
        }),
    ))
}

fn telemetry_block(telemetry: &crate::model::SessionTelemetry) -> gpui::Div {
    let mut block = div().flex().flex_col().gap(px(2.));
    let detail = inspector::workspace_detail_line(telemetry);
    if !detail.is_empty() {
        block = block.child(detail);
    }
    for (label, value) in inspector::telemetry_metric_cells(telemetry) {
        block = block.child(format!("{label}: {value}"));
    }
    block
}

fn map_page(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let messages = root.desk.messages();
    let mut page = div().flex().flex_col().gap(px(6.));
    for entry in build_outline(&messages) {
        page = page.child(outline_row(entry, cx));
    }
    page
}

fn outline_row(entry: crate::model::OutlineEntry, cx: &mut Context<Root>) -> gpui::Div {
    let index = entry.message_index;
    let end = entry.end_message_index;
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(format!("#{} {}", entry.turn_number, entry.title))
        .child(entry.meta)
        .child(text_button(
            format!("jump-{index}"),
            "Jump",
            true,
            false,
            cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                root.jump_to = Some(index);
                cx.notify();
            }),
        ))
        .child(text_button(
            format!("fork-{end}"),
            "Branch",
            true,
            false,
            cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.branch_from(Some(end), None);
                cx.notify();
            }),
        ))
}

pub fn palette(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let entries = build_palette_entries(&root.desk.state, &root.sheet_text, now_epoch_secs());
    let mut list = div().flex().flex_col().gap(px(4.));
    for (index, entry) in entries.iter().take(12).enumerate() {
        let action = entry.action.clone();
        let selected = index == root.sheet_index;
        list = list.child(
            div()
                .id(SharedString::new(format!("palette-{index}")))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(8.))
                .bg(if selected {
                    rgb(0x2a3348)
                } else {
                    rgb(0x1c1c22)
                })
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                        root.activate_palette(action.clone(), cx);
                    }),
                )
                .child(entry.title.clone())
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(0x9a9aa3))
                        .child(entry.subtitle.clone()),
                ),
        );
    }
    sheet_frame(root, "Command palette", list, cx)
}

pub fn text_sheet(
    root: &mut Root,
    title: &str,
    placeholder: &str,
    cx: &mut Context<Root>,
) -> gpui::Div {
    let text = if root.sheet_text.is_empty() {
        placeholder.to_string()
    } else {
        root.sheet_text.clone()
    };
    let body = div()
        .id("sheet-field")
        .px(px(8.))
        .py(px(6.))
        .rounded(px(8.))
        .bg(rgb(0x2c2c34))
        .track_focus(&root.sheet_focus)
        .child(text);
    sheet_frame(root, title, body, cx)
}

fn sheet_frame(
    root: &mut Root,
    title: &str,
    body: impl IntoElement,
    cx: &mut Context<Root>,
) -> gpui::Div {
    let _ = root;
    div()
        .absolute()
        .top(px(80.))
        .left(px(280.))
        .w(px(460.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(12.))
        .rounded(px(12.))
        .bg(rgb(0x24242c))
        .border_1()
        .border_color(rgb(0x3a3a42))
        .child(div().font_weight(FontWeight(700.)).child(title.to_string()))
        .child(body)
        .child(tool_button("sheet-close", "Close", cx, |root, _, cx| {
            root.sheet = Sheet::None;
            cx.notify();
        }))
}

pub fn workspace_sheet(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let options = root.desk.state.workspaces.search(&root.sheet_text);
    let mut list = div().flex().flex_col().gap(px(4.));
    for (index, option_index) in options.iter().take(8).enumerate() {
        let Some(option) = root
            .desk
            .state
            .workspaces
            .options
            .get(*option_index)
            .cloned()
        else {
            continue;
        };
        let selected = index == root.sheet_index;
        let cwd = option.root.clone();
        list = list.child(
            div()
                .px(px(8.))
                .py(px(4.))
                .rounded(px(8.))
                .bg(if selected {
                    rgb(0x2a3348)
                } else {
                    rgb(0x1c1c22)
                })
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                        root.desk.choose_workspace(Some(cwd.clone()));
                        root.sheet = Sheet::None;
                        cx.notify();
                    }),
                )
                .child(option.label.clone()),
        );
    }
    sheet_frame(root, "Workspace", list, cx)
}

impl Root {
    pub(super) fn toggle_palette(&mut self) {
        if self.sheet == Sheet::Palette {
            self.sheet = Sheet::None;
            return;
        }
        self.open_sheet(Sheet::Palette);
    }

    pub(super) fn open_sheet(&mut self, sheet: Sheet) {
        self.sheet = sheet;
        self.sheet_text.clear();
        self.sheet_index = 0;
        self.pending_focus = Some(Focus::Sheet);
    }

    pub(super) fn global_chord(&mut self, event: &KeyDownEvent) -> bool {
        let mods = &event.keystroke.modifiers;
        if !mods.control || mods.alt {
            return false;
        }
        self.control_chord(event.keystroke.key.as_str(), mods.shift)
    }

    fn control_chord(&mut self, key: &str, shift: bool) -> bool {
        match key {
            "k" if !shift => self.toggle_palette(),
            "f" if shift => self.open_find_chord(),
            "p" if !shift => {
                self.desk.toggle_pin();
            }
            "i" if !shift => self.desk.toggle_inspector(),
            _ => return false,
        }
        true
    }

    fn open_find_chord(&mut self) {
        self.desk.open_find(None);
        self.pending_focus = Some(Focus::Find);
    }

    pub(super) fn try_recall(&mut self, key: &str, mods: &gpui::Modifiers) -> bool {
        if mods.control || mods.alt || mods.shift || mods.platform {
            return false;
        }
        let back = match key {
            "up" => true,
            "down" => false,
            _ => return false,
        };
        self.desk.recall(back)
    }

    pub(super) fn slash_key(&mut self, key: &str, mods: &gpui::Modifiers) -> bool {
        if !slash_open(self) || mods.control || mods.alt {
            return false;
        }
        let count = slash_candidates(self).len();
        match key {
            "up" => self.slash_index = self.slash_index.saturating_sub(1),
            "down" => self.slash_index = (self.slash_index + 1).min(count.saturating_sub(1)),
            "tab" => self.accept_selected_slash(false),
            "enter" if !mods.shift => self.accept_selected_slash(true),
            "escape" => self.slash_hidden = true,
            _ => return false,
        }
        true
    }

    fn accept_selected_slash(&mut self, execute: bool) {
        let Some(candidate) = slash_candidates(self).get(self.slash_index).cloned() else {
            return;
        };
        self.accept_slash(
            &candidate.insert_text,
            execute && candidate.zero_arg_execute,
        );
    }

    fn accept_slash(&mut self, insert: &str, execute: bool) {
        self.slash_hidden = false;
        self.slash_index = 0;
        if execute {
            self.desk.replace_draft(insert.trim().to_string());
            self.desk.submit();
            self.take_composer_follow();
            return;
        }
        self.desk.replace_draft(insert.to_string());
    }

    pub(super) fn on_sheet_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.sheet = Sheet::None;
            return true;
        }
        if self.sheet == Sheet::Palette {
            return self.palette_key(key, event, cx);
        }
        if self.sheet == Sheet::Workspace {
            return self.workspace_key(key, event);
        }
        self.field_key(key, event)
    }

    fn palette_key(&mut self, key: &str, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let count =
            build_palette_entries(&self.desk.state, &self.sheet_text, now_epoch_secs()).len();
        match key {
            "up" => {
                self.sheet_index = self.sheet_index.saturating_sub(1);
                true
            }
            "down" => {
                self.sheet_index = (self.sheet_index + 1).min(count.saturating_sub(1));
                true
            }
            "enter" => {
                self.activate_index(cx);
                true
            }
            "backspace" => pop_text(&mut self.sheet_text),
            _ => push_text(&mut self.sheet_text, event),
        }
    }

    fn workspace_key(&mut self, key: &str, event: &KeyDownEvent) -> bool {
        let count = self.desk.state.workspaces.search(&self.sheet_text).len();
        match key {
            "up" => {
                self.sheet_index = self.sheet_index.saturating_sub(1);
                true
            }
            "down" => {
                self.sheet_index = (self.sheet_index + 1).min(count.saturating_sub(1));
                true
            }
            "enter" => {
                self.commit_workspace();
                true
            }
            "backspace" => pop_text(&mut self.sheet_text),
            _ => push_text(&mut self.sheet_text, event),
        }
    }

    fn field_key(&mut self, key: &str, event: &KeyDownEvent) -> bool {
        if key == "enter" {
            self.commit_field();
            return true;
        }
        if key == "backspace" {
            return pop_text(&mut self.sheet_text);
        }
        push_text(&mut self.sheet_text, event)
    }

    fn activate_index(&mut self, cx: &mut Context<Self>) {
        let entries = build_palette_entries(&self.desk.state, &self.sheet_text, now_epoch_secs());
        let Some(action) = entries
            .get(self.sheet_index)
            .map(|entry| entry.action.clone())
        else {
            return;
        };
        self.activate_palette(action, cx);
    }

    fn activate_palette(&mut self, action: PaletteAction, cx: &mut Context<Self>) {
        match action {
            PaletteAction::CopyTranscript => self.copy_transcript(cx),
            PaletteAction::ExportTranscript => self.export_transcript(cx),
            PaletteAction::RenameCurrent => self.open_sheet(Sheet::Rename),
            PaletteAction::AttachImage => self.open_sheet(Sheet::Attach),
            PaletteAction::SwitchWorkspace => self.open_sheet(Sheet::Workspace),
            PaletteAction::SwitchModel => {
                self.sheet = Sheet::None;
                self.desk.open_picker();
                self.pending_focus = Some(Focus::Search);
            }
            PaletteAction::FocusFilter => {
                self.sheet = Sheet::None;
                self.pending_focus = Some(Focus::Session);
            }
            other => {
                self.desk.run_palette(other);
                self.sheet = Sheet::None;
            }
        }
    }

    fn copy_transcript(&mut self, cx: &mut Context<Self>) {
        if let Some((_, markdown)) = self.desk.transcript_export() {
            cx.write_to_clipboard(ClipboardItem::new_string(markdown));
            self.desk.state.notice = "Copied the conversation as Markdown.".into();
        }
        self.sheet = Sheet::None;
    }

    fn export_transcript(&mut self, cx: &mut Context<Self>) {
        let Some((name, markdown)) = self.desk.transcript_export() else {
            return;
        };
        let path = std::env::temp_dir().join(name);
        self.desk.state.notice = match std::fs::write(&path, &markdown) {
            Ok(()) => format!("Exported {}", path.display()),
            Err(error) => format!("Export failed: {error}"),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(markdown));
        self.sheet = Sheet::None;
    }

    fn commit_workspace(&mut self) {
        let options = self.desk.state.workspaces.search(&self.sheet_text);
        let chosen = options
            .get(self.sheet_index)
            .and_then(|index| self.desk.state.workspaces.options.get(*index))
            .map(|option| option.root.clone());
        self.desk.choose_workspace(chosen);
        self.sheet = Sheet::None;
    }

    fn commit_field(&mut self) {
        let text = self.sheet_text.clone();
        match self.sheet {
            Sheet::Rename => {
                self.desk.rename_to(text);
            }
            Sheet::Attach => {
                if let Err(error) = self.desk.stage_path(&text) {
                    self.desk.state.notice = error;
                }
            }
            _ => {}
        }
        self.sheet = Sheet::None;
    }

    pub(super) fn on_find_key(&mut self, event: &KeyDownEvent) -> bool {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.desk.close_find();
            return true;
        }
        if key == "enter" {
            let delta = if event.keystroke.modifiers.shift {
                -1
            } else {
                1
            };
            self.desk.step_find(delta);
            return true;
        }
        if key == "backspace" {
            let mut query = self.desk.state.transcript_find.query.clone();
            query.pop();
            self.desk.set_find_query(query);
            return true;
        }
        if let Some(text) = event.keystroke.key_char.as_deref() {
            let mut query = self.desk.state.transcript_find.query.clone();
            query.push_str(text);
            self.desk.set_find_query(query);
            return true;
        }
        false
    }

    pub(super) fn on_session_key(&mut self, event: &KeyDownEvent) -> bool {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.pending_focus = Some(Focus::Composer);
            return true;
        }
        if key == "backspace" {
            let mut query = self.desk.state.session_query.clone();
            query.pop();
            self.desk.set_session_query(query);
            return true;
        }
        if let Some(text) = event.keystroke.key_char.as_deref() {
            let mut query = self.desk.state.session_query.clone();
            query.push_str(text);
            self.desk.set_session_query(query);
            return true;
        }
        false
    }

    pub(super) fn on_side_key(&mut self, event: &KeyDownEvent) -> bool {
        let key = event.keystroke.key.as_str();
        if key == "enter" {
            let question = std::mem::take(&mut self.side_text);
            self.desk.ask_side(question);
            return true;
        }
        if key == "backspace" {
            self.side_text.pop();
            return true;
        }
        if let Some(text) = event.keystroke.key_char.as_deref() {
            self.side_text.push_str(text);
            return true;
        }
        false
    }
}

fn pop_text(text: &mut String) -> bool {
    text.pop();
    true
}

fn push_text(text: &mut String, event: &KeyDownEvent) -> bool {
    let Some(typed) = event.keystroke.key_char.as_deref() else {
        return false;
    };
    if event.keystroke.modifiers.control || event.keystroke.modifiers.alt {
        return false;
    }
    text.push_str(typed);
    true
}
