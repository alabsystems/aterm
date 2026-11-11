// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How long the program at a PTY has left its input unread.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A raw-mode reader drains its queue within one event-loop turn.
pub const WARN_AFTER: Duration = Duration::from_secs(1);

/// The base age at which a stall is published for a program spinning meanwhile.
pub const STALL_AFTER: Duration = Duration::from_secs(10);

/// The base age at which a program asleep (little CPU, nothing drawn) with input waiting is
/// published as stalled.
pub const QUIET_STALL_AFTER: Duration = Duration::from_secs(300);

/// How contended the machine's CPUs are while a watch looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Contention {
    /// Runnable threads per online CPU, times 1000.
    per_cpu_milli: u32,
}

impl Contention {
    /// The most a threshold is stretched.
    pub const MAX_STRETCH: u32 = 10;

    /// The contention for a `load` average over `cpus` online CPUs.
    #[must_use]
    pub fn of_load(load: f64, cpus: usize) -> Self {
        if cpus == 0 || !load.is_finite() || load <= 0.0 {
            return Self::default();
        }
        let per_cpu = load / cpus as f64 * 1000.0;
        let per_cpu_milli = if per_cpu >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            per_cpu as u32
        };
        Self { per_cpu_milli }
    }

    /// Runnable threads per CPU, times 1000 (for a status or a log line).
    #[must_use]
    #[inline]
    pub const fn per_cpu_milli(self) -> u32 {
        self.per_cpu_milli
    }

    /// The stretch factor, times 1000: `min(max(1, load/cpu), MAX_STRETCH)`.
    #[must_use]
    #[inline]
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

    /// `base`, stretched by the runnable load per CPU when that exceeds one.
    #[must_use]
    #[inline]
    pub fn stretch(self, base: Duration) -> Duration {
        base.saturating_mul(self.factor_milli()) / 1000
    }

    /// The CPU time one runnable thread can expect across `wall_ns` of wall time, in nanoseconds.
    #[must_use]
    #[inline]
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

/// What was seen of the program while its input waited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Nothing seen but this one reading (a `status` of an unwatched session, a refusal).
    Unobserved,
    /// A watch looked, across its probes.
    Watched {
        /// Output arrived after the oldest unread byte.
        drew: bool,
        /// The leader is burning CPU while it neither reads nor draws.
        spinning: bool,
        /// macOS's program check holds the leader before its first instruction: it reads nothing
        /// because it has not started, which is no stall.
        program_check: bool,
        /// How contended the machine's CPUs were at the look: the entry thresholds stretch by it.
        contention: Contention,
    },
    /// A stall is already published for this session: it stands while its input waits.
    Published {
        /// macOS's program check holds the leader before its first instruction: a program that
        /// has not started is no stall, whatever was published before it.
        program_check: bool,
    },
}

/// When the kernel accepted each input byte a sink handed it.
#[derive(Clone, Debug)]
pub struct InputLedger {
    /// Bytes the kernel has accepted (every `end` adds its count).
    accepted: u64,
    /// Bytes of the one write currently inside `write(2)`.
    inflight: u64,
    /// The accepted runs.
    ring: VecDeque<Run>,
    /// The sink's birth.
    born: Instant,
    /// The last discard's mark ([`Self::mark_discard`]).
    discard_mark: Option<(u64, u64)>,
}

impl InputLedger {
    /// Most runs the ring keeps; past this two adjacent runs merge.
    pub const CAPACITY: usize = 64;

    /// A write accepted within this long of the newest run joins it.
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

    /// Discard `epoch` has just emptied the queue.
    pub const fn mark_discard(&mut self, epoch: u64) {
        self.discard_mark = Some((epoch, self.accepted.saturating_add(self.inflight)));
    }

    /// How many bytes the kernel has accepted since discard `epoch`'s mark.
    #[must_use]
    pub fn accepted_since_discard(&self, epoch: u64) -> Option<u64> {
        let (marked, at) = self.discard_mark?;
        (marked == epoch).then(|| self.accepted.saturating_sub(at))
    }

    /// Move every stamp (each run's and the birth) `by` into the past, saturating.
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

    /// The write left `write` with `n` bytes accepted, stamped `at`.
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

    fn merge_cheapest(&mut self, now: Instant) {
        let mut best: Option<(usize, u128, u128)> = None;
        for (i, (run, next)) in self.ring.iter().zip(self.ring.iter().skip(1)).enumerate() {
            let span = next.last.saturating_duration_since(run.first).as_nanos();
            let age = now.saturating_duration_since(run.first).as_nanos().max(1);
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

    /// No later than when the oldest of the `unread` bytes FIONREAD counts was accepted.
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

/// One run of accepted bytes in the [`InputLedger`].
#[derive(Clone, Copy, Debug)]
struct Run {
    /// One past the offset of the run's newest byte.
    end: u64,
    /// The stamp of the run's oldest write.
    first: Instant,
    /// The stamp of the run's newest write.
    last: Instant,
}

/// One reading of a PTY's input backlog — [`crate::sink::SinkWriter::input_backlog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputBacklog {
    /// Bytes in the slave's input queue the program has not read.
    pub queued: usize,
    /// Bytes still in the sink's process-local spill, not yet handed to the kernel.
    pub spilled: Option<usize>,
    /// A lower bound on how long the oldest of the `queued` bytes has waited.
    pub wait: Duration,
    /// The slave is in canonical (line) mode.
    pub canonical: bool,
    /// The slave's signal characters while `ISIG` is set, from the same `tcgetattr` as `canonical`.
    pub signals: Option<aterm_pty::TtySignals>,
    /// Bytes the program wrote that aterm has not read yet (TIOCOUTQ).
    pub output_backlog: Option<usize>,
}

/// How many bytes a non-canonical slave's input queue takes before a write into the master answers
/// EAGAIN.
pub const KERNEL_QUEUE_ROOM: usize = 1022;

impl InputBacklog {
    /// Every input byte the program has not read.
    #[must_use]
    #[inline]
    pub const fn unread(&self) -> usize {
        self.queued.saturating_add(match self.spilled {
            Some(s) => s,
            None => 0,
        })
    }

    /// Whether writing exactly `bytes` now becomes a signal rather than input.
    #[must_use]
    #[inline]
    pub fn signals_on_write(&self, bytes: &[u8]) -> bool {
        let [byte] = bytes else {
            return false;
        };
        self.signals.is_some_and(|s| s.signals(*byte))
            && self.spilled == Some(0)
            && self.queued < KERNEL_QUEUE_ROOM
    }
}

/// What a session's input backlog says about the program reading it — the `status input=` word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputWord {
    /// No reading: the master is not a tty, or nothing counts its unread input
    /// ([`crate::sink::SinkWriter::unread_input`]: neither macOS nor Linux).
    Unmeasured,
    /// Nothing unread.
    Clear,
    /// Unread input not (yet) published as a stall.
    Pending,
    /// Complete lines a canonical reader has not asked for yet: ordinary shell type-ahead.
    Typeahead,
    /// A raw reader has left input unread past [`STALL_AFTER`] while spinning and drawing nothing.
    Stalled,
    /// Input is queued and the foreground job is stopped: nothing reads it until the job continues.
    Stopped,
}

impl InputWord {
    /// The wire word.
    #[must_use]
    #[inline]
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

/// The word for one reading.
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
        Liveness::Published { program_check } => !program_check && b.wait >= STALL_AFTER,
        Liveness::Watched {
            drew,
            spinning,
            program_check,
            contention,
        } => {
            !program_check
                && b.output_backlog == Some(0)
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

/// What the unread-input gate does with one input-writing control verb ([`gate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Gate {
    /// Write, and answer as ever.
    Open,
    /// Write, and say so.
    Warn,
    /// Write nothing: `ERR busy input-unread …`.
    Refuse,
}

/// The unread-input gate for one input-writing control verb.
#[must_use]
#[inline]
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

/// Raw input in the kernel's queue that has waited [`WARN_AFTER`] unread.
#[must_use]
#[inline]
pub fn lagging(b: &InputBacklog) -> bool {
    b.queued > 0 && !b.canonical && b.wait >= WARN_AFTER
}

#[cfg(test)]
#[path = "input_backlog_tests.rs"]
mod tests;
