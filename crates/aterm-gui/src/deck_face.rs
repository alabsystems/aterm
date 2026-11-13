// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stats deck's face, which every deck painter and the lens measure with: what a word
//! measures in the chrome's UI faces ([`FaceMetrics`]), which of a text's forms fits its room
//! (rule F), and the boxes a drawn deck records for its layout tests ([`Boxes`]).

use std::sync::Arc;

use aterm_render::font::Font;

use crate::tray_raster::UnicodeChars;
use crate::widget::TextFace;

/// A glyph's ink above the baseline, per em: the box T-G4 checks.
pub(crate) const ASCENT_EM: f64 = 0.76;
/// A descender's depth below the baseline, per em (§3.1: "descent ≈ 2 pt" at 9.5 pt).
pub(crate) const DESCENT_EM: f64 = 0.22;

fn deck_glyphs() -> impl Iterator<Item = char> {
    (' '..='~').chain(['·', '×', '–', '’', '…'])
}

/// The units per em of [`HELVETICA_NEUE`]'s advances.
#[cfg(test)]
const HELVETICA_NEUE_UPEM: f64 = 1000.0;

/// Helvetica Neue's advances in font units, `(char, regular, Medium)`: the
/// macOS chrome's `TextFace::Ui` and `TextFace::UiBold`
/// (`/System/Library/Fonts/HelveticaNeue.ttc` faces 0 and 10), for every
/// char of [`deck_glyphs`].
#[cfg(test)]
const HELVETICA_NEUE: [(char, u16, u16); 100] = [
    (' ', 278, 278),
    ('!', 259, 278),
    ('"', 426, 444),
    ('#', 556, 556),
    ('$', 556, 556),
    ('%', 1000, 1000),
    ('&', 630, 648),
    ('\'', 278, 278),
    ('(', 259, 278),
    (')', 259, 278),
    ('*', 352, 370),
    ('+', 600, 600),
    (',', 278, 278),
    ('-', 389, 389),
    ('.', 278, 278),
    ('/', 333, 352),
    ('0', 556, 556),
    ('1', 556, 556),
    ('2', 556, 556),
    ('3', 556, 556),
    ('4', 556, 556),
    ('5', 556, 556),
    ('6', 556, 556),
    ('7', 556, 556),
    ('8', 556, 556),
    ('9', 556, 556),
    (':', 278, 278),
    (';', 278, 278),
    ('<', 600, 600),
    ('=', 600, 600),
    ('>', 600, 600),
    ('?', 556, 556),
    ('@', 800, 800),
    ('A', 648, 667),
    ('B', 685, 704),
    ('C', 722, 722),
    ('D', 704, 722),
    ('E', 611, 630),
    ('F', 574, 593),
    ('G', 759, 759),
    ('H', 722, 722),
    ('I', 259, 278),
    ('J', 519, 537),
    ('K', 667, 685),
    ('L', 556, 574),
    ('M', 871, 889),
    ('N', 722, 722),
    ('O', 760, 760),
    ('P', 648, 667),
    ('Q', 760, 760),
    ('R', 685, 704),
    ('S', 648, 648),
    ('T', 574, 593),
    ('U', 722, 722),
    ('V', 611, 611),
    ('W', 926, 944),
    ('X', 611, 648),
    ('Y', 648, 648),
    ('Z', 611, 630),
    ('[', 259, 296),
    ('\\', 333, 352),
    (']', 259, 296),
    ('^', 600, 600),
    ('_', 500, 500),
    ('`', 222, 241),
    ('a', 537, 556),
    ('b', 593, 611),
    ('c', 537, 556),
    ('d', 593, 611),
    ('e', 537, 556),
    ('f', 296, 315),
    ('g', 574, 593),
    ('h', 556, 574),
    ('i', 222, 241),
    ('j', 222, 241),
    ('k', 519, 537),
    ('l', 222, 241),
    ('m', 853, 870),
    ('n', 556, 574),
    ('o', 574, 593),
    ('p', 593, 611),
    ('q', 593, 611),
    ('r', 333, 352),
    ('s', 500, 519),
    ('t', 315, 333),
    ('u', 556, 574),
    ('v', 500, 519),
    ('w', 758, 778),
    ('x', 518, 537),
    ('y', 500, 519),
    ('z', 480, 500),
    ('{', 333, 296),
    ('|', 222, 222),
    ('}', 333, 296),
    ('~', 600, 600),
    ('·', 278, 278),
    ('×', 600, 600),
    ('–', 500, 500),
    ('’', 278, 278),
    ('…', 1000, 1000),
];

/// Glyph advance and kerning metrics for the chrome UI regular and bold faces.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FaceMetrics {
    /// `(char, regular, bold)` advances per em, in char order.
    advances: Vec<(char, f64, f64)>,
    /// The `(regular, bold)` advances of printable ASCII from `advances`, by
    /// [`ascii_slot`]: nearly every char the deck measures, looked up without a search.
    ascii: [Option<(f64, f64)>; ASCII_CHARS],
    /// `((left, right), regular, bold)` kerning per em of the pairs whose
    /// kerning is not zero, in pair order.
    kerns: Vec<((char, char), f64, f64)>,
    /// What the pen adds to each advance of a char the table has, per em
    /// (`tray_raster::UI_TRACKING_EM`).
    tracking: f64,
    /// What a char the table lacks measures: its widest advance, and never
    /// less than the pen's 0.6 em placeholder, so a word in other glyphs is
    /// not planned short.
    missing: f64,
}

/// How many chars printable ASCII (`' '..='~'`) has.
const ASCII_CHARS: usize = 95;

/// `ch`'s place in [`FaceMetrics`]' ASCII table, when it is printable ASCII.
fn ascii_slot(ch: char) -> Option<usize> {
    (' '..='~')
        .contains(&ch)
        .then(|| ch as usize - ' ' as usize)
}

impl FaceMetrics {
    fn from_rows(rows: impl Iterator<Item = (char, f64, f64)>) -> Self {
        let mut advances: Vec<_> = rows.collect();
        advances.sort_by_key(|&(ch, _, _)| ch);
        let mut ascii = [None; ASCII_CHARS];
        for &(ch, regular, bold) in &advances {
            if let Some(slot) = ascii_slot(ch).and_then(|at| ascii.get_mut(at)) {
                *slot = Some((regular, bold));
            }
        }
        let missing = advances
            .iter()
            .map(|&(_, regular, bold)| regular.max(bold))
            .fold(0.6, f64::max);
        Self {
            advances,
            ascii,
            kerns: Vec::new(),
            tracking: 0.0,
            missing,
        }
    }

    /// Returns embedded metrics for macOS Helvetica Neue.
    #[cfg(test)]
    pub(crate) fn helvetica_neue() -> Self {
        Self::from_rows(HELVETICA_NEUE.iter().map(|&(ch, regular, bold)| {
            (
                ch,
                f64::from(regular) / HELVETICA_NEUE_UPEM,
                f64::from(bold) / HELVETICA_NEUE_UPEM,
            )
        }))
    }

    /// Extracts [`FaceMetrics`] from installed regular and bold [`Font`]s.
    pub(crate) fn of_faces(regular: &Font, bold: &Font) -> Self {
        let mut m = Self::advances_of(regular, bold);
        let glyphs: Vec<(char, u16, u16)> = m
            .advances
            .iter()
            .map(|&(ch, _, _)| {
                (
                    ch,
                    regular.unicode_glyph_index(ch),
                    bold.unicode_glyph_index(ch),
                )
            })
            .collect();
        for &(left, lr, lb) in &glyphs {
            for &(right, rr, rb) in &glyphs {
                let (r, b) = (em_kern(regular, lr, rr), em_kern(bold, lb, rb));
                if r != 0.0 || b != 0.0 {
                    m.kerns.push(((left, right), r, b));
                }
            }
        }
        m.tracking = f64::from(crate::tray_raster::UI_TRACKING_EM);
        m
    }

    fn advances_of(regular: &Font, bold: &Font) -> Self {
        Self::from_rows(
            deck_glyphs()
                .filter(|&ch| regular.maps(ch) && bold.maps(ch))
                .map(|ch| (ch, em_advance(regular, ch), em_advance(bold, ch))),
        )
    }

    /// Returns placeholder metrics with 0.6 em fixed advances.
    pub(crate) fn placeholder() -> Self {
        Self::from_rows(std::iter::empty())
    }

    /// Returns metrics for the embedded DejaVu Sans Mono test font.
    #[cfg(test)]
    pub(crate) fn test_stack() -> Self {
        let (_, face) = aterm_render::shared_parsed_face(aterm_render::embedded_font(), 0)
            .expect("the embedded DejaVu Sans Mono parses");
        Self::advances_of(&face, &face)
    }

    /// Measures the advance width of `s` at `pt` in regular or bold.
    pub(crate) fn width(&self, s: &str, pt: f64, bold: bool) -> f64 {
        let mut em = 0.0;
        let mut prev: Option<char> = None;
        for ch in s.chars() {
            match self.advance(ch, bold) {
                Some(advance) => {
                    em += advance + self.tracking;
                    if let Some(left) = prev {
                        em += self.kern(left, ch, bold);
                    }
                    prev = Some(ch);
                }
                None => {
                    em += self.missing;
                    prev = None;
                }
            }
        }
        em * pt
    }

    fn advance(&self, ch: char, bold: bool) -> Option<f64> {
        let (regular, heavy) = match ascii_slot(ch) {
            Some(at) => self.ascii.get(at).copied().flatten()?,
            None => self
                .advances
                .binary_search_by_key(&ch, |&(c, _, _)| c)
                .ok()
                .and_then(|at| self.advances.get(at))
                .map(|&(_, regular, heavy)| (regular, heavy))?,
        };
        Some(if bold { heavy } else { regular })
    }

    fn kern(&self, left: char, right: char, bold: bool) -> f64 {
        self.kerns
            .binary_search_by_key(&(left, right), |&(pair, _, _)| pair)
            .ok()
            .and_then(|at| self.kerns.get(at))
            .map_or(
                0.0,
                |&(_, regular, heavy)| if bold { heavy } else { regular },
            )
    }

    /// Returns the first candidate in `forms` that fits `room`, or truncates the last with `…`.
    pub(crate) fn fit(&self, forms: &[&str], room: f64, pt: f64, bold: bool) -> String {
        if let Some(form) = forms
            .iter()
            .find(|form| self.width(form, pt, bold) <= room + 0.01)
        {
            return (*form).to_owned();
        }
        let Some(last) = forms.last() else {
            return String::new();
        };
        let cuts = last.char_indices().map(|(at, _)| at).rev();
        cuts.map(|at| last.get(..at).unwrap_or_default().trim_end())
            .filter(|head| !head.is_empty())
            .map(|head| format!("{head}…"))
            .find(|cut| self.width(cut, pt, bold) <= room)
            .unwrap_or_default()
    }
}

fn em_advance(font: &Font, ch: char) -> f64 {
    let upem = (1.0 / font.scale_factor(1.0)).round();
    f64::from(font.char_metrics(ch, upem).advance_width) / f64::from(upem)
}

fn em_kern(font: &Font, left: u16, right: u16) -> f64 {
    let upem = (1.0 / font.scale_factor(1.0)).round();
    font.horizontal_kern_indexed(left, right, upem)
        .map_or(0.0, |k| f64::from(k) / f64::from(upem))
}

/// The chrome face's advances the deck measures with: the UI faces the rasterizer draws `TextFace::Ui` and `UiBold` in, or the pen's 0.6 em placeholder while none is installed.
pub(crate) fn deck_metrics() -> Arc<FaceMetrics> {
    type Held = (Option<Arc<Font>>, Option<Arc<Font>>, Arc<FaceMetrics>);
    thread_local! {
        static HELD: std::cell::RefCell<Option<Held>> = const { std::cell::RefCell::new(None) };
    }
    let regular = crate::tray_raster::ui_font_of(TextFace::Ui);
    let bold = crate::tray_raster::ui_font_of(TextFace::UiBold);
    let same = |a: &Option<Arc<Font>>, b: &Option<Arc<Font>>| match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    HELD.with(|held| {
        let mut held = held.borrow_mut();
        if let Some((r, b, m)) = held.as_ref()
            && same(r, &regular)
            && same(b, &bold)
        {
            return Arc::clone(m);
        }
        let metrics = Arc::new(match (&regular, &bold) {
            (Some(r), Some(b)) => FaceMetrics::of_faces(r, b),
            _ => FaceMetrics::placeholder(),
        });
        *held = Some((regular, bold, Arc::clone(&metrics)));
        metrics
    })
}

/// A text and its advance at 1 pt in the deck's face, regular and bold:
/// measured once, so fitting it at any size is arithmetic.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Txt {
    pub(crate) s: String,
    em: [f64; 2],
}

impl Txt {
    /// Measures `s` in `m`.
    pub(crate) fn new(s: impl Into<String>, m: &FaceMetrics) -> Self {
        let s = s.into();
        let em = [m.width(&s, 1.0, false), m.width(&s, 1.0, true)];
        Self { s, em }
    }

    /// Its advance at `pt`, regular or bold.
    pub(crate) fn w(&self, pt: f64, bold: bool) -> f64 {
        self.em[usize::from(bold)] * pt
    }
}

/// Measures each of a text's forms ([`Txt::new`]), in the order given.
pub(crate) fn txts(forms: impl IntoIterator<Item = String>, m: &FaceMetrics) -> Vec<Txt> {
    forms.into_iter().map(|s| Txt::new(s, m)).collect()
}

/// Rule F: the first of `forms` that fits `room` at `pt`, else the last cut with `…`.
pub(crate) fn fit(forms: &[Txt], room: f64, pt: f64, bold: bool, m: &FaceMetrics) -> String {
    if let Some(t) = forms.iter().find(|t| t.w(pt, bold) <= room + 0.01) {
        return t.s.clone();
    }
    forms
        .last()
        .map_or_else(String::new, |last| m.fit(&[&last.s], room, pt, bold))
}

/// What T-G4 records of a drawn deck: one text, mark or region, in points.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Boxed {
    pub(crate) kind: BoxKind,
    pub(crate) text: String,
    /// `x0, y0, x1, y1`.
    pub(crate) r: [f64; 4],
    /// The column a text must stay inside, where it has one.
    pub(crate) room: Option<(f64, f64)>,
}

/// What a recorded box is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoxKind {
    Text,
    Mark,
    /// A strip's cells and their over caps' room.
    Strip,
    Bar,
    /// Disk's warn line, past the bar's top and bottom.
    Tick,
    /// A header swatch or a key item's cell or ramp.
    Swatch,
    /// A cluster's squares.
    Core,
    /// The strain gauge's dial, its face's square.
    Gauge,
}

/// The boxes a deck painter records while a test asks for them (`Boxes::record`): outside
/// tests it holds and records nothing.
#[derive(Default)]
pub(crate) struct Boxes {
    #[cfg(test)]
    boxes: Option<Vec<Boxed>>,
}

impl Boxes {
    /// Records every box noted from now on.
    #[cfg(test)]
    pub(crate) fn record(&mut self) {
        self.boxes = Some(Vec::new());
    }

    /// The boxes recorded, none unless [`Self::record`] asked for them.
    #[cfg(test)]
    pub(crate) fn take(&mut self) -> Vec<Boxed> {
        self.boxes.take().unwrap_or_default()
    }

    /// Records a box of `kind` at `r`, with its text and the room it must stay inside.
    #[cfg(test)]
    pub(crate) fn note(
        &mut self,
        kind: BoxKind,
        r: [f64; 4],
        text: &str,
        room: Option<(f64, f64)>,
    ) {
        if let Some(boxes) = self.boxes.as_mut() {
            boxes.push(Boxed {
                kind,
                text: text.to_owned(),
                r,
                room,
            });
        }
    }

    /// The boxes T-G4 checks are recorded only under test.
    #[cfg(not(test))]
    pub(crate) const fn note(&mut self, _: BoxKind, _: [f64; 4], _: &str, _: Option<(f64, f64)>) {}
}

#[cfg(test)]
#[path = "deck_face_tests.rs"]
mod tests;
