// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Error type and process exit codes (`0` done, `1` refused, `2` usage, `3` could not run).

use std::fmt;

/// Why a verb did not finish.
#[derive(Debug)]
pub enum Error {
    /// A gate refused.
    Refused(String),
    /// The command line is wrong.
    Usage(String),
    /// Something this run needs is missing or did not answer.
    CannotRun(String),
}

impl Error {
    /// A refusal.
    pub fn refused(message: impl Into<String>) -> Self {
        Self::Refused(message.into())
    }

    /// A usage error.
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    /// A could-not-run.
    pub fn cannot_run(message: impl Into<String>) -> Self {
        Self::CannotRun(message.into())
    }

    /// An I/O failure while doing `what`: a could-not-run.
    pub fn io(what: impl fmt::Display, error: &std::io::Error) -> Self {
        Self::CannotRun(format!("{what}: {error}"))
    }

    /// The process exit code this error is.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Refused(_) => 1,
            Self::Usage(_) => 2,
            Self::CannotRun(_) => 3,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(m) => write!(f, "REFUSED — {m}"),
            Self::Usage(m) => write!(f, "usage: {m}"),
            Self::CannotRun(m) => write!(f, "could not run — {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// This crate's result.
pub type Result<T> = std::result::Result<T, Error>;
