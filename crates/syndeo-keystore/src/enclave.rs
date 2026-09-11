//! The wrapping key, held where the Secure Enclave gates it.
//!
//! What this buys over the `keyring` crate is one attribute pair the crate does
//! not expose:
//!
//! * `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` — the item is unreadable
//!   while the machine is locked and never leaves this device, not even into a
//!   Keychain backup.
//! * `SecAccessControl` with `kSecAccessControlUserPresence` — reading it
//!   requires Touch ID, or the device password if biometry fails or is not
//!   enrolled. The fallback is the operating system's, so there is no path
//!   through it that we wrote.
//!
//! Both require the *data protection* keychain on macOS, which in turn requires
//! the binary to be signed with a keychain access group. An unsigned build gets
//! `errSecMissingEntitlement` here, falls back to the ordinary keychain, and
//! reports presence as unenforced — which is the honest answer, and is what
//! keeps the passphrase mandatory on that build.

#![cfg(target_os = "macos")]

use crate::wrapping::{Result, WrappingError};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFRelease, CFTypeRef};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::passwords::AccessControlOptions;
use security_framework_sys::base::{errSecItemNotFound, errSecSuccess};
use security_framework_sys::item::{
    kSecAttrAccessControl, kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword,
    kSecReturnAttributes, kSecReturnData, kSecUseDataProtectionKeychain, kSecValueData,
};
use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching, SecItemDelete};
use zeroize::Zeroizing;

/// A generic-password item in the data protection keychain, gated on presence.
pub struct EnclaveItem {
    service: String,
    account: String,
}

impl EnclaveItem {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        EnclaveItem {
            service: service.into(),
            account: account.into(),
        }
    }

    /// Whether this build can enrol an item with the presence attributes at all.
    ///
    /// Answered by actually enrolling a throwaway item and deleting it again,
    /// because the question is not "is this macOS" but "did the operating system
    /// accept these attributes from this binary". Neither adding nor deleting
    /// evaluates the access control, so this never asks the user for anything.
    ///
    /// Probed once per process; the answer cannot change while it runs.
    pub fn presence_can_be_enforced() -> bool {
        static PROBED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *PROBED.get_or_init(|| {
            let probe = EnclaveItem::new(
                "com.sum.syndeo.keystore.probe",
                "presence-capability-probe",
            );
            let _ = probe.delete();
            match probe.store(&[0u8; 32]) {
                Ok(()) => {
                    let _ = probe.delete();
                    true
                }
                Err(err) => {
                    tracing::debug!(%err, "the platform will not enforce user presence for this build");
                    false
                }
            }
        })
    }

    fn access_control() -> Result<SecAccessControl> {
        SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
            AccessControlOptions::USER_PRESENCE.bits(),
        )
        .map_err(|e| WrappingError::Store(format!("access control: {e}")))
    }

    /// Class, service and account — everything that names the item, and nothing
    /// that decides what is returned.
    fn identity(&self) -> Vec<(CFString, CFType)> {
        unsafe {
            vec![
                (
                    CFString::wrap_under_get_rule(kSecClass),
                    CFString::wrap_under_get_rule(kSecClassGenericPassword).into_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrService),
                    CFString::from(self.service.as_str()).into_CFType(),
                ),
                (
                    CFString::wrap_under_get_rule(kSecAttrAccount),
                    CFString::from(self.account.as_str()).into_CFType(),
                ),
                // Without this macOS uses the old file-based keychain, which
                // ignores `kSecAttrAccessControl` entirely — the attributes
                // would be accepted and then not enforced, which is the one
                // outcome worse than not having them.
                (
                    CFString::wrap_under_get_rule(kSecUseDataProtectionKeychain),
                    CFBoolean::from(true).into_CFType(),
                ),
            ]
        }
    }

    pub fn store(&self, key: &[u8; 32]) -> Result<()> {
        // Replacing is delete-then-add: `SecItemUpdate` cannot change an access
        // control, and a half-updated credential is worse than a missing one.
        let _ = self.delete();

        let mut query = self.identity();
        query.push(unsafe {
            (
                CFString::wrap_under_get_rule(kSecAttrAccessControl),
                Self::access_control()?.into_CFType(),
            )
        });
        query.push(unsafe {
            (
                CFString::wrap_under_get_rule(kSecValueData),
                CFData::from_buffer(key).into_CFType(),
            )
        });

        let dictionary = CFDictionary::from_CFType_pairs(&query);
        let status = unsafe { SecItemAdd(dictionary.as_concrete_TypeRef(), std::ptr::null_mut()) };
        if status != errSecSuccess {
            return Err(WrappingError::Store(describe(status)));
        }
        Ok(())
    }

    /// Read the key. **This is the call that asks the user for Touch ID.**
    pub fn load(&self) -> Result<Zeroizing<[u8; 32]>> {
        let mut query = self.identity();
        query.push(unsafe {
            (
                CFString::wrap_under_get_rule(kSecReturnData),
                CFBoolean::from(true).into_CFType(),
            )
        });

        let dictionary = CFDictionary::from_CFType_pairs(&query);
        let mut found: CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(dictionary.as_concrete_TypeRef(), &mut found) };
        if status == errSecItemNotFound {
            return Err(WrappingError::Missing);
        }
        if status != errSecSuccess || found.is_null() {
            return Err(WrappingError::Store(describe(status)));
        }

        let bytes = unsafe {
            let data = CFData::wrap_under_create_rule(found as _);
            let bytes = data.bytes().to_vec();
            drop(data);
            bytes
        };
        let array: [u8; 32] = bytes.try_into().map_err(|_| WrappingError::Malformed)?;
        Ok(Zeroizing::new(array))
    }

    /// Whether the item is there, without reading it.
    ///
    /// Asking for attributes rather than data is what makes this free: the
    /// access control is evaluated when the *data* is returned, so this answers
    /// "is the key enrolled here" without a biometric prompt.
    pub fn exists(&self) -> bool {
        let mut query = self.identity();
        query.push(unsafe {
            (
                CFString::wrap_under_get_rule(kSecReturnAttributes),
                CFBoolean::from(true).into_CFType(),
            )
        });

        let dictionary = CFDictionary::from_CFType_pairs(&query);
        let mut found: CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(dictionary.as_concrete_TypeRef(), &mut found) };
        if !found.is_null() {
            unsafe { CFRelease(found) };
        }
        status == errSecSuccess
    }

    pub fn delete(&self) -> Result<()> {
        let dictionary = CFDictionary::from_CFType_pairs(&self.identity());
        let status = unsafe { SecItemDelete(dictionary.as_concrete_TypeRef()) };
        match status {
            s if s == errSecSuccess || s == errSecItemNotFound => Ok(()),
            other => Err(WrappingError::Store(describe(other))),
        }
    }
}

/// An OSStatus, with the ones that actually happen named.
fn describe(status: i32) -> String {
    let meaning = match status {
        -34018 => {
            " (missing entitlement: this binary is not signed with a keychain access group, \
                    so the data protection keychain is not available to it)"
        }
        -25291 => " (no keychain is available)",
        -25300 => " (no such item)",
        -128 => " (the user cancelled the authentication)",
        -25293 => " (authentication failed)",
        _ => "",
    };
    format!("OSStatus {status}{meaning}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether presence is enforceable depends on how this binary was built and
    /// signed, so this cannot assert an answer — only that asking is safe, is
    /// quick, and never puts a dialog in front of whoever ran the suite.
    #[test]
    fn probing_for_presence_is_free_and_never_prompts() {
        let started = std::time::Instant::now();
        let supported = EnclaveItem::presence_can_be_enforced();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the probe blocked, which means it asked the user something"
        );

        // And it is stable: a capability that flickers would make the passphrase
        // requirement flicker with it.
        assert_eq!(supported, EnclaveItem::presence_can_be_enforced());
        eprintln!("platform presence enforceable by this build: {supported}");
    }

    #[test]
    fn an_absent_item_reads_as_missing_rather_than_as_an_error() {
        let item = EnclaveItem::new("com.sum.syndeo.keystore.test", "definitely-not-enrolled");
        let _ = item.delete();
        assert!(!item.exists());
        assert!(matches!(item.load(), Err(WrappingError::Missing)));
    }
}
