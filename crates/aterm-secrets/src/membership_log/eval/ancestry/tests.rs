// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn oracle(parents: &[Vec<usize>]) -> Vec<Vec<bool>> {
    let mut past = vec![vec![false; parents.len()]; parents.len()];
    for (child, edges) in parents.iter().enumerate() {
        let (earlier, rest) = past.split_at_mut(child);
        let row = &mut rest[0];
        for &parent in edges {
            row[parent] = true;
            for (slot, inherited) in row.iter_mut().zip(&earlier[parent]).take(parent) {
                *slot |= *inherited;
            }
        }
    }
    past
}

#[test]
fn an_impossible_row_budget_or_noncausal_edge_is_rejected() {
    assert!(Ancestry::from_parents_for_tests(vec![Vec::new(); 65], 15).is_err());
    assert!(Ancestry::from_parents_for_tests(vec![vec![0]], 8).is_err());
    assert!(Ancestry::from_parents_for_tests(vec![vec![1], vec![]], 8).is_err());
    assert!(Ancestry::from_parents_for_tests(Vec::new(), 0).is_ok());
}

#[test]
fn strict_rows_and_set_operations_cross_word_boundaries_exactly() {
    let parents: Vec<Vec<usize>> = (0..130)
        .map(|i| match i {
            0 => vec![],
            1..=64 => vec![0],
            65 => vec![1, 63, 64],
            66..=128 => vec![i - 1],
            _ => vec![2, 127, 128],
        })
        .collect();
    let expected = oracle(&parents);
    let graph = Ancestry::from_parents_for_tests(parents, ROW_BYTES).unwrap();
    let session = graph.session();
    for (b, row) in expected.iter().enumerate() {
        let mut copy = Bits::new(expected.len());
        session.copy_past_into(b, &mut copy);
        for (a, &wanted) in row.iter().enumerate() {
            assert_eq!(session.before(a, b), wanted, "{a} before {b}");
            assert_eq!(session.upto(a, b), a == b || wanted);
            assert_eq!(copy.get(a), wanted);
        }
    }
    let mut combined = Bits::new(expected.len());
    session.union_past_into(65, &mut combined);
    session.union_past_into(129, &mut combined);
    for (a, (&left, &right)) in expected[65].iter().zip(&expected[129]).enumerate() {
        assert_eq!(combined.get(a), left || right);
    }
    let mut one = Bits::new(expected.len());
    one.set(64);
    assert!(session.past_intersects(65, &one));
    one = Bits::new(expected.len());
    one.set(65);
    assert!(!session.past_intersects(65, &one));
    let cost = session.stats();
    assert_eq!(cost.builds, expected.len());
    assert_eq!(cost.evictions, 0);
    assert_eq!(cost.copy_words, expected.len() * 3);
    assert!(cost.intersection_words > 0);
    assert!(cost.fast_rejects > 0);
}

#[test]
fn the_current_record_bound_fills_once_with_parent_word_unions() {
    let n = 4096;
    let parents: Vec<Vec<usize>> = (0..n)
        .map(|i| if i == 0 { vec![] } else { vec![i - 1] })
        .collect();
    let graph = Ancestry::from_parents_for_tests(parents, ROW_BYTES).unwrap();
    let session = graph.session();
    assert!(session.before(0, n - 1));
    let filled = session.stats();
    assert_eq!(filled.builds, n);
    assert_eq!(filled.evictions, 0);
    assert_eq!(filled.edges, 2 * (n - 1));
    assert_eq!(filled.zero_words, n * 64);
    assert_eq!(filled.union_words, (n - 1) * 64);
    assert_eq!(filled.requests, 1);
    assert_eq!(filled.hits, 0);
    assert_eq!(filled.before_queries, 1);
    assert_eq!(filled.scratch_words, 4 * 64);
    assert_eq!(filled.scan_words, 64);
    assert!(filled.scratch_word_bytes >= 2 * 64 * std::mem::size_of::<u64>());
    assert!(filled.scratch_ordinal_bytes >= 2 * n * std::mem::size_of::<usize>());
    assert_eq!(
        filled.scratch_capacity_bytes,
        filled.scratch_word_bytes + filled.scratch_ordinal_bytes
    );
    assert_eq!(filled.peak_row_bytes, ROW_BYTES);
    assert!(filled.peak_stack <= n);
    for b in (1..n).rev() {
        assert!(session.before(b - 1, b));
    }
    let warm = session.stats();
    assert_eq!(warm.builds, filled.builds);
    assert_eq!(warm.requests, n);
    assert_eq!(warm.hits, n - 1);
    assert_eq!(warm.before_queries, n);
    assert_eq!(warm.edges, filled.edges);
    assert_eq!(warm.scan_words, filled.scan_words);
    assert_eq!(warm.scratch_words, filled.scratch_words);
    eprintln!("ancestry n={n} cold={filled:?} warm={warm:?}");
}

#[test]
fn tiny_cache_reconvergence_evicts_without_recursive_row_storage() {
    let n = 257;
    let parents: Vec<Vec<usize>> = (0..n)
        .map(|i| match i {
            0 => vec![],
            1 => vec![0],
            _ => vec![i - 1, i / 2],
        })
        .collect();
    let expected = oracle(&parents);
    let edges: usize = parents.iter().map(Vec::len).sum();
    let budget = 3 * n.div_ceil(64) * 8;
    let graph = Ancestry::from_parents_for_tests(parents, budget).unwrap();
    let session = graph.session();
    for pass in 0..2 {
        for step in 1..n {
            let b = if pass == 0 { step } else { n - step };
            for (a, &wanted) in expected[b].iter().enumerate() {
                assert_eq!(session.before(a, b), wanted);
            }
        }
    }
    let cost = session.stats();
    assert!(cost.evictions > 0);
    assert!(cost.builds > 3);
    assert!(cost.peak_row_bytes <= budget);
    assert!(cost.peak_stack <= n);
    assert!(cost.edges <= cost.builds * edges);
}

#[test]
fn shared_clone_queries_and_poison_recovery_preserve_exact_rows() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Ancestry>();
    let graph =
        Ancestry::from_parents_for_tests(vec![vec![], vec![0], vec![0], vec![1, 2]], 32).unwrap();
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let graph = Arc::clone(&graph);
            std::thread::spawn(move || {
                let session = graph.session();
                assert!(session.before(0, 3));
                assert!(session.before(1, 3));
                assert!(session.before(2, 3));
                assert!(!session.before(1, 2));
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap();
    }
    let fault = Arc::clone(&graph);
    assert!(
        std::thread::spawn(move || {
            fault.with_builder_held_for_test(|| panic!("interrupt the derived cache owner"));
        })
        .join()
        .is_err()
    );
    assert!(!graph.session().before(2, 1));
    assert!(graph.session().before(2, 3));
    assert!(
        graph.cache.is_poisoned(),
        "warm rows do not lock the poisoned builder"
    );
    let built = graph.session().stats().builds;
    graph.with_builder_held_for_test(|| {});
    assert!(!graph.cache.is_poisoned());
    assert!(graph.session().before(2, 3));
    assert_eq!(
        graph.session().stats().builds,
        built,
        "published rows survive recovery"
    );
}
