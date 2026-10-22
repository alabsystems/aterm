// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Cargo test binary's own per-user root, so no test reaches the person's.
//!
//! A test that resolves a per-user path without naming one got the person's: on
//! 2026-10-18 an aterm-gui unit test read the live window's token and dialled its
//! control socket four times a run (the window logged each as a refused
//! connection), and the same suite made directories in the person's state, log and
//! config folders and read their `aterm.toml` and themes. So in a test binary every
//! per-user resolver — [`crate::control_socket_dir`] here, and
//! `aterm_types::dirs`' state, data, log and config folders — answers under
//! [`scratch_root`] instead, unless the test named its own (`$XDG_RUNTIME_DIR`,
//! `$ATERM_STATE_HOME`, `$XDG_CONFIG_HOME`).
//!
//! A test binary is what Cargo builds for `cargo test`: an executable named
//! `<crate>-<16 hex digits>` in `<target>/<profile>/deps/` ([`is_test_binary`]), in a
//! build with debug assertions. Nothing else is: the `aterm` an integration test
//! runs is its uplifted copy, `<target>/<profile>/aterm`, which a test isolates with
//! its own environment, and a build without debug assertions (every shipped one)
//! compiles the check out.
//!
//! The root is `<temp>/aterm-tu-<uid>/<pid>-<stamp>`, made `0700` at first use; every
//! root sits in that one private per-user directory, so the sweeps list roots and never
//! the temp dir. At first use every root of a dead process goes, and at exit (`exit(3)`,
//! unix) this process's own and every dead one's. A root is taken back only when its
//! owner is provably gone: its pid is this process's at first use (its own root is not
//! made yet) or at exit, or no live process has it. A forked child that exits leaves its
//! parent's.

use std::ffi::OsStr;
use std::path::Path;
#[cfg(debug_assertions)]
use std::path::PathBuf;

/// This test binary's per-user root (module docs), made at the first call; `None` in
/// any other process, and when the root cannot be made or its directory is not this
/// user's own (the resolvers then answer the person's paths, as before).
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

/// Whether `exe` is a Cargo test binary: `deps/<name>-<16 lowercase hex digits>`
/// (`.exe` on Windows). A pure reading of the path.
#[must_use]
pub fn is_test_binary(exe: &Path) -> bool {
    let in_deps = exe
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|dir| dir == "deps");
    let executable = exe.extension().is_none_or(|ext| ext == "exe");
    let named = exe
        .file_stem()
        .and_then(OsStr::to_str)
        .and_then(|stem| stem.rsplit_once('-'))
        .is_some_and(|(name, hash)| {
            !name.is_empty()
                && hash.len() == 16
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    in_deps && executable && named
}

/// Whether `dir` is the login session's runtime directory, `/run/user/<uid>`
/// (systemd's `pam_systemd` exports it to every process of a desktop session): an
/// inherited `$XDG_RUNTIME_DIR` naming the person's own, never one a test chose.
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
/// back the roots of dead processes there (and any earlier one of this pid's), and
/// arrange its removal at exit. Debug builds only, as [`scratch_root`]'s check is: a
/// shipped build compiles none of this housekeeping.
#[cfg(debug_assertions)]
fn claim(temp: &Path) -> Option<PathBuf> {
    let roots = roots_dir(temp)?;
    sweep(&roots);
    // The stamp makes the name unique per process instance: a recycled pid never
    // lands in a dead run's root even where a sweep failed part-way. Short, as the
    // whole path is: a socket path under it must stay inside `sun_path`'s 104 bytes
    // (`$TMPDIR` is 49 on macOS, `aterm-tu-<uid>/` 13 more, `/run/aterm-<pid>.sock`
    // up to 22 more).
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let root = roots.join(format!("{}-{stamp:x}", std::process::id()));
    make_private(&root).ok()?;
    sweep_at_exit(&roots);
    Some(root)
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

/// Remove every root directly under `roots` whose pid is this process's or names no
/// live process. Best-effort: a test run never fails over housekeeping.
#[cfg(debug_assertions)]
fn sweep(roots: &Path) {
    let Ok(entries) = std::fs::read_dir(roots) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(owner_pid) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if pid == me || !crate::process::pid_alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The owner pid in a root's name: the pid, then `-` and the stamp.
#[cfg(debug_assertions)]
fn owner_pid(name: &str) -> Option<u32> {
    name.split_once('-')?.0.parse().ok()
}

/// Run [`sweep`] over `roots` inside `exit(3)`.
#[cfg(all(unix, debug_assertions))]
fn sweep_at_exit(roots: &Path) {
    static ROOTS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    if ROOTS.set(roots.to_path_buf()).is_err() {
        return;
    }
    extern "C" fn at_exit() {
        if let Some(roots) = ROOTS.get() {
            sweep(roots);
        }
    }
    unsafe extern "C" {
        /// `stdlib.h`, declared inline as `aterm-gui`'s process scratch declares it.
        fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
    }
    // SAFETY: registers a plain `extern "C"` function with no captured state; a failed
    // registration only leaves the root to the next test binary's first-use sweep.
    unsafe { atexit(at_exit) };
}

/// No exit hook off unix: the next test binary's first-use sweep takes the root back.
#[cfg(all(not(unix), debug_assertions))]
fn sweep_at_exit(_roots: &Path) {}

// Debug builds only, as the root itself is: a release test build has no root.
#[cfg(all(test, debug_assertions))]
mod tests {
    #[cfg(unix)]
    use super::{adoptable, roots_dir, sweep};
    use super::{is_test_binary, owner_pid, scratch_root};
    use std::path::Path;

    /// Cargo's test binaries are `deps/<crate>-<16 hex>`; the uplifted `aterm` an
    /// integration test runs, a build script, an example and a shipped app are not.
    #[test]
    fn only_a_cargo_test_binary_is_one() {
        for exe in [
            "/t/debug/deps/aterm_gui-ed1c215a8f696a87",
            "/t/debug/deps/aterm-4e7f22a66ff5f4f6",
            r"C:\t\debug\deps\aterm_uds-0123456789abcdef.exe",
        ] {
            let exe = exe.replace('\\', std::path::MAIN_SEPARATOR_STR);
            assert!(is_test_binary(Path::new(&exe)), "{exe}");
        }
        for exe in [
            "/t/debug/aterm",
            "/t/debug/examples/demo-ed1c215a8f696a87",
            "/t/debug/build/aterm-gui-ed1c215a8f696a87/build-script-build",
            "/t/debug/deps/aterm_gui-ED1C215A8F696A87",
            "/t/debug/deps/aterm_gui-ed1c215a8f696a8",
            "/t/debug/deps/-ed1c215a8f696a87",
            "/t/debug/deps/libaterm_uds-ed1c215a8f696a87.rlib",
            "/Applications/aterm.app/Contents/MacOS/aterm",
        ] {
            assert!(!is_test_binary(Path::new(exe)), "{exe}");
        }
    }

    /// This binary is a test binary, so it has a root: private, in the temp dir's
    /// private directory of roots, named for this process. Negative control: a process
    /// that is not one has none ([`only_a_cargo_test_binary_is_one`]'s refusals are what
    /// `scratch_root` reads).
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

    /// A first use takes back a dead process's root and an earlier one of this pid's,
    /// and leaves a live process's and anything not named as a root.
    #[cfg(unix)]
    #[test]
    fn a_first_use_takes_back_only_what_no_live_process_owns() {
        let temp = aterm_tempfile::TempDir::new().expect("a temp dir");
        let temp = temp.path();
        let me = std::process::id();
        // The parent of this test process outlives it; pid 0 is never a process.
        let parent = std::os::unix::process::parent_id();
        let dir = |name: String| {
            let path = temp.join(name);
            std::fs::create_dir(&path).expect("a root");
            path
        };
        let dead = dir("0-1".to_owned());
        let mine = dir(format!("{me}-1"));
        let live = dir(format!("{parent}-1"));
        let other = dir("x".to_owned());
        sweep(temp);
        assert!(!dead.exists(), "a dead process's root goes");
        assert!(!mine.exists(), "an earlier root of this pid goes");
        assert!(live.exists(), "a live process's root stays");
        assert!(other.exists(), "a name that is no root stays");
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
