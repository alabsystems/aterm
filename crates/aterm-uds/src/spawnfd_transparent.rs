// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`transparent`]: THE ONE SPAWN WHOSE CHILD INHERITS EVERYTHING — every
//! descriptor this process holds without close-on-exec, at its own number, as
//! upstream `std::process::Command` handed it on before Trust's SP-1. It is
//! for the PASS-THROUGHS ONLY: a verb that stands in for a program its caller
//! ran and waits for it (`aterm pkg lease -- <cmd>`, `aterm ship`). Their
//! caller's descriptors belong to that program — a make jobserver's pipe named
//! in `MAKEFLAGS`, a `3>file` redirect, a number handed down in the
//! environment — so dropping them would break the program, not protect it.
//! Every other child is built by [`super::command`], which hands on its stdio
//! and nothing else (clippy.toml: one child constructor).
//!
//! WHY IT IS NOT `std`'s `Command`: Trust's std (SP-1, trust 6eb03c9a18) hands
//! a `Command` child fds 0-2 plus what `CommandExt::inherit_fd` names, and a
//! std without SP-1 has neither `inherit_fd` nor `inherit_all_fds` (unstable
//! where they exist), so no `std` spelling passes the table through on both.
//! This one owes nothing to the std it links: on Darwin one `posix_spawn`
//! without `POSIX_SPAWN_CLOEXEC_DEFAULT`, elsewhere a fork with no strip.
//!
//! What the child gets is what `Command::new(program).args(args)` gave it
//! through upstream std's `spawn` and `status`, and nothing a caller here sets:
//!
//! * **the program** — `posix_spawnp`'s search on this process's `PATH` (a
//!   name holding a `/` is that file); a missing one is `NotFound`, one that
//!   may not be run `PermissionDenied`, a NUL `InvalidInput`, and a failed
//!   exec is an `Err`, never a child.
//! * **environment and working directory** — this process's, as they stand at
//!   the spawn.
//! * **stdio** — this process's three slots, named by no file action: each
//!   reaches the child exactly as this process holds it (a closed one stays
//!   closed, as `std` leaves it).
//! * **every other descriptor** — handed on unless it is close-on-exec.
//! * **signals** — the spawning thread's mask and this process's
//!   dispositions, with `SIGPIPE` back at its default: `std`'s rule.
//!
//! [`TransparentChild`] is [`super::LaneChild`]: `std`'s `Child`'s process
//! half by pid, reaped exactly once.
//!
//! An `exec` needs none of this: an exec keeps the whole table on every std
//! (SP-1 leaves `CommandExt::exec` as upstream has it), so the pass-throughs
//! that become their program (`aterm pkg run`, the reroutes, the landing
//! shim) still `exec` a `std::process::Command`, each a reviewed `#[expect]`.

use super::{Image, LaneChild, Placement, platform};
use std::ffi::{OsStr, OsString};
use std::io;
use std::process::ExitStatus;

/// What [`TransparentCommand::spawn`] starts: `std::process::Child`'s process
/// half (`id`, `wait`, `try_wait`, `kill`), by pid, reaped exactly once.
pub type TransparentChild = LaneChild;

/// `std::process::Command::new(program)` with arguments, started so that the
/// child inherits this process's WHOLE table (the module docs). Built by
/// [`transparent`].
#[derive(Debug)]
pub struct TransparentCommand {
    program: OsString,
    args: Vec<OsString>,
}

/// A [`TransparentCommand`] for `program`: a PASS-THROUGH's child, which holds
/// every descriptor this process holds without close-on-exec — DELIBERATELY,
/// because a pass-through's caller handed them to the program it stands in
/// for. Never for a child of aterm's own: that is [`super::command`].
#[must_use]
pub fn transparent(program: impl AsRef<OsStr>) -> TransparentCommand {
    TransparentCommand {
        program: program.as_ref().to_owned(),
        args: Vec::new(),
    }
}

impl TransparentCommand {
    /// Append one argument.
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Append arguments.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    /// The program, as it was named.
    #[must_use]
    pub fn get_program(&self) -> &OsStr {
        &self.program
    }

    /// The arguments after the program.
    pub fn get_args(&self) -> impl ExactSizeIterator<Item = &OsStr> {
        self.args.iter().map(OsString::as_os_str)
    }

    /// Start the child: this process's stdio, environment, working directory
    /// and every descriptor it holds without close-on-exec.
    ///
    /// # Errors
    /// As `std`'s: `InvalidInput` for a NUL, `NotFound` for a program that
    /// does not exist, `PermissionDenied` for one that may not be run, and
    /// whatever else the spawn answers.
    pub fn spawn(&mut self) -> io::Result<TransparentChild> {
        let env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        let image = Image::build(&self.program, &self.args, &env)?;
        let pid = platform::spawn(
            &image,
            &Placement {
                inherit_all: true,
                // The child's `PATH` is this process's own, so `std` searched
                // it with `posix_spawnp` (Darwin's walk; Linux resolves the
                // name before its fork either way).
                long_entry_ends: cfg!(target_vendor = "apple"),
                ..Placement::default()
            },
        )?;
        Ok(LaneChild { pid, status: None })
    }

    /// Run the child to its end ([`Self::spawn`], then wait).
    ///
    /// # Errors
    /// As [`Self::spawn`]'s, and waiting.
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn()?.wait()
    }
}
