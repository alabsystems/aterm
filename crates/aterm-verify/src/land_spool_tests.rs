// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::checks::Check;
use crate::land_record::tests::{BASE, DAY, NOW, T, record, signer};

#[test]
fn interrupted_spool_cleanup_keeps_the_unpublished_record_visible() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let spool = Dir::at(tmp.path().join("spool"));
    let me = signer(7);
    let mut record = record(Check::Fmt, Word::Ok, T, BASE, NOW);
    let appended = record.sign(Some(&me)).unwrap();
    record.run.id = "newer".into();
    let newer = record.sign(Some(&me)).unwrap();
    spool.put(&newer).unwrap();
    use std::sync::atomic::{AtomicBool, Ordering};
    let excluded = AtomicBool::new(false);
    let stopped = std::panic::catch_unwind(|| {
        spool.done_with(&appended, |_| {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(spool.path().join(LOCK))
                .unwrap();
            excluded.store(
                matches!(
                    aterm_toolchain::record_lock::RecordLock::try_lock(file, 0),
                    Err(std::fs::TryLockError::WouldBlock)
                ),
                Ordering::Relaxed,
            );
            panic!("cleanup stopped before comparison")
        })
    });
    assert!(stopped.is_err());
    assert!(
        excluded.load(Ordering::Relaxed),
        "another writer cannot enter during comparison"
    );
    assert_eq!(
        spool.pending().unwrap().records.as_slice(),
        std::slice::from_ref(&newer)
    );
    spool.done(&appended).unwrap();
    assert_eq!(
        spool.pending().unwrap().records.as_slice(),
        std::slice::from_ref(&newer)
    );
    spool.done(&newer).unwrap();
    assert_eq!(spool.pending().unwrap(), Pending::default());
}

/// A spooled record reads back byte for byte at its journal path until `done` forgets it, in name
/// order; a FAIL put again at its path keeps its first record while a pass takes the newer one;
/// a file no record's name spells (an atomic write's temporary, a stray) is no record; a put
/// expires every kept record whose day the journal's prune would drop; and a clone's spool lives in
/// its git common directory, shared by its worktrees. NEGATIVE CONTROL, run as a mutant that made
/// this test fail: let `put` replace a kept FAIL (the second FAIL's bytes are read back).
#[test]
fn a_spooled_record_stays_until_an_append_holds_it() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let spool = Dir::at(tmp.path().join("spool"));
    assert_eq!(spool.pending().unwrap(), Pending::default(), "no spool yet");
    let me = signer(7);
    let signed = |check, word, ended: u64, id: &str| {
        let mut r = record(check, word, T, BASE, ended);
        r.run.id = id.into();
        r.sign(Some(&me)).unwrap()
    };
    let fail = signed(Check::Lint, Word::Fail, NOW, "first");
    let ok = signed(Check::Fmt, Word::Ok, NOW, "first");
    spool.put(&fail).unwrap();
    spool.put(&ok).unwrap();
    spool
        .put(&signed(Check::Lint, Word::Fail, NOW, "second"))
        .unwrap();
    let newer = signed(Check::Fmt, Word::Ok, NOW, "second");
    spool.put(&newer).unwrap();
    for stray in [".fmt.tmp.1.0123abcd", "README"] {
        std::fs::write(spool.path().join(stray), b"not a record").unwrap();
    }
    let mut want = vec![fail.clone(), newer.clone()];
    want.sort_by_key(|signed| file_of(&signed.path).unwrap());
    assert_eq!(
        spool.pending().unwrap(),
        Pending {
            records: want,
            unreadable: Vec::new()
        }
    );
    assert_eq!(
        path_of(&file_of(&fail.path).unwrap()),
        Some(fail.path.clone())
    );

    spool.done(&newer).unwrap();
    spool.done(&newer).unwrap();
    assert_eq!(
        spool.pending().unwrap().records,
        std::slice::from_ref(&fail)
    );

    let corrupt = Dir::at(tmp.path().join("corrupt-spool"));
    std::fs::create_dir_all(corrupt.path()).unwrap();
    let file = corrupt.path().join(file_of(&fail.path).unwrap());
    std::fs::write(&file, b"not a record").unwrap();
    assert!(
        corrupt.put(&fail).is_err(),
        "presence is not proof of a kept FAIL"
    );
    assert_eq!(std::fs::read(file).unwrap(), b"not a record");

    let later = signed(Check::Test, Word::Ok, (DAY + 3) * 86_400, "later");
    spool.put(&later).unwrap();
    assert_eq!(
        spool.pending().unwrap().records,
        [later],
        "a put on day {} expires day {DAY}",
        DAY + 3
    );

    let clone = tmp.path().join("clone");
    let git = |dir: &Path, args: &[&str]| crate::judged::git(dir, args).unwrap();
    git(tmp.path(), &["init", "--quiet", "clone"]);
    git(
        &clone,
        &[
            "-c",
            "user.name=Andrew Yates",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    git(&clone, &["worktree", "add", "--quiet", "../worktree"]);
    let at = Dir::of_clone(&clone).unwrap().path().to_owned();
    assert!(
        at.ends_with("aterm-verify/.ubd/land-pending"),
        "{}",
        at.display()
    );
    let common = at
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap();
    assert_eq!(
        std::fs::canonicalize(common).unwrap(),
        std::fs::canonicalize(clone.join(".git")).unwrap(),
        "the spool is in the clone's git common directory"
    );
    assert_eq!(
        Dir::of_clone(&tmp.path().join("worktree")).unwrap().path(),
        at,
        "and its worktrees share it"
    );
}

/// A run record spools as `runs.<day>.<run>.<seq>.<kind>.toml`, a name that reads back as its
/// journal path and that no verdict record's name spells; it replaces the record kept at its path
/// whatever its result, as no verdict FAIL does; and it keeps its kind's retention, 14 days: a put
/// three days on expires the verdict record of its day and keeps it, and a put fifteen days on
/// expires it while it keeps the run record of three days on. NEGATIVE CONTROLS, each run as a
/// mutant that made this test fail: [`kept_as`] expiring a run record by a verdict's retention
/// (it read as expired three days on); a run record's name taking an ordinal that is not
/// canonical decimal (`04` read as a record's name).
#[test]
fn a_spooled_run_record_files_by_its_kind() {
    use crate::land_run::Kind;
    use crate::land_run::tests::{RUN, record as run_record};
    let tmp = aterm_tempfile::tempdir().unwrap();
    let spool = Dir::at(tmp.path().join("spool"));
    let me = signer(7);
    let end = |result: &str, at: u64| {
        crate::land_run::Record {
            result: result.to_owned(),
            at,
            ..run_record(Kind::End, 4)
        }
        .sign(Some(&me))
        .unwrap()
    };
    let first = end("not-landed", NOW);
    let file = file_of(&first.path).unwrap();
    assert_eq!(file, format!("runs.{DAY}.{RUN}.4.end.toml"));
    assert_eq!(path_of(&file), Some(first.path.clone()));
    for stray in [
        format!("runs.{DAY}.{RUN}.4.verdict.toml"),
        format!("runs.{DAY}.{RUN}.04.end.toml"),
        format!("runs.x.{RUN}.4.end.toml"),
        format!("{DAY}.{RUN}.4.end.toml"),
    ] {
        assert_eq!(path_of(&stray), None, "{stray}");
    }
    assert_eq!(kept_as(&first.path, DAY + 3), Ok((DAY, false, false)));
    assert_eq!(kept_as(&first.path, DAY + 15), Ok((DAY, true, false)));

    let verdict = record(Check::Lint, crate::land_record::Word::Fail, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    spool.put(&verdict).unwrap();
    spool.put(&first).unwrap();
    let again = end("fail", NOW);
    spool.put(&again).unwrap();
    let mut want = vec![verdict, again.clone()];
    want.sort_by_key(|signed| file_of(&signed.path).unwrap());
    assert_eq!(
        spool.pending().unwrap().records,
        want,
        "a run record replaces"
    );

    let later = end("landed", (DAY + 3) * 86_400);
    spool.put(&later).unwrap();
    assert_eq!(
        spool.pending().unwrap().records,
        [again, later.clone()],
        "a put on day {} expires the verdict record of day {DAY}, not the run record",
        DAY + 3
    );
    let latest = end("landed", (DAY + 15) * 86_400);
    spool.put(&latest).unwrap();
    assert_eq!(
        spool.pending().unwrap().records,
        [later, latest],
        "a put on day {} expires day {DAY}'s run record and keeps day {}'s",
        DAY + 15,
        DAY + 3
    );
}

/// `done` FORGETS A RECORD ONLY WHILE ITS PATH HOLDS THOSE BYTES: a pass a later put replaced
/// (a concurrent landing of the clone, between an append's read of the spool and its `done`)
/// stays for the next append, and goes once `done` names its own bytes; a FAIL whose first
/// record the spool keeps is not forgotten by another FAIL's bytes at its path; a path nothing
/// holds is forgotten as done; and nothing is left under a name a reader takes. A file named as
/// a record that does not read (here one past [`crate::land_record::MAX_BYTES`]) is listed as
/// unreadable, with why, and stays. NEGATIVE CONTROLS, each run once as a mutant that made this
/// test fail: `done` removing by path whatever its bytes (the newer pass was gone); `pending`
/// passing over a file it could not read (nothing was listed unreadable).
#[test]
fn done_forgets_a_record_only_while_its_path_holds_its_bytes() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let spool = Dir::at(tmp.path().join("spool"));
    let me = signer(7);
    let signed = |check, word, id: &str| {
        let mut r = record(check, word, T, BASE, NOW);
        r.run.id = id.into();
        r.sign(Some(&me)).unwrap()
    };
    let appended = signed(Check::Fmt, Word::Ok, "first");
    let newer = signed(Check::Fmt, Word::Ok, "second");
    assert_eq!(appended.path, newer.path);
    assert_ne!(appended.bytes, newer.bytes);
    spool.put(&appended).unwrap();
    spool.put(&newer).unwrap();
    spool.done(&appended).unwrap();
    assert_eq!(
        spool.pending().unwrap().records,
        std::slice::from_ref(&newer),
        "the newer pass stays"
    );
    spool.done(&newer).unwrap();
    assert_eq!(spool.pending().unwrap(), Pending::default());
    spool.done(&newer).unwrap();

    let fail = signed(Check::Lint, Word::Fail, "first");
    let other = signed(Check::Lint, Word::Fail, "second");
    spool.put(&fail).unwrap();
    spool.done(&other).unwrap();
    assert_eq!(
        spool.pending().unwrap().records,
        std::slice::from_ref(&fail),
        "the first FAIL stays"
    );

    let big = signed(Check::Test, Word::Ok, "big");
    let file = spool.path().join(file_of(&big.path).unwrap());
    let max = usize::try_from(crate::land_record::MAX_BYTES).unwrap();
    std::fs::write(&file, vec![b' '; max + 1]).unwrap();
    let pending = spool.pending().unwrap();
    assert_eq!(pending.records, std::slice::from_ref(&fail));
    let [(unread, why)] = pending.unreadable.as_slice() else {
        panic!("one unreadable file: {pending:?}");
    };
    assert_eq!(unread, &file);
    assert!(!why.is_empty());
    assert!(spool.done(&big).is_err());
    assert_eq!(spool.pending().unwrap(), pending);
    let mut names: Vec<String> = std::fs::read_dir(spool.path())
        .unwrap()
        .map(|item| item.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    let mut want = vec![
        LOCK.to_owned(),
        file_of(&fail.path).unwrap(),
        file_of(&big.path).unwrap(),
    ];
    want.sort();
    assert_eq!(names, want, "only records and the stable lock remain");
}
