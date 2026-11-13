// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn parallel_work_and_reused_pids_do_not_count_as_serial_exposure() {
    let frames: Vec<_> = [(0., 0), (10., 10), (20., 20), (30., 60), (40., 70)]
        .into_iter()
        .map(|(at, secs)| Frame {
            at,
            members: vec![Member {
                key: (1, 1),
                cpu: secs * 1_000_000_000,
                runnable: secs * 1_000_000_000,
            }],
        })
        .collect();
    let pass = Pass {
        name: "LLVM".into(),
        end: 30.,
        wall: 20.,
    };
    let out = exposure(&frames, (1, 1), &pass);
    assert_eq!(out.covered, 20.);
    assert_eq!(out.serial, 10.);
    assert_eq!(out.cpu, 50.);
    assert_eq!(exposure(&frames, (1, 2), &pass).covered, 0.);
    let serial: Vec<_> = frames
        .into_iter()
        .map(|mut f| {
            f.members[0].runnable = (f.at * 1e9) as u64;
            f
        })
        .collect();
    assert_eq!(exposure(&serial, (1, 1), &pass).serial, 20.);
}
