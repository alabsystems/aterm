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
    match spec.id {
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

/// What `targo` resolves a call's unit graph with, building nothing: the call's own arguments
/// ([`piece_compile_args`]) and `--unit-graph -Z unstable-options`.
pub const UNIT_GRAPH: [&str; 3] = ["--unit-graph", "-Z", "unstable-options"];

/// The unit graph a test compile's call over `piece` would build ([`UNIT_GRAPH`]), resolved under
/// the main lane's build directory and the test compile's environment ([`test_compile_cmd`]): the one
/// line of it on the child's output that is a version-1 graph. Why not otherwise: the child failed
/// or printed none.
///
/// # Errors
/// As above.
pub fn unit_graph(ctx: &Ctx, piece: &crate::shape::Piece) -> Result<String, String> {
    crate::release_test_env::prepare(ctx)
        .map_err(|why| format!("cannot prepare release test environment: {why}"))?;
    let mut args = piece_compile_args(piece);
    args.extend(UNIT_GRAPH.map(String::from));
    let (_, bind) = test_doc_binding(ctx);
    let cmd = test_compile_cmd(ctx, bind, args);
    let out = exec::run(&cmd, ctx.exec_env());
    let graph = out
        .output
        .lines()
        .find(|l| l.starts_with("{\"version\":1"))
        .map(str::to_owned);
    match (out.ok, graph) {
        (true, Some(g)) => Ok(g),
        (_, _) => Err(format!(
            "`targo {}` {}",
            cmd.argv()[1..].join(" "),
            out.trimmed_output()
                .lines()
                .rfind(|l| !l.trim().is_empty())
                .unwrap_or("printed no unit graph")
        )),
    }
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
/// all of those, so an error in one still stops the run. GAP: the benches and examples of unchanged
/// dependents (at most the workspace's 104 targets, 19 of them with macOS arms, on 2026-11-04) have
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

/// One [`tippy_passes`] invocation: its arguments and how the report names its selection.
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

/// `<tippy> -p <pkg>… --features <pkg/feat>,… <LINT_TAIL>`: the second pass, over the targets [`tippy_args`] cannot reach (`gated`, [`gated_lint_features`]).
#[must_use]
pub fn tippy_gated_args(scope: &Scope, gated: &[(String, String)]) -> Option<Vec<String>> {
    let mut packages: Vec<String> = Vec::new();
    let mut features: Vec<String> = Vec::new();
    for (pkg, feat) in gated {
        let Some(spec) = scope.spec_of(pkg) else {
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
    /// What exit `0` proved, in one line.
    pub pass: &'static str,
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
        pass: "every row the registered WinitWindowDelegate and WinitView hold agrees with the runtime's own authority, and each class claims the protocols its rows come from",
    },
    ObjcDrive {
        example: "objc_ime_drive",
        code3: Code3::WeakerPassOk(
            "a composition ran through the ported WinitView's NSTextInputClient rows, and with Option as Alt, each Option chord arrived as its bare key — but the same chords WITHOUT Option as Alt did not all compose here (macOS did not make the drive the active app, or this layout's Option chords are not the U.S. dead keys), so whether Option as Alt stops each dead key was NOT measured (exit 3)",
        ),
        no_status: Severity::CouldNotRun,
        pass: "a composition ran through the ported WinitView's NSTextInputClient rows — preedit, cursor conversion, commit and the candidate-window rectangle — and with Option as Alt, no U.S. Option dead key (E, U, I, N, `) started a composition where the same key without it did, and Option+B arrived as b, not ∫",
    },
    ObjcDrive {
        example: "objc_toolbar_drive",
        code3: Code3::Hung,
        no_status: Severity::CouldNotRun,
        pass: "the real tab strip answered every question — every state its drawing matrix names, and all four declared classes read off live objects against the runtime's own authority — and the menu bar's real status item held This Mac as its glance counts it, and listed a box that stopped its agent above an older report whose row's tip says its program carries on meanwhile, and a landing's Done for you row and worker row whose tips say what aterm checked",
    },
    ObjcDrive {
        example: "objc_window_drive",
        code3: Code3::None,
        // A death by a signal is a crash in the window code the drive runs: a finding. Closing an answered NSAlert's glass child window at exit (aterm 0.110.0 on macOS 27.2) died by SIGSEGV in the exit turn's pool pop.
        no_status: Severity::GateFailed,
        pass: "the real window answered every question — title, style mask, geometry, limits, theme, drag-and-drop, close and fullscreen — and the exit, in the turn a click answered a real NSAlert, closed only winit's own window and none of the alert's",
    },
    ObjcDrive {
        example: "objc_event_drive",
        code3: Code3::AbortFinding,
        no_status: Severity::GateFailed,
        pass: "every NSEvent-taking WinitView row and sendEvent: survived a real NSEvent of every type AppKit can deliver; the exception control was contained and the panic control aborted",
    },
    ObjcDrive {
        example: "objc_frame_clock_drive",
        code3: Code3::Hung,
        // A death by a signal is a crash in the link's callback or AppKit's window teardown while the drive held them: a finding. (A link left uninvalidated did not crash it at the desk, 2026-10-27 and 2026-10-28: no tick came after the close. On 2026-10-28 the drive's weak reference, still loading the link after the close, caught it, exit 1.)
        no_status: Severity::GateFailed,
        pass: "a real window's display link ran the deck's clock: by the link's own timestamps its ticks each named a period the screen runs, targeted the coming vsync (the median tick's and the best tick's lead both positive) and never stalled LINK_SILENCE, and the clock's hang detector never tripped; paused, the CADisplayLink said so (-isPaused) and fired nothing while the clock's detector tripped on real time and timed the window as one with no link, and the restarted link's first tick resumed the clock; the link ticked while the main run loop ran in NSEventTrackingRunLoopMode; and closing the window invalidated every link its view made (a weak reference to each CADisplayLink, which its run loop retains until -invalidate, loaded nil after the close, and no tick fired for a vsync after it)",
    },
    ObjcDrive {
        example: "objc_confirm_drive",
        code3: Code3::Hung,
        no_status: Severity::CouldNotRun,
        pass: "the Stop Worker sheet came up on a real NSAlert with its title, body and buttons; a click on Stop answered yes, and a click on Cancel, Return and Escape each no; and Rename This Mac's real alert showed its two fields, its buttons and the live line for what was typed into them through their field editor, disabled Rename for a number the read it was handed through the main queue gave another Mac (Return then did nothing and Escape cancelled), redrew the line when a read landed after typing, moved each untouched field to a read newer than the window's view while a field typed into before it kept its words and the caret stayed in the field being edited, emptied the Number field of a number a migration still waits on until it was typed to claim it, and answered Rename with only what was typed",
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

/// How the ladder reads one driver's exit without a transcript.
#[must_use]
pub fn objc_drive_outcome(drive: &ObjcDrive, code: Option<i32>) -> (Outcome, String) {
    objc_drive_outcome_with_transcript(drive, code, "")
}

/// How the ladder reads one driver's exit and transcript, quoting its `NOT RUN — <reason>` line on exit `2`.
#[must_use]
pub fn objc_drive_outcome_with_transcript(
    drive: &ObjcDrive,
    code: Option<i32>,
    transcript: &str,
) -> (Outcome, String) {
    let name = drive.example;
    let (outcome, what) = match (code, drive.code3) {
        (Some(0), _) => (Outcome::Ok, drive.pass.to_string()),
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

/// `targo --unverified run -q -p aterm-libc-oracle --target-dir <lane>/gate-bin -- --root <R> --target-dir <lane>`, from `<R>`: the oracle built and run inside its own lane ([`Lane::LibcOracleTarget`], `<R>/libc-oracle/target.noindex`), so it never queues on the main workspace's cargo lock.
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

fn run_tests_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    if let Err(why) = crate::release_test_env::prepare(ctx) {
        r.cannot_run(format!(
            "cannot prepare release test environment: {why}; no test was run"
        ));
        return false;
    }
    let rerun = crate::findings::Rerun::of(cmd, ctx.test_thread_override());
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
/// `compile-finished` event goes to stderr at once, its line to its caller for its report. Never
/// a call that runs tests or programs: their stdout is theirs.
fn run_compile(ctx: &Ctx, step: &str, call: &str, cmd: &Cmd) -> Compile {
    let cmd = crate::compile_account::with_message_format(cmd.clone());
    let began = std::time::Instant::now();
    crate::event_log::emit(
        ctx.events.as_ref(),
        &crate::report::row_event(step, call, None),
    );
    let exec::MachineRun { run, stdout } = exec::run_machine(&cmd, ctx.exec_env(), Some(step));
    let ended = std::time::Instant::now();
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

/// [`compile_labeled`], skipped on an empty change cone.
fn compile_scoped(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) {
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    compile_labeled(ctx, r, label, cmd);
}

/// Why a selection-reading stage did nothing on an empty change cone, and the fix.
pub const EMPTY_CONE: &str = "(the change cone holds no crate — fix: pass -p <crate> or --all)";

fn targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    Cmd::new(&ctx.tools.targo).args(args)
}

/// A `targo` invocation in the main target, sharing plain targo's build cache
/// ([`Ctx::main_build_dir`]) and workspace feature unification
/// ([`aterm_toolchain::build_cache::with_test_graph`]). Generated configs go before the verb.
/// If a caller supplies command-local config, all configs move after the verb together:
/// Trust 16387 reads command-local configs in place of global ones.
#[must_use]
pub fn main_targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    let mut args = aterm_toolchain::build_cache::with_test_graph(&args);
    let mut cmd = Cmd::new(&ctx.tools.targo);
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
/// Behind a call that did not pass, a call is COULD NOT RUN and starts no cargo.
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
    if let Err(why) = crate::release_test_env::prepare(ctx) {
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
    let (compiled, _, artifacts, script_runs) =
        compile_tests(ctx, r, &format!("{label}{suffix}"), &compile);
    crate::disk::note_used(ctx, &artifacts, &script_runs);
    if !compiled {
        ctx.test_compile_failed
            .store(true, std::sync::atomic::Ordering::Release);
    } else if let Some(part) = spec.part.as_ref().filter(|p| p.shaped()) {
        shape_after(ctx, part, &artifacts, began);
    }
    // Only the closing workspace compile owns the bundle drivers. This runs in
    // qualify's tree branch, beside the release build, never in its build branch.
    if compiled && cut.is_none() && crate::plan::folds_measuring(ctx) {
        for args in appkit_prebuild_args() {
            let label = format!("appkit prebuild: targo {}", args.join(" "));
            let cmd = main_build_cmd(ctx, args);
            if !compile_labeled(ctx, r, &label, &cmd) {
                ctx.test_compile_failed
                    .store(true, std::sync::atomic::Ordering::Release);
                break;
            }
        }
    }
}

/// Build the exact units the bundle's AppKit rows will build again. Cargo's
/// fingerprints, rather than a saved verdict, decide whether they are fresh.
#[must_use]
pub fn appkit_prebuild_args() -> Vec<Vec<String>> {
    let mut builds = vec![redraw_conformance_build_args()];
    if cfg!(target_os = "macos") {
        builds.push(objc_drives_build_args());
    }
    builds
}

/// The closing call `part` keeps again each refusal its plan read ([`crate::shape::Part::refused`],
/// [`crate::memory::Book::refuse`]), so none it keeps reading becomes its record's oldest row;
/// one it could not keep goes on its time line.
fn keep_refusals(ctx: &Ctx, part: &crate::shape::Part) {
    let Some(book) = ctx.memory.as_ref() else {
        return;
    };
    let whole = crate::shape::Piece::of(ctx.scope.crates(), false);
    let checkout = ctx.root.display().to_string();
    let failed: Vec<String> = part
        .refused
        .iter()
        .filter_map(|cut| {
            book.refuse(&crate::shape::refusal_key(&checkout, &whole, cut))
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

/// What a test compile's child did: whether it passed, the test executables it built, every
/// artifact it reported ([`crate::shape::artifacts`]), and the `out_dir` of every build script it
/// ran ([`crate::shape::script_runs`]).
type Compiled = (
    bool,
    Result<Vec<crate::testrun::TestExe>, String>,
    Vec<crate::shape::Artifact>,
    Vec<String>,
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
            return (false, Err(why), Vec::new(), Vec::new());
        }
    };

    let artifacts = crate::shape::artifacts(&stdout);
    let script_runs = crate::shape::script_runs(&stdout);
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
    (run.ok, exes, artifacts, script_runs)
}

/// After call `part` of a shaped test compile, which began at `began`, passed with `artifacts`: a
/// cut keeps the files it built, and when it began, for the calls after it; every later call
/// checks the earlier cuts' files came back fresh ([`crate::shape::fresh_check`], the sources it
/// rebuilt from read under the checkout: [`crate::shape::changed_since`]), and keeps a refusal row
/// for each cut that did not ([`crate::shape::refusal_key`]), so no later run cuts it; what it
/// found goes on the call's time line.
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
    let whole = crate::shape::Piece::of(ctx.scope.crates(), false);
    let checkout = ctx.root.display().to_string();
    let mut said = vec![said]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    for (cut, _) in &refused {
        let key = crate::shape::refusal_key(&checkout, &whole, cut);
        if let Some(Err(e)) = ctx.memory.as_ref().map(|b| b.refuse(&key)) {
            said.push(format!("shape: the refusal was not kept: {e}"));
        }
        run.built.retain(|(c, _, _)| c != cut);
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
    crate::release_test_env::command(ctx, cmd.demoted())
}

fn test_cmds(ctx: &Ctx, bind: bool) -> [Cmd; 2] {
    let (k, v) = TRAIL_LAWS_FULL;
    let mut compile = test_compile_args(&ctx.scope);
    if crate::plan::folds_measuring(ctx) {
        compile.extend(measuring_bench_args());
    }
    [
        test_compile_cmd(ctx, bind, compile),
        crate::release_test_env::command(
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
/// are stated, never a finding (owner ruling 2026-11-05: a busy machine never blocks a release).
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
        // and never a refusal (owner ruling 2026-11-05: a busy machine never blocks a release).
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
    run_tests_labeled(ctx, r, &label, &cmd);
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
    let cmd = crate::release_test_env::command(
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
    cmd.args(args)
        .env("PATH", path)
        .env(
            "CARGO_TARGET_DIR",
            lane_dir(ctx, Lane::TippyTarget).unwrap_or_else(|| ctx.target_dir.clone()),
        )
        .env("TRUST_NO_MIGRATE_WARN", "1")
        .demoted()
}

fn tippy(ctx: &Ctx, r: &mut Report) {
    let Some(bin) = ctx.tools.tippy.clone() else {
        r.skip(ctx.tools.missing_tippy_label());
        return;
    };
    let borrowed = crate::fmt::borrowed_suffix(ctx, crate::toolchain::TIPPY);
    for pass in tippy_passes(&ctx.scope) {
        compile_scoped(
            ctx,
            r,
            &format!("tippy {} -D warnings{borrowed}", pass.label),
            &tippy_cmd(ctx, &bin, lint_unified(pass.args)),
        );
        crate::red_now(ctx, crate::checks::Check::Lint, r);
    }
    if ctx.release
        && let Some(args) = tippy_gated_args(&ctx.scope, &gated_lint_features(&ctx.root))
    {
        compile_scoped(
            ctx,
            r,
            &format!("tippy required-features targets -D warnings{borrowed}"),
            &tippy_cmd(ctx, &bin, lint_unified(args)),
        );
        crate::red_now(ctx, crate::checks::Check::Lint, r);
    }
    if let Some(args) = tippy_release_args(&ctx.scope) {
        compile_scoped(
            ctx,
            r,
            &format!("tippy -p {SHIPPED} --release -D warnings{borrowed}"),
            &tippy_cmd(ctx, &bin, args),
        );
    }
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
/// ([`census_baseline`]).
fn walks(ctx: &Ctx, r: &mut Report, check: &str) {
    let selected: Vec<&aterm_census::Census> = aterm_census::CENSUSES.iter().collect();
    let root = ctx.root.clone();
    let var = engine_vars(ctx);
    let outcomes = match exec::bounded(ctx.child_ceiling, move || {
        census_walk(&root, &var, &selected)
    }) {
        Ok(o) => o,
        Err(why) => {
            r.cannot_run(format!("{check} — the walk {}", why.sentence()));
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
}

/// The engine-naming variables of `ctx`'s environment ([`crate::EnvSnapshot::engine_var`]),
/// owned so a walk on another thread reads them.
fn engine_vars(ctx: &Ctx) -> impl Fn(&str) -> Option<std::ffi::OsString> + Send + 'static {
    let held = ["PUBLICATION_ENGINE", "HOME"].map(|name| (name, ctx.env.engine_var(name)));
    move |name| {
        held.iter()
            .find(|(held, _)| *held == name)
            .and_then(|(_, value)| value.clone())
    }
}

/// The `selected` censuses over the checkout at `root`, `capture-privacy` held to the baseline
/// of the engine's commit `var` names ([`census_baseline`]).
fn census_walk(
    root: &std::path::Path,
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    selected: &[&aterm_census::Census],
) -> Vec<aterm_census::CensusOutcome> {
    aterm_census::run_censuses(root, selected, &census_baseline(root, var))
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
            let mut git = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
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
    main_targo(ctx, args)
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
    let out = run_row(ctx, &r.title, REDRAW_CONFORMANCE_BIN, &Cmd::new(&bin));
    r.record_driven(&out, "aterm-redraw-conformance", |code, _| {
        redraw_outcome(code)
    });
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
    for drive in &OBJC_DRIVES {
        objc_drive(ctx, r, drive);
    }
}

fn objc_drive(ctx: &Ctx, r: &mut Report, drive: &ObjcDrive) {
    let name = drive.example;
    let bin = main_example(ctx, name);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "{name}: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    let out = run_row(ctx, &r.title, name, &Cmd::new(&bin));
    r.record_driven(&out, name, |code, transcript| {
        objc_drive_outcome_with_transcript(drive, code, transcript)
    });
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
    let (cargo, root) = (ctx.forge_cargo(), ctx.root.clone());
    let checked = exec::bounded(ctx.child_ceiling, move || {
        aterm_forge::check::run(&cargo, &root, &[])
    })
    .unwrap_or_else(|why| Err(format!("the walk {}", why.sentence())));
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
mod tests {
    use super::*;
    use crate::EnvSnapshot;
    use std::path::PathBuf;

    #[test]
    fn census_reports_harness_debt_as_a_gate_failure() {
        let temp = aterm_tempfile::TempDir::new().expect("scratch repository");
        let root = temp.path();
        let git = |args: &[&str]| {
            let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
                .current_dir(root)
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "A"]);
        git(&["config", "user.email", "a@example.invalid"]);
        let version_file = aterm_phase::CliHarness::ClaudeCode.version_file();
        let source = "crates/aterm-agent/src/harness/upgrade_drive.rs";
        for path in [version_file, source, CHECKOUT_MARKER] {
            std::fs::create_dir_all(root.join(path).parent().expect("parent")).expect("mkdir");
        }
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\n[workspace.package]\nversion = \"0.1.0\"\n",
        )
        .expect("manifest");
        std::fs::write(
            root.join(version_file),
            format!("{}\n", aterm_phase::harness_version::source_line((0, 1))),
        )
        .expect("version source");
        std::fs::write(root.join(source), "fn drive() {}\n").expect("harness source");
        std::fs::write(root.join(CHECKOUT_MARKER), "").expect("checkout marker");
        git(&["add", "."]);
        git(&["commit", "-qm", "public release"]);
        git(&["tag", "-am", "public release", "v0.1.0"]);
        std::fs::write(root.join(source), "fn drive() { changed(); }\n")
            .expect("changed harness source");
        git(&["add", "."]);
        git(&["commit", "-qm", "unbumped harness source"]);

        let ctx = Ctx::new(
            root.to_path_buf(),
            Scope::workspace(),
            EnvSnapshot::default(),
            root.join("scratch"),
        );
        let mut report = Report::new("census");
        census(&ctx, &mut report);
        let rows: Vec<_> = report.outcomes().collect();
        // NEGATIVE CONTROL: removing Census's harness-debt call leaves no debt-labelled gate
        // failure even though the source changed after the public tag.
        assert!(
            rows.iter().any(|(outcome, label)| {
                *outcome == Outcome::Fail(Severity::GateFailed)
                    && label.contains("harness landing debt since public v0.1.0")
            }),
            "production Census stage did not report the debt: {rows:?}"
        );
    }

    /// THE CENSUS WALK HOLDS CAPTURES TO THE BASELINE OF THE ENGINE'S COMMIT: a capture holding a
    /// word only the named engine's committed baseline lists is a `capture-privacy` finding of
    /// the guard census. NEGATIVE CONTROL: the same word left uncommitted in that engine's
    /// working tree (what the census read before) judges nothing, and the walk says it held
    /// captures to the commit's baseline.
    #[test]
    fn the_census_walk_holds_captures_to_the_engines_committed_baseline() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let write = |path: &std::path::Path, text: &str| {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        };
        let git = |at: &std::path::Path, args: &[&str]| {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let out = crate::changed::git(at, &path)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        let root = dir.path().join("repo");
        write(
            &root.join(aterm_census::export_scan::rows::EXTRA),
            "# x\nzebrafish\n",
        );
        write(&root.join("crates/x/src/lib.rs"), "// x\n");
        let capture: String = b"plain quokkaword\r\n"
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        write(
            &root.join("crates/x/tests/fixtures/base.ptylog"),
            &format!("1000 O {capture}\n"),
        );
        let engine = dir.path().join("engine");
        let baseline = engine.join(aterm_census::export_scan::rows::BASELINE);
        write(&baseline, "# rows\n");
        git(dir.path(), &["init", "-q", "-b", "main", "engine"]);
        git(&engine, &["add", "-A"]);
        git(&engine, &["commit", "-q", "-m", "engine"]);
        write(&baseline, "# rows\nquokkaword\n");
        let named = engine.clone().into_os_string();
        let var = move |name: &str| (name == "PUBLICATION_ENGINE").then(|| named.clone());
        let guards: Vec<&aterm_census::Census> = aterm_census::CENSUSES
            .iter()
            .filter(|c| c.name == "guards")
            .collect();
        assert_eq!(guards.len(), 1, "the guard census");
        let log = |outcomes: Vec<aterm_census::CensusOutcome>| {
            outcomes.into_iter().map(|o| o.log).collect::<String>()
        };
        let uncommitted = log(census_walk(&root, &var, &guards));
        assert!(
            !uncommitted.contains("base.ptylog"),
            "NEGATIVE CONTROL: an uncommitted row is no part of the engine: {uncommitted}"
        );
        git(&engine, &["commit", "-q", "-am", "row"]);
        let committed = log(census_walk(&root, &var, &guards));
        assert!(
            committed.contains("base.ptylog"),
            "the committed row holds: {committed}"
        );
    }

    /// A cone lints all the targets of what the change touched and the default targets of the rest,
    /// matching by package name whichever spec the cone holds it by. CONTROL: `-p`, `--all`, an
    /// empty cone, a cone where everything is touched (a lock row's) stay one `--all-targets` pass;
    /// and a list that matches nothing never reaches [`Scope::args`], which would answer
    /// `--workspace`.
    #[test]
    fn a_cone_lints_all_the_targets_of_what_the_change_touched() {
        let spec = "path+file:///r/crates/aterm-x#x@1.0.0";
        let crates = vec![
            "aterm-gui".to_string(),
            "aterm-uds".to_string(),
            "getrandom@0.2.17".to_string(),
            spec.to_string(),
        ];
        let cone = |touched: &[&str]| {
            Scope::changed("main", crates.clone(), true)
                .with_all_targets(touched.iter().map(ToString::to_string).collect())
        };
        let tail = |all: bool| -> Vec<String> {
            let tail: &[&str] = if all {
                &LINT_TAIL
            } else {
                &["--keep-going", "--", "-D", "warnings"]
            };
            tail.iter().map(ToString::to_string).collect()
        };
        let select = |names: &[&str]| -> Vec<String> {
            names
                .iter()
                .flat_map(|name| ["-p".to_string(), (*name).to_string()])
                .collect()
        };
        let args = |scope: &Scope| -> Vec<Vec<String>> {
            tippy_passes(scope).into_iter().map(|p| p.args).collect()
        };
        assert_eq!(
            args(&cone(&["aterm-uds", "getrandom", "x"])),
            [
                [select(&["aterm-uds", "getrandom@0.2.17", spec]), tail(true)].concat(),
                [select(&["aterm-gui"]), tail(false)].concat(),
            ]
        );
        let passes = tippy_passes(&cone(&["aterm-uds"]));
        assert_eq!(passes[0].label, "-p aterm-uds --all-targets");
        assert!(
            passes[1]
                .label
                .starts_with("-p aterm-gui -p getrandom@0.2.17")
        );
        assert!(!passes[1].label.contains("--all-targets"), "{passes:?}");
        // What the cone holds only by a dev edge is in neither pass, matched by package name.
        let left_out = |names: &[&str]| {
            cone(&["aterm-uds"]).with_unlinted(names.iter().map(ToString::to_string).collect())
        };
        assert_eq!(
            args(&left_out(&["aterm-gui", "getrandom"])),
            [
                [select(&["aterm-uds"]), tail(true)].concat(),
                [select(&[spec]), tail(false)].concat(),
            ]
        );
        assert_eq!(
            args(&left_out(&["aterm-gui", "getrandom", "x"])),
            [[select(&["aterm-uds"]), tail(true)].concat()],
            "with none of the rest left, no second pass"
        );
        // The whole cone: a lock row the change reached.
        let whole = Scope::changed("main", crates.clone(), true);
        assert_eq!(args(&whole), [tippy_args(&whole)]);
        for scope in [Scope::workspace(), Scope::crate_only("aterm-gui")] {
            assert_eq!(args(&scope), [tippy_args(&scope)], "{scope:?}");
        }
        let empty = Scope::changed("main", Vec::new(), false);
        assert!(empty.selects_nothing());
        assert_eq!(args(&empty), [tippy_args(&empty)]);
        assert_eq!(
            args(&cone(&[])),
            [[
                select(&["aterm-gui", "aterm-uds", "getrandom@0.2.17", spec]),
                tail(false)
            ]
            .concat()],
            "no list that matches nothing widens to --workspace"
        );
    }

    #[test]
    fn a_versioned_package_keeps_its_required_feature_lint_pass() {
        let gated = vec![("aterm-gui".to_string(), "control-conformance".to_string())];
        let bare = tippy_gated_args(&Scope::crate_only("aterm-gui"), &gated)
            .expect("the selected bare package has a required-feature pass");
        for wrong in [
            "aterm-gui-extra@0.106.0",
            "other-aterm-gui@0.106.0",
            "aterm-gui@",
        ] {
            assert_eq!(tippy_gated_args(&Scope::crate_only(wrong), &gated), None);
        }
        let spec = "aterm-gui@0.106.0";
        let scopes = [
            Scope::crate_only(spec),
            Scope::changed("main", vec![spec.into()], true),
        ];
        let mut expected = vec!["-p".to_string(), spec.to_string()];
        expected.extend(LINT_TAIL.map(String::from));
        for scope in &scopes {
            assert_eq!(
                tippy_args(scope),
                expected,
                "the ordinary pass keeps its exact version"
            );
        }
        let passes: Vec<_> = scopes
            .iter()
            .map(|scope| tippy_gated_args(scope, &gated))
            .collect();
        let mut versioned = bare;
        versioned[1] = spec.to_string();
        assert_eq!(passes, vec![Some(versioned.clone()), Some(versioned)]);
    }

    fn ctx(scope: Scope) -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            scope,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    /// A PURE COMPILE CALL WRITES ITS COMPILE LINE (A1): its Cargo arguments get the message
    /// format before their `--` (the lint driver's arguments after it are left alone), its units
    /// come from its artifact messages (2 fresh, 1 compiled, 1 with no `fresh` unknown), its
    /// output bytes from the 3 MiB file the compiled one names (not the 5 MiB a fresh one names,
    /// nor the file the unknown one names, which is gone), and its row is decided as
    /// before, on its rendered diagnostics, with no machine message in its report and what it
    /// printed for a person kept. A flag already given is not given twice, and a call with no
    /// `--` gets it last. NEGATIVE CONTROLS: the flag after `--` (where the lint driver would get
    /// it) fails the stand-in; the call's stdout put back whole puts `{"reason"` in the report.
    #[cfg(unix)]
    #[test]
    fn a_pure_compile_call_writes_its_line_and_keeps_cargos_messages_out_of_its_report() {
        let root = crate::mktemp_dir("atv-compile-line").expect("scratch");
        let rlib = root.join("liba.rlib");
        std::fs::write(&rlib, vec![0_u8; 3 << 20]).expect("rlib");
        std::fs::write(root.join("b.rlib"), vec![0_u8; 5 << 20]).expect("a fresh unit's rlib");
        let artifact = |name: &str, fresh: &str, file: &std::path::Path| {
            format!(
                "{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"{name}\",\"kind\":[\"lib\"]}},{fresh}\"filenames\":[\"{}\"]}}",
                file.display()
            )
        };
        let wire = [
            artifact("a", "\"fresh\":false,", &rlib),
            artifact("b", "\"fresh\":true,", &root.join("b.rlib")),
            artifact("c", "\"fresh\":true,", &root.join("c.rlib")),
            artifact("d", "", &root.join("gone.rlib")),
            "{\"reason\":\"build-finished\",\"success\":true}".to_owned(),
        ]
        .join("\n");
        std::fs::write(root.join("wire"), format!("{wire}\n")).expect("wire");
        let c = Ctx::new(
            root.clone(),
            Scope::workspace(),
            EnvSnapshot::default(),
            root.clone(),
        );
        let script = "[ \"$1 $2 $3\" = \"--keep-going --message-format=json-render-diagnostics --\" ] \
                      || { echo \"flag misplaced: $*\" >&2; exit 3; }; /bin/cat wire; \
                      echo 'a person reads this'; echo 'warning: rendered for a person' >&2";
        let cmd = Cmd::new("/bin/sh").args([
            "-c",
            script,
            "compile-fixture",
            "--keep-going",
            "--",
            "-D",
            "warnings",
        ]);
        let mut r = Report::new("tippy lint (-p a)");
        assert!(compile_labeled(&c, &mut r, "tippy -p a", &cmd));
        let block = r.render();
        assert!(
            block.contains("  compile tippy lint/tippy -p a: 2 fresh / 1 compiled / 1 unknown, ")
                && block.contains(&format!(", {} out", aterm_capacity::size(3 << 20))),
            "{block}"
        );
        assert!(!block.contains("{\"reason\""), "{block}");
        assert!(
            block.contains("a person reads this") && block.contains("rendered for a person"),
            "{block}"
        );
        assert_eq!(
            r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
            [crate::Outcome::Ok]
        );
        let flagged = |args: &[&str]| {
            crate::compile_account::with_message_format(Cmd::new("targo").args(args.to_vec()))
                .args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            flagged(&["build", "--message-format=json", "--", "-x"]),
            ["build", "--message-format=json", "--", "-x"]
        );
        assert_eq!(
            flagged(&["build", "-q"]),
            ["build", "-q", crate::compile_account::MESSAGE_FORMAT]
        );
        assert_eq!(
            flagged(&["test", "--", "--message-format=x"]),
            [
                "test",
                crate::compile_account::MESSAGE_FORMAT,
                "--",
                "--message-format=x"
            ]
        );
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn a_successful_clean_compile_indexes_its_executables_but_a_failure_or_edit_does_not() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let target = root.join("target");
        std::fs::create_dir_all(target.join("tools")).unwrap();
        // This probe is a test double, not a compiler; echo supplies observable identity
        // output without copying an Apple executable into scratch.
        std::os::unix::fs::symlink("/bin/echo", target.join("tools/trustc")).unwrap();
        let binary = target.join("aterm_gui-0123456789abcdef");
        std::fs::write(&binary, b"binary").unwrap();
        let wire = aterm_json::json!({"reason":"compiler-artifact", "package_id":"aterm-gui", "manifest_path":root.join("crates/aterm-gui/Cargo.toml"), "target":{"kind":["lib"],"name":"aterm_gui"}, "profile":{"test":true}, "executable":binary}).to_string();
        std::fs::write(root.join("wire"), wire).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Andrew Yates",
                "-c",
                "user.email=test@aterm.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "compile input",
            ],
        ] {
            assert!(
                aterm_uds::spawnfd::command("git")
                    .current_dir(&root)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let mut ctx = Ctx::new(
            root.clone(),
            Scope::workspace(),
            EnvSnapshot::default(),
            target.clone(),
        );
        ctx.tools.targo = target.join("tools/targo");
        let path = ctx.state_dir().join("test-executables.json");
        for (script, code, indexed) in [
            ("/bin/cat wire; exit \"$1\"", "0", true),
            ("/bin/cat wire; exit \"$1\"", "3", false),
            ("/bin/cat wire; /usr/bin/touch changed", "0", false),
        ] {
            let _ = std::fs::remove_file(&path);
            let cmd = Cmd::new("/bin/sh").args(["-c", script, "compile-index", code]);
            let mut report = Report::new("test compile");
            let _ = compile_tests(&ctx, &mut report, "test compile", &cmd);
            let index = crate::test_index::Index::read(&path);
            assert_eq!(index.is_some(), indexed, "{}", report.render());
            if let Some(index) = index {
                assert!(
                    index
                        .fixture_binary(&index.tree, &index.toolchain, &root)
                        .is_some()
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn compiler_stderr_cannot_supply_unit_artifacts_or_test_executables() {
        let root = crate::mktemp_dir("atv-machine-stream").expect("scratch");
        let wire = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl")
            .replace("\"test\":false", "\"test\":true")
            .replace("\"executable\":null", "\"executable\":\"/synthetic-test\"");
        std::fs::write(root.join("wire"), &wire).expect("wire");
        let c = Ctx::new(
            root.clone(),
            Scope::workspace(),
            EnvSnapshot::default(),
            root.clone(),
        );
        let mut failures = Vec::new();
        for (source, code) in [("stderr", 0), ("stderr", 3), ("stdout", 3)] {
            let script = if source == "stderr" {
                "printf 'human stdout\\n'; /bin/cat wire >&2; printf 'human stderr\\n' >&2; exit \"$1\""
            } else {
                "printf 'human stdout\\n'; /bin/cat wire; printf 'human stderr\\n' >&2; exit \"$1\""
            };
            let cmd =
                Cmd::new("/bin/sh").args(["-c", script, "machine-fixture", &code.to_string()]);
            let mut report = Report::new("machine stream");
            let (ok, exes, artifacts, _) = compile_tests(&c, &mut report, "machine stream", &cmd);
            assert_eq!(ok, code == 0);
            let exes = exes.expect("complete wire");
            let raw = report.render();
            assert!(raw.contains("human stdout") && raw.contains("human stderr"));
            // The test compile is a compile call: one line, its units read from stdout alone.
            let units = if source == "stdout" {
                ": 0 fresh / 1 compiled, "
            } else {
                ": 0 fresh / 0 compiled, "
            };
            let lines = compile_lines(&raw, "machine stream/machine stream");
            if lines.len() != 1 || !lines[0].contains(units) {
                failures.push(format!(
                    "{source}/exit{code}: compile lines {lines:?}; expected one with {units:?}"
                ));
            }
            let expected = usize::from(source == "stdout");
            let causes = raw.matches("\"rebuild_reason\":\"changed-env\"").count();
            if (exes.len(), artifacts.len(), causes) != (expected, expected, expected) {
                failures.push(format!(
                    "{source}/exit{code}: executables={}, artifacts={}, causes={causes}; expected {expected}",
                    exes.len(), artifacts.len()
                ));
            }
        }
        for code in [0, 3] {
            let cmd = Cmd::new("/bin/sh").args([
                "-c",
                "printf '\\377\\n'; /bin/cat wire; printf 'human stderr\\n' >&2; exit \"$1\"",
                "invalid-fixture",
                &code.to_string(),
            ]);
            let mut report = Report::new("invalid machine stream");
            let (ok, exes, artifacts, _) = compile_tests(&c, &mut report, "invalid stream", &cmd);
            assert!(!ok);
            assert!(exes.is_err() && artifacts.is_empty());
            let raw = report.render();
            let lines = compile_lines(&raw, "invalid machine stream/invalid stream");
            assert!(
                lines.len() == 1 && lines[0].contains(": units unreported, "),
                "{raw}"
            );
            let actual = if code == 0 {
                crate::Outcome::Ok
            } else {
                crate::Outcome::Fail(crate::Severity::GateFailed)
            };
            assert_eq!(
                report.outcomes().next().map(|(outcome, _)| outcome),
                Some(actual)
            );
            assert!(report.outcomes().any(|(outcome, _)| {
                outcome == crate::Outcome::Fail(crate::Severity::CouldNotRun)
            }));
            assert!(report.render().contains("human stderr"));
        }
        std::fs::remove_dir_all(&root).expect("cleanup");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The compile lines of `call` (`<step>/<label>`) in a rendered report.
    fn compile_lines<'a>(block: &'a str, call: &str) -> Vec<&'a str> {
        let head = format!("  compile {call}: ");
        block.lines().filter(|l| l.starts_with(&head)).collect()
    }

    /// A DRIVEN BINARY'S BUILD WRITES ITS ONE COMPILE LINE, BUILT OR NOT: the build of a binary a
    /// stage drives is a compile call ([`run_compile`]) whose line, its units and the bytes it
    /// wrote read from Cargo's messages, stands in its report whether the build passes (the line
    /// alone, no row) or fails (its output with what it printed for a person, the line, its FAIL
    /// row and the row of what it kept from running), and no Cargo message reaches the report.
    #[cfg(unix)]
    #[test]
    fn a_driven_binarys_build_writes_its_compile_line_whether_it_builds_or_not() {
        let tmp = std::fs::canonicalize(crate::mktemp_dir("atv-drive-build").expect("mktemp"))
            .expect("real");
        std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
        let built = tmp.join("libdriven.rlib");
        std::fs::write(&built, vec![0_u8; 2 << 20]).expect("rlib");
        let targo = tmp.join("bin/targo");
        let mut c = Ctx::new(
            tmp.clone(),
            Scope::workspace(),
            EnvSnapshot::default(),
            tmp.join("scratch"),
        );
        c.tools.targo = targo.clone();
        for code in [0, 1] {
            crate::standin_body(
                &targo,
                &format!(
                    "printf '%s\\n' '{{\"reason\":\"compiler-artifact\",\"fresh\":false,\"filenames\":[\"{}\"]}}'\n\
                     echo 'printed for a person'\nexit {code}",
                    built.display()
                ),
            );
            let mut r = Report::new("redraw conformance (the verb matrix)");
            let ok = build_to_drive(
                &c,
                &mut r,
                vec!["build".into(), "--bin".into(), "driven".into()],
                "targo build --bin driven".into(),
                "the verb matrix",
            );
            assert_eq!(ok, code == 0);
            let block = r.render();
            let lines = compile_lines(&block, "redraw conformance/targo build --bin driven");
            let out = format!(", {} out", aterm_capacity::size(2 << 20));
            assert!(
                lines.len() == 1
                    && lines[0].contains(": 0 fresh / 1 compiled, ")
                    && lines[0].ends_with(&out),
                "{block}"
            );
            assert!(!block.contains("{\"reason\""), "{block}");
            let outcomes: Vec<crate::Outcome> = r.outcomes().map(|(o, _)| o).collect();
            if code == 0 {
                assert!(outcomes.is_empty(), "{block}");
            } else {
                assert_eq!(
                    outcomes,
                    [
                        crate::Outcome::Fail(crate::Severity::GateFailed),
                        crate::Outcome::Fail(crate::Severity::CouldNotRun)
                    ],
                    "{block}"
                );
                assert!(block.contains("printed for a person"), "{block}");
            }
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    fn fixture_gated() -> Vec<(String, String)> {
        let root = crate::mktemp_dir("atv-gated").expect("mktemp");
        for (dir, manifest) in [
            (
                "gui",
                "[package]\nname = \"aterm-gui\"\n\n[[bin]]\nname = \"harness\"\n\
                 required-features = [\"control-conformance\"]\n\n[[bench]]\nname = \"b\"\n\
                 required-features = [\"bench-support\"]\n",
            ),
            (
                "scrollback",
                "[package]\nname = \"aterm-scrollback\"\n\n[[bench]]\nname = \"x\"\n\
                 required-features = [\"disk-tier\"]\n\n[[bench]]\nname = \"y\"\n\
                 required-features = [\"disk-tier\"]\n",
            ),
            ("core", "[package]\nname = \"aterm-core\"\n"),
        ] {
            std::fs::create_dir_all(root.join("crates").join(dir)).expect("mkdir");
            std::fs::write(root.join("crates").join(dir).join("Cargo.toml"), manifest)
                .expect("write");
        }
        std::fs::create_dir_all(root.join("crates/no-manifest")).expect("mkdir");
        let gated = gated_lint_features(&root);
        std::fs::remove_dir_all(&root).ok();
        gated
    }

    #[test]
    fn a_measuring_plan_builds_its_benches_once_and_before_the_quiet_machine() {
        use crate::checks::{Check, RELEASE};
        use crate::plan::{folds_measuring, named_skip};
        let benches = measuring_bench_args();
        assert_eq!(benches.len(), 2 * MEASURING_TARGETS.len());
        let app = || Some(crate::MeasureArtifact::unnamed("/release/aterm.app"));
        let narrowed = || Scope::packages(&["aterm-verify".to_string()]);
        let contexts = [
            ctx(Scope::workspace())
                .with_checks(&RELEASE)
                .with_release(true)
                .with_measure_artifact(app()),
            ctx(Scope::workspace())
                .with_checks(&RELEASE)
                .with_release(true),
            ctx(Scope::workspace())
                .with_checks(&[Check::Test])
                .with_checkers_in_tests(true)
                .with_compile_only(true),
            ctx(narrowed())
                .with_checks(&[Check::Test])
                .with_checkers_in_tests(true),
            ctx(Scope::workspace())
                .with_checks(&[Check::Test, Check::Measure])
                .with_measure_artifact(app()),
            ctx(narrowed())
                .with_checks(&[Check::Test, Check::Measure])
                .with_measure_artifact(app()),
            ctx(Scope::workspace())
                .with_checks(&[Check::Measure])
                .with_measure_artifact(app()),
            ctx(Scope::workspace()).with_checks(&[Check::Test]),
        ];
        for c in &contexts {
            let ids: Vec<StageId> = crate::plan::plan(c).iter().map(|s| s.id).collect();
            let [compile, run] = test_cmds(c, false);
            let (compile, run) = (compile.argv(), run.argv());
            let folded = compile.iter().any(|a| a == "--bench");
            assert_eq!(folded, folds_measuring(c), "{:?}: {compile:?}", c.checks);
            if folded {
                assert!(
                    compile
                        .windows(benches.len())
                        .any(|w| w == benches.as_slice())
                );
                assert!(compile.iter().any(|a| a == "--tests"), "{compile:?}");
            }
            assert!(
                !run.iter().any(|a| a == "--bench"),
                "the run child runs no bench: {run:?}"
            );
            let prebuilt = ids.contains(&StageId::MeasurePrebuild);
            let compiled = folded && ids.contains(&StageId::TestCompile);
            assert!(!(prebuilt && compiled), "built twice: {:?}", c.checks);
            if ids.contains(&StageId::MeasuringTests) && !named_skip(c, StageId::MeasuringTests) {
                assert!(
                    prebuilt || compiled,
                    "the quiet stage would compile its benches: {:?}",
                    c.checks
                );
            }
        }
    }

    #[test]
    fn the_measuring_stage_does_not_run_behind_the_failed_compile_that_builds_its_benches() {
        use crate::checks::{Check, RELEASE};
        let behind = "the test compile that builds its benches failed";
        let outcome = |c: Ctx| {
            let mut c = c
                .with_measure_artifact(Some(crate::MeasureArtifact::unnamed("/release/aterm.app")));
            c.tools.targo = PathBuf::from("/usr/bin/false");
            c.tools.refused = None;
            c.test_compile_failed
                .store(true, std::sync::atomic::Ordering::Release);
            let mut r = Report::new("measuring");
            measuring_tests(&c, &mut r);
            r.outcomes()
                .next()
                .map(|(o, l)| (o, l.to_string()))
                .expect("a decision")
        };
        let release = ctx(Scope::workspace())
            .with_checks(&RELEASE)
            .with_release(true);
        let (o, label) = outcome(release);
        assert_eq!(
            o,
            crate::Outcome::Fail(crate::Severity::CouldNotRun),
            "{label}"
        );
        assert!(
            label.contains(behind) && label.ends_with(NOT_RUN_BEHIND),
            "{label}"
        );
        // NEGATIVE CONTROL: a run whose prebuild row built the benches measures whatever its
        // test compile did.
        let dev = ctx(Scope::workspace()).with_checks(&[Check::Test, Check::Measure]);
        let (_, label) = outcome(dev);
        assert!(!label.contains(behind), "{label}");
    }

    #[test]
    fn the_measuring_stage_hands_paint_and_spin_the_bundle_it_was_handed() {
        let c = ctx(Scope::workspace());
        let artifact = crate::MeasureArtifact::unnamed("/release/aterm.app");
        let env_of = |cmd: &Cmd, key: &str| {
            cmd.envs
                .iter()
                .find(|(k, _)| k.to_str() == Some(key))
                .map(|(_, v)| PathBuf::from(v))
        };
        let (_, run) = measuring_cmd(&c, &artifact, measuring_args());
        let (_, prebuild) = measuring_targo(&c, measure_prebuild_args());
        for var in ["ATERM_PAINT_BIN", "ATERM_SPIN_BIN"] {
            assert_eq!(
                env_of(&run, var),
                Some(PathBuf::from("/release/aterm.app/Contents/MacOS/aterm")),
                "{var}"
            );
            assert_eq!(
                env_of(&prebuild, var),
                None,
                "the compile needs no bundle: {var}"
            );
        }
        assert_eq!(
            env_of(&run, "ATERM_MEASURE_NO_WINDOWS"),
            None,
            "windows by default"
        );
        let quiet = ctx(Scope::workspace()).with_no_windows(true);
        let (_, quiet_run) = measuring_cmd(&quiet, &artifact, measuring_args());
        assert_eq!(
            env_of(&quiet_run, "ATERM_MEASURE_NO_WINDOWS"),
            Some(PathBuf::from("1")),
            "--no-windows leaves the window rows out"
        );
        assert!(
            !run.argv().iter().any(|a| a == "build"),
            "the stage runs the suites and builds nothing: {:?}",
            run.argv()
        );
        let (pre, rows) = (prebuild.argv(), run.argv());
        assert_eq!(pre[..pre.len() - 1], rows, "{pre:?} vs {rows:?}");
        assert!(rows.windows(2).any(|w| w == MAIN_PROFILE));
        assert!(!rows.iter().any(|a| a == "--ignored" || a == "measuring::"));
        assert_eq!(pre.last().map(String::as_str), Some("--no-run"));
        let mut c = ctx(Scope::workspace());
        c.tools.targo = PathBuf::from("/usr/bin/false");
        c.tools.refused = None;
        let mut r = Report::new("measuring");
        measuring_tests(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(outcome, crate::Outcome::Skip, "{label}");
    }

    #[test]
    fn a_glue_row_without_its_browser_could_not_run() {
        use crate::wasm_page::Refused;
        let c = ctx(Scope::workspace());
        let row = |run: crate::perf::LaneRun| {
            let mut r = Report::new("wasm-glue");
            glue_row(&c, &mut r, "wasm-glue (the toy)", "the toy", move |_| run);
            r.outcomes()
                .next()
                .map(|(o, l)| (o, l.to_string()))
                .expect("a row")
        };
        let (outcome, label) = row(Refused::Unavailable(
            "no pinned chrome-headless-shell placed".into(),
        )
        .verdict("the toy's driver"));
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun), "{label}");
        assert_eq!(label.matches("fix:").count(), 1, "{label}");
        assert!(
            label.contains("no pinned chrome-headless-shell placed")
                && label.contains("verify provision"),
            "{label}"
        );
        let (outcome, label) = row(crate::perf::LaneRun::skip(
            "no stock pin",
            "targo --unverified verify provision",
        ));
        assert_eq!(outcome, Outcome::Skip, "{label}");
        let (outcome, label) =
            row(Refused::Failed("drive.mjs threw".into()).verdict("the toy's driver"));
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed), "{label}");
    }

    #[test]
    fn libc_oracle_reads_the_gates_verdict_before_the_machine() {
        let run = |code: Option<i32>, output: &str| exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: code == Some(0),
            output: output.to_string(),
            code,
            spawn_error: None,
            usage: None,
        };
        let full = exec::ENOSPC_SENTENCE;
        for (code, output, want) in [
            (Some(0), "", Outcome::Ok),
            (Some(1), "", Outcome::Fail(Severity::GateFailed)),
            (Some(1), full, Outcome::Fail(Severity::GateFailed)),
            (Some(3), "", Outcome::Fail(Severity::CouldNotRun)),
            (None, "", Outcome::Fail(Severity::CouldNotRun)),
            (Some(101), "", Outcome::Fail(Severity::GateFailed)),
            (Some(101), full, Outcome::Fail(Severity::CouldNotRun)),
            (Some(2), "", Outcome::Fail(Severity::GateFailed)),
        ] {
            assert_eq!(
                libc_oracle_outcome(&run(code, output)),
                want,
                "{code:?} {output:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_machine_with_no_doc_driver_anywhere_is_diagnosed_on_the_doctests_row() {
        let spec = |id| StageSpec {
            id,
            title: "t".into(),
            lane: crate::plan::Lane::MainTarget,
            exclusive: false,
            after_stages: Vec::new(),
            mem: 0,
            part: None,
        };
        let tmp = crate::mktemp_dir("atv-doc-scope").expect("mktemp");
        let documented = tmp.join("crates/documented");
        std::fs::create_dir_all(documented.join("src")).unwrap();
        std::fs::write(
            documented.join("Cargo.toml"),
            "[package]\nname = \"documented\"\n",
        )
        .unwrap();
        std::fs::write(
            documented.join("src/lib.rs"),
            "/// ```\n/// assert!(true);\n/// ```\npub fn f() {}\n",
        )
        .unwrap();
        crate::standin_body(
            &tmp.join("trustc"),
            "printf 'rustc 1.93.0 (trust)\\nbinary: rustc\\nhost: aarch64-apple-darwin\\nrelease: \
             1\\n'",
        );
        let ok_targo = {
            let p = tmp.join("targo");
            crate::standin_body(
                &p,
                "case \"$*\" in\n  *config*get*build.target*)\n    echo 'error: config value \
                 `build.target` is not set' >&2\n    exit 101 ;;\nesac\nexit 0",
            );
            p
        };
        let no_driver = |scope| {
            let mut c = ctx(scope);
            c.tools.stage2_dir = tmp.clone();
            c.tools.targo = ok_targo.clone();
            c.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
            c.path_env = "/nonexistent-dir".into();
            c.root = tmp.clone();
            c.scratch = std::env::temp_dir();
            c
        };
        let cc = no_driver(Scope::workspace());

        let r = run_stage(&cc, &spec(StageId::Test));
        let outcomes: Vec<Outcome> = r.outcomes().map(|(o, _)| o).collect();
        assert_eq!(outcomes, [Outcome::Ok], "{}", r.render());

        let r = run_stage(&cc, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Fail(Severity::CouldNotRun));

        let cb = no_driver(Scope::changed("main", vec!["aterm-lab".into()], false));
        let r = run_stage(&cb, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "bin-only doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip, "{}", outcomes[0].1);

        let bin_only = tmp.join("crates/binonly");
        std::fs::create_dir_all(bin_only.join("src")).unwrap();
        std::fs::write(
            bin_only.join("Cargo.toml"),
            "[package]\nname = \"binonly\"\n",
        )
        .unwrap();
        let mut cx = no_driver(Scope::crate_only("binonly"));
        cx.root = tmp.clone();
        let r = run_stage(&cx, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes[0].0, Outcome::Skip, "{}", outcomes[0].1);

        std::fs::write(
            bin_only.join("src/lib.rs"),
            "//! ```text\n//! not rust\n//! ```\n",
        )
        .unwrap();
        let r = run_stage(&cx, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes[0].0, Outcome::Ok, "{}", outcomes[0].1);
        std::fs::write(bin_only.join("src/lib.rs"), "//! ```\n//! 1;\n//! ```\n").unwrap();
        let r = run_stage(&cx, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(
            outcomes[0].0,
            Outcome::Fail(Severity::CouldNotRun),
            "{}",
            outcomes[0].1
        );
        let r = run_stage(&cx, &spec(StageId::Test));
        assert!(r.outcomes().all(|(o, _)| o == Outcome::Ok));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_doc_driver_rule_pins_stage2_first_and_diagnoses_absence() {
        assert_eq!(doc_driver_from(true, true, true), DocDriver::Stage2);
        assert_eq!(doc_driver_from(true, false, false), DocDriver::Stage2);
        assert_eq!(doc_driver_from(false, true, true), DocDriver::Ambient);
        assert_eq!(doc_driver_from(false, true, false), DocDriver::Ambient);
        assert_eq!(doc_driver_from(false, false, true), DocDriver::BarePath);
        assert_eq!(doc_driver_from(false, false, false), DocDriver::Absent);
    }

    #[test]
    fn the_doctests_select_exactly_what_the_test_compile_selects() {
        fn selection(argv: &[String]) -> Vec<String> {
            let mut v = Vec::new();
            let mut it = argv.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "-p" => v.push(format!("-p {}", it.next().expect("a -p value"))),
                    "--workspace" => v.push(a.clone()),
                    _ => {}
                }
            }
            v
        }
        for scope in [
            Scope::workspace(),
            Scope::crate_only("aterm-grid"),
            Scope::packages(&["aterm-core".into(), "aterm-types".into()]),
            Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true),
        ] {
            let doc = doctest_args(&scope);
            assert!(!selection(&doc).is_empty(), "{doc:?}");
            assert_eq!(selection(&doc), selection(&test_compile_args(&scope)));
            let mut c = ctx(scope);
            c.tools.trustdoc = PathBuf::from("/stage2/trustdoc");
            for skip in [&[][..], &["plain".to_string()][..]] {
                let cmd = doctest_cmd(&c, &DocDriver::Stage2, skip).expect("an exact driver");
                let unified = aterm_toolchain::build_cache::with_test_graph(&doc);
                assert!(
                    cmd.args.ends_with(
                        &unified
                            .iter()
                            .map(std::ffi::OsString::from)
                            .collect::<Vec<_>>()
                    ),
                    "{:?}",
                    cmd.args
                );
            }
        }
    }

    #[test]
    fn the_doctests_split_the_scopes_libraries_by_what_their_docs_hold() {
        let tmp = crate::mktemp_dir("atv-doc-split").expect("mktemp");
        for (name, file, text) in [
            (
                "documented",
                "lib.rs",
                "/// ```\n/// assert!(true);\n/// ```\npub fn f() {}\n",
            ),
            ("plain", "lib.rs", "//! ```text\n//! not rust\n//! ```\n"),
            (
                "binonly",
                "main.rs",
                "/// ```\n/// 1;\n/// ```\nfn main() {}\n",
            ),
        ] {
            let dir = tmp.join("crates").join(name);
            std::fs::create_dir_all(dir.join("src")).expect("mkdir");
            std::fs::write(
                dir.join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n"),
            )
            .expect("manifest");
            std::fs::write(dir.join("src").join(file), text).expect("source");
        }
        let split = |scope, root: &std::path::Path| {
            let mut c = ctx(scope);
            c.root = root.to_path_buf();
            doctest_split(&c)
        };
        let names = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            split(Scope::workspace(), &tmp),
            Some(DocSplit {
                run: names(&["documented"]),
                skip: names(&["plain"]),
            })
        );
        assert_eq!(
            split(Scope::packages(&names(&["plain", "ghost"])), &tmp),
            Some(DocSplit {
                run: names(&["ghost"]),
                skip: names(&["plain"]),
            })
        );
        assert_eq!(
            split(Scope::crate_only("plain"), &tmp),
            Some(DocSplit {
                run: Vec::new(),
                skip: names(&["plain"]),
            })
        );
        assert_eq!(split(Scope::workspace(), &tmp.join("no-members")), None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    fn env_of(cmd: &Cmd, k: &str) -> Option<std::ffi::OsString> {
        cmd.envs
            .iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
    }

    #[cfg(unix)]
    #[test]
    fn checker_policy_is_explicit_and_every_other_child_scrubs_it() {
        const PROBE: &str = "ATERM_SPEC_POLICY_PROBE";
        let temp = aterm_tempfile::tempdir().unwrap();
        let mut c = ctx(Scope::workspace());
        c.root = temp.path().to_owned();
        c.scratch = temp.path().join("scratch");
        std::fs::create_dir_all(&c.scratch).unwrap();
        let trustdoc = temp.path().join("stage2/trustdoc");
        crate::standin_body(&trustdoc, "exit 0");
        c.tools.trustdoc = trustdoc.clone();
        if std::env::var_os(PROBE).is_some() {
            for key in [
                aterm_capacity::RELEASE_RUN_ENV,
                "ATERM_RELEASE_INPUTS_SEAL",
                "ATERM_RELEASE_CUTTER",
            ] {
                let out = exec::run(&Cmd::new("/usr/bin/printenv").arg(key), c.exec_env());
                assert_eq!(out.code, Some(1), "inherited {key}: {}", out.output);
            }
            let out = exec::run(
                &Cmd::new("/usr/bin/printenv").arg(CHECKERS_ENV),
                c.exec_env(),
            );
            assert_eq!(
                out.code,
                Some(1),
                "an inherited checker policy leaked: {}",
                out.output
            );
            for required in [false, true] {
                let out = exec::run(
                    &with_checkers(Cmd::new("/usr/bin/printenv").arg(CHECKERS_ENV), required),
                    c.exec_env(),
                );
                assert!(out.ok, "{}", out.output);
                assert_eq!(out.output.trim(), if required { "1" } else { "0" });
            }
            return;
        }
        let child = Cmd::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "stages::tests::checker_policy_is_explicit_and_every_other_child_scrubs_it",
            ])
            .env(PROBE, "1")
            .env(aterm_capacity::RELEASE_RUN_ENV, "1")
            .env("ATERM_RELEASE_INPUTS_SEAL", "/owner/seal")
            .env("ATERM_RELEASE_CUTTER", "/owner/cutter")
            .env(CHECKERS_ENV, "1");
        let out = exec::run(&child, c.exec_env());
        assert!(out.ok && out.output.contains("1 passed"), "{}", out.output);

        let mut release = ctx(Scope::workspace()).with_release(true);
        release.tools.trustdoc = trustdoc.clone();
        let mut as_release = ctx(Scope::workspace()).with_checkers_in_tests(true);
        as_release.tools.trustdoc = trustdoc;
        for context in [&c, &release, &as_release] {
            let [compile, run] = test_cmds(context, false);
            let shaped_compile = test_compile_cmd(
                context,
                false,
                vec![
                    "--unverified".into(),
                    "test".into(),
                    "-p".into(),
                    "x".into(),
                ],
            );
            assert_eq!(env_of(&compile, CHECKERS_ENV), None);
            let want = Some(if context.checkers_in_tests { "1" } else { "0" }.into());
            assert_eq!(env_of(&run, CHECKERS_ENV), want);
            let marker = context.checkers_in_tests.then(|| "1".into());
            assert_eq!(env_of(&compile, aterm_capacity::RELEASE_RUN_ENV), marker);
            assert_eq!(
                env_of(&shaped_compile, aterm_capacity::RELEASE_RUN_ENV),
                marker
            );
            assert_eq!(env_of(&run, aterm_capacity::RELEASE_RUN_ENV), marker);
            assert_eq!(compile.exact_env, context.checkers_in_tests);
            assert_eq!(shaped_compile.exact_env, context.checkers_in_tests);
            assert_eq!(run.exact_env, context.checkers_in_tests);
            assert_eq!(
                env_of(
                    &doctest_cmd(context, &DocDriver::Stage2, &[]).expect("the stage2 doc driver"),
                    aterm_capacity::RELEASE_RUN_ENV
                ),
                marker
            );
            assert_eq!(
                env_of(
                    &doctest_cmd(context, &DocDriver::Stage2, &[]).expect("the stage2 doc driver"),
                    CHECKERS_ENV
                ),
                want
            );
            assert_eq!(run.args, test_cmds(&c, false)[1].args);
        }
        let checker = spec_checkers_cmd(&c);
        assert_eq!(env_of(&checker, CHECKERS_ENV), Some("1".into()));
        assert_eq!(checker.args, test_cmds(&c, false)[1].args);
    }

    /// RELEASE TEST PLANNING AND COMPILATION SEE THE PREPARED DISPOSABLE HOME: the unit-graph
    /// resolver and the actual compile each start only after the fixed release history exists in
    /// the exact HOME they receive. NEGATIVE CONTROL: removing that HOME between the two calls
    /// requires the compile path to prepare it again.
    #[cfg(unix)]
    #[test]
    fn release_test_planning_and_compilation_prepare_their_declared_home() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        let home = scratch.join("release-test-home");
        let record = home.join(".local/state/aterm-release/disk-footprint.json");
        let targo = temp.path().join("bin/targo");
        crate::standin_body(
            &targo,
            &format!(
                "case \" $* \" in *' build '*) exit 0;; esac\n\
                 test \"$HOME\" = '{}' || exit 17\n\
                 test \"$ATERM_RELEASE_RUN\" = 1 || exit 18\n\
                 case \"$*\" in *--unit-graph*) echo '{{\"version\":1}}';; esac",
                home.display()
            ),
        );
        let mut c = Ctx::new(
            temp.path().to_owned(),
            Scope::workspace(),
            EnvSnapshot::default(),
            scratch,
        )
        .with_checkers_in_tests(true);
        c.tools.targo = targo;
        assert!(!record.exists());

        let piece = crate::shape::Piece::of(&["aterm-verify".to_string()], false);
        assert!(unit_graph(&c, &piece).is_ok());
        assert!(record.exists());

        std::fs::remove_dir_all(&home).unwrap();
        let spec = crate::plan::declared(&c.scope).remove(0);
        let mut report = Report::new(spec.title.clone());
        test_compile(&c, &spec, &mut report);
        assert!(
            !c.test_compile_failed
                .load(std::sync::atomic::Ordering::Acquire),
            "{}",
            report.render()
        );
        assert!(record.exists());
    }

    #[test]
    fn the_stand_in_drives_rustdoc_only_where_the_scan_cleared_a_library() {
        let (driver, skip_env) = (crate::docdriver::DRIVER_ENV, crate::docdriver::SKIP_ENV);
        let stage2 = std::path::absolute("/stage2/trustdoc").expect("absolute");
        let caller = std::path::absolute("/caller/rustdoc").expect("absolute");
        let recorder = std::path::absolute("/bin/aterm-verify").expect("absolute");
        let mut c = ctx(Scope::workspace());
        c.tools.trustdoc = stage2.clone();
        c.test_recorder = Some(recorder.clone());
        c.env.rustdoc_override = Some(caller.clone().into_os_string());
        c = c.with_checkers_in_tests(true);
        let skip = ["other".to_string(), "plain".to_string()];
        for (kind, real) in [(DocDriver::Stage2, &stage2), (DocDriver::Ambient, &caller)] {
            let cmd = doctest_cmd(&c, &kind, &skip).expect("an exact driver");
            assert_eq!(
                env_of(&cmd, "RUSTDOC"),
                Some(recorder.clone().into_os_string()),
                "{kind:?}"
            );
            assert_eq!(
                env_of(&cmd, driver),
                Some(real.clone().into_os_string()),
                "{kind:?}"
            );
            assert_eq!(
                env_of(&cmd, skip_env),
                Some("other,plain".into()),
                "{kind:?}"
            );
            let cmd = doctest_cmd(&c, &kind, &[]).expect("an exact driver");
            assert_eq!(
                env_of(&cmd, driver),
                Some(real.clone().into_os_string()),
                "{kind:?}"
            );
            assert_eq!(env_of(&cmd, skip_env), None, "{kind:?}");
            assert_eq!(
                env_of(&cmd, "RUSTDOC"),
                Some(recorder.clone().into_os_string()),
                "{kind:?}"
            );
            assert_eq!(
                env_of(&cmd, crate::docdriver::RUNTOOL_ENV),
                Some(recorder.clone().into_os_string()),
                "{kind:?}"
            );
            for inherited in [
                crate::docdriver::DRIVER_ENV,
                crate::docdriver::SKIP_ENV,
                crate::docdriver::RUNTOOL_ENV,
                crate::docdriver::RUNTOOL_SCRATCH_ENV,
                crate::docdriver::RUNTOOL_SAMPLES_ENV,
                crate::docdriver::RUNTOOL_HALT_ENV,
            ] {
                assert!(
                    cmd.remove_envs.iter().any(|name| name == inherited),
                    "{kind:?} did not scrub inherited {inherited}"
                );
            }
        }
        let compile = test_compile_cmd(&c, false, test_compile_args(&c.scope));
        assert!(compile.exact_env);
        assert_eq!(env_of(&compile, "RUSTDOC"), Some(caller.into_os_string()));
    }

    /// THE STAND-IN RUNS THE FILE CARGO WOULD HAVE RUN, named absolutely. With
    /// no override cargo takes the plain `trustdoc` beside the targo it runs,
    /// the file a link to it resolves to, before PATH; a bare name the stand-in
    /// looked up on its own PATH could be another file.
    #[cfg(unix)]
    #[test]
    fn the_stand_in_is_told_the_driver_cargo_resolves() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let tmp = std::fs::canonicalize(crate::mktemp_dir("atv-doc-driver").expect("mktemp"))
            .expect("real");
        let file = |p: &std::path::Path| {
            std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
            std::fs::write(p, "").expect("write");
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        };
        let (build, links, path) = (tmp.join("build"), tmp.join("links"), tmp.join("path"));
        for f in [
            build.join("targo"),
            build.join("trustdoc"),
            path.join("trustdoc"),
        ] {
            file(&f);
        }
        file(&tmp.join("elsewhere/trustdoc"));
        std::fs::create_dir_all(&links).expect("mkdir");
        symlink(build.join("targo"), links.join("targo")).expect("symlink");
        let mut c = ctx(Scope::workspace());
        c.tools.targo = links.join("targo");
        c.tools.trustdoc = links.join("trustdoc");
        c.path_env = std::env::join_paths([&links, &path]).expect("join");
        c.test_recorder = Some(PathBuf::from("/bin/aterm-verify"));
        let resolved = |c: &Ctx, kind| cargo_doc_driver(c, &kind);
        assert_eq!(
            resolved(&c, DocDriver::BarePath),
            Some(build.join("trustdoc"))
        );
        let cmd = doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()])
            .expect("the resolved driver");
        assert_eq!(env_of(&cmd, "RUSTDOC"), Some("/bin/aterm-verify".into()));
        assert_eq!(
            env_of(&cmd, crate::docdriver::DRIVER_ENV),
            Some(build.join("trustdoc").into_os_string())
        );
        c.env.rustdoc_override = Some("".into());
        assert_eq!(
            resolved(&c, DocDriver::Ambient),
            Some(build.join("trustdoc"))
        );
        for (value, want) in [
            ("/caller/rustdoc", Some(PathBuf::from("/caller/rustdoc"))),
            ("tools/rustdoc", Some(PathBuf::from("/repo/tools/rustdoc"))),
            ("trustdoc", Some(path.join("trustdoc"))),
            ("rustdoc", None),
            (
                "tools\\rustdoc",
                Some(PathBuf::from("/repo/tools\\rustdoc")),
            ),
        ] {
            c.env.rustdoc_override = Some(value.into());
            assert_eq!(resolved(&c, DocDriver::Ambient), want, "{value}");
        }
        let root = tmp.join("repo");
        std::fs::create_dir_all(root.join(".cargo")).expect("mkdir");
        let repo = std::mem::replace(&mut c.root, root.clone());
        for (config, named) in [
            (
                "[build]\nrustdocflags = ['--cfg', 'x']\n# rustdoc = 'no'\n",
                true,
            ),
            ("[build]\nrustdoc = '/config/rustdoc'\n", false),
        ] {
            std::fs::write(root.join(".cargo/config.toml"), config).expect("config");
            c.env.rustdoc_override = None;
            assert_eq!(
                resolved(&c, DocDriver::BarePath).is_some(),
                named,
                "{config}"
            );
            c.env.rustdoc_override = Some("".into());
            assert_eq!(
                resolved(&c, DocDriver::Ambient).is_some(),
                named,
                "{config}"
            );
        }
        c.env.rustdoc_override = Some("/caller/rustdoc".into());
        assert_eq!(
            resolved(&c, DocDriver::Ambient),
            Some(PathBuf::from("/caller/rustdoc"))
        );
        assert!(
            doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()]).is_err(),
            "a configured driver verify cannot resolve is refused"
        );
        c.root = repo;
        c.env.rustdoc_override = Some("".into());
        std::fs::remove_file(build.join("trustdoc")).expect("rm");
        symlink(tmp.join("elsewhere/trustdoc"), build.join("trustdoc")).expect("symlink");
        assert_eq!(
            resolved(&c, DocDriver::BarePath),
            Some(path.join("trustdoc"))
        );
        c.path_env = links.into_os_string();
        assert_eq!(resolved(&c, DocDriver::BarePath), None);
        assert!(
            doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()]).is_err(),
            "an unresolved driver is refused rather than run unmonitored"
        );
        assert_eq!(resolved(&c, DocDriver::Absent), None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_config_names_a_doc_driver_only_by_its_key() {
        for line in [
            "rustdoc = '/x/rustdoc'",
            "rustdoc=\"trustdoc\"",
            "build.rustdoc = 'r'",
            "\"rustdoc\" = 'r'",
            "build = { jobs = 4, rustdoc = 'r' }",
        ] {
            assert!(names_rustdoc(line), "{line}");
        }
        for line in [
            "rustdocflags = ['--cfg', 'docsrs']",
            "my-rustdoc = 'r'",
            "rustc = 'rustdoc'",
            "[build]",
        ] {
            assert!(!names_rustdoc(line), "{line}");
        }
    }

    /// A narrow test build on the main lane resolves the workspace's features, so it builds only
    /// units the workspace build did, by the real `targo`'s unit graphs over a scratch workspace
    /// where one member alone turns a shared path dependency's feature on; and the words leave
    /// the workspace graph as it was. NEGATIVE CONTROL: the same narrow call without them holds
    /// a unit the workspace build never makes (the dependency without its feature).
    #[test]
    fn a_narrow_main_lane_test_build_builds_only_what_the_workspace_build_did() {
        let directory = aterm_tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver='2'\nmembers=['wide','narrow','shared']\n",
        )
        .unwrap();
        for (name, extra) in [
            ("shared", "[features]\nextra=[]\n"),
            (
                "wide",
                "[dependencies]\nshared={path='../shared',features=['extra']}\n",
            ),
            ("narrow", "[dependencies]\nshared={path='../shared'}\n"),
        ] {
            let package = root.join(name);
            std::fs::create_dir_all(package.join("src")).unwrap();
            std::fs::write(
                package.join("Cargo.toml"),
                format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{extra}"),
            )
            .unwrap();
            std::fs::write(package.join("src/lib.rs"), "").unwrap();
        }
        let mut c = ctx(Scope::crate_only("narrow"));
        c.root = root.to_owned();
        c.scratch = root.join("scratch");
        std::fs::create_dir_all(&c.scratch).unwrap();
        c.tools.targo = PathBuf::from(env!("CARGO"));
        c.main_build_dir = None;
        c.child_ceiling = Some(std::time::Duration::from_secs(60));
        let mut interner = crate::unitgraph::Interner::default();
        let mut graph = |cmd: Cmd| {
            let cmd = cmd
                .args(UNIT_GRAPH)
                .arg("--offline")
                .env("CARGO_HOME", root.join("cargo-home"));
            let out = exec::run(&cmd, c.exec_env());
            assert!(out.ok, "{:?}: {}", cmd.argv(), out.output);
            let text = out
                .output
                .lines()
                .find(|l| l.starts_with("{\"version\":1"))
                .unwrap_or_else(|| panic!("no unit graph: {}", out.output));
            crate::unitgraph::Graph::read(text, &mut interner).unwrap()
        };
        let call = |selection: &[&str]| {
            ["--unverified", "test"]
                .iter()
                .chain(selection)
                .map(|w| (*w).to_owned())
                .collect::<Vec<_>>()
        };
        let wide = graph(main_targo(&c, call(&["--workspace"])));
        let narrow = graph(main_targo(&c, call(&["-p", "narrow"])));
        assert!(narrow.within(&wide), "{:?}", narrow.names());
        assert_eq!(
            graph(targo(&c, call(&["--workspace"]))).ids(),
            wide.ids(),
            "the words change nothing of the workspace build"
        );
        let plain = graph(targo(&c, call(&["-p", "narrow"])));
        assert!(!plain.within(&wide), "the control: {:?}", plain.names());
    }

    /// The real checkout's dev build of `aterm`, as the shim's rule places it, resolves exactly
    /// the unit graph the typed build does (TARGO-MEMBER-FEATURE-SCOPE-1), and that graph's
    /// dependency units are all in the typed `targo test -p aterm`'s: one profile, and one
    /// dependency shape in the manifests (B21), with no words chosen for it. CONTROLS: the
    /// workspace's feature unification around the build (verify's own lanes' words) and a test
    /// profile with debuginfo each build units the test graph lacks. A newly feature-flipped dev
    /// dependency fails the unmutated comparison.
    #[test]
    fn dev_build_reuses_the_typed_test_builds_dependencies() {
        use aterm_toolchain::build_cache::{
            Machine, Tool, read_config, with_build_dir, with_test_graph,
        };

        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = aterm_tempfile::manifest_dir()
            .join("../..")
            .canonicalize()
            .unwrap();
        let mut c = ctx(Scope::crate_only("aterm"));
        c.root = root.clone();
        c.scratch = scratch.path().to_owned();
        c.tools.targo = PathBuf::from(env!("CARGO"));
        c.main_build_dir = None;
        c.child_ceiling = Some(std::time::Duration::from_secs(60));
        let machine = Machine {
            root: None,
            home: root.parent(),
            cwd: Some(&root),
            env: &|_| None,
            read: &read_config,
            writable: &|_| Some(true),
        };
        let mut interner = crate::unitgraph::Interner::default();
        let mut graph = |args: Vec<String>| {
            // A cached compile need not retain registry archives. Keep resolution
            // pinned while Cargo obtains any sources needed to inspect the graph.
            let out = exec::run(
                &targo(&c, args).args(UNIT_GRAPH).arg("--locked"),
                c.exec_env(),
            );
            assert!(out.ok, "{}", out.output);
            let text = out
                .output
                .lines()
                .find(|l| l.starts_with("{\"version\":1"))
                .unwrap();
            let mut value: aterm_json::Value = aterm_json::from_str(text).unwrap();
            // The label is not a compiler input; every effective setting stays in the key.
            for unit in value
                .as_object_mut()
                .unwrap()
                .get_mut("units")
                .unwrap()
                .as_array_mut()
                .unwrap()
            {
                unit.as_object_mut()
                    .unwrap()
                    .get_mut("profile")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("name");
            }
            crate::unitgraph::Graph::read(&value.to_string(), &mut interner).unwrap()
        };
        let build = ["--unverified", "build", "-p", "aterm"]
            .map(str::to_owned)
            .to_vec();
        let test = ["--unverified", "test", "-p", "aterm"]
            .map(str::to_owned)
            .to_vec();
        let tested = graph(test.clone());
        let dev = graph(with_build_dir(&build, Tool::Targo, &machine).unwrap());
        assert_eq!(
            dev.ids(),
            graph(build.clone()).ids(),
            "placement changed the graph"
        );
        let shared = tested.ids();
        let missing: Vec<_> = dev
            .units
            .iter()
            .filter(|u| !shared.contains(&u.id) && !u.target_kinds.iter().any(|k| k == "bin"))
            .collect();
        assert!(missing.is_empty(), "dev/test dependency drift: {missing:?}");
        assert!(
            !graph(with_test_graph(&build)).within(&tested),
            "feature control"
        );
        let mut changed = test;
        changed.splice(0..0, ["--config".into(), "profile.test.debug=true".into()]);
        assert!(!dev.within(&graph(changed)), "profile control");
    }

    #[test]
    fn every_argv_reaches_its_whole_selection() {
        let has = |argv: &[String], w: &str| argv.iter().any(|a| a == w);
        for scope in [
            Scope::workspace(),
            Scope::crate_only("aterm-grid"),
            Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true),
        ] {
            for argv in [
                test_compile_args(&scope),
                test_run_args(&scope),
                doctest_args(&scope),
            ] {
                assert!(has(&argv, "--no-fail-fast"), "{argv:?}");
                for c in scope.crates() {
                    assert!(
                        argv.windows(2).any(|w| w[0] == "-p" && w[1] == *c),
                        "{c}: {argv:?}"
                    );
                }
            }
            let lint = tippy_args(&scope);
            let sep = lint.iter().position(|x| x == "--").expect("a -- separator");
            assert!(has(&lint[..sep], "--keep-going") && has(&lint[..sep], "--all-targets"));
            assert!(
                lint[sep..]
                    .windows(2)
                    .any(|w| w[0] == "-D" && w[1] == "warnings")
            );
            for c in scope.crates() {
                assert!(lint[..sep].contains(c), "{c}: {lint:?}");
            }
        }
        for argv in [measuring_args(), measure_prebuild_args()] {
            assert!(has(&argv, "--no-fail-fast"), "{argv:?}");
        }
        let g = tippy_gated_args(&Scope::workspace(), &fixture_gated()).expect("gated");
        let sep = g.iter().position(|x| x == "--").expect("a -- separator");
        assert!(
            has(&g[..sep], "--keep-going") && has(&g[sep..], "warnings"),
            "{g:?}"
        );
    }

    #[test]
    fn a_run_compared_a_frame_only_when_the_golden_test_passed() {
        let run = |ok: bool, tests: &[&str]| {
            let mut output = format!(
                "     Running unittests src/lib.rs (target/debug/deps/aterm_gpu-1)\n\nrunning {} \
                 tests\n",
                tests.len()
            );
            for t in tests {
                output.push_str(&format!("test {t} ... ok\n"));
            }
            exec::Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok,
                output,
                code: Some(i32::from(!ok)),
                spawn_error: None,
                usage: None,
            }
        };
        let compared = "renderer::frame_record_tests::armed_frames_hold_to_their_cpu_twins";
        assert!(!compared_no_frame(&run(true, &[compared])));
        let mut one_thread = run(true, &[]);
        one_thread.output.push_str(&format!(
            "test {compared} ... frame-record: the video take's format is bgra8\nok\n"
        ));
        assert!(
            !compared_no_frame(&one_thread),
            "what one libtest thread prints"
        );
        let mut behind_a_line = run(true, &[]);
        behind_a_line
            .output
            .push_str(&format!("aterm-gpu: test {compared} ... ok\n"));
        assert!(
            !compared_no_frame(&behind_a_line),
            "another test's unfinished stderr line before the name"
        );
        let mut ignored = run(true, &[]);
        ignored
            .output
            .push_str(&format!("test {compared} ... ignored\n"));
        assert!(compared_no_frame(&ignored));
        let mut ignored_behind_a_line = run(true, &[]);
        ignored_behind_a_line
            .output
            .push_str(&format!("aterm-gpu: test {compared} ... ignored\n"));
        assert!(compared_no_frame(&ignored_behind_a_line));
        assert!(compared_no_frame(&run(
            true,
            &[
                "renderer::frame_record_tests::armed_frames_hold_to_their_cpu_twins_on_bundled_faces"
            ]
        )));
        assert!(compared_no_frame(&run(true, &[])));
        assert!(compared_no_frame(&run(
            true,
            &["renderer::frame_record_tests::a_neighbour"]
        )));
        assert!(
            !compared_no_frame(&run(false, &[])),
            "a failed child is the test's verdict"
        );
    }

    #[test]
    fn an_empty_change_selection_compiles_nothing_in_any_of_those_stages() {
        let c = ctx(Scope::changed("main", vec![], true));
        for stage in [
            StageId::TestCompile,
            StageId::Test,
            StageId::Doctests,
            StageId::Tippy,
            StageId::SpecCheckers,
        ] {
            let mut cc = ctx(c.scope.clone());
            cc.tools.targo = PathBuf::from("/bin/sh");
            cc.tools.tippy = Some(PathBuf::from("/bin/sh"));
            let r = run_stage(
                &cc,
                &StageSpec {
                    id: stage,
                    title: "t".into(),
                    lane: crate::plan::Lane::MainTarget,
                    exclusive: false,
                    after_stages: Vec::new(),
                    mem: 0,
                    part: None,
                },
            );
            let outcomes: Vec<_> = r.outcomes().collect();
            assert_eq!(outcomes.len(), 1, "{stage:?} decided once");
            assert_eq!(outcomes[0].0, crate::ladder::Outcome::Skip, "{stage:?}");
            assert!(
                outcomes[0].1.contains(EMPTY_CONE),
                "{stage:?} said: {}",
                outcomes[0].1
            );
        }
    }

    #[test]
    fn a_borrowed_lint_driver_runs_under_its_own_build_and_says_so() {
        use crate::toolchain::TIPPY;
        let env_of = |cmd: &Cmd, key: &str| {
            cmd.envs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        let managed = PathBuf::from("/m/store/trust/9192/bin").join(TIPPY);
        let mut c = ctx(Scope::workspace());
        c.tools.tippy = Some(managed.clone());
        c.tools.borrowed = vec![(TIPPY, managed.clone())];
        let cmd = tippy_cmd(&c, &managed, tippy_args(&c.scope));
        assert_eq!(
            env_of(&cmd, "TRUST_STAGE2_BIN").as_deref(),
            Some("/m/store/trust/9192/bin")
        );
        assert_eq!(
            env_of(&cmd, "PATH")
                .as_deref()
                .and_then(|p| std::env::split_paths(p).next()),
            Some(PathBuf::from("/m/store/trust/9192/bin")),
            "{cmd:?}"
        );
        assert_eq!(
            crate::fmt::borrowed_suffix(&c, TIPPY),
            format!(" (the managed Trust build's {})", managed.display())
        );
        let own = c.tools.stage2_dir.join(TIPPY);
        c.tools.tippy = Some(own.clone());
        c.tools.borrowed.clear();
        let cmd = tippy_cmd(&c, &own, tippy_args(&c.scope));
        assert_eq!(env_of(&cmd, "TRUST_STAGE2_BIN"), None);
        assert_eq!(crate::fmt::borrowed_suffix(&c, TIPPY), "");
    }

    #[test]
    fn every_row_a_borrowed_lint_driver_decides_names_it() {
        use crate::toolchain::TIPPY;
        let root = std::fs::canonicalize(crate::mktemp_dir("atv-lintrows").expect("mktemp"))
            .expect("real");
        std::fs::create_dir_all(root.join("crates/gui")).expect("mkdir");
        std::fs::write(
            root.join("crates/gui/Cargo.toml"),
            "[package]\nname = \"aterm-gui\"\n\n[[bench]]\nname = \"b\"\n\
             required-features = [\"bench-support\"]\n",
        )
        .expect("manifest");
        let managed = root.join("m/bin").join(TIPPY);
        let own = root.join("s2/bin").join(TIPPY);
        let scope = Scope::packages(&["aterm-gpu".to_string(), "aterm-gui".to_string()]);
        let mut c = Ctx::new(root.clone(), scope, EnvSnapshot::default(), root.join("t"))
            .with_release(true);
        c.tools.stage2_dir = own.parent().expect("dir").to_path_buf();
        c.tools.tippy = Some(managed.clone());
        c.tools.borrowed = vec![(TIPPY, managed.clone())];
        let rows = |c: &Ctx| {
            let mut r = Report::new("lint");
            tippy(c, &mut r);
            r.outcomes()
                .map(|(o, l)| (o, l.to_string()))
                .collect::<Vec<_>>()
        };
        let said = format!(" (the managed Trust build's {})", managed.display());
        let borrowed = rows(&c);
        assert_eq!(borrowed.len(), 2, "{borrowed:?}");
        assert!(
            borrowed.iter().all(|(_, l)| l.contains(&said)),
            "{borrowed:?}"
        );
        c.tools.tippy = Some(own);
        c.tools.borrowed.clear();
        let theirs = rows(&c);
        assert_eq!(theirs.len(), 2, "{theirs:?}");
        assert!(
            theirs
                .iter()
                .all(|(_, l)| !l.contains("the managed Trust build's")),
            "{theirs:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn compile_only_children_yield_and_children_that_run_code_do_not() {
        let mut c = ctx(Scope::workspace());
        c.tools.tippy = Some(PathBuf::from("/s2/targo-tippy"));

        for bind in [false, true] {
            let [compile, run] = test_cmds(&c, bind);
            assert!(compile.demoted, "targo test --no-run");
            assert!(!run.demoted, "targo test --tests runs code");
            let artifact = crate::MeasureArtifact::unnamed("/release/aterm.app");
            let (_, measuring) = measuring_cmd(&c, &artifact, measuring_args());
            assert!(
                !measuring.demoted,
                "the measuring benches run the paint, spin, aos and rewind guards"
            );
        }
        assert!(
            tippy_cmd(
                &c,
                std::path::Path::new("/s2/targo-tippy"),
                tippy_args(&c.scope)
            )
            .demoted,
            "tippy"
        );
        for args in [redraw_conformance_build_args(), objc_drives_build_args()] {
            assert!(main_build_cmd(&c, args.clone()).demoted, "{args:?}");
        }
        for argv in [
            redraw_conformance_build_args(),
            objc_drives_build_args(),
            spec_check_cmd(&c).argv()[1..].to_vec(),
        ] {
            assert!(argv.windows(2).any(|w| w == MAIN_PROFILE), "{argv:?}");
        }
    }

    #[test]
    fn the_gated_features_are_read_from_the_manifests() {
        assert_eq!(
            manifest_required_features(
                "[package]\nname = \"p\"\n# `required-features` keeps it out = [\"prose\"]\n\
                 [[bin]]\nname = \"not-p\"\nrequired-features = [\"a\", \"b\"] # why\n\
                 [[bench]]\nname = \"x\"\nrequired-features = [\n    \"c\",\n]\n"
            ),
            [("p", "a"), ("p", "b"), ("p", "c")].map(|(p, f)| (p.to_string(), f.to_string()))
        );
        assert!(
            manifest_required_features("[workspace]\nrequired-features = [\"a\"]\n").is_empty()
        );
        assert_eq!(
            fixture_gated(),
            [
                ("aterm-gui", "bench-support"),
                ("aterm-gui", "control-conformance"),
                ("aterm-scrollback", "disk-tier"),
            ]
            .map(|(p, f)| (p.to_string(), f.to_string()))
        );
    }

    #[test]
    fn the_gated_pass_narrows_with_the_scope_and_disappears_when_it_has_nothing_to_reach() {
        let gated = fixture_gated();
        let one = tippy_gated_args(&Scope::crate_only("aterm-scrollback"), &gated)
            .expect("aterm-scrollback owns two gated benches");
        let features = one
            .iter()
            .position(|a| a == "--features")
            .expect("features");
        assert_eq!(one[..features], ["-p", "aterm-scrollback"]);
        assert_eq!(one[features + 1], "aterm-scrollback/disk-tier");
        assert_eq!(
            tippy_gated_args(&Scope::crate_only("aterm-core"), &gated),
            None
        );
        assert_eq!(
            tippy_gated_args(
                &Scope::changed("main", vec!["aterm-core".into()], true),
                &gated
            ),
            None
        );
        assert!(
            tippy_gated_args(
                &Scope::changed("main", vec!["aterm-gui".into()], true),
                &gated
            )
            .is_some(),
            "a changed-scope run that rebuilt aterm-gui must still lint its benches"
        );
        let by_spec = tippy_gated_args(
            &Scope::changed("main", vec!["aterm-scrollback@9.9.9".into()], true),
            &gated,
        )
        .expect("the spec names the gated package");
        assert_eq!(
            by_spec[..4],
            [
                "-p",
                "aterm-scrollback@9.9.9",
                "--features",
                "aterm-scrollback/disk-tier"
            ]
        );
    }

    #[test]
    fn the_shipped_build_is_linted_with_debug_assertions_off() {
        let want = [
            "-p",
            "aterm",
            "--release",
            "--keep-going",
            "--",
            "-D",
            "warnings",
        ];
        for scope in [
            Scope::workspace(),
            Scope::changed("main", vec!["aterm-font".into(), "aterm".into()], true),
        ] {
            assert_eq!(tippy_release_args(&scope).expect("holds aterm"), want);
        }
        assert_eq!(tippy_release_args(&Scope::crate_only("aterm-font")), None);
        let row = |release: bool, scope: Scope| {
            let mut c = ctx(scope).with_release(release);
            c.tools.tippy = Some(PathBuf::from("/repo/bin").join(crate::toolchain::TIPPY));
            let mut r = Report::new("lint");
            tippy(&c, &mut r);
            r.outcomes()
                .filter(|(_, l)| l.contains("--release"))
                .count()
        };
        for release in [false, true] {
            assert_eq!(row(release, Scope::crate_only("aterm")), 1);
            assert_eq!(row(release, Scope::workspace()), 1);
            assert_eq!(row(release, Scope::crate_only("aterm-font")), 0);
            assert_eq!(row(release, Scope::changed("main", Vec::new(), false)), 0);
        }
    }

    /// EACH CALL OF A SHAPED TEST COMPILE RUNS ITS OWN COMMAND, THE CLOSING ONE TODAY'S: a
    /// stand-in targo traces each start, and the calls `-p aterm-gui --lib`, `-p aterm-gui` and
    /// the run's selection start `test -p aterm-gui ... --lib --no-run`, the same with `--tests`,
    /// and the unshaped command byte for byte, each under the main lane's build directory. Once
    /// a call fails, the calls behind it are COULD NOT RUN and start nothing. NEGATIVE CONTROL:
    /// the same three calls with none failing start three commands.
    #[cfg(unix)]
    #[test]
    fn each_call_of_a_shaped_test_compile_runs_its_own_command() {
        use crate::shape::{Part, Piece};
        let tmp = std::fs::canonicalize(crate::mktemp_dir("atv-shape-calls").expect("mktemp"))
            .expect("real");
        let trace = tmp.join("starts");
        let targo = tmp.join("bin/targo");
        crate::standin_body(
            &targo,
            &format!(
                "echo \"$*\" >> '{}'\nread code < '{}'\nexit \"$code\"",
                trace.display(),
                tmp.join("code").display()
            ),
        );
        let gui = vec!["aterm-gui".to_owned()];
        let pair = vec!["aterm-effects".to_owned(), "aterm-gui".to_owned()];
        let lib = Piece::of(&gui, true);
        let cut = Piece::of(&gui, false);
        let whole = Piece::of(&pair, false);
        let calls = |c: &Ctx| -> Vec<StageSpec> {
            let declared = crate::plan::declared(&c.scope).remove(0);
            [
                (&lib, 0, vec![]),
                (&cut, 1, vec![lib.clone()]),
                (&whole, 2, vec![lib.clone(), cut.clone()]),
            ]
            .into_iter()
            .map(|(piece, index, after)| StageSpec {
                part: Some(Part {
                    piece: piece.clone(),
                    index,
                    of: 3,
                    after,
                    said: String::new(),
                    planned: None,
                    refused: Vec::new(),
                }),
                ..declared.clone()
            })
            .collect()
        };
        type Rows = Vec<Vec<(crate::Outcome, String)>>;
        let run = |code: &str| -> (Rows, Vec<String>, Ctx) {
            std::fs::write(tmp.join("code"), code).expect("code");
            std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
            let _ = std::fs::remove_file(&trace);
            let mut c = Ctx::new(
                tmp.clone(),
                Scope::packages(&pair),
                EnvSnapshot::default(),
                tmp.join("scratch"),
            );
            c.tools.targo = targo.clone();
            let rows = calls(&c)
                .iter()
                .map(|spec| {
                    let mut r = Report::new(spec.title.clone());
                    test_compile(&c, spec, &mut r);
                    r.outcomes().map(|(o, l)| (o, l.to_owned())).collect()
                })
                .collect();
            let starts = std::fs::read_to_string(&trace)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            (rows, starts, c)
        };

        let (rows, starts, c) = run("0");
        assert!(
            rows.iter()
                .all(|r| r.iter().all(|(o, _)| *o == crate::Outcome::Ok)),
            "{rows:?}"
        );
        let lead = format!(
            "--config {}",
            c.main_build_dir.as_deref().expect("a build dir")
        );
        let tail = "--no-fail-fast --tests --no-run --message-format=json-render-diagnostics";
        let test = format!(
            "--unverified {} test {}",
            aterm_toolchain::build_cache::TEST_GRAPH_CONFIG.join(" "),
            aterm_toolchain::build_cache::TEST_GRAPH_FLAG
        );
        assert_eq!(
            starts,
            [
                format!(
                    "{lead} {test} -p aterm-gui --no-fail-fast --lib --no-run --message-format=json-render-diagnostics"
                ),
                format!("{lead} {test} -p aterm-gui {tail}"),
                test_cmds(&c, false)[0].argv()[1..].join(" "),
            ]
        );
        assert_eq!(
            starts[2],
            format!("{lead} {test} -p aterm-effects -p aterm-gui {tail}")
        );

        let (rows, starts, _) = run("101");
        assert_eq!(starts.len(), 1, "{starts:?}");
        assert!(matches!(rows[0][0].0, crate::Outcome::Fail(_)), "{rows:?}");
        for behind in &rows[1..] {
            assert_eq!(behind.len(), 1);
            assert_eq!(
                behind[0].0,
                crate::Outcome::Fail(crate::Severity::CouldNotRun)
            );
            assert!(
                behind[0]
                    .1
                    .contains("an earlier call of this test compile failed"),
                "{behind:?}"
            );
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A CUT THE CLOSING CALL REBUILT IS REFUSED FOR THE NEXT RUN: a stand-in targo reports the
    /// lib-test call building `libaterm_gui-1.rlib`, and the closing call reporting it again,
    /// fresh or not. Fresh, the closing call's time line says so and the book keeps no refusal;
    /// rebuilt, the book keeps the refusal row the planner reads ([`crate::shape::refusal_key`])
    /// and the time line names it. NEGATIVE CONTROL: the fresh run keeps no row.
    #[cfg(unix)]
    #[test]
    fn a_cut_the_closing_call_rebuilt_is_refused_for_the_next_run() {
        use crate::shape::{Part, Piece};
        let tmp = std::fs::canonicalize(crate::mktemp_dir("atv-shape-fresh").expect("mktemp"))
            .expect("real");
        std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
        let targo = tmp.join("bin/targo");
        let artifact = |fresh: &str| {
            format!(
                "{{\"reason\":\"compiler-artifact\",\"filenames\":[\"/d/libaterm_gui-1.rlib\"],\
                 \"fresh\":{fresh}}}"
            )
        };
        crate::standin_body(
            &targo,
            &format!(
                "read fresh < '{}'\ncase \"$*\" in *--lib*) echo '{}';; *) echo \"$fresh\";; esac",
                tmp.join("closing").display(),
                artifact("false")
            ),
        );
        let gui = vec!["aterm-gui".to_owned()];
        let lib = Piece::of(&gui, true);
        let whole = Piece::of(&gui, false);
        let run = |fresh: &str| -> (crate::memory::Records, Option<String>) {
            std::fs::write(tmp.join("closing"), artifact(fresh)).expect("closing");
            let path = tmp.join(format!("book-{fresh}.json"));
            let mut c = Ctx::new(
                tmp.clone(),
                Scope::packages(&gui),
                EnvSnapshot::default(),
                tmp.join("scratch"),
            )
            .with_memory(crate::memory::Book::at(
                Some(path),
                Some(24 << 30),
                Some(21 << 30),
            ));
            c.tools.targo = targo.clone();
            let declared = crate::plan::declared(&c.scope).remove(0);
            for (piece, index, after) in [(&lib, 0, vec![]), (&whole, 1, vec![lib.clone()])] {
                let spec = StageSpec {
                    part: Some(Part {
                        piece: piece.clone(),
                        index,
                        of: 2,
                        after,
                        said: String::new(),
                        planned: None,
                        refused: Vec::new(),
                    }),
                    ..declared.clone()
                };
                let mut r = Report::new(spec.title.clone());
                test_compile(&c, &spec, &mut r);
                let rendered = r.render();
                let units: Vec<aterm_json::Value> = rendered
                    .lines()
                    .filter_map(|line| line.strip_prefix("verify-event: "))
                    .map(|line| aterm_json::from_str::<aterm_json::Value>(line).unwrap())
                    .filter(|event| event["event"].as_str() == Some("compile-unit-finished"))
                    .collect();
                assert_eq!(units.len(), 1, "{rendered}");
                assert_eq!(
                    units[0]["fresh"].as_bool(),
                    Some(piece != &lib && fresh == "true")
                );
                assert_eq!(units[0]["rebuild_reason"], aterm_json::Value::Null);
                assert!(
                    r.outcomes().all(|(o, _)| o == crate::Outcome::Ok),
                    "{}",
                    r.render()
                );
            }
            let said = c
                .shape_run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .said
                .remove(&1);
            (c.memory.as_ref().expect("a book").records(), said)
        };
        let refused = crate::shape::refusal_key(&tmp.display().to_string(), &whole, &lib);

        let (records, said) = run("true");
        assert!(records.rows().iter().all(|r| r.key != refused));
        assert_eq!(
            said.as_deref(),
            Some("shape: fresh here: `-p aterm-gui --lib`'s 1 file")
        );

        let (records, said) = run("false");
        assert!(
            records.rows().iter().any(|r| r.key == refused),
            "{:?}",
            records.rows()
        );
        assert!(said.is_some_and(|s| s.contains("`-p aterm-gui --lib` refused: it rebuilt")));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A CUT REBUILT FROM A SOURCE EDITED UNDER THE RUN IS NOT REFUSED: a stand-in targo reports
    /// the lib-test call building `deps/libaterm_gui-1.rlib` and the closing call rebuilding it;
    /// its dep-info lists `crates/aterm-gui/src/lib.rs`, under the checkout. Modified after the
    /// cut began (an edit by the person or another agent in the worktree), the book keeps no
    /// refusal and the time line names the source; unchanged since, the cut is refused as the
    /// proof's miss. NEGATIVE CONTROL, by hand: with the sources unread, the edited run kept the
    /// refusal, and every later run of the selection would have been one call claiming the
    /// whole estimate alone.
    #[cfg(unix)]
    #[test]
    fn a_cut_rebuilt_from_a_source_edited_under_the_run_is_not_refused() {
        use crate::shape::{Part, Piece};
        let tmp = std::fs::canonicalize(crate::mktemp_dir("atv-shape-edit").expect("mktemp"))
            .expect("real");
        std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
        let deps = tmp.join("build/deps");
        std::fs::create_dir_all(&deps).expect("deps");
        let source = tmp.join("crates/aterm-gui/src/lib.rs");
        std::fs::create_dir_all(source.parent().expect("its folder")).expect("src");
        std::fs::write(&source, "").expect("source");
        let rlib = deps.join("libaterm_gui-1.rlib");
        std::fs::write(
            deps.join("aterm_gui-1.d"),
            format!("{}: crates/aterm-gui/src/lib.rs\n", rlib.display()),
        )
        .expect("dep-info");
        let targo = tmp.join("bin/targo");
        crate::standin_body(
            &targo,
            &format!(
                "echo '{{\"reason\":\"compiler-artifact\",\"filenames\":[\"{}\"],\"fresh\":false}}'",
                rlib.display()
            ),
        );
        let gui = vec!["aterm-gui".to_owned()];
        let lib = Piece::of(&gui, true);
        let whole = Piece::of(&gui, false);
        let run = |at: std::time::SystemTime| -> (crate::memory::Records, Option<String>) {
            std::fs::File::options()
                .write(true)
                .open(&source)
                .and_then(|f| f.set_modified(at))
                .expect("its time");
            let path = tmp.join("book.json");
            let _ = std::fs::remove_file(&path);
            let mut c = Ctx::new(
                tmp.clone(),
                Scope::packages(&gui),
                EnvSnapshot::default(),
                tmp.join("scratch"),
            )
            .with_memory(crate::memory::Book::at(
                Some(path),
                Some(24 << 30),
                Some(21 << 30),
            ));
            c.tools.targo = targo.clone();
            let declared = crate::plan::declared(&c.scope).remove(0);
            for (piece, index, after) in [(&lib, 0, vec![]), (&whole, 1, vec![lib.clone()])] {
                let spec = StageSpec {
                    part: Some(Part {
                        piece: piece.clone(),
                        index,
                        of: 2,
                        after,
                        said: String::new(),
                        planned: None,
                        refused: Vec::new(),
                    }),
                    ..declared.clone()
                };
                let mut r = Report::new(spec.title.clone());
                test_compile(&c, &spec, &mut r);
                assert!(
                    r.outcomes().all(|(o, _)| o == crate::Outcome::Ok),
                    "{}",
                    r.render()
                );
            }
            let said = c
                .shape_run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .said
                .remove(&1);
            (c.memory.as_ref().expect("a book").records(), said)
        };
        let refused = crate::shape::refusal_key(&tmp.display().to_string(), &whole, &lib);
        let day = std::time::Duration::from_secs(24 * 3600);
        let now = std::time::SystemTime::now();

        let (records, said) = run(now + day);
        assert!(
            records.rows().iter().all(|r| r.key != refused),
            "{:?}",
            records.rows()
        );
        assert!(
            said.as_deref().is_some_and(|s| s.contains(
                "not refused, its sources changed under the run: `-p aterm-gui --lib` (it rebuilt \
                 1 of its artifacts, and `crates/aterm-gui/src/lib.rs` changed after it began)"
            )),
            "{said:?}"
        );

        let (records, said) = run(now - day);
        assert!(records.rows().iter().any(|r| r.key == refused));
        assert!(said.is_some_and(|s| s.contains("`-p aterm-gui --lib` refused: it rebuilt")));
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// A REFUSAL ITS SELECTION KEEPS READING IS KEPT AGAIN, SO IT OUTLIVES THE RECORD'S CHURN: the
    /// one call a refused cut left S1's test compile in keeps the refusal row again as it runs,
    /// its newest peak now, so 512 rows kept after the first refusal (the record keeps 512, and
    /// lets the one whose newest peak is oldest go) leave it in place. NEGATIVE CONTROL, by hand:
    /// with the refusal not kept again, the 512 rows pushed it out, and the next run would have
    /// cut S1 the same wrong way.
    #[cfg(unix)]
    #[test]
    fn a_refusal_its_selection_keeps_reading_outlives_the_records_churn() {
        use crate::shape::{Part, Piece};
        let tmp = std::fs::canonicalize(crate::mktemp_dir("atv-shape-keep").expect("mktemp"))
            .expect("real");
        std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
        let targo = tmp.join("bin/targo");
        crate::standin_body(&targo, "exit 0");
        let s1 = crate::shape::tests::s1();
        let whole = Piece::of(&s1, false);
        let peel: Vec<String> = s1.iter().filter(|c| *c != "aterm-gui").cloned().collect();
        let peel = Piece::of(&peel, false);
        let path = tmp.join("book.json");
        let refused = crate::shape::refusal_key(&tmp.display().to_string(), &whole, &peel);
        let mut first = crate::memory::Records::default();
        first.push(refused.clone(), 0, 9);
        first.save(&path).expect("saved");
        let mut c = Ctx::new(
            tmp.clone(),
            Scope::packages(&s1),
            EnvSnapshot::default(),
            tmp.join("scratch"),
        )
        .with_memory(crate::memory::Book::at(
            Some(path.clone()),
            Some(24 << 30),
            Some(21 << 30),
        ));
        c.tools.targo = targo;
        let mut part = Part::whole(whole, String::new());
        part.refused = vec![peel];
        let spec = StageSpec {
            part: Some(part),
            ..crate::plan::declared(&c.scope).remove(0)
        };
        let mut r = Report::new(spec.title.clone());
        test_compile(&c, &spec, &mut r);
        assert!(
            r.outcomes().all(|(o, _)| o == crate::Outcome::Ok),
            "{}",
            r.render()
        );
        let mut records = crate::memory::Records::load(&path);
        let rows = u64::try_from(crate::memory::ROWS_KEPT).expect("a count");
        for at in 10..10 + rows {
            records.push(
                crate::memory::Key {
                    stage: "Test".to_owned(),
                    checkout: format!("/churn/{at}"),
                    crates: None,
                    variant: None,
                },
                1,
                at,
            );
        }
        assert!(
            records.rows().iter().any(|r| r.key == refused),
            "the refusal went: {} rows",
            records.rows().len()
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// THE TEST COMPILE NOTES WHAT IT USED, which a release run over the workspace that passes
    /// keeps ([`crate::disk::keep_used`], `atpkg::build_cache::stale::USED`): from its child's
    /// messages, the unit of a test it built in the build directory, the unit of a bin a test
    /// needs, which cargo named by its copy in the target alone, and the unit of a build script it
    /// ran, with the package it built tests of. NEGATIVE CONTROL: a compile whose child reported
    /// nothing keeps no record.
    #[cfg(unix)]
    #[test]
    fn the_test_compile_notes_what_it_used_for_the_release_record() {
        let tmp =
            std::fs::canonicalize(crate::mktemp_dir("atv-used").expect("mktemp")).expect("real");
        std::fs::create_dir_all(tmp.join("scratch")).expect("scratch");
        let leaf = tmp.join("cache/.cargo-build");
        let target = tmp.join("target");
        let [test, bin, script] = ["1111111111111111", "2222222222222222", "3333333333333333"];
        let out = |hash: &str| leaf.join("debug/build/atpkg").join(hash).join("out");
        let ran = out(bin).join("atpkg_abrew");
        std::fs::create_dir_all(out(bin)).expect("the bin's unit");
        std::fs::write(&ran, b"the bin the tests ran").expect("the bin");
        std::fs::create_dir_all(target.join("debug")).expect("the target");
        let copy = target.join("debug/atpkg-abrew");
        std::fs::write(&copy, b"the bin the tests ran").expect("its copy");
        let at = std::fs::metadata(&ran)
            .and_then(|m| m.modified())
            .expect("written");
        std::fs::File::options()
            .write(true)
            .open(&copy)
            .and_then(|f| f.set_modified(at))
            .expect("its copy's time");
        let id = "path+file:///w/aterm/crates/atpkg#0.155.0";
        let artifact = |file: &std::path::Path, test: bool| {
            aterm_json::json!({
                "reason": "compiler-artifact",
                "package_id": id,
                "target": {"kind": [if test { "lib" } else { "bin" }], "name": "atpkg"},
                "profile": {"test": test},
                "filenames": [file.display().to_string()],
                "executable": file.display().to_string(),
                "fresh": true,
            })
            .to_string()
        };
        let wire = [
            artifact(&out(test).join(format!("atpkg-{test}")), true),
            artifact(&copy, false),
            aterm_json::json!({
                "reason": "build-script-executed",
                "package_id": id,
                "out_dir": out(script).display().to_string(),
            })
            .to_string(),
        ]
        .join("\n");
        let targo = tmp.join("bin/targo");
        crate::standin_body(
            &targo,
            &format!("/bin/cat '{}'", tmp.join("wire").display()),
        );
        let compiled = |wire: &str| {
            std::fs::write(tmp.join("wire"), wire).expect("the wire");
            let mut c = Ctx::new(
                tmp.clone(),
                Scope::workspace(),
                EnvSnapshot {
                    cargo_target_dir: Some(target.clone()),
                    ..EnvSnapshot::default()
                },
                tmp.join("scratch"),
            )
            .with_checkers_in_tests(true);
            c.tools.targo = targo.clone();
            c.main_build_dir = Some(format!("build.build-dir='{}'", leaf.display()));
            let spec = crate::plan::declared(&c.scope).remove(0);
            let mut r = Report::new(spec.title.clone());
            test_compile(&c, &spec, &mut r);
            assert!(
                r.outcomes().all(|(o, _)| o == crate::Outcome::Ok),
                "{}",
                r.render()
            );
            c
        };
        let record = leaf.join(atpkg::build_cache::stale::USED);
        let c = compiled("");
        assert_eq!(
            crate::disk::keep_used(&c, true),
            None,
            "it reported nothing"
        );
        assert!(!record.exists());

        let c = compiled(&wire);
        assert!(crate::disk::keep_used(&c, true).is_some(), "kept");
        let kept = atpkg::build_cache::stale::Used::decode(&std::fs::read(&record).expect("kept"))
            .expect("a record");
        assert_eq!(
            kept.profiles.get("debug").cloned().unwrap_or_default(),
            [test, bin, script].map(str::to_owned).into()
        );
        assert_eq!(kept.members, ["atpkg".to_owned()].into());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn no_targo_is_fail_closed_at_the_test_compile_and_honest_everywhere_after() {
        let c = ctx(Scope::workspace());
        assert!(!c.tools.have_targo());

        let mut r = Report::new("test compile");
        let spec = crate::plan::declared(&c.scope).remove(0);
        test_compile(&c, &spec, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(
            outcome,
            crate::Outcome::Fail(crate::Severity::CouldNotRun),
            "{label}"
        );

        let dependent: [fn(&Ctx, &mut Report); 9] = [
            test,
            measuring_tests,
            measure_prebuild,
            doctests,
            spec_check,
            spec_checkers,
            export,
            libc_oracle,
            redraw_conformance,
        ];
        for stage in dependent {
            let mut r = Report::new("s");
            stage(&c, &mut r);
            for (outcome, label) in r.outcomes() {
                assert_eq!(outcome, crate::Outcome::Skip, "{label}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn checkers_do_not_retry_a_failed_main_test_compile() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let mut c = Ctx::new(
            temp.path().into(),
            Scope::workspace(),
            EnvSnapshot::default(),
            temp.path().join("scratch"),
        );
        c.tools.targo = "/usr/bin/false".into();
        c.tools.refused = None;
        let mut compile = Report::new("checkers compile");
        test_compile(&c, &crate::plan::declared(&c.scope).remove(0), &mut compile);
        assert!(
            c.test_compile_failed
                .load(std::sync::atomic::Ordering::Acquire)
        );
        c.scratch = temp.path().join("runtime-must-not-start");
        let mut runtime = Report::new("checkers");
        spec_checkers(&c, &mut runtime);
        assert!(
            !c.scratch.exists(),
            "a recording pass would create the scratch directory"
        );
        assert!(
            runtime
                .outcomes()
                .all(|(outcome, _)| outcome == Outcome::Fail(Severity::CouldNotRun))
        );
    }

    #[test]
    fn the_spec_check_runs_whole_under_release_and_names_a_missing_checker() {
        let c = ctx(Scope::workspace());
        let tail = |cmd: &Cmd| {
            cmd.argv()[1..]
                .iter()
                .skip_while(|a| *a != "spec-check")
                .cloned()
                .collect::<Vec<_>>()
        };
        let dev = spec_check_cmd(&c);
        assert_eq!(tail(&dev), ["spec-check"]);
        let argv = dev.argv();
        assert!(
            argv.windows(2).any(|w| w == ["-p", "aterm-models"])
                && !argv.iter().any(|a| a == "--workspace"),
            "a run keeps its package; `targo run` refuses --workspace: {argv:?}"
        );
        assert!(
            !dev.envs.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "the run's target dir, inherited"
        );
        let release = spec_check_cmd(&ctx(Scope::workspace()).with_release(true));
        assert_eq!(tail(&release), ["spec-check", "--", "--full"]);
        let run = |code: Option<i32>, output: &str| exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: code == Some(0),
            output: output.to_string(),
            code,
            spawn_error: None,
            usage: None,
        };
        let missing = run(
            Some(3),
            "spec-check: could not run `models`: it needs Trust `ty`, which was not found. \
             Install it: aterm pkg install ty\n",
        );
        assert_eq!(
            libc_oracle_outcome(&missing),
            Outcome::Fail(Severity::CouldNotRun)
        );
        assert_eq!(
            spec_check_not_run_reason(&missing).as_deref(),
            Some(
                "could not run `models`: it needs Trust `ty`, which was not found. Install it: \
                 aterm pkg install ty"
            )
        );
        let input = run(
            Some(3),
            "skip  ay           0.0s  could not run: no proofs/ay\n\nspec-check: COULD NOT RUN ay\n",
        );
        assert_eq!(
            spec_check_not_run_reason(&input).as_deref(),
            Some("no proofs/ay")
        );
        assert_eq!(
            libc_oracle_outcome(&run(Some(1), "")),
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(libc_oracle_outcome(&run(Some(0), "")), Outcome::Ok);
    }

    #[test]
    fn the_export_check_reads_ship_checks_exit_code() {
        let c = ctx(Scope::workspace());
        let cmd = export_cmd(&c);
        assert_eq!(cmd.term_grace, Some(EXPORT_TERM_GRACE));
        assert_eq!(
            cmd.argv(),
            main_targo(&c, ship_args(["check", "--export"])).argv(),
            "export uses the main target's build-dir and test graph"
        );
        assert!(
            cmd.envs.iter().all(|(name, _)| name != "CARGO_TARGET_DIR"),
            "the retired trust lane must not supply a target directory"
        );
        assert_eq!(export_outcome(Some(0), "").0, Outcome::Ok);
        for code in [1, 2, 101] {
            assert_eq!(
                export_outcome(Some(code), "").0,
                Outcome::Fail(Severity::GateFailed),
                "{code}"
            );
        }
        assert_eq!(export_outcome(Some(3), "").0, Outcome::Skip);
        for code in [Some(130), None] {
            assert_eq!(
                export_outcome(code, "").0,
                Outcome::Fail(Severity::CouldNotRun)
            );
        }

        #[cfg(unix)]
        {
            use aterm_census::export::mainline;
            let dir = aterm_tempfile::tempdir().expect("private Git fixture");
            let root = dir.path();
            let repo = root.join("repo");
            let origin = root.join("origin.git");
            std::fs::create_dir(&repo).unwrap();
            let git = |cwd: &std::path::Path, args: &[&str]| {
                let mut cmd = aterm_uds::fixture_git::git();
                cmd.current_dir(cwd)
                    .args([
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@example.invalid",
                        "-c",
                        "commit.gpgsign=false",
                    ])
                    .args(args);
                let out =
                    aterm_uds::deadline::output_within(&mut cmd, mainline::READ_WAIT).unwrap();
                assert!(
                    out.status.success(),
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8_lossy(&out.stdout).trim().to_owned()
            };
            git(
                root,
                &[
                    "init",
                    "-q",
                    "--bare",
                    "-b",
                    "main",
                    origin.to_str().unwrap(),
                ],
            );
            git(&repo, &["init", "-q", "-b", "main"]);
            git(
                &repo,
                &["remote", "add", "origin", origin.to_str().unwrap()],
            );
            git(&repo, &["commit", "-q", "--allow-empty", "-m", "main"]);
            git(&repo, &["push", "-q", "origin", "HEAD:main"]);
            git(&repo, &["fetch", "-q", "origin"]);
            let main = git(&repo, &["rev-parse", "HEAD"]);
            let scratch = root.join("capture");
            std::fs::create_dir(&scratch).unwrap();
            let mut c = Ctx::new(
                repo.clone(),
                Scope::workspace(),
                EnvSnapshot::default(),
                scratch,
            );
            c.path_env = std::env::var_os("PATH").unwrap_or_default();
            c.tools.targo = PathBuf::from("/bin/sh");
            c.tools.refused = None;
            c.tools.lock_refusal = None;
            let starts = std::cell::Cell::new(0);
            let producer = |_: &Cmd| {
                starts.set(starts.get() + 1);
                exec::run(
                    &Cmd::new("/bin/sh").args(["-c", "printf 'export producer ran\\n'"]),
                    c.exec_env(),
                )
            };
            let mut ready = Report::new("mainline export");
            export_with(&c, &mut ready, producer);
            assert_eq!(starts.get(), 1, "the eligible export producer actually ran");
            assert_eq!(ready.outcomes().next().unwrap().0, Outcome::Ok);
            git(&repo, &["commit", "-q", "--allow-empty", "-m", "feature"]);
            git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/feature"]);
            git(&repo, &["fetch", "-q", "origin"]);
            assert!(matches!(
                export_mainline(&c),
                Err(mainline::Refusal::NotOnMain)
            ));
            let mut feature = Report::new("pushed feature export");
            export_with(&c, &mut feature, producer);
            assert_eq!(starts.get(), 1, "a pushed feature branch starts no build");
            assert_eq!(feature.outcomes().next().unwrap().0, Outcome::Skip);
            git(&repo, &["push", "-q", "origin", "HEAD:main"]);
            git(&repo, &["update-ref", mainline::REF, &main]);
            assert!(
                matches!(export_mainline(&c), Err(mainline::Refusal::NotOnMain)),
                "only the locally fetched ref qualifies"
            );
            git(&repo, &["fetch", "-q", "origin"]);
            let mut fetched = Report::new("fetched mainline export");
            export_with(&c, &mut fetched, producer);
            assert_eq!(starts.get(), 2);
            assert_eq!(fetched.outcomes().next().unwrap().0, Outcome::Ok);
            git(&repo, &["update-ref", "-d", mainline::REF]);
            assert!(
                matches!(export_mainline(&c), Err(mainline::Refusal::Unread(_))),
                "an absent ref is not a nonancestor finding"
            );
            let mut unread = Report::new("unread mainline export");
            export_with(&c, &mut unread, producer);
            assert_eq!(starts.get(), 2, "unread ancestry starts no build");
            assert_eq!(unread.outcomes().next().unwrap().0, Outcome::Skip);
        }
    }

    #[test]
    fn appkit_builds_and_drives_from_the_main_target_with_no_feature() {
        let a = redraw_conformance_build_args();
        assert!(!a.iter().any(|x| x == "--features"), "{a:?}");
        let bin = a.iter().position(|x| x == "--bin").expect("a --bin");
        assert_eq!(a[bin + 1], REDRAW_CONFORMANCE_BIN);
        assert!(a.contains(&"aterm-gui".to_string()));
        let c = ctx(Scope::workspace());
        assert_eq!(
            main_bin(&c, REDRAW_CONFORMANCE_BIN),
            PathBuf::from("/repo/target/debug/aterm-redraw-conformance")
        );
        assert_eq!(
            main_example(&c, OBJC_DRIVES[0].example),
            PathBuf::from("/repo/target/debug/examples/objc_live_class_audit")
        );
        let build = main_build_cmd(&c, objc_drives_build_args());
        assert!(
            !build.envs.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "the main target, inherited"
        );
    }

    #[test]
    fn objc_build_selects_every_driver_once_in_one_invocation() {
        let args = objc_drives_build_args();
        let examples: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "--example")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(examples, OBJC_DRIVES.map(|drive| drive.example));
        assert_eq!(args.iter().filter(|arg| *arg == "build").count(), 1);
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--examples" || arg == "--features")
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_rows_keep_results_and_report_only_their_cpu_increment() {
        let c = ctx(Scope::workspace());
        let meter = crate::memory::Meter::new();
        let member = |cpu_ns| aterm_sysprobe::MemberUsage {
            pid: 7,
            start: 1,
            cpu_ns,
            ..Default::default()
        };
        meter.book(7, &[member(2_000_000_000)]);
        let _held = crate::memory::metering(Some(meter.clone()));
        let mut report = Report::new("native fixture");
        let result = native_row(&c, &mut report, "planted failure", |r| {
            meter.book(7, &[member(5_000_000_000)]);
            r.fail("negative control");
            17
        });
        assert_eq!(result, 17);
        assert!(
            report
                .outcomes()
                .any(|(o, _)| o == Outcome::Fail(Severity::GateFailed))
        );
        let rows = |report: &Report| {
            report
                .render()
                .lines()
                .filter_map(|line| line.strip_prefix("verify-event: "))
                .map(|json| aterm_json::from_str::<aterm_json::Value>(json).unwrap())
                .collect::<Vec<_>>()
        };
        let events = rows(&report);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event"].as_str(), Some("row-started"));
        assert_eq!(events[1]["event"].as_str(), Some("row-finished"));
        assert_eq!(events[1]["row"].as_str(), Some("planted failure"));
        assert_eq!(events[1]["cpu_s"].as_f64(), Some(3.0));
        let _unmetered = crate::memory::metering(None);
        let mut report = Report::new("unmetered fixture");
        native_row(&c, &mut report, "unread", |_| ());
        assert!(rows(&report)[1]["cpu_s"].is_null());
    }

    #[test]
    fn no_windows_skips_each_windowed_stage_by_name_and_nothing_else() {
        use crate::plan::{WINDOWED, plan};
        let every: Vec<crate::checks::Check> =
            crate::checks::CHECKS.iter().map(|s| s.check).collect();
        let base = || {
            let mut c = ctx(Scope::workspace()).with_checks(&every);
            c.tools.targo = PathBuf::from("/nonexistent/targo");
            c
        };
        let plain = base();
        let quiet = base().with_no_windows(true);
        let planned = plan(&quiet);
        assert_eq!(planned, plan(&plain), "the flag removes no stage");
        let mut seen = Vec::new();
        for spec in &planned {
            let Some((_, name)) = WINDOWED.iter().find(|(id, _)| *id == spec.id) else {
                assert_eq!(spec.id.no_windows_skip(), None, "{:?}", spec.id);
                continue;
            };
            seen.push(spec.id);
            let r = run_stage(&quiet, spec);
            let rows: Vec<_> = r.outcomes().collect();
            assert_eq!(rows.len(), 1, "{:?}", spec.id);
            assert_eq!(rows[0].0, Outcome::Skip, "{:?}", spec.id);
            assert!(rows[0].1.starts_with(name), "{}", rows[0].1);
            let ran = run_stage(&plain, spec);
            assert!(
                ran.outcomes()
                    .all(|(_, label)| !label.contains("--no-windows")),
                "{:?}: without the flag the stage's own body decides: {}",
                spec.id,
                ran.render()
            );
            assert!(ran.outcomes().next().is_some(), "{:?} decided", spec.id);
        }
        for (id, _) in WINDOWED {
            assert!(seen.contains(&id), "{id:?} is planned");
        }
    }

    /// NEGATIVE CONTROL: exit `2` (`NOT RUN`) is always `CouldNotRun` (never `Ok`), and when the
    /// driver's transcript names `NOT RUN — <reason>` (such as a closed clamshell with no active
    /// display), the ladder row quotes that reason directly instead of hiding it behind the
    /// generic fallback.
    #[test]
    fn every_objc_drive_reads_its_exit_contract() {
        for drive in &OBJC_DRIVES {
            let name = drive.example;
            let read = |code| objc_drive_outcome(drive, code);
            assert_eq!(read(Some(0)).0, Outcome::Ok, "{name}");
            assert_eq!(
                read(Some(1)).0,
                Outcome::Fail(Severity::GateFailed),
                "{name}"
            );
            assert_eq!(
                read(Some(2)).0,
                Outcome::Fail(Severity::CouldNotRun),
                "{name}"
            );
            let with_reason = objc_drive_outcome_with_transcript(
                drive,
                Some(2),
                &format!(
                    "{name}: NOT RUN — no display is online for drawing (CoreGraphics lists no active display)\n"
                ),
            );
            assert_eq!(
                with_reason.0,
                Outcome::Fail(Severity::CouldNotRun),
                "{name}"
            );
            assert_eq!(
                with_reason.1,
                format!(
                    "{name}: NOT RUN — no display is online for drawing (CoreGraphics lists no active display) (exit 2, never a pass)"
                ),
                "{name}"
            );
            let want = match drive.code3 {
                Code3::WeakerPassOk(_) => Outcome::Ok,
                Code3::Hung => Outcome::Fail(Severity::CouldNotRun),
                Code3::AbortFinding | Code3::None => Outcome::Fail(Severity::GateFailed),
            };
            assert_eq!(read(Some(3)).0, want, "{name}");
            for code in [4, 5, 101, 255] {
                assert_eq!(
                    read(Some(code)).0,
                    Outcome::Fail(Severity::GateFailed),
                    "{name}: exit {code} must not be green"
                );
            }
            assert_eq!(read(None).0, Outcome::Fail(drive.no_status), "{name}");
        }
        for code3 in [Code3::Hung, Code3::AbortFinding, Code3::None] {
            assert!(OBJC_DRIVES.iter().any(|d| d.code3 == code3), "{code3:?}");
        }
    }

    #[test]
    fn a_redraw_gate_that_could_not_run_is_never_a_pass_and_never_a_skip() {
        assert_eq!(
            redraw_outcome(Some(2)).0,
            Outcome::Fail(Severity::CouldNotRun)
        );

        assert_eq!(redraw_outcome(Some(0)).0, Outcome::Ok);
        assert_eq!(
            redraw_outcome(Some(1)).0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(
            redraw_outcome(Some(101)).0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(redraw_outcome(None).0, Outcome::Fail(Severity::CouldNotRun));
        for code in [None, Some(1), Some(2), Some(101), Some(-1)] {
            assert_ne!(redraw_outcome(code).0, Outcome::Ok, "{code:?}");
        }
    }

    fn measured_result(
        target: &str,
        planned: usize,
        passed: usize,
        failed: usize,
        refused: usize,
    ) -> String {
        format!(
            "measure result: {target} planned={planned} passed={passed} \
             failed={failed} refused={refused} unanswered=0 undecided=0\n"
        )
    }

    fn measured_subject(own: bool) -> Subject {
        Subject {
            name: "aterm 0.137.0 (22cb722a2ec9) at /Applications/aterm.app".into(),
            head: Some("215fb53331abd9acb2a33e7d7a21d1fa3c2bd8f2".into()),
            own,
        }
    }

    /// Every ladder row `decide_measuring_child` records for `output`.
    fn decide_measured(ok: bool, output: String, subject: &Subject) -> Vec<(Outcome, String)> {
        let run = exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok,
            output,
            code: Some(if ok { 0 } else { 101 }),
            spawn_error: None,
            usage: None,
        };
        let mut r = Report::new("measuring");
        decide_measuring_child(&mut r, &run, "measuring matrices", subject);
        r.outcomes().map(|(o, l)| (o, l.to_string())).collect()
    }

    #[test]
    fn a_measuring_child_requires_complete_decided_matrices() {
        let result = measured_result;
        let paint = result("paint", 23, 23, 0, 0);
        let spin =
            result("spin", 2, 2, 0, 0) + &result("aos", 1, 1, 0, 0) + &result("rewind", 2, 2, 0, 0);
        let decide = |ok: bool, output: String| {
            decide_measured(ok, output, &measured_subject(true))
                .into_iter()
                .next()
                .expect("a decision")
        };
        let could_not_run = Outcome::Fail(Severity::CouldNotRun);
        for output in [
            String::new(),
            "test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured\n".into(),
            result("paint", 0, 0, 0, 0) + &spin,
            result("paint", 23, 22, 0, 0) + &spin,
            result("paint", 23, 24, 0, 0) + &spin,
            paint.clone(),
            spin.clone(),
            paint.clone() + &paint + &spin,
            paint.replace("passed=23", "passed=unknown") + &spin,
            paint.clone() + &spin + "measure result: spin planned=2\n",
            paint.clone() + &result("spin", 2, 2, 0, 0),
            result("paint", usize::MAX, usize::MAX, 0, 1) + &spin,
        ] {
            assert_eq!(decide(true, output.clone()).0, could_not_run, "{output}");
        }
        let (outcome, label) = decide(true, spin.clone());
        assert_eq!(outcome, could_not_run);
        assert!(
            label.contains("paint") && !label.contains("spin"),
            "{label}"
        );
        assert_eq!(decide(true, paint.clone() + &spin).0, Outcome::Ok);
        assert_eq!(
            decide(false, result("paint", 23, 22, 0, 1) + &spin).0,
            could_not_run,
            "a row that could not run is not a finding about paint"
        );
        let undecided = |rows: &str| -> String {
            rows.split(',')
                .map(|row| {
                    format!(
                        "measure row: rewind::{row} UNDECIDED\n\
                         REWIND COST UNPROVED: the interval holds the bound\n"
                    )
                })
                .collect()
        };
        let spread = |passed: usize, undecided: usize| {
            format!(
                "measure result: rewind planned=2 passed={passed} failed=0 refused=0 \
                 unanswered=0 undecided={undecided}\n"
            )
        };
        let base = paint.clone() + &result("spin", 2, 2, 0, 0) + &result("aos", 1, 1, 0, 0);
        let one = undecided("cost") + &spread(1, 1);
        let (outcome, label) = decide(true, base.clone() + &one);
        assert_eq!(
            outcome,
            Outcome::Ok,
            "an undecided row beside passes is stated, not a refusal"
        );
        assert!(
            label.ends_with(
                "undecided, stated and not a finding: rewind::cost \
                 (REWIND COST UNPROVED: the interval holds the bound)"
            ),
            "{label}"
        );
        assert_eq!(
            decide(false, base.clone() + &one).0,
            Outcome::Fail(Severity::GateFailed),
            "the runner exits 0 there, so a crash after the summaries still fails"
        );
        let all_undecided = result("paint", 23, 0, 0, 0).replace("undecided=0", "undecided=23")
            + &result("spin", 2, 0, 0, 0).replace("undecided=0", "undecided=2")
            + &result("aos", 1, 0, 0, 0).replace("undecided=0", "undecided=1")
            + &undecided("cost,steps")
            + &spread(0, 2);
        let (outcome, label) = decide(true, all_undecided.clone());
        assert_eq!(
            outcome,
            Outcome::Ok,
            "a wholly undecided matrix on a busy machine is stated, never a refusal"
        );
        assert!(label.contains("rewind::steps"), "{label}");
        assert_eq!(
            decide(false, all_undecided).0,
            Outcome::Fail(Severity::GateFailed),
            "the runner exits 0 there, so a nonzero exit after the summaries still fails"
        );
        let loaded = "measure paint: unqualified — admitted loaded at its patience bound: CPU \
                      63.0% busy over 2.0s (quiet is under 25%), load 41.2 (quiet is at most \
                      28.0)\n";
        let all = base.clone() + &spread(2, 0);
        let (outcome, label) = decide(true, all.clone() + loaded);
        assert_eq!(
            outcome,
            Outcome::Ok,
            "a loaded matrix that failed nothing passes"
        );
        assert!(
            label.contains(
                "ran without a qualified quiet admission (stated, not a finding): paint — \
                 admitted loaded at its patience bound"
            ),
            "{label}"
        );
        let (outcome, label) = decide(true, all.clone());
        assert_eq!(outcome, Outcome::Ok);
        assert!(!label.contains("qualified quiet"), "{label}");
        assert_eq!(
            decide(
                true,
                result("paint", 23, 22, 1, 0)
                    + &result("spin", 2, 2, 0, 0)
                    + &result("aos", 1, 1, 0, 0)
                    + &spread(2, 0)
                    + loaded
            )
            .0,
            Outcome::Fail(Severity::GateFailed),
            "a failed row on a loaded machine is still a finding"
        );
        let refused = "measure paint: COULD NOT RUN — cannot acquire its exclusive timing \
                       claim: timing not qualified: no CPU window formed at its patience bound\n";
        let rest = result("spin", 2, 2, 0, 0) + &result("aos", 1, 1, 0, 0) + &spread(2, 0);
        assert_eq!(
            decide(false, rest.clone() + refused).0,
            could_not_run,
            "a bench that refused its own admission could not run, never a finding"
        );
        assert_eq!(
            decide(false, rest.clone()).0,
            Outcome::Fail(Severity::GateFailed),
            "a missing summary with no refusal and a nonzero exit is the child's failure"
        );
        let crashed = result("spin", 2, 2, 0, 0) + &result("aos", 1, 1, 0, 0) + refused;
        assert_eq!(
            decide(false, crashed).0,
            Outcome::Fail(Severity::GateFailed),
            "one bench's refusal never hides another's crash"
        );
        assert_eq!(
            decide(
                false,
                result("paint", 23, 22, 1, 0)
                    + &result("spin", 2, 2, 0, 0)
                    + &result("aos", 1, 1, 0, 0)
                    + &one
            )
            .0,
            Outcome::Fail(Severity::GateFailed),
            "a failed row outweighs an undecided one"
        );
        assert_eq!(
            decide(
                false,
                base.clone() + &result("rewind", 2, 0, 0, 1).replace("undecided=0", "undecided=1")
            )
            .0,
            could_not_run,
            "a row that could not run outweighs an undecided one"
        );
        assert_eq!(
            decide(
                false,
                result("paint", 23, 0, 0, 23)
                    + &result("spin", 2, 0, 0, 2)
                    + &result("aos", 1, 0, 0, 1)
                    + &result("rewind", 2, 0, 0, 2)
            )
            .0,
            could_not_run,
            "neither matrix decided anything"
        );
        assert_eq!(
            decide(
                false,
                result("paint", 23, 22, 1, 0)
                    + &result("spin", 2, 0, 0, 2)
                    + &result("aos", 1, 0, 0, 1)
                    + &result("rewind", 2, 0, 0, 2)
            )
            .0,
            Outcome::Fail(Severity::GateFailed),
            "a definite finding outweighs another matrix's refusal"
        );
        assert_eq!(
            decide(true, result("paint", 23, 22, 1, 0) + &spin).0,
            Outcome::Fail(Severity::GateFailed),
            "an incorrect zero exit cannot erase the row's finding"
        );
        assert_eq!(
            decide(false, paint + &spin).0,
            Outcome::Fail(Severity::GateFailed),
            "complete passes cannot hide an unexpected process failure"
        );
        assert_eq!(
            decide(
                false,
                "error: could not compile the measurement binary\n".into()
            )
            .0,
            Outcome::Fail(Severity::GateFailed)
        );
    }

    /// A row the subject has no answer to is a finding on HEAD's own build, and on another build
    /// one skip line naming exactly that row, beside the verdict on the rows it answered.
    #[test]
    fn an_unanswered_row_fails_heads_build_and_is_named_on_another() {
        let paint = measured_result("paint", 23, 23, 0, 0);
        let spin = "measure row: spin::an_idle_open_rewind_view_arms_no_timer UNANSWERED\n\
                    ctl rewind open: the subject does not know `rewind` (ERR unknown verb)\n\
                    measure result: spin planned=3 passed=2 failed=0 refused=0 unanswered=1 undecided=0\n";
        let aos = "measure row: aos::a_repair UNANSWERED\naos repair named no main_us=\n\
                   measure result: aos planned=1 passed=0 failed=0 refused=0 unanswered=1 undecided=0\n";
        let rewind = measured_result("rewind", 2, 2, 0, 0);
        let output = paint.clone()
            + spin
            + &bench_exit("spin", "exit status: 3")
            + aos
            + &bench_exit("aos", "exit status: 3")
            + &rewind;
        let named = "spin::an_idle_open_rewind_view_arms_no_timer (ctl rewind open: the subject \
                     does not know `rewind` (ERR unknown verb)); aos::a_repair (aos repair named \
                     no main_us=)";

        let other = decide_measured(false, output.clone(), &measured_subject(false));
        assert_eq!(other.len(), 2, "{other:?}");
        assert_eq!(other[0].0, Outcome::Ok, "{other:?}");
        assert_eq!(other[1].0, Outcome::Skip, "{other:?}");
        assert!(other[1].1.contains("(215fb53331ab)"), "{other:?}");
        assert!(
            other[1].1.ends_with(&format!("no answer to {named}")),
            "{other:?}"
        );

        let own = decide_measured(false, output.clone(), &measured_subject(true));
        assert_eq!(own.len(), 1, "{own:?}");
        assert_eq!(own[0].0, Outcome::Fail(Severity::GateFailed), "{own:?}");
        assert!(
            own[0].1.ends_with(&format!("no answer to {named}")),
            "{own:?}"
        );

        let failed =
            paint.replace("passed=23 failed=0", "passed=22 failed=1") + spin + aos + &rewind;
        let rows = decide_measured(false, failed, &measured_subject(false));
        assert_eq!(rows[0].0, Outcome::Fail(Severity::GateFailed), "{rows:?}");
        assert_eq!(rows[1].0, Outcome::Skip, "{rows:?}");
        let refused = paint.replace(
            "passed=23 failed=0 refused=0",
            "passed=22 failed=0 refused=1",
        ) + spin
            + aos
            + &rewind;
        let rows = decide_measured(false, refused, &measured_subject(false));
        assert_eq!(rows[0].0, Outcome::Fail(Severity::CouldNotRun), "{rows:?}");
        assert_eq!(rows[1].0, Outcome::Skip, "{rows:?}");
    }

    /// cargo's lines for a `--no-fail-fast` bench whose process ended `how`.
    fn bench_exit(target: &str, how: &str) -> String {
        format!(
            "error: bench failed, to rerun pass `-p aterm-conformance --bench {target}`\n\n\
             Caused by:\n  process didn't exit successfully: \
             `/t/build.noindex/test/deps/{target}-0123abcd --bench` ({how})\n"
        )
    }

    /// On a subject HEAD did not build, an unanswered row passes the rows it answered only when
    /// the runner's own exit 3 on those matrices is every failure cargo names: a crash, a panic or
    /// a kill after the summaries were printed is the child's failure, in any bench.
    #[test]
    fn an_unanswered_row_never_hides_a_crash_after_the_summaries() {
        let rewind = "measure row: rewind::a_rewind_step_is_shown_within_8ms UNANSWERED\n\
                      ctl metrics percentiles does not report n_rewind_shown\n\
                      measure result: rewind planned=2 passed=1 failed=0 refused=0 unanswered=1 undecided=0\n";
        let paint = measured_result("paint", 23, 23, 0, 0);
        let rest = measured_result("spin", 2, 2, 0, 0) + &measured_result("aos", 1, 1, 0, 0);
        let other = measured_subject(false);
        let first = |output: String| decide_measured(false, output, &other)[0].clone();
        let decided = |output: String| first(output).0;

        let alone = paint.clone() + &rest + rewind + &bench_exit("rewind", "exit status: 3");
        assert_eq!(
            decided(alone.clone()),
            Outcome::Ok,
            "the runner's own exit 3"
        );
        let rows = decide_measured(false, alone.clone(), &other);
        assert_eq!(rows[1].0, Outcome::Skip, "{rows:?}");
        assert!(rows[1].1.contains("n_rewind_shown"), "{rows:?}");
        let colored = alone.replace("error:", "\x1b[1m\x1b[91merror\x1b[0m:");
        assert_eq!(
            decided(colored),
            Outcome::Ok,
            "a colored cargo reads the same"
        );

        let segv = "signal: 11, SIGSEGV: invalid memory reference";
        for (output, why) in [
            (
                paint.clone()
                    + &bench_exit("paint", segv)
                    + &rest
                    + rewind
                    + &bench_exit("rewind", "exit status: 3"),
                "paint died of a signal after its summary",
            ),
            (
                paint.clone() + &rest + rewind + &bench_exit("rewind", segv),
                "the bench with the unanswered row died of a signal after its summary",
            ),
            (
                paint.clone()
                    + "error: bench failed, to rerun pass `-p aterm-conformance --bench paint`\n"
                    + &rest
                    + rewind
                    + &bench_exit("rewind", "exit status: 3"),
                "paint panicked after its summary (libtest's 101 has no `Caused by`)",
            ),
            (
                paint.clone() + &rest + rewind,
                "cargo named no failed bench, so its 101 is not the runner's",
            ),
            (
                paint.clone() + &rest + rewind + &bench_exit("rewind", "exit status: 1"),
                "another exit status",
            ),
            (
                paint.clone()
                    + &bench_exit("paint", "exit status: 3")
                    + &rest
                    + rewind
                    + &bench_exit("rewind", "exit status: 3"),
                "an exit 3 from a matrix that answered every row",
            ),
        ] {
            assert_eq!(
                decided(output.clone()),
                Outcome::Fail(Severity::GateFailed),
                "{why}: {output}"
            );
        }

        let killed = exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: false,
            output: alone,
            code: None,
            spawn_error: None,
            usage: None,
        };
        let mut r = Report::new("measuring");
        decide_measuring_child(&mut r, &killed, "measuring matrices", &other);
        assert_ne!(
            r.outcomes().next().map(|(o, _)| o),
            Some(Outcome::Ok),
            "a cargo that did not exit itself was killed"
        );
    }

    #[test]
    fn a_callers_cargo_build_jobs_is_a_ceiling_for_the_side_lanes() {
        let lane = Lane::CellsTarget;
        let own = lane_build_jobs(lane);
        assert_eq!(lane_jobs(lane, None), own, "{lane:?}");
        assert_eq!(
            lane_jobs(lane, Some(1)),
            Some(1),
            "{lane:?}: a smaller ceiling wins"
        );
        assert_eq!(lane_jobs(lane, Some(1000)), own, "{lane:?}: never a floor");
        let ceiling = |raw: &str| {
            let mut c = ctx(Scope::workspace());
            c.env.cargo_build_jobs = Some(raw.into());
            c.build_jobs_ceiling()
        };
        assert_eq!(ceiling(" 3 "), Some(3));
        for junk in ["", "0", "-1", "four", "4.5"] {
            assert_eq!(ceiling(junk), None, "{junk:?}");
        }
        let mut c = ctx(Scope::workspace());
        c.env.cargo_build_jobs = Some("3".into());
        let envs = |cmd: &Cmd| -> Vec<(String, String)> {
            cmd.envs
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect()
        };
        let export = envs(&export_cmd(&c));
        assert!(!export.iter().any(|(name, _)| name == "CARGO_BUILD_JOBS"));
        assert!(!export.iter().any(|(name, _)| name == "CARGO_TARGET_DIR"));
    }

    /// The words of `argv` (its program first) before its subcommand and those after it up to
    /// `--`, as cargo reads the line: a flag's value (`--config`, `-Z`, …) is no subcommand.
    fn around_verb(argv: &[String]) -> (Vec<&str>, &str, Vec<&str>) {
        let mut words = argv[1..].iter().map(String::as_str);
        let mut before = Vec::new();
        while let Some(word) = words.next() {
            if matches!(
                word,
                "--config" | "-Z" | "--manifest-path" | "--target-dir" | "--color" | "-C"
            ) {
                before.push(word);
                before.extend(words.next());
            } else if word.starts_with('-') {
                before.push(word);
            } else {
                return (before, word, words.take_while(|w| *w != "--").collect());
            }
        }
        panic!("no subcommand: {argv:?}")
    }

    /// Every `--config` of a main-lane child, and of the test run's recording pass, goes before
    /// its subcommand, the test graph's beside the build cache's and the recording runner's, and
    /// the test graph's flag right after it: Trust's `targo` reads a `--config` after the verb in
    /// place of those before it, which dropped the cache and the runner (every test binary then
    /// ran in cargo's serial pass, unrecorded). NEGATIVE CONTROL: B16's line, its `--config`
    /// after the verb, holds one there.
    #[test]
    fn a_main_lane_child_names_every_config_before_its_verb() {
        let mut c = ctx(Scope::crate_only("x"));
        c.main_build_dir = Some("build.build-dir='/cache'".to_string());
        let [compile, run] = test_cmds(&c, false);
        let recording = crate::testrun::recording_cmd(
            &run,
            "target.aarch64-apple-darwin.runner=[\"r\"]",
            "aarch64-apple-darwin",
        );
        let flag = aterm_toolchain::build_cache::TEST_GRAPH_FLAG;
        for cmd in [compile, run, recording, spec_check_cmd(&c)] {
            let argv = cmd.argv();
            let (before, _, after) = around_verb(&argv);
            assert!(!after.contains(&"--config"), "{argv:?}");
            assert_eq!(after.first(), Some(&flag), "{argv:?}");
            let configs = before.iter().filter(|w| **w == "--config").count();
            assert!(configs >= 2, "the cache's and the test graph's: {argv:?}");
        }
        let b16 = [
            "targo",
            "--config",
            "build.build-dir='/cache'",
            "--unverified",
            "test",
            flag,
            "--config",
            "resolver.feature-unification=\"workspace\"",
        ]
        .map(String::from);
        assert!(around_verb(&b16).2.contains(&"--config"));
    }

    #[test]
    fn a_callers_local_config_keeps_the_main_lanes_cache_and_test_graph() {
        let mut c = ctx(Scope::crate_only("x"));
        c.main_build_dir = Some("build.build-dir='/cache'".to_string());
        let argv = main_targo(
            &c,
            ["--unverified", "test", "--config", "build.jobs=1"]
                .map(String::from)
                .to_vec(),
        )
        .argv();
        let (before, verb, after) = around_verb(&argv);
        assert_eq!(verb, "test");
        assert!(!before.contains(&"--config"), "{argv:?}");
        for value in [
            "build.build-dir='/cache'",
            aterm_toolchain::build_cache::TEST_GRAPH_CONFIG[1],
            "build.jobs=1",
        ] {
            assert!(after.contains(&value), "{argv:?}");
        }
    }

    #[test]
    fn the_main_lanes_children_lead_with_the_shims_build_dir() {
        let mut c = ctx(Scope::workspace());
        let repo_target = std::path::absolute(&c.target_dir).expect("absolute");
        let fallback = format!(
            "build.build-dir='{}'",
            repo_target.join("build.noindex").display()
        );
        assert_eq!(
            c.main_build_dir.as_deref(),
            Some(fallback.as_str()),
            "no HOME in the fixture: no shim cache"
        );
        assert!(
            c.child_env_add
                .iter()
                .any(|(k, v)| k == "CARGO_TARGET_DIR" && v == c.target_dir.as_os_str())
        );
        let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
        let unify = aterm_toolchain::build_cache::TEST_GRAPH_FLAG;
        let narrow = || {
            ["--unverified", "test", "-p", "x"]
                .map(String::from)
                .to_vec()
        };
        assert_eq!(
            main_targo(&c, narrow()).argv()[1..],
            [
                "--config",
                fallback.as_str(),
                "--unverified",
                config,
                workspace,
                "test",
                unify,
                "-p",
                "x"
            ]
        );
        let value = "build.build-dir='/h/Library/Caches/aterm/cargo-build.noindex{workspace-root}/.cargo-build'";
        c.main_build_dir = Some(value.to_string());
        let [compile, run] = test_cmds(&c, false);
        let export = export_cmd(&c);
        for cmd in [
            &compile,
            &run,
            &spec_check_cmd(&c),
            &linux_lab_build_cmd(&c),
            &export,
        ] {
            assert_eq!(cmd.argv()[1..3], ["--config", value], "{:?}", cmd.argv());
            assert_eq!(
                (&cmd.argv()[4..6], &cmd.argv()[7]),
                (
                    &[config, workspace].map(String::from)[..],
                    &unify.to_owned()
                ),
                "the workspace's features, around the verb: {:?}",
                cmd.argv()
            );
            assert!(
                !cmd.envs
                    .iter()
                    .any(|(name, _)| name == aterm_toolchain::build_cache::BUILD_DIR_ENV),
                "a nested cargo in a build script must not inherit it: {:?}",
                cmd.envs
            );
        }
        assert!(
            main_targo(&c, vec!["tippy".into()])
                .envs
                .iter()
                .any(
                    |(name, directory)| name == aterm_toolchain::build_cache::BUILD_DIR_ENV
                        && directory
                            == aterm_toolchain::build_cache::directory_from_config(value).unwrap()
                ),
            "the same placement must reach an external subcommand"
        );
        c.main_build_dir = None;
        assert_eq!(
            main_targo(&c, narrow()).argv()[1..],
            ["--unverified", config, workspace, "test", unify, "-p", "x"],
            "unplaced (off macOS, no shim), the main lane still resolves the workspace's features"
        );
        assert!(
            !main_targo(&c, vec!["tippy".into()])
                .envs
                .iter()
                .any(|(name, _)| name == aterm_toolchain::build_cache::BUILD_DIR_ENV)
        );
        assert!(
            export_cmd(&c)
                .envs
                .iter()
                .all(|(name, _)| name != "CARGO_TARGET_DIR"),
            "an unplaced main lane still does not create a private target"
        );
    }

    #[test]
    fn the_codex_alab_proof_drives_the_main_targets_aterm() {
        let lane = |cmd: &Cmd| {
            cmd.envs
                .iter()
                .find(|(k, _)| k.to_str() == Some("CARGO_TARGET_DIR"))
                .map(|(_, v)| PathBuf::from(v))
        };
        let every: Vec<crate::checks::Check> =
            crate::checks::CHECKS.iter().map(|s| s.check).collect();
        let c = ctx(Scope::workspace()).with_checks(&every);
        let plan = crate::plan::plan(&c);
        let lane_of = |id: StageId| {
            let spec = plan
                .iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("{id:?} is planned"));
            lane_dir(&c, spec.lane)
        };

        let aterm = codex_aterm_binary(&c);
        let build = codex_aterm_build_cmd(&c);
        let suite = codex_alab_proof_cmd(&c);
        let lead = [
            "--config".to_string(),
            c.main_build_dir.clone().expect("a build dir"),
        ];
        assert_eq!(build.argv()[1..3], lead);
        let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
        let unify = aterm_toolchain::build_cache::TEST_GRAPH_FLAG;
        assert_eq!(
            build.argv()[3..],
            [
                "--unverified",
                config,
                workspace,
                "build",
                unify,
                "--profile",
                "test",
                "-q",
                "-p",
                "aterm",
                "-p",
                "aterm-lab",
                "--bin",
                "aterm",
                "--bin",
                "aterm-lab"
            ]
        );
        assert!(build.demoted, "the build only compiles");
        assert!(!suite.demoted, "the suite runs code");
        assert_eq!(lane(&build), None, "the main target, inherited");
        assert_eq!(lane_of(StageId::CodexAlabProof), Some(c.target_dir.clone()));
        assert_eq!(aterm, c.target_dir.join("debug/aterm"));
        assert_eq!(
            suite.argv()[1..],
            [
                "codex-alab-proof".to_string(),
                "--aterm".to_string(),
                aterm.display().to_string()
            ]
        );
        assert_eq!(suite.program, c.target_dir.join("debug/aterm-lab"));
        assert!(suite.envs.is_empty(), "{:?}", suite.envs);
        assert_eq!(
            suite.term_grace,
            Some(CODEX_TERM_GRACE),
            "a lane over the ceiling is sent SIGTERM first, so it tears down"
        );
    }

    #[test]
    fn the_linux_lane_runs_the_main_targets_lab_and_its_not_run_is_never_a_pass() {
        use crate::Outcome;
        let c = ctx(Scope::workspace()).with_checks(&[crate::checks::Check::Linux]);
        let build = linux_lab_build_cmd(&c);
        let lead = [
            "--config".to_string(),
            c.main_build_dir.clone().expect("a build dir"),
        ];
        assert_eq!(build.argv()[1..3], lead);
        let [config, workspace] = aterm_toolchain::build_cache::TEST_GRAPH_CONFIG;
        let unify = aterm_toolchain::build_cache::TEST_GRAPH_FLAG;
        assert_eq!(
            build.argv()[3..],
            [
                "--unverified",
                config,
                workspace,
                "build",
                unify,
                "--profile",
                "test",
                "-q",
                "-p",
                "aterm-lab",
                "--bin",
                "aterm-lab"
            ]
        );
        assert!(linux_lab_build_args().windows(2).any(|w| w == MAIN_PROFILE));
        assert!(
            !build.envs.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "the main target, inherited"
        );
        assert!(build.demoted, "the build only compiles");
        let lane = linux_lane_cmd(&c);
        assert!(!lane.demoted, "the lane runs code");
        assert!(
            lane.term_grace.is_some(),
            "the lab stops its VM before the kill"
        );
        assert_eq!(lane.program, c.target_dir.join("debug/aterm-lab"));
        assert_eq!(lane.argv()[1..], ["linux".to_string(), "proof".to_string()]);
        let admitting = |mode| linux_lane_cmd(&ctx(Scope::workspace()).with_quiet(Some(mode)));
        assert_eq!(
            admitting(aterm_capacity::Admit::Now).argv()[1..],
            ["linux", "--admit", "now", "proof"].map(String::from),
            "the run's --admit now is the lab's"
        );
        assert_eq!(
            admitting(aterm_capacity::Admit::Wait).argv()[1..],
            ["linux", "proof"].map(String::from),
            "wait is the lab's own default"
        );
        assert_eq!(
            linux_lane_outcome(Some(0), ""),
            (Outcome::Ok, LINUX_LANE.to_string())
        );
        assert_eq!(
            linux_lane_outcome(Some(1), "").0,
            Outcome::Fail(Severity::GateFailed)
        );
        let transcript = "aterm-lab: NOT RUN: no limactl: LAB_LIMACTL=/path/to/limactl names \
                          one; get lima's official release from github.com/lima-vm/lima/releases\n";
        let (outcome, label) = linux_lane_outcome(Some(3), transcript);
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            label.contains("no limactl: LAB_LIMACTL=/path/to/limactl names one"),
            "{label}"
        );
        assert_eq!(
            linux_lane_outcome(None, "").0,
            Outcome::Fail(Severity::CouldNotRun)
        );
        let verdict_over = |(outcome, label): (Outcome, String)| {
            let mut r = Report::new("linux");
            r.record(outcome, label);
            let result = crate::report::judge(
                crate::checks::Check::Linux,
                &[&r],
                1.0,
                "",
                String::new(),
                None,
            );
            crate::report::verdict(&[result], false, 1.0)
        };
        let (line, code) = verdict_over(linux_lane_outcome(Some(3), transcript));
        assert_eq!(code, crate::exit::COULD_NOT_RUN, "{line}");
        assert!(line.starts_with("verify: COULD NOT RUN — linux"), "{line}");
        let (line, code) = verdict_over(codex_alab_proof_outcome(Some(3), transcript));
        assert_eq!(code, crate::exit::PASS, "{line}");
    }

    #[test]
    fn the_codex_alab_proof_without_a_package_is_a_named_skip_never_a_pass() {
        use crate::Outcome;
        assert_eq!(
            codex_alab_proof_outcome(Some(0), "PASS"),
            (Outcome::Ok, CODEX_ALAB_LANE.to_string())
        );
        assert_eq!(
            codex_alab_proof_outcome(Some(1), "FAIL: 2 check(s)").0,
            Outcome::Fail(Severity::GateFailed)
        );
        let transcript = "aterm-lab: NOT RUN: no codex-alab package (the managed codex: /x is not there; pass --package <dir>)\n";
        let (outcome, label) = codex_alab_proof_outcome(Some(3), transcript);
        assert_eq!(outcome, Outcome::Skip);
        assert!(label.contains("no codex-alab package"), "{label}");
        assert_eq!(
            codex_alab_proof_outcome(None, "").0,
            Outcome::Fail(Severity::CouldNotRun),
            "a signal decided nothing"
        );
        let (outcome, label) = codex_alab_proof_outcome(Some(3), "");
        assert_eq!(outcome, Outcome::Skip);
        assert!(label.contains("the lab gave no reason"), "{label}");
        for other in [2, 77, 101] {
            assert_eq!(
                codex_alab_proof_outcome(Some(other), "").0,
                Outcome::Fail(Severity::GateFailed),
                "exit {other}"
            );
        }
        let c = ctx(Scope::workspace());
        let cmd = codex_alab_proof_cmd(&c);
        assert_eq!(cmd.argv()[1], "codex-alab-proof");
        assert_eq!(cmd.argv()[3], codex_aterm_binary(&c).to_string_lossy());
        assert_eq!(
            std::path::Path::new(&cmd.argv()[0]).parent(),
            codex_aterm_binary(&c).parent()
        );
    }

    #[test]
    fn manifest_required_features_strips_comments_and_handles_single_quotes() {
        let manifest = "[package]\nname = 'my-pkg'\n\
                        [[bin]]\nname = \"b\"\n\
                        required-features = [\n\
                          \"feat-a\", # a comment with ] inside\n\
                          'feat-b',\n\
                        ]\n";
        assert_eq!(
            manifest_required_features(manifest),
            vec![
                ("my-pkg".to_string(), "feat-a".to_string()),
                ("my-pkg".to_string(), "feat-b".to_string()),
            ]
        );
    }
}
