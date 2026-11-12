use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::backend::{self, Backend};
use crate::inspector::{InspectorAction, InspectorPanel};
use crate::markdown;
use crate::model::*;
use crate::palette::{self, CommandPalette, TriageBar};
use crate::recall::{self, SidebarRowItem, TranscriptFindBar};
use crate::studio::{self, AttachmentStrip, SlashDeck, WorkspacePicker};

struct Ui {
    backend: Option<Backend>,
    state: RefCell<AppState>,
    pending_resumes: RefCell<HashSet<String>>,
    window: adw::ApplicationWindow,
    sidebar: gtk::Box,
    triage: Rc<TriageBar>,
    palette: RefCell<Option<Rc<CommandPalette>>>,
    palette_trigger: gtk::Button,
    branch_chat: gtk::Button,
    find_chat: gtk::Button,
    pin_chat: gtk::Button,
    rename_chat: gtk::Button,
    find_bar: Rc<TranscriptFindBar>,
    inspector: InspectorPanel,
    inspector_toggle: gtk::Button,
    inspector_badge: gtk::Label,
    delete_chat: gtk::Button,
    composer_box: gtk::Box,
    workspace_picker: Rc<WorkspacePicker>,
    slash_deck: Rc<SlashDeck>,
    attachments: Rc<AttachmentStrip>,
    composer_hint: gtk::Label,
    sessions: gio::ListStore,
    selection: gtk::SingleSelection,
    messages: gio::ListStore,
    message_list: gtk::ListView,
    transcript: gtk::ScrolledWindow,
    content: gtk::Stack,
    title: gtk::Label,
    connection: gtk::Label,
    notice: gtk::Label,
    lineage_bar: gtk::Box,
    activity: gtk::Label,
    spinner: gtk::Spinner,
    editor: gtk::TextView,
    model_menu: gtk::MenuButton,
    model_label: gtk::Label,
    model_provider: gtk::Label,
    model_current: Rc<RefCell<Option<ModelIdentity>>>,
    model_popover: gtk::Popover,
    model_search: gtk::SearchEntry,
    model_results: gio::ListStore,
    model_list: gtk::ListView,
    model_selection: gtk::SingleSelection,
    model_status: gtk::Label,
    model_pending: RefCell<Option<PendingModel>>,
    send: gtk::Button,
    stop: gtk::Button,
    new_chat: gtk::Button,
    reconnect: gtk::Button,
    more_sessions: gtk::Button,
    older: gtk::Button,
    latest: gtk::Button,
    readonly_bar: gtk::Box,
    readonly: gtk::Label,
    readonly_fork: gtk::Button,
    interaction: gtk::Box,
    interaction_scroll: gtk::ScrolledWindow,
    rendered_session: RefCell<Option<String>>,
    rendered_revision: Cell<u64>,
    rendered_row_gate: Cell<(bool, bool)>,
    rendered_find_stamp: RefCell<(bool, String, usize, usize)>,
    rendered_lineage: RefCell<Option<LineageStamp>>,
    rendered_interaction: RefCell<Option<(String, String)>>,
    changing_draft: Cell<bool>,
    follow_bottom: Cell<bool>,
    select_created: Cell<bool>,
    suppress_selection: Cell<bool>,
    sequence: Cell<u64>,
    search_debounce_seq: Cell<u64>,
}

type LineageStamp = (
    Option<String>,
    Option<(String, String)>,
    Vec<(String, String)>,
);

type FindRenderStamp = (bool, String, usize, usize);

#[derive(Clone, Debug, PartialEq, Eq)]
struct MessageRowData {
    index: usize,
    message: ChatMessage,
    can_branch: bool,
    can_edit_retry: bool,
    can_regenerate: bool,
    is_match: bool,
    is_active_match: bool,
}

fn row_action_gates(
    conversation: Option<&Conversation>,
    connected: bool,
    creating: bool,
) -> (bool, bool) {
    let Some(c) = conversation else {
        return (false, false);
    };
    let idle = connected && !c.status.busy() && !c.rewinding;
    let can_branch = idle && !creating;
    let writable_idle = idle && c.runtime_id.is_some() && c.read_only_reason.is_none();
    (can_branch, writable_idle)
}

fn find_render_stamp(find: &TranscriptFind) -> FindRenderStamp {
    (
        find.open,
        find.query.clone(),
        find.active_pos,
        find.matches.len(),
    )
}

fn build_message_rows(
    conversation: Option<&Conversation>,
    find: &TranscriptFind,
    connected: bool,
    creating: bool,
) -> Vec<MessageRowData> {
    let Some(c) = conversation else {
        return Vec::new();
    };
    let (can_branch_gate, writable_idle) = row_action_gates(Some(c), connected, creating);
    let (last_user, last_assistant, _) = last_turn_targets(&c.messages);
    c.messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let (is_match, is_active_match) = find.match_state(index);
            MessageRowData {
                index,
                message: message.clone(),
                can_branch: can_branch_gate && is_branchable_message(message),
                can_edit_retry: writable_idle && last_user.as_deref() == Some(message.id.as_str()),
                can_regenerate: writable_idle
                    && last_assistant.as_deref() == Some(message.id.as_str()),
                is_match,
                is_active_match,
            }
        })
        .collect()
}

fn compute_lineage_stamp(state: &AppState) -> LineageStamp {
    let Some(selected) = state.selected.clone() else {
        return (None, None, Vec::new());
    };
    let parent = state.parent_for(&selected);
    let children = state
        .branches_of(&selected)
        .into_iter()
        .map(|s| {
            let title = if s.title.is_empty() {
                "Untitled branch".into()
            } else {
                s.title.clone()
            };
            (s.stored_id.clone(), title)
        })
        .collect();
    (Some(selected), parent, children)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MetaOutcome {
    Unhandled,
    Handled,
    RefreshSessions,
    ResetToLaunch,
}

/// A switch awaiting an explicit confirmation, kept so the retry repeats the same target.
struct PendingModel {
    stored_id: String,
    runtime_id: String,
    target: ModelIdentity,
    prime: Option<ModelIdentity>,
}

/// The activity text a terminal completion collapses to. Non-terminal
/// statuses mean the completion was not a confirmed end.
fn completion_activity(status: &TurnStatus) -> String {
    match status {
        TurnStatus::Complete => String::new(),
        TurnStatus::Interrupted => "Response stopped. Partial text may be incomplete.".into(),
        TurnStatus::Failed => "The server reported a failed response.".into(),
        TurnStatus::Idle
        | TurnStatus::Submitting
        | TurnStatus::Streaming
        | TurnStatus::AwaitingInput
        | TurnStatus::Stopping
        | TurnStatus::Unknown => "Response ended without a confirmed completion.".into(),
    }
}

/// The collapsed trigger caption. Never claims an unconfirmed pair.
fn model_caption(model: &ModelState) -> String {
    match model {
        ModelState::Unknown => "Choose a model".into(),
        ModelState::Confirmed(identity) => identity.model.clone(),
        ModelState::Switching { target, .. } => format!("Switching to {}…", target.model),
        ModelState::Queued { target, .. } => format!("{} · next turn", target.model),
        ModelState::Unresolved(Some(identity)) => format!("{} · unconfirmed", identity.model),
        ModelState::Unresolved(None) => "Model unconfirmed".into(),
    }
}

fn provider_name(identity: &ModelIdentity, catalog: &Catalog) -> String {
    catalog
        .options
        .iter()
        .find(|option| &option.identity == identity)
        .map_or_else(
            || identity.provider.clone(),
            |option| option.provider_name.clone(),
        )
}

/// The provider line under the model. Empty while nothing is known.
fn model_provider_line(model: &ModelState, catalog: &Catalog) -> String {
    match model {
        ModelState::Unknown => "server default".into(),
        ModelState::Confirmed(identity) | ModelState::Unresolved(Some(identity)) => {
            provider_name(identity, catalog)
        }
        ModelState::Switching { target, .. } | ModelState::Queued { target, .. } => {
            provider_name(target, catalog)
        }
        ModelState::Unresolved(None) => "unconfirmed".into(),
    }
}

fn model_tooltip(model: &ModelState, catalog: &Catalog) -> String {
    match model {
        ModelState::Unknown => "Choose the model for this conversation".into(),
        ModelState::Confirmed(identity) => format!(
            "{} · {}\nChanging this affects only this conversation.",
            identity.model,
            provider_name(identity, catalog)
        ),
        ModelState::Switching { target, .. } => format!("Switching to {} …", target.model),
        ModelState::Queued { target, .. } => format!(
            "{} · {}\nApplies from the next message; the running turn keeps its model.",
            target.model,
            provider_name(target, catalog)
        ),
        ModelState::Unresolved(Some(identity)) => format!(
            "{} · {}\nThe last change was never confirmed. Pick again to be sure.",
            identity.model,
            provider_name(identity, catalog)
        ),
        ModelState::Unresolved(None) => {
            "The last change was never confirmed. Pick again to be sure.".into()
        }
    }
}

/// Run `apply` against the conversation owning `runtime_id`, if tracked.
fn with_conversation(
    state: &mut AppState,
    runtime_id: &str,
    apply: impl FnOnce(&mut Conversation),
) -> bool {
    state
        .conversation_for_runtime_mut(runtime_id)
        .is_some_and(|c| {
            apply(c);
            true
        })
}

fn header_title(state: &AppState) -> &str {
    state
        .selected
        .as_ref()
        .and_then(|id| state.sessions.iter().find(|s| &s.stored_id == id))
        .map(|s| s.title.as_str())
        .filter(|title| !title.is_empty())
        .unwrap_or("Hermes Native")
}

fn activity_text(state: &AppState, resume_loading: bool) -> &str {
    if state.pending_launch.is_some() {
        return "Starting conversation…";
    }
    if resume_loading {
        return "Loading server history…";
    }
    state
        .selected
        .as_ref()
        .and_then(|id| state.conversations.get(id))
        .map_or("", |c| c.activity.as_str())
}

fn spinner_active(state: &AppState, resume_loading: bool) -> bool {
    state.pending_launch.is_some()
        || resume_loading
        || state
            .selected
            .as_ref()
            .and_then(|id| state.conversations.get(id))
            .is_some_and(|c| c.status.busy() || c.history_loading || c.rewinding)
}

fn editor_editable(state: &AppState) -> bool {
    match &state.selected {
        None => state.pending_launch.is_none(),
        Some(id) => state
            .conversations
            .get(id)
            .is_some_and(|c| c.read_only_reason.is_none()),
    }
}

fn can_send_in_conversation(c: &Conversation) -> bool {
    let intent = parse_composer_input(&c.draft);
    if is_curation_or_find_intent(&intent) || is_local_studio_intent(&intent) {
        return true;
    }
    if c.runtime_id.is_none() || c.read_only_reason.is_some() || c.rewinding {
        return false;
    }
    can_send_live_intent(c, &intent)
}

fn is_curation_or_find_intent(intent: &ComposerIntent) -> bool {
    matches!(
        intent,
        ComposerIntent::FindInTranscript(_)
            | ComposerIntent::TogglePin
            | ComposerIntent::Rename(_)
            | ComposerIntent::Archive
    )
}

fn is_local_studio_intent(intent: &ComposerIntent) -> bool {
    matches!(
        intent,
        ComposerIntent::AttachImage | ComposerIntent::PickWorkspace(_)
    )
}

fn can_send_live_intent(c: &Conversation, intent: &ComposerIntent) -> bool {
    match intent {
        ComposerIntent::Empty => !c.attachments.is_empty() && !c.status.busy(),
        ComposerIntent::SideQuestion(_) => true,
        ComposerIntent::Compress => !c.status.busy() && c.messages.len() >= 2,
        ComposerIntent::Prompt(_) => can_send_prompt_intent(c),
        other => can_send_timeline_intent(c, other),
    }
}

fn can_send_prompt_intent(c: &Conversation) -> bool {
    if c.attachments.is_empty() {
        c.status != TurnStatus::Stopping
    } else {
        !c.status.busy()
    }
}

fn can_send_timeline_intent(c: &Conversation, intent: &ComposerIntent) -> bool {
    if c.status.busy() {
        return false;
    }
    match intent {
        ComposerIntent::Branch(_) => count_visible_branch_messages(&c.messages) > 0,
        ComposerIntent::Rewind(_) => last_turn_targets(&c.messages).0.is_some(),
        _ => false,
    }
}

fn can_send_on_launch(state: &AppState) -> bool {
    if state.creating || state.pending_launch.is_some() {
        return false;
    }
    let intent = parse_composer_input(&state.launch_draft);
    is_local_studio_intent(&intent)
        || !state.launch_draft.trim().is_empty()
        || !state.launch_attachments.is_empty()
}

fn composer_send_sensitive(state: &AppState, reload_pending: bool) -> bool {
    if state.connection.as_ref() != Some(&ConnectionState::Connected) || reload_pending {
        return false;
    }
    match &state.selected {
        None => can_send_on_launch(state),
        Some(id) => state
            .conversations
            .get(id)
            .is_some_and(can_send_in_conversation),
    }
}

fn composer_hint_text(state: &AppState) -> &'static str {
    let Some(id) = &state.selected else {
        let cmd = state.launch_draft.split_whitespace().next().unwrap_or("");
        return composer_studio_slash_hint(cmd).unwrap_or(
            "Enter to start a new chat · / skills · Ctrl+U image · Ctrl+Shift+W workspace",
        );
    };
    let Some(c) = state.conversations.get(id) else {
        return "Enter to send · Alt+Enter side-ask (/btw) · Ctrl+Shift+F find";
    };
    composer_conversation_hint(c)
}

fn composer_conversation_hint(c: &Conversation) -> &'static str {
    if c.read_only_reason.is_some() {
        return "Read-only transcript · Ctrl+Shift+F find · Ctrl+D fork · Ctrl+P pin";
    }
    if let Some(slash_hint) = composer_slash_hint(c.draft.trim_start()) {
        return slash_hint;
    }
    if c.status.steerable() {
        "Enter to steer active turn · Alt+Enter or /btw side-ask · Ctrl+I Flight Deck"
    } else {
        "Enter to send · / skills & commands · Ctrl+U image · Ctrl+K omnisearch"
    }
}

fn composer_slash_hint(trimmed: &str) -> Option<&'static str> {
    if let Some(hint) = composer_curation_slash_hint(trimmed) {
        return Some(hint);
    }
    if trimmed.starts_with("/btw") || trimmed.starts_with("/ask") {
        return Some("Enter to ask snapshot side question (/btw) · never saved to history");
    }
    if trimmed.starts_with("/branch") || trimmed.starts_with("/fork") {
        return Some("Enter to fork conversation into a new branch (/branch [optional title])");
    }
    if trimmed.starts_with("/undo")
        || trimmed.starts_with("/retry")
        || trimmed.starts_with("/regen")
    {
        return Some("Enter to rewind last turn (/undo drop · /retry edit · /regen re-run)");
    }
    None
}

fn composer_curation_slash_hint(trimmed: &str) -> Option<&'static str> {
    match trimmed.split_whitespace().next().unwrap_or("") {
        "/find" | "/search" => Some("Enter to search within this transcript (/find [query])"),
        "/pin" => Some("Enter to pin or unpin this conversation on the server (/pin)"),
        "/rename" | "/title" => Some("Enter to rename this conversation (/rename <new title>)"),
        "/archive" => Some("Enter to archive this conversation on the server (/archive)"),
        other => composer_studio_slash_hint(other),
    }
}

fn composer_studio_slash_hint(cmd: &str) -> Option<&'static str> {
    match cmd {
        "/compress" | "/compact" => Some("Enter to compact older turns on the server (/compress)"),
        "/attach" | "/image" => {
            Some("Enter to attach an image for multimodal vision (/attach · Ctrl+U)")
        }
        "/workspace" | "/cwd" => {
            Some("Enter to choose or switch remote project directory (/workspace [query])")
        }
        _ => None,
    }
}

fn build_branch_trigger() -> gtk::Button {
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    inner.append(&gtk::Image::from_icon_name(
        "media-playlist-shuffle-symbolic",
    ));
    let label = gtk::Label::new(Some("Branch"));
    label.add_css_class("palette-trigger-label");
    inner.append(&label);
    let button = gtk::Button::new();
    button.set_child(Some(&inner));
    button.add_css_class("flat");
    button.add_css_class("palette-trigger");
    button.set_tooltip_text(Some("Branch conversation into a new thread · Ctrl+D"));
    button.update_property(&[gtk::accessible::Property::Label("Branch conversation")]);
    button.set_visible(false);
    button
}

fn build_readonly_bar() -> (gtk::Box, gtk::Label, gtk::Button) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bar.add_css_class("readonly-notice");
    set_margins(&bar, 12);
    bar.set_visible(false);
    let label = gtk::Label::new(None);
    label.set_wrap(true);
    label.set_selectable(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    let fork = gtk::Button::with_label("⑂ Fork into live chat");
    fork.add_css_class("suggested-action");
    fork.add_css_class("readonly-fork-btn");
    fork.set_valign(gtk::Align::Center);
    fork.set_tooltip_text(Some(
        "Create a writable branch seeded with this saved transcript · Ctrl+D",
    ));
    bar.append(&label);
    bar.append(&fork);
    (bar, label, fork)
}

fn bind_message_row(object: &glib::Object) {
    let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
    let value = item
        .item()
        .and_downcast::<glib::BoxedAnyObject>()
        .expect("message item");
    let data = value.borrow::<MessageRowData>();
    let (speaker_label, row_class, speaker_class) = role_style(&data.message.role);
    let row = gtk::Box::new(gtk::Orientation::Vertical, 10);
    row.add_css_class("message");
    row.add_css_class(row_class);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let speaker = gtk::Label::new(Some(speaker_label));
    speaker.add_css_class("speaker");
    speaker.add_css_class(speaker_class);
    speaker.set_hexpand(true);
    speaker.set_xalign(0.0);
    heading.append(&speaker);
    apply_message_match_chrome(&row, &heading, &data);
    append_message_actions(&heading, &data);
    let copy = icon_button("edit-copy-symbolic", "Copy original message");
    copy.add_css_class("flat");
    copy.add_css_class("message-copy");
    let source = value.clone();
    copy.connect_clicked(move |button| {
        button
            .clipboard()
            .set_text(&source.borrow::<MessageRowData>().message.text);
    });
    heading.append(&copy);
    row.append(&heading);
    row.append(&message_body(&data.message));
    let column = adw::Clamp::builder()
        .maximum_size(860)
        .tightening_threshold(600)
        .child(&row)
        .build();
    item.set_child(Some(&column));
}

fn apply_message_match_chrome(row: &gtk::Box, heading: &gtk::Box, data: &MessageRowData) {
    if !data.is_match {
        return;
    }
    row.add_css_class("message-match");
    let text = if data.is_active_match {
        row.add_css_class("message-match-active");
        "ACTIVE MATCH"
    } else {
        "MATCH"
    };
    let badge = gtk::Label::new(Some(text));
    badge.add_css_class("source-badge");
    badge.add_css_class("match-badge");
    badge.set_valign(gtk::Align::Center);
    heading.append(&badge);
}

fn append_message_actions(heading: &gtk::Box, data: &MessageRowData) {
    if data.can_edit_retry {
        let btn = message_pill_button(
            "↺ Edit & retry",
            "Rewind this turn and load your prompt into the composer",
        );
        btn.connect_clicked(|b| {
            let _ = b.activate_action("win.rewind", Some(&"edit".to_variant()));
        });
        heading.append(&btn);
    }
    if data.can_regenerate {
        let btn = message_pill_button(
            "↻ Regenerate",
            "Rewind this turn and generate a fresh response",
        );
        btn.connect_clicked(|b| {
            let _ = b.activate_action("win.rewind", Some(&"regen".to_variant()));
        });
        heading.append(&btn);
    }
    if data.can_branch {
        let idx = data.index as u32;
        let btn = message_pill_button(
            "⑂ Branch",
            "Fork a new conversation starting up through this message",
        );
        btn.connect_clicked(move |b| {
            let _ = b.activate_action("win.branch-at", Some(&idx.to_variant()));
        });
        heading.append(&btn);
    }
}

fn message_pill_button(label: &str, tooltip: &str) -> gtk::Button {
    let btn = gtk::Button::with_label(label);
    btn.add_css_class("flat");
    btn.add_css_class("message-action-btn");
    btn.set_tooltip_text(Some(tooltip));
    btn
}

fn build_inspector_trigger() -> (gtk::Button, gtk::Label) {
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    inner.append(&gtk::Image::from_icon_name(
        "utilities-system-monitor-symbolic",
    ));
    let badge = gtk::Label::new(Some("Deck"));
    badge.add_css_class("inspector-trigger-badge");
    inner.append(&badge);
    let button = gtk::Button::new();
    button.set_child(Some(&inner));
    button.add_css_class("flat");
    button.add_css_class("inspector-trigger");
    button.set_tooltip_text(Some(
        "Toggle Flight Deck: /btw Side-Ask, Live Trace & Outline · Ctrl+I",
    ));
    button.update_property(&[gtk::accessible::Property::Label("Flight Deck inspector")]);
    (button, badge)
}

fn build_lineage_bar() -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class("lineage-bar");
    set_margins(&bar, 12);
    bar.set_margin_bottom(4);
    bar.set_visible(false);
    bar
}

fn build_workspace_split(main: &gtk::Box, inspector_root: &gtk::Box) -> gtk::Paned {
    let split = gtk::Paned::new(gtk::Orientation::Horizontal);
    split.set_start_child(Some(main));
    split.set_end_child(Some(inspector_root));
    split.set_resize_start_child(true);
    split.set_resize_end_child(false);
    split.set_shrink_start_child(true);
    split.set_shrink_end_child(true);
    split.set_wide_handle(true);
    split
}

pub fn build(app: &adw::Application, config: Result<Config, String>) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Hermes Native")
        .default_width(1120)
        .default_height(800)
        .width_request(640)
        .height_request(480)
        .build();
    let header = adw::HeaderBar::new();
    let title = gtk::Label::new(Some("Hermes Native"));
    title.add_css_class("title");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    header.set_title_widget(Some(&title));
    let toggle_sidebar = icon_button("sidebar-show-symbolic", "Toggle conversations · Ctrl+B");
    let palette_trigger = palette::build_palette_trigger();
    let branch_chat = build_branch_trigger();
    header.pack_start(&toggle_sidebar);
    header.pack_start(&palette_trigger);
    header.pack_start(&branch_chat);
    let connection = gtk::Label::new(Some("Connecting…"));
    connection.add_css_class("connection-label");
    let (inspector_toggle, inspector_badge) = build_inspector_trigger();
    let reconnect = icon_button(
        "view-refresh-symbolic",
        "Reconnect and reload server history",
    );
    let delete_chat = icon_button("user-trash-symbolic", "Delete conversation");
    delete_chat.add_css_class("flat");
    delete_chat.set_visible(false);
    let rename_chat = icon_button("document-edit-symbolic", "Rename conversation · F2");
    rename_chat.add_css_class("flat");
    rename_chat.set_visible(false);
    let pin_chat = icon_button("non-starred-symbolic", "Pin conversation · Ctrl+P");
    pin_chat.add_css_class("flat");
    pin_chat.set_visible(false);
    let find_chat = icon_button("edit-find-symbolic", "Find in transcript · Ctrl+Shift+F");
    find_chat.add_css_class("flat");
    find_chat.set_visible(false);
    header.pack_end(&inspector_toggle);
    header.pack_end(&reconnect);
    header.pack_end(&delete_chat);
    header.pack_end(&rename_chat);
    header.pack_end(&pin_chat);
    header.pack_end(&find_chat);
    header.pack_end(&connection);

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar.add_css_class("hermes-sidebar");
    sidebar.set_width_request(230);
    set_margins(&sidebar, 14);
    let brand = gtk::Label::new(Some("HERMES"));
    brand.add_css_class("hermes-brand");
    brand.set_xalign(0.0);
    let caption = gtk::Label::new(Some("REMOTE CONVERSATIONS"));
    caption.add_css_class("hermes-caption");
    caption.set_xalign(0.0);
    sidebar.append(&brand);
    sidebar.append(&caption);
    let new_chat = text_icon_button("list-add-symbolic", "New conversation");
    new_chat.set_tooltip_text(Some("Create a server-side conversation · Ctrl+N"));
    new_chat.add_css_class("suggested-action");
    sidebar.append(&new_chat);
    let triage = TriageBar::new();
    sidebar.append(&triage.container);
    sidebar.append(&triage.empty_label);
    let sessions = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(sessions.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let session_factory = gtk::SignalListItemFactory::new();
    session_factory.connect_bind(|_, object| recall::bind_sidebar_row(object));
    let session_list = gtk::ListView::new(Some(selection.clone()), Some(session_factory));
    // This property selects the row under the pointer. Leave it off so hover
    // cannot resume a conversation; clicks and arrow keys still select.
    session_list.set_single_click_activate(false);
    session_list.add_css_class("navigation-sidebar");
    let session_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&session_list)
        .build();
    sidebar.append(&session_scroll);
    let more_sessions = gtk::Button::with_label("Load more conversations");
    more_sessions.add_css_class("flat");
    more_sessions.set_visible(false);
    sidebar.append(&more_sessions);

    let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main.set_hexpand(true);
    main.add_css_class("hermes-transcript");
    let notice = gtk::Label::new(None);
    notice.set_wrap(true);
    notice.set_selectable(true);
    notice.set_xalign(0.0);
    notice.add_css_class("readonly-notice");
    set_margins(&notice, 12);
    notice.set_visible(false);
    main.append(&notice);
    let lineage_bar = build_lineage_bar();
    main.append(&lineage_bar);
    let (readonly_bar, readonly, readonly_fork) = build_readonly_bar();
    main.append(&readonly_bar);
    let history_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    history_bar.set_halign(gtk::Align::Center);
    let older = gtk::Button::with_label("Load earlier messages");
    older.add_css_class("flat");
    let latest = gtk::Button::with_label("Reload history");
    latest.set_tooltip_text(Some("Reload this conversation from the server"));
    latest.add_css_class("flat");
    history_bar.append(&older);
    history_bar.append(&latest);
    main.append(&history_bar);
    let find_bar = TranscriptFindBar::new();
    main.append(&find_bar.root);

    let messages = gio::ListStore::new::<glib::BoxedAnyObject>();
    let message_factory = gtk::SignalListItemFactory::new();
    message_factory.connect_bind(|_, object| bind_message_row(object));
    let message_list = gtk::ListView::new(
        Some(gtk::NoSelection::new(Some(messages.clone()))),
        Some(message_factory),
    );
    message_list.set_margin_start(24);
    message_list.set_margin_end(24);
    message_list.set_margin_bottom(24);
    message_list.set_valign(gtk::Align::Start);
    let transcript = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&message_list)
        .build();
    let content = gtk::Stack::new();
    content.set_vexpand(true);
    content.add_named(&transcript, Some("messages"));
    let empty = gtk::Box::new(gtk::Orientation::Vertical, 14);
    empty.set_valign(gtk::Align::Center);
    empty.set_halign(gtk::Align::Center);
    set_margins(&empty, 24);
    let empty_icon = gtk::Image::from_icon_name("mail-send-symbolic");
    empty_icon.set_pixel_size(40);
    empty_icon.add_css_class("dim-label");
    empty.append(&empty_icon);
    let empty_title = gtk::Label::new(Some("A little space to think."));
    empty_title.add_css_class("empty-title");
    empty_title.set_wrap(true);
    empty.append(&empty_title);
    let empty_subtitle = gtk::Label::new(Some(
        "Start typing below to launch a new conversation,\nor press Ctrl+K to jump anywhere on your Hermes server.",
    ));
    empty_subtitle.set_wrap(true);
    empty_subtitle.set_justify(gtk::Justification::Center);
    empty_subtitle.add_css_class("empty-subtitle");
    empty.append(&empty_subtitle);
    empty.append(&palette::build_shortcut_hints());
    content.add_named(&empty, Some("empty"));
    content.set_visible_child_name("empty");
    main.append(&content);

    let footer = gtk::Box::new(gtk::Orientation::Vertical, 10);
    set_margins(&footer, 20);
    let interaction = gtk::Box::new(gtk::Orientation::Vertical, 8);
    interaction.add_css_class("interaction-panel");
    let interaction_scroll = gtk::ScrolledWindow::builder()
        .child(&interaction)
        .max_content_height(280)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .visible(false)
        .build();
    interaction_scroll.add_css_class("interaction-scroll");
    footer.append(&interaction_scroll);
    let activity_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let spinner = gtk::Spinner::new();
    activity_row.append(&spinner);
    let activity = gtk::Label::new(None);
    activity.set_xalign(0.0);
    activity.set_wrap(true);
    activity.set_selectable(true);
    activity.add_css_class("activity-label");
    activity_row.append(&activity);
    footer.append(&activity_row);
    let slash_deck = SlashDeck::new();
    footer.append(&slash_deck.revealer);
    let composer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    composer.add_css_class("hermes-composer");
    let attachments = AttachmentStrip::new();
    composer.append(&attachments.root);
    let editor = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .top_margin(5)
        .bottom_margin(5)
        .left_margin(5)
        .right_margin(5)
        .build();
    editor.add_css_class("composer-input");
    editor.update_property(&[gtk::accessible::Property::Label("Message")]);
    let editor_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(60)
        .max_content_height(160)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&editor)
        .build();
    composer.append(&editor_scroll);
    let model_menu = gtk::MenuButton::new();
    model_menu.add_css_class("flat");
    model_menu.add_css_class("model-trigger");
    model_menu.update_property(&[gtk::accessible::Property::Label("Model")]);
    let trigger = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let model_label = gtk::Label::new(Some("Choose a model"));
    model_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    model_label.set_max_width_chars(38);
    model_label.set_xalign(0.0);
    model_label.add_css_class("model-caption");
    trigger.append(&model_label);
    let model_provider = gtk::Label::new(Some("server default"));
    model_provider.set_ellipsize(gtk::pango::EllipsizeMode::End);
    model_provider.set_max_width_chars(38);
    model_provider.set_xalign(0.0);
    model_provider.add_css_class("model-provider");
    trigger.append(&model_provider);
    model_menu.set_child(Some(&trigger));

    let model_popover = gtk::Popover::new();
    model_popover.add_css_class("model-popover");
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 8);
    set_margins(&panel, 10);
    panel.set_width_request(380);
    let model_search = gtk::SearchEntry::new();
    model_search.set_placeholder_text(Some("Search provider or model"));
    model_search.update_property(&[gtk::accessible::Property::Label("Search models")]);
    panel.append(&model_search);
    let model_status = gtk::Label::new(Some("Loading models…"));
    model_status.set_xalign(0.0);
    model_status.set_wrap(true);
    model_status.add_css_class("picker-status");
    panel.append(&model_status);
    let model_results = gio::ListStore::new::<glib::BoxedAnyObject>();
    let model_selection = gtk::SingleSelection::new(Some(model_results.clone()));
    model_selection.set_autoselect(false);
    model_selection.set_can_unselect(true);
    let model_factory = gtk::SignalListItemFactory::new();
    let current_model = Rc::new(RefCell::new(None::<ModelIdentity>));
    let current_for_factory = Rc::clone(&current_model);
    model_factory.connect_bind(move |_, object| {
        bind_model_row(current_for_factory.as_ref(), object);
    });
    let model_list = gtk::ListView::new(Some(model_selection.clone()), Some(model_factory));
    // Hover must never choose a model; only an explicit activation commits.
    model_list.set_single_click_activate(false);
    model_list.add_css_class("model-list");
    let model_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(200)
        .max_content_height(320)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&model_list)
        .build();
    panel.append(&model_scroll);
    model_popover.set_child(Some(&panel));
    model_menu.set_popover(Some(&model_popover));
    let workspace_picker = WorkspacePicker::new();
    let model_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    model_row.append(&model_menu);
    model_row.append(&workspace_picker.button);
    let model_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    model_spacer.set_hexpand(true);
    model_row.append(&model_spacer);
    model_row.append(&attachments.attach_btn);
    composer.append(&model_row);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let composer_hint = gtk::Label::new(Some("Enter to send · Shift+Enter for a new line"));
    composer_hint.set_xalign(0.0);
    composer_hint.set_hexpand(true);
    composer_hint.set_ellipsize(gtk::pango::EllipsizeMode::End);
    composer_hint.add_css_class("composer-hint");
    actions.append(&composer_hint);
    let stop = icon_button("media-playback-stop-symbolic", "Stop response");
    actions.append(&stop);
    let send = text_icon_button("mail-send-symbolic", "Send");
    send.add_css_class("suggested-action");
    actions.append(&send);
    composer.append(&actions);
    footer.append(&composer);
    let footer_column = adw::Clamp::builder()
        .maximum_size(900)
        .tightening_threshold(640)
        .child(&footer)
        .build();
    main.append(&footer_column);

    let inspector = InspectorPanel::new();
    let workspace_split = build_workspace_split(&main, &inspector.root);
    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_start_child(Some(&sidebar));
    paned.set_end_child(Some(&workspace_split));
    paned.set_position(285);
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(true);
    paned.set_wide_handle(true);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&paned));
    window.set_content(Some(&toolbar));
    let sidebar_for_toggle = sidebar.clone();
    toggle_sidebar.connect_clicked(move |_| {
        sidebar_for_toggle.set_visible(!sidebar_for_toggle.is_visible());
    });

    let (backend, initial_notice) = match config {
        Ok(config) => (Some(backend::spawn(config)), String::new()),
        Err(error) => (None, error),
    };
    let ui = Rc::new(Ui {
        backend,
        state: RefCell::new(AppState {
            notice: initial_notice,
            ..Default::default()
        }),
        pending_resumes: RefCell::new(HashSet::new()),
        window,
        sidebar,
        triage,
        palette: RefCell::new(None),
        palette_trigger,
        branch_chat,
        find_chat,
        pin_chat,
        rename_chat,
        find_bar,
        inspector,
        inspector_toggle,
        inspector_badge,
        delete_chat,
        composer_box: composer,
        workspace_picker,
        slash_deck,
        attachments,
        composer_hint,
        sessions,
        selection,
        messages,
        message_list,
        transcript,
        content,
        title,
        connection,
        notice,
        lineage_bar,
        activity,
        spinner,
        editor,
        model_menu,
        model_label,
        model_provider,
        model_current: current_model,
        model_popover,
        model_search,
        model_results,
        model_list,
        model_selection,
        model_status,
        model_pending: RefCell::new(None),
        send,
        stop,
        new_chat,
        reconnect,
        more_sessions,
        older,
        latest,
        readonly_bar,
        readonly,
        readonly_fork,
        interaction,
        interaction_scroll,
        rendered_session: RefCell::new(None),
        rendered_revision: Cell::new(u64::MAX),
        rendered_row_gate: Cell::new((false, false)),
        rendered_find_stamp: RefCell::new((false, String::new(), 0, 0)),
        rendered_lineage: RefCell::new(None),
        rendered_interaction: RefCell::new(None),
        changing_draft: Cell::new(false),
        follow_bottom: Cell::new(true),
        select_created: Cell::new(false),
        suppress_selection: Cell::new(false),
        sequence: Cell::new(0),
        search_debounce_seq: Cell::new(0),
    });
    let pointer_row_click = Rc::new(Cell::new(false));
    connect_actions(&ui, &session_list, &pointer_row_click);
    connect_workspace(&ui);
    ui.refresh_sessions();
    ui.refresh();
    ui.window.present();
    ui.editor.grab_focus();
    let weak = Rc::downgrade(&ui);
    // The window owns this strong reference until close. Callbacks otherwise use weak references.
    let keep_alive = RefCell::new(Some(ui.clone()));
    ui.window.connect_close_request(move |_| {
        if let Some(ui) = keep_alive.borrow_mut().take()
            && let Some(backend) = &ui.backend
        {
            let _ = backend.send(Command::Shutdown);
        }
        glib::Propagation::Proceed
    });
    glib::timeout_add_local(Duration::from_millis(60), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !ui.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        let mut changed = false;
        if let Some(backend) = &ui.backend {
            for _ in 0..512 {
                let Some(event) = backend.try_recv() else {
                    break;
                };
                ui.apply(event);
                changed = true;
            }
        }
        if changed {
            ui.refresh();
        }
        glib::ControlFlow::Continue
    });
}

fn bind_model_row(current: &RefCell<Option<ModelIdentity>>, object: &glib::Object) {
    let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
    let value = item
        .item()
        .and_downcast::<glib::BoxedAnyObject>()
        .expect("model item");
    let option = value.borrow::<ModelOption>();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("model-row");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    text.set_hexpand(true);
    let model = gtk::Label::new(Some(&option.identity.model));
    model.set_xalign(0.0);
    model.set_ellipsize(gtk::pango::EllipsizeMode::End);
    model.add_css_class("model-name");
    text.append(&model);
    let provider = gtk::Label::new(Some(&option.provider_name));
    provider.set_xalign(0.0);
    provider.set_ellipsize(gtk::pango::EllipsizeMode::End);
    provider.add_css_class("model-provider");
    text.append(&provider);
    row.append(&text);
    if !option.selectable() {
        let note = gtk::Label::new(Some("unavailable"));
        note.add_css_class("model-provider");
        row.append(&note);
    }
    if current
        .borrow()
        .as_ref()
        .is_some_and(|shown| *shown == option.identity)
    {
        row.add_css_class("model-current");
        row.append(&gtk::Image::from_icon_name("object-select-symbolic"));
    }
    item.set_child(Some(&row));
}

fn connect_model_picker(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    ui.model_search.connect_search_changed(move |_| {
        weak.upgrade().inspect(|ui| ui.refresh_model_results());
    });
    let weak = Rc::downgrade(ui);
    ui.model_search.connect_activate(move |_| {
        weak.upgrade().inspect(|ui| ui.commit_model_selection());
    });
    let weak = Rc::downgrade(ui);
    ui.model_search.connect_stop_search(move |_| {
        weak.upgrade().inspect(|ui| {
            ui.model_popover.popdown();
            ui.editor.grab_focus();
        });
    });
    let arrows = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    arrows.connect_key_pressed(move |_, key, _, _| model_arrow(weak.upgrade().as_ref(), key));
    ui.model_search.add_controller(arrows);
    let weak = Rc::downgrade(ui);
    ui.model_menu
        .connect_notify_local(Some("active"), move |menu, _| {
            open_model_picker(weak.upgrade().as_ref(), menu.is_active());
        });
    let weak = Rc::downgrade(ui);
    ui.model_results.connect_items_changed(move |_, _, _, _| {
        weak.upgrade().inspect(|ui| select_first_model(ui));
    });
    let weak = Rc::downgrade(ui);
    ui.model_list.connect_activate(move |_, _| {
        weak.upgrade().inspect(|ui| ui.commit_model_selection());
    });
    let weak = Rc::downgrade(ui);
    ui.model_popover.connect_show(move |_| {
        weak.upgrade().inspect(|ui| {
            ui.refresh_model_results();
            ui.model_search.grab_focus();
        });
    });
}

fn model_arrow(ui: Option<&Rc<Ui>>, key: gdk::Key) -> glib::Propagation {
    let step = match key {
        gdk::Key::Down | gdk::Key::KP_Down => 1,
        gdk::Key::Up | gdk::Key::KP_Up => -1,
        _ => return glib::Propagation::Proceed,
    };
    let Some(ui) = ui else {
        return glib::Propagation::Proceed;
    };
    move_model_highlight(ui, step);
    glib::Propagation::Stop
}

fn move_model_highlight(ui: &Ui, step: i64) {
    let count = i64::from(ui.model_selection.n_items());
    if count == 0 {
        return;
    }
    let current = ui.model_selection.selected();
    let current = if current == gtk::INVALID_LIST_POSITION {
        0
    } else {
        i64::from(current)
    };
    let next = (current + step).clamp(0, count - 1);
    ui.model_selection.set_selected(next as u32);
    ui.model_list
        .scroll_to(next as u32, gtk::ListScrollFlags::NONE, None);
}

fn open_model_picker(ui: Option<&Rc<Ui>>, active: bool) {
    if !active {
        return;
    }
    let Some(ui) = ui else {
        return;
    };
    ui.model_search.set_text("");
    ui.refresh_model_results();
    ui.model_search.grab_focus();
}

fn select_first_model(ui: &Ui) {
    if ui.model_selection.n_items() > 0
        && ui.model_selection.selected() == gtk::INVALID_LIST_POSITION
    {
        ui.model_selection.set_selected(0);
    }
}

fn connect_actions(ui: &Rc<Ui>, session_list: &gtk::ListView, pointer_row_click: &Rc<Cell<bool>>) {
    let respond = gio::SimpleAction::new(
        "respond",
        Some(glib::VariantTy::new("(ssv)").expect("static variant type")),
    );
    let weak = Rc::downgrade(ui);
    respond.connect_activate(move |_, parameter| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let Some((runtime_id, request_id, body)) =
            parameter.and_then(|p| p.get::<(String, String, glib::Variant)>())
        else {
            return;
        };
        let Some(answer) = decode_interaction_answer(&body) else {
            return;
        };
        let pending = {
            let state = ui.state.borrow();
            state.stored_for_runtime(&runtime_id).filter(|id| {
                state
                    .conversations
                    .get(id)
                    .is_some_and(|conversation| conversation.accepts_response(&request_id))
            })
        };
        let Some(stored_id) = pending else {
            return;
        };
        if ui.command(Command::Respond {
            runtime_id,
            request_id,
            answer,
        }) && let Some(conversation) = ui.state.borrow_mut().conversations.get_mut(&stored_id)
        {
            conversation.response_sent();
        }
        ui.refresh();
    });
    ui.window.add_action(&respond);
    let weak = Rc::downgrade(ui);
    let clicking = Rc::clone(pointer_row_click);
    session_list.connect_activate(move |_, position| {
        // GTK 4.24 claims the double-click release in the row gesture, so bubble never clears it.
        clicking.set(false);
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.suppress_selection.get() {
            return;
        }
        // Enter and double-click. A single click selects without activating.
        ui.activate_sidebar_position(position, true);
    });
    watch_sidebar_clicks(session_list, pointer_row_click, ui);
    let weak = Rc::downgrade(ui);
    let clicking = Rc::clone(pointer_row_click);
    ui.selection.connect_selected_notify(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        // Programmatic splices and set_selected must not be treated as a click.
        if ui.suppress_selection.get() {
            return;
        }
        ui.adopt_sidebar_selection(clicking.replace(false));
    });
    let weak = Rc::downgrade(ui);
    ui.new_chat.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.create();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.send.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.submit();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.reconnect.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.trigger_reconnect());
    });
    let weak = Rc::downgrade(ui);
    ui.stop.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let runtime = {
            let state = ui.state.borrow();
            state
                .selected
                .as_ref()
                .and_then(|id| state.conversations.get(id))
                .and_then(|c| c.runtime_id.clone())
        };
        if let Some(runtime_id) = runtime {
            ui.command(Command::Interrupt { runtime_id });
        }
    });
    let weak = Rc::downgrade(ui);
    ui.more_sessions.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.load_more_sessions());
    });
    let weak = Rc::downgrade(ui);
    ui.older.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let request = {
            let mut state = ui.state.borrow_mut();
            let selected = state.selected.clone();
            selected.and_then(|id| {
                state
                    .conversations
                    .get_mut(&id)
                    .and_then(|c| c.begin_history_page().map(|before| (id, before)))
            })
        };
        if let Some((stored_id, before)) = request {
            ui.follow_bottom.set(false);
            if !ui.command(Command::History {
                stored_id: stored_id.clone(),
                before,
            }) && let Some(conversation) =
                ui.state.borrow_mut().conversations.get_mut(&stored_id)
            {
                conversation.history_loading = false;
            }
            ui.refresh();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.latest.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.reload_current());
    });
    let weak = Rc::downgrade(ui);
    ui.editor.buffer().connect_changed(move |buffer| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.changing_draft.get() {
            return;
        }
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string();
        ui.state.borrow_mut().set_active_draft(text);
        ui.refresh_controls();
    });
    let key = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    key.connect_key_pressed(move |_, key, _, modifiers| {
        handle_composer_key(weak.upgrade().as_ref(), key, modifiers)
    });
    ui.editor.add_controller(key);
    connect_model_picker(ui);
    let adjustment = ui.transcript.vadjustment();
    let weak = Rc::downgrade(ui);
    adjustment.connect_value_changed(move |adjustment| {
        if let Some(ui) = weak.upgrade() {
            ui.follow_bottom
                .set(adjustment.value() + adjustment.page_size() >= adjustment.upper() - 64.0);
        }
    });
}

fn handle_composer_key(
    ui: Option<&Rc<Ui>>,
    key: gdk::Key,
    modifiers: gdk::ModifierType,
) -> glib::Propagation {
    if (key != gdk::Key::Return && key != gdk::Key::KP_Enter)
        || modifiers.contains(gdk::ModifierType::SHIFT_MASK)
    {
        return glib::Propagation::Proceed;
    }
    if let Some(ui) = ui {
        if modifiers.contains(gdk::ModifierType::ALT_MASK) {
            ui.submit_composer_as_side();
        } else {
            ui.submit();
        }
    }
    glib::Propagation::Stop
}

fn connect_workspace(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    let palette = CommandPalette::new(Rc::new(move |action| {
        let _ = weak.upgrade().map(|ui| ui.run_palette_action(action));
    }));
    let weak = Rc::downgrade(ui);
    palette.connect_query(move |palette| {
        if let Some(ui) = weak.upgrade() {
            let query = palette.query_text();
            ui.queue_remote_search(&query);
            palette.populate(&ui.state.borrow());
        }
    });
    *ui.palette.borrow_mut() = Some(palette);

    let weak = Rc::downgrade(ui);
    ui.palette_trigger.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.toggle_palette());
    });
    let weak = Rc::downgrade(ui);
    ui.delete_chat.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.confirm_delete_current());
    });
    connect_inspector(ui);
    connect_timeline(ui);
    connect_recall_and_curation(ui);
    connect_studio(ui);

    let weak_filter = Rc::downgrade(ui);
    let weak_query = Rc::downgrade(ui);
    let weak_esc = Rc::downgrade(ui);
    ui.triage.connect(
        move |filter| {
            let _ = weak_filter.upgrade().map(|ui| {
                ui.state.borrow_mut().session_filter = filter;
                ui.refresh_sessions();
            });
        },
        move |query| {
            let _ = weak_query.upgrade().map(|ui| {
                ui.state.borrow_mut().session_query = query.clone();
                ui.queue_remote_search(&query);
                ui.refresh_sessions();
            });
        },
        move || {
            let _ = weak_esc.upgrade().map(|ui| {
                ui.editor.grab_focus();
            });
        },
    );

    let shortcuts = gtk::EventControllerKey::new();
    shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ui);
    shortcuts.connect_key_pressed(move |_, key, _, modifiers| {
        weak.upgrade().map_or(glib::Propagation::Proceed, |ui| {
            handle_window_shortcut(&ui, key, modifiers)
        })
    });
    ui.window.add_controller(shortcuts);
}

fn connect_inspector(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    ui.inspector_toggle.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.toggle_inspector());
    });
    let weak = Rc::downgrade(ui);
    ui.inspector.connect(move |action| {
        let _ = weak.upgrade().map(|ui| ui.handle_inspector_action(action));
    });
}

fn connect_timeline(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    ui.branch_chat.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.branch_from(None, None));
    });
    let weak = Rc::downgrade(ui);
    ui.readonly_fork.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.branch_from(None, None));
    });
    register_branch_action(ui);
    register_rewind_action(ui);
    register_open_session_action(ui);
}

fn register_branch_action(ui: &Rc<Ui>) {
    let action = gio::SimpleAction::new(
        "branch-at",
        Some(glib::VariantTy::new("u").expect("static variant type")),
    );
    let weak = Rc::downgrade(ui);
    action.connect_activate(move |_, param| {
        if let Some(index) = param.and_then(|p| p.get::<u32>())
            && let Some(ui) = weak.upgrade()
        {
            ui.branch_from(Some(index as usize), None);
        }
    });
    ui.window.add_action(&action);
}

fn register_rewind_action(ui: &Rc<Ui>) {
    let action = gio::SimpleAction::new(
        "rewind",
        Some(glib::VariantTy::new("s").expect("static variant type")),
    );
    let weak = Rc::downgrade(ui);
    action.connect_activate(move |_, param| {
        if let Some(mode) = param
            .and_then(|p| p.get::<String>())
            .and_then(|s| parse_rewind_action(&s))
            && let Some(ui) = weak.upgrade()
        {
            ui.rewind_last(mode);
        }
    });
    ui.window.add_action(&action);
}

fn register_open_session_action(ui: &Rc<Ui>) {
    let action = gio::SimpleAction::new(
        "open-session",
        Some(glib::VariantTy::new("s").expect("static variant type")),
    );
    let weak = Rc::downgrade(ui);
    action.connect_activate(move |_, param| {
        if let Some(id) = param.and_then(|p| p.get::<String>())
            && let Some(ui) = weak.upgrade()
        {
            ui.select_created.set(false);
            ui.select(&id, true);
        }
    });
    ui.window.add_action(&action);
}

fn parse_rewind_action(kind: &str) -> Option<RewindMode> {
    match kind {
        "undo" => Some(RewindMode::Undo),
        "edit" => Some(RewindMode::EditRetry),
        "regen" => Some(RewindMode::Regenerate),
        _ => None,
    }
}

fn connect_recall_and_curation(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    ui.find_chat.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.open_transcript_find(None));
    });
    let weak = Rc::downgrade(ui);
    ui.pin_chat.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.toggle_pin_current());
    });
    let weak = Rc::downgrade(ui);
    ui.rename_chat.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.prompt_rename_current());
    });
    let weak_q = Rc::downgrade(ui);
    let weak_step = Rc::downgrade(ui);
    let weak_older = Rc::downgrade(ui);
    let weak_close = Rc::downgrade(ui);
    ui.find_bar.connect(
        move |query| {
            let _ = weak_q
                .upgrade()
                .map(|ui| ui.update_transcript_find_query(query));
        },
        move |delta| {
            let _ = weak_step.upgrade().map(|ui| ui.step_transcript_find(delta));
        },
        move || {
            let _ = weak_older.upgrade().map(|ui| ui.older.emit_clicked());
        },
        move || {
            let _ = weak_close.upgrade().map(|ui| ui.close_transcript_find());
        },
    );
}

fn connect_studio(ui: &Rc<Ui>) {
    let weak = Rc::downgrade(ui);
    ui.workspace_picker.connect_selected(move |cwd| {
        let _ = weak.upgrade().map(|ui| ui.choose_workspace(cwd));
    });
    let weak = Rc::downgrade(ui);
    ui.workspace_picker.connect_closed(move || {
        let _ = weak.upgrade().map(|ui| ui.editor.grab_focus());
    });
    let weak = Rc::downgrade(ui);
    ui.slash_deck.connect_activated(move |cand| {
        let _ = weak
            .upgrade()
            .map(|ui| ui.accept_slash_candidate(cand, false));
    });
    let weak = Rc::downgrade(ui);
    ui.attachments.attach_btn.connect_clicked(move |_| {
        let _ = weak.upgrade().map(|ui| ui.open_attach_image_dialog());
    });
    let weak = Rc::downgrade(ui);
    ui.attachments.connect_remove(move |id| {
        let _ = weak.upgrade().map(|ui| ui.remove_staged_attachment(id));
    });
    let weak = Rc::downgrade(ui);
    studio::install_image_drop_target(&ui.composer_box, move |files| {
        let _ = weak.upgrade().map(|ui| ui.stage_local_files(&files));
    });
    let studio_keys = gtk::EventControllerKey::new();
    studio_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ui);
    studio_keys.connect_key_pressed(move |_, key, _, modifiers| {
        weak.upgrade().map_or(glib::Propagation::Proceed, |ui| {
            handle_studio_editor_key(&ui, key, modifiers)
        })
    });
    ui.editor.add_controller(studio_keys);
}

fn handle_studio_editor_key(
    ui: &Rc<Ui>,
    key: gdk::Key,
    modifiers: gdk::ModifierType,
) -> glib::Propagation {
    if is_image_clipboard_paste_shortcut(ui, key, modifiers) {
        ui.paste_clipboard_image();
        return glib::Propagation::Stop;
    }
    if !ui.slash_deck.is_open()
        || modifiers.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK)
    {
        return glib::Propagation::Proceed;
    }
    handle_slash_deck_key(ui, key, modifiers)
}

fn is_image_clipboard_paste_shortcut(ui: &Ui, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
    shortcut_command(key, modifiers) == Some('v')
        && ui.can_stage_images_now()
        && studio::clipboard_has_image_only(&ui.editor.clipboard().formats())
}

fn handle_slash_deck_key(
    ui: &Rc<Ui>,
    key: gdk::Key,
    modifiers: gdk::ModifierType,
) -> glib::Propagation {
    match key {
        gdk::Key::Down | gdk::Key::KP_Down => {
            ui.slash_deck.step(1);
            glib::Propagation::Stop
        }
        gdk::Key::Up | gdk::Key::KP_Up => {
            ui.slash_deck.step(-1);
            glib::Propagation::Stop
        }
        gdk::Key::Escape => {
            let draft = ui.state.borrow().active_draft().to_string();
            ui.slash_deck.dismiss(&draft);
            glib::Propagation::Stop
        }
        gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
            ui.complete_selected_slash(true);
            glib::Propagation::Stop
        }
        gdk::Key::Return | gdk::Key::KP_Enter
            if !modifiers.contains(gdk::ModifierType::SHIFT_MASK) =>
        {
            ui.complete_selected_slash(false);
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    }
}

fn shortcut_command(key: gdk::Key, modifiers: gdk::ModifierType) -> Option<char> {
    let clean = modifiers
        & (gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::ALT_MASK);
    if clean != gdk::ModifierType::CONTROL_MASK {
        return None;
    }
    key.to_unicode().map(|ch| ch.to_ascii_lowercase())
}

fn ctrl_shift_command(key: gdk::Key, modifiers: gdk::ModifierType) -> Option<char> {
    let clean = modifiers
        & (gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::ALT_MASK);
    if clean != (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK) {
        return None;
    }
    key.to_unicode().map(|ch| ch.to_ascii_lowercase())
}

fn is_plain_f2(key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
    let clean = modifiers
        & (gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::ALT_MASK);
    clean.is_empty() && key == gdk::Key::F2
}

fn handle_window_shortcut(
    ui: &Rc<Ui>,
    key: gdk::Key,
    modifiers: gdk::ModifierType,
) -> glib::Propagation {
    if handle_extended_shortcut(ui, key, modifiers) {
        return glib::Propagation::Stop;
    }
    let Some(ch) = shortcut_command(key, modifiers) else {
        return glib::Propagation::Proceed;
    };
    if ch == 'k' {
        ui.toggle_palette();
        return glib::Propagation::Stop;
    }
    if !dispatch_workspace_shortcut(ui, ch) {
        return glib::Propagation::Proceed;
    }
    glib::Propagation::Stop
}

fn handle_extended_shortcut(ui: &Rc<Ui>, key: gdk::Key, modifiers: gdk::ModifierType) -> bool {
    if is_plain_f2(key, modifiers) {
        ui.close_palette();
        ui.prompt_rename_current();
        return true;
    }
    match ctrl_shift_command(key, modifiers) {
        Some('f') => {
            ui.close_palette();
            ui.open_transcript_find(None);
            true
        }
        Some('e') => {
            ui.close_palette();
            ui.archive_current();
            true
        }
        Some('w') => {
            ui.close_palette();
            ui.open_workspace_picker();
            true
        }
        _ => false,
    }
}

fn dispatch_workspace_shortcut(ui: &Rc<Ui>, ch: char) -> bool {
    match ch {
        'n' => {
            ui.close_palette();
            ui.create();
        }
        'd' => {
            ui.close_palette();
            ui.branch_from(None, None);
        }
        'b' => {
            ui.close_palette();
            ui.toggle_sidebar();
        }
        'i' => {
            ui.close_palette();
            ui.toggle_inspector();
        }
        'f' => {
            ui.close_palette();
            ui.focus_sidebar_filter();
        }
        'p' => {
            ui.close_palette();
            ui.toggle_pin_current();
        }
        'm' => {
            ui.close_palette();
            ui.open_model_menu();
        }
        'r' => {
            ui.close_palette();
            ui.reload_or_reconnect();
        }
        _ => return dispatch_studio_shortcut(ui, ch),
    }
    true
}

fn dispatch_studio_shortcut(ui: &Rc<Ui>, ch: char) -> bool {
    if ch == 'u' {
        ui.close_palette();
        ui.open_attach_image_dialog();
        return true;
    }
    false
}

fn visible_selected_position(state: &AppState, visible_indices: &[usize]) -> u32 {
    state
        .selected
        .as_ref()
        .and_then(|id| {
            visible_indices.iter().position(|&index| {
                state
                    .sessions
                    .get(index)
                    .is_some_and(|s| &s.stored_id == id)
            })
        })
        .map(|pos| pos as u32)
        .unwrap_or(gtk::INVALID_LIST_POSITION)
}

impl Ui {
    fn command(&self, command: Command) -> bool {
        let result = self
            .backend
            .as_ref()
            .ok_or_else(|| {
                "Connection configuration is missing. Correct it and restart the application."
                    .into()
            })
            .and_then(|backend| backend.send(command));
        if let Err(error) = result {
            self.state.borrow_mut().notice = error;
            false
        } else {
            true
        }
    }

    fn create(&self) {
        if !self.new_chat.is_sensitive() {
            return;
        }
        let cwd = self.state.borrow().launch_cwd.clone();
        self.select_created.set(true);
        self.state.borrow_mut().creating = true;
        if !self.command(Command::Create {
            title: "New conversation".into(),
            cwd,
        }) {
            self.state.borrow_mut().creating = false;
        }
        self.refresh();
    }

    fn set_editor_text(&self, text: &str) {
        self.changing_draft.set(true);
        self.editor.buffer().set_text(text);
        self.changing_draft.set(false);
    }

    fn set_editor_text_at_end(&self, text: &str) {
        self.set_editor_text(text);
        let buffer = self.editor.buffer();
        let end = buffer.end_iter();
        buffer.place_cursor(&end);
    }

    fn clear_active_draft(&self) {
        self.state.borrow_mut().set_active_draft(String::new());
        self.set_editor_text("");
    }

    fn select(&self, id: &str, focus_editor: bool) {
        let switched = self.state.borrow().selected.as_deref() != Some(id);
        {
            let mut state = self.state.borrow_mut();
            state.pending_launch = None;
            state.selected = Some(id.to_string());
            state.notice.clear();
            if switched {
                state.pending_find_query = None;
            }
        }
        if switched {
            self.recompute_find_for_selected();
        }
        self.follow_bottom.set(true);
        let draft = self
            .state
            .borrow_mut()
            .conversations
            .entry(id.to_string())
            .or_default()
            .draft
            .clone();
        self.set_editor_text(&draft);
        let needs_resume =
            self.state.borrow().conversations.get(id).is_none_or(|c| {
                !c.loaded || (c.runtime_id.is_none() && c.read_only_reason.is_none())
            });
        if needs_resume {
            self.resume(id);
        }
        self.refresh();
        self.sync_sidebar_selection();
        if focus_editor {
            self.editor.grab_focus();
        }
    }

    fn recompute_find_for_selected(&self) {
        let mut state = self.state.borrow_mut();
        if !state.transcript_find.open {
            return;
        }
        let messages = state
            .selected
            .as_ref()
            .and_then(|id| state.conversations.get(id))
            .map(|c| c.messages.clone())
            .unwrap_or_default();
        state.transcript_find.recompute(&messages);
    }

    fn resume(&self, id: &str) {
        if !self.pending_resumes.borrow_mut().insert(id.to_string()) {
            return;
        }
        self.follow_bottom.set(true);
        if !self.command(Command::Resume {
            stored_id: id.to_string(),
        }) {
            self.pending_resumes.borrow_mut().remove(id);
        }
        self.refresh();
    }

    fn submit(self: &Rc<Self>) {
        if self.try_submit_local_studio_intent() {
            return;
        }
        if self.state.borrow().selected.is_none() {
            self.submit_from_launch();
            return;
        }
        if self.try_submit_curation_or_find() {
            return;
        }
        let Some((id, runtime_id, draft)) = self.extract_submit_target() else {
            return;
        };
        self.dispatch_live_intent(id, runtime_id, &draft);
    }

    fn dispatch_live_intent(&self, id: String, runtime_id: String, draft: &str) {
        match parse_composer_input(draft) {
            ComposerIntent::SideQuestion(question) => {
                self.dispatch_composer_side_question(&id, question);
            }
            ComposerIntent::Branch(custom_title) => {
                self.clear_composer_draft(&id);
                self.branch_from(None, custom_title);
            }
            ComposerIntent::Rewind(mode) => {
                self.clear_composer_draft(&id);
                self.rewind_last(mode);
            }
            ComposerIntent::Compress => {
                self.clear_composer_draft(&id);
                self.compress_current();
            }
            ComposerIntent::Empty => self.dispatch_prompt_or_steer(id, runtime_id, String::new()),
            ComposerIntent::Prompt(text) => self.dispatch_prompt_or_steer(id, runtime_id, text),
            _ => {}
        }
    }

    fn try_submit_local_studio_intent(self: &Rc<Self>) -> bool {
        let intent = parse_composer_input(self.state.borrow().active_draft());
        if !is_local_studio_intent(&intent) {
            return false;
        }
        self.clear_active_draft();
        match intent {
            ComposerIntent::AttachImage => self.open_attach_image_dialog(),
            ComposerIntent::PickWorkspace(query) => self.pick_workspace_by_query(query),
            _ => {}
        }
        true
    }

    fn try_submit_curation_or_find(&self) -> bool {
        let candidate = {
            let state = self.state.borrow();
            let Some(id) = state.selected.clone() else {
                return false;
            };
            let Some(c) = state.conversations.get(&id) else {
                return false;
            };
            (id, parse_composer_input(&c.draft))
        };
        let (id, intent) = candidate;
        if !is_curation_or_find_intent(&intent) {
            return false;
        }
        self.clear_composer_draft(&id);
        match intent {
            ComposerIntent::FindInTranscript(query) => {
                self.open_transcript_find(query.as_deref());
            }
            ComposerIntent::TogglePin => self.toggle_pin_current(),
            ComposerIntent::Rename(title) => self.rename_current_to(title),
            ComposerIntent::Archive => self.archive_current(),
            _ => {}
        }
        true
    }

    fn clear_composer_draft(&self, id: &str) {
        if let Some(c) = self.state.borrow_mut().conversations.get_mut(id) {
            c.draft.clear();
        }
        self.set_editor_text("");
    }

    fn extract_submit_target(&self) -> Option<(String, String, String)> {
        let state = self.state.borrow();
        let id = state.selected.as_ref()?;
        if self.pending_resumes.borrow().contains(id) || !self.send.is_sensitive() {
            return None;
        }
        let c = state.conversations.get(id)?;
        let runtime_id = c.runtime_id.as_ref()?;
        Some((id.clone(), runtime_id.clone(), c.draft.clone()))
    }

    fn dispatch_prompt_or_steer(&self, id: String, runtime_id: String, text: String) {
        let busy = self
            .state
            .borrow()
            .conversations
            .get(&id)
            .is_some_and(|c| c.status.steerable());
        if busy {
            self.steer(id, runtime_id, text);
            return;
        }
        let attachments = self.state.borrow_mut().take_conversation_attachments(&id);
        let display_text = studio::format_optimistic_prompt(&text, &attachments);
        let activity = submit_activity_label(!attachments.is_empty());
        if !self.command(Command::Submit {
            runtime_id,
            text,
            attachments: attachments.clone(),
        }) {
            if let Some(c) = self.state.borrow_mut().conversations.get_mut(&id) {
                c.attachments = attachments;
            }
            self.refresh();
            return;
        }
        self.record_optimistic_submit(&id, display_text, activity);
        self.set_editor_text("");
        self.follow_bottom.set(true);
        self.refresh();
    }

    fn submit_from_launch(&self) {
        if !self.send.is_sensitive() {
            return;
        }
        let cwd = self.state.borrow().launch_cwd.clone();
        let Some((title, _)) = self.state.borrow_mut().begin_launch_submit() else {
            return;
        };
        self.select_created.set(true);
        if !self.command(Command::Create { title, cwd }) {
            self.state.borrow_mut().cancel_pending_launch();
        }
        self.refresh();
    }

    fn send_initial_prompt(
        &self,
        id: &str,
        runtime_id: String,
        text: String,
        attachments: Vec<StagedImage>,
    ) {
        let display_text = studio::format_optimistic_prompt(&text, &attachments);
        let activity = submit_activity_label(!attachments.is_empty());
        if !self.command(Command::Submit {
            runtime_id,
            text: text.clone(),
            attachments: attachments.clone(),
        }) {
            self.restore_failed_initial_prompt(id, text, attachments);
            return;
        }
        self.record_optimistic_submit(id, display_text, activity);
        self.follow_bottom.set(true);
        self.refresh();
    }

    fn restore_failed_initial_prompt(&self, id: &str, text: String, attachments: Vec<StagedImage>) {
        if let Some(c) = self.state.borrow_mut().conversations.get_mut(id) {
            c.draft = text.clone();
            c.attachments = attachments;
        }
        if self.state.borrow().selected.as_deref() == Some(id) {
            self.set_editor_text(&text);
        }
        self.refresh();
    }

    fn record_optimistic_submit(&self, id: &str, display_text: String, activity: &'static str) {
        self.sequence.set(self.sequence.get() + 1);
        if let Some(c) = self.state.borrow_mut().conversations.get_mut(id) {
            c.clear_turn_reasoning();
            c.record_message(ChatMessage {
                id: format!("client-{}", self.sequence.get()),
                role: Role::User,
                text: display_text,
            });
            c.draft.clear();
            c.status = TurnStatus::Submitting;
            c.activity = activity.into();
        }
    }

    fn submit_composer_as_side(&self) {
        let candidate = {
            let state = self.state.borrow();
            let Some(id) = state.selected.clone() else {
                return;
            };
            let Some(c) = state.conversations.get(&id) else {
                return;
            };
            let question = match parse_composer_input(&c.draft) {
                ComposerIntent::SideQuestion(q) | ComposerIntent::Prompt(q) => q,
                _ => String::new(),
            };
            (id, question)
        };
        let (id, question) = candidate;
        if question.is_empty() {
            self.open_inspector(InspectorTab::SideAsk);
            return;
        }
        self.dispatch_composer_side_question(&id, question);
    }

    fn dispatch_composer_side_question(&self, id: &str, question: String) {
        if !self.submit_side_question(question) {
            return;
        }
        if let Some(c) = self.state.borrow_mut().conversations.get_mut(id) {
            c.draft.clear();
        }
        self.set_editor_text("");
        self.refresh();
    }

    fn submit_side_question(&self, question: String) -> bool {
        let prepared = {
            let mut state = self.state.borrow_mut();
            if state.connection.as_ref() != Some(&ConnectionState::Connected) {
                return false;
            }
            let Some(id) = state.selected.clone() else {
                return false;
            };
            let runtime_id = state
                .conversations
                .get(&id)
                .filter(|c| c.read_only_reason.is_none())
                .and_then(|c| c.runtime_id.clone());
            let Some(runtime_id) = runtime_id else {
                return false;
            };
            let client_id = state.allocate_side_id();
            state.inspector_open = true;
            state.inspector_tab = InspectorTab::SideAsk;
            if let Some(c) = state.conversations.get_mut(&id) {
                c.begin_side_question(client_id, question.clone());
            }
            (id, runtime_id, client_id)
        };
        let (id, runtime_id, client_id) = prepared;
        if !self.command(Command::AskSide {
            runtime_id,
            client_id,
            question,
        }) {
            let msg = self.state.borrow().notice.clone();
            if let Some(c) = self.state.borrow_mut().conversations.get_mut(&id) {
                c.fail_side_question(client_id, msg);
            }
        }
        self.refresh();
        true
    }

    fn toggle_inspector(&self) {
        let (open, tab) = {
            let mut state = self.state.borrow_mut();
            state.inspector_open = !state.inspector_open;
            (state.inspector_open, state.inspector_tab)
        };
        self.refresh();
        if open && tab == InspectorTab::SideAsk {
            self.inspector.focus_side_entry();
        } else if !open {
            self.editor.grab_focus();
        }
    }

    fn open_inspector(&self, tab: InspectorTab) {
        {
            let mut state = self.state.borrow_mut();
            state.inspector_open = true;
            state.inspector_tab = tab;
        }
        self.refresh();
        if tab == InspectorTab::SideAsk {
            self.inspector.focus_side_entry();
        }
    }

    fn handle_inspector_action(&self, action: InspectorAction) {
        match action {
            InspectorAction::Close => {
                self.state.borrow_mut().inspector_open = false;
                self.refresh();
                self.editor.grab_focus();
            }
            InspectorAction::SelectTab(tab) => {
                self.state.borrow_mut().inspector_tab = tab;
                self.refresh();
            }
            InspectorAction::AskSide(question) => {
                let _ = self.submit_side_question(question);
            }
            InspectorAction::InsertDraft(snippet) => self.insert_into_composer(&snippet),
            InspectorAction::JumpToMessage(index) => self.jump_to_message(index),
            InspectorAction::BranchAt(index) => self.branch_from(Some(index), None),
            InspectorAction::CompressContext => self.compress_current(),
        }
    }

    fn insert_into_composer(&self, snippet: &str) {
        let clean = snippet.trim();
        if clean.is_empty() || !self.editor.is_editable() {
            return;
        }
        let combined = {
            let mut state = self.state.borrow_mut();
            let current = state.active_draft().trim_end();
            let next = if current.is_empty() {
                clean.to_string()
            } else {
                format!("{current}\n\n{clean}")
            };
            state.set_active_draft(next.clone());
            next
        };
        self.set_editor_text(&combined);
        self.refresh_controls();
        self.editor.grab_focus();
    }

    fn jump_to_message(&self, index: usize) {
        if (index as u32) >= self.messages.n_items() {
            return;
        }
        self.follow_bottom.set(false);
        self.message_list
            .scroll_to(index as u32, gtk::ListScrollFlags::FOCUS, None);
    }

    fn toggle_palette(&self) {
        let palette = self.palette.borrow().clone();
        if let Some(palette) = palette {
            let state = self.state.borrow();
            palette.toggle(&self.window, &state);
        }
    }

    fn close_palette(&self) {
        if let Some(palette) = self.palette.borrow().clone() {
            palette.close();
        }
    }

    fn toggle_sidebar(&self) {
        let next = !self.sidebar.is_visible();
        self.sidebar.set_visible(next);
        if !next {
            self.editor.grab_focus();
        }
    }

    fn focus_sidebar_filter(&self) {
        self.sidebar.set_visible(true);
        self.triage.search.grab_focus();
    }

    fn set_session_filter(&self, filter: SessionFilter) {
        self.sidebar.set_visible(true);
        self.state.borrow_mut().session_filter = filter;
        self.refresh_sessions();
    }

    fn open_model_menu(&self) {
        if self.model_menu.is_sensitive() {
            self.model_menu.popup();
        }
    }

    fn trigger_reconnect(&self) {
        if !self.reconnect.is_sensitive() {
            return;
        }
        self.state.borrow_mut().notice.clear();
        self.state.borrow_mut().disconnect();
        self.pending_resumes.borrow_mut().clear();
        self.command(Command::Reconnect);
        self.refresh();
    }

    fn load_more_sessions(&self) {
        if !self.more_sessions.is_sensitive() {
            return;
        }
        let offset = self.state.borrow().sessions.len();
        self.state.borrow_mut().list_loading = true;
        self.command(Command::List { offset });
        self.refresh();
    }

    fn reload_current(&self) {
        if !self.latest.is_sensitive() {
            return;
        }
        let id = self.state.borrow().selected.clone();
        if let Some(id) = id {
            self.resume(&id);
        }
    }

    fn reload_or_reconnect(&self) {
        if self.state.borrow().selected.is_some() {
            self.reload_current();
        } else {
            self.trigger_reconnect();
        }
    }

    fn confirm_delete_current(self: &Rc<Self>) {
        let target = {
            let state = self.state.borrow();
            if state.connection.as_ref() != Some(&ConnectionState::Connected) {
                return;
            }
            let Some(id) = state.selected.clone() else {
                return;
            };
            let title = state
                .sessions
                .iter()
                .find(|s| s.stored_id == id)
                .map(|s| s.title.as_str())
                .filter(|t| !t.is_empty())
                .unwrap_or("Untitled conversation")
                .to_string();
            (id, title)
        };
        let (stored_id, title) = target;
        let body = format!(
            "“{title}” will be permanently deleted from the Hermes server. This cannot be undone."
        );
        let dialog = adw::AlertDialog::new(Some("Delete conversation?"), Some(&body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response == "delete"
                && let Some(ui) = weak.upgrade()
            {
                ui.command(Command::Delete {
                    stored_id: stored_id.clone(),
                });
                ui.refresh();
            }
        });
        dialog.present(Some(&self.window));
    }

    fn run_palette_action(self: &Rc<Self>, action: PaletteAction) {
        self.close_palette();
        match action {
            PaletteAction::NewChat => self.create(),
            PaletteAction::SwitchModel => self.open_model_menu(),
            PaletteAction::ToggleSidebar => self.toggle_sidebar(),
            PaletteAction::FocusFilter => self.focus_sidebar_filter(),
            PaletteAction::FilterSessions(filter) => self.set_session_filter(filter),
            PaletteAction::OpenSession(id) => {
                self.select_created.set(false);
                self.select(&id, true);
            }
            PaletteAction::OpenSearchHit { stored_id, query } => {
                self.select_created.set(false);
                self.open_session_with_find(&stored_id, &query);
            }
            other => self.run_palette_session_action(other),
        }
    }

    fn run_palette_session_action(self: &Rc<Self>, action: PaletteAction) {
        match action {
            PaletteAction::ToggleInspector => self.toggle_inspector(),
            PaletteAction::OpenInspector(tab) => self.open_inspector(tab),
            PaletteAction::FindInTranscript => self.open_transcript_find(None),
            PaletteAction::BranchCurrent => self.branch_from(None, None),
            PaletteAction::RewindLast(mode) => self.rewind_last(mode),
            PaletteAction::ReloadHistory => self.reload_current(),
            PaletteAction::LoadMoreSessions => self.load_more_sessions(),
            PaletteAction::Reconnect => self.trigger_reconnect(),
            other => self.run_palette_curation_action(other),
        }
    }

    fn run_palette_curation_action(self: &Rc<Self>, action: PaletteAction) {
        match action {
            PaletteAction::TogglePinCurrent => self.toggle_pin_current(),
            PaletteAction::RenameCurrent => self.prompt_rename_current(),
            PaletteAction::ArchiveCurrent => self.archive_current(),
            PaletteAction::DeleteCurrent => self.confirm_delete_current(),
            other => self.run_palette_studio_action(other),
        }
    }

    fn run_palette_studio_action(self: &Rc<Self>, action: PaletteAction) {
        match action {
            PaletteAction::SwitchWorkspace => self.open_workspace_picker(),
            PaletteAction::AttachImage => self.open_attach_image_dialog(),
            PaletteAction::CompressCurrent => self.compress_current(),
            _ => {}
        }
    }

    fn queue_remote_search(self: &Rc<Self>, raw_query: &str) {
        let trimmed = raw_query.trim().to_string();
        let token = self.search_debounce_seq.get().wrapping_add(1);
        self.search_debounce_seq.set(token);
        if trimmed.chars().count() < 2 {
            let _ = self.state.borrow_mut().begin_remote_search("");
            return;
        }
        self.state.borrow_mut().remote_search_loading = true;
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(180), move || {
            if let Some(ui) = weak.upgrade() {
                ui.fire_debounced_search(token, trimmed);
            }
        });
    }

    fn fire_debounced_search(&self, token: u64, query: String) {
        if self.search_debounce_seq.get() != token {
            return;
        }
        let request_seq = {
            let mut state = self.state.borrow_mut();
            if state.connection.as_ref() != Some(&ConnectionState::Connected) {
                state.remote_search_loading = false;
                None
            } else {
                state.begin_remote_search(&query)
            }
        };
        if let Some((request_seq, clean_query)) = request_seq
            && !self.command(Command::SearchSessions {
                query: clean_query,
                request_seq,
            })
        {
            self.state.borrow_mut().remote_search_loading = false;
        }
        self.refresh_sessions();
    }

    fn open_session_with_find(&self, stored_id: &str, query: &str) {
        ensure_session_from_search_hit(&mut self.state.borrow_mut(), stored_id);
        let clean_query = query.trim().to_string();
        self.select(stored_id, false);
        if clean_query.is_empty() {
            self.editor.grab_focus();
            return;
        }
        let ready_now = !self.pending_resumes.borrow().contains(stored_id)
            && self
                .state
                .borrow()
                .conversations
                .get(stored_id)
                .is_some_and(|c| c.loaded);
        if ready_now {
            self.open_transcript_find(Some(&clean_query));
        } else {
            self.state.borrow_mut().pending_find_query = Some(clean_query);
        }
    }

    fn open_transcript_find(&self, initial_query: Option<&str>) {
        let active_idx = {
            let mut state = self.state.borrow_mut();
            let Some(id) = state.selected.clone() else {
                return;
            };
            let query =
                initial_query.map_or_else(|| state.transcript_find.query.clone(), str::to_string);
            let messages = state
                .conversations
                .get(&id)
                .map(|c| c.messages.clone())
                .unwrap_or_default();
            state.transcript_find.open_with(&query, &messages);
            state.transcript_find.active_message_index()
        };
        if active_idx.is_some() {
            self.follow_bottom.set(false);
        }
        self.refresh();
        self.find_bar.focus_entry();
        if let Some(index) = active_idx {
            self.schedule_scroll_to_message(index);
        }
    }

    fn update_transcript_find_query(&self, query: String) {
        let active_idx = {
            let mut state = self.state.borrow_mut();
            let messages = state
                .selected
                .as_ref()
                .and_then(|id| state.conversations.get(id))
                .map(|c| c.messages.clone())
                .unwrap_or_default();
            state.transcript_find.set_query(query, &messages);
            state.transcript_find.active_message_index()
        };
        if active_idx.is_some() {
            self.follow_bottom.set(false);
        }
        self.refresh();
        if let Some(index) = active_idx {
            self.schedule_scroll_to_message(index);
        }
    }

    fn step_transcript_find(&self, delta: isize) {
        let active_idx = {
            let mut state = self.state.borrow_mut();
            state.transcript_find.step(delta);
            state.transcript_find.active_message_index()
        };
        if active_idx.is_some() {
            self.follow_bottom.set(false);
        }
        self.refresh();
        if let Some(index) = active_idx {
            self.schedule_scroll_to_message(index);
        }
    }

    fn close_transcript_find(&self) {
        self.state.borrow_mut().transcript_find.close();
        self.refresh();
        self.editor.grab_focus();
    }

    fn schedule_scroll_to_message(&self, index: usize) {
        self.follow_bottom.set(false);
        let list = self.message_list.clone();
        let store = self.messages.clone();
        glib::idle_add_local_once(move || {
            if (index as u32) < store.n_items() {
                list.scroll_to(index as u32, gtk::ListScrollFlags::FOCUS, None);
            }
        });
    }

    fn toggle_pin_current(&self) {
        let target = {
            let state = self.state.borrow();
            if state.connection.as_ref() != Some(&ConnectionState::Connected) {
                return;
            }
            state
                .selected
                .clone()
                .map(|id| (id, !state.is_selected_pinned()))
        };
        let Some((stored_id, next_pinned)) = target else {
            return;
        };
        self.command(Command::MutateSession {
            stored_id,
            mutation: SessionMutation::SetPinned(next_pinned),
        });
        self.refresh();
    }

    fn rename_current_to(&self, title: String) {
        let clean = title.trim().to_string();
        if clean.is_empty() {
            return;
        }
        let stored_id = {
            let state = self.state.borrow();
            if state.connection.as_ref() != Some(&ConnectionState::Connected) {
                return;
            }
            state.selected.clone()
        };
        let Some(stored_id) = stored_id else {
            return;
        };
        self.command(Command::MutateSession {
            stored_id,
            mutation: SessionMutation::Rename(clean),
        });
        self.refresh();
    }

    fn prompt_rename_current(self: &Rc<Self>) {
        let target = {
            let state = self.state.borrow();
            if state.connection.as_ref() != Some(&ConnectionState::Connected) {
                return;
            }
            let Some(id) = state.selected.clone() else {
                return;
            };
            let current = session_display_title(&state, &id);
            (id, current)
        };
        let (_, current_title) = target;
        let weak = Rc::downgrade(self);
        recall::present_rename_dialog(&self.window, &current_title, move |new_title| {
            if let Some(ui) = weak.upgrade() {
                ui.rename_current_to(new_title);
            }
        });
    }

    fn archive_current(&self) {
        let stored_id = {
            let state = self.state.borrow();
            if state.connection.as_ref() != Some(&ConnectionState::Connected) {
                return;
            }
            state.selected.clone()
        };
        let Some(stored_id) = stored_id else {
            return;
        };
        self.command(Command::MutateSession {
            stored_id,
            mutation: SessionMutation::Archive,
        });
        self.refresh();
    }

    fn branch_from(&self, end_inclusive: Option<usize>, custom_title: Option<String>) {
        let prepared = {
            let state = self.state.borrow();
            prepare_branch_command(&state, end_inclusive, custom_title)
        };
        let Some((stored_id, cmd)) = prepared else {
            return;
        };
        self.select_created.set(true);
        {
            let mut state = self.state.borrow_mut();
            state.creating = true;
            state.notice.clear();
            if let Some(c) = state.conversations.get_mut(&stored_id) {
                c.activity = "Creating branch…".into();
            }
        }
        if !self.command(cmd) {
            let mut state = self.state.borrow_mut();
            state.creating = false;
            if let Some(c) = state.conversations.get_mut(&stored_id) {
                c.activity.clear();
            }
        }
        self.refresh();
    }

    fn rewind_last(&self, mode: RewindMode) {
        let prepared = {
            let state = self.state.borrow();
            prepare_rewind_command(&state, mode)
        };
        let Some((stored_id, _, cmd)) = prepared else {
            return;
        };
        {
            let mut state = self.state.borrow_mut();
            state.notice.clear();
            if let Some(c) = state.conversations.get_mut(&stored_id) {
                c.rewinding = true;
                c.activity = "Rewinding last turn…".into();
            }
        }
        if !self.command(cmd)
            && let Some(c) = self.state.borrow_mut().conversations.get_mut(&stored_id)
        {
            c.rewinding = false;
            c.activity.clear();
        }
        self.refresh();
    }

    /// Busy-turn steer: optimistic row now, live turn untouched until the
    /// gateway answers. Multiple steers stack, one row each.
    fn steer(&self, id: String, runtime_id: String, text: String) {
        if !self.command(Command::Steer {
            runtime_id,
            text: text.clone(),
        }) {
            self.refresh();
            return;
        }
        self.sequence.set(self.sequence.get() + 1);
        let mut state = self.state.borrow_mut();
        if let Some(c) = state.conversations.get_mut(&id) {
            c.record_message(ChatMessage {
                id: format!("client-{}", self.sequence.get()),
                role: Role::User,
                text,
            });
            c.draft.clear();
            c.activity = "Steering…".into();
        }
        drop(state);
        self.changing_draft.set(true);
        self.editor.buffer().set_text("");
        self.changing_draft.set(false);
        self.follow_bottom.set(true);
        self.refresh();
    }

    /// Rebuild the result list from the cached catalog and current query. No network.
    fn refresh_model_results(&self) {
        let (options, status) = {
            let state = self.state.borrow();
            let catalog = &state.catalog;
            let query = self.model_search.text().to_string();
            let options: Vec<glib::BoxedAnyObject> = catalog
                .search(&query)
                .into_iter()
                .filter_map(|index| catalog.options.get(index))
                .cloned()
                .map(glib::BoxedAnyObject::new)
                .collect();
            let status = if let Some(error) = &catalog.error {
                error.clone()
            } else if catalog.loading {
                "Loading models…".into()
            } else if catalog.is_empty() {
                "No models are configured on this server.".into()
            } else if options.is_empty() {
                "No model matches that search.".into()
            } else {
                String::new()
            };
            (options, status)
        };
        self.model_results
            .splice(0, self.model_results.n_items(), &options);
        self.model_status.set_text(&status);
        self.model_status.set_visible(!status.is_empty());
    }

    fn commit_model_selection(&self) {
        let Some(selected) = self.model_selection.selected_item() else {
            return;
        };
        let Some(value) = selected.downcast::<glib::BoxedAnyObject>().ok() else {
            return;
        };
        let (identity, selectable) = {
            let option = value.borrow::<ModelOption>();
            (option.identity.clone(), option.selectable())
        };
        if !selectable {
            self.model_status
                .set_text("This catalog entry cannot be sent to the gateway.");
            self.model_status.set_visible(true);
            return;
        }
        self.model_popover.popdown();
        self.editor.grab_focus();
        self.choose_model(identity, false);
    }

    /// Ask the backend to switch the open conversation. `confirm` is only ever true after
    /// the user accepted an explicit confirmation prompt.
    fn choose_model(&self, target: ModelIdentity, confirm: bool) {
        let request = {
            let state = self.state.borrow();
            let Some(stored) = state.selected.clone() else {
                return;
            };
            let Some(conversation) = state.conversations.get(&stored) else {
                return;
            };
            // An unresolved earlier attempt must be settled before another one starts.
            if conversation.read_only_reason.is_some() || conversation.model.switching() {
                return;
            }
            let Some(runtime) = conversation.runtime_id.clone() else {
                return;
            };
            // A chat that never ran has no agent yet: the gateway's scoped switch would be
            // acknowledged without taking effect, so the conversation's own pair primes it
            // first. A chat that already ran is initialized.
            let prime = if conversation.messages.is_empty() {
                conversation.model.confirmed().cloned()
            } else {
                None
            };
            (stored, runtime, prime)
        };
        let (stored, runtime, prime) = request;
        if !self.command(Command::SetModel {
            runtime_id: runtime.clone(),
            prime: prime.clone(),
            target: target.clone(),
            confirm_expensive: confirm,
        }) {
            return;
        }
        if let Some(conversation) = self.state.borrow_mut().conversations.get_mut(&stored) {
            conversation.begin_model_switch(target.clone());
        }
        *self.model_pending.borrow_mut() = Some(PendingModel {
            stored_id: stored,
            runtime_id: runtime,
            target,
            prime,
        });
        self.refresh();
        // Rebind the rows so the current-pair mark follows the request.
        self.refresh_model_results();
    }

    /// The gateway wants an explicit confirmation. Nothing changed yet.
    fn ask_model_confirmation(self: &Rc<Self>, runtime_id: &str, message: String) {
        let pending = self
            .model_pending
            .borrow()
            .as_ref()
            .filter(|pending| pending.runtime_id == runtime_id)
            .map(|pending| PendingModel {
                stored_id: pending.stored_id.clone(),
                runtime_id: pending.runtime_id.clone(),
                target: pending.target.clone(),
                prime: pending.prime.clone(),
            });
        let Some(pending) = pending else {
            return;
        };
        let dialog = adw::AlertDialog::new(Some("Use this model?"), Some(&message));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("switch", "Switch");
        dialog.set_response_appearance("switch", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |dialog, response| {
            if response != "switch" {
                return;
            }
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let _ = &pending.prime;
            ui.choose_model(pending.target.clone(), true);
            let _ = dialog;
        });
        dialog.present(Some(&self.window));
    }

    fn apply_model(self: &Rc<Self>, event: &Event) -> bool {
        match event {
            Event::Catalog { catalog } => {
                self.state.borrow_mut().catalog = catalog.clone();
                self.refresh_model_results();
            }
            Event::ModelAck {
                runtime_id,
                resolved,
                deferred,
            } => {
                let primed = self.model_pending.borrow().as_ref().is_some_and(|pending| {
                    pending.runtime_id == *runtime_id && pending.prime.is_some()
                });
                self.settle_model(runtime_id, |conversation| {
                    conversation.model_acknowledged(resolved.clone(), *deferred, !primed);
                });
            }
            Event::ModelRejected {
                runtime_id,
                message,
            } => {
                self.settle_model(runtime_id, |conversation| conversation.model_rejected());
                self.state.borrow_mut().notice = message.clone();
            }
            Event::ModelConfirmation {
                runtime_id,
                message,
                ..
            } => {
                self.settle_model(runtime_id, |conversation| conversation.model_rejected());
                self.ask_model_confirmation(runtime_id, message.clone());
            }
            Event::ModelAmbiguous { runtime_id } => {
                self.settle_model(runtime_id, |conversation| conversation.model_ambiguous());
                self.state.borrow_mut().notice = "The model change was accepted for delivery but never answered. It stays unconfirmed and will not be retried.".into();
            }
            Event::ModelObserved { runtime_id, model } => {
                let identity = self.state.borrow().catalog.identity_for_model(model);
                if let Some(identity) = identity {
                    self.settle_model(runtime_id, |conversation| {
                        conversation.observe_model(identity)
                    });
                }
            }
            _ => return false,
        }
        true
    }

    fn settle_model(&self, runtime_id: &str, update: impl FnOnce(&mut Conversation)) {
        *self.model_pending.borrow_mut() = None;
        if let Some(conversation) = self
            .state
            .borrow_mut()
            .conversation_for_runtime_mut(runtime_id)
        {
            update(conversation);
        }
    }

    fn note_created(
        &self,
        session: SessionSummary,
        runtime_id: String,
        model: Option<ModelIdentity>,
    ) {
        let id = session.stored_id.clone();
        let launch_bundle = {
            let mut state = self.state.borrow_mut();
            let seeded_draft = state.take_launch_draft_for_blank_chat();
            let seeded_attachments = state.take_launch_attachments_for_blank_chat();
            let bundle = state.take_pending_launch_bundle();
            let initial_cwd = state.launch_cwd.clone().unwrap_or_default();
            state.sessions.insert(0, session);
            let mut conversation = Conversation {
                runtime_id: Some(runtime_id.clone()),
                loaded: true,
                draft: seeded_draft,
                attachments: seeded_attachments,
                ..Default::default()
            };
            if !initial_cwd.is_empty() {
                conversation.telemetry.cwd = initial_cwd;
            }
            if let Some(model) = model {
                conversation.confirm_model(model);
            }
            state.conversations.insert(id.clone(), conversation);
            state.creating = false;
            bundle
        };
        self.refresh_sessions();
        if self.select_created.replace(false) {
            self.select(&id, true);
        }
        if let Some((prompt, attachments)) = launch_bundle {
            self.send_initial_prompt(&id, runtime_id, prompt, attachments);
        }
    }

    fn apply_inspector(&self, event: &Event) -> bool {
        let mut state = self.state.borrow_mut();
        match event {
            Event::SideStarted {
                runtime_id,
                client_id,
                task_id,
            } => {
                with_conversation(&mut state, runtime_id, |c| {
                    c.bind_side_task(*client_id, task_id.clone())
                });
            }
            Event::SideComplete {
                runtime_id,
                task_id,
                question,
                text,
            } => {
                with_conversation(&mut state, runtime_id, |c| {
                    c.complete_side_question(task_id, question, text.clone())
                });
            }
            Event::SideFailed {
                runtime_id,
                client_id,
                message,
            } => {
                with_conversation(&mut state, runtime_id, |c| {
                    c.fail_side_question(*client_id, message.clone())
                });
            }
            Event::Reasoning {
                runtime_id,
                text,
                replace,
            } => {
                with_conversation(&mut state, runtime_id, |c| {
                    c.append_reasoning(text, *replace)
                });
            }
            Event::ToolTraceEvent { runtime_id, update } => {
                with_conversation(&mut state, runtime_id, |c| {
                    c.apply_tool_event(update.clone())
                });
            }
            Event::Telemetry {
                runtime_id,
                telemetry,
            } => {
                with_conversation(&mut state, runtime_id, |c| {
                    c.apply_telemetry(telemetry.clone())
                });
            }
            _ => return false,
        }
        true
    }

    fn note_connection(&self, connection: ConnectionState) {
        let connected = connection == ConnectionState::Connected;
        let selected = {
            let mut state = self.state.borrow_mut();
            if !connected {
                state.disconnect();
                // A catalog belongs to one connection; keep no stale rows across a reconnect.
                state.catalog = Catalog::default();
                state.workspaces = WorkspaceCatalog::default();
                state.skills.clear();
                self.pending_resumes.borrow_mut().clear();
            }
            state.connection = Some(connection);
            state.creating = false;
            state.list_loading = connected;
            state.selected.clone()
        };
        if connected {
            self.command(Command::List { offset: 0 });
            if let Some(id) = selected {
                self.resume(&id);
            }
        }
    }

    fn apply_auxiliary(self: &Rc<Self>, event: &Event) -> bool {
        self.apply_model(event)
            || self.apply_inspector(event)
            || self.apply_timeline(event)
            || self.apply_studio(event)
    }

    fn apply_studio(&self, event: &Event) -> bool {
        match event {
            Event::Workspaces { catalog } => {
                self.state.borrow_mut().workspaces = catalog.clone();
            }
            Event::Skills { skills } => {
                self.state.borrow_mut().skills = skills.clone();
            }
            Event::Compressed {
                stored_id,
                runtime_id,
                messages,
                before,
                summary,
            } => {
                self.note_compressed(
                    stored_id.clone(),
                    runtime_id.clone(),
                    messages.clone(),
                    *before,
                    summary.clone(),
                );
            }
            _ => return false,
        }
        true
    }

    fn note_compressed(
        &self,
        stored_id: String,
        runtime_id: String,
        messages: Option<Vec<ChatMessage>>,
        before: usize,
        summary: CompressSummary,
    ) {
        let did_reload = messages.is_some();
        let jump_idx = {
            let mut state = self.state.borrow_mut();
            apply_compressed_state(
                &mut state, &stored_id, runtime_id, messages, before, &summary,
            );
            did_reload
                .then(|| sync_find_after_transcript_load(&mut state, &stored_id))
                .flatten()
        };
        if did_reload {
            self.refresh_sessions();
        }
        if let Some(index) = jump_idx {
            self.schedule_scroll_to_message(index);
        }
    }

    fn open_workspace_picker(&self) {
        self.workspace_picker.open();
    }

    fn pick_workspace_by_query(&self, query: Option<String>) {
        let Some(raw_query) = query else {
            self.open_workspace_picker();
            return;
        };
        let resolved = self.state.borrow().workspaces.resolve_query(&raw_query);
        if let Some(cwd) = resolved {
            self.choose_workspace(cwd);
        } else {
            self.state.borrow_mut().notice = format!("No remote workspace matched “{raw_query}”.");
            self.refresh();
            self.open_workspace_picker();
        }
    }

    fn choose_workspace(&self, cwd: Option<String>) {
        let should_launch_new = {
            let mut state = self.state.borrow_mut();
            state.notice.clear();
            state.launch_cwd = cwd.clone();
            workspace_pick_starts_new_chat(&state, cwd.as_deref())
        };
        if should_launch_new && self.new_chat.is_sensitive() {
            self.create();
        } else {
            self.refresh();
            self.editor.grab_focus();
        }
    }

    fn complete_selected_slash(self: &Rc<Self>, tab_only: bool) {
        let Some(cand) = self.slash_deck.selected_candidate() else {
            return;
        };
        self.accept_slash_candidate(cand, tab_only);
    }

    fn accept_slash_candidate(self: &Rc<Self>, cand: SlashCandidate, tab_only: bool) {
        if !tab_only && cand.zero_arg_execute {
            self.state.borrow_mut().set_active_draft(cand.token.clone());
            self.set_editor_text_at_end(&cand.token);
            self.slash_deck.hide();
            self.refresh_controls();
            self.submit();
            return;
        }
        self.state
            .borrow_mut()
            .set_active_draft(cand.insert_text.clone());
        self.set_editor_text_at_end(&cand.insert_text);
        self.slash_deck.hide();
        self.refresh_controls();
        self.editor.grab_focus();
    }

    fn can_stage_images_now(&self) -> bool {
        let state = self.state.borrow();
        let connected = state.connection.as_ref() == Some(&ConnectionState::Connected);
        can_stage_images(&state, connected)
    }

    fn open_attach_image_dialog(self: &Rc<Self>) {
        if !self.can_stage_images_now() {
            return;
        }
        let weak = Rc::downgrade(self);
        studio::spawn_image_file_dialog(&self.window, move |files| {
            if let Some(ui) = weak.upgrade() {
                ui.stage_local_files(&files);
            }
        });
    }

    fn paste_clipboard_image(self: &Rc<Self>) {
        if !self.can_stage_images_now() {
            return;
        }
        let weak = Rc::downgrade(self);
        studio::spawn_clipboard_image_paste(self.editor.clipboard(), move |result| {
            if let Some(ui) = weak.upgrade() {
                ui.finish_clipboard_image_paste(result);
            }
        });
    }

    fn finish_clipboard_image_paste(&self, result: Result<Vec<u8>, String>) {
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(err) => {
                self.state.borrow_mut().notice = err;
                self.refresh();
                return;
            }
        };
        let id = self.state.borrow_mut().allocate_attachment_id();
        let name = format!("clipboard-{id}.png");
        match StagedImage::from_bytes(id, &name, bytes) {
            Ok(img) => self.commit_staged_images(vec![img], None),
            Err(err) => {
                self.state.borrow_mut().notice = err;
                self.refresh();
            }
        }
    }

    fn stage_local_files(&self, files: &[gio::File]) {
        if !self.can_stage_images_now() {
            return;
        }
        let (staged, first_err) = studio::read_local_image_files(files, || {
            self.state.borrow_mut().allocate_attachment_id()
        });
        self.commit_staged_images(staged, first_err);
    }

    fn commit_staged_images(&self, staged: Vec<StagedImage>, read_err: Option<String>) {
        {
            let mut state = self.state.borrow_mut();
            state.notice.clear();
            if let Err(err) = state.stage_active_images(staged) {
                state.notice = err;
            } else if let Some(err) = read_err {
                state.notice = err;
            }
        }
        self.refresh();
        self.editor.grab_focus();
    }

    fn remove_staged_attachment(&self, id: u64) {
        if self.state.borrow_mut().remove_active_attachment(id) {
            self.refresh();
            self.editor.grab_focus();
        }
    }

    fn compress_current(&self) {
        let prepared = {
            let state = self.state.borrow();
            prepare_compress_command(&state)
        };
        let Some((stored_id, cmd)) = prepared else {
            return;
        };
        {
            let mut state = self.state.borrow_mut();
            state.notice.clear();
            if let Some(c) = state.conversations.get_mut(&stored_id) {
                c.rewinding = true;
                c.activity = "Compressing older turns…".into();
            }
        }
        if !self.command(cmd)
            && let Some(c) = self.state.borrow_mut().conversations.get_mut(&stored_id)
        {
            c.rewinding = false;
            c.activity.clear();
        }
        self.refresh();
    }

    fn apply_timeline(&self, event: &Event) -> bool {
        match event {
            Event::Branched {
                session,
                runtime_id,
                parent_title,
                messages,
                before,
                model,
            } => {
                self.note_branched(
                    session.clone(),
                    runtime_id.clone(),
                    parent_title.clone(),
                    messages.clone(),
                    *before,
                    model.clone(),
                );
            }
            Event::Undone {
                stored_id,
                runtime_id,
                mode,
                prompt_text,
                messages,
                before,
                ..
            } => {
                self.note_undone(
                    stored_id.clone(),
                    runtime_id.clone(),
                    *mode,
                    prompt_text.clone(),
                    messages.clone(),
                    *before,
                );
            }
            _ => return false,
        }
        true
    }

    fn note_branched(
        &self,
        session: SessionSummary,
        runtime_id: String,
        parent_title: String,
        messages: Vec<ChatMessage>,
        before: usize,
        model: Option<ModelIdentity>,
    ) {
        let id = session.stored_id.clone();
        insert_branched_state(
            &mut self.state.borrow_mut(),
            session,
            runtime_id,
            parent_title,
            messages,
            before,
            model,
        );
        self.refresh_sessions();
        if self.select_created.replace(false) {
            self.select(&id, true);
        }
    }

    fn note_undone(
        &self,
        stored_id: String,
        runtime_id: String,
        mode: RewindMode,
        prompt_text: String,
        messages: Vec<ChatMessage>,
        before: usize,
    ) {
        let is_selected = {
            let mut state = self.state.borrow_mut();
            if let Some(summary) = state.sessions.iter_mut().find(|s| s.stored_id == stored_id) {
                summary.message_count = before + messages.len();
            }
            if let Some(c) = state.conversations.get_mut(&stored_id) {
                c.runtime_id = Some(runtime_id.clone());
                apply_rewound_conversation(c, mode, &prompt_text, messages, before);
            }
            state.selected.as_deref() == Some(stored_id.as_str())
        };
        self.refresh_sessions();
        match mode {
            RewindMode::Regenerate => {
                self.dispatch_prompt_or_steer(stored_id, runtime_id, prompt_text);
            }
            RewindMode::Undo | RewindMode::EditRetry => {
                if is_selected {
                    let draft = self.state.borrow().active_draft().to_string();
                    self.set_editor_text(&draft);
                    if mode == RewindMode::EditRetry {
                        self.editor.grab_focus();
                    }
                }
            }
        }
    }

    fn apply(self: &Rc<Self>, event: Event) {
        if self.apply_auxiliary(&event) {
            return;
        }
        let mut state = self.state.borrow_mut();
        match event {
            Event::Connection(connection) => {
                drop(state);
                self.note_connection(connection);
            }
            Event::Sessions {
                sessions,
                offset,
                has_more,
            } => {
                if offset == 0 {
                    state.sessions.clear();
                }
                for session in sessions {
                    state.upsert_session(session);
                }
                state.sync_branch_flags();
                state.list_has_more = has_more;
                state.list_loading = false;
                drop(state);
                self.refresh_sessions();
            }
            Event::Created {
                session,
                runtime_id,
                model,
            } => {
                drop(state);
                self.note_created(session, runtime_id, model);
            }
            Event::Resumed {
                stored_id,
                runtime_id,
                messages,
                before,
                read_only_reason,
                model,
            } => {
                drop(state);
                self.note_resumed(
                    stored_id,
                    runtime_id,
                    messages,
                    before,
                    read_only_reason,
                    model,
                );
            }
            Event::History {
                stored_id,
                messages,
                before,
                next_before,
            } => {
                drop(state);
                self.note_history_page(stored_id, messages, before, next_before);
            }
            other => {
                let meta = self.apply_history(&mut state, &other);
                if meta == MetaOutcome::Unhandled {
                    self.apply_stream(&mut state, other);
                } else {
                    drop(state);
                    self.finish_meta(meta);
                }
            }
        }
    }

    fn note_resumed(
        &self,
        stored_id: String,
        runtime_id: Option<String>,
        messages: Vec<ChatMessage>,
        before: usize,
        read_only_reason: Option<String>,
        model: Option<ModelIdentity>,
    ) {
        self.pending_resumes.borrow_mut().remove(&stored_id);
        *self.model_pending.borrow_mut() = None;
        let jump_idx = {
            let mut state = self.state.borrow_mut();
            let c = state.conversations.entry(stored_id.clone()).or_default();
            c.runtime_id = runtime_id;
            c.messages = messages;
            c.history_before = before;
            c.history_loading = false;
            c.loaded = true;
            c.read_only_reason = read_only_reason;
            c.status = TurnStatus::Idle;
            c.activity.clear();
            c.interaction = None;
            // Resume metadata is the restored stored pair; it replaces any stale claim.
            c.model = model.map_or(ModelState::Unknown, ModelState::Confirmed);
            c.revision += 1;
            c.inspector_revision += 1;
            sync_find_after_transcript_load(&mut state, &stored_id)
        };
        if let Some(index) = jump_idx {
            self.schedule_scroll_to_message(index);
        }
    }

    fn note_history_page(
        &self,
        stored_id: String,
        messages: Vec<ChatMessage>,
        before: usize,
        next_before: usize,
    ) {
        let jump_idx = {
            let mut state = self.state.borrow_mut();
            if let Some(c) = state.conversations.get_mut(&stored_id) {
                c.receive_history_page(before, next_before, messages);
            }
            sync_find_after_history_page(&mut state, &stored_id)
        };
        if let Some(index) = jump_idx {
            self.schedule_scroll_to_message(index);
        }
    }

    /// Session-list, title, search, curation, deletion, and failure events.
    fn apply_history(&self, state: &mut AppState, event: &Event) -> MetaOutcome {
        match event {
            Event::SessionTitle { session_id, title } => {
                if state.apply_session_title(session_id, title) {
                    MetaOutcome::RefreshSessions
                } else {
                    MetaOutcome::Handled
                }
            }
            Event::SearchResults {
                query,
                request_seq,
                hits,
            } => apply_search_event(state, query, *request_seq, hits),
            Event::SessionMutated {
                stored_id,
                mutation,
            } => apply_mutation_event(state, stored_id, mutation),
            Event::Deleted { stored_id } => {
                if state.remove_session(stored_id) {
                    MetaOutcome::ResetToLaunch
                } else {
                    MetaOutcome::RefreshSessions
                }
            }
            Event::Error {
                operation,
                session_id,
                message,
            } => {
                self.record_error(state, operation, session_id.as_deref(), message);
                MetaOutcome::Handled
            }
            _ => MetaOutcome::Unhandled,
        }
    }

    fn record_error(
        &self,
        state: &mut AppState,
        operation: &str,
        session_id: Option<&str>,
        message: &str,
    ) {
        state.notice = format!("{operation}: {message}");
        if operation == "create" {
            state.cancel_pending_launch();
        }
        state.creating = false;
        state.list_loading = false;
        let Some(id) = session_id else {
            return;
        };
        self.pending_resumes.borrow_mut().remove(id);
        let stored = if state.conversations.contains_key(id) {
            Some(id.to_string())
        } else {
            state.stored_for_runtime(id)
        };
        if let Some(c) = stored.and_then(|key| state.conversations.get_mut(&key)) {
            c.history_loading = false;
            c.operation_failed(operation, message.to_string());
        }
    }

    fn finish_meta(&self, outcome: MetaOutcome) {
        match outcome {
            MetaOutcome::Unhandled | MetaOutcome::Handled => {}
            MetaOutcome::RefreshSessions => self.refresh_sessions(),
            MetaOutcome::ResetToLaunch => {
                self.refresh_sessions();
                let draft = self.state.borrow().launch_draft.clone();
                self.set_editor_text(&draft);
                self.editor.grab_focus();
            }
        }
    }

    /// Live-turn events. Returns true when the event was handled.
    fn apply_stream(&self, state: &mut AppState, event: Event) -> bool {
        match event {
            Event::Submitted { runtime_id } => {
                with_conversation(state, &runtime_id, |c| c.submit_acknowledged());
                true
            }
            Event::MessageDelta {
                runtime_id,
                message_id,
                delta,
            } => {
                with_conversation(state, &runtime_id, |c| c.receive_delta(message_id, delta));
                true
            }
            Event::MessageComplete {
                runtime_id,
                message,
                status,
            } => {
                with_conversation(state, &runtime_id, |c| {
                    c.finish_message(message);
                    c.activity = completion_activity(&status);
                    c.status = status;
                    c.interaction = None;
                });
                true
            }
            Event::Activity { runtime_id, text } => {
                with_conversation(state, &runtime_id, |c| c.activity = text);
                true
            }
            Event::Interrupting { runtime_id } => {
                with_conversation(state, &runtime_id, |c| c.begin_stopping());
                true
            }
            Event::SteerAccepted {
                runtime_id,
                outcome,
            } => {
                with_conversation(state, &runtime_id, |c| c.steer_acknowledged(outcome));
                true
            }
            Event::TurnRolled { runtime_id } => {
                with_conversation(state, &runtime_id, |c| c.turn_rolled());
                true
            }
            Event::Interaction {
                runtime_id,
                request,
            } => {
                with_conversation(state, &runtime_id, |c| {
                    c.receive_interaction(request);
                });
                true
            }
            Event::InteractionCancelled {
                runtime_id,
                request_id,
            } => {
                with_conversation(state, &runtime_id, |c| c.withdraw_interaction(&request_id));
                true
            }
            _ => false,
        }
    }

    fn refresh_sessions(&self) {
        let (rows, selected) = {
            let state = self.state.borrow();
            build_sidebar_rows(&state)
        };
        let visible_count = rows.len();
        // Drop the state borrow before these mutations. Hold the guard across
        // both: splice emits selection changes before the replacement position
        // is applied, and a nested handler must not clear this flag.
        self.suppress_selection.set(true);
        self.sessions.splice(0, self.sessions.n_items(), &rows);
        self.selection.set_selected(selected);
        self.suppress_selection.set(false);
        let state = self.state.borrow();
        self.triage.sync(&state, visible_count);
        if let Some(palette) = self.palette.borrow().clone() {
            palette.refresh_if_open(&state);
        }
    }

    fn sync_sidebar_selection(&self) {
        let selected = {
            let state = self.state.borrow();
            sidebar_selected_index(&state)
        };
        self.suppress_selection.set(true);
        self.selection.set_selected(selected);
        self.suppress_selection.set(false);
    }

    fn sidebar_item_at(&self, position: u32) -> Option<SidebarRowItem> {
        self.sessions
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|value| value.borrow::<SidebarRowItem>().clone())
    }

    fn activate_sidebar_position(&self, position: u32, focus_editor: bool) {
        let Some(item) = self.sidebar_item_at(position) else {
            return;
        };
        self.open_sidebar_item(item, focus_editor);
    }

    fn adopt_sidebar_selection(&self, focus_editor: bool) {
        if self.suppress_selection.get() {
            return;
        }
        let Some(item) = self.sidebar_item_at(self.selection.selected()) else {
            // A cleared highlight must not leave the open conversation behind.
            self.sync_sidebar_selection();
            return;
        };
        let id = item.stored_id().to_string();
        if self.state.borrow().selected.as_deref() == Some(id.as_str()) {
            if let Some(query) = item.search_query() {
                self.open_transcript_find(Some(query));
            } else if focus_editor {
                self.editor.grab_focus();
            }
            return;
        }
        self.open_sidebar_item(item, focus_editor);
    }

    fn open_sidebar_item(&self, item: SidebarRowItem, focus_editor: bool) {
        self.select_created.set(false);
        match item {
            SidebarRowItem::Local(session) => {
                if let Some(query) = local_fts_handoff(&self.state.borrow(), &session.stored_id) {
                    self.open_session_with_find(&session.stored_id, &query);
                } else {
                    self.select(&session.stored_id, focus_editor);
                }
            }
            SidebarRowItem::RemoteHit { hit, query } => {
                self.state
                    .borrow_mut()
                    .upsert_session(hit_to_session_summary(&hit));
                self.open_session_with_find(&hit.stored_id, &query);
            }
        }
    }

    fn refresh(&self) {
        let state = self.state.borrow();
        let selected = state.selected.as_ref();
        let conversation = selected.and_then(|id| state.conversations.get(id));
        self.sync_message_list(&state, conversation);
        self.content.set_visible_child_name(
            if conversation.is_some_and(|c| !c.messages.is_empty()) {
                "messages"
            } else {
                "empty"
            },
        );
        self.title.set_text(header_title(&state));
        self.notice.set_text(&state.notice);
        self.notice.set_visible(!state.notice.is_empty());
        self.sync_lineage_bar(&state);
        let reason = conversation.and_then(|c| c.read_only_reason.as_deref());
        self.readonly.set_text(reason.unwrap_or_default());
        self.readonly_bar.set_visible(reason.is_some());
        let resume_loading = selected.is_some_and(|id| self.pending_resumes.borrow().contains(id));
        self.activity
            .set_text(activity_text(&state, resume_loading));
        self.spinner
            .set_spinning(spinner_active(&state, resume_loading));
        self.older
            .set_visible(conversation.is_some_and(|c| c.history_before > 0));
        self.older
            .set_sensitive(conversation.is_some_and(|c| !c.status.busy() && !c.history_loading));
        self.latest.set_visible(selected.is_some());
        drop(state);
        self.refresh_interaction();
        self.refresh_controls();
    }

    fn sync_message_list(&self, state: &AppState, conversation: Option<&Conversation>) {
        let connected = state.connection.as_ref() == Some(&ConnectionState::Connected);
        let gates = row_action_gates(conversation, connected, state.creating);
        let find_stamp = find_render_stamp(&state.transcript_find);
        let identity_changed = *self.rendered_session.borrow() != state.selected;
        let revision = conversation.map_or(0, |c| c.revision);
        if self.message_list_up_to_date(identity_changed, revision, gates, &find_stamp) {
            return;
        }
        if identity_changed {
            self.messages.remove_all();
            *self.rendered_session.borrow_mut() = state.selected.clone();
        }
        let current = build_message_rows(
            conversation,
            &state.transcript_find,
            connected,
            state.creating,
        );
        sync_list_store_rows(&self.messages, &current);
        self.rendered_revision.set(revision);
        self.rendered_row_gate.set(gates);
        *self.rendered_find_stamp.borrow_mut() = find_stamp;
        if self.follow_bottom.get() {
            let adjustment = self.transcript.vadjustment();
            glib::idle_add_local_once(move || {
                adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
            });
        }
    }

    fn message_list_up_to_date(
        &self,
        identity_changed: bool,
        revision: u64,
        gates: (bool, bool),
        find_stamp: &FindRenderStamp,
    ) -> bool {
        !identity_changed
            && self.rendered_revision.get() == revision
            && self.rendered_row_gate.get() == gates
            && *self.rendered_find_stamp.borrow() == *find_stamp
    }

    fn sync_lineage_bar(&self, state: &AppState) {
        let stamp = compute_lineage_stamp(state);
        if *self.rendered_lineage.borrow() == Some(stamp.clone()) {
            return;
        }
        *self.rendered_lineage.borrow_mut() = Some(stamp.clone());
        while let Some(child) = self.lineage_bar.first_child() {
            self.lineage_bar.remove(&child);
        }
        let (_, parent, children) = stamp;
        if parent.is_none() && children.is_empty() {
            self.lineage_bar.set_visible(false);
            return;
        }
        populate_lineage_bar(&self.lineage_bar, parent, &children);
        self.lineage_bar.set_visible(true);
    }

    fn refresh_model_trigger(
        &self,
        conversation: Option<&Conversation>,
        catalog: &Catalog,
        writable: bool,
    ) {
        let model = conversation.map_or(&ModelState::Unknown, |c| &c.model);
        self.model_label.set_text(&model_caption(model));
        self.model_provider
            .set_text(&model_provider_line(model, catalog));
        self.model_menu
            .set_tooltip_text(Some(&model_tooltip(model, catalog)));
        self.model_menu
            .set_sensitive(writable && !model.switching());
        *self.model_current.borrow_mut() = model.confirmed().or_else(|| model.target()).cloned();
    }

    fn refresh_controls(&self) {
        let state = self.state.borrow();
        let connected = state.connection.as_ref() == Some(&ConnectionState::Connected);
        let conversation = state
            .selected
            .as_ref()
            .and_then(|id| state.conversations.get(id));
        let writable = connected
            && conversation.is_some_and(|c| c.runtime_id.is_some() && c.read_only_reason.is_none());
        let reload_pending = state
            .selected
            .as_ref()
            .is_some_and(|id| self.pending_resumes.borrow().contains(id));
        self.editor.set_editable(editor_editable(&state));
        self.send
            .set_sensitive(composer_send_sensitive(&state, reload_pending));
        self.composer_hint.set_text(composer_hint_text(&state));
        self.delete_chat
            .set_visible(connected && state.selected.is_some());
        self.stop
            .set_visible(conversation.is_some_and(|c| c.status.busy()));
        self.stop.set_sensitive(
            connected && !conversation.is_some_and(|c| c.status == TurnStatus::Stopping),
        );
        self.latest.set_sensitive(connected && !reload_pending);
        self.new_chat.set_sensitive(connected && !state.creating);
        self.more_sessions.set_visible(state.list_has_more);
        self.more_sessions
            .set_sensitive(connected && !state.list_loading);
        self.reconnect.set_sensitive(
            self.backend.is_some()
                && !matches!(state.connection, Some(ConnectionState::Connecting)),
        );
        self.sync_branch_controls(&state, conversation, connected);
        self.sync_curation_controls(&state, conversation, connected);
        self.refresh_model_trigger(conversation, &state.catalog, writable);
        self.refresh_connection_badge(&state);
        self.sync_inspector_chrome(&state, conversation, writable);
        self.sync_studio(&state, conversation, connected, writable);
    }

    fn sync_studio(
        &self,
        state: &AppState,
        conversation: Option<&Conversation>,
        connected: bool,
        writable: bool,
    ) {
        let branch = conversation.map_or("", |c| c.telemetry.branch.as_str());
        self.workspace_picker.sync(
            &state.workspaces,
            state.effective_active_cwd(),
            branch,
            state.selected.is_some(),
            connected && !state.creating,
        );
        let editable = connected && editor_editable(state);
        self.slash_deck
            .sync(state.active_draft(), &state.skills, editable);
        let can_stage =
            can_stage_images(state, connected) && (state.selected.is_none() || writable);
        self.attachments.sync(state.active_attachments(), can_stage);
    }

    fn sync_curation_controls(
        &self,
        state: &AppState,
        conversation: Option<&Conversation>,
        connected: bool,
    ) {
        let has_chat = state.selected.is_some();
        let can_curate = connected && has_chat;
        self.find_chat.set_visible(has_chat);
        self.find_chat
            .set_sensitive(conversation.is_some_and(|c| !c.messages.is_empty()));
        self.pin_chat.set_visible(can_curate);
        self.pin_chat.set_sensitive(can_curate);
        self.rename_chat.set_visible(can_curate);
        self.rename_chat.set_sensitive(can_curate);
        sync_pin_button(&self.pin_chat, state.is_selected_pinned());
        let history_before = conversation.map_or(0, |c| c.history_before);
        let history_loading = conversation.is_some_and(|c| c.history_loading || c.status.busy());
        self.find_bar
            .sync(&state.transcript_find, history_before, history_loading);
    }

    fn sync_branch_controls(
        &self,
        state: &AppState,
        conversation: Option<&Conversation>,
        connected: bool,
    ) {
        let has_branchable =
            conversation.is_some_and(|c| count_visible_branch_messages(&c.messages) > 0);
        let (can_branch_gate, _) = row_action_gates(conversation, connected, state.creating);
        let can_branch_now = can_branch_gate && has_branchable;
        self.branch_chat
            .set_visible(state.selected.is_some() && has_branchable);
        self.branch_chat.set_sensitive(can_branch_now);
        self.readonly_fork.set_visible(has_branchable);
        self.readonly_fork.set_sensitive(can_branch_now);
    }

    fn sync_inspector_chrome(
        &self,
        state: &AppState,
        conversation: Option<&Conversation>,
        writable: bool,
    ) {
        let badge = conversation
            .and_then(|c| c.telemetry.header_badge())
            .unwrap_or_else(|| "Deck".into());
        self.inspector_badge.set_text(&badge);
        if state.inspector_open {
            self.inspector_toggle.add_css_class("inspector-open");
        } else {
            self.inspector_toggle.remove_css_class("inspector-open");
        }
        self.inspector.sync(
            state.inspector_open,
            state.inspector_tab,
            state.selected.as_deref(),
            conversation,
            writable,
        );
    }

    fn refresh_connection_badge(&self, state: &AppState) {
        self.connection.remove_css_class("connection-live");
        self.connection.remove_css_class("connection-error");
        match &state.connection {
            Some(ConnectionState::Connected) => {
                self.connection.set_text("Connected");
                self.connection.add_css_class("connection-live");
            }
            Some(ConnectionState::Connecting) => self.connection.set_text("Connecting…"),
            Some(ConnectionState::AuthenticationFailed(message)) => {
                self.connection.set_text("Authentication failed");
                self.connection.set_tooltip_text(Some(message));
                self.connection.add_css_class("connection-error");
            }
            Some(ConnectionState::Disconnected(message)) => {
                self.connection.set_text("Disconnected");
                self.connection.set_tooltip_text(Some(message));
                self.connection.add_css_class("connection-error");
            }
            None => self.connection.set_text(if self.backend.is_none() {
                "Not configured"
            } else {
                "Connecting…"
            }),
        }
    }

    fn refresh_interaction(&self) {
        let request = {
            let state = self.state.borrow();
            state
                .selected
                .as_ref()
                .and_then(|id| state.conversations.get(id))
                .and_then(|c| c.interaction.clone().zip(c.runtime_id.clone()))
        };
        let identity = request
            .as_ref()
            .map(|(request, runtime)| (runtime.clone(), request.id.clone()));
        if *self.rendered_interaction.borrow() == identity {
            return;
        }
        *self.rendered_interaction.borrow_mut() = identity;
        while let Some(child) = self.interaction.first_child() {
            self.interaction.remove(&child);
        }
        let open = request.is_some();
        self.interaction.set_visible(open);
        self.interaction_scroll.set_visible(open);
        let Some((request, runtime_id)) = request else {
            return;
        };
        if !request.prompt.is_empty() {
            self.interaction
                .append(&full_label(&request.prompt, "clarify-prompt"));
        }
        match request.kind {
            InteractionKind::Approval => self.build_approval(&request, &runtime_id),
            InteractionKind::Clarification => self.build_clarification(&request, &runtime_id),
        }
        self.interaction.set_sensitive(true);
    }

    fn build_approval(&self, request: &InteractionRequest, runtime_id: &str) {
        let choices = if request.choices.is_empty() {
            vec!["once".to_string(), "deny".to_string()]
        } else {
            request.choices.clone()
        };
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_hexpand(true);
        for choice in choices {
            let button = gtk::Button::with_label(approval_label(&choice));
            button.update_property(&[gtk::accessible::Property::Label(approval_label(&choice))]);
            if choice == "deny" {
                button.add_css_class("destructive-action");
            }
            let runtime_id = runtime_id.to_string();
            let request_id = request.id.clone();
            let offered = choice.clone();
            button.connect_clicked(move |button| {
                dispatch_respond(
                    button,
                    &runtime_id,
                    &request_id,
                    glib::Variant::from(offered.as_str()),
                );
            });
            buttons.append(&button);
        }
        self.interaction.append(&buttons);
    }

    fn build_clarification(&self, request: &InteractionRequest, runtime_id: &str) {
        let slots = Rc::new(RefCell::new(Vec::<ClarifySlot>::new()));
        for question in &request.questions {
            if slots.borrow().iter().any(|slot| slot.qid == question.qid) {
                continue;
            }
            let block = gtk::Box::new(gtk::Orientation::Vertical, 6);
            block.add_css_class("clarify-question");
            let prompt = full_label(&question.prompt, "clarify-prompt");
            prompt.update_property(&[gtk::accessible::Property::Description(&format!(
                "Question {}",
                question.qid
            ))]);
            block.append(&prompt);
            let locked = request.answers.get(&question.qid).cloned();
            let slot = if let Some(value) = locked {
                let text = value.as_deref().unwrap_or("Skipped");
                let shown = full_label(text, "clarify-locked");
                shown.set_sensitive(false);
                block.append(&shown);
                ClarifySlot {
                    qid: question.qid.clone(),
                    locked: Some(value),
                    skipped: Rc::new(Cell::new(false)),
                    entry: None,
                    checks: Vec::new(),
                    multi: question.multi_select,
                }
            } else {
                let skipped = Rc::new(Cell::new(false));
                let controls = gtk::Box::new(gtk::Orientation::Vertical, 4);
                let (entry, checks) = if question.choices.is_empty() {
                    let entry = gtk::Entry::new();
                    entry.set_placeholder_text(Some("Your answer"));
                    entry.update_property(&[gtk::accessible::Property::Label(&format!(
                        "Answer for {}",
                        question.qid
                    ))]);
                    controls.append(&entry);
                    (Some(entry), Vec::new())
                } else {
                    let mut checks = Vec::new();
                    let mut leader = None;
                    for choice in &question.choices {
                        let check = gtk::CheckButton::with_label(choice);
                        check.update_property(&[gtk::accessible::Property::Label(choice.as_str())]);
                        if !question.multi_select {
                            if let Some(leader) = &leader {
                                check.set_group(Some(leader));
                            } else {
                                leader = Some(check.clone());
                            }
                        }
                        controls.append(&check);
                        checks.push((choice.clone(), check));
                    }
                    (None, checks)
                };
                block.append(&controls);
                let skip = gtk::Button::with_label("Skip");
                skip.add_css_class("flat");
                let skipped_flag = Rc::clone(&skipped);
                let controls_for_skip = controls.clone();
                skip.connect_clicked(move |button| {
                    let next = !skipped_flag.get();
                    skipped_flag.set(next);
                    controls_for_skip.set_sensitive(!next);
                    button.set_label(if next { "Answer instead" } else { "Skip" });
                });
                block.append(&skip);
                ClarifySlot {
                    qid: question.qid.clone(),
                    locked: None,
                    skipped,
                    entry,
                    checks,
                    multi: question.multi_select,
                }
            };
            slots.borrow_mut().push(slot);
            self.interaction.append(&block);
        }
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let submit = gtk::Button::with_label("Submit answers");
        submit.add_css_class("suggested-action");
        let submit_runtime = runtime_id.to_string();
        let request_id = request.id.clone();
        let collected = Rc::clone(&slots);
        submit.connect_clicked(move |button| {
            let Some(answers) = collect_clarification(&collected.borrow()) else {
                return;
            };
            dispatch_respond(
                button,
                &submit_runtime,
                &request_id,
                glib::Variant::from(&answers),
            );
        });
        let cancel = gtk::Button::with_label("Skip all");
        cancel.add_css_class("flat");
        let runtime_id = runtime_id.to_string();
        let request_id = request.id.clone();
        cancel.connect_clicked(move |button| {
            dispatch_respond(button, &runtime_id, &request_id, glib::Variant::from(false));
        });
        buttons.append(&submit);
        buttons.append(&cancel);
        self.interaction.append(&buttons);
    }
}

fn prepare_compress_command(state: &AppState) -> Option<(String, Command)> {
    if state.connection.as_ref() != Some(&ConnectionState::Connected) {
        return None;
    }
    let stored_id = state.selected.clone()?;
    let c = state.conversations.get(&stored_id)?;
    if c.read_only_reason.is_some() || c.status.busy() || c.rewinding || c.messages.len() < 2 {
        return None;
    }
    let runtime_id = c.runtime_id.clone()?;
    let cmd = Command::Compress {
        stored_id: stored_id.clone(),
        runtime_id,
    };
    Some((stored_id, cmd))
}

fn apply_compressed_state(
    state: &mut AppState,
    stored_id: &str,
    runtime_id: String,
    messages: Option<Vec<ChatMessage>>,
    before: usize,
    summary: &CompressSummary,
) {
    if let Some(msgs) = &messages
        && let Some(row) = state.sessions.iter_mut().find(|s| s.stored_id == stored_id)
    {
        row.message_count = before + msgs.len();
    }
    let Some(c) = state.conversations.get_mut(stored_id) else {
        return;
    };
    c.runtime_id = Some(runtime_id);
    c.rewinding = false;
    c.activity = summary.status_line();
    if let Some(msgs) = messages {
        c.messages = msgs;
        c.history_before = before;
        c.revision += 1;
    }
    c.inspector_revision += 1;
}

fn can_stage_images(state: &AppState, connected: bool) -> bool {
    if !connected {
        return false;
    }
    match &state.selected {
        None => !state.creating && state.pending_launch.is_none(),
        Some(id) => state.conversations.get(id).is_some_and(|c| {
            c.runtime_id.is_some()
                && c.read_only_reason.is_none()
                && !c.rewinding
                && !c.status.busy()
        }),
    }
}

fn workspace_pick_starts_new_chat(state: &AppState, chosen: Option<&str>) -> bool {
    let Some(id) = &state.selected else {
        return false;
    };
    let current = state
        .conversations
        .get(id)
        .map(|c| c.telemetry.cwd.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(state.workspaces.default_cwd.as_str());
    let target = chosen
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(state.workspaces.default_cwd.as_str());
    current != target
}

fn submit_activity_label(has_attachments: bool) -> &'static str {
    if has_attachments {
        "Uploading image & sending…"
    } else {
        "Sending…"
    }
}

fn prepare_branch_command(
    state: &AppState,
    end_inclusive: Option<usize>,
    custom_title: Option<String>,
) -> Option<(String, Command)> {
    if state.connection.as_ref() != Some(&ConnectionState::Connected) || state.creating {
        return None;
    }
    let stored_id = state.selected.clone()?;
    let c = state.conversations.get(&stored_id)?;
    if c.status.busy() || c.rewinding {
        return None;
    }
    let seed_fallback = seed_messages_through(&c.messages, end_inclusive);
    if seed_fallback.is_empty() {
        return None;
    }
    let visible_count =
        end_inclusive.and_then(|idx| visible_branch_count_through(&c.messages, idx));
    let parent_title = session_display_title(state, &stored_id);
    let runtime_id = c
        .runtime_id
        .clone()
        .filter(|_| c.read_only_reason.is_none());
    let cmd = Command::Branch {
        stored_id: stored_id.clone(),
        runtime_id,
        parent_title,
        before_loaded: c.history_before,
        visible_count,
        custom_title,
        seed_fallback,
    };
    Some((stored_id, cmd))
}

fn prepare_rewind_command(state: &AppState, mode: RewindMode) -> Option<(String, String, Command)> {
    if state.connection.as_ref() != Some(&ConnectionState::Connected) {
        return None;
    }
    let stored_id = state.selected.clone()?;
    let c = state.conversations.get(&stored_id)?;
    if c.read_only_reason.is_some() || c.status.busy() || c.rewinding {
        return None;
    }
    let runtime_id = c.runtime_id.clone()?;
    let (_, _, prompt_text) = last_turn_targets(&c.messages);
    let prompt_text = prompt_text?;
    let cmd = Command::Undo {
        stored_id: stored_id.clone(),
        runtime_id: runtime_id.clone(),
        mode,
        prompt_text,
    };
    Some((stored_id, runtime_id, cmd))
}

fn insert_branched_state(
    state: &mut AppState,
    session: SessionSummary,
    runtime_id: String,
    parent_title: String,
    messages: Vec<ChatMessage>,
    before: usize,
    model: Option<ModelIdentity>,
) {
    let id = session.stored_id.clone();
    let parent_id = session.parent_session_id.clone();
    clear_parent_branch_activity(state, parent_id.as_deref());
    state.upsert_session(session);
    promote_session_front(state, &id);
    if let Some(pid) = &parent_id {
        state.record_branch(&id, pid, &parent_title);
    } else {
        state.sync_branch_flags();
    }
    let mut conversation = Conversation {
        runtime_id: Some(runtime_id),
        messages,
        history_before: before,
        loaded: true,
        activity: format!("Branched from “{parent_title}”."),
        revision: 1,
        inspector_revision: 1,
        ..Default::default()
    };
    if let Some(model) = model {
        conversation.confirm_model(model);
    }
    state.conversations.insert(id, conversation);
    state.creating = false;
}

fn clear_parent_branch_activity(state: &mut AppState, parent_id: Option<&str>) {
    let Some(pid) = parent_id else {
        return;
    };
    if let Some(parent_conv) = state.conversations.get_mut(pid)
        && parent_conv.activity == "Creating branch…"
    {
        parent_conv.activity.clear();
    }
}

fn promote_session_front(state: &mut AppState, id: &str) {
    if let Some(pos) = state.sessions.iter().position(|s| s.stored_id == id)
        && pos > 0
    {
        let item = state.sessions.remove(pos);
        state.sessions.insert(0, item);
    }
}

fn sync_pin_button(button: &gtk::Button, pinned: bool) {
    if pinned {
        button.set_icon_name("starred-symbolic");
        button.add_css_class("pin-active");
        button.set_tooltip_text(Some("Unpin conversation · Ctrl+P"));
    } else {
        button.set_icon_name("non-starred-symbolic");
        button.remove_css_class("pin-active");
        button.set_tooltip_text(Some("Pin conversation · Ctrl+P"));
    }
}

fn apply_search_event(
    state: &mut AppState,
    query: &str,
    request_seq: u64,
    hits: &[SearchHit],
) -> MetaOutcome {
    if state.apply_search_results(query.to_string(), request_seq, hits.to_vec()) {
        MetaOutcome::RefreshSessions
    } else {
        MetaOutcome::Handled
    }
}

fn apply_mutation_event(
    state: &mut AppState,
    stored_id: &str,
    mutation: &SessionMutation,
) -> MetaOutcome {
    match state.apply_mutation(stored_id, mutation) {
        MutationOutcome::ArchivedSelected => MetaOutcome::ResetToLaunch,
        MutationOutcome::Updated | MutationOutcome::ArchivedOther => MetaOutcome::RefreshSessions,
    }
}

fn sync_find_after_transcript_load(state: &mut AppState, stored_id: &str) -> Option<usize> {
    if state.selected.as_deref() != Some(stored_id) {
        return None;
    }
    let messages = state
        .conversations
        .get(stored_id)
        .map(|c| c.messages.clone())
        .unwrap_or_default();
    if let Some(query) = state.pending_find_query.take() {
        state.transcript_find.open_with(&query, &messages);
        return state.transcript_find.active_message_index();
    }
    if state.transcript_find.open {
        state.transcript_find.recompute(&messages);
    }
    None
}

fn sync_find_after_history_page(state: &mut AppState, stored_id: &str) -> Option<usize> {
    if state.selected.as_deref() != Some(stored_id) || !state.transcript_find.open {
        return None;
    }
    let messages = state
        .conversations
        .get(stored_id)
        .map(|c| c.messages.clone())
        .unwrap_or_default();
    state.transcript_find.recompute(&messages);
    state.transcript_find.active_message_index()
}

fn build_sidebar_rows(state: &AppState) -> (Vec<glib::BoxedAnyObject>, u32) {
    let indices = state.visible_sessions();
    let mut rows: Vec<glib::BoxedAnyObject> = indices
        .iter()
        .filter_map(|&index| state.sessions.get(index))
        .cloned()
        .map(SidebarRowItem::Local)
        .map(glib::BoxedAnyObject::new)
        .collect();
    let query = state.session_query.trim().to_string();
    for hit in state.remote_only_sidebar_hits() {
        rows.push(glib::BoxedAnyObject::new(SidebarRowItem::RemoteHit {
            hit: hit.clone(),
            query: query.clone(),
        }));
    }
    let selected = sidebar_selected_index(state);
    (rows, selected)
}

fn sidebar_selected_index(state: &AppState) -> u32 {
    let indices = state.visible_sessions();
    let local_pos = visible_selected_position(state, &indices);
    if local_pos != gtk::INVALID_LIST_POSITION {
        return local_pos;
    }
    let Some(selected_id) = state.selected.as_deref() else {
        return gtk::INVALID_LIST_POSITION;
    };
    state
        .remote_only_sidebar_hits()
        .iter()
        .position(|hit| hit.stored_id == selected_id)
        .map_or(gtk::INVALID_LIST_POSITION, |pos| {
            (indices.len() + pos) as u32
        })
}

fn local_fts_handoff(state: &AppState, stored_id: &str) -> Option<String> {
    let q = state.session_query.trim();
    if q.chars().count() < 2 || !state.has_active_remote_search(q) {
        return None;
    }
    state
        .remote_search_hits
        .iter()
        .any(|hit| hit.stored_id == stored_id)
        .then(|| q.to_string())
}

fn ensure_session_from_search_hit(state: &mut AppState, stored_id: &str) {
    if state.sessions.iter().any(|s| s.stored_id == stored_id) {
        return;
    }
    let summary = state
        .remote_search_hits
        .iter()
        .find(|hit| hit.stored_id == stored_id)
        .map(hit_to_session_summary);
    if let Some(summary) = summary {
        state.upsert_session(summary);
    }
}

fn hit_to_session_summary(hit: &SearchHit) -> SessionSummary {
    SessionSummary {
        stored_id: hit.stored_id.clone(),
        title: hit.title.clone(),
        source: hit.source.clone(),
        updated_at: hit.updated_at.clone(),
        read_only_reason: None,
        message_count: hit.message_count,
        parent_session_id: hit.parent_session_id.clone(),
        is_branch: hit.parent_session_id.is_some(),
        pinned: hit.pinned,
        preview: hit.snippet_plain.clone(),
        model: hit.model.clone(),
    }
}

fn apply_rewound_conversation(
    c: &mut Conversation,
    mode: RewindMode,
    prompt_text: &str,
    messages: Vec<ChatMessage>,
    before: usize,
) {
    c.rewinding = false;
    c.messages = messages;
    c.history_before = before;
    c.status = TurnStatus::Idle;
    c.interaction = None;
    c.clear_turn_reasoning();
    c.revision += 1;
    c.inspector_revision += 1;
    match mode {
        RewindMode::Undo => {
            c.activity = "Rewound last turn.".into();
            if c.draft.trim().is_empty() {
                c.draft = prompt_text.to_string();
            }
        }
        RewindMode::EditRetry => {
            c.activity = "Turn rewound — edit your prompt and press Enter.".into();
            c.draft = prompt_text.to_string();
        }
        RewindMode::Regenerate => {
            c.activity = "Regenerating response…".into();
        }
    }
}

fn sync_list_store_rows(store: &gio::ListStore, current: &[MessageRowData]) {
    if store.n_items() as usize > current.len() {
        store.splice(
            current.len() as u32,
            store.n_items() - current.len() as u32,
            &[] as &[glib::BoxedAnyObject],
        );
    }
    for (index, row_data) in current.iter().enumerate() {
        let same = store
            .item(index as u32)
            .and_downcast::<glib::BoxedAnyObject>()
            .is_some_and(|v| *v.borrow::<MessageRowData>() == *row_data);
        if !same {
            let value = glib::BoxedAnyObject::new(row_data.clone());
            if (index as u32) < store.n_items() {
                store.splice(index as u32, 1, &[value]);
            } else {
                store.append(&value);
            }
        }
    }
}

fn populate_lineage_bar(
    bar: &gtk::Box,
    parent: Option<(String, String)>,
    children: &[(String, String)],
) {
    let badge = gtk::Label::new(Some("⑂"));
    badge.add_css_class("fork-badge");
    bar.append(&badge);
    let summary = gtk::Label::new(Some(&lineage_summary_text(parent.as_ref(), children.len())));
    summary.set_xalign(0.0);
    summary.set_hexpand(true);
    summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
    summary.add_css_class("lineage-label");
    bar.append(&summary);
    if let Some((pid, ptitle)) = parent {
        let up = gtk::Button::with_label("↖ Parent");
        up.add_css_class("flat");
        up.add_css_class("lineage-btn");
        up.set_tooltip_text(Some(&format!("Jump to parent conversation “{ptitle}”")));
        up.connect_clicked(move |btn| {
            let _ = btn.activate_action("win.open-session", Some(&pid.to_variant()));
        });
        bar.append(&up);
    }
    if !children.is_empty() {
        bar.append(&build_branches_menu(children));
    }
}

fn lineage_summary_text(parent: Option<&(String, String)>, child_count: usize) -> String {
    if let Some((_, ptitle)) = parent {
        return format!("Branched from “{ptitle}”");
    }
    let noun = if child_count == 1 {
        "branch"
    } else {
        "branches"
    };
    format!("Parent of {child_count} {noun}")
}

fn build_branches_menu(children: &[(String, String)]) -> gtk::MenuButton {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    set_margins(&list, 6);
    list.set_width_request(240);
    let popover = gtk::Popover::new();
    for (cid, ctitle) in children {
        let row_btn = gtk::Button::with_label(&format!("⑂  {ctitle}"));
        row_btn.add_css_class("flat");
        row_btn.add_css_class("branch-popover-row");
        let cid = cid.clone();
        let pop = popover.clone();
        row_btn.connect_clicked(move |btn| {
            pop.popdown();
            let _ = btn.activate_action("win.open-session", Some(&cid.to_variant()));
        });
        list.append(&row_btn);
    }
    popover.set_child(Some(&list));
    let menu = gtk::MenuButton::new();
    menu.set_label(&format!("⑂ Branches · {}", children.len()));
    menu.add_css_class("flat");
    menu.add_css_class("lineage-btn");
    menu.set_tooltip_text(Some("Jump to a conversation branched from this thread"));
    menu.set_popover(Some(&popover));
    menu
}

fn session_display_title(state: &AppState, stored_id: &str) -> String {
    state
        .sessions
        .iter()
        .find(|s| s.stored_id == stored_id)
        .map(|s| s.title.trim())
        .filter(|t| !t.is_empty())
        .unwrap_or("Conversation")
        .to_string()
}

struct ClarifySlot {
    qid: String,
    locked: Option<Option<String>>,
    skipped: Rc<Cell<bool>>,
    entry: Option<gtk::Entry>,
    checks: Vec<(String, gtk::CheckButton)>,
    multi: bool,
}

fn full_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class(class);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_selectable(true);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label
}

fn approval_label(choice: &str) -> &str {
    match choice {
        "once" => "Allow once",
        "session" => "Allow for this chat",
        "always" => "Always allow",
        "deny" => "Deny",
        other => other,
    }
}

fn dispatch_respond(button: &gtk::Button, runtime_id: &str, request_id: &str, body: glib::Variant) {
    let payload = (runtime_id, request_id, body).to_variant();
    let _ = button.activate_action("win.respond", Some(&payload));
}

fn decode_interaction_answer(body: &glib::Variant) -> Option<InteractionAnswer> {
    if let Some(choice) = body.get::<String>() {
        return Some(InteractionAnswer::Approval(choice));
    }
    if body.get::<bool>().is_some() {
        return Some(InteractionAnswer::Cancel);
    }
    body.get::<HashMap<String, Option<String>>>()
        .map(InteractionAnswer::Clarification)
}

fn collect_clarification(slots: &[ClarifySlot]) -> Option<HashMap<String, Option<String>>> {
    let mut answers = HashMap::new();
    for slot in slots {
        if answers.contains_key(&slot.qid) {
            return None;
        }
        if let Some(locked) = &slot.locked {
            answers.insert(slot.qid.clone(), locked.clone());
            continue;
        }
        if slot.skipped.get() {
            answers.insert(slot.qid.clone(), None);
            continue;
        }
        if let Some(entry) = &slot.entry {
            let text = entry.text().trim().to_string();
            if text.is_empty() {
                entry.grab_focus();
                return None;
            }
            answers.insert(slot.qid.clone(), Some(text));
            continue;
        }
        let selected: Vec<String> = slot
            .checks
            .iter()
            .filter(|(_, check)| check.is_active())
            .map(|(choice, _)| choice.clone())
            .collect();
        if selected.is_empty() || (!slot.multi && selected.len() != 1) {
            if let Some((_, check)) = slot.checks.first() {
                check.grab_focus();
            }
            return None;
        }
        let value = if slot.multi {
            serde_json::to_string(&selected).ok()?
        } else {
            selected.into_iter().next()?
        };
        answers.insert(slot.qid.clone(), Some(value));
    }
    Some(answers)
}

fn text_icon_button(icon: &str, label: &str) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.set_halign(gtk::Align::Center);
    content.append(&gtk::Image::from_icon_name(icon));
    content.append(&gtk::Label::new(Some(label)));
    let button = gtk::Button::new();
    button.set_child(Some(&content));
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn icon_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// Speaker label, row class, and speaker class for a message role, so each
/// role reads differently in the transcript.
fn role_style(role: &Role) -> (&'static str, &'static str, &'static str) {
    match role {
        Role::User => ("YOU", "message-user", "speaker-user"),
        Role::Assistant => ("HERMES", "message-assistant", "speaker-assistant"),
        Role::Tool => ("TOOL", "message-tool", "speaker-tool"),
        Role::System => ("SYSTEM", "message-system", "speaker-system"),
    }
}

/// Tool rows are compact single-line chips; every other role renders Markdown.
fn message_body(message: &ChatMessage) -> gtk::Widget {
    if message.role == Role::Tool {
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        chip.add_css_class("tool-chip");
        let icon = gtk::Image::from_icon_name("system-run-symbolic");
        icon.add_css_class("tool-icon");
        icon.set_valign(gtk::Align::Center);
        chip.append(&icon);
        let label = gtk::Label::new(Some(&message.text));
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.add_css_class("tool-label");
        chip.append(&label);
        chip.upcast()
    } else {
        let rendered = markdown::render(&message.text);
        rendered.add_css_class("message-prose");
        rendered
    }
}

fn set_margins(widget: &impl IsA<gtk::Widget>, amount: i32) {
    widget.set_margin_top(amount);
    widget.set_margin_bottom(amount);
    widget.set_margin_start(amount);
    widget.set_margin_end(amount);
}

fn watch_sidebar_clicks(list: &gtk::ListView, clicking: &Rc<Cell<bool>>, ui: &Rc<Ui>) {
    let capture = gtk::EventControllerLegacy::new();
    capture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let flag = Rc::clone(clicking);
    let series: Rc<Cell<Option<ClickSeries>>> = Rc::new(Cell::new(None));
    let generation = Rc::new(Cell::new(0u32));
    let click_list = list.clone();
    capture.connect_event(move |_, event| {
        if event.event_type() == gdk::EventType::KeyPress {
            // Keybindings run after capture. They must not consume a stale click.
            flag.set(false);
        } else if is_primary_button(event, gdk::EventType::ButtonPress) {
            let press_generation = generation.get().wrapping_add(1);
            generation.set(press_generation);
            flag.set(true);
            if claims_double_click_release(event, &click_list, series.as_ref()) {
                // The row gesture claims this release during the press, so bubble never sees it.
                // Retire after dispatch, and only if no later press replaced this click.
                let flag = Rc::clone(&flag);
                let generation = Rc::clone(&generation);
                glib::idle_add_local_once(move || {
                    if generation.get() == press_generation {
                        flag.set(false);
                    }
                });
            }
        }
        glib::Propagation::Proceed
    });
    list.add_controller(capture);

    let bubble = gtk::EventControllerLegacy::new();
    bubble.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let flag = Rc::clone(clicking);
    let weak = Rc::downgrade(ui);
    let click_list = list.clone();
    bubble.connect_event(move |_, event| {
        if is_primary_button(event, gdk::EventType::ButtonRelease) {
            let was_click = flag.replace(false);
            // Same-row clicks do not notify. Changed rows already consumed the flag.
            if was_click
                && click_list.focus_child().is_some()
                && let Some(ui) = weak.upgrade()
            {
                ui.editor.grab_focus();
            }
        }
        glib::Propagation::Proceed
    });
    list.add_controller(bubble);
}

#[derive(Clone, Copy)]
struct ClickSeries {
    origin_x: f64,
    origin_y: f64,
    last_time: u32,
    n: u32,
}

fn claims_double_click_release(
    event: &gdk::Event,
    list: &gtk::ListView,
    series: &Cell<Option<ClickSeries>>,
) -> bool {
    let Some(button) = event.downcast_ref::<gdk::ButtonEvent>() else {
        return false;
    };
    let Some((x, y)) = button.position() else {
        series.set(None);
        return false;
    };
    let time = button.time();
    let settings = list.settings();
    let distance = settings.gtk_double_click_distance().max(0) as f64;
    let window = settings.gtk_double_click_time().max(0) as u32;
    let next = match series.get() {
        Some(prev)
            if time.wrapping_sub(prev.last_time) <= window
                && (prev.origin_x - x).abs() < distance
                && (prev.origin_y - y).abs() < distance =>
        {
            ClickSeries {
                origin_x: prev.origin_x,
                origin_y: prev.origin_y,
                last_time: time,
                n: prev.n.saturating_add(1),
            }
        }
        _ => ClickSeries {
            origin_x: x,
            origin_y: y,
            last_time: time,
            n: 1,
        },
    };
    let claims = next.n == 2;
    series.set(Some(next));
    claims
}

fn is_primary_button(event: &gdk::Event, kind: gdk::EventType) -> bool {
    event.event_type() == kind
        && event
            .downcast_ref::<gdk::ButtonEvent>()
            .is_some_and(|button| button.button() == gdk::BUTTON_PRIMARY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(model: &str) -> ModelIdentity {
        ModelIdentity {
            provider: "custom:acme".into(),
            model: model.into(),
        }
    }

    fn catalog() -> Catalog {
        Catalog::parse(&serde_json::json!({"providers": [
            {"slug": "acme", "name": "Acme", "is_user_defined": true, "models": ["one", "two"]}
        ]}))
        .expect("catalog")
    }

    #[test]
    fn trigger_text_never_claims_an_unconfirmed_selection() {
        let catalog = catalog();
        assert_eq!(model_caption(&ModelState::Unknown), "Choose a model");
        assert_eq!(
            model_provider_line(&ModelState::Unknown, &catalog),
            "server default"
        );
        assert_eq!(model_caption(&ModelState::Confirmed(pair("one"))), "one");
        assert_eq!(
            model_provider_line(&ModelState::Confirmed(pair("one")), &catalog),
            "Acme"
        );
        assert_eq!(
            model_caption(&ModelState::Switching {
                from: Some(pair("one")),
                target: pair("two")
            }),
            "Switching to two…"
        );
        assert_eq!(
            model_caption(&ModelState::Queued {
                from: Some(pair("one")),
                target: pair("two")
            }),
            "two · next turn"
        );
        assert_eq!(
            model_caption(&ModelState::Unresolved(None)),
            "Model unconfirmed"
        );
        assert_eq!(
            model_caption(&ModelState::Unresolved(Some(pair("one")))),
            "one · unconfirmed"
        );
    }

    #[test]
    fn queued_switch_says_it_applies_from_the_next_message() {
        let catalog = catalog();
        let tooltip = model_tooltip(
            &ModelState::Queued {
                from: Some(pair("one")),
                target: pair("two"),
            },
            &catalog,
        );
        assert!(tooltip.contains("next message"), "{tooltip}");
        assert!(tooltip.contains("Acme"), "{tooltip}");
    }

    #[test]
    fn launch_composer_is_editable_and_arms_send_when_connected() {
        let mut state = AppState::default();
        assert!(editor_editable(&state));
        assert!(!composer_send_sensitive(&state, false));
        assert!(composer_hint_text(&state).contains("Enter to start a new chat"));

        state.launch_draft = "  Explain Wayland  ".into();
        assert!(!composer_send_sensitive(&state, false));
        state.connection = Some(ConnectionState::Connected);
        assert!(composer_send_sensitive(&state, false));

        let _ = state.begin_launch_submit();
        assert!(!editor_editable(&state));
        assert!(!composer_send_sensitive(&state, false));
        assert_eq!(activity_text(&state, false), "Starting conversation…");
        assert!(spinner_active(&state, false));
    }

    #[test]
    fn conversation_controls_track_readonly_steering_and_filtered_selection() {
        let mut state = AppState {
            connection: Some(ConnectionState::Connected),
            sessions: vec![
                SessionSummary {
                    stored_id: "cron-1".into(),
                    title: String::new(),
                    source: "cron".into(),
                    message_count: 1,
                    ..Default::default()
                },
                SessionSummary {
                    stored_id: "chat-1".into(),
                    title: "Hyprland rice".into(),
                    source: "tui".into(),
                    message_count: 4,
                    ..Default::default()
                },
            ],
            selected: Some("cron-1".into()),
            ..Default::default()
        };
        assert_eq!(header_title(&state), "Hermes Native");
        state.selected = Some("chat-1".into());
        assert_eq!(header_title(&state), "Hyprland rice");

        state.session_filter = SessionFilter::Chats;
        let visible = state.visible_sessions();
        assert_eq!(visible_selected_position(&state, &visible), 0);
        state.session_filter = SessionFilter::Auto;
        let visible = state.visible_sessions();
        assert_eq!(
            visible_selected_position(&state, &visible),
            gtk::INVALID_LIST_POSITION
        );

        state.conversations.insert(
            "chat-1".into(),
            Conversation {
                runtime_id: Some("rt-1".into()),
                draft: "steer this".into(),
                status: TurnStatus::Streaming,
                ..Default::default()
            },
        );
        assert!(composer_send_sensitive(&state, false));
        assert!(composer_hint_text(&state).contains("steer"));
    }

    #[test]
    fn shortcut_command_accepts_plain_ctrl_letters_only() {
        assert_eq!(
            shortcut_command(gdk::Key::k, gdk::ModifierType::CONTROL_MASK),
            Some('k')
        );
        assert_eq!(
            shortcut_command(gdk::Key::K, gdk::ModifierType::CONTROL_MASK),
            Some('k')
        );
        assert_eq!(
            shortcut_command(
                gdk::Key::k,
                gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK
            ),
            None
        );
        assert_eq!(
            shortcut_command(gdk::Key::k, gdk::ModifierType::empty()),
            None
        );
    }

    #[test]
    fn btw_prefix_surfaces_snapshot_hint_and_allows_side_questions_while_stopping() {
        let mut state = AppState {
            connection: Some(ConnectionState::Connected),
            selected: Some("chat-1".into()),
            ..Default::default()
        };
        let mut chat = Conversation {
            runtime_id: Some("rt-1".into()),
            status: TurnStatus::Stopping,
            draft: "normal prompt".into(),
            ..Default::default()
        };
        assert!(!can_send_in_conversation(&chat));
        chat.draft = "/btw what file did we edit?".into();
        assert!(can_send_in_conversation(&chat));
        state.conversations.insert("chat-1".into(), chat);
        assert!(composer_send_sensitive(&state, false));
        assert!(composer_hint_text(&state).contains("never saved to history"));
    }

    #[test]
    fn message_rows_expose_branch_and_rewind_actions_only_when_idle() {
        let mut chat = Conversation {
            runtime_id: Some("rt-1".into()),
            messages: vec![
                ChatMessage {
                    id: "u1".into(),
                    role: Role::User,
                    text: "Turn 1".into(),
                },
                ChatMessage {
                    id: "t1".into(),
                    role: Role::Tool,
                    text: "Used read_file".into(),
                },
                ChatMessage {
                    id: "a1".into(),
                    role: Role::Assistant,
                    text: "Answer 1".into(),
                },
                ChatMessage {
                    id: "u2".into(),
                    role: Role::User,
                    text: "Turn 2".into(),
                },
                ChatMessage {
                    id: "a2".into(),
                    role: Role::Assistant,
                    text: "Answer 2".into(),
                },
            ],
            ..Default::default()
        };
        let no_find = TranscriptFind::default();
        let rows = build_message_rows(Some(&chat), &no_find, true, false);
        assert_eq!(rows.len(), 5);
        assert!(rows[0].can_branch && !rows[0].can_edit_retry);
        assert!(!rows[1].can_branch);
        assert!(rows[2].can_branch && !rows[2].can_regenerate);
        assert!(rows[3].can_branch && rows[3].can_edit_retry);
        assert!(rows[4].can_branch && rows[4].can_regenerate);

        // Busy or read-only gates disable live rewind; read-only still allows branching.
        chat.status = TurnStatus::Streaming;
        let busy_rows = build_message_rows(Some(&chat), &no_find, true, false);
        assert!(
            busy_rows
                .iter()
                .all(|r| !r.can_branch && !r.can_edit_retry && !r.can_regenerate)
        );

        chat.status = TurnStatus::Idle;
        chat.runtime_id = None;
        chat.read_only_reason = Some("Read-only cron log".into());
        let ro_rows = build_message_rows(Some(&chat), &no_find, true, false);
        assert!(ro_rows[0].can_branch);
        assert!(!ro_rows[3].can_edit_retry);
        assert!(!ro_rows[4].can_regenerate);
    }

    #[test]
    fn transcript_find_and_curation_helpers_flag_active_matches_and_slash_hints() {
        let messages = vec![
            ChatMessage {
                id: "u1".into(),
                role: Role::User,
                text: "Investigate Hyprland blur".into(),
            },
            ChatMessage {
                id: "a1".into(),
                role: Role::Assistant,
                text: "Hyprland decoration blur uses Kawase passes".into(),
            },
        ];
        let mut find = TranscriptFind::default();
        find.open_with("hyprland", &messages);
        let chat = Conversation {
            runtime_id: Some("rt-1".into()),
            messages,
            draft: "/find kawase".into(),
            ..Default::default()
        };
        assert!(can_send_in_conversation(&chat));
        assert!(composer_conversation_hint(&chat).contains("search within this transcript"));

        let rows = build_message_rows(Some(&chat), &find, true, false);
        assert!(rows[0].is_match && rows[0].is_active_match);
        assert!(rows[1].is_match && !rows[1].is_active_match);
        find.step(1);
        let stepped = build_message_rows(Some(&chat), &find, true, false);
        assert!(stepped[1].is_match && stepped[1].is_active_match);
        assert_eq!(
            ctrl_shift_command(
                gdk::Key::F,
                gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK
            ),
            Some('f')
        );
        assert!(is_plain_f2(gdk::Key::F2, gdk::ModifierType::empty()));
    }

    #[test]
    fn prepare_branch_and_rewind_commands_and_lineage_summary() {
        let mut state = AppState {
            connection: Some(ConnectionState::Connected),
            selected: Some("root".into()),
            sessions: vec![SessionSummary {
                stored_id: "root".into(),
                title: "Compositor notes".into(),
                source: "tui".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut chat = Conversation {
            runtime_id: Some("rt-root".into()),
            draft: "/branch Wayland take".into(),
            messages: vec![
                ChatMessage {
                    id: "u1".into(),
                    role: Role::User,
                    text: "First".into(),
                },
                ChatMessage {
                    id: "a1".into(),
                    role: Role::Assistant,
                    text: "Reply".into(),
                },
            ],
            ..Default::default()
        };
        assert!(can_send_in_conversation(&chat));
        assert!(composer_conversation_hint(&chat).contains("fork conversation"));
        chat.draft = "/retry".into();
        assert!(can_send_in_conversation(&chat));
        assert!(composer_conversation_hint(&chat).contains("rewind last turn"));
        state.conversations.insert("root".into(), chat);

        let (_, branch_cmd) =
            prepare_branch_command(&state, Some(0), Some("Point fork".into())).expect("branch");
        assert!(matches!(
            branch_cmd,
            Command::Branch {
                visible_count: Some(1),
                ..
            }
        ));
        let (_, _, rewind_cmd) =
            prepare_rewind_command(&state, RewindMode::EditRetry).expect("rewind");
        assert!(matches!(
            rewind_cmd,
            Command::Undo {
                mode: RewindMode::EditRetry,
                ..
            }
        ));

        insert_branched_state(
            &mut state,
            SessionSummary {
                stored_id: "child-1".into(),
                title: "Compositor notes (branch)".into(),
                source: "tui".into(),
                parent_session_id: Some("root".into()),
                ..Default::default()
            },
            "rt-child".into(),
            "Compositor notes".into(),
            Vec::new(),
            0,
            None,
        );
        let (_, parent_none, children) = compute_lineage_stamp(&state);
        assert!(parent_none.is_none());
        assert_eq!(children.len(), 1);
        assert_eq!(lineage_summary_text(None, 1), "Parent of 1 branch");

        state.selected = Some("child-1".into());
        let (_, parent_some, _) = compute_lineage_stamp(&state);
        assert_eq!(
            lineage_summary_text(parent_some.as_ref(), 0),
            "Branched from “Compositor notes”"
        );
        assert_eq!(parse_rewind_action("edit"), Some(RewindMode::EditRetry));
        assert_eq!(parse_rewind_action("regen"), Some(RewindMode::Regenerate));
        assert_eq!(parse_rewind_action("undo"), Some(RewindMode::Undo));

        let mut rewound = Conversation {
            rewinding: true,
            ..Default::default()
        };
        apply_rewound_conversation(
            &mut rewound,
            RewindMode::EditRetry,
            "Edited prompt",
            Vec::new(),
            0,
        );
        assert!(!rewound.rewinding);
        assert_eq!(rewound.draft, "Edited prompt");
    }

    #[test]
    fn studio_multimodal_attachments_and_compress_controls_follow_session_state() {
        let png = StagedImage::from_bytes(
            1,
            "shot.png",
            vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0],
        )
        .expect("png");
        let mut state = AppState {
            connection: Some(ConnectionState::Connected),
            launch_attachments: vec![png.clone()],
            ..Default::default()
        };
        assert!(can_stage_images(&state, true));
        // Even with an empty text draft, a staged image on the Launch Screen arms Send.
        assert!(composer_send_sensitive(&state, false));
        state.launch_draft = "/workspace native".into();
        assert!(composer_hint_text(&state).contains("project directory"));

        state.selected = Some("chat-1".into());
        let mut chat = Conversation {
            runtime_id: Some("rt-1".into()),
            attachments: vec![png],
            messages: vec![
                ChatMessage {
                    id: "u1".into(),
                    role: Role::User,
                    text: "One".into(),
                },
                ChatMessage {
                    id: "a1".into(),
                    role: Role::Assistant,
                    text: "Two".into(),
                },
            ],
            ..Default::default()
        };
        // Image-only send in an idle conversation is enabled, but disabled while busy.
        assert!(can_send_in_conversation(&chat));
        chat.status = TurnStatus::Streaming;
        assert!(!can_send_in_conversation(&chat));
        chat.status = TurnStatus::Idle;
        chat.draft = "/compress".into();
        assert!(can_send_in_conversation(&chat));
        assert!(composer_conversation_hint(&chat).contains("compact older turns"));
        state.conversations.insert("chat-1".into(), chat);
        state.sessions.push(SessionSummary {
            stored_id: "chat-1".into(),
            message_count: 6,
            ..Default::default()
        });

        let (_, cmd) = prepare_compress_command(&state).expect("compress cmd");
        assert!(matches!(cmd, Command::Compress { .. }));

        apply_compressed_state(
            &mut state,
            "chat-1",
            "rt-2".into(),
            Some(vec![ChatMessage {
                id: "s1".into(),
                role: Role::Assistant,
                text: "Summary".into(),
            }]),
            0,
            &CompressSummary {
                removed: 5,
                before_tokens: 9000,
                after_tokens: 1200,
                noop: false,
                headline: "Compacted 5 messages".into(),
            },
        );
        assert_eq!(state.sessions[0].message_count, 1);
        assert_eq!(
            state.conversations["chat-1"].runtime_id.as_deref(),
            Some("rt-2")
        );
        assert_eq!(
            state.conversations["chat-1"].activity,
            "Compacted 5 messages"
        );
        assert_eq!(submit_activity_label(true), "Uploading image & sending…");
    }
}
