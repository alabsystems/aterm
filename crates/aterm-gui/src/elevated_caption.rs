// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Administrator: <title>` window caption prefix for elevated processes,
//! applied only to the OS window title.

use std::borrow::Cow;

/// What an elevated window's caption starts with.
pub(crate) const PREFIX: &str = "Administrator: ";

/// Returns `title` prefixed with [`PREFIX`] when `elevated` is true and the
/// prefix is not already present.
#[must_use]
pub(crate) fn caption(title: &str, elevated: bool) -> Cow<'_, str> {
    if !elevated || title.starts_with(PREFIX) {
        return Cow::Borrowed(title);
    }
    let mut badged = String::with_capacity(PREFIX.len() + title.len());
    badged.push_str(PREFIX);
    badged.push_str(title);
    Cow::Owned(badged)
}

/// Set `window`'s caption to `title`, badged when this process is elevated.
pub(crate) fn set_title(window: &aterm_window::window::Window, title: &str) {
    window.set_title(&caption(title, aterm_uds::elevation::is_elevated()));
}

#[cfg(test)]
mod tests {
    use super::caption;

    /// An elevated process badges every caption once; an unelevated one
    /// passes the title through untouched.
    #[test]
    fn an_elevated_caption_says_administrator_once() {
        assert_eq!(caption("~\\aterm", false), "~\\aterm");
        assert_eq!(caption("~\\aterm", true), "Administrator: ~\\aterm");
        assert_eq!(caption("aterm", true), "Administrator: aterm");
        assert_eq!(
            caption("Administrator: C:\\Windows\\system32\\cmd.exe", true),
            "Administrator: C:\\Windows\\system32\\cmd.exe"
        );
        assert_eq!(caption("", true), "Administrator: ");
    }
}
