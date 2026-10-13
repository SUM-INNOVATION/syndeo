use syndeo_cache::redact::Redacted;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("cache: {0}")]
    Cache(#[from] syndeo_cache::CacheError),
    #[error("http: {0}")]
    Http(#[from] http::Error),
    #[error("hyper: {0}")]
    Hyper(#[from] hyper::Error),
    #[error("transport: {0}")]
    Transport(String),
    #[error("invalid url {}", Redacted(.0))]
    InvalidUrl(String),
    #[error("dns: {0}")]
    Dns(String),
    #[error("tls: {0}")]
    Tls(String),
    #[error("body exceeds the {limit} byte ceiling")]
    BodyTooLarge { limit: u64 },
    #[error("more than {limit} redirects")]
    TooManyRedirects { limit: u8 },
    #[error("redirect loop back to {}", Redacted(.0))]
    RedirectLoop(String),
    #[error("the stored body of {} was lost, and the origin would not send it again", Redacted(.0))]
    LostBody(String),
    #[error("integrity: {0}")]
    Integrity(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, NetError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// An error says which origin it was about, and nothing more of the URL:
    /// errors are logged, and a URL's path and query can hold a token.
    #[test]
    fn errors_name_a_urls_origin_and_nothing_private() {
        let url = "https://user:secret@example.com:8443/reset/private?token=private#private";
        for err in [
            NetError::InvalidUrl(url.into()),
            NetError::RedirectLoop(url.into()),
            NetError::LostBody(url.into()),
        ] {
            let shown = err.to_string();
            assert!(shown.contains("https://example.com:8443"), "{shown}");
            for private in ["private", "secret", "token", "reset"] {
                assert!(!shown.contains(private), "{private} in {shown}");
            }
        }
        let unreadable = NetError::InvalidUrl("ht!tp://example.com/private".into()).to_string();
        assert_eq!(unreadable, "invalid url <url>");
    }

    /// No log line or message in this crate shows a URL as itself; see
    /// `syndeo_cache::redact::unredacted`.
    #[test]
    fn nothing_in_this_crate_shows_a_url_as_itself() {
        for (file, source) in [
            ("body.rs", include_str!("body.rs")),
            ("config.rs", include_str!("config.rs")),
            ("dns.rs", include_str!("dns.rs")),
            ("error.rs", include_str!("error.rs")),
            ("fetch.rs", include_str!("fetch.rs")),
            ("h3.rs", include_str!("h3.rs")),
            ("lib.rs", include_str!("lib.rs")),
            ("main.rs", include_str!("main.rs")),
            ("service.rs", include_str!("service.rs")),
            ("tls.rs", include_str!("tls.rs")),
        ] {
            let found = syndeo_cache::redact::unredacted(source);
            assert!(found.is_empty(), "{file}: {found:#?}");
        }
    }
}
