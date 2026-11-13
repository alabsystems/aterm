// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::swarm_roster::{SwarmMember, SwarmPolicy, SwarmRoster};

fn put(
    by_audience: &mut BTreeMap<Audience, BTreeMap<String, Entry>>,
    audience: Audience,
    name: &str,
    value: &str,
) {
    by_audience.entry(audience.clone()).or_default().insert(
        name.to_owned(),
        Entry {
            name: name.to_owned(),
            value: crate::SecretBytes::from(value.as_bytes()),
            audience,
            if_absent: false,
            anchor: None,
        },
    );
}

/// The label an allowing member gives keeps off what the roster's approver keeps off: a
/// deny-listed label (a stolen Mac's, which `repair_half_done_revokes` moves off the members)
/// and a `[machines]` name. No number is reserved: a policy that
/// names a steward `m50` holds nothing, and `m3` is free while no Mac holds it. The
/// fleet-accounts sheet's numbers are counted toward "one above the highest" without holding
/// a label by themselves.
///
/// NEGATIVE CONTROL: skip `deny_labels` and `vault_name` (the code before C11's fix): `m9` and
/// `m40` read free and the first `taken` assert fails; seed `taken` with `m3` or read the
/// policy's stewards (the reservations this ruling removed) and the second assert fails.
#[test]
fn a_newcomer_label_keeps_off_denied_and_machines_names_and_counts_the_sheet() {
    let roster = SwarmRoster {
        members: vec![
            SwarmMember {
                recipient: "age1synthetic".to_owned(),
                vault_name: "m40-studio".to_owned(),
                label: "m41".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 0,
                claimed_label: Some("m42".to_owned()),
                claimed_at_ms: Some(1_000),
                node_id: None,
                node_key: None,
            },
            SwarmMember {
                recipient: "age1denied".to_owned(),
                vault_name: "m98-forged".to_owned(),
                label: "m9".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 0,
                claimed_label: Some("m99".to_owned()),
                claimed_at_ms: Some(1_000),
                node_id: None,
                node_key: None,
            },
        ],
        deny_labels: vec!["m9".to_owned()],
        deny_nodes: Vec::new(),
    };
    let policy = SwarmPolicy {
        network_id: None,
        servers: Vec::new(),
        stewards: vec![crate::swarm_roster::StewardPolicy {
            label: "m50".to_owned(),
            rank: 1,
            always_on: true,
        }],
    };
    let mut by_audience = BTreeMap::new();
    put(
        &mut by_audience,
        Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        &roster.to_toml(),
    );
    put(
        &mut by_audience,
        Audience::Hosts(vec!["m22".to_owned()]),
        crate::SWARM_POLICY_ENTRY,
        &policy.to_toml(),
    );
    put(
        &mut by_audience,
        Audience::Everyone,
        "fleet-accounts.toml",
        "[[account]]\nlabel = \"a\"\nmachines = [\"m70\"]\n",
    );
    let (taken, counted) = labels_taken(&by_audience);
    for held in ["m9", "m40", "m41", "m42"] {
        assert!(taken.contains(held), "{held} is taken: {taken:?}");
    }
    for free in ["m3", "m50"] {
        assert!(
            !taken.contains(free),
            "no Mac holds {free}, so nothing reserves it: {taken:?}"
        );
    }
    assert!(
        !taken.contains("m98") && !taken.contains("m99"),
        "denied row's forged claimed_label or vault_name must not be taken: {taken:?}"
    );
    assert!(!taken.contains("m70"), "a sheet's number holds no label");
    assert!(counted.contains("m70"), "{counted:?}");
}

#[test]
fn kept_out_with_node_refuses_removed_or_already_bound_node_key() {
    use crate::membership_log::{Body, Entrant, Evidence};

    let network = [0x51; 16];
    let authority = [0x52; 32];
    let make_req = |tag: u8| {
        let seed = [tag; 32];
        let inc = [tag; 16];
        let id = Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let req = Request::create(
            network,
            authority,
            [tag.wrapping_add(0x20); 16],
            inc,
            &id,
            &seed,
        )
        .expect("request");
        (seed, inc, req)
    };

    let (seed1, inc1, req1) = make_req(1);
    let actor1 = req1.binding().actor();
    let node1 = [0xAA; 32];
    let ent1 = Entrant {
        request: req1,
        node_key: node1,
        name: "m1".to_owned(),
        label: "m1".to_owned(),
    };
    let genesis = Record::found(&[9; 32], vec![ent1], 1_000).expect("genesis");
    let mut log = Log::new(genesis).expect("log");

    // A newcomer presenting m1's active node_key under a fresh binding is refused.
    let (_, _, req2) = make_req(2);
    let conflict = kept_out_with_node(&log, req2.binding(), node1, false)
        .expect("reusing active node_key under another binding must be refused");
    assert!(
        conflict.contains("already bound to a different seat"),
        "{conflict}"
    );

    // Admit m2 with node2, then remove m2; a fresh binding reusing node2 must be refused.
    let node2 = [0xBB; 32];
    let actor2 = req2.binding().actor();
    let admit2 = log
        .draft(
            &actor1,
            2_000,
            Body::Admit {
                entrant: Entrant {
                    request: req2,
                    node_key: node2,
                    name: "m2".to_owned(),
                    label: "m2".to_owned(),
                },
                evidence: Evidence::Phrase,
            },
        )
        .sign(&seed1, inc1)
        .unwrap();
    let admit2_id = *admit2.id();
    log.insert(admit2).unwrap();
    let rem2 = log
        .draft(
            &actor1,
            3_000,
            Body::Remove {
                member: actor2,
                cutoff: admit2_id,
            },
        )
        .sign(&seed1, inc1)
        .unwrap();
    log.insert(rem2).unwrap();

    let (_, _, req3) = make_req(3);
    let removed_why = kept_out_with_node(&log, req3.binding(), node2, false)
        .expect("removed node_key must be kept out even with a fresh binding");
    assert!(
        removed_why.contains("the vault removed this Mac"),
        "{removed_why}"
    );

    // Even under `restore = true` ("Allow again"), a removed Mac cannot claim a node_key
    // (`node1`) that is already bound to a different active member (`m1`).
    let steal_active = kept_out_with_node(&log, req3.binding(), node1, true)
        .expect("restore must not allow claiming another active member's node_key");
    assert!(
        steal_active.contains("already bound to a different seat"),
        "{steal_active}"
    );

    // Restoring single-member-removed `node2` under `req3` (`restore = true`) is permitted
    // and supersedes the removed seat so `node2` becomes `NodeStanding::Member`.
    assert!(kept_out_with_node(&log, req3.binding(), node2, true).is_none());
    let admit3 = log
        .draft(
            &actor1,
            4_000,
            Body::Admit {
                entrant: Entrant {
                    request: req3,
                    node_key: node2,
                    name: "m3".to_owned(),
                    label: "m3".to_owned(),
                },
                evidence: Evidence::Phrase,
            },
        )
        .sign(&seed1, inc1)
        .unwrap();
    log.insert(admit3).unwrap();
    assert!(
        matches!(
            log.membership().node(&node2),
            crate::membership_log::NodeStanding::Member(_)
        ),
        "restored node_key must become Member, got: {:?}",
        log.membership().node(&node2)
    );
}

#[test]
fn write_from_log_bundle_refuses_admitter_left_out_without_roster_entry() {
    use crate::membership_log::{Body, Entrant, Evidence, encode_records};
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    let root = aterm_tempfile::tempdir().unwrap();
    let vault_seed = [9u8; 32];
    let vault_signer: [u8; 32] = Ed25519KeyPair::from_seed_unchecked(&vault_seed)
        .unwrap()
        .public_key()
        .as_ref()
        .try_into()
        .unwrap();
    let network = [0x61; 16];
    let authority = [0x62; 32];
    let make_req = |tag: u8, id: &Identity| {
        let seed = [tag; 32];
        let inc = [tag; 16];
        let req = Request::create(
            network,
            authority,
            [tag.wrapping_add(0x20); 16],
            inc,
            id,
            &seed,
        )
        .expect("request");
        (seed, inc, req)
    };

    let id1 = Identity::from_secret_bytes([0x41; 32]);
    let id2 = Identity::from_secret_bytes([0x42; 32]);
    let (seed1, inc1, req1) = make_req(1, &id1);
    let (seed2, inc2, req2) = make_req(2, &id2);
    let actor1 = req1.binding().actor();
    let actor2 = req2.binding().actor();

    let genesis = Record::found(
        &vault_seed,
        vec![
            Entrant {
                request: req1.clone(),
                node_key: [0x11; 32],
                name: "m1".to_owned(),
                label: "m1".to_owned(),
            },
            Entrant {
                request: req2.clone(),
                node_key: [0x22; 32],
                name: "m2".to_owned(),
                label: "m2".to_owned(),
            },
        ],
        1_000,
    )
    .expect("genesis");
    let mut log = Log::new(genesis).expect("log");

    // m1 mints `[everyone]` epoch 1, wraps to m1 and m2, and puts `"shared"` (no roster entry).
    let (ep1_draft, ep1_key) = log
        .draft_epoch(&actor1, 1_500, Audience::Everyone, 1)
        .unwrap();
    let ep1 = ep1_draft.sign(&seed1, inc1).unwrap();
    let ep1_id = *ep1.id();
    log.insert(ep1).unwrap();
    for rec in [id1.recipient(), id2.recipient()] {
        let w = log
            .draft_wrap(&actor1, 1_501, ep1_id, &ep1_key, rec)
            .unwrap()
            .sign(&seed1, inc1)
            .unwrap();
        log.insert(w).unwrap();
    }
    let put = log
        .draft_put(
            &actor1,
            1_502,
            ep1_id,
            &ep1_key,
            &Entry {
                name: "shared".to_owned(),
                value: crate::SecretBytes::from(&b"v1"[..]),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            },
        )
        .unwrap()
        .sign(&seed1, inc1)
        .unwrap();
    log.insert(put).unwrap();

    // m2 admits m3 and wraps epoch 1 to m3.
    let m3_ok = Paths::for_home(&root.path().join("m3-ok"));
    let id3 = machine_identity(&m3_ok).unwrap();
    let (_, _, req3) = make_req(3, &id3);
    let admit3 = log
        .draft(
            &actor2,
            2_000,
            Body::Admit {
                entrant: Entrant {
                    request: req3.clone(),
                    node_key: [0x33; 32],
                    name: "m3".to_owned(),
                    label: "m3".to_owned(),
                },
                evidence: Evidence::Phrase,
            },
        )
        .sign(&seed2, inc2)
        .unwrap();
    let admit3_id = *admit3.id();
    log.insert(admit3).unwrap();
    let wrap3_ep1 = log
        .draft_wrap(&actor2, 2_001, ep1_id, &ep1_key, id3.recipient())
        .unwrap()
        .sign(&seed2, inc2)
        .unwrap();
    log.insert(wrap3_ep1).unwrap();

    let allowed = Allowed::sign(req3.binding(), "m3", &seed2).unwrap();
    let make_bundle = |l: &Log| Bundle {
        vault: "test-vault".to_owned(),
        signer: vault_signer,
        followed: Vec::new(),
        servers: vec!["http://127.0.0.1:9/v1/test-vault".to_owned()],
        allowed: allowed.clone(),
        handed: Handed::Log {
            source: None,
            admit: admit3_id,
            records: encode_records(l.records()),
        },
    };

    // Positive control: before m2 is left out of `[everyone]`, m3 accepts the log bundle.
    write_from_log_bundle(&m3_ok, &make_bundle(&log)).expect("active admitter succeeds");

    // Negative control: m1 rotates `[everyone]` to epoch 2, wrapping to m1 and m3 while
    // leaving admitter m2 out (still with no `aterm-swarm-roster` entry in the log).
    let (ep2_draft, ep2_key) = log
        .draft_epoch(&actor1, 3_000, Audience::Everyone, 2)
        .unwrap();
    let ep2 = ep2_draft.sign(&seed1, inc1).unwrap();
    let ep2_id = *ep2.id();
    log.insert(ep2).unwrap();
    for rec in [id1.recipient(), id3.recipient()] {
        let w = log
            .draft_wrap(&actor1, 3_001, ep2_id, &ep2_key, rec)
            .unwrap()
            .sign(&seed1, inc1)
            .unwrap();
        log.insert(w).unwrap();
    }
    let put2 = log
        .draft_put(
            &actor1,
            3_002,
            ep2_id,
            &ep2_key,
            &Entry {
                name: "shared".to_owned(),
                value: crate::SecretBytes::from(&b"v2"[..]),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            },
        )
        .unwrap()
        .sign(&seed1, inc1)
        .unwrap();
    log.insert(put2).unwrap();

    let m3_revoked = Paths::for_home(&root.path().join("m3-revoked"));
    fsx::ensure_private_dir(&m3_revoked.dir).unwrap();
    std::fs::copy(m3_ok.dir.join("identity"), m3_revoked.dir.join("identity")).unwrap();
    let err = write_from_log_bundle(&m3_revoked, &make_bundle(&log))
        .expect_err("admitter left out of Everyone must be refused even without roster entry")
        .to_string();
    assert!(
        err.contains("the member that signed this Admit is revoked in the vault"),
        "{err}"
    );
}

/// The Commons request of the Mac at `home` (its identity made there if it has none) to the
/// network and authority of the genesis `member` holds.
fn request_of(member: &Paths, home: &Paths) -> Request {
    machine_identity(home).expect("identity");
    let store = load_records(member).expect("records");
    let log = store.log().expect("a log");
    let membership_log::Body::Genesis { root, .. } = log.genesis().body() else {
        panic!("a genesis");
    };
    Device::open(home)
        .expect("device")
        .expect("an identity")
        .request(root.network, root.authority)
        .expect("request")
}

/// `m22`'s file vault, its genesis listing `names` (each in `[machines]`, none enrolled in
/// Commons) as founders with neither a binding nor a node key, as the lab's `swarm e2e` lists
/// `s`, `x` and `y`: `m22`, the bound founder that allows, and each founder's request and the
/// label its genesis gives it.
fn unproved_founders(root: &std::path::Path, names: &[&str]) -> (Paths, Vec<(Request, String)>) {
    let vault = root.join("alab.vault");
    let m22 = Paths::for_home(&root.join("m22"));
    test_create_unbound(
        &m22,
        vault.to_str().expect("a UTF-8 path"),
        None,
        Some("m22"),
    )
    .expect("create");
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).expect("admin"));
    admin_commons_init(&m22).expect("commons init");
    let homes: Vec<Paths> = names
        .iter()
        .map(|n| Paths::for_home(&root.join(n)))
        .collect();
    let recipients: Vec<String> = homes
        .iter()
        .map(|home| {
            machine_identity(home)
                .expect("identity")
                .recipient()
                .to_bech32()
        })
        .collect();
    let admin = admin_open(&m22).expect("admin");
    let mut doc = admin.doc.clone();
    for (name, recipient) in names.iter().zip(&recipients) {
        doc.add_machine(name, recipient).expect("add machine");
    }
    doc.set_entry(&Audience::Everyone, "shared", b"v1")
        .expect("entry");
    admin_publish_approved(&m22, &admin, &doc, &recipients).expect("approve");
    seed_vault_genesis(&m22, &root.join("genesis.bin")).expect("seed the genesis");
    let membership = load_records(&m22)
        .expect("records")
        .log()
        .expect("a log")
        .membership();
    let founders = homes
        .iter()
        .map(|home| {
            let request = request_of(&m22, home);
            let label = membership
                .unproved()
                .iter()
                .find(|f| f.recipient == request.binding().recipient() && f.node_key.is_none())
                .expect("the genesis lists it unproved, with no node key")
                .label
                .clone();
            (request, label)
        })
        .collect();
    (m22, founders)
}

/// A person's Allow on `m22` of the Mac `request` names, asking as `name` with no node key,
/// as [`hand_over`] runs it for a new Mac that reads the log: the label it was let in under.
fn allow(m22: &Paths, request: &Request, name: &str) -> String {
    let by_log = log_path(
        m22,
        request,
        [0; 32],
        name,
        aterm_time::unix_millis().expect("a clock"),
        (
            Some(Reader::BoundedLog(
                crate::crossing::RecordsBound::uncrossed(),
            )),
            false,
            (Book::default(), false),
        ),
        None,
    )
    .expect("the Allow runs");
    let ByLog::Admitted(admission) = by_log else {
        panic!("m22 did not let {name} in through the log: {by_log:?}");
    };
    admission.label
}

/// One-mesh E2 (f184): a person's Allow of a founder the genesis lists with its recipient and
/// neither a binding nor a node key seats it under the label its genesis gave it, as its proof
/// from the home ALab LAN does (`readmit::decide`'s `Founder`), not under the next free one
/// (the lab's founder `x`, labelled `m6`, was seated as `m9`). Where an `Admit` claimed that
/// label first (`z`, which m22 admits under `y`'s), the Allow takes the next free label,
/// which the evaluator seats bare, and `z` keeps its own.
///
/// NEGATIVE CONTROL: [`ALLOW_NEXT_LABEL`] on, as the test runs it around `w`'s Allow on every
/// pass (its `CONTROL` assert): the founder takes the next free label, and set around `x`'s
/// Allow the first assert fails. With `unproved_founder_label`'s claim scan cut, `y` takes
/// `z`'s label, the evaluator would seat it suffixed, and the Allow keeps nothing.
#[test]
fn a_persons_allow_seats_an_unproved_founder_under_its_genesis_label() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (m22, founders) = unproved_founders(root.path(), &["w", "x", "y"]);
    let [(w, w_label), (x, x_label), (y, y_label)] = &founders[..] else {
        panic!("three founders");
    };
    let membership = || {
        load_records(&m22)
            .expect("records")
            .log()
            .expect("a log")
            .membership()
    };

    ALLOW_NEXT_LABEL.with(|on| on.set(true));
    let old = allow(&m22, w, "w");
    ALLOW_NEXT_LABEL.with(|on| on.set(false));
    assert_ne!(
        &old, w_label,
        "CONTROL: the rule before f184 takes the next free label"
    );

    assert_eq!(
        &allow(&m22, x, "x"),
        x_label,
        "x keeps the label its genesis gave it"
    );
    let seated = membership();
    assert_eq!(
        seated.label(&x.binding().actor()),
        Some(x_label.as_str()),
        "x is seated under it bare"
    );
    assert!(
        seated
            .unproved()
            .iter()
            .all(|f| f.recipient != x.binding().recipient()),
        "x is proved"
    );

    // z, a Mac no genesis lists, claims y's label first.
    let z = request_of(&m22, &Paths::for_home(&root.path().join("z")));
    log_entries::admit_member_in_log(
        &m22,
        Entrant {
            request: z.clone(),
            node_key: [0; 32],
            name: "z".to_owned(),
            label: y_label.clone(),
        },
        membership_log::Evidence::Phrase,
    )
    .expect("m22 admits z under y's label");
    let label = allow(&m22, y, "y");
    assert_ne!(
        &label, y_label,
        "a label an Admit claimed is not the founder's to take"
    );
    let seated = membership();
    assert_eq!(
        seated.label(&y.binding().actor()),
        Some(label.as_str()),
        "y is seated bare"
    );
    assert_eq!(
        seated.label(&z.binding().actor()),
        Some(y_label.as_str()),
        "z keeps its label"
    );
}
