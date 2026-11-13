// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn the_real_snapshot_reader_captures_the_original_process_lifetime() {
    let mut world = World::new("s-live-lifetime").unwrap();
    let kernel = SourceKernel {
        controls: world.controls(),
        process: world.process.clone(),
        shell: world.shell.clone(),
    };
    let snapshot = snapshot_with(&world.opts, &kernel).expect("the real live snapshot path");
    assert_eq!(snapshot.pid, world.process.pid);
    assert_eq!(snapshot.session.as_deref(), Some(SESSION));
    let lifetime = snapshot
        .lifetime
        .expect("captured around the live process facts");
    assert_eq!(lifetime.pid(), snapshot.pid);
    assert_eq!(lifetime.observe(), ProcessLiveness::Alive);
    world.close().unwrap();
    assert_eq!(lifetime.observe(), ProcessLiveness::Ended);
}
