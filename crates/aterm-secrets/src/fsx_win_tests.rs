// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! fsx's laws on Windows: what the Unix laws in `fsx_tests.rs` judge by a mode, a symlink or an
//! inode, judged here by an access list, a junction and a 128-bit file id. Every file is in a
//! scratch folder of the test's own.

use crate::fsx::{
    Admit, FileStamp, Glance, KeptDigests, Kind, Reader, append_at, as_if_later,
    digest_while_seen_in, ensure_private_dir, file_id, fresh_looks, has_private_nonempty, kind,
    open_lock, open_private_reader, read_owned, read_private, read_public, reads_of,
    seen_if_present, stamp, wipe_and_remove, wipe_if, write_atomic, write_atomic_if_changed,
    write_new,
};
use std::fs::File;
use std::os::windows::fs::FileExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

use aterm_uds::acl::{self, Descriptor, Parts, Sid};

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::tempdir().expect("tempdir")
}

/// `path`'s plain spelling (`C:\…`): the verbatim prefix dropped, for `mklink`.
fn plain(path: &Path) -> String {
    let text = path.to_str().expect("test paths are UTF-8");
    text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
}

/// `link` made a junction to the folder `target` with `cmd /C mklink /J`, spawned as every child.
fn junction(link: &Path, target: &Path) {
    let mut command = aterm_uds::spawnfd::command("cmd");
    command.args(["/C", "mklink", "/J", &plain(link), &plain(target)]);
    let out = aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
        .expect("mklink ran");
    assert!(out.status.success(), "mklink: {out:?}");
}

/// The file at `path` given the protected list of `entries` (SDDL access entries), its owner
/// left as it is: `(A;;FA;;;<user>)(A;;FA;;;SY)` is owner-only.
fn list(path: &Path, entries: &str) {
    Descriptor::from_sddl(&format!("D:P{entries}"))
        .and_then(|descriptor| descriptor.apply_to_path(path, Parts::DACL | Parts::PROTECTED_DACL))
        .expect("the list written");
}

/// This user's SID in SDDL.
fn me() -> String {
    Sid::token_user().expect("the token's user").to_string()
}

/// The file at `path` opened to others: owner-only entries and a read grant to every user.
fn open_to_users(path: &Path) {
    list(path, &format!("(A;;FA;;;{})(A;;FA;;;SY)(A;;FR;;;BU)", me()));
}

/// The file at `path` made owner-only again.
fn owner_only(path: &Path) {
    list(path, &format!("(A;;FA;;;{})(A;;FA;;;SY)", me()));
}

/// Whether the file at `path` is owner-only, as aterm-uds judges it.
fn is_owner_only(path: &Path) -> bool {
    let file = File::open(path).expect("open");
    acl::owner_only(&file, &Sid::token_user().expect("user"))
        .expect("judged")
        .is_ok()
}

/// The file at `path`'s last write moved to `when` (its change time moves with it to now).
fn last_written(path: &Path, when: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(when))
        .expect("set its last write");
}

/// The read-only file at `path` (a test's own scratch file) made writable, so its folder goes.
fn removable(path: &Path) {
    let mut permissions = std::fs::metadata(path).expect("meta").permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the scratch file is the test's own, made removable again"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).expect("writable");
}

/// The names in `dir`, sorted.
/// Make the file at `path` a reparse point of a tag no Microsoft filter owns (a cloud
/// placeholder's kind), opened as itself to set it.
fn tag_foreign(path: &Path) {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FOREIGN_TAG: u32 = 0x0000_7A7E;
    let file = File::options()
        .read(true)
        .write(true)
        .custom_flags(acl::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .unwrap();
    aterm_dirfd::seam::set_reparse_tag(&file, FOREIGN_TAG, [0x7a; 16]).unwrap();
}

/// The change time of the file at `path`, in filetime ticks.
fn change_time(path: &Path) -> i64 {
    aterm_dirfd::fstat(File::open(path).unwrap())
        .unwrap()
        .change_time
}

/// Put the change time of the file at `path` back to `ticks`, as any writer of its attributes
/// may: a change made within a tick of the system clock of the last one leaves it there too.
fn put_back_change_time(path: &Path, ticks: i64) {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    let file = File::options()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .open(path)
        .unwrap();
    aterm_dirfd::seam::set_change_time(&file, ticks).unwrap();
    assert_eq!(change_time(path), ticks, "the change time put back");
}

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

/// A same-size rewrite in place of a settled file moves its stamp: its last write moves, though
/// its volume, id and length stay. The file's last write is first put an hour back, as a settled
/// file's is, so the law holds however fast the rewrite follows.
/// NEGATIVE CONTROL (2026-11-16): `FileStamp::of` on Windows leaving the last write out
/// (`mtime_ns: 0`): `the rewrite moved its last write`.
#[test]
fn a_same_size_rewrite_in_place_moves_the_stamp() {
    let root = scratch();
    let path = root.path().join("custody");
    write_atomic(&path, b"custody", 0o600).expect("write");
    last_written(&path, SystemTime::now() - Duration::from_secs(3600));
    let before = stamp(&path).expect("stamped");
    File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.seek_write(b"CUSTODY", 0))
        .expect("rewritten in place");
    let after = stamp(&path).expect("stamped");
    assert_eq!(
        (after.dev, after.ino, after.len),
        (before.dev, before.ino, before.len),
        "the same file, of the same length"
    );
    assert_ne!(
        after.mtime_ns, before.mtime_ns,
        "the rewrite moved its last write"
    );
    assert_ne!(after, before, "a same-size rewrite moves the stamp");
}

/// A change to a settled file's access list moves what a look sees of it: its change time, the
/// one fact of a look that shows a descriptor change on Windows, where a look reads no access
/// list. The change time is first put an hour back, as a settled file's is, so the change falls
/// on another tick of the clock however fast it follows.
/// NEGATIVE CONTROL (2026-11-16): `FileStamp::of` leaving the change time out (`ctime_ns: 0`):
/// `an access-list change`.
#[test]
fn an_access_list_change_moves_what_a_look_sees_of_a_settled_file() {
    const HOUR: i64 = 3600 * 10_000_000;
    let root = scratch();
    let path = root.path().join("file");
    write_atomic(&path, b"value", 0o600).expect("write");
    put_back_change_time(&path, change_time(&path) - HOUR);
    let before = seen_if_present(&path).expect("look").expect("there");
    assert_eq!(
        seen_if_present(&path).expect("look"),
        Some(before),
        "CONTROL: an untouched file looks the same"
    );
    open_to_users(&path);
    assert_ne!(
        seen_if_present(&path).expect("look"),
        Some(before),
        "an access-list change"
    );
}

/// Another file renamed over a path, of the same length and the same last write, has another
/// stamp: its 128-bit file id is another, and the id moves with the file it names.
/// NEGATIVE CONTROL (2026-11-16): `FileStamp::of` on Windows leaving the id out (`ino: 0`):
/// `another file id`.
#[test]
fn a_file_renamed_over_with_the_same_length_and_times_has_another_stamp() {
    let root = scratch();
    let (path, other) = (root.path().join("records"), root.path().join("records.new"));
    write_atomic(&path, b"same", 0o600).expect("write");
    write_atomic(&other, b"same", 0o600).expect("write");
    let when = SystemTime::now() - Duration::from_secs(3600);
    last_written(&path, when);
    last_written(&other, when);
    let (before, beside) = (
        stamp(&path).expect("stamped"),
        stamp(&other).expect("stamped"),
    );
    assert_eq!((before.len, before.mtime_ns), (beside.len, beside.mtime_ns));
    std::fs::rename(&other, &path).expect("renamed over");
    let after = stamp(&path).expect("stamped");
    assert_eq!((after.len, after.mtime_ns), (before.len, before.mtime_ns));
    assert_ne!(after.ino, before.ino, "another file id");
    assert_eq!(
        (after.dev, after.ino),
        (beside.dev, beside.ino),
        "the id the file had"
    );
    assert_ne!(after, before);
}

/// A private read holds the file to owner-only (this user's, under a protected list of the user
/// and SYSTEM alone), one link and no reparse point; an owned read to this user's file that no
/// other account may change; a lock is made owner-only and refused with two links or another
/// owner; an append is refused on a file opened to others, of two links, or through a junction.
/// A junction, and a file that is a reparse point of another kind, is refused as one.
/// NEGATIVE CONTROLS (2026-11-16): `not_owner_only` answering `None`: `refused`;
/// `foreign_writer` answering `None`: `owned refuses others' writing`; `open_admitted` taking a
/// private file of two links: `two links`; `open_admitted` leaving out its reparse-point refusal:
/// the junction's refusal names no reparse point; `open_lock` leaving a lock unnarrowed: `the lock
/// is narrowed to owner-only`.
#[test]
fn private_reads_hold_a_file_to_owner_only_one_link_and_no_reparse_point() {
    let root = scratch();
    let path = root.path().join("token");
    write_atomic(&path, b"value", 0o600).expect("write");
    assert!(
        is_owner_only(&path),
        "a private write is owner-only from its creation"
    );
    assert!(has_private_nonempty(&path, 64));
    assert_eq!(
        read_private(&path, 64)
            .expect("read")
            .expect("present")
            .as_slice(),
        b"value"
    );
    open_to_users(&path);
    let error = read_private(&path, 64).expect_err("refused");
    assert!(!error.0.contains("value"), "{error}");
    assert!(!has_private_nonempty(&path, 64));
    assert!(
        read_owned(&path, 64)
            .expect("owned admits others' reading")
            .is_some()
    );
    list(
        &path,
        &format!("(A;;FA;;;{})(A;;FA;;;SY)(A;;FRFW;;;BU)", me()),
    );
    let error = read_owned(&path, 64).expect_err("owned refuses others' writing");
    assert!(error.0.contains("change it"), "{error}");
    assert!(read_public(&path, 64).expect("public admits it").is_some());
    owner_only(&path);
    let hard = root.path().join("token.hard");
    std::fs::hard_link(&path, &hard).expect("hard link");
    let error = read_private(&path, 64).expect_err("two links");
    assert!(error.0.contains("hard links"), "{error}");
    std::fs::remove_file(&hard).expect("rm");
    assert!(read_private(&path, 64).expect("one link again").is_some());

    let (real, link) = (root.path().join("real"), root.path().join("link"));
    std::fs::create_dir(&real).expect("dir");
    junction(&link, &real);
    let error = read_public(&link, 64).expect_err("a junction is refused");
    assert!(error.0.contains("reparse point"), "{error}");
    let tagged = root.path().join("tagged");
    write_atomic(&tagged, b"value", 0o600).expect("write");
    tag_foreign(&tagged);
    let error = read_private(&tagged, 64).expect_err("a file that is a reparse point");
    assert!(error.0.contains("reparse point"), "{error}");
    assert!(!has_private_nonempty(&tagged, 64));
    assert_eq!(kind(&link).expect("kind"), Kind::Symlink);
    assert!(
        read_private(&root.path().join("missing"), 64)
            .expect("ok")
            .is_none()
    );

    let lock_path = root.path().join("state.lock");
    std::fs::write(&lock_path, b"").expect("seed lock");
    assert!(
        !is_owner_only(&lock_path),
        "the seed takes its folder's entries"
    );
    drop(open_lock(&lock_path).expect("open_lock"));
    assert!(
        is_owner_only(&lock_path),
        "the lock is narrowed to owner-only"
    );
    let lock_hard = root.path().join("state.lock.hard");
    std::fs::hard_link(&lock_path, &lock_hard).expect("hard link lock");
    assert!(open_lock(&lock_path).is_err());
    std::fs::remove_file(&lock_hard).expect("rm");
    let error = super::as_another_user(|| open_lock(&lock_path)).expect_err("another's lock");
    assert!(error.0.contains("not this user"), "{error}");
    drop(open_lock(&lock_path).expect("this user's lock again"));

    let append = root.path().join("append.bin");
    write_atomic(&append, b"head", 0o600).expect("write");
    let stamped = |path: &Path| stamp(path).expect("stamped");
    append_at(&append, &stamped(&append), b"-tail").expect("append private");
    assert_eq!(std::fs::read(&append).expect("read"), b"head-tail");
    open_to_users(&append);
    assert!(append_at(&append, &stamped(&append), b"!").is_err());
    owner_only(&append);
    let append_hard = root.path().join("append.hard");
    std::fs::hard_link(&append, &append_hard).expect("hard link");
    assert!(append_at(&append, &stamped(&append), b"!").is_err());
    std::fs::remove_file(&append_hard).expect("rm");
    append_at(&append, &stamped(&append), b"!").expect("owner-only again");
    write_atomic(&real.join("in.bin"), b"abcd", 0o600).expect("seed in");
    let inner = stamped(&real.join("in.bin"));
    assert!(
        append_at(&link.join("in.bin"), &inner, b"ef").is_err(),
        "through a junction"
    );
}

/// A digest is kept while its file stands and read again after each change a reader would see:
/// its bytes rewritten in place at the same length (its last write moved away too, as the Unix
/// law does), its attributes changed (its change time put back, where a change within one tick
/// of the system clock leaves it), another file renamed over it at the same length, the file
/// removed; and a digest one reader kept never answers a reader that refuses the file.
/// NEGATIVE CONTROL (2026-11-16): `seen_of` leaving the attributes out (`attributes: 0`): `its
/// attributes changed, its change time put back`.
#[test]
fn a_digest_is_kept_while_its_file_stands_and_read_again_after_each_change() {
    const PRIVATE: Reader = Reader {
        admit: Admit::Private,
        max: 64,
    };
    fn read(path: &Path) -> crate::Result<Option<(FileStamp, [u8; 32])>> {
        PRIVATE.digest(path)
    }
    fn fresh(reader: Reader, path: &Path) -> Option<Option<[u8; 32]>> {
        reader
            .digest(path)
            .ok()
            .map(|read| read.map(|(_, digest)| digest))
    }
    fn store() -> KeptDigests {
        std::sync::Mutex::new(std::collections::VecDeque::new())
    }
    let sha = aterm_digest::Sha256::digest;
    let later = SystemTime::now() + Duration::from_secs(3600);
    /// A change a reader would see, and what makes it.
    type Change = (&'static str, fn(&Path));
    let changes: [Change; 4] = [
        ("its bytes rewritten in place at the same length", |path| {
            let file = File::options().write(true).open(path).unwrap();
            file.seek_write(b"CUSTODY", 0).unwrap();
            file.set_modified(std::time::UNIX_EPOCH).unwrap();
        }),
        ("its attributes changed, its change time put back", |path| {
            let before = change_time(path);
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(path, permissions).unwrap();
            put_back_change_time(path, before);
        }),
        ("another file renamed over it at the same length", |path| {
            let beside = path.with_extension("new");
            write_atomic(&beside, b"CUSTODY", 0o600).unwrap();
            std::fs::rename(&beside, path).unwrap();
        }),
        ("the file removed", |path| {
            std::fs::remove_file(path).unwrap()
        }),
    ];
    for (what, make) in changes {
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
        assert!(
            reads_of(&path) > reads,
            "{what}: answered with the kept digest, unread"
        );
        assert_eq!(again, fresh(PRIVATE, &path), "{what}");
        if path.exists() {
            removable(&path);
        }
    }

    let laxer = Reader {
        admit: Admit::Any,
        max: 64,
    };
    let root = scratch();
    let path = root.path().join("custody");
    write_atomic(&path, b"custody", 0o640).unwrap();
    assert!(
        !is_owner_only(&path),
        "a mode that grants others takes its folder's entries"
    );
    let kept = store();
    let admitted =
        digest_while_seen_in(&kept, None, later, &path, laxer, |path| laxer.digest(path));
    assert_eq!(admitted.unwrap(), Some(sha(b"custody")));
    let refused = digest_while_seen_in(&kept, None, later, &path, PRIVATE, |path| {
        PRIVATE.digest(path)
    });
    assert!(
        refused.is_err(),
        "a reader refusing its access list: {refused:?}"
    );
}

/// A glance's first look at a file stands only while a fresh look shows the same, and only where
/// that first look showed a regular file settled when the glance began: an untouched file stands;
/// a byte-identical copy renamed over it, an attribute change (its change time put back), a first
/// look begun a second after the file's change, a path never looked at, one first found absent
/// and a folder do not.
/// NEGATIVE CONTROL (2026-11-16): `seen_of` leaving the attributes out (`attributes: 0`): `an
/// attribute change`.
#[test]
fn a_glances_first_look_stands_only_while_a_fresh_look_shows_it_settled() {
    let root = scratch();
    let path = |name: &str| root.path().join(name);
    for name in ["untouched", "renamed", "attributes", "unsettled", "never"] {
        write_atomic(&path(name), b"control anchor", 0o600).unwrap();
    }
    std::fs::create_dir(path("folder")).unwrap();
    let seen = seen_if_present(&path("unsettled")).unwrap().unwrap();
    let changed = std::time::UNIX_EPOCH
        + Duration::from_nanos(
            u64::try_from(seen.stamp.mtime_ns.max(seen.stamp.ctime_ns)).unwrap(),
        );
    as_if_later(Duration::from_secs(3600), || {
        let glance = Glance::new();
        for name in ["untouched", "renamed", "attributes", "absent", "folder"] {
            glance.symlink_metadata(&path(name)).unwrap();
        }
        let unsettled = Glance::begun_at(changed + Duration::from_secs(1));
        unsettled.symlink_metadata(&path("unsettled")).unwrap();
        std::fs::copy(path("renamed"), path("renamed.copy")).unwrap();
        std::fs::rename(path("renamed.copy"), path("renamed")).unwrap();
        let before = change_time(&path("attributes"));
        let mut permissions = std::fs::metadata(path("attributes")).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(path("attributes"), permissions).unwrap();
        put_back_change_time(&path("attributes"), before);
        write_atomic(&path("absent"), b"made since", 0o600).unwrap();
        let stands = |glance: &Glance, name: &str| {
            let before = fresh_looks();
            let answer = glance.first_stands(&path(name));
            (answer, fresh_looks() - before)
        };
        assert_eq!(stands(&glance, "untouched"), (true, 1), "an untouched file");
        assert_eq!(
            stands(&glance, "renamed"),
            (false, 1),
            "a copy renamed over"
        );
        assert_eq!(
            stands(&glance, "attributes"),
            (false, 1),
            "an attribute change"
        );
        assert_eq!(
            stands(&unsettled, "unsettled"),
            (false, 0),
            "a first look before it settled"
        );
        assert_eq!(
            stands(&glance, "never"),
            (false, 0),
            "a path never looked at"
        );
        assert_eq!(
            stands(&glance, "absent"),
            (false, 0),
            "a path first found absent"
        );
        assert_eq!(stands(&glance, "folder"), (false, 0), "a folder");
        assert_eq!(
            stands(&glance, "untouched"),
            (true, 1),
            "CONTROL: it stands as it was"
        );
    });
    removable(&path("attributes"));
}

/// A file's id is one under each of its names and another for another file or a junction.
/// NEGATIVE CONTROL (2026-11-16): a look that follows a reparse point (`open_look` without
/// `FILE_FLAG_OPEN_REPARSE_POINT`): `a junction is itself, never its target`.
#[test]
fn file_ids_tell_one_file_from_two() {
    let root = scratch();
    let a = root.path().join("a");
    write_atomic(&a, b"x", 0o600).expect("a");
    let hard = root.path().join("hard");
    std::fs::hard_link(&a, &hard).expect("link");
    let other = root.path().join("other");
    write_atomic(&other, b"x", 0o600).expect("other");
    assert!(file_id(&a).is_some());
    assert_eq!(file_id(&a), file_id(&hard));
    assert_ne!(file_id(&a), file_id(&other));
    assert_eq!(file_id(&root.path().join("missing")), None);
    let (dir, link) = (root.path().join("dir"), root.path().join("junction"));
    std::fs::create_dir(&dir).expect("dir");
    junction(&link, &dir);
    assert!(file_id(&link).is_some());
    assert_ne!(
        file_id(&link),
        file_id(&dir),
        "a junction is itself, never its target"
    );
}

/// A wipe zeroes and removes a file, and removes a junction as itself, its folder kept.
/// NEGATIVE CONTROLS (2026-11-16): `remove_link` without its folder-link removal:
/// `link removed`; `write_all_at` writing nothing: `a handle open before the wipe reads zeros`.
#[test]
fn wipe_removes_files_and_only_the_link_of_a_link() {
    let root = scratch();
    let file = root.path().join("f");
    write_atomic(&file, b"secret", 0o600).expect("write");
    let (dir, link) = (root.path().join("dir"), root.path().join("l"));
    std::fs::create_dir(&dir).expect("dir");
    write_atomic(&dir.join("in"), b"inside", 0o600).expect("write");
    junction(&link, &dir);
    wipe_and_remove(&link).expect("link removed");
    assert_eq!(kind(&link).expect("kind"), Kind::Missing);
    assert_eq!(std::fs::read(dir.join("in")).expect("intact"), b"inside");
    let mut open = File::open(&file).expect("open");
    wipe_and_remove(&file).expect("wiped");
    assert_eq!(kind(&file).expect("kind"), Kind::Missing);
    let mut left = Vec::new();
    std::io::Read::read_to_end(&mut open, &mut left).expect("read");
    assert_eq!(left, [0u8; 6], "a handle open before the wipe reads zeros");
    wipe_and_remove(&file).expect("missing is fine");
    assert!(!wipe_if(&dir.join("in"), 64, |bytes| bytes == b"other").expect("judged"));
}

/// A new file fills only an empty name: owner-only, one link and no temporary left where it is
/// made; a file or a junction there leaves the name as it is.
/// NEGATIVE CONTROL (2026-11-16): `rename_noreplace` renaming without `NOREPLACE`: `taken`.
#[test]
fn write_new_fills_only_an_empty_name() {
    let root = scratch();
    let path = root.path().join("gap");
    assert!(write_new(&path, b"filled", 0o600).expect("written"));
    assert_eq!(std::fs::read(&path).expect("read"), b"filled");
    assert!(is_owner_only(&path));
    assert!(!write_new(&path, b"again", 0o600).expect("taken"));
    assert_eq!(std::fs::read(&path).expect("read"), b"filled");
    let (dir, link) = (root.path().join("dir"), root.path().join("junction"));
    std::fs::create_dir(&dir).expect("dir");
    junction(&link, &dir);
    assert!(!write_new(&link, b"v", 0o600).expect("taken"));
    assert_eq!(kind(&link).expect("kind"), Kind::Symlink);
    assert_eq!(
        names_in(root.path()),
        ["dir", "gap", "junction"],
        "no temp left"
    );
    let missing = root.path().join("no-dir").join("x");
    let error = write_new(&missing, b"v", 0o600).expect_err("no parent");
    assert!(error.0.contains("no-dir"), "{error}");
}

/// A write-if-changed leaves a file that holds the bytes in the mode's class as it is, and
/// replaces other bytes and a file opened to others.
/// NEGATIVE CONTROL (2026-11-16): `not_owner_only` answering `None`: `the same bytes opened to
/// others`.
#[test]
fn a_write_if_changed_leaves_the_same_bytes_in_place() {
    let root = scratch();
    let path = root.path().join("derived");
    assert!(write_atomic_if_changed(&path, b"text one", 0o600).unwrap());
    let first = stamp(&path).unwrap();
    assert!(!write_atomic_if_changed(&path, b"text one", 0o600).unwrap());
    assert_eq!(stamp(&path), Some(first), "the same bytes: the same file");
    assert!(write_atomic_if_changed(&path, b"text two", 0o600).unwrap());
    assert_ne!(
        stamp(&path).unwrap().ino,
        first.ino,
        "other bytes of the same length"
    );
    open_to_users(&path);
    assert!(
        write_atomic_if_changed(&path, b"text two", 0o600).unwrap(),
        "the same bytes opened to others"
    );
    assert!(
        is_owner_only(&path),
        "the same bytes opened to others are written again"
    );
    assert!(
        write_atomic_if_changed(&path, b"text two", 0o644).unwrap(),
        "the same bytes under a mode of another class"
    );
    assert!(!is_owner_only(&path));
    assert!(!write_atomic_if_changed(&path, b"text two", 0o644).unwrap());
}

/// A reader whose file another writer renamed a new file over while it read still holds what it
/// read: the file it opened lost its last name, and nothing else of it changed. Its change time
/// is first put an hour back, as a settled file's is.
/// NEGATIVE CONTROL (2026-11-16): `gone` answering `false` survives: NTFS moves nothing of the
/// stamp of a file another is renamed over, its change time included, so the unchanged stamp
/// alone holds this law on Windows; `gone` stands for a file system that moves the change time.
#[test]
fn a_reader_of_a_file_renamed_over_while_it_read_holds_its_read() {
    let root = scratch();
    let path = root.path().join("records");
    write_atomic(&path, b"first", 0o600).expect("write");
    put_back_change_time(&path, change_time(&path) - 3600 * 10_000_000);
    let reader = open_private_reader(&path).expect("opened").expect("there");
    write_atomic(&path, b"second", 0o600).expect("renamed over the open file");
    reader
        .check_unchanged()
        .expect("the file it read, unlinked, stands");
    assert_eq!(reader.read_private(64).expect("read").as_slice(), b"first");
    let appended = open_private_reader(&path).expect("opened").expect("there");
    let now = stamp(&path).expect("stamped");
    append_at(&path, &now, b"!").expect("appended");
    assert!(appended.check_unchanged().is_err(), "an append is a change");
}

/// A private folder is made owner-only with its parents, and an elevated process is refused.
#[test]
fn a_private_dir_is_made_owner_only() {
    let root = scratch();
    let dir = root.path().join("a").join("secrets");
    ensure_private_dir(&dir).expect("made");
    crate::fsx::private_dir_settled(&dir).expect("settled");
    let (real, link) = (root.path().join("real"), root.path().join("link"));
    std::fs::create_dir(&real).expect("dir");
    junction(&link, &real);
    assert!(ensure_private_dir(&link).is_err(), "a junction is refused");
}
