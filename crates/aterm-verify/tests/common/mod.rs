// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fixture half `gate_contract.rs` and `environment_contract.rs` share: the
//! run context over a synthetic repo, and the stand-in smoke both ladders launch.

use std::path::Path;

use aterm_verify::{Ctx, EnvSnapshot, Mode, Scope};

/// A run context over the synthetic repo at `root` with the stand-in toolchain
/// `stage2`, and `tweak` given the last say over the environment the stages read.
///
/// THE ENVIRONMENT IS BUILT, NOT CAPTURED: `PATH` and `HOME` from the caller and
/// nothing else, so a variable the developer or the gate running this suite
/// exports cannot change what a fixture decides. Measured 2026-09-16: the merge
/// gate exports `CARGO_BUILD_JOBS=4` on a 4-core Mac, the driver lane capped its
/// 8 to that, and fixtures pinning the lane's own cap failed at exit 71. The
/// Tier-2 prover locations point inside the sandbox, so a machine with trust-mc
/// built decides the same as one without.
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
    mode: Mode,
    scope: Scope,
    tweak: impl FnOnce(&mut EnvSnapshot),
) -> Ctx {
    let mut env = EnvSnapshot {
        path: std::env::var_os("PATH").unwrap_or_default(),
        home: std::env::var_os("HOME").unwrap_or_default().into(),
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

/// The stand-in `targo`'s answer to the smoke's build: an `aterm-gui` that
/// checks every precondition the smoke promises the instance it launches — exit
/// 81-92, one code per broken promise — then links its control socket to
/// `listener` (the fixture's own listening socket: the smoke waits for a socket
/// that accepts a connect, not for a file) and stays up; and an `aterm-ctl` that
/// answers the verbs the headless smoke drives on the socket it was handed.
/// A `case` block for the stub's body; the caller adds the rest.
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
    ;;
esac"#
        .replace("@LISTENER@", &listener.display().to_string())
}
