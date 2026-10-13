//! What the address field makes of what was typed, which pages a window may
//! show, and the page a window shows when one cannot be opened. Plain Rust,
//! so all of it is tested without WebKit.

use url::Url;

/// What typing into the address field leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed {
    /// Load this page.
    Load(Url),
    /// Nothing was typed.
    Empty,
    /// Not something Syndeo opens; says why.
    Refused(String),
}

/// Schemes written without `//` that someone might still type, each refused
/// rather than mistaken for a host name with a port.
const OPAQUE: [&str; 6] = ["about", "javascript", "data", "file", "mailto", "blob"];

/// Read what was typed into the address field.
///
/// `http://` and `https://` addresses are opened as they are. Anything with
/// another scheme is refused. Without a scheme, a host — a name with a dot,
/// `localhost`, or an IP address — with an optional port and path is opened
/// over https. Anything else, including words with spaces, is refused:
/// Syndeo has no search provider, so it does not send what was typed to one.
pub fn interpret(input: &str) -> Typed {
    let text = input.trim();
    if text.is_empty() {
        return Typed::Empty;
    }
    if let Some(scheme) = explicit_scheme(text) {
        return match scheme.as_str() {
            "http" | "https" => match Url::parse(text) {
                Ok(url) if url.host_str().is_some_and(|h| !h.is_empty()) => Typed::Load(url),
                _ => Typed::Refused(format!("{text} is not a web address Syndeo can open.")),
            },
            "about" if text.eq_ignore_ascii_case("about:blank") => {
                Typed::Load(Url::parse("about:blank").expect("about:blank is a URL"))
            }
            other => Typed::Refused(format!(
                "Syndeo opens http and https addresses, not {other}: ones."
            )),
        };
    }
    if text.chars().any(char::is_whitespace) {
        return Typed::Refused(
            "Syndeo opens web addresses. It does not search, so what you typed was not sent \
             anywhere."
                .to_string(),
        );
    }
    match Url::parse(&format!("https://{text}")) {
        Ok(url) if url.host().is_some_and(|host| looks_like_a_host(&host)) => Typed::Load(url),
        _ => Typed::Refused(format!(
            "{text} is not a web address. Syndeo does not search, so it was not sent anywhere."
        )),
    }
}

/// The scheme of `text`, when it plainly has one: `scheme://`, or one of the
/// schemes written without slashes. `example.com:8080` has none.
fn explicit_scheme(text: &str) -> Option<String> {
    let (scheme, rest) = text.split_once(':')?;
    let valid = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    if !valid {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    (rest.starts_with("//") || OPAQUE.contains(&scheme.as_str())).then_some(scheme)
}

fn looks_like_a_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(name) => {
            *name == "localhost" || (name.contains('.') && !name.ends_with('.'))
        }
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => true,
    }
}

/// Whether a window's main frame may show a page with this scheme. The web's
/// own, and `about:` for Syndeo's own pages; never a local file, a `data:`
/// or `blob:` page, or `javascript:`.
pub fn allowed_in_main_frame(scheme: &str) -> bool {
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "about"
    )
}

/// The page a window shows when `address` could not be opened.
pub fn failure_page(address: &str, why: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>Syndeo couldn't open this page</title>\
         <style>:root {{ color-scheme: light dark; }} \
         body {{ font: 15px/1.5 -apple-system, sans-serif; max-width: 40rem; margin: 4rem auto; padding: 0 1.5rem; }} \
         h1 {{ font-size: 1.5rem; font-weight: 600; }} code {{ word-break: break-all; }}</style></head>\
         <body><h1>Syndeo couldn't open this page</h1><p><code>{}</code></p><p>{}</p></body></html>",
        escape(address),
        escape(why)
    )
}

/// Text for an HTML page.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loads(input: &str) -> String {
        match interpret(input) {
            Typed::Load(url) => url.to_string(),
            other => panic!("{input:?} gave {other:?}"),
        }
    }

    fn refused(input: &str) -> String {
        match interpret(input) {
            Typed::Refused(why) => why,
            other => panic!("{input:?} gave {other:?}"),
        }
    }

    #[test]
    fn web_addresses_open_as_typed() {
        assert_eq!(loads("http://example.com/"), "http://example.com/");
        assert_eq!(
            loads("https://example.com/a?b=c#d"),
            "https://example.com/a?b=c#d"
        );
        assert_eq!(loads("  HTTPS://Example.COM  "), "https://example.com/");
        assert_eq!(loads("about:blank"), "about:blank");
    }

    #[test]
    fn a_host_without_a_scheme_opens_over_https() {
        assert_eq!(loads("example.com"), "https://example.com/");
        assert_eq!(
            loads("www.rust-lang.org/learn"),
            "https://www.rust-lang.org/learn"
        );
        assert_eq!(loads("example.com:8443/x"), "https://example.com:8443/x");
        assert_eq!(loads("localhost:3000"), "https://localhost:3000/");
        assert_eq!(loads("192.168.1.10"), "https://192.168.1.10/");
        assert_eq!(loads("[::1]:8080"), "https://[::1]:8080/");
    }

    #[test]
    fn other_schemes_are_refused_never_opened() {
        for input in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<b>hi</b>",
            "ftp://example.com/",
            "mailto:someone@example.com",
            "blob:https://example.com/x",
            "about:config",
        ] {
            let why = refused(input);
            assert!(why.contains("not"), "{input}: {why}");
        }
        assert!(refused("file:///etc/passwd").contains("not file: ones"));
    }

    #[test]
    fn words_are_not_sent_anywhere() {
        assert!(refused("how to bake bread").contains("does not search"));
        assert!(refused("bread").contains("does not search"));
        assert!(refused("example.").contains("does not search"));
        assert_eq!(interpret("   "), Typed::Empty);
    }

    #[test]
    fn a_web_scheme_without_a_host_is_refused() {
        assert!(refused("https://").contains("not a web address"));
        assert!(refused("http://").contains("not a web address"));
        // As every browser reads it, by the WHATWG URL rules: the empty
        // authority is skipped and `path` is the host.
        assert_eq!(loads("http:///path"), "http://path/");
    }

    #[test]
    fn only_the_webs_own_pages_and_syndeos_own_reach_the_main_frame() {
        for allowed in ["http", "https", "HTTPS", "about"] {
            assert!(allowed_in_main_frame(allowed), "{allowed}");
        }
        for refused in [
            "file",
            "data",
            "blob",
            "javascript",
            "ftp",
            "x-apple-data-detectors",
            "",
        ] {
            assert!(!allowed_in_main_frame(refused), "{refused}");
        }
    }

    #[test]
    fn pages_are_given_text_never_markup() {
        assert_eq!(
            escape("<b>\"&\"</b>"),
            "&lt;b&gt;&quot;&amp;&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn the_failure_page_shows_text_never_markup() {
        let page = failure_page("https://x/<script>", "it said \"no\" & 'why'");
        assert!(page.contains("https://x/&lt;script&gt;"));
        assert!(page.contains("it said &quot;no&quot; &amp; &#39;why&#39;"));
        assert!(!page.contains("<script>"));
    }
}
