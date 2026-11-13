// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn an_underscore_has_ink_under_subpixel_rendering_too() {
    let mut renderer =
        crate::Renderer::from_system(15.0, crate::Theme::default()).expect(crate::NO_FONT);
    if !renderer.set_font_subpixel("rgb") {
        return;
    }
    let mut input = crate::RenderInput::empty();
    input.rows = 1;
    input.cols = 2;
    input.cells = vec![vec![crate::RenderCell::default(); 2]];
    for (i, ch) in "_-".chars().enumerate() {
        input.cells[0][i].ch = ch;
        input.cells[0][i].fg = [255, 255, 255];
        input.cells[0][i].bg = [0, 0, 0];
    }
    let frame = renderer.render_input(&input);
    let ground = frame.pixels[0];
    let ink = |cell: usize| {
        let w = frame.width / 2;
        (0..frame.height)
            .flat_map(|y| (cell * w..(cell + 1) * w).map(move |x| (x, y)))
            .filter(|&(x, y)| frame.pixels[y * frame.width + x] != ground)
            .count()
    };
    assert!(
        ink(1) > 0,
        "the control glyph must have ink, or this measures nothing"
    );
    assert!(
        ink(0) > 0,
        "an underscore is invisible under subpixel rendering: {} ink pixels \
         against the hyphen's {}",
        ink(0),
        ink(1)
    );
}
