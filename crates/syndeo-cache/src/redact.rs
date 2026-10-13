//! URLs as logs and error messages may show them: the scheme, the host and
//! the port, and never the userinfo, path, query or fragment. Those can hold
//! a search, a token, a document's identifier or a password-reset link, and
//! a log is no place for any of them.

use std::fmt;

/// `url`, shown as its origin: `scheme://host`, with the port when the URL
/// names one. A URL without a host shows only its scheme, and anything that
/// does not parse shows as `<url>`: never as itself.
pub struct Redacted<'a>(pub &'a str);

impl fmt::Display for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Ok(url) = url::Url::parse(self.0) else {
            return f.write_str("<url>");
        };
        match url.host_str() {
            Some(host) if !host.is_empty() => {
                write!(f, "{}://{host}", url.scheme())?;
                match url.port() {
                    Some(port) => write!(f, ":{port}"),
                    None => Ok(()),
                }
            }
            _ => write!(f, "{}:<redacted>", url.scheme()),
        }
    }
}

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The lines of `source`, Rust code before its tests, that log or format a
/// URL as itself rather than through [`Redacted`]: a tracing field such as
/// `%` or `?` before `url` or `request.url`, a bare `url` field, or `url` in
/// braces in a format string. Each crate whose logs can carry a URL runs
/// this over its own sources in a test, so a new log line cannot quietly
/// show one.
pub fn unredacted(source: &str) -> Vec<String> {
    let code = source
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(source);
    let mut found = Vec::new();
    for (number, line) in code.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let bytes = line.as_bytes();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut from = 0;
        while let Some(at) = line[from..].find("url") {
            let start = from + at;
            let end = start + 3;
            from = end;
            if bytes.get(end).is_some_and(|&b| word(b)) {
                continue;
            }
            // The whole path the word ends: `url`, `request.url`.
            let mut path = start;
            while path > 0 && (word(bytes[path - 1]) || bytes[path - 1] == b'.') {
                path -= 1;
            }
            let before = if path > 0 { bytes[path - 1] } else { b' ' };
            let after = line[end..].trim_start();
            let field = before == b'%' || before == b'?';
            let bare = line[..path].trim_end().ends_with("!(") && after.starts_with(',');
            let braced = before == b'{' && (after.starts_with('}') || after.starts_with(':'));
            if field || bare || braced {
                found.push(format!("{}: {}", number + 1, line.trim()));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(url: &str) -> String {
        Redacted(url).to_string()
    }

    #[test]
    fn only_the_scheme_host_and_port_are_shown() {
        assert_eq!(
            shown("https://user:secret@example.com:8443/reset/7f3a?token=abc&q=my+search#frag"),
            "https://example.com:8443"
        );
        assert_eq!(shown("http://example.com/a/b?c=d"), "http://example.com");
        assert_eq!(shown("HTTPS://Example.COM/"), "https://example.com");
        assert_eq!(shown("http://127.0.0.1:4567/x"), "http://127.0.0.1:4567");
        assert_eq!(shown("http://[::1]:8080/x?y"), "http://[::1]:8080");
    }

    #[test]
    fn nothing_private_survives_whatever_the_url() {
        for url in [
            "https://user:secret@example.com/reset?token=private#private",
            "http://example.com/private?private=private",
            "data:text/html,private",
            "about:private",
            "private",
            "https://exa mple.com/private",
            "",
        ] {
            let shown = shown(url);
            assert!(!shown.contains("private"), "{url} showed as {shown}");
            assert!(
                !shown.contains("secret") && !shown.contains("token"),
                "{url} showed as {shown}"
            );
        }
        assert_eq!(shown("data:text/html,private"), "data:<redacted>");
        assert_eq!(shown("not a url at all"), "<url>");
    }

    #[test]
    fn the_scanner_finds_each_way_a_url_is_shown_as_itself() {
        let pct = '%';
        for shown in [
            format!("tracing::warn!({pct}url, \"fetch failed\");"),
            format!("tracing::warn!(url = {pct}request.url, \"x\");"),
            "tracing::debug!(url = ?state.request.url, \"x\");".to_string(),
            "tracing::debug!(url, \"x\");".to_string(),
            "format!(\"fetching {url}: {err}\")".to_string(),
            "format!(\"fetching {url:?}\")".to_string(),
        ] {
            assert_eq!(unredacted(&shown).len(), 1, "missed: {shown}");
        }
        for fine in [
            format!("tracing::warn!(url = {pct}Redacted(&request.url), \"x\");"),
            "tracing::debug!(url = %Redacted(url), \"x\");".to_string(),
            "let parsed = url::Url::parse(text);".to_string(),
            "    // %url in a comment".to_string(),
            "let my_url_count = 1;".to_string(),
        ] {
            assert!(unredacted(&fine).is_empty(), "flagged: {fine}");
        }
        let after_tests =
            "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() { format!(\"{url}\"); }\n}\n";
        assert!(unredacted(after_tests).is_empty());
    }

    #[test]
    fn nothing_in_this_crate_shows_a_url_as_itself() {
        for (file, source) in [
            ("blob.rs", include_str!("blob.rs")),
            ("cache.rs", include_str!("cache.rs")),
            ("error.rs", include_str!("error.rs")),
            ("headers.rs", include_str!("headers.rs")),
            ("index.rs", include_str!("index.rs")),
            ("lib.rs", include_str!("lib.rs")),
            ("policy.rs", include_str!("policy.rs")),
            ("range.rs", include_str!("range.rs")),
            ("redact.rs", include_str!("redact.rs")),
            ("sri.rs", include_str!("sri.rs")),
            ("stats.rs", include_str!("stats.rs")),
            ("vary.rs", include_str!("vary.rs")),
        ] {
            let found = unredacted(source);
            assert!(found.is_empty(), "{file}: {found:#?}");
        }
    }

    #[test]
    fn debug_shows_the_same() {
        assert_eq!(
            format!("{:?}", Redacted("https://example.com/private")),
            "https://example.com"
        );
    }
}
