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

pub(crate) fn walk(handle: &Handle, visit: &mut impl FnMut(&Handle)) {
    visit(handle);
    for child in handle.children.borrow().iter() {
        walk(child, visit);
    }
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
