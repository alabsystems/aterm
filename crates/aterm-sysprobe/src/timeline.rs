// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MACHINE'S TIMELINE (macOS): when the kernel booted, and when the
//! machine last slept and woke and on what, the anchors the cause engine's
//! `startup` and `wake` contexts are read against
//! (`docs/DESIGN-resource-details-2026-10-20.md` §5.1).
//!
//! * [`timeline`] — `kern.boottime`, `kern.sleeptime`, `kern.waketime` and
//!   `kern.wakereason` (`sysctlbyname`, any uid, a few µs).
//! * [`waketime_ms`] — `kern.waketime` alone: the read a caller makes each
//!   time, re-reading the rest only when it moved.
//! * [`wake_word`] — the word a wake reason is named by. PURE.
//!
//! A kernel boot is not a userspace start: userspace can start again long
//! after the kernel booted (m22, 2026-10-20: boot 09:06:13, userspace
//! 10:28:38, 5 s after a wake), so that start is the earliest live birth of
//! a pid above 1 (launchd outlives a userspace restart: born 09:06:14 there),
//! read from [`crate::kproc::record`], not here. Every other platform answers
//! `None`.

/// The longest wake-reason word kept ([`wake_word`]).
pub const WAKE_WORD_CAP: usize = 64;

/// The machine's timeline, read now ([`timeline`]): unix milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Timeline {
    /// When the kernel booted (`kern.boottime`, which macOS moves when the
    /// wall clock is stepped).
    pub boot_ms: i64,
    /// When the machine last went to sleep (`kern.sleeptime`); `None` with
    /// no sleep since boot.
    pub sleep_ms: Option<i64>,
    /// When it last woke (`kern.waketime`); `None` with no wake since boot.
    pub wake_ms: Option<i64>,
    /// What it last woke on: [`wake_word`] of `kern.wakereason`
    /// (`RTP.multi-touch`, the trackpad); `None` when it names none.
    pub wake_reason: Option<String>,
}

/// The machine's timeline; `None` when the boot time cannot be read, and off
/// macOS.
#[must_use]
pub fn timeline() -> Option<Timeline> {
    imp::timeline()
}

/// When the machine last woke, unix milliseconds (`kern.waketime`); `None`
/// with no wake since boot, when refused, and off macOS.
#[must_use]
pub fn waketime_ms() -> Option<i64> {
    imp::waketime_ms()
}

/// The word a wake `reason` is named by: the last of its space-separated
/// words that is one, at most [`WAKE_WORD_CAP`] letters, digits, `.`, `_` or
/// `-`. Apple silicon lists the wake's path, the device last
/// (`MTP.DOCK.CHANNELS.AP0.IRQ nub-spmi-a0.0x0B RTP.multi-touch` is
/// `RTP.multi-touch`). `None` when no word is one. PURE.
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

#[cfg(not(target_os = "macos"))]
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
