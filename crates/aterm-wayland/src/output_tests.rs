// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn v1_output_publishes_state_without_done_while_v2_waits_for_done() {
    let mode = Mode {
        flags: wl_output::mode::CURRENT,
        width: 1920,
        height: 1080,
        refresh: 60_000,
    };
    let mut v1 = OutputBook {
        version: 1,
        pending: OutputInfo {
            global: 7,
            scale: 1,
            ..OutputInfo::default()
        },
        done: None,
    };
    let heard = v1
        .hear(Reported::Mode(mode))
        .expect("v1 commits immediately");
    assert_eq!(heard.current_mode(), Some(mode));
    assert_eq!(
        v1.done.as_ref().and_then(OutputInfo::current_mode),
        Some(mode)
    );

    let mut v2 = OutputBook {
        version: 2,
        pending: OutputInfo {
            global: 8,
            scale: 1,
            ..OutputInfo::default()
        },
        done: None,
    };
    assert!(v2.hear(Reported::Mode(mode)).is_none());
    assert!(v2.done.is_none());
    let heard = v2.hear(Reported::Done).expect("v2 commits on done");
    assert_eq!(heard.current_mode(), Some(mode));
}
