//! Native Markdown for chat transcripts.
//!
//! Call [`render`] on the GTK main thread after `gtk::init`. The returned
//! widget does not retain the source; whole-message copy stays with the UI.
//! Re-render the full source on each streaming update.
//!
//! Inline emphasis is Pango (`<b>`, `<i>`, `<s>`, `<tt>`, `<a>`), not CSS.
//! Everything else is a theme widget with an `md-*` class. No colors are set
//! here. Suggested `style.css` hooks:
//!
//! - `md-root` vertical transcript block (box spacing 6)
//! - `md-p` paragraph, `md-h1`…`md-h6` headings, `md-html` literal HTML
//! - `md-quote` block quote (start margin 12, spacing 4)
//! - `md-list`, `md-list-ordered`, `md-list-unordered`, `md-item`,
//!   `md-marker`, `md-item-body`, `md-task`
//! - `md-rule` thematic break
//! - `md-codeblock`, `md-code-header`, `md-code-lang`, `md-code-copy`,
//!   `md-code-scroll`, `md-code-view`
//! - `md-table-scroll`, `md-table`, `md-th`, `md-td`
//!
//! Only `http`, `https`, and `mailto` links launch, and only after
//! normalization. HTML is literal text. Images are alt text and never fetched.

use gtk::accessible::{Property, Relation};
use gtk::glib;
use gtk::prelude::*;
use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd,
};

const MAX_DEPTH: usize = 64;
const MAX_HREF_LEN: usize = 8 * 1024;

/// Build a selectable native widget for `source`.
#[must_use]
pub fn render(source: &str) -> gtk::Widget {
    let blocks = parse(source);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.add_css_class("md-root");
    root.set_hexpand(true);
    root.set_halign(gtk::Align::Fill);
    root.set_valign(gtk::Align::Start);
    root.set_can_focus(false);
    render_blocks(&root, &blocks);
    root.upcast()
}

#[derive(Debug, PartialEq, Eq)]
enum Inline {
    Text(String),
    Code(String),
    Html(String),
    SoftBreak,
    HardBreak,
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strike(Vec<Inline>),
    /// `href` is `Some` only when the destination may be launched.
    Link {
        href: Option<String>,
        children: Vec<Inline>,
    },
    /// Alt text only. The image URL is never stored or fetched.
    Image {
        alt: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct ListItem {
    task: Option<bool>,
    blocks: Vec<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, PartialEq, Eq)]
enum Block {
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    Quote(Vec<Block>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Code {
        language: String,
        code: String,
    },
    Rule,
    Table {
        aligns: Vec<ColAlign>,
        headers: Vec<Vec<Vec<Inline>>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Html(String),
}

enum InlineKind {
    Paragraph,
    Heading(u8),
    Cell,
    Strong,
    Emphasis,
    Strike,
    Link { href: Option<String> },
    Image,
}

enum Frame {
    Blocks(Vec<Block>),
    Quote(Vec<Block>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Item {
        task: Option<bool>,
        blocks: Vec<Block>,
    },
    Table {
        aligns: Vec<ColAlign>,
        in_header: bool,
        headers: Vec<Vec<Vec<Inline>>>,
        rows: Vec<Vec<Vec<Inline>>>,
        row: Vec<Vec<Inline>>,
    },
    Code {
        language: String,
        text: String,
    },
    Html(String),
    Inlines {
        kind: InlineKind,
        items: Vec<Inline>,
    },
}

fn parse(source: &str) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut stack = vec![Frame::Blocks(Vec::new())];
    let mut skip = 0u32;

    for event in Parser::new_ext(source, options) {
        if skip > 0 {
            match event {
                Event::Start(_) => skip += 1,
                Event::End(_) => skip -= 1,
                Event::Text(text)
                | Event::Code(text)
                | Event::Html(text)
                | Event::InlineHtml(text) => {
                    append_overflow(&mut stack, text.as_ref());
                }
                Event::SoftBreak | Event::HardBreak => append_overflow(&mut stack, "\n"),
                Event::FootnoteReference(label) => {
                    append_overflow(&mut stack, &format!("[^{}]", label.as_ref()));
                }
                Event::InlineMath(text) | Event::DisplayMath(text) => {
                    append_overflow(&mut stack, text.as_ref());
                }
                Event::Rule | Event::TaskListMarker(_) => {}
            }
            continue;
        }

        match event {
            Event::Start(tag) => {
                if stack.len() >= MAX_DEPTH {
                    skip = 1;
                    continue;
                }
                start_tag(&mut stack, tag);
            }
            Event::End(tag) => end_tag(&mut stack, tag),
            Event::Text(text) => push_text(&mut stack, text.as_ref()),
            Event::Code(text) => push_code_span(&mut stack, text.as_ref()),
            Event::Html(text) | Event::InlineHtml(text) => push_html(&mut stack, text.as_ref()),
            Event::SoftBreak => push_break(&mut stack, false),
            Event::HardBreak => push_break(&mut stack, true),
            Event::Rule => push_block(&mut stack, Block::Rule),
            Event::TaskListMarker(checked) => set_task(&mut stack, checked),
            Event::FootnoteReference(label) => {
                push_text(&mut stack, &format!("[^{}]", label.as_ref()));
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                push_text(&mut stack, text.as_ref())
            }
        }
    }

    while stack.len() > 1 {
        close_top(&mut stack);
    }
    match stack.pop() {
        Some(Frame::Blocks(blocks)) => blocks,
        _ => Vec::new(),
    }
}

fn start_tag(stack: &mut Vec<Frame>, tag: Tag<'_>) {
    match tag {
        Tag::Paragraph => push_inlines(stack, InlineKind::Paragraph),
        Tag::Heading { level, .. } => {
            push_inlines(stack, InlineKind::Heading(heading_level(level)))
        }
        Tag::BlockQuote(_) => stack.push(Frame::Quote(Vec::new())),
        Tag::CodeBlock(kind) => stack.push(Frame::Code {
            language: code_language(kind),
            text: String::new(),
        }),
        Tag::HtmlBlock => stack.push(Frame::Html(String::new())),
        Tag::List(start) => stack.push(Frame::List {
            ordered: start.is_some(),
            start: start.unwrap_or(1),
            items: Vec::new(),
        }),
        Tag::Item => stack.push(Frame::Item {
            task: None,
            blocks: Vec::new(),
        }),
        Tag::Table(aligns) => stack.push(Frame::Table {
            aligns: aligns.into_iter().map(col_align).collect(),
            in_header: false,
            headers: Vec::new(),
            rows: Vec::new(),
            row: Vec::new(),
        }),
        Tag::TableHead => {
            if let Some(Frame::Table { in_header, .. }) = stack.last_mut() {
                *in_header = true;
            }
        }
        Tag::TableRow => {
            if let Some(Frame::Table { row, .. }) = stack.last_mut() {
                row.clear();
            }
        }
        Tag::TableCell => push_inlines(stack, InlineKind::Cell),
        Tag::Emphasis => push_inlines(stack, InlineKind::Emphasis),
        Tag::Strong => push_inlines(stack, InlineKind::Strong),
        Tag::Strikethrough => push_inlines(stack, InlineKind::Strike),
        Tag::Superscript | Tag::Subscript => push_inlines(stack, InlineKind::Emphasis),
        Tag::Link {
            link_type,
            dest_url,
            ..
        } => push_inlines(
            stack,
            InlineKind::Link {
                href: link_href(link_type, dest_url.as_ref()),
            },
        ),
        Tag::Image { .. } => push_inlines(stack, InlineKind::Image),
        Tag::FootnoteDefinition(_)
        | Tag::MetadataBlock(_)
        | Tag::DefinitionList
        | Tag::DefinitionListDefinition => stack.push(Frame::Blocks(Vec::new())),
        Tag::DefinitionListTitle => push_inlines(stack, InlineKind::Paragraph),
    }
}

fn end_tag(stack: &mut Vec<Frame>, tag: TagEnd) {
    match tag {
        TagEnd::Paragraph
        | TagEnd::Heading(_)
        | TagEnd::TableCell
        | TagEnd::Emphasis
        | TagEnd::Strong
        | TagEnd::Strikethrough
        | TagEnd::Superscript
        | TagEnd::Subscript
        | TagEnd::Link
        | TagEnd::Image
        | TagEnd::DefinitionListTitle => close_inline(stack),
        TagEnd::CodeBlock => close_code(stack),
        TagEnd::HtmlBlock => close_html(stack),
        TagEnd::List(_) => close_list(stack),
        TagEnd::Item => close_item(stack),
        TagEnd::BlockQuote(_) => close_quote(stack),
        TagEnd::Table => close_table(stack),
        TagEnd::TableHead => {
            flush_row(stack);
            if let Some(Frame::Table { in_header, .. }) = stack.last_mut() {
                *in_header = false;
            }
        }
        TagEnd::TableRow => flush_row(stack),
        TagEnd::FootnoteDefinition
        | TagEnd::DefinitionList
        | TagEnd::DefinitionListDefinition
        | TagEnd::MetadataBlock(_) => close_blocks_frame(stack),
    }
}

fn push_inlines(stack: &mut Vec<Frame>, kind: InlineKind) {
    stack.push(Frame::Inlines {
        kind,
        items: Vec::new(),
    });
}

fn push_text(stack: &mut [Frame], text: &str) {
    match stack.last_mut() {
        Some(Frame::Code { text: buf, .. } | Frame::Html(buf)) => buf.push_str(text),
        Some(Frame::Inlines { items, .. }) => push_inline(items, Inline::Text(text.to_string())),
        _ => append_overflow(stack, text),
    }
}

fn push_code_span(stack: &mut [Frame], text: &str) {
    match stack.last_mut() {
        Some(Frame::Code { text: buf, .. } | Frame::Html(buf)) => buf.push_str(text),
        Some(Frame::Inlines { items, .. }) => push_inline(items, Inline::Code(text.to_string())),
        _ => push_implicit_inline(stack, Inline::Code(text.to_string())),
    }
}

fn push_html(stack: &mut [Frame], text: &str) {
    match stack.last_mut() {
        Some(Frame::Code { text: buf, .. } | Frame::Html(buf)) => buf.push_str(text),
        Some(Frame::Inlines { items, .. }) => push_inline(items, Inline::Html(text.to_string())),
        _ => push_implicit_inline(stack, Inline::Html(text.to_string())),
    }
}

fn push_break(stack: &mut [Frame], hard: bool) {
    match stack.last_mut() {
        Some(Frame::Code { text, .. } | Frame::Html(text)) => text.push('\n'),
        Some(Frame::Inlines { items, .. }) => {
            items.push(if hard {
                Inline::HardBreak
            } else {
                Inline::SoftBreak
            });
        }
        _ => append_overflow(stack, if hard { "\n" } else { " " }),
    }
}

fn push_inline(items: &mut Vec<Inline>, inline: Inline) {
    match (items.last_mut(), inline) {
        (Some(Inline::Text(existing)), Inline::Text(extra))
        | (Some(Inline::Html(existing)), Inline::Html(extra)) => existing.push_str(&extra),
        (_, inline) => items.push(inline),
    }
}

fn set_task(stack: &mut [Frame], checked: bool) {
    for frame in stack.iter_mut().rev() {
        if let Frame::Item { task, .. } = frame {
            if task.is_none() {
                *task = Some(checked);
            }
            return;
        }
    }
}

fn close_inline(stack: &mut Vec<Frame>) {
    let Some(Frame::Inlines { kind, items }) = stack.pop() else {
        return;
    };
    match kind {
        InlineKind::Paragraph => {
            if !items.is_empty() {
                push_block(stack, Block::Paragraph(items));
            }
        }
        InlineKind::Heading(level) => {
            if !items.is_empty() {
                push_block(
                    stack,
                    Block::Heading {
                        level,
                        inlines: items,
                    },
                );
            }
        }
        InlineKind::Cell => {
            if let Some(Frame::Table { row, .. }) = stack.last_mut() {
                row.push(items);
            }
        }
        InlineKind::Strong => push_wrapped(stack, Inline::Strong, items),
        InlineKind::Emphasis => push_wrapped(stack, Inline::Emphasis, items),
        InlineKind::Strike => push_wrapped(stack, Inline::Strike, items),
        InlineKind::Link { href } => {
            push_wrapped(stack, |children| Inline::Link { href, children }, items);
        }
        InlineKind::Image => {
            let alt = plain_text(&items);
            push_wrapped(stack, |_| Inline::Image { alt }, Vec::new());
        }
    }
}

fn push_wrapped(stack: &mut [Frame], wrap: impl FnOnce(Vec<Inline>) -> Inline, items: Vec<Inline>) {
    push_implicit_inline(stack, wrap(items));
}

fn push_implicit_inline(stack: &mut [Frame], inline: Inline) {
    for frame in stack.iter_mut().rev() {
        match frame {
            Frame::Inlines { items, .. } => {
                push_inline(items, inline);
                return;
            }
            Frame::Blocks(blocks) | Frame::Quote(blocks) | Frame::Item { blocks, .. } => {
                if let Some(Block::Paragraph(items)) = blocks.last_mut() {
                    push_inline(items, inline);
                } else {
                    blocks.push(Block::Paragraph(vec![inline]));
                }
                return;
            }
            _ => {}
        }
    }
}

fn close_code(stack: &mut Vec<Frame>) {
    let Some(Frame::Code { language, text }) = stack.pop() else {
        return;
    };
    push_block(
        stack,
        Block::Code {
            language,
            code: text,
        },
    );
}

fn close_html(stack: &mut Vec<Frame>) {
    let Some(Frame::Html(text)) = stack.pop() else {
        return;
    };
    if !text.is_empty() {
        push_block(stack, Block::Html(text));
    }
}

fn close_list(stack: &mut Vec<Frame>) {
    let Some(Frame::List {
        ordered,
        start,
        items,
    }) = stack.pop()
    else {
        return;
    };
    push_block(
        stack,
        Block::List {
            ordered,
            start,
            items,
        },
    );
}

fn close_item(stack: &mut Vec<Frame>) {
    let Some(Frame::Item { task, blocks }) = stack.pop() else {
        return;
    };
    let item = ListItem { task, blocks };
    if let Some(Frame::List { items, .. }) = stack.last_mut() {
        items.push(item);
    } else {
        for block in item.blocks {
            push_block(stack, block);
        }
    }
}

fn close_quote(stack: &mut Vec<Frame>) {
    let Some(Frame::Quote(blocks)) = stack.pop() else {
        return;
    };
    push_block(stack, Block::Quote(blocks));
}

fn close_table(stack: &mut Vec<Frame>) {
    flush_row(stack);
    let Some(Frame::Table {
        aligns,
        headers,
        rows,
        ..
    }) = stack.pop()
    else {
        return;
    };
    if headers.is_empty() && rows.is_empty() {
        return;
    }
    push_block(
        stack,
        Block::Table {
            aligns,
            headers,
            rows,
        },
    );
}

fn flush_row(stack: &mut [Frame]) {
    let Some(Frame::Table {
        in_header,
        headers,
        rows,
        row,
        ..
    }) = stack.last_mut()
    else {
        return;
    };
    if row.is_empty() {
        return;
    }
    let finished = std::mem::take(row);
    if *in_header {
        headers.push(finished);
    } else {
        rows.push(finished);
    }
}

fn close_blocks_frame(stack: &mut Vec<Frame>) {
    if stack.len() <= 1 || !matches!(stack.last(), Some(Frame::Blocks(_))) {
        return;
    }
    if let Some(Frame::Blocks(blocks)) = stack.pop() {
        for block in blocks {
            push_block(stack, block);
        }
    }
}

fn close_top(stack: &mut Vec<Frame>) {
    match stack.last() {
        Some(Frame::Inlines { .. }) => close_inline(stack),
        Some(Frame::Code { .. }) => close_code(stack),
        Some(Frame::Html(_)) => close_html(stack),
        Some(Frame::List { .. }) => close_list(stack),
        Some(Frame::Item { .. }) => close_item(stack),
        Some(Frame::Quote(_)) => close_quote(stack),
        Some(Frame::Table { .. }) => close_table(stack),
        Some(Frame::Blocks(_)) => close_blocks_frame(stack),
        None => {}
    }
}

fn push_block(stack: &mut [Frame], block: Block) {
    for frame in stack.iter_mut().rev() {
        match frame {
            Frame::Blocks(blocks) | Frame::Quote(blocks) | Frame::Item { blocks, .. } => {
                blocks.push(block);
                return;
            }
            _ => {}
        }
    }
}

fn append_overflow(stack: &mut [Frame], text: &str) {
    if text.is_empty() {
        return;
    }
    match stack.last_mut() {
        Some(Frame::Code { text: target, .. } | Frame::Html(target)) => target.push_str(text),
        _ => push_implicit_inline(stack, Inline::Text(text.to_string())),
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn code_language(kind: CodeBlockKind<'_>) -> String {
    match kind {
        CodeBlockKind::Indented => String::new(),
        CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
    }
}

fn col_align(align: Alignment) -> ColAlign {
    match align {
        Alignment::Center => ColAlign::Center,
        Alignment::Right => ColAlign::Right,
        Alignment::Left | Alignment::None => ColAlign::Left,
    }
}

fn link_href(link_type: LinkType, dest: &str) -> Option<String> {
    if matches!(link_type, LinkType::Email) && !dest.contains(':') {
        let mut mailto = String::from("mailto:");
        mailto.push_str(dest.trim());
        return safe_href(&mailto);
    }
    safe_href(dest)
}

/// Launchable href, or `None` when the destination must stay inert.
///
/// The returned string is the normalized URL, never the raw destination.
/// Callers still escape it before placing it in Pango markup, and the label
/// handler checks again before `UriLauncher`.
fn safe_href(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > MAX_HREF_LEN {
        return None;
    }
    if raw.chars().any(href_forbidden) {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    match url.scheme() {
        "http" | "https" => {
            if url.host_str().is_none_or(str::is_empty) {
                return None;
            }
        }
        "mailto" => {}
        _ => return None,
    }
    let normalized = url.to_string();
    let scheme_end = normalized.find(':')?;
    if !matches!(&normalized[..scheme_end], "http" | "https" | "mailto") {
        return None;
    }
    if normalized.chars().any(href_forbidden) {
        return None;
    }
    Some(normalized)
}

fn href_forbidden(c: char) -> bool {
    c.is_control()
        || c.is_whitespace()
        || matches!(
            c,
            '<' | '>'
                | '"'
                | '\''
                | '`'
                | '\\'
                | '\u{00AD}'
                | '\u{200B}'
                | '\u{200C}'
                | '\u{200D}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{2060}'
                | '\u{FEFF}'
                | '\u{2028}'
                | '\u{2029}'
        )
        || ('\u{202A}'..='\u{202E}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
}

fn plain_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    write_plain(&mut out, inlines);
    out
}

fn write_plain(out: &mut String, inlines: &[Inline]) {
    for inline in inlines {
        match inline {
            Inline::Text(text) | Inline::Code(text) | Inline::Html(text) => out.push_str(text),
            Inline::SoftBreak | Inline::HardBreak => out.push(' '),
            Inline::Strong(children)
            | Inline::Emphasis(children)
            | Inline::Strike(children)
            | Inline::Link { children, .. } => write_plain(out, children),
            Inline::Image { alt } => out.push_str(alt),
        }
    }
}

fn inline_markup(inlines: &[Inline]) -> String {
    let mut out = String::new();
    write_markup(&mut out, inlines);
    out
}

fn write_markup(out: &mut String, inlines: &[Inline]) {
    for inline in inlines {
        match inline {
            Inline::Text(text) | Inline::Html(text) => push_escaped(out, text),
            Inline::Code(text) => {
                out.push_str("<tt>");
                push_escaped(out, text);
                out.push_str("</tt>");
            }
            Inline::SoftBreak => out.push(' '),
            Inline::HardBreak => out.push('\n'),
            Inline::Strong(children) => wrap_tag(out, "b", children),
            Inline::Emphasis(children) => wrap_tag(out, "i", children),
            Inline::Strike(children) => wrap_tag(out, "s", children),
            Inline::Link {
                href: Some(href),
                children,
            } => {
                out.push_str("<a href=\"");
                push_escaped(out, href);
                out.push_str("\">");
                let start = out.len();
                write_markup(out, children);
                if out.len() == start {
                    push_escaped(out, href);
                }
                out.push_str("</a>");
            }
            Inline::Link {
                href: None,
                children,
            } => write_markup(out, children),
            Inline::Image { alt } => {
                if alt.is_empty() {
                    out.push_str("image");
                } else {
                    push_escaped(out, alt);
                }
            }
        }
    }
}

fn wrap_tag(out: &mut String, tag: &str, children: &[Inline]) {
    out.push('<');
    out.push_str(tag);
    out.push('>');
    write_markup(out, children);
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn push_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\0' => out.push('\u{FFFD}'),
            '\r' => {}
            c if c.is_control() && c != '\n' && c != '\t' => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
}

fn render_blocks(parent: &gtk::Box, blocks: &[Block]) {
    for block in blocks {
        parent.append(&render_block(block));
    }
}

fn render_block(block: &Block) -> gtk::Widget {
    match block {
        Block::Paragraph(inlines) => {
            let label = prose_label(&inline_markup(inlines), &["md-p"]);
            label.set_accessible_role(gtk::AccessibleRole::Paragraph);
            label.upcast()
        }
        Block::Heading { level, inlines } => {
            let class = match level {
                1 => "md-h1",
                2 => "md-h2",
                3 => "md-h3",
                4 => "md-h4",
                5 => "md-h5",
                _ => "md-h6",
            };
            let markup = format!("<b>{}</b>", inline_markup(inlines));
            let label = prose_label(&markup, &[class]);
            label.set_accessible_role(gtk::AccessibleRole::Heading);
            label.update_property(&[Property::Level(i32::from(*level))]);
            label.upcast()
        }
        Block::Quote(blocks) => {
            let quote = gtk::Box::new(gtk::Orientation::Vertical, 4);
            quote.add_css_class("md-quote");
            quote.set_accessible_role(gtk::AccessibleRole::BlockQuote);
            quote.set_margin_start(12);
            quote.set_hexpand(true);
            quote.set_halign(gtk::Align::Fill);
            render_blocks(&quote, blocks);
            quote.upcast()
        }
        Block::List {
            ordered,
            start,
            items,
        } => render_list(*ordered, *start, items).upcast(),
        Block::Code { language, code } => render_code(language, code).upcast(),
        Block::Rule => {
            let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
            rule.add_css_class("md-rule");
            rule.set_hexpand(true);
            rule.set_margin_top(4);
            rule.set_margin_bottom(4);
            rule.upcast()
        }
        Block::Table {
            aligns,
            headers,
            rows,
        } => render_table(aligns, headers, rows).upcast(),
        Block::Html(text) => {
            let mut escaped = String::new();
            push_escaped(&mut escaped, text);
            let label = prose_label(&escaped, &["md-html"]);
            label.set_accessible_role(gtk::AccessibleRole::Paragraph);
            label.update_property(&[Property::Description("Literal HTML, not rendered")]);
            label.upcast()
        }
    }
}

fn prose_label(markup: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_use_markup(true);
    label.set_markup(markup);
    label.set_selectable(true);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_natural_wrap_mode(gtk::NaturalWrapMode::Word);
    label.set_max_width_chars(72);
    label.set_xalign(0.0);
    label.set_halign(gtk::Align::Fill);
    label.set_hexpand(true);
    label.set_valign(gtk::Align::Start);
    label.set_justify(gtk::Justification::Left);
    label.set_accessible_role(gtk::AccessibleRole::Paragraph);
    for class in classes {
        label.add_css_class(class);
    }
    label.connect_activate_link(|label, uri| {
        if let Some(safe) = safe_href(uri) {
            let window = label.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&safe).launch(
                window.as_ref(),
                None::<&gtk::gio::Cancellable>,
                |_| {},
            );
        }
        glib::Propagation::Stop
    });
    label
}

fn render_list(ordered: bool, start: u64, items: &[ListItem]) -> gtk::Box {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    list.add_css_class("md-list");
    list.add_css_class(if ordered {
        "md-list-ordered"
    } else {
        "md-list-unordered"
    });
    list.set_accessible_role(gtk::AccessibleRole::List);
    list.set_hexpand(true);
    list.set_halign(gtk::Align::Fill);
    let count = i32::try_from(items.len()).unwrap_or(i32::MAX);
    let mut number = start;
    for (index, item) in items.iter().enumerate() {
        let pos = i32::try_from(index).unwrap_or(i32::MAX).saturating_add(1);
        list.append(&render_item(item, ordered, number, pos, count));
        number = number.saturating_add(1);
    }
    list
}

fn render_item(item: &ListItem, ordered: bool, number: u64, pos: i32, count: i32) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("md-item");
    row.set_accessible_role(gtk::AccessibleRole::ListItem);
    row.update_relation(&[Relation::PosInSet(pos), Relation::SetSize(count)]);
    row.set_hexpand(true);
    row.set_halign(gtk::Align::Fill);
    row.set_valign(gtk::Align::Start);

    if let Some(checked) = item.task {
        let check = gtk::CheckButton::new();
        check.add_css_class("md-task");
        check.set_active(checked);
        check.set_can_target(false);
        check.set_focusable(false);
        check.set_halign(gtk::Align::Start);
        check.set_valign(gtk::Align::Start);
        check.set_margin_top(2);
        let name = if checked {
            "Completed task"
        } else {
            "Incomplete task"
        };
        check.update_property(&[Property::Label(name), Property::ReadOnly(true)]);
        row.append(&check);
    } else {
        let marker_text = if ordered {
            format!("{number}.")
        } else {
            "•".to_string()
        };
        let marker = gtk::Label::new(Some(&marker_text));
        marker.add_css_class("md-marker");
        marker.set_accessible_role(gtk::AccessibleRole::Presentation);
        marker.set_selectable(false);
        marker.set_halign(gtk::Align::Start);
        marker.set_valign(gtk::Align::Start);
        marker.set_xalign(1.0);
        row.append(&marker);
    }

    let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
    body.add_css_class("md-item-body");
    body.set_hexpand(true);
    body.set_halign(gtk::Align::Fill);
    render_blocks(&body, &item.blocks);
    row.append(&body);
    row
}

fn render_code(language: &str, code: &str) -> gtk::Box {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
    block.add_css_class("md-codeblock");
    block.set_hexpand(true);
    block.set_halign(gtk::Align::Fill);
    block.set_accessible_role(gtk::AccessibleRole::Group);
    let accessible = code_accessible_name(language);
    block.update_property(&[Property::Description(&accessible)]);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.add_css_class("md-code-header");
    header.set_hexpand(true);
    if language.is_empty() {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        header.append(&spacer);
    } else {
        let lang = gtk::Label::new(Some(language));
        lang.add_css_class("md-code-lang");
        lang.set_selectable(true);
        lang.set_wrap(true);
        lang.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        lang.set_max_width_chars(24);
        lang.set_xalign(0.0);
        lang.set_halign(gtk::Align::Start);
        lang.set_hexpand(true);
        header.append(&lang);
    }

    let copy = gtk::Button::with_label("Copy code");
    copy.add_css_class("md-code-copy");
    copy.add_css_class("flat");
    copy.set_tooltip_text(Some("Copy code"));
    copy.set_halign(gtk::Align::End);
    copy.set_valign(gtk::Align::Center);
    let copied = code.to_string();
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&copied);
    });
    header.append(&copy);
    block.append(&header);

    let view = gtk::TextView::new();
    view.add_css_class("md-code-view");
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::None);
    view.set_accepts_tab(false);
    view.set_left_margin(8);
    view.set_right_margin(8);
    view.set_top_margin(6);
    view.set_bottom_margin(6);
    view.set_hexpand(true);
    view.set_vexpand(false);
    view.buffer().set_text(code);
    view.update_property(&[Property::ReadOnly(true)]);

    let scroll = horizontal_scroll(&view, "md-code-scroll");
    block.append(&scroll);
    block
}

fn code_accessible_name(language: &str) -> String {
    if language.is_empty() {
        "Code".to_string()
    } else {
        let lang: String = language.chars().take(64).collect();
        format!("{lang} code")
    }
}

fn render_table(
    aligns: &[ColAlign],
    headers: &[Vec<Vec<Inline>>],
    rows: &[Vec<Vec<Inline>>],
) -> gtk::ScrolledWindow {
    let grid = gtk::Grid::new();
    grid.add_css_class("md-table");
    grid.set_accessible_role(gtk::AccessibleRole::Table);
    grid.set_row_spacing(4);
    grid.set_column_spacing(12);
    grid.set_hexpand(true);
    grid.set_halign(gtk::Align::Fill);
    grid.set_vexpand(false);
    grid.set_valign(gtk::Align::Start);
    let cols = table_columns(aligns, headers, rows);
    let row_count = i32::try_from(headers.len() + rows.len()).unwrap_or(i32::MAX);
    let col_count = i32::try_from(cols).unwrap_or(i32::MAX);
    grid.update_relation(&[Relation::RowCount(row_count), Relation::ColCount(col_count)]);

    let mut row_index = 1i32;
    for header in headers {
        attach_row(&grid, header, aligns, cols, row_index, true);
        row_index = row_index.saturating_add(1);
    }
    for row in rows {
        attach_row(&grid, row, aligns, cols, row_index, false);
        row_index = row_index.saturating_add(1);
    }
    horizontal_scroll(&grid, "md-table-scroll")
}

fn table_columns(
    aligns: &[ColAlign],
    headers: &[Vec<Vec<Inline>>],
    rows: &[Vec<Vec<Inline>>],
) -> usize {
    let mut cols = aligns.len();
    for row in headers.iter().chain(rows) {
        cols = cols.max(row.len());
    }
    cols
}

fn attach_row(
    grid: &gtk::Grid,
    cells: &[Vec<Inline>],
    aligns: &[ColAlign],
    cols: usize,
    row_index: i32,
    header: bool,
) {
    for col in 0..cols {
        let inlines = cells.get(col).map(Vec::as_slice).unwrap_or(&[]);
        let align = aligns.get(col).copied().unwrap_or(ColAlign::Left);
        let mut markup = inline_markup(inlines);
        if header && !markup.is_empty() {
            markup = format!("<b>{markup}</b>");
        }
        let label = prose_label(&markup, &[if header { "md-th" } else { "md-td" }]);
        // GtkGrid, asked for an unconstrained height, sizes columns to each
        // child's minimum width and then measures height at that width.
        // A wrapping label's minimum width is about one character, so the
        // row becomes a stack of glyphs. GtkScrolledWindow does not forward
        // the real width unless the horizontal policy is NEVER
        // (gtk_scrolled_window_measure, GTK 4.24), and a NEVER vertical
        // policy adopts that stacked minimum. The transcript list uses the
        // row minimum, so the frame is a tall blank box and follow-bottom
        // hides the text above the viewport. Keep cells unwrapped: height
        // is the hard-break line count, and wide text scrolls horizontally.
        label.set_wrap(false);
        label.set_ellipsize(gtk::pango::EllipsizeMode::None);
        label.set_width_chars(-1);
        label.set_max_width_chars(-1);
        label.set_vexpand(false);
        label.set_valign(gtk::Align::Start);
        label.set_accessible_role(if header {
            gtk::AccessibleRole::ColumnHeader
        } else {
            gtk::AccessibleRole::Cell
        });
        apply_align(&label, align);
        let column = i32::try_from(col).unwrap_or(i32::MAX).saturating_add(1);
        label.update_relation(&[Relation::RowIndex(row_index), Relation::ColIndex(column)]);
        grid.attach(&label, column - 1, row_index - 1, 1, 1);
    }
}

fn apply_align(label: &gtk::Label, align: ColAlign) {
    let (xalign, justify) = match align {
        ColAlign::Left => (0.0, gtk::Justification::Left),
        ColAlign::Center => (0.5, gtk::Justification::Center),
        ColAlign::Right => (1.0, gtk::Justification::Right),
    };
    label.set_xalign(xalign);
    label.set_justify(justify);
}

fn horizontal_scroll(child: &impl IsA<gtk::Widget>, class: &str) -> gtk::ScrolledWindow {
    // Height must not depend on a width this window refuses to forward.
    // Callers keep children width-independent (code does not wrap; table
    // cells do not wrap) so the minimum height is the content height.
    child.set_vexpand(false);
    child.set_valign(gtk::Align::Start);
    let scroll = gtk::ScrolledWindow::new();
    scroll.add_css_class(class);
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    scroll.set_propagate_natural_width(false);
    scroll.set_propagate_natural_height(true);
    scroll.set_min_content_width(0);
    scroll.set_min_content_height(0);
    scroll.set_has_frame(true);
    scroll.set_hexpand(true);
    scroll.set_vexpand(false);
    scroll.set_halign(gtk::Align::Fill);
    scroll.set_valign(gtk::Align::Start);
    scroll.set_child(Some(child));
    scroll
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hrefs(blocks: &[Block]) -> Vec<Option<String>> {
        let mut out = Vec::new();
        for block in blocks {
            collect_block_hrefs(block, &mut out);
        }
        out
    }

    fn collect_block_hrefs(block: &Block, out: &mut Vec<Option<String>>) {
        match block {
            Block::Paragraph(inlines) | Block::Heading { inlines, .. } => {
                collect_hrefs(inlines, out)
            }
            Block::Quote(blocks) => {
                for child in blocks {
                    collect_block_hrefs(child, out);
                }
            }
            Block::List { items, .. } => {
                for item in items {
                    for child in &item.blocks {
                        collect_block_hrefs(child, out);
                    }
                }
            }
            Block::Table { headers, rows, .. } => {
                for row in headers.iter().chain(rows) {
                    for cell in row {
                        collect_hrefs(cell, out);
                    }
                }
            }
            Block::Code { .. } | Block::Rule | Block::Html(_) => {}
        }
    }

    fn collect_hrefs(inlines: &[Inline], out: &mut Vec<Option<String>>) {
        for inline in inlines {
            match inline {
                Inline::Strong(children)
                | Inline::Emphasis(children)
                | Inline::Strike(children) => collect_hrefs(children, out),
                Inline::Link { href, children } => {
                    out.push(href.clone());
                    collect_hrefs(children, out);
                }
                Inline::Text(_)
                | Inline::Code(_)
                | Inline::Html(_)
                | Inline::SoftBreak
                | Inline::HardBreak
                | Inline::Image { .. } => {}
            }
        }
    }

    fn prose_markup(blocks: &[Block]) -> String {
        let mut out = String::new();
        for block in blocks {
            append_block_markup(block, &mut out);
        }
        out
    }

    fn append_block_markup(block: &Block, out: &mut String) {
        match block {
            Block::Paragraph(inlines) | Block::Heading { inlines, .. } => {
                out.push_str(&inline_markup(inlines));
            }
            Block::Quote(blocks) => {
                for child in blocks {
                    append_block_markup(child, out);
                }
            }
            Block::List { items, .. } => {
                for item in items {
                    for child in &item.blocks {
                        append_block_markup(child, out);
                    }
                }
            }
            Block::Table { headers, rows, .. } => {
                for row in headers.iter().chain(rows) {
                    for cell in row {
                        out.push_str(&inline_markup(cell));
                    }
                }
            }
            Block::Html(text) => out.push_str(&inline_markup(&[Inline::Html(text.clone())])),
            Block::Code { .. } | Block::Rule => {}
        }
    }

    #[test]
    fn unsafe_link_schemes_stay_inert() {
        let rejected = [
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            " javascript:alert(1)",
            "javascript:alert(1) ",
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "FILE:///etc/passwd",
            "file://localhost/etc/passwd",
            "vbscript:msgbox(1)",
            "ftp://example.com/file",
            "ws://example.com",
            "wss://example.com",
            "sms:+15551212",
            "tel:+15551212",
            "intent:open#Intent;end",
            "about:blank",
            "blob:https://example.com/uuid",
            "ms-msdt:id",
            "//example.com",
            "/etc/passwd",
            "#fragment",
            "https://example.com\\\"onclick=\\\"alert(1)",
            "https://example.com/<script>",
            "https://example.com\u{202e}.evil.com",
            "https://example.com\\@evil.com",
            "",
        ];
        for dest in rejected {
            assert!(safe_href(dest).is_none(), "{dest:?} must not launch");
        }

        let allowed = [
            ("https://example.com", "https://example.com/"),
            ("HTTPS://example.com", "https://example.com/"),
            ("http://example.com/a", "http://example.com/a"),
            ("mailto:user@example.com", "mailto:user@example.com"),
            ("MAILTO:user@example.com", "mailto:user@example.com"),
            ("https://[::1]/x", "https://[::1]/x"),
        ];
        for (raw, expected) in allowed {
            assert_eq!(safe_href(raw).as_deref(), Some(expected), "{raw}");
        }
        let query = safe_href("https://example.com/?a=1&b=2").expect("https query");
        assert!(query.contains('&'));
        assert!(!query.contains("&amp;"));

        let source = "\
[js](javascript:alert(1))
[js2](JAVASCRIPT:alert(1))
[data](data:text/html,hi)
[file](file:///etc/passwd)
[ftp](ftp://example.com)
[rel](//example.com)
[break](https://example.com/\"onclick=\"alert(1))
[good](https://example.com)
[mail](mailto:user@example.com)
[refbad][evil]
[refgood][ok]

[evil]: javascript:alert(1)
[ok]: https://example.com/ok
";
        let blocks = parse(source);
        let found = hrefs(&blocks);
        assert!(
            found
                .iter()
                .any(|href| href.as_deref() == Some("https://example.com/"))
        );
        assert!(
            found
                .iter()
                .any(|href| href.as_deref() == Some("mailto:user@example.com"))
        );
        assert!(
            found
                .iter()
                .any(|href| href.as_deref() == Some("https://example.com/ok"))
        );
        assert!(found.iter().filter(|href| href.is_none()).count() >= 6);
        assert!(found.iter().flatten().all(|href| {
            let scheme = href.split(':').next().unwrap_or("");
            matches!(scheme, "http" | "https" | "mailto")
        }));
        let markup = prose_markup(&blocks);
        assert!(!markup.to_ascii_lowercase().contains("href=\"javascript"));
        assert!(!markup.to_ascii_lowercase().contains("href=\"file"));
        assert!(!markup.to_ascii_lowercase().contains("href=\"data"));
        assert!(markup.contains("href=\"https://example.com/\""));
        let query_markup = prose_markup(&parse("[q](https://example.com/?a=1&b=2)"));
        assert!(query_markup.contains("href=\"https://example.com/?a=1&amp;b=2\""));
        assert!(!query_markup.contains("a=1&b=2"));
    }

    #[test]
    fn literal_pango_cannot_break_out() {
        let source = "See <span foreground=\"red\">x</span> & \"quotes\" `<b>` [</a><a href=\"javascript:alert(1)\">](https://example.com)";
        let blocks = parse(source);
        let markup = prose_markup(&blocks);
        assert!(markup.contains("&lt;span"));
        assert!(markup.contains("&amp;"));
        assert!(markup.contains("&quot;"));
        assert!(markup.contains("<tt>&lt;b&gt;</tt>"));
        assert!(markup.contains("&lt;/a&gt;"));
        assert!(!markup.contains("<span"));
        assert!(!markup.contains("<b>"));
        assert!(!markup.contains("href=\"javascript"));
        assert_eq!(markup.matches("<a ").count(), 1);
        assert!(markup.contains("href=\"https://example.com/\""));

        let image = parse("![<img src=x onerror=alert(1)>](javascript:alert(1))");
        let image_markup = prose_markup(&image);
        assert!(image_markup.contains("&lt;img"));
        assert!(!image_markup.contains("<img"));
        assert!(!image_markup.contains("javascript"));
        assert!(!image_markup.contains("http"));
        assert!(hrefs(&image).iter().all(Option::is_none));

        let html = parse("<script>alert(1)</script>\n\n[stay](https://example.com)");
        assert!(matches!(html.first(), Some(Block::Html(text)) if text.contains("<script>")));
        let html_markup = prose_markup(&html);
        assert!(html_markup.contains("&lt;script&gt;"));
        assert!(!html_markup.contains("<script"));
        assert!(html_markup.contains("href=\"https://example.com/\""));
    }

    #[test]
    fn nested_lists_quotes_and_tables_keep_boundaries() {
        // Blank line: a non-1 ordered marker cannot interrupt a paragraph.
        let lists = parse(
            "\
1. outer
   - inner

     3. deep
2. next

gap

3. third

- [x] done
  - [ ] open
- [ ] later
",
        );
        match &lists[0] {
            Block::List {
                ordered: true,
                start: 1,
                items,
            } => {
                assert_eq!(items.len(), 2);
                assert!(items[0].task.is_none());
                match &items[0].blocks[1] {
                    Block::List {
                        ordered: false,
                        items: inner,
                        ..
                    } => {
                        assert_eq!(inner.len(), 1);
                        match &inner[0].blocks[1] {
                            Block::List {
                                ordered: true,
                                start: 3,
                                items: deep,
                            } => {
                                assert_eq!(deep.len(), 1);
                                assert_eq!(plain_text(paragraph(&deep[0].blocks[0])), "deep");
                            }
                            other => panic!("expected numbered nested list, got {other:?}"),
                        }
                    }
                    other => panic!("expected unordered child, got {other:?}"),
                }
                assert_eq!(plain_text(paragraph(&items[1].blocks[0])), "next");
            }
            other => panic!("expected outer list, got {other:?}"),
        }
        match &lists[2] {
            Block::List {
                ordered: true,
                start: 3,
                items,
            } => {
                assert_eq!(items.len(), 1);
                assert_eq!(plain_text(paragraph(&items[0].blocks[0])), "third");
            }
            other => panic!("expected start-at-3 list, got {other:?}"),
        }
        match &lists[3] {
            Block::List {
                ordered: false,
                items,
                ..
            } => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].task, Some(true));
                assert_eq!(items[1].task, Some(false));
                match &items[0].blocks[1] {
                    Block::List { items: child, .. } => assert_eq!(child[0].task, Some(false)),
                    other => panic!("expected nested task, got {other:?}"),
                }
            }
            other => panic!("expected task list, got {other:?}"),
        }

        let quoted = parse("> 1. one\n>    - two\n> 2. three\n");
        match &quoted[0] {
            Block::Quote(blocks) => match &blocks[0] {
                Block::List {
                    ordered: true,
                    items,
                    ..
                } => {
                    assert_eq!(items.len(), 2);
                    assert!(matches!(
                        items[0].blocks[1],
                        Block::List { ordered: false, .. }
                    ));
                }
                other => panic!("expected list inside quote, got {other:?}"),
            },
            other => panic!("expected quote, got {other:?}"),
        }

        let table = parse(
            "\
| Left | Mid | Right |
| :--- | :---: | ---: |
| **a** |  | [bad](javascript:alert(1)) |
| `1. not a list` | ![pic](file:///etc/passwd) | [ok](https://example.com) |
",
        );
        match &table[0] {
            Block::Table {
                aligns,
                headers,
                rows,
            } => {
                assert_eq!(aligns, &[ColAlign::Left, ColAlign::Center, ColAlign::Right]);
                assert_eq!(headers.len(), 1);
                assert_eq!(headers[0].len(), 3);
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].len(), 3, "empty cell must not shift columns");
                assert!(rows[0][1].is_empty());
                assert!(matches!(rows[0][0].as_slice(), [Inline::Strong(_)]));
                assert_eq!(
                    rows[0][2][0],
                    Inline::Link {
                        href: None,
                        children: vec![Inline::Text("bad".to_string())],
                    }
                );
                assert!(matches!(&rows[1][0][0], Inline::Code(text) if text == "1. not a list"));
                assert_eq!(
                    rows[1][1][0],
                    Inline::Image {
                        alt: "pic".to_string()
                    }
                );
                assert!(
                    matches!(&rows[1][2][0], Inline::Link { href: Some(href), .. } if href == "https://example.com/")
                );
            }
            other => panic!("expected table, got {other:?}"),
        }
        let table_markup = prose_markup(&table);
        assert!(!table_markup.contains("file:"));
        assert!(!table_markup.contains("javascript"));
        assert!(table_markup.contains("href=\"https://example.com/\""));

        let quoted_table = parse("> | a | b |\n> | --- | --- |\n> | 1 | 2 |\n");
        match &quoted_table[0] {
            Block::Quote(blocks) => {
                assert!(matches!(blocks[0], Block::Table { .. }));
            }
            other => panic!("expected quoted table, got {other:?}"),
        }

        let code = parse("````html\n<table><b>\n\n| a | b |\n````\n");
        match &code[0] {
            Block::Code { language, code } => {
                assert_eq!(language, "html");
                assert_eq!(code, "<table><b>\n\n| a | b |\n");
            }
            other => panic!("fence must stay a code block, got {other:?}"),
        }

        let listed = parse("1. before\n\n   ```\n   <b>raw</b>\n   ```\n\n2. after\n");
        match &listed[0] {
            Block::List { items, .. } => {
                assert_eq!(items.len(), 2);
                assert!(items[0].blocks.iter().any(|block| matches!(
                    block,
                    Block::Code { code, .. } if code.contains("<b>raw</b>") && !code.contains("&lt;")
                )));
                assert_eq!(plain_text(paragraph(&items[1].blocks[0])), "after");
            }
            other => panic!("expected list around code, got {other:?}"),
        }
    }

    #[test]
    fn tight_list_keeps_inline_styles_links_images_and_code_together() {
        let blocks = parse(
            "- before **bold** and [safe](https://example.com) ![picture](file:///ignored) `code` after\n",
        );
        let Block::List { items, .. } = &blocks[0] else {
            panic!("expected list");
        };
        assert_eq!(items[0].blocks.len(), 1);
        let markup = prose_markup(&items[0].blocks);
        assert!(markup.contains("<b>bold</b>"));
        assert!(markup.contains("href=\"https://example.com/\""));
        assert!(markup.contains("picture"));
        assert!(markup.contains("<tt>code</tt>"));
        assert_eq!(
            plain_text(paragraph(&items[0].blocks[0])),
            "before bold and safe picture code after"
        );
    }

    #[test]
    fn depth_limit_preserves_text_order_inside_an_open_paragraph() {
        let source = format!("{}before **middle** after\n", "> ".repeat(MAX_DEPTH - 2));
        assert_eq!(prose_markup(&parse(&source)), "before middle after");
    }

    fn paragraph(block: &Block) -> &[Inline] {
        match block {
            Block::Paragraph(inlines) => inlines,
            other => panic!("expected paragraph, got {other:?}"),
        }
    }
}
