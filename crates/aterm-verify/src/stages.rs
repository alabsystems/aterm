// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stages themselves.
//!
//! Every stage that shelled out still shells out to the SAME command with the
//! same arguments and the same environment. The argv of each one is built by a
//! small pure function below, so the port is checked by unit tests rather than by
//! a reviewer diffing two languages — including the details that are easy to lose
//! and expensive to lose: tippy's separate `CARGO_TARGET_DIR` and
//! `TRUST_NO_MIGRATE_WARN`, `RUSTDOC=<stage2>/trustdoc` on the doc-running
//! stages, `ATERM_SEARCH_REGEX_LANE=1` on the regex lane, and `--unverified` on
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
        StageId::GrepGuards => grep_guards(ctx, &mut r),
        StageId::InstallChannel => install_channel(ctx, &mut r),
        StageId::TrustGateVerdict => trust_gate_verdict(ctx, &mut r),
        StageId::TrustContractProbe => trust_contract_probe(ctx, &mut r),
        StageId::StartCompare => start_compare(ctx, &mut r),
        StageId::LicenseHeaders => license_headers(ctx, &mut r),
        StageId::FeatureGates => feature_gates(ctx, &mut r),
        StageId::FreezeGate => freeze_gate(ctx, &mut r),
        StageId::ProofInventory => proof_inventory(ctx, &mut r),
        StageId::ControlSocketSmoke => smoke_stages::control_socket_smoke(ctx, &mut r),
        StageId::GuiSmoke => smoke_stages::gui_typing_smoke(ctx, &mut r),
        StageId::RedrawConformance => redraw_conformance(ctx, &mut r),
        StageId::DifferentialOracle => differential_oracle(ctx, &mut r),
        StageId::KaniFloor => kani_floor(ctx, &mut r),
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

/// `<tippy> <scope> --all-targets -- -D warnings`
#[must_use]
pub fn tippy_args(scope: &Scope) -> Vec<String> {
    let mut a = scope.args();
    a.extend(
        ["--all-targets", "--", "-D", "warnings"]
            .into_iter()
            .map(String::from),
    );
    a
}

/// `targo --unverified run -q -p xtask -- gate <name>`
#[must_use]
pub fn xtask_gate_args(gate: &str) -> Vec<String> {
    [
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
    .collect()
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
/// ladder still prints three green rows.
fn kani_cmd(gate: &std::path::Path, krate: &str) -> Cmd {
    Cmd::new(gate)
        .env("KANI_CRATE", krate)
        .capture(Capture::Emit)
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
//    so trustdoc is bound here as well as in the explicit doc-only stage.
// ---------------------------------------------------------------------------
fn test(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("targo test (no targo)");
        return;
    }
    let label = format!("targo test {}", ctx.scope.label());
    let cmd = targo(ctx, test_args(&ctx.scope));
    if ctx.tools.have_trustdoc() {
        run_scoped(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        );
    } else {
        run_scoped(ctx, r, &label, &cmd);
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
    // `cargo test --doc -p …` is a hard ERROR — "no library targets found in
    // package `X`" — when NONE of the selected packages has a lib, and an
    // all-binary selection is an ordinary outcome under `--changed`: `xtask` is
    // bin-only, so a branch that edits only crates/xtask selects exactly {xtask}
    // and this stage would go RED on a completely healthy tree. A tier that
    // cries wolf is worse than no tier, so ask first and skip honestly — a skip
    // is counted, and the verdict narrows accordingly.
    if !ctx.scope.selects_nothing() && !ctx.scope.has_lib_target() {
        r.skip(format!(
            "targo test --doc (no package in scope has a library target: {})",
            ctx.scope.label()
        ));
        return;
    }
    let cmd = targo(ctx, doctest_args(&ctx.scope));
    if ctx.tools.have_trustdoc() {
        run_scoped(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        );
    } else {
        run_scoped(ctx, r, &label, &cmd);
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
    if ctx.tools.have_trustdoc() {
        run_labeled(
            ctx,
            r,
            &format!("{label} (trustdoc)"),
            &with_trustdoc(ctx, cmd),
        );
    } else {
        run_labeled(ctx, r, label, &cmd);
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
fn tippy_cmd(ctx: &Ctx, bin: &std::path::Path) -> Cmd {
    let dir = bin.parent().unwrap_or(&ctx.tools.stage2_dir).to_path_buf();
    let mut path = std::ffi::OsString::from(dir.as_os_str());
    path.push(":");
    path.push(&ctx.path_env);
    Cmd::new(bin)
        .args(tippy_args(&ctx.scope))
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
    let cmd = tippy_cmd(ctx, &bin);
    run_scoped(
        ctx,
        r,
        &format!("tippy {} -D warnings", ctx.scope.label()),
        &cmd,
    );
}

// ---------------------------------------------------------------------------
// 3) GREP GUARDS (zero-tolerance, always whole-tree)
// ---------------------------------------------------------------------------
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
//    sentence that IS the campaign claim ("100% MACHINE-PROVED (workspace, …)")
//    and has two knobs that shrink the run. Until its self-test existed the
//    verdict logic had never been exercised against a narrowed run at all, and
//    it printed the workspace sentence for runs that were not the workspace.
//    Hard-required, exactly like test-install-channel.sh: a missing self-test is
//    not a skip, because the thing it guards is a claim.
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
// 4.5b) L0 TEMPORAL-SAFETY GATE — ONE build of tools/freeze-safety-gate enforces
//    five obligations (temporal proof, main-loop census, lock-order census,
//    wasm-process census, scope-cardinality census), any one FAILING the build
//    with a counterexample-backed diagnostic.
// ---------------------------------------------------------------------------
fn freeze_gate(ctx: &Ctx, r: &mut Report) {
    if !ctx.tools.have_targo() {
        r.skip("L0 temporal-safety gate (no targo)");
        return;
    }
    run_labeled(
        ctx,
        r,
        "freeze-safety-gate (5 obligations)",
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
    let build = exec::run(
        &targo(ctx, redraw_conformance_build_args()),
        ctx.exec_env(),
    );
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
fn kani_floor(ctx: &Ctx, r: &mut Report) {
    if ctx.selftest {
        r.skip("trust-mc (selftest)");
        return;
    }
    let gate = ctx.root.join("scripts/verify-kani-proofs.sh");
    let mc_root = ctx.env.trust_mc_sysroot.clone().unwrap_or_else(|| {
        ctx.env
            .home
            .join("trust/first-party/trust-mc/target/trust-mc")
    });
    let ay_dir = ctx
        .env
        .ay_bin_dir
        .clone()
        .unwrap_or_else(|| ctx.env.home.join("trust/first-party/ay/target/release"));

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
        r.skip("trust-mc / Kani BMC floor (tool unavailable; pending build)");
        return;
    }
    if !is_executable_file(&ay_dir.join("ay")) && !have_on_path("ay", &ctx.path_env) {
        r.raw("  NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN: ay is unavailable");
        r.raw(format!(
            "          at {} and on PATH (the embedded + ty tiers still ran).",
            ay_dir.display()
        ));
        r.skip("trust-mc / Kani BMC floor (solver unavailable; pending build)");
        return;
    }
    for krate in KANI_CRATES {
        run_labeled(
            ctx,
            r,
            &format!("verify-kani-proofs.sh ({krate})"),
            &kani_cmd(&gate, krate),
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
            tippy_args(&s),
            ["--workspace", "--all-targets", "--", "-D", "warnings"]
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
            ["-p", "aterm-grid", "--all-targets", "--", "-D", "warnings"]
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
        let cmd = tippy_cmd(&c, std::path::Path::new("/s2/targo-tippy"));
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
        let selectors: Vec<String> = KANI_CRATES
            .iter()
            .map(|k| {
                let cmd = kani_cmd(gate, k);
                assert_eq!(env_names(&cmd), ["KANI_CRATE"]);
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
        assert_eq!(
            redraw_outcome(None).0,
            Outcome::Fail(Severity::CouldNotRun)
        );
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
