//! Favorites: the few sites kept at the top of the sidebar, as Arc keeps
//! them, each a tile with the site's icon, or its initial when the icon is
//! not to hand. Plain Rust: what is kept, as JSON, and nothing of how it is
//! shown (`sidebar_macos`) or stored (the application's defaults).

use serde_json::{json, Value};

/// How many there can be: Arc's twelve, three rows of four.
pub const MOST: usize = 12;

/// A favorite: a page's address, and the title it had when it was added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Favorite {
    pub address: String,
    pub title: String,
}

/// The favorites in `json`, as `render` wrote them; none for anything else.
pub fn parse(json: &str) -> Vec<Favorite> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let address = item.get("address")?.as_str()?;
            let title = item.get("title").and_then(Value::as_str).unwrap_or("");
            Some(Favorite {
                address: address.to_string(),
                title: title.to_string(),
            })
        })
        .collect()
}

/// `favorites`, as JSON for `parse`.
pub fn render(favorites: &[Favorite]) -> String {
    Value::Array(
        favorites
            .iter()
            .map(|f| json!({"address": f.address, "title": f.title}))
            .collect(),
    )
    .to_string()
}

/// Add a page to `favorites`, at the end; not one already there, and not
/// past `MOST`. Whether it was added.
pub fn add(favorites: &mut Vec<Favorite>, address: &str, title: &str) -> bool {
    if favorites.len() >= MOST || favorites.iter().any(|f| f.address == address) {
        return false;
    }
    favorites.push(Favorite {
        address: address.to_string(),
        title: title.to_string(),
    });
    true
}

/// Take a page out of `favorites`. Whether it was there.
pub fn remove(favorites: &mut Vec<Favorite>, address: &str) -> bool {
    let before = favorites.len();
    favorites.retain(|f| f.address != address);
    favorites.len() != before
}

/// The host of `address`, without a leading `www.`, lower-cased: what two
/// addresses are the same site by, and what a tile's initial comes from.
pub fn host(address: &str) -> Option<String> {
    let parsed = url::Url::parse(address).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// A tile's initial, when it has no icon: the site's first letter, or the
/// title's, or nothing.
pub fn initial(address: &str, title: &str) -> String {
    host(address)
        .and_then(|h| h.chars().next())
        .or_else(|| title.trim().chars().next())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_keeps_every_favorite() {
        let mut list = Vec::new();
        assert!(add(&mut list, "https://example.com/", "Example"));
        assert!(add(&mut list, "https://www.rust-lang.org/", "Rust"));
        assert!(!add(&mut list, "https://example.com/", "Again"));
        assert_eq!(parse(&render(&list)), list);
    }

    #[test]
    fn no_more_than_most() {
        let mut list = Vec::new();
        for i in 0..MOST {
            assert!(add(&mut list, &format!("https://site{i}.test/"), ""));
        }
        assert!(!add(&mut list, "https://one-too-many.test/", ""));
        assert!(remove(&mut list, "https://site0.test/"));
        assert!(!remove(&mut list, "https://site0.test/"));
        assert_eq!(list.len(), MOST - 1);
    }

    #[test]
    fn the_initial_is_the_sites_first_letter() {
        assert_eq!(initial("https://www.rust-lang.org/learn", "Learn"), "R");
        assert_eq!(initial("not an address", "example"), "E");
        assert_eq!(initial("", "   "), "");
        assert_eq!(
            host("HTTPS://WWW.Example.COM/a"),
            Some("example.com".into())
        );
    }

    #[test]
    fn anything_else_parses_to_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("{}").is_empty());
        assert!(parse("[{\"title\":\"no address\"}]").is_empty());
    }
}
