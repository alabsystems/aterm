// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(super) struct Fixture {
    pub(super) _root: aterm_tempfile::TempDir,
    pub(super) paths: Paths,
    source: PathBuf,
}

impl Fixture {
    pub(super) fn new() -> Self {
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

    pub(super) fn publish(&self, change: impl FnOnce(&mut Document)) {
        let admin = admin_open(&self.paths).unwrap();
        let mut doc = admin.doc.clone();
        change(&mut doc);
        admin_publish(&self.paths, &admin, &doc).unwrap();
    }

    pub(super) fn read_token(&self) -> Option<VerifiedEntry> {
        read_verified_entry(&self.paths, &config(&self.paths).unwrap().unwrap(), "token").unwrap()
    }

    pub(super) fn tracked(&self) -> Vec<PathBuf> {
        vec![
            self.paths.state_file(),
            self.paths.source_file(),
            self.paths.cache_file(),
            self.paths.identity_file(),
            self.paths.lock_file(),
            self.source.clone(),
            old_path(&self.paths.home, "token"),
            old_path(&self.paths.home, "other"),
        ]
    }

    pub(super) fn bytes(&self) -> Vec<Option<Vec<u8>>> {
        self.tracked()
            .iter()
            .map(|p| std::fs::read(p).ok())
            .collect()
    }
}

/// Where an older build wrote `name` under `home`: `~/<path>` for a home path, else
/// `~/.secrets/<name>`. A file there is the person's or that build's; no read consults it.
fn old_path(home: &Path, name: &str) -> PathBuf {
    match name.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => home.join(".secrets").join(name),
    }
}

/// Write `bytes` at `name`'s [`old_path`], as a person or an older build left it.
fn plant(home: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = old_path(home, name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    fsx::write_atomic(&path, bytes, 0o600).unwrap();
    path
}

fn set_state(paths: &Paths, change: impl FnOnce(&mut State)) {
    let mut state = load_state(paths).unwrap();
    change(&mut state);
    save_state(paths, &state).unwrap();
}

fn publish_public_pair(f: &Fixture, roster: &[u8], signature: &[u8]) {
    f.publish(|doc| {
        for (index, value) in [roster, signature].into_iter().enumerate() {
            doc.set_entry_with(
                &Audience::Everyone,
                PUBLIC_ROSTER_NAMES[index],
                value,
                true,
                None,
            )
            .unwrap();
        }
        for (index, value) in [roster, signature].into_iter().enumerate() {
            doc.set_entry_with(
                &Audience::Everyone,
                PUBLIC_ROSTER_NAMES[index],
                value,
                true,
                Some(PUBLIC_ROSTER_NAMES[1 - index]),
            )
            .unwrap();
        }
    });
}

#[test]
fn authenticated_local_default_policy_preserves_always_managed_and_anchor_boundaries() {
    let f = Fixture::new();
    assert!(!f.read_token().unwrap().unanchored_default());
    f.publish(|doc| {
        doc.set_entry_with(
            &Audience::Everyone,
            "token",
            b"inert-authentic-token",
            true,
            None,
        )
        .unwrap();
    });
    assert!(f.read_token().unwrap().unanchored_default());
    f.publish(|doc| {
        doc.set_entry_with(&Audience::Everyone, "other", b"inert-other", true, None)
            .unwrap();
        doc.set_entry_with(
            &Audience::Everyone,
            "token",
            b"inert-authentic-token",
            true,
            Some("other"),
        )
        .unwrap();
    });
    assert!(!f.read_token().unwrap().unanchored_default());
}

#[test]
fn public_roster_reads_current_pair_without_adopting_stale_files() {
    let f = Fixture::new();
    for name in PUBLIC_ROSTER_NAMES {
        plant(&f.paths.home, name, b"personal old pair");
    }
    let signature = [7; 64];
    publish_public_pair(&f, b"new public roster including a revocation", &signature);
    let before = f.bytes();
    let state = load_state(&f.paths).unwrap();
    assert!(
        PUBLIC_ROSTER_NAMES
            .iter()
            .all(|name| !state.legacy_manifest.contains_key(*name))
    );
    // An ordinary read of the complete pair's roster reads the vault in place, not the person's
    // old file at its path.
    assert_eq!(
        read_verified_entry(
            &f.paths,
            &config(&f.paths).unwrap().unwrap(),
            PUBLIC_ROSTER_NAMES[0]
        )
        .unwrap()
        .unwrap()
        .value()
        .as_slice(),
        b"new public roster including a revocation"
    );
    let PublicRosterRead::Pair(pair) = current_public_roster(&f.paths).unwrap() else {
        panic!("current public pair unavailable");
    };
    assert_eq!(pair.roster, b"new public roster including a revocation");
    assert_eq!(pair.signature, signature);
    assert_eq!(f.bytes(), before, "observation changes no vault state");
    for name in PUBLIC_ROSTER_NAMES {
        assert_eq!(
            std::fs::read(old_path(&f.paths.home, name)).unwrap(),
            b"personal old pair"
        );
    }
}

#[test]
fn public_roster_absence_is_authenticated_and_partial_pairs_are_errors() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert!(matches!(
        current_public_roster(&Paths::for_home(root.path())).unwrap(),
        PublicRosterRead::Unconfigured
    ));
    let f = Fixture::new();
    assert!(matches!(
        current_public_roster(&f.paths).unwrap(),
        PublicRosterRead::Absent
    ));
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, PUBLIC_ROSTER_NAMES[0], b"only body")
            .unwrap();
    });
    assert!(current_public_roster(&f.paths).is_err());
    publish_public_pair(&f, b"body", b"short signature");
    let PublicRosterRead::Pair(pair) = current_public_roster(&f.paths).unwrap() else {
        panic!("opaque pair unavailable");
    };
    assert_eq!(
        pair.signature, b"short signature",
        "the release admission policy, not this source reader, rejects invalid signatures"
    );
    let hidden = Fixture::new();
    hidden.publish(|doc| {
        for name in PUBLIC_ROSTER_NAMES {
            doc.set_entry(
                &Audience::Hosts(vec!["peer".into()]),
                name,
                b"other recipient's entry",
            )
            .unwrap();
        }
    });
    assert!(matches!(
        current_public_roster(&hidden.paths).unwrap(),
        PublicRosterRead::Absent
    ));
}

#[test]
fn public_roster_refuses_unavailable_or_noncanonical_source() {
    for fault in ["busy", "failed", "revoked", "denied", "source", "audience"] {
        let f = Fixture::new();
        publish_public_pair(&f, b"public body", &[8; 64]);
        assert!(matches!(
            current_public_roster(&f.paths).unwrap(),
            PublicRosterRead::Pair(_)
        ));
        let guard = match fault {
            "busy" => Some(StateLock::take(&f.paths).unwrap()),
            "failed" => {
                set_state(&f.paths, |s| s.failed = true);
                None
            }
            "revoked" => {
                set_state(&f.paths, |s| s.membership = Stored::Revoked);
                None
            }
            "denied" => {
                f.publish(|doc| {
                    let roster = crate::swarm_roster::SwarmRoster {
                        deny_labels: vec!["m22".into()],
                        members: vec![crate::swarm_roster::SwarmMember {
                            recipient: read_identity(&f.paths)
                                .unwrap()
                                .unwrap()
                                .recipient()
                                .to_bech32(),
                            vault_name: "admin".into(),
                            label: "m22".into(),
                            aliases: Vec::new(),
                            hint: None,
                            hw_hint: None,
                            admitted_ms: 1,
                            claimed_label: None,
                            claimed_at_ms: None,
                            node_id: None,
                            node_key: None,
                        }],
                        ..Default::default()
                    };
                    assert!(roster.is_member_denied(&roster.members[0]));
                    doc.set_entry(
                        &Audience::Everyone,
                        crate::SWARM_ROSTER_ENTRY,
                        roster.to_toml().as_bytes(),
                    )
                    .unwrap();
                });
                None
            }
            "source" => {
                let mut changed = config(&f.paths).unwrap().unwrap();
                changed.source = Source::Path(f.paths.home.join("other.vault"));
                save_config(&f.paths, &changed).unwrap();
                None
            }
            "audience" => {
                f.publish(|doc| {
                    doc.set_entry_with(
                        &Audience::Hosts(vec!["admin".into()]),
                        PUBLIC_ROSTER_NAMES[0],
                        b"host override",
                        true,
                        None,
                    )
                    .unwrap();
                });
                None
            }
            _ => unreachable!(),
        };
        assert!(
            current_public_roster_until(&f.paths, Instant::now()).is_err(),
            "{fault}"
        );
        drop(guard);
    }
}

/// A release's stage read the roster at the moment the running aterm's vault sync held the
/// state, and refused the cut as `busy or unavailable` (0.155.0, 2026-11-04). A hold that ends
/// while the read waits lets it go ahead. The hold ends on the reader's first blocked try, in
/// the reader's own thread, so no scheduling races the wait. CONTROL: the same hold kept past a
/// deadline already reached still refuses as busy.
#[test]
fn public_roster_waits_out_a_writer_holding_the_vault_state() {
    let f = Fixture::new();
    publish_public_pair(&f, b"public body", &[8; 64]);
    let held = StateLock::take(&f.paths).unwrap();
    let err = current_public_roster_until(&f.paths, Instant::now()).unwrap_err();
    assert!(err.to_string().contains("busy or unavailable"), "{err}");
    let expired = current_public_roster_after(&f.paths, Duration::from_secs(61)).unwrap_err();
    assert!(
        expired.to_string().contains("busy or unavailable"),
        "{expired}"
    );

    let paths = &f.paths;
    let (read, blocked) = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let blocked = std::rc::Rc::new(std::cell::Cell::new(0));
                let count = std::rc::Rc::clone(&blocked);
                let mut held = Some(held);
                crate::client::OBSERVE_BLOCKED.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        count.set(count.get() + 1);
                        drop(held.take());
                    }));
                });
                let read = current_public_roster_after(paths, Duration::from_secs(31));
                crate::client::OBSERVE_BLOCKED.with(|hook| hook.borrow_mut().take());
                (read, blocked.get())
            })
            .join()
            .unwrap()
    });
    let PublicRosterRead::Pair(pair) = read.unwrap() else {
        panic!("the pair once the writer let go");
    };
    assert_eq!(blocked, 1, "the read found the state held once");
    assert_eq!(pair.roster, b"public body");
}

#[test]
fn mailbox_component_index_is_bound_to_verified_source_and_exact_fields() {
    use crate::mailbox_component::{Component, Part, SOURCE_ENTRY};
    let f = Fixture::new();
    let password =
        Component::for_mailbox("https://mail.example/", "owner@example", Part::Password).unwrap();
    let second = Component::for_mailbox(
        "https://mail.example/",
        "owner@example",
        Part::SecondPassword,
    )
    .unwrap();
    let other =
        Component::for_mailbox("https://other.example/", "owner@example", Part::Password).unwrap();
    assert!(!mailbox_component_held(&f.paths, password).unwrap());
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, b"user = 'owner@example'\nwebmail_url = 'https://mail.example/'\npassword = 'fixture-login'\nmailbox_password = 'fixture-second'\n").unwrap();
    });
    assert!(mailbox_component_held(&f.paths, password).unwrap());
    assert!(mailbox_component_held(&f.paths, second).unwrap());
    assert!(!mailbox_component_held(&f.paths, other).unwrap());
    assert_eq!(
        read_mailbox_component(&f.paths, password)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"fixture-login"
    );
    assert_eq!(
        read_mailbox_component(&f.paths, second)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"fixture-second"
    );
    let index = f.paths.dir.join("mailbox-components.toml");
    let text = std::fs::read_to_string(&index).unwrap();
    assert!(
        !text.contains("fixture-login")
            && !text.contains("fixture-second")
            && !text.contains("owner@example")
    );
    std::fs::remove_file(&index).unwrap();
    let reads = MAILBOX_READS.with(std::cell::Cell::get);
    assert!(!mailbox_component_held(&f.paths, password).unwrap());
    assert_eq!(
        reads,
        MAILBOX_READS.with(std::cell::Cell::get),
        "a missing-index holder must not open mailbox plaintext"
    );
    assert!(matches!(
        sync(&f.paths, None).unwrap(),
        SyncOutcome::Unchanged { .. }
    ));
    assert!(
        MAILBOX_READS.with(std::cell::Cell::get) > reads,
        "authorized unchanged sync backfills the index"
    );
    assert!(mailbox_component_held(&f.paths, password).unwrap());
    // A file at the source's old path is no source: the index and the read follow the vault.
    plant(
        &f.paths.home,
        SOURCE_ENTRY,
        b"user = 'owner@example'\npassword = 'untrusted-change'\n",
    );
    assert!(mailbox_component_held(&f.paths, password).unwrap());
    assert_eq!(
        read_mailbox_component(&f.paths, password)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"fixture-login"
    );
}

/// A verified read of the registered entry finds the index already saying what it would write
/// and leaves the file as it is: an authorized component read and an explicit refresh each
/// came with the same text and replaced the file (a new inode every read, 2026-11-11). An
/// index that is gone is written again by the same refresh.
/// NEGATIVE CONTROL (2026-11-11): with the index written by `fsx::write_atomic` again, the stamp
/// after the reads is another file's (`the index is left as it is`).
#[test]
fn mailbox_component_index_is_not_rewritten_with_what_it_holds() {
    use crate::mailbox_component::{Component, Part, SOURCE_ENTRY};
    let f = Fixture::new();
    let password =
        Component::for_mailbox("https://mail.example/", "owner@example", Part::Password).unwrap();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, b"user = 'owner@example'\nwebmail_url = 'https://mail.example/'\npassword = 'fixture-login'\n").unwrap();
    });
    let index = f.paths.dir.join("mailbox-components.toml");
    let written = fsx::stamp(&index).unwrap();
    let reads = MAILBOX_READS.with(std::cell::Cell::get);
    assert_eq!(
        read_mailbox_component(&f.paths, password)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"fixture-login"
    );
    assert!(refresh_mailbox_component_index(&f.paths).unwrap());
    assert!(
        MAILBOX_READS.with(std::cell::Cell::get) > reads,
        "each was a verified read of the entry"
    );
    assert_eq!(
        fsx::stamp(&index),
        Some(written),
        "the index is left as it is"
    );
    assert!(mailbox_component_held(&f.paths, password).unwrap());

    std::fs::remove_file(&index).unwrap();
    assert!(refresh_mailbox_component_index(&f.paths).unwrap());
    assert!(mailbox_component_held(&f.paths, password).unwrap());
}

#[test]
fn mailbox_component_index_excludes_ambiguous_fields_and_stale_versions() {
    use crate::mailbox_component::{Component, Part, SOURCE_ENTRY};
    let f = Fixture::new();
    let password =
        Component::for_mailbox("https://mail.example/", "owner@example", Part::Password).unwrap();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, b"user = 'owner@example'\nwebmail_url = 'https://mail.example/'\npassword = 'fixture-one'\n").unwrap();
    });
    let index = f.paths.dir.join("mailbox-components.toml");
    let old = std::fs::read(&index).unwrap();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "other", b"new-version")
            .unwrap();
    });
    std::fs::write(&index, &old).unwrap();
    assert!(!mailbox_component_held(&f.paths, password).unwrap());
    assert!(
        read_mailbox_component(&f.paths, password)
            .unwrap()
            .is_none()
    );
    assert!(refresh_mailbox_component_index(&f.paths).unwrap());
    assert!(mailbox_component_held(&f.paths, password).unwrap());
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, b"user = 'owner@example'\nwebmail_url = 'https://mail.example/'\npassword = 'fixture-one'\n[[mailboxes]]\nlogin = 'owner@example'\nwebmail_url = 'https://mail.example/'\npassword = 'fixture-two'\n").unwrap();
    });
    assert!(!mailbox_component_held(&f.paths, password).unwrap());
    std::fs::write(index, old).unwrap();
    assert!(!mailbox_component_held(&f.paths, password).unwrap());
    assert!(
        read_mailbox_component(&f.paths, password)
            .unwrap()
            .is_none()
    );
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
    fork.vault.envelope[0] ^= 1;
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
            .as_ref()
            .unwrap()
            .snapshot()
            .continues(first.entry.unwrap().snapshot())
    );
    let other = observe_verified_entry(&f.paths, &config, "other")
        .unwrap()
        .unwrap();
    assert!(!other.version.not_before(&restored.version));
    // A forged file at the entry's old path changes neither the version nor the bytes: the
    // observation reads the vault in place.
    plant(&f.paths.home, "token", b"forged");
    let forged = observe_verified_entry(&f.paths, &config, "token")
        .unwrap()
        .unwrap();
    assert!(forged.version.not_before(&restored.version));
    assert!(restored.version.not_before(&forged.version));
    let entry = forged.entry.unwrap();
    assert_eq!(entry.value().as_slice(), b"inert-authentic-token");
    assert!(
        entry
            .snapshot()
            .continues(restored.entry.as_ref().unwrap().snapshot())
    );
}

#[test]
fn entry_versions_require_retained_log_facts_even_with_an_unchanged_envelope() {
    let f = Fixture::new();
    let mut first = f.read_token().unwrap().snapshot().version();
    first.vault.log_ids = Some(std::sync::Arc::new([[1; 32]].into_iter().collect()));
    let mut extended = first.clone();
    extended.vault.log_ids = Some(std::sync::Arc::new(
        [[1; 32], [2; 32]].into_iter().collect(),
    ));
    assert!(extended.not_before(&first));
    assert!(!first.not_before(&extended));
    let mut replaced = extended.clone();
    replaced.vault.log_ids = Some(std::sync::Arc::new(
        [[2; 32], [3; 32], [4; 32]].into_iter().collect(),
    ));
    assert!(!replaced.not_before(&first));
    replaced.vault.log_ids = None;
    assert!(!replaced.not_before(&first));

    // A public frontier may redundantly name an ancestor. That changes its encoding,
    // not the causal prefix it represents, including when checking a published candidate.
    seed_vault_genesis(&f.paths, &f._root.path().join("version.genesis")).unwrap();
    let held = super::super::load_records(&f.paths).unwrap();
    let log = held.log().unwrap();
    let cfg = config(&f.paths).unwrap().unwrap();
    let state = load_state(&f.paths).unwrap();
    let actual = super::super::version::AuthenticatedVersion::from_source(
        &cfg.name,
        cfg.signer.unwrap(),
        cfg.followed.clone(),
        Some(super::super::version::EnvelopeVersion {
            seq: state.seq,
            digest: state.digest.unwrap(),
        }),
        Some(log),
    )
    .unwrap();
    let envelope_only = super::super::version::VersionClaim::new(
        cfg.name.clone(),
        cfg.signer.unwrap(),
        actual.claim().envelope().cloned(),
        None,
    )
    .unwrap();
    use super::super::version::VersionRelation::Same;
    assert_eq!(actual.relation(&envelope_only), Same);
    assert!(actual.refines_for_recovery(&envelope_only));
    assert!(!actual.refines_for_recovery(actual.claim()));
    let mut other_envelope = envelope_only.envelope().unwrap().clone();
    other_envelope.digest[0] ^= 1;
    let other_envelope = super::super::version::VersionClaim::new(
        cfg.name.clone(),
        cfg.signer.unwrap(),
        Some(other_envelope),
        None,
    )
    .unwrap();
    assert!(!actual.refines_for_recovery(&other_envelope));
    let mut redundant = log.heads();
    assert!(!redundant.contains(log.network()));
    redundant.push(*log.network());
    redundant.sort_unstable();
    let redundant = super::super::version::VersionClaim::new(
        cfg.name.clone(),
        cfg.signer.unwrap(),
        actual.claim().envelope().cloned(),
        Some(super::super::version::LogVersion {
            network: *log.network(),
            heads: redundant,
        }),
    )
    .unwrap();
    assert_eq!(actual.relation(&redundant), Same);
    assert_eq!(actual.relation_between(&redundant, actual.claim()), Same);
    assert_eq!(actual.relation_between(actual.claim(), &redundant), Same);
    let log_only = super::super::version::AuthenticatedVersion::from_source(
        &cfg.name,
        cfg.signer.unwrap(),
        cfg.followed.clone(),
        None,
        Some(log),
    )
    .unwrap();
    assert!(!log_only.refines_for_recovery(&envelope_only));
    assert!(!actual.refines_for_recovery(log_only.claim()));
    assert_eq!(actual.relation(log_only.claim()), Same);
    assert_eq!(
        log_only.relation_between(actual.claim(), log_only.claim()),
        Same,
        "a custodian's existing envelope does not advance the recipient's same log prefix"
    );
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "token", b"new-log-value")
            .unwrap();
    });
    let next_store = super::super::load_records(&f.paths).unwrap();
    let next_state = load_state(&f.paths).unwrap();
    let next = super::super::version::AuthenticatedVersion::from_source(
        &cfg.name,
        cfg.signer.unwrap(),
        cfg.followed.clone(),
        Some(super::super::version::EnvelopeVersion {
            seq: next_state.seq,
            digest: next_state.digest.unwrap(),
        }),
        next_store.log(),
    )
    .unwrap();
    use super::super::version::VersionRelation::Advanced;
    assert_eq!(next.relation(log_only.claim()), Advanced);
    let next_log = super::super::version::AuthenticatedVersion::from_source(
        &cfg.name,
        cfg.signer.unwrap(),
        cfg.followed.clone(),
        None,
        next_store.log(),
    )
    .unwrap();
    assert_eq!(
        next_log.relation_between(next.claim(), log_only.claim()),
        Advanced
    );
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "other", b"later-log-value")
            .unwrap();
    });
    let latest_store = super::super::load_records(&f.paths).unwrap();
    let latest_state = load_state(&f.paths).unwrap();
    let latest = super::super::version::AuthenticatedVersion::from_source(
        &cfg.name,
        cfg.signer.unwrap(),
        cfg.followed.clone(),
        Some(super::super::version::EnvelopeVersion {
            seq: latest_state.seq,
            digest: latest_state.digest.unwrap(),
        }),
        latest_store.log(),
    )
    .unwrap();
    assert_eq!(latest.relation(next_log.claim()), Advanced);
    assert_eq!(
        latest.relation_between(next.claim(), next_log.claim()),
        Same,
        "a newer local view cannot make an older candidate advance its own prefix"
    );
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
    floor.vault.seq = 1;
    floor.vault.envelope = [1; 32];
    floor.vault.log_ids = Some(std::sync::Arc::new([[1; 32]].into_iter().collect()));
    let mut candidate = floor.clone();
    let scenario = match action {
        "Same" => 1,
        "Advance" => {
            candidate.vault.seq = 2;
            2
        }
        "Rollback" => {
            candidate.vault.seq = 0;
            3
        }
        "Fork" => {
            candidate.vault.envelope = [2; 32];
            4
        }
        "LogExtend" => {
            candidate.vault.log_ids = Some(std::sync::Arc::new(
                [[1; 32], [2; 32]].into_iter().collect(),
            ));
            5
        }
        "LogDrop" => {
            candidate.vault.log_ids = None;
            6
        }
        "LogSwap" => {
            candidate.vault.log_ids = Some(std::sync::Arc::new(
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
        candidate.vault.seq >= floor.vault.seq
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
fn a_selected_reserved_read_distinguishes_a_conditional_host_winner_from_absence() {
    const NAME: &str = "aterm-selected-control";
    for log_backed in [false, true] {
        let f = Fixture::new();
        f.publish(|doc| {
            doc.set_entry(&Audience::Everyone, NAME, b"everyone-value")
                .unwrap();
            doc.set_entry_with(&Audience::Everyone, "control-anchor", b"anchor", true, None)
                .unwrap();
            doc.set_entry_with(
                &Audience::Hosts(vec!["admin".into()]),
                NAME,
                b"conditional-host-value",
                true,
                Some("control-anchor"),
            )
            .unwrap();
        });
        if log_backed {
            seed_vault_genesis(&f.paths, &f._root.path().join("selected.genesis")).unwrap();
            admin_mirror_log(&f.paths, &[]).unwrap();
        }
        let expected = config(&f.paths).unwrap().unwrap();
        assert!(
            observe_reserved_entry(&f.paths, &expected, NAME)
                .unwrap()
                .is_none(),
            "the surviving conditional host winner must not expose Everyone"
        );
        let absent = observe_reserved_entry(&f.paths, &expected, "aterm-selected-absent")
            .unwrap()
            .expect("an authenticated absent entry still has its source version");
        assert!(absent.entry.is_none());
    }
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
        (
            "busy",
            "a vault pass on this machine has held the vault's state lock",
        ),
    ] {
        let guard = match fault {
            "failed" => {
                set_state(&f.paths, |s| s.failed = true);
                None
            }
            "busy" => Some(StateLock::take(&f.paths).unwrap()),
            _ => unreachable!(),
        };
        let read = with_current_wait(Duration::from_millis(50), || {
            fleet::reserved_code(&f.paths, &expected)
        });
        drop(guard);
        match read {
            Err(error) => assert!(error.to_string().contains(said), "{fault}: {error}"),
            Ok(code) => panic!("{fault}: read as {:?}", code.map(|_| "a code")),
        }
        save_state(&f.paths, &held).unwrap();
    }
    assert!(fleet::reserved_code(&f.paths, &expected).unwrap().is_some());
    // A pass released within the wait is waited out: the code is read, not refused. NEGATIVE
    // CONTROL (2026-11-11): with `StateLock::observe_judging` observing once, this read is the
    // busy error the converge pass's `sync.fleet` row failed on.
    let mut pass = Some(StateLock::take(&f.paths).unwrap());
    let (read, tries) = test_while_state_lock_held(
        move || drop(pass.take()),
        || fleet::reserved_code(&f.paths, &expected),
    );
    assert!(read.unwrap().is_some());
    assert!(tries >= 1, "the read met the held pass and waited");
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

/// Restores the anchor walk when dropped ([`HIDDEN_ANCHOR`]).
pub(super) struct HiddenAnchor;

impl Drop for HiddenAnchor {
    fn drop(&mut self) {
        HIDDEN_ANCHOR.set(None);
    }
}

/// Publish `name` put-if-absent, anchored to a put-if-absent `unreceived-anchor`, and read that
/// anchor as absent from this reader's view while the returned guard lives: `name` is present in
/// the version, its chain unresolved. The document check refuses any envelope that holds such a
/// chain, so the walk's test seam stands in for it.
pub(super) fn publish_unresolved(f: &Fixture, name: &str, value: &[u8]) -> HiddenAnchor {
    f.publish(|doc| {
        doc.set_entry_with(
            &Audience::Everyone,
            "unreceived-anchor",
            b"inert-anchor",
            true,
            None,
        )
        .unwrap();
        doc.set_entry_with(
            &Audience::Everyone,
            name,
            value,
            true,
            Some("unreceived-anchor"),
        )
        .unwrap();
    });
    hide_anchor("unreceived-anchor")
}

/// Read `name` as absent from the view in the anchor walk while the returned guard lives.
pub(super) fn hide_anchor(name: &str) -> HiddenAnchor {
    HIDDEN_ANCHOR.set(Some(name.into()));
    HiddenAnchor
}

#[test]
fn if_absent_entries_read_their_vault_value_whatever_a_local_file_holds() {
    let f = Fixture::new();
    let name = "local-value";
    // The person's own file was there first; the read ignores it.
    let path = plant(&f.paths.home, name, b"person-inert-value");
    f.publish(|doc| {
        doc.set_entry_with(&Audience::Everyone, name, b"vault-inert-value", true, None)
            .unwrap()
    });
    let expected = config(&f.paths).unwrap().unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        read_verified_entry(&f.paths, &expected, name)
            .unwrap()
            .unwrap()
            .value()
            .as_slice(),
        b"vault-inert-value"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the read writes no file"
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        read_verified_entry(&f.paths, &expected, name)
            .unwrap()
            .unwrap()
            .value()
            .as_slice(),
        b"vault-inert-value"
    );
    assert!(!path.exists(), "the read writes no file");
    // NEGATIVE CONTROL: an if-absent entry whose anchor chain does not resolve in this view is
    // unavailable: present in the version, never read.
    let hidden = publish_unresolved(&f, name, b"vault-inert-value");
    let observed = observe_verified_entry(&f.paths, &expected, name)
        .unwrap()
        .unwrap();
    assert!(observed.entry.is_none());
    assert!(
        read_verified_entry(&f.paths, &expected, name)
            .unwrap()
            .is_none()
    );
    // With its anchor in the view again, the same entry reads.
    drop(hidden);
    assert_eq!(
        read_verified_entry(&f.paths, &expected, name)
            .unwrap()
            .unwrap()
            .value()
            .as_slice(),
        b"vault-inert-value"
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

/// A vault holding a Developer ID key (anchored to its certificate when `anchored`), with the
/// copy an older build wrote at the key's old home path, identical bytes.
fn signing_fixture(anchored: bool) -> Fixture {
    let f = Fixture::new();
    plant(
        &f.paths.home,
        "~/.aterm/apple/devid-existing.key",
        b"inert-existing-key",
    );
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
fn signing_material_reads_in_place_and_claims_nothing() {
    let f = signing_fixture(false);
    let name = "~/.aterm/apple/devid-existing.key";
    let expected = config(&f.paths).unwrap().unwrap();
    let key = old_path(&f.paths.home, name);
    let state = std::fs::read(f.paths.state_file()).unwrap();
    // The file at the old path, as written, edited, then gone: each read returns the vault's
    // bytes, through the signing reader and the ordinary one alike, and changes nothing.
    for file in ["kept", "edited", "missing"] {
        match file {
            "kept" => {}
            "edited" => fsx::write_atomic(&key, b"foreign-key", 0o600).unwrap(),
            "missing" => std::fs::remove_file(&key).unwrap(),
            _ => unreachable!(),
        }
        let held = std::fs::read(&key).ok();
        let signing = read_verified_entry(&f.paths, &expected, name)
            .map(|read| read.map(|entry| entry.into_parts().0))
            .unwrap()
            .unwrap();
        let ordinary = read_verified_entry(&f.paths, &expected, name)
            .unwrap()
            .unwrap();
        assert_eq!(signing.as_slice(), b"inert-existing-key", "{file}");
        assert_eq!(ordinary.value().as_slice(), b"inert-existing-key", "{file}");
        assert_eq!(
            std::fs::read(f.paths.state_file()).unwrap(),
            state,
            "{file}"
        );
        assert_eq!(std::fs::read(&key).ok(), held, "{file}");
    }
    // NEGATIVE CONTROL: the vault is what the read follows; a rotated key reads rotated.
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, name, b"inert-rotated-key")
            .unwrap();
    });
    assert_eq!(
        read_verified_entry(&f.paths, &expected, name)
            .map(|read| read.map(|entry| entry.into_parts().0))
            .unwrap()
            .unwrap()
            .as_slice(),
        b"inert-rotated-key"
    );
    for other in [
        "token",
        "~/.aterm/apple/devid-nested/other.key",
        "~/.aterm/apple/devid-existing.txt",
    ] {
        assert!(!crate::entry_class::signing_material(other), "{other}");
    }
}

fn publication_fixture(name: &str) -> Fixture {
    let f = Fixture::new();
    plant(&f.paths.home, name, b"inert-publication-token");
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, name, b"inert-publication-token")
            .unwrap();
    });
    f
}

#[test]
fn publication_reader_reads_the_two_token_names_in_place_and_claims_nothing() {
    for name in ["gh_access_token", "gh_access_token_alabsystems"] {
        let f = publication_fixture(name);
        let expected = config(&f.paths).unwrap().unwrap();
        let path = old_path(&f.paths.home, name);
        let before = std::fs::read(f.paths.state_file()).unwrap();
        // A forged file at the old path changes nothing: every reader returns the vault's bytes.
        fsx::write_atomic(&path, b"forged-publication-token", 0o600).unwrap();
        assert_eq!(
            read_verified_entry(&f.paths, &expected, name)
                .unwrap()
                .unwrap()
                .value()
                .as_slice(),
            b"inert-publication-token"
        );
        let entry = read_publication_material(&f.paths, &expected, name)
            .unwrap()
            .unwrap();
        assert_eq!(entry.value().as_slice(), b"inert-publication-token");
        for again in [
            entry.snapshot().read(&f.paths).unwrap(),
            entry.snapshot().read_publication(&f.paths).unwrap(),
        ] {
            assert_eq!(
                again.unwrap().value().as_slice(),
                b"inert-publication-token"
            );
        }
        assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), before);
        assert_eq!(std::fs::read(&path).unwrap(), b"forged-publication-token");
        // NEGATIVE CONTROL: the narrow reading names only the two token entries.
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
fn publication_token_and_snapshot_refuse_stale_foreign_or_revoked_material() {
    let name = "gh_access_token";
    for fault in ["none", "audience", "revoked", "source", "stale", "failed"] {
        let f = publication_fixture(name);
        let expected = config(&f.paths).unwrap().unwrap();
        let entry = read_publication_material(&f.paths, &expected, name)
            .unwrap()
            .unwrap();
        match fault {
            "none" => {}
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
            "stale" => set_state(&f.paths, |s| s.seq = 0),
            "failed" => set_state(&f.paths, |s| s.failed = true),
            _ => unreachable!(),
        }
        let before = std::fs::read(f.paths.state_file()).unwrap();
        let read = read_publication_material(&f.paths, &expected, name);
        let again = entry.snapshot().read_publication(&f.paths);
        // NEGATIVE CONTROL: with no fault, both reads return the vault's bytes.
        let accepted = |read: &crate::Result<Option<VerifiedEntry>>| {
            read.as_ref().is_ok_and(|read| {
                read.as_ref()
                    .is_some_and(|entry| entry.value().as_slice() == b"inert-publication-token")
            })
        };
        assert_eq!(accepted(&read), fault == "none", "{fault}");
        assert_eq!(accepted(&again), fault == "none", "snapshot {fault}");
        assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), before);
    }
}

#[test]
fn signing_material_reader_keeps_current_authority_whatever_the_file_holds() {
    use std::os::unix::fs::PermissionsExt as _;
    for fault in [
        "changed",
        "manifest",
        "public",
        "link",
        "parent-link",
        "failed",
        "revoked",
        "audience",
        "config",
    ] {
        // A fault of the file at the old path, or of the older build's manifest that tracked it,
        // changes nothing; a fault of the vault's authority refuses.
        let file = matches!(
            fault,
            "changed" | "manifest" | "public" | "link" | "parent-link"
        );
        let f = signing_fixture(false);
        let name = "~/.aterm/apple/devid-existing.key";
        let expected = config(&f.paths).unwrap().unwrap();
        let key = old_path(&f.paths.home, name);
        match fault {
            "changed" => fsx::write_atomic(&key, b"foreign-key", 0o600).unwrap(),
            "manifest" => set_state(&f.paths, |s| {
                s.legacy_manifest
                    .insert(name.into(), crate::digest_hex(b"different"));
            }),
            "failed" => set_state(&f.paths, |s| s.failed = true),
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
        let read = read_verified_entry(&f.paths, &expected, name)
            .map(|read| read.map(|entry| entry.into_parts().0));
        assert_eq!(
            matches!(&read, Ok(Some(value)) if value.as_slice() == b"inert-existing-key"),
            file,
            "{fault}"
        );
        if !file {
            assert!(!matches!(read, Ok(Some(_))), "{fault}");
        }
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
fn an_anchored_signing_key_reads_only_with_its_anchor_chain_in_the_same_view() {
    use std::os::unix::fs::PermissionsExt as _;
    let key_name = "~/.aterm/apple/devid-existing.key";
    let cert_name = "~/.aterm/apple/devid-existing.cer";
    for fault in ["none", "anchor-file", "pair", "unreceived"] {
        let f = signing_fixture(true);
        let expected = config(&f.paths).unwrap().unwrap();
        let cert = old_path(&f.paths.home, cert_name);
        let mut hidden = None;
        match fault {
            "none" => {}
            // A foreign, loosened file at the anchor's old path: no part of the chain.
            "anchor-file" => {
                fsx::write_atomic(&cert, b"foreign-anchor", 0o600).unwrap();
                std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            // The certificate anchored back to the key: a complete pair, as a Developer ID pair
            // is published, which resolves.
            "pair" => f.publish(|doc| {
                doc.set_entry_with(
                    &Audience::Everyone,
                    cert_name,
                    b"inert-certificate",
                    true,
                    Some(key_name),
                )
                .unwrap();
            }),
            // The certificate absent from this reader's view: the chain does not resolve.
            "unreceived" => hidden = Some(hide_anchor(cert_name)),
            _ => unreachable!(),
        }
        let resolves = fault != "unreceived";
        let before = std::fs::read(f.paths.state_file()).unwrap();
        let signing = read_verified_entry(&f.paths, &expected, key_name)
            .map(|read| read.map(|entry| entry.into_parts().0))
            .unwrap();
        let ordinary = read_verified_entry(&f.paths, &expected, key_name).unwrap();
        assert_eq!(
            signing.is_some_and(|value| value.as_slice() == b"inert-existing-key"),
            resolves,
            "{fault}"
        );
        assert_eq!(
            ordinary.is_some_and(|entry| entry.value().as_slice() == b"inert-existing-key"),
            resolves,
            "{fault}"
        );
        // The pair reads together from one version, or not at all.
        let set = read_verified_set(&f.paths, &expected, &[cert_name, key_name]).unwrap();
        assert_eq!(
            set.map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.value().as_slice().to_vec())
                    .collect::<Vec<_>>()
            }),
            resolves.then(|| vec![
                b"inert-certificate".to_vec(),
                b"inert-existing-key".to_vec()
            ]),
            "{fault}"
        );
        // NEGATIVE CONTROL: one absent member makes the whole set absent; a reserved one refuses.
        assert!(
            read_verified_set(&f.paths, &expected, &[key_name, "absent-member"])
                .unwrap()
                .is_none()
        );
        assert!(
            read_verified_set(&f.paths, &expected, &[key_name, crate::commons::ENTRY]).is_err()
        );
        assert_eq!(before, std::fs::read(f.paths.state_file()).unwrap());
        drop(hidden);
    }
}

#[test]
fn a_complete_anchor_ring_reads_member_by_member_and_as_one_set() {
    // Shaped like the release ring (DESIGN-secrets §4): each member put-if-absent and anchored
    // to the next, the last back to the first.
    let ring = ["ring-key", "ring-machine", "ring-credentials"];
    let values: [&[u8]; 3] = [
        b"inert-ring-key",
        b"inert-ring-machine",
        b"inert-ring-credentials",
    ];
    let f = Fixture::new();
    f.publish(|doc| {
        for (index, name) in ring.iter().enumerate() {
            doc.set_entry_with(&Audience::Everyone, name, values[index], true, None)
                .unwrap();
        }
        for (index, name) in ring.iter().enumerate() {
            doc.set_entry_with(
                &Audience::Everyone,
                name,
                values[index],
                true,
                Some(ring[(index + 1) % ring.len()]),
            )
            .unwrap();
        }
    });
    let expected = config(&f.paths).unwrap().unwrap();
    for (index, name) in ring.iter().enumerate() {
        let entry = read_verified_entry(&f.paths, &expected, name)
            .unwrap()
            .unwrap();
        assert_eq!(entry.value().as_slice(), values[index], "{name}");
        assert!(entry.snapshot().read(&f.paths).unwrap().is_some(), "{name}");
    }
    let set = read_verified_set(&f.paths, &expected, &ring)
        .unwrap()
        .unwrap();
    assert_eq!(
        set.iter()
            .map(|entry| entry.value().as_slice())
            .collect::<Vec<_>>(),
        values
    );
    // NEGATIVE CONTROL: one member missing from this reader's view (a document holding the rest
    // without it is refused, so the walk's seam drops it): the members anchored through it read
    // nothing, and the set reads nothing.
    let hidden = hide_anchor("ring-credentials");
    for name in ["ring-key", "ring-machine"] {
        assert!(
            read_verified_entry(&f.paths, &expected, name)
                .unwrap()
                .is_none(),
            "{name}"
        );
    }
    assert!(
        read_verified_set(&f.paths, &expected, &ring)
            .unwrap()
            .is_none()
    );
    drop(hidden);
    assert!(
        read_verified_set(&f.paths, &expected, &ring)
            .unwrap()
            .is_some()
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
        mirrors: Vec::new(),
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
fn stale_apply_context_never_supplies_authenticated_bytes_and_unsafe_files_change_nothing() {
    use std::os::unix::fs::PermissionsExt as _;
    for fault in [
        "none",
        "public-file",
        "file-link",
        "file-missing",
        "failed",
        "unbound",
        "no-digest",
        "sequence",
        "recipient",
        "source",
    ] {
        // NEGATIVE CONTROL: with no fault, and with any fault of the file at the old path, the
        // read returns the vault's bytes; any fault of the applied context refuses.
        let in_place = matches!(fault, "none" | "public-file" | "file-link" | "file-missing");
        let f = Fixture::new();
        let expected = config(&f.paths).unwrap().unwrap();
        // The file an older build wrote at the token's old path, which the file faults change.
        let token = plant(&f.paths.home, "token", b"inert-authentic-token");
        match fault {
            "none" => {}
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
            "file-missing" => std::fs::remove_file(&token).unwrap(),
            "source" => {
                let mut changed = expected.clone();
                changed.source = Source::Path(f.paths.home.join("different.vault"));
                save_config(&f.paths, &changed).unwrap();
            }
            _ => unreachable!(),
        }
        let before = f.bytes();
        let read = read_verified_entry(&f.paths, &expected, "token");
        assert_eq!(
            matches!(&read, Ok(Some(entry)) if entry.value().as_slice() == b"inert-authentic-token"),
            in_place,
            "{fault}"
        );
        if !in_place {
            assert!(!matches!(read, Ok(Some(_))), "{fault}");
        }
        assert_eq!(before, f.bytes(), "{fault}");
    }
}

/// The fixture token of the class scenarios: inert, in the setup-token grammar.
const CLASS_TOKEN: &[u8] = concat!("sk-ant-", "oat01-inert-conformance_fixture").as_bytes();

/// The class scenarios of [`entry_transition`] (DESIGN-claude-setup-token §4.2): on a seated
/// member (m7) of a log vault, `claude_token::write` stores a token, and the typed read
/// (`ClaudeToken`, [`Reading::ClaudeToken`]) or an ordinary read
/// (`ClaudeTokenOrdinary`, [`read_verified_entry`]) of its entry is made.
fn claude_token_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let macs = crate::client::log_entries_tests::two_macs(&[]);
    let m7 = &macs.m7;
    let key = crate::claude_token::Key::parse("ac-0123456789abcdef").unwrap();
    let meta = crate::claude_token::Meta {
        email: "inert@example.invalid".into(),
        account_id: "c-0123456789ab".into(),
        minted_at: 1,
        expires_at: u64::MAX / 2,
        expiry_from: crate::claude_token::ExpiryFrom::Assumed,
        minted_on: "m7".into(),
        cli: "2.1.318".into(),
        minted_by: None,
        verified: None,
    };
    let soon = Instant::now() + Duration::from_secs(60);
    assert_eq!(
        crate::claude_token::write(
            m7,
            &key,
            &SecretBytes::from(CLASS_TOKEN),
            &meta,
            OnBusy::Refuse(soon)
        )
        .unwrap(),
        MemberWritten::Posted
    );
    let expected = config(m7).unwrap().unwrap();
    let name = key.entry();
    let tracked = [
        m7.state_file(),
        m7.cache_file(),
        m7.home.join(".secrets").join(&name),
    ];
    let bytes = || {
        tracked
            .iter()
            .map(|p| std::fs::read(p).ok())
            .collect::<Vec<_>>()
    };
    let before = bytes();
    let (scenario, accepted) = match action {
        "ClaudeToken" => (
            19,
            read_entry(m7, &expected, &name, Reading::ClaudeToken)
                .ok()
                .flatten()
                .is_some_and(|(entry, _)| entry.value().as_slice() == CLASS_TOKEN),
        ),
        "ClaudeTokenOrdinary" => (
            20,
            read_verified_entry(m7, &expected, &name)
                .ok()
                .flatten()
                .is_some(),
        ),
        _ => panic!("unknown class scenario"),
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next.insert("effects", i64::from(before != bytes()));
    macs.server.stop();
    next
}

pub(crate) fn entry_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    if action.starts_with("ClaudeToken") {
        return claude_token_transition(state, action);
    }
    let f = Fixture::new();
    let mut pin = f.read_token().unwrap().snapshot;
    let mut expected = config(&f.paths).unwrap().unwrap();
    let mut guard = None;
    let mut rebound = false;
    let mut name = "token";
    let mut exact = true;
    let scenario = match action {
        "Valid" => 1,
        "FileIgnored" => {
            // The file an older build wrote, edited, then behind a link, then gone: the read
            // returns the vault's bytes each time and opens none of them.
            plant(&f.paths.home, "token", b"tampered");
            let edited = read_verified_entry(&f.paths, &expected, name)
                .unwrap()
                .is_some_and(|entry| entry.value().as_slice() == b"inert-authentic-token");
            let kept = f.paths.home.join("kept-secrets");
            std::fs::rename(f.paths.home.join(".secrets"), &kept).unwrap();
            std::os::unix::fs::symlink(&kept, f.paths.home.join(".secrets")).unwrap();
            let linked = read_verified_entry(&f.paths, &expected, name)
                .unwrap()
                .is_some_and(|entry| entry.value().as_slice() == b"inert-authentic-token");
            std::fs::remove_file(f.paths.home.join(".secrets")).unwrap();
            exact = edited && linked;
            2
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
            // An apply that began and recorded nothing.
            set_state(&f.paths, |s| s.failed = true);
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
    let accepted = exact
        && result.ok().flatten().is_some_and(|entry| {
            name != "token" || rebound || entry.value().as_slice() == b"inert-authentic-token"
        });
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next.insert("effects", i64::from(before != f.bytes()));
    drop(guard);
    next
}

#[test]
fn verified_entry_reads_conform_in_place_and_rolled_back_pins_are_caught() {
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
    // NEGATIVE CONTROL: each of these readings accepting the bytes is no step of the model, and
    // breaks its law; an ordinary read of a Claude Code token entry among them.
    for action in ["Rollback", "Fork", "Revoked", "ClaudeTokenOrdinary"] {
        let mut bad = entry_transition(&model.init_state(), action);
        bad.insert("accepted", 1);
        assert!(!model.check_invariant("OnlyBoundBytes", &bad));
        assert!(!model.successors(action, &model.init_state()).contains(&bad));
    }
    // NEGATIVE CONTROL: a read the file at the old path turned away (a file-backed reader) breaks
    // availability.
    let mut bad = entry_transition(&model.init_state(), "FileIgnored");
    bad.insert("accepted", 0);
    assert!(!model.check_invariant("Available", &bad));
    assert!(
        !model
            .successors("FileIgnored", &model.init_state())
            .contains(&bad)
    );
}
