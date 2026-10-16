// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stages themselves. Each stage's argv is built by a small pure function,
//! so it is unit-tested: tippy's separate `CARGO_TARGET_DIR` and
//! `TRUST_NO_MIGRATE_WARN`, the doc-driver rule on the doc-running stages
//! (`DocDriver`), and `--unverified` on every driver invocation (`targo` refuses
//! a bare verb, so a run cannot be quietly unverified).

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
    // `--no-windows`: a stage that puts a window on the desktop is a named
    // skip, decided before it builds or launches anything (`plan::WINDOWED`).
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
        StageId::SealedLane => sealed_lane(ctx, &mut r),
        StageId::Tippy => tippy(ctx, &mut r),
        StageId::Formatting => crate::fmt::run(ctx, &mut r),
        StageId::Guards => guards(ctx, &mut r),
        StageId::Export => export(ctx, &mut r),
        StageId::TrustAdvisory => trust_lane(ctx, &mut r),
        StageId::LibcOracle => libc_oracle(ctx, &mut r),
        StageId::Census => census(ctx, &mut r),
        StageId::SpecCheck => spec_check(ctx, &mut r),
        StageId::TyConformances => ty_conformances(ctx, &mut r),
        StageId::DriverBuilds => driver_builds(ctx, &mut r),
        StageId::TailDriverBuilds => tail_driver_builds(ctx, &mut r),
        #[cfg(unix)]
        StageId::ControlSocketSmoke => smoke_stages::control_socket_smoke(ctx, &mut r),
        #[cfg(unix)]
        StageId::GuiSmoke => smoke_stages::gui_typing_smoke(ctx, &mut r),
        // The smokes drive a unix control socket, and the ladder is a unix
        // program; off unix the rows are named.
        #[cfg(not(unix))]
        StageId::ControlSocketSmoke | StageId::GuiSmoke => {
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
        StageId::Forge => forge_gate(ctx, &mut r),
        StageId::CodexLiveUpgrade => codex_live_upgrade(ctx, &mut r),
    }
    r
}

// ---------------------------------------------------------------------------
// The argv builders. Pure, so each is a unit test.
// ---------------------------------------------------------------------------

/// `targo --unverified test <scope> --no-fail-fast --no-run`: the test compile
/// row's one child and the test stage's first, compiling every test target and
/// running nothing.
///
/// Cargo's default selection, not `--tests`: `--tests` would skip the example
/// builds but change the graph the doctests share, so the doctests, on the
/// critical path, would recompile aterm-gui and its dependencies. Here they
/// build at t0, beside the other lanes.
///
/// `--no-fail-fast` is the coverage flag, as `--keep-going` is for
/// [`tippy_args`]: without it cargo stops at the first failing test binary, and
/// the silence of every binary behind it reads as green.
///
/// `--message-format=json-render-diagnostics`: diagnostics print as usual, and
/// cargo's `compiler-artifact` messages name every test executable with its
/// target, which gives the test run each binary's re-run spec
/// ([`crate::testrun`]). The JSON lines are removed from what the ladder shows
/// ([`crate::testrun::take_test_executables`]); the message format is not part
/// of cargo's fingerprint.
#[must_use]
pub fn test_compile_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.push("--no-run".to_string());
    a.push("--message-format=json-render-diagnostics".to_string());
    a
}

/// `targo --unverified test <scope> --no-fail-fast --tests`: the second child,
/// run only when the first compiled. `--tests` runs every lib, bin and
/// integration test target and no doctests, and adds no units to the `--no-run`
/// graph. The measuring rows are `#[ignore]` ([`measuring_args`] runs them).
///
/// Cargo runs it with a recording runner ([`crate::testrun::recording_cmd`]) and
/// verify runs the recorded binaries several at once
/// ([`crate::testrun::Limits::by_load`]); this argv as is runs whenever the
/// recording does not add up. Every binary gets [`LIBTEST_REPORT_TIME`].
#[must_use]
pub fn test_run_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.push("--tests".to_string());
    a.push("--".to_string());
    a.extend(LIBTEST_REPORT_TIME.iter().map(|s| (*s).to_string()));
    a
}

/// How many rows the test stage's slowest-tests table prints.
pub const SLOWEST_TESTS_LISTED: usize = 25;

/// What the test run hands every test binary after `--`: each verdict line then
/// carries its test's wall time (`test x ... ok <1.234s>`), and the stage prints
/// the slowest ([`crate::libtest::slowest`]). The harness-less `--tests` targets
/// (`aterm-objc`'s `main_thread_bound_drive`, `aterm-gui`'s
/// `headless_event_loop`, which need the main thread) parse these flags
/// themselves. Under `-Z` libtest also reads some environment variables; the run
/// removes them from every child ([`crate::LIBTEST_UNSTABLE_ENV`]).
pub const LIBTEST_REPORT_TIME: [&str; 2] = ["-Zunstable-options", "--report-time"];

/// The measuring stage's test targets in aterm-conformance: each must pass a
/// row, or the stage measured nothing for it ([`decide_measuring_child`]).
const MEASURING_TARGETS: [&str; 2] = ["paint", "spin"];

/// `targo --unverified test -p aterm-conformance --no-fail-fast --test paint
/// --test spin -- --ignored measuring::`: the measuring stage's one child, the
/// paint and spin matrices ([`MEASURING_TARGETS`]) that judge the release
/// bundle's frames and idle wakes on a live instance. Every row is `#[ignore]`,
/// so only `--ignored` selects them.
#[must_use]
pub fn measuring_args() -> Vec<String> {
    let mut a: Vec<String> = [
        "--unverified",
        "test",
        "-p",
        "aterm-conformance",
        "--no-fail-fast",
    ]
    .map(String::from)
    .into();
    for t in MEASURING_TARGETS {
        a.extend(["--test".to_string(), t.to_string()]);
    }
    a.extend(["--", "--ignored", "measuring::"].map(String::from));
    a
}

fn test_base_args(scope: &Scope) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "test".to_string()];
    a.extend(scope.args());
    a.push("--no-fail-fast".to_string());
    a
}

/// `targo --unverified test --doc <scope> --no-fail-fast`: the only doctest
/// runner (the test stage runs `--tests`); `--no-fail-fast` as in
/// [`test_compile_args`]. The `[profile.dev.build-override]` pin keeps this graph
/// to the test compile's units plus the doctests.
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

/// `targo --unverified build -q -p aterm-gui -p aterm-ctl` — the two binaries
/// the smokes drive.
#[must_use]
pub fn smoke_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "-p",
        "aterm-ctl",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `targo --unverified test -p aterm --features sealed --test two_nodes_sealed
/// --test fabric_world --no-fail-fast`: the sealed cross-host rung, and the
/// fabric world suite under the same feature, whose `sealed`-only tests
/// (`second_host`) compile nowhere else. `two_nodes_sealed` is
/// `required-features = ["sealed"]`, so the workspace test stage never builds
/// it. Both suites drive this package's own binary (`CARGO_BIN_EXE_aterm`),
/// which cargo builds first, in the driver lane ([`sealed_lane_cmd`]).
#[must_use]
pub fn sealed_lane_args() -> Vec<String> {
    [
        "--unverified",
        "test",
        "-p",
        "aterm",
        "--features",
        "sealed",
        "--test",
        "two_nodes_sealed",
        "--test",
        "fabric_world",
        "--no-fail-fast",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `<tippy> <scope> --all-targets --keep-going -- -D warnings`
/// ([`LINT_TAIL`]). `--keep-going` is the coverage flag: without it cargo stops
/// at the first failing unit, so the lint would report a prefix of the
/// workspace while reading like all of it. A member whose dependency fails to
/// compile still cannot be linted; the compiler's output says which.
#[must_use]
pub fn tippy_args(scope: &Scope) -> Vec<String> {
    let mut a = scope.args();
    a.extend(LINT_TAIL.map(String::from));
    a
}

/// What both lint passes end with: cargo's coverage flags, then the lint
/// driver's arguments, which reach workspace members only (targo-tippy hands
/// them to `tippy-driver`, cargo's `RUSTC_WORKSPACE_WRAPPER`), so the
/// dependencies stay fresh. The `clean { … }` islands need no flag: each owning
/// crate's build script switches them on under a Trust compiler.
pub const LINT_TAIL: [&str; 5] = ["--all-targets", "--keep-going", "--", "-D", "warnings"];

/// The workspace's `required-features` targets, as the `(package, feature)`
/// pairs that switch them on, read from the members' manifests
/// (`crates/*/Cargo.toml`), sorted and without repeats. `--all-targets` silently
/// skips a target whose `required-features` are off, so the main tippy pass
/// never compiles, say, `aterm-gui`'s `control-conformance` redraw harness.
/// Derived rather than listed, so a new gated target is linted the day it is
/// declared.
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

/// The `(package, feature)` pairs one manifest's `required-features` keys name,
/// under the `[package]` name (what `-p` takes). Declarations only, never
/// comments beside them; a list may span lines.
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

/// `<tippy> -p <pkg>… --features <pkg/feat>,… <LINT_TAIL>`: the second pass,
/// over the targets [`tippy_args`] cannot reach (`gated`,
/// [`gated_lint_features`]). Separate because cargo offers features per package
/// (`-p <pkg> --features <pkg>/<feat>`), not workspace-wide; it re-lints the
/// named packages and builds the dependency variants their wider feature set
/// resolves. `None` when the scope selects no gated package.
#[must_use]
pub fn tippy_gated_args(scope: &Scope, gated: &[(String, String)]) -> Option<Vec<String>> {
    let mut packages: Vec<&str> = Vec::new();
    let mut features: Vec<String> = Vec::new();
    for (pkg, feat) in gated {
        if !scope.includes_crate(pkg) {
            continue;
        }
        if !packages.contains(&pkg.as_str()) {
            packages.push(pkg);
        }
        features.push(format!("{pkg}/{feat}"));
    }
    if packages.is_empty() {
        return None;
    }
    let mut a: Vec<String> = packages
        .iter()
        .flat_map(|p| ["-p".to_string(), (*p).to_string()])
        .collect();
    a.push("--features".to_string());
    a.push(features.join(","));
    a.extend(LINT_TAIL.map(String::from));
    Some(a)
}

/// The five L0 censuses, by their `aterm_census::CENSUSES` names: the
/// `census` check. The source guards, the sixth walk, are the `guards` check,
/// so the `dev` set runs each walk once.
pub const L0_CENSUSES: [&str; 5] = ["mainloop", "locks", "wasm", "scope", "lazy-init"];

/// `targo --unverified run -q --profile gate -p aterm-release -- <verb>`: the
/// release tools, built as the `ship` alias builds them.
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

/// The redraw harness's target name — the `[[bin]]`, the built file and the
/// argv below all have to agree, so they read it from here.
pub const REDRAW_CONFORMANCE_BIN: &str = "aterm-redraw-conformance";

/// `targo --unverified build -q -p aterm-gui --features control-conformance
///  --bin aterm-redraw-conformance`
///
/// The feature is `required-features` on the target: without it cargo silently
/// builds nothing and the stage would gate on a stale binary.
#[must_use]
pub fn redraw_conformance_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm-gui",
        "--features",
        "control-conformance",
        "--bin",
        REDRAW_CONFORMANCE_BIN,
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How an objc driver's exit `3` reads, the one code whose meaning is the
/// driver's own. `0` pass, `1` finding and `2` not run mean the same for every
/// driver ([`objc_drive_outcome`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Code3 {
    /// A pass that claims less than `0`: some rows were checked against the
    /// fork's own declaration because this host's AppKit registers no protocol
    /// for them. Worded apart from `0` so no line claims runtime authority.
    WeakerPassOk(&'static str),
    /// The driver's watchdog fired (a modal tracking loop never returned), so it
    /// decided nothing: could not run.
    Hung,
    /// The driver's signal trap turned an abort into `3`, and for this driver
    /// the abort is the finding it exists to make.
    AbortFinding,
    /// The driver never answers `3`: an unexpected code, a finding.
    None,
}

/// One objc driver: an `[[example]]` that owns `fn main`, because libtest cannot
/// host AppKit (every `#[test]` body runs off the main thread, where `EventLoop`
/// construction panics). Its header states its exit contract; this row is how
/// the ladder reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjcDrive {
    pub package: &'static str,
    pub example: &'static str,
    pub code3: Code3,
    /// How a death with no exit status (a signal the driver did not trap)
    /// reads.
    pub no_status: Severity,
    /// What exit `0` proved, in one line.
    pub pass: &'static str,
}

/// The objc drivers the `objc drives` stage builds and runs, in order — every
/// row on every macOS run, whatever the scope, and every row after a failure.
pub const OBJC_DRIVES: [ObjcDrive; 5] = [
    // The registered classes read back off a real NSWindow; a mistyped argument
    // or a dropped protocol leaves the build and every unit test green.
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_live_class_audit",
        code3: Code3::WeakerPassOk(
            "every registered row agrees — and at least one was checked against THIS FORK'S OWN DECLARATION, not this host's runtime, because its AppKit does not register the protocol that declares it (the auditor's part F and VERDICT lines name each one; exit 3)",
        ),
        no_status: Severity::CouldNotRun,
        pass: "every row the registered WinitWindowDelegate and WinitView hold agrees with the runtime's own authority, and each class claims the protocols its rows come from",
    },
    // The audit proves `WinitView` is shaped right; this drives a whole
    // composition through its NSTextInputClient rows.
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_ime_drive",
        code3: Code3::None,
        no_status: Severity::CouldNotRun,
        pass: "a composition ran through the ported WinitView's NSTextInputClient rows — preedit, cursor conversion, commit and the candidate-window rectangle",
    },
    // It enters `-mouseDown:` IMPs directly; a context menu that really popped
    // would never return, and its watchdog says so as `3`.
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_toolbar_drive",
        code3: Code3::Hung,
        no_status: Severity::CouldNotRun,
        pass: "the real tab strip answered every question — 27 drawn states, and all four declared classes read off live objects against the runtime's own authority",
    },
    // The only row that touches the window surface, where a use-after-free can
    // compile clean.
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_window_drive",
        code3: Code3::None,
        no_status: Severity::CouldNotRun,
        pass: "the real window answered every question — title, style mask, geometry, limits, theme, drag-and-drop, close and fullscreen",
    },
    // An abort here, trapped (`3`) or not (no status), is the finding, never
    // could-not-run.
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_event_drive",
        code3: Code3::AbortFinding,
        no_status: Severity::GateFailed,
        pass: "every NSEvent-taking WinitView row and sendEvent: survived a real NSEvent of every type AppKit can deliver; the exception control was contained and the panic control aborted",
    },
];

/// `targo --unverified build -q -p <package> --example <example>` — one
/// driver's own build, a fingerprint check after the tail driver builds row.
#[must_use]
pub fn objc_drive_build_args(drive: &ObjcDrive) -> Vec<String> {
    [
        "--unverified",
        "build",
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

/// How the ladder reads one driver's exit. Only `0`, and the audit's weaker `3`,
/// are passes: `2` means no window server answered or nothing was installed to
/// drive, and must never read as green.
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
            "no event loop, no window, or nothing installed to drive, so nothing was proven (exit 2, never a pass)".to_string(),
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

/// `targo --unverified run -q -p aterm-libc-oracle --target-dir <lane>/gate-bin
/// -- --root <R> --target-dir <lane>`, from `<R>`: the oracle built and run
/// inside its own lane ([`Lane::LibcOracleTarget`], `<R>/libc-oracle/target`),
/// so it never queues on the main workspace's cargo lock. Both paths are
/// absolute because the oracle's children run from other cwds.
///
/// # Errors
/// The current-directory error from making a relative path absolute, which the
/// stage reports as could not run.
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

/// How the ladder reads the oracle's exit: `0` pass, `1` a finding (the
/// oracle's own verdict, which outranks a later stop, so even beside a full
/// disk), `3` or no status decided nothing. Any other code (`101`: targo could
/// not build or start the oracle; `2`: usage) is the machine's only when it ran
/// out of disk ([`exec::Run::environment_failure`]), else a finding.
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

/// Why a libc oracle that decided nothing did not run, for its row: the
/// machine's failure ([`exec::Run::environment_failure`]), else the oracle's
/// own last `COULD NOT RUN: <reason>` line, so the verdict names the fix rather
/// than leaving it in the raw log.
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

// ---------------------------------------------------------------------------
// Shared shapes
// ---------------------------------------------------------------------------

/// Run, print what the child said, and decide. Returns whether the child ran and
/// passed.
fn run_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    run_child(ctx, r, label, cmd, |r, run, label| {
        r.decide_child(run, label)
    })
}

/// [`run_labeled`] for a child that runs tests: a failure is itemized per failed
/// test ([`Report::decide_test_child`]).
fn run_tests_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    let env = crate::findings::rerun_env(cmd, ctx.test_threads);
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

/// [`run_labeled`] for the stages that compile the selected crates. An empty
/// cone never falls through to a bare `targo build`, which would build the
/// whole workspace under a narrow label: it is a named skip ([`EMPTY_CONE`]).
fn run_scoped(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) {
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    run_labeled(ctx, r, label, cmd);
}

/// Why a selection-reading stage did nothing on an empty change cone, and the fix.
pub const EMPTY_CONE: &str = "(the change cone holds no crate — fix: pass -p <crate> or --all)";

/// A bare `targo` invocation; [`in_lane`] puts it in a side lane.
fn targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    Cmd::new(&ctx.tools.targo).args(args)
}

/// `CARGO_BUILD_JOBS` for a side lane, `None` where cargo's default stays. The
/// side lanes overlap the build, and uncapped each would claim every core; tune
/// from the rows' `  time  ` lines. Cargo does not fingerprint the job count, so
/// a cap never causes a rebuild. The driver lane, whose binaries gate the
/// exclusive smokes, gets twice the other side lanes'.
#[must_use]
pub const fn lane_build_jobs(lane: Lane) -> Option<u32> {
    match lane {
        Lane::TrustTarget | Lane::CellsTarget => Some(4),
        Lane::DriverTarget => Some(8),
        _ => None,
    }
}

/// The job cap a side lane's child gets: [`lane_build_jobs`], never more than
/// `ceiling`, the run's [`Ctx::build_jobs_ceiling`] (the caller's own
/// `CARGO_BUILD_JOBS` and the memory cap), so a small machine's export and a
/// degraded host's memory hold on every lane.
#[must_use]
pub fn lane_jobs(lane: Lane, ceiling: Option<u32>) -> Option<u32> {
    let cap = lane_build_jobs(lane)?;
    Some(ceiling.map_or(cap, |c| cap.min(c)))
}

/// Put a cargo child in its lane: that lane's own `CARGO_TARGET_DIR` and job
/// cap, through the command's environment. `MainTarget`, `AfterCompile` (the
/// same directory) and `Pure` come back unchanged.
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

/// A build of a binary the run drives, in the driver lane. Only the build is
/// [`Cmd::demoted`]; the driven binary runs as its own child at the inherited
/// tier.
#[must_use]
pub fn driver_build_cmd(ctx: &Ctx, args: Vec<String>) -> Cmd {
    in_lane(ctx, Lane::DriverTarget, targo(ctx, args)).demoted()
}

/// The driver lane's target dir: every driven binary is resolved under it.
#[must_use]
pub fn drivers_dir(ctx: &Ctx) -> std::path::PathBuf {
    lane_dir(ctx, Lane::DriverTarget).unwrap_or_else(|| ctx.target_dir.clone())
}

/// A binary the driver lane built: `<drivers>/debug/<name>`. Launched
/// directly (not through `targo run`), so the recorded PID is the real process
/// and no lane banner lands in a captured reply.
#[must_use]
pub fn driver_bin(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    drivers_dir(ctx).join("debug").join(name)
}

/// An `[[example]]` the driver lane built — cargo puts those under
/// `<target>/debug/examples/`, not beside the binaries.
#[must_use]
pub fn driver_example(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    driver_bin(ctx, "examples").join(name)
}

/// Bind Trust's renamed documentation driver for the stages that run doctests.
fn with_trustdoc(ctx: &Ctx, cmd: Cmd) -> Cmd {
    cmd.env("RUSTDOC", ctx.tools.trustdoc.as_os_str())
}

/// Which doc driver a doc-running stage gets.
/// * `Stage2`: the pinned driver, bound through `RUSTDOC`, overriding anything
///   the caller exported.
/// * `Ambient`: a caller-exported `RUSTDOC`/`CARGO_BUILD_RUSTDOC`, which cargo
///   prefers over a PATH lookup; the ladder names it.
/// * `BarePath`: cargo resolves the `trustdoc` beside targo, else a bare
///   `trustdoc` on the children's PATH (the `~/.local/bin` farm link).
/// * `Absent`: a doctest-compiling run would die at exec with a raw OS error, so
///   the stage diagnoses instead, unless the run compiles no lib target.
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

/// Would this scope's `targo test` compile any doctests? Cargo spawns rustdoc
/// only for lib targets. The cone answers from its `Members` table, `-p` from
/// the members' manifests, and the workspace always holds libs. Unanswerable
/// answers yes (fail closed, as `Members::any_has_lib` does).
fn scope_compiles_doctests(ctx: &Ctx) -> bool {
    match &ctx.scope {
        Scope::Packages(ps) => ps
            .iter()
            .any(|c| crate::changed::crate_dir_has_lib(&ctx.root, c)),
        s => s.has_lib_target(),
    }
}

/// The rule as a pure truth table. The stage2's own copy outranks everything
/// (its `RUSTDOC` binding overrides even a caller's export); the caller's export
/// outranks the PATH walk, as cargo ranks it; the farm link is the fallback for
/// a stage2 without trustdoc.
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

// ---------------------------------------------------------------------------
// The test stage: its `--no-run` compile (a fingerprint check after the test
// compile row, which runs the same child at t0), then the `--tests` run if that
// compiled ([`crate::testrun`]). No doctests, so no doc-driver diagnosis;
// trustdoc is still bound when the stage2 has one, so both rows share one
// environment.
// ---------------------------------------------------------------------------

/// The doc-driver binding the test children share, and the label suffix that
/// names it: the stage2's trustdoc when it has one, the caller's own export,
/// or nothing — no test child spawns rustdoc, so an absent driver is the
/// doctests stage's to diagnose.
fn test_doc_binding(ctx: &Ctx) -> (&'static str, bool) {
    match doc_driver(ctx) {
        DocDriver::Stage2 => (" (trustdoc)", true),
        DocDriver::Ambient => (" (caller's RUSTDOC)", false),
        DocDriver::BarePath | DocDriver::Absent => ("", false),
    }
}

fn test_compile(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        // Could not run, not failed: nothing about the tree was decided. Never a
        // stock-cargo fallback, which would make the run quietly unverified.
        r.cannot_run(ctx.tools.missing_targo_label());
        return;
    }
    let label = format!("targo test {} --no-run", ctx.scope.label());
    // The empty change-selection outranks the doc-driver binding, as in the
    // test stage below: a run that compiles nothing names no driver.
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    let (suffix, bind) = test_doc_binding(ctx);
    let [compile, _] = test_cmds(ctx, bind);
    // The row decides on the compile; the executables its JSON names are the
    // test stage's to read, from its own `--no-run` over this compile.
    let _ = compile_tests(ctx, r, &format!("{label}{suffix}"), &compile);
}

/// Run the test compile and decide `label` on its output with cargo's JSON taken
/// out; return whether it compiled and the test executables its JSON reported
/// ([`crate::testrun::take_test_executables`]), which the recording is checked
/// against.
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

/// The test stage's two children, compile then run, with trustdoc bound when
/// `bind`. Only the compile is [`Cmd::demoted`]: the run launches code, and a QoS
/// clamp would reach it. The run carries [`TRAIL_LAWS_FULL`].
fn test_cmds(ctx: &Ctx, bind: bool) -> [Cmd; 2] {
    let cmd = |args: Vec<String>| {
        let c = targo(ctx, args);
        if bind { with_trustdoc(ctx, c) } else { c }
    };
    let (k, v) = TRAIL_LAWS_FULL;
    [
        cmd(test_compile_args(&ctx.scope)).demoted(),
        cmd(test_run_args(&ctx.scope)).env(k, v),
    ]
}

/// What every suite a failed build kept from running adds to its could-not-run
/// row: the build's own failure is the finding, and what stood behind it decided
/// nothing.
pub const NOT_RUN_BEHIND: &str = "; nothing behind it was decided";

/// The COULD NOT RUN row for `what`, which the failure above it kept from
/// running ([`NOT_RUN_BEHIND`]).
pub fn not_run_behind(r: &mut Report, what: &str, because: &str) {
    r.cannot_run(format!("{what} — {because}{NOT_RUN_BEHIND}"));
}

/// Build what a stage drives, in the driver lane: `true` when it built. When it
/// did not, the build's own row and, behind it, a could-not-run row for `drive`
/// ([`not_run_behind`]); only one row when the machine refused the build.
fn build_to_drive(
    ctx: &Ctx,
    r: &mut Report,
    args: Vec<String>,
    build_label: String,
    drive: &str,
) -> bool {
    let build = exec::run(&driver_build_cmd(ctx, args), ctx.exec_env());
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

/// Run the rainbow trail laws' full grid. `aterm-effects`' case-family laws
/// (`tests/trail_host/mod.rs`) run only each family's spine (the cases either
/// side of each threshold) in a debug build, and every case with this variable
/// set. Read at run time, so the compile child needs no copy and nothing is
/// rebuilt.
pub const TRAIL_LAWS_FULL: (&str, &str) = ("TRAIL_LAWS_FULL", "1");

fn test(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test (no targo)");
        return;
    }
    let label = format!("targo test {}", ctx.scope.label());
    // The empty change-selection outranks the doc-driver verdict: a run that
    // compiles nothing needs no doc driver, and says why it ran nothing.
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
        // Not a skip (nothing was absent): the tests the compile kept from
        // running decided nothing.
        r.cannot_run(format!(
            "{run_label} — the compile above failed, and cargo runs no test after a compile error{}",
            NOT_RUN_BEHIND
        ));
    }
}

// ---------------------------------------------------------------------------
// The measuring rows ([`measuring_args`]), run exclusively because their
// verdicts depend on machine load, and never demoted, since the paint and spin
// rows launch the aterm they judge. The suites are handed the release bundle
// (`--app <APP>`): the bytes the release signs, never a build of the run's own.
// ---------------------------------------------------------------------------

/// Why a `measure` stage handed no bundle ran nothing, and the fix.
pub const NO_APP: &str = "(no release bundle to judge — fix: pass --app <path to aterm.app>)";

/// The paint and spin suites' prebuilt-artifact overrides, both set to the
/// handed bundle's `Contents/MacOS/aterm` ([`crate::MeasureArtifact::bin`]).
const RELEASE_PAINT_BIN: &str = "ATERM_PAINT_BIN";
const RELEASE_SPIN_BIN: &str = "ATERM_SPIN_BIN";

/// The measuring stage's test child, [`measuring_args`] handed the release
/// bundle's `aterm` through both overrides, and its ladder label.
fn measuring_cmd(ctx: &Ctx, artifact: &crate::MeasureArtifact) -> (String, Cmd) {
    let args = measuring_args();
    let label = format!("targo {}", args[1..].join(" "));
    let bin = artifact.bin("aterm");
    let cmd = targo(ctx, args)
        .env(RELEASE_PAINT_BIN, bin.as_os_str())
        .env(RELEASE_SPIN_BIN, bin.as_os_str());
    (label, cmd)
}

fn measuring_tests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("measuring tests (no targo)");
        return;
    }
    // Never a binary of the run's own choosing: the bundle publication signs.
    let Some(artifact) = &ctx.measure_artifact else {
        r.skip(format!("measuring tests {NO_APP}"));
        return;
    };
    let (label, cmd) = measuring_cmd(ctx, artifact);
    let env = crate::findings::rerun_env(&cmd, ctx.test_threads);
    run_child(ctx, r, &label, &cmd, |r, run, label| {
        decide_measuring_child(r, run, label, &env);
    });
}

/// A measuring child decides as any test child does, except that one which
/// exited 0 with a target ([`MEASURING_TARGETS`]) that passed no row measured
/// nothing there: a filter that selects nothing (a renamed module, a lost
/// `--ignored`) is could not run, never a green measure check. Read per target,
/// so the other target's passes cannot cover it.
fn decide_measuring_child(
    r: &mut Report,
    run: &exec::Run,
    label: &str,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) {
    if run.ok {
        let by_target = crate::libtest::passed_by_target(&run.output);
        let empty: Vec<String> = MEASURING_TARGETS
            .iter()
            .map(|t| format!("tests/{t}.rs"))
            .filter(|want| {
                !by_target
                    .iter()
                    .any(|(target, passed)| target == want && passed.is_some_and(|n| n > 0))
            })
            .collect();
        if !empty.is_empty() {
            r.cannot_run(format!(
                "{label} — {}: {}",
                empty.join(", "),
                crate::libtest::PASSED_NONE
            ));
            return;
        }
    }
    r.decide_test_child(run, label, env);
}

// ---------------------------------------------------------------------------
// Doctests
// ---------------------------------------------------------------------------
fn doctests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test --doc (no targo)");
        return;
    }
    let label = format!("targo test --doc {}", ctx.scope.label());
    // The empty change-selection outranks every other verdict.
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} {EMPTY_CONE}"));
        return;
    }
    // `cargo test --doc` is an error ("no library targets found") when no
    // selected package has a lib, which is ordinary for a cone or `-p aterm-lab`
    // (bin-only): a named skip.
    if !scope_compiles_doctests(ctx) {
        r.skip(format!(
            "targo test --doc (no package in scope has a library target: {})",
            ctx.scope.label()
        ));
        return;
    }
    let cmd = targo(ctx, doctest_args(&ctx.scope));
    match doc_driver(ctx) {
        DocDriver::Stage2 => {
            run_tests_labeled(
                ctx,
                r,
                &format!("{label} (trustdoc)"),
                &with_trustdoc(ctx, cmd),
            );
        }
        DocDriver::Ambient => {
            run_tests_labeled(ctx, r, &format!("{label} (caller's RUSTDOC)"), &cmd);
        }
        DocDriver::BarePath => {
            run_tests_labeled(ctx, r, &label, &cmd);
        }
        // The only doctest runner, so the diagnosis lives here: no doc driver
        // means no doctest was decided.
        DocDriver::Absent => r.cannot_run(ctx.tools.missing_trustdoc_label()),
    }
}

/// The sealed rung's one child, labelled, in the driver lane, where cargo
/// builds the `aterm` it drives as the suites' own binary dependency.
#[must_use]
pub fn sealed_lane_cmd(ctx: &Ctx) -> (String, Cmd) {
    (
        "targo test -p aterm --features sealed --test two_nodes_sealed --test fabric_world"
            .to_string(),
        in_lane(ctx, Lane::DriverTarget, targo(ctx, sealed_lane_args())),
    )
}

fn sealed_lane(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("sealed fabric lane (no targo)");
        return;
    }
    let (label, cmd) = sealed_lane_cmd(ctx);
    run_labeled(ctx, r, &label, &cmd);
}

// ---------------------------------------------------------------------------
// Tippy: the lint under the Trust toolchain's Clippy fork. Present means
// `-D warnings` must pass; absent is a named skip. Its own target dir
// (`<target>/verify/tippy`) lets it compile beside the test build.
// ---------------------------------------------------------------------------
/// The exact command the lint stage spawns, so the test asserts on it rather
/// than a replica.
fn tippy_cmd(ctx: &Ctx, bin: &std::path::Path, args: Vec<String>) -> Cmd {
    let dir = bin.parent().unwrap_or(&ctx.tools.stage2_dir).to_path_buf();
    let mut path = std::ffi::OsString::from(dir.as_os_str());
    path.push(":");
    path.push(&ctx.path_env);
    Cmd::new(bin)
        .args(args)
        .env("PATH", path)
        .env(
            "CARGO_TARGET_DIR",
            lane_dir(ctx, Lane::TippyTarget).unwrap_or_else(|| ctx.target_dir.clone()),
        )
        .env("TRUST_NO_MIGRATE_WARN", "1")
        // A lint only compiles.
        .demoted()
}

fn tippy(ctx: &Ctx, r: &mut Report) {
    let Some(bin) = ctx.tools.tippy.clone() else {
        r.skip(ctx.tools.missing_tippy_label());
        return;
    };
    run_scoped(
        ctx,
        r,
        &format!("tippy {} -D warnings", ctx.scope.label()),
        &tippy_cmd(ctx, &bin, tippy_args(&ctx.scope)),
    );
    // The second pass, under `release` only, in its own row: the targets whose
    // `required-features` are off ([`gated_lint_features`]; among them the
    // sealed rung, which also switches on aterm-link's `sealed` arms) are
    // linted here or nowhere.
    if ctx.release
        && let Some(args) = tippy_gated_args(&ctx.scope, &gated_lint_features(&ctx.root))
    {
        run_scoped(
            ctx,
            r,
            "tippy required-features targets -D warnings",
            &tippy_cmd(ctx, &bin, args),
        );
    }
}

// ---------------------------------------------------------------------------
// The in-process walks: the censuses and the source guards
// (`aterm_census::CENSUSES`), each over the checkout, its finding printed
// under the row that failed.
// ---------------------------------------------------------------------------

/// The file every aterm checkout has: the in-process checks read the tree's
/// own sources and policy files, and nothing elsewhere.
const CHECKOUT_MARKER: &str = "crates/aterm-gui/Cargo.toml";

/// Is the run's root an aterm checkout? A NOT RUN row naming `what` when not.
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

/// Run the `names` walks of `aterm_census::CENSUSES` over the checkout for
/// `check`: one row each, and NOT RUN for a name the library no longer has.
fn walks(ctx: &Ctx, r: &mut Report, check: &str, names: &[&str]) {
    if !in_checkout(ctx, r, check) {
        return;
    }
    let selected: Vec<&aterm_census::Census> = aterm_census::CENSUSES
        .iter()
        .filter(|c| names.contains(&c.name))
        .collect();
    for gone in names
        .iter()
        .filter(|n| !selected.iter().any(|c| c.name == **n))
    {
        r.cannot_run(format!("{check} — aterm-census has no walk named `{gone}`"));
    }
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

/// The source guards: the rules no type, visibility or clippy.toml row can
/// hold (a call graph, a literal's shape, a licence header).
fn guards(ctx: &Ctx, r: &mut Report) {
    walks(ctx, r, "guards", &["guards"]);
}

// ---------------------------------------------------------------------------
// Export guards: `ship check --export`, the publication engine's own `pub stage
// --dry-run` over a scratch clone of HEAD, the export step `ship release` runs.
// It judges only a commit on origin/main and needs the network, the engine's
// checkout, gitleaks and rustup, so it is in no set: a named check to run
// before asking for a release.
// ---------------------------------------------------------------------------

/// The export check's row.
pub const EXPORT_LABEL: &str = "ship check --export";

/// `ship check --export` from the root, built in the trust lane beside the
/// same release tools the trust lane runs.
#[must_use]
pub fn export_cmd(ctx: &Ctx) -> Cmd {
    in_lane(
        ctx,
        Lane::TrustTarget,
        targo(ctx, ship_args(["check", "--export"])),
    )
}

/// The `fix:` lines a transcript printed, joined: what a skip row shows. Only
/// shown, never read for the verdict, which is the exit code.
fn fixes(transcript: &str) -> Option<String> {
    let fixes: Vec<&str> = transcript
        .lines()
        .filter_map(|l| l.trim().strip_prefix("fix: "))
        .collect();
    (!fixes.is_empty()).then(|| fixes.join("; "))
}

/// How `ship check --export` answers, by its exit code: `0` the engine's guards
/// pass HEAD's export; `1` a guard or the engine refuses the tree, and `2` HEAD
/// names no release, both findings; `3` nothing refused but it could not run
/// here (no network, engine, gitleaks or rustup, or HEAD not on origin/main), a
/// named skip; `130` or no status, interrupted. Any other code is the release
/// tools failing to build: a finding.
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

// ---------------------------------------------------------------------------
// The trust lane: `ship trust run -p <lib>…` (aterm-release's `trust_lane.rs`),
// i.e. `targo trust check <lib> --lib --allow-l0-gaps` per library, each held
// by the release's own judge to its floor in
// `crates/aterm-release/trust/floors.tsv`.
//
// Which libraries: those (and the vendored forks with a floor) whose own files
// the selection touched (`-p` names them, the cone diffs against its base,
// `--all` against `origin/main`), never their dependents, and from a diff never
// one this clone measured at 10 CPU-minutes or more (the lane's cost history):
// those belong to the release, which runs the lane `--whole-tree`, or to a `-p`
// that names the library.
// ---------------------------------------------------------------------------

/// What the trust lane verifies in one run ([`trust_selection`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustSelection {
    /// These libraries; `why` says where they came from.
    Crates { crates: Vec<String>, why: String },
    /// Nothing to verify, and why — a decision, recorded as a pass.
    Nothing(String),
    /// The selection could not be computed — a skip, named, never a pass.
    Undecided(String),
}

/// The pure decision behind [`TrustSelection`]. `touched` answers which
/// libraries the change touched against a ref ([`crate::changed::touched_since`]);
/// `shared` is the ref a whole-tree run diffs against; `has_lib` filters the
/// `-p` crates, which may be bin-only; `heavy` names a library a diff leaves
/// out ([`heavy_libraries`]), which a `-p` still verifies.
#[must_use]
pub fn trust_selection(
    scope: &Scope,
    has_lib: &dyn Fn(&str) -> bool,
    touched: &dyn Fn(&str) -> Result<crate::changed::Touched, String>,
    shared: &str,
    heavy: &dyn Fn(&str) -> bool,
) -> TrustSelection {
    let from_diff = |base: &str| match touched(base) {
        Err(why) => TrustSelection::Undecided(format!(
            "could not tell which libraries the change touched against {base} ({why})"
        )),
        Ok(t) => {
            let short: String = t.merge_base.chars().take(9).collect();
            let mut why = format!(
                "the libraries changed since {} (merge-base {short})",
                t.base
            );
            if let Some(p) = &t.replanned {
                why.push_str(&format!(
                    "; {p} changed too (the release run verifies every library)"
                ));
            }
            // A library whose change is confined to its tests, benches or
            // examples (its other targets, or a cfg(test) `*_tests.rs` module)
            // has the MIR it had ([`crate::changed::lib_inputs`]); the row says so.
            let mut left_out = if t.tests_only.is_empty() {
                String::new()
            } else {
                format!(
                    "; left out, only their tests, benches or examples changed: {}",
                    t.tests_only.join(" ")
                )
            };
            let (heavy, libraries): (Vec<String>, Vec<String>) =
                t.libraries.into_iter().partition(|lib| heavy(lib));
            if !heavy.is_empty() {
                left_out.push_str(&format!(
                    "; left out, 10 CPU-minutes or more each (the release verifies them, \
                     `-p <lib>` here): {}",
                    heavy.join(" ")
                ));
            }
            if libraries.is_empty() {
                TrustSelection::Nothing(format!("{why}: none{left_out}"))
            } else {
                why.push_str(&left_out);
                TrustSelection::Crates {
                    crates: libraries,
                    why,
                }
            }
        }
    };
    match scope {
        Scope::Packages(ps) => {
            let crates: Vec<String> = ps.iter().filter(|c| has_lib(c)).cloned().collect();
            let named = ps
                .iter()
                .map(|c| format!("-p {c}"))
                .collect::<Vec<_>>()
                .join(" ");
            if crates.is_empty() {
                TrustSelection::Nothing(format!("{named}: no library"))
            } else {
                TrustSelection::Crates { crates, why: named }
            }
        }
        Scope::Changed(ch) => from_diff(&ch.base),
        Scope::Workspace => from_diff(shared),
    }
}

/// The CPU seconds at which a library's last measured lane check makes it
/// the release's, not a diff's ([`heavy_libraries`]).
pub const TRUST_HEAVY_CPU_SECS: f64 = 600.0;

/// The libraries the lane's cost history (`<name> <CPU s> <wall s> …` rows,
/// `#` comments) measured at [`TRUST_HEAVY_CPU_SECS`] or more.
#[must_use]
pub fn heavy_libraries(costs: &str) -> std::collections::BTreeSet<String> {
    costs
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut cols = l.split('\t');
            let name = cols.next()?.trim();
            let cpu: f64 = cols.next()?.trim().parse().ok()?;
            (!name.is_empty() && cpu >= TRUST_HEAVY_CPU_SECS).then(|| name.to_string())
        })
        .collect()
}

/// This clone's lane cost history, `<git common dir>/aterm-release/trust-costs.tsv`
/// (what `ship trust run` writes); empty when there is none yet, so a first run
/// leaves nothing out.
fn trust_costs(ctx: &Ctx) -> String {
    let common = crate::changed::git(&ctx.root, &ctx.path_env)
        .args(["rev-parse", "--git-common-dir"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| ctx.root.join(String::from_utf8_lossy(&o.stdout).trim()));
    common
        .and_then(|dir| std::fs::read_to_string(dir.join("aterm-release/trust-costs.tsv")).ok())
        .unwrap_or_default()
}

/// The lane's wall budget per library: under the run's three-hour child
/// ceiling ([`exec::DEFAULT_CHILD_CEILING`]), so an OVER-BUDGET row lands
/// before the ceiling ends the run.
pub const TRUST_CRATE_BUDGET_SECS: u32 = 5400;

/// The lane's command: the release tools built in the trust lane's dir, its
/// checks' slots under `slots/` there, and the prover the run resolved (the
/// lane reads `TRUST_STAGE2_BIN`, which every child is handed). Demoted, and
/// ended with `SIGTERM` first: the lane's handler ends each check's whole
/// process tree, which a bare `SIGKILL` would orphan.
#[must_use]
pub fn trust_lane_cmd(ctx: &Ctx, only: &[String]) -> Cmd {
    let lane = lane_dir(ctx, Lane::TrustTarget).unwrap_or_else(|| ctx.state_dir().join("trust"));
    let mut verb: Vec<String> = vec!["trust".into(), "run".into()];
    for lib in only {
        verb.push("-p".into());
        verb.push(lib.clone());
    }
    verb.push("--crate-budget".into());
    verb.push(TRUST_CRATE_BUDGET_SECS.to_string());
    verb.push("--target-root".into());
    verb.push(lane.join("slots").display().to_string());
    in_lane(ctx, Lane::TrustTarget, targo(ctx, ship_args(verb)))
        .demoted()
        .term_grace(exec::TERM_GRACE)
}

/// The line a transcript printed after `prefix`, last one first: what a row
/// names as its reason. Only shown, never read for the verdict.
fn said<'a>(transcript: &'a str, prefix: &str) -> Option<&'a str> {
    transcript
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix(prefix))
        .map(str::trim)
        .filter(|l| !l.is_empty())
}

/// How `ship trust run` answers, by its exit code: `0` every verified library at
/// or above its floor; `1` one fell below it, had none or broke, a finding whose
/// rows the judge printed; `2` nothing decided (the lane could not run, or
/// trustd or the clock left libraries undecided); `4` narrowed by memory (this
/// host cannot verify every selected library), a named skip; `130` or no
/// status, interrupted. Any other code is the release tools failing to build: a
/// finding.
#[must_use]
pub fn trust_outcome(code: Option<i32>, scope: &str, transcript: &str) -> (Outcome, String) {
    match code {
        Some(crate::exit::PASS) => (
            Outcome::Ok,
            format!("trust lane: every verified library at or above its floor ({scope})"),
        ),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            format!("trust lane: FAILED — the judge's rows above name each library ({scope})"),
        ),
        Some(2) => (
            Outcome::Fail(Severity::CouldNotRun),
            format!(
                "trust lane: nothing decided — {} ({scope})",
                said(transcript, "trust: COULD NOT RUN — ")
                    .unwrap_or("its COULD NOT RUN line above says why")
            ),
        ),
        Some(4) => (
            Outcome::Skip,
            format!(
                "trust lane: NARROWED BY MEMORY — this host cannot verify every selected \
                 library; its MEMORY lines above name the RAM that can (fix: run it on such a \
                 host) ({scope})"
            ),
        ),
        Some(130) | None => (
            Outcome::Fail(Severity::CouldNotRun),
            format!("trust lane: interrupted ({scope})"),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("trust lane: exit {c} (the release tools did not run) ({scope})"),
        ),
    }
}

fn trust_lane(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("trust lane (no targo)");
        return;
    }
    let shared = crate::changed::default_base(&ctx.root, &ctx.path_env);
    let has_lib = |c: &str| crate::changed::crate_dir_has_lib(&ctx.root, c);
    let touched = |b: &str| crate::changed::touched_since(&ctx.root, &ctx.tools, &ctx.path_env, b);
    let heavy = heavy_libraries(&trust_costs(ctx));
    let is_heavy = |c: &str| heavy.contains(c);
    let (only, scope) = match trust_selection(&ctx.scope, &has_lib, &touched, &shared, &is_heavy) {
        TrustSelection::Nothing(why) => {
            r.pass(format!("trust lane: nothing to verify — {why}"));
            return;
        }
        TrustSelection::Undecided(why) => {
            r.skip(format!("trust lane: {why}"));
            return;
        }
        TrustSelection::Crates { crates, why } => {
            let scope = format!("{}: {}", why, crates.join(" "));
            (crates, scope)
        }
    };
    let out = exec::run(&trust_lane_cmd(ctx, &only), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = trust_outcome(out.code, &scope, &out.output);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

// ---------------------------------------------------------------------------
// First-party libc ABI oracle, for this host's native cell
// (`crates/aterm-libc-oracle`): the cell's const/layout/type assertions compiled
// and its runtime checks run, the shim's public shape and every extern's linker
// symbol held to the reference libc's, and planted defects the oracle must
// catch, all under the Trust sysroot. Other cells are decided where they are
// native; a cell no machine hosts is decided by no check, as the oracle's
// report says. The oracle's separate workspace keeps `[patch.crates-io]` from
// rewriting the reference libc edge to the shim under test.
// ---------------------------------------------------------------------------
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

// ---------------------------------------------------------------------------
// The L0 censuses: main-loop reach, lock order, wasm reach, scope cardinality
// and lazy-init reentrancy, each a lexical walk that fails with the site it
// found ([`L0_CENSUSES`]).
// ---------------------------------------------------------------------------
fn census(ctx: &Ctx, r: &mut Report) {
    walks(ctx, r, "census", &L0_CENSUSES);
}

// ---------------------------------------------------------------------------
// The Trust checkers over the formal models: `spec-check` holds every registered
// model to `ty`, every `ay` certificate to its declared verdict and the registry
// to `trust-ir` spec-link. Plain tests are interpreter-only by design, so this is
// the one place those tools are asked.
// ---------------------------------------------------------------------------

/// `targo --unverified run -q -p aterm-spec --features anchors --bin spec-check`,
/// in the caller's target dir once the test compile has built its library (the
/// bin's `required-features`: the model catalog sits behind `anchors`, and a
/// `run` does not unify the dev self-row that turns it on for tests);
/// under `release` with `-- --full`, which adds the minutes-class `ay` row.
#[must_use]
pub fn spec_check_cmd(ctx: &Ctx) -> Cmd {
    let mut args: Vec<String> = [
        "--unverified",
        "run",
        "-q",
        "-p",
        "aterm-spec",
        "--features",
        "anchors",
        "--bin",
        "spec-check",
    ]
    .map(String::from)
    .into();
    if ctx.release {
        args.extend(["--".to_string(), "--full".to_string()]);
    }
    targo(ctx, args)
}

/// Why a `spec-check` that decided nothing did not run: the machine's failure, else
/// its own `could not run` line (a missing `ty`, `ay` or `trust-ir` names its install).
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
    let label = "spec-check (every registered model under ty, the ay certificates, trust-ir)";
    let out = exec::run(&spec_check_cmd(ctx), ctx.exec_env());
    r.raw(out.output.as_str());
    // spec-check's exits are the libc oracle's: 0 pass, 1 a finding, 3 a checker
    // or input missing.
    let outcome = libc_oracle_outcome(&out);
    let why = (outcome == Outcome::Fail(Severity::CouldNotRun))
        .then(|| spec_check_not_run_reason(&out))
        .flatten();
    match why {
        Some(why) => r.cannot_run(format!("{label} — {why}")),
        None => r.record_child(&out, outcome, label),
    }
}

/// The hand-written specs' Tier-1 conformances: each drives real code against a
/// `.tla` only `ty` reads, so each is `#[ignore]`d and named `…_under_ty`, and
/// panics (VERIFICATION GATE) where `ty` is missing. `(package, test)`.
pub const TY_ONLY_CONFORMANCES: [(&str, &str); 4] = [
    (
        "aterm-core",
        "real_altscreen_roundtrip_conforms_to_altscreen_spec_under_ty",
    ),
    (
        "aterm-gpu",
        "real_gpu_encode_slice_decision_conforms_to_gpuencode_spec_under_ty",
    ),
    (
        "aterm-pty",
        "real_write_all_offset_trajectory_conforms_to_writeall_spec_under_ty",
    ),
    (
        "aterm-gui",
        "real_path_confine_conforms_to_pathconfine_spec_under_ty",
    ),
];

/// `targo --unverified test -p <each TY_ONLY_CONFORMANCES package> --no-fail-fast
/// --tests`, then `--no-run` (the compile row) or `-- --ignored _under_ty` (the
/// run). Both name one graph, so the run compiles nothing more.
#[must_use]
pub fn ty_conformance_args(compile: bool) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "test".to_string()];
    for (package, _) in TY_ONLY_CONFORMANCES {
        a.extend(["-p".to_string(), package.to_string()]);
    }
    a.extend(["--no-fail-fast", "--tests"].map(String::from));
    if compile {
        a.push("--no-run".to_string());
    } else {
        a.extend(["--", "--ignored", "_under_ty"].map(String::from));
    }
    a
}

/// The [`TY_ONLY_CONFORMANCES`] a passing run's log has no `ok` verdict for: a
/// renamed or lost test decided nothing, so it is never a green row.
#[must_use]
pub fn ty_conformances_not_passed(log: &str) -> Vec<&'static str> {
    TY_ONLY_CONFORMANCES
        .iter()
        .map(|(_, test)| *test)
        .filter(|test| {
            !log.lines().any(|l| {
                l.strip_prefix("test ")
                    .and_then(|rest| rest.split_once(" ... "))
                    .is_some_and(|(name, verdict)| {
                        (name == *test || name.ends_with(&format!("::{test}")))
                            && verdict.starts_with("ok")
                    })
            })
        })
        .collect()
}

fn ty_conformances(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("ty-only conformances (no targo)");
        return;
    }
    let label = "targo test -- --ignored _under_ty (the ty-only conformances)";
    let compile = targo(ctx, ty_conformance_args(true)).demoted();
    let out = exec::run(&compile, ctx.exec_env());
    if !out.ok {
        r.raw(out.output.as_str());
        r.fail_child(&out, format!("{label} --no-run"));
        if out.environment_failure().is_none() {
            not_run_behind(r, label, "the compile above failed");
        }
        return;
    }
    let run = targo(ctx, ty_conformance_args(false));
    let env = crate::findings::rerun_env(&run, ctx.test_threads);
    run_child(ctx, r, label, &run, |r, out, label| {
        let missing = if out.ok {
            ty_conformances_not_passed(&out.output)
        } else {
            Vec::new()
        };
        if missing.is_empty() {
            r.decide_test_child(out, label, &env);
        } else {
            r.cannot_run(format!(
                "{label} — {}: {}",
                missing.join(", "),
                crate::libtest::PASSED_NONE
            ));
        }
    });
}

// ---------------------------------------------------------------------------
// Driver builds: the binaries the tail drives, compiled in
// `<target>/verify/drivers` before the exclusive section. Every driver stage
// still runs its own build argv (a fingerprint no-op after these) and drives
// only what that build left, so no stage takes its binary on a prebuild's word.
//
// Two rows. `driver builds`, the lane's first row at t0, builds the smokes'
// `aterm-gui`/`aterm-ctl`. `tail driver builds`, the lane's last row before the
// barrier, builds what only the rows behind the barrier drive
// ([`tail_driver_build_cmds`]); the test run waits for the first, not the
// second ([`crate::plan::NOT_AWAITED`]).
//
// None of this can come from the test compile's `target/`: `targo test
// --workspace` unifies every member's dev-dependency features, while the driven
// binaries are the per-package variants the smokes and drives exist to exercise.
// ---------------------------------------------------------------------------

/// `targo --unverified build -q -p <package> --example <each>`: every
/// [`OBJC_DRIVES`] row in one cargo invocation, whose graph is the union of the
/// single-example graphs.
#[must_use]
pub fn objc_driver_prebuild_args() -> Vec<String> {
    let mut a: Vec<String> = ["--unverified", "build", "-q"]
        .into_iter()
        .map(String::from)
        .collect();
    for drive in &OBJC_DRIVES {
        if !a.iter().any(|x| x == drive.package) {
            a.push("-p".to_string());
            a.push(drive.package.to_string());
        }
    }
    for drive in &OBJC_DRIVES {
        a.push("--example".to_string());
        a.push(drive.example.to_string());
    }
    a
}

/// The driver-builds row's one child: the two smoke binaries.
#[must_use]
pub fn driver_build_cmds(ctx: &Ctx) -> Vec<(String, Cmd)> {
    vec![(
        "driver prebuild: targo build -p aterm-gui -p aterm-ctl".to_string(),
        driver_build_cmd(ctx, smoke_build_args()),
    )]
}

/// The tail driver builds row's children, labelled, each driven only behind the
/// first barrier: the lab and the `aterm` the Codex lane drives (macOS, under
/// `codex`), and under `appkit` the redraw harness and the objc drivers
/// (macOS). Under `--no-windows` the last two are left out
/// ([`TAIL_WINDOWED_BUILDS`]): their stages are [`crate::plan::WINDOWED`] and
/// skip before building, while the Codex lane drives its `aterm` headless.
#[must_use]
pub fn tail_driver_build_cmds(ctx: &Ctx) -> Vec<(String, Cmd)> {
    use crate::checks::Check;
    let mac = cfg!(target_os = "macos");
    let windows = !ctx.no_windows && ctx.checks.contains(&Check::Appkit);
    let mut v = Vec::new();
    if mac && ctx.checks.contains(&Check::Codex) {
        v.push((
            "driver prebuild: targo build -p aterm -p aterm-lab (the Codex lane's binaries)"
                .to_string(),
            codex_aterm_build_cmd(ctx),
        ));
    }
    if windows {
        v.push((
            format!("driver prebuild: targo build --bin {REDRAW_CONFORMANCE_BIN}"),
            driver_build_cmd(ctx, redraw_conformance_build_args()),
        ));
    }
    if mac && windows {
        v.push((
            "driver prebuild: targo build --example (the objc drivers)".to_string(),
            driver_build_cmd(ctx, objc_driver_prebuild_args()),
        ));
    }
    v
}

/// What `--no-windows` leaves out of the tail driver builds, as the row's
/// note says it ([`tail_driver_build_cmds`]): off macOS the objc drivers are
/// never built, so the flag leaves out only the redraw harness.
pub const TAIL_WINDOWED_BUILDS: &str = if cfg!(target_os = "macos") {
    "not built under --no-windows: the redraw harness and the objc drivers"
} else {
    "not built under --no-windows: the redraw harness"
};

fn driver_builds(ctx: &Ctx, r: &mut Report) {
    run_driver_builds(ctx, r, "driver builds", driver_build_cmds(ctx));
}

fn tail_driver_builds(ctx: &Ctx, r: &mut Report) {
    if ctx.no_windows {
        r.raw(format!("  {TAIL_WINDOWED_BUILDS}"));
    }
    run_driver_builds(ctx, r, "tail driver builds", tail_driver_build_cmds(ctx));
}

fn run_driver_builds(ctx: &Ctx, r: &mut Report, row: &str, cmds: Vec<(String, Cmd)>) {
    if !ctx.tools.have_targo() {
        r.skip(format!("{row} (no targo)"));
        return;
    }
    // Every child, even after a failure: they are independent builds, and the
    // row that drives each one reports its own build either way.
    for (label, cmd) in cmds {
        run_labeled(ctx, r, &label, &cmd);
    }
}

// ---------------------------------------------------------------------------
// Control redraw conformance: the one check that can see a control-socket
// `select` repaint a window. `EventLoop` construction panics off the main thread
// and libtest runs tests on spawned threads, so aterm-gui's unit tests build
// their host with `proxy: None` and cannot catch a dropped `EventLoopProxy`.
// `aterm-redraw-conformance` owns `fn main` and holds a real proxy; it is behind
// `required-features`, so only this stage builds it.
// ---------------------------------------------------------------------------

/// How the ladder reads one harness exit code (`0` pass / `1` fail / `2` not
/// run, declared in aterm-gui's `control_redraw_conformance.rs`).
///
/// `2` (no event loop constructible here: headless, no display) is
/// COULD-NOT-RUN, never green. Any other code is a finding: the harness drives
/// shipped code, so dying is something the tree did.
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
        // The test compile already reported COULD-NOT-RUN for the same absence;
        // naming it again here keeps the skip counted and the verdict narrowed.
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
    let bin = driver_bin(ctx, REDRAW_CONFORMANCE_BIN);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "redraw conformance: just-built harness missing ({})",
            bin.display()
        ));
        return;
    }
    // Driven as a binary, never `targo run`: the driver's lane banner goes to
    // stderr and cargo's own codes would collide with the harness's 0/1/2.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = redraw_outcome(out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

// ---------------------------------------------------------------------------
// The objc drives: the ported classes and the tab strip, driven on a real
// AppKit, one row per [`OBJC_DRIVES`] entry. Each checks what no unit test can
// reach (libtest cannot host AppKit).
// ---------------------------------------------------------------------------

fn objc_drives(ctx: &Ctx, r: &mut Report) {
    if !cfg!(target_os = "macos") {
        // Not a skip for lack of a tool: every driven class is declared inside
        // `#[cfg(target_os = "macos")]` and does not exist here at all.
        r.skip("objc drives (macOS only: the driven classes are macOS ones)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("objc drives (no targo)");
        return;
    }
    // Every row, even after a failure: each is its own build and its own
    // verdict, and one red must not hide the rest.
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
    let bin = driver_example(ctx, name);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "{name}: just-built driver missing ({})",
            bin.display()
        ));
        return;
    }
    // Driven as a binary, never `targo run`: cargo's lane banner would land in
    // the transcript, its exit codes would collide with the driver's, and the
    // event drive's signal death (the finding) would become 101.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = objc_drive_outcome(drive, out.code);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

// ---------------------------------------------------------------------------
// The Codex live upgrade: `lab codex-upgrade` moves a real Codex from the
// managed store's older build to its current one under a private headless
// instance of the `aterm` binary, sandboxed to one loopback port. It is the
// `codex` check, run alone and by hand: it reads this machine's managed store
// and the vendor's current Codex, so it decides a different thing on each
// machine and day.
//
// The stage builds both binaries itself (`-p aterm -p aterm-lab` into the
// driver lane, a fingerprint no-op after the tail driver builds row), runs the
// lane only if that build succeeded, and hands the lab that `aterm`, so it
// never falls back to a stale `target/`. The lane's NOT RUN is never a pass
// ([`codex_live_upgrade_outcome`]).
// ---------------------------------------------------------------------------

/// `targo --unverified build -q -p aterm -p aterm-lab --bin aterm --bin
/// aterm-lab`: the lab and the one binary it drives.
#[must_use]
pub fn codex_aterm_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
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

/// The label of the Codex stage's first child — the build its lane depends on.
pub const CODEX_ATERM_BUILD_LABEL: &str =
    "targo build -p aterm -p aterm-lab (the lab and the aterm the Codex lane drives)";

/// The Codex lane, as the ladder names it.
pub const CODEX_LANE: &str = "lab codex-upgrade";

/// The `SIGTERM` grace a lane over the hang ceiling gets: it cleans up at its
/// next wait, and a `ctl turn` already in flight can take 60-90 s; a `SIGKILL`
/// before that leaves Codex daemons that have left its process group.
pub const CODEX_TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(90);

/// The build of both binaries, in the driver lane. Compile-only, so demoted;
/// the lane itself runs code and keeps the inherited tier.
#[must_use]
pub fn codex_aterm_build_cmd(ctx: &Ctx) -> Cmd {
    driver_build_cmd(ctx, codex_aterm_build_args())
}

/// The binary the lane is handed: the driver lane's own `debug/aterm`, which
/// the stage's first child writes. Never `<root>/target/debug/aterm`.
#[must_use]
pub fn codex_aterm_binary(ctx: &Ctx) -> std::path::PathBuf {
    driver_bin(ctx, "aterm")
}

/// `<drivers>/debug/aterm-lab codex-upgrade --aterm <drivers>/debug/aterm`.
/// Anything else it needs it makes itself: a private socket, a scratch HOME,
/// and a clean environment around the instance it starts.
#[must_use]
pub fn codex_live_upgrade_cmd(ctx: &Ctx) -> Cmd {
    Cmd::new(driver_bin(ctx, "aterm-lab"))
        .term_grace(CODEX_TERM_GRACE)
        .arg("codex-upgrade")
        .arg("--aterm")
        .arg(codex_aterm_binary(ctx))
}

/// The reason the lab gave for not running: its `aterm-lab: NOT RUN: <why>`
/// line.
fn not_run_reason(transcript: &str) -> Option<&str> {
    said(transcript, "aterm-lab: NOT RUN:")
}

/// How the ladder reads the lab's exit: `0` pass, `1` a finding, `3` (this
/// machine lacks a prerequisite: no older managed Codex) a named skip with the
/// lab's reason, never a pass. `2` is a usage error in the argv this stage
/// built, and any other code a lane that died mid-way; both are findings. No
/// status decided nothing.
#[must_use]
pub fn codex_live_upgrade_outcome(code: Option<i32>, transcript: &str) -> (Outcome, String) {
    let name = CODEX_LANE;
    match code {
        Some(0) => (Outcome::Ok, name.to_string()),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: a check failed against the live aterm — its rows above say which"),
        ),
        Some(3) => (
            Outcome::Skip,
            format!(
                "{name}: NOT RUN — {} (this machine lacks a prerequisite; a named skip, never a \
                 pass)",
                not_run_reason(transcript).unwrap_or("the lab gave no reason")
            ),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: exit {c} (the lane answers 0 pass, 1 a finding, 3 NOT RUN)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            format!("{name}: no exit status — killed by a signal, or never spawned"),
        ),
    }
}

/// The `codex` check, in order: the platform, a toolchain, the build of both
/// binaries, the binaries, the lane.
fn codex_live_upgrade(ctx: &Ctx, r: &mut Report) {
    let name = CODEX_LANE;
    if !cfg!(target_os = "macos") {
        r.skip(format!(
            "{name} (macOS only: the instance runs under sandbox-exec, and the ps stand-in \
             reads the process table through libproc)"
        ));
        return;
    }
    if !ctx.tools.have_targo() {
        // Counted and named: with no toolchain nothing can build the binary,
        // and the lane's coverage is absent from the run.
        r.skip(format!(
            "{name} (no targo — nothing can build the aterm it drives)"
        ));
        return;
    }
    if !run_labeled(ctx, r, CODEX_ATERM_BUILD_LABEL, &codex_aterm_build_cmd(ctx)) {
        // Not a skip: running the lane would drive whatever binary an earlier
        // run left, and the lane it keeps from running decided nothing
        // ([`NOT_RUN_BEHIND`]).
        r.cannot_run(format!(
            "{name} — the build above failed, so this lane has no fresh aterm to drive{}",
            NOT_RUN_BEHIND
        ));
        return;
    }
    for bin in [codex_aterm_binary(ctx), driver_bin(ctx, "aterm-lab")] {
        if !is_executable_file(&bin) {
            r.cannot_run(format!(
                "{name}: the just-built {} is missing",
                bin.display()
            ));
            return;
        }
    }
    let out = exec::run(&codex_live_upgrade_cmd(ctx), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = codex_live_upgrade_outcome(out.code, &out.output);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

/// The `wasm-glue` check ([`crate::wasm_glue`]'s toy): aterm-wasm-abi's toy
/// module built on rustup's stable in the cells lane's directory, bound in
/// both flavors and held to its goldens, driven under node, and a planted
/// conversion bug the driver must catch. One row.
fn wasm_glue(ctx: &Ctx, r: &mut Report) {
    const LABEL: &str = "wasm-glue (the generator on aterm-wasm-abi's toy, under node)";
    let work = lane_dir(ctx, Lane::CellsTarget)
        .unwrap_or_else(|| ctx.state_dir().join("cells"))
        .join("wasm-glue");
    let (root, mut log) = (ctx.root.clone(), String::new());
    let run = exec::bounded(ctx.child_ceiling, move || {
        let run = crate::wasm_glue::toy::gate(&root, &work, false, &mut log);
        (run, log)
    });
    match run {
        Ok((run, log)) => {
            r.raw(log);
            match run.status {
                crate::perf::Status::Skip { why, fix } => {
                    r.skip(format!("{LABEL} — {why} (fix: {fix})"));
                }
                crate::perf::Status::Fail(findings) => {
                    r.fail(LABEL);
                    r.raw(findings.join("\n"));
                }
                crate::perf::Status::Pass => r.pass(LABEL),
                // The toy judges or refuses; any other answer is no verdict.
                _ => r.cannot_run(format!("{LABEL} — the toy answered no verdict")),
            }
        }
        Err(why) => r.cannot_run(format!("{LABEL} — the toy {}", why.sentence())),
    }
}

/// The `cells` check ([`crate::cells`]): the forge cells no fleet machine
/// hosts (or each `--cell`), each linted for its own triple on rustup's
/// toolchain (a STOCK EXCEPTION), one row per cell.
fn foreign_cells(ctx: &Ctx, r: &mut Report) {
    if in_checkout(ctx, r, "cells") {
        crate::cells::run(ctx, r);
    }
}

/// The `forge` check: the third-party surface policy
/// (`aterm_forge::check::run`, the function `targo --unverified forge check`
/// runs) — forks live and patched, carved paths absent, provenance attested,
/// the budget ratchet, the fork ledger. No compiler and no network; its
/// `targo tree` calls ask this run's own targo ([`Ctx::forge_cargo`]).
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

    fn ctx(scope: Scope) -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            scope,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    /// A fixture tree's `crates/*/Cargo.toml` — two gated packages (one
    /// target gated twice), one with none, one directory with no manifest —
    /// read by [`gated_lint_features`], as the lint reads the real one.
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

    // --- the trust lane ------------------------------------------------------

    fn touched(libraries: &[&str], replanned: Option<&str>) -> crate::changed::Touched {
        crate::changed::Touched {
            base: "origin/main".to_string(),
            merge_base: "0123456789abcdef".to_string(),
            libraries: libraries.iter().map(ToString::to_string).collect(),
            replanned: replanned.map(ToString::to_string),
            tests_only: Vec::new(),
        }
    }

    fn select(scope: &Scope, t: Result<crate::changed::Touched, String>) -> TrustSelection {
        let asked = std::cell::RefCell::new(None::<String>);
        let has_lib = |c: &str| c != "aterm-lab";
        let answer = |base: &str| {
            *asked.borrow_mut() = Some(base.to_string());
            t.clone().map(|mut t| {
                t.base = base.to_string();
                t
            })
        };
        let heavy = |c: &str| c == "aterm-gui";
        let got = trust_selection(scope, &has_lib, &answer, "origin/main", &heavy);
        // A -p run names its crates and must not pay for git; every other
        // shape reads the diff — against its own --base, or the shared branch.
        let want = match scope {
            Scope::Packages(_) => None,
            Scope::Changed(ch) => Some(ch.base.clone()),
            Scope::Workspace => Some("origin/main".to_string()),
        };
        assert_eq!(*asked.borrow(), want, "{scope:?}");
        got
    }

    /// `-p` names its own crates, libraries only; the cone and a whole-tree run
    /// verify the libraries (and admitted forks) the change touched: the seeds,
    /// never the reverse cone a test run needs.
    #[test]
    fn the_trust_lane_verifies_what_the_run_says_it_touched() {
        assert_eq!(
            select(&Scope::crate_only("aterm-grid"), Err("unasked".into())),
            TrustSelection::Crates {
                crates: vec!["aterm-grid".into()],
                why: "-p aterm-grid".into()
            }
        );
        assert!(matches!(
            select(&Scope::crate_only("aterm-lab"), Err("unasked".into())),
            TrustSelection::Nothing(why) if why.contains("no library")
        ));
        // The cone: what touched_since answers against its base — the seeds and
        // touched forks, not the cone (aterm-gui, which only depends on the grid).
        let changed = Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true);
        assert_eq!(
            select(&changed, Ok(touched(&["aterm-grid", "smol_str"], None))),
            TrustSelection::Crates {
                crates: vec!["aterm-grid".into(), "smol_str".into()],
                why: "the libraries changed since main (merge-base 012345678)".into()
            }
        );
        // The whole tree: the libraries touched against origin/main.
        assert_eq!(
            select(&Scope::workspace(), Ok(touched(&["aterm-types"], None))),
            TrustSelection::Crates {
                crates: vec!["aterm-types".into()],
                why: "the libraries changed since origin/main (merge-base 012345678)".into()
            }
        );
        assert!(matches!(
            select(&Scope::workspace(), Ok(touched(&[], None))),
            TrustSelection::Nothing(why) if why.ends_with(": none")
        ));
        // A library whose tests alone changed is named as left out, beside the
        // ones verified and when it is the only one.
        let tests_only = |libs: &[&str]| {
            let mut t = touched(libs, None);
            t.tests_only = vec!["aterm-verify".into()];
            Ok(t)
        };
        assert!(matches!(
            select(&Scope::workspace(), tests_only(&["aterm-types"])),
            TrustSelection::Crates { crates, why } if crates == ["aterm-types"]
                && why.ends_with("only their tests, benches or examples changed: aterm-verify")
        ));
        assert!(matches!(
            select(&Scope::workspace(), tests_only(&[])),
            TrustSelection::Nothing(why) if why.contains(": none; left out, only their tests")
        ));
        // A library this clone measured at 10 CPU-minutes or more is the
        // release's when a diff selects it, and named; a -p still verifies it.
        assert!(matches!(
            select(&Scope::workspace(), Ok(touched(&["aterm-gui", "aterm-types"], None))),
            TrustSelection::Crates { crates, why } if crates == ["aterm-types"]
                && why.ends_with("`-p <lib>` here): aterm-gui")
        ));
        assert!(matches!(
            select(&Scope::workspace(), Ok(touched(&["aterm-gui"], None))),
            TrustSelection::Nothing(why) if why.contains(": none; left out, 10 CPU-minutes")
        ));
        assert!(matches!(
            select(&Scope::crate_only("aterm-gui"), Err("unasked".into())),
            TrustSelection::Crates { crates, .. } if crates == ["aterm-gui"]
        ));
        assert_eq!(
            heavy_libraries(
                "# trust lane cost history\naterm-gui\t7200\t3000\t1\t2026-10-15\n\
                 aterm-types\t42.5\t40\t1\t2026-10-15\naterm-core\t600\t500\t1\t-\nbad row\n"
            )
            .into_iter()
            .collect::<Vec<_>>(),
            ["aterm-core", "aterm-gui"]
        );
        // A re-planning change is said, not widened into hours of verification.
        assert!(matches!(
            select(&Scope::workspace(), Ok(touched(&["aterm-types"], Some("Cargo.lock")))),
            TrustSelection::Crates { why, .. } if why.contains("Cargo.lock changed too")
        ));
        // An unreadable diff is a named skip, never a pass.
        assert!(matches!(
            select(&Scope::workspace(), Err("no merge-base".into())),
            TrustSelection::Undecided(why) if why.contains("no merge-base")
        ));
    }

    /// The lane's exit code is the verdict: only `0` passes, `1` is the judge's
    /// finding, `2` decided nothing and names the lane's own reason, `4` is a
    /// named skip naming its fix, and an interrupt or a death is never green.
    #[test]
    fn the_trust_outcome_is_the_lanes_exit_code() {
        let (o, l) = trust_outcome(Some(0), "-p aterm-grid", "");
        assert_eq!(o, Outcome::Ok);
        assert!(l.ends_with("(-p aterm-grid)"), "{l}");
        assert_eq!(
            trust_outcome(Some(1), "s", "").0,
            Outcome::Fail(Severity::GateFailed)
        );
        let (o, l) = trust_outcome(
            Some(2),
            "s",
            "trust: COULD NOT RUN — interrupted; nothing was decided\n",
        );
        assert_eq!(o, Outcome::Fail(Severity::CouldNotRun));
        assert!(l.contains("interrupted; nothing was decided"), "{l}");
        let (o, l) = trust_outcome(Some(4), "s", "");
        assert_eq!(o, Outcome::Skip);
        assert!(
            l.contains("NARROWED BY MEMORY") && l.contains("fix: "),
            "{l}"
        );
        for code in [Some(130), None] {
            assert_eq!(
                trust_outcome(code, "s", "").0,
                Outcome::Fail(Severity::CouldNotRun)
            );
        }
        assert_eq!(
            trust_outcome(Some(101), "s", "").0,
            Outcome::Fail(Severity::GateFailed)
        );
    }

    /// The lane runs the release tools' `trust run`, one `-p` per library, its
    /// slots in the trust lane's dir and its budget under the run's ceiling;
    /// demoted, and ended with `SIGTERM` first.
    #[test]
    fn the_trust_lane_command_names_its_libraries_and_its_lane() {
        let c = ctx(Scope::workspace());
        let cmd = trust_lane_cmd(&c, &["aterm-bits".into(), "aterm-grid".into()]);
        let mut want = ship_args(["trust", "run", "-p", "aterm-bits", "-p", "aterm-grid"]);
        want.extend(
            [
                "--crate-budget",
                "5400",
                "--target-root",
                "/repo/target/verify/trust/slots",
            ]
            .map(String::from),
        );
        assert_eq!(
            cmd.args,
            want.iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        );
        let env = |k: &str| {
            cmd.envs
                .iter()
                .rev()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(
            env("CARGO_TARGET_DIR").as_deref(),
            Some("/repo/target/verify/trust")
        );
        assert_eq!(env("CARGO_BUILD_JOBS").as_deref(), Some("4"));
        assert!(cmd.demoted);
        assert_eq!(cmd.term_grace, Some(exec::TERM_GRACE));
    }

    /// The measuring stage judges the bundle the run was handed:
    /// `ATERM_PAINT_BIN` and `ATERM_SPIN_BIN` are its `Contents/MacOS/aterm`,
    /// and no child of the stage builds a release `aterm`. A run handed no
    /// bundle measures nothing: a skip naming the flag, never a binary of the
    /// run's own choosing.
    #[test]
    fn the_measuring_stage_hands_paint_and_spin_the_bundle_it_was_handed() {
        let c = ctx(Scope::workspace());
        let artifact = crate::MeasureArtifact {
            app: PathBuf::from("/release/aterm.app"),
        };
        let env_of = |cmd: &Cmd, key: &str| {
            cmd.envs
                .iter()
                .find(|(k, _)| k.to_str() == Some(key))
                .map(|(_, v)| PathBuf::from(v))
        };
        let (_, run) = measuring_cmd(&c, &artifact);
        for var in ["ATERM_PAINT_BIN", "ATERM_SPIN_BIN"] {
            assert_eq!(
                env_of(&run, var),
                Some(PathBuf::from("/release/aterm.app/Contents/MacOS/aterm")),
                "{var}"
            );
        }
        assert!(
            !run.argv().iter().any(|a| a == "build"),
            "the stage runs the suites and builds nothing: {:?}",
            run.argv()
        );
        // No bundle: a skip, with the flag that hands one.
        let mut c = ctx(Scope::workspace());
        c.tools.targo = PathBuf::from("/usr/bin/false");
        c.tools.refused = None;
        let mut r = Report::new("measuring");
        measuring_tests(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(outcome, crate::Outcome::Skip, "{label}");
        assert!(label.contains("--app <path to aterm.app>"), "{label}");
    }

    /// The oracle runs from the checkout's root with both of its target dirs in
    /// the oracle's own per-checkout lane, absolute however the root was given.
    #[test]
    fn libc_oracle_runs_the_gate_in_its_own_lane_from_the_root() {
        let c = ctx(Scope::workspace());
        let cmd = libc_oracle_cmd(&c).expect("the absolute fixture root resolves");
        assert_eq!(
            cmd.argv()[1..],
            [
                "--unverified",
                "run",
                "-q",
                "-p",
                "aterm-libc-oracle",
                "--target-dir",
                "/repo/libc-oracle/target/gate-bin",
                "--",
                "--root",
                "/repo",
                "--target-dir",
                "/repo/libc-oracle/target",
            ]
        );
        assert_eq!(cmd.cwd.as_deref(), Some(std::path::Path::new("/repo")));
        assert!(cmd.envs.is_empty(), "{:?}", cmd.envs);

        let mut relative = ctx(Scope::workspace());
        relative.root = PathBuf::from("relative-repo");
        let cmd = libc_oracle_cmd(&relative).expect("a relative repo root can be absolutized");
        let argv = cmd.argv();
        let lane = PathBuf::from(argv.last().expect("the oracle's target dir"));
        assert!(lane.is_absolute(), "{lane:?}");
        assert!(
            lane.ends_with("relative-repo/libc-oracle/target"),
            "{lane:?}"
        );
        assert!(cmd.cwd.as_ref().is_some_and(|d| d.is_absolute()));
    }

    /// The oracle's own verdicts are read as given: 1 is a finding even beside
    /// a full disk, 3 and a signal decided nothing. Any other code is the
    /// machine's only when it ran out of disk.
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

    /// A libc oracle that decided nothing names why on its row: the machine's
    /// failure first, else the oracle's own last `COULD NOT RUN:` line.
    #[test]
    fn a_libc_gate_that_could_not_run_names_its_reason() {
        let run = |output: &str| exec::Run {
            ok: false,
            output: output.to_string(),
            code: Some(3),
            spawn_error: None,
        };
        let stopped = "libc oracle: aarch64-apple-darwin (cell darwin-aarch64), under Trust\n\
                       COULD NOT RUN: the reference libc crate is not in the cargo registry\n";
        assert_eq!(
            libc_oracle_not_run_reason(&run(stopped)).as_deref(),
            Some("the reference libc crate is not in the cargo registry")
        );
        let full = format!("{stopped}{}\n", exec::ENOSPC_SENTENCE);
        let why = libc_oracle_not_run_reason(&run(&full)).expect("a reason");
        assert!(!why.contains("reference libc"), "the disk outranks: {why}");
        assert_eq!(libc_oracle_not_run_reason(&run("killed\n")), None);
    }

    #[test]
    fn a_machine_with_no_doc_driver_anywhere_is_diagnosed_on_the_doctests_row() {
        // targo exists, trustdoc does not, and nothing on the children's PATH
        // answers to the bare name: the doctests stage says COULD-NOT-RUN with the
        // remedy before spawning anything, and the test stage's two children,
        // which spawn no rustdoc, both run (`/usr/bin/true` stands in for targo).
        let spec = |id| StageSpec {
            id,
            title: "t".into(),
            lane: crate::plan::Lane::MainTarget,
            exclusive: false,
            after_lanes: Vec::new(),
            after_stages: Vec::new(),
        };
        let no_driver = |scope| {
            let mut c = ctx(scope);
            c.tools.targo = PathBuf::from("/usr/bin/true");
            c.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
            c.path_env = "/nonexistent-dir".into();
            // These cases exec, so the child needs a real cwd.
            c.root = std::env::temp_dir();
            c.scratch = std::env::temp_dir();
            c
        };
        let cc = no_driver(Scope::workspace());

        let r = run_stage(&cc, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(
            outcomes,
            [
                (Outcome::Ok, "targo test --workspace --no-run"),
                (Outcome::Ok, "targo test --workspace --tests"),
            ]
        );

        let r = run_stage(&cc, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            outcomes[0].1.starts_with("no doc driver") && outcomes[0].1.contains("Fix: "),
            "the diagnosis names the remedy: {}",
            outcomes[0].1
        );

        // A cone with no lib target compiles no doctests, so the doctests stage
        // skips on its lib-target guard.
        let cb = no_driver(Scope::changed("main", vec!["aterm-lab".into()], false));
        let r = run_stage(&cb, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "bin-only doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip, "{}", outcomes[0].1);

        // `-p <crate>` answers the lib question from the member's own
        // manifest: a bin-only selection skips, and the same one with a lib is
        // a real diagnosis.
        let tmp = crate::mktemp_dir("atv-doc-scope").expect("mktemp");
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

        std::fs::write(bin_only.join("src/lib.rs"), "").unwrap();
        let r = run_stage(&cx, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(
            outcomes[0].0,
            Outcome::Fail(Severity::CouldNotRun),
            "{}",
            outcomes[0].1
        );
        // …while the test stage runs on the same scope: it needs no driver.
        let r = run_stage(&cx, &spec(StageId::Test));
        assert!(r.outcomes().all(|(o, _)| o == Outcome::Ok));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_doc_driver_rule_pins_stage2_first_and_diagnoses_absence() {
        // The pinned driver always wins; the caller's export outranks the PATH
        // walk as cargo ranks env over config; a farm link keeps an older stage2
        // running; nothing anywhere is a diagnosis, never a raw exec death.
        assert_eq!(doc_driver_from(true, true, true), DocDriver::Stage2);
        assert_eq!(doc_driver_from(true, false, false), DocDriver::Stage2);
        assert_eq!(doc_driver_from(false, true, true), DocDriver::Ambient);
        assert_eq!(doc_driver_from(false, true, false), DocDriver::Ambient);
        assert_eq!(doc_driver_from(false, false, true), DocDriver::BarePath);
        assert_eq!(doc_driver_from(false, false, false), DocDriver::Absent);
    }

    /// `--no-fail-fast` is the coverage flag: without it `targo test` stops at
    /// the first failing binary and the stage reports on a prefix of the tree.
    /// It changes nothing about the verdict, since cargo still exits non-zero.
    #[test]
    fn every_test_argv_carries_no_fail_fast_so_one_binary_cannot_hide_the_rest() {
        for argv in [
            test_compile_args(&Scope::workspace()),
            test_run_args(&Scope::workspace()),
            test_compile_args(&Scope::crate_only("aterm-grid")),
            test_run_args(&Scope::crate_only("aterm-grid")),
            test_compile_args(&Scope::changed("main", vec!["aterm-gui".into()], true)),
            test_run_args(&Scope::changed("main", vec!["aterm-gui".into()], true)),
            measuring_args(),
            doctest_args(&Scope::workspace()),
            doctest_args(&Scope::crate_only("aterm-grid")),
        ] {
            assert!(
                argv.iter().any(|a| a == "--no-fail-fast"),
                "a test argv without --no-fail-fast reports a PREFIX of its \
                 scope: {argv:?}"
            );
        }
    }

    #[test]
    fn a_scope_reaches_the_test_doctest_and_lint_argv_together() {
        let s = Scope::crate_only("aterm-grid");
        assert_eq!(
            test_compile_args(&s),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-grid",
                "--no-fail-fast",
                "--no-run",
                "--message-format=json-render-diagnostics"
            ]
        );
        assert_eq!(
            doctest_args(&s),
            [
                "--unverified",
                "test",
                "--doc",
                "-p",
                "aterm-grid",
                "--no-fail-fast"
            ]
        );
        assert_eq!(
            tippy_args(&s),
            [
                "-p",
                "aterm-grid",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings"
            ]
        );
    }

    #[test]
    fn a_change_scope_reaches_the_same_three_argvs_as_one_dash_p_per_crate() {
        let s = Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true);
        assert_eq!(
            test_compile_args(&s),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui",
                "--no-fail-fast",
                "--no-run",
                "--message-format=json-render-diagnostics"
            ]
        );
        assert_eq!(
            doctest_args(&s),
            [
                "--unverified",
                "test",
                "--doc",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui",
                "--no-fail-fast"
            ]
        );
        assert_eq!(
            tippy_args(&s),
            [
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings"
            ]
        );
    }

    #[test]
    fn an_empty_change_selection_compiles_nothing_in_any_of_those_stages() {
        // The guard is in `run_scoped`, so it holds for every stage that uses
        // it; a stage without it would build the whole tree under the label
        // `<no crates selected>`.
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
                    after_lanes: Vec::new(),
                    after_stages: Vec::new(),
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
    fn the_lint_is_denied_warnings_after_the_separator() {
        let a = tippy_args(&Scope::workspace());
        let sep = a.iter().position(|x| x == "--").expect("a -- separator");
        assert_eq!(&a[sep + 1..], ["-D", "warnings"]);
        assert!(a[..sep].contains(&"--all-targets".to_string()));
        // `--keep-going` is a coverage guarantee: without it the first crate
        // that trips `-D warnings` ends the run. It must sit on cargo's side of
        // the separator; after `--` it would reach the lint driver, which does
        // not know the flag.
        assert!(
            a[..sep].contains(&"--keep-going".to_string()),
            "the lint must not stop at the first failing crate: {a:?}"
        );
        // …and the `release` required-features pass holds to the same three.
        let g = tippy_gated_args(&Scope::workspace(), &fixture_gated())
            .expect("the workspace selects both");
        let sep = g.iter().position(|x| x == "--").expect("a -- separator");
        assert_eq!(&g[sep + 1..], ["-D", "warnings"], "{g:?}");
        for flag in ["--all-targets", "--keep-going"] {
            assert!(g[..sep].contains(&flag.to_string()), "{flag}: {g:?}");
        }
    }

    /// The workspace run's cargo children, spelled out: an added `--lib`,
    /// `--bins` or `--exclude`, or a dropped `--no-fail-fast` or `--keep-going`,
    /// would still print a green row over less of the tree than its name says.
    #[test]
    fn the_workspace_argvs_are_exactly_these() {
        let ws = Scope::workspace();
        let words = |a: &[&str]| a.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        // The compile also asks for cargo's JSON, which names each test
        // executable for the recording (`crate::testrun`); the message format is
        // not part of cargo's fingerprint.
        assert_eq!(
            test_compile_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--workspace",
                "--no-fail-fast",
                "--no-run",
                "--message-format=json-render-diagnostics"
            ])
        );
        assert_eq!(
            test_run_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--workspace",
                "--no-fail-fast",
                "--tests",
                "--",
                "-Zunstable-options",
                "--report-time",
            ])
        );
        assert_eq!(
            doctest_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--doc",
                "--workspace",
                "--no-fail-fast"
            ])
        );
        assert_eq!(
            measuring_args(),
            words(&[
                "--unverified",
                "test",
                "-p",
                "aterm-conformance",
                "--no-fail-fast",
                "--test",
                "paint",
                "--test",
                "spin",
                "--",
                "--ignored",
                "measuring::",
            ])
        );
        assert_eq!(
            tippy_args(&ws),
            words(&[
                "--workspace",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings"
            ])
        );
        assert_eq!(
            tippy_gated_args(&ws, &fixture_gated()),
            Some(words(&[
                "-p",
                "aterm-gui",
                "-p",
                "aterm-scrollback",
                "--features",
                "aterm-gui/bench-support,aterm-gui/control-conformance,aterm-scrollback/disk-tier",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings",
            ]))
        );
    }

    #[test]
    fn tippy_names_its_target_dir_and_keeps_migration_quiet() {
        // Both matter: a shared target dir would queue the lint behind the
        // test build on cargo's lock, and the migration warning would fail the
        // lint under -D warnings.
        let mut c = ctx(Scope::workspace());
        c.tools.tippy = Some(PathBuf::from("/s2/targo-tippy"));
        c.path_env = std::ffi::OsString::from("/usr/bin");
        // Assert on the command the stage builds, not a replica: a replica
        // agrees with itself after the stage stops setting a variable.
        let cmd = tippy_cmd(
            &c,
            std::path::Path::new("/s2/targo-tippy"),
            tippy_args(&c.scope),
        );
        let names: Vec<String> = cmd
            .envs
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["PATH", "CARGO_TARGET_DIR", "TRUST_NO_MIGRATE_WARN"]);
        assert_eq!(cmd.envs[1].1.to_string_lossy(), "/repo/target/verify/tippy");
        assert_eq!(
            cmd.envs[0].1.to_string_lossy(),
            "/s2:/usr/bin",
            "the tippy binary's own dir must LEAD the inherited PATH, so the \
             lint runs under THE toolchain rather than whatever `cargo` the \
             ambient PATH offers first"
        );
    }

    /// Which children yield the CPU ([`Cmd::demoted`]): every child that only
    /// compiles. A child that runs code keeps the inherited tier, since a clamp
    /// reaches everything it launches (the test run and the sealed rung launch
    /// aterms).
    #[test]
    fn compile_only_children_yield_and_children_that_run_code_do_not() {
        let mut c = ctx(Scope::workspace());
        c.tools.tippy = Some(PathBuf::from("/s2/targo-tippy"));

        for bind in [false, true] {
            let [compile, run] = test_cmds(&c, bind);
            assert!(compile.demoted, "targo test --no-run");
            assert!(!run.demoted, "targo test --tests runs code");
            assert_eq!(compile.argv()[1..], test_compile_args(&c.scope)[..]);
            assert_eq!(run.argv()[1..], test_run_args(&c.scope)[..]);
            let artifact = crate::MeasureArtifact {
                app: PathBuf::from("/release/aterm.app"),
            };
            let (_, measuring) = measuring_cmd(&c, &artifact);
            assert!(
                !measuring.demoted,
                "the measuring tests run the paint and spin guards"
            );
            assert_eq!(measuring.argv()[1..], measuring_args()[..]);
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
        for (label, cmd) in driver_build_cmds(&c)
            .into_iter()
            .chain(tail_driver_build_cmds(&c))
        {
            assert!(cmd.demoted, "{label}");
        }
        let (suite_label, suite) = sealed_lane_cmd(&c);
        assert!(
            !suite.demoted,
            "{suite_label} boots real aterm --headless processes"
        );
    }

    /// The gated list is the manifests' own: each `required-features` key counts
    /// under its `[package]` name (never the target's `name =`, never comments),
    /// inline or spanning lines; a manifest with no `[package]` names nothing.
    /// Sorted, deduplicated.
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
        // The real tree has gated targets, and the scan reaches them.
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert!(
            !gated_lint_features(&repo).is_empty(),
            "the manifest scan found nothing in the real tree — it broke"
        );
    }

    #[test]
    fn the_gated_pass_narrows_with_the_scope_and_disappears_when_it_has_nothing_to_reach() {
        let gated = fixture_gated();
        // One gated package selected: only its features, only its `-p`.
        let one = tippy_gated_args(&Scope::crate_only("aterm-scrollback"), &gated)
            .expect("aterm-scrollback owns two gated benches");
        assert_eq!(
            one,
            [
                "-p",
                "aterm-scrollback",
                "--features",
                "aterm-scrollback/disk-tier",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings",
            ]
        );
        // A scope with no gated target must not compile two crates it was
        // narrowed away from just to lint nothing.
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
    }

    /// The environment variable names one command carries, in order.
    fn env_names(cmd: &Cmd) -> Vec<String> {
        cmd.envs
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_test_run_runs_the_trail_laws_full_grid() {
        // Without it the trail laws run only each family's spine in the run's
        // debug build.
        let c = ctx(Scope::workspace());
        for bind in [false, true] {
            let [compile, run] = test_cmds(&c, bind);
            let (k, v) = TRAIL_LAWS_FULL;
            assert!(
                run.envs
                    .iter()
                    .any(|(ek, ev)| ek.to_string_lossy() == k && ev.to_string_lossy() == v),
                "the test run carries {k}={v}: {:?}",
                run.envs
            );
            assert!(
                !env_names(&compile).iter().any(|n| n == k),
                "the compile needs no copy of {k}"
            );
        }
    }

    #[test]
    fn the_doc_running_stages_bind_trusts_renamed_doc_driver() {
        // Trust renames rustdoc to `trustdoc`. Unbound, doctests either fail to
        // launch or run under whatever rustdoc the PATH offers.
        let c = ctx(Scope::workspace());
        let cmd = with_trustdoc(&c, targo(&c, doctest_args(&c.scope)));
        assert_eq!(env_names(&cmd), ["RUSTDOC"]);
        assert_eq!(
            cmd.envs[0].1.as_os_str(),
            c.tools.trustdoc.as_os_str(),
            "the doc driver must be THE toolchain's, by absolute path"
        );
    }

    #[test]
    fn no_targo_is_fail_closed_at_the_test_compile_and_honest_everywhere_after() {
        let c = ctx(Scope::workspace());
        assert!(!c.tools.have_targo());

        let mut r = Report::new("test compile");
        test_compile(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(outcome, crate::Outcome::Fail(crate::Severity::CouldNotRun));
        assert!(label.starts_with("targo not found"), "{label}");
        assert!(
            label.contains("scripts/recreate_bootstrap.py --stage 2 --require-seed --fresh-seed"),
            "the diagnostic says how to fix it, from the pinned Trust seed: {label}"
        );

        // The dependent stages then skip, each named.
        let dependent: [fn(&Ctx, &mut Report); 10] = [
            test,
            measuring_tests,
            doctests,
            spec_check,
            ty_conformances,
            export,
            trust_lane,
            libc_oracle,
            driver_builds,
            redraw_conformance,
        ];
        for stage in dependent {
            let mut r = Report::new("s");
            stage(&c, &mut r);
            for (outcome, label) in r.outcomes() {
                assert_eq!(outcome, crate::Outcome::Skip, "{label}");
                assert!(label.contains("no targo"), "{label}");
            }
        }
    }

    /// `spec-check` builds in the run's own target dir (after the test compile,
    /// whose library build it reuses), whole under `release` (`--full`); its
    /// exit is read as the libc oracle's, and a missing checker's own line is
    /// the row's reason.
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
        assert!(
            dev.argv()
                .windows(2)
                .any(|w| w[0] == "--features" && w[1] == "anchors"),
            "the bin's required feature: {:?}",
            dev.argv()
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

    /// The ty-only conformances compile then run one graph over their four
    /// packages, and a passing run that lost one of them decided nothing.
    #[test]
    fn the_ty_only_conformances_run_by_name_and_a_lost_one_is_not_green() {
        let compile = ty_conformance_args(true);
        let run = ty_conformance_args(false);
        let split = |a: &[String]| a.iter().position(|x| x == "--tests").expect("--tests") + 1;
        assert_eq!(compile[..split(&compile)], run[..split(&run)]);
        assert_eq!(compile[split(&compile)..], ["--no-run"]);
        assert_eq!(run[split(&run)..], ["--", "--ignored", "_under_ty"]);
        for (package, test) in TY_ONLY_CONFORMANCES {
            assert!(run.windows(2).any(|w| w == ["-p", package]), "{package}");
            assert!(test.ends_with("_under_ty"), "{test}");
        }
        let all: String = TY_ONLY_CONFORMANCES
            .iter()
            .map(|(_, t)| format!("test some::path::{t} ... ok\n"))
            .collect();
        assert!(ty_conformances_not_passed(&all).is_empty());
        let (_, lost) = TY_ONLY_CONFORMANCES[3];
        let three = all.replace(&format!("{lost} ... ok"), &format!("{lost} ... FAILED"));
        assert_eq!(ty_conformances_not_passed(&three), [lost]);
        assert_eq!(ty_conformances_not_passed("").len(), 4);
    }

    /// `ship check --export`'s exit code is the verdict: `0` passes, `1` and
    /// `2` are findings, `3` is a skip that shows the fix lines it printed, and
    /// an interrupt is never green. It builds in the trust lane, beside the
    /// release tools that lane runs.
    #[test]
    fn the_export_check_reads_ship_checks_exit_code() {
        assert_eq!(
            export_outcome(Some(0), ""),
            (Outcome::Ok, EXPORT_LABEL.to_string())
        );
        for code in [1, 2, 101] {
            assert_eq!(
                export_outcome(Some(code), "").0,
                Outcome::Fail(Severity::GateFailed),
                "{code}"
            );
        }
        let (o, l) = export_outcome(
            Some(3),
            "  NOT RUN export guards — no gitleaks on PATH\n          fix: brew install gitleaks\n",
        );
        assert_eq!(o, Outcome::Skip);
        assert!(l.ends_with("fix: brew install gitleaks"), "{l}");
        let (_, bare) = export_outcome(Some(3), "");
        assert!(bare.contains("NOT RUN lines above"), "{bare}");
        for code in [Some(130), None] {
            assert_eq!(
                export_outcome(code, "").0,
                Outcome::Fail(Severity::CouldNotRun)
            );
        }
        let c = ctx(Scope::workspace());
        let cmd = export_cmd(&c);
        assert_eq!(
            cmd.args,
            ship_args(["check", "--export"])
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        );
        assert!(
            cmd.envs.iter().any(|(k, v)| k == "CARGO_TARGET_DIR"
                && v.to_string_lossy() == "/repo/target/verify/trust")
        );
    }

    /// `--no-windows` compiles no window driver: the tail driver builds keep only
    /// the Codex lane's binaries (macOS) and leave out the redraw harness and the
    /// objc drivers, and the row says so. Without the flag every tail build is
    /// there (`tail_builds_select_only_what_the_tail_drives`).
    #[test]
    fn no_windows_leaves_the_window_drivers_out_of_the_tail_builds() {
        use crate::checks::Check;
        let mut quiet = ctx(Scope::workspace())
            .with_checks(&[Check::Codex, Check::Appkit])
            .with_no_windows(true);
        let argvs: Vec<Vec<String>> = tail_driver_build_cmds(&quiet)
            .into_iter()
            .map(|(_, cmd)| cmd.argv()[1..].to_vec())
            .collect();
        let want = if cfg!(target_os = "macos") {
            vec![codex_aterm_build_args()]
        } else {
            Vec::new()
        };
        assert_eq!(argvs, want);
        for id in [StageId::RedrawConformance, StageId::ObjcDrives] {
            assert!(
                id.no_windows_skip().is_some(),
                "{id:?} skips under the flag"
            );
        }
        quiet.tools.targo = PathBuf::from("/nonexistent/targo");
        let mut r = Report::new("tail driver builds");
        tail_driver_builds(&quiet, &mut r);
        assert!(r.render().contains(TAIL_WINDOWED_BUILDS), "{}", r.render());
    }

    #[test]
    fn the_redraw_gate_builds_the_feature_without_which_it_builds_nothing() {
        // `aterm-redraw-conformance` carries `required-features =
        // ["control-conformance"]`. Without the feature cargo matches no target
        // and exits 0, and the stage would decide on a stale binary.
        let a = redraw_conformance_build_args();
        let feat = a
            .iter()
            .position(|x| x == "--features")
            .expect("the run must ask for the feature that compiles it");
        assert_eq!(a[feat + 1], "control-conformance");
        let bin = a.iter().position(|x| x == "--bin").expect("a --bin");
        assert_eq!(a[bin + 1], REDRAW_CONFORMANCE_BIN);
        assert!(a.contains(&"aterm-gui".to_string()));
    }

    /// `--no-windows`: each windowed stage is one named skip with its fix,
    /// decided before anything is built or launched; the flag removes no stage
    /// and touches no other. Without the flag each runs its own body (here,
    /// with no driver, to its own skip).
    #[test]
    fn no_windows_skips_each_windowed_stage_by_name_and_nothing_else() {
        use crate::plan::{WINDOWED, plan};
        let every: Vec<crate::checks::Check> =
            crate::checks::CHECKS.iter().map(|s| s.check).collect();
        let base = || {
            let mut c = ctx(Scope::workspace()).with_checks(&every);
            // No driver, so a stage body that does run cannot build or launch.
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
            assert!(
                rows[0].1.starts_with(name) && rows[0].1.contains("fix: run without --no-windows"),
                "{}",
                rows[0].1
            );
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

    /// Every row's exit contract ([`objc_drive_outcome`]), each reading of `3`
    /// some row's.
    #[test]
    fn every_objc_drive_reads_its_exit_contract() {
        for drive in &OBJC_DRIVES {
            let name = drive.example;
            let read = |code| objc_drive_outcome(drive, code);
            for code in 0..=3 {
                assert!(
                    read(Some(code)).1.starts_with(&format!("{name}: ")),
                    "{name}: every line names its driver"
                );
            }
            assert_eq!(read(Some(0)).0, Outcome::Ok, "{name}");
            assert_eq!(
                read(Some(1)).0,
                Outcome::Fail(Severity::GateFailed),
                "{name}"
            );
            let (two, two_label) = read(Some(2));
            assert_eq!(two, Outcome::Fail(Severity::CouldNotRun), "{name}");
            assert!(two_label.contains("no event loop"), "{two_label}");
            let (three, three_label) = read(Some(3));
            match drive.code3 {
                Code3::WeakerPassOk(_) => assert_eq!(three, Outcome::Ok, "{name}"),
                Code3::Hung => {
                    assert_eq!(three, Outcome::Fail(Severity::CouldNotRun), "{name}");
                    assert!(three_label.contains("HUNG"), "{three_label}");
                }
                Code3::AbortFinding => {
                    assert_eq!(three, Outcome::Fail(Severity::GateFailed), "{name}");
                    assert!(three_label.contains("ABORTED"), "{three_label}");
                }
                Code3::None => {
                    assert_eq!(three, Outcome::Fail(Severity::GateFailed), "{name}");
                    assert!(three_label.contains("unexpected exit 3"), "{three_label}");
                }
            }
            // A panic (101), a code a future edit adds without teaching the
            // row, and above all no code is green.
            for code in [4, 5, 101, 255] {
                assert_eq!(
                    read(Some(code)).0,
                    Outcome::Fail(Severity::GateFailed),
                    "{name}: exit {code} must not be green"
                );
            }
            assert_eq!(read(None).0, Outcome::Fail(drive.no_status), "{name}");
        }
        // Each reading of `3` is some row's, so none of the arms above is dead.
        for code3 in [Code3::Hung, Code3::AbortFinding, Code3::None] {
            assert!(OBJC_DRIVES.iter().any(|d| d.code3 == code3), "{code3:?}");
        }
    }

    /// The audit's `3` is green and says a weaker thing than its `0`: some rows
    /// were held to the fork's own declaration rather than the runtime's, so its
    /// line must not assert runtime authority for them.
    #[test]
    fn the_audits_weaker_pass_never_shares_a_sentence_with_its_pass() {
        let audit = OBJC_DRIVES
            .iter()
            .find(|d| matches!(d.code3, Code3::WeakerPassOk(_)))
            .expect("the live-class audit answers 3 as a weaker pass");
        let zero = objc_drive_outcome(audit, Some(0)).1;
        let three = objc_drive_outcome(audit, Some(3)).1;
        assert!(
            three.contains("FORK'S OWN DECLARATION"),
            "exit 3 must name the authority it actually used: {three}"
        );
        assert_ne!(three, zero, "0 and 3 are different claims");
        assert!(
            zero.contains("runtime's own authority") && !zero.contains("FORK'S OWN"),
            "exit 0 keeps claiming the runtime arbitrated, and only that: {zero}"
        );
    }

    #[test]
    fn a_redraw_gate_that_could_not_run_is_never_a_pass_and_never_a_skip() {
        // Exit 2 is the harness saying no event loop is constructible here; it
        // must read neither as green nor as a quiet skip.
        let (outcome, label) = redraw_outcome(Some(2));
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
        assert_ne!(outcome, Outcome::Ok);
        assert_ne!(outcome, Outcome::Skip);
        assert!(label.contains("no event loop"), "{label}");

        assert_eq!(redraw_outcome(Some(0)).0, Outcome::Ok);
        assert_eq!(
            redraw_outcome(Some(1)).0,
            Outcome::Fail(Severity::GateFailed)
        );
        // A panic (101) is a finding and a signal decided nothing; neither is green.
        assert_eq!(
            redraw_outcome(Some(101)).0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(redraw_outcome(None).0, Outcome::Fail(Severity::CouldNotRun));
        for code in [None, Some(1), Some(2), Some(101), Some(-1)] {
            assert_ne!(redraw_outcome(code).0, Outcome::Ok, "{code:?}");
        }
    }

    /// A measuring child that passed no row measured nothing: COULD NOT RUN,
    /// however cleanly it exited (every row ignored, a filter matching
    /// nothing). One that passed rows is green, and a failed one is a finding.
    #[test]
    fn a_measuring_child_that_passed_no_row_could_not_run() {
        let result = |target: &str, passed: usize, failed: usize, ignored: usize| {
            format!(
                "     Running tests/{target}.rs (target/debug/deps/{target}-1)\n\
                 test result: ok. {passed} passed; {failed} failed; {ignored} ignored; 0 \
                 measured; 0 filtered out; finished in 0.01s\n"
            )
        };
        let both = |paint: (usize, usize, usize), spin: (usize, usize, usize)| {
            result("paint", paint.0, paint.1, paint.2) + &result("spin", spin.0, spin.1, spin.2)
        };
        let decide = |ok: bool, output: String| {
            let run = exec::Run {
                ok,
                output,
                code: Some(if ok { 0 } else { 101 }),
                spawn_error: None,
            };
            let mut r = Report::new("measuring");
            decide_measuring_child(&mut r, &run, "measuring tests", &[]);
            let (outcome, label) = r.outcomes().next().expect("a decision");
            (outcome, label.to_string())
        };
        assert_eq!(
            decide(true, both((0, 0, 23), (0, 0, 2))).0,
            Outcome::Fail(Severity::CouldNotRun),
            "every row ignored"
        );
        let (outcome, label) = decide(true, both((0, 0, 0), (2, 0, 0)));
        assert_eq!(
            outcome,
            Outcome::Fail(Severity::CouldNotRun),
            "paint selected nothing beside a spin that passed"
        );
        assert!(
            label.contains("tests/paint.rs") && !label.contains("tests/spin.rs"),
            "{label}"
        );
        assert_eq!(
            decide(true, result("paint", 23, 0, 0)).0,
            Outcome::Fail(Severity::CouldNotRun),
            "spin never ran"
        );
        assert_eq!(
            decide(true, String::new()).0,
            Outcome::Fail(Severity::CouldNotRun),
            "no binary ran"
        );
        assert_eq!(decide(true, both((23, 0, 0), (2, 0, 0))).0, Outcome::Ok);
        assert_eq!(
            decide(false, both((22, 1, 0), (2, 0, 0))).0,
            Outcome::Fail(Severity::GateFailed)
        );
    }

    /// A caller's `CARGO_BUILD_JOBS` is a ceiling for the side lanes' constants,
    /// never a floor, never a parse error: a small machine's export holds on
    /// every lane, not only on the ones that inherit it.
    #[test]
    fn a_callers_cargo_build_jobs_is_a_ceiling_for_the_side_lanes() {
        for side in [Lane::TrustTarget, Lane::CellsTarget] {
            assert_eq!(lane_jobs(side, None), Some(4), "{side:?}");
            assert_eq!(lane_jobs(side, Some(2)), Some(2), "{side:?}");
            assert_eq!(lane_jobs(side, Some(16)), Some(4), "{side:?}");
        }
        assert_eq!(lane_jobs(Lane::DriverTarget, None), Some(8));
        assert_eq!(lane_jobs(Lane::DriverTarget, Some(4)), Some(4));
        assert_eq!(lane_jobs(Lane::DriverTarget, Some(8)), Some(8));
        // The caller's value as the run reads it: a positive integer, spaces
        // allowed.
        let ceiling = |raw: &str| {
            let mut c = ctx(Scope::workspace());
            c.env.cargo_build_jobs = Some(raw.into());
            c.build_jobs_ceiling()
        };
        assert_eq!(ceiling(" 3 "), Some(3));
        // Not a positive integer: no ceiling, so the constant stands, and the
        // raw value still reaches the main lane by inheritance to fail with
        // cargo's own message.
        for junk in ["", "0", "-1", "four", "4.5"] {
            assert_eq!(ceiling(junk), None, "{junk:?}");
        }
        // Lanes with no cap of their own stay uncapped here — they inherit.
        for lane in [
            Lane::MainTarget,
            Lane::AfterCompile,
            Lane::Pure,
            Lane::TippyTarget,
        ] {
            assert_eq!(lane_jobs(lane, Some(2)), None, "{lane:?}");
        }
        // Through `in_lane`: the ceiling reaches the child's environment.
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
        for (label, cmd) in driver_build_cmds(&c)
            .into_iter()
            .chain(tail_driver_build_cmds(&c))
        {
            assert!(
                envs(&cmd).contains(&("CARGO_BUILD_JOBS".to_string(), "3".to_string())),
                "{label}: {:?}",
                envs(&cmd)
            );
        }
        assert!(envs(&export_cmd(&c)).contains(&("CARGO_BUILD_JOBS".to_string(), "3".to_string())));
    }

    /// The tail driver builds build only what the tail drives, so the test run
    /// can start without them ([`crate::plan::NOT_AWAITED`]): every child names
    /// its targets, none of them the first row's `aterm-gui` or `aterm-ctl`
    /// (which a bare `-p aterm-gui` would relink), and every tail binary is
    /// prebuilt, so nothing compiles inside the exclusive section.
    #[test]
    fn tail_builds_select_only_what_the_tail_drives() {
        use crate::checks::Check;
        // Each check prebuilds only what it drives.
        let built = |checks: &[Check]| -> Vec<String> {
            tail_driver_build_cmds(&ctx(Scope::workspace()).with_checks(checks))
                .into_iter()
                .map(|(label, _)| label)
                .collect()
        };
        assert!(built(&[Check::Test]).is_empty());
        assert!(
            built(&[Check::Codex])
                .iter()
                .all(|l| !l.contains(REDRAW_CONFORMANCE_BIN) && !l.contains("objc")),
            "{:?}",
            built(&[Check::Codex])
        );
        let c = ctx(Scope::workspace()).with_checks(&[Check::Codex, Check::Appkit]);
        let mac = cfg!(target_os = "macos");
        let mut tail_driven: Vec<String> = vec![REDRAW_CONFORMANCE_BIN.to_string()];
        if mac {
            tail_driven.push("aterm".to_string());
            tail_driven.push("aterm-lab".to_string());
            tail_driven.extend(OBJC_DRIVES.iter().map(|d| d.example.to_string()));
        }
        let before_the_barrier = ["aterm-gui", "aterm-ctl"];
        let mut selected: Vec<String> = Vec::new();
        for (label, cmd) in tail_driver_build_cmds(&c) {
            let argv = cmd.argv();
            let mut named = 0;
            for w in argv.windows(2) {
                if w[0] == "--bin" || w[0] == "--example" {
                    named += 1;
                    assert!(tail_driven.contains(&w[1]), "{label}: {}", w[1]);
                    assert!(
                        !before_the_barrier.contains(&w[1].as_str()),
                        "{label}: {}",
                        w[1]
                    );
                    selected.push(w[1].clone());
                }
            }
            assert!(named > 0, "{label} names no target: {argv:?}");
            assert!(
                !argv
                    .iter()
                    .any(|a| a == "--bins" || a == "--examples" || a == "--all-targets"),
                "{label}: {argv:?}"
            );
        }
        selected.sort();
        tail_driven.sort();
        assert_eq!(selected, tail_driven, "every tail binary is prebuilt");
        // The two rows' children are exactly the driver builds, each once.
        let mut both: Vec<Vec<String>> = driver_build_cmds(&c)
            .into_iter()
            .chain(tail_driver_build_cmds(&c))
            .map(|(_, cmd)| cmd.argv()[1..].to_vec())
            .collect();
        let mut want = vec![smoke_build_args(), redraw_conformance_build_args()];
        if mac {
            want.push(codex_aterm_build_args());
            want.push(objc_driver_prebuild_args());
        }
        both.sort();
        want.sort();
        assert_eq!(both, want);
    }

    #[test]
    fn side_lane_cmds_name_their_own_target_dir() {
        let env = |cmd: &Cmd| -> Vec<(String, String)> {
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
        let lane = |dir: &str, jobs: Option<&str>| {
            let mut v = vec![("CARGO_TARGET_DIR".to_string(), dir.to_string())];
            if let Some(j) = jobs {
                v.push(("CARGO_BUILD_JOBS".to_string(), j.to_string()));
            }
            v
        };
        let c = ctx(Scope::workspace());

        assert_eq!(
            env(&export_cmd(&c)),
            lane("/repo/target/verify/trust", Some("4"))
        );
        for (label, cmd) in driver_build_cmds(&c)
            .into_iter()
            .chain(tail_driver_build_cmds(&c))
            .chain(std::iter::once(sealed_lane_cmd(&c)))
        {
            assert_eq!(
                env(&cmd),
                lane("/repo/target/verify/drivers", Some("8")),
                "{label}"
            );
        }
        for args in [
            redraw_conformance_build_args(),
            objc_drive_build_args(&OBJC_DRIVES[0]),
        ] {
            assert_eq!(
                env(&driver_build_cmd(&c, args)),
                lane("/repo/target/verify/drivers", Some("8"))
            );
        }
        assert_eq!(
            drivers_dir(&c),
            PathBuf::from("/repo/target/verify/drivers")
        );
        // The main lane is handed nothing of its own: its children get the
        // run's target dir from the run's environment. Nor is `AfterCompile`,
        // the same directory.
        assert!(
            env(&in_lane(
                &c,
                Lane::MainTarget,
                targo(&c, test_compile_args(&c.scope))
            ))
            .is_empty()
        );
        assert!(
            env(&in_lane(
                &c,
                Lane::AfterCompile,
                targo(&c, doctest_args(&c.scope))
            ))
            .is_empty()
        );
    }

    /// The driven binary is built in the driver lane and handed to its suite:
    /// the Codex lane's build child only compiles, in the lane the plan
    /// schedules the row in; the binary is that dir's (never the main target's,
    /// a previous run's or one still linking); and the suite is handed it as its
    /// argument and nothing else. The sealed rung has no build child: its suites
    /// drive `CARGO_BIN_EXE_aterm`, which cargo builds in the same lane, and it
    /// is handed nothing beyond the lane.
    #[test]
    fn every_driven_binary_is_built_in_the_driver_lane_and_handed_to_its_suite() {
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
        let drivers = drivers_dir(&c);
        let lane_of = |id: StageId| {
            let spec = plan
                .iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("{id:?} is planned"));
            lane_dir(&c, spec.lane)
        };

        let aterm = codex_aterm_binary(&c);
        let build = codex_aterm_build_cmd(&c);
        let suite = codex_live_upgrade_cmd(&c);
        assert_eq!(build.argv()[1..], codex_aterm_build_args()[..]);
        assert!(build.demoted, "the build only compiles");
        assert!(!suite.demoted, "the suite runs code");
        assert_eq!(lane(&build), Some(drivers.clone()));
        assert_eq!(lane(&build), lane_of(StageId::CodexLiveUpgrade));
        assert!(
            aterm.starts_with(&drivers) && aterm.parent() != Some(c.target_dir.as_path()),
            "{}",
            aterm.display()
        );
        assert_eq!(
            suite.argv()[1..],
            [
                "codex-upgrade".to_string(),
                "--aterm".to_string(),
                aterm.display().to_string()
            ]
        );
        assert_eq!(suite.program, drivers.join("debug/aterm-lab"));
        assert!(suite.envs.is_empty(), "{:?}", suite.envs);
        assert_eq!(
            suite.term_grace,
            Some(CODEX_TERM_GRACE),
            "a lane over the ceiling is sent SIGTERM first, so it tears down"
        );

        let (_, sealed) = sealed_lane_cmd(&c);
        assert!(!sealed.demoted, "the sealed suites run code");
        assert_eq!(lane(&sealed), lane_of(StageId::SealedLane));
        assert_eq!(
            sealed.envs,
            [
                ("CARGO_TARGET_DIR".into(), drivers.into_os_string()),
                ("CARGO_BUILD_JOBS".into(), "8".into()),
            ],
            "the lane and nothing else"
        );
    }

    /// The Codex lane's NOT RUN is never a pass: its `3` is a named skip with
    /// the lab's own reason; `1` and any other code are findings, and no status
    /// decided nothing.
    #[test]
    fn the_codex_lanes_not_run_is_never_a_pass_and_quotes_the_labs_reason() {
        use crate::Outcome;
        let cx = CODEX_LANE;
        assert_eq!(
            codex_live_upgrade_outcome(Some(0), "PASS"),
            (Outcome::Ok, cx.to_string())
        );
        assert_eq!(
            codex_live_upgrade_outcome(Some(1), "").0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(
            codex_live_upgrade_outcome(None, "").0,
            Outcome::Fail(Severity::CouldNotRun),
            "a signal decided nothing"
        );
        // The lab's reason is the label, so the verdict names the remedy.
        let transcript = "managed Codex 0.157.1\naterm-lab: NOT RUN: no managed Codex (aterm pkg install codex)\n";
        assert_eq!(
            codex_live_upgrade_outcome(Some(3), transcript),
            (
                Outcome::Skip,
                "lab codex-upgrade: NOT RUN — no managed Codex (aterm pkg install codex) (this \
                 machine lacks a prerequisite; a named skip, never a pass)"
                    .to_string()
            )
        );
        let (outcome, label) = codex_live_upgrade_outcome(Some(3), "");
        assert_eq!(outcome, Outcome::Skip);
        assert!(label.contains("the lab gave no reason"), "{label}");
        for other in [2, 77, 101] {
            assert_eq!(
                codex_live_upgrade_outcome(Some(other), "").0,
                Outcome::Fail(Severity::GateFailed),
                "exit {other}"
            );
        }
    }
}
