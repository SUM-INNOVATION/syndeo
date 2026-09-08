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
    #[error("invalid url {0}")]
    InvalidUrl(String),
    #[error("dns: {0}")]
    Dns(String),
    #[error("tls: {0}")]
    Tls(String),
    #[error("body exceeds the {limit} byte ceiling")]
    BodyTooLarge { limit: u64 },
    #[error("more than {limit} redirects")]
    TooManyRedirects { limit: u8 },
    #[error("redirect loop back to {0}")]
    RedirectLoop(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, NetError>;
