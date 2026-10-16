// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fixture half of the integration suites: the run context over a
//! synthetic repo, and the stand-in smoke a run launches.

use std::fs;
use std::path::{Path, PathBuf};

use aterm_verify::checks::Check;
use aterm_verify::smoke_stages::Pacing;
use aterm_verify::{Ctx, EnvSnapshot, Scope};

/// Put an executable `#!/bin/sh` stand-in running `body` at `path`, replacing
/// whatever is there: one file per distinct text, hard-linked into place, so
/// macOS's first-exec assessment is paid once per text and not once per
/// fixture. [`aterm_verify::standin`] says why and how to write the text.
pub fn standin(path: &Path, body: &str) {
    aterm_verify::standin(path, body);
}

/// [`standin`] for a text that cannot be shared (it bakes in a path, or a code):
/// the one constant text `. "$0.body"` is hard-linked into place and `body` is
/// written beside it as a data file the stand-in sources, so every fixture of
/// the contract binary shares one inode, assessed once. [`aterm_verify::standin_body`]
/// says what a sourced body changes (`$0` is the link, `return` and a syntax
/// error read differently) and what it is not for: a stand-in whose changing
/// stamp a tripwire must see, a compiler above all, stays on [`standin`].
pub fn standin_body(path: &Path, body: &str) {
    aterm_verify::standin_body(path, body);
}

/// A run context over the synthetic repo at `root` with the stand-in toolchain
/// `stage2`, the HOME `home` and the checks `checks`, and `tweak` given the last
/// say over the environment the stages read.
///
/// THE ENVIRONMENT IS BUILT, NOT CAPTURED: `PATH` from the caller, a `HOME` of
/// the fixture's own, and nothing else, so a variable the developer or the run
/// executing this suite exports cannot change what a fixture decides.
///
/// NO WINDOWS: a fixture's run is a test on a machine a person may be using,
/// so the stages that put a window on the desktop take their `--no-windows`
/// skip; a test that reads one of their rows over stand-in drivers turns it
/// off. THE HEADLESS SMOKE IS NOT PACED ([`Pacing::INSTANT`]): the `aterm-ctl`
/// it drives answers canned metrics. A run with `measure` is handed the
/// fixture's own bundle ([`fixture_bundle`]).
pub fn fixture_ctx(
    root: &Path,
    stage2: &Path,
    scratch: &Path,
    home: &Path,
    checks: &[Check],
    scope: Scope,
    tweak: impl FnOnce(&mut EnvSnapshot),
) -> Ctx {
    let mut env = EnvSnapshot {
        path: std::env::var_os("PATH").unwrap_or_default(),
        home: home.to_path_buf(),
        trust_stage2_bin: Some(stage2.to_path_buf()),
        ..EnvSnapshot::default()
    };
    tweak(&mut env);
    let artifact = checks.contains(&Check::Measure).then(|| {
        let dir = scratch.parent().unwrap_or(scratch).join("release");
        aterm_verify::MeasureArtifact::at(&fixture_bundle(&dir)).expect("the fixture bundle")
    });
    Ctx::new(root.to_path_buf(), scope, env, scratch.to_path_buf())
        .with_checks(checks)
        .with_no_windows(true)
        .with_smoke_pacing(Pacing::INSTANT)
        .with_measure_artifact(artifact)
}

/// A release bundle for `measure` to be handed (`--app <APP>`), at
/// `<dir>/aterm.app`, laid once: an executable `Contents/MacOS/aterm` and the
/// argv0 names the GUI smoke drives linked to it. Keep `dir` outside the
/// fixture's checkout.
pub fn fixture_bundle(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let app = dir.join("aterm.app");
    let macos = app.join("Contents/MacOS");
    let aterm = macos.join("aterm");
    if !aterm.exists() {
        fs::create_dir_all(&macos).expect("mkdir the bundle");
        fs::write(&aterm, "#!/bin/sh\nexit 0\n").expect("write the bundle's aterm");
        fs::set_permissions(&aterm, fs::Permissions::from_mode(0o755)).expect("chmod");
        for alias in ["aterm-gui", "aterm-ctl"] {
            std::os::unix::fs::symlink("aterm", macos.join(alias)).expect("alias");
        }
    }
    app
}

/// The stand-in `targo`'s answer to the smoke's build: an `aterm-gui` that
/// checks every precondition the smoke promises the instance it launches — exit
/// 81-92, one code per broken promise — then links its control socket to
/// `listener` (the fixture's own listening socket: the smoke waits for a socket
/// that accepts a connect, not for a file) and stays up; and an `aterm-ctl` that
/// answers the verbs the headless smoke drives on the socket it was handed.
/// A `case` block for the stub's body; the caller adds the rest.
///
/// THE BUILD LINKS THE TWO, IT DOES NOT WRITE THEM. Written with `cat >`, every
/// build would make two new files, and a new file's first exec waits on macOS's
/// assessment of it (176-399 ms each at load 4, served one file at a time
/// machine-wide). Each is the one constant [`standin_body`] text,
/// hard-linked into the target dir from `smoke-standin` beside `listener` (laid
/// here), with its behaviour written beside it as `<name>.body`, data it
/// sources: one inode, assessed once per process. The same lines run in the
/// same shell — `$0` is still the binary's own path, `$@` its arguments, `exit`
/// and `exec` its own ([`aterm_verify::standin_body`] lists what a sourced body
/// changes, none of which these use) — so the instance checks the same promises
/// with the same codes and the smoke reads the same answers. A link that cannot
/// be made leaves the binaries missing, which the smoke names; it never falls
/// back.
///
/// THE BUILD RUNS THE INSTANCE IT LINKED ONCE: with no
/// arguments it fails its first precondition and exits at once, and its first
/// exec — the assessment, should the link be a file of its own — is paid in the
/// build stage, which nothing but the child ceiling bounds, never inside the
/// smoke's listen wait.
pub fn answering_smoke(listener: &Path) -> String {
    let linked = listener.with_file_name("smoke-standin");
    standin(&linked, r#". "$0.body""#);
    r#"case "$*" in
  *aterm-gui*aterm-ctl*)
    mkdir -p "$CARGO_TARGET_DIR/debug"
    cat >"$CARGO_TARGET_DIR/debug/aterm-gui.body" <<'GUI'
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
# counts against the smoke's listen wait, which heavy load can exhaust. Same
# lines, same codes.
seen=
while IFS= read -r line || test -n "$line"; do
  case "$line" in
    'agents_auto_prime = false') seen="$seen a" ;;
    '[update]') seen="$seen u" ;;
    '[packages]') seen="$seen p" ;;
    'enabled = false') seen="$seen e" ;;
    '[machine]') seen="$seen m" ;;
    'universal_control = "leave"') seen="$seen c" ;;
  esac
done <"$config" || exit 87
for want in a:87 u:91 p:82 e:83 m:84 c:86; do
  case "$seen " in *" ${want%%:*} "*) ;; *) exit "${want#*:}" ;; esac
done
mkdir -p "$XDG_RUNTIME_DIR/aterm"
ln -s '@LISTENER@' "$XDG_RUNTIME_DIR/aterm/aterm.sock"
exec sleep 300
GUI
    cat >"$CARGO_TARGET_DIR/debug/aterm-ctl.body" <<'CTL'
test "$1" = --sock && test "$2" = "$XDG_RUNTIME_DIR/aterm/aterm.sock" || exit 88
shift 2
case "$1" in
  metrics) echo "OK frames=41 max_input_present_ms=8.100 redraw_retry_gated=0 present_drops=0 sync_rel_timeout=0 perf_reduced=0 wake_heals=0 " ;;
  cursor) echo "OK 0 0 1 blinking_block" ;;
  send) echo "OK" ;;
  *) echo "ERR unknown verb"; exit 1 ;;
esac
CTL
    ln -f '@LINKED@' "$CARGO_TARGET_DIR/debug/aterm-gui"
    ln -f '@LINKED@' "$CARGO_TARGET_DIR/debug/aterm-ctl"
    "$CARGO_TARGET_DIR/debug/aterm-gui" </dev/null >/dev/null 2>&1 || :
    ;;
esac"#
        .replace("@LISTENER@", &listener.display().to_string())
        .replace("@LINKED@", &linked.display().to_string())
}
