// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::swarm_roster::{StewardPolicy, SwarmMember};

const ME: &str = "age1me";

fn n(text: &str) -> Number {
    Number::parse(text).expect("a number")
}

fn row(
    recipient: &str,
    label: &str,
    aliases: &[&str],
    claim: Option<&str>,
    vault: &str,
) -> SwarmMember {
    SwarmMember {
        recipient: recipient.to_owned(),
        vault_name: vault.to_owned(),
        label: label.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        hint: None,
        hw_hint: None,
        admitted_ms: 0,
        claimed_label: claim.map(str::to_owned),
        claimed_at_ms: claim.map(|_| 1_000),
        node_id: None,
        node_key: None,
    }
}

fn roster() -> SwarmRoster {
    SwarmRoster {
        members: vec![
            row(ME, "m8", &["m5"], Some("m70"), "m8-macbook-pro"),
            row("age1heron", "m20", &["m21"], Some("m22"), "m23-mac-mini"),
            row("age1otter", "m30", &["m31"], Some("m32"), "m33-imac"),
            row("age1puffin", "m35", &[], Some("m36-typo"), "studio"),
        ],
        deny_labels: vec!["m30".to_owned(), "m40".to_owned()],
        deny_nodes: Vec::new(),
    }
}

fn policy() -> SwarmPolicy {
    SwarmPolicy {
        network_id: None,
        reserved_labels: vec!["m60".to_owned(), "m20".to_owned()],
        release_hosts: vec!["m61".to_owned(), "m8".to_owned()],
        servers: vec![
            "m8".to_owned(),
            "m63".to_owned(),
            "http://m64.local:7450".to_owned(),
            "https://vault-host.example:443".to_owned(),
            "65".to_owned(),
        ],
        stewards: vec![StewardPolicy {
            label: "m62".to_owned(),
            rank: 0,
            always_on: true,
        }],
    }
}

fn seats() -> Vec<(String, String)> {
    vec![
        ("m8".to_owned(), ME.to_owned()),
        ("m9".to_owned(), ME.to_owned()),
        ("m50".to_owned(), "age1kestrel".to_owned()),
    ]
}

/// Every number admission keeps off a newcomer is held from this Mac's rename, with what holds
/// it; this Mac's own row and seats hold nothing; and the policy's servers are the numbers they
/// are dialed by, a bare label or a URL's `m<N>.local`.
#[test]
fn the_vault_holds_what_admission_keeps_and_this_macs_own_is_free() {
    let labels = assemble(ME, Some(&roster()), Some(&policy()), &seats());
    let held: Vec<_> = labels
        .held
        .iter()
        .map(|(num, why)| (num.to_string(), *why))
        .collect();
    let want = [
        ("m3", LabelHold::Reserved),
        ("m20", LabelHold::Label),
        ("m21", LabelHold::Alias),
        ("m22", LabelHold::Claim),
        ("m23", LabelHold::VaultName),
        ("m30", LabelHold::Denied),
        ("m31", LabelHold::Denied),
        ("m35", LabelHold::Label),
        ("m40", LabelHold::Denied),
        ("m50", LabelHold::Seat),
        ("m60", LabelHold::Reserved),
        ("m61", LabelHold::ReleaseHost),
        ("m62", LabelHold::Steward),
        ("m63", LabelHold::Server),
        ("m64", LabelHold::Server),
    ];
    let want: Vec<_> = want
        .iter()
        .map(|(num, why)| ((*num).to_owned(), *why))
        .collect();
    assert_eq!(held, want);
    assert_eq!(
        labels.own,
        BTreeSet::from([n("m5"), n("m8"), n("m9"), n("m70")]),
        "this Mac's label, alias, claim and seats"
    );
    assert_eq!(labels.label, Some(n("m8")));
    assert_eq!(
        labels.servers,
        BTreeSet::from([n("m8"), n("m63"), n("m64")]),
        "bare digits and a host that is no m<N>.local are dialed by no number"
    );
}

/// This Mac's own numbers never free another Mac's hold: not a clash's suffixed seat beside the
/// first claimant's bare one, not a sheet number two rows claim, not this Mac's stale
/// `[machines]` name leading with another Mac's label. They free only a role's hold.
#[test]
fn this_macs_own_numbers_never_free_another_macs() {
    let roster = SwarmRoster {
        members: vec![
            row(ME, "m8", &[], Some("m70"), "m23-old-name"),
            row("age1heron", "m23", &[], Some("m70"), "m23-mac-mini"),
            row("age1otter", "m40", &[], None, "m40-imac"),
        ],
        ..SwarmRoster::default()
    };
    let seats = [
        ("m40-3fa2".to_owned(), ME.to_owned()),
        ("m40".to_owned(), "age1otter".to_owned()),
    ];
    let labels = assemble(ME, Some(&roster), None, &seats);
    assert_eq!(labels.held.get(&n("m40")), Some(&LabelHold::Label));
    assert_eq!(labels.held.get(&n("m70")), Some(&LabelHold::Claim));
    assert_eq!(labels.held.get(&n("m23")), Some(&LabelHold::Label));
    assert!(
        !labels.own.contains(&n("m40")),
        "a suffixed seat's number is the first claimant's"
    );
    // A role's label this Mac holds stays its own; the same policy read by another Mac holds it.
    let steward = SwarmPolicy {
        stewards: vec![StewardPolicy {
            label: "m8".to_owned(),
            rank: 0,
            always_on: true,
        }],
        ..SwarmPolicy::default()
    };
    let mine = assemble(ME, Some(&roster), Some(&steward), &[]);
    assert!(!mine.held.contains_key(&n("m8")));
    let theirs = assemble("age1stranger", Some(&roster), Some(&steward), &[]);
    assert_eq!(theirs.held.get(&n("m8")), Some(&LabelHold::Label));
}

/// The rules' edges: a denied row's claim and `[machines]` name hold nothing (as admission reads
/// a denied row), nor does a claim that is no bare number; with nothing read only `m3` is held;
/// and the same rows read as another Mac's hold every one of this Mac's numbers.
#[test]
fn a_denied_rows_claim_a_malformed_claim_and_nothing_read() {
    let labels = assemble(ME, Some(&roster()), Some(&policy()), &seats());
    for free in ["m32", "m33", "m36", "m8", "m65"] {
        assert!(
            !labels.held.contains_key(&n(free)),
            "{free} is free: {labels:?}"
        );
    }
    let nothing = assemble(ME, None, None, &[]);
    assert_eq!(
        nothing,
        LabLabels {
            held: BTreeMap::from([(n("m3"), LabelHold::Reserved)]),
            ..LabLabels::default()
        }
    );
    let stranger = assemble("age1stranger", Some(&roster()), None, &seats());
    for num in ["m5", "m8", "m9", "m70"] {
        assert!(stranger.held.contains_key(&n(num)), "{num}: {stranger:?}");
    }
    assert!(stranger.own.is_empty() && stranger.label.is_none());
}

/// No vault configured reads as nothing held at all: off the lab there is nothing to check.
#[test]
fn no_vault_holds_nothing() {
    let home = aterm_tempfile::TempDir::new().expect("a home");
    let paths = Paths::locate_for_home(home.path());
    assert_eq!(lab_labels(&paths).expect("reads"), LabLabels::default());
}

/// A real vault's copy: an applied roster's other rows are held and this Mac's own label is
/// not. A Mac still joining, or one whose last apply did not finish, refuses rather than read as
/// nothing held; one its vault took out, or a plain vault, holds nothing.
#[test]
fn a_vault_holds_its_rosters_labels_and_an_unwhole_copy_refuses() {
    let root = aterm_tempfile::tempdir().expect("a home");
    let paths = Paths::for_home(root.path());
    let identity = machine_identity(&paths).expect("an identity");
    let me = identity.recipient().to_bech32();
    let admin = Identity::generate().expect("an admin");
    let mut doc = Document::new_vault("admin", &admin.recipient().to_bech32());
    doc.add_machine("m7", &me).expect("this Mac");
    let roster = SwarmRoster {
        members: vec![
            row(&me, "m7", &[], None, "m7"),
            row("age1heron", "m20", &["m21"], None, "m20"),
        ],
        ..SwarmRoster::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .expect("a roster");
    let seed = [77; 32];
    let config = Config {
        source: Source::Url("http://127.0.0.1:1/v1/labels".to_owned()),
        mirrors: Vec::new(),
        name: "labels".to_owned(),
        signer: Some(envelope::signer_public(&seed).expect("a signer")),
        followed: Vec::new(),
        host: "m7".to_owned(),
    };
    save_config(&paths, &config).expect("configured");
    sync_with(
        &paths,
        seal("labels", 1, &seed, &[78; 32], false, &doc, &[]).expect("sealed"),
    )
    .expect("applied");
    let state = load_state(&paths).expect("a state");
    assert_eq!(state.membership, Stored::In);
    let labels = lab_labels(&paths).expect("reads");
    assert_eq!(labels.held.get(&n("m20")), Some(&LabelHold::Label));
    assert_eq!(labels.held.get(&n("m21")), Some(&LabelHold::Alias));
    assert_eq!(labels.label, Some(n("m7")));
    assert!(!labels.held.contains_key(&n("m7")), "{labels:?}");
    let mut failed = state.clone();
    failed.failed = true;
    let mut unapplied = state.clone();
    unapplied
        .pending
        .insert("other".to_owned(), "00".repeat(32));
    let mut joining = state.clone();
    joining.membership = Stored::Pending;
    let mut never = state.clone();
    never.membership = Stored::None;
    for unwhole in [failed, unapplied, joining, never] {
        save_state(&paths, &unwhole).expect("saved");
        assert!(lab_labels(&paths).is_err(), "{unwhole:?}");
    }
    for off in [Stored::Revoked, Stored::Plain] {
        let mut out = state.clone();
        out.membership = off;
        save_state(&paths, &out).expect("saved");
        assert_eq!(lab_labels(&paths).expect("reads"), LabLabels::default());
    }
}
