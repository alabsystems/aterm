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
mod tests {
    use super::*;

    #[test]
    fn a_canceled_or_expired_request_never_enters_the_engine() {
        let live = Arc::new(Mutex::new(true));
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut request = Request::new(
            Operation::Capture {
                session: 7,
                target: NameTarget::Job("b1".into()),
            },
            Arc::clone(&live),
            tx,
        );
        *live.lock().unwrap() = false;
        assert_eq!(
            request.run_active(|| -> Result<(), NamingError> { panic!("canceled operation ran") }),
            Err(Error::Canceled)
        );
        *live.lock().unwrap() = true;
        request.deadline = Instant::now();
        assert_eq!(
            request.run_active(|| -> Result<(), NamingError> { panic!("expired operation ran") }),
            Err(Error::Canceled)
        );
        request.respond(Err(Error::Unavailable));
        assert!(matches!(rx.recv().unwrap(), Err(Error::Canceled)));
    }

    #[test]
    fn identity_requires_exact_waiter_birth_and_an_exec_version() {
        let mut waiter = NamingWaiter {
            pid: 71,
            start: 123_456,
            exec_version: Some(8),
            exe: "worker".into(),
        };
        let identity = expected(&waiter).unwrap();
        assert_eq!(
            (identity.pid, identity.birth.0, identity.idversion),
            (71, 123_456, Some(8))
        );
        waiter.exec_version = None;
        assert_eq!(expected(&waiter), Err(Error::Engine(NamingError::Unread)));
        waiter.exec_version = Some(u64::MAX);
        assert_eq!(expected(&waiter), Err(Error::Engine(NamingError::Unread)));
    }

    #[test]
    fn scope_refusal_precedes_any_probe_or_sampler_request() {
        let shared = Shared::new();
        assert_eq!(
            command(&shared, 1, NameTarget::Job("b1".into()), false, false),
            "ERR scope\n"
        );
    }

    #[test]
    fn shutdown_cancels_a_lease_without_releasing_a_newer_operation() {
        let shared = Shared::new();
        assert!(matches!(shared.enter_naming(), Err(Error::Unavailable)));
        let (tx, _rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx.clone());
        let old = shared.enter_naming().unwrap();
        assert!(matches!(shared.enter_naming(), Err(Error::Busy)));
        shared.uninstall();
        assert!(old.canceled());
        assert!(matches!(shared.enter_naming(), Err(Error::Unavailable)));
        shared.install(tx);
        let current = shared.enter_naming().unwrap();
        drop(old);
        assert!(!current.canceled());
        assert!(matches!(shared.enter_naming(), Err(Error::Busy)));
        drop(current);
        assert!(shared.enter_naming().is_ok());
    }

    #[test]
    fn a_full_probe_queue_never_coalesces_a_naming_request() {
        let shared = Shared::new();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        shared.install(tx.clone());
        tx.try_send(crate::vitals_host::Msg::Pull).unwrap();
        let lease = shared.enter_naming().unwrap();
        let result = shared.request_name(
            &lease.live,
            Operation::Capture {
                session: 7,
                target: NameTarget::Job("b1".into()),
            },
        );
        assert!(matches!(result, Err(Error::Busy)));
        assert!(matches!(
            rx.try_recv().unwrap(),
            crate::vitals_host::Msg::Pull
        ));
        assert!(matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_closed_caller_cannot_apply_after_its_worker_and_descriptor_are_gone() {
        use std::os::fd::AsRawFd as _;
        let (client, server) = aterm_uds::CtlStream::pair().unwrap();
        let live = Arc::new(Mutex::new(true));
        let (reply, _answer) = std::sync::mpsc::sync_channel(1);
        let serving = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
        let request = Request::new(
            Operation::Capture {
                session: 7,
                target: NameTarget::Job("b1".into()),
            },
            live,
            reply,
        );
        assert!(request.is_active());
        drop(serving);
        drop(server);
        assert!(
            request.is_active(),
            "the request owns its connection duplicate"
        );
        drop(client);
        // The probe thread has no control-worker TLS and the original descriptor is closed.
        let result = std::thread::spawn(move || {
            request.run_active(|| -> Result<(), NamingError> {
                panic!("closed caller entered engine")
            })
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(Error::Canceled));
    }

    #[test]
    fn naming_json_keeps_typed_unknown_holder_without_raw_evidence() {
        let reply = success("b1", TierB::Lock, Duration::from_millis(1234), true);
        let (_, body) = reply.split_once('\n').unwrap();
        let body: aterm_json::Value = aterm_json::from_str(body.trim()).unwrap();
        assert_eq!(body["kind"].as_str(), Some("sys.name"));
        assert_eq!(body["named"].as_str(), Some("lock"));
        assert_eq!(body["job"].as_str(), Some("b1"));
        assert_eq!(body["took_ms"].as_u64(), Some(1234));
        assert_eq!(body["holder"]["why"].as_str(), Some("not_sampled"));
        assert_eq!(body.as_object().unwrap().len(), 6);
        let timer = success("b1", TierB::Timer, Duration::ZERO, true);
        let body: aterm_json::Value =
            aterm_json::from_str(timer.split_once('\n').unwrap().1.trim()).unwrap();
        assert!(body.get("holder").is_none());
    }
}
