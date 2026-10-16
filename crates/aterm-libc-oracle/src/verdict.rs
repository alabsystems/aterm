// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a run decided, as data: each phase's status, every finding, and the
//! reason the run could not go on, if it stopped. Exit codes: 0 pass, 1 finding,
//! 2 usage, 3 could not run. A finding outranks a stop: what was decided before
//! the machine failed is still a defect in the tree.

use std::fmt;

use serde::Serialize;

use crate::cargo::{Diagnostic, FailedTest};
use crate::controls::{ControlFailure, ControlId};
use crate::shape::ShapeDiff;
use crate::siblings::SiblingFinding;
use crate::symbols::SymbolDiff;

pub const EXIT_PASS: u8 = 0;
pub const EXIT_FINDING: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_COULD_NOT_RUN: u8 = 3;

/// The run could not go on: a prerequisite is missing. Says what, and the fix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    /// A checked-in input the oracle reads is unusable (a stale `Cargo.lock`,
    /// an unreadable `union.json`).
    Input {
        what: String,
        detail: String,
    },
    Sibling(SiblingFinding),
    /// The native cell's conformance build or runtime tests failed.
    Conformance {
        errors: Vec<Diagnostic>,
        failed_tests: Vec<FailedTest>,
        detail: String,
    },
    /// The shim did not document, so its emitted shape and symbols are unknown.
    ShimDoc {
        errors: Vec<Diagnostic>,
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
            Self::Conformance { detail, .. } => {
                write!(f, "conformance failed:\n{}", indent(detail))
            }
            Self::ShimDoc { detail, .. } => {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Fail,
    /// Not run: an earlier phase made its verdict meaningless, or the run
    /// stopped before it.
    Skip,
}

/// One phase's line.
#[derive(Clone, Debug, Serialize)]
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
#[derive(Clone, Debug, Default, Serialize)]
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

    /// The one-word verdict.
    #[must_use]
    pub fn verdict(&self) -> &'static str {
        match self.exit_code() {
            EXIT_PASS => "pass",
            EXIT_FINDING => "finding",
            _ => "could_not_run",
        }
    }

    /// The report as one JSON object (`verdict` added).
    #[must_use]
    pub fn to_json(&self) -> String {
        #[derive(Serialize)]
        struct Out<'r> {
            verdict: &'static str,
            #[serde(flatten)]
            report: &'r Report,
        }
        aterm_json::to_string(&Out {
            verdict: self.verdict(),
            report: self,
        })
        .unwrap_or_else(|e| format!(r#"{{"verdict":"could_not_run","error":"{e}"}}"#))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::SymbolDiff;

    #[test]
    fn findings_outrank_a_stop_and_a_stop_alone_could_not_run() {
        let mut report = Report::default();
        assert_eq!((report.exit_code(), report.verdict()), (EXIT_PASS, "pass"));
        report.could_not_run = Some(CouldNotRun::bare("no trustc"));
        assert_eq!(
            (report.exit_code(), report.verdict()),
            (EXIT_COULD_NOT_RUN, "could_not_run")
        );
        report
            .findings
            .push(Finding::Symbol(SymbolDiff::AbsentFromReference {
                name: "made_up".into(),
            }));
        assert_eq!(
            (report.exit_code(), report.verdict()),
            (EXIT_FINDING, "finding"),
            "a defect found before the machine failed is still a defect"
        );
        assert!(report.could_not_run.is_some(), "the stop is still reported");
    }

    #[test]
    fn the_json_carries_typed_findings() {
        let report = Report {
            host: Some("aarch64-apple-darwin".into()),
            cell: Some("mac"),
            findings: vec![Finding::Symbol(SymbolDiff::Mismatch {
                name: "getpid".into(),
                shim: "getpid$X".into(),
                reference: "getpid".into(),
            })],
            ..Report::default()
        };
        let json: aterm_json::Value = aterm_json::from_str(&report.to_json()).expect("json");
        assert_eq!(json["verdict"].as_str(), Some("finding"));
        assert_eq!(json["cell"].as_str(), Some("mac"));
        let finding = &json["findings"][0];
        assert_eq!(finding["kind"].as_str(), Some("symbol"));
        assert_eq!(finding["symbol"].as_str(), Some("mismatch"));
        assert_eq!(finding["shim"].as_str(), Some("getpid$X"));
    }
}
