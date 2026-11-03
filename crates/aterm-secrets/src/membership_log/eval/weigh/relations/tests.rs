// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn invalid_candidate_layouts_are_rejected_before_storage() {
    assert!(Relations::new(&[2, 1]).is_err());
    assert!(Relations::new(&[1, 1]).is_err());
    assert!(Relations::new(&[usize::MAX]).is_err());
    let too_many: Vec<_> = (0..=MAX_CANDIDATES).collect();
    assert!(Relations::new(&too_many).is_err());
    let empty = Relations::new(&[]).unwrap();
    assert_eq!(empty.payload_bytes(), 0);
    assert_eq!(empty.metadata_bytes(), 0);
    assert_eq!(empty.allocated_rows(), 0);
    assert!(!empty.undoes(0, 0));
    assert!(empty.undoes_from(0).next().is_none());
}

#[test]
fn sparse_original_ids_and_independent_planes_round_trip() {
    let ids = [1, 70, 1023, 2046];
    let mut relations = Relations::new(&ids).unwrap();
    assert!(relations.set_undoes(1, 2046));
    assert!(!relations.set_undoes(1, 2046));
    assert!(relations.set_undoes(1, 1));
    assert!(relations.set_strips(2046, 1));
    assert_eq!(relations.active(), ids);
    assert_eq!(relations.undoes_from(1).collect::<Vec<_>>(), vec![1, 2046]);
    assert_eq!(relations.strips_from(2046).collect::<Vec<_>>(), vec![1]);
    assert_eq!(
        relations.undoes_pairs().collect::<Vec<_>>(),
        vec![(1, 1), (1, 2046)]
    );
    assert_eq!(
        relations.strips_pairs().collect::<Vec<_>>(),
        vec![(2046, 1)]
    );
    assert!(!relations.undoes(2046, 1));
    assert!(!relations.strips(1, 2046));
    assert!(!relations.undoes(0, 1));
    assert!(relations.undoes_from(0).next().is_none());
    assert!(!relations.defeats(1, 1));
    assert_eq!(relations.payload_bytes(), 2 * 8);
    assert_eq!(relations.allocated_rows(), 2);
    assert_eq!(
        relations.metadata_bytes(),
        (ids.len() + 2047) * std::mem::size_of::<usize>()
            + 2 * ids.len() * std::mem::size_of::<Option<Box<[u64]>>>()
    );
}

#[test]
fn the_maximum_layout_has_bounded_payload_and_linear_grounding_scratch() {
    let ids: Vec<_> = (0..MAX_CANDIDATES).collect();
    let mut relations = Relations::new(&ids).unwrap();
    for &id in &ids {
        relations.set_undoes(id, id);
        relations.set_strips(id, id);
    }
    let (accepted, scratch, output) = super::super::grounded_with_storage(&ids, &relations);
    assert_eq!(accepted, ids);
    assert_eq!(relations.payload_bytes(), 1_048_064);
    assert_eq!(relations.allocated_rows(), 2 * ids.len());
    assert!(relations.payload_bytes() <= MAX_BYTES);
    assert_eq!(
        relations.metadata_bytes(),
        2 * ids.len() * std::mem::size_of::<usize>()
            + 2 * ids.len() * std::mem::size_of::<Option<Box<[u64]>>>()
    );
    assert_eq!(
        scratch,
        ids.len() * (2 * std::mem::size_of::<usize>() + std::mem::size_of::<Option<bool>>())
    );
    assert_eq!(output, ids.len() * std::mem::size_of::<usize>());
    eprintln!(
        "relations candidates={} payload={} metadata={} scratch={} output={}",
        ids.len(),
        relations.payload_bytes(),
        relations.metadata_bytes(),
        scratch,
        output
    );
}
