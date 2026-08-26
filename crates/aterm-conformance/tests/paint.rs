// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PAINT CONFORMANCE — the shape matrix that pixel-checks the shipped binary.
//!
//! Born from the 2026-08-24 blackout audit (docs/RELEASE-PROOF-DISCIPLINE.md):
//! v0.48.0 and v0.49.0 shipped the rainbow cursor trail dark past green gates
//! because every proof measured a different machine, screen or profile than the
//! owner runs, and nothing in CI or the cut ever pixel-checked a shipped
//! artifact. This suite closes the CI half: it launches the RELEASE-profile
//! `target/release/aterm` HEADLESS, drives real keystrokes through the control
//! socket, records the take with `ctl video … full pace`, and asserts on the
//! pixels — through `tools/paint-conformance/paint_probe.sh`, the same driver
//! and scanner the release cut's paint smoke uses, so the two gates cannot
//! drift apart.
//!
//! THE MATRIX (each row a test):
//!   1. main-screen /bin/sh prompt typing            → effect ink PRESENT
//!   2. alt-screen fake-Claude shape (DEC-2026 bracketed repaints + DECTCEM
//!      hide + 150ms spinner + per-key echo), typing → effect ink PRESENT
//!   3. alt-screen cold spinner, NOTHING typed       → ZERO ink in EVERY frame
//!   4. alt-screen ESC7/ESC8 token streamer, typing  → effect ink PRESENT
//!      (the "unowned batch" cadence that tripped the ownership fence; kept
//!      as a matrix row so that regression class stays dead)
//!   5. UNFOCUSED alt-screen fake-Claude, typed over the control socket,
//!      captured WITHOUT a recording                 → RAINBOW ink PRESENT
//!   6. UNFOCUSED alt-screen cold spinner, nothing typed, same unpinned
//!      capture                                      → NOT ink (row 5's control)
//!   7. shipped-default style, first still + typing   → resident PET in every
//!      frame, including frame zero, AND rainbow ink with no interior blackout
//!   8. classic flying-kitty style, sustained typing → kitty is EARNED and its
//!      whole-head bitmap survives into three isolated final stills
//!
//! Rows 7 and 8 close a different false-green class. The first six rows pin
//! `rainbow kitty` and accept generic dynamic rainbow pixels; they neither run
//! the shipped default (`rainbow kitty pet`) nor type long enough to earn the
//! classic flying cat. The companion rows pair every `ctl image` with the
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
//! The busiest SINGLE frame runs 98-136 px on every ink arm, which is why the
//! ≥150 floor gates the TAKE and not a frame — a per-frame floor of 150 would
//! be vacuously red on healthy builds.
//!
//! WIRING: the pre-push gate covers this matrix through the `guards` lane of
//! `xtask gate lint` (which `.githooks/pre-push` runs on every push) —
//! `tools/paint_guard.sh` re-runs `cargo test -p aterm-conformance --test
//! paint` whenever the paint-relevant trees (aterm-effects / aterm-render /
//! aterm-gui / this gate itself) differ from the last take it proved green.
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
//! The binary under test is `target/release/aterm` — the RELEASE profile, the
//! bits a cut would ship (audit rule 1: parts of the old proof ran debug
//! binaries). If it is stale this test rebuilds it (`cargo build --release -p
//! aterm`, once per run); `ATERM_PAINT_BIN=<path>` overrides for driving a
//! specific artifact.

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
use std::sync::{Mutex, OnceLock};

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

/// Resolve — and, once per test run, freshen — the RELEASE binary under test.
///
/// `cargo build --release -p aterm` is the staleness check: warm and unchanged
/// it is a no-op costing well under a second, and it is exactly what the
/// blackout audit demands (rule 1 — a release-claiming proof runs the RELEASE
/// profile, not whatever binary happens to be lying around). `ATERM_PAINT_BIN`
/// bypasses the build for callers that already hold the artifact to judge
/// (the release smoke drives the just-built bundle through the same probe).
#[cfg(target_os = "macos")]
fn release_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(p) = std::env::var("ATERM_PAINT_BIN") {
            let p = PathBuf::from(p);
            assert!(
                p.is_file(),
                "ATERM_PAINT_BIN={} does not exist",
                p.display()
            );
            return p;
        }
        let root = workspace_root();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        // Under the branded driver a nested invocation must name its own lane
        // (`targo --unverified`): the outer authorization does not propagate,
        // and a bare `targo build` here would refuse and fail the whole matrix
        // for a reason that has nothing to do with paint.
        let mut cmd = Command::new(&cargo);
        if Path::new(&cargo)
            .file_stem()
            .is_some_and(|n| n.to_string_lossy().starts_with("targo"))
        {
            cmd.arg("--unverified");
        }
        let status = cmd
            .args(["build", "--release", "-p", "aterm"])
            .current_dir(&root)
            .status()
            .unwrap_or_else(|e| panic!("could not spawn `{cargo} build --release -p aterm`: {e}"));
        assert!(
            status.success(),
            "`{cargo} build --release -p aterm` failed ({status}) — the paint matrix judges the \
             RELEASE binary and refuses to run without one (set ATERM_PAINT_BIN to drive a \
             prebuilt artifact)"
        );
        let bin = match std::env::var("CARGO_TARGET_DIR") {
            Ok(t) => {
                let t = PathBuf::from(t);
                let t = if t.is_absolute() { t } else { root.join(t) };
                t.join("release/aterm")
            }
            Err(_) => root.join("target/release/aterm"),
        };
        assert!(
            bin.is_file(),
            "built the release profile but {} is missing",
            bin.display()
        );
        bin
    })
    .clone()
}

/// The bitmap morphology and continuity classifiers have their own negative
/// controls. Keep this beside the artifact rows so a future threshold edit
/// cannot silently let a one-cell cursor impersonate a whole kitty, or erase
/// an interior two-frame blackout from the score.
#[cfg(target_os = "macos")]
#[test]
fn paint_scanner_semantic_classifiers_keep_their_negative_controls() {
    let dir = workspace_root().join("tools/paint-conformance");
    let out = Command::new("python3")
        .args(["-m", "unittest", "-v", "scan_test.py"])
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

/// What one matrix row expects of its take.
#[cfg(target_os = "macos")]
enum Expect {
    /// Effect ink present: ≥150 dynamic saturated px across the take, ≥4 of
    /// 12 hue buckets, a visible clump in some single frame, and no interior
    /// multi-frame rainbow blackout or status/raster contradiction.
    Ink,
    /// The dark control: ZERO dynamic saturated px in EVERY frame.
    Dark,
    /// The control for a path that carries an unavoidable cursor-cell
    /// artifact: the take FAILS the exact ink predicate, read the other way.
    /// See the probe's `--expect quiet` note for the
    /// measurements; on the unpinned path the deciding term is hue spread
    /// (1 bucket vs the trail's 9), not pixel count.
    Quiet,
}

/// Drive one shape through the probe on the ORIGINAL (recorded) capture path —
/// rows 1-4. See [`Capture::PinnedVideo`] for what that path perturbs, and
/// [`probe_with`] for the rest.
#[cfg(target_os = "macos")]
fn probe(shape: &str, keys: &str, expect: Expect) {
    probe_with(shape, keys, expect, Capture::PinnedVideo);
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
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum Companion {
    /// The shipped-default full-body resident cat, present from frame zero.
    Pet,
    /// The classic flying head, earned by a sustained typing run.
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
    probe_with_companion(shape, keys, expect, capture, None);
}

#[cfg(target_os = "macos")]
fn probe_with_companion(
    shape: &str,
    keys: &str,
    expect: Expect,
    capture: Capture,
    companion: Option<Companion>,
) {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _take_turns = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let bin = release_bin();
    let root = workspace_root();
    let script = root.join("tools/paint-conformance/paint_probe.sh");
    assert!(script.is_file(), "{} is missing", script.display());

    let (expect_arg, what) = match expect {
        Expect::Ink => ("ink", "effect ink present"),
        Expect::Dark => ("dark", "zero effect ink in every frame"),
        Expect::Quiet => (
            "quiet",
            "a take that the ink gate itself would NOT score as effect ink",
        ),
    };
    let mut cmd = Command::new(&script);
    cmd.arg(&bin)
        .args(["--shape", shape, "--record", "5", "--expect", expect_arg])
        .args(["--min-ink", "150", "--min-hues", "4", "--budget", "180"]);
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
        Some(Companion::Cat) => {
            cmd.args(["--style", "classic", "--companion", "cat"]);
        }
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
    let meaning = match capture {
        Capture::PinnedVideo => {
            "the shipped binary does not paint its flagship effect at all — this row's window              was FOCUSED and RECORDED, i.e. every excuse was already granted"
        }
        Capture::UnpinnedFocused => {
            "the engine and captured pixels disagree about the cursor companion, or the              rainbow contains an interior blackout — this row pairs every status read with its              exact unpinned frame and generic rainbow pixels cannot satisfy the animal obligation"
        }
        Capture::UnpinnedUnfocused => {
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
    probe("prompt", "e,c,h,o,space,h,e,l,l,o", Expect::Ink);
}

/// Matrix row 2: typing into the fake-Claude alt-screen shape (DEC-2026
/// bracketed repaints + DECTCEM hide + 150ms spinner + per-key echo) leaves
/// trail ink — the exact shape both dark releases were never proven against.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_fake_claude_typing_paints_trail_ink() {
    probe("fake-claude", "r,a,i,n,b,o,w,space,o,n", Expect::Ink);
}

/// Matrix row 3, the dark control: an alt-screen spinner repainting on its own
/// with NOTHING typed earns zero effect ink in every frame. This row is what
/// makes the other rows falsifiable — a scanner that sees ink here is
/// measuring noise, not the effect.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_cold_spinner_paints_zero_ink() {
    probe("cold-spinner", "", Expect::Dark);
}

/// Matrix row 4: typing beside an ESC7/ESC8 token streamer (unowned batches
/// landing away from the caret several times a second) leaves trail ink.
/// Verified live 2026-08-24 against HEAD 7cbf4651 ("an unowned batch retires
/// what it invalidates"): the no-row-probe fence fix holds under this shape.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_esc7_esc8_streamer_typing_paints_trail_ink() {
    probe("streamer", "h,e,l,l,o,space,w,o,r,l,d", Expect::Ink);
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
#[cfg(target_os = "macos")]
#[test]
fn unfocused_typed_window_paints_its_trail_under_an_unpinned_capture() {
    probe_with(
        "fake-claude",
        "r,a,i,n,b,o,w,space,o,n",
        Expect::Ink,
        Capture::UnpinnedUnfocused,
    );
}

/// Matrix row 6, row 5's control: the SAME unfocused window under the SAME
/// unpinned capture, with NOTHING typed, is a take the ink gate would not
/// score as ink. Without it row 5 could be satisfied by a scanner that counts
/// the client's own repaints; with it, row 5's 9 hue buckets are provably the
/// typed trail and nothing else. It also pins the other half of the contract
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
    probe_with("cold-spinner", "", Expect::Quiet, Capture::UnpinnedUnfocused);
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
        "r,a,i,n,b,o,w,space,o,n",
        Expect::Ink,
        Capture::UnpinnedFocused,
    );
}

/// Row 6b: the same law beside an ESC7/ESC8 token streamer — unowned batches
/// landing away from the caret several times a second, which is precisely the
/// concurrent-decoration traffic that makes the batch counter a bad proxy.
#[cfg(target_os = "macos")]
#[test]
fn focused_streamer_typing_paints_trail_ink_without_a_recording_pin() {
    probe_with(
        "streamer",
        "h,e,l,l,o,space,w,o,r,l,d",
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
    probe_with("cold-spinner", "", Expect::Dark, Capture::UnpinnedFocused);
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

/// Matrix row 8: the old flying kitty is rare by design and needs a sustained
/// high-band run. Four repetitions are 48 forward keys at the image probe's
/// 80ms cadence — comfortably past its ~2.6s earn law. The row requires at
/// least four `cat_active` captures and three consecutive isolated final
/// stills with a whole-head bitmap. Generic rainbow pixels alone satisfy none
/// of those terms.
#[cfg(target_os = "macos")]
#[test]
fn classic_style_sustained_typing_earns_and_paints_the_flying_kitty() {
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
