// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn tmp(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-lay-{label}-"))
}

#[test]
fn staging_never_removes_an_occupied_candidate() {
    for kind in ["file", "symlink", "directory"] {
        let d = tmp("occupied");
        let dest = d.join("tool");
        std::fs::write(&dest, b"existing executable").unwrap();
        let candidate = staged_at(&dest, "tool", 0);
        let other = d.join("other");
        std::fs::write(&other, b"another file").unwrap();
        match kind {
            "file" => std::fs::write(&candidate, b"unrelated bytes").unwrap(),
            "symlink" => std::os::unix::fs::symlink(&other, &candidate).unwrap(),
            "directory" => std::fs::create_dir(&candidate).unwrap(),
            _ => unreachable!(),
        }
        let result = stage_numbered(&Executable::new(&dest, b"replacement"), |_| Ok(()), 0);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&dest).unwrap(), b"existing executable");
        assert_eq!(std::fs::read(&other).unwrap(), b"another file");
        match kind {
            "file" => assert_eq!(std::fs::read(&candidate).unwrap(), b"unrelated bytes"),
            "symlink" => assert_eq!(std::fs::read_link(&candidate).unwrap(), other),
            "directory" => assert!(std::fs::symlink_metadata(&candidate).unwrap().is_dir()),
            _ => unreachable!(),
        }
    }
}

#[test]
fn a_failed_stage_removes_only_the_temporary_it_created() {
    let d = tmp("flush-failure");
    let dest = d.join("tool");
    std::fs::write(&dest, b"existing executable").unwrap();
    let candidate = staged_at(&dest, "tool", 0);
    let error = stage_numbered(
        &Executable::new(&dest, b"replacement"),
        |_| Err(io::Error::other("injected flush failure")),
        0,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(std::fs::read(&dest).unwrap(), b"existing executable");
    assert!(!candidate.try_exists().unwrap());
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
