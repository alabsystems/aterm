// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Cargo test binary's own per-user root, so no test reaches the person's.

use std::ffi::OsStr;
use std::path::Path;
#[cfg(debug_assertions)]
use std::path::PathBuf;

/// Set in the process [`isolated_lifecycle`] starts, to the exact test name it runs.
#[cfg(all(unix, any(test, debug_assertions)))]
const LIFECYCLE_CASE: &str = "ATERM_ISOLATED_LIFECYCLE_CASE";

/// Run a lifecycle body only in its fresh, exact-test owned process. The outer
/// test waits for actual body entry and a successful child exit, then returns
/// false; the child returns true. Stores created afterward cannot be inherited
/// by unrelated parallel tests between fork and exec. No lease retry is involved.
/// Output and process-tree CPU/I/O progress renew the one-minute inactivity
/// detector, so a body doing durable work has no total runtime budget.
/// The body starts with SIGHUP, SIGINT, SIGQUIT and SIGTERM at their default
/// actions, set in its process before it returns true, not the runner's: a
/// test binary run as a `&` job or under `nohup` starts with some ignored, and
/// an exec keeps an ignored signal ignored, so a body whose subject catches only
/// what it was not started ignoring (an armed front) would never hear one.
///
/// # Panics
/// The owned child cannot start, exceeds its hang detector, fails, or does not
/// enter the exact requested case. A missing or renamed test never passes.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn isolated_lifecycle(case: &str) -> bool {
    assert!(
        !case.is_empty() && !case.starts_with('-'),
        "an exact test name is required"
    );
    const CHILD: &str = LIFECYCLE_CASE;
    let marker = format!("entered isolated lifecycle: {case}");
    if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new(case)) {
        for sig in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM] {
            // SAFETY: SIG_DFL is a disposition, no handler; this process runs this body alone.
            unsafe { libc::signal(sig, libc::SIG_DFL) };
        }
        println!("{marker}");
        return true;
    }
    let mut command = crate::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", case, "--test-threads=1", "--nocapture"])
        .env(CHILD, case);
    let output = crate::deadline::output_while_process_progressing(
        &mut command,
        crate::deadline::ECHO_HANG,
        format!("isolated lifecycle {case}"),
    )
    .unwrap_or_else(|error| panic!("isolated lifecycle {case}: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.lines().any(|line| line.ends_with(&marker)),
        "owned lifecycle {case} did not enter and pass: {}\n{}\n{}",
        output.status,
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

/// This test binary's per-user root (module docs), made at the first call.
#[must_use]
pub fn scratch_root() -> Option<&'static Path> {
    #[cfg(debug_assertions)]
    {
        static ROOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            if !is_test_binary(&exe) {
                return None;
            }
            claim(&std::env::temp_dir())
        })
        .as_deref()
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// This test binary's short root, `/tmp/.ubd/aterm-tu-<uid>.noindex/<pid>-<stamp>` on macOS, made at the first
/// call and removed at exit as [`scratch_root`]'s is: a place for a Unix socket whose path must fit
/// `sun_path` (104 bytes on macOS), which a root under the test `TMPDIR` does not. `None` in any
/// other program, and off unix.
#[must_use]
pub fn short_root() -> Option<&'static Path> {
    #[cfg(all(unix, debug_assertions))]
    {
        static ROOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            if !is_test_binary(&exe) {
                return None;
            }
            claim(Path::new("/tmp"))
        })
        .as_deref()
    }
    #[cfg(not(all(unix, debug_assertions)))]
    {
        None
    }
}

/// Whether `exe` is a Cargo test binary: an executable with the name Cargo gives one,
/// `<name>-<16 hex digits>`, in any folder, so that a copy kept aside to compare two builds stays
/// out of the person's folders as the original does. Under any other name it cannot be told from a
/// development program, and it runs on the person's real state, sockets and fleet port. Cargo names
/// an example and a build script the same way, and each of those is a program.
///
/// Linux names a running image that a later build unlinked `<path> (deleted)` (its
/// `/proc/self/exe`, which `current_exe` reads), and that image is still the test binary it was.
#[must_use]
pub fn is_test_binary(exe: &Path) -> bool {
    #[cfg(unix)]
    let exe = {
        use std::os::unix::ffi::OsStrExt as _;
        exe.as_os_str()
            .as_bytes()
            .strip_suffix(b" (deleted)")
            .map_or(exe, |path| Path::new(OsStr::from_bytes(path)))
    };
    let executable = exe.extension().is_none_or(|ext| ext == "exe");
    let named = exe
        .file_stem()
        .and_then(OsStr::to_str)
        .and_then(|stem| stem.rsplit_once('-'))
        .is_some_and(|(name, hash)| {
            !name.is_empty()
                && !name.starts_with("build_script_")
                && hash.len() == 16
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    let example = exe
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "examples");
    executable && named && !example
}

/// Whether `dir` is the login session's runtime directory (`/run/user/<uid>`).
#[must_use]
pub fn is_login_runtime_dir(dir: &OsStr) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `getuid` takes nothing and cannot fail.
        let uid = unsafe { libc::getuid() };
        Path::new(dir) == Path::new("/run/user").join(uid.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// Make this process's root in `temp`'s directory of roots ([`roots_dir`]), after taking
/// back the roots of dead processes there (and any earlier one of this pid's) that no first exec
/// can still be assessed under ([`sweep`]), and arrange its removal at exit. Debug builds only, as
/// [`scratch_root`]'s check is: a shipped build compiles none of this housekeeping.
#[cfg(debug_assertions)]
fn claim(temp: &Path) -> Option<PathBuf> {
    let root = claim_among(
        temp,
        &mut OWN_ROOTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )?;
    sweep_at_exit();
    Some(root)
}

/// Every root this process claimed ([`claim`]), each under its directory of roots' [`canonical`]
/// path: [`scratch_root`]'s and [`short_root`]'s share one where `temp_dir` is `/tmp` (Linux with
/// no `TMPDIR`), so each sweep there must tell both from an earlier owner's of the pid.
#[cfg(debug_assertions)]
static OWN_ROOTS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// [`claim`] with `own`, this process's roots, held by the caller: its sweep leaves each of them
/// alone, and the new root joins them before any other claim's sweep can find it.
#[cfg(debug_assertions)]
fn claim_among(temp: &Path, own: &mut Vec<PathBuf>) -> Option<PathBuf> {
    let roots = roots_dir(temp)?;
    sweep(&roots, own, Pass::Claim, crate::process::awake_ms());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let name = format!("{}-{stamp:x}", std::process::id());
    let root = roots.join(&name);
    make_private(&root).ok()?;
    own.push(canonical(&roots).join(name));
    Some(root)
}

/// `dir` by its canonical path, or as given when it has none: a directory of roots reached
/// through another of its names (`/tmp` is `/private/tmp` on macOS) is still the one directory.
#[cfg(debug_assertions)]
fn canonical(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// `<temp>`'s quiet folder holding `aterm-tu-<uid>.noindex`, made `0700` or adopted when it is already this user's own
/// ([`adoptable`]); `None` when it is not, or cannot be made.
#[cfg(debug_assertions)]
fn roots_dir(temp: &Path) -> Option<PathBuf> {
    let temp = aterm_tempfile::quiet_in(temp);
    #[cfg(unix)]
    // SAFETY: `getuid` takes nothing and cannot fail.
    let me = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let me = 0;
    let roots = temp.join(format!("aterm-tu-{me}.noindex"));
    match make_private(&roots) {
        Ok(()) => Some(roots),
        Err(made) if made.kind() == std::io::ErrorKind::AlreadyExists => {
            adoptable(&std::fs::symlink_metadata(&roots).ok()?, me).then_some(roots)
        }
        Err(_) => None,
    }
}

/// Whether `found` (an `lstat`) is a directory of roots user `me` may adopt: a real
/// directory, not a link, that `me` owns and no one else can write.
#[cfg(debug_assertions)]
fn adoptable(found: &std::fs::Metadata, me: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        found.is_dir() && found.uid() == me && found.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = me;
        found.is_dir()
    }
}

/// `dir` made, `0700` on unix: the control directory under a root must be private. A
/// name already taken (in a shared `/tmp`, perhaps another user's directory or link)
/// is an error here, never used.
#[cfg(debug_assertions)]
fn make_private(dir: &Path) -> std::io::Result<()> {
    let builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        let mut builder = builder;
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
    };
    builder.create(dir)
}

/// Whether a program a test copied may still be in its first exec under this process's root: a
/// [`crate::deadline::KeptUntilRan`] was dropped before its children showed they ran
/// ([`hold_own_root`]), so the root's sweep at exit leaves it under custody ([`sweep`]).
#[cfg(any(test, debug_assertions))]
static OWN_ROOT_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Keep this process's root under first-exec custody at its exit ([`OWN_ROOT_HELD`]).
#[cfg(any(test, debug_assertions))]
pub(crate) fn hold_own_root() {
    OWN_ROOT_HELD.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Which sweep runs: a claim's while this process lives, or the one at its exit.
#[cfg(debug_assertions)]
#[derive(Clone, Copy)]
enum Pass {
    /// [`claim`]'s: this process's own roots are live, so it leaves them alone.
    Claim,
    /// [`sweep_at_exit`]'s, `held` when a copy kept unproven holds this process's roots
    /// ([`hold_own_root`]).
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "no exit hook runs a sweep off unix")
    )]
    Exit {
        /// [`OWN_ROOT_HELD`].
        held: bool,
    },
}

/// Take back the roots directly under `roots` that no live process owns. `own` names this
/// process's roots ([`OWN_ROOTS`]): a [`Pass::Claim`] sweep never touches or stamps one, and the
/// sweep at exit removes each at once unless it is held (a test kept a copy unproven,
/// [`hold_own_root`]), when it keeps it under first-exec custody. Any other root whose pid is this
/// process's (an earlier owner of the pid) or names no live process is kept under that custody
/// too. Its owner may have died (a SIGKILL, a halted run) while macOS still assessed the first
/// exec of a program a test copied under it, and unlinking that file mid-assessment crashes
/// syspolicyd. So the sweep that finds such a root stamps it
/// ([`crate::deadline::try_stamp_exec`]): every first exec under it began before its owner died,
/// so before that stamp. A later sweep removes it once [`crate::deadline::FIRST_EXEC_HANG`] of
/// awake time (`awake_now`) has passed since ([`crate::deadline::copy_wait_left`]); where no
/// awake clock reads, of wall time since the blank stamp made the root's own modification time. A
/// root whose stamp cannot be written or read where the clock reads (a full disk, an `exec` that
/// is no regular file) stays until one can. Best-effort: a test run never fails over housekeeping.
#[cfg(debug_assertions)]
fn sweep(roots: &Path, own: &[PathBuf], pass: Pass, awake_now: Option<u64>) {
    let Ok(entries) = std::fs::read_dir(roots) else {
        return;
    };
    let dir = canonical(roots);
    let me = std::process::id();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(owner_pid) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if own.contains(&dir.join(entry.file_name())) {
            match pass {
                Pass::Claim => {}
                Pass::Exit { held: false } => {
                    let _ = std::fs::remove_dir_all(&path);
                }
                Pass::Exit { held: true } => custody(&path, awake_now),
            }
        } else if pid == me || !crate::process::pid_alive(pid) {
            custody(&path, awake_now);
        }
    }
}

/// [`sweep`]'s first-exec custody of `root`: stamp it when it holds no stamp, else remove it once
/// [`crate::deadline::copy_wait_left_at`] lets it go.
#[cfg(debug_assertions)]
fn custody(root: &Path, awake_now: Option<u64>) {
    let stamped = crate::deadline::exec_stamp(root).is_some()
        || awake_now.is_none()
            && std::fs::symlink_metadata(root.join(crate::deadline::EXEC_STAMP)).is_ok();
    if !stamped {
        let _ = crate::deadline::try_stamp_exec(root);
    } else if crate::deadline::copy_wait_left_at(root, std::time::SystemTime::now(), awake_now)
        .is_none()
    {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// The owner pid in a root's name: the pid, then `-` and the stamp.
#[cfg(debug_assertions)]
fn owner_pid(name: &str) -> Option<u32> {
    let (pid, _stamp) = name.split_once('-')?;
    (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        .then(|| pid.parse().ok())
        .flatten()
}

/// Arrange, once per process, that `exit(3)` runs [`sweep_own`] over [`OWN_ROOTS`] (every root
/// [`scratch_root`] and [`short_root`] made), preserving first-exec custody for each root.
#[cfg(all(unix, debug_assertions))]
fn sweep_at_exit() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    extern "C" fn at_exit() {
        let own = OWN_ROOTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = OWN_ROOT_HELD.load(std::sync::atomic::Ordering::SeqCst);
        sweep_own(&own, held, crate::process::awake_ms());
    }
    unsafe extern "C" {
        fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
    }
    REGISTERED.call_once(|| {
        // SAFETY: registers a plain `extern "C"` function with no captured state; a failed
        // registration only leaves the roots to the next test binary's first-use sweep.
        let _ = unsafe { atexit(at_exit) };
    });
}

/// The sweep at exit: [`sweep`] once over each directory of roots that holds one of `own` (each
/// under its [`canonical`] directory, as [`OWN_ROOTS`] keeps them), with every one of `own` known
/// there as this process's, `held` or not.
#[cfg(all(unix, debug_assertions))]
fn sweep_own(own: &[PathBuf], held: bool, awake_now: Option<u64>) {
    let mut swept: Vec<&Path> = Vec::new();
    for roots in own.iter().filter_map(|root| root.parent()) {
        if !swept.contains(&roots) {
            swept.push(roots);
            sweep(roots, own, Pass::Exit { held }, awake_now);
        }
    }
}

/// No exit hook off unix: the next test binary's first-use sweep takes the roots back.
#[cfg(all(not(unix), debug_assertions))]
fn sweep_at_exit() {}

#[cfg(all(test, debug_assertions))]
#[path = "test_binary_tests.rs"]
mod tests;
