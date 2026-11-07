// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual public-reader allocation measurement. No live facts or timing assertion.

#[cfg(all(unix, debug_assertions))]
fn main() {
    use aterm_netplace::place::{Known, REMEMBERED_MAX, STAYS_MAX, Stay, StayAt};
    use aterm_netplace::{Facts, NetworkId, Record, read_with, record, sample};
    let sample = sample::hotspot(1, 1);
    let facts = Facts {
        route: sample.route.clone(),
        tunnel: sample.tunnel,
        path: sample.path,
    };
    for count in [0, 1, 16, STAYS_MAX] {
        let state = aterm_tempfile::tempdir().expect("private reader state");
        let dir = record::dir(state.path());
        let written = record::Written {
            record: Record {
                stays: (0..count)
                    .map(|index| Stay {
                        start_ms: (index as u64 + 1) * 1000,
                        end_ms: (index + 1 < count).then_some((index as u64 + 2) * 1000),
                        at: StayAt::Remote,
                        network: sample.network.clone(),
                        subnet: Some("172.20.10.0/28".to_owned()),
                        gateway: facts.route.as_ref().and_then(|route| route.gateway),
                        from_start: false,
                    })
                    .collect(),
                remembered: (0..REMEMBERED_MAX)
                    .map(|index| Known {
                        network: NetworkId::parse(&format!("n{index:016x}")).expect("synthetic id"),
                        learned_ms: 1,
                        confirmed_ms: 2,
                    })
                    .collect(),
            },
            ..record::Written::default()
        };
        record::write(&dir, &written).expect("write private history");
        assert_eq!(record::read(&dir), Some(written));
        let before = std::fs::read(dir.join(record::FILE)).expect("saved history");
        let expected = read_with(state.path(), facts.clone(), 100_000);
        let inputs = vec![facts.clone(); 64];
        let start = std::time::Instant::now();
        let (_, allocations) = aterm_heap_count::allocations_in(|| {
            for facts in inputs {
                let place = read_with(std::hint::black_box(state.path()), facts, 100_000);
                assert_eq!(place, expected);
                std::hint::black_box(place);
            }
        });
        println!(
            "reader=place stays={count} remembered={REMEMBERED_MAX} rounds=64 allocations={allocations} elapsed_ns={}",
            start.elapsed().as_nanos()
        );
        let here = aterm_netplace::facts::Here {
            facts: facts.clone(),
            ..Default::default()
        };
        let expected = aterm_netplace::proof::read_with(state.path(), here.clone(), 100_000);
        let inputs = vec![here; 64];
        let start = std::time::Instant::now();
        let (_, allocations) = aterm_heap_count::allocations_in(|| {
            for here in inputs {
                let now = aterm_netplace::proof::read_with(
                    std::hint::black_box(state.path()),
                    here,
                    100_000,
                );
                assert_eq!(now, expected);
                std::hint::black_box(now);
            }
        });
        println!(
            "reader=proof stays={count} remembered={REMEMBERED_MAX} rounds=64 allocations={allocations} elapsed_ns={}",
            start.elapsed().as_nanos()
        );
        assert_eq!(
            std::fs::read(dir.join(record::FILE)).unwrap(),
            before,
            "reader writes nothing"
        );
    }
}

#[cfg(not(all(unix, debug_assertions)))]
fn main() {
    eprintln!("reader_cost requires Unix and --profile opt-test for synthetic facts");
}
