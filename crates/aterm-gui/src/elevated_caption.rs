// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The badge an elevated window wears in its caption. An elevated aterm
//! (`aterm_uds::elevation::is_elevated`, a Windows administrator's full token)
//! titles every window `Administrator: <title>`, as Windows Terminal and the
//! console host do, so the titlebar, the taskbar button and Alt-Tab all say it
//! (`docs/WINDOWS_PARAGON_DESIGN.md` §5.7: an elevated session is a separate
//! window with an unmistakable badge).
//!
//! Only the caption carries it. A window's title state (`current_title`, which
//! the tab strip, the menu and the introspection verbs read, and whose leading
//! busy-spinner frame the redraw coalescing compares) stays unbadged, so every
//! site that hands a title to the OS goes through [`set_title`].

use std::borrow::Cow;

/// What an elevated window's caption starts with.
pub(crate) const PREFIX: &str = "Administrator: ";

/// `title` as the caption of a window in a process that is (`elevated`) or is
/// not elevated. A title that already starts with the badge (a shell under an
/// elevated console host names itself so) is not badged twice.
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
