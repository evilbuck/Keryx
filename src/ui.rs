use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::backend::{self, Backend};
use crate::markdown;
use crate::model::*;

struct Ui {
    backend: Option<Backend>,
    state: RefCell<AppState>,
    pending_resumes: RefCell<HashSet<String>>,
    window: adw::ApplicationWindow,
    sessions: gio::ListStore,
    selection: gtk::SingleSelection,
    messages: gio::ListStore,
    transcript: gtk::ScrolledWindow,
    content: gtk::Stack,
    title: gtk::Label,
    connection: gtk::Label,
    notice: gtk::Label,
    activity: gtk::Label,
    spinner: gtk::Spinner,
    editor: gtk::TextView,
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
    let toggle_sidebar = icon_button("sidebar-show-symbolic", "Toggle conversations");
    header.pack_start(&toggle_sidebar);
    let connection = gtk::Label::new(Some("Connecting…"));
    connection.add_css_class("connection-label");
    let reconnect = icon_button(
        "view-refresh-symbolic",
        "Reconnect and reload server history",
    );
    header.pack_end(&reconnect);
    header.pack_end(&connection);

    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 16);
    sidebar.add_css_class("hermes-sidebar");
    sidebar.set_width_request(220);
    set_margins(&sidebar, 16);
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
    let sessions = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(sessions.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let session_factory = gtk::SignalListItemFactory::new();
    session_factory.connect_bind(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
        let value = item
            .item()
            .and_downcast::<glib::BoxedAnyObject>()
            .expect("session item");
        let session = value.borrow::<SessionSummary>();
        let row = gtk::Box::new(gtk::Orientation::Vertical, 5);
        row.add_css_class("session-row");
        let title = gtk::Label::new(Some(if session.title.is_empty() {
            "Untitled conversation"
        } else {
            &session.title
        }));
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.add_css_class("session-title");
        row.append(&title);
        let meta = gtk::Label::new(Some(&format!(
            "{}{}",
            session.source,
            if session.read_only_reason.is_some() {
                " · read-only"
            } else {
                ""
            }
        )));
        meta.set_xalign(0.0);
        meta.add_css_class("session-meta");
        row.append(&meta);
        let identity = match &session.read_only_reason {
            Some(reason) => format!("Conversation ID: {}\n{}", session.stored_id, reason),
            None => format!("Conversation ID: {}", session.stored_id),
        };
        row.set_tooltip_text(Some(&identity));
        item.set_child(Some(&row));
    });
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
        let row = gtk::Box::new(gtk::Orientation::Vertical, 10);
        row.add_css_class("message");
        if message.role == Role::User {
            row.add_css_class("message-user");
        }
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let speaker = gtk::Label::new(Some(match message.role {
            Role::User => "YOU",
            Role::Assistant => "HERMES",
            Role::Tool => "TOOL",
            Role::System => "SYSTEM",
        }));
        speaker.add_css_class("speaker");
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
        let rendered = markdown::render(&message.text);
        rendered.add_css_class("message-prose");
        row.append(&rendered);
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
        "Choose a conversation, or start something new.\nYour history stays on your Hermes server.",
    ));
    empty_subtitle.set_wrap(true);
    empty_subtitle.set_justify(gtk::Justification::Center);
    empty_subtitle.add_css_class("empty-subtitle");
    empty.append(&empty_subtitle);
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
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let hint = gtk::Label::new(Some("Enter to send · Shift+Enter for a new line"));
    hint.set_xalign(0.0);
    hint.set_hexpand(true);
    hint.add_css_class("composer-hint");
    actions.append(&hint);
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

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_start_child(Some(&sidebar));
    paned.set_end_child(Some(&main));
    paned.set_position(285);
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(false);
    paned.set_wide_handle(true);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&paned));
    window.set_content(Some(&toolbar));
    toggle_sidebar.connect_clicked(move |_| {
        sidebar.set_visible(!sidebar.is_visible());
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
        sessions,
        selection,
        messages,
        transcript,
        content,
        title,
        connection,
        notice,
        activity,
        spinner,
        editor,
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
    ui.refresh();
    ui.window.present();
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
        if let Some(ui) = weak.upgrade() {
            ui.state.borrow_mut().notice.clear();
            ui.state.borrow_mut().disconnect();
            ui.pending_resumes.borrow_mut().clear();
            ui.command(Command::Reconnect);
            ui.refresh();
        }
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
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let offset = ui.state.borrow().sessions.len();
        ui.state.borrow_mut().list_loading = true;
        ui.command(Command::List { offset });
        ui.refresh();
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
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let id = ui.state.borrow().selected.clone();
        if let Some(id) = id {
            ui.resume(&id);
        }
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
        let mut state = ui.state.borrow_mut();
        if let Some(id) = state.selected.clone() {
            state.conversations.entry(id).or_default().draft = text;
        }
        drop(state);
        ui.refresh_controls();
    });
    let key = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    key.connect_key_pressed(move |_, key, _, modifiers| {
        if (key == gdk::Key::Return || key == gdk::Key::KP_Enter)
            && !modifiers.contains(gdk::ModifierType::SHIFT_MASK)
        {
            if let Some(ui) = weak.upgrade() {
                ui.submit();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    ui.editor.add_controller(key);
    let shortcuts = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(ui);
    shortcuts.connect_key_pressed(move |_, key, _, modifiers| {
        if modifiers.contains(gdk::ModifierType::CONTROL_MASK)
            && (key == gdk::Key::n || key == gdk::Key::N)
        {
            if let Some(ui) = weak.upgrade() {
                ui.create();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    ui.window.add_controller(shortcuts);
    let adjustment = ui.transcript.vadjustment();
    let weak = Rc::downgrade(ui);
    adjustment.connect_value_changed(move |adjustment| {
        if let Some(ui) = weak.upgrade() {
            ui.follow_bottom
                .set(adjustment.value() + adjustment.page_size() >= adjustment.upper() - 64.0);
        }
    });
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

    fn select(&self, id: &str, focus_editor: bool) {
        self.state.borrow_mut().selected = Some(id.to_string());
        self.state.borrow_mut().notice.clear();
        self.follow_bottom.set(true);
        let draft = self
            .state
            .borrow_mut()
            .conversations
            .entry(id.to_string())
            .or_default()
            .draft
            .clone();
        self.changing_draft.set(true);
        self.editor.buffer().set_text(&draft);
        self.changing_draft.set(false);
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
        let reload_pending = self
            .state
            .borrow()
            .selected
            .as_ref()
            .is_some_and(|id| self.pending_resumes.borrow().contains(id));
        if reload_pending || !self.send.is_sensitive() {
            return;
        }
        let request = {
            let state = self.state.borrow();
            state.selected.as_ref().and_then(|id| {
                state.conversations.get(id).and_then(|c| {
                    c.runtime_id
                        .as_ref()
                        .map(|runtime| (id.clone(), runtime.clone(), c.draft.trim().to_string()))
                })
            })
        };
        let Some((id, runtime_id, text)) = request else {
            return;
        };
        if text.is_empty() {
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
        let mut state = self.state.borrow_mut();
        if let Some(c) = state.conversations.get_mut(&id) {
            c.record_message(ChatMessage {
                id: format!("client-{}", self.sequence.get()),
                role: Role::User,
                text,
            });
            c.draft.clear();
            c.status = TurnStatus::Submitting;
            c.activity = "Sending…".into();
        }
        drop(state);
        self.changing_draft.set(true);
        self.editor.buffer().set_text("");
        self.changing_draft.set(false);
        self.follow_bottom.set(true);
        self.refresh();
    }

    fn apply(&self, event: Event) {
        let mut state = self.state.borrow_mut();
        match event {
            Event::Connection(connection) => {
                let connected = connection == ConnectionState::Connected;
                if !connected {
                    state.disconnect();
                    self.pending_resumes.borrow_mut().clear();
                }
                state.connection = Some(connection);
                state.creating = false;
                state.list_loading = connected;
                let selected = state.selected.clone();
                drop(state);
                if connected {
                    self.command(Command::List { offset: 0 });
                    if let Some(id) = selected {
                        self.resume(&id);
                    }
                }
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
            } => {
                let id = session.stored_id.clone();
                state.sessions.insert(0, session);
                state.conversations.insert(
                    id.clone(),
                    Conversation {
                        runtime_id: Some(runtime_id),
                        loaded: true,
                        ..Default::default()
                    },
                );
                state.creating = false;
                drop(state);
                self.refresh_sessions();
                if self.select_created.replace(false) {
                    self.select(&id, true);
                }
            }
            Event::Resumed {
                stored_id,
                runtime_id,
                messages,
                before,
                read_only_reason,
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
                c.revision += 1;
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
            Event::Submitted { runtime_id } => {
                if let Some(c) = state.conversation_for_runtime_mut(&runtime_id) {
                    c.submit_acknowledged();
                }
            }
            Event::MessageDelta {
                runtime_id,
                message_id,
                delta,
            } => {
                if let Some(c) = state.conversation_for_runtime_mut(&runtime_id) {
                    c.receive_delta(message_id, delta);
                }
            }
            Event::MessageComplete {
                runtime_id,
                message,
                status,
            } => {
                if let Some(c) = state.conversation_for_runtime_mut(&runtime_id) {
                    c.finish_message(message);
                    c.activity = match status {
                        TurnStatus::Complete => String::new(),
                        TurnStatus::Interrupted => {
                            "Response stopped. Partial text may be incomplete.".into()
                        }
                        TurnStatus::Failed => "The server reported a failed response.".into(),
                        TurnStatus::Idle
                        | TurnStatus::Submitting
                        | TurnStatus::Streaming
                        | TurnStatus::AwaitingInput
                        | TurnStatus::Stopping
                        | TurnStatus::Unknown => {
                            "Response ended without a confirmed completion.".into()
                        }
                    };
                    c.status = status;
                    c.interaction = None;
                }
            }
            Event::Activity { runtime_id, text } => {
                if let Some(c) = state.conversation_for_runtime_mut(&runtime_id) {
                    c.activity = text;
                }
            }
            Event::Interaction {
                runtime_id,
                request,
            } => {
                if let Some(c) = state.conversation_for_runtime_mut(&runtime_id) {
                    c.receive_interaction(request);
                }
            }
            Event::InteractionCancelled {
                runtime_id,
                request_id,
            } => {
                if let Some(c) = state.conversation_for_runtime_mut(&runtime_id) {
                    c.withdraw_interaction(&request_id);
                }
            }
            Event::Interrupting { runtime_id } => {
                if let Some(c) = state.conversation_for_runtime_mut(&runtime_id) {
                    c.begin_stopping();
                }
            }
            Event::Deleted { stored_id } => {
                state.sessions.retain(|s| s.stored_id != stored_id);
                state.conversations.remove(&stored_id);
            }
            Event::Error {
                operation,
                session_id,
                message,
            } => {
                state.notice = format!("{operation}: {message}");
                state.creating = false;
                state.list_loading = false;
                if let Some(id) = session_id {
                    self.pending_resumes.borrow_mut().remove(&id);
                    let stored = if state.conversations.contains_key(&id) {
                        Some(id)
                    } else {
                        state.stored_for_runtime(&id)
                    };
                    if let Some(id) = stored {
                        let c = state.conversations.get_mut(&id).unwrap();
                        c.history_loading = false;
                        c.operation_failed(&operation, message);
                    }
                }
            }
        }
    }

    fn refresh_sessions(&self) {
        let (rows, selected) = {
            let state = self.state.borrow();
            let rows: Vec<_> = state
                .sessions
                .iter()
                .cloned()
                .map(glib::BoxedAnyObject::new)
                .collect();
            let selected = state
                .selected
                .as_ref()
                .and_then(|id| state.sessions.iter().position(|s| &s.stored_id == id))
                .map(|index| index as u32)
                .unwrap_or(gtk::INVALID_LIST_POSITION);
            (rows, selected)
        };
        // Drop the state borrow before these mutations. Hold the guard across
        // both: splice emits selection changes before the replacement position
        // is applied, and a nested handler must not clear this flag.
        self.suppress_selection.set(true);
        self.sessions.splice(0, self.sessions.n_items(), &rows);
        self.selection.set_selected(selected);
        self.suppress_selection.set(false);
    }

    fn sync_sidebar_selection(&self) {
        let selected = {
            let state = self.state.borrow();
            state
                .selected
                .as_ref()
                .and_then(|id| state.sessions.iter().position(|s| &s.stored_id == id))
                .map(|index| index as u32)
                .unwrap_or(gtk::INVALID_LIST_POSITION)
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
        self.title.set_text(
            selected
                .and_then(|id| state.sessions.iter().find(|s| &s.stored_id == id))
                .map_or("Hermes Native", |s| s.title.as_str()),
        );
        self.notice.set_text(&state.notice);
        self.notice.set_visible(!state.notice.is_empty());
        let reason = conversation.and_then(|c| c.read_only_reason.as_deref());
        self.readonly.set_text(reason.unwrap_or_default());
        self.readonly.set_visible(reason.is_some());
        self.activity.set_text(
            if selected.is_some_and(|id| self.pending_resumes.borrow().contains(id)) {
                "Loading server history…"
            } else {
                conversation.map_or("", |c| c.activity.as_str())
            },
        );
        self.spinner.set_spinning(
            conversation.is_some_and(|c| c.status.busy() || c.history_loading)
                || selected.is_some_and(|id| self.pending_resumes.borrow().contains(id)),
        );
        self.older
            .set_visible(conversation.is_some_and(|c| c.history_before > 0));
        self.older
            .set_sensitive(conversation.is_some_and(|c| !c.status.busy() && !c.history_loading));
        self.latest.set_visible(selected.is_some());
        drop(state);
        self.refresh_interaction();
        self.refresh_controls();
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
        self.editor
            .set_editable(conversation.is_some_and(|c| c.read_only_reason.is_none()));
        self.send.set_sensitive(
            writable
                && !reload_pending
                && conversation.is_some_and(|c| !c.status.busy() && !c.draft.trim().is_empty()),
        );
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
