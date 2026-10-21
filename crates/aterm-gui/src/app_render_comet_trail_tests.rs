// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::cursor_glow::GlowStyle;
use aterm_effects::cursor_fx::forge_cursor_fill;

/// Each trail-style spelling's `glow_config()` / `trail_config()` (a config contract); a row
/// asserts only the fields its case is about (`None`: not asserted).
///
/// * "comet": the additive crown (bloom + landing ring), the beam along the swept path, and
///   the `TrailCell` ember bed's config (capped by `READABLE_ALPHA_CAP`), with its own `Comet`
///   aurora and glacial default hue. A frame runs no cell bed beside the comet's own streak
///   (`comet_tail_is_its_streak`): the streak is the tail.
/// * "beam": the steady tube (`GlowStyle::Beam`, ice-blue default): radius 0, ring off and no
///   trail body even when crown and ring are configured; an explicit trail colour still wins.
/// * The additive-only styles (lumen, rainbow, …) keep crown, ring and beam with no trail
///   body; water drops the beam, laser keeps it.
/// * "off", and `cursor_trail = false` for any style, disable both layers.
#[test]
fn trail_style_spellings_map_to_their_layers() {
    #[derive(Default)]
    struct Row {
        label: &'static str,
        master: Option<bool>,
        style: &'static str,
        radius: Option<f32>,
        ring: Option<bool>,
        color: Option<&'static str>,
        want_enabled: Option<bool>,
        want_style: Option<GlowStyle>,
        want_color: Option<u32>,
        /// `Some(true)`: a bloom crown (radius > 0); `Some(false)`: beam-only (radius 0).
        want_crown: Option<bool>,
        want_ring: Option<bool>,
        want_beam: Option<bool>,
        want_trail: Option<bool>,
        want_comet: Option<bool>,
    }
    let rows = [
        Row {
            label: "comet",
            master: Some(true),
            style: "comet",
            want_enabled: Some(true),
            want_style: Some(GlowStyle::Comet),
            // No pinned trail colour: the comet defaults to glacial blue.
            want_color: Some(crate::cursor_glow::COMET_DEFAULT_COLOR),
            want_crown: Some(true),
            want_ring: Some(true),
            want_beam: Some(true),
            want_trail: Some(true),
            want_comet: Some(true),
            ..Row::default()
        },
        Row {
            label: "beam, crown and ring configured on",
            master: Some(true),
            style: "beam",
            radius: Some(1.5),
            ring: Some(true),
            want_enabled: Some(true),
            want_style: Some(GlowStyle::Beam),
            want_color: Some(crate::cursor_glow::BEAM_DEFAULT_COLOR),
            want_crown: Some(false),
            want_ring: Some(false),
            // The beam style is the pure additive beam.
            want_beam: Some(true),
            want_trail: Some(false),
            want_comet: Some(false),
            ..Row::default()
        },
        Row {
            label: "beam with an explicit trail colour",
            master: Some(true),
            style: "beam",
            radius: Some(1.5),
            ring: Some(true),
            color: Some("#ff00ff"),
            want_color: Some(0x00FF_00FF),
            ..Row::default()
        },
        Row {
            label: "lumen",
            master: Some(true),
            style: "lumen",
            want_enabled: Some(true),
            want_crown: Some(true),
            want_ring: Some(true),
            want_beam: Some(true),
            want_trail: Some(false),
            ..Row::default()
        },
        Row {
            label: "water keeps its crown, droplets only",
            master: Some(true),
            style: "water",
            want_enabled: Some(true),
            want_crown: Some(true),
            want_beam: Some(false),
            ..Row::default()
        },
        Row {
            label: "laser keeps its monochrome beam",
            master: Some(true),
            style: "laser",
            want_beam: Some(true),
            ..Row::default()
        },
        Row {
            label: "off",
            master: Some(true),
            style: "off",
            want_enabled: Some(false),
            want_trail: Some(false),
            ..Row::default()
        },
        Row {
            label: "master off, style comet",
            master: Some(false),
            style: "comet",
            want_enabled: Some(false),
            want_trail: Some(false),
            ..Row::default()
        },
    ];
    for row in rows {
        let label = row.label;
        let mut app = App::headless_for_test();
        app.config.cursor_trail_style = Some(row.style.into());
        if let Some(master) = row.master {
            app.config.cursor_trail = Some(master);
        }
        if let Some(radius) = row.radius {
            app.config.cursor_trail_radius = Some(radius);
        }
        if let Some(ring) = row.ring {
            app.config.cursor_trail_ring = Some(ring);
        }
        if let Some(color) = row.color {
            app.config.cursor_trail_color = Some(color.into());
        }
        let g = app.glow_config();
        if let Some(want) = row.want_enabled {
            assert_eq!(g.enabled, want, "{label}: aurora enabled");
        }
        if let Some(want) = row.want_style {
            assert_eq!(g.style, want, "{label}: glow style");
        }
        if let Some(want) = row.want_color {
            assert_eq!(g.color, want, "{label}: colour");
        }
        match row.want_crown {
            Some(true) => assert!(g.radius > 0.0, "{label}: keeps the bloom crown"),
            Some(false) => assert_eq!(g.radius, 0.0, "{label}: beam-only, no bloom crown"),
            None => {}
        }
        if let Some(want) = row.want_ring {
            assert_eq!(g.ring, want, "{label}: landing ring");
        }
        if let Some(want) = row.want_beam {
            assert_eq!(g.beam, want, "{label}: additive beam");
        }
        if let Some(want) = row.want_trail {
            assert_eq!(
                app.trail_config().enabled,
                want,
                "{label}: comet trail body"
            );
        }
        if let Some(want) = row.want_comet {
            assert_eq!(app.trail_is_comet(), want, "{label}: trail_is_comet");
        }
    }
}

#[test]
fn forge_cursor_fill_obeys_body_master_and_style_gates() {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("fire".into());
    let fire = app.glow_config();
    assert_eq!(
        forge_cursor_fill(true, &fire, || Some(0x00FE_DCBA)),
        Some(0x00FE_DCBA),
        "negative control: an enabled Fire cursor may use the forge fill"
    );

    assert_eq!(
        forge_cursor_fill(false, &fire, || panic!("suppressed forge state sampled")),
        None,
        "serious mode's CursorBody gate suppresses retained Fire fill"
    );

    let mut zero_intensity = fire;
    zero_intensity.intensity = 0.0;
    assert_eq!(
        forge_cursor_fill(true, &zero_intensity, || {
            panic!("zero-intensity forge state sampled")
        }),
        None,
        "reduced motion and zero-intensity configurations suppress Fire fill"
    );

    app.config.cursor_trail = Some(false);
    let disabled = app.glow_config();
    assert_eq!(
        forge_cursor_fill(true, &disabled, || panic!("disabled forge state sampled")),
        None,
        "the cursor-trail master disables Fire fill too"
    );

    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("lumen".into());
    let lumen = app.glow_config();
    assert_eq!(
        forge_cursor_fill(true, &lumen, || panic!("non-Fire forge state sampled")),
        None
    );
}

/// The colour the FRAME hands the engines (`cursor_frame`'s folded config and
/// comet colour), not the pre-frame `glow_config()` the table above reads: the
/// per-frame fold is where the live cursor colour lands, and where it once
/// overwrote every canonical hue with the theme's seed. One row per case of
/// the one colour law (`aterm_effects::cursor_fx::automatic_glow_color`); each
/// asserts the aurora colour, the comet body's colour and the automatic
/// accent, so crown, nucleus and trail cannot disagree in hue.
#[test]
fn the_frame_colour_follows_the_one_colour_law() {
    use crate::cursor_glow::{
        BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, LASER_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR,
    };
    // A program's OSC 12, and the live foreground an unpinned caret falls
    // back to (no OSC 12: the theme's seed, or OSC 10).
    const OSC12: [u8; 3] = [0xFF, 0x40, 0xC0];
    const LIVE_FG: [u8; 3] = [0xC8, 0xD3, 0xF5];
    let rgb = aterm_render::rgb_to_u32;
    // (case, style, cursor_trail_color, live cursor, pinned, the colour).
    type Row<'a> = (&'a str, &'a str, Option<&'a str>, [u8; 3], bool, u32);
    let rows: [Row<'_>; 10] = [
        (
            "comet, nothing pinned",
            "comet",
            None,
            LIVE_FG,
            false,
            COMET_DEFAULT_COLOR,
        ),
        ("comet under OSC 12", "comet", None, OSC12, true, rgb(OSC12)),
        (
            "an explicit trail colour beats a pinned cursor",
            "comet",
            Some("#123456"),
            OSC12,
            true,
            0x0012_3456,
        ),
        (
            "beam, nothing pinned",
            "beam",
            None,
            LIVE_FG,
            false,
            BEAM_DEFAULT_COLOR,
        ),
        ("beam under OSC 12", "beam", None, OSC12, true, rgb(OSC12)),
        (
            "sparkle, nothing pinned",
            "sparkle",
            None,
            LIVE_FG,
            false,
            SPARKLE_DEFAULT_COLOR,
        ),
        (
            "sparkle under OSC 12",
            "sparkle",
            None,
            OSC12,
            true,
            rgb(OSC12),
        ),
        // The negative control: a pin never recolours the lightning.
        (
            "laser under OSC 12",
            "laser",
            None,
            OSC12,
            true,
            LASER_DEFAULT_COLOR,
        ),
        // …and a style with no hue of its own follows the live colour pinned
        // or not, which is today's lumen behaviour.
        (
            "lumen, nothing pinned",
            "lumen",
            None,
            LIVE_FG,
            false,
            rgb(LIVE_FG),
        ),
        ("lumen under OSC 12", "lumen", None, OSC12, true, rgb(OSC12)),
    ];
    for (label, style, trail_color, live, pinned, want) in rows {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some(style.into());
        app.config.cursor_trail_color = trail_color.map(Into::into);
        let fx = app
            .cursor_frame(
                crate::WindowId(0),
                super::CursorFxInputs {
                    live_cursor_rgb: live,
                    cursor_color_pinned: pinned,
                    ..super::CursorFxInputs::sample_for_test(std::time::Instant::now())
                },
            )
            .expect("fixture window");
        assert_eq!(fx.glow_cfg.color, want, "{label}: aurora colour");
        // No keystroke, so no ignition heat: the comet's colour is its base.
        assert_eq!(fx.trail_color, want, "{label}: comet body colour");
        assert_eq!(
            fx.glow_cfg.accent,
            aterm_effects::cursor_fx::brighten(want, 1.5),
            "{label}: automatic accent"
        );
    }
}
