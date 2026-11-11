// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_glyph_rank_is_ordered_total_and_lossy() {
    assert_eq!(typed_glyph_rank(None), 0, "no key behind the cue");
    for (k, c) in ('a'..='z').enumerate() {
        assert_eq!(typed_glyph_rank(Some(c)), 1 + k as u8, "{c:?}");
        assert_eq!(
            typed_glyph_rank(Some(c.to_ascii_uppercase())),
            typed_glyph_rank(Some(c)),
            "{c:?}: case must fold"
        );
    }
    for (k, c) in ('0'..='9').enumerate() {
        assert_eq!(typed_glyph_rank(Some(c)), 27 + k as u8, "{c:?}");
    }
    let buckets: std::collections::BTreeSet<u8> = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"
        .chars()
        .map(|c| typed_glyph_rank(Some(c)))
        .collect();
    assert_eq!(
        buckets,
        (37..=44).collect::<std::collections::BTreeSet<u8>>(),
        "ASCII punctuation must populate all eight ranks 37..=44"
    );
    for c in ['漢', '£', 'é', '٣', ' ', '\t'] {
        assert_eq!(typed_glyph_rank(Some(c)), 45, "{c:?}");
    }
    for u in 0u32..0x3000 {
        if let Some(c) = char::from_u32(u) {
            assert!(typed_glyph_rank(Some(c)) <= 45, "{c:?} ranked out of range");
        }
    }
}

#[test]
fn the_glyph_class_table_is_one_for_host_bench_and_engine() {
    use glyph_class::*;
    assert_eq!(typed_glyph_class(None), LETTER, "no key behind the cue");
    for c in '0'..='9' {
        assert_eq!(typed_glyph_class(Some(c)), DIGIT, "{c:?}");
    }
    for c in (0x21u8..0x7f)
        .map(char::from)
        .filter(char::is_ascii_punctuation)
    {
        let class = typed_glyph_class(Some(c));
        assert!(
            (QMARK..=DASH).contains(&class) && class != DIGIT,
            "{c:?} fell out of the mark buckets ({class})"
        );
    }
    for c in ('a'..='z')
        .chain('A'..='Z')
        .chain([' ', 'é', '漢', '٣', '£', '\t'])
    {
        assert_eq!(typed_glyph_class(Some(c)), LETTER, "{c:?}");
    }
    for u in 0u32..0x3000 {
        if let Some(c) = char::from_u32(u) {
            assert!(
                typed_glyph_class(Some(c)) <= DASH,
                "{c:?} classed out of range"
            );
        }
    }
}

#[test]
fn an_explicit_instrument_auditions_itself_under_the_music_box() {
    use GlowStyle::{Lumen, RainbowKitty, Water};
    let synth = TrailSynth::new(48_000.0, 7);
    for (look, voice, v2) in [
        (RainbowKitty, SoundVoice::Marimba, false),
        (RainbowKitty, SoundVoice::Felt, false),
        (RainbowKitty, SoundVoice::Of(Water), false),
        (RainbowKitty, SoundVoice::Style, true),
        (RainbowKitty, SoundVoice::RainbowKittyV2, true),
        (RainbowKitty, SoundVoice::Of(RainbowKitty), true),
        (Lumen, SoundVoice::Style, false),
        (Lumen, SoundVoice::RainbowKittyV2, true),
        (Lumen, SoundVoice::Of(RainbowKitty), true),
    ] {
        let e = SoundEvent {
            voice,
            ..ev(look, SoundKind::Land)
        };
        assert_eq!(synth.v2_engaged(&e), v2, "{voice:?} under {look:?}");
    }
    assert!(
        !synth.v2_engaged(&SoundEvent {
            voice: SoundVoice::RainbowKittyV2,
            ..bonk(RainbowKitty)
        }),
        "the bonk is style-agnostic and never the music box's"
    );
}

#[test]
fn the_block_pre_roll_delays_every_voice_of_the_push_and_nothing_after() {
    const LEAD_S: f32 = 0.005;
    let typed = |style: GlowStyle| SoundEvent {
        bed: false,
        ..ev(style, SoundKind::Typed)
    };
    let onset = |style: GlowStyle, lead: f32| -> usize {
        let mut s = TrailSynth::new(48_000.0, 0xB10C_1EAD);
        s.push_meta(
            typed(style),
            EventMeta {
                at_ms: 1_000,
                block_lead_s: lead,
                ..EventMeta::default()
            },
        );
        assert_eq!(s.block_lead_s, 0.0, "the latch is cleared on the way out");
        let mut out = vec![0.0f32; 4 * 512 * CHANNELS];
        for block in out.chunks_mut(512 * CHANNELS) {
            s.render(block);
        }
        out.iter()
            .position(|&x| x != 0.0)
            .expect("a typed key sounds")
            / CHANNELS
    };
    for style in [GlowStyle::Lumen, GlowStyle::RainbowKitty] {
        let (t0, t1) = (onset(style, 0.0), onset(style, LEAD_S));
        let want = (LEAD_S * 48_000.0).round() as usize;
        assert!(
            t1.abs_diff(t0 + want) <= 1,
            "{style:?}: onset moved {} frames for a {want}-frame lead",
            t1 as i64 - t0 as i64
        );
    }
    let mut s = TrailSynth::new(48_000.0, 1);
    s.push_meta(
        typed(GlowStyle::Lumen),
        EventMeta {
            block_lead_s: f32::NAN,
            ..EventMeta::default()
        },
    );
    assert!(s.voices.iter().all(|v| !v.on), "a NaN pre-roll is refused");
    s.push_meta(
        typed(GlowStyle::Lumen),
        EventMeta {
            block_lead_s: 9.0,
            ..EventMeta::default()
        },
    );
    let pending = s
        .voices
        .iter()
        .filter(|v| v.on)
        .map(|v| -v.t)
        .fold(0.0f32, f32::max);
    assert!(
        pending <= BLOCK_LEAD_MAX_S + 1e-6,
        "a wild pre-roll is clamped to the ceiling, got {pending}"
    );
}

const fn ev(style: GlowStyle, kind: SoundKind) -> SoundEvent {
    SoundEvent {
        style,
        voice: SoundVoice::Style,
        kind: SoundGesture::Trail(kind),
        pan: 0.0,
        heat: 0.5,
        hue: 0.3,
        gain: 0.4,
        tone: Tone::Technical,
        bed: true,
        shifted: false,
    }
}

const fn bonk(style: GlowStyle) -> SoundEvent {
    SoundEvent {
        style,
        voice: SoundVoice::Style,
        kind: SoundGesture::Words(WordGesture::Bonk),
        pan: 0.0,
        heat: 0.5,
        hue: 0.3,
        gain: 0.4,
        tone: Tone::Technical,
        bed: true,
        shifted: false,
    }
}

const fn output(style: GlowStyle, gesture: OutputGesture) -> SoundEvent {
    SoundEvent {
        style,
        voice: SoundVoice::Style,
        kind: SoundGesture::Output(gesture),
        pan: 0.0,
        heat: 0.0,
        hue: 0.3,
        gain: 0.4,
        tone: Tone::Technical,
        bed: false,
        shifted: false,
    }
}

const fn toned(style: GlowStyle, kind: SoundKind, tone: Tone) -> SoundEvent {
    SoundEvent {
        tone,
        ..ev(style, kind)
    }
}

const fn reduce(mut r: f32) -> f32 {
    while r < 1.0 {
        r *= 2.0;
    }
    while r >= 2.0 {
        r /= 2.0;
    }
    r
}

fn assert_outside_bonk_zones(raw: f32, ctx: &str) {
    let r = reduce(raw);
    let m2_zone = r > 1.0 + 1e-4 && r < 1.09;
    let tritone_zone = r > 1.395 && r < 1.43;
    assert!(
        !m2_zone && !tritone_zone,
        "{ctx}: interval {r} lands in a bonk rub zone"
    );
}

const STYLES: [GlowStyle; 8] = [
    GlowStyle::Lumen,
    GlowStyle::Phaser,
    GlowStyle::Sparkle,
    GlowStyle::Fire,
    GlowStyle::Laser,
    GlowStyle::Beam,
    GlowStyle::Water,
    GlowStyle::Comet,
];

const LOOKS: [GlowStyle; 9] = [
    GlowStyle::Lumen,
    GlowStyle::Phaser,
    GlowStyle::RainbowKitty,
    GlowStyle::Sparkle,
    GlowStyle::Fire,
    GlowStyle::Laser,
    GlowStyle::Beam,
    GlowStyle::Water,
    GlowStyle::Comet,
];

const VOICES: &[SoundVoice] = SoundVoice::ALL;

const fn is_music_box(voice: SoundVoice, style: GlowStyle) -> bool {
    matches!(
        voice,
        SoundVoice::RainbowKittyV2 | SoundVoice::Of(GlowStyle::RainbowKitty)
    ) || (matches!(voice, SoundVoice::Style) && matches!(style, GlowStyle::RainbowKitty))
}

const KINDS: [SoundKind; 8] = [
    SoundKind::Typed,
    SoundKind::Backspace,
    SoundKind::Glide { dir: 1 },
    SoundKind::Navigation,
    SoundKind::Sweep { dir: 1 },
    SoundKind::Kill,
    SoundKind::Jump,
    SoundKind::Land,
];

#[test]
fn every_gesture_speaks_in_its_first_post_cue_buffer() {
    const BLOCK: usize = 512;
    const HEAD: usize = 16;
    for &voice in VOICES {
        for style in LOOKS {
            for kind in KINDS {
                // The music box has no landing voice (§12.3: the meteor's
                // bell is the landing), so there is no onset to time.
                if is_music_box(voice, style) && kind == SoundKind::Land {
                    continue;
                }
                let mut s = TrailSynth::new(48_000.0, 0x0A7E);
                let mut e = ev(style, kind);
                e.voice = voice;
                e.bed = false;
                s.push(e);
                let ctx = format!("{voice:?}/{style:?}/{kind:?}");
                assert!(s.live_voices() > 0, "{ctx}: cued but spawned nothing");
                let lead = s
                    .voices
                    .iter()
                    .filter(|v| v.on)
                    .map(|v| -v.t)
                    .fold(f32::MAX, f32::min);
                assert_eq!(
                    lead, 0.0,
                    "{ctx}: the LEADING voice must be immediate — a pre-delay \
                     here is keystroke latency no host can claw back"
                );
                let mut blk = vec![0.0f32; BLOCK * CHANNELS];
                s.render(&mut blk);
                assert!(
                    blk[..HEAD * CHANNELS].iter().any(|x| *x != 0.0),
                    "{ctx}: the first post-cue buffer must OPEN with signal"
                );
            }
        }
    }
}

#[test]
fn all_styles_sound_and_decay() {
    use GlowStyle::{Lumen, RainbowKitty};
    const TRAILS: [SoundGesture; 6] = [
        SoundGesture::Trail(SoundKind::Typed),
        SoundGesture::Trail(SoundKind::Backspace),
        SoundGesture::Trail(SoundKind::Navigation),
        SoundGesture::Trail(SoundKind::Kill),
        SoundGesture::Trail(SoundKind::Jump),
        SoundGesture::Words(WordGesture::Bonk),
    ];
    const WITH_GLIDE: [SoundGesture; 7] = [
        SoundGesture::Trail(SoundKind::Typed),
        SoundGesture::Trail(SoundKind::Backspace),
        SoundGesture::Trail(SoundKind::Navigation),
        SoundGesture::Trail(SoundKind::Kill),
        SoundGesture::Trail(SoundKind::Jump),
        SoundGesture::Trail(SoundKind::Glide { dir: 1 }),
        SoundGesture::Words(WordGesture::Bonk),
    ];
    let mut rows: Vec<(SoundVoice, &[GlowStyle], &[SoundGesture])> = vec![
        (SoundVoice::Style, &LOOKS, &TRAILS),
        (SoundVoice::Mech, &[Lumen, RainbowKitty], &WITH_GLIDE),
    ];
    for voice in NEW_VOICES {
        rows.push((voice, &[Lumen, RainbowKitty], &WITH_GLIDE));
    }
    for (voice, styles, gestures) in rows {
        for &style in styles {
            for &gesture in gestures {
                let what = format!("{voice:?}/{style:?}/{gesture:?}");
                let mut s = TrailSynth::new(48_000.0, 42);
                let mut e = voiced(voice, style, SoundKind::Typed);
                e.kind = gesture;
                s.push(e);
                let mut peak = 0.0f32;
                let mut buf = [0.0f32; 1024];
                for _ in 0..(6 * 48_000 * 2 / 1024) {
                    s.render(&mut buf);
                    for &x in &buf {
                        assert!(x.is_finite(), "{what} produced non-finite");
                        peak = peak.max(x.abs());
                    }
                }
                assert!(peak > 1e-4, "{what} was inaudible (peak {peak})");
                assert!(peak <= 0.98, "{what} clipped (peak {peak})");
                assert!(s.is_quiet(), "{what} never decayed to silence");
                s.render(&mut buf);
                assert!(buf.iter().all(|&x| x == 0.0), "{what} quiet but nonzero");
            }
        }
    }
}

const fn voiced(voice: SoundVoice, style: GlowStyle, kind: SoundKind) -> SoundEvent {
    SoundEvent {
        voice,
        ..ev(style, kind)
    }
}

fn isolated_peak(voice: SoundVoice, kind: SoundKind, shifted: bool) -> f32 {
    let mut s = TrailSynth::new(48_000.0, 0x5EED_1234);
    let mut e = voiced(voice, GlowStyle::RainbowKitty, kind);
    e.hue = 0.0;
    e.bed = false;
    e.shifted = shifted;
    s.push(e);
    let mut buf = vec![0.0f32; (48_000.0f32 * 2.4) as usize * CHANNELS];
    s.render(&mut buf);
    buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

const NEW_VOICES: [SoundVoice; 3] = [
    SoundVoice::Typewriter,
    SoundVoice::Marimba,
    SoundVoice::Felt,
];

#[test]
fn voice_roster_round_trips() {
    const AUTO: Option<SoundVoice> = SoundVoice::parse("\u{00a0}AUTO\u{2003}");
    assert_eq!(AUTO, Some(SoundVoice::Style));
    assert_eq!(SoundVoice::parse("auto"), Some(SoundVoice::Style));
    for &v in SoundVoice::ALL {
        assert_eq!(SoundVoice::parse(v.name()), Some(v), "{v:?}");
        for padding in ["  ", "\u{00a0}", "\u{2003}\u{3000}"] {
            assert_eq!(
                SoundVoice::parse(&format!(
                    "{padding}{}{padding}",
                    v.name().to_ascii_uppercase()
                )),
                Some(v)
            );
        }
    }
    for &(alias, voice) in SoundVoice::ALIASES {
        assert_eq!(SoundVoice::parse(alias), Some(voice), "alias {alias:?}");
        assert_eq!(
            SoundVoice::parse(&format!("\u{00a0}{}\u{2003}", alias.to_ascii_uppercase())),
            Some(voice),
            "padded alias {alias:?}"
        );
        assert!(
            SoundVoice::ALL.iter().all(|v| v.name() != alias),
            "alias {alias:?} shadows a canonical name"
        );
    }
    for junk in [
        "",
        "  ",
        "plasma",
        "glass",
        "auto bell",
        "\u{200b}auto",
        "auto\u{feff}",
        "warm  pluck",
        "music\u{00a0}box",
    ] {
        assert_eq!(SoundVoice::parse(junk), None, "{junk:?}");
    }
}

#[test]
fn standalone_voice_is_the_style_palette_verbatim() {
    let script = [
        SoundKind::Typed,
        SoundKind::Typed,
        SoundKind::Backspace,
        SoundKind::Navigation,
        SoundKind::Glide { dir: 1 },
        SoundKind::Sweep { dir: -1 },
        SoundKind::Kill,
        SoundKind::Jump,
        SoundKind::Land,
        SoundKind::Typed,
    ];
    let run = |voice: SoundVoice, look: GlowStyle| {
        let mut s = TrailSynth::new(48_000.0, 0x0574_111D);
        let mut out = Vec::new();
        let mut buf = [0.0f32; 960];
        for kind in script {
            s.push(voiced(voice, look, kind));
            for _ in 0..8 {
                s.render(&mut buf);
                out.extend_from_slice(&buf);
            }
        }
        s.push(SoundEvent {
            voice,
            ..bonk(look)
        });
        for _ in 0..40 {
            s.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    };
    for style in LOOKS {
        let reference = run(SoundVoice::Style, style);
        let other = if style == GlowStyle::Lumen {
            GlowStyle::Comet
        } else {
            GlowStyle::Lumen
        };
        let standalone = run(SoundVoice::Of(style), other);
        assert!(
            reference.len() == standalone.len()
                && reference
                    .iter()
                    .zip(&standalone)
                    .all(|(x, y)| x.to_bits() == y.to_bits()),
            "{style:?}: Of({style:?}) under {other:?} must be bit-identical to Style"
        );
    }
}

/// NEGATIVE CONTROL: unpitched percussive/crackle voices (`Mech`, `Typewriter`, `Of(Fire)`)
/// keep identical partial frequencies when `walk` changes under a fixed RNG seed.
#[test]
fn every_pitched_voice_lands_on_the_melody_note() {
    for voice in [
        SoundVoice::Marimba,
        SoundVoice::Felt,
        SoundVoice::Of(GlowStyle::Lumen),
    ] {
        let (ts, typed) = family_voices_in(voice, GlowStyle::Lumen, SoundKind::Typed, false);
        assert!(!typed.is_empty(), "{voice:?} speaks");
        let anchor = palette_for(voice, GlowStyle::Lumen).anchor_hz();
        let expect_t = ts.melody_hz(anchor, ts.walk);
        assert!(
            typed
                .iter()
                .any(|&(t_land, t_enter)| t_enter < t_land && (t_land - expect_t).abs() < 0.5),
            "{voice:?}: upward-gliding voice must land on {expect_t}, got {typed:?}"
        );
    }

    let (ts, beam) = family_voices_in(
        SoundVoice::Of(GlowStyle::Beam),
        GlowStyle::Lumen,
        SoundKind::Typed,
        false,
    );
    let expect_beam = ts.melody_hz(330.0, (ts.walk / 2) * 2);
    assert!(
        beam.iter()
            .any(|&(t_land, t_enter)| { t_enter > t_land && (t_land - expect_beam).abs() < 0.5 }),
        "Beam: blip must land on {expect_beam} from above, got {beam:?}"
    );

    for style in [GlowStyle::Phaser, GlowStyle::Classic] {
        let voice = SoundVoice::Of(style);
        let (ts, typed) = family_voices_in(voice, GlowStyle::Lumen, SoundKind::Typed, false);
        let expect_t = ts.melody_hz(392.0, (0.3_f32 * 5.0) as i32);
        assert!(
            typed
                .iter()
                .any(|&(t_land, t_enter)| t_enter > t_land && (t_land - expect_t).abs() < 0.5),
            "{voice:?}: phaser voice must land on hue degree {expect_t} from above, got {typed:?}"
        );
    }

    let (ts, sparkle) = family_voices_in(
        SoundVoice::Of(GlowStyle::Sparkle),
        GlowStyle::Lumen,
        SoundKind::Typed,
        false,
    );
    let sp0 = ts.melody_hz(523.25, ts.walk);
    let sp1 = ts.melody_hz(523.25, ts.walk + 1);
    assert!(
        sparkle.iter().any(|&(t_land, t_enter)| {
            (t_enter - t_land).abs() < 1e-4
                && ((t_land - sp0).abs() < 0.5 || (t_land - sp1).abs() < 0.5)
        }),
        "Sparkle: struck chime must sit on {sp0} or {sp1}, got {sparkle:?}"
    );

    let (ts, comet) = family_voices_in(
        SoundVoice::Of(GlowStyle::Comet),
        GlowStyle::Lumen,
        SoundKind::Typed,
        false,
    );
    let expect_comet = ts.melody_hz(220.0, ts.walk);
    assert!(
        comet.iter().any(|&(t_land, t_enter)| {
            (t_land - expect_comet * 0.97).abs() < 0.5
                && (t_enter - expect_comet * 1.04).abs() < 0.5
        }),
        "Comet: sweep must bracket {expect_comet}, got {comet:?}"
    );

    let (ts, water) = family_voices_in(
        SoundVoice::Of(GlowStyle::Water),
        GlowStyle::Lumen,
        SoundKind::Typed,
        false,
    );
    let expect_water = ts.melody_hz(430.0, ts.walk);
    assert!(
        water.iter().any(|&(t_land, t_enter)| {
            (t_land - expect_water * 1.25).abs() < 0.5
                && (t_enter - expect_water * 0.85).abs() < 0.5
        }),
        "Water: droplet must sweep across {expect_water}, got {water:?}"
    );

    let (ts, laser) = family_voices_in(
        SoundVoice::Of(GlowStyle::Laser),
        GlowStyle::Lumen,
        SoundKind::Typed,
        false,
    );
    let expect_laser = ts.melody_hz(880.0, ts.walk.min(5));
    assert!(
        laser.iter().any(|&(t_land, t_enter)| {
            (t_land - expect_laser * 0.25).abs() < 0.5 && (t_enter - expect_laser).abs() < 0.5
        }),
        "Laser: zap must sweep from {expect_laser} to its two-octave drop, got {laser:?}"
    );

    for voice in [
        SoundVoice::RainbowKittyV2,
        SoundVoice::Of(GlowStyle::RainbowKitty),
    ] {
        let mut s = TrailSynth::new(48_000.0, 0xFA_1117);
        s.push(SoundEvent {
            voice,
            ..ev(GlowStyle::Lumen, SoundKind::Typed)
        });
        let expect_v2 = penta(
            RainbowKittyV2Palette::anchor_hz(),
            i32::from(s.v2.walk()) + i32::from(s.song_key),
        );
        assert!(
            s.voices.iter().any(|v| {
                v.on && v.p[0].lvl > 0.0
                    && v.p[0].glide == 0.0
                    && (v.p[0].f0 - expect_v2).abs() < 0.5
            }),
            "{voice:?}: music-box tine must strike {expect_v2}"
        );
    }

    // Negative control: unpitched voices ignore the walk degree.
    let unpitched_partials = |voice: SoundVoice, forced_walk: i32| -> Vec<(u32, u32)> {
        let mut s = TrailSynth::new(48_000.0, 0xFA_1117);
        s.since_event = 0.1;
        s.since_voice = 1.0;
        s.song_pulse = 1; // ghost slot so `advance_song` preserves `s.walk`
        s.walk = forced_walk;
        s.push(SoundEvent {
            voice,
            ..ev(GlowStyle::Lumen, SoundKind::Typed)
        });
        assert_eq!(s.walk, forced_walk, "fixture must preserve forced walk");
        s.voices
            .iter()
            .filter(|v| v.on)
            .flat_map(|v| v.p)
            .filter(|p| p.lvl > 0.0)
            .map(|p| (p.f1.to_bits(), p.f0.to_bits()))
            .collect()
    };
    assert_ne!(
        unpitched_partials(SoundVoice::Marimba, 0),
        unpitched_partials(SoundVoice::Marimba, 4),
        "probe must distinguish distinct walk degrees on a pitched voice"
    );
    for voice in [
        SoundVoice::Mech,
        SoundVoice::Typewriter,
        SoundVoice::Of(GlowStyle::Fire),
    ] {
        let w0 = unpitched_partials(voice, 0);
        assert!(!w0.is_empty(), "{voice:?} spawns a pitched body partial");
        assert_eq!(
            w0,
            unpitched_partials(voice, 4),
            "{voice:?}: unpitched voice must be invariant to the melody walk"
        );
    }
}

#[test]
fn new_voice_beds_are_silent_for_keyboards_and_designed_for_instruments() {
    let run = |voice: SoundVoice, bed: bool| {
        let mut s = TrailSynth::new(48_000.0, 99);
        let mut out = Vec::new();
        let mut buf = [0.0f32; 960];
        for _ in 0..20 {
            let mut e = voiced(voice, GlowStyle::RainbowKitty, SoundKind::Typed);
            e.bed = bed;
            s.push(e);
            for _ in 0..4 {
                s.render(&mut buf);
                out.extend_from_slice(&buf);
            }
        }
        out
    };
    for (voice, silent) in [
        (SoundVoice::Mech, true),
        (SoundVoice::Typewriter, true),
        (SoundVoice::Marimba, false),
        (SoundVoice::Felt, false),
    ] {
        let (on, off) = (run(voice, true), run(voice, false));
        let same = on.iter().zip(&off).all(|(x, y)| x.to_bits() == y.to_bits());
        assert_eq!(
            same, silent,
            "{voice:?}: a keyboard's bed contributes exactly zero samples; an \
             instrument's designed bed must change the render"
        );
    }
}

#[test]
fn flood_is_ducked() {
    struct Flood {
        label: String,
        event: SoundEvent,
        seed: u32,
        blocks_per_key: usize,
        pan_sweep: bool,
        relative: bool,
    }
    let palette = |label: String, event: SoundEvent| Flood {
        label,
        event,
        seed: 3,
        blocks_per_key: 2,
        pan_sweep: true,
        relative: true,
    };
    let voice = |v: SoundVoice| Flood {
        label: format!("{v:?}"),
        event: voiced(v, GlowStyle::Lumen, SoundKind::Typed),
        seed: 7,
        blocks_per_key: 4,
        pan_sweep: false,
        relative: false,
    };
    let mut rows: Vec<Flood> = LOOKS
        .iter()
        .map(|&st| palette(format!("{st:?}"), ev(st, SoundKind::Typed)))
        .collect();
    rows.push(voice(SoundVoice::Mech));
    rows.extend(NEW_VOICES.map(voice));
    for row in rows {
        let label = &row.label;
        let synth = || TrailSynth::new(48_000.0, row.seed);
        let mut buf = [0.0f32; 960];
        let mut s = synth();
        let mut flood = 0.0f32;
        for i in 0..75 {
            let mut e = row.event;
            if row.pan_sweep {
                e.pan = ((i % 20) as f32) / 10.0 - 1.0;
            }
            s.push(e);
            for _ in 0..row.blocks_per_key {
                s.render(&mut buf);
                for &x in &buf {
                    assert!(x.is_finite(), "{label}: flood produced non-finite");
                    flood = flood.max(x.abs());
                }
            }
        }
        if row.relative {
            let mut s1 = synth();
            s1.push(row.event);
            let mut single = 0.0f32;
            for _ in 0..100 {
                s1.render(&mut buf);
                for &x in &buf {
                    single = single.max(x.abs());
                }
            }
            assert!(
                flood <= single * 2.5 + MASTER * (0.05 / 0.9),
                "{label} flood peak {flood} vs single {single} — governor failed"
            );
        }
        assert!(flood < 0.5, "{label} flood absolute peak too hot: {flood}");
    }
}

#[test]
fn a_drained_cue_batch_speaks_once_however_deep_the_backlog() {
    for style in LOOKS {
        let one = {
            let mut s = TrailSynth::new(48_000.0, 11);
            s.push(ev(style, SoundKind::Typed));
            s.live_voices()
        };
        assert!(one > 0, "{style:?}: a lone keystroke must speak");
        for depth in [8usize, 24] {
            let mut s = TrailSynth::new(48_000.0, 11);
            for _ in 0..depth {
                s.push(ev(style, SoundKind::Typed));
            }
            if style == GlowStyle::RainbowKitty {
                let mut buf = [0.0f32; 960];
                let mut peak = 0.0f32;
                for _ in 0..4 {
                    s.render(&mut buf);
                    for &x in &buf {
                        peak = peak.max(x.abs());
                    }
                }
                let mut lone = TrailSynth::new(48_000.0, 11);
                lone.push(ev(style, SoundKind::Typed));
                let mut single = 0.0f32;
                for _ in 0..4 {
                    lone.render(&mut buf);
                    for &x in &buf {
                        single = single.max(x.abs());
                    }
                }
                assert!(
                    peak <= single * 2.5 + MASTER * (0.05 / 0.9),
                    "{style:?}: {depth} keystroke cues drained together peaked \
                     at {peak} against one key's {single} — the lane cap and \
                     the limiter are what must hold this, since the melody \
                     may no longer thin a key"
                );
                assert!(
                    peak < 0.5,
                    "{style:?}: {depth}-deep batch absolute peak too hot: {peak}"
                );
                continue;
            }
            assert_eq!(
                s.live_voices(),
                one,
                "{style:?}: {depth} keystroke cues drained together must \
                 speak exactly as loudly as one — the min-gap governor \
                 thins the rest, so backlog depth cannot buy a click"
            );
        }
    }
}

#[test]
fn bed_off_events_never_energise_the_bed_and_the_setting_reenables() {
    for style in STYLES {
        let mut s = TrailSynth::new(48_000.0, 7);
        let bedless = SoundEvent {
            bed: false,
            ..ev(style, SoundKind::Typed)
        };
        s.push(bedless);
        let mut buf = [0.0f32; 960];
        let mut peak = 0.0f32;
        for _ in 0..50 {
            s.render(&mut buf);
            peak = buf.iter().fold(peak, |m, x| m.max(x.abs()));
        }
        assert!(
            peak > 1e-4,
            "{style:?}: the note itself must still speak with the bed off"
        );
        assert_eq!(
            s.debug_bed(),
            (0.0, 0.0),
            "{style:?}: the bed floor never lifts under a bed-off event"
        );
        s.push(SoundEvent {
            bed: true,
            ..bedless
        });
        assert!(
            s.debug_bed().0 > 0.0,
            "{style:?}: a bed-on event re-energises the layer immediately"
        );
    }
}

#[test]
fn sky_bed_pitches_stay_on_the_active_lattice_for_every_tone() {
    let mut bed_degrees: Vec<i32> = Vec::new();
    for chord in 0..rainbow_kitty_v2::SKY_BED_CHORDS {
        bed_degrees.extend(rainbow_kitty_v2::sky_bed_degrees(chord));
    }
    let melody_degrees: Vec<i32> = (-2..=10).collect();
    for tone in Tone::ALL {
        let mut s = TrailSynth::new(48_000.0, 1);
        s.tone = tone;
        for &bd in &bed_degrees {
            for &md in bed_degrees.iter().chain(&melody_degrees) {
                let r = s.melody_hz(330.0, bd) / s.melody_hz(330.0, md);
                let ctx = format!("{tone:?} bed deg {bd} vs deg {md}");
                assert_outside_bonk_zones(r, &ctx);
                assert_outside_bonk_zones(2.0 / reduce(r), &format!("{ctx} (inversion)"));
            }
        }
    }
}

#[test]
fn nonfinite_event_fields_are_rejected_before_state_mutation() {
    for kind in [
        SoundGesture::Trail(SoundKind::Jump),
        SoundGesture::Words(WordGesture::Bonk),
    ] {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for field in 0..4 {
                let mut s = TrailSynth::new(48_000.0, 17);
                let mut e = ev(GlowStyle::Water, SoundKind::Jump);
                e.kind = kind;
                match field {
                    0 => e.pan = bad,
                    1 => e.heat = bad,
                    2 => e.hue = bad,
                    3 => e.gain = bad,
                    _ => unreachable!(),
                }
                s.push(e);
                assert!(s.is_quiet(), "field {field}={bad:?} mutated synth state");
                assert_eq!(s.live_voices(), 0);
                let mut buf = [1.0f32; 512];
                s.render(&mut buf);
                assert!(
                    buf.iter()
                        .all(|sample| sample.is_finite() && *sample == 0.0)
                );
            }
        }
    }
}

#[test]
fn bonk_bypasses_min_gap_thinning_like_jump() {
    let mut s = TrailSynth::new(48_000.0, 21);
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    let after_first = s.live_voices();
    assert!(after_first >= 1);
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert_eq!(s.live_voices(), after_first, "typed within gap must thin");
    s.push(bonk(GlowStyle::Lumen));
    assert_eq!(
        s.live_voices(),
        after_first + 2,
        "bonk within gap must be admitted"
    );
}

#[test]
fn output_cues_bypass_but_never_claim_the_human_voice_gap() {
    let mut s = TrailSynth::new(48_000.0, 0x0A17_0A17);
    s.push(ev(GlowStyle::Lumen, SoundKind::Jump));
    let after_enter = s.live_voices();
    let musical = |s: &TrailSynth| {
        (
            s.rng,
            s.walk,
            s.theme_pos,
            s.phrase_idx,
            s.bass_step,
            s.song_pulse,
            s.song_notes,
        )
    };
    let musical_state = musical(&s);
    let human_gap = s.since_voice;
    assert!(
        human_gap < MIN_GAP,
        "Enter must actually occupy the shared human admission gap"
    );

    s.push(output(GlowStyle::Lumen, OutputGesture::Shimmer));
    assert_eq!(s.live_voices(), after_enter + 1, "Shimmer must be admitted");
    assert_eq!(
        musical(&s),
        musical_state,
        "machine output must not rewrite the human melody"
    );
    assert_eq!(
        s.since_voice, human_gap,
        "machine output must not claim the next human admission slot"
    );

    s.since_voice = MIN_GAP;
    s.push(output(GlowStyle::Lumen, OutputGesture::Shimmer));
    let after_output = s.live_voices();
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert!(
        s.live_voices() > after_output,
        "an unauthored cue cannot steal the following key's beat"
    );
}

#[test]
fn typing_transposes_with_the_song_and_returns_when_it_ends() {
    let hz_of = |s: &TrailSynth| {
        s.voices
            .iter()
            .filter(|v| v.on)
            .flat_map(|v| v.p.iter())
            .filter(|p| p.lvl > 0.0)
            .map(|p| p.f1)
            .fold(0.0f32, f32::max)
    };
    let mut plain = TrailSynth::new(48_000.0, 7);
    plain.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    let neutral = hz_of(&plain);
    assert!(neutral > 0.0, "the typed note speaks");

    const SIG_IN_KEY_TWO: u32 = 4;
    let bar = SoundEvent {
        kind: SoundGesture::Celebration(CelebrationGesture::riff_bar(0, SIG_IN_KEY_TWO)),
        ..ev(GlowStyle::Lumen, SoundKind::Typed)
    };
    let mut singing = TrailSynth::new(48_000.0, 7);
    singing.push(bar);
    assert_eq!(
        i32::from(singing.song_key),
        celebration_root(SIG_IN_KEY_TWO) + celebration_mode(SIG_IN_KEY_TWO),
        "the riff latches the key it actually transposes by (root + mode)"
    );
    let mut after = TrailSynth::new(48_000.0, 7);
    after.song_key = 2;
    after.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    let transposed = hz_of(&after);
    assert!(
        transposed > neutral * 1.05,
        "a typed note rides the song's key: {neutral} -> {transposed}"
    );

    let mut busy = TrailSynth::new(48_000.0, 7);
    busy.push(bar);
    assert_ne!(busy.song_key, 0, "the song latched a key");
    let mut blk = vec![0.0f32; 480 * CHANNELS];
    for i in 0..1_200 {
        if i % 5 == 0 {
            busy.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        }
        busy.render(&mut blk);
        assert!(!busy.is_quiet(), "the fixture never falls silent");
    }
    assert_eq!(
        busy.song_key, 0,
        "the borrowed key is released with the sing duck, not with silence"
    );
}

#[test]
fn bonk_is_discordant_against_the_walk_and_moves_nothing() {
    let mut s = TrailSynth::new(48_000.0, 33);
    let walk_before = s.walk;
    let root = penta(523.25, walk_before);
    s.push(bonk(GlowStyle::RainbowKitty));
    assert_eq!(s.walk, walk_before, "a bonk must not step the melody walk");
    assert_eq!(
        s.debug_bed(),
        (0.0, 0.0),
        "a bonk must not swell the ambient bed"
    );
    let clash: Vec<f32> = s
        .voices
        .iter()
        .filter(|v| v.on && v.duck_exempt)
        .flat_map(|v| v.p.iter())
        .filter(|p| p.lvl > 0.0 && p.f1 > root * 0.9)
        .map(|p| p.f1 / root)
        .collect();
    for ratio in [BONK_MINOR_SECOND, BONK_TRITONE] {
        assert!(
            clash.iter().any(|r| (r - ratio).abs() < 1e-3),
            "missing the {ratio} clash: {clash:?}"
        );
    }
}

#[test]
fn bonk_ducks_the_melody_around_it() {
    let build = || {
        let mut s = TrailSynth::new(48_000.0, 77);
        let mut buf = [0.0f32; 960];
        for _ in 0..6 {
            s.push(ev(GlowStyle::Beam, SoundKind::Typed));
            for _ in 0..4 {
                s.render(&mut buf);
            }
        }
        s
    };
    let mut plain = build();
    let mut bonked = build();
    bonked.push(bonk(GlowStyle::Beam));
    assert_eq!(bonked.duck, 1.0, "bonk admission must arm the duck");
    assert_eq!(
        plain.debug_bed(),
        bonked.debug_bed(),
        "the bonk must leave the bed untouched"
    );
    let mut skip = [0.0f32; 960];
    for _ in 0..32 {
        plain.render(&mut skip);
        bonked.render(&mut skip);
    }
    assert_eq!(bonked.live_voices(), 0, "bonk voices must be dead by now");
    let mut e_plain = 0.0f64;
    let mut e_bonked = 0.0f64;
    let mut buf = [0.0f32; 960];
    for _ in 0..20 {
        plain.render(&mut buf);
        for &x in &buf {
            e_plain += f64::from(x) * f64::from(x);
        }
        bonked.render(&mut buf);
        for &x in &buf {
            e_bonked += f64::from(x) * f64::from(x);
        }
    }
    assert!(
        e_bonked < e_plain * 0.9,
        "duck failed to dip the melody: bonked {e_bonked} vs plain {e_plain}"
    );
    for _ in 0..300 {
        bonked.render(&mut buf);
    }
    assert_eq!(bonked.duck, 0.0, "the duck must snap back to exact rest");
}

const fn riff_sig(style: GlowStyle, bar: u16, sig: u32) -> SoundEvent {
    SoundEvent {
        style,
        voice: SoundVoice::Style,
        kind: SoundGesture::Celebration(CelebrationGesture::riff_bar(bar, sig)),
        pan: 0.0,
        heat: 1.0,
        hue: 0.0,
        gain: 0.4,
        tone: Tone::Technical,
        bed: false,
        shifted: false,
    }
}

const fn riff(style: GlowStyle, bar: u16) -> SoundEvent {
    riff_sig(style, bar, crate::kitty_sing::NEUTRAL_SIGNATURE)
}

#[test]
fn celebration_bypasses_min_gap_thinning() {
    let mut s = TrailSynth::new(48_000.0, 5);
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
    let before = s.live_voices();
    s.push(riff(GlowStyle::RainbowKitty, 0));
    assert_eq!(
        s.live_voices(),
        before + 8,
        "the whole bar must be admitted despite the gap"
    );
    let mut s = TrailSynth::new(48_000.0, 3);
    s.push(SoundEvent {
        bed: true,
        ..riff(GlowStyle::RainbowKitty, 0)
    });
    assert_eq!(
        s.debug_bed(),
        (0.0, 0.0),
        "a riff bar must not swell the ambience it plays over"
    );
}

#[test]
fn celebration_ducks_the_melody_and_hands_back() {
    let quiet_note = |sing: bool| {
        let mut s = TrailSynth::new(48_000.0, 9);
        s.push(SoundEvent {
            bed: false,
            ..ev(GlowStyle::RainbowKitty, SoundKind::Typed)
        });
        if sing {
            s.sing = 1.0;
            s.sing_hold = 10.0;
        }
        let mut e = 0.0f64;
        let mut buf = [0.0f32; 960];
        for _ in 0..10 {
            s.render(&mut buf);
            for &x in &buf {
                e += f64::from(x) * f64::from(x);
            }
        }
        e
    };
    let open = quiet_note(false);
    let ducked = quiet_note(true);
    assert!(
        ducked < open * 0.4,
        "sing duck must attenuate the melody: {ducked} vs {open}"
    );

    let mut s = TrailSynth::new(48_000.0, 9);
    s.push(riff(GlowStyle::RainbowKitty, 0));
    assert_eq!(s.sing, 1.0, "a riff bar must arm the sing duck");
    assert!(s.sing_hold > 0.0, "…and hold it for the bar");
    let mut buf = [0.0f32; 960];
    for block in 0..700 {
        if block % 20 == 0 {
            s.push(SoundEvent {
                bed: false,
                ..ev(GlowStyle::RainbowKitty, SoundKind::Navigation)
            });
        }
        s.render(&mut buf);
    }
    assert_eq!(s.sing, 0.0, "the sing duck must hand back to exact rest");
}

const fn outro_sig(sig: u32) -> SoundEvent {
    SoundEvent {
        kind: SoundGesture::Celebration(CelebrationGesture::outro(sig)),
        ..riff_sig(GlowStyle::RainbowKitty, 0, sig)
    }
}

#[test]
fn the_outro_s_lead_is_do_and_holds_the_sing_duck() {
    let sig = crate::kitty_sing::song_signature('G');
    let mut s = TrailSynth::new(48_000.0, 9);
    s.push(riff_sig(GlowStyle::RainbowKitty, 0, sig));
    s.push(outro_sig(sig));
    let shift = celebration_root(sig) + celebration_mode(sig);
    let want = s.melody_hz(CELEBRATION_BASE_HZ, shift);
    let sub = s.melody_hz(CELEBRATION_BASE_HZ, CELEBRATION_OUTRO_SUB_DEG + shift);
    let lead = s
        .voices
        .iter()
        .filter(|v| v.on && v.celebration && v.duck_exempt)
        .find(|v| (v.p[0].f0 - want).abs() < 1e-3)
        .expect("the outro's lead sounds on do in the run's key");
    assert!((lead.p[2].f0 - sub).abs() < 1e-3, "…over the bass do");
    assert_eq!(s.sing, 1.0, "the outro holds the sing duck");
    assert!(
        (s.sing_hold - CELEBRATION_OUTRO_S).abs() < 1e-6,
        "…for exactly its own length, then hands back"
    );
}

#[test]
fn rain_glints_are_muted_under_the_sing() {
    let glint = |style| SoundEvent {
        bed: false,
        ..ev(style, SoundKind::Stardust { twinkle_hz: 0 })
    };
    let mut s = TrailSynth::new(48_000.0, 9);
    let quiet = s.live_voices();
    s.push(glint(GlowStyle::RainbowKitty));
    assert!(
        s.live_voices() > quiet,
        "precondition: a glint sounds when nothing sings"
    );

    let mut s = TrailSynth::new(48_000.0, 9);
    s.push(riff(GlowStyle::RainbowKitty, 0));
    assert!(s.sing > 0.0);
    let under = s.live_voices();
    s.push(glint(GlowStyle::RainbowKitty));
    assert_eq!(
        s.live_voices(),
        under,
        "under the sing the glint is dropped"
    );

    let mut s = TrailSynth::new(48_000.0, 9);
    s.sing = 1.0;
    s.sing_hold = 10.0;
    let other = s.live_voices();
    s.push(glint(GlowStyle::Sparkle));
    assert!(
        s.live_voices() > other,
        "another palette's glint is not the music box's"
    );
}

#[test]
fn celebration_bass_always_has_a_carrier() {
    for (b, (lead, bass)) in CELEBRATION_PHRASE.iter().zip(&CELEBRATION_BASS).enumerate() {
        for (beat, &deg) in bass.iter().enumerate() {
            if deg != REST {
                assert_ne!(
                    lead[beat * 2],
                    REST,
                    "bar {b} beat {beat}: bass note has no carrier voice"
                );
            }
        }
    }
}

#[test]
fn celebration_bar_fits_the_polyphony_budget() {
    for (b, bar) in CELEBRATION_PHRASE.iter().enumerate() {
        let mut n = bar.iter().filter(|d| **d != REST).count();
        if b == CELEBRATION_PHRASE_BARS - 1 {
            n += CELEBRATION_FILL.len();
        }
        assert!(n <= 10, "bar {b} spawns {n} voices — over the riff budget");
    }
}

#[test]
fn celebration_riff_follows_the_active_tone_lattice() {
    let render_toned = |tone: Tone| {
        let mut s = TrailSynth::new(48_000.0, 42);
        s.tone = tone;
        s.push(riff(GlowStyle::RainbowKitty, 0));
        let mut acc = 0u64;
        let mut buf = [0.0f32; 960];
        for _ in 0..160 {
            s.render(&mut buf);
            for &x in &buf {
                assert!(x.is_finite(), "{tone:?} riff produced non-finite");
                acc = acc.rotate_left(7).wrapping_add(u64::from(x.to_bits()));
            }
        }
        acc
    };
    let technical = render_toned(Tone::Technical);
    for tone in [Tone::Excited, Tone::Frustrated, Tone::Playful] {
        assert_ne!(
            render_toned(tone),
            technical,
            "{tone:?} must transpose the riff onto its own consonant lattice"
        );
    }
}

#[test]
fn key_switch_damps_the_old_keys_ring() {
    let t = crate::kitty_sing::song_signature('t');
    let e = crate::kitty_sing::song_signature('e');
    assert_ne!(t, e, "fixture: two different songs");
    let mut s = TrailSynth::new(48_000.0, 42);
    let mut buf = [0.0f32; 960];
    for bar in [3u16, 4] {
        s.push(riff_sig(GlowStyle::RainbowKitty, bar, t));
        assert!(
            s.voices.iter().all(|v| v.damp == 0.0),
            "bar {bar}: a same-sig bar must never damp the ringing tails"
        );
        for _ in 0..160 {
            s.render(&mut buf);
        }
    }
    let ringing = s.voices.iter().filter(|v| v.on && v.celebration).count();
    assert!(
        ringing > 0,
        "fixture: the old key's tails must still ring at the boundary"
    );
    s.push(riff_sig(GlowStyle::RainbowKitty, 5, e));
    assert_eq!(
        s.voices.iter().filter(|v| v.on && v.damp > 0.0).count(),
        ringing,
        "every old tail must be ordered into the release"
    );
    for _ in 0..10 {
        s.render(&mut buf);
    }
    assert_eq!(
        s.voices.iter().filter(|v| v.on && v.damp > 0.0).count(),
        0,
        "the old key's ring must be silent within 100 ms of the boundary"
    );
    assert!(
        s.voices.iter().any(|v| v.on && v.celebration) && buf.iter().any(|&x| x != 0.0),
        "while the new verse sings on"
    );

    let mut s2 = TrailSynth::new(48_000.0, 42);
    s2.push(riff_sig(GlowStyle::RainbowKitty, 4, t));
    s2.push(riff_sig(GlowStyle::RainbowKitty, 5, e));
    for _ in 0..10 {
        s2.render(&mut buf);
    }
    assert_eq!(
        s2.voices.iter().filter(|v| v.on && v.damp > 0.0).count(),
        0,
        "an unstarted old-key voice must expire unheard, not sound late"
    );
}

#[test]
fn cross_key_difference() {
    let intervals = |sig: u32| -> Vec<i32> {
        let mut degs = Vec::new();
        for (idx, &is_verse) in CELEBRATION_VERSE.iter().enumerate() {
            if is_verse {
                degs.extend(
                    celebration_bar_degrees(sig, idx)
                        .into_iter()
                        .filter(|&d| d != REST),
                );
            }
        }
        degs.windows(2).map(|w| w[1] - w[0]).collect()
    };
    let keys = ['w', 'a', 'z'];
    for (i, &a) in keys.iter().enumerate() {
        for &b in &keys[i + 1..] {
            assert_ne!(
                intervals(crate::kitty_sing::song_signature(a)),
                intervals(crate::kitty_sing::song_signature(b)),
                "{a:?} and {b:?} must sing different verse contours"
            );
        }
    }
}

#[test]
fn fill_carries_root() {
    let sig = 24u32;
    assert_eq!(celebration_root(sig), 2);
    assert_eq!(celebration_mode(sig), 0);
    let mut s = TrailSynth::new(48_000.0, 3);
    s.push(riff_sig(GlowStyle::RainbowKitty, 7, sig));
    let mut fills: Vec<&Voice> = s
        .voices
        .iter()
        .filter(|v| v.on && v.dur.to_bits() == CELEBRATION_EIGHTH.to_bits())
        .collect();
    fills.sort_by(|a, b| a.delay.total_cmp(&b.delay));
    assert_eq!(fills.len(), CELEBRATION_FILL.len(), "four fill sixteenths");
    for (k, v) in fills.iter().enumerate() {
        let deg = CELEBRATION_FILL[k];
        assert_eq!(
            v.p[0].f0.to_bits(),
            penta(CELEBRATION_BASE_HZ, deg + 2).to_bits(),
            "fill note {k} must sing in the held key's root"
        );
        assert_ne!(
            v.p[0].f0.to_bits(),
            penta(CELEBRATION_BASE_HZ, deg).to_bits(),
            "regression guard: fill note {k} fell back to the reference key"
        );
    }
}

#[test]
fn verse_walk_always_moves_and_keeps_rhythm_and_register() {
    for deg in &CELEBRATION_FILL {
        assert!((0..=9).contains(deg), "fill degree {deg} out of range");
    }
    for deg in CELEBRATION_BASS.iter().flatten().filter(|d| **d != REST) {
        assert!(
            (-12..=-1).contains(deg),
            "bass degree {deg} outside the bass register"
        );
    }
    let sigs: Vec<u32> = "wazqx09~ \t"
        .chars()
        .map(crate::kitty_sing::song_signature)
        .chain([0, 1, 5, 11, 12, 24, 0xDEAD_BEEF, 0x7FFF_FFFF, u32::MAX])
        .collect();
    for &sig in &sigs {
        assert!((-2..=2).contains(&celebration_root(sig)), "root guard");
        assert!(MODE_ROTATIONS.contains(&celebration_mode(sig)));
        for idx in 0..CELEBRATION_PHRASE_BARS {
            let row = celebration_bar_degrees(sig, idx);
            if !CELEBRATION_VERSE[idx] {
                assert_eq!(
                    row, CELEBRATION_PHRASE[idx],
                    "chorus bar {idx} drifted for sig {sig:#x}"
                );
            }
            for slot in 0..row.len() {
                assert_eq!(
                    row[slot] == REST,
                    CELEBRATION_PHRASE[idx][slot] == REST,
                    "bar {idx} slot {slot}: the rhythm is authored, never per-key"
                );
            }
            let sounding: Vec<i32> = row.iter().copied().filter(|&d| d != REST).collect();
            for &d in &sounding {
                assert!(
                    (0..=9).contains(&d),
                    "bar {idx}: degree {d} off the singable range (sig {sig:#x})"
                );
            }
            if CELEBRATION_VERSE[idx] {
                for w in sounding.windows(2) {
                    assert_ne!(w[0], w[1], "bar {idx}: a verse drone (sig {sig:#x})");
                }
            }
        }
    }
}

#[test]
fn mode_rotation_alone_changes_the_chorus_color() {
    let render = |sig: u32| {
        let mut s = TrailSynth::new(48_000.0, 42);
        s.push(riff_sig(GlowStyle::RainbowKitty, 6, sig));
        let mut acc = 0u64;
        let mut buf = [0.0f32; 960];
        for _ in 0..160 {
            s.render(&mut buf);
            for &x in &buf {
                acc = acc.rotate_left(7).wrapping_add(u64::from(x.to_bits()));
            }
        }
        acc
    };
    assert_eq!(celebration_root(12), celebration_root(22));
    assert_ne!(celebration_mode(12), celebration_mode(22));
    assert_ne!(render(12), render(22), "the mode axis must be audible");
}

#[test]
fn neutral_signature_is_the_reference_voicing() {
    use crate::kitty_sing::NEUTRAL_SIGNATURE;
    assert_eq!(celebration_root(NEUTRAL_SIGNATURE), 0);
    assert_eq!(celebration_mode(NEUTRAL_SIGNATURE), 0);
}

#[test]
fn tone_tables_are_mutually_consonant_and_exclude_the_bonk_clash() {
    for tone in Tone::ALL {
        let (table, transpose) = tone_tables(tone);
        assert!(
            (1.0..2.0).contains(&transpose),
            "{tone:?}: transpose {transpose} out of the octave"
        );
        for (i, &a) in table.iter().enumerate() {
            assert!(
                (1.0..2.0).contains(&a),
                "{tone:?}: degree {i} out of octave"
            );
            for &b in &table[i..] {
                let r = b / a;
                assert_outside_bonk_zones(r, &format!("{tone:?} {a}:{b}"));
                assert_outside_bonk_zones(2.0 / reduce(r), &format!("{tone:?} inv {a}:{b}"));
            }
        }
    }
    let m2 = reduce(BONK_MINOR_SECOND);
    assert!(m2 > 1.0 + 1e-4 && m2 < 1.09, "minor-second zone drifted");
    let tt = reduce(BONK_TRITONE);
    assert!(tt > 1.395 && tt < 1.43, "tritone zone drifted");
}

#[test]
fn every_tone_recolors_and_still_decays_to_silence() {
    let render_hash = |tone: Tone| {
        let mut s = TrailSynth::new(48_000.0, 0xBEEF);
        let mut acc = 0u64;
        let mut buf = [0.0f32; 1024];
        for _ in 0..8 {
            s.push(toned(GlowStyle::Lumen, SoundKind::Typed, tone));
            for _ in 0..2 {
                s.render(&mut buf);
                for &x in &buf {
                    assert!(x.is_finite(), "{tone:?} produced non-finite");
                    assert!(x.abs() <= 0.98, "{tone:?} clipped");
                    acc = acc.rotate_left(9).wrapping_add(u64::from(x.to_bits()));
                }
            }
        }
        for _ in 0..(6 * 48_000 * 2 / 1024) {
            s.render(&mut buf);
        }
        assert!(s.is_quiet(), "{tone:?} never decayed to silence");
        acc
    };
    let neutral = render_hash(Tone::Technical);
    for tone in [Tone::Calm, Tone::Excited, Tone::Frustrated, Tone::Playful] {
        assert_ne!(
            render_hash(tone),
            neutral,
            "{tone:?} rendered byte-identically to Technical"
        );
    }
}

#[test]
fn the_bar_is_a_regular_beat_of_consonant_offsets() {
    let accents: Vec<usize> = SONG_PULSE
        .iter()
        .enumerate()
        .filter(|(_, s)| **s == SONG_ACCENT)
        .map(|(i, _)| i)
        .collect();
    assert!(
        accents.len() >= 2 && accents[0] == 0,
        "the bar opens on its downbeat"
    );
    let step = accents[1] - accents[0];
    assert!(
        accents.windows(2).all(|w| w[1] - w[0] == step) && SONG_PULSE.len().is_multiple_of(step),
        "the beat must be EVEN and close the bar so it loops in phase: {accents:?}"
    );
    for &slot in SONG_PULSE.iter().filter(|&&s| s != SONG_ACCENT) {
        assert!(
            (-5..=4).contains(&slot),
            "a ghost {slot} degrees out leaves the melody's register (an octave \
             down is the floor: further and it lands in the space downbeat's band)"
        );
        assert_ne!(
            slot, 0,
            "a ghost on the melody's own degree is a second voice at the SAME \
             frequency as the accent still ringing above it — a comb filter"
        );
    }
}

#[test]
fn punctuation_and_the_note_after_a_pause_always_sing() {
    for kind in [SoundKind::Jump, SoundKind::Kill, SoundKind::Navigation] {
        let mut s = TrailSynth::new(48_000.0, 0x9011_1CE5);
        let mut buf = [0.0f32; 9600];
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
        let sung = s.song_notes;
        s.push(ev(GlowStyle::Lumen, kind));
        assert!(
            s.song_notes > sung && s.song_accent,
            "{kind:?} must land as a melody note, not a ghost"
        );
    }
    let mut s = TrailSynth::new(48_000.0, 0x9011_1CE6);
    let mut buf = [0.0f32; 9600];
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    s.render(&mut buf);
    assert_ne!(s.song_pulse, 0, "fixture: mid-bar");
    for _ in 0..8 {
        s.render(&mut buf);
    }
    let sung = s.song_notes;
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert!(
        s.song_notes > sung && s.song_accent,
        "the first note after a think must be the tune"
    );
    assert_eq!(s.song_pulse, 1, "…from the top of the bar");
}

#[test]
fn the_theme_is_played_in_order_and_loops() {
    let mut s = TrailSynth::new(48_000.0, 0x0DDB_A11E);
    s.tone = Tone::Technical;
    let mut sung = Vec::new();
    while sung.len() < SONG_THEME.len() * 2 + 1 {
        let before = s.song_notes;
        s.since_voice = 1.0;
        s.since_event = 0.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        if s.song_notes != before {
            sung.push(s.walk as i8);
        }
    }
    assert_eq!(
        &sung[..SONG_THEME.len()],
        &SONG_THEME[..],
        "the first pass must be the theme, note for note"
    );
    assert_eq!(
        &sung[SONG_THEME.len()..SONG_THEME.len() * 2],
        &SONG_THEME[..],
        "…and the second pass is the same tune again, in phase"
    );
    assert_eq!(
        sung[SONG_THEME.len() * 2],
        SONG_THEME[0],
        "the loop falls back onto the hook"
    );
}

#[test]
fn a_rest_cadences_the_phrase_and_opens_the_next() {
    let mut s = TrailSynth::new(48_000.0, 0xCADE_5EED);
    let mut buf = [0.0f32; 256];
    let accent = |s: &mut TrailSynth, kind: SoundKind| {
        let before = s.song_notes;
        while s.song_notes == before {
            s.since_voice = 1.0;
            s.since_event = 0.0;
            s.push(ev(GlowStyle::Lumen, kind));
        }
    };
    for _ in 0..3 {
        accent(&mut s, SoundKind::Typed);
    }
    assert_eq!(s.walk, 4, "fixture: three notes into the hook");
    assert_eq!(s.phrase_idx, 0, "…still inside phrase A");
    s.push(ev(GlowStyle::Lumen, SoundKind::Jump));
    assert_eq!(
        i32::from(SONG_THEME[usize::from(SONG_FORM[1]) - 1]),
        s.walk,
        "a rest cadences onto the phrase's own last note, got {}",
        s.walk
    );
    assert_eq!(s.phrase_idx, 1, "…and opens the ANSWER, not the hook again");
    s.render(&mut buf);
    accent(&mut s, SoundKind::Typed);
    assert_eq!(
        s.walk,
        i32::from(SONG_THEME[usize::from(SONG_FORM[1])]),
        "the phrase after a rest continues the form"
    );
    let mut p = TrailSynth::new(48_000.0, 0x9A11_5EED);
    for _ in 0..3 {
        accent(&mut p, SoundKind::Typed);
    }
    p.since_event = PHRASE_PAUSE_S + 0.1;
    p.since_voice = 1.0;
    p.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert_eq!(
        p.walk,
        i32::from(SONG_THEME[usize::from(SONG_FORM[1]) - 1]),
        "a comma-length gap cadences exactly as an Enter does"
    );
    assert_eq!(p.phrase_idx, 1, "…and advances the form too");
}

#[test]
fn a_rest_before_a_phrase_starts_cannot_skip_it() {
    let mut s = TrailSynth::new(48_000.0, 0x5C1D_0001);
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert_eq!(
        s.walk,
        i32::from(SONG_THEME[0]),
        "a session opens on the theme's first note"
    );
    assert_eq!(s.phrase_idx, 0, "…inside the first phrase, not past it");
    s.since_event = PHRASE_PAUSE_S + 0.1;
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert_eq!(s.phrase_idx, 1, "one rest, one phrase");
    s.since_event = PHRASE_PAUSE_S + 0.1;
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert_eq!(
        s.phrase_idx, 1,
        "a rest with nothing played cannot burn a phrase"
    );
    assert_eq!(
        s.walk,
        i32::from(SONG_THEME[usize::from(SONG_FORM[1])]),
        "…it plays the phrase's first note instead"
    );
}

#[test]
fn the_theme_never_folds_in_any_tone() {
    for tone in [
        Tone::Technical,
        Tone::Calm,
        Tone::Excited,
        Tone::Frustrated,
        Tone::Playful,
    ] {
        let (lo, hi) = tone_register(tone);
        let lean = melody_lean(tone);
        for (i, &deg) in SONG_THEME.iter().enumerate() {
            let want = i32::from(deg) + lean;
            assert_eq!(
                fold_register(want, lo, hi),
                want,
                "{tone:?}: theme note {i} (degree {deg} + lean {lean}) folds out of \
                 {lo}..{hi} — the register is rewriting the melody"
            );
        }
    }
}

#[test]
fn the_song_form_partitions_the_theme() {
    assert_eq!(usize::from(SONG_FORM[0]), 0, "the form opens at note 0");
    assert_eq!(
        usize::from(SONG_FORM[SONG_FORM.len() - 1]),
        SONG_THEME.len(),
        "the form's sentinel is the theme's length"
    );
    for w in SONG_FORM.windows(2) {
        assert!(
            w[1] > w[0],
            "a phrase must contain at least one note: {w:?}"
        );
    }
}

#[test]
fn the_bass_walk_never_returns_inside_four_words() {
    const MIN_RETURN: usize = 4;
    assert_eq!(
        SONG_BASS
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        5,
        "the walk must use every root of the lattice"
    );
    let n = SONG_BASS.len();
    let mut s = TrailSynth::new(48_000.0, 0x5A_CE_11);
    for tone in Tone::ALL {
        s.tone = tone;
        for style in STYLES {
            let anchor = palette_for(SoundVoice::Style, style).anchor_hz();
            for key in -5i8..=5 {
                s.song_key = key;
                let hz: Vec<f32> = (0..n as u8).map(|i| s.space_root_hz(anchor, i)).collect();
                for i in 0..n {
                    for j in i + 1..n {
                        let d = (j - i).min(n - (j - i));
                        let cents = 1200.0 * (hz[j] / hz[i]).log2().abs();
                        assert!(
                            cents > 25.0 || d >= MIN_RETURN,
                            "{style:?}/{tone:?}/key {key}: the walk returns to \
                             {:.1} Hz after only {d} word(s) (steps {i} and {j})",
                            hz[i]
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_register_fold_is_total_and_never_flattens() {
    for (lo, hi) in [(0, 7), (0, 6), (0, 8), (3, 3)] {
        for v in -40..40 {
            let f = fold_register(v, lo, hi);
            assert!(f >= lo && f <= hi, "fold({v}) = {f} escaped {lo}..{hi}");
        }
    }
    assert_ne!(fold_register(8, 0, 7), fold_register(9, 0, 7));
    assert_ne!(fold_register(-1, 0, 7), fold_register(-2, 0, 7));
}

fn family_voices(style: GlowStyle, kind: SoundKind) -> (TrailSynth, Vec<(f32, f32)>) {
    family_voices_in(SoundVoice::Style, style, kind, false)
}

fn family_voices_in(
    voice: SoundVoice,
    style: GlowStyle,
    kind: SoundKind,
    shifted: bool,
) -> (TrailSynth, Vec<(f32, f32)>) {
    let mut s = TrailSynth::new(48_000.0, 0xFA_1117);
    let mut buf = [0.0f32; 1024];
    for _ in 0..3 {
        s.push(SoundEvent {
            voice,
            ..ev(style, SoundKind::Typed)
        });
        for _ in 0..25 {
            s.render(&mut buf);
        }
    }
    assert_eq!(
        SONG_PULSE[usize::from(s.song_pulse)],
        SONG_ACCENT,
        "fixture: the probe must speak on an accent"
    );
    let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
    s.since_voice = 1.0;
    s.push(SoundEvent {
        voice,
        shifted,
        ..ev(style, kind)
    });
    let mut out: Vec<(f32, f32)> = s
        .voices
        .iter()
        .enumerate()
        .filter(|(i, v)| v.on && !before[*i])
        .filter_map(|(_, v)| {
            let p = v.p.iter().max_by(|a, b| a.lvl.total_cmp(&b.lvl))?;
            (p.lvl > 0.0).then_some((p.f1, p.f0))
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    (s, out)
}

#[test]
fn a_deletion_does_not_advance_the_song() {
    let mut a = TrailSynth::new(48_000.0, 0xD0_1E_7E);
    let mut b = TrailSynth::new(48_000.0, 0xD0_1E_7E);
    let mut buf = [0.0f32; 1024];
    for _ in 0..5 {
        a.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        b.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
        for _ in 0..9 {
            a.render(&mut buf);
            b.render(&mut buf);
        }
    }
    for _ in 0..12 {
        b.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace));
        for _ in 0..9 {
            a.render(&mut buf);
            b.render(&mut buf);
        }
    }
    for _ in 0..9 {
        a.render(&mut buf);
    }
    assert_eq!(
        a.walk,
        b.walk,
        "twelve corrections moved the song by {} degrees",
        b.walk - a.walk
    );
    assert_eq!(
        (a.theme_pos, a.phrase_idx),
        (b.theme_pos, b.phrase_idx),
        "…and they must not have moved the theme's playhead either"
    );
}

#[test]
fn the_space_is_a_lawful_walking_bass() {
    let (s, spaces) = family_voices(GlowStyle::Lumen, SoundKind::Space);
    let (land, enter) = spaces[0];
    assert!(
        (enter - land).abs() < 0.5,
        "a bass root arrives, it does not lean: {enter} -> {land}"
    );
    let anchor = palette_for(SoundVoice::Style, GlowStyle::Lumen).anchor_hz();
    let expect = s.space_root_hz(anchor, 0);
    assert!(
        (land - expect).abs() < 0.5,
        "the first word's root is the palette's octave-folded tonic: \
         got {land}, expected {expect}"
    );
    assert!(
        (SPACE_BASS_LO_HZ * 2.0..SPACE_BASS_LO_HZ * 4.0).contains(&land),
        "…inside the ONE audible bass register ({land} Hz)"
    );
    let new_root = |w: &mut TrailSynth| {
        let before: [bool; MAX_VOICES] = core::array::from_fn(|i| w.voices[i].on);
        w.since_voice = 1.0;
        w.push(ev(GlowStyle::Lumen, SoundKind::Space));
        w.voices
            .iter()
            .enumerate()
            .filter(|(i, v)| v.on && !before[*i] && v.bass)
            .filter_map(|(_, v)| (v.p[0].lvl > 0.0).then_some(v.p[0].f1))
            .next()
            .expect("the bass speaks")
    };
    let mut w = TrailSynth::new(48_000.0, 0xBA_55_11);
    let mut buf = [0.0f32; 2048];
    for i in 0..SONG_BASS.len() {
        let f = new_root(&mut w);
        let want = s.space_root_hz(anchor, i as u8);
        assert!(
            (f - want).abs() < 0.5,
            "word {i} played {f} Hz, not the walk's root {want} Hz"
        );
        w.render(&mut buf);
        w.since_voice = 1.0;
        w.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        w.render(&mut buf);
    }
    let mut t = TrailSynth::new(48_000.0, 0xFA_1117);
    let mut moved = false;
    for _ in 0..40 {
        t.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        for _ in 0..6 {
            t.render(&mut buf);
        }
        if t.walk != s.walk {
            moved = true;
            break;
        }
    }
    assert!(moved, "fixture: the melody must reach a different degree");
    assert_eq!(t.bass_step, 0, "fixture: no word boundary has passed");
    let far = new_root(&mut t);
    assert!(
        (far - land).abs() < 0.5,
        "the bass must not follow the melody: {far} Hz at degree {} \
         against {land} Hz at degree {}",
        t.walk,
        s.walk
    );
    let (tp, _) = family_voices(GlowStyle::Lumen, SoundKind::Typed);
    assert_eq!(
        s.theme_pos,
        tp.theme_pos.saturating_sub(1),
        "a space must not step the song (the Typed probe stepped once more)"
    );
}

#[test]
fn a_word_boundary_re_bars_the_pulse() {
    let mut s = TrailSynth::new(48_000.0, 0xBA_12_11);
    let mut buf = [0.0f32; 2048];
    for _ in 0..5 {
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
    }
    assert_ne!(s.song_pulse, 0, "fixture: the bar is mid-figure");
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Space));
    assert_eq!(s.song_pulse, 0, "a word boundary opens a fresh bar");
    let sung = s.song_notes;
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert_eq!(
        s.song_notes,
        sung + 1,
        "the first letter of a word must be an accent"
    );
    assert!(s.song_accent, "…and must be designed as one");
    s.render(&mut buf);
    for _ in 0..3 {
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
    }
    let mid = s.song_pulse;
    assert_ne!(mid, 0, "fixture: the bar is mid-figure again");
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Space));
    assert_eq!(s.song_pulse, 0, "the run's HEAD re-bars");
    s.render(&mut buf);
    for _ in 0..3 {
        s.since_voice = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        s.render(&mut buf);
    }
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    s.render(&mut buf);
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Space));
    let at_head = s.song_pulse;
    s.render(&mut buf);
    s.since_voice = 1.0;
    s.push(ev(GlowStyle::Lumen, SoundKind::Space));
    assert_eq!(
        s.song_pulse, at_head,
        "the tail of a whitespace run must not re-bar again"
    );
}

#[test]
fn a_whitespace_run_lands_one_downbeat() {
    let mut s = TrailSynth::new(48_000.0, 0x5A_CE_00);
    let mut buf = [0.0f32; 8000];
    let bass = |s: &TrailSynth| s.voices.iter().filter(|v| v.on && v.bass).count();
    let space = |s: &mut TrailSynth| {
        let before = s.live_voices();
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        s.live_voices() - before
    };
    let step_before = s.bass_step;
    assert_eq!(space(&mut s), 2, "the head speaks: a root and its air");
    assert_eq!(bass(&s), 1, "…exactly one of them a bass root");
    let head_root = s
        .voices
        .iter()
        .find(|v| v.on && v.bass)
        .map(|v| v.p[0].f1)
        .expect("the head's root");
    let mut last_step = head_root;
    let mut held = false;
    for i in 0..12 {
        s.render(&mut buf);
        let before: [bool; MAX_VOICES] = core::array::from_fn(|k| s.voices[k].on);
        assert_eq!(
            space(&mut s),
            2,
            "space {i} of the run must SPEAK twice — its breath and its indent \
             step (a key that makes no sound reads as a dropped keystroke)"
        );
        assert!(
            bass(&s) <= 1,
            "a run of spaces must never stack bass roots (found {})",
            bass(&s)
        );
        let step = s
            .voices
            .iter()
            .enumerate()
            .filter(|(k, v)| v.on && !before[*k] && !v.bass && v.p[0].lvl > 0.0)
            .map(|(_, v)| v.p[0].f1)
            .next()
            .expect("…and its INDENT STEP: a pitched bell");
        if held {
            assert!(
                (step - last_step).abs() < 0.01,
                "once the figure has reached the roof it HOLDS its top note \
                 ({last_step} -> {step} Hz on step {i})"
            );
        } else if (step - last_step).abs() < 0.01 && i > 0 {
            held = true;
            assert!(
                step * 1.2001 >= SPACE_STEP_HI_HZ,
                "the figure may only hold when the next lattice degree would cross \
                 the roof ({step} Hz held under {SPACE_STEP_HI_HZ})"
            );
        } else {
            assert!(
                step > last_step * 1.05,
                "step {i} must climb over the note before it ({last_step} -> {step} Hz)"
            );
        }
        assert!(
            (SPACE_BASS_LO_HZ * 2.0..SPACE_STEP_HI_HZ).contains(&step),
            "step {i} must sit in the space register or the octave over it ({step} Hz)"
        );
        last_step = step;
    }
    assert!(
        held,
        "twelve tail spaces from a root under 440 Hz must reach the 880 Hz roof and hold"
    );
    assert_eq!(
        s.bass_step,
        (step_before + 1) % SONG_BASS.len() as u8,
        "a whitespace run must spend exactly one step of the walk"
    );
    s.render(&mut buf);
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    s.render(&mut buf);
    s.push(ev(GlowStyle::Lumen, SoundKind::Space));
    assert!(
        s.voices.iter().any(|v| v.on && v.bass && v.damp <= 0.0),
        "a word boundary after a letter is a fresh root"
    );
    assert_eq!(
        s.bass_step,
        (step_before + 2) % SONG_BASS.len() as u8,
        "…and the harmony moves on"
    );
}

#[test]
fn a_held_spacebar_cannot_machine_gun_the_indent_steps() {
    let mut s = TrailSynth::new(48_000.0, 0x5A_CE_33);
    let mut buf = [0.0f32; 3_168];
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    s.render(&mut buf);
    let (mut steps, mut breaths) = (0usize, 0usize);
    for _ in 0..30 {
        let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        for (i, v) in s.voices.iter().enumerate() {
            if v.on && !before[i] && !v.bass {
                if v.p[0].lvl > 0.0 {
                    steps += 1;
                } else {
                    breaths += 1;
                }
            }
        }
        s.render(&mut buf);
    }
    assert!(
        (4..=13).contains(&steps),
        "a held spacebar minted {steps} steps/s — the gate admits one per 75+ ms"
    );
    assert!(breaths >= steps, "every admitted tail space breathes");
}

#[test]
fn two_word_boundaries_never_stack_one_bass_note() {
    let mut s = TrailSynth::new(48_000.0, 0xBA_55_01);
    let mut buf = [0.0f32; 4096];
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Space));
    s.render(&mut buf);
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Typed));
    s.render(&mut buf);
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Space));
    let live: Vec<&Voice> = s.voices.iter().filter(|v| v.on && v.bass).collect();
    let undamped = live.iter().filter(|v| v.damp <= 0.0).count();
    assert_eq!(
        undamped,
        1,
        "exactly one bass root may be sounding at a time (of {} live)",
        live.len()
    );
    assert!(
        live.iter().any(|v| v.damp > 0.0 && v.damp0 == SPACE_DAMP_S),
        "the previous root is released, not cut: a hard stop clicks"
    );
}

#[test]
fn shift_is_the_ting_and_never_claims_the_beat() {
    let live = |s: &TrailSynth| s.voices.iter().filter(|v| v.on).count();
    let mut s = TrailSynth::new(48_000.0, 0xC0FF_EE01);
    s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
    let before = live(&s);
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    assert!(
        live(&s) > before,
        "the capital right behind a shift must not be thinned by its own grace note"
    );
    let mut s = TrailSynth::new(48_000.0, 0xC0FF_EE02);
    let mut buf = [0.0f32; 2_880];
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    s.render(&mut buf);
    let before = live(&s);
    s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
    assert_eq!(
        live(&s),
        before + 1,
        "a Shift 30 ms after the letter before its capital must ting — the \
         keystroke gap is not the ting's gate"
    );
    s.render(&mut buf);
    let before = live(&s);
    s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
    assert_eq!(
        live(&s),
        before,
        "a second Shift 30 ms after an admitted one is thinned (SHIFT_MIN_GAP)"
    );
    s.render(&mut buf);
    s.render(&mut buf);
    let before = live(&s);
    s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
    assert_eq!(
        live(&s),
        before + 1,
        "a Shift 90 ms after the last admitted one tings"
    );
}

#[test]
fn a_thinned_space_head_re_opens_the_run() {
    let mut buf = [0.0f32; 2_880];
    let fresh_after_space = |s: &mut TrailSynth| -> Vec<Voice> {
        let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
        s.push(ev(GlowStyle::Lumen, SoundKind::Space));
        s.voices
            .iter()
            .enumerate()
            .filter(|(i, v)| v.on && !before[*i])
            .map(|(_, v)| *v)
            .collect()
    };
    let mut c = TrailSynth::new(48_000.0, 0x5A_CE_7E);
    c.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    for _ in 0..4 {
        c.render(&mut buf);
    }
    let head = fresh_after_space(&mut c);
    assert_eq!(
        head.iter().filter(|v| v.bass).count(),
        1,
        "fixture: a head is one root"
    );

    let mut s = TrailSynth::new(48_000.0, 0x5A_CE_7E);
    s.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    s.render(&mut buf);
    let thinned = fresh_after_space(&mut s);
    assert!(
        thinned.is_empty(),
        "fixture: the first space lands inside MIN_GAP and is thinned"
    );
    assert_eq!(s.space_run_root_hz, 0.0, "fixture: no root has sounded yet");
    for _ in 0..3 {
        s.render(&mut buf);
    }
    let fresh = fresh_after_space(&mut s);
    assert_eq!(
        fresh.iter().filter(|v| v.bass).count(),
        1,
        "the space after a thinned head is the head: one bass root"
    );
    assert_eq!(
        fresh.len(),
        head.len(),
        "…and exactly a head's voices — no indent step off a root nobody heard \
         ({} voices against the control's {})",
        fresh.len(),
        head.len()
    );
    assert!(
        fresh.iter().all(|v| v.dur != SPACE_STEP_DUR_S || v.bass),
        "no voice on the indent step's envelope"
    );
    assert!(
        s.space_run_root_hz > 0.0,
        "the run is rooted on the note that sounded"
    );
    assert_eq!(
        s.space_run_len, 0,
        "the run's length restarts at its real head"
    );
}

#[test]
fn the_ting_rotates_and_never_drones() {
    let mut s = TrailSynth::new(48_000.0, 0x5111_F700);
    let mut buf = [0.0f32; 2048];
    let anchor = palette_for(SoundVoice::Style, GlowStyle::Lumen).anchor_hz();
    let walk0 = s.walk;
    let mut sung: Vec<f32> = Vec::new();
    for _ in 0..3 * SHIFT_ROTATION.len() {
        let before: [bool; MAX_VOICES] = core::array::from_fn(|i| s.voices[i].on);
        s.since_voice = 1.0;
        s.since_shift = 1.0;
        s.push(ev(GlowStyle::Lumen, SoundKind::Shift));
        let (f, enter) = s
            .voices
            .iter()
            .enumerate()
            .filter(|(i, v)| v.on && !before[*i] && v.p[0].lvl > 0.0)
            .map(|(_, v)| (v.p[0].f1, v.p[0].f0))
            .next()
            .expect("the lift speaks");
        assert!(
            (enter - f).abs() < 0.5,
            "a ting is struck, it does not lean: {enter} -> {f}"
        );
        sung.push(f);
        s.render(&mut buf);
    }
    assert_eq!(
        s.walk, walk0,
        "fixture: a bare shift composes nothing — the melody must not have moved"
    );
    for i in 0..sung.len() {
        for j in i + 1..sung.len() {
            let d = j - i;
            assert!(
                1200.0 * (sung[j] / sung[i]).log2().abs() > 25.0 || d >= SHIFT_ROTATION.len(),
                "lift {j} repeated lift {i}'s note after {d} press(es): {sung:?}"
            );
        }
    }
    for (i, &f) in sung.iter().enumerate() {
        let want = ting_octave(s.melody_hz(
            anchor,
            walk0 + GESTURE_CHAR_STEP + SHIFT_ROTATION[i % SHIFT_ROTATION.len()],
        ));
        assert!(
            (f - want).abs() < 0.5,
            "ting {i} played {f} Hz, not the rotation's degree at {want} Hz"
        );
        assert!(
            (SHIFT_TING_LO_HZ..SHIFT_TING_LO_HZ * 2.0).contains(&f),
            "ting {i} left its register ({f} Hz)"
        );
    }
    let mut t = TrailSynth::new(48_000.0, 0x5111_F701);
    t.since_voice = 1.0;
    t.push(ev(GlowStyle::Lumen, SoundKind::Shift));
    let after_one = t.shift_step;
    assert_eq!(after_one, 1, "fixture: the first lift sounded");
    t.push(ev(GlowStyle::Lumen, SoundKind::Typed));
    let before: [bool; MAX_VOICES] = core::array::from_fn(|i| t.voices[i].on);
    t.push(ev(GlowStyle::Lumen, SoundKind::Shift));
    assert!(
        !t.voices.iter().enumerate().any(|(i, v)| v.on && !before[i]),
        "fixture: the lift behind a keystroke must be thinned"
    );
    assert_eq!(
        t.shift_step, after_one,
        "a thinned lift must not step the rotation"
    );
}

#[test]
fn a_shifted_glyph_lands_an_octave_over_its_plain_self() {
    for style in STYLES {
        let (plain_s, plain) = family_voices_in(SoundVoice::Style, style, SoundKind::Typed, false);
        let (lifted_s, lifted) = family_voices_in(SoundVoice::Style, style, SoundKind::Typed, true);
        let plain_hz: Vec<f32> = plain.iter().map(|&(f1, _)| f1).collect();
        assert!(
            !plain_hz.is_empty() && plain_hz.len() == lifted.len(),
            "{style:?}: the keystroke speaks, and the lift adds or drops no voice"
        );
        let near = |t: f32| {
            plain_hz
                .iter()
                .any(|&p| (t - p).abs() < 0.5f32.max(p * 0.002))
        };
        let mut octaves = 0;
        for &(f, _) in &lifted {
            if near(f / 2.0) {
                octaves += 1;
            } else {
                assert!(
                    near(f),
                    "{style:?}: a shifted voice at {f} Hz is neither its plain \
                     self nor an octave over it ({plain_hz:?})"
                );
            }
        }
        assert!(
            octaves >= 1,
            "{style:?}: no voice took the capital's octave ({plain_hz:?} -> {lifted:?})"
        );
        assert_eq!(
            (lifted_s.walk, lifted_s.theme_pos, lifted_s.phrase_idx),
            (plain_s.walk, plain_s.theme_pos, plain_s.phrase_idx),
            "{style:?}: shiftedness must not move the tune"
        );
    }
    let root = |shifted: bool| {
        let mut s = TrailSynth::new(48_000.0, 0x5A_FE_01);
        s.push(SoundEvent {
            shifted,
            ..ev(GlowStyle::Lumen, SoundKind::Space)
        });
        s.voices
            .iter()
            .filter(|v| v.on && v.bass)
            .map(|v| v.p[0].f1)
            .next()
    };
    assert_eq!(
        root(false),
        root(true),
        "a shifted Space is still a word boundary"
    );
    for &voice in SoundVoice::ALL {
        let plain = isolated_peak(voice, SoundKind::Typed, false);
        let cap = isolated_peak(voice, SoundKind::Typed, true);
        let db = 20.0 * (cap / plain).log10();
        assert!(
            db >= 1.5,
            "{voice:?}: a capital must be LOUDER than its plain self \
             (plain {plain}, shifted {cap}: {db:+.2} dB)"
        );
    }
}

#[test]
fn the_ting_decays_out_instead_of_being_cut() {
    let life_ms = SHIFT_TING_DUR_S * 1000.0;
    let ramp_ms = life_ms - RELEASE_RAMP_S * 1000.0;
    for (voice, style) in [
        (SoundVoice::Style, GlowStyle::Lumen),
        (SoundVoice::Style, GlowStyle::Comet),
        (SoundVoice::Felt, GlowStyle::Lumen),
    ] {
        let mut s = TrailSynth::new(48_000.0, 0x504F_4F46);
        s.push_meta(
            SoundEvent {
                hue: 0.0,
                bed: false,
                ..voiced(voice, style, SoundKind::Shift)
            },
            EventMeta {
                at_ms: 1,
                ..EventMeta::default()
            },
        );
        let mut o = vec![0.0f32; 24_000 * CHANNELS];
        s.render(&mut o);
        let m: Vec<f32> = o.chunks(CHANNELS).map(|f| 0.5 * (f[0] + f[1])).collect();
        let peak = m.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        let ring = |db: f32| {
            m.iter()
                .rposition(|x| x.abs() > peak * 10f32.powf(db / 20.0))
                .map_or(0.0, |i| i as f32 / 48.0)
        };
        let (to40, to60) = (ring(-40.0), ring(-60.0));
        assert!(
            to40 + 10.0 <= ramp_ms,
            "{voice:?}/{style:?}: the ting must reach −40 dB re its peak by decay, at least \
             10 ms before the ramp starts ({to40:.1} ms vs the ramp at {ramp_ms:.1})"
        );
        assert!(
            to60 >= ramp_ms && to60 <= life_ms,
            "{voice:?}/{style:?}: the −60 dB point should be the ramp's ({to60:.1} ms, ramp \
             {ramp_ms:.1}..{life_ms:.1}) — otherwise the −40 dB reading was a cut"
        );
    }
}

#[test]
fn the_deletion_is_a_poof_and_only_a_poof() {
    for kind in [SoundKind::Backspace, SoundKind::KillWord] {
        for &voice in SoundVoice::ALL {
            for style in LOOKS {
                let ctx = format!("{kind:?}/{voice:?}/{style:?}");
                let mut s = TrailSynth::new(48_000.0, 0xFE17_0FF5);
                let walk = s.walk;
                let mut e = voiced(voice, style, kind);
                e.bed = false;
                s.push(e);
                assert_eq!(s.walk, walk, "{ctx}: a deletion does not compose");
                let live: Vec<&Voice> = s.voices.iter().filter(|v| v.on).collect();
                assert_eq!(
                    live.len(),
                    2,
                    "{ctx}: a deletion is the body and the cap — no palette voice \
                     may speak under them"
                );
                assert!(
                    live.iter().any(|v| v.delay == 0.0),
                    "{ctx}: the body speaks in the first buffer"
                );
                for v in &live {
                    assert!(
                        v.p.iter().all(|p| p.lvl <= 0.0) && v.n_lvl > 0.0,
                        "{ctx}: the poof is noise and carries NO tonal partial"
                    );
                    if v.delay == 0.0 {
                        assert!(
                            v.n_glide <= 0.0 && v.n_f0 == v.n_f1,
                            "{ctx}: the BODY's band is static"
                        );
                    }
                    assert!(v.n_q < 1.0, "{ctx}: broad Q — a resonant burst whistles");
                    assert!(v.attack >= 0.0015, "{ctx}: a sub-millisecond attack clicks");
                }
            }
        }
    }
}

#[test]
fn a_word_motion_is_a_character_motion_at_scale() {
    let anchor = palette_for(SoundVoice::Style, GlowStyle::Lumen).anchor_hz();
    for dir in [1i8, -1] {
        let (gs, glide) = family_voices(GlowStyle::Lumen, SoundKind::Glide { dir });
        let (ss, sweep) = family_voices(GlowStyle::Lumen, SoundKind::Sweep { dir });
        assert_eq!(glide.len(), 1, "a character motion is one note");
        assert_eq!(
            sweep.len(),
            CURSOR_SWEEP_RUN,
            "a word motion is one note per character crossed"
        );
        assert_eq!(gs.walk, ss.walk, "fixture: one degree for both probes");
        let step = i32::from(dir) * GESTURE_CHAR_STEP;
        assert!(
            (glide[0].0 - gs.melody_hz(anchor, gs.walk + step)).abs() < 0.5,
            "dir {dir}: a character motion must land one step out"
        );
        let mut expect: Vec<f32> = (0..CURSOR_SWEEP_RUN)
            .map(|i| ss.melody_hz(anchor, ss.walk + step * i as i32))
            .collect();
        expect.sort_by(f32::total_cmp);
        for (got, want) in sweep.iter().map(|p| p.0).zip(expect) {
            assert!(
                (got - want).abs() < 0.5,
                "dir {dir}: the run must be the character step repeated: {got} vs {want}"
            );
        }
        let span = sweep[sweep.len() - 1].0 / sweep[0].0;
        let one = gs.melody_hz(anchor, gs.walk + GESTURE_CHAR_STEP) / gs.melody_hz(anchor, gs.walk);
        assert!(
            span > one * 1.05,
            "dir {dir}: a word motion must travel further than a character one \
             ({span} vs {one})"
        );
    }
}

#[test]
fn the_clouds_puff_survives_the_bell_it_follows() {
    let mut s = TrailSynth::new(48_000.0, 1);
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace));
    let after_bell = s.live_voices();
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
    assert!(
        s.live_voices() > after_bell,
        "the puff must speak behind the bell it accompanies"
    );

    let mut s = TrailSynth::new(48_000.0, 1);
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
    let after_puff = s.live_voices();
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Backspace));
    assert!(
        s.live_voices() > after_puff,
        "the puff is an accompaniment: it must not thin the key that follows"
    );

    let mut s = TrailSynth::new(48_000.0, 0x9A11);
    let walk = s.walk;
    s.push(ev(GlowStyle::RainbowKitty, SoundKind::Poof));
    assert_eq!(s.walk, walk, "a puff of air does not compose the melody");
}

#[test]
fn a_held_delete_run_shimmers_once_on_release() {
    let pending = |s: &TrailSynth| {
        s.voices
            .iter()
            .filter(|v| v.on && v.shimmer && v.t < 0.0 && v.damp <= 0.0)
            .count()
    };
    let mut s = TrailSynth::new(48_000.0, 0x51D3_0001);
    let mut buf = vec![0.0f32; 4_800 * CHANNELS];
    for i in 0..8u32 {
        let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
        e.bed = false;
        s.push(e);
        let want = usize::from(i + 1 >= HELD_ERASE_RUN_MIN);
        assert_eq!(
            pending(&s),
            want,
            "after {} admitted deletions the run holds exactly {want} \
             pending shimmer(s)",
            i + 1
        );
        s.render(&mut buf);
    }
    assert_eq!(pending(&s), 1, "the last booking survives the release");
    s.render(&mut buf);
    let sounding: Vec<&Voice> = s
        .voices
        .iter()
        .filter(|v| v.on && v.shimmer && v.t >= 0.0)
        .collect();
    assert_eq!(sounding.len(), 1, "exactly one shimmer reaches the ear");
    assert!(
        sounding[0].damp <= 0.0,
        "…and nothing damped the one that speaks"
    );

    let mut s = TrailSynth::new(48_000.0, 0x51D3_0002);
    for _ in 0..(HELD_ERASE_RUN_MIN - 1) {
        let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
        e.bed = false;
        s.push(e);
        s.render(&mut buf);
    }
    assert_eq!(pending(&s), 0, "a short run is not a held run");

    let mut s = TrailSynth::new(48_000.0, 0x51D3_0003);
    for _ in 0..HELD_ERASE_RUN_MIN {
        let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
        e.bed = false;
        s.push(e);
        s.render(&mut buf);
    }
    assert_eq!(pending(&s), 1, "fixture: the hold booked its shimmer");
    let mut t = ev(GlowStyle::RainbowKitty, SoundKind::Typed);
    t.bed = false;
    s.push(t);
    assert_eq!(
        pending(&s),
        0,
        "a keystroke inside the pre-delay expires the booking"
    );
    s.render(&mut buf);
    s.render(&mut buf);
    let mut e = ev(GlowStyle::RainbowKitty, SoundKind::Backspace);
    e.bed = false;
    s.push(e);
    assert_eq!(pending(&s), 0, "a lone correction is not a held run");
}

#[test]
fn cursor_motions_never_step_the_song_and_only_the_sweep_bypasses_the_gap() {
    for (kind, bypasses) in [
        (SoundKind::Glide { dir: 1 }, false),
        (SoundKind::Sweep { dir: -1 }, true),
    ] {
        let mut s = TrailSynth::new(48_000.0, 0x6117);
        let walk = s.walk;
        s.push(ev(GlowStyle::Lumen, kind));
        assert!(s.live_voices() > 0, "{kind:?} speaks");
        assert_eq!(s.walk, walk, "{kind:?} must not step the phrase melody");

        let mut a = TrailSynth::new(48_000.0, 1);
        a.push(ev(GlowStyle::Lumen, SoundKind::Typed));
        let before = a.live_voices();
        a.push(ev(GlowStyle::Lumen, kind));
        assert_eq!(
            a.live_voices() > before,
            bypasses,
            "{kind:?} inside the min-gap: only the sweep bypasses thinning"
        );
    }
}

#[test]
fn sin_table_stays_below_one_16bit_step() {
    const ONE_16BIT_STEP: f32 = 1.0 / 32768.0;
    let mut worst = 0.0f32;
    let mut worst_ph = 0.0f32;
    for i in 0..400_000u32 {
        let base = i as f32 / 400_000.0;
        for ph in [base, base - 1.0, base + 1.0, base + 7.5] {
            let exact = ((ph - ph.floor()) * core::f32::consts::TAU).sin();
            let d = (sin01(ph) - exact).abs();
            if d > worst {
                worst = d;
                worst_ph = ph;
            }
        }
    }
    assert!(
        worst < ONE_16BIT_STEP,
        "sin01's table drifted {worst:e} from libm at ph={worst_ph} — \
         {:.3} of a 16-bit step, so it can now move an output sample",
        worst / ONE_16BIT_STEP
    );
}
