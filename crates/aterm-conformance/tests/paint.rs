// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Paint conformance: the shape matrix that pixel-checks the shipped binary.
//!
//! Launches a release-profile `aterm` headless, drives real keystrokes through
//! the control socket, captures the take, and asserts on the pixels through
//! `tools/paint-conformance/paint_probe.sh`. Run by `tools/verify.sh --measure
//! <aterm.app>` against the executable of the bundle a release built once, it is
//! the release's pixel proof of the bytes it ships (docs/RELEASE-PROOF-DISCIPLINE.md).
//!
//! The matrix has 23 live-artifact rows plus the scanner's semantic negative
//! controls: prompt, fake-Claude, ESC7/ESC8 streamer and cold-output shapes; a
//! single pane and a two-pane split (row 12 and its off twin); pinned video and
//! unpinned focused/unfocused images; the default resident pet, the earned
//! flying cat and the owner-spelling overlap; and four matched typed `off`
//! twins, which keep cursor/text deltas while requiring zero ribbon geometry
//! and a quiet effect ledger.
//!
//! Trail-only rows pin the companion-free `flying` style. Companion rows pin
//! their style, pair every `ctl image` with the engine's `trail status`, and
//! demand both the semantic claim and a tall foreground component in that
//! frame, so neither a live state with no sprite nor rainbow ink with no animal
//! passes.
//!
//! The observer rule: an in-flight `ctl video` recording pins the motion-focus
//! input (`App::motion_focus` = `focused || this window is being recorded`),
//! and a `--headless` window starts `focused: true`. Rows 1-4 therefore cannot
//! catch a broken motion gate. Rows 5 and 6 remove both: `--capture image`
//! takes a burst of `ctl image` renders (no recording) and `--focus out` goes
//! through the same `App::on_focus` the OS event calls, while control-socket
//! typing keeps driving the window. With the `cursor_fx_focus` typed-wake term
//! removed, only that arm goes red.
//!
//! Row 5's discriminator is hue spread, not pixel count: an unfocused typed
//! window still moves a saturated block cursor and echoes glyphs, but only the
//! rainbow trail spreads ink across the hue wheel.
//!
//! Ink means dynamic saturated pixels: saturated and changed against frame 0
//! (static syntax colour is unchanged; echoed glyphs are unsaturated). The
//! probe's thresholds: ≥150 such px across the take in ≥4 of 12 hue buckets.
//! The typed rows use [`MATURE_RUN`]'s 29 keys, where healthy takes sit far
//! above both floors and the controls stay near zero.
//!
//! The merge gate's test run skips every `measuring::…` test
//! (`aterm_verify::stages::MEASURING_TESTS`); they are the MEASURE tier
//! (`tools/verify.sh --measure <APP>`, `--full <APP>`), run alone in the
//! exclusive `measuring tests` stage over the release bundle the release built
//! once, and a release signs only bytes they measured green. A probe
//! that decides nothing (exit 2, or paint's unproved exit 3) is recorded as
//! COULD NOT RUN, never as a finding or as green.
//!
//! macOS only: the scanner decodes frames with `sips` and the artifact is the
//! macOS bundle's binary. The video rows audit a real-time pipeline, so
//! verdict-bearing runs want a quiet machine; re-run a charged video row with
//! `ATERM_PAINT_KEEP=1` before believing or dismissing it (the probe header's
//! LOAD sensitivity note). The binary is the release bundle's, handed over in
//! `ATERM_PAINT_BIN`; without it a row says `not measured`
//! (`support::release_bin`).
#![cfg(target_os = "macos")]

mod support;

#[path = "paint/measuring.rs"]
mod measuring;
