// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS **Secure Keyboard Entry** — the OS-level keystroke-snooping guard
//! (`EnableSecureEventInput`), the protection iTerm2 offers under the same
//! name: while engaged, other processes cannot observe this app's key events
//! (event taps, `CGEventTap` keyloggers, the Input Method framework's
//! snooping surface).
//!
//! FOCUS-SCOPED, per Apple's own fairness guidance (TN2150): a process that
//! holds secure input while backgrounded suppresses every other app's global
//! hotkeys and event taps the whole time — TN2150 calls this out verbatim and
//! instructs releasing on deactivation, and iTerm2's
//! `iTermSecureKeyboardEntryController` does exactly that. So the Carbon
//! state here is `desired && app_active`: the config toggle sets DESIRE, app
//! activation (any aterm window focused) gates ENGAGEMENT. The elegant
//! consequence: protection is live precisely when keystrokes can reach this
//! process at all, and the rest of the desktop is never taxed for a
//! backgrounded terminal.
//!
//! History, for honesty: the ENGINE has carried an advisory
//! `secure_keyboard_entry` flag for a long time (`state_accessors.rs`), whose
//! own doc instructs "the UI layer must check this flag and enable the
//! appropriate protection" — and no UI layer ever did, while the flag was
//! dutifully reset-preserved, checkpointed, and carried across seamless
//! updates. It also has zero non-test producers: no escape sequence, verb, or
//! menu ever set it. This module is the missing actuator, driven by the one
//! real producer that exists — the user's `secure_keyboard_entry` config key
//! (Settings ▸ Security ▸ Permissions) — applied at launch and on every
//! config commit, gated by focus.
//!
//! The OS API is REFCOUNTED and must be balanced per process; every
//! transition below runs under ONE lock, so this process's contribution is 0
//! or 1 by construction, however the two producers interleave. Process exit —
//! and, measured on this machine (Darwin 25.5.0), `execve` too, so the
//! seamless-update handoff is covered — releases the count; a crash can never
//! wedge the system state.
//!
//! Off macOS this is a no-op module: Wayland is secure by default, X11 cannot
//! be secured, and Windows has no equivalent — the Settings surface says
//! "unavailable on this platform" rather than pretending.

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Mutex;

    // Carbon's HIToolbox owns this API family (Enable/Disable/
    // IsSecureEventInputEnabled); only the two transitions are declared —
    // the query answers about OTHER processes too, so it can never verify
    // our balance and earns no binding. Both return OSStatus.
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
        /// Whether THIS process currently holds one enable count.
        engaged: bool,
    }

    static STATE: Mutex<State> = Mutex::new(State {
        desired: false,
        app_active: false,
        engaged: false,
    });

    /// Reconcile the Carbon state with `desired && app_active`, under the
    /// lock. On an FFI refusal the recorded state keeps the OLD truth (so a
    /// later call retries instead of believing the failed transition) and the
    /// OSStatus is returned for the caller's user-facing channel.
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

    /// The config toggle (launch + every config commit). `Err(OSStatus)` when
    /// the OS refused the transition — the caller owns telling the USER,
    /// because a security switch that silently fails to take is the one kind
    /// of failure this feature must never have.
    pub(crate) fn set_desired(want: bool) -> Result<(), i32> {
        let mut state = STATE.lock().expect("secure-input state lock");
        state.desired = want;
        sync(&mut state)
    }

    /// The focus gate (any aterm window focused). Best-effort: a refusal here
    /// is logged, and the next transition retries — focus flickers every app
    /// switch, so this edge must never spam the user.
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
