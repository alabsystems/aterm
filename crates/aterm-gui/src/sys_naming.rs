// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit wait naming: a control worker samples between two probe-thread observations.

use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_vitals::jobs::{NamingError, NamingTicket, NamingWaiter, TierB};
use aterm_vitals::sys::query::NameTarget;

use crate::sys_host::Shared;

/// A request's hang detector, rather than a promise about sampling latency.
pub(crate) const BOUND: Duration = Duration::from_secs(60);
/// Check shutdown while a control worker waits for the probe.
pub(crate) const CANCEL_POLL: Duration = Duration::from_millis(100);
pub(crate) type Live = Arc<Mutex<bool>>;

#[derive(Debug)]
pub(crate) enum Operation {
    Capture { session: u64, target: NameTarget },
    Apply { ticket: NamingTicket, word: TierB },
}

#[derive(Debug)]
pub(crate) enum Answer {
    Captured(NamingTicket),
    Applied { job: String, word: TierB },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Engine(NamingError),
    Canceled,
    Unavailable,
    Busy,
    Timeout,
}

impl From<NamingError> for Error {
    fn from(error: NamingError) -> Self {
        Self::Engine(error)
    }
}

impl Error {
    fn word(self) -> &'static str {
        match self {
            Self::Engine(NamingError::Gone) => "gone",
            Self::Engine(NamingError::Ambiguous) => "ambiguous",
            Self::Engine(NamingError::NotWaiting) => "not_waiting",
            Self::Engine(NamingError::Unread) => "not_sampled",
            Self::Engine(NamingError::AlreadyNamed) => "already_named",
            Self::Engine(NamingError::Stale) => "stale",
            Self::Canceled => "canceled",
            Self::Unavailable => "not_sampled",
            Self::Busy => "busy",
            Self::Timeout => "timeout",
        }
    }

    fn reply(self) -> String {
        format!("ERR unavailable name why={}\n", self.word())
    }
}

/// One in flight through capture, sampling and publication. Shutdown invalidates the same flag.
pub(crate) struct Lease<'a> {
    shared: &'a Shared,
    pub(crate) live: Live,
}

impl<'a> Lease<'a> {
    pub(crate) fn new(shared: &'a Shared, live: Live) -> Self {
        Self { shared, live }
    }

    fn canceled(&self) -> bool {
        crate::control::caller_hung_up()
            || !*self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.shared.leave_naming(&self.live);
    }
}

/// A distinct request, never coalesced with a generic pull or another naming request.
#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) operation: Operation,
    live: Live,
    reply: SyncSender<Result<Answer, Error>>,
    deadline: Instant,
    /// An owned duplicate can never turn into a different connection after fd reuse.
    #[cfg(unix)]
    caller: Option<std::os::fd::OwnedFd>,
    caller_unread: bool,
}

impl Request {
    pub(crate) fn new(
        operation: Operation,
        live: Live,
        reply: SyncSender<Result<Answer, Error>>,
    ) -> Self {
        #[cfg(unix)]
        let caller = aterm_control::hangup::serving_fd().map(|fd| {
            use std::os::fd::BorrowedFd;
            // SAFETY: ServingFd names the live connection this worker is serving; the
            // duplicate is made before this same worker can close that borrowed descriptor.
            unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()
        });
        #[cfg(unix)]
        let caller_unread = caller.as_ref().is_some_and(Result::is_err);
        #[cfg(not(unix))]
        let caller_unread = false;
        Self {
            operation,
            live,
            reply,
            deadline: Instant::now() + BOUND,
            #[cfg(unix)]
            caller: caller.and_then(Result::ok),
            caller_unread,
        }
    }

    fn caller_gone(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            self.caller_unread
                || self
                    .caller
                    .as_ref()
                    .is_some_and(|caller| aterm_uds::hangup::fd_peer_closed(caller.as_raw_fd()))
        }
        #[cfg(not(unix))]
        {
            self.caller_unread
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.deadline > Instant::now()
            && !self.caller_gone()
            && *self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Linearize cancellation against a pure engine operation. The peer check is nonblocking;
    /// process probes and sample collection never run under this lock.
    pub(crate) fn run_active<T>(
        &self,
        run: impl FnOnce() -> Result<T, NamingError>,
    ) -> Result<T, Error> {
        let active = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        if !*active || self.deadline <= Instant::now() || self.caller_gone() {
            return Err(Error::Canceled);
        }
        run().map_err(Error::from)
    }

    pub(crate) fn respond(self, answer: Result<Answer, Error>) {
        let answer = if self.is_active() {
            answer
        } else {
            Err(Error::Canceled)
        };
        let _ = self.reply.try_send(answer);
    }
}

/// Only macOS supplies an exec-versioned identity for the explicit sampler in this implementation.
fn expected(waiter: &NamingWaiter) -> Result<aterm_uds::process::Identity, Error> {
    let pid = i32::try_from(waiter.pid).ok().filter(|pid| *pid > 1);
    let version = waiter
        .exec_version
        .and_then(|version| i32::try_from(version).ok());
    match (pid, version) {
        (Some(pid), Some(version)) => Ok(aterm_uds::process::Identity {
            pid,
            birth: aterm_uds::process::Birth(waiter.start),
            idversion: Some(version),
        }),
        _ => Err(Error::Engine(NamingError::Unread)),
    }
}

fn mapped(kind: aterm_uds::wait_sample::Kind) -> TierB {
    use aterm_uds::wait_sample::Kind;
    match kind {
        Kind::Timer => TierB::Timer,
        Kind::Lock => TierB::Lock,
        Kind::Read => TierB::Read,
        Kind::Event => TierB::Event,
        Kind::Ipc => TierB::Ipc,
        Kind::Mutex => TierB::Mutex,
        Kind::Children => TierB::Children,
        Kind::Unparsed => TierB::Unparsed,
    }
}

/// Runs only on an explicitly requesting control worker, never the UI or probe thread.
pub(crate) fn command(
    shared: &Shared,
    session: u64,
    target: NameTarget,
    owner: bool,
    json: bool,
) -> String {
    if !owner {
        return "ERR scope\n".to_owned();
    }
    if !cfg!(target_os = "macos") {
        return "ERR unavailable name why=unsupported\n".to_owned();
    }
    let result = (|| {
        let lease = shared.enter_naming()?;
        let Answer::Captured(ticket) =
            shared.request_name(&lease.live, Operation::Capture { session, target })?
        else {
            return Err(Error::Unavailable);
        };
        let identity = expected(ticket.waiter())?;
        let directory = aterm_types::dirs::state_dir()
            .ok_or(Error::Unavailable)?
            .join("wait-samples.noindex");
        #[cfg(target_os = "macos")]
        aterm_uds::private_file::create_private_dir(&directory).map_err(|_| Error::Unavailable)?;
        let sample = aterm_uds::wait_sample::sample(identity, &directory, || lease.canceled())
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::TimedOut => Error::Timeout,
                std::io::ErrorKind::Interrupted => Error::Canceled,
                std::io::ErrorKind::NotFound => Error::Engine(NamingError::Gone),
                _ => Error::Unavailable,
            })?;
        let word = mapped(sample.kind);
        let answer = shared.request_name(&lease.live, Operation::Apply { ticket, word })?;
        let Answer::Applied { job, word } = answer else {
            return Err(Error::Unavailable);
        };
        Ok(success(&job, word, sample.elapsed, json))
    })();
    result.unwrap_or_else(Error::reply)
}

fn success(job: &str, word: TierB, elapsed: Duration, json: bool) -> String {
    if json {
        use aterm_vitals::sys::json::Jv;
        let mut body = Jv::obj([
            ("schema", Jv::u(1)),
            ("kind", Jv::s("sys.name")),
            ("named", Jv::s(word.word())),
            ("job", Jv::s(job)),
            (
                "took_ms",
                Jv::u(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)),
            ),
        ]);
        if word == TierB::Lock {
            body.set("holder", Jv::why("not_sampled"));
        }
        return format!("OK 1 sys v=1 view=name\n{}\n", body.to_text());
    }
    let holder = if word == TierB::Lock {
        " holder=unknown"
    } else {
        ""
    };
    format!(
        "OK named={} job={job}{holder} took_ms={}\n",
        word.word(),
        elapsed.as_millis()
    )
}

#[cfg(test)]
#[path = "sys_naming_tests.rs"]
mod tests;
