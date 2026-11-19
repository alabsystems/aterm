// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a run that reads through `fsx` records in a trace scope (`aterm_uds::trace`): the stamp of
//! each file a read or a look took its answer from, or that nothing was there; a standing, which
//! the folder's times do not move, of a folder judged by its identity; a glance's look given
//! again; each folder a walk lists, by the names it holds; a read that failed as a retry; and each
//! write here as a write. Every law holds on both arms; where it opens a file or folder to other
//! users, a Unix mode stands for what a Windows access list does.

use super::*;
use aterm_uds::trace::{self, Moved, Now, Trace, Unkept};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How far past the clock the traced reads here are judged settled ([`trace::read_as_if_later`]):
/// past the settle window, so a file this test just wrote is read as one written long before.
const LATER: Duration = Duration::from_secs(10);

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::tempdir().expect("tempdir")
}

/// A `0600` file `name` in `dir` holding `bytes`.
fn private_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    write_atomic(&path, bytes, 0o600).unwrap();
    path
}

/// `path` opened to other users: on Unix of mode 0644 (a folder 0755); on Windows a read grant to
/// every user added to its protected list.
fn open_to_others(path: &Path) {
    #[cfg(unix)]
    {
        let mode = if path.is_dir() { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(windows)]
    owner_only_and(path, "(A;;FR;;;BU)");
}

/// `path` the owner's alone again: on Unix of mode 0600 (a folder 0700); on Windows under the
/// protected list of the user and SYSTEM alone.
fn private_again(path: &Path) {
    #[cfg(unix)]
    {
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(windows)]
    owner_only_and(path, "");
}

/// `path` given the protected list of the user and SYSTEM (a folder's entries passed on), and
/// `more` (SDDL access entries), its owner left as it is.
#[cfg(windows)]
fn owner_only_and(path: &Path, more: &str) {
    use aterm_uds::acl::{Descriptor, Parts, Sid};
    let me = Sid::token_user().expect("the token's user").to_string();
    let inherit = if path.is_dir() { "OICI" } else { "" };
    Descriptor::from_sddl(&format!(
        "D:P(A;{inherit};FA;;;{me})(A;{inherit};FA;;;SY){more}"
    ))
    .and_then(|descriptor| descriptor.apply_to_path(path, Parts::DACL | Parts::PROTECTED_DACL))
    .expect("the list written");
}

/// What `f` returned and the trace it recorded, its reads judged settled ([`LATER`]).
fn traced<T>(f: impl FnOnce() -> T) -> (T, Trace) {
    trace::read_as_if_later(LATER, || trace::scope(f))
}

/// Whether `trace` stands now.
fn stands(trace: &Trace) -> Result<(), Moved> {
    trace.stands(Now::at_now(), Instant::now())
}

/// Whether `trace` recorded `path` with a stamp (`true`) or as nothing there (`false`).
fn recorded(trace: &Trace, path: &Path) -> Option<bool> {
    trace
        .files()
        .find(|(at, _)| *at == path)
        .map(|(_, stamp)| stamp.is_some())
}

/// A reader by its name: what it is called, and a read of a path through it.
type Named = (&'static str, fn(&Path));

/// The readers here a run reads a file's bytes or its stamp through.
const READERS: [Named; 10] = [
    ("read_private", |path| drop(read_private(path, 64).unwrap())),
    ("read_owned", |path| drop(read_owned(path, 64).unwrap())),
    ("read_owned_head", |path| {
        drop(read_owned_head(path, 2).unwrap());
    }),
    ("read_public", |path| drop(read_public(path, 64).unwrap())),
    ("read_aterm_config", |path| {
        drop(read_aterm_config(path, 64).unwrap());
    }),
    ("read_owned_stamped", |path| {
        drop(read_owned_stamped(path, 64).unwrap());
    }),
    ("open_private_reader", |path| {
        drop(open_private_reader(path).unwrap());
    }),
    ("private_digest", |path| {
        let _ = private_digest(path, 64, None).unwrap();
    }),
    ("stamp", |path| {
        let _ = stamp(path);
    }),
    ("seen_if_present", |path| {
        let _ = seen_if_present(path).unwrap();
    }),
];

/// Each reader here records the stamp of the file it read, or that nothing was there, so a
/// replay of the trace sees a file renamed over it, or one made where there was none.
/// NEGATIVE CONTROLS (2026-11-14): with `open_admitted` recording nothing (its
/// `trace::fs::opening` removed), `read_private` leaves no record (`read_private records the
/// file it read`); with `lstat_now` and `kind_and_stamp_if_present` taking std's `lstat`,
/// `stamp` leaves none (`stamp records the file it read`).
#[test]
fn each_reader_records_the_file_it_read_and_a_miss() {
    let root = scratch();
    for (name, read) in READERS {
        let file = private_file(root.path(), name, b"held");
        let missing = root.path().join(format!("{name}.missing"));
        let ((), read_trace) = traced(|| read(&file));
        let ((), miss_trace) = traced(|| read(&missing));
        assert_eq!(
            recorded(&read_trace, &file),
            Some(true),
            "{name} records the file it read"
        );
        assert_eq!(
            recorded(&miss_trace, &missing),
            Some(false),
            "{name} records that nothing was there"
        );
        assert_eq!(read_trace.unkept(), None, "{name}: a read is kept");
        assert_eq!(stands(&read_trace), Ok(()), "{name}: nothing moved");
        assert_eq!(stands(&miss_trace), Ok(()), "{name}: nothing came");

        write_atomic(&file, b"another", 0o600).unwrap();
        write_atomic(&missing, b"made", 0o600).unwrap();
        assert_eq!(
            stands(&read_trace),
            Err(Moved::File(file.clone())),
            "{name}: a file renamed over the one read moves it"
        );
        assert_eq!(
            stands(&miss_trace),
            Err(Moved::File(missing.clone())),
            "{name}: a file made where none was moves it"
        );
    }
}

/// A journal's witness records its folder and the three files a read of it opens, so an append
/// moves what a replay looks at.
/// NEGATIVE CONTROL (2026-11-14): with `lstat_now` and `kind_and_stamp_if_present` taking std's
/// `lstat`, the witness leaves no record of its folder (`the witness records its folder and each
/// file`).
#[test]
fn a_journal_witness_records_its_folder_and_each_file() {
    let root = scratch();
    let dir = root.path().join("journal");
    ensure_private_dir(&dir).unwrap();
    let files = aterm_journal::committed_files(&dir, "table");
    for file in [&files.anchor, &files.checkpoint, &files.astream] {
        write_atomic(file, b"held", 0o600).unwrap();
    }
    let (seen, trace) = traced(|| JournalSeen::of(&dir, "table"));
    assert!(seen.is_some(), "a journal whose files are all there");
    for path in [&dir, &files.anchor, &files.checkpoint, &files.astream] {
        assert_eq!(
            recorded(&trace, path),
            Some(true),
            "the witness records its folder and each file: {}",
            path.display()
        );
    }
    assert_eq!(stands(&trace), Ok(()));
    let mut astream = std::fs::OpenOptions::new()
        .append(true)
        .open(&files.astream)
        .unwrap();
    astream.write_all(b" and more").unwrap();
    assert_eq!(
        stands(&trace),
        Err(Moved::File(files.astream.clone())),
        "an append moves it"
    );
}

/// A folder judged by its identity, owner and mode (on Windows its access list) records a standing
/// of what it judges, and files written in it, as the vault's subscriber writes `state.toml`
/// beside the vault's files, move none of it, while a stamp of the same folder moves with its
/// times. The folder opened to others moves the standing, [`folder_in`]'s own as well as
/// [`private_dir_settled`]'s.
/// NEGATIVE CONTROL (2026-11-14): with `folder_in` taking its `lstat` through `lstat_in`, whose
/// `lstat` records the folder's stamp, the folder's trace moves at the files written in it
/// (`files written in a folder move none of what is judged of it`).
/// NEGATIVE CONTROL (2026-11-16): with the Unix `folder_digest` holding no mode, the trace of
/// `folder_in` alone stands across the folder opened to others (`a folder opened to others moves
/// what folder_in judged of it`: left `Ok(())`), while the trace of every look still moves at
/// `private_dir_settled`'s standing.
/// NEGATIVE CONTROL (2026-11-16, on macOS): with `open_to_others` changing nothing, `a folder
/// opened to others moves it` failed, left `Ok(())`, right `Err(Standing(".../vault"))`.
#[test]
fn a_folder_judged_by_its_identity_stands_while_files_are_written_in_it() {
    let root = scratch();
    let dir = root.path().join("vault");
    ensure_private_dir(&dir).unwrap();
    let glance = Glance::new();
    let ((), judged) = traced(|| {
        assert!(folder_in(None, &dir).unwrap().is_some());
        assert!(folder_in(Some(&glance), &dir).unwrap().is_some());
        assert_eq!(kind(&dir).unwrap(), Kind::Dir);
        assert!(kind_and_id(&dir).unwrap().1.is_some());
        assert!(file_id(&dir).is_some());
        private_dir_settled(&dir).unwrap();
    });
    let ((), folder) = traced(|| assert!(folder_in(None, &dir).unwrap().is_some()));
    let (_, stamped) = traced(|| stamp(&dir));
    assert_eq!(judged.unkept(), None);
    assert_eq!(folder.unkept(), None);

    let state = dir.join("state.toml");
    write_atomic(&state, b"heard = 1\n", 0o600).unwrap();
    write_atomic(&state, b"heard = 2\n", 0o600).unwrap();
    assert_eq!(
        stands(&judged),
        Ok(()),
        "files written in a folder move none of what is judged of it"
    );
    assert_eq!(
        stands(&folder),
        Ok(()),
        "files written in a folder move none of what folder_in judged of it"
    );
    assert_eq!(
        stands(&stamped),
        Err(Moved::File(dir.clone())),
        "its stamp moves with its times"
    );
    assert_eq!(recorded(&judged, &dir), None, "no stamp of the folder");
    assert!(judged.standings().any(|at| at == dir));

    open_to_others(&dir);
    assert_eq!(
        stands(&judged),
        Err(Moved::Standing(dir.clone())),
        "a folder opened to others moves it"
    );
    assert_eq!(
        stands(&folder),
        Err(Moved::Standing(dir.clone())),
        "a folder opened to others moves what folder_in judged of it"
    );
}

/// A glance records each look it gives again: the scope that heard its first look may not be the
/// one armed now. One whose file changed after that first look records a retry, since the run took
/// an answer from before the change.
/// NEGATIVE CONTROL (2026-11-14): with `Glance::symlink_metadata` giving a kept look without
/// `retrace`, the second scope holds no record of the file (`a look given again is recorded`).
#[test]
fn a_glance_records_each_look_it_gives_again() {
    let root = scratch();
    let file = private_file(root.path(), "a", b"one");
    let glance = Glance::new();
    assert!(glance.symlink_metadata(&file).unwrap().is_some());
    let (_, again) = traced(|| glance.symlink_metadata(&file).unwrap());
    assert_eq!(
        recorded(&again, &file),
        Some(true),
        "a look given again is recorded"
    );
    assert_eq!(again.unkept(), None);

    write_atomic(&file, b"two", 0o600).unwrap();
    let (_, changed) = traced(|| glance.symlink_metadata(&file).unwrap());
    assert!(
        matches!(changed.unkept(), Some(Unkept::Retry(_))),
        "a look given after its file changed is not kept: {:?}",
        changed.unkept()
    );
}

/// A read that fails for a reason no stamp decides records a retry, while one refused for what
/// its stamp shows (a file others may read: on Unix its mode, on Windows its access list, which
/// the stamp's digest and change time hold) records none: the refusal stands until the file
/// changes, and the file made the owner's alone again moves it. On Windows a private file has one
/// name too: one refused for a second name stands until that name is removed. And there each
/// change moves the stamp by what it changed, not only by the change time NTFS moves with it:
/// with the change time put back, the narrowed list still moves it (the stamp's digest of the
/// owner and list), and so does the second name removed (its link count).
/// NEGATIVE CONTROL (2026-11-14): with `open_admitted`'s `failed` recording nothing, the failed
/// read is kept (`a failed read records a retry`).
/// NEGATIVE CONTROLS (2026-11-16, on macOS): with `private_again` changing nothing, `its access
/// narrowed moves it` failed, left `Ok(())`, right `Err(File(".../open"))`; with `open_to_others`
/// changing nothing, `a file others may read is refused` failed.
/// NEGATIVE CONTROLS (2026-11-18, on Windows), each in `aterm_uds::trace`'s Windows `Meta::of`:
/// with the owner and list's digest left out (`uid: 0`), `its access narrowed moves it, its
/// change time put back` failed, left `Ok(())`, right `Err(File(".../open"))`, while `its access
/// narrowed moves it` held by the change time alone; with the link count left out (`nlink: 0`),
/// `its second name removed moves it, its change time put back` failed, left `Ok(())`, right
/// `Err(File(".../named"))`.
#[test]
fn a_read_that_fails_records_a_retry_and_a_refusal_none() {
    let root = scratch();
    let file = private_file(root.path(), "a", b"held");
    fail_reads(&file, 1);
    let (read, failed) = traced(|| read_private(&file, 64));
    assert!(read.is_err());
    assert!(
        matches!(failed.unkept(), Some(Unkept::Retry(_))),
        "a failed read records a retry: {:?}",
        failed.unkept()
    );

    let open = private_file(root.path(), "open", b"held");
    open_to_others(&open);
    let (read, refused) = traced(|| read_private(&open, 64));
    assert!(read.is_err(), "a file others may read is refused");
    assert_eq!(refused.unkept(), None, "a refusal records no retry");
    assert_eq!(
        stands(&refused),
        Ok(()),
        "a refusal stands while the file does"
    );
    #[cfg(windows)]
    let refused_at = change_time(&open);
    private_again(&open);
    assert_eq!(
        stands(&refused),
        Err(Moved::File(open.clone())),
        "its access narrowed moves it"
    );
    #[cfg(windows)]
    {
        put_back_change_time(&open, refused_at);
        assert_eq!(
            stands(&refused),
            Err(Moved::File(open.clone())),
            "its access narrowed moves it, its change time put back"
        );

        let named = private_file(root.path(), "named", b"held");
        let second = root.path().join("named.second");
        std::fs::hard_link(&named, &second).unwrap();
        let (read, refused) = traced(|| read_private(&named, 64));
        assert!(read.is_err(), "a file of two names is refused");
        assert_eq!(refused.unkept(), None, "a refusal records no retry");
        let refused_at = change_time(&named);
        std::fs::remove_file(&second).unwrap();
        assert_eq!(
            stands(&refused),
            Err(Moved::File(named.clone())),
            "its second name removed moves it"
        );
        put_back_change_time(&named, refused_at);
        assert_eq!(
            stands(&refused),
            Err(Moved::File(named.clone())),
            "its second name removed moves it, its change time put back"
        );
    }
}

/// The change time of the file at `path`, in filetime ticks.
#[cfg(windows)]
fn change_time(path: &Path) -> i64 {
    aterm_dirfd::fstat(std::fs::File::open(path).unwrap())
        .unwrap()
        .change_time
}

/// Put the change time of the file at `path` back to `ticks`, as any writer of its attributes
/// may: a change made within a tick of the system clock of the last one leaves it there too.
#[cfg(windows)]
fn put_back_change_time(path: &Path, ticks: i64) {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    let file = std::fs::File::options()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .open(path)
        .unwrap();
    aterm_dirfd::seam::set_change_time(&file, ticks).unwrap();
    assert_eq!(change_time(path), ticks, "the change time put back");
}

/// Each write here records a write, the ones `aterm_uds::atomic_file` makes for it and its own;
/// a write that would leave a file as it is records none, and the stamp of the file it judged.
/// NEGATIVE CONTROLS (2026-11-14): with `append_at_with`'s `wrote` removed, an append is kept
/// (`append_at records a write`); with `write_atomic_if_changed` not recording the file's stamp,
/// a change to the file left as it was moves nothing (`a change to the file it left moves it`).
#[test]
fn each_write_records_a_write_and_one_left_undone_its_file() {
    let root = scratch();
    let dir = root.path();
    let written = |name: &str, write: &dyn Fn()| {
        let ((), trace) = traced(write);
        assert!(
            trace.effects().any(|effect| effect == trace::Effect::Write),
            "{name} records a write"
        );
    };
    let log = private_file(dir, "log", b"one");
    let at = stamp(&log).unwrap();
    written("append_at", &|| {
        let _ = append_at(&log, &at, b" two").unwrap();
    });
    written("write_atomic", &|| {
        write_atomic(&dir.join("w"), b"x", 0o600).unwrap();
    });
    written("write_new", &|| {
        assert!(write_new(&dir.join("n"), b"x", 0o600).unwrap());
    });
    let from = private_file(dir, "from", b"x");
    written("rename_noreplace", &|| {
        rename_noreplace(&from, &dir.join("to")).unwrap();
    });
    let doomed = private_file(dir, "doomed", b"x");
    written("wipe_and_remove", &|| wipe_and_remove(&doomed).unwrap());
    written("ensure_private_dir", &|| {
        ensure_private_dir(&dir.join("made")).unwrap();
    });
    written("open_lock", &|| {
        drop(open_lock(&dir.join("made.lock")).unwrap())
    });

    let held = private_file(dir, "held", b"held");
    let (wrote, left) = traced(|| write_atomic_if_changed(&held, b"held", 0o600).unwrap());
    assert!(!wrote, "the same bytes leave the file");
    assert_eq!(left.unkept(), None, "a write left undone is kept");
    let ((), taken) = traced(|| drop(open_lock(&dir.join("made.lock")).unwrap()));
    assert_eq!(
        taken.unkept(),
        None,
        "a lock taken in its own file writes nothing"
    );
    write_atomic(&held, b"other", 0o600).unwrap();
    assert_eq!(
        stands(&left),
        Err(Moved::File(held.clone())),
        "a change to the file it left moves it"
    );
}

/// A tree's walk records each folder it lists, the names it holds and the kind each names, so a
/// replay of the trace sees an entry made in a folder below the root, which the root's stamp does
/// not hold, while a file there renamed over under its own name moves nothing.
/// NEGATIVE CONTROL (2026-11-16): with `tree_paths` listing each folder through std's
/// `read_dir`, a file made in the folder below the root leaves the trace standing (`an entry
/// made below the root moves it`: left `Ok(())`, right `Err(Listing(".../tree/below"))`).
#[test]
fn a_tree_walk_records_each_folder_it_lists() {
    let root = scratch();
    let tree = root.path().join("tree");
    let below = tree.join("below");
    ensure_private_dir(&below).unwrap();
    let held = private_file(&below, "held", b"one");
    let (listed, trace) = traced(|| tree_paths(&tree, &[]));
    assert_eq!(
        listed,
        Some(vec![tree.clone(), below.clone(), held.clone()])
    );
    assert_eq!(trace.unkept(), None, "a walk of a tree is kept");
    write_atomic(&held, b"two", 0o600).unwrap();
    assert_eq!(
        stands(&trace),
        Ok(()),
        "a file renamed over under its own name moves nothing"
    );
    private_file(&below, "made", b"new");
    assert_eq!(
        stands(&trace),
        Err(Moved::Listing(below.clone())),
        "an entry made below the root moves it"
    );
}
