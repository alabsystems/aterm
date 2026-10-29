// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A shed frame is still a heard key.

use aterm_core::render::GlowQuad;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::trail_sound::SoundKind;
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;

fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * 8) as u16,
        win_h: (ROWS * 16) as u16,
        head: 0,
    }
}

fn cfg(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        style,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        ..Default::default()
    }
}

fn dark(base: &GlowConfig, audible: bool) -> GlowConfig {
    GlowConfig {
        intensity: 0.0,
        audible,
        ..*base
    }
}

struct Rig {
    glow: CursorGlow,
    g: Geom,
    out: Vec<GlowQuad>,
}

impl Rig {
    fn new() -> Self {
        Self {
            glow: crate::common::quads_glow(),
            g: geom(),
            out: Vec::new(),
        }
    }

    fn tick(&mut self, now: Instant, c: &GlowConfig) {
        self.tick_at(now, c, 0);
    }

    fn tick_at(&mut self, now: Instant, c: &GlowConfig, col: u16) {
        self.out.clear();
        self.glow
            .tick(Some((2, col)), now, c, self.g, &mut self.out);
    }
}

#[test]
fn a_shed_frame_keeps_the_key_seam_open() {
    let base = cfg(GlowStyle::RainbowKitty);
    let t0 = Instant::now();

    let mut r = Rig::new();
    r.tick(t0, &base);
    let t1 = t0 + Duration::from_millis(16);
    r.tick(t1, &dark(&base, true));
    assert!(
        r.glow.sound_seam_open(),
        "a shed frame must leave the key seam open"
    );
    assert!(
        r.glow.cue_keystroke_kind(t1, SoundKind::Typed),
        "a key pressed during a shed frame must record a click"
    );
    assert!(
        r.glow.take_key_cue().is_some(),
        "the host must be able to pop that cue on the same call"
    );

    let mut r = Rig::new();
    r.tick(t0, &base);
    r.tick(t1, &dark(&base, false));
    assert!(
        !r.glow.sound_seam_open(),
        "an unfocused window must leave the seam shut"
    );
    assert!(
        !r.glow.cue_keystroke_kind(t1, SoundKind::Typed),
        "an unfocused window must not chirp"
    );

    let mut r = Rig::new();
    r.tick(t0, &base);
    let off = GlowConfig {
        enabled: false,
        audible: true,
        ..base
    };
    r.tick(t1, &off);
    assert!(
        !r.glow.sound_seam_open(),
        "`cursor_trail = false` / serious mode stays authoritative over sound"
    );
    assert!(!r.glow.cue_keystroke_kind(t1, SoundKind::Typed));
}

#[test]
fn the_silent_visual_click_reads_this_tick_s_thermal_constants() {
    let t0 = Instant::now();
    let fire = cfg(GlowStyle::Fire);
    let lumen = cfg(GlowStyle::Lumen);

    let t1 = t0 + Duration::from_millis(16);

    let probe = |first: &GlowConfig, second: &GlowConfig| -> f32 {
        let mut r = Rig::new();
        r.tick(t0, first);
        assert!(r.glow.cue_keystroke_kind(t0, SoundKind::Typed));
        let _ = r.glow.take_key_cue();
        r.tick(t1, second);
        assert!(r.glow.cue_keystroke_kind(t1, SoundKind::Typed));
        r.glow.take_key_cue().expect("a cue").heat
    };

    let shed_heat = probe(&fire, &dark(&lumen, true));
    let lumen_heat = probe(&lumen, &lumen);
    let fire_heat = probe(&fire, &fire);

    assert!(
        (shed_heat - lumen_heat).abs() < 1e-6,
        "the shed click must read THIS tick's style: {shed_heat} vs lumen {lumen_heat}"
    );
    assert!(
        (fire_heat - lumen_heat).abs() > 1e-6,
        "the control is vacuous unless Fire and Lumen really differ \
         ({fire_heat} vs {lumen_heat})"
    );
}

#[test]
fn a_shed_tick_keeps_the_keyed_click_ledger() {
    let base = cfg(GlowStyle::Water);
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(16);
    let t2 = t1 + Duration::from_millis(20);

    let mut r = Rig::new();
    r.tick(t0, &base);
    r.tick_at(t1, &dark(&base, true), 0);
    assert!(r.glow.cue_keystroke_kind(t1, SoundKind::Typed));
    let _ = r.glow.take_key_cue();
    let _ = r.glow.drain_sound_cues().count();
    r.glow.note_typed(t2);
    r.tick_at(t2, &base, 1);
    assert_eq!(
        r.glow.drain_sound_cues().count(),
        0,
        "the banked credit must mute the echo of the key it stood for"
    );

    let mut r = Rig::new();
    r.tick(t0, &base);
    r.tick_at(t1, &dark(&base, false), 0);
    assert!(!r.glow.cue_keystroke_kind(t1, SoundKind::Typed));
    r.glow.note_typed(t2);
    r.tick_at(t2, &base, 1);
    assert_eq!(
        r.glow.drain_sound_cues().count(),
        1,
        "with no credit banked the echo must click once"
    );

    let mut banked = [0usize; 2];
    for (i, lit) in [false, true].into_iter().enumerate() {
        let mut r = Rig::new();
        r.tick(t0, &base);
        let frame = if lit { base } else { dark(&base, true) };
        let mut t = t1;
        for _ in 0..16 {
            r.tick_at(t, &frame, 0);
            let _ = r.glow.cue_keystroke_kind(t, SoundKind::Typed);
            let _ = r.glow.take_key_cue();
            let _ = r.glow.drain_sound_cues().count();
            t += Duration::from_millis(2);
        }
        let mut silent = 0;
        for col in 1..=16u16 {
            r.tick_at(t, &base, col);
            if r.glow.drain_sound_cues().count() == 0 {
                silent += 1;
            }
            t += Duration::from_millis(2);
        }
        banked[i] = silent;
    }
    assert_eq!(
        banked[0], banked[1],
        "the shed path must bank exactly what the LIT path banks — the bound \
         is pre-existing, not something the split opened (shed {}, lit {})",
        banked[0], banked[1]
    );
}

#[test]
fn a_master_off_tick_still_wipes_the_sound_ledger() {
    let base = cfg(GlowStyle::Water);
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(16);

    for (label, teardown) in [("master-off", true), ("reset", false)] {
        let mut r = Rig::new();
        r.tick(t0, &base);
        assert!(r.glow.cue_keystroke_kind(t0, SoundKind::Typed));
        if teardown {
            let off = GlowConfig {
                enabled: false,
                audible: true,
                ..base
            };
            r.tick(t1, &off);
        } else {
            r.glow.reset();
        }
        assert_eq!(
            r.glow.drain_sound_cues().count(),
            0,
            "{label} must drop the pending cues"
        );
        let t2 = t1 + Duration::from_millis(20);
        r.tick_at(t1 + Duration::from_millis(4), &base, 0);
        let _ = r.glow.drain_sound_cues().count();
        r.glow.note_typed(t2);
        r.tick_at(t2, &base, 1);
        assert_eq!(
            r.glow.drain_sound_cues().count(),
            1,
            "{label} must drop the keyed-click credits too"
        );
    }
}

#[test]
fn idle_zero_holds_while_the_audio_seam_is_open() {
    let base = cfg(GlowStyle::RainbowKitty);
    let t0 = Instant::now();
    let mut r = Rig::new();
    r.tick(t0, &base);

    let mut now = t0;
    for _ in 0..40 {
        now += Duration::from_millis(250);
        r.tick(now, &dark(&base, true));
        assert!(r.glow.sound_seam_open());
        let _ = r.glow.cue_keystroke_kind(now, SoundKind::Typed);
        let _ = r.glow.take_key_cue();
    }
    assert!(
        !r.glow.is_active(),
        "a dark-but-audible run must still disarm the frame train"
    );
}

#[test]
fn a_dark_audible_run_never_fills_the_cue_backlog() {
    let base = cfg(GlowStyle::RainbowKitty);
    let t0 = Instant::now();
    let mut r = Rig::new();
    r.tick(t0, &base);

    let mut now = t0;
    for i in 0..64 {
        now += Duration::from_millis(30);
        r.tick(now, &dark(&base, true));
        assert!(
            r.glow.cue_keystroke_kind(now, SoundKind::Typed),
            "key {i} was refused — the backlog filled while dark"
        );
        assert!(r.glow.take_key_cue().is_some(), "key {i} minted no cue");
    }
    assert_eq!(
        r.glow.drain_sound_cues().count(),
        0,
        "the queue must be empty after the host popped every cue"
    );
}
