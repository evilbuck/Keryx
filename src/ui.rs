//! GPUI window for Hermes Native.
//!
//! [`crate::desk::Desk`] owns the conversation. This module paints it and turns
//! keystrokes and clicks into desk intents. Tokio stays on the `hermes-net`
//! thread; a GPUI timer drains that thread's events onto the UI entity.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, ClipboardItem, Context, FocusHandle, FontWeight, KeyBinding,
    KeyDownEvent, MouseButton, ScrollDelta, ScrollHandle, ScrollWheelEvent, TitlebarOptions,
    Window, WindowBounds, WindowDecorations, WindowOptions, actions, div, prelude::*, px, rgb,
    size,
};

use crate::desk::Desk;
use crate::markdown::render_markdown;
use crate::model::{ChatMessage, Config, ConnectionState, InteractionKind, ModelState, Role};
use crate::present::{self, QuestionDraft, Tone};

actions!(hermes, [NewConversation]);

pub struct Launch {
    pub config: Result<Config, String>,
    pub preview_dir: Option<PathBuf>,
}

pub fn launch(options: Launch) {
    let config = options.config;
    let preview_dir = options.preview_dir;
    Application::new().run(move |cx| open_window(config, preview_dir, cx));
}

fn open_window(config: Result<Config, String>, preview_dir: Option<PathBuf>, cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-n", NewConversation, None)]);
    let bounds = Bounds::centered(None, size(px(1120.), px(800.)), cx);
    if cx
        .open_window(window_options(bounds), move |window, cx| {
            let root = cx.new(|cx| Root::new(config, preview_dir, cx));
            watch_close(window, &root, cx);
            root
        })
        .is_err()
    {
        eprintln!("Could not open the Hermes window.");
        cx.quit();
    }
    cx.activate(true);
}

fn window_options(bounds: Bounds<gpui::Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Hermes Native".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_min_size: Some(size(px(640.), px(480.))),
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some("io.github.hermes_native.Client".into()),
        focus: true,
        show: true,
        ..Default::default()
    }
}

fn watch_close(window: &mut Window, root: &gpui::Entity<Root>, cx: &mut App) {
    let root = root.clone();
    window.on_window_should_close(cx, move |_window, cx| {
        root.update(cx, |root, _cx| root.desk.shutdown());
        true
    });
}

#[derive(Clone, Copy)]
enum Focus {
    Composer,
    Search,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewStep {
    Sidebar,
    Transcript,
    Reply,
    Picker,
    Approval,
    Readonly,
    Done,
}

struct Preview {
    dir: PathBuf,
    step: PreviewStep,
    published: Option<String>,
    since: Instant,
    asked_create: bool,
    sent_reply: bool,
    sent_approval: bool,
    picked_notes: bool,
    picked_board: bool,
}

impl Preview {
    fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("ack"), "");
        let _ = std::fs::write(dir.join("phase"), "starting");
        Self {
            dir,
            step: PreviewStep::Sidebar,
            published: None,
            since: Instant::now(),
            asked_create: false,
            sent_reply: false,
            sent_approval: false,
            picked_notes: false,
            picked_board: false,
        }
    }
}

pub struct Root {
    desk: Desk,
    composer_focus: FocusHandle,
    search_focus: FocusHandle,
    clarify_focus: FocusHandle,
    answer_qid: Option<String>,
    transcript: ScrollHandle,
    cursor: usize,
    anchor: usize,
    seen_generation: u64,
    followed_revision: u64,
    pending_focus: Option<Focus>,
    polling: bool,
    focused_once: bool,
    spin: u8,
    preview: Option<Preview>,
}

impl Root {
    fn new(
        config: Result<Config, String>,
        preview_dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            desk: Desk::new(config),
            composer_focus: cx.focus_handle(),
            search_focus: cx.focus_handle(),
            clarify_focus: cx.focus_handle(),
            answer_qid: None,
            transcript: ScrollHandle::new(),
            cursor: 0,
            anchor: 0,
            seen_generation: 0,
            followed_revision: u64::MAX,
            pending_focus: None,
            polling: false,
            focused_once: false,
            spin: 0,
            preview: preview_dir.map(Preview::new),
        }
    }

    fn arm(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(40))
                .await;
            this.update(cx, |root, cx| root.tick(cx)).ok();
        })
        .detach();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let changed = self.desk.poll();
        let stepped = self.advance_preview();
        self.spin = self.spin.wrapping_add(1);
        if changed || stepped {
            cx.notify();
        }
        self.arm(cx);
    }

    fn prepare(&mut self, window: &mut Window) {
        self.sync_cursor();
        self.claim_focus(window);
        self.follow_transcript();
    }

    fn sync_cursor(&mut self) {
        if self.seen_generation == self.desk.draft_generation {
            return;
        }
        self.seen_generation = self.desk.draft_generation;
        let len = self.desk.draft().len();
        self.cursor = len;
        self.anchor = len;
    }

    fn claim_focus(&mut self, window: &mut Window) {
        if let Some(target) = self.pending_focus.take() {
            self.focus_target(target, window);
            return;
        }
        if !self.focused_once {
            self.focused_once = true;
            self.composer_focus.focus(window);
        }
    }

    fn focus_target(&self, target: Focus, window: &mut Window) {
        match target {
            Focus::Composer => self.composer_focus.focus(window),
            Focus::Search => self.search_focus.focus(window),
        }
    }

    fn follow_transcript(&mut self) {
        let revision = self.desk.selected_revision();
        if self.desk.follow_bottom && revision != self.followed_revision {
            self.transcript.scroll_to_bottom();
            self.followed_revision = revision;
        }
    }

    fn on_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.desk.create() {
            self.composer_focus.focus(window);
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && self.escape() {
            cx.notify();
            return;
        }
        if self.search_focus.is_focused(window) {
            self.on_search_key(event, cx);
        } else if self.clarify_focus.is_focused(window) {
            self.on_answer_key(event, cx);
        } else if self.composer_focus.is_focused(window) {
            self.on_composer_key(event, cx);
        }
    }

    fn escape(&mut self) -> bool {
        if self.desk.confirm.is_some() {
            self.desk.answer_confirm(false);
            return true;
        }
        if self.desk.picker_open {
            self.desk.close_picker();
            self.pending_focus = Some(Focus::Composer);
            return true;
        }
        false
    }

    fn on_search_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "enter" {
            self.desk.commit_picker();
            self.pending_focus = Some(Focus::Composer);
            cx.notify();
            return;
        }
        if key == "up" || key == "down" {
            self.desk.move_picker(if key == "up" { -1 } else { 1 });
            cx.notify();
            return;
        }
        if self.edit_search(
            key,
            event.keystroke.key_char.as_deref(),
            &event.keystroke.modifiers,
        ) {
            cx.notify();
        }
    }

    fn edit_search(&mut self, key: &str, typed: Option<&str>, mods: &gpui::Modifiers) -> bool {
        if mods.control || mods.alt || mods.platform {
            return false;
        }
        if key == "backspace" {
            let mut query = self.desk.picker_query.clone();
            query.pop();
            self.desk.set_picker_query(query);
            return true;
        }
        if let Some(text) = typed {
            let mut query = self.desk.picker_query.clone();
            query.push_str(text);
            self.desk.set_picker_query(query);
            return true;
        }
        false
    }

    fn on_answer_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(qid) = self.answer_qid.clone() else {
            return;
        };
        let Some(next) = answer_edit(
            &self.question_text(&qid),
            event.keystroke.key.as_str(),
            event.keystroke.key_char.as_deref(),
            &event.keystroke.modifiers,
        ) else {
            return;
        };
        self.desk.set_question_text(&qid, next);
        cx.notify();
    }

    fn question_text(&self, qid: &str) -> String {
        self.desk
            .questions
            .iter()
            .find(|question| question.qid == qid)
            .map(|question| question.text.clone())
            .unwrap_or_default()
    }

    fn on_composer_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;
        if self.composer_command(key, mods, cx) {
            cx.notify();
            return;
        }
        if mods.control || mods.alt || mods.platform {
            return;
        }
        if key == "enter" {
            self.enter_key(mods.shift);
            cx.notify();
            return;
        }
        if let Some(text) = event.keystroke.key_char.as_deref() {
            self.insert_text(text);
            cx.notify();
        }
    }

    fn composer_command(
        &mut self,
        key: &str,
        mods: &gpui::Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.move_caret(key, mods.shift) {
            return true;
        }
        if self.edit_key(key) {
            return true;
        }
        self.chord(key, mods, cx)
    }

    fn move_caret(&mut self, key: &str, shift: bool) -> bool {
        let draft = self.desk.draft();
        let Some((anchor, cursor)) = caret_move(&draft, self.cursor, self.anchor, key, shift)
        else {
            return false;
        };
        self.anchor = anchor;
        self.cursor = cursor;
        true
    }

    fn edit_key(&mut self, key: &str) -> bool {
        match key {
            "backspace" => self.backspace(),
            "delete" => self.delete_forward(),
            _ => return false,
        }
        true
    }

    fn chord(&mut self, key: &str, mods: &gpui::Modifiers, cx: &mut Context<Self>) -> bool {
        if !mods.control {
            return false;
        }
        match key {
            "a" => self.select_all(),
            "c" => self.copy_selection(cx),
            "x" => self.cut_selection(cx),
            "v" => self.paste(cx),
            _ => return false,
        }
        true
    }

    fn enter_key(&mut self, shift: bool) {
        if shift {
            self.insert_text("\n");
        } else {
            self.desk.submit();
        }
    }

    fn insert_text(&mut self, extra: &str) {
        if !self.desk.controls().editable {
            return;
        }
        let draft = self.desk.draft();
        let (start, end) = selection_of(&draft, self.anchor, self.cursor);
        self.replace(&draft, start, end, extra);
    }

    fn backspace(&mut self) {
        let draft = self.desk.draft();
        let (start, end) = selection_of(&draft, self.anchor, self.cursor);
        if start != end {
            self.replace(&draft, start, end, "");
            return;
        }
        let prev = prev_boundary(&draft, start);
        self.replace(&draft, prev, start, "");
    }

    fn delete_forward(&mut self) {
        let draft = self.desk.draft();
        let (start, end) = selection_of(&draft, self.anchor, self.cursor);
        if start != end {
            self.replace(&draft, start, end, "");
            return;
        }
        let next = next_boundary(&draft, start);
        self.replace(&draft, start, next, "");
    }

    fn replace(&mut self, draft: &str, start: usize, end: usize, extra: &str) {
        let mut next = String::new();
        next.push_str(&draft[..start]);
        next.push_str(extra);
        next.push_str(&draft[end..]);
        self.cursor = start + extra.len();
        self.anchor = self.cursor;
        self.desk.set_draft(next);
    }

    fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.desk.draft().len();
    }

    fn copy_selection(&self, cx: &mut Context<Self>) {
        let draft = self.desk.draft();
        let (start, end) = selection_of(&draft, self.anchor, self.cursor);
        if start == end {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(draft[start..end].to_string()));
    }

    fn cut_selection(&mut self, cx: &mut Context<Self>) {
        self.copy_selection(cx);
        self.insert_text("");
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.insert_text(&text);
    }
}

impl gpui::Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.polling {
            self.polling = true;
            self.arm(cx);
        }
        self.prepare(window);
        let controls = self.desk.controls();
        let connection = self.desk.connection();
        shell(self, &controls, &connection, cx)
    }
}

fn shell(
    root: &mut Root,
    controls: &present::Controls,
    connection: &present::ConnectionView,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("hermes-root")
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(0x16161a))
        .text_color(rgb(0xeeeeee))
        .font_family("sans")
        .on_action(cx.listener(|root, _: &NewConversation, window, cx| root.on_new(window, cx)))
        .on_key_down(
            cx.listener(|root, event: &KeyDownEvent, window, cx| root.on_key(event, window, cx)),
        )
        .child(header(root, controls, connection, cx))
        .child(body(root, controls, cx))
        .children(overlay(root, cx))
}

fn header(
    root: &mut Root,
    controls: &present::Controls,
    connection: &present::ConnectionView,
    cx: &mut Context<Root>,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(16.))
        .h(px(52.))
        .bg(rgb(0x1e1e24))
        .border_b_1()
        .border_color(rgb(0x34343c))
        .child(header_start(root, controls, cx))
        .child(connection_label(connection, controls.reconnect, cx))
}

fn header_start(
    _root: &mut Root,
    controls: &present::Controls,
    cx: &mut Context<Root>,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(text_button(
            "toggle-sidebar",
            "Conversations",
            true,
            false,
            cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.toggle_sidebar();
                cx.notify();
            }),
        ))
        .child(
            div()
                .text_size(px(16.))
                .font_weight(FontWeight(700.))
                .child(controls.title.clone()),
        )
}

fn connection_label(
    connection: &present::ConnectionView,
    reconnect: bool,
    cx: &mut Context<Root>,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(tone_color(connection.tone))
                .child(connection.label),
        )
        .child(text_button(
            "reconnect",
            "Reconnect",
            reconnect,
            false,
            cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.reconnect();
                cx.notify();
            }),
        ))
}

fn tone_color(tone: Tone) -> gpui::Rgba {
    match tone {
        Tone::Live => rgb(0x3dba7a),
        Tone::Error => rgb(0xe05a5a),
        Tone::Neutral => rgb(0xb0b0b8),
    }
}

fn body(root: &mut Root, controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .when(root.desk.sidebar_open, |row| {
            row.child(sidebar(root, controls, cx))
        })
        .child(main_column(root, controls, cx).flex_1().min_w(px(0.)))
}

fn sidebar(root: &mut Root, controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .w(px(280.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(16.))
        .bg(rgb(0x1e1e24))
        .border_r_1()
        .border_color(rgb(0x34343c))
        .child(brand())
        .child(text_button(
            "new-chat",
            "New conversation",
            controls.new_chat,
            true,
            cx.listener(|root, _: &gpui::MouseDownEvent, window, cx| root.on_new(window, cx)),
        ))
        .child(session_scroll(root, cx).flex_1())
        .when(controls.more_visible, |column| {
            column.child(text_button(
                "more-sessions",
                "Load more conversations",
                controls.more_enabled,
                false,
                cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                    root.desk.load_more();
                    cx.notify();
                }),
            ))
        })
}

fn brand() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(18.))
                .font_weight(FontWeight(800.))
                .child("HERMES"),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(0x9a9aa3))
                .child("REMOTE CONVERSATIONS"),
        )
}

fn session_scroll(root: &mut Root, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    div()
        .id("sessions")
        .flex_1()
        .overflow_y_scroll()
        .child(session_list(root, cx))
}

fn session_list(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let selected = root.desk.state.selected.clone();
    let mut list = div().flex().flex_col().gap(px(6.));
    for session in root.desk.state.sessions.clone() {
        let id = session.stored_id.clone();
        let active = selected.as_deref() == Some(id.as_str());
        let read_only = session_read_only(&root.desk, &id);
        list = list.child(session_row(
            &session,
            active,
            read_only,
            cx.listener(move |root, _: &gpui::MouseDownEvent, window, cx| {
                root.desk.select(&id);
                root.composer_focus.focus(window);
                cx.notify();
            }),
        ));
    }
    list
}

fn session_read_only(desk: &Desk, id: &str) -> bool {
    desk.state
        .sessions
        .iter()
        .find(|session| session.stored_id == id)
        .and_then(|session| session.read_only_reason.as_ref())
        .is_some()
        || desk
            .state
            .conversations
            .get(id)
            .and_then(|chat| chat.read_only_reason.as_ref())
            .is_some()
}

fn session_row(
    session: &crate::model::SessionSummary,
    active: bool,
    read_only: bool,
    handler: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let mut row = div()
        .id(gpui::SharedString::new(format!(
            "session-{}",
            session.stored_id
        )))
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(12.))
        .rounded(px(10.))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, handler)
        .child(
            div()
                .font_weight(FontWeight(600.))
                .child(present::session_heading(&session.title)),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(0x9a9aa3))
                .child(present::session_meta(&session.source, read_only)),
        );
    if active {
        row = row.bg(rgb(0x2a3348));
    }
    row
}

fn main_column(root: &mut Root, controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .h_full()
        .min_h(px(0.))
        .bg(rgb(0x16161a))
        .child(notice(&root.desk.state.notice))
        .child(readonly_banner(controls))
        .child(history_bar(controls, cx))
        .child(transcript(root, controls, cx).flex_1())
        .child(footer(root, controls, cx))
}

fn notice(text: &str) -> gpui::Div {
    let mut banner = div()
        .px(px(20.))
        .text_size(px(13.))
        .text_color(rgb(0xf0c674));
    if !text.is_empty() {
        banner = banner.py(px(8.)).child(text.to_string());
    }
    banner
}

fn readonly_banner(controls: &present::Controls) -> gpui::Div {
    let mut banner = div().px(px(20.));
    if let Some(reason) = &controls.readonly {
        banner = banner.py(px(8.)).child(
            div()
                .p(px(12.))
                .rounded(px(9.))
                .bg(rgb(0x3a321c))
                .text_color(rgb(0xf0c674))
                .child(reason.clone()),
        );
    }
    banner
}

fn history_bar(controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .justify_center()
        .gap(px(8.))
        .py(px(6.))
        .when(controls.older_visible, |bar| {
            bar.child(text_button(
                "older",
                "Load earlier messages",
                controls.older_enabled,
                false,
                cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                    root.desk.load_older();
                    cx.notify();
                }),
            ))
        })
        .when(controls.latest_visible, |bar| {
            bar.child(text_button(
                "latest",
                "Reload history",
                controls.latest_enabled,
                false,
                cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                    root.desk.reload();
                    cx.notify();
                }),
            ))
        })
}

fn transcript(
    root: &mut Root,
    controls: &present::Controls,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("transcript")
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .track_scroll(&root.transcript)
        .on_scroll_wheel(cx.listener(|root, event: &ScrollWheelEvent, _, _| {
            if wheel_up(&event.delta) {
                root.desk.follow_bottom = false;
            }
        }))
        .child(transcript_body(root, controls, cx))
}

fn transcript_body(
    root: &mut Root,
    controls: &present::Controls,
    cx: &mut Context<Root>,
) -> gpui::Div {
    if controls.show_messages {
        message_column(root, cx)
    } else {
        empty_state()
    }
}

fn empty_state() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .h_full()
        .gap(px(8.))
        .p(px(24.))
        .child(
            div()
                .text_size(px(26.))
                .font_weight(FontWeight(700.))
                .child("A little space to think."),
        )
        .child(
            div()
                .text_color(rgb(0x9a9aa3))
                .child("Choose a conversation, or start something new."),
        )
        .child(
            div()
                .text_color(rgb(0x9a9aa3))
                .child("Your history stays on your Hermes server."),
        )
}

fn message_column(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let messages = root
        .desk
        .state
        .selected
        .as_ref()
        .and_then(|id| root.desk.state.conversations.get(id))
        .map(|chat| chat.messages.clone())
        .unwrap_or_default();
    let mut column = div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(24.))
        .w_full()
        .max_w(px(860.))
        .mx_auto();
    for message in messages {
        column = column.child(message_card(&message, cx));
    }
    column
}

fn message_card(message: &ChatMessage, cx: &mut Context<Root>) -> gpui::Div {
    let text = message.text.clone();
    let id = message.id.clone();
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(14.))
        .rounded(px(12.))
        .bg(message_bg(&message.role))
        .child(message_heading(&message.role, id, text, cx))
        .child(message_body(message))
}

fn message_bg(role: &Role) -> gpui::Rgba {
    match role {
        Role::User => rgb(0x1d2a44),
        Role::Assistant => rgb(0x222228),
        Role::Tool => rgb(0x16161a),
        Role::System => rgb(0x3a321c),
    }
}

fn message_heading(role: &Role, id: String, text: String, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight(800.))
                .text_color(role_color(role))
                .child(present::role_label(role)),
        )
        .child(text_button(
            format!("copy-{id}"),
            "Copy",
            true,
            false,
            cx.listener(move |_root, _: &gpui::MouseDownEvent, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            }),
        ))
}

fn role_color(role: &Role) -> gpui::Rgba {
    match role {
        Role::User => rgb(0x7ab0ff),
        Role::Assistant => rgb(0x3dba7a),
        Role::Tool => rgb(0xe0a23a),
        Role::System => rgb(0xf0c674),
    }
}

fn message_body(message: &ChatMessage) -> gpui::AnyElement {
    if message.role == Role::Tool {
        div()
            .font_family("monospace")
            .text_size(px(12.))
            .text_color(rgb(0xe0a23a))
            .child(message.text.clone())
            .into_any_element()
    } else {
        render_markdown(&message.text, &format!("md-{}", message.id))
    }
}

fn footer(root: &mut Root, controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(16.))
        .w_full()
        .max_w(px(900.))
        .mx_auto()
        .child(interaction(root, cx))
        .child(activity_row(controls, root.spin))
        .child(composer(root, controls, cx))
}

fn activity_row(controls: &present::Controls, tick: u8) -> gpui::Div {
    div()
        .flex()
        .gap(px(8.))
        .text_size(px(12.))
        .text_color(rgb(0xb0b0b8))
        .child(spin_mark(controls.spinning, tick))
        .child(controls.activity.clone())
}

fn spin_mark(spinning: bool, tick: u8) -> &'static str {
    if !spinning {
        return "";
    }
    match tick % 3 {
        0 => "·",
        1 => "•",
        _ => "●",
    }
}

fn interaction(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    root.desk.sync_questions();
    let Some(kind) = interaction_kind(&root.desk) else {
        return div();
    };
    let mut panel = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(12.))
        .rounded(px(10.))
        .bg(rgb(0x243044));
    if let Some(prompt) = interaction_prompt(&root.desk) {
        panel = panel.child(prompt);
    }
    panel = match kind {
        InteractionKind::Approval => panel.child(approval_buttons(&root.desk, cx)),
        InteractionKind::Clarification => panel.child(clarify_panel(root, cx)),
    };
    if !root.desk.form_note.is_empty() {
        panel = panel.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(0xf0c674))
                .child(root.desk.form_note.clone()),
        );
    }
    panel
}

fn interaction_kind(desk: &Desk) -> Option<InteractionKind> {
    open_interaction(desk).map(|request| request.kind.clone())
}

fn interaction_prompt(desk: &Desk) -> Option<String> {
    open_interaction(desk)
        .map(|request| request.prompt.clone())
        .filter(|prompt| !prompt.is_empty())
}

fn open_interaction(desk: &Desk) -> Option<&crate::model::InteractionRequest> {
    desk.state
        .selected
        .as_ref()
        .and_then(|id| desk.state.conversations.get(id))
        .and_then(|chat| chat.interaction.as_ref())
}

fn approval_buttons(desk: &Desk, cx: &mut Context<Root>) -> gpui::Div {
    let choices = open_interaction(desk)
        .map(present::approval_choices)
        .unwrap_or_default();
    let mut row = div().flex().gap(px(8.)).flex_wrap();
    for choice in choices {
        let label = present::approval_label(&choice).to_string();
        let primary = choice != "deny";
        row = row.child(text_button(
            format!("approve-{choice}"),
            label,
            true,
            primary,
            cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.approve(&choice);
                cx.notify();
            }),
        ));
    }
    row
}

fn clarify_panel(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let questions = root.desk.questions.clone();
    let mut column = div().flex().flex_col().gap(px(8.));
    for question in questions {
        column = column.child(question_block(root, &question, cx));
    }
    column = column.child(clarify_actions(cx));
    column
}

fn question_block(root: &mut Root, question: &QuestionDraft, cx: &mut Context<Root>) -> gpui::Div {
    let mut block = div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(question.prompt.clone());
    if let Some(locked) = &question.locked {
        block = block.child(locked.clone().unwrap_or_else(|| "Skipped".into()));
        return block;
    }
    block = block.child(question_choices(root, question, cx));
    let qid = question.qid.clone();
    block.child(text_button(
        format!("skip-{qid}"),
        if question.skipped {
            "Answer instead"
        } else {
            "Skip"
        },
        true,
        false,
        cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
            root.desk.toggle_skip(&qid);
            cx.notify();
        }),
    ))
}

fn question_choices(
    root: &mut Root,
    question: &QuestionDraft,
    cx: &mut Context<Root>,
) -> gpui::AnyElement {
    if question.choices.is_empty() {
        return free_answer(root, question, cx).into_any_element();
    }
    let mut row = div().flex().flex_col().gap(px(4.));
    for (index, choice) in question.choices.iter().enumerate() {
        let qid = question.qid.clone();
        let mark = if question.selected.get(index).copied().unwrap_or(false) {
            "[x] "
        } else {
            "[ ] "
        };
        row = row.child(text_button(
            format!("choice-{}-{index}", question.qid),
            format!("{mark}{choice}"),
            !question.skipped,
            false,
            cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.toggle_choice(&qid, index);
                cx.notify();
            }),
        ));
    }
    row.into_any_element()
}

fn free_answer(
    root: &mut Root,
    question: &QuestionDraft,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let qid = question.qid.clone();
    let shown = if question.skipped {
        "Skipped".to_string()
    } else if question.text.is_empty() {
        "Your answer".to_string()
    } else {
        question.text.clone()
    };
    div()
        .id(gpui::SharedString::new(format!("answer-{qid}")))
        .p(px(8.))
        .rounded(px(8.))
        .bg(rgb(0x1c1c22))
        .text_color(if question.text.is_empty() {
            rgb(0x8a8a92)
        } else {
            rgb(0xeeeeee)
        })
        .track_focus(&root.clarify_focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |root, _: &gpui::MouseDownEvent, window, cx| {
                root.answer_qid = Some(qid.clone());
                root.clarify_focus.focus(window);
                cx.notify();
            }),
        )
        .child(shown)
}

fn answer_edit(
    current: &str,
    key: &str,
    typed: Option<&str>,
    mods: &gpui::Modifiers,
) -> Option<String> {
    if mods.control || mods.alt || mods.platform {
        return None;
    }
    let mut next = current.to_string();
    if key == "backspace" {
        next.pop();
        return Some(next);
    }
    let text = typed?;
    next.push_str(text);
    Some(next)
}

fn clarify_actions(cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .gap(px(8.))
        .child(text_button(
            "submit-answers",
            "Submit answers",
            true,
            true,
            cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.submit_questions();
                cx.notify();
            }),
        ))
        .child(text_button(
            "skip-all",
            "Skip all",
            true,
            false,
            cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.cancel_questions();
                cx.notify();
            }),
        ))
}

fn composer(root: &mut Root, controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(10.))
        .rounded(px(14.))
        .border_1()
        .border_color(rgb(0x3a3a42))
        .bg(rgb(0x1c1c22))
        .child(composer_input(root, controls, cx))
        .child(model_trigger(root, controls, cx))
        .child(composer_actions(controls, cx))
}

fn composer_input(
    root: &mut Root,
    controls: &present::Controls,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("composer")
        .min_h(px(60.))
        .track_focus(&root.composer_focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, _: &gpui::MouseDownEvent, window, cx| {
                root.composer_focus.focus(window);
                cx.notify();
            }),
        )
        .child(composer_text(root, controls.readonly.is_some()))
}

fn composer_text(root: &Root, read_only: bool) -> gpui::Div {
    let draft = root.desk.draft();
    if draft.is_empty() {
        return div().text_color(rgb(0x8a8a92)).child(if read_only {
            "Read only"
        } else {
            "Message"
        });
    }
    let (start, end) = selection_of(&draft, root.anchor, root.cursor);
    if start != end {
        return selected_text(&draft, start, end);
    }
    let cursor = floor_boundary(&draft, root.cursor.min(draft.len()));
    div()
        .flex()
        .flex_wrap()
        .child(draft[..cursor].to_string())
        .child(div().w(px(1.)).h(px(18.)).bg(rgb(0xffffff)))
        .child(draft[cursor..].to_string())
}

fn selected_text(draft: &str, start: usize, end: usize) -> gpui::Div {
    div()
        .flex()
        .flex_wrap()
        .child(draft[..start].to_string())
        .child(div().bg(rgb(0x3d7eff)).child(draft[start..end].to_string()))
        .child(draft[end..].to_string())
}

fn model_trigger(
    root: &mut Root,
    controls: &present::Controls,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let model = open_model(&root.desk);
    let caption = present::model_caption(&model);
    let provider = present::model_provider_line(&model, &root.desk.state.catalog);
    div()
        .id("model-trigger")
        .flex()
        .flex_col()
        .px(px(8.))
        .py(px(4.))
        .rounded(px(8.))
        .cursor_pointer()
        .opacity(if controls.model_enabled { 1. } else { 0.45 })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, _: &gpui::MouseDownEvent, window, cx| {
                root.desk.open_picker();
                if root.desk.picker_open {
                    root.search_focus.focus(window);
                }
                cx.notify();
            }),
        )
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight(600.))
                .child(caption),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(rgb(0x9a9aa3))
                .child(provider),
        )
}

fn open_model(desk: &Desk) -> ModelState {
    desk.state
        .selected
        .as_ref()
        .and_then(|id| desk.state.conversations.get(id))
        .map(|chat| chat.model.clone())
        .unwrap_or_default()
}

fn composer_actions(controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .text_size(px(11.))
                .text_color(rgb(0x8a8a92))
                .child("Enter to send · Shift+Enter for a new line"),
        )
        .when(controls.stop_visible, |row| {
            row.child(text_button(
                "stop",
                "Stop",
                controls.stop_enabled,
                false,
                cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                    root.desk.stop();
                    cx.notify();
                }),
            ))
        })
        .child(text_button(
            "send",
            "Send",
            controls.send,
            true,
            cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                root.desk.submit();
                cx.notify();
            }),
        ))
}

fn overlay(root: &mut Root, cx: &mut Context<Root>) -> Vec<gpui::Div> {
    let mut layers = Vec::new();
    if root.desk.picker_open {
        layers.push(picker(root, cx));
    }
    if root.desk.confirm.is_some() {
        layers.push(confirm_dialog(root, cx));
    }
    layers
}

fn picker(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let rows = root.desk.picker_rows();
    let status = present::picker_status(&root.desk.state.catalog, rows.len());
    let note = if root.desk.picker_note.is_empty() {
        status
    } else {
        root.desk.picker_note.clone()
    };
    div()
        .absolute()
        .bottom(px(120.))
        .left(px(24.))
        .w(px(380.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(10.))
        .rounded(px(12.))
        .bg(rgb(0x24242c))
        .border_1()
        .border_color(rgb(0x3a3a42))
        .child(search_field(root, cx))
        .when(!note.is_empty(), |panel| {
            panel.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(0xb0b0b8))
                    .child(note),
            )
        })
        .child(model_list(root, &rows, cx))
}

fn search_field(root: &mut Root, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    let query = if root.desk.picker_query.is_empty() {
        "Search provider or model".to_string()
    } else {
        root.desk.picker_query.clone()
    };
    let color = if root.desk.picker_query.is_empty() {
        rgb(0x8a8a92)
    } else {
        rgb(0xeeeeee)
    };
    div()
        .id("model-search")
        .p(px(8.))
        .rounded(px(8.))
        .bg(rgb(0x1c1c22))
        .text_color(color)
        .track_focus(&root.search_focus)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|root, _: &gpui::MouseDownEvent, window, cx| {
                root.search_focus.focus(window);
                cx.notify();
            }),
        )
        .child(query)
}

fn model_list(
    root: &Root,
    rows: &[crate::model::ModelOption],
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let current = open_model(&root.desk);
    let mut list = div()
        .id("model-list")
        .flex()
        .flex_col()
        .gap(px(4.))
        .max_h(px(320.))
        .overflow_y_scroll();
    for (index, option) in rows.iter().enumerate() {
        list = list.child(model_row(
            option,
            index,
            index == root.desk.picker_index,
            &current,
            cx,
        ));
    }
    list
}

fn model_row(
    option: &crate::model::ModelOption,
    index: usize,
    highlighted: bool,
    current: &ModelState,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let selected = current.confirmed().or_else(|| current.target()) == Some(&option.identity);
    let mut row = div()
        .id(gpui::SharedString::new(format!("model-{index}")))
        .flex()
        .justify_between()
        .px(px(8.))
        .py(px(6.))
        .rounded(px(7.))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |root, _: &gpui::MouseDownEvent, window, cx| {
                if root.desk.commit_picker_index(index) {
                    root.composer_focus.focus(window);
                }
                cx.notify();
            }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(FontWeight(600.))
                        .child(option.identity.model.clone()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(0x9a9aa3))
                        .child(option.provider_name.clone()),
                ),
        );
    if !option.selectable() {
        row = row.child(
            div()
                .text_size(px(11.))
                .text_color(rgb(0x9a9aa3))
                .child("unavailable"),
        );
    } else if selected {
        row = row.child("●");
    }
    if highlighted {
        row = row.bg(rgb(0x2a3348));
    }
    row
}

fn confirm_dialog(root: &mut Root, cx: &mut Context<Root>) -> gpui::Div {
    let message = root
        .desk
        .confirm
        .as_ref()
        .map(|prompt| prompt.message.clone())
        .unwrap_or_default();
    div()
        .absolute()
        .top(px(80.))
        .left(px(80.))
        .w(px(420.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(16.))
        .rounded(px(12.))
        .bg(rgb(0x24242c))
        .border_1()
        .border_color(rgb(0x3a3a42))
        .child(div().font_weight(FontWeight(700.)).child("Use this model?"))
        .child(message)
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(text_button(
                    "confirm-cancel",
                    "Cancel",
                    true,
                    false,
                    cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                        root.desk.answer_confirm(false);
                        cx.notify();
                    }),
                ))
                .child(text_button(
                    "confirm-switch",
                    "Switch",
                    true,
                    true,
                    cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                        root.desk.answer_confirm(true);
                        cx.notify();
                    }),
                )),
        )
}

fn text_button(
    id: impl AsRef<str>,
    label: impl Into<String>,
    enabled: bool,
    primary: bool,
    handler: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let mut button = div()
        .id(gpui::SharedString::new(id.as_ref().to_string()))
        .px(px(10.))
        .py(px(6.))
        .rounded(px(8.))
        .text_size(px(13.))
        .child(label.into());
    button = if primary {
        button.bg(rgb(0x3d7eff)).text_color(rgb(0xffffff))
    } else {
        button.bg(rgb(0x2c2c34))
    };
    if enabled {
        button
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, handler)
    } else {
        button.opacity(0.45)
    }
}

fn wheel_up(delta: &ScrollDelta) -> bool {
    match delta {
        ScrollDelta::Pixels(point) => point.y > px(0.),
        ScrollDelta::Lines(point) => point.y > 0.,
    }
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        index = text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn selection_of(text: &str, anchor: usize, cursor: usize) -> (usize, usize) {
    let anchor = floor_boundary(text, anchor);
    let cursor = floor_boundary(text, cursor);
    if anchor < cursor {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    }
}

fn prev_boundary(text: &str, cursor: usize) -> usize {
    let cursor = floor_boundary(text, cursor.min(text.len()));
    if cursor == 0 {
        0
    } else {
        floor_boundary(text, cursor - 1)
    }
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    let cursor = floor_boundary(text, cursor.min(text.len()));
    if cursor >= text.len() {
        return text.len();
    }
    let mut index = cursor + 1;
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn caret_move(
    text: &str,
    cursor: usize,
    anchor: usize,
    key: &str,
    shift: bool,
) -> Option<(usize, usize)> {
    let next = match key {
        "left" => prev_boundary(text, cursor),
        "right" => next_boundary(text, cursor),
        "home" => line_start(text, cursor),
        "end" => line_end(text, cursor),
        "up" => move_vertical(text, cursor, false),
        "down" => move_vertical(text, cursor, true),
        _ => return None,
    };
    let anchor = if shift { anchor } else { next };
    Some((anchor, next))
}

fn line_start(text: &str, cursor: usize) -> usize {
    let cursor = floor_boundary(text, cursor.min(text.len()));
    text[..cursor]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn line_end(text: &str, cursor: usize) -> usize {
    let cursor = floor_boundary(text, cursor.min(text.len()));
    text[cursor..]
        .find('\n')
        .map(|index| cursor + index)
        .unwrap_or(text.len())
}

fn move_vertical(text: &str, cursor: usize, down: bool) -> usize {
    let start = line_start(text, cursor);
    let end = line_end(text, cursor);
    let column = cursor.saturating_sub(start);
    if down {
        return lower_line(text, end, column);
    }
    upper_line(text, start, column)
}

fn lower_line(text: &str, end: usize, column: usize) -> usize {
    if end >= text.len() {
        return text.len();
    }
    let next_start = end + 1;
    let next_end = line_end(text, next_start);
    next_start + column.min(next_end.saturating_sub(next_start))
}

fn upper_line(text: &str, start: usize, column: usize) -> usize {
    if start == 0 {
        return 0;
    }
    let prev_end = start - 1;
    let prev_start = line_start(text, prev_end);
    prev_start + column.min(prev_end.saturating_sub(prev_start))
}

impl Root {
    fn advance_preview(&mut self) -> bool {
        if self.preview_holding() || self.preview_finished() {
            return false;
        }
        match self.preview.as_ref().map(|preview| preview.step) {
            Some(PreviewStep::Sidebar) => self.step_sidebar(),
            Some(PreviewStep::Transcript) => self.step_transcript(),
            Some(PreviewStep::Reply) => self.step_reply(),
            Some(PreviewStep::Picker) => self.step_picker(),
            Some(PreviewStep::Approval) => self.step_approval(),
            Some(PreviewStep::Readonly) => self.step_readonly(),
            Some(PreviewStep::Done) => self.step_done(),
            None => false,
        }
    }

    fn preview_holding(&self) -> bool {
        self.preview.as_ref().is_some_and(holding)
    }

    fn preview_finished(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|preview| matches!(preview.published.as_deref(), Some("done" | "error")))
    }

    fn step_sidebar(&mut self) -> bool {
        if self.phase_done("sidebar") {
            self.begin_step(PreviewStep::Transcript);
            return true;
        }
        if sidebar_ready(&self.desk) {
            self.publish("sidebar");
            return true;
        }
        self.fail_late()
    }

    fn step_transcript(&mut self) -> bool {
        if self.phase_done("transcript") {
            self.begin_step(PreviewStep::Reply);
            return true;
        }
        if !self.preview_flag(|preview| preview.picked_notes) {
            self.desk.select("notes");
            self.pending_focus = Some(Focus::Composer);
            self.set_preview(|preview| preview.picked_notes = true);
            return true;
        }
        if transcript_ready(&self.desk) {
            self.publish("transcript");
            return true;
        }
        self.fail_late()
    }

    fn step_reply(&mut self) -> bool {
        if self.phase_done("reply") {
            self.begin_step(PreviewStep::Picker);
            return true;
        }
        if !self.preview_flag(|preview| preview.asked_create) {
            self.desk.create();
            self.set_preview(|preview| preview.asked_create = true);
            return true;
        }
        if new_chat_ready(&self.desk) && !self.preview_flag(|preview| preview.sent_reply) {
            self.desk.set_draft("Show a markdown reply".into());
            self.desk.submit();
            self.set_preview(|preview| preview.sent_reply = true);
            return true;
        }
        if reply_visible(&self.desk) {
            self.publish("reply");
            return true;
        }
        self.fail_late()
    }

    fn step_picker(&mut self) -> bool {
        if self.phase_done("picker") {
            self.desk.close_picker();
            self.begin_step(PreviewStep::Approval);
            return true;
        }
        self.desk.open_picker();
        self.pending_focus = Some(Focus::Search);
        self.publish("picker");
        true
    }

    fn step_approval(&mut self) -> bool {
        if self.phase_done("approval") {
            self.begin_step(PreviewStep::Readonly);
            return true;
        }
        if new_chat_ready(&self.desk) && !self.preview_flag(|preview| preview.sent_approval) {
            self.desk.set_draft("approve the preview tool".into());
            self.desk.submit();
            self.set_preview(|preview| preview.sent_approval = true);
            return true;
        }
        if interaction_open(&self.desk) {
            self.publish("approval");
            return true;
        }
        self.fail_late()
    }

    fn step_readonly(&mut self) -> bool {
        if self.phase_done("readonly") {
            self.begin_step(PreviewStep::Done);
            return true;
        }
        if !self.preview_flag(|preview| preview.picked_board) {
            self.desk.select("board");
            self.set_preview(|preview| preview.picked_board = true);
            return true;
        }
        if board_readonly(&self.desk) {
            self.publish("readonly");
            return true;
        }
        self.fail_late()
    }

    fn step_done(&mut self) -> bool {
        self.publish("done");
        true
    }

    fn phase_done(&self, phase: &str) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|preview| preview.published.as_deref() == Some(phase) && !holding(preview))
    }

    fn begin_step(&mut self, step: PreviewStep) {
        if let Some(preview) = self.preview.as_mut() {
            preview.step = step;
            preview.published = None;
            preview.since = Instant::now();
        }
    }

    fn publish(&mut self, phase: &str) {
        let Some(preview) = self.preview.as_mut() else {
            return;
        };
        if preview.published.as_deref() == Some(phase) {
            return;
        }
        let _ = std::fs::write(preview.dir.join("phase"), phase);
        preview.published = Some(phase.to_string());
        preview.since = Instant::now();
    }

    fn fail_late(&mut self) -> bool {
        let late = self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.since.elapsed() > Duration::from_secs(20));
        if !late {
            return false;
        }
        self.publish("error");
        self.begin_step(PreviewStep::Done);
        true
    }

    fn preview_flag(&self, flag: impl Fn(&Preview) -> bool) -> bool {
        self.preview.as_ref().is_some_and(flag)
    }

    fn set_preview(&mut self, set: impl FnOnce(&mut Preview)) {
        if let Some(preview) = self.preview.as_mut() {
            set(preview);
        }
    }
}

fn holding(preview: &Preview) -> bool {
    let Some(phase) = preview.published.as_deref() else {
        return false;
    };
    if ack_is(&preview.dir, phase) {
        return false;
    }
    preview.since.elapsed() < Duration::from_secs(15)
}

fn ack_is(dir: &std::path::Path, phase: &str) -> bool {
    std::fs::read_to_string(dir.join("ack"))
        .map(|text| text.trim() == phase)
        .unwrap_or(false)
}

fn sidebar_ready(desk: &Desk) -> bool {
    desk.state.connection == Some(ConnectionState::Connected) && !desk.state.sessions.is_empty()
}

fn transcript_ready(desk: &Desk) -> bool {
    desk.state.selected.as_deref() == Some("notes")
        && desk
            .state
            .conversations
            .get("notes")
            .is_some_and(|chat| chat.loaded && !chat.messages.is_empty())
}

fn new_chat_ready(desk: &Desk) -> bool {
    desk.state
        .selected
        .as_deref()
        .is_some_and(|id| id.starts_with("stored-"))
        && desk
            .state
            .selected
            .as_ref()
            .and_then(|id| desk.state.conversations.get(id))
            .is_some_and(|chat| chat.loaded && chat.runtime_id.is_some())
}

fn reply_visible(desk: &Desk) -> bool {
    desk.state
        .selected
        .as_ref()
        .and_then(|id| desk.state.conversations.get(id))
        .is_some_and(|chat| {
            chat.messages.iter().any(|message| {
                message.role == Role::Assistant && message.text.contains("# Preview")
            })
        })
}

fn interaction_open(desk: &Desk) -> bool {
    open_interaction(desk).is_some()
}

fn board_readonly(desk: &Desk) -> bool {
    desk.state
        .conversations
        .get("board")
        .and_then(|chat| chat.read_only_reason.as_ref())
        .is_some()
}
