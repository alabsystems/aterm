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
    if let Err(e) = aterm_uds::deadline::try_mark_ran(copy) {
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
    home.is_absolute().then(|| code_dir_in(&home))
}

/// [`code_dir`] for the absolute `home`.
#[must_use]
pub fn code_dir_in(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Application Support")
        .join("aterm")
        .join("keeper-code")
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

/// Copy `exe`'s code into `dir` and re-execute from it with [`ORIGIN_ENV`] naming `exe`.
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

/// The keeper (its pid and start) whose copy, or half-made `.partial` copy, `name` names: none
/// for any other name.
#[must_use]
pub fn owner_of(name: &str) -> Option<(u32, Birth)> {
    let name = name.strip_suffix(".partial").unwrap_or(name);
    let mut parts = name.split('-');
    let pid: u32 = parts
        .next()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()?;
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
    copy_tree_kind(src, dst, std::fs::symlink_metadata(src)?.file_type())
}

fn copy_tree_kind(src: &Path, dst: &Path, kind: std::fs::FileType) -> io::Result<()> {
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
    } else if kind.is_dir() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let meta = std::fs::symlink_metadata(src)?;
        std::fs::create_dir(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            copy_tree_kind(&entry.path(), &dst.join(entry.file_name()), ft)?;
        }
        let dir_fd = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(dst)?;
        let perms = meta.permissions();
        if dir_fd
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o7777 == perms.mode() & 0o7777)
        {
            return Ok(());
        }
        dir_fd.set_permissions(perms)
    } else if kind.is_file() {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let (mut source, src_meta) = aterm_uds::regular_file::open(src)?;
        let perms = src_meta.permissions();
        let mut sink = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(perms.mode())
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(dst)?;
        std::io::copy(&mut source, &mut sink)?;
        if sink
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o7777 == perms.mode() & 0o7777)
        {
            return Ok(());
        }
        sink.set_permissions(perms)
    } else {
        Err(io::Error::other(format!(
            "{} is not a file, a directory or a link",
            src.display()
        )))
    }
}

/// Become `copy`, with [`ORIGIN_ENV`] naming `origin`.
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
#[path = "code_tests.rs"]
mod tests;
