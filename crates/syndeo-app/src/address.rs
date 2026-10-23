//! What the address field makes of what was typed, where a search is sent,
//! which pages a window may show, how a page is reached, and the page a
//! window shows when one cannot be opened. Plain Rust, so all of it is
//! tested without WebKit.

use std::net::{Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

/// What typing into the address field leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Typed {
    /// Load this page.
    Load(Url),
    /// Search for what was typed: this page, at the chosen search engine.
    Search(Url),
    /// Nothing was typed.
    Empty,
    /// Not something Syndeo opens; says why.
    Refused(String),
}

/// Where words typed in the address field are sent when Return is pressed,
/// and only then: Syndeo suggests nothing as you type, fetches nothing ahead
/// and checks nothing with the search engine, so it hears of a search only
/// when it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Search {
    DuckDuckGo,
    Startpage,
    Brave,
    Google,
    Bing,
    /// Syndeo does not search: words are refused, and sent nowhere.
    Off,
}

impl Search {
    /// Every choice, in the order Settings offers them.
    pub const ALL: [Search; 6] = [
        Search::DuckDuckGo,
        Search::Startpage,
        Search::Brave,
        Search::Google,
        Search::Bing,
        Search::Off,
    ];

    /// The choice before anyone makes one.
    pub const DEFAULT: Search = Search::DuckDuckGo;

    /// How the choice is stored.
    pub fn id(self) -> &'static str {
        match self {
            Search::DuckDuckGo => "duckduckgo",
            Search::Startpage => "startpage",
            Search::Brave => "brave",
            Search::Google => "google",
            Search::Bing => "bing",
            Search::Off => "off",
        }
    }

    /// How Settings names it.
    pub fn title(self) -> &'static str {
        match self {
            Search::DuckDuckGo => "DuckDuckGo",
            Search::Startpage => "Startpage",
            Search::Brave => "Brave Search",
            Search::Google => "Google",
            Search::Bing => "Bing",
            Search::Off => "Off — Syndeo does not search",
        }
    }

    /// The choice as stored: nothing stored is the default, and anything
    /// Syndeo does not know is Off, so that what was typed is never sent to
    /// a search engine nobody chose.
    pub fn from_setting(stored: Option<&str>) -> Search {
        match stored {
            None => Search::DEFAULT,
            Some(id) => Search::ALL
                .into_iter()
                .find(|search| search.id() == id)
                .unwrap_or(Search::Off),
        }
    }

    /// The engine's fixed search address, always https, and the name of the
    /// field the words go in.
    fn template(self) -> Option<(&'static str, &'static str)> {
        match self {
            Search::DuckDuckGo => Some(("https://duckduckgo.com/", "q")),
            Search::Startpage => Some(("https://www.startpage.com/do/search", "q")),
            Search::Brave => Some(("https://search.brave.com/search", "q")),
            Search::Google => Some(("https://www.google.com/search", "q")),
            Search::Bing => Some(("https://www.bing.com/search", "q")),
            Search::Off => None,
        }
    }

    /// The page that searches for `words`, or none when search is off. The
    /// words are one query field, encoded as a form encodes it, so nothing
    /// in them can end the field, add another, or change where it is sent.
    pub fn url(self, words: &str) -> Option<Url> {
        let (address, field) = self.template()?;
        let mut url = Url::parse(address).expect("a fixed search address is a URL");
        url.query_pairs_mut().append_pair(field, words);
        Some(url)
    }
}

/// How the page a window shows is reached, as its bar says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Through Syndeo's proxy, which may cache it.
    Syndeo,
    /// This Mac's own loopback: WebKit opens it directly, never through the
    /// proxy, so Syndeo does not cache it.
    Direct,
    /// A local-network address: WebKit may open it directly or through the
    /// proxy, and either way Syndeo does not cache it, because the proxy
    /// stores nothing that came from an address that is not public.
    Local,
}

impl Route {
    /// What the bar says.
    pub fn label(self) -> &'static str {
        match self {
            Route::Syndeo => "Through Syndeo",
            Route::Direct => "Direct — not cached by Syndeo",
            Route::Local => "Local — not cached by Syndeo",
        }
    }
}

/// Loopback, which WebKit was measured always opening directly: 127/8, and
/// ::1. (IPv4's loopback carried in IPv6 is 127/8: see [`route`].)
const DIRECT_V4: (Ipv4Addr, u8) = (Ipv4Addr::new(127, 0, 0, 0), 8);
const DIRECT_V6: (Ipv6Addr, u8) = (Ipv6Addr::LOCALHOST, 128);

/// Local-network ranges: private, link-local and shared (carrier-grade NAT)
/// IPv4, and unique local and link-local IPv6. WebKit was measured opening
/// some of these directly — this Mac's own addresses — and others through
/// the proxy, so which way it goes is not promised; only that Syndeo does
/// not cache them.
const LOCAL_V4: [(Ipv4Addr, u8); 5] = [
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
];
const LOCAL_V6: [(Ipv6Addr, u8); 2] = [
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
];

fn in_v4(ip: Ipv4Addr, (network, length): (Ipv4Addr, u8)) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(length)).unwrap_or(0);
    u32::from(ip) & mask == u32::from(network) & mask
}

fn in_v6(ip: Ipv6Addr, (network, length): (Ipv6Addr, u8)) -> bool {
    let mask = u128::MAX.checked_shl(128 - u32::from(length)).unwrap_or(0);
    u128::from(ip) & mask == u128::from(network) & mask
}

/// How the page at `url` is reached, by the rule its bar follows, from what
/// was measured on CI and `ci/test-app.sh routes` gates:
///
/// - **Direct** for a host of exactly `localhost` or `localhost.`, or a
///   loopback address (above).
/// - **Local** for an address in a local-network range (above).
/// - **Through Syndeo** for every other name, whatever it resolves to (a
///   `.local` name, one under `.localhost`, a single label, a public name
///   for this Mac), and every other address.
///
/// An IPv4 address carried in IPv6 (`::ffff:a.b.c.d`) is that IPv4 address,
/// and is classified as one: mapped loopback is Direct, a mapped
/// local-network address Local, and a mapped public address Through Syndeo.
///
/// None for a page that is not the web's, which is not reached at all.
///
/// This is the page's own route. What it loads — images, scripts, frames —
/// is each reached by its own address, and a redirect is followed to the
/// address it leads to.
pub fn route(url: &Url) -> Option<Route> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = match url.host()? {
        Host::Ipv6(ip) => ip.to_ipv4_mapped().map_or(Host::Ipv6(ip), Host::Ipv4),
        host => host,
    };
    Some(match host {
        Host::Domain("localhost" | "localhost.") => Route::Direct,
        Host::Domain(_) => Route::Syndeo,
        Host::Ipv4(ip) if in_v4(ip, DIRECT_V4) => Route::Direct,
        Host::Ipv4(ip) if LOCAL_V4.iter().any(|&range| in_v4(ip, range)) => Route::Local,
        Host::Ipv6(ip) if in_v6(ip, DIRECT_V6) => Route::Direct,
        Host::Ipv6(ip) if LOCAL_V6.iter().any(|&range| in_v6(ip, range)) => Route::Local,
        Host::Ipv4(_) | Host::Ipv6(_) => Route::Syndeo,
    })
}

/// [`route`], for an address as text.
pub fn route_of(address: &str) -> Option<Route> {
    Url::parse(address).ok().as_ref().and_then(route)
}

/// What Syndeo says of an address whose host is an IPv6 address with a
/// zone, typed or followed from a page.
pub const ZONED: &str = "Syndeo can't open IPv6 link-local addresses that name a network \
                         interface, such as fe80::1%en0, yet.";

/// Whether `address` names an IPv6 host with a zone — `[fe80::1%en0]`, or
/// `[fe80::1%25en0]` as a URL writes it. Syndeo refuses those, typed or
/// followed from a page, until it can open them properly.
pub fn has_zone(address: &str) -> bool {
    let rest = address.split_once("://").map_or(address, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let Some((_, host)) = authority.split_once('[') else {
        return false;
    };
    let host = host.split_once(']').map_or(host, |(host, _)| host);
    host.contains('%')
}

/// Schemes written without `//` that someone might still type, each refused
/// rather than mistaken for a host name with a port.
const OPAQUE: [&str; 6] = ["about", "javascript", "data", "file", "mailto", "blob"];

/// Read what was typed into the address field, with `search` as the chosen
/// search engine.
///
/// `http://` and `https://` addresses are opened as they are. Anything with
/// another scheme is refused. Without a scheme, a host — a name with a dot,
/// `localhost`, or an IP address — with an optional port and path is opened
/// over https. An IPv6 host with a zone is refused either way. Anything else,
/// including words with spaces, is a search with `search`, or, with search
/// off, refused and sent nowhere.
pub fn interpret(input: &str, search: Search) -> Typed {
    let text = input.trim();
    if text.is_empty() {
        return Typed::Empty;
    }
    let scheme = explicit_scheme(text);
    if matches!(scheme.as_deref(), None | Some("http" | "https")) && has_zone(text) {
        return Typed::Refused(ZONED.to_string());
    }
    if let Some(scheme) = scheme {
        return match scheme.as_str() {
            "http" | "https" => match Url::parse(text) {
                Ok(url) if url.host_str().is_some_and(|h| !h.is_empty()) => Typed::Load(url),
                _ => Typed::Refused(format!("{text} is not a web address Syndeo can open.")),
            },
            "about" if text.eq_ignore_ascii_case("about:blank") => {
                Typed::Load(Url::parse("about:blank").expect("about:blank is a URL"))
            }
            other => Typed::Refused(other_scheme(other)),
        };
    }
    if !text.chars().any(char::is_whitespace) {
        if let Ok(url) = Url::parse(&format!("https://{text}")) {
            if url.host().is_some_and(|host| looks_like_a_host(&host)) {
                return Typed::Load(url);
            }
        }
    }
    match search.url(text) {
        Some(url) => Typed::Search(url),
        None => Typed::Refused(format!(
            "{text} is not a web address. Search is off, so Syndeo does not search, and what \
             you typed was not sent anywhere. To search from here, choose a search engine in \
             Settings."
        )),
    }
}

/// Why an address with another scheme than http or https is refused.
fn other_scheme(scheme: &str) -> String {
    format!("Syndeo opens http and https addresses, not {scheme}: ones.")
}

/// What Syndeo does with an address another application asks it to open: a
/// link clicked in Mail, `open -a Syndeo <address>`, or any link at all once
/// someone makes Syndeo their default browser.
#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    /// Open this page, in a tab made as every tab is.
    Open(Url),
    /// Not something Syndeo opens; says why, in the words typing it gets.
    Refused(String),
}

/// Read an address another application asks Syndeo to open.
///
/// Only an `http://` or `https://` address with a host is opened. Everything
/// else is refused: another scheme, a file, Syndeo's own `about:` pages, an
/// IPv6 host with a zone, and anything that is not an address at all, which
/// is never searched for: another application's words are not sent to a
/// search engine, whichever one Settings has chosen.
pub fn asked(address: &str) -> Asked {
    let text = address.trim();
    let scheme = explicit_scheme(text).unwrap_or_default();
    if scheme != "http" && scheme != "https" {
        return Asked::Refused(if scheme.is_empty() {
            format!("{text} is not a web address Syndeo can open.")
        } else {
            other_scheme(&scheme)
        });
    }
    if has_zone(text) {
        return Asked::Refused(ZONED.to_string());
    }
    match Url::parse(text) {
        Ok(url) if url.host_str().is_some_and(|host| !host.is_empty()) => Asked::Open(url),
        _ => Asked::Refused(format!("{text} is not a web address Syndeo can open.")),
    }
}

/// The scheme of `text`, when it plainly has one: `scheme://`, or one of the
/// schemes written without slashes. `example.com:8080` has none.
pub fn explicit_scheme(text: &str) -> Option<String> {
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

/// `NSURLErrorSecureConnectionFailed`.
pub const SECURE_CONNECTION_FAILED: isize = -1200;

/// `NSURLErrorCancelled`: what a load is when the window refused its
/// certificate.
pub const CANCELLED: isize = -999;

/// How macOS refused a page through Syndeo's proxy, in the two ways Syndeo
/// answers by sending its host directly (#58). Measured on hosted macOS
/// 14.8, 15.7 and 26.6 runners, through the same proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// macOS 15 and earlier, for a site on the HSTS preload list built into
    /// macOS, such as github.com: macOS accepts only a publicly trusted
    /// certificate there, whatever an application answers. It asks the
    /// window, which vouches for the proxy's certificate, and then fails the
    /// connection anyway (NSURLErrorDomain -1200). macOS 26 does not do this.
    Preloaded,
    /// Every macOS, for Apple's own sites, such as www.apple.com: macOS pins
    /// them to Apple's certificates, so the window's own check of the
    /// proxy's fails ("certificates do not meet pinning requirements") and
    /// the load is cancelled (-999).
    Pinned,
}

/// Whether a main frame's failure, `domain` and `code`, was macOS refusing
/// it through the proxy: `vouched`, the window vouched for its host's
/// certificate recently; `pinned`, the window's check of it failed on
/// macOS's own pinning. None for every other failure.
pub fn refusal(domain: &str, code: isize, vouched: bool, pinned: bool) -> Option<Refusal> {
    if domain != "NSURLErrorDomain" {
        return None;
    }
    match code {
        SECURE_CONNECTION_FAILED if vouched => Some(Refusal::Preloaded),
        CANCELLED if pinned => Some(Refusal::Pinned),
        _ => None,
    }
}

/// What a window does after a main frame's failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFailure {
    /// Send its host directly, from now on, and load the page again.
    RetryDirect(Refusal),
    /// What WebKit says: it was no refusal, or it was a page this window
    /// already retried directly, which is never retried again.
    AsWebKitSays,
}

/// What to do after a main frame's failure: `refusal`, from [`refusal`];
/// `retried`, this window already retried this page directly.
pub fn after_failure(refusal: Option<Refusal>, retried: bool) -> AfterFailure {
    match refusal {
        Some(refusal) if !retried => AfterFailure::RetryDirect(refusal),
        _ => AfterFailure::AsWebKitSays,
    }
}

/// Why a refused page is not shown, when its host could not be sent
/// directly: what macOS did, and why, for `macos`, its major version. The
/// page says this in place of WebKit's words, which read as the site's fault.
pub fn explain(refusal: Refusal, host: &str, macos: u64) -> String {
    let around = "Syndeo opens every secure page through its own proxy, with a certificate \
                  of its own, and could not send this site around it, so it cannot open it.";
    match refusal {
        Refusal::Preloaded if macos >= 26 => format!(
            "macOS refused the secure connection to {host}, after this window had checked \
             that its certificate came from Syndeo's own proxy. {around}"
        ),
        Refusal::Preloaded => format!(
            "macOS refused the secure connection to {host}, after this window had checked \
             that its certificate came from Syndeo's own proxy. On macOS {macos} and earlier, \
             macOS accepts only a publicly trusted certificate for the sites on the HSTS \
             preload list built into macOS, such as github.com, whatever an application \
             says. {around}"
        ),
        Refusal::Pinned => format!(
            "macOS pins {host} to Apple's own certificates, so it accepts no other for it, \
             Syndeo's included. {around}"
        ),
    }
}

/// Whether `host` goes around the proxy when `domain` is sent directly: the
/// domain itself, or a name under it. That is how the Network framework
/// matches a domain excluded from a proxy configuration: measured on macOS
/// 15 and 26, excluding `d` sent `d` and `x.d` directly and `xd` through the
/// proxy (#58).
pub fn covered(host: &str, domain: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    !domain.is_empty()
        && (host == domain
            || host
                .strip_suffix(domain.as_str())
                .is_some_and(|rest| rest.ends_with('.')))
}

/// Whether `host` goes around the proxy, given the domains sent directly.
pub fn sent_direct(host: &str, direct: &[String]) -> bool {
    direct.iter().any(|domain| covered(host, domain))
}

/// The hosts a page's window sends directly once the page has loaded: those
/// it vouched for recently that the proxy then said were refused, as macOS
/// refuses its preloaded sites, and those macOS pinned; none already sent
/// directly, each once.
pub fn to_send_direct(
    vouched: &[String],
    refused: &[String],
    pinned: &[String],
    direct: &[String],
) -> Vec<String> {
    let mut hosts: Vec<String> = vouched
        .iter()
        .filter(|host| refused.contains(host))
        .chain(pinned)
        .filter(|host| !sent_direct(host, direct))
        .cloned()
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// [`route_of`], except that a page whose host is sent directly is Direct.
pub fn route_with(address: &str, direct: &[String]) -> Option<Route> {
    let route = route_of(address)?;
    let host = Url::parse(address).ok()?.host_str()?.to_string();
    Some(if sent_direct(&host, direct) {
        Route::Direct
    } else {
        route
    })
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
        // An address is opened whatever the search engine.
        let off = interpret(input, Search::Off);
        assert_eq!(off, interpret(input, Search::DuckDuckGo), "{input:?}");
        match off {
            Typed::Load(url) => url.to_string(),
            other => panic!("{input:?} gave {other:?}"),
        }
    }

    fn refused(input: &str) -> String {
        match interpret(input, Search::Off) {
            Typed::Refused(why) => why,
            other => panic!("{input:?} gave {other:?}"),
        }
    }

    fn searched(input: &str, search: Search) -> Url {
        match interpret(input, search) {
            Typed::Search(url) => url,
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
    fn other_schemes_are_refused_never_opened_or_searched() {
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
            assert!(
                matches!(interpret(input, Search::DuckDuckGo), Typed::Refused(_)),
                "{input} was searched for"
            );
        }
        assert!(refused("file:///etc/passwd").contains("not file: ones"));
    }

    #[test]
    fn with_search_off_words_are_not_sent_anywhere() {
        assert!(refused("how to bake bread").contains("does not search"));
        assert!(refused("bread").contains("does not search"));
        assert!(refused("example.").contains("does not search"));
        assert!(refused("bread").contains("Settings"));
        assert_eq!(interpret("   ", Search::DuckDuckGo), Typed::Empty);
    }

    #[test]
    fn words_are_a_search_with_the_chosen_engine() {
        assert_eq!(
            searched("how to bake bread", Search::DuckDuckGo).as_str(),
            "https://duckduckgo.com/?q=how+to+bake+bread"
        );
        assert_eq!(
            searched("bread", Search::Google).as_str(),
            "https://www.google.com/search?q=bread"
        );
        // What is not a host is a search, not a guess at one.
        for input in [
            "example.",
            "news:today",
            "what is 2:3",
            "foo/bar",
            "syndeoprobe",
        ] {
            searched(input, Search::DuckDuckGo);
        }
    }

    /// Each engine's address is fixed, https, and pinned here.
    #[test]
    fn each_engine_has_its_own_fixed_https_address() {
        let words = "syndeo test";
        let expected = [
            (Search::DuckDuckGo, "https://duckduckgo.com/?q=syndeo+test"),
            (
                Search::Startpage,
                "https://www.startpage.com/do/search?q=syndeo+test",
            ),
            (
                Search::Brave,
                "https://search.brave.com/search?q=syndeo+test",
            ),
            (
                Search::Google,
                "https://www.google.com/search?q=syndeo+test",
            ),
            (Search::Bing, "https://www.bing.com/search?q=syndeo+test"),
        ];
        for (search, url) in expected {
            assert_eq!(search.url(words).unwrap().as_str(), url, "{search:?}");
        }
        assert_eq!(Search::Off.url(words), None);
        assert_eq!(Search::ALL.len(), expected.len() + 1);
    }

    /// Whatever is typed is the one query field and nothing else: it cannot
    /// end the field, add one, add a fragment, or change the address.
    #[test]
    fn the_words_are_one_encoded_field_whatever_they_hold() {
        for words in [
            "a&b=c",
            "q=x&q=y",
            "#fragment",
            "100% sure",
            "1+1=2",
            "?",
            "/path/../x",
            "\"quoted\" <script>alert(1)</script>",
            "ünïcödé 日本語 🙂",
            "https://evil.example/?q=1",
            "%41%26",
            "a\tb",
        ] {
            for search in Search::ALL {
                let Some(url) = search.url(words) else {
                    continue;
                };
                let (address, field) = search.template().unwrap();
                let fixed = Url::parse(address).unwrap();
                assert_eq!(url.scheme(), "https", "{search:?} {words:?}");
                assert_eq!(url.host(), fixed.host(), "{search:?} {words:?}");
                assert_eq!(url.path(), fixed.path(), "{search:?} {words:?}");
                assert_eq!(url.fragment(), None, "{search:?} {words:?}");
                let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
                assert_eq!(
                    pairs,
                    [(field.to_string(), words.to_string())],
                    "{search:?} {words:?}: {url}"
                );
            }
        }
        assert_eq!(
            Search::DuckDuckGo.url("a b&c=d#e+f%").unwrap().as_str(),
            "https://duckduckgo.com/?q=a+b%26c%3Dd%23e%2Bf%25"
        );
    }

    #[test]
    fn the_stored_choice_is_read_strictly() {
        assert_eq!(Search::from_setting(None), Search::DuckDuckGo);
        for search in Search::ALL {
            assert_eq!(Search::from_setting(Some(search.id())), search);
        }
        for unknown in [
            "",
            "DuckDuckGo",
            "yahoo",
            "duckduckgo ",
            "https://evil.example/?q=",
        ] {
            assert_eq!(
                Search::from_setting(Some(unknown)),
                Search::Off,
                "{unknown:?} sends words somewhere nobody chose"
            );
        }
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
    fn an_ipv6_address_with_a_zone_is_refused_however_it_is_written() {
        for input in [
            "http://[fe80::1%25en0]:8080/page",
            "https://[fe80::1%en0]/",
            "[fe80::1%en0]:8080",
            "[fe80::1%25lo0]",
            "http://user@[fe80::1%25en0]/",
            "http://[fe80::1%25en0",
        ] {
            for search in [Search::Off, Search::DuckDuckGo] {
                assert_eq!(
                    interpret(input, search),
                    Typed::Refused(ZONED.to_string()),
                    "{input}"
                );
            }
        }
        assert_eq!(loads("http://[fe80::1]/"), "http://[fe80::1]/");
        assert!(has_zone("http://[fe80::1%25en0]:80/page?t=%41"));
        assert!(!has_zone("http://example.com/%41?q=%25"));
        assert!(!has_zone("http://[::1]/a%20b"));
        assert!(!has_zone("http://example.com/[a%b]"));
    }

    fn route_shown(address: &str) -> Option<Route> {
        route_of(address)
    }

    /// What was measured on CI, each address as typed. Direct: what WebKit
    /// always opened directly. Local: local-network addresses, which WebKit
    /// opened directly when they were this Mac's own and through the proxy
    /// when they were not. Through Syndeo: every name and public address.
    #[test]
    fn the_bar_says_what_was_measured() {
        for direct in [
            "http://localhost:8080/",
            "http://localhost.:8080/",
            "https://LOCALHOST/",
            "http://127.0.0.1:8080/",
            "http://127.0.0.5:8080/",
            "http://[::1]:8080/",
            "http://[::ffff:127.0.0.1]:8080/",
        ] {
            assert_eq!(route_shown(direct), Some(Route::Direct), "{direct}");
        }
        for local in [
            "http://192.168.64.8:8080/",
            "http://192.168.64.3:8080/",
            "http://10.255.42.1:8080/",
            "http://10.200.200.200:8080/",
            "http://172.31.42.1:8080/",
            "http://172.20.200.200:8080/",
            "http://192.168.200.200:8080/",
            "http://169.254.42.1:8080/",
            "http://100.64.42.1:8080/",
            "http://100.100.100.100:8080/",
            "http://[fd42:5a7e:6e5d::1]:8080/",
            "http://[fe80::1]:8080/",
        ] {
            assert_eq!(route_shown(local), Some(Route::Local), "{local}");
        }
        for through in [
            "http://example.com/",
            "https://example.com/",
            "http://104.20.23.154/",
            "http://syndeoprobe:8080/",
            "http://syndeo-probe.localhost:8080/",
            "http://runner.local:8080/",
            "http://127.0.0.1.nip.io:8080/",
            "http://syndeo.local/stats",
        ] {
            assert_eq!(route_shown(through), Some(Route::Syndeo), "{through}");
        }
        for not_the_web in [
            "about:blank",
            "file:///etc/passwd",
            "data:text/html,x",
            "ftp://example.com/",
            "ws://127.0.0.1/",
        ] {
            assert_eq!(route_shown(not_the_web), None, "{not_the_web}");
        }
        assert_eq!(Route::Direct.label(), "Direct — not cached by Syndeo");
        assert_eq!(Route::Local.label(), "Local — not cached by Syndeo");
        assert_eq!(Route::Syndeo.label(), "Through Syndeo");
    }

    /// Each range is exactly as wide as it says: its first and last
    /// addresses are in it, and the addresses either side of it are not.
    #[test]
    fn each_range_ends_where_it_says() {
        for (route, inside, outside) in [
            (
                Route::Direct,
                ["127.0.0.0", "127.255.255.255"],
                ["126.255.255.255", "128.0.0.0"],
            ),
            (Route::Direct, ["[::1]", "[::1]"], ["[::]", "[::2]"]),
            (
                Route::Direct,
                ["[::ffff:127.0.0.0]", "[::ffff:127.255.255.255]"],
                ["[::ffff:126.255.255.255]", "[::ffff:128.0.0.0]"],
            ),
            (
                Route::Local,
                ["10.0.0.0", "10.255.255.255"],
                ["9.255.255.255", "11.0.0.0"],
            ),
            (
                Route::Local,
                ["172.16.0.0", "172.31.255.255"],
                ["172.15.255.255", "172.32.0.0"],
            ),
            (
                Route::Local,
                ["192.168.0.0", "192.168.255.255"],
                ["192.167.255.255", "192.169.0.0"],
            ),
            (
                Route::Local,
                ["169.254.0.0", "169.254.255.255"],
                ["169.253.255.255", "169.255.0.0"],
            ),
            (
                Route::Local,
                ["100.64.0.0", "100.127.255.255"],
                ["100.63.255.255", "100.128.0.0"],
            ),
            (
                Route::Local,
                ["[fc00::]", "[fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]"],
                ["[fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]", "[fe00::]"],
            ),
            (
                Route::Local,
                ["[fe80::]", "[febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff]"],
                ["[fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff]", "[fec0::]"],
            ),
        ] {
            for host in inside {
                let address = format!("http://{host}/");
                assert_eq!(route_shown(&address), Some(route), "{address}");
            }
            for host in outside {
                let address = format!("http://{host}/");
                assert_eq!(route_shown(&address), Some(Route::Syndeo), "{address}");
            }
        }
    }

    /// An IPv4 address carried in IPv6 is classified as that IPv4 address:
    /// each IPv4 range's first and last addresses, and the addresses either
    /// side of it, say the same mapped as they do plain.
    #[test]
    fn an_ipv4_address_carried_in_ipv6_is_classified_as_itself() {
        for (route, inside, outside) in [
            (
                Route::Direct,
                ["127.0.0.0", "127.255.255.255"],
                ["126.255.255.255", "128.0.0.0"],
            ),
            (
                Route::Local,
                ["10.0.0.0", "10.255.255.255"],
                ["9.255.255.255", "11.0.0.0"],
            ),
            (
                Route::Local,
                ["172.16.0.0", "172.31.255.255"],
                ["172.15.255.255", "172.32.0.0"],
            ),
            (
                Route::Local,
                ["192.168.0.0", "192.168.255.255"],
                ["192.167.255.255", "192.169.0.0"],
            ),
            (
                Route::Local,
                ["169.254.0.0", "169.254.255.255"],
                ["169.253.255.255", "169.255.0.0"],
            ),
            (
                Route::Local,
                ["100.64.0.0", "100.127.255.255"],
                ["100.63.255.255", "100.128.0.0"],
            ),
        ] {
            for (v4, expected) in inside
                .iter()
                .map(|a| (a, route))
                .chain(outside.iter().map(|a| (a, Route::Syndeo)))
            {
                let plain = format!("http://{v4}/");
                let mapped = format!("http://[::ffff:{v4}]/");
                assert_eq!(route_shown(&plain), Some(expected), "{plain}");
                assert_eq!(route_shown(&mapped), Some(expected), "{mapped}");
            }
        }
        for public in ["104.20.23.154", "93.184.215.14", "1.1.1.1", "8.8.8.8"] {
            let mapped = format!("https://[::ffff:{public}]/");
            assert_eq!(route_shown(&mapped), Some(Route::Syndeo), "{mapped}");
        }
        // Only the mapped form: the old IPv4-compatible one (::a.b.c.d) is an
        // IPv6 address like any other, and ::1 is loopback, not 0.0.0.1.
        assert_eq!(route_shown("http://[::127.0.0.1]/"), Some(Route::Syndeo));
        assert_eq!(route_shown("http://[::10.0.0.1]/"), Some(Route::Syndeo));
        assert_eq!(route_shown("http://[::1]/"), Some(Route::Direct));
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

    fn opened(address: &str) -> String {
        match asked(address) {
            Asked::Open(url) => url.to_string(),
            other => panic!("{address:?} gave {other:?}"),
        }
    }

    fn not_opened(address: &str) -> String {
        match asked(address) {
            Asked::Refused(why) => why,
            other => panic!("{address:?} gave {other:?}"),
        }
    }

    #[test]
    fn an_application_asking_for_a_web_address_gets_it_opened() {
        assert_eq!(opened("http://example.com/"), "http://example.com/");
        assert_eq!(
            opened("https://example.com/a?b=c#d"),
            "https://example.com/a?b=c#d"
        );
        assert_eq!(opened(" HTTPS://Example.COM "), "https://example.com/");
        assert_eq!(opened("http://127.0.0.1:8080/x"), "http://127.0.0.1:8080/x");
        assert_eq!(opened("http://[::1]/"), "http://[::1]/");
    }

    /// Refused with the words typing the same address gets.
    #[test]
    fn an_application_asking_for_anything_else_is_refused() {
        for address in [
            "file:///etc/passwd",
            "ftp://example.com/",
            "javascript:alert(1)",
            "data:text/html,<b>hi</b>",
            "mailto:someone@example.com",
            "blob:https://example.com/x",
            "about:blank",
            "about:config",
            "x-other-app://open?thing=1",
        ] {
            let why = not_opened(address);
            assert!(
                why.starts_with("Syndeo opens http and https addresses, not "),
                "{address}: {why}"
            );
        }
        assert_eq!(
            not_opened("file:///etc/passwd"),
            refused("file:///etc/passwd"),
            "not the words typing it gets"
        );
        assert!(not_opened("ftp://example.com/").contains("not ftp: ones"));
        assert_eq!(not_opened("http://[fe80::1%25en0]/"), ZONED);
        assert_eq!(not_opened("https://[fe80::1%en0]:8443/"), ZONED);
        assert!(not_opened("http://").contains("is not a web address Syndeo can open"));
        assert!(not_opened("").contains("is not a web address Syndeo can open"));
    }

    /// What another application asks for is never searched for, whatever
    /// the search engine: nothing of it is sent anywhere.
    #[test]
    fn an_application_asking_for_words_or_a_bare_host_is_refused_never_searched() {
        for address in ["how to bake bread", "bread", "example.com", "example.com/a"] {
            let why = not_opened(address);
            assert!(
                why.contains("is not a web address Syndeo can open"),
                "{address}: {why}"
            );
        }
    }

    #[test]
    fn pages_are_given_text_never_markup() {
        assert_eq!(
            escape("<b>\"&\"</b>"),
            "&lt;b&gt;&quot;&amp;&quot;&lt;/b&gt;"
        );
    }

    /// Only the two refusals measured are refusals: a secure connection
    /// that failed after the window vouched for its host, and a load the
    /// window cancelled because macOS pinned its host.
    #[test]
    fn only_the_measured_refusals_are_refusals() {
        let url = "NSURLErrorDomain";
        assert_eq!(refusal(url, -1200, true, false), Some(Refusal::Preloaded));
        assert_eq!(refusal(url, -999, false, true), Some(Refusal::Pinned));
        // Not vouched for: a certificate the window refused, or never saw.
        assert_eq!(refusal(url, -1200, false, false), None);
        // A cancelled load the window did not refuse on macOS's pinning:
        // Stop, or another page.
        assert_eq!(refusal(url, -999, true, false), None);
        for (domain, code) in [
            (url, -1202),
            (url, -1201),
            (url, -1004),
            (url, -1001),
            ("WebKitErrorDomain", -1200),
            ("WebKitErrorDomain", -999),
            ("NSOSStatusErrorDomain", -1200),
        ] {
            assert_eq!(refusal(domain, code, true, true), None, "{domain} {code}");
        }
    }

    /// A refusal is retried directly once; a page already retried is never
    /// retried again, and shows what WebKit says.
    #[test]
    fn a_refusal_is_retried_directly_once_and_never_again() {
        for refused in [Refusal::Preloaded, Refusal::Pinned] {
            assert_eq!(
                after_failure(Some(refused), false),
                AfterFailure::RetryDirect(refused)
            );
            assert_eq!(
                after_failure(Some(refused), true),
                AfterFailure::AsWebKitSays
            );
        }
        assert_eq!(after_failure(None, false), AfterFailure::AsWebKitSays);
    }

    #[test]
    fn a_refusal_that_cannot_be_retried_says_what_macos_did_and_why() {
        for macos in [14, 15] {
            let why = explain(Refusal::Preloaded, "github.com", macos);
            assert!(
                why.starts_with("macOS refused the secure connection to github.com"),
                "{why}"
            );
            assert!(why.contains("HSTS preload list"), "{why}");
            assert!(
                why.contains(&format!("On macOS {macos} and earlier")),
                "{why}"
            );
            assert!(!why.to_lowercase().contains("ssl"), "{why}");
        }
        assert!(!explain(Refusal::Preloaded, "github.com", 26).contains("HSTS"));
        let why = explain(Refusal::Pinned, "www.apple.com", 26);
        assert!(
            why.starts_with("macOS pins www.apple.com to Apple's own certificates"),
            "{why}"
        );
    }

    /// As the Network framework matches an excluded domain, measured: the
    /// domain and the names under it, never a name that only ends the same.
    #[test]
    fn a_domain_sent_directly_covers_itself_and_its_subdomains_only() {
        assert!(covered("github.com", "github.com"));
        assert!(covered("GitHub.com.", "github.com"));
        assert!(covered("gist.github.com", "github.com"));
        assert!(covered("a.b.github.com", "github.com"));
        assert!(!covered("notgithub.com", "github.com"));
        assert!(!covered("github.com.evil.example", "github.com"));
        assert!(!covered("com", "github.com"));
        assert!(!covered("github.com", ""));
        let direct = vec!["github.com".to_string(), "www.apple.com".to_string()];
        assert!(sent_direct("www.apple.com", &direct));
        assert!(!sent_direct("apple.com", &direct));
        assert!(!sent_direct("example.com", &direct));
    }

    /// Only hosts the window vouched for that the proxy said were refused,
    /// and hosts macOS pinned, are sent directly; never one the window did
    /// not vouch for, nor one already sent.
    #[test]
    fn only_refused_hosts_this_window_vouched_for_are_sent_directly() {
        let s = |list: &[&str]| list.iter().map(|h| h.to_string()).collect::<Vec<_>>();
        assert_eq!(
            to_send_direct(
                &s(&[
                    "example.com",
                    "www.google-analytics.com",
                    "github.githubassets.com"
                ]),
                &s(&["www.google-analytics.com", "paypal.com"]),
                &s(&[]),
                &s(&[]),
            ),
            s(&["www.google-analytics.com"]),
            "paypal.com was refused for another window, and is not this one's"
        );
        assert_eq!(
            to_send_direct(
                &s(&["github.com"]),
                &s(&["github.com"]),
                &s(&["www.apple.com"]),
                &s(&["github.com"])
            ),
            s(&["www.apple.com"])
        );
        assert!(to_send_direct(&s(&["example.com"]), &s(&[]), &s(&[]), &s(&[])).is_empty());
    }

    #[test]
    fn a_page_whose_host_is_sent_directly_is_direct_in_the_bar() {
        let direct = vec!["github.com".to_string()];
        assert_eq!(
            route_with("https://github.com/", &direct),
            Some(Route::Direct)
        );
        assert_eq!(
            route_with("https://gist.github.com/x", &direct),
            Some(Route::Direct)
        );
        assert_eq!(
            route_with("https://example.com/", &direct),
            Some(Route::Syndeo)
        );
        assert_eq!(route_with("http://127.0.0.1/", &[]), Some(Route::Direct));
        assert_eq!(route_with("about:blank", &direct), None);
    }

    #[test]
    fn the_failure_page_shows_text_never_markup() {
        let page = failure_page("https://x/<script>", "it said \"no\" & 'why'");
        assert!(page.contains("https://x/&lt;script&gt;"));
        assert!(page.contains("it said &quot;no&quot; &amp; &#39;why&#39;"));
        assert!(!page.contains("<script>"));
    }
}
