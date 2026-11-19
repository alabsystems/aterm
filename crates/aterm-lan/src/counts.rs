// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the broker counts of its own work, and the file it writes them to.
//!
//! Read them on a running Mac with
//! `cat /var/run/systems.alab.aterm-lan/broker.counts`
//! ([`aterm_uds::lan::SOCKET_DIR`]/[`aterm_uds::lan::COUNTS_NAME`]): the previous snapshot and
//! the current one, each with its Unix milliseconds, so one read gives a rate. The file holds
//! counts, CPU nanoseconds and the pids of at most [`REQUESTERS`] requesting processes: no name,
//! no path, no token and nothing a request carried.
//!
//! Every count only rises and is one relaxed add where it is counted. The requesters' table is
//! the one exception: a count per process cannot be one atomic, so it takes a lock of its own for
//! a scan of [`REQUESTERS`] entries, once per connection the broker checks, as that check
//! returns. Two paths take that lock, each for the scan or a copy of the table and nothing else:
//! the connection's thread ([`Counts::requester`]) and the accept loop's, where it writes a
//! snapshot ([`Counts::requesters`]).
//!
//! What the CPU figures cover: `conn_cpu_ns` is the `lan-conn` threads', `check_cpu_ns` the
//! full checks' helper threads' (`aterm-trust-check`) and `resolve_cpu_ns` the name
//! resolutions' (`lan-resolve`, one for each `resolve` request and each `connect` to a host
//! name). `process_cpu_ns` less those three is every other thread's: the accept loop's (the
//! snapshots it writes included), the update watch's, and whatever threads Security.framework
//! and the system resolver run of their own. It is not the full checks' cost alone.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use aterm_keeper::identity::CheckCost;
use aterm_uds::cpu::ThreadsCpu;
use aterm_uds::peer::AuditToken;

/// One thing the broker counts. A connection the accept loop took is counted in
/// [`Count::Accepted`] and ends in exactly one of [`Count::Busy`], [`Count::NoThread`] or the
/// endings below; an admitted one ([`Count::Admitted`]) in exactly one ending from
/// [`Count::HelloUnsent`] on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Count {
    /// Connections the accept loop took.
    Accepted,
    /// Told the broker was serving its most connections, unchecked.
    Busy,
    /// No thread could be started for it.
    NoThread,
    /// Calls of the full check of a requester's code: one for each connection admitted by
    /// code, counted where the check is called. A call is not a check that started: one made
    /// with no audit token, or while an earlier check's helper is overdue (`trustd` stalled),
    /// settles without a helper and ends as [`Count::Unchecked`] or [`Count::Unanswered`]. The
    /// checks that ran are `checks_returned` and those still running, so `checks_run` less
    /// `checks_returned` is not the checks in flight.
    ChecksRun,
    /// Connections whose requesting process was not in the requesters' table as its check
    /// returned ([`Counts::requester`]).
    NewRequesters,
    /// Ended: the peer's uid or code was judged and refused.
    Refused,
    /// Ended: the check could not run.
    Unchecked,
    /// Ended: the check did not finish (`trustd` did not answer).
    Unanswered,
    /// Admitted: the check passed.
    Admitted,
    /// Ended: the broker's hello could not be sent.
    HelloUnsent,
    /// Ended: the peer closed before its hello.
    ClosedUnsaid,
    /// Ended: the peer's hello named another protocol.
    WrongProtocol,
    /// Ended: the peer's first frame was not a hello.
    WrongFirstFrame,
    /// Ended: a read of the hello or the request failed (its deadline included).
    ReadError,
    /// Ended: a hello and nothing more (`aterm lan status`).
    HelloOnly,
    /// Ended: the frame after the hello was not a request.
    WrongRequestFrame,
    /// Ended: bytes followed the one request.
    TrailingBytes,
    /// A connect request.
    Connect,
    /// A UDP socket request.
    Udp,
    /// A name resolution request.
    Resolve,
    /// A Bonjour browse.
    Browse,
    /// A Bonjour service resolve.
    ResolveService,
    /// A DNS-SD record query.
    Query,
    /// A Bonjour advert.
    Register,
    /// A request for Spotlight's stores ([`crate::store`]).
    SpotlightStore,
    /// Walks of Spotlight's stores: at most one every [`crate::store::WALK_EVERY`], however many
    /// requests ask.
    StoreWalks,
    /// An ask to consider the asking process's bundle for adoption, answered by nothing
    /// ([`aterm_uds::lan::wire::Request::Consider`]).
    Considered,
    /// Ended: the request was answered (a standing operation: its client went).
    Answered,
    /// Ended: the request failed and its error was sent.
    Failed,
    /// A Spotlight exclusion request (`Request::ScopeHome`), answered whichever way with its own
    /// frame ([`crate::scope`]).
    ScopeHome,
}

impl Count {
    /// Every count, in the order a snapshot prints them.
    pub(crate) const ALL: [Self; 30] = [
        Self::Accepted,
        Self::Busy,
        Self::NoThread,
        Self::ChecksRun,
        Self::NewRequesters,
        Self::Refused,
        Self::Unchecked,
        Self::Unanswered,
        Self::Admitted,
        Self::HelloUnsent,
        Self::ClosedUnsaid,
        Self::WrongProtocol,
        Self::WrongFirstFrame,
        Self::ReadError,
        Self::HelloOnly,
        Self::WrongRequestFrame,
        Self::TrailingBytes,
        Self::Connect,
        Self::Udp,
        Self::Resolve,
        Self::Browse,
        Self::ResolveService,
        Self::Query,
        Self::Register,
        Self::SpotlightStore,
        Self::StoreWalks,
        Self::Considered,
        Self::Answered,
        Self::Failed,
        Self::ScopeHome,
    ];

    /// The count's name in a snapshot.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Busy => "busy",
            Self::NoThread => "no_thread",
            Self::ChecksRun => "checks_run",
            Self::NewRequesters => "new_requesters",
            Self::Refused => "refused",
            Self::Unchecked => "unchecked",
            Self::Unanswered => "unanswered",
            Self::Admitted => "admitted",
            Self::HelloUnsent => "hello_unsent",
            Self::ClosedUnsaid => "closed_unsaid",
            Self::WrongProtocol => "wrong_protocol",
            Self::WrongFirstFrame => "wrong_first_frame",
            Self::ReadError => "read_error",
            Self::HelloOnly => "hello_only",
            Self::WrongRequestFrame => "wrong_request_frame",
            Self::TrailingBytes => "trailing_bytes",
            Self::Connect => "connect",
            Self::Udp => "udp",
            Self::Resolve => "resolve",
            Self::Browse => "browse",
            Self::ResolveService => "resolve_service",
            Self::Query => "query",
            Self::Register => "register",
            Self::SpotlightStore => "spotlight_store",
            Self::StoreWalks => "store_walks",
            Self::Considered => "considered",
            Self::Answered => "answered",
            Self::Failed => "failed",
            Self::ScopeHome => "scope_home",
        }
    }
}

/// How many requesting processes the table remembers.
pub(crate) const REQUESTERS: usize = 8;

/// One requesting process: its whole audit token (compared whole, so a pid reused or an image
/// exec'd is another requester) and what it did while it was in the table.
#[derive(Clone, Copy)]
struct Requester {
    token: AuditToken,
    /// Its connections whose check returned, and those of them the check admitted.
    connections: u64,
    admitted: u64,
    /// [`Count::Accepted`] as it entered the table: its counts are of the connections accepted
    /// after that many, so a requester that left the table and came back shows a later start,
    /// not a count that fell.
    since_accepted: u64,
    /// [`Requesters::seen`] at its last connection.
    seen: u64,
}

/// At most [`REQUESTERS`] requesting processes; [`Counts::requester`] says which one a new
/// requester replaces.
#[derive(Default)]
struct Requesters {
    slots: [Option<Requester>; REQUESTERS],
    /// Connections counted in the table so far: each requester's last is its `seen`.
    seen: u64,
}

/// One requester as a snapshot says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) pid: u32,
    pub(crate) connections: u64,
    pub(crate) admitted: u64,
    pub(crate) since_accepted: u64,
}

/// The broker's counts.
pub(crate) struct Counts {
    counts: [AtomicU64; Count::ALL.len()],
    /// The CPU time of the threads started for connections (`lan-conn`), added as each ends and,
    /// for a standing operation, each time it hears something.
    pub(crate) conn_cpu_ns: AtomicU64,
    /// The full checks' helper threads (`aterm-trust-check`): how many returned, and their CPU
    /// time. The keeper's live macOS law holds that a costed check adds to it (see
    /// [`CheckCost`]), and this crate's live macOS law that the broker's admission by code makes
    /// its checks costed (`a_connection_admitted_by_code_is_counted_with_its_check`). Both count
    /// checks; the CPU figure is a clock's reading and no law asserts a value of it.
    pub(crate) check_cost: Arc<CheckCost>,
    /// The name resolutions' threads (`lan-resolve`) whose lookup returned, and their CPU time
    /// ([`aterm_uds::lan::here::resolve_counted`]).
    pub(crate) resolve_threads: Arc<ThreadsCpu>,
    requesters: Mutex<Requesters>,
}

impl Counts {
    pub(crate) fn new() -> Self {
        Self {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
            conn_cpu_ns: AtomicU64::new(0),
            check_cost: Arc::new(CheckCost::default()),
            resolve_threads: Arc::new(ThreadsCpu::new()),
            requesters: Mutex::new(Requesters::default()),
        }
    }

    /// Count one `what`: its count after this one.
    pub(crate) fn add(&self, what: Count) -> u64 {
        self.counts[what as usize].fetch_add(1, Ordering::Relaxed) + 1
    }

    /// `what` so far.
    pub(crate) fn get(&self, what: Count) -> u64 {
        self.counts[what as usize].load(Ordering::Relaxed)
    }

    /// Count a connection of the process `token` names, whose check returned and admitted it or
    /// not (none where the kernel gave no token: it is in no requester's count).
    ///
    /// A requester in the table is counted there. One that is not is counted in
    /// [`Count::NewRequesters`] and takes a slot: an empty one, else that of the requester seen
    /// longest ago among those the check never admitted, else (every requester in the table was
    /// admitted at least once) that of the one seen longest ago, and only where this connection
    /// was admitted. So a requester that keeps connecting is not replaced by ones that came and
    /// went before its last connection, and a process the check does not admit never replaces
    /// one it admitted.
    pub(crate) fn requester(&self, token: Option<AuditToken>, admitted: bool) {
        let Some(token) = token else { return };
        let accepted = self.get(Count::Accepted);
        let mut table = self
            .requesters
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        table.seen += 1;
        let seen = table.seen;
        if let Some(known) = table
            .slots
            .iter_mut()
            .flatten()
            .find(|known| known.token == token)
        {
            known.connections += 1;
            known.admitted += u64::from(admitted);
            known.seen = seen;
            return;
        }
        let longest_ago = |slots: &[Option<Requester>; REQUESTERS], of_admitted: bool| {
            slots
                .iter()
                .enumerate()
                .filter_map(|(at, slot)| Some((at, (*slot)?)))
                .filter(|(_, known)| (known.admitted != 0) == of_admitted)
                .min_by_key(|(_, known)| known.seen)
                .map(|(at, _)| at)
        };
        let slot = table
            .slots
            .iter()
            .position(Option::is_none)
            .or_else(|| longest_ago(&table.slots, false))
            .or_else(|| admitted.then(|| longest_ago(&table.slots, true)).flatten());
        if let Some(slot) = slot {
            table.slots[slot] = Some(Requester {
                token,
                connections: 1,
                admitted: u64::from(admitted),
                since_accepted: accepted,
                seen,
            });
        }
        drop(table);
        self.add(Count::NewRequesters);
    }

    /// The table's requesters, in the slot order: the table is copied out under its lock and
    /// read after it.
    pub(crate) fn requesters(&self) -> Vec<Listed> {
        let slots = self
            .requesters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slots;
        slots
            .iter()
            .flatten()
            .map(|known| Listed {
                pid: known.token.pid(),
                connections: known.connections,
                admitted: known.admitted,
                since_accepted: known.since_accepted,
            })
            .collect()
    }

    /// One snapshot: a line of every count at `at_ms` (Unix milliseconds) with the process's own
    /// CPU time `process_cpu_ns`, then a line per requester: its pid, its connections whose check
    /// returned, those the check admitted, and `accepted` as it entered the table.
    pub(crate) fn snapshot(&self, at_ms: u64, process_cpu_ns: u64) -> String {
        let mut out = format!("at_ms={at_ms}");
        for what in Count::ALL {
            let _ = write!(out, " {}={}", what.name(), self.get(what));
        }
        let _ = writeln!(
            out,
            " checks_returned={} check_cpu_ns={} conn_cpu_ns={} resolve_threads={} \
             resolve_cpu_ns={} process_cpu_ns={process_cpu_ns}",
            self.check_cost.checks.load(Ordering::Relaxed),
            self.check_cost.cpu_ns.load(Ordering::Relaxed),
            self.conn_cpu_ns.load(Ordering::Relaxed),
            self.resolve_threads.ended.load(Ordering::Relaxed),
            self.resolve_threads.cpu_ns.load(Ordering::Relaxed),
        );
        for listed in self.requesters() {
            let Listed {
                pid,
                connections,
                admitted,
                since_accepted,
            } = listed;
            let _ = writeln!(
                out,
                "requester pid={pid} connections={connections} admitted={admitted} \
                 since_accepted={since_accepted}"
            );
        }
        out
    }
}

/// The counts file's first line.
pub(crate) const HEAD: &str = "aterm-lan counts 1";

/// Where the counts are written, and the snapshot written last.
pub(crate) struct Sink {
    path: PathBuf,
    previous: String,
    /// Whether a failed write was logged: the first is, once.
    failed: bool,
    /// The wall clock, in Unix milliseconds, and the process's CPU time, in nanoseconds: the
    /// system's, or a test's.
    clock: fn() -> u64,
    process_cpu: fn() -> u64,
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

impl Sink {
    /// The counts file beside `socket`, in the folder the broker serves from: the one
    /// [`crate::server::prepare`] made root's alone, so no other account can put a link where
    /// this writes. This makes no folder, and [`Self::write`] writes only into one that is
    /// there.
    pub(crate) fn beside(socket: &Path) -> Self {
        Self::beside_with(socket, unix_ms, aterm_uds::cpu::process_ns)
    }

    pub(crate) fn beside_with(socket: &Path, clock: fn() -> u64, process_cpu: fn() -> u64) -> Self {
        Self {
            path: socket.with_file_name(aterm_uds::lan::COUNTS_NAME),
            previous: String::new(),
            failed: false,
            clock,
            process_cpu,
        }
    }

    /// Replace the file with the snapshot written last and one of `counts` now, by rename, so a
    /// reader finds one whole file. Nothing is appended and nothing is flushed: the counts are of
    /// this process, which a power loss ends. Where this finds no folder for the file, it writes
    /// nothing and makes nothing: the writer below would make a missing folder, which would then
    /// be one [`crate::server::prepare`] never checked.
    pub(crate) fn write(&mut self, counts: &Counts, log: fn(std::fmt::Arguments<'_>)) {
        let current = counts.snapshot((self.clock)(), (self.process_cpu)());
        let text = format!("{HEAD}\nprevious\n{}current\n{current}", self.previous);
        self.previous = current;
        let folder = self.path.parent().unwrap_or(Path::new(""));
        let written = match std::fs::symlink_metadata(folder) {
            Ok(meta) if meta.is_dir() => {
                aterm_uds::atomic_file::write_flushed(&self.path, text.as_bytes(), 0o644, None)
            }
            Ok(_) => Err(std::io::Error::other(format!(
                "{} is not a folder",
                folder.display()
            ))),
            Err(error) => Err(error),
        };
        match written {
            Ok(()) => {}
            Err(error) if !self.failed => {
                self.failed = true;
                log(format_args!("the counts were not written: {error}"));
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
#[path = "counts_tests.rs"]
mod tests;
