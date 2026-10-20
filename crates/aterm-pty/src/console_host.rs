// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console HOST a Windows pseudoconsole runs in: Windows' own conhost (the
//! inbox `CreatePseudoConsole`) or an OpenConsole.exe, Windows Terminal's build
//! of the same host. This file holds the types every platform names; the policy
//! (aterm.toml's `conpty` choice, where `auto` finds an OpenConsole, which
//! programs keep conhost, the fallback note, the report lines) is
//! `policy`, platform-neutral so its tests run on every host; the launch is
//! `windows/openconsole.rs`, a port of microsoft/terminal's
//! `src/winconpty/winconpty.cpp` (MIT).
//!
//! THE DEFAULT. With aterm.toml's `conpty` absent a window's tabs run in the
//! OpenConsole.exe shipped beside aterm (`apps/aterm-win/build.ps1` lays a
//! pinned, Microsoft-signed one there), and in the inbox host when there is
//! none (a bare `cargo build`); the default never copies anything or reaches
//! into Windows Terminal's package. `conpty = "inbox"` keeps the inbox host,
//! what every build before 2026-10-17 spawned, and resolves nothing. An
//! embedder that never states a choice (the CLI's passthrough session, this
//! crate's own tests) keeps the inbox host too.
//!
//! WHY OPENCONSOLE. Measured again on 2026-10-17 (Windows 11 26200, the
//! bundled 1.25.2609.30003 against the inbox host, `docs/WINDOWS_PARAGON_DESIGN.md`
//! §5.4): the terminal's own answers reach the program (DA1, DECRQM
//! 2026/2027/2031, OSC 10/11/12, XTGETTCAP, the kitty graphics query), sixel
//! and kitty images draw, `SGR 58` passes whole, and bulk output is faster for
//! less host CPU. These are the numbers the 2026-09-29 snapshot (6ad1a274d8)
//! recorded on Windows 11 26200 against a copy of Windows Terminal 1.24's
//! OpenConsole (1.24.2607.10001), measured 2026-09-28:
//! * pass-through: the inbox host rewrote pwsh's `SGR 58` underline colour to
//!   `4m … 24m`, hard-wrapped a 110-column line at 80 with CUP, rewrote OSC 8
//!   `id=` and moved a curly underline across a line break; OpenConsole emitted
//!   all of them byte for byte;
//! * width: the inbox host measured `e`+U+0301 as 2 cells, man-ZWJ-woman as 5,
//!   a family as 8 (12 of 25 samples left gaps of 1 to 6 cells in aterm's
//!   grid); OpenConsole measured graphemes the way aterm does (24 of 25);
//! * re-rendering: the inbox host repainted 723 to 2436 bytes on every resize
//!   (10 of 10) and a stale main buffer after a killed full-screen app;
//!   OpenConsole emitted nothing on a resize, and a 100,000-line flood took
//!   46 ms of the child's time instead of 395.
//!
//! What it does NOT change: the input limits. A kitty CSI-u report still
//! reaches a record reader as typed characters, Shift+Enter still reads as CR
//! for a VT-input reader, and a lone ESC after one win32-input record is held
//! exactly as the inbox host holds it, so every input workaround in this crate
//! and the frontend applies to both hosts ([`ConsoleHostKind`] says where the
//! two differ). One it makes worse: a reader in VT-input mode gets text alone,
//! so the MSYS2 runtime (Git Bash) never sees the inline record that absorbs its
//! post-resize drop; a second console client writes it there instead (the
//! Windows backend's `console_guard`), and without that client the default and
//! `auto` keep those programs on conhost (`policy::runs_a_posix_runtime`).

use std::path::PathBuf;

#[cfg(any(windows, test))]
pub(crate) mod policy;

/// aterm.toml `conpty`: which console host a new tab's pseudoconsole runs in.
/// Read on Windows only; every other platform has no console host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsoleHostChoice {
    /// The key absent or empty, or `"bundled"`: the default. The
    /// `OpenConsole.exe` beside the running executable (the pinned one the
    /// Windows build ships), else the Windows console host. Never a copy of
    /// Windows Terminal's: the default runs a host this build was tested with,
    /// or none. Git Bash and the other MSYS2/Cygwin programs are treated as
    /// under [`Self::Auto`].
    Bundled,
    /// `"inbox"`: the Windows console host (`CreatePseudoConsole`), what every
    /// build before the OpenConsole default spawned.
    Inbox,
    /// `"auto"`: an `OpenConsole.exe` beside the running executable, else a
    /// copy of the installed Windows Terminal's (taken into aterm's state
    /// directory, `%LOCALAPPDATA%\aterm\conpty`, because an unpackaged process
    /// may not launch it inside `WindowsApps`), else the Windows console host.
    /// Git Bash and the other MSYS2/Cygwin programs keep the Windows console
    /// host only when no `aterm.exe` beside aterm can guard their input
    /// (`policy::runs_a_posix_runtime`).
    Auto,
    /// Any other value: this OpenConsole.exe (an absolute path) for every tab,
    /// falling back to the Windows console host when it does not start.
    Path(PathBuf),
}

impl ConsoleHostChoice {
    /// The choice aterm.toml's `conpty` value names. `None`, an empty value and
    /// `bundled` (any case) are [`Self::Bundled`]; `inbox` (any case) is
    /// [`Self::Inbox`]; `auto` (any case) is [`Self::Auto`]; anything else is a
    /// path.
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

/// The console host behind one live session: where the two hosts' behaviour
/// differs, and so where a workaround keys on it.
///
/// RECORDED THE SAME under both by the 2026-09-29 snapshot (measured
/// 2026-09-28), so keyed on neither: the resize seam (`ResizePolicy::ConPty`
/// matched OpenConsole's cursor at 9 of 9 checkpoints, the native policy at 5
/// and garbled a real pwsh), the Enter-chord and Escape win32 records
/// (OpenConsole asks for `?9001h` too and holds a lone ESC after one record
/// exactly as conhost does) and the kitty keyboard posture (OpenConsole 1.24
/// turns a CSI-u report into one typed character per byte for a record
/// reader). The MSYS resize guard is the exception: OpenConsole never delivers
/// its inline record to the MSYS2 runtime's VT-input reader, so an OpenConsole
/// session's guard is written by a second console client, and only when such
/// a program is in the tab (the Windows backend's `console_guard`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleHostKind {
    /// Windows' own conhost, through kernel32 `CreatePseudoConsole` (or a
    /// DefTerm handoff, where conhost made the pseudoconsole).
    Inbox,
    /// An OpenConsole.exe this crate launched.
    OpenConsole,
}

impl ConsoleHostKind {
    /// Whether an application's alternate-screen switch reaches the terminal
    /// VERBATIM and is never followed by a repaint of the buffer switched to.
    /// OpenConsole, as the snapshot recorded it: `less` killed mid-screen left
    /// conhost's alternate buffer up on both hosts (the dead buffer is the
    /// console's state, not the host's), but OpenConsole then emitted only the
    /// next app's `?1049l`, none of the 240 to 593 bytes of repainted primary
    /// buffer the inbox host sends, so a LOCAL leave of the orphaned screen puts
    /// the terminal out of step with every absolute cursor move the shell's line
    /// editor makes afterwards, and only a console client's `?1049l` (the
    /// Windows backend's `resync_alt_screen`) leaves both screens together.
    #[must_use]
    pub fn passes_screen_switches_through(self) -> bool {
        matches!(self, Self::OpenConsole)
    }
}

/// What `aterm doctor` and the log say about the console host a new tab gets:
/// the kind, whether an opted-in OpenConsole fell back, and one line naming
/// the host and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleHostReport {
    /// The host a new tab gets.
    pub kind: ConsoleHostKind,
    /// `true` when the choice named an OpenConsole (the bundled default,
    /// `auto` or a path) and tabs get the Windows console host instead;
    /// `false` for an OpenConsole and for the inbox host `conpty = "inbox"`
    /// asked for.
    pub fell_back: bool,
    /// `OpenConsole 1.25.2609.30003 (bundled beside aterm: …)`, `Windows
    /// console host (aterm.toml conpty = "inbox")`, … — see `policy::report`.
    pub line: String,
}

impl std::fmt::Display for ConsoleHostReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line)
    }
}

/// Unix-seam parity for the Windows backend's `set_console_host`: there is no
/// console host off Windows, so the choice is accepted and ignored, and the
/// window's one call shape compiles on every platform.
#[cfg(unix)]
pub fn set_console_host(_choice: ConsoleHostChoice) {}

/// Unix-seam parity for the Windows backend's `take_console_host_fallback`:
/// nothing falls back where there is no console host.
#[cfg(unix)]
#[must_use]
pub fn take_console_host_fallback() -> Option<String> {
    None
}

/// Unix-seam parity for the Windows backend's `conpty_host`: no session runs
/// in a console host off Windows.
#[cfg(unix)]
#[must_use]
pub fn conpty_host(_master: i32) -> Option<ConsoleHostKind> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // The default: the key absent, empty or `bundled` is the OpenConsole
    // beside aterm; only `inbox` names the Windows console host every earlier
    // build spawned.
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

    // The window calls these three on every platform: the Windows backend and
    // the unix parity must keep one call shape, which this pins on both.
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
