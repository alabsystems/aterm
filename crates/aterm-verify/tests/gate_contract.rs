// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CLI, end to end, over a synthetic repo: plan, scheduler, stages, the
//! per-check lines and the exit code, driven by `#!/bin/sh` stand-ins for the
//! toolchain and the binaries the checks drive.
#![cfg(unix)]

// The shared fixture half, mounted once by `tests/contract.rs`.
use crate::common;

use std::fs;
use std::path::PathBuf;

use aterm_verify::checks::{CHECKS, Check};
use aterm_verify::ladder::{Report, Tally, tally};
use aterm_verify::plan::{Lane, StageId, StageSpec};
use aterm_verify::{Ctx, EnvSnapshot, Scope, Show, exit, mktemp_dir, plan, sched, stages};

/// A wait for something that must happen is a hang detector, never a latency budget.
const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

/// Every check a fixture can stand in for in one run: all but the ones that
/// read the real tree in-process (`fmt`, `guards`, `census`, `forge`,
/// `cells`, each unit-tested where it lives), the node-driven `wasm-smoke` and
/// `wasm-glue`, the bundle's `measure`, the sealed rung and the machine-store
/// `codex`.
fn land() -> Vec<Check> {
    CHECKS
        .iter()
        .map(|s| s.check)
        .filter(|c| {
            !matches!(
                c,
                Check::Fmt
                    | Check::Guards
                    | Check::Census
                    | Check::Forge
                    | Check::Cells
                    | Check::WasmSmoke
                    | Check::WasmGlue
                    | Check::Measure
                    | Check::Sealed
                    | Check::Codex
            )
        })
        .collect()
}

/// Every check.
fn every() -> Vec<Check> {
    CHECKS.iter().map(|s| s.check).collect()
}

/// A repo-shaped directory: stand-ins for the binaries the CLI drives, all
/// passing.
struct FakeRepo {
    root: PathBuf,
    stage2: PathBuf,
    scratch: PathBuf,
    /// The fixture's own HOME.
    home: PathBuf,
    /// The control socket the answering smoke's `aterm-gui` links to, held
    /// listening for the fixture's life.
    ctl_sock: PathBuf,
    _listener: std::os::unix::net::UnixListener,
}

/// The driver lane's build dir in a fixture: `<root>/target/verify/drivers`.
const DRIVERS: &str = "target/verify/drivers";

impl FakeRepo {
    fn new() -> Self {
        let base = mktemp_dir("atv-repo").expect("mktemp");
        let root = base.join("repo");
        let stage2 = base.join("stage2");
        let scratch = base.join("scratch");
        let home = base.join("home");
        for d in [&root, &stage2, &scratch, &home] {
            fs::create_dir_all(d).expect("mkdir");
        }
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
        // Every binary the CLI builds and then DRIVES, present and passing, so
        // an unrelated test never reads a missing binary as a finding.
        for rel in driven_stubs() {
            me.driver_stub(&rel, 0);
        }
        fs::create_dir_all(me.root.join(DRIVERS).join("debug")).expect("mkdir");
        me.script(&format!("{DRIVERS}/debug/aterm"), "echo 'aterm: stub'");
        me
    }

    /// A stand-in for a binary the driver lane builds and a stage then DRIVES
    /// — `rel` under the lane's `debug/` — that exits `code`.
    fn driver_stub(&self, rel: &str, code: i32) -> &Self {
        self.driver_script(rel, &format!("echo '{rel}: stub'; exit {code}"))
    }

    /// A stand-in driven binary with its own `body`, as data behind the one
    /// shared stand-in text ([`Self::script_body`]).
    fn driver_script(&self, rel: &str, body: &str) -> &Self {
        self.script_body(&format!("{DRIVERS}/debug/{rel}"), body);
        self
    }

    fn script(&self, rel: &str, body: &str) {
        common::standin(&self.root.join(rel), body);
    }

    /// [`Self::script`] for a body that is not shared: data beside the one
    /// constant stand-in ([`common::standin_body`]).
    fn script_body(&self, rel: &str, body: &str) {
        common::standin_body(&self.root.join(rel), body);
    }

    /// A stage2 whose driver also produces the two binaries the headless smoke
    /// drives ([`common::answering_smoke`]), so the smoke runs for real.
    fn with_answering_smoke(&self) -> &Self {
        self.with_stage2_body(&format!(
            "echo \"argv: $*\"\n{}\n{}\nexit 0",
            common::answering_smoke(&self.ctl_sock),
            answering_ty_conformances()
        ))
    }

    /// Install a stand-in Trust stage2 whose `targo` runs `targo_body`.
    fn with_stage2(&self, targo_body: &str) -> &Self {
        for (name, body) in [("targo", targo_body), ("trustdoc", "exit 0")] {
            common::standin(&self.stage2.join(name), body);
        }
        self
    }

    /// [`Self::with_stage2`] with `targo_body` as data behind the one shared
    /// stand-in text, for a body that bakes in a path or an argv of its own.
    fn with_stage2_body(&self, targo_body: &str) -> &Self {
        common::standin_body(&self.stage2.join("targo"), targo_body);
        common::standin(&self.stage2.join("trustdoc"), "exit 0");
        self
    }

    fn ctx(&self, checks: &[Check], scope: Scope) -> Ctx {
        self.ctx_with(checks, scope, |_| {})
    }

    /// [`Self::ctx`] with the last say over the environment the stages read.
    fn ctx_with(
        &self,
        checks: &[Check],
        scope: Scope,
        tweak: impl FnOnce(&mut EnvSnapshot),
    ) -> Ctx {
        common::fixture_ctx(
            &self.root,
            &self.stage2,
            &self.scratch,
            &self.home,
            checks,
            scope,
            tweak,
        )
    }

    /// [`Self::ctx`] with the windowed stages RUNNING (`--no-windows` off,
    /// which the fixture otherwise sets): this repo's stand-in drivers open
    /// nothing.
    fn ctx_with_windows(&self, checks: &[Check], scope: Scope) -> Ctx {
        self.ctx(checks, scope).with_no_windows(false)
    }

    /// Run `checks` over `scope`: the full ladder (the log) followed by the
    /// check lines and the verdict (stdout), and the exit code.
    fn run(&self, checks: &[Check], scope: Scope) -> (String, i32) {
        run_ctx(&self.ctx(checks, scope))
    }
}

/// The `spec` check's ty-only conformance run, answered with a pass for each
/// test it names ([`stages::TY_ONLY_CONFORMANCES`]).
fn answering_ty_conformances() -> String {
    let passes: String = stages::TY_ONLY_CONFORMANCES
        .iter()
        .map(|(_, test)| format!("echo 'test conformance::{test} ... ok'; "))
        .collect();
    format!("case \"$*\" in *_under_ty*) {passes};; esac")
}

/// Run `ctx` as the CLI runs it: the log, then stdout, and the exit code.
fn run_ctx(ctx: &Ctx) -> (String, i32) {
    let (mut out, mut log): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    let code = aterm_verify::run(ctx, &mut out, &mut log, &Show::default()).expect("writable");
    let text = format!(
        "{}\n{}",
        String::from_utf8(log).expect("utf-8 log"),
        String::from_utf8(out).expect("utf-8 out")
    );
    (text, code)
}

impl Drop for FakeRepo {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            fs::remove_dir_all(base).ok();
        }
    }
}

/// Every driven binary's path under the driver lane's `debug/`: the redraw
/// harness and the objc drivers' examples, named by the stages themselves.
fn driven_stubs() -> Vec<String> {
    std::iter::once(stages::REDRAW_CONFORMANCE_BIN.to_string())
        .chain(stages::OBJC_DRIVES.iter().map(|d| objc_stub(d.example)))
        .collect()
}

/// An objc driver's built example, under the driver lane's `debug/`.
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

/// Every ladder decision, as `("ok"|"skip"|"FAIL"|"NOT RUN", label)`.
fn decisions(ladder: &str) -> Vec<(&str, &str)> {
    ladder
        .lines()
        .filter_map(|l| {
            for tag in ["ok", "skip", "FAIL", "NOT RUN"] {
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

/// A CHECK'S LINE IS PRINTED THE MOMENT ITS CHECK FINISHES, not when its turn
/// in the ladder comes. The test compile here blocks until the guards' FAIL
/// line is on stdout (a file), bounded: a run that holds lines back until the
/// slow check ends never shows it, so the compile fails at its bound and the
/// test check's line says so. The log keeps the ladder in declared order.
///
/// A finding is a failed check, not a broken machine: exit `1`, its line
/// names the failing row, its findings carry what the child printed, and its
/// rerun is the check alone.
#[test]
fn a_check_line_is_printed_the_moment_its_check_finishes() {
    let repo = FakeRepo::new();
    let out_path = repo.scratch.join("stdout.txt");
    repo.with_stage2_body(&format!(
        "case \"$*\" in\n  *'--export'*)\n    \
         echo '[made-up-rule] crates/x/src/a.rs:3: a banned token'; exit 1 ;;\n  \
         *'--no-run'*)\n    i=0\n    \
         while ! grep -q '^FAIL  *export' '{out}' && [ \"$i\" -lt 1200 ]; do\n      \
         sleep 0.05; i=$((i + 1))\n    done\n    \
         grep -q '^FAIL  *export' '{out}' || \
         {{ echo 'the export line was never readable while the build ran'; exit 1; }} ;;\nesac\nexit 0",
        out = out_path.display()
    ));
    let ctx = repo.ctx(&[Check::Export, Check::Test], Scope::workspace());
    let mut out = fs::File::create(&out_path).expect("stdout file");
    let mut log: Vec<u8> = Vec::new();
    let code = aterm_verify::run(&ctx, &mut out, &mut log, &Show::default()).expect("writable");
    let printed = fs::read_to_string(&out_path).expect("stdout");
    let log = String::from_utf8(log).expect("utf-8");
    assert_eq!(code, exit::FAILED, "{printed}\n{log}");
    let at = |text: &str, needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("no {needle:?} in:\n{text}"))
    };
    let export = at(&printed, "FAIL    export");
    let test = at(&printed, "ok      test");
    assert!(export < test, "the export line came first:\n{printed}");
    assert!(
        printed.starts_with("verify: test export — the whole workspace\n"),
        "{printed}"
    );
    assert!(
        printed.contains("  ship check --export: the publication engine refuses HEAD's export\n"),
        "{printed}"
    );
    assert!(
        printed.contains("[made-up-rule] crates/x/src/a.rs:3: a banned token"),
        "the finding carries what the child printed: {printed}"
    );
    assert!(
        printed.contains("rerun: targo --unverified verify export\n"),
        "{printed}"
    );
    assert!(
        printed.contains("verify: FAIL — export failed; 1 ok in "),
        "{printed}"
    );
    // The log is the ladder, in declared order whatever finished first.
    assert!(
        at(&log, "=== test compile (--workspace) ===") < at(&log, "=== export guards ("),
        "{log}"
    );
}

/// `--json`: one object per check as it finishes, then one summary object;
/// nothing else on stdout.
#[test]
fn json_is_one_object_per_check_then_a_summary() {
    let repo = FakeRepo::new();
    repo.with_stage2_body(
        "case \"$*\" in *'--export'*) \
         echo '[licence-header] crates/x/src/a.rs:1: no SPDX line'; exit 1 ;; esac\nexit 0",
    );
    let ctx = repo.ctx(&[Check::Libc, Check::Export], Scope::workspace());
    let (mut out, mut log): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    let code = aterm_verify::run(
        &ctx,
        &mut out,
        &mut log,
        &Show {
            json: true,
            log: Some("/x/run.log"),
            ..Show::default()
        },
    )
    .expect("writable");
    let out = String::from_utf8(out).expect("utf-8");
    assert_eq!(code, exit::FAILED, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(
        lines.iter().all(|l| l.starts_with('{') && l.ends_with('}')),
        "{out}"
    );
    let libc = lines
        .iter()
        .find(|l| l.contains("\"name\":\"libc\""))
        .expect("libc");
    assert!(
        libc.starts_with("{\"type\":\"check\",\"name\":\"libc\",\"outcome\":\"ok\""),
        "{libc}"
    );
    let export = lines
        .iter()
        .find(|l| l.contains("\"name\":\"export\""))
        .expect("export");
    assert!(export.contains("\"outcome\":\"fail\""), "{export}");
    assert!(
        export.contains(
            "\"id\":\"ship check --export: the publication engine refuses HEAD's export\""
        ),
        "{export}"
    );
    assert!(export.contains("no SPDX line"), "{export}");
    assert!(
        export.contains("\"rerun\":\"targo --unverified verify export\""),
        "{export}"
    );
    assert!(
        lines[2].starts_with("{\"type\":\"summary\",\"outcome\":\"fail\",\"exit\":1,\"checks\":2,\"ok\":1,\"failed\":1"),
        "{}",
        lines[2]
    );
    assert!(
        lines[2].ends_with(",\"selection\":\"the whole tree\",\"log\":\"/x/run.log\"}"),
        "{}",
        lines[2]
    );
}

/// `fmt` hands every file git tracks to the stage2's `trustfmt`, makes each
/// file it would change a finding, and under `--fix` rewrites exactly those
/// files.
#[test]
fn fmt_reports_what_trustfmt_would_change_and_fix_rewrites_only_that() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let trace = repo.scratch.join("trustfmt-writes");
    common::standin_body(
        &repo.stage2.join("trustfmt"),
        &format!(
            "check=; [ \"$1\" = --check ] && check=1\nrc=0\nfor f in \"$@\"; do\n  \
             case \"$f\" in *b.rs)\n    \
             if [ -n \"$check\" ]; then echo \"Diff in $PWD/$f:1:\"; rc=1; \
             else echo \"$f\" >> {}; fi ;;\n  esac\ndone\nexit $rc",
            sh_quote(&trace.display().to_string())
        ),
    );
    fs::write(repo.root.join("rustfmt.toml"), "edition = \"2024\"\n").expect("rustfmt.toml");
    fs::create_dir_all(repo.root.join("src")).expect("mkdir");
    for f in ["src/a.rs", "src/b.rs"] {
        fs::write(repo.root.join(f), "fn main() {}\n").expect("write");
    }
    for args in [&["init", "-q"][..], &["add", "src"]] {
        let git = aterm_uds::spawnfd::command("git")
            .args(args)
            .current_dir(&repo.root)
            .status()
            .expect("git");
        assert!(git.success(), "git {args:?}");
    }

    let (out, code) = repo.run(&[Check::Fmt], Scope::workspace());
    assert_eq!(code, exit::FAILED, "{out}");
    assert!(
        out.contains(
            "trustfmt: 1 of 2 files unformatted (fix: targo --unverified verify fmt --fix)"
        ),
        "{out}"
    );
    assert!(
        out.contains("        src/b.rs\n"),
        "the file is the finding: {out}"
    );
    assert!(!trace.exists(), "a check writes nothing");

    let (out, code) = run_ctx(&repo.ctx(&[Check::Fmt], Scope::workspace()).with_fix(true));
    assert_eq!(code, exit::PASS, "{out}");
    assert!(
        out.contains("trustfmt --fix: 1 of 2 files reformatted"),
        "{out}"
    );
    assert_eq!(fs::read_to_string(&trace).expect("written"), "src/b.rs\n");
}

/// The checks that read the tree in-process refuse a root that is not an
/// aterm checkout rather than pass over nothing.
#[test]
fn the_in_process_checks_refuse_a_root_that_is_not_an_aterm_checkout() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let checks = [Check::Guards, Check::Census, Check::Forge, Check::Cells];
    let (out, code) = repo.run(&checks, Scope::workspace());
    assert_eq!(code, exit::COULD_NOT_RUN, "{out}");
    for c in checks {
        let line = out
            .lines()
            .find(|l| l.starts_with(&format!("NOT RUN {}", c.name())))
            .unwrap_or_else(|| panic!("{}: {out}", c.name()));
        assert!(line.contains("is not an aterm checkout"), "{line}");
    }
}

/// A skip prints its reason and fix and fails nothing — except under
/// `release`, where publication needs every check to have run: exit 3.
#[test]
fn a_skip_fails_nothing_except_under_release() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    // No targo-tippy beside the stand-in targo: the lint is a named skip.
    let (dev, code) = repo.run(&[Check::Lint, Check::Export], Scope::workspace());
    assert_eq!(code, exit::PASS, "{dev}");
    assert!(dev.contains("skip    lint"), "{dev}");
    assert!(dev.contains("fix: "), "a skip names its fix: {dev}");
    assert!(
        dev.contains("verify: PASS — 1 ok, 1 skipped (lint) in "),
        "{dev}"
    );
    let ctx = repo
        .ctx(&[Check::Lint, Check::Export], Scope::workspace())
        .with_release(true);
    let (release, code) = run_ctx(&ctx);
    assert_eq!(code, exit::COULD_NOT_RUN, "{release}");
    assert!(
        release.contains("verify: COULD NOT RUN — `release` needs every check, and lint skipped"),
        "{release}"
    );
}

/// An empty change cone runs the whole-tree checks and skips the ones that
/// read the selection, each naming the fix.
#[test]
fn an_empty_cone_skips_the_selection_checks_with_their_fix() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let cone = Scope::changed("origin/main", Vec::new(), false);
    let ctx = repo.ctx(&[Check::Test, Check::Doc, Check::Export], cone);
    let (mut out, mut log): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    let code = aterm_verify::run(
        &ctx,
        &mut out,
        &mut log,
        &Show {
            cone: Some("the change cone since origin/main is empty: no workspace crate changed"),
            ..Show::default()
        },
    )
    .expect("writable");
    let out = String::from_utf8(out).expect("utf-8");
    assert_eq!(code, exit::PASS, "{out}");
    assert!(
        out.starts_with(
            "verify: test doc export — the change cone since origin/main is empty: no \
             workspace crate changed\n"
        ),
        "{out}"
    );
    assert!(out.contains("ok      export"), "{out}");
    for check in ["test", "doc"] {
        let line = out
            .lines()
            .find(|l| l.starts_with(&format!("skip    {check}")))
            .unwrap_or_else(|| panic!("{check}: {out}"));
        assert!(line.contains("fix: pass -p <crate> or --all"), "{line}");
    }
}

#[test]
fn the_log_prints_every_stage_in_the_declared_order_however_they_ran() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let (ladder, _) = repo.run(&every(), Scope::workspace());
    let ctx = repo.ctx(&every(), Scope::workspace());
    let expected: Vec<String> = plan::plan(&ctx).into_iter().map(|s| s.title).collect();
    assert_eq!(headers(&ladder), expected);
}

#[test]
fn a_run_with_no_driver_fails_closed() {
    // The bare machine: the test compile COULD NOT RUN, and the checks that
    // need the driver skip by name.
    let repo = FakeRepo::new();
    let (ladder, code) = repo.run(&land(), Scope::workspace());
    assert_eq!(code, exit::COULD_NOT_RUN, "{ladder}");
    let failed = labels_with(&ladder, "NOT RUN");
    assert!(
        failed.iter().any(|l| l.contains("holds no targo")),
        "the missing driver is named: {failed:?}"
    );
    for needed in [
        "targo test (no targo)",
        "targo test --doc (no targo)",
        "driver builds (no targo)",
        "tail driver builds (no targo)",
        "smoke (no targo)",
    ] {
        assert!(
            labels_with(&ladder, "skip").iter().any(|l| l == needed),
            "missing skip: {needed}"
        );
    }
    assert!(ladder.contains("NOT RUN test"), "{ladder}");
}

#[test]
fn a_failing_driver_fails_every_stage_that_drives_it_and_nothing_else() {
    let repo = FakeRepo::new();
    repo.with_stage2_body("echo 'error: unknown unstable option: `trust-verify`' >&2; exit 1");
    let (ladder, code) = repo.run(&land(), Scope::workspace());

    assert_eq!(code, exit::FAILED);
    for driven in [
        "targo test --workspace --no-run (trustdoc)",
        "targo test --doc --workspace (trustdoc)",
    ] {
        assert!(
            labels_with(&ladder, "FAIL").iter().any(|l| l == driven),
            "expected FAIL: {driven}"
        );
    }
    assert!(
        ladder.contains("unknown unstable option"),
        "the diagnostic reaches the reader"
    );
    // The test run's second child never starts after a failed compile, and
    // the ladder says so without calling it a skip: a COULD NOT RUN row.
    assert!(
        ladder.contains(
            "  NOT RUN targo test --workspace --tests (trustdoc) — the compile above \
             failed"
        ),
        "{ladder}"
    );
    assert!(
        !decisions(&ladder)
            .iter()
            .any(|(_, l)| *l == "targo test --workspace --tests (trustdoc)"),
        "{ladder}"
    );
}

#[test]
fn a_scoped_run_narrows_the_driver_where_the_check_reads_the_selection() {
    let repo = FakeRepo::new();
    // Everything green — including a control-socket smoke that really launches
    // under every precondition it promises, really types and really tears down.
    repo.with_answering_smoke();
    let (ladder, code) = repo.run(&land(), Scope::crate_only("aterm-grid"));

    assert_eq!(code, exit::PASS, "{ladder}");
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
    for check in ["test", "doc"] {
        assert!(
            ladder
                .lines()
                .any(|l| l.starts_with(&format!("ok      {check}")) && l.ends_with("-p aterm-grid")),
            "a selection check's line names the selection: {ladder}"
        );
    }
}

/// NARROWED BY MEMORY: the trust lane's exit 4 — this host's verifier ceiling
/// cannot verify a selected library — is a named SKIP row, the lane's own
/// MEMORY lines above it, never a FAIL row and never a pass about the library.
/// The lane is asked for exactly the selection's libraries.
#[test]
fn a_trust_lane_narrowed_by_memory_is_a_named_skip_never_a_finding() {
    let repo = FakeRepo::new();
    repo.with_stage2_body(
        "case \"$*\" in *'trust run -p aterm-grid --crate-budget'*) \
         echo 'MEMORY: aterm-grid not verified here: 20 GiB of RAM verifies it'; exit 4 ;; \
         *'trust run'*) echo \"unexpected argv: $*\"; exit 1 ;; esac\nexit 0",
    );
    let (ladder, code) = repo.run(&[Check::Trust], Scope::crate_only("aterm-grid"));

    assert_eq!(code, exit::PASS, "a narrowing is not a failure: {ladder}");
    let row = labels_with(&ladder, "skip")
        .into_iter()
        .find(|l| l.starts_with("trust lane: NARROWED BY MEMORY — "))
        .unwrap_or_else(|| panic!("no narrowed trust row: {ladder}"));
    assert!(row.ends_with("(-p aterm-grid: aterm-grid)"), "{row}");
    assert!(
        ladder.contains("MEMORY: aterm-grid not verified here: 20 GiB of RAM verifies it"),
        "{ladder}"
    );
    assert!(labels_with(&ladder, "FAIL").is_empty(), "{ladder}");
    assert!(ladder.contains("skip    trust"), "{ladder}");
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
    repo.with_stage2_body(&format!("echo \"argv: $*\"\n{first_send_only}\nexit 0"));
    let ctx = repo.ctx(&land(), Scope::crate_only("aterm-grid"));
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::ControlSocketSmoke)
        .expect("planned");
    let r = stages::run_stage(&ctx, &spec);
    let ladder = r.render();

    assert_eq!(
        tally(std::slice::from_ref(&r)).gate_failures,
        ["smoke: only 1/30 keys accepted (< 15), too few for a lost wake to register"],
        "{ladder}"
    );
    assert!(
        ladder.contains("  ok    smoke: aterm-ctl cursor -> OK 0 0 1 blinking_block"),
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
        /// the stub's shell on every run.
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
        /// The driven binary, under `target/verify/drivers/debug/`.
        stub: String,
        /// Outcomes beyond `1`, what each decides, and the word the line prints.
        beyond: &'static [(Beyond, Reads, &'static str)],
    }
    let mut rows = vec![Row {
        id: StageId::RedrawConformance,
        label: "aterm-redraw-conformance",
        stub: stages::REDRAW_CONFORMANCE_BIN.to_string(),
        beyond: &[(Beyond::Exit(2), Reads::CouldNotRun, "no event loop")],
    }];
    if cfg!(target_os = "macos") {
        rows.extend([
            Row {
                id: StageId::ObjcDrives,
                label: "objc_window_drive",
                stub: objc_stub("objc_window_drive"),
                beyond: &[(Beyond::Exit(2), Reads::CouldNotRun, "no event loop")],
            },
            // `3` is a PASS that claims less than `0`, in its own words.
            Row {
                id: StageId::ObjcDrives,
                label: "objc_live_class_audit",
                stub: objc_stub("objc_live_class_audit"),
                beyond: &[
                    (Beyond::Exit(2), Reads::CouldNotRun, "no event loop"),
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
    let ctx = repo.ctx_with_windows(&land(), Scope::workspace());
    // EACH ROW RUNS ITS OWN CLEAN STAGE. The objc rows' clean runs are alike
    // only while the stage carries nothing from one run to the next, and each
    // one after the first follows the previous row's failing runs: it is that
    // row's check that a red leaves no trace.
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
                Reads::Pass => ("  ok    ", 0, 0),
                Reads::CouldNotRun => ("  NOT RUN ", 0, 1),
                Reads::Finding => ("  FAIL  ", 1, 0),
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
                r.render().contains(&format!("{tag}{name}: {words}")),
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
    repo.with_stage2_body(&format!(
        "case \"$*\" in *{}*) {COMPILE_RED} ;; esac\nexit 0",
        first.example
    ));
    repo.driver_stub(&objc_stub(second.example), 1);
    let ctx = repo.ctx_with_windows(&land(), Scope::workspace());
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
        t.could_not_run
            .iter()
            .any(|c| c.starts_with(&format!("{} — the build above failed", first.example))),
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
fn the_pure_guards_do_not_wait_for_the_main_lane() {
    // The reason this is a program and not a script: on a real tree the build is
    // minutes and the guards are milliseconds.
    //
    // A RENDEZVOUS, not a stopwatch. Every main-lane stage holds until every
    // pure guard has finished, so the guards must be able to run while the
    // build is in flight: a scheduler that queued them behind the build
    // deadlocks here, and the build's bounded wait turns that into a failure.
    // A stopwatch would fail under load and pass a guards-behind-the-build
    // scheduler anyway: the guards do no work, so serialising them costs
    // nothing a clock can see.
    let repo = FakeRepo::new();
    let ctx = repo.ctx(&every(), Scope::workspace());
    let specs = plan::plan(&ctx);
    let pure = specs.iter().filter(|s| s.lane == Lane::Pure).count();
    assert!(pure > 0, "no pure guards in the plan: nothing to overlap");
    let finished = std::sync::Mutex::new(0_usize);
    let guard_done = std::sync::Condvar::new();
    let starved = std::sync::atomic::AtomicBool::new(false);
    let build_holds_for_the_guards = |s: &StageSpec| {
        match s.lane {
            // Once one main stage has starved, the rest need not each wait out
            // the hang detector to fail the same way.
            Lane::MainTarget if !starved.load(std::sync::atomic::Ordering::SeqCst) => {
                let count = finished.lock().expect("pure count");
                let (_count, wait) = guard_done
                    .wait_timeout_while(count, HANG_DETECTOR, |done| *done < pure)
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
        "the main lane held a minute for {pure} pure guards that could not run beside it: \
         they waited for the build"
    );
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// THE CEILING, end to end: a caller's `CARGO_BUILD_JOBS` reaches the side
/// lane's own child as the smaller of the two, not as the lane's cap and not as
/// the caller's value.
///
/// `lane_jobs`'s unit tests (stages.rs) prove the arithmetic; this proves the
/// wiring — that the number a real stage puts in a real child's environment is
/// the capped one; otherwise only two fixtures failing at exit 71 would notice
/// (see the `cargo_build_jobs` pin in `ctx_with`).
#[test]
fn a_callers_job_count_caps_the_side_lane_child_it_reaches() {
    // 2 is below the driver lane's cap of 8, so the child must see 2; a
    // caller above the cap, or none, sees the cap.
    for (caller, want) in [("2", "2"), ("16", "8"), ("", "8")] {
        let repo = FakeRepo::new();
        let trace = repo.scratch.join("jobs-seen");
        let target = repo.root.join(DRIVERS);
        repo.with_stage2_body(&format!(
            r#"test "$CARGO_TARGET_DIR" = {target} || exit 70
case "$*" in
  '--unverified test -p aterm --features sealed --test two_nodes_sealed --test fabric_world --no-fail-fast')
    echo "$CARGO_BUILD_JOBS" >> {trace}
    ;;
  *) exit 74 ;;
esac
"#,
            target = sh_quote(&target.display().to_string()),
            trace = sh_quote(&trace.display().to_string()),
        ));
        let ctx = repo.ctx_with(&every(), Scope::workspace(), |env| {
            env.cargo_build_jobs = Some(caller.into());
        });
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::SealedLane)
            .expect("sealed stage");
        let report = stages::run_stage(&ctx, &spec);
        let seen = fs::read_to_string(&trace).expect("stage invoked the driver");
        assert_eq!(
            seen,
            format!("{want}\n"),
            "caller {caller:?}: the sealed lane's child sees the capped count"
        );
        assert!(
            !tally(std::slice::from_ref(&report)).failed(),
            "caller {caller:?}: {}",
            report.render()
        );
    }
}

/// A TAKE THAT DECIDED NOTHING IS COULD NOT RUN, NOT A FINDING. The paint
/// rows open a could-not-run or unproved panic with the sentinel, so a
/// measuring child whose every failure says so is a COULD NOT RUN row: exit 3,
/// not 1. The negative control is the same log without the sentinel: a FAIL,
/// exit 1.
#[test]
fn a_paint_probe_that_decided_nothing_is_a_could_not_run_row() {
    let log = |message: &str| {
        format!(
            "     Running tests/paint.rs (target/debug/deps/paint-abc)\n\nrunning 2 tests\n\
             test measuring::main_screen_prompt_with_effect_off_has_no_ribbon ... ok\n\
             test measuring::main_screen_prompt_typing_paints_trail_ink ... FAILED\n\n\
             failures:\n\n---- measuring::main_screen_prompt_typing_paints_trail_ink stdout ----\n\n\
             thread 'measuring::main_screen_prompt_typing_paints_trail_ink' panicked at \
             crates/aterm-conformance/tests/paint.rs:72:24:\n{message}\n\
             note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\n\n\
             failures:\n    measuring::main_screen_prompt_typing_paints_trail_ink\n\n\
             test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 0.01s\n\nerror: test failed, to rerun pass `-p aterm-conformance --test paint`\n"
        )
    };
    let decided_nothing = "PAINT COULD NOT RUN [prompt]: the take decided nothing, which is not \
                           a pass: the control socket listening did not happen within 60s \
                           [sched_ticks=1200 sched_late_max_us=900 wall_ms=60100]";
    for (sentinel, code, word) in [
        (true, exit::COULD_NOT_RUN, "NOT RUN measure"),
        (false, exit::FAILED, "FAIL    measure"),
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
        repo.with_stage2_body(&format!(
            "case \"$*\" in\n  *'-- --ignored measuring::'*) cat '{}'; exit 101 ;;\nesac\nexit 0",
            out.display()
        ));
        let (ladder, got) = repo.run(&[Check::Measure], Scope::workspace());
        assert_eq!(got, code, "sentinel={sentinel}:\n{ladder}");
        let labels = labels_with(&ladder, if sentinel { "NOT RUN" } else { "FAIL" });
        assert!(
            labels.iter().any(|l| l.starts_with(
                "targo test -p aterm-conformance --no-fail-fast --test paint --test spin -- \
                 --ignored measuring::"
            )),
            "sentinel={sentinel}: {labels:?}"
        );
        assert!(ladder.contains(word), "sentinel={sentinel}:\n{ladder}");
    }
}

/// A compile red, as a stub driver prints one: the build of whatever it names
/// fails, every other invocation passes.
const COMPILE_RED: &str = "echo 'error[E0425]: cannot find value `gone` in this scope' >&2; \
     echo '  --> crates/aterm-gui/src/bin/rot.rs:3:13' >&2; exit 101";

/// EVERY STAGE THAT DRIVES WHAT A BUILD LEAVES NAMES WHAT A FAILED BUILD KEPT
/// FROM RUNNING. Every stage is run with a driver whose every `build` and
/// `--no-run` fails, and every stage with a finding stands a COULD NOT RUN row
/// for what the build kept from running — but the stages whose only children
/// ARE builds: the test compile and the two driver builds rows.
#[test]
fn every_stage_names_what_a_failed_build_kept_from_running() {
    const BUILDS_ONLY: [StageId; 3] = [
        StageId::TestCompile,
        StageId::DriverBuilds,
        StageId::TailDriverBuilds,
    ];
    let repo = FakeRepo::new();
    repo.with_stage2_body(&format!(
        "case \"$*\" in *build*|*--no-run*) {COMPILE_RED} ;; esac\nexit 0"
    ));
    let ctx = repo.ctx_with_windows(&land(), Scope::workspace());
    let mut drove = Vec::new();
    for spec in plan::plan(&ctx) {
        let t = tally(std::slice::from_ref(&stages::run_stage(&ctx, &spec)));
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
        StageId::ControlSocketSmoke,
        StageId::RedrawConformance,
    ] {
        assert!(drove.contains(&id), "{id:?} not reached: {drove:?}");
    }
    if cfg!(target_os = "macos") {
        assert!(
            drove.contains(&StageId::ObjcDrives),
            "ObjcDrives not reached: {drove:?}"
        );
    }
}

/// A stand-in `targo` that does what cargo does with a runner: for
/// the compile it writes `bins` as test executables and reports each in a JSON
/// `compiler-artifact` line; for `--tests` it prints each binary's `Running`
/// header and, when the argv carries the recording runner, executes the runner
/// with the binary's argv from the binary's package directory with
/// `CARGO_PKG_NAME` set — or, with no runner (the serial child), runs the
/// binary itself and prints cargo's re-run line after a failure. `extra` is
/// shell run first for `--tests`, so a test can bend the recording pass.
/// `bins` are `(package, target flag, name)`; each binary's body is
/// `bodies(name)`.
///
/// The `targo` bakes in the repo's paths, so it goes in as DATA behind the one
/// shared stand-in text ([`FakeRepo::with_stage2_body`]): as text it was a fresh
/// inode per repo, and each fresh inode's first exec waits on the machine's
/// assessment. Calling it again replaces the `targo` (a case that bends the
/// recording pass differently), so one repo serves every case.
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
    repo.with_stage2_body(&format!(
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

/// THE TEST BINARIES, SEVERAL AT A TIME, END TO END, through the real recorder
/// (`aterm-verify --record-test-binary`, the binary this package builds) and a
/// stand-in cargo that runs it the way cargo runs a runner.
///
/// Pinned: the binaries ran two at a time (two of them meet: each waits for
/// the other to have started) with the whole thread count each, as cargo's
/// own run gives it; every binary ran in its own package's directory (two
/// packages here) with the environment cargo gave its runner; the compile's JSON is gone from
/// the ladder; and the one failed test is itemized under cargo's own re-run
/// spec, from the combined log's shape.
#[test]
fn the_test_binaries_run_as_cargo_recorded_them_two_at_a_time() {
    let repo = FakeRepo::new();
    let marks = repo.scratch.join("marks");
    fs::create_dir_all(marks.join("running")).expect("mkdir");
    let bins = [
        ("fixture", "--lib", "a"),
        ("fixture2", "--test d", "d"),
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
            "echo \"{name}: cwd=$(pwd -P) pkg=$CARGO_PKG_NAME threads=$RUST_TEST_THREADS \
             args=$*\"\ntouch '{m}/running/{name}'\n{rest}\nrm -f '{m}/running/{name}'\n"
        )
    };
    // Each binary bakes in the marks' path, so as text each was a fresh inode
    // and a first exec of its own: they are data behind the shared stand-in.
    let write = |name: &str, text: String| {
        common::standin_body(&deps.join(format!("{name}-1")), &text);
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
    write("d", body("d", &libtest_ok("d")));

    let mut ctx = repo.ctx(&land(), Scope::workspace());
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
            "  test binaries: 4 — RUST_TEST_THREADS=6 each, in cargo's order (no times recorded \
             yet), 2 at a time"
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
            < at("Running tests/d.rs (target/debug/deps/d-1)")
            && at("Running tests/d.rs") < at("Running tests/b.rs")
            && at("Running tests/b.rs") < at("Running tests/c.rs"),
        "printed in cargo's order, whatever order they ran in:\n{block}"
    );
    for (name, pkg, threads) in [
        ("a", "fixture", 6),
        ("b", "fixture", 6),
        ("c", "fixture", 6),
        ("d", "fixture2", 6),
    ] {
        let want = format!(
            "{name}: cwd={} pkg={pkg} threads={threads} args=-Zunstable-options \
             --report-time",
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
        report
            .findings()
            .iter()
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>(),
        ["-p fixture --test b -- t::broke"],
        "{block}"
    );
    assert!(t.could_not_run.is_empty(), "{t:?}");
}

/// THE POOL WIDENS BY THE LOAD THE STAGE READS. `--test-jobs 3` over three shared binaries, through
/// the real recorder, twice: the stage's load reader ([`Ctx::test_load`])
/// answering an idle machine, then a busy one.
///
/// Idle: the three run at once — each waits for the other two to have
/// started — and the stage's line says `3 at the most`. Busy: never past the
/// floor of two — `a` and `b` meet and stay a second, and no binary starts
/// beside two others — and the line says `2 at the most`. The pure law is
/// `testrun`'s own tests; this pins the stage's wiring of it, which none of
/// them reach: a stage that read an idle machine whatever its reader said, or
/// ran a fixed pool of `--test-jobs` (`3 at a time`), passed every other test.
#[test]
fn the_test_pool_widens_past_two_only_while_the_load_it_reads_is_under_the_cores() {
    let repo = FakeRepo::new();
    let marks = repo.scratch.join("marks");
    fs::create_dir_all(marks.join("running")).expect("mkdir");
    let bins = [
        ("fixture", "--test a", "a"),
        ("fixture", "--test b", "b"),
        ("fixture", "--test c", "c"),
    ];
    with_recording_cargo(&repo, &bins, "");
    let deps = repo.root.join("target/debug/deps");
    let m = marks.display();
    // A run's marks carry its mode (`idle`, `busy`), which the test writes
    // to `mode` before the run, so the second run reads none of the first's.
    let wait_for = |me: &str, other: &str| {
        format!(
            "i=0; until [ -e \"{m}/$mode-started-{other}\" ]; do i=$((i+1)); [ $i -gt 600 ] && \
             {{ echo '{me}: {other} never ran beside me'; exit 1; }}; sleep 0.05; done\n"
        )
    };
    let body = |me: &str, others: [&str; 2], busy: &str| {
        let idle: String = others.iter().map(|o| wait_for(me, o)).collect();
        format!(
            "mode=$(cat '{m}/mode')\n\
             n=$(ls '{m}/running' | wc -l | tr -d ' ')\n\
             if [ \"$mode\" = busy ] && [ \"$n\" -gt 1 ]; then echo \"{me}: $n binaries beside \
             me\"; exit 1; fi\n\
             touch '{m}/running/{me}'; touch \"{m}/$mode-started-{me}\"\n\
             if [ \"$mode\" = idle ]; then\n{idle}else\n{busy}fi\n\
             rm -f '{m}/running/{me}'\n\
             printf '\\nrunning 1 test\\ntest {me}::fine ... ok\\n\\ntest result: ok. 1 passed; \
             0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\\n\\n'\n"
        )
    };
    // Each binary bakes in the marks' path: data behind the shared stand-in.
    for (me, others, busy) in [
        ("a", ["b", "c"], format!("{}sleep 1\n", wait_for("a", "b"))),
        ("b", ["a", "c"], format!("{}sleep 1\n", wait_for("b", "a"))),
        ("c", ["a", "b"], "sleep 1\n".to_string()),
    ] {
        common::standin_body(&deps.join(format!("{me}-1")), &body(me, others, &busy));
    }

    let idle: fn() -> Option<f64> = || Some(0.0);
    let busy: fn() -> Option<f64> = || Some(1.0e9);
    for (mode, reader, peak) in [("idle", idle, 3), ("busy", busy, 2)] {
        fs::write(marks.join("mode"), mode).expect("the mode");
        let mut ctx = repo.ctx(&land(), Scope::workspace());
        ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
        ctx.test_jobs = 3;
        ctx.test_threads = Some(4);
        ctx.test_load = Some(reader);
        // A host with room: the memory cap (`testrun::memory_jobs`) is not this test's.
        ctx.host_memory = Some(128 << 30);
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::Test)
            .expect("the test stage");
        let block = stages::run_stage(&ctx, &spec).render();
        assert_eq!(
            decisions(&block),
            [
                ("ok", "targo test --workspace --no-run (trustdoc)"),
                ("ok", "targo test --workspace --tests (trustdoc)"),
            ],
            "{mode}:\n{block}"
        );
        let line = format!(
            "up to 3 at once, past 2 only while fewer than {} cores read busy: {peak} at the \
             most",
            (ctx.cores() * 3 / 4).max(1)
        );
        assert!(block.contains(&line), "{mode}: no {line:?} in:\n{block}");
        assert!(!block.contains("beside me"), "{mode}:\n{block}");
    }
}

/// NEVER LESS THAN CARGO: a recording that does not add up runs
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
    // ONE REPO FOR THE THREE CASES. What differs between them is only `extra`,
    // which bends the recording pass and is the stand-in cargo's own body
    // ([`with_recording_cargo`] replaces it): the two binaries are the same text
    // in every case, each run reads a record directory of its own and removes
    // it, and a case leaves nothing the next one reads. A repo each would buy
    // nothing the cases could tell apart.
    let repo = FakeRepo::new();
    let deps = repo.root.join("target/debug/deps");
    for (name, code, verdict) in [("b", 101, "FAILED"), ("c", 0, "ok")] {
        let failed = if code == 0 {
            String::new()
        } else {
            "\\nfailures:\\n\\n---- t::x stdout ----\\n\\nthread '\"'\"'t::x'\"'\"' panicked at \
             src/x.rs:1:1:\\nno\\n\\nfailures:\\n    t::x\\n"
                .to_string()
        };
        let (passed, nfailed) = if code == 0 { (1, 0) } else { (0, 1) };
        // Data behind the shared stand-in, like the stand-in cargo's body.
        common::standin_body(
            &deps.join(format!("{name}-1")),
            &format!(
                "echo RAN-{name}\nprintf '\\nrunning 1 test\\ntest t::x ... \
                 {verdict}\\n{failed}\\ntest result: {}. {passed} passed; {nfailed} failed; 0 \
                 ignored; 0 measured; 0 filtered out; finished in 0.01s\\n\\n'\nexit {code}\n",
                if code == 0 { "ok" } else { "FAILED" }
            ),
        );
    }
    for (extra, why) in cases {
        with_recording_cargo(&repo, &bins, extra);
        let mut ctx = repo.ctx(&land(), Scope::workspace());
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
            report
                .findings()
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>(),
            ["-p fixture --test b -- t::x"],
            "{extra}:\n{block}"
        );
    }
}

/// A RUNNER THAT RUNS NO TEST IS NO PASS. A runner for the host named in a
/// cargo config or the environment (`CARGO_TARGET_<TRIPLE>_RUNNER`) outranks
/// the run's `cfg(all())` recording runner, so the recording pass is judged as
/// the serial run it was — or, when it printed no result, the serial child
/// runs, through the same runner. One that runs nothing exits 0 for every
/// binary, which would read `ok` with no test run. Here the stand-in cargo's runner is
/// outranked (`cfg=''`) and each binary "runs" as such a runner would: it
/// prints nothing and exits 0. The row is COULD NOT RUN, and says why.
#[test]
fn a_runner_that_ran_no_test_is_could_not_run_never_ok() {
    let repo = FakeRepo::new();
    let bins = [("fixture", "--test b", "b"), ("fixture", "--test c", "c")];
    with_recording_cargo(&repo, &bins, "cfg=''");
    let deps = repo.root.join("target/debug/deps");
    for name in ["b", "c"] {
        common::standin_body(&deps.join(format!("{name}-1")), "exit 0");
    }
    let mut ctx = repo.ctx(&land(), Scope::workspace());
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
        t.could_not_run[0].starts_with("targo test --workspace --tests (trustdoc) — ")
            && t.could_not_run[0].contains("CARGO_TARGET_<TRIPLE>_RUNNER"),
        "{t:?}"
    );
}

/// A HUNG BINARY IS NAMED BY ITS OWN CEILING: each binary the run
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
            "echo RAN-{n}\nprintf '\\nrunning 1 test\\ntest {n}::fine ... ok\\n\\ntest \
             result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in \
             0.01s\\n\\n'"
        )
    };
    // DATA BEHIND THE ONE SHARED STAND-IN, as the pool's own test writes its
    // binaries ([`common::standin_body`]): three files of their own would each
    // be a fresh inode whose first exec waits on macOS's assessment, one file
    // at a time machine-wide. The lines run in the same shell, and `exec sleep
    // 600` still turns the hung binary itself into the sleep the ceiling ends.
    for (name, body) in [
        ("fine", ok("fine")),
        (
            "hang",
            "printf '\\nrunning 2 tests\\ntest h::quick ... ok\\ntest h::wedged has been \
             running for over 60 seconds\\n'\nexec sleep 600"
                .to_string(),
        ),
        ("after", ok("after")),
    ] {
        common::standin_body(&deps.join(format!("{name}-1")), &body);
    }
    // THE FIRST EXEC OF A FRESH FILE IS PAID HERE, not inside the ceiling.
    // macOS makes the first exec of every newly written executable wait on a
    // check it serves one file at a time, machine-wide, for seconds under
    // load. The ceiling below is ONE for the whole run, so `after`, which
    // starts in `fine`'s slot, gets what `fine` left of it. Both finishing
    // binaries are run once first; the hung one is not (it would hang), and
    // its exec only delays its own kill.
    for name in ["fine", "after"] {
        let out = std::process::Command::new(deps.join(format!("{name}-1")))
            .output()
            .expect("run the fixture once");
        assert!(out.status.success(), "{name}: {out:?}");
    }
    let mut ctx = repo.ctx(&land(), Scope::workspace());
    ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
    ctx.test_jobs = 2;
    // A hang detector for every child that finishes (a shell script each,
    // already exec'd once above), never a latency budget: only the wedged
    // binary ever reaches a minute, so a passing run costs that minute once.
    ctx.child_ceiling = Some(std::time::Duration::from_secs(60));
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
