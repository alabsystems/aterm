// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows: a claim file whose delete is pending is gone to every look, as an unlinked one is on
//! unix. Windows' by nature: unix has no delete that leaves the name in place.

use super::*;

/// A CLAIM WHOSE DELETE IS PENDING IS GONE TO EVERY LOOK: a claim's file deleted while a handle
/// of it is open keeps its name, and refuses every open of it, until that handle closes;
/// `CreateFileW` answers such an open `ERROR_ACCESS_DENIED`. A holder's own release passes
/// through that state (its delete's handle is open until the delete returns), so a listing made
/// then failed with `PermissionDenied`, as `aterm-verify`'s quiet-claim tests did. Here the state
/// is held still: the file deleted under its holder's open lock handle by the legacy disposition,
/// which keeps the name until the last handle closes. The listing, the pid's claims and the
/// reaping scan each pass it over and keep the claim beside it, and a bounded read of it is
/// `NotFound`. NEGATIVE CONTROL: with the bounded reader opening the name through `CreateFileW`
/// again, `Ledger::list` here failed with `PermissionDenied`.
#[test]
fn a_claim_whose_delete_is_pending_is_gone_to_every_look() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ledger = Ledger::at(dir.path().join("ledger"));
    let (kept, _kept) = ledger.lay(&Need::new("kept"), 1).expect("kept laid");
    let (going, going_laid) = ledger.lay(&Need::new("going"), 2).expect("going laid");
    let path = going_laid.path().to_owned();
    let name = path.file_name().expect("a claim file name").to_owned();
    let folder = aterm_dirfd::pin(&std::fs::canonicalize(ledger.dir()).expect("canonical"))
        .expect("the ledger folder");
    let id = aterm_dirfd::file_id(
        aterm_dirfd::openat(
            &folder,
            &name,
            aterm_dirfd::OFlags::PATH,
            aterm_dirfd::Mode::empty(),
        )
        .expect("the claim file"),
    )
    .expect("its id");
    aterm_dirfd::seam::legacy_classes(true);
    let removed = aterm_dirfd::unlinkat(&folder, &name, aterm_dirfd::AtFlags::empty(), id);
    aterm_dirfd::seam::legacy_classes(false);
    removed.expect("deleted under its holder's handle");

    // Premise: the name is still listed, and an open as `CreateFileW` makes it is refused.
    assert!(
        std::fs::read_dir(ledger.dir())
            .expect("listed")
            .any(|entry| entry.expect("an entry").file_name() == name),
        "a file whose delete is pending keeps its name"
    );
    assert_eq!(
        std::fs::File::open(&path)
            .expect_err("a delete-pending file opens for nobody")
            .kind(),
        io::ErrorKind::PermissionDenied
    );

    let seqs = |records: Vec<Record>| records.iter().map(|r| r.seq).collect::<Vec<_>>();
    assert_eq!(
        seqs(ledger.list().expect("a listing while a delete is pending")),
        [kept.seq]
    );
    assert_eq!(
        seqs(
            ledger
                .of_pid(std::process::id())
                .expect("the pid's claims while a delete is pending")
        ),
        [kept.seq]
    );
    assert_eq!(
        seqs(
            ledger
                .scan(true)
                .expect("a reaping scan while a delete is pending")
        ),
        [kept.seq]
    );
    assert_eq!(
        crate::open_regular_reader(&path)
            .expect_err("a delete-pending file is gone")
            .kind(),
        io::ErrorKind::NotFound
    );

    drop(going_laid);
    assert!(
        !std::fs::read_dir(ledger.dir())
            .expect("listed")
            .any(|entry| entry.expect("an entry").file_name() == name),
        "the name goes with its last handle"
    );
    assert_ne!(going.seq, kept.seq);
}
