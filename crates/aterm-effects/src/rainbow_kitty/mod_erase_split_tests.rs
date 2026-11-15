// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Backspace whose echo shares its engine tick, against the same Backspace split across two:
//! `lab trail-film`'s `backspace` scene on GitHub Light, replayed at the engine's seam with the
//! events the headless host hands it, stamped alike in both orders.

use super::*;
use aterm_render::HaloMode;

/// GitHub Light's ink and page.
const PAPER: (u32, u32) = (0x001F_2328, 0x00FF_FFFF);
/// The film's step, ms.
const FRAME_MS: u64 = 16;
/// The film's row and the column its prompt leaves the caret at.
const ROW: u16 = 4;
const PROMPT_END: u16 = 2;
/// The typing line's keys: 24 at 60 ms from 1 s.
const KEYS: u64 = 24;
const KEY_FROM_MS: u64 = 1_000;
const KEY_EVERY_MS: u64 = 60;
/// Twelve Backspaces 50 ms apart from 60 ms after the last key.
const BACKSPACES: u64 = 12;
const BACKSPACE_FROM_MS: u64 = KEY_FROM_MS + KEY_EVERY_MS * KEYS + 60;
const BACKSPACE_EVERY_MS: u64 = 50;
/// A key's echo lands this long after it.
const ECHO_MS: u64 = 2;
/// The Backspace this probe splits: the third (2600 ms), which on the film shares its tick
/// with its echo.
const PROBED: u64 = 2;

/// The film's glass: 14 x 26 px cells from (12, 2).
const fn film_geom() -> Geom {
    Geom {
        cw: 14,
        ch: 26,
        rows: 14,
        cols: 72,
        origin_x: 12,
        origin_y: 2,
        win_w: 1032,
        win_h: 378,
        head: 0,
    }
}

fn paper() -> Config {
    Config {
        dark_theme: false,
        intensity: 1.0,
        duration: Duration::from_millis(260),
        ribbon_tall: true,
        ribbon_flat: false,
        theme_fg: PAPER.0,
        theme_bg: PAPER.1,
        reduced_motion: false,
        light: Light::OFF,
        head_dx: 0.5,
    }
}

#[derive(Default)]
struct Sink {
    under: Vec<GlowQuad>,
    out: Vec<GlowQuad>,
    under_ramps: aterm_render::GlowRamps,
    out_ramps: aterm_render::GlowRamps,
    halos: Vec<RainHalo>,
    beams: Vec<BeamVertex>,
    cues: Vec<SoundCue>,
}

impl Sink {
    const fn frame(&mut self) -> Frame<'_> {
        Frame {
            under: &mut self.under,
            out: &mut self.out,
            under_ramps: &mut self.under_ramps,
            out_ramps: &mut self.out_ramps,
            halos: &mut self.halos,
            beams: &mut self.beams,
            cues: &mut self.cues,
            caret: CaretSeam::EMPTY,
            companion: None,
            fp: 0,
            rows_cost: Some(aterm_render::RampCost::NEVER),
        }
    }

    /// The frame on the page as the renderer lays it: the under quads, the over quads, then the
    /// halos (added, then laid over).
    fn glass(&self, g: Geom, page: u32) -> Vec<u32> {
        let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
        let mut px = vec![page; w * h];
        for q in self.under.iter().chain(&self.out) {
            let qw = u32::from(q.w.max(1));
            let step = |i: u32, a: u32, b: u32| {
                (a * (2 * qw - (2 * i + 1)) + b * (2 * i + 1) + qw) / (2 * qw)
            };
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                    let i = (x - usize::from(q.x)) as u32;
                    let (color, alpha) = if q.is_flat() {
                        (q.color, q.alpha)
                    } else {
                        let lerp = |sh: u32| {
                            step(i, (q.color >> sh) & 0xff, (q.color2 >> sh) & 0xff) << sh
                        };
                        (
                            lerp(16) | lerp(8) | lerp(0),
                            step(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                        )
                    };
                    let slot = &mut px[y * w + x];
                    *slot = aterm_render::over_premul(*slot, color, alpha);
                }
            }
        }
        for mode in [HaloMode::Add, HaloMode::Over] {
            for q in self.halos.iter().filter(|q| q.mode == mode) {
                let over_cap = aterm_render::halo_over_cap(q.color);
                let (cx, cy) = (i32::from(q.cx), i32::from(q.cy));
                let rx2 = i32::from(q.rx) * i32::from(q.rx);
                let ry2 = i32::from(q.ry) * i32::from(q.ry);
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    let ny = aterm_render::halo_row_ny(y as i32 - cy, ry2);
                    if ny >= 256 {
                        continue;
                    }
                    for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                        let wt = aterm_render::halo_weight(x as i32 - cx, ny, rx2).min(255);
                        if wt <= 0 {
                            continue;
                        }
                        let slot = &mut px[y * w + x];
                        *slot = match mode {
                            HaloMode::Add => aterm_render::add_sat(
                                *slot,
                                aterm_render::premul_rgb(q.color, wt as u8),
                            ),
                            HaloMode::Over => {
                                aterm_render::over_rgb(*slot, q.color, (wt as u8).min(over_cap))
                            }
                        };
                    }
                }
            }
        }
        px
    }
}

/// What the engine is handed at an instant: an event, or a frame.
#[derive(Clone, Copy, Debug)]
enum Step {
    Ev(Event),
    Tick,
}

/// The first frame at or after `ms`.
const fn frame_at_or_after(ms: u64) -> u64 {
    ms.div_ceil(FRAME_MS) * FRAME_MS
}

/// The film's script as the headless host hands it to the engine (`aterm-render`'s `Replay`, the
/// film `lab trail-film` judges), ms from the start; `split` adds one frame between the probed
/// Backspace's keystroke and its echo, and changes nothing else. A key is its `Typed` at its own
/// time and, on the first frame after its echo, its sweep (born at the key) and the caret's move.
/// A Backspace is the console's delete (`Erase`) at its own time and the host's text-blind
/// keystroke for it (`EffectsPipeline::note_keystroke`), which the pipeline's next clock step
/// books as a `Typed` at the echo's time; the erase's echo is the caret's move one cell back on
/// the next frame. Returns the script, sorted (an event before a frame at the same ms), and the
/// probed Backspace's frame.
fn script(split: bool) -> (Vec<(u64, Step)>, u64) {
    let mut s: Vec<(u64, Step)> = Vec::new();
    s.push((
        200,
        Step::Ev(Event::Move {
            from: (ROW, 0),
            to: (ROW, PROMPT_END),
            licence: Licence::Typed,
            dir: Dir::Right,
        }),
    ));
    let mut col = PROMPT_END;
    for k in 0..KEYS {
        let at = KEY_FROM_MS + KEY_EVERY_MS * k;
        let echo = frame_at_or_after(at + ECHO_MS);
        s.push((
            at,
            Step::Ev(Event::Typed {
                cells: 1,
                shifted: false,
                class: TypedClass::Glyph,
            }),
        ));
        s.push((
            echo,
            Step::Ev(Event::Sweep {
                row: ROW,
                col0: col,
                col1: col + 1,
            }),
        ));
        s.push((
            echo,
            Step::Ev(Event::Move {
                from: (ROW, col),
                to: (ROW, col + 1),
                licence: Licence::Typed,
                dir: Dir::Right,
            }),
        ));
        col += 1;
    }
    let mut probed = 0;
    for k in 0..BACKSPACES {
        let at = BACKSPACE_FROM_MS + BACKSPACE_EVERY_MS * k;
        let echo = frame_at_or_after(at + ECHO_MS);
        s.push((at, Step::Ev(Event::Erase)));
        s.push((
            at + ECHO_MS,
            Step::Ev(Event::Typed {
                cells: 1,
                shifted: false,
                class: TypedClass::Glyph,
            }),
        ));
        if split && k == PROBED {
            s.push((at + ECHO_MS, Step::Tick));
        }
        s.push((
            echo,
            Step::Ev(Event::Move {
                from: (ROW, col),
                to: (ROW, col - 1),
                licence: Licence::Typed,
                dir: Dir::Left,
            }),
        ));
        if k == PROBED {
            probed = echo;
        }
        col -= 1;
    }
    let end = BACKSPACE_FROM_MS + BACKSPACE_EVERY_MS * BACKSPACES + 1_400;
    let mut f = FRAME_MS;
    while f <= end {
        s.push((f, Step::Tick));
        f += FRAME_MS;
    }
    // Stable: the events keep their order, and each goes before a frame at its own ms.
    s.sort_by_key(|&(at, step)| (at, matches!(step, Step::Tick)));
    (s, probed)
}

/// One order's frame `at` (ms): the engine as it stood after it, and its glass on paper.
fn film_frame(t0: Instant, split: bool, at: u64) -> (Engine, Vec<u32>) {
    play(t0, script(split).0, at)
}

/// `s` played from `t0` to its frame `at` (ms): the engine as it stood after it, and its glass on
/// paper.
fn play(t0: Instant, s: Vec<(u64, Step)>, at: u64) -> (Engine, Vec<u32>) {
    let g = film_geom();
    let cfg = paper();
    let mut eng = Engine::new();
    eng.set_engaged(true);
    for row in 0..14u16 {
        eng.probe_mut().probe_row(i32::from(row), &[false; 72]);
    }
    let mut glass = Vec::new();
    for (ms, step) in s {
        if ms > at {
            break;
        }
        let now = t0 + Duration::from_millis(ms);
        match step {
            Step::Ev(ev) => eng.on_event(ev, now),
            Step::Tick => {
                let mut sink = Sink::default();
                let mut fr = sink.frame();
                eng.tick(now, g, &cfg, &mut fr);
                if ms == at {
                    glass = sink.glass(g, cfg.theme_bg);
                }
            }
        }
    }
    (eng, glass)
}

/// The live cell that owns `col` on the film's row: when it was born and when its attack began.
fn owner(eng: &Engine, col: u16) -> Option<(Instant, Instant)> {
    eng.ribbon
        .cells()
        .iter()
        .find(|c| c.row == ROW && c.col == col && !c.leaving())
        .map(|c| (c.born, c.attack_at))
}

/// The most the two orders' glass differs by on the probed frame, in WCAG relative luminance, over
/// the caret's row and the rail's rows under it, from the cell left of the erased one to the cell
/// right of the caret it left; and the pixel.
fn widest_swing(a: &[u32], b: &[u32], erased: u16) -> (f32, (usize, usize)) {
    let g = film_geom();
    let (w, cw, ch) = (usize::from(g.win_w), g.cw, g.ch);
    let x0 = usize::from(g.origin_x) + usize::from(erased - 1) * cw;
    let x1 = usize::from(g.origin_x) + usize::from(erased + 2) * cw;
    let y0 = usize::from(g.origin_y) + usize::from(ROW) * ch;
    let y1 = y0 + ch + ch / 2;
    let mut worst = (0.0f32, (0, 0));
    for y in y0..y1 {
        for x in x0..x1 {
            let d = (crate::color_math::relative_luminance(a[y * w + x])
                - crate::color_math::relative_luminance(b[y * w + x]))
            .abs();
            if d > worst.0 {
                worst = (d, (x, y));
            }
        }
    }
    worst
}

/// Both orders read from the probed frame to the next Backspace ([`both_orders`]).
struct Orders {
    /// The erased column's owner on the probed frame, sharing the tick: born, attack start (ms
    /// from `t0`).
    same: (u64, u64),
    /// The same, split across two ticks.
    split: (u64, u64),
    /// The widest swing between the orders' glass, in WCAG relative luminance.
    swing: f32,
    /// Its frame (ms) and pixel.
    frame: u64,
    px: (usize, usize),
}

/// Both orders from the probed frame to the next Backspace: the erased column's owner on the
/// probed frame, and the widest swing between the orders' glass over those frames.
fn both_orders() -> Orders {
    let (_, frame) = script(false);
    let t0 = Instant::now();
    let erased = erased_column();
    let rel = |t: Instant| -> u64 {
        u64::try_from(t.saturating_duration_since(t0).as_millis()).unwrap_or(u64::MAX)
    };
    let owned = |eng: &Engine, order: &str| {
        let (born, attack) = owner(eng, erased).unwrap_or_else(|| panic!("{order}: no cell"));
        (rel(born), rel(attack))
    };
    let mut owners = None;
    let mut worst = (0.0f32, (frame, (0, 0)));
    let next = BACKSPACE_FROM_MS + BACKSPACE_EVERY_MS * (PROBED + 1);
    for at in (frame..next).step_by(FRAME_MS as usize) {
        let k = at - frame;
        let (same, same_glass) = film_frame(t0, false, at);
        let (split, split_glass) = film_frame(t0, true, at);
        if k == 0 {
            owners = Some((owned(&same, "same tick"), owned(&split, "split")));
        }
        let (swing, px) = widest_swing(&same_glass, &split_glass, erased);
        if swing > worst.0 {
            worst = (swing, (at, px));
        }
    }
    let (same, split) = owners.expect("the probed frame");
    Orders {
        same,
        split,
        swing: worst.0,
        frame: worst.1.0,
        px: worst.1.1,
    }
}

/// The column the probed Backspace erases: the caret stood at 26 after the line, and the probed
/// Backspace is the third.
const fn erased_column() -> u16 {
    PROMPT_END + KEYS as u16 - 1 - PROBED as u16
}

/// A turn of the flash law: the least change of relative luminance that counts (WCAG 2.3.1).
const TURN: f32 = 0.10;

/// The widest swing the two orders may differ by: half a turn. What is left is the retraction of
/// the cell the caret leaves, which starts at the erase when the echo shares its tick and at the
/// echo's move when it lands on the next, one frame apart.
const SPLIT_INVARIANT_SWING: f32 = TURN / 2.0;

/// A Backspace draws the same light whether its echo shares its engine tick or lands on the next.
/// Split across two ticks, the erase acts where the caret stands when it comes, and the `Typed`
/// the host notes with every key lands in the first tick's unechoed tail and is held. Sharing the
/// tick, the erase acted on the frame's caret, already moved back, retracting the cell the caret
/// retreats onto; and the `Typed`, replayed at the caret before the retreat, laid a fresh cell
/// there, its attack starting from nothing: the rail and band under the new caret dropped toward
/// the page for a frame and came back, a turn pair on every such Backspace (the light-page
/// Backspace film's rail pixels over the flash law). The control is that order put back.
#[test]
fn a_backspace_draws_the_same_light_whether_its_echo_shares_its_tick_or_not() {
    let Orders {
        same: (same_born, same_attack),
        split: (split_born, split_attack),
        swing,
        frame,
        px,
    } = both_orders();
    eprintln!(
        "erased column {}: same-tick born {same_born} attack {same_attack}, split born \
         {split_born} attack {split_attack}; widest swing {swing:.3} on frame {frame} at px \
         {px:?}",
        erased_column()
    );
    assert_eq!(
        (same_born, same_attack),
        (split_born, split_attack),
        "the erased column keeps the cell its key laid in both orders"
    );
    assert!(
        swing < SPLIT_INVARIANT_SWING,
        "the orders differ by {swing:.3} on frame {frame} at px {px:?}"
    );
}

/// The control: with the same-tick order as it was, the erased column's cell is born again at the
/// keystroke's `Typed`, and the orders differ by a turn or more.
#[test]
fn control_the_key_laid_after_the_erase_flashes_the_erased_column() {
    let Orders {
        same: (same_born, _),
        split: (split_born, _),
        swing,
        frame,
        px,
    } = with_same_tick_erase_as_was(both_orders);
    eprintln!(
        "control: same-tick born {same_born}, split born {split_born}; swing {swing:.3} on \
         frame {frame} at px {px:?}"
    );
    assert_eq!(
        same_born,
        BACKSPACE_FROM_MS + BACKSPACE_EVERY_MS * PROBED + ECHO_MS,
        "same tick: the erased column's cell is born at the keystroke's Typed"
    );
    assert!(
        split_born < BACKSPACE_FROM_MS,
        "split: the column keeps the cell its key laid"
    );
    assert!(
        swing >= TURN,
        "the orders differ by a turn or more: {swing:.3}"
    );
}

/// The second Backspace of the pair: 4 ms after the probed one, inside the same frame.
const PAIR_GAP_MS: u64 = 4;

/// The film's script with a second Backspace [`PAIR_GAP_MS`] after the probed one, a key repeat
/// over a slow frame. `own_ticks`: each Backspace's echo lands on a tick of its own, a retreat of
/// one cell each; otherwise the host samples the caret once on the frame and sees one retreat of
/// two cells, both Backspaces' events in its tick. Returns the script, sorted as [`script`]'s,
/// and the pair's frame.
fn pair_script(own_ticks: bool) -> (Vec<(u64, Step)>, u64) {
    let (base, _) = script(false);
    let mut s: Vec<(u64, Step)> = base
        .into_iter()
        .filter(|&(at, step)| at < BACKSPACE_FROM_MS || matches!(step, Step::Tick))
        .collect();
    let typed = Step::Ev(Event::Typed {
        cells: 1,
        shifted: false,
        class: TypedClass::Glyph,
    });
    let back = |from: u16, to: u16| {
        Step::Ev(Event::Move {
            from: (ROW, from),
            to: (ROW, to),
            licence: Licence::Typed,
            dir: Dir::Left,
        })
    };
    let mut col = PROMPT_END + KEYS as u16;
    let mut pair = 0;
    for k in 0..BACKSPACES {
        let at = BACKSPACE_FROM_MS + BACKSPACE_EVERY_MS * k;
        s.push((at, Step::Ev(Event::Erase)));
        s.push((at + ECHO_MS, typed));
        if k == PROBED {
            let second = at + PAIR_GAP_MS;
            let echo = frame_at_or_after(second + ECHO_MS);
            if own_ticks {
                s.push((at + ECHO_MS + 1, back(col, col - 1)));
                s.push((at + ECHO_MS + 1, Step::Tick));
            }
            s.push((second, Step::Ev(Event::Erase)));
            s.push((second + ECHO_MS, typed));
            if own_ticks {
                s.push((echo, back(col - 1, col - 2)));
            } else {
                s.push((echo, back(col, col - 2)));
            }
            pair = echo;
            col -= 2;
        } else {
            s.push((frame_at_or_after(at + ECHO_MS), back(col, col - 1)));
            col -= 1;
        }
    }
    s.sort_by_key(|&(at, step)| (at, matches!(step, Step::Tick)));
    (s, pair)
}

/// The pair's two orders compared from its frame to a frame past the next Backspace's.
struct PairOrders {
    /// The cells born on the pair's row since its first key, on the pair's frame, with the pair
    /// sharing one tick (column, born, ms from `t0`).
    shared: Vec<(u16, u64)>,
    /// The same with each key echoing on a tick of its own.
    own: Vec<(u16, u64)>,
    /// The widest swing between the two orders' glass.
    swing: f32,
    /// The frame (ms) of that swing.
    frame: u64,
    /// The pixel of that swing.
    px: (usize, usize),
}

/// The pair's two orders from its frame to a frame past the next Backspace's ([`PairOrders`]).
fn both_pair_orders() -> PairOrders {
    let (_, frame) = pair_script(false);
    let t0 = Instant::now();
    let first = BACKSPACE_FROM_MS + BACKSPACE_EVERY_MS * PROBED;
    let rel = |t: Instant| -> u64 {
        u64::try_from(t.saturating_duration_since(t0).as_millis()).unwrap_or(u64::MAX)
    };
    let fresh = |eng: &Engine| -> Vec<(u16, u64)> {
        eng.ribbon
            .cells()
            .iter()
            .filter(|c| c.row == ROW && !c.leaving() && rel(c.born) >= first)
            .map(|c| (c.col, rel(c.born)))
            .collect()
    };
    // The cell left of the pair's second erased column, to the cell right of the caret it left.
    let left = erased_column() - 1;
    let mut cells = None;
    let mut worst = (0.0f32, frame, (0, 0));
    let next = BACKSPACE_FROM_MS + BACKSPACE_EVERY_MS * (PROBED + 1) + 4 * FRAME_MS;
    for at in (frame..next).step_by(FRAME_MS as usize) {
        let (shared, shared_glass) = play(t0, pair_script(false).0, at);
        let (own, own_glass) = play(t0, pair_script(true).0, at);
        if at == frame {
            cells = Some((fresh(&shared), fresh(&own)));
        }
        let (swing, px) = widest_swing(&shared_glass, &own_glass, left);
        if swing > worst.0 {
            worst = (swing, at, px);
        }
    }
    let (shared, own) = cells.expect("the pair's frame");
    PairOrders {
        shared,
        own,
        swing: worst.0,
        frame: worst.1,
        px: worst.2,
    }
}

/// Two Backspaces that share one engine tick (a key repeat over a slow frame: the host samples
/// the caret once and sees one retreat of two cells) draw the same light as when each echoes on
/// a tick of its own: neither keystroke's `Typed` lays a cell on its erased column, and each erase
/// acts at the caret the ones before it left.
#[test]
fn two_backspaces_sharing_a_tick_draw_the_same_light_as_each_on_its_own() {
    let PairOrders {
        shared,
        own,
        swing,
        frame,
        px,
    } = both_pair_orders();
    eprintln!(
        "pair: shared-tick fresh cells {shared:?}, own-ticks {own:?}; widest swing {swing:.3} on \
         frame {frame} at px {px:?}"
    );
    assert_eq!(shared, own, "the pair lays the same cells in both orders");
    assert!(
        shared.is_empty(),
        "no keystroke of the pair lays a cell: {shared:?}"
    );
    assert!(
        swing < SPLIT_INVARIANT_SWING,
        "the orders differ by {swing:.3} on frame {frame} at px {px:?}"
    );
}

/// The control: with the same-tick order as it was, the pair's first keystroke lays a fresh cell
/// on its erased column and the orders differ by a turn or more.
#[test]
fn control_the_pair_s_first_key_flashes_its_erased_column() {
    let PairOrders {
        shared,
        swing,
        frame,
        px,
        ..
    } = with_same_tick_erase_as_was(both_pair_orders);
    eprintln!(
        "pair control: shared-tick fresh cells {shared:?}; swing {swing:.3} on frame {frame} at px {px:?}"
    );
    assert!(
        shared.iter().any(|&(col, _)| col == erased_column()),
        "the pair's first keystroke lays a cell on its erased column: {shared:?}"
    );
    assert!(
        swing >= TURN,
        "the orders differ by a turn or more: {swing:.3}"
    );
}

/// The film's typing line, then a word kill of `cells` (Ctrl-W) at the end of it and its retreat
/// in the same tick, as the seam hands both in one call: the killed cells' retract instants, ms
/// after the kill, nearest the caret first.
fn kill_schedule(cells: u16) -> Vec<(u16, i64)> {
    let (base, _) = script(false);
    let mut s: Vec<(u64, Step)> = base
        .into_iter()
        .filter(|&(at, step)| at < BACKSPACE_FROM_MS || matches!(step, Step::Tick))
        .collect();
    let end = PROMPT_END + KEYS as u16;
    let kill_at = BACKSPACE_FROM_MS;
    s.push((
        kill_at,
        Step::Ev(Event::Kill {
            cells,
            scope: KillScope::Word,
        }),
    ));
    s.push((
        kill_at,
        Step::Ev(Event::Move {
            from: (ROW, end),
            to: (ROW, end - cells),
            licence: Licence::Typed,
            dir: Dir::Left,
        }),
    ));
    s.sort_by_key(|&(at, step)| (at, matches!(step, Step::Tick)));
    let t0 = Instant::now();
    let (eng, _) = play(t0, s, frame_at_or_after(kill_at + 1));
    let kill = t0 + Duration::from_millis(kill_at);
    let mut at: Vec<(u16, i64)> = eng
        .ribbon
        .cells()
        .iter()
        .filter(|c| c.row == ROW && c.col >= end - cells && c.col < end)
        .filter_map(|c| {
            let t = c.retract_at?;
            let ms = if t >= kill {
                i64::try_from(t.duration_since(kill).as_millis()).unwrap_or(i64::MAX)
            } else {
                -i64::try_from(kill.duration_since(t).as_millis()).unwrap_or(i64::MAX)
            };
            Some((c.col, ms))
        })
        .collect();
    at.sort_unstable();
    at
}

/// A word kill and its retreat in one tick retract the killed word as they did before Backspace's
/// tick split was held: every killed cell, farthest from the line's start first, the cell the
/// caret retreats onto last. The kill keeps the frame's caret (it never splits from its retreat),
/// so the erase arm that moves a same-tick Backspace's erase to the caret before its retreat is
/// not a kill's.
#[test]
fn a_word_kill_and_its_retreat_in_one_tick_retract_the_whole_word() {
    let killed = 6u16;
    let at = kill_schedule(killed);
    eprintln!("kill of {killed}: retract ms after the kill by column {at:?}");
    assert_eq!(
        at.len(),
        usize::from(killed),
        "every killed cell retracts: {at:?}"
    );
    assert!(
        at.windows(2).all(|w| w[0].1 >= w[1].1),
        "farthest first: {at:?}"
    );
    // Over the kill's own span (`kill_span_s`, 0.24 s and 12 ms a cell): the nearest cell's turn
    // comes `(n - 1) / n` of it after the kill, not over a Backspace's `RETRACT_FADE_S`.
    let span_ms = super::timing::kill_span_s(killed) * 1_000.0;
    let nearest = f32::from(killed - 1) / f32::from(killed) * span_ms;
    let got = at[0].1 as f32;
    assert!(
        (got - nearest).abs() <= 2.0,
        "the cell the caret retreats onto retracts {got} ms after the kill, not {nearest:.0} \
         (the kill's span {span_ms:.0} ms): {at:?}"
    );
}
