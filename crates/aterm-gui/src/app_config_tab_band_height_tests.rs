// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(windows)]
use super::Config;
use super::{TabBandHeight, synthetic_band_head_px};

#[cfg(windows)]
fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[cfg(windows)]
fn window_cell(config: &Config) -> ((usize, usize), i32, String) {
    let face = Config::font_family_admission(config.font_family_request().as_deref())
        .expect("the family is admitted");
    let mut r = aterm_render::Renderer::from_system_with_face(
        face.as_ref(),
        super::resolve_font_px(config),
        aterm_render::Theme::default(),
    )
    .expect("a face");
    r.set_line_height(config.line_height_or_default());
    (
        r.cell_size(),
        r.baseline(),
        r.primary_source_path().unwrap_or_default().to_string(),
    )
}

#[cfg(windows)]
#[test]
fn the_windows_defaults_draw_the_judged_9x23_cell() {
    assert_eq!(crate::FONT_PX, 17.0);
    assert_eq!(super::DEFAULT_LINE_HEIGHT, 1.1);
    assert_eq!(
        window_cell(&Config::default()),
        (
            (9, 23),
            17,
            aterm_render::bundled::IOSEVKA_NL_ID.to_string()
        )
    );
}

/// A config that names an Iosevka Term set draws that set, on the cell the default draws: the
/// set with its ligatures (`Iosevka Term`), the heavier one that was the default before
/// (`Iosevka Term Semibold`), and the default's own by name (`Iosevka Term NL`). Control: each
/// is a face of its own, and none is the default's unless it names it.
#[cfg(windows)]
#[test]
fn a_config_that_names_an_iosevka_term_set_draws_that_set() {
    use aterm_render::bundled::{
        IOSEVKA_FAMILY, IOSEVKA_ID, IOSEVKA_NL_FAMILY, IOSEVKA_NL_ID, IOSEVKA_SEMIBOLD_FAMILY,
        IOSEVKA_SEMIBOLD_ID,
    };
    let mut drawn = Vec::new();
    for (family, id) in [
        (IOSEVKA_FAMILY, IOSEVKA_ID),
        (IOSEVKA_SEMIBOLD_FAMILY, IOSEVKA_SEMIBOLD_ID),
        (IOSEVKA_NL_FAMILY, IOSEVKA_NL_ID),
    ] {
        if aterm_render::resolve_font_family(family, aterm_render::FaceTarget::REGULAR).is_some() {
            aterm_spec::host::could_not_run(format!(
                "{family} is installed on this host, and an installed face wins the name"
            ));
        }
        let named = cfg(&format!("font_family = \"{family}\"\n"));
        assert_eq!(
            window_cell(&named),
            ((9, 23), 17, id.to_string()),
            "{family}"
        );
        let bytes = aterm_render::read_resolved_font(id).expect("a bundled face");
        drawn.push((family, bytes));
    }
    for (i, (family, bytes)) in drawn.iter().enumerate() {
        for (other, other_bytes) in &drawn[i + 1..] {
            assert!(
                bytes[..] != other_bytes[..],
                "{family} and {other} are one face"
            );
        }
    }
    let default_id = window_cell(&Config::default()).2;
    assert_eq!(default_id, IOSEVKA_NL_ID);
}

#[cfg(windows)]
#[test]
fn three_lines_restore_the_cascadia_9x21_cell() {
    let family = aterm_render::bundled::CASCADIA_FAMILY;
    if aterm_render::resolve_font_family(family, aterm_render::FaceTarget::REGULAR).is_some() {
        aterm_spec::host::could_not_run(format!(
            "{family} is installed on this host, and an installed face wins the name"
        ));
    }
    let old = cfg("font_family = \"Cascadia Mono PL\"\nfont_px = 16\nline_height = 1.0\n");
    assert_eq!(
        window_cell(&old),
        ((9, 21), 15, aterm_render::bundled::CASCADIA_ID.to_string())
    );
    let lh = cfg("font_family = \"Cascadia Mono PL\"\nfont_px = 16\n");
    assert_ne!(window_cell(&lh).0, (9, 21));
    let px = cfg("font_family = \"Cascadia Mono PL\"\nline_height = 1.0\n");
    assert_ne!(window_cell(&px).0, (9, 21));
}

#[test]
fn synthetic_band_head_law() {
    let target = TabBandHeight::Standard.target_logical_px();
    let band = target as usize;
    let band_150 = (f64::from(target) * 1.5).round() as usize;
    let cap = 10 * super::SYNTHETIC_BAND_HEAD_CELL_CAP;
    for (case, (t, pad_top, rows, cell_h, scale), expected) in [
        ("no strip", (target, 2, 0, 21, 1.0), 0),
        (
            "96 dpi totals the target",
            (target, 2, 1, 21, 1.0),
            band - 2 - 21,
        ),
        (
            "150% re-derives",
            (target, 3, 1, 28, 1.5),
            band_150 - 3 - 28,
        ),
        (
            "one row absorbs the target",
            (target, 2, 1, band - 2, 1.0),
            0,
        ),
        ("two rows absorb it", (target, 2, 2, 21, 1.0), 0),
        ("oversized target caps", (4000.0, 0, 1, 10, 1.0), cap),
        ("+inf target caps", (f32::INFINITY, 0, 1, 10, 1.0), cap),
        ("NaN target", (f32::NAN, 2, 1, 21, 1.0), 0),
        ("zero target", (0.0, 2, 1, 21, 1.0), 0),
        ("negative target", (-32.0, 2, 1, 21, 1.0), 0),
    ] {
        assert_eq!(
            synthetic_band_head_px(t, pad_top, rows, cell_h, scale),
            expected,
            "{case}"
        );
    }
    let at_1x = synthetic_band_head_px(32.0, 2, 1, 21, 1.0);
    assert_ne!(at_1x, 0, "control: a finite positive target has a head");
    for scale in [f64::NAN, f64::INFINITY, 0.0, -2.0] {
        assert_eq!(
            synthetic_band_head_px(32.0, 2, 1, 21, scale),
            at_1x,
            "scale {scale}"
        );
    }
}

#[cfg(windows)]
#[test]
fn the_shipped_windows_face_reaches_the_standard_band() {
    let Some(renderer) =
        aterm_render::Renderer::from_system(crate::FONT_PX, aterm_render::Theme::default())
    else {
        aterm_spec::host::could_not_run("no system monospace font here");
    };
    let target = TabBandHeight::Standard.target_logical_px();
    let pad_logical = Config::default().window_padding_top_or_default();
    let mut renderer = renderer;
    renderer.set_line_height(Config::default().line_height_or_default());
    for scale in [1.0_f64, 1.5] {
        let px = crate::font_px_for_scale(crate::FONT_PX, scale);
        renderer.set_px(px);
        let (_, cell_h) = renderer.cell_size();
        let pad_top = crate::logical_to_device_px(pad_logical, scale);
        let head = synthetic_band_head_px(target, pad_top, 1, cell_h, scale);
        let band = head + pad_top + cell_h;
        crate::logging::stderr_line!(
            "C3 band at scale {scale}: font_px={px} cell_h={cell_h} pad_top={pad_top} \
             before={} head={head} band={band}",
            pad_top + cell_h
        );
        if head > 0 {
            assert_eq!(
                band,
                crate::logical_to_device_px(target, scale),
                "the band lands ON the WinUI target at scale {scale}"
            );
        } else {
            assert!(
                band >= crate::logical_to_device_px(target, scale),
                "a zero head is only correct when the strip already fills the band"
            );
        }
    }
}
