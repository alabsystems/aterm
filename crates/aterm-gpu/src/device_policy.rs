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
mod tests {
    use super::DevicePolicy;

    #[test]
    fn headless_selection_never_calls_the_window_fallback() {
        for listed in [Some(7), None] {
            assert_eq!(
                DevicePolicy::Headless.select(|| listed, || panic!("window fallback")),
                listed
            );
        }
    }

    #[test]
    fn windowed_selection_keeps_the_listed_device_and_uses_only_a_needed_fallback() {
        let calls = std::cell::Cell::new(0);
        for (listed, expected, count) in [(Some(7), Some(7), 0), (None, Some(9), 1)] {
            let selected = DevicePolicy::Windowed.select(
                || listed,
                || {
                    calls.set(calls.get() + 1);
                    Some(9)
                },
            );
            assert_eq!(selected, expected);
            assert_eq!(calls.get(), count);
        }
        assert_eq!(DevicePolicy::Windowed.select(|| None::<u8>, || None), None);
    }
}
