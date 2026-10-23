// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-Unix `spawnfd` surface providing [`command`], [`transparent`], and [`InheritNothing`].

use std::ffi::OsStr;

/// Alias for `std::process::Command` at reviewed raw spawn sites.
#[expect(
    clippy::disallowed_types,
    reason = "the one name a reviewed raw std Command is taken by; off Unix spawnfd's Command is std's too"
)]
pub type StdCommand = std::process::Command;

/// Non-Unix child command builder (`std::process::Command`).
pub type Command = StdCommand;
pub use std::process::{Child, Stdio};

/// Run `spawn` once (non-Unix twin of the Unix `ETXTBSY` retry helper).
///
/// # Errors
/// What `spawn` answered.
pub fn spawn_past_text_busy<T>(
    mut spawn: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    spawn()
}

/// The Unix descriptor-strip trait, a no-op off Unix.
pub trait InheritNothing {
    /// No-op off Unix.
    fn inherit_nothing(&mut self) -> &mut Self;
}

impl InheritNothing for Command {
    fn inherit_nothing(&mut self) -> &mut Self {
        self
    }
}

/// Build `Command::new(program)` configured with quiet Windows console flags.
#[must_use]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::spawn::set_flags(&mut command, 0);
    command
}

/// Child handle started by [`transparent`].
pub type TransparentChild = Child;

/// Pass-through child builder off Unix, delegating to [`command`].
#[must_use]
pub fn transparent(program: impl AsRef<OsStr>) -> Command {
    command(program)
}
