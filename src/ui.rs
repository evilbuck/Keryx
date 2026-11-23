//! GPUI window for Hermes Native.
//!
//! [`crate::desk::Desk`] owns the conversation. This module paints it and turns
//! keystrokes and clicks into desk intents. Tokio stays on the `hermes-net`
//! thread; a GPUI timer drains that thread's events onto the UI entity.
//! `--agent-socket` is off unless set. When set, the same timer polls local
//! control requests and dispatches them through the real window.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, ClipboardItem, Context, FocusHandle, FontWeight, KeyBinding,
    KeyDownEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PlatformInput, ScrollDelta, ScrollHandle, ScrollWheelEvent, TitlebarOptions, Window,
    WindowBounds, WindowDecorations, WindowOptions, actions, canvas, div, point, prelude::*, px,
    rgb, size,
};

use crate::automation::{Automation, Request};

use crate::desk::Desk;
use crate::markdown::render_markdown;
use crate::model::{ChatMessage, Config, ConnectionState, InteractionKind, ModelState, Role};
use crate::present::{self, QuestionDraft, Tone};

mod chrome;

actions!(hermes, [NewConversation]);

const APP_ID: &str = "io.github.hermes_native.Client";
const WINDOW_TITLE: &str = "Hermes Native";
const SIDEBAR_WIDTH: f32 = 280.;
const INSPECTOR_WIDTH: f32 = 320.;
const MIN_CHAT_WIDTH: f32 = 320.;
const COMPACT_FOOTER_HEIGHT: f32 = 560.;

pub struct Launch {
    pub config: Result<Config, String>,
    pub preview_dir: Option<PathBuf>,
    pub agent_socket: Option<PathBuf>,
}

pub fn launch(options: Launch) -> Result<(), String> {
    let automation = match options.agent_socket {
        Some(path) => Some(crate::automation::start(&path)?),
        None => None,
    };
    let config = options.config;
    let preview_dir = options.preview_dir;
    Application::new().run(move |cx| open_window(config, preview_dir, automation, cx));
    Ok(())
}

fn open_window(
    config: Result<Config, String>,
    preview_dir: Option<PathBuf>,
    automation: Option<Automation>,
    cx: &mut App,
) {
    cx.bind_keys([KeyBinding::new("ctrl-n", NewConversation, None)]);
    let bounds = Bounds::centered(None, size(px(1120.), px(800.)), cx);
    if cx
        .open_window(window_options(bounds), move |window, cx| {
            let root = cx.new(|cx| Root::new(config, preview_dir, automation, cx));
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
            title: Some(WINDOW_TITLE.into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        window_min_size: Some(size(px(640.), px(480.))),
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some(APP_ID.into()),
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
    Find,
    Sheet,
    Session,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreviewStep {
    Sidebar,
    Transcript,
    Reply,
    Quote,
    Slash,
    Palette,
    Deck,
    Find,
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
    sidebar_was_open: bool,
    composer_focus: FocusHandle,
    turn_controls_focus: FocusHandle,
    search_focus: FocusHandle,
    clarify_focus: FocusHandle,
    session_focus: FocusHandle,
    find_focus: FocusHandle,
    sheet_focus: FocusHandle,
    side_focus: FocusHandle,
    sheet: chrome::Sheet,
    sheet_text: String,
    sheet_index: usize,
    sheet_scroll: ScrollHandle,
    slash_index: usize,
    slash_hidden: bool,
    side_text: String,
    jump_to: Option<usize>,
    revealed_find_match: Option<usize>,
    answer_qid: Option<String>,
    transcript: ScrollHandle,
    draft_scroll: ScrollHandle,
    reveal_draft_caret: bool,
    cursor: usize,
    anchor: usize,
    seen_generation: u64,
    followed_revision: u64,
    pending_focus: Option<Focus>,
    polling: bool,
    focused_once: bool,
    spin: u8,
    preview: Option<Preview>,
    automation: Option<Automation>,
    agent_window: Option<gpui::AnyWindowHandle>,
    pending_agent: Option<Request>,
    agent_scheduled: bool,
    agent_closed: bool,
    capture: Option<CaptureJob>,
}

impl Root {
    fn new(
        config: Result<Config, String>,
        preview_dir: Option<PathBuf>,
        automation: Option<Automation>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            desk: Desk::new(config),
            sidebar_was_open: true,
            composer_focus: cx.focus_handle(),
            turn_controls_focus: cx.focus_handle(),
            search_focus: cx.focus_handle(),
            clarify_focus: cx.focus_handle(),
            session_focus: cx.focus_handle(),
            find_focus: cx.focus_handle(),
            sheet_focus: cx.focus_handle(),
            side_focus: cx.focus_handle(),
            sheet: chrome::Sheet::None,
            sheet_text: String::new(),
            sheet_index: 0,
            sheet_scroll: ScrollHandle::new(),
            slash_index: 0,
            slash_hidden: false,
            side_text: String::new(),
            jump_to: None,
            revealed_find_match: None,
            answer_qid: None,
            transcript: ScrollHandle::new(),
            draft_scroll: ScrollHandle::new(),
            reveal_draft_caret: false,
            cursor: 0,
            anchor: 0,
            seen_generation: 0,
            followed_revision: u64::MAX,
            pending_focus: None,
            polling: false,
            focused_once: false,
            spin: 0,
            preview: preview_dir.map(Preview::new),
            automation,
            agent_window: None,
            pending_agent: None,
            agent_scheduled: false,
            agent_closed: false,
            capture: None,
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
        self.poll_capture();
        let queued = self.queue_agent_request();
        if let Some(window) = self.agent_window
            && let Some(request) = self.pending_agent.take()
        {
            self.agent_scheduled = true;
            let view = cx.entity();
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| {
                    run_agent(view, request, window, cx);
                });
            });
        }
        self.spin = self.spin.wrapping_add(1);
        if changed || stepped || queued || self.pending_agent.is_some() {
            cx.notify();
        }
        self.arm(cx);
    }

    fn prepare(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.agent_window = Some(window.window_handle());
        self.fit_side_panels(window);
        self.sync_cursor();
        self.claim_focus(window);
        self.sync_turn_focus(window);
        self.sync_find_match();
        self.follow_transcript();
    }

    fn sync_turn_focus(&mut self, window: &mut Window) {
        if self.compact_turn_controls(window) {
            if self.composer_focus.is_focused(window) || self.search_focus.is_focused(window) {
                self.desk.close_picker();
                self.turn_controls_focus.focus(window);
            }
        } else if self.turn_controls_focus.is_focused(window)
            || (open_interaction(&self.desk).is_none() && self.clarify_focus.is_focused(window))
        {
            self.composer_focus.focus(window);
        }
    }

    fn compact_turn_controls(&self, window: &Window) -> bool {
        window.viewport_size().height < px(COMPACT_FOOTER_HEIGHT)
            && open_interaction(&self.desk).is_some()
    }

    fn fit_side_panels(&mut self, window: &Window) {
        let min_split_width = px(SIDEBAR_WIDTH + MIN_CHAT_WIDTH + INSPECTOR_WIDTH);
        if window.viewport_size().width < min_split_width
            && self.desk.sidebar_open
            && self.desk.state.inspector_open
        {
            // A newly opened panel takes over from the one already visible.
            self.desk.sidebar_open = !self.sidebar_was_open;
            self.desk.state.inspector_open = self.sidebar_was_open;
        }
        self.sidebar_was_open = self.desk.sidebar_open;
        if (!self.desk.sidebar_open && self.session_focus.is_focused(window))
            || (!self.desk.state.inspector_open && self.side_focus.is_focused(window))
        {
            self.pending_focus.get_or_insert(Focus::Composer);
        }
    }

    fn sync_cursor(&mut self) {
        if self.seen_generation == self.desk.draft_generation {
            return;
        }
        self.seen_generation = self.desk.draft_generation;
        let len = self.desk.draft().len();
        self.cursor = len;
        self.anchor = len;
        self.reveal_draft_caret = true;
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
            Focus::Find => self.find_focus.focus(window),
            Focus::Sheet => self.sheet_focus.focus(window),
            Focus::Session => self.session_focus.focus(window),
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
        if self.global_chord(event) || self.surface_key(event, window, cx) {
            cx.notify();
            return;
        }
        self.focus_key(event, window, cx);
    }

    fn surface_key(
        &mut self,
        event: &KeyDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.key == "escape" && self.escape() {
            return true;
        }
        if self.sheet != chrome::Sheet::None {
            return self.on_sheet_key(event, cx);
        }
        self.focused_field(event, window)
    }

    fn focused_field(&mut self, event: &KeyDownEvent, window: &Window) -> bool {
        if self.find_focus.is_focused(window) {
            return self.on_find_key(event);
        }
        if self.session_focus.is_focused(window) {
            return self.on_session_key(event);
        }
        if self.side_focus.is_focused(window) {
            return self.on_side_key(event);
        }
        false
    }

    fn focus_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_focus.is_focused(window) {
            self.on_search_key(event, cx);
        } else if self.clarify_focus.is_focused(window) {
            self.on_answer_key(event, cx);
        } else if self.composer_focus.is_focused(window) {
            self.on_composer_key(event, cx);
        }
    }

    fn escape(&mut self) -> bool {
        if self.sheet != chrome::Sheet::None {
            self.close_sheet();
            return true;
        }
        if self.desk.confirm.is_some() {
            self.desk.answer_confirm(false);
            return true;
        }
        if self.desk.picker_open {
            self.desk.close_picker();
            self.pending_focus = Some(Focus::Composer);
            return true;
        }
        if self.desk.state.transcript_find.open {
            self.close_find();
            return true;
        }
        false
    }

    fn on_search_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "enter" {
            if self.desk.commit_picker() {
                self.pending_focus = Some(Focus::Composer);
            }
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
        if self.slash_key(key, mods) {
            return true;
        }
        if self.try_recall(key, mods) {
            return true;
        }
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
        self.reveal_draft_caret = true;
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
            self.take_composer_follow();
        }
    }

    fn take_composer_follow(&mut self) {
        match self.desk.composer_follow.take() {
            Some(crate::desk::ComposerFollow::Attach) => self.open_sheet(chrome::Sheet::Attach),
            Some(crate::desk::ComposerFollow::Workspace(query)) => {
                self.open_sheet(chrome::Sheet::Workspace);
                if let Some(query) = query {
                    self.sheet_text = query;
                }
            }
            Some(crate::desk::ComposerFollow::Find) => self.focus_find(),
            None => {}
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
        if !self.desk.controls().editable {
            return;
        }
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
        if !self.desk.controls().editable {
            return;
        }
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
        self.reveal_draft_caret = true;
        let (index, hidden) = chrome::slash_state_after_edit(self.slash_hidden, &next);
        self.slash_index = index;
        self.slash_hidden = hidden;
        self.desk.set_draft(next);
    }

    fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.desk.draft().len();
        self.reveal_draft_caret = true;
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
        self.prepare(window, cx);
        let controls = self.desk.controls();
        let connection = self.desk.connection();
        let viewport = window.viewport_size();
        let compact_header = viewport.width < px(760.);
        let find_focused =
            self.desk.state.transcript_find.open && self.find_focus.is_focused(window);
        let compact_footer = self.compact_turn_controls(window)
            || (viewport.height < px(COMPACT_FOOTER_HEIGHT) && find_focused);
        shell(self, &controls, &connection, compact_header, compact_footer, cx)
    }
}

fn shell(
    root: &mut Root,
    controls: &present::Controls,
    connection: &present::ConnectionView,
    compact_header: bool,
    compact_footer: bool,
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
        .child(header(root, controls, connection, compact_header, cx))
        .child(body(root, controls, compact_footer, cx))
        // Keep a keyboard target mounted while the editor is temporarily absent.
        .child(
            div()
                .id("turn-controls-focus")
                .absolute()
                .size(px(0.))
                .track_focus(&root.turn_controls_focus),
        )
        .children(overlay(root, cx))
}

fn header(
    root: &mut Root,
    controls: &present::Controls,
    connection: &present::ConnectionView,
    compact: bool,
    cx: &mut Context<Root>,
) -> gpui::Div {
    let sidebar = conversations_button(cx).flex_shrink_0();
    let title = header_title(root, controls);
    let tools = chrome::header_tools(root, cx).flex_shrink_0();
    let status = connection_label(connection, controls.reconnect, cx).flex_shrink_0();
    let bar = div()
        .flex()
        .flex_shrink_0()
        .px(px(16.))
        .bg(rgb(0x1e1e24))
        .border_b_1()
        .border_color(rgb(0x34343c));
    if compact {
        return bar
            .flex_col()
            .gap(px(4.))
            .py(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(title.flex_1())
                    .child(status),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(sidebar)
                    .child(tools),
            );
    }
    bar.items_center()
        .gap(px(12.))
        .min_h(px(56.))
        .child(sidebar)
        .child(title)
        .child(tools)
        .child(div().flex_1())
        .child(status)
}

fn lineage_caption(desk: &Desk) -> gpui::Div {
    let text = desk
        .state
        .selected
        .as_deref()
        .and_then(|id| desk.state.parent_for(id))
        .map(|(_, title)| format!("Branched from {title}"))
        .unwrap_or_default();
    div()
        .truncate()
        .text_size(px(11.))
        .text_color(rgb(0x9a9aa3))
        .child(text)
}

fn conversations_button(cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    text_button(
        "toggle-sidebar",
        "Conversations",
        true,
        false,
        cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
            root.desk.toggle_sidebar();
            cx.notify();
        }),
    )
}

fn header_title(root: &Root, controls: &present::Controls) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .min_w(px(0.))
        .child(
            div()
                .truncate()
                .text_size(px(16.))
                .font_weight(FontWeight(700.))
                .child(controls.title.clone()),
        )
        .child(lineage_caption(&root.desk))
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

fn body(
    root: &mut Root,
    controls: &present::Controls,
    compact_footer: bool,
    cx: &mut Context<Root>,
) -> gpui::Div {
    div()
        .flex()
        .flex_1()
        .min_h(px(0.))
        .w_full()
        .when(root.desk.sidebar_open, |row| {
            row.child(sidebar(root, controls, cx))
        })
        .child(
            main_column(root, controls, compact_footer, cx)
                .flex_1()
                .min_w(px(0.)),
        )
        .when(root.desk.state.inspector_open, |row| {
            row.child(chrome::inspector(root, cx))
        })
}

fn sidebar(root: &mut Root, controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Div {
    div()
        .w(px(SIDEBAR_WIDTH))
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
        .child(chrome::filter_row(root, cx))
        .child(chrome::session_search(root, cx))
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
    let sessions = visible_session_rows(&root.desk);
    let mut list = div().flex().flex_col().gap(px(6.));
    for session in sessions {
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
    for hit in root.desk.state.remote_only_sidebar_hits() {
        list = list.child(hit_row(hit, &root.desk.state.session_query, cx));
    }
    list
}

fn visible_session_rows(desk: &Desk) -> Vec<crate::model::SessionSummary> {
    desk.state
        .visible_sessions()
        .into_iter()
        .filter_map(|index| desk.state.sessions.get(index).cloned())
        .collect()
}

fn hit_row(
    hit: &crate::model::SearchHit,
    query: &str,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let id = hit.stored_id.clone();
    let find = query.to_string();
    div()
        .id(gpui::SharedString::new(format!("hit-{id}")))
        .p(px(12.))
        .rounded(px(10.))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |root, _: &gpui::MouseDownEvent, window, cx| {
                root.desk
                    .run_palette(crate::model::PaletteAction::OpenSearchHit {
                        stored_id: id.clone(),
                        query: find.clone(),
                    });
                root.pending_focus = Some(Focus::Find);
                root.composer_focus.focus(window);
                cx.notify();
            }),
        )
        .child(hit.title.clone())
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(0x9a9aa3))
                .child(hit.snippet_plain.clone()),
        )
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
                .child(chrome::session_caption(session, read_only)),
        );
    if active {
        row = row.bg(rgb(0x2a3348));
    }
    row
}

fn main_column(
    root: &mut Root,
    controls: &present::Controls,
    compact_footer: bool,
    cx: &mut Context<Root>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .h_full()
        .min_h(px(0.))
        .bg(rgb(0x16161a))
        .child(notice(&root.desk.state.notice))
        .child(readonly_banner(controls))
        .when(root.desk.state.transcript_find.open, |column| {
            column.child(chrome::find_bar(root, cx))
        })
        .child(history_bar(controls, cx))
        .child(transcript(root, controls, cx).flex_1())
        .when(!compact_footer, |column| column.child(footer(root, controls, cx)))
        .when(compact_footer, |column| {
            column
                .when_some(interaction(root, cx), |column, panel| {
                    column.child(
                        div()
                            .id("compact-interaction")
                            .min_h(px(0.))
                            .max_h(px(208.))
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .child(panel.flex_shrink_0()),
                    )
                })
                .when(controls.stop_visible, |column| {
                    column.child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .justify_end()
                            .p(px(12.))
                            .child(stop_button(controls, cx)),
                    )
                })
        })
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
    let viewport = div()
        .id("transcript")
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .track_scroll(&root.transcript)
        .on_scroll_wheel(cx.listener(|root, event: &ScrollWheelEvent, _, _| {
            if wheel_up(&event.delta) {
                root.desk.follow_bottom = false;
            }
        }));
    if controls.show_messages {
        message_column(viewport, root, cx)
    } else {
        viewport.child(empty_state())
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

fn message_column(
    viewport: gpui::Stateful<gpui::Div>,
    root: &Root,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    let messages = root
        .desk
        .state
        .selected
        .as_ref()
        .and_then(|id| root.desk.state.conversations.get(id))
        .map_or(&[][..], |chat| chat.messages.as_slice());
    let mut column = viewport
        .flex()
        .flex_col()
        .gap(px(12.))
        .px(px(24.));
    let active = root
        .jump_to
        .or_else(|| root.desk.state.transcript_find.active_message_index());
    for (index, message) in messages.iter().enumerate() {
        column = column.child(
            message_card(message, index, active == Some(index), cx)
                .flex_shrink_0()
                .w_full()
                .max_w(px(812.))
                .mx_auto()
                .when(index == 0, |card| card.mt(px(24.)))
                .when(index + 1 == messages.len(), |card| card.mb(px(24.))),
        );
    }
    column
}

fn message_card(
    message: &ChatMessage,
    index: usize,
    active: bool,
    cx: &mut Context<Root>,
) -> gpui::Div {
    let text = message.text.clone();
    let id = message.id.clone();
    let role = message.role.clone();
    let mut card = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(14.))
        .rounded(px(12.))
        .bg(message_bg(&message.role))
        .child(message_heading(role, index, id, text, cx))
        .child(message_body(message));
    if active {
        card = card.border_1().border_color(rgb(0x3d7eff));
    }
    card
}

fn message_bg(role: &Role) -> gpui::Rgba {
    match role {
        Role::User => rgb(0x1d2a44),
        Role::Assistant => rgb(0x222228),
        Role::Tool => rgb(0x16161a),
        Role::System => rgb(0x3a321c),
    }
}

fn message_heading(
    role: Role,
    index: usize,
    id: String,
    text: String,
    cx: &mut Context<Root>,
) -> gpui::Div {
    let quotable = matches!(role, Role::User | Role::Assistant);
    div()
        .flex()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight(800.))
                .text_color(role_color(&role))
                .child(present::role_label(&role)),
        )
        .child(message_actions(quotable, index, id, text, cx))
}

fn message_actions(
    quotable: bool,
    index: usize,
    id: String,
    text: String,
    cx: &mut Context<Root>,
) -> gpui::Div {
    div()
        .flex()
        .gap(px(6.))
        .when(quotable, |row| {
            row.child(text_button(
                format!("quote-{index}"),
                "Quote",
                true,
                false,
                cx.listener(move |root, _: &gpui::MouseDownEvent, window, cx| {
                    if root.desk.quote_into_draft(index) {
                        root.composer_focus.focus(window);
                    }
                    cx.notify();
                }),
            ))
            .child(text_button(
                format!("branch-{index}"),
                "Branch",
                true,
                false,
                cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
                    root.desk.branch_from(Some(index), None);
                    cx.notify();
                }),
            ))
        })
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
        .children(interaction(root, cx))
        .when(chrome::slash_open(root), |column| {
            column.child(chrome::slash_row(root, cx))
        })
        .child(chrome::attachment_row(root, cx))
        .when(controls.spinning || !controls.activity.is_empty(), |column| {
            column.child(activity_row(controls, root.spin))
        })
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

fn interaction(root: &mut Root, cx: &mut Context<Root>) -> Option<gpui::Div> {
    root.desk.sync_questions();
    let kind = interaction_kind(&root.desk)?;
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
    Some(panel)
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
        .child(
            div()
                .relative()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(model_trigger(root, controls, cx))
                .child(chrome::workspace_button(root, cx))
                .child(rewind_button(
                    "undo-turn",
                    "Undo",
                    crate::model::RewindMode::Undo,
                    cx,
                ))
                .child(rewind_button(
                    "regen-turn",
                    "Regenerate",
                    crate::model::RewindMode::Regenerate,
                    cx,
                ))
                .when(root.desk.picker_open, |row| {
                    row.child(picker_anchor(root, cx))
                }),
        )
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
        .max_h(px(128.))
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .track_scroll(&root.draft_scroll)
        .pr(px(2.))
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

fn composer_text(root: &mut Root, read_only: bool) -> gpui::Div {
    let reveal = std::mem::take(&mut root.reveal_draft_caret);
    let draft = root.desk.draft();
    if draft.is_empty() {
        return div().text_color(rgb(0x8a8a92)).child(if read_only {
            "Read only"
        } else {
            "Message"
        });
    }
    let (start, end) = selection_of(&draft, root.anchor, root.cursor);
    let cursor = floor_boundary(&draft, root.cursor.min(draft.len()));
    let mut text = gpui::StyledText::new(draft);
    if start != end {
        let selection = gpui::HighlightStyle {
            background_color: Some(rgb(0x3d7eff).into()),
            ..Default::default()
        };
        text = text.with_highlights([(start..end, selection)]);
    }
    let caret = composer_caret(
        text.layout().clone(),
        cursor,
        start == end,
        reveal.then(|| root.draft_scroll.clone()),
    );
    div()
        .relative()
        .w_full()
        .flex_shrink_0()
        .child(text)
        .child(caret)
}

fn composer_caret(
    layout: gpui::TextLayout,
    cursor: usize,
    visible: bool,
    scroll: Option<ScrollHandle>,
) -> impl IntoElement {
    canvas(
        move |_, window, _| {
            let origin = layout.position_for_index(cursor)?;
            let caret = Bounds::new(origin, size(px(1.), layout.line_height()));
            if let Some(scroll) = scroll {
                reveal_caret(scroll, caret, window);
            }
            visible.then_some(caret)
        },
        |_, caret, window, _| {
            if let Some(caret) = caret {
                window.paint_quad(gpui::fill(caret, rgb(0xffffff)));
            }
        },
    )
    .absolute()
    .size_full()
}

fn reveal_caret(scroll: ScrollHandle, caret: Bounds<gpui::Pixels>, window: &Window) {
    let viewport = scroll.bounds();
    let dy = if caret.top() < viewport.top() {
        viewport.top() - caret.top()
    } else {
        (viewport.bottom() - caret.bottom()).min(px(0.))
    };
    if dy != px(0.) {
        let offset = scroll.offset() + point(px(0.), dy);
        // Child positions have already been laid out with this frame's scroll offset.
        window.on_next_frame(move |window, _| {
            scroll.set_offset(offset);
            window.refresh();
        });
    }
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
        .max_w_full()
        .px(px(8.))
        .py(px(4.))
        .rounded(px(8.))
        .cursor_pointer()
        .opacity(if controls.model_enabled { 1. } else { 0.45 })
        // When open, the picker's outside-click handler owns clicks on the trigger too.
        .when(!root.desk.picker_open, |trigger| {
            trigger.on_mouse_down(
                MouseButton::Left,
                cx.listener(|root, _: &MouseDownEvent, window, cx| {
                    root.desk.open_picker();
                    if root.desk.picker_open {
                        root.search_focus.focus(window);
                    }
                    cx.notify();
                }),
            )
        })
        .child(
            div()
                .truncate()
                .text_size(px(12.))
                .font_weight(FontWeight(600.))
                .child(caption),
        )
        .child(
            div()
                .truncate()
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
                .min_w(px(0.))
                .text_size(px(11.))
                .text_color(rgb(0x8a8a92))
                .child("Enter to send · Shift+Enter for a new line"),
        )
        .when(controls.stop_visible, |row| row.child(stop_button(controls, cx)))
        .child(
            text_button(
                "send",
                "Send",
                controls.send,
                true,
                cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
                    root.desk.submit();
                    root.take_composer_follow();
                    cx.notify();
                }),
            )
            .flex_shrink_0(),
        )
}

fn stop_button(controls: &present::Controls, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    text_button(
        "stop",
        "Stop",
        controls.stop_enabled,
        false,
        cx.listener(|root, _: &gpui::MouseDownEvent, _, cx| {
            root.desk.stop();
            cx.notify();
        }),
    )
    .flex_shrink_0()
}

fn rewind_button(
    id: &'static str,
    label: &'static str,
    mode: crate::model::RewindMode,
    cx: &mut Context<Root>,
) -> gpui::Stateful<gpui::Div> {
    text_button(
        id,
        label,
        true,
        false,
        cx.listener(move |root, _: &gpui::MouseDownEvent, _, cx| {
            root.desk.rewind(mode);
            cx.notify();
        }),
    )
}

fn overlay(root: &mut Root, cx: &mut Context<Root>) -> Vec<gpui::AnyElement> {
    let mut layers = Vec::new();
    match root.sheet {
        chrome::Sheet::Palette => layers.push(chrome::palette(root, cx).into_any_element()),
        chrome::Sheet::Rename => layers.push(
            chrome::text_sheet(root, "Rename conversation", "New title", cx).into_any_element(),
        ),
        chrome::Sheet::Attach => layers.push(
            chrome::text_sheet(root, "Attach a file", "Path to an image or text file", cx)
                .into_any_element(),
        ),
        chrome::Sheet::Workspace => {
            layers.push(chrome::workspace_sheet(root, cx).into_any_element())
        }
        chrome::Sheet::None => {}
    }
    if root.desk.confirm.is_some() {
        layers.push(confirm_dialog(root, cx));
    }
    layers
}

fn picker_anchor(root: &mut Root, cx: &mut Context<Root>) -> impl IntoElement {
    let panel = picker(root, cx);
    // Measure against the composer row, not the window: the drawer is outside this space.
    canvas(
        move |bounds, window, cx| {
            let bottom = (bounds.top() - px(6.)).max(px(0.));
            let width = bounds.size.width.min(px(380.));
            let mut panel = panel.w(width).max_h(bottom).into_any_element();
            let measured = panel.layout_as_root(size(width, bottom).into(), window, cx);
            window.defer_draw(panel, point(bounds.left(), bottom - measured.height), 0);
        },
        |_, (), _, _| {},
    )
    .absolute()
    .left_0()
    .top_0()
    .w_full()
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
        .occlude()
        .on_mouse_down_out(cx.listener(|root, _: &MouseDownEvent, window, cx| {
            root.desk.close_picker();
            if root.search_focus.is_focused(window) {
                root.composer_focus.focus(window);
            }
            cx.notify();
        }))
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
        .min_h(px(0.))
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

fn confirm_dialog(root: &mut Root, cx: &mut Context<Root>) -> gpui::AnyElement {
    let message = root
        .desk
        .confirm
        .as_ref()
        .map(|prompt| prompt.message.clone())
        .unwrap_or_default();
    div()
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .on_mouse_down(MouseButton::Left, |_, _, _| {})
        .child(confirm_card(message, cx))
        .into_any_element()
}

fn confirm_card(message: String, cx: &mut Context<Root>) -> gpui::Div {
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
    let cursor = floor_boundary(text, cursor.min(text.len()));
    let start = line_start(text, cursor);
    let target = if down {
        let end = line_end(text, cursor);
        if end == text.len() {
            return end;
        }
        end + 1
    } else {
        if start == 0 {
            return 0;
        }
        line_start(text, start - 1)
    };
    let end = line_end(text, target);
    let column = text[start..cursor].chars().count();
    text[target..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(byte, _)| target + byte)
}

impl Root {
    fn advance_preview(&mut self) -> bool {
        if self.preview_holding() || self.preview_finished() {
            return false;
        }
        let step = self.preview.as_ref().map(|preview| preview.step);
        self.preview_early(step) || self.preview_late(step)
    }

    fn preview_early(&mut self, step: Option<PreviewStep>) -> bool {
        match step {
            Some(PreviewStep::Sidebar) => self.step_sidebar(),
            Some(PreviewStep::Transcript) => self.step_transcript(),
            Some(PreviewStep::Reply) => self.step_reply(),
            Some(PreviewStep::Quote) => self.step_quote(),
            Some(PreviewStep::Slash) => self.step_slash(),
            Some(PreviewStep::Palette) => self.step_palette(),
            _ => false,
        }
    }

    fn preview_late(&mut self, step: Option<PreviewStep>) -> bool {
        match step {
            Some(PreviewStep::Deck) => self.step_deck(),
            Some(PreviewStep::Find) => self.step_find(),
            Some(PreviewStep::Picker) => self.step_picker(),
            Some(PreviewStep::Approval) => self.step_approval(),
            Some(PreviewStep::Readonly) => self.step_readonly(),
            Some(PreviewStep::Done) => self.step_done(),
            _ => false,
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
            self.begin_step(PreviewStep::Quote);
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

    fn step_quote(&mut self) -> bool {
        if self.phase_done("quote") {
            self.begin_step(PreviewStep::Slash);
            return true;
        }
        if self.phase_published("quote") {
            return false;
        }
        if self.desk.messages().len() < 2 {
            return self.fail_late();
        }
        let _ = self.desk.quote_into_draft(1);
        self.publish("quote");
        true
    }

    fn step_slash(&mut self) -> bool {
        if self.phase_done("slash") {
            self.desk.replace_draft(String::new());
            self.begin_step(PreviewStep::Palette);
            return true;
        }
        if self.phase_published("slash") {
            return false;
        }
        self.desk.replace_draft("/b".into());
        self.slash_hidden = false;
        self.publish("slash");
        true
    }

    fn step_palette(&mut self) -> bool {
        if self.phase_done("palette") {
            self.sheet = chrome::Sheet::None;
            self.begin_step(PreviewStep::Deck);
            return true;
        }
        self.sheet = chrome::Sheet::Palette;
        self.pending_focus = Some(Focus::Sheet);
        self.publish("palette");
        true
    }

    fn step_deck(&mut self) -> bool {
        if self.phase_done("deck") {
            self.desk.state.inspector_open = false;
            self.begin_step(PreviewStep::Find);
            return true;
        }
        self.desk.open_inspector(crate::model::InspectorTab::Trace);
        self.publish("deck");
        true
    }

    fn step_find(&mut self) -> bool {
        if self.phase_done("find") {
            self.desk.close_find();
            self.begin_step(PreviewStep::Picker);
            return true;
        }
        if self.phase_published("find") {
            return false;
        }
        self.desk.open_find(Some("markdown"));
        self.pending_focus = Some(Focus::Find);
        self.publish("find");
        true
    }

    fn phase_published(&self, phase: &str) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|preview| preview.published.as_deref() == Some(phase))
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

struct CaptureJob {
    done: std::sync::mpsc::Receiver<()>,
    flight: std::sync::Arc<Flight>,
}

struct Flight {
    cancel: std::sync::atomic::AtomicBool,
    gate: std::sync::Mutex<()>,
    child: std::sync::Mutex<Option<std::process::Child>>,
}

impl Drop for CaptureJob {
    fn drop(&mut self) {
        // Kill only. Waiting here would block the GPUI thread, and unlinking
        // here can delete a path that was replaced after the inode check.
        self.flight
            .cancel
            .store(true, std::sync::atomic::Ordering::SeqCst);
        kill_child(&self.flight.child);
        let _sync = self
            .flight
            .gate
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
    }
}

struct ShotPlan {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: f32,
    stable_id: String,
    window_active: bool,
}

struct CompositorWindow {
    stable_id: String,
    class: String,
    initial_class: String,
    title: String,
    pid: u32,
    mapped: bool,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    monitor: i32,
}

struct CompositorMonitor {
    id: i32,
    scale: f32,
}

impl Root {
    fn queue_agent_request(&mut self) -> bool {
        if self.agent_closed
            || self.agent_scheduled
            || self.pending_agent.is_some()
            || self.capture.is_some()
        {
            return false;
        }
        let received = self
            .automation
            .as_ref()
            .map(|automation| automation.requests.try_recv());
        let Some(received) = received else {
            return false;
        };
        match received {
            Ok(request) => {
                self.pending_agent = Some(request);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.agent_closed = true;
                false
            }
        }
    }

    fn agent_dispatched(&self, window: &Window) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "dispatched": true,
            "state": self.observed_state(window),
        })
    }

    fn agent_ok_state(&self, window: &Window) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "state": self.observed_state(window),
        })
    }

    fn observed_state(&self, window: &Window) -> serde_json::Value {
        let controls = self.desk.controls();
        let connection = self.desk.connection();
        let bounds = window.bounds();
        let pixels = |value: gpui::Pixels| value / px(1.);
        serde_json::json!({
            "focused": self.focused_name(window),
            "draft": self.desk.draft(),
            "cursor": self.cursor,
            "anchor": self.anchor,
            "sidebar_open": self.desk.sidebar_open,
            "picker_open": self.desk.picker_open,
            "picker_query": self.desk.picker_query,
            "picker_index": self.desk.picker_index,
            "sheet": sheet_name(self.sheet),
            "sheet_text": self.sheet_text,
            "confirm": self.desk.confirm.as_ref().map(|confirm| confirm.message.clone()),
            "notice": self.desk.state.notice,
            "connection": {
                "label": connection.label,
                "detail": connection.detail,
                "tone": tone_name(connection.tone),
            },
            "title": controls.title,
            "selected": self.desk.state.selected,
            "message_count": self.desk.messages().len(),
            "inspector_open": self.desk.state.inspector_open,
            "inspector_tab": inspector_tab_name(self.desk.state.inspector_tab),
            "session_filter": filter_name(self.desk.state.session_filter),
            "find_open": self.desk.state.transcript_find.open,
            "find_query": self.desk.state.transcript_find.query,
            "window_active": window.is_window_active(),
            "editable": controls.editable,
            "readonly": controls.readonly,
            "activity": controls.activity,
            "questions": self.desk.questions.iter().map(|question| question.qid.clone()).collect::<Vec<_>>(),
            "bounds": {
                "x": pixels(bounds.origin.x),
                "y": pixels(bounds.origin.y),
                "width": pixels(bounds.size.width),
                "height": pixels(bounds.size.height),
            },
            "preview": self.preview.as_ref().and_then(|preview| preview.published.clone()),
        })
    }

    fn focused_name(&self, window: &Window) -> &'static str {
        if self.composer_focus.is_focused(window) {
            "composer"
        } else if self.turn_controls_focus.is_focused(window) {
            "turn-controls"
        } else if self.search_focus.is_focused(window) {
            "search"
        } else if self.find_focus.is_focused(window) {
            "find"
        } else if self.sheet_focus.is_focused(window) {
            "sheet"
        } else if self.session_focus.is_focused(window) {
            "session"
        } else if self.side_focus.is_focused(window) {
            "side"
        } else if self.clarify_focus.is_focused(window) {
            "clarify"
        } else {
            "none"
        }
    }

    fn begin_capture(
        &mut self,
        path: String,
        window: &Window,
        reply: std::sync::mpsc::Sender<serde_json::Value>,
    ) {
        // Compositor queries and the grim wait block. They run on
        // `hermes-screenshot`, not this GPUI thread. Only the active bit is
        // sampled here, because `Window` is not usable off-thread.
        let window_active = window.is_window_active();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let flight = std::sync::Arc::new(Flight {
            cancel: std::sync::atomic::AtomicBool::new(false),
            gate: std::sync::Mutex::new(()),
            child: std::sync::Mutex::new(None),
        });
        let flight_worker = std::sync::Arc::clone(&flight);
        let worker_reply = reply.clone();
        let spawned = std::thread::Builder::new()
            .name("hermes-screenshot".into())
            .spawn(move || {
                let done = run_capture(&path, window_active, &flight_worker);
                publish(&flight_worker, &worker_reply, done);
                let _ = done_tx.send(());
            });
        match spawned {
            Ok(_worker) => {
                self.capture = Some(CaptureJob {
                    done: done_rx,
                    flight,
                });
            }
            Err(error) => {
                let _ = reply.send(failed(format!("screenshot could not start: {error}")));
            }
        }
    }

    fn poll_capture(&mut self) {
        let finished = self.capture.as_ref().is_some_and(|job| {
            matches!(
                job.done.try_recv(),
                Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected)
            )
        });
        if finished {
            self.capture = None;
        }
    }

}

fn run_agent(view: gpui::Entity<Root>, request: Request, window: &mut Window, cx: &mut App) {
    // Claim fails when the listener already told the client this command was
    // not applied. Running it now would make that response a lie.
    if !request.claim() {
        clear_agent(&view, cx);
        return;
    }
    let Request { command, reply, .. } = request;
    match command {
        crate::automation::Command::Key { key } => {
            let _ = reply.send(dispatch_key_outside(&view, &key, window, cx));
        }
        crate::automation::Command::Text { text } => {
            let _ = reply.send(dispatch_text_outside(&view, &text, window, cx));
        }
        crate::automation::Command::Click { x, y } => {
            let _ = reply.send(dispatch_click_outside(&view, x, y, window, cx));
        }
        crate::automation::Command::State => {
            let value = view.update(cx, |root, cx| {
                root.agent_scheduled = false;
                let value = root.agent_ok_state(window);
                cx.notify();
                value
            });
            let _ = reply.send(value);
        }
        crate::automation::Command::Screenshot { path } => {
            view.update(cx, |root, cx| {
                root.agent_scheduled = false;
                root.begin_capture(path, window, reply);
                cx.notify();
            });
        }
    }
}

fn dispatch_key_outside(
    view: &gpui::Entity<Root>,
    key: &str,
    window: &mut Window,
    cx: &mut App,
) -> serde_json::Value {
    let Ok(keystroke) = Keystroke::parse(key) else {
        clear_agent(view, cx);
        return failed(format!("invalid key {}", shown(key)));
    };
    let handled = dispatch_keystroke(keystroke, window, cx);
    finish_input(view, window, cx, |value| {
        value["handled"] = serde_json::json!(handled);
    })
}

fn dispatch_text_outside(
    view: &gpui::Entity<Root>,
    text: &str,
    window: &mut Window,
    cx: &mut App,
) -> serde_json::Value {
    let mut count = 0;
    let mut handled = 0;
    for ch in text.chars() {
        if dispatch_keystroke(text_keystroke(ch), window, cx) {
            handled += 1;
        }
        count += 1;
    }
    finish_input(view, window, cx, |value| {
        value["count"] = serde_json::json!(count);
        value["handled"] = serde_json::json!(handled);
    })
}

fn dispatch_click_outside(
    view: &gpui::Entity<Root>,
    x: f32,
    y: f32,
    window: &mut Window,
    cx: &mut App,
) -> serde_json::Value {
    if !x.is_finite() || !y.is_finite() {
        clear_agent(view, cx);
        return failed("click coordinates must be finite");
    }
    // Logical pixels from this window's origin, the same unit as state.bounds
    // width and height. Not screen coordinates and not screenshot pixels.
    window.draw(cx).clear();
    let position = point(px(x), px(y));
    let modifiers = window.modifiers();
    let moved = dispatch_mouse(
        window,
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        }),
        cx,
    );
    let down = dispatch_mouse(
        window,
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    let up = dispatch_mouse(
        window,
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        }),
        cx,
    );
    finish_input(view, window, cx, |value| {
        value["move"] = moved;
        value["down"] = down;
        value["up"] = up;
    })
}

fn dispatch_mouse(window: &mut Window, event: PlatformInput, cx: &mut App) -> serde_json::Value {
    let result = window.dispatch_event(event, cx);
    serde_json::json!({
        "propagate": result.propagate,
        "default_prevented": result.default_prevented,
    })
}

fn dispatch_keystroke(keystroke: Keystroke, window: &mut Window, cx: &mut App) -> bool {
    window.dispatch_keystroke(keystroke, cx)
}

fn finish_input(
    view: &gpui::Entity<Root>,
    window: &Window,
    cx: &mut App,
    extra: impl FnOnce(&mut serde_json::Value),
) -> serde_json::Value {
    view.update(cx, |root, cx| {
        root.agent_scheduled = false;
        let mut value = root.agent_dispatched(window);
        extra(&mut value);
        cx.notify();
        value
    })
}

fn clear_agent(view: &gpui::Entity<Root>, cx: &mut App) {
    view.update(cx, |root, cx| {
        root.agent_scheduled = false;
        cx.notify();
    });
}

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};

const COMPOSITOR_TIMEOUT: Duration = Duration::from_secs(2);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);
const COMPOSITOR_CAP: usize = 1024 * 1024;
const STDERR_CAP: usize = 8 * 1024;
const O_RDWR: i32 = 2;
const O_CREAT: i32 = 0o100;
const O_EXCL: i32 = 0o200;
const O_DIRECTORY: i32 = 0o200000;
const O_NOFOLLOW: i32 = 0o400000;
const O_CLOEXEC: i32 = 0o2000000;
const O_PATH: i32 = 0o10000000;
const AT_FDCWD: i32 = -100;
const RENAME_NOREPLACE: u32 = 1;

unsafe extern "C" {
    fn geteuid() -> u32;
    fn openat(dirfd: i32, path: *const i8, flags: i32, mode: u32) -> i32;
    fn unlinkat(dirfd: i32, path: *const i8, flags: i32) -> i32;
    fn renameat2(
        olddirfd: i32,
        oldpath: *const i8,
        newdirfd: i32,
        newpath: *const i8,
        flags: u32,
    ) -> i32;
}

struct CaptureDone {
    value: serde_json::Value,
    shot: Option<OwnedShot>,
}

struct OwnedShot {
    claimed: ClaimedShot,
    keep: bool,
}

impl Drop for OwnedShot {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        // Truncate the inode we still hold. A hardlink of this inode loses the
        // bytes; a replaced directory entry is a different inode and is not
        // written or unlinked.
        let _ = self.claimed.file.set_len(0);
        self.claimed.discard();
    }
}

#[derive(Debug)]
struct ClaimedShot {
    path: PathBuf,
    name: CString,
    dir: OwnedFd,
    file: File,
    dev: u64,
    ino: u64,
}

impl ClaimedShot {
    fn still_ours(&self) -> bool {
        let Ok(opened) = open_nofollow(self.dir.as_raw_fd(), &self.name) else {
            return false;
        };
        let Ok(meta) = metadata_fd(&opened) else {
            return false;
        };
        meta.file_type().is_file() && meta.dev() == self.dev && meta.ino() == self.ino
    }

    fn discard(&self) {
        unlink_if_same_file(
            self.dir.as_raw_fd(),
            &self.name,
            Inode {
                dev: self.dev,
                ino: self.ino,
            },
        );
    }
}

struct Inode {
    dev: u64,
    ino: u64,
}

struct CappedRead {
    flag: std::sync::Arc<AtomicBool>,
    rx: mpsc::Receiver<Result<Vec<u8>, String>>,
}

#[derive(Debug)]
enum WaitEnd {
    Exited(std::process::ExitStatus),
    TimedOut,
    Cancelled,
    Capped,
    Failed(String),
}

fn run_capture(path: &str, window_active: bool, flight: &Flight) -> CaptureDone {
    if cancelled(flight) {
        return done_err("screenshot cancelled");
    }
    let claimed = match prepare_shot_path(path) {
        Ok(claimed) => claimed,
        Err(error) => return done_err(error),
    };
    let mut shot = OwnedShot {
        claimed,
        keep: false,
    };
    let plan = match plan_capture(window_active, flight) {
        Ok(plan) => plan,
        Err(error) => return done_shot(error, shot),
    };
    if let Err(error) = prepare_file(&mut shot.claimed.file) {
        return done_shot(error, shot);
    }
    match capture_into(&mut shot.claimed, &plan, flight) {
        Ok(value) => CaptureDone {
            value,
            shot: Some(shot),
        },
        Err(error) => done_shot(error, shot),
    }
}

fn done_err(error: impl Into<String>) -> CaptureDone {
    CaptureDone {
        value: failed(error),
        shot: None,
    }
}

fn done_shot(error: impl Into<String>, shot: OwnedShot) -> CaptureDone {
    CaptureDone {
        value: failed(error),
        shot: Some(shot),
    }
}

fn publish(
    flight: &Flight,
    reply: &std::sync::mpsc::Sender<serde_json::Value>,
    mut done: CaptureDone,
) {
    let _gate = flight
        .gate
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if cancelled(flight) {
        let _ = reply.send(failed("screenshot cancelled"));
        return;
    }
    if done.value.get("ok").and_then(|ok| ok.as_bool()) == Some(true)
        && let Some(shot) = done.shot.as_mut()
    {
        shot.keep = true;
    }
    let _ = reply.send(done.value);
}

fn cancelled(flight: &Flight) -> bool {
    flight.cancel.load(Ordering::SeqCst)
}

fn plan_capture(window_active: bool, flight: &Flight) -> Result<ShotPlan, String> {
    let clients = compositor_clients(flight)?;
    let monitors = compositor_monitors(flight)?;
    let index = select_window(&clients, std::process::id())?;
    let ours = &clients[index];
    window_usable(ours)?;
    let scale = capture_scale(ours, &monitors)?;
    Ok(ShotPlan {
        x: ours.x,
        y: ours.y,
        width: ours.width,
        height: ours.height,
        scale,
        stable_id: ours.stable_id.clone(),
        window_active,
    })
}

fn window_usable(ours: &CompositorWindow) -> Result<(), String> {
    if !ours.mapped {
        return Err("window is not mapped".into());
    }
    check_stable_id(&ours.stable_id)?;
    if ours.width <= 0 || ours.height <= 0 {
        return Err("compositor reported an empty window".into());
    }
    Ok(())
}

fn check_stable_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.starts_with('-') || id.chars().any(|ch| ch.is_whitespace()) {
        return Err("compositor window id is not usable".into());
    }
    Ok(())
}

fn capture_scale(ours: &CompositorWindow, monitors: &[CompositorMonitor]) -> Result<f32, String> {
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.id == ours.monitor)
        .ok_or_else(|| "compositor did not report this window's monitor".to_string())?;
    if !monitor.scale.is_finite() || monitor.scale <= 0. {
        return Err("compositor reported an unusable scale".into());
    }
    Ok(monitor.scale)
}

fn capture_into(
    claimed: &mut ClaimedShot,
    plan: &ShotPlan,
    flight: &Flight,
) -> Result<serde_json::Value, String> {
    let mut child = spawn_grim(&claimed.file, &plan.stable_id)?;
    let stderr_rx = match child.stderr.take() {
        Some(pipe) => match spawn_reader(pipe, STDERR_CAP, false) {
            Ok(reader) => Some(reader),
            Err(error) => {
                silence(child);
                return Err(error);
            }
        },
        None => None,
    };
    install_child(&flight.child, child);
    if cancelled(flight) {
        kill_child(&flight.child);
        reap(&flight.child);
        let _ = recv_lossy(stderr_rx);
        return Err("screenshot cancelled".into());
    }
    let end = wait_child(&flight.child, CAPTURE_TIMEOUT, &flight.cancel, None);
    let stderr = recv_lossy(stderr_rx);
    match end {
        WaitEnd::Cancelled => Err("screenshot cancelled".into()),
        WaitEnd::TimedOut => Err("compositor capture timed out".into()),
        WaitEnd::Capped => Err("compositor capture failed: error output exceeded limit".into()),
        WaitEnd::Failed(error) => Err(format!("compositor capture failed: {error}")),
        WaitEnd::Exited(status) if !status.success() => Err(format!(
            "compositor capture failed: {}",
            stderr_text(&stderr)
        )),
        WaitEnd::Exited(_) => accept_png(claimed, plan),
    }
}

fn accept_png(claimed: &mut ClaimedShot, plan: &ShotPlan) -> Result<serde_json::Value, String> {
    let (png_width, png_height) = png_dimensions(&mut claimed.file)?;
    if !capture_size_matches(plan.width, plan.height, plan.scale, png_width, png_height) {
        let (expect_w, expect_h) = expected_pixels(plan.width, plan.height, plan.scale);
        return Err(format!(
            "compositor capture is {png_width}x{png_height}, expected {expect_w}x{expect_h} for the window"
        ));
    }
    if !claimed.still_ours() {
        return Err("screenshot path was replaced".into());
    }
    Ok(serde_json::json!({
        "ok": true,
        "path": claimed.path.display().to_string(),
        "capture": "toplevel",
        "x": plan.x,
        "y": plan.y,
        "width": plan.width,
        "height": plan.height,
        "png_width": png_width,
        "png_height": png_height,
        "window_active": plan.window_active,
        "source": "compositor",
    }))
}

fn grim_args(stable_id: &str) -> [&str; 7] {
    ["-s", "1", "-t", "png", "-T", stable_id, "-"]
}

fn spawn_grim(file: &File, stable_id: &str) -> Result<std::process::Child, String> {
    // `-` is grim's stdout sink. The PNG is written to the already-open
    // create_new descriptor, never by reopening the path.
    std::process::Command::new("grim")
        .args(grim_args(stable_id))
        .stdin(std::process::Stdio::null())
        .stdout(stdout_from(file)?)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "compositor capture needs grim; refusing a synthetic screenshot".into()
            } else {
                format!("grim could not be started: {error}")
            }
        })
}

fn stdout_from(file: &File) -> Result<std::process::Stdio, String> {
    file.try_clone()
        .map(std::process::Stdio::from)
        .map_err(|error| format!("could not retain screenshot file: {error}"))
}

fn prepare_file(file: &mut File) -> Result<(), String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("could not prepare screenshot file: {error}"))?;
    file.set_len(0)
        .map_err(|error| format!("could not prepare screenshot file: {error}"))?;
    Ok(())
}

fn prepare_shot_path(path: &str) -> Result<ClaimedShot, String> {
    claim_new_file(&check_shot_path(path)?)
}

fn check_shot_path(path: &str) -> Result<PathBuf, String> {
    let path = accepted_shot_path(path)?;
    ensure_real_parent(&path)?;
    require_absent_file(path)
}

fn accepted_shot_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path == "-" || path.starts_with('-') {
        return Err("screenshot path is empty or not a file path".into());
    }
    let path = PathBuf::from(path);
    let Some(name) = path.file_name() else {
        return Err("screenshot path is empty or not a file path".into());
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err("screenshot path is empty or not a file path".into());
    }
    Ok(path)
}

fn ensure_real_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        return parent_directory_state(parent);
    }
    Ok(())
}

fn parent_directory_state(parent: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(parent) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err("screenshot directory must not be a symlink".into())
        }
        Ok(meta) if !meta.is_dir() => Err("screenshot directory does not exist".into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("screenshot directory does not exist".into())
        }
        Err(error) => Err(format!("could not inspect screenshot path: {error}")),
    }
}

fn require_absent_file(path: PathBuf) -> Result<PathBuf, String> {
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_dir() => Err("screenshot path is a directory".into()),
        Ok(_) => Err("screenshot path already exists".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(format!("could not inspect screenshot path: {error}")),
    }
}

fn claim_new_file(path: &Path) -> Result<ClaimedShot, String> {
    let dir = open_directory(parent_dir(path))?;
    let name = c_file_name(path)?;
    match open_nofollow(dir.as_raw_fd(), &name) {
        Ok(_) => return Err("screenshot path already exists".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not inspect screenshot path: {error}")),
    }
    let file = create_exclusive_file(dir.as_raw_fd(), &name)?;
    accept_claimed_file(path, name, dir, file)
}

fn create_exclusive_file(dirfd: RawFd, name: &CStr) -> Result<File, String> {
    let fd = unsafe {
        openat(
            dirfd,
            name.as_ptr(),
            O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(exclusive_create_error(std::io::Error::last_os_error()));
    }
    // SAFETY: openat just returned this descriptor, and nothing else owns it.
    Ok(unsafe { File::from(OwnedFd::from_raw_fd(fd)) })
}

fn exclusive_create_error(error: std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return "screenshot path already exists".into();
    }
    format!("could not create screenshot file: {error}")
}

fn accept_claimed_file(
    path: &Path,
    name: CString,
    dir: OwnedFd,
    file: File,
) -> Result<ClaimedShot, String> {
    let meta = file
        .metadata()
        .map_err(|error| format!("could not stat screenshot file: {error}"))?;
    let claimed = ClaimedShot {
        path: path.to_path_buf(),
        name,
        dir,
        file,
        dev: meta.dev(),
        ino: meta.ino(),
    };
    if !meta.file_type().is_file() || meta.nlink() != 1 || meta.uid() != current_euid() {
        claimed.discard();
        return Err("screenshot path was not a new file".into());
    }
    Ok(claimed)
}

fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn c_file_name(path: &Path) -> Result<CString, String> {
    let name = path
        .file_name()
        .ok_or_else(|| "screenshot path is empty or not a file path".to_string())?;
    CString::new(name.as_bytes()).map_err(|_| "screenshot path is not usable".to_string())
}

fn open_directory(path: &Path) -> Result<OwnedFd, String> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "screenshot directory path is not usable".to_string())?;
    let fd = unsafe {
        openat(
            AT_FDCWD,
            c_path.as_ptr(),
            O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(directory_error(&std::io::Error::last_os_error()));
    }
    // SAFETY: openat just returned this descriptor, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn directory_error(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(20) {
        return "screenshot directory does not exist".into();
    }
    // O_NOFOLLOW on a symlink fails with ELOOP.
    if error.raw_os_error() == Some(40) {
        return "screenshot directory must not be a symlink".into();
    }
    format!("could not open screenshot directory: {error}")
}

fn open_nofollow(dirfd: RawFd, name: &CStr) -> Result<OwnedFd, std::io::Error> {
    let fd = io_retry(|| unsafe { openat(dirfd, name.as_ptr(), O_PATH | O_NOFOLLOW | O_CLOEXEC, 0) });
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat just returned this descriptor, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn metadata_fd(fd: &OwnedFd) -> Result<std::fs::Metadata, std::io::Error> {
    let cloned = fd.try_clone()?;
    std::fs::File::from(cloned).metadata()
}

fn unlink_if_same_file(dirfd: RawFd, name: &CStr, inode: Inode) {
    // Never unlink the public name after a separate lstat. That deletes a
    // replacement created between the check and the unlink. Rename the entry
    // aside, then delete the temporary name only if it is still our inode.
    let Ok(opened) = open_nofollow(dirfd, name) else {
        return;
    };
    let Ok(meta) = metadata_fd(&opened) else {
        return;
    };
    if !meta.file_type().is_file() || meta.dev() != inode.dev || meta.ino() != inode.ino {
        return;
    }
    after_matching_inode();
    let tmp = temp_name(&inode);
    if !rename_noreplace(dirfd, name, &tmp) {
        return;
    }
    let displaced_is_ours = open_nofollow(dirfd, &tmp).and_then(|fd| metadata_fd(&fd)).is_ok_and(
        |meta| meta.file_type().is_file() && meta.dev() == inode.dev && meta.ino() == inode.ino,
    );
    if displaced_is_ours {
        let _ = unlink_at(dirfd, &tmp);
        return;
    }
    if open_nofollow(dirfd, name).is_err() {
        let _ = rename_noreplace(dirfd, &tmp, name);
    }
}

fn temp_name(inode: &Inode) -> CString {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    CString::new(format!(
        ".hermes-shot-unlink-{}-{}-{}-{n}-{nanos}",
        std::process::id(),
        inode.dev,
        inode.ino,
    ))
    .expect("unlink temporary name")
}

fn rename_noreplace(dirfd: RawFd, from: &CStr, to: &CStr) -> bool {
    io_retry(|| unsafe { renameat2(dirfd, from.as_ptr(), dirfd, to.as_ptr(), RENAME_NOREPLACE) })
        == 0
}

fn unlink_at(dirfd: RawFd, name: &CStr) -> bool {
    io_retry(|| unsafe { unlinkat(dirfd, name.as_ptr(), 0) }) == 0
}

fn io_retry(mut op: impl FnMut() -> i32) -> i32 {
    loop {
        let rc = op();
        if rc < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return rc;
    }
}

fn current_euid() -> u32 {
    unsafe { geteuid() }
}

#[cfg(test)]
fn after_matching_inode() {
    tests::park_unlink_if_armed();
}

#[cfg(not(test))]
fn after_matching_inode() {}

fn png_dimensions(file: &mut File) -> Result<(u32, u32), String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("screenshot was not written: {error}"))?;
    let mut header = [0u8; 24];
    Read::read_exact(file, &mut header).map_err(|_| "screenshot is not a PNG".to_string())?;
    png_size(&header)
}

fn png_size(header: &[u8; 24]) -> Result<(u32, u32), String> {
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    if header[..8] != MAGIC || &header[12..16] != b"IHDR" {
        return Err("screenshot is not a PNG".into());
    }
    let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
    if width == 0 || height == 0 {
        return Err("screenshot PNG is empty".into());
    }
    Ok((width, height))
}

fn capture_size_matches(
    width: i32,
    height: i32,
    scale: f32,
    png_width: u32,
    png_height: u32,
) -> bool {
    let near = |actual: u32, expected: u32| actual.abs_diff(expected) <= 2;
    let (scaled_w, scaled_h) = expected_pixels(width, height, scale);
    let logical_w = width.max(0) as u32;
    let logical_h = height.max(0) as u32;
    (near(png_width, scaled_w) && near(png_height, scaled_h))
        || (near(png_width, logical_w) && near(png_height, logical_h))
}

fn expected_pixels(width: i32, height: i32, scale: f32) -> (u32, u32) {
    let pixels = |value: i32| (value as f32 * scale).round().max(0.) as u32;
    (pixels(width), pixels(height))
}

fn compositor_clients(flight: &Flight) -> Result<Vec<CompositorWindow>, String> {
    let value = compositor_json("clients", flight)?;
    let rows = value
        .as_array()
        .ok_or("compositor client list was not a JSON array")?;
    rows.iter().map(parse_client).collect()
}

fn compositor_monitors(flight: &Flight) -> Result<Vec<CompositorMonitor>, String> {
    let value = compositor_json("monitors", flight)?;
    let rows = value
        .as_array()
        .ok_or("compositor monitor list was not a JSON array")?;
    rows.iter().map(parse_monitor).collect()
}

fn compositor_json(kind: &str, flight: &Flight) -> Result<serde_json::Value, String> {
    if cancelled(flight) {
        return Err("screenshot cancelled".into());
    }
    let mut child = std::process::Command::new("hyprctl")
        .arg(kind)
        .arg("-j")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "compositor capture needs hyprctl; refusing a synthetic screenshot".into()
            } else {
                format!("compositor query failed: hyprctl could not be started: {error}")
            }
        })?;
    let stdout = match child.stdout.take() {
        Some(pipe) => pipe,
        None => {
            silence(child);
            return Err("compositor query failed: no output".into());
        }
    };
    let stdout_rx = match spawn_reader(stdout, COMPOSITOR_CAP, true) {
        Ok(reader) => reader,
        Err(error) => {
            silence(child);
            return Err(error);
        }
    };
    let stderr_rx = match child.stderr.take() {
        Some(pipe) => match spawn_reader(pipe, STDERR_CAP, false) {
            Ok(reader) => Some(reader),
            Err(error) => {
                silence(child);
                return Err(error);
            }
        },
        None => None,
    };
    install_child(&flight.child, child);
    if cancelled(flight) {
        kill_child(&flight.child);
        reap(&flight.child);
        join_reader(stdout_rx);
        let _ = recv_lossy(stderr_rx);
        return Err("screenshot cancelled".into());
    }
    let end = wait_child(
        &flight.child,
        COMPOSITOR_TIMEOUT,
        &flight.cancel,
        Some(&stdout_rx.flag),
    );
    let stderr = recv_lossy(stderr_rx);
    match end {
        WaitEnd::Cancelled => {
            join_reader(stdout_rx);
            Err("screenshot cancelled".into())
        }
        WaitEnd::TimedOut => {
            join_reader(stdout_rx);
            Err("compositor query timed out".into())
        }
        WaitEnd::Capped => {
            join_reader(stdout_rx);
            Err("compositor query output exceeded limit".into())
        }
        WaitEnd::Failed(error) => {
            join_reader(stdout_rx);
            Err(format!("compositor query failed: {error}"))
        }
        WaitEnd::Exited(status) if !status.success() => {
            join_reader(stdout_rx);
            Err(format!(
                "compositor query failed: {}",
                stderr_text(&stderr)
            ))
        }
        WaitEnd::Exited(_) => {
            let bytes = recv_strict(stdout_rx)?;
            serde_json::from_slice(&bytes)
                .map_err(|_| "compositor query returned invalid JSON".to_string())
        }
    }
}

fn parse_client(value: &serde_json::Value) -> Result<CompositorWindow, String> {
    let (x, y) = json_pair(value, "at")?;
    let (width, height) = json_pair(value, "size")?;
    Ok(CompositorWindow {
        stable_id: json_str(value, "stableId")?.to_string(),
        class: json_str(value, "class")?.to_string(),
        initial_class: value
            .get("initialClass")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        title: json_str(value, "title")?.to_string(),
        pid: json_u32(value, "pid")?,
        mapped: json_bool(value, "mapped")?,
        x,
        y,
        width,
        height,
        monitor: json_i32(value, "monitor")?,
    })
}

fn parse_monitor(value: &serde_json::Value) -> Result<CompositorMonitor, String> {
    Ok(CompositorMonitor {
        id: json_i32(value, "id")?,
        scale: json_f32(value, "scale")?,
    })
}

fn select_window(clients: &[CompositorWindow], pid: u32) -> Result<usize, String> {
    let pid_hits = matching(clients, |client| {
        client.pid == pid && (class_is_ours(client) || client.title == WINDOW_TITLE)
    });
    if pid_hits.len() == 1 {
        return Ok(pid_hits[0]);
    }
    if pid_hits.len() > 1 {
        return Err("compositor window for this process is ambiguous".into());
    }
    Err("compositor has no window for this process".into())
}

fn matching(clients: &[CompositorWindow], pred: impl Fn(&CompositorWindow) -> bool) -> Vec<usize> {
    clients
        .iter()
        .enumerate()
        .filter(|(_, client)| pred(client))
        .map(|(index, _)| index)
        .collect()
}

fn class_is_ours(client: &CompositorWindow) -> bool {
    client.class == APP_ID
        || client.initial_class == APP_ID
        || client.class.eq_ignore_ascii_case(APP_ID)
        || client.initial_class.eq_ignore_ascii_case(APP_ID)
}

fn install_child(slot: &Mutex<Option<std::process::Child>>, child: std::process::Child) {
    *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(child);
}

fn silence(mut child: std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn kill_child(slot: &Mutex<Option<std::process::Child>>) {
    if let Some(child) = slot
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .as_mut()
    {
        let _ = child.kill();
    }
}

fn take_child(slot: &Mutex<Option<std::process::Child>>) -> Option<std::process::Child> {
    slot.lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
}

fn reap(slot: &Mutex<Option<std::process::Child>>) {
    if let Some(mut child) = take_child(slot) {
        let _ = child.wait();
    }
}

fn wait_child(
    slot: &Mutex<Option<std::process::Child>>,
    timeout: Duration,
    cancel: &AtomicBool,
    capped: Option<&AtomicBool>,
) -> WaitEnd {
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::SeqCst) {
            kill_child(slot);
            reap(slot);
            return WaitEnd::Cancelled;
        }
        if capped.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            kill_child(slot);
            reap(slot);
            return WaitEnd::Capped;
        }
        let polled = {
            let mut guard = slot.lock().unwrap_or_else(|poison| poison.into_inner());
            let Some(child) = guard.as_mut() else {
                return WaitEnd::Failed("capture process disappeared".into());
            };
            child.try_wait()
        };
        match polled {
            Ok(Some(status)) => {
                let _ = take_child(slot);
                return WaitEnd::Exited(status);
            }
            Ok(None) if start.elapsed() > timeout => {
                kill_child(slot);
                reap(slot);
                return WaitEnd::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return WaitEnd::Failed(error.to_string()),
        }
    }
}

fn spawn_reader(
    reader: impl Read + Send + 'static,
    cap: usize,
    strict: bool,
) -> Result<CappedRead, String> {
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let flag_reader = std::sync::Arc::clone(&flag);
    std::thread::Builder::new()
        .name("hermes-shot-read".into())
        .spawn(move || {
            let _ = tx.send(read_capped(reader, cap, strict, &flag_reader));
        })
        .map_err(|error| format!("could not read process output: {error}"))?;
    Ok(CappedRead { flag, rx })
}

fn read_capped(
    mut reader: impl Read,
    cap: usize,
    strict: bool,
    flag: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = match reader.read(&mut tmp) {
            Ok(0) => return Ok(buf),
            Ok(n) => n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("could not read process output: {error}")),
        };
        if buf.len() + n > cap {
            if strict {
                flag.store(true, Ordering::SeqCst);
                return Err("output exceeded limit".into());
            }
            let room = cap.saturating_sub(buf.len());
            buf.extend_from_slice(&tmp[..room]);
            drain(&mut reader, &mut tmp);
            return Ok(buf);
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

fn drain(reader: &mut impl Read, tmp: &mut [u8]) {
    loop {
        match reader.read(tmp) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

fn recv_lossy(reader: Option<CappedRead>) -> Vec<u8> {
    let Some(reader) = reader else {
        return Vec::new();
    };
    match reader.rx.recv_timeout(Duration::from_secs(1)) {
        Ok(Ok(bytes)) => bytes,
        _ => Vec::new(),
    }
}

fn recv_strict(reader: CappedRead) -> Result<Vec<u8>, String> {
    match reader.rx.recv_timeout(Duration::from_secs(1)) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) if error == "output exceeded limit" => {
            Err("compositor query output exceeded limit".into())
        }
        Ok(Err(error)) => Err(format!("compositor query failed: {error}")),
        Err(_) => Err("compositor query failed: no output".into()),
    }
}

fn join_reader(reader: CappedRead) {
    let _ = reader.rx.recv_timeout(Duration::from_secs(1));
}


fn json_i32(value: &serde_json::Value, name: &str) -> Result<i32, String> {
    value
        .get(name)
        .and_then(serde_json::Value::as_i64)
        .and_then(|number| i32::try_from(number).ok())
        .ok_or_else(|| format!("compositor record missing {name}"))
}

fn json_u32(value: &serde_json::Value, name: &str) -> Result<u32, String> {
    value
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| format!("compositor record missing {name}"))
}

fn json_f32(value: &serde_json::Value, name: &str) -> Result<f32, String> {
    value
        .get(name)
        .and_then(serde_json::Value::as_f64)
        .map(|number| number as f32)
        .ok_or_else(|| format!("compositor record missing {name}"))
}

fn json_bool(value: &serde_json::Value, name: &str) -> Result<bool, String> {
    value
        .get(name)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| format!("compositor record missing {name}"))
}

fn json_str<'a>(value: &'a serde_json::Value, name: &str) -> Result<&'a str, String> {
    value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("compositor record missing {name}"))
}

fn json_pair(value: &serde_json::Value, name: &str) -> Result<(i32, i32), String> {
    let pair = value
        .get(name)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("compositor record missing {name}"))?;
    if pair.len() != 2 {
        return Err(format!("compositor record {name} is not a pair"));
    }
    let one = pair[0]
        .as_i64()
        .and_then(|number| i32::try_from(number).ok())
        .ok_or_else(|| format!("compositor record {name} is not integral"))?;
    let two = pair[1]
        .as_i64()
        .and_then(|number| i32::try_from(number).ok())
        .ok_or_else(|| format!("compositor record {name} is not integral"))?;
    Ok((one, two))
}

fn text_keystroke(ch: char) -> Keystroke {
    if ch == '\n' || ch == '\r' {
        return Keystroke {
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::none()
            },
            key: "enter".into(),
            key_char: Some("\n".into()),
        };
    }
    if ch == '\t' {
        return Keystroke::parse("tab")
            .unwrap_or_else(|_| Keystroke {
                modifiers: Modifiers::none(),
                key: "tab".into(),
                key_char: Some("\t".into()),
            })
            .with_simulated_ime();
    }
    if ch == ' ' {
        return Keystroke::parse("space")
            .unwrap_or_else(|_| Keystroke {
                modifiers: Modifiers::none(),
                key: "space".into(),
                key_char: Some(" ".into()),
            })
            .with_simulated_ime();
    }
    let mut modifiers = Modifiers::none();
    let mut key = ch.to_string();
    if ch.is_ascii_uppercase() {
        modifiers.shift = true;
        key.make_ascii_lowercase();
    }
    Keystroke {
        modifiers,
        key,
        key_char: Some(ch.to_string()),
    }
}

fn sheet_name(sheet: chrome::Sheet) -> &'static str {
    match sheet {
        chrome::Sheet::None => "none",
        chrome::Sheet::Palette => "palette",
        chrome::Sheet::Rename => "rename",
        chrome::Sheet::Attach => "attach",
        chrome::Sheet::Workspace => "workspace",
    }
}

fn tone_name(tone: Tone) -> &'static str {
    match tone {
        Tone::Neutral => "neutral",
        Tone::Live => "live",
        Tone::Error => "error",
    }
}

fn inspector_tab_name(tab: crate::model::InspectorTab) -> &'static str {
    match tab {
        crate::model::InspectorTab::SideAsk => "side",
        crate::model::InspectorTab::Trace => "trace",
        crate::model::InspectorTab::Outline => "outline",
    }
}

fn filter_name(filter: crate::model::SessionFilter) -> &'static str {
    match filter {
        crate::model::SessionFilter::All => "all",
        crate::model::SessionFilter::Chats => "chats",
        crate::model::SessionFilter::Auto => "auto",
        crate::model::SessionFilter::Pinned => "pinned",
    }
}

fn failed(error: impl Into<String>) -> serde_json::Value {
    serde_json::json!({"ok": false, "error": error.into()})
}

fn shown(text: &str) -> String {
    text.chars().take(80).collect()
}

fn stderr_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "no error output".into();
    }
    let mut owned = trimmed.replace('\n', " ");
    if owned.len() > 180 {
        owned.truncate(180);
    }
    owned
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Mutex, mpsc};
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn vertical_caret_keeps_character_columns_across_utf8_lines() {
        assert_eq!(caret_move("a\né", 1, 1, "down", false), Some((4, 4)));
        assert_eq!(caret_move("éx\nabc", 2, 2, "down", false), Some((5, 5)));
        assert_eq!(caret_move("éx\nabc", 5, 5, "up", true), Some((5, 2)));
    }

    struct UnlinkPark {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    thread_local! {
        static UNLINK_PARK: std::cell::RefCell<Option<UnlinkPark>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn park_unlink_if_armed() {
        let park = UNLINK_PARK.with(|slot| slot.borrow_mut().take());
        if let Some(park) = park {
            let _ = park.entered.send(());
            let _ = park.release.recv_timeout(Duration::from_secs(2));
        }
    }

    struct Temp(PathBuf);

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "hermes-shot-test-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn header(width: u32, height: u32) -> [u8; 24] {
        let mut bytes = [0u8; 24];
        bytes[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']);
        bytes[8..12].copy_from_slice(&13u32.to_be_bytes());
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes
    }

    fn client(pid: u32, class: &str, title: &str) -> CompositorWindow {
        CompositorWindow {
            stable_id: "18000014".into(),
            class: class.into(),
            initial_class: String::new(),
            title: title.into(),
            pid,
            mapped: true,
            x: 0,
            y: 0,
            width: 10,
            height: 20,
            monitor: 0,
        }
    }

    #[test]
    fn rejects_unsafe_or_missing_screenshot_paths() {
        assert!(check_shot_path("").unwrap_err().contains("not a file path"));
        assert!(check_shot_path("-").unwrap_err().contains("not a file path"));
        assert!(check_shot_path("-out.png").unwrap_err().contains("not a file path"));
        let dir = scratch();
        let _keep = Temp(dir.clone());
        assert!(
            check_shot_path(dir.to_str().unwrap())
                .unwrap_err()
                .contains("directory")
        );
        assert!(
            check_shot_path(dir.join("missing").join("a.png").to_str().unwrap())
                .unwrap_err()
                .contains("does not exist")
        );
        let real = dir.join("real");
        std::fs::create_dir(&real).unwrap();
        let linked = dir.join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let err = check_shot_path(linked.join("a.png").to_str().unwrap()).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert!(std::fs::symlink_metadata(real.join("a.png")).is_err());
    }

    #[test]
    fn claim_leaves_existing_files_and_symlinks_untouched() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let path = dir.join("shot.png");
        std::fs::write(&path, b"private").unwrap();
        let err = claim_new_file(&path).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"private");

        let secret = dir.join("secret");
        std::fs::write(&secret, b"keep").unwrap();
        let link = dir.join("link.png");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        let err = claim_new_file(&link).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&secret).unwrap(), b"keep");
    }

    #[test]
    fn stdout_redirect_does_not_overwrite_a_replaced_symlink_or_hardlink() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let path = dir.join("shot.png");
        let mut claimed = claim_new_file(&path).unwrap();
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().mode() & 0o777,
            0o600
        );
        let secret = dir.join("secret");
        std::fs::write(&secret, b"keep-me").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&secret, &path).unwrap();
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("printf 'png-bytes'")
            .stdin(std::process::Stdio::null())
            .stdout(stdout_from(&claimed.file).unwrap())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(std::fs::read(&secret).unwrap(), b"keep-me");
        claimed.file.seek(SeekFrom::Start(0)).unwrap();
        let mut got = String::new();
        std::io::Read::read_to_string(&mut claimed.file, &mut got).unwrap();
        assert_eq!(got, "png-bytes");
        assert!(!claimed.still_ours());
        claimed.discard();
        assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&secret).unwrap(), b"keep-me");

        let other = dir.join("other");
        std::fs::write(&other, b"other").unwrap();
        let hard = dir.join("hard.png");
        let mut claimed = claim_new_file(&hard).unwrap();
        std::fs::remove_file(&hard).unwrap();
        std::fs::hard_link(&other, &hard).unwrap();
        claimed.file.seek(SeekFrom::Start(0)).unwrap();
        claimed.file.write_all(b"nope").unwrap();
        assert_eq!(std::fs::read(&other).unwrap(), b"other");
        assert_eq!(std::fs::read(&hard).unwrap(), b"other");
        claimed.discard();
        assert_eq!(std::fs::read(&hard).unwrap(), b"other");
        assert_eq!(std::fs::read(&other).unwrap(), b"other");
    }

    #[test]
    fn discard_after_a_matching_stat_does_not_delete_a_replacement() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let path = dir.join("shot.png");
        let claimed = claim_new_file(&path).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        UNLINK_PARK.with(|slot| {
            *slot.borrow_mut() = Some(UnlinkPark {
                entered: entered_tx,
                release: release_rx,
            });
        });
        let path_for_replace = path.clone();
        let replacer = std::thread::spawn(move || {
            entered_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("unlink did not reach the inode check");
            std::fs::remove_file(&path_for_replace).unwrap();
            std::fs::write(&path_for_replace, b"replacement").unwrap();
            let inode = std::fs::symlink_metadata(&path_for_replace).unwrap().ino();
            release_tx.send(()).unwrap();
            inode
        });
        claimed.discard();
        let inode = replacer.join().unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(meta.ino(), inode);
        assert_ne!(meta.ino(), claimed.ino);
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    }

    #[test]
    fn png_and_size_failures_are_not_reported_as_captures() {
        assert!(png_size(&[0u8; 24]).unwrap_err().contains("not a PNG"));
        assert!(png_size(&header(0, 4)).unwrap_err().contains("empty"));
        let mut bad = header(2, 2);
        bad[12] = b'X';
        assert!(png_size(&bad).unwrap_err().contains("not a PNG"));

        let dir = scratch();
        let _keep = Temp(dir.clone());
        let path = dir.join("shot.png");
        let mut claimed = claim_new_file(&path).unwrap();
        let err = png_dimensions(&mut claimed.file).unwrap_err();
        assert!(err.contains("not a PNG"), "{err}");

        claimed.file.write_all(&header(8, 8)).unwrap();
        let plan = ShotPlan {
            x: 1,
            y: 2,
            width: 10,
            height: 20,
            scale: 1.0,
            stable_id: "18000014".into(),
            window_active: false,
        };
        let err = accept_png(&mut claimed, &plan).unwrap_err();
        assert!(err.contains("expected 10x20"), "{err}");

        claimed.file.seek(SeekFrom::Start(0)).unwrap();
        claimed.file.set_len(0).unwrap();
        claimed.file.write_all(&header(10, 20)).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"planted").unwrap();
        let err = accept_png(&mut claimed, &plan).unwrap_err();
        assert!(err.contains("replaced"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"planted");
        claimed.discard();
        assert_eq!(std::fs::read(&path).unwrap(), b"planted");
    }

    #[test]
    fn success_reports_the_measured_toplevel_png() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let path = dir.join("shot.png");
        let mut claimed = claim_new_file(&path).unwrap();
        claimed.file.write_all(&header(10, 20)).unwrap();
        let plan = ShotPlan {
            x: 3,
            y: 4,
            width: 10,
            height: 20,
            scale: 2.0,
            stable_id: "18000014".into(),
            window_active: true,
        };
        let value = accept_png(&mut claimed, &plan).unwrap();
        assert_eq!(value["ok"].as_bool(), Some(true));
        assert_eq!(value["capture"].as_str(), Some("toplevel"));
        assert_eq!(value["source"].as_str(), Some("compositor"));
        assert_eq!(value["png_width"].as_u64(), Some(10));
        assert_eq!(value["png_height"].as_u64(), Some(20));
        assert_eq!(value["width"].as_i64(), Some(10));
        let shown = path.display().to_string();
        assert_eq!(value["path"].as_str(), Some(shown.as_str()));
        let args = grim_args("18000014");
        assert_eq!(*args.last().unwrap(), "-");
        assert!(args.contains(&"-T"));
        assert!(!args.iter().any(|arg| arg.contains("shot.png")));
    }

    #[test]
    fn window_selection_rejects_ambiguous_hidden_and_bad_ids() {
        let ours = client(7, APP_ID, "other");
        let foreign = client(8, "nope", "nope");
        assert_eq!(select_window(&[foreign, ours], 7).unwrap(), 1);
        assert!(
            select_window(&[client(8, "nope", "nope")], 7)
                .unwrap_err()
                .contains("no window")
        );
        assert!(
            select_window(&[client(7, APP_ID, "a"), client(7, APP_ID, WINDOW_TITLE)], 7)
                .unwrap_err()
                .contains("ambiguous")
        );
        let mut hidden = client(7, APP_ID, WINDOW_TITLE);
        hidden.mapped = false;
        assert!(window_usable(&hidden).unwrap_err().contains("not mapped"));
        hidden.mapped = true;
        assert!(window_usable(&hidden).is_ok());
        hidden.stable_id = "-flag".into();
        assert!(window_usable(&hidden).unwrap_err().contains("not usable"));
        hidden.stable_id = "18000014".into();
        hidden.width = 0;
        assert!(window_usable(&hidden).unwrap_err().contains("empty"));
        hidden.width = 10;
        assert!(
            capture_scale(&hidden, &[CompositorMonitor { id: 1, scale: 2.0 }])
                .unwrap_err()
                .contains("monitor")
        );
        hidden.monitor = 1;
        assert!(
            capture_scale(&hidden, &[CompositorMonitor { id: 1, scale: f32::NAN }])
                .unwrap_err()
                .contains("unusable")
        );
        let inactive = serde_json::json!({
            "at": [0, 0],
            "size": [1, 1],
            "stableId": "1",
            "class": "c",
            "title": "t",
            "pid": 1,
            "mapped": true,
            "visible": false,
            "monitor": 0
        });
        let parsed = parse_client(&inactive).unwrap();
        assert!(parsed.mapped);
        assert!(window_usable(&parsed).is_ok());
    }

    #[test]
    fn capped_reads_and_waits_fail_closed() {
        let flag = AtomicBool::new(false);
        let err = read_capped(&b"0123456789"[..], 4, true, &flag).unwrap_err();
        assert!(err.contains("exceeded"), "{err}");
        assert!(flag.load(Ordering::SeqCst));
        let flag = AtomicBool::new(false);
        let bytes = read_capped(&b"0123456789"[..], 4, false, &flag).unwrap();
        assert_eq!(bytes, b"0123");

        let slot = Mutex::new(None);
        let child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        install_child(&slot, child);
        let started = Instant::now();
        let end = wait_child(&slot, Duration::from_millis(80), &AtomicBool::new(false), None);
        assert!(matches!(end, WaitEnd::TimedOut), "{end:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(slot.lock().unwrap().is_none());

        let child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        install_child(&slot, child);
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        let end = wait_child(&slot, Duration::from_secs(30), &cancel, None);
        assert!(matches!(end, WaitEnd::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancelled_publish_does_not_keep_or_report_success() {
        let dir = scratch();
        let _keep = Temp(dir.clone());
        let path = dir.join("shot.png");
        let claimed = claim_new_file(&path).unwrap();
        let flight = Flight {
            cancel: AtomicBool::new(true),
            gate: Mutex::new(()),
            child: Mutex::new(None),
        };
        let (tx, rx) = mpsc::channel();
        publish(
            &flight,
            &tx,
            CaptureDone {
                value: serde_json::json!({"ok": true, "capture": "toplevel"}),
                shot: Some(OwnedShot {
                    claimed,
                    keep: false,
                }),
            },
        );
        let reply = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(reply["ok"].as_bool(), Some(false));
        assert_eq!(reply["error"].as_str(), Some("screenshot cancelled"));
        assert!(std::fs::symlink_metadata(&path).is_err());
    }
}
