// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How long the program at a PTY has left its input unread — the kernel-
//! acceptance ledger behind [`crate::sink::SinkWriter::input_backlog`], and the
//! pure rules that turn one reading into a word and a refusal.
//!
//! WHY THIS EXISTS (2026-09-24): a Claude Code session froze — 140 GB
//! resident, still spinning on the CPU, never reading its tty again — with the
//! owner's Enter sitting unread in the slave's input queue. A supervisor's
//! screen-fenced key passed every screen check (the screen had not changed,
//! because the program had stopped drawing), was queued BEHIND that Enter, and
//! both were read together hours later against a screen nobody had looked at.
//! `aterm_pty::input_queue_len` (FIONREAD) says HOW MANY bytes are unread; this
//! module says how LONG the oldest of them has waited, which is the fact that
//! separates a program that is busy for a moment from one that has stopped
//! reading.
//!
//! The ledger records WHEN the kernel accepted each byte the sink handed it.
//! The kernel queue is FIFO, so the `unread` bytes FIONREAD counts are the
//! NEWEST `unread` bytes the kernel accepted — and their oldest one sits at
//! offset `accepted - unread` in the ledger's byte stream. One probe then
//! dates it, with no history of earlier probes: the answer does not depend on
//! whether anything happened to look between the human's byte and the
//! driver's key (the defect of dating a stall from the first probe that saw
//! it — the event loop only probes when it wakes, and a cross-session control
//! write posts no wake).
//!
//! Every answer is a LOWER BOUND on the true wait, never an over-claim: stamps
//! are taken after the syscall returns (later than the kernel's acceptance),
//! coalescing keeps the NEWER instant, a run merged to make room is dated by
//! the run it joins (later than its own), bytes that predate the sink are
//! dated from the sink's birth, and a write still in flight pushes the
//! computed offset NEWER. The one way to over-claim is a writer the ledger
//! does not see (another process's `TIOCSTI`): a named residual, not a case
//! this code can detect.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A raw-mode reader drains its queue within one event-loop turn, so a byte a
/// raw program has left unread this long says it has stopped reading. At this
/// age an input-writing control verb is refused (`ERR busy input-unread`): a
/// false refusal costs a driver one back-off, a missed one reproduces the
/// 2026-09-24 incident — a key read, much later, against a screen the program
/// never drew.
pub const REFUSE_AFTER: Duration = Duration::from_secs(1);

/// The age at which the stall is PUBLISHED (the `stalled` word, the rim, the
/// menu, a notification) — for a program that is SPINNING while it does so
/// ([`Liveness::Watched`]). Ten times [`REFUSE_AFTER`] so a multi-second GC
/// pause does not flap every surface an owner looks at; the refusal, which
/// costs nothing to lift, stays at one second.
pub const STALL_AFTER: Duration = Duration::from_secs(10);

/// The age at which a program that is ASLEEP — no CPU to speak of, nothing
/// drawn — while its input waits is published as stalled too. An old byte
/// alone does not say "frozen" (whole-branch review, 2026-09-25, measured on
/// Darwin ptys): `less` in front of a slow `git log -S` holds the `q` typed
/// at it, and zsh holds the key typed during a slow completion widget, both
/// in cbreak mode, both at ~0 CPU, both with aterm's output queue empty —
/// and both were published at ten seconds as "frozen … restart it". A
/// deadlocked program looks the same from outside, so the sleeper is not
/// exempt for ever, only for long enough that a slow producer or completion
/// has had its chance: five minutes. A program still DRAWING is never
/// published ([`Liveness::Watched`]'s `drew`).
pub const QUIET_STALL_AFTER: Duration = Duration::from_secs(300);

/// What was seen of the program itself while its input waited — the evidence
/// that separates the incident's frozen Claude Code (38.8 GiB resident,
/// ~380% CPU, nothing drawn for hours) from a healthy program that is merely
/// not reading yet. Only [`classify`]'s ENTRY into [`InputWord::Stalled`]
/// reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Nothing seen but this one reading — a `status` of a session no watch
    /// has published, a refusal. Never enough to call a program stalled:
    /// such a reading says `pending` until the watch, which does look, has
    /// published the stall.
    Unobserved,
    /// A watch looked, across its probes: `drew` — aterm read output from
    /// the program after its oldest unread byte was accepted; `spinning` —
    /// the foreground group leader burned at least half a core over the
    /// watch's last window.
    Watched {
        /// Output arrived after the oldest unread byte: the program is alive
        /// and working, only not reading yet (a progress line, a redraw).
        drew: bool,
        /// The leader is burning CPU while it neither reads nor draws.
        spinning: bool,
    },
    /// A stall is already published for this session: it stays until the
    /// input is read (or turns young), whatever the program does meanwhile,
    /// so the word does not flap.
    Published,
}

/// When the kernel accepted each input byte a sink handed it — the
/// kernel-acceptance ledger.
///
/// Offsets count every byte any sink site ever handed the kernel, in kernel
/// order: every production master write happens under the sink's fd
/// serialization lock, and each is bracketed [`Self::begin`] … [`Self::end`]
/// around exactly its one `write(2)`, so the ring is appended in the order the
/// kernel queued the bytes and its instants are monotone.
///
/// Bounded: at most [`Self::CAPACITY`] runs. A write within
/// [`Self::COALESCE`] of the newest run joins it, and a full ring makes room
/// by merging two adjacent runs — never by dropping one, so every accepted
/// byte keeps a run. Both losses date bytes LATER than they were accepted,
/// which can only shorten a computed wait.
///
/// WHICH two runs merge is what keeps the answer useful (S2 review,
/// 2026-09-24). The ring used to evict its OLDEST run, and the oldest run is
/// usually the one that dates the stall: the Enter a frozen program never
/// read, then 64 keystrokes or mouse reports an hour later, and the Enter
/// was dated by a keystroke seconds old — `stalled` fell back to `pending`
/// and the refusal lifted with the Enter still unread. Merging the pair
/// closest in ABSOLUTE time fixes that case and breaks another: once the
/// ring is full of a long session's history every gap left is tens of
/// seconds, so a stalled byte followed a few seconds later by more input is
/// the closest pair and is dated by that input instead. The ring therefore
/// merges the run that loses the smallest share of its oldest byte's AGE
/// (`merge_cheapest`): recent bytes keep fine dates, old ones share coarse
/// runs, and an isolated old byte — whose merge would cost nearly all of its
/// age — keeps its own. Measured by the tests below: every byte of a stream
/// sixteen times the ring's length is dated within 15% of its true age.
#[derive(Clone, Debug)]
pub struct InputLedger {
    /// Bytes the kernel has accepted (every `end` adds its count).
    accepted: u64,
    /// Bytes of the one write currently inside `write(2)`: the kernel may have
    /// accepted any prefix of them already. Zero outside a bracket.
    inflight: u64,
    /// The accepted runs, oldest first. They partition `[0, accepted)`: the
    /// first starts at offset 0 and the last ends at `accepted`.
    ring: VecDeque<Run>,
    /// The sink's birth. Bytes that predate the sink (an adopted session's
    /// queue after the seamless handoff) are dated from here.
    born: Instant,
    /// The last discard's mark ([`Self::mark_discard`]): the discard epoch it
    /// belongs to, and the offset the bytes accepted after that discard
    /// start at. `None` until the first discard.
    discard_mark: Option<(u64, u64)>,
}

impl InputLedger {
    /// Most runs the ring keeps; past this two adjacent runs merge.
    pub const CAPACITY: usize = 64;

    /// A write accepted within this long of the newest run joins it.
    /// Five milliseconds is far below every threshold the ledger feeds
    /// ([`REFUSE_AFTER`] is 200 times longer), so a merged run can shorten a
    /// computed wait by at most the time the run kept arriving — never
    /// lengthen it.
    pub const COALESCE: Duration = Duration::from_millis(5);

    /// An empty ledger for a sink born at `born`.
    #[must_use]
    pub fn new(born: Instant) -> Self {
        Self {
            accepted: 0,
            inflight: 0,
            ring: VecDeque::with_capacity(Self::CAPACITY),
            born,
            discard_mark: None,
        }
    }

    /// Discard `epoch` has just emptied the queue
    /// ([`crate::sink::SinkWriter::discard_unread_input`], called AFTER its
    /// flush): the bytes accepted from here on are the first the program can
    /// read since. The write still in flight counts as BEFORE the mark. Its
    /// bytes may have been flushed with the rest, or may land in the emptied
    /// queue behind the flush, and either way they are not evidence of a
    /// read. A short in-flight write leaves the mark past `accepted`, which
    /// under-counts the bytes that follow — the side that claims no read.
    pub fn mark_discard(&mut self, epoch: u64) {
        self.discard_mark = Some((epoch, self.accepted.saturating_add(self.inflight)));
    }

    /// How many bytes the kernel has accepted since discard `epoch`'s mark
    /// ([`Self::mark_discard`]). `None` when the mark belongs to another
    /// discard: one that has bumped its epoch but not yet flushed and marked,
    /// or none at all.
    #[must_use]
    pub fn accepted_since_discard(&self, epoch: u64) -> Option<u64> {
        let (marked, at) = self.discard_mark?;
        (marked == epoch).then(|| self.accepted.saturating_sub(at))
    }

    /// A write of `len` bytes is about to enter `write(2)`.
    pub fn begin(&mut self, len: usize) {
        self.inflight = u64::try_from(len).unwrap_or(u64::MAX);
    }

    /// The write left `write(2)` with `n` bytes accepted, stamped `at` (taken
    /// after the syscall returned, so no earlier than the acceptance).
    pub fn end(&mut self, n: usize, at: Instant) {
        self.inflight = 0;
        if n == 0 {
            return;
        }
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        self.accepted = self.accepted.saturating_add(n);
        if let Some(newest) = self.ring.back_mut()
            && at.saturating_duration_since(newest.last) < Self::COALESCE
        {
            // Keep the NEWER instant: the merged run's older bytes are then
            // dated later than they were accepted, which is the safe side.
            newest.end = self.accepted;
            newest.last = at.max(newest.last);
            return;
        }
        if self.ring.len() >= Self::CAPACITY {
            self.merge_cheapest(at);
        }
        self.ring.push_back(Run {
            end: self.accepted,
            first: at,
            last: at,
        });
    }

    /// Make room for one more run: fold the run whose merge into its
    /// successor costs the smallest RELATIVE dating error — the merged run is
    /// dated by the successor's `last`, so its oldest byte, `now - first` old,
    /// would read `now - successor.last` old, a loss of
    /// `(successor.last - first) / (now - first)` of its age. `now` is the
    /// stamp of the write being pushed.
    ///
    /// Any choice stays a lower bound (the run joins a NEWER one); this one
    /// keeps the answer close. An isolated old run whose successor is recent
    /// would lose nearly all of its age and is kept; a run of old bytes among
    /// old neighbours loses little and goes first; the pushed write always
    /// gets a run of its own. O([`Self::CAPACITY`]) per push, under the
    /// ledger mutex, never across a syscall.
    fn merge_cheapest(&mut self, now: Instant) {
        let mut best: Option<(usize, u128, u128)> = None;
        for (i, (run, next)) in self.ring.iter().zip(self.ring.iter().skip(1)).enumerate() {
            let span = next.last.saturating_duration_since(run.first).as_nanos();
            // Never zero: a push is stamped at least `COALESCE` after every
            // run; the floor only keeps the cross-multiplication below exact.
            let age = now.saturating_duration_since(run.first).as_nanos().max(1);
            // `span / age < best_span / best_age`, cross-multiplied so the
            // comparison is exact; ties keep the OLDER pair. u128 nanoseconds
            // do not saturate below ages of centuries, and a saturated
            // comparison only picks a worse merge, never an unsafe one.
            let cheaper = best.is_none_or(|(_, best_span, best_age)| {
                span.saturating_mul(best_age) < best_span.saturating_mul(age)
            });
            if cheaper {
                best = Some((i, span, age));
            }
        }
        if let Some((i, _, _)) = best
            && let Some(run) = self.ring.remove(i)
            && let Some(next) = self.ring.get_mut(i)
        {
            next.first = run.first;
        }
    }

    /// No later than when the OLDEST of the `unread` bytes FIONREAD counts was
    /// accepted, or `None` when there is nothing to date.
    ///
    /// With `top = accepted + inflight` and `idx = top - unread`:
    ///
    /// | case | answer |
    /// |---|---|
    /// | `unread == 0` | `None` |
    /// | `unread > top` (bytes older than this sink) | the sink's birth |
    /// | `idx >= accepted` (all of them may still be in flight) | `None` |
    /// | otherwise | the `last` of the first run whose `end > idx` |
    ///
    /// The runs partition `[0, accepted)` — making room merges two runs and
    /// never drops one — so some run's `end` exceeds every `idx` below
    /// `accepted`. `unread` must be read BEFORE this call (see
    /// [`crate::sink::SinkWriter::input_backlog`]): a write that lands in
    /// between only raises `top`, which moves `idx` newer.
    #[must_use]
    pub fn oldest_unread_at(&self, unread: usize) -> Option<Instant> {
        if unread == 0 {
            return None;
        }
        let unread = u64::try_from(unread).unwrap_or(u64::MAX);
        let top = self.accepted.saturating_add(self.inflight);
        let Some(idx) = top.checked_sub(unread) else {
            return Some(self.born);
        };
        if idx >= self.accepted {
            return None;
        }
        self.ring
            .iter()
            .find(|run| run.end > idx)
            .map(|run| run.last)
    }
}

/// One run of accepted bytes in the [`InputLedger`]: the offsets from the
/// previous run's `end` (0 for the first run) up to `end`.
#[derive(Clone, Copy, Debug)]
struct Run {
    /// One past the offset of the run's newest byte.
    end: u64,
    /// The stamp of the run's OLDEST write. Never an answer — dating the
    /// run's newer bytes by it would over-claim their wait — only the price
    /// of merging the run (`InputLedger::merge_cheapest`).
    first: Instant,
    /// The stamp of the run's NEWEST write: every byte of the run was
    /// accepted no later than this, and is dated by it.
    last: Instant,
}

/// One reading of a PTY's input backlog — [`crate::sink::SinkWriter::input_backlog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputBacklog {
    /// Bytes in the slave's input queue the program has not read (FIONREAD;
    /// complete lines only in canonical mode).
    pub queued: usize,
    /// Bytes still in the sink's process-local spill, not yet handed to the
    /// kernel; `None` when the spill mutex was busy and the probe would not
    /// wait for it.
    pub spilled: Option<usize>,
    /// A lower bound on how long the oldest of the `queued` bytes has waited.
    pub wait: Duration,
    /// The slave is in canonical (line) mode.
    pub canonical: bool,
    /// The slave's signal characters while `ISIG` is set, from the same
    /// `tcgetattr` as `canonical`; `None` in raw mode.
    pub signals: Option<aterm_pty::TtySignals>,
    /// Bytes the program WROTE that aterm has not read yet (TIOCOUTQ). A
    /// non-zero count says the lag may be aterm's, not the program's.
    pub output_backlog: Option<usize>,
}

/// How many bytes a non-canonical slave's input queue takes before a write
/// into the master answers EAGAIN — measured on Darwin 25.6 (2026-09-24)
/// through a non-blocking master, in cbreak and raw mode alike: 1022 bytes
/// land and the next write, a signal character included, is refused (xnu's
/// `TTYHOG - 2`). A write refused there is spilled by aterm, behind the bytes
/// the program has not read.
pub const KERNEL_QUEUE_ROOM: usize = 1022;

impl InputBacklog {
    /// Every input byte the program has not read: the kernel's queue plus
    /// the spill queued behind it.
    #[must_use]
    pub fn unread(&self) -> usize {
        self.queued.saturating_add(self.spilled.unwrap_or(0))
    }

    /// Whether writing exactly `bytes` now becomes a SIGNAL rather than input:
    /// one byte, one of the slave's signal characters while `ISIG` is set
    /// ([`aterm_pty::TtySignals`]), and a kernel that takes it now — nothing
    /// spilled ahead of it and room in the queue ([`KERNEL_QUEUE_ROOM`]).
    ///
    /// Such a write cannot be read after the unread bytes: the line
    /// discipline turns it into SIGINT, SIGQUIT or SIGTSTP as it is written,
    /// and (unless `NOFLSH`) flushes the queue with it — measured through
    /// `aterm_pty`'s `queue_len_never_counts_a_signal_character_while_isig_is_set`.
    /// So the unread-input gate does not refuse it (S4 review, 2026-09-24):
    /// `key ctrl+c` into a cbreak program that has left a byte unread is the
    /// remedy, not the incident. In raw mode the same byte is queued and this
    /// answers `false`; so does a spill that is busy or non-empty, and a full
    /// queue, where aterm would hold the byte behind the unread ones.
    #[must_use]
    pub fn signals_on_write(&self, bytes: &[u8]) -> bool {
        let [byte] = bytes else {
            return false;
        };
        self.signals.is_some_and(|s| s.signals(*byte))
            && self.spilled == Some(0)
            && self.queued < KERNEL_QUEUE_ROOM
    }
}

/// What a session's input backlog says about the program reading it — the
/// `status input=` word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputWord {
    /// No reading: not macOS, or the master is not a tty. Wire `-`.
    Unmeasured,
    /// Nothing unread.
    Clear,
    /// Unread input not (yet) published as a stall: younger than
    /// [`STALL_AFTER`], held back by aterm's own unread output, or left by a
    /// program seen drawing, or asleep for less than [`QUIET_STALL_AFTER`].
    Pending,
    /// Complete lines a CANONICAL reader has not asked for yet — the shell's
    /// ordinary type-ahead. Never flagged, never refused.
    Typeahead,
    /// A raw reader has left input unread past [`STALL_AFTER`] while
    /// spinning and drawing nothing (past [`QUIET_STALL_AFTER`] while asleep).
    Stalled,
    /// Input is queued and the foreground job is STOPPED (`SIGSTOP`/`SIGTSTP`):
    /// nothing will read it until the job is continued.
    Stopped,
}

impl InputWord {
    /// The wire word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unmeasured => "-",
            Self::Clear => "clear",
            Self::Pending => "pending",
            Self::Typeahead => "typeahead",
            Self::Stalled => "stalled",
            Self::Stopped => "stopped",
        }
    }
}

/// The word for one reading. `stopped` is the foreground job's stop state;
/// `liveness` is what was seen of the program while its input waited.
/// Decided in this order:
///
/// 1. no reading → `Unmeasured`;
/// 2. nothing unread (kernel plus spill) → `Clear`;
/// 3. a stopped job → `Stopped`;
/// 4. canonical mode → `Typeahead` (only complete lines are counted there,
///    and a complete line a shell has not asked for is ordinary type-ahead);
/// 5. `wait >= STALL_AFTER` and either a stall is already published, or a
///    watch saw the program neither read NOR draw, with aterm's own read of
///    its output caught up, while it was spinning on the CPU — or, asleep,
///    for [`QUIET_STALL_AFTER`] → `Stalled`;
/// 6. otherwise → `Pending`.
///
/// Rule 5's evidence gates only ENTRY. The output test: a program blocked
/// writing to an aterm that is not draining its output is waiting for aterm,
/// and must not be blamed. The liveness test (whole-branch review,
/// 2026-09-25): a raw or cbreak program that has a byte unread for ten
/// seconds is not thereby frozen — measured, `less` waiting on a slow pipe
/// and zsh inside a slow completion widget sleep at ~0 CPU, and a cbreak
/// progress line keeps drawing, all with the byte unread and the output
/// queue empty. The incident's program spun and drew nothing. Once a stall is
/// published, neither test clears it, so the word does not flap while aterm
/// catches up or the program wakes.
#[must_use]
pub fn classify(b: Option<&InputBacklog>, stopped: bool, liveness: Liveness) -> InputWord {
    let Some(b) = b else {
        return InputWord::Unmeasured;
    };
    if b.unread() == 0 {
        return InputWord::Clear;
    }
    if stopped {
        return InputWord::Stopped;
    }
    if b.canonical {
        return InputWord::Typeahead;
    }
    let enters = match liveness {
        Liveness::Unobserved => false,
        Liveness::Published => true,
        Liveness::Watched { drew, spinning } => {
            b.output_backlog == Some(0) && !drew && (spinning || b.wait >= QUIET_STALL_AFTER)
        }
    };
    if b.wait >= STALL_AFTER && enters {
        return InputWord::Stalled;
    }
    InputWord::Pending
}

/// Whether an input-writing control verb must be refused now: the kernel holds
/// bytes the program has not read, and either its job is stopped (at any age)
/// or it reads raw and the oldest of them has waited [`REFUSE_AFTER`]. A key
/// sent now would be read AFTER those bytes, against a screen the program has
/// not drawn. Canonical type-ahead never refuses.
#[must_use]
pub fn refuses(b: &InputBacklog, stopped: bool) -> bool {
    b.queued > 0 && (stopped || (!b.canonical && b.wait >= REFUSE_AFTER))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A ledger whose clock starts at `t0`, with one accepted write per
    /// `(len, at_ms)` pair.
    fn ledger(t0: Instant, writes: &[(usize, u64)]) -> InputLedger {
        let mut l = InputLedger::new(t0);
        for &(len, at) in writes {
            l.begin(len);
            l.end(len, t0 + ms(at));
        }
        l
    }

    /// THE LEDGER TRUTH TABLE, one row per case of `oldest_unread_at`.
    #[test]
    fn ledger_dates_the_oldest_unread_byte() {
        let t0 = Instant::now();

        // Empty: nothing unread has no date; unread bytes the sink never wrote
        // predate it and are dated from its birth.
        let empty = InputLedger::new(t0);
        assert_eq!(empty.oldest_unread_at(0), None);
        assert_eq!(empty.oldest_unread_at(3), Some(t0));

        // Three writes: `\r` at 100 ms, `ab` at 200 ms, `cde` at 300 ms.
        let l = ledger(t0, &[(1, 100), (2, 200), (3, 300)]);
        assert_eq!(l.oldest_unread_at(6), Some(t0 + ms(100)), "all unread");
        assert_eq!(
            l.oldest_unread_at(5),
            Some(t0 + ms(200)),
            "partial consumption: the program read the `\\r`"
        );
        assert_eq!(
            l.oldest_unread_at(4),
            Some(t0 + ms(200)),
            "mid-run: the second byte of `ab` is dated by its run"
        );
        assert_eq!(l.oldest_unread_at(3), Some(t0 + ms(300)));
        assert_eq!(l.oldest_unread_at(1), Some(t0 + ms(300)));
        assert_eq!(
            l.oldest_unread_at(9),
            Some(t0),
            "bytes older than the sink (an adopted queue) date from its birth"
        );
    }

    /// IN FLIGHT: while a write is inside `write(2)` the kernel may already
    /// have accepted any prefix of it, so unread bytes that could all be that
    /// prefix have no date — and an older unread byte is dated as if the
    /// whole in-flight write had landed, which moves it NEWER.
    #[test]
    fn ledger_in_flight_bytes_move_the_answer_newer() {
        let t0 = Instant::now();
        let mut l = ledger(t0, &[(2, 100)]);
        l.begin(4);
        assert_eq!(l.oldest_unread_at(4), None, "all four may be in flight");
        assert_eq!(l.oldest_unread_at(3), None);
        assert_eq!(
            l.oldest_unread_at(5),
            Some(t0 + ms(100)),
            "one byte must predate the flight"
        );
        l.end(1, t0 + ms(400));
        assert_eq!(
            l.oldest_unread_at(3),
            Some(t0 + ms(100)),
            "a short write accepted 1 of 4"
        );
        assert_eq!(l.oldest_unread_at(1), Some(t0 + ms(400)));
    }

    /// WHY THE SINK PARKS OUTSIDE THE BRACKET. A blocking writer on a full
    /// queue (the spill drainer, a paste) can wait in `poll(POLLOUT)` for as
    /// long as the program stays frozen. Were that park inside the bracket,
    /// its whole chunk would count as in flight, and any unread count no
    /// larger than the chunk — a full raw queue is smaller than the drainer's
    /// 8 KiB chunk — would read "no date": the probe blind exactly when a
    /// paste fills a frozen program's queue. So the sink brackets only the
    /// non-blocking `write(2)` and parks between brackets; this row pins the
    /// blindness it avoids.
    #[test]
    fn ledger_a_bracket_held_across_a_park_would_blind_the_probe() {
        let t0 = Instant::now();
        let mut l = ledger(t0, &[(1024, 100)]);
        l.begin(8 * 1024);
        assert_eq!(l.oldest_unread_at(1024), None, "blind while parked");
        l.end(0, t0 + ms(200));
        assert_eq!(
            l.oldest_unread_at(1024),
            Some(t0 + ms(100)),
            "a park between brackets keeps the date"
        );
    }

    /// COALESCING keeps the NEWER instant, and MAKING ROOM dates a merged
    /// byte by the run it joins — both later than the truth, so both only
    /// shorten a wait.
    #[test]
    fn ledger_coalescing_and_merging_stay_lower_bounds() {
        let t0 = Instant::now();
        // 2 ms apart: one run, dated by its newer write.
        let l = ledger(t0, &[(1, 100), (1, 102)]);
        assert_eq!(l.ring.len(), 1);
        assert_eq!(l.oldest_unread_at(2), Some(t0 + ms(102)));
        // 5 ms apart: two runs.
        let l = ledger(t0, &[(1, 100), (1, 105)]);
        assert_eq!(l.ring.len(), 2);
        assert_eq!(l.oldest_unread_at(2), Some(t0 + ms(100)));

        // One write per 10 ms, twice the capacity: half the runs merge, and
        // the runs still partition every accepted byte.
        let n = 2 * InputLedger::CAPACITY as u64;
        let writes: Vec<(usize, u64)> = (1..=n).map(|i| (1, 10 * i)).collect();
        let l = ledger(t0, &writes);
        assert_eq!(l.ring.len(), InputLedger::CAPACITY);
        assert_eq!(l.ring.back().map(|run| run.end), Some(n));
        assert_eq!(
            l.ring.front().map(|run| run.first),
            Some(t0 + ms(10)),
            "the first run still starts at the first write"
        );
        for unread in 1..=n {
            let truth = t0 + ms(10 * (n + 1 - unread));
            let dated = l.oldest_unread_at(unread as usize);
            assert!(
                dated.is_some_and(|at| at >= truth),
                "{unread} unread: dated {dated:?}, a lower bound on {truth:?}"
            );
        }
        assert_eq!(
            l.oldest_unread_at(1),
            Some(t0 + ms(10 * n)),
            "the newest write is never merged away"
        );
    }

    /// THE INCIDENT PAST CAPACITY (S2 review, 2026-09-24): the Enter a frozen
    /// program never read, then — an hour later — more keystrokes than the
    /// ring has runs. The Enter is the byte that dates the stall. Evicting
    /// the oldest run dated it by a keystroke seconds old, so `wait` fell
    /// from an hour to seconds, a published `stalled` fell back to
    /// `pending`, and at mouse-report rates the refusal lifted with the Enter
    /// still unread — a driver's key queued behind it again. Merging it into
    /// the keystrokes would cost it almost all of its age, so it keeps its
    /// date after every one of them.
    #[test]
    fn ledger_a_full_ring_keeps_the_date_of_the_stalled_byte() {
        let t0 = Instant::now();
        let hour = 3_600_000;
        let mut l = ledger(t0, &[(1, 0)]);
        for key in 0..2 * InputLedger::CAPACITY {
            l.begin(1);
            l.end(1, t0 + ms(hour + 150 * key as u64));
            assert_eq!(
                l.oldest_unread_at(key + 2),
                Some(t0),
                "the Enter plus {} keys unread",
                key + 1
            );
        }
        assert_eq!(l.ring.len(), InputLedger::CAPACITY);
    }

    /// A RING FULL OF OLD HISTORY — the case that rules out merging the pair
    /// closest in ABSOLUTE time: 600 commands typed over five hours (nine
    /// keys 150 ms apart, then a 30 s pause), the Enter the program freezes
    /// on, and 9 s later a burst of mouse reports. Every gap the history
    /// leaves in a full ring is about 30 s, so by absolute gap the Enter's
    /// 9 s one is the cheapest and the Enter joins the reports: a second
    /// after the burst it reads about 1 s old instead of 10.7 s, `pending`
    /// instead of `stalled`. By RELATIVE error that merge costs the Enter all of its
    /// age, so the history compresses instead.
    #[test]
    fn ledger_a_ring_full_of_history_keeps_the_date_of_the_stalled_byte() {
        let t0 = Instant::now();
        let mut writes = Vec::new();
        let mut at = 0;
        for _ in 0..600 {
            for _ in 0..9 {
                at += 150;
                writes.push((1, at));
            }
            at += 30_000;
        }
        let enter = at;
        writes.push((1, enter));
        at += 9_000;
        for _ in 0..85 {
            writes.push((12, at));
            at += 8;
        }
        let l = ledger(t0, &writes);
        assert_eq!(l.ring.len(), InputLedger::CAPACITY);
        assert_eq!(l.oldest_unread_at(1 + 85 * 12), Some(t0 + ms(enter)));
    }

    /// A STREAM LONGER THAN THE RING — a full raw queue of one-byte writes,
    /// 150 ms apart — is dated at EVERY unread count no earlier than the
    /// truth (a lower bound) and within 15% of the byte's true age. Evicting
    /// the oldest run kept only the newest 64 exact, so a second after the
    /// last write the queue's oldest byte read 10.45 s old instead of
    /// 154.45 s.
    #[test]
    fn ledger_a_stream_longer_than_the_ring_is_dated_within_a_fraction_of_its_age() {
        let t0 = Instant::now();
        let n: u64 = 1024;
        let writes: Vec<(usize, u64)> = (1..=n).map(|i| (1, 150 * i)).collect();
        let l = ledger(t0, &writes);
        let now = t0 + ms(150 * n + 1_000);
        for unread in 1..=n {
            let truth = t0 + ms(150 * (n + 1 - unread));
            let dated = l
                .oldest_unread_at(unread as usize)
                .expect("every unread byte is dated");
            assert!(dated >= truth, "{unread} unread: a lower bound");
            let (wait, true_wait) = (now - dated, now - truth);
            assert!(
                wait >= true_wait.mul_f64(0.85),
                "{unread} unread: dated {wait:?} old, truly {true_wait:?}"
            );
        }
    }

    /// A zero-byte `end` (refused, would-block, closed) only closes the
    /// bracket: nothing is dated by it.
    #[test]
    fn ledger_a_refused_write_records_nothing() {
        let t0 = Instant::now();
        let mut l = ledger(t0, &[(1, 100)]);
        l.begin(3);
        l.end(0, t0 + ms(900));
        assert_eq!(l.ring.len(), 1);
        assert_eq!(l.oldest_unread_at(1), Some(t0 + ms(100)));
    }

    /// THE DISCARD MARK counts only bytes accepted after the discard it
    /// belongs to (whole-branch review, fourth round, 2026-09-25: a program
    /// that lived through `signal term` and then read again stayed published
    /// as frozen, because nothing counted what it read after the drop). A
    /// write in flight at the mark counts as before it, and a short one
    /// under-counts what follows — both on the side that claims no read.
    #[test]
    fn ledger_the_discard_mark_counts_what_was_accepted_after_it() {
        let t0 = Instant::now();
        let mut l = ledger(t0, &[(3, 100)]);
        assert_eq!(l.accepted_since_discard(1), None, "no discard yet");
        l.mark_discard(1);
        assert_eq!(l.accepted_since_discard(1), Some(0));
        assert_eq!(
            l.accepted_since_discard(2),
            None,
            "the mark is another discard's"
        );
        l.begin(2);
        l.end(2, t0 + ms(200));
        assert_eq!(l.accepted_since_discard(1), Some(2));

        // A write in flight at the mark is not counted after it, however
        // it lands.
        l.begin(4);
        l.mark_discard(2);
        l.end(4, t0 + ms(300));
        assert_eq!(l.accepted_since_discard(2), Some(0));
        // A short one leaves the mark ahead: the next bytes under-count.
        l.begin(4);
        l.mark_discard(3);
        l.end(1, t0 + ms(400));
        l.begin(5);
        l.end(5, t0 + ms(500));
        assert_eq!(
            l.accepted_since_discard(3),
            Some(2),
            "6 accepted, 2 counted"
        );
    }

    fn raw(queued: usize, wait_ms: u64) -> InputBacklog {
        InputBacklog {
            queued,
            spilled: Some(0),
            wait: ms(wait_ms),
            canonical: false,
            signals: None,
            output_backlog: Some(0),
        }
    }

    /// THE REFUSAL RULE: raw input unread for a second refuses, 0.99 s does
    /// not; canonical type-ahead never refuses at any age; a stopped job with
    /// input queued refuses at once; nothing queued never refuses.
    #[test]
    fn refuses_at_one_second_raw_and_at_once_stopped() {
        assert!(!refuses(&raw(1, 990), false), "0.99 s passes");
        assert!(refuses(&raw(1, 1000), false), "1.0 s refuses");
        assert!(refuses(&raw(1, 0), true), "stopped refuses at any age");
        let canonical = InputBacklog {
            canonical: true,
            ..raw(4, 3_600_000)
        };
        assert!(!refuses(&canonical, false), "canonical never refuses");
        assert!(!refuses(&raw(0, 3_600_000), false), "nothing queued");
        assert!(!refuses(&raw(0, 0), true), "stopped with nothing queued");
        let spilled_only = InputBacklog {
            spilled: Some(9),
            ..raw(0, 3_600_000)
        };
        assert!(
            !refuses(&spilled_only, false),
            "only kernel-queued bytes refuse"
        );
    }

    /// THE WORD TABLE, in `classify`'s order.
    #[test]
    fn classify_decides_in_order() {
        use InputWord as W;
        use Liveness as L;
        let spins = L::Watched {
            drew: false,
            spinning: true,
        };
        assert_eq!(classify(None, true, L::Published), W::Unmeasured);
        assert_eq!(
            classify(Some(&raw(0, 60_000)), true, L::Published),
            W::Clear
        );
        let spilled = InputBacklog {
            spilled: Some(3),
            ..raw(0, 0)
        };
        assert_eq!(
            classify(Some(&spilled), false, spins),
            W::Pending,
            "spill counts"
        );
        assert_eq!(classify(Some(&raw(1, 0)), true, L::Unobserved), W::Stopped);
        let canonical = InputBacklog {
            canonical: true,
            ..raw(4, 600_000)
        };
        assert_eq!(classify(Some(&canonical), false, spins), W::Typeahead);
        assert_eq!(classify(Some(&raw(1, 9_999)), false, spins), W::Pending);
        assert_eq!(classify(Some(&raw(1, 10_000)), false, spins), W::Stalled);

        // aterm's own unread output blocks ENTRY into stalled, not staying.
        let lagging = InputBacklog {
            output_backlog: Some(512),
            ..raw(1, 60_000)
        };
        assert_eq!(classify(Some(&lagging), false, spins), W::Pending);
        assert_eq!(classify(Some(&lagging), false, L::Published), W::Stalled);
        let unknown = InputBacklog {
            output_backlog: None,
            ..raw(1, 60_000)
        };
        assert_eq!(
            classify(Some(&unknown), false, spins),
            W::Pending,
            "an unread output queue must be MEASURED zero to enter"
        );
    }

    /// THE LIVENESS GATE on entry (whole-branch review, 2026-09-25): a byte
    /// ten seconds unread is `stalled` only for a program seen SPINNING and
    /// drawing nothing — the incident — or seen asleep for
    /// [`QUIET_STALL_AFTER`]; a program seen drawing is never entered, and a
    /// one-off reading with nothing seen is never entered either. The three
    /// false positives measured on Darwin ptys are the negative controls, as
    /// the watch would see them: all three read `queued=1`, cbreak, output
    /// queue empty, at twelve seconds.
    #[test]
    fn classify_enters_a_stall_only_on_evidence_the_program_is_stuck() {
        use InputWord as W;
        use Liveness as L;
        let at = |s: u64| raw(1, s * 1000);
        let seen = |drew, spinning| L::Watched { drew, spinning };
        // `(sleep 20; echo hi) | less` with `q` typed: asleep, silent.
        assert_eq!(
            classify(Some(&at(12)), false, seen(false, false)),
            W::Pending
        );
        // A zsh widget waiting on a slow completion child: the leader (zsh)
        // asleep, silent — the child's CPU is not the leader's.
        assert_eq!(
            classify(Some(&at(12)), false, seen(false, false)),
            W::Pending
        );
        // A cbreak progress line redrawn every 100 ms with `q` typed: drawing.
        assert_eq!(
            classify(Some(&at(12)), false, seen(true, false)),
            W::Pending
        );
        assert_eq!(
            classify(Some(&at(3600)), false, seen(true, true)),
            W::Pending,
            "a program that draws is alive, at any age and any CPU"
        );
        // The incident: spinning, nothing drawn.
        assert_eq!(
            classify(Some(&at(12)), false, seen(false, true)),
            W::Stalled
        );
        // Asleep and silent for long enough: a deadlock looks like this too.
        assert_eq!(
            classify(Some(&at(299)), false, seen(false, false)),
            W::Pending
        );
        assert_eq!(
            classify(Some(&at(300)), false, seen(false, false)),
            W::Stalled
        );
        // Nothing seen: never entered, at any age — the watch decides.
        assert_eq!(classify(Some(&at(3600)), false, L::Unobserved), W::Pending);
        // The refusal rule does not read liveness: all of them are refused.
        assert!(refuses(&at(12), false));
    }

    #[test]
    fn input_words_are_the_wire_words() {
        use InputWord as W;
        let words: Vec<&str> = [
            W::Unmeasured,
            W::Clear,
            W::Pending,
            W::Typeahead,
            W::Stalled,
            W::Stopped,
        ]
        .into_iter()
        .map(InputWord::as_str)
        .collect();
        assert_eq!(
            words,
            ["-", "clear", "pending", "typeahead", "stalled", "stopped"]
        );
        // `path=frozen` is the stale-PATH adoption mark; no input word may
        // reuse it.
        assert!(!words.contains(&"frozen"));
    }

    /// A SIGNAL CHARACTER IS NOT INPUT while ISIG is set: exactly one byte,
    /// one of the tty's own signal characters (a remapped or disabled one
    /// counts as it is set), with nothing spilled ahead and room in the
    /// queue. Everything else — raw mode, two bytes, an unknown or non-empty
    /// spill, a full queue — is input the gate still judges.
    #[test]
    fn a_lone_signal_character_under_isig_is_not_input() {
        let cbreak = InputBacklog {
            signals: Some(aterm_pty::TtySignals {
                intr: Some(0x03),
                quit: Some(0x1c),
                susp: None,
            }),
            ..raw(1, 60_000)
        };
        assert!(refuses(&cbreak, false), "the rule itself is unchanged");
        assert!(cbreak.signals_on_write(&[0x03]));
        assert!(cbreak.signals_on_write(&[0x1c]));
        assert!(!cbreak.signals_on_write(&[0x1a]), "a disabled VSUSP");
        assert!(!cbreak.signals_on_write(b"\x03\x03"), "two bytes");
        assert!(!cbreak.signals_on_write(b""));
        assert!(!cbreak.signals_on_write(b"q"));
        assert!(!raw(1, 60_000).signals_on_write(&[0x03]), "raw: a byte");
        for spilled in [None, Some(1)] {
            let behind = InputBacklog { spilled, ..cbreak };
            assert!(!behind.signals_on_write(&[0x03]), "spill {spilled:?}");
        }
        let full = InputBacklog {
            queued: KERNEL_QUEUE_ROOM,
            ..cbreak
        };
        assert!(!full.signals_on_write(&[0x03]), "a full queue spills it");
        let room = InputBacklog {
            queued: KERNEL_QUEUE_ROOM - 1,
            ..cbreak
        };
        assert!(room.signals_on_write(&[0x03]));
    }

    #[test]
    fn unread_is_kernel_plus_spill() {
        let b = InputBacklog {
            spilled: Some(5),
            ..raw(3, 0)
        };
        assert_eq!(b.unread(), 8);
        let busy = InputBacklog {
            spilled: None,
            ..raw(3, 0)
        };
        assert_eq!(busy.unread(), 3);
    }
}
