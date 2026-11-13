// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Harness launch-card overlay on the terminal glass.
//!
//! Decorates supervised launch cards with `aterm_effects::harness_card` fx layers and
//! an undoable status line ([`CardLine`]: `enhanced by ALab <cli> harness`, the same
//! under every Autopilot mode, whose palette alone the mode sets). The line is the one
//! thing the harness writes under a card: every row below it is the vendor's, on the
//! glass as on the grid, and takes its own presses. The line's cells, ink and star are
//! rebuilt only when its key moves, and at rest a settled frame replays them. The mark
//! on the line (`ALab`) is a link ([`GlassHit`]: the hand, a plain press or a
//! link-modifier press opens the site once per double click, a reader's node). Glass
//! only: the grid, copy, `text` reads and recordings keep the vendor's words.
//! [`read_card`] caches card geometry across pane text rebuilds
//! ([`CardReading::holds`]), [`admit`] enforces holder-token, fence, and build
//! provenance ([`GateCheck`]), [`RESHOW_MS`] bounds flash rates, and [`layout`] places
//! the border, kitty, trail, and status line within the pane.

use std::ops::Range;
use std::sync::{Arc, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_core::render::{
    BandIcon, ChromeIcon, ChromeRaster, DecoBlend, DecoGlyph, FxLayer, GlowQuad, InkCell, PxRect,
    RainHalo, RenderInput, WordDecoration,
};
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_effects::harness_card::{
    CardArts, CardFrames, CardPalette, CardScene, FRAME_MS, HarnessCard, LineAt, kitty_cols,
    line_star,
};
use aterm_phase::{CardPlace, CliHarness};

use crate::autopilot::AutopilotView;
use crate::{App, WindowId};

const TRAIL_MIN: usize = 8;
const TRAIL_MAX: usize = 20;
const KITTY_MARGIN: usize = 4;
const LINE_LEAD: usize = 2;
/// Emphasized hero word in the card status line (`enhanced by ALab claude harness`): the
/// mark, which the top rule's label leads with too (`ALab v0.5 (v2.1.305)`).
pub(crate) const HERO_WORD: &str = aterm_types::identity::COMPANY;
const FLOW_TAG: u64 = 0xF10F_0000_0000_0000;
const MOTION_TAG: u64 = 0xA417_0000_0000_0000;

const NO_STAR: WordDecoration = WordDecoration {
    row: 0,
    col: 0,
    dx: 0,
    dy: 0,
    glyph: DecoGlyph::Star4,
    blend: DecoBlend::Add,
    color: 0,
    alpha: 0,
};

/// Minimum interval (ms) before re-showing a card hidden by an identity or fence change.
pub(crate) const RESHOW_MS: u64 = 334;
const _: () = assert!(RESHOW_MS as f32 >= 1000.0 / aterm_effects::TWINKLE_FLASH_BOUND_HZ);

/// Minimum unfocused duration (ms) before focus regain replays the card intro.
pub(crate) const RETURN_MS: u64 = 250;

fn moves_focused(focused: bool, away: Option<Instant>) -> bool {
    focused
        || away.is_some_and(|since| {
            card_now().saturating_duration_since(since) < Duration::from_millis(RETURN_MS)
        })
}
/// Launch card placement and parsed build read from a pane's text rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardRead {
    pub(crate) place: CardPlace,
    pub(crate) version: Box<str>,
    pub(crate) build: Option<atpkg::vendor_direct::Version>,
    words: Box<str>,
    whole: CardPlace,
    pub(crate) kind: usize,
    pub(crate) cut: bool,
}

impl CardRead {
    fn words(&self) -> ([&str; 4], usize) {
        let mut out = [""; 4];
        let mut len = 0;
        for (slot, row) in out.iter_mut().zip(self.words.split('\n')) {
            *slot = row;
            len += 1;
        }
        (out, len)
    }
}

/// Previous card reading passed to [`read_card`] across pane text rebuilds.
#[derive(Clone, Debug)]
pub(crate) struct CardBefore {
    pub(crate) card: Arc<CardRead>,
    pub(crate) last: bool,
}

impl CardBefore {
    /// Mark this reading as older than the most recent rebuild.
    pub(crate) fn earlier(self) -> Self {
        Self {
            last: false,
            ..self
        }
    }
}

/// Result of scanning a pane's rows for a launch card ([`read_card`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct CardReading {
    banner: Option<Option<usize>>,
    rows_kind: usize,
    /// Parsed card reading, if a valid launch card was placed.
    pub(crate) read: Option<Arc<CardRead>>,
}

impl CardReading {
    /// Reader kind: `3` Claude Code, `4` agy, `1` Muse Code.
    pub(crate) const fn rows_kind(&self) -> usize {
        self.rows_kind
    }

    /// Whether this reading remains valid after `changed` rows (ascending) were modified.
    pub(crate) fn holds(&self, session: u64, rows: &[String], changed: &[usize]) -> bool {
        let Some(banner) = self.banner else {
            return false;
        };
        crate::reader_guard::read_or_none(session, "harness card", rows, || match self.rows_kind {
            4 => {
                aterm_phase::agy::launch_card_unmoved(rows, banner, changed)
                    && aterm_phase::launch_card_unmoved(rows, None, changed)
            }
            1 => {
                aterm_phase::muse::launch_card_unmoved(rows, banner, changed)
                    && aterm_phase::launch_card_unmoved(rows, None, changed)
                    && aterm_phase::agy::launch_card_unmoved(rows, None, changed)
            }
            _ => match &self.read {
                Some(read) if read.cut => {
                    aterm_phase::launch_card_scrolled_unmoved(rows, &read.place, changed)
                }
                _ => aterm_phase::launch_card_unmoved(rows, banner, changed),
            },
        })
        .unwrap_or(false)
    }
}

/// Read or hold the launch card on `rows` behind the reader panic guard.
pub(crate) fn read_card(session: u64, rows: &[String], before: Option<CardBefore>) -> CardReading {
    #[cfg(test)]
    crate::work_counts::card_read();
    crate::reader_guard::read_or_none(session, "harness card", rows, || {
        let scan = aterm_phase::launch_card_scan(rows);
        if let Some(CardBefore { card, last: true }) = &before
            && card.kind == 3
        {
            let (words, len) = card.words();
            let words = &words[..len];
            if !card.cut && aterm_phase::launch_card_held(rows, scan, &card.place, words) {
                return CardReading {
                    banner: Some(Some(card.place.top)),
                    rows_kind: 3,
                    read: Some(Arc::clone(card)),
                };
            }
            if let Some(place) = aterm_phase::launch_card_scrolled(rows, scan, &card.whole, words) {
                let read = if card.cut && card.place == place {
                    Arc::clone(card)
                } else {
                    Arc::new(CardRead {
                        place,
                        whole: card.whole,
                        version: card.version.clone(),
                        build: card.build,
                        words: card.words.clone(),
                        kind: 3,
                        cut: true,
                    })
                };
                return CardReading {
                    banner: Some(None),
                    rows_kind: 3,
                    read: Some(read),
                };
            }
        }
        let (banner, place, rows_kind) = match scan {
            (None, None) => match aterm_phase::agy::launch_card_scan(rows) {
                (None, None) => {
                    let (b, p) = aterm_phase::muse::launch_card_scan(rows);
                    (b, p, 1usize)
                }
                (b, p) => (b, p, 4usize),
            },
            (b, p) => (b, p, 3usize),
        };
        let read = place.and_then(|place| {
            let version = match rows_kind {
                4 => aterm_phase::agy::launch_card_version(rows, &place)?,
                1 => aterm_phase::muse::launch_card_version(rows, &place)?,
                _ => aterm_phase::launch_card_version(rows, &place)?,
            };
            let claude;
            let other;
            let words: &[&str] = match rows_kind {
                4 => {
                    other = aterm_phase::agy::launch_card_words(rows, &place)?;
                    &other
                }
                1 => {
                    other = aterm_phase::muse::launch_card_words(rows, &place)?;
                    &other
                }
                _ => {
                    claude = aterm_phase::launch_card_words(rows, &place)?;
                    claude.as_slice()
                }
            };
            Some(match before {
                Some(CardBefore { card: was, .. })
                    if was.kind == rows_kind
                        && was.place == place
                        && !was.cut
                        && *was.version == *version
                        && was.words.split('\n').eq(words.iter().copied()) =>
                {
                    was
                }
                _ => Arc::new(CardRead {
                    place,
                    whole: place,
                    version: version.into(),
                    build: atpkg::vendor_direct::Version::parse(version),
                    words: words.join("\n").into(),
                    kind: rows_kind,
                    cut: false,
                }),
            })
        });
        CardReading {
            banner: Some(banner),
            rows_kind,
            read,
        }
    })
    .unwrap_or_default()
}

/// Pane position and dimensions within the rendered frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneGeom {
    pub(crate) strip: usize,
    pub(crate) row_off: usize,
    pub(crate) col_off: usize,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
}

impl PaneGeom {
    const fn frame_row(self, r: usize) -> usize {
        self.strip + self.row_off + r
    }

    const fn frame_col(self, c: usize) -> usize {
        self.col_off + c
    }
}

/// Per-pane card candidate gathered during the footer splice for [`App::compose_claude_cards`].
#[derive(Clone, Debug)]
pub(crate) struct CardLeaf {
    leaf_index: usize,
    session: u64,
    pane: PaneGeom,
    blank: RenderCell,
    view: AutopilotView,
    card: Option<(Arc<CardRead>, u64, u64)>,
    bare: Option<Bare>,
    token: u64,
    row_base: i64,
    bare_row: Option<i64>,
    drawerless: bool,
    mutations: u64,
}

/// Undoable status line cell edit, ink, and leading star under a decorated card.
#[derive(Clone, Debug)]
pub(crate) struct CardLine {
    on: bool,
    key: Option<LineKey>,
    settled: bool,
    step: Option<u64>,
    frame_row: usize,
    term_row: usize,
    col: usize,
    hero: Range<u16>,
    /// The frame columns of the mark, the line's link ([`line_link`]).
    link: Range<usize>,
    cells: Vec<RenderCell>,
    ground: RenderCell,
    ink: Vec<InkCell>,
    star: WordDecoration,
}

impl Default for CardLine {
    fn default() -> Self {
        Self {
            on: false,
            key: None,
            settled: false,
            step: None,
            frame_row: 0,
            term_row: 0,
            col: 0,
            hero: 0..0,
            link: 0..0,
            cells: Vec::new(),
            ground: RenderCell::default(),
            ink: Vec::new(),
            star: NO_STAR,
        }
    }
}

impl CardLine {
    /// Borrowed `(frame_row, col, cells, ground)` edit for `claude_footer::write_edits`.
    pub(crate) fn edit(&self) -> (usize, usize, &[RenderCell], RenderCell) {
        (self.frame_row, self.col, &self.cells, self.ground)
    }

    /// Starting `(frame_row, col)` key of this edit.
    pub(crate) const fn at(&self) -> (usize, usize) {
        (self.frame_row, self.col)
    }
}

/// Hit-test region of the card line's mark on the glass and the URL a press on it opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GlassHit {
    session: u64,
    frame_row: usize,
    term_row: usize,
    cols: Range<usize>,
    url: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LineKey {
    frame_row: usize,
    star_col: usize,
    text: &'static str,
    base: RenderCell,
    dark: bool,
    palette: CardPalette,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LaidKey {
    pane: PaneGeom,
    blank: RenderCell,
    place: CardPlace,
    view: AutopilotView,
    px: Pixels,
    serious: bool,
    kitty: (u8, u8),
}

/// Per-session decorated card animation, layout cache, and frame state.
#[derive(Debug)]
struct CardEntry {
    leaf_index: usize,
    session: u64,
    identity: u64,
    card: HarnessCard,
    view: AutopilotView,
    armed_at: Instant,
    ticked_at: Instant,
    shown: bool,
    was_shown: bool,
    shown_since: Instant,
    gate_hid: bool,
    deferred: Option<Instant>,
    asked: bool,
    place: (usize, usize, u16, u16),
    glass_box: (usize, usize, usize, usize),
    frames: CardFrames,
    flare: bool,
    shimmer: Option<((usize, usize), u64)>,
    laid: Option<(LaidKey, Option<(Layout<'static>, CardScene)>)>,
    laid_gen: u64,
    seen: Seen,
    line: CardLine,
}

impl CardEntry {
    fn arm(&mut self, now: Instant) {
        let seed = pinned_seed().unwrap_or_else(|| mix(self.session ^ self.identity));
        self.card.arm(now, seed);
        self.armed_at = now;
        self.asked = false;
    }

    /// Next frame deadline wanted by this card at `now`, if any.
    fn due(&self, now: Instant) -> Option<Instant> {
        if let Some(until) = self.deferred {
            return (!self.asked).then_some(until);
        }
        if !self.shown || self.asked {
            return None;
        }
        let wanted = self.card.deadline(now)?;
        if wanted <= now || self.card.flowing(now) {
            return Some(wanted);
        }
        let next = self
            .ticked_at
            .checked_add(Duration::from_millis(FRAME_MS))
            .unwrap_or(wanted);
        Some(next.min(wanted))
    }

    /// Whether this card is shown and still wants animation or deferred-show frames.
    const fn live(&self) -> bool {
        !self.asked && (self.deferred.is_some() || self.shown && !self.card.at_rest())
    }
}

/// Fx layers, glow quads, halos, ink, and decorations appended by the latest splice.
#[derive(Debug, Default)]
pub(crate) struct CardPainted {
    layers: Vec<FxLayer>,
    under: Vec<GlowQuad>,
    halo: Vec<RainHalo>,
    ink: Vec<InkCell>,
    decos: Vec<WordDecoration>,
    icons: Vec<(u16, ChromeIcon)>,
}

impl CardPainted {
    const fn is_empty(&self) -> bool {
        self.layers.is_empty()
            && self.under.is_empty()
            && self.halo.is_empty()
            && self.ink.is_empty()
            && self.decos.is_empty()
            && self.icons.is_empty()
    }

    fn clear(&mut self) {
        self.layers.clear();
        self.under.clear();
        self.halo.clear();
        self.ink.clear();
        self.decos.clear();
        self.icons.clear();
    }
}

/// Append an agents panel's halos, glow quads, and non-overlapping ink to `scratch`.
pub(crate) fn paint_panel(
    painted: &mut CardPainted,
    scratch: &mut RenderInput,
    ink: &[InkCell],
    halo: &[RainHalo],
    under: &[GlowQuad],
) {
    scratch.glow_halo.extend_from_slice(halo);
    painted.halo.extend_from_slice(halo);
    scratch.glow_under.extend_from_slice(under);
    painted.under.extend_from_slice(under);
    let before = scratch.ink.len();
    for cell in ink {
        let key = (cell.row, cell.col);
        if scratch.ink[..before]
            .binary_search_by_key(&key, |c| (c.row, c.col))
            .is_err()
        {
            scratch.ink.push(*cell);
            painted.ink.push(*cell);
        }
    }
    if scratch.ink.len() > before {
        scratch.ink.sort_unstable_by_key(|c| (c.row, c.col));
        painted.ink.sort_unstable_by_key(|c| (c.row, c.col));
    }
}

/// Per-window harness card state, active entries, and reusable scratch buffers.
#[derive(Debug, Default)]
pub(crate) struct WindowCards {
    entries: Vec<CardEntry>,
    painted: CardPainted,
    leaves: Vec<CardLeaf>,
    arts: Option<Arc<CardArts>>,
    keys: Vec<(EntryKey, bool)>,
    seen: Seen,
    spare_quads: Vec<GlowQuad>,
    spare_halos: Vec<RainHalo>,
    spare_ink: Vec<InkCell>,
    spare_decos: Vec<WordDecoration>,
    /// The empty pixel row every icon-only raster shares as its ground and
    /// rail ([`Self::paint_icon`]).
    no_px: Arc<[u32]>,
    panels: crate::claude_panel::WindowPanels,
    glass_hits: Vec<GlassHit>,
    /// The session whose glass link a press last opened, or would have, and when.
    link_pressed: Option<(u64, Instant)>,
    away: Option<Instant>,
    composed: u64,
    outcomes: Vec<(u64, CardOutcome, u64)>,
}

impl WindowCards {
    /// A digest of the glass hits (`glass_hits`) the last frame composed, `0` when none are open.
    pub(crate) fn glass_hits_digest(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        if self.glass_hits.is_empty() {
            return 0;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.glass_hits.len().hash(&mut h);
        for hit in &self.glass_hits {
            (hit.session, hit.frame_row, hit.term_row, &hit.cols).hash(&mut h);
        }
        h.finish() | 1
    }

    /// Recorded card fx layers for test assertions.
    #[cfg(test)]
    pub(crate) fn painted_layers(&self) -> &[FxLayer] {
        &self.painted.layers
    }

    /// Drop layers with `id` from the painted record without removing them from the frame.
    #[cfg(test)]
    pub(crate) fn forget_layers(&mut self, id: u64) {
        self.painted.layers.retain(|l| l.id != id);
    }

    /// Append a non-empty fx `layer` to `scratch` and record it for [`strip`].
    pub(crate) fn paint_layer(&mut self, scratch: &mut RenderInput, layer: FxLayer) {
        if layer.clip.is_empty() || layer.content.items().is_empty() {
            return;
        }
        self.painted.layers.push(layer.clone());
        scratch.fx_layers.push(layer);
    }

    /// Draw each full-size, narrow cell of `cells` (written on frame row
    /// `frame_row` from column `col`) whose character is a glass mark
    /// ([`BandIcon::glass_mark`]) as the chrome's icon of it, the cell keeping
    /// its character; a cell in small script (`Script::Sub`) keeps its glyph.
    /// The footer's write passes every cell it lays on a rule, line D's facts
    /// among them, so their `◆`, `⌂`, `◐`, `⎇` and `⎆` are drawn as the chips'
    /// marks are, whole strokes in their own cells.
    pub(crate) fn paint_marks(
        &mut self,
        scratch: &mut RenderInput,
        frame_row: usize,
        col: usize,
        cells: &[RenderCell],
    ) {
        let Ok(row) = u16::try_from(frame_row) else {
            return;
        };
        for icon in glass_marks(col, cells) {
            self.paint_icon(scratch, row, icon);
        }
    }

    /// Record and draw `icon` on `row`'s chrome raster for [`strip`]. A row
    /// with no raster gets one whose ground and rail are the one shared empty
    /// row and whose icon list is one the refill retired
    /// ([`RenderInput::retire_chrome_rasters`]), so at rest it allocates nothing.
    pub(crate) fn paint_icon(&mut self, scratch: &mut RenderInput, row: u16, icon: ChromeIcon) {
        match scratch
            .chrome_rasters
            .iter_mut()
            .find(|raster| raster.row == row)
        {
            Some(raster) => raster.icons.push(icon),
            None => {
                let mut icons = scratch.chrome_icon_list();
                icons.push(icon);
                scratch.chrome_rasters.push(ChromeRaster {
                    row,
                    ground: Arc::clone(&self.no_px),
                    rail: Arc::clone(&self.no_px),
                    rail_h: 0,
                    clear_rail: false,
                    own: Vec::new(),
                    split: None,
                    rings: Vec::new(),
                    icons,
                });
            }
        }
        self.painted.icons.push((row, icon));
    }

    /// Record a press that opens `session`'s glass link at `now`, and say whether it repeats
    /// the last such press within a double click's interval (`multi_click_ms`), so a double or
    /// triple click opens the link once.
    pub(crate) fn link_press_repeats(&mut self, session: u64, now: Instant) -> bool {
        let repeats = self.link_pressed.is_some_and(|(last, at)| {
            last == session
                && now.saturating_duration_since(at).as_millis() <= crate::multi_click_ms()
        });
        self.link_pressed = Some((session, now));
        repeats
    }

    /// Mark all cards in this window as hidden.
    pub(crate) fn hide_all(&mut self) {
        for entry in &mut self.entries {
            entry.was_shown = entry.shown;
            entry.shown = false;
            entry.line.on = false;
        }
        self.panels.hide_all();
        self.glass_hits.clear();
    }

    /// Borrow the agents panel state and painted record together.
    pub(crate) const fn panels_and_painted(
        &mut self,
    ) -> (&mut crate::claude_panel::WindowPanels, &mut CardPainted) {
        (&mut self.panels, &mut self.painted)
    }

    /// Borrow the agents panel state.
    pub(crate) const fn panels(&self) -> &crate::claude_panel::WindowPanels {
        &self.panels
    }

    /// Mutably borrow the agents panel state.
    pub(crate) const fn panels_mut(&mut self) -> &mut crate::claude_panel::WindowPanels {
        &mut self.panels
    }

    /// Ripple clock origin of an actively flowing card in this window.
    pub(crate) fn flow_origin(&self, now: impl FnOnce() -> Instant) -> Option<Instant> {
        let mut moving = self
            .entries
            .iter()
            .filter(|e| e.shown && !e.card.at_rest())
            .peekable();
        moving.peek()?;
        let now = now();
        moving
            .find(|e| e.card.flowing(now))
            .and_then(|e| e.card.ripple_origin())
    }

    /// Whether this window holds a card entry for `session`.
    pub(crate) fn keeps(&self, session: u64) -> bool {
        self.entries.iter().any(|e| e.session == session)
    }

    /// Whether any card is currently shown in this window.
    pub(crate) fn any_shown(&self) -> bool {
        self.entries.iter().any(|e| e.shown)
    }

    /// Take the reusable pane leaf buffer for the current frame's card pass.
    pub(crate) fn take_leaves(&mut self) -> Vec<CardLeaf> {
        let mut leaves = std::mem::take(&mut self.leaves);
        leaves.clear();
        leaves
    }

    /// Active card status lines to write this frame.
    pub(crate) fn lines(&self) -> impl Iterator<Item = &CardLine> {
        self.entries.iter().map(|e| &e.line).filter(|l| l.on)
    }

    /// Frame-cell bounding boxes of all shown cards for pet-chrome collision avoidance.
    pub(crate) fn glass_boxes(&self) -> impl Iterator<Item = (usize, usize, usize, usize)> + '_ {
        self.entries.iter().filter(|e| e.shown).map(|e| e.glass_box)
    }
}

fn wanted_frame(
    views: &crate::tab_model::ViewStore,
    ws: &crate::WindowState,
    entry: &CardEntry,
    now: Instant,
) -> Option<Instant> {
    entry
        .due(now)
        .filter(|_| crate::active_tab_displays_session(ws, views, entry.session))
}

/// Withdraw layers, glow quads, halos, ink, and stars recorded by the previous splice.
pub(crate) fn strip(cards: &mut WindowCards, scratch: &mut RenderInput) {
    if cards.painted.is_empty() {
        return;
    }
    let (painted, keys) = (&mut cards.painted, &mut cards.keys);
    withdraw_card_layers(&mut scratch.fx_layers, &painted.layers);
    remove_each_once(
        &mut scratch.glow_under,
        &painted.under,
        keys,
        quad_key,
        |q| q.row,
    );
    remove_each_once(&mut scratch.glow_halo, &painted.halo, keys, halo_key, |h| {
        h.row
    });
    remove_each_once(&mut scratch.ink, &painted.ink, keys, ink_key, |c| c.row);
    remove_each_once(
        &mut scratch.word_decorations,
        &painted.decos,
        keys,
        deco_key,
        |d| d.row,
    );
    withdraw_icons(&mut scratch.chrome_rasters, &painted.icons);
    painted.clear();
}

fn withdraw_icons(rasters: &mut [ChromeRaster], ours: &[(u16, ChromeIcon)]) {
    for &(row, icon) in ours {
        if let Some(raster) = rasters.iter_mut().find(|raster| raster.row == row)
            && let Some(k) = raster.icons.iter().position(|i| *i == icon)
        {
            raster.icons.remove(k);
        }
    }
}

/// The glass marks among `cells`, written from frame column `col`, that
/// [`WindowCards::paint_marks`] draws: each full-size, narrow cell whose
/// character is a glass mark ([`BandIcon::glass_mark`]), as the chrome icon
/// drawn in place of its font's glyph; a cell in small script keeps its glyph.
pub(crate) fn glass_marks(
    col: usize,
    cells: &[RenderCell],
) -> impl Iterator<Item = ChromeIcon> + '_ {
    cells.iter().enumerate().filter_map(move |(k, cell)| {
        if cell.wide || cell.script != aterm_core::terminal::Script::Normal {
            return None;
        }
        let icon = BandIcon::glass_mark(cell.ch)?;
        let col = u16::try_from(col + k).ok()?;
        Some(ChromeIcon { col, icon })
    })
}

/// Clip card ink, stars, and panel light out of an overlay menu `rect`.
pub(crate) fn cut_out(
    cards: &mut WindowCards,
    scratch: &mut RenderInput,
    rect: crate::tab_menu::MenuRect,
    cw: usize,
    pad: usize,
) {
    let painted = &cards.painted;
    if painted.ink.is_empty()
        && painted.decos.is_empty()
        && painted.under.is_empty()
        && painted.halo.is_empty()
    {
        return;
    }
    let rows = rect.row..rect.row + rect.h;
    let cols = rect.col..rect.col + rect.w;
    let window_px = (pad + rect.col * cw)..(pad + (rect.col + rect.w) * cw);
    let inside =
        |row: u16, col: u16| rows.contains(&usize::from(row)) && cols.contains(&usize::from(col));
    let WindowCards {
        painted,
        keys,
        spare_quads,
        spare_halos,
        spare_ink,
        spare_decos,
        ..
    } = cards;
    cut_each_once(
        &mut scratch.glow_under,
        &mut painted.under,
        keys,
        spare_quads,
        quad_key,
        |q, out| rows.contains(&usize::from(q.row)) && cut_quad(q, window_px.clone(), out),
    );
    cut_each_once(
        &mut scratch.glow_halo,
        &mut painted.halo,
        keys,
        spare_halos,
        halo_key,
        |h, _| {
            let (x0, x1) = (usize::from(h.x), usize::from(h.x) + usize::from(h.w));
            rows.contains(&usize::from(h.row)) && x1 > window_px.start && x0 < window_px.end
        },
    );
    cut_each_once(
        &mut scratch.ink,
        &mut painted.ink,
        keys,
        spare_ink,
        ink_key,
        |c, _| inside(c.row, c.col),
    );
    cut_each_once(
        &mut scratch.word_decorations,
        &mut painted.decos,
        keys,
        spare_decos,
        deco_key,
        |d, _| inside(d.row, d.col),
    );
}

fn cut_quad(q: &GlowQuad, xs: std::ops::Range<usize>, out: &mut Vec<GlowQuad>) -> bool {
    let (x0, x1) = (usize::from(q.x), usize::from(q.x) + usize::from(q.w));
    if x1 <= xs.start || x0 >= xs.end {
        return false;
    }
    if x0 < xs.start {
        out.push(quad_piece(q, 0, xs.start - x0));
    }
    if x1 > xs.end {
        out.push(quad_piece(q, xs.end - x0, x1 - x0));
    }
    true
}

fn quad_piece(q: &GlowQuad, from: usize, to: usize) -> GlowQuad {
    let w = usize::from(q.w).max(1);
    let at = |a: u32, b: u32, x: usize| {
        let (w, x) = (w as u64, x as u64);
        ((u64::from(a) * (w - x) + u64::from(b) * x + w / 2) / w) as u32
    };
    let rgb = |x: usize| {
        [16, 8, 0]
            .iter()
            .map(|s| at((q.color >> s) & 0xff, (q.color2 >> s) & 0xff, x) << s)
            .fold(0, |acc, c| acc | c)
    };
    let alpha = |x: usize| at(u32::from(q.alpha), u32::from(q.alpha2), x) as u8;
    GlowQuad {
        x: u16_of(usize::from(q.x) + from),
        w: u16_of(to - from),
        color: rgb(from),
        alpha: alpha(from),
        color2: rgb(to),
        alpha2: alpha(to),
        ..*q
    }
}

/// Replace entries from `ours` in `stream` with the pieces left by `cut`.
fn cut_each_once<T: Copy>(
    stream: &mut Vec<T>,
    ours: &mut Vec<T>,
    keys: &mut Vec<(EntryKey, bool)>,
    spare: &mut Vec<T>,
    key: fn(&T) -> EntryKey,
    cut: impl Fn(&T, &mut Vec<T>) -> bool,
) {
    keys.clear();
    spare.clear();
    for entry in ours.iter() {
        if cut(entry, spare) {
            keys.push((key(entry), false));
        } else {
            spare.push(*entry);
        }
    }
    if keys.is_empty() {
        return;
    }
    ours.clear();
    ours.extend_from_slice(spare);
    keys.sort_unstable();
    spare.clear();
    for item in stream.iter() {
        if take_key(keys, key(item)) {
            cut(item, spare);
        } else {
            spare.push(*item);
        }
    }
    stream.clear();
    stream.extend_from_slice(spare);
}

fn take_key(keys: &mut [(EntryKey, bool)], k: EntryKey) -> bool {
    let mut at = keys.partition_point(|(o, _)| *o < k);
    while let Some((o, used)) = keys.get_mut(at)
        && *o == k
    {
        if !*used {
            *used = true;
            return true;
        }
        at += 1;
    }
    false
}

type EntryKey = [u64; 3];

fn quad_key(q: &GlowQuad) -> EntryKey {
    [
        u64::from(q.row) << 48 | u64::from(q.x) << 32 | u64::from(q.y) << 16 | u64::from(q.w),
        u64::from(q.h) << 48 | u64::from(q.alpha) << 40 | u64::from(q.color),
        u64::from(q.alpha2) << 32 | u64::from(q.color2),
    ]
}

const fn halo_key(h: &RainHalo) -> EntryKey {
    let [a, b, c] = h.damage_words();
    [a, b, c]
}

fn ink_key(c: &InkCell) -> EntryKey {
    [
        u64::from(c.row) << 16 | u64::from(c.col),
        u64::from(pack(c.color)),
        0,
    ]
}

fn deco_key(d: &WordDecoration) -> EntryKey {
    [
        u64::from(d.row) << 48
            | u64::from(d.col) << 32
            | u64::from(d.dx.cast_unsigned()) << 24
            | u64::from(d.dy.cast_unsigned()) << 16
            | (d.glyph as u64) << 8
            | d.blend as u64,
        u64::from(d.alpha) << 32 | u64::from(d.color),
        0,
    ]
}

/// Remove one occurrence of each entry in `ours` from `stream`, preserving order.
fn remove_each_once<T: Copy + PartialEq>(
    stream: &mut Vec<T>,
    ours: &[T],
    keys: &mut Vec<(EntryKey, bool)>,
    key: fn(&T) -> EntryKey,
    row: fn(&T) -> u16,
) {
    if ours.is_empty() || stream.is_empty() {
        return;
    }
    if let Some(tail) = stream.len().checked_sub(ours.len())
        && stream[tail..] == *ours
    {
        stream.truncate(tail);
        return;
    }
    let (first, last) = ours
        .iter()
        .map(row)
        .fold((u16::MAX, 0), |(lo, hi), r| (lo.min(r), hi.max(r)));
    if !stream.iter().map(row).any(|r| first <= r && r <= last) {
        return;
    }
    #[cfg(test)]
    crate::work_counts::card_strip_sorted();
    keys.clear();
    keys.extend(ours.iter().map(|o| (key(o), false)));
    keys.sort_unstable();
    stream.retain(|item| !take_key(keys, key(item)));
}

fn append_card_frame(
    painted: &mut CardPainted,
    scratch: &mut RenderInput,
    session: u64,
    frame: aterm_effects::harness_card::CardFrame,
    pane: PxRect,
    grid: (i32, i32),
) {
    for layer in frame.layers(session, pane, grid) {
        if !layer.clip.is_empty() && !layer.content.items().is_empty() {
            painted.layers.push(layer.clone());
            scratch.fx_layers.push(layer);
        }
    }
}

/// Remove one occurrence of each layer in `ours` from `stream` by id, slot, space, and `Arc` identity.
fn withdraw_card_layers(stream: &mut Vec<FxLayer>, ours: &[FxLayer]) {
    if stream.ends_with(ours) {
        stream.truncate(stream.len() - ours.len());
        return;
    }
    for layer in ours.iter().rev() {
        let mine = |other: &FxLayer| {
            other.id == layer.id
                && other.slot == layer.slot
                && other.space == layer.space
                && Arc::ptr_eq(&other.content, &layer.content)
        };
        if let Some(at) = stream.iter().rposition(mine) {
            stream.remove(at);
        }
    }
}

/// Apply ink and leading stars for card lines whose cell edits took, and record their marks'
/// hit regions ([`GlassHit`]).
pub(crate) fn ink_lines(
    cards: &mut WindowCards,
    scratch: &mut RenderInput,
    took: impl Fn((usize, usize)) -> bool,
) {
    let WindowCards {
        entries,
        painted,
        glass_hits,
        ..
    } = cards;
    glass_hits.clear();
    let before = scratch.ink.len();
    let words = entries
        .iter()
        .map(|e| &e.line)
        .filter(|l| l.on && took(l.at()))
        .map(|l| (l.ink.as_slice(), l.star));
    for (ink, star) in words {
        for cell in ink {
            let key = (cell.row, cell.col);
            if scratch.ink[..before]
                .binary_search_by_key(&key, |c| (c.row, c.col))
                .is_err()
            {
                scratch.ink.push(*cell);
                painted.ink.push(*cell);
            }
        }
        scratch.word_decorations.push(star);
        painted.decos.push(star);
    }
    for entry in entries
        .iter()
        .filter(|e| e.line.on && !e.line.link.is_empty() && took(e.line.at()))
    {
        let line = &entry.line;
        glass_hits.push(GlassHit {
            session: entry.session,
            frame_row: line.frame_row,
            term_row: line.term_row,
            cols: line.link.clone(),
            url: aterm_types::identity::SITE_URL,
        });
    }
    if scratch.ink.len() > before {
        scratch.ink.sort_unstable_by_key(|c| (c.row, c.col));
        painted.ink.sort_unstable_by_key(|c| (c.row, c.col));
    }
}

/// Read the card clock (wall clock or thread-local test clock).
pub(crate) fn card_now() -> Instant {
    #[cfg(test)]
    {
        crate::work_counts::harness_clock_read();
        if let Some(now) = TEST_NOW.with(std::cell::Cell::get) {
            return now;
        }
    }
    Instant::now()
}

#[derive(Default)]
struct LazyNow(Option<Instant>);

impl LazyNow {
    fn get(&mut self) -> Instant {
        *self.0.get_or_insert_with(card_now)
    }
}

#[cfg(test)]
thread_local! {
    static TEST_NOW: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Set or clear the thread-local test clock for [`card_now`].
#[cfg(test)]
pub(crate) fn set_test_now(now: Option<Instant>) {
    TEST_NOW.with(|t| t.set(now));
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static SEED_PIN: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Pin or unpin the star constellation seed for newly armed cards.
#[cfg(any(test, debug_assertions))]
pub(crate) fn pin_card_seed(seed: Option<u64>) {
    SEED_PIN.with(|pin| pin.set(seed));
}

fn pinned_seed() -> Option<u64> {
    #[cfg(any(test, debug_assertions))]
    {
        SEED_PIN.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        None
    }
}

const fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Pack `[r, g, b]` into `0x00RRGGBB`.
pub(crate) fn pack(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
}

fn pack_fill((r, g, b): (f32, f32, f32)) -> u32 {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

/// The card line's rungs for the harness of reader kind `rows_kind` (`3` Claude Code, `4` agy,
/// `1` Muse Code), widest first, made once per harness ([`make_line_texts_for`]).
fn line_texts_for(rows_kind: usize) -> &'static [String] {
    static TEXTS: [OnceLock<Vec<String>>; 3] = [const { OnceLock::new() }; 3];
    let (harness, slot) = match rows_kind {
        4 => (CliHarness::Agy, &TEXTS[1]),
        1 => (CliHarness::Muse, &TEXTS[2]),
        _ => (CliHarness::ClaudeCode, &TEXTS[0]),
    };
    slot.get_or_init(|| make_line_texts_for(harness))
}

/// The card line's rungs, widest first, naming `harness` as the card line alone does: `enhanced
/// by ALab <cli> harness`, `ALab <cli> harness`, `ALab`. The line carries no version and no
/// URL: the top rule's label (`ALab v<ver> (v<build>)`) has both versions, and the mark is the
/// link to the site ([`line_link`]). Every rung keeps the mark, so the words around it give way
/// first.
fn make_line_texts_for(harness: CliHarness) -> Vec<String> {
    let named = format!("{HERO_WORD} {} harness", harness.cli());
    vec![format!("enhanced by {named}"), named, HERO_WORD.to_owned()]
}

/// The cells of `text`, a rung of the card line, that are the link to the site: its mark
/// ([`hero_cells`]).
fn line_link(text: &str) -> Option<Range<usize>> {
    hero_cells(text).map(|cells| usize::from(cells.start)..usize::from(cells.end))
}

fn text_cells(text: &str) -> usize {
    text.chars().map(aterm_grapheme::char_width).sum()
}

fn hero_cells(text: &str) -> Option<Range<u16>> {
    let whole = |at: usize| {
        let before = text[..at].chars().next_back();
        let after = text[at + HERO_WORD.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    };
    let at = text
        .match_indices(HERO_WORD)
        .map(|(at, _)| at)
        .find(|&at| whole(at))?;
    let start = u16_of(text_cells(&text[..at]));
    Some(start..start + u16_of(text_cells(HERO_WORD)))
}

/// Blank-cell and wide-continuation queries over a pane's rendered cells.
struct PaneCells<'a> {
    scratch: &'a RenderInput,
    pane: PaneGeom,
    blank: RenderCell,
}

impl<'a> PaneCells<'a> {
    fn row(&self, r: usize) -> PaneRow<'a> {
        let row = self.pane.frame_row(r);
        let held = |entries: Option<usize>| entries.is_some_and(|n| n > 0);
        PaneRow {
            scratch: self.scratch,
            row,
            cells: (r < self.pane.rows)
                .then(|| self.scratch.cells.get(row))
                .flatten()
                .map(Vec::as_slice),
            side: held(self.scratch.clusters.get(row).map(Vec::len))
                || held(self.scratch.combining.get(row).map(Vec::len))
                || held(self.scratch.images.get(row).map(Vec::len)),
            col_off: self.pane.col_off,
            cols: self.pane.cols,
            blank: self.blank,
        }
    }

    fn wide(&self, r: usize, c: usize) -> bool {
        #[cfg(test)]
        crate::work_counts::card_cell_read();
        r < self.pane.rows
            && c < self.pane.cols
            && self
                .scratch
                .cells
                .get(self.pane.frame_row(r))
                .and_then(|cells| cells.get(self.pane.frame_col(c)))
                .is_some_and(|cell| cell.wide)
    }
}

struct PaneRow<'a> {
    scratch: &'a RenderInput,
    row: usize,
    cells: Option<&'a [RenderCell]>,
    side: bool,
    col_off: usize,
    cols: usize,
    blank: RenderCell,
}

impl PaneRow<'_> {
    fn blank(&self, c: usize) -> bool {
        #[cfg(test)]
        crate::work_counts::card_cell_read();
        let Some(cells) = self.cells else {
            return false;
        };
        if c >= self.cols {
            return false;
        }
        let col = self.col_off + c;
        cells
            .get(col)
            .is_none_or(|cell| cell.ch == ' ' && *cell == self.blank)
            && !(self.side
                && (touches(&self.scratch.clusters, self.row, col)
                    || touches(&self.scratch.combining, self.row, col)
                    || touches(&self.scratch.images, self.row, col)))
    }

    fn run(&self, from: usize, to: usize) -> usize {
        (from..to).take_while(|&c| self.blank(c)).count()
    }
}

const fn sits_in(place: &CardPlace, pane: PaneGeom) -> bool {
    matches!(place.rows, 1 | 3 | 4) && place.top + place.rows <= pane.rows && pane.cols >= 2
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Wide {
        row: usize,
        col: usize,
        wide: bool,
    },
    Blanks {
        row: usize,
        from: usize,
        to: usize,
        len: usize,
    },
}

/// Recorded cell probes from [`layout`], used to validate cached layouts.
#[derive(Clone, Debug, Default)]
struct Seen(Vec<Probe>);

impl Seen {
    fn wide(&mut self, cells: &PaneCells<'_>, row: usize, col: usize) -> bool {
        let wide = cells.wide(row, col);
        self.0.push(Probe::Wide { row, col, wide });
        wide
    }

    fn blanks(&mut self, cells: &PaneCells<'_>, row: usize, from: usize, to: usize) -> usize {
        if from >= to {
            return 0;
        }
        let len = cells.row(row).run(from, to);
        self.0.push(Probe::Blanks { row, from, to, len });
        len
    }

    fn hold(&self, cells: &PaneCells<'_>) -> bool {
        self.0.iter().all(|probe| match *probe {
            Probe::Wide { row, col, wide } => cells.wide(row, col) == wide,
            Probe::Blanks { row, from, to, len } => {
                cells.row(row).run(from, to.min(from + len + 1)) == len
            }
        })
    }
}

fn touches<T>(channel: &[Vec<(usize, T)>], row: usize, col: usize) -> bool {
    channel
        .get(row)
        .is_some_and(|entries| entries.iter().any(|(c, _)| *c == col))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LinePlace<'t> {
    row: usize,
    col: usize,
    text: &'t str,
    cells: usize,
}

/// Computed card box, kitty placement, and status line coordinates in pane cells.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Layout<'t> {
    rows: usize,
    cols: usize,
    trail: Option<(usize, usize)>,
    kitty_col: Option<usize>,
    kitty_rows: usize,
    air: (bool, bool),
    line: Option<LinePlace<'t>>,
}

/// Compute the card [`Layout`] on `cells` and record probed cells in `seen`.
fn layout<'t>(
    cells: &PaneCells<'_>,
    place: &CardPlace,
    kc: usize,
    texts: &'t [String],
    seen: &mut Seen,
) -> Option<Layout<'t>> {
    #[cfg(test)]
    crate::work_counts::card_laid_out();
    seen.0.clear();
    let pane = cells.pane;
    if !sits_in(place, pane) {
        return None;
    }
    let top = place.top;
    let max_border = pane.cols - 1;
    let mut ends = place.ends;
    let logical_end = ends.iter().copied().max().unwrap_or(0);
    let mut card_end = logical_end;
    for k in 0..place.rows {
        let end = ends.get(k).copied().unwrap_or(logical_end);
        if seen.wide(cells, top + k, end) {
            card_end = card_end.max(end + 1);
            if let Some(end) = ends.get_mut(k) {
                *end += 1;
            }
        }
    }
    if card_end > logical_end && card_end > max_border {
        return None;
    }
    let pick = |seen: &mut Seen, row: usize, col: usize| {
        let reach = texts
            .iter()
            .map(|text| col + LINE_LEAD + text_cells(text))
            .filter(|&end| end <= max_border)
            .max()?;
        let blank = seen.blanks(cells, row, col, reach);
        texts.iter().find_map(|text| {
            let words = text_cells(text);
            let end = col + LINE_LEAD + words;
            (end <= max_border && end <= col + blank).then_some(LinePlace {
                row,
                col,
                text,
                cells: words,
            })
        })
    };
    let under = top + place.rows;
    let mut line = None;
    if under < pane.rows {
        line = pick(seen, under, place.text_col);
    }
    if line.is_none() {
        line = pick(seen, top, ends[0] + 2);
    }
    let rows = place.rows + usize::from(line.as_ref().is_some_and(|l| l.row == under));
    let line_end = line.as_ref().map(|l| l.col + LINE_LEAD + l.cells);
    let row0_end = match (&line, line_end) {
        (Some(l), Some(end)) if l.row == top => end,
        _ => ends[0],
    };
    let text_end = ends
        .iter()
        .copied()
        .chain(line_end)
        .chain([row0_end, card_end])
        .max()
        .unwrap_or(0);
    let mut kitty = None;
    if kc > 0 && rows >= 2 {
        let row1_end = if place.rows == 1 {
            line_end.unwrap_or(ends[0])
        } else {
            ends[1]
        };
        let start = row0_end.max(row1_end) + 2;
        let reach = (start + TRAIL_MAX + kc + KITTY_MARGIN)
            .max(text_end)
            .min(max_border);
        let blank = seen.blanks(cells, top, start, reach);
        let limit = start + seen.blanks(cells, top + 1, start, start + blank);
        let room = limit.saturating_sub(start + kc + KITTY_MARGIN);
        if start < limit && room >= TRAIL_MIN {
            let kitty_col = start + room.min(TRAIL_MAX);
            let cols = (kitty_col + kc + KITTY_MARGIN).max(text_end);
            if cols <= limit {
                kitty = Some((start, kitty_col, cols));
            }
        }
    }
    let kitty_rows = match kitty {
        Some((start, kitty_col, _)) if place.rows >= 3 => {
            let end = kitty_col + kc + KITTY_MARGIN;
            if seen.blanks(cells, top + 2, start, end) == end - start {
                3
            } else {
                2
            }
        }
        _ => 2,
    };
    let cols = kitty.map_or(text_end, |(_, _, cols)| cols).min(pane.cols);
    let span = (cols + 1).min(pane.cols);
    let below = top + rows;
    let tried = seen
        .0
        .iter()
        .any(|probe| matches!(*probe, Probe::Blanks { row, .. } if row == below));
    let blank_row = |seen: &mut Seen, row: usize| seen.blanks(cells, row, 0, span) == span;
    let air_above = top > 0 && blank_row(seen, top - 1);
    let air_below = below < pane.rows && !tried && blank_row(seen, below);
    Some(Layout {
        rows,
        cols,
        trail: kitty.map(|(start, kitty_col, _)| (start, kitty_col)),
        kitty_col: kitty.map(|(_, kitty_col, _)| kitty_col),
        kitty_rows,
        air: (air_above, air_below),
        line,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pixels {
    cw: usize,
    ch: usize,
    pad: usize,
    grid_top: usize,
}

fn pixel(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn u16_of(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// Build the window-pixel [`CardScene`] for a laid-out card.
#[allow(
    clippy::too_many_arguments,
    reason = "the scene's own fields, gathered from the pane, the window and the motion seam"
)]
fn scene(
    pane: PaneGeom,
    px: Pixels,
    place: &CardPlace,
    lay: &Layout,
    view: AutopilotView,
    (dark, ground): (bool, u32),
    serious: bool,
    (coat, eye): (u32, u32),
) -> CardScene {
    let origin_x = pixel(px.pad + pane.col_off * px.cw);
    let origin_y = pixel(px.grid_top + pane.frame_row(0) * px.ch);
    let pad_left = if pane.col_off == 0 { px.pad } else { 0 };
    CardScene {
        origin_x,
        origin_y,
        cw: u16_of(px.cw),
        ch: u16_of(px.ch),
        row_base: u16_of(pane.frame_row(0)),
        pad_left: u16_of(pad_left),
        pad_above: if lay.air.0 { u16_of(px.ch) } else { 0 },
        pad_below: if lay.air.1 { u16_of(px.ch) } else { 0 },
        top: u16_of(place.top),
        rows: u16_of(lay.rows),
        cols: u16_of(lay.cols),
        clip: (
            origin_x.saturating_sub(pixel(pad_left)),
            origin_y,
            origin_x.saturating_add(pixel(pane.cols * px.cw)),
            origin_y.saturating_add(pixel(pane.rows * px.ch)),
        ),
        trail: lay.trail.map(|(a, b)| (u16_of(a), u16_of(b))),
        kitty_col: lay.kitty_col.map(u16_of),
        kitty_rows: u8::try_from(lay.kitty_rows).unwrap_or(2),
        palette: view.card_palette().unwrap_or_default(),
        dark,
        ground,
        serious,
        coat,
        eye,
        wand: place.wand().map(|(col, row)| (u16_of(col), u16_of(row))),
    }
}

/// Whether `blank`'s background is dark.
pub(crate) fn dark_ground(blank: RenderCell) -> bool {
    aterm_effects::harness_card::dark_ground(pack(blank.bg))
}

fn kitty_width(key: &LaidKey) -> usize {
    if key.serious {
        0
    } else {
        usize::from(kitty_cols(u16_of(key.px.cw), u16_of(key.px.ch)))
    }
}

const fn pane_cells<'a>(scratch: &'a RenderInput, key: &LaidKey) -> PaneCells<'a> {
    PaneCells {
        scratch,
        pane: key.pane,
        blank: key.blank,
    }
}

fn lay_out(
    scratch: &RenderInput,
    read: &CardRead,
    key: &LaidKey,
    seen: &mut Seen,
) -> Option<(Layout<'static>, CardScene)> {
    let cells = pane_cells(scratch, key);
    let lay = layout(
        &cells,
        &read.place,
        kitty_width(key),
        line_texts_for(read.kind),
        seen,
    )?;
    let dark = dark_ground(key.blank);
    let fills =
        aterm_effects::cat_baker::ResolvedFills::from_indices(key.kitty.0, key.kitty.1, dark);
    let scene = scene(
        key.pane,
        key.px,
        &read.place,
        &lay,
        key.view,
        (dark, pack(key.blank.bg)),
        key.serious,
        (pack_fill(fills.coat), pack_fill(fills.iris)),
    );
    Some((lay, scene))
}

#[derive(Clone, Copy, Debug)]
struct InkFrame {
    now: Option<Instant>,
    still: bool,
    step: Option<u64>,
    replayed: bool,
}

impl InkFrame {
    fn of(card: &HarnessCard, frame: Option<(Instant, bool, Option<u64>)>, animate: bool) -> Self {
        let still = card.is_still() || !animate;
        Self {
            now: frame.map(|(now, _, _)| now).filter(|_| !still),
            still,
            step: frame.and_then(|(_, _, step)| step).filter(|_| !still),
            replayed: frame.is_none_or(|(_, drew, _)| !drew),
        }
    }

    fn keeps(&self, settled: bool, step: Option<u64>) -> bool {
        (settled && self.still) || (self.replayed && self.step.is_some() && step == self.step)
    }
}

/// Fingerprint of Claude's mascot glyphs and colors used to phase-lock shimmer steps.
fn mascot_ink(scratch: &RenderInput, pane: PaneGeom, place: &CardPlace) -> Option<u64> {
    let (_, wand_row) = place.wand()?;
    let mut ink = aterm_hash::FNV64_OFFSET;
    for row in wand_row..wand_row + 3 {
        let cells = scratch.cells.get(pane.frame_row(row));
        for col in 0..place.mascot_end {
            let (ch, [r, g, b]) = cells
                .and_then(|cells| cells.get(pane.frame_col(col)))
                .map_or((' ', [0; 3]), |cell| (cell.ch, cell.fg));
            let word = u64::from(u32::from(ch))
                ^ (u64::from(r) << 32 | u64::from(g) << 40 | u64::from(b) << 48);
            ink = aterm_hash::fnv_mix_u64(ink, word);
        }
    }
    Some(ink)
}

fn words_base(
    read: &CardRead,
    pane: PaneGeom,
    ground: RenderCell,
    scratch: &RenderInput,
) -> RenderCell {
    let last = read.place.rows.saturating_sub(1);
    let sample_row = read.place.top + if read.kind == 3 { last } else { last.min(2) };
    let gray = scratch
        .cells
        .get(pane.frame_row(sample_row))
        .and_then(|row| row.get(pane.frame_col(read.place.text_col)))
        .map_or(ground.fg, |cell| cell.fg);
    RenderCell {
        fg: crate::claude_footer::value_fg(gray, ground.fg),
        ..ground
    }
}

/// Mark `cell` as a link's letter: a single underline with no colour of its own, so it takes the
/// letter's ink.
const fn underline_link(cell: &mut RenderCell) {
    cell.underline = UnderlineStyle::Single;
    cell.underline_color = None;
}

fn push_words(text: &str, base: RenderCell, out: &mut Vec<RenderCell>) {
    for ch in text.chars() {
        let cell = RenderCell { ch, ..base };
        match aterm_grapheme::char_width(ch) {
            0 => {}
            1 => out.push(cell),
            _ => {
                out.push(cell);
                out.push(RenderCell {
                    ch: ' ',
                    wide: true,
                    ..base
                });
            }
        }
    }
}

/// Compute line ink and the leading `✦` decoration for `len` cells starting after `star_col`,
/// lit from the card's `wand` tip where it stands.
#[allow(
    clippy::too_many_arguments,
    reason = "the words' place, their card's wand, the pane, the frame's clock and motion"
)]
fn ink_words(
    card: &HarnessCard,
    wand: Option<(usize, usize)>,
    pane: PaneGeom,
    (frame_row, star_col, len): (usize, usize, usize),
    hero: Range<u16>,
    ground: RenderCell,
    inked: InkFrame,
    animate: bool,
    ink: &mut Vec<InkCell>,
) -> WordDecoration {
    ink.clear();
    #[cfg(test)]
    crate::work_counts::card_line_inked();
    let wand = wand.map(|(col, row)| (u16_of(pane.frame_col(col)), u16_of(pane.frame_row(row))));
    let at = LineAt {
        row: u16_of(frame_row),
        col: u16_of(star_col + LINE_LEAD),
        len: u16_of(len),
        hero,
        wand,
    };
    card.line_ink_into(inked.now, &at, pack(ground.bg), animate, ink);
    let (color, blend) = line_star(
        ink.first().map_or(pack(ground.fg), |c| pack(c.color)),
        pack(ground.bg),
    );
    WordDecoration {
        row: u16_of(frame_row),
        col: u16_of(star_col),
        dx: 0,
        dy: 0,
        glyph: DecoGlyph::Star4,
        blend,
        color,
        alpha: u8::MAX,
    }
}

/// Build or reuse [`CardLine`] cells, ink, and star for this frame.
#[allow(
    clippy::too_many_arguments,
    reason = "the line's place, its card, the frame's cells and the card's clock and motion"
)]
fn make_line(
    line: &mut CardLine,
    card: &HarnessCard,
    place: &LinePlace<'static>,
    read: &CardRead,
    pane: PaneGeom,
    ground: RenderCell,
    scratch: &RenderInput,
    palette: CardPalette,
    frame: Option<(Instant, bool, Option<u64>)>,
    animate: bool,
) {
    let frame_row = pane.frame_row(place.row);
    let star_col = pane.frame_col(place.col);
    let key = LineKey {
        frame_row,
        star_col,
        text: place.text,
        base: words_base(read, pane, ground, scratch),
        dark: dark_ground(ground),
        palette,
    };
    line.on = true;
    // The key holds the frame row, which a strip that grows as the grid scrolls keeps: the term
    // row the link's hit is checked against is refreshed on every frame.
    line.term_row = pane.row_off + place.row;
    let inked = InkFrame::of(card, frame, animate);
    if line.key == Some(key) && inked.keeps(line.settled, line.step) {
        return;
    }
    if line.key != Some(key) {
        line.frame_row = frame_row;
        line.col = star_col + LINE_LEAD;
        line.ground = ground;
        line.hero = hero_cells(place.text).unwrap_or(0..0);
        line.cells.clear();
        push_words(place.text, key.base, &mut line.cells);
        let hero = usize::from(line.hero.start)..usize::from(line.hero.end);
        if let Some(cells) = line.cells.get_mut(hero) {
            for cell in cells {
                cell.bold = true;
            }
        }
        let link = line_link(place.text).unwrap_or(0..0);
        if let Some(cells) = line.cells.get_mut(link.clone()) {
            for cell in cells {
                underline_link(cell);
            }
        }
        line.link = line.col + link.start..line.col + link.end;
        line.key = Some(key);
    }
    line.star = ink_words(
        card,
        read.place.wand(),
        pane,
        (frame_row, star_col, line.cells.len()),
        line.hero.clone(),
        ground,
        inked,
        animate,
        &mut line.ink,
    );
    line.settled = inked.still;
    line.step = inked.step;
}

/// Reason a pane's launch card was left undecorated this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bare {
    Unsupervised,
    NoDrawer,
    NoCard,
    Token,
    Kind,
    Fence,
    Tainted,
    Build,
    Deferred,
}

impl Bare {
    /// Diagnostic label for `card-bare reason=`.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Unsupervised => "unsupervised",
            Self::NoDrawer => "no-drawer",
            Self::NoCard => "no-card",
            Self::Token => "token",
            Self::Kind => "kind",
            Self::Fence => "fence",
            Self::Tainted => "tainted",
            Self::Build => "build",
            Self::Deferred => "deferred",
        }
    }

    /// Whether this bare reason triggers the [`RESHOW_MS`] re-show bound.
    pub(crate) const fn bounds_reshow(self) -> bool {
        matches!(
            self,
            Self::NoDrawer | Self::Token | Self::Kind | Self::Fence | Self::Tainted | Self::Build
        )
    }
}

/// Per-pane card pass outcome recorded for the dev card probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardOutcome {
    Decorated {
        token: u64,
        row: i64,
        armed: Option<&'static str>,
    },
    Bare(u64, Bare, Option<i64>),
}

/// Holder fence snapshot extracted with a pane's cells.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FenceFrame {
    pub(crate) holder: aterm_core::terminal::HolderView,
    pub(crate) base_y: i64,
    pub(crate) alt: bool,
    pub(crate) mutations: u64,
}

impl FenceFrame {
    /// Extract the holder fence from `scratch`.
    pub(crate) const fn of(scratch: &RenderInput) -> Self {
        Self {
            holder: scratch.holder,
            base_y: scratch.base_y,
            alt: scratch.engine_alt,
            mutations: scratch.screen_mutations,
        }
    }

    /// Whether the fence says another holder drew the pane's main-screen rows `top..end`: a
    /// current drawer whose clean fence admits none of them (a composer an exited process left on
    /// the screen, before the drawer in front draws its own). A row of them in any of its spans
    /// keeps them its own, whole or not: a drawer resumed with the caret past its composer's ink
    /// reselects a span that ends at the prompt row (exclusive) and opens the next under the mode
    /// row, so only the top rule is in its spans, the prompt row and the bottom rule are in none,
    /// and no one span holds the composer whole. An alternate screen, no drawer, and
    /// a tainted fence (nothing on the main screen admitted: its lay found ink, an anchor was
    /// lost, or it was adopted without a seed) say nothing either way.
    pub(crate) fn disowns(&self, top: usize, end: usize) -> bool {
        if self.alt || self.holder.token == 0 || self.holder.tainted {
            return false;
        }
        let row = |r: usize| {
            u64::try_from(self.base_y)
                .ok()
                .and_then(|base| base.checked_add(u64::try_from(r).ok()?))
        };
        match (row(top), row(end)) {
            (Some(lo), Some(hi)) => !(lo..hi).any(|r| self.holder.admits_rows(r, r + 1)),
            _ => false,
        }
    }
}

/// Check whether `read` belongs to the current drawer under `gate` and `fence`.
pub(crate) fn admit(
    gate: &crate::holder_image::CardGate,
    fence: &FenceFrame,
    read: &CardRead,
) -> Result<(), Bare> {
    use crate::holder_image::GateBuild;
    token_holds(gate, fence)?;
    if read.kind != gate.kind {
        return Err(Bare::Kind);
    }
    fence_holds(fence, read.place.top, gate.rows.max(read.place.rows))?;
    match gate.build {
        GateBuild::Exact(build) | GateBuild::Registry(build) if read.build == Some(build) => Ok(()),
        GateBuild::ProvenanceOnly => Ok(()),
        GateBuild::Exact(_) | GateBuild::Registry(_) | GateBuild::None => Err(Bare::Build),
    }
}

const fn token_holds(gate: &crate::holder_image::CardGate, fence: &FenceFrame) -> Result<(), Bare> {
    if gate.token == 0 || fence.holder.token != gate.token {
        return Err(Bare::Token);
    }
    Ok(())
}

/// Verify that `rows` starting at `top` (or the alternate screen) belong to `fence`.
fn fence_holds(fence: &FenceFrame, top: usize, rows: usize) -> Result<(), Bare> {
    if fence.alt {
        return if fence.holder.admits_alt() {
            Ok(())
        } else {
            Err(Bare::Fence)
        };
    }
    if fence.holder.tainted {
        return Err(Bare::Tainted);
    }
    let lo = u64::try_from(fence.base_y)
        .ok()
        .and_then(|base| base.checked_add(top as u64));
    if lo.is_some_and(|lo| fence.holder.admits_rows(lo, lo.saturating_add(rows as u64))) {
        Ok(())
    } else {
        Err(Bare::Fence)
    }
}

/// Gate verdict on a pane's launch card (`Ok((card, text_rebuild))` or [`Bare`]).
pub(crate) type Verdict = Result<(Arc<CardRead>, u64), Bare>;

/// Per-pane card provenance gate shared by the card pass and footer fact cache.
#[derive(Debug)]
pub(crate) struct GateCheck {
    gate: Option<crate::holder_image::CardGate>,
    fence: FenceFrame,
    verdict: Option<Verdict>,
    refused_top: Option<usize>,
}

impl GateCheck {
    /// Create a gate check for a pane's current drawer `gate` and `fence`.
    pub(crate) const fn new(
        gate: Option<crate::holder_image::CardGate>,
        fence: FenceFrame,
    ) -> Self {
        Self {
            gate,
            fence,
            verdict: None,
            refused_top: None,
        }
    }

    /// Absolute banner row of a card refused by [`Self::verdict`].
    pub(crate) fn refused_row(&self) -> Option<i64> {
        self.refused_top.map(|top| {
            self.fence
                .base_y
                .saturating_add(i64::try_from(top).unwrap_or(i64::MAX))
        })
    }

    /// Current drawer token, or `0` when none.
    pub(crate) fn token(&self) -> u64 {
        self.gate.map_or(0, |g| g.token)
    }

    /// Whether the fence says another holder drew rows `top..end` ([`FenceFrame::disowns`]).
    pub(crate) fn disowns(&self, top: usize, end: usize) -> bool {
        self.fence.disowns(top, end)
    }

    /// Whether row `row` is one the current drawer drew, the gate's reader being `kind`
    /// ([`crate::holder_image::CardProgram::rows_kind`]).
    pub(crate) fn admits_row(&self, kind: usize, row: usize) -> bool {
        self.gate.is_some_and(|gate| {
            gate.kind == kind
                && token_holds(&gate, &self.fence).is_ok()
                && fence_holds(&self.fence, row, 1).is_ok()
        })
    }

    /// Compute or return the cached [`Verdict`] for `session` at `leaf_index`.
    pub(crate) fn verdict(
        &mut self,
        texts: &crate::claude_footer::PaneTextCache,
        leaf_index: usize,
        session: u64,
    ) -> &Verdict {
        let (gate, fence, refused) = (self.gate, self.fence, &mut self.refused_top);
        self.verdict.get_or_insert_with(|| {
            let gate = gate.ok_or(Bare::NoDrawer)?;
            token_holds(&gate, &fence)?;
            let (read, text_gen) = texts.launch_card(leaf_index, session).ok_or(Bare::NoCard)?;
            if let Err(why) = admit(&gate, &fence, &read) {
                *refused = Some(read.place.top);
                return Err(why);
            }
            Ok((read, text_gen))
        })
    }

    /// Whether the footer's launch-card reading at `banner`/`status` passes the drawer gate.
    pub(crate) fn admits_reading(
        &mut self,
        texts: &crate::claude_footer::PaneTextCache,
        leaf_index: usize,
        session: u64,
        program: aterm_phase::Program,
        banner: Option<usize>,
        status: Option<usize>,
    ) -> bool {
        let Some(kind) =
            crate::holder_image::CardProgram::from_program(program).map(|p| p.rows_kind())
        else {
            return false;
        };
        if banner.is_none() && status.is_none() {
            return false;
        }
        if status.is_some_and(|row| !self.admits_row(kind, row)) {
            return false;
        }
        banner.is_none_or(|top| {
            matches!(
                self.verdict(texts, leaf_index, session),
                Ok((read, _)) if read.place.top == top && read.kind == kind
            )
        })
    }
}

/// Gather a [`CardLeaf`] for a live-viewport pane during the footer splice.
#[allow(
    clippy::too_many_arguments,
    reason = "the pane's place, its session, its geometry and ground, its Autopilot and its gate"
)]
pub(crate) fn card_leaf(
    texts: &crate::claude_footer::PaneTextCache,
    card_on: bool,
    leaf_index: usize,
    session: u64,
    pane: PaneGeom,
    blank: RenderCell,
    view: AutopilotView,
    supervised: bool,
    check: &mut GateCheck,
) -> CardLeaf {
    let wrapped = card_on && view.wears_glass(supervised).is_some();
    let token = check.token();
    let (card, bare) = if wrapped {
        match check.verdict(texts, leaf_index, session) {
            Ok((read, text_gen)) => (Some((Arc::clone(read), token, *text_gen)), None),
            Err(why) => (None, Some(*why)),
        }
    } else {
        (None, Some(Bare::Unsupervised))
    };
    CardLeaf {
        leaf_index,
        session,
        pane,
        blank,
        view,
        card,
        bare,
        token,
        row_base: check.fence.base_y,
        bare_row: if wrapped { check.refused_row() } else { None },
        drawerless: false,
        mutations: check.fence.mutations,
    }
}

/// Build a hide-only [`CardLeaf`] for a pane whose drawer left while a card entry remains.
pub(crate) fn drawerless_leaf(leaf_index: usize, session: u64, pane: PaneGeom) -> CardLeaf {
    CardLeaf {
        leaf_index,
        session,
        pane,
        blank: RenderCell::default(),
        view: AutopilotView::Off,
        card: None,
        bare: Some(Bare::NoDrawer),
        token: 0,
        row_base: 0,
        bare_row: None,
        drawerless: true,
        mutations: 0,
    }
}

impl CardLeaf {
    /// Whether this leaf actively read `index`'s card text cache.
    pub(crate) const fn reads(&self, index: usize) -> bool {
        self.leaf_index == index && !self.drawerless
    }
}

#[derive(Clone, Copy)]
struct Look {
    animate: bool,
    serious: bool,
    px: Pixels,
    kitty: (u8, u8),
}

impl App {
    /// Hit-test the card lines' marks on `wid`'s glass at window pixel `(x, y)`: the session
    /// the mark belongs to and the URL a press there opens.
    pub(crate) fn claude_glass_hit_at(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(u64, &'static str)> {
        let ws = self.windows.get(&wid)?;
        if ws.claude_card.glass_hits.is_empty() {
            return None;
        }
        let (row, col) = self.glass_cell_at(wid, x, y)?;
        let hit = ws
            .claude_card
            .glass_hits
            .iter()
            .find(|hit| hit.frame_row == row && hit.cols.contains(&col))?;
        (!self.chrome_owns_terminal_cell(wid, hit.term_row, col)
            && !self.chrome_overlay_at(wid, x, y))
        .then_some((hit.session, hit.url))
    }

    /// Whether the pointer rests on a card line's mark in `wid`
    /// ([`Self::claude_glass_hit_at`]).
    pub(crate) fn pointer_on_glass_link(&self, wid: WindowId) -> bool {
        let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) else {
            return false;
        };
        self.claude_glass_hit_at(wid, x, y).is_some()
    }

    /// Open the link of `session`'s card line for a screen reader's press on the mark's node,
    /// where the glass shows it: a tab name being edited settles first, as on a pointer's press.
    pub(crate) fn open_claude_link_for_reader(&mut self, wid: WindowId, session: u64) {
        let url = self.windows.get(&wid).and_then(|ws| {
            ws.claude_card
                .glass_hits
                .iter()
                .find(|hit| {
                    hit.session == session
                        && !self.chrome_owns_terminal_span(wid, hit.term_row, hit.cols.clone())
                })
                .map(|hit| hit.url)
        });
        let Some(url) = url.filter(|url| crate::is_safe_url(url)) else {
            return;
        };
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
        crate::app_mouse::open_url_external(url);
    }

    /// The card lines' marks in `wid` as the accessibility tree publishes them, on grid rows
    /// below the window's `strip` chrome rows: each a link named by the mark it shows.
    #[cfg(a11y_tree)]
    pub(crate) fn claude_links_a11y(
        &self,
        wid: WindowId,
        strip: usize,
    ) -> Vec<crate::a11y_tree::GridChip> {
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let mut links = Vec::new();
        for hit in &ws.claude_card.glass_hits {
            let url = hit.url;
            let Some(row) = hit.frame_row.checked_sub(strip) else {
                continue;
            };
            if self.chrome_owns_terminal_span(wid, hit.term_row, hit.cols.clone()) {
                continue;
            }
            let words: String = ws
                .input_scratch
                .cells
                .get(hit.frame_row)
                .and_then(|cells| cells.get(hit.cols.clone()))
                .map(|cells| cells.iter().map(|c| c.ch).collect())
                .unwrap_or_default();
            let label = words.trim().to_owned();
            if label.is_empty() {
                continue;
            }
            links.push(crate::a11y_tree::GridChip {
                session: hit.session,
                chip: crate::a11y_tree::HarnessChip::CardMark,
                label,
                description: Some(format!("Opens {url} in the browser")),
                toggled: None,
                row,
                col: hit.cols.start,
                width: hit.cols.len(),
            });
        }
        links
    }

    /// Whether cards in `wid` are allowed to animate.
    pub(crate) fn card_animates(&self, wid: WindowId, focused: bool) -> bool {
        let motion = crate::motion::MotionEffect::HarnessCard;
        self.effect_policy(motion, self.motion_focus(wid, focused))
            .animate(motion)
    }

    fn card_look(&self, wid: WindowId, focused: bool) -> Look {
        let animate = self.card_animates(wid, focused);
        let serious = !self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::HarnessCard);
        let (cw, ch) = self.win_cell_size(wid);
        Look {
            animate,
            serious,
            px: Pixels {
                cw,
                ch,
                pad: self.win_pad(wid),
                grid_top: self.win_pad_top(wid) + self.win_head(wid),
            },
            kitty: (self.launch_kitty.coat, self.launch_kitty.iris),
        }
    }

    /// Decorate gathered [`CardLeaf`] panes for `wid` and populate their status lines.
    pub(crate) fn compose_claude_cards(&mut self, wid: WindowId, mut leaves: Vec<CardLeaf>) {
        let Some((focused, away)) = self
            .windows
            .get(&wid)
            .map(|ws| (ws.focused, ws.claude_card.away))
        else {
            return;
        };
        let probe = crate::holder_image::DEV_ROWS && self.session_factory.card_probe;
        let mut lapsed = false;
        let look = leaves.iter().any(|leaf| leaf.card.is_some()).then(|| {
            let focus = moves_focused(focused, away);
            lapsed = !focus && away.is_some();
            self.card_look(wid, focus)
        });
        let focused_session = self.focused_session_id(wid);
        let pool = &self.pool;
        let Some(ws) = self.windows.get_mut(&wid).map(Box::as_mut) else {
            return;
        };
        let search_open = ws.search.is_some();
        let crate::WindowState {
            claude_card: cards,
            input_scratch: scratch,
            ..
        } = ws;
        strip(cards, scratch);
        if lapsed {
            cards.away = None;
        }
        cards.entries.retain(|e| pool.get(e.session).is_some());
        cards.hide_all();
        let mut clock = LazyNow::default();
        let mut outcomes = std::mem::take(&mut cards.outcomes);
        let WindowCards {
            entries,
            painted,
            seen,
            ..
        } = cards;
        for leaf in leaves.drain(..) {
            let find_up =
                search_open && (focused_session.is_none() || Some(leaf.session) == focused_session);
            let at = entries
                .iter()
                .position(|e| (e.leaf_index, e.session) == (leaf.leaf_index, leaf.session))
                .or_else(|| {
                    entries
                        .iter()
                        .position(|e| !e.shown && e.session == leaf.session)
                });
            if let Some(at) = at {
                entries[at].leaf_index = leaf.leaf_index;
            }
            let Some(((read, token, text_gen), look)) = leaf.card.as_ref().zip(look) else {
                let why = leaf.bare.unwrap_or(Bare::NoCard);
                if let Some(at) = at {
                    let entry = &mut entries[at];
                    entry.deferred = None;
                    entry.gate_hid |= why.bounds_reshow();
                    if !leaf.drawerless && !leaf.view.carries() {
                        entry.view = leaf.view;
                    }
                }
                if probe {
                    outcomes.push((
                        leaf.session,
                        CardOutcome::Bare(leaf.token, why, leaf.bare_row),
                        leaf.mutations,
                    ));
                }
                continue;
            };
            let same = |e: &CardEntry| e.identity == *token;
            if let Some(at) = at
                && same(&entries[at])
                && !entries[at].was_shown
            {
                let entry = &mut entries[at];
                if entry.gate_hid {
                    let now = clock.get();
                    let until = entry
                        .shown_since
                        .checked_add(Duration::from_millis(RESHOW_MS))
                        .unwrap_or(now);
                    if now < until {
                        entry.deferred = Some(until);
                        entry.asked = false;
                        if probe {
                            let row = leaf.row_base.saturating_add(read.place.top as i64);
                            outcomes.push((
                                leaf.session,
                                CardOutcome::Bare(*token, Bare::Deferred, Some(row)),
                                leaf.mutations,
                            ));
                        }
                        continue;
                    }
                }
                entry.deferred = None;
            }
            let key = LaidKey {
                pane: leaf.pane,
                blank: leaf.blank,
                place: read.place,
                view: leaf.view,
                px: look.px,
                serious: look.serious,
                kitty: look.kitty,
            };
            if read.cut {
                continue;
            }
            let kept = match at.map(|at| &entries[at]) {
                Some(e) if same(e) && !find_up => match &e.laid {
                    Some((k, laid))
                        if *k == key
                            && (e.laid_gen == *text_gen
                                || e.seen.hold(&pane_cells(scratch, &key))) =>
                    {
                        Some(laid.clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            let fresh = kept.is_none();
            let laid = kept.unwrap_or_else(|| lay_out(scratch, read, &key, seen));
            let keep = |entry: &mut CardEntry,
                        laid: Option<(Layout<'static>, CardScene)>,
                        seen: &mut Seen| {
                if find_up {
                    entry.laid = None;
                    return;
                }
                if fresh {
                    std::mem::swap(&mut entry.seen, seen);
                }
                entry.laid = Some((key, laid));
                entry.laid_gen = *text_gen;
            };
            let Some((lay, scene)) = laid else {
                if let Some(at) = at {
                    let entry = &mut entries[at];
                    entry.view = leaf.view;
                    if same(entry) {
                        keep(entry, None, seen);
                    }
                }
                if probe {
                    let row = leaf.row_base.saturating_add(read.place.top as i64);
                    outcomes.push((
                        leaf.session,
                        CardOutcome::Bare(*token, Bare::NoCard, Some(row)),
                        leaf.mutations,
                    ));
                }
                continue;
            };
            let mut armed = None;
            let at = match at {
                Some(at) if same(&entries[at]) => {
                    let entry = &mut entries[at];
                    if !entry.was_shown {
                        entry.shown_since = clock.get();
                        entry.gate_hid = false;
                    }
                    if entry.view != leaf.view {
                        entry.arm(clock.get());
                        armed = Some("view");
                    }
                    at
                }
                found => {
                    let now = clock.get();
                    let origin = entries
                        .iter()
                        .filter(|e| !e.card.at_rest())
                        .find(|e| e.card.flowing(now))
                        .and_then(|e| e.card.ripple_origin());
                    let mut entry = CardEntry {
                        leaf_index: leaf.leaf_index,
                        session: leaf.session,
                        identity: *token,
                        card: HarnessCard::default(),
                        view: leaf.view,
                        armed_at: now,
                        ticked_at: now,
                        shown: false,
                        was_shown: false,
                        shown_since: now,
                        gate_hid: false,
                        deferred: None,
                        asked: false,
                        place: (0, 0, 0, 0),
                        glass_box: (0, 0, 0, 0),
                        frames: CardFrames::default(),
                        flare: false,
                        shimmer: None,
                        laid: None,
                        laid_gen: 0,
                        seen: Seen::default(),
                        line: CardLine::default(),
                    };
                    if let Some(origin) = origin {
                        entry.card.adopt_ripple(origin);
                    }
                    entry.arm(now);
                    armed = Some("new");
                    match found {
                        Some(at) => {
                            entries[at] = entry;
                            at
                        }
                        None => {
                            entries.push(entry);
                            entries.len() - 1
                        }
                    }
                }
            };
            let entry = &mut entries[at];
            entry.view = leaf.view;
            let (glass, frame) = match entry.card.replay(&scene, look.animate, &entry.frames) {
                Some(kept) => {
                    entry.shimmer = None;
                    (kept, None)
                }
                None => {
                    let now = clock.get();
                    let wand = read.place.wand().filter(|_| read.kind == 3);
                    let lit = wand.is_some_and(|(col, row)| {
                        scratch
                            .cells
                            .get(leaf.pane.frame_row(row))
                            .and_then(|cells| cells.get(leaf.pane.frame_col(col)))
                            .is_some_and(|cell| aterm_phase::wand_flare(cell.ch, cell.fg))
                    });
                    let shimmer = wand.zip(mascot_ink(scratch, leaf.pane, &read.place));
                    if lit && !entry.flare {
                        entry.card.lock_glint(now);
                    } else if let (Some((at, ink)), Some((was_at, was))) = (shimmer, entry.shimmer)
                        && at == was_at
                        && ink != was
                    {
                        entry.card.lock_step(now);
                    }
                    entry.flare = lit;
                    entry.shimmer = shimmer;
                    match entry
                        .card
                        .replay_step(&scene, look.animate, &entry.frames, now)
                    {
                        Some(kept) => (kept, Some((now, false))),
                        None => {
                            let drawn =
                                entry
                                    .card
                                    .draw(now, &scene, look.animate, &mut entry.frames);
                            (drawn, Some((now, true)))
                        }
                    }
                }
            };
            if !glass.is_empty() {
                let (cx0, cy0, cx1, cy1) = scene.clip;
                let grid = (pixel(look.px.pad), pixel(look.px.grid_top));
                append_card_frame(
                    painted,
                    scratch,
                    entry.session,
                    glass,
                    PxRect::new(cx0, cy0, cx1, cy1),
                    grid,
                );
            }
            if let Some((now, true)) = frame {
                #[cfg(test)]
                crate::work_counts::card_ticked();
                entry.ticked_at = now;
            }
            entry.shown = true;
            entry.asked = false;
            entry.place = (read.place.top, read.place.text_col, scene.rows, scene.cols);
            let above = usize::from(lay.air.0).min(read.place.top);
            let below = usize::from(lay.air.1);
            entry.glass_box = (
                leaf.pane.frame_row(read.place.top - above),
                leaf.pane.frame_col(0),
                lay.rows + above + below,
                lay.cols + 1,
            );
            if let Some(line) = &lay.line {
                make_line(
                    &mut entry.line,
                    &entry.card,
                    line,
                    read,
                    leaf.pane,
                    leaf.blank,
                    scratch,
                    scene.palette,
                    frame
                        .map(|(now, drew)| (now, drew, entry.frames.kept().and_then(|k| k.step()))),
                    look.animate,
                );
            }
            keep(entry, Some((lay, scene)), seen);
            if probe {
                let row = leaf.row_base.saturating_add(read.place.top as i64);
                outcomes.push((
                    leaf.session,
                    CardOutcome::Decorated {
                        token: *token,
                        row,
                        armed,
                    },
                    leaf.mutations,
                ));
            }
        }
        cards.leaves = leaves;
        if probe {
            cards.composed += 1;
            for (session, outcome, mutations) in outcomes.drain(..) {
                if let Some(s) = pool.get(session) {
                    #[cfg(test)]
                    crate::work_counts::card_probe_locked();
                    s.ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .note_card_frame(outcome, cards.composed, mutations);
                }
            }
        }
        cards.outcomes = outcomes;
    }

    /// Prebuild and queue harness card arts at `wid`'s cell height for GPU residency.
    pub(crate) fn prefetch_card_arts(&mut self, wid: WindowId) {
        if !self.config.claude_footer_card() {
            return;
        }
        let ch = u16_of(self.win_cell_size(wid).1);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let arts = match &ws.claude_card.arts {
            Some(arts) if arts.ch() == ch => Arc::clone(arts),
            _ => {
                let Some(arts) = CardArts::for_cell(ch) else {
                    ws.claude_card.arts = None;
                    return;
                };
                ws.claude_card.arts = Some(Arc::clone(&arts));
                arts
            }
        };
        let list: Vec<Arc<aterm_core::render::FxArt>> = arts.arts().cloned().collect();
        self.queue_fx_prefetch(wid, &list);
    }

    /// Prefetch card arts on windows displaying `session` when a card drawer is published.
    pub(crate) fn prefetch_card_arts_for(&mut self, session: u64) {
        if !self.card_drawer_published(session) {
            return;
        }
        let wids: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| crate::active_tab_displays_session(ws, &self.view_store, session))
            .map(|(wid, _)| *wid)
            .collect();
        for wid in wids {
            self.prefetch_card_arts(wid);
        }
    }

    /// Whether `session`'s published holder image is a card drawer or its launcher shim.
    pub(crate) fn card_drawer_published(&self, session: u64) -> bool {
        self.pool.get(session).is_some_and(|s| {
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .holder_image()
                .is_some_and(|image| image.drawer.is_some() || image.launcher_for.is_some())
        })
    }

    /// Repaint-key term for `session`'s harness card at plan leaf `index` in `wid`.
    pub(crate) fn claude_card_term(
        &self,
        wid: WindowId,
        ws: &crate::WindowState,
        index: usize,
        session: u64,
        view: AutopilotView,
        supervised: bool,
    ) -> u64 {
        let wrapped = self.config.claude_footer_card() && view.wears_glass(supervised).is_some();
        let serious = wrapped
            && !self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::HarnessCard);
        let mut h = mix(u64::from(wrapped) | view.code() << 1 | u64::from(serious) << 8);
        if wrapped
            && let Some(until) = ws
                .claude_card
                .entries
                .iter()
                .find(|e| (e.leaf_index, e.session) == (index, session))
                .and_then(|e| e.deferred)
        {
            h ^= mix(0x5e5e_0000 | u64::from(card_now() >= until));
        }
        if wrapped
            && let Some(entry) = ws
                .claude_card
                .entries
                .iter()
                .find(|e| e.shown && (e.leaf_index, e.session) == (index, session))
        {
            let focus = moves_focused(ws.focused, ws.claude_card.away);
            let animate = !serious && self.card_animates(wid, focus);
            let (top, col, rows, cols) = entry.place;
            h ^= mix((top as u64) << 32 | col as u64)
                ^ (u64::from(rows) << 16 | u64::from(cols)).rotate_left(40)
                ^ mix(MOTION_TAG | u64::from(animate)).rotate_left(23);
            if !entry.card.at_rest() {
                let now = card_now();
                match entry.card.deadline(now) {
                    Some(_) if entry.card.flowing(now) => {
                        let step = entry.card.flow_step(now).unwrap_or(0);
                        h ^= mix(FLOW_TAG ^ step);
                    }
                    Some(due) if due > now => {
                        let ms = now.saturating_duration_since(entry.armed_at).as_millis();
                        h ^= mix(1 + u64::try_from(ms).unwrap_or(u64::MAX) / FRAME_MS);
                    }
                    _ => {}
                }
            }
        }
        h
    }

    /// Whether any card across any window may need an animation or deferred-show wake.
    pub(crate) fn claude_cards_live(&self) -> bool {
        self.windows
            .values()
            .any(|ws| ws.claude_card.entries.iter().any(CardEntry::live))
    }

    /// Earliest frame deadline wanted by any visible card at `now`.
    pub(crate) fn claude_card_deadline(&self, now: Instant) -> Option<Instant> {
        self.windows
            .values()
            .filter_map(|ws| {
                let entries = ws.claude_card.entries.iter();
                let soonest = entries
                    .filter_map(|e| wanted_frame(&self.view_store, ws, e, now))
                    .min()?;
                crate::messages_host::window_on_screen(ws).then_some(soonest)
            })
            .min()
    }

    /// Next card wake deadline on [`card_now`], if any card is live.
    pub(crate) fn claude_card_wake(&self) -> Option<Instant> {
        if !self.claude_cards_live() {
            return None;
        }
        self.claude_card_deadline(card_now())
    }

    /// Windows on screen with a card frame due at `now`.
    pub(crate) fn claude_cards_due(&mut self, now: Instant) -> Vec<WindowId> {
        let views = &self.view_store;
        let due_at = |ws: &crate::WindowState, at: usize| {
            wanted_frame(views, ws, &ws.claude_card.entries[at], now)
                .is_some_and(|when| when <= now)
        };
        let mut due = Vec::new();
        let let_go = |ws: &mut crate::WindowState| {
            for entry in &mut ws.claude_card.entries {
                if entry.deferred.is_some_and(|until| until <= now) {
                    entry.asked = true;
                }
            }
        };
        for (wid, ws) in &mut self.windows {
            for entry in &mut ws.claude_card.entries {
                if entry.live() && entry.deferred.is_none() && entry.card.deadline(now).is_none() {
                    entry.asked = true;
                }
            }
            let count = ws.claude_card.entries.len();
            let Some(first) = (0..count).find(|&at| due_at(ws, at)) else {
                let_go(ws);
                continue;
            };
            if !crate::messages_host::window_on_screen(ws) {
                let_go(ws);
                continue;
            }
            for at in first..count {
                if due_at(ws, at) {
                    ws.claude_card.entries[at].asked = true;
                }
            }
            due.push(*wid);
        }
        due
    }

    /// Windows due for a card redraw at [`card_now`].
    pub(crate) fn claude_card_redraws(&mut self) -> Vec<WindowId> {
        if !self.claude_cards_live() {
            return Vec::new();
        }
        self.claude_cards_due(card_now())
    }

    /// Handle window focus changes for card and panel return animations.
    pub(crate) fn claude_glass_focus(&mut self, wid: WindowId, focused: bool) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let cards = &mut ws.claude_card;
        if !focused {
            cards.away = (cards.any_shown() || cards.panels.any_breath()).then(card_now);
            return;
        }
        let mut now = LazyNow::default();
        if let Some(since) = cards.away.take()
            && now.get().saturating_duration_since(since) < Duration::from_millis(RETURN_MS)
        {
            return;
        }
        self.rearm_claude_cards(wid, || now.get());
        self.rearm_claude_panels(wid, || now.get());
    }

    /// Re-arm card intro animations for all shown cards in `wid`.
    pub(crate) fn rearm_claude_cards(&mut self, wid: WindowId, now: impl FnOnce() -> Instant) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if !ws.claude_card.any_shown() {
            return;
        }
        let now = now();
        for entry in ws.claude_card.entries.iter_mut().filter(|e| e.shown) {
            entry.arm(now);
        }
    }

    /// Handle the `card` control verb to read or pin the star constellation seed.
    pub(crate) fn control_card_seed(
        &mut self,
        asked: crate::card_verb::Asked,
    ) -> Result<String, String> {
        let crate::card_verb::Asked::Pin(seed) = asked else {
            return Ok(crate::card_verb::line(pinned_seed(), 0));
        };
        #[cfg(any(test, debug_assertions))]
        {
            pin_card_seed(seed);
            let wids: Vec<WindowId> = self.windows.keys().copied().collect();
            let mut rearmed = 0;
            let mut now = LazyNow::default();
            for wid in wids {
                rearmed += self.windows[&wid]
                    .claude_card
                    .entries
                    .iter()
                    .filter(|e| e.shown)
                    .count();
                self.rearm_claude_cards(wid, || now.get());
            }
            self.request_redraw_all_windows();
            Ok(crate::card_verb::line(seed, rearmed))
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            let _ = seed;
            Err("card needs a debug build".to_string())
        }
    }
}

#[cfg(test)]
#[path = "claude_card_tests.rs"]
mod tests;
