// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PRODUCER SIDE of the paper-master machine roster: the two-state signing gate,
//! the attribution stamp that has to land inside the signed bytes, the roster assets a
//! rostered release must carry, and the resume rule that stops one machine finishing
//! another machine's cut.
//!
//! Everything here runs with NO network, NO Apple account, NO real key and NO real
//! master. Every master, machine key and signature below is generated in-process from an
//! obviously synthetic seed and exists nowhere else in the tree.
//!
//! # Two kinds of test live here, and both are load-bearing
//!
//! **The ARMED tests are the ones to read first**, because the armed path is the one every
//! cut from this tree takes: `pins::PAPER_MASTER_PUBKEYS` has pinned the real paper master
//! since 2026-08-15 (`atpkg-keys setup --id m3`), which
//! `the_shipped_paper_master_is_armed_and_has_no_empty_member` below asserts. They drive
//! the same production code with a SYNTHETIC master, because no test may hold the real
//! one — its secret half is on paper.
//!
//! **The empty-anchor tests** pin the path a FORK takes: per-machine opt-in signing, no
//! attribution, no roster assets. An unarmed anchor really is inert and authorizes
//! nobody — it just is not this tree's state.
//!
//! Each test that kills a specific mutation says which one.

// The release crate is a binary on purpose (the spec's §9 file plan has no lib.rs), so
// the integration tests compile the modules under test directly. publish.rs reaches every
// stage through `crate::`, hence the full mount list.
#[path = "../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
#[path = "../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use std::path::{Path, PathBuf};

use aterm_update_core::Manifest;
use aterm_update_core::roster::{Attribution, Machine, Roster};
/// Standard padded Base64 — the shipped encoder (`aterm_codec::base64`), held
/// byte-identical to the retired `base64` package's `general_purpose::STANDARD`
/// by `crates/aterm-codec/tests/base64_oracle.rs`.
fn b64(raw: &[u8]) -> String {
    aterm_codec::base64::encode(raw).expect("test key material is far below MAX_INPUT_LEN")
}
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use publish::{RosterEvidence, SignaturePolicy};

// ---------------------------------------------------------------------------
// synthetic material — obviously fake, generated here, stored nowhere
// ---------------------------------------------------------------------------

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
/// 2027-02-01T00:00:00Z — the `valid_until` every fixture roster here carries, named so
/// the margin tests can stand on the boundary rather than guess at it.
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

/// The general form: a roster listing exactly these (id, pubkey) pairs, signed by
/// `signer`. Taking the keys as arguments is what lets a test bind a roster to a
/// FRESHLY GENERATED signing key rather than to a fixture's say-so.
fn roster_naming(
    machines_in: &[(&str, &str)],
    revoked: &[&str],
    signer: &[u8; 32],
) -> machines::RosterDocument {
    roster_naming_at(machines_in, revoked, signer, 4)
}

/// The same, with the roster GENERATION under the caller's control — what the channel
/// ratchet is about.
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

/// The ARMED evidence a healthy m3 cut presents.
fn armed<'a>(master: &'a [&'a str], document: &'a machines::RosterDocument) -> RosterEvidence<'a> {
    RosterEvidence {
        master_pubkeys: master,
        roster: Some(document),
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
    }
}

/// The anchors an ARMED cut resolves against, as parameters — the shape
/// `publish::signing_verdict` takes so that a test can drive it with a synthetic master.
fn anchors<'a>(masters: &'a [&'a str], identity: Option<&'a Path>) -> publish::SigningAnchors<'a> {
    publish::SigningAnchors {
        master_pubkeys: masters,
        identity_path: identity,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
    }
}

/// The INERT evidence — the tier a FORK or a pre-v0.21.0 build carries. WRONG BEFORE:
/// this said "the tier the shipped build carries", which stopped being true when
/// `PAPER_MASTER_PUBKEYS` was armed on 2026-08-15. Inert still means inert: an empty
/// master authorizes nobody.
fn inert<'a>() -> RosterEvidence<'a> {
    RosterEvidence {
        master_pubkeys: &[],
        roster: None,
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
    }
}

// ---------------------------------------------------------------------------
// THE ARMED ANCHOR — the tier is live as of 2026-08-15
// ---------------------------------------------------------------------------

/// The shipped anchor is ARMED: `atpkg-keys setup --id m3` pinned the paper master on
/// 2026-08-15 (this replaced the unset-anchor tripwire that stood here, as that test's
/// own doc prescribed). What must still never happen: an empty MEMBER, which would read
/// as armed and then authorize nobody.
#[test]
fn the_shipped_paper_master_is_armed_and_has_no_empty_member() {
    assert!(!aterm_update_core::pins::PAPER_MASTER_PUBKEYS.is_empty());
    assert!(aterm_update_core::pins::roster_tier_armed());
    assert!(
        !aterm_update_core::pins::PAPER_MASTER_PUBKEYS.contains(&""),
        "an empty master member is never legal"
    );
}

// ---------------------------------------------------------------------------
// THE ROSTER RATCHET — the producer's floor against the channel's
// ---------------------------------------------------------------------------

/// A cut may not publish a roster generation the channel has already moved past.
///
/// The client ratchets `roster_seq` on OBSERVATION and refuses anything below its
/// durable floor with `RosterReject::Rollback`, before any artifact crypto — and
/// `select_authoritative_release` picks exactly one candidate with no fallback to an
/// older release, so a rolled-back head does not delay those clients, it stops them
/// updating entirely while the cut reports success.
///
/// `machines::authorize_cut` cannot see that floor (it is channel state, not a property
/// of a local file) and passes 0 deliberately. This is the function that owns it, and
/// the test below checks the producer's verdict against the CLIENT's own `admit` on the
/// same numbers rather than against a restatement of the rule.
///
/// Kills the mutation "return Ok unconditionally", and the subtler "refuse only when
/// strictly greater" (republishing AT the head's generation is what a second machine
/// holding the same roster does, and must be allowed).
#[test]
fn a_cut_may_not_publish_an_older_roster_generation_than_the_channel_head() {
    let master = pk(&MASTER);
    let m3 = pk(&M3);
    // The producer's verdict must agree with the client's, generation for generation.
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

    // The message names both generations, because "refresh your roster" is only
    // actionable if the operator can see which one the channel is standing on.
    let err = publish::roster_floor_covered(Some(4), Some(5)).expect_err("a rollback");
    assert!(
        err.to_string().contains('4') && err.to_string().contains('5'),
        "{err}"
    );

    // AN UNROSTERED CHANNEL admits everything, which is the FORK state and
    // must stay free: no head generation means no floor to clear. WRONG BEFORE: this
    // called it "the shipped state" and the expect below "every cut this tree makes" —
    // the master has been armed since 2026-08-15, so a cut from this tree IS rostered.
    publish::roster_floor_covered(None, None).expect("an unrostered channel head");
    publish::roster_floor_covered(Some(4), None).expect("the first rostered release");
    // ...but DROPPING the tier against a rostered head is refused rather than ignored:
    // an armed client refuses an unattributed release structurally.
    publish::roster_floor_covered(None, Some(5)).expect_err("a downgrade is not a floor pass");
}

// ---------------------------------------------------------------------------
// DUTY — what a re-entry has any business re-proving
// ---------------------------------------------------------------------------

/// A pipeline entry that will still SIGN proves the whole roster chain. One that is
/// finishing already-signed bytes proves the key and stops.
///
/// The second case is not laxity, it is correctness: the roster such an entry would
/// read is not the roster the cut is publishing (that one is frozen in `dist/` and
/// inside a signature), so a verdict about it could only ever fail spuriously — and
/// satisfying it, by re-signing from the paper master, would not change one byte of
/// what gets published. What it WOULD do is strand a cut that is one upload from done,
/// on the path taken when something has already gone wrong, with the release live on
/// the publish repo and absent from the public channel the fleet reads.
///
/// Kills the mutation "run the roster chain regardless of duty" (every case below then
/// refuses).
#[test]
fn a_finish_only_entry_proves_the_key_and_not_the_roster() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let m3 = pk(&M3);
    let fresh = roster_doc(&[], &MASTER);
    let revoking = roster_doc(&["m3"], &MASTER);

    // Every arrangement a FINISH entry can meet, including the ones that legitimately
    // refuse a SIGN entry. All must return the key verdict and NO attribution.
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
        // Precondition, so the acceptances above are not vacuous: a SIGN entry with the
        // same evidence really does refuse the three unhealthy arrangements.
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

    // WHAT A FINISH ENTRY DOES NOT RE-ASK: whether the roster names the key. That was
    // answered at pre-claim about a key this entry is not permitted to change; asking
    // again could only fail SPURIOUSLY, on the path taken when something has ALREADY gone
    // wrong.
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
    // Precondition, so the acceptance above is not vacuous: the SAME key, at the entry
    // that actually chooses it, IS refused.
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
    // ...and a keyless machine still may not cut for a rostered channel.
    assert!(publish::channel_signature_policy(None, &finishing).is_err());
}

/// The MANIFEST BYTES half of the same promise: with no attribution to stamp, the
/// staged manifest is byte-identical to what `manifest_out` alone produces, and
/// carries neither key.
///
/// Kills the mutation "stamp a placeholder/default attribution when none is given".
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
    // And nothing is staged beside it.
    publish::stage_roster_assets(&dir, None).expect("no document, no assets");
    assert!(!dir.join("aterm-machines.toml").exists());
    assert!(!dir.join("aterm-machines.toml.sig").exists());
    clean(&dir);
}

/// The ASSET-SET half: an unattributed release requires exactly the set it always
/// did, and a roster asset smuggled onto it is refused as an unexpected object.
#[test]
fn an_unattributed_release_carries_no_roster_assets() {
    assert_eq!(
        mirror::required_asset_names("0.5.0", true, false),
        vec![
            "aterm-0.5.0-mac.zip".to_string(),
            "aterm-0.5.0-mac.zip.sha256".to_string(),
            "aterm-0.5.0.dmg".to_string(),
            "aterm-0.5.0.dmg.sha256".to_string(),
            "aterm-appcast.toml".to_string(),
            "aterm-appcast.toml.sig".to_string(),
            "aterm-mac.zip".to_string(),
            "aterm-mac.zip.sha256".to_string(),
            "aterm.dmg".to_string(),
            "aterm.dmg.sha256".to_string(),
        ],
        // WRONG BEFORE: "while the master is unpinned" — armed since 2026-08-15. What
        // keeps this set frozen is that the CUT is unattributed, which is what the
        // `rostered: false` argument above asks for.
        "the mirrored set must not grow for an unattributed cut \
         (the stable download twins and their alias sidecars are \
          version-independent, not roster growth)"
    );
    let manifest = manifest_out::build(&inputs("0.5.0", 500));
    assert_eq!(manifest.machine_id, None, "precondition: unattributed");
    let names = draft_names(&manifest, true, &[]);
    publish::validate_draft_asset_set(&names, &manifest, true, PROVENANCE, None)
        .expect("today's exact set is accepted");
    let smuggled = draft_names(&manifest, true, &["aterm-machines.toml"]);
    let err = publish::validate_draft_asset_set(&smuggled, &manifest, true, PROVENANCE, None)
        .expect_err("a roster on an unattributed release is not part of the exact set");
    assert!(err.to_string().contains("aterm-machines.toml"), "{err}");
}

/// THE INTEL DMG PAIR IS RETIRED (2026-08-26). A manifest that still names
/// `dmg_x86_64` was staged by a previous cutter under a container contract
/// this one neither produces nor mirrors, so the draft gate refuses it BY
/// NAME — before judging the asset set — rather than half-honouring a pair
/// the exact set no longer carries. And a smuggled `-x86_64.dmg` under a
/// manifest that (correctly) names none is a foreign object, as it always was.
#[test]
fn a_manifest_naming_the_retired_intel_dmg_is_refused_and_a_smuggled_one_too() {
    let plain = manifest_out::build(&inputs("0.5.0", 500));
    assert_eq!(plain.dmg_x86_64, None, "this cutter never emits the pair");
    assert_eq!(plain.dmg_x86_64_sha256, None);
    let smuggled = draft_names(&plain, true, &["aterm-0.5.0-x86_64.dmg"]);
    let err = publish::validate_draft_asset_set(&smuggled, &plain, true, PROVENANCE, None)
        .expect_err("an Intel DMG the manifest never named must be refused");
    assert!(err.to_string().contains("x86_64"), "{err}");

    let mut named = manifest_out::build(&inputs("0.5.0", 500));
    named.dmg_x86_64 = Some("aterm-0.5.0-x86_64.dmg".to_string());
    named.dmg_x86_64_sha256 = Some("ab".repeat(32));
    // Even a draft that DOES carry the pair is refused: the contract is gone.
    let mut names = draft_names(&named, true, &[]);
    names.push("aterm-0.5.0-x86_64.dmg".to_string());
    names.push("aterm-0.5.0-x86_64.dmg.sha256".to_string());
    let err = publish::validate_draft_asset_set(&names, &named, true, PROVENANCE, None)
        .expect_err("a manifest naming the retired pair is refused by name");
    assert!(err.to_string().contains("retired"), "{err}");
    let err = publish::refuse_retired_intel_dmg(&named).expect_err("same rule, directly");
    assert!(err.to_string().contains("aterm-0.5.0-x86_64.dmg"), "{err}");
    // A digest without a name is the same retired shape.
    let mut half = manifest_out::build(&inputs("0.5.0", 500));
    half.dmg_x86_64_sha256 = Some("ab".repeat(32));
    assert!(publish::refuse_retired_intel_dmg(&half).is_err());
}

// ---------------------------------------------------------------------------
// THE ARMED ANCHOR — the roster governs, and refuses
// ---------------------------------------------------------------------------

/// A listed machine cuts, and the verdict carries both halves: the policy the
/// pipeline has always had, plus WHO it is.
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

/// THE MUTATION TEST the whole armed path hangs on: delete the `machines::authorize_cut`
/// call from `channel_signature_policy` (return `(policy, None)` instead) and this fails
/// on every one of these cases — an unlisted key, a revoked machine, a lapsed roster, a
/// roster under the wrong master, a corrupted signature, and a missing roster all cut
/// happily.
///
/// Each case also asserts its own precondition, so none of them can pass vacuously by
/// failing for an unrelated reason.
#[test]
fn an_armed_anchor_refuses_every_machine_the_roster_does_not_authorize() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];

    // (1) A KEY ON NO ROSTER.
    let document = roster_doc(&[], &MASTER);
    assert!(
        !document_lists(&document, &pk(&STRANGER)),
        "precondition: the stranger really is absent from the roster"
    );
    let unlisted = armed(&masters, &document);
    let err = publish::channel_signature_policy(Some(&pk(&STRANGER)), &unlisted)
        .expect_err("an unlisted key may not cut");
    assert!(
        err.to_string().contains("not on the machine roster"),
        "{err}"
    );

    // (2) A REVOKED MACHINE, holding its own key and its own copy of the roster.
    let revoked = roster_doc(&["m11"], &MASTER);
    assert!(
        document_lists(&revoked, &pk(&M11)),
        "precondition: m11 is still LISTED — it is the deny-list that must stop it"
    );
    let err = publish::channel_signature_policy(Some(&pk(&M11)), &armed(&masters, &revoked))
        .expect_err("a revoked machine may not cut");
    assert!(err.to_string().contains("may not sign"), "{err}");
    // The refusal is TARGETED: m3 still cuts under the same document.
    publish::channel_signature_policy(Some(&pk(&M3)), &armed(&masters, &revoked))
        .expect("revoking m11 must not revoke m3");

    // (3) A LAPSED ROSTER. Publishing under it would produce a release every client
    // refuses, so the cutter is strictly the better place to find out.
    let mut lapsed = armed(&masters, &document);
    lapsed.now_unix = LONG_AFTER;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), &lapsed)
        .expect_err("a lapsed roster may not authorize a cut");
    assert!(err.to_string().contains("not usable for a cut"), "{err}");

    // (3b) A roster that is still valid, but not for LONG ENOUGH. The producer checks
    // the window at a strictly earlier clock than every client does, so "valid now" is
    // the wrong question — a cut takes the better part of an hour and the fleet stages
    // over six. Refusing pre-claim is the only place this is free.
    let mut about_to_lapse = armed(&masters, &document);
    about_to_lapse.now_unix = FIXTURE_VALID_UNTIL - 60;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), &about_to_lapse)
        .expect_err("a roster with a minute left may not start a ~20 minute cut");
    assert!(err.to_string().contains("not usable for a cut"), "{err}");

    // (4) THE WRONG MASTER — a roster signed by a key that is not the pinned anchor.
    let foreign = roster_doc(&[], &OTHER_MASTER);
    assert_eq!(
        foreign.bytes, document.bytes,
        "precondition: only the SIGNATURE differs, so this tests the anchor and not the body"
    );
    let err = publish::channel_signature_policy(Some(&pk(&M3)), &armed(&masters, &foreign))
        .expect_err("a roster under another master authorizes nothing");
    assert!(err.to_string().contains("does not verify"), "{err}");

    // (5) A CORRUPTED SIGNATURE over the right body under the right master.
    let mut torn = roster_doc(&[], &MASTER);
    torn.signature[0] ^= 0xff;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), &armed(&masters, &torn))
        .expect_err("a torn master signature authorizes nothing");
    assert!(err.to_string().contains("does not verify"), "{err}");

    // (6) NO ROSTER AT ALL. The armed anchor must never degrade to an unrostered
    // cut — this is the case that would silently re-open exactly what the tier closes.
    let mut absent = armed(&masters, &document);
    absent.roster = None;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), &absent)
        .expect_err("an armed anchor with no roster must refuse, never fall through");
    assert!(err.to_string().contains("machine_roster"), "{err}");

    // (7) A KEYLESS MACHINE. It could not sign anything anyway; it must be told so
    // pre-claim rather than at the moment of signing.
    let err = publish::channel_signature_policy(None, &armed(&masters, &document))
        .expect_err("a keyless machine may not cut for a rostered channel");
    assert!(err.to_string().contains("no signing material"), "{err}");
    assert!(
        err.to_string().contains("no ledger claim was made"),
        "{err}"
    );
}

/// An IDENTITY MISMATCH refuses. The declared id is never authority — the roster's
/// key→id map is — but a profile that disagrees with it means a copied profile or a
/// re-minted machine, and either would publish an attribution that is true of the
/// bytes and false of the world.
#[test]
fn a_machine_that_declares_the_wrong_id_may_not_cut() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let document = roster_doc(&[], &MASTER);
    let mut evidence = armed(&masters, &document);
    evidence.declared_machine_id = Some("m11");
    let err = publish::channel_signature_policy(Some(&pk(&M3)), &evidence)
        .expect_err("m3's key declared as m11 must refuse");
    assert!(err.to_string().contains("m11"), "{err}");
    assert!(err.to_string().contains("m3"), "{err}");
    // The truthful declaration passes, so the refusal above is about the MISMATCH and
    // not about declaring an id at all.
    evidence.declared_machine_id = Some("m3");
    publish::channel_signature_policy(Some(&pk(&M3)), &evidence)
        .expect("a truthful declaration is not an obstacle");
}

/// THE IDENTITY-MISMATCH REMEDY names the declared machine's own key only when the
/// roster actually names that machine — never a key nothing authorizes.
#[test]
fn the_identity_mismatch_remedy_offers_the_declared_machines_key_only_when_rostered() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let document = roster_doc(&[], &MASTER);
    let mut evidence = armed(&masters, &document);
    evidence.declared_machine_id = Some("m3");
    let err = publish::channel_signature_policy(Some(&pk(&M11)), &evidence)
        .expect_err("m11's key declared as m3 must refuse")
        .to_string();
    assert!(
        err.contains("or cut with the key that belongs to \"m3\""),
        "a rostered declared machine gets the alternative: {err}"
    );
    // Negative control: a declared id the roster does not name gets no alternative.
    evidence.declared_machine_id = Some("ghost");
    let err = publish::channel_signature_policy(Some(&pk(&M11)), &evidence)
        .expect_err("a declared id the roster does not name must refuse")
        .to_string();
    assert!(!err.contains("or cut with the key"), "{err}");
    assert!(!err.contains("strand"), "no retired wording: {err}");
}

// ---------------------------------------------------------------------------
// K1 RETIRED — the roster is the whole question
// ---------------------------------------------------------------------------

/// A ROSTERED KEY THAT NO KEYSET EVER HELD CUTS, with no acknowledgement of any kind.
///
/// Before K1 was retired a key that was not `UPDATE_CHANNEL_PUBKEYS[0]` was refused
/// pre-claim unless the command carried `--strand-pre-roster-clients`, because clients
/// older than v0.21.0 verified under that keyset alone. Those installs are abandoned
/// and every current client authorizes by the roster alone, so the roster is the whole
/// question: m11 — a freshly minted machine — cuts on the gate's first ask. Negative
/// controls: the same key REVOKED, and a key the roster never named, both refuse.
#[test]
fn a_rostered_key_no_keyset_ever_held_cuts_with_no_acknowledgement() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let document = roster_doc(&[], &MASTER);
    assert!(
        document_lists(&document, &pk(&M11)),
        "precondition: m11 is rostered"
    );
    let (policy, who) =
        publish::channel_signature_policy(Some(&pk(&M11)), &armed(&masters, &document))
            .expect("a rostered key cuts with no flag");
    assert_eq!(policy.pubkey.as_deref(), Some(pk(&M11).as_str()));
    assert_eq!(who.expect("attributed").machine_id, "m11");

    let revoked = roster_doc(&["m11"], &MASTER);
    assert!(
        publish::channel_signature_policy(Some(&pk(&M11)), &armed(&masters, &revoked)).is_err(),
        "revocation still bites"
    );
    assert!(
        publish::channel_signature_policy(Some(&pk(&STRANGER)), &armed(&masters, &document))
            .is_err(),
        "an unrostered key still refuses"
    );
}

/// AN UNPINNED MASTER (a fork) is per-machine opt-in and attributes nothing — even with
/// perfectly valid roster evidence dangled in front of it, at either duty.
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

// ---------------------------------------------------------------------------
// ATTRIBUTION INSIDE THE SIGNED BYTES
// ---------------------------------------------------------------------------

/// The stamp must land INSIDE what the signature covers, and the only way to show that
/// is to sign the staged file and verify against the file.
///
/// Two mutations die here:
///   * drop the `machines::attribute` call from `publish::stage_manifest` — the staged
///     bytes carry no `machine_id` and the first assertion fails;
///   * stamp AFTER signing (the negative control at the bottom does exactly that) —
///     the signature no longer verifies over the published bytes, which is the failure
///     `sign_manifest_with_policy`'s own read-back check produces in production.
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

    // Sign exactly what is on disk, as the pipeline does.
    let signature = kp(&M3).sign(&signed_bytes).as_ref().to_vec();
    publish::verify_detached_manifest_signature(&pk(&M3), &signed_bytes, &signature)
        .expect("the signature covers the staged bytes");
    // And the client reads the attribution back out of those very bytes.
    let parsed = Manifest::parse(&text).expect("client parses");
    assert_eq!(parsed.machine_id.as_deref(), Some("m3"));
    assert_eq!(parsed.roster_seq, Some(4));

    // THE NEGATIVE CONTROL — the "stamp after signing" ordering, done deliberately, so
    // the ordering above is demonstrated to matter rather than merely asserted.
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

// ---------------------------------------------------------------------------
// THE ROSTER ASSETS
// ---------------------------------------------------------------------------

/// An armed cut publishes the roster it was authorized by — byte-identically, from the
/// bytes the gate held, not from a re-read of the file — and the asset rules demand
/// both halves.
///
/// Kills the mutation "stage the roster from its path at build time instead of from the
/// authorized document": the staged bytes below are compared to the document's.
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

    // The mirrored set the client elects grows by exactly those two names.
    let names = mirror::required_asset_names("0.5.0", true, true);
    assert!(
        names.contains(&"aterm-machines.toml".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"aterm-machines.toml.sig".to_string()),
        "{names:?}"
    );
    mirror::validate_mirror_asset_set_with_linux(&names, "0.5.0", true, true, &[])
        .expect("the exact set");
    // ...and a mirror that forgets the roster is refused rather than published: the
    // armed client refuses such a head structurally, before any artifact crypto.
    let forgotten = mirror::required_asset_names("0.5.0", true, false);
    let err = mirror::validate_mirror_asset_set_with_linux(&forgotten, "0.5.0", true, true, &[])
        .expect_err("a rostered channel head without its roster is unelectable");
    assert!(err.to_string().contains("aterm-machines.toml"), "{err}");

    // The DRAFT set is judged by what the manifest says about itself: an attributed
    // manifest requires both assets, and their absence is named.
    let mut manifest = manifest_out::build(&inputs("0.5.0", 500));
    machines::attribute(
        &mut manifest,
        &Attribution {
            machine_id: "m3".into(),
            pubkey_b64: pk(&M3),
            roster_seq: 4,
        },
    );
    let complete = draft_names(
        &manifest,
        true,
        &["aterm-machines.toml", "aterm-machines.toml.sig"],
    );
    publish::validate_draft_asset_set(&complete, &manifest, true, PROVENANCE, None)
        .expect("an attributed draft carrying its roster is the exact set");
    let missing = draft_names(&manifest, true, &[]);
    let err = publish::validate_draft_asset_set(&missing, &manifest, true, PROVENANCE, None)
        .expect_err("an attributed draft without its roster must not flip visible");
    assert!(err.to_string().contains("aterm-machines.toml"), "{err}");
    clean(&dir);
}

/// THE BINARY PIN EXPECTATION IS THE PAPER MASTER, never the signer: the shipped binary
/// embeds `PAPER_MASTER_PUBKEYS[0]`'s fingerprint in `__DATA,__aterm_upin`, and the build
/// proves the embedded value against `expected_embedded_update_pin`. Whichever rostered
/// machine cuts, the expectation is the same string; a fork with no master has none.
///
/// Kills the mutation "expect the signing key's fingerprint": the last assertion fails.
#[test]
fn the_binary_pin_expectation_is_the_master_not_the_signer() {
    let master = pk(&MASTER);
    let expected = publish::expected_embedded_update_pin(&[master.as_str()])
        .expect("a pinned master has an expectation")
        .expect("pinned means Some");
    assert_eq!(expected.len(), 64);
    assert_eq!(
        publish::expected_embedded_update_pin(&[]).unwrap(),
        None,
        "a fork with no master has nothing to prove"
    );
    // The master's fingerprint is not any signer's.
    assert_ne!(
        Some(expected),
        publish::expected_embedded_update_pin(&[pk(&M3).as_str()]).unwrap(),
        "the expectation names the master, not the machine that cuts"
    );
}

/// THE RUNBOOK DESCRIBES THE PIN THIS TREE EMBEDS. An operator chasing a pin mismatch
/// reads docs/RELEASING.md, so while `PAPER_MASTER_PUBKEYS` is pinned the runbook must
/// name the master's fingerprint as the `__aterm_upin` record and carry none of the
/// retired opt-in-signing sentences — each of which sent the reader to compare against
/// the signing key, or to expect an unsigned cut this tree refuses pre-claim.
///
/// Negative control: the runbook as it stood before 2026-09-23 carried every one of the
/// retired sentences and failed here.
#[test]
fn the_runbook_describes_the_pin_this_tree_embeds() {
    assert!(
        !aterm_update_core::pins::PAPER_MASTER_PUBKEYS.is_empty(),
        "precondition: this tree pins a paper master"
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/RELEASING.md");
    let runbook = std::fs::read_to_string(&path).expect("read docs/RELEASING.md");
    // The runbook wraps prose at ~85 columns, so compare with every run of whitespace
    // collapsed to one space.
    let prose = runbook.split_whitespace().collect::<Vec<_>>().join(" ");
    for retired in [
        "UNSIGNED BY DEFAULT",
        "the signing-key fingerprint on a signed (opt-in) cut",
        "an unsigned cut has an empty pin",
        "an unsigned cut carries the empty pin",
        "attached ONLY when signing is opted in",
        "a keyless machine recovers an unsigned release",
        "survives only as a *build* input",
    ] {
        assert!(
            !prose.contains(retired),
            "docs/RELEASING.md still says {retired:?}, which is false for a tree that pins a \
             paper master"
        );
    }
    assert!(
        prose.contains("`__DATA,__aterm_upin` section — the SHA-256 fingerprint of the paper master, `pins::PAPER_MASTER_PUBKEYS[0]`"),
        "docs/RELEASING.md must name the paper master's fingerprint as the __aterm_upin record"
    );
}

// ---------------------------------------------------------------------------
// RESUME — one cut, one machine
// ---------------------------------------------------------------------------

/// A resume that re-authorizes as a DIFFERENT machine must abort: the manifest it would
/// finish is already signed over the first machine's attribution.
///
/// Both asymmetric cases refuse too, and they are the ones a naive `Option` comparison
/// gets wrong — an anchor armed (or unarmed) mid-cut leaves a journal and a live
/// verdict that disagree, and finishing anyway ships a release whose halves contradict
/// each other.
#[test]
fn a_resume_may_not_change_which_machine_cut_the_release() {
    // The FORK / pre-roster case: nameless journal, nameless verdict. WRONG BEFORE: this
    // said "the shipped case … every cut this tree makes"; the master has been armed
    // since 2026-08-15, so a cut from this tree journals a machine id.
    publish::resume_attribution_agrees(None, None).expect("an unattributed cut resumes");
    publish::resume_attribution_agrees(Some("m3"), Some("m3")).expect("same machine resumes");

    let err = publish::resume_attribution_agrees(Some("m3"), Some("m11"))
        .expect_err("another machine may not finish this cut");
    assert!(err.to_string().contains("m3"), "{err}");
    assert!(err.to_string().contains("m11"), "{err}");

    publish::resume_attribution_agrees(Some("m3"), None)
        .expect_err("a resume with no roster may not finish a rostered cut");
    publish::resume_attribution_agrees(None, Some("m3"))
        .expect_err("a roster armed mid-cut may not attribute bytes that carry no attribution");
}

// ---------------------------------------------------------------------------
// WHERE THE ROSTER BYTES COME FROM
// ---------------------------------------------------------------------------

/// The credentials profile names the roster, and the signature is its sibling. One name
/// in the file, not two, so a profile cannot pair a roster with a signature over some
/// other roster.
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
    let creds = sign::ReleaseCredentials::load(&profile).expect("loads");
    assert_eq!(creds.machine_id(), Some("m3"));
    assert_eq!(creds.machine_roster(), Some(roster_path.as_path()));
    let read = machines::RosterDocument::read(creds.machine_roster().unwrap()).expect("reads");
    assert_eq!(read, document, "the document is read whole, both halves");

    // A named-but-missing roster is a hard error at the pre-claim gate, not a silent
    // fall-through to an unrostered cut — the same rule the profile itself follows.
    let err = machines::RosterDocument::read(&dir.join("nope.toml"))
        .expect_err("a named roster that is not there must refuse");
    assert!(err.to_string().contains("machine_roster"), "{err}");
    // ...and so is a roster whose master signature is missing.
    std::fs::write(dir.join("lonely.toml"), &document.bytes).unwrap();
    let err = machines::RosterDocument::read(&dir.join("lonely.toml"))
        .expect_err("a roster with no master signature proves nothing");
    assert!(err.to_string().contains("master signature"), "{err}");

    // NOTHING SECRET is recorded: the debug line carries the public identity and the
    // machine id, and nothing else.
    let rendered = format!("{creds:?}");
    assert!(rendered.contains("m3"), "{rendered}");
    assert!(rendered.contains("redacted"), "{rendered}");
    assert!(
        !rendered.contains(&pkcs8_b64()),
        "the private key must never render"
    );
    clean(&dir);
}

/// THE ASSEMBLY, end to end: a real credentials profile on disk, a real roster file
/// beside it, a synthetic master — and the verdict a cut would carry.
///
/// This is the one test that runs the lines between the profile and the gate: reading
/// the named roster, resolving the declared id, and folding both into the evidence.
/// Those lines run on every cut from this tree (the master has been armed since
/// 2026-08-15 — WRONG BEFORE: this called them unreachable and the master unpinned), and
/// the anchors are still PARAMETERS so a test can drive them with a SYNTHETIC master
/// instead of the real one, whose secret half is on paper — the same reason
/// `resolve_apple_tier` takes its team id rather than reading `pins`.
///
/// Kills two mutations: "ignore `machine_roster` and pass `roster: None`" (the armed
/// call then refuses with "names no `machine_roster`"), and "pass
/// `declared_machine_id: None` always" (the mismatch case below then succeeds).
#[test]
fn a_real_profile_plus_a_real_roster_file_produce_the_cut_s_verdict() {
    let dir = tempdir("verdict");
    // A genuine keypair for the signing material, and a roster that names ITS public
    // key as m3 — so the roster and the profile are bound by the key, not by a
    // fixture's say-so.
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
    let creds = sign::ReleaseCredentials::load(&profile).expect("loads");
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

    // THE CONVENTIONAL RECORD is consulted when the profile declares no id — and a
    // stale one refuses the cut rather than publishing a wrong attribution.
    let identity = dir.join("machine.toml");
    std::fs::write(&identity, "id = \"m11\"\npubkey = \"unused\"\n").unwrap();
    let err = publish::signing_verdict(Some(&creds), &anchors(&masters, Some(&identity)))
        .expect_err("a mint record naming another machine must refuse");
    assert!(err.to_string().contains("m11"), "{err}");
    // ...and a truthful one does not get in the way.
    std::fs::write(&identity, "id = \"m3\"\npubkey = \"unused\"\n").unwrap();
    publish::signing_verdict(Some(&creds), &anchors(&masters, Some(&identity)))
        .expect("a truthful mint record is not an obstacle");

    // A profile that names a roster which is not there fails HERE — pre-claim, with
    // the file named — and never degrades to an unrostered cut.
    let orphan = write_profile2(
        &dir,
        "orphan.toml",
        &format!(
            "signing_key = \"{pkcs8}\"\nmachine_roster = \"{}\"\n",
            dir.join("gone.toml").display()
        ),
    );
    let creds = sign::ReleaseCredentials::load(&orphan).expect("loads");
    let err = publish::signing_verdict(Some(&creds), &anchors(&masters, None))
        .expect_err("a named-but-missing roster must refuse");
    assert!(err.to_string().contains("gone.toml"), "{err}");
    clean(&dir);
}

/// Explicit beats conventional, and the conventional file is only ever a cross-check.
#[test]
fn the_declared_machine_id_prefers_the_profile_and_falls_back_to_the_mint_record() {
    let dir = tempdir("identity");
    let identity = dir.join("machine.toml");
    std::fs::write(
        &identity,
        format!("id = \"m11\"\npubkey = \"{}\"\n", pk(&M11)),
    )
    .unwrap();

    // The profile wins outright — the conventional file is not consulted at all.
    assert_eq!(
        machines::declared_machine_id(Some("m3"), Some(&identity)).unwrap(),
        Some("m3".to_string())
    );
    // With no profile key, the mint record answers.
    assert_eq!(
        machines::declared_machine_id(None, Some(&identity)).unwrap(),
        Some("m11".to_string())
    );
    // A machine that was never minted declares nothing, which is not an error: most
    // machines never publish.
    assert_eq!(
        machines::declared_machine_id(None, Some(&dir.join("absent.toml"))).unwrap(),
        None
    );
    assert_eq!(machines::declared_machine_id(None, None).unwrap(), None);
    // A PRESENT but broken record IS an error — guessing an identity is how attribution
    // silently becomes wrong.
    std::fs::write(&identity, "id = = =").unwrap();
    assert!(machines::declared_machine_id(None, Some(&identity)).is_err());
    clean(&dir);
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

const PROVENANCE: &str = "aterm-0.5.0-build.txt";

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

/// 0600, always — the loader refuses anything else, and rightly: it holds a private key.
fn write_profile2(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

/// A fresh Ed25519 keypair as (base64 PKCS#8, base64 public key). Generated per call,
/// used in-process, never written anywhere but a temp profile this test deletes.
fn fresh_keypair() -> (String, String) {
    let rng = ring::rand::SystemRandom::new();
    let doc = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let kp = Ed25519KeyPair::from_pkcs8(doc.as_ref()).unwrap();
    (b64(doc.as_ref()), b64(kp.public_key().as_ref()))
}

/// A base64 PKCS#8 Ed25519 key, generated here. It signs nothing that leaves this
/// process and is regenerated on every run.
fn pkcs8_b64() -> String {
    // Deterministic across the calls inside one test: derived from a synthetic seed via
    // ring's PKCS#8 v2 encoding is not exposed, so cache one generated document.
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
        // This FIXTURE claims no team, and this file invents no Apple identity. WRONG
        // BEFORE: "the shipped tier claims no team" — `pins::APPLE_TEAM_ID` has been
        // armed ("A66A9P66Z7") since 2026-08-15.
        team_id: "",
        pub_date: "2026-08-11T00:00:00Z",
        min_build: None,
        changelog: "### Added\n- a thing\n",
    }
}

const COMMIT: &str = "abcdef0123456789abcdef0123456789abcdef01";
const DMG_SHA: &str = "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12";
const ZIP_SHA: &str = "cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34";

/// The exact asset names a draft carries for `manifest`, plus whatever `extra` adds.
fn draft_names(manifest: &Manifest, signed: bool, extra: &[&str]) -> Vec<String> {
    let mut names = vec![
        "aterm-appcast.toml".to_string(),
        manifest.dmg.clone(),
        format!("{}.sha256", manifest.dmg),
        PROVENANCE.to_string(),
    ];
    if let Some(zip) = manifest.zip.as_deref() {
        names.push(zip.to_string());
        names.push(format!("{zip}.sha256"));
    }
    if signed {
        names.push("aterm-appcast.toml.sig".to_string());
    }
    names.extend(extra.iter().map(|name| (*name).to_string()));
    names
}

/// Does the roster BODY list this public key? Read out of the serialized document rather
/// than out of the fixture builder's arguments, so the preconditions above are checked
/// against the bytes the gate will actually see.
fn document_lists(document: &machines::RosterDocument, pubkey: &str) -> bool {
    let text = std::str::from_utf8(&document.bytes).expect("fixture roster is utf8");
    text.contains(pubkey)
}
