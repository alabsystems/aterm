// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The row's colours and cells: the presence tones ([`tones`]), the same hues
//! as text inks ([`inks`]), and the painted row ([`paint_row`]) on the chrome
//! band's material ([`BandInks`]). The host maps the cells onto its own type.

use super::words::{cells, gap_before, text_presentation};
use super::{Host, SlotKind, Tone, Words};
use crate::MARGIN;
use crate::ink::{BandInks, ForcedPalette, InkedCell, bg_is_light, ensure_contrast, forced_ink};

/// The presence tones: teal for drive (a peer's hand is on the keyboard),
/// amber for wait (a human should look), red for stop (held, or at a limit),
/// violet for a story (something happened while you were away). Each has a
/// light and a dark pair, contrast-floored against the band: [`tones`] at the
/// 3:1 non-text floor a rim and a chip's dot share, [`inks`] at AA 4.5:1 for
/// the words the row paints in them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresenceTones {
    /// A peer's hand.
    pub drive: [u8; 3],
    /// A human should look.
    pub wait: [u8; 3],
    /// Held, or at a limit.
    pub stop: [u8; 3],
    /// Something happened while you were away.
    pub story: [u8; 3],
}

/// The presence tones on the band `c` of a theme whose background is
/// `theme_bg`, or under an OS-forced chrome palette (`forced`) the High
/// Contrast colours: hue is no channel there, so drive is `COLOR_HIGHLIGHT`
/// (its "selected" meaning fits a peer holding the window) and wait / stop /
/// story share the text ink. The row says which in words, and a hold's rim
/// stays doubled with its wash.
#[must_use]
pub fn tones(theme_bg: [u8; 3], c: &BandInks, forced: Option<ForcedPalette>) -> PresenceTones {
    if let Some(hc) = forced {
        let ink = forced_ink(hc.window_text, hc.btn_face);
        return PresenceTones {
            // `COLOR_HIGHLIGHT` is a fill in the HC vocabulary (the selected
            // tab's), so as an ink or a rim it is floored like every other.
            drive: forced_ink(hc.highlight, hc.btn_face),
            wait: ink,
            stop: ink,
            story: ink,
        };
    }
    let (drive, wait, stop, story) = if bg_is_light(theme_bg) {
        (0x001E_9A8A, 0x00B7_811A, 0x00C7_3F2C, 0x006A_5FD0)
    } else {
        (0x002F_B7A6, 0x00E0_A83A, 0x00E0_533F, 0x008B_7FE6)
    };
    PresenceTones {
        drive: ensure_contrast(rgb(drive), c.bar_bg, 3.0),
        wait: ensure_contrast(rgb(wait), c.bar_bg, 3.0),
        stop: ensure_contrast(rgb(stop), c.bar_bg, 3.0),
        story: ensure_contrast(rgb(story), c.bar_bg, 3.0),
    }
}

/// The presence hues as text inks on a band whose ground is `bar_bg`:
/// [`tones`]' hues held to the AA 4.5:1 text floor, for the words
/// [`paint_row`] paints; the rim keeps the 3:1 tones. Under a forced palette
/// only `COLOR_HIGHLIGHT`, a fill the OS never meant as ink, can need the lift.
#[must_use]
pub fn inks(tones: PresenceTones, bar_bg: [u8; 3]) -> PresenceTones {
    const AA: f64 = 4.5;
    PresenceTones {
        drive: ensure_contrast(tones.drive, bar_bg, AA),
        wait: ensure_contrast(tones.wait, bar_bg, AA),
        stop: ensure_contrast(tones.stop, bar_bg, AA),
        story: ensure_contrast(tones.story, bar_bg, AA),
    }
}

/// A packed `0x00RRGGBB` colour as sRGB bytes.
const fn rgb(c: u32) -> [u8; 3] {
    let [_, r, g, b] = c.to_be_bytes();
    [r, g, b]
}

/// The cells of a `cols`-wide row that carry text: the row keeps one margin
/// cell each side, and the `chrome` line and the a11y detail fit the words
/// at this width, so they never report a slot past the margin.
#[must_use]
pub const fn text_cols(cols: usize) -> usize {
    cols.saturating_sub(2 * MARGIN)
}

/// Paint the row ([`Words`]) at `cols`: the slots laid out by
/// [`Words::pieces`] on the band's material `c`, each in its own ink: the
/// role in `value`, a `since`/fabric figure in `label`, the hand in the drive
/// hue while a peer types (the rim's family at the text floor, `p` from
/// [`inks`]), `⊘ hold` in stop, the phase in warn under `Tone::Warn` and in
/// the drive hue under `Tone::Success`. Glyphs with an emoji form are pinned
/// to text presentation (one cell). Exactly `cols` cells, no closing seam.
///
/// Each word takes the cells the host's grid gives it ([`Host::columns`]): a
/// wide glyph is its lead cell plus a continuation
/// ([`InkedCell::continuation`]); a character the grid folds into the cell
/// before it (a combining mark, a variation selector, the rest of a ZWJ
/// sequence or flag) takes no cell and is not drawn, since a host row has no
/// cluster lane, while the row's text (`chrome`, the a11y sentence) keeps it.
/// A wide glyph that would straddle the row's end is not painted at all.
#[must_use]
pub fn paint_row<V: Host>(
    words: &Words,
    cols: usize,
    c: &BandInks,
    p: &PresenceTones,
) -> Vec<InkedCell> {
    let mut row = vec![
        InkedCell {
            ch: ' ',
            fg: c.label,
            bg: c.bar_bg,
            bold: false,
            text_presentation: false,
            continuation: false,
        };
        cols
    ];
    let pieces = words.pieces::<V>(text_cols(cols));
    let mut col = MARGIN;
    for (i, (kind, text)) in pieces.iter().enumerate() {
        col += gap_before(i, *kind);
        let (ink, bold) = match kind {
            SlotKind::Role => (c.value, true),
            SlotKind::Phase => match words.tone {
                Tone::Warn => (c.warn, false),
                Tone::Success => (p.drive, false),
                Tone::Info => (c.value, false),
            },
            SlotKind::Since => (c.label, false),
            SlotKind::Hand => {
                if text.starts_with('\u{2298}') {
                    (p.stop, true)
                } else if text.starts_with('\u{25c2}') || text.starts_with('\u{25b8}') {
                    (p.drive, false)
                } else {
                    (c.label, false)
                }
            }
            SlotKind::Mail => (c.value, false),
            SlotKind::Ctx => {
                if text.ends_with('\u{26a0}') {
                    (c.warn, false)
                } else {
                    (c.value, false)
                }
            }
            SlotKind::Fabric => {
                if text.starts_with('\u{2715}') {
                    (p.stop, false)
                } else if text.starts_with('~') {
                    (c.warn, false)
                } else {
                    (c.label, false)
                }
            }
        };
        for cell in cells::<V>(text) {
            if let Some([lead, rest @ ..]) = row.get_mut(col..col + cell.width) {
                *lead = InkedCell {
                    ch: cell.ch,
                    fg: ink,
                    bg: c.bar_bg,
                    bold,
                    // The glyphs that have an emoji form are pinned to one
                    // cell ([`text_presentation`]).
                    text_presentation: text_presentation(cell.ch),
                    continuation: false,
                };
                for k in rest {
                    *k = InkedCell {
                        ch: ' ',
                        fg: ink,
                        bg: c.bar_bg,
                        bold,
                        text_presentation: false,
                        continuation: true,
                    };
                }
            }
            col += cell.width;
        }
    }
    row
}
