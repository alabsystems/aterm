// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end CLI contract tests over a synthetic repository using `#!/bin/sh` stand-ins.
#![cfg(unix)]

use crate::common;

use std::fs;
use std::path::PathBuf;

use aterm_verify::checks::{CHECKS, Check};
use aterm_verify::ladder::tally;
use aterm_verify::plan::StageId;
use aterm_verify::{Ctx, Scope, Show, exit, mktemp_dir, plan, stages};

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

struct FakeRepo {
    root: PathBuf,
    stage2: PathBuf,
    scratch: PathBuf,
    /// The fixture's own HOME.
    home: PathBuf,
}

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
        for rel in driven_stubs() {
            me.driver_stub(&rel, 0);
        }
        me
    }

    fn driver_stub(&self, rel: &str, code: i32) -> &Self {
        self.driver_script(rel, &format!("echo '{rel}: stub'; exit {code}"))
    }

    fn driver_script(&self, rel: &str, body: &str) -> &Self {
        self.script_body(&format!("{DRIVERS}/debug/{rel}"), body);
        self
    }

    fn script_body(&self, rel: &str, body: &str) {
        common::standin_body(&self.root.join(rel), body);
    }

    fn with_stage2(&self, targo_body: &str) -> &Self {
        for (name, body) in [("targo", targo_body), ("trustdoc", "exit 0")] {
            common::standin(&self.stage2.join(name), body);
        }
        self
    }

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

    fn ctx_with_windows(&self, checks: &[Check], scope: Scope) -> Ctx {
        self.ctx(checks, scope).with_no_windows(false)
    }

    fn run(&self, checks: &[Check], scope: Scope) -> (String, i32) {
        run_ctx(&self.ctx(checks, scope))
    }
}

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

fn driven_stubs() -> Vec<String> {
    std::iter::once(stages::REDRAW_CONFORMANCE_BIN.to_string())
        .chain(stages::OBJC_DRIVES.iter().map(|d| objc_stub(d.example)))
        .collect()
}

fn objc_stub(example: &str) -> String {
    format!("examples/{example}")
}

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

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

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
        let git = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
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
#[test]
fn a_skip_fails_nothing_except_under_release() {
    let repo = FakeRepo::new();
    repo.with_stage2("exit 0");
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
    assert!(labels_with(&ladder, "FAIL").len() >= 4, "{ladder}");
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
    let lead = format!(
        "argv: --config build.build-dir='{}/target/build.noindex' ",
        repo.root.display()
    );
    assert!(
        ladder.contains(&format!("{lead}--unverified test -p aterm-grid")),
        "{ladder}"
    );
    assert!(
        ladder.contains(&format!("{lead}--unverified test --doc -p aterm-grid")),
        "{ladder}"
    );
    assert!(
        !ladder.contains("--workspace"),
        "nothing whole-tree was driven"
    );
}

#[test]
fn the_doctests_run_rustdoc_only_where_a_doctest_lives_over_the_test_compiles_selection() {
    let repo = FakeRepo::new();
    for (name, lib) in [
        (
            "documented",
            "/// ```\n/// assert!(true);\n/// ```\npub fn f() {}\n",
        ),
        ("plain", "//! ```text\n//! not rust\n//! ```\n"),
    ] {
        let dir = repo.root.join("crates").join(name);
        fs::create_dir_all(dir.join("src")).expect("mkdir");
        fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        )
        .expect("manifest");
        fs::write(dir.join("src/lib.rs"), lib).expect("lib");
    }
    repo.with_stage2_body(
        "echo \"argv: $*\"\n\
         st=0\n\
         for p in documented plain; do\n\
         echo \"   Doc-tests $p\"\n\
         CARGO_PKG_NAME=$p \"$RUSTDOC\" --crate-name $p --test crates/$p/src/lib.rs || st=101\n\
         done\n\
         exit $st",
    );
    let trustdoc = repo.stage2.join("trustdoc");
    let ran = |code: i32| {
        format!(
            "echo \"trustdoc ran $CARGO_PKG_NAME with $*: RUSTDOC=$RUSTDOC \
             driver=${{ATERM_VERIFY_DOC_DRIVER-unset}} skip=${{ATERM_VERIFY_DOC_SKIP-unset}}\"\n\
             printf '\\nrunning 1 test\\ntest src/lib.rs - f (line 1) ... ok\\n\\ntest result: ok. 1 \
             passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\\n\\n'\n\
             exit {code}"
        )
    };
    let doc_run = || {
        let mut ctx = repo.ctx(&[Check::Doc], Scope::workspace());
        ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
        run_ctx(&ctx)
    };

    common::standin_body(&trustdoc, &ran(0));
    let (ladder, code) = doc_run();
    assert_eq!(code, exit::PASS, "{ladder}");
    assert!(
        ladder.contains("--unverified test --doc --workspace --no-fail-fast"),
        "{ladder}"
    );
    let want = format!(
        "trustdoc ran documented with --crate-name documented --test crates/documented/src/lib.rs: \
         RUSTDOC={} driver=unset skip=unset",
        fs::canonicalize(&repo.stage2)
            .expect("canonical")
            .join("trustdoc")
            .display()
    );
    assert!(ladder.contains(&want), "missing {want:?} in:\n{ladder}");
    assert!(!ladder.contains("trustdoc ran plain"), "{ladder}");
    assert!(
        ladder
            .lines()
            .any(|l| l.starts_with("aterm-verify: ") && l.contains("plain's docs")),
        "the skip says why:\n{ladder}"
    );

    common::standin_body(&trustdoc, &ran(101));
    let (ladder, code) = doc_run();
    assert_eq!(code, exit::FAILED, "{ladder}");
    assert!(ladder.contains("trustdoc ran documented"), "{ladder}");
}

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

const COMPILE_RED: &str = "echo 'error[E0425]: cannot find value `gone` in this scope' >&2; \
     echo '  --> crates/aterm-gui/src/bin/rot.rs:3:13' >&2; exit 101";

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
  # cargo merges every --config; the runner is the one naming a runner (the
  # main lane's build-dir --config rides beside it).
  [ "$prev" = "--config" ] && case "$a" in *runner=*) cfg="$a" ;; esac
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

#[test]
fn admitted_test_workers_keep_their_share_on_replay_and_serial_fallback() {
    use std::num::NonZeroU32;
    for extra in ["", "cfg=''"] {
        let repo = FakeRepo::new();
        let bins = [
            ("fixture", "--test a", "a"),
            ("fixture", "--test b", "b"),
            ("fixture", "--test c", "c"),
            ("fixture", "--test d", "d"),
        ];
        with_recording_cargo(&repo, &bins, extra);
        for (_, _, name) in bins {
            common::standin_body(
                &repo.root.join(format!("target/debug/deps/{name}-1")),
                &format!(
                    "[ \"$RUST_TEST_THREADS\" = 2 ] && [ \"$CARGO_BUILD_JOBS\" = 6 ] || exit 99\necho RAN-{name}-threads-$RUST_TEST_THREADS-build-$CARGO_BUILD_JOBS\nprintf '\\nrunning 1 test\\ntest {name}::fine ... ok\\n\\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\\n\\n'"
                ),
            );
        }
        let mut ctx = repo.ctx(&[Check::Test], Scope::workspace());
        ctx.env.build_slot_held = Some("1".into());
        ctx.env.cargo_build_jobs = Some("6".into());
        ctx.child_env_add
            .push(("CARGO_BUILD_JOBS".into(), "6".into()));
        ctx = ctx
            .with_test_jobs(NonZeroU32::new(u32::MAX))
            .with_test_threads(NonZeroU32::new(2));
        ctx.test_recorder = Some(PathBuf::from(env!("CARGO_BIN_EXE_aterm-verify")));
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::Test)
            .expect("test stage");
        let report = stages::run_stage(&ctx, &spec);
        let block = report.render();
        assert!(!tally(std::slice::from_ref(&report)).failed(), "{block}");
        for name in ["a", "b", "c", "d"] {
            assert!(
                block.contains(&format!("RAN-{name}-threads-2-build-6")),
                "{block}"
            );
        }
        if extra.is_empty() {
            assert!(
                block.contains("RUST_TEST_THREADS=2 each, 3 at a time"),
                "{block}"
            );
        } else {
            assert!(block.contains("run by cargo, one at a time"), "{block}");
        }
    }
}

#[test]
fn invalid_managed_admission_refuses_before_any_stage_or_cli_check() {
    let repo = FakeRepo::new();
    let mut ctx = repo.ctx(&[Check::Test], Scope::workspace());
    ctx.env.build_slot_held = Some("1".into());
    ctx.env.cargo_build_jobs = None;
    let (block, code) = run_ctx(&ctx);
    assert_eq!(code, exit::USAGE, "{block}");
    assert!(block.contains("no check was run"), "{block}");
    let spec = plan::plan(&ctx)
        .into_iter()
        .find(|s| s.id == StageId::TestCompile)
        .expect("compile stage");
    assert!(
        stages::run_stage(&ctx, &spec)
            .render()
            .contains("no stage was run")
    );
    for value in [None, Some("0"), Some("no"), Some("4294967296")] {
        let mut cmd = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_aterm-verify"));
        cmd.arg("test")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .env("BUILD_SLOT_HELD", "1");
        if let Some(value) = value {
            cmd.env("CARGO_BUILD_JOBS", value);
        } else {
            cmd.env_remove("CARGO_BUILD_JOBS");
        }
        let output = aterm_uds::deadline::output_until(
            &mut cmd,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("CLI refusal");
        assert_eq!(output.status.code(), Some(exit::USAGE), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("no check was run"),
            "{output:?}"
        );
    }
}

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

#[test]
fn a_recording_that_does_not_add_up_falls_back_to_cargos_serial_run() {
    let bins = [("fixture", "--test b", "b"), ("fixture", "--test c", "c")];
    let cases = [
        "echo '     Running tests/ghost.rs (target/debug/deps/ghost-1)' >&2",
        "if [ -n \"$cfg\" ]; then cfg=$(printf '%s' \"$cfg\" | sed 's#\",\"/#\",\"/nonexistent/#'); fi",
        "cfg=''",
    ];
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
