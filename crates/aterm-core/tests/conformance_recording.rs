// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for hydration-faithfulness (design B.8.4): bind the
//! abstract `recording_model` (B.8.3 Tier-0 ty proof) to the SHIPPING engine.
//!
//! The Tier-0 model proves `P(replay@t) = P(live@t)` over an abstract parity
//! fold. This test proves the *real* engine obeys the same property: a keyframe
//! taken with [`Terminal::checkpoint`] at `t0`, plus the recorded delta events
//! `t0 -> t1` replayed through the real [`Terminal::process`] pipeline on a
//! freshly [`Terminal::from_checkpoint`]-hydrated terminal, reproduces the live
//! terminal's captured projection at `t1` exactly.
//!
//! Equality is the full captured checkpoint projection (`live.checkpoint() ==
//! replay.checkpoint()`) — grid bytes, cursor/region/wrap/tabs, modes, style,
//! charset, keyboards, etc. (the DEFERRED grouped/clock-domain fields are not in
//! the projection and so are not asserted here; see `checkpoint.rs`).
//!
//! A negative control (drop one delta chunk on replay) MUST make the projections
//! diverge — so the conformance is non-vacuous: it would catch an unfaithful
//! replay, not merely pass because both sides ran the same code.

use aterm_core::terminal::{HostBindings, Terminal};

/// A hard-case prefix script (driven to `t0`, the keyframe point). Exercises
/// scrollback, SGR, scroll region, alt-screen round-trip, cursor moves, charset,
/// kitty keyboard, cwd — i.e. a rich captured state to hydrate from. Ends at
/// parser-ground (required by `checkpoint()`).
const PREFIX: &[&[u8]] = &[
    b"\x1b[1;4;38;5;202;48;2;10;20;30mstyled prefix\x1b[0m\r\n",
    b"alpha\r\nbravo\r\ncharlie\r\ndelta\r\necho\r\nfoxtrot\r\ngolf\r\nhotel\r\n",
    b"\x1b[2;9r",                   // DECSTBM scroll region
    b"\x1b(0",                      // G0 = DEC special graphics
    b"\x1b[>5u",                    // kitty keyboard push
    b"\x1b]7;file://host/work\x07", // OSC 7 cwd
    b"\x1b[?1049h",                 // enter alt screen
    b"ALT BODY here\r\n",
    b"\x1b[?1049l", // leave alt screen (main saved/restored)
    b"\x1b[3;1Hback on main",
];

/// Delta events `t0 -> t1` (replayed on both the live and hydrated terminals).
/// Each chunk ends at parser-ground. These touch CAPTURED state (text, cursor,
/// scroll, SGR, a mode toggle) so the comparison is meaningful and dropping any
/// chunk perturbs the grid.
const DELTA: &[&[u8]] = &[
    b"\x1b[5;1Hmore output line\r\n",
    b"\x1b[7mreverse\x1b[0m and \x1b[32mgreen\x1b[0m\r\n",
    b"scroll1\r\nscroll2\r\nscroll3\r\n", // pushes content through the scroll region
    b"\x1b[1;1Htop-left edit",
    b"\x1b[?7l", // toggle autowrap off (a captured mode bit)
];

/// Drive `t` through a slice of byte chunks.
fn run(t: &mut Terminal, chunks: &[&[u8]]) {
    for c in chunks {
        t.process(c);
    }
}

#[test]
fn replay_from_checkpoint_matches_live_engine() {
    let (rows, cols) = (10u16, 40u16);

    // Live timeline: prefix -> t0 (keyframe) -> delta -> t1.
    let mut live = Terminal::new(rows, cols);
    run(&mut live, PREFIX);
    assert!(
        live.parser_is_ground(),
        "prefix must end at parser-ground for checkpoint()"
    );
    let keyframe = live.checkpoint(); // t0

    run(&mut live, DELTA); // advance live to t1
    assert!(live.parser_is_ground(), "delta must end at parser-ground");

    // Hydrated timeline: from_checkpoint(t0) -> replay the SAME delta -> t1'.
    let mut replay = Terminal::from_checkpoint(&keyframe, HostBindings::none());
    run(&mut replay, DELTA);

    // Faithfulness: the real engine's checkpoint+replay reproduces live at t1
    // over the full captured projection. This is P(replay@t1) = P(live@t1)
    // bound to the SHIPPING engine (the Tier-0 recording_model proves the
    // abstract version; this proves the concrete one).
    assert_eq!(
        live.checkpoint(),
        replay.checkpoint(),
        "checkpoint+replay must reproduce the live projection (B.8.4 faithfulness)"
    );
    // Human-readable cross-check on the rendered content.
    assert_eq!(
        live.visible_content(),
        replay.visible_content(),
        "rendered content must match after replay"
    );
}

#[test]
fn replay_negative_control_dropped_delta_diverges() {
    // Same setup, but DROP one delta chunk on replay. The projections MUST
    // diverge — proving the conformance assertion is non-vacuous (it detects an
    // unfaithful replay rather than passing trivially). This is the Tier-1
    // analogue of the recording_model's Buggy=1 dropped-event counterexample.
    let (rows, cols) = (10u16, 40u16);
    const DROP_IDX: usize = 2; // the "scroll1/2/3" chunk — perturbs the grid

    let mut live = Terminal::new(rows, cols);
    run(&mut live, PREFIX);
    let keyframe = live.checkpoint();
    run(&mut live, DELTA);

    let mut replay = Terminal::from_checkpoint(&keyframe, HostBindings::none());
    for (i, c) in DELTA.iter().enumerate() {
        if i == DROP_IDX {
            continue; // drop one recorded event
        }
        replay.process(c);
    }

    assert_ne!(
        live.checkpoint(),
        replay.checkpoint(),
        "dropping a recorded delta event MUST diverge (negative control); if this \
         passes, the faithfulness assertion is vacuous"
    );
}
