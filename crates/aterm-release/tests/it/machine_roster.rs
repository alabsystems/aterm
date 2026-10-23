// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Producer-side tests for the paper-master machine roster and signing gate.

use crate::{channel, machines, manifest_out, publish, sign};

use std::path::{Path, PathBuf};

use aterm_update_core::Manifest;
use aterm_update_core::roster::{Attribution, Machine, Roster};
/// Standard padded Base64 (`aterm_codec::base64`).
fn b64(raw: &[u8]) -> String {
    aterm_codec::base64::encode(raw).expect("test key material is far below MAX_INPUT_LEN")
}
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use publish::{RosterEvidence, SignaturePolicy};

/// Seeds. Repeated bytes so no reader could mistake one for a real key.
const MASTER: [u8; 32] = [0x71; 32];
const OTHER_MASTER: [u8; 32] = [0x72; 32];
const M3: [u8; 32] = [0x73; 32];
const M11: [u8; 32] = [0x74; 32];
const STRANGER: [u8; 32] = [0x75; 32];

/// 2026-08-11T00:00:00Z — inside the fixture roster's window.
const NOW: i64 = 1_786_406_400;
/// Far past every `valid_until` this file writes.
const LONG_AFTER: i64 = 1_900_000_000;
/// 2027-02-01T00:00:00Z, the `valid_until` of every fixture roster here.
const FIXTURE_VALID_UNTIL: i64 = 1_801_440_000;

fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).expect("synthetic seed")
}

fn pk(seed: &[u8; 32]) -> String {
    b64(kp(seed).public_key().as_ref())
}

/// A master-signed roster naming m3 and m11, with the deny-list under the caller's
/// control. Returns the document the producer gate consumes.
fn roster_doc(revoked: &[&str], signer: &[u8; 32]) -> machines::RosterDocument {
    roster_naming(&[("m3", &pk(&M3)), ("m11", &pk(&M11))], revoked, signer)
}

/// A roster listing exactly these (id, pubkey) pairs, signed by `signer`, so a test can bind
/// a roster to a freshly generated key.
fn roster_naming(
    machines_in: &[(&str, &str)],
    revoked: &[&str],
    signer: &[u8; 32],
) -> machines::RosterDocument {
    roster_naming_at(machines_in, revoked, signer, 4)
}

/// The same, with the roster generation (what the channel ratchet is about) as a parameter.
fn roster_naming_at(
    machines_in: &[(&str, &str)],
    revoked: &[&str],
    signer: &[u8; 32],
    roster_seq: u64,
) -> machines::RosterDocument {
    let r = Roster {
        schema: 1,
        roster_seq,
        valid_until: "2027-02-01T00:00:00Z".into(),
        machines: machines_in
            .iter()
            .map(|(id, pubkey)| Machine {
                id: (*id).to_string(),
                pubkey: (*pubkey).to_string(),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            })
            .collect(),
        revoked: revoked.iter().map(|s| (*s).to_string()).collect(),
    };
    let bytes = r.to_toml().expect("fixture roster serializes").into_bytes();
    let signature = kp(signer).sign(&bytes).as_ref().to_vec();
    machines::RosterDocument { bytes, signature }
}

/// The armed evidence a healthy m3 cut presents.
fn armed<'a>(master: &'a [&'a str], document: &'a machines::RosterDocument) -> RosterEvidence<'a> {
    RosterEvidence {
        master_pubkeys: master,
        roster: Some(document),
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
    }
}

/// The anchors an armed cut resolves against, as the parameters `publish::signing_verdict`
/// takes, so a test can use a synthetic master.
fn anchors<'a>(masters: &'a [&'a str], identity: Option<&'a Path>) -> publish::SigningAnchors<'a> {
    publish::SigningAnchors {
        master_pubkeys: masters,
        identity_path: identity,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
    }
}

/// The inert evidence a fork carries: an empty master authorizes nobody.
fn inert<'a>() -> RosterEvidence<'a> {
    RosterEvidence {
        master_pubkeys: &[],
        roster: None,
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
    }
}

/// The shipped anchor is armed, and never with an empty member, which would read as armed
/// and authorize nobody.
#[test]
fn the_shipped_paper_master_is_armed_and_has_no_empty_member() {
    assert!(!aterm_update_core::pins::PAPER_MASTER_PUBKEYS.is_empty());
    assert!(aterm_update_core::pins::roster_tier_armed());
    assert!(
        !aterm_update_core::pins::PAPER_MASTER_PUBKEYS.contains(&""),
        "an empty master member is never legal"
    );
}

/// A cut may not publish a roster generation the channel has moved past.
#[test]
fn a_cut_may_not_publish_an_older_roster_generation_than_the_channel_head() {
    let master = pk(&MASTER);
    let m3 = pk(&M3);
    for (carried, head) in [(4u64, 5u64), (5, 5), (6, 5), (0, 1)] {
        let document = roster_naming_at(&[("m3", &m3)], &[], &MASTER, carried);
        let verified = aterm_update_core::roster::verify_roster(
            &[master.as_str()],
            document.bytes.clone(),
            &document.signature,
        )
        .expect("the fixture master signed it");
        let client = Roster::parse(&verified)
            .expect("the client parses it")
            .admit(head, NOW);
        let producer = publish::roster_floor_covered(Some(carried), Some(head));
        assert_eq!(
            producer.is_ok(),
            client.is_ok(),
            "producer and client disagreed on generation {carried} against a channel \
             head at {head}: producer {producer:?}, client {client:?}"
        );
    }

    publish::roster_floor_covered(None, None).expect("an unrostered channel head");
    publish::roster_floor_covered(Some(4), None).expect("the first rostered release");
    publish::roster_floor_covered(None, Some(5)).expect_err("a downgrade is not a floor pass");
}

/// An entry that will still sign proves the whole roster chain; one finishing already-signed
/// bytes proves the key and stops. The roster such an entry would read is not the one it
/// publishes (that is frozen in `dist/` under a signature), so a verdict on it could only fail
/// spuriously and strand a cut one upload from done.
#[test]
fn a_finish_only_entry_proves_the_key_and_not_the_roster() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let m3 = pk(&M3);
    let fresh = roster_doc(&[], &MASTER);
    let revoking = roster_doc(&["m3"], &MASTER);

    let arrangements: Vec<(&str, Option<&machines::RosterDocument>, i64)> = vec![
        ("a healthy roster", Some(&fresh), NOW),
        ("a LAPSED roster", Some(&fresh), LONG_AFTER),
        ("a roster that REVOKED this machine", Some(&revoking), NOW),
        ("no roster named at all", None, NOW),
    ];
    for (label, document, now_unix) in arrangements {
        let evidence = RosterEvidence {
            master_pubkeys: &masters,
            roster: document,
            declared_machine_id: Some("someone-else"),
            now_unix,
            duty: publish::RosterDuty::Finish,
        };
        let (policy, attribution) = publish::channel_signature_policy(Some(&m3), &evidence)
            .unwrap_or_else(|e| panic!("a finish entry must not be blocked by {label}: {e}"));
        assert!(policy.required, "{label}");
        assert_eq!(policy.pubkey.as_deref(), Some(m3.as_str()), "{label}");
        assert_eq!(
            attribution, None,
            "{label}: a finish entry claims no attribution — the one in the signed bytes \
             is the only true answer, and comparing a fresh local claim against it is the \
             bug this closes"
        );
        let mut signing = RosterEvidence {
            master_pubkeys: &masters,
            roster: document,
            declared_machine_id: None,
            now_unix,
            duty: publish::RosterDuty::Sign,
        };
        if label != "a healthy roster" {
            assert!(
                publish::channel_signature_policy(Some(&m3), &signing).is_err(),
                "{label} must refuse a SIGN entry, or the FINISH case proves nothing"
            );
        } else {
            signing.declared_machine_id = Some("m3");
            assert!(publish::channel_signature_policy(Some(&m3), &signing).is_ok());
        }
    }

    let stranger = pk(&STRANGER);
    let finishing = RosterEvidence {
        master_pubkeys: &masters,
        roster: Some(&fresh),
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Finish,
    };
    let (policy, attribution) = publish::channel_signature_policy(Some(&stranger), &finishing)
        .expect("a finish entry does not re-litigate the roster");
    assert!(policy.required);
    assert_eq!(policy.pubkey.as_deref(), Some(stranger.as_str()));
    assert_eq!(attribution, None);
    let starting = RosterEvidence {
        master_pubkeys: &masters,
        roster: Some(&fresh),
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
    };
    assert!(
        publish::channel_signature_policy(Some(&stranger), &starting).is_err(),
        "the roster must bite where the key is chosen"
    );
    assert!(publish::channel_signature_policy(None, &finishing).is_err());
}

/// With no attribution to stamp, the staged manifest is byte-identical to what `manifest_out`
/// alone produces and carries neither key.
#[test]
fn an_unattributed_cut_stages_byte_identical_manifest_bytes() {
    let dir = tempdir("unattributed");
    let inputs = inputs("0.99.0", 990);
    let staged = publish::stage_manifest(&dir, &inputs, None, None).expect("stages");
    let bytes = std::fs::read(&staged).expect("staged bytes");
    let expected = manifest_out::emit(&manifest_out::build(&inputs)).expect("emits");
    assert_eq!(
        String::from_utf8(bytes).expect("utf8"),
        expected,
        "the unarmed cut must emit exactly the bytes this cutter has always emitted"
    );
    assert!(!expected.contains("machine_id"), "{expected}");
    assert!(!expected.contains("roster_seq"), "{expected}");
    publish::stage_roster_assets(&dir, None).expect("no document, no assets");
    assert!(!dir.join("aterm-machines.toml").exists());
    assert!(!dir.join("aterm-machines.toml.sig").exists());
    clean(&dir);
}

/// The Intel DMG pair is retired: every gate that reads a manifest refuses one naming
/// `dmg_x86_64` by name ([`publish::refuse_retired_intel_dmg`]), and a smuggled
/// `-x86_64.dmg` on a release is a foreign object.
#[test]
fn a_manifest_naming_the_retired_intel_dmg_is_refused_and_a_smuggled_one_too() {
    let plain = manifest_out::build(&inputs("0.5.0", 500));
    assert_eq!(plain.dmg_x86_64, None, "this cutter never emits the pair");
    assert_eq!(plain.dmg_x86_64_sha256, None);
    let mut named = manifest_out::build(&inputs("0.5.0", 500));
    named.dmg_x86_64 = Some("aterm-0.5.0-x86_64.dmg".to_string());
    named.dmg_x86_64_sha256 = Some("ab".repeat(32));
    publish::refuse_retired_intel_dmg(&named)
        .expect_err("a manifest naming the retired pair is refused by name");
    let mut half = manifest_out::build(&inputs("0.5.0", 500));
    half.dmg_x86_64_sha256 = Some("ab".repeat(32));
    assert!(publish::refuse_retired_intel_dmg(&half).is_err());
}

/// A listed machine cuts, and the verdict carries the policy and who it is.
#[test]
fn an_armed_anchor_authorizes_a_listed_machine_and_names_it() {
    let master = pk(&MASTER);
    let document = roster_doc(&[], &MASTER);
    let (policy, who) =
        publish::channel_signature_policy(Some(&pk(&M3)), &armed(&[&master], &document))
            .expect("a listed, unrevoked machine may cut");
    assert_eq!(
        policy,
        SignaturePolicy {
            required: true,
            pubkey: Some(pk(&M3)),
        }
    );
    let who = who.expect("an armed cut is always attributed");
    assert_eq!(who.machine_id, "m3");
    assert_eq!(who.roster_seq, 4);
}

/// Kills "drop the `machines::authorize_cut` call from `channel_signature_policy`": an unlisted
/// key, a revoked machine, a lapsed roster, a roster under the wrong master, a corrupted
/// signature and a missing roster would all cut. Each case asserts its own precondition.
#[test]
fn an_armed_anchor_refuses_every_machine_the_roster_does_not_authorize() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];

    let document = roster_doc(&[], &MASTER);
    assert!(
        !document_lists(&document, &pk(&STRANGER)),
        "precondition: the stranger really is absent from the roster"
    );
    let unlisted = armed(&masters, &document);
    publish::channel_signature_policy(Some(&pk(&STRANGER)), &unlisted)
        .expect_err("an unlisted key may not cut");

    let revoked = roster_doc(&["m11"], &MASTER);
    assert!(
        document_lists(&revoked, &pk(&M11)),
        "precondition: m11 is still LISTED — it is the deny-list that must stop it"
    );
    publish::channel_signature_policy(Some(&pk(&M11)), &armed(&masters, &revoked))
        .expect_err("a revoked machine may not cut");
    publish::channel_signature_policy(Some(&pk(&M3)), &armed(&masters, &revoked))
        .expect("revoking m11 must not revoke m3");

    let mut lapsed = armed(&masters, &document);
    lapsed.now_unix = LONG_AFTER;
    publish::channel_signature_policy(Some(&pk(&M3)), &lapsed)
        .expect_err("a lapsed roster may not authorize a cut");

    let mut about_to_lapse = armed(&masters, &document);
    about_to_lapse.now_unix = FIXTURE_VALID_UNTIL - 60;
    publish::channel_signature_policy(Some(&pk(&M3)), &about_to_lapse)
        .expect_err("a roster with a minute left may not start a ~20 minute cut");

    let foreign = roster_doc(&[], &OTHER_MASTER);
    assert_eq!(
        foreign.bytes, document.bytes,
        "precondition: only the SIGNATURE differs, so this tests the anchor and not the body"
    );
    publish::channel_signature_policy(Some(&pk(&M3)), &armed(&masters, &foreign))
        .expect_err("a roster under another master authorizes nothing");

    let mut torn = roster_doc(&[], &MASTER);
    torn.signature[0] ^= 0xff;
    publish::channel_signature_policy(Some(&pk(&M3)), &armed(&masters, &torn))
        .expect_err("a torn master signature authorizes nothing");

    let mut absent = armed(&masters, &document);
    absent.roster = None;
    publish::channel_signature_policy(Some(&pk(&M3)), &absent)
        .expect_err("an armed anchor with no roster must refuse, never fall through");

    publish::channel_signature_policy(None, &armed(&masters, &document))
        .expect_err("a keyless machine may not cut for a rostered channel");
}

/// An identity mismatch refuses. The roster's key→id map is the authority, but a profile that
/// disagrees means a copied profile or a re-minted machine, and would publish a false
/// attribution.
#[test]
fn a_machine_that_declares_the_wrong_id_may_not_cut() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let document = roster_doc(&[], &MASTER);
    let mut evidence = armed(&masters, &document);
    evidence.declared_machine_id = Some("m11");
    publish::channel_signature_policy(Some(&pk(&M3)), &evidence)
        .expect_err("m3's key declared as m11 must refuse");
    evidence.declared_machine_id = Some("m3");
    publish::channel_signature_policy(Some(&pk(&M3)), &evidence)
        .expect("a truthful declaration is not an obstacle");
}

/// An unpinned master (a fork) is per-machine opt-in and attributes nothing, even with valid
/// roster evidence, at either duty.
#[test]
fn an_unpinned_master_is_per_machine_opt_in_and_attributes_nothing() {
    let document = roster_doc(&[], &MASTER);
    let other = pk(&M11);
    for duty in [publish::RosterDuty::Sign, publish::RosterDuty::Finish] {
        for material in [None, Some(other.as_str())] {
            for evidence in [
                RosterEvidence { duty, ..inert() },
                RosterEvidence {
                    master_pubkeys: &[],
                    roster: Some(&document),
                    declared_machine_id: Some("m3"),
                    now_unix: LONG_AFTER,
                    duty,
                },
            ] {
                let (policy, attribution) =
                    publish::channel_signature_policy(material, &evidence).unwrap();
                assert_eq!(
                    policy,
                    publish::unrostered_signature_policy(material).unwrap(),
                    "{material:?} ({duty:?})"
                );
                assert_eq!(policy.required, material.is_some());
                assert_eq!(attribution, None, "{material:?} ({duty:?})");
            }
        }
    }
}

/// The stamp lands inside what the signature covers, shown by signing the staged file and
/// verifying against it.
#[test]
fn attribution_is_inside_the_bytes_the_signature_covers() {
    let dir = tempdir("attributed");
    let inputs = inputs("0.99.0", 990);
    let who = Attribution {
        machine_id: "m3".into(),
        pubkey_b64: pk(&M3),
        roster_seq: 4,
    };
    let staged = publish::stage_manifest(&dir, &inputs, Some(&who), None).expect("stages");
    let signed_bytes = std::fs::read(&staged).expect("staged bytes");
    let text = String::from_utf8(signed_bytes.clone()).expect("utf8");
    assert!(text.contains("machine_id = \"m3\""), "{text}");
    assert!(text.contains("roster_seq = 4"), "{text}");

    let signature = kp(&M3).sign(&signed_bytes).as_ref().to_vec();
    publish::verify_detached_manifest_signature(&pk(&M3), &signed_bytes, &signature)
        .expect("the signature covers the staged bytes");
    let parsed = Manifest::parse(&text).expect("client parses");
    assert_eq!(parsed.machine_id.as_deref(), Some("m3"));
    assert_eq!(parsed.roster_seq, Some(4));

    let unattributed = publish::stage_manifest(&dir, &inputs, None, None).expect("stages");
    let early_bytes = std::fs::read(&unattributed).expect("unattributed bytes");
    let early_signature = kp(&M3).sign(&early_bytes).as_ref().to_vec();
    let late =
        publish::stage_manifest(&dir, &inputs, Some(&who), None).expect("re-stages, stamped");
    let late_bytes = std::fs::read(&late).expect("late bytes");
    assert_ne!(
        early_bytes, late_bytes,
        "precondition: the stamp changed the bytes"
    );
    publish::verify_detached_manifest_signature(&pk(&M3), &late_bytes, &early_signature)
        .expect_err("a signature made before the stamp cannot cover the stamped bytes");
    clean(&dir);
}

/// An armed cut publishes the roster it was authorized by, byte for byte from the bytes the
/// gate held, and the asset rules demand both halves.
#[test]
fn an_armed_cut_stages_and_requires_both_roster_assets() {
    let dir = tempdir("roster-assets");
    let document = roster_doc(&[], &MASTER);
    publish::stage_roster_assets(&dir, Some(&document)).expect("stages both halves");
    assert_eq!(
        std::fs::read(dir.join("aterm-machines.toml")).expect("roster staged"),
        document.bytes,
        "the PUBLISHED roster must be the AUTHORIZING roster, byte for byte"
    );
    assert_eq!(
        std::fs::read(dir.join("aterm-machines.toml.sig")).expect("signature staged"),
        document.signature
    );

    let names = channel::required_asset_names("0.5.0", true, true);
    assert!(
        names.contains(&"aterm-machines.toml".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"aterm-machines.toml.sig".to_string()),
        "{names:?}"
    );
    channel::validate_channel_asset_set(&names, "0.5.0", true, true, &[]).expect("the exact set");
    let forgotten = channel::required_asset_names("0.5.0", true, false);
    channel::validate_channel_asset_set(&forgotten, "0.5.0", true, true, &[])
        .expect_err("a rostered channel head without its roster is unelectable");
    clean(&dir);
}

/// A resume that re-authorizes as a different machine aborts: the manifest it would finish is
/// signed over the first machine's attribution. The asymmetric cases refuse too: an anchor
/// armed or unarmed mid-cut leaves a journal and a live verdict that disagree.
#[test]
fn a_resume_may_not_change_which_machine_cut_the_release() {
    publish::resume_attribution_agrees(None, None).expect("an unattributed cut resumes");
    publish::resume_attribution_agrees(Some("m3"), Some("m3")).expect("same machine resumes");

    publish::resume_attribution_agrees(Some("m3"), Some("m11"))
        .expect_err("another machine may not finish this cut");

    publish::resume_attribution_agrees(Some("m3"), None)
        .expect_err("a resume with no roster may not finish a rostered cut");
    publish::resume_attribution_agrees(None, Some("m3"))
        .expect_err("a roster armed mid-cut may not attribute bytes that carry no attribution");
}

/// The credentials profile names the roster and the signature is its sibling: one name, so a
/// profile cannot pair a roster with a signature over another.
#[test]
fn the_credentials_profile_names_the_roster_and_the_signature_is_its_sibling() {
    let dir = tempdir("profile");
    let document = roster_doc(&[], &MASTER);
    let roster_path = dir.join("aterm-machines.toml");
    std::fs::write(&roster_path, &document.bytes).unwrap();
    std::fs::write(dir.join("aterm-machines.toml.sig"), &document.signature).unwrap();
    assert_eq!(
        machines::RosterDocument::signature_path(&roster_path),
        dir.join("aterm-machines.toml.sig")
    );

    let profile = write_profile(
        &dir,
        &format!(
            "signing_key = \"{}\"\nmachine_id = \"m3\"\nmachine_roster = \"{}\"\n",
            pkcs8_b64(),
            roster_path.display()
        ),
    );
    let creds = sign::ReleaseCredentials::load(&profile, None).expect("loads");
    assert_eq!(creds.machine_id(), Some("m3"));
    assert_eq!(creds.machine_roster(), Some(roster_path.as_path()));
    let read = machines::RosterDocument::read(creds.machine_roster().unwrap()).expect("reads");
    assert_eq!(read, document, "the document is read whole, both halves");

    machines::RosterDocument::read(&dir.join("nope.toml"))
        .expect_err("a named roster that is not there must refuse");
    std::fs::write(dir.join("lonely.toml"), &document.bytes).unwrap();
    machines::RosterDocument::read(&dir.join("lonely.toml"))
        .expect_err("a roster with no master signature proves nothing");

    let rendered = format!("{creds:?}");
    assert!(rendered.contains("m3"), "{rendered}");
    assert!(rendered.contains("redacted"), "{rendered}");
    assert!(
        !rendered.contains(&pkcs8_b64()),
        "the private key must never render"
    );
    clean(&dir);
}

/// One profile serves every Mac the secrets vault hands it to: a `~/` roster is read under
/// that Mac's home, any other value is the path as written, and a `~/` roster with no home,
/// or a relative one, is refused rather than read from the working directory.
#[test]
fn a_profile_s_tilde_roster_is_read_under_the_home_it_is_loaded_in() {
    let dir = tempdir("tilde-roster");
    let document = roster_doc(&[], &MASTER);
    let roster_dir = dir.join(".aterm/roster");
    std::fs::create_dir_all(&roster_dir).unwrap();
    std::fs::write(roster_dir.join("aterm-machines.toml"), &document.bytes).unwrap();
    std::fs::write(
        roster_dir.join("aterm-machines.toml.sig"),
        &document.signature,
    )
    .unwrap();

    let named = |roster: &str| {
        write_profile(
            &dir,
            &format!(
                "signing_key = \"{}\"\nmachine_roster = \"{roster}\"\n",
                pkcs8_b64()
            ),
        )
    };

    let profile = named("~/.aterm/roster/aterm-machines.toml");
    let creds = sign::ReleaseCredentials::load(&profile, Some(&dir)).expect("loads");
    assert_eq!(
        creds.machine_roster(),
        Some(roster_dir.join("aterm-machines.toml").as_path())
    );
    let read = machines::RosterDocument::read(creds.machine_roster().unwrap()).expect("reads");
    assert_eq!(
        read, document,
        "the roster and its sibling signature, under this home"
    );

    let other_home = dir.join("elsewhere");
    let creds = sign::ReleaseCredentials::load(&profile, Some(&other_home)).expect("loads");
    assert_eq!(
        creds.machine_roster(),
        Some(
            other_home
                .join(".aterm/roster/aterm-machines.toml")
                .as_path()
        ),
        "the same profile names the roster under each home it is loaded in"
    );

    let refused = sign::ReleaseCredentials::load(&profile, None)
        .expect_err("a ~/ roster with no home must refuse");
    assert!(refused.contains("HOME"), "{refused}");
    for relative in ["", "relative/home"] {
        let refused = sign::ReleaseCredentials::load(&profile, Some(Path::new(relative)))
            .expect_err("a ~/ roster under a relative home must refuse");
        assert!(refused.contains("HOME"), "{relative:?}: {refused}");
    }

    let profile = named("~//.aterm/roster/aterm-machines.toml");
    let creds = sign::ReleaseCredentials::load(&profile, Some(&dir)).expect("loads");
    assert_eq!(
        creds.machine_roster(),
        Some(roster_dir.join("aterm-machines.toml").as_path()),
        "`~//r` is still under the home, as a shell reads it"
    );

    let absolute = roster_dir.join("aterm-machines.toml");
    let profile = named(&absolute.display().to_string());
    let creds = sign::ReleaseCredentials::load(&profile, Some(&other_home)).expect("loads");
    assert_eq!(creds.machine_roster(), Some(absolute.as_path()));

    let profile = named("~someone/aterm-machines.toml");
    let creds = sign::ReleaseCredentials::load(&profile, Some(&dir)).expect("loads");
    assert_eq!(
        creds.machine_roster(),
        Some(Path::new("~someone/aterm-machines.toml")),
        "only `~/` names this home"
    );
    clean(&dir);
}

/// End to end: a real credentials profile and roster file on disk, a synthetic master, and the
/// verdict a cut would carry, through the lines between profile and gate (reading the named
/// roster, resolving the declared id, folding both into the evidence).
#[test]
fn a_real_profile_plus_a_real_roster_file_produce_the_cut_s_verdict() {
    let dir = tempdir("verdict");
    let (pkcs8, pubkey) = fresh_keypair();
    let document = roster_naming(&[("m3", &pubkey)], &[], &MASTER);
    let roster_path = dir.join("aterm-machines.toml");
    std::fs::write(&roster_path, &document.bytes).unwrap();
    std::fs::write(dir.join("aterm-machines.toml.sig"), &document.signature).unwrap();
    let profile = write_profile(
        &dir,
        &format!(
            "signing_key = \"{pkcs8}\"\nmachine_roster = \"{}\"\n",
            roster_path.display()
        ),
    );
    let creds = sign::ReleaseCredentials::load(&profile, None).expect("loads");
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let verdict =
        publish::signing_verdict(Some(&creds), &anchors(&masters, None)).expect("an armed cut");
    assert!(verdict.policy.required);
    assert_eq!(verdict.policy.pubkey.as_deref(), Some(pubkey.as_str()));
    let who = verdict.attribution.expect("armed means attributed");
    assert_eq!(who.machine_id, "m3");
    assert_eq!(
        verdict
            .roster
            .expect("the authorizing bytes are carried forward"),
        document,
        "the verdict must carry the exact document it verified, not the path"
    );

    let identity = dir.join("machine.toml");
    std::fs::write(&identity, "id = \"m11\"\npubkey = \"unused\"\n").unwrap();
    publish::signing_verdict(Some(&creds), &anchors(&masters, Some(&identity)))
        .expect_err("a mint record naming another machine must refuse");
    std::fs::write(&identity, "id = \"m3\"\npubkey = \"unused\"\n").unwrap();
    publish::signing_verdict(Some(&creds), &anchors(&masters, Some(&identity)))
        .expect("a truthful mint record is not an obstacle");

    let orphan = write_profile2(
        &dir,
        "orphan.toml",
        &format!(
            "signing_key = \"{pkcs8}\"\nmachine_roster = \"{}\"\n",
            dir.join("gone.toml").display()
        ),
    );
    let creds = sign::ReleaseCredentials::load(&orphan, None).expect("loads");
    publish::signing_verdict(Some(&creds), &anchors(&masters, None))
        .expect_err("a named-but-missing roster must refuse");
    clean(&dir);
}

/// The profile's machine id wins; the mint record answers only when the profile declares none.
#[test]
fn the_declared_machine_id_prefers_the_profile_and_falls_back_to_the_mint_record() {
    let dir = tempdir("identity");
    let identity = dir.join("machine.toml");
    std::fs::write(
        &identity,
        format!("id = \"m11\"\npubkey = \"{}\"\n", pk(&M11)),
    )
    .unwrap();

    assert_eq!(
        machines::declared_machine_id(Some("m3"), Some(&identity)).unwrap(),
        Some("m3".to_string())
    );
    assert_eq!(
        machines::declared_machine_id(None, Some(&identity)).unwrap(),
        Some("m11".to_string())
    );
    assert_eq!(
        machines::declared_machine_id(None, Some(&dir.join("absent.toml"))).unwrap(),
        None
    );
    assert_eq!(machines::declared_machine_id(None, None).unwrap(), None);
    std::fs::write(&identity, "id = = =").unwrap();
    assert!(machines::declared_machine_id(None, Some(&identity)).is_err());
    clean(&dir);
}

fn tempdir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-machine-roster-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn clean(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn write_profile(dir: &Path, body: &str) -> PathBuf {
    write_profile2(dir, "release-credentials.toml", body)
}

/// Mode 0600: the loader refuses anything else, since the file holds a private key.
fn write_profile2(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

/// A fresh Ed25519 keypair as (base64 PKCS#8, base64 public key), written only to a temp
/// profile the test deletes.
fn fresh_keypair() -> (String, String) {
    let rng = ring::rand::SystemRandom::new();
    let doc = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let kp = Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap();
    (b64(doc.as_ref()), b64(kp.public_key().as_ref()))
}

/// A base64 PKCS#8 Ed25519 key generated here, signing nothing that leaves this process.
fn pkcs8_b64() -> String {
    use std::sync::OnceLock;
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        let rng = ring::rand::SystemRandom::new();
        let doc = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        b64(doc.as_ref())
    })
    .clone()
}

fn inputs(version: &'static str, build: u64) -> manifest_out::ManifestInputs<'static> {
    manifest_out::ManifestInputs {
        version,
        build_number: build,
        commit: COMMIT,
        dmg_name: if version == "0.5.0" {
            "aterm-0.5.0.dmg"
        } else {
            "aterm-0.99.0.dmg"
        },
        dmg_sha256: DMG_SHA,
        zip_name: if version == "0.5.0" {
            "aterm-0.5.0-mac.zip"
        } else {
            "aterm-0.99.0-mac.zip"
        },
        zip_sha256: ZIP_SHA,
        repo_slug: "owner/repo",
        min_os: "11.0",
        team_id: "",
        pub_date: "2026-08-11T00:00:00Z",
        min_build: None,
        changelog: "### Added\n- a thing\n",
    }
}

const COMMIT: &str = "abcdef0123456789abcdef0123456789abcdef01";
const DMG_SHA: &str = "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12";
const ZIP_SHA: &str = "cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34";

/// Does the roster body list this public key? Read from the serialized document, so the
/// preconditions are checked against the bytes the gate sees.
fn document_lists(document: &machines::RosterDocument, pubkey: &str) -> bool {
    let text = std::str::from_utf8(&document.bytes).expect("fixture roster is utf8");
    text.contains(pubkey)
}
