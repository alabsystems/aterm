// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS Secure Keyboard Entry (`EnableSecureEventInput`): while engaged, other
//! processes cannot observe this app's key events (event taps, keyloggers, input methods).
//!
//! Focus-scoped per Apple's TN2150: a backgrounded holder suppresses every other app's
//! global hotkeys and event taps, so the Carbon state is `desired && app_active`. The
//! `secure_keyboard_entry` config key sets the desire (at launch and on every config
//! commit); app activation (any aterm window focused) gates engagement.
//!
//! The OS API is refcounted per process; every transition runs under one lock, so this
//! process contributes 0 or 1. Process exit and `execve` (the seamless update handoff)
//! release the count, so a crash cannot wedge system state.
//!
//! Off macOS this is a no-op: Wayland is secure by default, X11 cannot be secured and
//! Windows has no equivalent.

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Mutex;

    // Only the two transitions are bound: `IsSecureEventInputEnabled` also
    // reports other processes' state, so it cannot verify our balance. Both
    // return OSStatus.
    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn EnableSecureEventInput() -> i32;
        fn DisableSecureEventInput() -> i32;
    }

    struct State {
        /// The config toggle's value (the user's standing wish).
        desired: bool,
        /// Whether any aterm window is focused (the TN2150 gate).
        app_active: bool,
        /// Whether this process currently holds one enable count.
        engaged: bool,
    }

    static STATE: Mutex<State> = Mutex::new(State {
        desired: false,
        app_active: false,
        engaged: false,
    });

    /// Reconcile the Carbon state with `desired && app_active`, under the
    /// lock. On an FFI refusal the recorded state keeps the old value (so a
    /// later call retries) and the OSStatus is returned.
    fn sync(state: &mut State) -> Result<(), i32> {
        let want = state.desired && state.app_active;
        if want == state.engaged {
            return Ok(());
        }
        // SAFETY: plain Carbon calls with no arguments or pointers; the OS
        // refcounts them, and the lock keeps this process's contribution
        // balanced at 0 or 1.
        let status = unsafe {
            if want {
                EnableSecureEventInput()
            } else {
                DisableSecureEventInput()
            }
        };
        if status != 0 {
            return Err(status);
        }
        state.engaged = want;
        Ok(())
    }

    /// The config toggle (launch and every config commit). `Err(OSStatus)`
    /// when the OS refused the transition; the caller must tell the user, since
    /// a security switch must never fail silently.
    pub(crate) fn set_desired(want: bool) -> Result<(), i32> {
        let mut state = STATE.lock().expect("secure-input state lock");
        state.desired = want;
        sync(&mut state)
    }

    /// The focus gate (any aterm window focused). Best-effort: a refusal is
    /// logged and the next transition retries, so focus changes never notify
    /// the user.
    pub(crate) fn set_app_active(active: bool) {
        let mut state = STATE.lock().expect("secure-input state lock");
        state.app_active = active;
        if let Err(status) = sync(&mut state) {
            aterm_log::warn!(
                "secure keyboard entry: focus-gated transition failed (OSStatus {status}); \
                 will retry on the next focus or config change"
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub(crate) fn set_desired(_want: bool) -> Result<(), i32> {
        Ok(())
    }
    pub(crate) fn set_app_active(_active: bool) {}
}

pub(crate) use imp::{set_app_active, set_desired};
