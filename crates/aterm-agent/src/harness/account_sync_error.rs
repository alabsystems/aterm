// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synchronization failures retain their scheduling meaning until presentation.

/// A failed attempt that says nothing about whether its captured input is usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    Busy,
    Changed,
    Unavailable,
}

/// A diagnostic together with the decision whether the same input may be retried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncError {
    retry: Option<Retry>,
    message: String,
}

impl SyncError {
    /// A failure the same input may be retried after, for `reason`. Making one records a retry
    /// in the trace scope armed on this thread (`aterm_uds::trace::retry`): the run that met it
    /// did not finish its work, so it is not kept as a duty's verdict and owes a rerun, whether
    /// its caller passes the failure on or drops it.
    #[must_use]
    pub fn retry(reason: Retry, message: impl Into<String>) -> Self {
        let message = message.into();
        aterm_uds::trace::retry(format!("a retryable sync failure ({reason:?}): {message}"));
        Self {
            retry: Some(reason),
            message,
        }
    }

    #[must_use]
    pub fn retry_reason(&self) -> Option<Retry> {
        self.retry
    }

    #[must_use]
    pub fn context(mut self, context: impl FnOnce(&str) -> String) -> Self {
        self.message = context(&self.message);
        self
    }

    /// End typed propagation at a presentation or non-scheduling boundary.
    #[must_use]
    pub fn into_message(self) -> String {
        self.message
    }
}

impl From<String> for SyncError {
    fn from(message: String) -> Self {
        Self {
            retry: None,
            message,
        }
    }
}

impl From<&str> for SyncError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SyncError {}
