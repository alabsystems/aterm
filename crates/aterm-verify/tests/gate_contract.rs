// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The gate, end to end, over a synthetic repo.
//!
//! `tools/verify.sh` was 900 lines of bash that decided whether code may land,
//! and nothing had ever tested it. These are the tests that would have caught the
//! false green it was found printing: a whole run is driven here — plan,
//! scheduler, ladder, tally, verdict — and asserted on as text, because the text
//! IS the contract a reviewer reads.
//!
//! UNIX-PINNED, AT THE TARGET RATHER THAN PER TEST, for the same reason as
//! `environment_contract.rs`: the synthetic repo is built out of `#!/bin/sh`
//! stage scripts made runnable with `chmod 0755`, and a ladder driven by shell
//! stubs has no Windows spelling that would still be the same contract. What
//! the attribute buys is that the file COMPILES off unix, so the rest of this
//! crate's test targets — `profile_pin.rs` and every unit test in `src/` —
//! are compiled for a non-unix target instead of being lost with it.
#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use aterm_verify::cli::Mode;
use aterm_verify::ladder::{Report, Tally, tally};
use aterm_verify::plan::{Lane, StageId, StageSpec, Tier};
use aterm_verify::verdict::{MERGE_CONTRACT_SENTENCE, verdict};
use aterm_verify::{Ctx, EnvSnapshot, Scope, exit, mktemp_dir, plan, sched, stages};

/// A repo-shaped directory: the helper scripts the gate calls, all passing.
struct FakeRepo {
    root: PathBuf,
    stage2: PathBuf,
    scratch: PathBuf,
    /// The fixture's own HOME ([`common::checker_home`]).
    home: PathBuf,
    /// The control socket the answering smoke's `aterm-gui` links to, held
    /// listening for the fixture's life.
    ctl_sock: PathBuf,
    _listener: std::os::unix::net::UnixListener,
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
        let home = base.join("home");
        common::checker_home(&home);
        fs::create_dir_all(root.join("tools/perf-arena")).expect("mkdir");
        fs::create_dir_all(root.join("scripts")).expect("mkdir");
        fs::create_dir_all(root.join("libc-oracle")).expect("mkdir");
        fs::write(root.join("Cargo.toml"), b"[workspace]\n").expect("write");
        let ctl_sock = base.join("ctl.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&ctl_sock)
            .expect("bind the fixture's control socket");
        let me = Self {
            root,
            stage2,
            scratch,
            home,
            ctl_sock,
            _listener,
        };
        me.script("tools/verify.sh", "exit 0");
        me.script("tools/grep_guard.sh", "echo 'GUARD: PASS'; exit 0");
        me.script("tools/license_check.sh", "echo 'LICENSE: PASS'; exit 0");
        // The export content scan, by its stage's own name: a stand-in here,
        // and the REAL scan over a real git tree in the tests of its own
        // (`the_export_content_stage_…`).
        me.script(
            &format!("tools/{}", aterm_verify::stages::EXPORT_CONTENT_SCRIPT),
            "echo 'EXPORT CONTENT: PASS — a stand-in'; exit 0",
        );
        for name in aterm_verify::stages::DELIVERY_SUITES {
            me.script(&format!("tools/{name}"), "exit 0");
        }
        // The live lanes, from their roster for the same reason as the two below.
        for name in aterm_verify::stages::LIVE_ATERM_SUITES {
            me.script(&format!("tools/{name}"), "exit 0");
        }
        // …and the atpkg end-to-end pack, by its stage's own name. (A hand-written
        // copy of a roster drifted the moment the roster grew: every fixture here
        // reported the new suite `missing or not executable`, 2026-09-17.)
        me.script(
            &format!("tools/{}", aterm_verify::stages::ATPKG_DRIVEN_SUITE),
            "exit 0",
        );
        me.script("tools/test-trust-contract-probe.sh", "exit 0");
        for name in aterm_verify::stages::TRUST_LANE_SCRIPTS {
            // The lane says what it verified on a `GATED:` line, which the stage
            // reads; the self-test says nothing.
            let body = if name == "trust-gate-all.sh" {
                "echo 'GATED: 1 of 1 selected; not verified: none'; exit 0"
            } else {
                "exit 0"
            };
            me.script(&format!("tools/{name}"), body);
        }
        me.script("tools/perf-arena/test-start-compare.sh", "exit 0");
        me.script("libc-oracle/run.sh", "exit 0");
        // Every binary the gate builds and then DRIVES, from the stages' own
        // names: present and passing by default, so an unrelated test never
        // reads a missing binary as a finding. `driver_stub` re-writes one for
        // the tests about what its exit code means.
        for rel in driven_stubs() {
            me.driver_stub(&rel, 0);
        }
        // And the one `aterm` binary the live lanes are handed, in the driver
        // lane's dir where their stage's own build leaves it.
        fs::create_dir_all(me.root.join("target-drivers/debug")).expect("mkdir");
        me.script("target-drivers/debug/aterm", "echo 'aterm: stub'");
        me
    }

    /// A stand-in for a binary the driver lane builds and a stage then DRIVES
    /// — `rel` under `target-drivers/debug/` — that exits `code`. What these
    /// tests exercise is the STAGE's reading of the code (the redraw harness's
    /// `0`/`1`/`2`, each objc driver's own contract), never the drive itself.
    fn driver_stub(&self, rel: &str, code: i32) -> &Self {
        self.driver_script(rel, &format!("echo '{rel}: stub'; exit {code}"))
    }

    /// A stand-in driven binary with its own `body`.
    fn driver_script(&self, rel: &str, body: &str) -> &Self {
        let path = format!("target-drivers/debug/{rel}");
        if let Some(dir) = self.root.join(&path).parent() {
            fs::create_dir_all(dir).expect("mkdir");
        }
        self.script(&path, body);
        self
    }

    fn script(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("write");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// A stage2 whose driver also produces the two binaries the headless smoke
    /// drives ([`common::answering_smoke`]), so the smoke — launch, poll, burst,
    /// teardown — runs for real in a test.
    fn with_answering_smoke(&self) -> &Self {
        self.with_stage2(&format!(
            "echo \"argv: $*\"\n{}\nexit 0",
            common::answering_smoke(&self.ctl_sock)
        ))
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

    fn ctx(&self, mode: Mode, scope: Scope) -> Ctx {
        self.ctx_with(mode, scope, |_| {})
    }

    /// [`Self::ctx`] with one last say over the environment the stages read —
    /// for the cases that MEASURE a variable's effect instead of being at its
    /// mercy (`a_callers_job_count_caps_the_side_lane_child_it_reaches`).
    fn ctx_with(&self, mode: Mode, scope: Scope, tweak: impl FnOnce(&mut EnvSnapshot)) -> Ctx {
        common::fixture_ctx(
            &self.root,
            &self.stage2,
            &self.scratch,
            &self.home,
            mode,
            scope,
            tweak,
        )
    }

    fn run(&self, mode: Mode, scope: Scope) -> (String, i32) {
        let ctx = self.ctx(mode, scope);
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

/// Every driven binary's path under `target-drivers/debug/`: the redraw
/// harness and the objc drivers' examples, named by the stages themselves.
fn driven_stubs() -> Vec<String> {
    std::iter::once(stages::REDRAW_CONFORMANCE_BIN.to_string())
        .chain(stages::OBJC_DRIVES.iter().map(|d| objc_stub(d.example)))
        .collect()
}

/// An objc driver's built example, under `target-drivers/debug/`.
fn objc_stub(example: &str) -> String {
    format!("examples/{example}")
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

/// A VERDICT IS READABLE WHEN IT IS DECIDED, NOT WHEN ITS TURN TO PRINT COMES
/// (2026-09-23). The ladder prints in declared order, so a guard that FAILED in
/// its first second stayed unread until the build and test stages ahead of it
/// printed — up to an hour on a real run. Every stage's finish line, with its
/// outcome word, now goes to the run's log as it happens. Measured here with a
/// driver that takes a second per call and a grep guard that fails at once:
/// in the LOG the guard's `FAIL` comes before the build's finish; in the LADDER
/// (the negative control, which is and stays in declared order) the build's
/// block still comes first.
///
/// THE BUILD WAITS FOR THE GUARD'S LINE, NOT FOR A SECOND (2026-09-24). The
/// driver's one-second sleep raced the guard's stage on the wall clock: inside
/// a real gate's test stage the guard took 6.0 s and the build 4.4 s, and the
/// ordering the test asserts went red on a product that logged as it should.
/// The workspace build now blocks until the guard's `FAIL` is in the log,
/// bounded: a gate that logs as stages finish always lets the build finish
/// second. One that holds finish lines back at all — in declared order, or in
/// finish order but written only after the stages are over — never shows the
/// guard's line while the build waits, so the build FAILS at its bound and the
/// `— ok (` lookup below names it. The ordering assertion alone cannot tell a
/// log written late in finish order from one written live.
#[test]
fn every_stage_finish_is_logged_with_its_outcome_as_it_happens() {
    let repo = FakeRepo::new();
    let log_path = repo.scratch.join("progress.log");
    repo.with_stage2(&format!(
        "case \"$*\" in\n  *'--no-run'*)\n    i=0\n    \
         while ! grep -qF 'finish grep guards and license headers — FAIL' '{log}' && [ \"$i\" -lt 600 ]; do\n      \
         sleep 0.05; i=$((i + 1))\n    done\n    \
         grep -qF 'finish grep guards and license headers — FAIL' '{log}' || \
         {{ echo 'the guard FAIL was never readable while the build ran'; exit 1; }} ;;\nesac\nexit 0",
        log = log_path.display()
    ));
    repo.script("tools/grep_guard.sh", "echo 'GUARD: FAIL'; exit 1");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("the log opens");
    let ctx = repo
        .ctx(Mode::Fast, Scope::workspace())
        .with_progress_log(Some(log));
    let mut out: Vec<u8> = Vec::new();
    let code = aterm_verify::run(&ctx, &mut out).expect("the ladder is writable");
    let ladder = String::from_utf8(out).expect("utf-8");
    assert_eq!(code, exit::FAILED, "{ladder}");

    let logged = fs::read_to_string(&log_path).expect("the log was written");
    let at = |text: &str, needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("no {needle:?} in:\n{text}"))
    };
    let guard = at(
        &logged,
        "verify: finish grep guards and license headers — FAIL (",
    );
    let build = at(&logged, "verify: finish test compile (--workspace) — ok (");
    assert!(
        guard < build,
        "the guard's FAIL must be readable before the slow compile finishes:\n{logged}"
    );
    // Every planned stage finished, and said how.
    for spec in plan::plan(&ctx) {
        assert!(
            logged.contains(&format!("verify: finish {} — ", spec.title)),
            "{} never logged its finish:\n{logged}",
            spec.title
        );
    }
    // The negative control: the ladder itself is unchanged, in declared order.
    assert!(
        at(&ladder, "=== test compile (--workspace) ===")
            < at(&ladder, "=== grep guards and license headers ==="),
        "{ladder}"
    );
    assert!(
        !ladder.contains("verify: finish "),
        "stdout stays the ladder: {ladder}"
    );
}

#[test]
fn the_ladder_prints_every_stage_in_the_declared_order_however_they_ran() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let (ladder, _) = repo.run(Mode::Full, Scope::workspace());

    let ctx = repo.ctx(Mode::Full, Scope::workspace());
    let mut expected: Vec<String> = plan::plan(&ctx).into_iter().map(|s| s.title).collect();
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
    // test compile FAILS honestly, and the verdict says nothing was decided.
    let repo = FakeRepo::new();
    let (ladder, code) = repo.run(Mode::Fast, Scope::workspace());

    assert_eq!(code, exit::COULD_NOT_RUN);
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE));
    assert!(ladder.contains("VERIFY: COULD NOT RUN (mode=fast scope=workspace) — DO NOT merge"));
    let failed = labels_with(&ladder, "FAIL");
    assert!(
        failed.iter().any(|l| l.starts_with("targo not found")),
        "the missing driver is named: {failed:?}"
    );
    // …and the stages that need it skip by name, so the verdict can list them.
    for needed in [
        "targo test (no targo)",
        "targo test --doc (no targo)",
        "L0 temporal-safety gate (no targo)",
        "driver builds (no targo)",
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
    let (ladder, code) = repo.run(Mode::Fast, Scope::workspace());

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
fn a_failing_driver_fails_every_stage_that_drives_it_and_nothing_else() {
    let repo = FakeRepo::new();
    repo.with_stage2("echo 'error: unknown unstable option: `trust-verify`' >&2; exit 1");
    let (ladder, code) = repo.run(Mode::Fast, Scope::workspace());

    assert_eq!(code, exit::FAILED);
    for driven in [
        "targo test --workspace --no-run (trustdoc)",
        "targo test --doc --workspace (trustdoc)",
        "targo test --workspace --lib (trustdoc) -- launchd_copy_tests::",
        "gate lint --fmt-only",
        "gate forge",
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
    // The test run's second child never starts after a failed compile — as
    // the single child ran no test after one — and the ladder says so without
    // calling it a skip: a COULD NOT RUN row, counted (2026-09-27, third
    // review: a compile red main's receipt excuses must not excuse the tests
    // it kept from running).
    assert!(
        ladder.contains(
            "  FAIL  targo test --workspace --tests (trustdoc) — not run: the compile above failed"
        ),
        "{ladder}"
    );
    assert!(
        !decisions(&ladder)
            .iter()
            .any(|(_, l)| *l == "targo test --workspace --tests (trustdoc)"),
        "{ladder}"
    );
    // Likewise the atpkg pack: its atpkg build fails, so its suite never
    // starts against whatever binary an earlier build left behind.
    assert!(
        labels_with(&ladder, "FAIL")
            .iter()
            .any(|l| l == stages::ATPKG_BUILD_LABEL),
        "{ladder}"
    );
    assert!(
        ladder.contains(&format!(
            "  FAIL  {} — not run: the atpkg build above failed",
            stages::ATPKG_DRIVEN_SUITE
        )),
        "{ladder}"
    );
}

#[test]
fn a_scoped_run_narrows_the_driver_and_is_refused_the_contract() {
    let repo = FakeRepo::new();
    // Everything green — including a control-socket smoke that really launches
    // under every precondition it promises, really types and really tears down —
    // so the only thing standing between this run and the merge-contract
    // sentence is that it was narrowed.
    repo.with_answering_smoke();
    let (ladder, code) = repo.run(Mode::Fast, Scope::crate_only("aterm-grid"));

    assert_eq!(code, exit::PASS, "narrow is not failure: {ladder}");
    assert!(ladder.contains("argv: --unverified test -p aterm-grid"));
    assert!(ladder.contains("argv: --unverified test --doc -p aterm-grid"));
    assert!(
        !ladder.contains("--workspace"),
        "nothing whole-tree was driven"
    );
    assert!(
        ladder.contains("  ok    smoke: aterm-ctl cursor -> OK 0 0 1 blinking_block"),
        "{ladder}"
    );
    assert!(
        ladder.contains(
            "  ok    smoke: 30/30 keys accepted over the control socket, no lost-wake heals"
        ),
        "{ladder}"
    );

    assert!(
        !ladder.contains(MERGE_CONTRACT_SENTENCE),
        "THE regression: a scoped run claiming it all"
    );
    assert!(ladder.contains("NOT the merge contract"));
    assert!(ladder.contains(
        "- scoped to -p aterm-grid: the per-crate test, doctest and lint stages covered \
             no other crate"
    ));
    // The pacing smoke is the MEASURE tier's (2026-09-26): this run did not
    // plan it, so it is no skip — the verdict names it as not part of the
    // contract instead.
    assert!(!ladder.contains("gui smoke (--skip-gui-smoke)"), "{ladder}");
    assert!(
        ladder.contains("MEASURE tier: not part of the merge contract")
            && ladder.contains("      - gui typing-pacing smoke\n"),
        "{ladder}"
    );
}

/// A BURST THAT BARELY HAPPENED PROVES NO WAKE WAS LOST. A heal is booked only
/// when a later echo finds a lost wake's latch past its 100 ms expiry, so one
/// accepted key cannot register one: the stand-in instance takes the first
/// `send` and refuses the rest, and `wake_heals=0` after it is no pass. The
/// round trip before the burst keeps its own row, whatever the burst does.
#[test]
fn a_burst_the_instance_mostly_refused_proves_no_wake_was_lost() {
    let repo = FakeRepo::new();
    let every_send = "  send) echo \"OK\" ;;";
    let smoke = common::answering_smoke(&repo.ctl_sock);
    assert!(smoke.contains(every_send), "the stand-in's send arm moved");
    let first_send_only = smoke.replace(
        every_send,
        "  send) test -e \"$XDG_RUNTIME_DIR/sent\" && { echo 'ERR busy'; exit 1; }\n    \
         : >\"$XDG_RUNTIME_DIR/sent\"; echo OK ;;",
    );
    repo.with_stage2(&format!("echo \"argv: $*\"\n{first_send_only}\nexit 0"));
    let (ladder, code) = repo.run(Mode::Fast, Scope::crate_only("aterm-grid"));

    assert_eq!(code, exit::FAILED, "{ladder}");
    assert!(
        ladder.contains("  ok    smoke: aterm-ctl cursor -> OK 0 0 1 blinking_block"),
        "{ladder}"
    );
    assert!(
        labels_with(&ladder, "FAIL")
            .iter()
            .any(|l| l
                == "smoke: only 1/30 keys accepted (< 15), too few for a lost wake to register"),
        "{ladder}"
    );
    assert!(!ladder.contains("no lost-wake heals"), "{ladder}");
}

/// Every driven stage reads its driver's EXIT CODE, and nothing that decided
/// nothing is ever green.
///
/// The regressions these stages exist for live one layer down — the redraw
/// harness exits 1 when the production `EventLoopProxy` is dropped; the objc
/// drivers exit 1 on a retyped argument or a dropped protocol in the ported
/// classes, both of which left `cargo build` at 0. What is tested HERE is the
/// layer that was missing entirely: that something LOOKS at the answer. Every
/// row files `1` as a finding about the tree, and every code that means the
/// driver could not decide (`2`: no event loop, no window server, no input
/// context; the toolbar's `3`: its watchdog, because a context menu that
/// really popped would never return) as could-not-run that still fails the
/// run — a headless box reading any of them as a pass would restore the
/// identical silence in a new place. The audit's `3` is a weaker PASS.
///
/// The event drive is the one reading that differs: v0.72.0 died by `SIGABRT`
/// on the first mouse move and the driver reproduces that shape, so its `3`
/// (the trapped abort) and an untrapped signal death are THE finding, never
/// could-not-run — the siblings' reading would file the crash under "decided
/// nothing".
///
/// One row per distinct reading: the redraw harness (every unix) and, on
/// macOS, the objc drives stage through a plain driver, the audit, the
/// toolbar and the event drive — every other objc driver passing, so the
/// stage's verdict is that one driver's.
#[test]
fn every_driven_stage_reads_its_exit_code_and_nothing_undecided_is_green() {
    /// A driver outcome beyond `1`.
    enum Beyond {
        Exit(i32),
        /// A stand-in that dies by an untrapped signal. It dies by `SIGKILL`,
        /// not the v0.72.0 crash's `SIGABRT`: the ladder reads every signal
        /// death alike, and a `SIGABRT` made macOS write a crash report for
        /// the stub's shell on every run (grep_guard B16).
        Signal,
    }
    /// What a [`Beyond`] outcome decides.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Reads {
        Pass,
        CouldNotRun,
        Finding,
    }
    struct Row {
        id: StageId,
        /// The label the driver's verdict line carries.
        label: &'static str,
        /// The driven binary, under `target-drivers/debug/`.
        stub: String,
        /// Outcomes beyond `1`, what each decides, and the word the line prints.
        beyond: &'static [(Beyond, Reads, &'static str)],
    }
    let mut rows = vec![Row {
        id: StageId::RedrawConformance,
        label: "aterm-redraw-conformance",
        stub: stages::REDRAW_CONFORMANCE_BIN.to_string(),
        beyond: &[(Beyond::Exit(2), Reads::CouldNotRun, "NOT RUN")],
    }];
    if cfg!(target_os = "macos") {
        rows.extend([
            Row {
                id: StageId::ObjcDrives,
                label: "objc_window_drive",
                stub: objc_stub("objc_window_drive"),
                beyond: &[(Beyond::Exit(2), Reads::CouldNotRun, "NOT RUN")],
            },
            // `3` is a PASS that claims less than `0`, in its own words.
            Row {
                id: StageId::ObjcDrives,
                label: "objc_live_class_audit",
                stub: objc_stub("objc_live_class_audit"),
                beyond: &[
                    (Beyond::Exit(2), Reads::CouldNotRun, "NOT RUN"),
                    (Beyond::Exit(3), Reads::Pass, "every registered row agrees"),
                ],
            },
            // FOUR codes: the drive enters `-mouseDown:` IMPs directly, and a
            // hang reaching the ladder as a generic timeout would be a stage
            // that decided nothing while looking busy.
            Row {
                id: StageId::ObjcDrives,
                label: "objc_toolbar_drive",
                stub: objc_stub("objc_toolbar_drive"),
                beyond: &[(Beyond::Exit(3), Reads::CouldNotRun, "HUNG")],
            },
            Row {
                id: StageId::ObjcDrives,
                label: "objc_event_drive",
                stub: objc_stub("objc_event_drive"),
                beyond: &[
                    (Beyond::Exit(3), Reads::Finding, "ABORTED"),
                    (Beyond::Signal, Reads::Finding, "ABORTED"),
                ],
            },
        ]);
    }

    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    for row in rows {
        let name = row.label;
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == row.id)
            .unwrap_or_else(|| panic!("{name}: the stage is planned"));

        repo.driver_stub(&row.stub, 0);
        let r = stages::run_stage(&ctx, &spec);
        assert_eq!(
            tally(std::slice::from_ref(&r)),
            Tally::default(),
            "{name}: a clean driver leaves the run clean"
        );
        assert!(
            r.render().contains(&format!("  ok    {name}: ")),
            "{}",
            r.render()
        );

        repo.driver_stub(&row.stub, 1);
        let t = tally(&[stages::run_stage(&ctx, &spec)]);
        assert_eq!(
            t.gate_failures.len(),
            1,
            "{name}: exit 1 is a finding about the tree"
        );
        assert_eq!(t.could_not_run.len(), 0, "{name}");

        for (beyond, reads, words) in row.beyond {
            let what = match beyond {
                Beyond::Exit(code) => {
                    repo.driver_stub(&row.stub, *code);
                    format!("exit {code}")
                }
                Beyond::Signal => {
                    repo.driver_script(
                        &row.stub,
                        "echo 'stub about to die by a signal'; kill -KILL $$",
                    );
                    "an untrapped signal death".to_string()
                }
            };
            let r = stages::run_stage(&ctx, &spec);
            let t = tally(std::slice::from_ref(&r));
            let (tag, findings, undecided) = match reads {
                Reads::Pass => ("ok  ", 0, 0),
                Reads::CouldNotRun => ("FAIL", 0, 1),
                Reads::Finding => ("FAIL", 1, 0),
            };
            assert_eq!(
                t.gate_failures.len(),
                findings,
                "{name}: {what} reads {reads:?}"
            );
            assert_eq!(
                t.could_not_run.len(),
                undecided,
                "{name}: {what} reads {reads:?}"
            );
            assert_eq!(
                t.skipped(),
                0,
                "{name}: {what} is above all not a quiet skip"
            );
            assert_eq!(t.failed(), *reads != Reads::Pass, "{name}: {what}");
            assert!(
                r.render().contains(&format!("  {tag}  {name}: {words}")),
                "{}",
                r.render()
            );
        }
        repo.driver_stub(&row.stub, 0);
    }
}

/// THE OBJC DRIVES STAGE RUNS EVERY ROW, WHATEVER ONE ROW DID. A driver whose
/// build failed and a driver that found a defect each leave their own rows,
/// and every other driver still runs and says so.
#[test]
fn the_objc_drives_run_every_row_after_a_failure() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let [first, second, ..] = stages::OBJC_DRIVES;
    let repo = FakeRepo::new();
    repo.with_stage2(&format!(
        "case \"$*\" in *{}*) {COMPILE_RED} ;; esac\nexit 0",
        first.example
    ));
    repo.driver_stub(&objc_stub(second.example), 1);
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::ObjcDrives)
        .expect("planned");
    let r = stages::run_stage(&ctx, &spec);
    let t = tally(std::slice::from_ref(&r));
    let ladder = r.render();
    assert_eq!(
        t.gate_failures.len(),
        2,
        "the build and the finding: {ladder}"
    );
    assert!(
        t.could_not_run.iter().any(|c| c.starts_with(&format!(
            "{} — not run: the build above failed",
            first.example
        ))),
        "{ladder}"
    );
    for drive in &stages::OBJC_DRIVES[2..] {
        assert!(
            ladder.contains(&format!("  ok    {}: ", drive.example)),
            "{} ran after the failures: {ladder}",
            drive.example
        );
    }
}

#[test]
fn a_whole_green_run_is_the_only_thing_that_claims_the_contract() {
    // The smokes need a real terminal to answer a real socket, so the end-to-end
    // green case is built from the REAL plan with a stage runner that passes:
    // plan -> scheduler -> tally -> verdict, wired exactly as `run` wires them.
    let repo = FakeRepo::new();
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let specs = plan::plan(&ctx);

    let green = |s: &StageSpec| {
        let mut r = Report::new(s.title.clone());
        r.pass("did the thing");
        r
    };
    let reports = sched::run_stages(&specs, green, |_, _| {});
    let t = tally(&reports);
    let v = verdict(Mode::Fast, &Scope::workspace(), &t);
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
    let v = verdict(Mode::Fast, &Scope::workspace(), &t);
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
fn the_pure_guards_do_not_wait_for_the_main_lane() {
    // The reason this is a program and not a script: on a real tree the build is
    // minutes and the guards are milliseconds.
    //
    // A RENDEZVOUS, not a stopwatch (the load-sensitive test audit of
    // 2026-09-27). Every main-lane stage holds until every pure guard has
    // finished, so the guards must be able to run while the build is in flight:
    // a scheduler that queued them behind the build deadlocks here, and the
    // build's bounded wait turns that into a failure. The old form timed the
    // run against 60 ms per main stage + 400 ms of slack, which a loaded gate's
    // oversleeps could exceed, and which a guards-behind-the-build scheduler
    // passed anyway: the guards do no work, so serialising them cost nothing a
    // clock could see.
    let repo = FakeRepo::new();
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let specs = plan::plan(&ctx);
    let pure = specs.iter().filter(|s| s.lane == Lane::Pure).count();
    assert!(pure > 0, "no pure guards in the plan: nothing to overlap");
    let finished = std::sync::Mutex::new(0_usize);
    let guard_done = std::sync::Condvar::new();
    let starved = std::sync::atomic::AtomicBool::new(false);
    let build_holds_for_the_guards = |s: &StageSpec| {
        match s.lane {
            // Once one main stage has starved, the rest need not wait 30 s each
            // to fail the same way.
            Lane::MainTarget if !starved.load(std::sync::atomic::Ordering::SeqCst) => {
                let count = finished.lock().expect("pure count");
                let (_count, wait) = guard_done
                    .wait_timeout_while(count, std::time::Duration::from_secs(30), |done| {
                        *done < pure
                    })
                    .expect("pure count");
                if wait.timed_out() {
                    starved.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            Lane::Pure => {
                *finished.lock().expect("pure count") += 1;
                guard_done.notify_all();
            }
            _ => {}
        }
        Report::new(s.title.clone())
    };
    let reports = sched::run_stages(&specs, build_holds_for_the_guards, |_, _| {});
    assert_eq!(reports.len(), specs.len());
    assert!(
        !starved.load(std::sync::atomic::Ordering::SeqCst),
        "the main lane held 30 s for {pure} pure guards that could not run beside it: \
         they waited for the build"
    );
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A DRIVEN SUITE DRIVES THE BINARY ITS STAGE JUST BUILT — one law, end to end,
/// for every row whose suite drives a binary its own stage builds (in place of
/// a test per row since 2026-09-27): the sealed rung (`--full`), the atpkg
/// end-to-end pack, the foreground handback and the render desync lane. Over a
/// fixture whose driver
/// writes a FRESH binary into the lane's dir, with a STALE one in
/// `<root>/target/debug` — each suite's own fallback — and each suite
/// resolving its binary as its harness or script does:
///  * the build runs first, in the driver lane's dir at that lane's job cap
///    (the stand-in driver refuses anything else);
///  * the suite drives the fresh binary and never the stale one;
///  * a failed build runs no suite, and the ladder says `not run:` — no skip;
///  * a failed build and a failed suite each count once.
///
/// Each row's NEGATIVE CONTROL is its suite run the way it ran before its row
/// existed — alone, handed nothing — which drives the stale binary. Off macOS
/// the handback and the render desync lane are each one named skip that builds
/// nothing (neither lane has been measured anywhere else).
#[test]
fn every_driven_suite_drives_the_binary_its_stage_just_built() {
    struct Row {
        id: StageId,
        mode: Mode,
        /// The build child's argv, as the stand-in driver sees it.
        build: &'static str,
        /// What that build writes under the lane's `debug/`, and the suite's
        /// own fallback under `<root>/target/debug/`.
        bin: &'static str,
        /// The suite, as its `not run:` line names it.
        suite: &'static str,
    }
    let rows = [
        Row {
            id: StageId::SealedLane,
            mode: Mode::Full,
            build: "--unverified build -q -p aterm-gui -p aterm-ctl",
            bin: "aterm-gui",
            suite: "targo test -p aterm-link --features sealed --test two_nodes_sealed",
        },
        Row {
            id: StageId::AtpkgTooling,
            mode: Mode::Fast,
            build: "--unverified build -q -p atpkg",
            bin: "atpkg",
            suite: stages::ATPKG_DRIVEN_SUITE,
        },
        Row {
            id: StageId::ForegroundHandback,
            mode: Mode::Fast,
            build: "--unverified build -q -p aterm --bin aterm",
            bin: "aterm",
            suite: stages::FOREGROUND_HANDBACK_SUITE,
        },
        Row {
            id: StageId::RenderDesync,
            mode: Mode::Fast,
            build: "--unverified build -q -p aterm --bin aterm",
            bin: "aterm",
            suite: stages::RENDER_DESYNC_SUITE,
        },
    ];
    for row in rows {
        for (build_exit, suite_exit) in [(0, 0), (19, 0), (0, 23)] {
            let what = format!("{:?} ({build_exit}, {suite_exit})", row.id);
            let repo = FakeRepo::new();
            let trace = repo.scratch.join("driven-order");
            let target = repo.root.join("target-drivers");
            // FakeRepo seeds a driven `aterm` for the whole-ladder tests: what
            // this stage drives must be what its OWN build left.
            fs::remove_file(target.join("debug/aterm")).expect("unseed the driven aterm");
            let root = sh_quote(&repo.root.display().to_string());
            let tr = sh_quote(&trace.display().to_string());
            let bin = row.bin;
            repo.with_stage2(&format!(
                r#"test "$CARGO_TARGET_DIR" = {target} || exit 70
test "$CARGO_BUILD_JOBS" = 8 || exit 71
case "$*" in
  '{build}')
    echo build >> {tr}
    test {build_exit} = 0 || exit {build_exit}
    mkdir -p "$CARGO_TARGET_DIR/debug"
    printf '#!/bin/sh\necho fresh\n' > "$CARGO_TARGET_DIR/debug/{bin}"
    chmod 755 "$CARGO_TARGET_DIR/debug/{bin}"
    ;;
  '--unverified test -p aterm-link --features sealed --test two_nodes_sealed --no-fail-fast')
    bin="$CARGO_TARGET_DIR/debug/aterm-gui"
    [ -x "$bin" ] || bin={root}/target/debug/aterm-gui
    echo "suite $("$bin")" >> {tr}
    exit {suite_exit}
    ;;
  *) exit 74 ;;
esac
"#,
                target = sh_quote(&target.display().to_string()),
                build = row.build,
            ));
            // The suites' own resolution: `$ATPKG`, else the checkout's
            // `target/debug/atpkg`; `--binary`, else its `target/debug/aterm`.
            repo.script(
                &format!("tools/{}", stages::ATPKG_DRIVEN_SUITE),
                &format!(
                    "bin=\"$ATPKG\"\n[ -n \"$bin\" ] || bin={root}/target/debug/atpkg\n\
                     echo \"suite $(\"$bin\")\" >> {tr}\nexit {suite_exit}"
                ),
            );
            // The handback and render desync lanes take the binary the same
            // way; each refuses an unknown argument with its own not-run code.
            for (suite, not_run) in [
                (stages::FOREGROUND_HANDBACK_SUITE, 2),
                (stages::RENDER_DESYNC_SUITE, 3),
            ] {
                repo.script(
                    &format!("tools/{suite}"),
                    &format!(
                        "BIN={root}/target/debug/aterm\n\
                         while [ $# -gt 0 ]; do case $1 in --binary) BIN=$2; shift 2 ;; *) exit {not_run} ;; esac; done\n\
                         echo \"suite $(\"$BIN\")\" >> {tr}\nexit {suite_exit}"
                    ),
                );
            }
            fs::create_dir_all(repo.root.join("target/debug")).expect("shared target");
            repo.script(&format!("target/debug/{bin}"), "echo stale");

            // The negative control: the suite alone, handed nothing.
            let mut control = if row.id == StageId::SealedLane {
                let mut c = std::process::Command::new(repo.stage2.join("targo"));
                c.args(stages::sealed_lane_args())
                    .env("CARGO_TARGET_DIR", &target)
                    .env("CARGO_BUILD_JOBS", "8");
                c
            } else {
                std::process::Command::new(repo.root.join("tools").join(row.suite))
            };
            let old = control
                .current_dir(&repo.root)
                .env_remove("ATPKG")
                .output()
                .expect("the suite as it ran before its row");
            assert_eq!(old.status.code(), Some(suite_exit), "{what}: {old:?}");
            assert_eq!(
                fs::read_to_string(&trace).expect("the control ran"),
                "suite stale\n",
                "{what}: the control must demonstrate the stale fallback"
            );
            fs::write(&trace, "").expect("reset the trace");

            let ctx = repo.ctx(row.mode, Scope::workspace());
            let spec = plan::plan(&ctx)
                .into_iter()
                .find(|s| s.id == row.id)
                .unwrap_or_else(|| panic!("{what}: the stage is planned"));
            let report = stages::run_stage(&ctx, &spec);
            let rendered = report.render();
            let measured = fs::read_to_string(&trace).expect("the trace");
            let result = tally(std::slice::from_ref(&report));
            if matches!(row.id, StageId::ForegroundHandback | StageId::RenderDesync)
                && !cfg!(target_os = "macos")
            {
                assert_eq!(measured, "", "{what}: {rendered}");
                assert_eq!(result.skipped(), 1, "{what}: {rendered}");
                assert!(!result.failed(), "{what}: {rendered}");
                continue;
            }
            assert_eq!(
                measured,
                if build_exit == 0 {
                    "build\nsuite fresh\n"
                } else {
                    "build\n"
                },
                "{what}: {rendered}"
            );
            // The suite a failed build kept from running decided nothing, and
            // the tally hears it: a COULD NOT RUN row of its own (2026-09-27,
            // third review).
            assert_eq!(
                result.could_not_run.len(),
                usize::from(build_exit != 0),
                "{what}: {rendered}"
            );
            assert_eq!(
                result.gate_failures.len(),
                usize::from(build_exit != 0 || suite_exit != 0),
                "{what}: {rendered}"
            );
            assert_eq!(
                rendered.contains(&format!("  FAIL  {} — not run: ", row.suite)),
                build_exit != 0,
                "{what}: {rendered}"
            );
        }
    }
}

/// THE CEILING, end to end: a caller's `CARGO_BUILD_JOBS` reaches the side
/// lane's own child as the smaller of the two, not as the lane's cap and not as
/// the caller's value.
///
/// `lane_jobs`'s unit tests (stages.rs) prove the arithmetic; this proves the
/// wiring — that the number a real stage puts in a real child's environment is
/// the capped one. It is the test the 2026-09-16 gate wanted: the ceiling had
/// landed, and the only thing that noticed it inside a run was two fixtures
/// failing at exit 71 (see the `cargo_build_jobs` pin in `ctx_with`).
#[test]
fn a_callers_job_count_caps_the_side_lane_child_it_reaches() {
    // 2 is below the driver lane's cap of 8, so the child must see 2; the
    // driven-binary law above pins the uncapped 8 through the same code path.
    // The sealed rung is `--full`'s.
    for (caller, want) in [("2", "2"), ("16", "8"), ("", "8"), ("none", "8")] {
        let repo = FakeRepo::new();
        let trace = repo.scratch.join("jobs-seen");
        let target = repo.root.join("target-drivers");
        repo.with_stage2(&format!(
            r#"test "$CARGO_TARGET_DIR" = {target} || exit 70
case "$*" in
  '--unverified build -q -p aterm-gui -p aterm-ctl')
    echo "$CARGO_BUILD_JOBS" >> {trace}
    mkdir -p "$CARGO_TARGET_DIR/debug"
    echo fresh-sealed-gui > "$CARGO_TARGET_DIR/debug/aterm-gui"
    ;;
  '--unverified test -p aterm-link --features sealed --test two_nodes_sealed --no-fail-fast')
    echo "$CARGO_BUILD_JOBS" >> {trace}
    ;;
  *) exit 74 ;;
esac
"#,
            target = sh_quote(&target.display().to_string()),
            trace = sh_quote(&trace.display().to_string()),
        ));
        let ctx = repo.ctx_with(Mode::Full, Scope::workspace(), |env| {
            env.cargo_build_jobs = (caller != "none").then(|| caller.into());
        });
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::SealedLane)
            .expect("sealed stage");
        let report = stages::run_stage(&ctx, &spec);
        let seen = fs::read_to_string(&trace).expect("stage invoked the driver");
        assert_eq!(
            seen,
            format!("{want}\n{want}\n"),
            "caller {caller:?}: both children of the sealed lane see the capped count"
        );
        assert!(
            !tally(std::slice::from_ref(&report)).failed(),
            "caller {caller:?}: {}",
            report.render()
        );
    }
}

/// A HEADLESS `aterm` THAT DIES AT STARTUP IS A FINDING, NOT A BROKEN MACHINE.
///
/// The REAL `tools/test-foreground-handback.sh`, run by its stage against a
/// just-built `aterm` that exits before it answers — the shape of a tree whose
/// `aterm --headless` panics at startup. Until 2026-09-26 the lane answered
/// that with `2`, which the stage reads as COULD NOT RUN and the verdict as
/// "NOT a finding about your change — the environment is broken" (reproduced:
/// exit 2 after 20.6 s). The gate's own control-socket smoke records the same
/// shape as FAIL, and so does this lane now: a FAIL row and exit `1`, at once.
///
/// NEGATIVE CONTROL, so the case can tell the two apart: the lane's real
/// not-run paths — no binary, an argument it does not know — still answer `2`,
/// with a `NOT RUN: ` reason the stage quotes as COULD NOT RUN.
#[cfg(target_os = "macos")]
#[test]
fn a_headless_aterm_that_dies_at_startup_fails_the_handback_and_is_never_could_not_run() {
    let repo = FakeRepo::new();
    let real = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools")
        .join(stages::FOREGROUND_HANDBACK_SUITE);
    let lane = repo
        .root
        .join("tools")
        .join(stages::FOREGROUND_HANDBACK_SUITE);
    fs::copy(&real, &lane).expect("the real lane");
    // …and the library it sources, as a real checkout carries it: without it the
    // lane stops at its not-run check before it ever boots the aterm under test.
    fs::copy(
        real.with_file_name("lib-lifeline.sh"),
        lane.with_file_name("lib-lifeline.sh"),
    )
    .expect("the lane's lifeline library");
    let target = repo.root.join("target-drivers");
    fs::remove_file(target.join("debug/aterm")).expect("unseed the driven aterm");
    // The instance the lane boots marks the moment it dies — its last act
    // before `exit 101`, a builtin, so nothing runs between the two — and the
    // promptness below is measured from that mark. The lane's `--help` probe
    // runs the same binary first and leaves no mark.
    let died = repo.scratch.join("instance-died");
    repo.with_stage2(&format!(
        r#"case "$*" in
  '--unverified build -q -p aterm --bin aterm')
    mkdir -p "$CARGO_TARGET_DIR/debug"
    cat > "$CARGO_TARGET_DIR/debug/aterm" <<'AT'
#!/bin/sh
echo "thread main panicked at startup" >&2
case " $* " in *" --help "*) ;; *) : > {died} ;; esac
exit 101
AT
    chmod 755 "$CARGO_TARGET_DIR/debug/aterm"
    ;;
  *) exit 74 ;;
esac
"#,
        died = sh_quote(&died.display().to_string()),
    ));

    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::ForegroundHandback)
        .expect("the foreground handback is planned");
    let t = std::time::Instant::now();
    let report = stages::run_stage(&ctx, &spec);
    let took = t.elapsed();
    let done = std::time::SystemTime::now();
    let rendered = report.render();
    let died_at = fs::metadata(&died)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| {
            panic!("the booted instance ran and marked its death ({e}): {rendered}")
        });
    // A wall clock stepped back across the run reads as no wait at all; it
    // cannot manufacture one.
    let after_death = done.duration_since(died_at).unwrap_or_default();
    let result = tally(std::slice::from_ref(&report));
    assert_eq!(
        result.gate_failures,
        [
            "test-foreground-handback.sh: a check failed against the live aterm — its rows above \
             say which"
        ],
        "{rendered}"
    );
    assert!(result.could_not_run.is_empty(), "{rendered}");
    assert!(
        rendered.contains("FAIL  boot: /bin/zsh — the headless instance exited before it answered"),
        "the lane's own row names the early exit: {rendered}"
    );
    assert!(
        rendered.contains("thread main panicked at startup"),
        "…and shows the instance's log: {rendered}"
    );
    // …and it was not waited on for the lane's 20 s deadline, read BY CAUSE: the
    // early-exit break is the only path that says "exited before it answered",
    // and the deadline path says "never answered its control socket within
    // 20 s". (This was a stopwatch, `< 10 s`, and read 11.79 s in a loaded
    // merge-contract run, 2026-09-27, with the early exit taken.)
    assert!(
        !rendered.contains("never answered its control socket"),
        "an instance that has exited is not waited on for the deadline: {rendered}"
    );
    // PROMPT, MEASURED FROM THE DEATH. The lane polls 80 × 0.25 s, so a lane
    // that missed the exit spends at least that 20 s after the mark (one that
    // noticed only at its 60th poll read 15.5 s, and failed here); one that
    // saw it spends a `kill -0`, at most one 0.25 s sleep, its FAIL row and
    // its teardown (0.29-0.37 s, measured). Half the deadline tells the two
    // apart with room on both sides.
    //
    // NOT FROM THE STAGE'S START (`took`, the 10 s bound until 2026-09-27).
    // That clock also pays the FIRST exec of three files this fixture writes
    // moments before — the stand-in `targo`, the copied lane and the `aterm`
    // that build writes — and macOS makes the first exec of every newly
    // written executable wait on a check it serves one file at a time,
    // machine-wide (measured: a fresh script 147 ms against 13 ms re-run;
    // 16 threads running fresh ones, a 1.7 s median at 110 ms per exec end to
    // end; two such processes at once, the same one queue). Beside the other
    // cases in this binary, each writing and running dozens of stand-ins,
    // `took` read 11.4-13.1 s in 8 of 8 runs under load while the stage
    // returned 0.29-0.37 s after the death; sampled with `ps`, the lane sat
    // 7.5 s in its interpreter's exec, asleep with no CPU used, before its
    // first line ran. The merge contract's red run (13.3 s) is that shape.
    assert!(
        after_death < std::time::Duration::from_secs(10),
        "an instance that has exited is not waited on for the 20 s deadline: the stage \
         returned {after_death:?} after the instance died ({took:?} in all)"
    );

    // The negative control: the lane's real not-run path, still 2, still
    // COULD NOT RUN with the reason the lane printed.
    let out = std::process::Command::new(&lane)
        .args(["--binary", "/nonexistent/aterm"])
        .current_dir(&repo.root)
        .output()
        .expect("the lane runs");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let transcript = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stages::live_aterm_outcome(stages::FOREGROUND_HANDBACK_SUITE, Some(2), &transcript),
        (
            aterm_verify::Outcome::Fail(aterm_verify::Severity::CouldNotRun),
            "test-foreground-handback.sh: NOT RUN — no aterm binary at /nonexistent/aterm \
             (targo --unverified build -p aterm) (exit 2, never a pass)"
                .to_string()
        )
    );
}

/// THE TIERS, END TO END (2026-09-26). The default run — the merge contract
/// — plans no MEASURE stage, spawns no release build and no `measuring::`
/// child, runs the deadline tests alone, and still NAMES every MEASURE stage
/// in its verdict: leaving a
/// tier out is said, never skipped silently. `--measure` plans exactly the
/// MEASURE tier and nothing of the contract; this fixture skips the pacing
/// smoke (no window), so the run is green, claims nothing, and says it did
/// not measure the tree and why.
#[test]
fn the_default_names_the_measure_tier_it_leaves_out_and_measure_runs_it_alone() {
    let repo = FakeRepo::new();
    repo.with_stage2("echo \"argv: $*\"\nexit 0");
    let titles = plan::tier_titles(&Scope::workspace(), Tier::Measure);
    assert_eq!(titles.len(), 3, "{titles:?}");

    let (fast, _) = repo.run(Mode::Fast, Scope::workspace());
    for title in &titles {
        assert!(!headers(&fast).contains(title), "the default ran {title}");
        assert!(
            fast.contains(&format!("      - {title}\n")),
            "the default must name {title}:\n{fast}"
        );
    }
    assert!(fast.contains("MEASURE tier: not part of the merge contract"));
    assert!(
        !fast.contains("--release"),
        "the merge contract builds no release artifact:\n{fast}"
    );
    // The test run's child carries the recording runner before its libtest
    // separator (2026-09-26); the stand-in driver runs no runner, so it is the
    // whole test run here.
    assert!(fast.contains(
        "argv: --unverified test --workspace --no-fail-fast --tests --config \
         target.'cfg(all())'.runner=["
    ));
    assert!(fast.contains("\"] -- --skip measuring:: --skip launchd_copy_tests::"));
    assert!(fast.contains(
        "argv: --unverified test --workspace --no-fail-fast --lib -- launchd_copy_tests::"
    ));
    assert!(
        !fast.contains("-- measuring::"),
        "no measuring child in the contract:\n{fast}"
    );
    assert!(
        !fast.contains("test-start-compare.sh"),
        "the startup comparison is --full's:\n{fast}"
    );
    assert!(
        !fast.contains("verify: MEASURE tier —"),
        "a run that measured nothing says nothing about measuring:\n{fast}"
    );

    let (measure, code) = repo.run(Mode::Measure, Scope::workspace());
    let mut want = titles.clone();
    want.push("verdict".to_string());
    assert_eq!(headers(&measure), want, "{measure}");
    for argv in [
        "argv: --unverified build --locked --release -p aterm",
        "argv: --unverified test --workspace --no-fail-fast --test paint --test spin -- measuring::",
    ] {
        assert!(measure.contains(argv), "missing {argv:?}:\n{measure}");
    }
    assert!(
        !measure.contains("argv: --unverified test --workspace --no-fail-fast --no-run"),
        "no test compile of the merge contract: {measure}"
    );
    assert!(!measure.contains("launchd_copy_tests"), "{measure}");
    assert_eq!(code, exit::PASS, "{measure}");
    assert!(measure.contains(
        "verify: MEASURE tier — NOT MEASURED: `gui typing-pacing smoke` skipped: gui smoke \
         (--skip-gui-smoke)"
    ));
    assert!(measure.contains("VERIFY: PASS (mode=measure scope=workspace, 1 skipped) —"));
    assert!(!measure.contains(MERGE_CONTRACT_SENTENCE));
    assert!(
        !measure.contains("MEASURE tier: not part of the merge contract"),
        "it ran the tier:\n{measure}"
    );
}

/// A PROBE THAT DECIDED NOTHING IS COULD NOT RUN, NOT A FINDING (2026-09-26).
/// The paint matrix's exit 2 — no binary, a socket that never bound — now
/// opens its panic with the gate's sentinel, as its exit 3 has since
/// 2026-09-23, so a measuring child whose every failure says so is a COULD NOT
/// RUN row: the run exits 3, not 1, and it did not measure the tree. The
/// negative control is the same log in the words exit 2 used before, without
/// the sentinel: a FAIL, exit 1.
#[test]
fn a_paint_probe_that_decided_nothing_is_a_could_not_run_row() {
    let log = |message: &str| {
        format!(
            "     Running tests/paint.rs (target/debug/deps/paint-abc)\n\nrunning 2 tests\n\
             test measuring::scan_self_test ... ok\n\
             test measuring::main_screen_prompt_typing_paints_trail_ink ... FAILED\n\n\
             failures:\n\n---- measuring::main_screen_prompt_typing_paints_trail_ink stdout ----\n\n\
             thread 'measuring::main_screen_prompt_typing_paints_trail_ink' panicked at \
             crates/aterm-conformance/tests/paint/measuring.rs:425:14:\n{message}\n\
             note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\n\
             failures:\n    measuring::main_screen_prompt_typing_paints_trail_ink\n\n\
             test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.01s\n\nerror: test failed, to rerun pass `-p aterm-conformance --test paint`\n"
        )
    };
    let decided_nothing = "PAINT CONFORMANCE COULD NOT RUN [prompt]: the probe decided nothing, \
                           which is not a pass.\n  PAINT-COULD-NOT-RUN control socket never \
                           appeared within 60s (launch alive but starved?)";
    for (sentinel, code, word) in [
        (true, exit::COULD_NOT_RUN, "could not run"),
        (false, exit::FAILED, "FAILED"),
    ] {
        let repo = FakeRepo::new();
        let message = if sentinel {
            format!(
                "{} — {decided_nothing}",
                aterm_verify::libtest::COULD_NOT_RUN_SENTINEL
            )
        } else {
            decided_nothing.to_string()
        };
        let out = repo.scratch.join("paint.log");
        fs::write(&out, log(&message)).expect("write");
        repo.with_stage2(&format!(
            "case \"$*\" in\n  *'-- measuring::'*) cat '{}'; exit 101 ;;\nesac\nexit 0",
            out.display()
        ));
        let (ladder, got) = repo.run(Mode::Measure, Scope::workspace());
        assert_eq!(got, code, "sentinel={sentinel}:\n{ladder}");
        let labels = labels_with(&ladder, "FAIL");
        assert!(
            labels.iter().any(|l| l.starts_with(
                "targo test --workspace --test paint --test spin (trustdoc) -- measuring::"
            ) && (l.contains("could not run") == sentinel)),
            "sentinel={sentinel}: {labels:?}"
        );
        assert!(
            ladder.contains(&format!(
                "verify: MEASURE tier — NOT MEASURED: `measuring tests (--workspace; run alone)` {word}"
            )),
            "sentinel={sentinel}:\n{ladder}"
        );
    }
}

/// THE DIFFERENTIAL CLAIM, END TO END (2026-09-26): the REAL plan, scheduler,
/// tally and verdict, as `a_whole_green_run_is_the_only_thing_that_claims_the_contract`
/// drives them, with one stage red the way a real child makes it red (the
/// lint stage's `Report::fail_checker_child` fingerprints what it printed).
/// Judged against a main whose receipt lists exactly that failure — the
/// findings the tally itemized, written back the way a receipt lists them —
/// the run claims the contract and names the red as inherited, exit 0. The
/// same stage printing another error — or the same lint at another line
/// (2026-09-27, second review: a line is the message) — is a different
/// failure: NEW, exit 1. The same lint in a LIBRARY is never main's
/// (2026-09-27, fourth review): it left every crate built on that library
/// unlinted, and the output is the same whatever a branch did to them. And
/// the same red with no base at all is the absolute rule's FAIL.
#[test]
fn a_run_whose_every_red_main_already_has_claims_the_contract_against_main() {
    use aterm_verify::differential::{Against, BaseReds};
    use aterm_verify::receipt::Failure;
    use aterm_verify::verdict::verdict_against;

    let repo = FakeRepo::new();
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let specs = plan::plan(&ctx);
    let lint = |said: String| {
        move |s: &StageSpec| {
            let mut r = Report::new(s.title.clone());
            if s.title.starts_with("tippy") {
                let run = aterm_verify::exec::Run {
                    ok: false,
                    output: said.clone(),
                    code: Some(101),
                    spawn_error: None,
                };
                r.fail_checker_child(
                    &run,
                    "tippy --workspace -D warnings",
                    aterm_verify::differential::lint_reached_every_unit,
                );
            } else {
                r.pass("did the thing");
            }
            r
        }
    };
    // A lint in an integration test: a unit nothing else is built on.
    let said = |msg: &str, at: &str| {
        format!(
            "error: {msg}\n  --> crates/a/tests/{at}\nerror: could not compile `a` (test \"probe\") \
             due to 1 previous error\n"
        )
    };
    let main_says = said("this call to `clone` can be replaced", "probe.rs:12:5");
    let on_main = tally(&sched::run_stages(
        &specs,
        lint(main_says.clone()),
        |_, _| {},
    ));
    let now = 2_000_000_000;
    let main = Against::Base(BaseReds {
        commit: "9".repeat(40),
        source: "receipt 999999999".to_string(),
        failures: on_main
            .all_findings()
            .map(|f| Failure {
                id: f.id.clone(),
                hash: f.hash.clone(),
                since: "9".repeat(40),
                since_when: now - 3600,
            })
            .collect(),
        now,
        nearest: None,
    });

    // The same lint at another line is another failure.
    let t = tally(&sched::run_stages(
        &specs,
        lint(said(
            "this call to `clone` can be replaced",
            "probe.rs:40:9",
        )),
        |_, _| {},
    ));
    let v = verdict_against(Mode::Fast, &Scope::workspace(), &t, &main);
    assert!(!v.claims_merge_contract, "{}", v.text);
    assert_eq!(v.exit, exit::FAILED, "{}", v.text);

    // The branch: the same lint, the same way.
    let t = tally(&sched::run_stages(
        &specs,
        lint(main_says.clone()),
        |_, _| {},
    ));
    let v = verdict_against(Mode::Fast, &Scope::workspace(), &t, &main);
    assert!(v.claims_merge_contract, "{}", v.text);
    assert_eq!(v.exit, exit::PASS);
    assert!(v.text.contains(MERGE_CONTRACT_SENTENCE));
    assert!(
        v.text
            .contains("0 new, 1 inherited (red on main since 999999999)"),
        "{}",
        v.text
    );
    assert!(
        v.text
            .contains("      - tippy --workspace -D warnings (red on main since 999999999, 1 h)"),
        "{}",
        v.text
    );

    // Another error from the same stage: a different failure.
    let t = tally(&sched::run_stages(
        &specs,
        lint(said("unused import", "x.rs:3:1")),
        |_, _| {},
    ));
    let v = verdict_against(Mode::Fast, &Scope::workspace(), &t, &main);
    assert!(!v.claims_merge_contract);
    assert_eq!(v.exit, exit::FAILED);
    assert!(
        v.text.contains(
            "      - tippy --workspace -D warnings — red on main too, but failing differently"
        ),
        "{}",
        v.text
    );

    // The same lint in a library: never main's, however the same it reads.
    let in_lib = "error: this call to `clone` can be replaced\n  --> crates/a/src/lib.rs:12:5\n\
                  error: could not compile `a` (lib) due to 1 previous error\n"
        .to_string();
    let lib_red = tally(&sched::run_stages(&specs, lint(in_lib.clone()), |_, _| {}));
    let main_lib = Against::Base(BaseReds {
        commit: "9".repeat(40),
        source: "receipt 999999999".to_string(),
        failures: lib_red
            .all_findings()
            .map(|f| Failure {
                id: f.id.clone(),
                hash: f.hash.clone(),
                since: "9".repeat(40),
                since_when: now - 3600,
            })
            .collect(),
        now,
        nearest: None,
    });
    let t = tally(&sched::run_stages(&specs, lint(in_lib), |_, _| {}));
    let v = verdict_against(Mode::Fast, &Scope::workspace(), &t, &main_lib);
    assert!(!v.claims_merge_contract, "{}", v.text);
    assert_eq!(v.exit, exit::FAILED, "{}", v.text);
    assert!(
        v.text.contains("every unit that needs it went unlinted"),
        "{}",
        v.text
    );

    // No base: the absolute rule.
    let v = verdict(Mode::Fast, &Scope::workspace(), &on_main);
    assert!(!v.claims_merge_contract);
    assert_eq!(v.exit, exit::FAILED);
}

/// AN EXCUSED COMPILE RED NEVER EXCUSES THE TESTS IT KEPT FROM RUNNING
/// (2026-09-27, third review). The test stage runs no test after its compile
/// fails, and said so in a raw `not run:` line the tally never heard: the
/// compile's FAIL was "the decision". Against main's receipt that FAIL was
/// INHERITED when main's compile broke the same way — and then nothing at all
/// stood for the test run: a branch could break any test, and the run claimed
/// the merge contract. Now the run the compile prevented is a COULD NOT RUN
/// row, naming why. And the compile red itself is main's no longer
/// (2026-09-27, fourth review): a build stops at the first crate that fails,
/// so its output is the same whatever a branch broke in the crates behind
/// it — NEW, never inherited, exit 1.
#[test]
fn an_excused_compile_red_never_excuses_the_tests_it_kept_from_running() {
    use aterm_verify::differential::{Against, BaseReds};
    use aterm_verify::receipt::Failure;
    use aterm_verify::verdict::verdict_against;

    let repo = FakeRepo::new();
    repo.with_stage2(
        "case \"$*\" in *--no-run*) echo 'error[E0425]: cannot find value `gone` in this scope' \
         >&2; echo '  --> crates/x/tests/probe.rs:3:13' >&2; exit 101 ;; esac\nexit 0",
    );
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::Test)
        .expect("the test stage");
    let run = || tally(std::slice::from_ref(&stages::run_stage(&ctx, &spec)));
    let on_main = run();
    assert_eq!(
        on_main.gate_failures,
        ["targo test --workspace --no-run (trustdoc)"],
        "{on_main:?}"
    );
    let now = 2_000_000_000;
    let main = Against::Base(BaseReds {
        commit: "9".repeat(40),
        source: "receipt 999999999".to_string(),
        failures: on_main
            .all_findings()
            .map(|f| Failure {
                id: f.id.clone(),
                hash: f.hash.clone(),
                since: "9".repeat(40),
                since_when: now - 3600,
            })
            .collect(),
        now,
        nearest: None,
    });
    let branch = run();
    let v = verdict_against(Mode::Fast, &Scope::workspace(), &branch, &main);
    assert!(!v.claims_merge_contract, "{}", v.text);
    assert_eq!(v.exit, exit::FAILED, "{}", v.text);
    assert!(
        v.text.contains(
            "targo test --workspace --no-run (trustdoc) — never inherited: nothing shows it \
             ran to its end"
        ),
        "a build red is never main's: {}",
        v.text
    );
    assert!(
        v.text.contains(
            "targo test --workspace --tests (trustdoc) — not run: the compile above failed"
        ),
        "{}",
        v.text
    );
}

/// A compile red, as a stub driver prints one: the build of whatever it names
/// fails, every other invocation passes.
const COMPILE_RED: &str = "echo 'error[E0425]: cannot find value `gone` in this scope' >&2; \
     echo '  --> crates/aterm-gui/src/bin/rot.rs:3:13' >&2; exit 101";

/// One stage of the merge contract, run alone, tallied.
fn stage_tally(ctx: &aterm_verify::Ctx, id: StageId) -> Tally {
    let spec = plan::plan(ctx)
        .into_iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("{id:?} is planned in the merge contract"));
    tally(std::slice::from_ref(&stages::run_stage(ctx, &spec)))
}

/// Main's reds, as a baseline an hour old lists `on_main`'s findings.
fn mains_reds(on_main: &Tally) -> aterm_verify::differential::Against {
    let now = 2_000_000_000;
    aterm_verify::differential::Against::Base(aterm_verify::differential::BaseReds {
        commit: "9".repeat(40),
        source: "receipt 999999999".to_string(),
        failures: on_main
            .all_findings()
            .map(|f| aterm_verify::receipt::Failure {
                id: f.id.clone(),
                hash: f.hash.clone(),
                since: "9".repeat(40),
                since_when: now - 3600,
            })
            .collect(),
        now,
        nearest: None,
    })
}

/// P1, P3 (2026-09-27, fourth review): AN EXCUSED BUILD RED NEVER EXCUSES THE
/// DRIVE IT KEPT FROM RUNNING. The third review's `not run` rows covered four
/// sites; the redraw harness and the eight objc drivers still ended at their
/// build's FAIL. Main's driver build red (a rotted example — nothing else
/// builds `--example` targets), and the branch breaks what the driver checks
/// (the stub driver exits 1 now): the build red was INHERITED, the harness ran
/// on neither side, and the run printed `VERIFY: PASS … 1 inherited — merge
/// contract satisfied`, exit 0 (the review's probes, on the real stages). Now
/// the build's row is never inherited (a build stops at its first failure),
/// and the drive it kept from running is a COULD NOT RUN row of its own.
#[test]
fn an_excused_build_red_never_excuses_the_drive_it_kept_from_running() {
    use aterm_verify::verdict::verdict_against;

    let repo = FakeRepo::new();
    let mut cases = vec![(
        StageId::RedrawConformance,
        stages::REDRAW_CONFORMANCE_BIN.to_string(),
        stages::REDRAW_CONFORMANCE_BIN,
    )];
    if cfg!(target_os = "macos") {
        cases.push((
            StageId::ObjcDrives,
            objc_stub("objc_window_drive"),
            "objc_window_drive",
        ));
    }
    for (id, driven, named) in cases {
        repo.with_stage2(&format!(
            "case \"$*\" in *{named}*) {COMPILE_RED} ;; esac\nexit 0"
        ));
        let ctx = repo.ctx(Mode::Fast, Scope::workspace());
        repo.driver_stub(&driven, 0);
        let on_main = stage_tally(&ctx, id);
        assert_eq!(on_main.gate_failures.len(), 1, "{id:?}: {on_main:?}");
        // The branch breaks the driver's subject: it would exit 1 if it ran.
        repo.driver_stub(&driven, 1);
        let branch = stage_tally(&ctx, id);
        let v = verdict_against(
            Mode::Fast,
            &Scope::workspace(),
            &branch,
            &mains_reds(&on_main),
        );
        assert!(!v.claims_merge_contract, "{id:?}:\n{}", v.text);
        assert_eq!(v.exit, exit::FAILED, "{id:?}:\n{}", v.text);
        assert!(
            branch
                .could_not_run
                .iter()
                .any(|c| c.contains("— not run: the build above failed")),
            "{id:?}: the drive the build kept from running names itself: {branch:?}"
        );
    }
}

/// P2 (2026-09-27, fourth review): the control-socket smoke behind a smoke
/// build red main already has. It escaped the review's probe only because its
/// build writes to the smoke's log, so the row printed nothing and was opaque
/// by accident; now the smoke it kept from running says so.
#[test]
fn an_excused_smoke_build_red_never_excuses_the_smoke() {
    use aterm_verify::verdict::verdict_against;

    let repo = FakeRepo::new();
    repo.with_stage2(&format!(
        "case \"$*\" in *aterm-gui*aterm-ctl*) {COMPILE_RED} ;; esac\nexit 0"
    ));
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let on_main = stage_tally(&ctx, StageId::ControlSocketSmoke);
    assert_eq!(on_main.gate_failures.len(), 1, "{on_main:?}");
    let branch = stage_tally(&ctx, StageId::ControlSocketSmoke);
    let v = verdict_against(
        Mode::Fast,
        &Scope::workspace(),
        &branch,
        &mains_reds(&on_main),
    );
    assert!(!v.claims_merge_contract, "{}", v.text);
    assert!(
        branch
            .could_not_run
            .iter()
            .any(|c| c.contains("smoke: the smoke's checks — not run: the build above failed")),
        "{branch:?}"
    );
}

/// P5 (2026-09-27, fourth review): A SUITE THAT STOPS AT ITS FIRST FAILURE.
/// The `tools/test-*.sh` suites end at their first failing check (`fail() {
/// echo …; exit 1; }`), so their output is the same wherever they stopped.
/// Main fails check 1; the branch fails check 1 AND breaks check 2, which the
/// suite never reaches: the row was INHERITED and the run claimed the merge
/// contract, exit 0. Now a row whose check is not known to run to its end is
/// never inherited.
#[test]
fn an_excused_suite_red_never_excuses_the_checks_after_it() {
    use aterm_verify::verdict::verdict_against;

    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let suite = |check2_breaks: bool| {
        repo.script(
            &format!("tools/{}", stages::DELIVERY_SUITES[0]),
            &format!(
                "fail() {{ echo \"FAIL: $*\" >&2; exit 1; }}\n\
                 fail 'check 1: the channel pin names no build'\n\
                 {} && fail 'check 2: the installer runs unsigned code'\n\
                 echo PASS",
                if check2_breaks { "true" } else { "false" }
            ),
        );
    };
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    suite(false);
    let on_main = stage_tally(&ctx, StageId::DeliveryTooling);
    assert_eq!(on_main.gate_failures.len(), 1, "{on_main:?}");
    suite(true);
    let branch = stage_tally(&ctx, StageId::DeliveryTooling);
    let v = verdict_against(
        Mode::Fast,
        &Scope::workspace(),
        &branch,
        &mains_reds(&on_main),
    );
    assert!(!v.claims_merge_contract, "{}", v.text);
    assert_eq!(v.exit, exit::FAILED, "{}", v.text);
    assert!(
        v.text.contains(&format!(
            "{} — never inherited: nothing shows it ran to its end",
            stages::DELIVERY_SUITES[0]
        )),
        "{}",
        v.text
    );
}

/// A STAND-IN PUBLICATION ENGINE: the `bin/pub` functions
/// `tools/export-content-scan.py` drives, each doing the least the real one
/// does — the manifest's allowlist and `!` exclusions, `transforms.sh` sourced
/// over the export, one line-numbered raw record per hit through
/// `_adjudicate_forbidden`, a redacted report — so the STAGE is driven end to
/// end, the real scan over a planted hit in a real git tree, on any machine.
/// `tools/test-export-content-scan.sh` pins the scan against the real engine.
const FAKE_ENGINE: &str = r##"import os, re, subprocess, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
RUN_ERROR = 1000


def say(msg):
    print(f"== {msg}")


def die(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def _pattern_lines(path):
    if not path.exists():
        return []
    return [l.strip() for l in path.read_text().splitlines()
            if l.strip() and not l.strip().startswith("#")]


def manifest_specs(pub):
    specs = []
    for raw in (pub / "manifest.txt").read_text().splitlines():
        line = raw.split("#", 1)[0].strip()
        if line:
            specs.append(":(exclude)" + line[1:] if line.startswith("!") else line)
    return specs + [":(exclude)publish"]


def materialize_from_commit(root, head, out, specs, inventory=None, label="manifest",
                            allow_symlinks=True):
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, GIT_INDEX_FILE=str(out.parent / f".{out.name}.index"))
    subprocess.run(["git", "-C", str(root), "read-tree", head], env=env, check=True)
    paths = subprocess.run(["git", "-C", str(root), "ls-files", "-z", "--", *specs], env=env,
                           check=True, capture_output=True).stdout
    subprocess.run(["git", "-C", str(root), "checkout-index", "-z", "--stdin",
                    f"--prefix={out}/"], env=env, input=paths, check=True)
    return [{"path": p} for p in paths.decode().split("\0") if p]


def export_tree(root, head, out, specs):
    exp = out / "export"
    return exp, materialize_from_commit(root, head, exp, specs)


def run_transforms(root, pub, out, exp, captured_commit):
    env = dict(os.environ, ROOT=str(root), PUB=str(pub), OUT=str(out), EXPORT=str(exp))
    script = 'set -euo pipefail; fail() { echo "FAIL: $*" >&2; exit 1; }; . "$PUB/transforms.sh"'
    if subprocess.run(["bash", "-c", script], cwd=root, env=env).returncode != 0:
        die("transforms failed")


def reject_embedded_git_metadata(tree):
    pass


def org_rewrite(exp):
    return []


def check_symlinks(exp):
    pass


def _adjudicate_forbidden(tree, records):
    return records, {}


def scan_forbidden(tree, pub, out, label=""):
    patterns = sorted(set(_pattern_lines(HERE / "baseline" / "forbidden-content.txt"))
                      | set(_pattern_lines(pub / "forbidden-extra.txt")))
    records = []
    for path in sorted(p for p in tree.rglob("*") if p.is_file()):
        rel = path.relative_to(tree).as_posix()
        for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
            if any(re.search(p, line) for p in patterns):
                records.append((f"{rel}:{number}:{line}",
                                f"{rel}:{number}:[REDACTED] forbidden content", True))
    records, _ = _adjudicate_forbidden(tree, records)
    (out / "forbidden-hits.txt").write_text("".join(r + "\n" for _, r, _ in records))
    say(f"GUARD FAIL: forbidden content ({len(records)} hits)" if records
        else "guard ok: no forbidden content")
    return not records


def scan_private_refs(tree, out):
    (out / "private-ref-hits.txt").write_text("")
    return True


def check_gitleaks_config(exp):
    return True


def scan_gitleaks(source, out, report_name, label=""):
    return 0, out / report_name
"##;

/// The planted token, spelled in pieces so this file never carries it whole:
/// the stand-in engine's baseline pattern is `PLANTED_[0-9]+`.
fn planted(n: u32) -> String {
    format!("{}_{n}", "PLANTED")
}

/// `git -C root …` for a fixture, with a fixture identity, which must pass.
fn fixture_git(root: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .status()
        .expect("git runs");
    assert!(st.success(), "git {args:?} in {}", root.display());
}

/// A FakeRepo that is also a git tree the REAL export content scan can judge:
/// the scan copied in from this repository's `tools/`, a policy (`publish/`)
/// selecting `src` and `.gitleaks.toml`, one clean committed file — and a
/// stand-in engine ([`FAKE_ENGINE`]) beside it whose baseline has one pattern
/// in one section, plus a stand-in `gitleaks` on the stage's PATH (the
/// stand-in engine's gitleaks guard runs nothing). The context names the
/// engine through `$PUBLICATION_ENGINE`, so the caller's own never leaks in.
fn export_content_fixture() -> (FakeRepo, Ctx, PathBuf) {
    let repo = FakeRepo::new();
    let base = repo.root.parent().expect("the fixture base").to_path_buf();
    let scan = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools")
        .join(stages::EXPORT_CONTENT_SCRIPT);
    let installed = repo.root.join("tools").join(stages::EXPORT_CONTENT_SCRIPT);
    fs::copy(&scan, &installed).expect("copy the real scan into the fixture");
    fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).expect("chmod");

    let engine = base.join("engine");
    fs::create_dir_all(engine.join("bin")).expect("mkdir");
    fs::create_dir_all(engine.join("baseline")).expect("mkdir");
    fs::write(engine.join("bin/pub"), FAKE_ENGINE).expect("write the stand-in engine");
    fs::write(
        engine.join("baseline/forbidden-content.txt"),
        "# Planted tokens\nPLANTED_[0-9]+\n",
    )
    .expect("write the stand-in baseline");
    let bin = base.join("bin");
    fs::create_dir_all(&bin).expect("mkdir");
    let gitleaks = bin.join("gitleaks");
    fs::write(&gitleaks, "#!/bin/sh\nexit 0\n").expect("write");
    fs::set_permissions(&gitleaks, fs::Permissions::from_mode(0o755)).expect("chmod");

    for dir in ["publish", "src"] {
        fs::create_dir_all(repo.root.join(dir)).expect("mkdir");
    }
    fs::write(
        repo.root.join("publish/manifest.txt"),
        "src\n.gitleaks.toml\n",
    )
    .expect("write");
    fs::write(repo.root.join("publish/transforms.sh"), "").expect("write");
    fs::write(
        repo.root.join(".gitleaks.toml"),
        "[extend]\nuseDefault = true\n",
    )
    .expect("write");
    fs::write(repo.root.join("src/clean.rs"), "fn clean() {}\n").expect("write");
    fixture_git(&repo.root, &["init", "-q", "-b", "main"]);
    fixture_git(&repo.root, &["add", "publish", "src", ".gitleaks.toml"]);
    fixture_git(&repo.root, &["commit", "-qm", "a clean tree"]);

    let mut ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let mut path = std::ffi::OsString::from(&bin);
    path.push(":");
    path.push(&ctx.path_env);
    ctx.path_env = path;
    ctx.child_env_add
        .push(("PUBLICATION_ENGINE".into(), engine.clone().into_os_string()));
    (repo, ctx, engine)
}

/// The export content stage alone, run: its report.
fn export_content_report(ctx: &Ctx) -> Report {
    let spec = plan::plan(ctx)
        .into_iter()
        .find(|s| s.id == StageId::ExportContent)
        .expect("the export content scan is planned in the merge contract");
    stages::run_stage(ctx, &spec)
}

/// THE EXPORT CONTENT SCAN, END TO END (2026-09-29): the real scan over a
/// real git tree. A clean tree is `ok`; a planted hit — committed, or
/// uncommitted work — FAILS the stage as a finding, each hit named at its
/// SOURCE file:line with its pattern class (the pattern's own line and
/// section), and the matched text appears nowhere in the ladder.
#[test]
fn the_export_content_stage_names_a_planted_hit_at_its_source_line_and_passes_a_clean_tree() {
    let (repo, ctx, _engine) = export_content_fixture();
    let clean = export_content_report(&ctx);
    assert_eq!(
        clean.outcomes().collect::<Vec<_>>(),
        [(
            aterm_verify::ladder::Outcome::Ok,
            stages::EXPORT_CONTENT_SCRIPT
        )],
        "{}",
        clean.render()
    );
    assert!(
        clean.render().contains("EXPORT CONTENT: PASS"),
        "{}",
        clean.render()
    );

    fs::write(
        repo.root.join("src/leak.rs"),
        format!(
            "fn a() {{}}\nfn b() {{}}\nconst T: &str = \"{}\";\n",
            planted(7)
        ),
    )
    .expect("write");
    fixture_git(&repo.root, &["add", "src/leak.rs"]);
    fixture_git(&repo.root, &["commit", "-qm", "a planted hit"]);
    // …and one in uncommitted work, which the gate judges like every stage.
    fs::write(
        repo.root.join("src/draft.rs"),
        format!("// {}\n", planted(8)),
    )
    .expect("write");

    let red = export_content_report(&ctx);
    let ladder = red.render();
    let t = tally(std::slice::from_ref(&red));
    assert_eq!(t.gate_failures.len(), 1, "{ladder}");
    assert!(t.could_not_run.is_empty() && t.skips.is_empty(), "{ladder}");
    for hit in [
        "  src/leak.rs:3  forbidden content — Planted tokens (baseline/forbidden-content.txt:2)",
        "  src/draft.rs:1  forbidden content — Planted tokens (baseline/forbidden-content.txt:2)",
        "EXPORT CONTENT: FAIL",
    ] {
        assert!(ladder.contains(hit), "no `{hit}` in:\n{ladder}");
    }
    for secret in [planted(7), planted(8)] {
        assert!(
            !ladder.contains(&secret),
            "the matched text was printed:\n{ladder}"
        );
    }
}

/// NO ENGINE, NO SCAN — AND NO PASS (2026-09-29). The patterns are the
/// engine's, never copied here, so a machine with no engine checkout has
/// nothing to scan with: the stage is a NAMED SKIP carrying the scan's own
/// reason — never a finding about the tree, never a pass — and a run with it
/// does not claim the merge contract.
#[test]
fn a_machine_without_the_publication_engine_skips_the_export_scan_by_name() {
    let (repo, mut ctx, _engine) = export_content_fixture();
    let nowhere = repo.root.parent().expect("base").join("no-engine");
    ctx.child_env_add
        .retain(|(k, _)| k.as_os_str() != std::ffi::OsStr::new("PUBLICATION_ENGINE"));
    ctx.child_env_add.push((
        "PUBLICATION_ENGINE".into(),
        nowhere.clone().into_os_string(),
    ));
    let report = export_content_report(&ctx);
    let t = tally(std::slice::from_ref(&report));
    assert!(
        t.gate_failures.is_empty() && t.could_not_run.is_empty(),
        "{t:?}"
    );
    assert_eq!(t.skips.len(), 1, "{t:?}");
    assert!(
        t.skips[0].contains(&format!(
            "NOT RUN — no publication engine checkout at {}",
            nowhere.display()
        )) && t.skips[0].contains("never a pass"),
        "{t:?}"
    );
    let v = verdict(Mode::Fast, &Scope::workspace(), &t);
    assert!(!v.claims_merge_contract, "{}", v.text);
    assert!(!v.text.contains(MERGE_CONTRACT_SENTENCE), "{}", v.text);
}

/// THE SAME HITS AS MAIN ARE MAIN'S RED; A NEW ONE BLOCKS (2026-09-29). Each
/// hit is a finding of its own, keyed by its source path and pattern class and
/// never by its line (review of the stage): a branch whose export carries
/// main's hits inherits them — after it added a clean file, after an edit
/// above them moved every one, and after it cleared one of them — and a
/// branch that adds a hit is a new failure.
#[test]
fn an_export_hit_main_already_has_is_inherited_and_a_new_one_blocks() {
    use aterm_verify::verdict::verdict_against;
    let (repo, ctx, _engine) = export_content_fixture();
    fs::write(
        repo.root.join("src/leak.rs"),
        format!("// {}\nfn x() {{}}\n// {}\n", planted(1), planted(3)),
    )
    .expect("write");
    fs::write(
        repo.root.join("src/other.rs"),
        format!("// {}\n", planted(4)),
    )
    .expect("write");
    fixture_git(&repo.root, &["add", "src"]);
    fixture_git(&repo.root, &["commit", "-qm", "main's red"]);
    let on_main = stage_tally(&ctx, StageId::ExportContent);
    assert_eq!(on_main.gate_failures.len(), 1, "{on_main:?}");
    assert_eq!(
        on_main.all_findings().count(),
        3,
        "one finding per hit: {on_main:?}"
    );

    fs::write(repo.root.join("src/more.rs"), "fn more() {}\n").expect("write");
    fixture_git(&repo.root, &["add", "src/more.rs"]);
    fixture_git(&repo.root, &["commit", "-qm", "a clean change"]);
    let branch = stage_tally(&ctx, StageId::ExportContent);
    let v = verdict_against(
        Mode::Fast,
        &Scope::workspace(),
        &branch,
        &mains_reds(&on_main),
    );
    assert_eq!(v.exit, exit::PASS, "{}", v.text);
    assert!(v.text.contains("0 new, 3 inherited"), "{}", v.text);

    // Every hit of leak.rs moved down two lines, and other.rs's is cleared.
    fs::write(
        repo.root.join("src/leak.rs"),
        format!(
            "fn a() {{}}\nfn b() {{}}\n// {}\nfn x() {{}}\n// {}\n",
            planted(1),
            planted(3)
        ),
    )
    .expect("write");
    fs::write(repo.root.join("src/other.rs"), "fn other() {}\n").expect("write");
    let moved = stage_tally(&ctx, StageId::ExportContent);
    let v = verdict_against(
        Mode::Fast,
        &Scope::workspace(),
        &moved,
        &mains_reds(&on_main),
    );
    assert_eq!(v.exit, exit::PASS, "{}", v.text);
    assert!(v.text.contains("0 new, 2 inherited"), "{}", v.text);

    // One more hit in leak.rs, where main has two: new.
    fs::write(
        repo.root.join("src/leak.rs"),
        format!(
            "// {}\nfn x() {{}}\n// {}\n// {}\n",
            planted(1),
            planted(3),
            planted(5)
        ),
    )
    .expect("write");
    let more_of_mains = stage_tally(&ctx, StageId::ExportContent);
    let v = verdict_against(
        Mode::Fast,
        &Scope::workspace(),
        &more_of_mains,
        &mains_reds(&on_main),
    );
    assert_eq!(v.exit, exit::FAILED, "{}", v.text);

    fixture_git(&repo.root, &["checkout", "-q", "--", "src"]);
    fs::write(
        repo.root.join("src/more.rs"),
        format!("// {}\n", planted(2)),
    )
    .expect("write");
    let worse = stage_tally(&ctx, StageId::ExportContent);
    let v = verdict_against(
        Mode::Fast,
        &Scope::workspace(),
        &worse,
        &mains_reds(&on_main),
    );
    assert_eq!(v.exit, exit::FAILED, "{}", v.text);
    assert!(!v.claims_merge_contract, "{}", v.text);
}

/// AN ENGINE WHOSE API MOVED IS COULD NOT RUN, NEVER THE TREE'S FINDING
/// (2026-09-29, review of the stage). The engine is a live checkout its
/// owners commit to, and its functions have gained required parameters
/// before (`run_transforms`' `captured_commit`). Measured by the review on
/// this stand-in: `export_tree` given one more required parameter ended the
/// scan in Python's own traceback, exit 1 — the code of a finding — and the
/// stage failed the tree, never to be inherited. Now it is COULD NOT RUN,
/// naming the call. So is a guard that raises.
#[test]
fn an_engine_whose_api_moved_is_could_not_run_never_a_finding() {
    let (_repo, ctx, engine) = export_content_fixture();
    for (what, from, to, says) in [
        (
            "a required parameter more",
            "def export_tree(root, head, out, specs):",
            "def export_tree(root, head, out, specs, captured):",
            "export_tree() raised TypeError",
        ),
        (
            "a guard that raises",
            "def scan_private_refs(tree, out):\n",
            "def scan_private_refs(tree, out):\n    raise OSError('the report moved')\n",
            "scan_private_refs() raised OSError: the report moved",
        ),
    ] {
        assert!(
            FAKE_ENGINE.contains(from),
            "{what}: the stand-in has no `{from}`"
        );
        fs::write(engine.join("bin/pub"), FAKE_ENGINE.replace(from, to)).expect("write");
        let report = export_content_report(&ctx);
        let t = tally(std::slice::from_ref(&report));
        assert!(t.gate_failures.is_empty(), "{what}: {}", report.render());
        assert_eq!(t.could_not_run.len(), 1, "{what}: {}", report.render());
        assert!(
            t.could_not_run[0].contains(says) && t.could_not_run[0].contains("could not be driven"),
            "{what}: {t:?}"
        );
        assert!(
            !report.render().contains("Traceback"),
            "{what}: {}",
            report.render()
        );
    }
}

/// THE SPY ON THE ENGINE'S ADJUDICATION IS TRANSPARENT (2026-09-29, review
/// of the stage). It hard-coded `_adjudicate_forbidden(tree, records)` and
/// three-field records, so either one changing raised inside the engine's
/// forbidden scan and failed the tree. Here the engine calls it with one more
/// argument and keeps its records as mappings: the scan passes both through
/// untouched, reads the hits from the engine's redacted report instead, and
/// names the planted hit at its line — a finding, as it is.
#[test]
fn the_adjudication_spy_passes_through_what_the_engine_changed() {
    let (repo, ctx, engine) = export_content_fixture();
    let drifted = FAKE_ENGINE
        .replace(
            "def _adjudicate_forbidden(tree, records):\n    return records, {}",
            "def _adjudicate_forbidden(tree, records, label):\n    \
             return [dict(raw=r, report=p) for r, p, _ in records], {}",
        )
        .replace(
            "    records, _ = _adjudicate_forbidden(tree, records)\n    \
             (out / \"forbidden-hits.txt\").write_text(\"\".join(r + \"\\n\" for _, r, _ in records))",
            "    records, _ = _adjudicate_forbidden(tree, records, label)\n    \
             (out / \"forbidden-hits.txt\").write_text(\"\".join(r[\"report\"] + \"\\n\" for r in records))",
        );
    assert!(
        drifted.contains("_adjudicate_forbidden(tree, records, label)")
            && drifted.contains("r[\"report\"]"),
        "the stand-in's adjudication is not where this test looks for it"
    );
    fs::write(engine.join("bin/pub"), drifted).expect("write");
    fs::write(
        repo.root.join("src/leak.rs"),
        format!("fn a() {{}}\n// {}\n", planted(6)),
    )
    .expect("write");
    let report = export_content_report(&ctx);
    let ladder = report.render();
    let t = tally(std::slice::from_ref(&report));
    assert_eq!(t.gate_failures.len(), 1, "{ladder}");
    assert!(t.could_not_run.is_empty(), "{ladder}");
    assert!(
        ladder.contains(
            "  src/leak.rs:2  forbidden content — Planted tokens (baseline/forbidden-content.txt:2)"
        ),
        "{ladder}"
    );
    assert!(!ladder.contains(&planted(6)), "{ladder}");
}

/// EVERY STAGE THAT DRIVES WHAT A BUILD LEAVES NAMES WHAT A FAILED BUILD KEPT
/// FROM RUNNING (2026-09-27, fourth review) — the guard for the class the
/// third review fixed at four sites and the fourth found at nine more. Every
/// stage of the merge contract is run with a driver whose every `build` and
/// `--no-run` fails. Every finding any of them reports is never inherited (a
/// build stops at its first failure), and every stage with a finding stands a
/// COULD NOT RUN row for what the build kept from running — but the stages
/// whose only children ARE builds: the test compile, the driver builds and the
/// L0 gate (whose build is its check). A new stage that drives a built
/// artifact fails here until it says what a failed build kept from running.
#[test]
fn every_stage_names_what_a_failed_build_kept_from_running() {
    const BUILDS_ONLY: [StageId; 3] = [
        StageId::TestCompile,
        StageId::DriverBuilds,
        StageId::FreezeGate,
    ];
    let repo = FakeRepo::new();
    repo.with_stage2(&format!(
        "case \"$*\" in *build*|*--no-run*) {COMPILE_RED} ;; esac\nexit 0"
    ));
    let ctx = repo.ctx(Mode::Fast, Scope::workspace());
    let mut drove = Vec::new();
    for spec in plan::plan(&ctx) {
        let t = tally(std::slice::from_ref(&stages::run_stage(&ctx, &spec)));
        for f in t.all_findings() {
            assert!(
                f.opaque.is_some(),
                "{:?}: a finding behind a failed build is never main's: {f:?}",
                spec.id
            );
        }
        if t.gate_failures.is_empty() || BUILDS_ONLY.contains(&spec.id) {
            continue;
        }
        assert!(
            !t.could_not_run.is_empty(),
            "{:?} drives what a failed build left and names nothing it kept from running: {t:?}",
            spec.id
        );
        drove.push(spec.id);
    }
    // The guard reached the stages it is about.
    for id in [
        StageId::Test,
        StageId::AtpkgTooling,
        StageId::ControlSocketSmoke,
        StageId::RedrawConformance,
    ] {
        assert!(drove.contains(&id), "{id:?} not reached: {drove:?}");
    }
    if cfg!(target_os = "macos") {
        for id in [
            StageId::ObjcDrives,
            StageId::ForegroundHandback,
            StageId::RenderDesync,
        ] {
            assert!(drove.contains(&id), "{id:?} not reached: {drove:?}");
        }
    }
}

/// A stand-in `targo` that does what cargo does with a runner (2026-09-26): for
/// the compile it writes `bins` as test executables and reports each in a JSON
/// `compiler-artifact` line; for `--tests` it prints each binary's `Running`
/// header and, when the argv carries the recording runner, executes the runner
/// with the binary's argv from the binary's package directory with
/// `CARGO_PKG_NAME` set — or, with no runner (the serial child), runs the
/// binary itself and prints cargo's re-run line after a failure. `extra` is
/// shell run first for `--tests`, so a test can bend the recording pass.
/// `bins` are `(package, target flag, name)`; each binary's body is
/// `bodies(name)`.
fn with_recording_cargo(repo: &FakeRepo, bins: &[(&str, &str, &str)], extra: &str) {
    let deps = repo.root.join("target/debug/deps");
    fs::create_dir_all(&deps).expect("mkdir");
    let mut compile = String::new();
    let mut record = String::new();
    let mut serial = String::new();
    for (pkg, flag, name) in bins {
        let exe = deps.join(format!("{name}-1"));
        let (kind, tname) = match flag.split_once(' ') {
            Some((k, n)) => (k.trim_start_matches("--"), n),
            None => ("lib", *name),
        };
        compile.push_str(&format!(
            "printf '%s\\n' '{{\"reason\":\"compiler-artifact\",\"target\":{{\"kind\":[\"{kind}\"],\
             \"name\":\"{tname}\"}},\"profile\":{{\"test\":true}},\"executable\":\"{}\"}}'\n",
            exe.display()
        ));
        let src = if kind == "test" {
            format!("tests/{tname}.rs")
        } else {
            "unittests src/lib.rs".to_string()
        };
        let header = format!("     Running {src} (target/debug/deps/{name}-1)");
        fs::create_dir_all(repo.root.join("crates").join(pkg)).expect("mkdir");
        record.push_str(&format!(
            "echo '{header}' >&2\n(cd '{root}/crates/{pkg}' && CARGO_PKG_NAME={pkg} \"$runner\" \
             \"$flag\" \"$dir\" '{exe}' $targs) || {{ echo \"Caused by:\"; echo \"  process \
             didn't exit successfully: \\`$runner $flag $dir {exe}\\` (exit status: 1)\"; \
             status=101; }}\n",
            root = repo.root.display(),
            exe = exe.display()
        ));
        serial.push_str(&format!(
            "echo '{header}' >&2\n(cd '{root}/crates/{pkg}' && CARGO_PKG_NAME={pkg} '{exe}' \
             $targs) || {{ echo 'error: test failed, to rerun pass `-p {pkg} {flag}`'; \
             status=101; }}\n",
            root = repo.root.display(),
            exe = exe.display()
        ));
    }
    repo.with_stage2(&format!(
        r#"status=0
cfg=""; prev=""; seen=0; targs=""
for a in "$@"; do
  if [ "$seen" = 1 ]; then targs="$targs $a"; fi
  [ "$a" = "--" ] && seen=1
  [ "$prev" = "--config" ] && cfg="$a"
  prev="$a"
done
case "$*" in
  *--no-run*)
    echo '   Compiling fixture v0.1.0' >&2
{compile}    exit 0 ;;
  *--tests*)
{extra}
    if [ -n "$cfg" ]; then
      arr=${{cfg#*=\[\"}}; arr=${{arr%\"\]}}
      runner=${{arr%%\",\"*}}; rest=${{arr#*\",\"}}
      flag=${{rest%%\",\"*}}; dir=${{rest#*\",\"}}
{record}    else
{serial}    fi
    exit $status ;;
esac
exit 0"#
    ));
}

/// THE TEST BINARIES, SEVERAL AT A TIME, END TO END (2026-09-26), through the
/// real recorder (`aterm-verify --record-test-binary`, the binary this package
/// builds) and a stand-in cargo that runs it the way cargo runs a runner.
///
/// Pinned: the world-harness binary ran FIRST and ALONE with the whole thread
/// count, though it prints in its declared place; the shared binaries ran two
/// at a time (the first two meet: each waits for the other to have started)
/// with half the threads each; every binary ran in its own package directory
/// with the environment cargo gave its runner; the compile's JSON is gone from
/// the ladder; and the one failed test is itemized under cargo's own re-run
/// spec, from the combined log's shape.
#[test]
fn the_test_binaries_run_as_cargo_recorded_them_two_at_a_time_and_the_world_alone() {
    let repo = FakeRepo::new();
    let marks = repo.scratch.join("marks");
    fs::create_dir_all(marks.join("running")).expect("mkdir");
    let bins = [
        ("fixture", "--lib", "a"),
        ("aterm-link", "--test world", "world"),
        ("fixture", "--test b", "b"),
        ("fixture", "--test c", "c"),
    ];
    with_recording_cargo(&repo, &bins, "");
    let deps = repo.root.join("target/debug/deps");
    let m = marks.display();
    let libtest_ok = |n: &str| {
        format!(
            "printf '\\nrunning 1 test\\ntest {n}::fine ... ok\\n\\ntest result: ok. 1 passed; 0 \
             failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\\n\\n'"
        )
    };
    let meet = |me: &str, other: &str| {
        format!(
            "touch '{m}/started-{me}'; i=0\n\
             until [ -e '{m}/started-{other}' ]; do i=$((i+1)); [ $i -gt 600 ] && {{ echo \
             '{me}: {other} never ran beside me'; exit 1; }}; sleep 0.05; done"
        )
    };
    let body = |name: &str, rest: &str| {
        format!(
            "#!/bin/sh\necho \"{name}: cwd=$(pwd -P) pkg=$CARGO_PKG_NAME threads=$RUST_TEST_THREADS \
             args=$*\"\ntouch '{m}/running/{name}'\n{rest}\nrm -f '{m}/running/{name}'\n"
        )
    };
    let write = |name: &str, text: String| {
        let p = deps.join(format!("{name}-1"));
        fs::write(&p, text).expect("write");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).expect("chmod");
    };
    write(
        "a",
        body("a", &format!("{}\n{}", meet("a", "b"), libtest_ok("a"))),
    );
    write(
        "b",
        body(
            "b",
            &format!(
                "{}\nprintf '\\nrunning 2 tests\\ntest t::fine ... ok\\ntest t::broke ... \
                 FAILED\\n\\nfailures:\\n\\n---- t::broke stdout ----\\n\\nthread '\"'\"'t::broke'\"'\"' \
                 panicked at src/b.rs:3:5:\\nassertion failed: broke\\n\\nfailures:\\n    \
                 t::broke\\n\\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 \
                 filtered out; finished in 0.30s\\n\\n'\nrm -f '{m}/running/b'\nexit 101",
                meet("b", "a")
            ),
        ),
    );
    write("c", body("c", &libtest_ok("c")));
    write(
        "world",
        body(
            "world",
            &format!(
                "for k in 1 2 3; do n=$(ls '{m}/running' | grep -vx world | wc -l | tr -d ' '); \
                 [ \"$n\" = 0 ] || {{ echo \"world: $n binaries beside me\"; exit 1; }}; sleep \
                 0.1; done\necho 'world: alone'\n{}",
                libtest_ok("world")
            ),
        ),
    );

    let mut ctx = repo.ctx(Mode::Fast, Scope::workspace());
    ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
    ctx.test_jobs = 2;
    ctx.test_threads = Some(6);
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::Test)
        .expect("the test stage");
    let report = stages::run_stage(&ctx, &spec);
    let block = report.render();

    assert_eq!(
        decisions(&block),
        [
            ("ok", "targo test --workspace --no-run (trustdoc)"),
            ("FAIL", "targo test --workspace --tests (trustdoc)"),
        ],
        "{block}"
    );
    assert!(block.contains("   Compiling fixture v0.1.0"), "{block}");
    assert!(
        !block.contains("{\"reason\""),
        "the JSON leaves the ladder:\n{block}"
    );
    assert!(
        block.contains(
            "  test binaries: 4 — cargo recorded each one's argv, directory and environment \
             (a runner in its place) and the gate ran them: 1 alone first \
             (RUST_TEST_THREADS=6: -p aterm-link --test world), then 3 shared, 2 at a time \
             (RUST_TEST_THREADS=3 each)"
        ),
        "{block}"
    );
    let at = |needle: &str| {
        block
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle:?} in:\n{block}"))
    };
    assert!(
        at("Running unittests src/lib.rs (target/debug/deps/a-1)")
            < at("Running tests/world.rs (target/debug/deps/world-1)")
            && at("Running tests/world.rs") < at("Running tests/b.rs")
            && at("Running tests/b.rs") < at("Running tests/c.rs"),
        "printed in cargo's order, whatever order they ran in:\n{block}"
    );
    assert!(block.contains("world: alone"), "{block}");
    for (name, pkg, threads) in [
        ("a", "fixture", 3),
        ("b", "fixture", 3),
        ("c", "fixture", 3),
        ("world", "aterm-link", 6),
    ] {
        let want = format!(
            "{name}: cwd={} pkg={pkg} threads={threads} args=--skip measuring:: --skip \
             launchd_copy_tests::",
            fs::canonicalize(repo.root.join("crates").join(pkg))
                .expect("canonical")
                .display()
        );
        assert!(block.contains(&want), "missing {want:?} in:\n{block}");
    }
    assert!(!block.contains("never ran beside me"), "{block}");
    assert!(
        block.contains("error: test failed, to rerun pass `-p fixture --test b`\n"),
        "{block}"
    );
    let t = tally(std::slice::from_ref(&report));
    assert_eq!(
        t.gate_failures,
        ["targo test --workspace --tests (trustdoc)"]
    );
    assert_eq!(
        t.findings_of(0)
            .iter()
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>(),
        ["-p fixture --test b -- t::broke"],
        "{block}"
    );
    assert!(t.could_not_run.is_empty(), "{t:?}");
}

/// NEVER LESS THAN CARGO (2026-09-26): a recording that does not add up runs
/// the serial `--tests` child instead, and says why — cargo announcing a
/// binary the recorder never saw, or the recorder failing — and a pass in
/// which the recorder never ran (a runner the caller's config names outranks
/// ours) is judged as the serial run it was. In every case the tests RAN and
/// the one failure is decided exactly once.
#[test]
fn a_recording_that_does_not_add_up_falls_back_to_cargos_serial_run() {
    let bins = [("fixture", "--test b", "b"), ("fixture", "--test c", "c")];
    let cases = [
        (
            "echo '     Running tests/ghost.rs (target/debug/deps/ghost-1)' >&2",
            "run by cargo, one at a time — the recording did not add up: cargo announced 3 \
             test binaries and the recorder saw 2",
        ),
        (
            "if [ -n \"$cfg\" ]; then cfg=$(printf '%s' \"$cfg\" | sed 's#\",\"/#\",\"/nonexistent/#'); fi",
            "run by cargo, one at a time — the recording pass failed:",
        ),
        (
            "cfg=''",
            "run by cargo, one at a time — the recording runner was not used",
        ),
    ];
    for (extra, why) in cases {
        let repo = FakeRepo::new();
        with_recording_cargo(&repo, &bins, extra);
        let deps = repo.root.join("target/debug/deps");
        for (name, code, verdict) in [("b", 101, "FAILED"), ("c", 0, "ok")] {
            let p = deps.join(format!("{name}-1"));
            let failed = if code == 0 {
                String::new()
            } else {
                "\\nfailures:\\n\\n---- t::x stdout ----\\n\\nthread '\"'\"'t::x'\"'\"' panicked at \
                 src/x.rs:1:1:\\nno\\n\\nfailures:\\n    t::x\\n"
                    .to_string()
            };
            let (passed, nfailed) = if code == 0 { (1, 0) } else { (0, 1) };
            fs::write(
                &p,
                format!(
                    "#!/bin/sh\necho RAN-{name}\nprintf '\\nrunning 1 test\\ntest t::x ... \
                     {verdict}\\n{failed}\\ntest result: {}. {passed} passed; {nfailed} failed; 0 \
                     ignored; 0 measured; 0 filtered out; finished in 0.01s\\n\\n'\nexit {code}\n",
                    if code == 0 { "ok" } else { "FAILED" }
                ),
            )
            .expect("write");
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let mut ctx = repo.ctx(Mode::Fast, Scope::workspace());
        ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::Test)
            .expect("the test stage");
        let report = stages::run_stage(&ctx, &spec);
        let block = report.render();
        assert!(block.contains(why), "{extra}: missing {why:?} in:\n{block}");
        assert!(
            block.contains("RAN-b") && block.contains("RAN-c"),
            "{block}"
        );
        let t = tally(std::slice::from_ref(&report));
        assert_eq!(
            t.gate_failures,
            ["targo test --workspace --tests (trustdoc)"],
            "{extra}:\n{block}"
        );
        assert_eq!(
            t.findings_of(0)
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>(),
            ["-p fixture --test b -- t::x"],
            "{extra}:\n{block}"
        );
    }
}

/// A RUNNER THAT RUNS NO TEST IS NO PASS (2026-09-27, third review). A runner
/// for the host named in a cargo config or the environment
/// (`CARGO_TARGET_<TRIPLE>_RUNNER`) outranks the gate's `cfg(all())` recording
/// runner, so the recording pass is judged as the serial run it was — or, when
/// it printed no result, the serial child runs, through the same runner. One
/// that runs nothing exits 0 for every binary: the row was `ok`, and the merge
/// contract claimed, with no test run. Here the stand-in cargo's runner is
/// outranked (`cfg=''`) and each binary "runs" as such a runner would: it
/// prints nothing and exits 0. The row is COULD NOT RUN, and says why.
#[test]
fn a_runner_that_ran_no_test_is_could_not_run_never_ok() {
    let repo = FakeRepo::new();
    let bins = [("fixture", "--test b", "b"), ("fixture", "--test c", "c")];
    with_recording_cargo(&repo, &bins, "cfg=''");
    let deps = repo.root.join("target/debug/deps");
    for name in ["b", "c"] {
        let p = deps.join(format!("{name}-1"));
        fs::write(&p, "#!/bin/sh\nexit 0\n").expect("write");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let mut ctx = repo.ctx(Mode::Fast, Scope::workspace());
    ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::Test)
        .expect("the test stage");
    let report = stages::run_stage(&ctx, &spec);
    let block = report.render();
    assert!(
        !decisions(&block).contains(&("ok", "targo test --workspace --tests (trustdoc)")),
        "a run of no test passed:\n{block}"
    );
    let t = tally(std::slice::from_ref(&report));
    assert!(t.gate_failures.is_empty(), "{t:?}");
    assert_eq!(t.could_not_run.len(), 1, "{block}");
    assert!(
        t.could_not_run[0].starts_with("targo test --workspace --tests (trustdoc) — could not run")
            && t.could_not_run[0].contains("CARGO_TARGET_<TRIPLE>_RUNNER"),
        "{t:?}"
    );
}

/// A HUNG BINARY IS NAMED BY ITS OWN CEILING (2026-09-27): each binary the gate
/// runs is a stage child of its own, so the wall-clock ceiling ends the one that
/// hung — its TIMEOUT block names THAT binary (from the `Running` header the
/// gate wrote at the head of its log) and the test libtest said was still
/// running, with the `--exact` line to re-run it alone — while the binaries
/// beside it finish and print in cargo's order, and the row is one FAIL.
#[test]
fn a_hung_test_binary_is_named_by_its_own_timeout_and_the_rest_still_run() {
    let repo = FakeRepo::new();
    let bins = [
        ("fixture", "--test fine", "fine"),
        ("fixture", "--test hang", "hang"),
        ("fixture", "--test after", "after"),
    ];
    with_recording_cargo(&repo, &bins, "");
    let deps = repo.root.join("target/debug/deps");
    let ok = |n: &str| {
        format!(
            "#!/bin/sh\necho RAN-{n}\nprintf '\\nrunning 1 test\\ntest {n}::fine ... ok\\n\\ntest \
             result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in \
             0.01s\\n\\n'\n"
        )
    };
    for (name, body) in [
        ("fine", ok("fine")),
        (
            "hang",
            "#!/bin/sh\nprintf '\\nrunning 2 tests\\ntest h::quick ... ok\\ntest h::wedged has been \
             running for over 60 seconds\\n'\nexec sleep 600\n"
                .to_string(),
        ),
        ("after", ok("after")),
    ] {
        let p = deps.join(format!("{name}-1"));
        fs::write(&p, body).expect("write");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let mut ctx = repo.ctx(Mode::Fast, Scope::workspace());
    ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
    ctx.test_jobs = 2;
    // Generous for every child that finishes (a shell script each, on a loaded
    // machine); only the wedged binary ever reaches it.
    ctx.child_ceiling = Some(std::time::Duration::from_secs(20));
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::Test)
        .expect("the test stage");
    let report = stages::run_stage(&ctx, &spec);
    let block = report.render();

    assert_eq!(
        decisions(&block),
        [
            ("ok", "targo test --workspace --no-run (trustdoc)"),
            ("FAIL", "targo test --workspace --tests (trustdoc)"),
        ],
        "{block}"
    );
    assert!(
        block.contains(
            "  test binary: tests/hang.rs (target/debug/deps/hang-1) — it never printed its \
             `test result:` line\n"
        ),
        "the TIMEOUT names the binary that hung, not the last one started:\n{block}"
    );
    assert!(block.contains("h::wedged"), "{block}");
    assert!(
        block
            .contains("    re-run alone: target/debug/deps/hang-1 --exact h::wedged --nocapture\n"),
        "{block}"
    );
    assert!(
        !block.contains("test binary: tests/after.rs"),
        "a binary that finished is never the one named:\n{block}"
    );
    let at = |needle: &str| {
        block
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle:?} in:\n{block}"))
    };
    assert!(
        at("RAN-fine") < at("Running tests/hang.rs")
            && at("Running tests/hang.rs") < at("RAN-after"),
        "the binaries beside the hung one ran and print in cargo's order:\n{block}"
    );
    assert!(
        block.contains("error: test failed, to rerun pass `-p fixture --test hang`\n"),
        "{block}"
    );
    let t = tally(std::slice::from_ref(&report));
    assert_eq!(
        t.gate_failures,
        ["targo test --workspace --tests (trustdoc)"],
        "{block}"
    );
}

/// MEASURING THE TEST RUN ON THIS TREE (2026-09-26) — opt-in and never part of
/// any gate run: the REAL test stage (compile, record, run) over the crates
/// `ATERM_VERIFY_MEASURE_CRATES` names (comma-separated), `--test-jobs` =
/// `ATERM_VERIFY_MEASURE_JOBS` (default the gate's), with the load at both
/// ends. `ATERM_VERIFY_MEASURE_SERIAL=1` runs the serial child it replaced
/// instead — cargo's own `--tests` run, the same argv and environment — as the
/// baseline. Both print the run's findings, and `ATERM_VERIFY_MEASURE_LOG`
/// names a file for the stage's whole block:
///
/// ```text
/// ATERM_VERIFY_MEASURE_CRATES=atpkg,aterm-update ATERM_VERIFY_MEASURE_JOBS=2 \
///   targo --unverified test -p aterm-verify --test gate_contract -- --ignored \
///   measure_the_test_run_on_this_tree --nocapture
/// ```
#[test]
#[ignore = "measures this machine: run by hand with ATERM_VERIFY_MEASURE_CRATES"]
fn measure_the_test_run_on_this_tree() {
    let crates: Vec<String> = std::env::var("ATERM_VERIFY_MEASURE_CRATES")
        .expect("ATERM_VERIFY_MEASURE_CRATES names the crates to measure")
        .split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    let root = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .expect("the repository root");
    let scratch = mktemp_dir("atv-measure").expect("mktemp");
    let mut ctx = Ctx::new(
        root,
        Mode::Fast,
        Scope::changed("HEAD", crates, true),
        EnvSnapshot::capture(),
        scratch.clone(),
    )
    .with_pinned_child_facts(None);
    ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
    if let Some(jobs) = std::env::var("ATERM_VERIFY_MEASURE_JOBS")
        .ok()
        .and_then(|j| j.parse().ok())
    {
        ctx.test_jobs = jobs;
    }
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::Test)
        .expect("the test stage");
    let serial = std::env::var_os("ATERM_VERIFY_MEASURE_SERIAL").is_some();
    let load_start = aterm_verify::exec::load_average();
    let started = std::time::Instant::now();
    let report = if serial {
        let cmd = aterm_verify::exec::Cmd::new(&ctx.tools.targo)
            .args(stages::test_run_args(&ctx.scope))
            .env("RUSTDOC", ctx.tools.trustdoc.as_os_str());
        let run = aterm_verify::exec::run(&cmd, ctx.exec_env());
        let mut r = Report::new(spec.title.clone());
        r.decide_test_child(&run, "targo test --tests (serial, as before 2026-09-26)");
        r
    } else {
        stages::run_stage(&ctx, &spec)
    };
    let took = started.elapsed();
    let block = report.render();
    if let Some(log) = std::env::var_os("ATERM_VERIFY_MEASURE_LOG") {
        fs::write(&log, &block).expect("the stage's block");
    }
    let summary: Vec<&str> = block
        .lines()
        .filter(|l| {
            l.starts_with("  ok ")
                || l.starts_with("  FAIL")
                || l.starts_with("  skip")
                || l.starts_with("  test binaries:")
        })
        .collect();
    let findings: Vec<String> = tally(std::slice::from_ref(&report))
        .all_findings()
        .map(|f| format!("  finding: {}", f.id))
        .collect();
    println!(
        "measured: test {} {:.1}s (load {:?} -> {:?})\n{}\n{}",
        if serial {
            "run, serial".to_string()
        } else {
            format!("stage, --test-jobs {}", ctx.test_jobs)
        },
        took.as_secs_f64(),
        load_start,
        aterm_verify::exec::load_average(),
        summary.join("\n"),
        findings.join("\n")
    );
    fs::remove_dir_all(&scratch).ok();
}
