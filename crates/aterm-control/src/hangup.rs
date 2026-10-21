// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHETHER THE CALLER IS STILL THERE: the connection a control lane serves,
//! and the check every blocking verb (`await`, `ready`, `wait`, `turn`,
//! `await inbox`, `post --wait`) makes on each wake. A killed client frees its
//! lane within [`HANGUP_POLL`] instead of at the verb's own deadline.
//!
//! The lane marks the connection it serves ([`ServingFd::enter`]); a parked
//! verb parks at most [`hangup_park`] between checks of [`caller_hung_up`],
//! and answers [`HUNG_UP_REPLY`] once it reads true. Here, below the window
//! crate, so the fabric endpoint's waits and the dispatcher's read one mark.

/// How often a parked blocking verb re-checks whether its caller is still
/// there. Below the ~250 ms a killed client's lane may stay held.
pub const HANGUP_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// The reply a blocking verb returns when its caller hung up. Never read by
/// anyone — the connection is gone — but spelled for the file log and tests.
pub const HUNG_UP_REPLY: &str = "ERR hangup\n";

#[cfg(unix)]
thread_local! {
    /// Descriptor of the connection this lane is serving, for
    /// [`caller_hung_up`]. Set by [`ServingFd::enter`].
    static SERVING_FD: std::cell::Cell<Option<std::os::fd::RawFd>> =
        const { std::cell::Cell::new(None) };
}

/// Whether the client of the connection this lane serves has closed it, so no
/// answer can be read: the request policy of [`aterm_uds::hangup::PeerState`].
/// A half-closed client (request sent, write side shut) is still waiting and
/// not hung up, nor is one with a request queued. `false` off a serving lane.
#[must_use]
pub fn caller_hung_up() -> bool {
    #[cfg(unix)]
    {
        SERVING_FD
            .with(std::cell::Cell::get)
            .is_some_and(aterm_uds::hangup::fd_peer_closed)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// The descriptor of the connection this lane serves ([`ServingFd::enter`]),
/// `None` off a serving lane: what reads the connection's kernel peer reads.
#[cfg(unix)]
#[must_use]
pub fn serving_fd() -> Option<std::os::fd::RawFd> {
    SERVING_FD.with(std::cell::Cell::get)
}

/// Cap one park of a blocking verb at [`HANGUP_POLL`] while serving a
/// connection; with no connection it parks as asked.
#[must_use]
pub fn hangup_park(park: std::time::Duration) -> std::time::Duration {
    #[cfg(unix)]
    {
        if SERVING_FD.with(std::cell::Cell::get).is_some() {
            return park.min(HANGUP_POLL);
        }
    }
    park
}

/// Marks this thread as serving one connection for [`caller_hung_up`] until
/// dropped, when the connection it was serving before (a nested serve, say a
/// subscription's push lane inside a request lane) is the mark again; the
/// connection must outlive the guard.
pub struct ServingFd {
    #[cfg(unix)]
    outer: Option<std::os::fd::RawFd>,
}

impl ServingFd {
    /// Mark the connection on descriptor `fd` as the one this thread serves.
    #[cfg(unix)]
    #[must_use]
    pub fn enter(fd: std::os::fd::RawFd) -> Self {
        Self {
            outer: SERVING_FD.with(|cell| cell.replace(Some(fd))),
        }
    }

    /// No descriptor to probe off unix: nothing is marked.
    #[cfg(not(unix))]
    #[must_use]
    pub fn enter() -> Self {
        Self {}
    }
}

impl Drop for ServingFd {
    fn drop(&mut self) {
        #[cfg(unix)]
        SERVING_FD.with(|cell| cell.set(self.outer));
    }
}
