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
        StageId::TestCompile => test_compile(ctx, &mut r),
        StageId::Test => test(ctx, &mut r),
        StageId::MeasuringTests => measuring_tests(ctx, &mut r),
        StageId::Doctests => doctests(ctx, &mut r),
        StageId::Tippy => tippy(ctx, &mut r),
        StageId::Formatting => crate::fmt::run(ctx, &mut r),
        StageId::Export => export(ctx, &mut r),
        StageId::LibcOracle => libc_oracle(ctx, &mut r),
        StageId::Census => census(ctx, &mut r),
        StageId::SpecCheck => spec_check(ctx, &mut r),
        StageId::CheckersCompile => checkers_compile(ctx, &mut r),
        StageId::SpecCheckers => spec_checkers(ctx, &mut r),
        StageId::GpuOracle => gpu_oracle(ctx, &mut r),
        StageId::MeasurePrebuild => measure_prebuild(ctx, &mut r),
        #[cfg(unix)]
        StageId::GuiSmoke => smoke_stages::gui_typing_smoke(ctx, &mut r),
        #[cfg(not(unix))]
        StageId::GuiSmoke => {
            r.skip(format!("{} (unix only)", spec.title));
        }
        StageId::RedrawConformance => redraw_conformance(ctx, &mut r),
        StageId::ObjcDrives => objc_drives(ctx, &mut r),
        StageId::ForeignCells => foreign_cells(ctx, &mut r),
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

/// `targo --unverified test <scope> --no-fail-fast --tests --no-run` — the test compile row's one child and the test stage's first: compile every test target and run nothing.
#[must_use]
pub fn test_compile_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.extend(TEST_TARGETS.map(String::from));
    a.push("--no-run".to_string());
    a.push("--message-format=json-render-diagnostics".to_string());
    a
}

/// `targo --unverified test <scope> --no-fail-fast --tests`: the second child, run only when the first compiled: every lib, bin and integration test target with `test = true` ([`TEST_TARGETS`], the first child's), no doctests (the doctests stage runs those) and no examples or benches.
#[must_use]
pub fn test_run_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.extend(TEST_TARGETS.map(String::from));
    a
}

/// The targets both test children select: every target with `test = true` (the libs, the bins and the integration tests that keep the default) and no example ([`test_compile_args`]).
pub const TEST_TARGETS: [&str; 1] = ["--tests"];

const MEASURING_TARGETS: [&str; 4] = ["paint", "spin", "aos", "rewind"];

/// `targo --unverified bench --profile test -p aterm-conformance --no-fail-fast --bench paint --bench spin --bench aos --bench rewind`: the measuring stage's one child, the paint, spin, aos and rewind matrices ([`MEASURING_TARGETS`]) that judge the release bundle's frames, idle wakes, repair main-thread time and Rewind step latency on a live instance.
#[must_use]
pub fn measuring_args() -> Vec<String> {
    measuring_targets_args()
}

/// [`measuring_args`]'s compile alone (`--no-run`): the measuring rows' prebuild, the main lane's last row before the first barrier, so the exclusive stage that runs them compiles nothing.
#[must_use]
pub fn measure_prebuild_args() -> Vec<String> {
    let mut a = measuring_targets_args();
    a.push("--no-run".to_string());
    a
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
    for t in MEASURING_TARGETS {
        a.extend(["--bench".to_string(), t.to_string()]);
    }
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
        s => s.crates().to_vec(),
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
            let text = std::fs::read_to_string(e.path().join("Cargo.toml")).ok()?;
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

/// What both lint passes end with: cargo's coverage flags, then the lint driver's arguments, which reach workspace members only (targo-tippy hands them to `tippy-driver`, cargo's `RUSTC_WORKSPACE_WRAPPER`), so the dependencies stay fresh.
pub const LINT_TAIL: [&str; 5] = ["--all-targets", "--keep-going", "--", "-D", "warnings"];

/// The workspace's `required-features` targets, as the `(package, feature)` pairs that switch them on, read from the members' manifests (`crates/*/Cargo.toml`), sorted and without repeats.
#[must_use]
pub fn gated_lint_features(root: &std::path::Path) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("Cargo.toml")).ok())
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
    while let Some(line) = lines.next() {
        let Some(rest) = line
            .trim()
            .strip_prefix("required-features")
            .and_then(|r| r.trim_start().strip_prefix('='))
        else {
            continue;
        };
        let mut list = rest.to_string();
        while !list.contains(']') {
            let Some(more) = lines.next() else { break };
            list.push_str(more);
        }
        let inner = list.split_once('[').map_or("", |(_, r)| r);
        for feat in inner.split(']').next().unwrap_or("").split(',') {
            let feat = feat.trim().trim_matches('"');
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

/// `<tippy> -p aterm --release --keep-going -- -D warnings`: the lint's pass over the shipped build as `ship` builds it.
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

/// The profile of every binary the run builds in the main target to drive (`spec-check`, the redraw harness, the objc drivers, the Linux lane's lab): the test compile's, so each links against the units that compile already built instead of compiling the dev profile's graph a second time.
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
    pub package: &'static str,
    pub example: &'static str,
    pub code3: Code3,
    /// How a death with no exit status (a signal the driver did not trap) reads.
    pub no_status: Severity,
    /// What exit `0` proved, in one line.
    pub pass: &'static str,
}

/// The objc drivers the `objc drives` stage builds and runs, in order — every row on every macOS run, whatever the scope, and every row after a failure.
pub const OBJC_DRIVES: [ObjcDrive; 5] = [
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_live_class_audit",
        code3: Code3::WeakerPassOk(
            "every registered row agrees — and at least one was checked against THIS FORK'S OWN DECLARATION, not this host's runtime, because its AppKit does not register the protocol that declares it (the auditor's part F and VERDICT lines name each one; exit 3)",
        ),
        no_status: Severity::CouldNotRun,
        pass: "every row the registered WinitWindowDelegate and WinitView hold agrees with the runtime's own authority, and each class claims the protocols its rows come from",
    },
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_ime_drive",
        code3: Code3::WeakerPassOk(
            "a composition ran through the ported WinitView's NSTextInputClient rows, and with Option as Alt, each Option chord arrived as its bare key — but the same chords WITHOUT Option as Alt did not all compose here (macOS did not make the drive the active app, or this layout's Option chords are not the U.S. dead keys), so whether Option as Alt stops each dead key was NOT measured (exit 3)",
        ),
        no_status: Severity::CouldNotRun,
        pass: "a composition ran through the ported WinitView's NSTextInputClient rows — preedit, cursor conversion, commit and the candidate-window rectangle — and with Option as Alt, no U.S. Option dead key (E, U, I, N, `) started a composition where the same key without it did, and Option+B arrived as b, not ∫",
    },
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_toolbar_drive",
        code3: Code3::Hung,
        no_status: Severity::CouldNotRun,
        pass: "the real tab strip answered every question — every state its drawing matrix names, and all four declared classes read off live objects against the runtime's own authority — and the menu bar's real status item held This Mac as its glance counts it, and listed a box that stopped its agent above an older report whose row's tip says its program carries on meanwhile",
    },
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_window_drive",
        code3: Code3::None,
        no_status: Severity::CouldNotRun,
        pass: "the real window answered every question — title, style mask, geometry, limits, theme, drag-and-drop, close and fullscreen",
    },
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_event_drive",
        code3: Code3::AbortFinding,
        no_status: Severity::GateFailed,
        pass: "every NSEvent-taking WinitView row and sendEvent: survived a real NSEvent of every type AppKit can deliver; the exception control was contained and the panic control aborted",
    },
];

/// `targo --unverified build --profile test -q -p <package> --example <example>` — one driver's own build, in the main target ([`MAIN_PROFILE`]).
#[must_use]
pub fn objc_drive_build_args(drive: &ObjcDrive) -> Vec<String> {
    [
        "--unverified",
        "build",
        MAIN_PROFILE[0],
        MAIN_PROFILE[1],
        "-q",
        "-p",
        drive.package,
        "--example",
        drive.example,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How the ladder reads one driver's exit.
#[must_use]
pub fn objc_drive_outcome(drive: &ObjcDrive, code: Option<i32>) -> (Outcome, String) {
    let name = drive.example;
    let (outcome, what) = match (code, drive.code3) {
        (Some(0), _) => (Outcome::Ok, drive.pass.to_string()),
        (Some(1), _) => (
            Outcome::Fail(Severity::GateFailed),
            "a finding — the transcript above names each check that failed (exit 1)".to_string(),
        ),
        (Some(2), _) => (
            Outcome::Fail(Severity::CouldNotRun),
            "no event loop, no window, nothing installed to drive, or another app held the activation a check needs (the transcript says which), so its claim was not proven (exit 2, never a pass)".to_string(),
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
        (None, _) => (
            Outcome::Fail(drive.no_status),
            match drive.no_status {
                Severity::GateFailed => "ABORTED — the driver died by a signal its trap could not convert, which is still the finding it exists to make".to_string(),
                Severity::CouldNotRun => "no exit status — killed by a signal, or never spawned".to_string(),
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

fn run_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    run_child(ctx, r, label, cmd, |r, run, label| {
        r.decide_child(run, label)
    })
}

fn run_tests_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    let env = crate::findings::rerun_env(cmd, ctx.test_thread_override());
    let cmd = &cmd.clone().counting_waits();
    run_child(ctx, r, label, cmd, |r, run, label| {
        r.decide_test_child(run, label, &env);
    })
}

fn run_child(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    decide: impl FnOnce(&mut Report, &exec::Run, &str),
) -> bool {
    let out = exec::run(cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    decide(r, &out, label);
    out.ok
}

fn run_scoped(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) {
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    run_labeled(ctx, r, label, cmd);
}

/// Why a selection-reading stage did nothing on an empty change cone, and the fix.
pub const EMPTY_CONE: &str = "(the change cone holds no crate — fix: pass -p <crate> or --all)";

fn targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    Cmd::new(&ctx.tools.targo).args(args)
}

/// A `targo` invocation in the main target: [`targo`], led by the build-cache `--config` plain `targo` gets from atpkg's shim when the run found one ([`Ctx::main_build_dir`]), so the run and a manual build share one cache.
#[must_use]
pub fn main_targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    let mut cmd = Cmd::new(&ctx.tools.targo);
    if let Some(value) = &ctx.main_build_dir {
        cmd = cmd.arg("--config").arg(value);
    }
    cmd.args(args)
}

/// `CARGO_BUILD_JOBS` for a side lane, `None` where cargo's default stays.
#[must_use]
pub const fn lane_build_jobs(lane: Lane) -> Option<u32> {
    match lane {
        Lane::TrustTarget | Lane::CellsTarget => Some(4),
        Lane::CheckersTarget => Some(8),
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

fn test_compile(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.cannot_run(ctx.tools.missing_targo_label());
        return;
    }
    let label = format!("targo test {} --no-run", ctx.scope.label());
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    let (suffix, bind) = test_doc_binding(ctx);
    let [compile, _] = test_cmds(ctx, bind);
    let _ = compile_tests(ctx, r, &format!("{label}{suffix}"), &compile);
}

fn compile_tests(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    compile: &Cmd,
) -> (bool, Result<Vec<crate::testrun::TestExe>, String>) {
    let mut out = exec::run(compile, ctx.exec_env());
    let exes = crate::testrun::take_test_executables(&mut out.output);
    r.raw(out.output.as_str());
    r.decide_child(&out, label);
    (out.ok, exes)
}

fn test_cmds(ctx: &Ctx, bind: bool) -> [Cmd; 2] {
    let cmd = |args: Vec<String>| {
        let c = main_targo(ctx, args);
        if bind { with_trustdoc(ctx, c) } else { c }
    };
    let (k, v) = TRAIL_LAWS_FULL;
    [
        cmd(test_compile_args(&ctx.scope)).demoted(),
        cmd(test_run_args(&ctx.scope)).env(k, v).counting_waits(),
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
    let build = exec::run(&main_build_cmd(ctx, args), ctx.exec_env());
    if build.ok {
        return true;
    }
    r.raw(build.output.as_str());
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
    let [compile, run] = test_cmds(ctx, bind);
    let run_label = format!("{label} --tests{suffix}");
    let compile_label = format!("{label} --no-run{suffix}");
    let (compiled, exes) = compile_tests(ctx, r, &compile_label, &compile);
    if compiled {
        crate::testrun::run_tests(ctx, r, &run_label, &run, exes);
    } else {
        r.cannot_run(format!(
            "{run_label} — the compile above failed, and cargo runs no test after a compile error{}",
            NOT_RUN_BEHIND
        ));
    }
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

fn measuring_cmd(ctx: &Ctx, artifact: &crate::MeasureArtifact, args: Vec<String>) -> (String, Cmd) {
    let label = format!("targo {}", args[1..].join(" "));
    let bin = artifact.bin("aterm");
    let mut cmd = main_targo(ctx, args)
        .env(RELEASE_PAINT_BIN, bin.as_os_str())
        .env(RELEASE_SPIN_BIN, bin.as_os_str());
    if ctx.no_windows {
        cmd = cmd.env(MEASURE_NO_WINDOWS, "1");
    }
    (label, cmd)
}

fn measure_prebuild(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("measuring rows' compile (no targo)");
        return;
    }
    let Some(artifact) = &ctx.measure_artifact else {
        r.skip(format!("measuring rows' compile {NO_APP}"));
        return;
    };
    let (label, cmd) = measuring_cmd(ctx, artifact, measure_prebuild_args());
    run_labeled(ctx, r, &label, &cmd.demoted());
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
}

impl MeasuringCounts {
    fn parse(fields: &str) -> Option<Self> {
        let mut fields = fields.split_whitespace();
        let mut count = |key: &str| fields.next()?.strip_prefix(key)?.parse().ok();
        let counts = Self {
            planned: count("planned=")?,
            passed: count("passed=")?,
            failed: count("failed=")?,
            refused: count("refused=")?,
            unanswered: count("unanswered=")?,
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
                == Some(self.planned)
    }
}

/// Each `measure row: <target>::<row> UNANSWERED` in `output`, with the first line of its reason.
fn unanswered_rows(output: &str) -> Vec<String> {
    let mut lines = output.lines();
    let mut rows = Vec::new();
    while let Some(line) = lines.next() {
        if let Some(row) = line
            .strip_prefix("measure row: ")
            .and_then(|l| l.strip_suffix(" UNANSWERED"))
        {
            rows.push(format!("{row} ({})", lines.next().unwrap_or("").trim()));
        }
    }
    rows
}

fn decide_measuring_child(r: &mut Report, run: &exec::Run, label: &str, subject: &Subject) {
    let unanswered = unanswered_rows(&run.output);
    if subject.own && !unanswered.is_empty() {
        subject.unanswered(r, Some(run), label, &unanswered);
        return;
    }
    let mut incomplete = Vec::new();
    let mut refused = Vec::new();
    let mut any_failed = false;
    let mut all_reported = true;
    for target in MEASURING_TARGETS {
        let prefix = format!("measure result: {target} ");
        let mut summaries = run.output.lines().filter_map(|l| l.strip_prefix(&prefix));
        let counts = summaries.next().and_then(MeasuringCounts::parse);
        let counts = counts.filter(|_| summaries.next().is_none());
        match counts {
            Some(counts) => {
                any_failed |= counts.failed != 0;
                if !counts.complete() {
                    incomplete.push(target);
                }
                if counts.refused != 0 {
                    refused.push(target);
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
        && (run.ok || all_reported || run.output.contains("measure: COULD NOT RUN —"))
    {
        r.cannot_run(format!(
            "{label} — {}: no complete nonempty measurement matrix",
            incomplete.join(", ")
        ));
    } else if incomplete.is_empty() && !refused.is_empty() {
        r.cannot_run(format!(
            "{label} — {}: the instrument could not decide every row",
            refused.join(", ")
        ));
    } else if incomplete.is_empty() && !unanswered.is_empty() {
        r.pass(format!("{label} — every row the subject answered"));
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
    run_tests_labeled(ctx, r, &label, &doctest_cmd(ctx, &driver, &skip));
}

fn doctest_cmd(ctx: &Ctx, driver: &DocDriver, skip: &[String]) -> Cmd {
    let cmd = main_targo(ctx, doctest_args(&ctx.scope));
    let stand_in = ctx
        .test_recorder()
        .filter(|_| !skip.is_empty())
        .and_then(|s| Some((s, cargo_doc_driver(ctx, driver)?)));
    let Some((stand_in, real)) = stand_in else {
        return match driver {
            DocDriver::Stage2 => with_trustdoc(ctx, cmd),
            _ => cmd,
        };
    };
    cmd.env("RUSTDOC", stand_in)
        .env(crate::docdriver::DRIVER_ENV, real)
        .env(
            crate::docdriver::SKIP_ENV,
            crate::docdriver::skip_list(skip),
        )
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

fn tippy_cmd(ctx: &Ctx, bin: &std::path::Path, args: Vec<String>) -> Cmd {
    let dir = bin.parent().unwrap_or(&ctx.tools.stage2_dir).to_path_buf();
    let mut path = std::ffi::OsString::from(dir.as_os_str());
    path.push(":");
    path.push(&ctx.path_env);
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
    run_scoped(
        ctx,
        r,
        &format!("tippy {} -D warnings{borrowed}", ctx.scope.label()),
        &tippy_cmd(ctx, &bin, tippy_args(&ctx.scope)),
    );
    if ctx.release
        && let Some(args) = tippy_gated_args(&ctx.scope, &gated_lint_features(&ctx.root))
    {
        run_scoped(
            ctx,
            r,
            &format!("tippy required-features targets -D warnings{borrowed}"),
            &tippy_cmd(ctx, &bin, args),
        );
    }
    if ctx.release
        && let Some(args) = tippy_release_args(&ctx.scope)
    {
        run_scoped(
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

/// Every walk of [`aterm_census::CENSUSES`], each once.
fn walks(ctx: &Ctx, r: &mut Report, check: &str) {
    if !in_checkout(ctx, r, check) {
        return;
    }
    let selected: Vec<&aterm_census::Census> = aterm_census::CENSUSES.iter().collect();
    let root = ctx.root.clone();
    let outcomes = match exec::bounded(ctx.child_ceiling, move || {
        aterm_census::run_censuses(&root, &selected)
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

/// The export check's row.
pub const EXPORT_LABEL: &str = "ship check --export";

/// The `SIGTERM` grace `ship check --export` gets at its ceiling.
pub const EXPORT_TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// `ship check --export` from the root, its release tools built in `<target>/verify/.ubd/trust.noindex` ([`Lane::TrustTarget`]), ended at its ceiling with [`EXPORT_TERM_GRACE`].
#[must_use]
pub fn export_cmd(ctx: &Ctx) -> Cmd {
    in_lane(
        ctx,
        Lane::TrustTarget,
        targo(ctx, ship_args(["check", "--export"])),
    )
    .term_grace(EXPORT_TERM_GRACE)
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
    if !ctx.tools.have_targo() {
        r.skip("export guards (no targo)");
        return;
    }
    let out = exec::run(&export_cmd(ctx), ctx.exec_env());
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
    match why {
        Some(why) => r.cannot_run(format!("{label} — {why}")),
        None => r.record_child(&out, outcome, label),
    }
}

fn census(ctx: &Ctx, r: &mut Report) {
    walks(ctx, r, "census");
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

/// `targo --unverified test --workspace --features aterm-spec/checkers --no-fail-fast --tests`: the `checkers` check's child.
#[must_use]
pub fn spec_checkers_args() -> Vec<String> {
    let mut a: Vec<String> = [
        "--unverified",
        "test",
        "--workspace",
        "--features",
        "aterm-spec/checkers",
        "--no-fail-fast",
    ]
    .map(String::from)
    .into();
    a.extend(TEST_TARGETS.map(String::from));
    a
}

/// `targo --unverified test -p aterm-gpu --lib --no-fail-fast frame_record_tests`: the
/// `gpu-oracle` check's child, this host's arm against its CPU twins and the record of
/// wgpu's frames ([`crate::checks::GOLDEN`]).
#[must_use]
pub fn gpu_oracle_args() -> Vec<String> {
    let (pkg, test) = crate::checks::GOLDEN;
    let module = test.split_once("::").map_or(test, |(m, _)| m);
    [
        "--unverified",
        "test",
        "-p",
        pkg,
        "--lib",
        "--no-fail-fast",
        module,
    ]
    .map(String::from)
    .into()
}

fn no_run(mut args: Vec<String>) -> Vec<String> {
    args.push("--no-run".to_string());
    args
}

fn feature_compile(ctx: &Ctx, r: &mut Report, lane: Lane, args: Vec<String>, what: &str) {
    if !ctx.tools.have_targo() {
        r.skip(format!("{what} compile (no targo)"));
        return;
    }
    let label = format!("targo {} --no-run", args[1..].join(" "));
    let cmd = in_lane(ctx, lane, targo(ctx, no_run(args))).demoted();
    run_labeled(ctx, r, &label, &cmd);
}

fn checkers_compile(ctx: &Ctx, r: &mut Report) {
    feature_compile(
        ctx,
        r,
        Lane::CheckersTarget,
        spec_checkers_args(),
        "checkers",
    );
}

fn spec_checkers(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("checkers (no targo)");
        return;
    }
    let cmd = in_lane(ctx, Lane::CheckersTarget, targo(ctx, spec_checkers_args()));
    run_tests_labeled(
        ctx,
        r,
        "targo test --workspace --features aterm-spec/checkers",
        &cmd,
    );
}

/// Did a passing gpu-oracle child not pass the oracle's test ([`crate::checks::GOLDEN`])?
/// Its filter then matched no test, so no frame was held to its twin.
#[must_use]
pub fn gpu_oracle_compared_nothing(run: &exec::Run) -> bool {
    if !run.ok {
        return false;
    }
    let (_, test) = crate::checks::GOLDEN;
    let log = crate::libtest::strip_ansi(&run.output);
    !log.lines().any(|line| {
        line.strip_prefix("test ")
            .and_then(|rest| rest.split_once(" ... "))
            .is_some_and(|(name, verdict)| name.ends_with(test) && verdict.starts_with("ok"))
    })
}

fn gpu_oracle(ctx: &Ctx, r: &mut Report) {
    const LABEL: &str = "targo test -p aterm-gpu --lib frame_record_tests";
    if !ctx.tools.have_targo() {
        r.skip("gpu oracle (no targo)");
        return;
    }
    let cmd = main_targo(ctx, gpu_oracle_args());
    let env = crate::findings::rerun_env(&cmd, ctx.test_thread_override());
    run_child(ctx, r, LABEL, &cmd, |r, run, label| {
        if gpu_oracle_compared_nothing(run) {
            r.cannot_run(format!(
                "{label} — {} did not run, so no frame was held to its twin",
                crate::checks::GOLDEN.1
            ));
            return;
        }
        match gpu_oracle_verdict(run) {
            Some(verdict) => r.decide_test_child(run, format!("{label} — {verdict}"), &env),
            None => r.decide_test_child(run, label, &env),
        }
    });
}

/// The gpu-oracle child's verdict line (`frame-record: verdict: …`, the last one): how many
/// frames it held to their twins on which adapter, how many strayed, the widest margin, and
/// how many of the record's rows moved there.
#[must_use]
pub fn gpu_oracle_verdict(run: &exec::Run) -> Option<String> {
    crate::libtest::strip_ansi(&run.output)
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix("frame-record: verdict: "))
        .map(str::to_owned)
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
            "aterm-redraw-conformance: no exit status — killed by a signal, or never spawned".to_string(),
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
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = redraw_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
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
    for drive in &OBJC_DRIVES {
        objc_drive(ctx, r, drive);
    }
}

fn objc_drive(ctx: &Ctx, r: &mut Report, drive: &ObjcDrive) {
    let name = drive.example;
    if !build_to_drive(
        ctx,
        r,
        objc_drive_build_args(drive),
        format!("targo build -p {} --example {name}", drive.package),
        name,
    ) {
        return;
    }
    let bin = main_example(ctx, name);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "{name}: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_drive_outcome(drive, out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
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
                    || format!("{name}: no exit status — killed by a signal, or never spawned"),
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
    if !run_labeled(ctx, r, CODEX_ATERM_BUILD_LABEL, &codex_aterm_build_cmd(ctx)) {
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
    r.raw(out.output.as_str());
    let (outcome, label) = codex_alab_proof_outcome(out.code, &out.output);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
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
    if !run_labeled(ctx, r, LINUX_LAB_BUILD_LABEL, &linux_lab_build_cmd(ctx)) {
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
    r.raw(out.output.as_str());
    let (outcome, label) = linux_lane_outcome(out.code, &out.output);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

fn wasm_glue(ctx: &Ctx, r: &mut Report) {
    let work = crate::plan::cells_dir(ctx).join("wasm-glue");
    let root = ctx.root.clone();
    glue_row(
        ctx,
        r,
        "wasm-glue (the generator on aterm-wasm-abi's toy, in a browser)",
        "the toy",
        {
            let (root, work) = (root.clone(), work.clone());
            move |log| crate::wasm_glue::toy::gate(&root, &work, false, log)
        },
    );
    glue_row(
        ctx,
        r,
        "wasm-glue oracle (the toy's goldens and driver against wasm-bindgen 0.2.108)",
        "the oracle",
        move |log| crate::wasm_glue::oracle::gate(&root, &work.join("oracle"), log),
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
/// own (or each `--cell`), each linted for its own triple on rustup's
/// toolchain (a STOCK EXCEPTION), one row per cell.
fn foreign_cells(ctx: &Ctx, r: &mut Report) {
    if in_checkout(ctx, r, "cells") {
        crate::cells::run(ctx, r);
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
        let (_, prebuild) = measuring_cmd(&c, &artifact, measure_prebuild_args());
        for var in ["ATERM_PAINT_BIN", "ATERM_SPIN_BIN"] {
            assert_eq!(
                env_of(&run, var),
                Some(PathBuf::from("/release/aterm.app/Contents/MacOS/aterm")),
                "{var}"
            );
            assert_eq!(env_of(&prebuild, var), env_of(&run, var), "{var}");
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
            "wasm-bindgen 0.2.108 is not on PATH",
            "targo --unverified verify provision --record",
        ));
        assert_eq!(outcome, Outcome::Skip, "{label}");
        let (outcome, label) =
            row(Refused::Failed("drive.mjs threw".into()).verdict("the toy's driver"));
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed), "{label}");
    }

    #[test]
    fn libc_oracle_reads_the_gates_verdict_before_the_machine() {
        let run = |code: Option<i32>, output: &str| exec::Run {
            ok: code == Some(0),
            output: output.to_string(),
            code,
            spawn_error: None,
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

    #[test]
    fn a_machine_with_no_doc_driver_anywhere_is_diagnosed_on_the_doctests_row() {
        let spec = |id| StageSpec {
            id,
            title: "t".into(),
            lane: crate::plan::Lane::MainTarget,
            exclusive: false,
            after_stages: Vec::new(),
            mem: 0,
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
        let no_driver = |scope| {
            let mut c = ctx(scope);
            c.tools.targo = PathBuf::from("/usr/bin/true");
            c.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
            c.path_env = "/nonexistent-dir".into();
            c.root = tmp.clone();
            c.scratch = std::env::temp_dir();
            c
        };
        let cc = no_driver(Scope::workspace());

        let r = run_stage(&cc, &spec(StageId::Test));
        let outcomes: Vec<Outcome> = r.outcomes().map(|(o, _)| o).collect();
        assert_eq!(outcomes, [Outcome::Ok, Outcome::Ok], "{}", r.render());

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
                let cmd = doctest_cmd(&c, &DocDriver::Stage2, skip);
                assert!(
                    cmd.args
                        .ends_with(&doc.iter().map(std::ffi::OsString::from).collect::<Vec<_>>())
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

    #[test]
    fn the_stand_in_drives_rustdoc_only_where_the_scan_cleared_a_library() {
        let (driver, skip_env) = (crate::docdriver::DRIVER_ENV, crate::docdriver::SKIP_ENV);
        let mut c = ctx(Scope::workspace());
        c.tools.trustdoc = PathBuf::from("/stage2/trustdoc");
        c.test_recorder = Some(PathBuf::from("/bin/aterm-verify"));
        c.env.rustdoc_override = Some("/caller/rustdoc".into());
        let skip = ["other".to_string(), "plain".to_string()];
        for (kind, real) in [
            (DocDriver::Stage2, "/stage2/trustdoc"),
            (DocDriver::Ambient, "/caller/rustdoc"),
        ] {
            let cmd = doctest_cmd(&c, &kind, &skip);
            assert_eq!(
                env_of(&cmd, "RUSTDOC"),
                Some("/bin/aterm-verify".into()),
                "{kind:?}"
            );
            assert_eq!(env_of(&cmd, driver), Some(real.into()), "{kind:?}");
            assert_eq!(
                env_of(&cmd, skip_env),
                Some("other,plain".into()),
                "{kind:?}"
            );
            let cmd = doctest_cmd(&c, &kind, &[]);
            assert_eq!(env_of(&cmd, driver), None, "{kind:?}");
            assert_eq!(env_of(&cmd, skip_env), None, "{kind:?}");
            let bound =
                (kind == DocDriver::Stage2).then(|| std::ffi::OsString::from("/stage2/trustdoc"));
            assert_eq!(env_of(&cmd, "RUSTDOC"), bound, "{kind:?}");
        }
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
        let cmd = doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()]);
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
        let cmd = doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()]);
        assert_eq!(env_of(&cmd, "RUSTDOC"), None);
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
        let cmd = doctest_cmd(&c, &DocDriver::BarePath, &["plain".to_string()]);
        assert_eq!(env_of(&cmd, "RUSTDOC"), None);
        assert_eq!(env_of(&cmd, crate::docdriver::DRIVER_ENV), None);
        assert_eq!(env_of(&cmd, crate::docdriver::SKIP_ENV), None);
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
        for argv in [
            measuring_args(),
            measure_prebuild_args(),
            spec_checkers_args(),
            gpu_oracle_args(),
        ] {
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
    fn the_gpu_oracle_passes_only_when_its_test_ran_and_names_the_verdict() {
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
                ok,
                output,
                code: Some(i32::from(!ok)),
                spawn_error: None,
            }
        };
        let compared = "renderer::frame_record_tests::armed_frames_hold_to_their_cpu_twins";
        assert!(!gpu_oracle_compared_nothing(&run(true, &[compared])));
        assert_eq!(gpu_oracle_verdict(&run(true, &[compared])), None);
        let mut decided = run(true, &[compared]);
        decided.output.push_str(
            "frame-record: verdict: 118 of 127 frames compared with their CPU twins on Apple M5 \
             Max (aarch64), 0 off, the widest 10.0 codes off (fire/readback/0); the record holds \
             no Apple M5 Max block\n",
        );
        assert_eq!(
            gpu_oracle_verdict(&decided).as_deref(),
            Some(
                "118 of 127 frames compared with their CPU twins on Apple M5 Max (aarch64), 0 off, \
                 the widest 10.0 codes off (fire/readback/0); the record holds no Apple M5 Max block"
            )
        );
        assert!(gpu_oracle_compared_nothing(&run(true, &[])));
        assert!(gpu_oracle_compared_nothing(&run(
            true,
            &["renderer::frame_record_tests::a_neighbour"]
        )));
        assert!(
            !gpu_oracle_compared_nothing(&run(false, &[])),
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
        assert!(
            env_of(&cmd, "PATH").is_some_and(|p| p.starts_with("/m/store/trust/9192/bin:")),
            "{cmd:?}"
        );
        assert_eq!(
            crate::fmt::borrowed_suffix(&c, TIPPY),
            " (the managed Trust build's /m/store/trust/9192/bin/targo-tippy)"
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
        for args in [
            redraw_conformance_build_args(),
            objc_drive_build_args(&OBJC_DRIVES[0]),
        ] {
            assert!(main_build_cmd(&c, args.clone()).demoted, "{args:?}");
        }
        for argv in [
            redraw_conformance_build_args(),
            objc_drive_build_args(&OBJC_DRIVES[0]),
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
        let row = |release: bool| {
            let mut c = ctx(Scope::crate_only("aterm")).with_release(release);
            c.tools.tippy = Some(PathBuf::from("/repo/bin").join(crate::toolchain::TIPPY));
            let mut r = Report::new("lint");
            tippy(&c, &mut r);
            r.outcomes()
                .filter(|(_, l)| l.contains("--release"))
                .count()
        };
        assert_eq!(row(true), 1);
        assert_eq!(row(false), 0);
    }

    #[test]
    fn no_targo_is_fail_closed_at_the_test_compile_and_honest_everywhere_after() {
        let c = ctx(Scope::workspace());
        assert!(!c.tools.have_targo());

        let mut r = Report::new("test compile");
        test_compile(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(
            outcome,
            crate::Outcome::Fail(crate::Severity::CouldNotRun),
            "{label}"
        );

        let dependent: [fn(&Ctx, &mut Report); 10] = [
            test,
            measuring_tests,
            measure_prebuild,
            doctests,
            spec_check,
            spec_checkers,
            gpu_oracle,
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
            argv.windows(2).any(|w| w == ["-p", "aterm-models"]),
            "the bin's package: {argv:?}"
        );
        assert!(
            !dev.envs.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "the run's target dir, inherited"
        );
        let release = spec_check_cmd(&ctx(Scope::workspace()).with_release(true));
        assert_eq!(tail(&release), ["spec-check", "--", "--full"]);
        let run = |code: Option<i32>, output: &str| exec::Run {
            ok: code == Some(0),
            output: output.to_string(),
            code,
            spawn_error: None,
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
        let cmd = export_cmd(&ctx(Scope::workspace()));
        assert_eq!(cmd.term_grace, Some(EXPORT_TERM_GRACE));
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
        let build = main_build_cmd(&c, objc_drive_build_args(&OBJC_DRIVES[0]));
        assert!(
            !build.envs.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
            "the main target, inherited"
        );
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
             failed={failed} refused={refused} unanswered=0\n"
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
            ok,
            output,
            code: Some(if ok { 0 } else { 101 }),
            spawn_error: None,
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
            "an undecided take is not a finding about paint"
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
                    measure result: spin planned=3 passed=2 failed=0 refused=0 unanswered=1\n";
        let aos = "measure row: aos::a_repair UNANSWERED\naos repair named no main_us=\n\
                   measure result: aos planned=1 passed=0 failed=0 refused=0 unanswered=1\n";
        let rewind = measured_result("rewind", 2, 2, 0, 0);
        let output = paint.clone() + spin + aos + &rewind;
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

    #[test]
    fn a_callers_cargo_build_jobs_is_a_ceiling_for_the_side_lanes() {
        for lane in [Lane::TrustTarget, Lane::CellsTarget, Lane::CheckersTarget] {
            let own = lane_build_jobs(lane);
            assert_eq!(lane_jobs(lane, None), own, "{lane:?}");
            assert_eq!(
                lane_jobs(lane, Some(1)),
                Some(1),
                "{lane:?}: a smaller ceiling wins"
            );
            assert_eq!(lane_jobs(lane, Some(1000)), own, "{lane:?}: never a floor");
        }
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
        assert!(envs(&export_cmd(&c)).contains(&("CARGO_BUILD_JOBS".to_string(), "3".to_string())));
    }

    #[test]
    fn the_main_lanes_children_lead_with_the_shims_build_dir() {
        let mut c = ctx(Scope::workspace());
        let fallback = "build.build-dir='/repo/target/build.noindex'";
        assert_eq!(
            c.main_build_dir.as_deref(),
            Some(fallback),
            "no HOME in the fixture: no shim cache"
        );
        assert!(
            c.child_env_add
                .iter()
                .any(|(k, v)| k == "CARGO_TARGET_DIR" && v == "/repo/target")
        );
        assert_eq!(
            main_targo(&c, vec!["x".into()]).argv()[1..],
            ["--config", fallback, "x"]
        );
        let value = "build.build-dir='/h/Library/Caches/aterm/cargo-build.noindex{workspace-root}/.cargo-build'";
        c.main_build_dir = Some(value.to_string());
        let [compile, run] = test_cmds(&c, false);
        for cmd in [
            &compile,
            &run,
            &spec_check_cmd(&c),
            &linux_lab_build_cmd(&c),
        ] {
            assert_eq!(cmd.argv()[1..3], ["--config", value], "{:?}", cmd.argv());
        }
        assert!(
            !export_cmd(&c).argv().iter().any(|a| a == "--config"),
            "a side lane keeps its own dir"
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
        assert_eq!(build.argv()[3..], codex_aterm_build_args()[..]);
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
        assert_eq!(build.argv()[3..], linux_lab_build_args()[..]);
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
            let result =
                crate::report::judge(crate::checks::Check::Linux, &[&r], 1.0, "", String::new());
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
}
