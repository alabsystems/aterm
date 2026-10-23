// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Caller hangup detection ([`HANGUP_POLL`], [`ServingFd::enter`], [`hangup_park`],
//! [`caller_hung_up`], [`HUNG_UP_REPLY`]).

/// How often a parked blocking verb re-checks whether its caller is still there.
pub const HANGUP_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// The reply a blocking verb returns when its caller hung up.
pub const HUNG_UP_REPLY: &str = "ERR hangup\n";

#[cfg(unix)]
thread_local! {
    /// Descriptor of the connection this lane serves ([`caller_hung_up`], [`ServingFd::enter`]).
    static SERVING_FD: std::cell::Cell<Option<std::os::fd::RawFd>> =
        const { std::cell::Cell::new(None) };
}

/// Whether the served client closed the connection ([`aterm_uds::hangup::PeerState`]).
#[must_use]
#[inline]
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

/// The descriptor of the connection this lane serves.
#[cfg(unix)]
#[must_use]
#[inline]
pub fn serving_fd() -> Option<std::os::fd::RawFd> {
    SERVING_FD.with(std::cell::Cell::get)
}

/// Cap one park of a blocking verb at [`HANGUP_POLL`] while serving a connection.
#[must_use]
#[inline]
pub fn hangup_park(park: std::time::Duration) -> std::time::Duration {
    #[cfg(unix)]
    {
        if SERVING_FD.with(std::cell::Cell::get).is_some() {
            return park.min(HANGUP_POLL);
        }
    }
    park
}

/// Marks this thread as serving one connection for [`caller_hung_up`] until dropped.
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

    /// No descriptor to probe off unix.
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
