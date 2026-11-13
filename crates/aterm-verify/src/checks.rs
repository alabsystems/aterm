// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Definitions and sets for `aterm-verify` checks.

use std::path::Path;

use crate::plan::StageId;

/// One check, as named on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    Fmt,
    Census,
    ExportScan,
    ReleasedShim,
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
    /// The whole tree unless `-p` names crates: no cone or `--all` narrows it.
    TreeOrPackages,
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
    /// What else it needs that is no tool: the network, a mainline HEAD, an argument.
    pub also: &'static str,
    /// What it runs, in one line.
    pub runs: &'static str,
}

/// Every check, in the order `list` shows them and a verdict names them.
pub const CHECKS: [Spec; 20] = [
    Spec {
        check: Check::ReleasedShim,
        name: "released-shim",
        stages: &[StageId::ReleasedShim],
        reach: Reach::WholeTree,
        needs: &[],
        also: "macOS: installed release's targo shim and app helper, APFS, staged locked Trust build",
        runs: "on bootstrap input changes, installed shim/helper build aterm-time offline in a disposable checkout; personal caches and toolchains are read-only",
    },
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
        also: "in a blobless partial clone lacking the public baseline's harness version files, \
               network to import those blobs from its promisor remote (GitHub: an agh with \
               `repo fetch --objects`; up to 10 minutes)",
        runs: "aterm-census's source guards and its five L0 censuses",
    },
    Spec {
        check: Check::ExportScan,
        name: "export-scan",
        stages: &[StageId::ExportScan],
        reach: Reach::Selection,
        needs: &["gitleaks"],
        also: "one commit of the publication engine ($PUBLICATION_ENGINE's HEAD, else the main \
               of the origin the release's engine copy, else ~/publication, names, fetched)",
        runs: "the publication engine's content guards (forbidden content, private references, \
               gitleaks with its config policy and the engine's adjudication) over the export of \
               what the change touched (-p, --all, or a change under publish/, to \
               gitleaks' config or to the export's transforms: the whole export)",
    },
    Spec {
        check: Check::Lint,
        name: "lint",
        stages: &[StageId::Tippy],
        reach: Reach::Selection,
        needs: &["targo-tippy"],
        also: "",
        runs: "targo-tippy <selection> --all-targets -- -D warnings; a change cone has up to two \
               passes, that one over what the change touched and default targets over the rest \
               that link it, and leaves out what it holds only by a dev edge; then the \
               required-features targets (--features <pkg>/<feat>,… --all-targets) of the \
               selected packages that have them, in a cone of those it touched and those one \
               edge from them; also -p aterm --release whenever the selection reaches aterm",
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
            "bash-5.3",
            "zsh",
            "fish",
            "pwsh",
            "vim",
            "zstd",
            "gitleaks",
            "chrome-headless-shell",
        ],
        also: "",
        runs: "targo test <selection> --tests (no examples; under release, the measuring benches compiled too), binaries side by side within the run's CPU share, unseen first, then last run's failures, then longest known",
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
        reach: Reach::TreeOrPackages,
        needs: &["rustup"],
        also: "the checkout's stock pin with its foreign stds and clippy",
        runs: "every forge cell but this machine's own (or each --cell), its whole graph linted for its \
               own triple (-p: those crates alone); with a Windows cell and no -p or pulled selection \
               ruling them out, both windows-sys oracles (aterm-win32-oracle's oracle and oracle-052) \
               (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::WasmSmoke,
        name: "wasm-smoke",
        stages: &[StageId::WasmSmoke],
        reach: Reach::WholeTree,
        needs: &["chrome-headless-shell", "rustup"],
        also: "the stock pin's wasm32 std",
        runs: "both web modules (dev) in headless Chromium: their host boundary, then the CPU \
               replay; with test when aterm-wasm, aterm-gpu-web or aterm-wasm-abi is selected \
               (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::WasmGlue,
        name: "wasm-glue",
        stages: &[StageId::WasmGlue],
        reach: Reach::WholeTree,
        needs: &["chrome-headless-shell", "rustup"],
        also: "the stock pin's wasm32 std",
        runs: "`verify glue` on aterm-wasm-abi's toy: goldens, its driver in headless Chromium, a planted bug (STOCK EXCEPTION)",
    },
    Spec {
        check: Check::Browser,
        name: "browser",
        stages: &[StageId::BrowserLane],
        reach: Reach::WholeTree,
        needs: &["chrome-headless-shell", "firefox", "rustup"],
        also: "the stock pin's wasm32 std; on macOS and Linux arm64 Playwright's WebKit (`verify provision`), whose rows keep windows on macOS, and on macOS the system Safari, whose `safaridriver` an administrator enables once (`sudo safaridriver --enable`)",
        runs: "both web modules (dev) in headless Chromium, WebGL2 on ANGLE Metal (macOS) and SwiftShader, in headless Firefox, in Playwright's WebKit (on macOS it keeps a window: a named skip under --no-windows; on Linux, WPE, headless), and on macOS in the system Safari over safaridriver (its window takes the desk: it holds the quiet machine, and is a named skip under --no-windows); the GPU module on the page and in a worker (init_offscreen): a WebGL2 boot, GPU vs CPU pixels, GPU vs the recorded wgpu-WebGL frames (STOCK EXCEPTION)",
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
        runs: "spec-check (ty over every registered model, ay over the proof bundles; their \
               minutes-class obligations only under release)",
    },
    Spec {
        check: Check::Checkers,
        name: "checkers",
        stages: &[StageId::TestCompile, StageId::SpecCheckers],
        reach: Reach::Selection,
        needs: &["targo", "ty"],
        also: "",
        runs: "ATERM_SPEC_CHECKERS=1 reruns compiled tests (main target dir; after test; binaries side by side within the run's CPU share); under release, test asserts the variable itself",
    },
    Spec {
        check: Check::Appkit,
        name: "appkit",
        stages: &[StageId::RedrawConformance, StageId::ObjcDrives],
        reach: Reach::WholeTree,
        needs: &["desktop"],
        also: "macOS",
        runs: "the redraw harness and the objc drives (live-class audit, IME, toolbar, window, event, frame clock), \
               built in the main target (windows)",
    },
    Spec {
        check: Check::Export,
        name: "export",
        stages: &[StageId::Export],
        reach: Reach::WholeTree,
        needs: &["pub", "gitleaks", "git"],
        also: "network, rustup trust, HEAD on locally fetched origin/main",
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
        runs: "aterm-conformance's paint/spin/aos/rewind rows, then the GUI typing smoke, on <APP> (alone); \
               a row a bundle this checkout's HEAD did not build has no answer to is named, not judged",
    },
    Spec {
        check: Check::CodexAlab,
        name: "codex-alab",
        stages: &[StageId::CodexAlabProof],
        reach: Reach::WholeTree,
        needs: &["codex"],
        also: "a packaged codex-alab ($LAB_CODEX, or the managed codex)",
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
               Xvfb, weston, Mutter, Xwayland in a lima VM (STOCK EXCEPTION), and the recorded Linux \
               baseline replayed",
    },
];

/// The bare command's set: fast, and scoped to what changed.
pub const DEV: [Check; 7] = [
    Check::ReleasedShim,
    Check::Fmt,
    Check::Census,
    Check::ExportScan,
    Check::Lint,
    Check::Test,
    Check::Doc,
];

/// The release's tree half (`verify release --tree`): what the release's tree alone answers,
/// with no bundle to judge.
pub const RELEASE_TREE: [Check; 8] = [
    Check::Fmt,
    Check::Lint,
    Check::Test,
    Check::Doc,
    Check::Census,
    Check::Forge,
    Check::Libc,
    Check::Spec,
];

/// The release's bundle CLI half: AppKit keeps this canonical identity but the cutter can run it
/// before the build because it reads only the tree; measure takes `--app A` and waits for the
/// built bundle.
pub const RELEASE_BUNDLE: [Check; 2] = [Check::Appkit, Check::Measure];

/// What publication requires: the whole workspace, and a skip is not a pass. It is the union of
/// its two halves, [`RELEASE_TREE`] and [`RELEASE_BUNDLE`].
pub const RELEASE: [Check; RELEASE_TREE.len() + RELEASE_BUNDLE.len()] =
    joined(RELEASE_TREE, RELEASE_BUNDLE);

/// `a` then `b`, as one array.
const fn joined<const A: usize, const B: usize, const N: usize>(
    a: [Check; A],
    b: [Check; B],
) -> [Check; N] {
    let mut out = [Check::Fmt; N];
    let mut i = 0;
    while i < A {
        out[i] = a[i];
        i += 1;
    }
    while i < N {
        out[i] = b[i - A];
        i += 1;
    }
    out
}

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

    /// Does this check build its selection with this host's cargo: a stage that hands the `-p`
    /// specs to `targo` for this machine's target (the lint, the test build, the doctests)?
    #[must_use]
    pub fn builds_selection(self) -> bool {
        self.narrows()
            && self
                .spec()
                .stages
                .iter()
                .any(|s| matches!(s, StageId::Tippy | StageId::TestCompile | StageId::Doctests))
    }

    /// Does this check read `scope`, given how the run selected it?
    #[must_use]
    pub fn reads(self, scope: &crate::scope::Scope) -> bool {
        match self.spec().reach {
            Reach::Selection => true,
            Reach::WholeTree => false,
            Reach::TreeOrPackages => matches!(scope, crate::scope::Scope::Packages(_)),
        }
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

/// The package that holds each GPU arm's frames to their CPU twins, beside the
/// record of wgpu's (docs/AUDIT-third-party-2026-10-15.md GW.6 and
/// ORACLE-ADAPTER), and the test that compares them: a release's test run passes
/// only when it ran ([`crate::stages::compared_no_frame`]).
pub const GOLDEN: (&str, &str) = (
    "aterm-gpu",
    "frame_record_tests::armed_frames_hold_to_their_cpu_twins",
);

/// The checks that judge what some packages' own tests cannot, each with those packages.
pub const PULLS: [(Check, &[&str]); 1] = [(
    Check::WasmSmoke,
    &["aterm-wasm", "aterm-gpu-web", "aterm-wasm-abi"],
)];

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

fn forge_pull_reason(
    wanted: &[Check],
    alone: bool,
    packages: impl FnOnce() -> Result<Vec<String>, String>,
    touches: impl Fn(&str) -> bool,
) -> Option<String> {
    if alone || wanted != DEV {
        return None;
    }
    match packages() {
        Ok(packages) => packages.into_iter().find(|package| touches(package)),
        Err(_) => Some("the fork roster is unreadable (fail-closed)".to_string()),
    }
}

/// Why a dev selection pulls [`Check::Forge`], if it does. A touched package is read from the
/// fork policy or absorbed-crate registry. Changes to the notary itself also pull its real
/// attestation; an unreadable registry pulls Forge fail-closed so its own diagnostic is the
/// verdict. Explicit individual checks and release selections run exactly what they name.
#[must_use]
pub fn forge_pull(
    wanted: &[Check],
    alone: bool,
    root: &Path,
    touches: impl Fn(&str) -> bool,
) -> Option<String> {
    forge_pull_reason(
        wanted,
        alone,
        || {
            let mut packages = aterm_census::scan_set::fork_packages(root)?;
            // Provenance policy is itself an input to the attestation. A
            // reviewed first-party roster change must run its real notary.
            packages.push("aterm-forge".into());
            Ok(packages)
        },
        touches,
    )
}

/// Why an ordinary macOS dev selection pulls [`Check::Appkit`], if it does. The live drivers
/// are the tests of `aterm-gui`'s AppKit code and `aterm-window`'s delegates that libtest cannot
/// run on its worker threads. A cone that holds either package—directly or because it depends on
/// a changed input—pulls the windowed check. Explicit individual checks and release selections
/// run exactly what they name.
#[must_use]
pub fn appkit_pull(
    wanted: &[Check],
    alone: bool,
    macos: bool,
    holds: impl Fn(&str) -> bool,
) -> Option<&'static str> {
    if !macos || alone || wanted != DEV {
        return None;
    }
    ["aterm-gui", "aterm-window"]
        .into_iter()
        .find(|package| holds(package))
}

/// `wanted`, plus what it [`pulled`], in [`CHECKS`] order; `alone` is `wanted` as it is
/// ([`crate::cli::Args::wanted`]).
#[must_use]
pub fn with_pulled(wanted: &[Check], alone: bool, holds: impl Fn(&str) -> bool) -> Vec<Check> {
    if alone {
        return wanted.to_vec();
    }
    let pulled = pulled(wanted, holds);
    CHECKS
        .iter()
        .map(|s| s.check)
        .filter(|c| wanted.contains(c) || pulled.iter().any(|(p, _)| p == c))
        .collect()
}

/// Whether a run of `wanted` pulls [`Check::Cells`]: a dev run that lints a selection which
/// compiles something, and does not name `cells` itself. Another OS's arms of the code (its
/// `cfg`s, the test helpers only its targets compile) are linted on that OS's triple alone, so
/// a change this machine's lint and tests pass can still break those builds, as the cells
/// fixes 78f54c26c7, 266437995c and 333be891e9 each repaired on main. The pulled run checks one
/// cell per other OS, each only where its graph holds a selected crate, and of that graph `-p`'s
/// crates, or the cone and what the change reaches there ([`crate::cells::pulled_cells`],
/// [`crate::cells::narrows`]); it is not red on a Windows cell's test-target warnings
/// ([`crate::cells::leaves_test_warnings`]). `verify cells` checks every cell's whole graph.
#[must_use]
pub fn pulls_cells(wanted: &[Check], release: bool, compiles: bool) -> bool {
    !release && compiles && wanted.contains(&Check::Lint) && !wanted.contains(&Check::Cells)
}

/// `wanted` with `check` added, in [`CHECKS`] order.
#[must_use]
pub fn with(wanted: &[Check], check: Check) -> Vec<Check> {
    CHECKS
        .iter()
        .map(|s| s.check)
        .filter(|c| *c == check || wanted.contains(c))
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
    if RELEASE_TREE.contains(&check) {
        v.push("release --tree");
    }
    if RELEASE_BUNDLE.contains(&check) {
        v.push("release --bundle");
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
    if check == Check::Test {
        let release = RELEASE_TEST_NEEDS.map(|name| aterm_uds::host::tool(name).needs_label());
        words.push(format!("under release {}", release.join(", ")));
    }
    if !spec.also.is_empty() {
        words.push(spec.also.to_string());
    }
    if words.is_empty() {
        "-".to_string()
    } else {
        words.join(", ")
    }
}

/// What the `test` check needs only under `release`, whose run carries `ATERM_SPEC_CHECKERS=1`
/// (`ty`) and has to reach the [`GOLDEN`] test (a GPU adapter); either missing is its COULD NOT RUN.
pub const RELEASE_TEST_NEEDS: [&str; 2] = ["ty", "gpu"];

/// `verify list`'s second table: every tool a check needs, once, in first-mention order.
#[must_use]
pub fn tools_table() -> String {
    let mut names: Vec<&str> = Vec::new();
    for name in CHECKS
        .iter()
        .flat_map(|s| s.needs)
        .chain(&RELEASE_TEST_NEEDS)
    {
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
                Reach::TreeOrPackages => "tree or -p",
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
        "\nEvery check needs targo: without it verify runs none (COULD NOT RUN, exit 3).\n\
         Every run first holds vendor/astream to forge's OB-1 pin rule: checked out at its\n\
         gitlink, unmodified, and not moved back from origin/main's pin at the merge-base\n\
         unless a commit says `astream-pin-rollback: <reason>` (else FAIL, exit 1, none run).\n\
         \n\
         dev      fmt census export-scan lint test doc: the bare command, on the change cone\n\
         \x20        any run with test (but release) adds wasm-smoke when its selection\n\
         \x20        (the cone, -p or --all) holds aterm-wasm or aterm-gpu-web (on the cone:\n\
         \x20        a change to any crate either builds on) or aterm-wasm-abi (their\n\
         \x20        wasm32-only host boundary)\n\
         release  what `ship release` requires: --all implied, --app required, a skip is exit 3\n\
         perf     not a check: the performance lanes, `verify perf --list`\n",
    );
    out
}

#[cfg(test)]
#[path = "checks_tests.rs"]
mod tests;
