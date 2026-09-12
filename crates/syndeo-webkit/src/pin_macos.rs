//! Trusting the proxy's authority here, and nowhere else.
//!
//! Caching HTTPS means terminating it, so `syndeo-proxy` presents a certificate
//! it issued itself and the web view has to accept that issuer. The obvious way
//! is to install the authority in the system trust store, and it is the wrong
//! way: an authority every application on the machine trusts, sitting in a file
//! on disk, is a key worth stealing — whoever takes it can impersonate any site
//! to anything, not just to this browser.
//!
//! So it is pinned instead. This process trusts that one issuer, evaluated as
//! the *only* anchor, and the system trust store is never touched. Hostname
//! verification still applies: this accepts a certificate for `example.com`
//! issued by our authority, and refuses one for `example.com` issued by
//! anybody else — including the real web PKI, which is the point. A response
//! that did not come through our proxy does not get in.
//!
//! Implemented against the delegate rather than the data store because there is
//! nowhere else to put it: `WKWebsiteDataStore` will route a web view through a
//! proxy but has no say in what that proxy is allowed to present, and
//! `WKURLSchemeHandler` refuses `http` and `https` outright.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSString, NSURLAuthenticationChallenge, NSURLCredential};
use objc2_web_kit::{WKNavigationDelegate, WKWebView};
use security_framework::certificate::SecCertificate;
use security_framework::trust::SecTrust;
use std::ptr::NonNull;

/// `NSURLSessionAuthChallengeUseCredential`.
const USE_CREDENTIAL: isize = 0;
/// `NSURLSessionAuthChallengePerformDefaultHandling`.
const DEFAULT_HANDLING: isize = 1;
/// `NSURLSessionAuthChallengeCancelAuthenticationChallenge`.
const CANCEL: isize = 2;

pub struct PinnerIvars {
    /// The one authority this process will accept, beyond nothing else.
    anchor: SecCertificate,
}

define_class!(
    #[unsafe(super(NSObject))]
    // WebKit calls its delegate on the main thread and the protocol is
    // declared that way, so the class has to be too.
    #[thread_kind = MainThreadOnly]
    #[name = "SyndeoPinnedNavigationDelegate"]
    #[ivars = PinnerIvars]
    pub struct Pinner;

    unsafe impl NSObjectProtocol for Pinner {}

    unsafe impl WKNavigationDelegate for Pinner {
        /// Decide whether to believe the certificate on the other end.
        ///
        /// Everything that is not a server-trust challenge is handed back to
        /// the default handling: this exists to trust one issuer, not to become
        /// the authentication policy for the whole web view.
        #[unsafe(method(webView:didReceiveAuthenticationChallenge:completionHandler:))]
        fn did_receive_challenge(
            &self,
            _webview: &WKWebView,
            challenge: &NSURLAuthenticationChallenge,
            handler: &block2::DynBlock<dyn Fn(isize, *mut AnyObject)>,
        ) {
            let (disposition, credential) = self.judge(challenge);
            handler.call((disposition, credential));
        }
    }
);

impl Pinner {
    /// Read the authority from a PEM file and build a delegate that pins it.
    pub fn new(mtm: MainThreadMarker, pem: &str) -> anyhow::Result<Retained<Self>> {
        let der = der_from_pem(pem)?;
        let anchor = SecCertificate::from_der(&der)
            .map_err(|err| anyhow::anyhow!("reading the proxy's authority: {err}"))?;
        let this = Self::alloc(mtm).set_ivars(PinnerIvars { anchor });
        Ok(unsafe { msg_send![super(this), init] })
    }

    fn judge(&self, challenge: &NSURLAuthenticationChallenge) -> (isize, *mut AnyObject) {
        let space: Retained<AnyObject> = unsafe { msg_send![challenge, protectionSpace] };
        let method: Retained<NSString> = unsafe { msg_send![&*space, authenticationMethod] };
        if method.to_string() != "NSURLAuthenticationMethodServerTrust" {
            return (DEFAULT_HANDLING, std::ptr::null_mut());
        }

        // `serverTrust` is a `SecTrustRef` — a CoreFoundation type rather than
        // an Objective-C one — so it arrives as a pointer and is wrapped under
        // the get rule: the challenge owns it, and we are only borrowing it for
        // the length of this decision.
        let trust_ptr: *mut std::ffi::c_void = unsafe { msg_send![&*space, serverTrust] };
        let Some(trust_ptr) = NonNull::new(trust_ptr) else {
            return (CANCEL, std::ptr::null_mut());
        };
        let mut trust = unsafe {
            use core_foundation::base::TCFType;
            SecTrust::wrap_under_get_rule(trust_ptr.as_ptr().cast())
        };

        // Our authority, and *only* our authority. Without the second call the
        // system anchors stay in play and this would accept the real web PKI
        // too, which would mean accepting a response that never went through
        // the proxy.
        if trust
            .set_anchor_certificates(&[self.ivars().anchor.clone()])
            .is_err()
            || trust.set_trust_anchor_certificates_only(true).is_err()
        {
            return (CANCEL, std::ptr::null_mut());
        }

        match trust.evaluate_with_error() {
            Ok(()) => {
                let credential: Retained<NSURLCredential> = unsafe {
                    msg_send![<NSURLCredential as objc2::ClassType>::class(), credentialForTrust: trust_ptr.as_ptr()]
                };
                (USE_CREDENTIAL, Retained::into_raw(credential).cast())
            }
            Err(err) => {
                // Refused rather than ignored. A certificate this process
                // cannot trace to the proxy's authority is one it should not
                // believe, and saying so beats a blank page with no reason.
                tracing::warn!(%err, host = %host_of(&space), "refused a certificate");
                (CANCEL, std::ptr::null_mut())
            }
        }
    }
}

/// The delegate, as the protocol object WebKit wants.
pub fn as_delegate(
    pinner: &Retained<Pinner>,
) -> Retained<ProtocolObject<dyn WKNavigationDelegate>> {
    ProtocolObject::from_retained(pinner.clone())
}

fn host_of(space: &AnyObject) -> String {
    let host: Retained<NSString> = unsafe { msg_send![space, host] };
    host.to_string()
}

/// The DER inside a PEM, without pulling in a certificate parser to do it.
fn der_from_pem(pem: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----") && !line.trim().is_empty())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .map_err(|err| anyhow::anyhow!("the authority is not valid PEM: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_decodes_to_der_and_rubbish_is_refused() {
        // A DER certificate starts with a SEQUENCE tag; this is only checking
        // the envelope was stripped, not that the contents are a certificate.
        let pem = "-----BEGIN CERTIFICATE-----\nMIIBIjAN\n-----END CERTIFICATE-----\n";
        let der = der_from_pem(pem).unwrap();
        assert_eq!(der[0], 0x30, "DER should begin with a SEQUENCE tag");

        assert!(der_from_pem("-----BEGIN CERTIFICATE-----\n!!!!\n").is_err());
    }
}
