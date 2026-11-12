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

struct Ui {
    backend: Option<Backend>,
    state: RefCell<AppState>,
    pending_resumes: RefCell<HashSet<String>>,
    window: adw::ApplicationWindow,
    sidebar: gtk::Box,
    triage: Rc<TriageBar>,
    palette: RefCell<Option<Rc<CommandPalette>>>,
    palette_trigger: gtk::Button,
    inspector: InspectorPanel,
    inspector_toggle: gtk::Button,
    inspector_badge: gtk::Label,
    delete_chat: gtk::Button,
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
    readonly: gtk::Label,
    interaction: gtk::Box,
    interaction_scroll: gtk::ScrolledWindow,
    rendered_session: RefCell<Option<String>>,
    rendered_revision: Cell<u64>,
    rendered_interaction: RefCell<Option<(String, String)>>,
    changing_draft: Cell<bool>,
    follow_bottom: Cell<bool>,
    select_created: Cell<bool>,
    suppress_selection: Cell<bool>,
    sequence: Cell<u64>,
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
            .is_some_and(|c| c.status.busy() || c.history_loading)
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
    if c.runtime_id.is_none() || c.read_only_reason.is_some() {
        return false;
    }
    match parse_composer_input(&c.draft) {
        ComposerIntent::Empty => false,
        ComposerIntent::SideQuestion(_) => true,
        ComposerIntent::Prompt(_) => c.status != TurnStatus::Stopping,
    }
}

fn composer_send_sensitive(state: &AppState, reload_pending: bool) -> bool {
    if state.connection.as_ref() != Some(&ConnectionState::Connected) || reload_pending {
        return false;
    }
    match &state.selected {
        None => {
            !state.creating
                && state.pending_launch.is_none()
                && !state.launch_draft.trim().is_empty()
        }
        Some(id) => state
            .conversations
            .get(id)
            .is_some_and(can_send_in_conversation),
    }
}

fn composer_hint_text(state: &AppState) -> &'static str {
    let Some(id) = &state.selected else {
        return "Enter to start a new chat · Shift+Enter new line · Ctrl+K commands";
    };
    let Some(c) = state.conversations.get(id) else {
        return "Enter to send · Alt+Enter side-ask (/btw) · Ctrl+I Flight Deck";
    };
    composer_conversation_hint(c)
}

fn composer_conversation_hint(c: &Conversation) -> &'static str {
    if c.read_only_reason.is_some() {
        return "Read-only conversation · Ctrl+N for a new chat · Ctrl+I Flight Deck";
    }
    let trimmed = c.draft.trim_start();
    if trimmed.starts_with("/btw") || trimmed.starts_with("/ask") {
        return "Enter to ask snapshot side question (/btw) · never saved to history";
    }
    if c.status.steerable() {
        "Enter to steer active turn · Alt+Enter or /btw side-ask · Ctrl+I Flight Deck"
    } else {
        "Enter to send · Alt+Enter or /btw side-ask · Ctrl+I Flight Deck"
    }
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
    header.pack_start(&toggle_sidebar);
    header.pack_start(&palette_trigger);
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
    header.pack_end(&inspector_toggle);
    header.pack_end(&reconnect);
    header.pack_end(&delete_chat);
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
    session_factory.connect_bind(|_, object| palette::bind_session_row(object));
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
    let readonly = gtk::Label::new(None);
    readonly.set_wrap(true);
    readonly.set_selectable(true);
    readonly.set_xalign(0.0);
    readonly.add_css_class("readonly-notice");
    set_margins(&readonly, 12);
    readonly.set_visible(false);
    main.append(&readonly);
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

    let messages = gio::ListStore::new::<glib::BoxedAnyObject>();
    let message_factory = gtk::SignalListItemFactory::new();
    message_factory.connect_bind(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
        let value = item
            .item()
            .and_downcast::<glib::BoxedAnyObject>()
            .expect("message item");
        let message = value.borrow::<ChatMessage>();
        let (speaker_label, row_class, speaker_class) = role_style(&message.role);
        let row = gtk::Box::new(gtk::Orientation::Vertical, 10);
        row.add_css_class("message");
        row.add_css_class(row_class);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let speaker = gtk::Label::new(Some(speaker_label));
        speaker.add_css_class("speaker");
        speaker.add_css_class(speaker_class);
        speaker.set_hexpand(true);
        speaker.set_xalign(0.0);
        heading.append(&speaker);
        let copy = icon_button("edit-copy-symbolic", "Copy original message");
        copy.add_css_class("flat");
        copy.add_css_class("message-copy");
        let source = value.clone();
        copy.connect_clicked(move |button| {
            button
                .clipboard()
                .set_text(&source.borrow::<ChatMessage>().text);
        });
        heading.append(&copy);
        row.append(&heading);
        row.append(&message_body(&message));
        let column = adw::Clamp::builder()
            .maximum_size(860)
            .tightening_threshold(600)
            .child(&row)
            .build();
        item.set_child(Some(&column));
    });
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
    let composer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    composer.add_css_class("hermes-composer");
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
    let model_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    model_row.append(&model_menu);
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
        inspector,
        inspector_toggle,
        inspector_badge,
        delete_chat,
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
        readonly,
        interaction,
        interaction_scroll,
        rendered_session: RefCell::new(None),
        rendered_revision: Cell::new(u64::MAX),
        rendered_interaction: RefCell::new(None),
        changing_draft: Cell::new(false),
        follow_bottom: Cell::new(true),
        select_created: Cell::new(false),
        suppress_selection: Cell::new(false),
        sequence: Cell::new(0),
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
        let Some(id) = ui.session_id_at(position) else {
            return;
        };
        ui.select_created.set(false);
        ui.select(&id, true);
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
        handle_composer_key(weak.upgrade().as_deref(), key, modifiers)
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
    ui: Option<&Ui>,
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
        let _ = weak
            .upgrade()
            .map(|ui| palette.populate(&ui.state.borrow()));
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
                ui.state.borrow_mut().session_query = query;
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

fn handle_window_shortcut(
    ui: &Rc<Ui>,
    key: gdk::Key,
    modifiers: gdk::ModifierType,
) -> glib::Propagation {
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

fn dispatch_workspace_shortcut(ui: &Rc<Ui>, ch: char) -> bool {
    match ch {
        'n' => {
            ui.close_palette();
            ui.create();
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
        'm' => {
            ui.close_palette();
            ui.open_model_menu();
        }
        'r' => {
            ui.close_palette();
            ui.reload_or_reconnect();
        }
        _ => return false,
    }
    true
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
        self.select_created.set(true);
        self.state.borrow_mut().creating = true;
        if !self.command(Command::Create {
            title: "New conversation".into(),
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

    fn select(&self, id: &str, focus_editor: bool) {
        {
            let mut state = self.state.borrow_mut();
            state.pending_launch = None;
            state.selected = Some(id.to_string());
            state.notice.clear();
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

    fn submit(&self) {
        if self.state.borrow().selected.is_none() {
            self.submit_from_launch();
            return;
        }
        let Some((id, runtime_id, draft)) = self.extract_submit_target() else {
            return;
        };
        match parse_composer_input(&draft) {
            ComposerIntent::Empty => {}
            ComposerIntent::SideQuestion(question) => {
                self.dispatch_composer_side_question(&id, question);
            }
            ComposerIntent::Prompt(text) => self.dispatch_prompt_or_steer(id, runtime_id, text),
        }
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
        if !self.command(Command::Submit {
            runtime_id,
            text: text.clone(),
        }) {
            self.refresh();
            return;
        }
        self.sequence.set(self.sequence.get() + 1);
        if let Some(c) = self.state.borrow_mut().conversations.get_mut(&id) {
            c.clear_turn_reasoning();
            c.record_message(ChatMessage {
                id: format!("client-{}", self.sequence.get()),
                role: Role::User,
                text,
            });
            c.draft.clear();
            c.status = TurnStatus::Submitting;
            c.activity = "Sending…".into();
        }
        self.set_editor_text("");
        self.follow_bottom.set(true);
        self.refresh();
    }

    fn submit_from_launch(&self) {
        if !self.send.is_sensitive() {
            return;
        }
        let Some((title, _)) = self.state.borrow_mut().begin_launch_submit() else {
            return;
        };
        self.select_created.set(true);
        if !self.command(Command::Create { title }) {
            self.state.borrow_mut().cancel_pending_launch();
        }
        self.refresh();
    }

    fn send_initial_prompt(&self, id: &str, runtime_id: String, text: String) {
        if !self.command(Command::Submit {
            runtime_id,
            text: text.clone(),
        }) {
            if let Some(c) = self.state.borrow_mut().conversations.get_mut(id) {
                c.draft = text.clone();
            }
            if self.state.borrow().selected.as_deref() == Some(id) {
                self.set_editor_text(&text);
            }
            self.refresh();
            return;
        }
        self.sequence.set(self.sequence.get() + 1);
        if let Some(c) = self.state.borrow_mut().conversations.get_mut(id) {
            c.clear_turn_reasoning();
            c.record_message(ChatMessage {
                id: format!("client-{}", self.sequence.get()),
                role: Role::User,
                text,
            });
            c.draft.clear();
            c.status = TurnStatus::Submitting;
            c.activity = "Sending…".into();
        }
        self.follow_bottom.set(true);
        self.refresh();
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
                ComposerIntent::Empty => String::new(),
                ComposerIntent::SideQuestion(q) | ComposerIntent::Prompt(q) => q,
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
            other => self.run_palette_session_action(other),
        }
    }

    fn run_palette_session_action(self: &Rc<Self>, action: PaletteAction) {
        match action {
            PaletteAction::ToggleInspector => self.toggle_inspector(),
            PaletteAction::OpenInspector(tab) => self.open_inspector(tab),
            PaletteAction::ReloadHistory => self.reload_current(),
            PaletteAction::LoadMoreSessions => self.load_more_sessions(),
            PaletteAction::Reconnect => self.trigger_reconnect(),
            PaletteAction::DeleteCurrent => self.confirm_delete_current(),
            _ => {}
        }
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
        let launch_prompt = {
            let mut state = self.state.borrow_mut();
            let seeded_draft = state.take_launch_draft_for_blank_chat();
            let prompt = state.take_pending_launch();
            state.sessions.insert(0, session);
            let mut conversation = Conversation {
                runtime_id: Some(runtime_id.clone()),
                loaded: true,
                draft: seeded_draft,
                ..Default::default()
            };
            if let Some(model) = model {
                conversation.confirm_model(model);
            }
            state.conversations.insert(id.clone(), conversation);
            state.creating = false;
            prompt
        };
        self.refresh_sessions();
        if self.select_created.replace(false) {
            self.select(&id, true);
        }
        if let Some(prompt) = launch_prompt {
            self.send_initial_prompt(&id, runtime_id, prompt);
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
        self.apply_model(event) || self.apply_inspector(event)
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
                self.pending_resumes.borrow_mut().remove(&stored_id);
                let c = state.conversations.entry(stored_id).or_default();
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
                *self.model_pending.borrow_mut() = None;
                c.revision += 1;
                c.inspector_revision += 1;
            }
            Event::History {
                stored_id,
                messages,
                before,
                next_before,
            } => {
                if let Some(c) = state.conversations.get_mut(&stored_id) {
                    c.receive_history_page(before, next_before, messages);
                }
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

    /// Session-list, title, deletion, and failure events.
    fn apply_history(&self, state: &mut AppState, event: &Event) -> MetaOutcome {
        match event {
            Event::SessionTitle { session_id, title } => {
                if state.apply_session_title(session_id, title) {
                    MetaOutcome::RefreshSessions
                } else {
                    MetaOutcome::Handled
                }
            }
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
            let indices = state.visible_sessions();
            let rows: Vec<_> = indices
                .iter()
                .filter_map(|&index| state.sessions.get(index))
                .cloned()
                .map(glib::BoxedAnyObject::new)
                .collect();
            let selected = visible_selected_position(&state, &indices);
            (rows, selected)
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
            let indices = state.visible_sessions();
            visible_selected_position(&state, &indices)
        };
        self.suppress_selection.set(true);
        self.selection.set_selected(selected);
        self.suppress_selection.set(false);
    }

    fn session_id_at(&self, position: u32) -> Option<String> {
        self.sessions
            .item(position)
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|value| value.borrow::<SessionSummary>().stored_id.clone())
    }

    fn adopt_sidebar_selection(&self, focus_editor: bool) {
        if self.suppress_selection.get() {
            return;
        }
        let Some(id) = self.session_id_at(self.selection.selected()) else {
            // A cleared highlight must not leave the open conversation behind.
            self.sync_sidebar_selection();
            return;
        };
        if self.state.borrow().selected.as_deref() == Some(id.as_str()) {
            if focus_editor {
                self.editor.grab_focus();
            }
            return;
        }
        self.select_created.set(false);
        self.select(&id, focus_editor);
    }

    fn refresh(&self) {
        let state = self.state.borrow();
        let selected = state.selected.as_ref();
        let conversation = selected.and_then(|id| state.conversations.get(id));
        let identity_changed = *self.rendered_session.borrow() != state.selected;
        let revision = conversation.map_or(0, |c| c.revision);
        if identity_changed || self.rendered_revision.get() != revision {
            if identity_changed {
                self.messages.remove_all();
                *self.rendered_session.borrow_mut() = state.selected.clone();
            }
            let current = conversation.map_or(&[][..], |c| c.messages.as_slice());
            if self.messages.n_items() as usize > current.len() {
                self.messages.splice(
                    current.len() as u32,
                    self.messages.n_items() - current.len() as u32,
                    &[] as &[glib::BoxedAnyObject],
                );
            }
            for (index, message) in current.iter().enumerate() {
                let same = self
                    .messages
                    .item(index as u32)
                    .and_downcast::<glib::BoxedAnyObject>()
                    .is_some_and(|v| {
                        let old = v.borrow::<ChatMessage>();
                        old.id == message.id && old.text == message.text && old.role == message.role
                    });
                if !same {
                    let value = glib::BoxedAnyObject::new(message.clone());
                    if (index as u32) < self.messages.n_items() {
                        self.messages.splice(index as u32, 1, &[value]);
                    } else {
                        self.messages.append(&value);
                    }
                }
            }
            self.rendered_revision.set(revision);
            if self.follow_bottom.get() {
                let adjustment = self.transcript.vadjustment();
                glib::idle_add_local_once(move || {
                    adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
                });
            }
        }
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
        let reason = conversation.and_then(|c| c.read_only_reason.as_deref());
        self.readonly.set_text(reason.unwrap_or_default());
        self.readonly.set_visible(reason.is_some());
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
        self.refresh_model_trigger(conversation, &state.catalog, writable);
        self.refresh_connection_badge(&state);
        self.sync_inspector_chrome(&state, conversation, writable);
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
}
