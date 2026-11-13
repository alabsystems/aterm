// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! This machine's names on Windows: read in process, where no `scutil` or `/bin/hostname` exists.

use super::{machine_id_among, normalize_machine_id, windows_host_names};

#[test]
fn the_host_names_hold_the_computer_name() {
    let names = windows_host_names();
    assert!(!names.is_empty(), "Windows names this machine");
    assert!(
        names.iter().all(|n| !n.trim().is_empty()),
        "no blank name: {names:?}"
    );
    let computer = std::env::var("COMPUTERNAME").expect("Windows sets COMPUTERNAME");
    assert!(
        names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(computer.trim())),
        "COMPUTERNAME {computer:?} is among {names:?}"
    );
    assert_eq!(
        machine_id_among(names.clone()),
        names.iter().find_map(|n| normalize_machine_id(n)),
        "the machine id is the first of them that names a machine"
    );
    assert_eq!(
        normalize_machine_id(&computer),
        names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(computer.trim()))
            .and_then(|n| normalize_machine_id(n)),
        "and COMPUTERNAME names the same machine in either spelling"
    );
}
