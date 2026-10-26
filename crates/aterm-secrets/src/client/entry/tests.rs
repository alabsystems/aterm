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
