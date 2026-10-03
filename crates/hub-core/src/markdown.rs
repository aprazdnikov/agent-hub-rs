//! Agent Markdown → the HTML subset Telegram accepts (`parse_mode=HTML`).
//!
//! Telegram supports only inline tags plus <pre>/<blockquote>, so block structure is expressed
//! with text: headings become bold, list items get bullet markers, tables become key-value
//! lines or per-row cards.

use std::collections::VecDeque;
use std::iter;
use std::num::NonZeroUsize;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};

use crate::escape::{escape, plain};
use crate::render::{char_len, split_message};

// Telegram trims plain leading spaces.
const INDENT: &str = "\u{a0}\u{a0}";
const BLOCK_SEPARATOR: &str = "\n\n";
const RULE: &str = "──────────";
// Relative or exotic links make Telegram reject the whole message.
const LINK_SCHEMES: [&str; 4] = ["http://", "https://", "mailto:", "tg://"];

enum Inline {
    Text(String),
    Code(String),
    Break,
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strike(Vec<Inline>),
    Link { href: String, children: Vec<Inline> },
    Image { src: String, children: Vec<Inline> },
    Span(Vec<Inline>),
}

enum Block {
    Paragraph(Vec<Inline>),
    Heading(Vec<Inline>),
    Code { info: String, text: String },
    Quote(Vec<Block>),
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Table { head: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
    Rule,
}

struct Table {
    pieces: Vec<String>,
    separator: &'static str,
}

impl Table {
    fn joined(&self) -> String {
        self.pieces.join(self.separator)
    }
}

/// Render `markdown` as Telegram HTML messages, each at most `limit` characters.
#[must_use]
pub fn markdown_to_html_chunks(markdown: &str, limit: NonZeroUsize) -> Vec<String> {
    let mut events =
        Parser::new_ext(markdown, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH);
    let pieces =
        blocks(&mut events, 0).iter().flat_map(|block| top_block(block, limit)).collect::<Vec<_>>();
    pack(pieces, limit.get(), BLOCK_SEPARATOR)
}

// --- events → tree --------------------------------------------------------------------------

// Tree building and rendering recurse per nesting level; deeper input would overflow the stack
// and abort the process. Same bound as markdown-it's `maxNesting`.
const MAX_NESTING: usize = 100;

/// Reads blocks until the enclosing `End` (consumed) or the end of input.
fn blocks(events: &mut Parser<'_>, depth: usize) -> Vec<Block> {
    let mut out = Vec::new();
    // Tight list items carry inline content without a paragraph around it.
    let mut loose = Vec::new();
    let inner = depth + 1;
    while let Some(event) = events.next() {
        let block = match event {
            Event::End(_) => break,
            Event::Start(_) if inner > MAX_NESTING => {
                loose.push(Inline::Text(flatten(events)));
                continue;
            }
            Event::Start(Tag::Paragraph) => Block::Paragraph(inlines(events, inner)),
            Event::Start(Tag::Heading { .. }) => Block::Heading(inlines(events, inner)),
            Event::Start(Tag::BlockQuote(_)) => Block::Quote(blocks(events, inner)),
            Event::Start(Tag::CodeBlock(kind)) => {
                Block::Code { info: code_info(kind), text: text(events) }
            }
            // Raw HTML is shown as text: Telegram would reject or misread it.
            Event::Start(Tag::HtmlBlock) => {
                Block::Paragraph(vec![Inline::Text(text(events).trim_end_matches('\n').to_owned())])
            }
            Event::Start(Tag::List(start)) => Block::List { start, items: items(events, inner) },
            Event::Start(Tag::Table(_)) => table(events, inner),
            Event::Rule => Block::Rule,
            other => {
                loose.extend(inline(other, events, depth));
                continue;
            }
        };
        flush(&mut loose, &mut out);
        out.push(block);
    }
    flush(&mut loose, &mut out);
    out
}

fn flush(loose: &mut Vec<Inline>, out: &mut Vec<Block>) {
    if !loose.is_empty() {
        out.push(Block::Paragraph(std::mem::take(loose)));
    }
}

fn inlines(events: &mut Parser<'_>, depth: usize) -> Vec<Inline> {
    let mut out = Vec::new();
    while let Some(event) = events.next() {
        if matches!(event, Event::End(_)) {
            break;
        }
        out.extend(inline(event, events, depth));
    }
    out
}

fn inline(event: Event<'_>, events: &mut Parser<'_>, depth: usize) -> Option<Inline> {
    match event {
        Event::Start(_) if depth + 1 > MAX_NESTING => Some(Inline::Text(flatten(events))),
        Event::Start(tag) => Some(inline_tag(tag, events, depth + 1)),
        Event::Text(text)
        | Event::Html(text)
        | Event::InlineHtml(text)
        | Event::InlineMath(text)
        | Event::DisplayMath(text) => Some(Inline::Text(text.into_string())),
        Event::Code(code) => Some(Inline::Code(code.into_string())),
        Event::SoftBreak | Event::HardBreak => Some(Inline::Break),
        Event::FootnoteReference(name) => Some(Inline::Text(format!("[^{name}]"))),
        Event::TaskListMarker(done) => {
            Some(Inline::Text(if done { "☑ " } else { "☐ " }.to_owned()))
        }
        Event::Rule | Event::End(_) => None,
    }
}

/// `depth` is the depth of the tag's own children.
fn inline_tag(tag: Tag<'_>, events: &mut Parser<'_>, depth: usize) -> Inline {
    match tag {
        Tag::Strong => Inline::Strong(inlines(events, depth)),
        Tag::Emphasis => Inline::Emphasis(inlines(events, depth)),
        Tag::Strikethrough => Inline::Strike(inlines(events, depth)),
        Tag::Link { dest_url, .. } => {
            Inline::Link { href: dest_url.into_string(), children: inlines(events, depth) }
        }
        Tag::Image { dest_url, .. } => {
            Inline::Image { src: dest_url.into_string(), children: inlines(events, depth) }
        }
        // Extensions that are not enabled; keep their text.
        _ => Inline::Span(inlines(events, depth)),
    }
}

/// Text of a subtree too deep to keep its structure, read without recursion; the `Start`
/// has already been consumed.
fn flatten(events: &mut Parser<'_>) -> String {
    let mut out = String::new();
    let mut open: usize = 1;
    for event in events.by_ref() {
        match event {
            Event::Start(_) => open += 1,
            Event::End(_) => {
                open -= 1;
                if open == 0 {
                    break;
                }
            }
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => out.push_str(&text),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::FootnoteReference(_) | Event::TaskListMarker(_) | Event::Rule => {}
        }
    }
    out
}

fn text(events: &mut Parser<'_>) -> String {
    let mut out = String::new();
    for event in events.by_ref() {
        match event {
            Event::End(_) => break,
            Event::Text(text) | Event::Html(text) => out.push_str(&text),
            _ => {}
        }
    }
    out
}

fn code_info(kind: CodeBlockKind<'_>) -> String {
    match kind {
        CodeBlockKind::Fenced(info) => info.into_string(),
        CodeBlockKind::Indented => String::new(),
    }
}

/// `depth` is the depth of the list items.
fn items(events: &mut Parser<'_>, depth: usize) -> Vec<Vec<Block>> {
    let mut out = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::Item) => out.push(blocks(events, depth)),
            Event::End(_) => break,
            _ => {}
        }
    }
    out
}

/// `depth` is the depth of the table cells.
fn table(events: &mut Parser<'_>, depth: usize) -> Block {
    let mut head = Vec::new();
    let mut rows = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::TableHead) => head = cells(events, depth),
            Event::Start(Tag::TableRow) => rows.push(cells(events, depth)),
            Event::End(_) => break,
            _ => {}
        }
    }
    Block::Table { head, rows }
}

fn cells(events: &mut Parser<'_>, depth: usize) -> Vec<Vec<Inline>> {
    let mut out = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::TableCell) => out.push(inlines(events, depth)),
            // Whether the head wraps its cells in a row differs between versions.
            Event::Start(Tag::TableRow) => out.extend(cells(events, depth)),
            Event::End(_) => break,
            _ => {}
        }
    }
    out
}

// --- tree → HTML ----------------------------------------------------------------------------

fn top_block(block: &Block, limit: NonZeroUsize) -> Vec<String> {
    let max = limit.get();
    if let Block::Table { head, rows } = block {
        let table = render_table(head, rows);
        return pack(table.pieces, max, table.separator)
            .into_iter()
            .flat_map(|piece| {
                if char_len(&piece) <= max {
                    vec![piece]
                } else {
                    split_escaped(&plain(&piece), limit)
                }
            })
            .collect();
    }
    let rendered = render_block(block, 0);
    if char_len(&rendered) <= max {
        return if rendered.is_empty() { Vec::new() } else { vec![rendered] };
    }
    if let Block::Code { info, text } = block {
        return split_code(text, info, limit);
    }
    split_escaped(&plain_block(block), limit)
}

fn pack(blocks: Vec<String>, limit: usize, separator: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for block in blocks {
        if current.is_empty() {
            current = block;
        } else if char_len(&current) + char_len(separator) + char_len(&block) <= limit {
            current.push_str(separator);
            current.push_str(&block);
        } else {
            chunks.push(std::mem::replace(&mut current, block));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn join(parts: impl Iterator<Item = String>, separator: &str) -> String {
    parts.collect::<Vec<_>>().join(separator)
}

fn render_block(block: &Block, depth: usize) -> String {
    match block {
        Block::Paragraph(content) => render_inlines(content),
        Block::Heading(content) => format!("<b>{}</b>", render_inlines(content)),
        Block::Code { info, text } => pre(text, info),
        Block::Quote(children) => {
            format!("<blockquote>{}</blockquote>", render_blocks(children, depth, "\n"))
        }
        Block::List { start, items } => render_list(*start, items, depth),
        Block::Table { head, rows } => render_table(head, rows).joined(),
        Block::Rule => RULE.to_owned(),
    }
}

fn render_blocks(blocks: &[Block], depth: usize, separator: &str) -> String {
    join(
        blocks
            .iter()
            .map(|block| render_block(block, depth))
            .filter(|rendered| !rendered.is_empty()),
        separator,
    )
}

fn render_list(start: Option<u64>, items: &[Vec<Block>], depth: usize) -> String {
    let indent = INDENT.repeat(depth);
    join(
        items.iter().zip(0_u64..).map(|(item, offset)| {
            let marker = start.map_or_else(
                || "•".to_owned(),
                |first| format!("{}.", first.saturating_add(offset)),
            );
            // Nested lists render their own deeper indentation.
            format!("{indent}{marker} {}", render_blocks(item, depth + 1, "\n"))
        }),
        "\n",
    )
}

fn render_table(head: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>]) -> Table {
    let header: Vec<String> = head.iter().map(|cell| render_inlines(cell)).collect();
    let rows = rows.iter().map(|row| {
        let mut cells: Vec<String> =
            row.iter().map(|cell| render_inlines(cell).trim().to_owned()).collect();
        cells.resize(header.len(), String::new());
        cells
    });
    match header.as_slice() {
        [] => Table { pieces: Vec::new(), separator: "\n" },
        [_] => Table {
            pieces: rows.map(|row| format!("• {}", cell(&row, 0))).collect(),
            separator: "\n",
        },
        [_, _] => Table {
            pieces: rows
                .map(|row| format!("<b>{}</b>: {}", cell(&row, 0), cell(&row, 1)))
                .collect(),
            separator: "\n",
        },
        [_, labels @ ..] => Table {
            pieces: rows.map(|row| card(labels, &row)).collect(),
            separator: BLOCK_SEPARATOR,
        },
    }
}

fn cell(row: &[String], index: usize) -> &str {
    row.get(index).map_or("", String::as_str)
}

/// Wide rows read as cards on a phone: first cell as title, the rest as labelled fields.
fn card(labels: &[String], row: &[String]) -> String {
    let empty: &[String] = &[];
    let (title, values) =
        row.split_first().map_or(("", empty), |(title, values)| (title.as_str(), values));
    let title = if title.is_empty() { "—" } else { title };
    let fields =
        labels.iter().zip(values).filter(|(_, value)| !value.is_empty()).map(|(label, value)| {
            if label.is_empty() {
                format!("{INDENT}{value}")
            } else {
                format!("{INDENT}{label}: {value}")
            }
        });
    join(iter::once(format!("<b>{title}</b>")).chain(fields), "\n")
}

fn render_inlines(content: &[Inline]) -> String {
    content.iter().map(render_inline).collect()
}

fn render_inline(inline: &Inline) -> String {
    match inline {
        Inline::Text(text) => escape(text),
        Inline::Code(code) => format!("<code>{}</code>", escape(code)),
        Inline::Break => "\n".to_owned(),
        Inline::Strong(children) => format!("<b>{}</b>", render_inlines(children)),
        Inline::Emphasis(children) => format!("<i>{}</i>", render_inlines(children)),
        Inline::Strike(children) => format!("<s>{}</s>", render_inlines(children)),
        Inline::Link { href, children } => link(href, &render_inlines(children)),
        Inline::Image { src, children } => escape(&format!("{} ({src})", plain_inlines(children))),
        Inline::Span(children) => render_inlines(children),
    }
}

fn link(href: &str, label: &str) -> String {
    if LINK_SCHEMES.iter().any(|scheme| href.starts_with(scheme)) {
        format!("<a href=\"{}\">{label}</a>", escape(href))
    } else if href.is_empty() {
        label.to_owned()
    } else {
        format!("{label} ({})", escape(href))
    }
}

fn pre(code: &str, info: &str) -> String {
    let body = escape(code.trim_end_matches('\n'));
    let language = info.split_whitespace().next().unwrap_or_default();
    if is_language(language) {
        format!("<pre><code class=\"language-{language}\">{body}</code></pre>")
    } else {
        format!("<pre>{body}</pre>")
    }
}

fn is_language(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "_#+.-".contains(c))
}

fn split_code(code: &str, info: &str, limit: NonZeroUsize) -> Vec<String> {
    let budget = limit.get().saturating_sub(char_len(&pre("", info)));
    let mut pieces = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut size = 0;
    for line in code.trim_end_matches('\n').split('\n') {
        let cost = char_len(&escape(line)) + 1;
        if !current.is_empty() && size + cost > budget {
            pieces.push(current.join("\n"));
            current.clear();
            size = 0;
        }
        current.push(line);
        size += cost;
    }
    if !current.is_empty() {
        pieces.push(current.join("\n"));
    }
    pieces
        .into_iter()
        .flat_map(|piece| {
            let rendered = pre(&piece, info);
            if char_len(&rendered) <= limit.get() {
                vec![rendered]
            } else {
                split_escaped(&piece, limit)
            }
        })
        .collect()
}

/// Split plain text so that each escaped chunk fits; never cuts an entity in half.
fn split_escaped(text: &str, limit: NonZeroUsize) -> Vec<String> {
    let mut result = Vec::new();
    let mut pending: VecDeque<String> = split_message(text, limit).into();
    while let Some(chunk) = pending.pop_front() {
        let escaped = escape(&chunk);
        // A single character cannot be split further, even if its entity exceeds the limit.
        if char_len(&escaped) <= limit.get() || char_len(&chunk) <= 1 {
            result.push(escaped);
            continue;
        }
        let half = NonZeroUsize::new(char_len(&chunk) / 2).unwrap_or(NonZeroUsize::MIN);
        for piece in split_message(&chunk, half).into_iter().rev() {
            pending.push_front(piece);
        }
    }
    result
}

fn plain_block(block: &Block) -> String {
    match block {
        Block::Paragraph(content) | Block::Heading(content) => plain_inlines(content),
        Block::Code { text, .. } => text.clone(),
        Block::Quote(children) => children.iter().map(plain_block).collect(),
        Block::List { items, .. } => items.iter().flatten().map(plain_block).collect(),
        Block::Table { head, rows } => {
            head.iter().chain(rows.iter().flatten()).map(|cell| plain_inlines(cell)).collect()
        }
        Block::Rule => String::new(),
    }
}

fn plain_inlines(content: &[Inline]) -> String {
    content.iter().map(plain_inline).collect()
}

fn plain_inline(inline: &Inline) -> String {
    match inline {
        Inline::Text(text) | Inline::Code(text) => text.clone(),
        Inline::Break => " ".to_owned(),
        Inline::Strong(children)
        | Inline::Emphasis(children)
        | Inline::Strike(children)
        | Inline::Span(children)
        | Inline::Link { children, .. }
        | Inline::Image { children, .. } => plain_inlines(children),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rstest::rstest;

    use super::*;
    use crate::escape::plain;
    use crate::render::{TELEGRAM_TEXT_LIMIT, char_len};

    const ALLOWED_TAGS: [&str; 7] = ["b", "i", "s", "code", "pre", "blockquote", "a"];

    fn limit(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    fn render(markdown: &str) -> String {
        let chunks = markdown_to_html_chunks(markdown, TELEGRAM_TEXT_LIMIT);
        assert_eq!(chunks.len(), 1, "{chunks:?}");
        chunks.into_iter().next().unwrap()
    }

    fn tags(html: &str) -> Vec<String> {
        html.split('<')
            .skip(1)
            .filter_map(|part| part.split_once('>').map(|(tag, _)| tag.to_owned()))
            .collect()
    }

    fn tag_name(tag: &str) -> &str {
        tag.trim_start_matches('/').split_whitespace().next().unwrap_or_default()
    }

    #[rstest]
    #[case("**bold** and *italic*", "<b>bold</b> and <i>italic</i>")]
    #[case("~~gone~~", "<s>gone</s>")]
    #[case("run `uv sync`", "run <code>uv sync</code>")]
    #[case("# Title", "<b>Title</b>")]
    #[case("a < b & c > d", "a &lt; b &amp; c &gt; d")]
    #[case("---", "──────────")]
    fn inline_and_simple_blocks(#[case] markdown: &str, #[case] expected: &str) {
        assert_eq!(render(markdown), expected);
    }

    #[test]
    fn raw_html_is_escaped() {
        assert_eq!(render("<script>x</script>"), "&lt;script&gt;x&lt;/script&gt;");
        assert_eq!(render("a <b>x</b>"), "a &lt;b&gt;x&lt;/b&gt;");
    }

    #[test]
    fn fenced_code_keeps_language_and_escapes() {
        assert_eq!(
            render("```python\nif a < b:\n    pass\n```"),
            "<pre><code class=\"language-python\">if a &lt; b:\n    pass</code></pre>"
        );
    }

    #[test]
    fn fence_without_language_is_plain_pre() {
        assert_eq!(render("```\nls\n```"), "<pre>ls</pre>");
    }

    #[test]
    fn lists_get_markers_and_nesting() {
        assert_eq!(
            render("- one\n- two\n  - nested\n\n3. three\n4. four"),
            "• one\n• two\n\u{a0}\u{a0}• nested\n\n3. three\n4. four"
        );
    }

    #[test]
    fn loose_list_renders_like_tight() {
        assert_eq!(render("- one\n\n- two"), "• one\n• two");
    }

    #[test]
    fn links_keep_safe_schemes_only() {
        assert_eq!(
            render("[docs](https://example.com/?a=1&b=2)"),
            "<a href=\"https://example.com/?a=1&amp;b=2\">docs</a>"
        );
        assert_eq!(render("[file](src/app.py)"), "file (src/app.py)");
    }

    #[test]
    fn dangerous_link_is_plain_text() {
        assert_eq!(render("[x](javascript:alert(1))"), "x (javascript:alert(1))");
    }

    #[test]
    fn two_column_table_becomes_key_value_lines() {
        assert_eq!(
            render("| Ключ | Значение |\n|---|---|\n| timeout | **30** с |\n| retries | 3 |"),
            "<b>timeout</b>: <b>30</b> с\n<b>retries</b>: 3"
        );
    }

    #[test]
    fn wide_table_becomes_one_card_per_row() {
        let markdown = "| Репозиторий | Язык | Тесты |\n|---|---|---|\n\
                        | agent-hub | `Python` | 42 |\n| skill-issue | Markdown |  |";
        assert_eq!(
            render(markdown),
            "<b>agent-hub</b>\n\u{a0}\u{a0}Язык: <code>Python</code>\n\u{a0}\u{a0}Тесты: 42\
             \n\n<b>skill-issue</b>\n\u{a0}\u{a0}Язык: Markdown"
        );
    }

    #[test]
    fn single_column_table_becomes_bullets() {
        assert_eq!(render("| Файл |\n|---|\n| a.py |\n| b.py |"), "• a.py\n• b.py");
    }

    #[test]
    fn table_cell_escapes_markup() {
        assert_eq!(render("| k | v |\n|---|---|\n| a<b | x & y |"), "<b>a&lt;b</b>: x &amp; y");
    }

    #[test]
    fn long_table_splits_between_cards() {
        let rows = (0..40)
            .map(|i| format!("| row{i} | {} | {} |", "x".repeat(40), "y".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks =
            markdown_to_html_chunks(&format!("| n | a | b |\n|---|---|---|\n{rows}"), limit(500));

        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(char_len(chunk) <= 500);
            assert!(chunk.starts_with("<b>row"));
            assert_eq!(chunk.matches("<b>").count(), chunk.matches("</b>").count());
        }
    }

    #[test]
    fn blockquote() {
        assert_eq!(render("> quoted **text**"), "<blockquote>quoted <b>text</b></blockquote>");
    }

    #[test]
    fn blocks_are_separated_by_blank_line() {
        assert_eq!(render("para one\n\npara two"), "para one\n\npara two");
    }

    #[test]
    fn long_text_is_split_into_valid_chunks() {
        let markdown = (0..60)
            .map(|i| format!("**{i}** {}", "word & ".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunks = markdown_to_html_chunks(&markdown, limit(500));

        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(char_len(chunk) <= 500);
            assert_eq!(chunk.matches("<b>").count(), chunk.matches("</b>").count());
        }
    }

    #[test]
    fn huge_code_block_is_split_into_several_pre() {
        let code = (0..200).map(|i| format!("line {i} <tag>")).collect::<Vec<_>>().join("\n");
        let chunks = markdown_to_html_chunks(&format!("```\n{code}\n```"), limit(400));

        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(char_len(chunk) <= 400);
            assert!(chunk.starts_with("<pre>"));
            assert!(chunk.ends_with("</pre>"));
        }
        assert_eq!(plain(&chunks.join("\n")), code);
    }

    #[test]
    fn oversized_paragraph_falls_back_to_escaped_text() {
        let chunks = markdown_to_html_chunks(&"&".repeat(300), limit(100));

        assert!(chunks.iter().all(|chunk| char_len(chunk) <= 100));
        assert_eq!(plain(&chunks.concat()).replace('\n', ""), "&".repeat(300));
    }

    #[test]
    fn single_char_over_limit_terminates() {
        let chunks = markdown_to_html_chunks("&&&", limit(2));
        assert_eq!(plain(&chunks.concat()).replace('\n', ""), "&&&");
    }

    #[test]
    fn only_telegram_tags_are_emitted() {
        let markdown = "# H\n\n**b** *i* ~~s~~ `c` [l](https://x.y)\n\n> q\n\n- a\n\n\
                        ```js\nx\n```\n\n|a|b|\n|-|-|\n|1|2|\n\n<div>raw</div>\n\n[j](javascript:x)";
        let html = render(markdown);
        let found = tags(&html);
        assert!(!found.is_empty());
        assert!(found.iter().all(|tag| ALLOWED_TAGS.contains(&tag_name(tag))), "{found:?}");
    }

    #[rstest]
    #[case("- ".repeat(5000) + "x")]
    #[case(">".repeat(5000) + " x")]
    #[case("**".repeat(5000) + "x")]
    #[case("*a ".repeat(5000) + "x")]
    #[case("[".repeat(5000) + "x")]
    fn deep_nesting_does_not_overflow_the_stack(#[case] markdown: String) {
        // 1 MiB is the main-thread stack on Windows, the smallest one this code may run on.
        let chunks = std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || markdown_to_html_chunks(&markdown, TELEGRAM_TEXT_LIMIT))
            .unwrap()
            .join()
            .unwrap();

        assert!(chunks.iter().all(|chunk| char_len(chunk) <= TELEGRAM_TEXT_LIMIT.get()));
        assert!(plain(&chunks.concat()).contains('x'));
    }

    #[test]
    fn empty_markdown_has_no_chunks() {
        assert!(markdown_to_html_chunks("   ", TELEGRAM_TEXT_LIMIT).is_empty());
    }
}
