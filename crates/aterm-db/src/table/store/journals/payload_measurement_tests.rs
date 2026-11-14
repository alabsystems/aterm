// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual production-store lookups under an isolated cache with shipped limits;
//! no network or whole-app latency claim.

use super::super::{JOURNAL_BYTES, Journals, KEPT_BYTES, KEPT_TABLES, with_own};
use crate::{
    catalog::Directory,
    crypto::Causal,
    replica::Replica,
    table::{Scope, Session, tests::Lab},
};
use aterm_bench::measurement_clock::counters;
use aterm_digest::Sha256;
use aterm_secrets::commons::roster::Role;
use std::{sync::Arc, time::Instant};

const RECORDS: usize = 8192;
const WRITERS: usize = 4;
const LOOKUP_BATCH: usize = 16;

fn replica(lab: &Lab, writer: usize) -> Replica {
    Replica::commons_until(
        lab.paths[writer].clone(),
        lab.paths[writer].home.join("config.toml"),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
}

fn populated(payload_bytes: usize) -> (Lab, Scope, usize) {
    let lab = Lab::new(&[Role::Writer; WRITERS]);
    let bootstraps: Vec<_> = (0..WRITERS).map(|writer| lab.bootstrap(writer)).collect();
    let sessions: Vec<_> = bootstraps
        .iter()
        .map(|bootstrap| Session::open(bootstrap).unwrap())
        .collect();
    let directory = Directory::new(&lab.paths[0], bootstraps[0].trust());
    let scope = directory
        .open_application(&sessions[0], "repair-payload-measurement", "rows", 1)
        .unwrap();
    let mut store = directory.open_store(&sessions[0], scope).unwrap();
    let mut previous = [None; WRITERS];
    let payload = vec![b'x'; payload_bytes];
    let wires: Vec<_> = (0..RECORDS)
        .map(|row| {
            let writer = row % WRITERS;
            let wire = sessions[writer]
                .seal(
                    scope,
                    Causal {
                        sequence: u64::try_from(row / WRITERS + 1).unwrap(),
                        previous: previous[writer],
                        parents: previous[writer].as_slice(),
                    },
                    &payload,
                )
                .unwrap();
            previous[writer] = Some(Sha256::digest(&wire));
            wire
        })
        .collect();
    let fixture_wire_bytes = wires.iter().map(Vec::len).sum();
    for batch in wires.chunks(aterm_journal::MAX_APPEND_RECORDS) {
        let borrowed: Vec<_> = batch.iter().map(Vec::as_slice).collect();
        assert_eq!(
            store.ingest_batch(&sessions[0], &borrowed).unwrap().len(),
            batch.len()
        );
    }
    drop((store, sessions));
    (lab, scope, fixture_wire_bytes)
}

/// Compare the actual fresh Replica/store access used by the vault adapter with
/// eager or bounded lookup scheduling. Fixture writes, sealing, inventory and
/// digest validation precede all counters; counts, capacity bookkeeping, drops
/// and counter endpoints remain inside the reported own-process CPU cost.
/// Returned-payload capacity excludes the store/cache, allocator rounding,
/// identity lists, frame buffering, network I/O and every other process.
#[test]
#[ignore = "manual paired outgoing-lookup CPU/capacity measurement; no latency budget"]
fn outgoing_payload_lookup_cpu_measurement() {
    let cache = Arc::new(Journals::new(KEPT_TABLES, KEPT_BYTES, JOURNAL_BYTES));
    with_own(Arc::clone(&cache), || measure(&cache));
    assert_eq!(
        Arc::strong_count(&cache),
        1,
        "the diagnostic owns the cache"
    );
    let (_, requested_bytes) = aterm_heap_count::bytes_freed_in(|| drop(cache));
    assert!(
        requested_bytes > 0,
        "the allocator observed cache destruction"
    );
    println!(
        "{}",
        aterm_json::json!({
            "measurement": "outgoing_payload_lookup_retained_cache",
            "cache_owned_requested_bytes": requested_bytes,
            "scope": "allocations freed by the final cache drop, including its fixed container and mutexes; excludes allocator rounding, in-flight stores, global signature cache and RSS",
            "outside_lookup_cpu_counters": true,
        })
    );
}

fn measure(cache: &Journals) {
    let mode = std::env::var("ATERM_REPAIR_LOOKUP_MODE").unwrap_or_else(|_| "eager".into());
    assert!(matches!(mode.as_str(), "eager" | "bounded"));
    let payload_bytes: usize = std::env::var("ATERM_REPAIR_PAYLOAD_BYTES")
        .unwrap_or_else(|_| "32".into())
        .parse()
        .unwrap();
    assert!(matches!(payload_bytes, 32 | 2048));
    let offered: usize = std::env::var("ATERM_REPAIR_OFFER_RECORDS")
        .unwrap_or_else(|_| "16".into())
        .parse()
        .unwrap();
    assert!(matches!(offered, 16 | 512));
    let (lab, scope, fixture_wire_bytes) = populated(payload_bytes);
    let inventory = replica(&lab, 0)
        .mesh_inventory(scope.digest())
        .unwrap()
        .unwrap();
    assert_eq!(inventory.len(), RECORDS);
    let ids: Vec<_> = inventory.into_iter().take(offered).collect();
    let validated = replica(&lab, 0).mesh_wires(scope.digest(), &ids).unwrap();
    assert_eq!(validated.len(), ids.len());
    for (id, wire) in ids.iter().zip(&validated) {
        assert_eq!(*id, Sha256::digest(wire));
    }
    drop(validated);
    let chunk = if mode == "bounded" {
        LOOKUP_BATCH
    } else {
        offered
    };
    for round in 0..5 {
        let reads_before = cache.test_reads();
        let (opens_before, checks_before) = crate::table::store::test_opens_and_checks();
        let mut returned = 0;
        let mut peak_wires = 0;
        let mut peak_payload_capacity_bytes = 0;
        let probe = counters().expect("kernel own-process counters");
        let before = counters().expect("kernel own-process counters");
        for requested in ids.chunks(chunk) {
            let wires = replica(&lab, 0)
                .mesh_wires(scope.digest(), std::hint::black_box(requested))
                .unwrap();
            returned += wires.len();
            peak_wires = peak_wires.max(wires.len());
            let retained = wires.capacity() * size_of::<Vec<u8>>()
                + wires.iter().map(Vec::capacity).sum::<usize>();
            peak_payload_capacity_bytes = peak_payload_capacity_bytes.max(retained);
            drop(std::hint::black_box(wires));
        }
        let after = counters().expect("kernel own-process counters");
        let elapsed = after.since(before);
        let (opens_after, checks_after) = crate::table::store::test_opens_and_checks();
        let reads_after = cache.test_reads();
        assert_eq!(returned, offered);
        assert!(peak_wires <= chunk);
        assert!(elapsed.cpu_ns >= 0.0);
        println!(
            "{}",
            aterm_json::json!({
                "measurement": "outgoing_payload_lookup",
                "lookup_mode": mode,
                "clock": "proc_pid_rusage_self",
                "fixture_records": RECORDS,
                "fixture_wire_bytes": fixture_wire_bytes,
                "payload_bytes": payload_bytes,
                "offered_records": offered,
                "requested_chunk_records": chunk,
                "writers": WRITERS,
                "round": round,
                "warmup": round == 0,
                "store_opens": opens_after - opens_before,
                "store_checks": checks_after - checks_before,
                "journal_reads": reads_after - reads_before,
                "journal_cache_limit_bytes": JOURNAL_BYTES,
                "journal_cache_total_limit_bytes": KEPT_BYTES,
                "journal_cache_table_limit": KEPT_TABLES,
                "peak_returned_wires": peak_wires,
                "peak_payload_capacity_bytes": peak_payload_capacity_bytes,
                "before_cpu_ns": before.cpu_ns,
                "after_cpu_ns": after.cpu_ns,
                "probe_cpu_ns": before.since(probe).cpu_ns,
                "cpu_ns": elapsed.cpu_ns,
                "instructions": elapsed.instructions,
                "cycles": elapsed.cycles,
            })
        );
    }
}
