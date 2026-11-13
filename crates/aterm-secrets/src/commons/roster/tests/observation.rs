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
        commons_log_state_digest(&self.paths, heads, None).unwrap()
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

/// A remembered access is judged afresh once the device claim a node denial holds this Mac's
/// device to is replaced (`Device::existing_binding_for_observation` reads it): the state a log
/// observation stands on (`commons_log_state_digest`) names it, as it names the device key.
/// NEGATIVE CONTROL (2026-11-10): with the digest naming no device claim, the replaced claim
/// leaves it as it was (`a replaced device claim`).
#[test]
fn a_replaced_device_claim_moves_the_state_a_log_observation_stands_on() {
    let vault = Vault::new();
    let claim = vault.paths.dir.join("commons").join("device.claim");
    let held = std::fs::read(&claim).expect("CONTROL: the vault holds a device claim");
    let before = vault.digest();
    assert_eq!(
        vault.digest(),
        before,
        "CONTROL: a digest of a vault that stands"
    );
    let mut replaced = held.clone();
    replaced[0] ^= 1;
    let staged = claim.with_extension("replaced");
    std::fs::write(&staged, &replaced).unwrap();
    std::fs::rename(&staged, &claim).unwrap();
    assert_ne!(vault.digest(), before, "a replaced device claim");
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
            client::identity_recipient(&paths, None).unwrap(),
            Some(first.recipient())
        );
        let reads = crate::fsx::reads_of(&path);
        assert_eq!(
            client::identity_recipient(&paths, None).unwrap(),
            Some(first.recipient())
        );
        assert_eq!(crate::fsx::reads_of(&path), reads, "opened while it stood");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(client::identity_recipient(&paths, None).unwrap(), None);
        let second = client::machine_identity(&paths).unwrap();
        assert_ne!(second.recipient(), first.recipient());
        assert_eq!(
            client::identity_recipient(&paths, None).unwrap(),
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

/// The `lstat`s `f` took of each path under `dir` ([`crate::fsx::looks_under`]).
fn looks_during(
    dir: &std::path::Path,
    f: impl FnOnce(),
) -> std::collections::BTreeMap<std::path::PathBuf, usize> {
    let before = crate::fsx::looks_under(dir);
    f();
    crate::fsx::looks_under(dir)
        .into_iter()
        .filter_map(|(path, looks)| {
            let looked = looks - before.get(&path).copied().unwrap_or(0);
            (looked > 0).then_some((path, looked))
        })
        .collect()
}

/// A warm access check and a warm access each take one stamping `lstat` of each file they stamp
/// ([`crate::fsx::lstat_in`], through one [`crate::fsx::Glance`]): this Mac's recipient, the
/// membership log's memo, this Mac's own denial and the state the log stands on name many of the
/// same files, and the call stamps each once; the next call looks again. The state lock's own
/// looks at its folder and file (`StateLock::open_observed`, and `StateLock::guards` on its own
/// call) are no stamps and are not counted here; a check's judgement of its observation through
/// its glance (`StateLock::observes_in`) is, as one look at the lock file.
/// NEGATIVE CONTROLS (2026-11-08): with `log_observation_is_current`'s log memo stamping through
/// a look of its own, a check takes two `lstat`s of the identity, the state, the source and the
/// envelope (`a check's looks`); with `Bootstrap::access_in_observed`'s log
/// memo stamping through its own, an access takes two of each file both memos stamp (`an
/// access's looks`).
#[test]
fn a_warm_access_check_looks_at_each_file_once() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let home = vault.paths.home.clone();
    let shared = [
        vault.paths.dir.clone(),
        vault.paths.dir.join("identity"),
        vault.paths.state_file(),
        vault.paths.dir.join("source.toml"),
        vault.paths.cache_file(),
    ];
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        assert!(access.is_current(&vault.journal));
        let check = || looks_during(&home, || assert!(access.is_current(&vault.journal)));
        let looked = check();
        for file in &shared {
            assert_eq!(looked.get(file), Some(&1), "a check's looks: {looked:#?}");
        }
        assert!(
            looked.values().all(|looks| *looks == 1),
            "a check's looks: {looked:#?}"
        );
        assert_eq!(check(), looked, "the next check looks again");

        let access_in = || {
            looks_during(&home, || {
                assert!(vault.bootstrap.access_in(&vault.journal).unwrap().is_some());
            })
        };
        access_in();
        let looked = access_in();
        for file in &shared {
            assert_eq!(looked.get(file), Some(&1), "an access's looks: {looked:#?}");
        }
        assert!(
            looked.values().all(|looks| *looks == 1),
            "an access's looks: {looked:#?}"
        );
        assert_eq!(access_in(), looked, "the next access looks again");
    });
}

/// What a Commons peer check's stages ask through their looks, asked through one
/// ([`client::VaultLook`]): the roster and its deny list, every record held, the acknowledged log
/// and that log's roster. The records it read.
fn ask_through_a_look(vault: &Vault) -> crate::membership_log::Store {
    let look = client::VaultLook::new(&vault.paths);
    look.swarm_roster_until(std::time::Instant::now()).unwrap();
    look.swarm_denied_nodes().unwrap();
    let held = look.records().unwrap();
    let client::CommonsLog::Trusted(log, _) = look
        .acknowledged_commons_log(vault.bootstrap.trust())
        .unwrap()
    else {
        panic!("a trusted log");
    };
    look.local_swarm_roster_for_log(&log).unwrap();
    held
}

/// A warm look ([`client::VaultLook`]) asks each memo a Commons peer check's stages ask under one
/// observation of the state lock, takes one stamping `lstat` of each file they stamp, and opens
/// none; the records it gives are those a fresh read gives, and the next look looks again.
/// NEGATIVE CONTROLS (2026-11-08): with `VaultLook::ask` on each memo's own call
/// (`Memo::read_keyed`), a look observes the lock five times (`a look's observations`); with
/// `VaultLook::records` on `load_records`, it opens the files that read reads (`a look's
/// reads`).
#[test]
fn a_warm_look_looks_at_each_file_once_and_opens_none() {
    let vault = Vault::new();
    let home = vault.paths.home.clone();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let held = ask_through_a_look(&vault);
        assert_eq!(
            held.encode(),
            client::load_records(&vault.paths).unwrap().encode()
        );
        let observations = client::lock_observations_of(&vault.paths);
        let reads = crate::fsx::reads_under(&home);
        let looked = looks_during(&home, || {
            assert_eq!(ask_through_a_look(&vault).encode(), held.encode());
        });
        assert_eq!(
            client::lock_observations_of(&vault.paths) - observations,
            1,
            "a look's observations"
        );
        assert_eq!(crate::fsx::reads_under(&home), reads, "a look's reads");
        for file in [
            vault.paths.dir.clone(),
            vault.paths.dir.join("source.toml"),
            vault.paths.state_file(),
            vault.paths.cache_file(),
        ] {
            assert_eq!(looked.get(&file), Some(&1), "a look's looks: {looked:#?}");
        }
        assert!(
            looked.values().all(|looks| *looks == 1),
            "a look's looks: {looked:#?}"
        );
        assert_eq!(
            looks_during(&home, || {
                ask_through_a_look(&vault);
            }),
            looked,
            "the next look looks again"
        );
    });
}

/// A look judges whether its observation guards the lock at the vault's path once, through its
/// glance ([`client::StateLock::observes_in`]): a warm look's memo asks (`Memo::read_under`) and
/// an access's currency checks through it (`VaultLook::held`, then the memo ask under it), each
/// of which reads under the look only once the look judged it, take one `lstat` of the lock's
/// path, the look's, and judge it on their own calls no more than the look's own observation did
/// (`client::lock_judgements_of`), where the asks' own calls each judge their own.
/// NEGATIVE CONTROLS: with `VaultLook::held` judging on its own call (`StateLock::observes`,
/// 2026-11-10), or with `Memo::read_under`'s whole guard judging on its own call
/// (`under.lock().observes(paths)` in place of `under.caching_for(paths) ||
/// under.lock().observes_in(paths, Some(under.glance()))`, 2026-11-11: `left: 5, right: 1`), a
/// look's asks judge the lock on their own calls (`a look's own lock judgements`).
#[test]
fn a_look_judges_the_lock_it_observes_once() {
    let vault = Vault::new();
    let home = vault.paths.home.clone();
    let lock = vault.paths.dir.join(".lock");
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let asks = || {
        let look = client::VaultLook::new(&vault.paths);
        look.swarm_roster_until(std::time::Instant::now()).unwrap();
        look.records().unwrap();
        assert!(access.is_current_in(&vault.journal, &look));
        assert!(access.is_current_in(&vault.journal, &look));
    };
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        asks();
        let judged = client::lock_judgements_of(&vault.paths);
        let looked = looks_during(&home, asks);
        assert_eq!(
            client::lock_judgements_of(&vault.paths) - judged,
            1,
            "a look's own lock judgements"
        );
        assert_eq!(looked.get(&lock), Some(&1), "a look's looks at its lock");

        let judged = client::lock_judgements_of(&vault.paths);
        client::swarm_roster_now_until(&vault.paths, std::time::Instant::now()).unwrap();
        client::records_now(&vault.paths).unwrap();
        assert!(
            client::lock_judgements_of(&vault.paths) - judged >= 2,
            "CONTROL: the asks' own calls judge their own"
        );
    });
}

/// A look reads the records this Mac holds as they stand when it looks
/// ([`client::VaultLook::records`]): one taken after they changed gives what a fresh read of them
/// gives, by the stamps of the records' selection and data alone, every other file unmoved.
/// NEGATIVE CONTROL (2026-11-08): with `Memo::kept` matching a memo's stamps of its listed files
/// alone, not of the records' selection, the look gives the records from before the change (`a
/// look reads the records as they stand`).
#[test]
fn a_look_reads_the_records_as_they_stand_when_it_looks() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let held = ask_through_a_look(&vault);
        assert_eq!(ask_through_a_look(&vault).encode(), held.encode());
        assert!(held.len() > 1, "a log past its genesis");
        let data = crate::records_storage::reader_path(&vault.paths.records_file()).unwrap();
        let genesis =
            crate::membership_log::store::encode_records(held.log().unwrap().records().take(1));
        crate::fsx::write_atomic(&data, &genesis, 0o600).unwrap();
        let fresh = client::load_records(&vault.paths).unwrap();
        assert_eq!(fresh.len(), 1);
        assert_eq!(
            client::VaultLook::new(&vault.paths)
                .records()
                .unwrap()
                .encode(),
            fresh.encode(),
            "a look reads the records as they stand"
        );
    });
}

/// A look taken while a writer holds the state lock observes nothing, and reads the records
/// afresh, never what its memo kept from before that writer: a writer mid-apply meets the read,
/// as it did before the look.
/// NEGATIVE CONTROL (2026-11-08): with the records' memo not quiet (`Memo::of(Set::Vault,
/// false)`), the look under the writer gives what it kept and opens nothing (`a look under a
/// writer reads the records afresh`).
#[test]
fn a_look_under_a_writer_reads_the_records_afresh() {
    let vault = Vault::new();
    let home = vault.paths.home.clone();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let held = ask_through_a_look(&vault);
        let data = crate::records_storage::reader_path(&vault.paths.records_file()).unwrap();
        let writing = writer(&vault.paths).unwrap();
        let reads = crate::fsx::reads_of(&data);
        let look = client::VaultLook::new(&vault.paths);
        assert_eq!(look.records().unwrap().encode(), held.encode());
        assert!(
            crate::fsx::reads_of(&data) > reads,
            "a look under a writer reads the records afresh: {:#?}",
            crate::fsx::reads_under(&home)
        );
        drop(look);
        drop(writing);
    });
}

/// The records asked outside a look ([`client::records_now`]: each control exchange's membership
/// store) are the same memo's: a warm ask reads none of the records' data and gives what a fresh
/// read gives, an ask after the records changed gives them as they stand, and an ask while a
/// writer holds the state lock reads them afresh.
/// NEGATIVE CONTROLS (2026-11-09): with `records_now` on `load_records`, the warm ask reads the
/// data (`a warm ask reads nothing`); with the records' memo not quiet, the ask under the writer
/// gives what it kept (`an ask under a writer reads the records afresh`).
#[test]
fn an_ask_for_the_records_outside_a_look_asks_their_memo() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let held = client::records_now(&vault.paths).unwrap();
        assert_eq!(
            held.encode(),
            client::load_records(&vault.paths).unwrap().encode()
        );
        assert!(held.len() > 1, "a log past its genesis");
        let data = crate::records_storage::reader_path(&vault.paths.records_file()).unwrap();
        let reads = crate::fsx::reads_of(&data);
        assert_eq!(
            client::records_now(&vault.paths).unwrap().encode(),
            held.encode()
        );
        assert_eq!(
            crate::fsx::reads_of(&data),
            reads,
            "a warm ask reads nothing"
        );
        let genesis =
            crate::membership_log::store::encode_records(held.log().unwrap().records().take(1));
        crate::fsx::write_atomic(&data, &genesis, 0o600).unwrap();
        let fresh = client::load_records(&vault.paths).unwrap();
        assert_eq!(fresh.len(), 1);
        assert_eq!(
            client::records_now(&vault.paths).unwrap().encode(),
            fresh.encode(),
            "an ask reads the records as they stand"
        );
        let writing = writer(&vault.paths).unwrap();
        let reads = crate::fsx::reads_of(&data);
        assert_eq!(
            client::records_now(&vault.paths).unwrap().encode(),
            fresh.encode()
        );
        assert!(
            crate::fsx::reads_of(&data) > reads,
            "an ask under a writer reads the records afresh"
        );
        drop(writing);
    });
}

/// What a table session's open and the checks a local batch makes at once after it ask
/// (`aterm-db`'s `Replica::admit`), through `look`: the custody, the access, the access's
/// currency twice, and the peer's roster. The access it opened, and each check's answer.
fn a_session_through(vault: &Vault, look: &client::VaultLook<'_>) -> SessionJudged {
    let custody = vault.bootstrap.revalidate_custody_in(look);
    let access = vault
        .bootstrap
        .access_observed_in_look(&vault.journal, look);
    let checks = match &access {
        Ok(Some(access)) => Some((
            access.is_current_in(&vault.journal, look),
            access.is_current_observed_in(&vault.journal, look),
        )),
        _ => None,
    };
    let roster = look.swarm_roster_until(std::time::Instant::now());
    SessionJudged {
        custody,
        access,
        checks,
        roster,
    }
}

/// The same asks, each on its own call, as before the look.
fn a_session_on_own_calls(vault: &Vault) -> SessionJudged {
    let custody = vault.bootstrap.revalidate_custody();
    let access = vault.bootstrap.access_observed_in(&vault.journal);
    let checks = match &access {
        Ok(Some(access)) => Some((
            access.is_current(&vault.journal),
            access.is_current_observed(&vault.journal),
        )),
        _ => None,
    };
    let roster = client::swarm_roster_now_until(&vault.paths, std::time::Instant::now());
    SessionJudged {
        custody,
        access,
        checks,
        roster,
    }
}

/// What a session's asks answered.
struct SessionJudged {
    custody: crate::Result<()>,
    access: crate::Result<Option<Access>>,
    checks: Option<(bool, crate::Result<bool>)>,
    roster: crate::Result<Option<crate::swarm_roster::SwarmRoster>>,
}

impl SessionJudged {
    /// The answers as comparable values: an error by its kind, an access by what it was opened
    /// on.
    fn verdict(&self) -> impl PartialEq + std::fmt::Debug {
        let kind = |error: &crate::Error| format!("{:?}", error.kind());
        (
            self.custody.as_ref().map_err(kind).copied(),
            self.access.as_ref().map_err(kind).map(|access| {
                access
                    .as_ref()
                    .map(|access| (access.head, access.basis, access.log_heads_digest))
            }),
            self.checks
                .as_ref()
                .map(|(current, observed)| (*current, observed.as_ref().map_err(kind).copied())),
            self.roster.as_ref().map_err(kind).cloned(),
        )
    }
}

/// A warm table session's look ([`client::VaultLook`]: `Bootstrap::revalidate_custody_in`,
/// `Bootstrap::access_observed_in_look`, `Access::is_current_in`,
/// `Access::is_current_observed_in` and `VaultLook::swarm_roster_until`) observes the state lock
/// once for the open and its checks,
/// takes one `lstat` of each file they stamp or digest (the custody's pinned files among them),
/// opens no file the asks on their own calls do not, and answers as those calls do; the next look
/// looks again.
/// NEGATIVE CONTROLS (2026-11-09): with `Access::is_current_observed_in` on its own call, a
/// session observes the lock three times (`a session look's observations`); with
/// `Bootstrap::access_observed_in_look` on its own call, twice; with
/// `Bootstrap::revalidate_custody_in` on its own call, twice; with `VaultLook::swarm_roster_until`
/// asking its memo on its own call, twice. (2026-11-10) With the custody's digests on their own
/// calls (`Bootstrap::revalidate_custody_in` checking `revalidate_immutable_custody`), the
/// identity and the device's age identity and claim are looked at twice (`a session look's
/// looks`).
#[test]
fn a_session_look_observes_once_and_judges_as_its_own_calls() {
    let vault = Vault::new();
    let home = vault.paths.home.clone();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let own = a_session_on_own_calls(&vault);
        assert!(matches!(own.access, Ok(Some(_))), "{:?}", own.verdict());
        assert!(matches!(own.checks, Some((true, Ok(true)))));
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        let own_reads = crate::fsx::reads_under(&home);
        a_session_on_own_calls(&vault);
        let own_reads = read_since(&crate::fsx::reads_under(&home), &own_reads);

        let observations = client::lock_observations_of(&vault.paths);
        let reads = crate::fsx::reads_under(&home);
        let mut judged = None;
        let looked = looks_during(&home, || {
            judged = Some(a_session_through(
                &vault,
                &client::VaultLook::new(&vault.paths),
            ));
        });
        assert_eq!(
            client::lock_observations_of(&vault.paths) - observations,
            1,
            "a session look's observations"
        );
        let read = read_since(&crate::fsx::reads_under(&home), &reads);
        assert!(
            read.iter()
                .all(|(path, reads)| own_reads.get(path) >= Some(reads)),
            "a session look's reads {read:#?}, its own calls' {own_reads:#?}"
        );
        assert_eq!(judged.unwrap().verdict(), own.verdict());
        let commons = vault.paths.dir.join("commons");
        for file in [
            vault.paths.dir.clone(),
            vault.paths.state_file(),
            vault.paths.dir.join("source.toml"),
            vault.paths.cache_file(),
            vault.paths.dir.join("identity"),
            vault.paths.dir.join("commons.claim"),
            commons.join("device.age"),
            commons.join("device.claim"),
            commons.join("default.pin"),
            vault.bootstrap.journal_dir().join("control.anchor"),
        ] {
            assert_eq!(
                looked.get(&file),
                Some(&1),
                "a session look's looks of {}: {looked:#?}",
                file.display()
            );
        }
        assert!(
            looked.values().all(|looks| *looks == 1),
            "a session look's looks: {looked:#?}"
        );
        assert_eq!(
            looks_during(&home, || {
                a_session_through(&vault, &client::VaultLook::new(&vault.paths));
            }),
            looked,
            "the next look looks again"
        );
    });
}

/// What a warm table session's open and checks through `look` ([`a_session_through`]), and two
/// more checks of the currency of the access it opened, derived of the moment `look` stands for,
/// with what they answered.
fn derived_through(
    vault: &Vault,
    look: &client::VaultLook<'_>,
) -> (client::LookDerivations, SessionJudged) {
    let before = client::test_look_derivations();
    let judged = a_session_through(vault, look);
    let access = judged
        .access
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .expect("CONTROL: the look opens an access");
    for _ in 0..2 {
        assert!(access.is_current_in(&vault.journal, look));
    }
    (client::test_look_derivations().since(before), judged)
}

/// A look judges whether its observation guards the lock at the vault's path once
/// (`VaultLook::held`): a warm table session's open and checks through it, and two more checks
/// of its access's currency, judge it once through the look's glance
/// (`client::test_look_derivations`) and never again after they found it held, and answer as the
/// asks' own calls do, through a look of its own and through one standing on a source read; the
/// next look judges it again.
/// NEGATIVE CONTROLS (2026-11-11): with `Moment::hold` judging whether or not its look found the
/// lock held, a look judges it 8 times (`left: 8, right: 1`, `a look's lock judgments`); with
/// `Moment::hold` setting `held` before it judges, `a_source_read_look_judges_as_a_look_of_its_own`
/// and `a_session_look_with_no_observation_judges_as_its_own_calls` open an access under the
/// writer (`a source read look at a lock put away`, `a look at a lock put away`).
#[test]
fn a_look_judges_its_lock_once() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let own = a_session_on_own_calls(&vault);
        assert!(matches!(own.checks, Some((true, Ok(true)))));
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        let source_read = || {
            client::try_commons_source_read(&vault.paths)
                .unwrap()
                .expect("no writer holds the vault")
        };
        for which in ["its own", "a source read's", "the next one"] {
            let look = if which == "a source read's" {
                source_read().into_look(&vault.paths)
            } else {
                client::VaultLook::new(&vault.paths)
            };
            let (derived, judged) = derived_through(&vault, &look);
            assert_eq!(
                derived.derived.lock_judgments, 1,
                "a look's lock judgments ({which})"
            );
            assert_eq!(
                derived.rederived_after_kept.lock_judgments, 0,
                "a look's lock judged again ({which})"
            );
            assert_eq!(
                judged.verdict(),
                own.verdict(),
                "a look judging its lock once ({which})"
            );
        }
    });
}

/// A look stamps each set of files its memos read once (`Under::stamps`): a warm table session's
/// open and checks through it, and two more checks of its access's currency, stamp the vault's
/// files (`Set::Vault`) once and the Commons observation's (`Set::Observation`) once through the
/// look's glance (`client::test_look_derivations`), never again after they kept them, and answer
/// as the asks' own calls do; the next look stamps them again.
/// NEGATIVE CONTROLS (2026-11-11): with `Under::stamps` keeping nothing, a look stamps the vault's
/// files 8 times and the observation's twice (`left: (8, 2), right: (1, 1)`, `a look stamps each
/// set once`); with the stamps kept past their look (in a thread's own static), a look after the
/// first stamps nothing (`left: (0, 0), right: (1, 1)`, the same).
#[test]
fn a_look_stamps_each_input_set_once() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let own = a_session_on_own_calls(&vault);
        assert!(matches!(own.checks, Some((true, Ok(true)))));
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        for which in ["a look", "the next look"] {
            let (derived, judged) = derived_through(&vault, &client::VaultLook::new(&vault.paths));
            assert_eq!(
                (
                    derived.derived.vault_stamps,
                    derived.derived.observation_stamps
                ),
                (1, 1),
                "{which} stamps each set once"
            );
            assert_eq!(
                (
                    derived.rederived_after_kept.vault_stamps,
                    derived.rederived_after_kept.observation_stamps
                ),
                (0, 0),
                "{which} stamps a set again"
            );
            assert_eq!(
                judged.verdict(),
                own.verdict(),
                "{which} stamping each set once"
            );
        }
    });
}

/// A look that lets go of what it derived (`VaultLook::forget_derived`, as a retained table
/// session's checks do before each) derives it again at its next check through its glance, and
/// keeps its judgement of its lock: a warm table session's open and checks through a look, then
/// the same again after it forgot, stamp each set of files, look at the state and digest the log's
/// state once more and judge the lock no more, and answer as the asks' own calls do.
/// NEGATIVE CONTROLS (2026-11-11): with `VaultLook::forget_derived` letting go of nothing, the
/// check after it derives nothing (every count 0, `a second check after the look forgot derives
/// it again`); with it keeping the state's look and the log's state digest, the check after it
/// derives neither (`state_looks: 0, log_states: 0`, the same); with it keeping the currency's
/// judgements, the check after it judges none (`currency: 0`, the same).
#[test]
fn a_look_that_forgets_what_it_derived_derives_it_again() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let own = a_session_on_own_calls(&vault);
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        let look = client::VaultLook::new(&vault.paths);
        derived_through(&vault, &look);
        for check in ["a second check", "a third check"] {
            look.forget_derived();
            let (derived, judged) = derived_through(&vault, &look);
            assert_eq!(
                derived.derived,
                client::Derivations {
                    lock_judgments: 0,
                    vault_stamps: 1,
                    observation_stamps: 1,
                    state_looks: 1,
                    log_states: 1,
                    currency: 1,
                },
                "{check} after the look forgot derives it again"
            );
            assert_eq!(
                derived.rederived_after_kept,
                client::Derivations::default(),
                "{check} derives each value once"
            );
            assert_eq!(judged.verdict(), own.verdict(), "{check}");
        }
    });
}

/// A look digests the state a Commons log observation stands on once for each log
/// (`Under::log_state`) and looks at `state.toml` once (`Under::state_look`): a warm table
/// session's open and checks through it, and two more checks of its access's currency, derive the
/// log's state digest once and the state's look once (`client::test_look_derivations`), never
/// again after they kept them, and answer as the asks' own calls do; the next look derives them
/// again.
/// NEGATIVE CONTROLS (2026-11-11): with `Under::log_state` keeping nothing, a look digests the
/// log's state 5 times (`left: (5, 1), right: (1, 1)`, `a look digests its log state and looks at
/// its state once`); with `Under::state_look` keeping nothing, it looks at the state twice (`left:
/// (1, 2)`, the same).
#[test]
fn a_look_digests_its_log_state_once() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let own = a_session_on_own_calls(&vault);
        assert!(matches!(own.checks, Some((true, Ok(true)))));
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        for which in ["a look", "the next look"] {
            let (derived, judged) = derived_through(&vault, &client::VaultLook::new(&vault.paths));
            assert_eq!(
                (derived.derived.log_states, derived.derived.state_looks),
                (1, 1),
                "{which} digests its log state and looks at its state once"
            );
            assert_eq!(
                (
                    derived.rederived_after_kept.log_states,
                    derived.rederived_after_kept.state_looks
                ),
                (0, 0),
                "{which} derives its log state again"
            );
            assert_eq!(
                judged.verdict(),
                own.verdict(),
                "{which} digesting its log state once"
            );
        }
    });
}

/// A value derived from a file the look found unsettled is not kept (`fsx::fresh_looks`):
/// `source.toml` rewritten with the same bytes a second before a look began (within the settled
/// bound, `fsx::SETTLED`) is read afresh by each digest of the log's state through the look, and
/// the look keeps no digest, so each derives it again; through a look begun past the settled
/// bound, the first digest reads the rewritten file, whose digest the read keeps, and keeps
/// nothing, the second is given the file's kept digest and keeps the log's, and the third derives
/// nothing.
/// NEGATIVE CONTROL (2026-11-11): with `Under::log_state` keeping whether or not it looked afresh,
/// the unsettled look's first digest is kept and the second reads nothing (`left: (0, 0), right:
/// (1, 1)`, `an unsettled file feeds no kept derivation (unsettled, call 1)`).
#[test]
fn an_unsettled_file_feeds_no_kept_derivation() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let vault = Vault::new();
    let home = vault.paths.home.clone();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        let (_, heads) = client::trusted_commons_log(&vault.paths, vault.bootstrap.trust())
            .unwrap()
            .unwrap();
        let source = vault.paths.dir.join("source.toml");
        let mode = std::fs::symlink_metadata(&source)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        crate::fsx::write_atomic(&source, &std::fs::read(&source).unwrap(), mode).unwrap();
        let rewritten = std::fs::symlink_metadata(&source).unwrap();
        let at = |secs: i64, nanos: i64| {
            std::time::UNIX_EPOCH
                + std::time::Duration::from_nanos(
                    u64::try_from(secs * 1_000_000_000 + nanos).unwrap(),
                )
        };
        let changed = at(rewritten.mtime(), rewritten.mtime_nsec())
            .max(at(rewritten.ctime(), rewritten.ctime_nsec()));
        for (which, began, per_call) in [
            ("unsettled", 1, [(1, 1), (1, 1), (1, 1)]),
            ("settled", 3, [(1, 1), (0, 1), (0, 0)]),
        ] {
            let look = client::VaultLook::begun_at(
                &vault.paths,
                changed + std::time::Duration::from_secs(began),
            );
            let under = look.held().expect("CONTROL: a look holding the vault");
            for (call, (reads, derived)) in per_call.into_iter().enumerate() {
                let read = crate::fsx::reads_under(&home);
                let before = client::test_look_derivations();
                commons_log_state_digest(&vault.paths, heads, Some(under)).unwrap();
                let read = read_since(&crate::fsx::reads_under(&home), &read);
                assert_eq!(
                    (
                        read.get(&source).copied().unwrap_or(0),
                        client::test_look_derivations()
                            .since(before)
                            .derived
                            .log_states
                    ),
                    (reads, derived),
                    "an unsettled file feeds no kept derivation ({which}, call {call})"
                );
            }
        }
    });
}

/// A look judges each access's currency once (`Under::currency`): a warm table session's open and
/// checks through it, and two more checks of its access's currency, judge the currency of the
/// access it opened once (`client::test_look_derivations`), never again after they kept it, and
/// answer as the asks' own calls do; the next look judges it again.
/// NEGATIVE CONTROL (2026-11-11): with `Under::currency` keeping nothing, a look judges it 4 times
/// (`left: 4, right: 1`, `a look judges its access's currency once`).
#[test]
fn a_look_judges_each_access_currency_once() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let own = a_session_on_own_calls(&vault);
        assert!(matches!(own.checks, Some((true, Ok(true)))));
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        for which in ["a look", "the next look"] {
            let (derived, judged) = derived_through(&vault, &client::VaultLook::new(&vault.paths));
            assert_eq!(
                derived.derived.currency, 1,
                "{which} judges its access's currency once"
            );
            assert_eq!(
                derived.rederived_after_kept.currency, 0,
                "{which} judges its access's currency again"
            );
            assert_eq!(
                judged.verdict(),
                own.verdict(),
                "{which} judging its access's currency once"
            );
        }
    });
}

/// A look's judgement of an access's currency lives and dies with the look: after a warm table
/// session's look, a look finds a warm access current, and once this Mac's identity was put in
/// another's place the next look finds it no longer current, as the access's own call does.
/// NEGATIVE CONTROL (2026-11-11): with the judgements kept past their look (in a thread's own
/// static), the next look finds the access current (`the next look judges currency afresh`);
/// without the warm session first, the first look's judgement read afresh and was not kept, and the
/// control did not fail.
#[test]
fn the_next_look_judges_currency_afresh() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let other = aterm_tempfile::tempdir().unwrap();
    let other = client::Paths::for_home(other.path());
    client::machine_identity(&other).unwrap();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        assert!(
            access.is_current_in(&vault.journal, &client::VaultLook::new(&vault.paths)),
            "CONTROL: a warm access is current through a look"
        );
        crate::fsx::write_atomic(
            &vault.paths.dir.join("identity"),
            &std::fs::read(other.dir.join("identity")).unwrap(),
            0o600,
        )
        .unwrap();
        assert!(
            !access.is_current(&vault.journal),
            "CONTROL: its own call sees another identity"
        );
        assert!(
            !access.is_current_in(&vault.journal, &client::VaultLook::new(&vault.paths)),
            "the next look judges currency afresh"
        );
    });
}

/// The opens of each file under a folder since `before` ([`crate::fsx::reads_under`]).
fn read_since(
    now: &std::collections::BTreeMap<std::path::PathBuf, usize>,
    before: &std::collections::BTreeMap<std::path::PathBuf, usize>,
) -> std::collections::BTreeMap<std::path::PathBuf, usize> {
    now.iter()
        .filter_map(|(path, reads)| {
            let read = reads - before.get(path).copied().unwrap_or(0);
            (read > 0).then(|| (path.clone(), read))
        })
        .collect()
}

/// A session look that holds no observation of the vault's state lock judges as the asks' own
/// calls do: one taken while a writer holds the lock (a busy vault, no access opened, and a warm
/// access not current by its boolean check and busy by its observed one), and one whose
/// observation guards a lock file put away since it
/// looked, a writer now holding the lock at the vault's path ([`client::StateLock::observes`]).
/// NEGATIVE CONTROL (2026-11-09): with `VaultLook::held` giving the look's observation whether
/// or not it guards the lock at the vault's path, the session whose lock was put away opens an
/// access under the writer (`a look at a lock put away`).
#[test]
fn a_session_look_with_no_observation_judges_as_its_own_calls() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        let writing = writer(&vault.paths).unwrap();
        let look = client::VaultLook::new(&vault.paths);
        let judged = a_session_through(&vault, &look);
        let own = a_session_on_own_calls(&vault);
        assert_eq!(judged.verdict(), own.verdict(), "a look under a writer");
        assert!(
            matches!(&own.access, Err(busy) if busy.kind() == crate::ErrorKind::VaultBusy),
            "{:?}",
            own.verdict()
        );
        assert!(!access.is_current_in(&vault.journal, &look));
        assert_eq!(
            access
                .is_current_observed_in(&vault.journal, &look)
                .map_err(|error| error.kind()),
            Err(crate::ErrorKind::VaultBusy)
        );
        drop(look);
        drop(writing);

        let lock = vault.paths.dir.join(".lock");
        let look = client::VaultLook::new(&vault.paths);
        let put_away = lock.with_extension("put-away");
        std::fs::rename(&lock, &put_away).unwrap();
        std::fs::copy(&put_away, &lock).unwrap();
        let writing = writer(&vault.paths).unwrap();
        let judged = a_session_through(&vault, &look);
        let own = a_session_on_own_calls(&vault);
        assert_eq!(judged.verdict(), own.verdict(), "a look at a lock put away");
        assert!(own.access.is_err(), "{:?}", own.verdict());
        let kind = |error: crate::Error| error.kind();
        assert_eq!(
            (
                access.is_current_in(&vault.journal, &look),
                access
                    .is_current_observed_in(&vault.journal, &look)
                    .map_err(kind)
            ),
            (
                access.is_current(&vault.journal),
                access.is_current_observed(&vault.journal).map_err(kind)
            ),
            "a look at a lock put away checks as its own calls"
        );
        drop(writing);
        drop(look);
    });
}

/// A look standing on a Commons source read ([`client::CommonsSourceRead::into_look`]: `aterm-db`'s
/// table-repair read batch's) observes the state lock no more than the read did and opens no look
/// of its own (`client::test_looks_observed`), and its session's open and checks judge as a look
/// of its own and as the asks' own calls do; once the read's lock file was put away and a writer
/// holds the lock at the vault's path, they judge as those calls do under the writer (no access
/// opened).
/// NEGATIVE CONTROLS (2026-11-09): with `into_look` making a look of its own (`VaultLook::new`),
/// the look observes the lock once more (`a source read look's observations`); with
/// `VaultLook::standing_on` keeping no observation, its asks observe the lock on their own calls
/// (`a source read look's observations`).
#[test]
fn a_source_read_look_judges_as_a_look_of_its_own() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let own = a_session_on_own_calls(&vault);
        assert!(matches!(own.access, Ok(Some(_))), "{:?}", own.verdict());
        let looked = a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        assert_eq!(looked.verdict(), own.verdict(), "a look of its own");
        let source_read = || {
            client::try_commons_source_read(&vault.paths)
                .unwrap()
                .expect("no writer holds the vault")
        };

        let (observations, looks) = (
            client::lock_observations_of(&vault.paths),
            client::test_looks_observed(),
        );
        let look = source_read().into_look(&vault.paths);
        let judged = a_session_through(&vault, &look);
        assert_eq!(
            client::lock_observations_of(&vault.paths) - observations,
            1,
            "a source read look's observations"
        );
        assert_eq!(
            client::test_looks_observed() - looks,
            0,
            "a source read look's own looks"
        );
        assert_eq!(judged.verdict(), own.verdict(), "a source read look");
        drop(look);

        let lock = vault.paths.dir.join(".lock");
        let look = source_read().into_look(&vault.paths);
        let put_away = lock.with_extension("put-away");
        std::fs::rename(&lock, &put_away).unwrap();
        std::fs::copy(&put_away, &lock).unwrap();
        let writing = writer(&vault.paths).unwrap();
        let judged = a_session_through(&vault, &look);
        let own = a_session_on_own_calls(&vault);
        assert_eq!(
            judged.verdict(),
            own.verdict(),
            "a source read look at a lock put away"
        );
        assert!(own.access.is_err(), "{:?}", own.verdict());
        drop(writing);
        drop(look);
    });
}

/// A record's history and its key judged through a look at the vault (a table-repair read
/// batch's: `aterm-db`'s `Session::decode_envelope` and `Session::history_needs`) answer as their
/// own calls do, and observe the state lock no more than the look did: no roster observation of
/// their own (`client::test_roster_observations`), no read of the log on its own call
/// (`client::test_logs_read`), through a look of its own and through one standing on a source
/// read. Once the look's lock file was put away and a writer holds the lock at the vault's path,
/// they answer as their own calls do under the writer.
/// NEGATIVE CONTROLS (2026-11-10): with `Access::with_log_history_in`'s membership check on its
/// own call whatever its look, or with `Bootstrap::historicals_in` or
/// `Bootstrap::holds_histories_in` leasing with no look, a history's asks make roster
/// observations of their own (`a history's own roster observations`); with its read of the log
/// on its own call, they read the
/// log (`a history's own log reads`); with `Historical::with_key_in` on its own call whatever its
/// look, the key's check makes one (`a key's own roster observations`); with it asking the log on its
/// own call, or taking a source read of its own, under a held look, it reads the log (`a key's own
/// log reads`) or observes the lock (`a key's lock observations`); and with
/// `VaultLook::held_at` giving the look's observation whether or not it guards the lock at the
/// vault's path, a key taken under the writer answers (`a history through a look at a lock put
/// away`).
#[test]
fn a_record_history_through_a_look_judges_as_its_own_calls() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let basis = *access.basis();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let own_key = vault
            .bootstrap
            .historical(&vault.journal, &access, basis)
            .expect("CONTROL: its history opens")
            .with_key(&vault.journal, |key| *key)
            .expect("CONTROL: its key");
        // The history, whether this device holds it, and whether its key is the own call's.
        let history = |look: Option<&client::VaultLook<'_>>| {
            vault
                .bootstrap
                .historical_in(&vault.journal, &access, basis, look)
                .map(|history| {
                    (
                        *history.source(),
                        history.generation(),
                        history.members().len(),
                    )
                })
                .map_err(kind_of)
        };
        let holds = |look: Option<&client::VaultLook<'_>>| {
            vault
                .bootstrap
                .holds_histories_in(&vault.journal, &access, &[basis], look)
        };
        let asked = || {
            (
                client::lock_observations_of(&vault.paths),
                client::test_roster_observations(),
                client::test_logs_read(),
            )
        };
        let own = (history(None), holds(None));
        let before = asked();
        assert_eq!(
            history(None),
            own.0,
            "CONTROL: a history's own call answers again"
        );
        let own_asked = asked();
        assert!(
            own_asked.1 > before.1 && own_asked.2 > before.2,
            "CONTROL: a history's own calls ask on their own"
        );
        let source_read = || {
            client::try_commons_source_read(&vault.paths)
                .unwrap()
                .expect("no writer holds the vault")
        };
        for (look, which) in [
            (client::VaultLook::new(&vault.paths), "its own"),
            (source_read().into_look(&vault.paths), "a source read's"),
        ] {
            let look = Some(&look);
            let before = asked();
            let judged = (history(look), holds(look));
            let after = asked();
            assert_eq!(
                after.1 - before.1,
                0,
                "a history's own roster observations (a look of {which})"
            );
            assert_eq!(
                after.2 - before.2,
                0,
                "a history's own log reads (a look of {which})"
            );
            assert_eq!(
                after.0 - before.0,
                0,
                "a history's lock observations (a look of {which})"
            );
            assert_eq!(judged, own, "a history through a look of {which}");
            let history = vault
                .bootstrap
                .historical(&vault.journal, &access, basis)
                .unwrap();
            let before = asked();
            let judged = history
                .with_key_in(&vault.journal, look, |key| *key == own_key)
                .map_err(kind_of);
            let after = asked();
            assert_eq!(
                after.1 - before.1,
                0,
                "a key's own roster observations (a look of {which})"
            );
            assert_eq!(
                after.2 - before.2,
                0,
                "a key's own log reads (a look of {which})"
            );
            assert_eq!(
                after.0 - before.0,
                0,
                "a key's lock observations (a look of {which})"
            );
            assert_eq!(judged, Ok(true), "a key through a look of {which}");
        }

        let kept = vault
            .bootstrap
            .historical(&vault.journal, &access, basis)
            .unwrap();
        let lock = vault.paths.dir.join(".lock");
        let look = source_read().into_look(&vault.paths);
        let put_away = lock.with_extension("put-away");
        std::fs::rename(&lock, &put_away).unwrap();
        std::fs::copy(&put_away, &lock).unwrap();
        let writing = writer(&vault.paths).unwrap();
        let key = |look: Option<&client::VaultLook<'_>>| {
            kept.with_key_in(&vault.journal, look, |key| *key == own_key)
                .map_err(kind_of)
        };
        let own = (history(None), holds(None), key(None));
        assert!(
            own.0.is_err() && own.2.is_err(),
            "CONTROL: under the writer: {own:?}"
        );
        assert_eq!(
            (history(Some(&look)), holds(Some(&look)), key(Some(&look))),
            own,
            "a history through a look at a lock put away"
        );
        drop(writing);
        drop(look);
    });
}

/// A record's history and a kept key judged through a look refuse as their own calls do once the
/// access they stand on is no longer current: after this Mac's identity was put in another's
/// place, the next look (one that observes the state lock) finds a key kept from before no longer
/// current and leases no history, each as its own call refuses.
/// NEGATIVE CONTROLS (2026-11-10): with `Historical::with_key_in` giving the key under a held look
/// unjudged, it answers (`a kept key through the next look`); with
/// `Access::with_log_history_in` leasing under one without its membership check, the history
/// opens (`a history through the next look`).
#[test]
fn the_next_look_refuses_a_record_history_no_longer_current() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let basis = *access.basis();
    let kept = vault
        .bootstrap
        .historical(&vault.journal, &access, basis)
        .unwrap();
    let other = aterm_tempfile::tempdir().unwrap();
    let other = client::Paths::for_home(other.path());
    client::machine_identity(&other).unwrap();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        assert!(
            kept.with_key(&vault.journal, |_| ()).is_ok(),
            "CONTROL: the key stands"
        );
        crate::fsx::write_atomic(
            &vault.paths.dir.join("identity"),
            &std::fs::read(other.dir.join("identity")).unwrap(),
            0o600,
        )
        .unwrap();
        let look = client::VaultLook::new(&vault.paths);
        assert!(
            look.held_at(&vault.paths).is_some(),
            "CONTROL: the look observes the lock"
        );
        let own = (
            kept.with_key(&vault.journal, |_| ()).map_err(kind_of),
            vault
                .bootstrap
                .historical(&vault.journal, &access, basis)
                .map(|_| ())
                .map_err(kind_of),
        );
        assert!(
            own.0.is_err() && own.1.is_err(),
            "CONTROL: their own calls refuse: {own:?}"
        );
        assert_eq!(
            kept.with_key_in(&vault.journal, Some(&look), |_| ())
                .map_err(kind_of),
            own.0,
            "a kept key through the next look"
        );
        assert_eq!(
            vault
                .bootstrap
                .historical_in(&vault.journal, &access, basis, Some(&look))
                .map(|_| ())
                .map_err(kind_of),
            own.1,
            "a history through the next look"
        );
    });
}

/// An error by its kind, comparable.
fn kind_of(error: crate::Error) -> crate::ErrorKind {
    error.kind()
}

/// A look stands for one moment of the vault: a session look after this Mac's identity was put
/// in another's place judges as the asks' own calls do (its custody refused, and the access it
/// opens no longer current), and finds a warm access no longer current.
/// NEGATIVE CONTROL (2026-11-09): with `Access::is_current_observed_in` comparing no recipient,
/// the look finds the access it opens current (`a look after another identity`).
#[test]
fn the_next_session_look_sees_another_identity() {
    let vault = Vault::new();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let other = aterm_tempfile::tempdir().unwrap();
    let other = client::Paths::for_home(other.path());
    client::machine_identity(&other).unwrap();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        a_session_through(&vault, &client::VaultLook::new(&vault.paths));
        let identity = vault.paths.dir.join("identity");
        crate::fsx::write_atomic(
            &identity,
            &std::fs::read(other.dir.join("identity")).unwrap(),
            0o600,
        )
        .unwrap();
        let look = client::VaultLook::new(&vault.paths);
        let judged = a_session_through(&vault, &look);
        let own = a_session_on_own_calls(&vault);
        assert_eq!(
            judged.verdict(),
            own.verdict(),
            "a look after another identity"
        );
        assert!(own.custody.is_err());
        assert!(matches!(own.checks, Some((false, Ok(false)))));
        assert!(!access.is_current(&vault.journal));
        assert!(
            !access.is_current_in(&vault.journal, &look),
            "the next look sees another identity"
        );
    });
}

/// A look judges its custody as it first saw the pinned files (`Bootstrap::revalidate_custody_in`
/// through the look's glance): a pin replaced after a look's first check passes that look's next
/// check, as the files the state lock guards would, and the next look and the bootstrap's own call
/// refuse it; a look that never looked at the pin before it changed refuses it at its first check.
/// NEGATIVE CONTROL (2026-11-10): with the custody's digests on their own calls in a look, the
/// look refuses the pin it first saw (`a look judges its custody as it first saw it`).
#[test]
fn a_session_look_judges_its_custody_as_it_first_saw_it() {
    let vault = Vault::new();
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        vault.bootstrap.revalidate_custody().unwrap();
        let look = client::VaultLook::new(&vault.paths);
        vault.bootstrap.revalidate_custody_in(&look).unwrap();
        let pin = vault.paths.dir.join("commons").join("default.pin");
        let mut replaced = std::fs::read(&pin).unwrap();
        *replaced.last_mut().unwrap() ^= 1;
        crate::fsx::write_atomic(&pin, &replaced, 0o600).unwrap();
        assert!(
            vault.bootstrap.revalidate_custody_in(&look).is_ok(),
            "a look judges its custody as it first saw it"
        );
        drop(look);
        let refused = |judged: crate::Result<()>| {
            judged
                .err()
                .is_some_and(|error| error.to_string().contains("immutable custody changed"))
        };
        assert!(
            refused(
                vault
                    .bootstrap
                    .revalidate_custody_in(&client::VaultLook::new(&vault.paths))
            ),
            "the next look sees a replaced pin"
        );
        assert!(
            refused(vault.bootstrap.revalidate_custody()),
            "the bootstrap's own call sees a replaced pin"
        );
    });
}

/// A look confirms the control anchor it judged custody through (`Bootstrap::anchor_stands_in`),
/// as `aterm-db`'s round group asks after a follower's own control journal read: after the look's
/// custody check, the anchor stands while nothing changed, and not once a byte-identical copy was
/// renamed over it, which the next look's own custody check confirms. It does not stand through a
/// look at another vault, though that look's glance looked at this vault's anchor too, through a
/// look taken while a writer held the lock (which holds nothing, asked with no custody check
/// before it), or through a look begun a second after the anchor's change, whose first look at it
/// had not settled. Whether the look first saw it settled (`Bootstrap::anchor_settled_in`) answers
/// by that first look alone: the renamed anchor's still did.
/// NEGATIVE CONTROLS (2026-11-11): with `Bootstrap::anchor_stands_in` judging through
/// `VaultLook::held` whatever vault the look is at, the look at another vault confirms this one's
/// anchor (`a look at another vault`); with it asking whether the look holds the vault and not
/// `Glance::first_stands`, the renamed anchor stands (`a byte-identical anchor renamed over`).
#[test]
fn a_look_confirms_the_control_anchor_it_judged_custody_through() {
    let vault = Vault::new();
    let other = Vault::new();
    let anchor = vault.bootstrap.journal_dir().join("control.anchor");
    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let writing = writer(&vault.paths).unwrap();
        let busy = client::VaultLook::new(&vault.paths);
        assert!(
            !vault.bootstrap.anchor_stands_in(&busy),
            "a look that holds nothing"
        );
        drop((busy, writing));

        let look = client::VaultLook::new(&vault.paths);
        vault.bootstrap.revalidate_custody_in(&look).unwrap();
        assert!(
            vault.bootstrap.anchor_settled_in(&look),
            "CONTROL: the look first saw the anchor settled"
        );
        assert!(vault.bootstrap.anchor_stands_in(&look), "nothing changed");

        let elsewhere = client::VaultLook::new(&other.paths);
        other.bootstrap.revalidate_custody_in(&elsewhere).unwrap();
        elsewhere
            .held()
            .expect("CONTROL: the other look holds its vault")
            .glance()
            .symlink_metadata(&anchor)
            .unwrap()
            .expect("CONTROL: the other look looked at this vault's anchor");
        assert!(
            !vault.bootstrap.anchor_settled_in(&elsewhere),
            "a look at another vault first saw no anchor of this one"
        );
        assert!(
            !vault.bootstrap.anchor_stands_in(&elsewhere),
            "a look at another vault"
        );

        let copy = anchor.with_extension("copy");
        std::fs::copy(&anchor, &copy).unwrap();
        std::fs::rename(&copy, &anchor).unwrap();
        assert!(
            vault.bootstrap.anchor_settled_in(&look),
            "CONTROL: whether the look first saw it settled looks at nothing"
        );
        assert!(
            !vault.bootstrap.anchor_stands_in(&look),
            "a byte-identical anchor renamed over"
        );
        drop(look);
        let next = client::VaultLook::new(&vault.paths);
        vault.bootstrap.revalidate_custody_in(&next).unwrap();
        assert!(
            vault.bootstrap.anchor_stands_in(&next),
            "CONTROL: the next look's custody check confirms the copy"
        );
    });
    let seen = crate::fsx::seen_if_present(&anchor).unwrap().unwrap();
    let changed = std::time::UNIX_EPOCH
        + std::time::Duration::from_nanos(
            u64::try_from(seen.stamp.mtime_ns.max(seen.stamp.ctime_ns)).unwrap(),
        );
    let early =
        client::VaultLook::begun_at(&vault.paths, changed + std::time::Duration::from_secs(1));
    vault.bootstrap.revalidate_custody_in(&early).unwrap();
    assert!(
        !vault.bootstrap.anchor_settled_in(&early),
        "a look begun as the anchor changed first saw it unsettled"
    );
    assert!(
        !vault.bootstrap.anchor_stands_in(&early),
        "an anchor that had not settled when the look began"
    );
}

/// A source read through a look (`Bootstrap::local_source_read_in`) is the look's own where it
/// holds the lock at the bootstrap's vault: it takes none of its own
/// (`client::test_source_reads_held`), whether the look observed the lock itself or stands on a
/// source read's observation, and it judges the custody through the look: the pinned files as the
/// look first saw them, so a pin replaced since passes that look and the next look refuses it. A
/// look whose observation guards a lock file put away since it looked, and a look at another
/// vault, give a source read of their own.
/// NEGATIVE CONTROLS (2026-11-10): with it taking its own source read whatever the look, a look
/// holding the vault gives one (`a source read through a look holding the vault`); with it
/// standing on a look whether or not the look holds the lock at the vault's path, a look on a lock
/// put away gives none (`a source read through a look on a lock put away`); with it judging the
/// custody on its own call (`Bootstrap::revalidate_custody`), a look that judged the custody once
/// refuses a pin replaced since (`a source read through a look judges its custody as the look
/// first saw it`); with it judging no custody, the next look gives a replaced pin a source read
/// (`a source read through the next look sees a replaced pin`).
#[test]
fn a_source_read_through_a_look_holding_the_vault_is_the_looks() {
    let vault = Vault::new();
    let held = client::test_source_reads_held;
    let before = held();
    let look = client::VaultLook::new(&vault.paths);
    assert!(
        vault
            .bootstrap
            .local_source_read_in(&look)
            .unwrap()
            .is_none(),
        "a source read through a look holding the vault"
    );
    drop(look);
    let standing = client::try_commons_source_read(&vault.paths)
        .unwrap()
        .unwrap()
        .into_look(&vault.paths);
    assert!(
        vault
            .bootstrap
            .local_source_read_in(&standing)
            .unwrap()
            .is_none(),
        "a source read through a source read's look"
    );
    drop(standing);
    assert_eq!(held() - before, 0, "CONTROL: no source read of its own");

    let other = Vault::new();
    let elsewhere = client::VaultLook::new(&other.paths);
    let read = vault.bootstrap.local_source_read_in(&elsewhere).unwrap();
    assert!(
        read.is_some(),
        "a source read through a look at another vault"
    );
    drop((read, elsewhere));
    assert_eq!(held() - before, 1, "CONTROL: its own source read");

    let look = client::VaultLook::new(&vault.paths);
    let lock = vault.paths.dir.join(".lock");
    let put_away = lock.with_extension("put-away");
    std::fs::rename(&lock, &put_away).unwrap();
    std::fs::copy(&put_away, &lock).unwrap();
    let read = vault.bootstrap.local_source_read_in(&look).unwrap();
    assert!(
        read.is_some(),
        "a source read through a look on a lock put away"
    );
    drop((read, look));
    assert_eq!(held() - before, 2, "CONTROL: its own source read");

    crate::fsx::as_if_later(std::time::Duration::from_secs(3600), || {
        let look = client::VaultLook::new(&vault.paths);
        assert!(
            vault
                .bootstrap
                .local_source_read_in(&look)
                .unwrap()
                .is_none(),
            "CONTROL: a look holding the vault"
        );
        let pin = vault.paths.dir.join("commons").join("default.pin");
        let mut replaced = std::fs::read(&pin).unwrap();
        *replaced.last_mut().unwrap() ^= 1;
        crate::fsx::write_atomic(&pin, &replaced, 0o600).unwrap();
        assert!(
            matches!(vault.bootstrap.local_source_read_in(&look), Ok(None)),
            "a source read through a look judges its custody as the look first saw it"
        );
        drop(look);
        assert!(
            vault
                .bootstrap
                .local_source_read_in(&client::VaultLook::new(&vault.paths))
                .err()
                .is_some_and(|error| error.to_string().contains("immutable custody changed")),
            "a source read through the next look sees a replaced pin"
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
    assert!(
        access
            .opens_from_lease(&identity, *epoch.id(), &pending.lease_complete().unwrap())
            .is_err(),
        "an ask whether it opens is refused as the key's is"
    );
    assert!(log.get(parent.id()).is_none(), "the parent has not arrived");
}

/// A batch asks whether this device holds each basis as [`Bootstrap::historicals`] answers it, in
/// order, and takes no key to answer: it asks nothing of the kept keys, a basis the log opens is
/// noted as opened in the log the vault's memo keeps, and the next ask unwraps no key again.
///
/// NEGATIVE CONTROLS: `Bootstrap::holds_histories` answering through `historicals` fails the kept
/// keys' assertion; `Access::opens_from_lease` without its `HistoryLease::opened` lookup fails the
/// second ask's (one unwrap); `holds_histories` answering in reverse order fails the first.
#[test]
fn a_batch_asks_whether_it_holds_each_basis_as_its_historicals_answer() {
    use crate::membership_log::history::{HistoryView, RecordState, UNWRAPS};
    use std::cell::Cell;

    let vault = Vault::new();
    let identity = client::machine_identity(&vault.paths).unwrap();
    let access = vault.bootstrap.access_in(&vault.journal).unwrap().unwrap();
    let basis = *access.basis();
    let bases = [basis, [7; 32]];
    let asks = KEY_CACHE_ASKS.with(Cell::get);
    let held = vault
        .bootstrap
        .holds_histories(&vault.journal, &access, &bases);
    assert_eq!(held, [true, false]);
    assert_eq!(
        KEY_CACHE_ASKS.with(Cell::get),
        asks,
        "an ask whether it holds takes no kept key"
    );
    let (log, _) = client::trusted_commons_log_now(&vault.paths, vault.bootstrap.trust())
        .unwrap()
        .unwrap();
    let view = HistoryView::new(&log);
    let lease = view.lease_complete().unwrap();
    let RecordState::Complete(epoch) = lease.record(&basis).unwrap() else {
        panic!("CONTROL: the log holds the vault's own epoch");
    };
    assert!(
        lease.opened(&epoch, &identity).unwrap(),
        "the ask noted what opened in the log the memo keeps"
    );
    let unwraps = UNWRAPS.with(Cell::get);
    assert_eq!(
        vault
            .bootstrap
            .holds_histories(&vault.journal, &access, &bases),
        held
    );
    assert_eq!(
        UNWRAPS.with(Cell::get),
        unwraps,
        "an epoch that opened is answered without its key unwrapped again"
    );
    let answers: Vec<bool> = vault
        .bootstrap
        .historicals(&vault.journal, &access, &bases)
        .iter()
        .map(Result::is_ok)
        .collect();
    assert_eq!(held, answers);
    assert!(
        vault
            .bootstrap
            .holds_histories(&vault.journal, &access, &[])
            .is_empty()
    );
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
