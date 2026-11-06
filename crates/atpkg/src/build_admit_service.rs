// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit native host session. Observer delivery never supplies execution policy.
//! This local capability boundary is not a public socket or machine-wide singleton.

use super::{
    CancelRequest, CohortSettlement, Held, JobControl, SupervisedEvent,
    host::{Completion, Jobs, Ticket},
};
use aterm_capacity::host_protocol::{Attempt, Failure, Generation, Snapshot};
use aterm_uds::spawnfd;
use ring::rand::SecureRandom as _;
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    os::{fd::AsFd as _, unix::net::UnixDatagram},
    sync::{Arc, Condvar, Mutex, PoisonError, atomic::AtomicUsize},
    time::Instant,
};

const CONTROL_BYTES: usize = 26;
const CONTROL_TURN: usize = 16;
const READ_PAGE: usize = 64;

#[path = "build_admit_monitor.rs"]
pub mod monitor;

#[path = "build_admit_epochs.rs"]
mod epochs;
pub use super::host::RetainedRefused;
pub use epochs::{EpochAccepted, EpochId, EpochRefused};

/// Resource custody is installed, including if dispatch or publication later failed.
/// Keep the attempt for recovery; publication success is not a start or drain receipt.
#[derive(Debug)]
#[must_use = "an accepted retained attempt remains owned through later failure"]
pub struct RetainedAccepted {
    pub attempt: Attempt,
    /// Observer publication outcome. Dispatch failures stay with the owned attempt
    /// and are reported in its job snapshot and eventual completion.
    pub publication: io::Result<()>,
}

/// Incarnation-bound event cursor, not a control credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub generation: Generation,
    pub revision: u64,
}

/// An explicitly rejected control packet; no native action was inferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Malformed,
    StaleGeneration,
    StaleAttempt,
    InboxUnavailable,
}

/// Closed publication events. Original arbitrary error prose stays with the owner.
#[derive(Clone, Debug)]
pub enum Event {
    Submitted,
    /// Intent reached the job inbox; this is not kernel acceptance.
    IntentDelivered(CancelRequest),
    Rejected(Rejection),
    /// New admission was refused; existing native jobs remain owned.
    AdmissionRefused(Failure),
    Native(SupervisedEvent),
    Finished {
        succeeded: bool,
        cancelled: bool,
        settlement: Option<CohortSettlement>,
    },
    Failed(Failure),
    StateChanged,
    OwnerClosed,
}

/// One retained event; absent attempt denotes a service-level observation.
#[derive(Clone, Debug)]
pub struct Record {
    pub revision: u64,
    pub attempt: Option<u64>,
    pub event: Event,
}

/// An explicit lost range when a reader falls behind bounded retention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    pub first: u64,
    pub last: u64,
}

/// A coherent current snapshot and one bounded page of retained event history.
/// The snapshot may be newer than the page; continue from next, not its revision.
#[derive(Clone, Debug)]
pub struct Read {
    pub snapshot: Arc<Snapshot>,
    /// Original owner observation time, never restamped by a reader.
    pub observed_at: Instant,
    pub events: Vec<Record>,
    pub next: Cursor,
    pub gap: Option<Gap>,
}

/// No stale/future cursor is silently treated as a valid continuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    StaleGeneration,
    FutureCursor,
    Exhausted,
}

struct Publication {
    snapshot: Arc<Snapshot>,
    at: Instant,
    events: VecDeque<Record>,
    limit: usize,
    exhausted: bool,
}

/// A read-only local subscription. Cloning/dropping it owns no native job lifetime.
/// No subscriber callback or I/O runs inside the execution owner's tick.
#[derive(Clone)]
pub struct Observer(Arc<Mutex<Publication>>, Arc<Condvar>);

impl Observer {
    // The predicate and wait share the publication mutex, so a revision between
    // capture and park cannot be lost. This runs only on an independent reader.
    fn wait_revision(&self, revision: u64, until: Instant) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        while !state.exhausted && !state.snapshot.closed && state.snapshot.revision == revision {
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            state = self
                .1
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    #[cfg(test)]
    fn latest(&self) -> io::Result<(Arc<Snapshot>, Instant)> {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.exhausted {
            return Err(io::Error::other("host publication exhausted"));
        }
        Ok((Arc::clone(&state.snapshot), state.at))
    }
    /// Read latest data and up to 64 retained events. A fresh reader starts at
    /// the oldest retained event; an explicit old cursor reports its lost range.
    pub fn read(&self, cursor: Option<Cursor>) -> Result<Read, ReadError> {
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.exhausted {
            return Err(ReadError::Exhausted);
        }
        let generation = state.snapshot.generation;
        let oldest = state
            .events
            .front()
            .map_or(state.snapshot.revision.saturating_add(1), |row| {
                row.revision
            });
        let cursor = cursor.unwrap_or(Cursor {
            generation,
            revision: oldest - 1,
        });
        if cursor.generation != generation {
            return Err(ReadError::StaleGeneration);
        }
        if cursor.revision > state.snapshot.revision {
            return Err(ReadError::FutureCursor);
        }
        let first = cursor.revision.saturating_add(1);
        let gap = (first < oldest).then_some(Gap {
            first,
            last: oldest - 1,
        });
        let after = cursor.revision.max(oldest - 1);
        let events: Vec<_> = state
            .events
            .iter()
            .filter(|row| row.revision > after)
            .take(READ_PAGE)
            .cloned()
            .collect();
        let revision = events.last().map_or(after, |row| row.revision);
        Ok(Read {
            snapshot: Arc::clone(&state.snapshot),
            observed_at: state.at,
            events,
            next: Cursor {
                generation,
                revision,
            },
            gap,
        })
    }
}

/// Explicitly delegated local control endpoint. Generation is fencing data;
/// possession of this private sender, not decoded snapshot data, authorizes queueing.
/// A future socket server must delegate it only through its qualified authorization.
#[derive(Clone)]
pub struct Controller {
    generation: Generation,
    process: u32,
    sender: Arc<UnixDatagram>,
}

impl Controller {
    /// Queue typed intent without blocking. Success is only kernel mailbox
    /// queueing, not job recognition, signal acceptance or completion.
    /// # Errors
    /// Foreign/stale incarnation, zero attempt, full/disconnected native inbox.
    pub fn cancel(&self, attempt: Attempt, request: CancelRequest) -> io::Result<()> {
        if self.process != std::process::id()
            || attempt.generation != self.generation
            || attempt.sequence == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "control endpoint incarnation mismatch",
            ));
        }
        let mut packet = [0; CONTROL_BYTES];
        packet[0] = 1;
        packet[1..17].copy_from_slice(&self.generation.0);
        packet[17..25].copy_from_slice(&attempt.sequence.to_le_bytes());
        packet[25] = match request {
            CancelRequest::Terminate => 1,
            CancelRequest::Kill => 2,
        };
        if self.sender.send(&packet)? != packet.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "host control was not queued",
            ));
        }
        Ok(())
    }
}

/// One actual native execution owner plus independent bounded publication.
/// Keep it on the existing host's background execution context. Creating this
/// session does not install a new daemon or claim computer-wide leadership.
pub struct Owner {
    jobs: Jobs,
    epochs: epochs::Table,
    tickets: BTreeMap<u64, Ticket>,
    process: u32,
    generation: Generation,
    receiver: UnixDatagram,
    keep_control: Controller,
    observer: Observer,
    monitor_connections: Arc<AtomicUsize>,
    // Declared last: actual job/epoch custody drops before leadership is released.
    fence: Option<aterm_capacity::leadership::Fence>,
}

impl Owner {
    /// Establish an explicit native session with bounded root/history counts.
    /// # Errors
    /// Invalid limits, unavailable retained-child watch, random generation or native mailbox.
    pub fn new(max_jobs: usize, history: usize) -> io::Result<Self> {
        let mut generation = [0; 16];
        ring::rand::SystemRandom::new()
            .fill(&mut generation)
            .map_err(|_| io::Error::other("host generation unavailable"))?;
        Self::with_generation(max_jobs, history, Generation(generation))
    }

    /// Own execution under the already committed fence for one explicit private
    /// capacity namespace. Submitted claims must carry that namespace's actual
    /// custody. This does not install a computer-wide or cross-account service.
    /// # Errors
    /// Stale leadership or the native session initialization errors.
    pub fn fenced(
        fence: aterm_capacity::leadership::Fence,
        max_jobs: usize,
        history: usize,
    ) -> io::Result<Self> {
        fence.check_current()?;
        let mut owner = Self::with_generation(max_jobs, history, fence.generation())?;
        owner.fence = Some(fence);
        Ok(owner)
    }

    fn with_generation(
        max_jobs: usize,
        history: usize,
        generation: Generation,
    ) -> io::Result<Self> {
        if !(1..=1024).contains(&history) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "history limit must be 1..=1024",
            ));
        }
        let jobs = Jobs::new(max_jobs)?;
        let (sender, receiver) = aterm_uds::kept_out(UnixDatagram::pair)?;
        sender.set_nonblocking(true)?;
        receiver.set_nonblocking(true)?;
        let process = std::process::id();
        let keep_control = Controller {
            generation,
            process,
            sender: Arc::new(sender),
        };
        let observer = Observer(
            Arc::new(Mutex::new(Publication {
                snapshot: Arc::new(Snapshot {
                    generation,
                    revision: 0,
                    closed: false,
                    jobs: Vec::new(),
                    epochs: Vec::new(),
                }),
                at: Instant::now(),
                events: VecDeque::with_capacity(history),
                limit: history,
                exhausted: false,
            })),
            Arc::new(Condvar::new()),
        );
        Ok(Self {
            jobs,
            epochs: epochs::Table::new(max_jobs, history),
            tickets: BTreeMap::new(),
            process,
            generation,
            receiver,
            keep_control,
            observer,
            monitor_connections: Arc::new(AtomicUsize::new(0)),
            fence: None,
        })
    }
    fn check_owner(&self) -> io::Result<()> {
        if self.process == std::process::id() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "host session belongs to another process",
            ))
        }
    }
    /// Delegate read-only observation without job-lifetime ownership.
    #[must_use]
    pub fn observer(&self) -> Observer {
        self.observer.clone()
    }

    /// Delegate read-only monitoring for this currently fenced incarnation.
    /// Its connections retain publication data, never jobs or cancellation.
    /// # Errors
    /// Foreign owner, missing or stale namespace leadership.
    pub fn monitor_source(&self) -> io::Result<monitor::Source> {
        self.check_owner()?;
        self.fence
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "monitor delegation requires fenced owner",
                )
            })?
            .check_current()?;
        Ok(monitor::Source::new(
            self.observer(),
            self.generation,
            Arc::clone(&self.monitor_connections),
        ))
    }
    /// Explicitly delegate this session's local control capability.
    #[must_use]
    pub fn controller(&self) -> Controller {
        self.keep_control.clone()
    }

    /// Submit a fully admitted native command. The owner keeps its cancellation
    /// sender even if all display/control clients disconnect.
    /// # Errors
    /// Native job submission errors or exhausted publication before a new launch.
    pub fn submit(
        &mut self,
        held: Held,
        command: &mut spawnfd::Command,
        grace: std::time::Duration,
    ) -> io::Result<Attempt> {
        self.check_owner()?;
        if self.jobs.len() + self.epochs.active_len() >= aterm_capacity::host_protocol::MAX_JOBS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "host observation capacity is full",
            ));
        }
        if let Some(fence) = &self.fence
            && let Err(error) = fence.check_claim(held.claim())
        {
            // Admission refusal is observation, never an execution-policy error
            // handed to Jobs::tick. Already running children keep their custody.
            self.publish(vec![(None, Event::AdmissionRefused(Failure::of(&error)))])?;
            return Err(error);
        }
        let unavailable = {
            let state = self
                .observer
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.exhausted || state.snapshot.revision == u64::MAX
        };
        if unavailable {
            return Err(io::Error::other(
                "host publication exhausted; new submission refused",
            ));
        }
        let ticket = self.jobs.submit(held, command, grace)?;
        let sequence = ticket.id.sequence();
        self.tickets.insert(sequence, ticket);
        self.publish(vec![(Some(sequence), Event::Submitted)])?;
        Ok(Attempt {
            generation: self.generation,
            sequence,
        })
    }

    /// Retain actual resource guards before target birth and until native cohort settlement.
    /// The accepted attempt stays owned through dispatch, observer and wait failures;
    /// dropping clients never releases its resources. A reaped root with a retained or
    /// unreadable cohort remains in the table and keeps `is_empty()` false.
    ///
    /// This is process-local ownership, not a Python submission endpoint or crash-safe
    /// transfer to another process. Dropping the owner leaves pending retained jobs to
    /// its process's reaper, which supplies no receipt of completed cleanup.
    ///
    /// # Errors
    /// A pre-dispatch refusal returns the original admission and resource bundle.
    pub fn submit_retained<T: Send + 'static>(
        &mut self,
        held: Held,
        command: &mut spawnfd::Command,
        grace: std::time::Duration,
        retained: T,
    ) -> Result<RetainedAccepted, RetainedRefused<T>> {
        let ready = (|| {
            self.check_owner()?;
            if self.jobs.len() + self.epochs.active_len() >= aterm_capacity::host_protocol::MAX_JOBS
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "host observation capacity is full",
                ));
            }
            if let Some(fence) = &self.fence {
                fence.check_claim(held.claim())?;
            }
            let state = self
                .observer
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if state.exhausted || state.snapshot.revision == u64::MAX {
                return Err(io::Error::other(
                    "host publication exhausted; new submission refused",
                ));
            }
            Ok(())
        })();
        if let Err(error) = ready {
            return Err(RetainedRefused::new(error, held, retained));
        }
        let ticket = self.jobs.submit_retained(held, command, grace, retained)?;
        let sequence = ticket.id.sequence();
        self.tickets.insert(sequence, ticket);
        Ok(RetainedAccepted {
            attempt: Attempt {
                generation: self.generation,
                sequence,
            },
            publication: self.publish(vec![(Some(sequence), Event::Submitted)]),
        })
    }

    /// Direct owner-side job observations, including retained resources after root reap.
    /// These values supply no child, group-control or guard-release authority.
    /// # Errors
    /// This owner belongs to a different process.
    pub fn job_snapshot(&self) -> io::Result<Vec<super::host::JobInfo>> {
        self.check_owner()?;
        self.jobs.snapshot()
    }

    fn controls(&mut self, events: &mut Vec<(Option<u64>, Event)>) {
        for _ in 0..CONTROL_TURN {
            let mut packet = [0; CONTROL_BYTES + 1];
            match self.receiver.recv(&mut packet) {
                Ok(n) => {
                    if n != CONTROL_BYTES || packet[0] != 1 || !matches!(packet[25], 1 | 2) {
                        events.push((None, Event::Rejected(Rejection::Malformed)));
                        continue;
                    }
                    if packet[1..17] != self.generation.0 {
                        events.push((None, Event::Rejected(Rejection::StaleGeneration)));
                        continue;
                    }
                    let sequence =
                        u64::from_le_bytes(packet[17..25].try_into().expect("fixed control width"));
                    let Some(ticket) = self.tickets.get(&sequence) else {
                        events.push((Some(sequence), Event::Rejected(Rejection::StaleAttempt)));
                        continue;
                    };
                    let request = if packet[25] == 1 {
                        CancelRequest::Terminate
                    } else {
                        CancelRequest::Kill
                    };
                    events.push((
                        Some(sequence),
                        if ticket.cancel.request(request).is_ok() {
                            Event::IntentDelivered(request)
                        } else {
                            Event::Rejected(Rejection::InboxUnavailable)
                        },
                    ));
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    break;
                }
                Err(error) => {
                    events.push((None, Event::Failed(Failure::of(&error))));
                    break;
                }
            }
        }
    }

    /// Drive bounded controls and the actual job table. The trusted owner policy
    /// alone can fail execution; subscriber delivery is never called here.
    /// # Errors
    /// Foreign owner, native table/watch failure or projection read failure.
    pub fn tick(
        &mut self,
        policy: &mut dyn FnMut(Attempt, SupervisedEvent) -> io::Result<JobControl>,
        say: &mut dyn FnMut(&str),
    ) -> io::Result<Vec<Completion>> {
        self.check_owner()?;
        let mut events = Vec::new();
        self.controls(&mut events);
        let generation = self.generation;
        let completed = self.jobs.tick(
            &mut |id, event| {
                events.push((Some(id.sequence()), Event::Native(event)));
                policy(
                    Attempt {
                        generation,
                        sequence: id.sequence(),
                    },
                    event,
                )
            },
            say,
        );
        // One failing subsystem cannot prevent another original owner from
        // advancing its real pipes, child reap or safe settlement.
        if let Some(fence) = &self.fence {
            self.epochs.tick(fence, say);
        }
        let completed = completed?;
        for completion in &completed {
            let event = match &completion.result {
                Ok(outcome) => Event::Finished {
                    succeeded: outcome.succeeded(),
                    cancelled: outcome.cancelled(),
                    settlement: outcome.cohort_settlement(),
                },
                Err(error) => Event::Failed(Failure::of(error)),
            };
            events.push((Some(completion.id.sequence()), event));
            self.tickets.remove(&completion.id.sequence());
        }
        self.publish(events)?;
        Ok(completed)
    }

    fn publish(&self, mut events: Vec<(Option<u64>, Event)>) -> io::Result<()> {
        let jobs = self.jobs.protocol_rows()?;
        let epochs = self
            .epochs
            .rows(aterm_capacity::host_protocol::MAX_JOBS - jobs.len());
        let mut state = self
            .observer
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.exhausted {
            return Ok(());
        }
        if events.is_empty() && jobs == state.snapshot.jobs && epochs == state.snapshot.epochs {
            return Ok(());
        }
        if events.is_empty() {
            events.push((None, Event::StateChanged));
        }
        let Some(revision) = state.snapshot.revision.checked_add(events.len() as u64) else {
            state.exhausted = true;
            drop(state);
            self.observer.1.notify_all();
            return Ok(());
        };
        for (at, (attempt, event)) in (state.snapshot.revision + 1..=revision).zip(events) {
            if state.events.len() == state.limit {
                state.events.pop_front();
            }
            state.events.push_back(Record {
                revision: at,
                attempt,
                event,
            });
        }
        state.snapshot = Arc::new(Snapshot {
            generation: self.generation,
            revision,
            closed: false,
            jobs,
            epochs,
        });
        state.at = Instant::now();
        drop(state);
        self.observer.1.notify_all();
        Ok(())
    }

    /// Wait on the session's control mailbox, native jobs and nearest deadline.
    /// # Errors
    /// Native job table/readiness errors. Run on the execution owner, not a UI thread.
    pub fn wait(&self, until: Instant) -> io::Result<super::host::Wake> {
        self.check_owner()?;
        let (extra, immediate, until) = self.epochs.wait_plan(until);
        self.jobs
            .wait_with(self.receiver.as_fd(), &extra, immediate, until)
    }
    /// No owned root or unresolved epoch remains. Bounded settled history is
    /// observation only; an uncertain or faulted epoch keeps this false.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty() && self.epochs.active_len() == 0
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if self.process != std::process::id() {
            return;
        }
        let mut state = self
            .observer
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut snapshot = (*state.snapshot).clone();
        snapshot.closed = true;
        if let Some(revision) = snapshot.revision.checked_add(1) {
            snapshot.revision = revision;
            if state.events.len() == state.limit {
                state.events.pop_front();
            }
            state.events.push_back(Record {
                revision,
                attempt: None,
                event: Event::OwnerClosed,
            });
        } else {
            state.exhausted = true;
        }
        // Preserve the actual row observation time, not the client's read time.
        state.snapshot = Arc::new(snapshot);
        drop(state);
        self.observer.1.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication(revision: u64, first: u64, count: usize) -> Observer {
        Observer(
            Arc::new(Mutex::new(Publication {
                snapshot: Arc::new(Snapshot {
                    generation: Generation([8; 16]),
                    revision,
                    closed: false,
                    jobs: Vec::new(),
                    epochs: Vec::new(),
                }),
                at: Instant::now(),
                events: (0..count)
                    .map(|index| Record {
                        revision: first + index as u64,
                        attempt: None,
                        event: Event::StateChanged,
                    })
                    .collect(),
                limit: 1024,
                exhausted: false,
            })),
            Arc::new(Condvar::new()),
        )
    }

    #[test]
    fn bounded_pages_report_exact_retention_gap_without_skipping_to_snapshot() {
        let observer = publication(200, 101, 100);
        let first = observer
            .read(Some(Cursor {
                generation: Generation([8; 16]),
                revision: 3,
            }))
            .unwrap();
        assert_eq!(
            first.gap,
            Some(Gap {
                first: 4,
                last: 100
            })
        );
        assert_eq!(first.events.len(), READ_PAGE);
        assert_eq!(first.events[0].revision, 101);
        assert_eq!(first.next.revision, 164);
        assert_eq!(first.snapshot.revision, 200);
        let second = observer.read(Some(first.next)).unwrap();
        assert_eq!(second.events.len(), 36);
        assert_eq!(second.events[0].revision, 165);
        assert_eq!(second.next.revision, 200);
        assert!(second.gap.is_none());
        assert_eq!(second.observed_at, first.observed_at);
        assert!(observer.read(Some(second.next)).unwrap().events.is_empty());
        // Subscribing later does not pretend the old snapshot was just observed.
        assert_eq!(
            observer.clone().read(None).unwrap().observed_at,
            first.observed_at
        );
    }

    #[test]
    fn empty_initial_and_max_cursors_do_not_overflow_or_invent_events() {
        let observer = publication(0, 0, 0);
        let read = observer.read(None).unwrap();
        assert_eq!(read.next.revision, 0);
        assert!(read.events.is_empty());
        let observer = publication(u64::MAX, u64::MAX, 1);
        let read = observer.read(None).unwrap();
        assert_eq!(read.next.revision, u64::MAX);
        assert!(observer.read(Some(read.next)).unwrap().events.is_empty());
        observer.0.lock().unwrap().exhausted = true;
        assert_eq!(observer.read(None).unwrap_err(), ReadError::Exhausted);
    }

    #[test]
    fn actual_owner_eviction_closure_and_exhaustion_keep_original_observation() {
        let owner = Owner::new(1, 2).unwrap();
        let observer = owner.observer();
        owner.publish(vec![(None, Event::StateChanged); 5]).unwrap();
        let read = observer
            .read(Some(Cursor {
                generation: owner.generation,
                revision: 0,
            }))
            .unwrap();
        assert_eq!(read.gap, Some(Gap { first: 1, last: 3 }));
        assert_eq!(read.events.len(), 2);
        let at = read.observed_at;
        drop(owner);
        let read = observer.read(Some(read.next)).unwrap();
        assert!(read.snapshot.closed);
        assert_eq!(read.observed_at, at);
        assert_eq!(read.events.len(), 1);
        assert!(matches!(read.events[0].event, Event::OwnerClosed));

        let owner = Owner::new(1, 2).unwrap();
        let observer = owner.observer();
        observer.0.lock().unwrap().snapshot = Arc::new(Snapshot {
            generation: owner.generation,
            revision: u64::MAX,
            closed: false,
            jobs: Vec::new(),
            epochs: Vec::new(),
        });
        owner.publish(vec![(None, Event::StateChanged)]).unwrap();
        assert_eq!(observer.read(None).unwrap_err(), ReadError::Exhausted);
        assert!(owner.is_empty());
    }
}
