// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seeded random programs drawn ONLY from the subset both engines model, the
//! comparison that runs them, and the shrinker that cuts a failure down to a
//! repro.
//!
//! A program is a grid size and a list of tokens, each token one or two chunks
//! of bytes. Both engines are fed chunk by chunk and compared after every
//! chunk, so a divergence is caught at the token that caused it, and a token
//! split mid-sequence exercises both parsers' state across a `process()` /
//! `Record::Out` boundary.
//!
//! WHY THE GRAMMAR IS SHAPED AS IT IS. A byte soup would spend nearly all of
//! its length outside the subset (astream-term consumes and ignores most of
//! ECMA-48; aterm executes it), and every program would diverge for a reason
//! nobody needs to hear again. So each token is a construct inside the subset,
//! and the few places where the two engines differ BY DESIGN are kept out by
//! construction, each at the token that would reach it (search this file for
//! `[subset]`). Where a fold lacks an xterm behaviour (the ones astream's
//! `fix/term-oracle-divergences` added, pinned since 4e91e82), the token is
//! shaped around the gap only while [`Caps`] says the fold still has it.
//!
//! Most of those rules live inside one token (a background span resets the
//! background before it ends; an erase carries the motion that resolves a
//! wrap). The rest span tokens (no second `?1049h`, no `?1049l` on the main
//! screen, a DECRC on the alternate screen after a DECSC of the same visit),
//! and those are one [`Track`], which the generator obeys and the shrinker
//! re-checks: a program is in the subset only if `Track` admits every token
//! in order, so dropping a token can never rebuild a difference the grammar
//! keeps out, and a shrunk failure is still a real divergence.

use super::caps::Caps;
use super::engines::{Astream, Aterm, Snap};
use std::fmt::Write as _;

/// splitmix64: dependency-free, and the same stream on every host.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }

    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

/// What a token does to the state the cross-token [subset] rules read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// None of it.
    Plain,
    /// `CSI ? mode h` (`set`) or `l`, after any wrap-resolving motion.
    Alt { mode: u16, set: bool },
    /// DECSC or `CSI s`.
    Save,
    /// DECRC or `CSI u`, with the pen reset that may follow it.
    Restore,
    /// RIS.
    Reset,
    /// `CSI ? 6 h` (`set`) or `l`: origin mode.
    Origin { set: bool },
    /// DECSTBM, valid or not.
    Region,
}

/// What [`Track`] knows of one DECSC slot: it was written with origin mode
/// on, and a DECSTBM has come since.
#[derive(Clone, Copy, Debug, Default)]
struct OriginSlot {
    origin: bool,
    region_moved: bool,
}

impl OriginSlot {
    /// [subset] A restore of this slot would put origin mode back on over a
    /// region that may no longer hold the saved row, where xterm and aterm
    /// clamp differently (the `decrc_under_origin_mode_above_the_region`
    /// exclusion).
    fn blocks_restore(self) -> bool {
        self.origin && self.region_moved
    }
}

/// One token: its kind, and the chunks it is fed as, in order.
#[derive(Clone, Debug)]
pub struct Token {
    pub kind: Kind,
    pub chunks: Vec<Vec<u8>>,
}

/// One generated program.
#[derive(Clone, Debug)]
pub struct Program {
    pub cols: u16,
    pub rows: u16,
    pub tokens: Vec<Token>,
}

impl Program {
    pub fn bytes(&self) -> Vec<u8> {
        self.tokens
            .iter()
            .flat_map(|t| &t.chunks)
            .flatten()
            .copied()
            .collect()
    }

    /// Whether every token is one the grammar could have put there: the
    /// cross-token [subset] rules hold all the way through.
    pub fn in_subset(&self, caps: Caps) -> bool {
        let mut track = Track::default();
        self.tokens.iter().all(|t| {
            let ok = track.admits(t.kind, caps);
            track.step(t.kind);
            ok
        })
    }
}

/// The terminal state the cross-token [subset] rules read. Which buffer is up
/// is exactly known here, since only alternate-screen tokens and RIS change
/// it.
#[derive(Clone, Copy, Debug, Default)]
struct Track {
    in_alt: bool,
    /// A cursor save happened in this alternate-screen visit.
    alt_saved: bool,
    /// The last visit to the alternate screen ended with `?1049l`, after
    /// which aterm has dropped the buffer xterm keeps.
    alt_dropped: bool,
    /// A RIS came since the last `?1049h`: aterm dropped the alternate buffer
    /// there, which xterm (and a fold with `ris_keeps_alt`) keeps.
    ris_dropped_buffer: bool,
    /// A RIS came since the alternate screen's DECSC slot was last written:
    /// aterm emptied it there, which xterm (and a fold with `ris_keeps_alt`)
    /// keeps.
    ris_dropped_slot: bool,
    /// Origin mode is on.
    origin: bool,
    /// The main screen's DECSC slot, as origin mode sees it.
    main_slot: OriginSlot,
    /// The alternate screen's.
    alt_slot: OriginSlot,
}

impl Track {
    /// Whether a token of `kind` may come next.
    fn admits(&self, kind: Kind, caps: Caps) -> bool {
        match kind {
            // [subset] `?47` only once the fold keeps one persistent
            // alternate buffer, as xterm does: today it hands out a blank one
            // per entry.
            Kind::Alt { mode: 47, .. } if !caps.alt_persists => false,
            // [subset] No second `?1049h` while the fold skips its
            // `ClearScreen` (xterm runs it whether or not it switched).
            Kind::Alt {
                mode: 1049,
                set: true,
            } if self.in_alt => caps.alt_clears,
            // [subset] After a `?1049l` exit aterm frees the alternate buffer
            // (the `a_1049_visit_leaves_no_buffer_behind` exclusion) where
            // xterm keeps it, so while the fold keeps one the next entry is a
            // `?1049h`, whose clear makes the two agree again.
            Kind::Alt { mode, set: true } if !self.in_alt && self.alt_dropped => {
                mode == 1049 || !caps.alt_persists
            }
            // [subset] RIS keeps the alternate buffer in xterm and in a fold with
            // `ris_keeps_alt`; aterm drops it (the `ris_keeps_*` exclusions). So
            // the next entry after one is a `?1049h`, whose clear makes the two
            // agree again.
            Kind::Alt { mode, set: true }
                if !self.in_alt && self.ris_dropped_buffer && caps.ris_keeps_alt =>
            {
                mode == 1049
            }
            // [subset] No `?1049l` on the main screen: xterm (and the fold)
            // restore the cursor there, aterm moves nothing (the
            // `stray_1049l_*` exclusions). Its restore reads the main slot,
            // which [`OriginSlot::blocks_restore`] may rule out.
            Kind::Alt {
                mode: 1049,
                set: false,
            } => self.in_alt && !self.main_slot.blocks_restore(),
            // [subset] While the pinned fold drops the alternate screen's
            // DECSC slot on every switch (xterm keeps `sc[1]` with its one
            // persistent buffer), a restore on the alternate screen follows a
            // save made in the same visit.
            // Nor after a RIS while the fold keeps the slot aterm emptied, until a
            // save on the alternate screen writes it again.
            Kind::Restore => {
                let slot_known = !self.in_alt
                    || self.alt_saved
                    || (caps.alt_persists && !(self.ris_dropped_slot && caps.ris_keeps_alt));
                slot_known && !self.slot().blocks_restore()
            }
            Kind::Alt { .. }
            | Kind::Plain
            | Kind::Save
            | Kind::Reset
            | Kind::Origin { .. }
            | Kind::Region => true,
        }
    }

    /// The DECSC slot of the buffer that is up.
    fn slot(&self) -> OriginSlot {
        if self.in_alt {
            self.alt_slot
        } else {
            self.main_slot
        }
    }

    /// A DECSC (or the save `?1049h` makes) into the slot of the buffer
    /// that is up.
    fn save(&mut self) {
        let slot = OriginSlot {
            origin: self.origin,
            region_moved: false,
        };
        if self.in_alt {
            self.alt_slot = slot;
        } else {
            self.main_slot = slot;
        }
    }

    fn step(&mut self, kind: Kind) {
        match kind {
            Kind::Alt { mode, set } => {
                if set && !self.in_alt {
                    self.alt_saved = false;
                }
                if mode == 1049 {
                    if set {
                        // `?1049h` saves into the slot of the buffer it
                        // leaves (or of the alternate one, already up).
                        self.save();
                    }
                    if set && self.in_alt {
                        // A second `?1049h` saves into the alternate slot.
                        self.alt_saved = true;
                        self.ris_dropped_slot = false;
                    }
                    if set {
                        self.alt_dropped = false;
                        self.ris_dropped_buffer = false;
                    } else if self.in_alt {
                        self.alt_dropped = true;
                        // `?1049l` restores the main slot.
                        self.origin = self.main_slot.origin;
                    }
                }
                self.in_alt = set;
            }
            Kind::Save => {
                self.save();
                self.alt_saved |= self.in_alt;
                self.ris_dropped_slot &= !self.in_alt;
            }
            Kind::Restore => self.origin = self.slot().origin,
            Kind::Reset => {
                *self = Track {
                    ris_dropped_buffer: true,
                    ris_dropped_slot: true,
                    ..Track::default()
                }
            }
            Kind::Origin { set } => self.origin = set,
            Kind::Region => {
                self.main_slot.region_moved = true;
                self.alt_slot.region_moved = true;
            }
            Kind::Plain => {}
        }
    }
}

/// Grid sizes: the degenerate one-row and one-column screens (every glyph a
/// pending wrap, every line feed a scroll), small ones where wraps, tabs and
/// scroll regions collide, and a couple of ordinary widths.
const COLS: [u16; 11] = [1, 2, 3, 4, 5, 8, 10, 13, 16, 20, 33];
const ROWS: [u16; 8] = [1, 2, 3, 4, 5, 6, 8, 10];

/// Printable glyphs. All one cell wide in both engines: ASCII, and narrow
/// non-ASCII (Latin-1 letters, a box-drawing rule). The ASCII ones straddle
/// DEC Special Graphics' range (`^` just below it, `_` and `~` at its ends).
/// [subset] No East Asian wide glyph or combining mark: astream-term gives
/// every codepoint one cell (screen.rs "One codepoint per cell"), aterm
/// gives a wide glyph two and folds a mark into its base.
const GLYPHS: &[char] = &[
    'a', 'b', 'c', 'x', 'y', 'z', 'A', 'M', 'Z', '0', '7', '9', ' ', '-', '#', '~', '[', ';', 'q',
    'l', '_', '^', '`', '{', '}', '\u{e9}', '\u{df}', '\u{f1}', '\u{2500}',
];

fn csi(params: &str, final_byte: char) -> Vec<u8> {
    format!("\x1b[{params}{final_byte}").into_bytes()
}

/// A CSI numeric parameter: absent, zero (both read 0 as the default), small,
/// or past the grid so the clamps run.
fn num(r: &mut Rng, limit: u16) -> String {
    match r.below(6) {
        0 => String::new(),
        1 => "0".to_string(),
        2 | 3 => (1 + r.below(u64::from(limit.max(1)))).to_string(),
        4 => (1 + r.below(3)).to_string(),
        _ => (u64::from(limit) + 1 + r.below(5)).to_string(),
    }
}

fn text(r: &mut Rng, len: usize) -> Vec<u8> {
    let mut s = String::new();
    for _ in 0..len {
        s.push(r.pick(GLYPHS));
    }
    s.into_bytes()
}

/// A run length that lands on the interesting edges as often as in between:
/// a glyph or three, one short of the width, the width exactly (the pending
/// wrap), one past it, and two lines' worth (a wrap that scrolls).
fn run_len(r: &mut Rng, cols: u16) -> usize {
    let c = usize::from(cols);
    match r.below(7) {
        0..=2 => 1 + r.below(3) as usize,
        3 => c.saturating_sub(1).max(1),
        4 => c,
        5 => c + 1,
        _ => 2 * c,
    }
}

/// An SGR parameter list. Foreground and the three attributes always; the
/// background only when `bg` (see `Gen::token` for when that is safe).
/// [subset] No SGR astream-term does not model: faint (2) is its speculative
/// echo mark, never set by SGR; italic, blink, conceal, strike, double
/// underline (21), overline, super/subscript and truecolour have no slot in
/// its cell.
fn sgr(r: &mut Rng, bg: bool) -> Vec<u8> {
    let mut parts: Vec<String> = Vec::new();
    let n = r.below(4);
    if n == 0 {
        // `CSI m` and `CSI ; m`: an empty list and an empty parameter are both
        // SGR 0 (ECMA-48 8.3.117 default).
        return if r.chance(1, 2) {
            csi("", 'm')
        } else {
            csi(";", 'm')
        };
    }
    for _ in 0..n {
        let fg_or_attr = |r: &mut Rng| -> String {
            match r.below(9) {
                0 => r.pick(&["0", "1", "4", "7", "22", "24", "27"]).to_string(),
                1 | 2 => (30 + r.below(8)).to_string(),
                3 => "39".to_string(),
                4 => (90 + r.below(8)).to_string(),
                5 => format!("38;5;{}", r.below(256)),
                6 => format!("38:5:{}", r.below(256)),
                7 => format!("4:{}", r.below(6)),
                _ => r.pick(&["1", "4", "7"]).to_string(),
            }
        };
        let p = if bg && r.chance(1, 3) {
            match r.below(5) {
                0 | 1 => (40 + r.below(8)).to_string(),
                2 => (100 + r.below(8)).to_string(),
                3 => format!("48;5;{}", r.below(256)),
                _ => "49".to_string(),
            }
        } else {
            fg_or_attr(r)
        };
        parts.push(p);
    }
    csi(&parts.join(";"), 'm')
}

/// After a cursor restore. [subset] While the pinned fold restores the
/// position only, the pen is put back to a known state at once: xterm's
/// DECRC (and `?1049l`) restore the SGR rendition with the cursor, and the
/// pen the program saved is not the oracle's to compare.
fn after_restore(caps: Caps, mut t: Vec<u8>) -> Vec<u8> {
    if !caps.decsc_saves_pen {
        t.extend_from_slice(b"\x1b[0m");
    }
    t
}

/// A motion that resolves a pending wrap. [subset] While the pinned fold
/// erases past a parked glyph (the xterm.js reading) instead of through it
/// (xterm's `ClearRight`), every erase is preceded by one of these, so no
/// erase starts from a pending wrap.
fn resolve_wrap(r: &mut Rng, cols: u16, rows: u16) -> Vec<u8> {
    match r.below(3) {
        0 => csi(
            &format!(
                "{};{}",
                1 + r.below(u64::from(rows)),
                1 + r.below(u64::from(cols))
            ),
            'H',
        ),
        1 => csi(&(1 + r.below(u64::from(cols))).to_string(), 'G'),
        _ => b"\x08\x1b[C".to_vec(),
    }
}

fn erase(r: &mut Rng, caps: Caps) -> Vec<u8> {
    let modes: &[&str] = if caps.ed3_spares_screen {
        &["", "0", "1", "2", "3"]
    } else {
        // [subset] `ED 3` only while the fold leaves the screen alone.
        &["", "0", "1", "2"]
    };
    match r.below(5) {
        0 | 1 => csi(r.pick(modes), 'J'),
        2 | 3 => csi(r.pick(&["", "0", "1", "2"]), 'K'),
        // DECSED / DECSEL: with nothing protected (no DECSCA in the subset)
        // they erase exactly like ED / EL.
        _ => format!(
            "\x1b[?{}{}",
            r.pick(&["", "0", "1", "2"]),
            r.pick(&['J', 'K'])
        )
        .into_bytes(),
    }
}

/// The generator's own state: the grid, the capabilities, and the [`Track`]
/// a token's shape depends on.
struct Gen {
    r: Rng,
    cols: u16,
    rows: u16,
    caps: Caps,
    track: Track,
}

impl Gen {
    /// An alternate-screen switch, shaped to what [`Track`] admits. [subset]
    /// While the pinned fold carries a pending wrap across `?1049h` and
    /// `?1047l` (each of which xterm ends or starts with `ClearScreen`, and so
    /// `ResetWrap`), those two are preceded by a motion that resolves the
    /// wrap.
    fn alt_switch(&mut self) -> (Kind, Vec<u8>) {
        let caps = self.caps;
        let mut modes = vec![1049, 1047];
        if caps.alt_persists {
            modes.push(47);
        }
        let mut mode = self.r.pick(&modes);
        let set = self.r.chance(1, 2);
        if !self.track.admits(Kind::Alt { mode, set }, caps) {
            if mode == 1049 && set && self.track.in_alt {
                // The second `?1049h` the fold would not clear for.
                return (Kind::Plain, csi("?25", 'h'));
            }
            // A `?1049l` on the main screen becomes a stray `?1047l` (which
            // both ignore); an entry after a dropped buffer becomes `?1049h`.
            mode = if set { 1049 } else { 1047 };
        }
        let kind = Kind::Alt { mode, set };
        debug_assert!(
            self.track.admits(kind, caps),
            "{kind:?} after {:?}",
            self.track
        );
        let mut t = if !caps.alt_clears && ((mode == 1049 && set) || (mode == 1047 && !set)) {
            resolve_wrap(&mut self.r, self.cols, self.rows)
        } else {
            Vec::new()
        };
        t.extend(csi(&format!("?{mode}"), if set { 'h' } else { 'l' }));
        if mode == 1049 && !set {
            (kind, after_restore(caps, t))
        } else {
            (kind, t)
        }
    }

    /// One token, and its kind. See the module header for the shape of the
    /// grammar.
    fn token(&mut self) -> (Kind, Vec<u8>) {
        let (cols, rows, caps) = (self.cols, self.rows, self.caps);
        let lim = cols.max(rows);
        let r = &mut self.r;
        let plain = match r.below(36) {
            // Text: the bulk of any real stream.
            0..=8 => {
                let n = run_len(r, cols);
                text(r, n)
            }
            // C0 format effectors, and two C0s both engines ignore.
            9..=12 => {
                let mut set: Vec<&[u8]> =
                    vec![b"\r", b"\n", b"\r\n", b"\x08", b"\t", b"\x07", b"\x00"];
                if caps.vt_ff_feed {
                    set.extend_from_slice(&[b"\x0b", b"\x0c"]);
                }
                r.pick(&set).to_vec()
            }
            // Relative motion.
            13..=15 => {
                let f = r.pick(&['A', 'B', 'C', 'D', 'E', 'F']);
                csi(&num(r, lim), f)
            }
            // Absolute motion.
            16 | 17 => match r.below(4) {
                0 => csi(&num(r, cols), 'G'),
                1 => csi(&num(r, rows), 'd'),
                2 => csi(&format!("{};{}", num(r, rows), num(r, cols)), 'H'),
                _ => csi(&format!("{};{}", num(r, rows), num(r, cols)), 'f'),
            },
            18 | 19 => {
                let mut t = if caps.erase_resets_wrap {
                    Vec::new()
                } else {
                    resolve_wrap(r, cols, rows)
                };
                t.extend(erase(r, caps));
                t
            }
            20 | 21 => sgr(r, caps.bce_on_scroll),
            // A background span. [subset] While the pinned fold scrolls in a
            // default blank where xterm (and aterm) scroll in the current
            // background, a background is set only here: addressed, painted
            // short of the last column so no wrap (and so no scroll) can
            // happen under it, maybe erased with, and reset before the token
            // ends.
            22 => {
                let row = 1 + r.below(u64::from(rows));
                let col = r.below(u64::from(cols));
                let room = usize::from(cols) - 1 - col as usize;
                let len = r.below(room as u64 + 1) as usize;
                let mut t = csi(&format!("{row};{}", col + 1), 'H');
                let bg = match r.below(3) {
                    0 => format!("{}", 40 + r.below(8)),
                    1 => format!("{}", 100 + r.below(8)),
                    _ => format!("48;5;{}", r.below(256)),
                };
                t.extend(csi(&bg, 'm'));
                t.extend(text(r, len));
                if r.chance(1, 2) {
                    t.extend(erase(r, caps));
                }
                t.extend_from_slice(b"\x1b[49m");
                t
            }
            23 => return self.alt_switch(),
            // Cursor visibility, and DEC modes with no effect on the grid
            // (cursor keys, cursor blink, bracketed paste, focus reports).
            24 => {
                let m = r.pick(&["25", "25", "1", "12", "2004", "1004"]);
                csi(&format!("?{m}"), if r.chance(1, 2) { 'h' } else { 'l' })
            }
            // DECSTBM: valid regions, empty and inverted ones (ignored), a
            // bottom past the screen (clamped), and the bare reset.
            25 => {
                let t = match r.below(4) {
                    0 => csi("", 'r'),
                    _ => csi(&format!("{};{}", num(r, rows), num(r, rows)), 'r'),
                };
                return (Kind::Region, t);
            }
            // Save and restore the cursor: a restore only where [`Track`]
            // admits one, a save otherwise.
            26 => {
                return if !self.track.admits(Kind::Restore, caps) || r.chance(1, 2) {
                    (
                        Kind::Save,
                        r.pick(&[b"\x1b7".as_slice(), b"\x1b[s"]).to_vec(),
                    )
                } else {
                    let t = r.pick(&[b"\x1b8".as_slice(), b"\x1b[u"]).to_vec();
                    (Kind::Restore, after_restore(caps, t))
                };
            }
            27 if caps.index_ops => r.pick(&[b"\x1bD".as_slice(), b"\x1bE", b"\x1bM"]).to_vec(),
            28 if caps.insert_delete => {
                let f = r.pick(&['@', 'P', 'L', 'M', 'X']);
                csi(&num(r, lim), f)
            }
            // Sequences both consume without touching the grid, cursor or
            // pen: an OSC title (BEL- and ST-terminated), OSC 133 marks, the
            // ASCII charset designation (which undoes a DEC Special Graphics
            // one, where the fold models both), keypad modes, XTMODKEYS, DECSCUSR, a
            // DECRQSS query (aterm answers it; the answer is not on the
            // screen), PM, SOS. [subset] Not OSC 133 `A`: on the alternate
            // screen aterm reads a prompt mark as the app having died and
            // leaves it (the `osc133_a_leaves_an_orphaned_alt_screen`
            // exclusion).
            29 | 30 => r
                .pick(&[
                    b"\x1b]0;a title\x07".as_slice(),
                    b"\x1b]2;st title\x1b\\",
                    b"\x1b]133;B\x07",
                    b"\x1b]133;C\x07",
                    b"\x1b(B",
                    b"\x1b=",
                    b"\x1b>",
                    b"\x1b[>4;2m",
                    b"\x1b[2 q",
                    b"\x1bP$qm\x1b\\",
                    b"\x1b^private\x1b\\",
                    b"\x1bXsos\x1b\\",
                ])
                .to_vec(),
            // RIS, rarely: it resets everything both engines model.
            31 if r.chance(1, 4) => return (Kind::Reset, b"\x1bc".to_vec()),
            // Origin mode (DECOM): set more often than reset, so the
            // addressing that follows runs under it.
            32 if caps.origin_mode => {
                let set = r.chance(2, 3);
                return (Kind::Origin { set }, csi("?6", if set { 'h' } else { 'l' }));
            }
            // Insert mode (IRM) and newline mode (LNM), alone and together.
            33 if caps.insert_mode => {
                let modes: &[&str] = if caps.newline_mode {
                    &["4", "4", "20", "4;20", "20;4"]
                } else {
                    &["4"]
                };
                csi(r.pick(modes), if r.chance(1, 2) { 'h' } else { 'l' })
            }
            // SU and SD.
            34 if caps.scroll_up_down => csi(&num(r, rows), r.pick(&['S', 'T'])),
            // Character sets: ASCII or DEC Special Graphics into G0 or G1, SO
            // and SI, and now and then a designation and a glyph run in one
            // token. [subset] Never G2/G3, a single shift or a national set:
            // the fold models none of them.
            35 if caps.charsets => {
                let mut t = match r.below(4) {
                    0 | 1 => {
                        format!("\x1b{}{}", r.pick(&['(', ')']), r.pick(&['B', '0'])).into_bytes()
                    }
                    2 => r.pick(&[b"\x0e".as_slice(), b"\x0f"]).to_vec(),
                    _ => b"\x1b(0".to_vec(),
                };
                if r.chance(1, 3) {
                    let n = run_len(r, cols);
                    t.extend(text(r, n));
                }
                t
            }
            _ => {
                let n = run_len(r, cols);
                text(r, n)
            }
        };
        (Kind::Plain, plain)
    }
}

/// The program for `seed`.
pub fn program(seed: u64, caps: Caps) -> Program {
    let mut r = Rng::new(seed);
    let cols = r.pick(&COLS);
    let rows = r.pick(&ROWS);
    let n = 1 + r.below(48) as usize;
    let mut g = Gen {
        r,
        cols,
        rows,
        caps,
        track: Track::default(),
    };
    let mut tokens = Vec::with_capacity(n);
    for _ in 0..n {
        let (kind, t) = g.token();
        g.track.step(kind);
        // One token in four is fed in two chunks, split anywhere: inside an
        // escape, an OSC payload, or a multibyte glyph.
        let chunks = if t.len() > 1 && g.r.chance(1, 4) {
            let at = 1 + g.r.below(t.len() as u64 - 1) as usize;
            vec![t[..at].to_vec(), t[at..].to_vec()]
        } else {
            vec![t]
        };
        tokens.push(Token { kind, chunks });
    }
    let p = Program { cols, rows, tokens };
    debug_assert!(p.in_subset(caps), "the generator left its own subset");
    p
}

/// Where two engines first disagreed while running a program.
#[derive(Clone, Debug)]
pub struct Mismatch {
    /// Index of the token whose chunk exposed it.
    pub token: usize,
    pub what: String,
    pub astream: Snap,
    pub aterm: Snap,
}

/// Run `p` through both engines, comparing after every chunk.
pub fn run(p: &Program) -> Option<Mismatch> {
    let mut a = Astream::new(p.cols, p.rows);
    let mut t = Aterm::new(p.cols, p.rows);
    let (sa, st) = (a.snap(), t.snap());
    if let Some(what) = sa.first_difference(&st) {
        return Some(Mismatch {
            token: 0,
            what: format!("before any input: {what}"),
            astream: sa,
            aterm: st,
        });
    }
    for (i, tok) in p.tokens.iter().enumerate() {
        for chunk in &tok.chunks {
            a.feed(chunk);
            t.feed(chunk);
            let (sa, st) = (a.snap(), t.snap());
            if let Some(what) = sa.first_difference(&st) {
                return Some(Mismatch {
                    token: i,
                    what,
                    astream: sa,
                    aterm: st,
                });
            }
        }
    }
    None
}

/// Cut a failing program down: drop everything after the failing token, then
/// drop tokens one at a time while it still fails, then merge each token's
/// chunks where that keeps it failing. Cheap, since every program is short.
///
/// A candidate counts only if it is still in the subset
/// ([`Program::in_subset`]): dropping a `?1049l` or a RIS can make a later
/// `?1049h` a second entry, and dropping a DECSC can leave a DECRC reading an
/// alternate slot the pinned fold forgets, and either would rebuild a
/// difference the grammar keeps out. Inside the subset every mismatch is a
/// real divergence, so a shrink may trade the first one for another and
/// still point at a bug; the report keeps the first one too.
pub fn shrink(p: &Program, first: &Mismatch, caps: Caps) -> (Program, Mismatch) {
    let mut best = p.clone();
    best.tokens.truncate(first.token + 1);
    let mut why = run(&best).unwrap_or_else(|| first.clone());
    let mut i = 0;
    while i < best.tokens.len() {
        let mut cand = best.clone();
        cand.tokens.remove(i);
        match cand.in_subset(caps).then(|| run(&cand)).flatten() {
            Some(m) => {
                // Nothing after the token that now fails can matter.
                cand.tokens.truncate(m.token + 1);
                best = cand;
                why = m;
            }
            None => i += 1,
        }
    }
    for i in 0..best.tokens.len() {
        if best.tokens[i].chunks.len() > 1 {
            let mut cand = best.clone();
            cand.tokens[i].chunks = vec![cand.tokens[i].chunks.concat()];
            if let Some(m) = run(&cand) {
                best = cand;
                why = m;
            }
        }
    }
    (best, why)
}

/// `bytes` as a Rust byte-string literal, ready to paste into a test.
pub fn literal(bytes: &[u8]) -> String {
    let mut s = String::from("b\"");
    for &b in bytes {
        match b {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            0x20..=0x7e => s.push(b as char),
            _ => {
                let _ = write!(s, "\\x{b:02x}");
            }
        }
    }
    s.push('"');
    s
}

/// The failure report: seed, grid, where the generated program first
/// diverged, the shrunk program as a literal (with its chunk boundaries when
/// they matter), and both screens at its divergence.
pub fn report(seed: u64, original: (&Program, &Mismatch), p: &Program, m: &Mismatch) -> String {
    let chunked = p.tokens.iter().any(|t| t.chunks.len() > 1);
    let mut out = format!(
        "seed {seed:#018x}: {}x{} (cols x rows); the generated program ({} tokens) \
         first diverged at token {}: {}\n  shrunk to {} tokens, divergence at token {}: \
         {}\n  repro: {}\n",
        p.cols,
        p.rows,
        original.0.tokens.len(),
        original.1.token,
        original.1.what,
        p.tokens.len(),
        m.token,
        m.what,
        literal(&p.bytes()),
    );
    if chunked {
        let parts: Vec<String> = p
            .tokens
            .iter()
            .flat_map(|t| &t.chunks)
            .map(|c| literal(c))
            .collect();
        let _ = writeln!(out, "  fed as chunks: [{}]", parts.join(", "));
    }
    let _ = write!(
        out,
        "  astream-term (cursor {:?}):\n{}  aterm (cursor {:?}):\n{}",
        m.astream.cursor,
        m.astream.text_block(),
        m.aterm.cursor,
        m.aterm.text_block()
    );
    out
}
