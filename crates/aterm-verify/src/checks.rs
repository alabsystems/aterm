// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Definitions and sets for `aterm-verify` checks.

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
    /// The tools the machine must have for it to run (`aterm_uds::host::TOOLS`).
    pub needs: &'static [&'static str],
    /// What else it needs that is no tool: the network, a pushed HEAD, an argument.
    pub also: &'static str,
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
        needs: &["trustfmt"],
        also: "",
        runs: "trustfmt --check, each .rs at its crate's edition (--fix rewrites)",
    },
    Spec {
        check: Check::Census,
        name: "census",
        stages: &[StageId::Census],
        reach: Reach::WholeTree,
        needs: &[],
        also: "",
        runs: "aterm-census's source guards and its five L0 censuses",
    },
    Spec {
        check: Check::Lint,
        name: "lint",
        stages: &[StageId::Tippy],
        reach: Reach::Selection,
        needs: &["targo-tippy"],
        also: "",
        runs: "targo-tippy <selection> --all-targets -- -D warnings (+ aterm-gpu's wgpu-oracle arm; \
               under release, the required-features targets and -p aterm --release)",
    },
    Spec {
        check: Check::Test,
        name: "test",
        stages: &[StageId::TestCompile, StageId::Test],
        reach: Reach::Selection,
        needs: &[
            "targo",
            "cc",
            "bash",
            "zsh",
            "fish",
            "pwsh",
            "vim",
            "zstd",
            "chrome-headless-shell",
        ],
        also: "",
        runs: "targo test <selection> --tests (no examples), binaries in parallel",
    },
    Spec {
        check: Check::Doc,
        name: "doc",
        stages: &[StageId::Doctests],
        reach: Reach::Selection,
        needs: &["targo", "trustdoc"],
        also: "",
        runs: "targo test --doc <selection>",
    },
    Spec {
        check: Check::Forge,
        name: "forge",
        stages: &[StageId::Forge],
        reach: Reach::WholeTree,
        needs: &["targo"],
        also: "",
        runs: "aterm-forge check: the third-party surface policy",
    },
    Spec {
        check: Check::Cells,
        name: "cells",
        stages: &[StageId::ForeignCells],
        reach: Reach::WholeTree,
        needs: &["rustup"],
        also: "stable's foreign stds and clippy",
        runs: "the foreign cells (or each --cell), linted for their own triples; with a Windows cell, both \
               windows-sys oracles (aterm-win32-oracle's oracle and oracle-052) (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::WasmSmoke,
        name: "wasm-smoke",
        stages: &[StageId::WasmSmoke],
        reach: Reach::WholeTree,
        needs: &["chrome-headless-shell", "rustup"],
        also: "stable's wasm32",
        runs: "both web modules (dev) in headless Chromium: their host boundary, then the CPU replay (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::WasmGlue,
        name: "wasm-glue",
        stages: &[StageId::WasmGlue],
        reach: Reach::WholeTree,
        needs: &["chrome-headless-shell", "rustup", "wasm-bindgen"],
        also: "stable's wasm32",
        runs: "`verify glue` on aterm-wasm-abi's toy: goldens, its driver in headless Chromium, a planted bug; the wasm-bindgen oracle (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::Browser,
        name: "browser",
        stages: &[StageId::BrowserLane],
        reach: Reach::WholeTree,
        needs: &["chrome-headless-shell", "firefox", "rustup"],
        also: "stable's wasm32; on macOS Playwright's WebKit (`verify provision`), whose rows keep windows",
        runs: "both web modules (dev) in headless Chromium, WebGL2 on ANGLE Metal (macOS) and SwiftShader, in headless Firefox, and in Playwright's WebKit (it keeps a window: a named skip under --no-windows); the GPU module on the page and in a worker (init_offscreen): a WebGL2 boot, GPU vs CPU pixels, GPU vs the recorded wgpu-WebGL frames (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::Libc,
        name: "libc",
        stages: &[StageId::LibcOracle],
        reach: Reach::WholeTree,
        needs: &["targo"],
        also: "",
        runs: "aterm-libc-oracle (this host's native ABI cell)",
    },
    Spec {
        check: Check::Spec,
        name: "spec",
        stages: &[StageId::SpecCheck],
        reach: Reach::WholeTree,
        needs: &["ty", "ay"],
        also: "",
        runs: "spec-check (ty over every registered model, ay over the proof bundles)",
    },
    Spec {
        check: Check::Checkers,
        name: "checkers",
        stages: &[StageId::CheckersCompile, StageId::SpecCheckers],
        reach: Reach::WholeTree,
        needs: &["targo", "ty"],
        also: "",
        runs: "targo test --workspace --features aterm-spec/checkers (own target dir; after test)",
    },
    Spec {
        check: Check::GpuOracle,
        name: "gpu-oracle",
        stages: &[StageId::GpuOracleCompile, StageId::GpuOracle],
        reach: Reach::WholeTree,
        needs: &["targo", "gpu"],
        also: "",
        runs: "targo test -p aterm-gpu --features wgpu-oracle (own target dir; after test; with test when aterm-gpu is selected)",
    },
    Spec {
        check: Check::Appkit,
        name: "appkit",
        stages: &[StageId::RedrawConformance, StageId::ObjcDrives],
        reach: Reach::WholeTree,
        needs: &["desktop"],
        also: "macOS",
        runs: "the redraw harness and the objc drives, built in the main target (windows)",
    },
    Spec {
        check: Check::Export,
        name: "export",
        stages: &[StageId::Export],
        reach: Reach::WholeTree,
        needs: &["pub", "gitleaks", "git"],
        also: "network, rustup trust, HEAD pushed",
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
        needs: &["desktop"],
        also: "--app <APP>",
        runs: "aterm-conformance's paint/spin rows, then the GUI typing smoke, on <APP> (alone)",
    },
    Spec {
        check: Check::CodexAlab,
        name: "codex-alab",
        stages: &[StageId::CodexAlabProof],
        reach: Reach::WholeTree,
        needs: &["codex"],
        also: "a packaged codex-alab (--package, LAB_CODEX, or the managed codex)",
        runs: "lab codex-alab-proof (alone; by hand): the native harness end to end on the real package",
    },
    Spec {
        check: Check::Linux,
        name: "linux",
        stages: &[StageId::LinuxLane],
        reach: Reach::WholeTree,
        needs: &["rosetta", "limactl"],
        also: "network for the first provision",
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

/// The stages a selection of checks plans, each check's in order, without repeats.
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

/// The package whose tests and lint have a second arm behind a feature, and that feature.
pub const ORACLE: (&str, &str) = ("aterm-gpu", "wgpu-oracle");

/// The checks that judge what some packages' own tests cannot, each with those packages.
pub const PULLS: [(Check, &[&str]); 2] = [
    (Check::GpuOracle, &[ORACLE.0]),
    (
        Check::WasmSmoke,
        &["aterm-wasm", "aterm-gpu-web", "aterm-wasm-abi"],
    ),
];

/// The [`PULLS`] checks `wanted` lacks whose packages `holds` selects when `wanted` runs `test`.
#[must_use]
pub fn pulled(wanted: &[Check], holds: impl Fn(&str) -> bool) -> Vec<(Check, &'static str)> {
    if !wanted.contains(&Check::Test) {
        return Vec::new();
    }
    PULLS
        .iter()
        .filter(|(check, _)| !wanted.contains(check))
        .filter_map(|(check, packages)| packages.iter().find(|p| holds(p)).map(|p| (*check, *p)))
        .collect()
}

/// `wanted`, plus what it [`pulled`], in [`CHECKS`] order.
#[must_use]
pub fn with_pulled(wanted: &[Check], release: bool, holds: impl Fn(&str) -> bool) -> Vec<Check> {
    if release {
        return wanted.to_vec();
    }
    let pulled = pulled(wanted, holds);
    CHECKS
        .iter()
        .map(|s| s.check)
        .filter(|c| wanted.contains(c) || pulled.iter().any(|(p, _)| p == c))
        .collect()
}

/// The check a stage's rows count toward in this selection.
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

/// What `check` needs, as `list` prints it.
///
/// # Panics
///
/// When a need names no `aterm_uds::host::TOOLS` row.
#[must_use]
pub fn needs_of(check: Check) -> String {
    let spec = check.spec();
    let mut words: Vec<String> = spec
        .needs
        .iter()
        .map(|name| aterm_uds::host::tool(name).needs_label())
        .collect();
    if !spec.also.is_empty() {
        words.push(spec.also.to_string());
    }
    if words.is_empty() {
        "-".to_string()
    } else {
        words.join(", ")
    }
}

/// `verify list`'s second table: every tool a check needs, once, in first-mention order.
#[must_use]
pub fn tools_table() -> String {
    let mut names: Vec<&str> = Vec::new();
    for name in CHECKS.iter().flat_map(|s| s.needs) {
        if !names.contains(name) {
            names.push(name);
        }
    }
    let rows: Vec<[String; 3]> = names
        .iter()
        .map(|name| {
            let tool = aterm_uds::host::tool(name);
            [
                tool.name.to_string(),
                tool.provided_by(),
                tool.why.to_string(),
            ]
        })
        .collect();
    let head = ["TOOL", "FROM", "WHY"].map(String::from);
    let width = |i: usize| {
        rows.iter()
            .chain(std::iter::once(&head))
            .map(|r| r[i].chars().count())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1)];
    let mut out = String::new();
    for r in std::iter::once(&head).chain(&rows) {
        for (i, w) in widths.iter().enumerate() {
            out.push_str(&r[i]);
            out.push_str(&" ".repeat(w - r[i].chars().count() + 2));
        }
        out.push_str(&r[2]);
        out.push('\n');
    }
    out
}

/// `verify list`: every check, its sets, what it reads, needs and runs, then [`tools_table`].
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
                needs_of(s.check),
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
    out.push('\n');
    out.push_str(&tools_table());
    out.push_str(
        "\ndev      fmt census lint test doc: the bare command, on the change cone;\n\
         \x20        gpu-oracle too when the cone holds aterm-gpu, wasm-smoke when it\n\
         \x20        holds aterm-wasm or aterm-gpu-web (any crate either builds on) or\n\
         \x20        aterm-wasm-abi (their wasm32-only host boundary)\n\
         release  what `ship release` requires: --all implied, --app required, a skip is exit 3\n\
         perf     not a check: the performance lanes, `verify perf --list`\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::Scope;

    #[test]
    fn list_renders_each_checks_needs_from_the_tools_table() {
        let test = needs_of(Check::Test);
        for want in [
            "fish (verify provision)",
            "pwsh (verify provision)",
            "cc (CLT)",
            "zstd (unprovided)",
            "vim (OS)",
        ] {
            assert!(test.contains(want), "{want} in {test}");
        }
        // S11: the embedded verifier's tests run in the Linux guest, so the dev test check
        // needs neither of its tools on a Mac.
        assert!(
            !test.contains("python3") && !test.contains("openssl3"),
            "{test}"
        );
        assert_eq!(
            needs_of(Check::Linux),
            "Apple silicon with Rosetta, limactl (verify provision --linux), network for the first \
             provision"
        );
        assert_eq!(needs_of(Check::Census), "-");
        let table = list_table();
        assert!(!table.contains("brew"), "{table}");
        for spec in CHECKS {
            let needs = needs_of(spec.check);
            assert!(
                !spec.needs.contains(&"node") && !needs.split(", ").any(|w| w.starts_with("node ")),
                "{}: {needs}",
                spec.name
            );
        }
        assert!(!table.lines().any(|l| l.starts_with("node ")), "{table}");
        for check in [Check::Test, Check::WasmSmoke, Check::WasmGlue] {
            assert!(
                needs_of(check).contains("chrome-headless-shell (verify provision)"),
                "{}",
                needs_of(check)
            );
        }
        let fish = aterm_uds::host::tool("fish");
        let line = table
            .lines()
            .find(|l| l.starts_with("fish "))
            .unwrap_or_else(|| panic!("no fish row in {table}"));
        assert!(
            line.contains(" verify provision ") && line.ends_with(fish.why),
            "{line}"
        );
        assert_eq!(table.lines().filter(|l| l.starts_with("fish ")).count(), 1);
    }

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
        let gpu = |p: &str| p == "aterm-gpu";
        assert_eq!(
            with_pulled(&DEV, false, gpu),
            [DEV.as_slice(), &[Check::GpuOracle]].concat()
        );
        assert_eq!(with_pulled(&DEV, false, |_| false), DEV);
        assert_eq!(
            with_pulled(&[Check::Lint], false, gpu),
            [Check::Lint],
            "the lint's oracle arm is its own pass, not the oracle's tests"
        );
        assert_eq!(
            with_pulled(&RELEASE, true, |_| true),
            RELEASE,
            "release has it already, and runs its set and nothing more"
        );
    }

    #[test]
    fn a_selection_holding_a_web_module_runs_its_replay_with_its_tests() {
        use crate::changed::{Manifests, Member, Members, Replan, Selection, cone, select};
        const DEPENDENTS: [(&str, &str); 7] = [
            (
                "aterm-types",
                "aterm-grid aterm-core aterm-gui aterm-wasm aterm-gpu-web",
            ),
            (
                "aterm-grid",
                "aterm-core aterm-gui aterm-wasm aterm-gpu-web",
            ),
            ("aterm-core", "aterm-gui aterm-wasm aterm-gpu-web"),
            ("aterm-gui", ""),
            ("aterm-wasm", ""),
            ("aterm-gpu-web", ""),
            ("aterm-wasm-abi", ""),
        ];
        let members = Members::new(
            DEPENDENTS
                .iter()
                .map(|(n, _)| Member {
                    name: n.to_string(),
                    dir: format!("crates/{n}"),
                    has_lib: true,
                })
                .collect(),
        );
        let manifests: Manifests = DEPENDENTS
            .iter()
            .map(|(n, _)| (format!("crates/{n}"), Some(n.to_string())))
            .collect();
        let scope_after = |changed: &str| {
            let dependents = |name: &str| {
                let (_, users) = DEPENDENTS.iter().find(|(dep, _)| *dep == name)?;
                Some(
                    users
                        .split_whitespace()
                        .chain([name])
                        .map(String::from)
                        .collect(),
                )
            };
            let sel = select(
                &[format!("crates/{changed}/src/lib.rs")],
                &manifests,
                &members,
                &|| Some(Vec::new()),
                &dependents,
                &|_| Replan::Nothing,
            );
            assert!(
                matches!(sel, Selection::Narrowed { .. }),
                "{changed}: {sel:?}"
            );
            cone("origin/main", &sel).0
        };
        let dev_after = |changed: &str| {
            let scope = scope_after(changed);
            with_pulled(&DEV, false, |p| scope.includes_crate(p))
        };
        for engine in ["aterm-grid", "aterm-core", "aterm-types", "aterm-wasm"] {
            assert_eq!(
                dev_after(engine),
                [DEV.as_slice(), &[Check::WasmSmoke]].concat(),
                "a change to {engine}"
            );
        }
        assert_eq!(
            dev_after("aterm-gui"),
            DEV,
            "no web module builds on aterm-gui"
        );
        let abi = scope_after("aterm-wasm-abi");
        assert_eq!(
            pulled(&DEV, |p| abi.includes_crate(p)),
            [(Check::WasmSmoke, "aterm-wasm-abi")],
            "the modules' host boundary, which they build on wasm32 alone"
        );
        let core = scope_after("aterm-core");
        let holds = |p: &str| core.includes_crate(p);
        assert_eq!(pulled(&DEV, holds), [(Check::WasmSmoke, "aterm-wasm")]);
        assert_eq!(with_pulled(&[Check::Lint], false, holds), [Check::Lint]);
        assert_eq!(
            with_pulled(&[Check::Test, Check::WasmSmoke], false, holds),
            [Check::Test, Check::WasmSmoke],
            "named once"
        );
        let narrow = Scope::packages(&["aterm-core".to_string()]);
        assert_eq!(with_pulled(&DEV, false, |p| narrow.includes_crate(p)), DEV);
        assert_eq!(
            pulled(&DEV, |p| p == "aterm-gpu" || p == "aterm-gpu-web"),
            [
                (Check::GpuOracle, "aterm-gpu"),
                (Check::WasmSmoke, "aterm-gpu-web")
            ]
        );
        assert_eq!(
            with_pulled(&RELEASE, true, |_| true),
            RELEASE,
            "release runs its set: a stable wasm32 std is no release prerequisite"
        );
    }
}
