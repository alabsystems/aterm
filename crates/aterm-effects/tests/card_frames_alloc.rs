// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact allocation law for the harness card's frames.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use aterm_effects::harness_card::{
    CardFrame, CardFrames, CardPalette, CardScene, EPISODE_MS, FRAME_MS, HarnessCard,
};
use aterm_effects::ripple::FLOW_STEP_MS;

use crate::alloc_count;

const FLOW_STEPS: u64 = 16;

const WARM_EPISODES: usize = 2;

const COUNTED_EPISODES: usize = 2;

fn scene(dark: bool) -> CardScene {
    CardScene {
        origin_x: 10,
        origin_y: 46,
        cw: 9,
        ch: 18,
        row_base: 2,
        pad_left: 10,
        pad_above: 0,
        pad_below: 0,
        top: 3,
        rows: 4,
        cols: 60,
        clip: (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
        trail: Some((32, 40)),
        kitty_col: Some(40),
        kitty_rows: 2,
        palette: CardPalette::Cruise,
        dark,
        ground: if dark { 0x0012_1721 } else { 0x00F8_FAFD },
        serious: false,
        coat: 0x00E8_A060,
        eye: 0x0040_C080,
        wand: Some((10, 3)),
    }
}

/// What the counted episodes' draws allocated.
#[derive(Debug, Default)]
struct Spent {
    total: usize,
    most: usize,
    sizes: Vec<usize>,
}

fn episodes(dark: bool, hold: usize) -> Spent {
    let s = scene(dark);
    let mut card = HarnessCard::default();
    let mut frames = CardFrames::default();
    let mut held: VecDeque<CardFrame> = VecDeque::with_capacity(hold + 1);
    let mut spent = Spent::default();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let cycle = EPISODE_MS + (FLOW_STEPS + 1) * FLOW_STEP_MS;
    for episode in 0..WARM_EPISODES + COUNTED_EPISODES {
        let start = episode as u64 * cycle;
        card.arm(at(start), 0x5eed_ca4d);
        let intro = (0..EPISODE_MS).step_by(FRAME_MS as usize);
        let flow = (1..=FLOW_STEPS).map(|k| EPISODE_MS + k * FLOW_STEP_MS);
        for ms in intro.chain(flow) {
            let now = at(start + ms);
            let (frame, n, sizes) =
                alloc_count::allocations_during(|| card.draw(now, &s, true, &mut frames));
            assert!(
                !frame.is_empty(),
                "episode {episode}, {ms} ms: the card draws"
            );
            if episode >= WARM_EPISODES {
                spent.total += n;
                if n > spent.most {
                    spent.most = n;
                    spent.sizes = sizes.iter().copied().take_while(|&z| z > 0).collect();
                }
            }
            held.push_back(frame);
            if held.len() > hold {
                held.pop_front();
            }
        }
    }
    spent
}

#[test]
fn a_cards_frames_refill_their_ring_without_allocating() {
    for dark in [true, false] {
        let spent = episodes(dark, 2);
        assert_eq!(
            spent.total, 0,
            "dark {dark}: the steady state allocates nothing ({spent:?})"
        );
    }
}

#[test]
fn a_ring_miss_allocates_two_fresh_contents_and_no_growth() {
    for dark in [true, false] {
        let spent = episodes(dark, 3);
        assert!(
            spent.total > 0,
            "dark {dark}: CONTROL, held past the ring's depth every draw misses"
        );
        assert!(
            spent.most <= 6,
            "dark {dark}: a miss allocates each content and its items and inks once ({spent:?})"
        );
    }
}
