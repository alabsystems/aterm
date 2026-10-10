// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fixture half `gate_contract.rs` and `environment_contract.rs` share: the
//! run context over a synthetic repo, and the stand-in smoke both ladders launch.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aterm_verify::smoke_stages::Pacing;
use aterm_verify::{Ctx, EnvSnapshot, Mode, Scope};

/// Run `path` once with `arg`, unbounded, output discarded (as `aterm-cli`'s
/// `manual.rs` `run_once`).
///
/// WHY. The first exec of a file this process wrote waits on macOS's
/// assessment of it, and later execs do not: 0.4 s idle, past 60 s under load.
/// Every stand-in here is such a file, and under load a stand-in's first exec
/// outran a run's 5 s `--version` deadline and the smoke's listen wait, so
/// fixtures read false reds. Paying that first exec here, where nothing bounds
/// it, keeps it out of the bound a run puts on the same file. The bounds stay
/// a real run's: widening them would let a stand-in slow at EVERY exec — a
/// real regression — pass a fixture that a real run fails.
///
/// ONCE PER FILE, NOT PER CALL. What is assessed is the file, whatever path
/// reaches it: measured at load 4, a fresh script's first exec took 176-225 ms
/// and its second 3-10 ms, a NEW hard link of it 2.8-5.7 ms. Every fixture's
/// checkers are [`standin`] links of one text per checker, so re-running per
/// call would cost three spawns per fixture for nothing. The file is keyed by
/// its device, inode, mtime and length (a file rewritten in place is run
/// again), and a caller that finds the file being run by another WAITS for
/// that run to end, so no caller returns before the file's first exec has.
pub fn run_once(path: &Path, arg: &str) {
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    type Key = (u64, u64, i64, i64, u64);
    static RAN: Mutex<BTreeMap<Key, Arc<OnceLock<()>>>> = Mutex::new(BTreeMap::new());

    let run = || {
        let _ = Command::new(path)
            .arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    let Ok(m) = fs::metadata(path) else {
        return run();
    };
    let key = (m.dev(), m.ino(), m.mtime(), m.mtime_nsec(), m.size());
    let once = Arc::clone(
        RAN.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key)
            .or_default(),
    );
    once.get_or_init(run);
}

/// Put an executable `#!/bin/sh` stand-in running `body` at `path`, replacing
/// whatever is there: one file per distinct text, hard-linked into place, so
/// the first-exec assessment ([`run_once`]) is paid once per text and not once
/// per fixture. [`aterm_verify::standin`] says why and how to write the text.
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

/// A copy of the real tool `src` at `path`, executable: hard-linked from ONE
/// copy per process (per source as it stood — path, mtime and length — when a
/// fixture first asked), so a real script that several fixtures run is one
/// first exec, and one assessment ([`run_once`]), per process instead of one
/// per fixture (the export content scan is copied by four fixtures).
/// Read-only through every link, as a [`standin`] is; a link that cannot be
/// made (another filesystem) is a plain copy of its own, slower and as correct.
pub fn shared_copy(src: &Path, path: &Path) {
    use std::collections::BTreeMap;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::sync::{Mutex, OnceLock, PoisonError};
    type Key = (PathBuf, i64, i64, u64);
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    static COPIES: Mutex<BTreeMap<Key, PathBuf>> = Mutex::new(BTreeMap::new());

    let m = fs::metadata(src).expect("the tool to copy");
    let key = (src.to_path_buf(), m.mtime(), m.mtime_nsec(), m.size());
    let _ = fs::remove_file(path);
    let shared = DIR
        .get_or_init(|| aterm_verify::mktemp_dir("atv-copies").ok())
        .as_ref()
        .and_then(|dir| {
            let mut copies = COPIES.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(file) = copies.get(&key) {
                return Some(file.clone());
            }
            let file = dir.join(copies.len().to_string());
            fs::copy(src, &file).ok()?;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o555)).ok()?;
            copies.insert(key, file.clone());
            Some(file)
        });
    if shared.is_some_and(|file| fs::hard_link(file, path).is_ok()) {
        return;
    }
    fs::copy(src, path).expect("copy the tool");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// The `git` a fixture's own `git` calls run: the program `Command::new("git")`
/// would find on this process's PATH, except that macOS's `/usr/bin/git` — a
/// shim that asks `xcrun` for the developer tools' `git` and execs it — is
/// replaced by the binary it would exec, asked once per process.
///
/// WHY. The shim is a second exec and a lookup on every call:
/// measured, 6.9 ms a `git rev-parse` through it against 3.1 ms direct (load
/// ~6, medians of 30), and this binary's fixtures make some 280 such calls a
/// run. The git that runs is the one the shim runs; what the shim adds to its
/// environment (`SDKROOT`, `CPATH`, `LIBRARY_PATH`, `MANPATH`: compiler
/// settings) no git command here reads. A PATH that reaches another `git`
/// first, or an entry that is not an absolute directory, is left to
/// `Command::new("git")` as before, as is a host where `xcrun` answers nothing.
pub fn git_program() -> &'static std::ffi::OsStr {
    use std::sync::OnceLock;
    static GIT: OnceLock<std::ffi::OsString> = OnceLock::new();
    GIT.get_or_init(|| {
        let shim = Path::new("/usr/bin/git");
        let first = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.is_absolute().then(|| dir.join("git")))
                .find(|git| {
                    git.as_ref()
                        .is_none_or(|g| aterm_verify::is_executable_file(g))
                })
                .flatten()
        });
        if !cfg!(target_os = "macos") || first.as_deref() != Some(shim) {
            return "git".into();
        }
        Command::new("/usr/bin/xcrun")
            .args(["--find", "git"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
            .filter(|git| git.is_absolute() && aterm_verify::is_executable_file(git))
            .map_or_else(|| "git".into(), PathBuf::into_os_string)
    })
}

/// A run context over the synthetic repo at `root` with the stand-in toolchain
/// `stage2` and the HOME `home` ([`checker_home`]), and `tweak` given the last
/// say over the environment the stages read.
///
/// THE ENVIRONMENT IS BUILT, NOT CAPTURED: `PATH` from the caller, a `HOME` of
/// the fixture's own, and nothing else, so a variable the developer or the gate
/// running this suite exports cannot change what a fixture decides. Measured:
/// the merge gate exports `CARGO_BUILD_JOBS=4` on a 4-core Mac, the
/// driver lane capped its 8 to that, and fixtures pinning the lane's own cap
/// failed at exit 71.
///
/// THE HOME IS THE FIXTURE'S. With the caller's, a run named the caller's own
/// spec checkers (the live atpkg store's `ty`, `trust-ir` and `ay`, or PATH's
/// where a fixture's HOME had none), asked each its version under the
/// same deadline, and armed the checker tripwire on them, so an `aterm pkg
/// update` or gc during the suite could make a fixture COULD NOT RUN, or make
/// main's receipt and a branch's name different checkers. [`checker_home`]
/// lays a store shim for every checker, so discovery stops at the fixture's
/// store and never walks PATH.
///
/// THE BOUNDS ARE A REAL RUN'S: each stand-in's
/// first exec is paid before the run instead ([`run_once`]), by
/// [`checker_home`] and [`lay_checker`] for the checkers and by the build that
/// writes the stand-in instance ([`answering_smoke`]).
///
/// THE DISK FLOOR IS ZERO. A fixture with a fake toolchain builds nothing, and
/// the real floor made these ladders refuse at the disk preflight whenever the
/// HOST volume held less than it (at 17.6 GiB free, 23 `disk preflight` COULD
/// NOT RUNs); the estimate that replaced it would
/// too, since a fixture's empty lanes are budgeted cold. The preflight's own
/// laws set the requirement they need. And NO WINDOWS: a fixture's run is a
/// test inside the merge contract, on a machine a person may be using, so the
/// stages that put a window on the desktop take their `--no-windows` skip —
/// a test that reads one of their rows over stand-in drivers turns it off.
///
/// THE HEADLESS SMOKE IS NOT PACED ([`Pacing::INSTANT`]): the
/// `aterm-ctl` it drives is [`answering_smoke`]'s stub answering canned
/// metrics, so the ~2.75 s of pre-burst pause, key gaps and settle measured
/// nothing. Every one of the 30 sends still happens.
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
        ..EnvSnapshot::default()
    };
    tweak(&mut env);
    // A run of the MEASURE tier is handed the bundle it judges, as the CLI
    // requires (`--measure <APP>`): the fixture's own, beside its scratch.
    let artifact = mode.runs(aterm_verify::plan::Tier::Measure).then(|| {
        let dir = scratch.parent().unwrap_or(scratch).join("release");
        aterm_verify::MeasureArtifact::at(&fixture_bundle(&dir)).expect("the fixture bundle")
    });
    Ctx::new(root.to_path_buf(), mode, scope, env, scratch.to_path_buf())
        .with_disk_floor(0)
        .with_no_windows(true)
        .with_smoke_pacing(Pacing::INSTANT)
        .with_measure_artifact(artifact)
}

/// A release bundle for the MEASURE tier to be handed (`--measure <APP>`), at
/// `<dir>/aterm.app`, laid once: an executable `Contents/MacOS/aterm` whose
/// bytes are its own (`exit 0`), and the argv0 names the GUI smoke drives
/// linked to it. Keep `dir` outside the fixture's checkout, whose source would
/// otherwise carry it as an untracked file.
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
    standin(&target, body);
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
/// THE BUILD LINKS THE TWO, IT DOES NOT WRITE THEM. Written with `cat >`, every
/// build would make two new files, and a new file's first exec waits on macOS's
/// assessment of it (176-399 ms each at load 4, served one file at a time
/// machine-wide: [`run_once`]). Each is the one constant [`standin_body`] text,
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
/// THE BUILD RUNS THE INSTANCE IT LINKED ONCE ([`run_once`]'s rule): with no
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
