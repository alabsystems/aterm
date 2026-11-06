// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Cargo test binary's own per-user root, so no test reaches the person's.

use std::ffi::OsStr;
use std::path::Path;
#[cfg(debug_assertions)]
use std::path::PathBuf;

/// Run a lifecycle body only in its fresh, exact-test owned process. The outer
/// test waits for actual body entry and a successful child exit, then returns
/// false; the child returns true. Stores created afterward cannot be inherited
/// by unrelated parallel tests between fork and exec. No lease retry is involved.
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
    const CHILD: &str = "ATERM_ISOLATED_LIFECYCLE_CASE";
    let marker = format!("entered isolated lifecycle: {case}");
    if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new(case)) {
        println!("{marker}");
        return true;
    }
    let mut command = crate::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", case, "--test-threads=1", "--nocapture"])
        .env(CHILD, case);
    let output = crate::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
        .expect("the owned lifecycle process finishes");
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

/// This test binary's short root, `/tmp/aterm-tu-<uid>/<pid>-<stamp>`, made at the first call
/// and removed at exit as [`scratch_root`]'s is: a place for a Unix socket whose path must fit
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

/// `<temp>/aterm-tu-<uid>`, made `0700` or adopted when it is already this user's own
/// ([`adoptable`]); `None` when it is not, or cannot be made.
#[cfg(debug_assertions)]
fn roots_dir(temp: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    // SAFETY: `getuid` takes nothing and cannot fail.
    let me = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let me = 0;
    let roots = temp.join(format!("aterm-tu-{me}"));
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
mod tests {
    #[cfg(unix)]
    use super::{Pass, adoptable, canonical, claim_among, roots_dir, sweep, sweep_own};
    use super::{is_test_binary, owner_pid, scratch_root};
    #[cfg(unix)]
    use crate::deadline::{EXEC_STAMP, FIRST_EXEC_HANG, RAN, exec_stamp};
    use std::path::Path;
    #[cfg(unix)]
    use std::path::PathBuf;

    #[test]
    fn only_a_cargo_test_binary_is_one() {
        for exe in [
            "/t/debug/deps/aterm_gui-ed1c215a8f696a87",
            "/t/debug/deps/aterm-4e7f22a66ff5f4f6",
            r"C:\t\debug\deps\aterm_uds-0123456789abcdef.exe",
            "/t/debug/build/aterm-types/0123456789abcdef/out/aterm_types-0123456789abcdef",
            "/t/opt-test/build/package/0123456789abcdef/out/contract-0123456789abcdef",
            r"C:\t\debug\build\aterm-uds\0123456789abcdef\out\aterm_uds-0123456789abcdef.exe",
            // A copy kept aside to compare two builds is as isolated as the original.
            "/scratch/base/aterm_gui-ed1c215a8f696a87",
            r"C:\scratch\base\aterm_gui-ed1c215a8f696a87.exe",
        ] {
            let exe = exe.replace('\\', std::path::MAIN_SEPARATOR_STR);
            assert!(is_test_binary(Path::new(&exe)), "{exe}");
        }
        for exe in [
            "/t/debug/aterm",
            "/t/debug/aterm (deleted)",
            "/t/debug/examples/demo-ed1c215a8f696a87",
            "/t/debug/build/aterm-gui-ed1c215a8f696a87/build-script-build",
            "/t/debug/build/aterm-gui-ed1c215a8f696a87/build_script_build-ed1c215a8f696a87",
            "/t/debug/deps/aterm_gui-ED1C215A8F696A87",
            "/t/debug/deps/aterm_gui-ed1c215a8f696a8",
            "/t/debug/deps/-ed1c215a8f696a87",
            "/t/debug/deps/libaterm_uds-ed1c215a8f696a87.rlib",
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "/t/debug/build/pkg/0123456789abcdef/out/unit-0123456789abcdef.rlib",
            "/t/debug/build/pkg/0123456789abcdef/out/build-script-build",
            // The gap this name leaves: a copy renamed without its hash reads as a program.
            "/scratch/base/aterm_gui",
        ] {
            assert!(!is_test_binary(Path::new(exe)), "{exe}");
        }
        // Linux's name for a running image a later build unlinked, in a folder whose name is
        // no UTF-8 too.
        #[cfg(unix)]
        for exe in [
            &b"/t/debug/deps/aterm_agent-0123456789abcdef (deleted)"[..],
            b"/t/\xff/debug/deps/aterm_agent-0123456789abcdef (deleted)",
        ] {
            use std::os::unix::ffi::OsStrExt as _;
            let exe = Path::new(std::ffi::OsStr::from_bytes(exe));
            assert!(is_test_binary(exe), "{}", exe.display());
        }
    }

    #[test]
    fn a_test_binary_gets_a_private_root_of_its_own() {
        let root = scratch_root().expect("this test binary's root");
        let roots = root.parent().expect("a directory of roots");
        assert_eq!(roots.parent(), Some(std::env::temp_dir().as_path()));
        let name = root.file_name().and_then(|n| n.to_str()).expect("a name");
        assert_eq!(owner_pid(name), Some(std::process::id()));
        assert!(root.is_dir(), "made at first use");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |dir| std::fs::metadata(dir).expect("stat").permissions().mode();
            assert_eq!(mode(root) & 0o777, 0o700, "{:o}", mode(root));
            assert_eq!(mode(roots) & 0o022, 0, "{:o}", mode(roots));
        }
        assert_eq!(scratch_root(), Some(root), "one root per process");
    }

    /// The awake-clock reading `hang` after `at`.
    #[cfg(unix)]
    fn past(at: u64, hang: std::time::Duration) -> Option<u64> {
        Some(at + u64::try_from(hang.as_millis()).expect("a hang in milliseconds"))
    }

    /// The stamp a sweep left in `root` when it found it.
    #[cfg(unix)]
    fn stamp(root: &Path) -> u64 {
        exec_stamp(root).expect("stamped when the sweep found it")
    }

    /// `root` as [`super::OWN_ROOTS`] names one of this process's roots.
    #[cfg(unix)]
    fn known(root: &Path) -> PathBuf {
        canonical(root.parent().expect("a directory of roots"))
            .join(root.file_name().expect("a name"))
    }

    /// A sweep takes back this process's own root at once, and a dead process's root and an
    /// earlier one of this pid's only once [`FIRST_EXEC_HANG`] has passed since it found them; it
    /// leaves a live process's and anything not named as a root. CONTROL: a sweep that took a dead
    /// owner's root at once fails the first check.
    #[cfg(unix)]
    #[test]
    fn a_sweep_takes_back_only_what_no_live_process_owns_or_may_still_assess() {
        let temp = aterm_tempfile::TempDir::new().expect("a temp dir");
        let temp = temp.path();
        let me = std::process::id();
        let parent = std::os::unix::process::parent_id();
        let dir = |name: String| {
            let path = temp.join(name);
            std::fs::create_dir(&path).expect("a root");
            path
        };
        let dead = dir("0-1".to_owned());
        let mine = dir(format!("{me}-1"));
        let own = dir(format!("{me}-2"));
        let live = dir(format!("{parent}-1"));
        let signed = dir("+0-1".to_owned());
        let other = dir("x".to_owned());
        let awake = crate::process::awake_ms();
        sweep(temp, &[known(&own)], Pass::Exit { held: false }, awake);
        assert!(
            dead.exists(),
            "a dead process's root stays while a first exec may be assessed"
        );
        assert!(mine.exists(), "and so does an earlier root of this pid");
        assert!(!own.exists(), "this process's own root goes at its exit");
        // Each root is stamped as the sweep finds it, so the two may differ by a millisecond.
        let (first, last) = (
            stamp(&dead).min(stamp(&mine)),
            stamp(&dead).max(stamp(&mine)),
        );
        assert!(
            Some(first) >= awake && Some(last) <= crate::process::awake_ms(),
            "stamped when found"
        );
        sweep(
            temp,
            &[],
            Pass::Claim,
            past(first, FIRST_EXEC_HANG - std::time::Duration::from_millis(1)),
        );
        assert!(dead.exists() && mine.exists(), "until the hang has passed");
        sweep(temp, &[], Pass::Claim, past(last, FIRST_EXEC_HANG));
        assert!(!dead.exists(), "then a dead process's root goes");
        assert!(!mine.exists(), "and an earlier root of this pid");
        assert!(live.exists(), "a live process's root stays");
        assert!(exec_stamp(&live).is_none(), "unstamped");
        assert!(signed.exists(), "a signed prefix is no root");
        assert!(other.exists(), "a name that is no root stays");
    }

    /// A test binary the run killed (a halt, a SIGKILL) may have started a program it copied
    /// under its root that never showed it ran, so macOS may still assess its first exec: the
    /// next sweep leaves the copy in place, and a later one takes it once the hang has passed. The
    /// copy goes only after a child showed it ran from it.
    #[cfg(unix)]
    #[test]
    fn a_copy_whose_child_died_before_its_proof_outlives_its_owners_sweep() {
        use crate::spawnfd::{Stdio, command};
        let mut kept = crate::deadline::KeptUntilRan::new(
            aterm_tempfile::TempDir::new().expect("a temp dir").keep(),
        );
        let mut gone = command("/usr/bin/true")
            .spawn()
            .expect("spawn a short life");
        let owner = gone.id();
        assert!(gone.wait().expect("reap it").success());
        let root = kept.path().join(format!("{owner}-1"));
        std::fs::create_dir(&root).expect("a dead owner's root");
        let copy = root.join("cat");
        crate::deadline::ad_hoc_copy(Path::new("/bin/cat"), &copy);
        let spawn = || {
            command(&copy)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn the copy")
        };
        let mut killed = spawn();
        crate::deadline::kill_within(&mut killed, crate::deadline::ECHO_HANG);
        sweep(kept.path(), &[], Pass::Claim, crate::process::awake_ms());
        assert!(
            copy.exists(),
            "the copy stays while its first exec may be assessed"
        );
        let mut proven = spawn();
        drop(crate::deadline::cat_echoes(&mut proven));
        assert!(proven.wait().expect("reap the proof").success());
        kept.ran();
        sweep(
            kept.path(),
            &[],
            Pass::Claim,
            past(stamp(&root), FIRST_EXEC_HANG),
        );
        assert!(!root.exists(), "it goes once the hang has passed");
    }

    /// A test that kept a copy unproven ([`crate::deadline::KeptUntilRan`] dropped before its
    /// children showed they ran) keeps its process's own root under custody at exit, stamped like
    /// a dead owner's; with none kept the root goes at once. CONTROL: the second case.
    #[cfg(unix)]
    #[test]
    fn an_own_root_holding_an_unproven_copy_stays_at_exit() {
        let temp = aterm_tempfile::TempDir::new().expect("a temp dir");
        let me = std::process::id();
        let own = temp.path().join(format!("{me}-7"));
        std::fs::create_dir(&own).expect("this process's root");
        let held = Pass::Exit { held: true };
        sweep(
            temp.path(),
            &[known(&own)],
            held,
            crate::process::awake_ms(),
        );
        assert!(own.exists(), "held: kept");
        let at = stamp(&own);
        sweep(temp.path(), &[known(&own)], held, past(at, FIRST_EXEC_HANG));
        assert!(!own.exists(), "held: it goes once the hang has passed");
        std::fs::create_dir(&own).expect("this process's root again");
        sweep(
            temp.path(),
            &[known(&own)],
            Pass::Exit { held: false },
            crate::process::awake_ms(),
        );
        assert!(!own.exists(), "not held: it goes at once");
    }

    /// A temp directory, `real`, and `alias`, another name for it (a link to it), as `/tmp` is
    /// `/private/tmp` on macOS; the first value keeps both until it drops.
    #[cfg(unix)]
    fn shared_temp() -> (aterm_tempfile::TempDir, PathBuf, PathBuf) {
        let scratch = aterm_tempfile::TempDir::new().expect("a temp dir");
        let real = scratch.path().join("tmp");
        std::fs::create_dir(&real).expect("a temp directory");
        let alias = scratch.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("another name for it");
        (scratch, real, alias)
    }

    /// Where `temp_dir` is `/tmp` (Linux with no `TMPDIR`), [`super::scratch_root`]'s and
    /// [`super::short_root`]'s roots share one directory of roots, which a claim may also reach
    /// through another of its names (`/private/tmp` on macOS). A claim's sweep there never stamps
    /// or takes a root this process still owns, even one holding what would let a dead owner's go
    /// at once (a RAN marker beside a stamp), while it stamps an earlier owner's root of this pid
    /// as ever. CONTROL: a claim's sweep that took this process's roots for an earlier owner's
    /// stamps the first root at the second claim.
    #[cfg(unix)]
    #[test]
    fn a_claim_leaves_this_process_s_roots_alone_where_they_share_a_directory() {
        let (_scratch, real, alias) = shared_temp();
        let mut own = Vec::new();
        let first = claim_among(&real, &mut own).expect("a root");
        let roots = first.parent().expect("a directory of roots").to_path_buf();
        let earlier = roots.join(format!("{}-earlier", std::process::id()));
        std::fs::create_dir(&earlier).expect("an earlier owner's root of this pid");
        let second = claim_among(&real, &mut own).expect("a second root in that directory");
        assert_eq!(
            second.parent(),
            Some(roots.as_path()),
            "one directory of roots"
        );
        assert!(
            exec_stamp(&earlier).is_some(),
            "an earlier owner's root of this pid is stamped"
        );
        assert!(
            std::fs::symlink_metadata(first.join(EXEC_STAMP)).is_err(),
            "this process's first root is not"
        );
        std::fs::write(first.join(EXEC_STAMP), "0").expect("a stamp long past");
        std::fs::write(first.join(RAN), "").expect("a RAN marker");
        let third = claim_among(&alias, &mut own).expect("a root through another name");
        assert!(first.is_dir(), "a live root goes at no claim's sweep");
        assert!(
            std::fs::symlink_metadata(second.join(EXEC_STAMP)).is_err(),
            "nor is one stamped through another name of its directory"
        );
        assert!(third.is_dir(), "the third root is made");
    }

    /// Set, the test binary is [`standin_claims_two_roots`], claiming in this directory.
    #[cfg(unix)]
    const CLAIM_STANDIN: &str = "ATERM_UDS_CLAIM_STANDIN";

    /// Run by [`a_test_binary_takes_back_every_root_it_claimed_at_its_exit`] in a process of its
    /// own: two real claims ([`super::claim`]) in one directory, as `scratch_root` and `short_root`
    /// make where `temp_dir` is `/tmp`. It says each root and whether the second claim's sweep left
    /// the first unstamped, then ends as a test binary does, through `exit(3)` and its hook.
    #[cfg(unix)]
    #[test]
    fn standin_claims_two_roots() {
        let Some(temp) = std::env::var_os(CLAIM_STANDIN).map(PathBuf::from) else {
            return;
        };
        let first = super::claim(&temp).expect("a root");
        let second = super::claim(&temp).expect("a second root in that directory");
        let unstamped = std::fs::symlink_metadata(first.join(EXEC_STAMP)).is_err();
        println!("\nroot={}", first.display());
        println!("root={}", second.display());
        println!("first-unstamped={unstamped}");
    }

    /// A test binary's real claims and its exit hook, end to end: two claims in one directory of
    /// roots leave each other alone while the process lives, and the process's exit takes both
    /// back at once. This is the wiring the tests beside it hold through [`claim_among`] and
    /// [`sweep_own`]: [`super::claim`] recording each root in [`super::OWN_ROOTS`] and the hook
    /// handing all of them to the sweep. CONTROL: a claim that records into a list of its own
    /// stamps the first root at the second claim and leaves both at exit; a hook that hands the
    /// sweep only the first root leaves the second.
    #[cfg(unix)]
    #[test]
    fn a_test_binary_takes_back_every_root_it_claimed_at_its_exit() {
        let scratch = aterm_tempfile::TempDir::new().expect("a temp dir");
        let out = crate::deadline::ends_within(
            crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
                .args([
                    "test_binary::tests::standin_claims_two_roots",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CLAIM_STANDIN, scratch.path()),
            crate::deadline::ECHO_HANG,
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{out:?}");
        let roots: Vec<PathBuf> = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("root="))
            .map(PathBuf::from)
            .collect();
        assert_eq!(roots.len(), 2, "the child said both roots: {stdout}");
        assert!(
            stdout.lines().any(|line| line == "first-unstamped=true"),
            "the second claim's sweep left the first root alone: {stdout}"
        );
        for root in &roots {
            assert!(
                !root.exists(),
                "{} taken back at the exit: {stdout}",
                root.display()
            );
        }
    }

    /// The sweep at exit takes back at once every root this process made: two that share a
    /// directory of roots, one reached through another name of that directory and one in a
    /// directory of its own (macOS's test `TMPDIR` beside `/tmp`), so a short root beside the
    /// scratch root is not left stamped under custody for [`FIRST_EXEC_HANG`]. CONTROL: a sweep
    /// at exit that knew only the first own root of each directory keeps the second.
    #[cfg(unix)]
    #[test]
    fn the_sweep_at_exit_takes_every_root_this_process_made_at_once() {
        let (scratch, real, alias) = shared_temp();
        let apart = scratch.path().join("apart");
        std::fs::create_dir(&apart).expect("a temp directory of its own");
        let mut own = Vec::new();
        let made: Vec<PathBuf> = [&real, &real, &alias, &apart]
            .into_iter()
            .map(|temp| claim_among(temp, &mut own).expect("a root"))
            .collect();
        sweep_own(&own, false, crate::process::awake_ms());
        for root in &made {
            assert!(!root.exists(), "taken back at once: {}", root.display());
        }
    }

    /// A held exit keeps this process's roots that share a directory under a custody that begins
    /// at exit: an exit [`FIRST_EXEC_HANG`] after the second claim keeps both, and a later test
    /// binary's claim (for which this pid names an earlier owner) takes them once the hang has
    /// passed since. CONTROL: a claim's sweep that stamped the live first root would start its
    /// custody at the second claim, so this exit would take it while a first exec under it may
    /// still be assessed.
    #[cfg(unix)]
    #[test]
    fn a_held_exit_keeps_roots_that_share_a_directory_for_the_hang_from_exit() {
        let (_scratch, real, _alias) = shared_temp();
        let mut own = Vec::new();
        let first = claim_among(&real, &mut own).expect("a root");
        let second = claim_among(&real, &mut own).expect("a second root in that directory");
        let exit = past(
            crate::process::awake_ms().expect("an awake clock"),
            FIRST_EXEC_HANG,
        );
        sweep_own(&own, true, exit);
        assert!(first.is_dir() && second.is_dir(), "held: both kept at exit");
        let (earliest, latest) = (
            stamp(&first).min(stamp(&second)),
            stamp(&first).max(stamp(&second)),
        );
        let roots = first.parent().expect("a directory of roots");
        let short = FIRST_EXEC_HANG - std::time::Duration::from_millis(1);
        sweep(roots, &[], Pass::Claim, past(earliest, short));
        assert!(
            first.is_dir() && second.is_dir(),
            "until the hang has passed"
        );
        sweep(roots, &[], Pass::Claim, past(latest, FIRST_EXEC_HANG));
        assert!(
            !first.exists() && !second.exists(),
            "then a later claim takes both"
        );
    }

    /// A dropped [`crate::deadline::KeptUntilRan`] that never said its children ran holds this
    /// process's root ([`super::OWN_ROOT_HELD`]) for its sweep at exit.
    #[cfg(unix)]
    #[test]
    fn an_unproven_copy_holds_the_own_root() {
        let dir = aterm_tempfile::TempDir::new().expect("a temp dir").keep();
        let kept = crate::deadline::KeptUntilRan::new(&dir);
        let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(kept)));
        assert!(
            dropped.is_err(),
            "an unproven drop outside a panic is the test's failure"
        );
        assert!(super::OWN_ROOT_HELD.load(std::sync::atomic::Ordering::SeqCst));
        assert!(dir.exists(), "its directory stays");
        std::fs::remove_dir(&dir).expect("nothing was copied or run there");
    }

    /// The directory of roots is made `0700`, then adopted while it stays this user's
    /// and no one else can write it; a wider one, a file, a link (even to a private
    /// directory) and another user's are refused, so no other user's root is swept.
    #[cfg(unix)]
    #[test]
    fn only_this_users_own_directory_of_roots_is_adopted() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::TempDir::new().expect("a temp dir");
        // SAFETY: `getuid` takes nothing and cannot fail.
        let me = unsafe { libc::getuid() };
        let case = |name: &str| {
            let temp = scratch.path().join(name);
            std::fs::create_dir(&temp).expect("a temp dir");
            let roots = temp.join(format!("aterm-tu-{me}"));
            (temp, roots)
        };
        let chmod = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };

        let (temp, roots) = case("made");
        assert_eq!(roots_dir(&temp).as_deref(), Some(roots.as_path()), "made");
        let made = std::fs::symlink_metadata(&roots).expect("lstat");
        assert_eq!(made.permissions().mode() & 0o777, 0o700);
        assert!(!adoptable(&made, me.wrapping_add(1)), "another user's");
        for (mode, adopted) in [(0o700, true), (0o755, true), (0o770, false), (0o707, false)] {
            chmod(&roots, mode);
            assert_eq!(roots_dir(&temp).is_some(), adopted, "{mode:o}");
        }

        let (temp, roots) = case("file");
        std::fs::write(&roots, "").expect("a file");
        chmod(&roots, 0o600);
        assert_eq!(roots_dir(&temp), None, "a file");

        let (temp, roots) = case("link");
        let private = temp.join("private");
        std::fs::create_dir(&private).expect("a directory");
        chmod(&private, 0o700);
        std::os::unix::fs::symlink(&private, &roots).expect("a link");
        assert_eq!(roots_dir(&temp), None, "a link");
    }
}
