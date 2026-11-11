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
#[path = "wait_tally_tests.rs"]
mod tests;
