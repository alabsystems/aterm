// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// TEMPORARY diagnostic probe (delete after use): for each codepoint report
// aterm's cell width, the resolved face, the rasterized ink width/advance in
// device px, and how many CELLS the ink actually covers.
use aterm_render::{Renderer, Theme};

fn main() {
    let px: f32 = std::env::var("PROBE_PX").ok().and_then(|s| s.parse().ok()).unwrap_or(24.0);
    let mut r = Renderer::from_system(px, Theme::default()).expect("no font");
    r.debug_block_on_lazy_fallbacks();
    let (cell_w, cell_h) = r.cell_size();
    println!("primary = {:?}", r.primary_source_path());
    println!("px = {px}  cell = {cell_w}x{cell_h}");
    println!("{:<8} {:<40} {:>5} {:>12} {:>6} {:>6} {:>8} {:>10} {:>10}",
        "cp", "name", "wcw", "face", "xmin", "ink_w", "advance", "ink_cells", "adv_cells");
    let items: &[(char, &str)] = &[
        ('\u{27F5}', "LONG LEFTWARDS ARROW"),
        ('\u{27F6}', "LONG RIGHTWARDS ARROW"),
        ('\u{27F7}', "LONG LEFT RIGHT ARROW"),
        ('\u{27F8}', "LONG LEFTWARDS DOUBLE ARROW"),
        ('\u{27F9}', "LONG RIGHTWARDS DOUBLE ARROW"),
        ('\u{27FA}', "LONG LEFT RIGHT DOUBLE ARROW"),
        ('\u{27FC}', "LONG RIGHTWARDS ARROW FROM BAR"),
        ('\u{21D2}', "RIGHTWARDS DOUBLE ARROW"),
        ('\u{21D4}', "LEFT RIGHT DOUBLE ARROW"),
        ('\u{2192}', "RIGHTWARDS ARROW"),
        ('\u{2194}', "LEFT RIGHT ARROW"),
        ('\u{2190}', "LEFTWARDS ARROW"),
        ('\u{2500}', "BOX DRAWINGS LIGHT HORIZONTAL"),
        ('\u{2502}', "BOX DRAWINGS LIGHT VERTICAL"),
        ('\u{253C}', "BOX DRAWINGS LIGHT VERTICAL AND HORIZONTAL"),
        ('M', "LATIN CAPITAL LETTER M"),
        ('\u{4E00}', "CJK IDEOGRAPH ONE"),
        ('\u{2261}', "IDENTICAL TO"),
        ('\u{2264}', "LESS-THAN OR EQUAL TO"),
        ('\u{27E8}', "MATHEMATICAL LEFT ANGLE BRACKET"),
        ('\u{2A7D}', "LESS-THAN OR SLANTED EQUAL TO"),
        ('\u{27F0}', "UPWARDS QUADRUPLE ARROW"),
        ('\u{2B0C}', "LEFT RIGHT BLACK ARROW"),
    ];
    for &(ch, name) in items {
        let wcw = aterm_grapheme::char_width(ch);
        let key = r.glyph_key(ch);
        let face = format!("{:?}", key.source);
        let img = r.glyph_image(key);
        let (w, xmin, adv) = (img.width(), img.xmin(), img.advance());
        // ink cells: how many cells the painted span [xmin, xmin+w) touches
        let right = xmin + w as i32;
        let ink_cells = right as f32 / cell_w as f32;
        let adv_cells = adv / cell_w as f32;
        println!("U+{:04X}  {:<40} {:>5} {:>12} {:>6} {:>6} {:>8.2} {:>10.2} {:>10.2}",
            ch as u32, name, wcw, face, xmin, w, adv, ink_cells, adv_cells);
    }
    println!();
    for path in ["/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
                 "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
                 "/System/Library/Fonts/SFNSMono.ttf",
                 "/System/Library/Fonts/Apple Symbols.ttf",
                 "/System/Library/Fonts/Supplemental/NotoSansMath-Regular.ttf",
                 "/Users//example/aterm/crates/aterm-render/assets/DejaVuSansMono.ttf"] {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let Ok(f) = ttf_parser::Face::parse(&bytes, 0) else { continue };
        let upem = f32::from(f.units_per_em());
        print!("{path}  upem={upem}  ");
        for cp in [0x27F9u32, 0x27FA, 0x21D2, 0x2192, 0x4E00] {
            let ch = char::from_u32(cp).unwrap();
            match f.glyph_index(ch).filter(|g| g.0 != 0) {
                Some(g) => {
                    let adv = f.glyph_hor_advance(g).unwrap_or(0);
                    let bb = f.glyph_bounding_box(g);
                    let inkw = bb.map(|b| (b.x_max - b.x_min) as f32).unwrap_or(0.0);
                    print!("U+{cp:04X}=adv {:.3}em/ink {:.3}em  ", adv as f32/upem, inkw/upem);
                }
                None => print!("U+{cp:04X}=MISS  "),
            }
        }
        println!();
    }
}
