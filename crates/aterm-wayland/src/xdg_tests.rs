// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn surface_configure_latches_toplevel_geometry_and_states_across_serials() {
    let mut state = State::default();
    state.hear(Heard::Suggest {
        width: 1280,
        height: 720,
        states: vec![1, 4],
    });
    state.hear(Heard::Serial(10));
    assert_eq!(
        state.ready,
        Some(Configure {
            serial: 10,
            width: 1280,
            height: 720,
            states: vec![1, 4],
        })
    );

    // A subsequent bounds/decoration surface configure with no new xdg_toplevel.configure
    // keeps the latched toplevel width, height, and states.
    state.hear(Heard::Decorations(Decorations::Server));
    state.hear(Heard::Serial(11));
    assert_eq!(
        state.ready,
        Some(Configure {
            serial: 11,
            width: 1280,
            height: 720,
            states: vec![1, 4],
        })
    );
}
