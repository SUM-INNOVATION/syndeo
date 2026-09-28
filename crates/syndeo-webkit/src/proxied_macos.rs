//! The proxy every web view here uses, and the credential it answers with.
//!
//! wry can set a proxy on the data store it builds a web view with, but only a
//! host and a port: WebKit's proxy configuration also takes a username and
//! password, and wry has nowhere to put them. So the configuration is built
//! here instead — the data store the web view will use, with the authenticated
//! proxy set on it — and handed to wry before any web view exists, so the
//! first request any of them makes already knows the credential.
//!
//! WebKit sends the credential when the proxy answers 407, for `CONNECT` to
//! either scheme: a proxy configured this way relays TCP only, so plain
//! `http://` goes through a tunnel too.

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::MainThreadMarker;
use objc2_foundation::{ns_string, NSArray, NSObjectNSKeyValueCoding};
use objc2_web_kit::{WKWebViewConfiguration, WKWebsiteDataStore};
use std::ffi::{c_char, CString};

/// The username the proxy expects. Not a secret; the token is.
pub const USER: &str = "syndeo";

/// A per-launch credential for the proxy this browser starts.
///
/// Generated here, handed to the proxy once on its stdin, and set on the web
/// views' data store. It is never an argument, never in the environment, never
/// logged, and it is zeroed when it goes.
pub struct ProxyCredential {
    /// The token as lowercase hex, which is the password.
    password: String,
}

impl std::fmt::Debug for ProxyCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProxyCredential(redacted)")
    }
}

impl Drop for ProxyCredential {
    fn drop(&mut self) {
        // SAFETY: zeroes are valid UTF-8, and the string is not used again.
        unsafe { self.password.as_mut_vec() }.fill(0);
    }
}

impl ProxyCredential {
    /// A fresh credential, and the frame that hands it to `syndeo-proxy run
    /// --auth-stdin`: `SYA1`, the 32-byte token, a newline.
    pub fn generate() -> Result<(Self, Vec<u8>)> {
        use std::io::Read;
        let mut token = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut token))
            .context("reading randomness for the proxy's credential")?;
        let password = token.iter().map(|b| format!("{b:02x}")).collect();
        let mut frame = Vec::with_capacity(37);
        frame.extend_from_slice(b"SYA1");
        frame.extend_from_slice(&token);
        frame.push(b'\n');
        token.fill(0);
        Ok((ProxyCredential { password }, frame))
    }

    /// For tests that need to present the same credential themselves.
    #[doc(hidden)]
    pub fn password_for_tests(&self) -> &str {
        &self.password
    }
}

/// Where the web views send their traffic, and with what.
#[derive(Debug)]
pub struct ProxyTarget {
    pub host: String,
    pub port: String,
    /// `Some` for the proxy this browser started. `None` for one named with
    /// `--proxy`, which is somebody else's and asks for nothing.
    pub credential: Option<ProxyCredential>,
}

#[link(name = "Network", kind = "framework")]
extern "C" {
    fn nw_endpoint_create_host(hostname: *const c_char, port: *const c_char) -> *mut NSObject;
    fn nw_proxy_config_create_http_connect(
        endpoint: *mut NSObject,
        tls_options: *mut NSObject,
    ) -> *mut NSObject;
    fn nw_proxy_config_set_username_and_password(
        config: *mut NSObject,
        username: *const c_char,
        password: *const c_char,
    );
}

/// The data store every web view in this process uses: the one wry would have
/// chosen itself, so nothing about storage changes but the proxy.
pub fn data_store(mtm: MainThreadMarker) -> Retained<WKWebsiteDataStore> {
    unsafe { WKWebsiteDataStore::defaultDataStore(mtm) }
}

/// A web view configuration whose data store sends everything through
/// `target`, answering its 407 with the credential when there is one.
///
/// Built before the web view that uses it exists, which is what makes the
/// credential available to its very first request.
pub fn configuration(
    target: &ProxyTarget,
    mtm: MainThreadMarker,
) -> Result<Retained<WKWebViewConfiguration>> {
    let proxy = proxy_config(target)?;
    let store = data_store(mtm);
    let proxies: Retained<NSArray<NSObject>> = NSArray::from_retained_slice(&[proxy]);
    unsafe { store.setValue_forKey(Some(&proxies), ns_string!("proxyConfigurations")) };
    let configuration = unsafe { WKWebViewConfiguration::new(mtm) };
    unsafe { configuration.setWebsiteDataStore(&store) };
    Ok(configuration)
}

/// The Network framework's proxy configuration for `target`.
fn proxy_config(target: &ProxyTarget) -> Result<Retained<NSObject>> {
    let host = CString::new(target.host.as_str()).context("the proxy's host")?;
    let port = CString::new(target.port.as_str()).context("the proxy's port")?;
    // Both functions return a retained object, as `Retained::from_raw` expects.
    let endpoint =
        unsafe { Retained::from_raw(nw_endpoint_create_host(host.as_ptr(), port.as_ptr())) }
            .context("the proxy's address was not usable")?;
    let proxy = unsafe {
        Retained::from_raw(nw_proxy_config_create_http_connect(
            Retained::as_ptr(&endpoint) as *mut NSObject,
            std::ptr::null_mut(),
        ))
    }
    .context("could not configure the proxy")?;
    if let Some(credential) = &target.credential {
        let user = CString::new(USER).expect("a constant without NUL");
        let password = CString::new(credential.password.as_str()).expect("hex has no NUL in it");
        unsafe {
            nw_proxy_config_set_username_and_password(
                Retained::as_ptr(&proxy) as *mut NSObject,
                user.as_ptr(),
                password.as_ptr(),
            )
        };
        let mut bytes = password.into_bytes();
        bytes.fill(0);
    }
    Ok(proxy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_is_a_fresh_token_framed_for_the_proxy_and_never_printed() {
        let (first, frame) = ProxyCredential::generate().unwrap();
        let (second, _) = ProxyCredential::generate().unwrap();
        assert_eq!(frame.len(), 37);
        assert_eq!(&frame[..4], b"SYA1");
        assert_eq!(frame[36], b'\n');
        let hex: String = frame[4..36].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(first.password, hex);
        assert_ne!(first.password, second.password);
        assert_eq!(format!("{first:?}"), "ProxyCredential(redacted)");
        let target = ProxyTarget {
            host: "127.0.0.1".into(),
            port: "1".into(),
            credential: Some(first),
        };
        assert!(!format!("{target:?}").contains(&hex));
    }
}
