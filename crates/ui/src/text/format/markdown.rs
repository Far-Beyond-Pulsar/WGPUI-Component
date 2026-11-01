use gpui::SharedString;
use markdown::{
    mdast::{self, Node},
    Constructs, ParseOptions,
};
#[cfg(not(target_family = "wasm"))]
use mathjax_svg_rs::{
    render_tex as render_mathjax_tex, HorizontalAlign, Options as MathJaxOptions,
};
#[cfg(not(target_family = "wasm"))]
use mermaid_rs_renderer::render as render_mermaid;
use once_cell::sync::Lazy;
use regex::Regex;
use std::{collections::HashMap, sync::Mutex};

use crate::{
    highlighter::HighlightTheme,
    text::{
        node::{
            self, CodeBlock, ImageNode, InlineNode, LinkMark, NodeContext, Paragraph, Span, Table,
            TableRow, TextMark,
        },
        TextViewStyle,
    },
};

static MATH_SVG_CACHE: Lazy<Mutex<HashMap<(bool, String), SharedString>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static MERMAID_SVG_CACHE: Lazy<Mutex<HashMap<String, SharedString>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn normalize_svg(svg: &str) -> String {
    static SVG_OPEN_TAG_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?s)^<svg\b([^>]*)>").expect("valid svg tag regex"));
    static WIDTH_EX_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\bwidth="([0-9]*\.?[0-9]+)ex""#).expect("valid width ex regex"));
    static HEIGHT_EX_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"\bheight="([0-9]*\.?[0-9]+)ex""#).expect("valid height ex regex")
    });
    static STYLE_ATTR_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\sstyle="[^"]*""#).expect("valid style attr regex"));
    static XLINK_HREF_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\bxlink:href="#).expect("valid xlink href regex"));

    let Some(captures) = SVG_OPEN_TAG_RE.captures(svg) else {
        return svg.to_string();
    };

    let mut attributes = captures
        .get(1)
        .map(|value| value.as_str())
        .unwrap_or("")
        .to_string();

    // GPUI's SVG decoding path does not consistently honor CSS `ex` lengths.
    // Convert them to explicit px dimensions derived from MathJax's 16px font size.
    attributes = WIDTH_EX_RE
        .replace_all(&attributes, |caps: &regex::Captures| {
            let ex = caps
                .get(1)
                .and_then(|v| v.as_str().parse::<f32>().ok())
                .unwrap_or(0.0);
            format!(r#"width="{:.3}px""#, ex * 8.0)
        })
        .into_owned();

    attributes = HEIGHT_EX_RE
        .replace_all(&attributes, |caps: &regex::Captures| {
            let ex = caps
                .get(1)
                .and_then(|v| v.as_str().parse::<f32>().ok())
                .unwrap_or(0.0);
            format!(r#"height="{:.3}px""#, ex * 8.0)
        })
        .into_owned();

    // `vertical-align` is an HTML/CSS concern; remove inline style to avoid parser quirks.
    let normalized_attributes = STYLE_ATTR_RE.replace_all(&attributes, "");
    let open_tag_end = captures.get(0).map(|value| value.end()).unwrap_or(0);

    let normalized = format!("<svg{}>{}", normalized_attributes, &svg[open_tag_end..]);

    // Some SVG renderers only honor `href` on <use>, not legacy `xlink:href`.
    XLINK_HREF_RE.replace_all(&normalized, "href=").into_owned()
}

#[cfg(not(target_family = "wasm"))]
fn render_math_svg(value: &str, display_mode: bool) -> Option<SharedString> {
    let cache_key = (display_mode, value.to_string());
    if let Some(cached) = MATH_SVG_CACHE.lock().ok()?.get(&cache_key).cloned() {
        return Some(cached);
    }

    let svg = render_mathjax_tex(
        value,
        &MathJaxOptions {
            font_size: 16.0,
            horizontal_align: HorizontalAlign::Center,
        },
    )
    .ok()?;

    let svg: SharedString = normalize_svg(&svg).into();

    if let Ok(mut cache) = MATH_SVG_CACHE.lock() {
        cache.insert(cache_key, svg.clone());
    }

    Some(svg)
}

#[cfg(not(target_family = "wasm"))]
fn render_mermaid_svg(value: &str) -> Option<SharedString> {
    if let Some(cached) = MERMAID_SVG_CACHE.lock().ok()?.get(value).cloned() {
        return Some(cached);
    }

    let svg: SharedString = normalize_svg(&render_mermaid(value).ok()?).into();

    if let Ok(mut cache) = MERMAID_SVG_CACHE.lock() {
        cache.insert(value.to_string(), svg.clone());
    }

    Some(svg)
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::{normalize_katex_default_delimiters, parse_paragraph, render_math_svg};
    use crate::text::{node::NodeContext, node::Paragraph};
    use markdown::{mdast, ParseOptions};

    fn parse_inline(source: &str) -> Paragraph {
        let ast = markdown::to_mdast(source, &ParseOptions::gfm()).expect("valid markdown");
        let mdast::Node::Root(root) = ast else {
            panic!("expected markdown root");
        };
        let mdast::Node::Paragraph(source_paragraph) = &root.children[0] else {
            panic!("expected markdown paragraph");
        };

        let mut paragraph = Paragraph::default();
        let mut cx = NodeContext::default();
        for child in &source_paragraph.children {
            parse_paragraph(&mut paragraph, child, &mut cx);
        }
        paragraph
    }

    #[test]
    fn triple_asterisks_preserve_bold_and_italic() {
        let paragraph = parse_inline("***bold italic***");
        assert_eq!(paragraph.text_len(), "bold italic".len());
        assert!(paragraph
            .children
            .iter()
            .all(|node| { node.marks.iter().any(|(_, mark)| mark.bold && mark.italic) }));
    }

    #[test]
    fn nested_emphasis_preserves_outer_and_inner_marks() {
        let paragraph = parse_inline("**bold _and italic_**");
        let italic = paragraph
            .children
            .iter()
            .find(|node| node.text.as_ref() == "and italic")
            .expect("italic text run");
        assert!(italic
            .marks
            .iter()
            .any(|(_, mark)| mark.bold && mark.italic));

        let plain_bold = paragraph
            .children
            .iter()
            .find(|node| node.text.as_ref() == "bold ")
            .expect("bold text run");
        assert!(plain_bold.marks.iter().any(|(_, mark)| mark.bold));
        assert!(!plain_bold.marks.iter().any(|(_, mark)| mark.italic));
    }

    #[test]
    fn emphasis_keeps_inline_code_mark() {
        let paragraph = parse_inline("**bold `code`**");
        let code = paragraph
            .children
            .iter()
            .find(|node| node.text.as_ref() == "code")
            .expect("code text run");
        assert!(code.marks.iter().any(|(_, mark)| mark.bold && mark.code));
    }

    #[test]
    fn math_delimiter_normalization_skips_inline_code_spans() {
        assert_eq!(
            normalize_katex_default_delimiters("`\\(code\\)` and \\(math\\)"),
            "`\\(code\\)` and $math$"
        );
        assert_eq!(
            normalize_katex_default_delimiters("`` `\\(code\\)` ``"),
            "`` `\\(code\\)` ``"
        );
    }

    #[test]
    fn math_delimiter_normalization_skips_tilde_and_long_fences() {
        let source =
            "\\(outside\\)\n~~~math\n\\(tilde code\\)\n~~~\n````text\n\\(long fence\\)\n````\n";
        let expected =
            "$outside$\n~~~math\n\\(tilde code\\)\n~~~\n````text\n\\(long fence\\)\n````\n";
        assert_eq!(normalize_katex_default_delimiters(source), expected);
    }

    #[test]
    fn print_math_svg_debug_output() {
        let inline = render_math_svg(r"x^2 + y^2", false).expect("inline svg");
        let block = render_math_svg(r"\\frac{a}{b}", true).expect("block svg");

        tracing::debug!("INLINE SVG:\n{}", inline);
        tracing::debug!("BLOCK SVG:\n{}", block);
    }
}

static BLOCK_DELIM_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\\\[(?s:(.*?))\\\]").expect("valid block math regex"));
static INLINE_DELIM_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\\\((.*?)\\\)").expect("valid inline math regex"));

fn normalize_katex_default_delimiters(raw: &str) -> String {
    fn normalize_text(segment: &str) -> String {
        // Convert KaTeX default delimiters to markdown-math delimiters.
        // \[...\] -> $$...$$, \(...\) -> $...$
        let with_blocks = BLOCK_DELIM_RE
            .replace_all(segment, |caps: &regex::Captures| {
                format!("$$\n{}\n$$", &caps[1])
            })
            .into_owned();

        INLINE_DELIM_RE
            .replace_all(&with_blocks, |caps: &regex::Captures| {
                format!("${}$", &caps[1])
            })
            .into_owned()
    }

    fn normalize_outside_code(segment: &str) -> String {
        let mut out = String::with_capacity(segment.len());
        let mut cursor = 0;

        while let Some(relative_start) = segment[cursor..].find('`') {
            let start = cursor + relative_start;
            out.push_str(&normalize_text(&segment[cursor..start]));

            let run_end = start
                + segment[start..]
                    .bytes()
                    .take_while(|byte| *byte == b'`')
                    .count();
            let run_len = run_end - start;

            // CommonMark code spans close with a backtick run of the same
            // length. Longer or shorter runs inside the span are content.
            let mut search = run_end;
            let mut closing = None;
            while let Some(relative_close) = segment[search..].find('`') {
                let close_start = search + relative_close;
                let close_end = close_start
                    + segment[close_start..]
                        .bytes()
                        .take_while(|byte| *byte == b'`')
                        .count();
                if close_end - close_start == run_len {
                    closing = Some(close_end);
                    break;
                }
                search = close_end;
            }

            out.push_str(&segment[start..closing.unwrap_or(run_end)]);
            cursor = closing.unwrap_or(run_end);
        }

        out.push_str(&normalize_text(&segment[cursor..]));
        out
    }

    fn fence_run(line: &str) -> Option<(u8, usize, usize)> {
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        if indent > 3 {
            return None;
        }
        let rest = &line[indent..];
        let marker = *rest.as_bytes().first()?;
        if marker != b'`' && marker != b'~' {
            return None;
        }
        let count = rest.bytes().take_while(|byte| *byte == marker).count();
        (count >= 3).then_some((marker, count, indent + count))
    }

    fn closes_fence(line: &str, marker: u8, min_len: usize) -> bool {
        let Some((found_marker, count, end)) = fence_run(line) else {
            return false;
        };
        found_marker == marker && count >= min_len && line[end..].trim().is_empty()
    }

    // Keep fenced blocks untouched, including tilde fences and fences longer
    // than three backticks. Normalize inline code spans separately below.
    let mut out = String::with_capacity(raw.len());
    let mut outside_code = String::new();
    let mut fence: Option<(u8, usize)> = None;

    for line in raw.split_inclusive('\n') {
        if let Some((marker, min_len)) = fence {
            out.push_str(line);
            if closes_fence(
                line.trim_end_matches(|c| c == '\r' || c == '\n'),
                marker,
                min_len,
            ) {
                fence = None;
            }
        } else if let Some((marker, count, _)) =
            fence_run(line.trim_end_matches(|c| c == '\r' || c == '\n'))
        {
            out.push_str(&normalize_outside_code(&outside_code));
            outside_code.clear();
            out.push_str(line);
            fence = Some((marker, count));
        } else {
            outside_code.push_str(line);
        }
    }

    out.push_str(&normalize_outside_code(&outside_code));
    out
}

/// Parse Markdown into a tree of nodes.
pub(crate) fn parse(
    raw: &str,
    style: &TextViewStyle,
    cx: &mut NodeContext,
    highlight_theme: &HighlightTheme,
) -> Result<node::Node, SharedString> {
    let normalized = normalize_katex_default_delimiters(raw);
    let parse_options = ParseOptions {
        constructs: Constructs {
            math_flow: true,
            math_text: true,
            ..Constructs::gfm()
        },
        ..ParseOptions::gfm()
    };

    markdown::to_mdast(&normalized, &parse_options)
        .map(|n| ast_to_node(n, style, cx, highlight_theme))
        .map_err(|e| e.to_string().into())
}

fn parse_table_row(table: &mut Table, node: &mdast::TableRow, cx: &mut NodeContext) {
    let mut row = TableRow::default();
    node.children.iter().for_each(|c| {
        match c {
            Node::TableCell(cell) => {
                parse_table_cell(&mut row, cell, cx);
            }
            _ => {}
        };
    });
    table.children.push(row);
}

fn parse_table_cell(row: &mut node::TableRow, node: &mdast::TableCell, cx: &mut NodeContext) {
    let mut paragraph = Paragraph::default();
    node.children.iter().for_each(|c| {
        parse_paragraph(&mut paragraph, c, cx);
    });
    let table_cell = node::TableCell {
        children: paragraph,
        ..Default::default()
    };
    row.children.push(table_cell);
}

fn parse_paragraph(paragraph: &mut Paragraph, node: &mdast::Node, cx: &mut NodeContext) -> String {
    let span = node.position().map(|pos| Span {
        start: pos.start.offset,
        end: pos.end.offset,
    });
    if let Some(span) = span {
        paragraph.set_span(span);
    }

    let mut text = String::new();

    match node {
        Node::Paragraph(val) => {
            val.children.iter().for_each(|c| {
                text.push_str(&parse_paragraph(paragraph, c, cx));
            });
        }
        Node::Text(val) => {
            text = val.value.clone();
            paragraph.push_str(&val.value)
        }
        Node::Emphasis(val) => {
            text = parse_children_with_mark(
                paragraph,
                &val.children,
                TextMark::default().italic(),
                cx,
            );
        }
        Node::Strong(val) => {
            text =
                parse_children_with_mark(paragraph, &val.children, TextMark::default().bold(), cx);
        }
        Node::Delete(val) => {
            text = parse_children_with_mark(
                paragraph,
                &val.children,
                TextMark::default().strikethrough(),
                cx,
            );
        }
        Node::InlineCode(val) => {
            text = val.value.clone();
            paragraph.push(
                InlineNode::new(&text).marks(vec![(0..text.len(), TextMark::default().code())]),
            );
        }
        Node::Link(val) => {
            let link_mark = Some(LinkMark {
                url: val.url.clone().into(),
                title: val.title.clone().map(|s| s.into()),
                ..Default::default()
            });

            let mut child_paragraph = Paragraph::default();
            for child in val.children.iter() {
                text.push_str(&parse_paragraph(&mut child_paragraph, &child, cx));
            }

            // FIXME: GPUI InteractiveText does not support inline images yet.
            // So here we push images to the paragraph directly.
            for child in child_paragraph.children.iter_mut() {
                if let Some(image) = child.image.as_mut() {
                    image.link = link_mark.clone();
                }

                child.marks.push((
                    0..child.text.len(),
                    TextMark {
                        link: link_mark.clone(),
                        ..Default::default()
                    },
                ));
            }

            paragraph.merge(child_paragraph);
        }
        Node::Image(raw) => {
            text = raw.alt.clone();
            paragraph.push_image(ImageNode {
                url: raw.url.clone().into(),
                title: raw.title.clone().map(|t| t.into()),
                alt: Some(raw.alt.clone().into()),
                ..Default::default()
            });
        }
        Node::InlineMath(raw) => {
            text = raw.value.clone();
            #[cfg(not(target_family = "wasm"))]
            if let Some(svg) = render_math_svg(&raw.value, false) {
                paragraph.push_image(ImageNode {
                    url: raw.value.clone().into(),
                    alt: Some(raw.value.clone().into()),
                    title: Some(raw.value.clone().into()),
                    math_tex: Some(raw.value.clone().into()),
                    math_svg: Some(svg),
                    math_display_mode: false,
                    ..Default::default()
                });
                return text;
            }
            paragraph.push(
                InlineNode::new(&text).marks(vec![(0..text.len(), TextMark::default().code())]),
            );
        }
        Node::MdxTextExpression(raw) => {
            text = raw.value.clone();
            paragraph
                .push(InlineNode::new(&text).marks(vec![(0..text.len(), TextMark::default())]));
        }
        Node::Html(val) => match super::html::parse(&val.value, cx) {
            Ok(el) => match el {
                node::Node::Break { .. } => {
                    text = "\n".to_owned();
                    paragraph.push(InlineNode::new(&text));
                }
                node::Node::Paragraph(html_paragraph) => {
                    text = html_paragraph
                        .children
                        .iter()
                        .map(|child| child.text.as_ref())
                        .collect();
                    paragraph.merge(html_paragraph);
                }
                other => {
                    if cfg!(debug_assertions) {
                        tracing::warn!("unsupported inline html tag: {:#?}", other);
                    }
                    text = val.value.clone();
                    paragraph.push_str(&text);
                }
            },
            Err(err) => {
                if cfg!(debug_assertions) {
                    tracing::warn!("failed parsing html: {:#?}", err);
                }

                text = val.value.clone();
                paragraph.push_str(&text);
            }
        },
        Node::FootnoteReference(foot) => {
            let prefix = format!("[{}]", foot.identifier);
            text = prefix.clone();
            paragraph.push(InlineNode::new(&prefix).marks(vec![(
                0..prefix.len(),
                TextMark {
                    italic: true,
                    ..Default::default()
                },
            )]));
        }
        Node::LinkReference(link) => {
            let mut child_paragraph = Paragraph::default();
            for child in link.children.iter() {
                text.push_str(&parse_paragraph(&mut child_paragraph, child, cx));
            }

            let link_mark = LinkMark {
                url: "".into(),
                title: link.label.clone().map(Into::into),
                identifier: Some(link.identifier.clone().into()),
            };

            for child in child_paragraph.children.iter_mut() {
                child.marks.push((
                    0..child.text.len(),
                    TextMark {
                        link: Some(link_mark.clone()),
                        ..Default::default()
                    },
                ));
            }
            paragraph.merge(child_paragraph);
        }
        _ => {
            if cfg!(debug_assertions) {
                tracing::warn!("unsupported inline node: {:#?}", node);
            }
        }
    }

    text
}

/// Parse a formatting node without flattening its children. Flattening loses
/// nested marks (`***bold italic***`) and inline objects such as code and links.
fn parse_children_with_mark(
    paragraph: &mut Paragraph,
    children: &[mdast::Node],
    mark: TextMark,
    cx: &mut NodeContext,
) -> String {
    let mut text = String::new();
    let mut parsed_children = Paragraph::default();
    for child in children {
        text.push_str(&parse_paragraph(&mut parsed_children, child, cx));
    }

    for mut child in parsed_children.children {
        let len = child.text.len();
        if len > 0 {
            let has_full_range_mark = child
                .marks
                .last()
                .is_some_and(|(range, _)| range.start == 0 && range.end == len);
            if has_full_range_mark {
                let (_, existing) = child.marks.last_mut().expect("checked above");
                existing.bold |= mark.bold;
                existing.italic |= mark.italic;
                existing.strikethrough |= mark.strikethrough;
                existing.code |= mark.code;
                if mark.link.is_some() {
                    existing.link = mark.link.clone();
                }
            } else {
                child.marks.push((0..len, mark.clone()));
            }
        }
        paragraph.push(child);
    }

    text
}

/// Detect whether a blockquote's first paragraph is a GitHub callout (e.g.
/// `> [!WARNING]`). Returns the callout kind and any text on that first line
/// that follows the `]` (usually empty).
fn callout_of(children: &[Node]) -> Option<(crate::text::node::CalloutKind, String)> {
    let first = children.first()?;
    let mdast::Node::Paragraph(paragraph) = first else {
        return None;
    };
    let text = paragraph_plain_text(paragraph);
    callout_kind_and_remainder(&text)
}

fn callout_kind_and_remainder(text: &str) -> Option<(crate::text::node::CalloutKind, String)> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();

    let kinds = [
        ("note", crate::text::node::CalloutKind::Note),
        ("tip", crate::text::node::CalloutKind::Tip),
        ("important", crate::text::node::CalloutKind::Important),
        ("warning", crate::text::node::CalloutKind::Warning),
        ("caution", crate::text::node::CalloutKind::Caution),
    ];

    for (keyword, kind) in kinds {
        let token = format!("[!{}]", keyword);
        if lower.starts_with(&token) {
            let rest = trimmed[token.len()..].trim_start();
            return Some((kind, rest.to_string()));
        }
    }
    None
}

/// Concatenate the plain text of a paragraph's inline children (enough to
/// detect a leading callout token).
fn paragraph_plain_text(paragraph: &mdast::Paragraph) -> String {
    paragraph
        .children
        .iter()
        .map(paragraph_plain_text_of_node)
        .collect()
}

fn paragraph_plain_text_of_node(node: &mdast::Node) -> String {
    match node {
        mdast::Node::Text(t) => t.value.clone(),
        mdast::Node::InlineCode(c) => c.value.clone(),
        mdast::Node::InlineMath(math) => math.value.clone(),
        mdast::Node::Break(_) => "\n".to_string(),
        mdast::Node::Image(image) => image.alt.clone(),
        mdast::Node::Strong(value) | mdast::Node::Emphasis(value) | mdast::Node::Delete(value) => {
            value
                .children
                .iter()
                .map(paragraph_plain_text_of_node)
                .collect()
        }
        mdast::Node::Link(value) => value
            .children
            .iter()
            .map(paragraph_plain_text_of_node)
            .collect(),
        mdast::Node::LinkReference(value) => value
            .children
            .iter()
            .map(paragraph_plain_text_of_node)
            .collect(),
        mdast::Node::Html(value) => value.value.clone(),
        _ => String::new(),
    }
}

fn ast_to_node(
    value: mdast::Node,
    style: &TextViewStyle,
    cx: &mut NodeContext,
    highlight_theme: &HighlightTheme,
) -> node::Node {
    match value {
        Node::Root(val) => {
            let children = val
                .children
                .into_iter()
                .map(|c| ast_to_node(c, style, cx, highlight_theme))
                .collect();
            node::Node::Root { children }
        }
        Node::Paragraph(val) => {
            let mut paragraph = Paragraph::default();
            val.children.iter().for_each(|c| {
                parse_paragraph(&mut paragraph, c, cx);
            });

            node::Node::Paragraph(paragraph)
        }
        Node::Blockquote(val) => {
            // GitHub-flavored callout/alerts like `> [!NOTE]`.
            if let Some((kind, remainder)) = callout_of(&val.children) {
                let mut children: Vec<node::Node> = Vec::new();
                if !remainder.is_empty() {
                    children.push(node::Node::Paragraph(node::Paragraph::new(remainder)));
                }
                children.extend(
                    val.children
                        .into_iter()
                        .skip(1)
                        .map(|c| ast_to_node(c, style, cx, highlight_theme))
                        .filter(|n| !matches!(n, node::Node::Unknown)),
                );
                node::Node::Callout { kind, children }
            } else {
                let children = val
                    .children
                    .into_iter()
                    .map(|c| ast_to_node(c, style, cx, highlight_theme))
                    .collect();
                node::Node::Blockquote { children }
            }
        }
        Node::List(list) => {
            let children = list
                .children
                .into_iter()
                .map(|c| ast_to_node(c, style, cx, highlight_theme))
                .collect();
            node::Node::List {
                ordered: list.ordered,
                start: list.start.unwrap_or(1) as usize,
                children,
            }
        }
        Node::ListItem(val) => {
            let children = val
                .children
                .into_iter()
                .map(|c| ast_to_node(c, style, cx, highlight_theme))
                .collect();
            node::Node::ListItem {
                children,
                spread: val.spread,
                checked: val.checked,
            }
        }
        Node::Break(_) => node::Node::Break { html: false },
        Node::Code(raw) => {
            let lang = raw.lang.clone();
            let is_mermaid = lang
                .as_deref()
                .map(|lang| lang.eq_ignore_ascii_case("mermaid"))
                .unwrap_or(false);

            if is_mermaid {
                #[cfg(not(target_family = "wasm"))]
                if let Some(svg) = render_mermaid_svg(&raw.value) {
                    let mut paragraph = Paragraph::default();
                    paragraph.push_image(ImageNode {
                        url: raw.value.clone().into(),
                        alt: Some(raw.value.clone().into()),
                        title: Some(raw.value.clone().into()),
                        mermaid_code: Some(raw.value.clone().into()),
                        mermaid_svg: Some(svg),
                        ..Default::default()
                    });
                    return node::Node::Paragraph(paragraph);
                }
                node::Node::CodeBlock(CodeBlock::new(
                    raw.value.into(),
                    lang.map(|s| s.into()),
                    style,
                    highlight_theme,
                ))
            } else {
                node::Node::CodeBlock(CodeBlock::new(
                    raw.value.into(),
                    lang.map(|s| s.into()),
                    style,
                    highlight_theme,
                ))
            }
        }
        Node::Heading(val) => {
            let mut paragraph = Paragraph::default();
            val.children.iter().for_each(|c| {
                parse_paragraph(&mut paragraph, c, cx);
            });

            node::Node::Heading {
                level: val.depth,
                children: paragraph,
            }
        }
        Node::Math(val) => {
            #[cfg(not(target_family = "wasm"))]
            if let Some(svg) = render_math_svg(&val.value, true) {
                let mut paragraph = Paragraph::default();
                paragraph.push_image(ImageNode {
                    url: val.value.clone().into(),
                    alt: Some(val.value.clone().into()),
                    title: Some(val.value.clone().into()),
                    math_tex: Some(val.value.clone().into()),
                    math_svg: Some(svg),
                    math_display_mode: true,
                    ..Default::default()
                });
                return node::Node::Paragraph(paragraph);
            }
            node::Node::CodeBlock(CodeBlock::new(
                val.value.into(),
                None,
                style,
                highlight_theme,
            ))
        }
        Node::Html(val) => match super::html::parse(&val.value, cx) {
            Ok(el) => el,
            Err(err) => {
                if cfg!(debug_assertions) {
                    tracing::warn!("error parsing html: {:#?}", err);
                }

                node::Node::Paragraph(Paragraph::new(val.value))
            }
        },
        Node::MdxFlowExpression(val) => node::Node::CodeBlock(CodeBlock::new(
            val.value.into(),
            Some("mdx".into()),
            style,
            highlight_theme,
        )),
        Node::Yaml(val) => node::Node::CodeBlock(CodeBlock::new(
            val.value.into(),
            Some("yml".into()),
            style,
            highlight_theme,
        )),
        Node::Toml(val) => node::Node::CodeBlock(CodeBlock::new(
            val.value.into(),
            Some("toml".into()),
            style,
            highlight_theme,
        )),
        Node::MdxJsxTextElement(val) => {
            let mut paragraph = Paragraph::default();
            val.children.iter().for_each(|c| {
                parse_paragraph(&mut paragraph, c, cx);
            });
            node::Node::Paragraph(paragraph)
        }
        Node::MdxJsxFlowElement(val) => {
            let mut paragraph = Paragraph::default();
            val.children.iter().for_each(|c| {
                parse_paragraph(&mut paragraph, c, cx);
            });
            node::Node::Paragraph(paragraph)
        }
        Node::ThematicBreak(_) => node::Node::Divider,
        Node::Table(val) => {
            let mut table = Table::default();
            table.column_aligns = val
                .align
                .clone()
                .into_iter()
                .map(|align| align.into())
                .collect();
            val.children.iter().for_each(|c| {
                if let Node::TableRow(row) = c {
                    parse_table_row(&mut table, row, cx);
                }
            });

            node::Node::Table(table)
        }
        Node::FootnoteDefinition(def) => {
            let mut paragraph = Paragraph::default();
            let prefix = format!("[{}]: ", def.identifier);
            paragraph.push(InlineNode::new(&prefix).marks(vec![(
                0..prefix.len(),
                TextMark {
                    italic: true,
                    ..Default::default()
                },
            )]));

            def.children.iter().for_each(|c| {
                parse_paragraph(&mut paragraph, c, cx);
            });
            node::Node::Paragraph(paragraph)
        }
        Node::Definition(def) => {
            cx.add_ref(
                def.identifier.clone().into(),
                LinkMark {
                    url: def.url.clone().into(),
                    identifier: Some(def.identifier.clone().into()),
                    title: def.title.clone().map(Into::into),
                },
            );

            node::Node::Definition {
                identifier: def.identifier.clone().into(),
                url: def.url.clone().into(),
                title: def.title.clone().map(|s| s.into()),
            }
        }
        _ => {
            if cfg!(debug_assertions) {
                tracing::warn!("unsupported node: {:#?}", value);
            }
            node::Node::Unknown
        }
    }
}
