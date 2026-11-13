// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Config;

#[test]
fn a_member_reads_automatically_unless_watch_or_swarm_is_off() {
    for (text, reads) in [
        ("", true),
        ("[fleet]\n", true),
        ("[fleet]\nwatch = true\n", true),
        ("[fleet]\nwatch = false\n", false),
        ("[swarm]\nenabled = false\n", false),
        ("[swarm]\nenabled = false\n[fleet]\nwatch = true\n", false),
    ] {
        let config = aterm_toml::from_str::<Config>(text).expect("valid configuration");
        for minted in [false, true] {
            let book = crate::fleet_glance::BookFacts {
                in_fleet: true,
                minted,
                ..crate::fleet_glance::BookFacts::default()
            };
            assert_eq!(
                crate::fleet_glance::should_poll(
                    &book,
                    config.fleet_watch_enabled(),
                    None,
                    1_000,
                    false,
                    crate::fleet_glance::POLL_EVERY,
                ),
                reads,
                "minted={minted} {text:?}"
            );
        }
    }
}
