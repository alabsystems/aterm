// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spawnfd` off Unix: [`command`] and [`InheritNothing`], spelled as on Unix
//! (and `Command`, `Stdio`, `Child` as `std`'s), so a spawn site shared with
//! Unix (and a Windows-only one) is spelled the same everywhere.
//!
//! Nothing is stripped because there is no table of unflagged descriptors to
//! strip: a Windows child inherits only a handle marked inheritable, `std`
//! creates every handle it opens non-inheritable, and it marks the child's own
//! stdio copies inheritable only for the spawn (under its process-creation
//! lock).
//!
//! What a Windows child needs instead is its CONSOLE decided, and [`command`]
//! decides it: `Command::new` with [`crate::spawn::set_flags`] at
//! [`Console::Quiet`] — `CREATE_NO_WINDOW` exactly when THIS process has no
//! console (the window, whose bare console children each opened a VISIBLE
//! console window, measured 2026-09-30), and nothing when it has one (the child
//! shares it: its output, Ctrl+C, the code page). [`crate::spawn`]'s module docs
//! carry the measurements and why neither `CREATE_NO_WINDOW` always nor
//! `DETACHED_PROCESS` is the rule. A site that needs another console (the
//! windowed sibling's `Console::Detached`) or more flags (a new process group)
//! calls [`crate::spawn::set_gui_image_flags`] or [`crate::spawn::set_flags`] on
//! the returned `Command`, which REPLACES the word this set.
//!
//! [`Console::Quiet`]: crate::spawn::Console::Quiet

use std::ffi::OsStr;
/// `std`'s own types: what the Unix [`Command`], `Stdio` and `Child` stand in
/// for, so a spawn site shared with Unix names one set of types everywhere.
#[expect(
    clippy::disallowed_types,
    reason = "off Unix spawnfd's Command is std's: no descriptor table to strip"
)]
pub type Command = std::process::Command;
pub use std::process::{Child, Stdio};

/// The Unix strip's trait, a no-op here (the module docs say why).
pub trait InheritNothing {
    /// Nothing to install off Unix.
    fn inherit_nothing(&mut self) -> &mut Self;
}

impl InheritNothing for Command {
    fn inherit_nothing(&mut self) -> &mut Self {
        self
    }
}

/// `Command::new(program)` with its Windows console decided
/// ([`crate::spawn::Console::Quiet`]: windowless when this process has no
/// console, shared when it has one) — what the Unix [`command`] is, less the
/// strip it has no use for here. The decision is made now, when the `Command`
/// is built (`crate::spawn`'s "When the decision is made").
#[must_use]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::spawn::set_flags(&mut command, 0);
    command
}

/// What [`transparent`]'s command starts: `std`'s `Child`, under the Unix
/// pass-through spawn's name, so a holder waits on one type everywhere.
pub type TransparentChild = Child;

/// The pass-throughs' spawn off Unix: [`command`]'s `Command`. A Windows child
/// inherits only the handles marked inheritable whatever spawns it, so this is
/// the same child, console decided, that the pass-throughs started before the
/// Unix `spawnfd::transparent` existed (its docs say what Unix needs and why).
#[must_use]
pub fn transparent(program: impl AsRef<OsStr>) -> Command {
    command(program)
}
