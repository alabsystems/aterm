// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KEEPER RUNS FROM ITS OWN COPY OF ITS CODE.
//!
//! A window admits a keeper only when the keeper's code meets the window's
//! designated requirement (§5.7), and macOS checks a running process's code
//! through the file it was started from (`crate::identity` has the
//! measurement). The keeper is the one long-lived process an update leaves on
//! the old build: the update moves that bundle to `aterm.app.rollback`, and the
//! successor's boot-health confirmation deletes it. A keeper running from that
//! file would then fail every window's check — each launch's boot HELLO, every
//! link reconnect, the next successor at its Commit — so nothing would register
//! and no later window could take a handed shell back.
//!
//! So `aterm keeper serve` first copies its own code into a directory only
//! keepers use and re-executes itself from the copy ([`run_from_own_copy`]):
//! the same pid, so launchd's job, its log and every descriptor stay. The copy
//! is the whole app bundle (its signature seals the bundle) or a build tree's
//! bare executable; on APFS each file is a clone, sharing its blocks with the
//! original until an update deletes that. Before re-running, the keeper checks
//! the copy as a window will check it ([`Identity::admits_code_at`]). A copy
//! that cannot be made or does not pass is removed, and the keeper serves from
//! where it started, as before, saying so in its log.
//!
//! Each copy is named by its keeper's pid and kernel start time, which `exec`
//! keeps. A keeper cannot remove the copy it runs from while it serves (a
//! window would check it against nothing), and launchd ends it with a SIGTERM
//! it does not catch, so a copy can outlive its keeper. `serve` removes its own
//! copy when it returns; `aterm keeper stop` waits for the keeper to end, then
//! removes every copy whose keeper is gone ([`sweep`]), as each keeper start
//! does (a crash or a logout leaves one until then). A live keeper's copy is
//! never touched. The directory is durable on purpose: `$TMPDIR`, where the
//! socket lives, is swept daily of anything older than three days
//! (`com.apple.bsd.dirhelper`), and a copy keeps its original's dates.
//!
//! Only a keeper whose peers check its code (`--identity designated`, the
//! default) copies itself: under the same-uid floor nothing reads it.

use std::io;
use std::path::{Path, PathBuf};

use crate::identity::Identity;
use crate::wire::Birth;

/// The variable naming the executable a keeper was started as, set when it
/// re-executes from its own copy: its socket, label and relaunch bundle are
/// still that executable's.
pub const ORIGIN_ENV: &str = "ATERM_KEEPER_ORIGIN";

/// Where keepers keep the copies they run from:
/// `$HOME/Library/Application Support/aterm/keeper-code`. `None` without an
/// absolute `$HOME`.
#[must_use]
pub fn code_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.is_absolute().then(|| {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("keeper-code")
    })
}

/// This process's executable, canonical.
fn own_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()
}

/// The copy in `dir` this process runs from (`<dir>/<pid>-<start>`), if it
/// runs from one.
#[must_use]
pub fn own_copy(dir: &Path) -> Option<PathBuf> {
    let dir = dir.canonicalize().ok()?;
    let exe = own_exe()?;
    let name = exe.strip_prefix(&dir).ok()?.components().next()?;
    Some(dir.join(name))
}

/// The executable this keeper was started as: what [`ORIGIN_ENV`] names when
/// it runs from its copy (`from_copy`), else its own.
#[must_use]
pub fn origin_exe(from_copy: bool) -> Option<PathBuf> {
    let named = std::env::var_os(ORIGIN_ENV)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    match named {
        Some(origin) if from_copy => Some(origin),
        _ => own_exe(),
    }
}

/// Copy `exe`'s code into `dir`, check the copy against `identity`, and
/// re-execute this process from it with the same arguments and [`ORIGIN_ENV`]
/// naming `exe`. Returns only when that could not happen, with why; the copy
/// is removed then.
#[must_use]
pub fn run_from_own_copy(exe: &Path, dir: &Path, identity: &Identity) -> String {
    let copy = match copy_own_code(exe, dir, identity) {
        Ok(copy) => copy,
        Err(why) => return why,
    };
    let failed = reexec(&copy.exe, exe);
    let _ = std::fs::remove_dir_all(&copy.home);
    format!("re-running from {}: {failed}", copy.exe.display())
}

/// A copy made for this process.
struct Copy {
    /// Its directory, `<dir>/<pid>-<seconds>-<micros>`.
    home: PathBuf,
    /// The executable in it to run.
    exe: PathBuf,
}

/// What names a copy: its keeper's pid and kernel start time.
fn copy_name(pid: u32, birth: Birth) -> String {
    format!("{pid}-{}-{}", birth.seconds, birth.micros)
}

/// The keeper a copy's directory name says it belongs to (a copy still being
/// made carries `.partial`); `None` for any other name.
fn owner_of(name: &str) -> Option<(u32, Birth)> {
    let name = name.strip_suffix(".partial").unwrap_or(name);
    let mut parts = name.split('-');
    let pid = parts.next()?.parse().ok()?;
    let seconds = parts.next()?.parse().ok()?;
    let micros = parts.next()?.parse().ok()?;
    parts
        .next()
        .is_none()
        .then_some((pid, Birth { seconds, micros }))
}

fn copy_own_code(exe: &Path, dir: &Path, identity: &Identity) -> Result<Copy, String> {
    let at = |e: io::Error| format!("{}: {e}", dir.display());
    if let Some(parent) = dir.parent() {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(at)?;
    }
    crate::server::ensure_private_dir(dir).map_err(at)?;
    let pid = std::process::id();
    let birth = crate::server::born(pid).ok_or("this process's start time cannot be read")?;
    let name = copy_name(pid, birth);
    // The whole bundle when `exe` is a bundle's (its signature seals it),
    // else the executable alone; under the same name, inside `home`.
    let source = crate::job::own_bundle(exe).unwrap_or_else(|| exe.to_path_buf());
    let inner = source
        .parent()
        .and_then(|p| exe.strip_prefix(p).ok())
        .ok_or_else(|| format!("{} is not inside {}", exe.display(), source.display()))?
        .to_path_buf();
    let top = source.file_name().ok_or("the code has no name")?;
    let home = dir.join(&name);
    let partial = dir.join(format!("{name}.partial"));
    let made = std::fs::create_dir(&partial)
        .and_then(|()| copy_tree(&source, &partial.join(top)))
        .and_then(|()| std::fs::rename(&partial, &home));
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!(
            "copying {} into {}: {e}",
            source.display(),
            dir.display()
        ));
    }
    if let Err(why) = identity.admits_code_at(&home.join(top)) {
        let _ = std::fs::remove_dir_all(&home);
        return Err(format!(
            "its copy was not admitted by this build's check: {why}"
        ));
    }
    Ok(Copy {
        exe: home.join(inner),
        home,
    })
}

/// Copy `src` to `dst`: directories recreated, symbolic links recreated as
/// they read, files copied (a clone on APFS) with their permissions.
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if kind.is_dir() {
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        std::fs::set_permissions(dst, meta.permissions())
    } else if kind.is_file() {
        std::fs::copy(src, dst).map(drop)
    } else {
        Err(io::Error::other(format!(
            "{} is not a file, a directory or a link",
            src.display()
        )))
    }
}

/// Become `copy`, with this process's arguments (its `argv[0]` too) and
/// [`ORIGIN_ENV`] naming `origin`. Returns only the error of a failed `exec`.
///
/// The copy keeps the pid and stdio (launchd's log) and nothing else: it
/// runs before the keeper binds or holds anything, and a keeper is handed no
/// descriptor beyond its stdio. Built by `spawnfd::command` (`grep_guard`
/// B27): the copy holds this process's stdio and nothing else, and a failed
/// `exec` (Darwin's `POSIX_SPAWN_SETEXEC`) leaves this process as it was.
fn reexec(copy: &Path, origin: &Path) -> io::Error {
    let mut args = std::env::args_os();
    let mut cmd = aterm_uds::spawnfd::command(copy);
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    cmd.args(args).env(ORIGIN_ENV, origin).exec()
}

/// Remove every copy in `dir` whose keeper is gone: no process with its pid,
/// or another one than the start time names (`born` reads a pid's). Anything
/// else in `dir` is left alone. Returns how many were removed.
pub fn sweep(dir: &Path, born: impl Fn(u32) -> Option<Birth>) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(owner_of)
                .is_some_and(|(pid, birth)| born(pid) != Some(birth))
                && std::fs::remove_dir_all(entry.path()).is_ok()
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_is_named_by_its_keeper() {
        let birth = Birth {
            seconds: 1_790_000_000,
            micros: 42,
        };
        let name = copy_name(512, birth);
        assert_eq!(name, "512-1790000000-42");
        assert_eq!(owner_of(&name), Some((512, birth)));
        assert_eq!(owner_of(&format!("{name}.partial")), Some((512, birth)));
        for other in ["", "512", "512-1", "512-1-2-3", "x-1-2", ".DS_Store"] {
            assert_eq!(owner_of(other), None, "{other:?}");
        }
    }

    /// THE SWEEP removes exactly the copies whose keeper is gone — a dead pid,
    /// or a live pid that is another process than the one named (its start
    /// time differs: the pid was reused), and a half-made copy of either —
    /// and keeps a live keeper's copy and anything that is not a copy.
    #[test]
    fn the_sweep_removes_only_copies_whose_keeper_is_gone() {
        let dir = std::env::temp_dir().join(format!("akcopies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let live = Birth {
            seconds: 100,
            micros: 1,
        };
        let names = [
            ("7-100-1", true),          // its keeper, pid 7, lives
            ("7-100-2", false),         // pid 7 is another process now
            ("8-100-1", false),         // pid 8 is gone
            ("8-100-1.partial", false), // half made, its keeper gone
            ("7-100-1.partial", true),  // half made by a live keeper
            ("notes", true),            // not a copy
        ];
        for (name, _) in names {
            std::fs::create_dir_all(dir.join(name).join("aterm.app")).expect("copy");
        }
        let removed = sweep(&dir, |pid| (pid == 7).then_some(live));
        assert_eq!(removed, 3);
        for (name, kept) in names {
            assert_eq!(dir.join(name).exists(), kept, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A BUNDLE IS COPIED WHOLE and stays valid: an ad-hoc-signed bundle (its
    /// Info.plist and a resource sealed, an argv0 alias as a symbolic link)
    /// copied by `copy_tree` passes `codesign --verify --strict`. NEGATIVE
    /// CONTROL: the same copy with its sealed resource changed does not.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_copied_bundle_keeps_its_signature() {
        // Re-signing a copy of an Apple-signed binary waits on trustd.
        if aterm_uds::deadline::trust_service_silent() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("akbundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = dir.join("t.app");
        let macos = app.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).expect("bundle");
        std::fs::create_dir_all(app.join("Contents").join("Resources")).expect("resources");
        std::fs::write(
            app.join("Contents").join("Info.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>t</string>\
             <key>CFBundleIdentifier</key><string>com.aterm.keeper.copytest</string>\
             </dict></plist>\n",
        )
        .expect("plist");
        std::fs::write(app.join("Contents").join("Resources").join("r.txt"), "r").expect("r");
        std::fs::copy("/usr/bin/true", macos.join("t")).expect("exe");
        std::os::unix::fs::symlink("t", macos.join("t-alias")).expect("alias");
        // Each codesign under a hang detector: one that macOS stops answering
        // mid-run fails this test in a minute instead of hanging the suite.
        let codesign = |args: &[&str], path: &Path| {
            aterm_uds::deadline::output_until(
                aterm_uds::spawnfd::command("/usr/bin/codesign")
                    .args(args)
                    .arg(path),
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            )
            .expect("codesign")
        };
        let sign = codesign(&["--force", "--sign", "-"], &app);
        assert!(sign.status.success(), "{sign:?}");
        let verify = |path: &Path| codesign(&["--verify", "--strict"], path);
        assert!(verify(&app).status.success(), "the original verifies");
        let copy = dir.join("copy").join("t.app");
        std::fs::create_dir_all(copy.parent().expect("parent")).expect("dir");
        copy_tree(&app, &copy).expect("copy");
        let checked = verify(&copy);
        assert!(checked.status.success(), "the copy: {checked:?}");
        assert!(
            std::fs::symlink_metadata(copy.join("Contents/MacOS/t-alias"))
                .expect("alias")
                .file_type()
                .is_symlink()
        );
        std::fs::write(copy.join("Contents").join("Resources").join("r.txt"), "x").expect("r");
        assert!(!verify(&copy).status.success(), "a changed copy verifies");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
