// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-film` tests: each law on a synthetic film that breaks it and one that keeps it, the counts the readings
//! are made of, and a film folder read end to end.

use super::*;
use crate::trail_contrast::Rgb;

const WHITE: Rgb = [255, 255, 255];
const PAGE: Rgb = [0x0d, 0x11, 0x17];
const INK: Rgb = [0xe6, 0xed, 0xf3];
const CARET: Rgb = [0xff, 0xcc, 0x00];

/// The films' grid: 6x10 cells from (2, 1), 3 rows of 8, in a 52x32 frame.
const GRID: Grid = Grid {
    ox: 2,
    oy: 1,
    cw: 6,
    ch: 10,
    rows: 3,
    cols: 8,
    source: "dims",
};
const W: usize = 52;
const H: usize = 32;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn paint(px: &mut [Rgb], b: PxBox, colour: Rgb) {
    for y in b.y0..b.y1 {
        for x in b.x0..b.x1 {
            px[usize::try_from(y).unwrap() * W + usize::try_from(x).unwrap()] = colour;
        }
    }
}

/// A film in memory: `frames` 16 ms apart on a `page`, the pixel (5, 5) `lit` on the frames `on` picks.
fn strobe(page: Rgb, lit: Rgb, frames: u64, on: impl Fn(u64) -> bool) -> Flicker {
    let mut f = Flicker::new(W, H);
    for k in 1..=frames {
        let mut px = vec![page; W * H];
        if on(k) {
            px[5 * W + 5] = lit;
        }
        f.frame(16 * k, &Image::from_pixels(W, H, px), None)
            .expect("a frame of the film's size");
    }
    f
}

/// A reading with nothing in it but `flash`.
fn flashing(flash: PixelRead) -> Reading {
    Reading {
        width: W,
        grid: Some(GRID),
        flash,
        ..Reading::default()
    }
}

#[test]
fn four_flashes_a_second_break_the_law_and_two_keep_it() {
    // Inked for 8 frames and bare for 8 is a flash every 256 ms: 8 turns, 4 flashes, in a second.
    let four = strobe(WHITE, [40, 40, 40], 125, |k| (k / 8) % 2 == 1)
        .law
        .read(16);
    assert_eq!(
        (four.most, four.over.as_slice()),
        (4, &[5 * W as u32 + 5][..])
    );
    let (px, count, turns) = four.worst.clone().expect("a worst pixel");
    assert_eq!((px as usize, count, turns.len()), (5 * W + 5, 8, 8));
    assert!(
        turns.windows(2).all(|t| t[1] - t[0] == 128),
        "the turns are the strobe's: {turns:?}"
    );
    let broken = flashing(four).broken();
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert_eq!(
        broken[0],
        "flash: 1 px flash faster than 3 a second (over 6 turns in one), the worst 8 turns"
    );
    // Negative control's other side: the same strobe at half the rate is 2 flashes a second, under the law.
    let two = strobe(WHITE, [40, 40, 40], 125, |k| (k / 16) % 2 == 1)
        .law
        .read(16);
    assert_eq!((two.most, two.over.len()), (2, 0));
    assert!(flashing(two).broken().is_empty());
}

/// The pixel (5, 5) of a white page inked on the frames `on` picks, 4 s of 16 ms frames, read by the law.
fn steady(on: impl Fn(u64) -> bool) -> PixelRead {
    strobe(WHITE, [40, 40, 40], 250, on).law.read(16)
}

#[test]
fn a_steady_strobe_between_three_and_four_hertz_breaks_the_law() {
    // A turn every 144 ms is 3.47 Hz: seven turns in 864 ms. They are three whole flashes and the fourth
    // begun, so `most` is still 3, and the pixel is over the bound all the same.
    let fast = steady(|k| (k / 9) % 2 == 1);
    assert_eq!((fast.most, fast.turns, fast.over.len()), (3, 7, 1));
    let broken = flashing(fast).broken();
    assert_eq!(
        broken,
        ["flash: 1 px flash faster than 3 a second (over 6 turns in one), the worst 7 turns"]
    );
    // Every 160 ms, 3.12 Hz: seven turns in 960 ms.
    let over = steady(|k| (k / 10) % 2 == 1);
    assert_eq!((over.most, over.turns, over.over.len()), (3, 7, 1));
    // The control: every 176 ms is 2.84 Hz, six turns in any second, and keeps the law.
    let under = steady(|k| (k / 11) % 2 == 1);
    assert_eq!((under.most, under.turns, under.over.len()), (3, 6, 0));
    assert!(flashing(under).broken().is_empty());
}

#[test]
fn a_light_at_three_hertz_keeps_the_law_at_every_phase_of_the_frame_clock() {
    // A turn every 166.7 ms, seen on the next 16 ms frame. At some phases seven turns are read 992 ms apart,
    // a frame step under the second they took: the bound's second is a step short so that is not over.
    let mut sevens = 0;
    for phase in 0..16u64 {
        let read = steady(|k| ((16 * k + phase) * 6 / 1_000) % 2 == 1);
        assert_eq!((read.most, read.over.len()), (3, 0), "phase {phase} ms");
        sevens += usize::from(read.turns == 7);
    }
    assert!(sevens > 0, "no phase read seven turns in a second");
    // The control: 3.1 Hz (a turn every 161.3 ms) is over at every phase.
    for phase in 0..16u64 {
        let read = steady(|k| ((16 * k + phase) * 62 / 10_000) % 2 == 1);
        assert_eq!(read.over.len(), 1, "phase {phase} ms");
    }
    // Read with no step taken off the second, the 3.0 Hz light is called over at the phases that read seven.
    let bare = strobe(WHITE, [40, 40, 40], 250, |k| {
        ((16 * k + 8) * 6 / 1_000) % 2 == 1
    })
    .law
    .read(0);
    assert_eq!((bare.turns, bare.over.len()), (7, 1));
}

/// The law's turns of one pixel that reads `values` in turn.
fn turns_of(values: &[f64]) -> usize {
    let mut t = Turns::new(1, FLASH_SWING, FLASH_DARK_BELOW);
    for (k, &v) in (1u32..).zip(values) {
        t.see(0, v, 16 * k);
    }
    t.at.get(&0).map_or(0, Vec::len)
}

#[test]
fn a_swing_of_a_tenth_exactly_is_a_flash_and_a_darker_state_at_0_80_exactly_is_none() {
    // The swing's edge: 0.10 is a turn (first change, fall, rise), a hair under it is none.
    assert_eq!(turns_of(&[0.0, FLASH_SWING, 0.0, FLASH_SWING]), 3);
    assert_eq!(turns_of(&[0.0, 0.0999, 0.0, 0.0999]), 0);
    // The darker state's edge: at 0.80 exactly it is not below 0.80, so no change to it or from it is a
    // turn; a hair under is. First changes, then a fall after a turn.
    assert_eq!(turns_of(&[1.0, FLASH_DARK_BELOW, 1.0, FLASH_DARK_BELOW]), 0);
    assert_eq!(turns_of(&[1.0, 0.7999, 1.0, 0.7999]), 3);
    assert_eq!(
        turns_of(&[0.5, 1.0, FLASH_DARK_BELOW, 1.0, FLASH_DARK_BELOW]),
        1
    );
    assert_eq!(turns_of(&[0.5, 1.0, 0.7999, 1.0, 0.7999]), 4);
}

#[test]
fn a_fall_is_measured_from_the_highest_value_since_the_last_turn() {
    // On a light page a pixel steps down to 0.85 (no turn: its darker state is not under 0.80), wobbles up
    // to 0.88 and comes to 0.79. That is one fall of 0.21 from the 1.0 it stood at, not a last step of 0.09.
    assert_eq!(turns_of(&[1.0, 0.85, 0.88, 0.79]), 1);
    assert_eq!(turns_of(&[0.5, 1.0, 0.85, 0.88, 0.79]), 2);
    // The control: the same steps ending at 0.80 fall nowhere under it, and are no turn.
    assert_eq!(turns_of(&[1.0, 0.85, 0.88, 0.80]), 0);
}

#[test]
fn a_flash_is_wcag_s_pair_of_opposing_changes() {
    let strobed = |page, lit| strobe(page, lit, 125, |k| (k / 8) % 2 == 1);
    // A swing with the darker state at 0.80 or over is no flash: white to #EDEDED is 1.0 to 0.84.
    assert_eq!(strobed(WHITE, [237, 237, 237]).law.read(16).most, 0);
    // A swing under a tenth is none (#404040 to #646464 is 0.05 to 0.13), and one over it is (to #808080, 0.22).
    assert_eq!(strobed([64, 64, 64], [100, 100, 100]).law.read(16).most, 0);
    assert_eq!(strobed([64, 64, 64], [128, 128, 128]).law.read(16).most, 4);
    // One slow fall and rise, white to #AFAFAF (0.43) across 20 frames each way, is one flash however many
    // frames it takes.
    let mut f = Flicker::new(W, H);
    for k in 1..=40u64 {
        let step = if k <= 20 { k } else { 40 - k };
        let v = 255 - u8::try_from(step * 4).unwrap();
        let px = vec![[v, v, v]; W * H];
        f.frame(16 * k, &Image::from_pixels(W, H, px), None)
            .expect("a frame");
    }
    let slow = f.law.read(16);
    assert_eq!((slow.most, slow.over.len()), (1, 0));
}

#[test]
fn lightness_reads_a_dark_page_s_flicker_the_law_cannot() {
    // The dark theme's page lit to a dim grey and back, 4 times a second: 0.006 to 0.03 of linear light is no
    // tenth, so the law reads nothing, and it is 5 to 20 L*.
    let f = strobe(PAGE, [0x30, 0x30, 0x30], 125, |k| (k / 8) % 2 == 1);
    assert_eq!(f.law.read(16).most, 0);
    assert_eq!(f.light.read(16).most, 4);
    assert!((lightness(1.0) - 1.0).abs() < 1e-9 && lightness(0.0) == 0.0);
    assert!((lightness(0.184) - 0.5).abs() < 0.002, "mid grey is L* 50");
}

#[test]
fn the_busiest_second_is_half_open() {
    assert_eq!(most_in_a_second(&[]), (0, 0));
    assert_eq!(most_in_a_second(&[0, 500, 999, 1_000, 1_400]), (4, 1));
    assert_eq!(most_in_a_second(&[0, 1_000, 2_000]), (1, 0));
}

#[test]
fn the_twin_count_reads_the_caret_s_rows_but_not_its_cell() {
    // The caret at (1, 3). A pixel in each of: a cell of its row, its own cell, and nothing else lit.
    let clean = Image::from_pixels(W, H, vec![PAGE; W * H]);
    let row_cell = GRID.cell_box(1, 1, 1);
    let caret_cell = GRID.cell_box(1, 3, 1);
    let at = |b: PxBox| usize::try_from(b.y0 * W as i64 + b.x0).unwrap();
    let mut f = Flicker::new(W, H);
    for k in 1..=125u64 {
        let mut px = vec![PAGE; W * H];
        // Lit for 4 frames, out for 4: 8 rises a second.
        if (k / 4) % 2 == 1 {
            px[at(row_cell)] = WHITE;
            px[at(caret_cell)] = WHITE;
        }
        f.frame(
            16 * k,
            &Image::from_pixels(W, H, px),
            Some((&clean, &GRID, (1, 3))),
        )
        .expect("a frame");
    }
    let twin = f.twin.read();
    assert_eq!(twin.most, 8);
    assert_eq!(twin.over, vec![u32::try_from(at(row_cell)).unwrap()]);
    // The law reads the whole frame, the caret's cell too.
    assert_eq!(f.law.read(16).over.len(), 2);
}

/// `n` shots 16 ms apart from `from`, each made by `shot` from its index.
fn shots(from: u64, n: u64, shot: impl Fn(u64) -> PetShot) -> Vec<PetShot> {
    (0..n)
        .map(|k| PetShot {
            ms: from + 16 * k,
            ..shot(k)
        })
        .collect()
}

fn sitting() -> PetShot {
    PetShot {
        alpha: 255,
        pose: "sit".into(),
        ..PetShot::default()
    }
}

fn petted(pet: PetRead) -> Vec<String> {
    Reading {
        pet,
        ..Reading::default()
    }
    .broken()
}

#[test]
fn a_pet_on_a_letter_breaks_the_law() {
    let clear = judge_pet(&shots(16, 20, |_| sitting()));
    assert_eq!(
        (clear.on_glass, clear.on_letters, clear.on_caret),
        (20, 0, 0)
    );
    assert!(petted(clear).is_empty());
    // Negative control: frames 5 to 7 with the body on a lettered cell, frame 9 on the caret's.
    let over = judge_pet(&shots(16, 20, |k| PetShot {
        letters: usize::from((5..8).contains(&k)),
        on_caret: k == 9,
        ..sitting()
    }));
    assert_eq!((over.on_letters, over.on_caret), (3, 1));
    assert_eq!(over.first_over, Some(16 + 16 * 5));
    let broken = petted(over);
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(
        broken[0].starts_with("pet: on a lettered cell in 3 frames and on the caret's cell in 1")
    );
    // A frame whose letters were not read is never clean.
    let unread = judge_pet(&shots(16, 3, |k| PetShot {
        unread: k == 1,
        ..sitting()
    }));
    assert!(petted(unread)[0].starts_with("pet: 1 frames whose letters or caret were not read"));
}

#[test]
fn a_pose_shown_for_50_ms_breaks_the_law() {
    let posed = |frames: u64| {
        judge_pet(&shots(16, 30, |k| PetShot {
            pose: if (10..10 + frames).contains(&k) {
                "ear-flick".into()
            } else {
                "sit".into()
            },
            ..sitting()
        }))
    };
    // Negative control: three frames, 48 ms.
    let short = posed(3);
    assert_eq!(short.short_poses, 1);
    assert_eq!(
        short.min_dwell,
        Some((48, "ear-flick".to_string(), 16 + 160))
    );
    assert!(
        petted(short)[0]
            .starts_with("pet: 1 poses shown under 100 ms, the shortest ear-flick for 48 ms")
    );
    // Seven frames, 112 ms, is a pose. The poses the film's first and last frames cut are not timed.
    let long = posed(7);
    assert_eq!(
        (long.short_poses, long.min_dwell.as_ref().map(|m| m.0)),
        (0, Some(112))
    );
    assert!(petted(long).is_empty());
    let cut = judge_pet(&shots(16, 4, |k| PetShot {
        pose: if k < 2 { "sit".into() } else { "stand".into() },
        ..sitting()
    }));
    assert_eq!((cut.short_poses, cut.min_dwell), (0, None));
}

#[test]
fn a_dip_of_the_pet_s_alpha_that_returns_is_a_blink() {
    let dipped = |edge: bool| {
        judge_pet(&shots(16, 30, |k| PetShot {
            alpha: if (10..14).contains(&k) { 0 } else { 255 },
            at_edge: edge,
            ..sitting()
        }))
    };
    // Negative control: gone for four frames in the middle of the grid and back.
    let blink = dipped(false);
    assert_eq!(blink.blinks, vec![(16 + 16 * 9, 16 + 16 * 14, 0)]);
    assert!(
        petted(blink)
            .iter()
            .any(|b| b.starts_with("pet: its alpha dipped and came back 1 times"))
    );
    // Off through the grid's edge and on again through it is a walk, not a blink.
    assert!(dipped(true).blinks.is_empty());
    // A fade in, a hold and a fade out with no return is none; nor is a wobble under a tenth.
    let faded = judge_pet(&shots(16, 30, |k| PetShot {
        alpha: match k {
            0..5 => u8::try_from(k * 50).unwrap(),
            5..20 => 250 - u8::try_from(k % 2 * 20).unwrap(),
            _ => u8::try_from((30 - k) * 25).unwrap(),
        },
        ..sitting()
    }));
    assert!(faded.blinks.is_empty(), "{:?}", faded.blinks);
    // A dip that stays on glass is one too.
    let dimmed = judge_pet(&shots(16, 30, |k| PetShot {
        alpha: if (10..14).contains(&k) { 120 } else { 255 },
        ..sitting()
    }));
    assert_eq!(dimmed.blinks.len(), 1);
    assert_eq!(dimmed.blinks[0].2, 120);
}

#[test]
fn a_flash_is_the_pet_s_the_caret_s_or_the_trail_s_by_where_it_lay() {
    // Three pixels of a row, each turning every 64 ms. A pet's box holds the first on every frame and the
    // second on the first three of its sixteen turns; the caret's cell holds the third.
    let times: Vec<u32> = (1..=16).map(|k| 64 * k).collect();
    let turns: HashMap<u32, Vec<u32>> = [10, 20, 30]
        .into_iter()
        .map(|px| (px, times.clone()))
        .collect();
    let boxes: HashMap<u32, Boxes> = times
        .iter()
        .map(|&t| {
            let pet = if t <= 192 {
                (8, 22, 0, 1)
            } else {
                (8, 12, 0, 1)
            };
            let caret = PxBox {
                x0: 28,
                y0: 0,
                x1: 34,
                y1: 1,
            };
            (t, (vec![pet], Some(caret), vec![]))
        })
        .collect();
    let [pet, caret, icon, page, trail] = by_place(&turns, (2, 16), W, &boxes, &HashMap::new());
    assert_eq!(page, PixelRead::default());
    assert_eq!(icon, PixelRead::default());
    assert_eq!((pet.most, pet.over.as_slice()), (8, &[10][..]));
    assert_eq!((caret.most, caret.over.as_slice()), (8, &[30][..]));
    assert_eq!((trail.most, trail.over.as_slice()), (8, &[20][..]));
}

#[test]
fn a_turn_on_the_frame_a_body_left_its_pixel_is_the_body_s() {
    // Seven turns 144 ms apart; the pet's box holds the pixel on the frames of the first three. The fourth
    // falls on the frame after the box last held it (`gone` ms before), when its coat gave way to the page.
    let times: Vec<u32> = (1..=7).map(|k| 144 * k).collect();
    let read = |gone: u32| {
        let turns: HashMap<u32, Vec<u32>> = [(10, times.clone())].into_iter().collect();
        let mut boxes: HashMap<u32, Boxes> = times
            .iter()
            .enumerate()
            .map(|(k, &t)| {
                let body = if k < 3 { vec![(8, 12, 0, 1)] } else { vec![] };
                (t, (body, None, vec![]))
            })
            .collect();
        for was in [times[3] - 32, times[3] - 16] {
            let body = if was == times[3] - gone {
                vec![(8, 12, 0, 1)]
            } else {
                vec![]
            };
            boxes.insert(was, (body, None, vec![]));
        }
        by_place(&turns, (2, 16), W, &boxes, &HashMap::new()).map(|read| read.over)
    };
    let (none, this) = (Vec::<u32>::new, || vec![10u32]);
    // Gone the frame before the turn: four of seven are the pet's.
    assert_eq!(read(16), [this(), none(), none(), none(), none()]);
    // Control: gone two frames before, the turn is not the box's leaving, and three of seven stay the trail's.
    assert_eq!(read(32), [none(), none(), none(), none(), this()]);
}

#[test]
fn a_pixel_the_pet_and_the_caret_shared_is_not_the_trail_s_own() {
    // Seven turns 144 ms apart. `pet` of them fall in the pet's box, the next `caret` in the caret's cell and
    // the next `icon` in a word icon's box.
    let times: Vec<u32> = (1..=7).map(|k| 144 * k).collect();
    let split = |pet: usize, caret: usize, icon: usize| {
        let turns: HashMap<u32, Vec<u32>> = [(10, times.clone())].into_iter().collect();
        let boxes: HashMap<u32, Boxes> = times
            .iter()
            .enumerate()
            .map(|(k, &t)| {
                let body = if k < pet { vec![(8, 12, 0, 1)] } else { vec![] };
                let cell = (pet..pet + caret).contains(&k).then_some(PxBox {
                    x0: 8,
                    y0: 0,
                    x1: 12,
                    y1: 1,
                });
                let mark = if (pet + caret..pet + caret + icon).contains(&k) {
                    vec![(8, 12, 0, 1)]
                } else {
                    vec![]
                };
                (t, (body, cell, mark))
            })
            .collect();
        by_place(&turns, (2, 16), W, &boxes, &HashMap::new()).map(|read| read.over)
    };
    let (none, this) = (Vec::<u32>::new, || vec![10u32]);
    // Three in the pet's box and two in the caret's cell: neither holds half of seven, the two together
    // do, and the pixel goes to the one that held more.
    assert_eq!(split(3, 2, 0), [this(), none(), none(), none(), none()]);
    assert_eq!(split(2, 3, 0), [none(), this(), none(), none(), none()]);
    // A word icon's box is a place too: the icon's pop is not the trail's flash.
    assert_eq!(split(0, 1, 6), [none(), none(), this(), none(), none()]);
    assert_eq!(split(2, 2, 2), [this(), none(), none(), none(), none()]);
    // The control: two and one are three of seven, under half, and the pixel is the trail's own.
    assert_eq!(split(2, 1, 0), [none(), none(), none(), none(), this()]);
    assert_eq!(split(1, 1, 1), [none(), none(), none(), none(), this()]);
}

#[test]
fn a_pixel_that_turns_with_the_page_is_the_page_s_own() {
    // Seven turns 144 ms apart, a build log's line scrolling over the pixel. The page's clean twin turned there
    // on the same frames, or a frame step off (a turn is seen on the frame after it): the page alone is over the
    // bound and nothing is left of the pixel's turns once each of the page's has taken one, so it is the page's
    // own and no effect's flash.
    let times: Vec<u32> = (1..=7).map(|k| 144 * k).collect();
    let read = |turns: Vec<u32>, page: Vec<u32>| {
        let turns: HashMap<u32, Vec<u32>> = [(10, turns)].into_iter().collect();
        let page: HashMap<u32, Vec<u32>> = [(10, page)].into_iter().collect();
        by_place(&turns, (2, 16), W, &HashMap::new(), &page).map(|read| read.over)
    };
    let (none, this) = (Vec::<u32>::new, || vec![10u32]);
    let (paged, trail) = (
        [none(), none(), none(), this(), none()],
        [none(), none(), none(), none(), this()],
    );
    assert_eq!(read(times.clone(), times.clone()), paged);
    let late: Vec<u32> = times.iter().map(|t| t + 16).collect();
    assert_eq!(read(times.clone(), late), paged);
    // The controls: the page turned there on three of the seven, or two frames off: the trail's.
    assert_eq!(read(times.clone(), times[..3].to_vec()), trail);
    let off: Vec<u32> = times.iter().map(|t| t + 32).collect();
    assert_eq!(read(times.clone(), off), trail);
}

#[test]
fn the_page_excuses_no_flash_the_effects_carry_over_the_bound() {
    let (none, this) = (Vec::<u32>::new, || vec![10u32]);
    let read = |turns: Vec<u32>, page: Vec<u32>, caret: bool| {
        let boxes: HashMap<u32, Boxes> = turns
            .iter()
            .map(|&t| {
                let cell = caret.then_some(PxBox {
                    x0: 8,
                    y0: 0,
                    x1: 12,
                    y1: 1,
                });
                (t, (vec![], cell, vec![]))
            })
            .collect();
        let turns: HashMap<u32, Vec<u32>> = [(10, turns)].into_iter().collect();
        let page: HashMap<u32, Vec<u32>> = [(10, page)].into_iter().collect();
        by_place(&turns, (2, 16), W, &boxes, &page).map(|read| read.over)
    };
    let trail = [none(), none(), none(), none(), this()];
    // Eight turns 120 ms apart, four of them the page's: the page alone (two flashes) is under the bound, and
    // the effects' four carry the pixel over it. Half the turns fell with the page's, which once made it the
    // page's.
    let eight: Vec<u32> = (1..=8).map(|k| 120 * k).collect();
    assert_eq!(read(eight.clone(), eight[..4].to_vec(), false), trail);
    // The page over the bound alone, and the effects turning between each two of its turns: what is left once
    // the page's turns are taken (seven) is over the bound too.
    let page: Vec<u32> = (1..=7).map(|k| 144 * k).collect();
    let mut both: Vec<u32> = page.iter().flat_map(|&t| [t, t + 64]).collect();
    both.sort_unstable();
    assert_eq!(read(both, page.clone(), false), trail);
    // One turn of the page excuses one: a pixel that turns a frame before, on and a frame after each of the
    // page's seven keeps fourteen of its own.
    let blips: Vec<u32> = page.iter().flat_map(|&t| [t - 16, t, t + 16]).collect();
    assert_eq!(read(blips, page.clone(), false), trail);
    // The page over the bound at one moment excuses no other: the same eight turns at 5 s, after seven of the
    // page's alone at the pixel in its first second, are the trail's. Read over the whole film, the page was
    // over the bound and what was left of the pixel's turns was not, which once made them the page's.
    let late = || page.iter().copied().chain(eight.iter().map(|t| t + 5_000));
    let paged = page
        .iter()
        .copied()
        .chain(eight[..4].iter().map(|t| t + 5_000));
    assert_eq!(read(late().collect(), paged.collect(), false), trail);
    // Nor in the second before its own: a turn of the effects 130 ms before the page's seven makes a second of
    // seven turns, six of them the page's, though the page alone is over the bound a second later.
    let led: Vec<u32> = std::iter::once(14).chain(page.iter().copied()).collect();
    assert_eq!(read(led, page.clone(), false), trail);
    // Nor at its edge: the effects' turn among the page's seven leaves a second of six of the page's and itself.
    let mut among: Vec<u32> = std::iter::once(500).chain(page.iter().copied()).collect();
    among.sort_unstable();
    assert_eq!(read(among, page.clone(), false), trail);
    // The control: an effects' turn amid a page that turns ten times a second for three, every second holding it
    // over the bound by the page alone, is excused with it.
    let long: Vec<u32> = (1..=30).map(|k| 100 * k).collect();
    let mut amid: Vec<u32> = std::iter::once(1_550).chain(long.iter().copied()).collect();
    amid.sort_unstable();
    assert_eq!(
        read(amid, long, false),
        [none(), none(), none(), this(), none()]
    );
    // The boxes come first: the clean twin holds the caret, so the caret's cell turning with the page is the
    // caret's, not the page's.
    assert_eq!(
        read(page.clone(), page.clone(), true),
        [none(), this(), none(), none(), none()]
    );
    // The control: the same pixel out of the caret's cell is the page's.
    assert_eq!(
        read(page.clone(), page, false),
        [none(), none(), none(), this(), none()]
    );
}

#[test]
fn a_letter_typed_onto_the_page_is_read_as_the_page_s() {
    // Under the bound the place is a reading, not the law: three turns of a pixel a letter typed onto the page
    // turned, the clean twin turning with it, are the page's; with the page turning once of them, the trail's.
    let times: Vec<u32> = vec![144, 288, 432];
    let read = |page: Vec<u32>| {
        let turns: HashMap<u32, Vec<u32>> = [(10, times.clone())].into_iter().collect();
        let page: HashMap<u32, Vec<u32>> = [(10, page)].into_iter().collect();
        by_place(&turns, (2, 16), W, &HashMap::new(), &page).map(|read| read.turns)
    };
    assert_eq!(read(times.clone()), [0, 0, 0, 3, 0]);
    assert_eq!(read(vec![288]), [0, 0, 0, 0, 3]);
}

#[test]
fn a_page_turn_takes_one_turn_within_a_frame_of_it() {
    // Each of the page's turns takes the earliest turn within a frame step of it that no earlier one took.
    assert_eq!(unmatched(&[100, 116, 132], &[116], 16), vec![116, 132]);
    assert_eq!(unmatched(&[100, 116, 132], &[100, 132], 16), vec![132]);
    assert_eq!(unmatched(&[100, 116], &[84, 116], 16), Vec::<u32>::new());
    assert_eq!(unmatched(&[100, 150], &[116, 134], 16), Vec::<u32>::new());
    assert_eq!(unmatched(&[100], &[133], 16), vec![100]);
}

fn png(px: &[Rgb]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = aterm_png::Encoder::new(
        &mut out,
        u32::try_from(W).unwrap(),
        u32::try_from(H).unwrap(),
    );
    e.set_color(aterm_png::ColorType::Rgb);
    e.set_depth(aterm_png::BitDepth::Eight);
    let mut w = e.write_header().expect("a PNG header");
    let data: Vec<u8> = px.iter().flatten().copied().collect();
    w.write_image_data(&data).expect("the PNG's pixels");
    w.finish().expect("the PNG's end");
    out
}

/// The film's page with no effect drawn: `ab` on row 1 from column 1, each a two-pixel stroke, and a block caret
/// at (1, 3).
fn clean_page() -> Vec<Rgb> {
    let mut px = vec![PAGE; W * H];
    for col in [1, 2] {
        let b = GRID.cell_box(1, col, 1);
        paint(
            &mut px,
            PxBox {
                x0: b.x0 + 2,
                y0: b.y0 + 2,
                x1: b.x0 + 4,
                y1: b.y0 + 8,
            },
            INK,
        );
    }
    paint(&mut px, GRID.cell_box(1, 3, 1), CARET);
    px
}

/// A film of `frames` frames written under a new folder as `aterm-render`'s `trail_film::film` writes one: each
/// frame the clean page as `draw` changes it, with the status row `status` gives it.
fn film(
    frames: u64,
    draw: impl Fn(u64, &mut Vec<Rgb>),
    status: impl Fn(u64) -> String,
) -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let put = |name: String, bytes: &[u8]| {
        std::fs::write(dir.path().join(name), bytes).expect("a film file")
    };
    put(
        "film.dims.txt".into(),
        b"OK 3 8 52 32 cell_w=6 cell_h=10 pad=2 pad_top=1 head=0\n",
    );
    let clean = clean_page();
    for k in 1..=frames {
        let stem = format!("film.{:06}", 16 * k);
        let mut px = clean.clone();
        draw(k, &mut px);
        put(format!("{stem}.png"), &png(&px));
        if k == 1 {
            put(format!("{stem}.clean.png"), &png(&clean));
        } else {
            std::fs::hard_link(
                dir.path().join("film.000016.clean.png"),
                dir.path().join(format!("{stem}.clean.png")),
            )
            .expect("the twin's link");
        }
        put(format!("{stem}.text.txt"), b"\n ab\n\n");
        put(format!("{stem}.cursor.txt"), b"OK 1 3 1 steady_block\n");
        put(format!("{stem}.status.txt"), status(k).as_bytes());
    }
    dir
}

/// The open row's rail as the flagship laid it (film pixels of `jump`, dark and GitHub Light), and the foot band
/// it left on a row with letters under it, dark and GitHub Light.
const RAIL: [Rgb; 4] = [[0, 190, 60], [230, 110, 0], [60, 120, 255], [215, 60, 235]];
const FOOT_BAND: [Rgb; 4] = [[20, 60, 30], [70, 30, 10], [200, 232, 205], [246, 205, 170]];

/// A grid for the line's films: 6x10 cells, 3 rows of 16, from the frame's corner.
const LINE_GRID: Grid = Grid {
    ox: 0,
    oy: 0,
    cw: 6,
    ch: 10,
    rows: 3,
    cols: 16,
    source: "dims",
};

/// A page-coloured frame of [`LINE_GRID`] with `band` painted `(dy0, dy1)` px rows from row 1's bottom under the
/// cells `cols` picks.
fn lined(band: Rgb, (dy0, dy1): (i64, i64), cols: impl Fn(i64) -> bool) -> Image {
    let (w, h) = (96usize, 30usize);
    let mut px = vec![PAGE; w * h];
    for col in (0..LINE_GRID.cols).filter(|&c| cols(c)) {
        let b = LINE_GRID.cell_box(1, col, 1);
        for y in b.y1 + dy0..b.y1 + dy1 {
            for x in b.x0..b.x1 {
                px[usize::try_from(y).unwrap() * w + usize::try_from(x).unwrap()] = band;
            }
        }
    }
    Image::from_pixels(w, h, px)
}

/// A film typed along row 1 of [`LINE_GRID`] from column 2, a key every 48 ms (3 frames) for `keys` keys, then
/// held: each frame is what `draw` gives for the caret's column. The line's reading.
fn typed_film(keys: i64, draw: impl Fn(i64) -> Image) -> LineRead {
    let clean = lined(PAGE, (0, 0), |_| false);
    let mut track = LineTrack::default();
    for k in 0..(3 * keys + 30) {
        let caret = 2 + (k / 3).min(keys);
        let ms = 16 * u64::try_from(k + 1).unwrap();
        track.frame(ms, &draw(caret), &clean, &LINE_GRID, (1, caret));
    }
    track.read
}

#[test]
fn a_rail_s_pixel_is_vivid_and_a_foot_band_s_is_not() {
    for p in RAIL {
        assert!(vivid(p), "{p:?}");
    }
    // The negative control: the dim band's own colours, the page, the ink and a grey.
    for p in FOOT_BAND
        .into_iter()
        .chain([PAGE, INK, WHITE, [128, 128, 128]])
    {
        assert!(!vivid(p), "{p:?}");
    }
    // Chroma alone is not vivid: a saturated colour too dark, or too light, to read as the rail.
    assert!(!vivid([40, 0, 90]) && !vivid([255, 255, 100]));
}

#[test]
fn a_cell_is_lined_by_two_vivid_pixels_under_half_its_columns() {
    let clean = lined(PAGE, (0, 0), |_| false);
    let at = |frame: &Image, col| cell_lined(frame, &clean, &LINE_GRID, (1, col));
    // Inside the row's foot, and under the row where an open row's rail lies.
    assert!(at(&lined(RAIL[0], (-3, -1), |c| c == 4), 4));
    assert!(at(&lined(RAIL[0], (1, 3), |c| c == 4), 4));
    // Not the next cell's, not a hairline, not the dim band, not over the letters' body.
    assert!(!at(&lined(RAIL[0], (-3, -1), |c| c == 4), 5));
    assert!(!at(&lined(RAIL[0], (-2, -1), |c| c == 4), 4));
    assert!(!at(&lined(FOOT_BAND[0], (-3, 0), |c| c == 4), 4));
    assert!(!at(&lined(RAIL[0], (-9, -5), |c| c == 4), 4));
    // Not the page's own: a vivid band the clean twin holds too is no effect's line.
    let own = lined(RAIL[0], (-3, -1), |c| c == 4);
    assert!(!cell_lined(&own, &own, &LINE_GRID, (1, 4)));
    // Under two of a cell's six columns it is a tick, under three a line.
    let mut px = vec![PAGE; 96 * 30];
    let b = LINE_GRID.cell_box(1, 4, 1);
    for x in b.x0..b.x0 + 3 {
        for y in b.y1 - 3..b.y1 - 1 {
            px[usize::try_from(y).unwrap() * 96 + usize::try_from(x).unwrap()] = RAIL[1];
        }
        if x == b.x0 + 1 {
            assert!(!at(&Image::from_pixels(96, 30, px.clone()), 4));
        }
    }
    assert!(at(&Image::from_pixels(96, 30, px), 4));
}

#[test]
fn a_film_s_line_is_the_share_of_typed_cells_with_a_vivid_line_under_them() {
    let under = |caret: i64, on: bool| lined(RAIL[2], (-3, -1), |c| on && (2..caret).contains(&c));
    // A line under every cell the caret has left: every judged frame reads all of them.
    let whole = typed_film(12, |caret| under(caret, true));
    let (lit, cells, _) = whole.full.expect("the run was judged");
    assert_eq!((lit, cells), (12, 12), "the whole line typed: {whole:?}");
    assert_eq!(whole.free, (12, 12));
    let (_, fewest, _) = whole.least.expect("judged");
    assert!(whole.frames > 10 && fewest >= LINE_CELLS, "{whole:?}");
    assert_eq!((whole.share(), whole.least_share()), (Some(1.0), Some(1.0)));
    // The negative control: the same film with the dim foot band, where a held row had it, reads none.
    let dim = typed_film(12, |caret| {
        lined(FOOT_BAND[0], (-3, 0), |c| (2..caret).contains(&c))
    });
    assert_eq!((dim.share(), dim.least_share()), (Some(0.0), Some(0.0)));
    assert_eq!(dim.frames, whole.frames);
    // A line that parts round every other cell reads half.
    let half = typed_film(12, |caret| {
        lined(RAIL[2], (-3, -1), |c| (2..caret).contains(&c) && c % 2 == 0)
    });
    assert_eq!(half.share(), Some(0.5), "{half:?}");
    // A line that goes out at the last key reads none, whatever the frames before held; one that goes out
    // while the hand types and is back by the last key reads whole, with a least frame of none.
    let lost = typed_film(12, |caret| under(caret, caret < 14));
    assert_eq!(lost.share(), Some(0.0), "{lost:?}");
    assert!(lost.lit > 0, "{lost:?}");
    let dipped = typed_film(12, |caret| under(caret, caret != 12));
    assert_eq!(
        (dipped.share(), dipped.least_share()),
        (Some(1.0), Some(0.0))
    );
    // A cell is not judged before it is 96 ms old: a line that trails a typing hand by two cells is whole.
    let trailing = typed_film(12, |caret| {
        under(if caret < 14 { caret - 2 } else { caret }, true)
    });
    assert_eq!(trailing.least_share(), Some(1.0), "{trailing:?}");
    // Fewer than eight cells typed judge nothing, and neither does a jump along the row.
    assert_eq!(typed_film(6, |_| lined(PAGE, (0, 0), |_| false)).frames, 0);
    let clean = lined(PAGE, (0, 0), |_| false);
    let mut jump = LineTrack::default();
    for (k, col) in [2, 14, 14, 14, 14, 14, 14, 14, 14, 14]
        .into_iter()
        .enumerate()
    {
        jump.frame(16 * (k as u64 + 1), &clean, &clean, &LINE_GRID, (1, col));
    }
    assert_eq!(jump.read, LineRead::default());
    // The report's line.
    let line = Reading {
        line: half,
        ..Reading::default()
    }
    .render();
    assert!(
        line.contains("\n  LINE     line_cells_lit=50.0% line_at=lit:6;cells:12;ms:"),
        "{line}"
    );
    assert!(
        line.contains(" line_free_lit=50.0%;lit:6;cells:12 line_least="),
        "{line}"
    );
    assert!(Reading::default().render().contains(
        "line_cells_lit=none line_at=none line_free_lit=none line_least=none line_mean=none line_frames=0"
    ));
}

#[test]
fn a_cell_with_ink_under_the_baseline_is_counted_apart() {
    // Row 1 of the line's grid: a descender's stem in cell 5 (down to the cell's last pixel row), a letter that
    // ends on the baseline in cell 6 (its 8th pixel row of 10).
    let (w, h) = (96usize, 30usize);
    let at = |x: i64, y: i64| usize::try_from(y).unwrap() * w + usize::try_from(x).unwrap();
    let mut px = vec![PAGE; w * h];
    for (col, to) in [(5, 10), (6, 8)] {
        let b = LINE_GRID.cell_box(1, col, 1);
        for y in b.y0 + 2..b.y0 + to {
            px[at(b.x0 + 2, y)] = INK;
        }
    }
    let clean = Image::from_pixels(w, h, px.clone());
    assert!(cell_footed(&clean, &LINE_GRID, (1, 5)));
    assert!(!cell_footed(&clean, &LINE_GRID, (1, 6)));
    assert!(!cell_footed(&clean, &LINE_GRID, (1, 7)), "a blank cell");
    // A line under every typed cell but the descender's: 11 of 12, and every free cell.
    let mut track = LineTrack::default();
    for k in 0..66i64 {
        let caret = 2 + (k / 3).min(12);
        let mut frame = px.clone();
        for col in (2..caret).filter(|&c| c != 5) {
            let b = LINE_GRID.cell_box(1, col, 1);
            for y in b.y1 - 3..b.y1 - 1 {
                for x in b.x0..b.x1 {
                    frame[at(x, y)] = RAIL[3];
                }
            }
        }
        let ms = 16 * u64::try_from(k + 1).unwrap();
        track.frame(
            ms,
            &Image::from_pixels(w, h, frame),
            &clean,
            &LINE_GRID,
            (1, caret),
        );
    }
    let (lit, cells, _) = track.read.full.expect("judged");
    assert_eq!((lit, cells, track.read.free), (11, 12, (11, 11)));
}

const NO_PET: &str = "trail pet_action=none pet_body=none head_body=none pet_pose=sit pet_alpha=0 v2_meteors=0 word_icons=none\n";

#[test]
fn a_calm_film_keeps_every_law() {
    let dir = film(6, |_, _| {}, |_| NO_PET.to_string());
    let read = judge_film(dir.path(), false).expect("the film reads");
    assert_eq!((read.frames, read.last_ms, read.width), (6, 96, W));
    assert_eq!(
        (read.flash.most, read.light.most, read.twin.most),
        (0, 0, 0)
    );
    let floor = read.contrast.floor.clone().expect("the letters were read");
    assert!(floor.ratio > 10.0, "{floor:?}");
    assert_eq!(read.contrast.lowered, 0);
    let (caret, _) = read.contrast.caret_floor.expect("the caret was read");
    assert!(caret > CARET_BAR, "{caret}");
    assert_eq!(read.pet, PetRead::default());
    assert!(read.broken().is_empty(), "{:?}", read.broken());
    let lines = read.render();
    assert!(
        lines.contains("flash_most=0 flash_turns_most=0 flash_px_over3=0 flash_cells_over3=0"),
        "{lines}"
    );
    assert!(
        lines.contains("companion_on_letters=0 companion_on_caret=0"),
        "{lines}"
    );
    // A folder with no film is an error, never a clean reading.
    let empty = aterm_tempfile::tempdir().expect("a scratch folder");
    assert!(judge_film(empty.path(), false).is_err());
}

#[test]
fn a_planted_star_is_read_as_a_flash_and_nothing_else_is() {
    // A calm film of 70 frames (1120 ms). Control: unplanted, it reads no flash and no star.
    let dir = film(70, |_, _| {}, |_| NO_PET.to_string());
    let calm = judge_film(dir.path(), false).expect("the film reads");
    assert_eq!((calm.flash.over.len(), calm.star), (0, None));
    // Planted, the law puts the star's 9 px over the bound, and no other pixel.
    let read = judge_film(dir.path(), true).expect("the film reads");
    let star = read.star.expect("a star was planted");
    assert_eq!((star.at, star.over), (star_box(W, H), 9));
    assert_eq!((star.at.x0, star.at.y0), (48, 28));
    assert_eq!(read.flash.over.len(), 9, "{:?}", read.flash.over);
    let broken = read.broken();
    assert!(
        broken
            .iter()
            .any(|b| b.starts_with("flash: 9 px flash faster than 3 a second"))
            && !broken.iter().any(|b| b.starts_with("star:")),
        "{broken:?}"
    );
    assert!(
        read.render()
            .contains("STAR     planted_at=48,28 star_px_over3=9 of 9"),
        "{}",
        read.render()
    );
    // A star the law missed breaks the film by name.
    let missed = Reading {
        star: Some(StarRead { over: 3, ..star }),
        ..Reading::default()
    };
    assert_eq!(
        missed.broken(),
        [
            "star: the planted star flashed in 3 of its 9 px: the flash law missed a flash it was given"
        ]
    );
}

#[test]
fn a_star_is_lit_every_other_eighth_of_a_second_against_its_page() {
    let lit_at = |page: Rgb, ms: u64| {
        let mut frame = Image::from_pixels(W, H, vec![page; W * H]);
        plant_star(&mut frame, ms);
        let b = star_box(W, H);
        let x = usize::try_from(b.x0).expect("in the frame");
        let y = usize::try_from(b.y1 - 1).expect("in the frame");
        (frame.at(x, y), frame.at(x - 1, y))
    };
    assert_eq!(
        lit_at(PAGE, 0),
        (WHITE, PAGE),
        "white on a dark page, inside its box alone"
    );
    assert_eq!(lit_at(PAGE, 124), (WHITE, PAGE));
    assert_eq!(lit_at(PAGE, 125), (PAGE, PAGE), "dark for the next eighth");
    assert_eq!(lit_at(PAGE, 250), (WHITE, PAGE));
    assert_eq!(
        lit_at(WHITE, 16),
        ([0, 0, 0], WHITE),
        "black on a light page"
    );
}

#[test]
fn a_wash_over_a_letter_breaks_the_contrast_law() {
    // Negative control: from frame 3 the ground of `a`'s cell is lit to near its ink's colour.
    let clean = clean_page();
    let dir = film(
        6,
        |k, px| {
            if k < 3 {
                return;
            }
            let b = GRID.cell_box(1, 1, 1);
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    let i = usize::try_from(y).unwrap() * W + usize::try_from(x).unwrap();
                    if clean[i] == PAGE {
                        px[i] = [0xc0, 0xc8, 0xd0];
                    }
                }
            }
        },
        |_| NO_PET.to_string(),
    );
    let read = judge_film(dir.path(), false).expect("the film reads");
    assert_eq!(
        (read.contrast.lowered, read.contrast.lowered_frames),
        (4, 4)
    );
    let low = read
        .contrast
        .lowered_floor
        .clone()
        .expect("a lowered letter");
    assert_eq!((low.ms, low.row, low.col, low.ch.as_str()), (48, 1, 1, "a"));
    assert!(low.ratio < 2.0, "{low:?}");
    let broken = read.broken();
    assert!(
        broken.iter().any(|b| b
            .starts_with("contrast: the effects took 4 letter readings in 4 frames under 4.5:1")),
        "{broken:?}"
    );
    // A caret the effects dim to near the page breaks the caret's bar.
    let dir = film(
        3,
        |_, px| paint(px, GRID.cell_box(1, 3, 1), [0x20, 0x24, 0x2a]),
        |_| NO_PET.to_string(),
    );
    let read = judge_film(dir.path(), false).expect("the film reads");
    let broken = read.broken();
    assert!(
        broken
            .iter()
            .any(|b| b.starts_with("contrast: the caret reads")),
        "{broken:?}"
    );
}

#[test]
fn a_film_s_pet_is_read_from_its_status_rows() {
    // The body over `a`'s cell (x 8..14, y 11..21) on frames 3 and 4, beside the line before; two frames of
    // another pose between.
    let dir = film(
        12,
        |_, _| {},
        |k| {
            let body = if (3..5).contains(&k) {
                "8,14,11,21"
            } else {
                "32,44,11,21"
            };
            let pose = if (7..9).contains(&k) {
                "ear-flick"
            } else {
                "sit"
            };
            format!(
                "trail pet_action=sit pet_body={body} head_body=none pet_pose={pose} pet_alpha=255 word_icons=none\n"
            )
        },
    );
    let read = judge_film(dir.path(), false).expect("the film reads");
    assert_eq!(
        (read.pet.on_glass, read.pet.on_letters, read.pet.on_caret),
        (12, 2, 0)
    );
    assert_eq!(read.pet.first_over, Some(48));
    assert_eq!(read.pet.min_dwell, Some((32, "ear-flick".to_string(), 112)));
    let broken = read.broken();
    assert_eq!(broken.len(), 2, "{broken:?}");
    assert!(broken[0].starts_with("pet: on a lettered cell in 2 frames"));
    assert!(broken[1].starts_with("pet: 1 poses shown under 100 ms"));
}

#[test]
fn the_worst_pixel_s_layer_is_the_census_s() {
    // A two-pixel mark in a blank cell of the caret's row, lit for 4 frames and out for 4.
    let mark = GRID.cell_box(1, 6, 1);
    let dir = film(
        70,
        |k, px| {
            if (k / 4) % 2 == 1 {
                let at = PxBox {
                    x0: mark.x0 + 2,
                    y0: mark.y0 + 4,
                    x1: mark.x0 + 4,
                    y1: mark.y0 + 5,
                };
                paint(px, at, WHITE);
            }
        },
        |_| NO_PET.to_string(),
    );
    let read = judge_film(dir.path(), false).expect("the film reads");
    assert_eq!((read.flash.most, read.flash.over.len()), (8, 2));
    assert_eq!(
        read.twin.most, 8,
        "the scratch probe counted rises, two turns each"
    );
    let (px, _, turns) = read.flash.worst.clone().expect("a worst pixel");
    assert_eq!(read.place(px), ((40, 15), (1, 6)));
    // On the frames it turned on it is a star (lit) or the page again (out).
    assert_eq!(
        read.layers,
        vec![("none", turns.len() / 2), ("star", turns.len() / 2)]
    );
    let lines = read.render();
    assert!(
        lines.contains("flash_cells_over3=1 worst=px:40,15;cell:1,6;turns_ms:"),
        "{lines}"
    );
    assert!(lines.contains(";layer:none:8,star:8\n"), "{lines}");
    // It lies in no pet's box and not in the caret's cell: the trail's own.
    assert_eq!(read.places[TRAIL], read.flash);
    assert_eq!(read.trail_layers, read.layers);
    assert!(lines.contains(
        "pet_most=0 pet_turns_most=0 pet_px_over3=0 caret_most=0 caret_turns_most=0 caret_px_over3=0 \
             icon_most=0 icon_turns_most=0 icon_px_over3=0 page_most=0 page_turns_most=0 page_px_over3=0 \
             trail_most=8 trail_turns_most=16 trail_px_over3=2"
    ));
    assert_eq!(
        read.broken()[0],
        "flash: 2 px flash faster than 3 a second (over 6 turns in one), the worst 16 turns"
    );
}

#[test]
fn a_run_films_the_flagship_on_both_themes_and_carets_unless_told() {
    let a = parse_args(&args(&["out"])).expect("a run");
    assert_eq!((a.style.as_str(), a.pet.as_str()), ("rainbow", "cat"));
    assert_eq!(
        (a.word().as_str(), a.folder().as_str()),
        ("rainbow + cat", "rainbow-cat")
    );
    assert_eq!(a.themes, ["dark", "github-light"]);
    assert_eq!(a.carets, ["block", "bar"]);
    assert_eq!(
        a.scenes,
        ["typing", "burst", "context", "enter", "backspace"]
    );
    assert!(a.replay && !a.report_only && !a.plant_star);
    let a = parse_args(&args(&[
        "out",
        "fire",
        "--pet",
        "Dog",
        "--theme",
        "both,solarized-dark,dark",
        "--caret",
        "underline",
        "--scene",
        "steam,cjk",
        "--no-replay",
        "--report-only",
        "--plant-star",
    ]))
    .expect("a run");
    assert_eq!((a.style.as_str(), a.pet.as_str()), ("fire", "dog"));
    assert_eq!(
        (a.word().as_str(), a.folder().as_str()),
        ("fire + dog", "fire-dog")
    );
    let plain = parse_args(&args(&["out", "off", "--pet", "off"])).expect("a run");
    assert_eq!(
        (plain.word().as_str(), plain.folder().as_str()),
        ("off", "off-off")
    );
    assert_eq!(a.themes, ["dark", "github-light", "solarized-dark"]);
    assert_eq!(a.carets, ["underline"]);
    assert_eq!(a.scenes, ["steam", "cjk"]);
    assert!(!a.replay && a.report_only && a.plant_star);
    let all = parse_args(&args(&["out", "--all-scenes"])).expect("a run");
    assert_eq!(all.scenes.len(), SCENES.len());
    let root = Path::new("/films");
    let named = films(&a, root);
    assert_eq!(named.len(), 6);
    assert_eq!(named[0].3, root.join("dark_underline_steam"));
    for bad in [
        &[][..],
        &["out", "--theme", "blue"],
        &["out", "--scene", "both"],
        &["out", "--caret", "beam"],
        &["out", "--scene", "typing", "--all-scenes"],
        &["out", "rainbow", "kitty"],
        &["out", "rainbow kitty underline"],
        &["out", "rainbow dog pet"],
        &["out", "nyan"],
        &["out", "rainbow", "--pet", "kitty"],
        // The flier's flight is the app's: the replay films the pipeline's walkers.
        &["out", "rainbow", "--pet", "flying cat"],
        &["out", "--frames", "3"],
    ] {
        let e = parse_args(&args(bad)).expect_err("a usage error");
        assert_eq!(e.code, 2, "{bad:?}: {:?}", e.msg);
    }
    assert_eq!(style_slug("Classic Mono"), "classic-mono");
    let listed = list();
    assert!(
        SCENES.iter().all(|(name, _)| listed.contains(name)),
        "{listed}"
    );
    assert_eq!(listed.matches("  * ").count(), DEFAULT_SCENES + 1);
}
