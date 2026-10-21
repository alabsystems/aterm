// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-Unix `spawnfd` surface: provides [`command`], [`transparent`], and
//! [`InheritNothing`] matching the Unix API.
//!
//! Windows handles are non-inheritable by default (`std` marks only the
//! child's stdio handles inheritable under its spawn lock), so
//! [`InheritNothing`] is a no-op. [`command`] configures the child's Windows
//! console mode via [`crate::spawn::set_flags`] with [`Console::Quiet`]
//! (`CREATE_NO_WINDOW` when this process has no console, or shared console
//! when it has one).
//!
//! [`Console::Quiet`]: crate::spawn::Console::Quiet

use std::ffi::OsStr;
/// `std`'s own `Command`, under the one name a reviewed raw site takes it by:
/// the Unix `StdCommand`'s twin (its docs say why the alias is the one excuse
/// and why `StdCommand::new` is never spelled outside this crate).
#[expect(
    clippy::disallowed_types,
    reason = "the one name a reviewed raw std Command is taken by; off Unix spawnfd's Command is std's too"
)]
pub type StdCommand = std::process::Command;
/// `std`'s own types: what the Unix [`Command`], `Stdio` and `Child` stand in
/// for, so a spawn site shared with Unix names one set of types everywhere.
/// Off Unix there is no descriptor table to strip.
pub type Command = StdCommand;
pub use std::process::{Child, Stdio};

/// `spawn`, tried once: the refusal the Unix module tries again for a second (Linux's
/// `ETXTBSY`, an exec of a program something still holds open for writing) is not one a
/// Windows `CreateProcessW` gives, so a spawn site shared with Unix (atpkg's warm) is
/// spelled the same here.
///
/// # Errors
/// What `spawn` answered.
pub fn spawn_past_text_busy<T>(
    mut spawn: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    spawn()
}

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

/// Pass-through child builder off Unix: delegates to [`command`], since
/// Windows children inherit only explicitly marked handles.
#[must_use]
pub fn transparent(program: impl AsRef<OsStr>) -> Command {
    command(program)
}
