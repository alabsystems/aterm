// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Power-of-two thinning for control-socket and fleet handshake refusal logs.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Handshake refusal reason when the first line is neither `AUTH` nor `TOKEN`.
pub(crate) const NO_TOKEN: &str = "no AUTH or TOKEN line presented";
/// Handshake refusal reason when a token matches neither the instance token nor a live edge.
pub(crate) const WRONG_TOKEN: &str =
    "presented token matches neither the instance token nor a live edge";
/// Handshake refusal reason when an edge token is presented without an active terminal context.
pub(crate) const NO_CONTEXT: &str = "edge token presented with no active terminal context";

/// The refusal kinds `first_line_scope` answers with, each its own run;
/// anything else shares the last slot.
const KINDS: [&str; 3] = [NO_TOKEN, WRONG_TOKEN, NO_CONTEXT];

/// Connected peer identity (`pid` and program name) recorded on a refusal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Peer {
    pub(crate) pid: Option<u32>,
    pub(crate) program: Option<String>,
}

impl Peer {
    /// Reads the connected peer identity of `stream`.
    pub(crate) fn of(stream: &aterm_uds::CtlStream) -> Self {
        let pid = aterm_uds::fdpass::peer_pid(stream).ok();
        let program = pid.and_then(|p| i32::try_from(p).ok()).and_then(|pid| {
            let (argv0, _) = crate::session_status::program::argv_head_of(pid)?;
            crate::session_status::program::program_from_argv0(
                &argv0,
                &crate::session_status::program::LeaderImage::of(pid),
            )
        });
        Self { pid, program }
    }

    fn describe(&self) -> String {
        let pid = self
            .pid
            .map_or_else(|| "unknown".to_string(), |p| p.to_string());
        let program = self.program.as_deref().unwrap_or("unknown");
        format!("peer pid {pid} program {program}")
    }
}

/// How many peer programs one kind keeps a run for; the least recently
/// refused is dropped past it (and starts over, written at once, if it
/// returns).
const PROGRAMS_PER_KIND: usize = 8;

/// One peer program's run within a kind.
struct Run {
    /// The peer program (`None`: unknown).
    program: Option<String>,
    /// Refusals in the current run.
    count: u64,
    /// The current run's first refusal.
    start: Instant,
    /// This program's latest refusal of the kind.
    last: Instant,
    /// The quiet that started the current run (zero for a program's first):
    /// a periodic dialer's next burst is no quieter than its last, so it
    /// joins the run instead of restarting it.
    restart_gap: Duration,
}

/// One kind's refusals: its total, and one run per recent peer program.
struct KindState {
    /// Refusals of this kind since this instance started.
    total: u64,
    runs: Vec<Run>,
    /// A program's run was dropped past [`PROGRAMS_PER_KIND`]: a program with
    /// no run is no longer certainly refusing for the first time.
    dropped: bool,
}

impl KindState {
    const fn new() -> Self {
        Self {
            total: 0,
            runs: Vec::new(),
            dropped: false,
        }
    }
}

/// Why a refusal's line is written: what it says of the run it is in.
enum Written {
    /// The kind's first refusal since launch.
    First,
    /// A program with no run: `first` when the kind never dropped one, so it
    /// is certainly the program's first refusal of the kind.
    Peer { first: bool },
    /// A new run after this long a quiet.
    Quiet(Duration),
    /// The run's `count`th refusal, a power of two, `span` after its first.
    Nth { count: u64, span: Duration },
}

/// Per-kind handshake refusal counters and per-program run state.
pub(crate) struct AuthDenials {
    kinds: Mutex<[KindState; KINDS.len() + 1]>,
}

/// Local control-socket handshake refusal tracker.
pub(crate) static AUTH_DENIALS: AuthDenials = AuthDenials::new();

/// Fleet listener network refusal tracker.
pub(crate) static FLEET_REFUSALS: AuthDenials = AuthDenials::new();

/// Fleet gate authorization refusal tracker.
#[cfg(any(unix, test))]
pub(crate) static FLEET_GATE_DENIALS: AuthDenials = AuthDenials::new();

impl AuthDenials {
    pub(crate) const fn new() -> Self {
        Self {
            kinds: Mutex::new([const { KindState::new() }; KINDS.len() + 1]),
        }
    }

    /// Records a refusal for `reason` from `peer` at `now`, returning the audit line if due.
    pub(crate) fn record(&self, reason: &str, peer: &Peer, now: Instant) -> Option<String> {
        let slot = KINDS
            .iter()
            .position(|k| *k == reason)
            .unwrap_or(KINDS.len());
        let (written, total) = self.count(slot, peer.program.as_deref(), now)?;
        let peer = peer.describe();
        Some(match written {
            Written::First => format!("{reason} ({peer})"),
            Written::Peer { first: true } => {
                format!("{reason} (first from this program; {total} since launch; {peer})")
            }
            Written::Peer { first: false } => format!(
                "{reason} (not among the last {PROGRAMS_PER_KIND} programs refused; {total} since \
                 launch; {peer})"
            ),
            Written::Quiet(quiet) => {
                format!("{reason} (again after {quiet:.1?} quiet; {total} since launch; {peer})")
            }
            Written::Nth { count, span } => format!(
                "{reason} ({count} from this program in {span:.1?}, {total} since launch; {peer})"
            ),
        })
    }

    /// Records a network listener refusal at `now`, returning the audit line if due.
    pub(crate) fn record_network(&self, line: &str, now: Instant) -> Option<String> {
        let (written, total) = self.count(KINDS.len(), None, now)?;
        Some(match written {
            Written::First | Written::Peer { .. } => line.to_owned(),
            Written::Quiet(quiet) => {
                format!("{line} (again after {quiet:.1?} quiet; {total} since launch)")
            }
            Written::Nth { count, span } => {
                format!("{line} ({count} refused in {span:.1?}, {total} since launch)")
            }
        })
    }

    /// The run rule: count one refusal in `slot` from the dialer keyed `who` at
    /// `now`, and return why it is written with the slot's total, or `None` when it is only
    /// counted.
    fn count(&self, slot: usize, who: Option<&str>, now: Instant) -> Option<(Written, u64)> {
        let mut kinds = self.kinds.lock().unwrap_or_else(|p| p.into_inner());
        let k = &mut kinds[slot];
        k.total = k.total.saturating_add(1);
        let total = k.total;
        let fresh = |restart_gap| Run {
            program: who.map(str::to_owned),
            count: 1,
            start: now,
            last: now,
            restart_gap,
        };
        let why = match k.runs.iter().position(|r| r.program.as_deref() == who) {
            None => {
                let first = !k.dropped;
                if k.runs.len() >= PROGRAMS_PER_KIND
                    && let Some(oldest) = (0..k.runs.len()).min_by_key(|&i| k.runs[i].last)
                {
                    k.runs.swap_remove(oldest);
                    k.dropped = true;
                }
                k.runs.push(fresh(Duration::ZERO));
                if total == 1 {
                    Written::First
                } else {
                    Written::Peer { first }
                }
            }
            Some(i) => {
                let r = &mut k.runs[i];
                let quiet = now.saturating_duration_since(r.last);
                let span = r.last.saturating_duration_since(r.start);
                if r.count >= 2 && quiet > span.max(r.restart_gap).saturating_mul(2) {
                    *r = fresh(quiet);
                    Written::Quiet(quiet)
                } else {
                    r.count = r.count.saturating_add(1);
                    r.last = now;
                    if !r.count.is_power_of_two() {
                        return None;
                    }
                    Written::Nth {
                        count: r.count,
                        span: now.saturating_duration_since(r.start),
                    }
                }
            }
        };
        Some((why, total))
    }

    /// Returns total refusals of `reason`'s kind since startup.
    #[cfg(test)]
    pub(crate) fn total(&self, reason: &str) -> u64 {
        let slot = KINDS
            .iter()
            .position(|k| *k == reason)
            .unwrap_or(KINDS.len());
        self.kinds.lock().unwrap_or_else(|p| p.into_inner())[slot].total
    }

    /// Returns total network refusals recorded since startup.
    #[cfg(test)]
    pub(crate) fn network_total(&self) -> u64 {
        self.kinds.lock().unwrap_or_else(|p| p.into_inner())[KINDS.len()].total
    }
}

#[cfg(test)]
#[path = "control_denials_tests.rs"]
mod tests;
