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
//! WIRING: the pre-push gate covers this matrix through the `guards` lane of
//! `xtask gate lint` (which `.githooks/pre-push` runs on every push) —
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
//! `sips`, so elsewhere the suite compiles to one loud not-run notice instead
//! of a silent green (see `paint_matrix_is_a_macos_only_lane` below).
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

#[cfg(target_os = "macos")]
mod support;

/// The honest answer everywhere the lane cannot run. NOT a pass in disguise:
/// it prints exactly what was not proven, so a green run on Linux/Windows can
/// never be read as "the paint matrix held" — it says out loud that nothing
/// about paint was measured here.
#[cfg(not(target_os = "macos"))]
#[test]
fn paint_matrix_is_a_macos_only_lane() {
    eprintln!(
        "paint: NOT RUN — the paint-conformance matrix is a macOS-only lane (the scanner \
         decodes recorded frames via `sips`, and the artifact under judgment is the macOS \
         bundle's binary). NOTHING about effect paint was proven on this platform; the matrix \
         runs on the mac that cuts releases, via tools/paint_guard.sh and the release smoke."
    );
}

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::sync::Mutex;

/// The workspace root, from this crate's own manifest dir — the probe script
/// and the release binary are both addressed from it.
#[cfg(target_os = "macos")]
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-conformance has a workspace root two levels up")
        .to_path_buf()
}

/// The bitmap morphology and continuity classifiers have their own negative
/// controls. Keep this beside the artifact rows so a future threshold edit
/// cannot silently let a one-cell cursor impersonate a whole kitty, or erase
/// an interior two-frame blackout from the score.
#[cfg(target_os = "macos")]
#[test]
fn paint_scanner_semantic_classifiers_keep_their_negative_controls() {
    let dir = workspace_root().join("tools/paint-conformance");
    let out = Command::new("/usr/bin/python3")
        .args(["-m", "unittest", "-v", "scan_test.py"])
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir(&dir)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", dir.join("scan_test.py").display()));
    assert!(
        out.status.success(),
        "paint scanner semantic self-tests failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// THE MATURE RUN — the release smoke's own 29 keys, shared by every typed
/// trail row since the 2026-08-30 key-length recalibration.
///
/// The shipped traverse is PER-MARK (`RAINBOW_TRAVERSE_MIN_CELLS = 26` in
/// crates/aterm-effects/src/cursor_glow.rs): the arc spreads over the mark
/// being typed, so a 10-key mark spans ~2-3 of 12 hue buckets BY DESIGN and
/// the historical 10-11-key rows sat exactly on the `union_hues >= 4` /
/// `ribbon_window_hues >= 4` cliff — the same cliff the release smoke was
/// moved off on 2026-08-30 (crates/aterm-release/src/publish.rs: "a 10-key
/// take hovers exactly at the claim's `ribbon_hue_bands >= 4` boundary").
/// MEASURED on the shipped binary, 2026-08-30, before the move: row 5 read
/// union_hues=3 two runs in three (a per-launch hue-phase coin flip, total_ink
/// 2642 in the 3-bucket mode vs 7104 in the 5-bucket one), rows 1/2/4 read
/// union_hues=2-3 deterministically, row 6b ribbon_window_hues=3 two in
/// three. The SAME shapes at these 29 keys read union_hues=10-11 with
/// ribbon_window_hues=10 and ribbon_bound=42 — past the clamp floor the arc
/// actually completes, and the floors sit under it with real margin instead
/// of straddling a launch phase. No spaces: a space ends the mark, and the
/// row audits the design at the length its arc completes.
#[cfg(target_os = "macos")]
const MATURE_RUN: &str = "t,h,e,r,a,i,n,b,o,w,k,i,t,t,y,p,a,i,n,t,s,t,h,e,a,r,c,o,k";

/// What one matrix row expects of its take.
#[cfg(target_os = "macos")]
enum Expect {
    /// Effect ink present: ≥150 dynamic saturated px across the take, ≥4 of
    /// 12 hue buckets, a visible clump in some single frame, repeated
    /// ribbon-shaped geometry with no interior multi-frame blackout, and — on
    /// a paired take — NOT ONE frame where the engine claims a ribbon IT IS
    /// STILL DRIVING over a raster without that geometry. The claim itself must
    /// cover at least two frames.
    ///
    /// Two things make that a bind rather than a coin flip, and both were
    /// measured on row 6b (2026-08-29):
    ///
    /// * WHAT COUNTS AS A RIBBON. Saturated changed pixels must form either a
    ///   thin detached component or a dense horizontal run attached to the
    ///   cursor in X. A mature paired claim may retain a pale attached run, but
    ///   cannot turn a detached gray glyph/rule into a ribbon. Hue separation is
    ///   diagnostic only and cannot grant presence.
    /// * WHEN THE CLAIM IS HELD. `ribbon_hue_bands` counts DISTINCT HUE BANDS
    ///   among the live typing sparks, quantized to twentieths of a turn — not
    ///   the number of sparks, which is the separate `ribbon_segments`. A spark
    ///   keeps the hue it was laid in for life, so the count falls only as
    ///   sparks EXPIRE and keeps claiming right through the authored ~2 s
    ///   exhale, after the ribbon's colour has legitimately left the raster
    ///   (see `driven_last_frame`). The claim is therefore held only while the
    ///   engine's own ledger still reports the ribbon being driven.
    ///
    /// The contradiction count is NOT sliced to the raster-derived
    /// `sustained_rainbow_bounds` any more: a ribbon that dies mid-take ended
    /// those bounds at its own last lit frame and walked out of its own audit
    /// window. Injected on a healthy take (blackout from frame 12 to the end,
    /// engine still claiming and still driving), that shape scored 0
    /// contradictions and PASSED under the slice — `ribbon_dark=0
    /// ribbon_bound=18 rainbow_frames=11 rainbow_gap=0 total_ink=3257`; unsliced
    /// it charges 13 and fails.
    ///
    /// The two-frame floor is an obligation on the EVIDENCE and rides on this
    /// arm alone, deliberately outside the predicate [`Expect::Quiet`] negates.
    Ink,
    /// The dark control: ZERO dynamic saturated px in EVERY frame. Only valid
    /// for a style with NO RESIDENT COMPANION — see [`Expect::TrailDark`].
    Dark,
    /// ZERO TRAIL, resident companion allowed: no rainbow frame and no claimed
    /// ribbon, however many pixels the style's resident animal legitimately
    /// paints.
    ///
    /// This exists because `rainbow kitty` now carries the RESIDENT PET, and a
    /// resident is on glass at rest by definition — so `Dark` (literally zero
    /// lit pixels) became unsatisfiable for the owner's own spelling and read a
    /// legitimately drawn cat as a cold-output LEAK. Measured on one binary and
    /// one shape, the two are cleanly separable:
    ///   --style flying  (no resident)  total_ink=0                    Dark PASS
    ///   --style classic (resident pet) total_ink=40, ribbon_claimed=0,
    ///                                  rainbow_frames=0          TrailDark PASS
    /// The cold-output law is about LIGHT THE PROGRAM EARNED, and a resident
    /// companion earns nothing — it is simply present. Weakening `Dark` itself
    /// would have thrown away the absolute-zero statement for every style that
    /// can still make it; this keeps both claims instead of trading one away.
    TrailDark,
    /// The control for a path that carries an unavoidable cursor-cell
    /// artifact: the take FAILS the exact ink predicate, read the other way.
    /// See the probe's `--expect quiet` note for the
    /// measurements; on the unpinned path the deciding terms are take-level
    /// colour identity and ribbon geometry, not raw cursor/text pixel count.
    ///
    /// What is negated is the PAINT predicate only. [`Expect::Ink`]'s
    /// `ribbon_bound >= 2` floor is not part of it, and must not be: this row's
    /// own take books no driven mature ribbon (`ribbon_bound=0`, measured on 3
    /// of 3 live runs), so folding the floor in would let the control go green
    /// for want of a ledger entry on a take that painted.
    Quiet,
    /// Matched typed negative control: cursor/text deltas must be present, but
    /// the canonical off style must produce no ribbon witness and keep every
    /// effect ledger quiet.
    EffectOff,
}

/// Drive one shape through the probe on the ORIGINAL (recorded) capture path —
/// rows 1-4. See [`Capture::PinnedVideo`] for what that path perturbs, and
/// [`probe_with`] for the rest.
#[cfg(target_os = "macos")]
fn probe(shape: &str, keys: &str, expect: Expect) {
    // TRAIL rows run on the COMPANION-FREE head, not on `classic`.
    //
    // `classic` now carries a resident animal. `flying` exercises the same trail
    // without that independent bitmap, so these rows isolate ribbon continuity.
    // Rows that judge a resident pin their own style and bind its status to
    // companion morphology separately.
    probe_styled(shape, keys, expect, Capture::PinnedVideo, "flying");
}

/// [`probe`] with the style named outright.
#[cfg(target_os = "macos")]
fn probe_styled(shape: &str, keys: &str, expect: Expect, capture: Capture, style: &str) {
    probe_with_companion_style(shape, keys, expect, capture, None, Some(style));
}

/// How the take is captured — and therefore WHAT THE INSTRUMENT PERTURBS.
/// See the observer-rule note at the top of this file.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum Capture {
    /// `ctl video` — the recording PINS `motion_focus` for the recorded
    /// window. Fine for proving a focused window paints; structurally unable
    /// to catch a motion-gate regression, because recording repairs it.
    PinnedVideo,
    /// An unpinned image burst which leaves the headless window's initial
    /// focused state intact. Used by the companion rows: they are proving the
    /// presentation, not the unfocused wake seam.
    UnpinnedFocused,
    /// A burst of `ctl image` captures with the window driven UNFOCUSED
    /// (`ctl focus out`). Nothing about the motion gate is touched: this is
    /// the arm that can go red.
    UnpinnedUnfocused,
}

/// A companion whose engine state and captured bitmap are both obligations.
///
/// Each variant carries its STYLE ARM as well as its animal, because after the
/// 2026-08-26 rename the two are not independent: `rainbow kitty` and
/// `rainbow kitty pet` both name the resident, and only the explicit
/// `rainbow kitty flying` earns a flypast.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum Companion {
    /// The shipped-default full-body resident cat, present from frame zero
    /// (`--style default`: the config key is left ABSENT).
    Pet,
    /// The SAME resident, selected by the owner's literal config line
    /// `cursor_trail_style = "rainbow kitty"` (`--style classic`). A separate
    /// row from [`Companion::Pet`] on purpose — see row 10.
    PetOwnerSpelling,
    /// The old flying head, earned by a sustained typing run, behind its
    /// explicit opt-in spelling (`--style flying`).
    Cat,
}

/// Drive one shape through `tools/paint-conformance/paint_probe.sh` on the
/// given capture path and assert its verdict.
///
/// Serialized: each case launches its own headless instance and captures from
/// it, and two concurrent takes would contend for frames and CPU — a paint
/// gate must never fail because of its own harness load.
#[cfg(target_os = "macos")]
fn probe_with(shape: &str, keys: &str, expect: Expect, capture: Capture) {
    probe_with_companion_style(shape, keys, expect, capture, None, Some("flying"));
}

/// A row that pins an explicit `--style` with NO companion obligation — the
/// absolute-zero cold control needs a companion-free style (`flying`) to make a
/// literal-zero claim that a resident-pet style no longer can.
#[cfg(target_os = "macos")]
fn probe_with_style(shape: &str, keys: &str, expect: Expect, capture: Capture, style: &str) {
    probe_with_companion_style(shape, keys, expect, capture, None, Some(style));
}

#[cfg(target_os = "macos")]
fn probe_with_companion(
    shape: &str,
    keys: &str,
    expect: Expect,
    capture: Capture,
    companion: Option<Companion>,
) {
    probe_with_companion_style(shape, keys, expect, capture, companion, None);
}

#[cfg(target_os = "macos")]
fn probe_with_companion_style(
    shape: &str,
    keys: &str,
    expect: Expect,
    capture: Capture,
    companion: Option<Companion>,
    style: Option<&str>,
) {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _take_turns = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let root = workspace_root();
    let bin = support::release_bin(&root, &["ATERM_PAINT_BIN"]);
    let script = root.join("tools/paint-conformance/paint_probe.sh");
    assert!(script.is_file(), "{} is missing", script.display());

    let (expect_arg, what) = match &expect {
        Expect::Ink => ("ink", "effect ink present"),
        Expect::Dark => ("dark", "zero effect ink in every frame"),
        Expect::TrailDark => (
            "trail-dark",
            "zero trail (no rainbow frame, no claimed ribbon) — a resident companion may paint",
        ),
        Expect::Quiet => (
            "quiet",
            "a take that the ink gate itself would NOT score as effect ink",
        ),
        Expect::EffectOff => (
            "effect-off",
            "typed cursor/text pixels but no ribbon from the canonical off style",
        ),
    };
    let mut cmd = Command::new("/bin/bash");
    cmd.arg(&script)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    cmd.arg(&bin)
        .args(["--shape", shape, "--record", "5", "--expect", expect_arg])
        .args(["--min-ink", "150", "--min-hues", "4", "--budget", "180"])
        // THE BACKEND FENCE, stated here rather than inherited from the probe's
        // default. This matrix exists to pixel-check the SHIPPED artifact, and
        // the shipped artifact renders on the GPU: `App::ensure_pixel_backend`
        // used to fall back to the CPU renderer silently (into `gui.log`, which
        // the probe dumps only when the socket never appears), so a GPU backend
        // that failed to initialize AT ALL produced a fully green matrix drawn
        // entirely on the CPU. Every row was insured against the failure of the
        // thing it exists to judge. The probe now turns that into a
        // COULD-NOT-RUN, which this file already treats as "the probe decided
        // nothing, which is not a pass".
        .args(["--backend", "gpu"]);
    match capture {
        Capture::PinnedVideo => {}
        Capture::UnpinnedFocused => {
            cmd.args(["--capture", "image"]);
        }
        Capture::UnpinnedUnfocused => {
            cmd.args(["--capture", "image", "--focus", "out"]);
        }
    }
    match companion {
        None => {}
        Some(Companion::Pet) => {
            cmd.args(["--style", "default", "--companion", "pet"]);
        }
        Some(Companion::PetOwnerSpelling) => {
            cmd.args(["--style", "classic", "--companion", "pet"]);
        }
        Some(Companion::Cat) => {
            cmd.args(["--style", "flying", "--companion", "cat"]);
        }
    }
    // An explicitly pinned style is only meaningful when no companion arm
    // already selected one; the companion arms own their style by construction.
    if let Some(style) = style
        && companion.is_none()
    {
        cmd.args(["--style", style]);
    }
    if !keys.is_empty() {
        cmd.args(["--keys", keys]);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("could not spawn {}: {e}", script.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let report = stdout
        .lines()
        .rev()
        .find(|l| l.starts_with("PAINT"))
        .unwrap_or("<no PAINT report line>")
        .to_string();
    // What a RED on this row actually means. Rows 1-4 and rows 5-6 fail for
    // different reasons and send a reader to different code, so they say so
    // rather than sharing one message that fits neither.
    let meaning = match (&expect, capture) {
        (Expect::EffectOff, _) => {
            "the canonical `off` style leaked trail geometry, or the scanner attributed unrelated pixels to a ribbon — inspect the reported geometry and the `off` style resolution"
        }
        (_, Capture::PinnedVideo) => {
            "the shipped binary does not paint its flagship effect at all — this row's window              was FOCUSED and RECORDED, i.e. every excuse was already granted"
        }
        (_, Capture::UnpinnedFocused) => {
            "the engine and captured pixels disagree about the cursor companion, or the              rainbow contains an interior blackout — this row pairs every status read with its              exact unpinned frame and generic rainbow pixels cannot satisfy the animal obligation"
        }
        (_, Capture::UnpinnedUnfocused) => {
            "an UNFOCUSED window being typed into does not paint its trail — the v0.48-v0.50              blackout class, back. Look at `App::cursor_fx_focus`'s typed-wake term and the              W11b demotion it exists to override; note that `ctl video` would have HIDDEN this              (its recording pin un-suppresses the same gate), which is why this row does not              use one"
        }
    };
    match out.status.code() {
        Some(0) => eprintln!("paint[{shape}]: {report}"),
        Some(1) => panic!(
            "PAINT CONFORMANCE FAILED [{shape}]: expected {what}, and the take says otherwise — \
             {meaning} (docs/RELEASE-PROOF-DISCIPLINE.md).\n  {report}\n\
             --- probe stderr ---\n{stderr}"
        ),
        Some(2) => panic!(
            "PAINT CONFORMANCE COULD NOT RUN [{shape}]: the probe decided nothing, which is not \
             a pass.\n  {report}\n--- probe stderr ---\n{stderr}"
        ),
        code => panic!(
            "paint probe [{shape}] died abnormally (exit {code:?}, the protocol is 0/1/2)\n\
             --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ),
    }
}

/// Matrix row 1: a human typing at a main-screen /bin/sh prompt leaves trail ink.
#[cfg(target_os = "macos")]
#[test]
fn main_screen_prompt_typing_paints_trail_ink() {
    probe("prompt", MATURE_RUN, Expect::Ink);
}

/// Row 1's matched negative control: identical prompt, keys, focus and video
/// path, with only the effect style changed. It must retain real dynamic cursor
/// pixels while producing no ribbon witness, so the scanner cannot pass row 1
/// merely by counting cursor motion or echoed text.
#[cfg(target_os = "macos")]
#[test]
fn main_screen_prompt_with_effect_off_has_no_ribbon() {
    probe_with_style(
        "prompt",
        "e,c,h,o,space,h,e,l,l,o",
        Expect::EffectOff,
        Capture::PinnedVideo,
        "off",
    );
}

/// Matrix row 2: typing into the fake-Claude alt-screen shape (DEC-2026
/// bracketed repaints + DECTCEM hide + 150ms spinner + per-key echo) leaves
/// trail ink — the exact shape both dark releases were never proven against.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_fake_claude_typing_paints_trail_ink() {
    probe("fake-claude", MATURE_RUN, Expect::Ink);
}

/// Matrix row 3, the dark control: an alt-screen spinner repainting on its own
/// with NOTHING typed earns zero effect ink in every frame. This row is what
/// makes the other rows falsifiable — a scanner that sees ink here is
/// measuring noise, not the effect.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_cold_spinner_paints_zero_ink() {
    probe("cold-spinner", "", Expect::TrailDark);
}

/// Matrix row 4: typing beside an ESC7/ESC8 token streamer (unowned batches
/// landing away from the caret several times a second) leaves trail ink.
/// Verified live 2026-08-24 against HEAD 7cbf4651 ("an unowned batch retires
/// what it invalidates"): the no-row-probe fence fix holds under this shape.
///
/// CONTINUITY IS JUDGED IN TIME, NOT FRAMES (2026-08-26). This row used to
/// assert `rainbow_gap <= 1` — the longest run of non-rainbow FRAMES anywhere
/// in the whole five-second recording. That term was flaky and blind at the
/// same time, and both halves are measured, one binary, one shape, capture the
/// only variable:
///
///   arm                     rainbow_gap (old)     driven_dark_us (new)
///   six HEALTHY takes       0, 0, 4, 25, 26, 26   0 every time
///   effect OFF, two takes   0, 0                  1_632_392 / 1_771_652
///
/// FLAKY: four of six healthy takes went red. The gap was never a blackout —
/// it was the exhale `RAINBOW_DISP_RELEASE_TAU` deliberately authors ("the
/// rainbow EXHALES after the last key ... melts over ~2 s of visible
/// dimming"). After the last keystroke the ribbon dims below the scanner's
/// hue-separation bar for a few hundred ms until the streamer's next unowned
/// batch re-lights it. Frames 130 and 145 of a red take were pulled and
/// looked at: 130 carries a faint multi-hue underline under `hello world`,
/// 145 the same underline decayed to fewer hues. Nothing blacked out.
///
/// BLIND: with the effect switched OFF entirely (an unknown
/// `cursor_trail_style`, which at the time resolved Unknown and disabled the
/// effect — since 2026-08-29 it falls back to the default, so a blind control
/// spells `off`), the same term reads ZERO — a take with no sustained two-frame rainbow anywhere
/// gives `sustained_rainbow_bounds` nothing to bound, so the gap is vacuously
/// zero. The term read 0 on a dead effect and 26 on a live one. Raising the
/// threshold could only have traded one blindness for more, which is why the
/// unit changed instead.
///
/// NOT a capture artefact, checked before anything was rewritten: the video
/// tap records every SUBMITTED frame, and the index proves it — `seq` strictly
/// contiguous, `ring_skipped=0`, `evicted_frames=0`, and inside every dark run
/// the inter-frame delta sat at the nominal 17.8 ms. The darkness was really
/// on glass; it simply was not a defect.
///
/// WHAT THE ROW MEASURES NOW: `ctl video ... keys` stamps every keystroke into
/// `index.json`'s `inputs[]` on the SAME clock as `frames[].t_us`, so the take
/// carries the interval in which the trail is actually being DRIVEN. Inside
/// that window (plus a 150 ms grace for the stroke in flight) the scan reports
/// `driven_dark_us`, the longest blackout in wall-clock time, charging only
/// frames actually SAMPLED dark — first-dark to last-dark plus one nominal
/// interval, never from the preceding lit frame. So a render stall cannot
/// manufacture a blackout: the healthy takes measured 0 with sampling holes up
/// to 86 ms, and `sampling_hole_us` is reported beside the verdict so a coarse
/// capture is visible rather than silently deciding the row.
///
/// THE BOUND IS PROVEN IN BOTH DIRECTIONS. Blackouts injected into a healthy
/// take (frames inside the window overwritten with the baseline) score exactly
/// N x the nominal interval: 1 frame 17_683 us, 2 frames 35_087, 3 frames
/// 52_608. The bound is 1.5 nominal intervals (26_524 us here) — ONE dark
/// sampled frame is tolerated as capture jitter, TWO is a regression. That is
/// strictly tighter than the `rainbow_gap <= 1` it replaces, while being
/// immune to the exhale and to cadence drift.
///
/// Image-burst rows carry no recording and no `inputs[]`, so they report
/// `driven_dark_us=-1` and keep the old whole-take frame gap, which is all
/// such a take can support.
///
/// THE CONTINUITY FLAG MOVED TO LITNESS (2026-08-30, the litness
/// recalibration's video half — see [`MATURE_RUN`] for the key-length side,
/// and the notes in scan.py's `driven_window_darkness` for the machinery).
/// The per-frame HUE witness that used to feed `driven_dark_us` reads the
/// authored young band (~1.2 s of one or two hues after the first keystroke
/// — real, lit, driven ink) as darkness, and flickers sub-bar mid-take on
/// healthy glass under the per-mark traverse. MEASURED on the shipped
/// binary: a structurally healthy 29-key take (union_hues=11, one 113-frame
/// lit run) charged driven_dark_us=1,250,924 against the 26.5k bound — red
/// at ANY key length — and three consecutive healthy takes charged 53-70 ms
/// interior "blackouts" on lit glass. The term now reads band POPULATION
/// (flat saturated ink, the paired bind's own litness: >= 22 flat px from
/// the first keystroke's glass arrival to the take's end on the preserved
/// healthy take, while a blanked layer counts 0, a grey trail is not
/// saturated, and the caret block stands in tall columns), opens the charge
/// window at the pixels' first sustained lit frame (the pre-glass lead is
/// excused and reported as `mature_lead_us`; a take with no sustained
/// litness exempts nothing), and hands the close's first 0.4 s to the
/// authored exhale (`EXHALE_CLOSE_ALLOWANCE_US` — the video mirror of the
/// paired takes' close guard). The excused lead is itself BOUNDED at the
/// probe's verdict (`MATURE_LEAD_MAX_US`, 500 ms — healthy takes measure
/// 120-141 ms), so a dead start can never hide inside the excusal. The
/// ARC promise stays at take level, on `union_hues`, exactly where the
/// paired rows hold it.
///
/// REPLAYED both ways on a preserved healthy 29-key fake-claude take
/// (2026-08-30, shipped binary): untouched, driven_dark_us=0; the SAME
/// pixels with three interior driven frames blanked to the baseline charge
/// 52,089 us — 3 x the take's 17,672 us nominal, double the bound — so the
/// fence-defect class this term exists for still fires through the rescope.
///
/// THE PEER WAVE FIXED THE SAME BLINDNESS FROM THE GEOMETRY SIDE the same
/// day (its measured account follows, kept as history), and the merge fused
/// the two: the continuity flag is `scan.driven_lit` — the band-population
/// litness above, now counted over `band_ink_metrics`' object domain at the
/// RIBBON_LIT_MIN_PX=7 floor, ORed with the ribbon-geometry arm so no frame
/// either wave read as lit can go dark. The reconciliation measurements live
/// at `driven_lit` in scan.py.
///
/// WHAT COUNTS AS LIT IN THAT WINDOW MOVED, 2026-08-30 (`scan.driven_lit`).
/// The witness used to be ribbon-shaped GEOMETRY, whose thin arm caps a
/// component at four pixels tall because it was calibrated on the explicit
/// underline highlighter. These rows pin `--style flying`, i.e.
/// `ribbon_look=tall` — the banded body the letters sit inside — and under the
/// per-mark classic-rainbow traverse a fresh mark lights that whole body: one
/// connected object 7-10 px wide and 6-8 px TALL, 32-55 saturated changed
/// pixels, its arc visibly unrolling frame to frame. Nothing was dark; the
/// instrument could not see it, and all three video ink rows charged the young
/// mark as a 245-420 ms interior blackout (`driven_dark_us` 262_648 / 263_482 /
/// 420_155 against a 26_5xx bound, deterministic across runs). Continuity now
/// reads the band's own flat-column population — `flat_ink >= 12`, the identical
/// term the claim-bound path has charged since the same day — ORed with the
/// geometry arm, so a frame can only move dark to lit. MEASURED on one take,
/// style the only variable: live `flying` holds flat_ink 31-59 across every
/// frame of the driven window and drops to 0 at the exhale; `off` holds
/// flat_ink 0 on all 11 frames while its caret block paints 98 px each time.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_esc7_esc8_streamer_typing_paints_trail_ink() {
    probe("streamer", MATURE_RUN, Expect::Ink);
}

/// Matrix row 5 — THE ONE ROW THAT CAN GO RED. An UNFOCUSED window
/// (`ctl focus out` → the same `App::on_focus` a winit `Focused(false)` calls)
/// being typed into over the control socket, captured by an UNPINNED burst of
/// `ctl image` renders that leaves `motion_focus` exactly as the un-observed
/// app has it. This is the owner's real shape and the v0.48-v0.50 blackout's
/// exact condition; see the observer-rule note at the top of this file.
///
/// MEASURED 2026-08-24, both arms, RELEASE profile, headless 584x350:
///   healthy HEAD                 frames=29 total_ink=1553 union_hues=9  PASS
///   typed-wake term removed      frames=29 total_ink=1470 union_hues=1  FAIL
/// The pixel COUNT barely moves (an unfocused typed window still drags a
/// saturated block cursor and echoes glyphs); the HUE SPREAD is the witness,
/// and `--min-hues 4` sits between 1 and 9 with margin on both sides.
///
/// Those numbers are the 10-key fixed-traverse era's. At [`MATURE_RUN`]'s 29
/// keys on the shipped per-mark binary (2026-08-30) the healthy arm reads
/// frames=67 total_ink=9,736 union_hues=11 ribbon_window_hues=11 — the same
/// shape at 10 keys had become a per-launch hue-phase coin flip (union_hues
/// 3 two runs in three, the exact cliff the const documents), while the
/// broken arm's signature is unchanged: an unfocused window whose typed-wake
/// term is gone still collapses to ONE hue bucket however many keys land, so
/// the floor's separation only widened.
#[cfg(target_os = "macos")]
#[test]
fn unfocused_typed_window_paints_its_trail_under_an_unpinned_capture() {
    probe_with(
        "fake-claude",
        MATURE_RUN,
        Expect::Ink,
        Capture::UnpinnedUnfocused,
    );
}

/// Row 5's same-shape effect-off twin on the unpinned, unfocused capture path.
#[cfg(target_os = "macos")]
#[test]
fn unfocused_typed_window_with_effect_off_has_no_ribbon() {
    probe_with_style(
        "fake-claude",
        "r,a,i,n,b,o,w,space,o,n",
        Expect::EffectOff,
        Capture::UnpinnedUnfocused,
        "off",
    );
}

/// Matrix row 6, row 5's control: the SAME unfocused window under the SAME
/// unpinned capture, with NOTHING typed, is a take the ink gate would not
/// score as ink. Without it row 5 could be satisfied by a scanner that counts
/// the client's own repaints; with it, row 5's 9 hue buckets (11 at
/// [`MATURE_RUN`]'s 29 keys) are provably the typed trail and nothing else. It also pins the other half of the contract
/// the typed wake must not break — an IDLE unfocused window still demotes
/// (W11b), so the fix bought the trail back without turning background
/// decoration on.
///
/// `Expect::Quiet`, not `Expect::Dark`, and the reason is measured rather than
/// assumed. The client hides its caret (DECTCEM) inside every DEC-2026
/// bracket, so an `image` capture landing mid-bracket sees the cursor cell
/// differ from frame 0 — exactly 98 px, one 7x14 cell, in one or two frames of
/// ~25, varying run to run: `total_ink` came back 0, 0, 0, 98, 98, 196, 196
/// over seven runs of this shape on 2026-08-24. `total_ink == 0` would flake
/// one run in three, and a raw ink ceiling is no better (196 straddles the 150
/// floor). The trail's signature is HUE SPREAD, and that separation is not
/// close: 1 bucket here, 9 in row 5.
///
/// NOT VACUOUS, checked: pointed at row 5's typed shape, `--expect quiet`
/// FAILS (`total_ink=577 union_hues=8`).
#[cfg(target_os = "macos")]
#[test]
fn unfocused_idle_window_paints_nothing_under_an_unpinned_capture() {
    probe_with(
        "cold-spinner",
        "",
        Expect::Quiet,
        Capture::UnpinnedUnfocused,
    );
}

/// Matrix rows 6a-6c: THE OWNER'S ACTUAL CONFIGURATION — a FOCUSED window,
/// captured WITHOUT a recording pin. This is the hole every previous trail
/// blackout fell through, and it is worth stating plainly because the shape of
/// the gap is the whole lesson.
///
/// Before these rows, `UnpinnedFocused` existed but was spent entirely on the
/// COMPANION rows (7 and 8), which assert that a cat or a pet is *present*.
/// Every row that asserted trail INK was either PINNED (rows 1-4, where
/// `ctl video` holds `motion_focus` open for the recorded window and therefore
/// repairs the very gate under test) or UNFOCUSED (rows 5-6, which exercise
/// the typed-wake path instead). The one combination nobody measured is the
/// one every user is actually in: focused, unrecorded, typing into a TUI that
/// repaints on its own.
///
/// MEASURED 2026-08-25 against HEAD, `fake-claude`, identical keys and
/// thresholds, the ONLY variable being the capture:
///   pinned `ctl video`   total_ink=5515  rainbow_run=98  PASS
///   unpinned `ctl image`  total_ink=266   rainbow_run=5   FAIL (best_ink=38)
/// A 14-20x ink collapse attributable to nothing but the instrument. The
/// paired status rows named the mechanism: of ten keys typed, four confirmed
/// and six retired `motion-downgrade`, with `focused=true motion_stage=full`
/// throughout — the multi-batch generation fence condemning honest keystrokes
/// because a concurrent spinner batch landed alongside the echo. A continuous
/// recording cadence observes every generation singly, so the fence never
/// trips and the row goes green over a stunted ribbon.
///
/// These rows are the falsification. Row 6c is their dark control: without it
/// a scanner that simply counted cursor pixels could satisfy 6a and 6b.
#[cfg(target_os = "macos")]
#[test]
fn focused_alt_screen_typing_paints_trail_ink_without_a_recording_pin() {
    probe_with(
        "fake-claude",
        MATURE_RUN,
        Expect::Ink,
        Capture::UnpinnedFocused,
    );
}

/// Row 6a's matched effect-off twin on the unpinned focused capture path.
#[cfg(target_os = "macos")]
#[test]
fn focused_typed_window_with_effect_off_has_no_ribbon() {
    probe_with_style(
        "fake-claude",
        "r,a,i,n,b,o,w,space,o,n",
        Expect::EffectOff,
        Capture::UnpinnedFocused,
        "off",
    );
}

/// Row 6b: the same law beside an ESC7/ESC8 token streamer — unowned batches
/// landing away from the caret several times a second, which is precisely the
/// concurrent-decoration traffic that makes the batch counter a bad proxy.
///
/// THIS ROW CARRIES THE RIBBON BIND. A mature engine claim is charged only
/// through the last frame the engine reports as driven; the authored exhale is
/// outside that domain. Inside it, presence comes from changed-pixel ribbon
/// geometry (thin detached component or cursor-attached X-span), not hue spread.
/// The contradiction tally is deliberately unsliced: raster-derived sustained
/// bounds once let a ribbon that died from frame 12 onward end its own audit
/// window and pass with `ribbon_dark=0`; the full driven window charges 13.
/// `ribbon_bound >= 2` keeps a zero contradiction count non-vacuous.
#[cfg(target_os = "macos")]
#[test]
fn focused_streamer_typing_paints_trail_ink_without_a_recording_pin() {
    probe_with(
        "streamer",
        MATURE_RUN,
        Expect::Ink,
        Capture::UnpinnedFocused,
    );
}

/// Row 6c, the dark control for 6a/6b: a FOCUSED alt-screen spinner repainting
/// on its own with NOTHING typed earns zero effect ink under the same unpinned
/// capture. Cold program output must stay dark no matter how many batches it
/// lands — the concurrent-decoration law relaxes a BATCH COUNT, never the
/// requirement that a real keystroke armed the candidate.
#[cfg(target_os = "macos")]
#[test]
fn focused_cold_spinner_paints_zero_ink_without_a_recording_pin() {
    probe_with(
        "cold-spinner",
        "",
        Expect::TrailDark,
        Capture::UnpinnedFocused,
    );
}

/// Matrix row 7: THE SHIPPED DEFAULT, not the classic style the historical
/// rows pin. The resident pet owes a full-body bitmap in the very first
/// requested still (before typing has created any generic rainbow ink), stays
/// present through every paired status/frame, and rides a continuous rainbow
/// while a human-sized word is typed.
#[cfg(target_os = "macos")]
#[test]
fn shipped_default_first_still_and_typing_carry_the_resident_pet() {
    probe_with_companion(
        "prompt",
        "r,a,i,n,b,o,w,k,i,t,t,y",
        Expect::Ink,
        Capture::UnpinnedFocused,
        Some(Companion::Pet),
    );
}

/// The explicit underline spelling must travel through the shipped config
/// loader, resolve to the rainbow engine, report underline geometry, and paint
/// a real ribbon. Default and owner-shorthand rows now use the tall body, so
/// neither proves that this independently selectable alternate still works.
///
/// AT [`MATURE_RUN`]'s 29 KEYS, like every other typed ink row (2026-08-30,
/// the merge's recalibration of this upstream-new row). The row landed typing
/// 7 keys, and under the shipped per-mark traverse a 7-key mark's live sparks
/// span at most 3 hue bands BY DESIGN — MEASURED on a kept take: 23 paired
/// rows, `ribbon_hue_bands` 0/1/2/3 and never 4, so the mature-claim
/// threshold (`bands >= 4`) could not fire, `ribbon_bound=0`, and the
/// non-vacuity floor refused a take whose underline was painting perfectly
/// well (total_ink=1648, union_hues=9). That is the same stale-calibration
/// class that moved rows 1-6 to [`MATURE_RUN`]; at 29 keys the underline
/// matures its claim like the other image rows (measured 3x on the merged
/// tree: ribbon_claimed=50, ribbon_bound=43, ribbon_window_hues 5-7,
/// total_ink 8,393-20,387, union_hues 10, verdict PASS on every take).
#[cfg(target_os = "macos")]
#[test]
fn explicit_underline_spelling_paints_an_underline_ribbon() {
    probe_with_style(
        "prompt",
        MATURE_RUN,
        Expect::Ink,
        Capture::UnpinnedFocused,
        "underline",
    );
}

/// Pet-free reproduction of the short explicit-tall hue defect. Unlike the
/// default/literal rows, no companion can supply its four scanner buckets.
#[cfg(target_os = "macos")]
#[test]
fn explicit_tall_short_word_paints_four_hues_without_a_pet() {
    probe_with_style(
        "prompt",
        "r,a,i,n,b,o,w,k,i,t,t,y",
        Expect::Ink,
        Capture::UnpinnedFocused,
        "tall",
    );
}

/// Exact off twin: same prompt, keys, focus and unpinned capture.
#[cfg(target_os = "macos")]
#[test]
fn explicit_tall_short_word_control_has_no_ribbon_when_effect_is_off() {
    probe_with_style(
        "prompt",
        "r,a,i,n,b,o,w,k,i,t,t,y",
        Expect::EffectOff,
        Capture::UnpinnedFocused,
        "off",
    );
}

/// THE ABSOLUTE-ZERO CONTROL, kept alive after the resident pet landed.
///
/// The three cold rows now assert [`Expect::TrailDark`] because the owner's
/// `rainbow kitty` carries a RESIDENT companion, and a resident is on glass at
/// rest — literal zero is unsatisfiable for that spelling. That is a correct
/// relaxation for THAT style and a bad one to apply everywhere, because it
/// would leave nothing in the tree still making the strongest possible claim.
/// So this row makes it: a style with NO resident (`flying`), cold output, and
/// literally ZERO lit pixels in every frame.
///
/// MEASURED 2026-08-26, same binary, same shape, the only variable the style:
///   --style flying   total_ink=0                                  Dark PASS
///   --style classic  total_ink=40 ribbon_claimed=0 rainbow_frames=0
/// If a cold-output leak ever reappears, this row goes red on pixel one.
#[cfg(target_os = "macos")]
#[test]
fn cold_output_paints_literally_nothing_for_a_companion_free_style() {
    probe_with_style(
        "cold-spinner",
        "",
        Expect::Dark,
        Capture::UnpinnedFocused,
        "flying",
    );
}

/// Matrix row 8: the old flying kitty is rare by design and needs a sustained
/// high-band run. Four repetitions are 48 forward keys at the image probe's
/// 80ms cadence — comfortably past its ~2.6s earn law. The row requires at
/// least four `cat_active` captures and three consecutive isolated final
/// stills with a whole-head bitmap. Generic rainbow pixels alone satisfy none
/// of those terms.
///
/// SINCE 2026-08-26 THIS ROW DRIVES `cursor_trail_style = "rainbow kitty
/// flying"`, not `rainbow kitty` — the head's explicit opt-in. It is the proof
/// that widening the pet predicate REROUTED the head rather than deleting it:
/// the row's `pet_claimed == 0` term would go red the moment the escape hatch
/// stopped selecting the flypast, and row 10 holds the other side.
///
/// THE FINAL-STILL FLOOR IS THE RESTING HEAD'S (2026-08-30). The earned head
/// SETTLES as momentum drains: by the witness stills (typing over, plane
/// cleared, 1.05+ s quiet) it stands at its momentum-zero resting size, and
/// the strict 12x18 morphology floor read that plainly drawn head as MISSING
/// about one take in three — `cat_final_run=0 cat_missing=27` on takes whose
/// preserved stills visibly carry the whole head beside the caret. MEASURED
/// on six healthy takes of this row against the shipped binary (18 stills):
/// the head is present in EVERY still at 22-28 x 16-23 px, and the three
/// failing takes' stills all read exactly 16 px tall — a stable resting
/// size, sampled 16,17,17 across each take's stills, not a fade passing
/// through. scan.py's `CAT_STILL_MIN_H` (15) now admits the resting head on
/// the isolating stills alone, and only while the engine claims the cat;
/// mid-take frames keep the strict floor, a one-cell text/ribbon row is
/// structurally <= 14 px, and the caret's 7 px width fails the width floor —
/// the impostor pins in scan_test.py hold every one of those edges, and the
/// resting-head pin is red-proven against the pre-recalibration scanner.
#[cfg(target_os = "macos")]
#[test]
fn flying_style_sustained_typing_earns_and_paints_the_flying_kitty() {
    const RUN: &str = concat!(
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y",
    );
    probe_with_companion(
        "prompt",
        RUN,
        Expect::Ink,
        Capture::UnpinnedFocused,
        Some(Companion::Cat),
    );
}

/// Matrix row 9 — THE COLD ADVANCING CARET, the one shape only the freshness
/// LICENCE can keep dark (docs/design/EFFECTS-LICENSE-REDESIGN.md).
///
/// Rows 3 and 6 are already cold controls, but neither of them ever presents a
/// cold cursor DELTA: `cold-spinner` hides its caret inside every repaint and
/// parks it on the input row, and row 4's `streamer` restores the caret with
/// ESC 7 / ESC 8 so its resting position never moves (and is typed into
/// anyway). Both are dark whatever the gating law is. This row is the gap: an
/// append-shaped token stream, alt screen, caret SHOWN and never hidden, whose
/// RESTING POSITION WALKS several cells per batch, several times a second —
/// with NOTHING typed. That is exactly the light v0.43.0 could not refuse (its
/// tick spawned on every presented cursor delta, which is why a bare revert
/// re-opens `cat` sweeps and token streams), and exactly what a fresh-key-hint
/// licence refuses: no press, no fresh hint, nothing minted.
///
/// `Expect::Dark` — total_ink == 0, the strict reading, not row 6's negated
/// `quiet` — is affordable because the fixture desaturates the ONE known
/// non-effect emitter: a caret that walks paints ~98 dynamic saturated px per
/// moved cell in aterm's default `#50FA7B`, so the fixture sets it to the
/// foreground grey over OSC 12 before its first byte. Every authored effect
/// hue is untouched, which is what the red arm below measures.
///
/// NOT VACUOUS, and the probe refuses to guess: before it captures anything it
/// reads `ctl cursor` twice, 0.6 s apart, and turns the row into COULD-NOT-RUN
/// unless the caret is genuinely `visible=1` and genuinely at a different cell.
/// A fixture that failed to launch or stopped streaming cannot report a dark
/// green here.
///
/// PROVEN RED, 2026-08-25, red-first — BEFORE the licence seam exists. The
/// broken arm is a scratch build of THIS tree with `CursorGlow::spawn`'s
/// universal candidate gate neutered (`admitted_intent` becomes `Some(..)` for
/// every presented delta, i.e. v0.43.0's own spawn behaviour, which had no
/// candidate gate at all); the healthy arm is the same worktree restored and
/// rebuilt, so the two binaries differ in that term and nothing else.
///
///   candidate gate NEUTERED  frames=282 total_ink=20 best_ink=4 hues=1  FAIL
///   candidate gate NEUTERED  frames=283 total_ink=18 best_ink=4 hues=1  FAIL
///   HEAD, restored           frames=144 total_ink=0  best_ink=0 hues=0  PASS
///   HEAD, restored           frames=149 total_ink=0  best_ink=0 hues=0  PASS
///
/// The broken arm's ink is STRUCTURAL, not luck: a 2-4 px spark on every line
/// FOLD, landing at frames 11 / 57 / 104 / 149 / 193 / 238 / 282 of one take —
/// once per 0.72 s of stream, so any take long enough to fold carries it. The
/// healthy arm is exactly zero across every frame of a take that folds just as
/// often. That is the whole row: cold output walking the caret earns light
/// without a gate, and none with one.
///
/// (A first draft of the fixture streamed "…the lazy dog…" and the HEALTHY
/// build came back total_ink=84309 — `dog` is a Keyword-Kitty lexicon entry and
/// aterm had decorated the screen with animal sprites that then scrolled. Word
/// decoration is content-triggered and is not the light this row judges; the
/// fixture's alphabet is now nonsense and the probe switches `[sparkle_words]`
/// off for this shape. Recorded here because a future reader will otherwise
/// re-derive it the same expensive way.)
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_cold_token_streamer_with_a_walking_caret_paints_zero_ink() {
    probe("cold-streamer", "", Expect::TrailDark);
}

/// Matrix row 10 — THE OWNER'S LITERAL CONFIG LINE, added 2026-08-26 after the
/// owner said twice, against v0.60.0, "I STILL don't see the cursor kitty pet,
/// I see the old style kitty head".
///
/// Their `~/.config/aterm/aterm.toml` reads `cursor_trail_style = "rainbow
/// kitty"`. Row 7 proves the shipped DEFAULT carries the resident, and it was
/// green through every one of those releases — because the default spelling is
/// `rainbow kitty pet` and nobody's row ever drove the string the owner
/// actually has. That is the gap this row closes: it writes their exact line
/// and demands the same three companion obligations row 7 demands
/// (`pet_claimed == frames`, `cat_claimed == 0`, `pet_final_run >= 3`), so a
/// spelling that resolves to the wrong animal cannot be green here no matter
/// how healthy the rainbow underneath it is.
///
/// The pair is the whole point: this row and row 8 together say the rename
/// MOVED the flying head to `rainbow kitty flying` rather than dropping it, and
/// neither can be satisfied by the other's animal.
#[cfg(target_os = "macos")]
#[test]
fn the_owners_rainbow_kitty_spelling_carries_the_resident_pet() {
    probe_with_companion(
        "prompt",
        "r,a,i,n,b,o,w,k,i,t,t,y",
        Expect::Ink,
        Capture::UnpinnedFocused,
        Some(Companion::PetOwnerSpelling),
    );
}

/// Matrix row 11 — ONE COMPANION BODY PER FRAME, on the owner's own line.
///
/// Owner, 2026-08-21, on v0.60.0 with `cursor_trail_style = "rainbow kitty"`:
/// *"when there is a kitty head pet instead of the running kitty pet on the
/// cursor, FIX THE BUG where there are two overlapping kitties drawn sometimes
/// on effects!"* Two animals were on the glass at once.
///
/// WHY THE KEYS ARE ROW 8'S SUSTAINED RUN AND NOT ROW 10'S SINGLE WORD. Both
/// companions only have a claim on the same frame once the typing run is long
/// enough to earn the flypast — that is the exact window in which the resident
/// steps aside for the singing head, and the only window in which a second body
/// could ever be drawn. Row 10 types one word and never reaches it.
///
/// WHAT GOES RED. `companion_stack` counts isolating frames (the launch still
/// and the three final cleared-plane stills) carrying two or more
/// companion-shaped components, and this row — like rows 7, 8 and 10 — demands
/// zero. It is deliberately a PIXEL obligation: `cursor_cat.is_active() &&
/// cursor_pet.is_active()` is legal engine state that several unit tests pin on
/// purpose, so a `pet_claimed`/`cat_claimed` conjunction would be asking the
/// wrong question. Active is not drawn. The rest of the pet obligations ride
/// along unchanged, so the row cannot go green by losing the resident either.
#[cfg(target_os = "macos")]
#[test]
fn the_owners_spelling_never_draws_two_companions_in_one_frame() {
    const RUN: &str = concat!(
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y,",
        "r,a,i,n,b,o,w,k,i,t,t,y",
    );
    probe_with_companion(
        "prompt",
        RUN,
        Expect::Ink,
        Capture::UnpinnedFocused,
        Some(Companion::PetOwnerSpelling),
    );
}
