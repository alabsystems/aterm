// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Ink-extent probe: for each pose a relay body can wear, report the RGBA
// alpha bounding box inside the 56x34 (cell 10x20) dest tile.
use aterm_effects::cat_baker::CatColorKey;
use aterm_effects::pet_baker::PetBakeKey;
use aterm_effects::pet_glyphs_gen::PetGlyphId as P;

fn main() {
    let w: u16 = 56;
    let h: u16 = 34;
    let poses = [
        ("Run0", P::PetRun0),
        ("Run1", P::PetRun1),
        ("Run2", P::PetRun2),
        ("Run3", P::PetRun3),
        ("Walk0", P::PetWalk0),
        ("Walk1", P::PetWalk1),
        ("Walk2", P::PetWalk2),
        ("Walk3", P::PetWalk3),
        ("Crouch", P::PetCrouch),
        ("CrouchWiggle", P::PetCrouchWiggle),
        ("LeapRise", P::PetLeapRise),
        ("Leap", P::PetLeap),
        ("LeapDescend", P::PetLeapDescend),
        ("Land", P::PetLand),
        ("Stand", P::PetStand),
        ("Perk", P::PetPerk),
        ("Sit", P::PetSit),
        ("Loaf", P::PetLoaf),
        ("Startle", P::PetStartle),
        ("Stretch", P::PetStretch),
        ("Playbow", P::PetPlaybow),
    ];
    println!("pose,top_px,bot_px,left_px,right_px,ink_rows,top_frac_rows,bot_gap_px");
    for (name, p) in poses {
        let key = PetBakeKey {
            pose: p,
            coat: 3,
            iris: 2,
            colors: CatColorKey::default(),
            w,
            h,
        };
        let t = key.bake();
        let px = t.pixels();
        let (mut top, mut bot, mut left, mut right) = (h as i32, -1i32, w as i32, -1i32);
        // per-row coverage too
        let mut rows = Vec::new();
        for y in 0..h as i32 {
            let mut cnt = 0;
            let mut lo = w as i32;
            let mut hi = -1i32;
            for x in 0..w as i32 {
                let a = px[((y as usize * w as usize + x as usize) * 4) + 3];
                if a >= 16 {
                    cnt += 1;
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
            rows.push((cnt, lo, hi));
            if cnt > 0 {
                if y < top {
                    top = y
                }
                if y > bot {
                    bot = y
                }
                if lo < left {
                    left = lo
                }
                if hi > right {
                    right = hi
                }
            }
        }
        println!(
            "{name},{top},{bot},{left},{right},{},{:.4},{}",
            bot - top + 1,
            top as f32 / 20.0,
            h as i32 - 1 - bot
        );
        // dump row coverage compactly
        let prof: Vec<String> = rows.iter().map(|(c, _, _)| format!("{c}")).collect();
        println!("   rowcov: {}", prof.join(" "));
        let ext: Vec<String> = rows
            .iter()
            .map(|(c, lo, hi)| {
                if *c > 0 {
                    format!("{lo}-{hi}")
                } else {
                    "-".into()
                }
            })
            .collect();
        println!("   rowext: {}", ext.join(" "));
    }
}
