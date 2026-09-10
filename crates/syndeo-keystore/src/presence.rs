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
//!   not by us, and a biometric failure falls back to the device password by the
//!   operating system's rules rather than by ours. This is implemented, in
//!   [`crate::enclave`]; whether it is *in force* depends on the build, for the
//!   reason below.
//! * **Shell confirmation.** The shell shows the payload and the user agrees to
//!   that exact payload. This is enforced always, by the confirmation MAC in
//!   `syndeo-ipc`, and it is a property of our own code.
//!
//! **Why platform presence may still be off.** The attributes only mean anything
//! in macOS's *data protection* keychain, and that is only reachable by a binary
//! signed with a keychain access group entitlement. A `cargo build` binary is
//! not, so it gets `errSecMissingEntitlement`, falls back to the ordinary
//! keychain, and reports presence as unenforced. To turn it on, sign the
//! keystore binary with an entitlements file granting `keychain-access-groups`
//! — `crates/syndeo-keystore/Syndeo.entitlements`, and with a real signing
//! identity, since ad-hoc signing with that entitlement produces a binary the
//! kernel kills at launch. Nothing in the code changes, [`available`] starts
//! returning true, and [`policy`] relaxes the passphrase requirement on its own.
//!
//! Where platform presence is not enforced, the rule is that the passphrase
//! stops being optional. [`policy`] is what implements that.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// True when the operating system, not our process, gates access to the
    /// wrapping key on a biometric or device password.
    pub platform_presence: bool,
    /// True when a passphrase must be supplied to unseal.
    pub passphrase_required: bool,
}

/// Whether *this build* can have the platform enforce presence.
///
/// Not whether the platform could in principle, and not whether any particular
/// key is gated — for that, ask the store, because a key already written to the
/// ordinary keychain is not gated no matter what this build is capable of.
/// Deliberately conservative: true only when an enrolment carrying the presence
/// attributes was actually accepted.
pub fn available() -> bool {
    #[cfg(target_os = "macos")]
    {
        crate::enclave::EnclaveItem::presence_can_be_enforced()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux's Secret Service and Windows' Credential Manager have their own
        // equivalents; neither is wired, so neither is claimed, and the
        // passphrase stays mandatory there.
        false
    }
}

/// Given whether the platform is enforcing presence on the wrapping key, and
/// whether the user set a passphrase, what the keystore requires.
pub fn policy(platform_presence: bool, user_set_a_passphrase: bool) -> Policy {
    Policy {
        platform_presence,
        // Biometric first, passphrase as the second factor — but where the
        // platform is not enforcing presence, the passphrase is the only factor
        // and so becomes mandatory.
        passphrase_required: !platform_presence || user_set_a_passphrase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_platform_presence_a_passphrase_is_mandatory() {
        assert!(policy(false, false).passphrase_required);
        assert!(policy(false, true).passphrase_required);
    }

    #[test]
    fn with_platform_presence_the_passphrase_is_the_users_choice() {
        assert!(
            !policy(true, false).passphrase_required,
            "the Enclave is the factor; a passphrase on top is optional"
        );
        assert!(
            policy(true, true).passphrase_required,
            "a user who set one still has to supply it"
        );
    }

    /// Whether presence is available depends on how this binary was built and
    /// signed, so the test asserts consistency rather than an answer — a
    /// capability that flickers would make the passphrase requirement flicker.
    #[test]
    fn the_platform_answer_is_stable_within_a_process() {
        assert_eq!(available(), available());
    }
}
