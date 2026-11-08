// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the readers that ask every few seconds cost on an unchanged log (the Commons owner's
//! poll, the members desk, the fleet listener's deny list, a survey's roster): counted, never
//! timed. Each is held at the owner's size: 65 Macs, eight of them removed and four of those let
//! back in, about 2,650 records (`aterm db network-state` named 65 peers on 2026-11-06).

use super::roster::RESTORED_DERIVED;
use super::*;
use crate::commons;
use crate::membership_log::{
    Body, CLOSURE_RECORDS, Draft, Entrant, Evidence, Founder, Log, MEMBERSHIP_CLONES, MemberId,
    Record, RecordId, Root,
};
use crate::swarm_roster::{SwarmMember, SwarmRoster};
use std::collections::BTreeMap;

struct Mac {
    seed: [u8; 32],
    incarnation: [u8; 16],
    identity: Identity,
    member: MemberId,
}

fn mac(tag: u8) -> (Mac, Entrant) {
    let seed = [tag; 32];
    let incarnation = [tag; 16];
    let identity = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
    let request = crate::commons::binding::Request::create(
        [0x31; 16],
        [0x42; 32],
        [tag.wrapping_add(0x20); 16],
        incarnation,
        &identity,
        &seed,
    )
    .expect("request");
    let member = request.binding().actor();
    let entrant = Entrant {
        request,
        node_key: [tag.wrapping_add(0x80); 32],
        name: format!("mac-{tag}"),
        label: format!("m{tag}"),
    };
    (
        Mac {
            seed,
            incarnation,
            identity,
            member,
        },
        entrant,
    )
}

fn sign(by: &Mac, log: &mut Log, draft: Draft) -> RecordId {
    let record = draft.sign(&by.seed, by.incarnation).expect("sign");
    let id = *record.id();
    log.insert(record).expect("insert");
    id
}

/// The founder reseals `[everyone]` to every member.
fn reseal(log: &mut Log, macs: &[Mac], generation: u64, ms: u64) {
    let (draft, key) = log
        .draft_epoch(&macs[0].member, ms, Audience::Everyone, generation)
        .expect("epoch");
    let epoch = sign(&macs[0], log, draft);
    let members = log.membership();
    for to in macs.iter().filter(|to| members.contains(&to.member)) {
        let wrap = log
            .draft_wrap(&macs[0].member, ms, epoch, &key, to.identity.recipient())
            .expect("wrap");
        sign(&macs[0], log, wrap);
    }
}

fn admit(log: &mut Log, macs: &[Mac], entrant: &Entrant, ms: u64) {
    let draft = log.draft(
        &macs[0].member,
        ms,
        Body::Admit {
            entrant: entrant.clone(),
            evidence: Evidence::Phrase,
        },
    );
    sign(&macs[0], log, draft);
}

/// A log of `n` Macs the founder let in one by one, resealing `[everyone]` after each; then
/// `removed` of them (`m3` on) taken out, half of those let back in, and `extra` reseals more.
fn big_log(n: u8, removed: u8, extra: u64) -> (Log, Vec<Mac>, Vec<Entrant>) {
    let (macs, entrants): (Vec<Mac>, Vec<Entrant>) = (1..=n).map(mac).unzip();
    let genesis = Record::genesis(
        &[7; 32],
        Root::of(entrants[0].request.binding()),
        vec![Founder::from(entrants[0].clone())],
        1_000,
    )
    .expect("genesis");
    let mut log = Log::new(genesis).expect("log");
    let mut ms = 1_001;
    let mut generation = 0;
    for entrant in entrants.iter().skip(1) {
        admit(&mut log, &macs, entrant, ms);
        generation += 1;
        reseal(&mut log, &macs, generation, ms);
        ms += 1;
    }
    for target in &macs[2..2 + usize::from(removed)] {
        let cutoff = log
            .membership()
            .get(&target.member)
            .map(|seat| seat.entry)
            .expect("a seat");
        let removal = log.draft(
            &macs[0].member,
            ms,
            Body::Remove {
                member: target.member,
                cutoff,
            },
        );
        sign(&macs[0], &mut log, removal);
        ms += 1;
    }
    generation += 1;
    reseal(&mut log, &macs, generation, ms);
    for entrant in &entrants[2..2 + usize::from(removed / 2)] {
        admit(&mut log, &macs, entrant, ms);
        ms += 1;
    }
    for _ in 0..=extra {
        generation += 1;
        reseal(&mut log, &macs, generation, ms);
        ms += 1;
    }
    (log, macs, entrants)
}

/// The roster a vault holds for these Macs: a row for each, by its recipient and node key.
fn roster_of(macs: &[Mac], entrants: &[Entrant]) -> SwarmRoster {
    SwarmRoster {
        members: macs
            .iter()
            .zip(entrants)
            .map(|(mac, entrant)| SwarmMember {
                recipient: mac.identity.recipient().to_bech32(),
                vault_name: entrant.name.clone(),
                label: entrant.label.clone(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 0,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: Some(aterm_codec::hex::encode_lower(&entrant.node_key)),
            })
            .collect(),
        ..Default::default()
    }
}

/// What one overlay of `roster` by `log` copied and derived: Membership snapshots cloned, records
/// copied out of causal closures, and restored-seat derivations.
fn overlay_cost(log: &Log, roster: &SwarmRoster) -> (SwarmRoster, [usize; 3]) {
    let before = [
        MEMBERSHIP_CLONES.get(),
        CLOSURE_RECORDS.get(),
        RESTORED_DERIVED.get(),
    ];
    let mut overlaid = roster.clone();
    roster::overlay_log_membership(log, None, &mut overlaid);
    let after = [
        MEMBERSHIP_CLONES.get(),
        CLOSURE_RECORDS.get(),
        RESTORED_DERIVED.get(),
    ];
    (overlaid, [0, 1, 2].map(|i| after[i] - before[i]))
}

/// The roster overlay every denial check and roster read runs, at the owner's size: the first
/// overlay of a log derives which Macs it let back in once, copying no record's causal past and
/// cloning no Membership; every later overlay of the same records, through any clone of the log,
/// derives nothing; and a log that holds one record more derives again and says what that record
/// changed. Every overlay names the same Macs restored and denied.
///
/// NEGATIVE CONTROLS (2026-11-06): with `restored_by_log`'s derivation back to a fresh one per call
/// (`log.restored(derive_restored)` replaced by `&derive_restored(log)`), the second overlay
/// derives again (`left: [0, 0, 1], right: [0, 0, 0]`); with the past of each seat taken by
/// `ancestor_closure` again (the shape before), the first overlay copies 118,200 records and
/// clones 189 Membership snapshots (`left: [189, 118200, 1], right: [0, 0, 1]`).
#[test]
fn an_overlay_of_an_unchanged_log_derives_its_restored_macs_once() {
    let (log, macs, entrants) = big_log(65, 8, 4);
    assert!(log.held_count() > 2_600, "{} records", log.held_count());
    let roster = roster_of(&macs, &entrants);
    let label = |tag: u8| format!("m{tag}");

    let (overlaid, cost) = overlay_cost(&log, &roster);
    assert_eq!(cost, [0, 0, 1], "the first overlay");
    // m3..m10 were taken out and m3..m6 let back in: m7..m10 stay out.
    let (restored, _) = roster::restored_by_log(&log);
    assert_eq!(restored, (3..=6).map(label).collect::<BTreeSet<_>>());
    let denied = |roster: &SwarmRoster| roster.deny_labels.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(
        denied(&overlaid),
        (7..=10).map(label).collect::<BTreeSet<_>>()
    );
    assert_eq!(overlaid.members.len(), 61);
    assert!(
        overlaid
            .members
            .iter()
            .all(|m| !overlaid.is_member_denied(m))
    );

    for again in [log.clone(), log.clone()] {
        let (same, cost) = overlay_cost(&again, &roster);
        assert_eq!(cost, [0, 0, 0], "an overlay of the same records");
        assert_eq!(same, overlaid);
    }

    // m7 let back in too: the log holding that record derives again, and m7 is restored.
    let mut grown = log.clone();
    admit(&mut grown, &macs, &entrants[6], 9_000);
    let (regrown, cost) = overlay_cost(&grown, &roster);
    assert_eq!(cost, [0, 0, 1], "the first overlay of one record more");
    assert_eq!(
        roster::restored_by_log(&grown).0,
        (3..=7).map(label).collect::<BTreeSet<_>>()
    );
    assert_eq!(
        denied(&regrown),
        (8..=10).map(label).collect::<BTreeSet<_>>()
    );
    // The log it grew from still answers as it did.
    assert_eq!(overlay_cost(&log, &roster), (overlaid, [0, 0, 0]));
}

fn verdict(observed: &crate::Result<crate::commons::Observation>) -> String {
    match observed {
        Ok(crate::commons::Observation::Visible(_)) => "visible".to_owned(),
        Ok(crate::commons::Observation::Unavailable(reason)) => reason.code().to_owned(),
        Err(error) => format!("error: {error}"),
    }
}

/// The files under `paths`' home that `read` opened, with how many times.
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

/// A settled observation of `paths` is the fresh one, and a second one of the same files opens
/// none of them, the records pin included (`Paths::records_data_file` names the data file to stamp
/// from `records_storage::selected`'s `lstat`s); every file the fresh one reads is among those
/// whose stamps the memo compares.
///
/// NEGATIVE CONTROL (2026-11-08): with `Paths::records_data_file` back on
/// `records_storage::data_path`, the second observation opens the missing pin (`a settled
/// observation of unchanged files`).
fn settles(paths: &Paths, expected: &str) {
    let (fresh, read) = reads_during(paths, || observe_commons(paths));
    assert_eq!(verdict(&fresh), expected);
    let inputs = vault_memo::observation_inputs(paths).expect("the inputs");
    let unstamped: Vec<_> = read.keys().filter(|path| !inputs.contains(path)).collect();
    assert!(unstamped.is_empty(), "read but not stamped: {unstamped:#?}");
    assert_eq!(verdict(&commons::observe_settled(paths)), expected);
    let (again, read) = reads_during(paths, || commons::observe_settled(paths));
    assert_eq!(verdict(&again), expected);
    let read: Vec<_> = read.into_iter().collect();
    assert_eq!(read, vec![], "a settled observation of unchanged files");
}

fn with_a_denied_node(m22: &Paths, m7: &Paths) {
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    let roster = SwarmRoster {
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
    admin_publish(m22, &admin, &doc).unwrap();
    sync(m7, None).unwrap();
}

fn narrow_commons_to_m22(m22: &Paths) {
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    let control = doc
        .entries()
        .into_iter()
        .find(|entry| entry.name == crate::commons::ENTRY)
        .expect("the Commons control entry");
    doc.unset_entry(&control.audience, crate::commons::ENTRY)
        .unwrap();
    doc.set_entry(
        &Audience::Hosts(vec!["m22".to_owned()]),
        crate::commons::ENTRY,
        control.value.as_slice(),
    )
    .unwrap();
    log_entries::as_0154(|| admin_publish(m22, &admin, &doc))
        .expect("publish the narrowed Commons control");
}

/// The Commons runtime's presence poll and the members desk ask for the Commons observation
/// every few seconds of files that change only when the vault does. A settled observation of
/// unchanged files reads none of them, whatever it found: control visible (with a roster whose
/// denial check reads this Mac's device), and control narrowed away from a member and from a
/// Mac approved after the genesis. A Mac its vault narrows out of control is seen out at the next
/// call, with no window: the vault write moved the stamps of a file it had kept.
///
/// NEGATIVE CONTROL (2026-11-06): with `observe_settled` reading through `observe_commons` (the
/// poll before), the second observation of unchanged files opens all nine again (`left:
/// [(".../commons/device.age", 1), (".../commons/device.claim", 1), ..., (".../vault", 1)],
/// right: []`); with `observation_inputs` left at the deny list's, the device files are read but
/// not stamped (`read but not stamped: [".../commons/device.age", ".../commons/device.claim"]`).
#[test]
fn a_settled_observation_of_unchanged_files_reads_none_of_them() {
    let _answering = ServersAnswering::set();
    let lab =
        log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    with_a_denied_node(&lab.m22, &lab.m7);
    settles(&lab.m7, "visible");

    // m9: approved after the genesis and joined while control reached it.
    let m9 = Paths::for_home(&lab.root.path().join("m9"));
    let m9_rec = machine_identity(&m9).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m9", &m9_rec).unwrap();
        admin_publish_approved(&lab.m22, &admin, &doc, std::slice::from_ref(&m9_rec)).unwrap();
    }
    log_entries_tests::join_host(&m9, &format!("{}#{}", lab.url, lab.signer), "m9");
    let _ = crate::commons::bootstrap::Bootstrap::required(&m9);

    narrow_commons_to_m22(&lab.m22);
    sync(&lab.m7, None).expect("m7 syncs");
    sync(&m9, None).expect("m9 syncs");
    let out = verdict(&observe_commons(&lab.m7));
    assert_ne!(out, "visible", "m7 is narrowed out of control");
    assert_eq!(
        verdict(&commons::observe_settled(&lab.m7)),
        out,
        "at the next call"
    );
    settles(&lab.m7, &out);
    settles(&m9, &verdict(&observe_commons(&m9)));
    lab.server.stop();
}

/// What the members desk reads of `paths` ([`Local::of`](crate::members::desk::Local::of),
/// [`undo_offers`] and, for no member, whether it left: the Commons observation and
/// [`member_by_log`]) finds a member or not as `member` says, and every file it reads is among
/// those [`desk_inputs`] stamps; reading them moves no stamp.
fn desk_reads_only_stamped_files(paths: &Paths, member: bool) {
    let before = desk_inputs(paths).expect("the desk's inputs stamp");
    let ((local, offers), read) = reads_during(paths, || {
        let local = crate::members::desk::Local::of(paths);
        if matches!(local, Ok(None)) {
            let _ = (crate::commons::observe(paths), member_by_log(paths));
        }
        (local, undo_offers(paths))
    });
    assert_eq!(
        local.expect("readable").is_some(),
        member,
        "a member's desk"
    );
    offers.expect("its removals read");
    let inputs = vault_memo::desk_files(paths).expect("the inputs");
    let unstamped: Vec<_> = read.keys().filter(|path| !inputs.contains(path)).collect();
    assert!(unstamped.is_empty(), "read but not stamped: {unstamped:#?}");
    assert!(
        desk_inputs(paths) == Some(before),
        "a desk's read moves none of its inputs"
    );
}

/// The members desk reads this Mac's membership every 30 s, and at every network move, whatever
/// it found; its lane reads again only when [`desk_inputs`] moved since a settled read. Every file
/// the desk reads is one those stamps cover, for a member that holds Commons control (with a roster
/// whose denial check reads its device) and for one the Commons epochs leave out (its log seat,
/// pinned anchor and control journal read instead), and for that member once it was removed; the
/// removal moves them.
///
/// NEGATIVE CONTROL (2026-11-06): with `desk_files` left at `observation_inputs`, the custody and
/// claims the member's desk is read from are read but not stamped (`read but not stamped:
/// [".../commons/control/control.anchor", ".../commons/default.pin", ".../commons.claim",
/// ".../creation.claim"]`).
#[test]
fn the_desk_reads_only_files_its_inputs_stamp() {
    let _answering = ServersAnswering::set();
    let lab =
        log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    with_a_denied_node(&lab.m22, &lab.m7);
    desk_reads_only_stamped_files(&lab.m7, true);
    lab.server.stop();

    let root = aterm_tempfile::tempdir().unwrap();
    let excluded = test_commons_excluded(root.path()).expect("the excluded member");
    desk_reads_only_stamped_files(&excluded.member, true);
    let before = desk_inputs(&excluded.member);
    remove_member(&excluded.admin, "member", false).expect("the admin removes the member");
    sync(&excluded.member, None).expect("the member syncs its removal");
    assert!(
        desk_inputs(&excluded.member) != before,
        "a removal moves the desk's inputs"
    );
    desk_reads_only_stamped_files(&excluded.member, false);
    excluded.server.stop();
}

/// The trust a Commons peer check of `paths`' log is made under: its genesis root.
fn trust_of(paths: &Paths) -> crate::commons::journal::Trust {
    let store = load_acknowledged_records(paths).unwrap();
    let crate::membership_log::Body::Genesis { root, .. } = store.log().unwrap().genesis().body()
    else {
        panic!("a log begins with its genesis");
    };
    crate::commons::journal::Trust {
        network: root.network,
        authority: root.authority,
        genesis: [0; 32],
    }
}

fn denies(roster: &crate::Result<Option<SwarmRoster>>, label: &str) -> bool {
    roster
        .as_ref()
        .is_ok_and(|roster| roster.as_ref().is_some_and(|r| r.is_label_denied(label)))
}

/// A Commons peer check reads this Mac's roster, its acknowledged log and the roster for that log
/// at every check, four or five checks a session, every session. Every file those reads open is
/// one the memos stamp; read through them again, with no file moved, they open none, the records
/// pin included; a vault change is read at the next call. While a writer holds the vault's state
/// lock nothing kept is given: the roster for the log refuses the busy vault and the roster read
/// finds it busy, as each did before, so a busy writer never yields a permissive verdict.
///
/// NEGATIVE CONTROLS (2026-11-06): with the memos not quiet (`Memo::keyed(inputs, false)`), the
/// kept roster for the log is given under the writer (`a writer holding the vault meets the
/// read`); with `local_swarm_roster_for_log_now` keyed by no log's heads (`[0; 32]`), the grown
/// log gets the roster kept for the log before it (`the roster for another log is read for it`:
/// `deny_labels: ["m99"]` against `["m98"]`). (2026-11-08): with `Paths::records_data_file` back
/// on `records_storage::data_path`, the kept reads open the missing pin (`kept reads of unchanged
/// files`).
#[test]
fn a_peer_checks_reads_are_kept_until_a_file_moves_and_never_under_a_writer() {
    let _answering = ServersAnswering::set();
    let lab =
        log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    with_a_denied_node(&lab.m22, &lab.m7);
    let m7 = &lab.m7;
    let trust = trust_of(m7);
    let deadline = || std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let ((roster, acknowledged), read) = reads_during(m7, || {
        (
            swarm_roster_until(m7, deadline()),
            acknowledged_commons_log(m7, trust),
        )
    });
    let Ok(CommonsLog::Trusted(log, _)) = acknowledged else {
        panic!("m7's acknowledged log judges a Commons peer");
    };
    let (for_log, read_for_log) = reads_during(m7, || local_swarm_roster_for_log(m7, &log));
    assert!(denies(&roster, "m99") && denies(&for_log, "m99"));
    let inputs = vault_memo::inputs(m7).expect("the inputs");
    let unstamped: Vec<_> = read
        .keys()
        .chain(read_for_log.keys())
        .filter(|path| !inputs.contains(path))
        .collect();
    assert!(unstamped.is_empty(), "read but not stamped: {unstamped:#?}");

    let kept = || {
        (
            swarm_roster_now_until(m7, deadline()),
            acknowledged_commons_log_now(m7, trust).map(|log| log.trusted().map(|(_, d)| d)),
            local_swarm_roster_for_log_now(m7, &log),
        )
    };
    let first = kept();
    let (again, read) = reads_during(m7, kept);
    assert_eq!(again.0.as_ref().ok(), first.0.as_ref().ok());
    assert_eq!(again.1.as_ref().ok(), first.1.as_ref().ok());
    assert_eq!(again.2.as_ref().ok(), for_log.as_ref().ok());
    let read: Vec<_> = read.into_iter().collect();
    assert_eq!(read, vec![], "kept reads of unchanged files");

    {
        let _writer = test_hold_state_lock(m7).unwrap();
        assert!(
            local_swarm_roster_for_log_now(m7, &log).is_err(),
            "a writer holding the vault meets the read"
        );
        assert!(
            swarm_roster_now_until(m7, std::time::Instant::now()).is_err(),
            "a busy roster is not the kept one"
        );
    }

    // m22 lets m9 in and denies m98. Its grown log is another log than m7's, read for it though
    // m7's files have not moved; once m7 syncs, its files have, so its roster is read again.
    let m9 = Paths::for_home(&lab.root.path().join("m9"));
    let m9_rec = machine_identity(&m9).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m9", &m9_rec).unwrap();
        let roster = SwarmRoster {
            deny_labels: vec!["m98".to_owned()],
            ..Default::default()
        };
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish_approved(&lab.m22, &admin, &doc, std::slice::from_ref(&m9_rec)).unwrap();
    }
    let Ok(CommonsLog::Trusted(grown, _)) = acknowledged_commons_log(&lab.m22, trust) else {
        panic!("m22's grown log judges a Commons peer");
    };
    assert_ne!(
        grown.records().count(),
        log.records().count(),
        "the log grew"
    );
    let for_grown = local_swarm_roster_for_log(m7, &grown);
    assert_ne!(
        for_grown.as_ref().ok(),
        for_log.as_ref().ok(),
        "the grown log's roster is another"
    );
    assert_eq!(
        local_swarm_roster_for_log_now(m7, &grown).ok(),
        for_grown.ok(),
        "the roster for another log is read for it"
    );
    sync(m7, None).expect("m7 syncs");
    assert!(denies(&swarm_roster_now_until(m7, deadline()), "m98"));
    lab.server.stop();
}
