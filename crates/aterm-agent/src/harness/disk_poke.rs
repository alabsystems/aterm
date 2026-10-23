// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE POKE: anything in this process that SEES the disk full.

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
    /// An agent's wait (`await sys free>=`) needs `need` bytes the volume does not have: a DEMAND,
    /// which the next pass adopts.
    Demand {
        /// The session that waits.
        sid: String,
        /// The free bytes it needs.
        need: u64,
    },
}

impl Cause {
    /// One line for the log and the JSON.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Cause::Screen { sid, code } => format!("session {sid}'s screen says {code}"),
            Cause::Write(what) => format!("a write failed for want of space: {what}"),
            Cause::Demand { sid, need } => format!(
                "session {sid} waits for {} free (await sys)",
                super::human_bytes(*need)
            ),
        }
    }
}

/// The handler a poke calls.
pub type Handler = Arc<dyn Fn(&Cause) + Send + Sync>;

static HANDLER: Mutex<Option<Handler>> = Mutex::new(None);

/// Register the one handler (the window's disk watch), replacing any.
pub fn set_handler(handler: Handler) {
    *HANDLER.lock().unwrap_or_else(PoisonError::into_inner) = Some(handler);
}

/// Ask the disk watch to look now, for `cause`: handed to the handler when one is registered, and
/// nothing otherwise.
pub fn poke(cause: Cause) {
    let handler = HANDLER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(h) = handler {
        h(&cause);
    }
}

/// Is `e` the file system saying it is out of space or quota (`ENOSPC`, `EDQUOT`)?
#[must_use]
pub fn is_out_of_space(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
    )
}
