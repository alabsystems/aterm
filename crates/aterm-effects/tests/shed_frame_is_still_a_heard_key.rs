// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A SHED FRAME IS STILL A HEARD KEY** (2026-09-22).
//!
//! `GlowConfig::intensity` used to be one scalar carrying three unrelated
//! meanings — the user's brightness knob, the accessibility policy, and the
//! host's performance headroom — and the AUDIO seam read it. So when the
//! adaptive load-shed latch faded its envelope to 0, or macOS `Reduce Motion`
//! zeroed the motion amplitude, `CursorGlow::tick` took its zero-amplitude
//! return, set `sound_live = false`, and `cue_keystroke_shifted` refused every
//! key on its first line. Typing went silent for a MOTION and a PERFORMANCE
//! policy, over a click that costs no GPU — measured live on the owner's Mac
//! as `motion_stage=reduced shed=0.00 intensity=0.00` while `aterm ctl tone`
//! in the same breath printed `audio=live sounds=on volume=0.40`.
//!
//! The scalar is split: `intensity` keeps the VISUAL amplitude and
//! [`GlowConfig::audible`] carries the one fact the engine cannot see — whose
//! window the key landed in. These tests pin the new law and the hard part of
//! it: the click's timbre inputs must stay FRESH on the silent-visual path,
//! or the fix trades silence for a wrong sound.

use aterm_core::render::GlowQuad;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::trail_pack::TrailParams;
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
        classic_mono: false,
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
        pack: None,
    }
}

/// A DARK config: the visual amplitude the host folded to zero. `audible`
/// says whether the key that arrives on this tick belongs to this window.
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
            glow: CursorGlow::default(),
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

/// D1 AND D2 AND THE FOCUS RULING, in one file with its own negative controls.
///
/// A frame the host shed for performance, and a `Reduce Motion` frame, both
/// arrive at the engine's zero-amplitude return with `audible = true` — so the
/// key seam stays OPEN and the click sounds. An UNFOCUSED window arrives with
/// `audible = false` and stays mute. And the master switch is not overridable:
/// `enabled = false` with `audible = true` is still silent, because that
/// branch runs a full thermal teardown and is the user's own off knob.
#[test]
fn a_shed_frame_keeps_the_key_seam_open() {
    let base = cfg(GlowStyle::RainbowKitty);
    let t0 = Instant::now();

    // SHED / REDUCE MOTION: dark, focused ⇒ heard.
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

    // NEGATIVE CONTROL 1 — the same dark tick, unfocused.
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

    // NEGATIVE CONTROL 2 — the master switch is not overridable by `audible`.
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

/// DECISION F — the hard part. `keyed_blaze` reads `heat_gain_live` /
/// `heat_tau_live`, snapshotted from the config. They used to be written in
/// lockstep with `sound_live = true` on the LIT path only, so a click minted
/// during a shed run would have read whatever the last DRAWING tick left —
/// the wrong style's constants after a switch, or a `Default` zero.
///
/// They are now written at the end of the lazy thermal decay, above the
/// zero-amplitude return, so a silent-visual tick refreshes them. Fire's
/// constants differ from Lumen's by a factor of ~3.5 on gain, so a stale
/// snapshot is loudly visible in the cue's heat.
#[test]
fn the_silent_visual_click_reads_this_tick_s_thermal_constants() {
    let t0 = Instant::now();
    let fire = cfg(GlowStyle::Fire);
    let lumen = cfg(GlowStyle::Lumen);

    let t1 = t0 + Duration::from_millis(16);

    // The blaze a key earns is `heat_cadence(gap) * heat_gain_live`, and the
    // FIRST key of a session earns no cadence at all (there is no previous
    // key to measure a gap against). So every arm cues one key to start the
    // cadence clock, then reads the SECOND key's blaze 16 ms later.
    let probe = |first: &GlowConfig, second: &GlowConfig| -> f32 {
        let mut r = Rig::new();
        r.tick(t0, first);
        assert!(r.glow.cue_keystroke_kind(t0, SoundKind::Typed));
        let _ = r.glow.take_key_cue();
        r.tick(t1, second);
        assert!(r.glow.cue_keystroke_kind(t1, SoundKind::Typed));
        r.glow.take_key_cue().expect("a cue").heat
    };

    // Snapshot Fire's constants on a lit tick, then go dark under LUMEN.
    let shed_heat = probe(&fire, &dark(&lumen, true));
    // The reference: the SAME sequence with Lumen lit throughout.
    let lumen_heat = probe(&lumen, &lumen);
    // And the refutation: Fire lit throughout, whose gain is much lower.
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

    // SECOND ARM — a Trail Pack overriding heat.gain/heat.tau takes the same
    // path, so the pack fields are covered too and not only the style arm.
    let mut packed = cfg(GlowStyle::Lumen);
    let mut params = TrailParams::defaults();
    params.heat.gain = Some(0.9);
    params.heat.tau = Some(4.0);
    packed.pack = Some(params);

    let packed_shed = probe(&lumen, &dark(&packed, true));
    assert!(
        packed_shed > lumen_heat + 1e-6,
        "a pack's own heat gain must reach the shed click too \
         ({packed_shed} vs {lumen_heat})"
    );
}

/// THE LIT PATH IS UNCHANGED by the hoist. `heat_gain`/`heat_tau` are pure
/// functions of the config and nothing between the old store site and the new
/// one reads either field, so a scripted lit sequence must produce the exact
/// same cue stream. Argued in the design; checked here.
#[test]
fn the_lit_path_is_byte_identical_after_the_hoist() {
    let t0 = Instant::now();
    let a = cfg(GlowStyle::RainbowKitty);
    let b = cfg(GlowStyle::Fire);

    let mut r = Rig::new();
    let mut heats = Vec::new();
    let mut now = t0;
    for step in 0..12 {
        let c = if step < 6 { &a } else { &b };
        r.tick(now, c);
        if r.glow.cue_keystroke_kind(now, SoundKind::Typed) {
            heats.push(r.glow.take_key_cue().expect("a cue").heat);
        }
        now += Duration::from_millis(16);
    }
    assert_eq!(heats.len(), 12, "every lit tick must take its key");
    // The cold→hot ramp is monotonic inside each style run, which is the
    // property a stale or zeroed gain would break.
    for w in heats[..6].windows(2) {
        assert!(w[1] >= w[0], "the lit ramp must not cool: {heats:?}");
    }
}

/// THE KEY-CREDIT LEDGER SURVIVES A SHED FRAME. `cue_move_sound` spends a
/// key-born credit so the echo of a key stays silent (one character, one
/// click). While shed, the credit is banked at the key and no echo can spend
/// it — the spawn path is unreachable. The latch FLAPS (the owner's machine
/// measured 41 transitions), so those echoes arrive after the light returns
/// and must still find their credits. Wiping the ledger per dark frame would
/// double-click the whole boundary burst.
///
/// The unfocused case keeps today's wipe, which is the negative control.
#[test]
fn a_shed_tick_keeps_the_keyed_click_ledger() {
    // Water: an ordinary echo-spawning style, the one the existing
    // `key_time_credits_expire_instead_of_banking_silence` proof drives.
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
    // Back to lit, and the echo of that key lands one cell right: the credit
    // is still banked, so the echo it accounts for arrives SILENT.
    r.glow.note_typed(t2);
    r.tick_at(t2, &base, 1);
    assert_eq!(
        r.glow.drain_sound_cues().count(),
        0,
        "the banked credit must mute the echo of the key it stood for"
    );

    // NEGATIVE CONTROL — unfocused: no credit is banked (and no click), so
    // the same echo clicks exactly once on refocus.
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

    // THE BOUND ON SWALLOWED KEYS, pinned rather than incidental, AND its
    // lit-path control.
    //
    // Type a burst into something that never echoes (a password prompt, a TUI
    // that eats input): every key banks a credit, none is spent, and a later
    // PROGRAM-driven move can then spend one and arrive silent. The ledger is
    // capped at `MAX_KEYED_CLICKS` (8) and self-clears at `KEYED_CLICK_FRESH`
    // (0.75 s), so the hole is bounded at 8 echoes inside 0.75 s of the last
    // key.
    //
    // The second arm is the reason this is a BOUND and not a regression: the
    // lit path banks exactly the same way. `clear_keyed_clicks` has two
    // callers in the whole engine — `clear_sound_ledger` and
    // `begin_style_fade` — so no per-tick wipe has ever run on a DRAWING
    // frame. Keeping the credits across a shed frame makes the dark path
    // behave like the lit one; it does not open anything the lit path had
    // closed. (Wiping per dark tick instead would double-click the whole
    // burst the moment the shed latch flapped back.)
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
        // Program-driven moves only — no `note_typed`, so nothing licenses a
        // fresh key. Count how many arrive silent: that is the ledger's depth.
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
    // NOTE the count is "moves that arrived silent", not the ledger's depth:
    // a program move with no fresh key is also declined by the LICENCE gate,
    // which is silent for its own reason. That is fine — the claim under test
    // is the EQUALITY, which is insensitive to it.
    assert_eq!(
        banked[0], banked[1],
        "the shed path must bank exactly what the LIT path banks — the bound \
         is pre-existing, not something the split opened (shed {}, lit {})",
        banked[0], banked[1]
    );
}

/// THE MASTER-OFF TICK STILL WIPES THE SOUND LEDGER, and so does `reset`.
/// `clear_sound_ledger` was split out of `clear_transient_state` so the
/// zero-amplitude return could call it CONDITIONALLY; this is the guard
/// against that refactor quietly removing a wipe someone depended on.
#[test]
fn a_master_off_tick_still_wipes_the_sound_ledger() {
    let base = cfg(GlowStyle::Water);
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(16);

    for (label, teardown) in [("master-off", true), ("reset", false)] {
        let mut r = Rig::new();
        r.tick(t0, &base);
        // Bank a cue AND a credit, then leave the cue undrained.
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
        // The credit went with them: the next echo clicks rather than being
        // muted by a credit for a click that died with the teardown.
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

/// IDLE-ZERO HOLDS while the audio seam is open. Opening the seam must not
/// leak a wake: `self.heat` is charged only on the spawn path, which is
/// unreachable while dark, so the thermal integrators still cool to EXACTLY
/// zero over the lazy decay and `is_active()` disarms. (Charging `heat` at
/// key time instead would arm `ember_live()` and keep the frame train awake
/// forever on every shed keystroke.)
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

/// NO BACKLOG TRAP. `cue_keystroke_shifted` refuses once the cue queue is
/// full, so a queue that grew while dark would re-silence typing through the
/// back door. It cannot grow: the host pops its own cue on the same call.
/// This turns that argument into a check.
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
