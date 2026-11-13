// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn id(value: u64) -> Digest {
    let mut id = [0; 32];
    id[..8].copy_from_slice(&value.to_be_bytes());
    id
}

fn template() -> InventoryObservation {
    let pair = Pair::new(false, 0);
    let replica = pair.replica(0);
    let peer = pair.lab.bootstrap(1).request().unwrap().binding().actor();
    replica
        .round(peer)
        .unwrap()
        .inventory(peer, pair.scope.digest(), &mut None, &mut None)
        .unwrap()
        .unwrap()
        .observation
}

fn inventory(
    known: BTreeSet<Digest>,
    excluded: BTreeSet<Digest>,
    template: InventoryObservation,
) -> Inventory {
    let observation = InventoryObservation {
        inventory: InventorySummary::of(&known),
        ..template
    };
    Inventory::new(known, excluded, observation, None)
}

/// Equal retained inventories reuse a fingerprint; every different set still has the canonical
/// fingerprint, including equal-sized sets and inventories whose offered subset differs.
#[test]
fn peer_summaries_reuse_only_exact_retained_sets() {
    let template = template();
    for known_mask in [0u16, 1, 0x55, 0xff, 0xaa] {
        let known: BTreeSet<_> = (0..8)
            .filter(|bit| known_mask & (1 << bit) != 0)
            .map(id)
            .collect();
        for excluded in [BTreeSet::new(), known.clone()] {
            let local = inventory(known.clone(), excluded, template);
            for peer_mask in 0u16..256 {
                let peer: BTreeSet<_> = (0..8)
                    .filter(|bit| peer_mask & (1 << bit) != 0)
                    .map(id)
                    .collect();
                let expected = InventorySummary::of(&peer);
                let before = INVENTORY_HASHES.get();
                assert_eq!(local.peer_summary(&peer), expected);
                assert_eq!(
                    INVENTORY_HASHES.get() - before,
                    usize::from(peer != known),
                    "only an exact retained set avoids hashing"
                );
            }
        }
    }
}

/// The previous materialized offered set is the oracle for the lazy view: exclusions may name
/// records this store never held, and a wrap pivot need not be offered or even known.
/// NEGATIVE CONTROL: offering an excluded ID or hashing the retained count fails the comparison.
#[test]
fn offered_views_match_materialized_sets_and_canonical_summaries() {
    let template = template();
    let mut cases = 0;
    for known_mask in [0u16, 1, 0b01010101, 0xff, 0x80, 0xaa, 0x18] {
        let known: BTreeSet<_> = (0..8)
            .filter(|bit| known_mask & (1 << bit) != 0)
            .map(|bit| id(bit * 2 + 2))
            .collect();
        for excluded_mask in 0..1024 {
            let excluded: BTreeSet<_> = (0..10)
                .filter(|bit| excluded_mask & (1 << bit) != 0)
                .map(|bit| id(bit * 2 + 2))
                .collect();
            let offered: BTreeSet<_> = known
                .iter()
                .filter(|key| !excluded.contains(*key))
                .copied()
                .collect();
            let candidate = inventory(known.clone(), excluded, template);
            assert_eq!(candidate.offered_summary(), InventorySummary::of(&offered));
            for after in std::iter::once(None).chain((0..=21).map(|value| Some(id(value)))) {
                assert_eq!(
                    candidate.offered_cycle(after).copied().collect::<Vec<_>>(),
                    cycle(&offered, after).copied().collect::<Vec<_>>(),
                    "known={known_mask:x} excluded={excluded_mask:x} after={after:?}"
                );
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 164_864);
}

/// The actual round must offer exactly the checked store API's eligible records, including
/// after signed fork evidence excludes one and then both independent writers.
#[test]
fn round_offers_match_checked_store_inventory_through_real_forks() {
    for independent in [1, 3] {
        real_forks(independent);
    }
}

fn real_forks(independent: usize) {
    let pair = Pair::new(false, 1);
    let first = {
        let bootstrap = pair.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let store = Store::open(&session, &pair.paths[0], pair.scope).unwrap();
        let key = *store.inventory(&session).unwrap().next().unwrap();
        store.wire(&session, &key).unwrap().unwrap().to_vec()
    };
    let second = {
        let bootstrap = pair.lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &pair.paths[1], pair.scope).unwrap();
        (0..independent)
            .map(|_| {
                let receipt = store
                    .commit_catalog(&session, b"independent writer")
                    .unwrap();
                store.wire(&session, &receipt.id).unwrap().unwrap().to_vec()
            })
            .collect::<Vec<_>>()
    };
    {
        let bootstrap = pair.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::open(&session, &pair.paths[0], pair.scope).unwrap();
        for wire in &second {
            store.ingest(&session, wire).unwrap();
        }
    }
    let second = second.into_iter().next().unwrap();
    let check = |expected: usize| {
        let peer = pair.lab.bootstrap(1).request().unwrap().binding().actor();
        let replica = pair.replica(0);
        let inventory = replica
            .round(peer)
            .unwrap()
            .inventory(peer, pair.scope.digest(), &mut None, &mut None)
            .unwrap()
            .unwrap();
        let bootstrap = pair.lab.bootstrap(0);
        let session = Session::observe(&bootstrap).unwrap();
        let store = Store::observe(&session, &pair.paths[0], pair.scope).unwrap();
        let known: BTreeSet<_> = store.inventory(&session).unwrap().copied().collect();
        let offered: BTreeSet<_> = store
            .replication_inventory(&session)
            .unwrap()
            .copied()
            .collect();
        assert_eq!(known.len(), independent + 1);
        assert_eq!(offered.len(), expected);
        assert_eq!(inventory.known, known);
        assert_eq!(inventory.offered_summary(), InventorySummary::of(&offered));
        for after in std::iter::once(None).chain(known.iter().copied().map(Some)) {
            assert_eq!(
                inventory.offered_cycle(after).copied().collect::<Vec<_>>(),
                cycle(&offered, after).copied().collect::<Vec<_>>()
            );
        }
    };
    check(independent + 1);
    for (writer, wire) in [first, second].into_iter().enumerate() {
        let author = pair.lab.bootstrap(writer);
        let mut fork = wire.clone();
        let end = fork.len() - 64;
        fork[end - 1] ^= 1;
        crate::table::tests::resign(&author, &mut fork);
        {
            let bootstrap = pair.lab.bootstrap(0);
            let session = Session::open(&bootstrap).unwrap();
            let proof =
                crate::table::conflict::Conflict::from_pair(&session, pair.scope, &wire, &fork)
                    .unwrap();
            Store::open(&session, &pair.paths[0], pair.scope)
                .unwrap()
                .ingest_conflict(&session, proof.wire())
                .unwrap();
        }
        check(if writer == 0 { independent } else { 0 });
    }
}

fn previous_work(
    source: &BTreeSet<Digest>,
    excluded: &BTreeSet<Digest>,
    _template: InventoryObservation,
) -> (InventorySummary, InventorySummary, usize) {
    let known: BTreeSet<_> = source.iter().copied().collect();
    let excluded = excluded.clone();
    let offered: BTreeSet<_> = source
        .iter()
        .filter(|key| !excluded.contains(*key))
        .copied()
        .collect();
    let known_summary = InventorySummary::of(&known);
    let offered_summary = InventorySummary::of(&offered);
    let selected = cycle(&offered, Some(id(13))).take(BATCH).count();
    (known_summary, offered_summary, selected)
}

fn candidate_work(
    source: &BTreeSet<Digest>,
    excluded: &BTreeSet<Digest>,
    template: InventoryObservation,
) -> (InventorySummary, InventorySummary, usize) {
    let candidate = inventory(source.iter().copied().collect(), excluded.clone(), template);
    (
        candidate.observation.inventory,
        candidate.offered_summary(),
        candidate.offered_cycle(Some(id(13))).take(BATCH).count(),
    )
}

/// Materialization, summaries and one bounded cyclic send scan, with the unchanged quarantine
/// calculation outside both arms. These numbers do not measure opening or decrypting a store.
/// Both arms warm before sampling; an empty-summary cache may initialize during that warm-up.
/// Allocation and instruction passes are separate so active counter bookkeeping is not timed.
#[test]
#[ignore = "manual paired inventory allocation and instruction measurement"]
fn measure_inventory_work() {
    use aterm_bench::measurement_clock::{counters, declare_interactive};
    use std::hint::black_box;
    type Probe = fn(
        &BTreeSet<Digest>,
        &BTreeSet<Digest>,
        InventoryObservation,
    ) -> (InventorySummary, InventorySummary, usize);
    let template = template();
    let _ = declare_interactive();
    let probes: [(&str, Probe); 2] = [("reference", previous_work), ("candidate", candidate_work)];
    for size in [0, 1, 32, 256, 3000] {
        let known: BTreeSet<_> = (0..size).map(|value| id(value * 2 + 2)).collect();
        for mask in [
            "none",
            "some",
            "all",
            "foreign",
            "mixed",
            "all_foreign",
            "last",
        ] {
            let excluded: BTreeSet<_> = match mask {
                "none" => BTreeSet::new(),
                "some" => known.iter().step_by(3).copied().collect(),
                "all" => known.clone(),
                "foreign" => (0..16).map(|value| id(size * 2 + value + 3)).collect(),
                "mixed" => known
                    .iter()
                    .step_by(3)
                    .copied()
                    .chain((0..16).map(|value| id(size * 2 + value + 3)))
                    .collect(),
                "all_foreign" => known
                    .iter()
                    .copied()
                    .chain((0..16).map(|value| id(size * 2 + value + 3)))
                    .collect(),
                "last" => known
                    .iter()
                    .take(known.len().saturating_sub(1))
                    .copied()
                    .collect(),
                _ => unreachable!(),
            };
            assert_eq!(
                previous_work(&known, &excluded, template),
                candidate_work(&known, &excluded, template)
            );
            for (_, probe) in probes {
                black_box(probe(&known, &excluded, template));
            }
            for sample in 0..5 {
                for arm in if sample % 2 == 0 { [0, 1] } else { [1, 0] } {
                    let (variant, probe) = probes[arm];
                    let run = || {
                        for _ in 0..200 {
                            black_box(probe(
                                black_box(&known),
                                black_box(&excluded),
                                black_box(template),
                            ));
                        }
                    };
                    let (_, allocations) = aterm_heap_count::allocations_in(run);
                    let before = counters();
                    run();
                    let after = counters();
                    let instructions = before
                        .zip(after)
                        .map(|(a, b)| b.instructions.saturating_sub(a.instructions));
                    eprintln!(
                        "inventory size={size} mask={mask} variant={variant} sample={sample} iterations=200 allocations={allocations} instructions={instructions:?} instruction_allocations_counted=false"
                    );
                }
            }
        }
    }
}
