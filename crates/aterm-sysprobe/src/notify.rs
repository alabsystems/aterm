// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Darwin notification key's state, read on change (A4 §4): one check token per key
//! (`notify_register_check`), `notify_check` each reading (1.8 / 3.3 µs at load1 16 in N1), and
//! `notify_get_state` only when the check says the key was posted since (57 / 88 µs, never per
//! reading). A check token wakes nothing: no port, no thread, no run loop. Elsewhere a watch
//! reads `unsupported`.
//!
//! [`thermal_pressure`] watches `com.apple.system.thermalpressurelevel`, the documented thermal
//! pressure level (0 Nominal … 4 Sleeping); the model validates the level.

use std::ffi::CStr;

#[cfg(target_os = "macos")]
use crate::iokit as ffi;

/// The documented thermal pressure level's notify key.
pub const THERMAL_PRESSURE: &CStr = c"com.apple.system.thermalpressurelevel";

/// One notify key's state, read on change. Its token is cancelled on drop.
#[derive(Debug)]
pub struct Watch {
    /// The check token; `None` where registration failed or the system has no notify.
    #[cfg(target_os = "macos")]
    token: Option<std::ffi::c_int>,
    /// The state last read, or why there is none.
    state: Result<u64, &'static str>,
    /// How many times this watch called `notify_get_state`: the law that it reads on a post only.
    #[cfg(all(test, target_os = "macos"))]
    get_states: u32,
}

impl Watch {
    /// Registers a check token on `name`. A failure is kept and every read answers it
    /// (`notify_register`); off macOS every read answers `unsupported`.
    #[must_use]
    pub fn new(name: &CStr) -> Self {
        #[cfg(target_os = "macos")]
        {
            let mut token = 0;
            // SAFETY: a NUL-terminated name and a writable token.
            let rc = unsafe { ffi::notify_register_check(name.as_ptr(), &raw mut token) };
            if rc == 0 {
                Self {
                    token: Some(token),
                    state: Err("not_read"),
                    #[cfg(all(test, target_os = "macos"))]
                    get_states: 0,
                }
            } else {
                Self {
                    token: None,
                    state: Err("notify_register"),
                    #[cfg(all(test, target_os = "macos"))]
                    get_states: 0,
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = name;
            Self {
                state: Err("unsupported"),
            }
        }
    }

    /// The key's state: read again only when the check token says it was posted since the last
    /// read (the first check after registration always does), or while the last read failed.
    /// (Not `read`: a watch is no lock, and the lock census reads every `.read()` as one.)
    ///
    /// # Errors
    /// `notify_register` where registration failed, `notify_check` or `notify_get_state` where
    /// that call failed this reading, `unsupported` off macOS.
    pub fn state(&mut self) -> Result<u64, &'static str> {
        #[cfg(target_os = "macos")]
        if let Some(token) = self.token {
            let mut posted = 0;
            // SAFETY: a token notify_register_check gave and this watch has not cancelled, and a
            // writable flag.
            if unsafe { ffi::notify_check(token, &raw mut posted) } != 0 {
                self.state = Err("notify_check");
            } else if posted != 0 || self.state.is_err() {
                #[cfg(test)]
                {
                    self.get_states += 1;
                }
                let mut state = 0;
                // SAFETY: as above, and a writable state.
                self.state = if unsafe { ffi::notify_get_state(token, &raw mut state) } == 0 {
                    Ok(state)
                } else {
                    Err("notify_get_state")
                };
            }
        }
        self.state
    }
}

#[cfg(target_os = "macos")]
impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            // SAFETY: a token notify_register_check gave, cancelled once.
            let _ = unsafe { ffi::notify_cancel(token) };
        }
    }
}

/// A watch on the documented thermal pressure level ([`THERMAL_PRESSURE`]).
#[must_use]
pub fn thermal_pressure() -> Watch {
    Watch::new(THERMAL_PRESSURE)
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;
