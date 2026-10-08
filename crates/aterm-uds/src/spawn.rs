// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which Windows CONSOLE a child process gets — decided in one place, when its
//! `Command` is built: by `aterm_uds::spawnfd::command`, every shipped child's
//! constructor, which applies [`Console::Quiet`] off Unix, and by [`set_flags`].
//! Off Windows nothing here changes a `Command`.
//!
//! ## The defect this exists to prevent (measured 2026-09-30)
//!
//! The windowed image (`aterm-gui.exe`, PE subsystem GUI) has no console: none
//! is given to a GUI-subsystem process, and the one it attaches to when typed at
//! a shell is given back before the window opens. Windows hands every
//! CONSOLE-subsystem child such a parent starts without a console flag a NEW,
//! VISIBLE console window. In ten minutes the window started `curl.exe` 58
//! times (atpkg's index probe and the vendor-direct watch) and 42 visible
//! console windows were caught; every `atpkg.exe` pass kept its console on
//! screen for the whole pass (9–18 s), and so did `atpkg machine apply` at
//! launch, `hostname.exe` at the first tab label, `aterm.exe fabric` from the
//! Fabric menu and `aterm.exe drive ledger` from the ledger key.
//!
//! The child cannot cure this itself: a child that lives 126 ms still flashes,
//! and one whose first act is `FreeConsole` flashes before its `main` runs
//! (both measured). The LAUNCHER must decide, and this module is where it does.
//!
//! ## The rule: `CREATE_NO_WINDOW` exactly when this process has no console
//!
//! [`Console::Quiet`], the default (every child `aterm_uds::spawnfd::command`
//! builds), adds `CREATE_NO_WINDOW` when THIS process is attached to no console
//! ([`have_console`]) and nothing when it has one:
//!
//! * **No console (the window):** the child gets a WINDOWLESS console of its
//!   own — `GetConsoleWindow` is NULL, `GetConsoleProcessList` counts 1, and no
//!   window appears (measured, including the real `curl.exe -V`, which exited 0
//!   with its output read over pipes). Its own plain console children SHARE that
//!   windowless console (measured), so what `atpkg.exe` starts in turn (curl,
//!   `cmd /C mklink`, a `__warm` pass) is covered by the one flag at the top.
//! * **A console (a terminal session, `aterm pkg …` typed at a prompt, a
//!   ConPTY tab):** the child SHARES it, as it always did: its output reaches
//!   the parent's screen and Ctrl+C reaches it.
//!
//! **Why not `CREATE_NO_WINDOW` always.** From a parent that HAS a console, the
//! flag gives the child a private console instead of the parent's, and measured
//! from such a parent whose child inherited its console handles: `WriteFile` on
//! the inherited stdout fails with `ERROR_INVALID_HANDLE` (6), so the output is
//! silently lost; a prompt on `CONOUT$` goes to a console nobody can see; Ctrl+C
//! never reaches it (an `aterm pkg …` curl would be orphaned); its code page
//! drops from the parent's 65001 to 437; and the extra conhost costs ~10 ms a
//! spawn (20 spawns: 29.3/28.8 ms each with no flag, 38.3/39.6 ms with it). A
//! parent with no console has none of those to lose: its child would have had a
//! new console with code page 437 anyway, and only the window goes.
//!
//! **Why not `DETACHED_PROCESS`.** A detached child has no console at all, and
//! every plain console child IT starts then gets a new VISIBLE one (measured).
//! [`Console::Detached`] is only for a GUI image (the console image handing a
//! window to `aterm-gui.exe`), which has no console under any flag.
//!
//! **Other flags compose through `extra`.** `CREATE_NEW_PROCESS_GROUP` alone
//! from a parent with no console gives a VISIBLE window (measured: the codex
//! daemon-update verb's flag before this module); with `CREATE_NO_WINDOW` it is
//! windowless. The console bits themselves (`DETACHED_PROCESS`,
//! `CREATE_NEW_CONSOLE`, `CREATE_NO_WINDOW`) come only from [`Console`]: any in
//! `extra` are dropped.
//!
//! **[`set_flags`] is the ONLY setter.** std's
//! `std::os::windows::process::CommandExt::creation_flags` REPLACES the whole
//! flag word (measured: `CREATE_NO_WINDOW` then `CREATE_NEW_PROCESS_GROUP`
//! shared the parent's console), so a second call anywhere after this module's
//! silently brings the flash back. [`set_flags`] replaces the word too, which is
//! how a site overrides the quiet choice `spawnfd::command` made: it calls
//! [`set_flags`] on that `Command` with the whole choice (`Console::Detached`
//! for the windowed sibling; `Console::Quiet` plus `CREATE_NEW_PROCESS_GROUP`
//! for the codex daemon-update), and a raw `Command` B27 allows (a transparent
//! pass-through) gets its console from [`set_flags`] the same way.
//!
//! A GUI-subsystem target ignores every console flag (measured: a WinForms
//! child showed its form, with no console, under no flag, `CREATE_NO_WINDOW`,
//! `DETACHED_PROCESS` and `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`), so a
//! caller never needs to know which subsystem the program it starts was built
//! for.
//!
//! ## When the decision is made
//!
//! When the `Command` is built (`spawnfd::command`, [`set_flags`]), not when
//! it is spawned. In aterm-gui the two agree because the window
//! attaches to its parent's console (`win32::attach_parent_console`) and gives
//! it back (`win32::release_attached_console`) in `main_entry`, before any lane
//! spawns anything. A later `AttachConsole`/`FreeConsole` must keep that order.
//! It is also why the answer is asked per `Command` and never latched once at
//! startup.
//!
//! ## The fence
//!
//! B27 of `tools/grep_guard.sh` keeps every shipped `Command` on
//! `aterm_uds::spawnfd::command`, which applies this rule off Unix; its
//! allow-list names the raw exceptions. B28 keeps the decision here, in shipped
//! code: std's `creation_flags` setter appears nowhere but this file, raw
//! `CreateProcess*` / `ShellExecute*` / `WinExec` only in the ConPTY seam and
//! the URL opener (by count), an explicit [`Console::Detached`] or
//! [`Console::Visible`] only at the windowed sibling, no `Command` builder of
//! this module's is called from outside aterm-uds (one would skip the Unix
//! inheritance strip; the two it had, `command` and `command_as`, were removed
//! when B27 landed), and each raw site B27 allows by fn that Windows reaches
//! calls [`set_flags`] itself (B28e; the Unix-only rows are listed there). The
//! one raw spawn without this rule is `aterm-verify`'s toolchain probes (B27's
//! one whole-crate row that ships): dependency-free by charter, they are
//! reached only from `aterm help rust`, typed at a console whose children share
//! it, where [`Console::Quiet`] would add nothing; started with no console at
//! all, each probe would show a console window.
//!
//! ## What is not here
//!
//! * The ConPTY spawn (`aterm-pty`'s `CreateProcessW` with
//!   `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`): the pseudoconsole IS that child's
//!   console, and it shows no window (measured). `CREATE_NO_WINDOW` beside the
//!   attribute is unmeasured; do not add it. Any other raw `CreateProcessW` that
//!   starts a console image calls [`creation_flags`] with [`have_console`].
//! * `wShowWindow`/`SW_HIDE`: measured to keep the window hidden, but std's
//!   `CommandExt::show_window` is unstable (E0658) and the public snapshot builds
//!   on stock stable, so `creation_flags` is the only lever.
//!
//! [`Console`]: crate::spawn::Console
//! [`Console::Quiet`]: crate::spawn::Console::Quiet
//! [`Console::Detached`]: crate::spawn::Console::Detached
//! [`Console::Visible`]: crate::spawn::Console::Visible
//! [`creation_flags`]: crate::spawn::creation_flags
//! [`have_console`]: crate::spawn::have_console
//! [`set_flags`]: crate::spawn::set_flags

use std::process::Command;

/// `DETACHED_PROCESS` (WinBase.h): the child gets NO console — neither this
/// process's nor a new one. For a GUI image only ([`Console::Detached`]).
///
/// NOT `CREATE_NEW_PROCESS_GROUP` either, the other flag that sounds like a
/// detach: it starts the child with Ctrl+C DISABLED, and every shell a window
/// started that way spawns inherits the setting, so no tab could interrupt a
/// command (`aterm`'s `windowed_sibling`, where this constant lived until
/// 2026-09-30).
pub const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_CONSOLE` (WinBase.h): the child gets a new, VISIBLE console
/// ([`Console::Visible`]).
pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
/// `CREATE_NEW_PROCESS_GROUP` (WinBase.h): the child leads a new console process
/// group, with Ctrl+C disabled in it. Not a console bit: it composes through
/// `extra`.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `CREATE_NO_WINDOW` (WinBase.h): a console image gets a console of its own
/// with NO window. Ignored for a GUI image.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The bits that decide a child's console. They come from [`Console`] alone:
/// [`creation_flags`] drops any a caller passes in `extra`.
pub const CONSOLE_BITS: u32 = DETACHED_PROCESS | CREATE_NEW_CONSOLE | CREATE_NO_WINDOW;

/// Which console a Windows child gets. Ignored off Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Console {
    /// The default. THIS process's console when it has one (shared output,
    /// Ctrl+C, console modes); otherwise a WINDOWLESS console of the child's
    /// own (`CREATE_NO_WINDOW`), which its console children share.
    Quiet,
    /// No console at all (`DETACHED_PROCESS`): only for a GUI image, such as
    /// the windowed sibling the console image starts. A console image started
    /// this way gives each plain console child of its own a VISIBLE console.
    Detached,
    /// A new VISIBLE console (`CREATE_NEW_CONSOLE`): a window someone asked for.
    Visible,
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
        Console::Visible => extra | CREATE_NEW_CONSOLE,
    }
}

/// Whether THIS process is attached to a console right now: a visible one, a
/// windowless one (`CREATE_NO_WINDOW`) or a ConPTY pseudoconsole.
///
/// `GetConsoleProcessList`, never `GetConsoleWindow`: the latter is NULL for a
/// windowless console, so it cannot tell one from none (measured). The list
/// call fails (0, `ERROR_INVALID_HANDLE`) exactly when there is no console;
/// `GetConsoleCP` measured the same on every state.
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

/// Set `cmd`'s creation flags for `console` plus `extra`, decided now
/// ([`creation_flags`] over [`have_console`]). THE ONLY creation-flags setter:
/// std's `creation_flags` replaces the whole word, so any later call drops
/// `CREATE_NO_WINDOW` (module docs). It replaces the word as well, so called on
/// a `Command` `spawnfd::command` built it overrides that quiet choice with the
/// whole of this one. A no-op off Windows.
pub fn set_flags(cmd: &mut Command, console: Console, extra: u32) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
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

    /// The Win32 values, from WinBase.h. `DETACHED_PROCESS` came here from
    /// `aterm`'s `windowed_sibling` with its pin.
    #[test]
    fn the_flag_values_are_the_win32_ones() {
        assert_eq!(DETACHED_PROCESS, 0x8);
        assert_eq!(CREATE_NEW_CONSOLE, 0x10);
        assert_eq!(CREATE_NEW_PROCESS_GROUP, 0x200);
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
        assert_eq!(CONSOLE_BITS, 0x0800_0018);
        assert_eq!(
            CONSOLE_BITS & CREATE_NEW_PROCESS_GROUP,
            0,
            "not a console bit"
        );
    }

    /// The rule, whole: a quiet child is windowless only when this process has
    /// no console, the explicit choices ignore whether it has one, and the
    /// console bits come from the choice alone.
    #[test]
    fn the_console_is_decided_by_the_choice_and_whether_this_process_has_one() {
        let group = CREATE_NEW_PROCESS_GROUP;
        for (console, extra, have, want) in [
            // A window (no console): windowless, with or without a group.
            (Console::Quiet, 0, false, CREATE_NO_WINDOW),
            (Console::Quiet, group, false, CREATE_NO_WINDOW | group),
            // A console: shared, as before this module.
            (Console::Quiet, 0, true, 0),
            (Console::Quiet, group, true, group),
            // The explicit choices.
            (Console::Detached, 0, false, DETACHED_PROCESS),
            (Console::Detached, 0, true, DETACHED_PROCESS),
            (Console::Detached, group, true, DETACHED_PROCESS | group),
            (Console::Visible, 0, false, CREATE_NEW_CONSOLE),
            (Console::Visible, 0, true, CREATE_NEW_CONSOLE),
            // Console bits passed in `extra` are dropped.
            (Console::Quiet, CREATE_NEW_CONSOLE | group, true, group),
            (Console::Quiet, DETACHED_PROCESS, false, CREATE_NO_WINDOW),
            (Console::Quiet, CREATE_NO_WINDOW, true, 0),
            (Console::Detached, CREATE_NO_WINDOW, false, DETACHED_PROCESS),
            (
                Console::Visible,
                CONSOLE_BITS | group,
                true,
                CREATE_NEW_CONSOLE | group,
            ),
        ] {
            assert_eq!(
                creation_flags(console, extra, have),
                want,
                "{console:?} extra {extra:#x} have_console {have}"
            );
        }
    }

    /// Every flag word holds exactly one console bit, or none (a quiet child of
    /// a console holder): two would leave the choice to Windows' precedence
    /// (measured: `DETACHED_PROCESS` wins over `CREATE_NO_WINDOW`).
    #[test]
    fn no_flag_word_carries_two_console_bits() {
        for console in [Console::Quiet, Console::Detached, Console::Visible] {
            for have in [false, true] {
                for extra in [0, CREATE_NEW_PROCESS_GROUP, CONSOLE_BITS, u32::MAX] {
                    let bits = creation_flags(console, extra, have) & CONSOLE_BITS;
                    assert!(
                        bits.count_ones() <= 1,
                        "{console:?} extra {extra:#x} have_console {have}: {bits:#x}"
                    );
                }
            }
        }
    }

    /// Off Windows [`super::set_flags`] sets nothing, whatever the choice: the
    /// `Command` keeps its program and arguments, and the answer to "a
    /// console?" is no.
    #[cfg(not(windows))]
    #[test]
    fn off_windows_set_flags_changes_nothing() {
        let mut cmd = std::process::Command::new("/bin/true");
        super::set_flags(&mut cmd, Console::Detached, CREATE_NEW_PROCESS_GROUP);
        assert_eq!(cmd.get_program(), "/bin/true");
        assert_eq!(cmd.get_args().count(), 0);
        assert!(!super::have_console());
    }
}
