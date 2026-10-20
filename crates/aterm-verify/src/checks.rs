// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The checks the CLI runs: each a name, the stages that decide it, what it
//! needs and what it runs; and the two sets, `dev` and `release`.

use crate::plan::StageId;

/// One check, as named on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    Fmt,
    Census,
    Lint,
    Test,
    Doc,
    Forge,
    Cells,
    WasmSmoke,
    WasmGlue,
    Browser,
    Libc,
    Spec,
    Checkers,
    GpuOracle,
    Appkit,
    Export,
    Measure,
    CodexAlab,
    Linux,
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
        reach: Reach::WholeTree,
        needs: "trustfmt",
        runs: "trustfmt --check, each .rs at its crate's edition (--fix rewrites)",
    },
    Spec {
        check: Check::Census,
        name: "census",
        stages: &[StageId::Census],
        reach: Reach::WholeTree,
        needs: "-",
        runs: "aterm-census's source guards and its five L0 censuses",
    },
    Spec {
        check: Check::Lint,
        name: "lint",
        stages: &[StageId::Tippy],
        reach: Reach::Selection,
        needs: "targo-tippy",
        runs: "targo-tippy <selection> --all-targets -- -D warnings (+ aterm-gpu's wgpu-oracle arm)",
    },
    Spec {
        check: Check::Test,
        name: "test",
        stages: &[StageId::TestCompile, StageId::Test],
        reach: Reach::Selection,
        needs: "targo, fish, zsh; python3.11+ and openssl 3 (aterm-update), node (aterm-verify's glue)",
        runs: "targo test <selection> --tests (no examples), binaries in parallel",
    },
    Spec {
        check: Check::Doc,
        name: "doc",
        stages: &[StageId::Doctests],
        reach: Reach::Selection,
        needs: "targo, trustdoc",
        runs: "targo test --doc <selection>",
    },
    Spec {
        check: Check::Forge,
        name: "forge",
        stages: &[StageId::Forge],
        reach: Reach::WholeTree,
        needs: "targo",
        runs: "aterm-forge check: the third-party surface policy",
    },
    Spec {
        check: Check::Cells,
        name: "cells",
        stages: &[StageId::ForeignCells],
        reach: Reach::WholeTree,
        needs: "rustup stable + the foreign stds, clippy",
        runs: "the foreign cells (or each --cell), linted for their own triples (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::WasmSmoke,
        name: "wasm-smoke",
        stages: &[StageId::WasmSmoke],
        reach: Reach::WholeTree,
        needs: "node, rustup stable + wasm32",
        runs: "both web modules (dev) under node: their host boundary, then the CPU replay (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::WasmGlue,
        name: "wasm-glue",
        stages: &[StageId::WasmGlue],
        reach: Reach::WholeTree,
        needs: "node, rustup stable + wasm32, wasm-bindgen 0.2.108 (the oracle)",
        runs: "`verify glue` on aterm-wasm-abi's toy: goldens, node, a planted bug; the wasm-bindgen oracle (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::Browser,
        name: "browser",
        stages: &[StageId::BrowserLane],
        reach: Reach::WholeTree,
        needs: "a Chromium and a Firefox (`verify fetch-browser`), rustup stable + wasm32",
        runs: "both web modules (dev) in headless Chromium, WebGL2 on ANGLE Metal (macOS) and SwiftShader, and in headless Firefox; the GPU module on the page and in a worker (init_offscreen): a WebGL2 boot, GPU vs CPU pixels, GPU vs the recorded wgpu-WebGL frames (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::Libc,
        name: "libc",
        stages: &[StageId::LibcOracle],
        reach: Reach::WholeTree,
        needs: "targo",
        runs: "aterm-libc-oracle (this host's native ABI cell)",
    },
    Spec {
        check: Check::Spec,
        name: "spec",
        stages: &[StageId::SpecCheck],
        reach: Reach::WholeTree,
        needs: "ty, ay (aterm pkg install --default-set)",
        runs: "spec-check (ty over every registered model, ay over the proof bundles)",
    },
    Spec {
        check: Check::Checkers,
        name: "checkers",
        stages: &[StageId::CheckersCompile, StageId::SpecCheckers],
        reach: Reach::WholeTree,
        needs: "targo, ty",
        runs: "targo test --workspace --features aterm-spec/checkers (own target dir; after test)",
    },
    Spec {
        check: Check::GpuOracle,
        name: "gpu-oracle",
        stages: &[StageId::GpuOracleCompile, StageId::GpuOracle],
        reach: Reach::WholeTree,
        needs: "targo, a GPU wgpu can open",
        runs: "targo test -p aterm-gpu --features wgpu-oracle (own target dir; after test; with test when aterm-gpu is selected)",
    },
    Spec {
        check: Check::Appkit,
        name: "appkit",
        stages: &[StageId::RedrawConformance, StageId::ObjcDrives],
        reach: Reach::WholeTree,
        needs: "macOS, a free desktop",
        runs: "the redraw harness and the objc drives, built in the main target (windows)",
    },
    Spec {
        check: Check::Export,
        name: "export",
        stages: &[StageId::Export],
        reach: Reach::WholeTree,
        needs: "network, ~/publication, gitleaks, rustup trust, HEAD pushed",
        runs: "ship check --export (the publication engine's export guards)",
    },
    Spec {
        check: Check::Measure,
        name: "measure",
        stages: &[
            StageId::MeasurePrebuild,
            StageId::MeasuringTests,
            StageId::GuiSmoke,
        ],
        reach: Reach::WholeTree,
        needs: "--app <APP>, a free desktop",
        runs: "the ignored paint/spin rows on <APP>, GUI pacing smoke (alone)",
    },
    Spec {
        check: Check::CodexAlab,
        name: "codex-alab",
        stages: &[StageId::CodexAlabProof],
        reach: Reach::WholeTree,
        needs: "a packaged codex-alab (--package, LAB_CODEX, or the managed codex)",
        runs: "lab codex-alab-proof (alone; by hand): the native harness end to end on the real package",
    },
    Spec {
        check: Check::Linux,
        name: "linux",
        stages: &[StageId::LinuxLane],
        reach: Reach::WholeTree,
        needs: "Apple silicon with Rosetta, lima (brew install lima), network for the first provision",
        runs: "lab linux proof (alone; by hand): Linux tests on aarch64 and x86_64 (Rosetta), aterm-gui under \
               Xvfb, weston, Mutter, Xwayland in a lima VM (STOCK EXCEPTION), and LW.4's recorded baseline \
               replayed",
    },
];

/// The bare command's set: fast, and scoped to what changed.
pub const DEV: [Check; 5] = [
    Check::Fmt,
    Check::Census,
    Check::Lint,
    Check::Test,
    Check::Doc,
];

/// What publication requires: the whole workspace, and a skip is not a pass.
/// `cells` is not in it (owner ruling 2026-10-16): the release builds only the
/// macOS bundle, so nothing it builds is for the foreign triples, and a
/// Windows-only type error or a stable without a cross std stopped a macOS
/// release. `verify cells` runs by hand.
pub const RELEASE: [Check; 12] = [
    Check::Fmt,
    Check::Lint,
    Check::Test,
    Check::Doc,
    Check::Census,
    Check::Forge,
    Check::Libc,
    Check::Spec,
    Check::Checkers,
    Check::GpuOracle,
    Check::Appkit,
    Check::Measure,
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

/// The stages a selection of checks plans, each check's in order, without
/// repeats.
#[must_use]
pub fn stages_of(checks: &[Check]) -> Vec<StageId> {
    let mut v: Vec<StageId> = Vec::new();
    for id in checks.iter().flat_map(|c| c.spec().stages) {
        if !v.contains(id) {
            v.push(*id);
        }
    }
    v
}

/// The package whose tests and lint have a second arm behind a feature, and that
/// feature: aterm-gpu's renderer against the wgpu oracle. The default build
/// compiles none of the oracle's tests, so a selection that holds aterm-gpu
/// tests and lints that arm too ([`with_pulled`], the lint's oracle pass).
pub const ORACLE: (&str, &str) = ("aterm-gpu", "wgpu-oracle");

/// `wanted`, plus the checks a selection that holds [`ORACLE`]'s package pulls
/// in: `test` brings `gpu-oracle`, so the oracle's tests run wherever
/// aterm-gpu's do, never only at release.
#[must_use]
pub fn with_pulled(wanted: &[Check], selects_oracle: bool) -> Vec<Check> {
    let mut v = wanted.to_vec();
    if selects_oracle && v.contains(&Check::Test) && !v.contains(&Check::GpuOracle) {
        v.push(Check::GpuOracle);
    }
    v
}

/// The check a stage's rows count toward in this selection: the selected check
/// that runs it.
#[must_use]
pub fn owner(id: StageId, checks: &[Check]) -> Option<Check> {
    checks
        .iter()
        .find(|c| c.spec().stages.contains(&id))
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
        "\ndev      fmt census lint test doc: the bare command, on the change cone;\n\
         \x20        gpu-oracle too when the cone holds aterm-gpu\n\
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
        assert_eq!(lookup("browser"), Some(Word::Check(Check::Browser)));
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
    fn a_selection_holding_aterm_gpu_runs_its_oracle_with_its_tests() {
        assert_eq!(
            with_pulled(&DEV, true),
            [DEV.as_slice(), &[Check::GpuOracle]].concat()
        );
        assert_eq!(with_pulled(&DEV, false), DEV);
        assert_eq!(
            with_pulled(&[Check::Lint], true),
            [Check::Lint],
            "the lint's oracle arm is its own pass, not the oracle's tests"
        );
        assert_eq!(
            with_pulled(&RELEASE, true),
            RELEASE,
            "release has it already"
        );
    }
}
