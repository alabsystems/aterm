// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A DECISION'S RULE TRACE: the checks a policy consulted on its way to a
//! decision, in the order it consulted them, each with what came of it —
//! diagnostic data for answering "why did it press 2?".
//!
//! The approval policy ([`super::approval::decide_traced`]) and the turn-end
//! policy ([`super::turn_end::decide_turn_end_traced`]) each take a
//! [`Trace`] and write a [`TraceStep`] at every check that decided, overruled
//! a decision, or was consulted and passed the box on; the plain `decide` and
//! `decide_turn_end` are the same functions handed [`NoTrace`], so there is
//! ONE policy and a trace can never tell of a decision the policy did not
//! make.
//!
//! **Computed only where a reader asks, never in the loop.** The shipped loop
//! and a recording one decide through [`NoTrace`]: a step is built only
//! behind [`Trace::on`] ([`note`]), so a live decision builds no string and
//! allocates nothing for a trace it does not keep, and a tape holds no trace
//! (a trace is a function of what the tape does hold). Diagnostic callers
//! can collect approval steps by deciding again from the recorded facts and
//! policy inputs. A turn-end reader would need to rebuild the fold through
//! strict replay on the recording build. These public diagnostic APIs have
//! no current CLI reader.
//!
//! **Named by rule id.** A step that decided by a rule names it
//! ([`TraceStep::rule`]: `read-only@classify-v3`, `continue@v1`); a check
//! that has no rule of its own — a gate that hands the box to a person, a
//! level's rules that proved nothing, the press gate overruling an answer —
//! is named by the check ([`TraceStep::check`]). The decision a trace ends
//! on is said by [`deciding`]: the last step that decided or overruled.

/// One step of a decision's trace ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStep {
    /// Where in the policy: the check's name (`usage-limit`, `question`,
    /// `safe-rules`, `full-power`, `press-gate`, `act-at-point` …).
    pub check: &'static str,
    /// The rule that decided at this step, by its id, when the outcome is a
    /// rule's act (an approval, a decline, a typed continuation).
    pub rule: Option<&'static str>,
    /// What came of it.
    pub outcome: Outcome,
}

impl TraceStep {
    /// The step's name: its rule's id where a rule decided, else its check's.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.rule.unwrap_or(self.check)
    }
}

/// What came of one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Consulted, and the box left to what follows.
    Passed,
    /// A level's rules did not prove the box, for this reason, and the
    /// decision went on to what follows (full power, a warning's rule).
    Unproven(String),
    /// Decided here: the decision as the policy states it.
    Decided(String),
    /// A later check put another decision in the place of the one made, for
    /// this reason.
    Overruled(String),
}

/// Where a decision writes its trace ([module docs](self)).
pub trait Trace {
    /// Whether steps are kept: `false`, and none is ever built.
    fn on(&self) -> bool;

    /// Keep `step`.
    fn step(&mut self, step: TraceStep);
}

/// No trace: what every decision of the loop is made with.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTrace;

impl Trace for NoTrace {
    fn on(&self) -> bool {
        false
    }

    fn step(&mut self, _: TraceStep) {}
}

/// A trace kept in order.
impl Trace for Vec<TraceStep> {
    fn on(&self) -> bool {
        true
    }

    fn step(&mut self, step: TraceStep) {
        self.push(step);
    }
}

/// `trace`'s step at `check` under `rule`, its outcome built only when the
/// trace keeps steps.
pub(crate) fn note(
    trace: &mut dyn Trace,
    check: &'static str,
    rule: Option<&'static str>,
    outcome: impl FnOnce() -> Outcome,
) {
    if trace.on() {
        trace.step(TraceStep {
            check,
            rule,
            outcome: outcome(),
        });
    }
}

/// The step a trace's decision was made at: the last that decided or
/// overruled one. `None` for a trace that decided nothing (an empty one).
#[must_use]
pub fn deciding(steps: &[TraceStep]) -> Option<&TraceStep> {
    steps
        .iter()
        .rev()
        .find(|s| matches!(s.outcome, Outcome::Decided(_) | Outcome::Overruled(_)))
}

/// `steps` as text, one step a line, each indented by `indent`: its name,
/// then what came of it. Every text here is the policy's own words about a
/// screen it read, so the caller prints it as it prints any text off a tape.
#[must_use]
pub fn render(steps: &[TraceStep], indent: &str) -> String {
    let mut out = String::new();
    for s in steps {
        let (word, said) = match &s.outcome {
            Outcome::Passed => ("passed", None),
            Outcome::Unproven(why) => ("unproven", Some(why.as_str())),
            Outcome::Decided(what) => ("decided", Some(what.as_str())),
            Outcome::Overruled(why) => ("overruled", Some(why.as_str())),
        };
        out.push_str(indent);
        out.push_str(s.name());
        if s.rule.is_some() {
            out.push_str(" (");
            out.push_str(s.check);
            out.push(')');
        }
        out.push_str(": ");
        out.push_str(word);
        if let Some(said) = said {
            out.push_str(" — ");
            out.push_str(said);
        }
        out.push('\n');
    }
    out
}
