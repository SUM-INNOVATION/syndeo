//! Turning a tree into something worth reading.

use crate::{tag, walk};
use markup5ever_rcdom::{Handle, NodeData};

/// Elements whose contents are not prose.
const SKIPPED: &[&str] = &["script", "style", "noscript", "template", "svg", "head"];

/// Elements that end a line of prose.
const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "aside",
    "nav",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "tr",
    "td",
    "th",
    "blockquote",
    "pre",
    "figcaption",
    "dt",
    "dd",
    "br",
    "hr",
    "form",
    "fieldset",
    "table",
    "ul",
    "ol",
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

/// One step of a traversal that also needs to know when an element ends.
enum Step {
    Enter(Handle),
    /// An element's children are done.
    Leave {
        block: Option<usize>,
        newline: bool,
    },
}

/// Readable text, in one pass with a stack of its own — see [`crate::walk`]
/// for why not the call stack.
fn render(root: &Handle, out: &mut String) {
    let mut pending = vec![Step::Enter(root.clone())];
    while let Some(step) = pending.pop() {
        let node = match step {
            Step::Enter(node) => node,
            Step::Leave { newline, .. } => {
                if newline {
                    out.push('\n');
                }
                continue;
            }
        };
        crate::counters::visited();
        let mut block = false;
        if let Some(name) = tag(&node) {
            if SKIPPED.contains(&name.as_str()) {
                continue;
            }
            block = BLOCK.contains(&name.as_str());
            if block {
                out.push('\n');
            }
        }
        if let NodeData::Text { contents } = &node.data {
            crate::counters::appended();
            out.push_str(&contents.borrow());
        }
        pending.push(Step::Leave {
            block: None,
            newline: block,
        });
        pending.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .map(|child| Step::Enter(child.clone())),
        );
    }
}

/// One block per block-level element that directly contains text.
///
/// Each piece of text belongs to the nearest block element around it, and to
/// no other: one pass, each text node appended once. (Counting it again for
/// every enclosing block it reached through a non-block element — a `table`
/// through its `tbody`, a `div` through a `span` — made the result grow with
/// depth times size, which a page could make as large as it liked.) Blocks
/// come out in document order, each where its element opens.
pub fn blocks(root: &Handle) -> Vec<TextBlock> {
    let mut out: Vec<Option<TextBlock>> = Vec::new();
    // For each open block: its slot in `out` and the text gathered so far.
    let mut open: Vec<(usize, String)> = Vec::new();
    let mut pending = vec![Step::Enter(root.clone())];
    while let Some(step) = pending.pop() {
        let node = match step {
            Step::Enter(node) => node,
            Step::Leave { block, .. } => {
                if block.is_some() {
                    let (slot, own) = open.pop().expect("a block was opened");
                    let text = normalize(&own);
                    if let Some(entry) = out[slot].as_mut() {
                        entry.text = text;
                    }
                }
                continue;
            }
        };
        crate::counters::visited();
        let mut opened = None;
        if let Some(name) = tag(&node) {
            if SKIPPED.contains(&name.as_str()) {
                continue;
            }
            if BLOCK.contains(&name.as_str()) {
                let slot = out.len();
                out.push(Some(TextBlock {
                    heading_level: heading_level(&name),
                    element: name,
                    text: String::new(),
                }));
                open.push((slot, String::new()));
                opened = Some(slot);
            }
        }
        if let NodeData::Text { contents } = &node.data {
            if let Some((_, own)) = open.last_mut() {
                crate::counters::appended();
                own.push_str(&contents.borrow());
            }
        }
        pending.push(Step::Leave {
            block: opened,
            newline: false,
        });
        pending.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .map(|child| Step::Enter(child.clone())),
        );
    }
    out.into_iter()
        .flatten()
        .filter(|block| !block.text.is_empty())
        .collect()
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
