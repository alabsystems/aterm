// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows console creation flags (`dwCreationFlags`) for child processes ([`Console`](crate::spawn::Console), [`set_flags`](crate::spawn::set_flags), [`set_gui_image_flags`](crate::spawn::set_gui_image_flags)).
use crate::spawnfd::StdCommand;

/// `DETACHED_PROCESS` (`WinBase.h`): spawns the child without a console ([`Console::Detached`]).
pub const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_CONSOLE` (`WinBase.h`): allocates a new visible console ([`creation_flags`] strips this bit).
pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
/// `CREATE_NEW_PROCESS_GROUP` (`WinBase.h`): starts a new console process group.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `CREATE_NO_WINDOW` (`WinBase.h`): allocates a windowless console for a console-subsystem child.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Mask of console creation flag bits controlled by [`Console`].
pub const CONSOLE_BITS: u32 = DETACHED_PROCESS | CREATE_NEW_CONSOLE | CREATE_NO_WINDOW;

/// Windows console policy for a spawned child process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Console {
    /// Inherits this process's console if attached, or uses `CREATE_NO_WINDOW` otherwise.
    Quiet,
    /// Detaches from any console (`DETACHED_PROCESS`), for GUI images only ([`set_gui_image_flags`]).
    Detached,
}

/// Computes `dwCreationFlags` for `console`, `extra`, and `have_console`.
#[must_use]
pub const fn creation_flags(console: Console, extra: u32, have_console: bool) -> u32 {
    let extra = extra & !CONSOLE_BITS;
    match console {
        Console::Quiet if have_console => extra,
        Console::Quiet => extra | CREATE_NO_WINDOW,
        Console::Detached => extra | DETACHED_PROCESS,
    }
}

/// Returns whether this process is currently attached to a Windows console.
#[cfg(windows)]
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn have_console() -> bool {
    let mut first = 0u32;
    // SAFETY: `first` is a one-element out-buffer that outlives the call, with its length
    // passed as 1; the API writes at most that many pids and returns the console's whole
    // count (0 on failure).
    unsafe { crate::win::ffi::GetConsoleProcessList(&mut first, 1) != 0 }
}

/// Returns `false` on non-Windows targets.
#[cfg(not(windows))]
#[must_use]
pub fn have_console() -> bool {
    false
}

/// Sets `cmd`'s creation flags for [`Console::Quiet`] plus `extra` ([`creation_flags`], [`have_console`]).
pub fn set_flags(cmd: &mut StdCommand, extra: u32) -> &mut StdCommand {
    set_console(cmd, Console::Quiet, extra)
}

/// Sets `cmd`'s creation flags for a GUI image ([`Console::Detached`]).
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
            (Console::Quiet, 0, false, CREATE_NO_WINDOW),
            (Console::Quiet, group, false, CREATE_NO_WINDOW | group),
            (Console::Quiet, 0, true, 0),
            (Console::Quiet, group, true, group),
            (Console::Detached, 0, false, DETACHED_PROCESS),
            (Console::Detached, 0, true, DETACHED_PROCESS),
            (Console::Detached, group, true, DETACHED_PROCESS | group),
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
