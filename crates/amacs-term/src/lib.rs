// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `amacs` program (docs/DESIGN-amacs-2026-10-19.md §5-§7): `amacs FILE`, the argv0 alias of
//! the one `aterm` binary, and `aterm edit FILE`.

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

/// `amacs [ARGS]` / `aterm edit [ARGS]` (`args` past the program name or verb): the whole program,
/// to its exit status.
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
        open_in: decide::OpenIn::default(),
    };
    match (decide::decide(&inputs), sid) {
        (decide::Face::Exit(code, line), _) => {
            client::say(&mut tty, line.trim_start_matches("amacs: "));
            ExitCode::from(code)
        }
        (decide::Face::Terminal, _) | (decide::Face::Native, None) => {
            terminal_face(write, &args.targets, None)
        }
        (decide::Face::Native, Some(sid)) => {
            let for_word = client::for_word(client::parent_program().as_deref());
            let mut window = client::SessionWindow { sid };
            match client::edit_files(&args, for_word.as_deref(), &mut window, &mut tty) {
                client::Ending::Done => ExitCode::SUCCESS,
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
