// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The letter guard's work follows what moved: replayed through the shipping
// effects pipeline (`EffectsPipeline::apply`), a guard that takes back the
// frame before's streams while its lights, text and grid are unchanged, and
// a cell's verdict while the cell's lights and text are the frame before's,
// publishes every frame's light streams exactly as one that judges every
// cell of every frame afresh, and a frame whose lights did not move judges
// no cell at all.

use aterm_core::render::{GlowQuad, RenderInput};
use aterm_core::terminal::{ClockReading, Rgb, Terminal};
use aterm_effects::cursor_fx::TrailColor;
use aterm_effects::kitty_pet::PetInputKind;
use aterm_effects::pipeline::EffectsPipeline;

const ROWS: u16 = 14;
const COLS: u16 = 72;
const CELL: (usize, usize) = (18, 38);
const FRAME_MS: u64 = 16;

struct Replay {
    term: Terminal,
    fx: EffectsPipeline,
    ms: u64,
}

impl Replay {
    fn new(style: &str, (fg, bg, cursor): (u32, u32, u32), reuse: bool) -> Self {
        let rgb = |c: u32| Rgb {
            r: (c >> 16) as u8,
            g: (c >> 8) as u8,
            b: c as u8,
        };
        let mut term = Terminal::new(ROWS, COLS);
        term.set_default_foreground(rgb(fg));
        term.set_default_background(rgb(bg));
        term.set_default_cursor_color(Some(rgb(cursor)));
        let mut fx = EffectsPipeline::new();
        fx.set_focused(true);
        fx.set_cursor_glow(
            true,
            style,
            TrailColor::Automatic,
            None,
            260,
            24,
            1.0,
            0.6,
            true,
            cursor,
        );
        let pet = aterm_effects::cursor_glow::GlowStyle::style_names_any_pet(style);
        fx.set_cursor_pet(pet, 0x5EED);
        fx.set_letter_guard_reuse(reuse);
        Self { term, fx, ms: 0 }
    }

    fn to(&mut self, ms: u64) {
        if ms > self.ms {
            self.fx.advance((ms - self.ms) as f64);
            self.ms = ms;
        }
    }

    fn out(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.fx.now(),
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
    }

    fn key(&mut self, c: char) {
        if c == '\r' {
            self.fx.note_console_input(PetInputKind::Submit);
            self.fx.note_keystroke();
            return;
        }
        let input = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let _ = self.fx.note_committed_char(&mut self.term, &input, c);
    }

    /// One frame: the light streams it publishes, each stream's ramps
    /// expanded into the quads they draw.
    fn present(&mut self) -> Frame {
        let mut input: RenderInput = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        self.fx.apply(&mut self.term, &mut input, CELL.0, CELL.1);
        let mut over = Vec::new();
        input
            .cursor_glow_add_ramps
            .expand_into(&input.cursor_glow_add, &mut over);
        let mut under = Vec::new();
        input
            .glow_under_ramps
            .expand_into(&input.glow_under, &mut under);
        Frame {
            over,
            under,
            halos: input.glow_halo.iter().map(|h| h.damage_words()).collect(),
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
struct Frame {
    over: Vec<GlowQuad>,
    under: Vec<GlowQuad>,
    halos: Vec<[u64; 3]>,
}

/// `text` typed from `start` ms, a key every `every` ms, its echo 2 ms on.
fn typed(text: &str, start: u64, every: u64) -> Vec<(u64, Ev)> {
    let mut ev = Vec::new();
    for (i, c) in text.chars().enumerate() {
        let at = start + i as u64 * every;
        ev.push((at, Ev::Key(c)));
        let mut b = [0u8; 4];
        ev.push((at + 2, Ev::Out(c.encode_utf8(&mut b).as_bytes().to_vec())));
    }
    ev
}

#[derive(Clone)]
enum Ev {
    Key(char),
    Out(Vec<u8>),
}

/// Run `events` and `tail` ms after the last, a frame every [`FRAME_MS`]:
/// every frame's streams, and the guard's work per frame `(judged, reused,
/// kept)`: the cells it judged, the cells whose verdict it took back, and
/// whether it took the frame before's streams back whole.
fn run(r: &mut Replay, events: &[(u64, Ev)], tail: u64) -> (Vec<Frame>, Vec<(u64, u64, u64)>) {
    let mut timeline: Vec<(u64, u8, usize)> = events
        .iter()
        .enumerate()
        .map(|(i, (at, _))| (*at, 0, i))
        .collect();
    let end = events.last().map_or(0, |e| e.0) + tail;
    let mut f = FRAME_MS;
    while f <= end {
        timeline.push((f, 1, 0));
        f += FRAME_MS;
    }
    timeline.sort_unstable();
    let (mut frames, mut work) = (Vec::new(), Vec::new());
    for (at, kind, i) in timeline {
        r.to(at);
        if kind == 1 {
            let before = r.fx.letter_guard_work();
            frames.push(r.present());
            let after = r.fx.letter_guard_work();
            work.push((after.0 - before.0, after.1 - before.1, after.2 - before.2));
            continue;
        }
        match events[i].1.clone() {
            Ev::Key(c) => r.key(c),
            Ev::Out(bytes) => r.out(&bytes),
        }
    }
    (frames, work)
}

const DARK: (u32, u32, u32) = (0x00D0_D0D0, 0x0011_1318, 0x0050_FA7B);
const LIGHT: (u32, u32, u32) = (0x001F_2328, 0x00FF_FFFF, 0x0009_69DA);
/// Solarized Dark: dim text, whose letters the lights cut most.
const DIM: (u32, u32, u32) = (0x0083_9496, 0x0000_2B36, 0x0093_A1A1);

/// A frame whose lights, text and grid are the frame before's takes the
/// guard's last streams back and judges no cell, while publishing what a
/// guard judging every cell afresh does; that negative control judges the
/// letters the resting caret's light lies on in those very frames, so the
/// frames kept had work to skip.
#[test]
fn a_frame_whose_lights_did_not_move_judges_no_cell() {
    let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[5H$ ".to_vec()))];
    ev.extend(typed("echo hello world", 1_000, 50));
    for style in ["rainbow kitty pet", "phaser", "laser"] {
        for look in [DARK, LIGHT] {
            let mut kept = Replay::new(style, look, true);
            let (frames, work) = run(&mut kept, &ev, 3_000);
            let mut afresh = Replay::new(style, look, false);
            let (frames_afresh, work_afresh) = run(&mut afresh, &ev, 3_000);
            assert!(
                frames == frames_afresh,
                "{style} on {:06x}: the streams of the guard that keeps frames",
                look.1
            );
            let still: Vec<usize> = (0..work.len()).filter(|&i| work[i].2 > 0).collect();
            for &i in &still {
                assert_eq!(
                    (work[i].0, work[i].1),
                    (0, 0),
                    "{style} on {:06x}: frame {i} kept whole, yet it judged cells",
                    look.1
                );
            }
            let skipped: u64 = still.iter().map(|&i| work_afresh[i].0).sum();
            assert!(
                skipped > 0,
                "{style} on {:06x}: no kept frame had a cell to judge ({} kept)",
                look.1,
                still.len()
            );
            assert!(
                work_afresh.iter().all(|w| w.2 == 0),
                "control: a guard judging afresh kept a frame"
            );
        }
    }
}

/// Over randomized typing (style, theme, text, cadence), the guard that
/// keeps frames and verdicts publishes every frame's streams exactly as
/// the guard that judges every cell of every frame afresh. The negative
/// control (the guard off) publishes other streams in most scenes, so the
/// comparison weighed cut light.
#[test]
fn kept_frames_and_verdicts_publish_what_judging_afresh_does() {
    let mut seed = 0x6775_6172_6421_u64;
    let mut next = || {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let styles = [
        "rainbow kitty pet",
        "rainbow kitty",
        "phaser",
        "comet",
        "fire",
        "laser",
        "sparkle",
        "aurora",
    ];
    let looks = [DARK, LIGHT, DIM];
    let scenes = 6;
    let mut cut = 0;
    for scene in 0..scenes {
        let style = styles[(next() % styles.len() as u64) as usize];
        let look = looks[(next() % looks.len() as u64) as usize];
        let len = 6 + next() % 24;
        let text: String = (0..len)
            .map(|_| match next() % 6 {
                0 => ' ',
                k => char::from(b'a' + ((next() + k) % 26) as u8),
            })
            .collect();
        let every = 30 + next() % 90;
        let mut ev = vec![(200, Ev::Out(b"\x1b[2J\x1b[4H$ ".to_vec()))];
        ev.extend(typed(&text, 600, every));
        let tail = 200 + next() % 1_200;
        let (frames, _) = run(&mut Replay::new(style, look, true), &ev, tail);
        let (afresh, _) = run(&mut Replay::new(style, look, false), &ev, tail);
        assert!(
            frames == afresh,
            "scene {scene} ({style} on {:06x}, {text:?} every {every} ms): the kept guard's streams",
            look.1
        );
        let mut bare = Replay::new(style, look, true);
        bare.fx.set_letter_guard(false);
        let (unguarded, _) = run(&mut bare, &ev, tail);
        cut += usize::from(unguarded != frames);
    }
    assert!(
        2 * cut >= scenes,
        "control: the guard cut light in {cut} of {scenes} scenes"
    );
}

/// The fold of every frame's guarded streams ([`guarded_fold`]) on main, captured at the parent
/// of `docs/design/PRISM-GLASS.md` §15.2 1.9, which noted no light-owned range. At b11eed7965 it
/// was `0x2f59_80aa_2780_cadd`. The trail/looks10 branch changes these styles' lights and the
/// guard's cuts; this test, run on that branch's tip before it merged 1.9 (0088eabcf0), folded to
/// `0xc907_d12e_9b56_8217`, and so did the merged tree: 1.9 still leaves the guard as its parent
/// had it. Review 9's trail/rev9fix floats Comet's dark dust only into a row blank across on the
/// page's own ground (the owner's ruling); merged onto main 227b056193 it folds to the value
/// below. Folded style by style there and on that main, 22 of the 24 styles and grounds fold
/// alike, and the two that differ are Comet on the dark and dim grounds, where the dust is drawn.
const GUARDED_FOLD_AT_THE_PARENT: u64 = 0x0664_f9bc_280d_ea74;

/// FNV-1a over `words`, from `h`.
fn fold_words(mut h: u64, words: impl IntoIterator<Item = u64>) -> u64 {
    for w in words {
        for b in w.to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    h
}

/// Every quad of a published stream as its words.
fn quad_words(q: &GlowQuad) -> [u64; 3] {
    [
        u64::from(q.row) | u64::from(q.x) << 16 | u64::from(q.y) << 32 | u64::from(q.w) << 48,
        u64::from(q.h) | u64::from(q.alpha) << 16 | u64::from(q.alpha2) << 24,
        u64::from(q.color) | u64::from(q.color2) << 32,
    ]
}

/// One fold of the guarded streams every frame publishes, over every style the guard reads and
/// the dark, light and dim grounds, a coloured line beside the typed one: what
/// `EffectsPipeline::apply` hands the renderer once the letter guard had its word.
fn guarded_fold(guard: bool) -> u64 {
    let mut ev = vec![(
        200,
        Ev::Out(b"\x1b[2J\x1b[3H\x1b[31mred\x1b[0m \x1b[34mblue\x1b[0m plain\x1b[5H$ ".to_vec()),
    )];
    ev.extend(typed("echo hello world", 1_000, 50));
    let styles = [
        "rainbow kitty pet",
        "rainbow kitty",
        "phaser",
        "comet",
        "fire",
        "laser",
        "sparkle",
        "aurora",
    ];
    let mut h = 0xCBF2_9CE4_8422_2325_u64;
    for style in styles {
        for look in [DARK, LIGHT, DIM] {
            let mut r = Replay::new(style, look, true);
            r.fx.set_letter_guard(guard);
            let (frames, _) = run(&mut r, &ev, 1_500);
            for f in &frames {
                h = fold_words(h, f.under.iter().flat_map(quad_words));
                h = fold_words(h, [u64::MAX]);
                h = fold_words(h, f.over.iter().flat_map(quad_words));
                h = fold_words(h, [u64::MAX - 1]);
                h = fold_words(h, f.halos.iter().flatten().copied());
                h = fold_words(h, [u64::MAX - 2]);
            }
        }
    }
    h
}

/// The guard's output on main is its parent's (§15.2 1.9 "One owner"): with no light-owned range
/// noted, as on every frame of these grounds, the streams every frame publishes fold to the
/// constant captured at the parent. The negative control, the guard off, folds otherwise, so the
/// fold weighed the guard's cuts.
#[test]
fn the_guarded_streams_fold_as_at_the_parent() {
    let fold = guarded_fold(true);
    assert_eq!(
        fold, GUARDED_FOLD_AT_THE_PARENT,
        "the guarded streams fold to {fold:#018x}"
    );
    assert_ne!(
        guarded_fold(false),
        fold,
        "control: the streams with the guard off fold alike"
    );
}
