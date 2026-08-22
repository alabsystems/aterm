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
//! **The empty-anchor tests are the ones to read first.** `pins::PAPER_MASTER_PUBKEYS` is
//! EMPTY in this tree, and `aterm_update::github::select_authoritative_release` picks
//! exactly one candidate with no fallback to an older release — so a shipped client that
//! meets a release it cannot verify is not delayed, it is WEDGED permanently. Any change
//! to the unarmed path is therefore a fleet-bricking bug, and the tests below pin that
//! path from four directions: the gate's verdict, the emitted manifest bytes, the
//! required asset set, and the draft asset allow-list.
//!
//! **The armed tests** drive the same production code with a synthetic master, because
//! the armed path is unreachable from this tree and an unreachable rule with no test is a
//! rule that rots for exactly as long as nobody would notice.
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
#[path = "../src/seedpack.rs"]
#[allow(dead_code)] // mounted for bundle/publish, whose seed lane references it
mod seedpack;
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
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
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
    B64.encode(kp(seed).public_key().as_ref())
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
fn armed<'a>(
    master: &'a [&'a str],
    keyset: &'a [&'a str],
    document: &'a machines::RosterDocument,
) -> RosterEvidence<'a> {
    RosterEvidence {
        master_pubkeys: master,
        committed_keyset: keyset,
        roster: Some(document),
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
        // FAIL-CLOSED, exactly as a real cut with no flag on the command line. Every
        // healthy fixture below signs with a key the keyset carries, so this default
        // never fires for them — which is what makes the two tests that DO trip it mean
        // something.
        pre_roster: publish::PreRosterClients::Protected,
    }
}

/// The anchors an ARMED cut resolves against, as parameters — the shape
/// `publish::signing_verdict` takes so that a test can drive it with a synthetic master.
fn anchors<'a>(
    masters: &'a [&'a str],
    keyset: &'a [&'a str],
    identity: Option<&'a Path>,
) -> publish::SigningAnchors<'a> {
    publish::SigningAnchors {
        master_pubkeys: masters,
        committed_keyset: keyset,
        identity_path: identity,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
        pre_roster: publish::PreRosterClients::Protected,
    }
}

/// The INERT evidence — the tier the shipped build carries.
fn inert<'a>() -> RosterEvidence<'a> {
    RosterEvidence {
        master_pubkeys: &[],
        committed_keyset: &[],
        roster: None,
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
        pre_roster: publish::PreRosterClients::Protected,
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
        "an empty keyset member is never legal"
    );
}

/// THE FLEET-BRICKING CASE, tested hardest: with no master pinned the gate is
/// `committed_channel_signature_policy` and nothing else — same verdict, same error
/// text, no attribution — across the ENTIRE decision table, and even when roster
/// evidence is dangled in front of it.
///
/// Kills the mutation "let the roster path run whenever a roster document is present":
/// the last two cases below supply a perfectly valid roster and a declared machine id
/// under an empty anchor, and still demand the single-key verdict.
#[test]
fn an_unpinned_master_reproduces_the_single_key_gate_exactly() {
    let pin = pk(&M3);
    let other = pk(&M11);
    let document = roster_doc(&[], &MASTER);
    // BOTH DUTIES, because the duty split added a branch to the shared gate and the
    // empty anchor must reach neither side of it. `Finish` skips the roster chain when
    // the anchor is ARMED; with an empty anchor there is no chain to skip and the
    // delegation must happen first, exactly as it did before RosterDuty existed.
    for duty in [publish::RosterDuty::Sign, publish::RosterDuty::Finish] {
        let with = |mut e: RosterEvidence<'static>| {
            e.duty = duty;
            e
        };
        // (committed, material) across the whole table, plus the two dangled-evidence
        // cases. Every one is compared against the OLD function's own answer, so this
        // test cannot drift away from the thing it is protecting.
        let table: Vec<(Option<&str>, Option<&str>, RosterEvidence<'_>)> = vec![
            (None, None, with(inert())),
            (None, Some(other.as_str()), with(inert())),
            (Some(pin.as_str()), None, with(inert())),
            (Some(pin.as_str()), Some(other.as_str()), with(inert())),
            (Some(pin.as_str()), Some(pin.as_str()), with(inert())),
            // Evidence present, anchor empty: the roster must be ignored ENTIRELY.
            (
                Some(pin.as_str()),
                Some(pin.as_str()),
                RosterEvidence {
                    master_pubkeys: &[],
                    committed_keyset: &[],
                    roster: Some(&document),
                    declared_machine_id: Some("m11"),
                    now_unix: LONG_AFTER,
                    duty,
                    pre_roster: publish::PreRosterClients::Protected,
                },
            ),
            // A key the roster WOULD have authorized, under an empty anchor, is still
            // refused by the equality check — the old rule, unchanged.
            (
                Some(pin.as_str()),
                Some(other.as_str()),
                RosterEvidence {
                    master_pubkeys: &[],
                    committed_keyset: &[],
                    roster: Some(&document),
                    declared_machine_id: Some("m11"),
                    now_unix: NOW,
                    duty,
                    pre_roster: publish::PreRosterClients::Protected,
                },
            ),
        ];
        for (committed, material, evidence) in table {
            let old = publish::committed_channel_signature_policy(committed, material);
            let new = publish::channel_signature_policy(committed, material, &evidence);
            match (old, new) {
                (Ok(old), Ok((new, attribution))) => {
                    assert_eq!(
                        old, new,
                        "verdict changed for {committed:?}/{material:?} ({duty:?})"
                    );
                    assert_eq!(
                        attribution, None,
                        "an unpinned master must attribute nothing for \
                         {committed:?}/{material:?} ({duty:?})"
                    );
                }
                (Err(old), Err(new)) => assert_eq!(
                    old.to_string(),
                    new.to_string(),
                    "refusal text changed for {committed:?}/{material:?} ({duty:?}) — the \
                     operator-facing message is part of the behaviour"
                ),
                (old, new) => panic!(
                    "the two-state gate disagreed with the single-key gate for \
                     {committed:?}/{material:?} ({duty:?}): {old:?} vs {new:?}"
                ),
            }
        }
    }
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

    // AN UNROSTERED CHANNEL admits everything, which is the shipped state and must stay
    // free: no head generation means no floor to clear.
    publish::roster_floor_covered(None, None).expect("every cut this tree makes");
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
/// refuses) and "skip the keyset check too" (the last case then passes).
#[test]
fn a_finish_only_entry_proves_the_key_and_not_the_roster() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let m3 = pk(&M3);
    let m11 = pk(&M11);
    let keyset = [m3.clone(), m11.clone()];
    let keyset_refs: Vec<&str> = keyset.iter().map(String::as_str).collect();
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
            committed_keyset: &keyset_refs,
            roster: document,
            declared_machine_id: Some("someone-else"),
            now_unix,
            duty: publish::RosterDuty::Finish,
            pre_roster: publish::PreRosterClients::Answered,
        };
        let (policy, attribution) =
            publish::channel_signature_policy(Some(&m3), Some(&m3), &evidence)
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
            committed_keyset: &keyset_refs,
            roster: document,
            declared_machine_id: None,
            now_unix,
            duty: publish::RosterDuty::Sign,
            pre_roster: publish::PreRosterClients::Protected,
        };
        if label != "a healthy roster" {
            assert!(
                publish::channel_signature_policy(Some(&m3), Some(&m3), &signing).is_err(),
                "{label} must refuse a SIGN entry, or the FINISH case proves nothing"
            );
        } else {
            signing.declared_machine_id = Some("m3");
            assert!(publish::channel_signature_policy(Some(&m3), Some(&m3), &signing).is_ok());
        }
    }

    // WHAT A FINISH ENTRY DOES NOT RE-ASK: whether this cut strands pre-roster clients.
    //
    // That question was answered at pre-claim, by the operator, about a key this entry is
    // not permitted to change. Asking again could only fail SPURIOUSLY — the bytes are
    // already signed, so no answer here changes what will be published — and it would
    // fail on the path taken when something has ALREADY gone wrong, turning a cut that is
    // one upload from done into one that can never be finished. Exactly the trade
    // `RosterDuty::Finish` already makes for the roster chain.
    let stranger = pk(&STRANGER);
    let finishing = RosterEvidence {
        master_pubkeys: &masters,
        committed_keyset: &keyset_refs,
        roster: Some(&fresh),
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Finish,
        pre_roster: publish::PreRosterClients::Answered,
    };
    let (policy, attribution) =
        publish::channel_signature_policy(Some(&m3), Some(&stranger), &finishing)
            .expect("a finish entry does not re-litigate the installed base");
    assert!(policy.required);
    assert_eq!(policy.pubkey.as_deref(), Some(stranger.as_str()));
    assert_eq!(attribution, None);
    // Precondition, so the acceptance above is not vacuous: the SAME key, at the entry
    // that actually chooses it, IS refused.
    let starting = RosterEvidence {
        master_pubkeys: &masters,
        committed_keyset: &keyset_refs,
        roster: Some(&fresh),
        declared_machine_id: None,
        now_unix: NOW,
        duty: publish::RosterDuty::Sign,
        pre_roster: publish::PreRosterClients::Protected,
    };
    assert!(
        publish::channel_signature_policy(Some(&m3), Some(&stranger), &starting).is_err(),
        "the pre-roster obligation must bite where the key is chosen"
    );
    // ...and a keyless machine still may not cut for a rostered channel.
    assert!(publish::channel_signature_policy(Some(&m3), None, &finishing).is_err());
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
    let staged = publish::stage_manifest(&dir, &inputs, None).expect("stages");
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
            "aterm.dmg".to_string(),
        ],
        "the mirrored set must not grow while the master is unpinned \
         (the stable download twin is version-independent, not roster growth)"
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

// ---------------------------------------------------------------------------
// THE ARMED ANCHOR — the roster governs, and refuses
// ---------------------------------------------------------------------------

/// A listed machine cuts, and the verdict carries both halves: the policy the
/// pipeline has always had, plus WHO it is.
#[test]
fn an_armed_anchor_authorizes_a_listed_machine_and_names_it() {
    let master = pk(&MASTER);
    let document = roster_doc(&[], &MASTER);
    let keyset = [pk(&M3), pk(&M11)];
    let keyset: Vec<&str> = keyset.iter().map(String::as_str).collect();
    let (policy, who) = publish::channel_signature_policy(
        Some(&pk(&M3)),
        Some(&pk(&M3)),
        &armed(&[&master], &keyset, &document),
    )
    .expect("a listed, unrevoked machine inside the keyset may cut");
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
    let full_keyset = [pk(&M3), pk(&M11), pk(&STRANGER)];
    let keyset: Vec<&str> = full_keyset.iter().map(String::as_str).collect();

    // (1) A KEY ON NO ROSTER. The replacement for the old "must equal
    // UPDATE_CHANNEL_PUBKEYS[0]" equality: same refusal, wider allowance.
    //
    // Acknowledged deliberately. The pre-roster obligation is a DIFFERENT question and
    // it is asked first (it is decidable from two strings, so an operator hears about
    // the fleet before being sent to fix a roster file). Answering it here is what makes
    // this case test the roster's authority rather than the gate's ordering — and it is
    // the strong form besides: even with stranding accepted in full, an unlisted key is
    // refused.
    let document = roster_doc(&[], &MASTER);
    assert!(
        !document_lists(&document, &pk(&STRANGER)),
        "precondition: the stranger really is absent from the roster"
    );
    let mut unlisted = armed(&masters, &keyset, &document);
    unlisted.pre_roster = publish::PreRosterClients::Stranded;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&STRANGER)), &unlisted)
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
    // Acknowledged for the same reason case (1) is: m11 is a NON-HEAD keyset member, so
    // the pre-roster obligation would otherwise answer first and this case would stop
    // testing revocation. With stranding accepted in full, the deny-list still holds.
    let mut revoked_ack = armed(&masters, &keyset, &revoked);
    revoked_ack.pre_roster = publish::PreRosterClients::Stranded;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M11)), &revoked_ack)
        .expect_err("a revoked machine may not cut");
    assert!(err.to_string().contains("may not sign"), "{err}");
    // The refusal is TARGETED: m3 still cuts under the same document.
    publish::channel_signature_policy(
        Some(&pk(&M3)),
        Some(&pk(&M3)),
        &armed(&masters, &keyset, &revoked),
    )
    .expect("revoking m11 must not revoke m3");

    // (3) A LAPSED ROSTER. Publishing under it would produce a release every client
    // refuses, so the cutter is strictly the better place to find out.
    let mut lapsed = armed(&masters, &keyset, &document);
    lapsed.now_unix = LONG_AFTER;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M3)), &lapsed)
        .expect_err("a lapsed roster may not authorize a cut");
    assert!(err.to_string().contains("not usable for a cut"), "{err}");

    // (3b) A roster that is still valid, but not for LONG ENOUGH. The producer checks
    // the window at a strictly earlier clock than every client does, so "valid now" is
    // the wrong question — a cut takes the better part of an hour and the fleet stages
    // over six. Refusing pre-claim is the only place this is free.
    let mut about_to_lapse = armed(&masters, &keyset, &document);
    about_to_lapse.now_unix = FIXTURE_VALID_UNTIL - 60;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M3)), &about_to_lapse)
        .expect_err("a roster with a minute left may not start a ~20 minute cut");
    assert!(err.to_string().contains("not usable for a cut"), "{err}");

    // (4) THE WRONG MASTER — a roster signed by a key that is not the pinned anchor.
    let foreign = roster_doc(&[], &OTHER_MASTER);
    assert_eq!(
        foreign.bytes, document.bytes,
        "precondition: only the SIGNATURE differs, so this tests the anchor and not the body"
    );
    let err = publish::channel_signature_policy(
        Some(&pk(&M3)),
        Some(&pk(&M3)),
        &armed(&masters, &keyset, &foreign),
    )
    .expect_err("a roster under another master authorizes nothing");
    assert!(err.to_string().contains("does not verify"), "{err}");

    // (5) A CORRUPTED SIGNATURE over the right body under the right master.
    let mut torn = roster_doc(&[], &MASTER);
    torn.signature[0] ^= 0xff;
    let err = publish::channel_signature_policy(
        Some(&pk(&M3)),
        Some(&pk(&M3)),
        &armed(&masters, &keyset, &torn),
    )
    .expect_err("a torn master signature authorizes nothing");
    assert!(err.to_string().contains("does not verify"), "{err}");

    // (6) NO ROSTER AT ALL. The armed anchor must never degrade to the single-key
    // path — this is the case that would silently re-open exactly what the tier closes.
    let mut absent = armed(&masters, &keyset, &document);
    absent.roster = None;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M3)), &absent)
        .expect_err("an armed anchor with no roster must refuse, never fall through");
    assert!(err.to_string().contains("machine_roster"), "{err}");

    // (7) A KEYLESS MACHINE. It could not sign anything anyway; it must be told so
    // pre-claim rather than at the moment of signing.
    let err = publish::channel_signature_policy(
        Some(&pk(&M3)),
        None,
        &armed(&masters, &keyset, &document),
    )
    .expect_err("a keyless machine may not cut for a rostered channel");
    assert!(err.to_string().contains("no signing material"), "{err}");
    assert!(
        err.to_string().contains("no ledger claim was made"),
        "{err}"
    );
}

/// THE OBLIGATION TO PRE-ROSTER CLIENTS, and the ONE thing that discharges it.
///
/// The roster authorizes m11 — that is settled, and no keyset can overrule it. What the
/// keyset still decides is whether a client running a build OLDER than the roster can
/// verify m11's release, and the answer is no: such a client verifies under its own
/// compiled-in keyset, has never heard of a roster, and `select_authoritative_release`
/// gives it exactly one candidate with no fallback. It would not miss this update, it
/// would never update again.
///
/// So the cutter REFUSES by default and takes the operator's assertion on the command
/// line. Three mutations die here:
///
///   * "keep requiring keyset membership" — the accepted case below is refused, and
///     adding a machine needs a shipped release again, which is the whole thing this
///     change removes;
///   * "just warn and proceed" — the refused case below succeeds, and a fleet gets
///     wedged by a cut whose only signal was a line in a transcript;
///   * "let the flag stand in for the roster" — the last case below succeeds, and an
///     unrostered key publishes.
#[test]
fn a_rostered_key_outside_the_keyset_needs_the_operator_to_accept_stranding_old_clients() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let document = roster_doc(&[], &MASTER);
    // m11 is on the roster; the shipped keyset carries only m3.
    assert!(
        document_lists(&document, &pk(&M11)),
        "precondition: m11 is rostered"
    );
    let keyset = [pk(&M3)];
    let keyset: Vec<&str> = keyset.iter().map(String::as_str).collect();

    // (1) UNACKNOWLEDGED — refused, and the refusal has to name the way out or it is
    //     just an obstacle.
    let err = publish::channel_signature_policy(
        Some(&pk(&M3)),
        Some(&pk(&M11)),
        &armed(&masters, &keyset, &document),
    )
    .expect_err("stranding the installed base is not a decision a program may take");
    let err = err.to_string();
    assert!(err.contains("UPDATE_CHANNEL_PUBKEYS"), "{err}");
    assert!(err.contains(publish::PRE_ROSTER_STRANDING_FLAG), "{err}");
    assert!(err.contains("never update again"), "{err}");
    // The fact, and its POSITION: it answers the operator's first worry ("what did I
    // just break?"), so it is hoisted into the headline rather than left in the tail
    // where a 189-word paragraph buried it.
    assert!(err.contains("No ledger claim was made"), "{err}");
    assert!(
        err.lines().next().is_some_and(|l| l.contains("No ledger claim was made")),
        "the reassuring fact belongs in the headline: {err}"
    );

    // (2) ACKNOWLEDGED — the same machine, the same roster, the same key, accepted with
    //     NOTHING added to any keyset. THIS is "adding a machine is a local act".
    let mut acknowledged = armed(&masters, &keyset, &document);
    acknowledged.pre_roster = publish::PreRosterClients::Stranded;
    let (policy, who) =
        publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M11)), &acknowledged)
            .expect("the roster authorizes m11; the operator accepted the cost");
    assert!(policy.required);
    assert_eq!(policy.pubkey.as_deref(), Some(pk(&M11).as_str()));
    assert_eq!(who.expect("attributed").machine_id, "m11");
    assert_eq!(
        keyset,
        vec![pk(&M3).as_str()],
        "and the keyset is untouched — the flag is an assertion, not an edit"
    );

    // (3) THE FLAG IS NOT AN AUTHORIZATION. A key the roster does not name is refused
    //     however loudly the operator accepts stranding anybody.
    let stranger = pk(&STRANGER);
    assert!(
        !document_lists(&document, &stranger),
        "precondition: the stranger is not rostered"
    );
    let err = publish::channel_signature_policy(Some(&pk(&M3)), Some(&stranger), &acknowledged)
        .expect_err("the roster is the authority; the flag only accepts a cost");
    assert!(err.to_string().contains("not on the machine roster"), "{err}");

    // (4) A REVOKED machine is likewise refused with the flag set — revocation is what
    //     the tier exists for, and no acknowledgement can spend it.
    let revoked = roster_doc(&["m11"], &MASTER);
    let mut with_revocation = armed(&masters, &keyset, &revoked);
    with_revocation.pre_roster = publish::PreRosterClients::Stranded;
    let err = publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M11)), &with_revocation)
        .expect_err("a revoked machine may not cut, acknowledged or not");
    assert!(err.to_string().contains("may not sign"), "{err}");
}

/// THE COMMITTED HEAD NEEDS NO FLAG, and never sees one. The ordinary case — cutting
/// from the incumbent, the machine every pre-roster client can already verify — is
/// unchanged and silent.
///
/// Kills the mutation "require the acknowledgement on every armed cut": the incumbent
/// would then need to assert something false about the fleet in order to serve it.
#[test]
fn the_incumbent_cuts_under_an_armed_master_with_no_acknowledgement_at_all() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let document = roster_doc(&[], &MASTER);
    let keyset = [pk(&M3), pk(&M11)];
    let keyset: Vec<&str> = keyset.iter().map(String::as_str).collect();
    assert_eq!(
        keyset[0],
        pk(&M3),
        "precondition: m3 IS the committed head, not merely a member"
    );
    let evidence = armed(&masters, &keyset, &document);
    assert_eq!(
        evidence.pre_roster,
        publish::PreRosterClients::Protected,
        "precondition: no acknowledgement is in play"
    );
    let (policy, who) =
        publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M3)), &evidence)
            .expect("the committed head owes the installed base nothing");
    assert!(policy.required);
    assert_eq!(who.expect("attributed").machine_id, "m3");
}

/// AN ACCEPT-ONLY KEYSET MEMBER IS NOT A SHIPPED KEY, and the gate must not confuse the
/// two. This is the case that made the first version of this gate a fleet-bricking bug.
///
/// `UPDATE_CHANNEL_PUBKEYS` in the working tree is what the NEXT build will carry, not
/// what the fielded ones do. Step 1 of the documented rotation APPENDS a key precisely
/// so that a future build can ship it — so at the moment of appending, a non-head member
/// is in the tree and in nobody's installed build. K2 (`aterm-update-v3`) is exactly
/// that today: added to `pins.rs` on 2026-08-12, present in no published tag.
///
/// A membership test therefore calls the most dangerous key in the file "safe", with no
/// flag, no warning and no transcript line, while every client in the field holds the
/// head alone and wedges on it permanently. Only equality with index 0 is a claim the
/// tree can actually support, because promotion TO index 0 is the reviewed commit in
/// which the operator asserts adoption.
///
/// Kills the mutation "test membership instead of head equality" — under it, case (1)
/// below is accepted silently.
#[test]
fn a_non_head_keyset_member_strands_pre_roster_clients_just_as_a_stranger_does() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let document = roster_doc(&[], &MASTER);
    // The shipped shape: the head every client holds, plus one accept-only member.
    let keyset = [pk(&M3), pk(&M11)];
    let keyset: Vec<&str> = keyset.iter().map(String::as_str).collect();
    assert_eq!(keyset[0], pk(&M3), "precondition: m3 is the head");
    assert_eq!(
        keyset[1],
        pk(&M11),
        "precondition: m11 is a MEMBER, and is not the head — the whole point"
    );
    assert!(
        document_lists(&document, &pk(&M11)),
        "precondition: the roster authorizes m11, so nothing here is about authorization"
    );

    // (1) UNACKNOWLEDGED — refused, and the refusal must explain the distinction rather
    //     than just assert it, because "but it IS in the keyset" is the obvious reply.
    let err = publish::channel_signature_policy(
        Some(&pk(&M3)),
        Some(&pk(&M11)),
        &armed(&masters, &keyset, &document),
    )
    .expect_err("a non-head member is in no shipped build; it may not silently strand one");
    let err = err.to_string();
    assert!(err.contains("UPDATE_CHANNEL_PUBKEYS[1]"), "{err}");
    assert!(err.contains("ACCEPT-ONLY"), "{err}");
    assert!(err.contains("SHIPPED"), "{err}");
    assert!(err.contains(publish::PRE_ROSTER_STRANDING_FLAG), "{err}");
    assert!(
        err.contains(pk(&M3).as_str()),
        "the remedy must name the head that WOULD have been safe: {err}"
    );
    // The fact, and its POSITION: it answers the operator's first worry ("what did I
    // just break?"), so it is hoisted into the headline rather than left in the tail
    // where a 189-word paragraph buried it.
    assert!(err.contains("No ledger claim was made"), "{err}");
    assert!(
        err.lines().next().is_some_and(|l| l.contains("No ledger claim was made")),
        "the reassuring fact belongs in the headline: {err}"
    );

    // (2) ACKNOWLEDGED — the operator may still do it, exactly as for a stranger. The
    //     flag is the only thing that changes the verdict, which is what makes the
    //     refusal an assertion rather than an obstacle.
    let mut acknowledged = armed(&masters, &keyset, &document);
    acknowledged.pre_roster = publish::PreRosterClients::Stranded;
    let (policy, who) =
        publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M11)), &acknowledged)
            .expect("the roster authorizes m11; the operator accepted the cost");
    assert_eq!(policy.pubkey.as_deref(), Some(pk(&M11).as_str()));
    assert_eq!(who.expect("attributed").machine_id, "m11");

    // (3) THE UNARMED PATH ALREADY REFUSED THIS, and always has — head equality, by
    //     name. Arming the master must not widen who may sign without saying so, and
    //     this is the comparison that proves the two paths now agree.
    let err = publish::committed_channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M11)))
        .expect_err("the single-key cutter has never allowed a non-head key to sign");
    assert!(err.to_string().contains("UPDATE_CHANNEL_PUBKEYS[0]"), "{err}");
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
    let keyset = [pk(&M3), pk(&M11)];
    let keyset: Vec<&str> = keyset.iter().map(String::as_str).collect();
    let mut evidence = armed(&masters, &keyset, &document);
    evidence.declared_machine_id = Some("m11");
    let err = publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M3)), &evidence)
        .expect_err("m3's key declared as m11 must refuse");
    assert!(err.to_string().contains("m11"), "{err}");
    assert!(err.to_string().contains("m3"), "{err}");
    // The truthful declaration passes, so the refusal above is about the MISMATCH and
    // not about declaring an id at all.
    evidence.declared_machine_id = Some("m3");
    publish::channel_signature_policy(Some(&pk(&M3)), Some(&pk(&M3)), &evidence)
        .expect("a truthful declaration is not an obstacle");
}

/// THE MISMATCH REMEDY MUST NOT POINT AT THE CLIFF — the staircase bug.
///
/// This is the exact shape of the SAFE path on the bootstrap machine, which is what
/// makes it worth a test of its own. `atpkg-keys setup` writes `~/.aterm/machine.toml`
/// naming THIS box ("m3"), and the first armed release must nevertheless go out under
/// the incumbent head's key, attributed to "incumbent-head". So the declared id and the
/// roster's answer disagree on precisely the path the documentation tells the operator
/// to take.
///
/// There are two ways out and they are not equivalent: set `machine_id`, or switch keys.
/// Switching keys means signing with m3's key, which is NOT the committed head, which
/// lands on the pre-roster refusal, whose way through is `--strand-pre-roster-clients` —
/// on a fleet that by construction still has pre-roster clients in it. Two fail-closed
/// refusals composing into a staircase down to a bricked installed base is still the
/// program leading the way there.
///
/// Kills the mutation "offer `or cut with the key that belongs to <declared>`
/// unconditionally".
#[test]
fn the_identity_mismatch_refusal_never_recommends_a_key_that_would_strand_the_fleet() {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    // The bootstrap shape: the roster names the incumbent head AND this machine, and the
    // committed keyset carries the head alone — nothing has shipped m3's key.
    let head = pk(&M3);
    let mine = pk(&M11);
    let document = roster_naming(&[("incumbent-head", &head), ("m3", &mine)], &[], &MASTER);
    let keyset = [head.clone()];
    let keyset: Vec<&str> = keyset.iter().map(String::as_str).collect();
    assert_eq!(keyset[0], head, "precondition: the incumbent IS the head");
    assert!(
        !keyset.contains(&mine.as_str()),
        "precondition: this machine's key is in no shipped build — the whole hazard"
    );

    // Cutting with the HEAD key on a box whose machine.toml says "m3".
    let mut evidence = armed(&masters, &keyset, &document);
    evidence.declared_machine_id = Some("m3");
    let err = publish::channel_signature_policy(Some(&head), Some(&head), &evidence)
        .expect_err("the declared id and the roster's answer disagree");
    let err = err.to_string();
    assert!(
        err.contains("set `machine_id` = \"incumbent-head\"")
            || err.contains("machine_id = \"incumbent-head\""),
        "the remedy must name the fix that is actually safe: {err}"
    );
    assert!(
        err.contains("Do NOT switch to \"m3\"'s key"),
        "the unsafe alternative must be named as the hazard it is: {err}"
    );
    assert!(
        !err.contains("or cut with the key that belongs to"),
        "the fleet-bricking alternative must not be offered at all: {err}"
    );

    // THE CONTROL, without which the assertions above could hold vacuously: when the
    // declared machine's key IS the committed head, switching to it strands nobody and
    // the alternative is offered in full.
    let mut safe = armed(&masters, &keyset, &document);
    safe.declared_machine_id = Some("incumbent-head");
    // Acknowledged, because signing with m3's key IS a stranding and that question is
    // asked first. Answering it is what lets this case reach the mismatch check at all.
    safe.pre_roster = publish::PreRosterClients::Stranded;
    let err = publish::channel_signature_policy(Some(&head), Some(&mine), &safe)
        .expect_err("cutting with m3's key while declaring incumbent-head must refuse");
    let err = err.to_string();
    assert!(
        err.contains("or cut with the key that belongs to \"incumbent-head\""),
        "a genuinely safe alternative must still be offered: {err}"
    );
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
    let staged = publish::stage_manifest(&dir, &inputs, Some(&who)).expect("stages");
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
    let unattributed = publish::stage_manifest(&dir, &inputs, None).expect("stages");
    let early_bytes = std::fs::read(&unattributed).expect("unattributed bytes");
    let early_signature = kp(&M3).sign(&early_bytes).as_ref().to_vec();
    let late = publish::stage_manifest(&dir, &inputs, Some(&who)).expect("re-stages, stamped");
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
    mirror::validate_mirror_asset_set(&names, "0.5.0", true, true).expect("the exact set");
    // ...and a mirror that forgets the roster is refused rather than published: the
    // armed client refuses such a head structurally, before any artifact crypto.
    let forgotten = mirror::required_asset_names("0.5.0", true, false);
    let err = mirror::validate_mirror_asset_set(&forgotten, "0.5.0", true, true)
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

/// THE LONG-FUSE TRAP the armed path would otherwise have walked into: the shipped
/// binary embeds the committed keyset HEAD in `__DATA,__aterm_upin`, and the build
/// proves the embedded value against `expected_embedded_update_pin`. Deriving that
/// expectation from the SIGNING key is correct only while signer == head, which is
/// precisely the invariant the roster relaxes — so a rostered non-head machine would
/// have cleared every pre-claim gate, burned a ledger number, and failed the Mach-O
/// pin proof fifteen minutes into the build.
///
/// Kills the mutation "expect the signing key's fingerprint": the first assertion
/// then reports m11's fingerprint for a tree pinned to m3.
#[test]
fn the_binary_pin_expectation_follows_the_committed_head_not_the_signer() {
    let head = pk(&M3);
    let member = pk(&M11);
    let by_head = publish::expected_embedded_update_pin(Some(&head), Some(&member))
        .expect("a pinned channel has an expectation")
        .expect("pinned means Some");
    let head_only = publish::expected_embedded_update_pin(Some(&head), Some(&head))
        .expect("valid")
        .expect("pinned means Some");
    assert_eq!(
        by_head, head_only,
        "the binary embeds the COMMITTED anchor, so the cutting machine cannot move it"
    );
    // Precondition, so the equality above is not vacuous: the two keys really differ.
    assert_ne!(head, member);
    assert_ne!(
        by_head,
        publish::expected_embedded_update_pin(None, Some(&member))
            .expect("valid")
            .expect("a signing key is an expectation of last resort")
    );
    // An UNPINNED fork keeps today's behaviour exactly: the signing key is the
    // expectation, and a machine with neither has nothing to prove.
    assert_eq!(
        publish::expected_embedded_update_pin(None, Some(&member)).unwrap(),
        publish::expected_embedded_update_pin(None, Some(&member)).unwrap()
    );
    assert_eq!(
        publish::expected_embedded_update_pin(None, None).unwrap(),
        None
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
    // The shipped case: nameless journal, nameless verdict. Every cut this tree makes.
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
    // fall-through to the single-key path — the same rule the profile itself follows.
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
/// Those lines are unreachable from this tree (the master is unpinned), which is
/// exactly why the anchors are PARAMETERS — the same reason `resolve_apple_tier` takes
/// its team id rather than reading `pins`.
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
    let keyset = [pubkey.as_str()];
    let verdict = publish::signing_verdict(&dir, Some(&creds), &anchors(&masters, &keyset, None))
        .expect("an armed cut");
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
    let err = publish::signing_verdict(
        &dir,
        Some(&creds),
        &anchors(&masters, &keyset, Some(&identity)),
    )
    .expect_err("a mint record naming another machine must refuse");
    assert!(err.to_string().contains("m11"), "{err}");
    // ...and a truthful one does not get in the way.
    std::fs::write(&identity, "id = \"m3\"\npubkey = \"unused\"\n").unwrap();
    publish::signing_verdict(
        &dir,
        Some(&creds),
        &anchors(&masters, &keyset, Some(&identity)),
    )
    .expect("a truthful mint record is not an obstacle");

    // A profile that names a roster which is not there fails HERE — pre-claim, with
    // the file named — and never degrades to the single-key path.
    let orphan = write_profile2(
        &dir,
        "orphan.toml",
        &format!(
            "signing_key = \"{pkcs8}\"\nmachine_roster = \"{}\"\n",
            dir.join("gone.toml").display()
        ),
    );
    let creds = sign::ReleaseCredentials::load(&orphan).expect("loads");
    let err = publish::signing_verdict(&dir, Some(&creds), &anchors(&masters, &keyset, None))
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
    (
        B64.encode(doc.as_ref()),
        B64.encode(kp.public_key().as_ref()),
    )
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
        B64.encode(doc.as_ref())
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
        // The shipped tier claims no team; this file invents no Apple identity.
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
