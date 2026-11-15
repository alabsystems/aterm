// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two words of a survey header's machine look that are not figures: `os=` and `target=`.
//!
//! The machine that answers writes them and every reader reads them by these shapes, on every
//! platform: a Windows machine's survey answer says `os=windows` as a Mac's says `os=macos`.
//! Anything else (`-`, a path, a phrase, an address) reads as unread.

/// The OS words a look names.
pub const OS_WORDS: &[&str] = &["macos", "linux", "windows"];

/// The OS `raw` names: exactly one of [`OS_WORDS`].
#[must_use]
pub fn os_of(raw: &str) -> Option<&'static str> {
    OS_WORDS.iter().copied().find(|word| *word == raw)
}

/// The longest target triple a look carries.
pub const TARGET_MAX: usize = 48;

/// The target triple `raw` names, where it has a triple's shape: a lowercase letter, then
/// lowercase letters, digits, `_` and `-`, at most [`TARGET_MAX`] bytes. A triple this build
/// does not know still reads (a newer aterm's target); `-`, a path, a phrase and an address do
/// not. Every reader of a header's `target=` reads it by this one shape.
#[must_use]
pub fn target_of(raw: &str) -> Option<String> {
    let shaped = raw.len() <= TARGET_MAX
        && raw.starts_with(|c: char| c.is_ascii_lowercase())
        && raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    shaped.then(|| raw.to_owned())
}
