//! The network process.
//!
//! Rule one of the process model: renderers never talk to the network directly.
//! They send a [`FetchRequest`] and receive a [`FetchResponse`]. Sockets, DNS,
//! certificates and the cache all live behind that call.

pub mod config;
pub mod dns;
pub mod error;
pub mod fetch;
pub mod service;
pub mod tls;

pub use config::NetConfig;
pub use dns::{DnsMode, Resolver};
pub use error::{NetError, Result};
pub use fetch::{FetchRequest, FetchResponse, Net, Source};
