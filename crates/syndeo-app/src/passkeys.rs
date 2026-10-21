//! Passkeys: what macOS lets this copy of Syndeo do with them, and saying
//! so, once, at launch.
//!
//! WebKit hands a page's passkey requests (WebAuthn) to macOS itself: it
//! makes an AuthenticationServices request for the page's relying party,
//! with the page's origin in its client data, and macOS shows its own sheet
//! (WebKit's `WebAuthenticatorCoordinatorProxy.mm`). macOS answers such
//! a request, for any site, only from a browser that holds Apple's managed
//! entitlement, [`ENTITLEMENT`], which comes with a provisioning profile.
//! Without it a page's passkey request fails, as it does in any application
//! that embeds a web view. Syndeo is signed with no entitlement and no
//! profile, so today it has none, and passkeys do not work in it.
//!
//! This reads, from the running process's own code signature, whether it
//! holds the entitlement, and logs what that means. Without it, nothing else
//! happens: AuthenticationServices is not asked anything. With it, the one
//! thing asked is whether the person has let Syndeo use their passkeys,
//! which is read and never requested: nothing here shows a prompt, and
//! nothing here touches the keychain.

/// Apple's managed entitlement that lets a web browser make passkey and
/// security-key requests for any relying party, on macOS 13.3 and later.
pub const ENTITLEMENT: &str = "com.apple.developer.web-browser.public-key-credential";

/// Whether the person has let this browser use their passkeys, in the
/// keychain and in other credential managers:
/// `ASAuthorizationWebBrowserPublicKeyCredentialManagerAuthorizationState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorization {
    Authorized,
    Denied,
    NotDetermined,
    /// A value this version of Syndeo does not know.
    Other(isize),
}

impl Authorization {
    /// From AuthenticationServices' raw value.
    pub fn from_raw(raw: isize) -> Self {
        match raw {
            0 => Authorization::Authorized,
            1 => Authorization::Denied,
            2 => Authorization::NotDetermined,
            other => Authorization::Other(other),
        }
    }
}

/// What this copy of Syndeo can do with passkeys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Passkeys {
    /// It does not hold [`ENTITLEMENT`]: a page's passkey requests fail.
    NotEntitled,
    /// It holds it, on a macOS before 13.3, which has no such entitlement.
    TooOld,
    /// It holds it: macOS answers a page's passkey requests with its own
    /// sheet, and the person's choice so far is this.
    Handled(Authorization),
}

/// What this copy can do: `entitled`, from its signature; `recent`, whether
/// macOS is 13.3 or later; `authorization`, asked of AuthenticationServices
/// only when both hold, and never otherwise.
pub fn status(entitled: bool, recent: bool, authorization: impl FnOnce() -> isize) -> Passkeys {
    match (entitled, recent) {
        (false, _) => Passkeys::NotEntitled,
        (true, false) => Passkeys::TooOld,
        (true, true) => Passkeys::Handled(Authorization::from_raw(authorization())),
    }
}

impl Passkeys {
    /// The log's words for it.
    pub fn sentence(self) -> String {
        match self {
            Passkeys::NotEntitled => format!(
                "passkeys: not available in this copy of Syndeo, which does not hold {ENTITLEMENT}; \
                 a page's passkey requests fail, and nothing was asked of anyone"
            ),
            Passkeys::TooOld => format!(
                "passkeys: not available: this copy holds {ENTITLEMENT}, which macOS honours from 13.3"
            ),
            Passkeys::Handled(authorization) => format!(
                "passkeys: macOS answers a page's passkey requests (this copy holds {ENTITLEMENT}); \
                 the person's choice so far: {}",
                match authorization {
                    Authorization::Authorized => "allowed".to_string(),
                    Authorization::Denied => "not allowed".to_string(),
                    Authorization::NotDetermined => "not yet asked".to_string(),
                    Authorization::Other(raw) => format!("unknown ({raw})"),
                }
            ),
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{status, Passkeys, ENTITLEMENT};
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSOperatingSystemVersion, NSProcessInfo, NSString};
    use std::ffi::c_void;
    use std::ptr;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecTaskCreateFromSelf(allocator: *const c_void) -> *mut c_void;
        fn SecTaskCopyValueForEntitlement(
            task: *mut c_void,
            entitlement: *const c_void,
            error: *mut *mut c_void,
        ) -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: *const c_void;
        fn CFEqual(a: *const c_void, b: *const c_void) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    // So that the class below is there to be found: AuthenticationServices
    // is otherwise loaded only once WebKit first needs it.
    #[link(name = "AuthenticationServices", kind = "framework")]
    extern "C" {
        static ASAuthorizationErrorDomain: *const c_void;
    }

    /// Whether the running process's code signature holds `name` as true.
    fn holds(name: &str) -> bool {
        unsafe {
            let task = SecTaskCreateFromSelf(ptr::null());
            if task.is_null() {
                return false;
            }
            // An NSString is a CFString.
            let key = NSString::from_str(name);
            let value = SecTaskCopyValueForEntitlement(
                task,
                Retained::as_ptr(&key).cast(),
                ptr::null_mut(),
            );
            let yes = !value.is_null() && CFEqual(value, kCFBooleanTrue) != 0;
            if !value.is_null() {
                CFRelease(value);
            }
            CFRelease(task);
            yes
        }
    }

    /// Whether macOS is 13.3 or later.
    fn recent() -> bool {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 13,
            minorVersion: 3,
            patchVersion: 0,
        })
    }

    /// AuthenticationServices' answer to whether the person has let this
    /// browser use their passkeys: read, never requested.
    fn authorization() -> isize {
        std::hint::black_box(ptr::addr_of!(ASAuthorizationErrorDomain));
        let Some(class) = AnyClass::get(c"ASAuthorizationWebBrowserPublicKeyCredentialManager")
        else {
            return -1;
        };
        unsafe {
            let manager: Retained<AnyObject> = msg_send![class, new];
            msg_send![&manager, authorizationStateForPlatformCredentials]
        }
    }

    /// What this copy of Syndeo can do with passkeys, logged once.
    pub fn report() -> Passkeys {
        let passkeys = status(holds(ENTITLEMENT), recent(), authorization);
        tracing::info!("{}", passkeys.sentence());
        passkeys
    }
}

#[cfg(target_os = "macos")]
pub use macos::report;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entitlement_is_apples_browser_passkey_entitlement_exactly() {
        assert_eq!(
            ENTITLEMENT,
            "com.apple.developer.web-browser.public-key-credential"
        );
    }

    /// Without the entitlement, AuthenticationServices is asked nothing at
    /// all, so nothing can prompt; on a macOS before 13.3, neither.
    #[test]
    fn without_the_entitlement_nothing_is_asked() {
        let never = || -> isize { panic!("AuthenticationServices was asked") };
        assert_eq!(status(false, true, never), Passkeys::NotEntitled);
        assert_eq!(status(false, false, never), Passkeys::NotEntitled);
        assert_eq!(status(true, false, never), Passkeys::TooOld);
    }

    #[test]
    fn with_it_the_persons_choice_is_read() {
        assert_eq!(
            status(true, true, || 0),
            Passkeys::Handled(Authorization::Authorized)
        );
        assert_eq!(
            status(true, true, || 1),
            Passkeys::Handled(Authorization::Denied)
        );
        assert_eq!(
            status(true, true, || 2),
            Passkeys::Handled(Authorization::NotDetermined)
        );
        assert_eq!(
            status(true, true, || -1),
            Passkeys::Handled(Authorization::Other(-1))
        );
    }

    /// What CI's app test looks for in the log of a copy without the
    /// entitlement, which every copy CI builds is.
    #[test]
    fn the_log_says_plainly_what_this_copy_can_do() {
        assert_eq!(
            Passkeys::NotEntitled.sentence(),
            "passkeys: not available in this copy of Syndeo, which does not hold \
             com.apple.developer.web-browser.public-key-credential; a page's passkey requests \
             fail, and nothing was asked of anyone"
        );
        assert!(Passkeys::Handled(Authorization::NotDetermined)
            .sentence()
            .ends_with("the person's choice so far: not yet asked"));
        assert!(Passkeys::TooOld.sentence().contains("from 13.3"));
    }

    /// Nothing here requests the person's permission: macOS's own sheet asks
    /// what it needs to when a page makes a request. Nor does anything here
    /// touch the keychain.
    #[test]
    fn nothing_here_prompts_or_touches_the_keychain() {
        let code = include_str!("passkeys.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for never in [
            "requestAuthorizationForPublicKeyCredentials",
            "SecItem",
            "SecKeychain",
            "ASAuthorizationController",
        ] {
            assert!(!code.contains(never), "passkeys.rs has {never}");
        }
    }
}
