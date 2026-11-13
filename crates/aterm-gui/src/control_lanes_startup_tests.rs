// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn abandoned_start_releases_its_worker_state() {
    for (rpc, subscriptions, open) in [(1, 1, 1), (0, 1, 1), (1, 0, 0)] {
        let lanes = Lanes::new(LaneLimits {
            rpc,
            wait: 1,
            inline: 1,
            open,
            subscriptions,
        });
        let weak = Arc::downgrade(&lanes);
        let started = lanes.start();
        assert_eq!((started.rpc, started.subscriptions), (rpc, subscriptions));
        drop(started);
        drop(lanes);
        assert!(
            aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                Duration::from_millis(5),
                || weak.upgrade().is_none().then_some(())
            )
            .is_some(),
            "abandoned startup retained its idle workers"
        );
    }
}
