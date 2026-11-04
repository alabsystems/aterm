// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::{SCOPE_LEN, tests::Lab};
use aterm_codec::hex;
use aterm_secrets::commons::{journal::Trust, roster::Role};

/// A certificate as `Conflict::from_pair` wrote it at aterm `b4d6d6dc05` (astream `d5830b2`):
/// one writer's two table operations, each sealed as sequence 1 of its stream with no
/// predecessor, framed `ACTF1` in the order `Fork::between` names. The file is the scope it was
/// written in, laid out as `Scope::bytes` lays it (the network, the control authority's public
/// key and the genesis digest its session's trust held, the table and the audience generation),
/// then the certificate. Nothing in it is private. It stands for one stored in a person's
/// `table.astream`, so it is never regenerated: a rule or a format that moves reads it first.
const FROZEN: &[u8] = include_bytes!("../../../tests/fixtures/conflict-certificate.actf1");
/// Its two sources' record ids, the smaller first, and the actor both name, as the writing pin
/// computed them.
const FIRST: &str = "7ff9457f54dc38a90d78f33bd121b64652ddbd7d7a0bdb09a6b8d076d1e10711";
const SECOND: &str = "d3783220ac74b1442429aa4e0c35b920da25892875b15cce03c17d68e5a92604";
const ACTOR: &str = "e373d59de4b4483120f06f4111f5c4810d1e6d10418bd72f7057d9fe40a18716\
                     b3eb07f33b5938b25303cf54351ec2f1";

fn frozen() -> (Scope, &'static [u8]) {
    let (scope, wire) = FROZEN.split_at(SCOPE_LEN);
    let trust = Trust {
        network: scope[..16].try_into().unwrap(),
        authority: scope[16..48].try_into().unwrap(),
        genesis: scope[48..80].try_into().unwrap(),
    };
    let generation = u64::from_le_bytes(scope[96..].try_into().unwrap());
    let scope = Scope::new(trust, scope[80..96].try_into().unwrap(), generation).unwrap();
    (scope, wire)
}

/// Every other conflict test seals its certificate and judges it through the one pin, so a
/// change that writes and reads alike stays green in all of them and leaves a person's
/// `table.astream` that no longer opens: the signing `DOMAIN`, the `ACTF1` framing, the `ACTO1`
/// operation bytes or the `Scope` bytes they carry, or a record id redefined together with the
/// sort `Conflict::from_pair` writes its sources in. A move of `Fork::between`'s order is not
/// one: `from_pair` writes the source with the smaller SHA-256, its record id, first, so every
/// certificate those tests write is then refused. This one reads a certificate an earlier pin
/// wrote through `Claim::read`, all `Conflict::restore` asks of the bytes, and holds both source
/// headers to what that pin computed. `Conflict::restore` then takes it past every byte rule to
/// the session's first question, which this lab, not the one that wrote it, refuses.
/// CONTROL: the same certificate with its first and second sources swapped is refused by
/// `Conflict::restore` as not canonically ordered, before any session question.
#[test]
fn a_conflict_certificate_an_earlier_pin_wrote_replays_at_this_pin() {
    let (scope, wire) = frozen();
    let (claim, sources) = Claim::read(scope, wire).unwrap();
    let Claim::Fork(headers) = claim else {
        panic!("an ACTF1 certificate reads as a fork");
    };
    for (header, id) in headers.iter().zip([FIRST, SECOND]) {
        assert_eq!(hex::encode_lower(&header.id), id);
        assert_eq!(hex::encode_lower(&header.actor), ACTOR);
        assert_eq!(header.sequence, 1);
        assert_eq!(header.previous, None);
        assert!(header.parents.is_empty());
    }
    assert_eq!(sources[0].start, 9);
    assert_eq!(sources[1].end, wire.len());

    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let held = Conflict::restore(&session, scope, wire).err().unwrap();
    assert!(held.0.contains("another trust context"), "{held:?}");

    let mut swapped = b"ACTF1".to_vec();
    for source in [&wire[sources[1].clone()], &wire[sources[0].clone()]] {
        swapped.extend_from_slice(&(source.len() as u32).to_le_bytes());
        swapped.extend_from_slice(source);
    }
    let refused = Conflict::restore(&session, scope, &swapped).err().unwrap();
    assert!(refused.0.contains("not canonically ordered"), "{refused:?}");
}
