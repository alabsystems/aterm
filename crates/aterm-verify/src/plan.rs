// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LADDER, as data: which stages run, in which order, contending for what.
//!
//! The order is the script's order and is part of the contract — reviewers and
//! agents read these runs top to bottom and know where to look. Concurrency
//! changes when a stage RUNS, never where it PRINTS.

use crate::Ctx;
use crate::cli::Mode;

/// A contended resource. Two stages in the same lane are serialised in declared
/// order; different lanes overlap freely. In practice a lane is a cargo target
/// directory, which is exactly the thing two concurrent cargo invocations would
/// queue on anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Shells out to nothing that touches a target dir.
    Pure,
    /// `target/` — the workspace build, its tests, the xtask gates, the smokes.
    MainTarget,
    /// `target-tippy/` — the lint keeps a SEPARATE target dir so the trust
    /// toolchain's artifacts stay off the stock build. That is also what makes
    /// it free to run beside the build.
    TippyTarget,
    /// `tools/freeze-safety-gate/` is its own workspace with its own target dir.
    FreezeGateTarget,
}

/// Every stage of the gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageId {
    Build,
    Test,
    Doctests,
    RegexLane,
    Tippy,
    GrepGuards,
    InstallChannel,
    TrustGateVerdict,
    StartCompare,
    LicenseHeaders,
    FeatureGates,
    FreezeGate,
    ProofInventory,
    ControlSocketSmoke,
    GuiSmoke,
    DifferentialOracle,
    KaniFloor,
}

/// A stage's identity, its ladder header, and its scheduling constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSpec {
    pub id: StageId,
    /// The `=== … ===` header, resolved against the scope where it varies.
    pub title: String,
    pub lane: Lane,
    /// Runs with nothing else in flight. Only the two smokes, and only because
    /// they measure frame rates and latencies: a stage whose verdict depends on
    /// how busy the machine is must own the machine.
    pub exclusive: bool,
}

/// Build the run's stage list.
///
/// Two things can remove a stage entirely (as opposed to skipping it):
///  * `--scope` narrowing away from `aterm-search` drops the regex lane, because
///    there is nothing for it to run — the script never printed its header either;
///  * `--fast` drops the two `--full`-only stages.
///
/// Nothing else is conditional here. Absent TOOLS produce skips inside a stage,
/// never a missing stage, because a stage that vanishes is a stage nobody
/// notices was not run.
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    let label = ctx.scope.label();
    let mut v = vec![
        spec(StageId::Build, format!("build ({label})"), Lane::MainTarget),
        spec(StageId::Test, format!("test ({label})"), Lane::MainTarget),
        spec(
            StageId::Doctests,
            format!("doctests ({label})"),
            Lane::MainTarget,
        ),
    ];
    if ctx.scope.includes_regex_lane() {
        v.push(spec(
            StageId::RegexLane,
            "regex search lane (aterm-search --features regex)",
            Lane::MainTarget,
        ));
    }
    v.push(spec(
        StageId::Tippy,
        format!("tippy lint ({label})"),
        Lane::TippyTarget,
    ));
    v.push(spec(StageId::GrepGuards, "grep guards", Lane::Pure));
    v.push(spec(
        StageId::InstallChannel,
        "bootstrap update-channel arbitration/identity",
        Lane::Pure,
    ));
    v.push(spec(
        StageId::TrustGateVerdict,
        "trust-gate verdict self-test",
        Lane::Pure,
    ));
    v.push(spec(
        StageId::StartCompare,
        "startup comparison scheduler",
        Lane::Pure,
    ));
    v.push(spec(StageId::LicenseHeaders, "license headers", Lane::Pure));
    v.push(spec(
        StageId::FeatureGates,
        "feature gates (drift/dormant)",
        Lane::MainTarget,
    ));
    v.push(spec(
        StageId::FreezeGate,
        "L0 temporal-safety gate (freeze/data-loss/deadlock — 5 obligations)",
        Lane::FreezeGateTarget,
    ));
    v.push(spec(
        StageId::ProofInventory,
        "computed proof inventory",
        Lane::MainTarget,
    ));
    v.push(exclusive(
        StageId::ControlSocketSmoke,
        "control-socket smoke",
        Lane::MainTarget,
    ));
    v.push(exclusive(
        StageId::GuiSmoke,
        "gui typing-pacing smoke",
        Lane::MainTarget,
    ));
    if ctx.mode == Mode::Full {
        v.push(spec(
            StageId::DifferentialOracle,
            "differential oracle (aterm vs alacritty)",
            Lane::MainTarget,
        ));
        v.push(spec(
            StageId::KaniFloor,
            "trust-mc / Kani BMC floor (config-free parser harnesses)",
            Lane::MainTarget,
        ));
    }
    v
}

fn spec(id: StageId, title: impl Into<String>, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: title.into(),
        lane,
        exclusive: false,
    }
}

fn exclusive(id: StageId, title: impl Into<String>, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: title.into(),
        lane,
        exclusive: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSnapshot;
    use crate::scope::Scope;
    use std::path::PathBuf;

    fn ctx(mode: Mode, scope: Scope) -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            mode,
            scope,
            false,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    fn ids(ctx: &Ctx) -> Vec<StageId> {
        plan(ctx).into_iter().map(|s| s.id).collect()
    }

    #[test]
    fn the_fast_ladder_is_the_fourteen_stages_in_the_documented_order() {
        assert_eq!(
            ids(&ctx(Mode::Fast, Scope::workspace())),
            [
                StageId::Build,
                StageId::Test,
                StageId::Doctests,
                StageId::RegexLane,
                StageId::Tippy,
                StageId::GrepGuards,
                StageId::InstallChannel,
                StageId::TrustGateVerdict,
                StageId::StartCompare,
                StageId::LicenseHeaders,
                StageId::FeatureGates,
                StageId::FreezeGate,
                StageId::ProofInventory,
                StageId::ControlSocketSmoke,
                StageId::GuiSmoke,
            ]
        );
    }

    #[test]
    fn full_adds_the_two_opt_in_tiers_at_the_end_and_changes_nothing_else() {
        let fast = ids(&ctx(Mode::Fast, Scope::workspace()));
        let full = ids(&ctx(Mode::Full, Scope::workspace()));
        assert_eq!(full[..fast.len()], fast[..]);
        assert_eq!(
            full[fast.len()..],
            [StageId::DifferentialOracle, StageId::KaniFloor]
        );
    }

    #[test]
    fn scoping_puts_the_crate_in_every_header_that_names_a_scope() {
        let p = plan(&ctx(Mode::Fast, Scope::crate_only("aterm-grid")));
        let titles: Vec<&str> = p.iter().map(|s| s.title.as_str()).collect();
        assert!(titles.contains(&"build (-p aterm-grid)"));
        assert!(titles.contains(&"test (-p aterm-grid)"));
        assert!(titles.contains(&"doctests (-p aterm-grid)"));
        assert!(titles.contains(&"tippy lint (-p aterm-grid)"));
        // The whole-tree stages keep their whole-tree titles.
        assert!(titles.contains(&"grep guards"));
        assert!(titles.contains(&"license headers"));
    }

    #[test]
    fn scoping_away_from_aterm_search_removes_the_regex_lane_only() {
        let scoped = ids(&ctx(Mode::Fast, Scope::crate_only("aterm-grid")));
        assert!(!scoped.contains(&StageId::RegexLane));
        let full = ids(&ctx(Mode::Fast, Scope::workspace()));
        let expected: Vec<StageId> = full
            .into_iter()
            .filter(|i| *i != StageId::RegexLane)
            .collect();
        assert_eq!(scoped, expected, "no other stage is dropped by a scope");
        assert!(
            ids(&ctx(Mode::Fast, Scope::crate_only("aterm-search"))).contains(&StageId::RegexLane)
        );
    }

    #[test]
    fn only_the_measuring_stages_are_exclusive() {
        let p = plan(&ctx(Mode::Full, Scope::workspace()));
        let ex: Vec<StageId> = p.iter().filter(|s| s.exclusive).map(|s| s.id).collect();
        assert_eq!(ex, [StageId::ControlSocketSmoke, StageId::GuiSmoke]);
    }

    #[test]
    fn every_cargo_stage_declares_the_target_dir_it_contends_for() {
        // A stage that lied about its lane would let two cargo invocations queue
        // on a lock the scheduler thought was free — slower, and unexplainable.
        for s in plan(&ctx(Mode::Full, Scope::workspace())) {
            let want = match s.id {
                StageId::Tippy => Lane::TippyTarget,
                StageId::FreezeGate => Lane::FreezeGateTarget,
                StageId::GrepGuards
                | StageId::InstallChannel
                | StageId::TrustGateVerdict
                | StageId::StartCompare
                | StageId::LicenseHeaders => Lane::Pure,
                _ => Lane::MainTarget,
            };
            assert_eq!(s.lane, want, "{:?} declares the wrong lane", s.id);
        }
    }

    #[test]
    fn a_missing_tool_never_removes_a_stage() {
        // Toolchain absence is invisible in the plan: it becomes a skip INSIDE
        // the stage, so the ladder still shows the row and the verdict still
        // names it. A stage that disappeared would be a stage nobody missed.
        let nothing_installed = ctx(Mode::Full, Scope::workspace());
        assert!(!nothing_installed.tools.have_targo());
        assert_eq!(plan(&nothing_installed).len(), 17);
    }
}
