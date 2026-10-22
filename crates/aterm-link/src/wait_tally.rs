// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WAIT TALLY: where the bridge's one loop spends its time, counted where
//! it spends it — a development seam the fabric bench's ping-pong arm reads
//! (`crates/aterm/tests/fabric_bench/run/pingpong.rs`).
//!
//! WHY IT IS COUNTED IN HERE. The bridge is one thread. While it waits for a
//! broker's answer, for aterm's answer on the verb lane, or for a state-dir
//! write, it does nothing else. From outside, a bench sees only the round
//! trip; how much of it is the loop, and which of its steps, is a fact only
//! this process has. So the loop counts it.
//!
//! VERSION 2 (`wait-tally v=2`, B10) counts every kind of step the loop takes,
//! not only its waits on the broker (`v=1`, which left 60-65% of an ordered
//! round trip unattributed). Each [`Row`] is a [`Hist`]: a count, a total, the
//! largest, and the count in each log2 band, because a sum cannot answer a
//! tail question (was the one 50 ms round a state-dir replace or an ack?).
//!
//! - THE BROKER ([`Row::Request`], [`Row::Ack`], [`Row::Commit`]): every
//!   request that reads its own reply — a `Fetch`, a `Last` page, the `Will`, a
//!   `Budget`, a blocking `Publish` — from the moment it was written to the
//!   moment its reply was read (`Bridge::observe`); every pipelined ack read,
//!   waited for or not, around the read alone (`Bridge::settled`), and the
//!   inbox group's commits among them again apart. (A `Last` walk the bridge
//!   gives up on for running past its bound hands `observe` an error of its own
//!   making: one more request, and no wait.)
//! - THE VERB LANE ([`Row::LaneOutbox`] to [`Row::LaneOther`]): every round
//!   trip to aterm, timed in `Bridge::ctl_request` and keyed by its verb head
//!   ([`lane_row`]): the outbox peek, the roster read a drain makes (`sessions
//!   bridge`), a post's retirement (`outbox sent`), a delivery (`deliver`),
//!   and everything else.
//! - THE STATE DIR ([`Row::DirReplace`] to [`Row::DirFlush`], by the kind of
//!   write; [`Row::SetPostPin`] to [`Row::AppendSelfAcked`], by the call that
//!   made it): counted by the [`crate::state::StateDir`] itself ([`Kept`]),
//!   at its disk door, in a build with debug assertions (the bench's two
//!   builds both have them; [`Tally::dir`] says whether a line's dir rows
//!   were counted). The kind rows split the dir's time; the call rows say
//!   whose it was, and overlap them.
//! - THE PUSH LANES' WAKE ([`Row::WakeEvent`] to [`Row::QueuedRecord`]):
//!   every item the push reader (aterm's `EVENT` lines) or a broker reader
//!   queues in the [`crate::mailbox::Mailbox`] is stamped, and the time until
//!   the loop takes it is counted — apart for an item that arrived while the
//!   loop was parked on the mailbox (its wake: the scheduler's) and for one
//!   that arrived while the loop was working (its wait behind that work).
//! - A POST'S PATH ([`Row::PostPublish`], [`Row::DrainPosts`]): from taking a
//!   `post` event to each fresh post's publish written on the broker socket,
//!   and the posts each outbox peek listed.
//! - THE LOOP ITSELF ([`Row::Park`], [`Row::TallyWrite`]): the time parked on
//!   the mailbox, and this tally's own writes. With [`Tally::at_ns`], the
//!   bridge's own clock at each write, two tallies say how long the loop ran,
//!   how long it was parked, and what took the rest ([`Row::partitions`]): the
//!   rows that partition the loop's time are disjoint. What they leave is
//!   unaccounted wall time, including code between measured calls, scheduling
//!   delays and instrumentation overhead. No CPU clock is sampled.
//!
//! Every clock read the seam makes is here (an `Instant` is read only while it
//! is on), so the census guard that holds the bridge's files to their door
//! (`aterm-census`'s `bridge-fx`) sees none in them.
//!
//! HOW IT LEAVES THE PROCESS. With [`SEAM`] set in a build that reads seams,
//! the bridge opens `<state>/`[`FILE`] at its start and, at each idle moment —
//! its loop found nothing queued and has read its posts' answers (waiting, as
//! their senders wait) and the other acks that had arrived
//! (`Bridge::settle_owed`), and again once its idle backstop has read the rest
//! (`Bridge::park_and_take`) — when the tally has moved since the last write,
//! writes the tally there ([`Recorder::idle`]): one positioned write of one
//! line of constant length at offset 0 — no temp, no rename, no flush. The line
//! carries a checksum of itself ([`Tally::parse`]), so a reader that raced a
//! write reads a line that does not parse, never a count that is half old and
//! half new.
//!
//! The line also names this process's pid, and the inbox group cursor the loop
//! last committed — once the commit's ack was read: a reader waits for a tally
//! that has committed past the message it just saw (the loop's idle write
//! after that commit landed), and refuses a pair of tallies from two processes
//! ([`Tally::since`]) — a relaunched bridge starts again from zero.
//!
//! WHAT IT COSTS THE LOOP IT MEASURES. The loop the arm measures is NOT the
//! loop that runs without the seam: it is that loop plus this, and its own
//! row says how much ([`Row::TallyWrite`]: rendering the rows that moved,
//! hashing the line and one `pwrite`, at each idle moment something moved).
//! Elsewhere it is two clock reads and a few additions per step counted, a
//! lock per state-dir write and one per item taken, and a clock read per item
//! queued. An item that arrives while the write runs waits for it.
//!
//! A RELEASED BINARY HAS NO TALLY. `dev_seam!` is `None` there, the recorder is
//! off, and every note below is one branch on that; the state dir's counting
//! is not compiled at all without debug assertions.

use std::io;
use std::path::Path;
#[cfg(any(test, debug_assertions))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(test, debug_assertions))]
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The development seam that turns the tally on: any value, in a build that
/// reads seams (a debug build, or `--features dev-seams`). Inherited by the
/// bridge from the aterm that launches it (`aterm-gui`'s `fabric_launch.rs`).
pub const SEAM: &str = "ATERM_LINK_WAIT_TALLY";

/// The file in the bridge's state dir the tally is written to.
pub const FILE: &str = "wait-tally";

/// What the line starts with: the format's name and version.
const HEAD: &str = "wait-tally v=2";

/// The log2 bands each row counts in ([`Hist::buckets`]).
pub const BUCKETS: usize = 18;

/// What a row's values are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Nanoseconds of `Instant`, the monotonic clock. Band 0 is under 1024 ns;
    /// band `i` from 1 holds `[2^(i+9), 2^(i+10))` ns; the last, from 2^26 ns
    /// (67.1 ms) up.
    Ns,
    /// A count. Band 0 is 0; band `i` from 1 holds `[2^(i-1), 2^i)`; the last,
    /// from 65536 up.
    Count,
}

impl Unit {
    /// How far a value is shifted before its band is its bit length.
    const fn shift(self) -> u32 {
        match self {
            Unit::Ns => 10,
            Unit::Count => 0,
        }
    }
}

/// One thing the loop does, counted ([`Hist`]). The order is the line's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Row {
    /// Requests that read their own reply: the write to the reply read.
    Request,
    /// Pipelined acks read, waited for or not: the read alone.
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
    /// Every other verb-lane round trip.
    LaneOther,
    /// State-dir atomic replaces: temp written (and flushed, when asked), renamed.
    DirReplace,
    /// State-dir unlinks.
    DirUnlink,
    /// State-dir appends.
    DirAppend,
    /// State-dir flushes of a file in place or of a directory.
    DirFlush,
    /// `StateDir::set_post_pin`: a post's reservation (a replace).
    SetPostPin,
    /// `StateDir::set_in_flight`: a self-lane post's journal entry (a replace).
    SetInFlight,
    /// `StateDir::clear_in_flight`: its removal once it landed (an unlink).
    ClearInFlight,
    /// `StateDir::clear_post_seq`: a retired post's reservation forgotten.
    ClearPostSeq,
    /// `StateDir::append_self_acked`: a landed self-lane offset appended.
    AppendSelfAcked,
    /// aterm's events queued while the loop was parked: the wake to the take.
    WakeEvent,
    /// Broker records queued while the loop was parked: the wake to the take.
    WakeRecord,
    /// aterm's events queued while the loop worked: the wait to the take.
    QueuedEvent,
    /// Broker records queued while the loop worked: the wait to the take.
    QueuedRecord,
    /// A `post` event taken to each fresh post's publish written.
    PostPublish,
    /// The posts each outbox peek listed (a count; 0 is an empty peek).
    DrainPosts,
    /// The loop parked on its mailbox.
    Park,
    /// This tally's own idle writes.
    TallyWrite,
}

/// How many rows a line has.
pub const ROWS: usize = 25;

impl Row {
    /// Every row, in the line's order.
    pub const ALL: [Row; ROWS] = [
        Row::Request,
        Row::Ack,
        Row::Commit,
        Row::LaneOutbox,
        Row::LaneSessions,
        Row::LaneSent,
        Row::LaneDeliver,
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
    pub fn index(self) -> usize {
        self as usize
    }

    /// Its name in the line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Row::Request => "request",
            Row::Ack => "ack",
            Row::Commit => "commit",
            Row::LaneOutbox => "lane_outbox",
            Row::LaneSessions => "lane_sessions",
            Row::LaneSent => "lane_sent",
            Row::LaneDeliver => "lane_deliver",
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

    /// What its values are.
    #[must_use]
    pub fn unit(self) -> Unit {
        match self {
            Row::DrainPosts => Unit::Count,
            _ => Unit::Ns,
        }
    }

    /// Whether the row is one of the disjoint spans that PARTITION the loop's
    /// time: the loop is in at most one of them at a time, so with the
    /// loop's wall time ([`Tally::at_ns`]) what they leave is unaccounted wall
    /// time, including scheduling delays. The others overlap them: the
    /// commits are acks or requests, a call row is a replace or an unlink,
    /// a wake falls inside a park and a
    /// queued item's wait inside the loop's work, and a post's path spans a
    /// peek, its writes and its send.
    #[must_use]
    pub fn partitions(self) -> bool {
        matches!(
            self,
            Row::Request
                | Row::Ack
                | Row::LaneOutbox
                | Row::LaneSessions
                | Row::LaneSent
                | Row::LaneDeliver
                | Row::LaneOther
                | Row::DirReplace
                | Row::DirUnlink
                | Row::DirAppend
                | Row::DirFlush
                | Row::Park
                | Row::TallyWrite
        )
    }

    /// The row the state dir keeps ([`Kept`]): its kinds and its calls.
    #[must_use]
    pub fn kept_by_the_dir(self) -> bool {
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
        )
    }
}

/// THE ROW A VERB-LANE REQUEST IS COUNTED IN, by its verb head. Pure
/// (unit-tested).
#[must_use]
pub fn lane_row(line: &str) -> Row {
    if line.starts_with("outbox sent ") {
        Row::LaneSent
    } else if line == "outbox" || line.starts_with("outbox ") {
        Row::LaneOutbox
    } else if line == "sessions bridge" {
        Row::LaneSessions
    } else if line.starts_with("deliver ") {
        Row::LaneDeliver
    } else {
        Row::LaneOther
    }
}

/// One row's values since the process started: how many, their sum, the
/// largest, and how many in each log2 band ([`Unit`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hist {
    pub count: u64,
    pub total: u64,
    /// The largest value counted. In a difference ([`Hist::since`]): the
    /// largest this process saw, when the window saw it, and 0 when the window
    /// saw nothing larger than what came before it ([`Hist::top`] then bounds
    /// the window's largest).
    pub max: u64,
    pub buckets: [u32; BUCKETS],
}

impl Hist {
    /// The band `v` falls in. Pure (unit-tested).
    #[must_use]
    pub fn band(v: u64, unit: Unit) -> usize {
        let shifted = v >> unit.shift();
        let bits = (u64::BITS - shifted.leading_zeros()) as usize;
        bits.min(BUCKETS - 1)
    }

    /// The least value band `i` holds. Pure (unit-tested).
    #[must_use]
    pub fn floor(i: usize, unit: Unit) -> u64 {
        if i == 0 {
            0
        } else {
            (1u64 << (i - 1).min(63)) << unit.shift()
        }
    }

    /// Count `v`.
    pub fn add(&mut self, v: u64, unit: Unit) {
        self.count = self.count.saturating_add(1);
        self.total = self.total.saturating_add(v);
        self.max = self.max.max(v);
        let b = &mut self.buckets[Hist::band(v, unit)];
        *b = b.saturating_add(1);
    }

    /// How many values were at least band `i`'s floor.
    #[must_use]
    pub fn at_least(&self, i: usize) -> u64 {
        self.buckets
            .iter()
            .skip(i)
            .map(|&n| u64::from(n))
            .sum::<u64>()
    }

    /// The floor and (for every band but the last) the bound of the highest
    /// band holding a value: `None` for an empty row.
    #[must_use]
    pub fn top(&self, unit: Unit) -> Option<(u64, Option<u64>)> {
        let i = self.buckets.iter().rposition(|&n| n > 0)?;
        let bound = (i + 1 < BUCKETS).then(|| Hist::floor(i + 1, unit));
        Some((Hist::floor(i, unit), bound))
    }

    /// What was counted between `earlier` and this. Pure (unit-tested).
    ///
    /// # Errors
    ///
    /// A count, a total or a band that went backwards: not the same row of
    /// one process.
    pub fn since(&self, earlier: &Hist) -> Result<Hist, String> {
        let back = |what: &str, then: u64, now: u64| {
            format!("its `{what}` went backwards, {then} to {now}")
        };
        let mut buckets = [0u32; BUCKETS];
        for (i, slot) in buckets.iter_mut().enumerate() {
            *slot = self.buckets[i]
                .checked_sub(earlier.buckets[i])
                .ok_or_else(|| {
                    back(
                        &format!("band {i}"),
                        u64::from(earlier.buckets[i]),
                        u64::from(self.buckets[i]),
                    )
                })?;
        }
        Ok(Hist {
            count: self
                .count
                .checked_sub(earlier.count)
                .ok_or_else(|| back("count", earlier.count, self.count))?,
            total: self
                .total
                .checked_sub(earlier.total)
                .ok_or_else(|| back("total", earlier.total, self.total))?,
            max: if self.max > earlier.max { self.max } else { 0 },
            buckets,
        })
    }
}

/// The loop's time since this process started, and where its inbox group
/// cursor stood at the write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// The bridge process the counts are of.
    pub pid: u32,
    /// The inbox group cursor this process last committed (0 before any).
    pub committed: u64,
    /// The bridge's own monotonic clock at the write, in nanoseconds since its
    /// recorder started: two tallies' difference is the loop's wall time
    /// between them.
    pub at_ns: u64,
    /// Whether the state dir's rows were counted ([`Row::kept_by_the_dir`]):
    /// only in a build with debug assertions.
    pub dir: bool,
    /// Each [`Row`], at its [`Row::index`].
    pub rows: [Hist; ROWS],
}

/// Writes `v` as exactly `out.len()` lowercase hex digits.
fn put_hex(out: &mut [u8], mut v: u64) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(v & 0xf) as usize];
        v >>= 4;
    }
}

/// The text of one row: ` <name>=<count>,<total>,<max>,<band>.<band>…`, every
/// number fixed-width hex (16 digits, 8 for a band). Its length depends on
/// the row's name alone.
fn row_text(row: Row, h: &Hist) -> Vec<u8> {
    let mut out = vec![b'0'; row_len(row)];
    put_row(&mut out, row, h);
    out
}

/// A row's text's length: its name's, and 214.
fn row_len(row: Row) -> usize {
    1 + row.name().len() + 1 + 3 * 17 + BUCKETS * 9 - 1
}

/// [`row_text`] written into `out`, which is exactly [`row_len`] long: the
/// in-place rewrite of one row's place in the line ([`Recorder::idle`]).
fn put_row(out: &mut [u8], row: Row, h: &Hist) {
    let name = row.name().as_bytes();
    out[0] = b' ';
    out[1..=name.len()].copy_from_slice(name);
    let mut at = 1 + name.len();
    out[at] = b'=';
    at += 1;
    for (i, v) in [h.count, h.total, h.max].into_iter().enumerate() {
        if i > 0 {
            out[at] = b',';
            at += 1;
        }
        put_hex(&mut out[at..at + 16], v);
        at += 16;
    }
    out[at] = b',';
    at += 1;
    for (i, &n) in h.buckets.iter().enumerate() {
        if i > 0 {
            out[at] = b'.';
            at += 1;
        }
        put_hex(&mut out[at..at + 8], u64::from(n));
        at += 8;
    }
    debug_assert_eq!(at, out.len());
}

/// The head: the format, the pid, the cursor, the clock and the dir flag,
/// zero-padded decimal so its length is fixed.
fn head_text(t: &Tally) -> String {
    format!(
        "{HEAD} pid={:010} committed={:020} at={:020} dir={}",
        t.pid,
        t.committed,
        t.at_ns,
        u8::from(t.dir)
    )
}

/// The ` sum=<16 hex>\n` tail of a line whose body is `body`.
fn sum_text(body: &[u8]) -> String {
    format!(" sum={:016x}\n", checksum(body))
}

/// Every line's length ([`Tally::render`]): the head's ([`head_text`]: the
/// format, ten digits of pid, twenty of cursor, twenty of clock, one of dir
/// flag), each row's ([`row_len`]) and the checksum's ([`sum_text`]). Counted,
/// not rendered, so the idle write's check of a line renders nothing it does
/// not write.
fn line_len() -> usize {
    let head = HEAD.len()
        + " pid=".len()
        + 10
        + " committed=".len()
        + 20
        + " at=".len()
        + 20
        + " dir=".len()
        + 1;
    let rows: usize = Row::ALL.iter().map(|&row| row_len(row)).sum();
    head + rows + " sum=".len() + 16 + "\n".len()
}

/// THE LINE'S CHECKSUM: aterm-hash's word-at-a-time Fx hash, not FNV-1a. It
/// only has to tell a torn read from a whole line, and the loop pays for it
/// at every idle write: over a line of about 6 KiB FNV-1a's byte-serial
/// multiply chain was most of the write's cost ([`Row::TallyWrite`]).
fn checksum(body: &[u8]) -> u64 {
    aterm_hash::fx_hash_bytes(body)
}

impl Tally {
    /// One row.
    #[must_use]
    pub fn row(&self, r: Row) -> &Hist {
        &self.rows[r.index()]
    }

    /// Every wait on the broker, requests and acks.
    #[must_use]
    pub fn blocked_ns(&self) -> u64 {
        self.row(Row::Request)
            .total
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

    /// The line [`Tally::parse`] reads, newline included. Every number is
    /// fixed-width, so every line is the same length and a write at offset 0
    /// replaces the last one whole. Pure (unit-tested).
    #[must_use]
    pub fn render(&self) -> String {
        let mut body = head_text(self).into_bytes();
        for row in Row::ALL {
            body.extend_from_slice(&row_text(row, self.row(row)));
        }
        let sum = sum_text(&body);
        body.extend_from_slice(sum.as_bytes());
        String::from_utf8(body).unwrap_or_default()
    }

    /// Read a line [`Tally::render`] wrote. Pure (unit-tested).
    ///
    /// # Errors
    ///
    /// A line that is not one: another format or version, a field missing, out
    /// of order, not a number or not its width, or a checksum that does not
    /// match — which is what a read that raced the write sees.
    pub fn parse(text: &str) -> Result<Tally, String> {
        let line = text
            .strip_suffix('\n')
            .ok_or_else(|| format!("an unterminated tally line: {text:?}"))?;
        let (body, sum) = line
            .rsplit_once(" sum=")
            .ok_or_else(|| format!("a tally line with no checksum: {line:?}"))?;
        let want = format!("{:016x}", checksum(body.as_bytes()));
        if sum != want {
            return Err(format!(
                "a tally line whose checksum is {sum}, not {want} (a read that raced the write, \
                 or not a tally): {line:?}"
            ));
        }
        let rest = body
            .strip_prefix(HEAD)
            .and_then(|rest| rest.strip_prefix(' '))
            .ok_or_else(|| format!("not a `{HEAD}` line: {line:?}"))?;
        let mut words = rest.split(' ');
        let mut next = |key: &str| -> Result<&str, String> {
            words
                .next()
                .and_then(|w| w.strip_prefix(key))
                .and_then(|w| w.strip_prefix('='))
                .ok_or_else(|| format!("a tally line with no `{key}=` in its place: {line:?}"))
        };
        let dec = |key: &str, v: &str| -> Result<u64, String> {
            v.parse::<u64>()
                .map_err(|_| format!("a tally line whose `{key}` is not a number: {line:?}"))
        };
        let pid = u32::try_from(dec("pid", next("pid")?)?)
            .map_err(|_| format!("a tally line whose pid is not a pid: {line:?}"))?;
        let committed = dec("committed", next("committed")?)?;
        let at_ns = dec("at", next("at")?)?;
        let dir = match next("dir")? {
            "0" => false,
            "1" => true,
            _ => return Err(format!("a tally line whose `dir` is not 0 or 1: {line:?}")),
        };
        let hex = |key: &str, v: &str, width: usize| -> Result<u64, String> {
            if v.len() != width {
                return Err(format!(
                    "a tally line whose `{key}` is not {width} hex digits: {line:?}"
                ));
            }
            u64::from_str_radix(v, 16)
                .map_err(|_| format!("a tally line whose `{key}` is not hex: {line:?}"))
        };
        let mut rows = [Hist::default(); ROWS];
        for (slot, row) in rows.iter_mut().zip(Row::ALL) {
            let key = row.name();
            let value = next(key)?;
            let mut fields = value.split(',');
            let mut field = |what: &str| -> Result<&str, String> {
                fields
                    .next()
                    .ok_or_else(|| format!("a tally line whose `{key}` has no {what}: {line:?}"))
            };
            let count = hex(key, field("count")?, 16)?;
            let total = hex(key, field("total")?, 16)?;
            let max = hex(key, field("max")?, 16)?;
            let bands = field("bands")?;
            if fields.next().is_some() {
                return Err(format!(
                    "a tally line whose `{key}` has fields past its bands: {line:?}"
                ));
            }
            let mut buckets = [0u32; BUCKETS];
            let mut parts = bands.split('.');
            for b in &mut buckets {
                let part = parts.next().ok_or_else(|| {
                    format!("a tally line whose `{key}` has fewer than {BUCKETS} bands: {line:?}")
                })?;
                *b = u32::try_from(hex(key, part, 8)?).unwrap_or(u32::MAX);
            }
            if parts.next().is_some() {
                return Err(format!(
                    "a tally line whose `{key}` has more than {BUCKETS} bands: {line:?}"
                ));
            }
            *slot = Hist {
                count,
                total,
                max,
                buckets,
            };
        }
        if words.next().is_some() {
            return Err(format!("a tally line with words past its fields: {line:?}"));
        }
        Ok(Tally {
            pid,
            committed,
            at_ns,
            dir,
            rows,
        })
    }

    /// What the loop did between `earlier` and this tally: each row's
    /// difference ([`Hist::since`]), the wall time between the two writes,
    /// and this tally's cursor. Pure (unit-tested).
    ///
    /// # Errors
    ///
    /// Two processes' tallies (a relaunched bridge counts from zero again), a
    /// clock or a count that went backwards, or two lines that disagree on
    /// whether the dir was counted.
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
        let mut rows = [Hist::default(); ROWS];
        for (i, slot) in rows.iter_mut().enumerate() {
            *slot = self.rows[i]
                .since(&earlier.rows[i])
                .map_err(|why| format!("the tally's `{}`: {why}", Row::ALL[i].name()))?;
        }
        Ok(Tally {
            pid: self.pid,
            committed: self.committed,
            at_ns,
            dir: self.dir,
            rows,
        })
    }

    /// Whether anything but the clock and the tally's own writes differs from
    /// `other`: what makes an idle moment write.
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

/// When an item was queued for the loop, and whether the loop was parked on
/// its mailbox then ([`crate::mailbox::Mailbox`]'s stamps).
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

    /// The item is taken now: its wait, and whether it was a wake (queued
    /// while the loop was parked).
    pub(crate) fn taken(self) -> (u64, bool) {
        (nanos(self.at.elapsed()), self.parked)
    }
}

/// ROWS ANOTHER PART OF THE BRIDGE KEEPS for the recorder, under a lock of its
/// own, and the recorder copies at its idle write ([`Kept::copy_into`]): the
/// state dir's ([`Row::kept_by_the_dir`]). Off until the recorder that will
/// read it is on ([`Kept::turn_on`]); off, a timer reads no clock.
/// Compiled only with debug assertions, as the state dir's counting is.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default)]
pub(crate) struct Kept {
    on: AtomicBool,
    rows: Mutex<[Hist; ROWS]>,
}

#[cfg(any(test, debug_assertions))]
impl Kept {
    /// Count from now on (`true`), or no more.
    pub(crate) fn turn_on(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    /// A span of `row`, counted when the timer drops — on every way out of the
    /// call that holds it.
    pub(crate) fn timer(&self, row: Row) -> Timer<'_> {
        Timer {
            kept: self,
            row,
            started: self.on.load(Ordering::Relaxed).then(Instant::now),
        }
    }

    fn note(&self, row: Row, ns: u64) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        rows[row.index()].add(ns, row.unit());
    }

    /// Copy every row this keeps that holds a value into `t`.
    pub(crate) fn copy_into(&self, t: &mut Tally) {
        let rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        for (i, h) in rows.iter().enumerate() {
            if h.count > 0 {
                t.rows[i] = *h;
            }
        }
    }
}

#[cfg(any(test, debug_assertions))]
/// One span a [`Kept`] counts when it drops ([`Kept::timer`]).
#[derive(Debug)]
pub(crate) struct Timer<'a> {
    kept: &'a Kept,
    row: Row,
    started: Option<Instant>,
}

#[cfg(any(test, debug_assertions))]
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
    /// The tally file, open for the life of the process; `None` is off.
    out: Option<std::fs::File>,
    /// When the recorder started: [`Tally::at_ns`]'s zero.
    started: Instant,
    tally: Tally,
    /// What the file holds: the last tally written, and its line.
    written: Option<(Tally, Vec<u8>)>,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            out: None,
            started: Instant::now(),
            tally: Tally::default(),
            written: None,
        }
    }
}

impl Recorder {
    /// On when [`SEAM`] is set in a build that reads seams, writing to
    /// `<state>/`[`FILE`]; off otherwise, and off — said once, on stderr — when
    /// the file cannot be opened.
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
    /// An occupied name, including an old tally or a link, is left untouched;
    /// the optional recorder stays off rather than adopting it.
    ///
    /// # Errors
    ///
    /// The exclusive open's, including an already occupied name.
    pub(crate) fn at(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        Ok(Self {
            out: Some(out),
            started: Instant::now(),
            tally: Tally {
                pid: std::process::id(),
                dir: cfg!(any(test, debug_assertions)),
                ..Tally::default()
            },
            written: None,
        })
    }

    /// Whether anything is counted.
    pub(crate) fn on(&self) -> bool {
        self.out.is_some()
    }

    /// When a counted span starts: `None` while off, so a released bridge
    /// reads no clock for it.
    pub(crate) fn starts(&self) -> Option<Instant> {
        self.on().then(Instant::now)
    }

    /// The span [`Recorder::starts`] began has ended now: count it in `row`.
    pub(crate) fn note(&mut self, row: Row, started: Option<Instant>) {
        if let Some(started) = started {
            self.tally.rows[row.index()].add(nanos(started.elapsed()), row.unit());
        }
    }

    /// Count `n` in a row of counts ([`Unit::Count`]).
    pub(crate) fn count(&mut self, row: Row, n: u64) {
        if self.on() {
            self.tally.rows[row.index()].add(n, row.unit());
        }
    }

    /// A request written at `started` has had its reply read now.
    pub(crate) fn request(&mut self, started: Instant) {
        if self.on() {
            self.note(Row::Request, Some(started));
        }
    }

    /// When an ack read is about to start: `None` while off, so a released
    /// bridge reads no clock for it.
    pub(crate) fn ack_starts(&self) -> Option<Instant> {
        self.starts()
    }

    /// The ack read that [`Recorder::ack_starts`] began has returned an ack —
    /// the inbox group's commit's when `commit` (aterm-0's pipelined commit,
    /// counted in [`Row::Commit`] as well, timed by the same read). A read
    /// that did not wait (a try) is counted the same way, at what it cost.
    pub(crate) fn ack(&mut self, started: Option<Instant>, commit: bool) {
        if let Some(started) = started {
            let ns = nanos(started.elapsed());
            self.tally.rows[Row::Ack.index()].add(ns, Unit::Ns);
            if commit {
                self.tally.rows[Row::Commit.index()].add(ns, Unit::Ns);
            }
        }
    }

    /// An idle moment — the loop found nothing queued and has since read the
    /// acks it owed, during which an item may have arrived — its inbox group
    /// cursor at `committed`, and `outside` copying the rows the mailbox and
    /// the state dir keep: write the tally if it moved since the last write
    /// (the clock and this row aside). A write that fails is said on stderr
    /// and turns the tally off, so a full disk costs one line rather than one
    /// per round.
    ///
    /// ONLY THE ROWS THAT MOVED ARE RENDERED: the line is fixed-length and
    /// each row has a fixed place in it, so the line last written is kept and
    /// a changed row is written over its place, then the head and the
    /// checksum.
    pub(crate) fn idle(&mut self, committed: u64, outside: impl FnOnce(&mut Tally)) {
        use std::os::unix::fs::FileExt as _;
        if !self.on() {
            return;
        }
        let began = Instant::now();
        self.tally.committed = committed;
        outside(&mut self.tally);
        if self
            .written
            .as_ref()
            .is_some_and(|(w, _)| !self.tally.moved_from(w))
        {
            return;
        }
        self.tally.at_ns = nanos(self.started.elapsed());
        let line = match self.written.take() {
            Some((last, mut line)) => {
                let head = head_text(&self.tally);
                line[..head.len()].copy_from_slice(head.as_bytes());
                let mut at = head.len();
                for row in Row::ALL {
                    let len = row_len(row);
                    if self.tally.row(row) != last.row(row) {
                        put_row(&mut line[at..at + len], row, self.tally.row(row));
                    }
                    at += len;
                }
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
            ..Tally::default()
        };
        t.rows[Row::Request.index()] = hist(&[31_000_000], Unit::Ns);
        t.rows[Row::Ack.index()] = hist(&[9_000, 18_000_000], Unit::Ns);
        t.rows[Row::Commit.index()] = hist(&[9_000], Unit::Ns);
        t.rows[Row::LaneDeliver.index()] = hist(&[120_000, 250_000, 40_000_000], Unit::Ns);
        t.rows[Row::DirReplace.index()] = hist(&[200_000], Unit::Ns);
        t.rows[Row::DrainPosts.index()] = hist(&[0, 1, 1, 3], Unit::Count);
        t.rows[Row::Park.index()] = hist(&[500_000_000], Unit::Ns);
        t
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

    /// THE V=2 LINE READS BACK AS WRITTEN, AND EVERY LINE IS ONE LENGTH, so a
    /// write at offset 0 always replaces the line whole: the smallest tally
    /// and the largest render to the same width.
    #[test]
    fn a_tally_line_reads_back_as_written_and_every_line_is_one_length() {
        let t = sample();
        let line = t.render();
        assert!(line.starts_with("wait-tally v=2 pid=0000004242 "), "{line}");
        assert_eq!(Tally::parse(&line), Ok(t));
        assert_eq!(t.blocked_ns(), 31_000_000 + 18_009_000);
        let zero = Tally::default();
        let full = Tally {
            pid: u32::MAX,
            committed: u64::MAX,
            at_ns: u64::MAX,
            dir: true,
            rows: [Hist {
                count: u64::MAX,
                total: u64::MAX,
                max: u64::MAX,
                buckets: [u32::MAX; BUCKETS],
            }; ROWS],
        };
        assert_eq!(zero.render().len(), full.render().len());
        assert_eq!(
            [zero.render().len(), full.render().len(), line.len()],
            [line_len(); 3],
            "the counted length is every line's"
        );
        assert_eq!(Tally::parse(&full.render()), Ok(full));
        assert_eq!(full.blocked_ns(), u64::MAX, "saturates");
        assert_eq!(
            line.matches(' ').count(),
            5 + ROWS + 1,
            "a space before each of the head's five fields, each row and the checksum"
        );
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
        // The two whole lines parse.
        assert!(Tally::parse(&old).is_ok() && Tally::parse(&new).is_ok());
        // A CORRUPTED CHECKSUM: one digit of it changed.
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
            "wait-tally v=2\n",
            &old[..old.len() - 1],                            // unterminated
            &old.replace("wait-tally v=2", "wait-tally v=1"), // another version
            &old.replace(" sum=", " sum=0"),                  // a checksum that is not one
        ] {
            assert!(Tally::parse(bad).is_err(), "{bad:?}");
        }
        // A line with the right checksum over the wrong fields is still
        // refused: a v=1 body, and a row a band short.
        for body in ["wait-tally v=2 pid=1 requests=1".to_string(), {
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
        let d = later.since(&earlier).expect("one process");
        assert_eq!((d.pid, d.committed, d.at_ns), (4242, 200, 50_000_000));
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
        // A WINDOW WHOSE LARGEST IS BELOW AN EARLIER ONE: its max is not known
        // (0), and its top band bounds it.
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
        assert_eq!(Row::ALL.len(), ROWS);
        for (i, row) in Row::ALL.iter().enumerate() {
            assert_eq!(row.index(), i);
        }
    }

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
            // NEGATIVE CONTROLS: near misses are other verbs.
            ("sessions status", Row::LaneOther),
            ("outboxes", Row::LaneOther),
            ("link mirror on", Row::LaneOther),
            ("delivered", Row::LaneOther),
        ] {
            assert_eq!(lane_row(line), row, "{line}");
        }
    }

    #[test]
    fn an_off_recorder_counts_nothing_and_an_on_one_writes_only_what_moved() {
        // OFF: nothing counted, no clock read, nothing written.
        let mut off = Recorder::default();
        off.request(Instant::now());
        let started = off.ack_starts();
        assert_eq!(started, None);
        assert_eq!(off.starts(), None);
        off.ack(started, true);
        off.note(Row::LaneDeliver, off.starts());
        off.count(Row::DrainPosts, 3);
        off.idle(9, |_| panic!("an off recorder copies nothing"));
        assert_eq!(off.tally(), Tally::default());

        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join(FILE);
        let mut on = Recorder::at(&path).expect("open");
        assert!(on.on());
        let read = || std::fs::read_to_string(&path).expect("read the tally");
        assert_eq!(read(), "", "nothing until the first idle moment");
        on.request(Instant::now());
        on.request(Instant::now());
        // A publish's ack, then a pipelined commit's: both acks, one commit,
        // and the commit's time is the time of the read that returned it.
        let started = on.ack_starts();
        assert!(started.is_some());
        on.ack(started, false);
        let started = on.ack_starts();
        on.ack(started, true);
        let started = on.starts();
        on.note(Row::LaneSent, started);
        on.count(Row::DrainPosts, 2);
        // The rows another part keeps are copied in at the write.
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
                counts(&written, Row::Commit),
                counts(&written, Row::Ack),
                counts(&written, Row::LaneSent),
                written.row(Row::DrainPosts).total,
            ),
            (std::process::id(), 2, 1, 2, 1, 2)
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

        // AN IDLE MOMENT WITH NOTHING MOVED WRITES NOTHING, though its own
        // write and the clock moved: the file is overwritten from outside,
        // and the next idle moment leaves it.
        std::fs::write(&path, "sentinel").expect("overwrite");
        on.idle(17, |t| kept.copy_into(t));
        assert_eq!(read(), "sentinel");
        // NEGATIVE CONTROL: a moved cursor alone is a change, and is written
        // whole over the outside bytes' place, rendered only where it moved.
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
    /// link target. Only the fresh file belongs to this recorder.
    #[cfg(unix)]
    #[test]
    fn a_wait_tally_never_truncates_an_occupied_name_or_its_link_target() {
        let dir = aterm_tempfile::tempdir().expect("exclusive fixture");
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
