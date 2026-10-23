// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seeded random program generator, oracle comparison, and failure shrinker.

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
    /// `CSI ? 6 h` (`set`) or `l`.
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

    /// Whether every token is one the grammar could have put there.
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
            Kind::Alt { mode: 47, .. } if !caps.alt_persists => false,
            Kind::Alt {
                mode: 1049,
                set: true,
            } if self.in_alt => caps.alt_clears,
            Kind::Alt { mode, set: true } if !self.in_alt && self.alt_dropped => {
                mode == 1049 || !caps.alt_persists
            }
            Kind::Alt { mode, set: true }
                if !self.in_alt && self.ris_dropped_buffer && caps.ris_keeps_alt =>
            {
                mode == 1049
            }
            Kind::Alt {
                mode: 1049,
                set: false,
            } => self.in_alt && !self.main_slot.blocks_restore(),
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
                        self.alt_saved = true;
                        self.ris_dropped_slot = false;
                    }
                    if set {
                        self.alt_dropped = false;
                        self.ris_dropped_buffer = false;
                    } else if self.in_alt {
                        self.alt_dropped = true;
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
/// non-ASCII (Latin-1 letters, a box-drawing rule).
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
fn sgr(r: &mut Rng, bg: bool) -> Vec<u8> {
    let mut parts: Vec<String> = Vec::new();
    let n = r.below(4);
    if n == 0 {
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

    /// One token, and its kind.
    fn token(&mut self) -> (Kind, Vec<u8>) {
        let (cols, rows, caps) = (self.cols, self.rows, self.caps);
        let lim = cols.max(rows);
        let r = &mut self.r;
        let plain = match r.below(36) {
            0..=8 => {
                let n = run_len(r, cols);
                text(r, n)
            }
            9..=12 => {
                let mut set: Vec<&[u8]> =
                    vec![b"\r", b"\n", b"\r\n", b"\x08", b"\t", b"\x07", b"\x00"];
                if caps.vt_ff_feed {
                    set.extend_from_slice(&[b"\x0b", b"\x0c"]);
                }
                r.pick(&set).to_vec()
            }
            13..=15 => {
                let f = r.pick(&['A', 'B', 'C', 'D', 'E', 'F']);
                csi(&num(r, lim), f)
            }
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
            24 => {
                let m = r.pick(&["25", "25", "1", "12", "2004", "1004"]);
                csi(&format!("?{m}"), if r.chance(1, 2) { 'h' } else { 'l' })
            }
            25 => {
                let t = match r.below(4) {
                    0 => csi("", 'r'),
                    _ => csi(&format!("{};{}", num(r, rows), num(r, rows)), 'r'),
                };
                return (Kind::Region, t);
            }
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
            31 if r.chance(1, 4) => return (Kind::Reset, b"\x1bc".to_vec()),
            32 if caps.origin_mode => {
                let set = r.chance(2, 3);
                return (Kind::Origin { set }, csi("?6", if set { 'h' } else { 'l' }));
            }
            33 if caps.insert_mode => {
                let modes: &[&str] = if caps.newline_mode {
                    &["4", "4", "20", "4;20", "20;4"]
                } else {
                    &["4"]
                };
                csi(r.pick(modes), if r.chance(1, 2) { 'h' } else { 'l' })
            }
            34 if caps.scroll_up_down => csi(&num(r, rows), r.pick(&['S', 'T'])),
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

/// Cut a failing program down.
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

/// The failure report.
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
