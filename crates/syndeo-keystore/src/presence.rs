//! User presence.
//!
//! The correct equivalent of "requires sudo" for a signing key is not a
//! root-owned file and a setuid helper — that means running the keystore
//! privileged, and macOS caches sudo credentials for five minutes anyway, which
//! is weaker than a per-operation check. It is per-signature user presence.
//!
//! Two things provide it, and they are not the same thing:
//!
//! * **Platform presence.** The Keychain item holding the wrapping key carries
//!   `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` and a `SecAccessControl`
//!   requiring `.userPresence`, so Touch ID is enforced by the Secure Enclave,
//!   not by us. Reaching those attributes needs a direct Security.framework
//!   binding; the `keyring` crate does not expose them, so on this build
//!   [`available`] reports false.
//! * **Shell confirmation.** The shell shows the payload and the user agrees to
//!   that exact payload. This is enforced today, by the confirmation MAC in
//!   `syndeo-ipc`, and it is a property of our own code.
//!
//! Where platform presence is not enforced, the amendment's own rule applies:
//! the passphrase stops being optional. [`policy`] is what implements that, so
//! the moment the binding lands the requirement relaxes on its own.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// True when the operating system, not our process, gates access to the
    /// wrapping key on a biometric or device password.
    pub platform_presence: bool,
    /// True when a passphrase must be supplied to unseal.
    pub passphrase_required: bool,
}

/// Whether the platform enforces presence on the keyring item.
///
/// Deliberately conservative: this returns true only when the enforcement is
/// real, never when it is merely likely.
pub fn available() -> bool {
    // Enabling this requires storing the wrapping key through
    // Security.framework with a SecAccessControl of `.userPresence`, which is
    // the follow-up to this module. Claiming it before it is wired would be
    // claiming a guarantee the Secure Enclave is not actually making.
    false
}

/// Given whether the user set a passphrase, what the keystore requires.
pub fn policy(user_set_a_passphrase: bool) -> Policy {
    let platform_presence = available();
    Policy {
        platform_presence,
        // Biometric first, passphrase as the second factor — but where the
        // platform cannot enforce presence, the passphrase is the only factor
        // and so becomes mandatory.
        passphrase_required: !platform_presence || user_set_a_passphrase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_platform_presence_a_passphrase_is_mandatory() {
        assert!(!available(), "update this test when the binding lands");
        assert!(policy(false).passphrase_required);
        assert!(policy(true).passphrase_required);
    }
}
