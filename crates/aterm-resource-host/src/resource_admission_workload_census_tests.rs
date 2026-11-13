// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn request(offset: u32) -> wire::Request {
    wire::Request {
        root_attempt: [1; 16],
        invocation_sha256: [2; 32],
        snapshot: [3; 16],
        offset,
    }
}

fn source(kind: CensusKind) -> Census {
    Census {
        kind,
        identity: None,
        completion: None,
        started: None,
        retired: false,
        drained: false,
        uncertain: false,
        closed: true,
        refused: true,
    }
}

#[test]
fn native_census_reducer_preserves_original_status_zero_missing_and_unknown() {
    // Reducer controls use explicit observations, not a decoded admission
    // or a claim that these fixture values came from an owning wait.
    let mut original = source(CensusKind::Compiler);
    original.identity = Some((42, 9));
    original.started = Some(member::Started {
        pid: 42,
        birth: 9,
        monotonic_ns: 11,
        unix_ns: 12,
    });
    let mut accounting = control::accounting::Observation::uncollected();
    accounting.cpu_times = Some(control::accounting::CpuTimes {
        user_ns: 0,
        system_ns: 0,
    });
    original.completion = Some(member::Completed {
        outcome: aterm_types::resource_admission::Outcome::Signal(9),
        cause: CompletionCause::Cancelled,
        runtime_ns: 17,
        accounting,
    });
    original.retired = true;
    original.drained = true;
    let observed = entry([4; 16], &original);
    assert_eq!(observed.started.unwrap().monotonic_ns, 11);
    assert_eq!(observed.accounting, accounting);
    assert!(matches!(
        observed.terminal,
        control::Status::Finished {
            outcome: control::Outcome::Signal(9),
            cause: control::CompletionCause::Cancelled,
            runtime_ns: 17
        }
    ));
    original.uncertain = true;
    let unknown = entry([4; 16], &original);
    assert!(matches!(unknown.terminal, control::Status::Unknown { .. }));
    assert_eq!(
        unknown.accounting,
        control::accounting::Observation::uncollected()
    );
    assert_eq!(unknown.identity, observed.identity);
    let refused = entry([5; 16], &source(CensusKind::Query));
    assert!(matches!(refused.terminal, control::Status::Refused { .. }));
    assert_eq!(
        refused.accounting,
        control::accounting::Observation::uncollected()
    );
    assert!(refused.identity.is_none());
}

#[test]
fn immutable_native_population_pages_keep_every_role_and_refuse_replay_or_skips() {
    let kinds = [
        CensusKind::Compiler,
        CensusKind::Linker,
        CensusKind::Query,
        CensusKind::BuildScript,
        CensusKind::Inventory,
        CensusKind::Sample,
    ];
    let originals: Vec<_> = (0_u64..260)
        .map(|index| {
            let mut attempt = [0; 16];
            attempt[..8].copy_from_slice(&(index + 10).to_be_bytes());
            entry(attempt, &source(kinds[index as usize % kinds.len()]))
        })
        .collect();
    let mut snapshots = Snapshots::default();
    let mut req = request(0);
    let mut received = Vec::new();
    loop {
        let capture = (req.offset == 0).then(|| originals.clone());
        let reply = snapshots.page(&req, capture).unwrap();
        let wire::Reply::Page(page) = wire::Reply::decode(&reply.encode().unwrap(), &req).unwrap()
        else {
            panic!("expected original page")
        };
        assert_eq!(page.total, 260);
        received.extend(page.entries);
        assert!(
            snapshots.page(&req, None).is_err(),
            "one-use original offset"
        );
        if received.len() == 260 {
            break;
        }
        let mut foreign = req.clone();
        foreign.snapshot = [9; 16];
        foreign.offset = 0;
        assert!(
            snapshots.page(&foreign, Some(Vec::new())).is_err(),
            "incomplete original cannot be replaced"
        );
        let mut skipped = req.clone();
        skipped.offset = received.len() as u32 + 1;
        assert!(snapshots.page(&skipped, None).is_err());
        req.offset = received.len() as u32;
    }
    assert_eq!(received, originals);
    assert!(snapshots.page(&request(0), Some(Vec::new())).is_err());
    let mut empty = request(0);
    empty.snapshot = [8; 16];
    let wire::Reply::Page(empty) = snapshots.page(&empty, Some(Vec::new())).unwrap() else {
        panic!("expected empty original population")
    };
    assert_eq!(empty.total, 0);
    assert!(empty.entries.is_empty());
}
