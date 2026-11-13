// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Retired landing hand-over ([`HIDDEN_VERB`](crate::landing::HIDDEN_VERB), [`HandOver`](crate::landing::HandOver), [`shim_command`](crate::landing::shim_command), [`sweep`](crate::landing::sweep)).

use std::path::PathBuf;

use crate::store::{Layout, ToolName};

/// The hidden verb an older twin's prelude execs.
pub const HIDDEN_VERB: &str = "__landing";

/// The verb's operands, parsed: `<program> [<prefix>] -- [args…]`.
#[derive(Debug, PartialEq, Eq)]
pub struct HandOver<'a> {
    /// The agent program the twin stands for (`claude`), not yet `ToolName`-vetted.
    pub program: &'a str,
    /// The absolute prefix operand the twin passes, so the store is found with no `HOME`.
    pub prefix: Option<PathBuf>,
    /// The user's arguments, verbatim, with the one `--` the twin inserted stripped.
    pub args: &'a [String],
}

impl<'a> HandOver<'a> {
    /// `None` only with no program.
    #[must_use]
    pub fn parse(rest: &'a [String]) -> Option<Self> {
        let (program, rest) = rest.split_first()?;
        let (prefix, rest) = match rest.split_first() {
            Some((p, tail)) if p != "--" && std::path::Path::new(p).is_absolute() => {
                (Some(PathBuf::from(p)), tail)
            }
            _ => (None, rest),
        };
        let args = if rest.first().is_some_and(|a| a == "--") {
            &rest[1..]
        } else {
            rest
        };
        Some(Self {
            program,
            prefix,
            args,
        })
    }
}

/// The command the verb runs ([`Layout::shim`], [`crate::platform::exec_or_run`]).
#[must_use]
pub fn shim_command(
    layout: &Layout,
    tool: &ToolName,
    args: &[String],
) -> aterm_uds::spawnfd::StdCommand {
    #[expect(
        clippy::disallowed_types,
        reason = "a pass-through by exec: its caller's descriptors are the program's"
    )]
    let mut command = std::process::Command::new(layout.shim(tool));
    aterm_uds::spawn::set_flags(&mut command, 0);
    command.args(args);
    command
}

/// Remove `<prefix>/landing/` and `<prefix>/optin/` and everything in them.
pub fn sweep(layout: &Layout) {
    for dir in [layout.landing_dir(), layout.prefix.join("optin")] {
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.is_dir() => {
                let _ = std::fs::remove_dir_all(&dir);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&dir);
            }
            Err(_) => {}
        }
    }
}

/// The operands and the command, on every platform.
#[cfg(test)]
#[path = "landing_pure_tests.rs"]
mod pure_tests;

/// The sweep over a real directory.
#[cfg(all(test, unix))]
#[path = "landing_tests.rs"]
mod tests;

/// The hand-over on Windows: the `bin/<program>.cmd` shim runs as a child and its real exit
/// code comes back. Runs only on a Windows host.
#[cfg(all(test, windows))]
#[path = "landing_windows_tests.rs"]
mod windows_tests;
