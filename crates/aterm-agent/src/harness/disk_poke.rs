// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE POKE: anything in this process that SEES the disk full — a write
//! that failed with `ENOSPC` or `EDQUOT`, a driven Claude Code's "Transcript
//! writes are failing (disk full — ENOSPC)" on its screen — asks the window's
//! disk watch to look NOW instead of at its next probe.
//!
//! A poke removes nothing: it asks for a measurement. What a pass then
//! removes is decided by the measured free figure and the watch's
//! witnesses, so a misread screen costs one `statvfs` and nothing else.
//! The host registers the one handler ([`set_handler`]); without one a poke
//! is only remembered ([`last`]), for `aterm harness disk` to show.

use std::sync::{Arc, Mutex, PoisonError};

/// What saw the disk full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    /// A driven agent's screen said so (`code`: `ENOSPC`, `EDQUOT`).
    Screen {
        /// The session.
        sid: String,
        /// The error code the screen named.
        code: String,
    },
    /// A write of aterm's own failed for want of space (`what` failed).
    Write(String),
}

impl Cause {
    /// One line for the log and the JSON.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Cause::Screen { sid, code } => format!("session {sid}'s screen says {code}"),
            Cause::Write(what) => format!("a write failed for want of space: {what}"),
        }
    }
}

/// The handler a poke calls.
pub type Handler = Arc<dyn Fn(&Cause) + Send + Sync>;

static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);
static LAST: Mutex<Option<(Cause, i64)>> = Mutex::new(None);

/// Register the one handler (the window's disk watch), replacing any.
pub fn set_handler(handler: Handler) {
    *HANDLER.lock().unwrap_or_else(PoisonError::into_inner) = Some(handler);
}

/// Ask the disk watch to look now, for `cause`, at `now` (unix seconds).
/// Remembered for [`last`]; handed to the handler when one is registered.
pub fn poke(cause: Cause, now: i64) {
    *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Some((cause.clone(), now));
    let handler = HANDLER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(h) = handler {
        h(&cause);
    }
}

/// [`poke`] at the system's now: for a writer with no clock of its own to
/// hand, the supervisor's journal ([`crate::supervise::Journal`]). Its loop
/// reads every clock through its `Fx` door, and a poke's arrival time is not
/// one of them: it is the host's, handed to the host's handler, and the loop
/// never observes it — so it is read here, outside the loop's source.
pub fn poke_now(cause: Cause) {
    poke(cause, crate::supervise::journal::unix_ms() / 1000);
}

/// The last poke and when it came, if any.
#[must_use]
pub fn last() -> Option<(Cause, i64)> {
    LAST.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Is `e` the file system saying it is out of space or quota (`ENOSPC`,
/// `EDQUOT`)?
#[must_use]
pub fn is_out_of_space(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
    )
}
