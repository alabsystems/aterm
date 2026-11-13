// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Independent first-start marks ordered against store removal by a stable program gate.
//! The gate is beside the program directory, never swept or replaced.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use super::{UNPROVEN_SUFFIX, sidecar_path};

pub(crate) const MARKS_SUFFIX: &str = ".unproven.d";
const STAMP: &str = "stamp";
const STAMP_BYTES: usize = 128;

// Cross-process first-start protocol: an unproved image can still be undergoing
// native assessment after its starter dies. Only exact-image execution proof
// releases this starter's mark; elapsed time and a reboot do not.
const PERMANENT_STAMP: &str = "first-exec permanent v1\n";

/// One starter's private mark and its still-open file: a replacement is never released.
#[must_use = "an unreleased first-exec mark holds its build for FIRST_EXEC_HANG"]
pub struct FirstExec {
    program: PathBuf,
    dir: PathBuf,
    file: File,
}

/// Held through registration, release, or the final destructive act. The gate pathname is
/// never removed, even after uninstall: an opener can still hold its inode.
pub(crate) struct Removal {
    _file: crate::lock::Flock,
    program: PathBuf,
}

/// This process's registrations and releases, one at a time. Each takes its program's gate
/// without waiting, so siblings that start together (a bundle's resolve check runs every exposed
/// binary of one build at once, `flow::bundle_resolve_check`) refused one another, and a release
/// that met a sibling's registration kept its mark, which then held the build for
/// [`aterm_uds::deadline::FIRST_EXEC_HANG`] and failed its flip (`docs/measured/
/// nux-round9-2026-11-02.md`, item 12). Each holds it only around its own gate, never across a
/// wait, and no removal takes it, so a removal still refuses a start, and is refused, at once.
static IN_PROCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn busy() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "first exec registration or removal is busy",
    )
}

/// Resolve aliases within the same actual program directory. Numeric aside names and their
/// ordinary links share a marker key; the program gate orders every build in the directory.
fn key(build: &Path) -> io::Result<PathBuf> {
    let parent = build
        .parent()
        .ok_or_else(|| io::Error::other("build has no program directory"))?;
    let program = std::fs::canonicalize(parent)?;
    let physical = match std::fs::canonicalize(build) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => program.join(
            build
                .file_name()
                .ok_or_else(|| io::Error::other("build has no name"))?,
        ),
        Err(error) => return Err(error),
    };
    if physical.parent() != Some(program.as_path()) {
        return Err(io::Error::other("build alias leaves its program directory"));
    }
    let name = physical
        .file_name()
        .ok_or_else(|| io::Error::other("build has no name"))?;
    let ordinary = name.to_str().and_then(|name| {
        name.strip_suffix(".noindex")
            .filter(|ordinary| super::parse_build_name(ordinary).is_some())
    });
    Ok(program.join(ordinary.map_or_else(|| name.to_os_string(), OsString::from)))
}

fn gate(program: &Path) -> io::Result<Removal> {
    let mut name = OsString::from(".");
    name.push(
        program
            .file_name()
            .ok_or_else(|| io::Error::other("program has no name"))?,
    );
    name.push(".first-exec.gate");
    let path = program.with_file_name(name);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000);
    }
    let file = aterm_uds::kept_out(|| options.open(&path))?;
    if !file.metadata()?.is_file() || !std::fs::symlink_metadata(&path)?.file_type().is_file() {
        return Err(io::Error::other(
            "first exec gate is not a regular non-link file",
        ));
    }
    let held = crate::lock::Flock::try_lock(file).map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => busy(),
        std::fs::TryLockError::Error(error) => error,
    })?;
    if !astream_sys::file_matches_path(&held, &path)? {
        return Err(io::Error::other("first exec gate changed while opening"));
    }
    Ok(Removal {
        _file: held,
        program: program.to_path_buf(),
    })
}

impl Removal {
    /// The caller already owns this program's gate; do not take a nested flock. Check the
    /// actual target too, including a nonnumeric incoming or superseded tree's own mark.
    pub(crate) fn check(&self, tree: &Path) -> io::Result<()> {
        let key = key(tree)?;
        if key.parent() != Some(self.program.as_path()) || held_unproven(&key) {
            return Err(io::Error::other(
                "the target is outside the gate or held for first exec",
            ));
        }
        Ok(())
    }
}

/// Hold before spawn. A gate another process or a removal holds, or an unwritable mark, refuses
/// without waiting or starting; this process's other registrations and releases go first.
/// # Errors
/// Unknown paths, a busy registration/removal gate, or a mark that cannot be written durably.
pub fn hold_first_exec(exe: &Path) -> io::Result<FirstExec> {
    hold_first_exec_at(exe, aterm_uds::process::awake_ms())
}

pub(super) fn hold_first_exec_at(exe: &Path, awake: Option<u64>) -> io::Result<FirstExec> {
    let resolved = match std::fs::canonicalize(exe) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => exe.to_path_buf(),
        Err(error) => return Err(error),
    };
    let build = resolved
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("no build holds this program"))?;
    let key = key(build)?;
    let program = key
        .parent()
        .ok_or_else(|| io::Error::other("no program"))?
        .to_path_buf();
    let _turn = IN_PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _gate = gate(&program)?;
    if self::key(build)? != key {
        return Err(io::Error::other("build alias changed before registration"));
    }
    let marks =
        sidecar_path(&key, MARKS_SUFFIX).ok_or_else(|| io::Error::other("no marker name"))?;
    match std::fs::symlink_metadata(&marks) {
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(io::Error::other(
                "first exec inventory is not a real directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(&marks)?,
        Err(error) => return Err(error),
    }
    let dir = aterm_tempfile::Builder::new()
        .prefix("start-")
        .tempdir_in(&marks)?;
    let mut file = crate::platform::open_create_new_write(&dir.path().join(STAMP), 0o600)?;
    file.write_all(
        awake
            .map(|now| now.to_string())
            .unwrap_or_default()
            .as_bytes(),
    )?;
    super::sync_contents_or_accept_refusal(&file)?;
    super::sync_dir(dir.path())?;
    super::sync_dir(&marks)?;
    let dir = dir.keep();
    Ok(FirstExec { program, dir, file })
}

impl FirstExec {
    /// Only this starter's mark goes. A gate another process or a removal holds, replaced names
    /// and unknown metadata retain it; this process's other registrations and releases go first.
    pub fn ran(self) {
        let _turn = IN_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(_gate) = gate(&self.program) else {
            return;
        };
        let stamp = self.dir.join(STAMP);
        if !std::fs::symlink_metadata(&stamp).is_ok_and(|meta| meta.file_type().is_file())
            || !astream_sys::file_matches_path(&self.file, &stamp).unwrap_or(false)
        {
            return;
        }
        if std::fs::remove_file(&stamp).is_ok() {
            let _ = std::fs::remove_dir(&self.dir);
            if let Some(marks) = self.dir.parent() {
                let _ = std::fs::remove_dir(marks);
            }
        }
    }
}

fn age_holds(text: &str, modified: Option<std::time::SystemTime>, awake: Option<u64>) -> bool {
    if text == PERMANENT_STAMP {
        return true;
    }
    let blank = text.is_empty();
    let text = text.trim();
    let stamp = if blank {
        None
    } else {
        let Some(stamp) = super::parse_build_name(text) else {
            return true;
        };
        Some(stamp)
    };
    let age = match (stamp, awake) {
        (Some(at), Some(now)) => match now.checked_sub(at) {
            Some(ms) => std::time::Duration::from_millis(ms),
            None => return false,
        },
        _ => modified
            .and_then(|at| at.elapsed().ok())
            .unwrap_or_default(),
    };
    age < aterm_uds::deadline::FIRST_EXEC_HANG
}

fn legacy_holds(mark: &Path, awake: Option<u64>) -> bool {
    match crate::metadata_io::read_bounded_regular_utf8(mark, STAMP_BYTES) {
        Ok(text) => age_holds(
            &text,
            std::fs::symlink_metadata(mark)
                .ok()
                .and_then(|meta| meta.modified().ok()),
            awake,
        ),
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

pub(crate) fn held_unproven(tree: &Path) -> bool {
    held_unproven_at(tree, aterm_uds::process::awake_ms())
}

pub(super) fn held_unproven_at(tree: &Path, awake: Option<u64>) -> bool {
    let build = if tree
        .parent()
        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "bin"))
    {
        tree.parent().and_then(Path::parent).unwrap_or(tree)
    } else {
        tree
    };
    let key = match key(build) {
        Ok(key) => key,
        Err(error) => return error.kind() != io::ErrorKind::NotFound,
    };
    let legacy = [Some(key.clone()), aterm_toolchain::layout::aside_of(&key)];
    if legacy.into_iter().flatten().any(|key| {
        sidecar_path(&key, UNPROVEN_SUFFIX).is_none_or(|mark| legacy_holds(&mark, awake))
    }) {
        return true;
    }
    let Some(marks) = sidecar_path(&key, MARKS_SUFFIX) else {
        return true;
    };
    let entries = match std::fs::read_dir(&marks) {
        Ok(entries) => entries,
        Err(error) => return error.kind() != io::ErrorKind::NotFound,
    };
    for entry in entries {
        let Ok(entry) = entry else { return true };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            return true;
        }
        let contents = match std::fs::read_dir(entry.path())
            .and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
        {
            Ok(contents) => contents,
            Err(_) => return true,
        };
        if contents.len() != 1
            || contents[0].file_name() != STAMP
            || !contents[0].file_type().is_ok_and(|kind| kind.is_file())
        {
            return true;
        }
        let stamp = entry.path().join(STAMP);
        let text = match crate::metadata_io::read_bounded_regular_utf8(&stamp, STAMP_BYTES) {
            Ok(text) => text,
            Err(_) => return true,
        };
        if age_holds(
            &text,
            entry.metadata().ok().and_then(|meta| meta.modified().ok()),
            awake,
        ) {
            return true;
        }
    }
    false
}

/// Unknown or incomplete program inventories refuse uninstall too.
pub(crate) fn held_build_in(program: &Path) -> Option<PathBuf> {
    held_build_among(
        program,
        std::fs::read_dir(program)
            .map(|entries| entries.map(|entry| entry.map(|entry| entry.path()))),
    )
}

fn held_build_among(
    program: &Path,
    entries: io::Result<impl Iterator<Item = io::Result<PathBuf>>>,
) -> Option<PathBuf> {
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => return Some(program.to_path_buf()),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return Some(program.to_path_buf()),
        };
        let Some(name) = entry.file_name() else {
            return Some(program.to_path_buf());
        };
        let Some(name) = name.to_str() else {
            return Some(program.to_path_buf());
        };
        let marked = name
            .strip_suffix(MARKS_SUFFIX)
            .or_else(|| name.strip_suffix(UNPROVEN_SUFFIX));
        let base = marked.unwrap_or(name);
        let base = base
            .strip_suffix(".noindex")
            .filter(|ordinary| super::parse_build_name(ordinary).is_some())
            .unwrap_or(base);
        if (marked.is_some() || super::parse_build_name(base).is_some())
            && held_unproven(&program.join(base))
        {
            return Some(program.join(base));
        }
    }
    None
}

/// Resolve the program once, then hold its stable gate without admitting any removal yet.
pub(crate) fn program_gate(program: &Path) -> io::Result<Removal> {
    let program = match std::fs::canonicalize(program) {
        Ok(program) => program,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = program
                .parent()
                .ok_or_else(|| io::Error::other("no store directory"))?;
            std::fs::canonicalize(parent)?.join(
                program
                    .file_name()
                    .ok_or_else(|| io::Error::other("no program name"))?,
            )
        }
        Err(error) => return Err(error),
    };
    gate(&program)
}

/// The same stable program gate protects inventory enumeration and the whole uninstall.
pub(crate) fn first_exec_program_removal(program: &Path) -> io::Result<Removal> {
    let gate = program_gate(program)?;
    let program = &gate.program;
    if held_build_in(program).is_some() {
        return Err(io::Error::other(
            "a first exec or unknown inventory holds this program",
        ));
    }
    Ok(gate)
}

/// Held through discard, replacement or lay-aside. Existing GC leases remain independently held.
pub(crate) fn first_exec_removal(build: &Path) -> io::Result<Removal> {
    let key = key(build)?;
    let program = key.parent().ok_or_else(|| io::Error::other("no program"))?;
    let gate = gate(program)?;
    if self::key(build)? != key {
        return Err(io::Error::other(
            "a first exec or unknown inventory holds this build",
        ));
    }
    gate.check(build)?;
    Ok(gate)
}

#[cfg(all(test, unix))]
#[path = "store_first_exec_own_tests.rs"]
mod tests;
