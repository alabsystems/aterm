// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bridge loop wait tally ([`SEAM`](crate::wait_tally::SEAM), [`FILE`](crate::wait_tally::FILE), [`Tally::parse`](crate::wait_tally::Tally::parse), [`Tally::since`](crate::wait_tally::Tally::since),
//! [`Tally::at_ns`](crate::wait_tally::Tally::at_ns), [`Recorder::idle`](crate::wait_tally::Recorder::idle)) with per-[`Row`](crate::wait_tally::Row) log2 [`Hist`](crate::wait_tally::Hist) buckets
//! ([`Row::partitions`](crate::wait_tally::Row::partitions)): broker ([`Row::Request`](crate::wait_tally::Row::Request), [`Row::BudgetRequest`](crate::wait_tally::Row::BudgetRequest), [`Row::Ack`](crate::wait_tally::Row::Ack), [`Row::Commit`](crate::wait_tally::Row::Commit)), verb lane
//! ([`Row::LaneOutbox`](crate::wait_tally::Row::LaneOutbox), the presence reads [`Row::LaneStatus`](crate::wait_tally::Row::LaneStatus) to
//! [`Row::LaneSessionsStatus`](crate::wait_tally::Row::LaneSessionsStatus), [`Row::LaneOther`](crate::wait_tally::Row::LaneOther), [`lane_row`](crate::wait_tally::lane_row)), [`crate::state::StateDir`]
//! ([`Row::DirReplace`](crate::wait_tally::Row::DirReplace), [`Row::DirFlush`](crate::wait_tally::Row::DirFlush), [`Row::SetPostPin`](crate::wait_tally::Row::SetPostPin), [`Row::AppendSelfAcked`](crate::wait_tally::Row::AppendSelfAcked),
//! [`Row::LogpinFence`](crate::wait_tally::Row::LogpinFence), [`Kept`](crate::wait_tally::Kept), [`Tally::dir`](crate::wait_tally::Tally::dir)), [`crate::mailbox::Mailbox`] wakes ([`Row::WakeEvent`](crate::wait_tally::Row::WakeEvent),
//! [`Row::QueuedRecord`](crate::wait_tally::Row::QueuedRecord)), posts ([`Row::PostPublish`](crate::wait_tally::Row::PostPublish), [`Row::DrainPosts`](crate::wait_tally::Row::DrainPosts)), and loop park/write
//! ([`Row::Park`](crate::wait_tally::Row::Park), [`Row::TallyWrite`](crate::wait_tally::Row::TallyWrite)). The line's head ([`FORMAT`](crate::wait_tally::FORMAT)) names how many rows
//! follow, so a reader built for another count refuses the line, and carries the process's
//! `astream_sys::flush_tally` beside the word that says which flushes it counts
//! ([`Tally::flushes`](crate::wait_tally::Tally::flushes), `aterm_durable::ASTREAM_COUNTS`), then
//! how long they took and how many were slow (`astream_sys::flush_times`,
//! [`Tally::times`](crate::wait_tally::Tally::times)). The tail after the rows carries the
//! process's last slow calls ([`Tally::calls`](crate::wait_tally::Tally::calls): each call of
//! [`SLOW_CALL_BAND`](crate::wait_tally::SLOW_CALL_BAND) or slower, its row, its start on
//! `astream_sys::clock_ns`'s clock and its length) and astream-sys's ring of its last slow
//! flushes, each with its index, so every slow call can be lined up with the flushes that ran
//! under it and a reader can say which slow flushes the ring lost ([`Flushed::missed`](crate::wait_tally::Flushed::missed)).

use std::io;
use std::path::Path;
#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aterm_types::wait_tally::{put_row, row_len, row_text, sum_text, Words};
pub use aterm_types::wait_tally::{Hist, Unit, BUCKETS};

/// The development seam that turns the tally on.
pub const SEAM: &str = "ATERM_LINK_WAIT_TALLY";

/// The file in the bridge's state dir the tally is written to.
pub const FILE: &str = "wait-tally";

/// What the line starts with: the format's name and version. The head's fields follow it,
/// each fixed-width, the row count ([`ROWS`]) first.
pub const FORMAT: &str = "wait-tally v=8";

/// What `astream_sys::flush_tally`, in the head, counts of this build's flushes
/// (`aterm_durable::ASTREAM_COUNTS`): one word for every line a build writes, so every line
/// is one length.
pub const FLUSHES_COUNTED: &str = aterm_durable::ASTREAM_COUNTS;

/// One thing the loop does, counted ([`Hist`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Row {
    /// Requests that read their own reply, but the subject budget's ([`Row::BudgetRequest`]).
    Request,
    /// Of the requests that read their own reply, the node's subject budget's
    /// (`subject_budget`), apart from [`Row::Request`] (critic item G04).
    BudgetRequest,
    /// Pipelined acks read, waited for or not.
    Ack,
    /// Of both, the inbox group's commits (a pipelined ack each since aterm-0).
    Commit,
    /// The verb lane's outbox peek (`outbox …`, not `outbox sent`).
    LaneOutbox,
    /// The verb lane's roster read (`sessions bridge`).
    LaneSessions,
    /// The verb lane's post retirement (`outbox sent …`).
    LaneSent,
    /// The verb lane's deliveries (`deliver …`).
    LaneDeliver,
    /// The verb lane's status read of one session (`@<sid> status`).
    LaneStatus,
    /// The verb lane's metadata read of one session (`@<sid> meta`).
    LaneMeta,
    /// The verb lane's screen read of one session (`@<sid> text …`).
    LaneText,
    /// The verb lane's status read of every session at once (`sessions status`).
    LaneSessionsStatus,
    /// Every other verb-lane round trip.
    LaneOther,
    /// State-dir atomic replaces.
    DirReplace,
    /// State-dir unlinks.
    DirUnlink,
    /// State-dir appends.
    DirAppend,
    /// State-dir flushes of a file in place or of a directory.
    DirFlush,
    /// `StateDir::set_post_pin`.
    SetPostPin,
    /// `StateDir::set_in_flight`.
    SetInFlight,
    /// `StateDir::clear_in_flight`: an in-flight entry ended alone. Every own record's but a
    /// landed post's or owed receipt's, whose entry ends with its group ([`Row::ClearPostSeq`]);
    /// and a landed post's or receipt's its group left standing: one a drain releases once
    /// aterm no longer lists the post or receipt (left by the attach's replay, or by aterm gone
    /// before it answered), and one a delivery ends after the disk refused its group's write or
    /// its self-lane offset.
    ClearInFlight,
    /// `StateDir::clear_post_seq`, and `StateDir::settle_posts`: an ack pass's landed posts in one
    /// write, their entries' ends with their reservations' (critic item E07), and each landed
    /// owed receipt's entry end, a group of one (critic item F15; a receipt's reservation is a
    /// file of its own, whose end is an unlink, [`Row::DirUnlink`]).
    ClearPostSeq,
    /// `StateDir::append_self_acked`.
    AppendSelfAcked,
    /// The mirror pin's fence: the one write of `logpin` an open makes durable
    /// (`StateDir::ensure_logpin_durable`, or the first `StateDir::set_logpin`), its two full
    /// flushes included (critic item D06), or orders for its broker's settle
    /// (`StateDir::order_logpin`, its two barriers included, critic item F07).
    LogpinFence,
    /// Aterm's events queued while the loop was parked.
    WakeEvent,
    /// Broker records queued while the loop was parked.
    WakeRecord,
    /// Aterm's events queued while the loop worked.
    QueuedEvent,
    /// Broker records queued while the loop worked.
    QueuedRecord,
    /// Time from taking a `post` event to writing each fresh post's publish.
    PostPublish,
    /// The posts each outbox peek listed (a count; 0 is an empty peek).
    DrainPosts,
    /// The loop parked on its mailbox.
    Park,
    /// This tally's own idle writes.
    TallyWrite,
}

/// How many rows a line holds.
pub const ROWS: usize = 31;

impl Row {
    /// Every row, in the line's order.
    pub const ALL: [Row; ROWS] = [
        Row::Request,
        Row::BudgetRequest,
        Row::Ack,
        Row::Commit,
        Row::LaneOutbox,
        Row::LaneSessions,
        Row::LaneSent,
        Row::LaneDeliver,
        Row::LaneStatus,
        Row::LaneMeta,
        Row::LaneText,
        Row::LaneSessionsStatus,
        Row::LaneOther,
        Row::DirReplace,
        Row::DirUnlink,
        Row::DirAppend,
        Row::DirFlush,
        Row::SetPostPin,
        Row::SetInFlight,
        Row::ClearInFlight,
        Row::ClearPostSeq,
        Row::AppendSelfAcked,
        Row::LogpinFence,
        Row::WakeEvent,
        Row::WakeRecord,
        Row::QueuedEvent,
        Row::QueuedRecord,
        Row::PostPublish,
        Row::DrainPosts,
        Row::Park,
        Row::TallyWrite,
    ];

    /// Its place in the line.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Its name in the line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Row::Request => "request",
            Row::BudgetRequest => "budget_request",
            Row::Ack => "ack",
            Row::Commit => "commit",
            Row::LaneOutbox => "lane_outbox",
            Row::LaneSessions => "lane_sessions",
            Row::LaneSent => "lane_sent",
            Row::LaneDeliver => "lane_deliver",
            Row::LaneStatus => "lane_status",
            Row::LaneMeta => "lane_meta",
            Row::LaneText => "lane_text",
            Row::LaneSessionsStatus => "lane_sessions_status",
            Row::LaneOther => "lane_other",
            Row::DirReplace => "dir_replace",
            Row::DirUnlink => "dir_unlink",
            Row::DirAppend => "dir_append",
            Row::DirFlush => "dir_flush",
            Row::SetPostPin => "set_post_pin",
            Row::SetInFlight => "set_in_flight",
            Row::ClearInFlight => "clear_in_flight",
            Row::ClearPostSeq => "clear_post_seq",
            Row::AppendSelfAcked => "append_self_acked",
            Row::LogpinFence => "logpin_fence",
            Row::WakeEvent => "wake_event",
            Row::WakeRecord => "wake_record",
            Row::QueuedEvent => "queued_event",
            Row::QueuedRecord => "queued_record",
            Row::PostPublish => "post_publish",
            Row::DrainPosts => "drain_posts",
            Row::Park => "park",
            Row::TallyWrite => "tally_write",
        }
    }

    /// What its values measure.
    #[must_use]
    pub const fn unit(self) -> Unit {
        match self {
            Row::DrainPosts => Unit::Count,
            _ => Unit::Ns,
        }
    }

    /// Whether the row is one of the disjoint spans that partition the loop's time.
    #[must_use]
    pub const fn partitions(self) -> bool {
        matches!(
            self,
            Row::Request
                | Row::BudgetRequest
                | Row::Ack
                | Row::LaneOutbox
                | Row::LaneSessions
                | Row::LaneSent
                | Row::LaneDeliver
                | Row::LaneStatus
                | Row::LaneMeta
                | Row::LaneText
                | Row::LaneSessionsStatus
                | Row::LaneOther
                | Row::DirReplace
                | Row::DirUnlink
                | Row::DirAppend
                | Row::DirFlush
                | Row::Park
                | Row::TallyWrite
        )
    }

    /// The row the state dir keeps ([`Kept`]).
    #[must_use]
    pub const fn kept_by_the_dir(self) -> bool {
        matches!(
            self,
            Row::DirReplace
                | Row::DirUnlink
                | Row::DirAppend
                | Row::DirFlush
                | Row::SetPostPin
                | Row::SetInFlight
                | Row::ClearInFlight
                | Row::ClearPostSeq
                | Row::AppendSelfAcked
                | Row::LogpinFence
        )
    }

    /// Whether its spans are calls, which [`Tally::calls`] keeps when slow: every time row but
    /// the loop's park (its idle wait for work) and the mailbox's queue waits (how long an
    /// item waited for the loop, not a call it made).
    #[must_use]
    pub const fn is_call(self) -> bool {
        !matches!(
            self,
            Row::DrainPosts
                | Row::Park
                | Row::WakeEvent
                | Row::WakeRecord
                | Row::QueuedEvent
                | Row::QueuedRecord
        )
    }
}

/// The row a verb-lane request is counted in, by its verb head: for a verb addressed to one
/// session (`@<sid> <verb> …`), the verb after the address.
#[must_use]
pub fn lane_row(line: &str) -> Row {
    let headed = |verb: &str| {
        line.strip_prefix(verb)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    };
    if line.starts_with("outbox sent ") {
        Row::LaneSent
    } else if headed("outbox") {
        Row::LaneOutbox
    } else if line == "sessions bridge" {
        Row::LaneSessions
    } else if headed("sessions status") {
        Row::LaneSessionsStatus
    } else if line.starts_with("deliver ") {
        Row::LaneDeliver
    } else if let Some((_, verb)) = line.strip_prefix('@').and_then(|l| l.split_once(' ')) {
        match verb.split(' ').next() {
            Some("status") => Row::LaneStatus,
            Some("meta") => Row::LaneMeta,
            Some("text") => Row::LaneText,
            _ => Row::LaneOther,
        }
    } else {
        Row::LaneOther
    }
}

/// The loop's time since this process started, and where its inbox group cursor stood at the write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// The bridge process the counts belong to.
    pub pid: u32,
    /// The inbox group cursor this process last committed (0 before any).
    pub committed: u64,
    /// The bridge's own monotonic clock at the write, in nanoseconds since its recorder started.
    pub at_ns: u64,
    /// Whether the state dir's rows were counted ([`Row::kept_by_the_dir`]).
    pub dir: bool,
    /// The flushes this process had asked astream-sys for at the write
    /// (`astream_sys::flush_tally`), of which [`FLUSHES_COUNTED`] says which kinds it counts.
    pub flushes: astream_sys::FlushTally,
    /// How long those flushes took, and the last slow ones, at the write
    /// (`astream_sys::flush_times`).
    pub times: Flushed,
    /// Each [`Row`], at its [`Row::index`].
    pub rows: [Hist; ROWS],
    /// The process's slow calls: how many, and its last [`CALLS_KEPT`] with their starts on
    /// [`FLUSH_CLOCK`].
    pub calls: SlowCalls,
}

/// The clock [`Tally::calls`] and [`Tally::times`] are on: astream-sys's, the machine's on
/// Apple (so another process's flushes compare), this process's elsewhere.
pub const FLUSH_CLOCK: astream_sys::FlushClock = astream_sys::FLUSH_CLOCK;

/// The band a call reaches to be a slow one ([`Tally::calls`]): band 11, 2^20 ns (1.05 ms)
/// and more, so the calls a window kept number at most its call rows' values from this band
/// up ([`Hist::at_least`]).
pub const SLOW_CALL_BAND: usize = 11;

/// How many slow calls a line carries: the process's last ones.
pub const CALLS_KEPT: usize = 128;

// A place writes its row's index in two digits.
const _: () = assert!(ROWS < 100);

/// One call of [`SLOW_CALL_BAND`] or slower: its row, and its start and length on
/// [`FLUSH_CLOCK`] (its start is the clock at its end less its length, the length read on
/// std's `Instant`, which reads the same clock on Apple).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlowCall {
    /// The row it was counted in ([`Row::is_call`]).
    pub row: Row,
    /// When it began, in nanoseconds on [`FLUSH_CLOCK`].
    pub start_ns: u64,
    /// How long it took.
    pub ns: u64,
}

impl SlowCall {
    /// When it ended.
    #[must_use]
    pub const fn end_ns(self) -> u64 {
        self.start_ns.saturating_add(self.ns)
    }
}

/// A process's slow calls ([`SlowCall`]): how many it made, and its last [`CALLS_KEPT`] in the
/// order they ended, fixed-size so a [`Tally`] stays `Copy` and its line one length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlowCalls {
    /// The slow calls counted, kept or not; in a difference ([`Tally::since`]), the window's.
    pub count: u64,
    /// The last ones, oldest first, then `None`s; in a difference, those of the window the
    /// later write still kept.
    pub kept: [Option<SlowCall>; CALLS_KEPT],
}

impl Default for SlowCalls {
    fn default() -> Self {
        Self {
            count: 0,
            kept: [None; CALLS_KEPT],
        }
    }
}

impl SlowCalls {
    /// The calls it keeps, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = SlowCall> + '_ {
        self.kept.iter().map_while(|c| *c)
    }

    /// How many it keeps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Whether it keeps none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kept[0].is_none()
    }

    /// Count `call`, which ended after every call it keeps, and keep it, the oldest leaving a
    /// full ring.
    fn push(&mut self, call: SlowCall) {
        self.count = self.count.saturating_add(1);
        let n = self.len();
        if n == CALLS_KEPT {
            self.kept.copy_within(1.., 0);
            self.kept[CALLS_KEPT - 1] = Some(call);
        } else {
            self.kept[n] = Some(call);
        }
    }

    /// Its last `n`, oldest first (all it keeps when it keeps fewer).
    fn last(&self, n: u64) -> [Option<SlowCall>; CALLS_KEPT] {
        let len = self.len();
        let skip = len.saturating_sub(usize::try_from(n).unwrap_or(usize::MAX));
        let mut out = [None; CALLS_KEPT];
        out[..len - skip].copy_from_slice(&self.kept[skip..len]);
        out
    }

    /// Its calls and `other`'s, each in the order they ended, merged by end (its own first on a
    /// tie): the last [`CALLS_KEPT`] of both, which each one's last ones hold, and both counts.
    #[must_use]
    pub fn merged(&self, other: &SlowCalls) -> SlowCalls {
        let (mut a, mut b) = (self.iter().peekable(), other.iter().peekable());
        let mut all = Vec::with_capacity(2 * CALLS_KEPT);
        while let Some(next) = match (a.peek(), b.peek()) {
            (Some(x), Some(y)) if y.end_ns() < x.end_ns() => b.next(),
            (Some(_), _) => a.next(),
            (None, _) => b.next(),
        } {
            all.push(next);
        }
        let mut kept = [None; CALLS_KEPT];
        let skip = all.len().saturating_sub(CALLS_KEPT);
        for (slot, call) in kept.iter_mut().zip(&all[skip..]) {
            *slot = Some(*call);
        }
        SlowCalls {
            count: self.count.saturating_add(other.count),
            kept,
        }
    }
}

/// Whether another process's times on [`FLUSH_CLOCK`] line up with this one's: on the machine's
/// clock they do (one boot); on a process's own they do not.
pub const ACROSS_PROCESSES: bool = matches!(FLUSH_CLOCK, astream_sys::FlushClock::Machine);

/// A slow flush as the ring keeps it, its kind, and the clock it is on (astream-sys's).
pub use astream_sys::{clock_ns, FlushKind, SlowFlush};

/// How many slow flushes a line carries: astream-sys's ring, whole.
pub const RING: usize = astream_sys::SLOW_FLUSHES_KEPT;

/// The process's flush times at a write (`astream_sys::flush_times`), fixed-size so a [`Tally`]
/// stays `Copy` and its line one length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flushed {
    /// Nanoseconds in full flushes (`full_sync`, `full_sync_data`) that returned.
    pub full_ns: u64,
    /// Nanoseconds in barriers that returned.
    pub barrier_ns: u64,
    /// Nanoseconds in plain flushes that returned.
    pub plain_ns: u64,
    /// The flushes slower than `astream_sys::SLOW_FLUSH_NS` that returned, kept or not; in a
    /// difference, the window's.
    pub slow_count: u64,
    /// The ring's slow flushes, oldest first (by their indices, each once), then `None`s: the
    /// process's last [`RING`] at the write (a difference keeps the later write's).
    pub slow: [Option<astream_sys::SlowFlush>; RING],
    /// The calls into astream-sys that had begun and not yet returned at the write
    /// (`astream_sys::FlushTimes::running`): flushes no count or ring here has yet, which may
    /// have begun at any time before it; 0, every slow flush begun by then is indexed below
    /// [`Flushed::slow_count`] (a difference keeps the later write's).
    pub running: u64,
}

impl Default for Flushed {
    fn default() -> Self {
        Self {
            full_ns: 0,
            barrier_ns: 0,
            plain_ns: 0,
            slow_count: 0,
            slow: [None; RING],
            running: 0,
        }
    }
}

impl Flushed {
    /// `t`, kept: its first [`RING`] slow flushes (astream-sys keeps no more).
    #[must_use]
    pub fn of(t: &astream_sys::FlushTimes) -> Self {
        let mut slow = [None; RING];
        for (slot, f) in slow.iter_mut().zip(&t.slow) {
            *slot = Some(*f);
        }
        Self {
            full_ns: t.full_ns,
            barrier_ns: t.barrier_ns,
            plain_ns: t.plain_ns,
            slow_count: t.slow_count,
            slow,
            running: t.running,
        }
    }

    /// This process's, now.
    #[must_use]
    pub fn now() -> Self {
        Self::of(&astream_sys::flush_times())
    }

    /// The slow flushes it holds, oldest first.
    pub fn slow(&self) -> impl Iterator<Item = astream_sys::SlowFlush> + '_ {
        self.slow.iter().map_while(|f| *f)
    }

    /// How many of the slow flushes indexed from `from` (an earlier reading's
    /// [`Flushed::slow_count`]) up to this one's count its ring does not hold: 0 exactly when it
    /// holds that window whole (`astream_sys::slow_flushes_missed`). Of a whole reading (a
    /// line's, or [`Flushed::now`]); a difference's count is the window's, so judge its window
    /// by the later reading's whole.
    #[must_use]
    pub fn missed(&self, from: u64) -> u64 {
        astream_sys::slow_flushes_missed(self.slow(), self.slow_count, from)
    }
}

/// The head: the format, the row count, the pid, the cursor, the clock, the dir flag, the
/// flush counts and the word that says what they count, then the flush clock's word, the
/// flush times, the slow count and the calls running, zero-padded decimal so its length is
/// fixed.
fn head_text(t: &Tally) -> String {
    format!(
        "{FORMAT} rows={ROWS:03} pid={:010} committed={:020} at={:020} dir={} full={:020} \
         barrier={:020} plain={:020} counted={FLUSHES_COUNTED} clock={} full_ns={:020} \
         barrier_ns={:020} plain_ns={:020} slow={:020} running={:020}",
        t.pid,
        t.committed,
        t.at_ns,
        u8::from(t.dir),
        t.flushes.full,
        t.flushes.barrier,
        t.flushes.plain,
        FLUSH_CLOCK.word(),
        t.times.full_ns,
        t.times.barrier_ns,
        t.times.plain_ns,
        t.times.slow_count,
        t.times.running,
    )
}

/// A slow flush's kind as the ring writes it, `-` for an empty place.
const fn kind_letter(f: Option<astream_sys::SlowFlush>) -> u8 {
    match f {
        Some(f) => match f.kind {
            astream_sys::FlushKind::Full => b'f',
            astream_sys::FlushKind::Barrier => b'b',
            astream_sys::FlushKind::Plain => b'p',
        },
        None => b'-',
    }
}

/// The tail after the rows: ` slow_calls=` the slow calls' count ([`SlowCalls::count`]) in
/// twenty decimal digits, then ` calls=` each place of [`Tally::calls`]'s ring as its row's
/// index in two decimal digits (`--` for none), its start and its length in sixteen hex digits
/// each, joined by `+`, the places joined by `.`; then ` ring=` each place of the flush ring
/// ([`Flushed::slow`]) as its kind's letter (`f`, `b`, `p`, `-` for none), its index, `@`, and
/// its start and end, joined by `-`, each in sixteen hex digits, the places joined by `.`.
fn tail_text(t: &Tally) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(tail_len());
    let _ = write!(out, " slow_calls={:020} calls=", t.calls.count);
    for (i, c) in t.calls.kept.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        match c {
            Some(c) => {
                let _ = write!(out, "{:02}{:016x}+{:016x}", c.row.index(), c.start_ns, c.ns);
            }
            None => {
                let _ = write!(out, "--{:016x}+{:016x}", 0, 0);
            }
        }
    }
    out.push_str(" ring=");
    for (i, f) in t.times.slow.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        let (index, start, end) = f.map_or((0, 0, 0), |f| (f.index, f.start_ns, f.end_ns));
        out.push(char::from(kind_letter(*f)));
        let _ = write!(out, "{index:016x}@{start:016x}-{end:016x}");
    }
    out
}

/// [`tail_text`]'s length.
const fn tail_len() -> usize {
    " slow_calls=".len() + 20 + " calls=".len() + CALLS_KEPT * (2 + 16 + 1 + 16 + 1) - 1
        + " ring=".len()
        + RING * (1 + 16 + 1 + 16 + 1 + 16 + 1)
        - 1
}

/// Every line's length ([`Tally::render`]): the head's ([`head_text`]: the
/// format, three digits of row count, ten of pid, twenty of cursor, twenty of clock, one of dir
/// flag, twenty of each flush count and the counted word, the flush clock's word, and twenty
/// of each flush time and of the slow count), each row's ([`row_len`]), the tail's
/// ([`tail_text`]) and the checksum's ([`sum_text`]).
fn line_len() -> usize {
    let head = FORMAT.len()
        + " rows=".len()
        + 3
        + " pid=".len()
        + 10
        + " committed=".len()
        + 20
        + " at=".len()
        + 20
        + " dir=".len()
        + 1
        + " full=".len()
        + 20
        + " barrier=".len()
        + 20
        + " plain=".len()
        + 20
        + " counted=".len()
        + FLUSHES_COUNTED.len()
        + " clock=".len()
        + FLUSH_CLOCK.word().len()
        + [
            " full_ns=",
            " barrier_ns=",
            " plain_ns=",
            " slow=",
            " running=",
        ]
        .iter()
        .map(|k| k.len() + 20)
        .sum::<usize>();
    let rows: usize = Row::ALL.iter().map(|&row| row_len(row.name())).sum();
    head + rows + tail_len() + " sum=".len() + 16 + "\n".len()
}

impl Tally {
    /// One row.
    #[must_use]
    pub const fn row(&self, r: Row) -> &Hist {
        &self.rows[r.index()]
    }

    /// Every wait on the broker, requests (the subject budget's too) and acks.
    #[must_use]
    pub const fn blocked_ns(&self) -> u64 {
        self.row(Row::Request)
            .total
            .saturating_add(self.row(Row::BudgetRequest).total)
            .saturating_add(self.row(Row::Ack).total)
    }

    /// The partitioning rows' total ([`Row::partitions`]).
    #[must_use]
    pub fn partitioned_ns(&self) -> u64 {
        Row::ALL
            .iter()
            .filter(|r| r.partitions())
            .fold(0u64, |sum, r| sum.saturating_add(self.row(*r).total))
    }

    /// The line [`Tally::parse`] reads, newline included.
    #[must_use]
    pub fn render(&self) -> String {
        let mut body = head_text(self).into_bytes();
        for row in Row::ALL {
            body.extend_from_slice(&row_text(row.name(), self.row(row)));
        }
        body.extend_from_slice(tail_text(self).as_bytes());
        let sum = sum_text(&body);
        body.extend_from_slice(sum.as_bytes());
        String::from_utf8(body).unwrap_or_default()
    }

    /// Read a line [`Tally::render`] wrote.
    ///
    /// # Errors
    /// A line that is not one, one of another row count, one whose flush counts count other
    /// flushes than this build's ([`FLUSHES_COUNTED`]), or one whose times are on another clock
    /// than this build's ([`FLUSH_CLOCK`]).
    pub fn parse(text: &str) -> Result<Tally, String> {
        let mut words = Words::of(text, FORMAT)?;
        let count = words.dec("rows")?;
        if usize::try_from(count).ok() != Some(ROWS) {
            return Err(format!(
                "a tally line of {count} rows, and this reader reads {ROWS}: another build's \
                 tally: {text:?}"
            ));
        }
        let pid = words.pid()?;
        let committed = words.dec("committed")?;
        let at_ns = words.dec("at")?;
        let dir = match words.next("dir")? {
            "0" => false,
            "1" => true,
            _ => return Err(format!("a tally line whose `dir` is not 0 or 1: {text:?}")),
        };
        let flushes = astream_sys::FlushTally {
            full: words.dec("full")?,
            barrier: words.dec("barrier")?,
            plain: words.dec("plain")?,
        };
        let counted = words.next("counted")?;
        if counted != FLUSHES_COUNTED {
            return Err(format!(
                "a tally line whose flush counts count `{counted}`, and this reader's count \
                 `{FLUSHES_COUNTED}`: another build's tally: {text:?}"
            ));
        }
        let clock = words.next("clock")?;
        if clock != FLUSH_CLOCK.word() {
            return Err(format!(
                "a tally line whose times are on the `{clock}` clock, and this reader's on the \
                 `{}` clock: another build's tally: {text:?}",
                FLUSH_CLOCK.word()
            ));
        }
        let mut times = Flushed {
            full_ns: words.dec("full_ns")?,
            barrier_ns: words.dec("barrier_ns")?,
            plain_ns: words.dec("plain_ns")?,
            slow_count: words.dec("slow")?,
            running: words.dec("running")?,
            ..Flushed::default()
        };
        let mut rows = [Hist::default(); ROWS];
        for (slot, row) in rows.iter_mut().zip(Row::ALL) {
            *slot = words.hist(row.name())?;
        }
        let hex = |v: &str, what: &str| -> Result<u64, String> {
            if v.len() == 16 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
                u64::from_str_radix(v, 16).map_err(|e| format!("{what}: {e}"))
            } else {
                Err(format!(
                    "a tally line whose {what} is not 16 hex digits: {text:?}"
                ))
            }
        };
        let mut calls = SlowCalls {
            count: words.dec("slow_calls")?,
            ..SlowCalls::default()
        };
        let mut each = words.next("calls")?.split('.');
        let (mut calls_ended, mut last_end) = (false, 0u64);
        for slot in &mut calls.kept {
            let place = each.next().ok_or_else(|| {
                format!("a tally line with fewer than {CALLS_KEPT} slow-call places: {text:?}")
            })?;
            let bad =
                || format!("a tally line whose slow-call place {place:?} is no call's: {text:?}");
            let (index, span) = place.split_at_checked(2).ok_or_else(bad)?;
            let (start, ns) = span.split_once('+').ok_or_else(bad)?;
            let (start_ns, ns) = (hex(start, "a call's start")?, hex(ns, "a call's length")?);
            if index == "--" && (start_ns, ns) == (0, 0) {
                calls_ended = true;
                continue;
            }
            let row = index
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| index.parse::<usize>().ok())
                .flatten()
                .and_then(|i| Row::ALL.get(i).copied())
                .filter(|row| row.is_call())
                .ok_or_else(bad)?;
            let call = SlowCall { row, start_ns, ns };
            if calls_ended || Hist::band(ns, Unit::Ns) < SLOW_CALL_BAND || call.end_ns() < last_end
            {
                return Err(format!(
                    "a tally line whose slow-call place {place:?} is out of its place, not slow, \
                     or ends before the call before it: {text:?}"
                ));
            }
            last_end = call.end_ns();
            *slot = Some(call);
        }
        if each.next().is_some() {
            return Err(format!(
                "a tally line with more than {CALLS_KEPT} slow-call places: {text:?}"
            ));
        }
        if u64::try_from(calls.len()).ok() != Some(calls.count.min(CALLS_KEPT as u64)) {
            return Err(format!(
                "a tally line that counts {} slow calls and keeps {}: {text:?}",
                calls.count,
                calls.len()
            ));
        }
        let mut places = words.next("ring")?.split('.');
        let mut ended = false;
        let mut last: Option<u64> = None;
        for slot in &mut times.slow {
            let place = places.next().ok_or_else(|| {
                format!("a tally line with fewer than {RING} ring places: {text:?}")
            })?;
            let (letter, rest) = place
                .split_at_checked(1)
                .ok_or_else(|| format!("a tally line with an empty ring place: {text:?}"))?;
            let (index, span) = rest
                .split_once('@')
                .ok_or_else(|| format!("a tally line whose ring place {place:?} has no index"))?;
            let (start, end) = span
                .split_once('-')
                .ok_or_else(|| format!("a tally line whose ring place {place:?} has no end"))?;
            let (index, start_ns, end_ns) = (
                hex(index, "a ring index")?,
                hex(start, "a ring start")?,
                hex(end, "a ring end")?,
            );
            let kind = match letter {
                "f" => astream_sys::FlushKind::Full,
                "b" => astream_sys::FlushKind::Barrier,
                "p" => astream_sys::FlushKind::Plain,
                "-" if (index, start_ns, end_ns) == (0, 0, 0) => {
                    ended = true;
                    continue;
                }
                _ => {
                    return Err(format!(
                        "a tally line whose ring place {place:?} is no flush's: {text:?}"
                    ));
                }
            };
            if ended || end_ns < start_ns {
                return Err(format!(
                    "a tally line whose ring place {place:?} is out of its place or ends before \
                     it starts: {text:?}"
                ));
            }
            if last.is_some_and(|l| index <= l) || index >= times.slow_count {
                return Err(format!(
                    "a tally line whose ring place {place:?} is not past the one before it or \
                     not below the {} slow flushes the line counts: {text:?}",
                    times.slow_count
                ));
            }
            last = Some(index);
            *slot = Some(astream_sys::SlowFlush {
                index,
                kind,
                start_ns,
                end_ns,
            });
        }
        if places.next().is_some() {
            return Err(format!(
                "a tally line with more than {RING} ring places: {text:?}"
            ));
        }
        words.end()?;
        Ok(Tally {
            pid,
            committed,
            at_ns,
            dir,
            flushes,
            times,
            rows,
            calls,
        })
    }

    /// What the loop did between `earlier` and this tally.
    ///
    /// # Errors
    /// Two processes' tallies (a relaunched bridge counts from zero again), a changed dir flag,
    /// or a clock, a flush count, a flush time, a slow flush or slow call count or a row that
    /// went backwards.
    pub fn since(&self, earlier: &Tally) -> Result<Tally, String> {
        if self.pid != earlier.pid {
            return Err(format!(
                "the tallies are of two bridge processes, pid {} and then {}: the bridge was \
                 relaunched between them",
                earlier.pid, self.pid
            ));
        }
        if self.dir != earlier.dir {
            return Err("the tallies disagree on whether the state dir was counted".to_string());
        }
        let at_ns = self.at_ns.checked_sub(earlier.at_ns).ok_or_else(|| {
            format!(
                "the tally's clock went backwards, {} to {}",
                earlier.at_ns, self.at_ns
            )
        })?;
        let flush = |what: &str, then: u64, now: u64| {
            now.checked_sub(then).ok_or_else(|| {
                format!("the tally's `{what}` flushes went backwards, {then} to {now}")
            })
        };
        let flushes = astream_sys::FlushTally {
            full: flush("full", earlier.flushes.full, self.flushes.full)?,
            barrier: flush("barrier", earlier.flushes.barrier, self.flushes.barrier)?,
            plain: flush("plain", earlier.flushes.plain, self.flushes.plain)?,
        };
        let time = |what: &str, then: u64, now: u64| {
            now.checked_sub(then)
                .ok_or_else(|| format!("the tally's {what} went backwards, {then} to {now}"))
        };
        let (was, is) = (&earlier.times, &self.times);
        let times = Flushed {
            full_ns: time("full flush time", was.full_ns, is.full_ns)?,
            barrier_ns: time("barrier time", was.barrier_ns, is.barrier_ns)?,
            plain_ns: time("plain flush time", was.plain_ns, is.plain_ns)?,
            slow_count: time("slow flush count", was.slow_count, is.slow_count)?,
            slow: is.slow,
            running: is.running,
        };
        let mut rows = [Hist::default(); ROWS];
        for (i, slot) in rows.iter_mut().enumerate() {
            *slot = self.rows[i]
                .since(&earlier.rows[i])
                .map_err(|why| format!("the tally's `{}`: {why}", Row::ALL[i].name()))?;
        }
        // Every call the window made ended after every call the earlier write kept, so the
        // window's are the later ring's last ones.
        let new = time("slow call count", earlier.calls.count, self.calls.count)?;
        let calls = SlowCalls {
            count: new,
            kept: self.calls.last(new),
        };
        Ok(Tally {
            pid: self.pid,
            committed: self.committed,
            at_ns,
            dir: self.dir,
            flushes,
            times,
            rows,
            calls,
        })
    }

    /// Whether anything but the clock, the flush counts and the tally's own writes differs from
    /// `other`: what makes an idle moment write. A write carries the flush counts of its moment;
    /// a flush alone is no move, since the process's counts move with any thread's flush, and
    /// the loop's own flushes come with the rows that time them.
    fn moved_from(&self, other: &Tally) -> bool {
        self.committed != other.committed
            || Row::ALL
                .iter()
                .any(|&r| r != Row::TallyWrite && self.row(r) != other.row(r))
    }
}

/// `d` in nanoseconds, saturating.
fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// Whether a span of `row` that took `ns` is a slow call ([`Tally::calls`]): a call row's, in
/// [`SLOW_CALL_BAND`] or past it.
fn is_slow(row: Row, ns: u64) -> bool {
    row.is_call() && Hist::band(ns, Unit::Ns) >= SLOW_CALL_BAND
}

/// When a span of `row` that took `ns` and ends now ended, on astream-sys's clock, read only
/// for a slow call ([`is_slow`]), so a span of two rows can share one reading.
fn end_of(row: Row, ns: u64) -> Option<u64> {
    is_slow(row, ns).then(astream_sys::clock_ns)
}

/// Count a span of `row` that took `ns` in `h`, and keep it in `calls` when it is a slow call
/// that ended at `end` ([`end_of`]): its start is `end` less `ns` (the clock std's `Instant`
/// reads on Apple, so the span's own clock and this one agree there to the gap between two
/// readings).
fn add_span(h: &mut Hist, calls: &mut SlowCalls, row: Row, ns: u64, end: Option<u64>) {
    if let Some(end) = end.filter(|_| is_slow(row, ns)) {
        calls.push(SlowCall {
            row,
            start_ns: end.saturating_sub(ns),
            ns,
        });
    }
    h.add(ns, row.unit());
}

/// When an item was queued for the loop, and whether the loop was parked on its mailbox then.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stamp {
    at: Instant,
    parked: bool,
}

impl Stamp {
    /// Now, the loop parked or not.
    pub(crate) fn now(parked: bool) -> Self {
        Self {
            at: Instant::now(),
            parked,
        }
    }

    /// The item is taken now: its wait, and whether it was a wake.
    pub(crate) fn taken(self) -> (u64, bool) {
        (nanos(self.at.elapsed()), self.parked)
    }
}

/// Rows another part of the bridge keeps for the recorder, under a lock of its own.
#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
#[derive(Debug, Default)]
pub(crate) struct Kept {
    on: AtomicBool,
    /// The rows, and the slow calls among them ([`Tally::calls`]).
    rows: Mutex<([Hist; ROWS], SlowCalls)>,
}

#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
impl Kept {
    /// Count from now on (`true`), or no more.
    pub(crate) fn turn_on(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    /// A span of `row`, counted when the timer drops.
    pub(crate) fn timer(&self, row: Row) -> Timer<'_> {
        Timer {
            kept: self,
            row,
            started: self.on.load(Ordering::Relaxed).then(Instant::now),
        }
    }

    fn note(&self, row: Row, ns: u64) {
        let mut kept = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let (rows, calls) = &mut *kept;
        // Its end is read under the lock, so the calls this keeps are in the order they ended,
        // and one [`Kept::copy_into`] holds every call that ended before it and none after.
        add_span(&mut rows[row.index()], calls, row, ns, end_of(row, ns));
    }

    /// Copy every row this keeps that holds a value into `t`, and merge its slow calls into
    /// `t`'s (the recorder's own, [`SlowCalls::merged`]).
    pub(crate) fn copy_into(&self, t: &mut Tally) {
        let kept = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let (rows, calls) = &*kept;
        for (i, h) in rows.iter().enumerate() {
            if h.count > 0 {
                t.rows[i] = *h;
            }
        }
        t.calls = t.calls.merged(calls);
    }
}

#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
/// One span a [`Kept`] counts when it drops ([`Kept::timer`]).
#[derive(Debug)]
pub(crate) struct Timer<'a> {
    kept: &'a Kept,
    row: Row,
    started: Option<Instant>,
}

#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
impl Drop for Timer<'_> {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            self.kept.note(self.row, nanos(started.elapsed()));
        }
    }
}

/// The bridge's side: counts while it is on, and writes at idle moments.
#[derive(Debug)]
pub(crate) struct Recorder {
    /// The tally file, open for the life of the process.
    out: Option<std::fs::File>,
    /// When the recorder started.
    started: Instant,
    tally: Tally,
    /// The slow calls of the rows this records itself; a write's [`Tally::calls`] is these
    /// merged with the ones the outside keeps ([`Kept::copy_into`]).
    calls: SlowCalls,
    /// What the file holds: the last tally written, and its line.
    written: Option<(Tally, Vec<u8>)>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            out: None,
            started: Instant::now(),
            tally: Tally::default(),
            calls: SlowCalls::default(),
            written: None,
        }
    }
}

impl Recorder {
    /// On when [`SEAM`] is set in a build that reads seams, writing to `<state>/`[`FILE`].
    pub(crate) fn from_env(state_root: &Path) -> Self {
        if aterm_types::dev_seam!(SEAM).is_none() {
            return Self::default();
        }
        let path = state_root.join(FILE);
        Self::at(&path).unwrap_or_else(|e| {
            eprintln!(
                "aterm-link: {SEAM} is set, but {} cannot be written ({e}); no wait tally",
                path.display()
            );
            Self::default()
        })
    }

    /// On, writing to a newly created private `path`.
    ///
    /// # Errors
    /// The exclusive open's, including an already occupied name.
    pub(crate) fn at(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self {
            out: Some(out),
            started: Instant::now(),
            tally: Tally {
                pid: std::process::id(),
                dir: cfg!(any(test, debug_assertions, feature = "dev-seams")),
                ..Tally::default()
            },
            calls: SlowCalls::default(),
            written: None,
        })
    }

    /// Whether anything is counted.
    pub(crate) const fn on(&self) -> bool {
        self.out.is_some()
    }

    /// When a counted span starts.
    pub(crate) fn starts(&self) -> Option<Instant> {
        self.on().then(Instant::now)
    }

    /// The span [`Recorder::starts`] began has ended now.
    pub(crate) fn note(&mut self, row: Row, started: Option<Instant>) {
        if let Some(started) = started {
            self.span(row, nanos(started.elapsed()));
        }
    }

    /// A span of `row` that took `ns` and ended now.
    fn span(&mut self, row: Row, ns: u64) {
        self.span_ended(row, ns, end_of(row, ns));
    }

    /// A span of `row` that took `ns` and ended at `end` ([`end_of`]).
    fn span_ended(&mut self, row: Row, ns: u64, end: Option<u64>) {
        add_span(
            &mut self.tally.rows[row.index()],
            &mut self.calls,
            row,
            ns,
            end,
        );
    }

    /// Count `n` in a row of counts ([`Unit::Count`]).
    pub(crate) fn count(&mut self, row: Row, n: u64) {
        if self.on() {
            self.tally.rows[row.index()].add(n, row.unit());
        }
    }

    /// A request written at `started` has had its reply read now, counted in `row`
    /// ([`Row::Request`], or [`Row::BudgetRequest`] for the subject budget's).
    pub(crate) fn request(&mut self, row: Row, started: Instant) {
        if self.on() {
            self.note(row, Some(started));
        }
    }

    /// When an ack read is about to start.
    pub(crate) fn ack_starts(&self) -> Option<Instant> {
        self.starts()
    }

    /// The ack read that [`Recorder::ack_starts`] began has returned an ack.
    pub(crate) fn ack(&mut self, started: Option<Instant>, commit: bool) {
        if let Some(started) = started {
            let ns = nanos(started.elapsed());
            // One span, two rows: one reading of its end, so a slow commit is its ack's span.
            let end = end_of(Row::Ack, ns);
            self.span_ended(Row::Ack, ns, end);
            if commit {
                self.span_ended(Row::Commit, ns, end);
            }
        }
    }

    /// An idle moment — the loop found nothing queued and has since read the acks it owed.
    pub(crate) fn idle(&mut self, committed: u64, outside: impl FnOnce(&mut Tally)) {
        use std::os::unix::fs::FileExt as _;
        if !self.on() {
            return;
        }
        let began = Instant::now();
        self.tally.committed = committed;
        self.tally.flushes = astream_sys::flush_tally();
        self.tally.calls = self.calls;
        outside(&mut self.tally);
        if self
            .written
            .as_ref()
            .is_some_and(|(w, _)| !self.tally.moved_from(w))
        {
            return;
        }
        // Read only for a write: a flush alone is no move, and its times ride the next write.
        self.tally.times = Flushed::now();
        self.tally.at_ns = nanos(self.started.elapsed());
        let line = match self.written.take() {
            Some((last, mut line)) => {
                let head = head_text(&self.tally);
                line[..head.len()].copy_from_slice(head.as_bytes());
                let mut at = head.len();
                for row in Row::ALL {
                    let len = row_len(row.name());
                    if self.tally.row(row) != last.row(row) {
                        put_row(&mut line[at..at + len], row.name(), self.tally.row(row));
                    }
                    at += len;
                }
                if self.tally.calls != last.calls || self.tally.times.slow != last.times.slow {
                    let tail = tail_text(&self.tally);
                    line[at..at + tail.len()].copy_from_slice(tail.as_bytes());
                }
                at += tail_len();
                let sum = sum_text(&line[..at]);
                line[at..].copy_from_slice(sum.as_bytes());
                line
            }
            None => self.tally.render().into_bytes(),
        };
        debug_assert_eq!(line.len(), line_len());
        let Some(out) = &self.out else {
            return;
        };
        match out.write_all_at(&line, 0) {
            Ok(()) => {
                self.written = Some((self.tally, line));
                self.note(Row::TallyWrite, Some(began));
            }
            Err(e) => {
                eprintln!("aterm-link: the wait tally could not be written ({e}); no wait tally");
                self.out = None;
            }
        }
    }

    /// The counts so far.
    #[cfg(test)]
    pub(crate) fn tally(&self) -> Tally {
        self.tally
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_types::wait_tally::checksum;

    fn hist(values: &[u64], unit: Unit) -> Hist {
        let mut h = Hist::default();
        for &v in values {
            h.add(v, unit);
        }
        h
    }

    fn sample() -> Tally {
        let mut t = Tally {
            pid: 4242,
            committed: 119,
            at_ns: 900_000_000,
            dir: true,
            flushes: astream_sys::FlushTally {
                full: 3,
                barrier: 40,
                plain: 5,
            },
            ..Tally::default()
        };
        t.rows[Row::Request.index()] = hist(&[31_000_000], Unit::Ns);
        t.rows[Row::Ack.index()] = hist(&[9_000, 18_000_000], Unit::Ns);
        t.rows[Row::Commit.index()] = hist(&[9_000], Unit::Ns);
        t.rows[Row::LaneDeliver.index()] = hist(&[120_000, 250_000, 40_000_000], Unit::Ns);
        t.rows[Row::DirReplace.index()] = hist(&[200_000], Unit::Ns);
        t.rows[Row::DrainPosts.index()] = hist(&[0, 1, 1, 3], Unit::Count);
        t.rows[Row::Park.index()] = hist(&[500_000_000], Unit::Ns);
        // Its three calls past 1.05 ms, in the order they ended; the park is no call.
        for c in [
            call(Row::Request, 7_000_000_000, 31_000_000),
            call(Row::Ack, 7_020_000_000, 18_000_000),
            call(Row::LaneDeliver, 7_100_000_000, 40_000_000),
        ] {
            t.calls.push(c);
        }
        t.times = Flushed {
            full_ns: 12_500_000,
            barrier_ns: 3_000_000,
            plain_ns: 0,
            slow_count: 2,
            running: 1,
            ..Flushed::default()
        };
        t.times.slow[0] = Some(slow(
            0,
            astream_sys::FlushKind::Full,
            7_000_002_000,
            7_009_000_000,
        ));
        t.times.slow[1] = Some(slow(
            1,
            astream_sys::FlushKind::Barrier,
            7_100_000_000,
            7_101_500_000,
        ));
        t
    }

    fn slow(
        index: u64,
        kind: astream_sys::FlushKind,
        start_ns: u64,
        end_ns: u64,
    ) -> astream_sys::SlowFlush {
        astream_sys::SlowFlush {
            index,
            kind,
            start_ns,
            end_ns,
        }
    }

    const fn call(row: Row, start_ns: u64, ns: u64) -> SlowCall {
        SlowCall { row, start_ns, ns }
    }

    /// THE BANDS ARE LOG2, and the edges are where the unit says: under a
    /// microsecond-ish 1024 ns is band 0, 1024 opens band 1, 2^26 ns (67.1 ms)
    /// and anything past it is the last; a count's band 0 is 0 alone.
    #[test]
    fn a_value_falls_in_its_log2_band_and_each_band_starts_at_its_floor() {
        assert_eq!(Hist::band(0, Unit::Ns), 0);
        assert_eq!(Hist::band(1023, Unit::Ns), 0);
        assert_eq!(Hist::band(1024, Unit::Ns), 1);
        assert_eq!(Hist::band(2047, Unit::Ns), 1);
        assert_eq!(Hist::band(2048, Unit::Ns), 2);
        assert_eq!(Hist::band((1 << 26) - 1, Unit::Ns), 16);
        assert_eq!(Hist::band(1 << 26, Unit::Ns), BUCKETS - 1);
        assert_eq!(Hist::band(u64::MAX, Unit::Ns), BUCKETS - 1);
        assert_eq!(Hist::band(0, Unit::Count), 0);
        assert_eq!(Hist::band(1, Unit::Count), 1);
        assert_eq!(Hist::band(3, Unit::Count), 2);
        assert_eq!(Hist::band(4, Unit::Count), 3);
        for unit in [Unit::Ns, Unit::Count] {
            for i in 1..BUCKETS {
                let floor = Hist::floor(i, unit);
                assert_eq!(Hist::band(floor, unit), i, "{unit:?} band {i}");
                assert_eq!(
                    Hist::band(floor - 1, unit),
                    i - 1,
                    "{unit:?} below band {i}"
                );
            }
        }
        let h = hist(&[500, 3_000, 9_000_000, 70_000_000], Unit::Ns);
        assert_eq!((h.count, h.total, h.max), (4, 79_003_500, 70_000_000));
        assert_eq!(
            h.at_least(Hist::band(1 << 20, Unit::Ns)),
            2,
            "two at 1 ms or more"
        );
        assert_eq!(h.top(Unit::Ns), Some((1 << 26, None)), "the open last band");
        assert_eq!(
            hist(&[3_000], Unit::Ns).top(Unit::Ns),
            Some((2048, Some(4096)))
        );
        assert_eq!(Hist::default().top(Unit::Ns), None);
    }

    /// THE V=6 LINE READS BACK AS WRITTEN, AND EVERY LINE IS ONE LENGTH, so a
    /// write at offset 0 always replaces the line whole: the smallest tally
    /// and the largest render to the same width, a full ring of slow calls and
    /// a full flush ring included.
    #[test]
    fn a_tally_line_reads_back_as_written_and_every_line_is_one_length() {
        let t = sample();
        let line = t.render();
        assert!(
            line.starts_with("wait-tally v=8 rows=031 pid=0000004242 "),
            "{line}"
        );
        assert!(
            line.contains(&format!(
                " full=00000000000000000003 barrier=00000000000000000040 \
                 plain=00000000000000000005 counted={FLUSHES_COUNTED} clock={} \
                 full_ns=00000000000012500000 barrier_ns=00000000000003000000 \
                 plain_ns=00000000000000000000 slow=00000000000000000002 \
                 running=00000000000000000001 request=",
                FLUSH_CLOCK.word()
            )),
            "the flush counts, what they count, their clock and times end the head: {line}"
        );
        assert!(
            line.contains(&format!(
                " slow_calls=00000000000000000003 calls=00{:016x}+{:016x}.02{:016x}+{:016x}.\
                 07{:016x}+{:016x}.--0000000000000000+0000000000000000.",
                7_000_000_000u64,
                31_000_000u64,
                7_020_000_000u64,
                18_000_000u64,
                7_100_000_000u64,
                40_000_000u64
            )) && line.contains(&format!(
                " ring=f{:016x}@{:016x}-{:016x}.b{:016x}@{:016x}-{:016x}.\
                 -0000000000000000@0000000000000000-0000000000000000.",
                0u64, 7_000_002_000u64, 7_009_000_000u64, 1u64, 7_100_000_000u64, 7_101_500_000u64
            )),
            "the slow calls, then the flush ring, each oldest first, then its empty places: {line}"
        );
        assert_eq!(
            (t.times.slow().count(), t.calls.len()),
            (2, 3),
            "each ring's entries end at its first empty place"
        );
        assert_eq!(Tally::parse(&line), Ok(t));
        assert_eq!(t.blocked_ns(), 31_000_000 + 18_009_000);
        let zero = Tally::default();
        let full = Tally {
            pid: u32::MAX,
            committed: u64::MAX,
            at_ns: u64::MAX,
            dir: true,
            flushes: astream_sys::FlushTally {
                full: u64::MAX,
                barrier: u64::MAX,
                plain: u64::MAX,
            },
            times: Flushed {
                full_ns: u64::MAX,
                barrier_ns: u64::MAX,
                plain_ns: u64::MAX,
                slow_count: u64::MAX,
                slow: std::array::from_fn(|i| {
                    let index = u64::MAX - RING as u64 + i as u64;
                    Some(slow(
                        index,
                        astream_sys::FlushKind::Plain,
                        u64::MAX,
                        u64::MAX,
                    ))
                }),
                running: u64::MAX,
            },
            rows: [Hist {
                count: u64::MAX,
                total: u64::MAX,
                max: u64::MAX,
                buckets: [u32::MAX; BUCKETS],
            }; ROWS],
            calls: SlowCalls {
                count: u64::MAX,
                kept: [Some(call(Row::TallyWrite, u64::MAX, u64::MAX)); CALLS_KEPT],
            },
        };
        assert_eq!(zero.render().len(), full.render().len());
        assert_eq!(
            [zero.render().len(), full.render().len(), line.len()],
            [line_len(); 3],
            "the counted length is every line's"
        );
        assert_eq!(Tally::parse(&full.render()), Ok(full));
        assert_eq!(full.blocked_ns(), u64::MAX, "saturates");
    }

    /// NEGATIVE CONTROL: a whole, checksummed line that names another row count is refused by
    /// its count, never read with its rows shifted; so is one whose flush counts count other
    /// flushes than this build's.
    #[test]
    fn a_line_of_another_row_count_or_flush_word_is_refused_by_its_head() {
        let line = sample().render();
        let (body, _) = line.trim_end().rsplit_once(" sum=").expect("a sum");
        let resummed = |body: String| format!("{body} sum={:016x}\n", checksum(body.as_bytes()));
        assert_eq!(
            Tally::parse(&resummed(body.to_string())),
            Ok(sample()),
            "the control: re-summed as it was, it reads"
        );
        let fewer = body.replacen("rows=031", "rows=025", 1);
        let why = Tally::parse(&resummed(fewer)).expect_err("another row count");
        assert!(
            why.contains("of 25 rows") && why.contains("reads 31"),
            "{why}"
        );
        let other = if FLUSHES_COUNTED == "all" {
            "barrier"
        } else {
            "all"
        };
        let word = body.replacen(
            &format!(" counted={FLUSHES_COUNTED} "),
            &format!(" counted={other} "),
            1,
        );
        let why = Tally::parse(&resummed(word)).expect_err("another flush word");
        assert!(why.contains(&format!("count `{other}`")), "{why}");
        // Times on another build's clock do not line up with this build's: refused by the word.
        let other = match FLUSH_CLOCK {
            astream_sys::FlushClock::Machine => astream_sys::FlushClock::Process,
            astream_sys::FlushClock::Process => astream_sys::FlushClock::Machine,
        };
        let clock = body.replacen(
            &format!(" clock={} ", FLUSH_CLOCK.word()),
            &format!(" clock={} ", other.word()),
            1,
        );
        let why = Tally::parse(&resummed(clock)).expect_err("another clock");
        assert!(
            why.contains(&format!("the `{}` clock", other.word())),
            "{why}"
        );
    }

    /// NEGATIVE CONTROL: a checksummed line whose flush ring or slow-call ring is no writer's is
    /// refused, never read as flushes or calls: a flush place of no kind, an empty place with a
    /// span, a flush after an empty place, one that ends before it starts, a short or a long
    /// ring; a call of a row that is no call's or of none, one under 1.05 ms, a call after an
    /// empty place, one that ends before the call before it, a start that is not 16 hex digits,
    /// a short or a long ring, and a count the kept calls do not match.
    #[test]
    fn a_flush_ring_or_a_call_ring_no_writer_wrote_is_refused() {
        let line = sample().render();
        let (body, _) = line.trim_end().rsplit_once(" sum=").expect("a sum");
        let resummed = |body: String| format!("{body} sum={:016x}\n", checksum(body.as_bytes()));
        let (head, ring) = body.rsplit_once(" ring=").expect("a ring");
        let places: Vec<&str> = ring.split('.').collect();
        assert_eq!(places.len(), RING);
        let with = |places: &[String]| resummed(format!("{head} ring={}", places.join(".")));
        let owned: Vec<String> = places.iter().map(|p| (*p).to_string()).collect();
        assert_eq!(Tally::parse(&with(&owned)), Ok(sample()), "the control");
        let mut each: Vec<(String, Vec<String>)> = Vec::new();
        let mut put = |why: &str, at: usize, place: String| {
            let mut p = owned.clone();
            p[at] = place;
            each.push((why.to_string(), p));
        };
        put("a place of no kind", 0, owned[0].replacen('f', "x", 1));
        put(
            "an empty place with a span",
            2,
            format!("-{:016x}@{:016x}-{:016x}", 0, 1, 2),
        );
        put("a flush after an empty place", 3, owned[0].clone());
        put(
            "an end before its start",
            0,
            format!("f{:016x}@{:016x}-{:016x}", 0, 9, 8),
        );
        put("a place with no end", 0, format!("f{:016x}@{:016x}", 0, 9));
        put(
            "a place with no index (a v=6 line's)",
            0,
            format!("f{:016x}-{:016x}", 1, 2),
        );
        let second = |index: u64| {
            format!(
                "b{index:016x}@{:016x}-{:016x}",
                7_100_000_000u64, 7_101_500_000u64
            )
        };
        assert_eq!(second(1), owned[1], "the control's second place");
        put("an index not past the one before it", 1, second(0));
        put("an index at the line's slow count", 1, second(2));
        let mut short = owned.clone();
        short.pop();
        each.push(("a short ring".to_string(), short));
        let mut long = owned.clone();
        long.push(owned[RING - 1].clone());
        each.push(("a long ring".to_string(), long));
        for (why, places) in each {
            assert!(Tally::parse(&with(&places)).is_err(), "{why}");
        }

        let (before, rest) = body.split_once(" slow_calls=").expect("the slow calls");
        let (count, rest) = rest.split_once(" calls=").expect("the call ring");
        let (calls, after) = rest.split_once(" ring=").expect("the flush ring");
        let places: Vec<String> = calls.split('.').map(str::to_string).collect();
        assert_eq!((count, places.len()), ("00000000000000000003", CALLS_KEPT));
        let with = |count: &str, places: &[String]| {
            resummed(format!(
                "{before} slow_calls={count} calls={} ring={after}",
                places.join(".")
            ))
        };
        assert_eq!(
            Tally::parse(&with(count, &places)),
            Ok(sample()),
            "the control"
        );
        let place = |row: &str, start: u64, ns: u64| format!("{row}{start:016x}+{ns:016x}");
        let mut each: Vec<(&str, Vec<String>)> = Vec::new();
        let mut put = |why: &'static str, at: usize, p: String| {
            let mut all = places.clone();
            all[at] = p;
            each.push((why, all));
        };
        let park = format!("{:02}", Row::Park.index());
        put(
            "the park, no call",
            2,
            place(&park, 7_100_000_000, 40_000_000),
        );
        put(
            "a row past the last",
            2,
            place(&format!("{ROWS:02}"), 7_100_000_000, 40_000_000),
        );
        put(
            "a row not in digits",
            2,
            place("0x", 7_100_000_000, 40_000_000),
        );
        put(
            "a call under 1.05 ms",
            2,
            place("07", 7_100_000_000, (1 << 20) - 1),
        );
        put(
            "a call that ends before the one before it",
            2,
            place("07", 7_000_000_000, 20_000_000),
        );
        put("a call after an empty place", 4, places[2].clone());
        put("an empty place with a span", 3, place("--", 0, 1));
        put(
            "a start not in hex",
            2,
            format!("07{}+{:016x}", "00000001a2b3c4dz", 40_000_000),
        );
        put("a place with no length", 2, places[2].replace('+', ""));
        let mut short = places.clone();
        short.pop();
        each.push(("a short ring", short));
        let mut long = places.clone();
        long.push(places[CALLS_KEPT - 1].clone());
        each.push(("a long ring", long));
        for (why, all) in each {
            assert!(Tally::parse(&with(count, &all)).is_err(), "{why}");
        }
        for other in ["00000000000000000002", "00000000000000000004"] {
            assert!(
                Tally::parse(&with(other, &places)).is_err(),
                "a count of {other} with three kept"
            );
        }
    }

    /// NEGATIVE CONTROL: a line whose checksum was corrupted is refused, and so
    /// is every mix of two lines a read racing the write could see — never
    /// read as a count that is half old and half new.
    #[test]
    fn a_line_that_raced_its_write_or_lost_its_checksum_is_refused_never_read_as_a_count() {
        let old = sample().render();
        let mut later = sample();
        later.committed = 121;
        later.rows[Row::LaneDeliver.index()].add(300_000, Unit::Ns);
        later.rows[Row::Park.index()].add(1_000_000, Unit::Ns);
        let new = later.render();
        assert_eq!(old.len(), new.len());
        for cut in 1..old.len() {
            let torn = format!("{}{}", &new[..cut], &old[cut..]);
            if torn != old && torn != new {
                assert!(Tally::parse(&torn).is_err(), "cut {cut}");
            }
        }
        assert!(Tally::parse(&old).is_ok() && Tally::parse(&new).is_ok());
        let (body, sum) = old.trim_end().rsplit_once(" sum=").expect("a sum");
        let flipped: String = sum
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i == 0 {
                    if c == '0' {
                        '1'
                    } else {
                        '0'
                    }
                } else {
                    c
                }
            })
            .collect();
        let corrupt = format!("{body} sum={flipped}\n");
        assert!(
            Tally::parse(&corrupt)
                .expect_err("a corrupted checksum")
                .contains("checksum"),
            "{corrupt}"
        );
        for bad in [
            "",
            "wait-tally v=8\n",
            &old[..old.len() - 1],
            &old.replace("wait-tally v=8", "wait-tally v=7"),
            &old.replace(" sum=", " sum=0"),
        ] {
            assert!(Tally::parse(bad).is_err(), "{bad:?}");
        }
        for body in ["wait-tally v=8 rows=031 pid=1 requests=1".to_string(), {
            let (body, _) = old.trim_end().rsplit_once(" sum=").expect("a sum");
            body.rsplit_once('.').expect("a band").0.to_string()
        }] {
            let short = format!("{body} sum={:016x}\n", checksum(body.as_bytes()));
            assert!(Tally::parse(&short).is_err(), "{short}");
        }
    }

    #[test]
    fn the_difference_of_two_tallies_is_one_processs_and_never_negative() {
        let earlier = sample();
        let mut later = earlier;
        later.committed = 200;
        later.at_ns += 50_000_000;
        later.rows[Row::LaneDeliver.index()].add(100_000, Unit::Ns);
        later.rows[Row::Ack.index()].add(90_000_000, Unit::Ns);
        later.flushes.barrier += 6;
        let d = later.since(&earlier).expect("one process");
        assert_eq!((d.pid, d.committed, d.at_ns), (4242, 200, 50_000_000));
        assert_eq!(
            d.flushes,
            astream_sys::FlushTally {
                full: 0,
                barrier: 6,
                plain: 0,
            },
            "the window's flushes"
        );
        assert_eq!(
            *d.row(Row::LaneDeliver),
            Hist {
                max: 0,
                ..hist(&[100_000], Unit::Ns)
            },
            "the window's one delivery, below the 40 ms before it: its largest not known"
        );
        assert_eq!(
            d.row(Row::Ack).max,
            90_000_000,
            "a new largest is the window's"
        );
        assert_eq!(d.row(Row::Request), &Hist::default(), "nothing new");
        let mut quieter = later;
        quieter.rows[Row::LaneDeliver.index()].add(130_000, Unit::Ns);
        let q = quieter.since(&later).expect("one process");
        assert_eq!(q.row(Row::LaneDeliver).max, 0);
        assert_eq!(
            q.row(Row::LaneDeliver).top(Unit::Ns),
            Some((131_072 / 2, Some(131_072)))
        );
        let relaunched = Tally { pid: 4243, ..later };
        assert!(relaunched
            .since(&earlier)
            .unwrap_err()
            .contains("relaunched"));
        let mut backwards = later;
        backwards.rows[Row::Ack.index()].count = 0;
        assert!(backwards.since(&earlier).unwrap_err().contains("`ack`"));
        let clock_back = Tally { at_ns: 1, ..later };
        assert!(clock_back.since(&earlier).unwrap_err().contains("clock"));
        let mut flushes_back = later;
        flushes_back.flushes.full = 0;
        assert!(flushes_back
            .since(&earlier)
            .unwrap_err()
            .contains("`full` flushes"));

        // EVERY SLOW CALL THE WINDOW MADE IS THE WINDOW'S, the slowest of its row or not: the
        // 90 ms ack above and a 2 ms delivery under the 40 ms one before the window are both
        // in the difference, and none of the calls the earlier write kept is. The flush times
        // subtract, the slow count with them, and the flush ring is the later write's whole.
        later.calls.push(call(Row::Ack, 7_400_000_000, 90_000_000));
        later.rows[Row::LaneDeliver.index()].add(2_000_000, Unit::Ns);
        later
            .calls
            .push(call(Row::LaneDeliver, 7_600_000_000, 2_000_000));
        later.times.full_ns += 9_000_000;
        later.times.slow_count += 1;
        later.times.running = 2;
        later.times.slow[2] = Some(slow(
            2,
            astream_sys::FlushKind::Full,
            7_500_000_100,
            7_509_000_000,
        ));
        let d = later.since(&earlier).expect("one process");
        assert_eq!(
            d.row(Row::LaneDeliver).max,
            0,
            "its largest not the window's"
        );
        assert_eq!(
            (d.calls.count, d.calls.iter().collect::<Vec<_>>()),
            (
                2,
                vec![
                    call(Row::Ack, 7_400_000_000, 90_000_000),
                    call(Row::LaneDeliver, 7_600_000_000, 2_000_000)
                ]
            ),
            "the window's slow calls, the one under its row's earlier largest included"
        );
        assert_eq!(
            d.calls.count,
            Row::ALL
                .iter()
                .filter(|r| r.is_call())
                .map(|&r| d.row(r).at_least(SLOW_CALL_BAND))
                .sum::<u64>(),
            "as many as its call rows counted from 1.05 ms up"
        );
        assert_eq!(
            (d.times.full_ns, d.times.barrier_ns, d.times.slow_count),
            (9_000_000, 0, 1)
        );
        assert_eq!(d.times.slow, later.times.slow, "the later ring, whole");
        assert_eq!(
            d.times.running, 2,
            "the later write's calls running, not a difference"
        );
        assert!(
            later
                .since(&later)
                .is_ok_and(|d| d.calls.count == 0 && d.calls.is_empty()),
            "no window, no calls"
        );
        let mut times_back = later;
        times_back.times.full_ns = 0;
        assert!(times_back
            .since(&earlier)
            .unwrap_err()
            .contains("full flush time"));
        let calls_back = Tally {
            calls: SlowCalls::default(),
            ..later
        };
        assert!(calls_back
            .since(&earlier)
            .unwrap_err()
            .contains("slow call count"));

        // A WINDOW OF MORE SLOW CALLS THAN A LINE KEEPS counts them all and keeps the last ones.
        let mut busy = earlier;
        for i in 0..CALLS_KEPT as u64 + 2 {
            busy.calls.push(call(
                Row::DirAppend,
                8_000_000_000 + i * 3_000_000,
                2_000_000,
            ));
        }
        let d = busy.since(&earlier).expect("one process");
        assert_eq!(
            (d.calls.count, d.calls.len()),
            (CALLS_KEPT as u64 + 2, CALLS_KEPT)
        );
        assert_eq!(
            d.calls.iter().next().map(|c| c.start_ns),
            Some(8_000_000_000 + 2 * 3_000_000),
            "the window's first two left the ring"
        );
    }

    /// TWO SOURCES MERGE BY END: the recorder's own calls and the state dir's, each in the order
    /// it ended, merge into one ring in end order, the recorder's first on a tie, both counts
    /// summed, and a merge past the ring keeps the last [`CALLS_KEPT`] of both.
    #[test]
    fn two_sources_of_slow_calls_merge_by_their_ends() {
        let of = |calls: &[SlowCall]| {
            let mut out = SlowCalls::default();
            for &c in calls {
                out.push(c);
            }
            out
        };
        let own = of(&[
            call(Row::Ack, 100, 2_000_000),
            call(Row::Commit, 100, 2_000_000),
            call(Row::Request, 5_000_000, 3_000_000),
        ]);
        let dir = of(&[
            call(Row::DirAppend, 0, 2_000_100),
            call(Row::LogpinFence, 1_000_000, 1_100_000),
        ]);
        let m = own.merged(&dir);
        assert_eq!(m.count, 5);
        assert_eq!(
            m.iter().map(|c| c.row).collect::<Vec<_>>(),
            [
                Row::Ack,
                Row::Commit,
                Row::DirAppend,
                Row::LogpinFence,
                Row::Request
            ],
            "{m:?}"
        );
        assert_eq!(dir.merged(&own).count, 5);
        let many = of(&(0..CALLS_KEPT as u64)
            .map(|i| call(Row::DirAppend, i * 10_000_000, 2_000_000))
            .collect::<Vec<_>>());
        let m = many.merged(&own);
        assert_eq!((m.count, m.len()), (CALLS_KEPT as u64 + 3, CALLS_KEPT));
        assert_eq!(
            m.iter().last(),
            many.iter().last(),
            "the last to end is the last kept"
        );
        assert!(
            m.iter().all(|c| c.row != Row::Ack),
            "the earliest ends left the ring"
        );
    }

    /// EVERY SLOW CALL IS KEPT WITH ITS START ON THE FLUSH CLOCK: a call of 1.05 ms or more is
    /// kept, its start inside the clock readings taken around it less its length, whether or
    /// not it is its row's slowest; a faster one, the park and a count row are not. An ack that
    /// commits is one span in two rows, with one start. The write carries the recorder's calls
    /// merged with the state dir's and the process's flush times, a flush ring that holds every
    /// slow flush this test made (read after the write) among its last ones, or below the
    /// oldest index it still holds. Off Apple the flush clock starts at the process's first
    /// reading, so a start before it saturates at 0, as the recorder's does.
    #[test]
    fn every_slow_call_is_kept_with_its_start_on_the_flush_clock() {
        let dir = crate::test_scratch_dir("tally");
        let path = dir.path().join(FILE);
        let mut on = Recorder::at(&path).expect("open");
        let slow_ns = Hist::floor(SLOW_CALL_BAND, Unit::Ns);
        assert_eq!(slow_ns, 1 << 20, "1.05 ms");
        let before = astream_sys::clock_ns();
        on.span(Row::LaneSent, 3 * slow_ns);
        let after = astream_sys::clock_ns();
        let first = on.calls.iter().next().expect("kept");
        assert_eq!((first.row, first.ns), (Row::LaneSent, 3 * slow_ns));
        assert!(
            (before.saturating_sub(3 * slow_ns)..=after.saturating_sub(3 * slow_ns))
                .contains(&first.start_ns),
            "{first:?} does not start its length before a reading in {before}..={after}"
        );
        on.span(Row::LaneSent, slow_ns);
        on.span(Row::LaneSent, slow_ns - 1);
        let ago = |ms| Instant::now().checked_sub(Duration::from_millis(ms));
        on.note(Row::Park, ago(5));
        on.count(Row::DrainPosts, 3);
        assert_eq!(
            on.calls.iter().map(|c| (c.row, c.ns)).collect::<Vec<_>>(),
            [(Row::LaneSent, 3 * slow_ns), (Row::LaneSent, slow_ns)],
            "the second, under the row's largest, kept; the faster one, the park and the count not"
        );
        on.ack(ago(2), true);
        let (ack, commit) = (
            on.calls.kept[2].expect("the ack"),
            on.calls.kept[3].expect("the commit"),
        );
        assert_eq!(
            (ack.row, commit.row, ack.start_ns, ack.ns),
            (Row::Ack, Row::Commit, commit.start_ns, commit.ns),
            "one span"
        );
        let kept = Kept::default();
        kept.turn_on(true);
        let opened = astream_sys::clock_ns();
        kept.note(Row::AppendSelfAcked, 2 * slow_ns);
        let closed = astream_sys::clock_ns();
        kept.note(Row::DirAppend, 5_000);
        on.idle(3, |t| kept.copy_into(t));
        let written = Tally::parse(&std::fs::read_to_string(&path).expect("read")).expect("a line");
        assert_eq!(written.calls.count, 5);
        let dir_call = written
            .calls
            .iter()
            .find(|c| c.row == Row::AppendSelfAcked)
            .expect("the state dir's slow call");
        assert!(
            (opened.saturating_sub(2 * slow_ns)..=closed.saturating_sub(2 * slow_ns))
                .contains(&dir_call.start_ns),
            "the kept row's start: {written:?}"
        );
        assert_eq!(written.calls.iter().last(), Some(dir_call), "it ended last");
        let times = astream_sys::flush_times();
        assert!(
            written.times.full_ns <= times.full_ns && written.times.slow_count <= times.slow_count
        );
        // The ring keeps the last flushes by index, which each takes as it returns: one that
        // left it has an index below the oldest kept, whenever it began.
        assert!(
            written.times.slow().all(|f| times.slow.contains(&f)
                || times.slow.first().is_some_and(|g| f.index < g.index)),
            "the written ring is the process's: {written:?} against {times:?}"
        );
    }

    /// THE PARTITION: the rows the loop is in one at a time, and only those,
    /// sum to its accounted time; a commit (an ack), a call row (a replace), a
    /// wake (inside a park) and a post's path are left out of it.
    #[test]
    fn only_the_disjoint_rows_partition_the_loops_time() {
        let t = sample();
        assert_eq!(
            t.partitioned_ns(),
            31_000_000 + 18_009_000 + 40_370_000 + 200_000 + 500_000_000
        );
        for row in [
            Row::Commit,
            Row::SetPostPin,
            Row::WakeEvent,
            Row::QueuedRecord,
            Row::PostPublish,
            Row::DrainPosts,
        ] {
            assert!(!row.partitions(), "{row:?}");
        }
        for row in [
            Row::LaneStatus,
            Row::LaneMeta,
            Row::LaneText,
            Row::LaneSessionsStatus,
        ] {
            assert!(row.partitions(), "a verb-lane round trip: {row:?}");
        }
        for (i, row) in Row::ALL.iter().enumerate() {
            assert_eq!(row.index(), i);
        }
    }

    /// Each write's head carries the process's astream-sys flush counts as of that write. A
    /// flush alone is no move (any thread's flush moves the process's counts): NEGATIVE
    /// CONTROL, the idle moment after a barrier with nothing else moved writes nothing, and the
    /// next move's write carries the barrier.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn each_write_carries_the_processs_flushes_and_a_flush_alone_writes_nothing() {
        use std::io::Write as _;
        let dir = crate::test_scratch_dir("tally");
        let path = dir.path().join(FILE);
        let read = || std::fs::read_to_string(&path).expect("read the tally");
        let mut on = Recorder::at(&path).expect("open");
        let before = astream_sys::flush_tally();
        on.idle(5, |_| {});
        let first = Tally::parse(&read()).expect("a whole line");
        assert!(first.flushes.barrier >= before.barrier && first.flushes.full >= before.full);
        std::fs::write(&path, "sentinel").expect("overwrite");
        let mut file = std::fs::File::create(dir.path().join("f")).expect("a scratch file");
        file.write_all(b"ordered").expect("its bytes");
        let at_least = astream_sys::flush_tally().barrier + 1;
        astream_sys::barrier_sync(&file).expect("this volume takes the barrier");
        on.idle(5, |_| {});
        assert_eq!(read(), "sentinel", "a flush alone writes nothing");
        on.count(Row::DrainPosts, 1);
        on.idle(5, |_| {});
        let again = Tally::parse(&read()).expect("rewritten for the move");
        assert!(again.flushes.barrier >= at_least, "{again:?}");
        assert_eq!(
            again.since(&first).map(|d| d.row(Row::DrainPosts).count),
            Ok(1)
        );
    }

    /// Each verb the bridge's lane sends is counted by its head, a session's verb by the verb
    /// after its address; the presence reads have rows of their own. CONTROL: a verb no arm
    /// names, or a head that only begins like one, is still every other verb's.
    #[test]
    fn a_verb_lane_request_is_counted_by_its_head() {
        for (line, row) in [
            ("outbox earlier-receipts=1", Row::LaneOutbox),
            ("outbox", Row::LaneOutbox),
            ("outbox sent s-a 4 off=17", Row::LaneSent),
            ("outbox sent s-a 4 off=- reason=refused", Row::LaneSent),
            ("sessions bridge", Row::LaneSessions),
            ("deliver s-b receipt=3 off=9", Row::LaneDeliver),
            ("deliver s-b fetched=9 err=refused", Row::LaneDeliver),
            ("@s-a status", Row::LaneStatus),
            ("@s-a meta", Row::LaneMeta),
            ("@s-a text --json", Row::LaneText),
            ("@s-a text", Row::LaneText),
            ("sessions status", Row::LaneSessionsStatus),
            ("@s-a topic ls", Row::LaneOther),
            ("@s-a statusline", Row::LaneOther),
            ("@s-a", Row::LaneOther),
            ("@s-a  status", Row::LaneOther),
            ("status", Row::LaneOther),
            ("text --json", Row::LaneOther),
            ("sessions statuses", Row::LaneOther),
            ("sessions", Row::LaneOther),
            ("outboxes", Row::LaneOther),
            ("link mirror on", Row::LaneOther),
            ("delivered", Row::LaneOther),
        ] {
            assert_eq!(lane_row(line), row, "{line}");
        }
    }

    #[test]
    fn an_off_recorder_counts_nothing_and_an_on_one_writes_only_what_moved() {
        let mut off = Recorder::default();
        off.request(Row::Request, Instant::now());
        let started = off.ack_starts();
        assert_eq!(started, None);
        assert_eq!(off.starts(), None);
        off.ack(started, true);
        off.note(Row::LaneDeliver, off.starts());
        off.count(Row::DrainPosts, 3);
        off.idle(9, |_| panic!("an off recorder copies nothing"));
        assert_eq!(off.tally(), Tally::default());

        let dir = crate::test_scratch_dir("tally");
        let path = dir.path().join(FILE);
        let mut on = Recorder::at(&path).expect("open");
        assert!(on.on());
        let read = || std::fs::read_to_string(&path).expect("read the tally");
        assert_eq!(read(), "", "nothing until the first idle moment");
        on.request(Row::Request, Instant::now());
        on.request(Row::Request, Instant::now());
        on.request(Row::BudgetRequest, Instant::now());
        let started = on.ack_starts();
        assert!(started.is_some());
        on.ack(started, false);
        let started = on.ack_starts();
        on.ack(started, true);
        let started = on.starts();
        on.note(Row::LaneSent, started);
        on.count(Row::DrainPosts, 2);
        let kept = Kept::default();
        drop(kept.timer(Row::DirUnlink));
        kept.turn_on(true);
        drop(kept.timer(Row::DirReplace));
        on.idle(17, |t| kept.copy_into(t));
        let written = Tally::parse(&read()).expect("a whole line");
        let counts = |t: &Tally, r: Row| t.row(r).count;
        assert_eq!(
            (
                written.pid,
                counts(&written, Row::Request),
                counts(&written, Row::BudgetRequest),
                counts(&written, Row::Commit),
                counts(&written, Row::Ack),
                counts(&written, Row::LaneSent),
                written.row(Row::DrainPosts).total,
            ),
            (std::process::id(), 2, 1, 1, 2, 1, 2),
            "a request is counted in the row it names, a budget read apart from the rest"
        );
        assert_eq!(
            (
                counts(&written, Row::DirReplace),
                counts(&written, Row::DirUnlink)
            ),
            (1, 0),
            "a kept row counts only once turned on"
        );
        assert_eq!(written.committed, 17);
        assert!(written.dir, "a test build counts the dir");
        assert!(written.row(Row::Commit).total <= written.row(Row::Ack).total);
        assert_eq!(
            counts(&written, Row::TallyWrite),
            0,
            "a write counts itself in the next line"
        );

        std::fs::write(&path, "sentinel").expect("overwrite");
        on.idle(17, |t| kept.copy_into(t));
        assert_eq!(read(), "sentinel");
        on.idle(18, |t| kept.copy_into(t));
        let again = Tally::parse(&read()).expect("rewritten whole");
        assert_eq!(again.committed, 18);
        assert_eq!(
            again.render(),
            read(),
            "the in-place line is the rendered one"
        );
        assert_eq!(again.row(Row::TallyWrite).count, 1);
        let d = again.since(&written).expect("one process");
        assert_eq!(d.row(Row::Request).count, 0);
        assert_eq!(d.row(Row::TallyWrite).count, 1, "the first write, counted");
        assert!(d.at_ns > 0, "the clock moved");
    }

    /// Occupied diagnostic names never authorize truncating their bytes or a
    /// link target.
    #[cfg(unix)]
    #[test]
    fn a_wait_tally_never_truncates_an_occupied_name_or_its_link_target() {
        let dir = crate::test_scratch_dir("tally");
        let fresh = dir.path().join("fresh-tally");
        let mut recorder = Recorder::at(&fresh).expect("fresh recorder");
        recorder.idle(7, |_| {});
        drop(recorder);
        assert_eq!(
            Tally::parse(&std::fs::read_to_string(&fresh).expect("fresh bytes"))
                .expect("fresh line")
                .committed,
            7
        );

        let occupied = dir.path().join("occupied");
        let unrelated = dir.path().join("unrelated");
        let linked = dir.path().join("linked-tally");
        std::fs::write(&occupied, b"previous diagnostic bytes").expect("owned occupied name");
        std::fs::write(&unrelated, b"unrelated owned sentinel").expect("owned sentinel");
        std::os::unix::fs::symlink(&unrelated, &linked).expect("owned link");
        let occupied_refused = Recorder::at(&occupied).is_err();
        let linked_refused = Recorder::at(&linked).is_err();
        assert!(
            occupied_refused
                && linked_refused
                && std::fs::read(&occupied).is_ok_and(|b| b == b"previous diagnostic bytes")
                && std::fs::read(&unrelated).is_ok_and(|b| b == b"unrelated owned sentinel")
                && std::fs::symlink_metadata(&linked).is_ok_and(|m| m.file_type().is_symlink()),
            "WAIT_TALLY_OCCUPIED_NAME_WAS_CLOBBERED"
        );
    }
}
