use std::cell::Cell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::model::{
    AppState, PaletteAction, PaletteEntry, SessionFilter, SessionSummary, SourceKind,
    build_palette_entries, classify_source, session_meta_line, source_badge,
};

pub struct TriageBar {
    pub container: gtk::Box,
    pub all_btn: gtk::ToggleButton,
    pub chats_btn: gtk::ToggleButton,
    pub auto_btn: gtk::ToggleButton,
    pub search: gtk::SearchEntry,
    pub empty_label: gtk::Label,
    updating: Cell<bool>,
}

impl TriageBar {
    pub fn new() -> Rc<Self> {
        let container = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let pills = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        pills.add_css_class("triage-bar");
        let all_btn = triage_button("All", "Show all conversations");
        let chats_btn = triage_button("Chats", "Interactive human chats only (hide cron)");
        let auto_btn = triage_button("Auto", "Scheduled cron and background sessions");
        all_btn.set_active(true);
        pills.append(&all_btn);
        pills.append(&chats_btn);
        pills.append(&auto_btn);
        container.append(&pills);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Filter conversations…"));
        search.add_css_class("sidebar-search");
        search.update_property(&[gtk::accessible::Property::Label("Filter conversations")]);
        container.append(&search);

        let empty_label = gtk::Label::new(Some("No matching conversations"));
        empty_label.add_css_class("triage-empty");
        empty_label.set_wrap(true);
        empty_label.set_visible(false);

        Rc::new(Self {
            container,
            all_btn,
            chats_btn,
            auto_btn,
            search,
            empty_label,
            updating: Cell::new(false),
        })
    }

    pub fn sync(&self, state: &AppState, visible_count: usize) {
        self.updating.set(true);
        let (all, chats, auto) = state.filter_counts();
        self.all_btn.set_label(&format!("All {all}"));
        self.chats_btn.set_label(&format!("Chats {chats}"));
        self.auto_btn.set_label(&format!("Auto {auto}"));
        self.all_btn
            .set_active(state.session_filter == SessionFilter::All);
        self.chats_btn
            .set_active(state.session_filter == SessionFilter::Chats);
        self.auto_btn
            .set_active(state.session_filter == SessionFilter::Auto);
        if self.search.text().as_str() != state.session_query {
            self.search.set_text(&state.session_query);
        }
        self.empty_label
            .set_visible(!state.sessions.is_empty() && visible_count == 0);
        self.updating.set(false);
    }

    pub fn connect(
        self: &Rc<Self>,
        on_filter: impl Fn(SessionFilter) + 'static,
        on_query: impl Fn(String) + 'static,
        on_escape: impl Fn() + 'static,
    ) {
        let filter_cb: Rc<dyn Fn(SessionFilter)> = Rc::new(on_filter);
        wire_triage_pill(self, &self.all_btn, SessionFilter::All, &filter_cb);
        wire_triage_pill(self, &self.chats_btn, SessionFilter::Chats, &filter_cb);
        wire_triage_pill(self, &self.auto_btn, SessionFilter::Auto, &filter_cb);
        let bar = Rc::downgrade(self);
        self.search.connect_search_changed(move |entry| {
            if let Some(bar) = bar.upgrade()
                && !bar.updating.get()
            {
                on_query(entry.text().to_string());
            }
        });
        let bar = Rc::downgrade(self);
        self.search.connect_stop_search(move |entry| {
            if !entry.text().is_empty() {
                entry.set_text("");
            } else if bar.upgrade().is_some() {
                on_escape();
            }
        });
    }
}

fn triage_button(label: &str, tooltip: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::with_label(label);
    button.set_hexpand(true);
    button.add_css_class("flat");
    button.add_css_class("triage-pill");
    button.set_tooltip_text(Some(tooltip));
    button
}

fn wire_triage_pill(
    bar: &Rc<TriageBar>,
    button: &gtk::ToggleButton,
    target: SessionFilter,
    on_filter: &Rc<dyn Fn(SessionFilter)>,
) {
    let weak = Rc::downgrade(bar);
    let cb = Rc::clone(on_filter);
    button.connect_toggled(move |btn| {
        let Some(bar) = weak.upgrade() else {
            return;
        };
        if bar.updating.get() {
            return;
        }
        if btn.is_active() {
            cb(target);
        } else {
            // Keep at least one pill active in the segmented group.
            bar.updating.set(true);
            btn.set_active(true);
            bar.updating.set(false);
        }
    });
}

pub struct CommandPalette {
    dialog: adw::Dialog,
    search: gtk::SearchEntry,
    results: gio::ListStore,
    selection: gtk::SingleSelection,
    list: gtk::ListView,
    empty: gtk::Label,
    open: Cell<bool>,
}

impl CommandPalette {
    pub fn new(on_action: Rc<dyn Fn(PaletteAction)>) -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.add_css_class("palette-root");
        set_box_margins(&root, 14);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Jump to a conversation or run a command…"));
        search.add_css_class("palette-search");
        search.update_property(&[gtk::accessible::Property::Label("Command palette search")]);
        root.append(&search);

        let empty = gtk::Label::new(Some("No matching command or conversation."));
        empty.add_css_class("triage-empty");
        empty.set_visible(false);
        root.append(&empty);

        let results = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(results.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let factory = gtk::SignalListItemFactory::new();
        let row_action = Rc::clone(&on_action);
        factory.connect_bind(move |_, object| {
            bind_palette_row(object, &row_action);
        });

        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.set_single_click_activate(false);
        list.add_css_class("palette-list");

        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();
        root.append(&scroll);
        root.append(&palette_footer());

        let dialog = adw::Dialog::builder()
            .title("Command Palette")
            .content_width(560)
            .content_height(430)
            .follows_content_size(false)
            .child(&root)
            .build();

        let palette = Rc::new(Self {
            dialog,
            search,
            results,
            selection,
            list,
            empty,
            open: Cell::new(false),
        });
        palette.wire_internal(on_action);
        palette
    }

    pub fn toggle(&self, window: &impl IsA<gtk::Widget>, state: &AppState) {
        if self.open.get() {
            self.close();
            return;
        }
        self.open.set(true);
        self.search.set_text("");
        self.populate(state);
        self.dialog.present(Some(window));
        self.search.grab_focus();
    }

    pub fn close(&self) {
        if self.open.replace(false) {
            let _ = self.dialog.close();
        }
    }

    pub fn refresh_if_open(&self, state: &AppState) {
        if self.open.get() {
            self.populate(state);
        }
    }

    pub fn populate(&self, state: &AppState) {
        let query = self.search.text().to_string();
        let entries = build_palette_entries(state, &query, now_epoch_secs());
        let boxed: Vec<glib::BoxedAnyObject> =
            entries.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.empty.set_visible(boxed.is_empty());
        self.results.splice(0, self.results.n_items(), &boxed);
        if self.selection.n_items() > 0 {
            self.selection.set_selected(0);
        }
    }

    pub fn connect_query(self: &Rc<Self>, on_query: impl Fn(&CommandPalette) + 'static) {
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(palette) = weak.upgrade()
                && palette.open.get()
            {
                on_query(&palette);
            }
        });
    }

    fn wire_internal(self: &Rc<Self>, on_action: Rc<dyn Fn(PaletteAction)>) {
        let weak = Rc::downgrade(self);
        self.dialog.connect_closed(move |_| {
            if let Some(palette) = weak.upgrade() {
                palette.open.set(false);
            }
        });
        let weak = Rc::downgrade(self);
        let commit_enter = Rc::clone(&on_action);
        self.search.connect_activate(move |_| {
            if let Some(palette) = weak.upgrade() {
                palette.activate_selected(&commit_enter);
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_stop_search(move |_| {
            if let Some(palette) = weak.upgrade() {
                palette.close();
            }
        });
        let weak = Rc::downgrade(self);
        let commit_list = Rc::clone(&on_action);
        self.list.connect_activate(move |_, _| {
            if let Some(palette) = weak.upgrade() {
                palette.activate_selected(&commit_list);
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            palette_key(weak.upgrade().as_deref(), key, modifiers)
        });
        self.dialog.add_controller(keys);
    }

    fn activate_selected(&self, on_action: &Rc<dyn Fn(PaletteAction)>) {
        let Some(item) = self.selection.selected_item() else {
            return;
        };
        let Ok(boxed) = item.downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let action = boxed.borrow::<PaletteEntry>().action.clone();
        self.close();
        on_action(action);
    }

    fn step_selection(&self, delta: i64) {
        let count = i64::from(self.selection.n_items());
        if count == 0 {
            return;
        }
        let raw = self.selection.selected();
        let current = if raw == gtk::INVALID_LIST_POSITION {
            0
        } else {
            i64::from(raw)
        };
        let next = (current + delta).clamp(0, count - 1) as u32;
        self.selection.set_selected(next);
        self.list.scroll_to(next, gtk::ListScrollFlags::NONE, None);
    }
}

fn palette_key(
    palette: Option<&CommandPalette>,
    key: gdk::Key,
    modifiers: gdk::ModifierType,
) -> glib::Propagation {
    let Some(palette) = palette else {
        return glib::Propagation::Proceed;
    };
    if modifiers.contains(gdk::ModifierType::CONTROL_MASK)
        && matches!(key, gdk::Key::k | gdk::Key::K)
    {
        palette.close();
        return glib::Propagation::Stop;
    }
    match key {
        gdk::Key::Down | gdk::Key::KP_Down => {
            palette.step_selection(1);
            glib::Propagation::Stop
        }
        gdk::Key::Up | gdk::Key::KP_Up => {
            palette.step_selection(-1);
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    }
}

fn bind_palette_row(object: &glib::Object, on_action: &Rc<dyn Fn(PaletteAction)>) {
    let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
    let value = item
        .item()
        .and_downcast::<glib::BoxedAnyObject>()
        .expect("palette entry");
    let entry = value.borrow::<PaletteEntry>();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("palette-row");
    if entry.active {
        row.add_css_class("palette-row-active");
    }
    let icon = gtk::Image::from_icon_name(entry.icon);
    icon.add_css_class("palette-icon");
    if entry.destructive {
        icon.add_css_class("palette-destructive");
    }
    row.append(&icon);
    row.append(&palette_row_labels(&entry));
    append_palette_badges(&row, &entry);

    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_PRIMARY)
        .build();
    let action = entry.action.clone();
    let cb = Rc::clone(on_action);
    click.connect_released(move |_, _, _, _| {
        cb(action.clone());
    });
    row.add_controller(click);
    item.set_child(Some(&row));
}

fn palette_row_labels(entry: &PaletteEntry) -> gtk::Box {
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    labels.set_hexpand(true);
    let title = gtk::Label::new(Some(&entry.title));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("palette-title");
    if entry.destructive {
        title.add_css_class("palette-destructive");
    }
    labels.append(&title);
    if !entry.subtitle.is_empty() {
        let subtitle = gtk::Label::new(Some(&entry.subtitle));
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subtitle.add_css_class("palette-subtitle");
        labels.append(&subtitle);
    }
    labels
}

fn append_palette_badges(row: &gtk::Box, entry: &PaletteEntry) {
    if !entry.badge.is_empty() {
        let badge = gtk::Label::new(Some(&entry.badge));
        badge.add_css_class("source-badge");
        badge.set_valign(gtk::Align::Center);
        if matches!(entry.action, PaletteAction::OpenSession(_)) && entry.badge == "CRON" {
            badge.add_css_class("source-auto");
        } else if entry.badge == "PARENT" {
            badge.add_css_class("fork-badge");
        }
        row.append(&badge);
    }
    if !entry.shortcut.is_empty() {
        let kbd = gtk::Label::new(Some(entry.shortcut));
        kbd.add_css_class("kbd-badge");
        kbd.set_valign(gtk::Align::Center);
        row.append(&kbd);
    }
}

fn palette_footer() -> gtk::Box {
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.add_css_class("palette-footer");
    let hint = gtk::Label::new(Some("↑↓ navigate · Enter run · Esc close"));
    hint.set_xalign(0.0);
    hint.set_hexpand(true);
    hint.add_css_class("composer-hint");
    footer.append(&hint);
    footer
}

pub fn build_palette_trigger() -> gtk::Button {
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    inner.append(&gtk::Image::from_icon_name("system-search-symbolic"));
    let label = gtk::Label::new(Some("Commands"));
    label.add_css_class("palette-trigger-label");
    inner.append(&label);
    let kbd = gtk::Label::new(Some("Ctrl+K"));
    kbd.add_css_class("kbd-badge");
    inner.append(&kbd);
    let button = gtk::Button::new();
    button.set_child(Some(&inner));
    button.add_css_class("flat");
    button.add_css_class("palette-trigger");
    button.set_tooltip_text(Some("Open command palette and session jumper · Ctrl+K"));
    button.update_property(&[gtk::accessible::Property::Label("Command palette")]);
    button
}

pub fn build_shortcut_hints() -> gtk::Box {
    let stack = gtk::Box::new(gtk::Orientation::Vertical, 6);
    stack.set_halign(gtk::Align::Center);
    stack.add_css_class("shortcut-strip");
    stack.append(&shortcut_hint_row(&[
        ("Enter", "Start chat"),
        ("Ctrl+K", "Palette"),
    ]));
    stack.append(&shortcut_hint_row(&[
        ("Ctrl+I", "Flight Deck"),
        ("Ctrl+D", "Branch"),
        ("Alt+Enter", "Side-Ask"),
    ]));
    stack
}

fn shortcut_hint_row(items: &[(&str, &str)]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.set_halign(gtk::Align::Center);
    for (key, desc) in items {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        item.add_css_class("shortcut-chip");
        let kbd = gtk::Label::new(Some(key));
        kbd.add_css_class("kbd-badge");
        let text = gtk::Label::new(Some(desc));
        text.add_css_class("shortcut-desc");
        item.append(&kbd);
        item.append(&text);
        row.append(&item);
    }
    row
}

pub fn bind_session_row(object: &glib::Object) {
    let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
    let value = item
        .item()
        .and_downcast::<glib::BoxedAnyObject>()
        .expect("session item");
    let session = value.borrow::<SessionSummary>();
    let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
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

    let meta_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
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

    let meta = gtk::Label::new(Some(&session_meta_line(&session, now_epoch_secs())));
    meta.set_xalign(0.0);
    meta.set_hexpand(true);
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
    meta.add_css_class("session-meta");
    meta_row.append(&meta);
    row.append(&meta_row);

    let identity = match &session.read_only_reason {
        Some(reason) => format!("Conversation ID: {}\n{}", session.stored_id, reason),
        None => format!("Conversation ID: {}", session.stored_id),
    };
    row.set_tooltip_text(Some(&identity));
    item.set_child(Some(&row));
}

pub fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn set_box_margins(widget: &impl IsA<gtk::Widget>, amount: i32) {
    widget.set_margin_top(amount);
    widget.set_margin_bottom(amount);
    widget.set_margin_start(amount);
    widget.set_margin_end(amount);
}
