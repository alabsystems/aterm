// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stray a fresh hint licenses.

use super::*;

// ---- THE STRAY THE FRESH HINT LICENSES ---------------------
//
// The anti-stray control above covers two arms — "no key at all" and "a
// STALE hint". The THIRD arm was never written: a hint that is FRESH and
// that the key never spent, because the key moved nothing (Ctrl-E at the
// end of the line — D11's own named case) or because its echo has not
// landed yet. For 250 ms after such a press the program's OWN caret
// relocation wore the key's licence.
//
// THE FIRST FIX-UP'S PROBE FIXED ONE TIMING and passed by construction:
// it presented the caret at rest until 100 ms after the press, the one
// value in the neighbourhood past `NAV_NOOP_SETTLE_MS` = 80. That bound
// can only bite once a presented frame CATCHES the caret at rest more
// than 80 ms after the press — and a spinner that repaints sooner (a PTY
// round trip is 12-40 ms and the hide follows it) never lets it. So the
// probe is swept over the SETTLE window — how long frames still see the
// caret at rest before the repaint hides it — and over the LANDING time.
// What the sweep found: no bound on TIME separates a relocation inside
// the echo window from the echo itself; the cross-row strays are refused
// by SHAPE (`BridgeReach::line_start`), and the same-row one is Ctrl-A's
// own frames and stays licensed, which the last law below says plainly.

/// Which key, if any, stands behind the relocation the probe replays.
#[derive(Clone, Copy, Debug)]
enum StrayArm {
    /// (A) The pinned control: the program moves its caret, nobody typed.
    NoKey,
    /// (B) The pinned control's twin: a TYPED key, whose bridge is capped
    /// at `HIDE_BRIDGE_TYPED_MAX_DIST` and cannot reach the target.
    TypedKey,
    /// (C) THE DEFECT: a NAVIGATION key that moved nothing — the caret
    /// sits at its own cell for as long as the settle window says — and
    /// then the app's spinner repaint hides and re-shows the caret
    /// somewhere else, inside the 250 ms licence window.
    NavKeyThatMovedNothing,
}

/// The frames around the press.
#[derive(Clone, Copy, Debug)]
struct StrayTiming {
    /// Presented frames still see the caret at rest until this many ms
    /// after the press; the repaint hides it 15 ms later. `0`: the hide
    /// lands before any frame could catch the caret at rest.
    settle: u64,
    /// The caret reappears at the target this many ms after the press.
    landing: u64,
}

impl StrayTiming {
    /// The spinner's own cadence: the relocation lands 140 ms after the
    /// press, or 40 ms after the hide when the settle is long.
    fn spinner(settle: u64) -> Self {
        Self {
            settle,
            landing: 140.max(settle + 40),
        }
    }
}

/// What one probe arm produced: the light engine's spawns and voices, and
/// the classic bed's VERDICT on the same relocation. The two engines are
/// driven through the SAME frames because they must make the same source
/// decision — the opaque bed and the additive light desynchronize on any
/// move they disagree about. The bed's verdict is a tally, not a pixel
/// count: a nav-paired move lays no comet by design, so its cells read 0
/// whether it bridged the landing or refused it (the first fix-up's arm
/// was vacuous for exactly that reason).
#[derive(Default, PartialEq, Eq, Debug)]
struct StrayOutcome {
    spawns: u64,
    cues: WordNavCues,
    trail: crate::cursor_trail::BridgeTally,
}

impl StrayOutcome {
    /// Refused by both engines: nothing minted, and the bed declined the
    /// one relocation it saw.
    fn refused() -> Self {
        Self {
            spawns: 0,
            cues: WordNavCues::default(),
            trail: crate::cursor_trail::BridgeTally {
                bridged: 0,
                declined: 1,
            },
        }
    }
}

/// The probe's bed: Rainbow Kitty on the alt screen, the caret parked at
/// `(2, 42)` in a 100-column pane, both engines seeded on the same frame.
struct StrayBed {
    glow: CursorGlow,
    trail: crate::cursor_trail::CursorTrail,
    c: GlowConfig,
    g: Geom,
    tc: crate::cursor_trail::TrailConfig,
    t0: Instant,
    cells: Vec<aterm_render::TrailCell>,
}

impl StrayBed {
    fn new() -> Self {
        let g = wide_geom();
        let c = cfg(GlowStyle::RainbowKitty, true);
        let tc = crate::cursor_trail::TrailConfig {
            enabled: true,
            duration: Duration::from_millis(200),
            max_len: 8,
            color: 0x0050_FA7B,
            intensity: 0.0,
            warmth: 0.0,
        };
        let mut bed = Self {
            glow: CursorGlow::default(),
            trail: crate::cursor_trail::CursorTrail::default(),
            c,
            g,
            tc,
            t0: Instant::now(),
            cells: Vec::new(),
        };
        bed.glow.note_context(true);
        bed.trail.note_context(true);
        bed.glow.note_pane_columns(0, g.cols);
        bed.trail.note_pane_columns(0, g.cols);
        bed.frame(Some((2, 42)), bed.t0);
        let _ = wordnav_drain(&mut bed.glow);
        bed
    }

    /// One presented frame, both engines.
    fn frame(&mut self, cur: Option<(u16, u16)>, at: Instant) {
        self.glow
            .tick(cur, at, &self.c, self.g, &mut typed_scratch());
        self.trail.tick(cur, at, &self.tc, &mut self.cells);
    }

    /// The repaint's hide bracket, both engines.
    fn hide(&mut self, at: Instant) {
        self.glow.note_repaint_blink(at);
        self.trail.note_repaint_blink(at);
        self.frame(None, at + Duration::from_millis(5));
    }

    /// Drive one arm through the stray frames: the key (if any) 100 ms
    /// after the seed, the caret at rest per `timing.settle`, the hide,
    /// and the program's relocation to `target` at `timing.landing`.
    /// Returns the press instant.
    fn stray_frames(&mut self, arm: StrayArm, target: (u16, u16), timing: StrayTiming) -> Instant {
        let key = self.t0 + Duration::from_millis(100);
        match arm {
            StrayArm::NoKey => {}
            StrayArm::TypedKey => {
                self.glow.note_typed(key);
                self.trail.note_typed(key);
            }
            StrayArm::NavKeyThatMovedNothing => {
                self.glow.note_motion(key);
                self.trail.note_navigation(key);
            }
        }
        // The key moved NOTHING: every frame until the settle sees the
        // caret still at its own cell.
        let mut ms = 20u64;
        while ms <= timing.settle {
            self.frame(Some((2, 42)), key + Duration::from_millis(ms));
            ms += 20;
        }
        // …then the spinner's repaint hides the caret and puts it back
        // somewhere else.
        assert!(
            timing.landing >= timing.settle + 40,
            "the landing follows the hide: {timing:?}"
        );
        self.hide(key + Duration::from_millis(timing.settle + 15));
        self.frame(Some(target), key + Duration::from_millis(timing.landing));
        key
    }
}

/// One timer probe, both engines, one relocation.
fn stray_probe(arm: StrayArm, target: (u16, u16), timing: StrayTiming) -> StrayOutcome {
    let mut bed = StrayBed::new();
    let spawns_before = bed.glow.spawns();
    let bed_before = bed.trail.bridge_tally();
    bed.stray_frames(arm, target, timing);
    let tally = bed.trail.bridge_tally();
    StrayOutcome {
        spawns: bed.glow.spawns() - spawns_before,
        cues: wordnav_drain(&mut bed.glow),
        trail: crate::cursor_trail::BridgeTally {
            bridged: tally.bridged - bed_before.bridged,
            declined: tally.declined - bed_before.declined,
        },
    }
}

/// The bed's verdict is not vacuous: the SAME probe, driven through the
/// Ink shape the nav bridge exists for — a word key whose echo lands
/// across a hide, four cells on — reads one BRIDGED relocation in the
/// classic bed and one licensed spawn in the light. Without this control
/// the trail arm of the laws below could return to reading 0 for every
/// reason at once.
#[test]
fn the_stray_probe_s_bed_bridges_a_real_word_hop_across_a_repaint() {
    let mut bed = StrayBed::new();
    let key = bed.t0 + Duration::from_millis(100);
    bed.glow.note_motion(key);
    bed.trail.note_navigation(key);
    bed.hide(key + Duration::from_millis(8));
    bed.frame(Some((2, 37)), key + Duration::from_millis(40));
    let cues = wordnav_drain(&mut bed.glow);
    assert_eq!((cues.nav, cues.meteor), (1, 0), "the word hop sounds");
    assert_eq!(bed.glow.spawns(), 1);
    assert_eq!(
        bed.trail.bridge_tally(),
        crate::cursor_trail::BridgeTally {
            bridged: 1,
            declined: 0
        },
        "the bed bridged the same landing — and laid no comet for it, \
         which is why a pixel count cannot stand in for this verdict"
    );
    assert!(bed.cells.is_empty());
}

/// (7) A FRESH NAV HINT IS NOT A LICENCE FOR THE PROGRAM'S OWN CROSS-ROW
/// RELOCATION, AT ANY REPAINT PHASE.
///
/// Three arms over one identical relocation, swept over the settle window
/// (0 to 100 ms: from a hide that lands before any frame could catch the
/// caret at rest, to the first fix-up's single timing) and over five
/// targets — four inside the 4-row ceiling, where geometry alone admits
/// them, and the owner's measured `(2,42)->(9,3)` beyond it. Arms A and B
/// are the standing law. Arm C was RED at every settle at or below 80 ms
/// on the four near targets before the line-gesture shape: `(4,90)`,
/// `(5,88)` and `(6,3)` each minted 1 spawn and 1 meteor, and `(0,30)`
/// the same, with `aterm ctl trail` naming the key as the author.
#[test]
fn a_nav_key_that_moved_nothing_licenses_no_cross_row_relocation_at_any_repaint_phase() {
    let targets = [
        (4u16, 90u16), // two rows down, across the pane
        (5, 88),       // three rows down (the review's autorepeat face)
        (6, 3),        // four rows down, at the box's start (its worst face)
        (0, 30),       // two rows UP to an interior column
        (9, 3),        // seven rows down: the owner's measured stray
    ];
    for settle in [0u64, 20, 40, 60, 80, 100] {
        for target in targets {
            for arm in [
                StrayArm::NoKey,
                StrayArm::TypedKey,
                StrayArm::NavKeyThatMovedNothing,
            ] {
                assert_eq!(
                    stray_probe(arm, target, StrayTiming::spinner(settle)),
                    StrayOutcome::refused(),
                    "settle {settle} ms, {arm:?} -> {target:?}: nobody's key made \
                     this move"
                );
            }
        }
    }
}

/// (8) …AND THE SAME-ROW ONE IS CTRL-A'S OWN FRAMES. From `(2,42)`, a
/// hidden→visible landing at `(2,3)` under a fresh nav hint is exactly
/// what Ctrl-A on a Claude Code line looks like through an Ink repaint
/// (the box starts at column 2-3), and `(2,99)` is Ctrl-E's; no frame,
/// and no byte the host sees, separates a spinner that re-laid the caret
/// there from the key's echo. So THIS LAW STATES THE RESIDUAL RATHER THAN
/// HIDING IT: inside the echo window the landing is LICENSED — that is
/// the word-move fix, and narrowing it would silence Ctrl-A across every
/// repaint — and only the clock refuses it: a frame that saw the caret
/// still at rest more than `NAV_NOOP_SETTLE_MS` after the press, or a
/// landing later than `NAV_BRIDGE_LANDING_MS`. The exposure that remains
/// is a relocation along the caret's own row, or one row off it, inside
/// 150 ms of a nav key that moved nothing, when no frame caught the caret
/// at rest first. The 250 ms it was before this round is 150 now.
#[test]
fn a_same_row_landing_inside_the_echo_window_is_ctrl_a_s_shape_and_only_the_clock_refuses_it() {
    let ms = |settle, landing| StrayTiming { settle, landing };
    let licensed =
        |o: &StrayOutcome| o.spawns == 1 && o.trail.bridged == 1 && o.trail.declined == 0;
    for target in [(2u16, 3u16), (2, 99)] {
        // Inside both bounds: Ctrl-A's echo, licensed at every settle.
        for timing in [ms(0, 140), ms(40, 140), ms(80, 140), ms(80, 150)] {
            let out = stray_probe(StrayArm::NavKeyThatMovedNothing, target, timing);
            assert!(
                licensed(&out),
                "{target:?} at {timing:?}: Ctrl-A's own landing must stay licensed \
                 — got {out:?}"
            );
            assert_eq!(
                out.cues.nav + out.cues.meteor,
                1,
                "{target:?} at {timing:?}: …and it sounds once"
            );
        }
        // Past either bound: refused, by both engines.
        for timing in [ms(100, 140), ms(0, 151), ms(40, 160), ms(100, 200)] {
            assert_eq!(
                stray_probe(StrayArm::NavKeyThatMovedNothing, target, timing),
                StrayOutcome::refused(),
                "{target:?} at {timing:?}: the caret was seen at rest past the settle, \
                 or the landing is later than an echo"
            );
        }
        // The two controls never license it at any timing.
        for timing in [ms(0, 140), ms(80, 140)] {
            for arm in [StrayArm::NoKey, StrayArm::TypedKey] {
                assert_eq!(
                    stray_probe(arm, target, timing),
                    StrayOutcome::refused(),
                    "{arm:?} -> {target:?} at {timing:?}"
                );
            }
        }
    }
}

/// (9) …AND A REFUSED STRAY MUST NOT EAT THE STAMP. The worst face of the
/// defect: the stray consumed the nav hint, so the word hop the owner
/// actually pressed landed a frame later UNLICENSED and SILENT — the very
/// failure the nav bridge exists to fix, defeated in its own shape. Driven
/// at the first fix-up's timing AND under a fast repaint, on the review's
/// two near faces.
#[test]
fn a_refused_stray_leaves_the_word_key_its_own_licence() {
    for (target, settle) in [((9u16, 3u16), 100u64), ((5, 88), 40), ((6, 3), 0)] {
        let mut bed = StrayBed::new();
        let key = bed.stray_frames(
            StrayArm::NavKeyThatMovedNothing,
            target,
            StrayTiming::spinner(settle),
        );
        assert_eq!(
            wordnav_drain(&mut bed.glow),
            WordNavCues::default(),
            "{target:?} at settle {settle}: the spinner's relocation is not the \
             key's gesture"
        );
        assert_eq!(bed.glow.spawns(), 0);

        // The owner's OWN word hop, pressed while that stray was still on
        // screen: it is visible-to-visible, and it sounds.
        let hop = key + Duration::from_millis(170);
        bed.glow.note_motion(hop);
        bed.frame(Some(target), hop + Duration::from_millis(4));
        let _ = wordnav_drain(&mut bed.glow);
        bed.frame(
            Some((target.0, target.1 + 6)),
            hop + Duration::from_millis(18),
        );
        let cues = wordnav_drain(&mut bed.glow);
        assert_eq!(
            (cues.nav, cues.meteor),
            (1, 0),
            "{target:?} at settle {settle}: the hop the owner pressed still sounds"
        );
    }
}

/// (10) AUTOREPEAT: holding Option+Left at the start of the line re-stamps
/// the hint every 33 ms, so the caret is never seen at rest for 80 ms
/// after a press and the settle bound structurally cannot bite. The
/// repaint's relocation three rows down is refused by SHAPE alone.
#[test]
fn a_held_word_key_at_the_line_start_licenses_no_relocation_the_repaint_makes() {
    let mut bed = StrayBed::new();
    let mut t = bed.t0;
    for _ in 0..5 {
        t += Duration::from_millis(33);
        bed.glow.note_motion(t);
        bed.trail.note_navigation(t);
        bed.frame(Some((2, 42)), t + Duration::from_millis(8));
    }
    let _ = wordnav_drain(&mut bed.glow);
    let spawns_before = bed.glow.spawns();
    bed.hide(t + Duration::from_millis(30));
    bed.frame(Some((5, 88)), t + Duration::from_millis(70));
    assert_eq!(wordnav_drain(&mut bed.glow), WordNavCues::default());
    assert_eq!(bed.glow.spawns() - spawns_before, 0);
    assert_eq!(
        bed.trail.bridge_tally(),
        crate::cursor_trail::BridgeTally {
            bridged: 0,
            declined: 1
        }
    );
}
