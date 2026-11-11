// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A context's device-selection authority, retained through lazy setup and rebuilds.

/// Whether acquiring a device may consult a window system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevicePolicy {
    /// A live window may use the platform's default-device fallback.
    Windowed,
    /// Offscreen work never falls back to a window-system default device or display.
    Headless,
}

impl DevicePolicy {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn select<T>(
        self,
        listed: impl FnOnce() -> Option<T>,
        window_default: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        match self {
            Self::Windowed => listed().or_else(window_default),
            Self::Headless => listed(),
        }
    }
}

#[cfg(test)]
#[path = "device_policy_tests.rs"]
mod tests;
