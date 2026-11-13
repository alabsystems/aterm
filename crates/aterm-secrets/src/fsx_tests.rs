// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::tempdir().expect("tempdir")
}

/// A glance gives each path the `lstat` it took of it first ([`Glance`]): a file rewritten,
/// removed or made since stands as the glance saw it, while a new glance sees each as it is;
/// each path has its own, and an error is not kept, so the next ask looks again.
/// NEGATIVE CONTROLS (2026-11-08): a glance that looks afresh at each ask sees the rewrite
/// (`a rewrite stands as first seen`); one that tells paths apart by their length alone gives
/// the second file the first one's `lstat` (`each path its own`); one that keeps a failed look
/// as an absence answers the file made at that path since with none (`an error is looked at
/// again`).
#[test]
fn a_glance_gives_each_path_the_lstat_it_took_first() {
    let root = scratch();
    let (a, b, made) = (
        root.path().join("a"),
        root.path().join("b"),
        root.path().join("c"),
    );
    write_atomic(&a, b"one", 0o600).unwrap();
    write_atomic(&b, b"second", 0o600).unwrap();
    let len = |meta: Option<std::fs::Metadata>| meta.map(|meta| meta.len());
    let glance = Glance::new();
    assert_eq!(len(glance.symlink_metadata(&a).unwrap()), Some(3));
    assert_eq!(
        len(glance.symlink_metadata(&b).unwrap()),
        Some(6),
        "each path its own"
    );
    assert_eq!(len(glance.symlink_metadata(&made).unwrap()), None);

    write_atomic(&a, b"rewritten", 0o600).unwrap();
    std::fs::remove_file(&b).unwrap();
    write_atomic(&made, b"made", 0o600).unwrap();
    assert_eq!(
        len(glance.symlink_metadata(&a).unwrap()),
        Some(3),
        "a rewrite stands as first seen"
    );
    assert_eq!(
        len(glance.symlink_metadata(&b).unwrap()),
        Some(6),
        "a removal stands as first seen"
    );
    assert_eq!(
        len(glance.symlink_metadata(&made).unwrap()),
        None,
        "a file made since stands absent"
    );
    let now = Glance::new();
    assert_eq!(len(now.symlink_metadata(&a).unwrap()), Some(9));
    assert_eq!(len(now.symlink_metadata(&b).unwrap()), None);
    assert_eq!(len(now.symlink_metadata(&made).unwrap()), Some(4));

    let child = a.join("child");
    assert!(
        glance.symlink_metadata(&child).is_err(),
        "a file has no child"
    );
    std::fs::remove_file(&a).unwrap();
    std::fs::create_dir(&a).unwrap();
    write_atomic(&child, b"child", 0o600).unwrap();
    assert_eq!(
        len(glance.symlink_metadata(&child).unwrap()),
        Some(5),
        "an error is looked at again"
    );
}

/// A glance's first look at a file stands as of a later moment only while one `lstat` taken then
/// shows the same, and only where that first look showed a regular file that had settled when the
/// glance began ([`Glance::first_stands`]): a file left untouched stands; a byte-identical copy
/// renamed over it, a `chmod`, a first look begun a second after the file's change, a path the
/// glance never looked at, one whose first look found nothing (though a file was made there
/// since) and a folder do not. Each ask that reached a fresh `lstat` moves this thread's count of
/// fresh looks ([`fresh_looks`]) by exactly one, and the others by none; whether the first look
/// showed a settled file ([`Glance::first_settled`]) looks at nothing, and answers by that first
/// look alone, so the renamed file's first look still shows one. Every time the law asks is taken
/// within [`as_if_later`] or from the file's own stamp, so none depends on how soon after its
/// writes the law runs.
/// NEGATIVE CONTROLS (2026-11-11): with `Glance::first_seen_settled` asking no settled rule, the
/// first look begun a second after its file's change stands (`a first look before its file
/// settled`: `left: (true, 1), right: (false, 0)`); with `Glance::first_stands` comparing a fresh
/// `lstat` with another fresh one, not with the glance's first, the renamed copy stands (`a
/// byte-identical copy renamed over`: `left: (true, 1), right: (false, 1)`).
#[test]
fn a_glances_first_look_stands_only_while_a_fresh_lstat_shows_it_settled() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = scratch();
    let path = |name: &str| root.path().join(name);
    for name in ["untouched", "renamed", "chmodded", "unsettled", "never"] {
        write_atomic(&path(name), b"control anchor", 0o600).unwrap();
    }
    std::fs::create_dir(path("folder")).unwrap();
    let seen = seen_if_present(&path("unsettled")).unwrap().unwrap();
    let changed = std::time::UNIX_EPOCH
        + std::time::Duration::from_nanos(
            u64::try_from(seen.stamp.mtime_ns.max(seen.stamp.ctime_ns)).unwrap(),
        );
    as_if_later(std::time::Duration::from_secs(3600), || {
        let glance = Glance::new();
        for name in ["untouched", "renamed", "chmodded", "absent", "folder"] {
            glance.symlink_metadata(&path(name)).unwrap();
        }
        let unsettled = Glance::begun_at(changed + std::time::Duration::from_secs(1));
        unsettled.symlink_metadata(&path("unsettled")).unwrap();
        std::fs::copy(path("renamed"), path("renamed.copy")).unwrap();
        std::fs::rename(path("renamed.copy"), path("renamed")).unwrap();
        std::fs::set_permissions(path("chmodded"), std::fs::Permissions::from_mode(0o400)).unwrap();
        write_atomic(&path("absent"), b"made since", 0o600).unwrap();
        let asked = |glance: &Glance, name: &str, ask: fn(&Glance, &Path) -> bool| {
            let before = fresh_looks();
            let answer = ask(glance, &path(name));
            (answer, fresh_looks() - before)
        };
        let stands = |glance: &Glance, name: &str| asked(glance, name, Glance::first_stands);
        assert_eq!(stands(&glance, "untouched"), (true, 1), "an untouched file");
        assert_eq!(
            stands(&glance, "renamed"),
            (false, 1),
            "a byte-identical copy renamed over"
        );
        assert_eq!(stands(&glance, "chmodded"), (false, 1), "a chmod");
        assert_eq!(
            stands(&unsettled, "unsettled"),
            (false, 0),
            "a first look before its file settled"
        );
        assert_eq!(
            stands(&glance, "never"),
            (false, 0),
            "a path the glance never looked at"
        );
        assert_eq!(
            stands(&glance, "absent"),
            (false, 0),
            "a path whose first look found nothing"
        );
        assert_eq!(stands(&glance, "folder"), (false, 0), "a folder");
        assert_eq!(
            stands(&glance, "untouched"),
            (true, 1),
            "CONTROL: the untouched file's first look stands as it was, the fresh one unkept"
        );
        let settled = |glance: &Glance, name: &str| asked(glance, name, Glance::first_settled);
        assert_eq!(
            [
                settled(&glance, "untouched"),
                settled(&glance, "renamed"),
                settled(&unsettled, "unsettled"),
                settled(&glance, "never"),
                settled(&glance, "absent"),
                settled(&glance, "folder"),
            ],
            [
                (true, 0),
                (true, 0),
                (false, 0),
                (false, 0),
                (false, 0),
                (false, 0)
            ],
            "whether a first look showed a settled file, looking at nothing"
        );
        let next = Glance::new();
        next.symlink_metadata(&path("renamed")).unwrap();
        assert_eq!(
            stands(&next, "renamed"),
            (true, 1),
            "CONTROL: the next glance's first look at the copy stands"
        );
    });
}

#[test]
fn a_stamp_distinguishes_missing_paths_from_failed_observations() {
    let root = scratch();
    let path = root.path().join("file");
    assert_eq!(stamp_if_present(&path).unwrap(), None);
    write_atomic(&path, b"present", 0o600).unwrap();
    assert!(stamp_if_present(&path).unwrap().is_some());
    assert!(stamp_if_present(&path.join("cannot-be-a-child")).is_err());
}

/// A digest of what a glance saw is kept only where its file had settled ([`SETTLED_NS`]) when
/// the glance began, not merely by the ask: a glance's `lstat` can be older than the ask, and a
/// change between them in one tick of a coarse clock would leave its witness standing. A file
/// a glance began to look at a second after its last change is read again at each ask through
/// it, though the asks come two hours after the change; one a glance begun an hour after the
/// change saw is read once. Each time is taken from the file's own stamp, so neither depends
/// on how soon after the write the law runs.
/// NEGATIVE CONTROL (2026-11-10): judged settled as of the ask, the file a glance began to
/// look at a second after its change is read once (`a digest of what a glance saw before its
/// file settled`).
#[test]
fn a_glance_fed_digest_is_kept_only_where_its_file_had_settled_when_the_glance_began() {
    const PRIVATE: Reader = Reader {
        admit: Admit::Private,
        max: 64,
    };
    let root = scratch();
    let path = root.path().join("custody");
    write_atomic(&path, b"custody", 0o600).unwrap();
    let seen = seen_if_present(&path).unwrap().unwrap();
    let changed = std::time::UNIX_EPOCH
        + std::time::Duration::from_nanos(
            u64::try_from(seen.stamp.mtime_ns.max(seen.stamp.ctime_ns)).unwrap(),
        );
    let after_it = |secs| changed + std::time::Duration::from_secs(secs);
    let begun_after_it = |secs| Glance::begun_at(after_it(secs));
    let reads_through = |glance: &Glance| {
        let kept = std::sync::Mutex::new(std::collections::VecDeque::new());
        let reads = std::cell::Cell::new(0);
        for _ in 0..2 {
            digest_while_seen_in(
                &kept,
                Some(glance),
                after_it(7200),
                &path,
                PRIVATE,
                |path| {
                    reads.set(reads.get() + 1);
                    PRIVATE.digest(path)
                },
            )
            .unwrap()
            .unwrap();
        }
        reads.get()
    };
    assert_eq!(
        reads_through(&begun_after_it(1)),
        2,
        "a digest of what a glance saw before its file settled"
    );
    assert_eq!(
        reads_through(&begun_after_it(3600)),
        1,
        "CONTROL: a digest of what a glance begun after its file settled saw"
    );
}

/// A digest is kept while its file stands ([`digest_while_seen`]): a settled file is read once
/// however often its digest is asked, and each change a reader would see is read again (its
/// bytes rewritten in place at the same length, the file opened to others, another file renamed
/// over it at the same length, the file removed), as is a file swapped in for the read that
/// gave the digest, at its path or through its parent directory, and a file changed within
/// [`SETTLED_NS`] of the ask. A digest one reader kept never answers a reader that refuses the
/// file.
/// NEGATIVE CONTROLS (2026-11-08): a digest kept by its path alone, and one kept by the file's
/// device, inode and length, each answer the rewrite in place with the digest they kept; one
/// kept without the opened file's stamp answers the parent's swap with the swapped-in file's;
/// one kept for any reader answers the stricter reader; one kept without the settled rule
/// reads a file changed a second before the ask once. A digest kept without its mode answers
/// no change here wrongly, since APFS moves a file's change time at each `chmod`, which
/// [`Seen`] holds for a coarser clock.
#[test]
fn a_digest_is_kept_while_its_file_stands_and_read_again_after_each_change() {
    use std::os::unix::fs::PermissionsExt as _;
    /// A change to a file whose digest is kept, and what makes it.
    struct Change {
        what: &'static str,
        make: fn(&Path),
    }
    const PRIVATE: Reader = Reader {
        admit: Admit::Private,
        max: 64,
    };
    fn read(path: &Path) -> crate::Result<Option<(FileStamp, [u8; 32])>> {
        PRIVATE.digest(path)
    }
    /// What a fresh read by `reader` answers: its digest, `None` where there is no file, or
    /// `None` for its refusal.
    fn fresh(reader: Reader, path: &Path) -> Option<Option<[u8; 32]>> {
        reader
            .digest(path)
            .ok()
            .map(|read| read.map(|(_, digest)| digest))
    }
    fn store() -> KeptDigests {
        std::sync::Mutex::new(std::collections::VecDeque::new())
    }
    /// When the file at `path` last changed, `secs` seconds on.
    fn after_its_change(path: &Path, secs: u64) -> std::time::SystemTime {
        let seen = seen_if_present(path).unwrap().unwrap();
        let changed = seen.stamp.mtime_ns.max(seen.stamp.ctime_ns);
        std::time::UNIX_EPOCH
            + std::time::Duration::from_nanos(u64::try_from(changed).unwrap())
            + std::time::Duration::from_secs(secs)
    }
    let sha = aterm_digest::Sha256::digest;
    // The file stood an hour unchanged at each ask but the settled rule's.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    let changes = [
        Change {
            what: "its bytes rewritten in place at the same length",
            make: |path| {
                let file = File::options().write(true).open(path).unwrap();
                file.write_all_at(b"CUSTODY", 0).unwrap();
                // Its mtime moved away too, so the case holds where a coarse clock gives the
                // rewrite the time the file had, which the settled rule answers at a real ask.
                file.set_modified(std::time::UNIX_EPOCH).unwrap();
            },
        },
        Change {
            what: "the file opened to others",
            make: |path| {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
            },
        },
        Change {
            what: "another file renamed over it at the same length",
            make: |path| {
                let beside = path.with_extension("new");
                write_atomic(&beside, b"CUSTODY", 0o600).unwrap();
                std::fs::rename(&beside, path).unwrap();
            },
        },
        Change {
            what: "the file removed",
            make: |path| std::fs::remove_file(path).unwrap(),
        },
    ];
    for Change { what, make } in changes {
        let root = scratch();
        let path = root.path().join("custody");
        write_atomic(&path, b"custody", 0o600).unwrap();
        let kept = store();
        let first = digest_while_seen_in(&kept, None, later, &path, PRIVATE, read).unwrap();
        assert_eq!(first, Some(sha(b"custody")), "{what}");
        let reads = reads_of(&path);
        assert_eq!(
            digest_while_seen_in(&kept, None, later, &path, PRIVATE, read).unwrap(),
            first
        );
        assert_eq!(reads_of(&path), reads, "{what}: read again while it stood");
        make(&path);
        let again = digest_while_seen_in(&kept, None, later, &path, PRIVATE, read).ok();
        assert_eq!(again, fresh(PRIVATE, &path), "{what}");
        assert_ne!(again, Some(first), "{what}: answered with the kept digest");
    }

    // Another file swapped in for the read, at the path or through its parent directory,
    // gives that read its digest, which is not kept: the next ask reads the file put back.
    let root = scratch();
    let dir = root.path().join("vault");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("custody");
    write_atomic(&path, b"custody", 0o600).unwrap();
    /// A swap of another file in for the read: it puts the other file there, reads it and
    /// puts the file back.
    type Swap = fn(&Path) -> crate::Result<Option<(FileStamp, [u8; 32])>>;
    let swaps: [(&str, Swap); 2] = [
        ("its path", |path| {
            let aside = path.with_extension("aside");
            std::fs::rename(path, &aside).unwrap();
            write_atomic(path, b"CUSTODY", 0o600).unwrap();
            let swapped_in = read(path);
            std::fs::rename(&aside, path).unwrap();
            swapped_in
        }),
        ("its parent directory", |path| {
            let dir = path.parent().unwrap();
            let aside = dir.with_extension("aside");
            std::fs::rename(dir, &aside).unwrap();
            std::fs::create_dir(dir).unwrap();
            write_atomic(path, b"CUSTODY", 0o600).unwrap();
            let swapped_in = read(path);
            std::fs::remove_file(path).unwrap();
            std::fs::remove_dir(dir).unwrap();
            std::fs::rename(&aside, dir).unwrap();
            swapped_in
        }),
    ];
    for (through, swap) in swaps {
        let kept = store();
        let seen = seen_if_present(&path).unwrap();
        let swapped = digest_while_seen_in(&kept, None, later, &path, PRIVATE, swap);
        assert_eq!(swapped.unwrap(), Some(sha(b"CUSTODY")), "through {through}");
        if through == "its parent directory" {
            assert_eq!(
                seen_if_present(&path).unwrap(),
                seen,
                "the parent's swap left the file's witness as it was"
            );
        }
        assert_eq!(
            digest_while_seen_in(&kept, None, later, &path, PRIVATE, read).unwrap(),
            Some(sha(b"custody")),
            "the file put back through {through}"
        );
    }

    // A digest one reader kept answers no reader that refuses the file.
    let stricter = [
        (
            "a reader refusing its mode",
            Reader {
                admit: Admit::Any,
                max: 64,
            },
            PRIVATE,
            0o640,
        ),
        (
            "a reader with a smaller cap",
            PRIVATE,
            Reader {
                admit: Admit::Private,
                max: 4,
            },
            0o600,
        ),
    ];
    for (what, laxer, stricter, mode) in stricter {
        let root = scratch();
        let path = root.path().join("custody");
        write_atomic(&path, b"custody", mode).unwrap();
        let kept = store();
        let admitted =
            digest_while_seen_in(&kept, None, later, &path, laxer, |path| laxer.digest(path));
        assert_eq!(admitted.unwrap(), Some(sha(b"custody")), "{what}");
        let refused = digest_while_seen_in(&kept, None, later, &path, stricter, |path| {
            stricter.digest(path)
        });
        assert!(refused.is_err(), "{what}: {refused:?}");
    }

    // A file that changed a second before the ask is read at each ask, and one that changed
    // three seconds before it once.
    let root = scratch();
    let path = root.path().join("custody");
    write_atomic(&path, b"changed", 0o600).unwrap();
    for (secs, read_at) in [(1, 2), (3, 1)] {
        let kept = store();
        let at = after_its_change(&path, secs);
        let reads = reads_of(&path);
        for _ in 0..2 {
            let changed = digest_while_seen_in(&kept, None, at, &path, PRIVATE, read).unwrap();
            assert_eq!(changed, Some(sha(b"changed")));
        }
        assert_eq!(
            reads_of(&path) - reads,
            read_at,
            "a file changed {secs} s before the ask"
        );
    }
}

/// F23 (rule 5): an append refuses a file renamed over the one its writer last left, though
/// it has the same length: `append_at` checks the file's device and inode with its length.
/// NEGATIVE CONTROL: the length-only check of main before F23 appends onto it, and the test
/// fails "the append went onto a file renamed over with the same length".
#[test]
fn an_append_refuses_a_file_renamed_over_with_the_same_length() {
    let root = scratch();
    let path = root.path().join("team.records");
    let left = write_atomic_stamped(&path, b"ours", 0o600)
        .expect("write")
        .expect("its stamp");
    assert_eq!(stamp(&path), Some(left));
    let beside = root.path().join("team.records.new");
    write_atomic(&beside, b"some", 0o600).expect("write");
    std::fs::rename(&beside, &path).expect("renamed over");
    assert_eq!(stamp(&path).map(|now| now.len), Some(left.len));
    assert!(
        append_at(&path, &left, b"tail").is_err(),
        "the append went onto a file renamed over with the same length"
    );
    assert_eq!(std::fs::read(&path).expect("read"), b"some");

    // The file it stamped takes the append, which returns the stamp it leaves.
    let now = stamp(&path).expect("stamped");
    let after = append_at(&path, &now, b"tail").expect("appended");
    assert_eq!(std::fs::read(&path).expect("read"), b"sometail");
    assert_eq!(stamp(&path), Some(after));
    assert_eq!(after.len, 8);
    assert_eq!(
        read_owned_stamped(&path, 64).expect("read"),
        Some((after, b"sometail".to_vec()))
    );
}

#[test]
fn a_private_dir_is_made_and_narrowed_to_0700() {
    let root = scratch();
    let dir = root.path().join("a").join("secrets");
    ensure_private_dir(&dir).expect("made");
    assert_eq!(
        std::fs::metadata(&dir).expect("meta").mode() & 0o7777,
        0o700
    );
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("widen");
    ensure_private_dir(&dir).expect("narrowed");
    assert_eq!(
        std::fs::metadata(&dir).expect("meta").mode() & 0o7777,
        0o700
    );
}

#[test]
fn a_symlinked_dir_is_refused() {
    let root = scratch();
    let real = root.path().join("real");
    std::fs::create_dir(&real).expect("dir");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let error = ensure_private_dir(&link).expect_err("refused");
    assert!(error.0.contains("symlink"), "{error}");
}

#[test]
fn private_reads_refuse_group_or_other_bits_and_symlinks() {
    let root = scratch();
    let path = root.path().join("token");
    write_atomic(&path, b"value", 0o600).expect("write");
    assert_eq!(
        std::fs::metadata(&path).expect("meta").mode() & 0o777,
        0o600
    );
    assert_eq!(
        read_private(&path, 64)
            .expect("read")
            .expect("present")
            .as_slice(),
        b"value"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
    let error = read_private(&path, 64).expect_err("refused");
    assert!(!error.0.contains("value"), "{error}");
    assert!(
        read_owned(&path, 64)
            .expect("owned admits group read")
            .is_some()
    );
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&path, &link).expect("link");
    assert!(read_public(&link, 64).is_err());
    assert!(
        read_private(&root.path().join("missing"), 64)
            .expect("ok")
            .is_none()
    );

    let lock_path = root.path().join("state.lock");
    std::fs::write(&lock_path, b"").expect("seed lock");
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o644))
        .expect("widen lock");
    let _lock = open_lock(&lock_path).expect("open_lock");
    assert_eq!(
        std::fs::symlink_metadata(&lock_path).expect("meta").mode() & 0o7777,
        0o600
    );
    let lock_hard = root.path().join("state.lock.hard");
    std::fs::hard_link(&lock_path, &lock_hard).expect("hard link lock");
    assert!(open_lock(&lock_path).is_err());
    std::fs::remove_file(&lock_hard).expect("rm hard link lock");

    let append_file = root.path().join("append.bin");
    write_atomic(&append_file, b"head", 0o600).expect("write append");
    let stamped = |path: &Path| stamp(path).expect("stamped");
    append_at(&append_file, &stamped(&append_file), b"-tail").expect("append private");
    assert_eq!(std::fs::read(&append_file).expect("read"), b"head-tail");
    std::fs::set_permissions(&append_file, std::fs::Permissions::from_mode(0o644))
        .expect("widen append");
    assert!(append_at(&append_file, &stamped(&append_file), b"!").is_err());
    std::fs::set_permissions(&append_file, std::fs::Permissions::from_mode(0o600))
        .expect("restore append");
    let append_hard = root.path().join("append.hard");
    std::fs::hard_link(&append_file, &append_hard).expect("hard link append");
    assert!(append_at(&append_file, &stamped(&append_file), b"!").is_err());
    std::fs::remove_file(&append_hard).expect("rm hard link append");
    let real_dir = root.path().join("real-dir");
    std::fs::create_dir(&real_dir).expect("real dir");
    write_atomic(&real_dir.join("in.bin"), b"abcd", 0o600).expect("seed in");
    let link_dir = root.path().join("link-dir");
    std::os::unix::fs::symlink(&real_dir, &link_dir).expect("symlink dir");
    let real = stamped(&real_dir.join("in.bin"));
    assert!(append_at(&link_dir.join("in.bin"), &real, b"ef").is_err());
}

/// A read fills one buffer sized from the file's length, a byte over so it sees the end
/// without growing: a buffer that grew freed each block it outgrew, unwiped.
#[test]
fn a_read_fills_one_buffer_sized_from_the_files_length() {
    let root = scratch();
    let path = root.path().join("value");
    write_atomic(&path, &[7u8; 1000], 0o600).expect("write");
    let bytes = read_owned(&path, 4096).expect("read").expect("there");
    assert_eq!((bytes.len(), bytes.capacity()), (1000, 1001));
}

#[test]
fn reads_are_bounded_and_a_fifo_never_blocks() {
    let root = scratch();
    let path = root.path().join("big");
    write_atomic(&path, &[7u8; 65], 0o600).expect("write");
    assert!(read_private(&path, 64).is_err());
    assert!(read_private(&path, 65).expect("ok").is_some());
    let fifo = root.path().join("fifo");
    let made = aterm_uds::spawnfd::command("/usr/bin/mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo");
    assert!(made.success());
    assert!(
        read_public(&fifo, 64).is_err(),
        "a FIFO is not a regular file"
    );
}

#[test]
fn rename_noreplace_refuses_an_existing_target() {
    let root = scratch();
    let a = root.path().join("a");
    let b = root.path().join("b");
    write_atomic(&a, b"a", 0o600).expect("a");
    write_atomic(&b, b"b", 0o600).expect("b");
    assert!(rename_noreplace(&a, &b).is_err());
    assert_eq!(std::fs::read(&b).expect("b"), b"b");
    std::fs::remove_file(&b).expect("rm");
    rename_noreplace(&a, &b).expect("moved");
    assert_eq!(kind(&a).expect("kind"), Kind::Missing);
    assert_eq!(std::fs::read(&b).expect("b"), b"a");
}

#[test]
fn a_rename_whose_unlink_fails_undoes_its_link() {
    let root = scratch();
    let locked = root.path().join("locked");
    std::fs::create_dir(&locked).expect("dir");
    let from = locked.join("f");
    write_atomic(&from, b"kept", 0o600).expect("write");
    let to = root.path().join("to");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("lock");
    let result = rename_noreplace(&from, &to);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("unlock");
    if euid() == 0 {
        result.expect("root renames");
        assert_eq!(kind(&from).expect("kind"), Kind::Missing);
        return;
    }
    let error = result.expect_err("the unlink is refused");
    assert!(error.0.contains("locked/f"), "{error}");
    assert_eq!(
        kind(&to).expect("kind"),
        Kind::Missing,
        "the link is undone"
    );
    assert_eq!(std::fs::read(&from).expect("from"), b"kept");
    assert_eq!(
        std::fs::symlink_metadata(&from).expect("meta").nlink(),
        1,
        "one name again"
    );
}

#[test]
fn file_ids_tell_one_file_from_two() {
    let root = scratch();
    let a = root.path().join("a");
    write_atomic(&a, b"x", 0o600).expect("a");
    let link = root.path().join("hard");
    std::fs::hard_link(&a, &link).expect("link");
    let other = root.path().join("other");
    write_atomic(&other, b"x", 0o600).expect("other");
    assert!(file_id(&a).is_some());
    assert_eq!(file_id(&a), file_id(&link));
    assert_ne!(file_id(&a), file_id(&other));
    assert_eq!(file_id(&root.path().join("missing")), None);
    let soft = root.path().join("soft");
    std::os::unix::fs::symlink(&a, &soft).expect("symlink");
    assert_ne!(file_id(&soft), file_id(&a));
}

#[test]
fn wipe_removes_files_and_only_the_link_of_a_link() {
    let root = scratch();
    let file = root.path().join("f");
    write_atomic(&file, b"secret", 0o600).expect("write");
    let link = root.path().join("l");
    std::os::unix::fs::symlink(&file, &link).expect("link");
    wipe_and_remove(&link).expect("link removed");
    assert_eq!(std::fs::read(&file).expect("intact"), b"secret");
    wipe_and_remove(&file).expect("wiped");
    assert_eq!(kind(&file).expect("kind"), Kind::Missing);
    wipe_and_remove(&file).expect("missing is fine");
}

#[test]
fn wipe_zeroes_the_bytes_under_every_name() {
    let root = scratch();
    let file = root.path().join("f");
    write_atomic(&file, b"secret", 0o600).expect("write");
    let kept = root.path().join("kept");
    std::fs::hard_link(&file, &kept).expect("link");
    let mut open = File::open(&file).expect("open");
    wipe_and_remove(&file).expect("wiped");
    assert_eq!(kind(&file).expect("kind"), Kind::Missing);
    assert_eq!(std::fs::read(&kept).expect("the other name"), [0u8; 6]);
    let mut left = Vec::new();
    open.read_to_end(&mut left).expect("read");
    assert_eq!(
        left, [0u8; 6],
        "a descriptor open before the wipe reads zeros"
    );
}

#[test]
fn a_judged_wipe_never_touches_a_file_renamed_over_the_one_it_judged() {
    let root = scratch();
    let file = root.path().join("config");
    write_atomic(&file, b"the vault's", 0o600).expect("write");
    assert!(!wipe_if(&file, 64, |bytes| bytes == b"other").expect("judged"));
    assert!(
        !wipe_if(&file, 4, |_| true).expect("judged"),
        "past the bound"
    );
    assert_eq!(std::fs::read(&file).expect("kept"), b"the vault's");

    let tools = root.path().join("config.lock");
    write_atomic(&tools, b"the tool's", 0o600).expect("write");
    let removed = wipe_if(&file, 64, |bytes| {
        std::fs::rename(&tools, &file).expect("renamed over");
        bytes == b"the vault's"
    })
    .expect("judged");
    assert!(!removed);
    assert_eq!(
        std::fs::read(&file).expect("the tool's file"),
        b"the tool's"
    );

    let kept = root.path().join("kept");
    std::fs::hard_link(&file, &kept).expect("link");
    assert!(wipe_if(&file, 64, |bytes| bytes == b"the tool's").expect("wiped"));
    assert_eq!(kind(&file).expect("kind"), Kind::Missing);
    assert_eq!(std::fs::read(&kept).expect("the other name"), [0u8; 10]);
}

/// The names in `dir`, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn write_new_fills_only_an_empty_name() {
    let root = scratch();
    let path = root.path().join("gap");
    assert!(write_new(&path, b"filled", 0o600).expect("written"));
    assert_eq!(std::fs::read(&path).expect("read"), b"filled");
    assert_eq!(
        std::fs::symlink_metadata(&path).expect("meta").mode() & 0o7777,
        0o600
    );
    assert_eq!(
        std::fs::symlink_metadata(&path).expect("meta").nlink(),
        1,
        "the temp's name is gone"
    );

    assert!(!write_new(&path, b"again", 0o600).expect("taken"));
    assert_eq!(std::fs::read(&path).expect("read"), b"filled");
    let dangling = root.path().join("dangling");
    let nowhere = root.path().join("nowhere");
    std::os::unix::fs::symlink(&nowhere, &dangling).expect("link");
    assert!(!write_new(&dangling, b"v", 0o600).expect("taken"));
    assert_eq!(kind(&dangling).expect("kind"), Kind::Symlink);
    assert_eq!(kind(&nowhere).expect("kind"), Kind::Missing);
    assert_eq!(names_in(root.path()), ["dangling", "gap"], "no temp left");

    let missing = root.path().join("no-dir").join("x");
    let error = write_new(&missing, b"v", 0o600).expect_err("no parent");
    assert!(error.0.contains("no-dir"), "{error}");
}

/// [`write_atomic_if_changed`] leaves a file that already holds the bytes under the mode as it
/// is (the same inode and times, `false`), and replaces anything else as [`write_atomic`] does
/// (`true`): nothing there, other bytes of the same or another length, the same bytes under
/// another mode, and a symlink to a file holding the bytes.
#[test]
fn a_write_if_changed_leaves_the_same_bytes_in_place() {
    let root = scratch();
    let path = root.path().join("derived");
    let ino = |path: &Path| std::fs::symlink_metadata(path).unwrap().ino();

    assert!(write_atomic_if_changed(&path, b"text one", 0o600).unwrap());
    let first = stamp(&path).unwrap();
    assert!(!write_atomic_if_changed(&path, b"text one", 0o600).unwrap());
    assert_eq!(stamp(&path), Some(first), "the same bytes: the same file");

    assert!(write_atomic_if_changed(&path, b"text two", 0o600).unwrap());
    assert_ne!(ino(&path), first.ino, "other bytes of the same length");
    assert_eq!(std::fs::read(&path).unwrap(), b"text two");
    assert!(write_atomic_if_changed(&path, b"text two and more", 0o600).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"text two and more");
    assert!(write_atomic_if_changed(&path, b"text", 0o600).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"text", "a prefix is not it");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(write_atomic_if_changed(&path, b"text", 0o600).unwrap());
    assert_eq!(
        std::fs::symlink_metadata(&path).unwrap().mode() & 0o7777,
        0o600,
        "the same bytes under a wider mode are written again"
    );

    let (target, link) = (root.path().join("target"), root.path().join("link"));
    write_atomic(&target, b"text", 0o600).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(write_atomic_if_changed(&link, b"text", 0o600).unwrap());
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_file(),
        "a symlink is replaced, not read through"
    );
}
