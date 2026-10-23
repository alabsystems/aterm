// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launcher's startup-notification token, consumed once.

use std::sync::Mutex;

/// What the launcher handed over, until the first window takes it.
struct Tokens {
    /// `DESKTOP_STARTUP_ID`: the X11 launch sequence's id.
    x11: Option<String>,
    /// `XDG_ACTIVATION_TOKEN`: the Wayland activation token.
    wayland: Option<String>,
}

/// Set once by [`capture`], emptied by the first [`take_for_first_window`].
static TOKENS: Mutex<Option<Tokens>> = Mutex::new(None);

/// A token as the launcher wrote it: one non-empty UTF-8 value. Anything else is no
/// token (an id that is not text cannot name an X11 property or a Wayland request).
fn token_value(raw: Option<std::ffi::OsString>) -> Option<String> {
    raw.and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
}

/// Read both variables and remove them from this process's environment, so nothing aterm spawns
/// inherits them.
pub(crate) fn capture() {
    let tokens = Tokens {
        x11: token_value(aterm_log::env::take("DESKTOP_STARTUP_ID")),
        wayland: token_value(aterm_log::env::take("XDG_ACTIVATION_TOKEN")),
    };
    *TOKENS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(tokens);
}

/// The token the first window presents, for its backend: Wayland's activation token, or X11's
/// startup id.
pub(crate) fn take_for_first_window(wayland: bool) -> Option<String> {
    let tokens = TOKENS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()?;
    if wayland { tokens.wayland } else { tokens.x11 }
}

/// The startup id a program aterm opens for a person's press carries on X11: `_TIME<t>`, the newest
/// press's server time (`aterm_window::platform::x11::user_time`).
pub(crate) fn launch_id() -> Option<String> {
    aterm_window::platform::x11::user_time().map(|time| format!("_TIME{time}"))
}

/// `command` with [`launch_id`] in its environment as `DESKTOP_STARTUP_ID`, when there is one.
pub(crate) fn for_a_press(mut command: aterm_uds::spawnfd::Command) -> aterm_uds::spawnfd::Command {
    if let Some(id) = launch_id() {
        command.env("DESKTOP_STARTUP_ID", id);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An opener started after a press carries the press's time as its startup id.
    #[test]
    fn an_opener_after_a_press_carries_its_time() {
        aterm_window::platform::x11::note_user_time_for_test(5_678_901);
        let opener = for_a_press(aterm_uds::spawnfd::command("xdg-open"));
        let id = opener
            .get_envs()
            .find(|(key, _)| *key == "DESKTOP_STARTUP_ID")
            .and_then(|(_, value)| value);
        assert_eq!(id, Some(std::ffi::OsStr::new("_TIME5678901")));
    }

    /// A launch token is one non-empty text value; an empty or non-UTF-8 one is none.
    #[test]
    fn a_token_is_a_non_empty_text_value() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        assert_eq!(
            token_value(Some(OsString::from(
                "gnome-shell/aterm/4242-0-host_TIME5678"
            ))),
            Some("gnome-shell/aterm/4242-0-host_TIME5678".to_string())
        );
        assert_eq!(token_value(Some(OsString::from(""))), None);
        assert_eq!(token_value(None), None);
        assert_eq!(
            token_value(Some(OsString::from_vec(b"id\xff".to_vec()))),
            None
        );
    }
}
