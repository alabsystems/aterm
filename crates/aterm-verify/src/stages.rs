// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stages themselves.
//!
//! Each stage's argv is built by a small pure function below, so it is checked by
//! unit tests: tippy's separate `CARGO_TARGET_DIR` and `TRUST_NO_MIGRATE_WARN`, the
//! doc-driver rule on the doc-running stages (`DocDriver`, private: `RUSTDOC=
//! <stage2>/trustdoc` when the stage2 carries it, else the caller's export or the
//! PATH farm link, else a diagnosis), and `--unverified` on every driver
//! invocation (`targo` refuses a bare verb so a gate cannot be quietly
//! unverified).

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
        StageId::Formatting => formatting(ctx, &mut r),
        StageId::GrepGuards => grep_guards(ctx, &mut r),
        StageId::ExportContent => export_content(ctx, &mut r),
        StageId::DeliveryTooling => delivery_tooling(ctx, &mut r),
        StageId::AtpkgTooling => atpkg_tooling(ctx, &mut r),
        StageId::TrustContractProbe => trust_contract_probe(ctx, &mut r),
        StageId::TrustAdvisory => trust_advisory(ctx, &mut r),
        StageId::StartCompare => start_compare(ctx, &mut r),
        StageId::LibcOracle => libc_oracle(ctx, &mut r),
        StageId::FreezeGate => freeze_gate(ctx, &mut r),
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
        StageId::ForegroundHandback => foreground_handback(ctx, &mut r),
        StageId::RenderDesync => render_desync(ctx, &mut r),
        StageId::CrossCells => cross_cells(ctx, &mut r),
        StageId::Forge => forge_gate(ctx, &mut r),
        StageId::ForeignCells => foreign_cells(ctx, &mut r),
        StageId::CodexLiveUpgrade => codex_live_upgrade(ctx, &mut r),
    }
    r
}

// ---------------------------------------------------------------------------
// The argv builders. Pure, so each is a unit test.
// ---------------------------------------------------------------------------

/// `targo --unverified test <scope> --no-fail-fast --no-run` — the test compile
/// row's one child and the test stage's first: compile every test target and run
/// nothing.
///
/// CARGO'S DEFAULT SELECTION, NOT `--tests` (measured 2026-10-01). `--tests`
/// would skip 65 example builds and 3 plain library builds the run never
/// executes, but it also changes the workspace graph the later stages share:
/// in the gate that tried it the doctests stage, after the test run and on the
/// critical path, recompiled aterm-gui, aterm-render, aterm-grid and their
/// proc-macro dependencies (191 units; 37 s -> 96 s). The default selection
/// builds them here, at t0, beside the other lanes, where nobody waits for them.
///
/// `--no-fail-fast` IS THE COVERAGE FLAG, the exact analogue of `--keep-going`
/// on [`tippy_args`] below, and for the same reason: without it cargo stops the
/// moment ONE test binary fails, so the stage reports on a PREFIX of the
/// workspace while reading like a statement about all of it. MEASURED
/// 2026-09-10: `aterm-conformance` sorts early and its paint rows are
/// load-sensitive, so three consecutive gates (0.79, 0.80, and the first
/// re-gate) reported on 41 of ~319 test binaries. Everything behind it —
/// including `aterm-forge`'s six baseline tests, which had been RED for two
/// days from a commit that had already landed — never ran, and their silence
/// read as green. An absence of failure is not a pass.
///
/// `--message-format=json-render-diagnostics`: diagnostics print as usual, and
/// cargo's `compiler-artifact` messages name every test executable with its
/// target, which gives the test run each binary's re-run spec
/// ([`crate::testrun`]). The JSON lines are removed from what the ladder shows
/// ([`crate::testrun::take_test_executables`]). The message format is not part
/// of cargo's fingerprint.
#[must_use]
pub fn test_compile_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.push("--no-run".to_string());
    a.push("--message-format=json-render-diagnostics".to_string());
    a
}

/// `targo --unverified test <scope> --no-fail-fast --tests` — the second child,
/// run only when the first compiled. `--tests` runs every lib, bin and
/// integration test target and no doctests (the doctests stage runs those), and
/// adds no units to the `--no-run` graph.
///
/// It skips [`MEASURING_TESTS`] (libtest `--skip` after `--`); [`measuring_args`]
/// runs exactly those, alone.
///
/// Cargo runs it with a recording runner ([`crate::testrun::recording_cmd`]) and
/// the gate runs the recorded binaries several at once
/// ([`crate::testrun::Limits::by_load`]); this argv as is runs whenever the
/// recording does not add up. Every binary gets [`LIBTEST_REPORT_TIME`].
#[must_use]
pub fn test_run_args(scope: &Scope) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.push("--tests".to_string());
    a.push("--".to_string());
    a.extend(LIBTEST_REPORT_TIME.iter().map(|s| (*s).to_string()));
    for name in &MEASURING_TESTS {
        a.push("--skip".to_string());
        a.push((*name).to_string());
    }
    a
}

/// How many rows the test stage's slowest-tests table prints.
pub const SLOWEST_TESTS_LISTED: usize = 25;

/// What the test run hands every test binary after `--`: each verdict line then
/// carries its test's wall time (`test x ... ok <1.234s>`), and the stage prints
/// the slowest ([`crate::libtest::slowest`]).
///
/// The `--tests` targets without libtest's harness, `aterm-objc`'s
/// `main_thread_bound_drive` and `aterm-gui`'s `headless_event_loop` (each needs
/// the main thread, which libtest never gives a test), parse these flags
/// themselves.
/// `crate::libtest` matches verdicts by their leading word, so the suffix
/// changes nothing it decides. Under `-Z` libtest also reads some environment
/// variables; the gate removes them from every child
/// ([`crate::LIBTEST_UNSTABLE_ENV`], in [`crate::CHILD_ENV_REMOVED`]).
pub const LIBTEST_REPORT_TIME: [&str; 2] = ["-Zunstable-options", "--report-time"];

/// The tests that measure (MEASURE tier): libtest name filters for the paint and
/// spin matrices (`aterm-conformance`), under a `measuring` module, which judge
/// the release artifact's frame timings on a live window. The test run skips
/// them; the exclusive `measuring tests` stage (`--measure`, `--full`) runs them.
///
/// The split is exact because the modules are pinned: the test run skips the
/// filter everywhere, and the filtered stage runs it only in the targets that
/// hold it (`measuring` in aterm-conformance's `paint` and `spin` tests).
/// `every_skipped_module_is_declared_where_its_stage_looks` refuses a matching
/// module anywhere else, where no stage would select it.
pub const MEASURING_TESTS: [&str; 1] = ["measuring::"];

/// The targets [`MEASURING_TESTS`] live in (`--test` names, in every selected
/// package that has one: only aterm-conformance does).
pub const MEASURING_TARGETS: [&str; 4] = ["--test", "paint", "--test", "spin"];

/// `targo --unverified test <scope> --no-fail-fast <targets> -- <filters>` — the
/// test run's own package selection, narrowed to the targets that hold the tests
/// it skipped and filtered to exactly them.
///
/// The scope is not narrowed: cargo resolves features over the selected
/// packages, so a per-package child (`-p aterm-conformance --test paint`) would
/// resolve dependencies at other features and recompile inside an exclusive
/// stage. With the run's own selection every unit is the test compile's.
fn filtered_test_args(scope: &Scope, targets: &[&str], filters: &[&str]) -> Vec<String> {
    let mut a = test_base_args(scope);
    a.extend(targets.iter().map(|s| (*s).to_string()));
    a.push("--".to_string());
    a.extend(filters.iter().map(|s| (*s).to_string()));
    a
}

/// `targo --unverified test <scope> --no-fail-fast --test paint --test spin --
/// measuring::` — the MEASURE tier's exclusive stage's test child. In a `--full`
/// run the compile is the test stage's; a `--measure` run compiles here.
#[must_use]
pub fn measuring_args(scope: &Scope) -> Vec<String> {
    filtered_test_args(scope, &MEASURING_TARGETS, &MEASURING_TESTS)
}

fn test_base_args(scope: &Scope) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "test".to_string()];
    a.extend(scope.args());
    a.push("--no-fail-fast".to_string());
    a
}

/// `targo --unverified test --doc <scope> --no-fail-fast` — the only doctest
/// runner (the test stage runs `--tests`). `--no-fail-fast` for the reason
/// [`test_compile_args`] gives. The `[profile.dev.build-override]` pin keeps
/// this graph to the test compile's units plus the doctests.
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
/// the smokes and the sealed rung drive.
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

/// `targo --unverified test -p aterm-link --features sealed --test two_nodes_sealed --test world --no-fail-fast`
///
/// The sealed cross-host rung, and the world suite under the same feature: its
/// `sealed`-only tests (r16_second_host) compile nowhere else, and one sat red
/// there unseen (2026-10-06). `tests/two_nodes_sealed.rs` is `#![cfg(feature =
/// "sealed")]`, so the workspace test stage compiles it to nothing; this is the
/// one run of the vendored astream-aead test. The rung's second child, in the
/// driver lane behind the `aterm-gui` build it drives ([`sealed_lane_cmds`]).
#[must_use]
pub fn sealed_lane_args() -> Vec<String> {
    [
        "--unverified",
        "test",
        "-p",
        "aterm-link",
        "--features",
        "sealed",
        "--test",
        "two_nodes_sealed",
        "--test",
        "world",
        "--no-fail-fast",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `targo --unverified build -q -p atpkg` — the binary the end-to-end pack
/// drives, built in the driver lane as that stage's first child and handed to
/// the suite ([`atpkg_pack_cmd`]), so the script never falls back to a previous
/// run's artifact.
#[must_use]
pub fn atpkg_build_args() -> Vec<String> {
    ["--unverified", "build", "-q", "-p", "atpkg"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// `<tippy> <scope> --all-targets --keep-going -- -D warnings --cfg clean_islands`
/// ([`LINT_TAIL`]).
///
/// `--keep-going` is the coverage flag: without it cargo stops scheduling at the
/// first failing unit, so under `-D warnings` the first crate with a finding ends
/// the run and the lint reports a prefix of the workspace while reading like all
/// of it.
///
/// A member whose dependency fails to compile still cannot be linted. `gate
/// lint` says so, and the differential verdict never inherits such a row from
/// main ([`crate::differential::lint_reached_every_unit`]).
#[must_use]
pub fn tippy_args(scope: &Scope) -> Vec<String> {
    let mut a = scope.args();
    a.extend(LINT_TAIL.map(String::from));
    a
}

/// What both lint passes end with: cargo's coverage flags, then the lint
/// driver's arguments after the separator.
///
/// `--cfg clean_islands` CLOSES A COVERAGE HOLE (2026-10-01). The cfg gates
/// aterm-gui's two `clean { … }` proof islands (`mod island;` in
/// `app_update_handoff.rs` and `config_watcher.rs`) and aterm-log's
/// `env_mutation` allows, and every other Trust lane gets it from
/// `.cargo/config.toml`'s `[target.'cfg(trust_verify)']` table. The lint never
/// did: cargo matches that table against the cfg the workspace compiler prints,
/// and `tippy-driver` prints no `trust_verify` (`trustc --print cfg` does) — so
/// no tippy unit carried the table's flags and both island modules went
/// unlinted. MEASURED 2026-10-01 (store build 9192, trustc 321aaeda7) on a
/// scratch crate under a copy of the table, guarded by `#[cfg(not(clean_islands))]
/// compile_error!`: `targo --unverified check` compiled it, `targo-tippy -- -D
/// warnings` stopped on the guard, and this tail compiled it.
///
/// After the separator is where it applies to the workspace and nothing else:
/// targo-tippy hands those words to `tippy-driver` in its argument channel
/// (`CLIPPY_ARGS`/`TIPPY_ENCODED_ARGS`), and `tippy-driver` is cargo's
/// `RUSTC_WORKSPACE_WRAPPER`, so only workspace members are compiled through
/// it. Measured the same day: a path dependency outside the scratch workspace,
/// guarded by `#[cfg(clean_islands)] compile_error!`, compiled under this tail,
/// and adding the cfg to a warm target dir left that dependency `Fresh` and
/// re-linted only the member. A `RUSTFLAGS` or `--config` rustflags spelling
/// would have reached, and rebuilt, every unit. In place the same day,
/// `-p aterm-gui -p aterm-log --all-targets` under this tail passed (70 s in a
/// warm `target-tippy`), the Clean kernel's note naming each island — notes
/// cargo replays when the unit is fresh, so a red lint's fingerprint does not
/// depend on whether aterm-gui was re-linted.
pub const LINT_TAIL: [&str; 7] = [
    "--all-targets",
    "--keep-going",
    "--",
    "-D",
    "warnings",
    "--cfg",
    "clean_islands",
];

/// The workspace's `required-features` targets, as the `(package, feature)`
/// pairs that switch them on — READ FROM THE MANIFESTS (`crates/*/Cargo.toml`,
/// the workspace's members), sorted and without repeats.
///
/// `--all-targets` builds none of those targets: cargo skips a target whose
/// `required-features` are off, silently and without a word in its output. So
/// the main tippy pass never compiles `aterm-gui`'s `control-conformance`
/// redraw harness. Derived rather than listed, so a new gated target is linted
/// the day it is declared and a retired feature leaves the pass the same day.
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

/// `<tippy> -p <pkg>… --features <pkg/feat>,… <LINT_TAIL>` — the SECOND pass,
/// the one that reaches the targets [`tippy_args`] cannot, over `gated`
/// ([`gated_lint_features`]), ending as the first does ([`LINT_TAIL`]).
///
/// A separate invocation because features belong to specific packages: cargo
/// offers `-p <pkg> --features <pkg>/<feat>`, not a workspace-wide switch. It
/// re-lints the named packages and builds the dependency variants their wider
/// feature set resolves.
///
/// `None` when the scope selects no gated package, so a narrowed run does not
/// compile crates it narrowed away.
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

/// `targo --unverified run -q -p xtask -- gate <name>`
#[must_use]
pub fn xtask_gate_args(gate: &str) -> Vec<String> {
    xtask_gate_args_with(gate, &[])
}

/// [`xtask_gate_args`] plus flags for the verb. Separate so the flags a stage
/// passes are visible at the stage rather than buried in a string.
#[must_use]
pub fn xtask_gate_args_with(gate: &str, flags: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = [
        "--unverified",
        "run",
        "-q",
        "-p",
        "xtask",
        "--",
        "gate",
        gate,
    ]
    .into_iter()
    .map(String::from)
    .collect();
    a.extend(flags.iter().map(|f| String::from(*f)));
    a
}

/// `targo --unverified build --manifest-path tools/freeze-safety-gate/Cargo.toml`
#[must_use]
pub fn freeze_gate_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "--manifest-path",
        "tools/freeze-safety-gate/Cargo.toml",
    ]
    .into_iter()
    .map(String::from)
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
    /// for them. Worded apart from `0` so no receipt claims runtime authority.
    WeakerPassOk(&'static str),
    /// The driver's watchdog fired — a modal tracking loop never returned — so
    /// it decided nothing: COULD NOT RUN.
    Hung,
    /// The driver's signal trap turned an abort into `3`, and for this driver
    /// the abort IS the finding it exists to make.
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
    // The only row that touches the window surface; its first run segfaulted
    // on a use-after-free that had compiled clean.
    ObjcDrive {
        package: "aterm-gui",
        example: "objc_window_drive",
        code3: Code3::None,
        no_status: Severity::CouldNotRun,
        pass: "the real window answered every question — title, style mask, geometry, limits, theme, tabs, drag-and-drop, close and fullscreen",
    },
    // v0.72.0 aborted on the first `mouseMoved:`: an abort here — trapped
    // (`3`) or not (no status) — is the finding, never could-not-run.
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

/// The nested workspace's checked-in driver is the oracle contract: one argv, no
/// cargo fallback here, so changes to its cell table reach the merge gate. The
/// target dir is absolute because `run.sh`'s children run from different cwds
/// (the oracle workspace, and a copy of it under `$TMPDIR`), and it is this
/// lane's own so it never shares another Cargo lock.
///
/// # Errors
/// Returns the current-directory error from making a relative repository root
/// absolute. The stage reports that as COULD-NOT-RUN.
pub fn libc_oracle_cmd(ctx: &Ctx) -> std::io::Result<Cmd> {
    let root = std::path::absolute(&ctx.root)?;
    Ok(Cmd::new(root.join("libc-oracle/run.sh"))
        .env("CARGO_TARGET_DIR", root.join("libc-oracle/target"))
        // Python imports inside gen/ would write bytecode into attributed source;
        // set here as well as in run.sh so a gate run cannot dirty its own
        // fingerprint.
        .env("PYTHONDONTWRITEBYTECODE", "1"))
}

/// `libc-oracle/run.sh` exit contract: 0 proves conformance, 1 is a finding, and
/// 3 means a preflight or environment inability decided nothing (the driver's own
/// preflight, or `gen/symgate.py` unable to document the reference libc under
/// Trust). A missing status likewise decided nothing; any other status violates
/// the driver's contract and is a finding.
#[must_use]
pub fn libc_oracle_outcome(code: Option<i32>) -> Outcome {
    match code {
        Some(crate::exit::PASS) => Outcome::Ok,
        Some(crate::exit::COULD_NOT_RUN) | None => Outcome::Fail(Severity::CouldNotRun),
        Some(_) => Outcome::Fail(Severity::GateFailed),
    }
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

/// [`run_labeled`] for a checker whose failing output can show it ran to its end
/// ([`Report::decide_checker_child`]): its finding is inherited from main only
/// when `ran_to_end` says so. Every other child's never is
/// ([`crate::differential::UNFINISHED`]).
fn run_checker(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    ran_to_end: crate::differential::RanToEnd,
) -> bool {
    let out = exec::run(cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    r.decide_checker_child(&out, label, ran_to_end);
    out.ok
}

/// [`run_labeled`] for a child that runs tests: a failure is itemized per failed
/// test ([`Report::decide_test_child`]), which the differential verdict compares
/// with main's receipt ([`crate::differential`]).
fn run_tests_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) -> bool {
    run_child(ctx, r, label, cmd, |r, run, label| {
        r.decide_test_child(run, label);
    })
}

fn run_child(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    decide: fn(&mut Report, &exec::Run, &str),
) -> bool {
    let out = exec::run(cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    decide(r, &out, label);
    out.ok
}

/// [`run_checker`] for the stages that compile the selected crates.
///
/// An empty selection must not fall through to a bare `targo build` (no `-p`
/// builds the whole workspace under a narrow label). Only `--changed` can select
/// nothing (a docs-only branch); that is a counted, named skip.
fn run_scoped(
    ctx: &Ctx,
    r: &mut Report,
    label: &str,
    cmd: &Cmd,
    ran_to_end: crate::differential::RanToEnd,
) {
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    run_checker(ctx, r, label, cmd, ran_to_end);
}

/// A `targo` invocation, always naming its lane.
fn targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    Cmd::new(&ctx.tools.targo).args(args)
}

/// `CARGO_BUILD_JOBS` for a side lane, `None` where cargo's default stays. The
/// side lanes overlap the build, and uncapped each would claim every core; tune
/// from the rows' `  time  ` lines. Cargo does not fingerprint the job count, so a
/// cap never causes a rebuild. The driver lane, whose binaries gate the
/// exclusive smokes, gets twice the verb lanes'.
#[must_use]
pub const fn lane_build_jobs(lane: Lane) -> Option<u32> {
    match lane {
        Lane::XtaskTarget | Lane::TrustTarget => Some(4),
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
/// cap, through the command's environment. `MainTarget`, `Doctests` (the same
/// directory) and `Pure` come back unchanged.
#[must_use]
pub fn in_lane(ctx: &Ctx, lane: Lane, cmd: Cmd) -> Cmd {
    if matches!(lane, Lane::MainTarget | Lane::Doctests | Lane::Pure) {
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

/// A build of a binary the gate drives, in the driver lane. Only the build is
/// [`Cmd::demoted`]; the driven binary runs as its own child at the inherited
/// tier.
#[must_use]
pub fn driver_build_cmd(ctx: &Ctx, args: Vec<String>) -> Cmd {
    in_lane(ctx, Lane::DriverTarget, targo(ctx, args)).demoted()
}

/// The driver lane's target dir: every driven binary is resolved under it.
#[must_use]
pub fn drivers_dir(ctx: &Ctx) -> std::path::PathBuf {
    lane_dir(ctx, Lane::DriverTarget).unwrap_or_else(|| ctx.root.join("target-drivers"))
}

/// A binary the driver lane built: `<target-drivers>/debug/<name>`. Launched
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

/// An xtask verb, in the xtask lane.
fn xtask_cmd(ctx: &Ctx, args: Vec<String>) -> Cmd {
    in_lane(ctx, Lane::XtaskTarget, targo(ctx, args))
}

/// The L0 gate's build in its own lane's target dir (`tools/freeze-safety-gate`
/// is its own workspace), so a caller's export never moves it onto another lock.
fn freeze_gate_cmd(ctx: &Ctx) -> Cmd {
    in_lane(ctx, Lane::FreezeGateTarget, targo(ctx, freeze_gate_args()))
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
/// only for lib targets, so the `Absent` diagnosis must not fire for a scope with
/// none. `--changed` answers from its `Members` table, `--scope <crate>` from the
/// member's manifest, and the workspace always holds libs. Unanswerable answers
/// yes (fail closed, as `Members::any_has_lib` does).
fn scope_compiles_doctests(ctx: &Ctx) -> bool {
    match &ctx.scope {
        Scope::Crate(c) => crate::changed::crate_dir_has_lib(&ctx.root, c),
        s => s.has_lib_target(),
    }
}

/// The rule as a pure truth table. The stage2's own copy outranks everything
/// (its `RUSTDOC` binding overrides even a caller's export); the caller's export
/// outranks the PATH walk, as cargo ranks it; the farm link
/// is the fallback for a stage2 without trustdoc.
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

/// A helper script under `tools/` (or anywhere), run as `<script> <root>`.
fn script_cmd(path: &std::path::Path, root: &std::path::Path) -> Cmd {
    Cmd::new(path).arg(root)
}

// ---------------------------------------------------------------------------
// 1) TEST COMPILE: the test stage's `--no-run` child, at t0. It says COULD NOT
//    RUN when there is no pinned `targo`.
// 2) TEST: compile every test target (`--no-run`, a fingerprint check after the
//    row above), then run them (`--tests`) if that compiled. No doctests here,
//    so no doc-driver diagnosis; trustdoc is still bound when the stage2 has one,
//    so both rows share one environment.
//
//    The `--tests` child runs with a recording runner, and the gate runs the
//    recorded binaries several at once, up to `--test-jobs`, the few that
//    cannot share the machine alone ([`crate::testrun`]). The row is decided
//    from one cargo-shaped log in cargo's order; the serial child runs whenever
//    the recording does not add up, said in one line. Both rows drop cargo's
//    JSON from what they print ([`compile_tests`]).
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
        // COULD-NOT-RUN, not FAILED: nothing about the tree was decided. Never a
        // stock-cargo fallback, which would make the gate quietly unverified.
        r.cannot_run(ctx.tools.missing_targo_label());
        return;
    }
    let label = format!("targo test {} --no-run", ctx.scope.label());
    // The empty change-selection outranks the doc-driver binding, as in the
    // test stage below: a run that compiles nothing names no driver.
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
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
/// clamp would reach it. The run skips [`MEASURING_TESTS`] and carries
/// [`TRAIL_LAWS_FULL`].
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

/// What every suite a failed build kept from running adds to its COULD NOT RUN
/// row.
///
/// Under the differential verdict a build red main already has is inherited, so
/// without this row nothing would stand for the suites behind it, and a branch
/// could break them all and claim the merge contract. Every build that feeds a
/// drive goes through [`build_to_drive`] or says the same. A build's own row is
/// never inherited either ([`crate::differential::UNFINISHED`]).
pub const NOT_RUN_BEHIND: &str = "; nothing behind it was decided, and a red main's receipt \
     excuses never excuses what it kept from running";

/// The COULD NOT RUN row for `what`, which the failure above it kept from
/// running ([`NOT_RUN_BEHIND`]).
pub fn not_run_behind(r: &mut Report, what: &str, because: &str) {
    r.cannot_run(format!("{what} — {because}{NOT_RUN_BEHIND}"));
}

/// Build what a stage drives, in the driver lane: `true` when it built. When it
/// did not, the build's own row and, behind it, a COULD NOT RUN row for `drive`
/// ([`not_run_behind`]); only one row when the machine refused the build, which
/// is COULD NOT RUN already.
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
/// set. Their limit rules are written against the full grid. Read at run time,
/// so the compile child needs no copy and nothing is rebuilt.
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
        r.skip(format!("{label} (change-scoped run selected no crates)"));
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
        // Not a skip (nothing was absent), and not silent: the compile's FAIL may
        // be excused by main's receipt, but the tests it kept from running
        // decided nothing.
        r.cannot_run(format!(
            "{run_label} — the compile above failed, and cargo runs no test after a compile error{}",
            NOT_RUN_BEHIND
        ));
    }
}

// ---------------------------------------------------------------------------
// 2.2) The tests the run above skipped, run exclusively (nothing else in
//    flight) because their verdicts depend on machine load. The test run's own
//    package selection, narrowed to the targets that hold the list and filtered
//    to it; never demoted, since the paint and spin rows launch the aterm they
//    judge. Planned only when the crate is selected (`plan.rs`).
//      * Measuring tests ([`MEASURING_TESTS`]): MEASURE tier. The suites are
//        handed the release bundle the run was given (`--measure <APP>`): the
//        bytes the release signs, built once by the release, never by the gate.
// ---------------------------------------------------------------------------

/// The paint and spin suites' prebuilt-artifact overrides. Keep each name at
/// its child-environment writer so the source guard can distinguish this
/// handoff from an ambient read. The gate sets both to the same artifact: the
/// handed bundle's `Contents/MacOS/aterm` ([`crate::MeasureArtifact::bin`]).
const RELEASE_PAINT_BIN: &str = "ATERM_PAINT_BIN";
const RELEASE_SPIN_BIN: &str = "ATERM_SPIN_BIN";

/// A filtered stage's test child — [`filtered_test_args`] over the run's scope,
/// with trustdoc bound the way the test run binds it so both see one
/// environment — and its ladder label.
fn filtered_test_cmd(ctx: &Ctx, targets: &[&str], filters: &[&str], bind: bool) -> (String, Cmd) {
    let label = format!(
        "targo test {} {}{} -- {}",
        ctx.scope.label(),
        targets.join(" "),
        test_doc_binding(ctx).0,
        filters.join(" ")
    );
    let c = targo(ctx, filtered_test_args(&ctx.scope, targets, filters));
    (label, if bind { with_trustdoc(ctx, c) } else { c })
}

/// The measuring stage's test child: [`measuring_args`], handed the release
/// bundle's `aterm` through both child-environment overrides.
fn measuring_cmd(ctx: &Ctx, artifact: &crate::MeasureArtifact, bind: bool) -> (String, Cmd) {
    let (label, cmd) = filtered_test_cmd(ctx, &MEASURING_TARGETS, &MEASURING_TESTS, bind);
    let bin = artifact.bin("aterm");
    let cmd = cmd
        .env(RELEASE_PAINT_BIN, bin.as_os_str())
        .env(RELEASE_SPIN_BIN, bin.as_os_str());
    (label, cmd)
}

fn measuring_tests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("measuring tests (no targo)");
        return;
    }
    // A MEASURE run always names its bundle (`cli`); one without it judged
    // nothing, and says so rather than measuring a binary of its own choosing.
    let Some(artifact) = &ctx.measure_artifact else {
        r.cannot_run("measuring tests: no release bundle was handed to judge (--measure <APP>)");
        return;
    };
    let (label, cmd) = measuring_cmd(ctx, artifact, test_doc_binding(ctx).1);
    run_tests_labeled(ctx, r, &label, &cmd);
}

// ---------------------------------------------------------------------------
// 2.5) Doctests
// ---------------------------------------------------------------------------
fn doctests(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test --doc (no targo)");
        return;
    }
    let label = format!("targo test --doc {}", ctx.scope.label());
    // The empty change-selection outranks every other verdict, same as the test
    // stage directly above.
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    // `cargo test --doc` is an error ("no library targets found") when no
    // selected package has a lib, which is ordinary under `--changed` or
    // `--scope xtask` (bin-only). Ask `scope_compiles_doctests` (the test stage's
    // answer too) and skip honestly; a skip is counted and narrows the verdict.
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
        // means no doctest was decided (COULD-NOT-RUN with the remedy).
        DocDriver::Absent => r.cannot_run(ctx.tools.missing_trustdoc_label()),
    }
}

/// The sealed rung's two children, labelled, in the order they run — the exact
/// commands, so a test asserts on them. Both run in the driver lane and name one
/// target dir.
///
/// `two_nodes_sealed` boots real `aterm-gui`s; its harness
/// (`crates/aterm-link/tests/harness/mod.rs`, `built_binary`) looks in the test
/// binary's own target dir, then `$CARGO_TARGET_DIR`, then `<root>/target`, and
/// refuses one older than any input its depfile names. The first child (the
/// smokes' build argv, a fingerprint no-op after the driver builds row) makes
/// the first search answer with a binary built from the sources under test.
#[must_use]
pub fn sealed_lane_cmds(ctx: &Ctx) -> [(String, Cmd); 2] {
    [
        (
            "targo build -p aterm-gui -p aterm-ctl (the aterm-gui the sealed rung drives)"
                .to_string(),
            driver_build_cmd(ctx, smoke_build_args()),
        ),
        (
            "targo test -p aterm-link --features sealed --test two_nodes_sealed --test world"
                .to_string(),
            in_lane(ctx, Lane::DriverTarget, targo(ctx, sealed_lane_args())),
        ),
    ]
}

fn sealed_lane(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("sealed fabric lane (no targo)");
        return;
    }
    let [(build_label, build), (run_label, run)] = sealed_lane_cmds(ctx);
    // An integration-test-only run compiles no doctests, so it has no
    // doc-driver rule to take: both children run as written.
    let built = run_labeled(ctx, r, &build_label, &build);
    if built {
        run_labeled(ctx, r, &run_label, &run);
    } else {
        // Not a skip: running the suite would drive a missing or stale
        // `aterm-gui`, and the suite it keeps from running decided nothing
        // ([`NOT_RUN_BEHIND`]).
        r.cannot_run(format!(
            "{run_label} — the aterm-gui build above failed, so this suite has no fresh aterm-gui to drive{}",
            NOT_RUN_BEHIND
        ));
    }
}

// ---------------------------------------------------------------------------
// 2.7) Tippy: the lint gate under the Trust toolchain's Clippy fork. Detected
//    and fail closed: present means `-D warnings` must pass; absent is an honest
//    skip. A separate target dir keeps its artifacts off the main build's.
// ---------------------------------------------------------------------------
/// The exact command the lint stage spawns, so the test asserts on it rather
/// than a replica. `CARGO_TARGET_DIR` must not go missing: the two lanes would
/// clobber each other's caches.
fn tippy_cmd(ctx: &Ctx, bin: &std::path::Path, args: Vec<String>) -> Cmd {
    let dir = bin.parent().unwrap_or(&ctx.tools.stage2_dir).to_path_buf();
    let mut path = std::ffi::OsString::from(dir.as_os_str());
    path.push(":");
    path.push(&ctx.path_env);
    Cmd::new(bin)
        .args(args)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", ctx.root.join("target-tippy"))
        .env("TRUST_NO_MIGRATE_WARN", "1")
        // A lint only compiles.
        .demoted()
}

fn tippy(ctx: &Ctx, r: &mut Report) {
    let Some(bin) = ctx.tools.tippy.clone() else {
        r.skip(ctx.tools.missing_tippy_label());
        return;
    };
    // A lint red main has is inherited only when it kept no unit from being
    // linted ([`crate::differential::lint_reached_every_unit`]): one in a
    // library leaves every crate built on it unlinted.
    run_scoped(
        ctx,
        r,
        &format!("tippy {} -D warnings", ctx.scope.label()),
        &tippy_cmd(ctx, &bin, tippy_args(&ctx.scope)),
        crate::differential::lint_reached_every_unit,
    );
    // The second pass, `--full` only: `--all-targets` above built no target whose
    // `required-features` are off, so those ([`gated_lint_features`]) are linted
    // here or nowhere. They are benches and a harness bin, not the shipped
    // build. Its own row, so the ladder shows whether it ran.
    if ctx.mode == crate::Mode::Full
        && let Some(args) = tippy_gated_args(&ctx.scope, &gated_lint_features(&ctx.root))
    {
        run_scoped(
            ctx,
            r,
            "tippy required-features targets -D warnings",
            &tippy_cmd(ctx, &bin, args),
            crate::differential::lint_reached_every_unit,
        );
    }
}

/// Formatting: `xtask gate lint --fmt-only`, the formatter lane's three passes.
/// It needs no compiler (trustfmt parses and prints) and reaches crates outside
/// `members = ["crates/*"]` that `targo-fmt --all` cannot see. The `XtaskTarget`
/// lane is for the xtask binary this shells through.
///
/// No stage-level skip beyond the shared one (no targo, so the verb cannot be
/// built): the lane itself separates drift (a finding) from a formatter that
/// could not run, and a skip here could hide the latter.
fn formatting(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("formatting (no targo)");
        return;
    }
    // Inherited only when the verb printed its verdict with every pass read
    // ([`crate::differential::fmt_ran_to_end`]).
    run_checker(
        ctx,
        r,
        "gate lint --fmt-only",
        &xtask_cmd(ctx, xtask_gate_args_with("lint", &["--fmt-only"])),
        crate::differential::fmt_ran_to_end,
    );
}

// ---------------------------------------------------------------------------
// 3) Grep guards and license headers (zero tolerance, always whole-tree): two
//    script children, each its own decision and each run whatever the other
//    decided. `license_check.sh` holds every `.rs` to its two-line SPDX header.
//    A missing script is a cannot-run, never a skip.
// ---------------------------------------------------------------------------

/// The scripts the grep-guards stage runs, in order.
pub const GUARD_SCRIPTS: [&str; 2] = ["grep_guard.sh", "license_check.sh"];

fn grep_guards(ctx: &Ctx, r: &mut Report) {
    for name in GUARD_SCRIPTS {
        let script = ctx.tools_dir().join(name);
        if !is_executable_file(&script) {
            r.cannot_run(format!(
                "{name} missing or not executable ({})",
                script.display()
            ));
            continue;
        }
        let out = exec::run(&script_cmd(&script, &ctx.root), ctx.exec_env());
        r.raw(out.output.as_str());
        r.decide_checker_child(&out, name, guard_ran_to_end(name));
    }
}

/// How a guard script's output shows it ran every check: its closing verdict
/// line ([`crate::differential::guard_ran_to_end`],
/// [`crate::differential::license_ran_to_end`]). A script this does not know
/// never shows it.
fn guard_ran_to_end(name: &str) -> crate::differential::RanToEnd {
    match name {
        "grep_guard.sh" => crate::differential::guard_ran_to_end,
        "license_check.sh" => crate::differential::license_ran_to_end,
        _ => |_| Err(crate::differential::UNFINISHED),
    }
}

// ---------------------------------------------------------------------------
// 3.2) Export content: `tools/export-content-scan.py`, the publication engine's
//    content guards over the export `pub stage` would build from this tree, run
//    by the engine's own code imported from its checkout. An early warning; the
//    release preflight's engine dry run stays the whole verdict. A missing
//    script is a cannot-run; no engine checkout or no gitleaks is a named skip.
// ---------------------------------------------------------------------------

/// The export content scan's script, under `tools/`.
pub const EXPORT_CONTENT_SCRIPT: &str = "export-content-scan.py";

/// The scan's not-run code: no publication engine checkout (`$PUBLICATION_ENGINE`,
/// else `~/Developer/publication` (`~/publication` until it moves there)) or no `gitleaks`. Read like the Codex lane's `77`
/// ([`export_content_outcome`]).
pub const EXPORT_CONTENT_NOT_RUN: i32 = 77;

/// `tools/export-content-scan.py <root>`, with a `SIGTERM` grace: the scan
/// writes a scratch export of the tree (~150 MB) that only its own exit
/// removes, and a gate that ends it early should not leave that behind.
#[must_use]
pub fn export_content_cmd(ctx: &Ctx) -> Cmd {
    script_cmd(&ctx.tools_dir().join(EXPORT_CONTENT_SCRIPT), &ctx.root).term_grace(exec::TERM_GRACE)
}

/// How the ladder reads the scan's exit. Tested, because some codes are not
/// verdicts about the tree:
///
/// * `77`: not run (no engine checkout, or no `gitleaks`). The patterns are the
///   engine's and never copied here, so there is nothing to scan with. A named
///   skip: counted, printed with the scan's reason, forfeiting the contract
///   claim.
/// * `3`: could not run. The engine could not be driven (import failed, an API
///   changed, a call raised, a scanner crashed or timed out), the scan stopped
///   before its verdict, or a `SIGTERM` ended it.
///
/// A verdict is a line, not a code: `0` passes only with `EXPORT CONTENT: PASS`,
/// and `1` is a finding only with `EXPORT CONTENT: FAIL` (hits named at their
/// source file:line with pattern class) or `EXPORT CONTENT: REFUSED` (the engine
/// refused the export). Without one the scan ended in a way it did not choose
/// (an uncaught Python exception also exits `1`): could not run. Any other code
/// is a finding.
#[must_use]
pub fn export_content_outcome(code: Option<i32>, transcript: &str) -> (Outcome, String) {
    let name = EXPORT_CONTENT_SCRIPT;
    let said = |prefix: &str| {
        transcript
            .lines()
            .rev()
            .find_map(|l| l.strip_prefix(prefix))
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let verdict = |v: &str| transcript.lines().any(|l| l.starts_with(v));
    let undecided = |code: i32, verdicts: &str| {
        (
            Outcome::Fail(Severity::CouldNotRun),
            format!(
                "{name}: exit {code} without its {verdicts} verdict line, so \
                 nothing shows a guard decided (the scan's own output is above)"
            ),
        )
    };
    match code {
        Some(crate::exit::PASS) if verdict(crate::differential::EXPORT_CONTENT_VERDICT_PASSED) => {
            (Outcome::Ok, name.to_string())
        }
        Some(crate::exit::PASS) => undecided(0, "`EXPORT CONTENT: PASS`"),
        Some(1) if verdict(crate::differential::EXPORT_CONTENT_VERDICT_FAILED) => (
            Outcome::Fail(Severity::GateFailed),
            format!(
                "{name}: the publication engine's content guards refuse this tree's export — \
                 the rows above name each hit at its source file:line with its pattern class"
            ),
        ),
        Some(1) if verdict(crate::differential::EXPORT_CONTENT_VERDICT_REFUSED) => (
            Outcome::Fail(Severity::GateFailed),
            format!(
                "{name}: the publication engine refuses to export this tree before its content \
                 guards run — its own words are above"
            ),
        ),
        Some(1) => undecided(1, "`EXPORT CONTENT: FAIL` or `REFUSED`"),
        Some(EXPORT_CONTENT_NOT_RUN) => (
            Outcome::Skip,
            format!(
                "{name}: NOT RUN — {} (exit 77: this machine lacks a prerequisite; a named skip, \
                 never a pass)",
                not_run_reason(transcript).unwrap_or("the scan skipped without saying why")
            ),
        ),
        Some(crate::exit::COULD_NOT_RUN) => (
            Outcome::Fail(Severity::CouldNotRun),
            format!(
                "{name}: {} (exit 3)",
                said("COULD NOT RUN: ").unwrap_or("the scan could not run and did not say why")
            ),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: unexpected exit {c} (the scan answers only 0, 1, 3 and 77)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            format!("{name}: no exit status — killed by a signal, or never spawned"),
        ),
    }
}

fn export_content(ctx: &Ctx, r: &mut Report) {
    let script = ctx.tools_dir().join(EXPORT_CONTENT_SCRIPT);
    if !is_executable_file(&script) {
        r.cannot_run(format!(
            "{EXPORT_CONTENT_SCRIPT} missing or not executable ({})",
            script.display()
        ));
        return;
    }
    let out = exec::run(&export_content_cmd(ctx), ctx.exec_env());
    r.raw(out.output.as_str());
    if r.child_could_not_run(&out, EXPORT_CONTENT_SCRIPT) {
        return;
    }
    let (outcome, label) = export_content_outcome(out.code, &out.output);
    if outcome == Outcome::Fail(Severity::GateFailed) {
        // One finding per hit, keyed by source path and pattern class (never
        // its line), so main's hits stay main's red when edits move them; a
        // refused export is the one row, never inherited
        // (`differential::export_content_findings`).
        r.fail_itemized_child(&out, label, crate::differential::export_content_findings);
    } else {
        r.record(outcome, label);
    }
}

// ---------------------------------------------------------------------------
// 3.5) Delivery tooling: the offline shell suites over the scripts that take
//    the app and its toolchain packages from a checkout to someone's machine (a
//    dev bundle, the installer, a cut, the site, the atpkg index). Each is its
//    own decision, runs on its own stubs, and a missing suite is a cannot-run,
//    never a skip. Several run at a time ([`delivery_tooling`]).
// ---------------------------------------------------------------------------

/// The delivery-tooling suites, in the order the stage prints them. Every one is
/// offline and, except [`DELIVERY_ALONE`]'s, self-contained under its own mktemp
/// dir (its header says how).
///
/// * `test-install-channel.sh`: tools/install.sh agrees with the in-app
///   updater's head pointer, tag grammar and asset identity; the Linux lane's
///   head-only target choice.
/// * `test-publish-export.sh`: publish/manifest.txt and publish/transforms.sh as
///   the publication engine runs them, the only check that the export's
///   rewrites stay narrow; config.sh's `aterm MAJOR.MINOR.0` version shape.
/// * `test-install-guard.sh`: install.sh's whole-file execution guard and its
///   `--dry-run` zero-mutation contract, under stubbed `curl`/`gh`.
/// * `test-cargo-pin.sh`: `tools/dev-app.sh` and install.sh build only under the
///   cargo that honours rust-toolchain.toml.
/// * `test-site-sync.sh`: publish/post-promote over a local bare repo and stub
///   `gh`, `curl`, `firebase` and site scripts (about a minute).
/// * `test-dev-sign-id.sh`: tools/dev-sign-id.sh through its `--keychain` seam,
///   a scratch keychain file under ~/Library/Keychains, never on the search
///   list, deleted on exit. Passes with 0 checks off macOS.
/// * `test-agent-fabric-demo.py`: tools/agent-fabric-demo.py keeps its runtime
///   root when a run fails and removes it when one passes (drives `close`
///   alone; no aterm or broker).
/// * `test-atpkg-vendor-tooling.sh`: the https publish rows, the spec flags
///   column, the mirror's release-asset reader, the publish lock, the signed
///   size of a packed tree.
/// * `test-atpkg-mirror-extras.sh`: tools/atpkg-mirror-public.sh's no-extras
///   gate, download retry, and a work list covering `pin` and every
///   `pin_by_target` overlay.
/// * `test-atpkg-prerelease-gate.sh`: every atpkg release create carries
///   `--prerelease`.
/// * `test-atpkg-target-pins.sh`, `test-atpkg-index-target-pins.sh`: per-target
///   pins, rendered by the indexer as the client reads them.
/// * `test-atpkg-index-publish.sh`: baseline+1, packs first, compare-and-swap,
///   never-clobber.
/// * `test-atpkg-spec-catch-up.sh`: the rustc-group lane's catch-up to the
///   public baseline and its preflight refusals.
/// * `test-atpkg-auto-alab.sh`: the ALab lane builds only what was signed (a
///   skip off macOS).
/// * `test-linux-auto-atpkg.sh`: the Linux stager serves every Linux triple the
///   client can ask for.
/// * `test-atpkg-pack-toolchain.sh`: the packers resolve the store's `targo`,
///   and a sysroot bundle signs the version of what it packs.
/// * `test-bootstrap-publisher.sh`: tools/bootstrap-publisher.sh's toolchain
///   walk (tools/lib-trust-order.sh) and rustup link, its install of
///   publish/config.sh's stock wasm32 pin by name (a rustup it installs has
///   auto-install off), and `--from-source` building Trust from the pinned Trust
///   seed (`--require-seed`; never a genesis fallback or a stock stage0) and
///   sealing it. Passes with 0 checks off macOS or without Rosetta.
/// * `test-export-content-scan.sh`: tools/export-content-scan.py against the
///   engine's code over fixture trees; without the engine and gitleaks it runs
///   its not-run cases only and says so.
/// * `test-linux-bootstrap.py`: tools/linux_release_verify.py over signed
///   synthetic fixtures; install.sh's embedded copy is byte-identical to it,
///   and its `MASTER_PUBKEYS` equal `pins.rs`'s `PAPER_MASTER_PUBKEYS`. Needs
///   Python 3.11+ and an OpenSSL 3 (first on PATH, or an installed Homebrew
///   `openssl@3`), and says so when either is missing.
/// * `test_atpkg_trust_closure.py`, `test_atpkg_pack_bundle.py`: the frozen
///   rustc closure and the bundle hygiene phase of tools/atpkg-pack-bundle.sh,
///   over a promoter-shaped seal fixture.
///
/// The Python suites run under the newest `python3.1x` on PATH.
///
/// The atpkg suites lay their own stub `atpkg` shims; only [`ATPKG_DRIVEN_SUITE`]
/// drives a real one. `test-atpkg-pack-arch-gate.sh` is run by aterm-release's
/// `pack_arch_gate` test; the release command's own steps and checks
/// (`targo --unverified ship release`) are aterm-release's tests.
pub const DELIVERY_SUITES: [&str; 22] = [
    "test-install-channel.sh",
    "test-publish-export.sh",
    "test-install-guard.sh",
    "test-cargo-pin.sh",
    "test-site-sync.sh",
    "test-dev-sign-id.sh",
    "test-agent-fabric-demo.py",
    "test-atpkg-vendor-tooling.sh",
    "test-atpkg-mirror-extras.sh",
    "test-atpkg-prerelease-gate.sh",
    "test-atpkg-target-pins.sh",
    "test-atpkg-index-target-pins.sh",
    "test-atpkg-index-publish.sh",
    "test-atpkg-spec-catch-up.sh",
    "test-atpkg-auto-alab.sh",
    "test-linux-auto-atpkg.sh",
    "test-atpkg-pack-toolchain.sh",
    "test-bootstrap-publisher.sh",
    "test-export-content-scan.sh",
    "test-linux-bootstrap.py",
    "test_atpkg_trust_closure.py",
    "test_atpkg_pack_bundle.py",
];

/// One delivery suite's command, with a `SIGTERM` grace: each suite cleans up in
/// an EXIT trap a `SIGKILL` would skip (for `test-dev-sign-id.sh`, a scratch
/// keychain in the real `~/Library/Keychains`; see [`Cmd::term_grace`]).
#[must_use]
pub fn delivery_suite_cmd(ctx: &Ctx, name: &str) -> Cmd {
    Cmd::new(ctx.tools_dir().join(name)).term_grace(exec::TERM_GRACE)
}

/// The delivery suites that run alone, each before the pool starts, because of
/// the shared state named beside it. Every other suite keeps what it touches
/// (HOME, TMPDIR, stub tools first on PATH, scratch git repositories) under its
/// own mktemp dir and runs [`delivery_jobs`] at a time.
pub const DELIVERY_ALONE: [(&str, &str); 1] = [(
    "test-dev-sign-id.sh",
    "creates, locks and deletes scratch keychains through the real `security` in the \
     owner's real ~/Library/Keychains, and fails if the user keychain search list it \
     read at its start has changed by its end",
)];

/// Why the delivery suite `name` must run alone, if it must ([`DELIVERY_ALONE`]).
#[must_use]
pub fn delivery_alone(name: &str) -> Option<&'static str> {
    DELIVERY_ALONE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, why)| *why)
}

/// How many delivery suites run at once on a machine of `cores`: one per four
/// cores, between one and four. Each suite is about one busy core, and the stage
/// runs at t0 beside the test compile and the lint. Four at most
/// because past that the longest suite alone sets the stage's wall.
#[must_use]
pub fn delivery_jobs(cores: u32) -> usize {
    usize::try_from(cores / 4).unwrap_or(1).clamp(1, 4)
}

/// Each delivery suite's last wall time, beside the test run's
/// ([`crate::testrun::COSTS_FILE`]), so the pool starts the longest first.
pub const DELIVERY_COSTS_FILE: &str = "delivery-costs.tsv";

/// One suite's run, when it started after the stage did, and how long it took.
type SuiteRun = (exec::Run, std::time::Duration, std::time::Duration);

/// Run the suites concurrently and print them as a serial run would: the
/// [`DELIVERY_ALONE`] suites first, one at a time, then the rest
/// [`delivery_jobs`] at a time, longest first by [`DELIVERY_COSTS_FILE`]
/// ([`crate::testrun::longest_first`]). Each suite's output, row and indented
/// `time` line print in [`DELIVERY_SUITES`] order. Each suite is its own child
/// with its own ceiling and verdict: a failure is a finding, an environment
/// failure or a missing suite COULD NOT RUN, and nothing is retried.
fn delivery_tooling(ctx: &Ctx, r: &mut Report) {
    let tools = ctx.tools_dir();
    let present: Vec<bool> = DELIVERY_SUITES
        .iter()
        .map(|name| is_executable_file(&tools.join(name)))
        .collect();
    let stage = std::time::Instant::now();
    let run = |i: usize| -> SuiteRun {
        let begun = stage.elapsed();
        let out = exec::run(&delivery_suite_cmd(ctx, DELIVERY_SUITES[i]), ctx.exec_env());
        (out, begun, stage.elapsed().saturating_sub(begun))
    };
    let (alone, shared): (Vec<usize>, Vec<usize>) = (0..DELIVERY_SUITES.len())
        .filter(|&i| present[i])
        .partition(|&i| delivery_alone(DELIVERY_SUITES[i]).is_some());
    let mut runs: Vec<Option<SuiteRun>> = (0..DELIVERY_SUITES.len())
        .map(|i| alone.contains(&i).then(|| run(i)))
        .collect();
    let costs_path = crate::receipt::dir(&ctx.caller_root())
        .ok()
        .and_then(|receipts| receipts.parent().map(|d| d.join(DELIVERY_COSTS_FILE)));
    let costs = costs_path
        .as_deref()
        .map(crate::testrun::read_costs)
        .unwrap_or_default();
    let order = crate::testrun::longest_first(&shared, |i| DELIVERY_SUITES[i], &costs);
    let jobs = delivery_jobs(ctx.cores()).min(shared.len().max(1));
    let pooled = crate::testrun::pool(DELIVERY_SUITES.len(), &order, jobs, run);
    for (slot, ran) in runs.iter_mut().zip(pooled) {
        if slot.is_none() {
            *slot = ran;
        }
    }
    if let Some(path) = &costs_path {
        let measured: Vec<(String, f64)> = DELIVERY_SUITES
            .iter()
            .zip(&runs)
            .filter_map(|(name, ran)| Some(((*name).to_string(), ran.as_ref()?.2.as_secs_f64())))
            .collect();
        crate::testrun::write_costs(path, &measured);
    }
    // What ran, counted over the suites present (a missing one is its own row
    // below). The [`DELIVERY_ALONE`] ones ran by themselves, hence "up to".
    let ran = alone.len() + shared.len();
    if ran > 0 {
        r.raw(format!("  delivery suites: {ran}, up to {jobs} at a time"));
    }
    for (i, name) in DELIVERY_SUITES.into_iter().enumerate() {
        match &runs[i] {
            Some((out, begun, took)) => {
                r.raw(out.output.as_str());
                r.decide_child(out, name);
                // The stage's own `time` spelling, indented under the row: one
                // `  time  ` line per stage is what the ladder's readers count.
                r.raw(format!("  {}", crate::time_line(*begun, *took, None)));
            }
            None if present[i] => r.cannot_run(format!("{name}: never run (a gate defect)")),
            None => r.cannot_run(format!(
                "{name} missing or not executable ({})",
                tools.join(name).display()
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// 3.52) atpkg end-to-end pack: `tools/test-atpkg-pack-one-compiler.sh`, the
//    sysroot-bundle pack's one-compiler contract (atpkg-pack-bundle.sh), keyless
//    and offline under one mktemp dir; its section D packs with a real `atpkg`.
//    A driver-lane stage: its first child builds that binary in this lane, the
//    pack runs only if that succeeded, with $ATPKG pointing at it. A missing
//    suite is a cannot-run, never a skip.
// ---------------------------------------------------------------------------

/// The one atpkg suite that drives a real `atpkg` rather than stubs of its own
/// making: section D of the pack contract, the end-to-end pack.
pub const ATPKG_DRIVEN_SUITE: &str = "test-atpkg-pack-one-compiler.sh";

/// The label of this stage's first child — the build the suite below depends on.
pub const ATPKG_BUILD_LABEL: &str = "targo build -p atpkg (the atpkg the pack suite drives)";

/// The binary [`ATPKG_DRIVEN_SUITE`] is pointed at: this lane's own
/// `debug/atpkg`, written by this stage's first child. Never
/// `<root>/target/debug/atpkg`, which another lane may still be linking or a
/// `--scope` run never builds.
#[must_use]
pub fn atpkg_driven_binary(ctx: &Ctx) -> std::path::PathBuf {
    driver_bin(ctx, "atpkg")
}

/// The build of that binary, in the driver lane. Compile-only, so demoted.
#[must_use]
pub fn atpkg_build_cmd(ctx: &Ctx) -> Cmd {
    driver_build_cmd(ctx, atpkg_build_args())
}

/// The pack suite's command, with `$ATPKG` set to the binary this stage just
/// built: the script's own fallback is `<root>/target/{debug,release}/atpkg`,
/// which could be a previous run's binary.
#[must_use]
pub fn atpkg_pack_cmd(ctx: &Ctx) -> Cmd {
    delivery_suite_cmd(ctx, ATPKG_DRIVEN_SUITE).env("ATPKG", atpkg_driven_binary(ctx))
}

fn atpkg_tooling(ctx: &Ctx, r: &mut Report) {
    let name = ATPKG_DRIVEN_SUITE;
    let t = ctx.tools_dir().join(name);
    if !is_executable_file(&t) {
        r.cannot_run(format!(
            "{name} missing or not executable ({})",
            t.display()
        ));
        return;
    }
    if !ctx.tools.have_targo() {
        // Counted and named: with no toolchain nothing can build the binary,
        // and section D's coverage is then genuinely absent from the run.
        r.skip(format!(
            "{name} (no targo — nothing built the atpkg its end-to-end pack drives)"
        ));
        return;
    }
    if run_labeled(ctx, r, ATPKG_BUILD_LABEL, &atpkg_build_cmd(ctx)) {
        run_labeled(ctx, r, name, &atpkg_pack_cmd(ctx));
    } else {
        // Not a skip: running the pack would drive whatever `<root>/target`
        // holds, and the pack the failed build kept from running decided
        // nothing ([`NOT_RUN_BEHIND`]).
        r.cannot_run(format!(
            "{name} — the atpkg build above failed, so the end-to-end pack has no fresh binary to drive{}",
            NOT_RUN_BEHIND
        ));
    }
}

// ---------------------------------------------------------------------------
// 3.56) Trust contract probe: two facts about the installed stage2 that no cargo
//    stage exercises while every build runs `--unverified`. The off-switch must
//    compile a contract without an ICE, and a `self`-field postcondition must
//    prove (tools/trust-probes/self_field_ensures.rs). Skips without the
//    toolchain like every stage2 stage; a missing script is a cannot-run.
// ---------------------------------------------------------------------------
fn trust_contract_probe(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("trust contract probe (no stage2 toolchain)");
        return;
    }
    let t = ctx.tools_dir().join("test-trust-contract-probe.sh");
    if is_executable_file(&t) {
        run_labeled(ctx, r, "test-trust-contract-probe.sh", &Cmd::new(&t));
    } else {
        r.cannot_run(format!(
            "test-trust-contract-probe.sh missing or not executable ({})",
            t.display()
        ));
    }
}

// ---------------------------------------------------------------------------
// 3.55) THE TRUST VERIFICATION LANE, ADVISORY (2026-09-27, decided under the
//    owner's standing direction of 2026-08-30: every repo compiles under Trust
//    with verification ON). Every build here runs `targo --unverified`, whose
//    off-switch is its own (`.cargo/config.toml` carries none since 2026-10-07,
//    so a command that names no lane verifies, fail-closed); THIS stage is
//    where verification runs: `tools/trust-gate-all.sh`,
//    i.e. `targo trust check <lib> --lib --allow-l0-gaps` per library under the
//    lane's unoptimized `trust-lane` profile (tools/trust-gate-specs.py: dev at
//    opt-level 0, dev's speed rows zeroed, so the verifier reads the code as
//    written) — targo verifies the one library and switches every dependency
//    unit off itself —
//    every obligation verified and reported. The run FAILS when a library's
//    proved count falls below the floor its prover recorded in
//    `tools/trust-gate-ratchet.tsv`, when a verified library has no floor, or
//    when its verification broke; a drop below a floor that trustd's refusals
//    cover, or a check targo would not start beside a trustd it cannot prove its
//    own build's (another build's, or one too loaded to answer — machine
//    contention, 2026-09-29), or a DIRTY trustd crash epoch, is COULD NOT RUN. In
//    order: the script's own verdict self-test (stubbed; 3.5 to 6 minutes at load
//    70-120, 2026-09-29); the ratchet's UP-ONLY check against the shared branch
//    ([`lowered_floors`] — a floor goes down only onto the prover the lane runs,
//    or with its reason written in the row and its total down at least as far,
//    a total the lane then holds to the one it measures); the lane.
//
//    WHICH LIBRARIES, in every mode. Measured 2026-09-27 on the store seal
//    (trustc 321aaeda7; docs/measured/trust-advisory-lane-2026-09-27.md): the
//    median library is about a minute of CPU, the tree 5.6 CPU-hours, and
//    aterm-gui alone ran past 3 h 30 min wall. So the merge contract verifies
//    the libraries and admitted forks whose OWN files the change touched, and
//    not their dependents: a library's rows are its own functions', though the
//    seal re-derives the bodies of workspace callees into them
//    (`-Ztrust-bundle-foreign-crate`), so an edit to a dependency can move a
//    dependent's rows (docs/measured/kani-and-trust-samples-2026-09-30.md: an
//    overflowing edit to aterm-hash's `add_to_hash`, checked in four of its
//    dependents, moved nothing beyond run-to-run noise). The release's whole-tree
//    run re-checks every dependent, and that re-check, not the sample, is why
//    this tier may leave them out. It skips the scope list's `full-only` libraries
//    (`tools/trust-gate-scope.tsv`: ten CPU-minutes or more each), naming them
//    in its row. The WHOLE tree, those included, is the release's: a whole-tree
//    run files the facts it measured for its tree (its floors aside), and the
//    cutter refuses a commit whose facts do not pass its own floors
//    (aterm-release `gates::trust_receipt_gate`, `trust_facts`) — so no floor
//    drops into a release unverified. Not `--full`'s: `--full` is stage children under the
//    gate's three-hour ceiling, and the whole tree does not fit in one.
//    Against what: `--scope` names its crate; `--changed` diffs against its
//    `--base`; a whole-tree run against `origin/main` (else `main`) — exactly
//    what this checkout would add to the shared branch.
// ---------------------------------------------------------------------------

/// What the Trust advisory lane verifies in one run ([`trust_selection`]).
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
/// `shared` is the ref a whole-tree run diffs against; `has_lib` filters a
/// `--scope` crate, which may be bin-only.
#[must_use]
pub fn trust_selection(
    scope: &Scope,
    has_lib: &dyn Fn(&str) -> bool,
    touched: &dyn Fn(&str) -> Result<crate::changed::Touched, String>,
    shared: &str,
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
            let left_out = if t.tests_only.is_empty() {
                String::new()
            } else {
                format!(
                    "; left out, only their tests, benches or examples changed: {}",
                    t.tests_only.join(" ")
                )
            };
            if t.libraries.is_empty() {
                TrustSelection::Nothing(format!("{why}: none{left_out}"))
            } else {
                why.push_str(&left_out);
                TrustSelection::Crates {
                    crates: t.libraries,
                    why,
                }
            }
        }
    };
    match scope {
        Scope::Crate(c) if has_lib(c) => TrustSelection::Crates {
            crates: vec![c.clone()],
            why: format!("--scope {c}"),
        },
        Scope::Crate(c) => TrustSelection::Nothing(format!("--scope {c}: no library")),
        Scope::Changed(ch) => from_diff(&ch.base),
        Scope::Workspace => from_diff(shared),
    }
}

/// THE UP-ONLY FLOOR (2026-09-27, review): every floor `tree` lowers against
/// `base` — each a line naming the library — as two `tools/trust-gate-ratchet.tsv`
/// texts (`<crate>\t<proved>\t<total>\t<prover>[\t<why>]`). A floor goes DOWN
/// in exactly two ways, each saying WHY in the row's fifth column (a reason the
/// base row did not already give). A RE-BASE onto `driver` — the `--version`
/// line of the prover this lane runs: a new prover meets the floors it
/// inherits, or its row says why it does not (2026-09-30, no floor excuses: the
/// lane holds every floor, whoever recorded it, and `ship trust judge
/// --update-ratchet --reason` writes such a row); a re-base that does not lower
/// needs none. A prover column changed to anything but `driver` names nothing
/// the lane measured (review, 2026-09-29), whatever its number, and with no
/// driver line no re-base is admitted; aterm-release's reuse of stored facts
/// holds the same rule (`trust_facts::floors_lowered`). Or a
/// hand edit that says WHY in the row's fifth column AND whose proved count fell
/// by no more than its total — the proved obligations left with the code that
/// carried them (aterm-session's paste-cut arithmetic made checked: 304
/// obligations became 300, one of the four proved). Decided 2026-09-29 under the
/// owner's standing direction: a reason alone let a floor drop by any amount with
/// the obligations still there, which is the lowering the ratchet exists to
/// refuse; and the lane holds such a row's total to the one it measures. A row
/// removed while `gated(name)` still holds — the library is still one the lane
/// verifies — lowers it to nothing. A floor or total that is not a count is named
/// too: a typo must not pass.
#[must_use]
pub fn lowered_floors(
    base: &str,
    tree: &str,
    gated: &dyn Fn(&str) -> bool,
    driver: Option<&str>,
) -> Vec<String> {
    struct Row<'a> {
        name: &'a str,
        proved: &'a str,
        total: &'a str,
        prover: &'a str,
        why: &'a str,
    }
    fn rows(text: &str) -> Vec<Row<'_>> {
        text.lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let mut f = l.split('\t');
                let name = f.next()?;
                let proved = f.next().unwrap_or("");
                let total = f.next().unwrap_or("");
                let prover = f.next().unwrap_or("");
                let why = f.next().unwrap_or("").trim();
                Some(Row {
                    name,
                    proved,
                    total,
                    prover,
                    why,
                })
            })
            .collect()
    }
    let now = rows(tree);
    let mut out = Vec::new();
    for r in &now {
        if r.proved.parse::<u64>().is_err() {
            out.push(format!("{}: floor `{}` is not a count", r.name, r.proved));
        }
        if r.total.parse::<u64>().is_err() {
            out.push(format!("{}: total `{}` is not a count", r.name, r.total));
        }
    }
    for was in rows(base) {
        let Ok(was_n) = was.proved.parse::<u64>() else {
            continue;
        };
        match now.iter().find(|r| r.name == was.name) {
            Some(is) => {
                let Ok(is_n) = is.proved.parse::<u64>() else {
                    continue;
                };
                if is.prover != was.prover {
                    if driver != Some(is.prover) {
                        out.push(format!(
                            "{}: floor {was_n} -> {is_n} re-based onto `{}`, which is not the \
                             prover this lane runs ({})",
                            was.name,
                            is.prover,
                            driver.unwrap_or("its `targo --version` did not answer")
                        ));
                    } else if is_n < was_n && (is.why.is_empty() || is.why == was.why) {
                        out.push(format!(
                            "{}: floor {was_n} -> {is_n} re-based onto the prover this lane \
                             runs with no new reason in the row's fifth column: a new prover \
                             meets the floors it inherits, or its row says why it does not",
                            was.name
                        ));
                    }
                    continue;
                }
                if is_n >= was_n {
                    continue;
                }
                if is.why.is_empty() || is.why == was.why {
                    out.push(format!(
                        "{}: floor {was_n} -> {is_n} under the same prover ({}) and no new \
                         reason in the row's fifth column",
                        was.name, was.prover
                    ));
                    continue;
                }
                // The reason must be true of the row: the proved obligations
                // went with their code, so the total fell at least as far. A
                // total that is not a count shows nothing.
                let gone = match (was.total.parse::<u64>(), is.total.parse::<u64>()) {
                    (Ok(w), Ok(i)) => Some(w.saturating_sub(i)),
                    _ => None,
                };
                if gone.is_none_or(|g| was_n - is_n > g) {
                    out.push(format!(
                        "{}: floor {was_n} -> {is_n} under the same prover ({}), but the \
                         total went {} -> {}: a reason covers only proved obligations that \
                         left with their code",
                        was.name, was.prover, was.total, is.total
                    ));
                }
            }
            None if gated(was.name) => out.push(format!(
                "{}: floor {was_n} removed while the lane still verifies it",
                was.name
            )),
            None => {}
        }
    }
    out
}

/// The lane's exit status and its `GATED:` line, as a ladder row:
/// * `0`: every verified library held its floor.
/// * `1`: a library fell below its floor, had none, has a hand-lowered total
///   that is not the tree's, or its verification broke; the row names each
///   failing library with its status from the lane's table.
///   A verification that did not finish is one that broke: ended partway
///   (NOT-VERIFIED(incomplete), whatever its Summary counted) or past its
///   per-crate wall budget (OVER-BUDGET, which keeps one crate from holding this
///   stage for hours).
/// * `2`: nothing was decided: the lane could not run, or trustd (Trust's memory
///   jobserver) did not admit the work (refused obligations covering a drop, a
///   trustd of another build or one that did not answer held its socket, or its
///   crash epoch was DIRTY). The script's own `COULD NOT RUN — …` words are the
///   row's reason.
/// * `4`: narrowed by memory: nothing failed and trustd refused nothing, but
///   this host's verifier ceiling (`targo trust`: RAM / 8) could not reproduce
///   every selected floor; `MEMORY:` lines name each. A named skip, read from
///   the `VERDICT: NARROWED BY MEMORY — …` line; a `4` without that line and its
///   `GATED:` line is COULD NOT RUN.
///
/// How a lane NARROWED BY MEMORY is named on the ladder, in the verdict's skip
/// list and in the receipt's `skipped` line ([`crate::receipt::skipped_line`]):
/// the lane's own words, so a library this host's verifier ceiling could not
/// verify reads as that wherever the run is read — never as a bare skip, and
/// never as the merge contract (a skip withholds it: [`crate::verdict::claims`]).
pub const TRUST_NARROWED_BY_MEMORY: &str = "trust advisory lane: NARROWED BY MEMORY — ";

/// `4` is NARROWED BY MEMORY (2026-09-29, the owner's direction that day: a
/// machine too small to verify something gets an intelligent warning and a
/// refusal, not a failure): nothing failed and trustd refused nothing, but this
/// host's verifier ceiling (`targo trust`: RAM / 8) could not reproduce every
/// selected library's floor, and the lane's `MEMORY:` lines name each one and the
/// RAM that verifies it. That is a named SKIP — no claim about those libraries,
/// no finding about the tree, and no merge-contract sentence for the run — read
/// from the lane's `VERDICT: NARROWED BY MEMORY — …` line; a `4` without that
/// line and its `GATED:` line decided nothing it can say, COULD NOT RUN.
#[must_use]
pub fn trust_lane_outcome(code: Option<i32>, scope: &str, output: &str) -> (Outcome, String) {
    let gated = output
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("GATED: "))
        .map(str::trim);
    match code {
        Some(0) => match gated {
            Some(g) if g.starts_with("0 of ") => (
                Outcome::Ok,
                format!(
                    "trust advisory lane: NOTHING verified — {g}; the release's whole-tree run \
                     verifies what was skipped ({scope})"
                ),
            ),
            Some(g) => (
                Outcome::Ok,
                format!(
                    "trust advisory lane: every verified library at or above its floor — {g} \
                     ({scope})"
                ),
            ),
            None => (
                Outcome::Fail(Severity::CouldNotRun),
                format!(
                    "trust advisory lane: exit 0 with no GATED line, so what it verified is \
                     unknown ({scope})"
                ),
            ),
        },
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            trust_lane_failed(scope, output),
        ),
        Some(2) => {
            // Its `trust-gate-all: COULD NOT RUN — …` (the lane could not start)
            // or `VERDICT: COULD NOT RUN — …` (trustd did not admit the work):
            // the last one printed.
            let why = output
                .lines()
                .rev()
                .find_map(|l| {
                    let l = l.trim();
                    l.strip_prefix("trust-gate-all: COULD NOT RUN — ")
                        .or_else(|| l.strip_prefix("VERDICT: COULD NOT RUN — "))
                })
                .map_or("the script could not start its lane", str::trim);
            (
                Outcome::Fail(Severity::CouldNotRun),
                format!("trust advisory lane: {why} ({scope})"),
            )
        }
        Some(4) => {
            let narrowed = output
                .lines()
                .rev()
                .find_map(|l| l.trim().strip_prefix("VERDICT: NARROWED BY MEMORY — "))
                .map(str::trim);
            match (narrowed, gated) {
                (Some(why), Some(g)) => (
                    Outcome::Skip,
                    format!("{TRUST_NARROWED_BY_MEMORY}{why} — {}", narrowed_gated(g)),
                ),
                _ => (
                    Outcome::Fail(Severity::CouldNotRun),
                    format!(
                        "trust advisory lane: exit 4 (narrowed by memory) without its NARROWED BY \
                         MEMORY verdict and GATED line, so what it narrowed is unknown ({scope})"
                    ),
                ),
            }
        }
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("trust advisory lane: unexpected exit {c} (the script answers only 0/1/2/4)"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            "trust advisory lane: no exit status — killed by a signal, or never spawned"
                .to_string(),
        ),
    }
}

/// Exit 1's row: each library the lane's table failed, with its status and
/// count (`aterm-net RATCHET-REGRESSION(<49) 44/197`). A row that passed, was
/// left out on purpose, or that nothing was decided about (memory, cap-bound,
/// trustd, lapsed clock) is not a failure. With no such row (a fork went
/// missing, the gate itself broke) the lane's own `VERDICT: FAIL` words.
fn trust_lane_failed(scope: &str, output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let header = lines.iter().rposition(|l| {
        l.split_whitespace().collect::<Vec<_>>() == ["CRATE", "STATUS", "PROVED", "TOTAL"]
    });
    let passed = |s: &str| {
        matches!(
            s,
            "open" | "CLEAN" | "FULL-ONLY" | "EXCLUDED" | "REFUSED(memory)"
        ) || s.starts_with("CAP-BOUND(")
            || s.starts_with("NOT-VERIFIED(trustd")
            || s.starts_with("NOT-VERIFIED(clock")
    };
    let failed: Vec<String> = header
        .map(|h| {
            lines[h + 1..]
                .iter()
                .map(|l| l.trim())
                .take_while(|l| !l.is_empty() && !l.starts_with("TOTAL:"))
                .filter_map(|l| {
                    let f: Vec<&str> = l.split_whitespace().collect();
                    // The last two columns are PROVED/TOTAL; a status may hold
                    // spaces (`AMBIGUOUS-SPEC(<a> vs <b>)`).
                    match f.as_slice() {
                        [name, status @ .., p, t] if !status.is_empty() => {
                            let s = status.join(" ");
                            (!passed(&s)).then(|| {
                                if *p != "-" && *t != "-" {
                                    format!("{name} {s} {p}/{t}")
                                } else {
                                    format!("{name} {s}")
                                }
                            })
                        }
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if !failed.is_empty() {
        return format!("trust advisory lane: FAILED — {}", failed.join(", "));
    }
    match lines
        .iter()
        .rev()
        .find_map(|l| l.trim().strip_prefix("VERDICT: FAIL"))
    {
        Some(why) => format!("trust advisory lane: FAILED {}", why.trim()),
        None => format!(
            "trust advisory lane: FAILED — exit 1 with no failing row or VERDICT line ({scope})"
        ),
    }
}

/// A narrowed row's `GATED:` tail less what its verdict already says: the count
/// verified, and only the libraries skipped for another reason than memory (a
/// `full-only` one in the per-commit tier, say), which the verdict does not name.
fn narrowed_gated(g: &str) -> String {
    let (count, skipped) = g.split_once("; not verified:").unwrap_or((g, ""));
    let others: Vec<&str> = skipped
        .split_inclusive(')')
        .map(str::trim)
        .filter(|e| {
            !e.is_empty() && *e != "none" && !e.ends_with("(memory)") && !e.ends_with("(cap-bound)")
        })
        .collect();
    let count = count.trim();
    if others.is_empty() {
        format!("verified {count}")
    } else {
        format!("verified {count}; also not verified: {}", others.join(" "))
    }
}

/// The lane's command: the script in its per-commit tier (`full-only` libraries
/// named and skipped), its target root, the slot count left to the script's
/// `--jobs auto`, and every `ATERM_GATE_*` input pinned (empty is the script's
/// default) so a caller's export cannot narrow the run, redirect the ratchet or
/// memory record, size the slots, or change its wait for trustd. Ended with
/// `SIGTERM` first ([`Cmd::term_grace`]): the script's trap ends its checks, each
/// in its own process group, which a bare `SIGKILL` would orphan.
#[must_use]
pub fn trust_lane_cmd(ctx: &Ctx, only: &[String]) -> Cmd {
    let root = lane_dir(ctx, Lane::TrustTarget).unwrap_or_else(|| ctx.root.join("target-trust"));
    let cmd = Cmd::new(ctx.tools_dir().join("trust-gate-all.sh"))
        .arg("--target-root")
        .arg(root)
        .arg("--per-commit")
        .env("ATERM_GATE_ONLY", only.join(" "))
        .env("ATERM_GATE_RATCHET", "")
        .env("ATERM_GATE_FORKS", "")
        .env("ATERM_GATE_SCOPE", "")
        .env("ATERM_GATE_MEMORY", "")
        .env("ATERM_GATE_HOST_MEMORY", "")
        .env("ATERM_GATE_LOGDIR", "")
        .env("ATERM_GATE_COSTS", "")
        .env("ATERM_GATE_LOAD_EVERY", "")
        .env("ATERM_GATE_MACHINE", "")
        .env("ATERM_GATE_TRUSTD", "")
        .env("ATERM_GATE_TRUSTD_WAIT", "")
        .env("ATERM_GATE_NO_PATH_CHECK", "");
    match lane_jobs(Lane::TrustTarget, ctx.build_jobs_ceiling()) {
        Some(jobs) => cmd.env("CARGO_BUILD_JOBS", jobs.to_string()),
        None => cmd,
    }
    .demoted()
    .term_grace(exec::TERM_GRACE)
}

/// The lane's scripts, under `tools/`: the verdict self-test, the candidate
/// gate's self-test (D27: `tools/trust-gate-candidate.sh` and the judge's `trust
/// compare`, over the 2026-10-02 runs and stub toolchains), then the lane.
pub const TRUST_LANE_SCRIPTS: [&str; 3] = [
    "test-trust-gate-verdict.sh",
    "test-trust-gate-candidate.sh",
    "trust-gate-all.sh",
];

/// The ratchet, relative to the repository root.
pub const TRUST_RATCHET: &str = "tools/trust-gate-ratchet.tsv";

fn trust_advisory(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("trust advisory lane (no stage2 toolchain)");
        return;
    }
    let [selftest, candidate, gate] = TRUST_LANE_SCRIPTS.map(|name| ctx.tools_dir().join(name));
    for t in [&selftest, &candidate, &gate] {
        if !is_executable_file(t) {
            r.cannot_run(format!(
                "trust advisory lane: {} missing or not executable",
                t.display()
            ));
            return;
        }
    }
    let shared = shared_branch_ref(ctx);
    let base = match &ctx.scope {
        Scope::Changed(ch) => ch.base.clone(),
        _ => shared.clone(),
    };
    // The self-tests stub the prover and the graph, so their verdicts are the
    // lane scripts' (`trust-gate-*`, the candidate gate among them, and
    // themselves) and the real judge's, which they build and drive
    // (aterm-release): minutes of wall that a change leaving them alone would
    // spend re-proving the base's. A diff git cannot read runs them.
    match crate::changed::first_touched(
        &ctx.root,
        &ctx.path_env,
        &base,
        &[
            "tools/trust-gate",
            "tools/test-trust-gate-verdict.sh",
            "tools/test-trust-gate-candidate.sh",
            "tools/fixtures/trust-gate-candidate/",
            "crates/aterm-release/",
        ],
    ) {
        Ok(None) => r.pass(format!(
            "test-trust-gate-verdict.sh, test-trust-gate-candidate.sh: not re-run — no trust-gate script or judge changed since {base}"
        )),
        _ => {
            if !run_labeled(ctx, r, "test-trust-gate-verdict.sh", &Cmd::new(&selftest)) {
                return;
            }
            if !run_labeled(ctx, r, "test-trust-gate-candidate.sh", &Cmd::new(&candidate)) {
                return;
            }
        }
    }
    trust_floors_up_only(ctx, r, &base);
    let has_lib = |c: &str| crate::changed::crate_dir_has_lib(&ctx.root, c);
    let touched = |b: &str| crate::changed::touched_since(&ctx.root, &ctx.tools, &ctx.path_env, b);
    let (only, scope) = match trust_selection(&ctx.scope, &has_lib, &touched, &shared) {
        TrustSelection::Nothing(why) => {
            r.pass(format!("trust advisory lane: nothing to verify — {why}"));
            return;
        }
        TrustSelection::Undecided(why) => {
            r.skip(format!("trust advisory lane: {why}"));
            return;
        }
        TrustSelection::Crates { crates, why } => {
            let scope = format!("{}: {}", why, crates.join(" "));
            (crates, scope)
        }
    };
    let cmd = trust_lane_cmd(ctx, &only);
    let out = exec::run(&cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = trust_lane_outcome(out.code, &scope, &out.output);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record(outcome, label);
}

/// The ratchet's up-only check ([`lowered_floors`]) against `base`'s copy.
/// `base` holding no ratchet is nothing to lower; a base git cannot read is a
/// named skip of this check alone, never a pass of it.
fn trust_floors_up_only(ctx: &Ctx, r: &mut Report, base: &str) {
    let tree = std::fs::read_to_string(ctx.root.join(TRUST_RATCHET)).unwrap_or_default();
    let shown = std::process::Command::new("git")
        .args(["show", &format!("{base}:{TRUST_RATCHET}")])
        .current_dir(&ctx.root)
        .env("PATH", &ctx.path_env)
        .output();
    let base_text = match shown {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        Ok(o) if String::from_utf8_lossy(&o.stderr).contains("does not exist") => String::new(),
        Ok(o) => {
            r.skip(format!(
                "trust ratchet up-only check: git could not read {base}'s {TRUST_RATCHET} ({})",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
            return;
        }
        Err(e) => {
            r.skip(format!(
                "trust ratchet up-only check: git did not run ({e})"
            ));
            return;
        }
    };
    let gated = crate::changed::gated_libraries(&ctx.root, &ctx.tools, &ctx.path_env);
    // An unread member table cannot say a library is gone, so every removed row
    // counts as a library the lane still verifies — the direction that refuses.
    let still = |name: &str| gated.as_ref().is_none_or(|g| g.contains(name));
    let lowered = lowered_floors(&base_text, &tree, &still, lane_driver_row(ctx).as_deref());
    if lowered.is_empty() {
        r.pass(format!(
            "trust ratchet up-only against {base}: no floor lowered under its own prover"
        ));
    } else {
        for l in &lowered {
            r.raw(format!("  LOWERED: {l}"));
        }
        r.record(
            Outcome::Fail(Severity::GateFailed),
            format!(
                "trust ratchet up-only against {base}: {} floor(s) lowered by hand — a floor \
                 goes down only with a new reason in the row's fifth column: re-based onto the \
                 prover this lane runs (`targo --unverified ship trust judge --update-ratchet \
                 --reason '<why>'` writes it from the lane's facts), or under the same prover \
                 with its total down at least as far",
                lowered.len()
            ),
        );
    }
}

/// The `--version` line of the `targo` the lane runs (the prover column
/// `trust-gate-all.sh --update-ratchet` writes, tabs and carriage returns made
/// spaces as the script does), or `None` when it does not answer.
fn lane_driver_row(ctx: &Ctx) -> Option<String> {
    let out = std::process::Command::new(&ctx.tools.targo)
        .arg("--version")
        .current_dir(&ctx.root)
        .env("PATH", &ctx.path_env)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .replace(['\t', '\r'], " ");
    (!line.trim().is_empty()).then_some(line)
}

/// The ref a whole-tree run's change is measured against: the shared branch as
/// this checkout last saw it, `origin/main`, else a local `main`.
fn shared_branch_ref(ctx: &Ctx) -> String {
    let resolves = |r: &str| {
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", r])
            .current_dir(&ctx.root)
            .env("PATH", &ctx.path_env)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if resolves("origin/main") {
        "origin/main".to_string()
    } else {
        "main".to_string()
    }
}

// ---------------------------------------------------------------------------
// 3.6) Startup comparison scheduler (`--full` only): the publishable-startup
//    evidence path must fail closed on malformed samples, mutable harness bytes,
//    uncertain thermal state, identical-artifact controls, and timed-out
//    process descendants.
// ---------------------------------------------------------------------------
fn start_compare(ctx: &Ctx, r: &mut Report) {
    let t = ctx.tools_dir().join("perf-arena/test-start-compare.sh");
    if is_executable_file(&t) {
        run_labeled(ctx, r, "test-start-compare.sh", &Cmd::new(&t));
    } else {
        r.cannot_run(format!(
            "test-start-compare.sh missing or not executable ({})",
            t.display()
        ));
    }
}

// ---------------------------------------------------------------------------
// 4.5a) First-party libc ABI oracle, for this host's native cell. `targo test`
//    compiles the cell's const/layout/type assertions and runs its
//    pointer-valued and C-macro checks, and the emitted-source gate closes
//    link-name aliases, all under the Trust sysroot. Other cells are decided
//    where they are native (`FLEET_HOST_TRIPLES` in xtask's gate.rs); a cell no
//    machine hosts is decided by no gate, as the driver's `### CELLS` report
//    says. No rustup is needed.
//
//    A missing or non-executable driver and exit 3 are COULD-NOT-RUN, never a
//    skip; exit 1 is a finding. No stock-workspace fallback: this separate
//    workspace keeps `[patch.crates-io]` from rewriting the reference libc edge
//    to the shim under test.
// ---------------------------------------------------------------------------
fn libc_oracle(ctx: &Ctx, r: &mut Report) {
    let cmd = match libc_oracle_cmd(ctx) {
        Ok(cmd) => cmd,
        Err(error) => {
            r.cannot_run(format!(
                "cannot resolve the libc-oracle target dir: {error}"
            ));
            return;
        }
    };
    if !is_executable_file(&cmd.program) {
        r.cannot_run(format!(
            "libc-oracle/run.sh missing or not executable ({})",
            cmd.program.display()
        ));
        return;
    }
    let label = "libc-oracle/run.sh (this host's native ABI cell; the other cells are decided where they are native)";
    let out = exec::run(&cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    // The machine before the exit code: run.sh maps a cargo child that died of
    // `No space left on device` (exit 101) onto rc=1, which would read as a
    // finding. This stage builds into `libc-oracle/target`, a lane the disk floor
    // bounds, so it is the likeliest to meet ENOSPC.
    if r.child_could_not_run(&out, label) {
        return;
    }
    r.record_child(&out, libc_oracle_outcome(out.code), label);
}

// ---------------------------------------------------------------------------
// 4.5b) L0 temporal-safety gate: one build of tools/freeze-safety-gate enforces
//    six obligations (temporal proof; main-loop, lock-order, wasm-process,
//    scope-cardinality and lazy-init reentrancy censuses), any one failing the
//    build with a counterexample-backed diagnostic.
// ---------------------------------------------------------------------------
fn freeze_gate(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("L0 temporal-safety gate (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "freeze-safety-gate (6 obligations)",
        &freeze_gate_cmd(ctx),
    );
}

// ---------------------------------------------------------------------------
// 4.7) Driver builds: the binaries the tail drives, compiled in
//    `target-drivers/` before the exclusive section. Every driver stage still
//    runs its own build argv (a fingerprint no-op after these) and drives only
//    what that build left, so no stage takes its binary on a prebuild's word.
//
//    Two rows. `driver builds`, the lane's first row at t0, builds the smokes'
//    `aterm-gui`/`aterm-ctl` (the `--full` sealed rung drives them too). `tail
//    driver builds`, the lane's last row before the barrier, builds what only
//    the rows behind the barrier drive: on macOS the `aterm` binary the live
//    lanes drive ([`live_aterm_build_args`]), the redraw harness, and on macOS
//    the objc drivers. The test run waits for the first, not the second
//    ([`crate::plan::NOT_AWAITED`]).
//
//    None of this can come from the test compile's `target/`: `targo test
//    --workspace` unifies every member's dev-dependency features, while the
//    driven binaries are the per-package variants the smokes and drives exist
//    to exercise.
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

/// The tail driver builds row's children, labelled: the `aterm` binary the live
/// lanes drive (macOS), the redraw harness and the objc drivers (macOS), each
/// driven only behind the first barrier, so the test run need not wait
/// ([`crate::plan::NOT_AWAITED`]; `tail_builds_select_only_what_the_tail_drives`
/// holds every argv to it).
///
/// Under `--no-windows` the redraw harness and the objc drivers are left out:
/// their only stages, `RedrawConformance` and `ObjcDrives`, are
/// [`crate::plan::WINDOWED`] and skip before building ([`TAIL_WINDOWED_BUILDS`]).
/// The live lanes' `aterm` stays: they drive it headless.
#[must_use]
pub fn tail_driver_build_cmds(ctx: &Ctx) -> Vec<(String, Cmd)> {
    let mac = cfg!(target_os = "macos");
    let windows = !ctx.no_windows;
    let mut v = Vec::new();
    if mac {
        v.push((
            "driver prebuild: targo build -p aterm --bin aterm (the live lanes' aterm)".to_string(),
            live_aterm_build_cmd(ctx),
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
// 5c) Control redraw conformance: the one check that can see a control-socket
//    `select` repaint a window. `EventLoop` construction panics off the main
//    thread and libtest runs tests on spawned threads, so aterm-gui's unit tests
//    build their host with `proxy: None` and cannot catch a dropped
//    `EventLoopProxy`. `aterm-redraw-conformance` owns `fn main` and holds a real
//    proxy; it is behind `required-features`, so only this stage builds it.
// ---------------------------------------------------------------------------

/// How the ladder reads one harness exit code (`0` pass / `1` fail / `2` not
/// run, declared in `aterm_gui::control_redraw_conformance`).
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
// 5d) The objc drives: the ported classes and the tab strip, driven on a real
//    AppKit, one row per [`OBJC_DRIVES`] entry. Each checks what no unit test can
//    reach (libtest cannot host AppKit).
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
// 5l) Live aterm lanes: shell lanes that drive a private headless instance of
//    the `aterm` binary the way a person's tab does:
//    `tools/test-foreground-handback.sh` (a real shell's job control, and the
//    modes a killed foreground program leaves armed),
//    `tools/test-codex-live-upgrade.sh` (the harness host moving a real Codex
//    from the managed store's older build to its current one, sandboxed to one
//    loopback port) and `tools/test-render-desync.sh` (a net-zero
//    alternate-screen flap and the three read surfaces that name it).
//
//    Each stage builds the binary itself (`-p aterm --bin aterm` into the driver
//    lane, a fingerprint no-op after the tail driver builds row), runs the lane
//    only if that build succeeded, and hands the lane that binary (`--binary
//    <path>` / the first argument), so none falls back to a stale `target/`. A
//    lane's not-run answer is never a pass ([`live_aterm_outcome`]).
//
//    Two tiers. The handback and render desync lanes are the per-commit
//    ladder's last driver-lane rows (`StageId::ForegroundHandback`, ~20 s;
//    `StageId::RenderDesync`, ~16 s). The Codex lane is `--full`'s last row,
//    run alone (`StageId::CodexLiveUpgrade`, ~10 min): it reads this machine's
//    managed store and the vendor's current Codex, so per commit it decided a
//    different thing on each machine and night (`plan.rs` has the reasoning).
//    No narrowing removes any of them: the claim is about the shipped binary.
// ---------------------------------------------------------------------------

/// `targo --unverified build -q -p aterm --bin aterm`: the one binary the lanes
/// drive, spelled as the Codex lane's header spells its build (plus `-q`).
#[must_use]
pub fn live_aterm_build_args() -> Vec<String> {
    [
        "--unverified",
        "build",
        "-q",
        "-p",
        "aterm",
        "--bin",
        "aterm",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The label of each live stage's first child — the build its lane depends on.
pub const LIVE_ATERM_BUILD_LABEL: &str =
    "targo build -p aterm --bin aterm (the aterm the live lanes drive)";

/// The handback lane. Takes the binary as `--binary <path>`; exits `0` iff no
/// row FAILed, `1` when one did, `2` when the lane could not run.
pub const FOREGROUND_HANDBACK_SUITE: &str = "test-foreground-handback.sh";

/// The Codex branch of the live agent upgrade. Takes the binary as its first
/// argument; exits `0` pass, `1` a check failed, `77` skipped. macOS only.
pub const CODEX_LIVE_UPGRADE_SUITE: &str = "test-codex-live-upgrade.sh";

/// The render desync lane. Takes the binary as `--binary <path>`, as the
/// handback lane does; exits `0` iff no row failed and every variant ran, `1`
/// when a row failed, `3` when the lane could not run (no binary, no python3, or
/// a machine too slow to land two resizes in one run).
pub const RENDER_DESYNC_SUITE: &str = "test-render-desync.sh";

/// Every live lane, the roster the fixtures seed from: the handback lane is
/// `StageId::ForegroundHandback`'s, the render desync lane
/// `StageId::RenderDesync`'s, the Codex lane `StageId::CodexLiveUpgrade`'s
/// (`--full` only).
pub const LIVE_ATERM_SUITES: [&str; 3] = [
    FOREGROUND_HANDBACK_SUITE,
    RENDER_DESYNC_SUITE,
    CODEX_LIVE_UPGRADE_SUITE,
];

/// The code a live lane answers when it could not run: the handback lane's
/// `2`, the render desync lane's `3`, the Codex lane's `77`.
#[must_use]
pub fn live_aterm_not_run_code(name: &str) -> i32 {
    match name {
        FOREGROUND_HANDBACK_SUITE => 2,
        RENDER_DESYNC_SUITE => 3,
        _ => 77,
    }
}

/// The build of that binary, in the driver lane. Compile-only, so demoted; the
/// lanes themselves RUN code and keep the inherited tier.
#[must_use]
pub fn live_aterm_build_cmd(ctx: &Ctx) -> Cmd {
    driver_build_cmd(ctx, live_aterm_build_args())
}

/// The binary the lanes are handed: this lane's own `debug/aterm`, which each
/// live stage's first child writes. Never `<root>/target/debug/aterm`, the
/// lanes' own default.
#[must_use]
pub fn live_aterm_binary(ctx: &Ctx) -> std::path::PathBuf {
    driver_bin(ctx, "aterm")
}

/// One lane's command, with the binary passed the way that lane's header takes
/// it. Anything else a lane needs it makes itself: a private socket, a scratch
/// HOME, and (the Codex lane) `env -i` around the instance it starts.
///
/// With a `SIGTERM` grace ([`Cmd::term_grace`]): each lane tears down in an EXIT
/// trap (its private instance and temp tree; for the Codex lane also the Codex
/// daemons and background terminals that left its process group), which a
/// `SIGKILL` of the group would skip.
#[must_use]
pub fn live_aterm_suite_cmd(ctx: &Ctx, name: &str) -> Cmd {
    let cmd = Cmd::new(ctx.tools_dir().join(name)).term_grace(exec::TERM_GRACE);
    let bin = live_aterm_binary(ctx);
    if name == CODEX_LIVE_UPGRADE_SUITE {
        cmd.arg(bin)
    } else {
        cmd.arg("--binary").arg(bin)
    }
}

/// The reason a lane gave for not running: the text after the last `SKIP: `
/// (the Codex lane's `skip()`) or `NOT RUN: ` (the handback and render desync
/// lanes' `not_run()`) line it printed — each prints exactly one, then exits.
fn not_run_reason(transcript: &str) -> Option<&str> {
    transcript
        .lines()
        .rev()
        .find_map(|l| {
            let l = l.trim();
            l.strip_prefix("SKIP: ")
                .or_else(|| l.strip_prefix("NOT RUN: "))
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// How the ladder reads a live lane's exit. Tested, because a not-run code read
/// as green would pass a lane that drove nothing. The lanes' tiers differ, so
/// their codes do:
///
/// * The handback lane's `2` is COULD NOT RUN (no binary, an unknown argument, a
///   socket path too long, no python3). An instance that exits or never answers
///   is a FAIL row and `1`.
/// * The Codex lane's `77`, once the binary is handed over, means this machine
///   lacks a prerequisite (no older managed Codex, no python3): in `--full` a
///   named skip, counted and printed with the lane's reason.
/// * The render desync lane reads like the handback lane with the gate's COULD
///   NOT RUN code `3`: no binary, no python3, or a machine too slow to land two
///   `ctl resize` calls within one 250 ms run.
///
/// Never a pass either way. Any other code is a finding: a lane that dies
/// mid-way died on the shipped binary or its own script, both the tree's.
#[must_use]
pub fn live_aterm_outcome(name: &str, code: Option<i32>, transcript: &str) -> (Outcome, String) {
    let codex = name == CODEX_LIVE_UPGRADE_SUITE;
    let not_run = live_aterm_not_run_code(name);
    match code {
        Some(0) => (Outcome::Ok, name.to_string()),
        Some(1) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: a check failed against the live aterm — its rows above say which"),
        ),
        Some(c) if c == not_run && !codex => (
            Outcome::Fail(Severity::CouldNotRun),
            format!(
                "{name}: {} (exit {c})",
                not_run_reason(transcript).unwrap_or("the lane could not run and did not say why")
            ),
        ),
        Some(77) if codex => (
            Outcome::Skip,
            format!(
                "{name}: NOT RUN — {} (exit 77: this machine lacks a prerequisite; a named skip, \
                 never a pass)",
                not_run_reason(transcript).unwrap_or("the lane skipped without saying why")
            ),
        ),
        Some(c) => (
            Outcome::Fail(Severity::GateFailed),
            format!("{name}: unexpected exit {c} (the lane answers only 0, 1 and {not_run})"),
        ),
        None => (
            Outcome::Fail(Severity::CouldNotRun),
            format!("{name}: no exit status — killed by a signal, or never spawned"),
        ),
    }
}

/// Why a live lane runs on macOS only, the reason its off-macOS skip names. The
/// Codex lane cannot run elsewhere. The handback lane is unmeasured elsewhere,
/// and one row is macOS's answer: it expects `bracketed_paste=false` from
/// `/bin/bash` 3.2, while bash 5.1+ arms bracketed paste by default.
#[must_use]
pub fn live_aterm_macos_only(name: &str) -> &'static str {
    match name {
        FOREGROUND_HANDBACK_SUITE => {
            "the lane has been measured nowhere else, and its bash row expects macOS's \
             /bin/bash 3.2, which has no bracketed paste, where bash 5.1+ arms it by default"
        }
        RENDER_DESYNC_SUITE => {
            "the lane has been measured nowhere else: its stand-in and verbs are portable, \
             but no run off macOS has been seen, and a skip says so where a pass would claim it"
        }
        _ => "the instance runs under sandbox-exec, and the ps stand-in reads sysctl's kinfo_proc",
    }
}

/// The per-commit live row: the foreground handback.
fn foreground_handback(ctx: &Ctx, r: &mut Report) {
    live_aterm_stage(ctx, r, FOREGROUND_HANDBACK_SUITE);
}

/// The per-commit live row: the render desync.
fn render_desync(ctx: &Ctx, r: &mut Report) {
    live_aterm_stage(ctx, r, RENDER_DESYNC_SUITE);
}

/// The `--full` live row: the Codex live upgrade.
fn codex_live_upgrade(ctx: &Ctx, r: &mut Report) {
    live_aterm_stage(ctx, r, CODEX_LIVE_UPGRADE_SUITE);
}

/// One live lane's stage, in order: the suite exists, the platform, a
/// toolchain, the build of the binary, the binary, the lane.
fn live_aterm_stage(ctx: &Ctx, r: &mut Report, name: &str) {
    let t = ctx.tools_dir().join(name);
    if !is_executable_file(&t) {
        r.cannot_run(format!(
            "{name} missing or not executable ({})",
            t.display()
        ));
        return;
    }
    if !cfg!(target_os = "macos") {
        r.skip(format!(
            "{name} (macOS only: {})",
            live_aterm_macos_only(name)
        ));
        return;
    }
    if !ctx.tools.have_targo() {
        // Counted and named, as the atpkg pack's: with no toolchain nothing can
        // build the binary, and the lane's coverage is absent from the run.
        r.skip(format!(
            "{name} (no targo — nothing can build the aterm it drives)"
        ));
        return;
    }
    if !run_labeled(ctx, r, LIVE_ATERM_BUILD_LABEL, &live_aterm_build_cmd(ctx)) {
        // Not a skip: running the lane would drive whatever binary an earlier
        // run left, and the lane it keeps from running decided nothing
        // ([`NOT_RUN_BEHIND`]).
        r.cannot_run(format!(
            "{name} — the aterm build above failed, so this suite has no fresh aterm to drive{}",
            NOT_RUN_BEHIND
        ));
        return;
    }
    let bin = live_aterm_binary(ctx);
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "{name}: the just-built aterm is missing ({})",
            bin.display()
        ));
        return;
    }
    let out = exec::run(&live_aterm_suite_cmd(ctx, name), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = live_aterm_outcome(name, out.code, &out.output);
    if r.child_could_not_run(&out, &label) {
        return;
    }
    r.record_child(&out, outcome, label);
}

/// The sentence `xtask gate cells` prints when it ran but one or more cells were
/// never compiled. Defined once: `crates/xtask/src/gate.rs` formats it,
/// [`cells_outcome`] matches it.
pub const CELLS_NOT_PROVEN: &str = "gate cells: NOT PROVEN";

/// How the ladder reads `gate cells`: the exit code and the transcript.
///
/// `gate cells` exits 0 for an uninstalled cross std (a property of the machine
/// that no change here can fix) and says so in words, so the exit code alone
/// cannot tell a run where no compiler started from the matrix discharged.
/// [`Outcome::Skip`] is the reading: counted, named, exit code untouched, and
/// the merge-contract sentence withheld by
/// [`crate::verdict::discharges_merge_contract`].
#[must_use]
pub fn cells_outcome(verb: &str, ok: bool, transcript: &str) -> (Outcome, String) {
    if !ok {
        return (Outcome::Fail(Severity::GateFailed), verb.to_string());
    }
    // Anchored at column 0, never `contains`: the verb prints its verdict
    // unindented and everything echoed from a compiler is indented or prefixed,
    // so tree text cannot fake it.
    if transcript
        .lines()
        .any(|line| line.starts_with(CELLS_NOT_PROVEN))
    {
        return (
            Outcome::Skip,
            format!(
                "{verb} (a forge cell had no toolchain here — no installed std, or no rustup; \
                 NOTHING was compiled for it; the NOT PROVEN line above names which and why)"
            ),
        );
    }
    (Outcome::Ok, verb.to_string())
}

/// `--full` only: every forge cell type-checked for its own triple.
///
/// The rest of the gate compiles aterm for one target; aterm ships five. `xtask
/// gate cells` runs a real compiler per triple, on a toolchain that carries that
/// std, from a cwd and target directory outside this repo. A cell whose
/// toolchain is not installed skips and says so (read by [`cells_outcome`]); a
/// cell that runs and fails is a failure. `ring` and `zstd-sys` are shimmed
/// (`tools/cross-cell-gate.tsv`, `cshim` rows), and each cell fails if any
/// in-repo package in its graph goes unread.
fn cross_cells(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("cross-cell type-check (no targo)");
        return;
    }
    let out = exec::run(&targo(ctx, xtask_gate_args("cells")), ctx.exec_env());
    r.raw(out.output.as_str());
    if r.child_could_not_run(&out, "gate cells") {
        return;
    }
    let (outcome, label) = cells_outcome("gate cells", out.ok, out.output.as_str());
    r.record_child(&out, outcome, label);
}

/// Every gate run: the cells no fleet machine hosts (`xtask gate cells-foreign`:
/// win, the two wasm32 cells, linux-arm, win-arm), type-checked for their own
/// triples, so a Windows or wasm break is caught per commit rather than only
/// under `--full`.
///
/// It runs in the xtask lane, which nothing waits for, with that lane's job cap,
/// and builds into the snapshot's cells lane ([`foreign_cells_cmd`]), which the
/// disk preflight counts and caps. The mac and x86_64-linux cells stay with the
/// native lanes of the machines that host them and with `--full`'s matrix.
///
/// Prerequisite: rustup's `stable` with the four foreign std targets. Without
/// them `xtask` exits 0 naming the missing std, this stage records a skip, and
/// the verdict withholds the merge-contract sentence.
/// STOCK EXCEPTION (Trust lacks these stds: E0463 for wasm32, windows-gnu and
/// both linux triples). Whether the merge contract keeps requiring
/// stock-compiled cells is an owner decision, not this stage's.
fn foreign_cells(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("gate cells-foreign (no targo)");
        return;
    }
    let out = exec::run(&foreign_cells_cmd(ctx), ctx.exec_env());
    r.raw(out.output.as_str());
    if r.child_could_not_run(&out, "gate cells-foreign") {
        return;
    }
    let (outcome, label) = cells_outcome("gate cells-foreign", out.ok, out.output.as_str());
    r.record_child(&out, outcome, label);
}

/// `xtask gate cells-foreign`, pointed at the run's cells lane
/// ([`crate::disk::cells_lane`]) through `$ATERM_CELL_TARGET_DIR`, so what it
/// writes is on the snapshot's volume, counted by the disk preflight and removed
/// by its cap, not under `~/.cache/aterm/cells`.
fn foreign_cells_cmd(ctx: &Ctx) -> Cmd {
    let cmd = xtask_cmd(ctx, xtask_gate_args("cells-foreign"));
    match crate::disk::cells_lane(&ctx.root) {
        Some(cells) => cmd.env("ATERM_CELL_TARGET_DIR", cells),
        None => cmd,
    }
}

/// Every gate run: `xtask gate forge` — the third-party surface (vendored
/// forks and first-party patch targets live in the cells that pull them in,
/// never dead and with no unpatched sibling, carved paths absent, provenance
/// attested, the `[OB-14]` budget ratchet, `[OB-17]` the fork ledger).
/// Decided 2026-09-25 under the owner's standing direction: a flat ~9-14 s
/// with no compiler and no network, in the xtask lane beside the
/// other gate verbs —
/// and a gate that only `--full` runs is a gate nothing automatic runs, which
/// is how `gate cells` missed a Windows break for two days.
fn forge_gate(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("gate forge (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "gate forge",
        &xtask_cmd(ctx, xtask_gate_args("forge")),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSnapshot;
    use crate::cli::Mode;
    use std::path::PathBuf;

    fn ctx(scope: Scope) -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            Mode::Fast,
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

    // --- the Trust advisory lane ---------------------------------------------

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
        let has_lib = |c: &str| c != "xtask";
        let answer = |base: &str| {
            *asked.borrow_mut() = Some(base.to_string());
            t.clone().map(|mut t| {
                t.base = base.to_string();
                t
            })
        };
        let got = trust_selection(scope, &has_lib, &answer, "origin/main");
        // A --scope run names its crate and must not pay for git; every other
        // shape reads the diff — against its own --base, or the shared branch.
        let want = match scope {
            Scope::Crate(_) => None,
            Scope::Changed(ch) => Some(ch.base.clone()),
            Scope::Workspace => Some("origin/main".to_string()),
        };
        assert_eq!(*asked.borrow(), want, "{scope:?}");
        got
    }

    /// THE SELECTION, in EVERY mode — the whole tree is the release's
    /// (`trust_receipt_gate`), never a stage child's. A scope names its own crate,
    /// libraries only; `--changed` and a whole-tree run verify the libraries
    /// (and admitted forks) the change touched — the SEEDS, never the reverse
    /// cone a test run needs (a dependent's moved rows are the release's).
    #[test]
    fn the_trust_lane_verifies_what_the_run_says_it_touched() {
        assert_eq!(
            select(&Scope::crate_only("aterm-grid"), Err("unasked".into())),
            TrustSelection::Crates {
                crates: vec!["aterm-grid".into()],
                why: "--scope aterm-grid".into()
            }
        );
        assert!(matches!(
            select(&Scope::crate_only("xtask"), Err("unasked".into())),
            TrustSelection::Nothing(why) if why.contains("no library")
        ));
        // --changed: what touched_since answers against ITS base — the seeds and
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

    /// The script's contract, row by row: only `0` passes; `2` is the lane not
    /// running, never a finding; and a pass says what it verified.
    #[test]
    fn the_trust_lane_outcome_reads_the_scripts_answers_and_what_it_gated() {
        let some = "GATED: 1 of 2 selected; not verified: aterm-grid (full-only)\n";
        let none = "x\nGATED: 0 of 2 selected; not verified: aterm-grid (full-only) aterm-core (full-only)\n";
        let (ok, label) = trust_lane_outcome(Some(0), "s", some);
        assert!(matches!(ok, Outcome::Ok));
        assert!(
            label.contains("at or above its floor") && label.contains("aterm-grid (full-only)")
        );
        let (ok, label) = trust_lane_outcome(Some(0), "s", none);
        assert!(matches!(ok, Outcome::Ok));
        assert!(label.contains("NOTHING verified"), "{label}");
        assert!(!label.contains("at or above its floor"), "{label}");
        // Exit 0 with no GATED line decides nothing about the tree.
        assert!(matches!(
            trust_lane_outcome(Some(0), "s", "TOTAL: 3/3 proved\n").0,
            Outcome::Fail(Severity::CouldNotRun)
        ));
        assert!(matches!(
            trust_lane_outcome(Some(1), "s", some).0,
            Outcome::Fail(Severity::GateFailed)
        ));
        assert!(matches!(
            trust_lane_outcome(Some(2), "s", "").0,
            Outcome::Fail(Severity::CouldNotRun)
        ));
        assert!(matches!(
            trust_lane_outcome(Some(7), "s", some).0,
            Outcome::Fail(Severity::GateFailed)
        ));
        assert!(matches!(
            trust_lane_outcome(None, "s", "").0,
            Outcome::Fail(Severity::CouldNotRun)
        ));
        // Exit 1 names each failing library and its status from the lane's
        // table, never the libraries that held or were skipped; with no failing
        // row it says the lane's own VERDICT words.
        let failed = "\nCRATE                        STATUS                       PROVED    TOTAL\n\
aterm-grid                   CLEAN                             9        9\n\
aterm-net                    RATCHET-REGRESSION(<49)          44      197\n\
aterm-search                 TOTAL-ABOVE-ROW(>464)           178      475\n\
aterm-ctl                    REFUSED(memory)                   -        -\n\
aterm-wasm                   NOT-VERIFIED(incomplete)          -        -\n\
foo                          AMBIGUOUS-SPEC(path+file:///a#foo@1.0 vs path+file:///b#foo@1.0)  -  -\n\
\nTOTAL: 231/681 proved\nVERDICT: FAIL (gate broke, or the ratchet regressed above)\n";
        let (outcome, label) = trust_lane_outcome(Some(1), "the scope", failed);
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed));
        assert_eq!(
            label,
            "trust advisory lane: FAILED — aterm-net RATCHET-REGRESSION(<49) 44/197, \
             aterm-search TOTAL-ABOVE-ROW(>464) 178/475, aterm-wasm NOT-VERIFIED(incomplete), \
             foo AMBIGUOUS-SPEC(path+file:///a#foo@1.0 vs path+file:///b#foo@1.0)"
        );
        assert_eq!(
            trust_lane_outcome(Some(1), "s", "VERDICT: FAIL (a fork went missing)\n").1,
            "trust advisory lane: FAILED (a fork went missing)"
        );
        assert!(
            trust_lane_outcome(Some(1), "the scope", "")
                .1
                .contains("the scope")
        );
        // Exit 1's row names the libraries the lane's verdict names.
        let (outcome, label) = trust_lane_outcome(
            Some(1),
            "s",
            "x\nVERDICT: FAIL — aterm-net RATCHET-REGRESSION(<49) 44/197; aterm-search TOTAL-ABOVE-ROW(>464) 178/475\n",
        );
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed));
        assert_eq!(
            label,
            "trust advisory lane: FAILED — aterm-net RATCHET-REGRESSION(<49) 44/197; aterm-search \
             TOTAL-ABOVE-ROW(>464) 178/475"
        );
        // Exit 2's reason is the lane's own COULD NOT RUN line
        // (`the_lanes_could_not_run_words_are_the_rows_reason`). Exit 4 is a named
        // skip carrying the lane's reason and what it gated; without its verdict
        // line it decided nothing.
        let (outcome, label) = trust_lane_outcome(Some(4), "s", NARROWED_LANE);
        assert_eq!(outcome, Outcome::Skip);
        assert!(
            label.starts_with(
                "trust advisory lane: NARROWED BY MEMORY — not verified on this host ("
            ),
            "{label}"
        );
        // The verdict names the libraries and the RAM once; the row adds only the
        // count verified, never the GATED list again.
        assert!(
            label.contains("not verified on this host (8 GiB RAM, verifier ceiling 1.0 GiB)")
                && label.ends_with("verifies them all — verified 1 of 3 selected"),
            "{label}"
        );
        assert_eq!(label.matches("aterm-ctl").count(), 1, "{label}");
        // A library skipped for another reason (the per-commit tier's full-only)
        // is not in the verdict, so the row still names it.
        let mixed = NARROWED_LANE.replace(
            "GATED: 1 of 3 selected; not verified: aterm-ctl (memory) aterm-verify (cap-bound)",
            "GATED: 1 of 4 selected; not verified: aterm-ctl (memory) aterm-verify (cap-bound) \
             aterm-gui (full-only)",
        );
        let (_, label) = trust_lane_outcome(Some(4), "s", &mixed);
        assert!(
            label.ends_with("— verified 1 of 4 selected; also not verified: aterm-gui (full-only)"),
            "{label}"
        );
        for partial in [
            "",
            "GATED: 1 of 3 selected; not verified: aterm-ctl (memory)\n",
            "VERDICT: NARROWED BY MEMORY — not verified on this host: aterm-ctl\n",
        ] {
            let (outcome, label) = trust_lane_outcome(Some(4), "s", partial);
            assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun), "{partial:?}");
            assert!(label.contains("exit 4"), "{label}");
        }
        assert!(
            trust_lane_outcome(Some(3), "s", some)
                .1
                .contains("answers only 0/1/2/4")
        );
    }

    /// What the lane prints when an 8 GiB host could not verify two of the three
    /// libraries it was asked about (tools/test-trust-gate-verdict.sh drives the
    /// script into this shape).
    const NARROWED_LANE: &str = "\
MEMORY: aterm-ctl not verified here: needs ~2.4 GiB, this host gives 1.0 GiB (8 GiB RAM / 8); 20 GiB of RAM verifies it
MEMORY: aterm-verify cap-bound: ran out at its 1.0 GiB ceiling (8 GiB RAM / 8): 122 proved vs floor 570, set at 16.0 GiB; floor kept, refused up front next run; 128 GiB of RAM verifies it
GATED: 1 of 3 selected; not verified: aterm-ctl (memory) aterm-verify (cap-bound)
VERDICT: NARROWED BY MEMORY — not verified on this host (8 GiB RAM, verifier ceiling 1.0 GiB): aterm-ctl aterm-verify; nothing that fit failed; 128 GiB of RAM verifies them all
";

    /// A lane narrowed by memory is a skip in the verdict: the run passes, names
    /// the skip, and is refused the merge-contract sentence.
    #[test]
    fn a_lane_narrowed_by_memory_is_a_named_skip_and_no_merge_contract() {
        let (outcome, label) = trust_lane_outcome(Some(4), "the libraries changed", NARROWED_LANE);
        let mut r = Report::new("trust advisory lane");
        r.record(outcome, label.clone());
        let t = crate::ladder::tally(&[r]);
        assert_eq!(t.skips, vec![label.clone()]);
        let v = crate::verdict::verdict(Mode::Fast, &Scope::workspace(), &t);
        assert_eq!(v.exit, crate::exit::PASS, "{}", v.text);
        assert!(!v.claims_merge_contract, "{}", v.text);
        assert!(
            v.text
                .contains("VERIFY: PASS (mode=fast scope=workspace, 1 skipped) —"),
            "{}",
            v.text
        );
        assert!(v.text.contains("NOT the merge contract"), "{}", v.text);
        assert!(
            v.text
                .contains("      - 1 skipped, so nothing is claimed about them:"),
            "{}",
            v.text
        );
        assert!(v.text.contains(&format!("      - {label}")), "{}", v.text);
        assert!(
            !v.text.contains(crate::verdict::MERGE_CONTRACT_SENTENCE),
            "{}",
            v.text
        );
        // ITS RECEIPT NAMES IT as a narrowing by memory, whatever skipped before it
        // (2026-10-01: the receipt named four skips and counted the rest, so a
        // fifth was "and 1 more"), and files no merge contract.
        let mut others = Report::new("elsewhere");
        for n in 0..4 {
            others.skip(format!("stage {n} (no tool)"));
        }
        let mut lane = Report::new("trust advisory lane");
        lane.record(Outcome::Skip, label.clone());
        let t = crate::ladder::tally(&[others, lane]);
        let line = crate::receipt::skipped_line(&t.skips);
        assert!(line.ends_with(&format!(", {label}")), "{line}");
        assert!(
            line.contains("trust advisory lane: NARROWED BY MEMORY — not verified on this host"),
            "{line}"
        );
        assert!(
            !crate::verdict::verdict(Mode::Fast, &Scope::workspace(), &t).claims_merge_contract
        );
        assert_eq!(crate::receipt::skipped_line(&[]), "none");
    }

    /// THE UP-ONLY FLOOR (review, 2026-09-27: a hand-lowered floor touched no
    /// library, so the stage passed "nothing to verify"). Lowered under the same
    /// prover with no new reason: named. With a reason in the fifth column and a
    /// total that fell at least as far, or re-based onto the prover the lane
    /// runs: allowed; a reason over a drop the total does not cover, or a
    /// re-base onto any other prover column (review, 2026-09-29) — the floor
    /// lowered or left where it was: named. Raised, unchanged, or a new row:
    /// allowed. A removed row: named while the library
    /// is still gated, allowed once it is gone. A floor or total that is not a
    /// count: named. (That an admitted reason's total is the tree's is the
    /// lane's to check: tools/test-trust-gate-verdict.sh, TOTAL-ABOVE-ROW.)
    #[test]
    fn a_floor_goes_down_only_onto_a_new_prover() {
        let a = "targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)";
        let b = "targo 1.99.0-dev (bbbbbbbbb 2026-09-24) (targo 0.1.0)";
        let base = format!(
            "aterm-grid\t987\t3806\t{a}\naterm-core\t936\t3265\t{a}\nold-crate\t5\t9\t{a}\n"
        );
        let all = |_: &str| true;
        // The lane runs prover `b`.
        let low = |base: &str, tree: &str, gated: &dyn Fn(&str) -> bool| {
            lowered_floors(base, tree, gated, Some(b))
        };
        assert!(low(&base, &base, &all).is_empty(), "unchanged");
        let raised = base.replace("987\t3806", "990\t3806");
        assert!(low(&base, &raised, &all).is_empty(), "raised");
        let hand = base.replace("987\t3806", "900\t3806");
        let got = low(&base, &hand, &all);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("aterm-grid: floor 987 -> 900 under the same prover"));
        // …unless the row says why in its fifth column and the total fell at
        // least as far as the floor. The same reason again is not a new one.
        let said = base.replace(
            &format!("987\t3806\t{a}\n"),
            &format!("985\t3800\t{a}\tthe cut's arithmetic is checked now\n"),
        );
        assert!(
            low(&base, &said, &all).is_empty(),
            "a named reason, the obligations gone with their code"
        );
        let again = said.replace("985\t3800", "984\t3799");
        assert_eq!(
            low(&said, &again, &all).len(),
            1,
            "an old reason is not a new one"
        );
        // A reason does not cover proved obligations that are still there (87
        // fewer proved of only 6 fewer in all), nor a drop against a total that
        // is not a count.
        let excuse = base.replace(
            &format!("987\t3806\t{a}\n"),
            &format!("900\t3800\t{a}\tthe cut's arithmetic is checked now\n"),
        );
        let got = low(&base, &excuse, &all);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("aterm-grid: floor 987 -> 900 under the same prover")
                && got[0].contains("total went 3806 -> 3800"),
            "{got:?}"
        );
        let untotalled = excuse.replace("900\t3800", "900\t38x0");
        assert!(
            low(&base, &untotalled, &all)
                .iter()
                .any(|l| l == "aterm-grid: total `38x0` is not a count")
        );
        // A re-base onto the lane's prover that LOWERS says why (2026-09-30: a new
        // prover meets the floors it inherits, or its row says why it does not).
        let rebased = base.replace(&format!("987\t3806\t{a}"), &format!("900\t3806\t{b}"));
        assert_eq!(
            low(&base, &rebased, &all),
            [
                "aterm-grid: floor 987 -> 900 re-based onto the prover this lane runs with no new \
              reason in the row's fifth column: a new prover meets the floors it inherits, or \
              its row says why it does not"
            ]
        );
        let said = base.replace(
            &format!("987\t3806\t{a}"),
            &format!("900\t3806\t{b}\tthe new seal proves 87 fewer (trust 5ec1)"),
        );
        assert!(low(&base, &said, &all).is_empty(), "a reasoned re-base");
        let up = base.replace(&format!("987\t3806\t{a}"), &format!("990\t3806\t{b}"));
        assert!(
            low(&base, &up, &all).is_empty(),
            "a re-base that does not lower"
        );
        let removed = base.replace(&format!("old-crate\t5\t9\t{a}\n"), "");
        assert_eq!(
            low(&base, &removed, &all),
            ["old-crate: floor 5 removed while the lane still verifies it"]
        );
        assert!(
            low(&base, &removed, &|n| n != "old-crate").is_empty(),
            "gone"
        );
        let typo = base.replace("936\t3265", "93x\t3265");
        assert!(
            low(&base, &typo, &all)
                .iter()
                .any(|l| l == "aterm-core: floor `93x` is not a count")
        );
        // A re-base is onto the prover this lane runs, never onto any other
        // prover column — "something else" with a floor of 0 was admitted.
        let elsewhere = base.replace(&format!("987\t3806\t{a}"), "0\t3806\tsomething else");
        let got = low(&base, &elsewhere, &all);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("aterm-grid: floor 987 -> 0 re-based onto `something else`")
                && got[0].contains(b),
            "{got:?}"
        );
        assert_eq!(
            lowered_floors(&base, &rebased, &all, None).len(),
            1,
            "no driver line, no re-base"
        );
        // A prover column rewritten with the floor where it was: the lane holds no
        // other prover's floor, so this too lowered it — to nothing (review,
        // 2026-09-29). Rewritten onto the lane's prover, it is a re-base.
        let relabelled = base.replace(&format!("987\t3806\t{a}"), "987\t3806\tsomething else");
        let got = low(&base, &relabelled, &all);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("aterm-grid: floor 987 -> 987 re-based onto `something else`"),
            "{got:?}"
        );
        let onto_lane = base.replace(&format!("987\t3806\t{a}"), &format!("987\t3806\t{b}"));
        assert!(
            low(&base, &onto_lane, &all).is_empty(),
            "re-labelled onto the lane's prover"
        );
        // No base ratchet: nothing to lower.
        assert!(low("", &base, &all).is_empty());
    }

    /// A lane that could not run names the script's own reason: a writable
    /// target root is a COULD NOT RUN row naming the component and the fix,
    /// never a crate's NOT-VERIFIED; an exit 2 with no reason line keeps the
    /// generic one.
    #[test]
    fn a_lane_that_could_not_run_names_the_scripts_reason() {
        let refusal = "trust-gate-all: COULD NOT RUN — the target root /tmp/co/target-trust has a \
                       path component, /private/tmp, that `targo trust` refuses for every verified \
                       runtime-library directory under it (mode 1777: group- or world-writable); \
                       nothing was verified (fix: a checkout outside /private/tmp, or --target-root \
                       <dir> whose every component, and every directory holding a symlink on its \
                       path, is owned by root or you and writable by its owner alone)\n";
        let (outcome, label) = trust_lane_outcome(Some(2), "the scope", refusal);
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            label.starts_with("trust advisory lane: the target root"),
            "{label}"
        );
        assert!(label.contains("component, /private/tmp,"), "{label}");
        assert!(label.contains("--target-root <dir>"), "{label}");
        assert!(label.ends_with("(the scope)"), "{label}");
        let (_, bare) = trust_lane_outcome(Some(2), "s", "some other noise\n");
        assert!(
            bare.contains("the script could not start its lane"),
            "{bare}"
        );
    }

    /// THE ROW IS THE LANE'S OWN WORDS (review, 2026-09-29). The real
    /// `tools/trust-gate-all.sh`, over a one-library graph a stub `targo` serves,
    /// answers another build's trustd on the socket before any check starts with
    /// exit 2, and its output becomes a NOT RUN row naming the daemon: a change to
    /// the script's words that the row would lose fails here. The stub `targo`
    /// builds a stub judge that only does the plumbing this refusal passes
    /// through (no prover identity, so no facts; nothing refused for memory). A
    /// check refused beside a DIRTY crash epoch ends in the JUDGE's `VERDICT:
    /// COULD NOT RUN — …` line, which needs the real judge: its words are held in
    /// `aterm-release` (`gates.rs`, the trust receipt gate) and end to end, with
    /// the real judge, by `tools/test-trust-gate-verdict.sh`, as is a regression
    /// beside this build's trustd.
    #[cfg(unix)]
    #[test]
    fn the_lanes_could_not_run_words_are_the_rows_reason() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/trust-gate-all.sh");
        // A unix socket's path must fit in 104 bytes.
        let base = std::env::temp_dir().join(format!("atv-lane-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let write_exe = |p: &Path, body: &str| {
            std::fs::write(p, body).unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write_exe(
            &bin.join("targo"),
            "#!/bin/bash\n\
             case \"$1\" in\n\
             --version) echo 'targo 1.99.0-dev (aaaaaaaaa 2026-09-17) (targo 0.1.0)'; exit 0 ;;\n\
             metadata) cat \"$(dirname \"$0\")/../meta.json\"; exit 0 ;;\n\
             --unverified) j=\"$CARGO_TARGET_DIR/gate/aterm-release\"; mkdir -p \"${j%/*}\"; \
             printf '#!/bin/sh\\ncase \"$2\" in prover) exit 1 ;; admit) exit 0 ;; esac\\nexit 3\\n' >\"$j\"; \
             chmod +x \"$j\"; exit 0 ;;\n\
             esac\n\
             echo \"stub targo: the preflight refuses the run before any check: $*\" >&2\n\
             exit 2\n",
        );
        write_exe(&bin.join("trustd"), "#!/bin/sh\nexit 0\n");
        // `python3` is Apple's /usr/bin/python3 as a Mac puts it ahead of the
        // machine prerequisite's 3.11+ on PATH: no `tomllib`, which the gate
        // list's profile reader needs (main was COULD NOT RUN on such a host,
        // 2026-10-01). A script it runs has `tomllib` blocked, so the reader must
        // find a newer interpreter itself; `python3 -c` passes through.
        let py = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join("python3"))
            .find(|p| p.is_file())
            .expect("a python3 on PATH");
        write_exe(
            &bin.join("python3"),
            &format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                 *.py) exec '{py}' -c 'import runpy, sys; sys.modules[\"tomllib\"] = None; \
                 del sys.argv[0]; runpy.run_path(sys.argv[0], run_name=\"__main__\")' \"$@\" ;;\n\
                 esac\n\
                 exec '{py}' \"$@\"\n",
                py = py.display()
            ),
        );
        std::fs::write(
            base.join("meta.json"),
            format!(
                r#"{{"workspace_root":"{b}","workspace_members":["alpha-id"],"packages":[{{"id":"alpha-id","name":"alpha","version":"1.0.0","source":null,"manifest_path":"{b}/crates/alpha/Cargo.toml"}}],"resolve":{{"nodes":[]}}}}"#,
                b = base.display()
            ),
        )
        .unwrap();
        // The gate list reads the root manifest for the lane's profile.
        std::fs::write(base.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(base.join("ratchet.tsv"), "alpha\t3\t3\n").unwrap();
        std::fs::write(base.join("forks.tsv"), "# name\tstatus\tnote\n").unwrap();
        std::fs::write(base.join("scope.tsv"), "# name\tscope\tpeak\treason\n").unwrap();
        std::fs::write(
            base.join("memory.tsv"),
            "# name\tfloor ceiling\tneed\tbound\tmeasured\n",
        )
        .unwrap();
        // Another build's trustd: it answers IDENTITY with a SHA-256 that is not
        // the stub driver's trustd's, and STATUS with no grant.
        let foreign = base.join("f.sock");
        let listener = std::os::unix::net::UnixListener::bind(&foreign).unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut out = conn.try_clone().unwrap();
                for line in BufReader::new(conn).lines().map_while(Result::ok) {
                    let reply = match line.as_str() {
                        "IDENTITY" => format!(
                            r#"{{"release":"1.99.0-dev","commit":"bbbbbbbbbbbb","executable_sha256":"{}"}}"#,
                            "f".repeat(64)
                        ),
                        "STATUS" => r#"{"active":[]}"#.to_string(),
                        _ => "ERR unknown-verb".to_string(),
                    };
                    if writeln!(out, "{reply}").is_err() {
                        break;
                    }
                }
            }
        });
        let run = |sock: &Path| {
            let o = std::process::Command::new("bash")
                .arg(&script)
                .arg("--target-root")
                .arg(base.join("root"))
                .arg("--jobs")
                .arg("1")
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        bin.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .env("ATERM_GATE_ONLY", "")
                .env("ATERM_GATE_RATCHET", base.join("ratchet.tsv"))
                .env("ATERM_GATE_FORKS", base.join("forks.tsv"))
                .env("ATERM_GATE_SCOPE", base.join("scope.tsv"))
                .env("ATERM_GATE_MEMORY", base.join("memory.tsv"))
                .env("ATERM_GATE_HOST_MEMORY", base.join("host-memory.tsv"))
                .env("ATERM_GATE_LOGDIR", base.join("logs"))
                .env("ATERM_GATE_COSTS", base.join("costs.tsv"))
                .env("ATERM_GATE_MACHINE", "")
                .env("ATERM_GATE_TRUSTD", sock)
                .env("ATERM_GATE_TRUSTD_WAIT", "0")
                .env("ATERM_GATE_NO_PATH_CHECK", "1")
                // The gate hands this test its own; the lane would drive it.
                .env_remove("TRUST_STAGE2_BIN")
                .stdin(std::process::Stdio::null())
                .output()
                .expect("run trust-gate-all.sh");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            (o.status.code(), text)
        };
        let cases = [(
            run(&foreign),
            vec![
                "a trustd of another build serves",
                "trustd 1.99.0-dev bbbbbbbbb, sha256 ffffffffffff",
                "re-run once it retires",
            ],
        )];
        let _ = std::fs::remove_dir_all(&base);
        for ((code, text), named) in cases {
            assert_eq!(code, Some(2), "COULD NOT RUN is exit 2: {text}");
            let (outcome, label) = trust_lane_outcome(code, "the scope", &text);
            assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun), "{label}");
            assert!(label.starts_with("trust advisory lane: "), "{label}");
            for n in named {
                assert!(label.contains(n), "the row names `{n}`: {label}\n{text}");
            }
        }
    }

    /// THE PATH CHECK, run in the real `tools/trust-gate-all.sh` — the one place
    /// both entry points pass through (this stage runs that script). A target
    /// root under a world-writable directory (1777: the sticky bit exempts
    /// nothing, as `targo trust` exempts nothing) or a group-writable one stops
    /// the script with exit 2 before anything is gated, naming the DEEPEST such
    /// directory; so does a root reached through a symlink that sits in a
    /// world-writable directory, although the physical path never passes
    /// through it (targo holds the directory holding each symlink to the same
    /// rule). An owner-only chain is blamed on no directory this test made, and
    /// on nothing at all when every ancestor of the test's directory is owned by
    /// root or the caller and writable by its owner alone. The check runs before
    /// the script looks for `targo`, so no driver is needed; modes are restored
    /// before the directory is removed.
    #[cfg(unix)]
    #[test]
    fn the_lane_script_refuses_a_writable_target_root_by_name() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::path::Path;
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/trust-gate-all.sh");
        let base = std::env::temp_dir().join(format!("atv-trust-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (a, b, real) = (base.join("a"), base.join("a/b"), base.join("real"));
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, a.join("link")).unwrap();
        let set = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        };
        for d in [&base, &a, &b, &real] {
            set(d, 0o700);
        }
        let phys = |p: &Path| std::fs::canonicalize(p).unwrap().display().to_string();
        let (base_p, a_p, b_p) = (phys(&base), phys(&a), phys(&b));
        let run_at = |root: &Path| {
            let o = std::process::Command::new("bash")
                .arg(&script)
                .arg("--target-root")
                .arg(root.join("target-trust"))
                .env("PATH", "/usr/bin:/bin")
                // No driver: the gate hands this test its own, which the lane would drive.
                .env_remove("TRUST_STAGE2_BIN")
                .env("ATERM_GATE_NO_PATH_CHECK", "")
                .env("ATERM_GATE_LOGDIR", base.join("logs"))
                .output()
                .expect("run trust-gate-all.sh");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            (o.status.code(), text)
        };
        let run = || run_at(&b);
        let clean = run();
        let via_link_clean = run_at(&a.join("link"));
        set(&a, 0o1777);
        let world = run();
        let via_link = run_at(&a.join("link"));
        set(&a, 0o700);
        set(&b, 0o770);
        let group = run();
        set(&a, 0o777);
        let deepest = run();
        for d in [&base, &a, &b, &real] {
            set(d, 0o700);
        }
        // Would targo refuse an ancestor of the test's directory on its own?
        // Owned by neither root nor the caller, or group/world-writable.
        let me = std::fs::metadata(&base).unwrap().uid();
        let ancestors_clean = std::fs::canonicalize(&base)
            .unwrap()
            .ancestors()
            .skip(1)
            .all(|p| {
                std::fs::metadata(p)
                    .map(|m| (m.uid() == 0 || m.uid() == me) && m.mode() & 0o022 == 0)
                    .unwrap_or(false)
            });
        let _ = std::fs::remove_dir_all(&base);

        let names = |t: &str, p: &str| t.contains(&format!("path component, {p},"));
        for (what, (_, text)) in [("direct", &clean), ("via a symlink", &via_link_clean)] {
            assert!(
                !text.contains(&format!("path component, {base_p}")),
                "{what}: an owner-only chain is blamed on nothing this test made: {text}"
            );
            if ancestors_clean {
                assert!(
                    !text.contains("COULD NOT RUN — the target root"),
                    "{what}: an owner-only chain throughout is refused nowhere: {text}"
                );
            }
        }
        for (what, (code, text), dir) in [
            ("1777 (the sticky bit exempts nothing)", &world, &a_p),
            ("a symlink held in a 1777 directory", &via_link, &a_p),
            ("group-writable", &group, &b_p),
            ("the deepest of two", &deepest, &b_p),
        ] {
            assert_eq!(*code, Some(2), "{what}: COULD NOT RUN is exit 2: {text}");
            assert!(text.contains("COULD NOT RUN"), "{what}: {text}");
            assert!(names(text, dir), "{what}: names {dir}: {text}");
            assert!(
                text.contains("--target-root <dir>"),
                "{what}: names the fix: {text}"
            );
            assert!(!text.contains("NOT-VERIFIED"), "{what}: {text}");
        }
    }

    /// The lane's command pins every `ATERM_GATE_*` input (scope, ratchet path,
    /// machine size, trustd socket and wait), always runs the per-commit tier,
    /// runs in its own lane, demoted, and is ended with `SIGTERM` first.
    #[test]
    fn the_trust_lane_command_pins_its_inputs_and_its_lane() {
        let env = |cmd: &Cmd, k: &str| {
            cmd.envs
                .iter()
                .rev()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        for mode in [Mode::Fast, Mode::Full] {
            let mut c = ctx(Scope::workspace());
            c.mode = mode;
            let cmd = trust_lane_cmd(&c, &["aterm-bits".into(), "aterm-grid".into()]);
            assert_eq!(cmd.program, PathBuf::from("/repo/tools/trust-gate-all.sh"));
            assert_eq!(
                cmd.args,
                ["--target-root", "/repo/target-trust", "--per-commit"]
                    .map(std::ffi::OsString::from),
                "{mode:?}: the per-commit tier in every mode; the slots are the script's"
            );
            assert_eq!(
                env(&cmd, "ATERM_GATE_ONLY").as_deref(),
                Some("aterm-bits aterm-grid")
            );
            // Every other input the script reads, as the script names it: a knob
            // added to the lane cannot reach the stage unpinned.
            let lane = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tools/trust-gate-all.sh"),
            )
            .unwrap();
            let mut read: Vec<&str> = lane
                .match_indices("ATERM_GATE_")
                .map(|(at, _)| {
                    let rest = &lane[at..];
                    let end = rest
                        .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                        .unwrap_or(rest.len());
                    &rest[..end]
                })
                .filter(|k| *k != "ATERM_GATE_ONLY")
                .collect();
            read.sort_unstable();
            read.dedup();
            for known in ["ATERM_GATE_TRUSTD_WAIT", "ATERM_GATE_HOST_MEMORY"] {
                assert!(read.contains(&known), "{known}: {read:?}");
            }
            for k in read {
                assert_eq!(env(&cmd, k).as_deref(), Some(""), "{k} is pinned empty");
            }
            assert_eq!(env(&cmd, "CARGO_BUILD_JOBS").as_deref(), Some("4"));
            assert!(cmd.demoted);
            assert_eq!(
                cmd.term_grace,
                Some(exec::TERM_GRACE),
                "SIGTERM first: the script's trap ends its checks, whose groups are their own"
            );
        }
    }

    /// A snapshot's foreign cells build in its cells lane: the stage's child
    /// carries `$ATERM_CELL_TARGET_DIR` naming the lane beside the snapshot,
    /// never `xtask`'s default under the caller's cache.
    #[test]
    fn a_snapshots_foreign_cells_build_in_its_cells_lane() {
        let target_dir = |c: &Cmd| {
            c.envs
                .iter()
                .find(|(k, _)| k == "ATERM_CELL_TARGET_DIR")
                .map(|(_, v)| PathBuf::from(v))
        };
        let snap = Ctx::new(
            PathBuf::from("/w/aterm-verify.noindex"),
            Mode::Fast,
            Scope::workspace(),
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .in_snapshot_of(
            PathBuf::from("/w/aterm"),
            crate::identity::TreeState {
                head: "0".repeat(40),
                dirty: std::collections::BTreeMap::new(),
            },
            Vec::new(),
        );
        assert_eq!(
            target_dir(&foreign_cells_cmd(&snap)),
            Some(PathBuf::from("/w/aterm-verify-cells.noindex"))
        );
        assert_eq!(
            crate::disk::cells_lane(&snap.root),
            target_dir(&foreign_cells_cmd(&snap)),
            "the stage builds where the preflight measures"
        );
    }

    /// The measuring stage judges the bundle the run was HANDED (2026-10-09,
    /// build once): `ATERM_PAINT_BIN` and `ATERM_SPIN_BIN` are its
    /// `Contents/MacOS/aterm`, and no child of the stage builds a release
    /// `aterm`. A run handed no bundle judged nothing: COULD NOT RUN, never a
    /// skip and never a binary of the gate's choosing. The suites' own sources
    /// read those names (checked from the tree, so a rename fails here).
    #[test]
    fn the_measuring_stage_hands_paint_and_spin_the_bundle_it_was_handed() {
        let c = ctx(Scope::workspace());
        let artifact = crate::MeasureArtifact {
            app: PathBuf::from("/release/aterm.app"),
            macho_sha256: "a".repeat(64),
        };
        let env_of = |cmd: &Cmd, key: &str| {
            cmd.envs
                .iter()
                .find(|(k, _)| k.to_str() == Some(key))
                .map(|(_, v)| PathBuf::from(v))
        };
        let (_, run) = measuring_cmd(&c, &artifact, false);
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
        let tests =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-conformance/tests");
        for (file, needle) in [
            ("paint/measuring.rs", r#"release_bin(&["ATERM_PAINT_BIN"])"#),
            (
                "spin/measuring.rs",
                r#"release_bin(&["ATERM_SPIN_BIN", "ATERM_PAINT_BIN"])"#,
            ),
            ("support/mod.rs", "for var in overrides {"),
        ] {
            let text = std::fs::read_to_string(tests.join(file)).expect(file);
            assert!(text.contains(needle), "{file}: {needle}");
        }

        // No bundle: COULD NOT RUN, with the flag that hands one.
        let mut c = ctx(Scope::workspace());
        c.tools.targo = PathBuf::from("/usr/bin/false");
        c.tools.refused = None;
        let mut r = Report::new("measuring");
        measuring_tests(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(
            outcome,
            crate::Outcome::Fail(crate::Severity::CouldNotRun),
            "{label}"
        );
        assert!(label.contains("--measure <APP>"), "{label}");
    }

    #[test]
    fn libc_oracle_owns_an_absolute_target_dir_and_suppresses_python_bytecode() {
        let c = ctx(Scope::workspace());
        let cmd = libc_oracle_cmd(&c).expect("the absolute fixture root resolves");
        assert_eq!(cmd.program, PathBuf::from("/repo/libc-oracle/run.sh"));
        assert_eq!(
            cmd.envs,
            [
                ("CARGO_TARGET_DIR".into(), "/repo/libc-oracle/target".into()),
                ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
            ],
            "the required lane names its own target dir"
        );

        let mut relative = ctx(Scope::workspace());
        relative.root = PathBuf::from("relative-repo");
        let cmd = libc_oracle_cmd(&relative).expect("a relative repo root can be absolutized");
        let target = &cmd.envs[0].1;
        assert!(
            PathBuf::from(target).is_absolute(),
            "run.sh's children run from different cwds, so this cannot be relative: {target:?}"
        );
        assert!(
            PathBuf::from(target).ends_with("relative-repo/libc-oracle/target"),
            "the lane must remain rooted in its repository: {target:?}"
        );
    }

    #[test]
    fn libc_oracle_exit_three_is_could_not_run_not_a_finding() {
        assert_eq!(libc_oracle_outcome(Some(0)), Outcome::Ok);
        assert_eq!(
            libc_oracle_outcome(Some(1)),
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(
            libc_oracle_outcome(Some(3)),
            Outcome::Fail(Severity::CouldNotRun)
        );
        assert_eq!(
            libc_oracle_outcome(None),
            Outcome::Fail(Severity::CouldNotRun)
        );
        assert_eq!(
            libc_oracle_outcome(Some(2)),
            Outcome::Fail(Severity::GateFailed),
            "an undeclared driver status is not allowed to masquerade as an environment verdict"
        );
    }

    #[test]
    fn a_machine_with_no_doc_driver_anywhere_is_diagnosed_on_the_doctests_row() {
        // targo exists, trustdoc does not, and nothing on the children's PATH
        // answers to the bare name. The doctests stage, the only doctest runner,
        // says COULD-NOT-RUN with the remedy before spawning anything. The test
        // stage's two children spawn no rustdoc, so they run (`/usr/bin/true`
        // stands in for targo, and two Ok outcomes prove both ran).
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
        let cb = no_driver(Scope::changed("main", vec!["xtask".into()], false));
        let r = run_stage(&cb, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "bin-only doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip, "{}", outcomes[0].1);

        // `--scope <crate>` answers the lib question from the member's own
        // manifest: a bin-only scope skips, and the same scope with a lib is a
        // real diagnosis.
        let tmp = crate::mktemp_dir("atv-doc-scope").expect("mktemp");
        let bin_only = tmp.join("crates/binonly");
        std::fs::create_dir_all(bin_only.join("src")).unwrap();
        std::fs::write(
            bin_only.join("Cargo.toml"),
            "[package]\nname = \"binonly\"\n",
        )
        .unwrap();
        let mut cx = no_driver(Scope::Crate("binonly".into()));
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
            measuring_args(&Scope::workspace()),
            measuring_args(&Scope::crate_only("aterm-conformance")),
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
                "warnings",
                "--cfg",
                "clean_islands"
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
                "warnings",
                "--cfg",
                "clean_islands"
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
                outcomes[0]
                    .1
                    .contains("(change-scoped run selected no crates)"),
                "{stage:?} said: {}",
                outcomes[0].1
            );
        }
    }

    #[test]
    fn the_lint_is_denied_warnings_after_the_separator() {
        let a = tippy_args(&Scope::workspace());
        let sep = a.iter().position(|x| x == "--").expect("a -- separator");
        assert_eq!(&a[sep + 1..], ["-D", "warnings", "--cfg", "clean_islands"]);
        assert!(a[..sep].contains(&"--all-targets".to_string()));
        // `--keep-going` is a coverage guarantee: without it the first crate
        // that trips `-D warnings` ends the run. It must sit on cargo's side of
        // the separator; after `--` it would reach the lint driver, which does
        // not know the flag.
        assert!(
            a[..sep].contains(&"--keep-going".to_string()),
            "the lint must not stop at the first failing crate: {a:?}"
        );
        // …and the `--full` required-features pass holds to the same three.
        let g = tippy_gated_args(&Scope::workspace(), &fixture_gated())
            .expect("the workspace selects both");
        let sep = g.iter().position(|x| x == "--").expect("a -- separator");
        assert_eq!(
            &g[sep + 1..],
            ["-D", "warnings", "--cfg", "clean_islands"],
            "{g:?}"
        );
        for flag in ["--all-targets", "--keep-going"] {
            assert!(g[..sep].contains(&flag.to_string()), "{flag}: {g:?}");
        }
    }

    /// The merge contract's cargo children, spelled out: an added `--lib`,
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
                "--skip",
                "measuring::",
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
            measuring_args(&ws),
            words(&[
                "--unverified",
                "test",
                "--workspace",
                "--no-fail-fast",
                "--test",
                "paint",
                "--test",
                "spin",
                "--",
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
                "warnings",
                "--cfg",
                "clean_islands"
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
                "--cfg",
                "clean_islands",
            ]))
        );
    }

    #[test]
    fn tippy_keeps_its_own_target_dir_and_migration_quiet() {
        // Both matter: a shared target dir would churn the main build on every
        // run, and the migration warning would fail the lint under -D warnings.
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
        assert_eq!(
            cmd.envs[1].1.to_string_lossy(),
            "/repo/target-tippy",
            "the lint must never share the stock build's target dir"
        );
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
                macho_sha256: "a".repeat(64),
            };
            let (_, measuring) = measuring_cmd(&c, &artifact, bind);
            assert!(
                !measuring.demoted,
                "the measuring tests run the paint and spin guards"
            );
            assert_eq!(measuring.argv()[1..], measuring_args(&c.scope)[..]);
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
        let [(build_label, build), (suite_label, suite)] = sealed_lane_cmds(&c);
        assert!(build.demoted, "{build_label}");
        assert!(
            !suite.demoted,
            "{suite_label} boots real aterm-gui --headless processes"
        );
        assert!(
            atpkg_build_cmd(&c).demoted,
            "the atpkg the pack suite drives is only compiled here"
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
                "--cfg",
                "clean_islands",
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
        // Without it the trail laws run only each family's spine in the gate's
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

        // The dependent stages then skip — honestly, and named, so the verdict
        // refuses the merge contract for the whole run.
        let dependent: [fn(&Ctx, &mut Report); 6] = [
            test,
            measuring_tests,
            doctests,
            freeze_gate,
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

    #[test]
    fn a_missing_helper_script_can_never_pass() {
        let c = ctx(Scope::workspace());
        let script_stages: [fn(&Ctx, &mut Report); 5] = [
            grep_guards,
            delivery_tooling,
            atpkg_tooling,
            start_compare,
            libc_oracle,
        ];
        for stage in script_stages {
            let mut r = Report::new("s");
            stage(&c, &mut r);
            let (outcome, label) = r.outcomes().next().expect("a decision");
            assert_eq!(
                outcome,
                crate::Outcome::Fail(crate::Severity::CouldNotRun),
                "{label} must fail closed"
            );
            assert!(label.contains("missing or not executable"), "{label}");
        }
    }

    /// The export content scan's exit: `0` passes only with its PASS line; `1` is
    /// the finding only with its FAIL or REFUSED line; `77` is a named skip with
    /// the scan's reason; `3` is COULD NOT RUN with its reason; anything else a
    /// finding; a death with no status decided nothing. A `0` or `1` without a
    /// verdict line decided nothing (an uncaught Python exception exits `1`).
    #[test]
    fn the_export_content_scan_reads_every_code_it_answers() {
        let name = EXPORT_CONTENT_SCRIPT;
        assert_eq!(
            export_content_outcome(Some(0), "EXPORT CONTENT: PASS — clean\n"),
            (Outcome::Ok, name.to_string())
        );
        let (o, l) = export_content_outcome(Some(0), "export content scan: …\n");
        assert_eq!(o, Outcome::Fail(Severity::CouldNotRun), "a pass says so");
        assert!(
            l.contains("exit 0 without its `EXPORT CONTENT: PASS`"),
            "{l}"
        );
        let (o, l) = export_content_outcome(Some(1), "EXPORT CONTENT: FAIL\n");
        assert_eq!(o, Outcome::Fail(Severity::GateFailed));
        assert!(l.contains("refuse this tree's export"), "{l}");
        let (o, l) = export_content_outcome(
            Some(1),
            "  engine: FAIL: transforms failed\nEXPORT CONTENT: REFUSED — the engine refused \
             the export (publish/transforms.sh); no content guard ran\n",
        );
        assert_eq!(o, Outcome::Fail(Severity::GateFailed));
        assert!(l.contains("refuses to export this tree"), "{l}");
        let (o, l) = export_content_outcome(
            Some(1),
            "export content scan: …\nTraceback (most recent call last):\n  File \"…\", line \
             595, in scan\nTypeError: export_tree() missing 1 required positional argument: \
             'captured'\n",
        );
        assert_eq!(
            o,
            Outcome::Fail(Severity::CouldNotRun),
            "an uncaught exception is no finding about the tree"
        );
        assert!(l.contains("exit 1 without its"), "{l}");
        let (o, l) = export_content_outcome(
            Some(EXPORT_CONTENT_NOT_RUN),
            "export content scan: …\nNOT RUN: no publication engine checkout at /x (--engine)\n",
        );
        assert_eq!(
            o,
            Outcome::Skip,
            "a machine without the engine is a named skip"
        );
        assert!(
            l.contains("NOT RUN — no publication engine checkout at /x (--engine)")
                && l.contains("never a pass"),
            "{l}"
        );
        let (o, l) = export_content_outcome(Some(77), "");
        assert_eq!(o, Outcome::Skip);
        assert!(l.contains("skipped without saying why"), "{l}");
        let (o, l) = export_content_outcome(
            Some(crate::exit::COULD_NOT_RUN),
            "COULD NOT RUN: the engine at /e has no scan_forbidden — its API moved\n",
        );
        assert_eq!(o, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            l.contains(": the engine at /e has no scan_forbidden — its API moved (exit 3)"),
            "{l}"
        );
        for code in [2, 4, 101] {
            let (o, l) = export_content_outcome(Some(code), "");
            assert_eq!(o, Outcome::Fail(Severity::GateFailed), "{code}");
            assert!(l.contains(&format!("unexpected exit {code}")), "{l}");
        }
        assert_eq!(
            export_content_outcome(None, "").0,
            Outcome::Fail(Severity::CouldNotRun)
        );
    }

    /// The scan is handed the root it judges and nothing else, and ended with
    /// `SIGTERM` first so its scratch export goes with it. A missing script is a
    /// cannot-run; a FAIL row is inheritable only when its verdict line shows all
    /// three guards decided (a refused export reached none).
    #[cfg(unix)]
    #[test]
    fn the_export_content_stage_runs_the_scan_on_its_root_and_reads_its_verdict() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::mktemp_dir("atv-export-content").expect("mktemp");
        let tools = tmp.join("tools");
        std::fs::create_dir_all(&tools).expect("mkdir");
        let mut c = ctx(Scope::workspace());
        c.root = tmp.clone();
        let cmd = export_content_cmd(&c);
        assert_eq!(
            cmd.argv(),
            [
                tools.join(EXPORT_CONTENT_SCRIPT).display().to_string(),
                tmp.display().to_string()
            ]
        );
        assert_eq!(cmd.term_grace, Some(exec::TERM_GRACE));
        assert!(cmd.envs.is_empty(), "{:?}", cmd.envs);

        let run = |c: &Ctx| {
            let mut r = Report::new("export content");
            export_content(c, &mut r);
            r
        };
        let missing = run(&c);
        let rows: Vec<_> = missing.outcomes().collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, Outcome::Fail(Severity::CouldNotRun));
        assert!(rows[0].1.contains("missing or not executable"), "{rows:?}");

        let script = |body: &str| {
            let path = tools.join(EXPORT_CONTENT_SCRIPT);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        };
        script("test -d \"$1/tools\" || exit 9; echo 'EXPORT CONTENT: PASS — clean'");
        let ok = run(&c);
        assert_eq!(
            ok.outcomes().collect::<Vec<_>>(),
            [(Outcome::Ok, EXPORT_CONTENT_SCRIPT)]
        );

        script(
            "echo '  crates/a/src/x.rs:12  forbidden content — Credentials (baseline/forbidden-content.txt:19)'; \
             echo '  crates/a/src/x.rs:40  forbidden content — Credentials (baseline/forbidden-content.txt:19)'; \
             echo 'EXPORT CONTENT: FAIL'; exit 1",
        );
        let found = run(&c);
        let (outcome, _, findings) = found.decisions().next().expect("a row");
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed));
        assert_eq!(
            findings
                .iter()
                .map(|f| (f.id.as_str(), f.opaque))
                .collect::<Vec<_>>(),
            [
                (
                    "export-content-scan.py -- crates/a/src/x.rs -- forbidden content — Credentials",
                    None
                ),
                (
                    "export-content-scan.py -- crates/a/src/x.rs -- forbidden content — Credentials #2",
                    None
                )
            ],
            "one finding per hit, keyed without its line, each inheritable"
        );
        assert!(found.render().contains("crates/a/src/x.rs:12"));

        script(
            "echo '  engine: FAIL: transforms failed'; \
             echo 'EXPORT CONTENT: REFUSED — the engine refused the export'; exit 1",
        );
        let refused = run(&c);
        let (outcome, _, findings) = refused.decisions().next().expect("a row");
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed));
        assert!(
            findings[0].opaque.is_some(),
            "a refused export decided no guard, so it is never inherited"
        );

        script("echo 'NOT RUN: no gitleaks on PATH'; exit 77");
        let skipped = run(&c);
        let rows: Vec<_> = skipped.outcomes().collect();
        assert_eq!(rows[0].0, Outcome::Skip);
        assert!(rows[0].1.contains("no gitleaks on PATH"), "{rows:?}");

        // A scan that died of an exception it never caught is no finding.
        script("echo 'Traceback (most recent call last):'; echo 'TypeError: x'; exit 1");
        let crashed = run(&c);
        let rows: Vec<_> = crashed.outcomes().collect();
        assert_eq!(rows[0].0, Outcome::Fail(Severity::CouldNotRun), "{rows:?}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Each delivery suite is its own decision, run from the tree the gate
    /// verifies: a red suite fails the stage under its own name while its
    /// siblings still run, and a missing one is a cannot-run. The all-green first
    /// pass is the negative control.
    #[cfg(unix)]
    #[test]
    fn every_delivery_suite_runs_and_decides_on_its_own() {
        let tmp = crate::mktemp_dir("atv-delivery-suites").expect("mktemp");
        let tools = tmp.join("tools");
        // One inode per distinct text, so the 28 `exit 0` suites pay the
        // first-exec assessment once, not 28 times (`crate::standin`).
        let suite = |name: &str, body: &str| crate::standin(&tools.join(name), body);
        for name in DELIVERY_SUITES {
            suite(name, "exit 0");
        }
        let mut c = ctx(Scope::workspace());
        c.root = tmp.clone();
        let decide = |c: &Ctx| {
            let mut r = Report::new("delivery tooling");
            delivery_tooling(c, &mut r);
            r.outcomes()
                .map(|(o, l)| (o, l.to_string()))
                .collect::<Vec<_>>()
        };

        let green = decide(&c);
        assert_eq!(
            green,
            DELIVERY_SUITES
                .iter()
                .map(|n| (Outcome::Ok, (*n).to_string()))
                .collect::<Vec<_>>()
        );
        // Each is its own suite, sent SIGTERM first when killed so its EXIT trap
        // cleans up, and handed nothing: the atpkg suites run producer scripts
        // with `$ATPKG` unset.
        for name in DELIVERY_SUITES {
            let cmd = delivery_suite_cmd(&c, name);
            assert_eq!(cmd.argv(), [tools.join(name).display().to_string()]);
            assert_eq!(cmd.term_grace, Some(exec::TERM_GRACE), "{name}");
            assert!(cmd.envs.is_empty(), "{name}: {:?}", cmd.envs);
        }

        suite("test-publish-export.sh", "echo widened >&2; exit 1");
        std::fs::remove_file(tools.join("test-install-guard.sh")).expect("rm");
        let mixed = decide(&c);
        assert_eq!(mixed.len(), DELIVERY_SUITES.len(), "{mixed:?}");
        assert_eq!(
            mixed[0],
            (Outcome::Ok, "test-install-channel.sh".to_string())
        );
        assert_eq!(
            mixed[1],
            (
                Outcome::Fail(Severity::GateFailed),
                "test-publish-export.sh".to_string()
            )
        );
        assert_eq!(mixed[2].0, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            mixed[2]
                .1
                .starts_with("test-install-guard.sh missing or not executable"),
            "{mixed:?}"
        );
        // …and every suite after them still ran: one red row never hides the
        // rest of the roster.
        assert!(
            mixed[3..].iter().all(|(o, _)| *o == Outcome::Ok),
            "{mixed:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// The suites share the machine and print as a serial run would. Each stub
    /// suite logs its start and end to one shared file. A [`DELIVERY_ALONE`]
    /// suite runs with no other suite between its start and end; output, row and
    /// `time` line print in [`DELIVERY_SUITES`] order; a failing suite fails the
    /// stage whether it ran in the pool or alone. Negative control: a green run
    /// of the same roster.
    #[cfg(unix)]
    #[test]
    fn delivery_suites_share_the_machine_and_print_in_declared_order() {
        let tmp = crate::mktemp_dir("atv-delivery-pool").expect("mktemp");
        let tools = tmp.join("tools");
        let log = tmp.join("log");
        // The stub's text names neither itself nor the log: `$0` is the link
        // that ran, `tools/../log` is `log` above. So the 28 green suites are
        // one text and one inode, and pay the first-exec assessment once
        // (`crate::standin`).
        let suite = |name: &str, code: u8| {
            crate::standin(
                &tools.join(name),
                &format!(
                    "n=${{0##*/}}; log=${{0%/*}}/../log\n\
                     echo \"start $n\" >>\"$log\"\necho \"said $n\"\n\
                     echo \"end $n\" >>\"$log\"\nexit {code}"
                ),
            );
        };
        let mut c = ctx(Scope::workspace());
        c.root = tmp.clone();
        let decide = |c: &Ctx| {
            std::fs::write(&log, "").expect("log");
            let mut r = Report::new("delivery tooling");
            delivery_tooling(c, &mut r);
            r
        };
        let serial = |failing: &[&str]| -> Vec<String> {
            DELIVERY_SUITES
                .iter()
                .flat_map(|n| {
                    let tag = if failing.contains(n) {
                        "  FAIL  "
                    } else {
                        "  ok    "
                    };
                    [format!("said {n}"), format!("{tag}{n}"), "time".to_string()]
                })
                .collect()
        };
        // What the stage printed, reduced to each suite's own line, its row
        // and the time line under it.
        let printed = |r: &Report| -> Vec<String> {
            r.render()
                .lines()
                .filter(|l| {
                    l.starts_with("said ")
                        || l.starts_with("    time  ")
                        || l.starts_with("  ok    ")
                        || l.starts_with("  FAIL  ")
                })
                .map(|l| {
                    if l.starts_with("    time  ") {
                        "time".to_string()
                    } else {
                        l.to_string()
                    }
                })
                .collect()
        };
        let alone_ran_alone = || {
            let lines: Vec<String> = std::fs::read_to_string(&log)
                .expect("log")
                .lines()
                .map(String::from)
                .collect();
            assert_eq!(lines.len(), 2 * DELIVERY_SUITES.len(), "{lines:?}");
            // Each ran before the pool started ([`DELIVERY_ALONE`]), so the log
            // opens with them: this pins the documented ORDER (alone suites
            // first, then the pool), which includes that none overlapped the
            // pool. The adjacency below catches an overlap only when
            // two instant stubs happen to cross, which one shared inode (no
            // first-exec wait to line them up) makes rare; this cannot miss.
            let first: Vec<String> = DELIVERY_ALONE
                .iter()
                .flat_map(|(n, _)| [format!("start {n}"), format!("end {n}")])
                .collect();
            assert_eq!(lines[..first.len()], first[..], "{lines:?}");
            for (name, _) in DELIVERY_ALONE {
                let at = lines
                    .iter()
                    .position(|l| *l == format!("start {name}"))
                    .expect("started");
                assert_eq!(lines[at + 1], format!("end {name}"), "{name}: {lines:?}");
            }
        };

        for name in DELIVERY_SUITES {
            suite(name, 0);
        }
        let green = decide(&c);
        assert_eq!(printed(&green), serial(&[]), "{}", green.render());
        assert_eq!(crate::outcome_word(&green), "ok");
        alone_ran_alone();

        let pooled = DELIVERY_SUITES[DELIVERY_SUITES.len() - 1];
        let (alone, _) = DELIVERY_ALONE[0];
        assert_eq!(delivery_alone(pooled), None);
        suite(pooled, 1);
        suite(alone, 1);
        let red = decide(&c);
        assert_eq!(printed(&red), serial(&[pooled, alone]), "{}", red.render());
        let t = crate::ladder::tally(std::slice::from_ref(&red));
        assert_eq!(
            t.gate_failures,
            [alone, pooled],
            "every failure, in declared order"
        );
        assert!(t.could_not_run.is_empty(), "{:?}", t.could_not_run);
        alone_ran_alone();

        assert_eq!(delivery_jobs(1), 1, "never zero");
        assert_eq!(delivery_jobs(18), 4);
        assert_eq!(delivery_jobs(256), 4, "never more than four");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// `--no-windows` compiles no window driver: the tail driver builds keep only
    /// the live lanes' `aterm` (macOS) and leave out the redraw harness and the
    /// objc drivers, and the row says so. Without the flag every tail build is
    /// there (`tail_builds_select_only_what_the_tail_drives`).
    #[test]
    fn no_windows_leaves_the_window_drivers_out_of_the_tail_builds() {
        let mut quiet = ctx(Scope::workspace()).with_no_windows(true);
        let argvs: Vec<Vec<String>> = tail_driver_build_cmds(&quiet)
            .into_iter()
            .map(|(_, cmd)| cmd.argv()[1..].to_vec())
            .collect();
        let want = if cfg!(target_os = "macos") {
            vec![live_aterm_build_args()]
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
            .expect("the gate must ask for the feature that compiles it");
        assert_eq!(a[feat + 1], "control-conformance");
        let bin = a.iter().position(|x| x == "--bin").expect("a --bin");
        assert_eq!(a[bin + 1], REDRAW_CONFORMANCE_BIN);
        assert!(a.contains(&"aterm-gui".to_string()));
    }

    #[test]
    fn a_cell_gate_that_compiled_nothing_is_counted_as_a_skip_not_a_pass() {
        // `gate cells` exits 0 for an uninstalled std on purpose, so only the
        // transcript tells the matrix discharged from no compiler started.
        let unproven = format!(
            "{CELLS_NOT_PROVEN} — 1 of the 5 cell(s) this run selected had no installed std, \
             so NO COMPILER READ THEM: win (x86_64-pc-windows-msvc). 4 checked: …\n"
        );
        let (outcome, label) = cells_outcome("gate cells", true, &unproven);
        assert_eq!(outcome, Outcome::Skip);
        assert_ne!(outcome, Outcome::Ok);
        assert!(label.contains("NOTHING was compiled"), "{label}");

        // A run that really did discharge it is still a pass.
        let green = "gate cells: GREEN — all 5 cells type-check, with nothing excused or \
                     shimmed: every one of the 312 in-repo crate-instances …\n";
        assert_eq!(cells_outcome("gate cells", true, green).0, Outcome::Ok);

        // A failure stays a failure however the transcript reads — including one
        // that also carries the marker, which a partly-skipped red run does.
        assert_eq!(
            cells_outcome("gate cells", false, &unproven).0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(
            cells_outcome("gate cells", false, green).0,
            Outcome::Fail(Severity::GateFailed)
        );

        // Anchored at column 0: a compiler diagnostic quoting the sentinel from
        // this repo's source is tree text, not a verdict.
        let quoted = format!(
            "   |     pub const CELLS_NOT_PROVEN: &str = \"{CELLS_NOT_PROVEN}\";\n\
             gate cells: GREEN — all 5 cells type-check…\n"
        );
        assert_eq!(cells_outcome("gate cells", true, &quoted).0, Outcome::Ok);
    }

    #[test]
    fn a_skipped_cell_gate_cannot_leave_the_merge_contract_claimed() {
        // End to end against the verdict: one skipped stage, whole-tree scope,
        // nothing failed, and the sentence is still withheld.
        let mut r = Report::new("cross-cell type-check");
        let (outcome, label) = cells_outcome(
            "gate cells-foreign",
            true,
            &format!("{CELLS_NOT_PROVEN} — 5 of 5 …\n"),
        );
        r.record(outcome, label);
        let t = crate::ladder::tally(&[r]);
        assert_eq!(t.skipped(), 1);
        assert!(!crate::verdict::discharges_merge_contract(
            crate::Mode::Full,
            &crate::scope::Scope::workspace(),
            &t
        ));
    }

    /// `--no-windows`: in every mode each planned [`crate::plan::WINDOWED`]
    /// stage is one named skip, decided before anything is built or launched,
    /// and the run cannot claim the merge contract; the flag removes no stage and
    /// touches no other. Negative control: without the flag each such stage runs
    /// its own body (here, with no driver, to its own skip).
    #[test]
    fn no_windows_skips_each_windowed_stage_by_name_and_nothing_else() {
        use crate::plan::{NO_WINDOWS_REASON, WINDOWED, plan};
        let base = |mode| {
            let mut c = ctx(Scope::workspace());
            c.mode = mode;
            // No driver, so a stage body that does run cannot build or launch.
            c.tools.targo = PathBuf::from("/nonexistent/targo");
            c
        };
        let mut seen = Vec::new();
        for mode in [Mode::Fast, Mode::Measure, Mode::Full] {
            let plain = base(mode);
            let quiet = base(mode).with_no_windows(true);
            let planned = plan(&quiet);
            assert_eq!(planned, plan(&plain), "{mode:?}: the flag removes no stage");
            for spec in &planned {
                let Some((_, name)) = WINDOWED.iter().find(|(id, _)| *id == spec.id) else {
                    assert_eq!(spec.id.no_windows_skip(), None, "{:?}", spec.id);
                    continue;
                };
                seen.push(spec.id);
                let want = format!("{name} ({NO_WINDOWS_REASON})");
                let r = run_stage(&quiet, spec);
                assert_eq!(
                    r.outcomes().collect::<Vec<_>>(),
                    [(Outcome::Skip, want.as_str())],
                    "{mode:?} {:?}",
                    spec.id
                );
                let t = crate::ladder::tally(std::slice::from_ref(&r));
                assert!(
                    !crate::verdict::discharges_merge_contract(mode, &quiet.scope, &t),
                    "{mode:?} {:?}: a windowed skip claims no merge contract",
                    spec.id
                );
                let ran = run_stage(&plain, spec);
                assert!(
                    ran.outcomes()
                        .all(|(_, label)| !label.contains(NO_WINDOWS_REASON)),
                    "{mode:?} {:?}: without the flag the stage's own body decides: {}",
                    spec.id,
                    ran.render()
                );
                assert!(ran.outcomes().next().is_some(), "{:?} decided", spec.id);
            }
        }
        for (id, _) in WINDOWED {
            assert!(seen.contains(&id), "{id:?} is planned by some mode");
        }
    }

    /// Every row reads the shared codes alike and its own `3` its own way: `0`
    /// passes, `1` is a finding, `2` is not run (never green, never a quiet
    /// skip), any undeclared code is a finding. Exit `3` is the audit's weaker
    /// pass, the toolbar's hung (could not run) or the event drive's aborted
    /// (the finding); a death with no status reads as the row says.
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
    /// were held to the fork's own declaration rather than the runtime's, so a
    /// receipt must not assert runtime authority for them.
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
        // A panic (101) or a signal is a finding/that-decided-nothing, never green.
        assert_eq!(
            redraw_outcome(Some(101)).0,
            Outcome::Fail(Severity::GateFailed)
        );
        assert_eq!(redraw_outcome(None).0, Outcome::Fail(Severity::CouldNotRun));
        for code in [None, Some(1), Some(2), Some(101), Some(-1)] {
            assert_ne!(redraw_outcome(code).0, Outcome::Ok, "{code:?}");
        }
    }

    /// The test run and the filtered stage split one selection: the same package
    /// selection in every scope, the run skipping the filter and the stage running
    /// exactly it over the targets that hold it
    /// ([`every_skipped_module_is_declared_where_its_stage_looks`]). Negative
    /// control: an argv that skips nothing.
    #[test]
    fn the_test_run_and_the_measuring_stage_split_one_selection_by_their_filter_list() {
        fn strs(a: &[String]) -> Vec<&str> {
            a.iter().map(String::as_str).collect()
        }
        for s in [
            Scope::workspace(),
            Scope::crate_only("aterm-conformance"),
            Scope::changed("main", vec!["atpkg".into(), "aterm-update".into()], true),
        ] {
            let base = test_base_args(&s);
            let run = test_run_args(&s);
            let split = |a: &[String]| {
                let at = a.iter().position(|x| x == "--").expect("libtest args");
                (a[..at].to_vec(), a[at + 1..].to_vec())
            };
            let (run_cargo, run_libtest) = split(&run);
            assert_eq!(run_cargo[..base.len()], base[..], "{run:?}");
            assert_eq!(strs(&run_cargo[base.len()..]), ["--tests"], "{run:?}");
            // The timing flags lead, and select nothing.
            let run_libtest = run_libtest
                .strip_prefix(&LIBTEST_REPORT_TIME.map(String::from)[..])
                .expect("the run times its tests")
                .to_vec();
            let skipped: Vec<&str> = run_libtest
                .chunks(2)
                .map(|pair| {
                    assert_eq!(pair[0], "--skip", "{run_libtest:?}");
                    pair[1].as_str()
                })
                .collect();
            let args = measuring_args(&s);
            let (cargo, libtest) = split(&args);
            assert_eq!(cargo[..base.len()], base[..], "one selection: {args:?}");
            assert_eq!(strs(&cargo[base.len()..]), MEASURING_TARGETS, "{args:?}");
            assert_eq!(strs(&libtest), MEASURING_TESTS, "{args:?}");
            assert_eq!(skipped, MEASURING_TESTS, "{run:?}");
        }
        // The negative control: the argv before the split skipped nothing, so
        // every one of these ran in the parallel run.
        let before = test_base_args(&Scope::workspace());
        assert!(!before.iter().any(|a| a == "--skip"), "{before:?}");
        // …and every filter names a module, so it matches only a test path
        // (`measuring::row`), never a function name containing the word.
        for f in &MEASURING_TESTS {
            assert!(f.ends_with("::"), "{f}");
        }
    }

    /// The modules `src` declares: every `mod <name>` that opens an item — at
    /// the start of a line, or after a visibility, an attribute or another
    /// item on it — outside `//` comments. Text that merely reads `mod x`
    /// inside a string is not one: its quote precedes it.
    fn declared_modules(src: &str) -> Vec<&str> {
        let ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut found = Vec::new();
        for line in src.lines().map(str::trim_start) {
            if line.starts_with("//") {
                continue;
            }
            let mut from = 0;
            while let Some(at) = line[from..].find("mod ").map(|i| from + i) {
                from = at + 4;
                let before = line[..at].trim_end();
                let opens = before.is_empty()
                    || before.ends_with("pub")
                    || before.ends_with([')', ']', '{', '}', ';']);
                if !opens || line[..at].ends_with(ident) {
                    continue;
                }
                let rest = line[from..].trim_start();
                let name = &rest[..rest.find(|c| !ident(c)).unwrap_or(rest.len())];
                if !name.is_empty() {
                    found.push(name);
                }
            }
        }
        found
    }

    /// A SKIPPED MODULE IS DECLARED ONLY WHERE ITS STAGE LOOKS (2026-09-27).
    ///
    /// libtest's `--skip` is a SUBSTRING match, so the test run's `--skip
    /// measuring::` skips every test under any module whose name ENDS in
    /// `measuring` (`spin_measuring::row` too), in every target of every crate
    /// — while the measuring stage selects only the `paint` and `spin` targets.
    /// A module named to match the filter anywhere else would be skipped by
    /// the run and selected by no stage:
    /// tests that never run and never say so. So, read from every source under
    /// `crates/` (the workspace's members): a module whose name ends in a
    /// filter's is declared in one of that filter's own sites. And each site
    /// declares its module, so the walk is not vacuous and the stage has
    /// something to select.
    #[test]
    fn every_skipped_module_is_declared_where_its_stage_looks() {
        let owners: [(&str, &[&str]); 1] = [(
            "measuring::",
            &[
                "aterm-conformance/tests/paint.rs",
                "aterm-conformance/tests/spin.rs",
            ],
        )];
        assert_eq!(
            owners.map(|(f, _)| f).to_vec(),
            MEASURING_TESTS,
            "a row per filter"
        );
        let owner = |name: &str| {
            owners
                .iter()
                .find(|(f, _)| name.ends_with(f.trim_end_matches(':')))
        };

        // The parser, on the shapes that matter: it must see these…
        // (spelled through `m`, so this file declares nothing the walk finds)
        let m = "mod";
        let seen =
            format!("{m} measuring;\n#[cfg(test)] {m} spin_measuring {{\npub(crate) {m} x;\n");
        assert_eq!(
            declared_modules(&seen),
            ["measuring", "spin_measuring", "x"]
        );
        // …and none of these.
        let unseen = format!("// {m} measuring;\nlet s = \"{m} measuring;\";\nre{m} x\n");
        assert!(declared_modules(&unseen).is_empty(), "{unseen}");

        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut found: Vec<(String, String)> = Vec::new();
        let mut stack = vec![crates.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("a source dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n != "target") {
                        stack.push(path);
                    }
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("a source file");
                    let rel = path
                        .strip_prefix(&crates)
                        .expect("under crates/")
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    for name in declared_modules(&text) {
                        if owner(name).is_some() {
                            found.push((rel.clone(), name.to_string()));
                        }
                    }
                }
            }
        }
        for (rel, name) in &found {
            let (filter, sites) = owner(name).expect("matched above");
            assert!(
                sites.contains(&rel.as_str()),
                "`mod {name}` in crates/{rel} matches the test run's `--skip {filter}`, but \
                 the stage that runs that filter looks only in {sites:?}, so its tests would \
                 run nowhere: rename the module, or move it into one of those targets"
            );
        }
        found.sort();
        assert_eq!(
            found,
            [
                ("aterm-conformance/tests/paint.rs", "measuring"),
                ("aterm-conformance/tests/spin.rs", "measuring"),
            ]
            .map(|(r, n)| (r.to_string(), n.to_string())),
            "each site declares the module its stage selects"
        );
    }

    /// A caller's `CARGO_BUILD_JOBS` is a CEILING for the side lanes' constants —
    /// never a floor, never a parse error. Measured 2026-09-15 on a 4-core Intel
    /// MacBook Pro: `CARGO_BUILD_JOBS=4 tools/verify.sh --fast` capped the main
    /// and lint lanes (they inherit) while the driver lane went to 8 and the
    /// three side lanes to 4 regardless, so the export changed nothing about
    /// the overlap that ran the box at a load of 17–20.
    #[test]
    fn a_callers_cargo_build_jobs_is_a_ceiling_for_the_side_lanes() {
        let xtask = Lane::XtaskTarget;
        assert_eq!(lane_jobs(xtask, None), Some(4));
        assert_eq!(lane_jobs(xtask, Some(2)), Some(2));
        assert_eq!(lane_jobs(xtask, Some(16)), Some(4));
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
            Lane::Doctests,
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
        assert!(
            envs(&xtask_cmd(&c, xtask_gate_args("forge")))
                .contains(&("CARGO_BUILD_JOBS".to_string(), "3".to_string()))
        );
    }

    /// The tail driver builds build only what the tail drives, which lets the
    /// test run start without them ([`crate::plan::NOT_AWAITED`]). Every child
    /// names its targets (`--bin` or `--example`; a bare `-p aterm-gui` would
    /// also link the one the `--full` sealed rung drives before the barrier);
    /// every target is one a row behind the barrier drives and never one a row
    /// the test run waits for drives; and every tail binary is built there, so
    /// no row behind the barrier compiles inside the exclusive section.
    #[test]
    fn tail_builds_select_only_what_the_tail_drives() {
        let c = ctx(Scope::workspace());
        let mac = cfg!(target_os = "macos");
        let mut tail_driven: Vec<String> = vec![REDRAW_CONFORMANCE_BIN.to_string()];
        if mac {
            tail_driven.push("aterm".to_string());
            tail_driven.extend(OBJC_DRIVES.iter().map(|d| d.example.to_string()));
        }
        let before_the_barrier = ["aterm-gui", "aterm-ctl", "atpkg"];
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
        // The two rows' children are exactly the four builds the one row ran.
        let mut both: Vec<Vec<String>> = driver_build_cmds(&c)
            .into_iter()
            .chain(tail_driver_build_cmds(&c))
            .map(|(_, cmd)| cmd.argv()[1..].to_vec())
            .collect();
        let mut want = vec![smoke_build_args(), redraw_conformance_build_args()];
        if mac {
            want.push(live_aterm_build_args());
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

        for cmd in [
            xtask_cmd(&c, xtask_gate_args_with("lint", &["--fmt-only"])),
            xtask_cmd(&c, xtask_gate_args("forge")),
        ] {
            assert_eq!(env(&cmd), lane("/repo/target-xtask", Some("4")));
        }
        for (label, cmd) in driver_build_cmds(&c)
            .into_iter()
            .chain(tail_driver_build_cmds(&c))
            .chain(sealed_lane_cmds(&c))
            .chain([(ATPKG_BUILD_LABEL.to_string(), atpkg_build_cmd(&c))])
        {
            assert_eq!(
                env(&cmd),
                lane("/repo/target-drivers", Some("8")),
                "{label}"
            );
        }
        for args in [
            redraw_conformance_build_args(),
            objc_drive_build_args(&OBJC_DRIVES[0]),
        ] {
            assert_eq!(
                env(&driver_build_cmd(&c, args)),
                lane("/repo/target-drivers", Some("8"))
            );
        }
        assert_eq!(drivers_dir(&c), PathBuf::from("/repo/target-drivers"));
        assert_eq!(
            env(&freeze_gate_cmd(&c)),
            lane("/repo/tools/freeze-safety-gate/target", None),
            "the L0 gate names the dir cargo used when nothing was exported"
        );
        // The main lane is handed nothing: its children keep inheriting
        // the caller's target dir, exactly as before. Nor is the doctests'
        // lane, the same directory since 2026-10-05.
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
                Lane::Doctests,
                targo(&c, doctest_args(&c.scope))
            ))
            .is_empty()
        );
    }

    /// EVERY DRIVEN BINARY IS BUILT IN THE DRIVER LANE AND HANDED TO ITS SUITE
    /// — one law for the four rows whose suites drive a binary their own stage
    /// builds: the sealed rung (2026-09-14), the atpkg pack (2026-09-16) and
    /// the two live lanes (2026-09-26). Whatever the caller exported, for each:
    /// the stage's build child only compiles; it and the suite name ONE dir,
    /// the lane the plan schedules the row in; the binary is that dir's, never
    /// `<root>/target`'s — the suites' own default, and a previous run's or one
    /// still linking; and the suite is handed it in the spelling its own source
    /// reads (read from the tree, so a renamed flag reddens here instead of
    /// falling back to the default). The sealed rung is handed NOTHING beyond
    /// the lane: its harness finds `aterm-gui` in the dir the suite was built
    /// into and refuses a stale one, and `ATERM_GUI_BIN` would skip that
    /// refusal instead of satisfying it.
    #[test]
    fn every_driven_binary_is_built_in_the_driver_lane_and_handed_to_its_suite() {
        enum Handed {
            /// Found by the suite's harness in the dir it was built into.
            Discovered,
            /// In this environment variable, and nothing else is set.
            Env(&'static str),
            /// As these arguments, and no environment at all.
            Argv(Vec<String>),
        }
        struct Row {
            id: StageId,
            build: Cmd,
            build_args: Vec<String>,
            suite: Cmd,
            bin: PathBuf,
            handed: Handed,
            /// The file that reads the handover, and what it must spell.
            source: &'static str,
            reads: &'static [&'static str],
        }
        let lane = |cmd: &Cmd| {
            cmd.envs
                .iter()
                .find(|(k, _)| k.to_str() == Some("CARGO_TARGET_DIR"))
                .map(|(_, v)| PathBuf::from(v))
        };
        let mut c = ctx(Scope::workspace());
        // The sealed rung and the Codex lane are `--full`'s.
        c.mode = Mode::Full;
        let plan = crate::plan::plan(&c);
        let drivers = drivers_dir(&c);
        let aterm = live_aterm_binary(&c);
        let [(_, sealed_build), (_, sealed_suite)] = sealed_lane_cmds(&c);
        let rows = [
            Row {
                id: StageId::SealedLane,
                build: sealed_build,
                build_args: smoke_build_args(),
                suite: sealed_suite,
                bin: driver_bin(&c, "aterm-gui"),
                handed: Handed::Discovered,
                source: "crates/aterm-link/tests/harness/mod.rs",
                reads: &[r#"built_binary("aterm-gui", "aterm-gui", "ATERM_GUI_BIN")"#],
            },
            Row {
                id: StageId::AtpkgTooling,
                build: atpkg_build_cmd(&c),
                build_args: atpkg_build_args(),
                suite: atpkg_pack_cmd(&c),
                bin: atpkg_driven_binary(&c),
                handed: Handed::Env("ATPKG"),
                source: "tools/test-atpkg-pack-one-compiler.sh",
                reads: &[r#"ATPKG_BIN="${ATPKG:-}""#],
            },
            Row {
                id: StageId::ForegroundHandback,
                build: live_aterm_build_cmd(&c),
                build_args: live_aterm_build_args(),
                suite: live_aterm_suite_cmd(&c, FOREGROUND_HANDBACK_SUITE),
                bin: aterm.clone(),
                handed: Handed::Argv(vec!["--binary".into(), aterm.display().to_string()]),
                source: "tools/test-foreground-handback.sh",
                // …its flag, its not-run code, which only `not_run` spells,
                // and the teardown that turns any other 2 into a finding.
                reads: &[
                    "--binary) BIN=$2; shift 2 ;;",
                    "    NOT_RUN=1\n    exit 2\n}",
                    "if [[ $status -eq 2 && -z $NOT_RUN ]]; then\n        status=1",
                ],
            },
            Row {
                id: StageId::RenderDesync,
                build: live_aterm_build_cmd(&c),
                build_args: live_aterm_build_args(),
                suite: live_aterm_suite_cmd(&c, RENDER_DESYNC_SUITE),
                bin: aterm.clone(),
                handed: Handed::Argv(vec!["--binary".into(), aterm.display().to_string()]),
                source: "tools/test-render-desync.sh",
                // The handback lane's three, with the gate's own `3`.
                reads: &[
                    "--binary) BIN=$2; shift 2 ;;",
                    "    NOT_RUN=1\n    exit 3\n}",
                    "if [[ $status -eq 3 && -z $NOT_RUN ]]; then\n        status=1",
                ],
            },
            Row {
                id: StageId::CodexLiveUpgrade,
                build: live_aterm_build_cmd(&c),
                build_args: live_aterm_build_args(),
                suite: live_aterm_suite_cmd(&c, CODEX_LIVE_UPGRADE_SUITE),
                bin: aterm.clone(),
                handed: Handed::Argv(vec![aterm.display().to_string()]),
                source: "tools/test-codex-live-upgrade.sh",
                reads: &[r#"A="${1:-$ROOT/target/debug/aterm}""#, "exit 77; }"],
            },
        ];
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for row in rows {
            let what = format!("{:?}", row.id);
            let spec = plan
                .iter()
                .find(|s| s.id == row.id)
                .unwrap_or_else(|| panic!("{what} is planned whole-tree under --full"));
            assert_eq!(row.build.argv()[1..], row.build_args[..], "{what}");
            assert!(row.build.demoted, "{what}: the build only compiles");
            assert!(!row.suite.demoted, "{what}: the suite runs code");
            assert_eq!(lane(&row.build), Some(drivers.clone()), "{what}");
            assert_eq!(lane(&row.build), lane_dir(&c, spec.lane), "{what}");
            assert!(
                row.bin.starts_with(&drivers) && !row.bin.starts_with(c.root.join("target")),
                "{what}: {}",
                row.bin.display()
            );
            match &row.handed {
                Handed::Discovered => {
                    assert_eq!(lane(&row.suite), lane(&row.build), "{what}: one dir");
                    assert_eq!(row.suite.envs, row.build.envs, "{what}: one environment");
                    // The ABSOLUTE pin: an equality alone passes an
                    // `ATERM_GUI_BIN` added to both commands, or to the lane
                    // itself — and the harness returns that variable before it
                    // refuses a stale binary.
                    assert_eq!(
                        row.suite.envs,
                        [
                            ("CARGO_TARGET_DIR".into(), drivers.clone().into_os_string()),
                            ("CARGO_BUILD_JOBS".into(), "8".into()),
                        ],
                        "{what}: the lane and nothing else — no ATERM_GUI_BIN bypass"
                    );
                }
                Handed::Env(var) => {
                    let set: Vec<(String, PathBuf)> = row
                        .suite
                        .envs
                        .iter()
                        .map(|(k, v)| (k.to_string_lossy().into_owned(), PathBuf::from(v)))
                        .collect();
                    assert_eq!(set, [((*var).to_string(), row.bin.clone())], "{what}");
                }
                Handed::Argv(args) => {
                    assert_eq!(row.suite.argv()[1..], args[..], "{what}");
                    assert!(row.suite.envs.is_empty(), "{what}: {:?}", row.suite.envs);
                }
            }
            if !matches!(row.handed, Handed::Discovered) {
                assert_eq!(
                    row.suite.term_grace,
                    Some(exec::TERM_GRACE),
                    "{what}: a killed suite is sent SIGTERM first, so its EXIT trap tears down"
                );
            }
            let text = std::fs::read_to_string(repo.join(row.source)).expect(row.source);
            for needle in row.reads {
                assert!(text.contains(needle), "{}: {needle:?}", row.source);
            }
        }
        // Negative control: the check tells spellings apart — the Codex lane
        // reads no `--binary` flag.
        let codex = std::fs::read_to_string(repo.join("tools").join(CODEX_LIVE_UPGRADE_SUITE))
            .expect("the Codex lane");
        assert!(!codex.contains("--binary)"));
    }

    /// A LIVE LANE'S NOT-RUN CODE IS NEVER A PASS. Before 2026-09-26 the two
    /// lanes were run by hand or not at all, and by hand each answered the
    /// missing `<root>/target/debug/aterm` with its not-run code — the handback
    /// lane `2`, the Codex lane `77` ("SKIP") — which a gate reading "nonzero is
    /// a failure, zero a pass" would get half right and one reading "only 1 is
    /// a failure" would pass. The handback lane's `2` is COULD NOT RUN; the
    /// Codex lane's `77`, in the `--full` tier it lives in, is a NAMED SKIP with
    /// the lane's own reason (counted, and forfeiting the run's contract claim).
    /// Each code only for the lane
    /// that declares it: the other lane's code is a finding.
    #[test]
    fn a_live_lanes_not_run_code_is_never_a_pass_and_quotes_the_lanes_reason() {
        use crate::Outcome;
        let fh = FOREGROUND_HANDBACK_SUITE;
        let rd = RENDER_DESYNC_SUITE;
        let cx = CODEX_LIVE_UPGRADE_SUITE;
        assert_eq!(
            live_aterm_outcome(fh, Some(0), "39 ok, 0 FAIL, 1 skip"),
            (Outcome::Ok, fh.to_string())
        );
        assert_eq!(
            live_aterm_outcome(cx, Some(0), "PASS"),
            (Outcome::Ok, cx.to_string())
        );
        for name in LIVE_ATERM_SUITES {
            assert_eq!(
                live_aterm_outcome(name, Some(1), "").0,
                Outcome::Fail(Severity::GateFailed),
                "{name}"
            );
            assert_eq!(
                live_aterm_outcome(name, None, "").0,
                Outcome::Fail(Severity::CouldNotRun),
                "{name}: a signal decided nothing"
            );
        }
        // The Codex lane's reason is the label, so the verdict names the remedy.
        let transcript = "managed Codex 0.157.1\nSKIP: the store holds no Codex older than 0.157.1 to upgrade from\n";
        assert_eq!(
            live_aterm_outcome(cx, Some(77), transcript),
            (
                Outcome::Skip,
                "test-codex-live-upgrade.sh: NOT RUN — the store holds no Codex older than 0.157.1 \
                 to upgrade from (exit 77: this machine lacks a prerequisite; a named skip, never a \
                 pass)"
                    .to_string()
            )
        );
        let (outcome, label) = live_aterm_outcome(cx, Some(77), "");
        assert_eq!(outcome, Outcome::Skip);
        assert!(
            label.contains("the lane skipped without saying why"),
            "{label}"
        );
        let (outcome, label) = live_aterm_outcome(
            fh,
            Some(2),
            "NOT RUN: no aterm binary at /x (targo --unverified build -p aterm)\n",
        );
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
        assert_eq!(
            label,
            "test-foreground-handback.sh: no aterm binary at /x (targo --unverified build -p \
             aterm) (exit 2)"
        );
        // A boot failure is the lane's FAIL row and exit 1 — a finding.
        assert_eq!(
            live_aterm_outcome(
                fh,
                Some(1),
                "FAIL  boot: /bin/zsh — the headless instance exited before it answered\n"
            )
            .0,
            Outcome::Fail(Severity::GateFailed)
        );
        // The render desync lane's `3` is COULD NOT RUN with its reason: a
        // machine that could not land the two resizes decided nothing.
        let (outcome, label) = live_aterm_outcome(
            rd,
            Some(3),
            "---- desync\nNOT RUN: two ctl resizes never landed within the 250 ms run gap in 3 \
             attempts: desync(gaps: 412ms 390ms 505ms) — a starved machine, not a finding\n",
        );
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
        assert_eq!(
            label,
            "test-render-desync.sh: two ctl resizes never landed within the 250 ms run gap in 3 \
             attempts: desync(gaps: 412ms 390ms 505ms) — a starved machine, not a finding (exit \
             3)"
        );
        // Each lane's not-run code is its own; another lane's is a finding.
        for (name, other) in [(fh, 77), (fh, 3), (rd, 2), (rd, 77), (cx, 2), (cx, 3)] {
            assert_eq!(
                live_aterm_outcome(name, Some(other), "").0,
                Outcome::Fail(Severity::GateFailed),
                "{name} exit {other}"
            );
        }
        assert_eq!(live_aterm_not_run_code(fh), 2);
        assert_eq!(live_aterm_not_run_code(rd), 3);
        assert_eq!(live_aterm_not_run_code(cx), 77);
    }
}
