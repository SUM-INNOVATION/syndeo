//! What `syndeo browse` prints for a person, as a string.
//!
//! Everything here that came from the page or its origin — the URL, the
//! content type, the title, the text, every link, subresource and form
//! field — passes through `syndeo_dom::terminal` before it reaches the
//! terminal, and before it is truncated or padded, so a column's width is
//! counted on what is actually shown. `--json` does not come through here:
//! JSON already escapes every control character, and its output is left
//! exactly as it was.

use std::fmt::Write;
use syndeo_dom::terminal::{multiline, single_line};
use syndeo_dom::Document;

/// How the fetch went, as the network process reported it.
pub struct Fetch {
    pub url: String,
    pub status: u16,
    pub source: String,
    pub protocol: String,
    pub elapsed_ms: u64,
    pub bytes: usize,
    pub content: Option<String>,
    pub content_type: Option<String>,
    /// The second fetch, for `--twice`: source, protocol, milliseconds.
    pub again: Option<(String, String, u64)>,
}

/// The whole human-readable report.
pub fn human(fetch: &Fetch, document: &Document, full: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", single_line(&fetch.url));
    let _ = writeln!(
        out,
        "  {}  {}  {}  {}ms  {}",
        fetch.status,
        single_line(&fetch.source),
        single_line(&fetch.protocol),
        fetch.elapsed_ms,
        syndeo_cache::stats::human(fetch.bytes as u64)
    );
    if let Some(content) = &fetch.content {
        let content = single_line(content);
        let _ = writeln!(
            out,
            "  content {}",
            content.chars().take(16).collect::<String>()
        );
    }
    if let Some(content_type) = &fetch.content_type {
        let _ = writeln!(out, "  type    {}", single_line(content_type));
    }
    if let Some((source, protocol, elapsed)) = &fetch.again {
        let _ = writeln!(
            out,
            "  again   {}  {}  {elapsed}ms",
            single_line(source),
            single_line(protocol)
        );
    }
    if let Some(cut) = document.cut_short() {
        let _ = writeln!(
            out,
            "  cut short: parsed {} of {}; the rest would cost more work than a page is allowed",
            syndeo_cache::stats::human(cut.parsed as u64),
            syndeo_cache::stats::human(cut.of as u64)
        );
    }
    let _ = writeln!(out);

    if let Some(title) = document.title() {
        let _ = writeln!(out, "# {}", single_line(&title));
        let _ = writeln!(out);
    }

    let text = document.text();
    let text = multiline(&text);
    let shown: Vec<&str> = text.lines().take(40).collect();
    let _ = writeln!(out, "{}", shown.join("\n"));
    if text.lines().count() > 40 {
        let _ = writeln!(out, "… {} more lines", text.lines().count() - 40);
    }

    if full {
        let links = document.links();
        if !links.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "links ({})", links.len());
            for link in links.iter().take(20) {
                let _ = writeln!(
                    out,
                    "  {:<60} {}",
                    cell(&link.url, 60),
                    cell(&link.text, 40)
                );
            }
        }
        let resources = document.subresources();
        if !resources.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "subresources ({})", resources.len());
            for resource in resources.iter().take(20) {
                let integrity = match &resource.integrity {
                    Some(_) => "integrity declared",
                    None => "no integrity — origin only",
                };
                let _ = writeln!(
                    out,
                    "  {:<11} {:<50} {}",
                    cell(&resource.kind, 11),
                    cell(&resource.url, 50),
                    integrity
                );
            }
        }
        let forms = document.forms();
        if !forms.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "forms ({})", forms.len());
            for form in &forms {
                let _ = writeln!(
                    out,
                    "  {} {}",
                    single_line(&form.method),
                    single_line(&form.action)
                );
                for field in &form.fields {
                    let _ = writeln!(
                        out,
                        "    {:<20} {:<10}{}",
                        cell(&field.name, 20),
                        cell(&field.kind, 10),
                        if field.required { " required" } else { "" }
                    );
                }
            }
        }
    }
    out
}

/// A value for one column: made printable first, then cut to `width`.
pub fn cell(value: &str, width: usize) -> String {
    let value = single_line(value);
    if value.chars().count() <= width {
        value.into_owned()
    } else {
        format!("{}…", value.chars().take(width - 1).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escape sequences and invisible characters in every place a page can put
    /// text: title, prose, link target and text, subresource URL, form action,
    /// method, field name and type.
    const HOSTILE: &str = "<html><head><title>T\u{1b}]52;c;cGF3bmVk\u{7}itle\u{202e}</title>\
        <script src=\"/s\u{1b}[2J.js\"></script></head><body>\
        <p>prose\u{1b}[1A\u{9b}line\u{2028}next</p>\
        <a href=\"/l\u{7f}ink\u{1b}]8;;https://evil.test\u{7}\">li\u{1b}[31mnk\u{200e}</a>\
        <form action=\"/a\u{1b}[H\" method=\"po\u{1b}st\"><input name=\"n\u{1b}[Kame\" type=\"te\u{2066}xt\"></form>\
        </body></html>";

    fn printable(s: &str) -> bool {
        s.chars().all(|c| {
            c == '\n'
                || !(c.is_control()
                    || matches!(
                        unicode_general_category::get_general_category(c),
                        unicode_general_category::GeneralCategory::Format
                    )
                    || c == '\u{2028}'
                    || c == '\u{2029}')
        })
    }

    fn fetch(url: &str) -> Fetch {
        Fetch {
            url: url.into(),
            status: 200,
            source: "origin".into(),
            protocol: "http/1.1".into(),
            elapsed_ms: 3,
            bytes: 10,
            content: Some("0123456789abcdef0123".into()),
            content_type: Some("text/html\u{1b}[2J".into()),
            again: Some(("cache".into(), "-".into(), 1)),
        }
    }

    #[test]
    fn nothing_a_page_supplies_reaches_the_terminal_as_a_control() {
        let document = Document::parse(HOSTILE, Some("https://site.test/"));
        for full in [false, true] {
            let shown = human(&fetch("https://site.test/p\u{1b}[2J"), &document, full);
            assert!(printable(&shown), "{shown:?}");
            assert!(!shown.contains('\u{1b}'));
        }
        let shown = human(&fetch("https://site.test/"), &document, true);
        assert!(shown.contains("# T]52;c;cGF3bmVkitle"), "{shown}");
        // The page's text normaliser already reads U+2028 as whitespace.
        assert!(shown.contains("prose[1Aline next"), "{shown}");
    }

    #[test]
    fn a_page_cut_short_says_so() {
        let html = format!("<title>deep</title>{}", "<div>".repeat(200_000));
        let document = Document::parse(&html, None);
        let shown = human(&fetch("https://site.test/"), &document, false);
        assert!(shown.contains("cut short: parsed"), "{shown}");
        let whole = Document::parse("<title>t</title><p>x", None);
        assert!(!human(&fetch("https://site.test/"), &whole, false).contains("cut short"));
    }

    #[test]
    fn widths_are_counted_on_what_is_shown() {
        // Invisible characters would otherwise count toward the width and
        // misalign every column after them.
        let value = format!("{}{}", "\u{200b}".repeat(30), "visible");
        assert_eq!(cell(&value, 20), "visible");
        assert_eq!(cell(&"x".repeat(30), 10), format!("{}…", "x".repeat(9)));
    }
}
