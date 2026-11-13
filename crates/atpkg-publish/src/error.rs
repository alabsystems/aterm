// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Error type and process exit codes (`0` done, `1` refused, `2` usage, `3` could not run).

use std::fmt;
use std::path::PathBuf;

/// Why a verb did not finish.
#[derive(Debug)]
pub enum Error {
    /// A gate refused.
    Refused(String),
    /// The command line is wrong.
    Usage(String),
    /// Something this run needs is missing or did not answer.
    CannotRun(String),
    /// A scoped controller's own status, retained rather than relabeled as a gate failure.
    ChildExit { code: u8, message: String },
    /// The index write committed, but anonymous readback did not confirm its quad.
    /// Retain this state so a caller never retries it as an uncommitted publish.
    PublishedUnconfirmed {
        /// The already committed index number.
        index_build: u64,
        /// The exact local signed quad that was committed.
        directory: PathBuf,
        /// The readback failure, preserving its refusal or transport classification.
        cause: Box<Error>,
    },
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

    /// This error with `note` after its message, keeping its kind and exit code: a fault
    /// found beside it that the error alone would hide. A published-but-unconfirmed index
    /// carries the note on its cause.
    #[must_use]
    pub(crate) fn with_note(self, note: &str) -> Self {
        match self {
            Self::Refused(m) => Self::Refused(format!("{m}; {note}")),
            Self::Usage(m) => Self::Usage(format!("{m}; {note}")),
            Self::CannotRun(m) => Self::CannotRun(format!("{m}; {note}")),
            Self::ChildExit { code, message } => Self::ChildExit {
                code,
                message: format!("{message}; {note}"),
            },
            Self::PublishedUnconfirmed {
                index_build,
                directory,
                cause,
            } => Self::PublishedUnconfirmed {
                index_build,
                directory,
                cause: Box::new(cause.with_note(note)),
            },
        }
    }

    /// The process exit code this error is.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Refused(_) => 1,
            Self::Usage(_) => 2,
            Self::CannotRun(_) => 3,
            Self::ChildExit { code, .. } => *code,
            Self::PublishedUnconfirmed { cause, .. } => cause.exit_code(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(m) => write!(f, "REFUSED — {m}"),
            Self::Usage(m) => write!(f, "usage: {m}"),
            Self::CannotRun(m) => write!(f, "could not run — {m}"),
            Self::ChildExit { message, .. } => write!(f, "{message}"),
            Self::PublishedUnconfirmed {
                index_build,
                directory,
                cause,
            } => write!(
                f,
                "atpkg-index-{index_build} was published, but its anonymous signed-quad readback is unconfirmed: {cause}. The committed quad is retained at {}. Reconcile this existing publication; do not create another index to retry it",
                directory.display()
            ),
        }
    }
}

impl std::error::Error for Error {}

/// This crate's result.
pub type Result<T> = std::result::Result<T, Error>;
