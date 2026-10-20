// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How long the program at a PTY has left its input unread: the
//! kernel-acceptance ledger behind [`crate::sink::SinkWriter::input_backlog`],
//! and the pure rules that turn one reading into a word and a gate.
//!
//! `aterm_pty::input_queue_len` (FIONREAD) says how many bytes are unread; this
//! module says how long the oldest has waited, which separates a program busy
//! for a moment from one that has stopped reading (and whose unchanged screen
//! would pass every screen fence while a driver's key queues behind unread input).
//!
//! The ledger records when the kernel accepted each byte the sink handed it.
//! The kernel queue is FIFO, so the `unread` bytes FIONREAD counts are the newest
//! `unread` accepted, and the oldest sits at offset `accepted - unread`. One
//! probe dates it, with no history of earlier probes (the event loop probes only
//! when it wakes, and a cross-session control write posts no wake).
//!
//! Every answer is a lower bound on the true wait: stamps are taken after the
//! syscall returns, coalescing keeps the newer instant, a merged run is dated by
//! the run it joins, bytes that predate the sink are dated from its birth, and a
//! write in flight moves the computed offset newer. The one over-claim is a
//! writer the ledger does not see (another process's `TIOCSTI`), which this
//! code cannot detect.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A raw-mode reader drains its queue within one event-loop turn, so a byte a
/// raw program has left unread this long is worth saying: an input-writing
/// control verb still writes, and its `OK` carries `warn=input-unread
/// input_bytes=<n> input_wait_ms=<ms>` ([`Gate::Warn`]).
///
/// A warning, not a refusal: on a heavily loaded machine a healthy program can
/// take longer than a second to read. Refusing needs the watch's evidence that
/// the program is not reading ([`Gate::Refuse`]).
pub const WARN_AFTER: Duration = Duration::from_secs(1);

/// The base age at which a stall is published (the `stalled` word, the rim,
/// the menu, a notification, and the refusal of socket input) for a program
/// spinning meanwhile ([`Liveness::Watched`]). The watch stretches it by CPU
/// contention ([`Contention::stretch`]) so a CPU-starved program is not called
/// frozen. Ten times [`WARN_AFTER`] so a multi-second GC pause does not flap.
pub const STALL_AFTER: Duration = Duration::from_secs(10);

/// The base age at which a program asleep (little CPU, nothing drawn) with
/// input waiting is published as stalled, stretched like [`STALL_AFTER`]. An old
/// byte alone does not mean frozen: `less` in front of a slow `git log -S`, or
/// zsh during a slow completion widget, holds a typed key at ~0 CPU with aterm's
/// output queue empty. A deadlock looks the same, so the sleeper is exempt only
/// long enough for a slow producer to finish. A program still drawing is never
/// published ([`Liveness::Watched`]'s `drew`).
pub const QUIET_STALL_AFTER: Duration = Duration::from_secs(300);

/// How contended the machine's CPUs are while a watch looks: runnable load per
/// CPU in thousandths (`load average / online CPUs × 1000`); [`Default`] is idle.
///
/// A runnable but unscheduled program draws nothing, reads nothing and burns
/// little CPU, just like the sleeper [`QUIET_STALL_AFTER`] catches, and every
/// second of its work takes several of wall time. So thresholds stretch by this
/// ratio ([`Self::stretch`]), up to [`Self::MAX_STRETCH`]: at 8.3 runnable
/// threads per CPU a spinning program is published at 83 s instead of 10 s. A
/// program seen spinning is being scheduled, and one seen drawing is never
/// published; the stretch only delays the call on the starved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Contention {
    /// Runnable threads per online CPU, times 1000.
    per_cpu_milli: u32,
}

impl Contention {
    /// The most a threshold is stretched, so a stall on the most loaded machine
    /// is still published within minutes (spinning) or the hour (asleep).
    pub const MAX_STRETCH: u32 = 10;

    /// The contention for a `load` average over `cpus` online CPUs. No CPUs,
    /// or a load that is not a finite non-negative number, reads as idle.
    #[must_use]
    pub fn of_load(load: f64, cpus: usize) -> Self {
        if cpus == 0 || !load.is_finite() || load <= 0.0 {
            return Self::default();
        }
        let per_cpu = load / cpus as f64 * 1000.0;
        // Saturates, and anything past MAX_STRETCH stretches the same.
        let per_cpu_milli = if per_cpu >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            per_cpu as u32
        };
        Self { per_cpu_milli }
    }

    /// Construct [`Contention`] directly from `per_cpu_milli` (`load / cpus * 1000`).
    #[must_use]
    pub const fn from_per_cpu_milli(per_cpu_milli: u32) -> Self {
        Self { per_cpu_milli }
    }

    /// Runnable threads per CPU, times 1000 (for a status or a log line).
    #[must_use]
    pub const fn per_cpu_milli(self) -> u32 {
        self.per_cpu_milli
    }

    /// Whether runnable load exceeds 1.0 thread per CPU (`per_cpu_milli > 1000`).
    #[must_use]
    pub const fn is_oversubscribed(self) -> bool {
        self.per_cpu_milli > 1000
    }

    /// Whether runnable load reaches or exceeds [`Self::MAX_STRETCH`].
    #[must_use]
    pub const fn is_at_max_stretch(self) -> bool {
        self.per_cpu_milli >= Self::MAX_STRETCH.saturating_mul(1000)
    }

    /// The stretch factor, times 1000: `min(max(1, load/cpu), MAX_STRETCH)`.
    #[must_use]
    pub const fn factor_milli(self) -> u32 {
        let max_milli = Self::MAX_STRETCH.saturating_mul(1000);
        if self.per_cpu_milli < 1000 {
            1000
        } else if self.per_cpu_milli > max_milli {
            max_milli
        } else {
            self.per_cpu_milli
        }
    }

    /// `base`, stretched by the runnable load per CPU when that exceeds one:
    /// `base × min(max(1, load/cpu), MAX_STRETCH)`. Never shorter than `base`.
    #[must_use]
    pub fn stretch(self, base: Duration) -> Duration {
        base.saturating_mul(self.factor_milli()) / 1000
    }

    /// Stretch a millisecond threshold by [`Self::factor_milli`], saturating on overflow.
    #[must_use]
    pub const fn stretch_millis(self, base_ms: u64) -> u64 {
        let scaled = (base_ms as u128).saturating_mul(self.factor_milli() as u128) / 1000;
        if scaled > (u64::MAX as u128) {
            u64::MAX
        } else {
            scaled as u64
        }
    }

    /// The CPU time one runnable thread can expect across `wall_ns` of wall
    /// time, in nanoseconds: all of it when idle, `1/(load/cpu)` of it when
    /// oversubscribed (the [`Self::stretch`] factor). The watch calls a program
    /// spinning when it burned at least half of this, so a frozen program
    /// spinning on a saturated machine is not taken for a sleeper.
    #[must_use]
    pub const fn fair_share_ns(self, wall_ns: u64) -> u64 {
        let factor = self.factor_milli() as u128;
        let denom = if factor == 0 { 1000 } else { factor };
        let share = (wall_ns as u128).saturating_mul(1000) / denom;
        if share > (u64::MAX as u128) {
            u64::MAX
        } else {
            share as u64
        }
    }
}

/// Oldest unread byte offset in a FIFO queue where `accepted` bytes have been written
/// and `unread` bytes remain in the kernel buffer (`accepted.saturating_sub(unread)`).
#[must_use]
pub const fn oldest_unread_offset(accepted: u64, unread: u64) -> u64 {
    accepted.saturating_sub(unread)
}

/// Return `true` iff `unread <= accepted` (no external writer injected unrecorded bytes).
#[must_use]
pub const fn unread_within_accepted(accepted: u64, unread: u64) -> bool {
    unread <= accepted
}

/// Encode `(accepted, unread)` input-backlog watermarks into 16 little-endian bytes.
#[must_use]
pub const fn encode_backlog_watermarks_le(accepted: u64, unread: u64) -> [u8; 16] {
    [
        (accepted & 0xFF) as u8,
        ((accepted >> 8) & 0xFF) as u8,
        ((accepted >> 16) & 0xFF) as u8,
        ((accepted >> 24) & 0xFF) as u8,
        ((accepted >> 32) & 0xFF) as u8,
        ((accepted >> 40) & 0xFF) as u8,
        ((accepted >> 48) & 0xFF) as u8,
        ((accepted >> 56) & 0xFF) as u8,
        (unread & 0xFF) as u8,
        ((unread >> 8) & 0xFF) as u8,
        ((unread >> 16) & 0xFF) as u8,
        ((unread >> 24) & 0xFF) as u8,
        ((unread >> 32) & 0xFF) as u8,
        ((unread >> 40) & 0xFF) as u8,
        ((unread >> 48) & 0xFF) as u8,
        ((unread >> 56) & 0xFF) as u8,
    ]
}

/// Decode 16 little-endian bytes into `(accepted, unread)` input-backlog watermarks.
#[must_use]
pub const fn decode_backlog_watermarks_le(bytes: [u8; 16]) -> (u64, u64) {
    let accepted = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let unread = (bytes[8] as u64)
        | ((bytes[9] as u64) << 8)
        | ((bytes[10] as u64) << 16)
        | ((bytes[11] as u64) << 24)
        | ((bytes[12] as u64) << 32)
        | ((bytes[13] as u64) << 40)
        | ((bytes[14] as u64) << 48)
        | ((bytes[15] as u64) << 56);
    (accepted, unread)
}

/// What was seen of the program while its input waited: the evidence that
/// separates a frozen program (huge, spinning, drawing nothing) from a healthy
/// one merely not reading yet. Only [`classify`]'s entry into
/// [`InputWord::Stalled`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Nothing seen but this one reading (a `status` of an unwatched session, a
    /// refusal). Never enough to call a stall: it says `pending` until the watch
    /// has published one.
    Unobserved,
    /// A watch looked, across its probes: `drew` means aterm read output after
    /// the oldest unread byte was accepted; `spinning` means the foreground
    /// group leader burned at least half of one runnable thread's CPU share over
    /// the last window ([`Contention::fair_share_ns`]).
    Watched {
        /// Output arrived after the oldest unread byte: the program is alive
        /// and working, only not reading yet (a progress line, a redraw).
        drew: bool,
        /// The leader is burning CPU while it neither reads nor draws.
        spinning: bool,
        /// How contended the machine's CPUs were at the look: the entry
        /// thresholds stretch by it ([`Contention::stretch`]).
        contention: Contention,
    },
    /// A stall is already published for this session: it stays until the
    /// input is read (or turns young), whatever the program does meanwhile,
    /// so the word does not flap.
    Published,
}

/// When the kernel accepted each input byte a sink handed it: the
/// kernel-acceptance ledger.
///
/// Offsets count every byte any sink site handed the kernel, in kernel order:
/// every production master write happens under the sink's fd lock, bracketed
/// [`Self::begin`] … [`Self::end`] around its one `write(2)`, so runs are
/// appended in queue order with monotone instants.
///
/// Bounded at [`Self::CAPACITY`] runs. A write within [`Self::COALESCE`] of the
/// newest run joins it, and a full ring merges two adjacent runs, never drops
/// one, so every accepted byte keeps a run. Both date bytes later than accepted,
/// which only shortens a computed wait.
///
/// The ring merges the run that loses the smallest share of its oldest byte's
/// age (`merge_cheapest`): recent bytes keep fine dates, old ones share coarse
/// runs, and an isolated old byte (the unread Enter a frozen program left
/// behind later keystrokes) keeps its own. Evicting the oldest, or merging the
/// closest pair in absolute time, would date that byte by newer input and hide
/// the stall. The tests check every byte of a stream sixteen times the ring's
/// length is dated within 15% of its true age.
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

    /// A write accepted within this long of the newest run joins it. Far below
    /// every threshold the ledger feeds, and it can only shorten a computed
    /// wait, never lengthen it.
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
    /// ([`crate::sink::SinkWriter::discard_unread_input`], called after its
    /// flush): bytes accepted from here on are the first the program can read
    /// since. The write in flight counts as before the mark: flushed or queued
    /// behind the flush, it is not evidence of a read. A short in-flight write
    /// leaves the mark past `accepted`, under-counting later bytes (the side
    /// that claims no read).
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

    /// Move every stamp (each run's and the birth) `by` into the past,
    /// saturating: the history ages together, so every bound stays a lower bound.
    /// Tests only ([`crate::sink::SinkWriter::backdate_input_for_testing`]).
    #[cfg(any(test, debug_assertions))]
    pub fn backdate(&mut self, by: Duration) {
        let back = |at: Instant| at.checked_sub(by).unwrap_or(at);
        self.born = back(self.born);
        for run in &mut self.ring {
            run.first = back(run.first);
            run.last = back(run.last);
        }
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
            // Keep the newer instant: the merged run's older bytes are dated
            // later than accepted, the safe side.
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

    /// Make room for one more run: fold the run whose merge into its successor
    /// costs the smallest relative dating error. Merged, its oldest byte
    /// (`now - first` old) reads `now - successor.last` old, a loss of
    /// `(successor.last - first) / (now - first)` of its age; `now` is the
    /// pushed write's stamp.
    ///
    /// Any choice stays a lower bound; this one keeps the answer close. The
    /// pushed write always gets its own run. O([`Self::CAPACITY`]) per push,
    /// under the ledger mutex, never across a syscall.
    fn merge_cheapest(&mut self, now: Instant) {
        let mut best: Option<(usize, u128, u128)> = None;
        for (i, (run, next)) in self.ring.iter().zip(self.ring.iter().skip(1)).enumerate() {
            let span = next.last.saturating_duration_since(run.first).as_nanos();
            // Never zero (a push is at least `COALESCE` after every run); the
            // floor only keeps the cross-multiplication exact.
            let age = now.saturating_duration_since(run.first).as_nanos().max(1);
            // `span / age < best_span / best_age`, cross-multiplied to stay
            // exact; ties keep the older pair. A saturated comparison can only
            // pick a worse merge, never an unsafe one.
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

    /// No later than when the oldest of the `unread` bytes FIONREAD counts was
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
    /// The runs partition `[0, accepted)`, so some run's `end` exceeds every
    /// `idx` below `accepted`. Read `unread` before this call (see
    /// [`crate::sink::SinkWriter::input_backlog`]): a write landing in between
    /// only raises `top`, moving `idx` newer.
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
    /// The stamp of the run's oldest write. Never an answer (it would over-claim
    /// the newer bytes' wait); only the cost of merging the run.
    first: Instant,
    /// The stamp of the run's newest write: every byte of the run was accepted
    /// no later than this, and is dated by it.
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
    /// Bytes the program wrote that aterm has not read yet (TIOCOUTQ). Non-zero
    /// means the lag may be aterm's, not the program's.
    pub output_backlog: Option<usize>,
}

/// How many bytes a non-canonical slave's input queue takes before a write into
/// the master answers EAGAIN (xnu's `TTYHOG - 2`, cbreak and raw alike). A
/// write refused there, a signal character included, is spilled by aterm behind
/// the unread bytes.
pub const KERNEL_QUEUE_ROOM: usize = 1022;

impl InputBacklog {
    /// Every input byte the program has not read: the kernel's queue plus
    /// the spill queued behind it.
    #[must_use]
    pub fn unread(&self) -> usize {
        self.queued.saturating_add(self.spilled.unwrap_or(0))
    }

    /// Whether writing exactly `bytes` now becomes a signal rather than input:
    /// one byte, one of the slave's signal characters while `ISIG` is set
    /// ([`aterm_pty::TtySignals`]), and a kernel that takes it now (nothing
    /// spilled ahead, room in the queue per [`KERNEL_QUEUE_ROOM`]).
    ///
    /// Such a write is not queued behind the unread bytes: the line discipline
    /// turns it into SIGINT, SIGQUIT or SIGTSTP and (unless `NOFLSH`) flushes the
    /// queue (measured by `tests/input_backlog_pty.rs`'s
    /// `a_control_key_a_cbreak_driver_eats_is_not_a_read`). So the
    /// unread-input gate does not refuse it: `key ctrl+c` into a stuck cbreak
    /// program is the remedy. Raw mode, a busy or non-empty spill, or a full
    /// queue answer `false`.
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
    /// [`STALL_AFTER`] (stretched by the machine's [`Contention`]), held back
    /// by aterm's own unread output, or left by a program seen drawing, or
    /// asleep for less than [`QUIET_STALL_AFTER`] (stretched alike).
    Pending,
    /// Complete lines a canonical reader has not asked for yet: ordinary shell
    /// type-ahead. Never flagged, never refused.
    Typeahead,
    /// A raw reader has left input unread past [`STALL_AFTER`] while
    /// spinning and drawing nothing (past [`QUIET_STALL_AFTER`] while asleep),
    /// each stretched by the machine's [`Contention`] at entry.
    Stalled,
    /// Input is queued and the foreground job is stopped (`SIGSTOP`/`SIGTSTP`):
    /// nothing reads it until the job continues.
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
/// 5. a stall already published and `wait >= STALL_AFTER` → `Stalled`;
/// 6. a watch saw the program neither read nor draw, with aterm's own read of
///    its output caught up, while it was spinning on the CPU for
///    `stretch(STALL_AFTER)` — or asleep for `stretch(QUIET_STALL_AFTER)` →
///    `Stalled`, where `stretch` is [`Contention::stretch`] at the look;
/// 7. otherwise → `Pending`.
///
/// Rule 6's evidence gates only entry. A program blocked writing to an aterm
/// that is not draining its output is waiting for aterm and is not blamed. An
/// unread byte alone is not a freeze: a sleeping `less` or zsh widget, or a
/// cbreak program still drawing, leaves one too. On an oversubscribed machine a
/// healthy program is slower, so the ages stretch with the load. Once published
/// (rule 5), none of these clears the stall, so the word does not flap; a byte
/// younger than the base [`STALL_AFTER`] does, since the program has read what
/// it was published for.
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
    let stalled = match liveness {
        Liveness::Unobserved => false,
        Liveness::Published => b.wait >= STALL_AFTER,
        Liveness::Watched {
            drew,
            spinning,
            contention,
        } => {
            b.output_backlog == Some(0)
                && !drew
                && if spinning {
                    b.wait >= contention.stretch(STALL_AFTER)
                } else {
                    b.wait >= contention.stretch(QUIET_STALL_AFTER)
                }
        }
    };
    if stalled {
        InputWord::Stalled
    } else {
        InputWord::Pending
    }
}

/// What the unread-input gate does with one input-writing control verb
/// ([`gate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gate {
    /// Write, and answer as ever.
    Open,
    /// Write, and say so: raw input has waited [`WARN_AFTER`] unread, so the
    /// bytes written now are read after it. The verb's `OK` carries
    /// `warn=input-unread input_bytes=<n> input_wait_ms=<ms>`.
    Warn,
    /// Write nothing: `ERR busy input-unread …`. Only where writing is known to
    /// be harmful: the job is stopped, or the watch has published the program
    /// as not reading and its input is still stale.
    Refuse,
}

/// The unread-input gate for one input-writing control verb: `stopped` is the
/// foreground job's stop state, `stall_published` whether the session's watch
/// has published a `stalled` fact (evidence one reading cannot give; see
/// [`classify`]).
///
/// | reading | answer |
/// |---|---|
/// | nothing in the kernel's queue | `Open` |
/// | a stopped job with input queued (any age, any mode) | `Refuse` |
/// | a published stall, raw, oldest byte at least [`STALL_AFTER`] old | `Refuse` |
/// | raw, oldest byte at least [`WARN_AFTER`] old | `Warn` |
/// | otherwise (young, canonical type-ahead) | `Open` |
///
/// Only the two refusals prevent harm: a key written behind a stopped job's
/// queue, or behind a program established as not reading, would be read later
/// against a screen the program never drew. A merely slow program gets the key,
/// and the driver gets the warning.
#[must_use]
pub fn gate(b: &InputBacklog, stopped: bool, stall_published: bool) -> Gate {
    if b.queued == 0 {
        return Gate::Open;
    }
    if stopped || (stall_published && !b.canonical && b.wait >= STALL_AFTER) {
        return Gate::Refuse;
    }
    if lagging(b) {
        return Gate::Warn;
    }
    Gate::Open
}

/// Raw input in the kernel's queue that has waited [`WARN_AFTER`] unread: the
/// warning's condition, and what a restart hold reads as "not reading what was
/// sent since".
#[must_use]
pub fn lagging(b: &InputBacklog) -> bool {
    b.queued > 0 && !b.canonical && b.wait >= WARN_AFTER
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

    /// The ledger truth table, one row per case of `oldest_unread_at`.
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

    /// In flight: the kernel may have accepted any prefix of a write inside
    /// `write(2)`, so unread bytes that could all be that prefix have no date,
    /// and an older unread byte is dated as if the whole write had landed (newer).
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

    /// Why the sink parks outside the bracket: a blocking writer on a full queue
    /// can wait in `poll(POLLOUT)` as long as the program stays frozen. Inside
    /// the bracket its whole chunk would count as in flight, and any unread
    /// count up to the chunk (a full raw queue is smaller than the drainer's
    /// 8 KiB chunk) would read "no date". This row pins that blindness.
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

    /// Coalescing keeps the newer instant, and making room dates a merged byte
    /// by the run it joins: both later than the truth, so both only shorten a wait.
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

    /// Past capacity: an Enter a frozen program never read, then more keystrokes
    /// than the ring has runs. The Enter dates the stall; merging it into the
    /// keystrokes would cost it almost all its age (and lift a published stall),
    /// so it keeps its date after every one of them.
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

    /// A ring full of old history rules out merging the pair closest in absolute
    /// time: 600 commands over five hours (nine keys 150 ms apart, then a 30 s
    /// pause), the Enter the program freezes on, and 9 s later a burst of mouse
    /// reports. By absolute gap the Enter would join the reports and read ~1 s
    /// old instead of 10.7 s; by relative error that merge costs all its age, so
    /// the history compresses instead.
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

    /// A stream longer than the ring (a full raw queue of one-byte writes, 150 ms
    /// apart) is dated at every unread count no earlier than the truth and
    /// within 15% of the byte's true age.
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

    /// The discard mark counts only bytes accepted after its discard, so a
    /// program that survives `signal term` and reads again is no longer
    /// published as frozen. A write in flight at the mark counts as before it,
    /// and a short one under-counts what follows: both claim no read.
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

    /// The gate: raw input unread for a second warns (the write goes ahead) and
    /// 0.99 s is open; canonical type-ahead is open at any age; the only
    /// refusals are a stopped job with input queued (any mode) and a published
    /// stall whose oldest byte is still [`STALL_AFTER`] old. Nothing queued is
    /// open, a published stall included.
    #[test]
    fn the_gate_warns_on_age_and_refuses_only_a_published_stall_or_a_stopped_job() {
        use Gate as G;
        assert_eq!(gate(&raw(1, 990), false, false), G::Open, "0.99 s");
        assert_eq!(gate(&raw(1, 1000), false, false), G::Warn, "1.0 s warns");
        // Negative control: age alone never refuses; until the watch has
        // published a stall, the program is only slow.
        for wait_ms in [1_000, 9_999, 10_000, 600_000, 3_600_000] {
            assert_eq!(
                gate(&raw(1, wait_ms), false, false),
                G::Warn,
                "{wait_ms} ms unpublished"
            );
        }
        // Published and still stale: refused.
        assert_eq!(gate(&raw(1, 10_000), false, true), G::Refuse);
        assert_eq!(gate(&raw(1, 3_600_000), false, true), G::Refuse);
        // Published, but what is queued now is younger than the stall: the
        // program has read what it was published for, and is only slow.
        assert_eq!(gate(&raw(1, 9_999), false, true), G::Warn);
        assert_eq!(gate(&raw(1, 500), false, true), G::Open);
        // A stopped job: refused at any age and in any mode.
        assert_eq!(gate(&raw(1, 0), true, false), G::Refuse);
        let canonical = InputBacklog {
            canonical: true,
            ..raw(4, 3_600_000)
        };
        assert_eq!(gate(&canonical, false, false), G::Open, "type-ahead");
        assert_eq!(
            gate(&canonical, false, true),
            G::Open,
            "type-ahead, even under a publication"
        );
        assert_eq!(gate(&canonical, true, false), G::Refuse, "stopped");
        assert_eq!(gate(&raw(0, 3_600_000), false, true), G::Open);
        assert_eq!(gate(&raw(0, 0), true, false), G::Open, "stopped, empty");
        let spilled_only = InputBacklog {
            spilled: Some(9),
            ..raw(0, 3_600_000)
        };
        assert_eq!(
            gate(&spilled_only, false, true),
            G::Open,
            "only kernel-queued bytes are judged"
        );
        assert!(lagging(&raw(1, 1000)) && !lagging(&raw(1, 999)));
        assert!(!lagging(&canonical) && !lagging(&raw(0, 60_000)));
    }

    /// The stretch: an idle machine, or one at or below one runnable thread per
    /// CPU, judges at the base thresholds; a loaded one stretches them by its
    /// load per CPU, up to ten times.
    #[test]
    fn contention_stretches_the_stall_thresholds_with_the_load_per_cpu() {
        let s = |load: f64, cpus: usize| Contention::of_load(load, cpus).stretch(STALL_AFTER);
        assert_eq!(Contention::default().stretch(STALL_AFTER), STALL_AFTER);
        assert_eq!(s(9.0, 18), STALL_AFTER, "half loaded: no stretch");
        assert_eq!(s(18.0, 18), STALL_AFTER, "one per CPU: no stretch");
        assert_eq!(s(36.0, 18), STALL_AFTER * 2);
        assert_eq!(s(150.0, 18), Duration::from_millis(83_330));
        assert_eq!(s(170.0, 18), Duration::from_millis(94_440));
        assert_eq!(s(10_000.0, 18), STALL_AFTER * Contention::MAX_STRETCH);
        assert_eq!(s(f64::INFINITY, 18), STALL_AFTER, "unreadable: idle");
        assert_eq!(s(f64::NAN, 18), STALL_AFTER);
        assert_eq!(s(-1.0, 18), STALL_AFTER);
        assert_eq!(s(150.0, 0), STALL_AFTER, "no CPUs known: idle");
        assert_eq!(
            Contention::of_load(150.0, 18).stretch(QUIET_STALL_AFTER),
            Duration::from_millis(2_499_900)
        );
        assert_eq!(Contention::of_load(150.0, 18).per_cpu_milli(), 8_333);
        // The fair share is the stretch's inverse.
        let second = 1_000_000_000;
        assert_eq!(Contention::default().fair_share_ns(second), second);
        assert_eq!(Contention::of_load(9.0, 18).fair_share_ns(second), second);
        assert_eq!(
            Contention::of_load(150.0, 18).fair_share_ns(second),
            120_004_800,
            "about an eighth of a core at a load of 150"
        );
        assert_eq!(
            Contention::of_load(190.0, 18).fair_share_ns(second),
            second / 10,
            "a tenth at a load of 190: the stretch's cap"
        );
        assert_eq!(
            Contention::of_load(10_000.0, 18).fair_share_ns(second),
            second / 10,
            "never below a tenth"
        );
    }

    /// The word table, in `classify`'s order.
    #[test]
    fn classify_decides_in_order() {
        use InputWord as W;
        use Liveness as L;
        let spins = L::Watched {
            drew: false,
            spinning: true,
            contention: Contention::default(),
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

        // aterm's own unread output blocks entry into stalled, not staying.
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

    /// The liveness gate on entry: a byte ten seconds unread is `stalled` only
    /// for a program seen spinning and drawing nothing, or seen asleep for
    /// [`QUIET_STALL_AFTER`]; a program seen drawing, or a one-off reading with
    /// nothing seen, is never entered. Negative controls: three known false
    /// positives as the watch sees them (`queued=1`, cbreak, output queue empty,
    /// twelve seconds).
    #[test]
    fn classify_enters_a_stall_only_on_evidence_the_program_is_stuck() {
        use InputWord as W;
        use Liveness as L;
        let at = |s: u64| raw(1, s * 1000);
        let seen = |drew, spinning| L::Watched {
            drew,
            spinning,
            contention: Contention::default(),
        };
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
        // Spinning, nothing drawn: stalled.
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
        // Unpublished, none of them is refused: the gate warns.
        assert_eq!(gate(&at(12), false, false), Gate::Warn);
    }

    /// A contended machine (load 150 on 18 CPUs) stretches the thresholds 8.33
    /// times: a spinning, silent program is entered at 83.3 s, not 10 s, and a
    /// sleeper at 41.7 min, not 5; an idle machine enters the same readings at
    /// once. A published stall is not re-judged by load: it stays while its byte
    /// is [`STALL_AFTER`] old.
    #[test]
    fn classify_stretches_entry_by_the_machines_contention() {
        use InputWord as W;
        let loaded = Contention::of_load(150.0, 18);
        let seen = |spinning, contention| Liveness::Watched {
            drew: false,
            spinning,
            contention,
        };
        let at_ms = |ms: u64| raw(1, ms);
        // Spinning and silent.
        assert_eq!(
            classify(
                Some(&at_ms(12_000)),
                false,
                seen(true, Contention::default())
            ),
            W::Stalled,
            "idle machine: the base ten seconds"
        );
        assert_eq!(
            classify(Some(&at_ms(83_000)), false, seen(true, loaded)),
            W::Pending,
            "loaded: 83 s is still slow, not frozen"
        );
        assert_eq!(
            classify(Some(&at_ms(83_330)), false, seen(true, loaded)),
            W::Stalled
        );
        // Asleep and silent.
        assert_eq!(
            classify(
                Some(&at_ms(300_000)),
                false,
                seen(false, Contention::default())
            ),
            W::Stalled
        );
        assert_eq!(
            classify(Some(&at_ms(2_000_000)), false, seen(false, loaded)),
            W::Pending,
            "a starved program looks asleep; it gets the stretched five minutes"
        );
        assert_eq!(
            classify(Some(&at_ms(2_499_900)), false, seen(false, loaded)),
            W::Stalled
        );
        // Published: the base, whatever the load.
        assert_eq!(
            classify(Some(&at_ms(10_000)), false, Liveness::Published),
            W::Stalled
        );
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

    /// A signal character is not input while ISIG is set: exactly one byte, one
    /// of the tty's own signal characters (as remapped or disabled), with nothing
    /// spilled ahead and room in the queue. Raw mode, two bytes, an unknown or
    /// non-empty spill, or a full queue is input the gate still judges.
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
        assert_eq!(
            gate(&cbreak, false, true),
            Gate::Refuse,
            "the rule itself is unchanged"
        );
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
}
