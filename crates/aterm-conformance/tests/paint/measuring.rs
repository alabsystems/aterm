// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The paint matrix's rows — the suite's own docs are in `../paint.rs`.

use super::support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// The workspace root, from this crate's own manifest dir — the probe script
/// and the release binary are both addressed from it.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-conformance has a workspace root two levels up")
        .to_path_buf()
}

/// The bitmap morphology and continuity classifiers' own negative controls: a
/// one-cell cursor must not pass as a kitty, and an interior two-frame blackout
/// must not vanish from the score.
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

/// Self-tests for `tools/paint-conformance/starvation.py`, which lets a red say
/// whether the machine starved the take: its floor is reachable on a real
/// machine, and no samples reads as `unknown`, never as clean.
#[test]
fn paint_probe_starvation_label_keeps_its_calibration_and_its_unknown() {
    let dir = workspace_root().join("tools/paint-conformance");
    let out = Command::new("/usr/bin/python3")
        .args(["-m", "unittest", "-v", "starvation_test.py"])
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir(&dir)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "could not run {}: {e}",
                dir.join("starvation_test.py").display()
            )
        });
    assert!(
        out.status.success(),
        "paint starvation-label self-tests failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// The mature run: the release smoke's 29 keys, shared by every typed trail
/// row.
///
/// The traverse is per-mark (`RAINBOW_TRAVERSE_MIN_CELLS` in
/// crates/aterm-effects/src/cursor_glow/raster.rs), so a ~10-key mark spans only
/// 2-3 of 12 hue buckets by design and sits on the `union_hues >= 4` cliff. At
/// 29 keys the arc completes (union_hues ~10-11, ribbon_window_hues ~10), so
/// the floors have real margin. No spaces: a space ends the mark.
const MATURE_RUN: &str = "t,h,e,r,a,i,n,b,o,w,k,i,t,t,y,p,a,i,n,t,s,t,h,e,a,r,c,o,k";

/// What one matrix row expects of its take.
enum Expect {
    /// Effect ink present: ≥150 dynamic saturated px across the take, ≥4 of
    /// 12 hue buckets, a visible clump in some single frame, repeated
    /// ribbon-shaped geometry with no interior multi-frame blackout, and (on a
    /// paired take) no frame where the engine claims a ribbon it is still
    /// driving over a raster without that geometry. The claim must cover at
    /// least two frames.
    ///
    /// * A ribbon is saturated changed pixels forming a thin detached component
    ///   or a dense horizontal run attached to the cursor in X. Hue separation is
    ///   diagnostic only.
    /// * `ribbon_hue_bands` counts distinct hue bands among live typing sparks
    ///   and keeps claiming through the authored ~2 s exhale, so the claim is
    ///   held only while the engine's ledger reports the ribbon driven
    ///   (`driven_last_frame`).
    ///
    /// The contradiction count spans the whole driven window, not the
    /// raster-derived `sustained_rainbow_bounds`, so a ribbon that dies mid-take
    /// cannot end its own audit window.
    ///
    /// The two-frame floor is an obligation on the evidence and rides on this
    /// arm alone, outside the predicate [`Expect::Quiet`] negates.
    Ink,
    /// The dark control: zero dynamic saturated px in every frame. Only valid
    /// for a style with no resident companion; see [`Expect::TrailDark`].
    Dark,
    /// Zero trail with a resident companion allowed: no rainbow frame and no
    /// claimed ribbon, however many pixels the resident animal paints. A
    /// resident pet is on glass at rest, so `Dark` cannot hold for it; `Dark`
    /// stays the absolute-zero claim for companion-free styles.
    TrailDark,
    /// The control for a path with an unavoidable cursor-cell artifact: the
    /// take fails the exact ink predicate. See the probe's `--expect quiet`
    /// note; on the unpinned path the deciding terms are take-level colour
    /// identity and ribbon geometry, not raw pixel count.
    ///
    /// Only the paint predicate is negated. [`Expect::Ink`]'s
    /// `ribbon_bound >= 2` floor is excluded, since this row books no driven
    /// ribbon and would otherwise pass for want of a ledger entry.
    Quiet,
    /// Matched typed negative control: cursor/text deltas must be present, but
    /// the canonical off style must produce no ribbon witness and keep every
    /// effect ledger quiet.
    EffectOff,
}

/// Drive one shape through the probe on the recorded capture path (rows 1-4).
/// See [`Capture::PinnedVideo`] for what that path perturbs, and
/// [`probe_with`] for the rest.
fn probe(shape: &str, keys: &str, expect: Expect) {
    // Trail rows run on the companion-free `flying` style, isolating ribbon
    // continuity from the resident animal's bitmap.
    probe_styled(shape, keys, expect, Capture::PinnedVideo, "flying");
}

/// [`probe`] with the style named outright.
fn probe_styled(shape: &str, keys: &str, expect: Expect, capture: Capture, style: &str) {
    probe_with_companion_style(shape, keys, expect, capture, None, Some(style));
}

/// How the take is captured, and so what the instrument perturbs (see the
/// observer rule in `../paint.rs`).
#[derive(Clone, Copy)]
enum Capture {
    /// `ctl video`: the recording pins `motion_focus` for the recorded window,
    /// so it cannot catch a motion-gate regression.
    PinnedVideo,
    /// An unpinned image burst leaving the window's initial focused state
    /// intact. Used by the companion rows.
    UnpinnedFocused,
    /// A burst of `ctl image` captures with the window unfocused
    /// (`ctl focus out`), motion gate untouched: the arm that can go red.
    UnpinnedUnfocused,
}

/// A companion whose engine state and captured bitmap are both obligations.
/// Each variant carries its style arm: `rainbow kitty` and `rainbow kitty pet`
/// both name the resident; only `rainbow kitty flying` earns a flypast.
#[derive(Clone, Copy)]
enum Companion {
    /// The shipped-default full-body resident cat, present from frame zero
    /// (`--style default`: the config key is left absent).
    Pet,
    /// The same resident via the literal config line
    /// `cursor_trail_style = "rainbow kitty"` (`--style classic`); see row 10.
    PetOwnerSpelling,
    /// The old flying head, earned by a sustained typing run, behind its
    /// explicit opt-in spelling (`--style flying`).
    Cat,
}

/// Drive one shape through `tools/paint-conformance/paint_probe.sh` on the
/// given capture path and assert its verdict. Serialized: each case launches
/// its own headless instance, and concurrent takes would contend for frames
/// and CPU.
fn probe_with(shape: &str, keys: &str, expect: Expect, capture: Capture) {
    probe_with_companion_style(shape, keys, expect, capture, None, Some("flying"));
}

/// A row pinning an explicit `--style` with no companion obligation (the
/// absolute-zero control needs the companion-free `flying`).
fn probe_with_style(shape: &str, keys: &str, expect: Expect, capture: Capture, style: &str) {
    probe_with_companion_style(shape, keys, expect, capture, None, Some(style));
}

fn probe_with_companion(
    shape: &str,
    keys: &str,
    expect: Expect,
    capture: Capture,
    companion: Option<Companion>,
) {
    probe_with_companion_style(shape, keys, expect, capture, companion, None);
}

fn probe_with_companion_style(
    shape: &str,
    keys: &str,
    expect: Expect,
    capture: Capture,
    companion: Option<Companion>,
    style: Option<&str>,
) {
    probe_row(shape, keys, expect, capture, companion, style, None);
}

/// The one row runner. `split` (`v`/`h`) splits the window before the shape
/// runs, so the take goes through the composed path (`--split` in the probe).
fn probe_row(
    shape: &str,
    keys: &str,
    expect: Expect,
    capture: Capture,
    companion: Option<Companion>,
    style: Option<&str>,
    split: Option<&str>,
) {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _take_turns = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let Some(bin) = support::release_bin(&["ATERM_PAINT_BIN"]) else {
        return;
    };
    let root = workspace_root();
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
        // Stated explicitly: the matrix checks the shipped artifact, which
        // renders on the GPU. The probe turns a GPU init failure into
        // COULD-NOT-RUN instead of a CPU fallback, and this file treats that as
        // not a pass.
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
    if let Some(split) = split {
        cmd.args(["--split", split]);
    }
    // Re-take an unproved row, bounded, and only where a re-take can differ
    // (another process can starve a single sampler tick). This launders
    // nothing:
    //   * exit 1 (FAIL) and exit 2 (COULD NOT RUN) are final on the take that
    //     produced them;
    //   * a later take is accepted only if it is sound on its own reading;
    //     nothing accumulates across takes (`paint_verdict` is stateless, per
    //     starvation_test.py);
    //   * a clamped tier (`sched_qos=utility|background`) is not re-taken,
    //     since every take there is starved.
    // Every disowned take's line is printed.
    use std::io::Write as _;
    const MAX_TAKES: usize = 3;
    let mut disowned: Vec<String> = Vec::new();
    let (out, stdout, stderr, report) = loop {
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("could not spawn {}: {e}", script.display()));
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let report = stdout
            .lines()
            .rev()
            .find(|l| l.starts_with("PAINT"))
            .unwrap_or("<no PAINT report line>")
            .to_string();
        let clamped_tier =
            report.contains("sched_qos=utility") || report.contains("sched_qos=background");
        if out.status.code() == Some(3) && !clamped_tier && disowned.len() + 1 < MAX_TAKES {
            // Written to the real stderr, not `eprintln!` (which libtest
            // discards on pass), so a re-taken row is visible in the gate log.
            let _ = writeln!(
                std::io::stderr(),
                "PAINT-RETAKE [{shape}] take {} of {MAX_TAKES} UNPROVED (disowned by its own \
                 sampler) - re-taking; a FAIL on any take is final.\n  {report}",
                disowned.len() + 1
            );
            disowned.push(report);
            continue;
        }
        break (out, stdout, stderr, report);
    };
    let earlier = if disowned.is_empty() {
        String::new()
    } else {
        format!(
            "\n--- {} earlier take(s) of this row, disowned by their own sampler ---\n  {}",
            disowned.len(),
            disowned.join("\n  ")
        )
    };
    // What a red means differs for rows 1-4 and rows 5-6, so each says so.
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
        Some(0) => eprintln!("paint[{shape}]: {report}{earlier}"),
        Some(1) => panic!(
            "PAINT CONFORMANCE FAILED [{shape}]: expected {what}, and the take says otherwise — \
             {meaning} (docs/RELEASE-PROOF-DISCIPLINE.md).\n  {report}{earlier}\n\
             --- probe stderr ---\n{stderr}"
        ),
        // Exit 2: the probe decided nothing (no binary, no `sips`, a socket
        // that never bound, a failed capture). The message opens with the
        // gate's COULD NOT RUN sentinel, so it is recorded as could-not-run,
        // never green; the MEASURE receipt then says `measured no`.
        Some(2) => panic!(
            "aterm-gate: COULD NOT RUN — PAINT CONFORMANCE COULD NOT RUN [{shape}]: the probe \
             decided nothing, which is not a pass.\n  {report}{earlier}\n\
             --- probe stderr ---\n{stderr}"
        ),
        // Exit 3: the take ran but its instrument disowns it
        // (`evidence=unproved`), so nothing follows about the glass. Red here,
        // with the COULD NOT RUN sentinel so the gate records the machine, not
        // the tree; `reason=` names the tier remedy.
        Some(3) => panic!(
            "aterm-gate: COULD NOT RUN — PAINT CONFORMANCE UNPROVED [{shape}]: the take is not \
             evidence about paint — see \
             `starved=`/`evidence=` and `reason=` on the line below for what the instrument \
             could not stand behind. Re-take on an unloaded machine, from an interactive shell \
             or a LaunchAgent with ProcessType=Interactive. {MAX_TAKES} take(s) at most were \
             made, and none was sound (a clamped tier is not re-taken).\n  {report}{earlier}\n\
             --- probe stderr ---\n{stderr}"
        ),
        code => panic!(
            "paint probe [{shape}] died abnormally (exit {code:?}, the protocol is 0/1/2/3)\n\
             --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ),
    }
}

/// Matrix row 1: a human typing at a main-screen /bin/sh prompt leaves trail ink.
#[test]
fn main_screen_prompt_typing_paints_trail_ink() {
    probe("prompt", MATURE_RUN, Expect::Ink);
}

/// Row 1's matched negative control: identical prompt, keys, focus and video
/// path, only the effect style changed. It must keep real dynamic cursor
/// pixels while producing no ribbon witness, so row 1 cannot pass by counting
/// cursor motion or echoed text.
///
/// With the effect off this is the matrix's one static window, and a
/// present-driven recorder shows its idle gaps as frame holes. The recording
/// loop publishes a sampling ledger (index.json `ticks[]`: ticks whose redraw
/// proved the screen unchanged), and scan.py reads `sampling_hole_us` from
/// ticks and frames together, so honest static gaps are not holes while a
/// render stall still is (scan_test.py
/// `test_a_static_windows_frame_gap_is_not_a_hole_when_the_ledger_ticked`,
/// `test_a_render_stall_under_a_live_loop_is_still_a_hole`). The alt-screen
/// twins are `focused_typed_window_with_effect_off…` and
/// `unfocused_typed_window_with_effect_off…`.
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
/// trail ink.
#[test]
fn alt_screen_fake_claude_typing_paints_trail_ink() {
    probe("fake-claude", MATURE_RUN, Expect::Ink);
}

/// Matrix row 3, the dark control: an alt-screen spinner repainting on its own
/// with nothing typed earns zero effect ink in every frame, which makes the
/// other rows falsifiable.
#[test]
fn alt_screen_cold_spinner_paints_zero_ink() {
    probe("cold-spinner", "", Expect::TrailDark);
}

/// Matrix row 4: typing beside an ESC7/ESC8 token streamer (unowned batches
/// landing away from the caret several times a second) leaves trail ink.
///
/// Continuity is judged in time, not frames. `ctl video ... keys` stamps every
/// keystroke into `index.json`'s `inputs[]` on the same clock as
/// `frames[].t_us`, so the take carries the interval in which the trail is
/// driven. Inside it (plus a 150 ms grace) the scan reports `driven_dark_us`,
/// the longest blackout in wall-clock time, charging only frames sampled dark
/// (first-dark to last-dark plus one nominal interval), so a render stall
/// cannot manufacture a blackout; `sampling_hole_us` is reported beside it.
/// The bound is 1.5 nominal intervals: one dark frame is capture jitter, two
/// is a regression.
///
/// A frame is lit when `scan.driven_lit` says so: the band's flat saturated
/// population (`band_ink_metrics` at `RIBBON_LIT_MIN_PX`) ORed with the
/// ribbon-geometry arm. The charge window opens at the first sustained lit
/// frame (the lead is reported as `mature_lead_us` and bounded by
/// `MATURE_LEAD_MAX_US`), and the close's first 0.4 s belongs to the authored
/// exhale (`EXHALE_CLOSE_ALLOWANCE_US`). The arc promise stays at take level
/// on `union_hues`. See scan.py's `driven_window_darkness` and `driven_lit`.
///
/// Image-burst rows have no `inputs[]`, so they report `driven_dark_us=-1` and
/// keep the whole-take frame gap.
#[test]
fn alt_screen_esc7_esc8_streamer_typing_paints_trail_ink() {
    probe("streamer", MATURE_RUN, Expect::Ink);
}

/// Matrix row 5, the row that can go red: an unfocused window (`ctl focus out`
/// → the same `App::on_focus` a winit `Focused(false)` calls) typed into over
/// the control socket, captured by an unpinned `ctl image` burst that leaves
/// `motion_focus` as the unobserved app has it.
///
/// The witness is hue spread, not pixel count: an unfocused window still drags
/// a block cursor and echoes glyphs, but without the typed-wake term the trail
/// collapses to one hue bucket, while the healthy arm reaches ~9-11 at
/// [`MATURE_RUN`]. `--min-hues 4` sits between them.
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

/// Matrix row 6, row 5's control: the same unfocused window and capture with
/// nothing typed is not scored as ink, so row 5's hue spread is provably the
/// typed trail. It also pins that an idle unfocused window still demotes.
///
/// `Expect::Quiet`, not `Expect::Dark`: the client hides its caret inside each
/// DEC-2026 bracket, so a capture landing mid-bracket sees one cursor cell
/// (98 px) differ in a frame or two. The trail's signature is hue spread (1
/// bucket here vs ~9 in row 5). Not vacuous: `--expect quiet` fails on row 5's
/// typed shape.
#[test]
fn unfocused_idle_window_paints_nothing_under_an_unpinned_capture() {
    probe_with(
        "cold-spinner",
        "",
        Expect::Quiet,
        Capture::UnpinnedUnfocused,
    );
}

/// Matrix rows 6a-6c: a focused window captured without a recording pin, the
/// configuration users are actually in (focused, unrecorded, typing into a TUI
/// that repaints on its own).
///
/// Rows 1-4 are pinned (`ctl video` holds `motion_focus` open, repairing the
/// gate under test) and rows 5-6 unfocused. Under a continuous recording
/// cadence the multi-batch generation fence never trips, so a fence that
/// condemns honest keystrokes beside a concurrent spinner batch is visible only
/// here. Row 6c is the dark control, so 6a/6b cannot pass by counting cursor
/// pixels.
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

/// Row 6b: the same beside an ESC7/ESC8 token streamer, the concurrent
/// decoration traffic that makes a batch counter a bad proxy.
///
/// This row carries the ribbon bind: a mature engine claim is charged only
/// through the last frame the engine reports as driven (the exhale is outside),
/// presence comes from ribbon geometry rather than hue spread, and the
/// contradiction tally spans the full driven window. `ribbon_bound >= 2` keeps
/// a zero contradiction count non-vacuous.
#[test]
fn focused_streamer_typing_paints_trail_ink_without_a_recording_pin() {
    probe_with(
        "streamer",
        MATURE_RUN,
        Expect::Ink,
        Capture::UnpinnedFocused,
    );
}

/// Row 6c, the dark control for 6a/6b: a focused alt-screen spinner with
/// nothing typed earns zero effect ink under the same unpinned capture. The
/// concurrent-decoration law relaxes a batch count, never the requirement that
/// a real keystroke armed the candidate.
#[test]
fn focused_cold_spinner_paints_zero_ink_without_a_recording_pin() {
    probe_with(
        "cold-spinner",
        "",
        Expect::TrailDark,
        Capture::UnpinnedFocused,
    );
}

/// Matrix row 7: the shipped default. The resident pet owes a full-body bitmap
/// in the first still (before any typing), stays present through every paired
/// status/frame, and rides a continuous rainbow while a word is typed.
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

/// The explicit underline spelling travels through the shipped config loader,
/// resolves to the rainbow engine, reports underline geometry and paints a
/// real ribbon; the default and shorthand rows use the tall body. Typed at
/// [`MATURE_RUN`]'s 29 keys, since a short mark cannot reach the
/// `ribbon_hue_bands >= 4` mature-claim threshold.
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

/// The explicit `rainbow kitty tall` spelling, through the shipped config
/// loader: it must resolve to the rainbow engine with `ribbon_look=tall` and
/// paint (`--style tall` binds `style=` to `resolved=`). The spelling also
/// draws the default's resident pet; the pet obligations at these 12 keys are
/// row 7's ([`shipped_default_first_still_and_typing_carry_the_resident_pet`]).
#[test]
fn explicit_tall_short_word_paints_four_hues() {
    probe_with_style(
        "prompt",
        "r,a,i,n,b,o,w,k,i,t,t,y",
        Expect::Ink,
        Capture::UnpinnedFocused,
        "tall",
    );
}

/// Exact off twin: same prompt, keys, focus and unpinned capture.
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

/// The absolute-zero control: a style with no resident (`flying`), cold
/// output, and literally zero lit pixels in every frame. The cold rows for
/// `rainbow kitty` assert [`Expect::TrailDark`] because its resident is on
/// glass at rest; this row keeps the strongest claim alive.
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

/// Matrix row 8: the flying kitty (`rainbow kitty flying`) is rare by design
/// and needs a sustained high-band run. Four repetitions are 48 forward keys;
/// the band opens near the 8th key and the earn law's 16-key travel floor
/// completes near the 23rd. The row requires at least four `cat_active`
/// captures, three consecutive isolated final stills with a whole-head bitmap,
/// and `pet_claimed == 0` (the flypast, not the resident). Generic rainbow
/// pixels satisfy none of those.
///
/// A headless subject observes a key only on a render, so the probe paces this
/// arm on its own clock (dropping the optional mid-gap capture rather than
/// slowing the typist, since the earn law refuses anything slower than ~0.60
/// s/key), bounds the decay tail from the last key so the stills land inside
/// the cat's 3.9 s lifetime, and prints `typed_gap_p50_ms` /
/// `typed_gap_max_ms`. One 12-key repetition is too short and still fails.
///
/// On the final stills the head has settled to its resting size, so scan.py's
/// `CAT_STILL_MIN_H` (15) admits it there, only while the engine claims the
/// cat; mid-take frames keep the strict floor. scan_test.py pins the impostor
/// edges (a one-cell row is <= 14 px, the caret fails the width floor).
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

/// Matrix row 9, the cold advancing caret: the one shape only the freshness
/// licence keeps dark (docs/design/EFFECTS-LICENSE-REDESIGN.md).
///
/// Rows 3 and 6 never present a cold cursor delta (the spinner hides and parks
/// its caret; the streamer restores it with ESC 7 / ESC 8). This row streams
/// append-shaped tokens on the alt screen with the caret shown and its resting
/// position walking several cells per batch, several times a second, with
/// nothing typed. A licence keyed on fresh key hints refuses it: no press, no
/// hint, nothing minted.
///
/// Judged [`Expect::TrailDark`]: the engine's cumulative ledger — the quiet
/// set `licensed=0 spawns=0 ribbon_active=false sparks=0 glow_active=false`,
/// read once after the take on this probe's fresh instance, so it covers the
/// instance's whole life — plus `ribbon_claimed=0` and `ribbon_dark=0` in the
/// raster pairing. The fixture desaturates the one known non-effect emitter: a
/// walking caret paints ~98 dynamic saturated px per moved cell in aterm's
/// default `#50FA7B`, so it sets the caret to the foreground grey over OSC 12
/// before its first byte. Every authored effect hue is untouched.
///
/// Not vacuous: the probe reads `ctl cursor` twice, 0.6 s apart, and reports
/// COULD-NOT-RUN unless the caret is `visible=1` and has moved. The red arm is
/// `CursorGlow::spawn` with its candidate gate neutered (`admitted_intent`
/// `Some(..)` for every presented delta).
///
/// The fixture's alphabet is nonsense and the probe turns `[sparkle_words]`
/// off, since word decoration (e.g. `dog` sprites) is content-triggered and
/// not the light this row judges.
#[test]
fn alt_screen_cold_token_streamer_with_a_walking_caret_paints_zero_ink() {
    probe("cold-streamer", "", Expect::TrailDark);
}

/// Matrix row 10: the literal config line `cursor_trail_style = "rainbow
/// kitty"`. Row 7 covers the default spelling (`rainbow kitty pet`); this row
/// demands the same companion obligations (`pet_claimed == frames`,
/// `cat_claimed == 0`, `pet_final_run >= 3`) for the shorthand, so a spelling
/// that resolves to the wrong animal cannot pass. With row 8 it shows the
/// flying head lives at `rainbow kitty flying` and neither row can pass with
/// the other's animal.
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

/// Matrix row 11: one companion body per frame, on the `rainbow kitty` line.
///
/// Uses row 8's sustained run: both companions can claim the same frame only
/// once typing earns the flypast (the resident steps aside for the head), the
/// only window where a second body could be drawn.
///
/// `companion_stack` counts isolating frames (the launch still and the three
/// final stills) with two or more companion-shaped components and must be
/// zero. It is a pixel obligation because both engines being active at once is
/// legal state; active is not drawn. The other pet obligations ride along, so
/// the row cannot pass by losing the resident.
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

/// Matrix row 12: a split window being typed into. `--split v` splits the
/// headless window before the shape, so the prompt and keys land in the new
/// focused pane (which keeps the rainbow body) and every frame goes through
/// the composed path (`redraw_compose`). Unpinned focused image capture,
/// paired with its effect-off twin below.
#[test]
fn split_window_typing_paints_the_rainbow_body_in_the_focused_pane() {
    probe_row(
        "prompt",
        MATURE_RUN,
        Expect::Ink,
        Capture::UnpinnedFocused,
        None,
        Some("flying"),
        Some("v"),
    );
}

/// Row 12's matched effect-off twin: the same split, keys and capture with
/// the canonical `off` style — typed cursor/text pixels, no ribbon — so the
/// row cannot pass on the divider, the second pane or echoed text.
#[test]
fn split_window_typing_with_effect_off_has_no_ribbon() {
    probe_row(
        "prompt",
        MATURE_RUN,
        Expect::EffectOff,
        Capture::UnpinnedFocused,
        None,
        Some("off"),
        Some("v"),
    );
}
