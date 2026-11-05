// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::frontier::{first_records, receive, wide_lab, wires};
use astream_wire::sync::MAX_PARENTS;
use aterm_json::json;

const NOW: u64 = 20_000 * EPOCH_S + 100;

fn row(counter: u64) -> Value {
    json!({"counter": counter, "until": NOW + EPOCH_S, "claims": []})
}

/// Four writers write through the product path: the three this test works on and the last, past
/// `LEGACY_MAX_MEMBERS`. The writers between them seal the same row ([`first_records`]), so every
/// read and capacity check still runs on `MAX_PARENTS + 1` rows and heads.
/// GAP: those 61 Macs stop running `write_at`'s pass (ring retirement, roll housekeeping and day
/// table enrollment), so a defect only on a middle Mac is not seen; four Macs still run it.
#[test]
fn claim_frontier_admission_preserves_rows_and_both_capacity_limits() {
    let writers = MAX_PARENTS + 1;
    let last = writers - 1;
    let lab = wide_lab(writers);
    let written = [0, 1, 2, last];
    for writer in written {
        write_at(&lab.paths[writer], &row(1), NOW, Caps::default()).unwrap();
    }
    let period = CLAIMS.epoch_of(NOW);
    let scope = CLAIMS.scope(lab.bootstrap(0).trust(), period).unwrap();
    let originals = written
        .into_iter()
        .flat_map(|writer| wires(&lab, writer, scope))
        .chain(first_records(&lab, 3..last, scope, |_, session| {
            // The bytes `write_at` encodes.
            let machine = me(session, &machines(session).unwrap()).unwrap();
            encode(&command(&machine, &row(1)).unwrap()).unwrap()
        }))
        .collect::<Vec<_>>();
    for writer in 0..3 {
        receive(&lab, writer, scope, &originals);
        assert_eq!(
            read_at(&lab.paths[writer], NOW, Caps::default())
                .unwrap()
                .rows
                .len(),
            writers
        );
    }
    let next_scope = CLAIMS.scope(lab.bootstrap(0).trust(), period + 1).unwrap();
    for (writer, caps) in [
        (
            0,
            Caps {
                records: 66,
                writer_records: 3,
            },
        ),
        (
            1,
            Caps {
                records: 67,
                writer_records: 2,
            },
        ),
    ] {
        let before = wires(&lab, writer, scope);
        write_at(&lab.paths[writer], &row(2), NOW, caps).unwrap();
        assert_eq!(wires(&lab, writer, scope), before);
        let next = wires(&lab, writer, next_scope);
        assert_eq!(next.len(), 1);
        write_at(&lab.paths[writer], &row(2), NOW, caps).unwrap();
        assert_eq!(wires(&lab, writer, next_scope), next);
    }

    let caps = Caps {
        records: 67,
        writer_records: 3,
    };
    write_at(&lab.paths[2], &row(2), NOW, caps).unwrap();
    let joined = wires(&lab, 2, scope);
    assert_eq!(joined.len(), 67);
    let bootstrap = lab.bootstrap(2);
    let session = Session::observe(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[2], bootstrap.trust());
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert_eq!(store.retained_by(&session, session.actor()).unwrap(), 3);
    drop((store, session));
    write_at(&lab.paths[2], &row(2), NOW, caps).unwrap();
    assert_eq!(wires(&lab, 2, scope), joined);

    receive(&lab, 0, scope, &joined);
    receive(&lab, 0, next_scope, &wires(&lab, 1, next_scope));
    receive(&lab, 1, scope, &joined);
    receive(&lab, 1, next_scope, &wires(&lab, 0, next_scope));
    let first = read_at(&lab.paths[0], NOW, Caps::default()).unwrap();
    let peer = read_at(&lab.paths[1], NOW, Caps::default()).unwrap();
    assert_eq!(first.rows.len(), writers);
    assert_eq!(first.rows, peer.rows);
    for paths in &lab.paths[..3] {
        let me = read_at(paths, NOW, Caps::default()).unwrap().me;
        assert_eq!(
            first
                .rows
                .iter()
                .find(|(machine, _)| *machine == me)
                .unwrap()
                .1["counter"],
            json!(2)
        );
    }
}
