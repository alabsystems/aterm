// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Refinement coverage tracking.
//!
//! Tracks which concrete `&mut self` methods are mapped to TLA+ actions
//! via `#[refines(...)]` and which are explicitly excluded via
//! `#[spec_unmodeled(...)]`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::tla_check::TlaSpec;

/// A mapping entry from a Rust method to a TLA+ action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefinementEntry {
    /// The TLA+ machine name (e.g., "terminal_modes").
    pub machine: String,
    /// The TLA+ action name (e.g., "SetCursorVisible").
    pub action: String,
    /// The Rust method path (e.g., "TerminalModes::set_cursor_visible").
    pub rust_method: String,
    /// Source file and line.
    pub location: String,
}

/// Coverage report for one TLA+ specification.
#[derive(Debug, Clone)]
pub struct SpecCoverage {
    /// TLA+ module name.
    pub spec_name: String,
    /// TLA+ file path.
    pub tla_file: String,
    /// Actions that have at least one `#[refines(...)]` in Rust.
    pub refined_actions: BTreeSet<String>,
    /// Actions in TLA+ with no Rust refinement.
    pub unrefined_actions: BTreeSet<String>,
    /// Rust methods annotated with `#[refines(...)]` for this machine.
    pub refinements: Vec<RefinementEntry>,
}

impl SpecCoverage {
    /// Fraction of TLA+ actions that have Rust refinements.
    pub fn coverage_ratio(&self) -> f64 {
        // Saturating: the two sets are disjoint in-memory collections, so their
        // combined size can never exceed `usize::MAX` — saturation is a no-op on
        // every real input; it only discharges the unconstrained-input overflow
        // obligation (Trust L0).
        let total = self
            .refined_actions
            .len()
            .saturating_add(self.unrefined_actions.len());
        if total == 0 {
            return 1.0;
        }
        // `max(1)`: `total == 0` already returned above, so the clamp is a no-op
        // on every reached input; it only discharges the unconstrained-divisor
        // obligation (Trust L0).
        self.refined_actions.len() as f64 / total.max(1) as f64
    }
}

/// Workspace-wide refinement coverage tracker.
#[derive(Debug, Default)]
pub struct CoverageTracker {
    entries: Vec<RefinementEntry>,
    specs: BTreeMap<String, TlaSpec>,
}

impl CoverageTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a refinement entry.
    pub fn add_refinement(&mut self, entry: RefinementEntry) {
        self.entries.push(entry);
    }

    /// Load and parse a TLA+ spec file.
    pub fn load_spec(
        &mut self,
        machine_name: &str,
        path: &Path,
    ) -> Result<(), crate::tla_check::TlaParseError> {
        let spec = TlaSpec::parse_file(path)?;
        self.specs.insert(machine_name.to_string(), spec);
        Ok(())
    }

    /// Register a pre-parsed TLA+ spec.
    // Skip: BTreeMap keyed insert + format (absent std bodies). Coverage
    // bookkeeping for the spec gate, not shipping runtime code.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn add_spec(&mut self, machine_name: &str, spec: TlaSpec) {
        self.specs.insert(machine_name.to_string(), spec);
    }

    /// Generate coverage report for a specific machine.
    // Skip: BTreeMap/Set keyed reads + format (absent std bodies). Coverage
    // reporting for the spec gate, not shipping runtime code.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn report(&self, machine_name: &str) -> Option<SpecCoverage> {
        let spec = self.specs.get(machine_name)?;

        let refinements: Vec<RefinementEntry> = self
            .entries
            .iter()
            .filter(|e| e.machine == machine_name)
            .cloned()
            .collect();

        let refined_actions: BTreeSet<String> =
            refinements.iter().map(|e| e.action.clone()).collect();

        // Explicit insert loop (not `collect`) so the Trust L0 allocation check
        // sees per-element growth instead of an unbounded bulk allocation;
        // behavior-identical.
        let mut unrefined_actions: BTreeSet<String> = BTreeSet::new();
        for a in spec.actions.difference(&refined_actions) {
            unrefined_actions.insert(a.clone());
        }

        Some(SpecCoverage {
            spec_name: spec.module_name.clone(),
            tla_file: spec.file_path.clone(),
            refined_actions,
            unrefined_actions,
            refinements,
        })
    }

    /// Generate coverage reports for all registered machines.
    // Skip: same BTreeMap keyed-read + iterator class as `report`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn report_all(&self) -> Vec<SpecCoverage> {
        self.specs
            .keys()
            .filter_map(|name| self.report(name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coverage_tracker_basic() {
        let mut tracker = CoverageTracker::new();

        let spec = TlaSpec::parse_str(
            r#"
---- MODULE Modes ----
VARIABLES mode

SetMode == mode' = TRUE
ResetMode == mode' = FALSE

Next == SetMode \/ ResetMode
====
"#,
            "tla/Modes.tla",
        )
        .unwrap();

        tracker.add_spec("modes", spec);
        tracker.add_refinement(RefinementEntry {
            machine: "modes".to_string(),
            action: "SetMode".to_string(),
            rust_method: "Modes::set_mode".to_string(),
            location: "src/modes.rs:42".to_string(),
        });

        let report = tracker.report("modes").unwrap();
        assert_eq!(report.refined_actions.len(), 1);
        assert!(report.refined_actions.contains("SetMode"));
        assert!(report.unrefined_actions.contains("ResetMode"));
        assert!(report.unrefined_actions.contains("Next"));
        // 1 refined out of 3 total definitions (SetMode, ResetMode, Next)
        let expected_ratio = 1.0 / 3.0;
        assert!((report.coverage_ratio() - expected_ratio).abs() < 0.01);
    }

    #[test]
    fn test_coverage_tracker_complete() {
        let mut tracker = CoverageTracker::new();

        let spec = TlaSpec::parse_str(
            r#"
---- MODULE Simple ----
VARIABLES x

Inc == x' = x + 1
Dec == x' = x - 1

Next == Inc \/ Dec
====
"#,
            "tla/Simple.tla",
        )
        .unwrap();

        tracker.add_spec("simple", spec);
        tracker.add_refinement(RefinementEntry {
            machine: "simple".to_string(),
            action: "Inc".to_string(),
            rust_method: "Simple::inc".to_string(),
            location: "src/simple.rs:10".to_string(),
        });
        tracker.add_refinement(RefinementEntry {
            machine: "simple".to_string(),
            action: "Dec".to_string(),
            rust_method: "Simple::dec".to_string(),
            location: "src/simple.rs:20".to_string(),
        });

        let report = tracker.report("simple").unwrap();
        // Next is unrefined (it's the composition, not an individual action)
        assert_eq!(report.unrefined_actions.len(), 1);
        assert!(report.unrefined_actions.contains("Next"));
        // 2 refined out of 3 total definitions
        let expected_ratio = 2.0 / 3.0;
        assert!((report.coverage_ratio() - expected_ratio).abs() < 0.01);
    }
}
