// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

pub(super) fn caller() -> Caller {
    Caller {
        sid: "s-slot-fixture".into(),
        pid: 42,
        birth: 123,
    }
}

#[cfg(unix)]
pub(super) fn fixture(key: bool) -> (aterm_tempfile::TempDir, Env) {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    platform::ensure_private_dir(&home).unwrap();
    if key {
        client::machine_identity_as(
            &client::Paths::locate_for_home(&home),
            aterm_secrets::Making {
                test_binary: true,
                ..Default::default()
            },
        )
        .unwrap();
    }
    let env = Env {
        state: root.path().join("state"),
        aterm_state: Some(root.path().join("aterm")),
        cwd: home.clone(),
        home: Some(home),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 1_790_000_000,
        sid: "s-slot-fixture".into(),
        sock: None,
        config: None,
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    (root, env)
}

// Persisted GrantBinding is data, never an admitted grant. This fixture can
// exercise slot authentication but cannot construct AdmittedGrant or install.
pub(super) fn record(caller: Caller) -> Record {
    let binding: owner::GrantBinding = aterm_json::from_value(aterm_json::json!({
        "request":{"id":"a".repeat(32),"caller":"b".repeat(64),"label":"fixture","revision":"c".repeat(64)},
        "reference":{"operation":"a".repeat(32),"capability":"d".repeat(64),
            "issuance_digest":"e".repeat(64),"producer_mac":"f".repeat(64)},
        "generation":1
    })).unwrap();
    Record {
        schema: 2,
        caller,
        binding,
        assignment_generation: 1,
        source_authority: codex_assignment::SourceAuthority::Assignment {
            operation: "1".repeat(32),
        },
        revision: Revision {
            generation: 1,
            digest: "2".repeat(64),
        },
    }
}

#[test]
fn startup_metadata_requires_its_own_nonce_and_cannot_replace_an_assignment() {
    // Record-shape checks only. These values cannot construct a native
    // InitialSourcePermit, AdmittedGrant or StartupClaim.
    let assigned = record(caller());
    let mut startup = assigned.clone();
    startup.assignment_generation = 0;
    assert!(!startup.valid());
    startup.source_authority = codex_assignment::SourceAuthority::Startup {
        nonce: "7".repeat(32),
    };
    assert!(startup.valid());
    assert!(startup_installation_allowed(None, &startup));
    assert!(startup_installation_allowed(Some(&startup), &startup));
    assert!(!startup_installation_allowed(Some(&assigned), &startup));
    let mut later = startup.clone();
    later.revision.generation += 1;
    later.revision.digest = "8".repeat(64);
    assert!(startup_installation_allowed(Some(&startup), &later));
    assert!(!startup_installation_allowed(Some(&later), &startup));
    later = startup.clone();
    later.revision.digest = "8".repeat(64);
    assert!(!startup_installation_allowed(Some(&startup), &later));
    later = startup.clone();
    later.source_authority = codex_assignment::SourceAuthority::Startup {
        nonce: "9".repeat(32),
    };
    assert!(!startup_installation_allowed(Some(&startup), &later));
    assert!(startup.source_authority.native_intent().is_none());
    assert_eq!(
        assigned.source_authority.native_intent(),
        Some("1".repeat(32).as_str())
    );
    startup.revision.generation = 0;
    assert!(!startup.valid());
}

#[test]
fn slot_keys_bind_full_process_lineage_and_are_bounded_utf8_keys() {
    let base = caller();
    let binding = record(base.clone()).binding;
    let key = slot_key(&base, &binding).unwrap();
    assert!(key.len() <= 256 && key.len().is_multiple_of(2) && hex(&key, key.len()));
    for alternate in [
        Caller {
            sid: "s-other".into(),
            ..base.clone()
        },
        Caller {
            pid: 43,
            ..base.clone()
        },
        Caller {
            birth: 124,
            ..base.clone()
        },
    ] {
        assert_ne!(key, slot_key(&alternate, &binding).unwrap());
    }
    assert!(
        slot_key(
            &Caller {
                sid: "../../escape".into(),
                ..base
            },
            &binding
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn credential_revision_domains_do_not_share_a_materialization_or_slot_key() {
    let (_root, env) = fixture(true);
    let caller = caller();
    let first = record(caller.clone()).binding;
    let mut second = first.clone();
    second.generation += 1;
    let first_key = slot_key(&caller, &first).unwrap();
    let second_key = slot_key(&caller, &second).unwrap();
    assert_ne!(first_key, second_key);
    assert_eq!(first_key, slot_key(&caller, &first).unwrap());
    let first_path = materialization_location(&env, &caller, &first).unwrap();
    let second_path = materialization_location(&env, &caller, &second).unwrap();
    assert_ne!(first_path, second_path);
    platform::ensure_private_dir(&first_path).unwrap();
    platform::ensure_private_dir(&second_path).unwrap();
    private_file::write_private(&first_path, "auth.json", b"first-grant-revision-14").unwrap();
    private_file::write_private(&second_path, "auth.json", b"second-grant-revision-1").unwrap();
    assert_eq!(
        read_document(&first_path).unwrap().as_slice(),
        b"first-grant-revision-14"
    );
    assert_eq!(
        read_document(&second_path).unwrap().as_slice(),
        b"second-grant-revision-1"
    );
}

#[test]
fn installed_target_credential_advance_uses_actual_assignment_predecessor() {
    let mut current = record(caller());
    current.binding.generation = 41;
    current.assignment_generation = 8;
    current.source_authority = codex_assignment::SourceAuthority::Assignment {
        operation: "a".repeat(32),
    };
    current.revision.generation = 13;
    current.revision.digest = "d".repeat(64);
    let mut next = current.clone();
    next.revision.generation = 14;
    next.revision.digest = "e".repeat(64);
    assert!(installation_allowed(Some(&current), &next, 7));
    assert!(!installation_allowed(Some(&current), &next, 8));
    assert_eq!(next.assignment_generation, 8);
    assert_eq!(next.binding.generation, 41);
}

#[test]
fn lost_ack_reconciles_only_the_exact_assignment_without_revision_rollback() {
    let current = record(caller());
    assert!(installation_allowed(None, &current, 0));
    assert!(installation_allowed(Some(&current), &current, 0));
    assert!(!installation_allowed(Some(&current), &current, 1));
    let mut destination = current.clone();
    destination.revision.generation += 1;
    destination.revision.digest = "4".repeat(64);
    assert!(installation_allowed(Some(&current), &destination, 0));
    assert!(!installation_allowed(Some(&destination), &current, 0));
    destination = current.clone();
    destination.revision.digest = "4".repeat(64);
    assert!(!installation_allowed(Some(&current), &destination, 0));
    destination = current.clone();
    destination.source_authority = codex_assignment::SourceAuthority::Assignment {
        operation: "5".repeat(32),
    };
    assert!(!installation_allowed(Some(&current), &destination, 0));
    destination = current.clone();
    destination.binding.generation += 1;
    assert!(!installation_allowed(Some(&current), &destination, 0));
    destination = current.clone();
    destination.caller.birth += 1;
    assert!(!installation_allowed(Some(&current), &destination, 0));
    destination = current.clone();
    destination.assignment_generation += 1;
    destination.source_authority = codex_assignment::SourceAuthority::Assignment {
        operation: "6".repeat(32),
    };
    assert!(installation_allowed(Some(&current), &destination, 1));
    assert!(!installation_allowed(Some(&current), &destination, 0));
}

#[cfg(unix)]
#[test]
fn unknown_source_never_creates_a_slot_or_machine_identity() {
    for key in [false, true] {
        let (_root, env) = fixture(key);
        let caller = caller();
        let (directory, home, _) = location(&env, &caller).unwrap();
        assert!(source(&env, &caller, &"a".repeat(32)).is_err());
        assert!(!directory.exists());
        assert_eq!(machine(&home).is_ok(), key);
    }
}

#[cfg(unix)]
#[test]
fn credential_readback_refuses_links_loose_permissions_and_oversized_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = root.path().join("materialization");
    platform::ensure_private_dir(&directory).unwrap();
    let path = directory.join("auth.json");
    private_file::write_private(&directory, "auth.json", b"private-document").unwrap();
    assert_eq!(
        read_document(&directory).unwrap().as_slice(),
        b"private-document"
    );
    std::fs::hard_link(&path, directory.join("alias")).unwrap();
    assert!(read_document(&directory).is_err());
    std::fs::remove_file(directory.join("alias")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_document(&directory).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(&path, directory.join("original")).unwrap();
    symlink(directory.join("original"), &path).unwrap();
    assert!(read_document(&directory).is_err());
    std::fs::remove_file(&path).unwrap();
    private_file::write_private(&directory, "auth.json", &vec![b'x'; MAX_DOCUMENT + 1]).unwrap();
    assert!(read_document(&directory).is_err());
}

#[cfg(unix)]
#[test]
fn public_recipient_reencryption_cannot_retarget_an_authenticated_slot() {
    let (_root, env) = fixture(true);
    let caller = caller();
    let mut opened = Opened::open(&env, &caller, true).unwrap().unwrap();
    let original = record(caller.clone());
    opened.save(original.clone()).unwrap();
    let directory = opened.directory.clone();
    let recipient = opened.identity.recipient();
    let old_mac = mac(&opened.identity, &original).unwrap();
    drop(opened);
    assert!(
        Opened::open(&env, &caller, false)
            .unwrap()
            .unwrap()
            .record
            .as_ref()
            == Some(&original)
    );
    let mut forged = original;
    forged.assignment_generation += 1;
    forged.source_authority = codex_assignment::SourceAuthority::Assignment {
        operation: "3".repeat(32),
    };
    let plaintext = SecretBytes::new(
        aterm_json::to_vec(&Envelope {
            record: forged,
            mac: old_mac,
        })
        .unwrap(),
    );
    let ciphertext = age::encrypt(&[recipient], plaintext.as_slice()).unwrap();
    aterm_secrets::fsx::write_atomic(&directory.join("candidate.age"), &ciphertext, 0o600).unwrap();
    assert!(Opened::open(&env, &caller, false).is_err());
}
