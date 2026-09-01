// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stages themselves.
//!
//! Every stage that shelled out still shells out to the SAME command with the
//! same arguments and the same environment. The argv of each one is built by a
//! small pure function below, so the port is checked by unit tests rather than by
//! a reviewer diffing two languages — including the details that are easy to lose
//! and expensive to lose: tippy's separate `CARGO_TARGET_DIR` and
//! `TRUST_NO_MIGRATE_WARN`, the doc-driver rule on the doc-running stages
//! (`DocDriver` — private to this module, so named rather than linked:
//! `RUSTDOC=<stage2>/trustdoc` when the stage2 carries it,
//! the caller's own export or the PATH farm link otherwise, a diagnosis when
//! nothing exists), `ATERM_SEARCH_REGEX_LANE=1` on the regex lane, and `--unverified` on
//! every driver invocation (naming the lane is the point: `targo` REFUSES a bare
//! verb precisely so a gate cannot be quietly unverified).

use crate::exec::{self, Capture, Cmd};
use crate::ladder::{Outcome, Report, Severity};
use crate::plan::{StageId, StageSpec};
use crate::scope::Scope;
use crate::smoke::debug_bin;
use crate::smoke_stages;
use crate::{Ctx, have_on_path, is_executable_file};

/// Dispatch one stage.
#[must_use]
pub fn run_stage(ctx: &Ctx, spec: &StageSpec) -> Report {
    let mut r = Report::new(spec.title.clone());
    match spec.id {
        StageId::Build => build(ctx, &mut r),
        StageId::Test => test(ctx, &mut r),
        StageId::Doctests => doctests(ctx, &mut r),
        StageId::RegexLane => regex_lane(ctx, &mut r),
        StageId::Tippy => tippy(ctx, &mut r),
        StageId::Formatting => formatting(ctx, &mut r),
        StageId::GrepGuards => grep_guards(ctx, &mut r),
        StageId::InstallChannel => install_channel(ctx, &mut r),
        StageId::TrustGateVerdict => trust_gate_verdict(ctx, &mut r),
        StageId::TrustContractProbe => trust_contract_probe(ctx, &mut r),
        StageId::StartCompare => start_compare(ctx, &mut r),
        StageId::LicenseHeaders => license_headers(ctx, &mut r),
        StageId::FeatureGates => feature_gates(ctx, &mut r),
        StageId::LibcOracle => libc_oracle(ctx, &mut r),
        StageId::FreezeGate => freeze_gate(ctx, &mut r),
        StageId::ProofInventory => proof_inventory(ctx, &mut r),
        StageId::ControlSocketSmoke => smoke_stages::control_socket_smoke(ctx, &mut r),
        StageId::GuiSmoke => smoke_stages::gui_typing_smoke(ctx, &mut r),
        StageId::RedrawConformance => redraw_conformance(ctx, &mut r),
        StageId::DifferentialOracle => differential_oracle(ctx, &mut r),
        StageId::KaniFloor => kani_floor(ctx, &mut r),
        StageId::CrossCells => cross_cells(ctx, &mut r),
    }
    r
}

// ---------------------------------------------------------------------------
// The argv builders. Pure, so the port is a test and not a promise.
// ---------------------------------------------------------------------------

/// `targo --unverified build <scope>`
#[must_use]
pub fn build_args(scope: &Scope) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "build".to_string()];
    a.extend(scope.args());
    a
}

/// `targo --unverified test <scope>`
#[must_use]
pub fn test_args(scope: &Scope) -> Vec<String> {
    let mut a = vec!["--unverified".to_string(), "test".to_string()];
    a.extend(scope.args());
    a
}

/// `targo --unverified test --doc <scope>` — run explicitly, because the unit
/// stage's `targo test` can skip documentation examples when scoped or under a
/// nextest-style runner, and doctests then rot silently.
#[must_use]
pub fn doctest_args(scope: &Scope) -> Vec<String> {
    let mut a = vec![
        "--unverified".to_string(),
        "test".to_string(),
        "--doc".to_string(),
    ];
    a.extend(scope.args());
    a
}

/// `targo --unverified test -p aterm-search --features regex`
#[must_use]
pub fn regex_lane_args() -> Vec<String> {
    [
        "--unverified",
        "test",
        "-p",
        "aterm-search",
        "--features",
        "regex",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `<tippy> <scope> --all-targets --keep-going -- -D warnings`
///
/// `--keep-going` IS THE COVERAGE FLAG, not a tuning knob. Without it cargo
/// stops scheduling new units the moment one fails, so under `-D warnings` the
/// FIRST crate with a finding ends the run and every crate cargo had not
/// reached yet goes unlinted — the lint reports a PREFIX of the workspace while
/// reading like a statement about all of it. MEASURED on this tree 2026-08-26:
/// the aborting form reported 3 findings in `atpkg`; the same tree under
/// `--keep-going` reported 9, in `atpkg`, `aterm-conformance` and `aterm-gui`.
/// That is also the mechanism behind the three "fixed it — no, here is another
/// one" rounds of 2026-08-11 (see `crates/xtask/src/gate.rs`): each round
/// fixed the crate that happened to abort first and revealed the next.
///
/// It does NOT make coverage unconditional: a member whose DEPENDENCY fails to
/// compile still cannot be linted, because there is no metadata to lint it
/// against. `gate lint` says so out loud rather than implying otherwise.
#[must_use]
pub fn tippy_args(scope: &Scope) -> Vec<String> {
    let mut a = scope.args();
    a.extend(
        ["--all-targets", "--keep-going", "--", "-D", "warnings"]
            .into_iter()
            .map(String::from),
    );
    a
}

/// The workspace's `required-features` targets, as the `(package, feature)`
/// pairs that switch them on.
///
/// SIX TARGETS HANG OFF THESE THREE PAIRS, and `--all-targets` builds none of
/// them: cargo skips a target whose `required-features` are off, silently and
/// without a word in its output. So the main tippy pass — `--workspace
/// --all-targets` — never compiled `aterm-gui`'s three `bench-support`
/// benches, its `control-conformance` bin, or `aterm-scrollback`'s two
/// `disk-tier` benches. That is not a theoretical hole: a broken bench build
/// lived in it for four days in August 2026, and the campaign's count gates
/// and reach guards LIVE in those benches, so an unbuilt bench is a gate that
/// silently stopped existing.
///
/// The table is checked against the tree by
/// `xtask`'s `the_gated_feature_table_matches_every_required_features_target`,
/// so a seventh gated target cannot be added without either extending this or
/// reddening that test.
pub const GATED_LINT_FEATURES: [(&str, &str); 3] = [
    ("aterm-gui", "bench-support"),
    ("aterm-gui", "control-conformance"),
    ("aterm-scrollback", "disk-tier"),
];

/// `<tippy> -p <pkg>… --features <pkg/feat>,… --all-targets --keep-going --
///  -D warnings` — the SECOND pass, the one that reaches the targets
/// [`tippy_args`] cannot.
///
/// A separate invocation rather than a flag on the first, because the features
/// belong to specific packages: turning them on for the whole workspace is not
/// something cargo offers, and `-p <pkg> --features <pkg>/<feat>` is. Its cost
/// is one re-lint of the two named packages against a wider feature set; every
/// dependency below them is a cache hit from the first pass.
///
/// `None` when the scope selects neither package — under `--scope aterm-core`
/// there is no gated target to reach, and running the pass anyway would compile
/// two crates the run had deliberately narrowed away.
#[must_use]
pub fn tippy_gated_args(scope: &Scope) -> Option<Vec<String>> {
    let mut packages: Vec<&str> = Vec::new();
    let mut features: Vec<String> = Vec::new();
    for (pkg, feat) in GATED_LINT_FEATURES {
        if !scope.includes_crate(pkg) {
            continue;
        }
        if !packages.contains(&pkg) {
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
    a.extend(
        ["--all-targets", "--keep-going", "--", "-D", "warnings"]
            .into_iter()
            .map(String::from),
    );
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

/// The crates carrying a Kani BMC floor, in the order the stage runs them.
pub const KANI_CRATES: [&str; 3] = ["aterm-parser", "aterm-render", "aterm-uds"];

/// The exact command one Kani floor run spawns. `KANI_CRATE` selects which
/// crate's proofs the script drives; lose it and every iteration of the loop
/// runs the same default, so two of the three crates go unproven while the
/// ladder still prints three green rows. `TRUST_MC_SYSROOT` / `AY_BIN_DIR`
/// carry what THIS process resolved ([`trust_mc_sysroot`], [`ay_bin_dir`]) so
/// the script and the availability decision above it can never disagree
/// about which trust-mc is being driven.
fn kani_cmd(
    gate: &std::path::Path,
    krate: &str,
    mc_root: &std::path::Path,
    ay_dir: &std::path::Path,
) -> Cmd {
    Cmd::new(gate)
        .env("KANI_CRATE", krate)
        .env("TRUST_MC_SYSROOT", mc_root)
        .env("AY_BIN_DIR", ay_dir)
        .capture(Capture::Emit)
}

/// Where trust-mc lives — the same order `scripts/verify-kani-proofs.sh` uses:
///
/// 1. `$TRUST_MC_SYSROOT` — an explicit location, never fallen back from.
/// 2. `<atpkg prefix>/store/trust-mc/current` — what `aterm pkg install
///    trust-mc` lays down, and the only sysroot most machines have. Taken when
///    it carries the driver (`bin/trust-mc-driver`); the managed bundle ships
///    no `cargo-trust-mc` name — the script derives that symlink OUTSIDE the
///    store, which is tree_root-attested and immutable.
/// 3. `$HOME/trust/first-party/trust-mc/target/trust-mc` — a from-source dev build.
#[must_use]
pub fn trust_mc_sysroot(env: &crate::EnvSnapshot) -> std::path::PathBuf {
    if let Some(explicit) = &env.trust_mc_sysroot {
        return explicit.clone();
    }
    let store = crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref())
        .join("store/trust-mc/current");
    if is_executable_file(&store.join("bin/trust-mc-driver"))
        || is_executable_file(&store.join("bin/cargo-trust-mc"))
    {
        return store;
    }
    env.home.join("trust/first-party/trust-mc/target/trust-mc")
}

/// Where the `ay` solver lives, in the script's order: `$AY_BIN_DIR`, else the
/// atpkg shim dir (`<prefix>/bin`, where `aterm pkg install ay` shims it), else
/// the from-source dev build `$HOME/trust/first-party/ay/target/release`.
#[must_use]
pub fn ay_bin_dir(env: &crate::EnvSnapshot) -> std::path::PathBuf {
    if let Some(explicit) = &env.ay_bin_dir {
        return explicit.clone();
    }
    let shims =
        crate::toolchain::atpkg_prefix(&env.home, env.xdg_config_home.as_deref()).join("bin");
    if is_executable_file(&shims.join("ay")) {
        return shims;
    }
    env.home.join("trust/first-party/ay/target/release")
}

/// The redraw harness's target name — the `[[bin]]`, the built file and the
/// argv below all have to agree, so they read it from here.
pub const REDRAW_CONFORMANCE_BIN: &str = "aterm-redraw-conformance";

/// `targo --unverified build -q -p aterm-gui --features control-conformance
///  --bin aterm-redraw-conformance`
///
/// The feature is the whole reason the argv is spelled out and tested: it is
/// `required-features` on the target, so without it cargo silently builds
/// NOTHING and the stage would gate on a binary from some previous run.
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

/// `targo --unverified test -p aterm-bench --test differential`
#[must_use]
pub fn differential_args() -> Vec<String> {
    [
        "--unverified",
        "test",
        "-p",
        "aterm-bench",
        "--test",
        "differential",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The nested workspace's checked-in driver is the oracle contract. Keeping
/// this as one argv value (with no cargo fallback or reimplementation here)
/// means changes to its target-cell matrix automatically reach the required
/// merge gate. The target dir is absolute because `run.sh` deliberately runs
/// cross-cell Cargo commands from `/`; inheriting a relative caller value would
/// turn that into `/target`, while inheriting an arbitrary absolute one would
/// collapse this scheduler lane onto somebody else's Cargo lock.
///
/// # Errors
/// Returns the current-directory error from making a relative repository root
/// absolute. The stage reports that as COULD-NOT-RUN.
pub fn libc_oracle_cmd(ctx: &Ctx) -> std::io::Result<Cmd> {
    let root = std::path::absolute(&ctx.root)?;
    Ok(Cmd::new(root.join("libc-oracle/run.sh"))
        .env("CARGO_TARGET_DIR", root.join("libc-oracle/target"))
        // Python imports inside gen/ are attributed source unless bytecode is
        // suppressed. Set it here as a required-stage contract as well as in
        // run.sh, so a gate invocation cannot dirty its own fingerprint.
        .env("PYTHONDONTWRITEBYTECODE", "1"))
}

/// `libc-oracle/run.sh` exit contract: 0 proves conformance, 1 is a finding,
/// and 3 means a preflight/environment inability decided nothing. A missing
/// status likewise decided nothing; every other status violates the driver's
/// declared contract and remains a gate finding.
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

/// The script's `run()`: under `--selftest` say so and execute nothing;
/// otherwise run, print what the child said, and decide.
fn run_labeled(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) {
    if ctx.selftest {
        r.skip(format!("{label} (selftest: not executed)"));
        return;
    }
    let out = exec::run(cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    r.decide(out.ok, label);
}

/// The script's `run_scoped()`: `run_labeled` for the stages that COMPILE the
/// selected crates.
///
/// An empty selection must not fall through to a bare `targo build` — with no
/// `-p` that builds the whole default workspace, which would be a full build
/// wearing a narrow run's label. Only `--changed` can select nothing (a
/// docs-only branch), and it is an honest skip, counted and named like any
/// other, so the verdict says the run compiled nothing.
fn run_scoped(ctx: &Ctx, r: &mut Report, label: &str, cmd: &Cmd) {
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    run_labeled(ctx, r, label, cmd);
}

/// A `targo` invocation, always naming its lane.
fn targo(ctx: &Ctx, args: Vec<String>) -> Cmd {
    Cmd::new(&ctx.tools.targo).args(args)
}

/// Bind Trust's renamed documentation driver for the stages that run doctests.
fn with_trustdoc(ctx: &Ctx, cmd: Cmd) -> Cmd {
    cmd.env("RUSTDOC", ctx.tools.trustdoc.as_os_str())
}

/// Which doc driver a doc-running stage gets. `Stage2` is the pinned driver,
/// bound through `RUSTDOC` (a `Command::env` that overrides anything the
/// caller exported — the gate pins its own driver whenever it has one).
/// `Ambient` is a caller-exported `RUSTDOC`/`CARGO_BUILD_RUSTDOC`: cargo
/// prefers either over the config's `[build] rustdoc`, so the child runs under
/// the operator's own binding and the ladder says so. `BarePath` leaves
/// cargo's discovery to resolve the config's bare `trustdoc` from the
/// children's PATH (the `~/.local/bin` farm link) — fail-closed, real doctest
/// verdicts. `Absent` means a doctest-compiling run would die at exec with a
/// raw OS error naming no remedy, so the stage diagnoses instead — unless the
/// run compiles no lib target (rustdoc is never spawned) or `--selftest`
/// executes nothing anyway; the stage arms below hold those qualifiers.
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
/// only for lib targets, so a scope with none runs green without any doc
/// driver — the Absent diagnosis must not fire there. `--changed` answers from
/// its resolved `Members` table; `--scope <crate>` never resolved the graph,
/// so it asks the member's own manifest (`crates/<name>` layout, both halves
/// of cargo's rule); the workspace always holds libs. Unanswerable answers
/// YES, the same fail-closed direction as `Members::any_has_lib` — here the
/// diagnosis rather than a bare run that would die raw if the answer was
/// really yes.
fn scope_compiles_doctests(ctx: &Ctx) -> bool {
    match &ctx.scope {
        Scope::Crate(c) => crate::changed::crate_dir_has_lib(&ctx.root, c),
        s => s.has_lib_target(),
    }
}

/// The rule as a truth table — pure, so it is a test and not a promise. The
/// stage2's own copy outranks everything: the gate runs THE toolchain's
/// drivers, and its `RUSTDOC` binding overrides even a caller's export. The
/// caller's export outranks the bare PATH walk because cargo gives it exactly
/// that precedence over the config key. The farm link is the fallback for
/// machines whose stage2 predates trustdoc, never a preference.
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
// 1) BUILD
// ---------------------------------------------------------------------------
fn build(ctx: &Ctx, r: &mut Report) {
    if ctx.tools.have_targo() {
        run_scoped(
            ctx,
            r,
            &format!("targo build {}", ctx.scope.label()),
            &targo(ctx, build_args(&ctx.scope)),
        );
    } else {
        // Fail-closed, and COULD-NOT-RUN rather than FAILED: nothing about the
        // tree was decided. Never a stock-cargo fallback — that would make the
        // gate quietly unverified, which is what the two-lane driver prevents.
        r.cannot_run(ctx.tools.missing_targo_label());
    }
}

// ---------------------------------------------------------------------------
// 2) TEST — `targo test` runs crate doctests after the unit/integration targets,
//    so trustdoc is bound here as well as in the explicit doc-only stage — and a
//    machine with NO doc driver anywhere is diagnosed here (COULD-NOT-RUN with
//    the remedy), not left to die at exec mid-stage after the unit tests passed.
// ---------------------------------------------------------------------------
fn test(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test (no targo)");
        return;
    }
    let label = format!("targo test {}", ctx.scope.label());
    // The empty change-selection outranks the doc-driver verdict: a run that
    // compiles NOTHING needs no doc driver, and must keep saying why it ran
    // nothing rather than blaming a tool it never needed.
    if ctx.scope.selects_nothing() {
        r.skip(format!("{label} (change-scoped run selected no crates)"));
        return;
    }
    let cmd = targo(ctx, test_args(&ctx.scope));
    match doc_driver(ctx) {
        DocDriver::Stage2 => run_labeled(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        ),
        DocDriver::Ambient => {
            run_labeled(ctx, r, &format!("{label} (caller's RUSTDOC)"), &cmd);
        }
        DocDriver::BarePath => run_labeled(ctx, r, &label, &cmd),
        // Diagnose only a run that would really spawn rustdoc: `--selftest`
        // executes nothing (run_labeled prints its skip), and a scope that
        // compiles no lib target compiles no doctests, so the child runs
        // green without a doc driver — declaring the machine broken there
        // would blame a tool the run never needed.
        DocDriver::Absent if ctx.selftest || !scope_compiles_doctests(ctx) => {
            run_labeled(ctx, r, &label, &cmd);
        }
        DocDriver::Absent => r.cannot_run(ctx.tools.missing_trustdoc_label()),
    }
}

// ---------------------------------------------------------------------------
// 2.5) DOCTESTS
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
    // `cargo test --doc -p …` is a hard ERROR — "no library targets found in
    // package `X`" — when NONE of the selected packages has a lib, and an
    // all-binary selection is an ordinary outcome under `--changed` (`xtask` is
    // bin-only, so a branch that edits only crates/xtask selects exactly
    // {xtask}) and under `--scope xtask` alike: this stage would go RED on a
    // completely healthy tree. A tier that cries wolf is worse than no tier,
    // so ask first — `scope_compiles_doctests`, the same answer the test
    // stage's diagnosis consults — and skip honestly: a skip is counted, and
    // the verdict narrows accordingly.
    if !scope_compiles_doctests(ctx) {
        r.skip(format!(
            "targo test --doc (no package in scope has a library target: {})",
            ctx.scope.label()
        ));
        return;
    }
    let cmd = targo(ctx, doctest_args(&ctx.scope));
    match doc_driver(ctx) {
        DocDriver::Stage2 => run_labeled(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        ),
        DocDriver::Ambient => {
            run_labeled(ctx, r, &format!("{label} (caller's RUSTDOC)"), &cmd);
        }
        DocDriver::BarePath => run_labeled(ctx, r, &label, &cmd),
        // Selftest executes nothing — run_labeled prints its uniform skip.
        DocDriver::Absent if ctx.selftest => run_labeled(ctx, r, &label, &cmd),
        // The test stage directly above already declared the COULD-NOT-RUN with
        // the full remedy — same pattern as the no-targo ladder, where build
        // declares once and the later stages skip pointing at it. (Whenever
        // this stage survives its lib-target guard, that scope made the test
        // stage's Absent arm a real cannot_run, so the pointer never dangles.)
        DocDriver::Absent => r.skip("targo test --doc (no doc driver — see the test line)"),
    }
}

// ---------------------------------------------------------------------------
// 2.6) REGEX SEARCH LANE. `aterm-search`'s regex-mode oracle battery is gated on
//    `feature = "regex"`, which the default test stage does NOT enable — without
//    this stage the whole battery compiles out to ZERO cases and the suite stays
//    green with no regex coverage. The `ATERM_SEARCH_REGEX_LANE` marker arms the
//    always-compiled `regex_lane_tripwire`, which hard-fails if the marker is set
//    but the feature was dropped, so the lane cannot silently lose its coverage.
// ---------------------------------------------------------------------------
/// The exact command the regex lane spawns — extracted so a test asserts on it
/// rather than on a replica. The marker is the whole point of the stage: without
/// `ATERM_SEARCH_REGEX_LANE` the suite still passes, green with no regex
/// coverage at all.
fn regex_lane_cmd(ctx: &Ctx) -> Cmd {
    targo(ctx, regex_lane_args()).env("ATERM_SEARCH_REGEX_LANE", "1")
}

fn regex_lane(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("regex search lane (no targo)");
        return;
    }
    let label = "targo test -p aterm-search --features regex";
    let cmd = regex_lane_cmd(ctx);
    // This lane compiles aterm-search's doctests too, so it takes the same
    // doc-driver rule as the test/doctest stages — left on the old two-way
    // binding it would be the one stage still dying raw at rustdoc exec on a
    // machine with no doc driver, and worse, dying as a GateFailed that
    // outranks the test stage's honest COULD-NOT-RUN in the verdict.
    match doc_driver(ctx) {
        DocDriver::Stage2 => run_labeled(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        ),
        DocDriver::Ambient => {
            run_labeled(ctx, r, &format!("{label} (caller's RUSTDOC)"), &cmd);
        }
        DocDriver::BarePath => run_labeled(ctx, r, label, &cmd),
        DocDriver::Absent if ctx.selftest => run_labeled(ctx, r, label, &cmd),
        // Planned only when aterm-search (a lib crate) is in scope, so the test
        // stage's Absent arm was a real cannot_run — the pointer never dangles.
        DocDriver::Absent => r.skip(format!("{label} (no doc driver — see the test line)")),
    }
}

// ---------------------------------------------------------------------------
// 2.7) TIPPY — the LINT gate runs under the TRUST toolchain's Clippy fork, built
//    from the Trust rustc fork, never stock clippy. Auto-detected and
//    fail-closed: present means `-D warnings` MUST pass; absent is an honest skip.
//    A SEPARATE target dir keeps the trust-toolchain artifacts off the stock build.
// ---------------------------------------------------------------------------
/// The exact command the lint stage spawns.
///
/// Extracted so the test can assert on THIS, not on a replica it built itself.
/// A test that re-spells `.env(..)` and then checks its own spelling passes even
/// when the stage stops setting the variable — and `CARGO_TARGET_DIR` is the one
/// that must not silently go missing: without it the Trust-toolchain lint
/// artifacts land in the stock build's target dir and the two lanes clobber
/// each other's caches.
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
}

fn tippy(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("tippy lint (selftest: not executed)");
        return;
    }
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
    // THE SECOND PASS IS NOT OPTIONAL POLISH. `--all-targets` above built no
    // target whose `required-features` are off, so without this the six in
    // [`GATED_LINT_FEATURES`] are linted by nobody — which is how a broken
    // bench build survived four days. Its own row, so the ladder shows whether
    // it ran.
    if let Some(args) = tippy_gated_args(&ctx.scope) {
        run_scoped(
            ctx,
            r,
            "tippy required-features targets -D warnings",
            &tippy_cmd(ctx, &bin, args),
        );
    }
}

// ---------------------------------------------------------------------------
// 3) GREP GUARDS (zero-tolerance, always whole-tree)
// ---------------------------------------------------------------------------
/// FORMATTING — `xtask gate lint --fmt-only`, i.e. the formatter lane's BOTH
/// passes and no other lane.
///
/// This stage did not exist until 2026-08-31, and the gap was declared rather
/// than hidden: `verify.sh` ran a tippy stage and no fmt stage, and said so.
/// Declaring a limit is not covering it. `.githooks/pre-push` has been advisory
/// since 2026-08-24, so between those two facts NOTHING in this repository ran
/// the formatter unless a human chose to — and the MEASURED consequence was
/// three consecutive rebases of `main` arriving with drift (5 files, 2, 1), one
/// of them in `aterm-link`, a crate outside `members = ["crates/*"]` that
/// `targo-fmt --all` structurally cannot see.
///
/// It is cheap enough to be uncontroversial: the check needs no compiler —
/// trustfmt parses and prints, it does not build — and cost 7.5 s over 1,761
/// tracked files on two measured runs. The `MainTarget` lane is for the xtask
/// binary this shells through, not for the check.
///
/// NO SKIP WITHOUT A TOOLCHAIN, and that asymmetry is deliberate: the lane
/// itself already distinguishes a formatter that found drift (FINDING) from one
/// that could not run (NOT RUN), and both block there. Adding a second opinion
/// here would let a stage-level skip hide a lane-level not-run — the exact
/// confusion the fmt lane spent a month in. The only skip is the one every
/// xtask stage shares: no targo, so the verb cannot be built at all.
fn formatting(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("formatting (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "gate lint --fmt-only",
        &targo(ctx, xtask_gate_args_with("lint", &["--fmt-only"])),
    );
}

fn grep_guards(ctx: &Ctx, r: &mut Report) {
    let g = ctx.tools_dir().join("grep_guard.sh");
    if !is_executable_file(&g) {
        r.cannot_run(format!(
            "grep_guard.sh missing or not executable ({})",
            g.display()
        ));
        return;
    }
    if ctx.selftest {
        r.skip("grep_guard.sh (selftest)");
        return;
    }
    let out = exec::run(&script_cmd(&g, &ctx.root), ctx.exec_env());
    r.raw(out.output.as_str());
    r.decide(out.ok, "grep_guard.sh");
}

// ---------------------------------------------------------------------------
// 3.5) BOOTSTRAP UPDATE CHANNEL — keep tools/install.sh aligned with the in-app
//    updater's complete-catalog numeric arbitration and exact asset identity.
// ---------------------------------------------------------------------------
fn install_channel(ctx: &Ctx, r: &mut Report) {
    let t = ctx.tools_dir().join("test-install-channel.sh");
    if is_executable_file(&t) {
        run_labeled(ctx, r, "test-install-channel.sh", &Cmd::new(&t));
    } else {
        r.cannot_run(format!(
            "test-install-channel.sh missing or not executable ({})",
            t.display()
        ));
    }
}

// ---------------------------------------------------------------------------
// 3.55) TRUST-GATE VERDICT SELF-TEST — tools/trust-gate-all.sh prints the
//    sentence that IS the campaign claim ("100% MACHINE-PROVED (workspace +
//    every vendored fork, …)") and has several inputs that shrink the run.
//    Until its self-test existed the verdict logic had never been exercised
//    against a narrowed run at all, and it printed the workspace sentence for
//    runs that were not the workspace. It now also covers the gate LIST: that
//    members are addressed `-p name@version` and forks by manifest path, and
//    that a fork can neither appear in the resolved graph undeclared nor vanish
//    from it while the roster still lists it. Hard-required, exactly like
//    test-install-channel.sh: a missing self-test is not a skip, because the
//    thing it guards is a claim.
// ---------------------------------------------------------------------------
fn trust_gate_verdict(ctx: &Ctx, r: &mut Report) {
    let t = ctx.tools_dir().join("test-trust-gate-verdict.sh");
    if is_executable_file(&t) {
        run_labeled(ctx, r, "test-trust-gate-verdict.sh", &Cmd::new(&t));
    } else {
        r.cannot_run(format!(
            "test-trust-gate-verdict.sh missing or not executable ({})",
            t.display()
        ));
    }
}

// ---------------------------------------------------------------------------
// 3.56) TRUST CONTRACT PROBE — two facts about the INSTALLED stage2 that no
//    cargo stage exercises while the workspace opt-out is on: the off-switch
//    must compile a contract without ICEing (2026-07-30: the first committed
//    `ensures` crashed trustc under the then-current off-switch, so contracts
//    could not land at all), and a `self`-field postcondition must PROVE (same
//    date: every such predicate was UNPARSEABLE → fail-closed Unknown — the
//    exact form every lifecycle property in aterm takes). Both held on stage2
//    51bf8a270 (2026-08-05); tools/trust-probes/self_field_ensures.rs is the
//    probe. Skips without the toolchain like every stage2 stage; a MISSING
//    script is cannot-run, because a probe that vanishes is how the ICE class
//    returns unheralded.
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
// 3.6) STARTUP COMPARISON SCHEDULER — the publishable-startup evidence path must
//    fail closed on malformed samples, mutable harness bytes, uncertain thermal
//    state, identical-artifact controls, and timed-out process descendants.
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
// 4) LICENSE / SPDX HEADERS (every .rs carries the two-line header)
// ---------------------------------------------------------------------------
fn license_headers(ctx: &Ctx, r: &mut Report) {
    let lic = ctx.tools_dir().join("license_check.sh");
    if !is_executable_file(&lic) {
        r.cannot_run(format!(
            "license_check.sh missing or not executable ({})",
            lic.display()
        ));
        return;
    }
    if ctx.selftest {
        r.skip("license_check.sh (selftest)");
        return;
    }
    let out = exec::run(&script_cmd(&lic, &ctx.root), ctx.exec_env());
    r.raw(out.output.as_str());
    r.decide(out.ok, "license_check.sh");
}

// ---------------------------------------------------------------------------
// 4.5) FEATURE GATES (advertise-vs-implement + dormant-feature detection), plus
//    the main-loop census: the ONLY enforced net for the multi-line bound-guard
//    form `let g = term_lock(..); g.resize(..)` that the single-line grep
//    tripwire cannot see.
// ---------------------------------------------------------------------------
fn feature_gates(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("feature gates (no targo)");
        return;
    }
    for gate in ["drift", "dormant", "mainloop"] {
        run_labeled(
            ctx,
            r,
            &format!("gate {gate}"),
            &targo(ctx, xtask_gate_args(gate)),
        );
    }
}

// ---------------------------------------------------------------------------
// 4.5a) FIRST-PARTY LIBC ABI ORACLE. The const/layout/type assertions compile
//    for every target cell, the emitted-symbol gate closes link-name aliases,
//    and `cargo test` executes the pointer-valued and C-macro checks for the
//    host's native cell. Therefore a native Linux run is the required Linux
//    runtime route; cross-compiling that cell alone is deliberately not enough.
//
//    Missing/non-executable driver and exit 3 are COULD-NOT-RUN, never a skip;
//    exit 1 is a conformance finding. There is no stock-workspace fallback:
//    this separate workspace is what prevents `[patch.crates-io]` from
//    rewriting the reference libc edge to the shim under test.
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
    let label = "libc-oracle/run.sh (cross-cell ABI + native runtime)";
    if ctx.selftest {
        r.skip(format!("{label} (selftest: not executed)"));
        return;
    }
    let out = exec::run(&cmd, ctx.exec_env());
    r.raw(out.output.as_str());
    r.record(libc_oracle_outcome(out.code), label);
}

// ---------------------------------------------------------------------------
// 4.5b) L0 TEMPORAL-SAFETY GATE — ONE build of tools/freeze-safety-gate enforces
//    six obligations (temporal proof, main-loop census, lock-order census,
//    wasm-process census, scope-cardinality census, lazy-init reentrancy
//    census), any one FAILING the build with a counterexample-backed
//    diagnostic.
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
        &targo(ctx, freeze_gate_args()),
    );
}

// ---------------------------------------------------------------------------
// 4.6) COMPUTED-ONLY PROOF INVENTORY — count the proof attributes, fail on scan
//    errors or an empty inventory, and reject a hand-maintained README total.
// ---------------------------------------------------------------------------
fn proof_inventory(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("gate counts (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "gate counts",
        &targo(ctx, xtask_gate_args("counts")),
    );
}

// ---------------------------------------------------------------------------
// 5c) CONTROL REDRAW CONFORMANCE — the one check that can see a control-socket
//    `select` actually repaint a window. `EventLoop` construction panics off the
//    process main thread and libtest runs every `#[test]` on a spawned one, so
//    every unit test in aterm-gui builds its host with `proxy: None`: a
//    regression that drops the production `EventLoopProxy` compiles and passes
//    the whole default suite. Only a target owning `fn main` can hold a real
//    proxy, which is what `aterm-redraw-conformance` is — and it is behind
//    `required-features`, so this stage is the only thing that ever builds it.
// ---------------------------------------------------------------------------

/// How the ladder reads one harness exit code (`0` pass / `1` fail / `2` NOT RUN,
/// declared in `aterm_gui::control_redraw_conformance`).
///
/// A FUNCTION, and tested, because of `2`. The harness answers it when no event
/// loop is constructible here — headless, no display — and a `2` read as green
/// would restore, one layer up, exactly the false pass this gate exists to
/// remove. It is COULD-NOT-RUN: the run decided nothing, which is neither a pass
/// nor a finding about the tree. Any OTHER code is a finding — the harness drives
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
            "aterm-redraw-conformance: NOT RUN — no event loop is constructible here, so nothing was proven about redraws (exit 2, never a pass)".to_string(),
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
    if ctx.selftest {
        r.skip("redraw conformance (selftest: not executed)");
        return;
    }
    if !ctx.tools.have_targo() {
        // The build stage already reported COULD-NOT-RUN for the same absence;
        // naming it again here keeps the skip counted and the verdict narrowed.
        r.skip("redraw conformance (no targo)");
        return;
    }
    let build = exec::run(&targo(ctx, redraw_conformance_build_args()), ctx.exec_env());
    if !build.ok {
        r.raw(build.output.as_str());
        r.fail(format!("targo build --bin {REDRAW_CONFORMANCE_BIN}"));
        return;
    }
    let bin = debug_bin(
        &ctx.root,
        ctx.env.cargo_target_dir.as_deref(),
        REDRAW_CONFORMANCE_BIN,
    );
    if !is_executable_file(&bin) {
        r.cannot_run(format!(
            "redraw conformance: just-built harness missing ({})",
            bin.display()
        ));
        return;
    }
    // Driven as a BINARY, never `targo run`: the driver's lane banner goes to
    // stderr and cargo's own codes would collide with the harness's 0/1/2.
    let out = exec::run(&Cmd::new(&bin), ctx.exec_env());
    r.raw(out.output.as_str());
    let (outcome, label) = redraw_outcome(out.code);
    r.record(outcome, label);
}

// ---------------------------------------------------------------------------
// 6) --full ONLY: differential oracle
// ---------------------------------------------------------------------------
fn differential_oracle(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("differential oracle (selftest)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("differential oracle (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "targo test -p aterm-bench --test differential",
        &targo(ctx, differential_args()),
    );
}

// ---------------------------------------------------------------------------
// 6b) --full ONLY: trust-mc / Kani BMC floor.
//
//    The real driver is `cargo trust-mc --config-free --harness <name>` via
//    scripts/verify-kani-proofs.sh. There is no `trust-mc verify` subcommand, and
//    stock cargo-kani/CBMC is banned — verification is discharged by trust-mc + ay.
//    An unavailable toolchain is reported PROMINENTLY and skipped, exactly as the
//    --full contract promises; it is never described as discharged.
// ---------------------------------------------------------------------------
/// `--full` only: every forge cell type-checked FOR ITS OWN TRIPLE.
///
/// The rest of this gate compiles aterm for one target. aterm ships five, and
/// until 2026-09-01 the other four were held by source reading — which is where
/// both defects the `once_cell` judge found had been living. `xtask gate cells`
/// runs a real compiler per triple, on a toolchain that carries that std, from
/// a cwd and into a target directory OUTSIDE this repo. A cell whose toolchain
/// is not installed SKIPS inside the verb and says out loud that nothing was
/// compiled for it; a cell that runs and fails is a FAILURE, never re-read as a
/// skip.
///
/// AND IT READS THIS REPO'S OWN CODE ON ALL FIVE. For the first day of its life
/// the verb was GREEN on linux and win while neither cell had type-checked one
/// line of aterm's first-party crates: `ring` and `zstd-sys` bundle C, their
/// build scripts could not run for those triples, and the excuse for that took
/// eighteen crates with it. They are SHIMMED now (`tools/cross-cell-gate.tsv`,
/// `cshim` rows), and each cell FAILS if any in-repo package in its graph goes
/// unread — an obligation with no escape hatch in the policy file.
fn cross_cells(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("cross-cell type-check (selftest)");
        return;
    }
    if !ctx.tools.have_targo() {
        r.skip("cross-cell type-check (no targo)");
        return;
    }
    run_labeled(ctx, r, "gate cells", &targo(ctx, xtask_gate_args("cells")));
}

fn kani_floor(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("trust-mc (selftest)");
        return;
    }
    let gate = ctx.root.join("scripts/verify-kani-proofs.sh");
    let mc_root = trust_mc_sysroot(&ctx.env);
    let ay_dir = ay_bin_dir(&ctx.env);

    if !is_executable_file(&gate) {
        r.cannot_run(format!(
            "verify-kani-proofs.sh missing or not executable ({})",
            gate.display()
        ));
        return;
    }
    if !is_executable_file(&mc_root.join("bin/cargo-trust-mc"))
        && !is_executable_file(&mc_root.join("bin/trust-mc-driver"))
    {
        r.raw("  NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN: trust-mc is unavailable");
        r.raw(format!(
            "          at {} (the embedded + ty tiers still ran).",
            mc_root.display()
        ));
        r.raw("          fix: `aterm pkg install trust-mc` (`aterm pkg doctor` names the store);");
        r.raw("          or set TRUST_MC_SYSROOT at a from-source build-trust-mc sysroot.");
        r.skip("trust-mc / Kani BMC floor (tool unavailable; `aterm pkg install trust-mc`)");
        return;
    }
    if !is_executable_file(&ay_dir.join("ay")) && !have_on_path("ay", &ctx.path_env) {
        r.raw("  NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN: ay is unavailable");
        r.raw(format!(
            "          at {} and on PATH (the embedded + ty tiers still ran).",
            ay_dir.display()
        ));
        r.raw(
            "          fix: `aterm pkg install ay`; or set AY_BIN_DIR at a from-source ay build.",
        );
        r.skip("trust-mc / Kani BMC floor (solver unavailable; `aterm pkg install ay`)");
        return;
    }
    for krate in KANI_CRATES {
        run_labeled(
            ctx,
            r,
            &format!("verify-kani-proofs.sh ({krate})"),
            &kani_cmd(&gate, krate, &mc_root, &ay_dir),
        );
    }
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
            false,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[test]
    fn every_driver_invocation_names_its_lane() {
        // `targo` REFUSES a bare verb on purpose: an artifact is either verified
        // or explicitly unverified, never implicitly one of them. A ported
        // command that dropped `--unverified` would not run at all — but one that
        // grew a bare `cargo` fallback would silently make the gate meaningless.
        let s = Scope::workspace();
        for argv in [
            build_args(&s),
            test_args(&s),
            doctest_args(&s),
            regex_lane_args(),
            xtask_gate_args("drift"),
            freeze_gate_args(),
            differential_args(),
            redraw_conformance_build_args(),
        ] {
            assert_eq!(
                argv.first().map(String::as_str),
                Some("--unverified"),
                "{argv:?}"
            );
        }
    }

    #[test]
    fn libc_oracle_owns_an_absolute_target_dir_and_suppresses_python_bytecode() {
        for caller_target in ["caller-relative", "/caller/absolute-target"] {
            let mut c = ctx(Scope::workspace());
            c.env.cargo_target_dir = Some(caller_target.into());
            let cmd = libc_oracle_cmd(&c).expect("the absolute fixture root resolves");
            assert_eq!(cmd.program, PathBuf::from("/repo/libc-oracle/run.sh"));
            assert_eq!(
                cmd.envs,
                [
                    ("CARGO_TARGET_DIR".into(), "/repo/libc-oracle/target".into()),
                    ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
                ],
                "the required lane must ignore caller CARGO_TARGET_DIR={caller_target}"
            );
        }

        let mut relative = ctx(Scope::workspace());
        relative.root = PathBuf::from("relative-repo");
        let cmd = libc_oracle_cmd(&relative).expect("a relative repo root can be absolutized");
        let target = &cmd.envs[0].1;
        assert!(
            PathBuf::from(target).is_absolute(),
            "run.sh executes cross commands from /, so this cannot be relative: {target:?}"
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
    fn a_machine_with_no_doc_driver_anywhere_is_diagnosed_not_left_to_die_at_exec() {
        // targo exists, trustdoc does not, and nothing on the children's PATH
        // answers to the bare name: the test stage must say COULD-NOT-RUN with
        // the remedy BEFORE spawning anything (the old behavior ran the child
        // and let cargo die at exec mid-stage, after the unit tests passed),
        // and the doctest stage points at that line rather than re-declaring.
        let mut cc = ctx(Scope::workspace());
        cc.tools.targo = PathBuf::from("/bin/sh");
        cc.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        cc.path_env = "/nonexistent-dir".into();
        let spec = |id| StageSpec {
            id,
            title: "t".into(),
            lane: crate::plan::Lane::MainTarget,
            exclusive: false,
        };

        let r = run_stage(&cc, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "test decided once");
        assert_eq!(outcomes[0].0, Outcome::Fail(Severity::CouldNotRun));
        assert!(
            outcomes[0].1.contains("~/.local/bin/trustdoc"),
            "the diagnosis names the remedy: {}",
            outcomes[0].1
        );

        let r = run_stage(&cc, &spec(StageId::Doctests));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "doctests decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip);
        assert!(
            outcomes[0].1.contains("see the test line"),
            "{}",
            outcomes[0].1
        );

        // The regex lane compiles aterm-search doctests, so it takes the same
        // rule — a skip pointing at the test line, never a raw exec death that
        // would land as GateFailed and outrank the honest COULD-NOT-RUN.
        let r = run_stage(&cc, &spec(StageId::RegexLane));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "regex lane decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip);
        assert!(
            outcomes[0].1.contains("see the test line"),
            "{}",
            outcomes[0].1
        );

        // `--selftest` executes nothing, so there is no machine to diagnose:
        // the ladder keeps its uniform selftest skips and the run stays green.
        let mut cs = ctx(Scope::workspace());
        cs.tools.targo = PathBuf::from("/bin/sh");
        cs.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        cs.path_env = "/nonexistent-dir".into();
        cs.selftest = true;
        let r = run_stage(&cs, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "selftest test decided once");
        assert_eq!(outcomes[0].0, Outcome::Skip);
        assert!(
            outcomes[0].1.contains("(selftest: not executed)"),
            "{}",
            outcomes[0].1
        );

        // A cone with no lib target compiles no doctests — rustdoc is never
        // spawned, so the run proceeds instead of blaming a tool it never
        // needed. `/usr/bin/true` stands in for targo: an Ok outcome proves
        // the stage RAN the child rather than declaring COULD-NOT-RUN.
        let mut cb = ctx(Scope::changed("main", vec!["xtask".into()], false));
        cb.tools.targo = PathBuf::from("/usr/bin/true");
        cb.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        cb.path_env = "/nonexistent-dir".into();
        // The ctx() helper's `/repo` root is fine for stages that never spawn;
        // this case must actually exec, so the child needs a real cwd.
        cb.root = std::env::temp_dir();
        cb.scratch = std::env::temp_dir();
        let r = run_stage(&cb, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes.len(), 1, "bin-only test decided once");
        assert_eq!(outcomes[0].0, Outcome::Ok, "{}", outcomes[0].1);

        // `--scope <crate>` answers the lib question from the member's own
        // manifest: a bin-only scope runs (rustdoc never spawns), and the
        // same scope with a lib is a real diagnosis.
        let tmp = crate::mktemp_dir("atv-doc-scope").expect("mktemp");
        let bin_only = tmp.join("crates/binonly");
        std::fs::create_dir_all(bin_only.join("src")).unwrap();
        std::fs::write(
            bin_only.join("Cargo.toml"),
            "[package]\nname = \"binonly\"\n",
        )
        .unwrap();
        let mut cx = ctx(Scope::Crate("binonly".into()));
        cx.tools.targo = PathBuf::from("/usr/bin/true");
        cx.tools.trustdoc = PathBuf::from("/nonexistent/trustdoc");
        cx.path_env = "/nonexistent-dir".into();
        cx.root = tmp.clone();
        cx.scratch = std::env::temp_dir();
        let r = run_stage(&cx, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(outcomes[0].0, Outcome::Ok, "{}", outcomes[0].1);

        std::fs::write(bin_only.join("src/lib.rs"), "").unwrap();
        let r = run_stage(&cx, &spec(StageId::Test));
        let outcomes: Vec<_> = r.outcomes().collect();
        assert_eq!(
            outcomes[0].0,
            Outcome::Fail(Severity::CouldNotRun),
            "{}",
            outcomes[0].1
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_doc_driver_rule_pins_stage2_first_and_diagnoses_absence() {
        // The pinned driver always wins (the gate runs THE toolchain's
        // drivers, and its RUSTDOC binding overrides even a caller's export);
        // the caller's export outranks the PATH walk exactly as cargo ranks
        // env over config; a farm link keeps an older stage2 running
        // fail-closed; and nothing-anywhere is a diagnosis, never a raw exec
        // death after the unit tests already passed.
        assert_eq!(doc_driver_from(true, true, true), DocDriver::Stage2);
        assert_eq!(doc_driver_from(true, false, false), DocDriver::Stage2);
        assert_eq!(doc_driver_from(false, true, true), DocDriver::Ambient);
        assert_eq!(doc_driver_from(false, true, false), DocDriver::Ambient);
        assert_eq!(doc_driver_from(false, false, true), DocDriver::BarePath);
        assert_eq!(doc_driver_from(false, false, false), DocDriver::Absent);
    }

    #[test]
    fn the_workspace_argv_is_what_the_script_ran() {
        let s = Scope::workspace();
        assert_eq!(build_args(&s), ["--unverified", "build", "--workspace"]);
        assert_eq!(test_args(&s), ["--unverified", "test", "--workspace"]);
        assert_eq!(
            doctest_args(&s),
            ["--unverified", "test", "--doc", "--workspace"]
        );
        assert_eq!(
            regex_lane_args(),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-search",
                "--features",
                "regex"
            ]
        );
        assert_eq!(
            xtask_gate_args("mainloop"),
            [
                "--unverified",
                "run",
                "-q",
                "-p",
                "xtask",
                "--",
                "gate",
                "mainloop"
            ]
        );
        assert_eq!(
            freeze_gate_args(),
            [
                "--unverified",
                "build",
                "--manifest-path",
                "tools/freeze-safety-gate/Cargo.toml"
            ]
        );
        assert_eq!(
            differential_args(),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-bench",
                "--test",
                "differential"
            ]
        );
        assert_eq!(
            libc_oracle_cmd(&ctx(Scope::workspace()))
                .expect("absolute fixture root")
                .argv(),
            ["/repo/libc-oracle/run.sh"]
        );
        assert_eq!(
            tippy_args(&s),
            [
                "--workspace",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings"
            ]
        );
    }

    #[test]
    fn a_scope_reaches_the_build_test_doctest_and_lint_argv_together() {
        let s = Scope::crate_only("aterm-grid");
        assert_eq!(
            build_args(&s),
            ["--unverified", "build", "-p", "aterm-grid"]
        );
        assert_eq!(test_args(&s), ["--unverified", "test", "-p", "aterm-grid"]);
        assert_eq!(
            doctest_args(&s),
            ["--unverified", "test", "--doc", "-p", "aterm-grid"]
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
        // and NOT the whole-tree stages
        assert_eq!(
            regex_lane_args(),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-search",
                "--features",
                "regex"
            ],
            "the regex lane is always that crate, or it is not run at all"
        );
    }

    #[test]
    fn a_change_scope_reaches_the_same_four_argvs_as_one_dash_p_per_crate() {
        let s = Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true);
        assert_eq!(
            build_args(&s),
            [
                "--unverified",
                "build",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui"
            ]
        );
        assert_eq!(
            test_args(&s),
            [
                "--unverified",
                "test",
                "-p",
                "aterm-grid",
                "-p",
                "aterm-gui"
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
                "aterm-gui"
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
        // The guard is in `run_scoped`, so it has to hold for every stage that
        // uses it — a stage that forgot would run the `--workspace` fallback and
        // build the whole tree under the label `<no crates selected>`.
        let c = ctx(Scope::changed("main", vec![], true));
        for stage in [
            StageId::Build,
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
        assert_eq!(&a[sep + 1..], ["-D", "warnings"]);
        assert!(a[..sep].contains(&"--all-targets".to_string()));
        // `--keep-going` is a COVERAGE guarantee, not a preference: without it
        // the first crate that trips `-D warnings` ends the run and the rest of
        // the workspace is never linted at all, while the verdict still reads
        // like a statement about the whole tree. It must sit on cargo's side of
        // the separator — passed after `--` it would reach the lint driver,
        // which does not know the flag.
        assert!(
            a[..sep].contains(&"--keep-going".to_string()),
            "the lint must not stop at the first failing crate: {a:?}"
        );
    }

    #[test]
    fn tippy_keeps_its_own_target_dir_and_migration_quiet() {
        // Both are load-bearing: a shared target dir would churn the stock build
        // on every gate run, and the migration warning is noise the lint would
        // otherwise turn into a failure under -D warnings.
        let mut c = ctx(Scope::workspace());
        c.tools.tippy = Some(PathBuf::from("/s2/targo-tippy"));
        c.path_env = std::ffi::OsString::from("/usr/bin");
        let mut r = Report::new("t");
        // selftest short-circuits before spawning, so this exercises construction
        // only; the environment is asserted from the same code path below.
        c.selftest = true;
        tippy(&c, &mut r);
        assert_eq!(r.outcomes().count(), 1);

        // Assert on the command the STAGE builds, not on one this test spells
        // out again: a replica agrees with itself even after the stage stops
        // setting a variable.
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

    #[test]
    fn the_gated_pass_names_every_required_features_target_and_only_those() {
        let argv = tippy_gated_args(&Scope::workspace()).expect("the workspace selects both");
        assert_eq!(
            argv,
            [
                "-p",
                "aterm-gui",
                "-p",
                "aterm-scrollback",
                "--features",
                "aterm-gui/bench-support,aterm-gui/control-conformance,\
                 aterm-scrollback/disk-tier",
                "--all-targets",
                "--keep-going",
                "--",
                "-D",
                "warnings",
            ],
            "the second pass turns on exactly the features that unlock the six \
             `required-features` targets, and keeps going past a red one"
        );
    }

    #[test]
    fn the_gated_pass_narrows_with_the_scope_and_disappears_when_it_has_nothing_to_reach() {
        // One gated package selected: only its features, only its `-p`.
        let one = tippy_gated_args(&Scope::crate_only("aterm-scrollback"))
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
        assert_eq!(tippy_gated_args(&Scope::crate_only("aterm-core")), None);
        assert_eq!(
            tippy_gated_args(&Scope::changed("main", vec!["aterm-core".into()], true)),
            None
        );
        assert!(
            tippy_gated_args(&Scope::changed("main", vec!["aterm-gui".into()], true)).is_some(),
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
    fn the_regex_lane_carries_the_marker_that_arms_the_regex_tests() {
        // Without `ATERM_SEARCH_REGEX_LANE` the aterm-search suite still runs
        // and still passes — green, with no regex coverage. The marker IS the
        // stage; asserting it on the command the stage builds is the only way
        // to notice it going missing.
        let c = ctx(Scope::workspace());
        let cmd = regex_lane_cmd(&c);
        assert_eq!(env_names(&cmd), ["ATERM_SEARCH_REGEX_LANE"]);
        assert_eq!(cmd.envs[0].1.to_string_lossy(), "1");
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, regex_lane_args());
    }

    #[test]
    fn the_doc_running_stages_bind_trusts_renamed_doc_driver() {
        // Trust renames rustdoc to `trustdoc`. Unbound, doctests either fail to
        // launch or run under whatever rustdoc the ambient PATH offers — which
        // is not the driver the rest of the gate used.
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
    fn every_kani_crate_gets_its_own_selector() {
        // One script, three runs, distinguished ONLY by `KANI_CRATE`. Drop it
        // and all three iterations prove the same crate while the ladder still
        // prints three green rows — three claims, one of them true.
        let gate = std::path::Path::new("/repo/tools/verify-kani-proofs.sh");
        let mc = std::path::Path::new("/store/trust-mc/current");
        let ay = std::path::Path::new("/store/bin");
        let selectors: Vec<String> = KANI_CRATES
            .iter()
            .map(|k| {
                let cmd = kani_cmd(gate, k, mc, ay);
                assert_eq!(
                    env_names(&cmd),
                    ["KANI_CRATE", "TRUST_MC_SYSROOT", "AY_BIN_DIR"]
                );
                assert_eq!(
                    cmd.envs[1].1.as_os_str(),
                    mc.as_os_str(),
                    "the script drives THIS trust-mc"
                );
                assert_eq!(cmd.envs[2].1.as_os_str(), ay.as_os_str(), "and THIS ay");
                cmd.envs[0].1.to_string_lossy().into_owned()
            })
            .collect();
        assert_eq!(selectors, KANI_CRATES);
        assert_eq!(
            selectors
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            KANI_CRATES.len(),
            "each run must select a DIFFERENT crate"
        );
    }

    #[test]
    fn trust_mc_and_ay_resolve_env_then_store_then_source() {
        // The order the script uses, decided in one place so the availability
        // check and the run can never disagree. Explicit env wins outright; the
        // atpkg store is taken only when it really holds the tool; the
        // from-source dev build is the last resort.
        let home = crate::mktemp_dir("atv-kani").expect("mktemp");
        let prefix = crate::toolchain::default_atpkg_prefix(&home);
        let env = EnvSnapshot {
            home: home.clone(),
            ..EnvSnapshot::default()
        };
        assert_eq!(
            trust_mc_sysroot(&env),
            home.join("trust/first-party/trust-mc/target/trust-mc"),
            "no store, no override: the dev default"
        );
        assert_eq!(
            ay_bin_dir(&env),
            home.join("trust/first-party/ay/target/release")
        );

        // `aterm pkg install trust-mc` / `ay` shape: the live-build link for the
        // sysroot, the shim dir for the solver.
        let mc = prefix.join("store/trust-mc/current/bin");
        std::fs::create_dir_all(&mc).expect("mkdir");
        std::fs::write(mc.join("trust-mc-driver"), b"#!/bin/sh\nexit 0\n").expect("write");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                mc.join("trust-mc-driver"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("chmod");
        }
        let shims = prefix.join("bin");
        std::fs::create_dir_all(&shims).expect("mkdir");
        std::fs::write(shims.join("ay"), b"#!/bin/sh\nexit 0\n").expect("write");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(shims.join("ay"), std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        assert_eq!(
            trust_mc_sysroot(&env),
            prefix.join("store/trust-mc/current")
        );
        assert_eq!(ay_bin_dir(&env), shims);

        let env = EnvSnapshot {
            trust_mc_sysroot: Some(PathBuf::from("/explicit/sysroot")),
            ay_bin_dir: Some(PathBuf::from("/explicit/ay")),
            ..env
        };
        assert_eq!(trust_mc_sysroot(&env), PathBuf::from("/explicit/sysroot"));
        assert_eq!(ay_bin_dir(&env), PathBuf::from("/explicit/ay"));
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn no_targo_is_fail_closed_at_the_build_and_honest_everywhere_after() {
        let c = ctx(Scope::workspace());
        assert!(!c.tools.have_targo());

        let mut r = Report::new("build");
        build(&c, &mut r);
        let (outcome, label) = r.outcomes().next().expect("a decision");
        assert_eq!(outcome, crate::Outcome::Fail(crate::Severity::CouldNotRun));
        assert!(label.starts_with("targo not found at "), "{label}");
        assert!(
            label.contains("x.py build --stage 2"),
            "the diagnostic says how to fix it"
        );

        // The dependent stages then skip — honestly, and named, so the verdict
        // refuses the merge contract for the whole run.
        let dependent: [fn(&Ctx, &mut Report); 7] = [
            test,
            doctests,
            regex_lane,
            feature_gates,
            freeze_gate,
            proof_inventory,
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
        let script_stages: [fn(&Ctx, &mut Report); 4] =
            [grep_guards, install_channel, start_compare, license_headers];
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

    #[test]
    fn the_redraw_gate_builds_the_feature_without_which_it_builds_nothing() {
        // `aterm-redraw-conformance` carries `required-features =
        // ["control-conformance"]`. Drop the feature and cargo does not error —
        // it matches no target and exits 0, so the stage would sail on and
        // decide on whatever binary an earlier run happened to leave behind.
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
    fn a_redraw_gate_that_could_not_run_is_never_a_pass_and_never_a_skip() {
        // THE WHOLE POINT OF THE MAPPING. Exit 2 is the harness saying no event
        // loop is constructible here; a headless box must not read that as green,
        // and it must not read as a quiet skip either — that is the same false
        // green one layer up.
        let (outcome, label) = redraw_outcome(Some(2));
        assert_eq!(outcome, Outcome::Fail(Severity::CouldNotRun));
        assert_ne!(outcome, Outcome::Ok);
        assert_ne!(outcome, Outcome::Skip);
        assert!(label.contains("NOT RUN"), "{label}");

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

    #[test]
    fn selftest_executes_nothing_heavy_and_says_so() {
        let mut c = ctx(Scope::workspace());
        c.selftest = true;
        c.tools.targo = PathBuf::from("/bin/sh"); // pretend a driver exists
        let mut r = Report::new("build");
        build(&c, &mut r);
        assert_eq!(
            r.outcomes().collect::<Vec<_>>(),
            [(
                crate::Outcome::Skip,
                "targo build --workspace (selftest: not executed)"
            )]
        );
    }
}
