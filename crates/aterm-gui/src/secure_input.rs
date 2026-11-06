// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Focus-scoped macOS Secure Keyboard Entry (`EnableSecureEventInput`): on while an aterm window
//! is focused and the person asked for it (`secure_keyboard_entry`), or a focused window shows a
//! private document in front (one-mesh design §4.8: what the person types there, a new password
//! included, reaches no other process's event tap).

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
        /// Whether a focused window shows a private document in front.
        private_front: bool,
        /// Whether this process currently holds one enable count.
        engaged: bool,
    }

    static STATE: Mutex<State> = Mutex::new(State {
        desired: false,
        app_active: false,
        private_front: false,
        engaged: false,
    });

    fn sync(state: &mut State) -> Result<(), i32> {
        sync_with(state, |want| {
            if cfg!(test) {
                // A test process never reaches the window server: the inert arm answers 0, so
                // only the count moves.
                return 0;
            }
            // SAFETY: plain Carbon calls with no arguments or pointers; the OS refcounts them, and
            // the lock keeps this process's contribution balanced at 0 or 1.
            unsafe {
                if want {
                    EnableSecureEventInput()
                } else {
                    DisableSecureEventInput()
                }
            }
        })
    }

    /// Asks `carbon` to enable (`true`) or disable secure input where the wish and focus disagree
    /// with what this process holds, and records the change only when its status is 0: a refused
    /// change is not recorded, so the next focus or config change asks again.
    fn sync_with(state: &mut State, carbon: impl FnOnce(bool) -> i32) -> Result<(), i32> {
        let want = (state.desired || state.private_front) && state.app_active;
        if want == state.engaged {
            return Ok(());
        }
        let status = carbon(want);
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
        settle(&mut state);
    }

    /// Updates whether a focused window shows a private document in front, and syncs Carbon
    /// event input best-effort.
    pub(crate) fn set_private_front(private: bool) {
        let mut state = STATE.lock().expect("secure-input state lock");
        if state.private_front == private {
            return;
        }
        state.private_front = private;
        settle(&mut state);
    }

    /// [`sync`], a refusal logged: the next focus, front or config change asks again.
    fn settle(state: &mut State) {
        if let Err(status) = sync(state) {
            aterm_log::warn!(
                "secure keyboard entry: focus-gated transition failed (OSStatus {status}); \
                 will retry on the next focus or config change"
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{State, sync_with};

        /// A change Carbon refuses is not recorded, so the next sync asks again: a refused
        /// enable is not held and a refused release keeps the hold; a settled state asks nothing.
        #[test]
        fn a_refused_change_is_not_held_and_the_next_sync_asks_again() {
            let mut s = State {
                desired: true,
                app_active: true,
                private_front: false,
                engaged: false,
            };
            assert_eq!(
                sync_with(&mut s, |want| {
                    assert!(want);
                    -25
                }),
                Err(-25)
            );
            assert!(!s.engaged, "a refused enable is not held");
            assert_eq!(
                sync_with(&mut s, |want| {
                    assert!(want);
                    0
                }),
                Ok(()),
                "asked again"
            );
            assert!(s.engaged);
            assert_eq!(
                sync_with(&mut s, |_| panic!("settled: nothing to ask")),
                Ok(())
            );
            s.desired = false;
            assert_eq!(
                sync_with(&mut s, |want| {
                    assert!(!want);
                    -1
                }),
                Err(-1)
            );
            assert!(s.engaged, "a refused disable keeps the hold");
        }

        /// A private document in front of a focused window engages secure input though the
        /// person never asked for it, and releases it when it leaves the front or the app loses
        /// focus.
        /// NEGATIVE CONTROL: with `sync_with`'s wish reading `desired` alone, the first sync asks
        /// nothing and `` a private front engages `` fails.
        #[test]
        fn a_private_front_engages_secure_input_while_focused() {
            let mut s = State {
                desired: false,
                app_active: true,
                private_front: true,
                engaged: false,
            };
            let mut asked = None;
            assert_eq!(
                sync_with(&mut s, |want| {
                    asked = Some(want);
                    0
                }),
                Ok(())
            );
            assert_eq!(asked, Some(true), "a private front engages");
            assert!(s.engaged);
            s.app_active = false;
            assert_eq!(sync_with(&mut s, i32::from), Ok(()));
            assert!(!s.engaged, "no focus, no hold");
            s.app_active = true;
            s.private_front = false;
            assert_eq!(
                sync_with(&mut s, |_| panic!(
                    "neither wished nor private: nothing to ask"
                )),
                Ok(())
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

#[cfg(target_os = "macos")]
pub(crate) use imp::set_private_front;
pub(crate) use imp::{set_app_active, set_desired};
