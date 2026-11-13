// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine boot, sleep, and wake timestamps: the kernel's own records on macOS; on Windows the
//! kernel's boot time, and the last suspend and resume a listener of this process heard
//! ([`crate::power::windows::listen`]), since Windows keeps neither where a process without
//! privilege reads it.

/// The longest wake-reason word kept ([`wake_word`]).
pub const WAKE_WORD_CAP: usize = 64;

/// The machine's timeline, read now ([`timeline`]): unix milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Timeline {
    /// When the kernel booted (`kern.boottime`).
    pub boot_ms: i64,
    /// When the machine last went to sleep (`kern.sleeptime`), or `None` since boot.
    pub sleep_ms: Option<i64>,
    /// When the machine last woke (`kern.waketime`), or `None` since boot.
    pub wake_ms: Option<i64>,
    /// Last wake reason token ([`wake_word`] of `kern.wakereason`), if any.
    pub wake_reason: Option<String>,
}

/// Read the machine's [`Timeline`], or `None` if unavailable or on Linux. On Windows the sleep and
/// the wake are the last this process heard announced (`None` until one was, or with no
/// listener), and no wake reason is read.
#[must_use]
pub fn timeline() -> Option<Timeline> {
    imp::timeline()
}

/// Read the last wake timestamp in unix milliseconds (`kern.waketime`; on Windows the last
/// resume this process heard), if any.
#[must_use]
pub fn waketime_ms() -> Option<i64> {
    imp::waketime_ms()
}

/// Extract the last valid device token (up to [`WAKE_WORD_CAP`] bytes) from `reason`.
#[must_use]
pub fn wake_word(reason: &str) -> Option<String> {
    let word = |w: &&str| {
        w.len() <= WAKE_WORD_CAP
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    reason
        .split_ascii_whitespace()
        .rev()
        .find(word)
        .map(str::to_owned)
}

/// A time sysctl's answer as an instant: `None` for 0 (the kernel's "none
/// since boot") and before it. PURE.
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn recorded(ms: Option<i64>) -> Option<i64> {
    ms.filter(|ms| *ms > 0)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Timeline, recorded};
    use crate::macos::{sysctl_text_in, sysctl_timeval_ms};

    pub(super) fn waketime_ms() -> Option<i64> {
        recorded(sysctl_timeval_ms(c"kern.waketime"))
    }

    pub(super) fn timeline() -> Option<Timeline> {
        Some(Timeline {
            boot_ms: recorded(sysctl_timeval_ms(c"kern.boottime"))?,
            sleep_ms: recorded(sysctl_timeval_ms(c"kern.sleeptime")),
            wake_ms: waketime_ms(),
            wake_reason: sysctl_text_in::<256>(c"kern.wakereason")
                .as_deref()
                .and_then(super::wake_word),
        })
    }
}

#[cfg(windows)]
mod imp {
    use super::Timeline;
    use crate::power::windows::{boot_unix_ms, last_sleep};

    pub(super) fn waketime_ms() -> Option<i64> {
        last_sleep().1
    }

    pub(super) fn timeline() -> Option<Timeline> {
        let (sleep_ms, wake_ms) = last_sleep();
        Some(Timeline {
            boot_ms: boot_unix_ms()?,
            sleep_ms,
            wake_ms,
            wake_reason: None,
        })
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::Timeline;

    pub(super) fn waketime_ms() -> Option<i64> {
        None
    }

    pub(super) fn timeline() -> Option<Timeline> {
        None
    }
}

#[cfg(test)]
#[path = "timeline_tests.rs"]
mod tests;
