// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `takes_over` as P3's kernels answer it (`aterm_mesh_core::no_lost_edit::Takeover`): each answer
//! on real envelopes, and each clause of the island's replaced-pin premise
//! (`ReplacedOnlyAtTheStart`, no_lost_edit_island.rs section 12) on the real check.

use super::*;
use crate::envelope::{
    Link, Locker, Pin, Retired, Unsigned, endorse, sign, signer_public, test_armor,
};

const OLD: [u8; 32] = [7u8; 32];
const NEW: [u8; 32] = [8u8; 32];
const STRANGER: [u8; 32] = [9u8; 32];
const MID: [u8; 32] = [10u8; 32];
const LATER: [u8; 32] = [11u8; 32];

fn public(seed: &[u8; 32]) -> [u8; 32] {
    signer_public(seed).expect("public")
}

/// The link `from` signs to the key of `to`.
fn link(from: &[u8; 32], to: &[u8; 32]) -> Link {
    (
        public(from),
        endorse(from, "alab", &public(to)).expect("endorsed"),
    )
}

/// An ALab envelope of `seq`, signed by `seed`, carrying `handoff`, naming `parents`.
fn signed(seed: &[u8; 32], seq: u64, handoff: Vec<Link>, parents: Vec<[u8; 32]>) -> Vec<u8> {
    let unsigned = Unsigned {
        name: "alab".to_owned(),
        seq,
        time: 1_792_530_000 + seq,
        handoff,
        parents,
        lockers: vec![
            Locker {
                label: "admin".to_owned(),
                armored: test_armor(&[seq as u8; 40]),
            },
            Locker {
                label: "1".to_owned(),
                armored: test_armor(b"everyone"),
            },
        ],
    };
    sign(&unsigned, seed).expect("signed")
}

/// What a server holds once it took `bytes`, beside a vault already `parented` or not.
fn held(bytes: &[u8], parented: bool) -> Held {
    let envelope = envelope::parse(bytes).expect("parsed");
    Held {
        seq: envelope.seq,
        signer: envelope.signer,
        head: envelope.digest(),
        parented: parented || !envelope.parents.is_empty(),
        handoff: envelope.handoff.clone(),
        bytes: Arc::new(bytes.to_vec()),
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    envelope::parse(bytes).expect("parsed").digest()
}

/// What `takes_over` answers `bytes` against `held`: the pin it is taken under, `None` for the
/// version held, or the refusal's status and text.
fn judged(bytes: &[u8], held: &Held, retired: &Retired) -> Result<Option<Pin>, (u16, String)> {
    let envelope = envelope::parse(bytes).expect("parsed");
    takes_over(&envelope, bytes, held, retired).map_err(|reply| {
        let text = match reply.body {
            Body::Text(text) => text,
            _ => String::new(),
        };
        (reply.status, text)
    })
}

/// A build that retires `keys` and names `current`.
fn build(keys: &[[u8; 32]], current: &[u8; 32]) -> Retired {
    Retired {
        keys: keys.to_vec(),
        current: Some(*current),
    }
}

fn refused(answer: Result<Option<Pin>, (u16, String)>, status: u16, why: &str) {
    let Err((got, text)) = answer else {
        panic!("taken where {status} {why} was due: {answer:?}");
    };
    assert_eq!(got, status, "{text}");
    assert!(text.contains(why), "{text}");
}

/// Each answer of the kernels, on envelopes that reach it, with the status and words the server
/// always gave it.
#[test]
fn takes_over_answers_as_the_kernels_do() {
    let none = Retired::default();
    let first = signed(&OLD, 4, Vec::new(), vec![[1; 32]]);
    let at = held(&first, false);
    let next = signed(&OLD, 5, Vec::new(), vec![digest(&first)]);
    assert_eq!(judged(&next, &at, &none), Ok(Some(Pin::Same)));
    assert_eq!(judged(&first, &at, &none), Ok(None), "the version held");
    refused(
        judged(
            &signed(&STRANGER, 5, Vec::new(), vec![digest(&first)]),
            &at,
            &none,
        ),
        403,
        "endorsed no handoff",
    );
    refused(
        judged(
            &signed(&OLD, 4, Vec::new(), vec![digest(&first)]),
            &at,
            &none,
        ),
        409,
        "is not newer than seq 4",
    );
    refused(
        judged(&signed(&OLD, 5, Vec::new(), vec![[2; 32]]), &at, &none),
        409,
        "is not among the envelope's parents",
    );
    refused(
        judged(&signed(&OLD, 5, Vec::new(), Vec::new()), &at, &none),
        409,
        "is not among the envelope's parents",
    );
    // A vault that never named parents takes a newer version that names none.
    let bare = signed(&OLD, 4, Vec::new(), Vec::new());
    assert_eq!(
        judged(
            &signed(&OLD, 5, Vec::new(), Vec::new()),
            &held(&bare, false),
            &none
        ),
        Ok(Some(Pin::Same))
    );
    // A newer version that drops the held chain's link, though its pin holds.
    let rotated = signed(&NEW, 4, vec![link(&OLD, &NEW)], vec![[1; 32]]);
    let dropped = signed(&NEW, 5, Vec::new(), vec![digest(&rotated)]);
    refused(
        judged(&dropped, &held(&rotated, false), &none),
        403,
        "drops or changes handoff 1 of 1",
    );
    // The pin's refusal stands before the sequence's.
    refused(
        judged(&signed(&STRANGER, 3, Vec::new(), Vec::new()), &at, &none),
        403,
        "endorsed no handoff",
    );
}

/// The premise's ground: a replaced pin takes a version of the build's line with no sequence and
/// no parents check (`replaced_takes_unchecked`), over whatever the retired line pumped its
/// sequence to.
#[test]
fn a_replaced_pin_takes_a_version_whatever_its_sequence_and_parents() {
    let retiring = build(&[public(&OLD)], &public(&NEW));
    let pumped = held(&signed(&OLD, 1 << 40, Vec::new(), vec![[1; 32]]), true);
    let real = signed(&NEW, 3, vec![link(&OLD, &NEW)], Vec::new());
    assert_eq!(
        judged(&real, &pumped, &retiring),
        Ok(Some(Pin::Replaced(public(&NEW))))
    );
    // The same version under a build that retires nothing is judged by its sequence.
    refused(
        judged(&real, &pumped, &Retired::default()),
        409,
        "is not newer",
    );
}

/// The premise's clauses: once a server holds a version of its build's line it gives way to
/// nothing, so it judges by sequence again, past a later rotation too, wherever the build retires
/// every key of the chain before the one it names (what `alab_release` prints); and it takes no
/// version a key it retires signed.
#[test]
fn once_it_holds_the_builds_line_a_server_judges_by_sequence_again() {
    let retiring = build(&[public(&OLD)], &public(&NEW));
    let taken = signed(&NEW, 3, vec![link(&OLD, &NEW)], Vec::new());
    let at = held(&taken, true);
    refused(
        judged(
            &signed(&NEW, 2, vec![link(&OLD, &NEW)], Vec::new()),
            &at,
            &retiring,
        ),
        409,
        "is not newer than seq 3",
    );
    refused(
        judged(
            &signed(&NEW, 3, vec![link(&OLD, &NEW)], vec![[3; 32]]),
            &at,
            &retiring,
        ),
        409,
        "is not newer than seq 3",
    );
    let chain = vec![link(&OLD, &NEW), link(&NEW, &LATER)];
    let past = held(&signed(&LATER, 5, chain.clone(), vec![[4; 32]]), true);
    refused(
        judged(&signed(&LATER, 4, chain, vec![[4; 32]]), &past, &retiring),
        409,
        "is not newer than seq 5",
    );
    refused(
        judged(
            &signed(&OLD, 9, Vec::new(), vec![digest(&taken)]),
            &at,
            &retiring,
        ),
        403,
        "rotated away from",
    );
}

/// Why the premise names the release's rule: a build that retires a key of the chain but leaves
/// the key it endorsed unknown gives way for good once the vault rotates past the key the build
/// names, and then takes an older version of the line over a newer one. A build that retires the
/// whole chain before its key, as `alab_release` asks, judges the same version by its sequence.
#[test]
fn a_build_that_leaves_a_key_of_the_chain_unknown_gives_way_for_good() {
    let chain = vec![link(&OLD, &MID), link(&MID, &NEW), link(&NEW, &LATER)];
    let newer = held(&signed(&LATER, 5, chain.clone(), vec![[4; 32]]), true);
    let older = signed(&LATER, 4, chain, Vec::new());
    assert_eq!(
        judged(&older, &newer, &build(&[public(&OLD)], &public(&NEW))),
        Ok(Some(Pin::Replaced(public(&LATER)))),
        "a partial retired list leaves the pin giving way after the rotation past NEW"
    );
    refused(
        judged(
            &older,
            &newer,
            &build(&[public(&OLD), public(&MID)], &public(&NEW)),
        ),
        409,
        "is not newer than seq 5",
    );
}
