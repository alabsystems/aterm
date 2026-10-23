// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Run verdict: each phase's status, every finding, and any early-stop reason.

use std::fmt;

use crate::controls::{ControlFailure, ControlId};
use crate::shape::ShapeDiff;
use crate::siblings::SiblingFinding;
use crate::symbols::SymbolDiff;

pub const EXIT_PASS: u8 = 0;
pub const EXIT_FINDING: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_COULD_NOT_RUN: u8 = 3;

/// The run could not go on: a prerequisite is missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CouldNotRun {
    pub what: String,
    pub fix: Option<String>,
}

impl CouldNotRun {
    #[must_use]
    pub fn new(what: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            fix: Some(fix.into()),
        }
    }

    /// A reason that already names its fix.
    #[must_use]
    pub fn bare(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            fix: None,
        }
    }
}

impl fmt::Display for CouldNotRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.what)?;
        match &self.fix {
            Some(fix) => write!(f, "\n  fix: {fix}"),
            None => Ok(()),
        }
    }
}

/// A defect in the tree: the shim, its generated oracle, or their inputs.
#[derive(Clone, Debug)]
pub enum Finding {
    /// A checked-in input the oracle reads is unusable (such as `Cargo.lock` or `union.json`).
    Input {
        what: String,
        detail: String,
    },
    Sibling(SiblingFinding),
    /// The native cell's conformance build or runtime tests failed.
    Conformance {
        detail: String,
    },
    /// The shim did not document, so its emitted shape and symbols are unknown.
    ShimDoc {
        detail: String,
    },
    Shape(ShapeDiff),
    Symbol(SymbolDiff),
    /// A planted defect the oracle no longer catches as it should.
    Control {
        control: ControlId,
        failure: ControlFailure,
    },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input { what, detail } => write!(f, "{what}\n{}", indent(detail)),
            Self::Sibling(finding) => write!(f, "sibling body: {finding}"),
            Self::Conformance { detail } => {
                write!(f, "conformance failed:\n{}", indent(detail))
            }
            Self::ShimDoc { detail } => {
                write!(f, "the shim did not document:\n{}", indent(detail))
            }
            Self::Shape(diff) => write!(f, "source shape: {diff}"),
            Self::Symbol(diff) => write!(f, "symbol: {diff}"),
            Self::Control { control, failure } => {
                write!(f, "control {}: {failure}", control.name())
            }
        }
    }
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("      {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One stage of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// `Cargo.lock` current, locked dependencies present offline.
    Inputs,
    /// Each same-OS pair's hand-written bodies are identical text.
    Siblings,
    /// The native cell's compile-time assertions and runtime tests.
    Conformance,
    /// The shim's public shape against the reference's over the cell.
    Shape,
    /// Every extern declaration's linker symbol against the reference's.
    Symbols,
    /// Planted defects the oracle must catch.
    Controls,
}

impl Phase {
    /// Every phase, in run order.
    pub const ALL: [Self; 6] = [
        Self::Inputs,
        Self::Siblings,
        Self::Conformance,
        Self::Shape,
        Self::Symbols,
        Self::Controls,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Inputs => "inputs",
            Self::Siblings => "siblings",
            Self::Conformance => "conformance",
            Self::Shape => "shape",
            Self::Symbols => "symbols",
            Self::Controls => "controls",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    /// Not run: an earlier phase made its verdict meaningless, or the run stopped before it.
    Skip,
}

/// One phase's line.
#[derive(Clone, Debug)]
pub struct PhaseReport {
    pub phase: Phase,
    pub status: Status,
    pub secs: f64,
    pub detail: String,
}

impl fmt::Display for PhaseReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = match self.status {
            Status::Ok => "ok",
            Status::Fail => "FAIL",
            Status::Skip => "skip",
        };
        write!(
            f,
            "  {status:<5}{:<13}{:>6.1}s  {}",
            self.phase.name(),
            self.secs,
            self.detail
        )
    }
}

/// The whole run.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// This box's triple, once the toolchain answered.
    pub host: Option<String>,
    /// The cell decided (its `union.json` key).
    pub cell: Option<&'static str>,
    pub phases: Vec<PhaseReport>,
    pub findings: Vec<Finding>,
    /// Why the run stopped early, if it did; any findings made before it stand.
    pub could_not_run: Option<CouldNotRun>,
}

impl Report {
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        if !self.findings.is_empty() {
            EXIT_FINDING
        } else if self.could_not_run.is_some() {
            EXIT_COULD_NOT_RUN
        } else {
            EXIT_PASS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::SymbolDiff;

    /// The exit code `targo --unverified verify libc` reads: a finding outranks
    /// a stop, and a stop alone could not run.
    #[test]
    fn findings_outrank_a_stop_and_a_stop_alone_could_not_run() {
        let mut report = Report::default();
        assert_eq!(report.exit_code(), EXIT_PASS);
        report.could_not_run = Some(CouldNotRun::bare("no trustc"));
        assert_eq!(report.exit_code(), EXIT_COULD_NOT_RUN);
        report
            .findings
            .push(Finding::Symbol(SymbolDiff::AbsentFromReference {
                name: "made_up".into(),
            }));
        assert_eq!(
            report.exit_code(),
            EXIT_FINDING,
            "a defect found before the machine failed is still a defect"
        );
    }
}
