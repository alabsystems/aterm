// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fixture half `gate_contract.rs` and `environment_contract.rs` share: the
//! run context over a synthetic repo, and the stand-in smoke both ladders launch.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aterm_verify::{Ctx, EnvSnapshot, Mode, Scope};

/// Run `path` once with `arg`, unbounded, output discarded (`aterm-cli`'s
/// `manual.rs` `run_once`, the repo's rule since bc2918c70).
///
/// WHY (2026-09-28). The first exec of a file this process wrote waits on
/// macOS's assessment of it, and later execs do not: 0.4 s idle, past 60 s
/// under load. Every stand-in here is such a file, and in a merge-contract run
/// (`verify-bcbd1f9d0`, run 2: twenty `exit 0` stand-ins took 366.7 s, at load
/// 1.5-2.5 on 14 cores) `a_spec_checker_repointed_mid_run_never_claims_the_contract`
/// read `no --version answer` where it asserts `ty 1`, and
/// `a_red_main_already_has_is_inherited_and_a_new_one_blocks` counted a second
/// red, `smoke: control socket never started listening`: a `ty` stand-in's
/// first exec past the 5 s deadline, a stand-in `aterm-gui`'s past the listen
/// wait. Paying that first exec here, where nothing bounds it, keeps it out
/// of the bound a run puts on the same file. The bounds stay a real run's
/// (f6921d3de waited 300 s instead, which let a stand-in slow at EVERY exec —
/// a real regression — pass a fixture that a real run fails, after five
/// minutes a question: the review of 2026-09-28).
pub fn run_once(path: &Path, arg: &str) {
    let _ = Command::new(path)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A run context over the synthetic repo at `root` with the stand-in toolchain
/// `stage2` and the HOME `home` ([`checker_home`]), and `tweak` given the last
/// say over the environment the stages read.
///
/// THE ENVIRONMENT IS BUILT, NOT CAPTURED: `PATH` from the caller, a `HOME` of
/// the fixture's own, and nothing else, so a variable the developer or the gate
/// running this suite exports cannot change what a fixture decides. Measured
/// 2026-09-16: the merge gate exports `CARGO_BUILD_JOBS=4` on a 4-core Mac, the
/// driver lane capped its 8 to that, and fixtures pinning the lane's own cap
/// failed at exit 71. The Tier-2 prover locations point inside the sandbox, so
/// a machine with trust-mc built decides the same as one without.
///
/// THE HOME IS THE FIXTURE'S (2026-09-28). With the caller's, a run named the
/// caller's own spec checkers — `verify-bcbd1f9d0` run 2's fixture receipts
/// named the live atpkg store's `ty`, `trust-ir` and `ay` builds, and the PATH
/// ones where a fixture's HOME had none — asked each its version under the
/// same deadline, and armed the checker tripwire on them, so an `aterm pkg
/// update` or gc during the suite could make a fixture COULD NOT RUN, or make
/// main's receipt and a branch's name different checkers. [`checker_home`]
/// lays a store shim for every checker, so discovery stops at the fixture's
/// store and never walks PATH.
///
/// THE BOUNDS ARE A REAL RUN'S (the review of 2026-09-28): each stand-in's
/// first exec is paid before the run instead ([`run_once`]), by
/// [`checker_home`] and [`lay_checker`] for the checkers and by the build that
/// writes the stand-in instance ([`answering_smoke`]).
///
/// THE DISK FLOOR IS ZERO. A fixture with a fake toolchain builds nothing, and
/// the real floor made these ladders refuse at the disk preflight whenever the
/// HOST volume held less than it (2026-09-23, 17.6 GiB free: 23 failures, every
/// one a `disk preflight` COULD NOT RUN); the estimate that replaced it would
/// too, since a fixture's empty lanes are budgeted cold. The preflight's own
/// laws set the requirement they need. And the GUI smoke measures a real
/// window, which a synthetic repo has none of, so it takes its honest skip.
pub fn fixture_ctx(
    root: &Path,
    stage2: &Path,
    scratch: &Path,
    home: &Path,
    mode: Mode,
    scope: Scope,
    tweak: impl FnOnce(&mut EnvSnapshot),
) -> Ctx {
    let mut env = EnvSnapshot {
        path: std::env::var_os("PATH").unwrap_or_default(),
        home: home.to_path_buf(),
        trust_stage2_bin: Some(stage2.to_path_buf()),
        trust_mc_sysroot: Some(root.join("no-trust-mc")),
        ay_bin_dir: Some(root.join("no-ay")),
        ..EnvSnapshot::default()
    };
    tweak(&mut env);
    Ctx::new(root.to_path_buf(), mode, scope, env, scratch.to_path_buf())
        .with_disk_floor(0)
        .with_gui_smoke_skipped(true)
}

/// Lay the fixture HOME `home`: for every spec checker
/// ([`aterm_verify::checkers::NAMES`]), a stand-in at
/// `<home>/store/<name>/0/bin/<name>` that answers `<name> fixture`, and the
/// atpkg `sh` exec stub that forwards to it in the store's shim directory
/// ([`aterm_verify::checkers::store_bin_dir`]) — so every checker resolves at
/// the store tier, as `aterm-spec`'s discovery would find it. A test that
/// needs a checker of its own lays it over the default one ([`lay_checker`]).
pub fn checker_home(home: &Path) {
    for name in aterm_verify::checkers::NAMES {
        lay_checker(home, name, "0", &format!("echo '{name} fixture'"));
    }
}

/// Lay one spec checker `name` of build `build` under the fixture HOME `home`:
/// a stand-in at `<home>/store/<name>/<build>/bin/<name>` whose body is
/// `body`, and the atpkg `sh` exec stub forwarding to it in the store's shim
/// directory. The stand-in — what a run resolves the stub to and asks its
/// `--version` — is run once before any run asks it ([`run_once`]), so its
/// first exec never falls inside the deadline. The stand-in's path.
pub fn lay_checker(home: &Path, name: &str, build: &str, body: &str) -> PathBuf {
    let store_bin = aterm_verify::checkers::store_bin_dir(home);
    fs::create_dir_all(&store_bin).expect("mkdir the store's shim directory");
    let target = home.join(format!("store/{name}/{build}/bin/{name}"));
    fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
    fs::write(&target, format!("#!/bin/sh\n{body}\n")).expect("write");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).expect("chmod");
    fs::write(
        store_bin.join(name),
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", target.display()),
    )
    .expect("write the shim");
    run_once(&target, "--version");
    target
}

/// The stand-in `targo`'s answer to the smoke's build: an `aterm-gui` that
/// checks every precondition the smoke promises the instance it launches — exit
/// 81-92, one code per broken promise — then links its control socket to
/// `listener` (the fixture's own listening socket: the smoke waits for a socket
/// that accepts a connect, not for a file) and stays up; and an `aterm-ctl` that
/// answers the verbs the headless smoke drives on the socket it was handed.
/// A `case` block for the stub's body; the caller adds the rest.
///
/// THE BUILD RUNS THE INSTANCE IT WROTE ONCE (the review of 2026-09-28,
/// [`run_once`]'s rule): with no arguments it fails its first precondition and
/// exits at once, and its first exec — the assessment of the new file — is
/// paid in the build stage, which nothing but the child ceiling bounds, never
/// inside the smoke's listen wait.
pub fn answering_smoke(listener: &Path) -> String {
    r#"case "$*" in
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
# one binary, nine of them missed it (2026-09-27). Same lines, same codes.
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
  metrics) echo "OK frames=41 max_input_present_ms=8.100 redraw_retry_gated=0 present_drops=0 sync_rel_timeout=0 perf_reduced=0 wake_heals=0 " ;;
  cursor) echo "OK 0 0 1 blinking_block" ;;
  send) echo "OK" ;;
  *) echo "ERR unknown verb"; exit 1 ;;
esac
CTL
    chmod 755 "$CARGO_TARGET_DIR/debug/aterm-gui" "$CARGO_TARGET_DIR/debug/aterm-ctl"
    "$CARGO_TARGET_DIR/debug/aterm-gui" </dev/null >/dev/null 2>&1 || :
    ;;
esac"#
        .replace("@LISTENER@", &listener.display().to_string())
}
