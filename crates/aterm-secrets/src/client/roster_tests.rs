// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_log_read_before_an_authority_change_cannot_supply_the_new_roster() {
    let macs = super::super::log_entries_tests::two_macs(&[]);
    let paths = &macs.m22;
    let store = load_records(paths).unwrap();
    let log = store.log().unwrap();
    assert!(
        log_entries::local_swarm_roster_for_log(paths, log)
            .unwrap()
            .is_some()
    );
    {
        let lock = StateLock::take(paths).unwrap();
        let mut next = config(paths).unwrap().unwrap();
        next.signer = Some([77; 32]);
        next.followed.clear();
        let mut state = load_state(paths).unwrap();
        state.source_identity = Some(source_identity(&next));
        save_state(paths, &state).unwrap();
        save_config(paths, &next).unwrap();
        assert!(log_entries::local_swarm_roster_for_log_held(paths, log, &lock).is_err());
    }
    assert!(log_entries::local_swarm_roster_for_log(paths, log).is_err());
    macs.server.stop();
}

#[test]
fn an_unreadable_membership_log_cannot_admit_a_cached_admin() {
    struct FailedReads(PathBuf);
    impl Drop for FailedReads {
        fn drop(&mut self) {
            fsx::fail_reads(&self.0, 0);
        }
    }
    let macs = super::super::log_entries_tests::two_macs(&[]);
    let paths = &macs.m22;
    let admin = admin_open(paths).unwrap();
    let identity = read_identity(paths).unwrap().unwrap();
    let bytes = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
        .unwrap()
        .unwrap();
    let envelope = envelope::parse(bytes.as_slice()).unwrap();
    assert!(
        log_entries::trusted_seeded_log(paths, &admin.config, &envelope)
            .unwrap()
            .is_some()
    );
    let refused = || {
        super::super::admin_removed_in_log(paths, &admin.config, &envelope, &identity, &admin.doc)
    };
    assert!(!refused());
    let fault = FailedReads(paths.records_file());
    fsx::fail_reads(&fault.0, usize::MAX);
    assert!(refused());
    drop(fault);
    assert!(!refused());
    macs.server.stop();
}

/// A roster read holds the state lock from before its one `Reads` to after it, so no writer
/// moves the source under it, and what it reads carries the log's removal over an older entry.
/// No source moves here: the lock excludes that, and the reader the hold is watched on is
/// [`with_roster_observation`]. Until 2026-11-05 this was
/// `a_source_move_during_a_roster_read_cannot_lose_log_removals`, whose firing with a writer
/// planted at each cut `docs/measured/authorization-read-integration-2026-11-03.md` records.
///
/// NEGATIVE CONTROLS (2026-11-05): with the `Held` arm of [`with_roster_observation`] releasing
/// its lock, or with `file.try_lock_shared()` gone from `StateLock::classify`, it fails at
/// "a writer took the state lock under a roster read".
#[test]
fn a_roster_read_keeps_a_writer_out_and_its_overlay_keeps_a_log_removal() {
    let macs = super::super::log_entries_tests::two_macs(&[]);
    let paths = &macs.m22;
    let before = admin_seed_swarm_roster(paths, &[]).unwrap();
    assert!(before.member_by_label_or_alias("m7").is_some());
    super::super::remove::test_remove_unposted(paths, "m7").unwrap();
    // Keep the earlier roster entry: its removed row is excluded by the log overlay,
    // independently of when a writer next refreshes that entry.
    super::super::remove::test_put_everyone(paths, ROSTER_ENTRY, before.to_toml().as_bytes())
        .unwrap();
    let safe = |roster: Option<&SwarmRoster>| {
        roster.is_none_or(|roster| {
            roster.is_label_denied("m7")
                && roster
                    .member_by_label_or_alias("m7")
                    .is_none_or(|member| roster.is_member_denied(member))
        })
    };
    let raw = base_swarm_roster(paths).unwrap().unwrap();
    assert!(raw.member_by_label_or_alias("m7").is_some());
    assert!(!raw.is_label_denied("m7"));
    assert!(safe(Some(
        &swarm_roster_until(
            paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap()
        .unwrap()
    )));

    let read = with_roster_observation(paths, |observation| {
        let lock = observation
            .state_lock()
            .expect("a vault's read holds its lock");
        assert!(
            StateLock::try_take(paths).unwrap().is_none(),
            "a writer took the state lock under a roster read"
        );
        swarm_roster_held(paths, lock)
    });
    assert!(safe(read.unwrap().unwrap().as_ref()));
    macs.server.stop();
}

#[test]
fn overlay_keeps_a_members_denial_when_its_log_node_key_replaces_the_roster_key() {
    use crate::commons::binding::Request;
    use crate::membership_log::{Entrant, Founder, Log, Record, Root};

    let entrant = |tag, label: &str| {
        let identity = Identity::from_secret_bytes([tag; 32]);
        let request = Request::create(
            [2; 16], [3; 32], [tag; 16], [tag; 16], &identity, &[tag; 32],
        )
        .unwrap();
        Entrant {
            request,
            node_key: [tag; 32],
            name: label.to_owned(),
            label: label.to_owned(),
        }
    };
    for unproved in [false, true] {
        let first = entrant(1, "m1");
        let second = entrant(2, "m2");
        let root = Root::of(first.request.binding());
        let recipient = second.request.binding().recipient().to_bech32();
        let mut second = Founder::from(second);
        if unproved {
            second.binding = None;
        }
        let log = Log::new(
            Record::genesis(&[99; 32], root, vec![Founder::from(first), second], 1).unwrap(),
        )
        .unwrap();
        let old_key = "03".repeat(32);
        let new_key = "02".repeat(32);
        let node_id = "44".repeat(16);
        for denial in 0..4 {
            let mut roster = SwarmRoster {
                members: vec![crate::swarm_roster::SwarmMember {
                    recipient: recipient.clone(),
                    vault_name: "m2".to_owned(),
                    label: "m2".to_owned(),
                    aliases: vec!["m20".to_owned()],
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1,
                    claimed_label: None,
                    claimed_at_ms: None,
                    node_id: Some(node_id.clone()),
                    node_key: Some(old_key.clone()),
                }],
                deny_labels: if denial == 1 {
                    vec!["m20".to_owned()]
                } else {
                    Vec::new()
                },
                deny_nodes: match denial {
                    2 => vec![node_id.clone()],
                    3 => vec![old_key.clone()],
                    _ => Vec::new(),
                },
            };
            if denial == 3 {
                let mut lost = roster.clone();
                lost.members[0].node_key = Some(new_key.clone());
                assert!(
                    !steward::member_is_denied_in_roster(&lost, &lost.members[0]),
                    "negative control: blindly replacing the key loses its denial"
                );
            }
            overlay_log_membership(&log, None, &mut roster);
            assert_eq!(
                roster.is_node_denied(&new_key),
                denial != 0,
                "unproved={unproved}, denial={denial}"
            );
            assert_eq!(
                steward::member_is_denied_in_roster(
                    &roster,
                    roster.member_by_recipient(&recipient).unwrap(),
                ),
                denial != 0
            );
            let denied = roster.deny_nodes.clone();
            overlay_log_membership(&log, None, &mut roster);
            assert_eq!(
                roster.deny_nodes, denied,
                "replaying the overlay preserves the denied keys"
            );
        }
    }
}

#[test]
fn overlay_log_membership_moves_log_removed_members_to_deny_labels_and_deny_nodes() {
    use crate::commons::binding::Request;
    use crate::membership_log::{Body, Entrant, Evidence, Log, Record};

    let mk_entrant = |tag: u8, label: &str| {
        let id = Identity::from_secret_bytes([tag ^ 0x55; 32]);
        let req = Request::create([2; 16], [3; 32], [tag; 16], [tag; 16], &id, &[tag; 32])
            .expect("request");
        (
            id,
            Entrant {
                request: req,
                node_key: [tag; 32],
                name: format!("{label}.local"),
                label: label.to_owned(),
            },
        )
    };

    let (m1_id, m1_ent) = mk_entrant(1, "m1");
    let (m2_id, m2_ent) = mk_entrant(2, "m2");
    let (m7_id, m7_ent) = mk_entrant(7, "m7");
    let m7_rebound_req = Request::create([2; 16], [3; 32], [17; 16], [17; 16], &m7_id, &[17; 32])
        .expect("m7 rebound request");
    let m7_rebound_ent = Entrant {
        request: m7_rebound_req,
        node_key: [17; 32],
        name: "m7.local".to_owned(),
        label: "m7".to_owned(),
    };
    let m1_actor = m1_ent.request.binding().actor();
    let m2_actor = m2_ent.request.binding().actor();
    let m7_actor = m7_rebound_ent.request.binding().actor();

    let genesis = Record::found(&[99; 32], vec![m1_ent, m2_ent], 1_000).expect("genesis");
    let genesis_id = *genesis.id();
    let mut log = Log::new(genesis).expect("log");

    let admit_m7_old = log
        .draft(
            &m1_actor,
            2_000,
            Body::Admit {
                entrant: m7_ent,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign admit old m7");
    log.insert(admit_m7_old).expect("insert admit old m7");

    let admit_m7 = log
        .draft(
            &m1_actor,
            2_500,
            Body::Admit {
                entrant: m7_rebound_ent,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign admit");
    let admit_m7_id = *admit_m7.id();
    log.insert(admit_m7).expect("insert admit");

    let remove_m2 = log
        .draft(
            &m1_actor,
            3_000,
            Body::Remove {
                member: m2_actor,
                cutoff: genesis_id,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign remove m2");
    log.insert(remove_m2).expect("insert remove m2");

    let remove_m7 = log
        .draft(
            &m1_actor,
            4_000,
            Body::Remove {
                member: m7_actor,
                cutoff: admit_m7_id,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign remove m7");
    log.insert(remove_m7).expect("insert remove m7");
    assert!(log.recipient_is_removed(&m7_id.recipient()));

    // Removed m7 attempts to self-Admit (claiming another recipient and label "m41") and
    // rename itself to "m43.local"; neither un-removes m7's recipient nor poisons m41/m43 in
    // `roster.deny_labels`.
    let (_m41_id, mut m41_ent) = mk_entrant(41, "m41");
    let self_forged_req = Request::create(
        [2; 16],
        [3; 32],
        [17; 16],
        [17; 16],
        &Identity::from_secret_bytes([0x99; 32]),
        &[17; 32],
    )
    .expect("forged self request");
    m41_ent.request = self_forged_req;
    let bogus_self_admit = log
        .draft(
            &m7_actor,
            4_500,
            Body::Admit {
                entrant: m41_ent,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&[17; 32], [17; 16])
        .expect("sign bogus self admit");
    log.insert(bogus_self_admit)
        .expect("insert bogus self admit");
    let bogus_name = log
        .draft(
            &m7_actor,
            4_600,
            Body::Name {
                name: "m43.local".to_owned(),
            },
        )
        .sign(&[17; 32], [17; 16])
        .expect("sign bogus name");
    log.insert(bogus_name).expect("insert bogus name");

    let mut roster = SwarmRoster {
        members: vec![
            crate::swarm_roster::SwarmMember {
                recipient: m1_id.recipient().to_bech32(),
                vault_name: "m1".to_owned(),
                label: "m1".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: Some("11111111111111111111111111111111".to_owned()),
                node_key: None,
            },
            crate::swarm_roster::SwarmMember {
                recipient: m2_id.recipient().to_bech32(),
                vault_name: "m2".to_owned(),
                label: "m2".to_owned(),
                aliases: vec!["m9".to_owned()],
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: Some("m10".to_owned()),
                claimed_at_ms: Some(1_000),
                node_id: Some("22222222222222222222222222222222".to_owned()),
                node_key: None,
            },
        ],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    overlay_log_membership(&log, None, &mut roster);

    assert_eq!(roster.members.len(), 1);
    assert_eq!(roster.members[0].label, "m1");
    assert_eq!(
        roster.deny_labels,
        vec![
            "m10".to_owned(),
            "m2".to_owned(),
            "m7".to_owned(),
            "m9".to_owned()
        ]
    );
    assert_eq!(
        roster.deny_nodes,
        vec![
            "0202020202020202020202020202020202020202020202020202020202020202".to_owned(),
            "0707070707070707070707070707070707070707070707070707070707070707".to_owned(),
            "1111111111111111111111111111111111111111111111111111111111111111".to_owned(),
            "22222222222222222222222222222222".to_owned(),
        ]
    );
}

#[test]
fn overlay_log_membership_cascades_key_denied_members_to_deny_labels_and_deny_nodes() {
    use crate::commons::binding::Request;
    use crate::membership_log::{Entrant, Founder, Log, Record, Root};

    let entrant = |tag: u8, label: &str| {
        let identity = Identity::from_secret_bytes([tag; 32]);
        let request = Request::create(
            [2; 16], [3; 32], [tag; 16], [tag; 16], &identity, &[tag; 32],
        )
        .unwrap();
        Entrant {
            request,
            node_key: [tag; 32],
            name: format!("{label}.local"),
            label: label.to_owned(),
        }
    };
    for unproved in [false, true] {
        let first = entrant(1, "m1");
        let second = entrant(2, "m2");
        let third = entrant(3, "m3");
        let root = Root::of(first.request.binding());
        let second_rec = second.request.binding().recipient().to_bech32();
        let mut second_founder = Founder::from(second);
        let mut third_founder = Founder::from(third);
        if unproved {
            second_founder.binding = None;
            third_founder.binding = None;
        }
        let log = Log::new(
            Record::genesis(
                &[99; 32],
                root,
                vec![Founder::from(first), second_founder, third_founder],
                1,
            )
            .unwrap(),
        )
        .unwrap();
        let second_nk = "02".repeat(32);
        let third_nk = "03".repeat(32);
        let prior_nid = "aa".repeat(16);
        let prior_nk = "bb".repeat(32);
        let mut roster = SwarmRoster {
            members: vec![crate::swarm_roster::SwarmMember {
                recipient: second_rec,
                vault_name: "m2".to_owned(),
                label: "m2".to_owned(),
                aliases: vec!["m20".to_owned()],
                hint: None,
                hw_hint: None,
                admitted_ms: 1,
                claimed_label: Some("m21".to_owned()),
                claimed_at_ms: Some(1),
                node_id: Some(prior_nid.clone()),
                node_key: Some(prior_nk.clone()),
            }],
            deny_labels: Vec::new(),
            deny_nodes: vec![second_nk.clone(), third_nk.clone()],
        };
        overlay_log_membership(&log, None, &mut roster);
        assert_eq!(roster.members.len(), 1);
        assert_eq!(roster.members[0].label, "m1");
        assert_eq!(
            roster.deny_labels,
            vec![
                "m2".to_owned(),
                "m20".to_owned(),
                "m21".to_owned(),
                "m3".to_owned()
            ],
            "unproved={unproved}"
        );
        assert_eq!(
            roster.deny_nodes,
            vec![second_nk, third_nk, prior_nid, prior_nk],
            "unproved={unproved}"
        );
    }
}

#[test]
fn overlay_log_membership_does_not_deny_renumbered_member_or_suppress_removed_colliding_label() {
    use crate::commons::binding::Request;
    use crate::membership_log::{Body, Entrant, Log, Record};

    let mk_entrant = |tag: u8, label: &str, name: &str| {
        let id = Identity::from_secret_bytes([tag ^ 0x33; 32]);
        let req = Request::create([2; 16], [3; 32], [tag; 16], [tag; 16], &id, &[tag; 32])
            .expect("request");
        (
            id,
            Entrant {
                request: req,
                node_key: [tag; 32],
                name: name.to_owned(),
                label: label.to_owned(),
            },
        )
    };

    let (m1_id, m1_ent) = mk_entrant(1, "m1", "m1.local");
    let (m2_id, m2_ent) = mk_entrant(2, "m2", "m2.local");
    // m70 collided on hostname `m2.local` when admitted, so its log `name` is `m2.local`
    // while its assigned label is `m70` and its `[machines]` vault_name is `m2-2`.
    let (m70_id, m70_ent) = mk_entrant(70, "m70", "m2.local");
    let m1_actor = m1_ent.request.binding().actor();
    let m2_actor = m2_ent.request.binding().actor();
    let m70_actor = m70_ent.request.binding().actor();

    let genesis = Record::found(&[99; 32], vec![m1_ent, m2_ent, m70_ent], 1_000).expect("genesis");
    let genesis_id = *genesis.id();
    let mut log = Log::new(genesis).expect("log");

    let remove_m2 = log
        .draft(
            &m1_actor,
            2_000,
            Body::Remove {
                member: m2_actor,
                cutoff: genesis_id,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign remove m2");
    log.insert(remove_m2).expect("insert remove m2");

    let mut roster = SwarmRoster {
        members: vec![
            crate::swarm_roster::SwarmMember {
                recipient: m1_id.recipient().to_bech32(),
                vault_name: "m1".to_owned(),
                label: "m1".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: None,
            },
            crate::swarm_roster::SwarmMember {
                recipient: m2_id.recipient().to_bech32(),
                vault_name: "m2".to_owned(),
                label: "m2".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: None,
            },
            crate::swarm_roster::SwarmMember {
                recipient: m70_id.recipient().to_bech32(),
                vault_name: "m2-2".to_owned(),
                label: "m70".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: None,
            },
        ],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };

    overlay_log_membership(&log, None, &mut roster);
    assert!(
        roster.is_label_denied("m2"),
        "removed m2 must be added to deny_labels despite m70's colliding m2.local hostname"
    );
    let m70_row = roster
        .member_by_recipient(&m70_id.recipient().to_bech32())
        .expect("m70 remains in roster");
    assert_eq!(m70_row.label, "m70");
    assert!(
        !roster.is_member_denied(m70_row),
        "surviving m70 must not be denied when colliding m2 is in deny_labels"
    );
    let membership = log.membership();
    let m70_log_member = membership.members().get(&m70_actor).expect("m70 in log");
    assert!(
        !log_entries::member_denied_by_roster(&roster, m70_log_member),
        "member_denied_by_roster must honor m70's active roster row over m2.local hostname"
    );
}

#[test]
fn swarm_roster_and_resolve_local_swarm_label_refuse_failed_pending_or_unpinned_state() {
    let root = aterm_tempfile::tempdir().unwrap();
    let vault_path = root.path().join("alab.vault");
    let admin_paths = Paths::for_home(&root.path().join("admin"));
    let _ = create(
        &admin_paths,
        vault_path.to_str().unwrap(),
        None,
        Some("m22"),
    )
    .unwrap();
    let _ = admin_seed_swarm_roster(&admin_paths, &[]).unwrap();

    assert!(
        swarm_roster_until(
            &admin_paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(
        resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()])
            .unwrap()
            .as_deref(),
        Some("m22")
    );

    let clean_state = load_state(&admin_paths).unwrap();
    let clean_cfg = config(&admin_paths).unwrap().unwrap();

    let mut failed_state = clean_state.clone();
    failed_state.failed = true;
    save_state(&admin_paths, &failed_state).unwrap();
    assert!(
        swarm_roster_until(
            &admin_paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap()
        .is_none(),
        "failed apply state must not expose un-overlaid base_swarm_roster"
    );
    assert_eq!(
        resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()]).unwrap(),
        None
    );

    let mut pending_state = clean_state.clone();
    pending_state
        .pending
        .insert("other".to_owned(), "00".repeat(32));
    save_state(&admin_paths, &pending_state).unwrap();
    assert!(
        swarm_roster_until(
            &admin_paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap()
        .is_none(),
        "pending apply state must not expose un-overlaid base_swarm_roster"
    );
    assert_eq!(
        resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()]).unwrap(),
        None
    );

    save_state(&admin_paths, &clean_state).unwrap();
    let mut unpinned_cfg = clean_cfg.clone();
    unpinned_cfg.signer = None;
    save_config(&admin_paths, &unpinned_cfg).unwrap();
    assert!(
        swarm_roster_until(
            &admin_paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()]).unwrap(),
        None
    );

    save_config(&admin_paths, &clean_cfg).unwrap();
    assert_eq!(
        resolve_local_swarm_label(&admin_paths, &["m22.local".to_owned()])
            .unwrap()
            .as_deref(),
        Some("m22")
    );
}

#[test]
fn overlay_and_lift_restored_ignore_forged_or_suffixed_claimed_labels() {
    use crate::commons::binding::Request;
    use crate::membership_log::{Body, Entrant, Evidence, Log, Record};

    let mk_entrant = |tag: u8, node_tag: u8, label: &str, name: &str| {
        let id = Identity::from_secret_bytes([tag ^ 0x55; 32]);
        let req = Request::create([2; 16], [3; 32], [tag; 16], [tag; 16], &id, &[tag; 32])
            .expect("request");
        (
            id,
            Entrant {
                request: req,
                node_key: [node_tag; 32],
                name: name.to_owned(),
                label: label.to_owned(),
            },
        )
    };

    let (m1_id, m1_ent) = mk_entrant(1, 1, "m1", "m1.local");
    let (m2_id, m2_ent) = mk_entrant(2, 2, "m2", "m2.local");
    let (m3_id, m3_ent) = mk_entrant(3, 3, "m3", "m3.local");
    let m1_actor = m1_ent.request.binding().actor();
    let m2_actor = m2_ent.request.binding().actor();
    let m3_actor = m3_ent.request.binding().actor();

    let genesis = Record::found(&[99; 32], vec![m1_ent, m2_ent, m3_ent], 1_000).expect("genesis");
    let genesis_id = *genesis.id();
    let mut log = Log::new(genesis).expect("log");

    // Remove m2, which carried a suffixed stale `claimed_label = Some("m1-3fa2")` colliding
    // with surviving `m1`'s canonical label.
    let remove_m2 = log
        .draft(
            &m1_actor,
            2_000,
            Body::Remove {
                member: m2_actor,
                cutoff: genesis_id,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign remove m2");
    log.insert(remove_m2).expect("insert remove m2");

    let mut roster = SwarmRoster {
        members: vec![
            crate::swarm_roster::SwarmMember {
                recipient: m1_id.recipient().to_bech32(),
                vault_name: "m1".to_owned(),
                label: "m1".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: Some(aterm_codec::hex::encode_lower(&[1u8; 32])),
            },
            crate::swarm_roster::SwarmMember {
                recipient: m2_id.recipient().to_bech32(),
                vault_name: "m2".to_owned(),
                label: "m2".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: Some("m1-3fa2".to_owned()),
                claimed_at_ms: Some(1_500),
                node_id: None,
                node_key: Some(aterm_codec::hex::encode_lower(&[2u8; 32])),
            },
            crate::swarm_roster::SwarmMember {
                recipient: m3_id.recipient().to_bech32(),
                vault_name: "m3".to_owned(),
                label: "m3".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: Some(aterm_codec::hex::encode_lower(&[3u8; 32])),
            },
        ],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };

    overlay_log_membership(&log, None, &mut roster);
    assert!(
        roster.is_label_denied("m2"),
        "removed m2 must be added to deny_labels: {:?}",
        roster.deny_labels
    );
    assert!(
        !roster.is_label_denied("m1"),
        "surviving m1 must not be collateral-denied by m2's suffixed claimed_label m1-3fa2: {:?}",
        roster.deny_labels
    );

    // Now remove m3 and restore m3 under a fresh secrets key with the same node_key `[3; 32]`,
    // while a denied row for m9 in `roster.members` carries a forged `claimed_label = Some("m3")`.
    let remove_m3 = log
        .draft(
            &m1_actor,
            3_000,
            Body::Remove {
                member: m3_actor,
                cutoff: genesis_id,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign remove m3");
    log.insert(remove_m3).expect("insert remove m3");

    let (_m3_restored_id, m3_restored_ent) = mk_entrant(30, 3, "m3", "m3.local");
    let admit_m3 = log
        .draft(
            &m1_actor,
            4_000,
            Body::Admit {
                entrant: m3_restored_ent,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&[1; 32], [1; 16])
        .expect("sign admit m3");
    log.insert(admit_m3).expect("insert admit m3");

    let (m9_id, _) = mk_entrant(9, 9, "m9", "m9.local");
    let m9_nk = aterm_codec::hex::encode_lower(&[9u8; 32]);
    let m3_nk = aterm_codec::hex::encode_lower(&[3u8; 32]);
    let mut restore_roster = SwarmRoster {
        members: vec![crate::swarm_roster::SwarmMember {
            recipient: m9_id.recipient().to_bech32(),
            vault_name: "m9".to_owned(),
            label: "m9".to_owned(),
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: 1_000,
            claimed_label: Some("m3".to_owned()),
            claimed_at_ms: Some(1_500),
            node_id: None,
            node_key: Some(m9_nk.clone()),
        }],
        deny_labels: vec!["m3".to_owned(), "m9".to_owned()],
        deny_nodes: vec![m3_nk.clone(), m9_nk.clone()],
    };
    let _ = lift_restored_from_roster(&log, &mut restore_roster);
    assert!(
        !restore_roster.is_label_denied("m3") && !restore_roster.is_node_denied(&m3_nk),
        "restored m3 must be lifted from deny_labels and deny_nodes"
    );
    assert!(
        restore_roster.is_label_denied("m9") && restore_roster.is_node_denied(&m9_nk),
        "denied m9 with forged claimed_label='m3' must not be lifted when m3 is restored: labels={:?}, nodes={:?}",
        restore_roster.deny_labels,
        restore_roster.deny_nodes
    );

    // A stale label-denied row (`m40`) sharing `node_id` / `node_key` with active `m1`
    // must not push `m1`'s node into `deny_nodes` or wipe `m1` during `overlay_log_membership`.
    let m1_nk = aterm_codec::hex::encode_lower(&[1u8; 32]);
    let mut stale_row_roster = SwarmRoster {
        members: vec![
            crate::swarm_roster::SwarmMember {
                recipient: "age1m40stale".to_owned(),
                vault_name: "m40".to_owned(),
                label: "m40".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 500,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: Some("00000000000000000000000000000001".to_owned()),
                node_key: Some(m1_nk.clone()),
            },
            crate::swarm_roster::SwarmMember {
                recipient: m1_id.recipient().to_bech32(),
                vault_name: "m1".to_owned(),
                label: "m1".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 1_000,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: Some("00000000000000000000000000000001".to_owned()),
                node_key: Some(m1_nk.clone()),
            },
        ],
        deny_labels: vec!["m40".to_owned()],
        deny_nodes: Vec::new(),
    };
    overlay_log_membership(&log, None, &mut stale_row_roster);
    assert!(
        !stale_row_roster.is_node_denied(&m1_nk)
            && !stale_row_roster.is_node_denied("00000000000000000000000000000001")
            && !stale_row_roster.is_label_denied("m1"),
        "stale denied row sharing node_id/node_key with active m1 must not poison m1: labels={:?}, nodes={:?}",
        stale_row_roster.deny_labels,
        stale_row_roster.deny_nodes
    );
    assert!(
        stale_row_roster.members.iter().any(|m| m.label == "m1"),
        "active m1 must remain in roster.members: {:?}",
        stale_row_roster.members
    );
}
