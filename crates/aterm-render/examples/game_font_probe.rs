//! ASCII-art probe: paint a line of text through the REAL glyph path
//! (`glyph_image` + the documented `cell_x + xmin` / `baseline - h - ymin`
//! anchors) so spacing, clipping and dropped glyphs are visible offline.
use aterm_render::{Renderer, Theme};

fn main() {
    let mut args = std::env::args().skip(1);
    let family = args.next().unwrap_or_else(|| "game:roblox".into());
    let px: f32 = args.next().unwrap_or_else(|| "16".into()).parse().unwrap();
    let text = args.next().unwrap_or_else(|| "minecraft, period.".into());

    let mut r = Renderer::from_system_with_family(Some(&family), px, Theme::default())
        .expect("renderer");
    let (cw, ch) = r.cell_size();
    let base = r.baseline();
    let cols = text.chars().count();
    let (w, h) = (cw * cols, ch);
    let mut canvas = vec![0u8; w * h];

    for (col, c) in text.chars().enumerate() {
        let key = r.glyph_key(c);
        let img = r.glyph_image(key);
        let (gw, gh, xmin, ymin) = (img.width(), img.height(), img.xmin(), img.ymin());
        let bytes = img.bytes().to_vec();
        let x0 = (col * cw) as i32 + xmin;
        let y0 = base - gh as i32 - ymin;
        for gy in 0..gh {
            for gx in 0..gw {
                let (x, y) = (x0 + gx as i32, y0 + gy as i32);
                if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
                    let v = bytes[gy * gw + gx];
                    let p = &mut canvas[y as usize * w + x as usize];
                    *p = (*p).max(v);
                }
            }
        }
    }

    println!("{family}  px={px}  cell={cw}x{ch}  baseline={base}  text={text:?}");
    let ramp = [' ', '.', ':', '*', '#', '@'];
    for y in 0..h {
        let row: String = (0..w)
            .map(|x| ramp[(canvas[y * w + x] as usize * (ramp.len() - 1)) / 255])
            .collect();
        println!("|{row}|");
    }
    // Cell ruler: every cell boundary marked, so gaps are attributable.
    let ruler: String = (0..w).map(|x| if x % cw == 0 { '+' } else { '-' }).collect();
    println!("|{ruler}|");
}
