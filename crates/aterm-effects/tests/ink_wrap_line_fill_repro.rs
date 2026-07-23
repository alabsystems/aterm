// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! REGRESSION PIN: the Claude Code / Ink input-box WRAP must NOT fill the new
//! line with cursor-trail art. This file began life as the bug's repro (its
//! four tests asserted the artifact into existence: fire meteor + strike
//! fountain, full-width Nyan ZOOM, full-vector Laser sweep); with the TYPED
//! RE-ANCHOR classifier landed the same byte-faithful harness now pins the
//! FIXED behavior — the landing row stays confined to the typed cells for
//! every style, and the mid-band (cells only a phantom jump vector could
//! reach) stays dark.
//!
//! Method: byte-for-byte Ink repaint bursts are fed through a REAL
//! `aterm_core::terminal::Terminal` running on the ALT SCREEN (Claude Code's
//! live-verified surface); after each burst the cursor is sampled EXACTLY
//! like `aterm-gui/src/app_render.rs`'s LOCK A:
//!     let cur = cursor_visible.then_some((cpos.row, cpos.col));
//! the REPAINT-BLINK is derived from the same terminal exactly as the host
//! does per frame (`repaint_blink_epoch()` diffed against a last-seen value →
//! `note_repaint_blink`, plus `note_context(is_alternate_screen())`), and all
//! of it is fed to `CursorGlow::tick` (the same seam `tick_cursor_fx`
//! drives). Each keypress ALSO arms `note_typed` first — the exact app_input
//! seam (a plain Character echo key arms the typed hint before its echo
//! appears; v0.48's kitty-protocol gate on that arm is retired — Claude Code
//! negotiates NO kitty flags, live-verified).
//!
//! The Ink burst per keystroke (Claude Code's repaint choreography — ground
//! truth captured live over the control socket):
//!     CSI ?2026h                    begin synchronized update
//!     CSI ?25l                      hide
//!     CUP(region_top, 1)            jump to the input region
//!     per line: CSI 2K + line       erase + rewrite (CRLF between lines)
//!     CUP(caret)                    reposition to the caret
//!     CSI ?25h                      show
//!     CSI ?2026l                    end synchronized update
//! The hide-INSIDE-sync pair is what advances `repaint_blink_epoch` — the
//! discriminator that lets the alt-screen re-anchor engage here while plain
//! vim (hide, no sync) keeps its jumps.
//!
//! The control is a plain zsh-style autowrap: the shell echoes each typed
//! char with NO repaint (main screen, no sync, no blink), and the terminal's
//! own deferred-wrap moves the cursor (last col -> next row col 0/1). It was
//! already caught by the SHAPE wrap detector and must stay byte-identically
//! healthy.

use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const ROWS: usize = 24;
const COLS: usize = 80;
const CW: usize = 8;
const CH: usize = 16;

/// 1-based rows of the Ink input box (Claude Code style).
const BOX_TOP: u16 = 19; // ╭──────╮
const TEXT0: u16 = 20; // │ > text
const TEXT1: u16 = 21; // │   text (continuation after wrap); ╰──────╯ on 22
/// 1-based column of the first text cell ("│ > " prefix = 4 cols).
const TEXT_COL: u16 = 5;
/// Interior wrap width: cols 5..=78 inclusive (right pad + border at 79/80).
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
        enabled: true,
        dark_theme: true,
        style,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        radius: 0.6,
        ring: true,
        beam: !matches!(style, GlowStyle::Water | GlowStyle::Nyan),
        head_dx: 0.5,
        pack: None,
    }
}

/// A cursor sample `(row, col)` as LOCK A reports it (`None` = hidden).
type Cur = Option<(u16, u16)>;

/// EXACTLY app_render's LOCK A — the per-frame cursor sample.
fn sample(term: &Terminal) -> Cur {
    let c = term.cursor();
    term.cursor_visible().then_some((c.row, c.col))
}

/// EXACTLY app_render's LOCK A repaint-blink derivation, run once per frame
/// immediately before the tick: diff the REAL terminal's monotonic
/// `repaint_blink_epoch` against the window's last-seen value — an advance is
/// the hide-inside-DEC-2026 repaint bracket, so note the blink — and stamp
/// the per-frame alt-screen context into the engine.
fn feed_blink(term: &Terminal, glow: &mut CursorGlow, seen: &mut u64, now: Instant) {
    let epoch = term.repaint_blink_epoch();
    if epoch != *seen {
        *seen = epoch;
        glow.note_repaint_blink(now);
    }
    glow.note_context(term.is_alternate_screen());
}

/// The caret cell (1-based) after `n` typed chars, per Ink's char layout.
fn caret(n: usize) -> (u16, u16) {
    if n < WRAP_W {
        (TEXT0, TEXT_COL + n as u16)
    } else {
        (TEXT1, TEXT_COL + (n - WRAP_W) as u16)
    }
}

/// One full Ink repaint burst for the typed prefix (hide, CUP region, EL+line
/// rewrites, CUP caret, show) — the byte pattern Claude Code emits per key.
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
    s.push_str("\x1b[?25l"); // hide — INSIDE sync: the repaint blink
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

/// The same burst but split where a frame could land MID-REPAINT: everything
/// up to (and including) the line rewrites first, the final CUP+show+sync-end
/// second.
fn ink_burst_split(text: &str) -> (Vec<u8>, Vec<u8>) {
    let whole = ink_burst(text);
    let n = text.chars().count();
    let (crow, ccol) = caret(n);
    let tail = format!("\x1b[{crow};{ccol}H\x1b[?25h\x1b[?2026l").into_bytes();
    let head = whole[..whole.len() - tail.len()].to_vec();
    (head, tail)
}

/// Per-row set of grid columns covered by this frame's emissions.
/// `rows` gets everything (streak/zoom/ember quads + halos + fire patches);
/// `rows_patches` gets ONLY FirePatches (spark-rooted flame cells) — the arm
/// discriminator the original repro used to prove the fill was jump art, not
/// the comet sweep; kept so the pin also bounds spark-rooted flames.
/// `rows_body` gets everything EXCEPT 4-point-star geometry (1-px vertical
/// arms, and short 1-px horizontal arms/streak slabs ≤ 6 px — under a cell):
/// the Nyan starfield now deliberately STREAMS OFF its own row band (the
/// owner's "away from the text" retune), so twinkle pluses legitimately
/// dust neighbouring rows; the zoom-bar pins below must keep counting BODY
/// coverage (bands/zoom slabs span full cells) without the star dust.
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
    /// (pr,pc) -> (cr,cc) of the wrap move as the Terminal reported it.
    wrap_move: (Cur, Cur),
    /// Union column coverage per row, accumulated from the wrap tick through
    /// +600 ms (embracing would-be meteor flight, strike, and 5 more keys).
    rows: Vec<BTreeSet<u16>>,
    /// Same accumulation window, FirePatches only (spark-rooted flame cells).
    rows_patches: Vec<BTreeSet<u16>>,
    /// Same accumulation window, minus 4-point-star geometry (see `cover`).
    rows_body: Vec<BTreeSet<u16>>,
    /// Emissions on the frame IMMEDIATELY after the wrap tick.
    quads_on_wrap_frame: usize,
}

/// Drive an Ink session: `total` chars typed at ~112 ms cadence, 6 idle
/// animation frames (16 ms) between keys — the 60 fps app loop. Every key
/// arms `note_typed` before its repaint burst (the app_input seam). If
/// `split_wrap_burst`, the WRAP keystroke's repaint is split across a frame
/// boundary so one frame samples the HIDDEN mid-repaint state.
fn run_ink(style: GlowStyle, total: usize, split_wrap_burst: bool) -> Run {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let mut glow = CursorGlow::default();
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

    // Claude Code owns the ALT SCREEN (live-verified) — the surface whose
    // re-anchor is blink-gated. Then the initial frame: empty prompt.
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
        // The keystroke that wraps the caret: char #74 fills line0, the caret
        // lands on line1 (see `caret`).
        let is_wrap_key = n == WRAP_W;
        let pre = sample(&term);
        t += Duration::from_millis(16);
        // The app_input seam: a plain Character keypress arms the typed hint
        // BEFORE its echo/repaint reaches the terminal.
        glow.note_typed(t);
        if split_wrap_burst && is_wrap_key {
            let (head, tail) = ink_burst_split(&typed);
            term.process(&head);
            // A frame lands MID-REPAINT: cursor is hidden here (and the hide
            // has already been processed inside the still-open sync, so the
            // blink derivation notes it on this very frame — exactly as the
            // host's tick, which runs before the sync-hold early return).
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
        // 6 idle animation frames between keys (60fps app loop).
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

/// Control: plain zsh — each keystroke echoes ONE char, the terminal's own
/// deferred autowrap moves the cursor. No repaint choreography at all. The
/// typed hint is armed per key exactly like the Ink run (the GUI cannot know
/// which program is attached), and must change NOTHING here: the shape
/// detector already classified this wrap, and non-wrap advances are dist ≤ 1.
fn run_zsh(style: GlowStyle, total: usize) -> Run {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let mut glow = CursorGlow::default();
    let mut blink_seen = 0u64;
    let c = cfg(style);
    let g = geom();
    let mut out = Vec::new();
    let t0 = Instant::now();
    let mut t = t0;
    // Park the shell line where the Ink caret row was, for a fair row compare.
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
        glow.note_typed(t);
        term.process(b"x");
        let post = sample(&term);
        // The autowrap move: previous frame on TEXT0, this frame on TEXT0+1.
        if !wrapped && post.is_some_and(|(r, _)| r == TEXT0) {
            wrapped = true;
            wrap_move = (pre, post);
            collecting = true;
            collect_until = t + Duration::from_millis(600);
        }
        // The host feeds the blink derivation on EVERY frame; a plain echoing
        // shell never advances the epoch (no hide, no sync), so this is the
        // no-blink identity half of the discriminator.
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

/// Columns 18..=60 of a row — far from the post-wrap caret neighbourhood
/// (cols 4..~16: typed cells + crown/halo bloom + laser crackle reach) AND
/// from the old line's right edge (cols ~74..78: cross-row flame skirt).
/// Only art swept/strewn along a phantom jump vector can land here — the
/// definitive line-fill discriminator.
fn mid_band(cols: &BTreeSet<u16>) -> usize {
    cols.iter().filter(|&&c| (18..=60).contains(&c)).count()
}

fn report(tag: &str, r: &Run) {
    eprintln!("== {tag} ==");
    eprintln!("  wrap move: {:?} -> {:?}", r.wrap_move.0, r.wrap_move.1);
    if let (Some((pr, pc)), Some((cr, cc))) = (r.wrap_move.0, r.wrap_move.1) {
        let dr = cr as i32 - pr as i32;
        let dc = cc as i32 - pc as i32;
        let shape_wrap = cr == pr.saturating_add(1) && cc <= 1 && (pc as usize) + 2 >= COLS;
        // The typed hint is armed on every key here, so the hinted RE-ANCHOR
        // (dr_abs == 1 && raw_dist > 2) collapses what the shape test misses.
        let re_anchor = dr.abs() == 1 && dr.abs().max(dc.abs()) > 2;
        eprintln!(
            "  dr={dr} dc={dc} shape-wrap={shape_wrap} hinted-re-anchor={re_anchor} \
             => typing={}",
            shape_wrap || re_anchor || dr.abs().max(dc.abs()) <= 1,
        );
    }
    for (row, cols) in r.rows.iter().enumerate() {
        if !cols.is_empty() {
            eprintln!(
                "  row {row:2}: {:3} cols covered  [{:?}..{:?}]  (FirePatch cells: {})",
                cols.len(),
                cols.iter().next().unwrap(),
                cols.iter().last().unwrap(),
                r.rows_patches[row].len(),
            );
        }
    }
    eprintln!("  wrap-frame quads={}", r.quads_on_wrap_frame);
}

/// FIXED, Fire: the Ink wrap ((19,77)->(20,4), visible->visible in one frame)
/// pairs with the typed hint and RE-ANCHORS — no meteor, no arrival strike,
/// no ember fountain strewn along a phantom 73-column vector. The landing row
/// stays confined to the typed cells (the control's healthy footprint), and
/// the mid-band the cursor never visited stays completely dark.
#[test]
fn ink_wrap_fire_stays_confined_to_typed_cells() {
    let ink = run_ink(GlowStyle::Fire, WRAP_W + 5, false);
    let zsh = run_zsh(GlowStyle::Fire, 78 + 4);
    report("INK fire", &ink);
    report("ZSH fire (control)", &zsh);

    // The derived move is exactly the Ink wrap shape (0-based): the caret
    // leaves the last interior column of TEXT0 and lands on TEXT1 col 4 —
    // NOT (col<=1, from col>=cols-2), so the SHAPE detector still misses it;
    // only the typed-hint pairing classifies it. Pin the shape so a harness
    // drift can never make this test pass vacuously.
    assert_eq!(
        ink.wrap_move.0,
        Some((TEXT0 - 1, TEXT_COL - 1 + WRAP_W as u16 - 1))
    );
    assert_eq!(ink.wrap_move.1, Some((TEXT1 - 1, TEXT_COL - 1)));

    // THE PIN: the landing row is a per-cell typing wake — typed cells + crown
    // bloom on the left (cols ≤ ~15) and the OLD line's legitimate cross-row
    // halo skirt at the right edge (cols ~74..78, cursor/spark-anchored art
    // that spilled a row band before the fix too) — never a line fill (the
    // bug measured 62 cols with >= 25 mid-band cells). Between those two
    // anchored clusters the row is COMPLETELY dark: pin a quiet band even
    // wider than the diagnostic mid-band.
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

    // CONTROL: plain zsh autowrap stays byte-identically healthy under the
    // per-key typed hint (shape-wrap already collapsed it; non-wrap advances
    // are dist <= 1, outside the re-anchor window).
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

    // Spark-rooted flames stay confined to the typed cells too (the original
    // arm discriminator, now a plain bound).
    let landing_patch_cells = ink.rows_patches[(TEXT1 - 1) as usize].len();
    assert!(
        landing_patch_cells <= 12,
        "landing-row FirePatch cells confined to typed cells: {landing_patch_cells}"
    );
    // And the wake CONTINUES at the wrap (the visible->visible re-anchor keeps
    // emitting — contrast the hidden-frame case below, which lays nothing).
    assert!(
        ink.quads_on_wrap_frame > 0,
        "the typing wake continues through the wrap frame"
    );
}

/// FIXED, Nyan: no full-width ZOOM streak — the ribbon continues at the
/// landing cell and the landing row stays a ribbon head, like the control.
#[test]
fn ink_wrap_nyan_keeps_the_ribbon_no_zoom() {
    let ink = run_ink(GlowStyle::Nyan, WRAP_W + 5, false);
    let zsh = run_zsh(GlowStyle::Nyan, 78 + 4);
    report("INK nyan", &ink);
    report("ZSH nyan (control)", &zsh);
    // BODY coverage (`rows_body`): the ZOOM-bar tell is full-cell band/slab
    // geometry. The starfield now deliberately streams OFF its own row band
    // (the owner's "away from the text" retune), so thin twinkle pluses
    // legitimately dust the neighbouring rows — including this landing row —
    // and are excluded from the zoom pin by `cover`'s star-arm filter.
    //
    // REPIN (fresh-ink pop): every key typed on the landing row now also
    // births a warm-white pop on its glyph cell whose ~150 ms birth-flash
    // halo blooms ±2 cells around it (radial halos land in `rows_body`), so
    // the 5 post-wrap keys legitimately widen the typed neighbourhood's
    // coverage from ~10 to a measured 15 cols. The bound moves 12 → 18 (the
    // same +3 headroom discipline as the original pin); the true zoom-bar
    // discriminator remains the MID-BAND assert below — cells only a phantom
    // jump vector could reach stay dark, pop or no pop.
    let landing = ink.rows_body[(TEXT1 - 1) as usize].len();
    let landing_mid = mid_band(&ink.rows_body[(TEXT1 - 1) as usize]);
    assert!(
        landing <= 18,
        "REGRESSION (nyan): landing row covered {landing} cols — the ZOOM bar is back"
    );
    assert_eq!(landing_mid, 0, "REGRESSION (nyan): mid-band rainbow");
    let z_landing = zsh.rows_body[(TEXT1 - 1) as usize].len();
    assert!(
        z_landing <= 12,
        "control nyan landing row stays a ribbon head: {z_landing}"
    );
}

/// FIXED, Laser (comet-path family): no full-vector spark sweep, no jump
/// bolt — the charged trail discharges in place on the old row while typing
/// continues on the new one. Both rows together stay far under the bug's
/// 80+80 full-line coverage.
#[test]
fn ink_wrap_laser_lays_no_full_vector_sweep() {
    let ink = run_ink(GlowStyle::Laser, WRAP_W + 5, false);
    let zsh = run_zsh(GlowStyle::Laser, 78 + 4);
    report("INK laser", &ink);
    report("ZSH laser (control)", &zsh);
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

/// HIDE-PATH PIN (unchanged behavior): if a frame catches the repaint
/// MID-BURST (cursor hidden), `self.last` resets to None and the hide-bridge
/// (chebyshev<=2) refuses the wrap-sized move — NO spawn at all, with or
/// without the typed hint (the `raw_dist > 2` guard keeps the bridge law
/// exact). The mid-band stays dark here for the OTHER reason: nothing spawns.
#[test]
fn hidden_mid_repaint_frame_suppresses_the_spawn_entirely() {
    let ink = run_ink(GlowStyle::Fire, WRAP_W + 5, true);
    report("INK fire, split burst", &ink);
    // Cursor-anchored art (crown bloom around the caret, the old line's
    // cross-row flame skirt at cols ~74..78) still shows, but the MID-BAND —
    // reachable only by jump-vector art — stays dark: no meteor spawned.
    let landing_mid = mid_band(&ink.rows[(TEXT1 - 1) as usize]);
    assert_eq!(
        landing_mid, 0,
        "hidden mid-repaint frame => bridge refuses dist>2 => no spawn, no fill"
    );
}
