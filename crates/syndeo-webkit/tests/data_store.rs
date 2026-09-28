//! The web view's data store carries the proxy and its credential before the
//! web view exists, and WebKit answers the proxy's 407 with that credential.
//!
//! A real WKWebView, built from the same configuration function the browser
//! uses, loading a page through a local proxy that refuses anything without
//! the credential. It needs the main thread, so this is not a libtest harness,
//! and it uses this user's default WebKit data store — which is what the
//! browser uses, and so what has to be tested — so it runs only where
//! SYNDEO_WEBKIT_UI_TEST is set: CI, on a disposable machine.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    if std::env::var("SYNDEO_WEBKIT_UI_TEST").as_deref() != Ok("1") {
        println!(
            "data_store: skipped; it drives a real WKWebView against this user's default \
             WebKit data store, so it runs where SYNDEO_WEBKIT_UI_TEST is set (CI)"
        );
        return;
    }
    ui::run();
    println!("data_store: ok");
}

#[cfg(target_os = "macos")]
mod ui {
    use base64::Engine;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
    use objc2_foundation::{
        ns_string, NSArray, NSDate, NSObjectNSKeyValueCoding, NSRect, NSRunLoop, NSString,
        NSURLRequest, NSURL,
    };
    use objc2_web_kit::WKWebView;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use syndeo_webkit::proxied::{self, ProxyCredential, ProxyTarget};

    #[derive(Debug, Clone, PartialEq)]
    enum Seen {
        /// A request to the proxy without the right credential, answered 407.
        Refused { method: String },
        /// A CONNECT with the right credential, answered 200.
        Tunnel { target: String },
        /// A request inside an authenticated tunnel.
        Inside { path: String },
    }

    fn read_head(stream: &mut TcpStream) -> Option<String> {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).ok()? == 0 {
                return None;
            }
            head.push(byte[0]);
            if head.len() > 64 * 1024 {
                return None;
            }
        }
        Some(String::from_utf8_lossy(&head).to_string())
    }

    /// A proxy that answers 407 until it is shown exactly `expected`, on the
    /// same connection, and serves a page through the tunnel after that.
    fn proxy(expected: String, seen: Arc<Mutex<Vec<Seen>>>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let expected = expected.clone();
                let seen = seen.clone();
                std::thread::spawn(move || loop {
                    let Some(head) = read_head(&mut stream) else {
                        return;
                    };
                    let mut words = head.split_whitespace();
                    let method = words.next().unwrap_or("").to_string();
                    let target = words.next().unwrap_or("").to_string();
                    let presented: Vec<&str> = head
                        .lines()
                        .filter_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("proxy-authorization")
                                .then(|| value.trim())
                        })
                        .collect();
                    if presented != [expected.as_str()] || method != "CONNECT" {
                        seen.lock().unwrap().push(Seen::Refused { method });
                        let _ = stream.write_all(
                            b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                              Proxy-Authenticate: Basic realm=\"syndeo\"\r\n\
                              Content-Length: 0\r\n\r\n",
                        );
                        continue;
                    }
                    seen.lock().unwrap().push(Seen::Tunnel {
                        target: target.clone(),
                    });
                    let _ = stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                    let Some(inner) = read_head(&mut stream) else {
                        return;
                    };
                    let path = inner.split_whitespace().nth(1).unwrap_or("").to_string();
                    seen.lock().unwrap().push(Seen::Inside { path });
                    let page = b"<!doctype html><title>ok</title>ok";
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            page.len()
                        )
                        .as_bytes(),
                    );
                    let _ = stream.write_all(page);
                    return;
                });
            }
        });
        port
    }

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("this test runs on the main thread");
        unsafe {
            let app: *mut AnyObject = msg_send![objc2::class!(NSApplication), sharedApplication];
            // NSApplicationActivationPolicyProhibited: no Dock icon, no menu.
            let _: bool = msg_send![app, setActivationPolicy: 2isize];
        }

        let (credential, _frame) = ProxyCredential::generate().unwrap();
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                proxied::USER,
                credential.password_for_tests()
            ))
        );
        let password = credential.password_for_tests().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let port = proxy(expected, seen.clone());

        let target = ProxyTarget {
            host: "127.0.0.1".into(),
            port: port.to_string(),
            credential: Some(credential),
        };
        let configuration = proxied::configuration(&target, mtm).unwrap();

        // Before any web view exists: the configuration names the production
        // data store, and that store holds exactly one proxy configuration.
        let store = proxied::data_store(mtm);
        let configured = unsafe { configuration.websiteDataStore() };
        assert!(
            Retained::as_ptr(&configured) == Retained::as_ptr(&store),
            "the configuration does not use the browser's data store"
        );
        let proxies = store
            .valueForKey(ns_string!("proxyConfigurations"))
            .expect("the data store has proxy configurations");
        let proxies = proxies
            .downcast::<NSArray>()
            .expect("proxy configurations are an array");
        assert_eq!(proxies.count(), 1);

        let webview = unsafe {
            WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::ZERO,
                &configuration,
            )
        };
        let used = unsafe { webview.configuration().websiteDataStore() };
        assert!(Retained::as_ptr(&used) == Retained::as_ptr(&store));

        let url =
            NSURL::URLWithString(&NSString::from_str("http://syndeo-ui-test.test/page")).unwrap();
        let request = NSURLRequest::requestWithURL(&url);
        unsafe { webview.loadRequest(&request) };

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let reached = seen
                .lock()
                .unwrap()
                .iter()
                .any(|s| matches!(s, Seen::Inside { .. }));
            if reached {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the page never came through the authenticated tunnel: {:?}",
                seen.lock().unwrap()
            );
            let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
            NSRunLoop::currentRunLoop().runUntilDate(&until);
        }

        let seen = seen.lock().unwrap().clone();
        assert!(
            seen.contains(&Seen::Tunnel {
                target: "syndeo-ui-test.test:80".into()
            }),
            "{seen:?}"
        );
        assert!(
            seen.contains(&Seen::Inside {
                path: "/page".into()
            }),
            "{seen:?}"
        );

        drop(webview);
        assert_nothing_kept(&password);
    }

    /// The credential is not left in the keychain or in WebKit's files.
    fn assert_nothing_kept(password: &str) {
        let keychain = std::process::Command::new("/usr/bin/security")
            .args(["find-internet-password", "-a", proxied::USER])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(
            !keychain.success(),
            "a proxy credential was stored in the keychain"
        );

        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        for root in ["Library/WebKit", "Library/Caches", "Library/HTTPStorages"] {
            let found = find(&home.join(root), password.as_bytes());
            assert!(found.is_none(), "the credential was written to {found:?}");
        }
    }

    fn find(dir: &std::path::Path, needle: &[u8]) -> Option<std::path::PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if let Some(found) = find(&path, needle) {
                    return Some(found);
                }
            } else if kind.is_file() {
                let small = entry
                    .metadata()
                    .map(|m| m.len() < 16 << 20)
                    .unwrap_or(false);
                if small {
                    if let Ok(bytes) = std::fs::read(&path) {
                        if bytes.windows(needle.len()).any(|w| w == needle) {
                            return Some(path);
                        }
                    }
                }
            }
        }
        None
    }
}
