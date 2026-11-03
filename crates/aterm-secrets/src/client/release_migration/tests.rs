// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_update_core::roster::{Machine, Roster, SUPPORTED_SCHEMA};

struct Fixture {
    doc: Document,
    master: Ed25519KeyPair,
    anchor: String,
    roster: Roster,
}

impl Fixture {
    fn new() -> Self {
        let rng = ring::rand::SystemRandom::new();
        let master =
            Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref())
                .unwrap();
        let anchor = aterm_codec::base64::encode(master.public_key().as_ref()).unwrap();
        let key = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(key.as_ref()).unwrap();
        let public = aterm_codec::base64::encode(pair.public_key().as_ref()).unwrap();
        let profile = SecretBytes::from(format!(
            "machine_id = \"shared\"\nsigning_key = \"{}\"\nmachine_roster = \"{LEGACY_ROSTER}\"\nnotary_profile = \"notary\"\n",
            aterm_codec::base64::encode(key.as_ref()).unwrap()
        ));
        let machine = format!("id = \"shared\"\npubkey = \"{public}\"\n");
        let identity = Identity::generate().unwrap();
        let base = Document::new_vault("admin", &identity.recipient().to_bech32());
        let entries = [key.as_ref(), machine.as_bytes(), profile.as_slice()]
            .into_iter()
            .enumerate()
            .map(|(index, value)| Entry {
                name: LEGACY[index].to_owned(),
                value: SecretBytes::from(value),
                audience: Audience::Everyone,
                if_absent: true,
                anchor: Some(LEGACY[(index + 1) % 3].to_owned()),
            })
            .collect::<Vec<_>>();
        let text = SecretBytes::from(format!(
            "{}\n{}",
            base.render().as_str().unwrap(),
            text::render_everyone_locker(&entries).as_str().unwrap()
        ));
        let doc = Document::parse(text.as_str().unwrap()).unwrap();
        let roster = Roster {
            schema: SUPPORTED_SCHEMA,
            roster_seq: 5,
            valid_until: "9999-12-31T00:00:00Z".into(),
            revoked: Vec::new(),
            machines: vec![Machine {
                id: "shared".into(),
                pubkey: public,
                added_at: "1970-01-01T00:00:00Z".into(),
                not_after: None,
            }],
        };
        let mut result = Self {
            doc,
            master,
            anchor,
            roster,
        };
        result.sign_roster();
        result
    }

    fn sign_roster(&mut self) {
        let bytes = self.roster.to_toml().unwrap().into_bytes();
        self.doc
            .set_entry(&Audience::Everyone, LEGACY_ROSTER, &bytes)
            .unwrap();
        self.doc
            .set_entry(
                &Audience::Everyone,
                &format!("{LEGACY_ROSTER}.sig"),
                self.master.sign(&bytes).as_ref(),
            )
            .unwrap();
    }

    fn migrate(&self) -> Migration {
        migrate(&self.doc, &[&self.anchor], 5, 1_000)
    }

    fn ready(&self) -> Document {
        match self.migrate() {
            Migration::Ready(doc) => *doc,
            _ => panic!("fixture should authorize migration"),
        }
    }

    fn identity_only(&self) -> Document {
        match migrate_identity(&self.doc, &[&self.anchor], 5, 1_000) {
            Migration::Ready(doc) => *doc,
            _ => panic!("fixture should authorize the original three-file migration"),
        }
    }

    fn replace_legacy(&mut self, index: usize, bytes: &[u8]) {
        self.doc
            .set_entry_with(
                &Audience::Everyone,
                LEGACY[index],
                bytes,
                true,
                Some(LEGACY[(index + 1) % 3]),
            )
            .unwrap();
    }
}

#[test]
fn empty_canonical_bundle_copies_exact_values_and_is_idempotent() {
    let fixture = Fixture::new();
    let next = fixture.ready();
    for (index, name) in LEGACY.iter().enumerate() {
        let original = fixture.doc.entry(&Audience::Everyone, name).unwrap();
        assert_eq!(next.entry(&Audience::Everyone, name).unwrap(), original);
        let copied = next.entry(&Audience::Everyone, CANONICAL[index]).unwrap();
        assert_eq!(copied.value, original.value);
        assert!(copied.if_absent);
        assert_eq!(copied.anchor.as_deref(), Some(CANONICAL[(index + 1) % 3]));
    }
    for (index, name) in CANONICAL_ROSTER.iter().enumerate() {
        let copied = next.entry(&Audience::Everyone, name).unwrap();
        let source = if index == 0 {
            LEGACY_ROSTER.to_owned()
        } else {
            format!("{LEGACY_ROSTER}.sig")
        };
        assert_eq!(
            copied.value,
            fixture
                .doc
                .entry(&Audience::Everyone, &source)
                .unwrap()
                .value
        );
        assert!(copied.if_absent);
        assert_eq!(copied.anchor.as_deref(), Some(CANONICAL_ROSTER[1 - index]));
    }
    assert!(matches!(
        migrate(&next, &[&fixture.anchor], 5, 1_000),
        Migration::Unchanged
    ));
}

#[test]
fn any_existing_target_even_host_specific_prevents_partial_replacement() {
    for name in CANONICAL.into_iter().chain(CANONICAL_ROSTER) {
        for audience in [Audience::Everyone, Audience::Hosts(vec!["admin".into()])] {
            let mut fixture = Fixture::new();
            fixture
                .doc
                .set_entry(&audience, name, b"existing choice")
                .unwrap();
            let before = fixture.doc.render();
            assert!(matches!(fixture.migrate(), Migration::Unchanged));
            assert_eq!(fixture.doc.render(), before);
        }
    }
}

#[test]
fn a_personal_legacy_profile_stays_while_the_modern_bundle_materializes() {
    let fixture = Fixture::new();
    let next = fixture.ready();
    let home = aterm_tempfile::tempdir().unwrap();
    let personal = materialize::target_path(home.path(), LEGACY[2]).unwrap();
    fsx::ensure_private_dir(personal.parent().unwrap()).unwrap();
    fsx::write_atomic(&personal, b"personal profile unchanged", 0o600).unwrap();
    let (_, report) =
        materialize::apply(home.path(), &next.entries(), &Manifest::default(), 1).unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(
        fsx::read_private(&personal, 4096)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"personal profile unchanged"
    );
    assert!(
        !materialize::target_path(home.path(), LEGACY[0])
            .unwrap()
            .exists()
    );
    assert!(
        !materialize::target_path(home.path(), LEGACY[1])
            .unwrap()
            .exists()
    );
    for (index, name) in CANONICAL.iter().enumerate() {
        let actual =
            fsx::read_private(&materialize::target_path(home.path(), name).unwrap(), 65536)
                .unwrap()
                .unwrap();
        assert_eq!(
            actual,
            fixture
                .doc
                .entry(&Audience::Everyone, LEGACY[index])
                .unwrap()
                .value
        );
    }
    let read = |name| {
        fsx::read_private(&materialize::target_path(home.path(), name).unwrap(), 65536)
            .unwrap()
            .unwrap()
    };
    assert!(
        crate::release_ring::authorize_with(
            read(CANONICAL[2]).as_slice(),
            read(CANONICAL[0]).as_slice(),
            read(CANONICAL_ROSTER[0]).as_slice().to_vec(),
            read(CANONICAL_ROSTER[1]).as_slice(),
            &[&fixture.anchor],
            0,
            1_000,
        )
        .is_ok()
    );
}

#[test]
fn mismatched_key_profile_machine_record_and_signature_are_refused() {
    let mut fixture = Fixture::new();
    let other = Fixture::new();
    let other_key = other
        .doc
        .entry(&Audience::Everyone, LEGACY[0])
        .unwrap()
        .value
        .clone();
    fixture.replace_legacy(0, other_key.as_slice());
    assert!(matches!(fixture.migrate(), Migration::Refused(_)));

    let mut fixture = Fixture::new();
    let other_record = other.doc.entry(&Audience::Everyone, LEGACY[1]).unwrap();
    fixture.replace_legacy(1, other_record.value.as_slice());
    assert!(matches!(fixture.migrate(), Migration::Refused(_)));

    let mut fixture = Fixture::new();
    fixture.replace_legacy(1, b"id = 'someone-else'\npubkey = 'invalid'\n");
    assert!(matches!(fixture.migrate(), Migration::Refused(_)));

    let mut fixture = Fixture::new();
    fixture
        .doc
        .set_entry(
            &Audience::Everyone,
            &format!("{LEGACY_ROSTER}.sig"),
            &[0; 64],
        )
        .unwrap();
    assert!(matches!(fixture.migrate(), Migration::Refused(_)));
}

#[test]
fn an_existing_three_file_bundle_gets_its_roster_without_rewriting_credentials() {
    let mut fixture = Fixture::new();
    fixture.doc = fixture.identity_only();
    let unrelated = Fixture::new();
    fixture.replace_legacy(
        0,
        unrelated
            .doc
            .entry(&Audience::Everyone, LEGACY[0])
            .unwrap()
            .value
            .as_slice(),
    );
    assert!(
        fixture
            .doc
            .entry(&Audience::Everyone, CANONICAL_ROSTER[0])
            .is_none()
    );
    let next = fixture.ready();
    for name in CANONICAL {
        assert_eq!(
            next.entry(&Audience::Everyone, name),
            fixture.doc.entry(&Audience::Everyone, name)
        );
    }
    assert!(
        next.entry(&Audience::Everyone, CANONICAL_ROSTER[0])
            .is_some()
    );
    assert!(
        next.entry(&Audience::Everyone, CANONICAL_ROSTER[1])
            .is_some()
    );
}

#[test]
fn a_profile_naming_the_canonical_roster_can_bootstrap_from_the_signed_legacy_pair() {
    let mut fixture = Fixture::new();
    let profile = fixture
        .doc
        .entry(&Audience::Everyone, LEGACY[2])
        .unwrap()
        .value
        .as_str()
        .unwrap()
        .replace(LEGACY_ROSTER, CANONICAL_ROSTER[0]);
    fixture.replace_legacy(2, profile.as_bytes());
    let next = fixture.ready();
    assert_eq!(
        next.entry(&Audience::Everyone, CANONICAL[2])
            .unwrap()
            .value
            .as_slice(),
        profile.as_bytes()
    );
    assert!(
        next.entry(&Audience::Everyone, CANONICAL_ROSTER[0])
            .is_some()
    );
    assert!(
        next.entry(&Audience::Everyone, CANONICAL_ROSTER[1])
            .is_some()
    );
}

#[test]
fn vault_owned_roster_pairs_advance_and_newer_canonical_pairs_never_roll_back() {
    let mut fixture = Fixture::new();
    let first = fixture.ready();
    let home = aterm_tempfile::tempdir().unwrap();
    let (manifest, report) =
        materialize::apply(home.path(), &first.entries(), &Manifest::default(), 1).unwrap();
    assert!(report.failed.is_empty());
    fixture.doc = first.clone();
    fixture.roster.roster_seq = 6;
    fixture.sign_roster();
    let next = fixture.ready();
    let (_, report) = materialize::apply(home.path(), &next.entries(), &manifest, 2).unwrap();
    assert!(report.failed.is_empty());
    for name in CANONICAL_ROSTER {
        let actual =
            fsx::read_private(&materialize::target_path(home.path(), name).unwrap(), 65536)
                .unwrap()
                .unwrap();
        assert_eq!(actual, next.entry(&Audience::Everyone, name).unwrap().value);
    }
    for name in CANONICAL {
        assert_eq!(
            first.entry(&Audience::Everyone, name),
            next.entry(&Audience::Everyone, name)
        );
    }
    fixture.doc = next;
    for name in [LEGACY_ROSTER.to_owned(), format!("{LEGACY_ROSTER}.sig")] {
        let old = first.entry(&Audience::Everyone, &name).unwrap();
        fixture
            .doc
            .set_entry(&Audience::Everyone, &name, old.value.as_slice())
            .unwrap();
    }
    assert!(matches!(fixture.migrate(), Migration::Unchanged));
}

#[test]
fn personal_canonical_roster_files_are_preserved() {
    let next = Fixture::new().ready();
    let home = aterm_tempfile::tempdir().unwrap();
    for name in CANONICAL_ROSTER {
        let path = materialize::target_path(home.path(), name).unwrap();
        fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
        fsx::write_atomic(&path, b"personal roster file", 0o600).unwrap();
    }
    let (_, report) =
        materialize::apply(home.path(), &next.entries(), &Manifest::default(), 1).unwrap();
    assert!(report.failed.is_empty());
    for name in CANONICAL_ROSTER {
        assert_eq!(
            fsx::read_private(&materialize::target_path(home.path(), name).unwrap(), 65536)
                .unwrap()
                .unwrap()
                .as_slice(),
            b"personal roster file"
        );
    }
}

#[test]
fn newer_revocations_and_same_generation_forks_do_not_fall_back_to_old_authority() {
    let mut revoked = Fixture::new();
    revoked.doc = revoked.ready();
    revoked.roster.roster_seq = 6;
    revoked.roster.revoked.push("shared".into());
    revoked.sign_roster();
    assert!(matches!(revoked.migrate(), Migration::Refused(_)));

    let mut fork = Fixture::new();
    fork.doc = fork.ready();
    fork.roster.valid_until = "9998-12-31T00:00:00Z".into();
    fork.sign_roster();
    assert!(matches!(fork.migrate(), Migration::Refused(_)));
}

#[test]
fn identical_roster_bytes_signed_by_two_trusted_masters_are_not_a_fork() {
    let mut fixture = Fixture::new();
    fixture.doc = fixture.ready();
    let other = Fixture::new();
    let raw = fixture
        .doc
        .entry(&Audience::Everyone, LEGACY_ROSTER)
        .unwrap()
        .value
        .clone();
    fixture
        .doc
        .set_entry(
            &Audience::Everyone,
            &format!("{LEGACY_ROSTER}.sig"),
            other.master.sign(raw.as_slice()).as_ref(),
        )
        .unwrap();
    assert!(matches!(
        migrate(&fixture.doc, &[&fixture.anchor, &other.anchor], 5, 1_000),
        Migration::Unchanged
    ));
}

#[test]
fn revoked_expired_and_rolled_back_rosters_are_refused() {
    let mut fixture = Fixture::new();
    fixture.roster.revoked.push("shared".into());
    fixture.sign_roster();
    assert!(matches!(fixture.migrate(), Migration::Refused(_)));
    fixture.roster.revoked.clear();
    fixture.roster.valid_until = "1970-01-01T00:00:01Z".into();
    fixture.sign_roster();
    assert!(matches!(fixture.migrate(), Migration::Refused(_)));
    let fixture = Fixture::new();
    assert!(matches!(
        migrate(&fixture.doc, &[&fixture.anchor], 6, 1_000),
        Migration::Refused(_)
    ));
    assert!(matches!(
        migrate(&fixture.doc, &["untrusted"], 0, 1_000),
        Migration::Refused(_)
    ));
}

#[test]
fn unprotected_or_incomplete_legacy_bundles_are_not_copied() {
    let mut fixture = Fixture::new();
    let key = fixture
        .doc
        .entry(&Audience::Everyone, LEGACY[0])
        .unwrap()
        .value
        .clone();
    fixture
        .doc
        .set_entry_with(&Audience::Everyone, LEGACY[0], key.as_slice(), true, None)
        .unwrap();
    assert!(matches!(fixture.migrate(), Migration::Refused(_)));
    let identity = Identity::generate().unwrap();
    let doc = Document::new_vault("admin", &identity.recipient().to_bech32());
    assert!(matches!(migrate(&doc, &[], 0, 1_000), Migration::Unchanged));
}

fn alab_config() -> Config {
    Config {
        source: Source::Url("http://fixture.invalid/vault/alab".into()),
        name: crate::ALAB_VAULT.into(),
        signer: decode_key32(crate::ALAB_SIGNER),
        followed: Vec::new(),
        host: "admin".into(),
    }
}

#[test]
fn migration_failure_leaves_the_join_pass_running_and_is_retryable() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let floor = paths.dir.join("release-ring-roster-floor");
    fsx::write_atomic(&floor, b"damaged", 0o600).unwrap();
    let lines = std::cell::RefCell::new(Vec::new());
    let mut rounds = Rounds::default();
    assert!(!rounds.step(&paths, &alab_config(), 1, &|line| {
        lines.borrow_mut().push(line.to_owned())
    }));
    assert!(lines.borrow()[0].contains("roster floor is invalid"));
    assert!(
        rounds.checked.is_none(),
        "a failed inspection is never settled"
    );
    // The boolean is the sole early-return signal in approver_pass: an error stays false,
    // permitting its ordinary hear_joins/auto_approve_listed path to run in the same pass.
    fsx::write_atomic(&floor, b"0\n", 0o600).unwrap();
    rounds.failed_at = Some(Instant::now() - Duration::from_secs(3));
    assert!(!rounds.step(&paths, &alab_config(), 1, &|line| {
        lines.borrow_mut().push(line.to_owned())
    }));
    assert_eq!(
        lines.borrow().len(),
        2,
        "the next pass attempts admin_open after the floor is repaired"
    );
    assert!(!lines.borrow()[1].contains("roster floor is invalid"));
}

#[test]
fn another_vault_and_explicit_off_never_open_or_modify_release_state() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&home.path().join("member"));
    let mut config = alab_config();
    config.signer = Some([0; 32]);
    let mut rounds = Rounds::default();
    assert!(!rounds.step(&paths, &config, 1, &|_| panic!("not applicable")));
    assert!(!paths.home.exists());
    let mut another = alab_config();
    another.name = "another-vault".into();
    assert!(!rounds.step(&paths, &another, 1, &|_| panic!("not the default vault")));
    assert!(!paths.home.exists());
    crate::service::record_subscriber_off(&paths.home).unwrap();
    assert!(!rounds.step(&paths, &alab_config(), 1, &|_| panic!("explicit off")));
    assert!(!paths.identity_file().exists());
    assert!(!paths.source_file().exists());
    assert!(!paths.dir.join("release-ring-roster-floor").exists());
}

#[test]
fn a_changed_floor_invalidates_the_current_inspection() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let config = alab_config();
    let mut rounds = Rounds {
        checked: Some(Checked {
            source: source_identity(&config),
            seq: 1,
            floor: 0,
            at: Instant::now(),
        }),
        failed_at: None,
    };
    assert!(!rounds.step(&paths, &config, 1, &|_| panic!("settled")));
    fsx::write_atomic(&paths.dir.join("release-ring-roster-floor"), b"5\n", 0o600).unwrap();
    let lines = std::cell::RefCell::new(Vec::new());
    assert!(!rounds.step(&paths, &config, 1, &|line| {
        lines.borrow_mut().push(line.to_owned())
    }));
    assert_eq!(
        lines.borrow().len(),
        1,
        "changed floor requires a fresh authenticated admin read"
    );
}

/// NEGATIVE CONTROL: `field(&profile, "machine_roster").unwrap_or(LEGACY_ROSTER)` silently
/// defaulted empty or non-string `machine_roster` fields to the legacy path instead of refusing.
#[test]
fn empty_or_non_string_machine_roster_in_legacy_profile_is_refused() {
    for replacement in ["machine_roster = \"\"", "machine_roster = 42"] {
        let mut fixture = Fixture::new();
        let profile = fixture
            .doc
            .entry(&Audience::Everyone, LEGACY[2])
            .unwrap()
            .value
            .as_str()
            .unwrap()
            .replace(
                &format!("machine_roster = \"{LEGACY_ROSTER}\""),
                replacement,
            );
        fixture.replace_legacy(2, profile.as_bytes());
        assert!(
            matches!(fixture.migrate(), Migration::Refused(_)),
            "{replacement}"
        );
    }
}
