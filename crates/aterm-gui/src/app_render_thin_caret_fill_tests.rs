// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The windowed app draws a bar caret in the colour the effects lifted for it: every present
//! path takes the caret's colour from `CursorFxFrame::caret_fill` (a bar or underline caret's
//! lifted colour over the block's) through [`presented_caret_fill`], as the pipeline's own
//! publish and the Settings preview do. Found by composed r2's gallery: the replay read the
//! lifted bar at 4.5 or more, while the app, taking the block's fill alone, drew GitHub Light's
//! #0969da on Comet's disc at 2.4. Since after-land L2 the paper Comet's head seats on a bar and
//! lays no light past it, so a bar's own cell under Comet stays the page and is read against
//! nothing; the fixture types Classic, the style whose streak still lights a bar's cell on
//! paper (its block fill alone reads 3.4-3.7:1 there).

use super::*;
use aterm_effects::color_math::contrast_ratio;
use std::time::Duration;

/// The three present paths that draw a caret: a lone pane's redraw
/// (`redraw_window_with_layout`, into the input scratch's `cursor_fill_override`), a split of
/// terminals' composed redraw (`redraw_compose_after_focused_extract`, into the window's
/// `composed_cursor_fill`), and a terminal beside a native view (`composed_cursor_frame_sampled`,
/// into the same).
#[derive(Clone, Copy, Debug)]
enum Path {
    Lone,
    Split,
    Mixed,
}

/// GitHub Light's caret colour, the bar the theme draws when the effects lift nothing.
fn theme_bar() -> u32 {
    aterm_types::scheme::builtin("GitHub Light")
        .expect("built-in light theme")
        .cursor
        .map_or(0x0009_69DA, |c| {
            (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
        })
}

/// Classic typing a line on GitHub Light behind a bar caret down `path`, one key every 80 ms on
/// a 16 ms frame grid: each frame that read its bar against a ground, with the colour that
/// path's present drew the bar in (the theme's where it drew no override).
fn typed_on_paper(path: Path) -> Vec<(u32, u32)> {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.theme = Some("GitHub Light".into());
    app.config.cursor_style = Some("bar".into());
    app.config.cursor_blink = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("classic".into());
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(false);
    let _ = app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
    let session = match path {
        Path::Lone => 0,
        Path::Split => app.split_active_stub_tab(wid),
        Path::Mixed => {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal)
                .0
        }
    };
    let term = app
        .pool
        .get(session)
        .expect("the typed terminal")
        .term
        .clone();
    {
        // A lone pane presents onto a surface: the scripted CPU one a headless window draws on.
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.present = Some(crate::PresentTarget::Cpu {
            surface: crate::present::CpuSurface::Scripted(Default::default()),
        });
        ws.focused = true;
    }
    {
        // Every path types on GitHub Light: a split's stub pane takes the scheme's colours as
        // the installed ones give the window's first pane theirs.
        let scheme = aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme");
        let mut typed = term_lock(&term);
        typed.set_default_foreground(scheme.foreground);
        typed.set_default_background(scheme.background);
        typed.set_default_cursor_color(scheme.cursor);
        typed.process(b"\x1b[6 q$ ");
    }
    let bar = theme_bar();
    let t0 = Instant::now();
    let mut read = Vec::new();
    for step in 1..=60u64 {
        let now = t0 + Duration::from_millis(16 * step);
        if step % 5 == 0 {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.fx.cursor_mut().glow.note_typed_cells(now, 1);
            ws.fx.cursor_mut().trail.note_typed(now);
            term_lock(&term).process(b"x");
        }
        let drawn = match path {
            Path::Lone => {
                app.redraw_window_at(wid, now);
                app.windows[&wid].input_scratch.cursor_fill_override
            }
            Path::Split => {
                let ws = &app.windows[&wid];
                let (rows, cols) = (usize::from(ws.rows), usize::from(ws.cols));
                let _ = app.redraw_compose(wid, rows, cols, false, false, None, 0, now);
                app.windows[&wid].composed_cursor_fill
            }
            Path::Mixed => {
                let _ = app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(now));
                app.windows[&wid].composed_cursor_fill
            }
        };
        if let Some(ground) = app.windows[&wid].fx.thin_caret_ground() {
            read.push((ground, drawn.unwrap_or(bar)));
        }
    }
    read
}

/// Every present path draws the bar at 4.5:1 or more on the light it sits in, on every frame
/// that read it against a ground. Negative control (`CARET_FILL_READ` off: each path draws
/// the block's fill alone, as before): on each path some frame draws the bar under 4.5:1.
#[test]
fn the_app_draws_a_bar_caret_in_its_lifted_colour() {
    for path in [Path::Lone, Path::Split, Path::Mixed] {
        let read = typed_on_paper(path);
        assert!(
            read.len() >= 20,
            "{path:?}: the bar is read against its cell on most frames: {}",
            read.len()
        );
        for &(ground, drawn) in &read {
            let c = contrast_ratio(drawn, ground);
            assert!(
                c >= 4.5 - 0.02,
                "{path:?}: the app draws the bar in #{drawn:06x}, {c:.2}:1 on its cell's \
                 #{ground:06x}"
            );
        }
        CARET_FILL_READ.with(|on| on.set(false));
        let old = typed_on_paper(path);
        CARET_FILL_READ.with(|on| on.set(true));
        let worst = old
            .iter()
            .map(|&(ground, drawn)| contrast_ratio(drawn, ground))
            .fold(f32::INFINITY, f32::min);
        assert!(
            worst < 4.5,
            "negative control: {path:?} drawing the block's fill alone reads {worst:.2}:1 at \
             worst"
        );
    }
}
