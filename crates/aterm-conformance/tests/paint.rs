// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PAINT CONFORMANCE — the shape matrix that pixel-checks the shipped binary.
//!
//! Born from the 2026-08-24 blackout audit (docs/RELEASE-PROOF-DISCIPLINE.md):
//! v0.48.0 and v0.49.0 shipped the rainbow cursor trail dark past green gates
//! because every proof measured a different machine, screen or profile than the
//! owner runs, and nothing in CI or the cut ever pixel-checked a shipped
//! artifact. This suite closes the CI half: it launches a RELEASE-profile
//! `aterm` HEADLESS, drives real keystrokes through the control
//! socket, records the take with `ctl video … full pace`, and asserts on the
//! pixels — through `tools/paint-conformance/paint_probe.sh`, the same driver
//! and scanner the release cut's paint smoke uses, so the two gates cannot
//! drift apart.
//!
//! THE MATRIX has 21 live-artifact rows plus the scanner's own semantic
//! negative controls. It covers prompt, fake-Claude, ESC7/ESC8 streamer and
//! cold-output shapes; pinned video and unpinned focused/unfocused images;
//! shipped-default resident pet, earned flying cat and owner-spelling overlap;
//! and four matched typed `off` twins. Those off twins preserve cursor/text
//! deltas while requiring zero ribbon geometry and a quiet effect ledger, so a
//! cursor, glyph or resident animal cannot satisfy the positive classifier.
//!
//! The companion rows close a different false-green class. Trail-only rows pin
//! the companion-free `flying` style; companion rows pin their explicit style
//! and pair every `ctl image` with the
//! engine's exact `trail status`, then demand BOTH the semantic claim and a
//! tall foreground connected component in that captured frame. A live pet/cat
//! state with an omitted sprite fails; rainbow ink with no animal also fails.
//!
//! WHY ROWS 5 AND 6 EXIST — THE OBSERVER RULE (docs/RELEASE-PROOF-DISCIPLINE.md).
//! Rows 1-4 drive `ctl video`, and an in-flight recording PINS the motion-focus
//! input for the recorded window (`App::motion_focus` = `focused || this window
//! is being recorded`). So those rows animate the cursor effects even when the
//! gate they depend on is broken. That is not hypothetical: it is how v0.48-v0.50
//! shipped the trail dark past this very matrix — the instrument healed the bug
//! it was sent to find. A second healer sat behind it: a `--headless` window
//! seeds `focused: true` and only a real `WindowEvent::Focused` corrects it, so
//! even an unrecorded headless take was a FOCUSED take.
//!
//! Rows 5 and 6 remove both. `--capture image` takes a BURST of `ctl image`
//! captures — a queued render, no recording, `motion_focus` untouched — and
//! `--focus out` routes through the same `App::on_focus` the OS event calls, so
//! `WindowState::focused` is genuinely false while control-socket typing (which
//! never grants OS key focus) keeps driving the window. That is the owner's real
//! shape, and it is the only row in this file that can go red for the reason the
//! file exists.
//!
//! PROVEN RED, 2026-08-24. HEAD's `cursor_fx_focus` typed-wake term was removed
//! in a scratch worktree (the v0.48-v0.50 blackout, reproduced) and the RELEASE
//! binary rebuilt there; the healthy arm is the SAME worktree restored and
//! rebuilt, so the two binaries differ in that term and nothing else.
//!
//!   row 2's shape, `ctl video` (pinned)      broken: total_ink=4930 hues=9 PASS
//!   row 5's shape, unpinned but still FOCUSED broken: total_ink=1100 hues=9 PASS
//!   row 5's shape, unpinned AND unfocused     broken: total_ink=1470 hues=1 FAIL
//!   row 5's shape, unpinned AND unfocused    healthy: total_ink=1553 hues=9 PASS
//!   row 6's shape (control)                  healthy: total_ink<=196 hues=1 PASS
//!
//! The first line is the blindness itself, measured: the gate that exists to
//! catch this bug passed the bug. The second is the second healer, measured.
//! Only the third arm — both healers removed — goes red.
//!
//! Note what the discriminator is in row 5: HUE SPREAD, not raw pixel count.
//! An unfocused typed window still moves a saturated block cursor and echoes
//! glyphs, and that lands ~1500 single-hue dynamic-saturated px in the take
//! whether or not the trail paints. The RAINBOW trail is what spreads ink
//! across the hue wheel — 9 of 12 buckets healthy, 1 dark — so row 5 is gated
//! on `--min-hues` with a floor of 4 sitting between the two arms with margin
//! on both sides.
//!
//! INK means dynamic saturated pixels: saturated AND changing against frame 0
//! (static syntax color is saturated but byte-identical; echoed monochrome
//! glyphs change but are unsaturated; only an effect is both). The thresholds
//! live in the probe: ≥150 dynamic saturated px across ≥4 of 12 hue buckets.
//! Re-measured 2026-08-24 across this whole matrix on a healthy HEAD (RELEASE
//! profile, headless 584x350) — a stale calibration is a lie the next reader
//! inherits, so these are the numbers the rows actually produced:
//!
//!   rows 1-4, capture=video   129-177 frames  total_ink 3856-5788  hues 8-10
//!   row 5,    capture=image        29 frames  total_ink 1425-1553  hues 9
//!   row 3,    the dark control     31 frames  total_ink 0
//!   row 6,    the quiet control    25 frames  total_ink 0-196      hues 0-1
//!
//! The busiest SINGLE frame ran 98-136 px on every ink arm under the retired
//! traverse, which is why the ≥150 floor gates the TAKE and not a frame.
//!
//! RE-MEASURED 2026-08-30, after the per-mark traverse shipped and the typed
//! rows moved to [`MATURE_RUN`]'s 29 keys (shipped dist binary, same headless
//! 584x350) — those 2026-08-24 lines above describe the fixed-40-cell
//! traverse at 10-11 keys and are kept as history, not calibration:
//!
//!   rows 1/2/4, capture=video  246-250 frames  total_ink 35,162-46,827
//!                              union_hues 11   driven_dark_us=0
//!                              mature_lead_us 120-159k (the pre-glass lead;
//!                              the 158,126 outlier is the merged tree's 3x
//!                              sweep — still 3x under the probe's 500 ms
//!                              MATURE_LEAD_MAX_US ceiling)
//!   rows 5/6a/6b, image        67 frames       total_ink 9,698-9,765
//!                              union_hues 11   ribbon_window_hues 11
//!                              ribbon_bound 43 ribbon_dark 0
//!
//! best_ink (the busiest single frame) now runs 220-482 on the ink arms — the
//! longer run stacks more concurrent sparks — so the 150 take-floor sits far
//! under every healthy arm on both axes, and the controls still measure 0-766
//! total with 0-3 hue buckets (the pet's coat, when one is minted saturated).
//!
//! WIRING: the `guards` lane of `xtask gate lint` covers this matrix —
//! `tools/paint_guard.sh` nonce-relinks this test and the release app, then runs
//! a private copy directly whenever Cargo's derived artifact/test source closure
//! or this gate's own machinery differs from the last take it proved green.
//!
//! RUNTIME, and why it moved: the probe's whole-run watchdog used to sleep out
//! the full `--budget` and be killed on exit, which orphaned its `sleep` — and
//! the orphan kept the probe's stdout/stderr open, so `Command::output()`
//! below did not RETURN until the budget elapsed no matter how fast the row
//! decided. Every row cost its 180 s budget; this file's four rows cost ~12
//! min of pure waiting. The watchdog now retires itself off a sentinel.
//! MEASURED 2026-08-24 after the watchdog fix: the historical SIX-row matrix,
//! serialized against the release binary, `finished in 47.69s`. The four-row
//! matrix it replaced could not finish in under 720 s (4 x its own 180 s
//! budget) no matter how fast the rows decided. Rows 7 and 8 are bounded image
//! bursts (36 and 108 frames respectively), not additional five-second videos.
//!
//! macOS-ONLY LANE, honestly: the scanner decodes frames by shelling to
//! `sips`, and the artifact under judgment is the macOS bundle's binary, so the
//! whole file is `#![cfg(target_os = "macos")]` and this target holds no tests
//! elsewhere. The matrix runs on the mac that cuts releases, via
//! tools/paint_guard.sh and the release smoke.
//!
//! QUIET-MACHINE LANE, too: the video rows audit a real-time pipeline, and
//! one sweep run beside a full workspace compile charged row 1 with a
//! 35,264 us rendered gap (normal sampling hole — the band, not the capture,
//! went dark) that ten deliberate CPU/memory-load re-creations could not
//! reproduce. Run verdict-bearing sweeps with no parallel builds, and re-run
//! any charged video row on a quiet machine with `ATERM_PAINT_KEEP=1` before
//! believing or dismissing it — the probe header's LOAD SENSITIVITY note
//! carries the measurements.
//!
//! The binary under test is RELEASE profile (audit rule 1: parts of the old
//! proof ran debug binaries). Without `ATERM_PAINT_BIN`, the shared conformance
//! helper freshens `target/conformance-release/release/aterm`; its dedicated
//! target avoids feature-thrashing the outer integration-test build and is
//! reused by the spin matrix. The override drives a specific artifact.
//!
//! THE TESTS LIVE IN `measuring` (2026-09-23). Every row times frames on a live window,
//! so a busy machine can turn one red without the code changing.
//! The merge gate runs every test named `measuring::…` ALONE, in its exclusive
//! `measuring tests` stage, and skips them in the parallel test run
//! (`aterm_verify::stages::MEASURING_TESTS`); `targo test` by hand still runs them.
#![cfg(target_os = "macos")]

mod support;

#[path = "paint/measuring.rs"]
mod measuring;
