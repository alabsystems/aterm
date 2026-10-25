// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn trust() -> Trust {
    Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    }
}
fn terms() -> Terms {
    Terms {
        resource: [4; 32],
        window: [5; 32],
        unit: "requests".into(),
        limit: 10,
        shares: BTreeMap::from([([6; 48], 6), ([7; 48], 4)]),
    }
}

#[test]
fn a_proposal_requires_its_independent_exact_activation_pin_and_root() {
    let proposal = Proposal::prepare(trust(), terms()).unwrap();
    let allocation = Allocation::verify(proposal.bytes(), proposal.fingerprint(), trust()).unwrap();
    assert_eq!(allocation.terms(), &terms());
    assert_eq!(allocation.share(&[6; 48]), Some(6));
    assert_eq!(allocation.share(&[8; 48]), None);
    assert_eq!(allocation.id(), proposal.fingerprint());
    assert_eq!(allocation.bytes(), proposal.bytes());
    assert_eq!(allocation.trust(), trust());
    assert!(Allocation::verify(proposal.bytes(), [9; 32], trust()).is_err());
    for other in [
        Trust {
            network: [8; 16],
            ..trust()
        },
        Trust {
            authority: [8; 32],
            ..trust()
        },
        Trust {
            genesis: [8; 32],
            ..trust()
        },
    ] {
        assert!(Allocation::verify(proposal.bytes(), proposal.fingerprint(), other).is_err());
    }
    for end in 0..proposal.bytes().len() {
        assert!(Proposal::decode(&proposal.bytes()[..end]).is_err());
    }
    let mut changed = proposal.bytes().to_vec();
    changed[150] ^= 1;
    assert!(Allocation::verify(&changed, proposal.fingerprint(), trust()).is_err());
    changed = proposal.bytes().to_vec();
    changed.push(0);
    assert!(Proposal::decode(&changed).is_err());
}

#[test]
fn shares_are_canonical_disjoint_and_bounded_at_full_width() {
    let mut bad = terms();
    bad.shares.insert([8; 48], 1);
    assert!(Proposal::prepare(trust(), bad).is_err());
    let mut bad = terms();
    bad.limit = u64::MAX;
    bad.shares = BTreeMap::from([([6; 48], u64::MAX), ([7; 48], 1)]);
    assert!(Proposal::prepare(trust(), bad.clone()).is_err());
    bad.shares.remove(&[7; 48]);
    let full = Proposal::prepare(trust(), bad).unwrap();
    assert_eq!(
        Allocation::verify(full.bytes(), full.fingerprint(), trust())
            .unwrap()
            .share(&[6; 48]),
        Some(u64::MAX)
    );
    let mut wire = Proposal::prepare(trust(), terms())
        .unwrap()
        .bytes()
        .to_vec();
    let start = 5 + 80 + 64 + 8 + 1 + "requests".len() + 1;
    let first = wire[start..start + 56].to_vec();
    wire.copy_within(start + 56..start + 112, start);
    wire[start + 56..start + 112].copy_from_slice(&first);
    assert!(Allocation::verify(&wire, fingerprint(&wire), trust()).is_err());
    wire = Proposal::prepare(trust(), terms())
        .unwrap()
        .bytes()
        .to_vec();
    wire.copy_within(start..start + 56, start + 56);
    assert!(Proposal::decode(&wire).is_err());
    assert!(Proposal::decode(&vec![0; MAX_WIRE + 1]).is_err());
    assert!(Allocation::verify(&vec![0; MAX_WIRE + 1], [1; 32], trust()).is_err());
}

#[test]
fn empty_or_shared_root_writer_rights_cannot_be_prepared() {
    for field in 0..6 {
        let mut t = terms();
        match field {
            0 => t.shares.clear(),
            1 => t.limit = 0,
            2 => t.resource = [0; 32],
            3 => t.window = [0; 32],
            4 => t.unit = "Requests".into(),
            _ => {
                t.shares = BTreeMap::from([([2; 48], 1)]);
            }
        }
        assert!(Proposal::prepare(trust(), t).is_err());
    }
    let mut t = terms();
    let mut actor = [6; 48];
    actor[32..].fill(0);
    t.shares = BTreeMap::from([(actor, 1)]);
    assert!(Proposal::prepare(trust(), t).is_err());
}
