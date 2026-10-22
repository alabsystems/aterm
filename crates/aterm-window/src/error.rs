// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/error.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (Added
// `EventLoopError::DisplayLost`: a display server that died under the loop is told apart from
// every other way a run fails.)

use std::{error, fmt};

use crate::platform_impl;

// TODO: Rename
/// An error that may be generated when requesting Winit state
#[derive(Debug)]
pub enum ExternalError {
    /// The operation is not supported by the backend.
    NotSupported(NotSupportedError),
    /// The operation was ignored.
    Ignored,
    /// The OS cannot perform the operation.
    Os(OsError),
}

/// The error type for when the requested operation is not supported by the backend.
#[derive(Clone)]
pub struct NotSupportedError {
    _marker: (),
}

/// The error type for when the OS cannot perform the requested operation.
#[derive(Debug)]
pub struct OsError {
    line: u32,
    file: &'static str,
    error: platform_impl::OsError,
}

/// A general error that may occur while running the Winit event loop
#[derive(Debug)]
pub enum EventLoopError {
    /// The operation is not supported by the backend.
    NotSupported(NotSupportedError),
    /// The OS cannot perform the operation.
    Os(OsError),
    /// The event loop can't be re-created.
    RecreationAttempt,
    /// Application has exit with an error status.
    ExitFailure(i32),
    /// The connection to the display server died under the loop: its transport ended (end of
    /// file, `ECONNRESET`, `EPIPE`, or a hang-up or error on its socket), as a Wayland compositor
    /// or an X server that crashed, was killed or restarted ends it. The error says how. Only
    /// that ends a run with this variant: a failure of the loop's own (a protocol error, a
    /// connection the client library closed, a failed `poll`) is one of the others.
    DisplayLost(std::io::Error),
}

impl From<OsError> for EventLoopError {
    #[inline]
    fn from(value: OsError) -> Self {
        Self::Os(value)
    }
}

impl NotSupportedError {
    #[inline]
    #[allow(dead_code)]
    pub(crate) const fn new() -> NotSupportedError {
        NotSupportedError { _marker: () }
    }
}

impl OsError {
    #[inline]
    #[allow(dead_code)]
    pub(crate) const fn new(
        line: u32,
        file: &'static str,
        error: platform_impl::OsError,
    ) -> OsError {
        OsError { line, file, error }
    }
}

#[allow(unused_macros)]
macro_rules! os_error {
    ($error:expr) => {{
        crate::error::OsError::new(line!(), file!(), $error)
    }};
}

impl fmt::Display for OsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.pad(&format!("os error at {}:{}: {}", self.file, self.line, self.error))
    }
}

impl fmt::Display for ExternalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            ExternalError::NotSupported(e) => e.fmt(f),
            ExternalError::Ignored => write!(f, "Operation was ignored"),
            ExternalError::Os(e) => e.fmt(f),
        }
    }
}

impl fmt::Debug for NotSupportedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.debug_struct("NotSupportedError").finish()
    }
}

impl fmt::Display for NotSupportedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.pad("the requested operation is not supported by Winit")
    }
}

impl fmt::Display for EventLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            EventLoopError::RecreationAttempt => write!(f, "EventLoop can't be recreated"),
            EventLoopError::NotSupported(e) => e.fmt(f),
            EventLoopError::Os(e) => e.fmt(f),
            EventLoopError::ExitFailure(status) => write!(f, "Exit Failure: {status}"),
            EventLoopError::DisplayLost(why) => write!(f, "the display connection ended: {why}"),
        }
    }
}

impl error::Error for OsError {}
impl error::Error for ExternalError {}
impl error::Error for NotSupportedError {}
impl error::Error for EventLoopError {}
