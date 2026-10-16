// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS CARD on the glass (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §3): while the aterm harness carries a Claude Code session — its Autopilot
//! Cruise or Land, and the window's host holding the session's live
//! supervisor claim — Claude Code's launch card wears an animated rainbow
//! border, a pixel-art aterm kitty riding a rainbow trail with stars, and the
//! line `✦ enhanced by aterm harness <version> · autopilot ∞ cruise` (Land:
//! `· autopilot ⤓ land`, in a sunset). Off, the master switch off and a
//! window with no host show the vendor's card bare. Owner, 2026-10-15: "I'd
//! like for a rainbow border and a similar pixel art with an aterm kitty and
//! fancy rainbows and star power to show off the power of the harness and a
//! line to show that claude code is being enhanced by what version of the
//! claude code harness".
//!
//! * WHAT DRAWS IT. `aterm_effects::harness_card` is the pure emitter (the
//!   border, trail, kitty and stars as [`GlowQuad`]s, the line's ink); this
//!   module finds the card, lays it out on the pane and puts the emitter's
//!   output on the glass. The card is FOUND by its geometry
//!   (`aterm_phase::launch_card_scan`, never spelled here),
//!   read at most once per rebuild of the pane's text (the footer's
//!   `claude_footer::PaneTextCache::launch_card`) and kept across a rebuild
//!   that changed no row the reading reads ([`CardReading::holds`]).
//! * WHERE IT IS COMPOSED. In `App::splice_claude_footer`, the one place that
//!   holds the pane's text rows, its rect and the strip offset for the frame
//!   being presented, on every route — single, composed, heterogeneous and
//!   the capture paths ([`card_leaf`] beside the footer's own
//!   reading, [`App::compose_claude_cards`] before its write). An effects-pass
//!   emitter would draw on the previous frame's rect, a row off while inline
//!   output scrolls the card.
//! * GATES, per pane. `tab_status` on (the footer's own gate: without the
//!   status sweep the facts go stale); `[claude_footer] card` on (absent is
//!   on, [`crate::app_config::Config::claude_footer_card`]); a live viewport
//!   (`display_offset == 0` — a scrolled-back frame draws none); the session's
//!   effective Autopilot Cruise or Land AND its live claim this host's
//!   ([`App::session_autopilot`]); and the located card's build equal to the
//!   running process's (`FooterFacts::version`, the rule
//!   `SessionTimeline::note_claude_card` keeps a card by), so a dead process's
//!   card left on the screen after a relaunch stays bare.
//! * THE LAYOUT ([`layout`]). The box holds the card's three rows, and the
//!   line's row under them when that row is blank across the line's span (the
//!   line goes there at the card's text column); otherwise the line goes on
//!   the banner row two cells past the banner's end, if those cells are blank,
//!   and the box keeps three rows; otherwise there is no line. A narrow pane
//!   drops ` · autopilot …` first, then steps the version down
//!   (`menu::VersionRung::Short`; a release has none), then the line. The
//!   kitty rides right of the card's first two rows, after a trail of 8 to 20
//!   cells, only where those cells are blank and leave four cells of stars
//!   and the border inside the pane; otherwise the kitty and its trail go and
//!   the border and the line stay.
//! * THE STREAMS. The border and the trail (the emitter's `under`) go to
//!   `RenderInput::glow_under` — WINDOW-ABSOLUTE pixels, drawn beneath the
//!   glyph ink; the kitty and the stars (`over`) to `RenderInput::nova_add` —
//!   GRID-INTERIOR pixels, which both renderers offset by `(pad, grid_top)`
//!   (CPU `draw_nova`, GPU `nova_add` instances). One scene is emitted in
//!   window space and the `over` quads are translated into the nova stream's
//!   space (`x − pad`, `y − grid_top`); in both streams, at splice time (the
//!   strip already prepended), a quad's `row` is its FRAME row — the
//!   renderers' row gate, which is the frame's `cells` index. The line is
//!   written into the frame's cells through the footer's own undoable write
//!   (one `FooterUndo` slot), coloured by `RenderInput::ink` (kept sorted and
//!   unique; another producer's ink keeps its cells) and led by a drawn
//!   `DecoGlyph::Star4` (`✦`), never a font glyph that may be missing.
//! * GLASS ONLY. No grid cell, copy, `text`/`screen` read or recording
//!   changes: the cells are undone with the footer's (`claude_footer::undo`)
//!   and the light is taken back by [`strip`] at the same points, so a frame
//!   that re-splices its scratch without refilling the effect streams (a
//!   capture after a live present: `cell_frame_into` never writes them) never
//!   doubles the light.
//! * EPISODES, THEN STILLNESS. An episode (`harness_card::EPISODE_MS`) is armed
//!   when a card is first decorated for a process (its group and its card's
//!   build), when the session's Autopilot changes (into Cruise or Land), and
//!   when the window regains focus ([`App::rearm_claude_cards`]). A card is
//!   the session's, not the leaf's: a pane that moves in the plan (zoomed, a
//!   sibling split or closed to its left) keeps its card and its episode.
//!   While it plays, each frame is due a `FRAME_MS` after the last one drawn
//!   — an instant no other wake can push on — and is judged on every wake
//!   (`App::claude_cards_due`, beside the band's motion). After it the frame
//!   is static: the repaint key's term is constant ([`App::claude_card_term`])
//!   and no wake is armed ([`App::claude_card_deadline`],
//!   `metrics::DeadlineOwner::ClaudeCard`); nor is one while the window's
//!   tab in front does not show the card's session (Settings, where no
//!   splice draws or hides the card). Reduced motion
//!   and an unfocused window get the static frame from the start; serious
//!   mode drops the kitty, trail and stars.
//! * UNDER THE TAB MENU. The tab context menu is painted after the card and
//!   hangs from the first grid row, where a fresh card sits: it cuts the
//!   card's light, ink and star out of its rectangle ([`cut_out`]), keeping
//!   the rest of the card on the glass.
//! * THE KITTY wears the launch kitty's look (`App::launch_kitty`, the
//!   process's base cat): its coat and iris stops resolved through the cat
//!   baker's own `ResolvedFills::from_indices` for the pane's polarity (a black
//!   coat lifted on a dark ground, as the cursor cat's is).
//! * WHAT A FRAME COSTS (owner, 2026-10-15: "the user experince must be
//!   extremely smooth and zero latency!"). A card at rest is REPLAYED: its
//!   settled frame is kept beside it (`harness_card::SettledFrame`) and put
//!   on the glass from there while the card stays still on the same scene —
//!   its kitty and stars kept moved into the nova stream's pixels
//!   ([`CardEntry::nova`]) — never drawn again, and a frame that is drawn
//!   makes no fingerprint (`HarnessCard::draw_kept`); its reading is kept while no row it reads changes — the card's
//!   three, and below them only a row that now draws a banner
//!   ([`CardReading::holds`]); its layout while the cells the layout read
//!   say what they said — known without a look while the pane's text is the
//!   same rebuild, else by testing again, once per rebuild, exactly the
//!   cells the layout read and no others ([`Seen`]: the layout reads each
//!   cell once, and no further along a row than its decision needs, so a
//!   wide pane costs it no more than a narrow one; laid anew while a find
//!   bar is up, whose tint writes cells no revision moves) — and its line —
//!   cells, ink and star — while what it shows holds. A card laid anew (it
//!   moved, the pane resized) notes what its layout read as it reads it, and
//!   reads no cell again. So a keystroke in the composer, a spinner or
//!   streamed output under the card, a chrome row's refill and a split's
//!   compose, each a rebuild of the pane's text, read no card and lay none
//!   out. Nothing is allocated for it once its buffers are grown, the
//!   window's repaint key and wake judges read no clock for it, the splice
//!   takes no lock for it (the process comes from the footer's own timeline
//!   lock), and a window with no card on its glass reads no clock, no motion
//!   policy and no geometry at all. The clock is read where an episode is
//!   live — its frames, the arm that starts it — and nowhere else, through
//!   [`card_now`] (`work_counts`, asserted by `claude_card_tests`'
//!   steady-state tests); a card hidden mid-episode stops asking once its
//!   episode wants nothing more ([`App::claude_cards_due`]).

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use aterm_core::render::{DecoBlend, DecoGlyph, GlowQuad, InkCell, RenderInput, WordDecoration};
use aterm_core::terminal::RenderCell;
use aterm_effects::harness_card::{
    CardPalette, CardScene, FRAME_MS, HarnessCard, SettledFrame, kitty_cols,
};
use aterm_phase::CardPlace;

use crate::autopilot::AutopilotView;
use crate::{App, WindowId};

/// The trail's shortest run (cells): with less room the kitty and its trail
/// are left out.
const TRAIL_MIN: usize = 8;
/// The trail's longest run (cells): more room is left blank.
const TRAIL_MAX: usize = 20;
/// Cells kept right of the kitty for its stars and the border (§3: "leave ≥ 4
/// cells right of it").
const KITTY_MARGIN: usize = 4;
/// The `✦` and the space after it, ahead of the line's words.
const LINE_LEAD: usize = 2;
/// The `✦`'s opacity on a light ground (source-over ink); on a dark ground it
/// is added light at full coverage.
const STAR_INK_ALPHA: u8 = 230;
/// What the card reads off a pane's rows: where the newest launch card sits
/// ([`CardPlace`]) and the build its banner names. Read at most once per
/// rebuild of the pane's text (`claude_footer::PaneTextCache::launch_card`),
/// only when a card is wanted there, and not at all where the rebuild
/// changed no row the last reading read ([`CardReading::holds`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardRead {
    pub(crate) place: CardPlace,
    pub(crate) version: Box<str>,
}

/// One reading of a pane's rows for the card ([`read_card`]): the card, and
/// where the reader's scan stopped (`aterm_phase::launch_card_scan`) — what
/// lets the next rebuild of the rows keep it without a scan
/// ([`Self::holds`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct CardReading {
    /// The newest row that draws a banner, as the scan found it (`Some(None)`:
    /// none does); `None` when the reader panicked, and the reading is never
    /// kept.
    banner: Option<Option<usize>>,
    /// The card, `None` when none is placed or its build does not parse.
    pub(crate) read: Option<Arc<CardRead>>,
}

impl CardReading {
    /// Whether this reading — of a pane's rows before the rows `changed`
    /// names (ascending) changed — is the reading of `rows` as they are now,
    /// known without a scan (`aterm_phase::launch_card_unmoved`, behind the
    /// reader's panic fence): a keystroke, a spinner or a streamed row under
    /// the card, a pure revision bump and any row above the card keep it.
    /// Never for a reading the reader could not take.
    pub(crate) fn holds(&self, session: u64, rows: &[String], changed: &[usize]) -> bool {
        let Some(banner) = self.banner else {
            return false;
        };
        crate::reader_guard::read_or_none(session, "harness card", rows, || {
            aterm_phase::launch_card_unmoved(rows, banner, changed)
        })
        .unwrap_or(false)
    }
}

/// The launch card on `rows` and the build it names, behind the reader's
/// panic fence (`reader_guard`): no card when none is placed (narrow, cut,
/// redesigned — the card goes bare, never wrong) or its banner's version
/// does not parse, and nothing known when the reader panicked. A card equal
/// to `before` — the last reading's, where it had one — IS `before`,
/// shared: a rebuild that moved the card nowhere allocates nothing for it.
pub(crate) fn read_card(
    session: u64,
    rows: &[String],
    before: Option<Arc<CardRead>>,
) -> CardReading {
    #[cfg(test)]
    crate::work_counts::card_read();
    crate::reader_guard::read_or_none(session, "harness card", rows, || {
        let (banner, place) = aterm_phase::launch_card_scan(rows);
        let read = place.and_then(|place| {
            let version = aterm_phase::launch_card_version(rows, &place)?;
            Some(match before {
                Some(was) if was.place == place && *was.version == *version => was,
                _ => Arc::new(CardRead {
                    place,
                    version: version.into(),
                }),
            })
        });
        CardReading {
            banner: Some(banner),
            read,
        }
    })
    .unwrap_or_default()
}

/// Where one pane sits in the frame being presented: the chrome rows above
/// the terminal grid (`strip`), the pane's origin in grid cells and its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneGeom {
    pub(crate) strip: usize,
    pub(crate) row_off: usize,
    pub(crate) col_off: usize,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
}

impl PaneGeom {
    /// The frame row of pane row `r`.
    fn frame_row(self, r: usize) -> usize {
        self.strip + self.row_off + r
    }

    /// The frame column of pane column `c`.
    fn frame_col(self, c: usize) -> usize {
        self.col_off + c
    }
}

/// One Claude Code pane the footer's splice found on the glass, gathered for
/// [`App::compose_claude_cards`].
#[derive(Clone, Debug)]
pub(crate) struct CardLeaf {
    leaf_index: usize,
    session: u64,
    pane: PaneGeom,
    /// The pane's blank cell (its live default colours).
    blank: RenderCell,
    /// The session's effective Autopilot, wrapped or not: a change into
    /// Cruise or Land arms an episode.
    view: AutopilotView,
    /// The card to decorate — the pane cache's reading, shared — the
    /// process it belongs to (its group) and the number of the CURRENT
    /// rebuild of the pane's text, which names this frame's cells (the
    /// reading itself may be kept from an earlier rebuild,
    /// [`CardReading::holds`]): `Some` only where every gate holds.
    card: Option<(Arc<CardRead>, i32, u64)>,
}

/// The line under one card, kept on its card ([`CardEntry::line`]) and
/// rebuilt only when what it shows moves: written by the footer's splice
/// into the same undoable write as the rule (`claude_footer::write_edits`,
/// borrowed, never copied), its ink and its star put on the glass only where
/// the write took ([`ink_lines`]).
#[derive(Clone, Debug)]
pub(crate) struct CardLine {
    /// Drawn this frame.
    on: bool,
    /// What the cells, the ink and the star were made for.
    key: Option<LineKey>,
    /// The ink is the settled gradient, made while the card was still: it
    /// stands while the card stays still.
    settled: bool,
    frame_row: usize,
    /// The frame column of the line's first word: the write's key.
    col: usize,
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
            frame_row: 0,
            col: 0,
            cells: Vec::new(),
            ground: RenderCell::default(),
            ink: Vec::new(),
            star: WordDecoration {
                row: 0,
                col: 0,
                dx: 0,
                dy: 0,
                glyph: DecoGlyph::Star4,
                blend: DecoBlend::Add,
                color: 0,
                alpha: 0,
            },
        }
    }
}

impl CardLine {
    /// The `(frame_row, col, cells, ground)` edit the footer's write takes,
    /// the cells borrowed.
    pub(crate) fn edit(&self) -> (usize, usize, &[RenderCell], RenderCell) {
        (self.frame_row, self.col, &self.cells, self.ground)
    }

    /// Where the edit starts, as the write's record keys it.
    pub(crate) fn at(&self) -> (usize, usize) {
        (self.frame_row, self.col)
    }
}

/// What a card's line was made for ([`CardLine::key`]): its place, its words,
/// the cell its words are drawn in and the ink's palette and polarity.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LineKey {
    frame_row: usize,
    star_col: usize,
    text: &'static str,
    base: RenderCell,
    dark: bool,
    palette: CardPalette,
}

/// What a card's layout and scene were made from ([`CardEntry::laid`]),
/// beside the cells the layout read ([`CardEntry::seen`]): the pane,
/// its blank, the card's place, the Autopilot whose words the line takes,
/// the window's pixels, serious mode and the kitty's look.
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

/// One decorated card's episode and memory, per session on the window's
/// glass: the leaf it was last found at moves with the pane.
#[derive(Debug)]
struct CardEntry {
    /// The plan leaf the card was last found at: a pane that moves in the
    /// plan (zoomed, a sibling split or closed to its left) takes its card
    /// along ([`App::compose_claude_cards`]).
    leaf_index: usize,
    session: u64,
    /// The process the card was first decorated for: its group and its card's
    /// build. Another is a new card, and a new episode.
    identity: (i32, Box<str>),
    card: HarnessCard,
    /// The Autopilot the pane showed last: a move into Cruise or Land arms.
    view: AutopilotView,
    /// When the episode was last armed: the repaint key's frame clock.
    armed_at: Instant,
    /// When the card was last TICKED (a replayed settled frame reads no
    /// clock and leaves it): while its episode plays, the next frame is due
    /// a [`FRAME_MS`] after it ([`Self::due`]).
    ticked_at: Instant,
    /// Ticked by the latest splice of this window: only a card on show keys
    /// the repaint or arms a wake.
    shown: bool,
    /// The frame this card is due — an episode frame or the settled frame
    /// it owes — has been asked for, or its episode wants no frame at any
    /// later instant ([`App::claude_cards_due`]); no further wake is armed
    /// for it until a tick draws it or an arm starts another episode, so a
    /// frame that never comes spins nothing.
    asked: bool,
    /// The pane rows and columns the last frame laid the card on.
    place: (usize, usize, u16, u16),
    /// The card's last settled frame, replayed while it rests.
    kept: SettledFrame,
    /// The last frame's kitty and stars in the nova stream's grid-interior
    /// pixels ([`nova_of`]), and the `(pad, grid_top)` they were moved by:
    /// while the card is replayed from `kept` they are `kept`'s, moved —
    /// every tick sets them anew from what it drew, and a replay draws
    /// `kept`, which only a tick fills — so a frame at rest appends them as
    /// they are.
    nova: Vec<GlowQuad>,
    nova_at: Option<(u16, u16)>,
    /// The last layout and scene, and what they were made from: kept while
    /// the key holds and the cells the layout read do (`None` inside: the
    /// card did not fit).
    laid: Option<(LaidKey, Option<(Layout<'static>, CardScene)>)>,
    /// The rebuild of the pane's text (`claude_footer::PaneTextCache`) the
    /// kept layout's cells were last read in: the same rebuild, the same
    /// cells, and nothing is read to keep it.
    laid_gen: u64,
    /// The cells the kept layout read and what it found there ([`Seen`]):
    /// tested again once per rebuild of the pane's text.
    seen: Seen,
    /// The line under the card.
    line: CardLine,
}

impl CardEntry {
    /// Start an episode at `now`, the stars seeded by the session and its
    /// process, so a re-arm keeps the same constellation.
    fn arm(&mut self, now: Instant) {
        let pgid = u64::from(self.identity.0.cast_unsigned());
        self.card.arm(now, mix(self.session ^ pgid.rotate_left(32)));
        self.armed_at = now;
        self.asked = false;
    }

    /// When this card on show next wants a frame, judged at `now`: while its
    /// episode plays, a [`FRAME_MS`] after the last frame drawn — ANCHORED
    /// there, never later (`HarnessCard::deadline` is a frame ahead of
    /// whatever instant it is asked at, so a stream of other wakes, each
    /// asking anew, would push the frame on for as long as the stream lasts);
    /// the settled frame owed once the episode runs out, `now`. `None` when
    /// settled, not on show, or once asked until a tick draws it.
    fn due(&self, now: Instant) -> Option<Instant> {
        if !self.shown || self.asked {
            return None;
        }
        let wanted = self.card.deadline(now)?;
        if wanted <= now {
            return Some(wanted);
        }
        let next = self
            .ticked_at
            .checked_add(Duration::from_millis(FRAME_MS))
            .unwrap_or(wanted);
        Some(next.min(wanted))
    }

    /// Whether this card on show may want a frame — known without a clock:
    /// shown, not yet asked (nor found wanting nothing more, `asked` too),
    /// and not at rest (`HarnessCard::at_rest`).
    fn live(&self) -> bool {
        self.shown && !self.asked && !self.card.at_rest()
    }
}

/// What one splice appended to the frame's effect streams, so the next frame
/// (and a capture's re-splice of the same scratch) takes exactly that back
/// ([`strip`]). Each stream's part is kept in the stream's own order.
#[derive(Debug, Default)]
struct CardPainted {
    under: Vec<GlowQuad>,
    over: Vec<GlowQuad>,
    ink: Vec<InkCell>,
    decos: Vec<WordDecoration>,
}

impl CardPainted {
    fn is_empty(&self) -> bool {
        self.under.is_empty()
            && self.over.is_empty()
            && self.ink.is_empty()
            && self.decos.is_empty()
    }

    fn clear(&mut self) {
        self.under.clear();
        self.over.clear();
        self.ink.clear();
        self.decos.clear();
    }
}

/// The harness card's per-window state (`WindowState::claude_card`): each
/// decorated card's episode, what the last splice appended to the frame's
/// streams, and the splice's, the emitter's and the cut's scratch.
#[derive(Debug, Default)]
pub(crate) struct WindowCards {
    entries: Vec<CardEntry>,
    painted: CardPainted,
    /// The panes the footer's splice gathers for the card pass, reused
    /// ([`Self::take_leaves`]).
    leaves: Vec<CardLeaf>,
    /// The emitter's two streams for one card (`HarnessCard::tick`), reused.
    under: Vec<GlowQuad>,
    over: Vec<GlowQuad>,
    /// [`strip`]'s sorted keys, reused.
    keys: Vec<(EntryKey, bool)>,
    /// What [`layout`] reads for the card being laid anew, reused (swapped
    /// with the card's own, [`CardEntry::seen`]).
    seen: Seen,
    /// [`cut_out`]'s spares, one per stream type, reused: a cut is built in
    /// one and copied back ([`cut_each_once`]).
    spare_quads: Vec<GlowQuad>,
    spare_ink: Vec<InkCell>,
    spare_decos: Vec<WordDecoration>,
}

impl WindowCards {
    /// No card is on show: the splice returned before its card pass (the
    /// footer's `tab_status` gate), so none keys the repaint or arms a wake.
    pub(crate) fn hide_all(&mut self) {
        for entry in &mut self.entries {
            entry.shown = false;
            entry.line.on = false;
        }
    }

    /// Whether any card is on show in this window.
    pub(crate) fn any_shown(&self) -> bool {
        self.entries.iter().any(|e| e.shown)
    }

    /// The splice's list of panes for this frame's card pass, empty, its
    /// buffer the last frame's ([`App::compose_claude_cards`] hands it back).
    pub(crate) fn take_leaves(&mut self) -> Vec<CardLeaf> {
        let mut leaves = std::mem::take(&mut self.leaves);
        leaves.clear();
        leaves
    }

    /// The lines this frame draws, for the footer's write.
    pub(crate) fn lines(&self) -> impl Iterator<Item = &CardLine> {
        self.entries.iter().map(|e| &e.line).filter(|l| l.on)
    }
}

/// The frame `entry`, a card of window `ws`, wants ([`CardEntry::due`]),
/// while its session is on the glass: presented by the window's active tab
/// (zoom-aware, allocation-free: `crate::active_tab_displays_session`). A
/// tab in front that does not show it — Settings, a native tab whose frames
/// run no footer splice to draw or hide the card — keeps its last terminal
/// frame's `shown`, and must arm no wake for frames nobody draws.
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

/// Take back the light, ink and star the last splice appended to `scratch`'s
/// streams: each recorded entry once, wherever it still is, and nothing
/// else. A stream a producer has refilled since (a re-extract, a split's
/// compose: the effects pass republishes every frame) holds none of it, and
/// that part costs a look at the stream and the record's rows, no sort
/// ([`remove_each_once`]); one a later band splice scrubbed rows from
/// (`scrub_overlay_row_band`) loses the rest. Called where the footer's
/// `undo` runs — the top of a frame, the top of a compose and the top of the
/// splice — so a re-splice of an unrefilled scratch never doubles the light.
/// The record is consumed.
pub(crate) fn strip(cards: &mut WindowCards, scratch: &mut RenderInput) {
    if cards.painted.is_empty() {
        return;
    }
    let (painted, keys) = (&mut cards.painted, &mut cards.keys);
    remove_each_once(
        &mut scratch.glow_under,
        &painted.under,
        keys,
        quad_key,
        |q| q.row,
    );
    remove_each_once(&mut scratch.nova_add, &painted.over, keys, quad_key, |q| {
        q.row
    });
    remove_each_once(&mut scratch.ink, &painted.ink, keys, ink_key, |c| c.row);
    remove_each_once(
        &mut scratch.word_decorations,
        &painted.decos,
        keys,
        deco_key,
        |d| d.row,
    );
    painted.clear();
}

/// Cut what the last splice put on the glass out of the frame cells `rect`:
/// the tab context menu (`App::splice_tab_menu`), topmost chrome painted
/// after the card, hangs from the first grid row, where a fresh card sits,
/// and no light, ink or star of the card may show on it. A light quad over
/// the rectangle keeps only its parts either side of it ([`cut_quad`]); an
/// inked cell or the star inside it goes. The record is cut the same way, so
/// [`strip`] still takes back exactly what is on the glass. `cw` and `pad`
/// are the window's cell width and left padding: `glow_under` is window
/// pixels, `nova_add` grid-interior ones; in both a quad's `row` is its
/// frame row.
pub(crate) fn cut_out(
    cards: &mut WindowCards,
    scratch: &mut RenderInput,
    rect: crate::tab_menu::MenuRect,
    cw: usize,
    pad: usize,
) {
    if cards.painted.is_empty() {
        return;
    }
    let rows = rect.row..rect.row + rect.h;
    let cols = rect.col..rect.col + rect.w;
    let span = |origin: usize| (origin + rect.col * cw)..(origin + (rect.col + rect.w) * cw);
    let (window_px, grid_px) = (span(pad), span(0));
    let inside =
        |row: u16, col: u16| rows.contains(&usize::from(row)) && cols.contains(&usize::from(col));
    let WindowCards {
        painted,
        keys,
        spare_quads,
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
        &mut scratch.nova_add,
        &mut painted.over,
        keys,
        spare_quads,
        quad_key,
        |q, out| rows.contains(&usize::from(q.row)) && cut_quad(q, grid_px.clone(), out),
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

/// What is left of light quad `q` outside the pixel columns `xs`, pushed to
/// `out`: `false` (nothing pushed) when the quad is clear of them; otherwise
/// `true`, and its part left of them and its part right of them, each where
/// there is one. A piece's ends are read off the quad's own ramp
/// (`GlowQuad::color2`) at the cut, so it draws what the whole drew there,
/// within the ramp's integer rounding; a flat quad's pieces are flat.
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

/// Columns `[from, to)` of quad `q` (relative to its left edge) as a quad of
/// their own, its ends sampled on `q`'s ramp at those edges.
fn quad_piece(q: &GlowQuad, from: usize, to: usize) -> GlowQuad {
    let w = usize::from(q.w).max(1);
    // One channel's value at relative x on the line from `a` (x = 0) to `b`
    // (x = w), rounded half up.
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

/// Replace in `stream`, once each and in its place, every one of `ours`
/// that `cut` touches, by what `cut` leaves of it; and `ours` itself the
/// same way, so the record and the glass stay one (the stream's entries
/// found as [`remove_each_once`] finds them). `cut` pushes the pieces it
/// leaves (none: the entry goes) and says whether it touched the entry —
/// pushing nothing when it did not — as a function of the entry alone.
/// Each is cut into `spare` (the window's, reused) and copied back, so
/// `ours` and `stream` keep their buffers: once the spare has grown to the
/// longest stream, a frame under the menu allocates nothing. With nothing of
/// `ours` touched, nothing changes and the stream is not walked.
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

/// Mark the first unused key equal to `k` in `keys` (sorted) used: whether
/// there was one.
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

/// An entry's identity for [`remove_each_once`]: every field, packed.
type EntryKey = [u64; 3];

/// A light quad's [`EntryKey`]: position, size, both ends' colour and opacity.
fn quad_key(q: &GlowQuad) -> EntryKey {
    [
        u64::from(q.row) << 48 | u64::from(q.x) << 32 | u64::from(q.y) << 16 | u64::from(q.w),
        u64::from(q.h) << 48 | u64::from(q.alpha) << 40 | u64::from(q.color),
        u64::from(q.alpha2) << 32 | u64::from(q.color2),
    ]
}

/// An ink cell's [`EntryKey`].
fn ink_key(c: &InkCell) -> EntryKey {
    [
        u64::from(c.row) << 16 | u64::from(c.col),
        u64::from(pack(c.color)),
        0,
    ]
}

/// A word decoration's [`EntryKey`].
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

/// Remove from `stream` each of `ours` once, wherever it is, keeping the
/// rest in order: the stream's tail when it is exactly `ours` (nothing
/// touched it since); nothing when no entry of the stream can be one of ours
/// — the stream is empty, or none of its entries lies on a frame row
/// (`row`) between ours' first and last (a stream a producer refilled since,
/// whose light sits on other rows: the cursor's, a keystroke's) — known by
/// a look at each row, with no key made and no sort; otherwise one pass over
/// it against `ours`' sorted keys (`keys`, the window's reused scratch). An
/// entry of another producer equal to one of ours in every field is light of
/// the same kind in the same place; which of the two goes changes no pixel.
/// (`key` packs every field, so two entries with one key are equal: the tail
/// is compared as values, no key made.)
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

/// Put each written line's ink and star on the glass, recorded for [`strip`]:
/// the ink only on cells no other producer inked (theirs keep their cells;
/// the stream stays sorted by `(row, col)` and unique, the renderers'
/// lockstep walk — so an unstable sort, which allocates nothing, is the
/// stable one), the star after the stream's last decoration. `took` says
/// which of this frame's lines ([`WindowCards::lines`]) the footer's write
/// put on the frame, by where each starts.
pub(crate) fn ink_lines(
    cards: &mut WindowCards,
    scratch: &mut RenderInput,
    took: impl Fn((usize, usize)) -> bool,
) {
    let WindowCards {
        entries, painted, ..
    } = cards;
    let before = scratch.ink.len();
    for line in entries
        .iter()
        .map(|e| &e.line)
        .filter(|l| l.on && took(l.at()))
    {
        for cell in &line.ink {
            let key = (cell.row, cell.col);
            if scratch.ink[..before]
                .binary_search_by_key(&key, |c| (c.row, c.col))
                .is_err()
            {
                scratch.ink.push(*cell);
                painted.ink.push(*cell);
            }
        }
        scratch.word_decorations.push(line.star);
        painted.decos.push(line.star);
    }
    if scratch.ink.len() > before {
        scratch.ink.sort_unstable_by_key(|c| (c.row, c.col));
        painted.ink.sort_unstable_by_key(|c| (c.row, c.col));
    }
}

/// The card's clock: the wall clock, or a test's. Every read is counted
/// (`work_counts::harness_clock_read`): the card reads it where an episode
/// is live, and nowhere else.
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

/// One pass's clock ([`card_now`]): read at most once, and only when
/// something asks — an episode armed or drawn.
#[derive(Default)]
struct LazyNow(Option<Instant>);

impl LazyNow {
    fn get(&mut self) -> Instant {
        *self.0.get_or_insert_with(card_now)
    }
}

#[cfg(test)]
thread_local! {
    /// A test's clock for the card ([`card_now`]): episodes are played out by
    /// stepping it, never by sleeping.
    static TEST_NOW: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// SplitMix64's finalizer: a seed and a key term from plain integers.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// `[r, g, b]` as `0x00RRGGBB`.
fn pack(rgb: [u8; 3]) -> u32 {
    (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2])
}

/// A cat baker fill (`0..=1` per channel) as `0x00RRGGBB`.
fn pack_fill((r, g, b): (f32, f32, f32)) -> u32 {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

/// The line's words for `view` ([`make_line_texts`]), made once per view:
/// the build is the process's, so they never change while it runs, and a
/// frame formats nothing.
fn line_texts(view: AutopilotView) -> &'static [String] {
    static TEXTS: [OnceLock<Vec<String>>; 5] = [const { OnceLock::new() }; 5];
    let slot = match view {
        AutopilotView::Cruise => &TEXTS[0],
        AutopilotView::Land => &TEXTS[1],
        AutopilotView::Off => &TEXTS[2],
        AutopilotView::MasterOff => &TEXTS[3],
        AutopilotView::Unhosted => &TEXTS[4],
    };
    slot.get_or_init(|| make_line_texts(view))
}

/// The line's words for `view`, widest first: with the Autopilot, without it,
/// then on the short version rung (a release has none). The version is the
/// running build's own signature (`menu::version_signature`), so one build
/// reads the same here as in the menu bar.
fn make_line_texts(view: AutopilotView) -> Vec<String> {
    use crate::menu::{VersionFacts, VersionRung, version_signature};
    let facts = VersionFacts::this_build();
    let mut texts = Vec::with_capacity(3);
    if let Some(full) = version_signature(facts, VersionRung::Full) {
        let head = format!("enhanced by aterm harness {full}");
        texts.push(format!(
            "{head} \u{b7} autopilot {} {}",
            view.glyph(),
            view.wire()
        ));
        texts.push(head);
    }
    if let Some(short) = version_signature(facts, VersionRung::Short) {
        texts.push(format!("enhanced by aterm harness {short}"));
    }
    texts
}

/// How many cells `text` takes (`aterm_grapheme::char_width`, a control or
/// zero-width char taking none).
fn text_cells(text: &str) -> usize {
    text.chars().map(aterm_grapheme::char_width).sum()
}

/// The frame cells of one pane, read for the layout: which cells are BLANK —
/// a space in the pane's blank style with nothing in the scratch's side
/// channels (a cluster, a combining mark, an image) — and which hold the
/// right half of a wide glyph. A cell past a row the engine trimmed is blank.
struct PaneCells<'a> {
    scratch: &'a RenderInput,
    pane: PaneGeom,
    blank: RenderCell,
}

impl<'a> PaneCells<'a> {
    /// Pane row `r`, for blank tests along it ([`PaneRow`]): its cells looked
    /// up once, and whether the scratch's side channels hold anything there.
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

    /// Whether pane cell `(r, c)` is the right half of a wide glyph; a cell
    /// outside the pane is not. (A card row's text reaches past the pane's
    /// last column only when it fills the pane, and the layout is then the
    /// same either way: no line beside it, no kitty, the box at the border.)
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

/// One pane row of a frame, read for blank tests along it ([`PaneCells::row`]).
struct PaneRow<'a> {
    scratch: &'a RenderInput,
    /// Its frame row.
    row: usize,
    /// Its frame cells; `None` for a row outside the pane or the frame,
    /// where no cell is blank.
    cells: Option<&'a [RenderCell]>,
    /// Whether a side channel (a cluster, a combining mark, an image) holds
    /// an entry on it: only then is each cell looked for there.
    side: bool,
    col_off: usize,
    cols: usize,
    blank: RenderCell,
}

impl PaneRow<'_> {
    /// Whether pane column `c` is blank, as the footer's rule test reads a
    /// plain cell (`claude_footer::rule_mask`); a cell outside the pane is
    /// not, and one past a row the engine trimmed is.
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

    /// How many blank cells lead the columns `[from, to)`: each read in
    /// turn, up to the first that is not.
    fn run(&self, from: usize, to: usize) -> usize {
        (from..to).take_while(|&c| self.blank(c)).count()
    }
}

/// Whether the card at `place` sits whole in `pane`, as [`layout`] needs.
fn sits_in(place: &CardPlace, pane: PaneGeom) -> bool {
    place.rows == 3 && place.top + place.rows <= pane.rows && pane.cols >= 2
}

/// One test a [`layout`] made of a pane's cells, and what it found
/// ([`Seen`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    /// Pane cell `(row, col)` was (`wide`) or was not a wide glyph's right
    /// half.
    Wide { row: usize, col: usize, wide: bool },
    /// Pane row `row`, read from column `from` toward `to`: its first `len`
    /// cells blank and — `len` short of `to - from` — the next one not, the
    /// one that ended the run (and the last one read).
    Blanks {
        row: usize,
        from: usize,
        to: usize,
        len: usize,
    },
}

/// What a [`layout`] read off the pane's cells: every test it made, and what
/// each found. The layout is a function of its key ([`LaidKey`]: the place,
/// the pane and its blank, the kitty's width, the line's words) and of these
/// alone — each test it makes is chosen by the key and the tests before it —
/// so on cells where every test finds what it found, the layout takes the
/// same path to the same result ([`Self::hold`]). Noting them costs a fresh
/// layout a push per run, never a cell read again; testing them again reads
/// exactly the cells the layout read — no run to the border, no cell twice.
#[derive(Clone, Debug, Default)]
struct Seen(Vec<Probe>);

impl Seen {
    /// Whether pane cell `(row, col)` is a wide glyph's right half, noted.
    fn wide(&mut self, cells: &PaneCells<'_>, row: usize, col: usize) -> bool {
        let wide = cells.wide(row, col);
        self.0.push(Probe::Wide { row, col, wide });
        wide
    }

    /// How many blank cells lead pane row `row` from column `from`, read no
    /// further than `to` ([`PaneRow::run`]), noted; nothing is read, or
    /// noted, for an empty span.
    fn blanks(&mut self, cells: &PaneCells<'_>, row: usize, from: usize, to: usize) -> usize {
        if from >= to {
            return 0;
        }
        let len = cells.row(row).run(from, to);
        self.0.push(Probe::Blanks { row, from, to, len });
        len
    }

    /// Whether every test noted finds on `cells` what it found — each run
    /// read as far as the layout read it, the cell that ended it included —
    /// so the layout these were noted for is the layout of `cells` under the
    /// same key.
    fn hold(&self, cells: &PaneCells<'_>) -> bool {
        self.0.iter().all(|probe| match *probe {
            Probe::Wide { row, col, wide } => cells.wide(row, col) == wide,
            Probe::Blanks { row, from, to, len } => {
                cells.row(row).run(from, to.min(from + len + 1)) == len
            }
        })
    }
}

/// Whether a column-keyed side channel holds an entry at `(row, col)`.
fn touches<T>(channel: &[Vec<(usize, T)>], row: usize, col: usize) -> bool {
    channel
        .get(row)
        .is_some_and(|entries| entries.iter().any(|(c, _)| *c == col))
}

/// Where the line goes: its pane row, the pane column of its `✦`, its words
/// (one of the layout's texts) and the cells they take.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinePlace<'t> {
    row: usize,
    col: usize,
    text: &'t str,
    cells: usize,
}

/// The card laid out on one pane, in pane cells.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Layout<'t> {
    /// How many rows the box holds from the banner row: the card's three and
    /// the line's when it sits under the card.
    rows: usize,
    /// The box's inner width, the column its right border is drawn in
    /// (`CardScene::cols`): never past the pane's last column.
    cols: usize,
    /// The trail, `[start, end)`, on the card's first two rows.
    trail: Option<(usize, usize)>,
    /// The kitty's left column on the card's first two rows.
    kitty_col: Option<usize>,
    line: Option<LinePlace<'t>>,
}

/// Lay the card at `place` out on the pane (`cells`), the kitty `kc` cells
/// wide (`harness_card::kitty_cols`; `0`: no kitty), the line one of `texts`
/// (widest first), noting every cell test it makes in `seen` (cleared first;
/// [`Seen`]). The module header has the rules; `None` when the card does not
/// sit whole in the pane. A cell is read once, and no further along a row
/// than the decision it serves needs: the line's run as far as its widest
/// words inside the border reach, the kitty's as far as decides where it
/// sits.
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
    // The right border is drawn IN this column at most, so it stays inside
    // the pane.
    let max_border = pane.cols - 1;
    // A row whose last glyph is wide ends one cell later than its text says
    // (the spacer cell reads as a trailing blank; `CardPlace::ends`).
    let mut ends = place.ends;
    for (k, end) in ends.iter_mut().enumerate() {
        if seen.wide(cells, top + k, *end) {
            *end += 1;
        }
    }
    // The line: under the card at its text column, else on the banner row two
    // cells past the banner — each with the widest words that fit, inside
    // the border, over cells that are blank. The row is read once from
    // `col`, as far as the widest words inside the border reach: words fit
    // where they end inside it and no further than its blank run.
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
    // The banner row's text ends where the line does when the line is on it.
    let row0_end = match (&line, line_end) {
        (Some(l), Some(end)) if l.row == top => end,
        _ => ends[0],
    };
    let text_end = ends
        .iter()
        .copied()
        .chain(line_end)
        .chain([row0_end])
        .max()
        .unwrap_or(0);
    // The kitty and its trail, right of the first two rows' text: the trail
    // from two cells past it, then the kitty, then its margin of stars and
    // the border — every cell blank on both rows, all inside the pane.
    let mut kitty = None;
    if kc > 0 {
        let start = row0_end.max(ends[1]) + 2;
        // `limit`: the first column from `start` where either row is not
        // blank — read no further than `reach`. Blank to there, the kitty
        // sits where any more room would put it (the trail at its longest,
        // the box's right edge at the kitty's margin or the text, whichever
        // is further), so a wide pane's rows are never read to the border
        // for it.
        let reach = (start + TRAIL_MAX + kc + KITTY_MARGIN)
            .max(text_end)
            .min(max_border);
        let blank = seen.blanks(cells, top, start, reach);
        let limit = start + seen.blanks(cells, top + 1, start, start + blank);
        let room = limit.saturating_sub(start + kc + KITTY_MARGIN);
        if start < limit && room >= TRAIL_MIN {
            let kitty_col = start + room.min(TRAIL_MAX);
            let cols = (kitty_col + kc + KITTY_MARGIN).max(text_end);
            // The stars fill the box right of the trail's start on these two
            // rows: none may land on text there.
            if cols <= limit {
                kitty = Some((start, kitty_col, cols));
            }
        }
    }
    let cols = kitty.map_or(text_end, |(_, _, cols)| cols).min(max_border);
    Some(Layout {
        rows,
        cols,
        trail: kitty.map(|(start, kitty_col, _)| (start, kitty_col)),
        kitty_col: kitty.map(|(_, kitty_col, _)| kitty_col),
        line,
    })
}

/// A window's pixel geometry for the card: the cell, the left padding and
/// the y of the frame's row 0 (`pad_top + head`), window-absolute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pixels {
    cw: usize,
    ch: usize,
    pad: usize,
    grid_top: usize,
}

/// `v` as an `i32` pixel, saturating.
fn pixel(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// `over` — quads in window pixels — in the nova stream's grid-interior
/// pixels, into `out` (emptied first): each moved left by `shift.0` (the
/// window's padding) and up by `shift.1` (its padding and head), and one
/// that would land left of or above the grid's origin left out.
fn nova_of(over: &[GlowQuad], shift: (u16, u16), out: &mut Vec<GlowQuad>) {
    #[cfg(test)]
    crate::work_counts::card_nova_moved(over.len());
    out.clear();
    out.extend(over.iter().filter_map(|quad| {
        Some(GlowQuad {
            x: quad.x.checked_sub(shift.0)?,
            y: quad.y.checked_sub(shift.1)?,
            ..*quad
        })
    }));
}

/// `v` as a `u16` (a cell index or a pixel size), saturating.
fn u16_of(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// The scene the emitter draws for a card laid out on `pane`, in WINDOW
/// pixels: grid cell `(0, 0)` of the pane at `(pad + col_off·cw, grid_top +
/// (strip + row_off)·ch)`, its rows tagged with their frame rows, the left
/// border in the window's padding for a pane at the window's left edge, and
/// the pane's rect as the clip.
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
    dark: bool,
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
        palette: if view == AutopilotView::Land {
            CardPalette::Land
        } else {
            CardPalette::Cruise
        },
        dark,
        serious,
        coat,
        eye,
    }
}

/// Whether a pane's ground is dark: the card's light is added there, and
/// inked on a light one.
fn dark_ground(blank: RenderCell) -> bool {
    aterm_effects::color_math::relative_luminance(pack(blank.bg)) < 0.5
}

/// The kitty's width in cells for a card laid for `key`: none in serious
/// mode, where the box hugs the card's text.
fn kitty_width(key: &LaidKey) -> usize {
    if key.serious {
        0
    } else {
        usize::from(kitty_cols(u16_of(key.px.cw), u16_of(key.px.ch)))
    }
}

/// The pane cells a card laid for `key` reads.
fn pane_cells<'a>(scratch: &'a RenderInput, key: &LaidKey) -> PaneCells<'a> {
    PaneCells {
        scratch,
        pane: key.pane,
        blank: key.blank,
    }
}

/// Lay a card out and make its scene ([`layout`], [`scene`]) for `key`,
/// noting the cells the layout read in `seen`: `None` where it does not fit.
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
        line_texts(key.view),
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
        dark,
        key.serious,
        (pack_fill(fills.coat), pack_fill(fills.iris)),
    );
    Some((lay, scene))
}

/// Make `line` the line laid at `place` on `pane` for this frame: its cells
/// (in the footer's value colour: the card's own gray — its working
/// directory's — lifted toward the pane's ink), its ink and its star. What
/// is already there is kept where it shows the same: the cells while their
/// key holds, the ink while the card also stays still (`now` is `None` for a
/// card whose frame was replayed: still, and no clock read).
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
    now: Option<Instant>,
    animate: bool,
) {
    let frame_row = pane.frame_row(place.row);
    let star_col = pane.frame_col(place.col);
    let gray = scratch
        .cells
        .get(pane.frame_row(read.place.top + 2))
        .and_then(|row| row.get(pane.frame_col(read.place.text_col)))
        .map_or(ground.fg, |cell| cell.fg);
    let dark = dark_ground(ground);
    let key = LineKey {
        frame_row,
        star_col,
        text: place.text,
        base: RenderCell {
            fg: crate::claude_footer::value_fg(gray, ground.fg),
            ..ground
        },
        dark,
        palette,
    };
    line.on = true;
    let still = card.is_still() || !animate;
    if line.key == Some(key) && line.settled && still {
        return;
    }
    if line.key != Some(key) {
        line.frame_row = frame_row;
        line.col = star_col + LINE_LEAD;
        line.ground = ground;
        line.cells.clear();
        for ch in place.text.chars() {
            let cell = RenderCell { ch, ..key.base };
            match aterm_grapheme::char_width(ch) {
                0 => {}
                1 => line.cells.push(cell),
                _ => {
                    line.cells.push(cell);
                    line.cells.push(RenderCell {
                        ch: ' ',
                        wide: true,
                        ..key.base
                    });
                }
            }
        }
        line.key = Some(key);
    }
    line.ink.clear();
    #[cfg(test)]
    crate::work_counts::card_line_inked();
    card.line_ink_into(
        now.filter(|_| !still),
        u16_of(frame_row),
        u16_of(line.col),
        u16_of(line.cells.len()),
        dark,
        animate,
        &mut line.ink,
    );
    line.settled = still;
    let star_rgb = line.ink.first().map_or(pack(ground.fg), |c| pack(c.color));
    line.star = WordDecoration {
        row: u16_of(frame_row),
        col: u16_of(star_col),
        dx: 0,
        dy: 0,
        glyph: DecoGlyph::Star4,
        blend: if dark {
            DecoBlend::Add
        } else {
            DecoBlend::Over
        },
        color: star_rgb,
        alpha: if dark { u8::MAX } else { STAR_INK_ALPHA },
    };
}

/// The footer's splice found a Claude Code pane on the glass (live viewport,
/// facts in hand): gather what the card pass needs of it — the session's
/// effective Autopilot `view` and whether this host supervises it
/// (`supervised`), both read ONCE this frame by the caller
/// ([`App::session_autopilot`]), and, where every gate holds (module header:
/// `card_on` is the `[claude_footer] card` switch), the card read off the
/// pane's cached text (`texts`) and the process it belongs to. `process` is
/// that process — its group and the build its facts name — as the caller
/// read them under its own timeline lock. A free function over the text
/// cache alone, with no `App`: it can reach no session, so it takes no lock
/// (the PTY reader holds the timeline's while Claude streams) — a second
/// one cannot be put back here without the compiler saying so — and nothing
/// is copied: the reading is the cache's.
#[allow(
    clippy::too_many_arguments,
    reason = "the pane's place, its session, its geometry and ground, its Autopilot and its process"
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
    process: Option<(i32, &str)>,
) -> CardLeaf {
    let wrapped = card_on && view.carries() && supervised;
    let card = wrapped
        .then(|| {
            let (pgid, version) = process?;
            let (read, text_gen) = texts.launch_card(leaf_index, session)?;
            // A card of another build is a process that is gone.
            (*read.version == *version).then_some((read, pgid, text_gen))
        })
        .flatten();
    CardLeaf {
        leaf_index,
        session,
        pane,
        blank,
        view,
        card,
    }
}

/// What a decorated card is drawn with in one window this frame: the motion
/// verdict, serious mode, the window's pixels and the kitty's look.
#[derive(Clone, Copy)]
struct Look {
    animate: bool,
    serious: bool,
    px: Pixels,
    kitty: (u8, u8),
}

impl App {
    /// What window `wid` draws a decorated card with this frame ([`Look`]).
    fn card_look(&self, wid: WindowId, focused: bool) -> Look {
        let motion = crate::motion::MotionEffect::HarnessCard;
        let animate = self
            .effect_policy(motion, self.motion_focus(wid, focused))
            .animate(motion);
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

    /// The card pass of the footer's splice: decorate each gathered pane's
    /// card on window `wid`'s frame — the light appended to its streams and
    /// recorded for [`strip`] — and make each its line, which the footer's
    /// write then takes ([`WindowCards::lines`]). Episodes are armed here (a
    /// new process's card, an Autopilot that moved), the cards of closed
    /// sessions are forgotten, and every card not decorated this frame stops
    /// keying the repaint. `leaves` is handed back to the window
    /// ([`WindowCards::take_leaves`]), empty.
    ///
    /// A card at rest costs a copy (module header: WHAT A FRAME COSTS): the
    /// clock is read only to arm an episode or draw one of its frames, and
    /// the motion policy, the window's pixels and the kitty's look only
    /// where a card is decorated.
    pub(crate) fn compose_claude_cards(&mut self, wid: WindowId, mut leaves: Vec<CardLeaf>) {
        let Some(focused) = self.windows.get(&wid).map(|ws| ws.focused) else {
            return;
        };
        // What the card is drawn with, read before the window is borrowed —
        // and only where one is decorated.
        let look = leaves
            .iter()
            .any(|leaf| leaf.card.is_some())
            .then(|| self.card_look(wid, focused));
        let pool = &self.pool;
        let Some(ws) = self.windows.get_mut(&wid).map(Box::as_mut) else {
            return;
        };
        // A find bar's tint writes cells no revision moves: the layout is
        // laid anew while one is up.
        let find_up = ws.search.is_some();
        let crate::WindowState {
            claude_card: cards,
            input_scratch: scratch,
            ..
        } = ws;
        strip(cards, scratch);
        // A card is forgotten with its session, not when its tab goes behind
        // another: coming back to a tab is not a new card, and replays nothing.
        cards.entries.retain(|e| pool.get(e.session).is_some());
        cards.hide_all();
        let mut clock = LazyNow::default();
        let WindowCards {
            entries,
            painted,
            under,
            over,
            seen,
            ..
        } = cards;
        for leaf in leaves.drain(..) {
            // The leaf's card: the one found at this leaf last, else its
            // session's from another leaf and not on show this frame — the
            // pane moved in the plan (zoomed, a sibling split or closed to
            // its left), and its card, episode and all, moves with it.
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
            let Some(((read, pgid, text_gen), look)) = leaf.card.as_ref().zip(look) else {
                // Not decorated this frame: follow its Autopilot only into a
                // mode that keeps the card bare (Off, the master switch), so a
                // move back into Cruise or Land is a change that arms. A
                // Cruise or Land frame drawn bare is no such move: Off
                // releases the session's claim, and the frame between the word
                // going back to Cruise and the new worker's claim (tens of ms)
                // is drawn bare — recording Cruise there would leave the
                // decorated frame after it no change to arm on. The same keeps
                // a card scrolled back, or under another build, from arming
                // anything when it shows again under the mode it had.
                if let Some(at) = at
                    && !leaf.view.carries()
                {
                    entries[at].view = leaf.view;
                }
                continue;
            };
            let key = LaidKey {
                pane: leaf.pane,
                blank: leaf.blank,
                place: read.place,
                view: leaf.view,
                px: look.px,
                serious: look.serious,
                kitty: look.kitty,
            };
            let same = |e: &CardEntry| e.identity.0 == *pgid && *e.identity.1 == *read.version;
            // The layout and the scene: the card's own while their key holds
            // and the cells its layout read say what they said — known
            // without a look while the pane's text is the same rebuild, else
            // by testing those cells again, once for the rebuild ([`Seen`]: a
            // keystroke, a spinner or a re-extract that moved none of them
            // keeps the layout). Laid anew, the layout notes what it reads as
            // it reads it, and no cell is read again for it.
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
            // Kept for the next frame with the cells it read (a fresh
            // layout's, swapped in) — never one laid over a find bar's tint,
            // which goes with no rebuild.
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
                continue;
            };
            let at = match at {
                Some(at) if same(&entries[at]) => {
                    if entries[at].view != leaf.view {
                        entries[at].arm(clock.get());
                    }
                    at
                }
                // First decorated for this process: a new card, a new episode.
                found => {
                    let now = clock.get();
                    let mut entry = CardEntry {
                        leaf_index: leaf.leaf_index,
                        session: leaf.session,
                        identity: (*pgid, read.version.clone()),
                        card: HarnessCard::default(),
                        view: leaf.view,
                        armed_at: now,
                        ticked_at: now,
                        shown: false,
                        asked: false,
                        place: (0, 0, 0, 0),
                        kept: SettledFrame::default(),
                        nova: Vec::new(),
                        nova_at: None,
                        laid: None,
                        laid_gen: 0,
                        seen: Seen::default(),
                        line: CardLine::default(),
                    };
                    entry.arm(now);
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
            // The nova stream's pixels are grid-interior: the `over` quads
            // move by the window's padding and head.
            let shift = (u16_of(look.px.pad), u16_of(look.px.grid_top));
            // A card at rest is replayed: its settled frame put on the
            // glass from where it is kept, no clock read; an episode's frame
            // is drawn on the clock.
            let ticked = if entry.card.replay(&scene, look.animate, &entry.kept) {
                if entry.nova_at != Some(shift) {
                    nova_of(entry.kept.over(), shift, &mut entry.nova);
                    entry.nova_at = Some(shift);
                }
                // The border and the trail: window pixels, under the glyph ink.
                scratch.glow_under.extend_from_slice(entry.kept.under());
                painted.under.extend_from_slice(entry.kept.under());
                None
            } else {
                // Drawn: its fingerprint, which nothing here reads, is
                // never made.
                let now = clock.get();
                under.clear();
                over.clear();
                entry
                    .card
                    .draw_kept(now, &scene, look.animate, &mut entry.kept, under, over);
                nova_of(over, shift, &mut entry.nova);
                entry.nova_at = Some(shift);
                scratch.glow_under.extend_from_slice(under);
                painted.under.extend_from_slice(under);
                Some(now)
            };
            // The kitty and the stars: the nova stream's grid-interior
            // pixels (every one lies right of the card's text, inside the
            // pane).
            scratch.nova_add.extend_from_slice(&entry.nova);
            painted.over.extend_from_slice(&entry.nova);
            #[cfg(test)]
            if ticked.is_some() {
                crate::work_counts::card_ticked();
            }
            if let Some(now) = ticked {
                entry.ticked_at = now;
            }
            entry.shown = true;
            entry.asked = false;
            entry.place = (read.place.top, read.place.text_col, scene.rows, scene.cols);
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
                    ticked,
                    look.animate,
                );
            }
            keep(entry, Some((lay, scene)), seen);
        }
        cards.leaves = leaves;
    }

    /// The harness card's repaint-key term for one Claude Code leaf (plan
    /// leaf `index` of window `ws`, showing `session`), folded into
    /// `claude_footer_fp` beside the chips' — `view` and `supervised` the
    /// session's Autopilot as the footer's key read it, once: whether the
    /// harness wraps it (the switch, its Autopilot, the claim), which
    /// Autopilot and, wrapped, whether serious mode strips it to its border
    /// (a toggle on a settled card moves nothing else), and for a card on
    /// show its place plus, while its episode plays, the episode's frame
    /// clock: one step a [`FRAME_MS`]. Reads the clock only for a card that
    /// is not at rest (`HarnessCard::at_rest`); takes no lock.
    ///
    /// The term names the frame the card WANTS, never the one it last drew:
    /// the window's key is read before its splice ticks the card, so a term
    /// that folded the drawn frame (its fingerprint) would move on the frame
    /// itself, and every later wake — the pointer's, another pane's output,
    /// a 120 Hz display's — would find the key moved and draw again, an
    /// episode then costing a frame a wake instead of one a [`FRAME_MS`].
    /// For the same reason the settled frame owed when the episode runs out
    /// keys as the settled card does: the clock's step goes, the key moves
    /// once, and the frame that draws the card settled leaves it where the
    /// next wake reads it.
    pub(crate) fn claude_card_term(
        &self,
        ws: &crate::WindowState,
        index: usize,
        session: u64,
        view: AutopilotView,
        supervised: bool,
    ) -> u64 {
        let wrapped = self.config.claude_footer_card() && view.carries() && supervised;
        let serious = wrapped
            && !self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::HarnessCard);
        let mut h = mix(u64::from(wrapped) | (view as u64) << 1 | u64::from(serious) << 8);
        if wrapped
            && let Some(entry) = ws
                .claude_card
                .entries
                .iter()
                .find(|e| e.shown && (e.leaf_index, e.session) == (index, session))
        {
            let (top, col, rows, cols) = entry.place;
            h ^= mix((top as u64) << 32 | col as u64)
                ^ (u64::from(rows) << 16 | u64::from(cols)).rotate_left(40);
            if !entry.card.at_rest() {
                let now = card_now();
                if entry.card.deadline(now).is_some_and(|due| due > now) {
                    let ms = now.saturating_duration_since(entry.armed_at).as_millis();
                    h ^= mix(1 + u64::try_from(ms).unwrap_or(u64::MAX) / FRAME_MS);
                }
            }
        }
        h
    }

    /// Whether any card on any window's glass may want a frame — known
    /// without a clock ([`CardEntry::live`]): the wake judges read the clock
    /// and walk the windows only when one does, so an idle desktop, a
    /// settled card and a window with no card cost every wake nothing.
    pub(crate) fn claude_cards_live(&self) -> bool {
        self.windows
            .values()
            .any(|ws| ws.claude_card.entries.iter().any(CardEntry::live))
    }

    /// The card's wake (`metrics::DeadlineOwner::ClaudeCard`): the nearest
    /// frame a card on glass wants ([`wanted_frame`]) — while an episode
    /// plays, a frame after the last one drawn (an instant later wakes do
    /// not move); `now` once for the settled frame it owes — each asked once
    /// ([`Self::claude_cards_due`]) and not again until a frame draws it;
    /// and `None` once every card has settled, so an idle window arms
    /// nothing. The cards are judged first: whether a window is on screen
    /// is asked of the window server (`messages_host::band_on_screen`, off
    /// macOS a round trip), and only of a window with a frame wanted.
    pub(crate) fn claude_card_deadline(&self, now: Instant) -> Option<Instant> {
        self.windows
            .values()
            .filter_map(|ws| {
                let entries = ws.claude_card.entries.iter();
                let soonest = entries
                    .filter_map(|e| wanted_frame(&self.view_store, ws, e, now))
                    .min()?;
                crate::messages_host::band_on_screen(ws).then_some(soonest)
            })
            .min()
    }

    /// [`Self::claude_card_deadline`] for the loop's wake: on the card's
    /// clock ([`card_now`]), read only while a card may want a frame
    /// ([`Self::claude_cards_live`]).
    pub(crate) fn claude_card_wake(&self) -> Option<Instant> {
        if !self.claude_cards_live() {
            return None;
        }
        self.claude_card_deadline(card_now())
    }

    /// The windows on screen whose card's frame is due at `now` — an episode
    /// frame or the settled frame owed — each frame asked for once: one that
    /// never comes arms nothing more. Their repaint key's card term has
    /// moved, so the redraw goes through. A level judged on EVERY wake, like
    /// the band's motion (`App::band_motion_due`): another window's traffic
    /// (`StartCause::WaitCancelled`) can pre-empt the card's timer for as
    /// long as it streams, and the card must not freeze on its first frame
    /// meanwhile. Allocates nothing when nothing is due, and asks the window
    /// server about no window without a frame due (the cards first, as in
    /// [`Self::claude_card_deadline`]).
    ///
    /// A card on show whose episode wants no frame at `now` wants none at
    /// any later instant until a splice ticks it or an arm starts another
    /// (`HarnessCard::deadline`: an episode run out, past its settle grace,
    /// with no frame drawn): it is marked asked here, so it stops being live
    /// ([`CardEntry::live`]) — a window minimized or hidden mid-episode,
    /// whose splice never runs, costs the wakes after it no clock read and
    /// no window judged, instead of two clock reads a wake for good. A tick
    /// (the window shown again, its tab back in front) or an arm (focus
    /// regained) makes it live again.
    pub(crate) fn claude_cards_due(&mut self, now: Instant) -> Vec<WindowId> {
        let views = &self.view_store;
        let due_at = |ws: &crate::WindowState, at: usize| {
            wanted_frame(views, ws, &ws.claude_card.entries[at], now)
                .is_some_and(|when| when <= now)
        };
        let mut due = Vec::new();
        for (wid, ws) in &mut self.windows {
            for entry in &mut ws.claude_card.entries {
                if entry.live() && entry.card.deadline(now).is_none() {
                    entry.asked = true;
                }
            }
            let count = ws.claude_card.entries.len();
            let Some(first) = (0..count).find(|&at| due_at(ws, at)) else {
                continue;
            };
            if !crate::messages_host::band_on_screen(ws) {
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

    /// [`Self::claude_cards_due`] for the loop's wake: on the card's clock
    /// ([`card_now`]), read only while a card may want a frame
    /// ([`Self::claude_cards_live`]).
    pub(crate) fn claude_card_redraws(&mut self) -> Vec<WindowId> {
        if !self.claude_cards_live() {
            return Vec::new();
        }
        self.claude_cards_due(card_now())
    }

    /// Window `wid` regained focus: every card on show there plays an episode
    /// again (§3). A window with none is untouched, and reads no clock.
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
}

#[cfg(test)]
#[path = "claude_card_tests.rs"]
mod tests;
