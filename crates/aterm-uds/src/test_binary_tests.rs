// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::{Pass, adoptable, canonical, claim_among, roots_dir, sweep, sweep_own};
use super::{is_test_binary, owner_pid, scratch_root};
#[cfg(unix)]
use crate::deadline::{EXEC_STAMP, FIRST_EXEC_HANG, RAN, exec_stamp};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// AN ISOLATED BODY STARTS WITH THE SIGNALS THAT END A PROCESS AT THEIR DEFAULTS, whatever
/// its process had: the body's process ignores the four first, as one a `nohup` `&` runner
/// started would, and the body reads each at its default once [`super::isolated_lifecycle`]
/// says it runs. Only the body's process moves them; this binary's own stay as they were.
/// NEGATIVE CONTROL (by hand: the reset in `isolated_lifecycle` removed): the body reads
/// SIGHUP ignored, and its outer test fails on the body's failed run.
#[cfg(unix)]
#[test]
fn an_isolated_body_starts_with_the_signals_that_end_a_process_at_their_defaults() {
    const CASE: &str = "test_binary::tests::an_isolated_body_starts_with_the_signals_that_end_a_process_at_their_defaults";
    const TERMINATING: [libc::c_int; 4] =
        [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];
    if std::env::var_os(super::LIFECYCLE_CASE).as_deref() == Some(std::ffi::OsStr::new(CASE)) {
        for sig in TERMINATING {
            // SAFETY: SIG_IGN is a disposition, no handler; this process runs this body alone.
            unsafe { libc::signal(sig, libc::SIG_IGN) };
        }
    }
    if !super::isolated_lifecycle(CASE) {
        return;
    }
    for sig in TERMINATING {
        // SAFETY: all-zero is a valid `sigaction`; a null new action only reads the current
        // one into `now`, which outlives the call.
        let now = unsafe {
            let mut now: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(sig, std::ptr::null(), &raw mut now), 0);
            now.sa_sigaction
        };
        assert_eq!(
            now,
            libc::SIG_DFL,
            "signal {sig} at its default in the body"
        );
    }
}

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
    assert_eq!(
        roots.parent(),
        Some(aterm_tempfile::quiet_in(&std::env::temp_dir()).as_path())
    );
    assert_eq!(
        roots.extension().and_then(|ext| ext.to_str()),
        Some("noindex")
    );
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
    canonical(root.parent().expect("a directory of roots")).join(root.file_name().expect("a name"))
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
        let roots = temp.join(format!("aterm-tu-{me}.noindex"));
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
