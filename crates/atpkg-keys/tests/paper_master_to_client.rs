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
//!
//! The refusals (wrong master, key off the roster, relabelled identity, revocation,
//! replay, an unpinned anchor) are proved over the client's own bytes in
//! `aterm_update_core::roster`'s tests and through a real install in
//! `owner_to_client.rs`; this file keeps the two acceptance paths.

#![cfg(unix)]

use aterm_update_core::roster::{Roster, RosterReject, verify_roster};
use atpkg_keys::master::parse_master;
use atpkg_keys::roster_ops::{add, empty};

/// An obviously synthetic master. Sixty-four characters of a visible repeating pattern —
/// it could not be mistaken for a generated key, and it appears nowhere outside tests.
const PAPER: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

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

/// THE SAME ROUND TRIP, BUT NOBODY TYPES A KEY — driven end to end by `setup` and `join`.
///
/// The chain above proves the cryptography. This proves the OPERATION: the two verbs
/// produce, with no hand transcription anywhere, a `pins.rs` and a roster that the real
/// client verifier accepts, and a machine key that signs a release attributed to the
/// machine that holds it.
///
/// **AND IT PROVES THE POINT OF THE WHOLE TIER: a machine is ROSTER-ONLY.** No minted key
/// appears in the anchor file; the roster alone authorizes
/// (`aterm_update::github::fetch_authoritative_release`), which is what makes adding a
/// machine a LOCAL act: run `join`, copy the roster out, publish.
#[test]
fn setup_then_join_produce_an_anchor_and_a_roster_the_client_accepts() {
    use atpkg_keys::pins_edit::{MASTER_ANCHOR, read_anchor};
    use atpkg_keys::provision::{
        Paths, Verb, plan, preflight, verify_master, write_pins, write_rest,
    };

    let dir = std::env::temp_dir().join("atpkg-keys-e2e/provisioned");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch tree");
    let at = |n: &str| dir.join(n).to_str().expect("utf-8 path").to_string();

    // An unarmed anchor file.
    std::fs::write(
        at("pins.rs"),
        "// Copyright 2026 Andrew Yates\n\
         // SPDX-License-Identifier: Apache-2.0\n\
         \n\
         /// The paper master. Empty means INERT.\n\
         pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n",
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
    let pre = preflight(Verb::Setup, "m3", &m3_paths).expect("a fresh tree accepts setup");
    let planned = plan(pre, &seed, NOW).expect("setup plans");
    write_pins(&planned).expect("the anchor is written and verified");
    let m3 = write_rest(planned).expect("setup completes");

    // --- join, on a second machine, against the anchor setup committed ---------------
    let m11_paths = paths("m11.key", "m11.toml");
    let pre = preflight(Verb::Join, "m11", &m11_paths).expect("an armed tree accepts join");
    verify_master(&pre, &seed).expect("the phrase proves against the committed anchor");
    let planned = plan(pre, &seed, NOW).expect("join plans");
    write_pins(&planned).expect("join leaves the anchor exactly as committed");
    let m11 = write_rest(planned).expect("join completes");

    // --- what the anchor file now says ----------------------------------------------
    let src = std::fs::read_to_string(at("pins.rs")).expect("the anchor file");
    let master_anchor = read_anchor(&src, MASTER_ANCHOR).unwrap().members;
    assert_eq!(
        master_anchor,
        vec![seed.pubkey_b64().unwrap()],
        "the master anchor names the paper master, written by the tool"
    );
    assert!(
        !src.contains(&m3.machine_pubkey) && !src.contains(&m11.machine_pubkey),
        "no minted key appears anywhere in the anchor file"
    );

    // --- a machine known only to the roster ---------------------------------------
    let bytes = appcast("m11", m11.roster_seq);
    let m11_key = std::fs::read(at("m11.key")).expect("the 0600 machine key");
    let sig = atpkg_keys::sign(&m11_key, &bytes).expect("the machine signs its own release");

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
}

// (The unset-anchor tripwire that stood here was deleted 2026-08-15 as part of the
// arming commit, exactly as its own doc prescribed. The tier is ARMED: pins.rs names
// the paper master minted by `atpkg-keys setup --id m3`.)
