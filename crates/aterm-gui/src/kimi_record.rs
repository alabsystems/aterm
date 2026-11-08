// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kimi Code's own record of the approvals its box waits on, read on one lane thread for the
//! status pass (`docs/DESIGN-kimi-wire-verdict-2026-11-07.md`). A Kimi tab whose screen shows a
//! Bash box the read-only rule passes asks for a read of its foreground process's record
//! ([`ask`]); the lane binds that process to its sessions' wires
//! ([`aterm_agent::harness::kimi_wire::Bound`]) and keeps the latest read, and [`judge`] gives the
//! box its published verdict from both. Nothing here presses: the verdict is display.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::harness::kimi_wire::{self, Bound, Pending, Verdict};

/// How long a box must have stood before a read of its record counts. Kimi appends a request
/// to its wire on its next microtask and writes it asynchronously (`AppendLogStore`, read in
/// 2.5.2's bundle), so a box can stand before its own request is on disk; an ordinary write
/// lands well within this. Nothing bounds a write held longer (an fsync stalled under load, a
/// full or failing disk), and Kimi draws its next box regardless, so the record can lag the
/// screen by any time: what keeps an answered request, still open on disk, from standing for
/// the box drawn after it is that a box is answered through its tab, so only a request asked
/// after the program last read the tab's input may stand for the box
/// ([`kimi_wire::verdict`], [`crate::session_status::StatusObserver::note_input`]).
pub(crate) const SETTLE: Duration = Duration::from_millis(500);

/// One read of a tab's record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    /// The process read for: the tab's foreground group leader.
    pub(crate) pid: i32,
    /// When the read began: everything on disk then is in it.
    pub(crate) at: Instant,
    /// What it found pending ([`Bound::read`]); none when the record could not say.
    pub(crate) pending: Option<Vec<Pending>>,
}

/// The latest read of each session's record.
static READS: Mutex<BTreeMap<u64, Read>> = Mutex::new(BTreeMap::new());

enum Command {
    Read { session: u64, pid: i32 },
    Forget(u64),
}

/// The one thread records are read on, started on first use.
static LANE: Mutex<Option<Sender<Command>>> = Mutex::new(None);

/// Queues a read of `session`'s record for its foreground process `pid`.
pub(crate) fn ask(session: u64, pid: i32) {
    if pid <= 0 {
        return;
    }
    let mut slot = LANE.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_none() {
        let (tx, rx) = channel::<Command>();
        match crate::qos::spawn_lane_thread(
            "aterm-kimi-record",
            crate::qos::Role::Responsive,
            move || {
                run(&rx);
            },
        ) {
            Ok(_) => *slot = Some(tx),
            Err(e) => {
                aterm_log::warn!("kimi record: could not start its thread: {e}");
                return;
            }
        }
    }
    if slot
        .as_ref()
        .is_some_and(|tx| tx.send(Command::Read { session, pid }).is_err())
    {
        // The thread is gone (a panic in a read): a fresh one next time.
        *slot = None;
    }
}

/// Drops a retired session's reads and bindings.
pub(crate) fn forget(session: u64) {
    let slot = LANE.lock().unwrap_or_else(PoisonError::into_inner);
    match slot.as_ref() {
        Some(tx) => {
            let _ = tx.send(Command::Forget(session));
        }
        None => {
            READS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&session);
        }
    }
}

/// The latest read of `session`'s record.
fn latest(session: u64) -> Option<Read> {
    READS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&session)
        .cloned()
}

fn run(rx: &Receiver<Command>) {
    let mut bounds: BTreeMap<u64, (i32, Bound)> = BTreeMap::new();
    while let Ok(first) = rx.recv() {
        let mut asks = BTreeMap::new();
        for command in std::iter::once(first).chain(rx.try_iter()) {
            match command {
                Command::Read { session, pid } => {
                    asks.insert(session, pid);
                }
                Command::Forget(session) => {
                    asks.remove(&session);
                    bounds.remove(&session);
                    READS
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&session);
                }
            }
        }
        for (session, pid) in asks {
            let bound = bounds
                .entry(session)
                .or_insert_with(|| (pid, Bound::default()));
            if bound.0 != pid {
                *bound = (pid, Bound::default());
            }
            let at = Instant::now();
            let pending =
                facts(pid).and_then(|(home, cwd, started)| bound.1.read(&home, &cwd, started));
            READS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(session, Read { pid, at, pending });
        }
    }
}

/// `pid`'s Kimi home, working directory and start: the home the files it holds open name
/// ([`kimi_wire::kimi_home_of`]), none where any of them is unread.
fn facts(pid: i32) -> Option<(PathBuf, PathBuf, SystemTime)> {
    let id = u32::try_from(pid).ok()?;
    let home = kimi_wire::kimi_home_of(&aterm_uds::holders::open_files(id).ok()?)?;
    let cwd = aterm_uds::holders::cwd_of(id)?;
    let birth = aterm_uds::process::read_process_start(pid)?.wall?;
    let started = SystemTime::UNIX_EPOCH
        + Duration::from_secs(birth.seconds)
        + Duration::from_micros(birth.microseconds);
    Some((home, cwd, started))
}

/// How long after a box is first seen (or its program last read input under it) its record is
/// read again while it reads `bash` or `bash:read-only`: well past an ordinary write ([`SETTLE`]), so a
/// request that lands late can still decide it or take `read-only` away, and a box whose record
/// never answers (one drawn otherwise than written, or a process whose start is unread) is not
/// read on and on while it waits on a person.
pub(crate) const READ_FOR: Duration = Duration::from_secs(5);

/// The wall clock now, in whole milliseconds since the Unix epoch (0 before it): the clock
/// Kimi stamps its records with, so the moment a tab's input was read compares with them.
pub(crate) fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The Bash box standing: its rows' key ([`crate::presence::Drawn::key`]) and when the program
/// last read its tab's input as it was first seen so, when that was, and the verdict it
/// settled at when no read can change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    key: u64,
    input_ms: u64,
    since: Instant,
    settled: Option<Verdict>,
}

#[cfg(test)]
impl Seen {
    /// When the box was first seen so.
    pub(crate) const fn since(self) -> Instant {
        self.since
    }
}

/// What [`judge_read`] made of a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Judged {
    /// Not a Kimi Bash box the screen passes: left as it was.
    Untouched,
    /// Judged from the record, which needs reading no more while this screen stands:
    /// `bash:not-read-only`, which no later line makes safer, or a verdict past [`READ_FOR`]
    /// (`bash:read-only` off a read begun since its screen was first seen).
    Settled,
    /// Judged `bash` or `bash:read-only` before [`READ_FOR`], or `bash:read-only` off a read
    /// begun before its screen was: read the box again, since a request that lands on disk
    /// later may decide it or take `read-only` away.
    Again,
}

/// [`judge_read`] with the lane's latest read of `session`'s record, asked for again each time a
/// box needs one.
pub(crate) fn judge(
    session: u64,
    pid: i32,
    verdict: &mut crate::presence::AgentVerdict,
    drawn: impl FnOnce() -> Option<crate::presence::Drawn>,
    input: (u64, Instant),
    seen: &mut Option<Seen>,
    now: Instant,
) -> Judged {
    judge_read(
        verdict,
        drawn,
        || {
            ask(session, pid);
            latest(session)
        },
        pid,
        input,
        seen,
        now,
    )
}

/// Give a Kimi Bash box whose screen passes the read-only rule its verdict from its record
/// (the design's §3). `drawn` is what the box draws. `input_ms` is when the program last read
/// its tab's input, in wall-clock ms since the Unix epoch
/// ([`crate::session_status::StatusObserver::note_input`]); `screen_at` is when the screen
/// judged was first seen. The box is seen anew when its own rows change or its program reads
/// more input: a box drawn like the one before it, once that one was answered (which takes
/// input), is a box seen anew, whatever the rest of the screen shows. `bash:read-only` comes
/// only off a read (`read`, taken only for such a box) begun [`SETTLE`] after the box was seen
/// so, for `pid`, whose pending requests [`kimi_wire::verdict`] passes against the box's
/// readings, one asked after `input_ms` being what it draws; `bash:not-read-only` where a
/// pending request fails; `bash` where the record cannot say. Every other verdict is left as it
/// is ([`Judged::Untouched`]). A box judged other than `bash:not-read-only` before [`READ_FOR`]
/// has passed is to be read again ([`Judged::Again`]): a request may land on disk after it. So
/// is one judged `bash:read-only` off a read begun before its screen was, until [`READ_FOR`]
/// after that screen: a box Kimi swapped for one drawn alike, with no input between them, is
/// judged off a read begun after the swap one pass later. A box settled at `bash:not-read-only`,
/// or at `bash` past [`READ_FOR`], keeps that verdict while it stands, without a read.
pub(crate) fn judge_read(
    verdict: &mut crate::presence::AgentVerdict,
    drawn: impl FnOnce() -> Option<crate::presence::Drawn>,
    read: impl FnOnce() -> Option<Read>,
    pid: i32,
    (input_ms, screen_at): (u64, Instant),
    seen: &mut Option<Seen>,
    now: Instant,
) -> Judged {
    let crate::presence::AgentVerdict::Agent {
        reading,
        program: aterm_phase::Program::Kimi,
        ..
    } = verdict
    else {
        *seen = None;
        return Judged::Untouched;
    };
    let crate::presence::AgentPhase::Prompt { detail } = &mut reading.phase else {
        *seen = None;
        return Judged::Untouched;
    };
    if detail.as_deref() != Some("bash:read-only") {
        *seen = None;
        return Judged::Untouched;
    }
    let Some(drawn) = drawn() else {
        // A box the screen's rule passed that its reader then reads no box: nothing to bind.
        *detail = Some("bash".to_string());
        *seen = None;
        return Judged::Settled;
    };
    if !seen.is_some_and(|s| s.key == drawn.key && s.input_ms == input_ms) {
        *seen = None;
    }
    let standing = seen.get_or_insert(Seen {
        key: drawn.key,
        input_ms,
        since: now,
        settled: None,
    });
    let word = |judged| {
        match judged {
            Verdict::ReadOnly => "bash:read-only",
            Verdict::NotReadOnly => "bash:not-read-only",
            Verdict::Withheld => "bash",
        }
        .to_string()
    };
    if let Some(settled) = standing.settled {
        *detail = Some(word(settled));
        return Judged::Settled;
    }
    let since = standing.since;
    let python_allow: Vec<String> = aterm_agent::supervise::DEFAULT_PYTHON_ALLOW
        .iter()
        .map(|g| (*g).to_string())
        .collect();
    let counted = read().filter(|r| r.pid == pid && r.at >= since + SETTLE);
    let fresh = counted.as_ref().is_some_and(|r| r.at >= screen_at);
    let judged = match counted {
        Some(read) => kimi_wire::verdict(
            read.pending.as_deref(),
            &drawn.readings,
            &python_allow,
            input_ms,
        ),
        None => Verdict::Withheld,
    };
    *detail = Some(word(judged));
    if judged != Verdict::NotReadOnly && now < since + READ_FOR
        || judged == Verdict::ReadOnly && !fresh && now < screen_at + READ_FOR
    {
        return Judged::Again;
    }
    if judged != Verdict::ReadOnly {
        standing.settled = Some(judged);
    }
    Judged::Settled
}

#[cfg(test)]
#[path = "kimi_record_tests.rs"]
pub(crate) mod tests;
