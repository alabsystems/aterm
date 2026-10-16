// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The checks the CLI runs: each a name, the stages that decide it, what it
//! needs and what it runs; and the two sets, `dev` and `release`.

use crate::plan::StageId;

/// One check, as named on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    Fmt,
    Guards,
    Census,
    Lint,
    Test,
    Doc,
    Forge,
    Cells,
    WasmSmoke,
    WasmGlue,
    Libc,
    Spec,
    Smoke,
    Appkit,
    Export,
    Trust,
    Measure,
    Sealed,
    Codex,
}

/// What a check runs over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The selection: the change cone, `-p`, or `--all`.
    Selection,
    /// The whole tree, whatever the selection: cheap enough, or not per-crate.
    WholeTree,
}

/// A check's definition.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub check: Check,
    pub name: &'static str,
    /// The stages whose rows decide it.
    pub stages: &'static [StageId],
    /// Stages it needs run first (prebuilds), pulled into the plan with it.
    pub helpers: &'static [StageId],
    pub reach: Reach,
    /// What the machine must have for it to run rather than skip.
    pub needs: &'static str,
    /// What it runs, in one line.
    pub runs: &'static str,
}

/// Every check, in the order `list` shows them and a verdict names them.
pub const CHECKS: [Spec; 19] = [
    Spec {
        check: Check::Fmt,
        name: "fmt",
        stages: &[StageId::Formatting],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "trustfmt",
        runs: "trustfmt --check, each .rs at its crate's edition (--fix rewrites)",
    },
    Spec {
        check: Check::Guards,
        name: "guards",
        stages: &[StageId::Guards],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "-",
        runs: "aterm-census's source guards (the rules no type or lint can hold)",
    },
    Spec {
        check: Check::Census,
        name: "census",
        stages: &[StageId::Census],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "-",
        runs: "aterm-census's five L0 censuses",
    },
    Spec {
        check: Check::Lint,
        name: "lint",
        stages: &[StageId::Tippy],
        helpers: &[],
        reach: Reach::Selection,
        needs: "targo-tippy",
        runs: "targo-tippy <selection> --all-targets -- -D warnings",
    },
    Spec {
        check: Check::Test,
        name: "test",
        stages: &[StageId::Test],
        helpers: &[StageId::TestCompile],
        reach: Reach::Selection,
        needs: "targo; python3.11+ and openssl 3 (aterm-update), node (aterm-verify's glue)",
        runs: "targo test <selection> --tests, binaries in parallel",
    },
    Spec {
        check: Check::Doc,
        name: "doc",
        stages: &[StageId::Doctests],
        helpers: &[],
        reach: Reach::Selection,
        needs: "targo, trustdoc",
        runs: "targo test --doc <selection>",
    },
    Spec {
        check: Check::Forge,
        name: "forge",
        stages: &[StageId::Forge],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "targo",
        runs: "aterm-forge check: the third-party surface policy",
    },
    Spec {
        check: Check::Cells,
        name: "cells",
        stages: &[StageId::ForeignCells],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "rustup stable + the foreign stds, clippy",
        runs: "the foreign cells (or each --cell), linted for their own triples (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::WasmSmoke,
        name: "wasm-smoke",
        stages: &[StageId::WasmSmoke],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "node, rustup stable + wasm32, the pinned wasm-bindgen",
        runs: "both web modules (dev) under node: their host boundary, then the CPU replay (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::WasmGlue,
        name: "wasm-glue",
        stages: &[StageId::WasmGlue],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "node, rustup stable + wasm32",
        runs: "`verify glue` on aterm-wasm-abi's toy: goldens, node, a planted bug (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::Libc,
        name: "libc",
        stages: &[StageId::LibcOracle],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "targo",
        runs: "aterm-libc-oracle (this host's native ABI cell)",
    },
    Spec {
        check: Check::Spec,
        name: "spec",
        stages: &[StageId::SpecCheck, StageId::TyConformances],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "ty, ay, trust-ir (aterm pkg install --default-set)",
        runs: "spec-check (ty, ay, trust-ir over every registered model), the ty-only tests",
    },
    Spec {
        check: Check::Smoke,
        name: "smoke",
        stages: &[StageId::ControlSocketSmoke],
        helpers: &[StageId::DriverBuilds],
        reach: Reach::WholeTree,
        needs: "targo, unix",
        runs: "a headless aterm-gui over its control socket (alone)",
    },
    Spec {
        check: Check::Appkit,
        name: "appkit",
        stages: &[StageId::RedrawConformance, StageId::ObjcDrives],
        helpers: &[StageId::TailDriverBuilds],
        reach: Reach::WholeTree,
        needs: "macOS, a free desktop",
        runs: "the redraw harness and the objc drives (windows)",
    },
    Spec {
        check: Check::Export,
        name: "export",
        stages: &[StageId::Export],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "network, ~/publication, gitleaks, rustup trust, HEAD pushed",
        runs: "ship check --export (the publication engine's export guards)",
    },
    Spec {
        check: Check::Trust,
        name: "trust",
        stages: &[StageId::TrustAdvisory],
        helpers: &[],
        reach: Reach::Selection,
        needs: "targo",
        runs: "ship trust run -p <the selection's libraries>",
    },
    Spec {
        check: Check::Measure,
        name: "measure",
        stages: &[StageId::MeasuringTests, StageId::GuiSmoke],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "--app <APP>, a free desktop",
        runs: "the ignored paint/spin rows on <APP>, GUI pacing smoke (alone)",
    },
    Spec {
        check: Check::Sealed,
        name: "sealed",
        stages: &[StageId::SealedLane],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "targo",
        runs: "targo test -p aterm --features sealed (two_nodes_sealed, fabric_world)",
    },
    Spec {
        check: Check::Codex,
        name: "codex",
        stages: &[StageId::CodexLiveUpgrade],
        helpers: &[],
        reach: Reach::WholeTree,
        needs: "an older managed Codex",
        runs: "lab codex-upgrade (alone; by hand)",
    },
];

/// The bare command's set: fast, and scoped to what changed.
pub const DEV: [Check; 6] = [
    Check::Fmt,
    Check::Guards,
    Check::Census,
    Check::Lint,
    Check::Test,
    Check::Doc,
];

/// What publication requires: the whole workspace, and a skip is not a pass.
pub const RELEASE: [Check; 14] = [
    Check::Fmt,
    Check::Lint,
    Check::Test,
    Check::Doc,
    Check::Guards,
    Check::Census,
    Check::Forge,
    Check::Cells,
    Check::Libc,
    Check::Spec,
    Check::Smoke,
    Check::Appkit,
    Check::Measure,
    Check::Sealed,
];

impl Check {
    /// This check's definition.
    #[must_use]
    pub fn spec(self) -> &'static Spec {
        CHECKS
            .iter()
            .find(|s| s.check == self)
            .expect("every check is in CHECKS")
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// Does this check read the selection?
    #[must_use]
    pub fn narrows(self) -> bool {
        self.spec().reach == Reach::Selection
    }
}

/// A word on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Word {
    Check(Check),
    Dev,
    Release,
    List,
}

/// The word `w` names, if any.
#[must_use]
pub fn lookup(w: &str) -> Option<Word> {
    match w {
        "dev" => Some(Word::Dev),
        "release" => Some(Word::Release),
        "list" => Some(Word::List),
        _ => CHECKS
            .iter()
            .find(|s| s.name == w)
            .map(|s| Word::Check(s.check)),
    }
}

/// The stages a selection of checks plans: each check's own and its helpers,
/// without repeats.
#[must_use]
pub fn stages_of(checks: &[Check]) -> Vec<StageId> {
    let mut v: Vec<StageId> = Vec::new();
    for c in checks {
        for id in c.spec().helpers.iter().chain(c.spec().stages) {
            if !v.contains(id) {
                v.push(*id);
            }
        }
    }
    v
}

/// The check a stage's rows count toward in this selection: the selected check
/// that runs it, else the first selected check that needs it as a helper.
#[must_use]
pub fn owner(id: StageId, checks: &[Check]) -> Option<Check> {
    checks
        .iter()
        .find(|c| c.spec().stages.contains(&id))
        .or_else(|| checks.iter().find(|c| c.spec().helpers.contains(&id)))
        .copied()
}

/// The sets a check belongs to, as `list` prints them.
#[must_use]
pub fn sets_of(check: Check) -> String {
    let mut v = Vec::new();
    if DEV.contains(&check) {
        v.push("dev");
    }
    if RELEASE.contains(&check) {
        v.push("release");
    }
    if v.is_empty() {
        "-".to_string()
    } else {
        v.join(" ")
    }
}

/// `verify list`: every check, its sets, what it reads, needs and runs.
#[must_use]
pub fn list_table() -> String {
    let rows: Vec<[String; 5]> = CHECKS
        .iter()
        .map(|s| {
            let over = match s.reach {
                Reach::Selection => "selection",
                Reach::WholeTree => "tree",
            };
            [
                s.name.to_string(),
                sets_of(s.check),
                over.to_string(),
                s.needs.to_string(),
                s.runs.to_string(),
            ]
        })
        .collect();
    let head = ["CHECK", "SETS", "OVER", "NEEDS", "RUNS"].map(String::from);
    let width = |i: usize| {
        rows.iter()
            .chain(std::iter::once(&head))
            .map(|r| r[i].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2), width(3)];
    let mut out = String::new();
    for r in std::iter::once(&head).chain(&rows) {
        for (i, w) in widths.iter().enumerate() {
            let pad = w - r[i].chars().count();
            out.push_str(&r[i]);
            out.push_str(&" ".repeat(pad + 2));
        }
        out.push_str(&r[4]);
        out.push('\n');
    }
    out.push_str(
        "\ndev      fmt guards census lint test doc: the bare command, on the change cone\n\
         release  what `ship release` requires: --all implied, --app required, a skip is exit 3\n\
         perf     not a check: the performance lanes, `verify perf --list`\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_names_one_thing() {
        assert_eq!(lookup("dev"), Some(Word::Dev));
        assert_eq!(lookup("release"), Some(Word::Release));
        assert_eq!(lookup("list"), Some(Word::List));
        assert_eq!(lookup("lint"), Some(Word::Check(Check::Lint)));
        assert_eq!(lookup("wasm-smoke"), Some(Word::Check(Check::WasmSmoke)));
        assert_eq!(lookup("wasm-glue"), Some(Word::Check(Check::WasmGlue)));
        assert_eq!(lookup("contract-probe"), None);
        assert_eq!(lookup("tippy"), None);
        let mut names: Vec<&str> = CHECKS.iter().map(|s| s.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), CHECKS.len());
        for s in &CHECKS {
            assert_eq!(s.check.spec().name, s.name);
            assert!(!s.stages.is_empty(), "{}", s.name);
        }
    }

    #[test]
    fn a_stage_counts_toward_the_check_that_runs_it_before_one_that_needs_it() {
        assert_eq!(
            owner(StageId::TestCompile, &[Check::Test]),
            Some(Check::Test)
        );
        assert_eq!(
            owner(StageId::TailDriverBuilds, &[Check::Codex, Check::Appkit]),
            Some(Check::Appkit)
        );
        assert_eq!(owner(StageId::Test, &[Check::Lint]), None);
        assert_eq!(
            stages_of(&[Check::Test, Check::Smoke]),
            [
                StageId::TestCompile,
                StageId::Test,
                StageId::DriverBuilds,
                StageId::ControlSocketSmoke
            ]
        );
    }

    /// A declared stage no check reaches is dead code in the CLI.
    #[test]
    fn every_declared_stage_belongs_to_some_check() {
        let all: Vec<Check> = CHECKS.iter().map(|s| s.check).collect();
        for s in crate::plan::declared(&crate::Scope::workspace()) {
            assert!(
                owner(s.id, &all).is_some(),
                "{:?} belongs to no check",
                s.id
            );
        }
    }
}
