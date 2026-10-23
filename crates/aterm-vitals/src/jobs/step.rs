// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-tick job classification, hysteresis, and stuck verdicts.
//! Implements [`classify_tick`] and [`NO_PROGRESS_CPU_NS_PER_MIN`].

use std::collections::VecDeque;

use super::{
    Class, Consumer, InputGate, JobKind, MU_FLOOR_MS, MU_PRIOR_MS, MuFrom, NEVER_RETURNS_MS,
    NO_PROGRESS_CPU_NS_PER_MIN, OWN_MIN, OWN_RING, PHI_QUIET_LEAVE_MILLI, PHI_QUIET_MILLI,
    PHI_STUCK_MILLI, RUNNING_WINDOW_MS, Standing, Stop, Verdict, Wait, WaitName, Zombies, median,
    phi_milli,
};
use crate::jobs::GateWord;

/// Page-wait milliseconds attributed to one page-in.
pub const PAGEIN_WAIT_MS: u64 = 1;

/// Channel that produced a progress event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Progress {
    /// PTY output in canonical mode carried a line feed.
    Line,
    /// PTY output in raw mode (a full-screen program redraws without LF).
    Raw,
    /// The output file grew and the new bytes hold a line feed.
    FileLine,
    /// The output file grew where its bytes may not be read (`lf=unverified`).
    FileUnverified,
    /// Writes to anything else above the output channel's share.
    SideWrites,
    /// Children born or reaped, with work.
    ChurnWithWork,
    /// The job read typed input.
    InputConsumed,
}

impl Progress {
    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Raw => "raw",
            Self::FileLine => "file-line",
            Self::FileUnverified => "file-unverified",
            Self::SideWrites => "side-writes",
            Self::ChurnWithWork => "churn-with-work",
            Self::InputConsumed => "input-consumed",
        }
    }
}

/// How the job counted this tick, as the model's `cls` reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Effective {
    /// Progress within the running window.
    #[default]
    Running,
    /// A legitimate class, in effect (held 2 ticks after a suspect run).
    Legit,
    /// A stuck-capable suspect.
    Capable,
    /// A suspect capped at quiet.
    Capped,
    /// The unread-input gate's word decides (published or pending).
    Gate,
    /// No live member.
    Exited,
}

/// What one tick saw: read by the conformance projection and by a person asking why.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tick {
    /// The progress channel this tick credited.
    pub progress: Option<Progress>,
    /// A member's reading was refused: the counters are unknown.
    pub refused: bool,
    /// μ (as stretched) grew since the last suspect tick, so φ fell.
    pub mu_grew: bool,
    /// How the job counted.
    pub effective: Effective,
}

/// Defect injected by the conformance test's negative controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fault {
    /// Raw output bytes count as progress (a CR spinner's redraw).
    RawBytesProgress,
    /// Births and reaps count without work.
    ChurnWithoutWork,
    /// Any logical write above zero counts (M7's noise).
    AnyLogicalWrites,
    /// φ accrues on a legitimate wait.
    AccrueOnLegit,
    /// A refused reading resets the quiet clock.
    RefusedResetsAge,
    /// A capped (unnamed) wait reaches stuck.
    UnnamedReachesStuck,
    /// A named stuck-capable wait is never raised to stuck.
    NeverRaiseNamed,
}

/// The evidence one tick of one job hands the step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    /// The host clock, ms.
    pub at_ms: u64,
    /// Since this job's previous tick, ms (0 on its first).
    pub dt_ms: u64,
    /// No live member, or the unit closed.
    pub exited: bool,
    /// A progress event and when it happened.
    pub progress: Option<(Progress, u64)>,
    /// A live member's counters were refused this tick.
    pub refused: bool,
    /// Some live member's CPU over this tick is unknown.
    pub cpu_unread: bool,
    /// Σ ΔCPU over the members, ns; `None` where unread.
    pub cpu_ns: Option<u64>,
    /// Σ Δ(system calls + wakeups); `None` where unread.
    pub calls: Option<u64>,
    /// A member was born or reaped this tick.
    pub churn: bool,
    /// ...with work (child CPU ≥ 50 ms/s, or the children wrote).
    pub churn_work: bool,
    /// Σ Δpage-ins.
    pub pageins: Option<u64>,
    /// A stopped member.
    pub stopped: Option<Stop>,
    /// The job holds the terminal's foreground.
    pub holds_fg: bool,
    /// The unread-input gate.
    pub gate: InputGate,
    /// Zombies first seen ≥ 10 s ago whose parent is a live member.
    pub zombies: Option<Zombies>,
    /// The Tier A wait of the waiting leaves.
    pub wait: Wait,
    /// A Tier B naming holding this episode, and how it counts.
    pub named: Option<Standing>,
}

/// The context one step reads beyond the evidence.
#[derive(Clone, Copy, Debug)]
pub struct Cx {
    /// Who waits for the job.
    pub consumer: Consumer,
    /// Its unit kind.
    pub kind: JobKind,
    /// `Contention::factor_milli`.
    pub stretch_milli: u32,
    /// The machine thrashes.
    pub thrashing: bool,
    /// The command class's learned μ.
    pub class_mu: Option<u64>,
    /// An injected defect (negative controls only).
    pub fault: Option<Fault>,
}

/// What the step decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Out {
    /// The class.
    pub class: Class,
    /// The shown wait, for a waiting job.
    pub wait: Option<Wait>,
    /// The stopped member, for a stopped job.
    pub stop: Option<Stop>,
    /// How it counts.
    pub standing: Option<Standing>,
    /// The quiet age, ms.
    pub quiet_ms: Option<u64>,
    /// φ × 1000.
    pub phi_milli: Option<u64>,
    /// μ as stretched, ms.
    pub mu_ms: Option<u64>,
    /// μ's source.
    pub mu_from: Option<MuFrom>,
    /// The verdict.
    pub verdict: Verdict,
    /// The no-progress word beside it.
    pub no_progress: bool,
    /// The episode.
    pub episode: u32,
    /// What the tick saw.
    pub tick: Tick,
}

/// A 2:1 hysteresis band entered and left after 2 agreeing ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Band {
    on: bool,
    streak: u8,
}

impl Band {
    fn feed(&mut self, enter: bool, leave: bool) {
        let moving = if self.on { leave } else { enter };
        if moving {
            self.streak += 1;
            if self.streak >= 2 {
                self.on = !self.on;
                self.streak = 0;
            }
        } else {
            self.streak = 0;
        }
    }
}

/// One job's classifier state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    /// When the job opened, host ms.
    pub opened_ms: u64,
    /// Its last progress event.
    pub last_progress_ms: Option<u64>,
    intervals: VecDeque<u64>,
    suspect_since_ms: u64,
    legit_streak: u32,
    effective: Option<Effective>,
    capable: bool,
    cap: Option<Verdict>,
    busy: Band,
    polling: Band,
    thrash: Band,
    shown: Option<Wait>,
    cand: Option<WaitName>,
    cand_streak: u8,
    /// The verdict.
    pub verdict: Verdict,
    /// Episodes opened so far.
    pub episode: u32,
    episode_open: bool,
    follow_since: Option<u64>,
    prev_mu_eff: Option<u64>,
    /// The start of the suspect run `run_cpu` sums over.
    run_cpu_from: Option<u64>,
    /// Σ ΔCPU over the members during that run, ns; `None` once a reading in it was unknown.
    run_cpu: Option<u64>,
}

impl Track {
    /// A job opened at `at_ms`.
    #[must_use]
    pub fn new(at_ms: u64) -> Self {
        Self {
            opened_ms: at_ms,
            suspect_since_ms: at_ms,
            ..Self::default()
        }
    }

    /// The median of its own progress intervals once it has [`OWN_MIN`].
    #[must_use]
    pub fn own_mu(&self) -> Option<u64> {
        (self.intervals.len() >= OWN_MIN).then(|| median(self.intervals.iter().copied()))
    }

    /// Whether a suspect episode is open.
    #[must_use]
    pub const fn episode_open(&self) -> bool {
        self.episode_open
    }

    fn close_episode(&mut self) {
        self.episode_open = false;
    }

    fn say(&mut self, v: Verdict) -> Verdict {
        if v.is_suspect() && !self.episode_open {
            self.episode += 1;
            self.episode_open = true;
        }
        self.verdict = v;
        v
    }

    /// Where the suspect quiet clock starts.
    fn suspect_from(&self) -> u64 {
        self.last_progress_ms
            .unwrap_or(0)
            .max(self.suspect_since_ms)
    }

    /// Fold one tick's CPU into the suspect run's sum.
    fn note_run_cpu(&mut self, ev: &Evidence) {
        let from = self.suspect_from();
        if self.run_cpu_from != Some(from) {
            self.run_cpu_from = Some(from);
            self.run_cpu = Some(0);
        }
        if ev.at_ms > from {
            let read = ev.cpu_ns.filter(|_| !ev.cpu_unread);
            self.run_cpu = self.run_cpu.zip(read).map(|(s, c)| s.saturating_add(c));
        }
    }
}

/// The no-progress word ([`NO_PROGRESS_CPU_NS_PER_MIN`]).
fn no_progress(out: &Out, cx: &Cx, run_cpu: Option<u64>) -> bool {
    let agent = cx.kind == JobKind::AgentCall
        && matches!(
            cx.consumer,
            Consumer::AgentFg | Consumer::AgentBg | Consumer::Agent
        );
    let (Some(quiet), Some(phi), Some(cpu)) = (out.quiet_ms, out.phi_milli, run_cpu) else {
        return false;
    };
    agent
        && out.verdict == Verdict::Quiet
        && phi >= PHI_STUCK_MILLI
        && u128::from(cpu) * 60_000 < u128::from(quiet) * u128::from(NO_PROGRESS_CPU_NS_PER_MIN)
}

fn fault(cx: &Cx, f: Fault) -> bool {
    cx.fault == Some(f)
}

/// One tick of one job.
pub fn classify_tick(t: &mut Track, ev: &Evidence, cx: &Cx) -> Out {
    let at = ev.at_ms;
    let mut tick = Tick {
        refused: ev.refused,
        ..Tick::default()
    };
    let mut out = Out {
        class: Class::Exited,
        wait: None,
        stop: None,
        standing: None,
        quiet_ms: None,
        phi_milli: None,
        mu_ms: None,
        mu_from: None,
        verdict: Verdict::Exited,
        no_progress: false,
        episode: t.episode,
        tick,
    };
    if ev.exited {
        t.close_episode();
        t.verdict = Verdict::Exited;
        t.effective = Some(Effective::Exited);
        out.tick.effective = Effective::Exited;
        return out;
    }

    if let Some((p, when)) = ev.progress {
        tick.progress = Some(p);
        let when = when.min(at);
        if let Some(last) = t.last_progress_ms
            && when > last
        {
            t.intervals.push_back(when - last);
            while t.intervals.len() > OWN_RING {
                t.intervals.pop_front();
            }
        }
        if t.last_progress_ms.is_none_or(|l| when > l) {
            t.last_progress_ms = Some(when);
        }
    }
    if ev.refused && fault(cx, Fault::RefusedResetsAge) {
        t.suspect_since_ms = at;
    }

    if !ev.refused && ev.dt_ms > 0 {
        if let Some(cpu) = ev.cpu_ns {
            let wall_ns = u128::from(ev.dt_ms) * 1_000_000;
            let fair = wall_ns * 1000 / u128::from(cx.stretch_milli.max(1000));
            let cpu = u128::from(cpu);
            t.busy.feed(cpu * 2 >= fair, cpu * 4 < fair);
            let per_s = ev
                .calls
                .map_or(0, |c| u128::from(c) * 1000 / u128::from(ev.dt_ms));
            let idle_churn = ev.churn && !ev.churn_work;
            t.polling.feed(
                (per_s >= 20 && cpu * 2 < fair) || idle_churn,
                per_s < 10 && !idle_churn,
            );
        }
        if let Some(pi) = ev.pageins {
            let wait_per_s = pi * PAGEIN_WAIT_MS * 1000 / ev.dt_ms;
            t.thrash.feed(
                cx.thrashing && wait_per_s >= 100,
                !cx.thrashing || wait_per_s < 50,
            );
        }
    }

    let holds_fg = ev.holds_fg;
    let published = holds_fg && matches!(ev.gate.word, GateWord::Stalled | GateWord::Stopped);
    let running = t
        .last_progress_ms
        .is_some_and(|l| at.saturating_sub(l) <= RUNNING_WINDOW_MS);
    let tty_wait = holds_fg && matches!(ev.wait.name, WaitName::TtyRaw | WaitName::TtyPassword);
    let class = if published {
        Class::UnreadInput
    } else if running {
        Class::Running
    } else if ev.stopped.is_some() {
        Class::Stopped
    } else if holds_fg && ev.gate.word == GateWord::Pending {
        Class::UnreadInput
    } else if t.thrash.on {
        Class::Thrashing
    } else if t.busy.on {
        Class::BusySilent
    } else if t.polling.on && !tty_wait {
        Class::Polling
    } else if ev.zombies.is_some() {
        Class::Zombie
    } else {
        Class::Waiting
    };
    out.class = class;

    if class == Class::Waiting {
        let cand = ev.wait.clone();
        match &t.shown {
            Some(s) if s.name != cand.name => {
                if t.cand == Some(cand.name) {
                    t.cand_streak += 1;
                } else {
                    t.cand = Some(cand.name);
                    t.cand_streak = 1;
                }
                if t.cand_streak >= 2 {
                    t.shown = Some(cand);
                    t.cand = None;
                    t.cand_streak = 0;
                }
            }
            _ => {
                t.shown = Some(cand);
                t.cand = None;
                t.cand_streak = 0;
            }
        }
    } else {
        t.cand = None;
        t.cand_streak = 0;
    }
    let shown = if class == Class::Waiting {
        t.shown.clone()
    } else {
        None
    };
    let follows = shown
        .as_ref()
        .is_some_and(|w| matches!(w.name, WaitName::Follow | WaitName::Listen))
        && cx.consumer == Consumer::AgentFg;
    if follows {
        t.follow_since.get_or_insert(at);
    } else {
        t.follow_since = None;
    }

    let (raw, legit_verdict, cap) = match class {
        Class::UnreadInput => (Effective::Gate, Verdict::Fine, None),
        Class::Running => (Effective::Running, Verdict::Fine, None),
        Class::Stopped => {
            if ev.stopped.as_ref().is_some_and(|s| s.parked) {
                (Effective::Legit, Verdict::Parked, None)
            } else {
                (Effective::Capable, Verdict::Fine, Some(Verdict::Quiet))
            }
        }
        Class::Thrashing => (Effective::Capped, Verdict::Fine, Some(Verdict::Slow)),
        Class::BusySilent => (Effective::Capped, Verdict::Fine, Some(Verdict::Busy)),
        Class::Polling | Class::Zombie => (Effective::Capable, Verdict::Fine, Some(Verdict::Quiet)),
        Class::Waiting | Class::Exited => {
            let w = shown.as_ref().unwrap_or(&ev.wait);
            let standing = ev.named.unwrap_or(w.standing);
            match standing {
                Standing::Legit => {
                    let v = if w.name == WaitName::TtyPassword && cx.consumer == Consumer::PersonFg
                    {
                        Verdict::WaitingYou
                    } else if follows && t.follow_since.is_some_and(|s| at - s >= NEVER_RETURNS_MS)
                    {
                        Verdict::NeverReturns
                    } else {
                        Verdict::Fine
                    };
                    (Effective::Legit, v, None)
                }
                Standing::Capable => (Effective::Capable, Verdict::Fine, Some(Verdict::Quiet)),
                Standing::Capped => (Effective::Capped, Verdict::Fine, Some(Verdict::Quiet)),
            }
        }
    };
    out.wait = shown;
    out.stop = if class == Class::Stopped {
        ev.stopped.clone()
    } else {
        None
    };

    if cx.consumer == Consumer::Nobody {
        t.effective = Some(raw);
        t.close_episode();
        t.verdict = Verdict::Fine;
        out.verdict = Verdict::Fine;
        tick.effective = raw;
        out.tick = tick;
        return out;
    }

    let was_suspect = matches!(t.effective, Some(Effective::Capable | Effective::Capped));
    let eff = if raw == Effective::Legit {
        t.legit_streak += 1;
        if was_suspect && t.legit_streak < 2 {
            if t.capable {
                Effective::Capable
            } else {
                Effective::Capped
            }
        } else {
            Effective::Legit
        }
    } else {
        t.legit_streak = 0;
        raw
    };
    let cap = if raw == Effective::Legit { t.cap } else { cap };
    tick.effective = eff;

    let leaving_gate = t.effective == Some(Effective::Gate) && eff != Effective::Gate;
    if leaving_gate {
        t.verdict = Verdict::Fine;
        t.close_episode();
    }
    let age = at.saturating_sub(t.last_progress_ms.unwrap_or(0).max(t.opened_ms));

    match eff {
        Effective::Exited => {}
        Effective::Running => {
            t.close_episode();
            t.verdict = Verdict::Fine;
            t.prev_mu_eff = None;
            out.verdict = Verdict::Fine;
            out.standing = None;
        }
        Effective::Gate => {
            out.input_gate_verdict(t, published);
        }
        Effective::Legit => {
            t.close_episode();
            t.verdict = legit_verdict;
            t.prev_mu_eff = None;
            out.verdict = legit_verdict;
            out.standing = Some(Standing::Legit);
            out.quiet_ms = Some(age);
            if fault(cx, Fault::AccrueOnLegit) {
                let quiet = at.saturating_sub(t.suspect_from());
                let (mu, _) = mu_of(t, cx);
                out.phi_milli = Some(phi_milli(quiet, mu));
            } else {
                out.phi_milli = Some(0);
            }
        }
        Effective::Capable | Effective::Capped => {
            if matches!(t.effective, Some(Effective::Legit)) {
                t.suspect_since_ms = at;
            }
            let capable = eff == Effective::Capable;
            t.capable = capable;
            t.cap = cap;
            let quiet = at.saturating_sub(t.suspect_from());
            let (mu, from) = mu_of(t, cx);
            let mu_eff = match class {
                Class::BusySilent | Class::Polling => {
                    mu.saturating_mul(u64::from(cx.stretch_milli.clamp(1000, 10_000))) / 1000
                }
                Class::Thrashing => {
                    let share = ev.pageins.map_or(0, |pi| {
                        (pi * PAGEIN_WAIT_MS * 1000 / ev.dt_ms.max(1)).min(900)
                    });
                    mu.saturating_mul(1000) / (1000 - share)
                }
                _ => mu,
            };
            tick.mu_grew = t.prev_mu_eff.is_some_and(|p| mu_eff > p);
            t.prev_mu_eff = Some(mu_eff);
            let phi = phi_milli(quiet, mu_eff);
            let cap_word = cap.unwrap_or(Verdict::Quiet);
            let reaches_stuck = (capable && !fault(cx, Fault::NeverRaiseNamed))
                || (!capable && fault(cx, Fault::UnnamedReachesStuck));
            let prev = if t.verdict.is_suspect() {
                t.verdict
            } else {
                Verdict::Fine
            };
            let v = match prev {
                Verdict::Stuck => {
                    if phi < PHI_QUIET_MILLI {
                        Verdict::Fine
                    } else {
                        Verdict::Stuck
                    }
                }
                Verdict::Quiet | Verdict::Busy | Verdict::Slow => {
                    if phi < PHI_QUIET_LEAVE_MILLI {
                        Verdict::Fine
                    } else if reaches_stuck && phi >= PHI_STUCK_MILLI {
                        Verdict::Stuck
                    } else {
                        cap_word
                    }
                }
                _ => {
                    if reaches_stuck && phi >= PHI_STUCK_MILLI {
                        Verdict::Stuck
                    } else if phi >= PHI_QUIET_MILLI {
                        cap_word
                    } else {
                        Verdict::Fine
                    }
                }
            };
            if v == Verdict::Fine {
                t.close_episode();
                t.verdict = Verdict::Fine;
            } else {
                t.say(v);
            }
            out.verdict = v;
            out.standing = Some(if capable {
                Standing::Capable
            } else {
                Standing::Capped
            });
            out.quiet_ms = Some(quiet);
            out.phi_milli = Some(phi);
            out.mu_ms = Some(mu_eff);
            out.mu_from = Some(from);
        }
    }
    t.note_run_cpu(ev);
    out.no_progress = no_progress(&out, cx, t.run_cpu);
    t.effective = Some(eff);
    out.episode = t.episode;
    out.tick = tick;
    out
}

/// μ and its source: the job's own median, else its class's, else the prior, never under the floor.
fn mu_of(t: &Track, cx: &Cx) -> (u64, MuFrom) {
    let (mu, from) = match (t.own_mu(), cx.class_mu) {
        (Some(own), _) => (own, MuFrom::Own),
        (None, Some(class)) => (class, MuFrom::Class),
        (None, None) => (MU_PRIOR_MS, MuFrom::Prior),
    };
    (mu.max(MU_FLOOR_MS), from)
}

impl Out {
    /// The gate's verdict: a published stall is stuck (the gate's own word, no φ); pending is fine.
    fn input_gate_verdict(&mut self, t: &mut Track, published: bool) {
        t.prev_mu_eff = None;
        if published {
            let v = t.say(Verdict::Stuck);
            self.verdict = v;
        } else {
            t.close_episode();
            t.verdict = Verdict::Fine;
            self.verdict = Verdict::Fine;
        }
        self.standing = Some(if published {
            Standing::Capable
        } else {
            Standing::Legit
        });
    }
}
