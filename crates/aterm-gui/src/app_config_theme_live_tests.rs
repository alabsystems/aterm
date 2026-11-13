// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn apply_theme_live_recolours_backend_and_drops_caches() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);

    let mut term = aterm_core::terminal::Terminal::new(2, 2);
    term.process(b"\x1b[?25l");
    let input = term.cell_frame(2, 2);
    let render = |backend: &mut crate::Backend| match backend {
        crate::Backend::Cpu(r) => r.render_input(&input),
        crate::Backend::Gpu(_) => unreachable!("headless test backend is CPU"),
    };
    let before = render(app.backend.ready_mut());

    app.windows.get_mut(&wid).unwrap().last_strip_fp = Some((
        0xFEED,
        80,
        false,
        None,
        false,
        None,
        crate::tab_bar::VersionSlotCols::NONE,
        false,
        crate::tab_bar::CaptionSlot::NONE,
        None,
    ));

    let new_theme = aterm_render::Theme::from(&aterm_types::scheme::builtin("Dracula").unwrap());
    assert_ne!(
        app.theme.bg, new_theme.bg,
        "test needs a real colour change"
    );

    app.apply_theme_live(new_theme);

    assert_eq!(app.theme.bg, new_theme.bg);
    assert_eq!(app.theme.fg, new_theme.fg);
    assert!(app.windows[&wid].last_strip_fp.is_none(), "strip cache");
    assert!(app.windows[&wid].last_present.is_none(), "RepaintKey gate");
    let got = render(app.backend.ready_mut());
    assert_ne!(got.pixels, before.pixels, "the idle grid recoloured");
    let mut fresh = aterm_render::Renderer::from_system(crate::FONT_PX, new_theme)
        .expect("system font for test renderer");
    fresh.set_line_height(super::Config::default().line_height_or_default());
    assert_eq!(got.pixels, fresh.render_input(&input).pixels);
}
