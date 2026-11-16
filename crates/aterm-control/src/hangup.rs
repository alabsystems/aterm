// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Caller hangup detection ([`HANGUP_POLL`], [`ServingFd::enter`], [`hangup_park`],
//! [`caller_hung_up`], [`HUNG_UP_REPLY`]), and the bound of a park the hang-up bell rings
//! ([`ARMED_POLL`], [`armed_park`]) on the connection a lane serves ([`with_served`]).

/// How often a parked blocking verb re-checks whether its caller is still there.
pub const HANGUP_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// How long one wait of a park the hang-up bell rings through a condition variable runs at most
/// while its arm stands ([`armed_park`]): a notify sent between the park's last ask after its
/// caller and its wait finds no waiter and is lost, and this bounds how long such a lost ring
/// leaves a hung-up caller unheard.
pub const ARMED_POLL: std::time::Duration = std::time::Duration::from_secs(5);

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

/// Cap one wait of a park the hang-up bell rings through a condition variable at
/// [`ARMED_POLL`] while its arm stands.
#[must_use]
#[inline]
pub fn armed_park(park: std::time::Duration) -> std::time::Duration {
    park.min(ARMED_POLL)
}

/// Run `f` with the connection this thread serves ([`ServingFd::enter`]) borrowed, or with
/// `None` off a serving lane. The borrow ends with `f`, so an arm of the hang-up bell made on it
/// inside `f` ends there too.
#[cfg(unix)]
pub fn with_served<R>(f: impl FnOnce(Option<std::os::fd::BorrowedFd<'_>>) -> R) -> R {
    let served = serving_fd().map(|fd| {
        // SAFETY: SERVING_FD names the connection this thread serves, which a frame up this
        // thread's stack keeps open until the ServingFd it entered drops ([`ServingFd::enter`]).
        // That guard stands for the whole of this call: `f` runs on this thread inside that
        // frame, and the borrow cannot outlive `f`.
        unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }
    });
    f(served)
}

/// Marks this thread as serving one connection for [`caller_hung_up`] until dropped.
pub struct ServingFd {
    #[cfg(unix)]
    outer: Option<std::os::fd::RawFd>,
}

impl ServingFd {
    /// Mark the connection on descriptor `fd` as the one this thread serves. Its caller keeps
    /// `fd` open, as that connection, until the guard drops: a park on this thread borrows it
    /// for the hang-up bell ([`with_served`]).
    ///
    /// It starts the process's hang-up bell first ([`aterm_uds::hangup::Bell::start`]), so a
    /// park on this thread arms it without starting it. A park arms that bell only on a thread
    /// inside a `ServingFd` (it arms only the connection served here), often under the lock of
    /// the state the park guards, while the first start waits, on macOS, for every spawn in
    /// flight (it takes the spawn lock, as the `accept` that made this connection did). A lane
    /// enters its connection before it takes any such lock; after the first start this is one
    /// read. An arm made where no connection is served (verify's remote mutation cancellation)
    /// starts the bell itself, holding no lock, since an arm never starts one.
    #[cfg(unix)]
    #[must_use]
    pub fn enter(fd: std::os::fd::RawFd) -> Self {
        let _ = aterm_uds::hangup::Bell::process().start();
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
