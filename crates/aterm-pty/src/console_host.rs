// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console HOST a Windows pseudoconsole runs in: Windows' own conhost (the inbox `CreatePseudoConsole`) or an OpenConsole.exe, Windows Terminal's build of the same host.

use std::path::PathBuf;

#[cfg(any(windows, test))]
pub(crate) mod policy;

/// aterm.toml `conpty`: which console host a new tab's pseudoconsole runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsoleHostChoice {
    /// The key absent or empty, or `"bundled"`: the default.
    Bundled,
    /// `"inbox"`: the Windows console host (`CreatePseudoConsole`), what every build before the OpenConsole default spawned.
    Inbox,
    /// `"auto"`: an `OpenConsole.exe` beside the running executable, else a copy of the installed Windows Terminal's (taken into aterm's state directory, `%LOCALAPPDATA%\aterm\conpty`, because an unpackaged process may not launch it inside `WindowsApps`), else the Windows console host.
    Auto,
    /// Any other value: this OpenConsole.exe (an absolute path) for every tab, falling back to the Windows console host when it does not start.
    Path(PathBuf),
}

impl ConsoleHostChoice {
    /// The choice aterm.toml's `conpty` value names.
    #[must_use]
    pub fn from_config(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("") => Self::Bundled,
            Some(v) if v.eq_ignore_ascii_case("bundled") => Self::Bundled,
            Some(v) if v.eq_ignore_ascii_case("inbox") => Self::Inbox,
            Some(v) if v.eq_ignore_ascii_case("auto") => Self::Auto,
            Some(v) => Self::Path(PathBuf::from(v)),
        }
    }
}

/// The console host behind one live session: where the two hosts' behaviour differs, and so where a workaround keys on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleHostKind {
    /// Windows' own conhost, through kernel32 `CreatePseudoConsole` (or a DefTerm handoff, where conhost made the pseudoconsole).
    Inbox,
    /// An OpenConsole.exe this crate launched.
    OpenConsole,
}

impl ConsoleHostKind {
    /// Whether an application's alternate-screen switch reaches the terminal VERBATIM and is never followed by a repaint of the buffer switched to.
    #[inline]
    #[must_use]
    pub const fn passes_screen_switches_through(self) -> bool {
        matches!(self, Self::OpenConsole)
    }
}

/// What `aterm doctor` and the log say about the console host a new tab gets: the kind, whether an opted-in OpenConsole fell back, and one line naming the host and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleHostReport {
    /// The host a new tab gets.
    pub kind: ConsoleHostKind,
    /// `true` when the choice named an OpenConsole (the bundled default, `auto` or a path) and tabs get the Windows console host instead; `false` for an OpenConsole and for the inbox host `conpty = "inbox"` asked for.
    pub fell_back: bool,
    /// `OpenConsole 1.25.2609.30003 (bundled beside aterm: …)`, `Windows console host (aterm.toml conpty = "inbox")`, … — see `policy::report`.
    pub line: String,
}

impl std::fmt::Display for ConsoleHostReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line)
    }
}

/// Unix-seam parity for the Windows backend's `set_console_host`: there is no console host off Windows, so the choice is accepted and ignored, and the window's one call shape compiles on every platform.
#[cfg(unix)]
pub fn set_console_host(_choice: ConsoleHostChoice) {}

/// Unix-seam parity for the Windows backend's `take_console_host_fallback`: nothing falls back where there is no console host.
#[cfg(unix)]
#[must_use]
pub const fn take_console_host_fallback() -> Option<String> {
    None
}

/// Unix-seam parity for the Windows backend's `conpty_host`: no session runs in a console host off Windows.
#[cfg(unix)]
#[must_use]
pub const fn conpty_host(_master: i32) -> Option<ConsoleHostKind> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_value_names_the_choice_and_absent_is_the_bundled_host() {
        for absent in [
            None,
            Some(""),
            Some("  "),
            Some("bundled"),
            Some(" Bundled "),
        ] {
            assert_eq!(
                ConsoleHostChoice::from_config(absent),
                ConsoleHostChoice::Bundled,
                "{absent:?}"
            );
        }
        assert_eq!(
            ConsoleHostChoice::from_config(Some("Inbox")),
            ConsoleHostChoice::Inbox
        );
        assert_eq!(
            ConsoleHostChoice::from_config(Some(" AUTO ")),
            ConsoleHostChoice::Auto
        );
        assert_eq!(
            ConsoleHostChoice::from_config(Some(r"C:\tools\OpenConsole.exe")),
            ConsoleHostChoice::Path(PathBuf::from(r"C:\tools\OpenConsole.exe"))
        );
    }

    #[test]
    fn only_openconsole_passes_screen_switches_through() {
        assert!(ConsoleHostKind::OpenConsole.passes_screen_switches_through());
        assert!(!ConsoleHostKind::Inbox.passes_screen_switches_through());
    }

    #[test]
    fn the_window_calls_have_one_shape_on_every_platform() {
        let _: fn(ConsoleHostChoice) = crate::set_console_host;
        let _: fn() -> Option<String> = crate::take_console_host_fallback;
        let _: fn(i32) -> Option<ConsoleHostKind> = crate::conpty_host;
    }

    #[cfg(unix)]
    #[test]
    fn off_windows_there_is_no_console_host() {
        set_console_host(ConsoleHostChoice::Auto);
        assert_eq!(take_console_host_fallback(), None);
        assert_eq!(conpty_host(0), None);
    }
}
