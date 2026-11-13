// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stages themselves.

use crate::exec::{self, Cmd};
use crate::ladder::{Outcome, Report, Severity};
use crate::plan::{Lane, StageId, StageSpec, lane_dir};
use crate::scope::Scope;
#[cfg(unix)]
use crate::smoke_stages;
use crate::{Ctx, is_executable_file};

#[cfg(test)]
#[path = "lint_qualification_tests.rs"]
mod lint_qualification_tests;

/// Dispatch one stage.
#[must_use]
pub fn run_stage(ctx: &Ctx, spec: &StageSpec) -> Report {
    run_stage_prepared(ctx, spec, None)
}

pub(crate) fn run_stage_prepared(
    ctx: &Ctx,
    spec: &StageSpec,
    cells: Option<crate::cells::PreparedRun>,
) -> Report {
    let mut r = Report::new(spec.title.clone());
    if let Err(why) = crate::test_workers::admitted_share(&ctx.env) {
        r.cannot_run(format!("{why}; no stage was run"));
        return r;
    }
    if ctx.no_windows
        && let Some(why) = spec.id.no_windows_skip()
    {
        r.skip(why);
        return r;
    }
    if matches!(spec.id, StageId::RedrawConformance | StageId::ObjcDrives) && (ctx.screen_locked)()
    {
        // Each builds what it drives into the main lane's build directory: a run that skips
        // one reports less of that directory than a run of its selection that does not, and
        // its record says so ([`crate::disk::note_skipped`]).
        crate::disk::note_skipped(
            ctx,
            if matches!(spec.id, StageId::RedrawConformance) {
                "redraw-conformance"
            } else {
                "objc-drives"
            },
        );
        r.skip("native AppKit checks require an unlocked console; the console session is locked");
        return r;
    }
    match spec.id {
        StageId::ReleasedShim => crate::released_shim::run(ctx, &mut r),
        StageId::TestCompile => test_compile(ctx, spec, &mut r),
        StageId::Test => test(ctx, &mut r),
        StageId::MeasuringTests => measuring_tests(ctx, &mut r),
        StageId::Doctests => doctests(ctx, &mut r),
        StageId::Tippy => tippy(ctx, &mut r),
        StageId::Formatting => crate::fmt::run(ctx, &mut r),
        StageId::Export => export(ctx, &mut r),
        StageId::LibcOracle => libc_oracle(ctx, &mut r),
        StageId::Census => census(ctx, &mut r),
        StageId::ExportScan => {
            if in_checkout(ctx, &mut r, "export-scan") {
                crate::export_scan::run(ctx, &mut r);
            }
        }
        StageId::SpecCheck => spec_check(ctx, &mut r),
        StageId::SpecCheckers => spec_checkers(ctx, &mut r),
        StageId::MeasurePrebuild => measure_prebuild(ctx, &mut r),
        #[cfg(unix)]
        StageId::GuiSmoke => smoke_stages::gui_typing_smoke(ctx, &mut r),
        #[cfg(not(unix))]
        StageId::GuiSmoke => {
            r.skip(format!("{} (unix only)", spec.title));
        }
        StageId::RedrawConformance => redraw_conformance(ctx, &mut r),
        StageId::ObjcDrives => objc_drives(ctx, &mut r),
        StageId::ForeignCells => foreign_cells(ctx, &mut r, cells),
        StageId::WasmSmoke => {
            if in_checkout(ctx, &mut r, "wasm-smoke") {
                crate::wasm_smoke::run(ctx, &mut r);
            }
        }
        StageId::WasmGlue => {
            if in_checkout(ctx, &mut r, "wasm-glue") {
                wasm_glue(ctx, &mut r);
            }
        }
        #[cfg(unix)]
        StageId::BrowserLane => {
            if in_checkout(ctx, &mut r, "browser") {
                crate::browser::run(ctx, &mut r);
            }
        }
        #[cfg(not(unix))]
        StageId::BrowserLane => {
            r.cannot_run(format!("{} (unix only)", spec.title));
        }
        StageId::Forge => forge_gate(ctx, &mut r),
        StageId::CodexAlabProof => codex_alab_proof(ctx, &mut r),
        StageId::LinuxLane => linux_lane(ctx, &mut r),
    }
    r
}

/// `targo --unverified test <scope> --no-fail-fast --tests --no-run` — the test compile row's child: compile every test target and run nothing.
#[must_use]
pub fn test_compile_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.extend(TEST_TARGETS.map(String::from));
    a.push("--no-run".to_string());
    a.push("--message-format=json-render-diagnostics".to_string());
    a
}

/// One cut of a shaped test compile ([`crate::shape::Part`]): [`test_compile_args`] over the
/// cut's crates, with `--lib` in place of `--tests` for a lib-test call.
#[must_use]
pub fn piece_compile_args(piece: &crate::shape::Piece) -> Vec<String> {
    let mut a = test_compile_args(&Scope::packages(&piece.crates));
    if piece.lib {
        for arg in &mut a {
            if arg == "--tests" {
                "--lib".clone_into(arg);
            }
        }
    }
    a
}

/// What `targo` resolves a call's unit graph with, building nothing: the call's own arguments and
/// `--unit-graph -Z unstable-options`.
pub const UNIT_GRAPH: [&str; 3] = ["--unit-graph", "-Z", "unstable-options"];

/// The unit graph a test compile's call over `piece` would build ([`UNIT_GRAPH`]), resolved under
/// the main lane's build directory and the test compile's environment ([`test_compile_cmd`]). A
/// closing call resolves the exact ordinary command, including folded measuring benches.
/// A cut resolves only [`piece_compile_args`]. Why not: a child failed, printed no version-1 graph,
/// or printed one that could not be read.
///
/// # Errors
/// As above.
pub fn unit_graph(
    ctx: &Ctx,
    piece: &crate::shape::Piece,
    closing: bool,
    interner: &mut crate::unitgraph::Interner,
) -> Result<crate::unitgraph::Graph, String> {
    crate::test_env::prepare(ctx)
        .map_err(|why| format!("cannot prepare release test environment: {why}"))?;
    let (_, bind) = test_doc_binding(ctx);
    let mut commands = Vec::new();
    if closing {
        let mut args = test_compile_args(&ctx.scope);
        if crate::plan::folds_measuring(ctx) {
            args.extend(measuring_bench_args());
        }
        args.extend(UNIT_GRAPH.map(String::from));
        commands.push(test_compile_cmd(ctx, bind, args));
    } else {
        let mut args = piece_compile_args(piece);
        args.extend(UNIT_GRAPH.map(String::from));
        commands.push(test_compile_cmd(ctx, bind, args));
    }
    let mut graphs = Vec::with_capacity(commands.len());
    for cmd in commands {
        let out = exec::run(&cmd, ctx.exec_env());
        let graph = out
            .output
            .lines()
            .find(|line| line.starts_with("{\"version\":1"));
        let Some(graph) = graph.filter(|_| out.ok) else {
            return Err(format!(
                "`targo {}` {}",
                cmd.argv()[1..].join(" "),
                out.trimmed_output()
                    .lines()
                    .rfind(|line| !line.trim().is_empty())
                    .unwrap_or("printed no unit graph")
            ));
        };
        graphs.push(
            crate::unitgraph::Graph::read(graph, interner).map_err(|why| {
                format!(
                    "`targo {}` printed an invalid unit graph: {why}",
                    cmd.argv()[1..].join(" ")
                )
            })?,
        );
    }
    Ok(crate::unitgraph::Graph::union(graphs))
}

/// `targo --unverified test <scope> --no-fail-fast --tests`: the second child, run only when the first compiled: every lib, bin and integration test target with `test = true` ([`TEST_TARGETS`], which the first child compiles, beside the measuring benches where it builds them: [`crate::plan::folds_measuring`]), no doctests (the doctests stage runs those) and no examples or benches.
#[must_use]
pub fn test_run_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.extend(TEST_TARGETS.map(String::from));
    a
}

/// The targets both test children select: every target with `test = true` (the libs, the bins and the integration tests that keep the default) and no example ([`test_compile_args`]); the release's compile child adds the measuring benches ([`measuring_bench_args`]).
pub const TEST_TARGETS: [&str; 1] = ["--tests"];

/// The measuring executables selected by the runner and its content receipts.
pub const MEASURING_TARGETS: [&str; 4] = ["paint", "spin", "aos", "rewind"];

/// `targo --unverified bench --profile test -p aterm-conformance --no-fail-fast --bench paint --bench spin --bench aos --bench rewind`: the measuring stage's one child, the paint, spin, aos and rewind matrices ([`MEASURING_TARGETS`]) that judge the release bundle's frames, idle wakes, repair main-thread time and Rewind step latency on a live instance.
#[must_use]
pub fn measuring_args() -> Vec<String> {
    measuring_targets_args()
}

/// [`measuring_args`]'s compile alone (`--no-run`): the measuring rows' prebuild, the main lane's last row before the first barrier, so the exclusive stage that runs them compiles nothing. Planned only where no workspace test compile builds them ([`crate::plan::folds_measuring`]).
#[must_use]
pub fn measure_prebuild_args() -> Vec<String> {
    let mut a = measuring_targets_args();
    a.push("--no-run".to_string());
    a
}

/// `--bench paint --bench spin --bench aos --bench rewind`: the measuring matrices' targets
/// ([`MEASURING_TARGETS`]), which the workspace's test compile adds where it builds them
/// ([`crate::plan::folds_measuring`]).
#[must_use]
pub fn measuring_bench_args() -> Vec<String> {
    MEASURING_TARGETS
        .iter()
        .flat_map(|t| ["--bench".to_string(), (*t).to_string()])
        .collect()
}

fn measuring_targets_args() -> Vec<String> {
    let mut a: Vec<String> = [
        "--unverified",
        "bench",
        MAIN_PROFILE[0],
        MAIN_PROFILE[1],
        "-p",
        "aterm-conformance",
        "--no-fail-fast",
    ]
    .map(String::from)
    .into();
    a.extend(measuring_bench_args());
    a
}

fn test_base_args(scope: &Scope) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "test".to_string()];
    a.extend(scope.args());
    a.push("--no-fail-fast".to_string());
    a
}

/// `targo --unverified test --doc <scope> --no-fail-fast` — the only doctest runner (the test stage runs no doctests), over the test compile's own selection ([`Scope::args`]), never a narrower one: cargo resolves features over what a command selects, so the doctests then link the very units the test compile built instead of rebuilding their own ([`crate::docdriver`] has the measurement).
#[must_use]
pub fn doctest_args(scope: &Scope) -> Vec<String> {
    let mut a = vec![
        "--unverified".to_string(),
        "test".to_string(),
        "--doc".to_string(),
    ];
    a.extend(scope.args());
    a.push("--no-fail-fast".to_string());
    a
}

/// The scope's libraries by whether rustdoc reads them ([`doctest_split`]), each list by package name, sorted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocSplit {
    /// The libraries whose sources hold a doctest, or that the scan could not read or find ([`crate::docscan`] fails wide).
    pub run: Vec<String>,
    /// The libraries whose sources hold none: cargo still builds each (with the features the test compile resolved), and the stand-in skips its pass ([`crate::docdriver`]).
    pub skip: Vec<String>,
}

/// The doctests' split of the scope's libraries: each member with a lib whose sources hold a doctest runs, each whose sources the scan proves hold none is skipped.
#[must_use]
pub fn doctest_split(ctx: &Ctx) -> Option<DocSplit> {
    let dirs = workspace_member_dirs(&ctx.root);
    let mut names: Vec<String> = match &ctx.scope {
        Scope::Workspace if dirs.is_empty() => return None,
        Scope::Workspace => dirs.iter().map(|(name, _)| name.clone()).collect(),
        s => s
            .crates()
            .iter()
            .map(|c| crate::scope::spec_name(c).unwrap_or(c).to_string())
            .collect(),
    };
    names.retain(|name| crate::changed::crate_dir_has_lib(&ctx.root, name));
    names.sort();
    names.dedup();
    let (run, skip) = names.into_iter().partition(|name| {
        dirs.iter()
            .find(|(n, _)| n == name)
            .is_none_or(|(_, dir)| crate::docscan::src_holds_a_doctest(&dir.join("src")))
    });
    Some(DocSplit { run, skip })
}

fn workspace_member_dirs(root: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let mut v: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let text = crate::read_regular_text(e.path().join("Cargo.toml")).ok()?;
            if text.lines().any(|l| l.trim() == "[workspace]") {
                return None;
            }
            Some((crate::changed::manifest_package_name(&text)?, e.path()))
        })
        .collect();
    v.sort();
    v
}

/// `<tippy> <scope> --all-targets --keep-going -- -D warnings` ([`LINT_TAIL`]).
#[must_use]
pub fn tippy_args(scope: &Scope) -> Vec<String> {
    let mut a = scope.args();
    a.extend(LINT_TAIL.map(String::from));
    a
}

/// The lint's ordinary passes. A cone (no `-p`, no `--all`) has up to two: `--all-targets` for what
/// the change touched ([`crate::scope::Changed::all_targets`]), and each crate's default targets,
/// its library and binaries, for the rest that link the change. What the cone holds only by a dev
/// edge is in neither ([`crate::scope::Changed::unlinted`]), since its library and binaries link
/// nothing that changed; a root manifest row, a lock row other than a touched member's own entry,
/// or a fork keeps it in the first pass with the whole cone. `test` compiles the test targets of
/// all of those, so an error in one still stops the run. The `required-features` targets of what
/// the change touched and of what depends on it directly have a pass of their own
/// ([`lint_invocations`]). GAP: the other benches and examples of unchanged dependents (at most the
/// workspace's 104 targets, 19 of them with macOS arms, on 2026-11-04) have
/// no compile for this machine in this run, only a pulled cell's type-check on another OS, and a
/// lint that fires only in one of them or in such a crate's test target waits for `verify lint -p`
/// or `--all`, though a rustc warning there (a `#[must_use]` newly discarded) reds a pulled cell
/// that judges test-target warnings ([`crate::cells::leaves_test_warnings`]).
#[must_use]
pub fn tippy_passes(scope: &Scope) -> Vec<TippyPass> {
    let Scope::Changed(cone) = scope else {
        return vec![TippyPass {
            label: scope.label(),
            args: tippy_args(scope),
        }];
    };
    if cone.crates.is_empty() {
        return vec![TippyPass {
            label: scope.label(),
            args: tippy_args(scope),
        }];
    }
    let (every, rest): (Vec<&String>, Vec<&String>) = cone.linted().partition(|spec| {
        crate::scope::spec_name(spec).is_none_or(|name| cone.all_targets.iter().any(|n| n == name))
    });
    let pass = |specs: &[&String], all_targets: bool| {
        let selection: Vec<String> = specs
            .iter()
            .flat_map(|spec| ["-p".to_string(), (*spec).clone()])
            .collect();
        let mut args = selection.clone();
        args.extend(
            LINT_TAIL
                .iter()
                .filter(|flag| all_targets || **flag != "--all-targets")
                .map(ToString::to_string),
        );
        TippyPass {
            label: format!(
                "{}{}",
                selection.join(" "),
                if all_targets { " --all-targets" } else { "" }
            ),
            args,
        }
    };
    let mut passes = Vec::new();
    if !every.is_empty() {
        passes.push(pass(&every, true));
    }
    if !rest.is_empty() {
        passes.push(pass(&rest, false));
    }
    passes
}

/// One lint invocation ([`tippy_passes`], [`lint_invocations`]): its arguments and how the report
/// names its selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TippyPass {
    pub label: String,
    pub args: Vec<String>,
}

/// What both lint passes end with: cargo's coverage flags, then the lint driver's arguments, which reach workspace members only (targo-tippy hands them to `tippy-driver`, cargo's `RUSTC_WORKSPACE_WRAPPER`), so the dependencies stay fresh.
pub const LINT_TAIL: [&str; 5] = ["--all-targets", "--keep-going", "--", "-D", "warnings"];

/// The workspace's `required-features` targets, as the `(package, feature)` pairs that switch them on, read from the members' manifests (`crates/*/Cargo.toml`), sorted and without repeats.
#[must_use]
pub fn gated_lint_features(root: &std::path::Path) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| crate::read_regular_text(e.path().join("Cargo.toml")).ok())
        .flat_map(|text| manifest_required_features(&text))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The `(package, feature)` pairs one manifest's `required-features` keys name, under the `[package]` name (what `-p` takes).
#[must_use]
pub fn manifest_required_features(text: &str) -> Vec<(String, String)> {
    let Some(pkg) = crate::changed::manifest_package_name(text) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut lines = text.lines();
    while let Some(raw) = lines.next() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some(rest) = line
            .strip_prefix("required-features")
            .and_then(|r| r.trim_start().strip_prefix('='))
        else {
            continue;
        };
        let mut list = rest.to_string();
        while !list.contains(']') {
            let Some(more) = lines.next() else { break };
            list.push(' ');
            list.push_str(more.split('#').next().unwrap_or(""));
        }
        let inner = list.split_once('[').map_or("", |(_, r)| r);
        for feat in inner.split(']').next().unwrap_or("").split(',') {
            let feat = feat.trim().trim_matches(['"', '\'']);
            if !feat.is_empty() {
                found.push((pkg.clone(), feat.to_string()));
            }
        }
    }
    found
}

/// `<tippy> -p <pkg>… --features <pkg/feat>,… <LINT_TAIL>`: the pass over the targets [`tippy_args`] cannot reach (`gated`, [`gated_lint_features`]), of the packages whose gated targets the selection compiles ([`Scope::gated_spec_of`]). `None` when it holds none.
#[must_use]
pub fn tippy_gated_args(scope: &Scope, gated: &[(String, String)]) -> Option<Vec<String>> {
    let mut packages: Vec<String> = Vec::new();
    let mut features: Vec<String> = Vec::new();
    for (pkg, feat) in gated {
        let Some(spec) = scope.gated_spec_of(pkg) else {
            continue;
        };
        if !packages.contains(&spec) {
            packages.push(spec);
        }
        features.push(format!("{pkg}/{feat}"));
    }
    if packages.is_empty() {
        return None;
    }
    let mut a: Vec<String> = packages
        .into_iter()
        .flat_map(|p| ["-p".to_string(), p])
        .collect();
    a.push("--features".to_string());
    a.push(features.join(","));
    a.extend(LINT_TAIL.map(String::from));
    Some(a)
}

/// Every lint invocation `scope` runs, in order, each with its row's words: the ordinary passes
/// ([`tippy_passes`]); the required-features targets of the packages whose gated targets it
/// compiles ([`tippy_gated_args`] over `gated`, [`gated_lint_features`]), under the same feature
/// unification ([`lint_unified`]), so it compiles again only what a feature changes; and the
/// shipped build's ([`tippy_release_args`]).
#[must_use]
pub fn lint_invocations(scope: &Scope, gated: &[(String, String)]) -> Vec<TippyPass> {
    let mut runs: Vec<TippyPass> = tippy_passes(scope)
        .into_iter()
        .map(|pass| TippyPass {
            label: pass.label,
            args: lint_unified(pass.args),
        })
        .collect();
    if let Some(args) = tippy_gated_args(scope, gated) {
        let sep = args.iter().position(|a| a == "--features").unwrap_or(0);
        runs.push(TippyPass {
            label: format!("required-features targets {}", args[..sep].join(" ")),
            args: lint_unified(args),
        });
    }
    if let Some(args) = tippy_release_args(scope) {
        runs.push(TippyPass {
            label: format!("-p {SHIPPED} --release"),
            args,
        });
    }
    runs
}

/// The package of the one shipped executable, which `ship` builds `--release`.
pub const SHIPPED: &str = "aterm";

/// `<tippy> -p aterm --release --keep-going -- -D warnings`: every selection reaching the
/// shipped executable also lints the build as `ship` builds it, with debug assertions off.
#[must_use]
pub fn tippy_release_args(scope: &Scope) -> Option<Vec<String>> {
    let spec = scope.spec_of(SHIPPED)?;
    let a = [
        "-p",
        spec.as_str(),
        "--release",
        "--keep-going",
        "--",
        "-D",
        "warnings",
    ];
    Some(a.map(String::from).into())
}

/// `targo --unverified run -q --profile gate -p aterm-release -- <verb>`: the release tools, built as the `ship` alias builds them.
#[must_use]
pub fn ship_args(verb: impl IntoIterator<Item = impl Into<String>>) -> Vec<String> {
    [
        "--unverified",
        "run",
        "-q",
        "--profile",
        "gate",
        "-p",
        "aterm-release",
        "--",
    ]
    .into_iter()
    .map(String::from)
    .chain(verb.into_iter().map(Into::into))
    .collect()
}

/// The redraw harness's target name — the `[[bin]]`, the built file and the argv below all have to agree, so they read it from here.
pub const REDRAW_CONFORMANCE_BIN: &str = "aterm-redraw-conformance";

/// The profile of every binary the run builds in the main target to drive (`spec-check`, the redraw harness, the objc drivers, the Linux lane's lab). This profile and the workspace's feature unification ([`main_targo`]) must both match the test compile's to reuse its dependencies.
pub const MAIN_PROFILE: [&str; 2] = ["--profile", "test"];

/// `targo --unverified build --profile test -q -p aterm-gui --bin aterm-redraw-conformance`, in the main target with no feature: the harness's conformance seam is compiled in wherever debug assertions are on.
#[must_use]
pub fn redraw_conformance_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        MAIN_PROFILE[0],
        MAIN_PROFILE[1],
        "-q",
        "-p",
        "aterm-gui",
        "--bin",
        REDRAW_CONFORMANCE_BIN,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

#[path = "objc_pass.rs"]
mod objc_pass;

/// How an objc driver's exit `3` reads, the one code whose meaning is the driver's own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Code3 {
    /// A pass that claims less than `0`, worded apart from it so no line claims what went unmeasured: the class audit checked some rows against the fork's own declaration because this host's AppKit registers no protocol for them; the IME drive's Option-as-Alt stage ran without a control that composed, so whether Option as Alt stops a dead key was not measured.
    WeakerPassOk(&'static str),
    /// The driver's watchdog fired (a modal tracking loop never returned), so it decided nothing: could not run.
    Hung,
    /// The driver's signal trap turned an abort into `3`, and for this driver the abort is the finding it exists to make.
    AbortFinding,
    /// The driver never answers `3`: an unexpected code, a finding.
    None,
}

/// One objc driver: an `[[example]]` that owns `fn main`, because libtest cannot host AppKit (every `#[test]` body runs off the main thread, where `EventLoop` construction panics).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjcDrive {
    pub example: &'static str,
    pub code3: Code3,
    /// How a death with no exit status (a signal the driver did not trap) reads.
    pub no_status: Severity,
}

/// The objc drivers built together, then run in order on macOS regardless of
/// scope. A build failure runs none; a runtime failure does not hide later rows.
pub const OBJC_DRIVES: [ObjcDrive; 7] = [
    ObjcDrive {
        example: "objc_live_class_audit",
        code3: Code3::WeakerPassOk(
            "every registered row agrees — and at least one was checked against THIS FORK'S OWN DECLARATION, not this host's runtime, because its AppKit does not register the protocol that declares it (the auditor's part F and VERDICT lines name each one; exit 3)",
        ),
        no_status: Severity::CouldNotRun,
    },
    ObjcDrive {
        example: "objc_ime_drive",
        code3: Code3::WeakerPassOk(
            "a composition ran through the ported WinitView's NSTextInputClient rows, and with Option as Alt, each Option chord arrived as its bare key — but the same chords WITHOUT Option as Alt did not all compose here (macOS did not make the drive the active app, or this layout's Option chords are not the U.S. dead keys), so whether Option as Alt stops each dead key was NOT measured (exit 3)",
        ),
        no_status: Severity::CouldNotRun,
    },
    ObjcDrive {
        example: "objc_toolbar_drive",
        code3: Code3::Hung,
        no_status: Severity::CouldNotRun,
    },
    ObjcDrive {
        example: "objc_window_drive",
        code3: Code3::None,
        // A death by a signal is a crash in the window code the drive runs: a finding. Closing an answered NSAlert's glass child window at exit (aterm 0.110.0 on macOS 27.2) died by SIGSEGV in the exit turn's pool pop.
        no_status: Severity::GateFailed,
    },
    ObjcDrive {
        example: "objc_event_drive",
        code3: Code3::AbortFinding,
        no_status: Severity::GateFailed,
    },
    ObjcDrive {
        example: "objc_frame_clock_drive",
        code3: Code3::Hung,
        // A death by a signal is a crash in the link's callback or AppKit's window teardown while the drive held them: a finding. (A link left uninvalidated did not crash it at the desk, 2026-10-27 and 2026-10-28: no tick came after the close. On 2026-10-28 the drive's weak reference, still loading the link after the close, caught it, exit 1.)
        no_status: Severity::GateFailed,
    },
    ObjcDrive {
        example: "objc_confirm_drive",
        code3: Code3::Hung,
        no_status: Severity::CouldNotRun,
    },
];

/// Build the seven explicit driver targets in one invocation. They share their
/// dependency graph and profile; each remains a separate main-thread process.
#[must_use]
pub fn objc_drives_build_args() -> Vec<String> {
    let mut args: Vec<String> = [
        "--unverified",
        "build",
        MAIN_PROFILE[0],
        MAIN_PROFILE[1],
        "-q",
        "-p",
        "aterm-gui",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for drive in &OBJC_DRIVES {
        args.extend(["--example".to_owned(), drive.example.to_owned()]);
    }
    args
}

/// Extracts the last `NOT RUN — <reason>` clause an objc driver printed to its transcript.
#[must_use]
pub fn objc_drive_not_run_reason(transcript: &str) -> Option<&str> {
    transcript.lines().rev().find_map(|line| {
        let (_, why) = line.trim().split_once("NOT RUN — ")?;
        let why = why.trim();
        (!why.is_empty()).then_some(why)
    })
}

/// The line every objc driver prints once it has judged, before its verdict rows.
pub const OBJC_VERDICT_HEADER: &str = "=== VERDICT ===";

/// The first line of `transcript` that names a finding: one whose words, after an optional
/// `<driver name>…: ` prefix, begin `FINDING` or `FAIL` (`  FINDING: …`, `    FAIL …`,
/// `objc-confirm-drive: FINDING — …`, `objc-window-drive terminate child: FAIL — …`).
#[must_use]
pub fn objc_drive_finding<'t>(drive: &ObjcDrive, transcript: &'t str) -> Option<&'t str> {
    let prefix = drive.example.replace('_', "-");
    transcript.lines().map(str::trim).find(|line| {
        let words = match line.strip_prefix(prefix.as_str()) {
            Some(rest) => rest.split_once(": ").map_or(rest, |(_, after)| after),
            None => line,
        };
        ["FINDING", "FAIL"].iter().any(|mark| {
            words.strip_prefix(mark).is_some_and(|after| {
                after
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
            })
        })
    })
}

/// How the ladder reads one driver's exit and transcript.
///
/// The verdict is the driver's, not its exit status alone: a line naming a finding is a finding
/// whatever the status, and a status that reads as a pass counts only when the transcript holds
/// [`OBJC_VERDICT_HEADER`]. A process that ends before it judges (a queued `terminate:` that
/// exits `0`, as KI-1110-3 saw) has proven nothing it was meant to prove. Exit `2` quotes the
/// driver's `NOT RUN — <reason>` line.
#[must_use]
pub fn objc_drive_outcome(
    drive: &ObjcDrive,
    code: Option<i32>,
    transcript: &str,
) -> (Outcome, String) {
    let (outcome, what) = objc_drive_exit_outcome(drive, code, transcript);
    if outcome == Outcome::Fail(Severity::GateFailed) {
        return (outcome, what);
    }
    let name = drive.example;
    if let Some(line) = objc_drive_finding(drive, transcript) {
        let status = code.map_or_else(|| "no exit status".to_owned(), |c| format!("exit {c}"));
        return (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: a finding the exit status did not report ({status}): {line}"),
        );
    }
    if outcome == Outcome::Ok && !transcript.lines().any(|l| l.trim() == OBJC_VERDICT_HEADER) {
        let status = code.map_or_else(String::new, |c| format!(" {c}"));
        return (
            Outcome::Fail(Severity::GateFailed),
            format!(
                "{name}: exit{status} without its {OBJC_VERDICT_HEADER} line — the process ended \
                 before the driver judged, so whatever it had found went unreported"
            ),
        );
    }
    (outcome, what)
}

/// The exit status's reading alone, which [`objc_drive_outcome`] holds to the transcript.
fn objc_drive_exit_outcome(
    drive: &ObjcDrive,
    code: Option<i32>,
    transcript: &str,
) -> (Outcome, String) {
    let name = drive.example;
    let (outcome, what) = match (code, drive.code3) {
        (Some(0), _) => match objc_pass::description(name) {
            Some(pass) => (Outcome::Ok, pass.to_owned()),
            None => (
                Outcome::Fail(Severity::GateFailed),
                format!("{name}: invalid Objective-C success-description table"),
            ),
        },
        (Some(1), _) => (
            Outcome::Fail(Severity::GateFailed),
            "a finding — the transcript above names each check that failed (exit 1)".to_string(),
        ),
        (Some(2), _) => (
            Outcome::Fail(Severity::CouldNotRun),
            match objc_drive_not_run_reason(transcript) {
                Some(why) => format!("NOT RUN — {why} (exit 2, never a pass)"),
                None => "no event loop, no window, nothing installed to drive, or another app held the activation a check needs (the transcript says which), so its claim was not proven (exit 2, never a pass)".to_string(),
            },
        ),
        (Some(3), Code3::WeakerPassOk(label)) => (Outcome::Ok, label.to_string()),
        (Some(3), Code3::Hung) => (
            Outcome::Fail(Severity::CouldNotRun),
            "HUNG — the watchdog fired, so a modal tracking loop never returned and the drive proved nothing (exit 3, never a pass)".to_string(),
        ),
        (Some(3), Code3::AbortFinding) => (
            Outcome::Fail(Severity::GateFailed),
            "ABORTED — a row raised an NSException or panicked on an NSEvent AppKit can deliver (the v0.72.0 mouse-move crash class); the driver's signal trap turned the abort into exit 3 and the transcript names the row".to_string(),
        ),
        (Some(c), code3) => (
            Outcome::Fail(Severity::GateFailed),
            format!(
                "unexpected exit {c} (the driver answers only {})",
                if code3 == Code3::None { "0/1/2" } else { "0/1/2/3" }
            ),
        ),
        (None, code3) => (
            Outcome::Fail(drive.no_status),
            match (drive.no_status, code3) {
                // Only an `AbortFinding` driver traps signals.
                (Severity::GateFailed, Code3::AbortFinding) => "ABORTED — the driver died by a signal its trap could not convert, which is still the finding it exists to make".to_string(),
                (Severity::GateFailed, _) => "CRASHED — the driver died by a signal (it traps none): a crash in the code it drives, a finding".to_string(),
                (Severity::CouldNotRun, _) => "no exit status — killed by a signal".to_string(),
            },
        ),
    };
    (outcome, format!("{name}: {what}"))
}

/// `targo --unverified run -q -p aterm-libc-oracle --target-dir <lane>/gate-bin -- --root <R> --target-dir <lane>`, from `<R>`: the oracle built and run inside its own lane ([`Lane::LibcOracleTarget`], `<R>/libc-oracle/.ubd/target.noindex`), so it never queues on the main workspace's cargo lock.
/// # Errors
/// The current-directory error from making a relative path absolute, which the stage reports as could not run.
pub fn libc_oracle_cmd(ctx: &Ctx) -> std::io::Result<Cmd> {
    let root = std::path::absolute(&ctx.root)?;
    let lane = std::path::absolute(
        lane_dir(ctx, Lane::LibcOracleTarget).unwrap_or_else(|| ctx.target_dir.clone()),
    )?;
    let mut cmd = targo(
        ctx,
        vec![
            "--unverified".into(),
            "run".into(),
            "-q".into(),
            "-p".into(),
            "aterm-libc-oracle".into(),
            "--target-dir".into(),
            lane.join("gate-bin").display().to_string(),
            "--".into(),
            "--root".into(),
            root.display().to_string(),
            "--target-dir".into(),
            lane.display().to_string(),
        ],
    );
    cmd.cwd = Some(root);
    Ok(cmd)
}

/// How the ladder reads the oracle's exit: `0` pass, `1` a finding (the oracle's own verdict, which outranks a later stop, so even beside a full disk), `3` or no status decided nothing.
#[must_use]
pub fn libc_oracle_outcome(run: &exec::Run) -> Outcome {
    match run.code {
        Some(crate::exit::PASS) => Outcome::Ok,
        Some(crate::exit::FAILED) => Outcome::Fail(Severity::GateFailed),
        Some(crate::exit::COULD_NOT_RUN) | None => Outcome::Fail(Severity::CouldNotRun),
        Some(_) if run.environment_failure().is_some() => Outcome::Fail(Severity::CouldNotRun),
        Some(_) => Outcome::Fail(Severity::GateFailed),
    }
}

/// Why a libc oracle that decided nothing did not run, for its row: the machine's failure ([`exec::Run::environment_failure`]), else the oracle's own last `COULD NOT RUN: <reason>` line, so the verdict names the fix rather than leaving it in the raw log.
#[must_use]
pub fn libc_oracle_not_run_reason(run: &exec::Run) -> Option<String> {
    run.environment_failure().or_else(|| {
        run.output
            .lines()
            .rev()
            .find_map(|l| l.trim().strip_prefix("COULD NOT RUN: "))
            .map(str::trim)
            .filter(|why| !why.is_empty())
            .map(str::to_string)
    })
}

fn run_tests_at_width(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    threads: Option<u32>,
) -> bool {
    if let Err(why) = crate::test_env::prepare(ctx) {
        r.cannot_run(format!(
            "cannot prepare release test environment: {why}; no test was run"
        ));
        return false;
    }
    let rerun = crate::findings::Rerun::of(cmd, threads);
    let cmd = &cmd.clone().counting_waits();
    run_child(ctx, r, label, cmd, |r, run, label| {
        r.decide_test_child(run, label, &rerun);
    })
}

fn run_child(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    decide: impl FnOnce(&mut Report, &exec::Run, &str),
) -> bool {
    let out = run_row(ctx, &r.title, label, cmd);
    for event in crate::report::measurement_events(&out.output) {
        crate::event_log::emit(ctx.events.as_ref(), &event);
    }
    r.raw(out.output.as_str());
    decide(r, &out, label);
    out.ok
}

fn run_row(ctx: &Ctx, stage: &str, row: &str, cmd: &Cmd) -> exec::Run {
    let began = std::time::Instant::now();
    crate::event_log::emit(
        ctx.events.as_ref(),
        &crate::report::row_event(stage, row, None),
    );
    let run = exec::run(cmd, ctx.exec_env());
    crate::event_log::emit(
        ctx.events.as_ref(),
        &crate::report::row_event(stage, row, Some((began.elapsed(), run.usage))),
    );
    run
}

/// A native step whose child lives across several calls, such as the GUI smoke.
/// Its CPU is the increment in the enclosing stage's sampled process-group meter.
#[cfg(unix)]
pub(crate) fn native_row<T>(
    ctx: &Ctx,
    r: &mut Report,
    row: &str,
    run: impl FnOnce(&mut Report) -> T,
) -> T {
    let stage = r.title.clone();
    let began = std::time::Instant::now();
    let meter = crate::memory::current();
    let before = meter.as_ref().and_then(|m| m.observed_cpu());
    let event = crate::report::row_event(&stage, row, None);
    r.raw(crate::event_log::emit(ctx.events.as_ref(), &event));
    let result = run(r);
    let usage = meter
        .as_ref()
        .and_then(|m| m.observed_cpu())
        .and_then(|after| {
            after
                .nanoseconds
                .checked_sub(before.map_or(0, |b| b.nanoseconds))
                .map(|ns| exec::TreeUsage {
                    cpu: std::time::Duration::from_nanos(ns),
                    ..Default::default()
                })
        });
    let event = crate::report::row_event(&stage, row, Some((began.elapsed(), usage)));
    r.raw(crate::event_log::emit(ctx.events.as_ref(), &event));
    result
}

/// One pure compile call ([`run_compile`]): its run, its compiler stdout apart from its
/// diagnostics, and the compile line its report gives it.
struct Compile {
    run: exec::Run,
    stdout: Result<String, String>,
    line: String,
    events: Vec<String>,
}

/// The step a stage's compile lines name: its report's title up to its first ` (`
/// (`test compile`, `tippy lint`, `checkers compile`).
pub(crate) fn step_of(r: &Report) -> &str {
    r.title.split(" (").next().unwrap_or(&r.title).trim()
}

/// Run `cmd`, a call that compiles and runs nothing it built, with Cargo's messages on a stdout
/// of its own ([`crate::compile_account::with_message_format`], [`exec::run_machine`]), and
/// account for it ([`crate::compile_account::Account`]) as call `call` of `step`: its
/// `compile-finished` event goes to stderr at once, its line to its caller for its report. What
/// it used of the build directory it builds into is noted for the run's record of it
/// ([`crate::disk::note_call`]). Never a call that runs tests or programs: their stdout is theirs.
fn run_compile(ctx: &Ctx, step: &str, call: &str, cmd: &Cmd) -> Compile {
    let cmd = crate::compile_account::with_message_format(cmd.clone());
    let began = std::time::Instant::now();
    crate::event_log::emit(
        ctx.events.as_ref(),
        &crate::report::row_event(step, call, None),
    );
    let exec::MachineRun { run, stdout } = exec::run_machine(&cmd, ctx.exec_env(), Some(step));
    let ended = std::time::Instant::now();
    crate::disk::note_call(ctx, &cmd, stdout.as_deref().ok(), run.ok);
    crate::event_log::emit(
        ctx.events.as_ref(),
        &crate::report::row_event(
            step,
            call,
            Some((ended.saturating_duration_since(began), run.usage)),
        ),
    );
    // The stage's meter read each compiler of this call while it ran: date them, and how many
    // ran at once over the call's wall, dated on the meter's own clock
    // ([`crate::memory::Meter::unit_spans`], [`crate::memory::Meter::unix_ms`]).
    let meter = crate::memory::current();
    let spans = meter
        .as_ref()
        .map(|meter| meter.unit_spans(began, ended))
        .unwrap_or_default();
    let overlap = meter
        .as_ref()
        .and_then(|meter| Some((meter.unix_ms(began)?, meter.unix_ms(ended)?)))
        .and_then(|(from, to)| crate::compile_account::Overlap::of(&spans, from, to));
    let account = crate::compile_account::Account::of(
        step,
        call,
        stdout.as_deref().ok(),
        run.usage,
        ended.saturating_duration_since(began),
    )
    .overlapping(overlap);
    crate::event_log::emit(ctx.events.as_ref(), &account.event());
    let events = stdout
        .as_deref()
        .map(|output| crate::report::compile_unit_events(call, output))
        .unwrap_or_default()
        .into_iter()
        .chain(crate::compile_account::span_events(call, &spans))
        .map(|event| {
            let stamped = crate::event_log::emit(ctx.events.as_ref(), &event);
            stamped.trim_end_matches('\n').to_owned()
        })
        .collect();
    Compile {
        run,
        stdout,
        line: format!("  {}", account.line()),
        events,
    }
}

/// `run`'s output with what its compiler printed on stdout for a person (`human`, Cargo's
/// messages taken out) put back ahead of it, each stream under a heading of its own; unchanged
/// when there is none. Stream order is kept within each capture; the headings claim no
/// interleaving.
fn rejoin(run: &mut exec::Run, human: String) {
    if human.is_empty() {
        return;
    }
    let mut joined = format!("--- compiler stdout ---\n{human}");
    if !run.output.is_empty() {
        joined.push_str("\n--- compiler stderr / executor diagnostics ---\n");
        joined.push_str(&run.output);
    }
    run.output = joined;
}

/// Run a pure compile call ([`run_compile`]) and decide its row ([`Report::decide_child`]): its
/// output, its compile line, then its row.
fn compile_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    let Compile {
        mut run,
        stdout,
        line,
        events,
    } = run_compile(ctx, step_of(r), label, cmd);
    for event in events {
        r.raw(event);
    }
    if let Ok(stdout) = &stdout {
        rejoin(&mut run, crate::compile_account::human(stdout));
    }
    r.raw(run.output.as_str());
    r.raw(line);
    r.decide_child(&run, label);
    run.ok
}

/// Why a selection-reading stage did nothing on an empty change cone, and the fix.
pub const EMPTY_CONE: &str = "(the change cone holds no crate — fix: pass -p <crate> or --all)";

fn targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    ctx.compiler_command(Cmd::new(&ctx.tools.targo).args(args))
}

/// A `targo` invocation in the main target, sharing plain targo's build cache
/// ([`Ctx::main_build_dir`]) and workspace feature unification
/// ([`aterm_toolchain::build_cache::with_test_graph`]). Generated configs go before the verb.
/// If a caller supplies command-local config, all configs move after the verb together:
/// Trust 16387 reads command-local configs in place of global ones.
#[must_use]
pub fn main_targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    let mut args = aterm_toolchain::build_cache::with_test_graph(&args);
    // Cargo's completed units remain reusable without retaining a second copy of
    // each unit's compiler state in every native verification lane.
    let mut cmd = ctx
        .compiler_command(Cmd::new(&ctx.tools.targo))
        .env("CARGO_INCREMENTAL", "0");
    if let Some(value) = &ctx.main_build_dir {
        args.splice(0..0, ["--config".to_owned(), value.clone()]);
        if aterm_toolchain::build_cache::loses_config(
            &args,
            aterm_toolchain::build_cache::Tool::Targo,
        ) && let Some(directory) = aterm_toolchain::build_cache::directory_from_config(value)
        {
            cmd = cmd.env(aterm_toolchain::build_cache::BUILD_DIR_ENV, directory);
        }
    }
    cmd.args(aterm_toolchain::build_cache::configs_after_subcommand(
        &args,
    ))
}

/// `CARGO_BUILD_JOBS` for a side lane, `None` where cargo's default stays.
#[must_use]
pub const fn lane_build_jobs(lane: Lane) -> Option<u32> {
    match lane {
        Lane::CellsTarget => Some(4),
        _ => None,
    }
}

/// The job cap a side lane's child gets: [`lane_build_jobs`], never more than `ceiling`, the run's [`Ctx::build_jobs_ceiling`] (the caller's own `CARGO_BUILD_JOBS`), so a small machine's export holds on every lane.
#[must_use]
pub fn lane_jobs(lane: Lane, ceiling: Option<u32>) -> Option<u32> {
    let cap = lane_build_jobs(lane)?;
    Some(ceiling.map_or(cap, |c| cap.min(c)))
}

/// Put a cargo child in its lane: that lane's own `CARGO_TARGET_DIR` and job cap, through the command's environment.
#[must_use]
pub fn in_lane(ctx: &Ctx, lane: Lane, cmd: Cmd) -> Cmd {
    if matches!(lane, Lane::MainTarget | Lane::AfterCompile | Lane::Pure) {
        return cmd;
    }
    let Some(dir) = lane_dir(ctx, lane) else {
        return cmd;
    };
    let cmd = cmd.env("CARGO_TARGET_DIR", dir);
    match lane_jobs(lane, ctx.build_jobs_ceiling()) {
        Some(jobs) => cmd.env("CARGO_BUILD_JOBS", jobs.to_string()),
        None => cmd,
    }
}

/// A build of a binary the run drives, in the main target.
#[must_use]
pub fn main_build_cmd(ctx: &Ctx, args: Vec<String>) -> Cmd {
    main_targo(ctx, args).demoted()
}

/// A binary the main target holds: `<target>/debug/<name>`.
#[must_use]
pub fn main_bin(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    ctx.target_dir.join("debug").join(name)
}

/// An `[[example]]` the main target holds — cargo puts those under `<target>/debug/examples/`, not beside the binaries.
#[must_use]
pub fn main_example(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    main_bin(ctx, "examples").join(name)
}

fn with_trustdoc(ctx: &Ctx, cmd: Cmd) -> Cmd {
    cmd.env("RUSTDOC", ctx.tools.trustdoc.as_os_str())
}

/// Bind the caller's captured doc driver explicitly, including after a release command clears
/// its inherited environment. `EnvSnapshot` normalized `RUSTDOC` and `CARGO_BUILD_RUSTDOC` to
/// this one Cargo-facing value before the disposable test HOME was selected.
fn with_ambient_rustdoc(ctx: &Ctx, cmd: Cmd) -> Cmd {
    match &ctx.env.rustdoc_override {
        Some(rustdoc) => cmd.env("RUSTDOC", rustdoc),
        None => cmd,
    }
}

/// Which doc driver a doc-running stage gets.
#[derive(Debug, PartialEq, Eq)]
enum DocDriver {
    Stage2,
    Ambient,
    BarePath,
    Absent,
}

fn doc_driver(ctx: &Ctx) -> DocDriver {
    doc_driver_from(
        ctx.tools.have_trustdoc(),
        ctx.env.rustdoc_override.is_some(),
        crate::have_on_path("trustdoc", &ctx.path_env),
    )
}

fn scope_compiles_doctests(ctx: &Ctx) -> bool {
    match &ctx.scope {
        Scope::Packages(ps) => ps
            .iter()
            .any(|c| crate::changed::crate_dir_has_lib(&ctx.root, c)),
        s => s.has_lib_target(),
    }
}

const fn doc_driver_from(
    stage2_has_trustdoc: bool,
    ambient_override: bool,
    bare_on_path: bool,
) -> DocDriver {
    if stage2_has_trustdoc {
        DocDriver::Stage2
    } else if ambient_override {
        DocDriver::Ambient
    } else if bare_on_path {
        DocDriver::BarePath
    } else {
        DocDriver::Absent
    }
}

fn test_doc_binding(ctx: &Ctx) -> (&'static str, bool) {
    match doc_driver(ctx) {
        DocDriver::Stage2 => (" (trustdoc)", true),
        DocDriver::Ambient => (" (caller's RUSTDOC)", false),
        DocDriver::BarePath | DocDriver::Absent => ("", false),
    }
}

/// The test compile, or one call of it when it is shaped ([`crate::shape`]): a cut over its own
/// crates ([`piece_compile_args`]), the closing call today's command over the run's selection.
/// Behind a call that did not pass, a call is COULD NOT RUN and starts no cargo. The whole call,
/// or a shaped compile's closing one, that passes closes the run's note of the main lane's build
/// directory ([`crate::disk::close`]): from then the run's record of it may be kept.
fn test_compile(ctx: &Ctx, spec: &StageSpec, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.cannot_run(ctx.tools.missing_targo_label());
        return;
    }
    let cut = spec
        .part
        .as_ref()
        .filter(|p| p.shaped() && !p.closing())
        .map(|p| &p.piece);
    let label = format!(
        "targo test {} --no-run{}",
        cut.map_or_else(|| ctx.scope.label(), crate::shape::Piece::label),
        if crate::plan::folds_measuring(ctx) {
            " (and the measuring benches)"
        } else {
            ""
        }
    );
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    if ctx
        .test_compile_failed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        not_run_behind(
            r,
            &label,
            "an earlier call of this test compile failed, and a call behind it would build what \
             failed again",
        );
        return;
    }
    if let Some(part) = spec
        .part
        .as_ref()
        .filter(|p| p.closing() && !p.refused.is_empty())
    {
        keep_refusals(ctx, part);
    }
    if let Err(why) = crate::test_env::prepare(ctx) {
        r.cannot_run(format!(
            "cannot prepare release test environment: {why}; no test was compiled"
        ));
        ctx.test_compile_failed
            .store(true, std::sync::atomic::Ordering::Release);
        return;
    }
    let (suffix, bind) = test_doc_binding(ctx);
    let compile = match cut {
        Some(piece) => test_compile_cmd(ctx, bind, piece_compile_args(piece)),
        None => {
            let [compile, _] = test_cmds(ctx, bind);
            compile
        }
    };
    let began = std::time::SystemTime::now();
    let (compiled, _, artifacts) = compile_tests(ctx, r, &format!("{label}{suffix}"), &compile);
    if !compiled {
        ctx.test_compile_failed
            .store(true, std::sync::atomic::Ordering::Release);
        return;
    }
    // The whole call, or the closing one of a shaped compile, built every test the run's
    // selection has: with what its other compile calls report, the run's note of the main
    // lane's build directory names all it uses there.
    if cut.is_none() {
        crate::disk::close(ctx, &crate::disk::Dir::Main);
    }
    if let Some(part) = spec.part.as_ref().filter(|p| p.shaped()) {
        shape_after(ctx, part, &artifacts, began);
    }
}

/// The closing call `part` keeps again each refusal its plan read ([`crate::shape::Part::refused`],
/// [`crate::memory::Book::refuse`]), so none it keeps reading becomes its record's oldest row;
/// one it could not keep goes on its time line.
fn keep_refusals(ctx: &Ctx, part: &crate::shape::Part) {
    let Some(book) = ctx.memory.as_ref() else {
        return;
    };
    let whole = &part.piece;
    let checkout = ctx.root.display().to_string();
    let failed: Vec<String> = part
        .refused
        .iter()
        .filter_map(|cut| {
            book.refuse(&crate::shape::refusal_key(&checkout, whole, cut))
                .err()
                .map(|e| {
                    format!(
                        "shape: the refusal of `{}` was not kept again: {e}",
                        cut.label()
                    )
                })
        })
        .collect();
    if !failed.is_empty() {
        said_on(ctx, part.index, failed.join("; "));
    }
}

/// Add `said` to what call `index`'s time line says of its shaping ([`crate::shape::Run::said`]).
fn said_on(ctx: &Ctx, index: usize, said: String) {
    ctx.shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .said
        .entry(index)
        .and_modify(|s| {
            s.push_str("; ");
            s.push_str(&said);
        })
        .or_insert(said);
}

/// What a test compile's child did: whether it passed, the test executables it built, and every
/// artifact it reported ([`crate::shape::artifacts`]).
type Compiled = (
    bool,
    Result<Vec<crate::testrun::TestExe>, String>,
    Vec<crate::shape::Artifact>,
);

fn compile_tests(ctx: &Ctx, r: &mut Report, label: &str, compile: &Cmd) -> Compiled {
    let identity_before = crate::test_index::identities(&ctx.root, &ctx.tools.targo).ok();
    let Compile {
        mut run,
        stdout,
        line,
        events,
    } = run_compile(ctx, step_of(r), label, compile);
    for event in events {
        r.raw(event);
    }
    let mut stdout = match stdout {
        Ok(stdout) => stdout,
        Err(why) => {
            r.raw(run.output.as_str());
            r.raw(line);
            r.decide_child(&run, label);
            r.cannot_run(&why);
            return (false, Err(why), Vec::new());
        }
    };

    let artifacts = crate::shape::artifacts(&stdout);
    let refusal = crate::compile_width::refusal(&run, &stdout);
    if run.ok {
        let indexed = crate::test_index::identities(&ctx.root, &ctx.tools.targo)
            .and_then(|identity| {
                if Some(&identity) != identity_before.as_ref() {
                    return Err(std::io::Error::other(
                        "candidate changed during test compile",
                    ));
                }
                crate::test_index::Index::from_messages(identity.0, identity.1, &stdout)
            })
            .and_then(|index| index.write(&ctx.state_dir().join("test-executables.json")));
        if let Err(error) = indexed {
            r.raw(format!("test executable index unavailable: {error}"));
        }
    }
    let exes = crate::testrun::take_test_executables(&mut stdout);
    rejoin(&mut run, stdout);
    r.raw(run.output.as_str());
    r.raw(line);
    if let Some(why) = refusal {
        r.cannot_run(format!("{label} — {why}"));
    } else {
        r.decide_child(&run, label);
    }
    (run.ok, exes, artifacts)
}

/// After call `part` of a shaped test compile, which began at `began`, passed with `artifacts`: a
/// cut keeps the files it built, and when it began, for the calls after it; every later call
/// checks the earlier cuts' files came back fresh ([`crate::shape::fresh_check`], the sources it
/// rebuilt from read under the checkout: [`crate::shape::changed_since`]). The exact closing call
/// keeps a refusal row for each cut that did not ([`crate::shape::refusal_key`]), so the key uses
/// the complete selection and no later run cuts it; what each call found goes on its time line.
fn shape_after(
    ctx: &Ctx,
    part: &crate::shape::Part,
    artifacts: &[crate::shape::Artifact],
    began: std::time::SystemTime,
) {
    let mut run = ctx
        .shape_run
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (refused, said) =
        crate::shape::fresh_check(&run.built, artifacts, part.closing(), &|since| {
            crate::shape::changed_since(&ctx.root, artifacts, since)
        });
    let checkout = ctx.root.display().to_string();
    let mut said = vec![said]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if part.closing() {
        let mut refused_pieces = std::mem::take(&mut run.pending_refusals);
        refused_pieces.extend(refused.iter().map(|(cut, _)| cut.clone()));
        refused_pieces.sort_by(|a, b| a.crates.cmp(&b.crates).then(a.lib.cmp(&b.lib)));
        refused_pieces.dedup();
        for cut in &refused_pieces {
            let key = crate::shape::refusal_key(&checkout, &part.piece, cut);
            if let Some(Err(e)) = ctx.memory.as_ref().map(|b| b.refuse(&key)) {
                said.push(format!("shape: the refusal was not kept: {e}"));
            }
            run.built.retain(|(c, _, _)| c != cut);
        }
    } else {
        for (cut, _) in &refused {
            if !run.pending_refusals.contains(cut) {
                run.pending_refusals.push(cut.clone());
            }
            run.built.retain(|(c, _, _)| c != cut);
        }
    }
    if !part.closing() {
        let files = artifacts
            .iter()
            .filter(|a| !a.fresh)
            .flat_map(|a| a.filenames.iter().cloned())
            .collect();
        run.built.push((part.piece.clone(), files, began));
    }
    drop(run);
    if !said.is_empty() {
        said_on(ctx, part.index, said.join("; "));
    }
}

fn test_compile_cmd(ctx: &Ctx, bind: bool, args: Vec<String>) -> Cmd {
    let cmd = main_targo(ctx, args);
    let cmd = if bind {
        with_trustdoc(ctx, cmd)
    } else if matches!(doc_driver(ctx), DocDriver::Ambient) {
        with_ambient_rustdoc(ctx, cmd)
    } else {
        cmd
    };
    crate::test_env::command(ctx, cmd.demoted())
}

fn test_cmds(ctx: &Ctx, bind: bool) -> [Cmd; 2] {
    let (k, v) = TRAIL_LAWS_FULL;
    let mut compile = test_compile_args(&ctx.scope);
    if crate::plan::folds_measuring(ctx) {
        compile.extend(measuring_bench_args());
    }
    [
        test_compile_cmd(ctx, bind, compile),
        crate::test_env::command(
            ctx,
            with_checkers(
                main_targo(ctx, test_run_args(&ctx.scope)),
                ctx.checkers_in_tests,
            ),
        )
        .env(k, v)
        .counting_waits(),
    ]
}

/// What every suite a failed build kept from running adds to its could-not-run row: the build's own failure is the finding, and what stood behind it decided nothing.
pub const NOT_RUN_BEHIND: &str = "; nothing behind it was decided";

/// The COULD NOT RUN row for `what`, which the failure above it kept from running ([`NOT_RUN_BEHIND`]).
pub fn not_run_behind(r: &mut Report, what: &str, because: &str) {
    r.cannot_run(format!("{what} — {because}{NOT_RUN_BEHIND}"));
}

fn build_to_drive(
    ctx: &Ctx,
    r: &mut Report,
    args: Vec<String>,
    build_label: String,
    drive: &str,
) -> bool {
    let Compile {
        run: mut build,
        stdout,
        line,
        events,
    } = run_compile(ctx, step_of(r), &build_label, &main_build_cmd(ctx, args));
    for event in events {
        r.raw(event);
    }
    if build.ok {
        r.raw(line);
        return true;
    }
    if let Ok(stdout) = &stdout {
        rejoin(&mut build, crate::compile_account::human(stdout));
    }
    r.raw(build.output.as_str());
    r.raw(line);
    r.fail_child(&build, build_label);
    if build.environment_failure().is_none() {
        not_run_behind(
            r,
            drive,
            "the build above failed, so there is nothing fresh to drive",
        );
    }
    false
}

/// Run the rainbow trail laws' full grid.
pub const TRAIL_LAWS_FULL: (&str, &str) = ("TRAIL_LAWS_FULL", "1");

fn test(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test (no targo)");
        return;
    }
    let label = format!("targo test {}", ctx.scope.label());
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    let (suffix, bind) = test_doc_binding(ctx);
    let [_, run] = test_cmds(ctx, bind);
    let run_label = format!("{label} --tests{suffix}");
    let behind = "the compile above failed, and cargo runs no test after a compile error";
    if ctx
        .test_compile_failed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        not_run_behind(r, &run_label, behind);
        return;
    }
    // The recording Cargo pass checks freshness and supplies its own target metadata.
    crate::testrun::run_tests(ctx, r, &run_label, &run, crate::checks::Check::Test);
}

/// Why a `measure` stage handed no bundle ran nothing, and the fix.
pub const NO_APP: &str = "(no release bundle to judge — fix: pass --app <path to aterm.app>; \
                          `targo --unverified ship dev-app --no-install` assembles this checkout's \
                          as dist/aterm (dev).app)";

const RELEASE_PAINT_BIN: &str = "ATERM_PAINT_BIN";
const RELEASE_SPIN_BIN: &str = "ATERM_SPIN_BIN";

/// `--no-windows` for the measuring matrices: each leaves out its rows that open a window.
const MEASURE_NO_WINDOWS: &str = "ATERM_MEASURE_NO_WINDOWS";

/// The named skip of the measuring rows `--no-windows` leaves out.
const MEASURE_WINDOW_ROWS_SKIP: &str = "spin's window rows (--no-windows: each opens a window on \
                                        the desktop — fix: run without --no-windows when the \
                                        desktop is free)";

/// The main lane's targo over `args` ([`main_targo`]), and its label.
fn measuring_targo(ctx: &Ctx, args: Vec<String>) -> (String, Cmd) {
    let label = format!("targo {}", args[1..].join(" "));
    (label, main_targo(ctx, args))
}

fn measuring_cmd(ctx: &Ctx, artifact: &crate::MeasureArtifact, args: Vec<String>) -> (String, Cmd) {
    let (label, cmd) = measuring_targo(ctx, args);
    let bin = artifact.bin("aterm");
    let mut cmd = cmd
        .env(RELEASE_PAINT_BIN, bin.as_os_str())
        .env(RELEASE_SPIN_BIN, bin.as_os_str());
    if ctx.no_windows {
        cmd = cmd.env(MEASURE_NO_WINDOWS, "1");
    }
    (label, cmd)
}

/// The measuring rows' compile: the matrices read the bundle only when they run
/// (`ATERM_PAINT_BIN`, `ATERM_SPIN_BIN`), and no unit reads either name, so it needs none.
fn measure_prebuild(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("measuring rows' compile (no targo)");
        return;
    }
    let (label, cmd) = measuring_targo(ctx, measure_prebuild_args());
    compile_labeled(ctx, r, &label, &cmd.demoted());
}

fn measuring_tests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("measuring matrices (no targo)");
        return;
    }
    let Some(artifact) = &ctx.measure_artifact else {
        r.skip(format!("measuring matrices {NO_APP}"));
        return;
    };
    let (label, cmd) = measuring_cmd(ctx, artifact, measuring_args());
    if crate::plan::folds_measuring(ctx)
        && ctx
            .test_compile_failed
            .load(std::sync::atomic::Ordering::Acquire)
    {
        // Its benches were the test compile's to build: compiling them here would hold the
        // quiet machine for a build no memory claim covers.
        not_run_behind(r, &label, "the test compile that builds its benches failed");
        return;
    }
    let subject = Subject::of(ctx, artifact);
    if ctx.no_windows {
        r.skip(MEASURE_WINDOW_ROWS_SKIP);
    }
    // Not `counting_waits`, though cargo runs its matrices as benches: each takes the machine's
    // quiet claim (aterm-conformance's `benches/support/quiet.rs`), at once where this run's own
    // claim covers it. Any wait for it is behind other runs, not on a scratch lock of its own, so
    // the ceiling leaves it out, as it does a lab's.
    run_child(ctx, r, &label, &cmd, |r, run, label| {
        decide_measuring_child(r, run, label, &subject);
    });
}

/// The bundle `measure` asked, as its rows' decisions need it.
pub(crate) struct Subject {
    /// [`crate::MeasureArtifact::describe`].
    name: String,
    /// This checkout's HEAD, in full, when git names it.
    head: Option<String>,
    /// HEAD built the bundle, so every row must have an answer. Another build, such as an
    /// installed release, may predate a row: what it has no answer to is named, not judged.
    own: bool,
}

impl Subject {
    pub(crate) fn of(ctx: &Ctx, artifact: &crate::MeasureArtifact) -> Self {
        let head = crate::head_commit(&ctx.root, &ctx.path_env);
        Self {
            name: artifact.describe(),
            own: head.as_deref().is_some_and(|h| artifact.built_from(h)),
            head,
        }
    }

    /// Record what the subject had no answer to (`rows`, each named with its reason), under the
    /// check's `label`: a finding when HEAD built it, else one skip line naming each row.
    pub(crate) fn unanswered(
        &self,
        r: &mut Report,
        run: Option<&exec::Run>,
        label: &str,
        rows: &[String],
    ) {
        let rows = rows.join("; ");
        if self.own {
            let label = format!(
                "{label} — {}, built from this checkout's HEAD, has no answer to {rows}",
                self.name
            );
            match run {
                Some(run) => r.record_child(run, Outcome::Fail(Severity::GateFailed), label),
                None => r.fail(label),
            }
        } else {
            let head = self
                .head
                .as_deref()
                .map_or("unknown", |h| &h[..h.len().min(12)]);
            r.skip(format!(
                "{label} — not judged on {}, which this checkout's HEAD ({head}) did not build: \
                 it has no answer to {rows}",
                self.name
            ));
        }
    }
}

#[derive(Clone, Copy)]
struct MeasuringCounts {
    planned: usize,
    passed: usize,
    failed: usize,
    refused: usize,
    unanswered: usize,
    undecided: usize,
}

impl MeasuringCounts {
    fn parse(fields: &str) -> Option<Self> {
        let mut fields = fields.split_whitespace();
        let mut count = |key: &str| {
            fields
                .next()?
                .strip_prefix(key)
                .filter(|v| v.bytes().all(|b| b.is_ascii_digit()))?
                .parse()
                .ok()
        };
        let counts = Self {
            planned: count("planned=")?,
            passed: count("passed=")?,
            failed: count("failed=")?,
            refused: count("refused=")?,
            unanswered: count("unanswered=")?,
            undecided: count("undecided=")?,
        };
        fields.next().is_none().then_some(counts)
    }

    fn complete(self) -> bool {
        self.planned > 0
            && self
                .passed
                .checked_add(self.failed)
                .and_then(|n| n.checked_add(self.refused))
                .and_then(|n| n.checked_add(self.unanswered))
                .and_then(|n| n.checked_add(self.undecided))
                == Some(self.planned)
    }
}

/// Each `measure row: <target>::<row> UNANSWERED` in `output`, with the first line of its reason.
fn unanswered_rows(output: &str) -> Vec<String> {
    rows_marked(output, " UNANSWERED")
}

/// Each `measure row: <target>::<row> UNDECIDED` in `output` (a row that measured, whose takes
/// are no evidence either way on this machine's load), with the first line of its reason.
fn undecided_rows(output: &str) -> Vec<String> {
    rows_marked(output, " UNDECIDED")
}

/// What a passing measure stage states beside its pass: each undecided row, and each target
/// that ran without a qualified quiet admission (`measure <target>: unqualified — <why>`). Both
/// are stated, never a finding (a busy machine never blocks a release: the row is named, not
/// counted red).
fn stated_note(output: &str) -> String {
    let rows = undecided_rows(output);
    let mut note = String::new();
    if !rows.is_empty() {
        note.push_str(&format!(
            "; undecided, stated and not a finding: {}",
            rows.join("; ")
        ));
    }
    let unqualified = unqualified_targets(output);
    if !unqualified.is_empty() {
        note.push_str(&format!(
            "; ran without a qualified quiet admission (stated, not a finding): {}",
            unqualified.join("; ")
        ));
    }
    note
}

/// Each `measure <target>: unqualified — <why>` line, as `<target> — <why>`.
fn unqualified_targets(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            MEASURING_TARGETS.iter().find_map(|target| {
                line.strip_prefix(&format!("measure {target}: unqualified — "))
                    .map(|why| format!("{target} — {why}"))
            })
        })
        .collect()
}

fn rows_marked(output: &str, status: &str) -> Vec<String> {
    let mut lines = output.lines();
    let mut rows = Vec::new();
    while let Some(line) = lines.next() {
        if let Some(row) = line
            .strip_prefix("measure row: ")
            .and_then(|l| l.strip_suffix(status))
        {
            rows.push(format!("{row} ({})", lines.next().unwrap_or("").trim()));
        }
    }
    rows
}

/// The runner's own exit on a matrix with an unanswered row (`benches/support/mod.rs`
/// `Counts::exit_code`), as cargo names a bench that exited with it.
const RUNNER_UNANSWERED_EXIT: &str = "(exit status: 3)";

/// Each bench a `--no-fail-fast` `targo bench` child names as failed, with how its process ended:
/// cargo's `` error: bench failed, to rerun pass `… --bench <target>` `` and, for any exit but
/// libtest's 101, the `process didn't exit successfully: … (<how>)` under its `Caused by:`.
fn failed_benches(output: &str) -> Vec<(String, Option<String>)> {
    let mut failed: Vec<(String, Option<String>)> = Vec::new();
    for line in output.lines() {
        if let Some(target) = line
            .strip_prefix("error: bench failed, to rerun pass `")
            .and_then(|spec| spec.split('`').next())
            .and_then(|spec| spec.rsplit_once("--bench "))
        {
            failed.push((target.1.trim().to_string(), None));
        } else if let Some(how) = line
            .trim_start()
            .strip_prefix("process didn't exit successfully: ")
            .and_then(|rest| rest.rfind(" (").map(|at| &rest[at + 1..]))
            && let Some((_, end @ None)) = failed.last_mut()
        {
            *end = Some(how.trim_end().to_string());
        }
    }
    failed
}

/// Whether a failed child's only failures are the runner's own exit on the matrices with an
/// unanswered row (`answered_short`): cargo exited 101 for its benches, and it names exactly those
/// as failed, each with [`RUNNER_UNANSWERED_EXIT`]. A bench that died of a signal, panicked, was
/// killed, or exited any other way after its summary is not that, and neither is a run whose
/// output does not account for each such matrix.
fn only_unanswered_exits(run: &exec::Run, answered_short: &[&str]) -> bool {
    let mut failed = failed_benches(&strip_ansi_if_any(&run.output));
    failed.sort();
    let mut named: Vec<&str> = failed.iter().map(|(t, _)| t.as_str()).collect();
    named.dedup();
    let mut short = answered_short.to_vec();
    short.sort_unstable();
    run.code == Some(101)
        && run.spawn_error.is_none()
        && !failed.is_empty()
        && named.len() == failed.len()
        && named == short
        && failed
            .iter()
            .all(|(_, how)| how.as_deref() == Some(RUNNER_UNANSWERED_EXIT))
}

fn strip_ansi_if_any(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\x1b') {
        crate::libtest::strip_ansi(s).into()
    } else {
        s.into()
    }
}

fn decide_measuring_child(r: &mut Report, run: &exec::Run, label: &str, subject: &Subject) {
    let unanswered = unanswered_rows(&run.output);
    if subject.own && !unanswered.is_empty() {
        subject.unanswered(r, Some(run), label, &unanswered);
        return;
    }
    let mut incomplete = Vec::new();
    let mut refused = Vec::new();
    let mut answered_short = Vec::new();
    let mut any_failed = false;
    let mut any_undecided = false;
    let mut all_reported = true;
    for target in MEASURING_TARGETS {
        let prefix = format!("measure result: {target} ");
        let mut summaries = run.output.lines().filter_map(|l| l.strip_prefix(&prefix));
        let counts = summaries.next().and_then(MeasuringCounts::parse);
        let counts = counts.filter(|_| summaries.next().is_none());
        match counts {
            Some(counts) => {
                any_failed |= counts.failed != 0;
                any_undecided |= counts.undecided != 0;
                if !counts.complete() {
                    incomplete.push(target);
                }
                if counts.refused != 0 {
                    refused.push(target);
                }
                if counts.unanswered != 0 {
                    answered_short.push(target);
                }
            }
            None => {
                all_reported = false;
                incomplete.push(target);
            }
        }
    }
    if any_failed {
        r.record_child(run, Outcome::Fail(Severity::GateFailed), label);
    } else if !incomplete.is_empty()
        && (run.ok
            || all_reported
            || run.output.contains("measure: COULD NOT RUN —")
            // Each incomplete target refused its own admission or entry: one that crashed
            // without saying so keeps the child's verdict below.
            || incomplete.iter().all(|target| {
                run.output
                    .contains(&format!("measure {target}: COULD NOT RUN — "))
            }))
    {
        r.cannot_run(format!(
            "{label} — {}: no complete nonempty measurement matrix",
            incomplete.join(", ")
        ));
    } else if incomplete.is_empty() && !refused.is_empty() {
        r.cannot_run(format!(
            "{label} — {}: the instrument could not run every row",
            refused.join(", ")
        ));
    } else if incomplete.is_empty()
        && !unanswered.is_empty()
        && (run.ok || only_unanswered_exits(run, &answered_short))
    {
        r.pass(format!(
            "{label} — every row the subject answered{}",
            stated_note(&run.output)
        ));
    } else if incomplete.is_empty()
        && run.ok
        && (any_undecided || !unqualified_targets(&run.output).is_empty())
    {
        // A row whose takes are no evidence either way on this machine's load, and a matrix that
        // ran without a qualified quiet admission, are stated beside the pass, never a finding
        // and never a refusal (a busy machine never blocks a release: the row is named, not
        // counted red).
        // The runner exits 0 there, so a crash or a lost timing protection after the summaries
        // still fails below, and a row that could not measure at all stays `refused` above.
        r.pass(format!(
            "{label} — no row failed{}",
            stated_note(&run.output)
        ));
    } else {
        r.decide_child(run, label);
    }
    if !unanswered.is_empty() {
        subject.unanswered(r, None, label, &unanswered);
    }
}

fn doctests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test --doc (no targo)");
        return;
    }
    let label = format!("targo test --doc {}", ctx.scope.label());
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    if !scope_compiles_doctests(ctx) {
        r.skip(format!(
            "targo test --doc (no package in scope has a library target: {})",
            ctx.scope.label()
        ));
        return;
    }
    let split = doctest_split(ctx);
    if split.as_ref().is_some_and(|s| s.run.is_empty()) {
        r.pass(format!(
            "targo test --doc (no crate in {} has a fenced Rust block in its docs)",
            ctx.scope.label()
        ));
        return;
    }
    let skip = split.map(|s| s.skip).unwrap_or_default();
    let driver = doc_driver(ctx);
    let label = match driver {
        DocDriver::Stage2 => format!("{label} (trustdoc)"),
        DocDriver::Ambient => format!("{label} (caller's RUSTDOC)"),
        DocDriver::BarePath => label,
        DocDriver::Absent => {
            r.cannot_run(ctx.tools.missing_trustdoc_label());
            return;
        }
    };
    let cmd = match doctest_cmd(ctx, &driver, &skip) {
        Ok(cmd) => cmd,
        Err(why) => {
            r.cannot_run(format!(
                "{label} — {why}; no doctest binary was run unmonitored"
            ));
            return;
        }
    };
    let halt_marker = ctx
        .halt
        .armed()
        .then(|| crate::docdriver::halt_marker(&ctx.scratch));
    if let Some(path) = &halt_marker
        && let Err(why) = crate::docdriver::prepare_halt_marker(path)
    {
        r.cannot_run(format!(
            "{label} — cannot prepare its first-red admission: {why}; no doctest binary was run"
        ));
        return;
    }
    let threads = if ctx.pool.is_some() {
        Some(1)
    } else {
        ctx.test_thread_override()
    };
    run_tests_at_width(ctx, r, &label, &cmd, threads);
    if let Some(path) = halt_marker
        && let Err(error) = crate::docdriver::remove_halt_marker(&path)
    {
        r.raw(format!(
            "aterm-verify: could not remove doctest first-red protocol {}: {error}",
            path.display()
        ));
    }
}

fn doctest_cmd(ctx: &Ctx, driver: &DocDriver, skip: &[String]) -> Result<Cmd, String> {
    let cmd = crate::test_env::command(
        ctx,
        with_checkers(
            main_targo(ctx, doctest_args(&ctx.scope)),
            ctx.checkers_in_tests,
        ),
    );
    let stand_in = ctx
        .test_recorder()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "cannot resolve verify's absolute doctest runtool".to_owned())?;
    let real = cargo_doc_driver(ctx, driver).ok_or_else(|| {
        "cannot resolve Cargo's exact doc driver to interpose the doctest runtool".to_owned()
    })?;
    let mut cmd = cmd
        .remove_env(crate::docdriver::DRIVER_ENV)
        .remove_env(crate::docdriver::SKIP_ENV)
        .remove_env(crate::docdriver::RUNTOOL_ENV)
        .remove_env(crate::docdriver::RUNTOOL_SCRATCH_ENV)
        .remove_env(crate::docdriver::RUNTOOL_SAMPLES_ENV)
        .remove_env(crate::docdriver::RUNTOOL_HALT_ENV)
        .env("RUSTDOC", &stand_in)
        .env(crate::docdriver::DRIVER_ENV, real)
        .env(crate::docdriver::RUNTOOL_ENV, &stand_in)
        .env(crate::docdriver::RUNTOOL_SCRATCH_ENV, &ctx.scratch);
    if ctx.pool.is_some() {
        // Each Cargo/rustdoc job already owns one token. Runtime doctests spend that one job.
        cmd = cmd.env("RUST_TEST_THREADS", "1");
    }
    if ctx.halt.armed() {
        cmd = cmd.env(
            crate::docdriver::RUNTOOL_HALT_ENV,
            crate::docdriver::halt_marker(&ctx.scratch),
        );
    }
    if let Some(samples) = &ctx.hang_samples {
        cmd = cmd.env(crate::docdriver::RUNTOOL_SAMPLES_ENV, samples);
    }
    if !skip.is_empty() {
        cmd = cmd.env(
            crate::docdriver::SKIP_ENV,
            crate::docdriver::skip_list(skip),
        );
    }
    Ok(cmd)
}

fn cargo_doc_driver(ctx: &Ctx, driver: &DocDriver) -> Option<std::path::PathBuf> {
    use aterm_toolchain::build_cache::{Machine, configs_name, read_config};
    let on_path = |name: &std::ffi::OsStr| crate::find_on_path(name, &ctx.path_env);
    let beside_targo_or_on_path = || {
        let name = crate::toolchain::tool_file("trustdoc");
        std::fs::canonicalize(&ctx.tools.targo)
            .ok()
            .and_then(|targo| Some(targo.parent()?.join(&name)))
            .filter(|f| {
                std::fs::symlink_metadata(f).is_ok_and(|m| m.file_type().is_file())
                    && is_executable_file(f)
            })
            .or_else(|| on_path(name.as_ref()))
    };
    let config_names_one = || {
        let lookup = |k: &str| {
            ctx.env
                .cargo_layout
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.clone())
        };
        let machine = Machine {
            home: Some(&ctx.env.home)
                .filter(|h| !h.as_os_str().is_empty())
                .map(std::path::PathBuf::as_path),
            root: None,
            cwd: Some(&ctx.root),
            env: &lookup,
            read: &read_config,
            read_manifest: &aterm_toolchain::build_cache::read_manifest,
            writable: &|_| None,
        };
        configs_name(&machine, names_rustdoc)
    };
    let file = match driver {
        // The ordinary command binds this path as `RUSTDOC`; the stand-in replaces that binding,
        // so this is the exact driver it must exec rather than another `trustdoc` on PATH.
        DocDriver::Stage2 => Some(ctx.tools.trustdoc.clone()),
        DocDriver::Ambient => match ctx.env.rustdoc_override.as_deref() {
            Some(value) if !value.is_empty() => {
                let spelled = value.to_str()?;
                if spelled.contains(['/', '\\']) {
                    Some(ctx.root.join(spelled))
                } else {
                    on_path(value)
                }
            }
            _ if config_names_one() => None,
            _ => beside_targo_or_on_path(),
        },
        DocDriver::BarePath if config_names_one() => None,
        DocDriver::BarePath => beside_targo_or_on_path(),
        DocDriver::Absent => None,
    };
    file.filter(|f| f.is_absolute())
}

fn names_rustdoc(line: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    line.match_indices("rustdoc").any(|(at, key)| {
        let rest = &line[at + key.len()..];
        !line[..at].ends_with(word)
            && !rest.starts_with(word)
            && rest
                .trim_start_matches([' ', '\t', '"', '\''])
                .starts_with('=')
    })
}

/// A lint pass's `args` led by the workspace's feature unification
/// ([`aterm_toolchain::build_cache::TEST_GRAPH_FLAG`] and
/// [`aterm_toolchain::build_cache::TEST_GRAPH_CONFIG`]; `targo-tippy` hands them to the `check`
/// it runs, after that verb, with no `--config` of its own before it, so neither replaces the
/// other), so `lint -p X` after `lint --all` checks no unit again, as every main-lane build
/// resolves ([`main_targo`]). Not the shipped build's pass ([`tippy_release_args`]): `ship`
/// builds that package alone, with its own features.
#[must_use]
pub fn lint_unified(args: Vec<String>) -> Vec<String> {
    std::iter::once(aterm_toolchain::build_cache::TEST_GRAPH_FLAG)
        .chain(aterm_toolchain::build_cache::TEST_GRAPH_CONFIG)
        .map(String::from)
        .chain(args)
        .collect()
}

fn tippy_cmd(ctx: &Ctx, bin: &std::path::Path, args: Vec<String>) -> Cmd {
    let dir = bin.parent().unwrap_or(&ctx.tools.stage2_dir).to_path_buf();
    let mut path = std::ffi::OsString::from(dir.as_os_str());
    if !ctx.path_env.is_empty() {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(&ctx.path_env);
    }
    let cmd = if ctx.tools.borrowed(crate::toolchain::TIPPY) == Some(bin) {
        Cmd::new(bin).env("TRUST_STAGE2_BIN", &dir)
    } else {
        Cmd::new(bin)
    };
    let mut cmd = cmd
        .args(args)
        .env("CARGO_INCREMENTAL", "0")
        .env("PATH", path)
        .env(
            "CARGO_TARGET_DIR",
            lane_dir(ctx, Lane::TippyTarget).unwrap_or_else(|| ctx.target_dir.clone()),
        )
        .env("TRUST_NO_MIGRATE_WARN", "1")
        .demoted();
    if ctx.release {
        // Cargo's loader paths describe the parent verifier, not the sealed compiler.
        // Remove them from the actual command as well as its content evidence. A loader
        // setting supplied by Cargo config is still detected as an unbounded input.
        for (name, _) in std::env::vars_os() {
            if crate::lint_content::loader_environment(&name.to_string_lossy()) {
                cmd = cmd.remove_env(name);
            }
        }
    }
    cmd
}

fn tippy(ctx: &Ctx, r: &mut Report) {
    let Some(bin) = ctx.tools.tippy.clone() else {
        r.skip(ctx.tools.missing_tippy_label());
        return;
    };
    let borrowed = crate::fmt::borrowed_suffix(ctx, crate::toolchain::TIPPY);
    if ctx.release {
        tippy_release_content(ctx, r, &bin, &borrowed);
    } else {
        let scope = ctx.scope.clone();
        // Every pass of the lint over the run's selection passed: what they reported of the
        // lint lane's build directory is all the run uses there, so its record may be kept
        // ([`crate::disk::close`]). The release's content passes lint package by package from
        // what earlier runs carried, never the whole selection in one run, and close nothing.
        if tippy_scope(ctx, r, &bin, &scope, &borrowed)
            && !scope.selects_nothing()
            && let Some(lane) = lane_dir(ctx, Lane::TippyTarget)
        {
            crate::disk::close(ctx, &crate::disk::Dir::Side(lane));
        }
    }
}

fn tippy_scope(
    ctx: &Ctx,
    r: &mut Report,
    bin: &std::path::Path,
    scope: &Scope,
    borrowed: &str,
) -> bool {
    if scope.selects_nothing() {
        r.skip(format!("tippy -D warnings{borrowed} {EMPTY_CONE}"));
        return true;
    }
    let mut passed = true;
    for pass in lint_invocations(scope, &gated_lint_features(&ctx.root)) {
        if !compile_labeled(
            ctx,
            r,
            &format!("tippy {} -D warnings{borrowed}", pass.label),
            &tippy_cmd(ctx, bin, pass.args),
        ) {
            passed = false;
            crate::red_now(ctx, crate::checks::Check::Lint, r);
            if ctx.halted().is_some() {
                return false;
            }
        }
    }
    passed
}

fn exact_package_tippy_commands(
    ctx: &Ctx,
    bin: &std::path::Path,
    spec: &str,
) -> Result<Vec<Cmd>, String> {
    lint_invocations(&Scope::crate_only(spec), &gated_lint_features(&ctx.root))
        .into_iter()
        .map(|TippyPass { mut args, .. }| {
            args.insert(0, "--unverified".into());
            exact_lint_command(
                ctx,
                crate::compile_account::with_message_format(tippy_cmd(ctx, bin, args)),
            )
        })
        .collect()
}

fn exact_lint_command(ctx: &Ctx, mut command: Cmd) -> Result<Cmd, String> {
    command.demoted = false;
    command.waits_count = true;
    let mut command = exec::materialize_exact_command(command, ctx.exec_env())?;
    command.envs.retain(|(name, _)| {
        !name
            .to_str()
            .is_some_and(|name| name.starts_with("TRUST_TARGO_"))
    });
    exec::finalize_exact_command(command, ctx.exec_env())
}

fn unbounded_lint_evidence(reason: impl Into<String>) -> crate::lint_content::Evidence {
    crate::lint_content::Evidence {
        content_key: None,
        reason: Some(reason.into()),
    }
}

#[derive(Clone, Debug, PartialEq)]
struct LintPackageFinished {
    spec: String,
    outcome: &'static str,
    evidence: crate::lint_content::Evidence,
    execution_s: f64,
    carried: bool,
}

struct LintPackageCandidate {
    package: crate::lint_content::Package,
    expected: crate::lint_content::Evidence,
    execution_s: f64,
    carried: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LintPackageOutcome {
    Ok,
    Fail,
    NotRun,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LintReadBoundary {
    PreDispatch,
    Final,
}

impl LintPackageOutcome {
    const fn words(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "FAIL",
            Self::NotRun => "NOT RUN",
        }
    }
}

/// The effects around [`schedule_lint_packages`]. Production supplies the real Tippy children,
/// fresh evidence reads and report events; tests supply deterministic effects to prove the
/// scheduler starts only misses and never turns an uncertain read into an OK.
trait LintScheduleOps {
    /// Run all unbounded packages in one ordinary Tippy batch. The seconds are per package.
    fn run_unbounded(
        &mut self,
        packages: &[crate::lint_content::Package],
    ) -> (LintPackageOutcome, f64);
    /// Reconstruct the exact command and freshly hash the package's complete declared inputs.
    fn prepare(
        &mut self,
        package: &crate::lint_content::Package,
        boundary: LintReadBoundary,
    ) -> Result<(Vec<Cmd>, crate::lint_content::Evidence), String>;
    /// Run exactly the commands returned by the immediately preceding [`Self::prepare`].
    fn run_bounded(
        &mut self,
        package: &crate::lint_content::Package,
        commands: &[Cmd],
    ) -> (LintPackageOutcome, f64);
    fn halted(&self) -> bool;
    fn cannot_run(&mut self, why: String);
    fn all_carried(&mut self, count: usize);
    fn finish(&mut self, row: &LintPackageFinished);
}

/// Execute one content-keyed release-lint partition. No OK is published until every execution
/// mode has ended and every successful or carried package has passed the final stability read.
fn schedule_lint_packages(
    selection: crate::lint_content::Selection,
    ops: &mut impl LintScheduleOps,
) {
    let asked_run = selection.run.len();
    let asked_carried = selection.carried.len();
    let (unbounded, bounded): (Vec<_>, Vec<_>) = selection
        .run
        .into_iter()
        .partition(|package| !package.evidence.bounded());
    let mut finished = Vec::new();
    let mut candidates = Vec::new();

    if !unbounded.is_empty() {
        let (outcome, execution_s) = ops.run_unbounded(&unbounded);
        finished.extend(unbounded.into_iter().map(|package| LintPackageFinished {
            spec: package.spec,
            outcome: outcome.words(),
            evidence: package.evidence,
            execution_s,
            carried: false,
        }));
    }

    let mut stopped = ops.halted();
    for package in bounded {
        if stopped {
            finished.push(LintPackageFinished {
                spec: package.spec,
                outcome: "NOT RUN",
                evidence: unbounded_lint_evidence(
                    "an earlier lint package stopped this release lint",
                ),
                execution_s: 0.0,
                carried: false,
            });
            continue;
        }
        let (commands, immediate) = match ops.prepare(&package, LintReadBoundary::PreDispatch) {
            Ok(prepared) => prepared,
            Err(why) => {
                ops.cannot_run(format!(
                    "lint package {} changed before dispatch: {why}",
                    package.spec
                ));
                finished.push(LintPackageFinished {
                    spec: package.spec,
                    outcome: "NOT RUN",
                    evidence: unbounded_lint_evidence(format!(
                        "the exact lint command changed before dispatch: {why}"
                    )),
                    execution_s: 0.0,
                    carried: false,
                });
                stopped = true;
                continue;
            }
        };
        if immediate != package.evidence {
            ops.cannot_run(format!(
                "lint package {} inputs changed between planning and dispatch",
                package.spec
            ));
            finished.push(LintPackageFinished {
                spec: package.spec,
                outcome: "NOT RUN",
                evidence: unbounded_lint_evidence(
                    "lint inputs changed between planning and immediate pre-dispatch hashing",
                ),
                execution_s: 0.0,
                carried: false,
            });
            stopped = true;
            continue;
        }

        let (outcome, execution_s) = ops.run_bounded(&package, &commands);
        if outcome == LintPackageOutcome::Ok {
            candidates.push(LintPackageCandidate {
                package,
                expected: immediate,
                execution_s,
                carried: false,
            });
        } else {
            finished.push(LintPackageFinished {
                spec: package.spec,
                outcome: outcome.words(),
                evidence: package.evidence,
                execution_s,
                carried: false,
            });
            stopped = true;
        }
    }

    candidates.extend(
        selection
            .carried
            .into_iter()
            .map(|(package, prior)| LintPackageCandidate {
                expected: package.evidence.clone(),
                package,
                execution_s: prior.execution_s,
                carried: true,
            }),
    );

    // This is deliberately after every execution. A miss may have changed a carried package's
    // inputs; publishing that package before this read would make the next hop trust stale facts.
    let mut drifted = false;
    for candidate in candidates {
        let current = ops
            .prepare(&candidate.package, LintReadBoundary::Final)
            .map(|(_, evidence)| evidence);
        match current {
            Ok(evidence) if evidence == candidate.expected => {
                finished.push(LintPackageFinished {
                    spec: candidate.package.spec,
                    outcome: "ok",
                    evidence,
                    execution_s: candidate.execution_s,
                    carried: candidate.carried,
                });
            }
            Ok(_) | Err(_) => {
                drifted = true;
                finished.push(LintPackageFinished {
                    spec: candidate.package.spec,
                    outcome: "NOT RUN",
                    evidence: unbounded_lint_evidence(
                        "lint inputs changed before the run-wide read-only stability check",
                    ),
                    execution_s: candidate.execution_s,
                    carried: false,
                });
            }
        }
    }
    if drifted {
        ops.cannot_run(
            "lint package inputs changed during the run; no drifting proof was carried".into(),
        );
    }
    if asked_run == 0 && !drifted {
        ops.all_carried(asked_carried);
    }
    finished.sort_by(|left, right| left.spec.cmp(&right.spec));
    for row in &finished {
        ops.finish(row);
    }
}

/// Bind a boundary read to the exact package identity whose commands the scheduler dispatches.
/// Keeping this adapter outside [`RealLintScheduleOps::prepare`] gives its production argument
/// handoff a direct negative control without replacing the real snapshot implementation.
fn read_lint_package_evidence(
    package: &crate::lint_content::Package,
    commands: &[Cmd],
    read: impl FnOnce(&str, &str, &[Cmd]) -> crate::lint_content::Evidence,
) -> crate::lint_content::Evidence {
    read(&package.spec, &package.dir, commands)
}

struct RealLintScheduleOps<'a> {
    ctx: &'a Ctx,
    report: &'a mut Report,
    bin: &'a std::path::Path,
    borrowed: &'a str,
    pre_snapshot: Option<Result<crate::lint_content::BoundarySnapshot<'a>, String>>,
    final_snapshot: Option<Result<crate::lint_content::BoundarySnapshot<'a>, String>>,
}

impl LintScheduleOps for RealLintScheduleOps<'_> {
    fn run_unbounded(
        &mut self,
        packages: &[crate::lint_content::Package],
    ) -> (LintPackageOutcome, f64) {
        let specs: Vec<String> = packages
            .iter()
            .map(|package| package.spec.clone())
            .collect();
        let scope = Scope::packages(&specs);
        let began = std::time::Instant::now();
        let passed = tippy_scope(self.ctx, self.report, self.bin, &scope, self.borrowed);
        let execution_s = began.elapsed().as_secs_f64() / packages.len() as f64;
        let outcome = if passed {
            LintPackageOutcome::Ok
        } else if self.ctx.halted().is_some() {
            LintPackageOutcome::Fail
        } else {
            LintPackageOutcome::NotRun
        };
        (outcome, execution_s)
    }

    fn prepare(
        &mut self,
        package: &crate::lint_content::Package,
        boundary: LintReadBoundary,
    ) -> Result<(Vec<Cmd>, crate::lint_content::Evidence), String> {
        let commands = exact_package_tippy_commands(self.ctx, self.bin, &package.spec)?;
        let ctx = self.ctx;
        let bin = self.bin;
        let slot = match boundary {
            LintReadBoundary::PreDispatch => &mut self.pre_snapshot,
            LintReadBoundary::Final => &mut self.final_snapshot,
        };
        let snapshot = slot
            .get_or_insert_with(|| crate::lint_content::BoundarySnapshot::new(ctx, bin))
            .as_ref()
            .map_err(Clone::clone)?;
        let evidence = read_lint_package_evidence(package, &commands, |spec, dir, commands| {
            snapshot.evidence(spec, dir, commands)
        });
        Ok((commands, evidence))
    }

    fn run_bounded(
        &mut self,
        package: &crate::lint_content::Package,
        commands: &[Cmd],
    ) -> (LintPackageOutcome, f64) {
        let began = std::time::Instant::now();
        let mut passed = true;
        for (index, command) in commands.iter().enumerate() {
            let label = format!(
                "tippy -p {} content pass {}/{} -D warnings{}",
                package.spec,
                index + 1,
                commands.len(),
                self.borrowed
            );
            if !compile_labeled(self.ctx, self.report, &label, command) {
                crate::red_now(self.ctx, crate::checks::Check::Lint, self.report);
                passed = false;
                break;
            }
        }
        let execution_s = began.elapsed().as_secs_f64();
        let outcome = if passed {
            LintPackageOutcome::Ok
        } else if self.ctx.halted().is_some() {
            LintPackageOutcome::Fail
        } else {
            LintPackageOutcome::NotRun
        };
        (outcome, execution_s)
    }

    fn halted(&self) -> bool {
        self.ctx.halted().is_some()
    }

    fn cannot_run(&mut self, why: String) {
        self.report.cannot_run(why);
    }

    fn all_carried(&mut self, count: usize) {
        self.report.pass(format!(
            "lint packages: {count} carried by complete content key, 0 ran"
        ));
    }

    fn finish(&mut self, row: &LintPackageFinished) {
        self.report
            .raw(self.ctx.record_event(&crate::lint_content::event(
                &row.spec,
                row.outcome,
                &row.evidence,
                row.execution_s,
                row.carried,
            )));
    }
}

fn tippy_release_content(ctx: &Ctx, r: &mut Report, bin: &std::path::Path, borrowed: &str) {
    let plan =
        match crate::lint_content::plan(ctx, |spec| exact_package_tippy_commands(ctx, bin, spec)) {
            Ok(plan) => plan,
            Err(why) => {
                r.raw(format!(
                    "lint package carry is unavailable ({why}); the complete workspace lint runs"
                ));
                let scope = ctx.scope.clone();
                let _ = tippy_scope(ctx, r, bin, &scope, borrowed);
                return;
            }
        };
    let empty = crate::lint_content::Passed::new();
    let selection = plan.select(ctx.passed_lint_packages.as_ref().unwrap_or(&empty));
    let mut ops = RealLintScheduleOps {
        ctx,
        report: r,
        bin,
        borrowed,
        pre_snapshot: None,
        final_snapshot: None,
    };
    schedule_lint_packages(selection, &mut ops);
}

const CHECKOUT_MARKER: &str = "crates/aterm-gui/Cargo.toml";

fn in_checkout(ctx: &Ctx, r: &mut Report, what: &str) -> bool {
    let here = ctx.root.join(CHECKOUT_MARKER).is_file();
    if !here {
        r.cannot_run(format!(
            "{what} — {} is not an aterm checkout (no {CHECKOUT_MARKER})",
            ctx.root.display()
        ));
    }
    here
}

/// Every walk of [`aterm_census::CENSUSES`], each once, after Census's shared checkout
/// prerequisite has passed and holding `capture-privacy` to the baseline of the engine's commit
/// ([`census_baseline`]). Each guard rule and census reuses the result the checkout's memo
/// under the state directory kept for it while every question it asked keeps its answer
/// ([`aterm_census::Memo`]); the transcript says what it reused.
fn walks(ctx: &Ctx, r: &mut Report, check: &str) {
    if ctx.input_paths.is_some() && ctx.carry_context.is_some() {
        preflight_censuses(ctx, r, check);
        return;
    }
    let selected: Vec<&aterm_census::Census> = aterm_census::CENSUSES.iter().collect();
    let root = ctx.root.clone();
    let var = engine_vars(ctx);
    let memo = aterm_census::Memo::for_root(&ctx.state_dir(), &ctx.root);
    let (outcomes, report) = match bounded_source(ctx, move || {
        census_walk(&root, &var, &selected, Some(&memo))
    }) {
        Ok(o) => o,
        Err(why) => {
            r.cannot_run(format!("{check} — the walk {why}"));
            return;
        }
    };
    for outcome in outcomes {
        if outcome.ok {
            r.raw(outcome.log.trim_end().to_string());
            r.pass(outcome.verdict);
        } else {
            r.fail(outcome.verdict);
            r.raw(outcome.log.trim_end().to_string());
        }
    }
    if let Some(report) = report {
        r.raw(report.line());
    }
}

/// The walk owns its CPU token until it actually ends, including after its reporting ceiling.
/// Nested census jobs execute on that worker with their ordinary input captures. Child
/// commands retain the bounded walk's executor, which stops and reaps them at its ceiling.
fn bounded_source<T: Send + 'static>(
    ctx: &Ctx,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let pool = ctx.pool.clone();
    let ceiling = ctx.child_ceiling;
    let began = std::time::Instant::now();
    exec::bounded(ceiling, move || {
        let Some(pool) = pool else {
            return Ok(work());
        };
        let executor = aterm_uds::output::current();
        crate::compile_width::run(
            Some(&pool),
            Some(1),
            None,
            &|| crate::stopping() || ceiling.is_some_and(|limit| began.elapsed() >= limit),
            || {
                aterm_census::on_one_worker(move || match executor {
                    Some(executor) => aterm_uds::output::with_executor(executor, work),
                    None => work(),
                })
            },
        )
        .map_err(|why| format!("CPU admission could not run: {why}"))?
        .map_err(|why| format!("the source worker could not start: {why}"))
    })
    .map_err(|why| why.sentence())?
}

#[cfg(all(test, unix))]
#[path = "stages_source_worker_tests.rs"]
mod source_worker_tests;

fn preflight_censuses(ctx: &Ctx, r: &mut Report, check: &str) {
    let root = ctx.root.clone();
    let baseline = census_baseline(&root, &engine_vars(ctx));
    let prior: std::collections::BTreeMap<_, _> = aterm_census::CENSUSES
        .iter()
        .filter_map(|census| {
            crate::input_content::prior(ctx, check, census.name)
                .map(|proof| (census.name, proof.clone()))
        })
        .collect();
    let memo = aterm_census::Memo::for_root(&ctx.state_dir(), &ctx.root);
    let pooled = ctx.pool.is_some();
    let result = bounded_source(ctx, move || {
        let run_one = |census: &aterm_census::Census| {
            let cached = prior.get(census.name).cloned();
            if let Some(proof) = cached {
                return (census.name, None, None, Some(proof), None);
            }
            // Guards include parallel and publication readers; their aggregate stays
            // unbounded, and this tree's census memo keeps each rule's record
            // instead. The five lexical graph censuses use the typed tree reader.
            if census.name == "guards" {
                let (mut outcomes, report) =
                    aterm_census::run_censuses(&root, &[census], &baseline, Some(&memo));
                return (census.name, outcomes.pop(), None, None, report);
            }
            let (outcome, inputs) = aterm_census::inputs::capture(&root, || {
                aterm_census::run_census(&root, census, &baseline)
            });
            (census.name, Some(outcome), inputs, None, None)
        };
        if pooled {
            aterm_census::CENSUSES
                .iter()
                .map(run_one)
                .collect::<Vec<_>>()
        } else {
            std::thread::scope(|scope| {
                let jobs: Vec<_> = aterm_census::CENSUSES
                    .iter()
                    .map(|census| scope.spawn(|| run_one(census)))
                    .collect();
                jobs.into_iter()
                    .map(|job| job.join().expect("census panic is contained"))
                    .collect::<Vec<_>>()
            })
        }
    });
    let outcomes = match result {
        Ok(outcomes) => outcomes,
        Err(why) => {
            r.cannot_run(format!("{check} — {why}"));
            return;
        }
    };
    for (name, outcome, inputs, carried, memo) in outcomes {
        if let Some(memo) = memo {
            r.raw(memo.line());
        }
        if let Some(proof) = carried {
            r.pass(format!("{name}: reused complete package input snapshots"));
            r.raw(crate::input_content::event(ctx, &proof, true));
        } else if let Some(outcome) = outcome {
            r.raw(outcome.log.trim_end().to_owned());
            if outcome.ok {
                r.pass(outcome.verdict);
                if let Some(proof) = inputs
                    .and_then(|inputs| crate::input_content::Proof::new(ctx, check, name, inputs))
                {
                    r.raw(crate::input_content::event(ctx, &proof, false));
                }
            } else {
                r.fail(outcome.verdict);
            }
        }
    }
}

/// The engine checkout and explicit pin variables of `ctx`'s environment ([`crate::EnvSnapshot::engine_var`]),
/// owned so a walk on another thread reads them.
fn engine_vars(ctx: &Ctx) -> impl Fn(&str) -> Option<std::ffi::OsString> + Send + 'static {
    let held = [
        "PUBLICATION_ENGINE",
        "HOME",
        crate::engine::GIT_DIR_ENV,
        crate::engine::COMMIT_ENV,
    ]
    .map(|name| (name, ctx.env.engine_var(name)));
    move |name| {
        held.iter()
            .find(|(held, _)| *held == name)
            .and_then(|(_, value)| value.clone())
    }
}

/// The `selected` censuses over the checkout at `root`, `capture-privacy` held to the baseline
/// of the engine's commit `var` names ([`census_baseline`]), reusing what `memo` kept.
fn census_walk(
    root: &std::path::Path,
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    selected: &[&aterm_census::Census],
    memo: Option<&aterm_census::Memo>,
) -> (
    Vec<aterm_census::CensusOutcome>,
    Option<aterm_census::MemoReport>,
) {
    aterm_census::run_censuses(root, selected, &census_baseline(root, var), memo)
}

/// The forbidden-content baseline of the engine's commit as `var` names the engine for the
/// checkout at `root` ([`crate::engine::named`]): a named checkout's HEAD, else its origin's main
/// imported now ([`crate::engine::native`]); never a working tree.
fn census_baseline(
    root: &std::path::Path,
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> aterm_census::EngineBaseline {
    let run = crate::engine::bounded;
    let commit = crate::engine::named(root, var)
        .and_then(|named| crate::engine::Commit::read(&named, &run, &crate::engine::native))
        .map_err(|e| e.why)?;
    let path = aterm_census::export_scan::rows::BASELINE;
    let text = commit.text(path, &run).map_err(|e| e.why)?;
    Ok((format!("{path} on {}", commit.shown()), text))
}

/// The export check's row.
pub const EXPORT_LABEL: &str = "ship check --export";

/// The `SIGTERM` grace `ship check --export` gets at its ceiling.
pub const EXPORT_TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// `ship check --export` from the root, its release tools built in the main build directory after
/// the test compile has populated it ([`Lane::MainTarget`]), ended at its ceiling with
/// [`EXPORT_TERM_GRACE`].
#[must_use]
pub fn export_cmd(ctx: &Ctx) -> Cmd {
    main_targo(ctx, ship_args(["check", "--export"])).term_grace(EXPORT_TERM_GRACE)
}

fn fixes(transcript: &str) -> Option<String> {
    let fixes: Vec<&str> = transcript
        .lines()
        .filter_map(|l| l.trim().strip_prefix("fix: "))
        .collect();
    (!fixes.is_empty()).then(|| fixes.join("; "))
}

/// How `ship check --export` answers, by its exit code: `0` the engine's guards pass HEAD's export; `1` a guard or the engine refuses the tree, and `2` HEAD names no release, both findings; `3` nothing refused but it could not run here (no network, engine, gitleaks or rustup, or HEAD not on origin/main), a named skip; `130` or no status, interrupted.
#[must_use]
pub fn export_outcome(code: Option<i32>, transcript: &str) -> (Outcome, String) {
    match code {
        Some(crate::exit::PASS) => (Outcome::Ok, EXPORT_LABEL.to_string()),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{EXPORT_LABEL}: the publication engine refuses HEAD's export"),
        ),
        Some(2) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{EXPORT_LABEL}: HEAD names no release"),
        ),
        Some(3) => (
            Outcome::Skip,
            format!(
                "{EXPORT_LABEL}: not judged here — {}",
                fixes(transcript).map_or_else(
                    || "its NOT RUN lines above say why".to_string(),
                    |f| format!("fix: {f}")
                )
            ),
        ),
        Some(130) | None => (
            Outcome::Fail(Severity::CouldNotRun),
            format!("{EXPORT_LABEL}: interrupted"),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{EXPORT_LABEL}: exit {c} (the release tools did not run)"),
        ),
    }
}

fn export(ctx: &Ctx, r: &mut Report) {
    export_with(ctx, r, |cmd| exec::run(cmd, ctx.exec_env()));
}

fn export_mainline(ctx: &Ctx) -> Result<(), aterm_census::export::mainline::Refusal> {
    aterm_census::export::mainline::require(
        ctx.head_at_start.as_deref().unwrap_or("HEAD"),
        |args| {
            let mut git = crate::git_provider::command();
            git.current_dir(&ctx.root)
                .env("PATH", &ctx.path_env)
                .arg("--no-optional-locks")
                .stdin(aterm_uds::spawnfd::Stdio::null());
            // Match the producer's object-only Git read, independently of inherited Git
            // redirects, replace refs or a partial clone's lazy network fetch.
            for (key, _) in std::env::vars_os() {
                if key.as_encoded_bytes().starts_with(b"GIT_") {
                    git.env_remove(key);
                }
            }
            git.args(args)
                .env("GIT_NO_REPLACE_OBJECTS", "1")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_NO_LAZY_FETCH", "1")
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0");
            aterm_uds::deadline::output_within(&mut git, aterm_census::export::mainline::READ_WAIT)
                .map(|out| {
                    (
                        out.status.code(),
                        String::from_utf8_lossy(&out.stderr).into_owned(),
                    )
                })
                .map_err(|error| error.to_string())
        },
    )
}

/// Resolve the cheap local prerequisite before starting the potentially uncached producer.
fn export_with(ctx: &Ctx, r: &mut Report, producer: impl FnOnce(&Cmd) -> exec::Run) {
    if !ctx.tools.have_targo() {
        r.skip("export guards (no targo)");
        return;
    }
    let revision = ctx.head_at_start.as_deref().unwrap_or("HEAD");
    if let Err(why) = export_mainline(ctx) {
        r.raw(why.why(revision));
        r.skip(format!(
            "{EXPORT_LABEL}: not judged here — fix: {}",
            why.remedy()
        ));
        return;
    }
    let out = producer(&export_cmd(ctx));
    r.raw(out.output.as_str());
    let (outcome, label) = export_outcome(out.code, &out.output);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

fn libc_oracle(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("libc ABI oracle (no targo)");
        return;
    }
    let cmd = match libc_oracle_cmd(ctx) {
        Ok(cmd) => cmd,
        Err(error) => {
            r.cannot_run(format!(
                "cannot resolve the libc-oracle target dir: {error}"
            ));
            return;
        }
    };
    let label = "aterm-libc-oracle (this host's native ABI cell; the other cells are decided \
                 where they are native)";
    let out = exec::run(&cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    let outcome = libc_oracle_outcome(&out);
    let why = if outcome == Outcome::Fail(Severity::CouldNotRun) {
        libc_oracle_not_run_reason(&out)
    } else {
        None
    };
    r.record_oracle(&out, outcome, label, why);
}

fn census(ctx: &Ctx, r: &mut Report) {
    if !in_checkout(ctx, r, "census") {
        return;
    }
    if ctx.fix {
        crate::census_fix::run(ctx, r);
    }
    match crate::changelog_guard::judge(ctx) {
        Ok(Some(found)) if found.is_empty() => r.pass("changelog structure"),
        Ok(Some(found)) => {
            r.fail("changelog structure");
            r.raw(found.join("\n"));
            return;
        }
        Ok(None) => {}
        Err(why) => r.cannot_run(format!("changelog structure — {why}")),
    }
    walks(ctx, r, "census");
    match crate::harness_debt::judge(
        &ctx.root,
        ctx.head_at_start.as_deref().unwrap_or("HEAD"),
        &ctx.path_env,
    ) {
        Ok(crate::harness_debt::Judgement::Clear { version, baseline }) => {
            r.pass(format!(
                "harness landing debt since public v{version}: none ({})",
                &baseline[..12]
            ));
        }
        Ok(crate::harness_debt::Judgement::Owed {
            version,
            baseline,
            debts,
        }) => {
            r.fail(format!(
                "harness landing debt since public v{version}: {} harness version{} owed",
                debts.len(),
                if debts.len() == 1 { " is" } else { "s are" }
            ));
            r.raw(format!("public baseline: {baseline}"));
            for debt in debts {
                r.raw(debt.line());
            }
            r.raw("fix: targo --unverified ship harness-bump".to_string());
        }
        Err(why) => r.cannot_run(format!("harness landing debt — {why}")),
    }
}

/// `targo --unverified run --profile test -q -p aterm-models --bin spec-check`, in the main target once the test compile is done ([`MAIN_PROFILE`]; the bin links every domain's models through `aterm_models::link_all`); under `release` with `-- --full`, which adds the minutes-class `ay` row.
#[must_use]
pub fn spec_check_cmd(ctx: &Ctx) -> Cmd {
    let mut args: Vec<String> = [
        "--unverified",
        "run",
        MAIN_PROFILE[0],
        MAIN_PROFILE[1],
        "-q",
        "-p",
        "aterm-models",
        "--bin",
        "spec-check",
    ]
    .map(String::from)
    .into();
    if ctx.release {
        args.extend(["--".to_string(), "--full".to_string()]);
    }
    let cmd = main_targo(ctx, args).remove_env(aterm_capacity::IMPLICIT_POOL_ENV);
    match &ctx.pool {
        Some(pool) => cmd.env(aterm_capacity::IMPLICIT_POOL_ENV, pool.handed()),
        None => cmd,
    }
}

/// Why a `spec-check` that decided nothing did not run: the machine's failure, else its own `could not run` line (a missing `ty` or `ay` names its install).
#[must_use]
pub fn spec_check_not_run_reason(run: &exec::Run) -> Option<String> {
    run.environment_failure().or_else(|| {
        run.output.lines().rev().find_map(|l| {
            let l = l.trim();
            l.strip_prefix("spec-check: ")
                .filter(|rest| rest.starts_with("could not run"))
                .or_else(|| l.split_once("  could not run: ").map(|(_, why)| why))
                .map(str::to_string)
        })
    })
}

fn spec_check(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("spec checkers (no targo)");
        return;
    }
    let label = "spec-check (every registered model under ty, the ay certificates)";
    let out = exec::run(&spec_check_cmd(ctx), ctx.exec_env());
    r.raw(out.output.as_str());
    let outcome = libc_oracle_outcome(&out);
    let why = (outcome == Outcome::Fail(Severity::CouldNotRun))
        .then(|| spec_check_not_run_reason(&out))
        .flatten();
    match why {
        Some(why) => r.cannot_run(format!("{label} — {why}")),
        None => r.record_child(&out, outcome, label),
    }
}

/// Runtime selection shared by serial Cargo runs and recorded test executables.
pub const CHECKERS_ENV: &str = "ATERM_SPEC_CHECKERS";

/// Assert the checker policy explicitly; inherited values never choose a verifier's verdict.
pub(crate) fn with_checkers(cmd: Cmd, required: bool) -> Cmd {
    cmd.env(CHECKERS_ENV, if required { "1" } else { "0" })
}

fn spec_checkers_cmd(ctx: &Ctx) -> Cmd {
    with_checkers(main_targo(ctx, test_run_args(&ctx.scope)), true)
}

fn spec_checkers(ctx: &Ctx, r: &mut Report) {
    if ctx.scope.selects_nothing() {
        r.skip(format!("checker-enabled tests {EMPTY_CONE}"));
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("checkers (no targo)");
        return;
    }
    let label = format!(
        "ATERM_SPEC_CHECKERS=1 targo test {} --tests",
        ctx.scope.label()
    );
    if ctx
        .test_compile_failed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        not_run_behind(
            r,
            &label,
            "test compilation failed; its runtime stage does not retry compilation",
        );
        return;
    }
    crate::testrun::run_tests(
        ctx,
        r,
        &label,
        &spec_checkers_cmd(ctx),
        crate::checks::Check::Checkers,
    );
}

/// Did a passing test run not run [`crate::checks::GOLDEN`]'s test? A release's run reads this over
/// every recorded binary's output ([`crate::testrun::run_tests`]): no frame
/// was then held to its twin. In a passing run a test libtest names and did not ignore passed, and
/// its `ok` need not share the line: this test writes to stderr past libtest's capture, so one
/// libtest thread ends its name's line with that, and the `ok` comes on a line of its own. Nor need
/// the name open the line: the log is one file both streams write, and another test's unfinished
/// stderr line can come before it (`aterm-gpu: test …`).
#[must_use]
pub fn compared_no_frame(run: &exec::Run) -> bool {
    if !run.ok {
        return false;
    }
    let (_, test) = crate::checks::GOLDEN;
    let log = crate::libtest::strip_ansi(&run.output);
    !log.lines().any(|line| {
        line.match_indices("test ").any(|(at, word)| {
            line[at + word.len()..]
                .split_once(" ... ")
                .is_some_and(|(name, verdict)| {
                    name.ends_with(test) && !verdict.starts_with("ignored")
                })
        })
    })
}

/// How the ladder reads one harness exit code (`0` pass / `1` fail / `2` not run, declared in aterm-gui's `control_redraw_conformance.rs`).
#[must_use]
pub fn redraw_outcome(code: Option<i32>) -> (Outcome, String) {
    match code {
        Some(0) => (
            Outcome::Ok,
            "aterm-redraw-conformance: the verb matrix passed against a real proxy, and a select reached the event loop".to_string(),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            "aterm-redraw-conformance: a check failed, or a redraw the host accepted never arrived".to_string(),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            "aterm-redraw-conformance: no event loop is constructible here, so nothing was proven about redraws (exit 2, never a pass)".to_string(),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("aterm-redraw-conformance: unexpected exit {c} (the harness answers only 0/1/2)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "aterm-redraw-conformance: no exit status — killed by a signal".to_string(),
        ),
    }
}

fn redraw_conformance(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("redraw conformance (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        redraw_conformance_build_args(),
        format!("targo build --bin {REDRAW_CONFORMANCE_BIN}"),
        "redraw conformance (the verb matrix against a real proxy)",
    ) {
        return;
    }
    let bin = main_bin(ctx, REDRAW_CONFORMANCE_BIN);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "redraw conformance: just-built harness missing ({})",
            bin.display()
        ));
        return;
    }
    let paths = [(REDRAW_CONFORMANCE_BIN, bin.clone())];
    let Some(runtime) = appkit_group_runtime(ctx, r, crate::appkit_content::REDRAW_GROUP, &paths)
    else {
        return;
    };
    let mut observed =
        appkit_group_proof(ctx, crate::appkit_content::REDRAW_GROUP, &paths, &runtime);
    if carry_appkit_group(
        ctx,
        r,
        crate::appkit_content::REDRAW_GROUP,
        &paths,
        &runtime,
        observed.as_ref(),
    ) {
        return;
    }
    // The second hash used by a refused carry is the nearest observation to execution.
    if ctx
        .passed_appkit_groups
        .as_ref()
        .and_then(|passed| passed.get(crate::appkit_content::REDRAW_GROUP))
        .is_some()
    {
        observed = appkit_group_proof(ctx, crate::appkit_content::REDRAW_GROUP, &paths, &runtime);
    }
    let out = run_row(ctx, &r.title, REDRAW_CONFORMANCE_BIN, &runtime.commands[0]);
    r.record_driven(&out, "aterm-redraw-conformance", |code, _| {
        redraw_outcome(code)
    });
    finish_appkit_group(
        ctx,
        r,
        crate::appkit_content::REDRAW_GROUP,
        &paths,
        &runtime,
        observed,
    );
}

fn objc_drives(ctx: &Ctx, r: &mut Report) {
    if !cfg!(target_os = "macos") {
        r.skip("objc drives (macOS only: the driven classes are macOS ones)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc drives (no targo)");
        return;
    }
    if !build_to_drive(
        ctx,
        r,
        objc_drives_build_args(),
        "targo build -p aterm-gui (all seven objc drivers)".to_owned(),
        "objc drives",
    ) {
        return;
    }
    let paths: Vec<(&str, std::path::PathBuf)> = OBJC_DRIVES
        .iter()
        .map(|drive| (drive.example, main_example(ctx, drive.example)))
        .collect();
    for (name, bin) in &paths {
        if !is_executable_file(bin) {
            r.cannot_run(format!(
                "{name}: just-built driver missing ({})",
                bin.display()
            ));
            return;
        }
    }
    let Some(runtime) = appkit_group_runtime(ctx, r, crate::appkit_content::OBJC_GROUP, &paths)
    else {
        return;
    };
    let mut observed = appkit_group_proof(ctx, crate::appkit_content::OBJC_GROUP, &paths, &runtime);
    if carry_appkit_group(
        ctx,
        r,
        crate::appkit_content::OBJC_GROUP,
        &paths,
        &runtime,
        observed.as_ref(),
    ) {
        return;
    }
    if ctx
        .passed_appkit_groups
        .as_ref()
        .and_then(|passed| passed.get(crate::appkit_content::OBJC_GROUP))
        .is_some()
    {
        observed = appkit_group_proof(ctx, crate::appkit_content::OBJC_GROUP, &paths, &runtime);
    }
    for (drive, command) in OBJC_DRIVES.iter().zip(&runtime.commands) {
        objc_drive(ctx, r, drive, command);
    }
    finish_appkit_group(
        ctx,
        r,
        crate::appkit_content::OBJC_GROUP,
        &paths,
        &runtime,
        observed,
    );
}

fn objc_drive(ctx: &Ctx, r: &mut Report, drive: &ObjcDrive, command: &Cmd) {
    let name = drive.example;
    let out = run_row(ctx, &r.title, name, command);
    r.record_driven(&out, name, |code, transcript| {
        objc_drive_outcome(drive, code, transcript)
    });
}

/// A group's deterministic child runtime. The per-user lock outlives every proof comparison and
/// child, while the fixed cwd is freshly recreated empty before the proof and removed on drop.
#[cfg(unix)]
struct AppkitRuntime {
    cwd: std::path::PathBuf,
    commands: Vec<Cmd>,
    _window_lock: aterm_uds::private_file::PrivateLock,
}

#[cfg(not(unix))]
struct AppkitRuntime {
    cwd: std::path::PathBuf,
    commands: Vec<Cmd>,
}

#[cfg(unix)]
impl AppkitRuntime {
    fn empty(&self) -> Result<(), String> {
        aterm_uds::private_file::check_private_dir(&self.cwd).map_err(|error| {
            format!(
                "working directory {} is no longer verifier-private: {error}",
                self.cwd.display()
            )
        })?;
        let mut entries = std::fs::read_dir(&self.cwd)
            .map_err(|error| format!("read working directory {}: {error}", self.cwd.display()))?;
        match entries.next() {
            None => Ok(()),
            Some(Ok(entry)) => Err(format!(
                "working directory {} is not empty (found {:?})",
                self.cwd.display(),
                entry.file_name()
            )),
            Some(Err(error)) => Err(format!(
                "read working directory {}: {error}",
                self.cwd.display()
            )),
        }
    }
}

#[cfg(not(unix))]
impl AppkitRuntime {
    fn empty(&self) -> Result<(), String> {
        Err(format!(
            "working directory {} has no owner-private Unix runtime contract",
            self.cwd.display()
        ))
    }
}

#[cfg(unix)]
impl Drop for AppkitRuntime {
    fn drop(&mut self) {
        // The driver may have changed its cwd's mode. The fixed parent is still private; recover
        // any real directory owned by this user, but never follow a replacement link or touch
        // another user's entry.
        if aterm_uds::private_file::check_owned_dir(&self.cwd).is_ok() {
            let _ = std::fs::remove_dir_all(&self.cwd);
        }
    }
}

#[cfg(unix)]
fn appkit_runtime_parent() -> std::path::PathBuf {
    aterm_tempfile::quiet_in(std::path::Path::new("/tmp")).join(format!(
        "aterm-verify-appkit-{}.noindex",
        aterm_uds::peer::our_uid()
    ))
}

fn finalized_appkit_command(
    ctx: &Ctx,
    path: &std::path::Path,
    cwd: &std::path::Path,
    environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) -> Result<Cmd, String> {
    let argv = [path.as_os_str().to_owned()];
    let command = Cmd::replayed(&argv, cwd, environment).counting_waits();
    // A driver is not a compiler or nested capacity claimant. Suppress those verifier-internal
    // annotations before finalizing so the child's complete environment is the explicit stable
    // allowlist above (empty in production). The outer claims remain held by the stage.
    let _covering = crate::memory::covering(None);
    let _reservation = crate::holding_resource_claim(None);
    crate::compile_width::with_jobs(None, || {
        exec::finalize_exact_command(command, ctx.exec_env())
    })
}

#[cfg(unix)]
fn appkit_group_runtime(
    ctx: &Ctx,
    r: &mut Report,
    group: &str,
    paths: &[(&str, std::path::PathBuf)],
) -> Option<AppkitRuntime> {
    let prepared = (|| -> Result<AppkitRuntime, String> {
        let parent = appkit_runtime_parent();
        aterm_uds::private_file::create_private_dir(&parent).map_err(|error| {
            format!(
                "create stable verifier-private runtime directory {}: {error}",
                parent.display()
            )
        })?;
        let parent = std::fs::canonicalize(&parent).map_err(|error| {
            format!(
                "resolve stable verifier-private runtime directory {}: {error}",
                parent.display()
            )
        })?;
        if parent.starts_with(&ctx.root) || ctx.root.starts_with(&parent) {
            return Err(format!(
                "stable working directory {} overlaps candidate checkout {}",
                parent.display(),
                ctx.root.display()
            ));
        }
        let window_lock = aterm_uds::private_file::PrivateLock::take(
            &parent,
            "windowed.lock",
            std::time::Duration::from_secs(60),
            "another AppKit verifier still owns the stable runtime",
        )
        .map_err(|error| format!("take the per-user AppKit runtime lock: {error}"))?;
        let cwd = parent.join(group);
        match std::fs::symlink_metadata(&cwd) {
            Ok(_) => {
                aterm_uds::private_file::check_owned_dir(&cwd).map_err(|error| {
                    format!("clear stable working directory {}: {error}", cwd.display())
                })?;
                std::fs::remove_dir_all(&cwd).map_err(|error| {
                    format!("clear stable working directory {}: {error}", cwd.display())
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "inspect stable working directory {}: {error}",
                    cwd.display()
                ));
            }
        }
        aterm_uds::private_file::create_private_dir(&cwd).map_err(|error| {
            format!(
                "recreate stable empty working directory {}: {error}",
                cwd.display()
            )
        })?;
        let cwd = std::fs::canonicalize(&cwd).map_err(|error| {
            format!(
                "resolve stable working directory {}: {error}",
                cwd.display()
            )
        })?;
        let commands = paths
            .iter()
            .map(|(_, path)| finalized_appkit_command(ctx, path, &cwd, Vec::new()))
            .collect::<Result<Vec<_>, _>>()?;
        if commands.iter().any(|command| !command.envs.is_empty()) {
            return Err(
                "the supposedly empty AppKit runtime gained a child environment variable".into(),
            );
        }
        let runtime = AppkitRuntime {
            cwd,
            commands,
            _window_lock: window_lock,
        };
        runtime.empty()?;
        Ok(runtime)
    })();
    match prepared {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            r.cannot_run(format!("AppKit {group}: {error}"));
            None
        }
    }
}

#[cfg(not(unix))]
fn appkit_group_runtime(
    _ctx: &Ctx,
    r: &mut Report,
    group: &str,
    _paths: &[(&str, std::path::PathBuf)],
) -> Option<AppkitRuntime> {
    r.cannot_run(format!(
        "AppKit {group}: a stable owner-private runtime is available only on Unix"
    ));
    None
}

fn appkit_group_proof(
    ctx: &Ctx,
    group: &str,
    paths: &[(&str, std::path::PathBuf)],
    runtime: &AppkitRuntime,
) -> Option<crate::appkit_content::GroupProof> {
    let context = ctx.carry_context.as_deref()?;
    runtime.empty().ok()?;
    crate::appkit_content::GroupProof::from_paths_and_commands(
        group,
        context,
        paths,
        &runtime.commands,
    )
    .ok()
}

/// Re-key the current build twice before carrying. The second read closes the interval in which
/// a concurrent build could replace a target after the first digest but before the decision.
fn carry_appkit_group(
    ctx: &Ctx,
    r: &mut Report,
    group: &str,
    paths: &[(&str, std::path::PathBuf)],
    runtime: &AppkitRuntime,
    observed: Option<&crate::appkit_content::GroupProof>,
) -> bool {
    let Some(prior) = ctx
        .passed_appkit_groups
        .as_ref()
        .and_then(|passed| passed.get(group))
    else {
        return false;
    };
    let Some(observed) = observed.filter(|observed| *observed == prior) else {
        return false;
    };
    let stable = appkit_group_proof(ctx, group, paths, runtime);
    if stable.as_ref() != Some(observed) {
        return false;
    }
    r.pass(format!(
        "AppKit {group}: carried prior ok after rebuilding and matching every executable byte"
    ));
    r.raw(ctx.record_event(&crate::appkit_content::event(observed, true)));
    true
}

/// Keep proof only for the bytes that stayed in place throughout the actual drive. A replacement
/// makes the stage incomplete: its rows describe one mapped image while the next hop would hash
/// another.
fn finish_appkit_group(
    ctx: &Ctx,
    r: &mut Report,
    group: &str,
    paths: &[(&str, std::path::PathBuf)],
    runtime: &AppkitRuntime,
    before: Option<crate::appkit_content::GroupProof>,
) {
    let Some(before) = before else {
        return;
    };
    if let Err(error) = runtime.empty() {
        r.cannot_run(format!("AppKit {group}: {error} after the group ran"));
        return;
    }
    match appkit_group_proof(ctx, group, paths, runtime) {
        Some(after) if after == before => {
            r.raw(ctx.record_event(&crate::appkit_content::event(&after, false)));
        }
        Some(_) => r.cannot_run(format!(
            "AppKit {group}: a just-driven executable changed while the group ran"
        )),
        None => r.cannot_run(format!(
            "AppKit {group}: a just-driven executable could not be re-read after the group ran"
        )),
    }
}

/// `targo --unverified build --profile test -q -p aterm -p aterm-lab --bin aterm --bin aterm-lab`: the lab and the one binary it drives ([`MAIN_PROFILE`]).
#[must_use]
pub fn codex_aterm_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        MAIN_PROFILE[0],
        MAIN_PROFILE[1],
        "-q",
        "-p",
        "aterm",
        "-p",
        "aterm-lab",
        "--bin",
        "aterm",
        "--bin",
        "aterm-lab",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The label of the Codex stage's first child — the build its proof depends on.
pub const CODEX_ATERM_BUILD_LABEL: &str =
    "targo build -p aterm -p aterm-lab (the lab and the aterm the Codex proof drives)";

/// The `SIGTERM` grace a lane over the hang ceiling gets: it cleans up at its next wait, and a `ctl turn` already in flight can take 60-90 s; a `SIGKILL` before that leaves Codex daemons that have left its process group.
pub const CODEX_TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(90);

/// The build of both binaries, in the main target.
#[must_use]
pub fn codex_aterm_build_cmd(ctx: &Ctx) -> Cmd {
    main_build_cmd(ctx, codex_aterm_build_args())
}

/// The binary the lane is handed: the main target's `debug/aterm`, which the stage's first child writes.
#[must_use]
pub fn codex_aterm_binary(ctx: &Ctx) -> std::path::PathBuf {
    main_bin(ctx, "aterm")
}

fn said<'a>(transcript: &'a str, prefix: &str) -> Option<&'a str> {
    transcript
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix(prefix))
        .map(str::trim)
        .filter(|l| !l.is_empty())
}

fn not_run_reason(transcript: &str) -> Option<&str> {
    said(transcript, "aterm-lab: NOT RUN:")
}

/// What a lab lane's `3` (`aterm-lab: NOT RUN: <why>`) is to the ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotRun {
    /// A prerequisite this machine may lack and still be whole (the codex-alab proof: no codex-alab package): a named skip, which only `release` refuses.
    Skip,
    /// The lane itself did not come up (the Linux lane: no lima, a VM, display or provision that failed, x86_64 without Rosetta, an interrupt while it waited for the run that holds it): COULD NOT RUN, exit 3, in every set.
    CouldNotRun,
}

/// How the ladder reads a lab lane's exit (`codex-alab-proof`, `linux proof`): `0` pass, `1` a finding (`finding` says where it is shown), `3` the lab's `NOT RUN:` reason read as `not_run` says, never a pass; any other code a finding, and no status decided nothing: where the ceiling ended it, the row says how long it worked and waited ([`crate::findings::CEILING_KILL`]).
#[must_use]
pub fn lab_lane_outcome(
    name: &str,
    finding: &str,
    not_run: NotRun,
    code: Option<i32>,
    transcript: &str,
) -> (Outcome, String) {
    let why = not_run_reason(transcript).unwrap_or("the lab gave no reason");
    match code {
        Some(0) => (Outcome::Ok, name.to_string()),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: {finding}"),
        ),
        Some(3) if not_run == NotRun::Skip => (
            Outcome::Skip,
            format!(
                "{name}: NOT RUN — {why} (this machine lacks a prerequisite; a named skip, never a \
                 pass)"
            ),
        ),
        Some(3) => (
            Outcome::Fail(Severity::CouldNotRun),
            format!("{name}: NOT RUN — {why} (the lane decided nothing; never a pass)"),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: exit {c} (the lane answers 0 pass, 1 a finding, 3 NOT RUN)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            transcript
                .lines()
                .find_map(|l| l.split_once(crate::findings::CEILING_KILL))
                .map_or_else(
                    || format!("{name}: no exit status — killed by a signal"),
                    |(_, kill)| format!("{name}: no exit status — ended at the ceiling{kill}"),
                ),
        ),
    }
}

/// The codex-alab proof, as the ladder names it.
pub const CODEX_ALAB_LANE: &str = "lab codex-alab-proof";

/// `<target>/debug/aterm-lab codex-alab-proof --aterm <target>/debug/aterm`.
#[must_use]
pub fn codex_alab_proof_cmd(ctx: &Ctx) -> Cmd {
    Cmd::new(main_bin(ctx, "aterm-lab"))
        .term_grace(CODEX_TERM_GRACE)
        .arg("codex-alab-proof")
        .arg("--aterm")
        .arg(codex_aterm_binary(ctx))
}

/// How the ladder reads the proof's exit: `0` pass, `1` a finding, `3` (no codex-alab package on this machine) a named skip with the lab's reason, never a pass.
#[must_use]
pub fn codex_alab_proof_outcome(code: Option<i32>, transcript: &str) -> (Outcome, String) {
    lab_lane_outcome(
        CODEX_ALAB_LANE,
        "a leg failed against the real codex-alab — its rows above say which",
        NotRun::Skip,
        code,
        transcript,
    )
}

fn codex_alab_proof(ctx: &Ctx, r: &mut Report) {
    let name = CODEX_ALAB_LANE;
    if !cfg!(target_os = "macos") {
        r.skip(format!(
            "{name} (macOS only: the instance runs under sandbox-exec)"
        ));
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip(format!(
            "{name} (no targo — nothing can build the aterm it drives)"
        ));
        return;
    }
    if !compile_labeled(ctx, r, CODEX_ATERM_BUILD_LABEL, &codex_aterm_build_cmd(ctx)) {
        r.cannot_run(format!(
            "{name} — the build above failed, so this proof has no fresh aterm to drive{}",
            NOT_RUN_BEHIND
        ));
        return;
    }
    for bin in [codex_aterm_binary(ctx), main_bin(ctx, "aterm-lab")] {
        if !is_executable_file(&bin) {
            r.cannot_run(format!(
                "{name}: the just-built {} is missing",
                bin.display()
            ));
            return;
        }
    }
    let out = exec::run(&codex_alab_proof_cmd(ctx), ctx.exec_env());
    r.record_driven(&out, name, codex_alab_proof_outcome);
}

// The guest compiles on a stock toolchain (STOCK EXCEPTION: no Linux build of Trust exists).

/// `targo --unverified build --profile test -q -p aterm-lab --bin aterm-lab` ([`MAIN_PROFILE`]).
#[must_use]
pub fn linux_lab_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        MAIN_PROFILE[0],
        MAIN_PROFILE[1],
        "-q",
        "-p",
        "aterm-lab",
        "--bin",
        "aterm-lab",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The label of the Linux stage's first child.
pub const LINUX_LAB_BUILD_LABEL: &str =
    "targo build -p aterm-lab (the lab that runs the Linux lane)";

/// The Linux lane, as the ladder names it.
pub const LINUX_LANE: &str = "lab linux proof";

/// The build of the lab, in the main target.
#[must_use]
pub fn linux_lab_build_cmd(ctx: &Ctx) -> Cmd {
    main_build_cmd(ctx, linux_lab_build_args())
}

/// The `SIGTERM` grace a Linux lane over the hang ceiling gets: the lab ends the step in flight, stops its units and the VM it booted, then exits.
pub const LINUX_TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(90);

/// `<target>/debug/aterm-lab linux proof`: every arch, every display. A run's `--admit now`
/// ([`Ctx::quiet`]) is the lab's too, for its boot's disk claim and its GUI rows' quiet claim.
#[must_use]
pub fn linux_lane_cmd(ctx: &Ctx) -> Cmd {
    let lane = Cmd::new(main_bin(ctx, "aterm-lab"))
        .term_grace(LINUX_TERM_GRACE)
        .arg("linux");
    let lane = if ctx.quiet == Some(aterm_capacity::Admit::Now) {
        lane.arg(aterm_capacity::Admit::FLAG).arg("now")
    } else {
        lane
    };
    lane.arg("proof")
}

/// The Linux lane's exit, read by [`lab_lane_outcome`]: its NOT RUN is COULD NOT RUN, exit 3, never a skip.
#[must_use]
pub fn linux_lane_outcome(code: Option<i32>, transcript: &str) -> (Outcome, String) {
    lab_lane_outcome(
        LINUX_LANE,
        "a row failed on Linux — the report above says which",
        NotRun::CouldNotRun,
        code,
        transcript,
    )
}

fn linux_lane(ctx: &Ctx, r: &mut Report) {
    let name = LINUX_LANE;
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        r.cannot_run(format!(
            "{name} (Apple silicon only: lima's vz VM with Rosetta for the x86_64 cell)"
        ));
        return;
    }
    if !ctx.tools.have_targo() {
        r.cannot_run(format!("{name} — {}", ctx.tools.missing_targo_label()));
        return;
    }
    if !compile_labeled(ctx, r, LINUX_LAB_BUILD_LABEL, &linux_lab_build_cmd(ctx)) {
        r.cannot_run(format!(
            "{name} — the build above failed, so there is no lab to run the lane{}",
            NOT_RUN_BEHIND
        ));
        return;
    }
    let lab = main_bin(ctx, "aterm-lab");
    if !is_executable_file(&lab) {
        r.cannot_run(format!(
            "{name}: the just-built {} is missing",
            lab.display()
        ));
        return;
    }
    let out = exec::run(&linux_lane_cmd(ctx), ctx.exec_env());
    r.record_driven(&out, name, linux_lane_outcome);
}

fn wasm_glue(ctx: &Ctx, r: &mut Report) {
    let work = crate::plan::cells_dir(ctx).join("wasm-glue");
    let root = ctx.root.clone();
    glue_row(
        ctx,
        r,
        "wasm-glue (the generator on aterm-wasm-abi's toy, in a browser)",
        "the toy",
        move |log| crate::wasm_glue::toy::gate(&root, &work, false, log),
    );
}

fn glue_row(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    what: &str,
    gate: impl FnOnce(&mut String) -> crate::perf::LaneRun + Send + 'static,
) {
    let run = exec::bounded(ctx.child_ceiling, move || {
        let mut log = String::new();
        let run = gate(&mut log);
        (run, log)
    });
    match run {
        Ok((run, log)) => {
            r.raw(log);
            match run.status {
                crate::perf::Status::CouldNotRun { why, fix } => {
                    r.cannot_run(format!("{label} — {why} (fix: {fix})"));
                }
                crate::perf::Status::Skip { why, fix } => {
                    r.skip(format!("{label} — {why} (fix: {fix})"));
                }
                crate::perf::Status::Fail(findings) => {
                    r.fail(label);
                    r.raw(findings.join("\n"));
                }
                crate::perf::Status::Pass => r.pass(label),
                _ => r.cannot_run(format!("{label} — {what} answered no verdict")),
            }
        }
        Err(why) => r.cannot_run(format!("{label} — {what} {}", why.sentence())),
    }
}

/// The `cells` check ([`crate::cells`]): every forge cell but this machine's
/// own (or each `--cell`), each linted for its own triple on the checkout's
/// stock pin (a STOCK EXCEPTION), one row per cell.
pub(crate) fn prepare_cells(ctx: &Ctx) -> Option<crate::cells::PreparedRun> {
    in_checkout(ctx, &mut Report::new("cells"), "cells").then(|| crate::cells::prepare(ctx))
}

fn foreign_cells(ctx: &Ctx, r: &mut Report, prepared: Option<crate::cells::PreparedRun>) {
    if in_checkout(ctx, r, "cells") {
        crate::cells::run_prepared(ctx, r, prepared);
    }
}

fn forge_gate(ctx: &Ctx, r: &mut Report) {
    const LABEL: &str = "forge check (the third-party surface)";
    if !in_checkout(ctx, r, LABEL) {
        return;
    }
    let (cargo, root) = (
        ctx.forge_cargo().with_parallel_cells(ctx.pool.is_none()),
        ctx.root.clone(),
    );
    let checked = bounded_source(ctx, move || aterm_forge::check::run(&cargo, &root, &[]))
        .unwrap_or_else(|why| Err(format!("the walk {why}")));
    match checked {
        Ok(outcome) if outcome.ok => {
            r.raw(outcome.log);
            r.pass(LABEL);
        }
        Ok(outcome) => {
            r.fail(LABEL);
            r.raw(outcome.log);
        }
        Err(why) => {
            r.cannot_run(format!("{LABEL} — {}", why.lines().next().unwrap_or("")));
            r.raw(why);
        }
    }
}

#[cfg(test)]
#[path = "stages_tests.rs"]
mod tests;
