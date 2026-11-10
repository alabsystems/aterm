// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn binding_denial_observation_does_not_wait_for_device_custody() {
    let lab = lab("denial-observation");
    let device = crate::commons::bootstrap::Device::open(&lab.m1)
        .unwrap()
        .expect("existing secrets identity");
    let request = device.request([1; 16], [2; 32]).unwrap();
    let publish_denied = |key: &str| {
        let admin = admin_open(&lab.m1).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            format!("deny_nodes = [\"{key}\"]\n").as_bytes(),
        )
        .unwrap();
        admin_publish(&lab.m1, &admin, &doc).unwrap();
    };
    publish_denied(&aterm_codec::hex::encode_lower(&[0x91; 32]));
    assert!(
        !local_recipient_denied_in_roster(&lab.m1),
        "an unrelated denied key does not revoke this device"
    );
    publish_denied(&aterm_codec::hex::encode_lower(request.binding().writer()));
    assert!(
        local_recipient_denied_in_roster(&lab.m1),
        "the authenticated local writer is denied before custody is held"
    );
    {
        let state = StateLock::take(&lab.m1).unwrap();
        assert!(
            local_recipient_denied_in_roster(&lab.m1),
            "a busy state cannot erase a known roster-only denial"
        );
        assert!(local_recipient_denied_in_roster_held(&lab.m1, &state));
    }

    let custody_dir = lab.m1.dir.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let lock = aterm_uds::private_file::PrivateLock::take(
            &custody_dir,
            "commons-device.lock",
            Duration::from_secs(60),
            "the fixture takes device custody before the query",
        )
        .unwrap();
        held_tx.send(()).unwrap();
        // A hang detector, not a latency budget: release even on the old deadlock.
        let finished = done_rx.recv_timeout(Duration::from_secs(60));
        drop(lock);
        finished
    });
    held_rx.recv_timeout(Duration::from_secs(60)).unwrap();
    let denied = local_recipient_denied_in_roster(&lab.m1);
    let _ = done_tx.send(());
    let finished_while_held = holder.join().unwrap();
    assert!(
        finished_while_held.is_ok(),
        "the denial query waited on a device lock while its holder waited for the query"
    );
    assert!(denied, "observation must retain the known writer denial");
}

/// A vault that publishes no default fleet code, `m1` its admin, at a path.
struct Lab {
    root: aterm_tempfile::TempDir,
    m1: Paths,
    source: PathBuf,
    pinned: String,
    signer: [u8; 32],
}

fn lab(name: &str) -> Lab {
    lab_with_host(name, "m1")
}

fn lab_with_host(name: &str, host: &str) -> Lab {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join(format!("{name}.vault"));
    let created = test_create_unbound(&m1, source.to_str().unwrap(), None, Some(host)).unwrap();
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

#[test]
fn a_legacy_reserved_host_gets_an_admitted_discovery_name_without_reconfiguration() {
    let lab = lab_with_host("alab", crate::alab_advertised());
    lab.as_alab(|| {
        let source_before = std::fs::read(lab.m1.source_file()).unwrap();
        let identity_before = std::fs::read(lab.m1.identity_file()).unwrap();
        let healthy = load_state(&lab.m1).unwrap();
        let store = lab.root.path().join("server");
        std::fs::create_dir(&store).unwrap();
        std::fs::write(store.join("alab.vault"), lab.published()).unwrap();
        let mut role = crate::cli::ServeRole::default();
        let expected = format!(
            "alab-member-{}",
            join_code(&read_identity(&lab.m1).unwrap().unwrap().recipient())
        );
        assert!(crate::text::valid_machine_name(&expected));
        assert_ne!(expected, crate::alab_advertised());
        let decide = |role: &mut crate::cli::ServeRole| {
            role.decide(
                &crate::cli::Advertise::AlabIfHeld,
                &store,
                &lab.m1,
                &mut Vec::new(),
                Some(lab.signer),
            )
            .0
        };
        assert_eq!(decide(&mut role).as_deref(), Some(expected.as_str()));
        assert_eq!(decide(&mut role).as_deref(), Some(expected.as_str()));
        assert!(
            matches!(
                observe_alab_member_with(&lab.m1, false, || {}),
                AlabRoleObservation::Allowed {
                    own_label: None,
                    ..
                }
            ),
            "automatic installation does not invent the explicit server's discovery name"
        );
        let start = Instant::now();
        let mut watcher = crate::server::AlabNameWatcher::new();
        for (seconds, takes) in [(0, false), (90, true)] {
            assert_eq!(
                watcher.decide(
                    &crate::server::AlabNameInput {
                        own_label: &expected,
                        own_seq: Some(healthy.seq),
                        holders: &[],
                        now: start + Duration::from_secs(seconds),
                    },
                    |_| None
                ),
                takes
            );
        }

        for pending in [false, true] {
            let mut incomplete = healthy.clone();
            incomplete.failed = !pending;
            if pending {
                incomplete.pending.insert("a-file".into(), "pending".into());
            }
            save_state(&lab.m1, &incomplete).unwrap();
            assert!(decide(&mut crate::cli::ServeRole::default()).is_none());
            assert_eq!(decide(&mut role).as_deref(), Some(expected.as_str()));
        }
        save_state(&lab.m1, &healthy).unwrap();
        std::fs::remove_file(lab.m1.identity_file()).unwrap();
        assert!(decide(&mut crate::cli::ServeRole::default()).is_none());
        assert!(decide(&mut role).is_none());
        assert!(
            !lab.m1.identity_file().exists(),
            "observation never recreates identity"
        );
        fsx::write_atomic(&lab.m1.identity_file(), &identity_before, 0o600).unwrap();
        assert_eq!(decide(&mut role).as_deref(), Some(expected.as_str()));
        assert_eq!(std::fs::read(lab.m1.source_file()).unwrap(), source_before);
        assert_eq!(
            std::fs::read(lab.m1.identity_file()).unwrap(),
            identity_before
        );
    });
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

#[test]
fn server_role_survives_only_same_context_transient_reads() {
    let lab = lab("alab");
    let m2 = lab.machine("m2", false);
    let store = lab.root.path().join("server");
    let mut role = crate::cli::ServeRole::default();
    let mut pins = Vec::new();
    let advertised = crate::cli::Advertise::Named(crate::alab_advertised().to_owned());
    let mut decide = |role: &mut crate::cli::ServeRole| {
        role.decide(&advertised, &store, &m2, &mut pins, Some(lab.signer))
            .0
    };
    lab.as_alab(|| {
        let healthy = load_state(&m2).unwrap();
        let mut pending = healthy.clone();
        pending
            .pending
            .insert("a-file".into(), "in-progress".into());
        assert_eq!(decide(&mut role).as_deref(), Some("m2"));
        save_state(&m2, &pending).unwrap();
        assert_eq!(decide(&mut role).as_deref(), Some("m2"));
        assert!(decide(&mut crate::cli::ServeRole::default()).is_none());
        let mut failed = healthy.clone();
        failed.failed = true;
        save_state(&m2, &failed).unwrap();
        assert_eq!(decide(&mut role).as_deref(), Some("m2"));
        fsx::write_atomic(&m2.state_file(), b"not valid toml [", 0o600).unwrap();
        assert_eq!(decide(&mut role).as_deref(), Some("m2"));
        save_state(&m2, &healthy).unwrap();
        assert_eq!(decide(&mut role).as_deref(), Some("m2"));

        // The final context check catches a change after the eligibility reads themselves.
        let original = config(&m2).unwrap().unwrap();
        let mut changed = original.clone();
        changed.host = "m27".to_owned();
        assert!(matches!(
            observe_alab_role_with(&m2, || {
                save_config(&m2, &changed).unwrap();
            }),
            AlabRoleObservation::Unavailable(None)
        ));
        save_config(&m2, &original).unwrap();

        assert!(matches!(
            observe_alab_member_with(&m2, false, || {
                save_config(&m2, &changed).unwrap();
            }),
            AlabRoleObservation::Unavailable(None)
        ));
        save_config(&m2, &original).unwrap();

        // Pending work under a different configured source is not the retained grant.
        let mut changed = original.clone();
        changed.source = Source::Url("http://different.invalid/v1/alab".to_owned());
        save_state(&m2, &pending).unwrap();
        save_config(&m2, &changed).unwrap();
        assert!(decide(&mut role).is_none());
        save_config(&m2, &original).unwrap();
        assert!(
            decide(&mut role).is_none(),
            "context loss discarded the old grant"
        );
        save_state(&m2, &healthy).unwrap();
        assert_eq!(decide(&mut role).as_deref(), Some("m2"));

        // A new local recipient never inherits the previous recipient's role.
        let identity = fsx::read_private(&m2.identity_file(), 16 << 10)
            .unwrap()
            .unwrap();
        let other = Paths::for_home(&lab.root.path().join("other"));
        machine_identity(&other).unwrap();
        let other_identity = fsx::read_private(&other.identity_file(), 16 << 10)
            .unwrap()
            .unwrap();
        fsx::write_atomic(&m2.identity_file(), other_identity.as_slice(), 0o600).unwrap();
        save_state(&m2, &pending).unwrap();
        assert!(decide(&mut role).is_none());
        fsx::write_atomic(&m2.identity_file(), identity.as_slice(), 0o600).unwrap();
        assert!(decide(&mut role).is_none());
        save_state(&m2, &healthy).unwrap();
        assert_eq!(decide(&mut role).as_deref(), Some("m2"));

        // A scoped server whose source disappears never re-enters standalone bootstrap.
        std::fs::remove_file(m2.source_file()).unwrap();
        assert!(decide(&mut role).is_none());
        assert_eq!(
            decide(&mut crate::cli::ServeRole::default()).as_deref(),
            Some(crate::alab_advertised())
        );
        save_config(&m2, &original).unwrap();
    });
    // Explicit Off and named services are not replaced by a cached own-label decision.
    lab.as_alab(|| {
        assert!(
            role.decide(
                &crate::cli::Advertise::Off,
                &store,
                &m2,
                &mut pins,
                Some(lab.signer)
            )
            .0
            .is_none()
        );
        assert_eq!(
            role.decide(
                &crate::cli::Advertise::Named("personal".to_owned()),
                &store,
                &m2,
                &mut pins,
                Some(lab.signer)
            )
            .0
            .as_deref(),
            Some("personal")
        );
    });
    // The same non-ALab personal vault keeps its explicit name too.
    assert_eq!(
        role.decide(
            &crate::cli::Advertise::Named("personal".to_owned()),
            &store,
            &m2,
            &mut pins,
            None
        )
        .0
        .as_deref(),
        Some("personal")
    );
}

#[test]
fn denied_server_never_falls_back_during_a_failed_or_pending_apply() {
    let lab = lab("alab");
    let m2 = lab.machine("m2", false);
    lab.as_alab(|| {
        let store = lab.root.path().join("server");
        std::fs::create_dir(&store).unwrap();
        std::fs::write(store.join("alab.vault"), lab.published()).unwrap();
        let original = config(&m2).unwrap().unwrap();
        // On its first tick, store evidence is not a remembered ALab configuration. Losing
        // the source between those reads must not use the old direct-name fallback.
        assert!(
            crate::cli::ServeRole::default()
                .decide_after_store(
                    &crate::cli::Advertise::AlabIfHeld,
                    &store,
                    &m2,
                    &mut Vec::new(),
                    Some(lab.signer),
                    || {
                        std::fs::remove_file(m2.source_file()).unwrap();
                    },
                )
                .0
                .is_none()
        );
        save_config(&m2, &original).unwrap();
        let mut role = crate::cli::ServeRole::default();
        let mut pins = Vec::new();
        let mut decide = |role: &mut crate::cli::ServeRole| {
            role.decide(
                &crate::cli::Advertise::AlabIfHeld,
                &store,
                &m2,
                &mut pins,
                Some(lab.signer),
            )
            .0
        };
        assert_eq!(decide(&mut role).as_deref(), Some("m2"));
        let admin = admin_open(&lab.m1).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            b"deny_labels = [\"m2\"]\n",
        )
        .unwrap();
        admin_publish(&lab.m1, &admin, &doc).unwrap();
        sync(&m2, None).unwrap();
        let mut denied = load_state(&m2).unwrap();
        denied.failed = true;
        denied.pending.insert("a-file".into(), "in-progress".into());
        save_state(&m2, &denied).unwrap();
        assert!(matches!(
            observe_alab_role(&m2),
            AlabRoleObservation::Denied
        ));
        assert!(decide(&mut role).is_none());
        assert!(decide(&mut crate::cli::ServeRole::default()).is_none());

        denied.membership = Stored::Revoked;
        save_state(&m2, &denied).unwrap();
        assert!(matches!(
            observe_alab_role(&m2),
            AlabRoleObservation::Denied
        ));
        assert!(decide(&mut role).is_none());
    });
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
/// its `SwarmMember` row, releasing its `ai-pool.toml` warm slots, leases, and `last_heard`, and
/// rotating `aterm-fleet-code`, preserving `@<host>:<port>`, as every removal does, F16);
/// `admin_revoke_stolen` rotates it again and emits a single-line `NEEDS YOU:` provider rotation
/// notice naming exposed credentials and warm AI accounts.
///
/// NEGATIVE CONTROL: rotate the code on `stolen` alone, and the plain revoke leaves it as it was.
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
                    hw_hint: None,
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
                    hw_hint: None,
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

        // 1. Normal `admin_revoke` on m2: full cascade, the fleet code rotated at its place.
        let opened = admin_open(&lab.m1).unwrap();
        let plain = admin_revoke(&lab.m1, &opened, "m2", true).unwrap();
        assert!(plain.fleet_code_rotated);

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
        let after_plain = lab.fleet_code().unwrap();
        let after_plain = std::str::from_utf8(&after_plain).unwrap().to_owned();
        assert_ne!(after_plain, initial_code);
        assert!(after_plain.starts_with("atf1-") && after_plain.ends_with("@m1.local:7100"));
        assert!(matches!(
            steward_revoke_check(&lab.m1).unwrap(),
            StewardRevoke::Current(_)
        ));

        // 2. Stolen `admin_revoke_stolen` on m3: rotates fleet code + emits single-line NEEDS YOU.
        let opened = admin_open(&lab.m1).unwrap();
        let stolen = admin_revoke_stolen(&lab.m1, &opened, "m3", true).unwrap();
        assert!(stolen.revocation.fleet_code_rotated);
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
        assert!(rotated_str != initial_code && rotated_str != after_plain);
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
                hw_hint: None,
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
fn revoke_without_a_historical_roster_never_allocates_labels() {
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
        assert!(
            roster.members.is_empty(),
            "revocation must not allocate labels"
        );
        assert!(!after.machines().iter().any(|m| m.name == "mac-mini-m4"));
        assert!(
            !after.code_only().is_empty(),
            "the revoked recipient stays denied"
        );
    });
}

/// F16's fallback, visible: a vault with no membership log (ALab until F12b) has no member's
/// removal. `aterm members remove` refuses and names the admin's revoke, which still takes the Mac
/// out through the admin key, and the steward's revoke repair still runs there.
///
/// NEGATIVE CONTROL: in `remove_member`, drop the no-log gate (`if trusted_store(paths)?
/// .is_none()`): the refusal comes from the draft ("holds no membership log"), no longer names
/// `aterm secrets revoke`, and the `"names the admin's revoke"` assert fails.
#[test]
fn a_vault_with_no_log_still_revokes_through_the_admin_key() {
    let lab = lab("no-log-revoke");
    let _m2 = lab.machine("m2", false);
    lab.as_alab(|| {
        assert!(!removes_by_log(&lab.m1), "CONTROL: no membership log here");
        let error = remove_member(&lab.m1, "m2", false).expect_err("no member's removal");
        assert!(
            error.to_string().contains("aterm secrets revoke"),
            "names the admin's revoke: {error}"
        );
        assert!(
            !matches!(
                steward_revoke_check(&lab.m1).unwrap(),
                StewardRevoke::Idle(_)
            ),
            "the steward's revoke repair still runs on a vault with no log"
        );
        let admin = admin_open(&lab.m1).unwrap();
        admin_revoke(&lab.m1, &admin, "m2", false).expect("the admin's revoke");
        assert!(
            admin_open(&lab.m1)
                .unwrap()
                .doc
                .machines()
                .iter()
                .all(|m| m.name != "m2"),
            "m2 is out"
        );
    });
}

#[test]
fn denied_roster_and_log_excluded_admins_cannot_serve_or_claim_steward_lease() {
    let lab = lab("steward-deny-roster-and-log");
    let m2 = lab.machine("m2", true);
    let m1_rec = machine_identity(&lab.m1).unwrap().recipient().to_bech32();
    let m2_rec = machine_identity(&m2).unwrap().recipient().to_bech32();

    lab.as_alab(|| {
        let opened = admin_open(&lab.m1).unwrap();
        let mut doc = opened.doc.clone();
        let policy = crate::swarm_roster::SwarmPolicy {
            network_id: Some("net-alab".to_owned()),
            servers: Vec::new(),
            stewards: vec![
                crate::swarm_roster::StewardPolicy {
                    label: "m1".to_owned(),
                    rank: 1,
                    always_on: true,
                },
                crate::swarm_roster::StewardPolicy {
                    label: "m2".to_owned(),
                    rank: 2,
                    always_on: true,
                },
            ],
        };
        let admins = doc.admins();
        doc.set_entry(
            &Audience::Hosts(admins),
            crate::swarm_roster::POLICY_ENTRY,
            policy.to_toml().as_bytes(),
        )
        .unwrap();
        let roster = crate::swarm_roster::SwarmRoster {
            members: vec![
                crate::swarm_roster::SwarmMember {
                    label: "m1".into(),
                    vault_name: "m1".into(),
                    recipient: m1_rec.clone(),
                    node_id: None,
                    node_key: None,
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1,
                    claimed_label: None,
                    claimed_at_ms: None,
                },
                crate::swarm_roster::SwarmMember {
                    label: "m2".into(),
                    vault_name: "m2".into(),
                    recipient: m2_rec.clone(),
                    node_id: None,
                    node_key: Some("de".repeat(32)),
                    aliases: Vec::new(),
                    hint: None,
                    hw_hint: None,
                    admitted_ms: 1,
                    claimed_label: None,
                    claimed_at_ms: None,
                },
            ],
            deny_labels: Vec::new(),
            deny_nodes: vec!["de".repeat(32)],
        };
        doc.set_entry(
            &Audience::Everyone,
            crate::swarm_roster::ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish(&lab.m1, &opened, &doc).unwrap();
        sync(&lab.m1, None).unwrap();
        sync(&m2, None).unwrap();

        // `m2` is denied by `deny_nodes` in `aterm-swarm-roster`: it must not serve `_alab-vault._tcp`
        // or claim the steward lease by falling back to `doc.machines()` or `config.host`.
        assert_eq!(steward_alab_policy_role(&m2), None);
        assert!(
            check_standby_eligibility(&m2, Some("net-alab"))
                .unwrap()
                .is_err()
        );
        let m2_clean_state = load_state(&m2).unwrap();
        let mut m2_failed_state = m2_clean_state.clone();
        m2_failed_state.failed = true;
        save_state(&m2, &m2_failed_state).unwrap();
        assert_eq!(
            steward_alab_policy_role(&m2),
            None,
            "failed apply state must not let denied m2 fall back to config.host for server role"
        );
        assert!(
            check_standby_eligibility(&m2, Some("net-alab"))
                .unwrap()
                .is_err()
        );
        save_state(&m2, &m2_clean_state).unwrap();
        assert!(matches!(
            steward_lease_check(&m2, Some("net-alab"), 1_760_000_000).unwrap(),
            StewardLease::Idle(_)
        ));
        assert!(matches!(
            steward_ensure_lease(&m2, Some("net-alab"), 1_760_000_000).unwrap(),
            StewardLease::Idle(_)
        ));

        // Positive control: `m1` is not denied and is eligible.
        assert_eq!(
            steward_alab_policy_role(&lab.m1),
            Some((true, Some("m1".to_owned())))
        );
        assert!(matches!(
            steward_lease_check(&lab.m1, Some("net-alab"), 1_760_000_000).unwrap(),
            StewardLease::Due { .. }
        ));
        // A symlinked `network-id` fallback in `~/.config/aterm/secrets` is refused rather than
        // followed when `observed_network_id` is `None`.
        let forged_net = lab.root.path().join("forged-net");
        std::fs::write(&forged_net, "net-alab\n").unwrap();
        let net_link = lab.m1.dir.join("network-id");
        std::os::unix::fs::symlink(&forged_net, &net_link).unwrap();
        assert!(matches!(
            steward_lease_check(&lab.m1, None, 1_760_000_000).unwrap(),
            StewardLease::Idle(_)
        ));
        std::fs::remove_file(&net_link).unwrap();

        // Now publish a lease held by denied `m2` with 6 hours remaining: `swarm_lease_holder`
        // must return `None` for the denied holder, and `m1` must immediately take over at
        // `epoch + 1` without waiting 6 hours for `m2`'s TTL to expire.
        let opened = admin_open(&lab.m1).unwrap();
        let mut doc = opened.doc.clone();
        let m2_lease = crate::swarm_roster::SwarmLease {
            holder: "m2".into(),
            epoch: 3,
            until: 1_760_021_600,
        };
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_LEASE_ENTRY,
            m2_lease.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish(&lab.m1, &opened, &doc).unwrap();
        sync(&lab.m1, None).unwrap();

        assert_eq!(
            crate::client::swarm_lease_holder(&lab.m1).unwrap(),
            None,
            "swarm_lease_holder must not return a denied lease holder"
        );
        assert!(matches!(
            steward_lease_check(&lab.m1, Some("net-alab"), 1_760_000_000).unwrap(),
            StewardLease::Due { makes, .. } if makes.contains("epoch 4") && makes.contains("m1")
        ));
        assert!(matches!(
            steward_ensure_lease(&lab.m1, Some("net-alab"), 1_760_000_000).unwrap(),
            StewardLease::Done(said) if said.contains("epoch 4") && said.contains("m1")
        ));
        sync(&lab.m1, None).unwrap();
        assert_eq!(
            crate::client::swarm_lease_holder(&lab.m1)
                .unwrap()
                .as_deref(),
            Some("m1")
        );

        // Positive control for renumbered member: if `m1`'s local `source.toml` still records a
        // pre-renumber `host = "m5"` that is in `deny_labels`, while `m1`'s roster row for its
        // recipient has active un-denied `label = "m1"`, `local_recipient_denied_in_roster` and
        // `read_reserved_entry` must honor the active roster row rather than denying `m1` via its
        // stale `config.host`.
        let opened = admin_open(&lab.m1).unwrap();
        let mut doc = opened.doc.clone();
        let mut roster_with_m5_denied = crate::swarm_roster::SwarmRoster::from_document(&doc)
            .unwrap()
            .unwrap();
        roster_with_m5_denied.deny_labels.push("m5".to_owned());
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            roster_with_m5_denied.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish(&lab.m1, &opened, &doc).unwrap();
        sync(&lab.m1, None).unwrap();

        let mut m1_cfg = config(&lab.m1).unwrap().unwrap();
        m1_cfg.host = "m5".to_owned();
        save_config(&lab.m1, &m1_cfg).unwrap();
        assert!(
            !local_recipient_denied_in_roster(&lab.m1),
            "active roster member m1 must not be denied by stale config.host m5"
        );
        assert!(
            entry::read_reserved_entry(&lab.m1, &m1_cfg, crate::SWARM_LEASE_ENTRY)
                .unwrap()
                .is_some(),
            "active roster member m1 must still read reserved entries when config.host is stale"
        );
    });
}
