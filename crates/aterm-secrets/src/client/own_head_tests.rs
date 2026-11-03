// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An approval an admin made on the lower-seq side of a partition, which every server dropped at
//! the heal for the other side's higher version (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.2,
//! F14): the approving admin carries it back ([`super::own_head`]).

use super::log_entries_tests::{SplitAdmins, envelope_view, join_host, split_admins};
use super::*;

/// The Mac `mx` the lower-seq side let in.
struct Approved {
    mx: Paths,
    mx_id: Identity,
}

/// `mx` asks to join on `s1`, and `st1` approves it by its code there (seq n+1). Approval by code
/// signs no `Admit` (a join request has no Commons binding): the approval lives only in that
/// envelope. Meanwhile `st2` publishes three versions on `s2` (n+3), editing `other-key`.
fn approve_on_the_lower_side(lab: &SplitAdmins) -> Approved {
    let pin = config(&lab.st1).unwrap().unwrap().signer.unwrap();
    let mx = Paths::for_home(&lab.root.path().join("mx"));
    join_host(
        &mx,
        &format!("{}#{}", lab.url1, crate::b64::encode_padded(&pin)),
        "mx",
    );
    let mx_id = read_identity(&mx).unwrap().unwrap();
    approve_request(&lab.st1, &mx_id.recipient().to_bech32()).expect("st1 approves mx on s1");
    sync(&mx, None).expect("mx reads its approval");
    assert_eq!(load_state(&mx).unwrap().membership, Stored::In, "CONTROL");
    for step in [b"v1".as_slice(), b"v2", b"v3-from-higher-seq"] {
        let a2 = admin_open(&lab.st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.set_entry(&Audience::Everyone, "other-key", step)
            .unwrap();
        admin_publish(&lab.st2, &a2, &d2).unwrap();
    }
    Approved { mx, mx_id }
}

/// `st1` starts a swarm roster with no rows, and `s2` and `st2` take it before the split: an
/// approval then gives its Mac a row, which a carry must bring back with the Mac.
fn seed_roster(lab: &SplitAdmins) {
    let a1 = admin_open(&lab.st1).unwrap();
    let mut d1 = a1.doc.clone();
    d1.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        b"deny_labels = []\n",
    )
    .unwrap();
    admin_publish(&lab.st1, &a1, &d1).unwrap();
    let seeded = std::fs::read(lab.st1.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url2.clone()), &seeded).expect("s2 takes the roster");
    transport::post_records(&lab.url2, &load_records(&lab.st1).unwrap().encode()).unwrap();
    sync(&lab.st2, None).expect("st2 reads the roster");
}

/// `id`'s label in the swarm roster `doc` keeps, if any.
fn roster_label(doc: &Document, id: &Identity) -> Option<String> {
    crate::swarm_roster::SwarmRoster::from_document(doc)
        .unwrap()
        .and_then(|roster| {
            roster
                .member_by_recipient(&id.recipient().to_bech32())
                .map(|member| member.label.clone())
        })
}

/// `id`'s row in the swarm roster `doc` keeps, if any: its label, when it was admitted, and its
/// vault name.
fn roster_row(doc: &Document, id: &Identity) -> Option<(String, u64, String)> {
    crate::swarm_roster::SwarmRoster::from_document(doc)
        .unwrap()
        .and_then(|roster| {
            roster
                .member_by_recipient(&id.recipient().to_bech32())
                .map(|member| {
                    (
                        member.label.clone(),
                        member.admitted_ms,
                        member.vault_name.clone(),
                    )
                })
        })
}

/// Every server serves `id` under the vault name it was approved under, in `[machines]` and in its
/// roster row; and, where `label_free` (no row took the label and no deny names it since), under the
/// row `approved` itself (the label it was approved under, and when): the carry brought its row
/// back, not a fresh one assigned at the heal.
fn assert_row_carried(
    lab: &SplitAdmins,
    id: &Identity,
    approved: &(String, u64, String),
    label_free: bool,
) {
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let recipient = id.recipient().to_bech32();
    for head in read_server_heads(&lab.st1, &urls).unwrap() {
        let row = roster_row(&head.doc, id);
        assert_eq!(
            row.as_ref().map(|(_, _, name)| name),
            Some(&approved.2),
            "{} serves seq {} with the carried Mac's row under another name",
            head.url,
            head.envelope.seq
        );
        if label_free {
            assert_eq!(
                row.as_ref(),
                Some(approved),
                "{} serves seq {} with another row for the carried Mac",
                head.url,
                head.envelope.seq
            );
        }
        assert!(
            head.doc
                .machines()
                .iter()
                .any(|m| m.recipient == recipient && m.name == approved.2),
            "{} serves seq {} with the carried Mac under another name",
            head.url,
            head.envelope.seq
        );
    }
}

/// The heal: the servers trade records, and `s1` takes `s2`'s higher version, as a server's peer
/// pull does (versions have no parents, so a server takes any higher seq).
fn heal(lab: &SplitAdmins) {
    for (from, to) in [(&lab.st1, &lab.url2), (&lab.st2, &lab.url1)] {
        transport::post_records(to, &load_records(from).unwrap().encode()).expect("records");
    }
    let Fetched::Body(higher) = transport::fetch(
        &Source::Url(lab.url2.clone()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap() else {
        panic!("s2 serves its version");
    };
    transport::publish(&Source::Url(lab.url1.clone()), &higher).expect("s1 takes s2's version");
}

/// Whether `id` opens the lockers of the envelope `url` serves.
fn addressed(url: &str, id: &Identity) -> bool {
    let Fetched::Body(served) = transport::fetch(
        &Source::Url(url.to_owned()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap() else {
        panic!("expected a served envelope on {url}");
    };
    let parsed = envelope::parse(&served).unwrap();
    open_lockers(&parsed, id).unwrap().is_some()
}

/// Every server serves a version that lists `id` in `[machines]`, gives it a row where the version
/// keeps a swarm roster, and is sealed to it, and the admin's newest version keeps its admission
/// evidence.
fn assert_let_in(lab: &SplitAdmins, name: &str, id: &Identity) {
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let recipient = id.recipient().to_bech32();
    for head in read_server_heads(&lab.st1, &urls).unwrap() {
        assert!(
            head.doc.machines().iter().any(|m| m.recipient == recipient),
            "{} serves seq {} without {name} in [machines]",
            head.url,
            head.envelope.seq
        );
        if crate::swarm_roster::SwarmRoster::from_document(&head.doc)
            .unwrap()
            .is_some()
        {
            assert!(
                roster_label(&head.doc, id).is_some(),
                "{} serves seq {} with no roster row for {name}",
                head.url,
                head.envelope.seq
            );
        }
    }
    for url in &urls {
        assert!(addressed(url, id), "{url} seals no locker to {name}");
    }
    let admin = admin_open(&lab.st1).unwrap();
    assert!(
        admin.admissions.evidence(&id.recipient()).is_some(),
        "the newest version lost {name}'s admission"
    );
}

/// An approval made on the lower-seq side of a split survives every server taking the other
/// side's higher version at the heal: the approving admin's own pass lets the Mac in again.
///
/// NEGATIVE CONTROL: `own_head` read as none (no kept version): fails with "… serves seq 12
/// without mx in [machines]".
#[test]
fn an_approval_on_the_lower_seq_side_survives_every_server_taking_the_higher_seq() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    seed_roster(&lab);
    let Approved { mx, mx_id } = approve_on_the_lower_side(&lab);
    let approved = roster_row(&admin_open(&lab.st1).unwrap().doc, &mx_id)
        .expect("CONTROL: the approval gives mx a roster row");
    heal(&lab);

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert_let_in(&lab, "mx", &mx_id);
    assert_row_carried(&lab, &mx_id, &approved, true);
    super::log_entries_tests::assert_in_commons_control(&lab.st1, &urls, &[("mx", &mx_id)]);
    let _ = sync_with_candidates(&mx, None, &urls);
    assert_eq!(load_state(&mx).unwrap().membership, Stored::In);
    for url in &urls {
        let (_, view) = envelope_view(url, &mx_id);
        assert_eq!(
            view.get("other-key").map(Vec::as_slice),
            Some(b"v3-from-higher-seq".as_slice()),
            "{url} lost the higher side's edit"
        );
    }

    lab.s1.stop();
    lab.s2.stop();
}

/// What `paths`' log says of `id` for `[everyone]`: left out on purpose, and wrapped in every
/// newest epoch.
fn everyone_standing(paths: &Paths, id: &Identity) -> (bool, bool) {
    let store = load_records(paths).unwrap();
    let log = store.log().unwrap();
    let everyone = crate::membership_log::EpochAudience::Everyone;
    (
        log.audience_left_out_recipient(&everyone, &id.recipient()),
        log.wraps_maximal_audience_to(&everyone, &id.recipient()),
    )
}

/// After the heal `st2` reads both servers and publishes an edit to `s2`: its version names no
/// `mx`, so its draft mints an `[everyone]` epoch without `mx`, which the log reads as leaving `mx`
/// out on purpose.
fn publish_after_the_heal(lab: &SplitAdmins, urls: &[String]) {
    let _ = sync_with_candidates(&lab.st2, None, urls);
    let a2 = admin_open(&lab.st2).unwrap();
    let mut d2 = a2.doc.clone();
    d2.set_entry(&Audience::Everyone, "after-heal", b"from-st2")
        .unwrap();
    admin_publish(&lab.st2, &a2, &d2).expect("st2 publishes after the heal");
}

/// The approval comes back although the other side mirrored it away first: once `st2`'s own
/// publish after the heal minted an `[everyone]` epoch without `mx` (the log then leaves `mx` out
/// on purpose), `st1`'s pass still lets `mx` back in past that "left out" judgement, in the
/// envelope, the log and Commons control.
///
/// NEGATIVE CONTROL: the draft plan's `readmit` bypass dropped: "the newest version lost mx's
/// admission"; the overlay's Commons host bypass dropped: "mx lost Commons control in the envelope
/// … serves"; the overlay's removal bypass dropped: "… serves seq 16 without mx in
/// [machines]"; the roster carry in `carry_lost`, the merge of heads' bypass or the
/// half-done-revoke repair's bypass dropped: "… serves seq 15 with no roster row for mx".
#[test]
fn an_approval_lost_at_the_heal_comes_back_after_the_other_side_mirrored_it_away() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    seed_roster(&lab);
    let Approved { mx, mx_id } = approve_on_the_lower_side(&lab);
    let approved = roster_row(&admin_open(&lab.st1).unwrap().doc, &mx_id)
        .expect("CONTROL: the approval gives mx a roster row");
    heal(&lab);
    publish_after_the_heal(&lab, &urls);
    let _ = sync_with_candidates(&lab.st2, None, &urls);
    assert!(
        everyone_standing(&lab.st2, &mx_id).0,
        "CONTROL: st2's log leaves mx out of [everyone]"
    );

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert_let_in(&lab, "mx", &mx_id);
    // `st2`'s merge of heads denied mx's label when it pruned the row of a Mac its log left out
    // (still open): the carried Mac keeps its name under a fresh label.
    assert_row_carried(&lab, &mx_id, &approved, false);
    super::log_entries_tests::assert_in_commons_control(&lab.st1, &urls, &[("mx", &mx_id)]);
    let _ = sync_with_candidates(&mx, None, &urls);
    assert_eq!(load_state(&mx).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(mx.home.join(".secrets/other-key")).unwrap(),
        b"v3-from-higher-seq"
    );
    assert_eq!(
        std::fs::read(mx.home.join(".secrets/after-heal")).unwrap(),
        b"from-st2"
    );

    lab.s1.stop();
    lab.s2.stop();
}

/// The merge of heads prunes a roster row whose Mac the log left out of `[everyone]` (and denies
/// its label for good), but not the row of a Mac the mirror carries back: `mx` keeps the label it
/// was approved under. `st2`'s version after the heal reaches both servers, so no merge of
/// diverged heads on `st2` denied that label first.
///
/// NEGATIVE CONTROL: the bypass dropped in `reconcile_merged_metadata_with_log` or in
/// `repair_half_done_revokes_with_log`: fails with "left: None, right: Some(\"m23\")".
#[test]
fn the_merge_of_heads_keeps_the_roster_row_of_a_mac_carried_back() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    seed_roster(&lab);
    let Approved { mx_id, .. } = approve_on_the_lower_side(&lab);
    heal(&lab);
    publish_after_the_heal(&lab, &urls);
    let after = std::fs::read(lab.st2.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url1.clone()), &after).expect("s1 takes it");
    transport::post_records(&lab.url1, &load_records(&lab.st2).unwrap().encode()).unwrap();
    let _ = sync_records_with_candidates(&lab.st1, None, &urls);

    let config = config(&lab.st1).unwrap().unwrap();
    let identity = read_identity(&lab.st1).unwrap().unwrap();
    let own = super::own_head::own_head(&lab.st1, &config, &identity)
        .expect("CONTROL: st1 keeps its own version");
    let heads = read_server_heads(&lab.st1, &urls).unwrap();
    let (log, _) = super::log_entries::trusted_seeded_log(&lab.st1, &config, &heads[0].envelope)
        .unwrap()
        .expect("st1's log");
    let mx = mx_id.recipient();
    assert!(
        log.audience_left_out_recipient(&crate::membership_log::EpochAudience::Everyone, &mx),
        "CONTROL: st1's log leaves mx out of [everyone]"
    );
    let approved = roster_label(&own.doc, &mx_id).expect("CONTROL: the approval gave mx a row");
    let mut pruned = own.doc.clone();
    super::ledger_publish::reconcile_merged_metadata_with_log(
        &mut pruned,
        &heads,
        Some(&log),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(
        roster_label(&pruned, &mx_id),
        None,
        "CONTROL: the merge prunes the row of a Mac the log left out"
    );

    let mut kept = own.doc.clone();
    super::ledger_publish::reconcile_merged_metadata_with_log(
        &mut kept,
        &heads,
        Some(&log),
        &BTreeSet::from([mx]),
    )
    .unwrap();
    assert_eq!(roster_label(&kept, &mx_id), Some(approved));

    lab.s1.stop();
    lab.s2.stop();
}

/// The records heal first: `st1`'s pass over `s1` alone mirrors them into a version that keeps
/// `mx`. Then every server takes `s2`'s higher version, which lacks it. `st1`'s log has not moved
/// since, so no "log moved" mirror runs: the pass's own-version check does.
///
/// NEGATIVE CONTROL: the sync pass's own-version check never due: fails with "… serves seq 9
/// without mx in [machines]".
#[test]
fn a_higher_seq_version_that_arrives_after_its_records_still_gets_the_lower_seq_approval() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let Approved { mx, mx_id } = approve_on_the_lower_side(&lab);
    for (from, to) in [(&lab.st1, &lab.url2), (&lab.st2, &lab.url1)] {
        transport::post_records(to, &load_records(from).unwrap().encode()).expect("records");
    }
    let _ = sync(&lab.st1, None);
    let mirrored = read_server_heads(&lab.st1, std::slice::from_ref(&lab.url1)).unwrap();
    assert!(
        mirrored[0]
            .doc
            .machines()
            .iter()
            .any(|m| m.recipient == mx_id.recipient().to_bech32()),
        "CONTROL: st1's mirror over s1 keeps mx"
    );
    heal(&lab);

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert_let_in(&lab, "mx", &mx_id);
    let _ = sync_with_candidates(&mx, None, &urls);
    assert_eq!(load_state(&mx).unwrap().membership, Stored::In);

    lab.s1.stop();
    lab.s2.stop();
}

/// After the heal, `st1` publishes an unrelated edit made on the higher side's version (which
/// lacks `mx`) before any pass of its own mirrors: the version it keeps as its own is still the
/// one that carries `mx`, and its next pass lets `mx` back in.
///
/// NEGATIVE CONTROL: `keep` replacing the kept version unconditionally: fails with "… serves
/// seq 10 without mx in [machines]".
#[test]
fn an_admins_own_publish_after_the_heal_keeps_the_head_that_still_carries() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let Approved { mx, mx_id } = approve_on_the_lower_side(&lab);
    heal(&lab);

    sync_without_reconcile(&lab.st1, &urls, None).expect("st1 applies the higher version");
    {
        let a1 = admin_open(&lab.st1).unwrap();
        assert!(
            !a1.doc
                .machines()
                .iter()
                .any(|m| m.recipient == mx_id.recipient().to_bech32()),
            "CONTROL: st1 edits a version without mx"
        );
        let mut d1 = a1.doc.clone();
        d1.set_entry(&Audience::Everyone, "unrelated", b"edit")
            .unwrap();
        admin_publish(&lab.st1, &a1, &d1).expect("st1's unrelated edit");
    }
    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert_let_in(&lab, "mx", &mx_id);
    let _ = sync_with_candidates(&mx, None, &urls);
    assert_eq!(load_state(&mx).unwrap().membership, Stored::In);

    lab.s1.stop();
    lab.s2.stop();
}

/// No split: `st2` lets `mx` in, and `st1` revokes it in the very next version before any pass of
/// `st2`'s saw a version that kept it. `mx` holds no seat in the log (an approval by code signs no
/// `Admit`), so the revoke writes no `Remove` there: the served version's `code-only` line is all
/// that says it went. `st2`'s own version still lists `mx` with `st2`'s approval, but `st2` carries
/// nothing back and forgets its own version.
///
/// NEGATIVE CONTROL: `lost_to` ignoring `code-only` lines: fails with "st2 carried mx back".
#[test]
fn a_mac_another_admin_let_go_after_the_approval_is_not_carried_back() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let pin = config(&lab.st2).unwrap().unwrap().signer.unwrap();
    let mx = Paths::for_home(&lab.root.path().join("mx"));
    join_host(
        &mx,
        &format!("{}#{}", lab.url2, crate::b64::encode_padded(&pin)),
        "mx",
    );
    let mx_id = read_identity(&mx).unwrap().unwrap();
    let mx_rec = mx_id.recipient().to_bech32();
    approve_request(&lab.st2, &mx_rec).expect("st2 approves mx");
    let approved = std::fs::read(lab.st2.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url1.clone()), &approved).expect("s1 takes it");
    transport::post_records(&lab.url1, &load_records(&lab.st2).unwrap().encode()).unwrap();
    let _ = sync_with_candidates(&lab.st1, None, &urls);
    admin_revoke(&lab.st1, &admin_open(&lab.st1).unwrap(), "mx", false).expect("st1 revokes mx");
    assert!(
        admin_open(&lab.st1)
            .unwrap()
            .doc
            .code_only()
            .contains(&mx_rec),
        "CONTROL: mx is revoked onto a code-only line"
    );
    assert!(
        !load_records(&lab.st1)
            .unwrap()
            .log()
            .unwrap()
            .recipient_is_removed(&mx_id.recipient()),
        "CONTROL: the log holds no removal of mx"
    );
    let let_go = std::fs::read(lab.st1.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url2.clone()), &let_go).expect("s2 takes it");
    transport::post_records(&lab.url2, &load_records(&lab.st1).unwrap().encode()).unwrap();
    assert!(
        lab.st2.published_file().exists(),
        "CONTROL: st2 keeps its own version"
    );

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st2, None, &urls);
    }
    assert!(!listed(&lab.st2, &urls, &mx_id), "st2 carried mx back");
    for url in &urls {
        assert!(!addressed(url, &mx_id), "{url} seals a locker to mx again");
    }
    assert!(
        !lab.st2.published_file().exists(),
        "st2 still keeps an own version whose every approval a newer version decided"
    );

    lab.s1.stop();
    lab.s2.stop();
}

/// `my` waits on a `code-only` line on both sides before the split; during it, `st2` approves it
/// by its code (which clears that line) while `st1` approves `mx`. `st1`'s own version still holds
/// `my`'s `code-only` line, and the carry takes none of it: `my` stays listed, addressed and
/// unremoved, and `mx` is let in.
///
/// NEGATIVE CONTROL: the whole kept version merged as a head (the plan's first design): fails with
/// "… took my out".
#[test]
fn a_lower_seq_carry_keeps_a_mac_the_other_side_let_in_by_code() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let pin = config(&lab.st1).unwrap().unwrap().signer.unwrap();
    let my = Paths::for_home(&lab.root.path().join("my"));
    let my_id = machine_identity(&my).unwrap();
    let my_rec = my_id.recipient().to_bech32();
    {
        let a1 = admin_open(&lab.st1).unwrap();
        let mut d1 = a1.doc.clone();
        d1.add_code_only(&my_rec).unwrap();
        admin_publish(&lab.st1, &a1, &d1).expect("my waits on a code-only line");
    }
    let waiting = std::fs::read(lab.st1.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url2.clone()), &waiting).expect("s2 takes it");
    transport::post_records(&lab.url2, &load_records(&lab.st1).unwrap().encode()).unwrap();
    let _ = sync_with_candidates(&lab.st2, None, &urls);

    // --- PARTITION --- `my` asks on s2 and st2 approves it by code; st1 approves mx on s1.
    join_host(
        &my,
        &format!("{}#{}", lab.url2, crate::b64::encode_padded(&pin)),
        "my",
    );
    approve_request(&lab.st2, &my_rec).expect("st2 approves my by code");
    let Approved { mx, mx_id } = approve_on_the_lower_side(&lab);
    assert!(
        admin_open(&lab.st1)
            .unwrap()
            .doc
            .code_only()
            .contains(&my_rec),
        "CONTROL: st1's version still holds my's code-only line"
    );
    heal(&lab);

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert_let_in(&lab, "mx", &mx_id);
    for head in read_server_heads(&lab.st1, &urls).unwrap() {
        assert!(
            head.doc.machines().iter().any(|m| m.recipient == my_rec),
            "{} took my out",
            head.url
        );
    }
    for url in &urls {
        assert!(addressed(url, &my_id), "{url} seals no locker to my");
    }
    assert!(
        !load_records(&lab.st1)
            .unwrap()
            .log()
            .unwrap()
            .recipient_is_removed(&my_id.recipient()),
        "the log removed my"
    );
    let _ = sync_with_candidates(&mx, None, &urls);
    assert_eq!(load_state(&mx).unwrap().membership, Stored::In);

    lab.s1.stop();
    lab.s2.stop();
}

/// An entry in a host list neither admin is in (`[m8 m9]`) is deleted on the higher side during
/// the split. The carry brings back `mx` alone, not the lower side's copy of that entry: the
/// `0.145` `m9` no longer reads it.
///
/// NEGATIVE CONTROL: the whole kept version merged as a head: fails with "… serves seq 12 with
/// the entry the higher side deleted".
#[test]
fn a_carry_does_not_resurrect_a_host_list_entry_the_other_side_deleted() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let ring = Audience::Hosts(vec!["m8".to_owned(), "m9".to_owned()]);
    {
        let a1 = admin_open(&lab.st1).unwrap();
        let mut d1 = a1.doc.clone();
        d1.set_entry(&ring, "ring-key", b"r0").unwrap();
        admin_publish(&lab.st1, &a1, &d1).expect("st1 sets ring-key for m8 and m9");
    }
    let seeded = std::fs::read(lab.st1.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url2.clone()), &seeded).expect("s2 takes it");
    transport::post_records(&lab.url2, &load_records(&lab.st1).unwrap().encode()).unwrap();
    let _ = sync_with_candidates(&lab.st2, None, &urls);
    assert!(
        envelope_view(&lab.url2, &lab.m_0145_id)
            .1
            .contains_key("ring-key"),
        "CONTROL: m9 reads ring-key"
    );

    // --- PARTITION --- st2 deletes ring-key on s2; st1 approves mx on s1.
    {
        let a2 = admin_open(&lab.st2).unwrap();
        let mut d2 = a2.doc.clone();
        assert!(d2.unset_entry(&ring, "ring-key").unwrap());
        admin_publish(&lab.st2, &a2, &d2).expect("st2 deletes ring-key");
    }
    let Approved { mx_id, .. } = approve_on_the_lower_side(&lab);
    heal(&lab);

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert_let_in(&lab, "mx", &mx_id);
    for url in &urls {
        let (seq, view) = envelope_view(url, &lab.m_0145_id);
        assert!(
            !view.contains_key("ring-key"),
            "{url} serves seq {seq} with the entry the higher side deleted"
        );
    }

    lab.s1.stop();
    lab.s2.stop();
}

/// `st2` covers the newest `[everyone]` epochs each side minted during the split (F16's tender)
/// while `st1`'s carry mints one wrapped to `mx` from the same two: the next cover leaves `mx`
/// out. `st1`'s next pass still wraps the newest epoch to `mx`, which stays in.
///
/// NEGATIVE CONTROL: `lost_to`'s second case (a carried Mac left unwrapped) dropped: fails with
/// "st1's log leaves mx unwrapped in a newest [everyone] epoch".
#[test]
fn a_carried_mac_stays_wrapped_after_a_concurrent_covering_epoch() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let pin = config(&lab.st1).unwrap().unwrap().signer.unwrap();
    // --- PARTITION --- st1 approves mx, then deletes `split-key` (a fresh [everyone] epoch wrapped
    // to mx); st2 deletes `other-key` (a fresh one without mx).
    let mx = Paths::for_home(&lab.root.path().join("mx"));
    join_host(
        &mx,
        &format!("{}#{}", lab.url1, crate::b64::encode_padded(&pin)),
        "mx",
    );
    let mx_id = read_identity(&mx).unwrap().unwrap();
    approve_request(&lab.st1, &mx_id.recipient().to_bech32()).expect("st1 approves mx");
    for (admin, key) in [(&lab.st1, "split-key"), (&lab.st2, "other-key")] {
        let a = admin_open(admin).unwrap();
        let mut d = a.doc.clone();
        assert!(d.unset_entry(&Audience::Everyone, key).unwrap());
        admin_publish(admin, &a, &d).expect("a delete mints a fresh [everyone] epoch");
    }
    for step in [b"h1".as_slice(), b"h2"] {
        let a2 = admin_open(&lab.st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.set_entry(&Audience::Everyone, "h-key", step).unwrap();
        admin_publish(&lab.st2, &a2, &d2).unwrap();
    }
    heal(&lab);

    // st2's tender covers both sides' epochs, before it holds st1's carry: its cover leaves mx
    // out, and stays on st2 for now.
    let tend = |admin: &Paths| {
        let _ = sync_records_with_candidates(admin, None, &urls);
        let now = aterm_time::unix_millis().unwrap_or(0);
        let mut signed = remove::tend_epochs_at(admin, now).expect("the tender looks");
        signed.extend(
            remove::tend_epochs_at(admin, now + 1_000 * log_entries::GAP_STEP_MS)
                .expect("the tender signs"),
        );
        signed
    };
    assert!(
        !tend(&lab.st2).is_empty(),
        "CONTROL: st2 covers the split's epochs"
    );
    let _ = sync_with_candidates(&lab.st1, None, &urls);
    assert!(
        everyone_standing(&lab.st1, &mx_id).1,
        "CONTROL: st1's carry wraps mx in its newest epoch"
    );
    for url in &urls {
        transport::post_records(url, &load_records(&lab.st2).unwrap().encode()).unwrap();
    }
    assert!(!tend(&lab.st2).is_empty(), "CONTROL: st2 covers again");
    assert!(
        !everyone_standing(&lab.st2, &mx_id).1,
        "CONTROL: st2's newest cover leaves mx unwrapped"
    );
    for url in &urls {
        transport::post_records(url, &load_records(&lab.st2).unwrap().encode()).unwrap();
    }

    let _ = sync_with_candidates(&lab.st1, None, &urls);
    assert!(
        everyone_standing(&lab.st1, &mx_id).1,
        "st1's log leaves mx unwrapped in a newest [everyone] epoch"
    );
    assert_let_in(&lab, "mx", &mx_id);
    let _ = sync_with_candidates(&mx, None, &urls);
    assert_eq!(load_state(&mx).unwrap().membership, Stored::In);

    lab.s1.stop();
    lab.s2.stop();
}

/// Publish `admin`'s current version with `edit` as an admin before 0.145 did: sealed straight to
/// every one of `urls`, with no `code-only` line for a Mac it let go and nothing in the log.
fn publish_as_an_older_admin(admin: &Paths, urls: &[String], edit: impl FnOnce(&mut Document)) {
    let opened = admin_open(admin).unwrap();
    let mut doc = opened.doc.clone();
    edit(&mut doc);
    let admissions = opened.admissions.retained(&doc).unwrap();
    let bytes = seal_stamped(
        &opened.config.name,
        opened.seq + 1,
        &opened.seed,
        &opened.commons_seed,
        opened.commons_bound,
        &doc,
        &opened.handoff,
        &admissions,
        None,
    )
    .unwrap();
    for url in urls {
        transport::publish(&Source::Url(url.clone()), &bytes).expect("an older admin's version");
    }
}

/// Whether any server among `urls` lists `id` in `[machines]`, read as `admin`.
fn listed(admin: &Paths, urls: &[String], id: &Identity) -> bool {
    let recipient = id.recipient().to_bech32();
    read_server_heads(admin, urls)
        .unwrap()
        .iter()
        .any(|head| head.doc.machines().iter().any(|m| m.recipient == recipient))
}

/// `st2` lets `mo` in, and `st1`'s own version lists it with that evidence. An admin before 0.145
/// then lets `mo` go with no `code-only` line and no removal in the log: `st1`'s own version names
/// `mo`, but the approval was not `st1`'s, so `st1` carries nothing back.
///
/// NEGATIVE CONTROL: the author filter dropped: fails with "st1 carried mo back".
#[test]
fn a_mac_an_older_admin_let_go_without_a_code_only_line_is_not_carried() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let mo_id = machine_identity(&Paths::for_home(&lab.root.path().join("mo"))).unwrap();
    let mo = mo_id.recipient().to_bech32();
    {
        let a2 = admin_open(&lab.st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.add_machine("mo", &mo).unwrap();
        admin_publish_approved(&lab.st2, &a2, &d2, std::slice::from_ref(&mo))
            .expect("st2 approves mo");
    }
    let approved = std::fs::read(lab.st2.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url1.clone()), &approved).expect("s1 takes it");
    transport::post_records(&lab.url1, &load_records(&lab.st2).unwrap().encode()).unwrap();
    let _ = sync_with_candidates(&lab.st1, None, &urls);
    {
        let a1 = admin_open(&lab.st1).unwrap();
        let mut d1 = a1.doc.clone();
        d1.set_entry(&Audience::Everyone, "st1-edit", b"x").unwrap();
        admin_publish(&lab.st1, &a1, &d1).expect("st1's own version lists mo");
    }
    let own = std::fs::read(lab.st1.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url2.clone()), &own).expect("s2 takes it");
    transport::post_records(&lab.url2, &load_records(&lab.st1).unwrap().encode()).unwrap();
    let _ = sync_with_candidates(&lab.st2, None, &urls);
    assert!(listed(&lab.st1, &urls, &mo_id), "CONTROL: mo is in");

    publish_as_an_older_admin(&lab.st2, &urls, |doc| {
        assert!(doc.remove_machine("mo", true).unwrap());
    });
    assert!(!listed(&lab.st1, &urls, &mo_id), "CONTROL: mo is let go");

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert!(!listed(&lab.st1, &urls, &mo_id), "st1 carried mo back");
    for url in &urls {
        assert!(!addressed(url, &mo_id), "{url} seals a locker to mo again");
    }

    lab.s1.stop();
    lab.s2.stop();
}

/// No split: `st1` lets `mx` in, and `st2`'s newer version keeps it. That alone does not settle
/// `st1`'s own version (another admin on its side of a split could have sealed it, and lost it at
/// the heal); once `mx`'s `Admit` reaches the log it holds a seat, `st1`'s own version has nothing
/// left to carry and is forgotten, so when `mx` is later let go with no `code-only` line (an admin
/// before 0.145), `st1` does not carry it back.
///
/// NEGATIVE CONTROL: a seat not settling a Mac (`log_member` dropped from `settled`): fails with
/// "st1 still keeps an own version every Mac of which holds a seat".
#[test]
fn an_own_version_whose_macs_hold_seats_carries_nothing_after() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let pin = config(&lab.st1).unwrap().unwrap().signer.unwrap();
    let mx = Paths::for_home(&lab.root.path().join("mx"));
    join_host(
        &mx,
        &format!("{}#{}", lab.url1, crate::b64::encode_padded(&pin)),
        "mx",
    );
    let mx_id = read_identity(&mx).unwrap().unwrap();
    approve_request(&lab.st1, &mx_id.recipient().to_bech32()).expect("st1 approves mx");
    let approved = std::fs::read(lab.st1.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url2.clone()), &approved).expect("s2 takes it");
    transport::post_records(&lab.url2, &load_records(&lab.st1).unwrap().encode()).unwrap();
    let _ = sync_with_candidates(&lab.st2, None, &urls);
    {
        let a2 = admin_open(&lab.st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.set_entry(&Audience::Everyone, "st2-edit", b"x").unwrap();
        admin_publish(&lab.st2, &a2, &d2).expect("st2's version keeps mx");
    }
    let newer = std::fs::read(lab.st2.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url1.clone()), &newer).expect("s1 takes it");
    transport::post_records(&lab.url1, &load_records(&lab.st2).unwrap().encode()).unwrap();
    let _ = sync_with_candidates(&lab.st1, None, &urls);
    assert!(
        lab.st1.published_file().exists(),
        "CONTROL: a newer version that lists mx does not settle st1's own version"
    );

    let trust = crate::commons::bootstrap::Bootstrap::required(&lab.st1)
        .unwrap()
        .trust();
    let request = crate::commons::bootstrap::Device::open(&mx)
        .unwrap()
        .expect("mx's device")
        .request(trust.network, trust.authority)
        .unwrap();
    super::log_entries::admit_member_in_log(
        &lab.st1,
        crate::membership_log::Entrant {
            request,
            node_key: [0x3c; 32],
            name: "mx".to_owned(),
            label: "m40".to_owned(),
        },
        crate::membership_log::Evidence::Phrase,
    )
    .expect("st1 seats mx in the log");
    let _ = sync_with_candidates(&lab.st1, None, &urls);
    assert!(
        !lab.st1.published_file().exists(),
        "st1 still keeps an own version every Mac of which holds a seat"
    );

    let _ = sync_with_candidates(&lab.st2, None, &urls);
    publish_as_an_older_admin(&lab.st2, &urls, |doc| {
        assert!(doc.remove_machine("mx", true).unwrap());
    });
    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert!(!listed(&lab.st1, &urls, &mx_id), "st1 carried mx back");

    lab.s1.stop();
    lab.s2.stop();
}

/// The higher side's last version reached the servers but its records never left `st2` (the
/// publish ended after the servers answered), so `st1`'s carry waits on them and publishes nothing.
/// The next look at the same three digests (its own version, the served one, its records) does not
/// run the mirror again.
///
/// NEGATIVE CONTROL: the attempt mark never matching: fails with "the same carry is due again with
/// nothing changed".
#[test]
fn a_carry_that_cannot_complete_is_not_retried_unchanged() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let pin = config(&lab.st1).unwrap().unwrap().signer.unwrap();
    let mx = Paths::for_home(&lab.root.path().join("mx"));
    join_host(
        &mx,
        &format!("{}#{}", lab.url1, crate::b64::encode_padded(&pin)),
        "mx",
    );
    let mx_id = read_identity(&mx).unwrap().unwrap();
    approve_request(&lab.st1, &mx_id.recipient().to_bech32()).expect("st1 approves mx");
    for step in [b"v1".as_slice(), b"v2", b"v3"] {
        let a2 = admin_open(&lab.st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.set_entry(&Audience::Everyone, "other-key", step)
            .unwrap();
        if step == b"v3" {
            CUT_AFTER_STORE.with(|cut| cut.set(true));
            assert!(
                admin_publish(&lab.st2, &a2, &d2).is_err(),
                "CONTROL: the cut"
            );
        } else {
            admin_publish(&lab.st2, &a2, &d2).unwrap();
        }
    }
    heal(&lab);

    let config = config(&lab.st1).unwrap().unwrap();
    let look = || {
        let Fetched::Body(served) = transport::fetch(
            &Source::Url(lab.url1.clone()),
            None,
            aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap() else {
            panic!("s1 serves a version");
        };
        let records =
            log_entries::current_records_digest(&lab.st1, &config, &load_state(&lab.st1).unwrap());
        own_head::due(&lab.st1, &config, &served, records.as_ref())
    };
    assert!(look(), "CONTROL: st1's own version carries mx");
    let _ = sync_with_candidates(&lab.st1, None, &urls);
    assert!(
        !listed(&lab.st1, &urls, &mx_id),
        "CONTROL: the carry waits on records no server has"
    );
    assert!(!look(), "the same carry is due again with nothing changed");

    lab.s1.stop();
    lab.s2.stop();
}

/// `st1`'s own version names a Mac `mz` the servers lost, but its roster does not parse, so the
/// carry fails: the pass still merges the diverged servers' heads (`mx` on `s1`, the higher side's
/// edit on `s2`) and applies the merge.
///
/// NEGATIVE CONTROL: the fallback taken only after a `PUT`: fails with "the carry's error failed
/// the pass: Err(Error(\"aterm-swarm-roster row 1: missing or empty `recipient`\", Other))".
#[test]
fn a_merge_error_from_the_own_head_falls_back_to_the_served_merge() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let Approved { mx_id, .. } = approve_on_the_lower_side(&lab);
    for (from, to) in [(&lab.st1, &lab.url2), (&lab.st2, &lab.url1)] {
        transport::post_records(to, &load_records(from).unwrap().encode()).expect("records");
    }
    {
        let opened = admin_open(&lab.st1).unwrap();
        let mut doc = opened.doc.clone();
        let mz = machine_identity(&Paths::for_home(&lab.root.path().join("mz")))
            .unwrap()
            .recipient()
            .to_bech32();
        doc.add_machine("mz", &mz).unwrap();
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            b"[[member]]\nrecipient = 7\n",
        )
        .unwrap();
        let mut admissions = opened.admissions.retained(&doc).unwrap();
        let me = read_identity(&lab.st1).unwrap().unwrap().recipient();
        admissions
            .record(
                &doc,
                &mz,
                admission::Evidence {
                    method: AdmissionMethod::Manual,
                    sequence: opened.seq,
                    authored_ms: 1,
                    author: me,
                },
            )
            .unwrap();
        let bytes = seal_stamped(
            &opened.config.name,
            opened.seq,
            &opened.seed,
            &opened.commons_seed,
            opened.commons_bound,
            &doc,
            &opened.handoff,
            &admissions,
            None,
        )
        .unwrap();
        fsx::write_atomic(&lab.st1.published_file(), &bytes, 0o600).unwrap();
    }

    let pass = sync_with_candidates(&lab.st1, None, &urls);
    assert!(pass.is_ok(), "the carry's error failed the pass: {pass:?}");
    let heads = read_server_heads(&lab.st1, &urls).unwrap();
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0].envelope.digest(), heads[1].envelope.digest());
    assert!(listed(&lab.st1, &urls, &mx_id), "the merge lost mx");
    let (_, view) = envelope_view(&lab.url1, &lab.m_0145_id);
    assert_eq!(
        view.get("other-key").map(Vec::as_slice),
        Some(b"v3-from-higher-seq".as_slice())
    );

    lab.s1.stop();
    lab.s2.stop();
}

/// `st1` makes `m8`, a member that reads `s1`, an admin before the split, and `s2` and `st2` take
/// it: `m8` is a second admin on what becomes the lower-seq side.
fn a_second_admin_on_the_lower_side(lab: &SplitAdmins) {
    let a1 = admin_open(&lab.st1).unwrap();
    let mut d1 = a1.doc.clone();
    d1.add_admin("m8").unwrap();
    admin_publish_admin(&lab.st1, &a1, &d1, "m8").unwrap();
    let promoted = std::fs::read(lab.st1.cache_file()).unwrap();
    transport::publish(&Source::Url(lab.url2.clone()), &promoted).expect("s2 takes it");
    transport::post_records(&lab.url2, &load_records(&lab.st1).unwrap().encode()).unwrap();
    sync(&lab.st2, None).expect("st2 reads it");
    sync(&lab.m_v2, None).expect("m8 reads its admin locker");
}

/// `m8`, the lower side's second admin, reads `s1` and publishes an edit there: its version lists
/// `mx`, with `st1`'s admission evidence, at a seq above `st1`'s approval.
fn second_admin_publishes_on_the_lower_side(lab: &SplitAdmins) {
    sync(&lab.m_v2, None).expect("m8 reads s1");
    let a3 = admin_open(&lab.m_v2).unwrap();
    let mut d3 = a3.doc.clone();
    d3.set_entry(&Audience::Everyone, "lower-key", b"from-m8")
        .unwrap();
    admin_publish(&lab.m_v2, &a3, &d3).expect("m8 publishes on s1");
}

/// Two admins on the lower-seq side: `st1` approves `mx`, then `m8` publishes there, and its
/// version lists `mx`. `st1`'s next pass keeps its own version: `m8`'s loses to the other side at
/// the heal as `st1`'s does, and `m8` carries no approval it did not make. At the heal every
/// server takes the higher side's version, and `st1` lets `mx` back in.
///
/// NEGATIVE CONTROL: the earlier forgetting rule (a newer version listing every Mac the kept one
/// let in absorbs it): fails with "st1 forgot its own version once m8's version listed mx".
#[test]
fn a_second_admin_publishing_on_the_lower_side_leaves_the_approval_carried() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    a_second_admin_on_the_lower_side(&lab);
    let Approved { mx, mx_id } = approve_on_the_lower_side(&lab);
    second_admin_publishes_on_the_lower_side(&lab);
    assert!(
        listed(&lab.st1, std::slice::from_ref(&lab.url1), &mx_id),
        "CONTROL: m8's version on s1 lists mx"
    );
    let _ = sync(&lab.st1, None);
    assert!(
        lab.st1.published_file().exists(),
        "st1 forgot its own version once m8's version listed mx"
    );
    heal(&lab);

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert_let_in(&lab, "mx", &mx_id);
    let _ = sync_with_candidates(&mx, None, &urls);
    assert_eq!(load_state(&mx).unwrap().membership, Stored::In);

    lab.s1.stop();
    lab.s2.stop();
}

/// The records heal while `s1` still serves `m8`'s lagging version (it lists `mx`) and `s2` the
/// higher side's, which `st2` published after reading `st1`'s records, so its log leaves `mx` out
/// of `[everyone]`. `st1`'s pass over both servers judges the loss against the highest version, not
/// the union of every version at its seq or newer, and its mirror keeps `mx`.
///
/// NEGATIVE CONTROL: judging against every head at the kept version's seq or newer (the union,
/// `m8`'s version included): fails with "… serves seq 13 without mx in [machines]".
#[test]
fn a_lagging_lower_side_version_beside_the_higher_one_still_counts_the_approval_lost() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    a_second_admin_on_the_lower_side(&lab);
    let Approved { mx_id, .. } = approve_on_the_lower_side(&lab);
    second_admin_publishes_on_the_lower_side(&lab);
    transport::post_records(&lab.url2, &load_records(&lab.m_v2).unwrap().encode())
        .expect("s2 takes the lower side's records");
    let _ = sync(&lab.st2, None);
    let a2 = admin_open(&lab.st2).unwrap();
    let mut d2 = a2.doc.clone();
    d2.set_entry(&Audience::Everyone, "after-heal", b"from-st2")
        .unwrap();
    admin_publish(&lab.st2, &a2, &d2).expect("st2 publishes on s2");
    assert!(
        everyone_standing(&lab.st2, &mx_id).0,
        "CONTROL: st2's log leaves mx out of [everyone]"
    );
    transport::post_records(&lab.url1, &load_records(&lab.st2).unwrap().encode())
        .expect("s1 takes the higher side's records");
    let heads = read_server_heads(&lab.st1, &urls).unwrap();
    assert!(
        heads[0].envelope.seq < heads[1].envelope.seq && listed(&lab.st1, &urls[..1], &mx_id),
        "CONTROL: s1 serves m8's lagging version, which lists mx"
    );

    let _ = sync_with_candidates(&lab.st1, None, &urls);
    assert_let_in(&lab, "mx", &mx_id);

    lab.s1.stop();
    lab.s2.stop();
}

/// The higher side rewrote the swarm roster during the split (a deny list edit), so the log's
/// roster after the heal is that side's, with no row for `mx`, and no merge there denied `mx`'s
/// label. The carry brings `mx`'s own row back: the label it was approved under, when, and its
/// name, not a fresh row assigned at the heal.
///
/// NEGATIVE CONTROL: no roster carry after the overlay (as before this test): fails with "… serves
/// seq 13 with another row for the carried Mac: left: Some((\"m100\", …, \"mx\")), right:
/// Some((\"m23\", …, \"mx\"))"; a label never counted free in `carry_roster_row` (a fresh row from
/// `assign_newcomer`): fails the same way.
#[test]
fn a_carried_mac_keeps_its_roster_row_when_the_other_side_rewrote_the_roster() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    seed_roster(&lab);
    let Approved { mx_id, .. } = approve_on_the_lower_side(&lab);
    let approved = roster_row(&admin_open(&lab.st1).unwrap().doc, &mx_id)
        .expect("CONTROL: the approval gives mx a roster row");
    {
        let a2 = admin_open(&lab.st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            b"deny_labels = [\"m99\"]\n",
        )
        .unwrap();
        admin_publish(&lab.st2, &a2, &d2).expect("st2 rewrites the roster on s2");
    }
    heal(&lab);

    for _ in 0..2 {
        let _ = sync_with_candidates(&lab.st1, None, &urls);
    }
    assert_let_in(&lab, "mx", &mx_id);
    assert_row_carried(&lab, &mx_id, &approved, true);

    lab.s1.stop();
    lab.s2.stop();
}
