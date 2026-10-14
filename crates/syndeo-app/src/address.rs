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
    /// By WebKit directly, not through the proxy, so Syndeo does not cache it.
    Direct,
}

impl Route {
    /// What the bar says.
    pub fn label(self) -> &'static str {
        match self {
            Route::Syndeo => "Through Syndeo",
            Route::Direct => "Direct — not cached by Syndeo",
        }
    }
}

/// The IPv4 ranges WebKit was measured opening directly, never through the
/// proxy it is configured with: loopback, the private ranges, link-local and
/// shared (carrier-grade NAT) space. Measured on CI in the runs recorded in
/// Phase 3 of the desktop plan; `ci/test-app.sh routes` measures them again,
/// with each range's edges, and fails when the bar disagrees.
const DIRECT_V4: [(Ipv4Addr, u8); 6] = [
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
];

/// The IPv6 ranges measured the same way: loopback, unique local and
/// link-local.
const DIRECT_V6: [(Ipv6Addr, u8); 3] = [
    (Ipv6Addr::LOCALHOST, 128),
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
];

/// How WebKit reaches the page at `url`, as measured: directly when its host
/// is exactly `localhost` or an address in one of the ranges above, and
/// through the proxy otherwise — every other name, whatever it resolves to
/// (a `.local` name, one under `.localhost`, a single label, a public name
/// for this Mac), and every public address. None for a page that is not the
/// web's, which is not reached at all.
///
/// This is the page's own route. What it loads — images, scripts, frames,
/// and where a redirect leads — is each routed by its own address.
pub fn route(url: &Url) -> Option<Route> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let direct = match url.host()? {
        Host::Domain(name) => name == "localhost",
        Host::Ipv4(ip) => DIRECT_V4.iter().any(|&(network, length)| {
            let mask = u32::MAX.checked_shl(32 - u32::from(length)).unwrap_or(0);
            u32::from(ip) & mask == u32::from(network) & mask
        }),
        Host::Ipv6(ip) => DIRECT_V6.iter().any(|&(network, length)| {
            let mask = u128::MAX.checked_shl(128 - u32::from(length)).unwrap_or(0);
            u128::from(ip) & mask == u128::from(network) & mask
        }),
    };
    Some(if direct { Route::Direct } else { Route::Syndeo })
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
            other => Typed::Refused(format!(
                "Syndeo opens http and https addresses, not {other}: ones."
            )),
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

    /// What was measured on CI: each address typed, and whether WebKit
    /// reached it directly or through the proxy.
    #[test]
    fn the_bar_says_what_was_measured() {
        for direct in [
            "http://localhost:8080/",
            "http://127.0.0.1:8080/",
            "http://[::1]:8080/",
            "http://192.168.64.8:8080/",
            "http://10.255.42.1:8080/",
            "http://172.31.42.1:8080/",
            "http://169.254.42.1:8080/",
            "http://100.64.42.1:8080/",
            "http://[fd42:5a7e:6e5d::1]:8080/",
            "http://[fe80::1]:8080/",
            "https://LOCALHOST/",
        ] {
            assert_eq!(route_shown(direct), Some(Route::Direct), "{direct}");
        }
        for through in [
            "http://example.com/",
            "https://example.com/",
            "http://104.20.23.154/",
            "http://syndeoprobe:8080/",
            "http://syndeo-probe.localhost:8080/",
            "http://runner.local:8080/",
            "http://127.0.0.1.nip.io:8080/",
            "http://localhost.:8080/",
            "http://syndeo.local/stats",
        ] {
            assert_eq!(route_shown(through), Some(Route::Syndeo), "{through}");
        }
        for not_the_web in ["about:blank", "file:///etc/passwd", "data:text/html,x"] {
            assert_eq!(route_shown(not_the_web), None, "{not_the_web}");
        }
        assert_eq!(Route::Direct.label(), "Direct — not cached by Syndeo");
        assert_eq!(Route::Syndeo.label(), "Through Syndeo");
    }

    /// Each range is exactly as wide as it says: its first and last
    /// addresses are direct, and the addresses either side of it are not.
    #[test]
    fn each_direct_range_ends_where_it_says() {
        for (inside, outside) in [
            (
                ["127.0.0.0", "127.255.255.255"],
                ["126.255.255.255", "128.0.0.0"],
            ),
            (
                ["10.0.0.0", "10.255.255.255"],
                ["9.255.255.255", "11.0.0.0"],
            ),
            (
                ["172.16.0.0", "172.31.255.255"],
                ["172.15.255.255", "172.32.0.0"],
            ),
            (
                ["192.168.0.0", "192.168.255.255"],
                ["192.167.255.255", "192.169.0.0"],
            ),
            (
                ["169.254.0.0", "169.254.255.255"],
                ["169.253.255.255", "169.255.0.0"],
            ),
            (
                ["100.64.0.0", "100.127.255.255"],
                ["100.63.255.255", "100.128.0.0"],
            ),
            (
                ["[fc00::]", "[fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]"],
                ["[fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]", "[fe00::]"],
            ),
            (
                ["[fe80::]", "[febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff]"],
                ["[fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff]", "[fec0::]"],
            ),
            (["[::1]", "[::1]"], ["[::]", "[::2]"]),
        ] {
            for host in inside {
                let address = format!("http://{host}/");
                assert_eq!(route_shown(&address), Some(Route::Direct), "{address}");
            }
            for host in outside {
                let address = format!("http://{host}/");
                assert_eq!(route_shown(&address), Some(Route::Syndeo), "{address}");
            }
        }
        // An IPv4 address carried in IPv6 was not measured, so the bar does
        // not say it is direct.
        assert_eq!(
            route_shown("http://[::ffff:127.0.0.1]/"),
            Some(Route::Syndeo)
        );
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
