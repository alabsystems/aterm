// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Commons observation reads each file it holds at most once (a pinned layout opens its data file
//! twice, validated and then read), and what is on disk when it starts is what it sees, whichever
//! memo of exact bytes the last one left behind. The counts are what [`fsx::reads_under`] sees: the
//! lock, the layout directory and the journal's own files are not among them, and the journal
//! counts as one read of `frontiers.checkpoint`.

use super::*;
use crate::commons::{Observation, Unavailable};
use log_entries_tests::{TwoMacs, two_macs};
use std::collections::BTreeMap;

fn reads_during<T>(paths: &Paths, read: impl FnOnce() -> T) -> (T, BTreeMap<PathBuf, usize>) {
    let before = fsx::reads_under(&paths.home);
    let answer = read();
    let mut reads = fsx::reads_under(&paths.home);
    reads.retain(|path, count| {
        *count -= before.get(path).copied().unwrap_or(0);
        *count > 0
    });
    (answer, reads)
}

fn observed_with_reads(paths: &Paths) -> (crate::Result<Observation>, BTreeMap<PathBuf, usize>) {
    reads_during(paths, || observe_commons(paths))
}

/// `expected` were each read exactly once, and no other file more than once.
fn assert_read_once(reads: &BTreeMap<PathBuf, usize>, expected: &[PathBuf]) {
    let repeated: Vec<_> = reads.iter().filter(|(_, count)| **count > 1).collect();
    assert!(repeated.is_empty(), "read more than once: {repeated:#?}");
    for path in expected {
        assert_eq!(reads.get(path), Some(&1), "{path:?} among {reads:#?}");
    }
}

fn is_visible(observed: &crate::Result<Observation>) -> bool {
    matches!(observed, Ok(Observation::Visible(_)))
}

fn is_unavailable(paths: &Paths, reason: Unavailable) -> bool {
    matches!(observe_commons(paths), Ok(Observation::Unavailable(found)) if found == reason)
}

/// The vault files every observation of an applied vault reads. An absent records pin is seen by
/// its `lstat` ([`crate::records_storage::selected`]), not read.
fn vault_files(paths: &Paths) -> Vec<PathBuf> {
    vec![
        paths.source_file(),
        paths.state_file(),
        paths.identity_file(),
        paths.dir.join("records.frontiers"),
    ]
}

/// [`vault_files`] of a vault holding a log, its journal acknowledging it.
fn logged_vault_files(paths: &Paths) -> Vec<PathBuf> {
    let mut files = vault_files(paths);
    files.push(paths.dir.join("frontiers.checkpoint"));
    files
}

/// `lab` with an applied roster that denies a node, so that a denial check needs this Mac's device
/// binding, and `m7` holding the version and its records.
fn with_a_denied_node(lab: &TwoMacs) {
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    let roster = crate::swarm_roster::SwarmRoster {
        deny_labels: vec!["m99".to_owned()],
        deny_nodes: vec![aterm_codec::hex::encode_lower(&[99; 32])],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync_with(&lab.m7, std::fs::read(lab.m22.cache_file()).unwrap()).unwrap();
}

/// NEGATIVE CONTROLS (2026-11-05): with the denial check given a second `Reads::new(paths)`, a file
/// is `read more than once`; with `OBSERVE_COMMONS_CACHE`'s lookup moved ahead of the applied
/// branch's denial check, the second observation reads no `device.age` (`left: None, right:
/// Some(1)`); with `selected` always going through `data_path`, the absent pin is opened (`left:
/// Some(1), right: None`).
#[test]
fn an_observation_of_an_applied_vault_reads_each_file_once() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    with_a_denied_node(&lab);
    let mut expected = logged_vault_files(&lab.m7);
    expected.extend([
        lab.m7.cache_file(),
        lab.m7.records_file(),
        lab.m7.dir.join("commons/device.age"),
        lab.m7.dir.join("commons/device.claim"),
    ]);
    // The second observation is answered from the memos of exact bytes the first left: it still
    // reads every current file, once.
    for _ in 0..2 {
        let (observed, reads) = observed_with_reads(&lab.m7);
        assert!(is_visible(&observed), "{observed:?}");
        assert_read_once(&reads, &expected);
        assert_eq!(reads.get(&lab.m7.dir.join("records.layout")), None);
    }
    lab.server.stop();
}

/// NEGATIVE CONTROLS (2026-11-05): with `data_path` back to `context.active()?`, the pin is `read
/// more than once`; with the denial check given a second `Reads::new(paths)`, the data file opens
/// four times (`left: Some(4), right: Some(2)`).
#[test]
fn an_observation_under_a_pinned_records_layout_opens_its_data_twice_and_the_rest_once() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    drop(crate::records_storage::write_path(&lab.m7.records_file(), true).unwrap());
    let mut expected = logged_vault_files(&lab.m7);
    expected.extend([
        lab.m7.cache_file(),
        lab.m7.dir.join("records.layout"),
        lab.m7.records_file().join("layout"),
    ]);
    let (observed, mut reads) = observed_with_reads(&lab.m7);
    assert!(is_visible(&observed), "{observed:?}");
    assert_eq!(
        reads.remove(&lab.m7.records_file().join("current")),
        Some(2),
        "validated, then read: {reads:#?}"
    );
    assert_read_once(&reads, &expected);
    lab.server.stop();
}

/// NEGATIVE CONTROL (2026-11-05): with the denial check given a second `Reads::new(paths)`, a file
/// is `read more than once`.
#[test]
fn an_observation_of_a_log_only_vault_reads_each_file_once() {
    let _answering = ServersAnswering::set();
    let (_root, paths, _config, state) = log_only_tests::applied_log();
    assert_eq!(state.digest, None);
    let mut expected = logged_vault_files(&paths);
    expected.push(paths.records_file());
    let (observed, reads) = observed_with_reads(&paths);
    assert!(is_visible(&observed), "{observed:?}");
    assert_read_once(&reads, &expected);
}

/// A vault with no log, as a Mac on a version alone reads it: the lineage of its cached envelope
/// decides which records it would take.
///
/// NEGATIVE CONTROL (2026-11-05): with the denial check given a second `Reads::new(paths)`, a file
/// is `read more than once`.
#[test]
fn an_observation_of_a_vault_with_no_log_reads_each_file_once() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("served.vault");
    test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut document = admin.doc.clone();
    document
        .set_entry(
            &Audience::Hosts(vec!["m1".into()]),
            crate::commons::ENTRY,
            b"inert-commons-key-material",
        )
        .unwrap();
    admin_publish(&paths, &admin, &document).unwrap();
    let mut expected = vault_files(&paths);
    expected.push(paths.cache_file());
    for _ in 0..2 {
        let (observed, reads) = observed_with_reads(&paths);
        assert!(is_visible(&observed), "{observed:?}");
        assert_read_once(&reads, &expected);
    }
}

/// The read each caller of a roster makes, under the one lock it holds.
fn roster_with_reads(
    paths: &Paths,
) -> (
    crate::Result<Option<crate::swarm_roster::SwarmRoster>>,
    BTreeMap<PathBuf, usize>,
) {
    let lock = StateLock::observe(paths).unwrap().unwrap();
    reads_during(paths, || roster::swarm_roster_held(paths, &lock))
}

/// A roster read asks for this Mac's log as it finds its base roster and again to overlay it.
///
/// NEGATIVE CONTROL (2026-11-05): with `swarm_roster_held` handing the overlay a second
/// `Reads::new(paths)`, a file is `read more than once`.
#[test]
fn a_roster_read_of_an_applied_vault_reads_each_file_once() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    with_a_denied_node(&lab);
    let mut expected = logged_vault_files(&lab.m7);
    expected.extend([lab.m7.cache_file(), lab.m7.records_file()]);
    let (roster, reads) = roster_with_reads(&lab.m7);
    assert!(roster.unwrap().unwrap().is_label_denied("m99"));
    assert_read_once(&reads, &expected);
    lab.server.stop();
}

/// NEGATIVE CONTROL (2026-11-05): with `swarm_roster_held` handing the overlay a second
/// `Reads::new(paths)`, a file is `read more than once`.
#[test]
fn a_roster_read_of_a_log_only_vault_reads_each_file_once() {
    let _answering = ServersAnswering::set();
    let (_root, paths, _config, state) = log_only_tests::applied_log();
    assert_eq!(state.digest, None);
    let mut expected = logged_vault_files(&paths);
    expected.push(paths.records_file());
    let (roster, reads) = roster_with_reads(&paths);
    assert!(roster.unwrap().is_some());
    assert_read_once(&reads, &expected);
}

/// One read of the Commons log, as each check of a Commons access makes: the store, the lineage
/// that judges it and the retained history that covers it.
///
/// NEGATIVE CONTROLS (2026-11-05): with `commons_log_under`'s `reads.config()` back to
/// `config(reads.paths())`, `source.toml` is `read more than once`; with `require_covers` handed
/// `&Reads::new(reads.paths())`, `records.frontiers` is.
#[test]
fn a_commons_log_read_reads_each_file_once() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    with_a_denied_node(&lab);
    let store = load_acknowledged_records(&lab.m7).unwrap();
    let crate::membership_log::Body::Genesis { root, .. } = store.log().unwrap().genesis().body()
    else {
        panic!("a log begins with its genesis");
    };
    let trust = crate::commons::journal::Trust {
        network: root.network,
        authority: root.authority,
        genesis: [0; 32],
    };
    let (log, reads) = reads_during(&lab.m7, || log_entries::trusted_commons_log(&lab.m7, trust));
    assert!(log.unwrap().is_some());
    assert_read_once(
        &reads,
        &[
            lab.m7.source_file(),
            lab.m7.records_file(),
            lab.m7.dir.join("records.frontiers"),
            lab.m7.dir.join("frontiers.checkpoint"),
        ],
    );
    lab.server.stop();
}

/// A failed read fails the observation, as it did when each check read for itself. Only the vault
/// file is asked about again later in an observation, so its arm is the one a memo decides; the
/// state and identity arms hold the error order, and
/// [`a_failed_read_answers_every_later_ask_of_the_same_reads`] holds their memos.
///
/// NEGATIVE CONTROL (2026-11-05): with `Reads::vault` keeping no error, the vault file is read
/// again (`left: Some(2), right: Some(1)`).
#[test]
fn a_failed_read_fails_the_observation_once() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    for path in [
        lab.m7.state_file(),
        lab.m7.identity_file(),
        lab.m7.cache_file(),
    ] {
        fsx::fail_reads(&path, usize::MAX);
        let (observed, reads) = observed_with_reads(&lab.m7);
        fsx::fail_reads(&path, 0);
        assert!(observed.is_err(), "{path:?}: {observed:?}");
        assert_eq!(reads.get(&path), Some(&1), "{path:?}");
    }
    assert!(is_visible(&observe_commons(&lab.m7)));
    lab.server.stop();
}

/// NEGATIVE CONTROLS (2026-11-05), each memo kept only for an `Ok` in its own run: `config`,
/// `state`, `identity`, `vault`, `enrollment` and `acknowledged` each fire their own arm at the
/// second ask (`… answers from its failure`), naming `source.toml`, `state.toml`, `identity`,
/// `vault`, `records.frontiers` and `records`.
#[test]
fn a_failed_read_answers_every_later_ask_of_the_same_reads() {
    type Ask = fn(&Reads<'_>, &Config) -> bool;
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    let m7 = &lab.m7;
    let config = configured(m7).unwrap();
    let asks: [(PathBuf, Ask); 6] = [
        (m7.source_file(), |reads, _| reads.config().is_ok()),
        (m7.state_file(), |reads, _| reads.state().is_ok()),
        (m7.identity_file(), |reads, _| reads.identity().is_ok()),
        (m7.cache_file(), |reads, _| reads.vault().is_ok()),
        (m7.dir.join("records.frontiers"), |reads, _| {
            reads.enrollment().is_ok()
        }),
        (m7.records_file(), |reads, config| {
            reads.acknowledged(config).is_ok()
        }),
    ];
    for (path, ask) in asks {
        assert!(ask(&Reads::new(m7), &config), "{path:?} reads");
        let before = fsx::reads_of(&path);
        fsx::fail_reads(&path, 1);
        let reads = Reads::new(m7);
        assert!(!ask(&reads, &config), "{path:?} fails");
        assert!(!ask(&reads, &config), "{path:?} answers from its failure");
        assert_eq!(fsx::reads_of(&path) - before, 1, "{path:?} was read again");
        assert!(
            ask(&Reads::new(m7), &config),
            "{path:?} reads the next time"
        );
    }
    lab.server.stop();
}

/// NEGATIVE CONTROL (2026-11-05): with `reads.state()?` gone from `local_recipient_denied`, the
/// check answers `Ok(false)` (`assertion failed: denial.is_err()`).
#[test]
fn an_unreadable_state_fails_a_denial_check_on_a_mac_with_no_configuration() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    machine_identity(&paths).unwrap();
    assert!(matches!(
        steward::local_recipient_denied(&Reads::new(&paths)),
        Ok(false)
    ));
    fsx::fail_reads(&paths.state_file(), usize::MAX);
    let denial = steward::local_recipient_denied(&Reads::new(&paths));
    fsx::fail_reads(&paths.state_file(), 0);
    assert!(denial.is_err(), "{denial:?}");
}

/// What a server's next poll decides ([`crate::cli::ServeRole::decide`]) on `paths`, its store at
/// `store`.
fn poll_server(
    paths: &Paths,
    store: &Path,
    signer: Option<[u8; 32]>,
    role: &mut crate::cli::ServeRole,
) -> Option<String> {
    let advertise = crate::cli::Advertise::Named(crate::alab_advertised().to_owned());
    role.decide(&advertise, store, paths, &mut Vec::new(), signer)
        .0
}

/// [`poll_server`] on `lab`'s `m7`.
fn poll_role(
    lab: &TwoMacs,
    signer: Option<[u8; 32]>,
    role: &mut crate::cli::ServeRole,
) -> Option<String> {
    poll_server(&lab.m7, &lab.root.path().join("role-server"), signer, role)
}

/// This thread's vault key is the ALab line, which a server role needs.
fn on_the_alab_line(signer: Option<[u8; 32]>) {
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: signer,
        });
    });
}

fn off_the_alab_line() {
    envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
}

/// The roster of `lab`'s next version says `roster`, and `m7` has applied that version.
fn publish_roster(lab: &TwoMacs, roster: &[u8]) {
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, crate::SWARM_ROSTER_ENTRY, roster)
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync(&lab.m7, None).unwrap();
}

/// `m7` rotates with the vault, its `source.toml` naming the keys it followed.
fn rotate(lab: &TwoMacs) {
    let opened = admin_open(&lab.m22).unwrap();
    admin_rotate(&lab.m22, &opened, &opened.doc.clone()).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync(&lab.m7, None).unwrap();
}

/// A held role outlasts a failed denial check, so a failed read of the vault file, which hides the
/// cached envelope's denial from that observation, must not end the role check. A vault that names
/// no `followed` spends one failed read on the role check's own configuration and one on each
/// look's. What the second look does not reach, the next poll does. A writer's lock changes none of
/// it: a contended poll looks with no lock, and that look gets the same second one.
///
/// NEGATIVE CONTROLS (2026-11-05): with no second look, a denied server keeps its role through the
/// poll (`rotated false, contended false, 2 failed`, each `left: Some("m7"), right: None`); with
/// the second look on the free lock's arm alone, through the writer's (`rotated false, contended
/// true, 2 failed`); with the writer's arm answering `unavailable()`, at once (`rotated false,
/// contended true, 0 failed`); with a failed look read as a denial, a Mac still in loses its role
/// (`left: None, right: Some("m7")`).
#[test]
fn a_failed_vault_read_hides_no_denial_from_a_server_holding_its_role() {
    let _answering = ServersAnswering::set();
    // Failed reads in the poll, and what the denied server holds for that poll.
    for (rotated, polls) in [
        (false, [(0, None), (1, None), (2, None), (3, Some("m7"))]),
        (
            true,
            [(0, None), (1, None), (2, Some("m7")), (3, Some("m7"))],
        ),
    ] {
        let lab = two_macs(&[]);
        if rotated {
            rotate(&lab);
        }
        let signer = configured(&lab.m7).unwrap().signer;
        on_the_alab_line(signer);
        let cache = lab.m7.cache_file();
        let mut held = crate::cli::ServeRole::default();
        let mut roles: [crate::cli::ServeRole; 8] = Default::default();
        for role in roles.iter_mut().chain([&mut held]) {
            assert_eq!(poll_role(&lab, signer, role).as_deref(), Some("m7"));
        }

        // A failed read costs a Mac that is still in nothing, and gives a new server nothing: from
        // the first that reaches a look on the free lock, and from none at all under a writer.
        let first = if rotated { 1 } else { 2 };
        for (contended, fails) in [
            (false, first),
            (false, 3),
            (true, 0),
            (true, 1),
            (true, 2),
            (true, 3),
        ] {
            let row = format!("rotated {rotated}, contended {contended}, {fails} failed");
            let writer = contended.then(|| StateLock::take(&lab.m7).unwrap());
            fsx::fail_reads(&cache, fails);
            assert_eq!(
                poll_role(&lab, signer, &mut held).as_deref(),
                Some("m7"),
                "{row}"
            );
            fsx::fail_reads(&cache, fails);
            assert_eq!(
                poll_role(&lab, signer, &mut crate::cli::ServeRole::default()),
                None,
                "{row}"
            );
            fsx::fail_reads(&cache, 0);
            drop(writer);
        }

        publish_roster(&lab, b"deny_labels = [\"m7\"]\n");

        let rows = [false, true]
            .into_iter()
            .flat_map(|contended| polls.map(|poll| (contended, poll)));
        for ((contended, (fails, during)), role) in rows.zip(&mut roles) {
            let row = format!("rotated {rotated}, contended {contended}, {fails} failed");
            let writer = contended.then(|| StateLock::take(&lab.m7).unwrap());
            fsx::fail_reads(&cache, fails);
            let held = poll_role(&lab, signer, role);
            fsx::fail_reads(&cache, 0);
            drop(writer);
            assert_eq!(held.as_deref(), during, "{row}");
            assert_eq!(poll_role(&lab, signer, role), None, "{row}: the next poll");
        }
        off_the_alab_line();
        lab.server.stop();
    }
}

/// Under a writer, a Mac its roster denies is denied by every reader that reads the denial with no
/// lock, the role's included, and a reader that needs the lock says it is busy, never "no roster".
///
/// NEGATIVE CONTROL (2026-11-05): with the writer's arm of `observe_alab_role_inner` answering
/// `unavailable()`, the role reads `Unavailable` (`matches!(… Denied)`).
#[test]
fn a_mac_its_roster_denies_stays_denied_under_a_writer() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    let m7 = &lab.m7;
    on_the_alab_line(configured(m7).unwrap().signer);
    assert!(matches!(
        steward::observe_alab_role(m7),
        steward::AlabRoleObservation::Allowed { .. }
    ));
    publish_roster(&lab, b"deny_labels = [\"m7\"]\n");
    assert!(is_unavailable(m7, Unavailable::Revoked));
    {
        let _writer = StateLock::take(m7).unwrap();
        assert!(matches!(
            steward::local_recipient_denied(&Reads::new(m7)),
            Ok(true)
        ));
        assert!(steward::local_recipient_denied_in_roster(m7));
        assert!(matches!(
            steward::observe_alab_role(m7),
            steward::AlabRoleObservation::Denied
        ));
        assert!(is_unavailable(m7, Unavailable::Busy));
        assert_eq!(
            swarm_roster_until(m7, Instant::now()).unwrap_err().kind(),
            crate::ErrorKind::VaultBusy
        );
        assert!(roster::swarm_roster_for_denial(m7).is_err());
    }
    assert!(is_unavailable(m7, Unavailable::Revoked));
    off_the_alab_line();
    lab.server.stop();
}

/// A writer mid-apply that has made this Mac's removal durable and still holds its lock: the next
/// look reads the removal, and what the last look read answers nothing.
///
/// NEGATIVE CONTROL (2026-11-05): with the writer's arm of `observe_alab_role_inner` answering
/// `unavailable()`, the role reads `Unavailable` (`matches!(… Denied)`).
#[test]
fn a_removal_a_writer_made_durable_is_read_while_it_holds_the_lock() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    let m7 = &lab.m7;
    on_the_alab_line(configured(m7).unwrap().signer);
    let denied = || steward::local_recipient_denied(&Reads::new(m7));
    assert!(matches!(denied(), Ok(false)));
    let recipient = read_identity(m7).unwrap().unwrap().recipient();
    {
        let lock = StateLock::take(&lab.m22).unwrap();
        log_entries::retire_recipients_in_log_local(
            &lab.m22,
            &lock,
            &[],
            &BTreeSet::from([recipient]),
        )
        .unwrap();
    }
    let records = load_records(&lab.m22).unwrap().encode();
    {
        let _writer = StateLock::take(m7).unwrap();
        assert!(matches!(denied(), Ok(false)), "not on this Mac yet");
        save_records_unlocked(m7, &records).unwrap();
        assert!(matches!(denied(), Ok(true)), "the next look reads it");
        assert!(matches!(
            steward::observe_alab_role(m7),
            steward::AlabRoleObservation::Denied
        ));
        assert!(is_unavailable(m7, Unavailable::Busy));
    }
    assert!(is_unavailable(m7, Unavailable::Revoked));
    off_the_alab_line();
    lab.server.stop();
}

/// A denial check that keeps failing holds a role for [`crate::cli::ROLE_UNJUDGED_POLLS`] polls and
/// no more, whichever file it was and whether or not the signer denied this Mac, because this Mac
/// cannot tell which. A Mac still in gets its role back once the fault clears; a denied one does
/// not. A new server gets none from any of it. With the applied records absent the cached envelope
/// still judges this Mac, so a held role outlasts the bound, as
/// `docs/measured/vault-cold-server-role-2026-11-04.md` decided for a warm server. Records that are
/// there and cannot be read (their mode refused, their bytes corrupt) judge nothing and may hold a
/// removal the envelope is behind, so where the envelope does not deny this Mac they get the bound,
/// as the vault file does. A denial that envelope holds ends the role at the first poll, whatever
/// became of the records.
///
/// NEGATIVE CONTROLS (2026-11-06): with the bound gone from `ServeRole::apply`, the role outlasts it
/// (`denied false, the vault file refused by its mode: the bound ends it`); with the records gone
/// read as a check that judged nothing (`Judgement::EnvelopeClear` mapped to `Unjudged`), the
/// role ends (`denied false, the applied records gone: a warm role outlasts it`); with
/// `EnvelopeClear` given to records that are there (no `records_storage::absent` gate), the role
/// outlasts the bound (`denied false, the records file refused by its mode (writable by others):
/// the bound ends it`); with the envelope's denial read as clear while custody fails, the role
/// stands (`denied true, the records file refused by its mode (writable by others): the
/// envelope's denial ends it`).
#[test]
fn a_denial_check_that_keeps_failing_ends_a_held_role() {
    use std::os::unix::fs::PermissionsExt as _;
    type Fault<'a> = (&'a str, &'a dyn Fn(), &'a dyn Fn());
    /// What a held role does while the fault lasts.
    #[derive(Clone, Copy)]
    enum During {
        /// It stands for the bound and the bound ends it.
        Bounded,
        /// It stands past the bound: the cached envelope judged this Mac.
        Outlasts,
        /// The first poll ends it: the cached envelope denies this Mac.
        Denied,
    }
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    let m7 = &lab.m7;
    let signer = configured(m7).unwrap().signer;
    on_the_alab_line(signer);
    let mut roles: [crate::cli::ServeRole; 10] = Default::default();
    for role in &mut roles {
        assert_eq!(poll_role(&lab, signer, role).as_deref(), Some("m7"));
    }
    let mut roles = roles.iter_mut();
    let mode = |path: std::path::PathBuf, mode| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let held = std::cell::RefCell::new(Vec::new());
    let refused_vault: Fault = (
        "the vault file refused by its mode",
        &|| mode(m7.cache_file(), 0o644),
        &|| mode(m7.cache_file(), 0o600),
    );
    let corrupt_state: Fault = (
        "a corrupt state.toml",
        &|| {
            *held.borrow_mut() = std::fs::read(m7.state_file()).unwrap();
            fsx::write_atomic(&m7.state_file(), b"not valid toml [", 0o600).unwrap();
        },
        &|| fsx::write_atomic(&m7.state_file(), &held.borrow(), 0o600).unwrap(),
    );
    let refused_records: Fault = (
        "the records file refused by its mode (writable by others)",
        &|| mode(m7.records_data_file().unwrap(), 0o666),
        &|| mode(m7.records_data_file().unwrap(), 0o600),
    );
    let corrupt_records: Fault = (
        "a corrupt records file",
        &|| {
            let data = m7.records_data_file().unwrap();
            *held.borrow_mut() = std::fs::read(&data).unwrap();
            fsx::write_atomic(&data, b"not membership records", 0o600).unwrap();
        },
        &|| fsx::write_atomic(&m7.records_data_file().unwrap(), &held.borrow(), 0o600).unwrap(),
    );
    let records_gone: Fault = (
        "the applied records gone",
        &|| {
            *held.borrow_mut() = std::fs::read(m7.records_file()).unwrap();
            std::fs::remove_file(m7.records_file()).unwrap();
        },
        &|| {
            save_records(m7, &held.borrow()).unwrap();
        },
    );
    let label = Some("m7".to_owned());
    let mut under = |denied: bool, during: During, (fault, begin, end): Fault| {
        let row = format!("denied {denied}, {fault}");
        let role = roles.next().unwrap();
        begin();
        match during {
            During::Bounded | During::Outlasts => {
                for poll in 0..crate::cli::ROLE_UNJUDGED_POLLS {
                    let during = poll_role(&lab, signer, role);
                    assert_eq!(during, label, "{row}, poll {poll}");
                }
            }
            During::Denied => {}
        }
        match during {
            During::Bounded => assert_eq!(
                poll_role(&lab, signer, role),
                None,
                "{row}: the bound ends it"
            ),
            During::Outlasts => assert_eq!(
                poll_role(&lab, signer, role),
                label,
                "{row}: a warm role outlasts it"
            ),
            During::Denied => assert_eq!(
                poll_role(&lab, signer, role),
                None,
                "{row}: the envelope's denial ends it"
            ),
        }
        assert_eq!(
            poll_role(&lab, signer, &mut crate::cli::ServeRole::default()),
            None,
            "{row}: a new server"
        );
        end();
        let again = if denied { None } else { label.clone() };
        assert_eq!(
            poll_role(&lab, signer, role),
            again,
            "{row}: once it clears"
        );
    };
    let records = [refused_records, corrupt_records];
    for fault in [refused_vault, corrupt_state].into_iter().chain(records) {
        under(false, During::Bounded, fault);
    }
    under(false, During::Outlasts, records_gone);
    publish_roster(&lab, b"deny_labels = [\"m7\"]\n");
    for fault in [refused_vault, corrupt_state] {
        under(true, During::Bounded, fault);
    }
    // A denial the cached envelope holds is read whatever became of the records, so no bound
    // comes into it.
    for fault in records.into_iter().chain([records_gone]) {
        under(true, During::Denied, fault);
    }
    off_the_alab_line();
    lab.server.stop();
}

/// [`two_macs`] before its genesis: `m7` has applied a version of a vault that holds no log.
fn macs_on_a_vault_with_no_log(root: &Path) -> (crate::server::Running, Paths, Paths) {
    let server = log_entries_tests::start_loopback_server(&root.join("s1"));
    let url = format!("http://{}/v1/alab", server.addr);
    let m22 = Paths::for_home(&root.join("m22"));
    let created = test_create_unbound(&m22, &url, None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).unwrap();
    let m7 = Paths::for_home(&root.join("m7"));
    let recipient = machine_identity(&m7).unwrap().recipient().to_bech32();
    let admin = admin_open(&m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine("m7", &recipient).unwrap();
    admin_publish_approved(&m22, &admin, &doc, std::slice::from_ref(&recipient)).unwrap();
    log_entries_tests::join_host(&m7, &format!("{url}#{}", created.signer), "m7");
    log_entries_tests::enroll_commons(&m22, &m7);
    (server, m22, m7)
}

/// `from`'s log and vault file on `m7`'s disk, through no apply.
fn hold_unapplied(m7: &Paths, from: &Paths) {
    let records = load_records(from).unwrap();
    assert!(
        records
            .log()
            .is_some_and(crate::membership_log::Log::has_epochs)
    );
    fsx::write_atomic(&m7.records_file(), &records.encode(), 0o600).unwrap();
    let vault = std::fs::read(from.cache_file()).unwrap();
    fsx::write_atomic(&m7.cache_file(), &vault, 0o600).unwrap();
}

/// The vault file's signer and handoff say which genesis a log may have only once that file is the
/// version this Mac applied. Otherwise its custody is what its configuration alone admits.
///
/// NEGATIVE CONTROLS (2026-11-05): with `self.verified()?.is_some()` gone from `Reads::custody`,
/// custody takes the rotated key's log (the second arm's `reads.custody().unwrap().is_none()`). The
/// other key's log needs that and `store.under(&reads.lineage(config))` gone from
/// `records_store_selected`, because the store refuses its genesis first (the first arm's).
#[test]
fn custody_takes_no_log_on_the_word_of_a_vault_file_this_mac_did_not_apply() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (server, m22, m7) = macs_on_a_vault_with_no_log(root.path());
    let pin = configured(&m7).unwrap().signer.unwrap();
    assert!(Reads::new(&m7).verified().unwrap().is_some());

    // Another key's vault of this name, and the log its key signed.
    let rival_server = log_entries_tests::start_loopback_server(&root.path().join("s2"));
    let rival = Paths::for_home(&root.path().join("rival"));
    let rival_url = format!("http://{}/v1/alab", rival_server.addr);
    let created = test_create_unbound(&rival, &rival_url, None, Some("m66")).unwrap();
    admin_keys::legacy::publish_legacy(&rival, &admin_open(&rival).unwrap());
    admin_commons_init(&rival).unwrap();
    let rival_key = decode_key32(&created.signer).unwrap();
    genesis::sign_genesis(&rival, &root.path().join("rival.genesis"), &rival_key, true).unwrap();
    hold_unapplied(&m7, &rival);
    let reads = Reads::new(&m7);
    assert_eq!(
        reads.vault().unwrap().unwrap().envelope().unwrap().signer,
        rival_key
    );
    assert!(reads.verified().unwrap().is_none());
    assert!(reads.custody().unwrap().is_none());
    assert!(observe_commons(&m7).is_err());

    // A genuine later version under a rotated key, not applied here, and the log that key signed:
    // its lineage reaches this pin and would take the log, and custody still does not.
    let opened = admin_open(&m22).unwrap();
    admin_rotate(&m22, &opened, &opened.doc.clone()).unwrap();
    let rotated = configured(&m22).unwrap().signer.unwrap();
    assert_ne!(rotated, pin);
    genesis::sign_genesis(&m22, &root.path().join("rotated.genesis"), &rotated, true).unwrap();
    hold_unapplied(&m7, &m22);
    let reads = Reads::new(&m7);
    let vault = reads.vault().unwrap().unwrap();
    let config = reads.config().unwrap().unwrap();
    assert_eq!(vault.envelope().unwrap().signer, rotated);
    assert!(
        vault
            .verify_for_reader(&config.name, &pin, &config.followed, None)
            .is_ok()
    );
    assert!(reads.verified().unwrap().is_none());
    assert!(reads.seeded().unwrap().is_some());
    assert!(reads.configured().unwrap().is_none());
    assert!(reads.custody().unwrap().is_none());
    assert!(is_unavailable(&m7, Unavailable::PendingApply));
    rival_server.stop();
    server.stop();
}

/// A `source.toml` naming no `followed` takes them from the vault file as it stands.
///
/// NEGATIVE CONTROL (2026-11-05): with `cached_chain` and `Reads::config` answering the last chain
/// of that file and pin when the read gives nothing, the moved file keeps its keys (`left:
/// [[254, 87, …]], right: []`).
#[test]
fn a_chain_read_from_the_vault_file_goes_with_the_file_and_returns_with_it() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    rotate(&lab);
    let m7 = &lab.m7;
    let source = configured(m7).unwrap();
    let followed = source.followed.clone();
    assert!(!followed.is_empty());
    save_config(
        m7,
        &Config {
            followed: Vec::new(),
            ..source
        },
    )
    .unwrap();
    let in_reads = |paths| {
        Reads::new(paths)
            .config()
            .unwrap()
            .unwrap()
            .followed
            .clone()
    };
    assert_eq!(configured(m7).unwrap().followed, followed);
    assert_eq!(in_reads(m7), followed);
    let aside = m7.dir.join("vault.aside");
    std::fs::rename(m7.cache_file(), &aside).unwrap();
    assert_eq!(configured(m7).unwrap().followed, Vec::<[u8; 32]>::new());
    assert_eq!(in_reads(m7), Vec::<[u8; 32]>::new());
    std::fs::rename(&aside, m7.cache_file()).unwrap();
    assert_eq!(configured(m7).unwrap().followed, followed);
    assert_eq!(in_reads(m7), followed);
    lab.server.stop();
}

/// A vault with no log has no seat to carry a node, so only the device says this Mac is the node
/// its roster denies. A device it cannot read answers neither way: a held role stands as under any
/// failed check, a new server gets none and every other reader refuses, and a denial it can read
/// elsewhere still ends all of it. A key gone from under its claim is such a device. A Mac with no
/// device is no node, which that deny list does not reach.
///
/// NEGATIVE CONTROLS (2026-11-05): with `roster_denies_local` taking a failed read as no node, the
/// new server gets the role (`the read fails: left: Some("m7"), right: None`); with a key gone from
/// under its claim reported as no device, likewise (`the key is gone: left: Some("m7")`); with no
/// device reported as a failed read, `no device: left: None, right: Some("m7")`; with the device's
/// error returned ahead of the other denials, `a label denies it too: left: Some("m7"), right:
/// None`.
#[test]
fn a_device_it_cannot_read_hides_no_node_denial_on_a_vault_with_no_log() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (server, m22, m7) = macs_on_a_vault_with_no_log(root.path());
    let signer = configured(&m7).unwrap().signer;
    on_the_alab_line(signer);
    let store = root.path().join("role-server");
    let poll = |role: &mut crate::cli::ServeRole| poll_server(&m7, &store, signer, role);
    let new_server = || poll(&mut crate::cli::ServeRole::default());
    let label = Some("m7".to_owned());
    let (mut read_fails, mut key_gone, mut also_label) = <(
        crate::cli::ServeRole,
        crate::cli::ServeRole,
        crate::cli::ServeRole,
    )>::default();
    for role in [&mut read_fails, &mut key_gone, &mut also_label] {
        assert_eq!(poll(role), label);
    }
    assert!(is_visible(&observe_commons(&m7)));

    let device = crate::commons::bootstrap::Device::open(&m7)
        .unwrap()
        .unwrap();
    let writer = device
        .request([1; 16], [2; 32])
        .unwrap()
        .binding()
        .writer()
        .to_owned();
    let recipient = read_identity(&m7).unwrap().unwrap().recipient().to_bech32();
    let deny = |roster: crate::swarm_roster::SwarmRoster| {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish_approved(&m22, &admin, &doc, std::slice::from_ref(&recipient)).unwrap();
        sync(&m7, None).unwrap();
    };
    let mut roster = crate::swarm_roster::SwarmRoster {
        deny_nodes: vec![aterm_codec::hex::encode_lower(&writer)],
        ..Default::default()
    };
    deny(roster.clone());

    // Each denied reader, with a device it can read.
    assert_eq!(new_server(), None);
    assert!(is_unavailable(&m7, Unavailable::Revoked));
    assert!(steward::local_recipient_denied_in_roster(&m7));

    // Each reader with one it cannot. The fault gone, the denial reads again.
    let unreadable = |row: &str, held: &mut crate::cli::ServeRole| {
        assert_eq!(poll(held), label, "{row}");
        assert_eq!(new_server(), None, "{row}");
        assert!(observe_commons(&m7).is_err(), "{row}");
        assert!(steward::local_recipient_denied_in_roster(&m7), "{row}");
    };
    let commons = m7.dir.join("commons");
    let key = commons.join("device.age");
    let key_aside = commons.join("device.age.aside");
    fsx::fail_reads(&key, usize::MAX);
    unreadable("the read fails", &mut read_fails);
    fsx::fail_reads(&key, 0);
    assert_eq!(poll(&mut read_fails), None);
    std::fs::rename(&key, &key_aside).unwrap();
    unreadable("the key is gone", &mut key_gone);
    std::fs::rename(&key_aside, &key).unwrap();
    assert_eq!(poll(&mut key_gone), None);

    // With neither its key nor its claim it is no node.
    let claim = commons.join("device.claim");
    let claim_aside = commons.join("device.claim.aside");
    std::fs::rename(&key, &key_aside).unwrap();
    std::fs::rename(&claim, &claim_aside).unwrap();
    assert_eq!(new_server(), label, "no device");
    assert!(is_visible(&observe_commons(&m7)), "no device");
    assert!(!steward::local_recipient_denied_in_roster(&m7), "no device");
    std::fs::rename(&key_aside, &key).unwrap();
    std::fs::rename(&claim_aside, &claim).unwrap();

    // A denial it can read wins over the device it cannot.
    roster.deny_labels = vec!["m7".to_owned()];
    deny(roster);
    fsx::fail_reads(&key, usize::MAX);
    assert_eq!(poll(&mut also_label), None, "a label denies it too");
    assert!(
        is_unavailable(&m7, Unavailable::Revoked),
        "a label denies it too"
    );
    fsx::fail_reads(&key, 0);
    off_the_alab_line();
    server.stop();
}

/// A device it cannot read, on a Mac still in, under a roster that denies another node: one failed
/// read costs it nothing, and while it lasts no reader takes this Mac for the node it may not be.
///
/// NEGATIVE CONTROL (2026-11-05): with `roster_denies_local` taking a failed read as no node, the
/// new server gets the role (`left: Some("m7"), right: None`).
#[test]
fn a_device_it_cannot_read_gives_a_mac_still_in_no_new_authority() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    with_a_denied_node(&lab);
    let m7 = &lab.m7;
    let signer = configured(m7).unwrap().signer;
    on_the_alab_line(signer);
    let new_server = || poll_role(&lab, signer, &mut crate::cli::ServeRole::default());
    let label = Some("m7".to_owned());
    let mut held = crate::cli::ServeRole::default();
    assert_eq!(poll_role(&lab, signer, &mut held), label);
    let key = m7.dir.join("commons/device.age");
    fsx::fail_reads(&key, 1);
    assert_eq!(poll_role(&lab, signer, &mut held), label);
    fsx::fail_reads(&key, usize::MAX);
    assert_eq!(poll_role(&lab, signer, &mut held), label);
    assert_eq!(new_server(), None);
    assert!(observe_commons(m7).is_err());
    assert!(steward::local_recipient_denied_in_roster(m7));
    fsx::fail_reads(&key, 0);
    assert_eq!(new_server(), label);
    assert!(is_visible(&observe_commons(m7)));
    assert!(!steward::local_recipient_denied_in_roster(m7));
    off_the_alab_line();
    lab.server.stop();
}

/// A node denial depends on `device.age` and `device.claim`, which the memo's key omits, so a
/// log-only observation asks it on every pass, as an applied one does.
///
/// NEGATIVE CONTROLS (2026-11-05): with `OBSERVE_COMMONS_CACHE`'s lookup moved ahead of the log-only
/// branch's denial check, the second observation reads no `device.age` (`left: None, right:
/// Some(1)`); with that count off as well, the denied device reads as visible
/// (`is_unavailable(&paths, Unavailable::Revoked)`).
#[test]
fn a_log_only_observation_judges_the_device_it_finds_behind_its_memo() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    let (reader, request, allowed) = log_entries_tests::newcomer(lab.root.path(), &lab.m22, "m8");

    // Another device of this Mac's key, which the roster denies.
    let denied = Paths::for_home(&lab.root.path().join("denied-device"));
    log_entries_tests::clone_identity_to(&reader, &denied);
    let device = crate::commons::bootstrap::Device::open(&denied)
        .unwrap()
        .unwrap();
    let binding = device.request([1; 16], [2; 32]).unwrap().binding().clone();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    let roster = crate::swarm_roster::SwarmRoster {
        deny_nodes: vec![aterm_codec::hex::encode_lower(binding.writer())],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let (bundle, logged) = log_entries_tests::hand(&lab.m22, &request, &allowed, true);
    assert!(logged);

    // The Mac: its key and its log, and no device yet.
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    log_entries_tests::clone_identity_to(&reader, &paths);
    admit::write_from_log_bundle(&paths, &bundle).unwrap();
    assert_eq!(load_state(&paths).unwrap().digest, None);
    let commons = paths.dir.join("commons");
    for _ in 0..2 {
        let (observed, reads) = observed_with_reads(&paths);
        assert!(is_visible(&observed), "{observed:?}");
        assert_eq!(reads.get(&commons.join("device.age")), Some(&1));
    }

    // The denied device arrives. Nothing the memo's key holds has moved.
    fsx::ensure_private_dir(&commons).unwrap();
    for name in ["device.age", "device.claim"] {
        let bytes = std::fs::read(denied.dir.join("commons").join(name)).unwrap();
        fsx::write_atomic(&commons.join(name), &bytes, 0o600).unwrap();
    }
    assert!(is_unavailable(&paths, Unavailable::Revoked));
    lab.server.stop();
}

/// NEGATIVE CONTROL (2026-11-05): with `observe_commons` answering each home's last visible
/// snapshot, its first change is missed (`is_unavailable(m7, Unavailable::MissingIdentity)`).
#[test]
fn a_change_on_disk_between_two_observations_is_seen_by_the_second() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    let m7 = &lab.m7;
    assert!(is_visible(&observe_commons(m7)));
    assert!(is_visible(&observe_commons(m7)));

    // A lost identity, and it back.
    let aside = m7.dir.join("identity.aside");
    std::fs::rename(m7.identity_file(), &aside).unwrap();
    assert!(is_unavailable(m7, Unavailable::MissingIdentity));
    std::fs::rename(&aside, m7.identity_file()).unwrap();
    assert!(is_visible(&observe_commons(m7)));

    // A revocation this Mac applied, and the state back.
    let applied = load_state(m7).unwrap();
    save_state(
        m7,
        &State {
            membership: Stored::Revoked,
            ..applied.clone()
        },
    )
    .unwrap();
    assert!(is_unavailable(m7, Unavailable::Revoked));
    save_state(m7, &applied).unwrap();
    assert!(is_visible(&observe_commons(m7)));

    // A moved source, and it back.
    let source = configured(m7).unwrap();
    save_config(
        m7,
        &Config {
            source: Source::Url("http://127.0.0.1:1/v1/elsewhere".to_owned()),
            ..source.clone()
        },
    )
    .unwrap();
    assert!(is_unavailable(m7, Unavailable::SourceChanged));
    save_config(m7, &source).unwrap();
    assert!(is_visible(&observe_commons(m7)));

    // A newer version on disk and not applied, and the applied one back.
    let held = std::fs::read(m7.cache_file()).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "another-entry", b"value")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let newer = std::fs::read(lab.m22.cache_file()).unwrap();
    fsx::write_atomic(&m7.cache_file(), &newer, 0o600).unwrap();
    assert!(is_unavailable(m7, Unavailable::PendingApply));
    fsx::write_atomic(&m7.cache_file(), &held, 0o600).unwrap();
    assert!(is_visible(&observe_commons(m7)));

    // A revocation written to the log: the next observation reports it, with the file it was
    // visible under untouched.
    let recipient = read_identity(m7).unwrap().unwrap().recipient();
    {
        let lock = StateLock::take(&lab.m22).unwrap();
        log_entries::retire_recipients_in_log_local(
            &lab.m22,
            &lock,
            &[],
            &BTreeSet::from([recipient]),
        )
        .unwrap();
    }
    save_records(m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    assert!(is_unavailable(m7, Unavailable::Revoked));
    lab.server.stop();
}

/// A vault the signer changed to deny this Mac: its roster, applied between two observations.
///
/// NEGATIVE CONTROLS (2026-11-05): with the denial check gone from `observe_commons`'s applied
/// branch, or `observe_commons` answering each home's last visible snapshot, the denied Mac reads as
/// visible (`is_unavailable(&lab.m7, Unavailable::Revoked)`).
#[test]
fn a_roster_denial_applied_between_two_observations_is_seen_by_the_second() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    with_a_denied_node(&lab);
    assert!(is_visible(&observe_commons(&lab.m7)));
    let device = crate::commons::bootstrap::Device::open(&lab.m7)
        .unwrap()
        .unwrap();
    let binding = device.request([1; 16], [2; 32]).unwrap().binding().clone();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    let roster = crate::swarm_roster::SwarmRoster {
        deny_nodes: vec![aterm_codec::hex::encode_lower(binding.writer())],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync_with(&lab.m7, std::fs::read(lab.m22.cache_file()).unwrap()).unwrap();
    assert!(is_unavailable(&lab.m7, Unavailable::Revoked));
    lab.server.stop();
}

/// A take of the state lock with nothing to finish reads each file it holds once: the step that
/// finishes a prepared transition reads the records layout before any read is shared, the shared
/// selection answers from its `lstat`, and the recovery steps share one read of the claim, the
/// journal, the configuration and the records.
///
/// NEGATIVE CONTROLS (2026-11-05): with [`retained_history::acknowledge_under`] reading for itself
/// (`acknowledge_selected(&Reads::new(reads.paths()), ...)`), `read more than once` names
/// `records.frontiers` and `frontiers.checkpoint`; with [`crate::records_storage::selected`] always
/// through `data_path`, it names `records.layout`.
#[test]
fn a_state_lock_take_with_nothing_to_finish_reads_each_file_once() {
    let _answering = ServersAnswering::set();
    let lab = two_macs(&[]);
    for paths in [&lab.m22, &lab.m7] {
        let before = fsx::reads_under(&paths.home);
        drop(StateLock::take(paths).unwrap());
        let mut reads = fsx::reads_under(&paths.home);
        reads.retain(|path, count| {
            *count -= before.get(path).copied().unwrap_or(0);
            *count > 0
        });
        assert_read_once(
            &reads,
            &[
                paths.source_file(),
                paths.dir.join("records.layout"),
                paths.dir.join("records.frontiers"),
                paths.dir.join("frontiers.checkpoint"),
            ],
        );
    }
    lab.server.stop();
}
