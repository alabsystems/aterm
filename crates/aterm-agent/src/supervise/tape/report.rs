// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A REPLAY SAYS ([`Report`]): how a strict replay of a harness tape
//! ended, the first place it stopped reproducing the recorded run if it did,
//! and what the replayed loop asked and printed — as values a test asserts
//! on and a person reads, never as prose a test pins.
//!
//! **The verdicts** (the replay plan's §1, each with its exit code):
//! * [`Verdict::Reproduced`] (0): every record was consumed, each request and
//!   output was the recorded one, and the loop ended as the recorded loop
//!   ended;
//! * [`Verdict::Ended`] (0): the tape has no end — a run killed, or a tape
//!   torn or capped — and everything up to where it stops reproduced;
//! * [`Verdict::Reached`] (0): `--until door=N` stopped the replay after
//!   effect `#N`, everything up to it reproduced;
//! * [`Verdict::Diverged`] (1): the first [`Divergence`] — in a
//!   counterfactual (acts mode, [`Mode::Acts`]) the first divergent ACT, both
//!   sides said ([`Cause::ActDiffers`]), or the act the candidate went on
//!   without ([`Cause::ActSkipped`]);
//! * [`Verdict::Stale`] (3): a counterfactual asked a read (or a wait) its
//!   segment of the tape never answered ([`Cause::Stale`]): what the
//!   candidate would have seen there is not on the tape;
//! * [`Verdict::Panicked`] (4): the recorded run panicked, and the replay
//!   panicked with the same message once every record was consumed — or,
//!   for a run that died inside a door's live call (a probe's body, the
//!   transport), reached that door asking the very request the tape says it
//!   died at, every record before it consumed.
//!
//! A tape that cannot be replayed at all — unreadable, damaged inside, of a
//! newer format, a header this build cannot read, a loop this build does
//! not replay — is no report: the driver refuses it (exit 2).
//!
//! **The divergence classes** (Design 1, the plan's §5.2), for the first
//! divergence only — everything after it is not run:
//! * [`Class::Decision`]: an ACT differs (a key, a send, a post, a badge, a
//!   signal, a restart: `tape::ctl::is_act`), an act is asked where the tape
//!   holds something else or the tape holds one where the loop asks
//!   something else — or a DECISION'S MARK differs (`decide.*`: the policy
//!   decided otherwise on the same screen, which is what an act differing
//!   would follow from);
//! * [`Class::Output`]: an output or a write differs while the doors agree —
//!   a printed line, a journal or ledger row, a notes line, the ledger's
//!   bound — or the loop ended otherwise than it did;
//! * [`Class::Drift`]: anything else — a read asked in another order, a
//!   clock read more or less often, a reply this build cannot decode, the
//!   loop asking past the tape's end or ending with records left.
//!
//! Strict replay is for re-executing a run on the build that recorded it;
//! on another build every divergence reads as "behaviour changed since the
//! recording's build", which [`Report::render`] says beside both builds.
//!
//! A COUNTERFACTUAL's report ([`ActsReport`]) adds the candidate policy, the
//! recorded and the candidate act at the first divergent one with the
//! decision before each, the last screen the candidate read, how far it
//! strayed from the tape ([`super::acts::ActsAccount`]) and the [`BOUND`]
//! line: what no counterfactual can say.

use std::fmt::Write as _;

/// How a replay ended ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Reproduced,
    Ended,
    /// `--until door=N` stopped it after effect `#door`.
    Reached {
        door: u64,
    },
    Diverged(Divergence),
    /// A COUNTERFACTUAL (acts mode) asked a read its segment of the tape
    /// never answered, and nothing before the cursor may stand for: the
    /// tape cannot say what the candidate would have seen (exit 3).
    Stale(Divergence),
    /// The recorded run unwound with `message`. `door`: it died inside that
    /// door's live call (a probe's body, the transport), and the replay
    /// reached the door asking the very request — a replay makes no live
    /// call, so the panic itself is the tape's; `None`: it died above every
    /// door, and the replay panicked with the same message.
    Panicked {
        message: String,
        door: Option<String>,
    },
}

impl Verdict {
    /// The word a report leads with.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Reproduced => "REPRODUCED",
            Self::Ended => "ENDED",
            Self::Reached { .. } => "REACHED",
            Self::Diverged(_) => "DIVERGED",
            Self::Stale(_) => "STALE",
            Self::Panicked { .. } => "PANICKED",
        }
    }

    /// The exit code `aterm drive replay` ends with (the plan's §1).
    #[must_use]
    pub fn code(&self) -> u8 {
        match self {
            Self::Reproduced | Self::Ended | Self::Reached { .. } => 0,
            Self::Diverged(_) => 1,
            Self::Stale(_) => 3,
            Self::Panicked { .. } => 4,
        }
    }
}

/// Which kind of difference the first divergence is ([module docs](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Decision,
    Output,
    Drift,
}

impl Class {
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Output => "output",
            Self::Drift => "drift",
        }
    }
}

/// What the first divergence was, as a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    /// astream's refusal at the door ([`astream_effects::ReplayDivergence`]):
    /// a kind, path, door, request or output that is not the recorded one,
    /// or a tape that ended under the loop.
    Refused(astream_effects::ReplayDivergence),
    /// The recorded reply at `door` does not decode in this build: the
    /// value's codec changed since the recording.
    Undecodable { door: String, why: String },
    /// The loop read flags that consume nothing this many times in a row
    /// (the progress guard): it would never reach the next record.
    NoProgress { reads: u64 },
    /// The loop returned with records left on the tape.
    EndedEarly { left: usize },
    /// The loop ended otherwise than the recorded one: `recorded` is the
    /// tape's end as it was said, `replayed` this replay's.
    OtherEnd { recorded: String, replayed: String },
    /// Something the recording made no record a replay can stand on — a
    /// mail delivery received under a recorder.
    Unreplayable { why: String },
    /// ACTS MODE: the candidate took another act than the segment's closing
    /// one — `recorded` the act the tape holds there (`None`: the candidate
    /// acted past the tape's last act), `asked` the candidate's (`None`: it
    /// ended with the recorded act still to take).
    ActDiffers {
        recorded: Option<ActSide>,
        asked: Option<ActSide>,
    },
    /// ACTS MODE: the candidate went on without the segment's closing act,
    /// `recorded`: it waits `asked`, for what the recorded run waited for
    /// (its timeout aside) only once it had taken that act — the act it
    /// skipped, the first divergent one.
    ActSkipped { recorded: ActSide, asked: ActSide },
    /// ACTS MODE: the candidate asked a read (of the class `class`: a read, a
    /// wait) its segment never answered — `request` as the tape writes it
    /// (`STALE`).
    Stale {
        class: &'static str,
        request: String,
    },
}

/// One side of a divergent act: where it is on the tape (the candidate's has
/// no place), its door and its words — a control request's, or a host
/// call's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActSide {
    pub at: Option<u64>,
    pub door: String,
    pub words: String,
}

impl std::fmt::Display for ActSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.door == super::doors::CTL {
            write!(f, "{}", self.words)?;
        } else {
            write!(f, "{} {}", self.door, self.words)?;
        }
        match self.at {
            Some(at) => write!(f, " (#{at})"),
            None => Ok(()),
        }
    }
}

/// A decision as a mark says it: its door, rule and outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    pub door: String,
    pub rule: Option<String>,
    pub outcome: String,
}

/// How a replay ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Every record in order, on the recording's own policy.
    #[default]
    Strict,
    /// A counterfactual: the acts held to the tape, the reads served from
    /// their segment (`tape::acts`), under a candidate policy.
    Acts,
}

/// What a counterfactual replay adds to its report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActsReport {
    /// The candidate policy, as said (`approve=safe`, `--policy FILE`).
    pub candidate: Vec<String>,
    /// How far the candidate strayed from the tape, counted.
    pub account: super::acts::ActsAccount,
    /// The rows of the last screen the candidate read.
    pub last_screen: Option<Vec<String>>,
    /// Where the candidate stopped short of the act the recorded run took
    /// next — STALE, or stopped otherwise before it — that act, which no
    /// cause names (a divergent or a skipped act names its own).
    pub recorded_next: Option<ActSide>,
    /// Where the candidate did not take the act the recorded run took: the
    /// decisions the recorded run made in that segment before it (its marks,
    /// in order).
    pub recorded_decisions: Vec<Decided>,
    /// The decisions the candidate made in its last segment, in order.
    pub candidate_decisions: Vec<Decided>,
}

/// What a counterfactual cannot say, said with every one (the replay plan's
/// §11).
pub const BOUND: &str = "BOUND: exact up to the first divergent act, and not past it: nothing \
                         re-drives the agent after an act the recording never took (no model, \
                         no inference); what lies below a door — the server, the window host's \
                         own work, each derived look — answers as it answered the recording";

impl std::fmt::Display for Cause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(d) => write!(f, "{d}"),
            Self::Undecodable { door, why } => {
                write!(
                    f,
                    "the reply recorded at {door:?} does not read in this build: {why}"
                )
            }
            Self::NoProgress { reads } => write!(
                f,
                "no progress: {reads} flag reads in a row consumed no record"
            ),
            Self::EndedEarly { left } => {
                write!(
                    f,
                    "the loop returned with {left} record(s) left on the tape"
                )
            }
            Self::OtherEnd { recorded, replayed } => {
                write!(
                    f,
                    "the loop ended {replayed:?}; the recorded one ended {recorded:?}"
                )
            }
            Self::Unreplayable { why } => write!(f, "no replay can make this run: {why}"),
            Self::ActDiffers { recorded, asked } => match (recorded, asked) {
                (Some(r), Some(a)) => write!(
                    f,
                    "the candidate takes another act: the recorded run took `{r}`, the \
                     candidate `{a}`"
                ),
                (None, Some(a)) => write!(
                    f,
                    "the candidate takes an act past the recorded run's last: `{a}`"
                ),
                (Some(r), None) => write!(f, "the candidate ended without the recorded act `{r}`"),
                (None, None) => write!(f, "the candidate's acts differ"),
            },
            Self::ActSkipped { recorded, asked } => write!(
                f,
                "the candidate went on without the recorded act `{recorded}`: it waits \
                 `{asked}`, for what the recorded run waited for (its timeout aside) only \
                 after that act"
            ),
            Self::Stale { class, request } => write!(
                f,
                "the candidate asks a {class} its segment of the tape never answered: \
                 {request}"
            ),
        }
    }
}

/// The first place a replay stopped reproducing the recorded run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// The effect's ordinal on the tape (`#N` in `tape cat`): the record the
    /// loop could not take, or the one whose reply it could not read.
    pub at: u64,
    /// The door the loop crossed there, when it crossed one.
    pub door: Option<String>,
    pub class: Class,
    pub cause: Cause,
}

/// A replay's whole account ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub verdict: Verdict,
    /// The tape's own id (its header's).
    pub tape: String,
    /// The loop the tape recorded (`watch`, `supervise`, `hosted`).
    pub entry: String,
    /// The build that recorded it, and this one.
    pub recorded_by: String,
    pub replayed_by: String,
    /// Effects consumed, and on the tape.
    pub consumed: usize,
    pub effects: usize,
    /// Every control request the replayed loop asked on its own lane, word
    /// by word, in order — the claim's renewals included.
    pub requests: Vec<Vec<String>>,
    /// Every request on the teller's lane (`story`).
    pub told: Vec<Vec<String>>,
    /// Every line the replayed loop printed (its `out` door's lines).
    pub lines: Vec<String>,
    /// How the replayed loop ended, as the tape says an end (`exit 1`; a
    /// supervise's review under its exit code, `replay::supervise_end`),
    /// when it returned.
    pub returned: Option<String>,
    /// What this build supplied where the tape's header did not give it the
    /// run's own (`tape::header::Header::supplied`): a key the header does
    /// not say, read at this build's value; a retired policy key's reading;
    /// and, for a policy the header marked inexact, each value it said that
    /// the owner's reader refused (with the value read in its place) and the
    /// inexactness itself.
    pub supplied: Vec<String>,
    /// How the tape's bytes ended, when not cleanly (a torn tail, a cap).
    pub tail: Option<String>,
    /// Strict, or a counterfactual.
    pub mode: Mode,
    /// A counterfactual's account (acts mode only).
    pub acts: Option<ActsReport>,
}

impl Report {
    /// The exit code `aterm drive replay` ends with.
    #[must_use]
    pub fn code(&self) -> u8 {
        self.verdict.code()
    }

    /// The report as text: the verdict line, then what the replay ran and
    /// where it stopped.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "{}", self.verdict.word());
        match &self.verdict {
            Verdict::Diverged(d) | Verdict::Stale(d) => {
                let _ = write!(out, " at #{}", d.at);
                if let Some(door) = &d.door {
                    let _ = write!(out, " ({})", super::show::shown(door));
                }
                // The cause quotes the tape (a request, a path, a reply's
                // codec error, an end's text): shown, as every field is.
                let _ = write!(
                    out,
                    ": {} — {}",
                    d.class.word(),
                    super::show::shown(&d.cause.to_string())
                );
            }
            Verdict::Reached { door } => {
                let _ = write!(out, " #{door}: everything up to it reproduced");
            }
            Verdict::Panicked {
                message,
                door: None,
            } => {
                let _ = write!(
                    out,
                    ": the recorded panic reproduced: {}",
                    super::show::shown(message)
                );
            }
            Verdict::Panicked {
                message,
                door: Some(door),
            } => {
                let _ = write!(
                    out,
                    " inside {}: the replay reached the door the recorded run died in, asking \
                     what it asked; the recorded panic: {}",
                    super::show::shown(door),
                    super::show::shown(message)
                );
            }
            Verdict::Ended => {
                out.push_str(": the tape has no end; everything on it reproduced");
            }
            Verdict::Reproduced => {}
        }
        out.push('\n');
        let _ = writeln!(
            out,
            "tape {} ({}), recorded by {}, replayed by {}",
            super::show::shown(&self.tape),
            self.entry,
            super::show::shown(&self.recorded_by),
            super::show::shown(&self.replayed_by),
        );
        if let Some(acts) = &self.acts {
            self.render_acts(acts, &mut out);
        }
        if matches!(self.verdict, Verdict::Diverged(_)) && self.recorded_by != self.replayed_by {
            let _ = writeln!(
                out,
                "  another build: a divergence here is behaviour changed since {}",
                super::show::shown(&self.recorded_by)
            );
        }
        let _ = writeln!(
            out,
            "  {} of {} effects consumed; {} request(s), {} told, {} line(s)",
            self.consumed,
            self.effects,
            self.requests.len(),
            self.told.len(),
            self.lines.len()
        );
        if let Some(how) = &self.returned {
            // A supervise's end carries its review, line for line under its
            // exit code (`replay::supervise_end`).
            let mut lines = how.lines();
            let _ = writeln!(
                out,
                "  the loop ended: {}",
                super::show::shown(lines.next().unwrap_or(""))
            );
            for line in lines {
                let _ = writeln!(out, "  = {}", super::show::shown(line));
            }
        }
        if let Some(tail) = &self.tail {
            let _ = writeln!(out, "  {tail}");
        }
        for key in &self.supplied {
            let _ = writeln!(out, "  supplied by this build: {}", super::show::shown(key));
        }
        // The ACTS among the requests — each key, send, post, badge, signal
        // or restart the replayed loop asked on its lane
        // (`tape::ctl::is_act`), in order: what reached the worker. Its reads,
        // waits and the claim's renewals are counted above and never listed:
        // a long run asks thousands; [`Self::requests`] holds every one.
        for words in &self.requests {
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            if super::ctl::is_act(&words) {
                let _ = writeln!(out, "  > {}", super::show::shown(&words.join(" ")));
            }
        }
        for line in &self.lines {
            let _ = writeln!(out, "  | {}", super::show::shown(line));
        }
        out
    }

    /// A counterfactual's own lines: the candidate, both sides of a divergent
    /// act and the decisions before it, the last screen the candidate read,
    /// how far it strayed, and what no counterfactual can say ([`BOUND`]).
    fn render_acts(&self, acts: &ActsReport, out: &mut String) {
        for said in &acts.candidate {
            let _ = writeln!(out, "  candidate: {}", super::show::shown(said));
        }
        let side = |s: &Option<ActSide>, none: &str| {
            s.as_ref()
                .map_or_else(|| none.to_string(), |s| super::show::shown(&s.to_string()))
        };
        match &self.verdict {
            Verdict::Diverged(Divergence {
                cause: Cause::ActDiffers { recorded, asked },
                ..
            }) => {
                let _ = writeln!(
                    out,
                    "  recorded act:  {}",
                    side(recorded, "none (past the recorded run's last act)")
                );
                let _ = writeln!(
                    out,
                    "  candidate act: {}",
                    side(asked, "none (the candidate ended without it)")
                );
            }
            Verdict::Diverged(Divergence {
                cause: Cause::ActSkipped { recorded, asked },
                ..
            }) => {
                let _ = writeln!(
                    out,
                    "  recorded act:  {}",
                    super::show::shown(&recorded.to_string())
                );
                let _ = writeln!(
                    out,
                    "  candidate act: none (it went on to wait `{}`, for what the recorded \
                     run waited for only after that act)",
                    super::show::shown(&asked.to_string())
                );
            }
            _ => {
                if acts.recorded_next.is_some() {
                    let _ = writeln!(
                        out,
                        "  recorded act:  {} (the recorded run's next, which the candidate \
                         has not taken)",
                        side(&acts.recorded_next, "-")
                    );
                }
            }
        }
        for (who, decided) in [
            ("recorded", &acts.recorded_decisions),
            ("candidate", &acts.candidate_decisions),
        ] {
            for d in decided {
                let _ = writeln!(
                    out,
                    "  {who} decision: {} {}: {}",
                    super::show::shown(&d.door),
                    super::show::shown(d.rule.as_deref().unwrap_or("-")),
                    super::show::shown(&d.outcome)
                );
            }
        }
        if let Some(rows) = &acts.last_screen {
            let _ = writeln!(out, "  the last screen the candidate read:");
            for row in rows {
                let _ = writeln!(out, "  : {}", super::show::shown(row));
            }
        }
        let a = &acts.account;
        let _ = writeln!(
            out,
            "  acts: {} of {} taken as recorded; records: {} consumed, {} dropped, {} served \
             again, {} looks from before, {} waits tainted, {} clock reads and {} sleeps \
             virtual, {} housekeeping not compared, {} outputs unmatched",
            a.taken,
            a.acts,
            a.consumed,
            a.dropped,
            a.repeated,
            a.keyed,
            a.tainted,
            a.virtual_clock,
            a.slept,
            a.housekept,
            a.unmatched_outputs
        );
        let _ = writeln!(out, "  {BOUND}");
    }
}
