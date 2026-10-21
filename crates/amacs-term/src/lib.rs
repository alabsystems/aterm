// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `amacs` program (docs/DESIGN-amacs-2026-10-19.md §5-§7): `amacs FILE`,
//! the argv0 alias of the one `aterm` binary, and `aterm edit FILE`.
//!
//! [`run_client`] picks the face ([`decide`]). Inside an aterm window it runs
//! the native face's client ([`client`]): each file opens in a fresh Editor
//! tab beside the caller's, and the client waits, silently, until the tab is
//! closed. Everywhere else (over ssh, in tmux or screen, with `-nw`, or when
//! the window will not take the file) it runs the terminal face ([`tui`]),
//! which edits in the terminal itself with the same engine. The machine
//! protocol (A3) is planned.

// A contracted fn is written once, through `trust_contract_fn!`: under Trust's
// verifying session (`cfg(trust_verify)`) it carries its native `ensures`
// clause, and every other compiler (stock rustc in the foreign cells) gets the
// plain fn and never parses the clause. The cited theorem is stated over
// `trust_import_amacs_term__<module>__<fn>`, the kernel definition Trust mints
// from the compiled body, so the proof is about this code, not a copy of it.
// The arms are aterm-log's (docs/PLAN-tests-to-specs-2026-10-15.md, "The
// recipe that works"); they are defined before the modules that use them.
#[cfg(trust_verify)]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret
            $($clauses)+
        { $($body)* }
    };
}

#[cfg(not(trust_verify))]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret { $($body)* }
    };
}

#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

pub mod client;
pub mod decide;
mod render;
pub mod tui;

use std::ffi::OsString;
use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

/// The environment variable naming the aterm session that hosts this
/// terminal (`s-<hex>`), which every session's shell is given.
const PARENT_SESSION_ENV: &str = "ATERM_PARENT_SESSION_ID";

/// `amacs [ARGS]` / `aterm edit [ARGS]` (`args` past the program name or
/// verb): the whole program, to its exit status. `write` is the run's one
/// buffer-write capability, which the launcher mints
/// (`aterm_cli::amacs_launch`) and only the terminal face uses: the native
/// face's window does its own writing.
#[must_use]
pub fn run_client(args: Vec<OsString>, write: aterm_buffer::WriteCap) -> ExitCode {
    let mut tty = std::io::stderr();
    let args = match client::parse_args(&args) {
        Ok(client::Command::Help) => {
            return aterm_uds::stdout::answer_exit("amacs", client::HELP, 0);
        }
        Ok(client::Command::Edit(args)) => args,
        Err(why) => {
            client::say(&mut tty, &why);
            let _ = writeln!(tty, "{}", client::SYNOPSIS);
            return ExitCode::from(decide::EXIT_USAGE);
        }
    };
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    // The session the native face hands the file to, read once. A value that
    // is not Unicode (no aterm writes one) names no session, so such a run
    // edits in the terminal instead of asking a window it cannot name.
    let sid = match std::env::var(PARENT_SESSION_ENV) {
        Ok(sid) if !sid.is_empty() => Some(sid),
        Ok(_) | Err(std::env::VarError::NotPresent | std::env::VarError::NotUnicode(_)) => None,
    };
    let inputs = decide::Inputs {
        tty_flag: args.tty,
        wait: args.wait,
        no_wait: args.no_wait,
        parent_session: sid.is_some(),
        tmux: set("TMUX"),
        sty: set("STY"),
        stdin_tty: std::io::stdin().is_terminal(),
        stdout_tty: std::io::stdout().is_terminal(),
        // `[amacs] open_in` is read once the config parser lands (C1).
        open_in: decide::OpenIn::default(),
    };
    match (decide::decide(&inputs), sid) {
        (decide::Face::Exit(code, line), _) => {
            client::say(&mut tty, line.trim_start_matches("amacs: "));
            ExitCode::from(code)
        }
        // The native face needs `parent_session` (`decide::native_face`'s
        // proved contract), which is `sid.is_some()`: no `None` reaches it.
        (decide::Face::Terminal, _) | (decide::Face::Native, None) => {
            terminal_face(write, &args.targets, None)
        }
        (decide::Face::Native, Some(sid)) => {
            let for_word = client::for_word(client::parent_program().as_deref());
            let mut window = client::SessionWindow { sid };
            match client::edit_files(&args, for_word.as_deref(), &mut window, &mut tty) {
                client::Ending::Done => ExitCode::SUCCESS,
                // The window would not take a file: it and the files after it
                // are this terminal's.
                client::Ending::Terminal { why, from } => {
                    let rest = args.targets.get(from..).unwrap_or_default();
                    terminal_face(write, rest, Some(&why))
                }
                client::Ending::Exit(code) => ExitCode::from(code),
            }
        }
    }
}

/// The terminal face over `targets`, the program the edit is for named on
/// its modeline. `why`, when given, is why the window did not take them.
fn terminal_face(
    write: aterm_buffer::WriteCap,
    targets: &[client::Target],
    why: Option<&str>,
) -> ExitCode {
    let options = tui::Options {
        for_word: client::for_word(client::parent_program().as_deref()),
        ..tui::Options::default()
    };
    tui::run_tui(write, targets, options, why)
}
