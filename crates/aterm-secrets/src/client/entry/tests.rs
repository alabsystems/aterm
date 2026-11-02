// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fixture {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    source: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("admin"));
        let source = root.path().join("test.vault");
        create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
        let f = Self {
            _root: root,
            paths,
            source,
        };
        f.publish(|doc| {
            doc.add_machine(
                "peer",
                &Identity::from_secret_bytes([9; 32]).recipient().to_bech32(),
            )
            .unwrap();
            doc.set_entry(&Audience::Everyone, "token", b"inert-authentic-token")
                .unwrap();
            doc.set_entry(&Audience::Everyone, "other", b"inert-other")
                .unwrap();
        });
        f
    }

    fn publish(&self, change: impl FnOnce(&mut Document)) {
        let admin = admin_open(&self.paths).unwrap();
        let mut doc = admin.doc.clone();
        change(&mut doc);
        admin_publish(&self.paths, &admin, &doc).unwrap();
    }

    fn read_token(&self) -> Option<VerifiedEntry> {
        read_verified_entry(&self.paths, &config(&self.paths).unwrap().unwrap(), "token").unwrap()
    }

    fn tracked(&self) -> Vec<PathBuf> {
        vec![
            self.paths.state_file(),
            self.paths.source_file(),
            self.paths.cache_file(),
            self.paths.identity_file(),
            self.paths.lock_file(),
            self.source.clone(),
            materialize::target_path(&self.paths.home, "token").unwrap(),
            materialize::target_path(&self.paths.home, "other").unwrap(),
        ]
    }

    fn bytes(&self) -> Vec<Option<Vec<u8>>> {
        self.tracked()
            .iter()
            .map(|p| std::fs::read(p).ok())
            .collect()
    }
}

fn set_state(paths: &Paths, change: impl FnOnce(&mut State)) {
    let mut state = load_state(paths).unwrap();
    change(&mut state);
    save_state(paths, &state).unwrap();
}

#[test]
fn authenticated_reads_are_local_redacted_and_bound_to_one_entry() {
    let f = Fixture::new();
    let before = f.bytes();
    let entry = f.read_token().unwrap();
    assert_eq!(entry.value().as_slice(), b"inert-authentic-token");
    assert_eq!(before, f.bytes());
    let shown = format!("{entry:?} {:?}", entry.snapshot());
    for private in [
        "inert-authentic-token",
        "test.vault",
        "admin",
        "fingerprint",
        "seq",
    ] {
        assert!(!shown.contains(private), "{shown}");
    }
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "other", b"changed-other")
            .unwrap()
    });
    assert_eq!(
        entry.snapshot.read(&f.paths).unwrap().unwrap().value,
        entry.value
    );
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "token", b"changed-token")
            .unwrap()
    });
    assert!(entry.snapshot.read(&f.paths).unwrap().is_none());
    assert_eq!(f.read_token().unwrap().value.as_slice(), b"changed-token");
}

#[test]
fn authenticated_entry_versions_retain_absence_and_reject_rollback_forks() {
    let f = Fixture::new();
    let config = config(&f.paths).unwrap().unwrap();
    let first = observe_verified_entry(&f.paths, &config, "token")
        .unwrap()
        .unwrap();
    f.publish(|doc| {
        doc.unset_entry(&Audience::Everyone, "token").unwrap();
    });
    let absent = observe_verified_entry(&f.paths, &config, "token")
        .unwrap()
        .unwrap();
    assert!(absent.entry.is_none());
    assert!(absent.version.not_before(&first.version));
    assert!(!first.version.not_before(&absent.version));
    let mut fork = absent.version.clone();
    fork.envelope[0] ^= 1;
    assert!(!fork.not_before(&absent.version));
    assert!(!absent.version.not_before(&fork));
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "token", b"inert-authentic-token")
            .unwrap();
    });
    let restored = observe_verified_entry(&f.paths, &config, "token")
        .unwrap()
        .unwrap();
    assert!(restored.version.not_before(&absent.version));
    assert!(
        restored
            .entry
            .unwrap()
            .snapshot()
            .continues(first.entry.unwrap().snapshot())
    );
    let other = observe_verified_entry(&f.paths, &config, "other")
        .unwrap()
        .unwrap();
    assert!(!other.version.not_before(&restored.version));
    std::fs::write(
        materialize::target_path(&f.paths.home, "token").unwrap(),
        b"forged",
    )
    .unwrap();
    let forged = observe_verified_entry(&f.paths, &config, "token")
        .unwrap()
        .unwrap();
    assert!(forged.entry.is_none());
    assert!(forged.version.not_before(&restored.version));
}

#[test]
fn entry_versions_require_retained_log_facts_even_with_an_unchanged_envelope() {
    let f = Fixture::new();
    let mut first = f.read_token().unwrap().snapshot().version();
    first.log_ids = Some(std::sync::Arc::new([[1; 32]].into_iter().collect()));
    let mut extended = first.clone();
    extended.log_ids = Some(std::sync::Arc::new(
        [[1; 32], [2; 32]].into_iter().collect(),
    ));
    assert!(extended.not_before(&first));
    assert!(!first.not_before(&extended));
    let mut replaced = extended.clone();
    replaced.log_ids = Some(std::sync::Arc::new(
        [[2; 32], [3; 32], [4; 32]].into_iter().collect(),
    ));
    assert!(!replaced.not_before(&first));
    replaced.log_ids = None;
    assert!(!replaced.not_before(&first));
}

pub(crate) fn version_transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    version_transition_with(before, action, false)
}

fn version_transition_with(
    before: &aterm_spec::interp::State,
    action: &str,
    bypass: bool,
) -> aterm_spec::interp::State {
    let f = Fixture::new();
    let mut floor = f.read_token().unwrap().snapshot().version();
    floor.seq = 1;
    floor.envelope = [1; 32];
    floor.log_ids = Some(std::sync::Arc::new([[1; 32]].into_iter().collect()));
    let mut candidate = floor.clone();
    let scenario = match action {
        "Same" => 1,
        "Advance" => {
            candidate.seq = 2;
            2
        }
        "Rollback" => {
            candidate.seq = 0;
            3
        }
        "Fork" => {
            candidate.envelope = [2; 32];
            4
        }
        "LogExtend" => {
            candidate.log_ids = Some(std::sync::Arc::new(
                [[1; 32], [2; 32]].into_iter().collect(),
            ));
            5
        }
        "LogDrop" => {
            candidate.log_ids = None;
            6
        }
        "LogSwap" => {
            candidate.log_ids = Some(std::sync::Arc::new(
                [[2; 32], [3; 32]].into_iter().collect(),
            ));
            7
        }
        "Context" => {
            candidate.name = "other".into();
            8
        }
        _ => unreachable!(),
    };
    let accepted = if bypass {
        candidate.seq >= floor.seq
    } else {
        candidate.not_before(&floor)
    };
    let mut after = before.clone();
    after.insert("scenario", scenario);
    after.insert("accepted", i64::from(accepted));
    after
}

#[test]
fn entry_version_conforms_and_catches_sequence_only_comparisons() {
    let model = aterm_models_db::entry_version_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    aterm_spec::check::each_on(aterm_spec::fs_lane::PLACES, &model.actions, |action| {
        let next = version_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    assert!(!model.check_invariant(
        "Retained",
        &version_transition_with(&initial, "LogDrop", true)
    ));
}

#[test]
fn reserved_entries_are_authenticated_without_becoming_secret_files() {
    let f = Fixture::new();
    f.publish(|doc| {
        doc.set_entry(
            &Audience::Everyone,
            "aterm-fleet-code",
            b"inert-reserved-code",
        )
        .unwrap()
    });
    let expected = config(&f.paths).unwrap().unwrap();
    let entry = read_reserved_entry(&f.paths, &expected, "aterm-fleet-code")
        .unwrap()
        .expect("signed reserved entry");
    assert_eq!(entry.value().as_slice(), b"inert-reserved-code");
    assert!(!f.paths.home.join(".secrets/aterm-fleet-code").exists());
    assert!(read_reserved_entry(&f.paths, &expected, "token").is_err());
    assert!(read_verified_entry(&f.paths, &expected, "aterm-fleet-code").is_err());
}

#[test]
fn log_backed_roster_and_admin_reads_terminate_and_keep_their_privileges() {
    let f = Fixture::new();
    f.publish(|doc| {
        doc.set_entry(
            &Audience::Everyone,
            fleet::FLEET_CODE_ENTRY,
            b"inert-reserved-code",
        )
        .unwrap()
    });
    admin_seed_swarm_roster(&f.paths, &[]).unwrap();
    seed_vault_genesis(&f.paths, &f._root.path().join("test.genesis")).unwrap();
    admin_mirror_log(&f.paths, &[]).unwrap();

    let expected = config(&f.paths).unwrap().unwrap();
    assert!(
        log_entries::trusted_local_log(&f.paths, &expected)
            .unwrap()
            .is_some(),
        "the admin verdict must consult a seeded membership log"
    );
    let roster = read_reserved_entry(&f.paths, &expected, crate::SWARM_ROSTER_ENTRY)
        .unwrap()
        .unwrap();
    assert!(
        crate::swarm_roster::SwarmRoster::from_toml(
            std::str::from_utf8(roster.value().as_slice()).unwrap()
        )
        .unwrap()
        .member_by_recipient(
            &read_identity(&f.paths)
                .unwrap()
                .unwrap()
                .recipient()
                .to_bech32()
        )
        .is_some()
    );
    let fleet = fleet::reserved_fleet(&f.paths, &expected).unwrap().unwrap();
    assert!(
        fleet.admin,
        "an explicit admin read keeps the real privilege"
    );
    assert_eq!(fleet.code.as_slice(), b"inert-reserved-code");
    assert!(f.read_token().is_some());
}

#[test]
fn a_log_backed_denied_admin_can_read_the_roster_but_has_no_admin_privilege() {
    for deny_node in [false, true] {
        let macs = crate::client::log_entries_tests::two_macs_with(
            &[
                (Audience::Everyone, fleet::FLEET_CODE_ENTRY, b"inert-code"),
                (Audience::Everyone, "token", b"inert-token"),
            ],
            true,
        );
        admin_seed_swarm_roster(&macs.m22, &[]).unwrap();
        sync(&macs.m7, None).unwrap();
        let expected = config(&macs.m7).unwrap().unwrap();
        assert!(
            fleet::reserved_fleet(&macs.m7, &expected)
                .unwrap()
                .unwrap()
                .admin
        );
        let admin = admin_open(&macs.m22).unwrap();
        let mut doc = admin.doc.clone();
        let mut roster = crate::swarm_roster::SwarmRoster::from_document(&doc)
            .unwrap()
            .unwrap();
        let recipient = read_identity(&macs.m7)
            .unwrap()
            .unwrap()
            .recipient()
            .to_bech32();
        let member = roster
            .members
            .iter_mut()
            .find(|m| m.recipient == recipient)
            .unwrap();
        let node_key = "22".repeat(32);
        member.node_key = Some(node_key.clone());
        if deny_node {
            roster.deny_nodes.push(node_key.clone());
        } else {
            roster.deny_labels.push(member.label.clone());
        }
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish(&macs.m22, &admin, &doc).unwrap();
        sync(&macs.m7, None).unwrap();

        let identity = read_identity(&macs.m7).unwrap().unwrap();
        let cached = envelope::parse(&std::fs::read(macs.m7.cache_file()).unwrap()).unwrap();
        assert!(
            age::decrypt(
                std::slice::from_ref(&identity),
                cached.admin_locker().unwrap().armored.as_bytes()
            )
            .is_ok(),
            "the denied member still decrypts the admin locker, so key absence cannot explain refusal"
        );
        assert!(
            log_entries::trusted_local_log(&macs.m7, &expected)
                .unwrap()
                .is_some()
        );
        let (_, is_admin) =
            read_reserved_entry_and_admin(&macs.m7, &expected, crate::SWARM_ROSTER_ENTRY)
                .unwrap()
                .unwrap();
        assert!(
            !is_admin,
            "a readable roster must not grant the denied admin's privilege"
        );
        assert!(
            fleet::swarm_denied_nodes(&macs.m7)
                .unwrap()
                .unwrap()
                .contains(&node_key)
        );
        assert!(
            read_reserved_entry(&macs.m7, &expected, fleet::FLEET_CODE_ENTRY)
                .unwrap()
                .is_none()
        );
        assert!(
            fleet::reserved_fleet(&macs.m7, &expected)
                .unwrap()
                .is_none()
        );
        assert!(
            read_verified_entry(&macs.m7, &expected, "token")
                .unwrap()
                .is_none()
        );
        macs.server.stop();
    }
}

#[test]
fn a_log_removed_admin_cannot_reuse_its_cached_admin_locker() {
    let macs = crate::client::log_entries_tests::two_macs_with(
        &[(Audience::Everyone, fleet::FLEET_CODE_ENTRY, b"inert-code")],
        true,
    );
    admin_seed_swarm_roster(&macs.m22, &[]).unwrap();
    sync(&macs.m7, None).unwrap();
    let expected = config(&macs.m7).unwrap().unwrap();
    assert!(
        fleet::reserved_fleet(&macs.m7, &expected)
            .unwrap()
            .unwrap()
            .admin
    );
    let cached = std::fs::read(macs.m7.cache_file()).unwrap();
    let (writer, actor, incarnation) = crate::client::log_entries_tests::member_key(&macs.m22);
    let (_, removed, _) = crate::client::log_entries_tests::member_key(&macs.m7);
    let mut log = load_records(&macs.m7).unwrap().log().unwrap().clone();
    let record = writer
        .device()
        .with_seed(|seed| {
            log.draft(
                &actor,
                100,
                crate::membership_log::Body::Remove {
                    member: removed,
                    cutoff: *log.genesis().id(),
                },
            )
            .sign(seed, incarnation)
        })
        .unwrap();
    log.insert(record.clone()).unwrap();
    save_records(&macs.m7, &crate::membership_log::encode_records(&[record])).unwrap();
    assert_eq!(cached, std::fs::read(macs.m7.cache_file()).unwrap());
    let identity = read_identity(&macs.m7).unwrap().unwrap();
    let envelope = envelope::parse(&cached).unwrap();
    assert!(
        age::decrypt(
            std::slice::from_ref(&identity),
            envelope.admin_locker().unwrap().armored.as_bytes()
        )
        .is_ok()
    );
    assert!(log.recipient_is_removed(&identity.recipient()));
    assert!(
        fleet::reserved_fleet(&macs.m7, &expected)
            .unwrap()
            .is_none()
    );
    assert!(
        read_reserved_entry_and_admin(&macs.m7, &expected, crate::SWARM_ROSTER_ENTRY)
            .unwrap()
            .is_none()
    );
    macs.server.stop();
}

#[test]
fn an_update_removes_the_fleet_code_file_an_older_build_wrote_at_the_same_version() {
    let f = Fixture::new();
    f.publish(|doc| {
        doc.set_entry(
            &Audience::Everyone,
            "aterm-fleet-code",
            b"inert-reserved-code",
        )
        .unwrap()
    });
    // What a build that did not reserve the name left: the file, and its manifest record.
    let path = f.paths.home.join(".secrets/aterm-fleet-code");
    fsx::write_atomic(&path, b"inert-reserved-code", 0o600).unwrap();
    let mut state = load_state(&f.paths).unwrap();
    let seq = state.seq;
    state.manifest.files.insert(
        "aterm-fleet-code".into(),
        materialize::digest_hex(b"inert-reserved-code"),
    );
    save_state(&f.paths, &state).unwrap();
    match sync(&f.paths, None).unwrap() {
        SyncOutcome::Applied {
            seq: applied,
            report,
        } => {
            assert_eq!(applied, seq, "no new version was published");
            assert_eq!(report.removed, ["aterm-fleet-code"], "{report:?}");
            assert!(report.failed.is_empty(), "{report:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!path.exists());
    assert!(
        !load_state(&f.paths)
            .unwrap()
            .manifest
            .files
            .contains_key("aterm-fleet-code")
    );
    assert!(matches!(
        sync(&f.paths, None).unwrap(),
        SyncOutcome::Unchanged { .. }
    ));
    let expected = config(&f.paths).unwrap().unwrap();
    let entry = read_reserved_entry(&f.paths, &expected, "aterm-fleet-code")
        .unwrap()
        .expect("the vault still holds the code");
    assert_eq!(entry.value().as_slice(), b"inert-reserved-code");
}

/// Holds a file immutable (`chflags uchg`, as its owner can), so no unlink of it succeeds until
/// this drops, unwinding included.
#[cfg(target_os = "macos")]
struct Immutable(PathBuf);

#[cfg(target_os = "macos")]
impl Immutable {
    fn set(path: &Path) -> Self {
        assert!(Self::chflags("uchg", path), "{}", path.display());
        Self(path.to_path_buf())
    }

    fn chflags(flags: &str, path: &Path) -> bool {
        aterm_uds::spawnfd::command("/usr/bin/chflags")
            .arg(flags)
            .arg(path)
            .status()
            .is_ok_and(|status| status.success())
    }
}

#[cfg(target_os = "macos")]
impl Drop for Immutable {
    fn drop(&mut self) {
        Self::chflags("nouchg", &self.0);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_fleet_code_file_whose_removal_fails_keeps_no_read_back_and_goes_on_a_later_pass() {
    let f = Fixture::new();
    f.publish(|doc| {
        doc.set_entry(
            &Audience::Everyone,
            "aterm-fleet-code",
            b"inert-reserved-code",
        )
        .unwrap()
    });
    let path = f.paths.home.join(".secrets/aterm-fleet-code");
    fsx::write_atomic(&path, b"inert-reserved-code", 0o600).unwrap();
    set_state(&f.paths, |s| {
        s.manifest.files.insert(
            "aterm-fleet-code".into(),
            materialize::digest_hex(b"inert-reserved-code"),
        );
    });
    let immutable = Immutable::set(&path);
    let stuck = sync(&f.paths, None);
    drop(immutable);
    match stuck.unwrap() {
        SyncOutcome::Applied { report, .. } => {
            let failed: Vec<&str> = report.failed.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(failed, ["aterm-fleet-code"], "{report:?}");
            assert!(report.removed.is_empty(), "{report:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(path.exists(), "the removal failed");
    let state = load_state(&f.paths).unwrap();
    assert!(
        !state.failed,
        "every entry the vault holds is in place: no read of the applied copy is kept back"
    );
    assert!(
        state.manifest.files.contains_key("aterm-fleet-code"),
        "kept, so the next pass tries again"
    );
    let expected = config(&f.paths).unwrap().unwrap();
    let code = fleet::reserved_code(&f.paths, &expected).unwrap();
    assert_eq!(
        code.as_ref().map(SecretBytes::as_slice),
        Some(b"inert-reserved-code".as_slice())
    );
    match sync(&f.paths, None).unwrap() {
        SyncOutcome::Applied { report, .. } => {
            assert_eq!(report.removed, ["aterm-fleet-code"], "{report:?}");
            assert!(report.failed.is_empty(), "{report:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!path.exists());
}

#[test]
fn a_fleet_code_read_before_the_apply_finished_says_so_rather_than_none() {
    let f = Fixture::new();
    let expected = config(&f.paths).unwrap().unwrap();
    assert!(
        fleet::reserved_code(&f.paths, &expected).unwrap().is_none(),
        "the vault publishes none"
    );
    f.publish(|doc| {
        doc.set_entry(
            &Audience::Everyone,
            "aterm-fleet-code",
            b"inert-reserved-code",
        )
        .unwrap()
    });
    let held = load_state(&f.paths).unwrap();
    for (fault, said) in [
        ("failed", "did not finish"),
        ("pending", "did not finish"),
        ("busy", "applying on this machine now"),
    ] {
        let guard = match fault {
            "failed" => {
                set_state(&f.paths, |s| s.failed = true);
                None
            }
            "pending" => {
                set_state(&f.paths, |s| {
                    s.pending
                        .insert("other".into(), materialize::digest_hex(b"pending"));
                });
                None
            }
            "busy" => Some(StateLock::take(&f.paths).unwrap()),
            _ => unreachable!(),
        };
        let read = fleet::reserved_code(&f.paths, &expected);
        drop(guard);
        match read {
            Err(error) => assert!(error.to_string().contains(said), "{fault}: {error}"),
            Ok(code) => panic!("{fault}: read as {:?}", code.map(|_| "a code")),
        }
        save_state(&f.paths, &held).unwrap();
    }
    assert!(fleet::reserved_code(&f.paths, &expected).unwrap().is_some());
}

#[test]
fn reserved_fleet_distinguishes_admin_locker_holders_from_members() {
    let root = aterm_tempfile::tempdir().unwrap();
    let m1 = Paths::for_home(&root.path().join("m1"));
    let m2 = Paths::for_home(&root.path().join("m2"));
    let source = root.path().join("test.vault");
    let created = test_create_unbound(&m1, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let recipient = machine_identity(&m2).unwrap().recipient().to_bech32();
    let admin = admin_cached(&m1).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine("m2", &recipient).unwrap();
    doc.set_entry(
        &Audience::Everyone,
        fleet::FLEET_CODE_ENTRY,
        b"inert-reserved-code",
    )
    .unwrap();
    admin_publish_approved(&m1, &admin, &doc, &[recipient]).unwrap();
    join(
        &m2,
        &JoinOptions {
            source: format!("{}#{}", source.display(), created.signer),
            host: Some("m2".into()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| false,
            password: crate::client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();

    let m1_cfg = config(&m1).unwrap().unwrap();
    let m2_cfg = config(&m2).unwrap().unwrap();
    let m1_fleet = fleet::reserved_fleet(&m1, &m1_cfg).unwrap().unwrap();
    let m2_fleet = fleet::reserved_fleet(&m2, &m2_cfg).unwrap().unwrap();
    assert!(
        m1_fleet.admin,
        "m1 created the vault and holds its admin locker"
    );
    assert!(
        !m2_fleet.admin,
        "NEGATIVE CONTROL: non-admin member m2 holds the fleet code without the admin locker"
    );
    assert_eq!(m1_fleet.code.as_slice(), b"inert-reserved-code");
    assert_eq!(m2_fleet.code.as_slice(), b"inert-reserved-code");
    assert!(
        !format!("{m1_fleet:?}").contains("inert-reserved-code"),
        "Debug never exposes the fleet code"
    );

    // Promoting m2 to admin and syncing makes m2 report `admin = true`; removing m2 returns it to
    // `admin = false`.
    let admin = admin_open(&m1).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_admin("m2").unwrap();
    admin_publish(&m1, &admin, &doc).unwrap();
    sync(&m2, None).unwrap();
    assert!(
        fleet::reserved_fleet(&m2, &m2_cfg).unwrap().unwrap().admin,
        "promoted second admin opens the admin locker"
    );

    let admin = admin_open(&m1).unwrap();
    let mut doc = admin.doc.clone();
    assert!(doc.remove_admin("m2").unwrap());
    admin_publish(&m1, &admin, &doc).unwrap();
    sync(&m2, None).unwrap();
    let m2_cfg = config(&m2).unwrap().unwrap();
    assert!(
        !fleet::reserved_fleet(&m2, &m2_cfg).unwrap().unwrap().admin,
        "removed admin no longer opens the admin locker"
    );

    // Denying m2 in aterm-swarm-roster refuses reserved_fleet and read_verified_entry on m2 while
    // still permitting SWARM_ROSTER_ENTRY itself to be read.
    let admin = admin_open(&m1).unwrap();
    let mut doc = admin.doc.clone();
    let mut roster = crate::swarm_roster::SwarmRoster::from_document(&doc)
        .unwrap()
        .unwrap_or_default();
    roster.deny_labels.push("m2".to_string());
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&m1, &admin, &doc).unwrap();
    sync(&m2, None).unwrap();
    let m2_cfg = config(&m2).unwrap().unwrap();
    assert!(
        fleet::reserved_fleet(&m2, &m2_cfg).unwrap().is_none(),
        "denied member must not read reserved_fleet"
    );
    assert!(
        read_reserved_entry(&m2, &m2_cfg, fleet::FLEET_CODE_ENTRY)
            .unwrap()
            .is_none(),
        "denied member must not read non-roster reserved entries"
    );
    assert!(
        read_reserved_entry(&m2, &m2_cfg, crate::SWARM_ROSTER_ENTRY)
            .unwrap()
            .is_some(),
        "SWARM_ROSTER_ENTRY itself must remain readable so swarm_roster can observe the denial"
    );
    assert!(
        !materialization_ready(&m2).unwrap(),
        "denied member must not report materialization_ready"
    );
    assert!(matches!(
        observe_materialization(&m2, &m2_cfg, || Ok(())).unwrap(),
        MaterializationObservation::Blocked(MaterializationBlocker::Revoked)
    ));
    assert!(matches!(
        observe_commons(&m2).unwrap(),
        crate::commons::Observation::Unavailable(crate::commons::Unavailable::Revoked)
    ));
}

#[test]
fn local_if_absent_files_are_not_promoted_to_authenticated_entries() {
    let f = Fixture::new();
    let name = "local-value";
    let path = materialize::target_path(&f.paths.home, name).unwrap();
    fsx::write_atomic(&path, b"same-inert-value", 0o600).unwrap();
    f.publish(|doc| {
        doc.set_entry_with(&Audience::Everyone, name, b"same-inert-value", true, None)
            .unwrap()
    });
    let expected = config(&f.paths).unwrap().unwrap();
    assert!(
        read_verified_entry(&f.paths, &expected, name)
            .unwrap()
            .is_none()
    );
    f.publish(|doc| {
        doc.set_entry_with(
            &Audience::Everyone,
            "filled-value",
            b"filled-inert-value",
            true,
            None,
        )
        .unwrap()
    });
    assert_eq!(
        read_verified_entry(&f.paths, &expected, "filled-value")
            .unwrap()
            .unwrap()
            .value
            .as_slice(),
        b"filled-inert-value"
    );
}

fn adopted_signing_fixture(anchored: bool) -> Fixture {
    let f = Fixture::new();
    let key = materialize::target_path(&f.paths.home, "~/.aterm/apple/devid-existing.key").unwrap();
    fsx::ensure_private_dir(key.parent().unwrap()).unwrap();
    fsx::write_atomic(&key, b"inert-existing-key", 0o600).unwrap();
    f.publish(|doc| {
        let anchor = "~/.aterm/apple/devid-existing.cer";
        if anchored {
            doc.set_entry_with(
                &Audience::Everyone,
                anchor,
                b"inert-certificate",
                true,
                None,
            )
            .unwrap();
        }
        doc.set_entry_with(
            &Audience::Everyone,
            "~/.aterm/apple/devid-existing.key",
            b"inert-existing-key",
            anchored,
            anchored.then_some(anchor),
        )
        .unwrap();
    });
    f
}

#[test]
fn signing_reader_accepts_identical_adopted_bytes_without_claiming_ownership() {
    let f = adopted_signing_fixture(false);
    let name = "~/.aterm/apple/devid-existing.key";
    let expected = config(&f.paths).unwrap().unwrap();
    let key = materialize::target_path(&f.paths.home, name).unwrap();
    let state = load_state(&f.paths).unwrap();
    assert!(!state.manifest.files.contains_key(name));
    assert!(state.own.as_ref().unwrap().contains(name));
    let before = (
        std::fs::read(f.paths.state_file()).unwrap(),
        std::fs::read(&key).unwrap(),
    );
    assert!(
        read_verified_entry(&f.paths, &expected, name)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        read_signing_material(&f.paths, &expected, name)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"inert-existing-key"
    );
    assert_eq!(
        before,
        (
            std::fs::read(f.paths.state_file()).unwrap(),
            std::fs::read(&key).unwrap()
        )
    );
    for other in [
        "token",
        "~/.aterm/apple/devid-nested/other.key",
        "~/.aterm/apple/devid-existing.txt",
    ] {
        assert!(read_signing_material(&f.paths, &expected, other).is_err());
    }
}

fn adopted_publication_fixture(name: &str) -> Fixture {
    let f = Fixture::new();
    let path = materialize::target_path(&f.paths.home, name).unwrap();
    fsx::write_atomic(&path, b"inert-publication-token", 0o600).unwrap();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, name, b"inert-publication-token")
            .unwrap();
    });
    f
}

#[test]
fn publication_reader_accepts_only_authenticated_equal_adopted_tokens_without_claiming_them() {
    for name in ["gh_access_token", "gh_access_token_alabsystems"] {
        let f = adopted_publication_fixture(name);
        let expected = config(&f.paths).unwrap().unwrap();
        let state = load_state(&f.paths).unwrap();
        assert!(!state.manifest.files.contains_key(name));
        assert!(state.own.as_ref().unwrap().contains(name));
        let before = std::fs::read(f.paths.state_file()).unwrap();
        assert!(
            read_verified_entry(&f.paths, &expected, name)
                .unwrap()
                .is_none()
        );
        let entry = read_publication_material(&f.paths, &expected, name)
            .unwrap()
            .unwrap();
        assert_eq!(entry.value().as_slice(), b"inert-publication-token");
        assert!(entry.snapshot().read(&f.paths).unwrap().is_none());
        assert!(
            entry
                .snapshot()
                .read_publication(&f.paths)
                .unwrap()
                .is_some()
        );
        assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), before);
        for unrelated in [
            "token",
            "aterm-commons",
            "~/.aterm/apple/devid-existing.key",
        ] {
            assert!(read_publication_material(&f.paths, &expected, unrelated).is_err());
        }
    }
}

#[test]
fn publication_token_and_snapshot_refuse_stale_foreign_revoked_or_unsafe_material() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let name = "gh_access_token";
    for fault in [
        "mismatch", "manifest", "audience", "revoked", "source", "public", "link", "hardlink",
        "stale", "failed", "pending",
    ] {
        let f = adopted_publication_fixture(name);
        let expected = config(&f.paths).unwrap().unwrap();
        let entry = read_publication_material(&f.paths, &expected, name)
            .unwrap()
            .unwrap();
        let path = materialize::target_path(&f.paths.home, name).unwrap();
        match fault {
            "mismatch" => fsx::write_atomic(&path, b"foreign", 0o600).unwrap(),
            "manifest" => set_state(&f.paths, |s| {
                s.manifest
                    .files
                    .insert(name.into(), materialize::digest_hex(b"foreign"));
            }),
            "audience" => f.publish(|doc| {
                doc.unset_entry(&Audience::Everyone, name).unwrap();
                doc.set_entry(
                    &Audience::Hosts(vec!["peer".into()]),
                    name,
                    b"inert-publication-token",
                )
                .unwrap();
            }),
            "revoked" => set_state(&f.paths, |s| s.membership = Stored::Revoked),
            "source" => {
                let mut changed = expected.clone();
                changed.source = Source::Path(f.paths.home.join("different.vault"));
                save_config(&f.paths, &changed).unwrap();
            }
            "public" => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap()
            }
            "link" => {
                let held = path.with_extension("held");
                std::fs::rename(&path, &held).unwrap();
                symlink(held, &path).unwrap();
            }
            "hardlink" => std::fs::hard_link(&path, path.with_extension("held")).unwrap(),
            "stale" => set_state(&f.paths, |s| s.seq = 0),
            "failed" => set_state(&f.paths, |s| s.failed = true),
            "pending" => set_state(&f.paths, |s| {
                s.pending
                    .insert(name.into(), materialize::digest_hex(b"pending"));
            }),
            _ => unreachable!(),
        }
        let before = std::fs::read(f.paths.state_file()).unwrap();
        assert!(
            !matches!(
                read_publication_material(&f.paths, &expected, name),
                Ok(Some(_))
            ),
            "{fault}"
        );
        assert!(
            !matches!(entry.snapshot().read_publication(&f.paths), Ok(Some(_))),
            "snapshot {fault}"
        );
        assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), before);
    }
}

#[test]
fn signing_adopted_reader_keeps_current_authority_and_private_path_guards() {
    use std::os::unix::fs::PermissionsExt as _;
    for fault in [
        "changed",
        "manifest",
        "failed",
        "pending",
        "revoked",
        "audience",
        "config",
        "public",
        "link",
        "parent-link",
    ] {
        let f = adopted_signing_fixture(false);
        let name = "~/.aterm/apple/devid-existing.key";
        let expected = config(&f.paths).unwrap().unwrap();
        let key = materialize::target_path(&f.paths.home, name).unwrap();
        match fault {
            "changed" => fsx::write_atomic(&key, b"foreign-key", 0o600).unwrap(),
            "manifest" => set_state(&f.paths, |s| {
                s.manifest
                    .files
                    .insert(name.into(), materialize::digest_hex(b"different"));
            }),
            "failed" => set_state(&f.paths, |s| s.failed = true),
            "pending" => set_state(&f.paths, |s| {
                s.pending
                    .insert(name.into(), materialize::digest_hex(b"pending"));
            }),
            "revoked" => set_state(&f.paths, |s| s.membership = Stored::Revoked),
            "audience" => f.publish(|doc| {
                doc.unset_entry(&Audience::Everyone, name).unwrap();
                doc.set_entry(
                    &Audience::Hosts(vec!["peer".into()]),
                    name,
                    b"inert-existing-key",
                )
                .unwrap();
            }),
            "config" => {
                let mut changed = expected.clone();
                changed.source = Source::Path(f.paths.home.join("another.vault"));
                save_config(&f.paths, &changed).unwrap();
            }
            "public" => {
                std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap()
            }
            "link" => {
                let kept = f.paths.home.join("kept-key");
                std::fs::rename(&key, &kept).unwrap();
                std::os::unix::fs::symlink(kept, &key).unwrap();
            }
            "parent-link" => {
                let parent = key.parent().unwrap();
                let kept = f.paths.home.join("kept-apple");
                std::fs::rename(parent, &kept).unwrap();
                std::os::unix::fs::symlink(kept, parent).unwrap();
            }
            _ => unreachable!(),
        }
        let before = (
            std::fs::read(f.paths.state_file()).unwrap(),
            std::fs::read(&key).ok(),
        );
        assert!(
            !matches!(
                read_signing_material(&f.paths, &expected, name),
                Ok(Some(_))
            ),
            "{fault}"
        );
        assert_eq!(
            before,
            (
                std::fs::read(f.paths.state_file()).unwrap(),
                std::fs::read(&key).ok()
            ),
            "{fault}"
        );
    }
}

#[test]
fn an_adopted_signing_key_requires_its_current_vault_owned_anchor_chain() {
    use std::os::unix::fs::PermissionsExt as _;
    for fault in ["none", "changed", "untracked", "missing", "public"] {
        let f = adopted_signing_fixture(true);
        let key_name = "~/.aterm/apple/devid-existing.key";
        let cert_name = "~/.aterm/apple/devid-existing.cer";
        let expected = config(&f.paths).unwrap().unwrap();
        let cert = materialize::target_path(&f.paths.home, cert_name).unwrap();
        let state = load_state(&f.paths).unwrap();
        assert!(!state.manifest.files.contains_key(key_name));
        assert!(state.own.as_ref().unwrap().contains(key_name));
        assert!(state.manifest.files.contains_key(cert_name));
        match fault {
            "none" => {}
            "changed" => fsx::write_atomic(&cert, b"foreign-anchor", 0o600).unwrap(),
            "untracked" => set_state(&f.paths, |s| {
                s.manifest.files.remove(cert_name);
            }),
            "missing" => std::fs::remove_file(&cert).unwrap(),
            "public" => {
                std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o644)).unwrap()
            }
            _ => unreachable!(),
        }
        let before = std::fs::read(f.paths.state_file()).unwrap();
        let accepted = read_signing_material(&f.paths, &expected, key_name)
            .ok()
            .flatten()
            .is_some();
        assert_eq!(accepted, fault == "none", "{fault}");
        assert!(
            read_verified_entry(&f.paths, &expected, key_name)
                .ok()
                .flatten()
                .is_none()
        );
        assert_eq!(before, std::fs::read(f.paths.state_file()).unwrap());
    }
}

#[test]
fn encrypted_recipient_grants_survive_friendly_host_renames() {
    let f = Fixture::new();
    f.publish(|doc| {
        doc.set_entry(
            &Audience::Hosts(vec!["admin".into()]),
            "host-token",
            b"host-inert",
        )
        .unwrap()
    });
    let mut expected = config(&f.paths).unwrap().unwrap();
    expected.host = "renamed-admin".into();
    save_config(&f.paths, &expected).unwrap();
    assert_eq!(
        read_verified_entry(&f.paths, &expected, "host-token")
            .unwrap()
            .unwrap()
            .value
            .as_slice(),
        b"host-inert"
    );
}

#[test]
fn unsigned_and_unconfigured_reads_create_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("absent"));
    let mut expected = Config {
        source: Source::Path(root.path().join("plain.secrets")),
        name: "plain".into(),
        signer: Some([5; 32]),
        followed: vec![],
        host: "reader".into(),
    };
    assert!(
        read_verified_entry(&paths, &expected, "token")
            .unwrap()
            .is_none()
    );
    assert!(!paths.home.exists());
    expected.signer = None;
    assert!(
        read_verified_entry(&paths, &expected, "token")
            .unwrap()
            .is_none()
    );
    assert!(!paths.home.exists());
}

#[test]
fn stale_apply_context_and_unsafe_files_never_supply_authenticated_bytes() {
    use std::os::unix::fs::PermissionsExt as _;
    for fault in [
        "failed",
        "unbound",
        "no-digest",
        "sequence",
        "recipient",
        "public-file",
        "file-link",
        "source",
    ] {
        let f = Fixture::new();
        let expected = config(&f.paths).unwrap().unwrap();
        let token = materialize::target_path(&f.paths.home, "token").unwrap();
        match fault {
            "failed" => set_state(&f.paths, |s| s.failed = true),
            "unbound" => set_state(&f.paths, |s| s.source_identity = None),
            "no-digest" => set_state(&f.paths, |s| s.digest = None),
            "sequence" => set_state(&f.paths, |s| s.seq += 1),
            "recipient" => fsx::write_atomic(
                &f.paths.identity_file(),
                Identity::from_secret_bytes([8; 32]).to_bech32().as_slice(),
                0o600,
            )
            .unwrap(),
            "public-file" => {
                std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap()
            }
            "file-link" => {
                let kept = f.paths.home.join("kept-token");
                std::fs::rename(&token, &kept).unwrap();
                std::os::unix::fs::symlink(kept, &token).unwrap();
            }
            "source" => {
                let mut changed = expected.clone();
                changed.source = Source::Path(f.paths.home.join("different.vault"));
                save_config(&f.paths, &changed).unwrap();
            }
            _ => unreachable!(),
        }
        let before = f.bytes();
        assert!(
            !matches!(
                read_verified_entry(&f.paths, &expected, "token"),
                Ok(Some(_))
            ),
            "{fault}"
        );
        assert_eq!(before, f.bytes(), "{fault}");
    }
}

pub(crate) fn entry_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let f = Fixture::new();
    let mut pin = f.read_token().unwrap().snapshot;
    let mut expected = config(&f.paths).unwrap().unwrap();
    let token = materialize::target_path(&f.paths.home, "token").unwrap();
    let mut guard = None;
    let mut rebound = false;
    let mut name = "token";
    let scenario = match action {
        "Valid" => 1,
        "LocalEdit" => {
            fsx::write_atomic(&token, b"tampered", 0o600).unwrap();
            2
        }
        "ForgedManifest" => {
            fsx::write_atomic(&token, b"tampered", 0o600).unwrap();
            set_state(&f.paths, |s| {
                s.manifest
                    .files
                    .insert("token".into(), materialize::digest_hex(b"tampered"));
            });
            3
        }
        "Cache" => {
            let mut bytes = std::fs::read(f.paths.cache_file()).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            fsx::write_atomic(&f.paths.cache_file(), &bytes, 0o600).unwrap();
            4
        }
        "Config" => {
            expected.signer = Some([8; 32]);
            5
        }
        "Busy" => {
            guard = Some(StateLock::take(&f.paths).unwrap());
            6
        }
        "Revoked" => {
            set_state(&f.paths, |s| s.membership = Stored::Revoked);
            7
        }
        "Partial" => {
            set_state(&f.paths, |s| {
                s.pending
                    .insert("other".into(), materialize::digest_hex(b"pending"));
            });
            8
        }
        "Unsigned" => {
            expected.signer = None;
            9
        }
        "Unrelated" => {
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "other", b"next-other")
                    .unwrap()
            });
            rebound = true;
            10
        }
        "Changed" => {
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "token", b"next-token")
                    .unwrap()
            });
            rebound = true;
            11
        }
        "Policy" => {
            f.publish(|doc| {
                doc.unset_entry(&Audience::Everyone, "token").unwrap();
                doc.set_entry(
                    &Audience::Hosts(vec!["admin".into(), "peer".into()]),
                    "token",
                    b"inert-authentic-token",
                )
                .unwrap();
            });
            rebound = true;
            12
        }
        "Rollback" => {
            let before = f.bytes();
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "other", b"next-other")
                    .unwrap()
            });
            pin = f.read_token().unwrap().snapshot;
            for (path, bytes) in f.tracked().into_iter().zip(before) {
                if path == f.paths.lock_file() {
                    continue;
                }
                if let Some(bytes) = bytes {
                    fsx::write_atomic(&path, &bytes, 0o600).unwrap();
                }
            }
            assert!(
                f.read_token().is_some(),
                "control: an old valid signature alone is insufficient"
            );
            rebound = true;
            13
        }
        "Recipient" => {
            fsx::write_atomic(
                &f.paths.identity_file(),
                Identity::from_secret_bytes([9; 32]).to_bech32().as_slice(),
                0o600,
            )
            .unwrap();
            assert!(
                f.read_token().is_some(),
                "control: both recipients can decrypt this entry"
            );
            rebound = true;
            14
        }
        "ParentLink" => {
            let kept = f.paths.home.join("kept-secrets");
            std::fs::rename(f.paths.home.join(".secrets"), &kept).unwrap();
            std::os::unix::fs::symlink(&kept, f.paths.home.join(".secrets")).unwrap();
            15
        }
        "Fork" => {
            let admin = admin_open(&f.paths).unwrap();
            let state = load_state(&f.paths).unwrap();
            let bytes = seal(
                &expected.name,
                state.seq,
                &admin.seed,
                &admin.commons_seed,
                admin.commons_bound,
                &admin.doc,
                &admin.handoff,
            )
            .unwrap();
            let replacement = envelope::parse(&bytes).unwrap();
            assert_ne!(state.digest, Some(replacement.digest()));
            fsx::write_atomic(&f.paths.cache_file(), &bytes, 0o600).unwrap();
            set_state(&f.paths, |s| s.digest = Some(replacement.digest()));
            assert!(
                f.read_token().is_some(),
                "control: a signed same-sequence fork is readable without an earlier pin"
            );
            rebound = true;
            16
        }
        "Missing" => {
            std::fs::remove_file(&token).unwrap();
            17
        }
        "Reserved" => {
            name = crate::commons::ENTRY;
            18
        }
        _ => panic!("unknown entry scenario"),
    };
    let before = f.bytes();
    let result = if rebound {
        pin.read(&f.paths)
    } else {
        read_verified_entry(&f.paths, &expected, name)
    };
    let accepted = result.ok().flatten().is_some();
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next.insert("effects", i64::from(before != f.bytes()));
    drop(guard);
    next
}

#[test]
fn verified_entry_reads_conform_and_forged_files_or_rolled_back_pins_are_caught() {
    let model = aterm_models_db::verified_entry_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = entry_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    for action in ["ForgedManifest", "Rollback", "Fork"] {
        let mut bad = entry_transition(&model.init_state(), action);
        bad.insert("accepted", 1);
        assert!(!model.check_invariant("OnlyBoundBytes", &bad));
        assert!(!model.successors(action, &model.init_state()).contains(&bad));
    }
}
