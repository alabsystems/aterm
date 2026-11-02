// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`transparent`].

use super::{Image, LaneChild, Placement, platform};
use std::ffi::{OsStr, OsString};
use std::io;
use std::process::ExitStatus;

/// What [`TransparentCommand::spawn`] starts.
pub type TransparentChild = LaneChild;

/// An owned, non-cloneable native exec prepared before its caller grants dispatch.
///
/// The environment, argument strings, executable search and platform attributes are captured
/// during preparation. No spawn lock is held while this value waits for permission to execute.
#[must_use = "preparation does not execute the image"]
pub struct PreparedExec(platform::PreparedExec);

impl PreparedExec {
    /// Replace this process only if the original absolute `CLOCK_MONOTONIC` microsecond
    /// deadline has not elapsed at native dispatch, after acquiring any spawn lock.
    ///
    /// Descriptor flags, mask, stdio, group and session are preserved. The native image gets
    /// default SIGPIPE; a returned failure restores the previous action. The same signal
    /// disposition serialization requirement as [`TransparentCommand::exec`] applies.
    /// An elapsed deadline returns `TimedOut`; a failed clock read refuses dispatch.
    pub fn exec_until(self, deadline_us: u64) -> io::Error {
        self.0.exec_until(deadline_us)
    }

    #[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
    pub(super) fn exec_with(
        self,
        deadline_us: u64,
        now: impl FnOnce() -> io::Result<u64>,
        dispatch: impl FnOnce() -> io::Error,
    ) -> io::Error {
        self.0.exec_with(deadline_us, now, |_| dispatch())
    }

    #[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
    pub(super) fn captured(&self) -> (&Image, &std::ffi::CStr) {
        self.0.captured()
    }
}

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
    fn image(&self) -> io::Result<Image> {
        let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        for (key, value) in &self.env {
            match env.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1.clone_from(value),
                None => env.push((key.clone(), value.clone())),
            }
        }
        Image::build(&self.program, &self.args, &env)
    }

    /// Capture a native exec's environment, C strings, executable search and platform objects
    /// before waiting for a grant. This neither executes nor holds a spawn lock.
    ///
    /// A bare program name is resolved now; a later PATH change cannot select another image.
    /// Relative paths still require the caller to keep its working directory unchanged.
    ///
    /// # Errors
    /// Invalid C strings, failed executable search, unavailable native exec, or platform
    /// attribute preparation. No signal disposition or descriptor flag has been changed.
    pub fn prepare_exec(&mut self) -> io::Result<PreparedExec> {
        platform::PreparedExec::new(self.image()?).map(PreparedExec)
    }

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

    /// Replace this process, preserving its descriptor flags, signal mask, process group,
    /// session and stdio. As for [`Self::spawn`], the new image receives default SIGPIPE.
    /// This executes the native image itself; ENOEXEC never falls back to a system shell.
    /// A failed exec restores the original SIGPIPE action before returning its error. If
    /// restoration itself fails, the error names it and the caller must treat itself as terminal.
    ///
    /// Callers must serialize signal-disposition changes around an exec. This does not
    /// serialize another thread that independently changes SIGPIPE's process-wide action.
    pub fn exec(&mut self) -> io::Error {
        let image = match self.image() {
            Ok(image) => image,
            Err(error) => return error,
        };
        #[cfg(target_vendor = "apple")]
        {
            match platform::spawn(
                &image,
                &Placement {
                    inherit_all: true,
                    replace: true,
                    native_image: true,
                    long_entry_ends: true,
                    ..Placement::default()
                },
            ) {
                Ok(_) => io::Error::other("the in-place exec returned a child"),
                Err(error) => error,
            }
        }
        #[cfg(target_os = "linux")]
        {
            platform::exec(&image)
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        {
            let _ = image;
            io::Error::new(io::ErrorKind::Unsupported, "in-place exec is unavailable")
        }
    }

    /// Start the child.
    /// # Errors
    /// As `std`'s: `InvalidInput` for a NUL, `NotFound` for a program that
    /// does not exist, `PermissionDenied` for one that may not be run, and
    /// whatever else the spawn answers.
    pub fn spawn(&mut self) -> io::Result<TransparentChild> {
        let image = self.image()?;
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
