// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`transparent`].

use super::{Image, LaneChild, Placement, platform};
use std::ffi::{OsStr, OsString};
use std::io;
use std::process::ExitStatus;

/// What [`TransparentCommand::spawn`] starts.
pub type TransparentChild = LaneChild;

/// `std::process::Command::new(program)` with arguments.
#[derive(Debug)]
pub struct TransparentCommand {
    program: OsString,
    args: Vec<OsString>,
    /// Variables set over this process's environment, in the order they were set.
    env: Vec<(OsString, OsString)>,
}

/// A [`TransparentCommand`] for `program`.
#[must_use]
pub fn transparent(program: impl AsRef<OsStr>) -> TransparentCommand {
    TransparentCommand {
        program: program.as_ref().to_owned(),
        args: Vec::new(),
        env: Vec::new(),
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

    /// Set `key` to `value` in the child's environment, over this process's own (the rest of
    /// which the child inherits, as `std::process::Command::env` does); a later set of the same
    /// key wins.
    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
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

    /// Start the child.
    /// # Errors
    /// As `std`'s: `InvalidInput` for a NUL, `NotFound` for a program that
    /// does not exist, `PermissionDenied` for one that may not be run, and
    /// whatever else the spawn answers.
    pub fn spawn(&mut self) -> io::Result<TransparentChild> {
        let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        for (key, value) in &self.env {
            match env.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1.clone_from(value),
                None => env.push((key.clone(), value.clone())),
            }
        }
        let image = Image::build(&self.program, &self.args, &env)?;
        let pid = platform::spawn(
            &image,
            &Placement {
                inherit_all: true,
                long_entry_ends: cfg!(target_vendor = "apple"),
                ..Placement::default()
            },
        )?;
        Ok(LaneChild { pid, status: None })
    }

    /// Run the child to its end ([`Self::spawn`], then wait).
    /// # Errors
    /// As [`Self::spawn`]'s, and waiting.
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn()?.wait()
    }
}
