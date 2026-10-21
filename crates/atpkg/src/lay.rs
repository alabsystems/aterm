// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Laying executables (the `bin/` shims, their `agents/` twins and `alab-` aliases, the
//! pending stubs, the reroute stubs, the tombstones) through one writer:
//! [`write_in_process`], and [`write_regenerable`] for a file every seed lays again.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

/// One executable to lay: where, and what bytes. Always mode `0755`, always temp+rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    /// The destination path.
    pub path: PathBuf,
    /// The file's bytes.
    pub body: Vec<u8>,
}

impl Executable {
    /// An executable at `path` with `body`.
    pub fn new(path: impl Into<PathBuf>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            body: body.into(),
        }
    }
}

/// Write one executable: a dotted sibling temp (`.<name>.lay-<pid>-<seq>`, never swept by
/// the reroute dir's walk; the sequence keeps two lays of one name in one process off each
/// other's temp), the bytes, mode `0755` set explicitly (so a restrictive umask cannot lay
/// a shim only its owner can run in a system prefix), a flush, then `rename(2)` over the
/// destination, so a shim on the user's PATH is never briefly absent or half-written. Only
/// the rename takes [`PUBLICATION_TURN`]: the staging before it is this writer's own file.
///
/// The flush is [`aterm_durable::ordered`] (on Apple targets `F_BARRIERFSYNC`): a real
/// shim, a tombstone or a trust exec root's re-render is what a terminal outside aterm
/// runs next, and no launch re-lays it before then, so its bytes must land before its
/// name. This writer does not flush the shim's parent after the rename, so it does
/// not promise that publication survives a power loss. A later `gc` full flush of the
/// build-store parent covers that directory and earlier fsync-submitted I/O on its
/// device; it does not submit this other, unflushed directory's rename. Thus the
/// restored Durable cleanup does not establish a never-dangling PATH guarantee.
/// This missing parent-publication persistence also existed with the former full-file flush.
pub fn write_in_process(file: &Executable) -> io::Result<()> {
    write_with(file, aterm_durable::ordered)
}

/// [`write_in_process`] for a file the next seed lays again when it is missing or its bytes
/// differ (a pending stub, its `agents/` twin, a reroute stub, the reroute directory's
/// marker): the same temp, mode and `rename(2)`, flushed with a plain `fsync(2)`
/// ([`crate::platform::sync_file_contents`]) instead of the drive-cache barrier. The data
/// still reaches the drive before the rename names it, so a kernel panic cannot leave an
/// empty file under the name; a power loss can lose the newest lay, and the next seed
/// re-lays it (`stub::stub_is_current`, the reroute byte compare). A first seed lays ~35 of
/// these; the barrier, ~4 ms each, would add ~140 ms and stall the whole volume during the
/// first launch's other I/O.
pub fn write_regenerable(file: &Executable) -> io::Result<()> {
    write_with(file, crate::platform::sync_file_contents)
}

/// Per-process sequence for the writer's temp names ([`write_in_process`]).
static LAY_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The temp a lay of `dest` stages in: `.<name>.lay-<pid>-<seq>` beside it.
pub(crate) fn staged_at(dest: &std::path::Path, name: &str, seq: u64) -> PathBuf {
    dest.with_file_name(format!(".{name}.lay-{}-{seq}", std::process::id()))
}

/// Whether a directory entry is the temp of a lay THIS process has in flight: staged, not
/// yet renamed over its destination (every failing lay removes its own temp, so one named
/// for this pid is mid-publish). The walks that hold [`PUBLICATION_TURN`] to sweep `bin/`
/// and `agents/` leave it to its writer, which renames it the moment the turn is free:
/// removing it would fail that lay, where before the staging left the turn the lay simply
/// ran after the sweep. A temp another process left is no lay of this one, and is swept.
pub(crate) fn in_flight_here(entry: &OsStr) -> bool {
    entry
        .to_str()
        .and_then(|name| name.strip_prefix('.'))
        .and_then(|name| name.rsplit_once(".lay-"))
        .and_then(|(_, tail)| tail.split_once('-'))
        .is_some_and(|(pid, seq)| {
            pid.parse() == Ok(std::process::id())
                && !seq.is_empty()
                && seq.bytes().all(|b| b.is_ascii_digit())
        })
}

/// Serializes every executable publication in the process (a lay's rename, or a removal)
/// with a reconcile's read of a primary and its publish of what that read selected. Lock
/// order: `activate::AGENTS_TURN`, then this. A writer stages its bytes before it asks for
/// this and releases it before it asks for an agents reconcile; a reconcile holds both and
/// writes through [`write_in_publication_turn`]. Held for a rename, not a flush: every
/// lay in the process (a test binary's thousand stores included) waits on it.
pub(crate) static PUBLICATION_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The one writer: stages the bytes with the caller's flush `sync` (a sibling temp only
/// this writer names), then takes [`PUBLICATION_TURN`] for the publication alone: the
/// `rename(2)` that is the only step a reader of the destination can see.
fn write_with(file: &Executable, sync: fn(&std::fs::File) -> io::Result<()>) -> io::Result<()> {
    let tmp = stage(file, sync)?;
    #[cfg(all(test, unix))]
    if matches!(
        PUBLICATION_TURN.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    ) {
        crate::activate::agents_checkpoint::waiting(&file.path);
    }
    let _turn = PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    publish(file, &tmp)
}

/// The ordered writer while the caller holds [`PUBLICATION_TURN`] across the read that
/// selected these bytes. Reacquiring the turn here would deadlock that reconcile.
pub(crate) fn write_in_publication_turn(
    file: &Executable,
    _turn: &std::sync::MutexGuard<'_, ()>,
) -> io::Result<()> {
    // Ordered, as [`write_in_process`]: the old shim or the new one.
    let tmp = stage(file, aterm_durable::ordered)?;
    publish(file, &tmp)
}

/// Remove a primary under [`PUBLICATION_TURN`]: a reconcile that already read it finishes
/// before the removal, or reads the absence after it, and never recreates the target it
/// captured.
pub(crate) fn remove_in_process(path: &std::path::Path) -> io::Result<()> {
    let _turn = PUBLICATION_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::fs::remove_file(path)
}

/// Write `file`'s bytes to a fresh sibling temp ([`staged_at`]), mode `0755`, flushed with
/// `sync`; the temp is removed again on any error.
fn stage(file: &Executable, sync: fn(&std::fs::File) -> io::Result<()>) -> io::Result<PathBuf> {
    let name = file
        .path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "executable has no file name")
        })?;
    let tmp = staged_at(
        &file.path,
        name,
        LAY_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    );
    let _ = std::fs::remove_file(&tmp);
    let written = (|| -> io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o755);
        }
        let mut f = opts.open(&tmp)?;
        use std::io::Write as _;
        f.write_all(&file.body)?;
        sync(&f)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    })();
    match written {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Rename a [`stage`]d temp over `file`'s destination; the temp is removed if that fails.
fn publish(file: &Executable, tmp: &std::path::Path) -> io::Result<()> {
    // Windows `rename` does not replace an existing file (the same brief window the
    // Windows shim writer documents; a shim there is per-user and serialized).
    #[cfg(windows)]
    let _ = std::fs::remove_file(&file.path);
    if let Err(e) = std::fs::rename(tmp, &file.path) {
        let _ = std::fs::remove_file(tmp);
        return Err(e);
    }
    Ok(())
}

// The tests check a Unix mode, so the module is Unix-only.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn tmp(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-lay-{label}-"))
    }

    /// A lay that a terminal outside aterm runs next is only ordered ahead of its rename
    /// (aterm-durable's counting seam), never fully flushed.
    #[test]
    fn an_in_process_lay_is_ordered() {
        use aterm_durable::{Flush, seam::Recording};
        let d = tmp("flush-tiers");
        let dest = d.join("tool");
        let recording = Recording::start();
        write_in_process(&Executable::new(&dest, "#!/bin/sh\n")).unwrap();
        assert_eq!(recording.take(), [Flush::Ordered]);
    }

    /// Both writers: mode 0755, the exact bytes, an existing file replaced by a rename (a
    /// new inode — never truncated in place, so a reader holding the old file reads it
    /// whole), no temp left behind.
    #[test]
    fn the_in_process_writer_lays_0755_atomically() {
        use std::os::unix::fs::MetadataExt as _;
        use std::os::unix::fs::PermissionsExt as _;
        type Writer = fn(&Executable) -> io::Result<()>;
        let writers: [(&str, Writer); 2] = [
            ("ordered", write_in_process),
            ("regenerable", write_regenerable),
        ];
        for (label, write) in writers {
            let d = tmp(label);
            let dest = d.join("tool");
            std::fs::write(&dest, "old\n").unwrap();
            let old = std::fs::File::open(&dest).unwrap();
            let old_ino = old.metadata().unwrap().ino();
            let f = Executable::new(&dest, "#!/bin/sh\necho new\n");
            write(&f).unwrap();
            assert_eq!(std::fs::read(&dest).unwrap(), f.body, "{label}");
            assert_ne!(
                std::fs::metadata(&dest).unwrap().ino(),
                old_ino,
                "{label}: replaced by rename"
            );
            assert_eq!(
                std::io::read_to_string(old).unwrap(),
                "old\n",
                "{label}: the old file stays whole for a reader that holds it"
            );
            assert_eq!(
                std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
                0o755,
                "{label}"
            );
            let leftovers: Vec<_> = std::fs::read_dir(&d)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with('.'))
                .collect();
            assert!(leftovers.is_empty(), "{label}: {leftovers:?}");
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// The turn covers the publication alone. A lay that finds [`PUBLICATION_TURN`] held
    /// has already staged its bytes: its one in-flight temp ([`in_flight_here`]) holds the
    /// whole body at mode `0755`, and the destination is untouched. Released, the turn lets
    /// the rename publish those bytes, and no temp is left. A lay that flushed under the
    /// turn held every other lay in the process behind its flush.
    #[test]
    fn a_lay_waits_for_the_turn_with_its_bytes_already_staged() {
        use crate::activate::agents_checkpoint::{self, Event};
        use std::os::unix::fs::PermissionsExt as _;
        let layout = crate::testdir::layout("atpkg-lay-staged-");
        let bin = layout.bin_dir();
        std::fs::create_dir_all(&bin).unwrap();
        let file = Executable::new(
            layout.shim(&crate::store::ToolName::new("ay").unwrap()),
            "#!/bin/sh\necho staged\n",
        );
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let staged_in = bin.clone();
        let _watch = agents_checkpoint::watch(
            &layout,
            std::sync::Arc::new(move |event| {
                if let Event::Waiting(name) = event {
                    let staged: Vec<(Vec<u8>, u32)> = std::fs::read_dir(&staged_in)
                        .unwrap()
                        .flatten()
                        .filter(|e| in_flight_here(&e.file_name()))
                        .map(|e| {
                            let mode = e.metadata().unwrap().permissions().mode() & 0o777;
                            (std::fs::read(e.path()).unwrap(), mode)
                        })
                        .collect();
                    let _ = waiting_tx.send((name.to_owned(), staged));
                }
            }),
        );
        let turn = PUBLICATION_TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let writer = std::thread::spawn({
            let file = file.clone();
            move || write_in_process(&file)
        });
        let (name, staged) = waiting_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the writer reaches the held turn");
        assert_eq!(name, "ay");
        assert_eq!(
            staged,
            [(file.body.clone(), 0o755)],
            "staged before the turn"
        );
        assert!(
            !file.path.exists(),
            "nothing published while the turn is held"
        );
        drop(turn);
        writer.join().unwrap().unwrap();
        assert_eq!(std::fs::read(&file.path).unwrap(), file.body);
        let left: Vec<_> = std::fs::read_dir(&bin)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, ["ay"], "the temp became the destination");
    }
}
