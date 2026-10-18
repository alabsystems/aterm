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
//! * [`Verdict::Diverged`] (1): the first [`Divergence`];
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
            Self::Panicked { .. } => "PANICKED",
        }
    }

    /// The exit code `aterm drive replay` ends with (the plan's §1).
    #[must_use]
    pub fn code(&self) -> u8 {
        match self {
            Self::Reproduced | Self::Ended | Self::Reached { .. } => 0,
            Self::Diverged(_) => 1,
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
}

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
    /// What this build supplied that the tape's header did not say
    /// (`tape::header::Header::supplied`).
    pub supplied: Vec<String>,
    /// How the tape's bytes ended, when not cleanly (a torn tail, a cap).
    pub tail: Option<String>,
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
            Verdict::Diverged(d) => {
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
}
