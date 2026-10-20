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
use aterm_verify::ladder::tally;
use aterm_verify::plan::StageId;
use aterm_verify::{Ctx, Scope, Show, exit, mktemp_dir, plan, stages};

/// Every check a fixture can stand in for in one run: all but the ones that
/// read the real tree in-process (`fmt`, `census`, `forge`, `cells`, each
/// unit-tested where it lives), the node-driven `wasm-smoke` and `wasm-glue`,
/// the Chromium-driven `browser`, the bundle's `measure`, the release's feature
/// builds (`checkers`, `gpu-oracle`, each a stage unit-tested in `stages`), the
/// machine-package `codex-alab` and the VM-driving `linux`.
fn land() -> Vec<Check> {
    CHECKS
        .iter()
        .map(|s| s.check)
        .filter(|c| {
            !matches!(
                c,
                Check::Fmt
                    | Check::Census
                    | Check::Forge
                    | Check::Cells
                    | Check::WasmSmoke
                    | Check::WasmGlue
                    | Check::Browser
                    | Check::Measure
                    | Check::Checkers
                    | Check::GpuOracle
                    | Check::CodexAlab
                    | Check::Linux
            )
        })
        .collect()
}

/// A repo-shaped directory: stand-ins for the binaries the CLI drives, all
/// passing.
struct FakeRepo {
    root: PathBuf,
    stage2: PathBuf,
    scratch: PathBuf,
    /// The fixture's own HOME.
    home: PathBuf,
}

/// Where a fixture's driven binaries are built: the main target, `<root>/target`.
const DRIVERS: &str = "target";

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
        let me = Self {
            root,
            stage2,
            scratch,
            home,
        };
        // Every binary the CLI builds and then DRIVES, present and passing, so
        // an unrelated test never reads a missing binary as a finding.
        for rel in driven_stubs() {
            me.driver_stub(&rel, 0);
        }
        me
    }

    /// A stand-in for a binary a stage builds in the main target and then
    /// DRIVES — `rel` under its `debug/` — that exits `code`.
    fn driver_stub(&self, rel: &str, code: i32) -> &Self {
        self.driver_script(rel, &format!("echo '{rel}: stub'; exit {code}"))
    }

    /// A stand-in driven binary with its own `body`, as data behind the one
    /// shared stand-in text ([`Self::script_body`]).
    fn driver_script(&self, rel: &str, body: &str) -> &Self {
        self.script_body(&format!("{DRIVERS}/debug/{rel}"), body);
        self
    }

    /// A stand-in at `rel` whose body is not shared: data beside the one
    /// constant stand-in ([`common::standin_body`]).
    fn script_body(&self, rel: &str, body: &str) {
        common::standin_body(&self.root.join(rel), body);
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
        common::fixture_ctx(
            &self.root,
            &self.stage2,
            &self.scratch,
            &self.home,
            checks,
            scope,
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

/// Every driven binary's path under the main target's `debug/`: the redraw
/// harness and the objc drivers' examples, named by the stages themselves.
fn driven_stubs() -> Vec<String> {
    std::iter::once(stages::REDRAW_CONFORMANCE_BIN.to_string())
        .chain(stages::OBJC_DRIVES.iter().map(|d| objc_stub(d.example)))
        .collect()
}

/// An objc driver's built example, under the main target's `debug/`.
fn objc_stub(example: &str) -> String {
    format!("examples/{example}")
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

/// Every ladder decision's tag, in order.
fn tags(ladder: &str) -> Vec<&str> {
    decisions(ladder).into_iter().map(|(t, _)| t).collect()
}

fn labels_with(ladder: &str, tag: &str) -> Vec<String> {
    decisions(ladder)
        .into_iter()
        .filter(|(t, _)| *t == tag)
        .map(|(_, l)| l.to_string())
        .collect()
}

/// `s` as one single-quoted shell word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `fmt` hands every file git tracks (and every untracked one cargo builds)
/// to the stage2's `trustfmt`, makes each file it would change a finding, and
/// under `--fix` rewrites exactly those files.
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
    let checks = [Check::Census, Check::Forge, Check::Cells];
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
    assert_eq!(labels_with(&dev, "skip").len(), 1, "{dev}");
    let ctx = repo
        .ctx(&[Check::Lint, Check::Export], Scope::workspace())
        .with_release(true);
    let (release, code) = run_ctx(&ctx);
    assert_eq!(code, exit::COULD_NOT_RUN, "{release}");
}

#[test]
fn a_run_with_no_driver_fails_closed() {
    // The bare machine: the test compile COULD NOT RUN, and the checks that
    // need the driver skip by name.
    let repo = FakeRepo::new();
    let (ladder, code) = repo.run(&land(), Scope::workspace());
    assert_eq!(code, exit::COULD_NOT_RUN, "{ladder}");
    assert!(!labels_with(&ladder, "NOT RUN").is_empty(), "{ladder}");
    assert!(
        labels_with(&ladder, "FAIL").is_empty(),
        "nothing is a finding: {ladder}"
    );
}

#[test]
fn a_failing_driver_fails_every_stage_that_drives_it_and_nothing_else() {
    let repo = FakeRepo::new();
    repo.with_stage2_body("echo 'error: unknown unstable option: `trust-verify`' >&2; exit 1");
    let (ladder, code) = repo.run(&land(), Scope::workspace());

    assert_eq!(code, exit::FAILED, "{ladder}");
    // Every stage that drives targo fails (test compile, doctests, libc,
    // spec, export…); the in-process checks are not in `land()`.
    assert!(labels_with(&ladder, "FAIL").len() >= 4, "{ladder}");
    // The test run never starts after a failed compile, and is no skip: a
    // COULD NOT RUN row.
    assert!(
        labels_with(&ladder, "NOT RUN")
            .iter()
            .any(|l| l.starts_with("targo test --workspace --tests")),
        "{ladder}"
    );
}

#[test]
fn a_scoped_run_narrows_the_driver_where_the_check_reads_the_selection() {
    let repo = FakeRepo::new();
    repo.with_stage2_body("echo \"argv: $*\"\nexit 0");
    let (ladder, code) = repo.run(&land(), Scope::crate_only("aterm-grid"));

    assert_eq!(code, exit::PASS, "{ladder}");
    assert!(ladder.contains("argv: --unverified test -p aterm-grid"));
    assert!(ladder.contains("argv: --unverified test --doc -p aterm-grid"));
    assert!(
        !ladder.contains("--workspace"),
        "nothing whole-tree was driven"
    );
}

/// Every driven stage LOOKS AT its driver's exit code — the regressions these
/// drivers exist for (a dropped `EventLoopProxy`, a retyped objc argument)
/// leave `cargo build` at 0 — and nothing that decided nothing is green: the
/// redraw harness's `0` passes, `1` is a finding, `2` (no event loop) could not
/// run; and on macOS the event drive's untrapped signal death is the finding it
/// exists to make (v0.72.0 died by `SIGABRT` on the first mouse move). The
/// stand-in dies by `SIGKILL`: the ladder reads every signal death alike, and
/// a `SIGABRT` makes macOS write a crash report on every run. The exit-code
/// tables themselves are `stages`' unit tests.
#[test]
fn every_driven_stage_reads_its_exit_code_and_nothing_undecided_is_green() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
    let ctx = repo.ctx_with_windows(&land(), Scope::workspace());
    let spec_of = |id: StageId| {
        plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("{id:?} is planned"))
    };
    let redraw = spec_of(StageId::RedrawConformance);
    for (code, findings, undecided) in [(0, 0, 0), (1, 1, 0), (2, 0, 1)] {
        repo.driver_stub(stages::REDRAW_CONFORMANCE_BIN, code);
        let t = tally(&[stages::run_stage(&ctx, &redraw)]);
        assert_eq!(
            (t.gate_failures.len(), t.could_not_run.len(), t.skipped()),
            (findings, undecided, 0),
            "redraw exit {code}: {t:?}"
        );
    }
    repo.driver_stub(stages::REDRAW_CONFORMANCE_BIN, 0);
    if cfg!(target_os = "macos") {
        let stub = objc_stub("objc_event_drive");
        repo.driver_script(&stub, "kill -KILL $$");
        let t = tally(&[stages::run_stage(&ctx, &spec_of(StageId::ObjcDrives))]);
        assert_eq!(t.gate_failures.len(), 1, "{t:?}");
        assert!(t.could_not_run.is_empty(), "a crash is a finding: {t:?}");
        repo.driver_stub(&stub, 0);
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
    let passed = labels_with(&ladder, "ok");
    for drive in &stages::OBJC_DRIVES[2..] {
        assert!(
            passed.iter().any(|l| l.starts_with(drive.example)),
            "{} ran after the failures: {ladder}",
            drive.example
        );
    }
}

/// A compile red, as a stub driver prints one: the build of whatever it names
/// fails, every other invocation passes.
const COMPILE_RED: &str = "echo 'error[E0425]: cannot find value `gone` in this scope' >&2; \
     echo '  --> crates/aterm-gui/src/bin/rot.rs:3:13' >&2; exit 101";

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

    assert_eq!(tags(&block), ["ok", "FAIL"], "{block}");
    assert!(block.contains("   Compiling fixture v0.1.0"), "{block}");
    assert!(
        !block.contains("{\"reason\""),
        "the JSON leaves the ladder:\n{block}"
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
            "{name}: cwd={} pkg={pkg} threads={threads} args=\n",
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
    assert_eq!(t.gate_failures.len(), 1, "{t:?}");
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

/// NEVER LESS THAN CARGO: a recording that does not add up runs
/// the serial `--tests` child instead, and says why — cargo announcing a
/// binary the recorder never saw, or the recorder failing — and a pass in
/// which the recorder never ran (a runner the caller's config names outranks
/// ours) is judged as the serial run it was. In every case the tests RAN and
/// the one failure is decided exactly once.
#[test]
fn a_recording_that_does_not_add_up_falls_back_to_cargos_serial_run() {
    let bins = [("fixture", "--test b", "b"), ("fixture", "--test c", "c")];
    // cargo announcing a binary the recorder never saw; the recorder failing;
    // a runner from the caller's config outranking the recorder.
    let cases = [
        "echo '     Running tests/ghost.rs (target/debug/deps/ghost-1)' >&2",
        "if [ -n \"$cfg\" ]; then cfg=$(printf '%s' \"$cfg\" | sed 's#\",\"/#\",\"/nonexistent/#'); fi",
        "cfg=''",
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
    for extra in cases {
        with_recording_cargo(&repo, &bins, extra);
        let mut ctx = repo.ctx(&land(), Scope::workspace());
        ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::Test)
            .expect("the test stage");
        let report = stages::run_stage(&ctx, &spec);
        let block = report.render();
        assert!(
            block.contains("RAN-b") && block.contains("RAN-c"),
            "{block}"
        );
        let t = tally(std::slice::from_ref(&report));
        assert_eq!(t.gate_failures.len(), 1, "{extra}:\n{block}");
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
    assert_eq!(
        tags(&block),
        ["ok", "NOT RUN"],
        "a run of no test passed:\n{block}"
    );
    let t = tally(std::slice::from_ref(&report));
    assert!(t.gate_failures.is_empty(), "{t:?}");
}
