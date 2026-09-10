use std::io;

/// Everything the cache can fail at.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("index: {0}")]
    Index(String),
    #[error("codec: {0}")]
    Codec(String),
    #[error("blob {0} is missing from the store")]
    MissingBlob(String),
    #[error("integrity check failed: expected {expected}, computed {actual}")]
    Integrity { expected: String, actual: String },
    #[error("malformed integrity metadata: {0}")]
    BadIntegrity(String),
    #[error(
        "the cache index was written under schema {} but this build understands {expected}",
        match found { Some(v) => v.to_string(), None => "an unversioned layout".to_string() }
    )]
    SchemaMismatch { found: Option<u64>, expected: u64 },
}

pub type Result<T> = std::result::Result<T, CacheError>;

macro_rules! index_err {
    ($t:ty) => {
        impl From<$t> for CacheError {
            fn from(e: $t) -> Self {
                CacheError::Index(e.to_string())
            }
        }
    };
}

index_err!(redb::Error);
index_err!(redb::DatabaseError);
index_err!(redb::TransactionError);
index_err!(redb::TableError);
index_err!(redb::StorageError);
index_err!(redb::CommitError);

impl From<bincode::Error> for CacheError {
    fn from(e: bincode::Error) -> Self {
        CacheError::Codec(e.to_string())
    }
}
