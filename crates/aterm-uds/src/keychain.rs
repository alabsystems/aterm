// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this user's default keychain would answer a read without macOS asking anyone for its
//! password. A program that reads or writes an item in a locked keychain (Apple's `notarytool`
//! with a keychain profile, `security`, a credential helper) makes macOS raise its "enter the
//! keychain password" dialog; so does one run where no default keychain exists ("a keychain
//! cannot be found"). Work nobody asked for reads this first and runs no such program while the
//! answer is not `Some(true)`.

/// Whether this user's default (login) keychain is unlocked now, read with no dialog and no
/// child process (`SecKeychainGetStatus`, which reports the lock state and never unlocks):
/// `Some(false)` while it is locked, `None` where it cannot be read (no default keychain, or
/// not macOS).
#[must_use]
pub fn default_unlocked() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        /// `SecKeychain.h`: `kSecUnlockStateStatus`.
        const UNLOCKED: u32 = 1;
        // Security is a system framework every aterm process already links.
        #[link(name = "Security", kind = "framework")]
        unsafe extern "C" {
            fn SecKeychainGetStatus(keychain: *const core::ffi::c_void, status: *mut u32) -> i32;
        }
        let mut status = 0u32;
        // SAFETY: a null keychain names the default one; the out-pointer names a live local of
        // the declared C type (`SecKeychainStatus`, a `UInt32`), which the call only writes.
        let read = unsafe { SecKeychainGetStatus(std::ptr::null(), &raw mut status) };
        (read == 0).then_some(status & UNLOCKED != 0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}
