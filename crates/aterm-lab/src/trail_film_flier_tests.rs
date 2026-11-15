// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nothing blinks: the flying cat's own animation, counted by the film's own [`Turns`].
//!
//! A real [`CursorCat`] flies the brand cat's glide (`set_body(Some(Cat))`) at a caret that
//! holds still through most of the script, so every turn a pixel makes there is the flier's own:
//! its breath, its face swaps, its dance, its one-shot reactions; then the caret runs a cell a
//! key at the film's burst speed, and the glide flies along with it over the page. Every 16 ms the frame is routed as the terminal routes it (the
//! bob through [`glide_bob_cells`]), placed and drawn by [`WordDecorations`], and every sprite it
//! emits is resampled bilinearly into its rect and composited, in linear light at the sprite's
//! alpha, on a white page and on a dark one. The film's flash law then reads each page: no pixel
//! makes more than six turns in any second.

use super::{FLASH_DARK_BELOW, FLASH_MOST, FLASH_SWING, PixelRead, Turns};
use aterm_effects::cat_baker::CatColorKey;
use aterm_effects::kitty_cursor::{
    CursorBody, CursorCat, FACE_LATCH_OFF, FLIER_CALM_OFF, SingSync, glide_bob_cells,
};
use aterm_effects::kitty_pet::PetSpecies;
use aterm_effects::kitty_registry::KittyLook;
use aterm_effects::kitty_sing::SING_BPM;
use aterm_effects::word_decorations::{
    EffectGeom, GLIDE_FOLLOWS, GLIDE_STRIDE_OFF, KittyCursorFrame, KittyCursorLayout,
    WordDecorations,
};
use aterm_render::sprites::FreeSprite;
use aterm_time::{Duration, Instant};

/// The film's frame step.
const STEP_MS: u32 = 16;
/// The caret's cell, (row, col): it moves only in the run, a cell a key.
const CARET: (u16, u16) = (4, 12);
const ROWS: u16 = 8;
const COLS: u16 = 100;
/// The pages: white and a dark theme's ground, each with its ink (for the cat's colour key).
const PAGES: [(&str, u32, u32); 2] = [
    ("white", 0xFF_FF_FF, 0x1F_23_28),
    ("dark", 0x1A_1B_26, 0xC0_CA_F5),
];

/// What the script does at one instant.
#[derive(Clone, Copy, Debug)]
enum Ev {
    Key,
    /// A key that moves the caret a cell to the right.
    Type,
    Delete,
    Enter,
    Curse,
    Delight,
}

/// The script's phases, in order, and how long each runs (ms).
const PHASES: [(&str, u32); 10] = [
    ("summon", 2_000),
    ("typing", 3_000),
    ("sing", 4_000),
    ("enter", 2_000),
    ("backspace", 2_000),
    ("wince", 1_500),
    ("delight", 1_500),
    ("flurry", 2_000),
    ("hover", 2_000),
    ("run", 3_500),
];

/// The phase at `ms` and its start.
fn phase_at(ms: u32) -> (usize, u32) {
    let mut from = 0;
    for (i, &(_, len)) in PHASES.iter().enumerate() {
        if ms < from + len {
            return (i, from);
        }
        from += len;
    }
    (PHASES.len() - 1, from)
}

fn total_ms() -> u32 {
    PHASES.iter().map(|&(_, len)| len).sum()
}

/// The script's events: a summoning run at 40 keys a second, then 3 s at 11 keys a second; a
/// 4 s song at [`SING_BPM`] (typing on); Enter every 300 ms between keys; Backspace bursts at
/// 12 a second between runs of keys; one wince phrase and one delight phrase over typing; a
/// flurry of oops and Enter faster than a face can stand; a hover with no keys at all; then the
/// film's burst (67 keys 50 ms apart), each key moving the caret a cell.
fn script() -> Vec<(u32, Ev)> {
    let mut out = Vec::new();
    let mut from = 0;
    for &(name, len) in &PHASES {
        let end = from + len;
        match name {
            "summon" => (from..end).step_by(25).for_each(|t| out.push((t, Ev::Key))),
            "typing" | "sing" => (from..end).step_by(91).for_each(|t| out.push((t, Ev::Key))),
            "run" => (from..end)
                .step_by(50)
                .take(67)
                .for_each(|t| out.push((t, Ev::Type))),
            "enter" => {
                for t in (from..end).step_by(91) {
                    out.push((t, Ev::Key));
                }
                for t in (from..end).step_by(300) {
                    out.push((t + 45, Ev::Enter));
                }
            }
            "backspace" => {
                // Two deletes at 12 a second, then a quick run of keys, over and over. (Four
                // drain the cat's momentum under its leave gate: it fades and is revived, the
                // lifecycle's dip, not the glide's own motion.)
                let mut t = from;
                while t < end {
                    for _ in 0..2 {
                        out.push((t, Ev::Delete));
                        t += 83;
                    }
                    for _ in 0..16 {
                        out.push((t, Ev::Key));
                        t += 40;
                    }
                }
            }
            "flurry" => {
                // Oops and delight, faster than a face can stand: a delete and an Enter, each
                // every 300 ms and 150 ms apart, in a run at 40 keys a second.
                for t in (from..end).step_by(25) {
                    out.push((t + 5, Ev::Key));
                }
                for t in (from..end).step_by(300) {
                    out.extend([(t, Ev::Delete), (t + 150, Ev::Enter)]);
                }
            }
            "wince" | "delight" => {
                (from..end).step_by(91).for_each(|t| out.push((t, Ev::Key)));
                out.push((
                    from + 200,
                    if name == "wince" {
                        Ev::Curse
                    } else {
                        Ev::Delight
                    },
                ));
            }
            _ => {}
        }
        from = end;
    }
    out.retain(|&(t, _)| t < total_ms());
    out.sort_by_key(|&(t, _)| t);
    out
}

/// One frame's drawn sprites and whether the glide was on glass.
struct Shot {
    ms: u32,
    sprites: Vec<FreeSprite>,
    glide: bool,
}

/// Fly the script at `cell_h` on `page` (its ground and ink) and keep every frame's sprites.
fn fly(cell_h: u16, page: (u32, u32)) -> Vec<Shot> {
    let t0 = Instant::now();
    let geom = EffectGeom {
        cell_w: (cell_h / 2).max(1),
        cell_h,
        rows: ROWS,
        cols: COLS,
    };
    let mut cat = CursorCat::default();
    cat.set_body(Some(PetSpecies::Cat));
    cat.set_look(KittyLook::DEFAULT);
    let mut wd = WordDecorations::default();
    let key = CatColorKey::from_rgb(page.0, page.1, page.0);
    let events = script();
    let mut next = 0;
    let sing = PHASES.iter().position(|&(n, _)| n == "sing").unwrap_or(0);
    let mut shots = Vec::new();
    let mut missed = Vec::new();
    let mut ms = 0;
    let mut caret = CARET;
    while ms < total_ms() {
        let now = t0 + Duration::from_millis(u64::from(ms));
        while next < events.len() && events[next].0 <= ms {
            let at = t0 + Duration::from_millis(u64::from(events[next].0));
            match events[next].1 {
                Ev::Key => cat.on_key(at, true),
                Ev::Type => {
                    cat.on_key(at, true);
                    caret.1 += 1;
                }
                Ev::Delete => cat.on_key(at, false),
                Ev::Enter => cat.on_enter(at),
                Ev::Curse if !cat.on_curse(at, 1) => missed.push(("wince", ms)),
                Ev::Delight if !cat.on_delight(at, 1) => missed.push(("delight", ms)),
                Ev::Curse | Ev::Delight => {}
            }
            next += 1;
        }
        let (phase, from) = phase_at(ms);
        let sync = if phase == sing {
            SingSync {
                drive: 1.0,
                beat: (ms - from) as f32 / 1000.0 * SING_BPM / 60.0,
            }
        } else {
            SingSync::NONE
        };
        cat.set_singing(now, sync);
        let mut frame = cat.frame(now);
        if matches!(frame.body, CursorBody::Glide(_)) {
            frame.bob = glide_bob_cells(frame.bob, cell_h);
        }
        let mut sprites = Vec::new();
        if frame.alpha > 0 {
            wd.begin_host_frame();
            let layout = KittyCursorLayout {
                geom,
                cursor: caret,
                look: frame.render_look(),
                body: frame.body,
                bob: frame.bob,
                lift: 0.0,
            };
            let placement = wd
                .prepare_kitty_cursor_placement(layout, cat.placement_frame(now), frame.pose)
                .unwrap_or_else(|| panic!("the flier places at {ms} ms"));
            let colors = cat
                .episode_colors()
                .unwrap_or_else(|| cat.colors_for_episode(key));
            let drawn = KittyCursorFrame {
                geom,
                cursor: caret,
                look: frame.render_look(),
                body: frame.body,
                colors,
                bob: frame.bob,
                lift: 0.0,
                alpha: frame.alpha,
                pose: frame.pose,
                sing: frame.sing,
                // The song's notes are kitty_sing's, the head's and the glide's alike: the
                // film's flier draws none either.
                notes: [None; aterm_effects::kitty_sing::MAX_NOTES],
            };
            wd.kitty_cursor_at_placement(drawn, placement, &mut sprites);
        }
        shots.push(Shot {
            ms,
            glide: frame.alpha > 0 && matches!(frame.body, CursorBody::Glide(_)),
            sprites,
        });
        ms += STEP_MS;
    }
    assert!(
        missed.is_empty(),
        "the flier was not on glass to react: {missed:?}"
    );
    shots
}

/// An sRGB channel in linear light, by the exact curve.
fn lin(c: u8) -> f32 {
    let s = f32::from(c) / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// `rgb` (0xRRGGBB) in linear light.
fn lin_rgb(rgb: u32) -> [f32; 3] {
    [
        lin((rgb >> 16) as u8),
        lin((rgb >> 8) as u8),
        lin(rgb as u8),
    ]
}

/// One sprite's art, premultiplied in linear light, `w x h`.
struct Art {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Art {
    fn of(sprite: &FreeSprite) -> Self {
        let (w, h) = (usize::from(sprite.art.w()), usize::from(sprite.art.h()));
        let tint = lin_rgb(sprite.tint);
        let data = sprite.art.data();
        let px = (0..w * h)
            .map(|i| {
                let t = &data[i * 4..i * 4 + 4];
                let a = f32::from(t[3]) / 255.0 * f32::from(sprite.alpha) / 255.0;
                [
                    lin(t[0]) * tint[0] * a,
                    lin(t[1]) * tint[1] * a,
                    lin(t[2]) * tint[2] * a,
                    a,
                ]
            })
            .collect();
        Self { w, h, px }
    }

    fn at(&self, x: i64, y: i64) -> [f32; 4] {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return [0.0; 4];
        }
        self.px[y as usize * self.w + x as usize]
    }

    /// Bilinear, transparent past the edge.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let (x0, y0) = (u.floor(), v.floor());
        let (fx, fy) = (u - x0, v - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let mut out = [0.0; 4];
        for (dx, dy, k) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let t = self.at(x0 + dx, y0 + dy);
            for c in 0..4 {
                out[c] += t[c] * k;
            }
        }
        out
    }
}

/// The union of every sprite's rect over the run, in grid pixels (x0, y0, x1, y1).
fn reach(runs: &[&[Shot]]) -> (i32, i32, i32, i32) {
    let mut r = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for shots in runs {
        for s in shots.iter().flat_map(|s| &s.sprites) {
            r = (
                r.0.min(s.x),
                r.1.min(s.y),
                r.2.max(s.x + i32::from(s.w)),
                r.3.max(s.y + i32::from(s.h)),
            );
        }
    }
    r
}

/// The flash law's reading of `shots` on a page of `bg`, over the whole run and phase by
/// phase.
struct Reading {
    whole: PixelRead,
    phases: Vec<PixelRead>,
    glide_frames: Vec<usize>,
}

fn read(shots: &[Shot], bg: u32) -> Reading {
    let (x0, y0, x1, y1) = reach(&[shots]);
    assert!(x1 > x0 && y1 > y0, "the flier drew nothing");
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let bg = lin_rgb(bg);
    let mut whole = Turns::new(w * h, FLASH_SWING, FLASH_DARK_BELOW);
    let mut phases: Vec<Turns> = PHASES
        .iter()
        .map(|_| Turns::new(w * h, FLASH_SWING, FLASH_DARK_BELOW))
        .collect();
    let mut glide_frames = vec![0; PHASES.len()];
    let mut canvas = vec![[0.0_f32; 3]; w * h];
    for shot in shots {
        canvas.fill(bg);
        for s in &shot.sprites {
            let art = Art::of(s);
            let (kx, ky) = (art.w as f32 / f32::from(s.w), art.h as f32 / f32::from(s.h));
            for py in s.y.max(y0)..(s.y + i32::from(s.h)).min(y1) {
                for px in s.x.max(x0)..(s.x + i32::from(s.w)).min(x1) {
                    if px < s.clip.x0 || px >= s.clip.x1 || py < s.clip.y0 || py >= s.clip.y1 {
                        continue;
                    }
                    let mut u = (px - s.x) as f32 + 0.5;
                    if s.flip_x {
                        u = f32::from(s.w) - u;
                    }
                    let v = (py - s.y) as f32 + 0.5;
                    let t = art.sample(u * kx - 0.5, v * ky - 0.5);
                    let i = (py - y0) as usize * w + (px - x0) as usize;
                    let c = &mut canvas[i];
                    for k in 0..3 {
                        c[k] = t[k] + c[k] * (1.0 - t[3]);
                    }
                }
            }
        }
        let (phase, _) = phase_at(shot.ms);
        glide_frames[phase] += usize::from(shot.glide);
        for (i, c) in canvas.iter().enumerate() {
            let y = f64::from(0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]);
            whole.see(i, y, shot.ms);
            phases[phase].see(i, y, shot.ms);
        }
    }
    Reading {
        whole: whole.read(STEP_MS),
        phases: phases.iter().map(|t| t.read(STEP_MS)).collect(),
        glide_frames,
    }
}

fn report(label: &str, r: &Reading) -> String {
    let mut out = format!(
        "{label}: over={} most={} turns={} worst={:?}",
        r.whole.over.len(),
        r.whole.most,
        r.whole.turns,
        r.whole.worst.as_ref().map(|w| (w.0, &w.2))
    );
    for (i, p) in r.phases.iter().enumerate() {
        out += &format!(
            " | {} over={} turns={} frames={} worst={:?}",
            PHASES[i].0,
            p.over.len(),
            p.turns,
            r.glide_frames[i],
            p.worst.as_ref().map(|w| (w.0, &w.2))
        );
    }
    out
}

/// The flier's own animation keeps the flash law on a white page and a dark one, at a 26 px and
/// a 20 px cell, in every phase of a script that types, sings, presses Enter, deletes in bursts,
/// winces, delights, swaps faces in a flurry, hovers and then runs the caret along its row at
/// 20 keys a second: no pixel makes more than six turns in any second. After the summon, every
/// phase has the glide on glass for most of its frames, so none is read vacuously.
///
/// Controls, each in the same run: with the glide's calm off (the head's three-level stride bob,
/// squash and stretch, lean and recoil, and its every-beat dance), with the face latch off (a
/// face follows every reaction at once), with the follower easing the glide after its rest as it
/// eases the head, and with the glide moving a cell a key instead of its two-cell stride, some
/// pixel turns more than six times a second.
#[test]
fn the_flier_never_flashes() {
    for cell_h in [26_u16, 20] {
        for &(name, bg, fg) in &PAGES {
            let shots = fly(cell_h, (bg, fg));
            let r = read(&shots, bg);
            let line = report(&format!("{name} {cell_h}px"), &r);
            eprintln!("{line}");
            for (i, &n) in r.glide_frames.iter().enumerate().skip(1) {
                assert!(
                    n * 2 >= (PHASES[i].1 / STEP_MS) as usize,
                    "{}: the glide must be on glass for most of the phase: {line}",
                    PHASES[i].0
                );
            }
            assert!(
                r.whole.over.is_empty(),
                "{} px turn more than {} times a second: {line}",
                r.whole.over.len(),
                2 * FLASH_MOST
            );
        }
    }

    let control = |off: &'static std::thread::LocalKey<std::cell::Cell<bool>>, label: &str| {
        off.with(|c| c.set(true));
        let shots = fly(26, (PAGES[0].1, PAGES[0].2));
        off.with(|c| c.set(false));
        let r = read(&shots, PAGES[0].1);
        let line = report(label, &r);
        eprintln!("{line}");
        assert!(
            !r.whole.over.is_empty(),
            "control: {label} must turn some pixel more than six times a second: {line}"
        );
    };
    control(&FLIER_CALM_OFF, "calm off");
    control(&FACE_LATCH_OFF, "latch off");
    control(&GLIDE_FOLLOWS, "follower on");
    control(&GLIDE_STRIDE_OFF, "a cell a key");
}
