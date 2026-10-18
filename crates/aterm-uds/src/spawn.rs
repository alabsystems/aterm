// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which Windows console a child process gets, decided when its `Command` is built: by
//! `aterm_uds::spawnfd::command` (every shipped child's constructor, which applies
//! [`Console::Quiet`] off Unix), by [`set_flags`] and, for a GUI image, by
//! [`set_gui_image_flags`]. Off Windows nothing here changes a `Command`.
//!
//! The windowed image (`aterm-gui.exe`, PE subsystem GUI) has no console, and Windows gives
//! every console-subsystem child such a parent starts without a console flag a new, visible
//! console window (`curl.exe`, `atpkg.exe`, `hostname.exe`, `aterm.exe fabric`, each a
//! flashing window). The child cannot cure this itself: one whose first act is `FreeConsole`
//! still flashes. The launcher decides, here.
//!
//! ## The rule: `CREATE_NO_WINDOW` exactly when this process has no console
//!
//! [`Console::Quiet`], the default, adds `CREATE_NO_WINDOW` when this process is attached to
//! no console ([`have_console`]) and nothing when it has one:
//!
//! * No console (the window): the child gets a windowless console of its own, and its own
//!   plain console children share it, so what `atpkg.exe` starts in turn is covered by the one
//!   flag at the top.
//! * A console (a terminal session, `aterm pkg …` typed at a prompt, a ConPTY tab): the child
//!   shares it; its output reaches the parent's screen and Ctrl+C reaches it.
//!
//! Not `CREATE_NO_WINDOW` always: from a parent that has a console the flag gives the child a
//! private one, so `WriteFile` on an inherited stdout fails `ERROR_INVALID_HANDLE`, a prompt on
//! `CONOUT$` goes nowhere visible, Ctrl+C never reaches it, its code page drops from 65001 to
//! 437, and the extra conhost costs about 10 ms a spawn.
//!
//! Not `DETACHED_PROCESS`: every plain console child a detached child starts gets a new
//! visible console. [`Console::Detached`] is only for a GUI image (the console image handing a
//! window to `aterm-gui.exe`), set by [`set_gui_image_flags`].
//!
//! Other flags compose through `extra`: `CREATE_NEW_PROCESS_GROUP` alone from a parent with no
//! console gives a visible window; with `CREATE_NO_WINDOW` it is windowless. The console bits
//! (`DETACHED_PROCESS`, `CREATE_NEW_CONSOLE`, `CREATE_NO_WINDOW`) come only from [`Console`];
//! any in `extra` are dropped. A GUI-subsystem target ignores every console flag, so a caller
//! never needs to know which subsystem the program it starts was built for.
//!
//! This module holds the only setter. std's `CommandExt::creation_flags` replaces the whole
//! flag word, so a second call anywhere after this module's brings the flash back; the
//! workspace clippy.toml bans it outside here. [`set_flags`] replaces the word too, which is
//! how a site overrides the quiet choice `spawnfd::command` made (`CREATE_NEW_PROCESS_GROUP`
//! for the codex daemon-update) and how a reviewed raw `Command` gets its console.
//!
//! ## When the decision is made
//!
//! When the `Command` is built, not when it is spawned, and per `Command`, never latched at
//! startup. In aterm-gui the two agree because `main_entry` attaches to its parent's console
//! (`win32::attach_parent_console`) and gives it back (`win32::release_attached_console`)
//! before any lane spawns anything; a later `AttachConsole`/`FreeConsole` must keep that order.
//!
//! ## The fence
//!
//! The workspace clippy.toml bans `std::process::Command`, std's `creation_flags` and the
//! `windows-sys` process-creation calls; each reviewed raw site carries its `#[expect]` on the
//! statement that builds it, and one that Windows reaches calls [`set_flags`] itself. The
//! setters here take that command as [`StdCommand`](crate::spawnfd::StdCommand), the ban's one
//! excuse, so none of them excuses a construction. The one raw spawn without this rule is
//! atpkg's toolchain probes (`toolchain.rs`, compiled into the dependency-free aterm-verify
//! too), reached only from `aterm help rust` typed at a console its children share; started
//! with no console, each probe would show a console window.
//!
//! ## What is not here
//!
//! * The ConPTY spawn (`aterm-pty`'s `CreateProcessW` with
//!   `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`): the pseudoconsole is that child's console and
//!   shows no window. `CREATE_NO_WINDOW` beside the attribute is untested; do not add it. Any
//!   other raw `CreateProcessW` that starts a console image calls [`creation_flags`] with
//!   [`have_console`].
//! * `wShowWindow`/`SW_HIDE`: std's `CommandExt::show_window` is unstable (E0658), so
//!   `creation_flags` is the only lever.
//!
//! [`Console`]: crate::spawn::Console
//! [`Console::Quiet`]: crate::spawn::Console::Quiet
//! [`Console::Detached`]: crate::spawn::Console::Detached
//! [`creation_flags`]: crate::spawn::creation_flags
//! [`have_console`]: crate::spawn::have_console
//! [`set_flags`]: crate::spawn::set_flags
//! [`set_gui_image_flags`]: crate::spawn::set_gui_image_flags

use crate::spawnfd::StdCommand;

/// `DETACHED_PROCESS` (WinBase.h): the child gets no console, neither this process's nor a
/// new one. For a GUI image only ([`Console::Detached`]).
///
/// Not `CREATE_NEW_PROCESS_GROUP`, the other flag that sounds like a detach: it starts the
/// child with Ctrl+C disabled, and every shell a window started that way spawns inherits the
/// setting, so no tab could interrupt a command.
pub const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_CONSOLE` (WinBase.h): the child gets a new, visible console. No choice here
/// sets it; [`creation_flags`] drops it from `extra`.
pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
/// `CREATE_NEW_PROCESS_GROUP` (WinBase.h): the child leads a new console process
/// group, with Ctrl+C disabled in it. Not a console bit: it composes through
/// `extra`.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `CREATE_NO_WINDOW` (WinBase.h): a console image gets a console of its own with no window.
/// Ignored for a GUI image.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The bits that decide a child's console. They come from [`Console`] alone:
/// [`creation_flags`] drops any a caller passes in `extra`.
pub const CONSOLE_BITS: u32 = DETACHED_PROCESS | CREATE_NEW_CONSOLE | CREATE_NO_WINDOW;

/// Which console a Windows child gets. Ignored off Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Console {
    /// The default. This process's console when it has one (shared output, Ctrl+C, console
    /// modes); otherwise a windowless console of the child's own (`CREATE_NO_WINDOW`), which
    /// its console children share.
    Quiet,
    /// No console at all (`DETACHED_PROCESS`): only for a GUI image ([`set_gui_image_flags`]).
    /// A console image started this way gives each plain console child a visible console.
    Detached,
}

/// The `dwCreationFlags` word for a child that gets `console`, with the caller's
/// own bits `extra` (console bits in it are dropped), when this process
/// `have_console`. Pure and `const`, so the rule is a table test on every host.
#[must_use]
pub const fn creation_flags(console: Console, extra: u32, have_console: bool) -> u32 {
    let extra = extra & !CONSOLE_BITS;
    match console {
        Console::Quiet if have_console => extra,
        Console::Quiet => extra | CREATE_NO_WINDOW,
        Console::Detached => extra | DETACHED_PROCESS,
    }
}

/// Whether this process is attached to a console right now: a visible one, a windowless one
/// (`CREATE_NO_WINDOW`) or a ConPTY pseudoconsole.
///
/// `GetConsoleProcessList`, never `GetConsoleWindow`, which is NULL for a windowless console.
/// The list call fails (0, `ERROR_INVALID_HANDLE`) exactly when there is no console.
#[cfg(windows)]
#[must_use]
// Skip: bottoms out at the `GetConsoleProcessList` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn have_console() -> bool {
    let mut first = 0u32;
    // SAFETY: a one-element out-buffer that outlives the call, and its length
    // passed as 1; the API writes at most that many pids and returns the
    // console's whole count (0 on failure).
    unsafe { crate::win::ffi::GetConsoleProcessList(&mut first, 1) != 0 }
}

/// Off Windows there is no console to attach a child to (a terminal is not
/// one), and [`set_flags`] sets nothing: always `false`.
#[cfg(not(windows))]
#[must_use]
pub fn have_console() -> bool {
    false
}

/// Set `cmd`'s creation flags for a [`Console::Quiet`] child plus `extra`, decided now
/// ([`creation_flags`] over [`have_console`]). It replaces the whole word, so on a `Command`
/// `spawnfd::command` built it overrides that choice. A no-op off Windows.
pub fn set_flags(cmd: &mut StdCommand, extra: u32) -> &mut StdCommand {
    set_console(cmd, Console::Quiet, extra)
}

/// Set `cmd`'s creation flags for a GUI image (the windowed sibling): no console at all
/// ([`Console::Detached`]), which a console image must never get. A no-op off Windows.
pub fn set_gui_image_flags(cmd: &mut StdCommand) -> &mut StdCommand {
    set_console(cmd, Console::Detached, 0)
}

/// The only creation-flags write: `console` plus `extra`, decided now.
fn set_console(cmd: &mut StdCommand, console: Console, extra: u32) -> &mut StdCommand {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        #[expect(
            clippy::disallowed_methods,
            reason = "the one creation-flags setter (module docs)"
        )]
        cmd.creation_flags(creation_flags(console, extra, have_console()));
    }
    #[cfg(not(windows))]
    let _ = (console, extra);
    cmd
}

#[cfg(test)]
mod tests {
    use super::{
        CONSOLE_BITS, CREATE_NEW_CONSOLE, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, Console,
        DETACHED_PROCESS, creation_flags,
    };

    /// A quiet child is windowless only when this process has no console, the explicit
    /// choice ignores whether it has one, and the console bits come from the choice alone.
    #[test]
    fn the_console_is_decided_by_the_choice_and_whether_this_process_has_one() {
        let group = CREATE_NEW_PROCESS_GROUP;
        for (console, extra, have, want) in [
            // A window (no console): windowless, with or without a group.
            (Console::Quiet, 0, false, CREATE_NO_WINDOW),
            (Console::Quiet, group, false, CREATE_NO_WINDOW | group),
            // A console: shared.
            (Console::Quiet, 0, true, 0),
            (Console::Quiet, group, true, group),
            // The explicit choices.
            (Console::Detached, 0, false, DETACHED_PROCESS),
            (Console::Detached, 0, true, DETACHED_PROCESS),
            (Console::Detached, group, true, DETACHED_PROCESS | group),
            // Console bits passed in `extra` are dropped.
            (Console::Quiet, CREATE_NEW_CONSOLE | group, true, group),
            (Console::Quiet, DETACHED_PROCESS, false, CREATE_NO_WINDOW),
            (Console::Quiet, CREATE_NO_WINDOW, true, 0),
            (Console::Detached, CREATE_NO_WINDOW, false, DETACHED_PROCESS),
            (
                Console::Detached,
                CONSOLE_BITS | group,
                true,
                DETACHED_PROCESS | group,
            ),
        ] {
            assert_eq!(
                creation_flags(console, extra, have),
                want,
                "{console:?} extra {extra:#x} have_console {have}"
            );
        }
    }
}
