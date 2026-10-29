// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A vault that publishes no default fleet code, `m1` its admin, at a path.
struct Lab {
    root: aterm_tempfile::TempDir,
    m1: Paths,
    source: PathBuf,
    pinned: String,
    signer: [u8; 32],
}

fn lab(name: &str) -> Lab {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join(format!("{name}.vault"));
    let created = test_create_unbound(&m1, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = admin_open(&m1).unwrap();
    admin_keys::legacy::publish_legacy(&m1, &admin);
    let signer = config(&m1).unwrap().unwrap().signer.unwrap();
    Lab {
        pinned: format!("{}#{}", source.display(), created.signer),
        root,
        m1,
        source,
        signer,
    }
}

impl Lab {
    /// `host` joins the vault `m1` lets it into; `admin` names it an admin too.
    fn machine(&self, host: &str, admin: bool) -> Paths {
        let paths = Paths::for_home(&self.root.path().join(host));
        let recipient = machine_identity(&paths).unwrap().recipient().to_bech32();
        let opened = admin_open(&self.m1).unwrap();
        let mut doc = opened.doc.clone();
        doc.add_machine(host, &recipient).unwrap();
        if admin {
            doc.add_admin(host).unwrap();
        }
        admin_publish(&self.m1, &opened, &doc).unwrap();
        join(
            &paths,
            &JoinOptions {
                source: self.pinned.clone(),
                host: Some(host.into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        paths
    }

    /// Run `flow` with this vault read as the ALab vault: its signer the key this build names.
    fn as_alab<T>(&self, flow: impl FnOnce() -> T) -> T {
        let set = |retired: Option<Retired>| {
            envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = retired);
        };
        set(Some(Retired {
            keys: Vec::new(),
            current: Some(self.signer),
        }));
        let out = flow();
        set(None);
        out
    }

    fn published(&self) -> Vec<u8> {
        std::fs::read(&self.source).unwrap()
    }

    /// The default fleet code the vault publishes, if any.
    fn fleet_code(&self) -> Option<Vec<u8>> {
        admin_open(&self.m1)
            .unwrap()
            .doc
            .entry(&Audience::Everyone, fleet::FLEET_CODE_ENTRY)
            .map(|entry| entry.value.as_slice().to_vec())
    }
}

/// Only the ALab vault's admin is its steward: a member is none, and a personal vault (the same
/// vault, its signer no key this build names) has none. The read writes nothing.
/// NEGATIVE CONTROL: a `stewardship` that skips the ALab check reads the personal vault's
/// admin as its steward, and one that skips the admin locker reads the member as one.
#[test]
fn only_the_alab_vaults_admin_is_its_steward() {
    let lab = lab("steward-who");
    let member = lab.machine("m2", false);
    let before = lab.published();

    let personal = stewardship(&lab.m1).unwrap();
    assert!(
        matches!(&personal, Stewardship::No(why) if why.contains("not ALab's")),
        "{personal:?}"
    );
    lab.as_alab(|| {
        let read = stewardship(&member).unwrap();
        assert!(
            matches!(&read, Stewardship::No(why) if why.contains("no admin key")),
            "{read:?}"
        );
        assert_eq!(
            stewardship(&lab.m1).unwrap(),
            Stewardship::Steward { fleet_code: false }
        );
    });
    assert_eq!(lab.published(), before, "a read writes nothing");
}

/// A vault pass applying on this Mac now (holding its state lock) leaves the steward's read for
/// the next pass, which reads it once that pass ends: no error, so the converge pass waits.
/// NEGATIVE CONTROL: a `stewardship` that errs while the lock is held fails the pass's row.
#[test]
fn a_steward_read_waits_for_a_vault_pass_applying_now() {
    let lab = lab("steward-busy");
    lab.as_alab(|| {
        let held = StateLock::take(&lab.m1).unwrap();
        let read = stewardship(&lab.m1).unwrap();
        assert!(
            matches!(&read, Stewardship::Busy(why) if why.contains("applying")),
            "{read:?}"
        );
        let revoke_err = steward_finish_revokes(&lab.m1).unwrap_err();
        assert!(
            revoke_err.to_string().contains("applying"),
            "{revoke_err:?}"
        );
        drop(held);
        assert_eq!(
            stewardship(&lab.m1).unwrap(),
            Stewardship::Steward { fleet_code: false }
        );
    });
}

/// An admin whose person turned this Mac's vault sync off (`aterm secrets agent off`) is no
/// steward while that record stands: a steward's publish ends in a sync of the copy the person
/// stopped. Once the record goes, it is the steward again.
/// NEGATIVE CONTROL: a `stewardship` that skips the record reads the admin as the steward, and
/// its pass forms, publishes and syncs on the Mac whose person stopped vault sync.
#[test]
fn an_admin_whose_person_turned_vault_sync_off_is_no_steward() {
    let lab = lab("steward-sync-off");
    lab.as_alab(|| {
        crate::service::record_subscriber_off(&lab.m1.home).unwrap();
        let read = stewardship(&lab.m1).unwrap();
        assert!(
            matches!(&read, Stewardship::No(why) if why.contains("agent off")),
            "{read:?}"
        );
        std::fs::remove_file(crate::service::subscriber_off_path(&lab.m1.home)).unwrap();
        assert_eq!(
            stewardship(&lab.m1).unwrap(),
            Stewardship::Steward { fleet_code: false }
        );
    });
}

/// `admin_revoke` runs the full §7.6 swarm cascade in a single vault publish (putting the
/// revoked Mac's `m<N>` label + aliases on `deny_labels`, its `node_id` on `deny_nodes`, removing
/// its `SwarmMember` row, and releasing its `ai-pool.toml` warm slots, leases, and `last_heard`
/// while leaving `aterm-fleet-code` unchanged); `admin_revoke_stolen` additionally rotates
/// `aterm-fleet-code` (preserving `@<host>:<port>`) and emits a single-line `NEEDS YOU:` provider
/// rotation notice naming exposed credentials and warm AI accounts.
#[test]
fn revoke_runs_the_full_swarm_cascade_and_stolen_rotates_fleet_code_with_needs_you() {
    let lab = lab("steward-revoke-cascade");
    let m2 = lab.machine("m2", false);
    let m3 = lab.machine("m3", false);
    let m2_rec = machine_identity(&m2).unwrap().recipient().to_bech32();
    let m3_rec = machine_identity(&m3).unwrap().recipient().to_bech32();

    let initial_code = "atf1-0000-0000-0000-0000-0000-0000-0000-0000-0000@m1.local:7100";
    let pool_toml = "\
[last_heard]
m2 = 1793000000
m3 = 1793000010

[leases]
slot_a = \"m2\"
slot_b = \"m3\"

[[lease]]
holder = \"m2\"
account = \"claude:a@example.com\"

[[lease]]
holder = \"m3\"
account = \"claude:b@example.com\"

[[account]]
provider = \"claude\"
email = \"a@example.com\"
warm = [\"m2\", \"m3\"]
since = [100, 200]
why = [\"enrolled\", \"enrolled\"]
used = [150, 250]
";

    lab.as_alab(|| {
        let opened = admin_open(&lab.m1).unwrap();
        let mut doc = opened.doc.clone();
        let roster = crate::swarm_roster::SwarmRoster {
            members: vec![
                crate::swarm_roster::SwarmMember {
                    label: "m2".into(),
                    vault_name: "m2".into(),
                    recipient: m2_rec.clone(),
                    node_id: Some("node-m2-deadbeef".into()),
                    node_key: None,
                    aliases: vec!["m9".into()],
                    hint: None,
                    admitted_ms: 1,
                    claimed_label: None,
                    claimed_at_ms: None,
                },
                crate::swarm_roster::SwarmMember {
                    label: "m3".into(),
                    vault_name: "m3".into(),
                    recipient: m3_rec.clone(),
                    node_id: Some("node-m3-cafebabe".into()),
                    node_key: None,
                    aliases: Vec::new(),
                    hint: None,
                    admitted_ms: 1,
                    claimed_label: None,
                    claimed_at_ms: None,
                },
            ],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };
        doc.set_entry(
            &Audience::Everyone,
            crate::swarm_roster::ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        doc.set_entry(&Audience::Everyone, "ai-pool.toml", pool_toml.as_bytes())
            .unwrap();
        doc.set_entry(
            &Audience::Everyone,
            fleet::FLEET_CODE_ENTRY,
            initial_code.as_bytes(),
        )
        .unwrap();
        doc.set_entry(&Audience::Everyone, "gh_access_token", b"gho_test")
            .unwrap();
        doc.set_entry(&Audience::Everyone, "ai-mailbox.toml", b"imap = true\n")
            .unwrap();
        admin_publish(&lab.m1, &opened, &doc).unwrap();

        // 1. Normal `admin_revoke` on m2: full cascade, fleet code NOT rotated.
        let opened = admin_open(&lab.m1).unwrap();
        admin_revoke(&lab.m1, &opened, "m2", true).unwrap();

        let after_m2 = admin_open(&lab.m1).unwrap().doc.clone();
        assert!(after_m2.machines().iter().all(|m| m.name != "m2"));
        let roster_after_m2 = crate::swarm_roster::SwarmRoster::from_document(&after_m2)
            .unwrap()
            .unwrap();
        assert_eq!(roster_after_m2.members.len(), 1);
        assert_eq!(roster_after_m2.members[0].label, "m3");
        assert!(roster_after_m2.deny_labels.contains(&"m2".to_owned()));
        assert!(roster_after_m2.deny_labels.contains(&"m9".to_owned()));
        assert!(
            roster_after_m2
                .deny_nodes
                .contains(&"node-m2-deadbeef".to_owned())
        );

        let pool_after_m2 = std::str::from_utf8(
            after_m2
                .entry(&Audience::Everyone, "ai-pool.toml")
                .unwrap()
                .value
                .as_slice(),
        )
        .unwrap();
        assert!(!pool_after_m2.contains("\"m2\""), "{pool_after_m2}");
        assert!(pool_after_m2.contains("\"m3\""), "{pool_after_m2}");
        assert_eq!(lab.fleet_code().as_deref(), Some(initial_code.as_bytes()));
        assert!(matches!(
            steward_revoke_check(&lab.m1).unwrap(),
            StewardRevoke::Current(_)
        ));

        // 2. Stolen `admin_revoke_stolen` on m3: rotates fleet code + emits single-line NEEDS YOU.
        let opened = admin_open(&lab.m1).unwrap();
        let stolen = admin_revoke_stolen(&lab.m1, &opened, "m3", true).unwrap();
        assert!(stolen.fleet_code_rotated);
        assert!(
            stolen.needs_you_line.starts_with("NEEDS YOU:")
                && !stolen.needs_you_line.contains('\n')
                && stolen.needs_you_line.contains("m3")
                && stolen.needs_you_line.contains("gh tokens")
                && stolen.needs_you_line.contains("mailbox")
                && stolen.needs_you_line.contains("claude:a@example.com")
                && stolen.needs_you_line.contains("claude:b@example.com"),
            "{}",
            stolen.needs_you_line
        );
        let rotated_bytes = lab.fleet_code().unwrap();
        let rotated_str = std::str::from_utf8(&rotated_bytes).unwrap();
        assert_ne!(rotated_str, initial_code);
        assert!(rotated_str.starts_with("atf1-") && rotated_str.ends_with("@m1.local:7100"));
    });
}

/// NEGATIVE CONTROL & STEWARD REPAIR: a vault-only removal (`doc.remove_machine` without the C25
/// cascade) leaves the removed Mac in `aterm-swarm-roster` and `ai-pool.toml`. `steward_revoke_check`
/// detects that half-done state without writing, and `steward_finish_revokes` completes the cascade
/// in one publish.
#[test]
fn steward_revoke_detects_and_finishes_a_half_done_vault_only_removal() {
    let lab = lab("steward-revoke-repair");
    let m2 = lab.machine("m2", false);
    let m2_rec = machine_identity(&m2).unwrap().recipient().to_bech32();

    lab.as_alab(|| {
        let opened = admin_open(&lab.m1).unwrap();
        let mut doc = opened.doc.clone();
        let roster = crate::swarm_roster::SwarmRoster {
            members: vec![crate::swarm_roster::SwarmMember {
                label: "m2".into(),
                vault_name: "m2".into(),
                recipient: m2_rec,
                node_id: Some("node-m2-half".into()),
                node_key: None,
                aliases: Vec::new(),
                hint: None,
                admitted_ms: 1,
                claimed_label: None,
                claimed_at_ms: None,
            }],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        };
        doc.set_entry(
            &Audience::Everyone,
            crate::swarm_roster::ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        doc.set_entry(
            &Audience::Everyone,
            "ai-pool.toml",
            b"[[account]]\nprovider = \"claude\"\nemail = \"x@example.com\"\nwarm = [\"m2\"]\n",
        )
        .unwrap();
        // Simulate a pre-C25 / interrupted removal that dropped m2 from [machines] only.
        doc.remove_machine("m2", true).unwrap();
        admin_publish(&lab.m1, &opened, &doc).unwrap();
        sync(&lab.m1, None).unwrap();

        let before = lab.published();
        let probe = steward_revoke_check(&lab.m1).unwrap();
        assert!(matches!(probe, StewardRevoke::Due { .. }), "{probe:?}");
        assert_eq!(lab.published(), before, "check writes nothing");

        let done = steward_finish_revokes(&lab.m1).unwrap();
        assert!(matches!(done, StewardRevoke::Done(_)), "{done:?}");

        let repaired = admin_open(&lab.m1).unwrap().doc.clone();
        let roster_after = crate::swarm_roster::SwarmRoster::from_document(&repaired)
            .unwrap()
            .unwrap();
        assert!(roster_after.members.is_empty());
        assert_eq!(roster_after.deny_labels, vec!["m2".to_owned()]);
        assert_eq!(roster_after.deny_nodes, vec!["node-m2-half".to_owned()]);
        let pool_after = std::str::from_utf8(
            repaired
                .entry(&Audience::Everyone, "ai-pool.toml")
                .unwrap()
                .value
                .as_slice(),
        )
        .unwrap();
        assert!(!pool_after.contains("\"m2\""), "{pool_after}");

        sync(&lab.m1, None).unwrap();
        let settled = lab.published();
        assert!(matches!(
            steward_revoke_check(&lab.m1).unwrap(),
            StewardRevoke::Current(_)
        ));
        assert!(matches!(
            steward_finish_revokes(&lab.m1).unwrap(),
            StewardRevoke::Current(_)
        ));
        assert_eq!(lab.published(), settled, "second pass publishes nothing");
    });
}

#[test]
fn revoke_before_explicit_roster_seed_seeds_remaining_members_first() {
    let lab = lab("steward-revoke-unseeded-roster");
    let _m2 = lab.machine("m2", false);
    let _m4 = lab.machine("mac-mini-m4", false);

    lab.as_alab(|| {
        let opened = admin_open(&lab.m1).unwrap();
        assert!(
            crate::swarm_roster::SwarmRoster::from_document(&opened.doc)
                .unwrap()
                .is_none()
        );
        admin_revoke(&lab.m1, &opened, "mac-mini-m4", true).unwrap();

        let after = admin_open(&lab.m1).unwrap().doc.clone();
        let roster = crate::swarm_roster::SwarmRoster::from_document(&after)
            .unwrap()
            .expect("revoke on ALab vault creates swarm roster");
        let labels: Vec<&str> = roster.members.iter().map(|m| m.label.as_str()).collect();
        assert!(
            labels.contains(&"m1") && labels.contains(&"m2"),
            "remaining vault members must be seeded into roster: {labels:?}"
        );
        assert!(
            !roster.deny_labels.is_empty(),
            "revoked member's seeded label must be added to deny_labels: {:?}",
            roster.deny_labels
        );
    });
}
