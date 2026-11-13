// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::{self, ENTRY};

fn fixture() -> (aterm_tempfile::TempDir, Paths, PathBuf) {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.vault");
    let paths = Paths::for_home(&root.path().join("admin"));
    test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut document = admin.doc.clone();
    document
        .set_entry(
            &Audience::Hosts(vec!["m1".into()]),
            ENTRY,
            b"inert-commons-key-material",
        )
        .unwrap();
    document
        .set_entry(&Audience::Everyone, "ordinary", b"ordinary value")
        .unwrap();
    admin_publish(&paths, &admin, &document).unwrap();
    (root, paths, source)
}

#[test]
fn unavailable_control_observations_name_the_boundary_without_initializing() {
    use commons::{Observation, Unavailable};
    let reason = |paths: &Paths| match commons::observe(paths).unwrap() {
        Observation::Unavailable(reason) => reason,
        Observation::Visible(_) => panic!("unexpected visible control"),
    };
    let empty = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&empty.path().join("unconfigured"));
    assert_eq!(reason(&paths), Unavailable::Unconfigured);
    assert!(!paths.home.exists());
    let lock = StateLock::take(&paths).unwrap();
    assert_eq!(reason(&paths), Unavailable::Busy);
    drop(lock);
    assert_eq!(reason(&paths), Unavailable::Unconfigured);
    let (_root, paths, _) = fixture();
    let original = load_state(&paths).unwrap();
    for (variant, expected) in [
        (0, Unavailable::NotAdmitted),
        (1, Unavailable::Revoked),
        (2, Unavailable::IncompleteApply),
        (3, Unavailable::IncompleteApply),
        (4, Unavailable::SourceChanged),
        (5, Unavailable::IncompleteApply),
    ] {
        let mut state = original.clone();
        match variant {
            0 => state.membership = Stored::Pending,
            1 => state.membership = Stored::Revoked,
            2 => state.failed = true,
            3 => {
                state.pending.insert("ordinary".into(), "incomplete".into());
            }
            4 => state.source_identity = None,
            _ => state.digest = None,
        }
        save_state(&paths, &state).unwrap();
        assert_eq!(reason(&paths), expected);
    }
    save_state(&paths, &original).unwrap();
    let lock = StateLock::take(&paths).unwrap();
    assert_eq!(reason(&paths), Unavailable::Busy);
    drop(lock);
    for (path, expected) in [
        (paths.lock_file(), Unavailable::MissingStateLock),
        (paths.identity_file(), Unavailable::MissingIdentity),
        (paths.cache_file(), Unavailable::MissingCache),
    ] {
        let aside = path.with_extension("retained-test-copy");
        std::fs::rename(&path, &aside).unwrap();
        assert_eq!(reason(&paths), expected);
        assert!(!path.exists(), "observation cannot replace missing custody");
        std::fs::rename(aside, path).unwrap();
    }
    let bytes = std::fs::read(paths.cache_file()).unwrap();
    fsx::write_atomic(&paths.cache_file(), b"corrupt", 0o600).unwrap();
    assert!(commons::observe(&paths).is_err());
    fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.unset_entry(&Audience::Hosts(vec!["m1".into()]), ENTRY)
        .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    assert_eq!(reason(&paths), Unavailable::ControlNotVisible);
    assert!(matches!(
        status(&paths).unwrap().membership,
        Membership::In { .. }
    ));
    assert!(!paths.dir.join("commons").exists());
}

/// A vault an older admin's build wrote an AI CLI's login into: this Mac never writes the
/// login, and the rest of the vault is applied with nothing failed, so its Commons control
/// stays visible and its files are repaired as any vault's are. The refusal is reported on
/// each pass that applies, in its own bucket.
#[test]
fn a_vault_holding_an_ai_clis_login_still_applies_and_keeps_its_commons_control() {
    use commons::Observation;
    const LOGIN: &str = "~/.codex/auth.json";
    let (_root, paths, _) = fixture();
    let login = paths.home.join(".codex").join("auth.json");
    std::fs::create_dir_all(login.parent().unwrap()).unwrap();
    fsx::write_atomic(&login, b"this machine's own login", 0o600).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut text = String::from_utf8(admin.doc.render().as_slice().to_vec()).unwrap();
    text.push_str(&format!("[everyone]\n{LOGIN}={{}}\n"));
    let older = Document::parse(&text).expect("an older admin's vault parses");
    admin_publish(&paths, &admin, &older).unwrap();
    let state = load_state(&paths).unwrap();
    assert!(!state.failed, "nothing failed");
    assert!(state.source_identity.is_some());
    assert!(
        matches!(commons::observe(&paths).unwrap(), Observation::Visible(_)),
        "the Commons control is still read"
    );
    assert_eq!(std::fs::read(&login).unwrap(), b"this machine's own login");
    // A repair pass applies the vault again and names the refusal.
    let ordinary = materialize::target_path(&paths.home, "ordinary").unwrap();
    std::fs::remove_file(&ordinary).unwrap();
    let outcome = sync(&paths, None).unwrap();
    let SyncOutcome::Applied { report, .. } = &outcome else {
        panic!("a repair applies: {outcome:?}");
    };
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(
        report
            .refused
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [LOGIN]
    );
    let config = config(&paths).unwrap().unwrap();
    let line = pass_line(&paths, &config, &outcome, &mut String::new()).unwrap();
    assert!(line.contains(LOGIN), "{line}");
    assert!(ordinary.exists(), "repaired");
    assert!(!load_state(&paths).unwrap().failed);
    assert_eq!(std::fs::read(&login).unwrap(), b"this machine's own login");
}

/// A manifest an older build wrote names an AI CLI's login that build wrote
/// (`~/.codex/auth.json`), and the CLI has since rotated the file as its own: the login is no
/// file this vault keeps, so it is never counted as changed, and nothing is due to be put
/// back over it.
#[test]
fn a_cli_login_an_older_manifest_names_is_never_counted_as_changed() {
    const LOGIN: &str = "~/.codex/auth.json";
    let (_root, paths, _) = fixture();
    sync(&paths, None).unwrap();
    let mut repaired = None;
    assert!(!repair_due(&paths, &mut repaired), "settled before");
    let login = paths.home.join(".codex").join("auth.json");
    std::fs::create_dir_all(login.parent().unwrap()).unwrap();
    fsx::write_atomic(&login, b"the CLI's own rotated login", 0o600).unwrap();
    let mut state = load_state(&paths).unwrap();
    state.manifest.files.insert(
        LOGIN.to_owned(),
        materialize::digest_hex(b"the copy an older build wrote"),
    );
    save_state(&paths, &state).unwrap();
    let state = load_state(&paths).unwrap();
    assert!(state.manifest.files.contains_key(LOGIN));
    assert!(
        !changed_files(&paths, &state).contains(LOGIN),
        "a CLI's login is never a changed vault file"
    );
    assert!(unsettled(&paths, &state).is_empty());
    assert!(files_intact(&paths, &state));
    assert!(!repair_due(&paths, &mut repaired), "nothing to put back");
    assert_eq!(
        std::fs::read(&login).unwrap(),
        b"the CLI's own rotated login"
    );
}

#[test]
fn a_bounded_commons_observation_waits_only_for_the_vault_and_reads_fresh_state() {
    use commons::{Observation, Unavailable};
    for changed in [false, true] {
        let (_root, paths, _) = fixture();
        let mut owner = Some(StateLock::take(&paths).unwrap());
        assert!(matches!(
            commons::observe(&paths).unwrap(),
            Observation::Unavailable(Unavailable::Busy)
        ));
        let mut waited = 0;
        assert!(matches!(
            commons::observe_until_with(&paths, Instant::now(), || waited += 1).unwrap(),
            Observation::Unavailable(Unavailable::Busy)
        ));
        assert_eq!(waited, 0, "an expired budget cannot wait");
        let observation =
            commons::observe_until_with(&paths, Instant::now() + Duration::from_secs(60), || {
                waited += 1;
                if changed {
                    let mut state = load_state(&paths).unwrap();
                    state.membership = Stored::Revoked;
                    save_state(&paths, &state).unwrap();
                }
                drop(owner.take());
            })
            .unwrap();
        assert_eq!(waited, 1, "the fixture released an actually held lock");
        if changed {
            assert!(matches!(
                observation,
                Observation::Unavailable(Unavailable::Revoked)
            ));
        } else {
            assert!(matches!(observation, Observation::Visible(_)));
        }
        assert!(!paths.dir.join("commons").exists());
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("absent"));
    assert!(matches!(
        commons::observe_until_with(&paths, Instant::now() + Duration::from_secs(60), || panic!(
            "a missing vault cannot become available by waiting"
        ),)
        .unwrap(),
        Observation::Unavailable(Unavailable::Unconfigured)
    ));
    assert!(!paths.home.exists());
}

#[test]
fn member_certification_uses_opened_eligibility_not_unpublished_edits() {
    use crate::commons::{
        binding::{Pending, Request},
        journal::{Journal, Trust},
        node::Edit,
    };
    let (root, paths, _source) = fixture();
    let mut admin = admin_open(&paths).unwrap();
    let network = [33; 16];
    let genesis = admin
        .seal_commons(
            network,
            Edit::Create {
                hosts: &["m1".into()],
                payload: b"initial control",
            },
        )
        .unwrap();
    let mut journal = Journal::open(
        &root.path().join("member-control"),
        Trust {
            network,
            authority: genesis.authority(),
            genesis: genesis.id(),
        },
    )
    .unwrap();
    journal.ingest(genesis.bytes()).unwrap();
    let member = Identity::from_secret_bytes([41; 32]);
    let seed = [43; 32];
    let request = Request::create(
        network,
        genesis.authority(),
        [47; 16],
        [53; 16],
        &member,
        &seed,
    )
    .unwrap();
    let proof = || {
        let (mut pending, challenge) =
            Pending::issue(&request, network, genesis.authority()).unwrap();
        pending
            .verify(&request.answer(&member, &seed, &challenge).unwrap())
            .unwrap()
    };
    admin
        .doc
        .add_machine("m2", &member.recipient().to_bech32())
        .unwrap();
    admin
        .doc
        .set_entry(
            &Audience::Hosts(vec!["m1".into(), "m2".into()]),
            ENTRY,
            b"inert-control",
        )
        .unwrap();
    assert!(
        admin
            .approve_commons_member(&journal, "m2", proof())
            .is_err()
    );
    use crate::commons::roster::{self, Enrollment, Grant, Role};
    assert!(
        admin
            .prepare_commons_genesis(network, vec![Enrollment::new("m2", proof(), Role::Writer)])
            .is_err()
    );
    admin_publish(&paths, &admin, &admin.doc).unwrap();
    assert!(
        admin
            .approve_commons_member(&journal, "m2", proof())
            .is_err(),
        "reopen after publishing"
    );
    let mut reopened = admin_open(&paths).unwrap();
    assert!(
        reopened
            .approve_commons_member(&journal, "m2", proof())
            .is_ok()
    );
    let initial = reopened
        .prepare_commons_genesis(network, vec![Enrollment::new("m2", proof(), Role::Writer)])
        .unwrap();
    let mut active = Journal::open(
        &root.path().join("typed-member-control"),
        Trust {
            network,
            authority: initial.authority(),
            genesis: initial.id(),
        },
    )
    .unwrap();
    active.ingest(initial.bytes()).unwrap();
    assert_eq!(
        roster::current(&active, &member)
            .unwrap()
            .unwrap()
            .local()
            .role(),
        Role::Writer
    );
    let next = reopened
        .prepare_commons_rotation(
            &active,
            vec![Grant::recorded(
                "m2",
                request.binding().clone(),
                Role::Reader,
            )],
        )
        .unwrap();
    active.ingest(next.bytes()).unwrap();
    assert_eq!(
        roster::current(&active, &member)
            .unwrap()
            .unwrap()
            .local()
            .role(),
        Role::Reader
    );
    reopened
        .doc
        .unset_entry(&Audience::Hosts(vec!["m1".into(), "m2".into()]), ENTRY)
        .unwrap();
    assert!(
        reopened
            .approve_commons_member(&journal, "m2", proof())
            .is_ok(),
        "a draft removal is not a published revocation"
    );
    admin_publish(&paths, &reopened, &reopened.doc).unwrap();
    assert!(
        admin_open(&paths)
            .unwrap()
            .approve_commons_member(&journal, "m2", proof())
            .is_err()
    );
    assert!(
        admin_open(&paths)
            .unwrap()
            .prepare_commons_rotation(
                &active,
                vec![Grant::recorded(
                    "m2",
                    request.binding().clone(),
                    Role::Writer
                )]
            )
            .is_err()
    );
}

#[test]
fn control_journal_uses_its_own_authority_without_ordinary_materialization() {
    use crate::commons::{
        journal::{Journal, State, Trust},
        node::Edit,
    };
    let (root, paths, _source) = fixture();
    let admin = admin_open(&paths).unwrap();
    let hosts = ["m1".to_string()];
    let network = [33; 16];
    let node = admin
        .seal_commons(
            network,
            Edit::Create {
                hosts: &hosts,
                payload: b"encrypted journal control",
            },
        )
        .unwrap();
    assert_eq!(node.authority(), admin.commons_authority().unwrap());
    assert_ne!(Some(node.authority()), admin.config.signer);
    assert!(
        admin
            .seal_commons(
                network,
                Edit::Create {
                    hosts: &["unknown-host".into()],
                    payload: b"secret"
                }
            )
            .is_err()
    );
    assert!(
        admin
            .seal_commons(
                network,
                Edit::Create {
                    hosts: &[],
                    payload: b"secret"
                }
            )
            .is_err()
    );
    let ordinary = paths.home.join(".secrets/ordinary");
    std::fs::remove_file(&ordinary).unwrap();
    std::fs::create_dir(&ordinary).unwrap();
    assert!(!materialization_ready(&paths).unwrap());
    let mut journal = Journal::open(
        &root.path().join("control"),
        Trust {
            network,
            authority: node.authority(),
            genesis: node.id(),
        },
    )
    .unwrap();
    journal.ingest(node.bytes()).unwrap();
    assert_eq!(
        journal.state(),
        State::Ready {
            head: node.id(),
            basis: node.id()
        }
    );
    let identity = read_identity(&paths).unwrap().unwrap();
    assert_eq!(
        journal
            .current(&identity)
            .unwrap()
            .unwrap()
            .payload
            .as_slice(),
        b"encrypted journal control"
    );
    assert!(
        !materialization_ready(&paths).unwrap(),
        "control repair never marks ordinary files applied"
    );
}

#[test]
fn commons_cache_is_authenticated_offline_and_never_a_plaintext_file() {
    let (_root, paths, source) = fixture();
    let state = load_state(&paths).unwrap();
    let config = config(&paths).unwrap().unwrap();
    std::fs::remove_file(&source).unwrap();
    let snapshot = commons::cached(&paths).unwrap().unwrap();
    assert_eq!(snapshot.control(), b"inert-commons-key-material");
    assert_eq!(snapshot.sequence(), state.seq);
    assert_eq!(Some(*snapshot.digest()), state.digest);
    assert_eq!(Some(*snapshot.signer()), config.signer);
    assert_eq!(snapshot.vault(), config.name);
    assert_eq!(
        snapshot.recipient(),
        read_identity(&paths).unwrap().unwrap().recipient()
    );
    assert!(!format!("{snapshot:?}").contains("inert-commons-key-material"));
    assert!(!state.manifest.files.contains_key(ENTRY));
    assert!(!paths.home.join(".secrets").join(ENTRY).exists());
    assert_eq!(
        std::fs::read(paths.home.join(".secrets/ordinary")).unwrap(),
        b"ordinary value"
    );
    let lock = StateLock::take(&paths).unwrap();
    assert!(commons::cached(&paths).unwrap().is_none());
    drop(lock);
    assert!(commons::cached(&paths).unwrap().is_some());
}

#[test]
fn commons_bootstrap_uses_the_vault_cache_and_preserves_custody_through_handoff() {
    use crate::commons::{
        binding::Pending,
        bootstrap::{Bootstrap, Descriptor, Device},
        roster::{Enrollment, Role},
    };
    let (_root, paths, source) = fixture();
    let admin = admin_open(&paths).unwrap();
    let authority = admin.commons_authority().unwrap();
    let network = [39; 16];
    let device = Device::open(&paths).unwrap().unwrap();
    let request = device.request(network, authority).unwrap();
    let (mut pending, challenge) = Pending::issue(&request, network, authority).unwrap();
    let possession = pending
        .verify(&device.answer(&request, &challenge).unwrap())
        .unwrap();
    let genesis = admin
        .prepare_commons_genesis(
            network,
            vec![Enrollment::new("m1", possession, Role::Writer)],
        )
        .unwrap();
    let descriptor = Descriptor::from_genesis(genesis).unwrap();
    let mut document = admin.doc.clone();
    document
        .set_entry(
            &Audience::Hosts(vec!["m1".into()]),
            ENTRY,
            &descriptor.bytes(),
        )
        .unwrap();
    admin_publish(&paths, &admin, &document).unwrap();
    let first = Bootstrap::cached(&paths).unwrap().unwrap();
    assert_eq!(first.request().unwrap().bytes(), request.bytes());
    assert!(first.witness().unwrap().is_some());
    let access = first.access().unwrap().unwrap();
    assert_eq!(access.local().binding(), request.binding());
    let old = first.trust();
    let admin = admin_open(&paths).unwrap();
    admin_rotate(&paths, &admin, &admin.doc).unwrap();
    assert_ne!(
        commons::cached(&paths).unwrap().unwrap().signer(),
        &authority
    );
    std::fs::remove_file(source).unwrap();
    let restarted = Bootstrap::cached(&paths).unwrap().unwrap();
    assert_eq!(restarted.trust(), old);
    assert_eq!(restarted.request().unwrap().bytes(), request.bytes());
    assert!(restarted.access().unwrap().is_some());
    assert!(!paths.home.join(".secrets").join(ENTRY).exists());
}

#[test]
fn commons_cache_refuses_unbound_revoked_pending_and_changed_state() {
    let (_root, paths, _source) = fixture();
    let original = load_state(&paths).unwrap();
    for variant in 0..6 {
        let mut state = original.clone();
        match variant {
            0 => state.source_identity = None,
            1 => state.digest = None,
            2 => state.membership = Stored::Revoked,
            3 => state.membership = Stored::Pending,
            4 => state.failed = true,
            _ => {
                state.pending.insert("ordinary".into(), "incomplete".into());
            }
        }
        save_state(&paths, &state).unwrap();
        assert!(
            commons::cached(&paths).unwrap().is_none(),
            "variant {variant}"
        );
    }
    save_state(&paths, &original).unwrap();
    let bytes = std::fs::read(paths.cache_file()).unwrap();
    let mut state = original.clone();
    state.seq += 1;
    save_state(&paths, &state).unwrap();
    assert!(
        commons::cached(&paths).is_err(),
        "rollback below the applied version"
    );
    state = original.clone();
    state.digest = Some([42; 32]);
    save_state(&paths, &state).unwrap();
    assert!(
        commons::cached(&paths).is_err(),
        "different hash at the applied version"
    );
    save_state(&paths, &original).unwrap();
    let mut corrupt = bytes.clone();
    let prefix = b"signature: ";
    let signature = corrupt
        .windows(prefix.len())
        .position(|part| part == prefix)
        .unwrap()
        + prefix.len();
    corrupt[signature] = if corrupt[signature] == b'A' {
        b'B'
    } else {
        b'A'
    };
    fsx::write_atomic(&paths.cache_file(), &corrupt, 0o600).unwrap();
    assert!(commons::cached(&paths).is_err(), "corrupt signature");
    fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
    let (_other_root, other, _) = fixture();
    let foreign = std::fs::read(other.cache_file()).unwrap();
    fsx::write_atomic(&paths.cache_file(), &foreign, 0o600).unwrap();
    assert!(
        commons::cached(&paths).is_err(),
        "a valid envelope from another authority cannot replace this vault"
    );
    fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
    let config = config(&paths).unwrap().unwrap();
    let mut unrelated = config.clone();
    unrelated.source = Source::Path(paths.home.join("a-different-source"));
    save_config(&paths, &unrelated).unwrap();
    assert!(commons::cached(&paths).unwrap().is_none());
    save_config(&paths, &config).unwrap();
    let key = std::fs::read(paths.identity_file()).unwrap();
    let other = Identity::generate().unwrap().to_bech32();
    fsx::write_atomic(&paths.identity_file(), other.as_slice(), 0o600).unwrap();
    assert!(
        commons::cached(&paths).unwrap().is_none(),
        "another recipient cannot open the grant"
    );
    fsx::write_atomic(&paths.identity_file(), &key, 0o600).unwrap();
    assert!(commons::cached(&paths).unwrap().is_some());
}

fn signed_lockers(paths: &Paths, plaintexts: &[String], future: bool) {
    let admin = admin_open(paths).unwrap();
    let identity = read_identity(paths).unwrap().unwrap();
    let old = load_state(paths).unwrap();
    let lockers = plaintexts
        .iter()
        .enumerate()
        .map(|(i, text)| Locker {
            label: (i + 1).to_string(),
            armored: age::armor(&age::encrypt(&[identity.recipient()], text.as_bytes()).unwrap()),
        })
        .collect();
    let bytes = envelope::sign(
        &Unsigned {
            name: admin.config.name.clone(),
            seq: old.seq + u64::from(future),
            time: 1,
            handoff: admin.handoff.clone(),
            parents: Vec::new(),
            lockers,
        },
        &admin.seed,
    )
    .unwrap();
    fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
    if !future {
        let mut state = old;
        state.digest = Some(envelope::parse(&bytes).unwrap().digest());
        save_state(paths, &state).unwrap();
    }
}

#[test]
fn commons_cache_requires_applied_explicit_nonduplicated_recipient_grants() {
    let (_root, paths, _source) = fixture();
    let grant = format!("[m1]\n{ENTRY}=inert-control\n");
    signed_lockers(&paths, std::slice::from_ref(&grant), true);
    assert!(
        commons::cached(&paths).unwrap().is_none(),
        "a future cache is not the applied version"
    );
    let (_root, paths, _source) = fixture();
    signed_lockers(&paths, &[grant.clone(), grant], false);
    assert!(
        commons::cached(&paths).is_err(),
        "two opened lockers cannot each grant Commons"
    );
    let (_root, paths, _source) = fixture();
    signed_lockers(
        &paths,
        &[format!("[other]\n{ENTRY}=inert-control\n")],
        false,
    );
    assert!(
        commons::cached(&paths).unwrap().is_some(),
        "an authority-encrypted recipient grant survives a host-label change"
    );
    let (_root, paths, _source) = fixture();
    signed_lockers(
        &paths,
        &[format!("[everyone]\n{ENTRY}=inert-control\n")],
        false,
    );
    assert!(
        commons::cached(&paths).is_err(),
        "an everyone locker cannot grant Commons"
    );
}

#[test]
fn unconfigured_and_plain_vaults_do_not_create_commons_keys() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("fresh"));
    assert!(commons::cached(&paths).unwrap().is_none());
    assert!(!paths.home.exists());
    let source = root.path().join("plain.secrets");
    fsx::write_atomic(&source, b"ordinary=inert\n", 0o600).unwrap();
    let paths = Paths::for_home(&root.path().join("plain"));
    join(
        &paths,
        &JoinOptions {
            source: source.to_str().unwrap().into(),
            host: Some("m1".into()),
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
    assert!(commons::cached(&paths).unwrap().is_none());
}

#[test]
fn commons_cache_follows_authority_rotation_and_drops_a_removed_grant() {
    let (_root, paths, _source) = fixture();
    let old_config = config(&paths).unwrap().unwrap();
    let old = *commons::cached(&paths).unwrap().unwrap().signer();
    let admin = admin_open(&paths).unwrap();
    admin_rotate(&paths, &admin, &admin.doc).unwrap();
    let rotated = commons::cached(&paths).unwrap().unwrap();
    assert_ne!(*rotated.signer(), old);
    assert!(rotated.followed().contains(&old));
    let current_config = config(&paths).unwrap().unwrap();
    let current_state = load_state(&paths).unwrap();
    let mut stale = current_state.clone();
    stale.source_identity = Some(source_identity(&old_config));
    save_config(&paths, &old_config).unwrap();
    save_state(&paths, &stale).unwrap();
    assert!(
        commons::cached(&paths).unwrap().is_none(),
        "a signer handoff must be applied before its control is exposed"
    );
    save_config(&paths, &current_config).unwrap();
    save_state(&paths, &current_state).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut document = admin.doc.clone();
    document
        .unset_entry(&Audience::Hosts(vec!["m1".into()]), ENTRY)
        .unwrap();
    admin_publish(&paths, &admin, &document).unwrap();
    assert!(commons::cached(&paths).unwrap().is_none());
}

#[test]
fn missing_or_damaged_commons_cache_is_repaired_without_a_new_vault_version() {
    let (_root, paths, _source) = fixture();
    let before = load_state(&paths).unwrap();
    let mut repaired = None;
    for missing in [true, false, true] {
        if missing {
            std::fs::remove_file(paths.cache_file()).unwrap();
        } else {
            fsx::write_atomic(&paths.cache_file(), b"damaged cache", 0o600).unwrap();
        }
        assert!(!materialization_ready(&paths).unwrap());
        assert!(
            repair_due(&paths, &mut repaired),
            "a lost internal cache is repairable more than once at the same version"
        );
        sync(&paths, None).unwrap();
        let snapshot = commons::cached(&paths).unwrap().unwrap();
        assert_eq!(snapshot.sequence(), before.seq);
        assert_eq!(Some(*snapshot.digest()), before.digest);
        assert!(materialization_ready(&paths).unwrap());
    }
}
