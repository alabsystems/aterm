// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn prelude_refuses_to_drop_final_exec_footprint_policy() {
    let mut command = spawnfd::command("/usr/bin/true");
    command.footprint_limit(spawnfd::FootprintLimit::new(64).unwrap());
    let error = match Owner::new(&command, &[], Instant::now() + Duration::from_secs(60)) {
        Ok(_) => panic!("a plan without a policy field silently discarded final-exec attributes"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(command.get_footprint_limit().unwrap().mib(), 64);
}
