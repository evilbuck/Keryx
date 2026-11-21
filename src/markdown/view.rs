//! Paint a parsed Markdown tree with GPUI elements.

use gpui::{AnyElement, ClipboardItem, FontWeight, MouseButton, div, prelude::*, px, rgb};

use super::{Block, ColAlign, Inline, ListItem, parse};

pub fn column(source: &str, prefix: &str) -> AnyElement {
    blocks(source, prefix).into_any_element()
}

fn blocks(source: &str, prefix: &str) -> gpui::Div {
    let mut column = div().flex().flex_col().gap(px(6.)).w_full();
    for (index, block) in parse(source).iter().enumerate() {
        column = column.child(block_el(block, &format!("{prefix}-{index}")));
    }
    column
}

fn block_el(block: &Block, id: &str) -> AnyElement {
    match block {
        Block::Paragraph(inlines) => runs(inlines, id, 15.).into_any_element(),
        Block::Heading { level, inlines } => heading(*level, inlines, id).into_any_element(),
        Block::Quote(children) => quote(children, id).into_any_element(),
        Block::List {
            ordered,
            start,
            items,
        } => list(*ordered, *start, items, id).into_any_element(),
        Block::Code { language, code } => code_block(language, code, id).into_any_element(),
        Block::Rule => rule().into_any_element(),
        Block::Table {
            aligns,
            headers,
            rows,
        } => table(aligns, headers, rows, id).into_any_element(),
        Block::Html(text) => html(text).into_any_element(),
    }
}

fn heading(level: u8, inlines: &[Inline], id: &str) -> gpui::Div {
    runs(inlines, id, heading_size(level))
        .font_weight(FontWeight(700.))
        .mt(px(4.))
}

fn heading_size(level: u8) -> f32 {
    match level {
        1 => 26.,
        2 => 22.,
        3 => 19.,
        4 => 17.,
        _ => 15.,
    }
}

fn quote(children: &[Block], id: &str) -> gpui::Div {
    div()
        .flex()
        .gap(px(8.))
        .w_full()
        .child(div().w(px(3.)).bg(rgb(0x5b8def)))
        .child(blocks_of(children, id).flex_1())
}

fn blocks_of(children: &[Block], prefix: &str) -> gpui::Div {
    let mut column = div().flex().flex_col().gap(px(4.)).w_full();
    for (index, block) in children.iter().enumerate() {
        column = column.child(block_el(block, &format!("{prefix}-q{index}")));
    }
    column
}

fn list(ordered: bool, start: u64, items: &[ListItem], id: &str) -> gpui::Div {
    let mut column = div().flex().flex_col().gap(px(4.)).w_full();
    let mut number = start;
    for (index, item) in items.iter().enumerate() {
        column = column.child(item_row(item, ordered, number, &format!("{id}-i{index}")));
        number = number.saturating_add(1);
    }
    column
}

fn item_row(item: &ListItem, ordered: bool, number: u64, id: &str) -> gpui::Div {
    div()
        .flex()
        .gap(px(8.))
        .items_start()
        .w_full()
        .child(marker(item.task, ordered, number))
        .child(blocks_of(&item.blocks, id).flex_1())
}

fn marker(task: Option<bool>, ordered: bool, number: u64) -> gpui::Div {
    div()
        .w(px(28.))
        .font_weight(FontWeight(700.))
        .text_color(rgb(0xb0b0b8))
        .child(marker_text(task, ordered, number))
}

fn marker_text(task: Option<bool>, ordered: bool, number: u64) -> String {
    match task {
        Some(true) => "[x]".into(),
        Some(false) => "[ ]".into(),
        None if ordered => format!("{number}."),
        None => "•".into(),
    }
}

fn code_block(language: &str, code: &str, id: &str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .bg(rgb(0x242428))
        .rounded(px(8.))
        .w_full()
        .child(code_header(
            language,
            format!("{id}-copy"),
            code.to_string(),
        ))
        .child(
            div()
                .px(px(8.))
                .py(px(6.))
                .font_family("monospace")
                .text_size(px(13.))
                .child(code.to_string()),
        )
}

fn code_header(language: &str, id: String, copied: String) -> gpui::Div {
    div()
        .flex()
        .justify_between()
        .items_center()
        .px(px(8.))
        .py(px(4.))
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(0x9a9aa3))
                .child(code_label(language)),
        )
        .child(copy_button(id, copied))
}

fn code_label(language: &str) -> String {
    if language.is_empty() {
        "code".into()
    } else {
        language.to_string()
    }
}

fn copy_button(id: String, copied: String) -> gpui::AnyElement {
    div()
        .id(gpui::SharedString::new(id))
        .px(px(8.))
        .py(px(2.))
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(12.))
        .child("Copy code")
        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
        })
        .into_any_element()
}

fn rule() -> gpui::Div {
    div().h(px(1.)).w_full().bg(rgb(0x3a3a42)).my(px(8.))
}

fn html(text: &str) -> gpui::Div {
    div()
        .font_family("monospace")
        .text_size(px(13.))
        .text_color(rgb(0xc8c8d0))
        .child(text.to_string())
}

fn table(
    aligns: &[ColAlign],
    headers: &[Vec<Vec<Inline>>],
    rows: &[Vec<Vec<Inline>>],
    id: &str,
) -> gpui::Div {
    let mut column = div().flex().flex_col().w_full();
    for (index, header) in headers.iter().enumerate() {
        column = column.child(table_row(header, aligns, true, &format!("{id}-h{index}")));
    }
    for (index, row) in rows.iter().enumerate() {
        column = column.child(table_row(row, aligns, false, &format!("{id}-r{index}")));
    }
    div().w_full().child(column)
}

fn table_row(cells: &[Vec<Inline>], aligns: &[ColAlign], header: bool, id: &str) -> gpui::Div {
    let mut row = div().flex().w_full();
    if header {
        row = row.bg(rgb(0x2c2c32));
    }
    for (index, cell) in cells.iter().enumerate() {
        let align = aligns.get(index).copied().unwrap_or(ColAlign::Left);
        row = row.child(cell_el(cell, align, header, &format!("{id}-c{index}")));
    }
    div()
        .flex()
        .flex_col()
        .w_full()
        .child(row)
        .child(div().h(px(1.)).w_full().bg(rgb(0x3a3a42)))
}

fn cell_el(cell: &[Inline], align: ColAlign, header: bool, id: &str) -> gpui::Div {
    let mut el = runs(cell, id, 14.).w(px(140.)).px(px(8.)).py(px(6.));
    if header {
        el = el.font_weight(FontWeight(700.));
    }
    align_cell(el, align)
}

fn align_cell(el: gpui::Div, align: ColAlign) -> gpui::Div {
    match align {
        ColAlign::Center => el.flex().justify_center(),
        ColAlign::Right => el.flex().justify_end(),
        ColAlign::Left => el,
    }
}

fn runs(inlines: &[Inline], prefix: &str, size: f32) -> gpui::Div {
    let mut row = div().flex().flex_wrap().w_full();
    let mut link_index = 0usize;
    for run in flatten(inlines) {
        row = paint_run(row, &run, prefix, size, &mut link_index);
    }
    row
}

fn paint_run(
    row: gpui::Div,
    run: &Run,
    prefix: &str,
    size: f32,
    link_index: &mut usize,
) -> gpui::Div {
    if run.hard {
        return row.child(div().w_full().h(px(4.)));
    }
    let mut next = row;
    for piece in pieces(&run.text) {
        let id = link_id(prefix, &run.style, link_index);
        next = next.child(styled_piece(id, &piece, &run.style, size));
    }
    next
}

fn link_id(prefix: &str, style: &Style, link_index: &mut usize) -> Option<String> {
    style.href.as_ref()?;
    let id = *link_index;
    *link_index = link_index.saturating_add(1);
    Some(format!("{prefix}-link-{id}"))
}

fn styled_piece(id: Option<String>, text: &str, style: &Style, size: f32) -> gpui::AnyElement {
    let el = decorate(
        div()
            .text_size(px(size))
            .text_color(rgb(0xeeeeee))
            .child(text.to_string()),
        style,
    );
    let Some(id) = id else {
        return el.into_any_element();
    };
    let Some(href) = style.href.clone() else {
        return el.into_any_element();
    };
    el.id(gpui::SharedString::new(id))
        .text_color(rgb(0x7ab0ff))
        .underline()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
            cx.open_url(&href);
        })
        .into_any_element()
}

fn decorate(mut el: gpui::Div, style: &Style) -> gpui::Div {
    if style.bold {
        el = el.font_weight(FontWeight(700.));
    }
    if style.italic {
        el = el.italic();
    }
    if style.strike {
        el = el.line_through();
    }
    if style.code {
        el = el
            .font_family("monospace")
            .px(px(3.))
            .rounded(px(3.))
            .bg(rgb(0x2e2e34));
    }
    el
}

#[derive(Clone)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    href: Option<String>,
}

impl Style {
    fn plain() -> Self {
        Self {
            bold: false,
            italic: false,
            strike: false,
            code: false,
            href: None,
        }
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn italic_on(mut self) -> Self {
        self.italic = true;
        self
    }

    fn strike_on(mut self) -> Self {
        self.strike = true;
        self
    }

    fn link(mut self, href: Option<String>) -> Self {
        self.href = href;
        self
    }
}

struct Run {
    text: String,
    style: Style,
    hard: bool,
}

impl Run {
    fn text(text: &str, style: Style) -> Self {
        Self {
            text: text.to_string(),
            style,
            hard: false,
        }
    }

    fn hard() -> Self {
        Self {
            text: String::new(),
            style: Style::plain(),
            hard: true,
        }
    }

    fn code(text: &str, style: &Style) -> Self {
        let mut style = style.clone();
        style.code = true;
        Self {
            text: text.to_string(),
            style,
            hard: false,
        }
    }
}

fn flatten(inlines: &[Inline]) -> Vec<Run> {
    let mut out = Vec::new();
    push_inlines(&mut out, inlines, Style::plain());
    out
}

fn push_inlines(out: &mut Vec<Run>, inlines: &[Inline], style: Style) {
    for inline in inlines {
        push_inline(out, inline, &style);
    }
}

fn push_inline(out: &mut Vec<Run>, inline: &Inline, style: &Style) {
    if push_leaf(out, inline, style) {
        return;
    }
    push_nested(out, inline, style);
}

fn push_leaf(out: &mut Vec<Run>, inline: &Inline, style: &Style) -> bool {
    match inline {
        Inline::Text(text) => out.push(Run::text(text, style.clone())),
        Inline::SoftBreak => out.push(Run::text(" ", style.clone())),
        Inline::HardBreak => out.push(Run::hard()),
        Inline::Code(text) => out.push(Run::code(text, style)),
        Inline::Html(text) => out.push(Run::text(text, style.clone())),
        Inline::Image { alt } => out.push(Run::text(alt, style.clone().italic_on())),
        Inline::Strong(_) | Inline::Emphasis(_) | Inline::Strike(_) | Inline::Link { .. } => {
            return false;
        }
    }
    true
}

fn push_nested(out: &mut Vec<Run>, inline: &Inline, style: &Style) {
    match inline {
        Inline::Strong(children) => push_inlines(out, children, style.clone().bold()),
        Inline::Emphasis(children) => push_inlines(out, children, style.clone().italic_on()),
        Inline::Strike(children) => push_inlines(out, children, style.clone().strike_on()),
        Inline::Link { href, children } => {
            push_inlines(out, children, style.clone().link(href.clone()));
        }
        _ => {}
    }
}

fn pieces(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if ch == ' ' {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_code_keeps_a_parent_link() {
        let blocks = parse("[`docs`](https://example.com)");
        let Block::Paragraph(inlines) = &blocks[0] else {
            panic!("paragraph");
        };
        let runs = flatten(inlines);
        assert!(runs.iter().any(|run| {
            run.style.code && run.style.href.as_deref() == Some("https://example.com/")
        }));
    }
}
