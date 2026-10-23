// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Focus-scoped macOS Secure Keyboard Entry (`EnableSecureEventInput`).

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Mutex;

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

    fn sync(state: &mut State) -> Result<(), i32> {
        let want = state.desired && state.app_active;
        if want == state.engaged {
            return Ok(());
        }
        // SAFETY: plain Carbon calls with no arguments or pointers; the OS refcounts them, and the
        // lock keeps this process's contribution balanced at 0 or 1.
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

    /// Sets the `secure_keyboard_entry` desired state and syncs Carbon event input.
    pub(crate) fn set_desired(want: bool) -> Result<(), i32> {
        let mut state = STATE.lock().expect("secure-input state lock");
        state.desired = want;
        sync(&mut state)
    }

    /// Updates the focus gate (`app_active`) and syncs Carbon event input best-effort.
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
