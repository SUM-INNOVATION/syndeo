//! What the operating system will tell us about the login session.
//!
//! Only one question is asked, and only where it can be answered honestly: is
//! the screen locked? `None` means the platform does not say, which is treated
//! as "not locked" — the idle timeout is what covers those platforms, and
//! guessing "locked" would lock a working session for no reason.

/// Whether the console session's screen is currently locked.
#[cfg(target_os = "macos")]
pub fn screen_is_locked() -> Option<bool> {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    // Declared here rather than pulled in through a binding crate: this is the
    // only symbol we want out of CoreGraphics, and its signature has not moved
    // since it was introduced.
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGSessionCopyCurrentDictionary() -> core_foundation::dictionary::CFDictionaryRef;
    }

    // The dictionary is absent when there is no window server session at all —
    // a daemon, or an ssh login — and then there is no screen to be locked.
    let dictionary: CFDictionary<CFString, CFType> = unsafe {
        let raw = CGSessionCopyCurrentDictionary();
        if raw.is_null() {
            return None;
        }
        CFDictionary::wrap_under_create_rule(raw)
    };

    let key = CFString::from_static_string("CGSSessionScreenIsLocked");
    match dictionary.find(&key) {
        Some(value) => value
            .downcast::<core_foundation::boolean::CFBoolean>()
            .map(|b| b.into()),
        // The key is simply absent while the screen is unlocked.
        None => Some(false),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn screen_is_locked() -> Option<bool> {
    // Linux would be a logind `LockedHint` over D-Bus and Windows a session
    // notification; neither is wired, so neither is claimed.
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn asking_is_always_safe_and_never_says_locked_while_a_test_runs() {
        // The point of this test is that the platform call does not panic, does
        // not leak, and does not report a locked screen on a machine that is
        // running the suite. `None` is a legitimate answer on a build machine
        // with no window server.
        assert_ne!(super::screen_is_locked(), Some(true));
    }
}
