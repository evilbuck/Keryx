use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use crate::markdown;
use crate::model::{
    Conversation, InspectorTab, OutlineEntry, SessionTelemetry, SideQuestion, SideQuestionStatus,
    ToolState, ToolTrace, build_outline, format_compact_tokens,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectorAction {
    Close,
    SelectTab(InspectorTab),
    AskSide(String),
    InsertDraft(String),
    JumpToMessage(usize),
    BranchAt(usize),
}

type ActionCallback = Rc<RefCell<Option<Box<dyn Fn(InspectorAction)>>>>;

#[derive(Default, PartialEq, Eq)]
struct RenderStamp {
    session_id: String,
    inspector_revision: u64,
    writable: bool,
}

#[derive(Clone)]
pub struct InspectorPanel {
    pub root: gtk::Box,
    workspace_badge: gtk::Label,
    ask_tab: gtk::ToggleButton,
    trace_tab: gtk::ToggleButton,
    outline_tab: gtk::ToggleButton,
    stack: gtk::Stack,
    side_scroll: gtk::ScrolledWindow,
    side_list: gtk::Box,
    side_entry: gtk::Entry,
    side_send: gtk::Button,
    context_summary: gtk::Label,
    context_bar: gtk::ProgressBar,
    workspace_line: gtk::Label,
    metrics_grid: gtk::Grid,
    reasoning_label: gtk::Label,
    reasoning_copy: gtk::Button,
    tools_count: gtk::Label,
    tools_list: gtk::Box,
    outline_list: gtk::Box,
    callback: ActionCallback,
    stamp: Rc<RefCell<Option<RenderStamp>>>,
}

impl InspectorPanel {
    pub fn new() -> Self {
        let callback: ActionCallback = Rc::new(RefCell::new(None));
        let (header, workspace_badge) = build_header(&callback);
        let (tabs_row, ask_tab, trace_tab, outline_tab) = build_tabs(&callback);
        let (ask_page, side_scroll, side_list, side_entry, side_send) = build_ask_page(&callback);
        let (
            trace_page,
            context_summary,
            context_bar,
            workspace_line,
            metrics_grid,
            reasoning_label,
            reasoning_copy,
            tools_count,
            tools_list,
        ) = build_trace_page();
        let (outline_page, outline_list) = build_outline_page();

        let stack = gtk::Stack::builder()
            .vexpand(true)
            .hexpand(true)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();
        stack.add_named(&ask_page, Some("ask"));
        stack.add_named(&trace_page, Some("trace"));
        stack.add_named(&outline_page, Some("outline"));

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(320)
            .visible(false)
            .css_classes(["flight-deck"])
            .build();
        root.append(&header);
        root.append(&tabs_row);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&stack);

        Self {
            root,
            workspace_badge,
            ask_tab,
            trace_tab,
            outline_tab,
            stack,
            side_scroll,
            side_list,
            side_entry,
            side_send,
            context_summary,
            context_bar,
            workspace_line,
            metrics_grid,
            reasoning_label,
            reasoning_copy,
            tools_count,
            tools_list,
            outline_list,
            callback,
            stamp: Rc::new(RefCell::new(None)),
        }
    }

    pub fn connect(&self, handler: impl Fn(InspectorAction) + 'static) {
        *self.callback.borrow_mut() = Some(Box::new(handler));
    }

    pub fn focus_side_entry(&self) {
        let _ = self.side_entry.grab_focus();
    }

    pub fn sync(
        &self,
        open: bool,
        tab: InspectorTab,
        session_id: Option<&str>,
        conversation: Option<&Conversation>,
        writable: bool,
    ) {
        self.root.set_visible(open);
        if !open {
            return;
        }
        self.sync_active_tab(tab);
        self.sync_composer_sensitivity(writable);
        let next = RenderStamp {
            session_id: session_id.unwrap_or("").to_string(),
            inspector_revision: conversation.map_or(0, |c| c.inspector_revision),
            writable,
        };
        if self.stamp.borrow().as_ref() == Some(&next) {
            return;
        }
        *self.stamp.borrow_mut() = Some(next);
        self.render_contents(conversation);
    }

    fn sync_active_tab(&self, tab: InspectorTab) {
        let (ask, trace, outline, page) = match tab {
            InspectorTab::SideAsk => (true, false, false, "ask"),
            InspectorTab::Trace => (false, true, false, "trace"),
            InspectorTab::Outline => (false, false, true, "outline"),
        };
        if self.ask_tab.is_active() != ask {
            self.ask_tab.set_active(ask);
        }
        if self.trace_tab.is_active() != trace {
            self.trace_tab.set_active(trace);
        }
        if self.outline_tab.is_active() != outline {
            self.outline_tab.set_active(outline);
        }
        self.stack.set_visible_child_name(page);
    }

    fn sync_composer_sensitivity(&self, writable: bool) {
        self.side_entry.set_sensitive(writable);
        let has_text = !self.side_entry.text().trim().is_empty();
        self.side_send.set_sensitive(writable && has_text);
        self.side_entry.set_placeholder_text(Some(if writable {
            "Ask about this chat without saving to history…"
        } else {
            "Open a live conversation to ask side questions"
        }));
    }

    fn render_contents(&self, conversation: Option<&Conversation>) {
        let empty = Conversation::default();
        let chat = conversation.unwrap_or(&empty);
        let outline = build_outline(&chat.messages);
        self.update_tab_labels(chat, outline.len());
        self.update_workspace_header(&chat.telemetry);
        self.render_side_questions(&chat.side_questions);
        self.render_telemetry(&chat.telemetry);
        self.render_reasoning(&chat.reasoning);
        self.render_tools(&chat.tools);
        self.render_outline(&outline);
    }

    fn update_tab_labels(&self, chat: &Conversation, turns: usize) {
        self.ask_tab
            .set_label(&tab_badge_label("Side-Ask", chat.side_questions.len()));
        self.trace_tab
            .set_label(&tab_badge_label("Trace", chat.tools.len()));
        self.outline_tab.set_label(&tab_badge_label("Map", turns));
    }

    fn update_workspace_header(&self, telemetry: &SessionTelemetry) {
        let badge = workspace_short_badge(telemetry);
        self.workspace_badge.set_visible(!badge.is_empty());
        self.workspace_badge.set_label(&badge);
    }

    fn render_side_questions(&self, items: &[SideQuestion]) {
        clear_box(&self.side_list);
        if items.is_empty() {
            self.side_list
                .append(&build_side_empty_state(&self.side_entry));
            return;
        }
        for item in items {
            self.side_list
                .append(&build_side_card(item, &self.callback));
        }
        let adj = self.side_scroll.vadjustment();
        gtk::glib::idle_add_local_once(move || {
            adj.set_value(adj.upper() - adj.page_size());
        });
    }

    fn render_telemetry(&self, telemetry: &SessionTelemetry) {
        self.context_summary.set_label(&telemetry.context_summary());
        let frac = telemetry.context_fraction().unwrap_or(0.0);
        self.context_bar.set_fraction(frac);
        apply_meter_severity(&self.context_bar, frac);

        let ws = workspace_detail_line(telemetry);
        self.workspace_line.set_visible(!ws.is_empty());
        self.workspace_line.set_label(&ws);

        populate_metrics_grid(&self.metrics_grid, telemetry);
    }

    fn render_reasoning(&self, reasoning: &str) {
        let trimmed = reasoning.trim();
        let has_reasoning = !trimmed.is_empty();
        self.reasoning_copy.set_visible(has_reasoning);
        if has_reasoning {
            self.reasoning_label.remove_css_class("dim-label");
            self.reasoning_label.set_label(trimmed);
        } else {
            self.reasoning_label.add_css_class("dim-label");
            self.reasoning_label
                .set_label("No reasoning tokens streamed for the current turn.");
        }
    }

    fn render_tools(&self, tools: &[ToolTrace]) {
        self.tools_count.set_label(&tools.len().to_string());
        self.tools_count.set_visible(!tools.is_empty());
        clear_box(&self.tools_list);
        if tools.is_empty() {
            let empty = gtk::Label::builder()
                .label("Tool calls, durations, and risk flags appear here live.")
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label", "caption"])
                .build();
            self.tools_list.append(&empty);
            return;
        }
        for trace in tools.iter().rev() {
            self.tools_list.append(&build_tool_row(trace));
        }
    }

    fn render_outline(&self, entries: &[OutlineEntry]) {
        clear_box(&self.outline_list);
        if entries.is_empty() {
            let empty = gtk::Label::builder()
                .label("Prompts and code-block milestones appear here as the chat grows.")
                .xalign(0.0)
                .wrap(true)
                .margin_top(12)
                .margin_start(4)
                .margin_end(4)
                .css_classes(["dim-label", "caption"])
                .build();
            self.outline_list.append(&empty);
            return;
        }
        for entry in entries {
            self.outline_list
                .append(&build_outline_row(entry, &self.callback));
        }
    }
}

impl Default for InspectorPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn emit_action(cb: &ActionCallback, action: InspectorAction) {
    if let Some(handler) = cb.borrow().as_ref() {
        handler(action);
    }
}

fn build_header(cb: &ActionCallback) -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(8)
        .css_classes(["flight-deck-header"])
        .build();
    let kicker = gtk::Label::builder()
        .label("FLIGHT DECK")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["flight-deck-kicker"])
        .build();
    let workspace_badge = gtk::Label::builder()
        .visible(false)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(18)
        .css_classes(["flight-deck-branch"])
        .build();
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close Flight Deck (Ctrl+I)")
        .css_classes(["flat", "circular"])
        .build();
    let cb_close = cb.clone();
    close.connect_clicked(move |_| emit_action(&cb_close, InspectorAction::Close));
    row.append(&kicker);
    row.append(&workspace_badge);
    row.append(&close);
    (row, workspace_badge)
}

fn build_tabs(
    cb: &ActionCallback,
) -> (
    gtk::Box,
    gtk::ToggleButton,
    gtk::ToggleButton,
    gtk::ToggleButton,
) {
    let ask = gtk::ToggleButton::builder()
        .label("Side-Ask")
        .active(true)
        .hexpand(true)
        .tooltip_text("Ephemeral /btw snapshot questions (Alt+Enter)")
        .css_classes(["flight-tab-btn"])
        .build();
    let trace = gtk::ToggleButton::builder()
        .label("Trace")
        .group(&ask)
        .hexpand(true)
        .tooltip_text("Live reasoning stream, context window, and tool timeline")
        .css_classes(["flight-tab-btn"])
        .build();
    let outline = gtk::ToggleButton::builder()
        .label("Map")
        .group(&ask)
        .hexpand(true)
        .tooltip_text("Jump directly to any turn in the transcript")
        .css_classes(["flight-tab-btn"])
        .build();

    wire_tab_toggle(&ask, InspectorTab::SideAsk, cb);
    wire_tab_toggle(&trace, InspectorTab::Trace, cb);
    wire_tab_toggle(&outline, InspectorTab::Outline, cb);

    let bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(8)
        .css_classes(["linked", "flight-deck-tabs"])
        .build();
    bar.append(&ask);
    bar.append(&trace);
    bar.append(&outline);
    (bar, ask, trace, outline)
}

fn wire_tab_toggle(btn: &gtk::ToggleButton, tab: InspectorTab, cb: &ActionCallback) {
    let cb = cb.clone();
    btn.connect_toggled(move |b| {
        if b.is_active() {
            emit_action(&cb, InspectorAction::SelectTab(tab));
        }
    });
}

fn build_ask_page(
    cb: &ActionCallback,
) -> (
    gtk::Box,
    gtk::ScrolledWindow,
    gtk::Box,
    gtk::Entry,
    gtk::Button,
) {
    let explainer = gtk::Label::builder()
        .label("Snapshot side-channel · answers against live context without altering chat history or interrupting Hermes.")
        .wrap(true)
        .xalign(0.0)
        .margin_top(8)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["side-ask-explainer"])
        .build();

    let side_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(4)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&side_list)
        .build();

    let (bar, entry, send) = build_side_composer(cb);
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .build();
    page.append(&explainer);
    page.append(&scroll);
    page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    page.append(&bar);
    (page, scroll, side_list, entry, send)
}

fn build_side_composer(cb: &ActionCallback) -> (gtk::Box, gtk::Entry, gtk::Button) {
    let entry = gtk::Entry::builder()
        .hexpand(true)
        .placeholder_text("Ask about this chat without saving to history…")
        .css_classes(["side-ask-entry"])
        .build();
    let send = gtk::Button::builder()
        .label("Ask")
        .sensitive(false)
        .tooltip_text("Send ephemeral /btw side question")
        .css_classes(["suggested-action"])
        .build();

    let send_ref = send.clone();
    entry.connect_changed(move |e| {
        send_ref.set_sensitive(e.is_sensitive() && !e.text().trim().is_empty());
    });

    let cb_enter = cb.clone();
    entry.connect_activate(move |e| submit_side_entry(e, &cb_enter));

    let entry_click = entry.clone();
    let cb_click = cb.clone();
    send.connect_clicked(move |_| submit_side_entry(&entry_click, &cb_click));

    let bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_top(8)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["side-ask-bar"])
        .build();
    bar.append(&entry);
    bar.append(&send);
    (bar, entry, send)
}

fn submit_side_entry(entry: &gtk::Entry, cb: &ActionCallback) {
    let question = entry.text().trim().to_string();
    if question.is_empty() {
        return;
    }
    entry.set_text("");
    emit_action(cb, InspectorAction::AskSide(question));
}

fn build_side_empty_state(entry: &gtk::Entry) -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .css_classes(["inspector-card"])
        .build();
    let title = gtk::Label::builder()
        .label("Ask on the side (/btw)")
        .xalign(0.0)
        .css_classes(["inspector-section-title"])
        .build();
    let hint = gtk::Label::builder()
        .label("Need a quick reminder, definition, or sanity check while Hermes is working? Side questions read a snapshot of this session and leave server history untouched.\n\nShortcut: type /btw <question> or press Alt+Enter in the main composer.")
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "caption"])
        .build();
    card.append(&title);
    card.append(&hint);

    for prompt in [
        "Summarize the key decisions so far",
        "What constraints or ports did we set?",
    ] {
        let chip = gtk::Button::builder()
            .label(prompt)
            .halign(gtk::Align::Start)
            .css_classes(["flat", "side-suggestion-chip"])
            .build();
        let entry = entry.clone();
        chip.connect_clicked(move |_| {
            if entry.is_sensitive() {
                entry.set_text(prompt);
                entry.set_position(-1);
                let _ = entry.grab_focus();
            }
        });
        card.append(&chip);
    }
    card
}

fn build_side_card(item: &SideQuestion, cb: &ActionCallback) -> gtk::Box {
    let mut classes = vec!["side-card"];
    match item.status {
        SideQuestionStatus::Pending => classes.push("pending"),
        SideQuestionStatus::Answered => {}
        SideQuestionStatus::Failed => classes.push("failed"),
    }
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(classes)
        .build();

    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let question = gtk::Label::builder()
        .label(&item.question)
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .css_classes(["side-question-title"])
        .build();
    let (badge_text, badge_class) = match item.status {
        SideQuestionStatus::Pending => ("ASKING…", "side-badge-pending"),
        SideQuestionStatus::Answered => ("SNAPSHOT", "side-badge-ok"),
        SideQuestionStatus::Failed => ("ERROR", "side-badge-err"),
    };
    let badge = gtk::Label::builder()
        .label(badge_text)
        .valign(gtk::Align::Start)
        .css_classes(["side-status-badge", badge_class])
        .build();
    top.append(&question);
    top.append(&badge);
    card.append(&top);

    append_side_body(&card, item, cb);
    card
}

fn append_side_body(card: &gtk::Box, item: &SideQuestion, cb: &ActionCallback) {
    match item.status {
        SideQuestionStatus::Pending => {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            let spinner = adw::Spinner::new();
            let label = gtk::Label::builder()
                .label("Reading conversation snapshot…")
                .xalign(0.0)
                .css_classes(["dim-label", "caption"])
                .build();
            row.append(&spinner);
            row.append(&label);
            card.append(&row);
        }
        SideQuestionStatus::Failed => {
            let err = gtk::Label::builder()
                .label(&item.answer)
                .wrap(true)
                .xalign(0.0)
                .selectable(true)
                .css_classes(["side-error-text"])
                .build();
            card.append(&err);
        }
        SideQuestionStatus::Answered => {
            card.append(&markdown::render(&item.answer));
            card.append(&build_side_actions(&item.answer, cb));
        }
    }
}

fn build_side_actions(answer: &str, cb: &ActionCallback) -> gtk::Box {
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_top(2)
        .build();
    let copy_btn = gtk::Button::builder()
        .label("Copy")
        .css_classes(["flat", "side-action-btn"])
        .build();
    let copy_text = answer.to_string();
    copy_btn.connect_clicked(move |btn| {
        btn.clipboard().set_text(&copy_text);
        btn.set_label("Copied");
    });

    let promote_btn = gtk::Button::builder()
        .label("↗ Insert into draft")
        .tooltip_text("Append this answer into the main composer so you can act on it")
        .css_classes(["flat", "side-action-btn"])
        .build();
    let draft_text = answer.to_string();
    let cb = cb.clone();
    promote_btn.connect_clicked(move |_| {
        emit_action(&cb, InspectorAction::InsertDraft(draft_text.clone()));
    });

    actions.append(&copy_btn);
    actions.append(&promote_btn);
    actions
}

#[allow(clippy::type_complexity)]
fn build_trace_page() -> (
    gtk::ScrolledWindow,
    gtk::Label,
    gtk::ProgressBar,
    gtk::Label,
    gtk::Grid,
    gtk::Label,
    gtk::Button,
    gtk::Label,
    gtk::Box,
) {
    let (telemetry_card, context_summary, context_bar, workspace_line, metrics_grid) =
        build_telemetry_card();
    let (reasoning_card, reasoning_label, reasoning_copy) = build_reasoning_card();
    let (tools_card, tools_count, tools_list) = build_tools_card();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(10)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&telemetry_card);
    content.append(&reasoning_card);
    content.append(&tools_card);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();

    (
        scroll,
        context_summary,
        context_bar,
        workspace_line,
        metrics_grid,
        reasoning_label,
        reasoning_copy,
        tools_count,
        tools_list,
    )
}

fn build_telemetry_card() -> (
    gtk::Box,
    gtk::Label,
    gtk::ProgressBar,
    gtk::Label,
    gtk::Grid,
) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .css_classes(["inspector-card"])
        .build();
    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let title = gtk::Label::builder()
        .label("CONTEXT & TELEMETRY")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["inspector-kicker"])
        .build();
    let summary = gtk::Label::builder()
        .label("Waiting for first inference…")
        .xalign(1.0)
        .css_classes(["caption", "dim-label"])
        .build();
    top.append(&title);
    top.append(&summary);

    let bar = gtk::ProgressBar::builder()
        .css_classes(["context-meter"])
        .build();
    let workspace_line = gtk::Label::builder()
        .visible(false)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["workspace-meta-line"])
        .build();
    let grid = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(6)
        .column_homogeneous(true)
        .build();

    card.append(&top);
    card.append(&bar);
    card.append(&workspace_line);
    card.append(&grid);
    (card, summary, bar, workspace_line, grid)
}

fn build_reasoning_card() -> (gtk::Box, gtk::Label, gtk::Button) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["inspector-card"])
        .build();
    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let title = gtk::Label::builder()
        .label("LIVE REASONING")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["inspector-kicker"])
        .build();
    let copy_btn = gtk::Button::builder()
        .label("Copy")
        .visible(false)
        .css_classes(["flat", "side-action-btn"])
        .build();
    top.append(&title);
    top.append(&copy_btn);

    let label = gtk::Label::builder()
        .label("No reasoning tokens streamed for the current turn.")
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .css_classes(["reasoning-body", "dim-label"])
        .build();
    let label_ref = label.clone();
    copy_btn.connect_clicked(move |btn| {
        btn.clipboard().set_text(&label_ref.text());
        btn.set_label("Copied");
    });

    card.append(&top);
    card.append(&label);
    (card, label, copy_btn)
}

fn build_tools_card() -> (gtk::Box, gtk::Label, gtk::Box) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .css_classes(["inspector-card"])
        .build();
    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let title = gtk::Label::builder()
        .label("TOOL TIMELINE")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["inspector-kicker"])
        .build();
    let count = gtk::Label::builder()
        .visible(false)
        .css_classes(["side-status-badge", "side-badge-ok"])
        .build();
    top.append(&title);
    top.append(&count);

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    card.append(&top);
    card.append(&list);
    (card, count, list)
}

fn build_outline_page() -> (gtk::ScrolledWindow, gtk::Box) {
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(8)
        .margin_bottom(12)
        .margin_start(10)
        .margin_end(10)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    (scroll, list)
}

fn populate_metrics_grid(grid: &gtk::Grid, telemetry: &SessionTelemetry) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    let cells = telemetry_metric_cells(telemetry);
    for (idx, (label, value)) in cells.into_iter().enumerate() {
        let cell = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(1)
            .css_classes(["metric-chip"])
            .build();
        cell.append(
            &gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .css_classes(["metric-label"])
                .build(),
        );
        cell.append(
            &gtk::Label::builder()
                .label(&value)
                .xalign(0.0)
                .css_classes(["metric-value"])
                .build(),
        );
        grid.attach(&cell, (idx % 2) as i32, (idx / 2) as i32, 1, 1);
    }
}

pub fn telemetry_metric_cells(t: &SessionTelemetry) -> Vec<(&'static str, String)> {
    let speed = t
        .tokens_per_sec
        .map_or_else(|| "—".into(), |tps| format!("{tps:.0} tok/s"));
    let extra = if let Some(cost) = t.cost_usd.filter(|&c| c > 0.0) {
        ("Cost", format!("${cost:.3}"))
    } else {
        ("API Calls", t.api_calls.to_string())
    };
    vec![
        ("Input", format_compact_tokens(t.input_tokens)),
        ("Output", format_compact_tokens(t.output_tokens)),
        ("Cached", format_compact_tokens(t.cache_read_tokens)),
        ("Reasoning", format_compact_tokens(t.reasoning_tokens)),
        ("Speed", speed),
        extra,
    ]
}

fn apply_meter_severity(bar: &gtk::ProgressBar, fraction: f64) {
    bar.remove_css_class("warn");
    bar.remove_css_class("high");
    if fraction >= 0.90 {
        bar.add_css_class("high");
    } else if fraction >= 0.75 {
        bar.add_css_class("warn");
    }
}

fn build_tool_row(trace: &ToolTrace) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .css_classes(["tool-trace-row"])
        .build();
    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let name = gtk::Label::builder()
        .label(&trace.name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["tool-trace-name"])
        .build();
    let (badge_text, badge_class) = tool_badge_spec(trace);
    let badge = gtk::Label::builder()
        .label(&badge_text)
        .css_classes(["side-status-badge", badge_class])
        .build();
    top.append(&name);
    top.append(&badge);
    row.append(&top);

    let sub = tool_subtitle_text(trace);
    if !sub.is_empty() {
        row.append(
            &gtk::Label::builder()
                .label(&sub)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
    }
    row
}

pub fn tool_badge_spec(trace: &ToolTrace) -> (String, &'static str) {
    match &trace.state {
        ToolState::Running => {
            let text = trace
                .progress_pct
                .map_or_else(|| "RUN".into(), |pct| format!("RUN {pct}%"));
            (text, "side-badge-pending")
        }
        ToolState::Finished => {
            let text = trace
                .duration_s
                .map_or_else(|| "DONE".into(), |s| format!("{s:.1}s"));
            (text, "side-badge-ok")
        }
        ToolState::RiskFlagged(_) => ("RISK".into(), "side-badge-err"),
    }
}

fn tool_subtitle_text(trace: &ToolTrace) -> String {
    if let ToolState::RiskFlagged(risk) = &trace.state {
        if trace.detail.is_empty() {
            return format!("Flagged: {risk}");
        }
        return format!("Flagged ({risk}) · {}", trace.detail);
    }
    trace.detail.clone()
}

fn build_outline_row(entry: &OutlineEntry, cb: &ActionCallback) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    let num = gtk::Label::builder()
        .label(format!("#{}", entry.turn_number))
        .css_classes(["outline-turn-num"])
        .build();
    let title = gtk::Label::builder()
        .label(&entry.title)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["outline-turn-title"])
        .build();
    top.append(&num);
    top.append(&title);
    row.append(&top);
    row.append(
        &gtk::Label::builder()
            .label(&entry.meta)
            .xalign(0.0)
            .css_classes(["caption", "dim-label"])
            .build(),
    );

    let jump_btn = gtk::Button::builder()
        .child(&row)
        .hexpand(true)
        .css_classes(["flat", "outline-row"])
        .build();
    let target = entry.message_index;
    let cb_jump = cb.clone();
    jump_btn
        .connect_clicked(move |_| emit_action(&cb_jump, InspectorAction::JumpToMessage(target)));

    let fork_btn = gtk::Button::builder()
        .label("⑂")
        .valign(gtk::Align::Center)
        .tooltip_text("Branch conversation after this turn")
        .css_classes(["flat", "side-action-btn"])
        .build();
    let fork_target = entry.end_message_index;
    let cb_fork = cb.clone();
    fork_btn
        .connect_clicked(move |_| emit_action(&cb_fork, InspectorAction::BranchAt(fork_target)));

    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .build();
    container.append(&jump_btn);
    container.append(&fork_btn);
    container
}

pub fn tab_badge_label(base: &str, count: usize) -> String {
    if count == 0 {
        base.to_string()
    } else {
        format!("{base} · {count}")
    }
}

pub fn workspace_short_badge(t: &SessionTelemetry) -> String {
    if !t.branch.is_empty() {
        return format!("⎇ {}", t.branch);
    }
    t.cwd
        .rsplit('/')
        .find(|seg| !seg.is_empty())
        .map_or_else(String::new, str::to_string)
}

pub fn workspace_detail_line(t: &SessionTelemetry) -> String {
    match (t.branch.is_empty(), t.cwd.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("Branch: {}", t.branch),
        (true, false) => t.cwd.clone(),
        (false, false) => format!("⎇ {} · {}", t.branch, t.cwd),
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_inspector_badges_and_telemetry_cells_format_cleanly() {
        assert_eq!(tab_badge_label("Side-Ask", 0), "Side-Ask");
        assert_eq!(tab_badge_label("Side-Ask", 3), "Side-Ask · 3");

        let mut t = SessionTelemetry {
            input_tokens: 12_500,
            output_tokens: 900,
            cache_read_tokens: 10_000,
            reasoning_tokens: 400,
            tokens_per_sec: Some(78.4),
            api_calls: 4,
            cwd: "/srv/hermes/app".into(),
            branch: "main".into(),
            ..Default::default()
        };
        assert_eq!(workspace_short_badge(&t), "⎇ main");
        assert_eq!(workspace_detail_line(&t), "⎇ main · /srv/hermes/app");
        t.branch.clear();
        assert_eq!(workspace_short_badge(&t), "app");

        let cells = telemetry_metric_cells(&t);
        assert_eq!(cells.len(), 6);
        assert_eq!(cells[0], ("Input", "12.5k".into()));
        assert_eq!(cells[4], ("Speed", "78 tok/s".into()));
        assert_eq!(cells[5], ("API Calls", "4".into()));

        t.cost_usd = Some(0.125);
        let with_cost = telemetry_metric_cells(&t);
        assert_eq!(with_cost[5], ("Cost", "$0.125".into()));

        let running = ToolTrace {
            tool_id: "1".into(),
            name: "terminal".into(),
            detail: "ls".into(),
            progress_pct: Some(40),
            duration_s: None,
            state: ToolState::Running,
        };
        assert_eq!(tool_badge_spec(&running).0, "RUN 40%");
    }

    fn populated_conversation() -> Conversation {
        let mut conv = Conversation {
            messages: vec![
                crate::model::ChatMessage {
                    id: "m1".into(),
                    role: crate::model::Role::User,
                    text: "Design the Wayland Flight Deck".into(),
                },
                crate::model::ChatMessage {
                    id: "m2".into(),
                    role: crate::model::Role::Assistant,
                    text: "Here is the layout:\n```rust\nstruct Deck;\n```".into(),
                },
            ],
            ..Default::default()
        };
        conv.begin_side_question(1, "What port?".into());
        conv.begin_side_question(2, "Summarize".into());
        conv.complete_side_question("t2", "Summarize", "**Clean** architecture".into());
        conv.begin_side_question(3, "Broken?".into());
        conv.fail_side_question(3, "Gateway offline".into());
        conv.append_reasoning("Checking Wayland constraints...", false);
        conv.apply_tool_event(crate::model::ToolEventUpdate::Completed {
            tool_id: "c1".into(),
            name: "read_file".into(),
            duration_s: Some(0.3),
            summary: "src/ui.rs".into(),
        });
        conv.apply_tool_event(crate::model::ToolEventUpdate::Risk {
            tool_id: "c2".into(),
            name: "terminal".into(),
            risk: "secret".into(),
        });
        conv.apply_telemetry(SessionTelemetry {
            context_used: Some(92_000),
            context_max: Some(100_000),
            context_percent: Some(92),
            cwd: "/srv/hermes".into(),
            branch: "feat/deck".into(),
            ..Default::default()
        });
        conv
    }

    #[test]
    fn gtk_inspector_panel_renders_empty_and_populated_states() {
        if gtk::init().is_err() {
            return;
        }
        let _ = adw::init();
        let panel = InspectorPanel::default();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let captured = actions.clone();
        panel.connect(move |action| captured.borrow_mut().push(action));

        panel.sync(false, InspectorTab::SideAsk, None, None, false);
        assert!(!panel.root.is_visible());

        panel.sync(true, InspectorTab::SideAsk, Some("s1"), None, false);
        assert!(panel.root.is_visible());

        let conv = populated_conversation();
        panel.sync(true, InspectorTab::Trace, Some("s1"), Some(&conv), true);
        panel.sync(true, InspectorTab::Outline, Some("s1"), Some(&conv), true);
        assert_eq!(panel.ask_tab.label().as_deref(), Some("Side-Ask · 3"));
        assert_eq!(panel.trace_tab.label().as_deref(), Some("Trace · 2"));
        assert_eq!(panel.outline_tab.label().as_deref(), Some("Map · 1"));
    }
}
