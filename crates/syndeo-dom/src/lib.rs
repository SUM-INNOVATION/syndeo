//! A headless DOM.
//!
//! This is the agent-first path through layer one: html5ever gives a
//! spec-correct tree without a renderer, a compositor, or a pixel. What the
//! agent needs from a page is its text, its links, its forms, and its
//! subresources — and, for peer fetch, the `integrity` attributes that say what
//! a subresource's bytes must hash to.
//!
//! Nothing here fetches. A [`Document`] is parsed from bytes the network process
//! already produced.

mod extract;
mod text;

pub use extract::{Form, FormField, Link, Subresource};
pub use text::TextBlock;

use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use syndeo_cache::Integrity;

/// A parsed page.
pub struct Document {
    dom: RcDom,
    base: Option<url::Url>,
}

impl Document {
    /// Parse HTML. `base_url` resolves relative references; without it, relative
    /// links are reported as they appear.
    pub fn parse(html: &str, base_url: Option<&str>) -> Self {
        let dom = html5ever::parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .read_from(&mut html.as_bytes())
            .expect("parsing html into an RcDom is infallible");
        Document {
            dom,
            base: base_url.and_then(|u| url::Url::parse(u).ok()),
        }
    }

    pub fn parse_bytes(bytes: &[u8], base_url: Option<&str>) -> Self {
        Self::parse(&String::from_utf8_lossy(bytes), base_url)
    }

    fn root(&self) -> Handle {
        self.dom.document.clone()
    }

    pub fn title(&self) -> Option<String> {
        let mut found = None;
        walk(&self.root(), &mut |handle| {
            if found.is_some() {
                return;
            }
            if let NodeData::Element { name, .. } = &handle.data {
                if name.local.as_ref() == "title" {
                    let text = text::collect_text(handle);
                    if !text.trim().is_empty() {
                        found = Some(text.trim().to_string());
                    }
                }
            }
        });
        found
    }

    /// Readable text, with the parts a human would not read — script, style,
    /// navigation chrome — left out.
    pub fn text(&self) -> String {
        text::readable(&self.root())
    }

    /// Text split into blocks, each tagged with the element it came from, which
    /// is what an agent needs to reason about structure.
    pub fn blocks(&self) -> Vec<TextBlock> {
        text::blocks(&self.root())
    }

    pub fn links(&self) -> Vec<Link> {
        extract::links(&self.root(), self.base.as_ref())
    }

    /// Scripts, stylesheets and images the page depends on, each carrying its
    /// declared integrity when it has one.
    pub fn subresources(&self) -> Vec<Subresource> {
        extract::subresources(&self.root(), self.base.as_ref())
    }

    pub fn forms(&self) -> Vec<Form> {
        extract::forms(&self.root(), self.base.as_ref())
    }

    /// Integrity metadata keyed by resolved URL.
    ///
    /// This is the input to the peer-fetch rule: a body offered by a peer is
    /// only accepted when there is an independent hash to check it against, and
    /// for a first, never-before-fetched subresource this map is the only place
    /// such a hash can come from.
    pub fn integrity_map(&self) -> Vec<(String, Integrity)> {
        self.subresources()
            .into_iter()
            .filter_map(|resource| {
                let raw = resource.integrity?;
                let integrity = Integrity::parse(&raw).ok()?;
                if integrity.is_empty() {
                    return None;
                }
                Some((resource.url, integrity))
            })
            .collect()
    }
}

/// Visit `handle` and everything under it, in document order.
///
/// With a stack of its own rather than the call stack: a page is free to nest
/// elements as deep as it likes, and recursing once per level let a few
/// hundred kilobytes of `<div>` overflow the thread and take the process
/// with it.
pub(crate) fn walk(handle: &Handle, visit: &mut impl FnMut(&Handle)) {
    let mut pending = vec![handle.clone()];
    while let Some(node) = pending.pop() {
        counters::visited();
        visit(&node);
        // Reversed, so the first child is the next one popped.
        pending.extend(node.children.borrow().iter().rev().cloned());
    }
}

/// How much work the traversals did, for tests that check it is linear
/// without timing anything.
#[cfg(test)]
pub(crate) mod counters {
    use std::cell::Cell;
    thread_local! {
        static VISITED: Cell<usize> = const { Cell::new(0) };
        static APPENDED: Cell<usize> = const { Cell::new(0) };
    }
    pub fn visited() {
        VISITED.with(|c| c.set(c.get() + 1));
    }
    pub fn appended() {
        APPENDED.with(|c| c.set(c.get() + 1));
    }
    /// (nodes visited, text appended) since the last reset, on this thread.
    pub fn take() -> (usize, usize) {
        (
            VISITED.with(|c| c.replace(0)),
            APPENDED.with(|c| c.replace(0)),
        )
    }
}

#[cfg(not(test))]
pub(crate) mod counters {
    #[inline(always)]
    pub fn visited() {}
    #[inline(always)]
    pub fn appended() {}
}

pub(crate) fn attribute(handle: &Handle, wanted: &str) -> Option<String> {
    if let NodeData::Element { attrs, .. } = &handle.data {
        for attr in attrs.borrow().iter() {
            if attr.name.local.as_ref().eq_ignore_ascii_case(wanted) {
                return Some(attr.value.to_string());
            }
        }
    }
    None
}

pub(crate) fn tag(handle: &Handle) -> Option<String> {
    match &handle.data {
        NodeData::Element { name, .. } => Some(name.local.as_ref().to_ascii_lowercase()),
        _ => None,
    }
}

pub(crate) fn resolve(base: Option<&url::Url>, reference: &str) -> String {
    match base {
        Some(base) => base
            .join(reference)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| reference.to_string()),
        None => reference.to_string(),
    }
}

#[cfg(test)]
mod depth {
    use super::*;

    /// Every node under `root`, counted without any of the code under test.
    fn node_count(root: &Handle) -> usize {
        let mut pending = vec![root.clone()];
        let mut count = 0;
        while let Some(node) = pending.pop() {
            count += 1;
            pending.extend(node.children.borrow().iter().cloned());
        }
        count
    }

    fn text_nodes(root: &Handle) -> usize {
        let mut pending = vec![root.clone()];
        let mut count = 0;
        while let Some(node) = pending.pop() {
            if matches!(node.data, NodeData::Text { .. }) {
                count += 1;
            }
            pending.extend(node.children.borrow().iter().cloned());
        }
        count
    }

    /// Run `test` on a thread whose stack is far too small to recurse once
    /// per level of the documents it builds, so a recursion cannot hide.
    fn on_a_small_stack(test: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(test)
            .unwrap()
            .join()
            .expect("the traversal overflowed a 256 KiB stack");
    }

    #[test]
    fn a_deeply_nested_page_is_parsed_read_and_dropped_on_a_small_stack() {
        // Through the parser, which is what a real page takes. Kept to a depth
        // that parses quickly: html5ever's own parse grows with the square of
        // the depth, which is its behaviour rather than ours. The depth that
        // proves there is no recursion is in the built-tree test above.
        on_a_small_stack(|| {
            let depth = 6_000;
            let mut html = String::with_capacity(depth * 30);
            html.push_str("<title>deep</title>");
            for i in 0..depth {
                if i % 1_500 == 0 {
                    html.push_str(r#"<a href="/x">x</a><form><input name="q"></form><img src="/i.png" integrity="sha256-AAAA">"#);
                }
                html.push_str("<div>");
            }
            html.push_str("the bottom");
            let document = Document::parse(&html, Some("https://deep.test/"));
            assert_eq!(document.title().as_deref(), Some("deep"));
            assert!(document.text().contains("the bottom"));
            assert!(document.blocks().iter().any(|b| b.text == "the bottom"));
            assert!(!document.links().is_empty());
            assert!(!document.subresources().is_empty());
            assert!(!document.forms().is_empty());
            let _ = document.integrity_map();
            drop(document);
        });
    }

    /// A document `depth` elements deep, built directly rather than parsed:
    /// the parser's own cost grows with the square of the depth, and this is
    /// about what happens after parsing.
    fn built(depth: usize) -> Document {
        use html5ever::{local_name, ns, QualName};
        use markup5ever_rcdom::Node;
        use std::cell::RefCell;
        let dom = RcDom::default();
        let element = |name: html5ever::LocalName, attrs: Vec<html5ever::Attribute>| {
            Node::new(NodeData::Element {
                name: QualName::new(None, ns!(html), name),
                attrs: RefCell::new(attrs),
                template_contents: RefCell::new(None),
                mathml_annotation_xml_integration_point: false,
            })
        };
        let adopt = |parent: &Handle, child: Handle| {
            child.parent.set(Some(std::rc::Rc::downgrade(parent)));
            parent.children.borrow_mut().push(child);
        };
        let mut current = dom.document.clone();
        for i in 0..depth {
            let div = element(local_name!("div"), Vec::new());
            if i % 50_000 == 0 {
                let link = element(
                    local_name!("a"),
                    vec![html5ever::Attribute {
                        name: QualName::new(None, ns!(), local_name!("href")),
                        value: "/x".into(),
                    }],
                );
                adopt(&current, link);
            }
            adopt(&current, div.clone());
            current = div;
        }
        adopt(
            &current,
            Node::new(NodeData::Text {
                contents: RefCell::new("the bottom".into()),
            }),
        );
        Document { dom, base: None }
    }

    #[test]
    fn a_tree_two_hundred_thousand_deep_is_traversed_and_dropped_on_a_small_stack() {
        on_a_small_stack(|| {
            let document = built(200_000);
            assert!(document.text().contains("the bottom"));
            assert!(document.blocks().iter().any(|b| b.text == "the bottom"));
            assert_eq!(document.links().len(), 4);
            assert!(document.subresources().is_empty());
            assert!(document.forms().is_empty());
            assert!(document.title().is_none());
            let _ = document.integrity_map();
            drop(document);
        });
    }

    #[test]
    fn every_traversal_visits_each_node_once() {
        let html = "<p>a<b>b</b></p><div>c<span>d<div>e</div></span></div>".repeat(50);
        let document = Document::parse(&html, None);
        let nodes = node_count(&document.root());
        for (name, run) in [
            (
                "links",
                Box::new(|d: &Document| drop(d.links())) as Box<dyn Fn(&Document)>,
            ),
            ("text", Box::new(|d: &Document| drop(d.text()))),
            ("blocks", Box::new(|d: &Document| drop(d.blocks()))),
        ] {
            counters::take();
            run(&document);
            let (visited, _) = counters::take();
            assert_eq!(visited, nodes, "{name} visited {visited} of {nodes} nodes");
        }
    }

    #[test]
    fn blocks_append_each_text_once_however_blocks_and_inlines_alternate() {
        // Alternating inline and block, each level with text of its own: the
        // shape that made the old block collection quadratic.
        let depth = 2_000;
        let mut html = String::new();
        for _ in 0..depth {
            html.push_str("<span>t<div>t");
        }
        let document = Document::parse(&html, None);
        let texts = text_nodes(&document.root());
        counters::take();
        let blocks = document.blocks();
        let (_, appended) = counters::take();
        assert!(
            appended <= texts,
            "{appended} appends for {texts} text nodes"
        );
        assert_eq!(blocks.len(), depth, "one block per div");
    }
}
