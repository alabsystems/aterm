// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The gate, end to end, over a synthetic repo.
//!
//! `tools/verify.sh` was 900 lines of bash that decided whether code may land,
//! and nothing had ever tested it. These are the tests that would have caught the
//! false green it was found printing: a whole run is driven here — plan,
//! scheduler, ladder, tally, verdict — and asserted on as text, because the text
//! IS the contract a reviewer reads.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use aterm_verify::changed::{self, Selection};
use aterm_verify::cli::Mode;
use aterm_verify::ladder::{Report, Tally, tally};
use aterm_verify::plan::{Lane, StageId, StageSpec};
use aterm_verify::verdict::{MERGE_CONTRACT_SENTENCE, verdict};
use aterm_verify::{Ctx, EnvSnapshot, Scope, exit, mktemp_dir, plan, sched, stages};

/// A repo-shaped directory: the helper scripts the gate calls, all passing.
struct FakeRepo {
    root: PathBuf,
    stage2: PathBuf,
    scratch: PathBuf,
}

impl FakeRepo {
    fn new() -> Self {
        let base = mktemp_dir("atv-repo").expect("mktemp");
        let root = base.join("repo");
        let stage2 = base.join("stage2");
        let scratch = base.join("scratch");
        for d in [&root, &stage2, &scratch] {
            fs::create_dir_all(d).expect("mkdir");
        }
        fs::create_dir_all(root.join("tools/perf-arena")).expect("mkdir");
        fs::create_dir_all(root.join("scripts")).expect("mkdir");
        fs::create_dir_all(root.join("libc-oracle")).expect("mkdir");
        fs::write(root.join("Cargo.toml"), b"[workspace]\n").expect("write");
        let me = Self {
            root,
            stage2,
            scratch,
        };
        me.script("tools/verify.sh", "exit 0");
        me.script("tools/grep_guard.sh", "echo 'GUARD: PASS'; exit 0");
        me.script("tools/license_check.sh", "echo 'LICENSE: PASS'; exit 0");
        me.script("tools/test-install-channel.sh", "exit 0");
        me.script("tools/test-trust-gate-verdict.sh", "exit 0");
        me.script("tools/test-trust-contract-probe.sh", "exit 0");
        me.script("tools/perf-arena/test-start-compare.sh", "exit 0");
        me.script("libc-oracle/run.sh", "exit 0");
        // The redraw harness the gate builds and then DRIVES. Present and passing
        // by default so an unrelated test never reads a missing binary as a
        // finding; `redraw_harness` re-writes it for the tests that are about
        // what its exit code means.
        me.redraw_harness(0);
        me
    }

    /// A stand-in `aterm-redraw-conformance` that exits `code` — the harness's
    /// own contract is `0` pass / `1` fail / `2` could-not-run, and what these
    /// tests exercise is the STAGE's reading of it, not cargo's.
    fn redraw_harness(&self, code: i32) -> &Self {
        fs::create_dir_all(self.root.join("target/debug")).expect("mkdir");
        self.script(
            "target/debug/aterm-redraw-conformance",
            &format!("echo 'aterm-redraw-conformance: stub'; exit {code}"),
        );
        self
    }

    fn script(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("write");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// A stage2 whose driver also produces the two binaries the smokes drive:
    /// an `aterm-gui` that binds something socket-shaped and stays up, and an
    /// `aterm-ctl` that answers the protocol. This is what lets the control-socket
    /// smoke — launch, poll, round-trip, burst, teardown — run for real in a test.
    fn with_answering_smoke(&self) -> &Self {
        self.with_stage2(
            r#"echo "argv: $*"
case "$*" in
  *aterm-gui*aterm-ctl*)
    mkdir -p target/debug
    cat >target/debug/aterm-gui <<'GUI'
#!/bin/sh
mkdir -p "$XDG_RUNTIME_DIR/aterm"
ln -s /dev/null "$XDG_RUNTIME_DIR/aterm/aterm.sock"
exec sleep 300
GUI
    cat >target/debug/aterm-ctl <<'CTL'
#!/bin/sh
case "$1" in
  cursor)  echo "OK row=0 col=0" ;;
  metrics) echo "OK frames=41 max_input_present_ms=8.100 redraw_retry_gated=0 present_drops=0 sync_rel_timeout=0 perf_reduced=0 wake_heals=0 " ;;
  send|key) echo "OK accepted" ;;
  *) echo "ERR unknown verb"; exit 1 ;;
esac
CTL
    chmod 755 target/debug/aterm-gui target/debug/aterm-ctl
    ;;
esac
exit 0"#,
        )
    }

    /// Install a stand-in Trust stage2. `targo_body` decides what the driver does.
    fn with_stage2(&self, targo_body: &str) -> &Self {
        for (name, body) in [("targo", targo_body), ("trustdoc", "exit 0")] {
            let p = self.stage2.join(name);
            fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("write");
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        self
    }

    fn ctx(&self, mode: Mode, scope: Scope, selftest: bool) -> Ctx {
        let mut env = EnvSnapshot::capture();
        env.trust_stage2_bin = Some(self.stage2.clone());
        // The GUI smoke measures a real window; a synthetic repo has none, so it
        // takes its honest skip instead of trying to open one.
        env.skip_gui_smoke = Some("1".into());
        // Point the Tier-2 prover locations inside the sandbox so these tests
        // decide the same thing on a machine that has trust-mc built and on one
        // that does not.
        env.trust_mc_sysroot = Some(self.root.join("no-trust-mc"));
        env.ay_bin_dir = Some(self.root.join("no-ay"));
        // …and unset the operator's target-dir redirect, for the same reason.
        // Two stages here do not just SPAWN a child, they DRIVE a binary the
        // build was supposed to leave behind — the control-socket smoke and the
        // redraw gate — and both resolve it with `smoke::debug_bin`, which
        // honours `CARGO_TARGET_DIR` exactly as cargo does. `capture()` reads
        // the ambient one, so a shell that exports a redirect (this repo is
        // worked in by several sessions at once and each keeps its own target
        // dir to stay off the shared cargo lock) sent both stages hunting
        // outside the sandbox for binaries the fixture had written INSIDE it:
        // `redraw conformance: just-built harness missing (<redirect>/debug/…)`
        // — a COULD-NOT-RUN, so `the_redraw_gate_…` read `could_not_run: 1`
        // where a passing harness must leave the tally clean — and
        // `smoke: just-built binaries missing`, which cost the scoped run its
        // exit::PASS. Nothing about the stages was wrong: they were reading the
        // right variable and the fixture was the one lying about the repo.
        // `None` is the pin because the synthetic repo has cargo's DEFAULT
        // layout — `redraw_harness` and `with_answering_smoke` write to
        // `<root>/target/debug`, which is precisely where `debug_bin` looks
        // when the variable is unset.
        env.cargo_target_dir = None;
        Ctx::new(
            self.root.clone(),
            mode,
            scope,
            selftest,
            env,
            self.scratch.clone(),
        )
    }

    fn run(&self, mode: Mode, scope: Scope, selftest: bool) -> (String, i32) {
        let ctx = self.ctx(mode, scope, selftest);
        let mut out: Vec<u8> = Vec::new();
        let code = aterm_verify::run(&ctx, &mut out).expect("the ladder is writable");
        (String::from_utf8(out).expect("utf-8 ladder"), code)
    }
}

impl Drop for FakeRepo {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            fs::remove_dir_all(base).ok();
        }
    }
}

/// The `=== … ===` headers, in the order they were printed.
fn headers(ladder: &str) -> Vec<String> {
    ladder
        .lines()
        .filter_map(|l| l.strip_prefix("=== ").and_then(|l| l.strip_suffix(" ===")))
        .map(str::to_string)
        .collect()
}

/// Every ladder decision, as `("ok"|"skip"|"FAIL", label)`.
fn decisions(ladder: &str) -> Vec<(&str, &str)> {
    ladder
        .lines()
        .filter_map(|l| {
            for tag in ["ok", "skip", "FAIL"] {
                let prefix = format!("  {tag}");
                if l.starts_with(&prefix) {
                    let rest = l[prefix.len()..].trim_start();
                    if l.len() > prefix.len() && l.as_bytes()[prefix.len()] == b' ' {
                        return Some((tag, rest));
                    }
                }
            }
            None
        })
        .collect()
}

fn labels_with(ladder: &str, tag: &str) -> Vec<String> {
    decisions(ladder)
        .into_iter()
        .filter(|(t, _)| *t == tag)
        .map(|(_, l)| l.to_string())
        .collect()
}

#[test]
fn the_ladder_prints_every_stage_in_the_declared_order_however_they_ran() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let (ladder, _) = repo.run(Mode::Full, Scope::workspace(), false);

    let ctx = repo.ctx(Mode::Full, Scope::workspace(), false);
    let mut expected: Vec<String> = plan::plan(&ctx).into_iter().map(|s| s.title).collect();
    assert_eq!(
        expected.len(),
        21,
        "19 gate stages plus the two --full tiers"
    );
    expected.push("verdict".to_string());
    assert_eq!(headers(&ladder), expected);
    // Concurrency must never reorder the record.
    let mut sorted = ladder
        .match_indices("=== ")
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    sorted.sort_unstable();
    assert!(sorted.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn a_run_with_no_driver_fails_closed_and_never_claims_the_contract() {
    // The bare-machine case the bash gate handled by printing skips: here the
    // build FAILS honestly, and the verdict says nothing was decided.
    let repo = FakeRepo::new();
    let (ladder, code) = repo.run(Mode::Fast, Scope::workspace(), false);

    assert_eq!(code, exit::COULD_NOT_RUN);
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE));
    assert!(ladder.contains("VERIFY: COULD NOT RUN (mode=fast scope=workspace) — DO NOT merge"));
    let failed = labels_with(&ladder, "FAIL");
    assert!(
        failed.iter().any(|l| l.starts_with("targo not found at ")),
        "the missing driver is named: {failed:?}"
    );
    // …and the stages that need it skip by name, so the verdict can list them.
    for needed in [
        "targo test (no targo)",
        "targo test --doc (no targo)",
        "regex search lane (no targo)",
        "feature gates (no targo)",
        "L0 temporal-safety gate (no targo)",
        "gate counts (no targo)",
        "smoke (no targo)",
    ] {
        assert!(
            labels_with(&ladder, "skip").iter().any(|l| l == needed),
            "missing skip: {needed}"
        );
    }
    // The guards do not need a driver, so they still really ran.
    assert!(labels_with(&ladder, "ok").contains(&"grep_guard.sh".to_string()));
    assert!(labels_with(&ladder, "ok").contains(&"license_check.sh".to_string()));
}

#[test]
fn a_failing_guard_is_a_finding_and_exits_one() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    // The stand-in guard reports a made-up check: the real guard's banned tokens
    // are zero-tolerance across crates/, so writing one here would fail the tree
    // this crate exists to gate.
    repo.script(
        "tools/grep_guard.sh",
        "echo '  FAIL A9a zero banned tokens 3'; echo 'GUARD: FAIL'; exit 1",
    );
    let (ladder, code) = repo.run(Mode::Fast, Scope::workspace(), false);

    assert_eq!(
        code,
        exit::FAILED,
        "a guard finding is a FAILED gate, not a broken machine"
    );
    assert!(ladder.contains("VERIFY: FAIL (mode=fast scope=workspace) — DO NOT merge"));
    assert!(ladder.contains("  FAIL  grep_guard.sh"));
    // The guard's own output is kept, above the line it explains.
    let at_output = ladder
        .find("FAIL A9a zero banned tokens 3")
        .expect("guard output");
    let at_ladder = ladder.find("  FAIL  grep_guard.sh").expect("ladder line");
    assert!(at_output < at_ladder);
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE));
}

#[test]
fn the_libc_oracle_driver_is_required_and_fails_closed() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let mut ctx = repo.ctx(Mode::Fast, Scope::workspace(), false);
    ctx.env.cargo_target_dir = Some("caller-relative-target".into());
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::LibcOracle)
        .expect("the libc oracle is planned");

    let owned_target = repo.root.join("libc-oracle/target");
    repo.script(
        "libc-oracle/run.sh",
        &format!(
            "test \"$CARGO_TARGET_DIR\" = \"{}\" || exit 1; \
             test \"$PYTHONDONTWRITEBYTECODE\" = 1 || exit 1; exit 0",
            owned_target.display()
        ),
    );
    let passed = stages::run_stage(&ctx, &spec);
    let t = tally(std::slice::from_ref(&passed));
    assert!(!t.failed(), "the driver received its owned environment");
    assert_eq!(t.gate_failures, 0);
    assert_eq!(t.could_not_run, 0);

    fs::remove_file(repo.root.join("libc-oracle/run.sh")).expect("remove driver");
    let missing = stages::run_stage(&ctx, &spec);
    let t = tally(std::slice::from_ref(&missing));
    assert_eq!(t.could_not_run, 1, "a missing oracle decides nothing");
    assert_eq!(t.gate_failures, 0);
    assert!(
        missing
            .render()
            .contains("libc-oracle/run.sh missing or not executable"),
        "{}",
        missing.render()
    );

    repo.script(
        "libc-oracle/run.sh",
        "echo 'libc ABI mismatch in native runtime oracle'; exit 1",
    );
    let failed = stages::run_stage(&ctx, &spec);
    let t = tally(std::slice::from_ref(&failed));
    assert_eq!(t.gate_failures, 1, "a red oracle is a finding");
    assert_eq!(t.could_not_run, 0);
    assert!(failed.render().contains("libc ABI mismatch"));
    assert!(
        failed
            .render()
            .contains("FAIL  libc-oracle/run.sh (cross-cell ABI + native runtime)")
    );

    repo.script(
        "libc-oracle/run.sh",
        "echo 'required target/toolchain unavailable'; exit 3",
    );
    let unavailable = stages::run_stage(&ctx, &spec);
    let t = tally(std::slice::from_ref(&unavailable));
    assert_eq!(
        t.gate_failures, 0,
        "an environment failure is not a finding"
    );
    assert_eq!(t.could_not_run, 1, "exit 3 means nothing was decided");
    assert!(
        unavailable
            .render()
            .contains("required target/toolchain unavailable")
    );
}

#[test]
fn a_failing_driver_fails_every_stage_that_drives_it_and_nothing_else() {
    let repo = FakeRepo::new();
    repo.with_stage2("echo 'error: unknown unstable option: `trust-verify`' >&2; exit 1");
    let (ladder, code) = repo.run(Mode::Fast, Scope::workspace(), false);

    assert_eq!(code, exit::FAILED);
    for driven in [
        "targo build --workspace",
        "targo test --workspace (trustdoc)",
        "targo test --doc --workspace (trustdoc)",
        "gate drift",
        "gate dormant",
        "gate mainloop",
        "gate counts",
        "freeze-safety-gate (6 obligations)",
    ] {
        assert!(
            labels_with(&ladder, "FAIL").iter().any(|l| l == driven),
            "expected FAIL: {driven}"
        );
    }
    assert!(labels_with(&ladder, "ok").contains(&"grep_guard.sh".to_string()));
    assert!(
        ladder.contains("unknown unstable option"),
        "the diagnostic reaches the reader"
    );
}

#[test]
fn the_redraw_gate_reads_its_harness_exit_code_and_a_two_is_never_green() {
    // The regression this stage exists for lives one layer down: drop the
    // production `EventLoopProxy` and the harness exits 1. What is tested HERE is
    // the layer that was missing entirely — whether anything LOOKS. Above all
    // `2`, the harness saying no event loop is constructible on this machine: a
    // headless box reading that as green would be the same false pass in a new
    // place.
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let ctx = repo.ctx(Mode::Fast, Scope::workspace(), false);
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::RedrawConformance)
        .expect("the redraw gate is planned");

    repo.redraw_harness(0);
    assert_eq!(
        tally(&[stages::run_stage(&ctx, &spec)]),
        Tally::default(),
        "a passing harness leaves the run clean"
    );

    repo.redraw_harness(1);
    let t = tally(&[stages::run_stage(&ctx, &spec)]);
    assert_eq!(t.gate_failures, 1, "exit 1 is a finding about the tree");
    assert_eq!(t.could_not_run, 0);

    repo.redraw_harness(2);
    let r = stages::run_stage(&ctx, &spec);
    // `from_ref`, not `[r.clone()]`: `r` is read again below, so the clone was
    // only ever there to make a one-element slice — and cloning a stage Run to
    // count it invites the reading that `tally` consumes what it is given.
    let t = tally(std::slice::from_ref(&r));
    assert_eq!(t.could_not_run, 1, "exit 2 decided nothing");
    assert_eq!(t.gate_failures, 0, "…and is not a finding about the tree");
    assert_eq!(t.skipped(), 0, "…and above all is not a quiet skip");
    assert!(t.failed(), "so the run cannot end green");
    assert!(
        r.render()
            .contains("  FAIL  aterm-redraw-conformance: NOT RUN"),
        "{}",
        r.render()
    );
}

#[test]
fn a_scoped_run_narrows_the_driver_and_is_refused_the_contract() {
    let repo = FakeRepo::new();
    // Everything green — including a control-socket smoke that really launches,
    // really answers and really tears down — so the only thing standing between
    // this run and the merge-contract sentence is that it was narrowed.
    repo.with_answering_smoke();
    let (ladder, code) = repo.run(Mode::Fast, Scope::crate_only("aterm-grid"), false);

    assert_eq!(code, exit::PASS, "narrow is not failure: {ladder}");
    assert!(ladder.contains("argv: --unverified build -p aterm-grid"));
    assert!(ladder.contains("argv: --unverified test -p aterm-grid"));
    assert!(ladder.contains("argv: --unverified test --doc -p aterm-grid"));
    assert!(
        !ladder.contains("--workspace"),
        "nothing whole-tree was driven"
    );
    assert!(
        !headers(&ladder)
            .iter()
            .any(|h| h.starts_with("regex search lane"))
    );
    assert!(ladder.contains("  ok    smoke: aterm-ctl cursor -> OK row=0 col=0"));
    assert!(ladder.contains("  ok    smoke: typing burst pacing counters clean"));

    assert!(
        !ladder.contains(MERGE_CONTRACT_SENTENCE),
        "THE regression: a scoped run claiming it all"
    );
    assert!(ladder.contains("NOT the merge contract"));
    assert!(
        ladder.contains(
            "- scoped to -p aterm-grid: the rest of the workspace was not built or tested"
        )
    );
    // and the skips are named beside it
    assert!(ladder.contains("      - gui smoke (ATERM_SKIP_GUI_SMOKE)"));
}

#[test]
fn a_change_scoped_run_drives_one_dash_p_per_crate_and_is_refused_the_contract() {
    // Driven with a plain stand-in driver rather than the answering smoke: the
    // control-socket smoke launches a real process against a 10 s socket budget,
    // and a second test doing that concurrently is a coin-toss on a loaded
    // machine. What this test is ABOUT is the argv and the verdict, so the green
    // run is composed the way `run` composes it (plan -> scheduler -> tally ->
    // verdict) instead of being bought with a second live subprocess.
    let repo = FakeRepo::new();
    repo.with_stage2("echo \"argv: $*\"\nexit 0");
    let cone = Scope::changed("main", vec!["aterm-grid".into(), "aterm-gui".into()], true);
    let (ladder, _) = repo.run(Mode::Fast, cone.clone(), false);

    assert!(ladder.contains("argv: --unverified build -p aterm-grid -p aterm-gui"));
    assert!(ladder.contains("argv: --unverified test -p aterm-grid -p aterm-gui"));
    assert!(ladder.contains("argv: --unverified test --doc -p aterm-grid -p aterm-gui"));
    assert!(
        !ladder.contains("--workspace"),
        "nothing whole-tree was driven"
    );
    // aterm-search is not in the cone, so the lane has nothing to run.
    assert!(
        !headers(&ladder)
            .iter()
            .any(|h| h.starts_with("regex search lane"))
    );
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE));

    let ctx = repo.ctx(Mode::Fast, cone, false);
    let green = |s: &StageSpec| {
        let mut r = Report::new(s.title.clone());
        r.pass("did the thing");
        r
    };
    let reports = sched::run_stages(&plan::plan(&ctx), green, |_, _| {});
    let v = verdict(Mode::Fast, &ctx.scope, false, &tally(&reports));
    assert_eq!(v.exit, exit::PASS, "narrow is not failure");
    assert!(
        !v.claims_merge_contract,
        "a change-scoped run claiming it all"
    );
    assert!(!v.text.contains(MERGE_CONTRACT_SENTENCE));
    assert!(v.text.contains("NOT the merge contract"));
    assert!(v.text.contains("(mode=fast scope=changed:2, 0 skipped) —"));
    assert!(v.text.contains(
        "      - change-scoped against main to 2 crate(s) (aterm-grid aterm-gui): \
         every other workspace crate was not built or tested\n"
    ));
}

#[test]
fn a_change_scoped_run_that_selected_nothing_compiles_nothing() {
    // The docs-only branch. The whole-tree guard stages still run; the compiling
    // stages skip BY NAME, and the empty `--workspace` fallback never fires —
    // that fallback exists only so a dropped guard would build too much.
    //
    // No answering smoke here: this test is about which stages COMPILE, and the
    // control-socket smoke launches a real process on a 10 s socket budget that a
    // loaded machine can miss. The verdict text for this scope is pinned in
    // `verdict.rs` over the whole matrix instead.
    let repo = FakeRepo::new();
    repo.with_stage2("echo \"argv: $*\"\nexit 0");
    let (ladder, _) = repo.run(Mode::Fast, Scope::changed("main", vec![], true), false);

    // No "(trustdoc)" on the skipped test/doctest lines: a run that compiled
    // nothing never chose a doc driver, so the skip names no tool it never
    // bound — the empty selection outranks the doc-driver verdict.
    for named in [
        "targo build <no crates selected> (change-scoped run selected no crates)",
        "targo test <no crates selected> (change-scoped run selected no crates)",
        "targo test --doc <no crates selected> (change-scoped run selected no crates)",
    ] {
        assert!(
            labels_with(&ladder, "skip").iter().any(|l| l == named),
            "missing skip: {named}\n{ladder}"
        );
    }
    assert!(
        !ladder.contains("argv: --unverified build --workspace"),
        "the empty selection must never fall through to a whole-tree build"
    );
    assert!(!ladder.contains("argv: --unverified test --workspace"));
    // The guards are whole-tree in every mode and really ran.
    assert!(labels_with(&ladder, "ok").contains(&"grep_guard.sh".to_string()));
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE));
}

#[test]
fn an_all_binary_change_selection_skips_the_doctests_instead_of_failing_them() {
    // `targo test --doc -p xtask` is a hard error on a healthy tree, so the
    // stage asks before it runs. A tier that cries wolf is worse than no tier.
    //
    // Driven under --selftest, exactly as the script decided this one: the answer
    // does not depend on running anything, and a decision only reachable by a
    // thirty-minute run is a decision nobody ever checks.
    let repo = FakeRepo::new();
    repo.with_stage2("echo \"argv: $*\"\nexit 0");
    let bins_only = Scope::changed("main", vec!["xtask".into()], false);
    let (ladder, _) = repo.run(Mode::Fast, bins_only, true);

    let skips = labels_with(&ladder, "skip");
    assert!(
        skips
            .iter()
            .any(|l| l == "targo test --doc (no package in scope has a library target: -p xtask)"),
        "{ladder}"
    );
    // The doctest stage never even reached the point of naming a command…
    assert!(!ladder.contains("targo test --doc -p xtask"));
    // …while the stages that CAN run on a bin-only crate still carry the scope.
    assert!(
        skips
            .iter()
            .any(|l| l == "targo build -p xtask (selftest: not executed)")
    );
    assert!(
        skips
            .iter()
            .any(|l| l == "targo test -p xtask (trustdoc) (selftest: not executed)")
    );
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE));
}

#[test]
fn the_change_scope_stage_is_printed_first_and_counted_like_any_other() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let selection = Selection::Narrowed {
        seeds: vec!["aterm-grid".into()],
        crates: vec!["aterm-grid".into(), "aterm-gui".into()],
        any_lib: true,
    };
    let (scope, report) = changed::stage_report("main", &selection);
    let ctx = repo.ctx(Mode::Fast, scope, true).with_prelude(Some(report));
    let mut out: Vec<u8> = Vec::new();
    aterm_verify::run(&ctx, &mut out).expect("the ladder is writable");
    let ladder = String::from_utf8(out).expect("utf-8");

    assert_eq!(
        headers(&ladder).first().map(String::as_str),
        Some("change scope (--changed --base main)"),
        "the stage that CHOSE the scope comes before the stages that use it"
    );
    assert!(ladder.contains("  changed crates:  aterm-grid\n"));
    assert!(ladder.contains("  + dependents:    aterm-grid aterm-gui\n"));
    assert!(ladder.contains("  ok    change scope: 2 crate(s) selected against main\n"));
}

#[test]
fn a_widened_change_scoped_run_is_a_whole_tree_run_and_keeps_the_claim() {
    // The direction of failure: a narrower that cannot compute its scope does
    // MORE work, so it forfeits nothing. That only holds if the change-scope
    // stage records an `ok` — a skip there would silently downgrade every
    // widened run, which is the tier's most common outcome on a broken machine.
    let repo = FakeRepo::new();
    let widened =
        Selection::Widened("targo is absent, so the dependency graph cannot be read".into());
    let (scope, report) = changed::stage_report("main", &widened);
    assert!(scope.is_workspace());

    let ctx = repo
        .ctx(Mode::Fast, scope, false)
        .with_prelude(Some(report));
    let specs = plan::plan(&ctx);
    let green = |s: &StageSpec| {
        let mut r = Report::new(s.title.clone());
        r.pass("did the thing");
        r
    };
    let mut reports = ctx.prelude.clone();
    reports.extend(sched::run_stages(&specs, green, |_, _| {}));
    let v = verdict(Mode::Fast, &ctx.scope, false, &tally(&reports));
    assert!(v.claims_merge_contract, "{}", v.text);
    assert!(v.text.contains(MERGE_CONTRACT_SENTENCE));
}

#[test]
fn selftest_matches_the_scripts_selftest_ladder_exactly() {
    // The reference is `tools/verify.sh --selftest` on this tree: every stage
    // skipped with the same words, the harness invariants really checked, and a
    // verdict that claims nothing.
    let repo = FakeRepo::new();
    repo.with_stage2("exit 1"); // never executed under --selftest
    let (ladder, code) = repo.run(Mode::Fast, Scope::workspace(), true);

    assert_eq!(code, exit::PASS);
    assert_eq!(
        decisions(&ladder),
        [
            ("skip", "targo build --workspace (selftest: not executed)"),
            (
                "skip",
                "targo test --workspace (trustdoc) (selftest: not executed)"
            ),
            (
                "skip",
                "targo test --doc --workspace (trustdoc) (selftest: not executed)"
            ),
            (
                "skip",
                "targo test -p aterm-search --features regex (trustdoc) (selftest: not executed)"
            ),
            ("skip", "tippy lint (selftest: not executed)"),
            ("skip", "gate lint --fmt-only (selftest: not executed)"),
            ("skip", "grep_guard.sh (selftest)"),
            ("skip", "test-install-channel.sh (selftest: not executed)"),
            (
                "skip",
                "test-trust-gate-verdict.sh (selftest: not executed)"
            ),
            (
                "skip",
                "test-trust-contract-probe.sh (selftest: not executed)"
            ),
            ("skip", "test-start-compare.sh (selftest: not executed)"),
            ("skip", "license_check.sh (selftest)"),
            ("skip", "gate drift (selftest: not executed)"),
            ("skip", "gate dormant (selftest: not executed)"),
            ("skip", "gate mainloop (selftest: not executed)"),
            (
                "skip",
                "libc-oracle/run.sh (cross-cell ABI + native runtime) (selftest: not executed)"
            ),
            (
                "skip",
                "freeze-safety-gate (6 obligations) (selftest: not executed)"
            ),
            ("skip", "gate counts (selftest: not executed)"),
            (
                "ok",
                "smoke helper invariants (short socket, target path, metrics, bounded reap)"
            ),
            ("skip", "control-socket smoke (selftest)"),
            ("skip", "gui typing-pacing smoke (selftest)"),
            ("skip", "redraw conformance (selftest: not executed)"),
            // The six verdict cases the bash gate printed here too. They are the
            // selftest's actual evidence: every other row above says "not
            // executed", so without these the ladder shows a run that checked
            // nothing and still said OK.
            ("ok", "verdict[whole tree, nothing skipped]"),
            ("ok", "verdict[a stage was skipped]"),
            ("ok", "verdict[narrowed (--scope/--changed)]"),
            ("ok", "verdict[narrowed AND skipped]"),
            ("ok", "verdict[a stage FAILED]"),
            ("ok", "verdict[FAILED while narrowed]"),
        ]
    );
    // The closing line says what ran AND what did not. The second clause is the
    // load-bearing half: every stage above reads "not executed", so a reader who
    // stops at "OK" must not come away thinking the tree was verified.
    assert!(
        ladder
            .contains("VERIFY: SELFTEST OK (driver executes; verdict claim narrows with the run;")
    );
    assert!(ladder.contains("no heavy gates run — this is NOT a verification of anything)"));
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE));
}

#[test]
fn a_whole_green_run_is_the_only_thing_that_claims_the_contract() {
    // The smokes need a real terminal to answer a real socket, so the end-to-end
    // green case is built from the REAL plan with a stage runner that passes:
    // plan -> scheduler -> tally -> verdict, wired exactly as `run` wires them.
    let repo = FakeRepo::new();
    let ctx = repo.ctx(Mode::Fast, Scope::workspace(), false);
    let specs = plan::plan(&ctx);

    let green = |s: &StageSpec| {
        let mut r = Report::new(s.title.clone());
        r.pass("did the thing");
        r
    };
    let reports = sched::run_stages(&specs, green, |_, _| {});
    let t = tally(&reports);
    let v = verdict(Mode::Fast, &Scope::workspace(), false, &t);
    assert!(v.claims_merge_contract);
    assert!(v.text.contains(MERGE_CONTRACT_SENTENCE));
    assert_eq!(v.exit, exit::PASS);

    // Now skip exactly one stage — the same run, one honest absence.
    let one_skip = |s: &StageSpec| {
        let mut r = Report::new(s.title.clone());
        if s.title.starts_with("tippy") {
            r.skip("tippy lint (Trust stage2 toolchain not built)");
        } else {
            r.pass("did the thing");
        }
        r
    };
    let reports = sched::run_stages(&specs, one_skip, |_, _| {});
    let t = tally(&reports);
    let v = verdict(Mode::Fast, &Scope::workspace(), false, &t);
    assert!(
        !v.claims_merge_contract,
        "one skipped stage forfeits the whole claim"
    );
    assert!(!v.text.contains(MERGE_CONTRACT_SENTENCE));
    assert!(
        v.text
            .contains("- tippy lint (Trust stage2 toolchain not built)")
    );
}

#[test]
fn the_full_tier_reports_an_unavailable_prover_prominently_and_never_as_discharged() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    repo.script("scripts/verify-kani-proofs.sh", "exit 0");
    let (ladder, _) = repo.run(Mode::Full, Scope::workspace(), false);

    assert!(ladder.contains("=== trust-mc / Kani BMC floor (config-free parser harnesses) ==="));
    assert!(ladder.contains(
        "  NOTICE: Tier-2 trust-mc/Kani obligations were NOT RUN: trust-mc is unavailable"
    ));
    assert!(ladder.contains("(the embedded + ty tiers still ran)."));
    // The skip line names the REPAIR, not a vague future: `stages.rs` moved
    // this label from "pending build" to the install command on 2026-08-31
    // (ecb1d6691, atpkg owning the toolchain seam) and left this assertion on
    // the old wording, so the tier's own contract test was red on main.
    assert!(ladder.contains(
        "  skip  trust-mc / Kani BMC floor (tool unavailable; `aterm pkg install trust-mc`)"
    ));
    // Never described as discharged, and the skip forfeits the contract.
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE));
    assert!(!ladder.contains("verify-kani-proofs.sh (aterm-parser)"));
    // …and the --fast ladder never mentions the tier at all.
    let (fast, _) = repo.run(Mode::Fast, Scope::workspace(), false);
    assert!(!fast.contains("trust-mc"));
    assert!(!fast.contains("differential oracle"));
}

#[test]
fn the_pure_guards_do_not_wait_for_the_build() {
    // The reason this is a program and not a script: on a real tree the build is
    // minutes and the guards are milliseconds.
    let repo = FakeRepo::new();
    let ctx = repo.ctx(Mode::Fast, Scope::workspace(), false);
    let specs = plan::plan(&ctx);
    let slow_build = |s: &StageSpec| {
        if s.lane == Lane::MainTarget {
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        Report::new(s.title.clone())
    };
    let start = std::time::Instant::now();
    let reports = sched::run_stages(&specs, slow_build, |_, _| {});
    let elapsed = start.elapsed();
    assert_eq!(reports.len(), specs.len());
    let main_stages = specs.iter().filter(|s| s.lane == Lane::MainTarget).count() as u64;
    assert!(
        elapsed < std::time::Duration::from_millis(60 * main_stages + 400),
        "the main lane is serial, everything else overlaps it: {elapsed:?}"
    );
}

#[test]
fn nothing_outside_the_verdict_may_spell_the_claim() {
    // A stage label that happened to contain the sentence would be a second way
    // to claim the contract, and nobody would ever look for it there.
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    for (mode, scope) in [
        (Mode::Fast, Scope::workspace()),
        (Mode::Full, Scope::workspace()),
        (Mode::Fast, Scope::crate_only("aterm-grid")),
    ] {
        let (ladder, _) = repo.run(mode, scope, false);
        let in_ladder = ladder
            .lines()
            .filter(|l| l.contains(MERGE_CONTRACT_SENTENCE))
            .collect::<Vec<_>>();
        assert!(in_ladder.is_empty(), "the claim leaked into: {in_ladder:?}");
    }
}

#[test]
fn the_root_is_found_by_its_markers_not_by_the_binarys_location() {
    let repo = FakeRepo::new();
    let deep = repo.root.join("crates/aterm-verify/src");
    fs::create_dir_all(&deep).expect("mkdir");
    assert_eq!(
        aterm_verify::locate_root(&deep).as_deref(),
        Some(repo.root.as_path())
    );
    assert_eq!(aterm_verify::locate_root(Path::new("/")), None);
}
