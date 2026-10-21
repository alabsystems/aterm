// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thinning of the handshake's `ERR auth` audit line. Every refusal is answered and counted;
//! its `DENIED:` WARN is written on the 1st, 2nd, 4th, 8th … refusal of a run, with the run's
//! count, the kind's total since launch, and the peer's pid and program. Nothing is blocked
//! or delayed, and there is no fixed window or cap.
//!
//! A run is one repeating dialer: refusals of one kind from one peer program (each
//! `aterm ctl` is a fresh pid, so the program is compared). Each program keeps its own run
//! within a kind, and its first refusal of a kind is written at once. A run's second refusal
//! always joins it; after that, a refusal starts a new run, written at once, when the quiet
//! since the program's previous refusal exceeds twice the longer of the run's span and the
//! quiet that started the run. The second term lets a dialer refusing in bursts start one new
//! run on its first quiet and fold its later, no quieter, bursts into it.
//!
//! Twice, not once: a steady dialer's next gap exceeds its last about half the time, and
//! jitter must not restart a run. A kind keeps runs for its [`PROGRAMS_PER_KIND`] most recent
//! programs; once it has dropped one, a program with no run is not called a first.
//!
//! Why twice, not once: at a run's 2nd refusal its span is a single gap, and a
//! steady dialer's next gap is longer than its last about half the time — a
//! live instance measured a 517 ms gap after a 516 ms one and restarted the
//! run on that jitter alone (2026-09-29). Twice is the same doubling that
//! spaces the lines: a steady stream's gaps never reach twice its span, and a
//! refusal after the dialer went quiet for more than twice as long as it had
//! been at it is written, however many came before (measured 2026-09-29: with
//! a count that never restarted, a wrong-token guess from a new process 50 s
//! after three blind dials was the 5th of its kind and left no line at all).
//! A kind keeps runs for its [`PROGRAMS_PER_KIND`] most recent programs; once
//! it has dropped one, a program with no run may be one it dropped, so its
//! line no longer says it is the program's first.
//!
//! THE KINDS are the handshake's reasons (`first_line_scope`): no `AUTH`/`TOKEN`
//! line at all (a blind dial), a presented token that matches neither the
//! instance token nor a live edge (a wrong token — a guess is never merged with
//! blind discovery dials), and a presented token with no active terminal
//! context to check it against as an edge.
//!
//! THE FLEET'S REFUSALS ([`FLEET_REFUSALS`], `crate::fleet_listen`, and
//! [`FLEET_GATE_DENIALS`]) keep the same rule in runs of their own: every
//! remote peer's refusals are ONE run there ([`AuthDenials::record_network`]),
//! since a remote dialer can change its address at will where a local one
//! cannot change its user.
//!
//! DYNAMIC, NEVER A GATE: there is no fixed window or cap. The spacing grows
//! with the run's measured count and restarts on the run's measured quiet.
//! Nothing is blocked or delayed — the thinning only decides which refusals are
//! WRITTEN, and every one is counted.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A first line that is neither `AUTH <hex>` nor `TOKEN <hex> …`.
pub(crate) const NO_TOKEN: &str = "no AUTH or TOKEN line presented";
/// A presented token that is neither the instance token nor a live edge.
pub(crate) const WRONG_TOKEN: &str =
    "presented token matches neither the instance token nor a live edge";
/// A presented token with no active terminal context to check it as an edge.
pub(crate) const NO_CONTEXT: &str = "edge token presented with no active terminal context";

/// The refusal kinds `first_line_scope` answers with, each its own run;
/// anything else shares the last slot.
const KINDS: [&str; 3] = [NO_TOKEN, WRONG_TOKEN, NO_CONTEXT];

/// Who dialed, as the kernel names the peer when the refusal is recorded
/// (`aterm_uds::fdpass::peer_pid`: on Darwin the process that last used the
/// connection): the pid, and the program its argv[0] names. Either can be
/// unknown (a peer gone before it was read, a platform without the call).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Peer {
    pub(crate) pid: Option<u32>,
    pub(crate) program: Option<String>,
}

impl Peer {
    /// The connected peer of `stream`, read only when a refusal is recorded.
    pub(crate) fn of(stream: &aterm_uds::CtlStream) -> Self {
        let pid = aterm_uds::fdpass::peer_pid(stream).ok();
        let program = pid
            .and_then(|p| i32::try_from(p).ok())
            .and_then(crate::session_status::program::argv_head_of)
            .and_then(|(argv0, _)| crate::session_status::program::program_from_argv0(&argv0));
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

/// Refusal runs, one set per kind in [`KINDS`] plus a shared "other".
pub(crate) struct AuthDenials {
    kinds: Mutex<[KindState; KINDS.len() + 1]>,
}

/// This process's runs: the control server's handshake records through it.
pub(crate) static AUTH_DENIALS: AuthDenials = AuthDenials::new();

/// The fleet listener's refusals (`crate::fleet_listen`), counted apart from
/// the local socket's — a remote peer's flood is never a local program's run,
/// nor the other way round — and thinned by the network rule in a run of
/// their own.
pub(crate) static FLEET_REFUSALS: AuthDenials = AuthDenials::new();

/// The fleet gate's refusals of a proved peer's line (`control::fleet_refusal`),
/// thinned by the same network rule in a run of their own, apart from the
/// listener's refusals of peers that proved nothing.
#[cfg(any(unix, test))]
pub(crate) static FLEET_GATE_DENIALS: AuthDenials = AuthDenials::new();

impl AuthDenials {
    pub(crate) const fn new() -> Self {
        Self {
            kinds: Mutex::new([const { KindState::new() }; KINDS.len() + 1]),
        }
    }

    /// Count one refusal for `reason` from `peer` at `now`, and return its audit-line text
    /// when this one is to be written, or `None` when it is only counted.
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
            // A dropped program, or one never seen: only the most recent programs keep runs.
            Written::Peer { first: false } => format!(
                "{reason} (not among the last {PROGRAMS_PER_KIND} programs refused; {total} since \
                 launch; {peer})"
            ),
            Written::Quiet(quiet) => {
                format!("{reason} (again after {quiet:.1?} quiet; {total} since launch; {peer})")
            }
            // Never "in a row": other programs' refusals and short quiets fall inside a run.
            Written::Nth { count, span } => format!(
                "{reason} ({count} from this program in {span:.1?}, {total} since launch; {peer})"
            ),
        })
    }

    /// Count one refusal of a network listener at `now` and return
    /// its line — `line`, the refusal's own, which names the peer and why —
    /// when THIS one is to be written, with the run's count past the first, or
    /// `None` when it is only counted.
    ///
    /// Every peer's refusals are one run: a remote dialer can change its address at will (one
    /// IPv6 /64 holds 2^64), so a run per address would write every new one at once.
    pub(crate) fn record_network(&self, line: &str, now: Instant) -> Option<String> {
        let (written, total) = self.count(KINDS.len(), None, now)?;
        Some(match written {
            // One run, never dropped: a peer of its own is the first refusal.
            Written::First | Written::Peer { .. } => line.to_owned(),
            Written::Quiet(quiet) => {
                format!("{line} (again after {quiet:.1?} quiet; {total} since launch)")
            }
            Written::Nth { count, span } => {
                format!("{line} ({count} refused in {span:.1?}, {total} since launch)")
            }
        })
    }

    /// The run rule (module doc): count one refusal in `slot` from the dialer keyed `who` at
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
                // Certain only while no program's run was ever dropped: this
                // refusal's own drop is of another program.
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

    /// Refusals of `reason`'s kind since this instance started.
    #[cfg(test)]
    pub(crate) fn total(&self, reason: &str) -> u64 {
        let slot = KINDS
            .iter()
            .position(|k| *k == reason)
            .unwrap_or(KINDS.len());
        self.kinds.lock().unwrap_or_else(|p| p.into_inner())[slot].total
    }

    /// Refusals [`Self::record_network`] counted since this instance started.
    #[cfg(test)]
    pub(crate) fn network_total(&self) -> u64 {
        self.kinds.lock().unwrap_or_else(|p| p.into_inner())[KINDS.len()].total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(pid: u32, program: &str) -> Peer {
        Peer {
            pid: Some(pid),
            program: Some(program.to_string()),
        }
    }

    /// A steady stream of blind dials from one program writes its 1st, 2nd, 4th and 8th,
    /// the later ones with the run's count; another kind's first refusal is written at once.
    #[test]
    fn a_steady_stream_is_written_at_powers_of_two_per_kind() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let d = AuthDenials::new();
        // A fresh pid every call, as each `aterm ctl` run has: same program.
        let written: Vec<u64> = (1..=13_u64)
            .filter(|&n| {
                d.record(NO_TOKEN, &peer(1000 + n as u32, "aterm"), at(n))
                    .is_some()
            })
            .collect();
        assert_eq!(written, [1, 2, 4, 8]);

        // A jittery steady stream stays one run: a 3rd gap a tenth longer than the 2nd is
        // not quiet.
        let ms = |m: u64| t0 + Duration::from_millis(m);
        let d = AuthDenials::new();
        let p = peer(7, "aterm");
        assert!(d.record(NO_TOKEN, &p, ms(0)).is_some());
        assert!(d.record(NO_TOKEN, &p, ms(1000)).is_some());
        assert_eq!(
            d.record(NO_TOKEN, &p, ms(2100)),
            None,
            "jitter is not quiet"
        );
        assert!(d.record(NO_TOKEN, &p, ms(3000)).is_some(), "the run's 4th");

        let d = AuthDenials::new();
        let p = peer(7, "aterm");
        assert!(
            d.record(NO_TOKEN, &p, at(1)).is_some(),
            "the 1st is written"
        );
        assert!(
            d.record(NO_TOKEN, &p, at(2)).is_some(),
            "the 2nd is written"
        );
        assert_eq!(
            d.record(NO_TOKEN, &p, at(3)),
            None,
            "the 3rd is only counted"
        );
        assert_eq!(d.total(NO_TOKEN), 3, "but counted");
        let other = d
            .record(WRONG_TOKEN, &p, at(4))
            .expect("another kind at once");
        assert!(other.starts_with(WRONG_TOKEN), "{other}");
        assert!(d.record("something new", &p, at(5)).is_some());
    }

    /// Three dials a second apart, then two more 50 s later: the quiet starts a new run, so
    /// the 4th is written and says so, and the 5th is the new run's 2nd.
    #[test]
    fn quiet_longer_than_the_run_restarts_the_spacing() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let d = AuthDenials::new();
        let p = peer(7, "aterm");
        assert!(d.record(NO_TOKEN, &p, at(0)).is_some());
        assert!(d.record(NO_TOKEN, &p, at(1)).is_some());
        assert!(d.record(NO_TOKEN, &p, at(2)).is_none());
        assert!(
            d.record(NO_TOKEN, &p, at(52)).is_some(),
            "50 s quiet after a 2 s run starts a new run"
        );
        assert!(
            d.record(NO_TOKEN, &p, at(53)).is_some(),
            "the new run's 2nd"
        );
        // Quiet no more than twice the run's span keeps thinning: the new run
        // spans 52..53 (1 s); a refusal 2 s later is its 3rd, only counted.
        assert!(d.record(NO_TOKEN, &p, at(55)).is_none());
    }

    /// A different peer program is never folded into another's run: it is written at once,
    /// though its quiet (1 s) is no longer than the run.
    #[test]
    fn a_different_peer_program_starts_a_new_run() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let d = AuthDenials::new();
        for s in 0..4 {
            let _ = d.record(NO_TOKEN, &peer(100 + s as u32, "aterm"), at(s));
        }
        assert!(
            d.record(NO_TOKEN, &peer(21897, "python3"), at(4)).is_some(),
            "a new program is written at once"
        );
    }

    /// A dialer refusing in bursts (two 150 ms apart every 5 s) starts one new run on its
    /// first quiet and is then thinned: 60 refusals write a handful of lines.
    #[test]
    fn a_periodic_burst_is_thinned_after_its_first_quiet() {
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        let d = AuthDenials::new();
        let p = peer(7, "aterm");
        let written = (0..30_u64)
            .flat_map(|round| [round * 5000, round * 5000 + 150])
            .filter(|&m| d.record(NO_TOKEN, &p, ms(m)).is_some())
            .count();
        assert!(written <= 10, "60 refusals in bursts wrote {written} lines");
        assert_eq!(d.total(NO_TOKEN), 60, "every refusal counted");
        // The run spans 5.0 s..145.15 s; a refusal 11 s on joins it (span 151.15 s), and a
        // quiet more than twice that span is written at once.
        let last = 29 * 5000 + 150;
        assert!(d.record(NO_TOKEN, &p, ms(last + 11_000)).is_none());
        assert!(
            d.record(NO_TOKEN, &p, ms(last + 11_000 + 400_000))
                .is_some()
        );
    }

    /// Two programs taking turns are each thinned on their own: `aterm` and a
    /// python3 poller alternating a second apart, forty in all, write each
    /// program's 1st, 2nd, 4th, 8th and 16th — ten lines, not forty.
    #[test]
    fn alternating_programs_are_thinned_independently() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let d = AuthDenials::new();
        let written: Vec<(u64, &str)> = (0..40_u64)
            .map(|n| (n, if n % 2 == 0 { "aterm" } else { "python3" }))
            .filter(|&(n, program)| {
                d.record(NO_TOKEN, &peer(100 + n as u32, program), at(n))
                    .is_some()
            })
            .collect();
        for program in ["aterm", "python3"] {
            let nth: Vec<usize> = written
                .iter()
                .filter(|(_, p)| *p == program)
                .map(|(n, _)| *n as usize / 2 + 1)
                .collect();
            assert_eq!(nth, [1, 2, 4, 8, 16], "{program}");
        }
    }

    /// The kinds are the handshake's own words: each refusal the handshake
    /// answers lands in its own slot, and a blind dial, a wrong token and a
    /// token with no context to check it against are three different kinds.
    #[test]
    fn the_kinds_are_the_handshakes_reasons() {
        let refuse = |first: &str| {
            let Err(denial) = super::super::first_line_scope(first, "feedbeef", || None) else {
                panic!("`{first}` is refused");
            };
            denial
        };
        assert_eq!(refuse("sessions"), NO_TOKEN);
        assert_eq!(refuse("AUTH abab"), NO_CONTEXT);
        assert_eq!(refuse("TOKEN abab sessions"), NO_CONTEXT);
        assert_eq!(refuse("AUTHX abab"), NO_TOKEN);
    }

    /// THE AUDIT FLOOD: an unauthenticated peer that opens and closes a
    /// connection a millisecond, from a new address each time, writes the 1st,
    /// 2nd, 4th … of its refusals, each saying how many there were, not one line
    /// per connection. A refusal after a quiet longer than the run is written
    /// at once, and a run's first line is the refusal's own.
    #[test]
    fn a_flood_of_network_refusals_is_one_thinned_run() {
        let refusals = AuthDenials::new();
        let t0 = Instant::now();
        let line = |i: u64| {
            format!(
                "fleet: 198.51.100.{}:{} not admitted: received corrupt message",
                1 + i % 250,
                10_000 + i
            )
        };
        let written: Vec<String> = (0..1000_u64)
            .filter_map(|i| refusals.record_network(&line(i), t0 + Duration::from_millis(i)))
            .collect();
        assert_eq!(
            written.len(),
            10,
            "the 1st, 2nd, 4th … 512th of 1000 refusals: {written:#?}"
        );
        assert_eq!(written[0], line(0), "the first refusal is written as it is");
        assert_eq!(
            written[9],
            format!("{} (512 refused in 511.0ms, 512 since launch)", line(511))
        );
        assert_eq!(refusals.network_total(), 1000);

        // Ten minutes of quiet: the next refusal starts a new run, written.
        let later = t0 + Duration::from_secs(600);
        let again = refusals
            .record_network("fleet: 198.51.100.251:4242 not admitted: no proof", later)
            .expect("a refusal after a long quiet is written");
        assert!(
            again.starts_with("fleet: 198.51.100.251:4242 not admitted: no proof (again after ")
                && again.ends_with("quiet; 1001 since launch)"),
            "{again}"
        );
    }
}
