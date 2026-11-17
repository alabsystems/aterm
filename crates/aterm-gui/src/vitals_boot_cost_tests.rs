// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::fs::File;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use aterm_vitals::boot_cost::{Collector, MAX_BOOTS, Observation, ProcessKey};

use super::*;

const THREE_MINUTES: u64 = 180_000_000_000;

fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
    let directory = aterm_tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap();
    aterm_uds::private_file::create_private_dir(&parent).unwrap();
    let path = parent.join(FILE);
    (directory, path)
}

fn book(boot: &str, at_ns: u64) -> Book {
    let mut collector = Collector::new(Book::default());
    assert!(
        collector
            .observe(
                Some(boot),
                Observation {
                    at_ns,
                    ..Observation::default()
                }
            )
            .unwrap()
    );
    collector.book().clone()
}

fn saved(path: &Path, book: &Book) {
    let result = transact(path, Some(book), Probes::NATIVE)
        .unwrap_or_else(|error| panic!("save: {error:?}"));
    assert_eq!(result.persistence, Persistence::Saved);
}

fn read(path: &Path) -> Book {
    transact(path, None, Probes::NATIVE)
        .unwrap_or_else(|error| panic!("read: {error:?}"))
        .book
}

fn settle(store: &mut BootStore) {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "boot store worker", || {
        store.poll();
        (!store.view().busy).then_some(())
    });
}

#[test]
fn persisted_boot_cuts_reopen_after_the_producer_is_gone() {
    let (_directory, path) = scratch();
    let original = book("boot-a", THREE_MINUTES);
    {
        let mut store = BootStore::new(Some(path.clone()));
        store.submit(original.clone());
        settle(&mut store);
        assert_eq!(store.view().book, original);
        assert_eq!(store.view().persistence, Persistence::Saved);
        assert!(!store.view().dirty);
    }
    let bytes = std::fs::read(&path).unwrap();
    let mut relaunched = BootStore::new(Some(path.clone()));
    settle(&mut relaunched);
    assert_eq!(relaunched.view().persistence, Persistence::Restored);
    assert_eq!(relaunched.view().book, original);
    assert_eq!(
        std::fs::read(path).unwrap(),
        bytes,
        "restore changes no stored cut"
    );
}

#[test]
fn an_inert_store_never_starts_a_worker_or_retains_a_submission() {
    let mut store = BootStore::new(None);
    store.submit(book("boot-a", THREE_MINUTES));
    store.retry();
    assert!(!store.poll());
    assert_eq!(store.view().persistence, Persistence::Unavailable);
    assert!(!store.view().busy);
    assert!(!store.view().dirty);
}

#[test]
fn restore_wins_a_conflicting_first_capture_and_neither_cut_is_summed() {
    let (_directory, path) = scratch();
    let persisted = book("boot-a", THREE_MINUTES);
    saved(&path, &persisted);
    let incoming = book("boot-a", THREE_MINUTES + 1);
    let mut expected = persisted.clone();
    expected.merge(&incoming).unwrap();
    let mut store = BootStore::new(Some(path.clone()));
    store.submit(incoming);
    settle(&mut store);
    assert_eq!(read(&path), expected);
    assert_eq!(store.view().book, expected);
    assert_eq!(
        store.view().book.boots[0].observations[0],
        persisted.boots[0].observations[0]
    );
    assert_eq!(store.view().book.boots[0].observations.len(), 1);
    assert_ne!(expected, persisted, "the conflict must remain observable");
}

#[test]
fn a_relaunched_producers_counter_epoch_cannot_replace_a_persisted_source() {
    let (_directory, path) = scratch();
    let make = |pid, counter_epoch| {
        let mut collector = Collector::new(Book::default());
        collector
            .observe(
                Some("boot-a"),
                Observation {
                    at_ns: THREE_MINUTES,
                    process: Some(ProcessKey {
                        pid,
                        start: u64::from(pid),
                        version: Some(1),
                    }),
                    counter_epoch,
                    ..Observation::default()
                },
            )
            .unwrap();
        collector.book().clone()
    };
    let original = make(17, 1);
    saved(&path, &original);
    let relaunched = make(23, 2);
    let mut expected = original.clone();
    expected.merge(&relaunched).unwrap();
    saved(&path, &relaunched);
    let restored = read(&path);
    assert_eq!(restored, expected);
    assert_eq!(
        restored.boots[0].observations[0],
        original.boots[0].observations[0]
    );
    assert_ne!(restored, original, "source conflict remains explicit");
}

#[test]
fn repeating_a_book_preserves_its_cut_and_reboot_keeps_distinct_identity() {
    let (_directory, path) = scratch();
    let original = book("boot-a", THREE_MINUTES);
    saved(&path, &original);
    saved(&path, &original);
    assert_eq!(read(&path), original);
    let next_boot = book("boot-b", THREE_MINUTES);
    let mut expected = original.clone();
    expected.merge(&next_boot).unwrap();
    saved(&path, &next_boot);
    assert_eq!(read(&path), expected);
    assert_eq!(expected.boots.len(), 2);
}

#[test]
fn retention_is_bounded_on_disk_as_well_as_in_the_model() {
    let (_directory, path) = scratch();
    let mut expected = Book::default();
    for index in 0..MAX_BOOTS + 3 {
        let incoming = book(&format!("boot-{index}"), THREE_MINUTES);
        expected.merge(&incoming).unwrap();
        saved(&path, &incoming);
    }
    assert_eq!(read(&path), expected);
    assert_eq!(expected.boots.len(), MAX_BOOTS);
    assert!(std::fs::metadata(path).unwrap().len() <= MAX_BYTES as u64);
}

#[test]
fn malformed_or_oversize_records_are_preserved_instead_of_overwritten() {
    let (_directory, path) = scratch();
    let incoming = book("boot-a", THREE_MINUTES);
    for bytes in [b"{".to_vec(), vec![b' '; MAX_BYTES + 1]] {
        aterm_uds::atomic_file::write(&path, &bytes, 0o600).unwrap();
        assert!(transact(&path, None, Probes::NATIVE).is_err());
        assert!(transact(&path, Some(&incoming), Probes::NATIVE).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn duplicate_incoming_boots_and_concatenated_documents_are_rejected_whole() {
    let (_directory, path) = scratch();
    let original = book("boot-a", THREE_MINUTES);
    saved(&path, &original);
    let before = std::fs::read(&path).unwrap();
    let mut duplicate = original.clone();
    duplicate.boots.push(duplicate.boots[0].clone());
    assert!(transact(&path, Some(&duplicate), Probes::NATIVE).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let concatenated = [before.as_slice(), before.as_slice()].concat();
    aterm_uds::atomic_file::write(&path, &concatenated, 0o600).unwrap();
    assert!(transact(&path, None, Probes::NATIVE).is_err());
    assert!(transact(&path, Some(&original), Probes::NATIVE).is_err());
    assert_eq!(std::fs::read(path).unwrap(), concatenated);
}

#[test]
fn links_nonregular_files_and_open_parent_permissions_refuse_storage() {
    let (_directory, path) = scratch();
    let target = path.with_file_name("unrelated");
    aterm_uds::atomic_file::write(&target, b"unrelated", 0o600).unwrap();
    symlink(&target, &path).unwrap();
    let refusal = transact(&path, Some(&book("boot-a", THREE_MINUTES)), Probes::NATIVE)
        .err()
        .expect("linked record is refused");
    assert_eq!(refusal.kind, io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read(&target).unwrap(), b"unrelated");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(transact(&path, None, Probes::NATIVE).is_err());
    std::fs::remove_dir(&path).unwrap();
    let parent = path.parent().unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(transact(&path, Some(&book("boot-a", THREE_MINUTES)), Probes::NATIVE).is_err());
    assert!(!path.exists());
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn linked_parent_ancestry_and_a_second_file_link_are_refused() {
    let (_directory, path) = scratch();
    let alias = path.with_file_name("alias");
    symlink(path.parent().unwrap(), &alias).unwrap();
    assert!(transact(&alias.join(FILE), None, Probes::NATIVE).is_err());
    saved(&path, &book("boot-a", THREE_MINUTES));
    let another = path.with_file_name("another-link");
    std::fs::hard_link(&path, &another).unwrap();
    assert!(transact(&path, None, Probes::NATIVE).is_err());
    assert!(transact(&path, Some(&book("boot-b", THREE_MINUTES)), Probes::NATIVE).is_err());
}

fn refuse_owner(_file: &File) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::PermissionDenied))
}

fn refuse_lock(_file: File) -> io::Result<crate::native_document_host::HeldAdvisoryLock> {
    Err(io::Error::from(io::ErrorKind::WouldBlock))
}

#[test]
fn unavailable_owner_or_lock_evidence_never_falls_back_to_an_unlocked_write() {
    let (_directory, path) = scratch();
    let original = book("boot-a", THREE_MINUTES);
    saved(&path, &original);
    let bytes = std::fs::read(&path).unwrap();
    let incoming = book("boot-b", THREE_MINUTES);
    for probes in [
        Probes {
            file: refuse_owner,
            ..Probes::NATIVE
        },
        Probes {
            directory: refuse_owner,
            ..Probes::NATIVE
        },
        Probes {
            lock: refuse_lock,
            ..Probes::NATIVE
        },
    ] {
        assert!(transact(&path, Some(&incoming), probes).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn an_existing_native_writer_lock_prevents_a_second_writer() {
    let (_directory, path) = scratch();
    let original = book("boot-a", THREE_MINUTES);
    saved(&path, &original);
    let held = crate::native_document_host::HeldAdvisoryLock::try_lock(
        aterm_uds::private_file::open_private_rw(path.parent().unwrap(), LOCK).unwrap(),
    )
    .unwrap();
    let error = transact(&path, Some(&book("boot-b", THREE_MINUTES)), Probes::NATIVE)
        .err()
        .unwrap();
    assert_eq!(error.kind, io::ErrorKind::WouldBlock);
    drop(held);
    assert_eq!(read(&path), original);
}

fn before_rename_failure(_publication: Publication<'_>) -> io::Result<io::Result<()>> {
    Err(io::Error::from(io::ErrorKind::StorageFull))
}

fn after_rename_failure(publication: Publication<'_>) -> io::Result<io::Result<()>> {
    publish(publication)??;
    Ok(Err(io::Error::from(io::ErrorKind::Other)))
}

fn unavailable_directory_flush(publication: Publication<'_>) -> io::Result<io::Result<()>> {
    let _refused = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::ENOTSUP);
    publish(publication)
}

#[test]
fn unavailable_directory_durability_keeps_the_same_pending_cut_after_publication() {
    let (_directory, path) = scratch();
    let incoming = book("boot-a", THREE_MINUTES + 11);
    let mut store = BootStore::with_probes(
        Some(path.clone()),
        Probes {
            publish: unavailable_directory_flush,
            ..Probes::NATIVE
        },
    );
    settle(&mut store);
    store.submit(incoming.clone());
    settle(&mut store);
    assert!(matches!(
        store.view().persistence,
        Persistence::PublishedDurabilityUnknown(Failure {
            operation: "flush_directory",
            kind: io::ErrorKind::Unsupported,
        })
    ));
    assert!(store.view().dirty);
    assert_eq!(store.pending, Some(incoming.clone()));
    assert_eq!(store.view().book, incoming);
    assert_eq!(
        read(&path),
        incoming,
        "rename published the original observed cut"
    );
    store.probes = Probes::NATIVE;
    store.retry();
    settle(&mut store);
    assert_eq!(store.view().persistence, Persistence::Saved);
    assert!(!store.view().dirty);
    assert_eq!(
        read(&path),
        incoming,
        "durability retry does not recapture the observation"
    );
}

#[test]
fn failed_publication_retains_the_identical_pending_observation_until_retry() {
    let (_directory, path) = scratch();
    let original = book("boot-a", THREE_MINUTES);
    saved(&path, &original);
    let old_bytes = std::fs::read(&path).unwrap();
    let incoming = book("boot-b", THREE_MINUTES + 7);
    let mut store = BootStore::with_probes(
        Some(path.clone()),
        Probes {
            publish: before_rename_failure,
            ..Probes::NATIVE
        },
    );
    settle(&mut store);
    store.submit(incoming.clone());
    settle(&mut store);
    assert!(matches!(
        store.view().persistence,
        Persistence::Failed(Failure {
            operation: "publish_record",
            ..
        })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), old_bytes);
    assert_eq!(store.pending, Some(incoming.clone()));
    assert!(store.view().dirty);
    store.probes = Probes::NATIVE;
    store.retry();
    settle(&mut store);
    let mut expected = original;
    expected.merge(&incoming).unwrap();
    assert_eq!(read(&path), expected);
    assert!(!store.view().dirty);
}

#[test]
fn post_rename_flush_failure_reports_published_bytes_with_unknown_durability() {
    let (_directory, path) = scratch();
    let incoming = book("boot-a", THREE_MINUTES + 9);
    let mut store = BootStore::with_probes(
        Some(path.clone()),
        Probes {
            publish: after_rename_failure,
            ..Probes::NATIVE
        },
    );
    settle(&mut store);
    store.submit(incoming.clone());
    settle(&mut store);
    assert!(matches!(
        store.view().persistence,
        Persistence::PublishedDurabilityUnknown(_)
    ));
    assert_eq!(read(&path), incoming);
    assert_eq!(store.view().book, incoming);
    assert_eq!(store.pending, Some(incoming));
    assert!(store.view().dirty);
    store.probes = Probes::NATIVE;
    store.retry();
    settle(&mut store);
    assert_eq!(store.view().persistence, Persistence::Saved);
    assert!(!store.view().dirty);
}

#[test]
fn retained_file_and_lock_identity_detect_replacements_even_with_equal_bytes() {
    let (_directory, path) = scratch();
    let original = book("boot-a", THREE_MINUTES);
    saved(&path, &original);
    let read_admission = native::Admission::open(&path, Probes::NATIVE).unwrap();
    let held_read = native::Read::open(&path, Probes::NATIVE, &read_admission)
        .unwrap_or_else(|error| panic!("read: {error:?}"));
    aterm_uds::atomic_file::write(&path, &encode(&original).unwrap(), 0o600).unwrap();
    assert!(
        held_read
            .intact(&path, Probes::NATIVE, &read_admission)
            .is_err()
    );
    drop(read_admission);
    let admission = native::Admission::open(&path, Probes::NATIVE).unwrap();
    aterm_uds::atomic_file::write(&path.with_file_name(LOCK), b"", 0o600).unwrap();
    assert!(admission.intact(&path, Probes::NATIVE).is_err());
}

#[test]
fn replacing_a_parent_directory_cannot_inherit_its_retained_writer_admission() {
    let (_directory, path) = scratch();
    let current = path.with_file_name("current");
    let retired = path.with_file_name("retired");
    aterm_uds::private_file::create_private_dir(&current).unwrap();
    let held_path = current.join(FILE);
    saved(&held_path, &book("boot-a", THREE_MINUTES));
    let admission = native::Admission::open(&held_path, Probes::NATIVE).unwrap();
    std::fs::rename(&current, &retired).unwrap();
    aterm_uds::private_file::create_private_dir(&current).unwrap();
    assert!(admission.intact(&held_path, Probes::NATIVE).is_err());
    assert!(!held_path.exists());
    drop(admission);
    assert_eq!(read(&retired.join(FILE)), book("boot-a", THREE_MINUTES));
}

fn retarget_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().unwrap();
    let retired = parent.with_file_name("retired");
    let other = parent.with_file_name("other");
    std::fs::rename(parent, retired)?;
    symlink(other, parent)
}

fn retarget_before_commit(publication: Publication<'_>) -> io::Result<io::Result<()>> {
    retarget_parent(publication.path)?;
    publish(publication)
}

fn retarget_after_commit(publication: Publication<'_>) -> io::Result<io::Result<()>> {
    let Publication {
        path,
        bytes,
        check: original,
        directory,
    } = publication;
    let mut check = || {
        original()?;
        retarget_parent(path)
    };
    publish(Publication {
        path,
        bytes,
        check: &mut check,
        directory,
    })
}

#[test]
fn retargeting_during_commit_never_redirects_publication_or_discards_pending_cuts() {
    for after_check in [false, true] {
        let (_directory, path) = scratch();
        let current = path.with_file_name("current");
        let retired = path.with_file_name("retired");
        let other = path.with_file_name("other");
        aterm_uds::private_file::create_private_dir(&current).unwrap();
        aterm_uds::private_file::create_private_dir(&other).unwrap();
        aterm_uds::atomic_file::write(&other.join(FILE), b"unrelated", 0o600).unwrap();
        let path = current.join(FILE);
        let original = book("boot-a", THREE_MINUTES);
        saved(&path, &original);
        let incoming = book("boot-b", THREE_MINUTES);
        let mut store = BootStore::with_probes(
            Some(path),
            Probes {
                publish: if after_check {
                    retarget_after_commit
                } else {
                    retarget_before_commit
                },
                ..Probes::NATIVE
            },
        );
        settle(&mut store);
        store.submit(incoming.clone());
        settle(&mut store);
        assert!(store.view().dirty);
        assert_eq!(store.pending, Some(incoming.clone()));
        let mut expected = original.clone();
        if after_check {
            expected.merge(&incoming).unwrap();
            assert!(matches!(
                store.view().persistence,
                Persistence::PublishedDurabilityUnknown(Failure {
                    operation: "published_store_identity",
                    ..
                })
            ));
        } else {
            assert!(matches!(
                store.view().persistence,
                Persistence::Failed(Failure {
                    operation: "publish_record",
                    ..
                })
            ));
        }
        assert_eq!(read(&retired.join(FILE)), expected);
        assert_eq!(std::fs::read(other.join(FILE)).unwrap(), b"unrelated");
        assert_eq!(
            std::fs::read_dir(retired).unwrap().count(),
            2,
            "record and lock, with no temporary"
        );
    }
}

#[test]
fn data_reads_after_parent_retargeting_stay_on_the_admitted_directory() {
    let (_directory, path) = scratch();
    let current = path.with_file_name("current");
    let other = path.with_file_name("other");
    aterm_uds::private_file::create_private_dir(&current).unwrap();
    aterm_uds::private_file::create_private_dir(&other).unwrap();
    let path = current.join(FILE);
    let original = book("boot-a", THREE_MINUTES);
    let unrelated = book("boot-z", THREE_MINUTES + 99);
    saved(&path, &original);
    saved(&other.join(FILE), &unrelated);
    let admission = native::Admission::open(&path, Probes::NATIVE).unwrap();
    retarget_parent(&path).unwrap();
    let held_read = native::Read::open(&path, Probes::NATIVE, &admission)
        .unwrap_or_else(|error| panic!("held read: {error:?}"));
    assert_eq!(held_read.book, original);
    assert!(held_read.intact(&path, Probes::NATIVE, &admission).is_ok());
    assert!(admission.intact(&path, Probes::NATIVE).is_err());
    assert_eq!(read(&other.join(FILE)), unrelated);
}

#[test]
fn completion_notifies_after_queueing_and_late_callback_installation_loses_no_result() {
    let (_directory, path) = scratch();
    let mut store = BootStore::new(Some(path.clone()));
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "completed restore before callback installation",
        || store.notice.ready.load(Ordering::Acquire).then_some(()),
    );
    let (notified, hears) = mpsc::channel();
    store.set_wake(Arc::new(move || {
        let _ = notified.send(());
    }));
    hears.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(store.poll());
    assert_eq!(store.view().persistence, Persistence::Missing);
    drop(store);

    // A fresh worker isolates the save notification from the restore's installation race.
    let mut store = BootStore::new(None);
    store.path = Some(path);
    let (notified, hears) = mpsc::sync_channel(0);
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let (ended, ending) = mpsc::channel();
    store.set_wake(Arc::new(move || {
        if notified.send(()).is_ok() {
            let _ = released
                .lock()
                .unwrap()
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        }
        let _ = ended.send(());
    }));
    let incoming = book("boot-a", THREE_MINUTES);
    store.submit(incoming.clone());
    hears.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    let result_was_already_queued = store.poll();
    release.send(()).unwrap();
    ending.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        result_was_already_queued,
        "notification follows result publication"
    );
    assert_eq!(store.view().book, incoming);
    assert_eq!(store.view().persistence, Persistence::Saved);
}

#[test]
fn a_new_process_reads_the_same_saved_cuts_through_the_named_boot_query() {
    use aterm_vitals::boot_cost::{RawCounters, WakeCount, WakeCounts};
    use aterm_vitals::explain::HistoricalReported;
    use aterm_vitals::sys::{SysQuery, View, boot};

    const CHILD_PATH: &str = "ATERM_TEST_BOOT_REOPEN_PATH";
    const PARENT_PID: &str = "ATERM_TEST_BOOT_REOPEN_PARENT_PID";
    const BOOT: &str = "process-reopen-boot";
    let projection = |book: Book| boot::BootReport {
        book,
        boot_id: Some(BOOT.to_owned()),
        persistence: "restored",
        ..boot::BootReport::default()
    };
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let parent: u32 = std::env::var(PARENT_PID).unwrap().parse().unwrap();
        assert_ne!(std::process::id(), parent, "reopen runs in another process");
        let mut store = BootStore::new(Some(PathBuf::from(path)));
        settle(&mut store);
        assert_eq!(store.view().persistence, Persistence::Restored);
        assert!(!store.view().dirty);
        let query = SysQuery::parse("boot --json").expect("the named read parses");
        assert_eq!(query.view, View::Boot);
        assert!(query.json);
        let report = projection(store.view().book.clone());
        let value = match query.view {
            View::Boot => boot::json(Some(&report)),
            _ => unreachable!("the parsed query selects the boot projection"),
        };
        // Begin a new line after libtest's unfinished `test <name> ... ` prefix.
        println!("\nBOOT_REOPEN_PID {}", std::process::id());
        println!("BOOT_REOPEN_JSON {}", value.to_text());
        return;
    }

    let (_directory, path) = scratch();
    let mut collector = Collector::new(Book::default());
    for (at_ns, step) in [
        (179_000_000_000, 1),
        (THREE_MINUTES, 2),
        (900_000_000_000, 3),
    ] {
        collector
            .observe(
                Some(BOOT),
                Observation {
                    at_ns,
                    before_ns: Some(at_ns),
                    after_ns: Some(at_ns),
                    process: Some(ProcessKey {
                        pid: std::process::id(),
                        start: 11,
                        version: Some(7),
                    }),
                    counter_epoch: 37,
                    process_cpu: RawCounters {
                        own_ns: Some(u64::MAX - 40 + step),
                        inherited_ns: Some(17 + step),
                    },
                    coalition_id: Some(19),
                    coalition_cpu: RawCounters {
                        own_ns: Some(100 + step),
                        inherited_ns: Some(23 + step),
                    },
                    historical_reported: HistoricalReported {
                        own_ns: 111 * step,
                        tabs_ns: 333 * step,
                        incomplete: false,
                    },
                    wakes: Some(WakeCounts {
                        epoch: 43,
                        variants: vec![WakeCount {
                            kind: "Output".to_owned(),
                            count: 47 * step,
                        }],
                    }),
                    ..Observation::default()
                },
            )
            .unwrap();
    }
    let known = collector.book().clone();
    assert_eq!(known.boots[0].observations.len(), 2);
    {
        let mut store = BootStore::new(Some(path.clone()));
        store.submit(known.clone());
        settle(&mut store);
        assert_eq!(store.view().persistence, Persistence::Saved);
        assert_eq!(store.view().book, known);
    }
    let before = std::fs::read(&path).unwrap();
    let expected = boot::json(Some(&projection(known))).to_text();
    // The existing test-child pattern selects the calling libtest name rather than pinning
    // its module path. The original Cargo binary keeps its process-isolation identity.
    let test = std::thread::current()
        .name()
        .expect("libtest names this test")
        .to_owned();
    let mut command =
        aterm_uds::spawnfd::command(std::env::current_exe().expect("the Cargo test binary"));
    command
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env(CHILD_PATH, &path)
        .env(PARENT_PID, std::process::id().to_string());
    let output = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
        &mut command,
        aterm_spec::wait::MUST_HAPPEN,
        "the boot checkpoint reader child",
        MAX_BYTES * 4,
    )
    .expect("the child exits with complete stdout and stderr");
    assert!(
        output.status.success(),
        "child stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout).expect("the child prints UTF-8");
    let child_pid: u32 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("BOOT_REOPEN_PID "))
        .expect("the child reports its own process")
        .parse()
        .unwrap();
    assert_ne!(child_pid, std::process::id());
    let restored = stdout
        .lines()
        .find_map(|line| line.strip_prefix("BOOT_REOPEN_JSON "))
        .expect("the named boot query produced its report");
    assert_eq!(
        restored, expected,
        "cuts, source epochs, raw channels and history survive process restart"
    );
    assert_eq!(
        std::fs::read(path).unwrap(),
        before,
        "the child restores without capturing or writing new cuts"
    );
}
