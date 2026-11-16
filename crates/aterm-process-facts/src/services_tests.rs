// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const GIB: u64 = 1 << 30;
const M3: u64 = 128 * GIB;

fn footprint(id: u64) -> Option<u64> {
    match id {
        1 => Some(140 * GIB),
        7 => Some(40 * GIB),
        9 => Some(GIB),
        11 => Some(50 * GIB),
        13 => Some(900 << 20),
        _ => None,
    }
}

#[test]
fn system_accounts_are_below_500_and_nobody() {
    assert!(system_uid(0) && system_uid(88) && system_uid(499) && system_uid(NOBODY_UID));
    assert!(!system_uid(500) && !system_uid(501));
    assert!(kernel_member(0) && kernel_member(1) && !kernel_member(2));
}

fn member(pid: u32, uid: u32, name: &str, start_s: u64, coalition: u64) -> Member {
    Member {
        pid,
        uid,
        name: name.into(),
        start_s,
        start_us: 0,
        coalition,
    }
}

/// The fold keeps the coalitions of system accounts alone, led by the earliest-born member,
/// leaves out the kernel's coalition, names a coalition whose footprint does not read by its
/// lead, and reads an image only at or above the supplied footprint cutoff.
#[test]
fn the_fold_keeps_coalitions_of_system_accounts_alone() {
    let members = [
        member(537, 0, "fseventsd", 100, 7),
        member(88, 0, "mds", 90, 9),
        member(89, 0, "mds_stores", 80, 9),
        member(700, 501, "keyboxd", 50, 11),
        member(701, 0, "helper", 40, 11),
        member(702, 0, "unread", 40, 12),
        member(160, 88, "WindowServer", 60, 13),
        member(0, 0, "kernel_task", 0, 1),
        member(1, 0, "launchd", 1, 1),
    ];
    let mut paths = Vec::new();
    let reading = fold(&members, M3, 7 * GIB, footprint, |pid| {
        paths.push(pid);
        Some(format!("/path/of/{pid}"))
    });
    let leads: Vec<(u64, u32, &str, u32)> = reading
        .services
        .iter()
        .map(|s| (s.coalition, s.pid, s.name.as_str(), s.live))
        .collect();
    assert_eq!(
        leads,
        [
            (7, 537, "fseventsd", 1),
            (9, 89, "mds_stores", 2),
            (13, 160, "WindowServer", 1)
        ]
    );
    assert_eq!(paths, [537], "only the service followed");
    assert_eq!(reading.total, M3);
    assert_eq!(reading.services[0].path.as_deref(), Some("/path/of/537"));
    let unread: Vec<(u64, u32)> = reading
        .unread
        .iter()
        .map(|m| (m.coalition, m.pid))
        .collect();
    assert_eq!(unread, [(12, 702)]);
    let mut observed_services = reading.services.clone();
    for service in &mut observed_services {
        service.path = None;
    }

    // The caller chooses the cutoff: lowering it reads the remaining observed services,
    // the exact largest footprint still reads its lead, and one byte above reads none.
    for (cutoff, expected) in [
        (0, vec![537, 89, 160]),
        (40 * GIB, vec![537]),
        (40 * GIB + 1, vec![]),
    ] {
        let mut paths = Vec::new();
        let mut again = fold(&members, M3, cutoff, footprint, |pid| {
            paths.push(pid);
            Some(format!("/path/of/{pid}"))
        });
        assert_eq!(paths, expected, "cutoff {cutoff}");
        for service in &mut again.services {
            service.path = None;
        }
        assert_eq!(again.services, observed_services);
        assert_eq!(again.total, reading.total);
        assert_eq!(again.unread, reading.unread);
    }
}
