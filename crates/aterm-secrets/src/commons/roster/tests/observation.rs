// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{client, commons::bootstrap::Bootstrap};

struct Vault {
    _root: aterm_tempfile::TempDir,
    paths: client::Paths,
    bootstrap: Bootstrap,
    journal: Journal,
}

impl Vault {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = client::Paths::for_home(&root.path().join("member"));
        let created = client::create(
            &paths,
            root.path().join("team.vault").to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        assert_eq!(created.local, None);
        client::seed_vault_genesis(&paths, &root.path().join("genesis")).unwrap();
        let bootstrap = Bootstrap::required(&paths).unwrap();
        assert!(bootstrap.is_log_backed().unwrap());
        let journal = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
        Self {
            _root: root,
            paths,
            bootstrap,
            journal,
        }
    }

    fn digest(&self) -> [u8; 32] {
        let (_, heads) = client::trusted_commons_log(&self.paths, self.bootstrap.trust())
            .unwrap()
            .unwrap();
        commons_log_state_digest(&self.paths, heads).unwrap()
    }
}

fn writer(paths: &client::Paths) -> Option<std::fs::File> {
    let file = std::fs::File::open(paths.dir.join(".lock")).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's nonblocking writer observes whether the public key callback retains its shared lock"
    )]
    let outcome = file.try_lock();
    match outcome {
        Ok(()) => Some(file),
        Err(std::fs::TryLockError::WouldBlock) => None,
        Err(error) => panic!("writer lock: {error}"),
    }
}

#[test]
fn a_busy_vault_cannot_serve_or_poison_commons_access() {
    let vault = Vault::new();
    let before = vault.digest();
    let held = writer(&vault.paths).unwrap();
    assert!(vault.bootstrap.access_in(&vault.journal).unwrap().is_none());
    drop(held);
    assert_eq!(
        vault.digest(),
        before,
        "releasing a writer changes no inputs"
    );
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    assert!(access.is_current(&vault.journal));

    let held = writer(&vault.paths).unwrap();
    assert!(vault.bootstrap.access_in(&vault.journal).unwrap().is_none());
    assert!(
        !access.is_current(&vault.journal),
        "a warm digest is still observed under the lock"
    );
    assert!(
        access
            .with_key(&vault.journal, |_| panic!("busy key exposed"))
            .is_err()
    );
    drop(held);
    assert_eq!(vault.digest(), before);
    assert!(vault.bootstrap.access_in(&vault.journal).unwrap().is_some());
    assert!(access.is_current(&vault.journal));
}

#[test]
fn current_and_historical_key_callbacks_retain_the_vault_observation() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let historical = vault
        .bootstrap
        .historical(&vault.journal, &access, *access.basis())
        .unwrap();
    access
        .with_key(&vault.journal, |_| {
            assert!(
                writer(&vault.paths).is_none(),
                "current key callback released its observation"
            );
        })
        .unwrap();
    historical
        .with_key(&vault.journal, |_| {
            assert!(
                writer(&vault.paths).is_none(),
                "historical key callback released its observation"
            );
        })
        .unwrap();
    assert!(
        writer(&vault.paths).is_some(),
        "both callbacks released their observations"
    );
}

/// This Mac's recipient is kept for the identity it was read from ([`client::identity_recipient`]):
/// while the identity file stands it is opened no more, and an identity put in its place answers
/// its own recipient.
/// NEGATIVE CONTROL (2026-11-08): a recipient kept for any identity answers the one put in its
/// place with the first one's.
#[test]
fn a_kept_recipient_is_the_recipient_of_the_identity_its_file_holds() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = client::Paths::for_home(root.path());
    let first = client::machine_identity(&paths).unwrap();
    let path = paths.dir.join("identity");
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        assert_eq!(
            client::identity_recipient(&paths).unwrap(),
            Some(first.recipient())
        );
        let reads = crate::fsx::reads_of(&path);
        assert_eq!(
            client::identity_recipient(&paths).unwrap(),
            Some(first.recipient())
        );
        assert_eq!(crate::fsx::reads_of(&path), reads, "opened while it stood");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(client::identity_recipient(&paths).unwrap(), None);
        let second = client::machine_identity(&paths).unwrap();
        assert_ne!(second.recipient(), first.recipient());
        assert_eq!(
            client::identity_recipient(&paths).unwrap(),
            Some(second.recipient()),
            "the identity put in the first one's place"
        );
    });
}

/// A warm access's quiet check opens this Mac's identity no more while the vault stands: it
/// compares the recipient kept for the identity file ([`client::identity_recipient`]) and reads
/// the identity only to judge a changed log afresh.
/// NEGATIVE CONTROL (2026-11-08): with the identity read before the check's quiet answer, as on
/// main before this law, two checks open it twice (`left: 2`).
#[test]
fn a_warm_access_check_opens_no_identity_while_the_vault_stands() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let identity = vault.paths.dir.join("identity");
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        assert!(access.is_current(&vault.journal));
        let reads = crate::fsx::reads_of(&identity);
        for _ in 0..2 {
            assert!(access.is_current(&vault.journal));
        }
        assert_eq!(
            crate::fsx::reads_of(&identity) - reads,
            0,
            "a quiet check opened the identity"
        );
    });
}

/// A warm access check and a warm access each observe the vault's state lock once, for the call:
/// the log and this Mac's denial they keep are given under that observation
/// ([`client::trusted_commons_log_under`]), with no other observation of their own.
/// NEGATIVE CONTROLS (2026-11-08): with `log_observation_is_current` back on
/// `trusted_commons_log_now`, a check observes the lock twice (`left: 2`, `a check's
/// observations`); with `Bootstrap::access_in_observed`'s log back on it, an access twice
/// (`left: 2`, `an access's observations`).
#[test]
fn a_warm_access_check_observes_the_state_lock_once() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        assert!(access.is_current(&vault.journal));
        let observations = client::lock_observations_of(&vault.paths);
        assert!(access.is_current(&vault.journal));
        assert_eq!(
            client::lock_observations_of(&vault.paths) - observations,
            1,
            "a check's observations"
        );
        assert!(vault.bootstrap.access_in(&vault.journal).unwrap().is_some());
        let observations = client::lock_observations_of(&vault.paths);
        assert!(vault.bootstrap.access_in(&vault.journal).unwrap().is_some());
        assert_eq!(
            client::lock_observations_of(&vault.paths) - observations,
            1,
            "an access's observations"
        );
    });
}

#[test]
fn a_failed_vault_read_cannot_poison_commons_access_without_an_input_change() {
    struct ReadFault(std::path::PathBuf);
    impl Drop for ReadFault {
        fn drop(&mut self) {
            crate::fsx::fail_reads(&self.0, 0);
        }
    }

    let vault = Vault::new();
    let before = vault.digest();
    let cache = vault.paths.dir.join("vault");
    let fault = ReadFault(cache.clone());
    crate::fsx::fail_reads(&cache, usize::MAX);
    let failed = vault.bootstrap.access_in(&vault.journal);
    drop(fault);
    assert!(
        failed.is_err(),
        "unreadable custody is not a settled absence"
    );
    assert_eq!(vault.digest(), before, "the fault changes no metadata");
    assert!(vault.bootstrap.access_in(&vault.journal).unwrap().is_some());
}

#[test]
fn warm_commons_keys_refuse_a_vault_that_loses_private_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    struct Permissions(std::path::PathBuf, std::fs::Permissions);
    impl Drop for Permissions {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }

    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let historical = vault
        .bootstrap
        .historical(&vault.journal, &access, *access.basis())
        .unwrap();
    let cache = vault.paths.dir.join("vault");
    let restore = Permissions(
        cache.clone(),
        std::fs::metadata(&cache).unwrap().permissions(),
    );
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o644)).unwrap();
    let fresh = vault.bootstrap.access_in(&vault.journal);
    let current = access.is_current(&vault.journal);
    let current_called = std::cell::Cell::new(false);
    let current_key = access.with_key(&vault.journal, |_| current_called.set(true));
    let history_called = std::cell::Cell::new(false);
    let history_key = historical.with_key(&vault.journal, |_| history_called.set(true));
    drop(restore);

    assert!(fresh.is_err(), "the vault is no longer privately readable");
    assert!(!current, "permission changes invalidate a warm digest");
    assert!(current_key.is_err());
    assert!(history_key.is_err());
    assert!(!current_called.get());
    assert!(!history_called.get());
    assert!(
        access.is_current(&vault.journal),
        "restored private custody recovers"
    );
}

#[test]
fn warm_historical_access_requires_available_epoch_and_wrap_records() {
    use crate::membership_log::{
        Body, Log, Record, RecordId,
        history::{HistoryView, RecordReader},
    };
    use std::cell::Cell;

    #[derive(Clone, Copy)]
    enum Fault {
        Missing,
        Wrong,
        Unreadable,
    }
    struct Reads<'a> {
        log: &'a Log,
        fault: Cell<Option<(RecordId, Fault)>>,
        failed_reads: Cell<usize>,
    }
    impl RecordReader for Reads<'_> {
        fn read(&self, id: &RecordId) -> Result<Option<&Record>> {
            if let Some((wanted, fault)) = self.fault.get()
                && wanted == *id
            {
                self.failed_reads.set(self.failed_reads.get() + 1);
                return match fault {
                    Fault::Missing => Ok(None),
                    Fault::Wrong => Ok(Some(self.log.genesis())),
                    Fault::Unreadable => Err(Error::new("historical record could not be read")),
                };
            }
            Ok(self.log.get(id))
        }
    }

    let vault = Vault::new();
    let identity = client::machine_identity(&vault.paths).unwrap();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let basis = *access.basis();
    let original = vault
        .bootstrap
        .historical(&vault.journal, &access, basis)
        .unwrap();
    let key = original.with_key(&vault.journal, |key| *key).unwrap();
    let (log, _) = client::trusted_commons_log(&vault.paths, vault.bootstrap.trust())
        .unwrap()
        .unwrap();
    let wrap = *log.records().find(|record| matches!(record.body(), Body::Wrap { epoch, recipient, .. } if *epoch == basis && *recipient == identity.recipient())).unwrap().id();
    let before = crate::membership_log::store::encode_records(log.records());
    let reads = Reads {
        log: &log,
        fault: Cell::new(None),
        failed_reads: Cell::new(0),
    };
    let history = HistoryView::with_reader(&log, &reads);
    let generation = history.lease_complete().unwrap().generation();

    for required in [basis, wrap] {
        for fault in [Fault::Missing, Fault::Wrong, Fault::Unreadable] {
            reads.fault.set(Some((required, fault)));
            reads.failed_reads.set(0);
            assert!(access.is_current(&vault.journal));
            let observed = history
                .lease_complete()
                .and_then(|lease| access.historical_from_lease(&identity, basis, &lease));
            let failed = observed.is_err();
            let called = Cell::new(false);
            if let Ok(Some(past)) = observed {
                let _ = past.with_key(&vault.journal, |_| called.set(true));
            }
            assert!(
                reads.failed_reads.get() > 0,
                "the exact required record was read"
            );
            assert!(
                failed,
                "a warm historical cache cannot hide unavailable evidence"
            );
            assert!(!called.get());
            reads.fault.set(None);
            assert_eq!(history.lease_complete().unwrap().generation(), generation);
            let recovered = access
                .historical_from_lease(&identity, basis, &history.lease_complete().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(recovered.with_key(&vault.journal, |key| *key).unwrap(), key);
        }
    }
    assert_eq!(
        crate::membership_log::store::encode_records(log.records()),
        before
    );
}

#[test]
fn a_pending_epoch_cannot_hit_cached_historical_absence() {
    use crate::membership_log::{Body, EpochAudience, Inserted, history::HistoryView};

    let vault = Vault::new();
    let identity = client::machine_identity(&vault.paths).unwrap();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let (mut log, _) = client::trusted_commons_log(&vault.paths, vault.bootstrap.trust())
        .unwrap()
        .unwrap();
    let request = vault.bootstrap.request().unwrap();
    let actor = request.binding().actor();
    let incarnation = *request.binding().incarnation();
    let (parent, epoch) = vault.bootstrap.device().with_seed(|seed| {
        let mut future = log.clone();
        let parent = future
            .draft(
                &actor,
                2,
                Body::Name {
                    name: "member".into(),
                },
            )
            .sign(seed, incarnation)
            .unwrap();
        future.insert(parent.clone()).unwrap();
        let (epoch, _) = future
            .draft_epoch(&actor, 3, EpochAudience::Commons, 100)
            .unwrap();
        (parent, epoch.sign(seed, incarnation).unwrap())
    });
    let absent = HistoryView::new(&log);
    let generation = absent.lease_complete().unwrap().generation();
    assert!(
        access
            .historical_from_lease(&identity, *epoch.id(), &absent.lease_complete().unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(log.insert(epoch.clone()).unwrap(), Inserted::Waiting);
    let pending = HistoryView::new(&log);
    assert_eq!(pending.lease_complete().unwrap().generation(), generation);
    assert!(
        access
            .historical_from_lease(&identity, *epoch.id(), &pending.lease_complete().unwrap())
            .is_err()
    );
    assert!(log.get(parent.id()).is_none(), "the parent has not arrived");
}

/// A batch answers each basis as it alone would, in order: one the log opens, and one it does not
/// hold, which falls back to the control journal and to a handoff, and fails.
///
/// NEGATIVE CONTROL: `Bootstrap::historicals` answering in reverse order fails it.
#[test]
fn a_batch_of_historical_bases_answers_each_as_it_alone_would() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let bases = [*access.basis(), [7; 32]];
    let opened = |answer: &Result<Historical>| {
        answer
            .as_ref()
            .map(|past| {
                let key = past.with_key(&vault.journal, |key| *key).unwrap();
                (*past.source(), past.generation(), key)
            })
            .map_err(Clone::clone)
    };
    let alone: Vec<_> = bases
        .iter()
        .map(|basis| opened(&vault.bootstrap.historical(&vault.journal, &access, *basis)))
        .collect();
    assert!(alone[0].is_ok(), "CONTROL: the vault opens its own epoch");
    assert!(alone[1].is_err(), "CONTROL: nothing holds the other");
    let batch: Vec<_> = vault
        .bootstrap
        .historicals(&vault.journal, &access, &bases)
        .iter()
        .map(opened)
        .collect();
    assert_eq!(batch, alone);
    assert!(
        vault
            .bootstrap
            .historicals(&vault.journal, &access, &[])
            .is_empty()
    );
}
