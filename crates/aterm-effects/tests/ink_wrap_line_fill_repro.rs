// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Regression tests for cursor-trail behavior across Ink input-box line wraps.

use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;

const BOX_TOP: u16 = 19; // ╭──────╮
const TEXT0: u16 = 20; // │ > text
const TEXT1: u16 = 21; // │   text (continuation after wrap); ╰──────╯ on 22
const TEXT_COL: u16 = 5;
const WRAP_W: usize = 74;

fn geom() -> Geom {
    Geom {
        cw: CW,
        ch: CH,
        rows: ROWS,
        cols: COLS,
        origin_x: 0,
        origin_y: 0,
        win_w: (COLS * CW) as u16,
        win_h: (ROWS * CH) as u16,
        head: 0,
    }
}

fn cfg(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        style,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty),
        ..Default::default()
    }
}

type Cur = Option<(u16, u16)>;

fn sample(term: &Terminal) -> Cur {
    let c = term.cursor();
    term.cursor_visible().then_some((c.row, c.col))
}

fn feed_blink(term: &Terminal, glow: &mut CursorGlow, seen: &mut u64, now: Instant) {
    let epoch = term.repaint_blink_epoch();
    if epoch != *seen {
        *seen = epoch;
        glow.note_repaint_blink(now);
    }
    glow.note_context(term.is_alternate_screen());
}

fn caret(n: usize) -> (u16, u16) {
    if n < WRAP_W {
        (TEXT0, TEXT_COL + n as u16)
    } else {
        (TEXT1, TEXT_COL + (n - WRAP_W) as u16)
    }
}

fn ink_burst(text: &str) -> Vec<u8> {
    let n = text.chars().count();
    let (line0, line1): (String, String) = {
        let chars: Vec<char> = text.chars().collect();
        let a: String = chars[..n.min(WRAP_W)].iter().collect();
        let b: String = chars[n.min(WRAP_W)..].iter().collect();
        (a, b)
    };
    let pad0 = " ".repeat(WRAP_W - line0.chars().count());
    let pad1 = " ".repeat(WRAP_W - line1.chars().count());
    let (crow, ccol) = caret(n);
    let mut s = String::new();
    s.push_str("\x1b[?2026h"); // begin synchronized update (ground truth)
    s.push_str("\x1b[?25l"); // hide inside the sync: the repaint blink
    s.push_str(&format!("\x1b[{BOX_TOP};1H")); // CUP region start
    s.push_str(&format!(
        "\x1b[2K\u{256d}{}\u{256e}\r\n",
        "\u{2500}".repeat(78)
    ));
    s.push_str(&format!("\x1b[2K\u{2502} > {line0}{pad0} \u{2502}\r\n"));
    s.push_str(&format!("\x1b[2K\u{2502}   {line1}{pad1} \u{2502}\r\n"));
    s.push_str(&format!("\x1b[2K\u{2570}{}\u{256f}", "\u{2500}".repeat(78)));
    s.push_str(&format!("\x1b[{crow};{ccol}H")); // CUP caret
    s.push_str("\x1b[?25h"); // show
    s.push_str("\x1b[?2026l"); // end synchronized update
    s.into_bytes()
}

fn ink_burst_split(text: &str) -> (Vec<u8>, Vec<u8>) {
    let whole = ink_burst(text);
    let n = text.chars().count();
    let (crow, ccol) = caret(n);
    let tail = format!("\x1b[{crow};{ccol}H\x1b[?25h\x1b[?2026l").into_bytes();
    let head = whole[..whole.len() - tail.len()].to_vec();
    (head, tail)
}

fn cover(
    glow: &CursorGlow,
    out: &[aterm_core::render::GlowQuad],
    rows: &mut [BTreeSet<u16>],
    rows_patches: &mut [BTreeSet<u16>],
    rows_body: &mut [BTreeSet<u16>],
) {
    let span = |x: u16, w: u16| {
        let c0 = (x as usize / CW) as u16;
        let c1 = ((x as usize + w.max(1) as usize - 1) / CW) as u16;
        c0..=c1.min(COLS as u16 - 1)
    };
    for q in out {
        if (q.row as usize) < ROWS && q.w > 0 {
            rows[q.row as usize].extend(span(q.x, q.w));
            let star_arm = q.w == 1 || (q.h == 1 && q.w <= 6);
            if !star_arm {
                rows_body[q.row as usize].extend(span(q.x, q.w));
            }
        }
    }
    for p in glow.patches() {
        if (p.row as usize) < ROWS && p.w > 0 {
            rows[p.row as usize].extend(span(p.x, p.w));
            rows_patches[p.row as usize].extend(span(p.x, p.w));
            rows_body[p.row as usize].extend(span(p.x, p.w));
        }
    }
    for h in glow.halos() {
        if (h.row as usize) < ROWS && h.w > 0 {
            rows[h.row as usize].extend(span(h.x, h.w));
            rows_body[h.row as usize].extend(span(h.x, h.w));
        }
    }
}

struct Run {
    wrap_move: (Cur, Cur),
    rows: Vec<BTreeSet<u16>>,
    rows_patches: Vec<BTreeSet<u16>>,
    rows_body: Vec<BTreeSet<u16>>,
    quads_on_wrap_frame: usize,
}

fn run_ink(style: GlowStyle, total: usize, split_wrap_burst: bool) -> Run {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let mut glow = crate::common::quads_glow();
    let mut blink_seen = 0u64;
    let c = cfg(style);
    let g = geom();
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut t = t0;
    let text = "the quick brown fox jumps over the lazy dog and keeps typing away \
                until the input box wraps onto its second line"
        .to_string();
    let chars: Vec<char> = text.chars().collect();
    assert!(chars.len() >= total && total > WRAP_W + 2);

    term.process(b"\x1b[?1049h");
    term.process(&ink_burst(""));
    feed_blink(&term, &mut glow, &mut blink_seen, t);
    glow.tick(sample(&term), t, &c, g, &mut out);

    let mut rows: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); ROWS];
    let mut rows_patches: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); ROWS];
    let mut rows_body: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); ROWS];
    let mut wrap_move = (None, None);
    let mut collecting = false;
    let mut collect_until = t;
    let mut quads_on_wrap_frame = 0usize;

    for n in 1..=total {
        let typed: String = chars[..n].iter().collect();
        let is_wrap_key = n == WRAP_W;
        let pre = sample(&term);
        t += Duration::from_millis(16);
        glow.note_synthetic_typed(t, 1);
        if split_wrap_burst && is_wrap_key {
            let (head, tail) = ink_burst_split(&typed);
            term.process(&head);
            let mid = sample(&term);
            assert_eq!(mid, None, "mid-repaint frame must sample a hidden cursor");
            feed_blink(&term, &mut glow, &mut blink_seen, t);
            glow.tick(mid, t, &c, g, &mut out);
            t += Duration::from_millis(16);
            term.process(&tail);
        } else {
            term.process(&ink_burst(&typed));
        }
        let post = sample(&term);
        if is_wrap_key {
            wrap_move = (pre, post);
            collecting = true;
            collect_until = t + Duration::from_millis(600);
        }
        feed_blink(&term, &mut glow, &mut blink_seen, t);
        glow.tick(post, t, &c, g, &mut out);
        if collecting && t <= collect_until {
            if is_wrap_key {
                quads_on_wrap_frame = out.len();
            }
            cover(&glow, &out, &mut rows, &mut rows_patches, &mut rows_body);
        }
        for _ in 0..6 {
            t += Duration::from_millis(16);
            feed_blink(&term, &mut glow, &mut blink_seen, t);
            glow.tick(sample(&term), t, &c, g, &mut out);
            if collecting && t <= collect_until {
                cover(&glow, &out, &mut rows, &mut rows_patches, &mut rows_body);
            }
        }
    }
    Run {
        wrap_move,
        rows,
        rows_patches,
        rows_body,
        quads_on_wrap_frame,
    }
}

fn run_zsh(style: GlowStyle, total: usize) -> Run {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let mut glow = crate::common::quads_glow();
    let mut blink_seen = 0u64;
    let c = cfg(style);
    let g = geom();
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut t = t0;
    term.process(format!("\x1b[{TEXT0};1H").as_bytes());
    feed_blink(&term, &mut glow, &mut blink_seen, t);
    glow.tick(sample(&term), t, &c, g, &mut out);

    let mut rows: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); ROWS];
    let mut rows_patches: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); ROWS];
    let mut rows_body: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); ROWS];
    let mut wrap_move = (None, None);
    let mut collecting = false;
    let mut collect_until = t;
    let mut wrapped = false;

    for _n in 1..=total {
        let pre = sample(&term);
        t += Duration::from_millis(16);
        glow.note_synthetic_typed(t, 1);
        term.process(b"x");
        let post = sample(&term);
        if !wrapped && post.is_some_and(|(r, _)| r == TEXT0) {
            wrapped = true;
            wrap_move = (pre, post);
            collecting = true;
            collect_until = t + Duration::from_millis(600);
        }
        feed_blink(&term, &mut glow, &mut blink_seen, t);
        glow.tick(post, t, &c, g, &mut out);
        if collecting && t <= collect_until {
            cover(&glow, &out, &mut rows, &mut rows_patches, &mut rows_body);
        }
        for _ in 0..6 {
            t += Duration::from_millis(16);
            feed_blink(&term, &mut glow, &mut blink_seen, t);
            glow.tick(sample(&term), t, &c, g, &mut out);
            if collecting && t <= collect_until {
                cover(&glow, &out, &mut rows, &mut rows_patches, &mut rows_body);
            }
        }
    }
    Run {
        wrap_move,
        rows,
        rows_patches,
        rows_body,
        quads_on_wrap_frame: 0,
    }
}

fn mid_band(cols: &BTreeSet<u16>) -> usize {
    cols.iter().filter(|&&c| (18..=60).contains(&c)).count()
}

#[test]
fn ink_wrap_fire_stays_confined_to_typed_cells() {
    let ink = run_ink(GlowStyle::Fire, WRAP_W + 5, false);
    let zsh = run_zsh(GlowStyle::Fire, 78 + 4);

    assert_eq!(
        ink.wrap_move.0,
        Some((TEXT0 - 1, TEXT_COL - 1 + WRAP_W as u16 - 1))
    );
    assert_eq!(ink.wrap_move.1, Some((TEXT1 - 1, TEXT_COL - 1)));

    let landing = ink.rows[(TEXT1 - 1) as usize].len();
    let landing_quiet = ink.rows[(TEXT1 - 1) as usize]
        .iter()
        .filter(|&&c| (16..=72).contains(&c))
        .count();
    assert!(
        landing <= 24,
        "REGRESSION (fire): landing row covered {landing} cols — the line fill is back"
    );
    assert_eq!(
        landing_quiet, 0,
        "REGRESSION (fire): {landing_quiet} cols in the quiet band (16..=72) the \
         cursor never visited lit up"
    );

    let z_landing = zsh.rows[(TEXT1 - 1) as usize].len();
    assert!(
        z_landing <= 12,
        "control: zsh autowrap stays a per-cell typing wake, got {z_landing} cols"
    );
    assert_eq!(
        mid_band(&zsh.rows[(TEXT1 - 1) as usize]),
        0,
        "control: no mid-band fire on the wrapped-onto row"
    );

    let landing_patch_cells = ink.rows_patches[(TEXT1 - 1) as usize].len();
    assert!(
        landing_patch_cells <= 12,
        "landing-row FirePatch cells confined to typed cells: {landing_patch_cells}"
    );
    assert!(
        ink.quads_on_wrap_frame > 0,
        "the typing wake continues through the wrap frame"
    );
}

#[test]
fn ink_wrap_rainbow_keeps_the_ribbon_no_zoom() {
    let ink = run_ink(GlowStyle::RainbowKitty, WRAP_W + 5, false);
    let zsh = run_zsh(GlowStyle::RainbowKitty, 78 + 4);
    let landing = ink.rows_body[(TEXT1 - 1) as usize].len();
    let landing_mid = mid_band(&ink.rows_body[(TEXT1 - 1) as usize]);
    assert!(
        landing <= 22,
        "REGRESSION (nyan): landing row covered {landing} cols — the ZOOM bar is back"
    );
    assert_eq!(landing_mid, 0, "REGRESSION (nyan): mid-band rainbow");
    let z_landing = zsh.rows_body[(TEXT1 - 1) as usize].len();
    assert!(
        z_landing <= 12,
        "control nyan landing row stays a ribbon head: {z_landing}"
    );
}

#[test]
fn ink_wrap_laser_lays_no_full_vector_sweep() {
    let ink = run_ink(GlowStyle::Laser, WRAP_W + 5, false);
    let zsh = run_zsh(GlowStyle::Laser, 78 + 4);
    let landing = ink.rows[(TEXT1 - 1) as usize].len();
    let origin_row = ink.rows[(TEXT0 - 1) as usize].len();
    assert!(
        landing + origin_row <= 45,
        "REGRESSION (laser): {landing}+{origin_row} cols covered — the sweep is back"
    );
    assert_eq!(
        mid_band(&ink.rows[(TEXT1 - 1) as usize]),
        0,
        "REGRESSION (laser): mid-band sparks on the landing row"
    );
    let z_landing = zsh.rows[(TEXT1 - 1) as usize].len();
    assert!(
        z_landing <= 12,
        "control laser landing row stays a typing wake: {z_landing}"
    );
}

#[test]
fn hidden_mid_repaint_frame_suppresses_the_spawn_entirely() {
    let ink = run_ink(GlowStyle::Fire, WRAP_W + 5, true);
    let landing_mid = mid_band(&ink.rows[(TEXT1 - 1) as usize]);
    assert_eq!(
        landing_mid, 0,
        "hidden mid-repaint frame => bridge refuses dist>2 => no spawn, no fill"
    );
}
