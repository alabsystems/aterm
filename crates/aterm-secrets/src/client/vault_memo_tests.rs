// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::fsx;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A test's wait for what must happen: a hang detector, not a latency budget.
const HANG: Duration = Duration::from_secs(60);

fn signed_denial(home: &std::path::Path, label: &str, denied: &str) -> Paths {
    let paths = Paths::for_home(home);
    let source = home.join("fixture.vault");
    crate::client::create(&paths, source.to_str().unwrap(), None, Some(label)).unwrap();
    let admin = crate::client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &crate::text::Audience::Everyone,
        crate::client::SWARM_ROSTER_ENTRY,
        format!("deny_nodes = [\"{denied}\"]\n").as_bytes(),
    )
    .unwrap();
    crate::client::admin_publish(&paths, &admin, &doc).unwrap();
    paths
}

#[test]
fn a_typed_denial_memo_recovers_after_busy_and_failed_reads() {
    use crate::client::{self, SwarmDenialSnapshot as Snapshot, SwarmDenialUnavailable as Why};
    struct FailedReads(PathBuf);
    impl Drop for FailedReads {
        fn drop(&mut self) {
            fsx::fail_reads(&self.0, 0);
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = signed_denial(root.path(), "m60", "m99");
    // Keep this fault-injected observation independent of the process-wide memo's
    // bounded eviction as other tests read their own vault folders concurrently.
    let memo = Memo::<Snapshot>::new(inputs);
    let now = || memo.read(&paths, client::swarm_denial_snapshot);
    let expected = Snapshot::Available(vec!["m99".to_owned()]);
    assert_eq!(now().unwrap(), expected);
    let failed = FailedReads(paths.cache_file());
    fsx::fail_reads(&failed.0, usize::MAX);
    assert!(
        client::swarm_denial_snapshot(&paths).is_err(),
        "the read fault is active"
    );
    assert_eq!(
        now().unwrap().into_nodes(),
        Some(vec!["m99".to_owned()]),
        "the listener projection preserves the typed memo's settled read"
    );
    drop(failed);

    let original = client::load_state(&paths).unwrap();
    let mut revoked = original.clone();
    revoked.membership = client::Stored::Revoked;
    client::save_state(&paths, &revoked).unwrap();
    assert_eq!(now().unwrap(), Snapshot::Unavailable(Why::Revoked));
    assert_eq!(now().unwrap().into_nodes(), None);
    client::save_state(&paths, &original).unwrap();
    assert_eq!(now().unwrap(), expected);

    // A state rewritten with what it held but the time it asked the vault (which no deny
    // list is read from) moves the memo's stamps, as a round that heard nothing new does not.
    let mut asked = original.clone();
    let writer = StateLock::take(&paths).unwrap();
    asked.requested += 1;
    client::save_state(&paths, &asked).unwrap();
    assert_eq!(now().unwrap(), Snapshot::Unavailable(Why::WriterBusy));
    drop(writer);
    assert_eq!(
        now().unwrap(),
        expected,
        "a busy observation is not kept after its writer leaves"
    );

    let failed = FailedReads(paths.cache_file());
    fsx::fail_reads(&failed.0, usize::MAX);
    asked.requested += 1;
    client::save_state(&paths, &asked).unwrap();
    assert!(now().is_err());
    drop(failed);
    assert_eq!(
        now().unwrap(),
        expected,
        "an I/O failure does not poison the memo until another write"
    );
}

#[test]
fn typed_denial_memo_keeps_vaults_separate_and_refuses_an_unapplied_source() {
    use crate::client::{self, SwarmDenialSnapshot as Snapshot};
    let root = aterm_tempfile::tempdir().unwrap();
    let one = signed_denial(&root.path().join("one"), "m60", "m99");
    let two = signed_denial(&root.path().join("two"), "m61", "m98");
    let first = Snapshot::Available(vec!["m99".to_owned()]);
    let second = Snapshot::Available(vec!["m98".to_owned()]);
    assert_eq!(swarm_denial_snapshot_now(&one).unwrap(), first);
    assert_eq!(swarm_denial_snapshot_now(&two).unwrap(), second);
    let original = client::config(&one).unwrap().unwrap();
    client::save_config(&one, &client::config(&two).unwrap().unwrap()).unwrap();
    assert!(
        !matches!(swarm_denial_snapshot_now(&one), Ok(Snapshot::Available(_))),
        "a replacement source cannot inherit the old source's verified list"
    );
    assert!(!matches!(swarm_denied_nodes_now(&one), Ok(Some(_))));
    assert_eq!(swarm_denial_snapshot_now(&two).unwrap(), second);
    client::save_config(&one, &original).unwrap();
    assert_eq!(swarm_denial_snapshot_now(&one).unwrap(), first);
}

/// A reader standing in for the vault's: the lines of the vault folder's `records`.
fn lines(paths: &Paths, reads: &AtomicUsize) -> crate::Result<Option<Vec<String>>> {
    reads.fetch_add(1, Ordering::SeqCst);
    let text = std::fs::read_to_string(paths.records_data_file()?)?;
    Ok(Some(text.lines().map(str::to_owned).collect()))
}

fn vault(home: &std::path::Path) -> Paths {
    let paths = Paths::for_home(home);
    fsx::ensure_private_dir(&paths.dir).unwrap();
    fsx::write_atomic(&paths.records_file(), b"m60\n", 0o600).unwrap();
    paths
}

/// A memo gives what it kept only under the record bounds it was read under
/// (`record_bounds`): under a test's own budgets or byte bound (`RecordBudgets::within`,
/// `RecordBudgets::within_bytes`) the same files recover to other records, so a read under
/// other bounds reads again.
///
/// NEGATIVE CONTROLS (2026-11-09): with `Memo::kept` not matching the bounds, the read under
/// smaller budgets is given the answer kept under the production ones (`read again under
/// other budgets`); with `record_bounds` leaving out the byte bound, so is the read under a
/// smaller one (`read again under another byte bound`).
#[test]
fn a_memo_reads_again_under_other_record_budgets() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = vault(root.path());
    let memo = Memo::new(inputs);
    let reads = AtomicUsize::new(0);
    let read = || memo.read(&paths, |p| lines(p, &reads)).unwrap();
    assert_eq!(read(), Some(vec!["m60".to_owned()]));
    assert_eq!(read(), Some(vec!["m60".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 1, "read once");
    let small = crate::membership_log::RecordBudgets {
        weighed: 2,
        keyed: 2,
        held: 4,
    };
    small.within(|| {
        read();
        read();
    });
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "read again under other budgets"
    );
    read();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "and again under the production ones"
    );
    crate::membership_log::RecordBudgets::within_bytes(1 << 20, || {
        read();
        read();
    });
    assert_eq!(
        reads.load(Ordering::SeqCst),
        4,
        "read again under another byte bound"
    );
}

/// An exclusive take of the state lock on a thread under its own look ([`VaultLook`]), one of
/// its own or one standing on a Commons source read
/// ([`super::super::CommonsSourceRead::into_look`]), panics at once in a build with debug
/// assertions rather than wait on the look's observation for good, which flock(2) would.
///
/// NEGATIVE CONTROLS (2026-11-09): with `Observing::new` registering no lock file in
/// `OBSERVING`, the take waits on the look's observation and the hang detector fails "the
/// take refuses at once"; with `VaultLook::standing_on` keeping no observation, the read's
/// lock is let go, and the take under a source read's look succeeds ("the take under a look
/// that observes the lock panics").
#[test]
fn a_take_of_the_state_lock_under_a_look_panics_rather_than_wait_on_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = vault(root.path());
    drop(StateLock::take(&paths).unwrap());
    for on_source_read in [false, true] {
        let (done, finished) = std::sync::mpsc::channel();
        let looking = paths.clone();
        std::thread::spawn(move || {
            let taken = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let look = if on_source_read {
                    super::super::try_commons_source_read(&looking)
                        .unwrap()
                        .expect("no writer holds the vault")
                        .into_look(&looking)
                } else {
                    VaultLook::new(&looking)
                };
                let observed = look.observation.is_some();
                let _held = StateLock::take(&looking);
                observed
            }));
            let _ = done.send(taken.map_err(|panic| panic.downcast_ref::<String>().cloned()));
        });
        let outcome = finished
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the take refuses at once rather than wait on its own look");
        let message = outcome
            .expect_err("the take under a look that observes the lock panics")
            .expect("a panic message");
        assert!(
            message.contains("under this thread's own observation"),
            "{on_source_read}: {message}"
        );
    }
}

#[test]
fn a_memo_observes_current_data_and_refuses_a_missing_layout_file() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = vault(root.path());
    let data = crate::records_storage::write_path(&paths.records_file(), true)
        .unwrap()
        .to_path_buf();
    let memo = Memo::new(inputs);
    let reads = AtomicUsize::new(0);
    assert_eq!(
        memo.read(&paths, |p| lines(p, &reads)).unwrap(),
        Some(vec!["m60".to_owned()])
    );
    let directory = fsx::stamp(&paths.records_file());
    std::fs::write(&data, b"m61-longer\n").unwrap();
    assert_eq!(fsx::stamp(&paths.records_file()), directory);
    assert_eq!(
        memo.read(&paths, |p| lines(p, &reads)).unwrap(),
        Some(vec!["m61-longer".to_owned()])
    );
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    std::fs::rename(&data, paths.records_file().join("saved-current")).unwrap();
    assert!(memo.read(&paths, |p| lines(p, &reads)).is_err());
}

/// A pinned layout standing as it was is read once, and read again by a reader that holds
/// fewer records than the layout pins, which the selection refuses (`check_capacity`).
/// NEGATIVE CONTROL: with the capacity left out of a layout's judgement
/// (`records_storage::Judged`), the memo answers the records it read under the larger one.
#[test]
fn a_pinned_layout_is_read_again_under_a_smaller_record_capacity() {
    fn budget(held: usize) -> crate::membership_log::RecordBudgets {
        crate::membership_log::RecordBudgets {
            weighed: 16,
            keyed: 128,
            held,
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = vault(root.path());
    let memo = Memo::new(inputs);
    let reads = AtomicUsize::new(0);
    let expected = Some(vec!["m60".to_owned()]);
    budget(64).within(|| {
        drop(crate::records_storage::write_path(&paths.records_file(), true).unwrap());
        assert_eq!(memo.read(&paths, |p| lines(p, &reads)).unwrap(), expected);
        assert_eq!(memo.read(&paths, |p| lines(p, &reads)).unwrap(), expected);
    });
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "a standing layout is read once"
    );
    assert!(
        budget(32)
            .within(|| memo.read(&paths, |p| lines(p, &reads)))
            .is_err(),
        "a reader holding fewer records than the layout pins reads it again and refuses it"
    );
}

/// Records the selection refuses stamp nothing, as the selection reads nothing from them: a
/// pinned layout whose header is not one, and a records entry left a directory with no pin (an
/// unfinished transition). So no read keeps an answer by them, and nothing takes their stamps
/// for settled (`client::MembersAsked::still_asking`). Records it reads through are stamped,
/// a plain records file with a preparation's intent beside it too. NEGATIVE CONTROLS: a pinned
/// layout stood without its judgement (`records_storage::Judgements::layout`) stamps the header
/// that is not one; every shape stood as a plain records file stamps the directory with no pin;
/// with the selection's witnesses looking at the intent whatever stands and a plain file only
/// where no intent stands beside it (both as eb0e73e0f1 had them), a file with an intent beside it
/// is unjudged.
#[test]
fn records_the_selection_refuses_are_unjudged() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = vault(root.path());
    assert!(
        stamp_inputs(&paths, inputs).is_some(),
        "a records file is judged"
    );
    let intent = crate::records_storage::intent_path(&paths.records_file()).unwrap();
    fsx::write_atomic(&intent, b"a preparation cut short\n", 0o600).unwrap();
    assert!(
        crate::records_storage::data_path(&paths.records_file()).is_ok(),
        "the selection reads a records file through an intent beside it"
    );
    assert!(
        stamp_inputs(&paths, inputs).is_some(),
        "a records file with an intent beside it is judged"
    );
    std::fs::remove_file(&intent).unwrap();
    drop(crate::records_storage::write_path(&paths.records_file(), true).unwrap());
    assert!(
        stamp_inputs(&paths, inputs).is_some(),
        "a pinned layout is judged"
    );
    fsx::write_atomic(
        &paths.records_file().join("layout"),
        b"not a layout\n",
        0o600,
    )
    .unwrap();
    assert!(
        stamp_inputs(&paths, inputs).is_none(),
        "a layout whose header is not one is unjudged"
    );
    let unfinished = Paths::for_home(&root.path().join("unfinished"));
    fsx::ensure_private_dir(&unfinished.dir).unwrap();
    std::fs::create_dir(unfinished.records_file()).unwrap();
    assert!(
        stamp_inputs(&unfinished, inputs).is_none(),
        "an entry left a directory with no pin is unjudged"
    );
}

/// A memo's stamps of the records look at a preparation's intent only where the selection reads
/// it (`records_storage::selection_witnesses`): beside a plain records file or a pinned layout
/// `data_path` reads no intent, and the stamps take no `lstat` of one; where no pin and nothing
/// at the entry stand, the intent decides the selection, and they take one. NEGATIVE CONTROL
/// (2026-11-11): with the witnesses looking at the intent whatever stands (before this law), the
/// stamps take `[1, 2, 1]`, the pinned layout's first judgement looking at its witnesses again
/// after it (`records_storage::Judgements::layout`).
#[test]
fn a_records_selection_looks_at_its_intent_only_where_it_reads_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let looked = |paths: &Paths| {
        let intent = crate::records_storage::intent_path(&paths.records_file()).unwrap();
        fsx::write_atomic(&intent, b"a preparation cut short\n", 0o600).unwrap();
        let looks = || {
            fsx::looks_under(&paths.dir)
                .get(&intent)
                .copied()
                .unwrap_or(0)
        };
        let before = looks();
        drop(stamp_inputs(paths, inputs));
        looks() - before
    };
    let plain = vault(&root.path().join("plain"));
    let pinned = vault(&root.path().join("pinned"));
    drop(crate::records_storage::write_path(&pinned.records_file(), true).unwrap());
    let absent = Paths::for_home(&root.path().join("absent"));
    fsx::ensure_private_dir(&absent.dir).unwrap();
    assert_eq!(
        [looked(&plain), looked(&pinned), looked(&absent)],
        [0, 0, 1],
        "the intent's lstats beside a plain file, a pinned layout and no records"
    );
}

/// An intent written beside records the selection reads past it (a plain records file, a pinned
/// layout) moves nothing a reader holds: the records' standing a memo stamps them by
/// (`records_storage::standing`) is the one before it, a selection held across it still stands
/// (`Selected::confirm`), and the records a memo gives ([`records_now`]) are the ones
/// `load_records` reads, which are the ones saved. Where no records stand, the intent decides the
/// selection, and the standing moves with it (`data_path` refuses this one). NEGATIVE CONTROL
/// (2026-11-11): with the witnesses looking at the intent whatever stands (before this law), the
/// plain file's standing moves (`a plain records file: an intent beside it moves no stamp`).
#[test]
fn an_intent_beside_records_read_past_it_moves_nothing_a_reader_holds() {
    let c = crate::membership_log::store::classified_tests::chain();
    let held = crate::membership_log::encode_records([&c.genesis, &c.first_by_m1]);
    let root = aterm_tempfile::tempdir().unwrap();
    let intent_beside = |entry: &std::path::Path| {
        let intent = crate::records_storage::intent_path(entry).unwrap();
        fsx::write_atomic(&intent, b"a preparation cut short\n", 0o600).unwrap();
    };
    for (shape, pinned) in [("a plain records file", false), ("a pinned layout", true)] {
        let paths = Paths::for_home(&root.path().join(if pinned { "pinned" } else { "plain" }));
        crate::client::save_records(&paths, &held).unwrap();
        let entry = paths.records_file();
        if pinned {
            drop(crate::records_storage::write_path(&entry, true).unwrap());
        }
        let before = crate::records_storage::standing(&entry, None);
        assert!(before.is_some(), "{shape} stands");
        let selection = crate::records_storage::selected(&entry).unwrap();
        intent_beside(&entry);
        assert!(
            crate::records_storage::standing(&entry, None) == before,
            "{shape}: an intent beside it moves no stamp"
        );
        assert!(
            selection.confirm().is_ok(),
            "{shape}: a selection held across the intent still stands"
        );
        assert_eq!(records_now(&paths).unwrap().encode(), held, "{shape}");
        assert_eq!(
            crate::client::load_records(&paths).unwrap().encode(),
            held,
            "{shape}"
        );
    }
    let absent = Paths::for_home(&root.path().join("absent"));
    fsx::ensure_private_dir(&absent.dir).unwrap();
    let entry = absent.records_file();
    assert!(
        crate::records_storage::standing(&entry, None).is_some(),
        "no records stand as none"
    );
    intent_beside(&entry);
    assert!(crate::records_storage::data_path(&entry).is_err());
    assert!(
        crate::records_storage::standing(&entry, None).is_none(),
        "where no records stand, an intent decides the selection"
    );
}

/// A signed vault's warm deny list has the same directory refusal as a fresh read, and
/// recovers when the original directory returns. Negative control on 02da6d5d7c: the fresh
/// read refused the symlink but the memo returned its earlier list, failing this test.
#[test]
fn a_warm_deny_list_refuses_a_symlinked_vault_directory() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let source = root.path().join("roster.vault");
    crate::client::create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
    let admin = crate::client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &crate::text::Audience::Everyone,
        crate::client::SWARM_ROSTER_ENTRY,
        b"deny_nodes = [\"m99\"]\n",
    )
    .unwrap();
    crate::client::admin_publish(&paths, &admin, &doc).unwrap();
    crate::client::sync(&paths, None).unwrap();
    let expected = Some(vec!["m99".to_owned()]);
    assert_eq!(crate::client::swarm_denied_nodes(&paths).unwrap(), expected);
    assert_eq!(
        crate::client::swarm_denied_nodes_now(&paths).unwrap(),
        expected
    );

    // All leaf files remain the same inodes with the same bytes and metadata.
    // Only the directory through which the read reaches them has changed.
    let held = paths.dir.with_extension("held");
    std::fs::rename(&paths.dir, &held).unwrap();
    std::os::unix::fs::symlink(&held, &paths.dir).unwrap();
    assert!(crate::client::swarm_denied_nodes(&paths).is_err());
    assert!(
        crate::client::swarm_denied_nodes_now(&paths).is_err(),
        "a cached membership answer must preserve the fresh reader's refusal"
    );
    std::fs::rename(&paths.dir, paths.dir.with_extension("held-link")).unwrap();
    std::fs::rename(&held, &paths.dir).unwrap();
    assert_eq!(
        crate::client::swarm_denied_nodes_now(&paths).unwrap(),
        expected
    );
}

/// A temporary failure to read the signed envelope is not a settled answer. Clearing
/// that failure changes no input metadata, so the same memo must read the deny list again.
#[test]
fn a_failed_cache_read_does_not_poison_the_memo_until_a_vault_write() {
    struct FailedReads(PathBuf);
    impl Drop for FailedReads {
        fn drop(&mut self) {
            fsx::fail_reads(&self.0, 0);
        }
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let source = root.path().join("roster.vault");
    crate::client::create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
    let admin = crate::client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &crate::text::Audience::Everyone,
        crate::client::SWARM_ROSTER_ENTRY,
        b"deny_nodes = [\"m99\"]\n",
    )
    .unwrap();
    crate::client::admin_publish(&paths, &admin, &doc).unwrap();
    crate::client::sync(&paths, None).unwrap();
    let expected = Some(vec!["m99".to_owned()]);
    assert_eq!(crate::client::swarm_denied_nodes(&paths).unwrap(), expected);

    let before = stamp_inputs(&paths, inputs).unwrap();
    let memo = Memo::<Option<Vec<String>>>::new(inputs);
    let failure = FailedReads(paths.cache_file());
    // Keep failing until explicitly cleared, independently of how many reads the
    // authorization path makes. The guard also clears the fault on an assertion panic.
    fsx::fail_reads(&failure.0, usize::MAX);
    let failed = memo.read(&paths, crate::client::swarm_denied_nodes);
    drop(failure);
    assert!(
        stamp_inputs(&paths, inputs).unwrap() == before,
        "clearing a read fault must not change the memo's input metadata"
    );
    assert_eq!(crate::client::swarm_denied_nodes(&paths).unwrap(), expected);
    assert_eq!(
        memo.read(&paths, crate::client::swarm_denied_nodes)
            .unwrap(),
        expected,
        "a transient read failure must not remain cached after the read recovers"
    );
    assert!(
        failed.is_err(),
        "the unread envelope must remain an I/O error"
    );
}

/// The roster memo must also recover after a cache read fails, without waiting for a
/// vault write to change its input stamps.
#[test]
fn a_failed_cache_read_does_not_poison_the_roster_memo_until_a_vault_write() {
    struct FailedReads(PathBuf);
    impl Drop for FailedReads {
        fn drop(&mut self) {
            fsx::fail_reads(&self.0, 0);
        }
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let source = root.path().join("roster.vault");
    crate::client::create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
    let admin = crate::client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &crate::text::Audience::Everyone,
        crate::client::SWARM_ROSTER_ENTRY,
        b"deny_nodes = [\"m99\"]\n",
    )
    .unwrap();
    crate::client::admin_publish(&paths, &admin, &doc).unwrap();
    crate::client::sync(&paths, None).unwrap();
    let expected = crate::client::swarm_roster_until(
        &paths,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert_eq!(expected.as_ref().unwrap().deny_nodes, ["m99"]);

    let before = stamp_inputs(&paths, inputs).unwrap();
    let memo = Memo::<Option<SwarmRoster>>::new(inputs);
    let failure = FailedReads(paths.cache_file());
    // The read fault lasts until explicitly cleared; no implementation read count
    // decides when the actual signed roster becomes available again.
    fsx::fail_reads(&failure.0, usize::MAX);
    let failed = memo.read(&paths, |paths| {
        crate::client::swarm_roster_until(
            paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
    });
    drop(failure);
    assert!(
        stamp_inputs(&paths, inputs).unwrap() == before,
        "clearing a read fault must not change the memo's input metadata"
    );
    assert_eq!(
        crate::client::swarm_roster_until(
            &paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap(),
        expected
    );
    assert_eq!(
        memo.read(&paths, |paths| crate::client::swarm_roster_until(
            paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        ))
        .unwrap(),
        expected,
        "a transient read failure must not remain cached after the roster recovers"
    );
    assert!(
        failed.is_err(),
        "the unread roster envelope must remain an I/O error"
    );
}

/// Two reads of unchanged inputs read once; a rewritten input, and another input appearing
/// beside it, are each read again at the next call, and the answer is the new list. Files in
/// the vault folder the read never opens (the vault server's `served/`, `commons/`, a job's
/// log), which change every few seconds on a live Mac, read nothing again.
///
/// NEGATIVE CONTROLS (2026-11-01): with the stamp comparison dropped so the memo always reads
/// (`held.stamps == stamps` replaced by `false` in `kept`), the second unchanged read reads
/// again (`left: 2, right: 1`); with it dropped so the memo never reads again (`true`), the
/// rewritten records' answer is the old list (`left: Some(["m60"]), right: Some(["m60",
/// "m61"])`); with every entry directly in the vault folder stamped (the shape before), a
/// write to `served/` reads again (`left: 4, right: 3`).
#[test]
fn a_deny_list_is_read_again_only_when_one_of_its_inputs_changed() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    let memo = Memo::<Option<Vec<String>>>::new(inputs);
    let reads = AtomicUsize::new(0);
    let now = || memo.read(&paths, |p| lines(p, &reads)).unwrap();

    assert_eq!(now(), Some(vec!["m60".to_owned()]));
    assert_eq!(now(), Some(vec!["m60".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 1);

    fsx::write_atomic(&paths.records_file(), b"m60\nm61\n", 0o600).unwrap();
    assert_eq!(now(), Some(vec!["m60".to_owned(), "m61".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 2);

    fsx::write_atomic(&paths.identity_file(), b"new input", 0o600).unwrap();
    assert_eq!(now(), Some(vec!["m60".to_owned(), "m61".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 3);

    for dir in [paths.served(), paths.dir.join("commons")] {
        fsx::ensure_private_dir(&dir).unwrap();
        fsx::write_atomic(&dir.join("heard"), b"heard", 0o600).unwrap();
    }
    fsx::write_atomic(
        &paths.dir.join("systems.alab.aterm-secrets.log"),
        b"line",
        0o600,
    )
    .unwrap();
    assert_eq!(now(), Some(vec!["m60".to_owned(), "m61".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 3);
}

/// What `state.toml` holds can come back where its metadata cannot, and a memo stamps it by
/// what it holds but `heard` and the rounds' receipts (`stamp_memo_inputs_in`): a state that
/// moved from A to B between a read's stamps and its read, which then read B, and back to A
/// after it, would have B's answer given under A's stamps. The answer of a read the state
/// moved under is not kept, so the next call under A reads A.
///
/// NEGATIVE CONTROL (2026-11-09): with every settled answer kept whatever the state's
/// standing after the read (`unmoved` always true), the call under A is given B's answer
/// (`left: 4, right: 3`).
#[test]
fn an_answer_read_after_the_state_moved_is_not_kept_under_its_old_standing() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    let state = |seq: u64| {
        let text = format!("membership = \"in\"\nseq = {seq}\n");
        fsx::write_atomic(&paths.state_file(), text.as_bytes(), 0o600).unwrap();
    };
    let seq = |paths: &Paths| crate::client::load_state(paths).map(|state| state.seq);
    state(3);
    let memo = Memo::<u64>::new(inputs);
    let moved = memo.read(&paths, |paths| {
        state(4);
        seq(paths)
    });
    assert_eq!(moved.unwrap(), 4);
    state(3);
    assert_eq!(memo.read(&paths, seq).unwrap(), 3);
}

/// Callers that miss together after a change wait for one read and take its answer: while
/// the first caller's read is held open, seven more ask, and once it returns every one of
/// them has the list and the folder was read once.
///
/// NEGATIVE CONTROLS (2026-11-01): with the second look in `held` after `refresh` dropped,
/// each waiting caller reads in turn once the first returns (`left: 8, right: 1`); with
/// `refresh` not taken at all, the first of the seven reads beside the first caller and the
/// rest take its answer without queueing, so this fails at its one-minute hang detector
/// ("the other callers never queued").
#[test]
fn callers_that_miss_together_wait_for_one_read() {
    const CALLERS: usize = 8;
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    let memo = Memo::<Option<Vec<String>>>::new(inputs);
    let reads = AtomicUsize::new(0);
    let answers = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            memo.read(&paths, |p| {
                reads.fetch_add(1, Ordering::SeqCst);
                let started = Instant::now();
                // Every caller, this one included, has missed and come to `refresh`.
                while memo.waiting.load(Ordering::SeqCst) < CALLERS {
                    assert!(started.elapsed() < HANG, "the other callers never queued");
                    std::thread::sleep(Duration::from_millis(1));
                }
                let text = std::fs::read_to_string(p.records_file()).unwrap();
                Ok(Some(text.lines().map(str::to_owned).collect()))
            })
        });
        let started = Instant::now();
        while reads.load(Ordering::SeqCst) == 0 {
            assert!(started.elapsed() < HANG, "the first caller never read");
            std::thread::sleep(Duration::from_millis(1));
        }
        let others: Vec<_> = (1..CALLERS)
            .map(|_| scope.spawn(|| memo.read(&paths, |p| lines(p, &reads))))
            .collect();
        std::iter::once(first)
            .chain(others)
            .map(|caller| caller.join().unwrap().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert!(
        answers
            .iter()
            .all(|answer| *answer == Some(vec!["m60".to_owned()]))
    );
}

/// A read made while a writer holds the vault's state lock (mid-apply), and a failed read,
/// are never kept: the next call on the same folder reads again, and the next settled answer
/// is kept.
///
/// NEGATIVE CONTROL (2026-11-01): with every `Ok` answer kept (`settled` always true), the
/// call after the mid-apply one is answered from the memo and never reads (`assertion failed:
/// failed.is_err()`).
#[test]
fn a_mid_apply_answer_is_read_again_at_the_next_call() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    let memo = Memo::<Option<Vec<String>>>::new(inputs);
    let reads = AtomicUsize::new(0);

    let writer = StateLock::take(&paths).unwrap();
    let mid_apply = memo.read(&paths, |_| {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    });
    assert_eq!(mid_apply.unwrap(), None);
    drop(writer);
    let failed = memo.read(&paths, |_| {
        reads.fetch_add(1, Ordering::SeqCst);
        Err(crate::Error::new("torn read"))
    });
    assert!(failed.is_err());
    assert_eq!(reads.load(Ordering::SeqCst), 2);

    let now = || memo.read(&paths, |p| lines(p, &reads)).unwrap();
    assert_eq!(now(), Some(vec!["m60".to_owned()]));
    assert_eq!(now(), Some(vec!["m60".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 3);
}

/// An `Ok(None)` read while no writer held the state lock is what the inputs say (this Mac
/// removed in the log, an envelope that does not verify), and is kept until they change:
/// with no lock file yet, and again once one appears and nobody holds it.
///
/// NEGATIVE CONTROL (2026-11-01): with only `Ok(Some(_))` kept (the shape before), every call
/// on that Mac reads again (`left: 2, right: 1`).
/// A lock an older build left wide is narrowed by the memo before it observes it, so the
/// first read after an upgrade settles and is kept: a reader that does not narrow it itself
/// reads once.
///
/// NEGATIVE CONTROL (2026-11-01): with the memo's `StateLock::narrow_owned` call removed (the
/// observation alone, as before), the wide lock fails the observation, no answer settles and
/// every call reads again (`left: 2, right: 1`, `a read past a wide lock is kept`).
#[test]
fn a_read_past_a_wide_lock_narrows_it_and_is_kept() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    drop(StateLock::take(&paths).unwrap());
    std::fs::set_permissions(paths.lock_file(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let memo = Memo::<Option<Vec<String>>>::new(inputs);
    let reads = AtomicUsize::new(0);
    let now = || memo.read(&paths, |p| lines(p, &reads)).unwrap();

    assert_eq!(now(), Some(vec!["m60".to_owned()]));
    assert_eq!(now(), Some(vec!["m60".to_owned()]));
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "a read past a wide lock is kept"
    );
    assert_eq!(
        std::fs::metadata(paths.lock_file()).unwrap().mode() & 0o7777,
        0o600
    );
}

#[test]
fn a_settled_answer_that_is_not_a_list_is_kept_until_the_inputs_change() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    let memo = Memo::<Option<Vec<String>>>::new(inputs);
    let reads = AtomicUsize::new(0);
    let none = || {
        memo.read(&paths, |_| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
        .unwrap()
    };

    assert_eq!(none(), None);
    assert_eq!(none(), None);
    assert_eq!(reads.load(Ordering::SeqCst), 1);

    drop(StateLock::take(&paths).unwrap());
    assert_eq!(none(), None);
    assert_eq!(none(), None);
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

/// A quiet memo asked under its caller's shared observation of the state lock gives what it
/// kept without observing the lock again, and reads a changed file as `read_keyed` does.
///
/// NEGATIVE CONTROL (2026-11-08): with `read_under` (then `read_keyed_under`) making `read_keyed`'s
/// call (the shape before), each kept answer observes the lock again (`left: 2, right: 0`).
#[test]
fn a_quiet_memo_under_its_callers_observation_takes_none_of_its_own() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    drop(StateLock::take(&paths).unwrap());
    let memo = Memo::<Option<Vec<String>>>::keyed(inputs, true);
    let reads = AtomicUsize::new(0);
    assert_eq!(
        memo.read_keyed(&paths, &(), |p| lines(p, &reads)).unwrap(),
        Some(vec!["m60".to_owned()])
    );
    let lock = StateLock::observe(&paths).unwrap().expect("no writer");
    let under = || {
        memo.read_under(&paths, &(), Under::of(&lock, &Moment::new(&paths)), |p| {
            lines(p, &reads)
        })
    };

    let observations = super::super::lock_observations_of(&paths);
    assert_eq!(under().unwrap(), Some(vec!["m60".to_owned()]));
    assert_eq!(under().unwrap(), Some(vec!["m60".to_owned()]));
    assert_eq!(super::super::lock_observations_of(&paths) - observations, 0);
    assert_eq!(reads.load(Ordering::SeqCst), 1, "read once");

    fsx::write_atomic(&paths.records_file(), b"m61-longer\n", 0o600).unwrap();
    assert_eq!(under().unwrap(), Some(vec!["m61-longer".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 2, "a changed file is read");
}

/// Only a shared observation that still guards the lock at the vault's path stands in for the
/// memo's own: under a writer's own exclusive take, and under an observation of a lock file
/// another was renamed over while a writer holds the new one, the memo reads afresh, as
/// `read_keyed` meets that writer, and keeps nothing.
///
/// NEGATIVE CONTROLS (2026-11-08, run again on `StateLock::observes_in` 2026-11-10): with
/// `StateLock::observes_in` taking any lock (no `shared`), the writer's own check is given the
/// answer kept from before its take (`left: 1, right: 3`, `a writer's own take reads afresh`);
/// with it taking a shared one without its guard check (`StateLock::guards_in`), the
/// observation of the lock renamed away is given the answer kept under the new lock while its
/// writer holds it (`left: 4, right: 5`, `a lock renamed away guards nothing`).
#[test]
fn a_writers_take_or_a_lock_renamed_away_is_no_observation_for_a_memo() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    drop(StateLock::take(&paths).unwrap());
    let memo = Memo::<Option<Vec<String>>>::keyed(inputs, true);
    let reads = AtomicUsize::new(0);
    let kept = || memo.read_keyed(&paths, &(), |p| lines(p, &reads)).unwrap();
    assert_eq!(kept(), Some(vec!["m60".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 1);

    {
        let writer = StateLock::take(&paths).unwrap();
        for _ in 0..2 {
            assert_eq!(
                memo.read_under(&paths, &(), Under::of(&writer, &Moment::new(&paths)), |p| {
                    lines(p, &reads)
                })
                .unwrap(),
                Some(vec!["m60".to_owned()])
            );
        }
        assert_eq!(
            reads.load(Ordering::SeqCst),
            3,
            "a writer's own take reads afresh"
        );
    }

    let renamed_away = StateLock::observe(&paths).unwrap().expect("no writer");
    fsx::write_atomic(&paths.lock_file(), b"", 0o600).unwrap();
    assert_eq!(kept(), Some(vec!["m60".to_owned()]));
    assert_eq!(kept(), Some(vec!["m60".to_owned()]));
    assert_eq!(reads.load(Ordering::SeqCst), 4, "kept under the new lock");
    let _writer = StateLock::take(&paths).unwrap();
    assert_eq!(
        memo.read_under(
            &paths,
            &(),
            Under::of(&renamed_away, &Moment::new(&paths)),
            |p| lines(p, &reads)
        )
        .unwrap(),
        Some(vec!["m60".to_owned()])
    );
    assert_eq!(
        reads.load(Ordering::SeqCst),
        5,
        "a lock renamed away guards nothing"
    );
}

/// An observation takes the descriptor an earlier one left idle instead of opening the lock
/// file again, and holds no lock once it drops: a writer takes the lock at once.
///
/// NEGATIVE CONTROLS (2026-11-08): with `StateLock::open_observed` opening every time (the
/// shape before), each observation opens the lock (`left: 2, right: 0`, `observed on the
/// idle descriptor`); with the drop putting the descriptor back before its unlock, a writer
/// finds the lock held (`a writer takes the lock an observation left`).
#[test]
fn an_observation_takes_the_descriptor_an_earlier_one_left_unlocked() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    drop(StateLock::take(&paths).unwrap());
    drop(StateLock::observe(&paths).unwrap().expect("no writer"));
    let opens = super::super::lock_opens_of(&paths);
    for _ in 0..2 {
        let lock = StateLock::observe(&paths).unwrap().expect("no writer");
        assert!(lock.observes(&paths));
    }
    assert_eq!(
        super::super::lock_opens_of(&paths) - opens,
        0,
        "observed on the idle descriptor"
    );
    assert!(
        StateLock::try_take(&paths).unwrap().is_some(),
        "a writer takes the lock an observation left"
    );
}

/// An idle descriptor is judged as a file just opened is: a lock given a mode others can read
/// or a second link is refused, a lock file another was renamed over is observed by its new
/// file, and a vault folder made a symlink to itself moved aside is refused, each as a fresh
/// open finds it.
///
/// NEGATIVE CONTROLS (2026-11-08): with `open_observed` taking an idle descriptor without
/// comparing it with the file at the lock's path, the observation after a lock file was
/// renamed over judges the old one and refuses it (`has 0 hard links`); without judging the
/// folder, the symlinked folder is observed (`a symlinked folder is refused`).
#[test]
fn an_idle_descriptor_is_judged_as_a_file_just_opened() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    drop(StateLock::take(&paths).unwrap());
    let idle = || drop(StateLock::observe(&paths).unwrap().expect("no writer"));

    idle();
    std::fs::set_permissions(paths.lock_file(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        StateLock::observe(&paths).is_err(),
        "a wide lock is refused"
    );
    std::fs::set_permissions(paths.lock_file(), std::fs::Permissions::from_mode(0o600)).unwrap();

    idle();
    let second = home.path().join("second-link");
    std::fs::hard_link(paths.lock_file(), &second).unwrap();
    assert!(
        StateLock::observe(&paths).is_err(),
        "a linked lock is refused"
    );
    std::fs::remove_file(&second).unwrap();

    idle();
    fsx::write_atomic(&paths.lock_file(), b"", 0o600).unwrap();
    let renamed_over = StateLock::observe(&paths).unwrap();
    assert!(
        renamed_over.is_some_and(|lock| lock.observes(&paths)),
        "a lock file renamed over is observed by its new file"
    );

    idle();
    let aside = home.path().join("vault-aside");
    std::fs::rename(&paths.dir, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &paths.dir).unwrap();
    assert!(
        StateLock::observe(&paths).is_err(),
        "a symlinked folder is refused"
    );
    std::fs::remove_file(&paths.dir).unwrap();
    std::fs::rename(&aside, &paths.dir).unwrap();

    idle();
    std::fs::rename(&paths.dir, &aside).unwrap();
    assert!(
        StateLock::observe(&paths).unwrap().is_none(),
        "a missing folder is no observation"
    );
}

/// A look's stamps of the Commons observation's files (`Set::Observation`, `Under::stamps`) are
/// its stamps of the vault's (`Set::Vault`) with the device files' after them, and so the stamps a
/// memo of `observation_inputs` takes of them through a glance of its own
/// (`stamp_memo_inputs_in`), in each state the vault can stand in: plain records, no
/// `state.toml`, no device files, a pinned layout, and a `commons/` folder nothing can look into,
/// where the observation's files cannot be stamped and the vault's can. A change to each input
/// moves the next look's stamps of the observation's files, and a change to a device file moves
/// them and leaves the vault's as they were.
/// NEGATIVE CONTROLS (2026-11-11): with the device files' stamps in the other order
/// (`with_device_files`), `the observation stamps are its inputs' own (plain records)`; with the
/// composed stamps leaving the device files out, the same; with `state` not carried (`state:
/// None`), the same.
#[test]
fn a_looks_observation_stamps_are_its_inputs_own() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = vault(root.path());
    drop(StateLock::take(&paths).unwrap());
    fsx::write_atomic(
        &paths.state_file(),
        b"membership = \"in\"\nseq = 3\n",
        0o600,
    )
    .unwrap();
    fsx::ensure_private_dir(&paths.dir.join("commons")).unwrap();
    for (file, bytes) in device_files(&paths).iter().zip([b"age", b"clm"]) {
        fsx::write_atomic(file, bytes, 0o600).unwrap();
    }
    // A new look's stamps of each set, its observation's held to the flat ones.
    let looked = |state: &str| {
        let look = VaultLook::new(&paths);
        let under = look.held().expect("CONTROL: a look that holds the vault");
        let vault = under.stamps(Set::Vault).map(|stamps| (*stamps).clone());
        let observation = under
            .stamps(Set::Observation)
            .map(|stamps| (*stamps).clone());
        let flat =
            stamp_memo_inputs_in(&paths, observation_inputs, true, Some(&fsx::Glance::new()));
        assert!(
            observation == flat,
            "the observation stamps are its inputs' own ({state})"
        );
        (vault, observation)
    };
    let mut was = looked("plain records");
    assert!(was.0.is_some() && was.1.is_some(), "CONTROL: stamped");
    let inputs = observation_inputs(&paths).unwrap();
    let device = device_files(&paths);
    for (n, input) in inputs.iter().enumerate() {
        fsx::ensure_private_dir(input.parent().unwrap()).unwrap();
        let bytes = if *input == paths.state_file() {
            format!("membership = \"in\"\nseq = {}\n", 4 + n)
        } else {
            format!("changed {n}\n")
        };
        fsx::write_atomic(input, bytes.as_bytes(), 0o600).unwrap();
        let now = looked(&input.display().to_string());
        assert!(
            now.1 != was.1,
            "a change to {} moves the observation stamps",
            input.display()
        );
        assert_eq!(
            now.0 != was.0,
            !device.contains(input),
            "a change to {} moves the vault stamps where it is a vault input",
            input.display()
        );
        was = now;
    }
    std::fs::remove_file(paths.state_file()).unwrap();
    looked("no state.toml");
    for file in &device {
        std::fs::remove_file(file).unwrap();
    }
    looked("no device files");
    drop(crate::records_storage::write_path(&paths.records_file(), true).unwrap());
    looked("a pinned layout");
    let commons = paths.dir.join("commons");
    std::fs::set_permissions(&commons, std::fs::Permissions::from_mode(0o000)).unwrap();
    let shut = looked("commons/ shut");
    std::fs::set_permissions(&commons, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        shut.0.is_some() && shut.1.is_none(),
        "a commons/ nothing can look into stamps the vault's files and not the observation's"
    );
}

/// A look keeps the stamps it took only where taking them looked at nothing afresh
/// (`fsx::fresh_looks`): in a new vault folder its first ask of a memo of the vault's files finds
/// no look of `state.toml` kept for it (`state_look`), reads the file and stamps it afresh, and
/// keeps nothing, so its next ask stamps the files again and keeps them, and its third is given
/// them; the next look, whose state is kept, stamps them once.
/// NEGATIVE CONTROL (2026-11-11): with `Under::stamps` keeping whether or not it looked afresh,
/// the first look stamps once (`left: 1, right: 2`, `a look stamps again what it read afresh (the
/// first look)`).
#[test]
fn a_look_stamps_again_what_it_read_afresh() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = vault(root.path());
    drop(StateLock::take(&paths).unwrap());
    fsx::write_atomic(
        &paths.state_file(),
        b"membership = \"in\"\nseq = 3\n",
        0o600,
    )
    .unwrap();
    let memo = Memo::<Option<Vec<String>>>::of(Set::Vault, true);
    let reads = AtomicUsize::new(0);
    for (which, asks, stamped) in [("the first look", 3, 2), ("the next look", 2, 1)] {
        let look = VaultLook::new(&paths);
        let before = test_look_derivations();
        for _ in 0..asks {
            assert_eq!(
                look.ask(&memo, &(), |p| lines(p, &reads)).unwrap(),
                Some(vec!["m60".to_owned()])
            );
        }
        let derived = test_look_derivations().since(before);
        assert_eq!(
            derived.derived.vault_stamps, stamped,
            "a look stamps again what it read afresh ({which})"
        );
        assert_eq!(derived.rederived_after_kept.vault_stamps, 0, "{which}");
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1, "CONTROL: read once");
}

/// What a look's moment judged stands only beside the look's own observation of the state lock
/// (`Moment::caching_for`): an [`Under`] that pairs the moment with another observation of the
/// same lock, which the look did not judge, stands for nothing it judged, so each ask under it is
/// judged where it is asked.
/// NEGATIVE CONTROL (2026-11-11): with `Moment::caching_for` not comparing where the lock stands
/// (no `self.lock.get() == address(lock)`), the moment beside another observation stands for the
/// look (`a look's moment beside another observation stands for nothing`).
#[test]
fn a_looks_moment_stands_only_beside_its_own_observation() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = vault(home.path());
    drop(StateLock::take(&paths).unwrap());
    let look = VaultLook::new(&paths);
    assert!(
        look.held().is_some_and(|under| under.caching_for(&paths)),
        "CONTROL: a look's own observation stands for its moment"
    );
    let other = StateLock::observe(&paths).unwrap().expect("no writer");
    assert!(
        !Under::of(&other, &look.moment).caching_for(&paths),
        "a look's moment beside another observation stands for nothing"
    );
}
