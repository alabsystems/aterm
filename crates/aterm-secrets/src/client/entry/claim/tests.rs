// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::Fixture;
use super::*;
use aterm_spec::interp::State as ModelState;

fn fixture() -> Fixture {
    let f = Fixture::new();
    seed_vault_genesis(&f.paths, &f._root.path().join("claim.genesis")).unwrap();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "other", b"seeded")
            .unwrap();
    });
    f
}

fn snapshot(f: &Fixture) -> EntrySnapshot {
    f.read_token().unwrap().snapshot
}

fn update(f: &Fixture, value: &[u8]) {
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "token", value).unwrap();
    });
}

fn envelope_fallback(f: &Fixture) {
    let admin = admin_open(&f.paths).unwrap();
    let mut state = load_state(&f.paths).unwrap();
    state.seq += 1;
    let bytes = seal(
        &admin.config.name,
        state.seq,
        &admin.seed,
        &admin.commons_seed,
        admin.commons_bound,
        &admin.doc,
        &admin.handoff,
    )
    .unwrap();
    state.digest = Some(envelope::parse(&bytes).unwrap().digest());
    fsx::write_atomic(&f.paths.cache_file(), &bytes, 0o600).unwrap();
    save_state(&f.paths, &state).unwrap();
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    if action == "Case" {
        next.insert("scenario", state["scenario"] + 1);
        return next;
    }
    assert_eq!(action, "Read");
    let f = fixture();
    let initial = snapshot(&f);
    let mut claim = EntryClaim::decode(&initial.entry_claim().unwrap().unwrap().encode()).unwrap();
    let mut current;
    let mut guarded = None;
    match state["scenario"] {
        0 => {
            update(&f, b"changed");
            current = snapshot(&f);
        }
        1 => {
            current = initial;
        }
        2 => {
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "other", b"unrelated")
                    .unwrap();
            });
            current = snapshot(&f);
        }
        3 => {
            update(&f, b"middle");
            claim.operation = snapshot(&f).semantic_operation().unwrap();
            update(&f, b"final");
            current = snapshot(&f);
        }
        4 => {
            claim.wrapper = [42; 32];
            update(&f, b"changed");
            current = snapshot(&f);
        }
        5 => {
            let old = claim.version.log().unwrap();
            claim.version = VersionClaim::new(
                claim.version.vault().into(),
                *claim.version.signer(),
                claim.version.envelope().cloned(),
                Some(LogVersion {
                    network: old.network,
                    heads: vec![[42; 32]],
                }),
            )
            .unwrap();
            update(&f, b"changed");
            current = snapshot(&f);
        }
        6 => {
            claim.recipient = Identity::from_secret_bytes([43; 32]).recipient();
            update(&f, b"changed");
            current = snapshot(&f);
        }
        7 => {
            claim.name = "other".into();
            update(&f, b"changed");
            current = snapshot(&f);
        }
        8 => {
            claim.audience = Audience::Hosts(vec!["admin".into()]);
            update(&f, b"changed");
            current = snapshot(&f);
        }
        9 => {
            update(&f, b"changed");
            claim = snapshot(&f).entry_claim().unwrap().unwrap();
            current = initial;
        }
        10 => {
            let mut log = claim.version.log().unwrap().clone();
            log.network = [43; 32];
            claim.version = VersionClaim::new(
                claim.version.vault().into(),
                *claim.version.signer(),
                claim.version.envelope().cloned(),
                Some(log),
            )
            .unwrap();
            update(&f, b"changed");
            current = snapshot(&f);
        }
        11 => {
            admin_revoke(&f.paths, &admin_open(&f.paths).unwrap(), "peer", false).unwrap();
            current = snapshot(&f);
            assert_eq!(current.semantic_operation(), initial.semantic_operation());
            assert_ne!(current.wrapper_operation(), initial.wrapper_operation());
        }
        12 => {
            envelope_fallback(&f);
            current = snapshot(&f);
            assert_eq!(current.origin(), super::super::EntryOrigin::Envelope);
            assert!(current.log_ids.is_some());
            assert!(current.entry_claim().unwrap().is_none());
        }
        13 | 14 => {
            update(&f, b"changed");
            current = snapshot(&f);
            // The resolver's real conflict and unreadability scenarios are covered
            // by DbEntryProvenance. Here exercise the comparison's conservative flags.
            let Source::Log {
                register: Some(register),
                ..
            } = &mut current.source
            else {
                unreachable!()
            };
            if state["scenario"] == 13 {
                register.ambiguous = true;
            } else {
                register.complete = false;
            }
            assert!(current.entry_claim().unwrap().is_none());
        }
        15 => {
            let mut state = load_state(&f.paths).unwrap();
            state.digest = None;
            state.source_identity = Some(source_identity(&config(&f.paths).unwrap().unwrap()));
            save_state(&f.paths, &state).unwrap();
            std::fs::remove_file(f.paths.cache_file()).unwrap();
            current = snapshot(&f);
            claim = current.entry_claim().unwrap().unwrap();
            assert!(claim.version.envelope().is_none());
        }
        16 => {
            update(&f, b"middle");
            update(&f, b"inert-authentic-token");
            current = snapshot(&f);
            // Same bytes permit admission; only the fresh callback entry can
            // prove this semantic round trip advanced beyond the old claim.
            assert_eq!(
                initial.compare_claim(&f.paths, &claim).unwrap(),
                EntryClaimRelation::Same
            );
            let admitted = initial
                .admit(&f.paths, |entry| {
                    entry.snapshot().compare_claim(&f.paths, &claim)
                })
                .unwrap();
            let super::super::EntryAdmission::Accepted(relation) = admitted else {
                panic!("fresh admission refused")
            };
            guarded = Some(relation.unwrap());
        }
        17 => {
            guarded = Some(historical_roster_denial_relation());
            current = initial;
        }
        18 => {
            update(&f, b"changed");
            current = snapshot(&f);
            PREFIX_ERROR.set(true);
        }
        _ => panic!("unknown claim case"),
    }
    let relation = match guarded {
        Some(relation) => Ok(relation),
        None => current.compare_claim(&f.paths, &claim),
    };
    PREFIX_ERROR.set(false);
    next.insert("done", 1);
    next.insert(
        "relation",
        match relation {
            Ok(EntryClaimRelation::Unproven) => 0,
            Ok(EntryClaimRelation::Same) => 1,
            Ok(EntryClaimRelation::Advanced) => 2,
            Err(_) => 3,
        },
    );
    next
}

const CLAIM_HOME: &str = "ATERM_TEST_ENTRY_CLAIM_HOME";

#[test]
fn entry_claim_reader_child() {
    let Some(home) = std::env::var_os(CLAIM_HOME) else {
        return;
    };
    let paths = Paths::for_home(Path::new(&home));
    let claim = EntryClaim::decode(&std::fs::read(paths.home.join("retired-entry.claim")).unwrap())
        .unwrap();
    let entry = read_verified_entry(&paths, &config(&paths).unwrap().unwrap(), "token")
        .unwrap()
        .unwrap();
    assert_eq!(
        entry.snapshot().compare_claim(&paths, &claim).unwrap(),
        EntryClaimRelation::Advanced
    );
    println!("retained-entry-claim=advanced");
}

#[test]
fn durable_entry_claim_survives_a_new_reader_process() {
    let f = fixture();
    let claim = snapshot(&f).entry_claim().unwrap().unwrap().encode();
    fsx::write_atomic(&f.paths.home.join("retired-entry.claim"), &claim, 0o600).unwrap();
    update(&f, b"middle");
    update(&f, b"inert-authentic-token");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "client::entry::claim::tests::entry_claim_reader_child",
            "--nocapture",
        ])
        .env(CLAIM_HOME, &f.paths.home);
    let output = aterm_uds::deadline::output_until_complete_stdout(
        &mut child,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("retained-entry-claim=advanced")
    );
}

#[test]
fn entry_claim_authority_checks_are_independent_of_recipient_and_selection() {
    let f = fixture();
    let before = snapshot(&f);
    let claim = before.entry_claim().unwrap().unwrap();
    update(&f, b"changed");
    let current = snapshot(&f);
    for (vault, signer) in [
        ("another-vault".to_owned(), *claim.version.signer()),
        (claim.version.vault().to_owned(), [43; 32]),
    ] {
        let mut wrong = claim.clone();
        wrong.version = VersionClaim::new(
            vault,
            signer,
            claim.version.envelope().cloned(),
            claim.version.log().cloned(),
        )
        .unwrap();
        let decoded = EntryClaim::decode(&wrong.encode()).unwrap();
        assert_eq!(
            current.compare_claim(&f.paths, &decoded).unwrap(),
            EntryClaimRelation::Unproven
        );
    }
    let mut envelope = claim.version.envelope().unwrap().clone();
    envelope.digest[0] ^= 1;
    let mut fork = claim.clone();
    fork.version = VersionClaim::new(
        claim.version.vault().into(),
        *claim.version.signer(),
        Some(envelope),
        claim.version.log().cloned(),
    )
    .unwrap();
    assert_eq!(
        before.compare_claim(&f.paths, &fork).unwrap(),
        EntryClaimRelation::Unproven
    );
    let other = fixture();
    assert_eq!(
        current.compare_claim(&other.paths, &claim).unwrap(),
        EntryClaimRelation::Unproven
    );
    let mut wrong_host = claim.clone();
    wrong_host.host = "another-host".into();
    assert_eq!(
        current.compare_claim(&f.paths, &wrong_host).unwrap(),
        EntryClaimRelation::Unproven
    );
    let mut redundant = claim.clone();
    let mut log = claim.version.log().unwrap().clone();
    log.heads.push(log.network);
    log.heads.sort();
    log.heads.dedup();
    assert_ne!(log.heads, claim.version.log().unwrap().heads);
    redundant.version = VersionClaim::new(
        claim.version.vault().into(),
        *claim.version.signer(),
        claim.version.envelope().cloned(),
        Some(log),
    )
    .unwrap();
    assert_eq!(
        current.compare_claim(&f.paths, &redundant).unwrap(),
        EntryClaimRelation::Unproven
    );
}

fn historical_roster_denial_relation() -> EntryClaimRelation {
    let _answering = ServersAnswering::set();
    let lab = crate::client::log_entries_tests::two_macs_with(
        &[(Audience::Everyone, "token", b"initial")],
        true,
    );
    admin_seed_swarm_roster(&lab.m22, &[]).unwrap();
    sync(&lab.m7, None).unwrap();
    let expected = config(&lab.m7).unwrap().unwrap();
    let read = || read_verified_entry(&lab.m7, &expected, "token").unwrap();
    let mut claim = read().unwrap().snapshot().entry_claim().unwrap().unwrap();
    let identity = read_identity(&lab.m7).unwrap().unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let mut roster = crate::swarm_roster::SwarmRoster::from_document(&admin.doc)
        .unwrap()
        .unwrap();
    let label = roster
        .members
        .iter()
        .find(|member| member.recipient == identity.recipient().to_bech32())
        .unwrap()
        .label
        .clone();
    roster.deny_labels.push(label);
    let publish = |roster: &crate::swarm_roster::SwarmRoster, value: &[u8]| {
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            roster.to_toml().as_bytes(),
        )
        .unwrap();
        doc.set_entry(&Audience::Everyone, "token", value).unwrap();
        admin_publish(&lab.m22, &admin, &doc).unwrap();
        sync(&lab.m7, None).unwrap();
    };
    publish(&roster, b"initial");
    assert!(
        read().is_none(),
        "ordinary reader must reject the denied prefix"
    );
    let log = load_records(&lab.m7).unwrap().log().unwrap().clone();
    let opened = log
        .open_selection(&identity, Some(&["token", crate::SWARM_ROSTER_ENTRY]))
        .unwrap()
        .unwrap();
    let key = (Audience::Everyone, "token".into());
    claim.operation = opened.operations[&key];
    claim.wrapper = opened.puts[&key];
    let state = load_state(&lab.m7).unwrap();
    claim.version = VersionClaim::new(
        claim.version.vault().into(),
        *claim.version.signer(),
        state
            .digest
            .map(|digest| crate::client::version::EnvelopeVersion {
                seq: state.seq,
                digest,
            }),
        Some(LogVersion {
            network: *log.network(),
            heads: log.heads(),
        }),
    )
    .unwrap();
    roster.deny_labels.clear();
    publish(&roster, b"changed-after-denial");
    let current = read().unwrap().snapshot;
    let Source::Log {
        history,
        register: Some(register),
    } = &current.source
    else {
        panic!("expected a log selection")
    };
    assert!(
        claim
            .version
            .log()
            .unwrap()
            .heads
            .iter()
            .all(|head| history.get(head).is_some())
    );
    assert!(!register.ambiguous && register.complete);
    assert_ne!(register.operation, claim.operation);
    assert!(
        history
            .past_of(&[register.operation])
            .holds(&claim.operation)
    );
    lab.server.stop();
    current.compare_claim(&lab.m7, &claim).unwrap()
}

#[test]
fn durable_entry_claims_verify_the_old_selection() {
    let model = aterm_models_db::entry_claim_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?} -> {actual:?}"
                );
            }
        }
    }
    for (mutant, scenario, invariant) in [
        (1, 3, "OldSelectionRequired"),
        (2, 11, "StableIsNotAdvance"),
        (3, 0, "SemanticProgressRecognized"),
        (4, 17, "OldSelectionRequired"),
        (5, 18, "OldSelectionRequired"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", scenario);
        MUTANT.set(mutant);
        let bad = transition(&state, "Read");
        MUTANT.set(0);
        assert!(!model.check_invariant(invariant, &bad));
    }
}

#[test]
fn entry_claim_codec_is_bounded_canonical_and_value_free() {
    let f = fixture();
    let observed = snapshot(&f);
    let mut claim = observed.entry_claim().unwrap().unwrap();
    claim.audience = Audience::Hosts(vec!["admin".into(), "peer".into()]);
    let bytes = claim.encode();
    assert_eq!(EntryClaim::decode(&bytes).unwrap(), claim);
    assert!(
        !bytes
            .windows(observed.fingerprint.len())
            .any(|part| part == observed.fingerprint.as_slice())
    );
    assert!(
        !bytes
            .windows(b"inert-authentic-token".len())
            .any(|part| part == b"inert-authentic-token")
    );
    for n in 0..bytes.len() {
        assert!(EntryClaim::decode(&bytes[..n]).is_err(), "prefix {n}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(EntryClaim::decode(&trailing).is_err());
    assert!(EntryClaim::decode(&vec![0; MAX_ENTRY_CLAIM_BYTES + 1]).is_err());
    for audience in [
        Audience::Hosts(vec![]),
        Audience::Hosts(vec!["peer".into(), "admin".into()]),
        Audience::Hosts(vec!["admin".into(), "admin".into()]),
    ] {
        let mut bad = claim.clone();
        bad.audience = audience;
        assert!(EntryClaim::decode(&bad.encode()).is_err());
    }
    let mut bad = claim.clone();
    bad.recipient = Recipient([0; 32]);
    assert!(EntryClaim::decode(&bad.encode()).is_err());
    let mut bad = claim.clone();
    bad.name = "../outside".into();
    assert!(EntryClaim::decode(&bad.encode()).is_err());
    let mut bad = claim.clone();
    bad.host.clear();
    assert!(EntryClaim::decode(&bad.encode()).is_err());
    let mut bad = claim.clone();
    bad.operation = [0; 32];
    assert!(EntryClaim::decode(&bad.encode()).is_err());
    let mut bad = claim;
    bad.version = VersionClaim::new(
        bad.version.vault().into(),
        *bad.version.signer(),
        bad.version.envelope().cloned(),
        None,
    )
    .unwrap();
    assert!(EntryClaim::decode(&bad.encode()).is_err());
}
