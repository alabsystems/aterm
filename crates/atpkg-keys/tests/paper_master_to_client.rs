// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ROUND TRIP: a written master phrase → a machine key → a roster → a release the
//! **actual client verifier** accepts, attributed to the machine that signed it.
//!
//! Every step here uses the shipping types on both sides — `atpkg_keys::master` and
//! `atpkg_keys::roster_ops` on the owner side, `aterm_update_core::roster` on the client
//! side. Nothing is re-implemented for the test, so a change that breaks the contract
//! breaks this file rather than passing a parallel implementation of itself.
//!
//! The CLI's `/dev/tty` prompt is deliberately NOT exercised here: a test harness has no
//! controlling terminal, and a prompt that could be satisfied without one would defeat its
//! own purpose (leak vector 5 — `join < phrase.txt` must fail). What the prompt
//! feeds into — `parse_master` — is tested exhaustively in `master.rs`.

#![cfg(unix)]

use aterm_update_core::roster::{Roster, RosterReject, verify_roster};
use atpkg_keys::master::parse_master;
use atpkg_keys::roster_ops::{add, empty, revoke};

/// An obviously synthetic master. Sixty-four characters of a visible repeating pattern —
/// it could not be mistaken for a generated key, and it appears nowhere outside tests.
const PAPER: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

/// A different obviously synthetic master, for the "wrong paper" case.
const OTHER_PAPER: &str = "zyxwvtsrqpnmkjhgfedcba9876543210zyxwvtsrqpnmkjhgfed0";

/// 2026-08-04T00:00:00Z.
const NOW: u64 = 1_785_801_600;

/// The bytes of a release manifest as the cutter would emit them, carrying the two
/// attribution keys. Signed as raw bytes, exactly as the client verifies them.
fn appcast(machine_id: &str, roster_seq: u64) -> Vec<u8> {
    let mut s = String::from("schema = 1\nversion = \"0.99.0\"\nbuild_number = 990\n");
    s.push_str("dmg = \"aterm-0.99.0.dmg\"\nsha256 = \"");
    s.push_str(&"ab".repeat(32));
    s.push_str("\"\nmachine_id = \"");
    s.push_str(machine_id);
    s.push_str("\"\nroster_seq = ");
    s.push_str(&roster_seq.to_string());
    s.push('\n');
    s.into_bytes()
}

/// Publish a roster the way the tool does — emit, master-sign — and take it back through
/// the client's own verify + parse. Returns the parsed roster and the master's pubkey.
fn publish(roster: &Roster, paper: &str) -> (Vec<u8>, Vec<u8>, String) {
    let seed = parse_master(paper).expect("synthetic phrase").seed();
    let bytes = roster.to_toml().expect("a valid roster emits").into_bytes();
    let sig = seed.sign(&bytes).expect("the master signs");
    (bytes, sig, seed.pubkey_b64().expect("public identity"))
}

/// THE WHOLE CHAIN, end to end: paper master → machine key → roster → a release the
/// client accepts and attributes correctly.
#[test]
fn a_master_phrase_mints_a_machine_whose_release_the_client_accepts() {
    // Owner side: mint m3's key on m3, and put it on a roster signed by the paper master.
    let (m3_key, m3_pub) = atpkg_keys::generate().expect("machine keypair");
    let roster = add(empty(NOW), "m3", &m3_pub, NOW).expect("m3 joins");
    let (roster_bytes, roster_sig, master_pub) = publish(&roster, PAPER);

    // The machine signs a release with its OWN key. The master is not present for this —
    // that is what makes "touch the paper only to mint" true.
    let bytes = appcast("m3", roster.roster_seq);
    let sig = atpkg_keys::sign(&m3_key, &bytes).expect("the machine signs its release");

    // Client side: the pinned master verifies the roster, the roster authorizes m3, and
    // m3's signature over the appcast is accepted.
    let verified =
        verify_roster(&[&master_pub], roster_bytes, &roster_sig).expect("pinned master verifies");
    let parsed = Roster::parse(&verified).expect("the roster parses");
    parsed
        .admit(0, NOW as i64)
        .expect("fresh, and above a first-contact floor");
    let who = parsed
        .authorize_appcast(&bytes, &sig, NOW as i64)
        .expect("m3 is live and signed this");

    // ATTRIBUTION: the verifier can say WHICH machine signed.
    assert_eq!(who.machine_id, "m3");
    assert_eq!(who.pubkey_b64, m3_pub);
    assert_eq!(who.roster_seq, parsed.roster_seq);
    who.bind(Some("m3"), Some(parsed.roster_seq))
        .expect("the manifest's own claim agrees with the key that signed");
}

/// THE WRONG PAPER. A roster signed by a different master is refused under the pinned one
/// — the transcription check `join` performs, in its essential form.
#[test]
fn a_roster_signed_by_a_different_master_is_refused() {
    let (_, m3_pub) = atpkg_keys::generate().unwrap();
    let roster = add(empty(NOW), "m3", &m3_pub, NOW).unwrap();
    let (bytes, sig, _) = publish(&roster, OTHER_PAPER);
    let (_, _, real_master) = publish(&roster, PAPER);
    assert_eq!(
        verify_roster(&[&real_master], bytes, &sig),
        Err(RosterReject::Verify),
        "a roster signed by the wrong master must never verify under the pinned one"
    );
}

/// THE WRONG MACHINE KEY. A key that is not on the roster cannot publish, even though the
/// roster itself is perfectly genuine and the signature is mathematically valid.
#[test]
fn a_release_signed_by_a_key_not_on_the_roster_is_refused() {
    let (_, m3_pub) = atpkg_keys::generate().unwrap();
    let (thief_key, thief_pub) = atpkg_keys::generate().unwrap();
    assert_ne!(m3_pub, thief_pub, "the fixture must actually differ");

    let roster = add(empty(NOW), "m3", &m3_pub, NOW).unwrap();
    let (rb, rs, master) = publish(&roster, PAPER);
    let parsed = Roster::parse(&verify_roster(&[&master], rb, &rs).unwrap()).unwrap();

    let bytes = appcast("m3", parsed.roster_seq);
    let forged = atpkg_keys::sign(&thief_key, &bytes).unwrap();
    assert_eq!(
        parsed.authorize_appcast(&bytes, &forged, NOW as i64),
        Err(RosterReject::Verify)
    );
}

/// MISMATCHED IDENTITY, both directions. A genuine m11 signature cannot be relabelled as
/// m3 (the id is inside the signed bytes), and a manifest claiming m3 while signed by m11
/// is refused at the bind.
#[test]
fn a_machine_cannot_sign_under_another_machines_identity() {
    let (m3_key, m3_pub) = atpkg_keys::generate().unwrap();
    let (m11_key, m11_pub) = atpkg_keys::generate().unwrap();
    let roster = add(empty(NOW), "m3", &m3_pub, NOW).unwrap();
    let roster = add(roster, "m11", &m11_pub, NOW).unwrap();
    let (rb, rs, master) = publish(&roster, PAPER);
    let parsed = Roster::parse(&verify_roster(&[&master], rb, &rs).unwrap()).unwrap();
    let seq = parsed.roster_seq;

    // m11 signs bytes that CLAIM to be m3's. The signature verifies (it is m11's own key)
    // but the claim is refused, because attribution follows the key.
    let lying = appcast("m3", seq);
    let sig = atpkg_keys::sign(&m11_key, &lying).unwrap();
    let who = parsed.authorize_appcast(&lying, &sig, NOW as i64).unwrap();
    assert_eq!(who.machine_id, "m11", "the key decides, not the label");
    assert_eq!(
        who.bind(Some("m3"), Some(seq)),
        Err(RosterReject::UnknownMachine)
    );

    // The other direction: m3's genuine release cannot have its label rewritten to m11,
    // because rewriting it changes the bytes the signature covers.
    let honest = appcast("m3", seq);
    let m3_sig = atpkg_keys::sign(&m3_key, &honest).unwrap();
    let relabelled = appcast("m11", seq);
    assert_eq!(
        parsed.authorize_appcast(&relabelled, &m3_sig, NOW as i64),
        Err(RosterReject::Verify)
    );
    // Negative control: unmodified, it is accepted and attributed to m3.
    assert_eq!(
        parsed
            .authorize_appcast(&honest, &m3_sig, NOW as i64)
            .unwrap()
            .machine_id,
        "m3"
    );
}

/// THE STOLEN LAPTOP, played out. m11 is taken; the owner revokes it from a surviving
/// machine with the paper master. The thief still holds a perfectly good key and a copy of
/// the OLD roster, whose master signature is valid forever.
#[test]
fn a_revoked_machine_is_refused_and_its_old_roster_cannot_be_replayed() {
    let (m3_key, m3_pub) = atpkg_keys::generate().unwrap();
    let (m11_key, m11_pub) = atpkg_keys::generate().unwrap();
    let before = add(empty(NOW), "m3", &m3_pub, NOW).unwrap();
    let before = add(before, "m11", &m11_pub, NOW).unwrap();
    let (old_bytes, old_sig, master) = publish(&before, PAPER);

    // The owner revokes m11. Only the paper master can do this; the thief's machine key
    // signs artifacts, not rosters.
    let after = revoke(before.clone(), "m11", NOW).unwrap();
    let (new_bytes, new_sig, _) = publish(&after, PAPER);
    assert!(after.roster_seq > before.roster_seq, "the counter advanced");

    let current = Roster::parse(&verify_roster(&[&master], new_bytes, &new_sig).unwrap()).unwrap();
    let bytes = appcast("m11", current.roster_seq);
    let thief_sig = atpkg_keys::sign(&m11_key, &bytes).unwrap();
    assert_eq!(
        current.authorize_appcast(&bytes, &thief_sig, NOW as i64),
        Err(RosterReject::Verify),
        "a revoked machine is not in the candidate set, so its valid signature is never \
         even checked"
    );
    // m3 keeps working: revocation is targeted.
    let m3_bytes = appcast("m3", current.roster_seq);
    assert!(
        current
            .authorize_appcast(
                &m3_bytes,
                &atpkg_keys::sign(&m3_key, &m3_bytes).unwrap(),
                NOW as i64
            )
            .is_ok()
    );

    // REPLAY. The thief serves the OLD roster — still master-signed, still cryptographically
    // perfect, still listing m11. It verifies...
    let replayed = verify_roster(&[&master], old_bytes, &old_sig).expect(
        "an old master signature never stops being valid; documents expire, signatures do not",
    );
    let old = Roster::parse(&replayed).unwrap();
    assert!(old.machines.iter().any(|m| m.id == "m11"));
    // ...and is refused by the durable floor of any client that has seen the new one.
    assert_eq!(
        old.admit(current.roster_seq, NOW as i64),
        Err(RosterReject::Rollback)
    );
    // The residual, asserted rather than glossed — and it is UNBOUNDED, by the owner's
    // decision: a client with NO floor (a fresh install) accepts the old roster, and
    // rosters now carry a forever `valid_until`, so no calendar ever closes that window.
    // Revocation reaches every RUNNING client in minutes; against a fresh install the
    // only remedy for a stolen key is a full re-key. This test pins that trade so it
    // stays a decision and never becomes a surprise.
    assert_eq!(old.admit(0, NOW as i64), Ok(()));
    let years_later = NOW as i64 + 20 * 365 * 86_400;
    assert_eq!(
        old.admit(0, years_later),
        Ok(()),
        "keys last forever: the replay window against floor-less clients never lapses"
    );
}

/// AN EMPTY MASTER ANCHOR IS INERT: it refuses a genuine, correctly signed roster rather
/// than waving it through. There is no configuration in which this tier accepts anything.
#[test]
fn an_unpinned_master_accepts_nothing_at_all() {
    let (_, m3_pub) = atpkg_keys::generate().unwrap();
    let roster = add(empty(NOW), "m3", &m3_pub, NOW).unwrap();
    let (bytes, sig, master) = publish(&roster, PAPER);
    // Genuine under its own master...
    assert!(verify_roster(&[&master], bytes.clone(), &sig).is_ok());
    // ...and refused with nothing pinned.
    assert_eq!(
        verify_roster(&[], bytes, &sig),
        Err(RosterReject::Disabled),
        "unpinned means inert, never permissive"
    );
}

/// THE SAME ROUND TRIP, BUT NOBODY TYPES A KEY — driven end to end by `setup` and `join`.
///
/// The chain above proves the cryptography. This proves the OPERATION: the two verbs
/// produce, with no hand transcription anywhere, a `pins.rs` and a roster that the real
/// client verifier accepts, and a machine key that signs a release attributed to the
/// machine that holds it.
///
/// **AND IT PROVES THE POINT OF THE WHOLE TIER: `m11` is ROSTER-ONLY.** Its key is in no
/// keyset, in this anchor file or any shipped one, and it never will be — neither verb
/// touches `UPDATE_CHANNEL_PUBKEYS`. Under an armed master that is enough, because the
/// roster alone authorizes (`aterm_update::github::fetch_authoritative_release`), which is
/// what makes adding a machine a LOCAL act: run `join`, copy the roster out, publish.
///
/// Two mutations die here:
///
/// * "put the machine key back in the keyset as a bridge" — the keyset assertion fails.
///   The entry would be an irrevocable grant to every client that shipped with it, and it
///   authorizes nothing the roster does not already.
/// * "make the client require keyset membership too" — the roster-only machine below stops
///   being able to publish, and the owner is back to needing a release from a machine that
///   can already sign, which is the ceremony this replaced.
#[test]
fn setup_then_join_produce_an_anchor_and_a_roster_the_client_accepts() {
    use atpkg_keys::pins_edit::{CHANNEL_ANCHOR, MASTER_ANCHOR, read_anchor};
    use atpkg_keys::provision::{
        Paths, Verb, plan, preflight, verify_master, write_pins, write_rest,
    };

    // The incumbent head — the key whose private half is on another machine and which
    // signed the live release. Its survival is the property that keeps the fleet alive.
    const HEAD_KEY: &str = "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=";

    let dir = std::env::temp_dir().join("atpkg-keys-e2e/provisioned");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch tree");
    let at = |n: &str| dir.join(n).to_str().expect("utf-8 path").to_string();

    // An unarmed anchor file in the two shapes the real one uses.
    std::fs::write(
        at("pins.rs"),
        "// Copyright 2026 Andrew Yates\n\
         // SPDX-License-Identifier: Apache-2.0\n\
         \n\
         /// The paper master. Empty means INERT.\n\
         pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n\
         \n\
         /// ORDER IS A CONTRACT: index 0 is the head.\n\
         pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[\n\
         \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
         ];\n",
    )
    .expect("unarmed fixture");

    let paths = |key: &str, rec: &str| Paths {
        pins: at("pins.rs"),
        roster: at("aterm-machines.toml"),
        key: at(key),
        machine_pub: at(rec),
        pins_explicit: true,
    };

    // --- setup, on the first machine ------------------------------------------------
    // The seed stands in for `generate_master()` + the paper the owner writes; everything
    // downstream of it is exactly what the verb does.
    let seed = parse_master(PAPER).expect("synthetic phrase").seed();
    let m3_paths = paths("m3.key", "m3.toml");
    let pre = preflight(Verb::Setup, "m3", "incumbent-head", &m3_paths)
        .expect("a fresh tree accepts setup");
    let planned = plan(pre, &seed, NOW).expect("setup plans");
    write_pins(&planned).expect("the anchor is written and verified");
    let m3 = write_rest(planned).expect("setup completes");

    // --- join, on a second machine, against the anchor setup committed ---------------
    let m11_paths = paths("m11.key", "m11.toml");
    let pre = preflight(Verb::Join, "m11", "incumbent-head", &m11_paths)
        .expect("an armed tree accepts join");
    verify_master(&pre, &seed).expect("the phrase proves against the committed anchor");
    let planned = plan(pre, &seed, NOW).expect("join plans");
    write_pins(&planned).expect("the keyset entry is written and verified");
    let m11 = write_rest(planned).expect("join completes");

    // --- what the anchor file now says ----------------------------------------------
    let src = std::fs::read_to_string(at("pins.rs")).expect("the anchor file");
    let master_anchor = read_anchor(&src, MASTER_ANCHOR).unwrap().members;
    let keyset = read_anchor(&src, CHANNEL_ANCHOR).unwrap().members;
    assert_eq!(
        master_anchor,
        vec![seed.pubkey_b64().unwrap()],
        "the master anchor names the paper master, written by the tool"
    );
    assert_eq!(
        keyset,
        vec![HEAD_KEY.to_string()],
        "UNTOUCHED: neither verb grants a machine the pre-roster allowance — a keyset \
         member cannot be un-shipped, so `machine-revoke` could never take it back"
    );
    assert!(
        !src.contains(&m3.machine_pubkey) && !src.contains(&m11.machine_pubkey),
        "no minted key appears anywhere in the anchor file"
    );

    // --- the machine that the compiled-in keyset has never heard of -------------------
    let bytes = appcast("m11", m11.roster_seq);
    let m11_key = std::fs::read(at("m11.key")).expect("the 0600 machine key");
    let sig = atpkg_keys::sign(&m11_key, &bytes).expect("the machine signs its own release");
    assert!(
        !keyset.contains(&m11.machine_pubkey),
        "precondition: m11 is ROSTER-ONLY, or the acceptance below proves nothing"
    );

    // --- the client's ONLY gate under an armed master: the master-signed roster -------
    let roster_bytes = std::fs::read(at("aterm-machines.toml")).expect("the roster");
    let roster_sig = std::fs::read(at("aterm-machines.toml.sig")).expect("its signature");
    let verified = verify_roster(&[master_anchor[0].as_str()], roster_bytes, &roster_sig)
        .expect("the roster verifies under the anchor the tool wrote");
    let parsed = Roster::parse(&verified).expect("it parses");
    parsed
        .admit(0, NOW as i64)
        .expect("fresh, above a first-contact floor");

    let who = parsed
        .authorize_appcast(&bytes, &sig, NOW as i64)
        .expect("m11 is live and signed this");
    assert_eq!(who.machine_id, "m11", "attribution follows the key");
    assert_eq!(who.pubkey_b64, m11.machine_pubkey);
    who.bind(Some("m11"), Some(parsed.roster_seq))
        .expect("the manifest's claim agrees with the key that signed");

    // m3, minted by the OTHER verb, is equally live on the same roster.
    let m3_bytes = appcast("m3", m11.roster_seq);
    let m3_key = std::fs::read(at("m3.key")).unwrap();
    let m3_sig = atpkg_keys::sign(&m3_key, &m3_bytes).unwrap();
    assert_eq!(
        parsed
            .authorize_appcast(&m3_bytes, &m3_sig, NOW as i64)
            .unwrap()
            .machine_id,
        "m3"
    );

    // NEGATIVE CONTROL: a key that neither verb minted is refused, so the acceptance
    // above is about the roster and not about the verifier waving anything through.
    let (thief_key, _) = atpkg_keys::generate().unwrap();
    let forged = atpkg_keys::sign(&thief_key, &bytes).unwrap();
    assert_eq!(
        parsed.authorize_appcast(&bytes, &forged, NOW as i64),
        Err(RosterReject::Verify)
    );

    // --- AND THE HALF THAT IS NOT FREE ------------------------------------------------
    // A client that predates the roster does not run any of the above. Its whole rule is
    // membership of the keyset it was COMPILED with, it has no fallback to an older
    // release, and no document can teach it a key. So the same m11 release that every
    // roster-aware client accepts is one such a client can never install — which is why
    // `cargo ship cut` refuses to sign under this key unless the operator asserts that
    // none are left (`--strand-pre-roster-clients`; proved in aterm-release's
    // tests/machine_roster.rs). Asserted here, at the seam, so the cost of the acceptance
    // above is recorded beside it rather than only in prose.
    for shipped in aterm_update_core::pins::UPDATE_CHANNEL_PUBKEYS {
        assert_ne!(
            *shipped, m11.machine_pubkey,
            "a roster-only machine is by definition outside every shipped keyset"
        );
    }
    assert!(
        keyset.contains(&HEAD_KEY.to_string()),
        "and the incumbent — the one machine those clients CAN verify — is still there, \
         which is why the first roster names it"
    );
}

// (The unset-anchor tripwire that stood here was deleted 2026-08-15 as part of the
// arming commit, exactly as its own doc prescribed. The tier is ARMED: pins.rs names
// the paper master minted by `atpkg-keys setup --id m3`.)
