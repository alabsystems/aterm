// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The keeper runs from its own copy of its code.

use std::io;
use std::path::{Path, PathBuf};

use crate::identity::Identity;
use crate::wire::Birth;

/// Variable naming the executable a keeper was started as.
pub const ORIGIN_ENV: &str = "ATERM_KEEPER_ORIGIN";

/// Marker file written into the copy's directory ([`mark_ran`], [`sweep`]).
pub const RAN: &str = aterm_uds::deadline::RAN;

/// Say, in `copy` (the directory [`own_copy`] names), that this keeper runs from it.
pub fn mark_ran(copy: &Path) {
    if let Err(e) = std::fs::write(copy.join(RAN), b"") {
        eprintln!(
            "aterm keeper: cannot mark {} as run from: {e}",
            copy.display()
        );
    }
}

/// Where keepers keep the copies they run from.
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

fn own_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()
}

/// The copy in `dir` this process runs from (`<dir>/<pid>-<start>`), if it runs from one.
#[must_use]
pub fn own_copy(dir: &Path) -> Option<PathBuf> {
    let dir = dir.canonicalize().ok()?;
    let exe = own_exe()?;
    let name = exe.strip_prefix(&dir).ok()?.components().next()?;
    Some(dir.join(name))
}

/// The executable this keeper was started as: what [`ORIGIN_ENV`] names when it runs from its copy.
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

/// Copy `exe`'s code into `dir` ([`ORIGIN_ENV`]).
#[must_use]
pub fn run_from_own_copy(exe: &Path, dir: &Path, identity: &Identity) -> String {
    let copy = match copy_own_code(exe, dir, identity) {
        Ok(copy) => copy,
        Err(why) => return why,
    };
    aterm_uds::deadline::stamp_exec(&copy.home);
    let failed = reexec(&copy.exe, exe);
    let _ = std::fs::remove_dir_all(&copy.home);
    format!("re-running from {}: {failed}", copy.exe.display())
}

struct Copy {
    /// Its directory, `<dir>/<pid>-<seconds>-<micros>`.
    home: PathBuf,
    /// The executable in it to run.
    exe: PathBuf,
}

fn copy_name(pid: u32, birth: Birth) -> String {
    format!("{pid}-{}-{}", birth.seconds, birth.micros)
}

fn owner_of(name: &str) -> Option<(u32, Birth)> {
    let name = name.strip_suffix(".partial").unwrap_or(name);
    let mut parts = name.split('-');
    let pid = parts.next()?.parse().ok()?;
    let seconds = parts.next()?.parse().ok()?;
    let micros = parts.next()?.parse().ok()?;
    let birth = Birth { seconds, micros };
    (parts.next().is_none() && copy_name(pid, birth) == name).then_some((pid, birth))
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

/// Become `copy` ([`ORIGIN_ENV`]).
fn reexec(copy: &Path, origin: &Path) -> io::Error {
    let mut args = std::env::args_os();
    let mut cmd = aterm_uds::spawnfd::command(copy);
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    cmd.args(args).env(ORIGIN_ENV, origin).exec()
}

/// Remove every copy in `dir` whose keeper is gone.
pub fn sweep(dir: &Path, born: impl Fn(u32) -> Option<Birth>) -> usize {
    sweep_at(dir, born, std::time::SystemTime::now())
}

/// [`sweep`] at `now`.
fn sweep_at(dir: &Path, born: impl Fn(u32) -> Option<Birth>, now: std::time::SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            if !owner_of(name).is_some_and(|(pid, birth)| born(pid) != Some(birth)) {
                return false;
            }
            if !may_go(&entry.path(), name.ends_with(".partial"), now) {
                eprintln!(
                    "aterm keeper: kept {}: its keeper ended before it said it ran from it, so \
                     macOS may still be assessing its first exec",
                    entry.path().display()
                );
                return false;
            }
            std::fs::remove_dir_all(entry.path()).is_ok()
        })
        .count()
}

/// Whether `home` may go ([`RAN`], [`run_from_own_copy`], [`aterm_uds::deadline::copy_may_go`]).
fn may_go(home: &Path, partial: bool, now: std::time::SystemTime) -> bool {
    partial || aterm_uds::deadline::copy_may_go(home, now)
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
        for other in [
            "",
            "512",
            "512-1",
            "512-1-2-3",
            "x-1-2",
            ".DS_Store",
            "+512-1-2",
            "0512-1-2",
            "512-01-2",
            "512-1-02",
            "512-1-+2.partial",
        ] {
            assert_eq!(owner_of(other), None, "{other:?}");
        }
    }

    #[test]
    fn the_sweep_removes_only_copies_whose_keeper_is_gone_and_ran() {
        let dir = std::env::temp_dir().join(format!("akcopies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let live = Birth {
            seconds: 100,
            micros: 1,
        };
        let names = [
            ("7-100-1", false, true, true),           // its keeper, pid 7, lives
            ("7-100-2", true, false, false),          // pid 7 is another process now
            ("8-100-1", true, false, false),          // pid 8 is gone
            ("9-100-1", false, true, false),          // pid 9 is gone, its copy never said it ran
            ("8-100-1.partial", false, false, false), // half made, its keeper gone
            ("7-100-1.partial", false, true, true),   // half made by a live keeper
            ("notes", false, true, true),             // not a copy
        ];
        for (name, ran, _, _) in names {
            std::fs::create_dir_all(dir.join(name).join("aterm.app")).expect("copy");
            if ran {
                mark_ran(&dir.join(name));
            }
        }
        let born = |pid| (pid == 7).then_some(live);
        let now = std::time::SystemTime::now();
        assert_eq!(sweep_at(&dir, born, now), 3);
        for (name, _, kept, _) in names {
            assert_eq!(dir.join(name).exists(), kept, "{name}");
        }
        let later = now + aterm_uds::deadline::FIRST_EXEC_HANG;
        assert_eq!(sweep_at(&dir, born, later), 1);
        for (name, _, _, kept) in names {
            assert_eq!(dir.join(name).exists(), kept, "{name}, later");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An ad-hoc-signed bundle (Info.plist and a resource sealed, an argv0 alias as a symbolic
    /// link) copied by `copy_tree` passes `codesign --verify --strict`. Negative control: the copy
    /// with its sealed resource changed does not.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_copied_bundle_keeps_its_signature() {
        aterm_uds::deadline::require_trust_service();
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
