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
