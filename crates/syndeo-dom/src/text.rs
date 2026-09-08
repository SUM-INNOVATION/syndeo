//! Turning a tree into something worth reading.

use crate::{tag, walk};
use markup5ever_rcdom::{Handle, NodeData};

/// Elements whose contents are not prose.
const SKIPPED: &[&str] = &["script", "style", "noscript", "template", "svg", "head"];

/// Elements that end a line of prose.
const BLOCK: &[&str] = &[
    "p", "div", "section", "article", "header", "footer", "main", "aside", "nav", "h1", "h2", "h3",
    "h4", "h5", "h6", "li", "tr", "td", "th", "blockquote", "pre", "figcaption", "dt", "dd", "br",
    "hr", "form", "fieldset", "table", "ul", "ol",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBlock {
    /// The element the text sits directly inside, lowercased.
    pub element: String,
    pub text: String,
    /// Heading depth, 1 to 6, for `h1`..`h6`.
    pub heading_level: Option<u8>,
}

/// All text under a node, including script bodies. Used for `<title>`, where
/// that is exactly what is wanted.
pub fn collect_text(handle: &Handle) -> String {
    let mut out = String::new();
    walk(handle, &mut |node| {
        if let NodeData::Text { contents } = &node.data {
            out.push_str(&contents.borrow());
        }
    });
    out
}

/// Readable text: block elements separated by newlines, script and style gone,
/// runs of whitespace collapsed.
pub fn readable(root: &Handle) -> String {
    let mut out = String::new();
    render(root, &mut out);
    normalize(&out)
}

fn render(handle: &Handle, out: &mut String) {
    if let Some(name) = tag(handle) {
        if SKIPPED.contains(&name.as_str()) {
            return;
        }
        if BLOCK.contains(&name.as_str()) {
            out.push('\n');
        }
    }
    if let NodeData::Text { contents } = &handle.data {
        out.push_str(&contents.borrow());
    }
    for child in handle.children.borrow().iter() {
        render(child, out);
    }
    if let Some(name) = tag(handle) {
        if BLOCK.contains(&name.as_str()) {
            out.push('\n');
        }
    }
}

/// One block per block-level element that directly contains text.
pub fn blocks(root: &Handle) -> Vec<TextBlock> {
    let mut out = Vec::new();
    collect_blocks(root, &mut out);
    out
}

fn collect_blocks(handle: &Handle, out: &mut Vec<TextBlock>) {
    if let Some(name) = tag(handle) {
        if SKIPPED.contains(&name.as_str()) {
            return;
        }
        if BLOCK.contains(&name.as_str()) {
            let mut own = String::new();
            for child in handle.children.borrow().iter() {
                if tag(child)
                    .map(|t| BLOCK.contains(&t.as_str()))
                    .unwrap_or(false)
                {
                    continue;
                }
                render(child, &mut own);
            }
            let text = normalize(&own);
            if !text.is_empty() {
                out.push(TextBlock {
                    element: name.clone(),
                    heading_level: heading_level(&name),
                    text,
                });
            }
        }
    }
    for child in handle.children.borrow().iter() {
        collect_blocks(child, out);
    }
}

fn heading_level(name: &str) -> Option<u8> {
    let bytes = name.as_bytes();
    if bytes.len() == 2 && bytes[0] == b'h' && bytes[1].is_ascii_digit() {
        let level = bytes[1] - b'0';
        if (1..=6).contains(&level) {
            return Some(level);
        }
    }
    None
}

/// Collapse whitespace runs, and blank-line runs, without losing paragraphing.
fn normalize(raw: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.split('\n') {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            if lines.last().map(|l| l.is_empty()).unwrap_or(true) {
                continue;
            }
            lines.push(String::new());
        } else {
            lines.push(collapsed);
        }
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.join("\n")
}
