// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that authored chrome color tokens reach every surface that paints them.

use super::*;
use crate::app_colorscheme::ThemeChange;

const AUTHORED: &str = "accent_color = \"#ff00aa\"\n\
                        border_color = \"#5a5a5a\"\n\
                        search_match_color = \"#335533\"\n\
                        active_tab_color = \"#123456\"\n";

const ACCENT: [u8; 3] = [0xff, 0x00, 0xaa];
const ACTIVE_TAB: [u8; 3] = [0x12, 0x34, 0x56];
const BORDER: [u8; 3] = [0x5a, 0x5a, 0x5a];

/// What each surface drew in one App.
#[derive(Debug)]
struct Drawn {
    /// Every strip cell's ground and rule.
    strip: Vec<([u8; 3], Option<[u8; 3]>)>,
    /// Every cell ground of the composed split frame.
    composed: Vec<[u8; 3]>,
    /// The seam token the split's divider takes on the terminal background.
    seam: [u8; 3],
    /// The Settings pages' accent.
    native_accent: [u8; 3],
    /// The matrix rain's accent.
    rain_accent: u32,
    /// The drop-target highlight's accent.
    drop_accent: u32,
}

fn drawn(config: &str) -> Drawn {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config = aterm_toml::from_str(config).expect("valid toml");
    app.install_resolved_colors(ThemeChange::ConfigReload);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.tab_strip_rows = 1;
    app.splice_tab_strip_with(wid, 1);
    let strip = app.windows[&wid].input_scratch.cells[0]
        .iter()
        .map(|cell| (cell.bg, cell.underline_color))
        .collect();
    app.tab_strip_rows = 0;
    let _ = app.split_active_stub_tab(wid);
    app.windows.get_mut(&wid).expect("window").focused = false;
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (usize::from(ws.rows), usize::from(ws.cols))
    };
    assert!(
        app.redraw_compose(wid, rows, cols, false, false, None, 0, Instant::now())
            .is_some(),
        "the split composes"
    );
    let composed = app.windows[&wid]
        .input_scratch
        .cells
        .iter()
        .flatten()
        .flat_map(|cell| {
            let hairline = ('\u{2500}'..='\u{257f}').contains(&cell.ch);
            [cell.bg, if hairline { cell.fg } else { cell.bg }]
        })
        .collect();
    let grid = app.chrome_palette(ChromeSurface::Grid);
    let seam = grid
        .tokens()
        .seam_on(crate::settings::u32_rgb(grid.theme.bg));
    let native_accent = crate::native_appearance::NativeRoles::from_theme(
        app.chrome_palette(ChromeSurface::NativePage),
    )
    .accent;
    app.recompute_matrix_rain();
    let rain_accent = app.rain.expect("rain resolves").theme_accent;
    app.windows.get_mut(&wid).expect("window").drag_hover = true;
    let drop_accent = app
        .host_visual_state(wid, Instant::now())
        .overlay
        .expect("the drop highlight paints")
        .accent;
    Drawn {
        strip,
        composed,
        seam,
        native_accent,
        rain_accent,
        drop_accent,
    }
}

/// THE AUTHORED CHROME REACHES EVERY SURFACE: one `aterm.toml` with the chrome
/// keys, and the strip's selected tab wears `active_tab_color` and, where it
/// has one, its rule the accent; the split's divider is the authored border (held to its floor, which
/// `#5a5a5a` on Default already clears); the Settings accent wears the accent's
/// hue; the rain and the drop highlight wear the accent. NEGATIVE CONTROL: the
/// same App without the keys draws none of them.
#[test]
fn authored_chrome_reaches_every_surface() {
    let hue = |c: [u8; 3]| aterm_messages::palette::oklch(c).2;
    let authored = drawn(AUTHORED);
    let derived = drawn("");
    assert!(
        authored.strip.iter().any(|(bg, _)| *bg == ACTIVE_TAB),
        "the selected tab is the authored colour: {:?}",
        authored.strip
    );
    assert_eq!(
        authored.strip.iter().any(|(_, rule)| *rule == Some(ACCENT)),
        !crate::tab_bar::STRIP_CHIP_CARDS,
        "the selected tab's rule is the accent where the strip draws one: {:?}",
        authored.strip
    );
    assert_eq!(
        authored.seam, BORDER,
        "the border clears its floor as authored"
    );
    assert!(
        authored.composed.contains(&BORDER),
        "the divider is the authored border"
    );
    let gap = (hue(authored.native_accent) - hue(ACCENT)).abs();
    assert!(
        gap.min(360.0 - gap) < 4.0,
        "the Settings accent {:02x?} wears the accent's hue",
        authored.native_accent
    );
    assert_eq!(authored.rain_accent, aterm_render::rgb_to_u32(ACCENT));
    assert_eq!(authored.drop_accent, aterm_render::rgb_to_u32(ACCENT));

    assert!(!derived.strip.iter().any(|(bg, _)| *bg == ACTIVE_TAB));
    assert!(!derived.strip.iter().any(|(_, rule)| *rule == Some(ACCENT)));
    assert_ne!(derived.seam, BORDER);
    assert!(!derived.composed.contains(&BORDER));
    assert!(
        derived.composed.contains(&derived.seam),
        "CONTROL: the derived divider is the derived seam"
    );
    assert_ne!(derived.native_accent, authored.native_accent);
    assert_ne!(derived.rain_accent, authored.rain_accent);
    assert_ne!(derived.drop_accent, authored.drop_accent);
}
