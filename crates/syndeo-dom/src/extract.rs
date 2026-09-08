//! Pulling the actionable parts out of a tree: where you can go, what the page
//! depends on, and what you can submit.

use crate::{attribute, resolve, tag, walk};
use markup5ever_rcdom::Handle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub text: String,
    pub rel: Option<String>,
}

/// Something the page needs fetched, and what its bytes must hash to if it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subresource {
    pub url: String,
    /// `script`, `stylesheet`, `image`, `font`, `iframe`, or the raw `rel`.
    pub kind: String,
    /// The `integrity` attribute, verbatim.
    pub integrity: Option<String>,
    /// The `crossorigin` attribute, which SRI requires for cross-origin loads.
    pub crossorigin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    pub name: String,
    pub kind: String,
    pub value: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub action: String,
    pub method: String,
    pub fields: Vec<FormField>,
}

pub fn links(root: &Handle, base: Option<&url::Url>) -> Vec<Link> {
    let mut out = Vec::new();
    walk(root, &mut |handle| {
        if tag(handle).as_deref() != Some("a") {
            return;
        }
        let Some(href) = attribute(handle, "href") else {
            return;
        };
        if href.starts_with('#') || href.trim().is_empty() {
            return;
        }
        out.push(Link {
            url: resolve(base, &href),
            text: crate::text::readable(handle),
            rel: attribute(handle, "rel"),
        });
    });
    out
}

pub fn subresources(root: &Handle, base: Option<&url::Url>) -> Vec<Subresource> {
    let mut out = Vec::new();
    walk(root, &mut |handle| {
        let Some(name) = tag(handle) else { return };
        let (reference, kind) = match name.as_str() {
            "script" => (attribute(handle, "src"), "script".to_string()),
            "img" => (attribute(handle, "src"), "image".to_string()),
            "iframe" => (attribute(handle, "src"), "iframe".to_string()),
            "link" => {
                let rel = attribute(handle, "rel").unwrap_or_default().to_ascii_lowercase();
                let kind = match rel.as_str() {
                    "stylesheet" => "stylesheet".to_string(),
                    "preload" | "modulepreload" | "prefetch" => rel.clone(),
                    other if other.is_empty() => return,
                    other => other.to_string(),
                };
                (attribute(handle, "href"), kind)
            }
            _ => return,
        };
        let Some(reference) = reference else { return };
        if reference.trim().is_empty() || reference.starts_with("data:") {
            return;
        }
        out.push(Subresource {
            url: resolve(base, &reference),
            kind,
            integrity: attribute(handle, "integrity"),
            crossorigin: attribute(handle, "crossorigin"),
        });
    });
    out
}

pub fn forms(root: &Handle, base: Option<&url::Url>) -> Vec<Form> {
    let mut out = Vec::new();
    walk(root, &mut |handle| {
        if tag(handle).as_deref() != Some("form") {
            return;
        }
        let action = attribute(handle, "action").unwrap_or_default();
        let mut fields = Vec::new();
        walk(handle, &mut |node| {
            let Some(name) = tag(node) else { return };
            let kind = match name.as_str() {
                "input" => attribute(node, "type").unwrap_or_else(|| "text".into()),
                "textarea" => "textarea".to_string(),
                "select" => "select".to_string(),
                "button" => attribute(node, "type").unwrap_or_else(|| "submit".into()),
                _ => return,
            };
            let Some(field_name) = attribute(node, "name") else {
                return;
            };
            fields.push(FormField {
                name: field_name,
                kind: kind.to_ascii_lowercase(),
                value: attribute(node, "value"),
                required: attribute(node, "required").is_some(),
            });
        });
        out.push(Form {
            action: if action.is_empty() {
                base.map(|b| b.to_string()).unwrap_or_default()
            } else {
                resolve(base, &action)
            },
            method: attribute(handle, "method")
                .unwrap_or_else(|| "get".into())
                .to_ascii_uppercase(),
            fields,
        });
    });
    out
}
