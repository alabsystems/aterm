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

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
    /// The control socket the answering smoke's `aterm-gui` links to, held
    /// listening for the fixture's life: the smoke waits for a socket that
    /// accepts a connect, not for a file.
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
            ctl_sock,
            _listener,
        };
        me.script("tools/verify.sh", "exit 0");
        me.script("tools/grep_guard.sh", "echo 'GUARD: PASS'; exit 0");
        me.script("tools/license_check.sh", "echo 'LICENSE: PASS'; exit 0");
        for name in aterm_verify::stages::DELIVERY_SUITES {
            me.script(&format!("tools/{name}"), "exit 0");
        }
        // The live lanes, from their roster for the same reason as the two below.
        for name in aterm_verify::stages::LIVE_ATERM_SUITES {
            me.script(&format!("tools/{name}"), "exit 0");
        }
        // DERIVED FROM THE ROSTER, never re-typed. This list was a hand-written copy of
        // `ATPKG_SUITES` and the two drifted the moment the roster grew: adding a suite
        // made every fixture here report `missing or not executable` for it, which reads
        // as a broken change rather than as an un-updated fixture (2026-09-17).
        for name in aterm_verify::stages::ATPKG_SUITES {
            me.script(&format!("tools/{name}"), "exit 0");
        }
        me.script("tools/test-trust-contract-probe.sh", "exit 0");
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

    /// A stage2 whose driver also produces the two binaries the smokes drive:
    /// an `aterm-gui` that links its control socket to the fixture's listening
    /// one and stays up, and an `aterm-ctl` that answers the protocol. This is what lets the control-socket
    /// smoke — launch, poll, round-trip, burst, teardown — run for real in a test.
    fn with_answering_smoke(&self) -> &Self {
        self.with_stage2(
            &r#"echo "argv: $*"
case "$*" in
  *aterm-gui*aterm-ctl*)
    mkdir -p "$CARGO_TARGET_DIR/debug"
    cat >"$CARGO_TARGET_DIR/debug/aterm-gui" <<'GUI'
#!/bin/sh
# These are real spawn-time fixture preconditions, not runner-wide overrides:
# an absent machine table still authorizes the product's per-user defaults.
test -z "${ATERM_NO_REROUTE+set}${ATERM_NO_AUTO_UPDATE+set}" || exit 81
test "$1" = --control-sock && test "$2" = "$XDG_RUNTIME_DIR/aterm/aterm.sock" || exit 88
# A headless launch carries its lifeline: `--lifeline-fd 0` on a FIFO stdin.
if test "$3" = --headless; then
  test "$4" = --lifeline-fd && test "$5" = 0 && test -p /dev/stdin || exit 92
fi
test "$HOME" = "${XDG_CONFIG_HOME%/cfg}/home" || exit 89
test -d "$HOME" || exit 90
config="$XDG_CONFIG_HOME/aterm/aterm.toml"
# One builtin pass, not seven `grep` spawns: every spawn before the socket is up
# counts against the smoke's 10 s budget, and at load ~150 with 30 ladders in
# this binary, nine of them missed it (2026-09-27). Same lines, same codes.
seen=
while IFS= read -r line || test -n "$line"; do
  case "$line" in
    'agents_auto_prime = false') seen="$seen a" ;;
    '[update]') seen="$seen u" ;;
    '[packages]') seen="$seen p" ;;
    'enabled = false') seen="$seen e" ;;
    '[machine]') seen="$seen m" ;;
    'spotlight_noindex = false') seen="$seen s" ;;
    'universal_control = "leave"') seen="$seen c" ;;
  esac
done <"$config" || exit 87
for want in a:87 u:91 p:82 e:83 m:84 s:85 c:86; do
  case "$seen " in *" ${want%%:*} "*) ;; *) exit "${want#*:}" ;; esac
done
mkdir -p "$XDG_RUNTIME_DIR/aterm"
ln -s '@LISTENER@' "$XDG_RUNTIME_DIR/aterm/aterm.sock"
exec sleep 300
GUI
    cat >"$CARGO_TARGET_DIR/debug/aterm-ctl" <<'CTL'
#!/bin/sh
test "$1" = --sock && test "$2" = "$XDG_RUNTIME_DIR/aterm/aterm.sock" || exit 88
shift 2
case "$1" in
  cursor)  echo "OK row=0 col=0" ;;
  metrics) echo "OK frames=41 max_input_present_ms=8.100 redraw_retry_gated=0 present_drops=0 sync_rel_timeout=0 perf_reduced=0 wake_heals=0 " ;;
  send|key) echo "OK accepted" ;;
  *) echo "ERR unknown verb"; exit 1 ;;
esac
CTL
    chmod 755 "$CARGO_TARGET_DIR/debug/aterm-gui" "$CARGO_TARGET_DIR/debug/aterm-ctl"
    ;;
esac
exit 0"#
            .replace("@LISTENER@", &self.ctl_sock.display().to_string()),
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

    fn ctx(&self, mode: Mode, scope: Scope) -> Ctx {
        self.ctx_with(mode, scope, |_| {})
    }

    /// [`Self::ctx`] with one last say over the environment the stages read —
    /// for the cases that MEASURE a variable's effect instead of being at its
    /// mercy (see the `cargo_build_jobs` pin below).
    fn ctx_with(&self, mode: Mode, scope: Scope, tweak: impl FnOnce(&mut EnvSnapshot)) -> Ctx {
        let mut env = EnvSnapshot::capture();
        env.trust_stage2_bin = Some(self.stage2.clone());
        // Point the Tier-2 prover locations inside the sandbox so these tests
        // decide the same thing on a machine that has trust-mc built and on one
        // that does not.
        env.trust_mc_sysroot = Some(self.root.join("no-trust-mc"));
        env.ay_bin_dir = Some(self.root.join("no-ay"));
        // …and the caller's job count. `lane_jobs` makes an exported
        // `CARGO_BUILD_JOBS` a CEILING on the side lanes, so the value a stage
        // hands its child is a function of
        // the ambient environment — and two fixtures here pin that value to a
        // lane's own cap (`test "$CARGO_BUILD_JOBS" = 8 || exit 71`). Measured
        // 2026-09-16: the merge gate itself exports `CARGO_BUILD_JOBS=4` on a
        // 4-core Mac, so inside that run the driver lane capped 8 to 4 and both
        // `sealed_lane_prepares_its_gui_before_testing_and_preserves_both_failures`
        // and `atpkg_tooling_builds_the_atpkg_its_pack_suite_drives_and_never_takes_a_stale_one`
        // failed at exit 71 — their stubs never reaching the build arm, so the
        // trace was missing rows and the driven binary missing entirely. The
        // stages were right and the fixture was reading the shell. `None` is
        // the pin because these tests assert the CAPS; the ceiling itself is
        // measured by `a_callers_job_count_caps_the_side_lane_child_it_reaches`,
        // which sets the variable through [`Self::ctx_with`] rather than
        // inheriting whatever ran the suite.
        env.cargo_build_jobs = None;
        tweak(&mut env);
        // THE DISK FLOOR IS ZERO HERE. A fixture with a fake toolchain builds
        // nothing, and the real floor made these tests' ladders refuse at the
        // disk preflight whenever the HOST volume held less than it — measured
        // 2026-09-23 inside a merge-contract run at 17.6 GiB free: 11 failures
        // here, 12 in environment_contract.rs, every one a `disk preflight`
        // COULD NOT RUN. The estimate that replaced that floor would refuse them
        // too, since a fixture's empty lanes are budgeted cold. The preflight
        // itself is measured by its own laws, which set the requirement they need.
        Ctx::new(self.root.clone(), mode, scope, env, self.scratch.clone())
            .with_disk_floor(0)
            // The GUI smoke measures a real window; a synthetic repo has none, so it
            // takes its honest skip instead of trying to open one.
            .with_gui_smoke_skipped(true)
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
        .chain(
            stages::OBJC_DRIVER_EXAMPLES
                .iter()
                .map(|(_, example)| format!("examples/{example}")),
        )
        .collect()
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
        "targo test --workspace --tests (trustdoc) -- measuring:: launchd_copy_tests::",
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
    // calling it a skip.
    assert!(
        ladder.contains(
            "  not run: targo test --workspace --tests (trustdoc) — the compile above failed"
        ),
        "{ladder}"
    );
    assert!(
        !decisions(&ladder)
            .iter()
            .any(|(_, l)| *l == "targo test --workspace --tests (trustdoc)"),
        "{ladder}"
    );
    // Likewise the sealed rung: its aterm-gui build fails, so its suite never
    // starts against whatever binary an earlier build left behind.
    assert!(
        labels_with(&ladder, "FAIL")
            .iter()
            .any(|l| l
                == "targo build -p aterm-gui -p aterm-ctl (the aterm-gui the sealed rung drives)"),
        "{ladder}"
    );
    assert!(
        ladder.contains(
            "  not run: targo test -p aterm-link --features sealed --test two_nodes_sealed — the aterm-gui build above failed"
        ),
        "{ladder}"
    );
}

/// THE SEALED RUNG NEVER DRIVES AN aterm-gui IT DID NOT JUST SEE BUILT
/// (2026-09-14), end to end over the real scheduler and stage runner.
///
/// `two_nodes_sealed` finds `aterm-gui` in the target dir it was compiled into
/// and refuses one older than its sources. At 28508563a it was spawned at t0 in
/// `target-sealed/`, which no stage ever built `aterm-gui` into, while the
/// build stage was still linking the binary its harness fell through to — 5 of
/// 9 tests refused STALE on a warm gate. The recording driver here shows the
/// order cargo is actually asked for: the suite is spawned only in the driver
/// lane's dir, only after the driver builds' children and the rung's own
/// `aterm-gui` build in that same dir — and never at all when that build fails.
#[test]
fn the_sealed_rung_never_runs_before_a_fresh_aterm_gui_is_built_in_its_dir() {
    for gui_build_exit in [0, 1] {
        let repo = FakeRepo::new();
        let record = repo.scratch.join("argv.txt");
        repo.with_stage2(&format!(
            "printf '%s %s\\n' \"$CARGO_TARGET_DIR\" \"$*\" >> '{}'\n\
             case \"$*\" in *'-p aterm-gui -p aterm-ctl'*) exit {gui_build_exit} ;; esac\n\
             exit 0",
            record.display()
        ));
        let (ladder, _) = repo.run(Mode::Fast, Scope::workspace());
        let lines: Vec<String> = fs::read_to_string(&record)
            .expect("the driver was invoked")
            .lines()
            .map(str::to_string)
            .collect();
        let suite = "--features sealed --test two_nodes_sealed";
        let drivers = repo.root.join("target-drivers").display().to_string();
        if gui_build_exit != 0 {
            assert!(
                !lines.iter().any(|l| l.contains(suite)),
                "the suite ran after its aterm-gui build failed:\n{}",
                lines.join("\n")
            );
            assert!(
                ladder.contains("  not run: targo test -p aterm-link --features sealed"),
                "{ladder}"
            );
            continue;
        }
        let at = lines
            .iter()
            .position(|l| l.contains(suite))
            .expect("the suite was spawned");
        assert!(
            lines[at].starts_with(&format!("{drivers} ")),
            "the suite must be compiled into the driver lane's dir: {}",
            lines[at]
        );
        // The driver lane is serialised, so its lines are in the order it ran.
        let before: Vec<&String> = lines[..at]
            .iter()
            .filter(|l| l.starts_with(&format!("{drivers} ")))
            .collect();
        assert!(
            before
                .last()
                .is_some_and(|l| l.contains("build -q -p aterm-gui -p aterm-ctl")),
            "the rung's own aterm-gui build must be the lane's last command before the suite: {before:?}"
        );
        assert!(
            before
                .iter()
                .any(|l| l.contains("--bin aterm-redraw-conformance"))
                && before
                    .iter()
                    .filter(|l| l.contains("-p aterm-gui -p aterm-ctl"))
                    .count()
                    >= 2,
            "the driver builds must have run before the rung: {before:?}"
        );
        assert!(
            labels_with(&ladder, "ok")
                .iter()
                .any(|l| l == "targo test -p aterm-link --features sealed --test two_nodes_sealed"),
            "{ladder}"
        );
    }
}

#[test]
fn a_scoped_run_narrows_the_driver_and_is_refused_the_contract() {
    let repo = FakeRepo::new();
    // Everything green — including a control-socket smoke that really launches,
    // really answers and really tears down — so the only thing standing between
    // this run and the merge-contract sentence is that it was narrowed.
    repo.with_answering_smoke();
    let (ladder, code) = repo.run(Mode::Fast, Scope::crate_only("aterm-grid"));

    assert_eq!(code, exit::PASS, "narrow is not failure: {ladder}");
    assert!(ladder.contains("argv: --unverified test -p aterm-grid"));
    assert!(ladder.contains("argv: --unverified test --doc -p aterm-grid"));
    assert!(
        !ladder.contains("--workspace"),
        "nothing whole-tree was driven"
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
    assert!(ladder.contains("      - gui smoke (--skip-gui-smoke)"));
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
/// identical silence in a new place.
///
/// The event drive is the one reading that differs: v0.72.0 died by `SIGABRT`
/// on the first mouse move and the driver reproduces that shape, so its `3`
/// (the trapped abort) and an untrapped signal death are THE finding, never
/// could-not-run — the siblings' reading would file the crash under "decided
/// nothing".
///
/// The objc rows are macOS-only (their stages are planned there); the redraw
/// row runs on every unix.
#[test]
fn every_driven_stage_reads_its_exit_code_and_nothing_undecided_is_green() {
    /// A driver outcome beyond `1` that is THE finding.
    enum Finding {
        Exit(i32),
        /// A stand-in that dies by an untrapped signal. It dies by `SIGKILL`,
        /// not the v0.72.0 crash's `SIGABRT`: the ladder reads every signal
        /// death alike (`objc_event_outcome(None)`), and a `SIGABRT` made macOS
        /// write a crash report for the stub's shell on every run (grep_guard
        /// B16).
        Signal,
    }
    struct Row {
        id: StageId,
        /// The label the stage's verdict line carries.
        label: &'static str,
        /// The driven binary, under `target-drivers/debug/`.
        stub: String,
        /// Codes beyond `1` that decided nothing, and the word the line prints.
        undecided: &'static [(i32, &'static str)],
        /// Outcomes beyond `1` that are THE finding, and the word the line prints.
        findings: &'static [(Finding, &'static str)],
    }
    let mut rows = vec![Row {
        id: StageId::RedrawConformance,
        label: "aterm-redraw-conformance",
        stub: stages::REDRAW_CONFORMANCE_BIN.to_string(),
        undecided: &[(2, "NOT RUN")],
        findings: &[],
    }];
    if cfg!(target_os = "macos") {
        rows.extend([
            Row {
                id: StageId::ObjcClassAudit,
                label: "objc live-class audit",
                stub: format!("examples/{}", stages::OBJC_CLASS_AUDIT_EXAMPLE),
                undecided: &[(2, "NOT RUN")],
                findings: &[],
            },
            // A SEPARATE stage from the audit because it asks a separate
            // question — the audit proves `WinitView` is shaped right, this
            // proves it composes — and a separate stage is a separate exit code
            // to misread.
            Row {
                id: StageId::ObjcImeDrive,
                label: "objc IME drive",
                stub: format!("examples/{}", stages::OBJC_IME_DRIVE_EXAMPLE),
                undecided: &[(2, "NOT RUN")],
                findings: &[],
            },
            // FOUR codes: the drive enters `-mouseDown:` IMPs directly, and a
            // hang reaching the ladder as a generic timeout would be a stage
            // that decided nothing while looking busy.
            Row {
                id: StageId::ObjcToolbarDrive,
                label: "objc toolbar drive",
                stub: format!("examples/{}", stages::OBJC_TOOLBAR_DRIVE_EXAMPLE),
                undecided: &[(2, "NOT RUN"), (3, "HUNG")],
                findings: &[],
            },
            // THREE codes: no menu, no modal tracking loop to hang in.
            Row {
                id: StageId::ObjcWindowDrive,
                label: "objc window drive",
                stub: format!("examples/{}", stages::OBJC_WINDOW_DRIVE_EXAMPLE),
                undecided: &[(2, "NOT RUN")],
                findings: &[],
            },
            Row {
                id: StageId::ObjcEventDrive,
                label: "objc event drive",
                stub: format!("examples/{}", stages::OBJC_EVENT_DRIVE_EXAMPLE),
                undecided: &[(2, "NOT RUN")],
                findings: &[(Finding::Exit(3), "ABORTED"), (Finding::Signal, "ABORTED")],
            },
            // The three the objc2 exit added read exactly as the window drive.
            Row {
                id: StageId::ObjcAlertDrive,
                label: "objc alert drive",
                stub: format!("examples/{}", stages::OBJC_ALERT_DRIVE_EXAMPLE),
                undecided: &[(2, "NOT RUN")],
                findings: &[],
            },
            Row {
                id: StageId::ObjcSwizzleDrive,
                label: "objc swizzle drive",
                stub: format!("examples/{}", stages::OBJC_SWIZZLE_DRIVE_EXAMPLE),
                undecided: &[(2, "NOT RUN")],
                findings: &[],
            },
            Row {
                id: StageId::ObjcBoundDrive,
                label: "objc bound drive",
                stub: format!("examples/{}", stages::OBJC_BOUND_DRIVE_EXAMPLE),
                undecided: &[(2, "NOT RUN")],
                findings: &[],
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
        assert_eq!(
            tally(&[stages::run_stage(&ctx, &spec)]),
            Tally::default(),
            "{name}: a clean driver leaves the run clean"
        );

        repo.driver_stub(&row.stub, 1);
        let t = tally(&[stages::run_stage(&ctx, &spec)]);
        assert_eq!(
            t.gate_failures.len(),
            1,
            "{name}: exit 1 is a finding about the tree"
        );
        assert_eq!(t.could_not_run.len(), 0, "{name}");

        for &(code, words) in row.undecided {
            repo.driver_stub(&row.stub, code);
            let r = stages::run_stage(&ctx, &spec);
            let t = tally(std::slice::from_ref(&r));
            assert_eq!(
                t.could_not_run.len(),
                1,
                "{name}: exit {code} decided nothing"
            );
            assert_eq!(
                t.gate_failures.len(),
                0,
                "{name}: …and is not a finding about the tree"
            );
            assert_eq!(t.skipped(), 0, "{name}: …and above all is not a quiet skip");
            assert!(t.failed(), "{name}: so the run cannot end green");
            assert!(
                r.render().contains(&format!("  FAIL  {name}: {words}")),
                "{}",
                r.render()
            );
        }

        for (finding, words) in row.findings {
            let what = match finding {
                Finding::Exit(code) => {
                    repo.driver_stub(&row.stub, *code);
                    format!("exit {code}")
                }
                Finding::Signal => {
                    repo.driver_script(
                        &row.stub,
                        "echo 'stub about to die by a signal'; kill -KILL $$",
                    );
                    "an untrapped signal death".to_string()
                }
            };
            let r = stages::run_stage(&ctx, &spec);
            let t = tally(std::slice::from_ref(&r));
            assert_eq!(
                t.gate_failures.len(),
                1,
                "{name}: {what} is THE finding, not harness noise"
            );
            assert_eq!(
                t.could_not_run.len(),
                0,
                "{name}: …and is never read as could-not-run"
            );
            assert!(t.failed(), "{name}: so the run cannot end green");
            assert!(
                r.render().contains(&format!("  FAIL  {name}: {words}")),
                "{}",
                r.render()
            );
        }
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

/// The sealed stage on its own: it prepares the `aterm-gui` its suite drives in
/// the suite's own target dir (the driver lane's) before the suite starts,
/// never lets a GUI in the shared `target/` stand in for it, and counts a
/// failed build and a failed suite each as the gate failure it is.
#[test]
fn sealed_lane_prepares_its_gui_before_testing_and_preserves_both_failures() {
    for (build_exit, test_exit) in [(0, 0), (19, 0), (0, 23)] {
        let repo = FakeRepo::new();
        let trace = repo.scratch.join("sealed-order");
        let target = repo.root.join("target-drivers");
        repo.with_stage2(&format!(
            r#"test "$CARGO_TARGET_DIR" = {target} || exit 70
test "$CARGO_BUILD_JOBS" = 8 || exit 71
case "$*" in
  '--unverified build -q -p aterm-gui -p aterm-ctl')
    echo build >> {trace}
    test {build_exit} = 0 || exit {build_exit}
    mkdir -p "$CARGO_TARGET_DIR/debug"
    echo fresh-sealed-gui > "$CARGO_TARGET_DIR/debug/aterm-gui"
    ;;
  '--unverified test -p aterm-link --features sealed --test two_nodes_sealed --no-fail-fast')
    test -f "$CARGO_TARGET_DIR/debug/aterm-gui" || exit 72
    test "$(cat "$CARGO_TARGET_DIR/debug/aterm-gui")" = fresh-sealed-gui || exit 73
    echo test >> {trace}
    exit {test_exit}
    ;;
  *) exit 74 ;;
esac
"#,
            target = sh_quote(&target.display().to_string()),
            trace = sh_quote(&trace.display().to_string()),
        ));
        // A shared-target artifact must not satisfy the local prerequisite.
        fs::create_dir_all(repo.root.join("target/debug")).expect("shared target");
        fs::write(
            repo.root.join("target/debug/aterm-gui"),
            b"stale-shared-gui",
        )
        .expect("stale shared GUI");
        let ctx = repo.ctx(Mode::Fast, Scope::workspace());
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::SealedLane)
            .expect("sealed stage");

        // The old test-only stage really fails against this fixture, even
        // though the shared target contains a GUI. This is the negative control.
        let old = std::process::Command::new(repo.stage2.join("targo"))
            .args(stages::sealed_lane_args())
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_BUILD_JOBS", "8")
            .current_dir(&repo.root)
            .output()
            .expect("old test-only invocation");
        assert_eq!(old.status.code(), Some(72));
        assert!(!trace.exists(), "an unprepared test must not run");

        let report = stages::run_stage(&ctx, &spec);
        let measured = fs::read_to_string(&trace).expect("stage invoked the driver");
        assert_eq!(
            measured,
            if build_exit == 0 {
                "build\ntest\n"
            } else {
                "build\n"
            },
            "{}",
            report.render()
        );
        let result = tally(std::slice::from_ref(&report));
        assert_eq!(result.could_not_run.len(), 0);
        assert_eq!(
            result.gate_failures.len(),
            usize::from(build_exit != 0 || test_exit != 0),
            "{}",
            report.render()
        );
        assert_eq!(
            report.render().contains(
                "  not run: targo test -p aterm-link --features sealed --test two_nodes_sealed — the aterm-gui build above failed"
            ),
            build_exit != 0,
            "{}",
            report.render()
        );
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
    // 2 is below the driver lane's cap of 8, so the child must see 2; the two
    // stage fixtures above pin the uncapped 8 through the same code path.
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
        let ctx = repo.ctx_with(Mode::Fast, Scope::workspace(), |env| {
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

/// THE ATPKG PUBLISH TOOLING, on its own: it builds the `atpkg` its end-to-end
/// pack suite drives, in the lane's own dir, before that suite starts; it hands
/// the suite that binary through `$ATPKG` so the script's own
/// `<root>/target/debug/atpkg` fallback cannot answer with a previous run's;
/// the two self-contained suites are handed no `ATPKG` and run either way; and
/// a failed build and a failed suite each count as the gate failure they are.
///
/// The NEGATIVE CONTROL is the shape this stage had until 2026-09-16: the pack
/// suite alone, with no `$ATPKG` — against this fixture it "passes", by packing
/// with the stale binary lying in `<root>/target/debug`.
#[test]
fn atpkg_tooling_builds_the_atpkg_its_pack_suite_drives_and_never_takes_a_stale_one() {
    for (build_exit, suite_exit) in [(0, 0), (19, 0), (0, 23)] {
        let repo = FakeRepo::new();
        let trace = repo.scratch.join("atpkg-order");
        let target = repo.root.join("target-drivers");
        repo.with_stage2(&format!(
            r#"test "$CARGO_TARGET_DIR" = {target} || exit 70
test "$CARGO_BUILD_JOBS" = 8 || exit 71
case "$*" in
  '--unverified build -q -p atpkg')
    echo build >> {trace}
    test {build_exit} = 0 || exit {build_exit}
    mkdir -p "$CARGO_TARGET_DIR/debug"
    printf '#!/bin/sh\necho fresh-atpkg\n' > "$CARGO_TARGET_DIR/debug/atpkg"
    chmod 755 "$CARGO_TARGET_DIR/debug/atpkg"
    ;;
  *) exit 74 ;;
esac
"#,
            target = sh_quote(&target.display().to_string()),
            trace = sh_quote(&trace.display().to_string()),
        ));
        // The stale binary the script's own fallback finds: a previous run's,
        // or — under a `--scope` narrowing — one this run never rebuilt.
        fs::create_dir_all(repo.root.join("target/debug")).expect("shared target");
        repo.script("target/debug/atpkg", "echo stale-atpkg");
        // The pack suite, resolving its binary exactly as
        // `tools/test-atpkg-pack-one-compiler.sh` section D does.
        repo.script(
            "tools/test-atpkg-pack-one-compiler.sh",
            &format!(
                r#"bin="$ATPKG"
if [ -z "$bin" ]; then
  for c in {root}/target/debug/atpkg {root}/target/release/atpkg; do
    if [ -x "$c" ]; then bin="$c"; break; fi
  done
fi
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "section D (the real pack end to end) cannot run: no atpkg binary" >&2
  exit 1
fi
echo "pack $("$bin")" >> {trace}
exit {suite_exit}
"#,
                root = sh_quote(&repo.root.display().to_string()),
                trace = sh_quote(&trace.display().to_string()),
            ),
        );
        // The two self-contained suites: they must be handed NO `$ATPKG`, or
        // the cases that measure a producer script without one stop measuring it.
        for name in [
            "test-atpkg-vendor-tooling.sh",
            "test-atpkg-mirror-extras.sh",
        ] {
            repo.script(
                &format!("tools/{name}"),
                &format!(
                    "[ -z \"$ATPKG\" ] || exit 66\necho {name} >> {trace}\nexit 0",
                    trace = sh_quote(&trace.display().to_string()),
                ),
            );
        }

        let ctx = repo.ctx(Mode::Fast, Scope::workspace());
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::AtpkgTooling)
            .expect("atpkg publish tooling stage");

        // The negative control: the pre-2026-09-16 shape, which took the stale
        // binary and reported a pass.
        let old =
            std::process::Command::new(repo.root.join("tools/test-atpkg-pack-one-compiler.sh"))
                .current_dir(&repo.root)
                .env_remove("ATPKG")
                .output()
                .expect("old pure-stage invocation");
        // It ran — reaching the suite's own verdict, whatever that is — which
        // is the point: nothing stopped it packing with a binary no stage of
        // this run built.
        assert_eq!(old.status.code(), Some(suite_exit), "{old:?}");
        assert_eq!(
            fs::read_to_string(&trace).expect("the control packed"),
            "pack stale-atpkg\n",
            "the control must demonstrate the stale fallback"
        );
        fs::write(&trace, "").expect("reset the trace");

        let report = stages::run_stage(&ctx, &spec);
        let measured = fs::read_to_string(&trace).expect("the stage ran its children");
        assert_eq!(
            measured,
            if build_exit == 0 {
                "build\ntest-atpkg-vendor-tooling.sh\ntest-atpkg-mirror-extras.sh\npack fresh-atpkg\n"
            } else {
                "build\ntest-atpkg-vendor-tooling.sh\ntest-atpkg-mirror-extras.sh\n"
            },
            "{}",
            report.render()
        );
        assert!(
            !measured.contains("stale-atpkg"),
            "the stage drove the stale binary: {}",
            report.render()
        );

        let result = tally(std::slice::from_ref(&report));
        assert_eq!(result.could_not_run.len(), 0, "{}", report.render());
        assert_eq!(
            result.gate_failures.len(),
            usize::from(build_exit != 0 || suite_exit != 0),
            "{}",
            report.render()
        );
        assert_eq!(
            report.render().contains(
                "  not run: test-atpkg-pack-one-compiler.sh — the atpkg build above failed"
            ),
            build_exit != 0,
            "{}",
            report.render()
        );
    }
}

/// A repo whose stage2 builds the live lanes' `aterm` into the driver lane (or
/// exits `build_exit`), with a STALE `aterm` in `<root>/target/debug` — the
/// lanes' own default — and each lane stubbed to resolve its binary exactly as
/// its script does, append `<lane> <what the binary printed>` to the trace, and
/// exit as told. FakeRepo seeds a driven `aterm` for the whole-ladder tests;
/// this one is removed, so what the stage drives is what its OWN build left.
fn live_lane_repo(build_exit: i32, handback_exit: i32) -> (FakeRepo, PathBuf) {
    let repo = FakeRepo::new();
    let trace = repo.scratch.join("live-order");
    let target = repo.root.join("target-drivers");
    fs::remove_file(target.join("debug/aterm")).expect("unseed the driven aterm");
    repo.with_stage2(&format!(
        r#"test "$CARGO_TARGET_DIR" = {target} || exit 70
test "$CARGO_BUILD_JOBS" = 8 || exit 71
case "$*" in
  '--unverified build -q -p aterm --bin aterm')
    echo build >> {trace}
    test {build_exit} = 0 || exit {build_exit}
    mkdir -p "$CARGO_TARGET_DIR/debug"
    printf '#!/bin/sh\necho fresh-aterm\n' > "$CARGO_TARGET_DIR/debug/aterm"
    chmod 755 "$CARGO_TARGET_DIR/debug/aterm"
    ;;
  *) exit 74 ;;
esac
"#,
        target = sh_quote(&target.display().to_string()),
        trace = sh_quote(&trace.display().to_string()),
    ));
    fs::create_dir_all(repo.root.join("target/debug")).expect("shared target");
    repo.script("target/debug/aterm", "echo stale-aterm");
    let root = sh_quote(&repo.root.display().to_string());
    let tr = sh_quote(&trace.display().to_string());
    // tools/test-foreground-handback.sh's resolution: `--binary`, else the
    // checkout's target/debug/aterm; `2` when there is none.
    repo.script(
        "tools/test-foreground-handback.sh",
        &format!(
            r#"BIN={root}/target/debug/aterm
while [ $# -gt 0 ]; do case $1 in --binary) BIN=$2; shift 2 ;; *) exit 2 ;; esac; done
[ -x "$BIN" ] || {{ echo "no aterm binary at $BIN" >&2; exit 2; }}
echo "handback $("$BIN")" >> {tr}
exit {handback_exit}
"#
        ),
    );
    // tools/test-codex-live-upgrade.sh's: the first argument, else the
    // checkout's target/debug/aterm; `77` (SKIP) when there is none.
    repo.script(
        "tools/test-codex-live-upgrade.sh",
        &format!(
            r#"A=${{1:-{root}/target/debug/aterm}}
[ -x "$A" ] || {{ echo "SKIP: no aterm at $A"; exit 77; }}
echo "codex $("$A")" >> {tr}
exit 0
"#
        ),
    );
    (repo, trace)
}

/// The lanes as a person ran them before 2026-09-26 — by hand, no argument —
/// which is each stage's NEGATIVE CONTROL: with a stale binary in
/// `<root>/target/debug` the lane drives THAT, and with none it answers its
/// not-run code; neither says anything about this tree.
fn live_lane_by_hand(repo: &FakeRepo, trace: &Path, suite: &str, lane: &str, not_run: i32) {
    fs::write(trace, "").expect("reset the trace");
    std::process::Command::new(repo.root.join(suite))
        .current_dir(&repo.root)
        .output()
        .expect("the lane as run by hand");
    assert_eq!(
        fs::read_to_string(trace).expect("the control ran"),
        format!("{lane} stale-aterm\n"),
        "the control must demonstrate the stale fallback"
    );
    fs::remove_file(repo.root.join("target/debug/aterm")).expect("rm stale");
    let bare = std::process::Command::new(repo.root.join(suite))
        .current_dir(&repo.root)
        .output()
        .expect("the lane with no binary at all");
    assert_eq!(bare.status.code(), Some(not_run), "{bare:?}");
    repo.script("target/debug/aterm", "echo stale-aterm");
    fs::write(trace, "").expect("reset the trace");
}

/// THE FOREGROUND HANDBACK, on its own: the stage builds the `aterm` the lane
/// drives, in the driver lane's dir, before the lane starts; it hands the lane
/// that binary as `--binary <path>`, so the lane's own `<root>/target/debug/aterm`
/// default cannot answer; a failed build runs nothing; and the lane's not-run
/// code, `2`, is COULD NOT RUN with the lane's reason, never a pass. Off macOS,
/// where the lane has never been measured and its bash row pins macOS's
/// `/bin/bash` 3.2, the stage is ONE named skip and builds nothing.
#[test]
fn the_foreground_handback_drives_the_aterm_its_stage_built_and_never_reads_not_run_as_a_pass() {
    for (build_exit, handback_exit) in [(0, 0), (19, 0), (0, 1), (0, 2)] {
        let (repo, trace) = live_lane_repo(build_exit, handback_exit);
        live_lane_by_hand(
            &repo,
            &trace,
            "tools/test-foreground-handback.sh",
            "handback",
            2,
        );

        let ctx = repo.ctx(Mode::Fast, Scope::workspace());
        let spec = plan::plan(&ctx)
            .into_iter()
            .find(|s| s.id == StageId::ForegroundHandback)
            .expect("the foreground handback is planned");
        let report = stages::run_stage(&ctx, &spec);
        let what = format!("({build_exit}, {handback_exit})\n{}", report.render());
        let measured = fs::read_to_string(&trace).expect("the stage ran its children");
        let runs = cfg!(target_os = "macos");
        let want = match (runs, build_exit) {
            (false, _) => "",
            (true, 0) => "build\nhandback fresh-aterm\n",
            (true, _) => "build\n",
        };
        assert_eq!(measured, want, "{what}");

        let result = tally(std::slice::from_ref(&report));
        assert_eq!(
            result.gate_failures.len(),
            usize::from(runs && (build_exit != 0 || handback_exit == 1)),
            "{what}"
        );
        assert_eq!(
            result.could_not_run.len(),
            usize::from(runs && build_exit == 0 && handback_exit == 2),
            "{what}"
        );
        assert_eq!(result.skipped(), usize::from(!runs), "{what}");
        let rendered = report.render();
        if !runs {
            assert!(
                rendered.contains(&format!(
                    "  skip  test-foreground-handback.sh (macOS only: {})",
                    stages::live_aterm_macos_only(stages::FOREGROUND_HANDBACK_SUITE)
                )),
                "{what}"
            );
        }
        assert_eq!(
            rendered
                .contains("  not run: test-foreground-handback.sh — the aterm build above failed"),
            runs && build_exit != 0,
            "{what}"
        );
        if runs && build_exit == 0 && handback_exit == 2 {
            assert!(
                rendered.contains("test-foreground-handback.sh: NOT RUN — "),
                "{what}"
            );
        }
        // The Codex lane is `--full`'s: the per-commit stage never starts it.
        assert!(!measured.contains("codex"), "{what}");
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
