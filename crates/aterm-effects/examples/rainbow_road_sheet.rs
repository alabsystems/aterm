// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A contact sheet of the setup screen's rainbow road ([`aterm_effects::rainbow_road`]) for a
//! person to judge: the run's moments on the dark and the light ground at 2x, the done moment on
//! the least road too, then every pose at 2.4x. Dev only; the page's own frames come from `lab system-state-gallery`.
//!
//! `targo --unverified run -p aterm-effects --example rainbow_road_sheet -- <out.png>`

use aterm_effects::rainbow_road::{
    CAT_FEET, CAT_H, CatArt, CatPose, LEAST_LENGTH, ROAD_H, ROAD_X, RoadArt,
};
use aterm_scene::Tile;

/// The page grounds the default dark theme and "GitHub Light" paint.
const DARK: [u8; 3] = [0x1E, 0x1E, 0x1E];
const LIGHT: [u8; 3] = [0xFF, 0xFF, 0xFF];
/// Device pixels a unit for the moments, and the poses.
const UNIT: f32 = 2.0;
const POSE_UNIT: f32 = 2.4;
/// Units of road past the words, as the approved render lays out.
const LENGTH: u16 = 802;

struct Sheet {
    w: u32,
    h: u32,
    rgb: Vec<u8>,
}

impl Sheet {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            rgb: vec![0; (w * h * 3) as usize],
        }
    }

    fn rect(&mut self, x: u32, y: u32, w: u32, h: u32, rgb: [u8; 3]) {
        for row in y..(y + h).min(self.h) {
            for col in x..(x + w).min(self.w) {
                let i = ((row * self.w + col) * 3) as usize;
                self.rgb[i..i + 3].copy_from_slice(&rgb);
            }
        }
    }

    fn over(&mut self, tile: &Tile, x: i32, y: i32) {
        for (row, line) in tile
            .pixels()
            .chunks_exact(tile.width() as usize * 4)
            .enumerate()
        {
            for (col, p) in line.chunks_exact(4).enumerate() {
                let (sx, sy) = (x + col as i32, y + row as i32);
                if p[3] == 0 || sx < 0 || sy < 0 || sx as u32 >= self.w || sy as u32 >= self.h {
                    continue;
                }
                let i = ((sy as u32 * self.w + sx as u32) * 3) as usize;
                let a = f32::from(p[3]) / 255.0;
                for c in 0..3 {
                    let d = f32::from(self.rgb[i + c]);
                    self.rgb[i + c] = (d + (f32::from(p[c]) - d) * a).round() as u8;
                }
            }
        }
    }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/rainbow_road_sheet.png".to_owned());
    let least = LEAST_LENGTH as u16;
    let moments: [(u16, u16, bool, CatPose); 6] = [
        (LENGTH, 0, false, CatPose::Sit),
        (LENGTH, 400, false, CatPose::Walk),
        (LENGTH, 600, false, CatPose::LookUp),
        (LENGTH, 300, false, CatPose::Worried),
        (LENGTH, 1000, true, CatPose::Loaf),
        (least, 1000, true, CatPose::Loaf),
    ];
    let road_h = (ROAD_H * UNIT) as u32;
    let lane_w = ((ROAD_X + f32::from(LENGTH)) * UNIT) as u32;
    let pose_h = (CAT_H * POSE_UNIT) as u32;
    let pose_w = (pose_h as f32 * CatArt::ASPECT).round() as u32;
    let grounds = [(true, DARK), (false, LIGHT)];
    let w = lane_w * 2;
    let h = road_h * moments.len() as u32 + pose_h * 2;
    let mut sheet = Sheet::new(w, h);
    for (column, (dark, ground)) in grounds.into_iter().enumerate() {
        let x0 = lane_w * column as u32;
        sheet.rect(x0, 0, lane_w, h, ground);
        for (row, (length, permille, done, pose)) in moments.into_iter().enumerate() {
            let y0 = road_h * row as u32;
            let art = RoadArt {
                length,
                permille,
                done,
                dark,
            };
            let rw = (road_h as f32 * art.aspect()).round() as u32;
            let road_x = x0 as i32 + (ROAD_X * UNIT) as i32;
            sheet.over(&art.bake(rw, road_h), road_x, y0 as i32);
            let road = art.road();
            let cat = CatArt::on(road, pose, dark);
            let ch = (CAT_H * UNIT) as u32;
            let cw = (ch as f32 * CatArt::ASPECT).round() as u32;
            let (fx, fy) = road.feet();
            sheet.over(
                &cat.bake(cw, ch),
                x0 as i32 + ((fx - CAT_FEET.0) * UNIT).round() as i32,
                y0 as i32 + ((fy - CAT_FEET.1) * UNIT).round() as i32,
            );
        }
        let y0 = road_h * moments.len() as u32;
        for (at, pose) in CatPose::ALL.into_iter().enumerate() {
            let tile = CatArt {
                pose,
                tilt: 0,
                dark,
            }
            .bake(pose_w, pose_h);
            let (col, row) = (at as u32 % 3, at as u32 / 3);
            sheet.over(
                &tile,
                (x0 + col * (pose_w + 20)) as i32,
                (y0 + row * pose_h) as i32,
            );
        }
    }
    let file = std::fs::File::create(&out).expect("create the sheet");
    let mut enc = aterm_png::Encoder::new(std::io::BufWriter::new(file), sheet.w, sheet.h);
    enc.set_color(aterm_png::ColorType::Rgb);
    enc.set_depth(aterm_png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&sheet.rgb)
        .expect("png data");
    println!("{out}");
}
