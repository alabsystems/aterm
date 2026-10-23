// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rule trace ([`super::approval::decide_traced`], [`super::turn_end::decide_turn_end_traced`]).
//! Collectors: [`Trace`], [`TraceStep`], [`NoTrace`], [`Trace::on`], and [`note`].
//! Selectors: [`TraceStep::rule`], [`TraceStep::check`], and [`deciding`].

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
    /// The step's name.
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
    fn on(&self) -> bool;

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

impl Trace for Vec<TraceStep> {
    fn on(&self) -> bool {
        true
    }

    fn step(&mut self, step: TraceStep) {
        self.push(step);
    }
}

/// `trace`'s step at `check` under `rule`, its outcome built only when the trace keeps steps.
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

/// The step a trace's decision was made at: the last that decided or overruled one.
#[must_use]
pub fn deciding(steps: &[TraceStep]) -> Option<&TraceStep> {
    steps
        .iter()
        .rev()
        .find(|s| matches!(s.outcome, Outcome::Decided(_) | Outcome::Overruled(_)))
}

/// `steps` as text, one step a line, each indented by `indent`.
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
