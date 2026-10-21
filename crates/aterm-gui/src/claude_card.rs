// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS CARD on the glass (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §3): while the aterm harness carries a Claude Code session — its Autopilot
//! Cruise or Land, and the window's host holding the session's live
//! supervisor claim — Claude Code's launch card wears an animated rainbow
//! border, a pixel-art aterm kitty riding a rainbow trail with stars, and the
//! line `✦ enhanced by aterm harness <version> · autopilot ∞ cruise` (an armed
//! timer's time after it, `∞ cruise ⤓18:00`; Land: `· autopilot ⤓ land`, in a
//! sunset; a landing's end, `· autopilot ✓ landed` — [`LineEnd`]). Off, the
//! master switch off and a window with no host show the vendor's card bare.
//! Owner, 2026-10-15: "I'd like for a rainbow border and a similar pixel art
//! with an aterm kitty and fancy rainbows and star power to show off the
//! power of the harness and a line to show that claude code is being
//! enhanced by what version of the claude code harness".
//!
//! * WHAT DRAWS IT. `aterm_effects::harness_card` is the pure emitter (the
//!   border, trail, kitty and stars as two fx contents, the line's ink); this
//!   module finds the card, lays it out on the pane and puts the emitter's
//!   output on the glass. The card is FOUND by its geometry
//!   (`aterm_phase::launch_card_scan`, never spelled here),
//!   read at most once per rebuild of the pane's text (the footer's
//!   `claude_footer::PaneTextCache::launch_card`) and kept across a rebuild
//!   that changed no row the reading reads ([`CardReading::holds`]). A card
//!   on the glass is HELD on a rebuild the reader cannot place it on while
//!   its words stand at its place — the vendor's fullscreen entrance moves
//!   the mascot, both arms up or a row low, beside words that never move —
//!   and goes on the rebuild its banner leaves ([`read_card`]).
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
//!   ([`App::session_autopilot`]; the hosted loop asks for the claim as its
//!   first request, `aterm_agent`'s `claim_at_naming`); and THE GATE
//!   (`docs/DESIGN-card-engine-2026-10-19.md` §3.8, [`admit`]): the card is
//!   the CURRENT drawer's own. The drawer is the session's published holder
//!   image while its group is the one in front
//!   (`SessionTimeline::card_gate`, read under the splice's one timeline lock
//!   a pane, facts or none — no registry row is waited on), and the card is
//!   judged by integer compares, cheapest first: the frame's fence names the
//!   drawer's token (`RenderInput::holder`, the engine's holder fence read
//!   under the extraction lock with the cells); the card is the drawer's
//!   reader's (`rows_kind`); its rows lie in one span the drawer laid and no
//!   taint stands (main), or the alternate screen was cleared while it was
//!   current (alt); and its banner passes the drawer's build policy — the
//!   image's source × its program's `aterm_phase::BannerBuild` (Claude Code:
//!   Exact, the image's own build, or for an image of unknown build the build
//!   its own registry row reports; agy and Muse Code: ProvenanceOnly, the
//!   fence alone). Each check fails closed. One verdict a pane a frame
//!   ([`GateCheck`]) serves the card and the footer's kept card
//!   (`SessionTimeline::note_claude_card`) alike. So a predecessor's card, a
//!   `cat`'d card, a card under a stale cursor and another build's card stay
//!   bare, and the drawer's own card is decorated on its first frame.
//! * THE RE-SHOW BOUND ([`RESHOW_MS`], §3.9). A card hidden by its identity
//!   or its fence (its drawer left, or the gate refused it) shows again no
//!   sooner than [`RESHOW_MS`] after its previous show began, its wake armed
//!   for that instant ([`CardEntry::due`]); a viewport hide (scroll-back, a
//!   tab, a menu) and a new drawer's first show are never held. So a
//!   flapping identity shows one card at most `1000 / RESHOW_MS` times a
//!   second, under WCAG 2.3.1's three
//!   ([`aterm_effects::TWINKLE_FLASH_BOUND_HZ`]).
//! * THE LAYOUT ([`layout`]). The box holds the card's three rows, and the
//!   line's row under them when that row is blank across the line's span (the
//!   line goes there at the card's text column); otherwise the line goes on
//!   the banner row two cells past the banner's end, if those cells are blank,
//!   and the box keeps three rows; otherwise there is no line. A narrow pane
//!   drops ` · autopilot …` first, then steps the version down
//!   (`menu::VersionRung::Short`; a release has none), then the line; an
//!   armed timer's ` ⤓18:00` goes before ` · autopilot …` does. The kitty
//!   rides right of the card's first two rows, after a trail of 8 to 20
//!   cells, only where those cells are blank and leave four cells of stars
//!   and the border inside the pane; otherwise the kitty and its trail go and
//!   the border and the line stay. Where the third row — the working
//!   directory's — is blank too, from the trail's start to the margin's end,
//!   the kitty stands three rows tall (§10.6), else two. The border stands
//!   half a row clear of the box (§10.6.8, the box's AIR) over a pane row
//!   blank from column 0 to the border's column — the row over the banner
//!   and the row under the last boxed row, each judged alone; else that
//!   side closes on the card as before: at the pane's first row (a card
//!   drawn from a cleared screen's top), under the line that launched
//!   Claude, over one of Claude's notices, at the pane's last row.
//!   The air moves the border and nothing else: the kitty, its trail and
//!   the stars stay where they were.
//! * THE LAYERS (`docs/DESIGN-card-engine-2026-10-19.md` §4.8). A card is
//!   two fx layers in `RenderInput::fx_layers`, named by its session
//!   (`harness_card::card_layer_id`): the border and the trail (the
//!   emitter's `under`) in the Under slot, WINDOW space, drawn beneath the
//!   glyph ink; the kitty, its rim and the stars (`over`) in the Over slot,
//!   GRID space, which the renderers offset by `(pad, grid_top)`. One scene
//!   is emitted in window pixels, card-local, and laid at the card's origin
//!   ([`aterm_effects::harness_card::CardFrame::layers`]), each layer
//!   clipped to the pane; the renderers cut the layers at the row bands, so
//!   nothing here splits or tags a row. The line is
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
//!   when a card is first decorated for a drawer (its snapshot token, which
//!   also seeds its stars), when the session's Autopilot changes (into Cruise or Land), and
//!   when the window regains focus ([`App::rearm_claude_cards`]); the kitty
//!   rides in on the first only, and once the card has been on the glass at
//!   rest it plays every later one in its seat (`harness_card`'s `Ride`). A
//!   card is the session's, not the leaf's: a pane that moves in the plan
//!   (zoomed, a sibling split or closed to its left) keeps its card and its
//!   episode.
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
//!   hangs from the first grid row, where a fresh card sits: its rectangle
//!   joins the frame's fx occluders (`app_render::scrub_overlay_card`), which
//!   cut the card's layers there, and the card's ink and star are cut out of
//!   it ([`cut_out`]), keeping the rest of the card on the glass.
//! * THE KITTY wears the launch kitty's look (`App::launch_kitty`, the
//!   process's base cat): its coat and iris stops resolved through the cat
//!   baker's own `ResolvedFills::from_indices` for the pane's polarity (a black
//!   coat lifted on a dark ground, as the cursor cat's is).
//! * WHAT A FRAME COSTS (owner, 2026-10-15: "the user experince must be
//!   extremely smooth and zero latency!"). A card at rest is REPLAYED: its
//!   settled frame is kept beside it (`harness_card::CardFrames`) and put
//!   on the glass from there — the same two content `Arc`s, which the
//!   renderer compares by pointer and builds nothing for — while the card
//!   stays still on a scene of the same shape (an inline scroll under it is
//!   the same frame at a new origin), never drawn again; an episode's frames
//!   refill a ring of contents in place when their prior contents are not
//!   retained elsewhere. Arts are built at the first admitted ask for a cell
//!   height, which may be a draw; publication and metric changes can queue
//!   them ahead ([`App::prefetch_card_arts`]). Its reading is kept while no row
//!   it reads changes — the card's
//!   three, and below them only a row that now draws a banner
//!   ([`CardReading::holds`]); its layout while the cells the layout read
//!   say what they said — known without a look while the pane's text is the
//!   same rebuild, else by testing again, once per rebuild, exactly the
//!   cells the layout read and no others ([`Seen`]: the layout reads each
//!   cell once, and no further along a row than its decision needs — the
//!   rows over and under the box no further than the border's column — so a
//!   wide pane costs it no more than a narrow one; laid anew while a find
//!   bar is up, whose tint writes cells no revision moves) — and its line —
//!   cells, ink and star — while what it shows holds. A card laid anew (it
//!   moved, the pane resized) notes what its layout read as it reads it, and
//!   reads no cell again. So a keystroke in the composer, a spinner or
//!   streamed output under the card, a chrome row's refill and a split's
//!   compose, each a rebuild of the pane's text, read no card and lay none
//!   out. Nothing is allocated for it once its buffers are grown, the
//!   window's repaint key and wake judges read no clock for it, the splice
//!   takes no lock for it (the gate comes from the footer's own timeline
//!   lock, one a pane a frame, facts or none), and a window with no card on its glass reads no clock, no motion
//!   policy and no geometry at all. The clock is read where an episode is
//!   live — its frames, the arm that starts it — and nowhere else, through
//!   [`card_now`] (`work_counts`, asserted by `claude_card_tests`'
//!   steady-state tests); a card hidden mid-episode stops asking once its
//!   episode wants nothing more ([`App::claude_cards_due`]).

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_core::render::{
    DecoBlend, DecoGlyph, FxLayer, InkCell, PxRect, RenderInput, WordDecoration,
};
use aterm_core::terminal::RenderCell;
use aterm_effects::harness_card::{
    CardArts, CardFrames, CardPalette, CardScene, FRAME_MS, HarnessCard, kitty_cols,
};
use aterm_phase::CardPlace;

use aterm_agent::harness::acts::TaskFace;

use crate::autopilot::AutopilotView;
use crate::claude_lights::LandAtGlass;
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
/// THE RE-SHOW BOUND (card engine §3.9, photosensitivity): a card hidden by
/// its identity or its fence (`Bare::bounds_reshow`) shows again no sooner
/// than this after its previous show BEGAN — a drawer flapping between two
/// holders, or a fence cut and laid again, flashes the card at most
/// `1000 / RESHOW_MS` times a second. A show held back is shown on time by
/// the card's own wake (`DeadlineOwner::ClaudeCard`, [`CardEntry::due`]),
/// never left bare at rest. A viewport hide (scroll-back, a tab switch, a
/// menu) is the person's and is exempt, and a card's first show is never
/// held back. `⌈1000 / 3.0⌉`: the design's 313 ms is 3.19 shows a second,
/// over WCAG 2.3.1's bound the crate certifies every flash rate under.
pub(crate) const RESHOW_MS: u64 = 334;
const _: () = assert!(RESHOW_MS as f32 >= 1000.0 / aterm_effects::TWINKLE_FLASH_BOUND_HZ);
/// What the card reads off a pane's rows: where the newest launch card sits
/// ([`CardPlace`]) and the build its banner names. Read at most once per
/// rebuild of the pane's text (`claude_footer::PaneTextCache::launch_card`),
/// only when a card is wanted there, and not at all where the rebuild
/// changed no row the last reading read ([`CardReading::holds`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardRead {
    pub(crate) place: CardPlace,
    pub(crate) version: Box<str>,
    /// The reader that placed it (`3` Claude Code, `4` agy, `1` Muse Code:
    /// [`CardReading::rows_kind`]), which the gate holds to the drawer's.
    pub(crate) kind: usize,
    /// `version` as a build, parsed once when the card is read: the gate's
    /// build policy compares integers per frame (card engine §3.8). `None`
    /// where it is no version `atpkg` spells.
    pub(crate) build: Option<atpkg::vendor_direct::Version>,
    /// The card's words at its place (`aterm_phase::launch_card_words`), its
    /// three rows' joined by `\n`: what the next reading HOLDS the card by on
    /// rows the reader does not place it on ([`read_card`]).
    words: Box<str>,
}

impl CardRead {
    /// The card's words, row by row.
    fn words(&self) -> [&str; 3] {
        let mut rows = self.words.splitn(3, '\n');
        [(); 3].map(|()| rows.next().unwrap_or_default())
    }
}

/// The card a pane's readings had, handed to the next reading of its rows
/// ([`read_card`]).
#[derive(Clone, Debug)]
pub(crate) struct CardBefore {
    pub(crate) card: Arc<CardRead>,
    /// The card is the LAST reading's — the one on the glass — and the rows
    /// it is handed to are the next rebuild's after that reading (or the
    /// same rows again), so the reading of them may hold it. Else it is an
    /// earlier reading's (the last had no card, or a rebuild no reading saw
    /// changed rows since), which a reading only shares where it places the
    /// same card again.
    pub(crate) last: bool,
}

impl CardBefore {
    /// This card, handed past a reading that did not have it.
    pub(crate) fn earlier(self) -> Self {
        Self {
            last: false,
            ..self
        }
    }
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
    /// Which scan supplied `banner` (`3` Claude, `4` Agy, `1` Muse), including
    /// an incomplete card with no parsed `read`: its fourth and fifth rows can
    /// still finish later.
    rows_kind: usize,
    /// The card, `None` when none is placed or its build does not parse.
    pub(crate) read: Option<Arc<CardRead>>,
}

impl CardReading {
    /// Which scan supplied the reading: `3` Claude Code, `4` agy, `1` Muse Code.
    pub(crate) const fn rows_kind(&self) -> usize {
        self.rows_kind
    }

    /// Whether this reading — of a pane's rows before the rows `changed`
    /// names (ascending) changed — is the reading of `rows` as they are now,
    /// known without a scan (`aterm_phase::launch_card_unmoved`, behind the
    /// reader's panic fence): a keystroke, a spinner or a streamed row under
    /// the card, a pure revision bump and any row above the card keep it.
    /// [`read_card`] asks Claude Code's scan, then agy's, then Muse Code's,
    /// and takes the first that finds a banner, so the reading holds while
    /// the scan that supplied it is unmoved and each scan asked before it
    /// still finds none; a scan asked after it was never read, and is not
    /// asked here. Never for a reading the reader could not take.
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
            _ => aterm_phase::launch_card_unmoved(rows, banner, changed),
        })
        .unwrap_or(false)
    }
}

/// The launch card on `rows` and the build it names, behind the reader's
/// panic fence (`reader_guard`): no card when none is placed (narrow, cut,
/// redesigned — the card goes bare, never wrong) or its banner's version
/// does not parse, and nothing known when the reader panicked. A card equal
/// to `before`'s — the last reading's, where it had one — IS that card,
/// shared: a rebuild that moved the card nowhere allocates nothing for it.
///
/// The last rebuild's card (`before`, [`CardBefore::last`]) is HELD on rows
/// the reader places nothing on at or under it while its words stand at its
/// place (`aterm_phase::launch_card_held`): the vendor's fullscreen entrance
/// moves the mascot left of the words — a row low in its dust, both arms up
/// — on frames the reader cannot place, and the card stays on the glass
/// through them instead of blinking. The held reading's banner is the card's
/// own row, so [`CardReading::holds`] keeps it while none of its rows
/// changes and no newer banner draws under it, and reads again — holding or
/// letting go — when one does: the card goes on the frame its words leave
/// (scrolled, cleared, overwritten, the program's screen gone, the narrow
/// layout) or a newer card shows under it. A pane whose grid moved (a
/// resize, another pane region) starts from no reading, and holds nothing.
pub(crate) fn read_card(session: u64, rows: &[String], before: Option<CardBefore>) -> CardReading {
    #[cfg(test)]
    crate::work_counts::card_read();
    crate::reader_guard::read_or_none(session, "harness card", rows, || {
        let scan = aterm_phase::launch_card_scan(rows);
        if let Some(CardBefore { card, last: true }) = &before
            && card.kind == 3
            && card.place.rows == 3
            && aterm_phase::launch_card_held(rows, scan, &card.place, card.words())
        {
            return CardReading {
                banner: Some(Some(card.place.top)),
                rows_kind: 3,
                read: Some(Arc::clone(card)),
            };
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
            let words = match rows_kind {
                4 => aterm_phase::agy::launch_card_words(rows, &place)?,
                1 => aterm_phase::muse::launch_card_words(rows, &place)?,
                _ => aterm_phase::launch_card_words(rows, &place)?,
            };
            Some(match before {
                Some(CardBefore { card: was, .. })
                    if was.kind == rows_kind
                        && was.place == place
                        && *was.version == *version
                        && was.words() == words =>
                {
                    was
                }
                _ => Arc::new(CardRead {
                    place,
                    version: version.into(),
                    kind: rows_kind,
                    build: atpkg::vendor_direct::Version::parse(version),
                    words: words.join("\n").into(),
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
    /// How the line ends ([`LineEnd`]): Land's task's end, or Cruise's armed
    /// timer, filled for a Land or Cruise leaf with a card by
    /// [`App::compose_claude_cards`].
    end: LineEnd,
    /// The card to decorate — the pane cache's reading, shared — the
    /// drawer's token (its holder image's, `HolderImage::token`) and the
    /// number of the CURRENT rebuild of the pane's text, which names this
    /// frame's cells (the reading itself may be kept from an earlier
    /// rebuild, [`CardReading::holds`]): `Some` only where every gate holds.
    card: Option<(Arc<CardRead>, u64, u64)>,
    /// Why the card is bare this frame (`None` with a card).
    bare: Option<Bare>,
    /// The current drawer's token (`0` with no drawer).
    token: u64,
    /// The absolute row of the pane's top visible line (the dev rows').
    row_base: i64,
    /// The absolute row of the banner of the card the gate refused this
    /// frame (`None` where no card was read): the dev rows' `card-bare
    /// row=`, which tells a predecessor's card refused from the drawer's own.
    bare_row: Option<i64>,
    /// The leaf only carries a hide: its pane holds no drawer and no facts
    /// ([`drawerless_leaf`]); its Autopilot was not read.
    drawerless: bool,
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
/// the cell its words are drawn in and the ink's palette and polarity. The
/// cell's background is the pane's ground, which a light line's letters are
/// solved against (`HarnessCard::line_ink_into`), so a new ground inks the
/// line anew.
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
    /// The words the line ends with: a face's move, or a timer's, lays the
    /// line anew (its widest words that fit change, made here and nowhere
    /// else, [`line_texts`]), and arms no episode (§9.4.1, §10.6).
    end: LineEnd,
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
    /// The drawer the card was first decorated for: its holder image's token
    /// (`HolderImage::token`, card engine §3.9). Another is a new card, and a
    /// new episode: an `exec` in place, a relaunch; Ctrl-Z/`fg` and `/clear`
    /// keep it.
    identity: u64,
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
    /// [`Self::shown`] as the splice before the latest left it
    /// ([`WindowCards::hide_all`]): a card shown now and not then STARTS a
    /// show.
    was_shown: bool,
    /// When the card's latest show began ([`RESHOW_MS`]).
    shown_since: Instant,
    /// Since the latest show began, the card was hidden by its identity or
    /// its fence (`Bare::bounds_reshow`): its next show is bounded.
    gate_hid: bool,
    /// A show held back by the re-show bound, due at this instant
    /// (`DeadlineOwner::ClaudeCard` wakes for it: [`Self::due`]).
    deferred: Option<Instant>,
    /// The frame this card is due — an episode frame or the settled frame
    /// it owes — has been asked for, or its episode wants no frame at any
    /// later instant ([`App::claude_cards_due`]); no further wake is armed
    /// for it until a tick draws it or an arm starts another episode, so a
    /// frame that never comes spins nothing.
    asked: bool,
    /// The pane rows and columns the last frame laid the card on.
    place: (usize, usize, u16, u16),
    /// The cells the card's glass stands over on the last frame it was shown,
    /// in frame cells `(row, col, rows, cols)`: the box's rows with its air
    /// (the border stands half a row into each air row) and its columns up to
    /// and including the border's. The border, the trail, the kitty and the
    /// stars are glass, not cells, so the resident pet's world cannot see
    /// them; the footer's splice reserves this rect for it
    /// (`crate::pet_chrome`, [`WindowCards::glass_boxes`]).
    glass_box: (usize, usize, usize, usize),
    /// The card's frames: its settled frame, replayed while it rests, the
    /// ring its episode's frames are built in, and its cell height's arts.
    frames: CardFrames,
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
    /// drawer's token, so a re-arm keeps the same constellation.
    fn arm(&mut self, now: Instant) {
        let seed = pinned_seed().unwrap_or_else(|| mix(self.session ^ self.identity));
        self.card.arm(now, seed);
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
        if let Some(until) = self.deferred {
            return (!self.asked).then_some(until);
        }
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
    /// and not at rest (`HarnessCard::at_rest`); or a show the re-show bound
    /// holds back, not yet asked for.
    fn live(&self) -> bool {
        !self.asked && (self.deferred.is_some() || self.shown && !self.card.at_rest())
    }
}

/// What one splice appended to the frame, so the next frame (and a
/// capture's re-splice of the same scratch) takes exactly that back
/// ([`strip`]): the cards' fx layers, each as pushed, and the lines' ink and
/// stars, each stream's part in the stream's own order.
#[derive(Debug, Default)]
struct CardPainted {
    layers: Vec<FxLayer>,
    ink: Vec<InkCell>,
    decos: Vec<WordDecoration>,
}

impl CardPainted {
    fn is_empty(&self) -> bool {
        self.layers.is_empty() && self.ink.is_empty() && self.decos.is_empty()
    }

    fn clear(&mut self) {
        self.layers.clear();
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
    /// The arts queued for residency ahead of a card's first frame
    /// ([`App::prefetch_card_arts`]), held so the card that comes draws with
    /// the very arts the renderer was handed.
    arts: Option<Arc<CardArts>>,
    /// [`strip`]'s sorted keys, reused.
    keys: Vec<(EntryKey, bool)>,
    /// What [`layout`] reads for the card being laid anew, reused (swapped
    /// with the card's own, [`CardEntry::seen`]).
    seen: Seen,
    /// [`cut_out`]'s spares, one per stream type, reused: a cut is built in
    /// one and copied back ([`cut_each_once`]).
    spare_ink: Vec<InkCell>,
    spare_decos: Vec<WordDecoration>,
    /// The card passes this window composed while the dev card probe runs
    /// (`SessionFactory::card_probe`): the dev rows' `composed=`.
    composed: u64,
    /// The probe's outcomes this pass, one a leaf, reused: noted on each
    /// session's timeline after the pass (`SessionTimeline::note_card_frame`).
    outcomes: Vec<(u64, CardOutcome)>,
}

impl WindowCards {
    /// Read only the card's recorded layers in sibling fixture assertions.
    #[cfg(test)]
    pub(crate) fn painted_layers(&self) -> &[FxLayer] {
        &self.painted.layers
    }

    /// No card is on show: the splice returned before its card pass (the
    /// footer's `tab_status` gate), so none keys the repaint or arms a wake.
    pub(crate) fn hide_all(&mut self) {
        for entry in &mut self.entries {
            entry.was_shown = entry.shown;
            entry.shown = false;
            entry.line.on = false;
        }
    }

    /// Whether this window keeps a card for `session` (on show or not):
    /// the splice tells its card pass when that session's drawer left
    /// ([`drawerless_leaf`]).
    pub(crate) fn keeps(&self, session: u64) -> bool {
        self.entries.iter().any(|e| e.session == session)
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

    /// The frame cells each card on show stands over, its border and air
    /// included ([`CardEntry::glass_box`]): the resident pet's occupied
    /// ground, like the footer's written cells (`crate::pet_chrome`).
    /// Without it the cat took the card's blank corner under the mascot,
    /// inside the rainbow border, for clean ground.
    pub(crate) fn glass_boxes(&self) -> impl Iterator<Item = (usize, usize, usize, usize)> + '_ {
        self.entries.iter().filter(|e| e.shown).map(|e| e.glass_box)
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

/// Take back the layers, ink and star the last splice put on `scratch`:
/// each card's two layers by their ids, wherever they still are (a compose
/// that cleared the layers holds none of them, and a walk of the layers
/// finds that), and each recorded ink cell and star once, wherever it still
/// is, and nothing else. A stream a producer has refilled since (a
/// re-extract, a split's compose: the effects pass republishes every frame)
/// holds none of it, and that part costs a look at the stream and the
/// record's rows, no sort ([`remove_each_once`]); one a later band splice
/// scrubbed rows from (`scrub_overlay_row_band`) loses the rest. Called
/// where the footer's `undo` runs — the top of a frame, the top of a compose
/// and the top of the splice — so a re-splice of an unrefilled scratch never
/// doubles a card, and at a capture's one-pane refill, which keeps the
/// layers: a tab strip's prepend moves a grid-space layer (its offset and
/// clip) before the splice looks, and a moved layer is no longer the one
/// recorded. The record is consumed.
pub(crate) fn strip(cards: &mut WindowCards, scratch: &mut RenderInput) {
    if cards.painted.is_empty() {
        return;
    }
    let (painted, keys) = (&mut cards.painted, &mut cards.keys);
    withdraw_card_layers(&mut scratch.fx_layers, &painted.layers);
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

/// Cut the line's ink and star the last splice put on the glass out of the
/// frame cells `rect`: the tab menus (`App::splice_tab_menu`, the Autopilot
/// menu), topmost chrome painted after the card, hang from the first grid
/// row, where a fresh card sits, and no ink or star of the card may show on
/// them. The card's layers are cut there by the occluder the menu's own
/// scrub pushes (`app_render::scrub_overlay_card`), which the renderers
/// subtract from every layer. The record is cut the same way, so [`strip`]
/// still takes back exactly what is on the glass.
pub(crate) fn cut_out(
    cards: &mut WindowCards,
    scratch: &mut RenderInput,
    rect: crate::tab_menu::MenuRect,
) {
    if cards.painted.ink.is_empty() && cards.painted.decos.is_empty() {
        return;
    }
    let rows = rect.row..rect.row + rect.h;
    let cols = rect.col..rect.col + rect.w;
    let inside =
        |row: u16, col: u16| rows.contains(&usize::from(row)) && cols.contains(&usize::from(col));
    let WindowCards {
        painted,
        keys,
        spare_ink,
        spare_decos,
        ..
    } = cards;
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

/// Append precisely the card's nonempty layers and remember that contribution.
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

/// Take back one exact occurrence of each recorded layer, retaining other
/// producers even when they carry the same id. The common tail needs no scan.
fn withdraw_card_layers(stream: &mut Vec<FxLayer>, ours: &[FxLayer]) {
    if stream.ends_with(ours) {
        stream.truncate(stream.len() - ours.len());
        return;
    }
    for layer in ours.iter().rev() {
        if let Some(at) = stream.iter().rposition(|other| other == layer) {
            stream.remove(at);
        }
    }
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

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The constellation the `card` verb pinned ([`crate::card_verb`]):
    /// every card armed while it stands seeds its stars with it, in place of
    /// its session and process. The main thread's; a test's thread pins its
    /// own cards only.
    static SEED_PIN: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Pin the cards' constellation to `seed` (`None`: each card's own again),
/// for the cards armed from now on (`card seed=`, [`crate::card_verb`]).
#[cfg(any(test, debug_assertions))]
pub(crate) fn pin_card_seed(seed: Option<u64>) {
    SEED_PIN.with(|pin| pin.set(seed));
}

/// The pinned constellation ([`pin_card_seed`]); never one in a release
/// build.
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

/// HOW A CARD'S LINE ENDS (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §9.4.1, §10.2.3 item 2): with the Autopilot's own words (`· autopilot ⤓
/// land`), or, under Land, with how the work ended — `· autopilot ✓ landed`
/// (all of it is on the remote main branch), the words the chip's own face
/// uses. A person's own exchange keeps the mode's words: the card says the
/// mode, the chip says it is quiet. Cruise takes nothing as the end, so its
/// line always says the mode; an armed timer follows it (§10.6): `·
/// autopilot ∞ cruise ⤓18:00`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum LineEnd {
    /// The Autopilot's own words.
    #[default]
    Mode,
    /// `✓ landed`.
    Landed,
    /// `∞ cruise ⤓18:00` — `⤓Sat 18:00` on another day: Cruise until the
    /// armed timer's time (`ChipFace::CruiseUntil`), as the timer's index
    /// gives it (`ChipGlass::land_at`) and the title writes it
    /// (`autopilot_menu::when`).
    CruiseUntil(LandAtGlass),
}

impl LineEnd {
    /// The end a line under `view` takes for the task's face `task` (`None`:
    /// unknown): a landing, under Land alone (Cruise takes nothing as the
    /// end).
    #[must_use]
    pub(crate) const fn of(view: AutopilotView, task: Option<TaskFace>) -> Self {
        match (view, task) {
            (AutopilotView::Land, Some(TaskFace::Landed)) => LineEnd::Landed,
            _ => LineEnd::Mode,
        }
    }

    /// This end under `view`, with the session's armed timer `land_at`
    /// (`ChipGlass::land_at`; `None`: none armed): Cruise's own words gain
    /// the timer's time ([`LineEnd::CruiseUntil`]); every other end, and every
    /// other view, says what it says.
    #[must_use]
    pub(crate) const fn timed(self, view: AutopilotView, land_at: Option<LandAtGlass>) -> Self {
        match (self, view, land_at) {
            (LineEnd::Mode, AutopilotView::Cruise, Some(at)) => LineEnd::CruiseUntil(at),
            _ => self,
        }
    }

    /// The glyph and the word the line ends with under `view`: the chip's
    /// own (the face table, `ChipFace`), so the card and the chip say a
    /// landing's end alike (a timer's time follows them, [`make_line_texts`]).
    const fn words(self, view: AutopilotView) -> (&'static str, &'static str) {
        let face = match self {
            LineEnd::Mode | LineEnd::CruiseUntil(_) => return (view.glyph(), view.wire()),
            LineEnd::Landed => crate::claude_lights::ChipFace::Landed,
        };
        match face.word() {
            Some(word) => (face.glyph(), word),
            None => (view.glyph(), view.wire()),
        }
    }
}

/// The line's words for `view` and how it ends ([`make_line_texts`]), made
/// once per pair: the build is the process's, so they never change while it
/// runs, and a frame formats nothing — they are looked up only where the
/// card is laid out, when its key moves or the cells its layout read do.
fn line_texts(view: AutopilotView, end: LineEnd) -> &'static [String] {
    static TEXTS: [OnceLock<Vec<String>>; 6] = [const { OnceLock::new() }; 6];
    // A landing is told under Land alone: its words are the face's; an
    // armed timer's time follows Cruise's own words (§10.6).
    let end = match (view, end) {
        (AutopilotView::Cruise, LineEnd::CruiseUntil(at)) => return timed_line_texts(None, at),
        (AutopilotView::Land, _) => end,
        _ => LineEnd::Mode,
    };
    let slot = match (view, end) {
        (AutopilotView::Land, LineEnd::Landed) => &TEXTS[5],
        (AutopilotView::Cruise, _) => &TEXTS[0],
        (AutopilotView::Land, LineEnd::Mode | LineEnd::CruiseUntil(_)) => &TEXTS[1],
        (AutopilotView::Off, _) => &TEXTS[2],
        // An agent's own switch off reads the master switch's words here.
        (AutopilotView::MasterOff | AutopilotView::AgentOff(_), _) => &TEXTS[3],
        (AutopilotView::Unhosted, _) => &TEXTS[4],
    };
    slot.get_or_init(|| make_line_texts(view, end))
}

/// The line's words under Cruise with a timer armed for `at`
/// ([`LineEnd::CruiseUntil`]), made once per face a timer can wear — its
/// clock and its day, at most 1440 × 8 of them, in practice the few times a
/// person sets — and kept for the process like every other line's words.
/// The agent tag is part of the key too: Claude's untagged line, agy's and
/// muse's each keep their own words. A timer armed again for the same agent
/// and time takes the words already made.
fn timed_line_texts(agent: Option<&'static str>, at: LandAtGlass) -> &'static [String] {
    type TimedFace = (Option<&'static str>, LandAtGlass, &'static [String]);
    static MADE: Mutex<Vec<TimedFace>> = Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&(_, _, texts)) = made
        .iter()
        .find(|(who, face, _)| *who == agent && *face == at)
    {
        return texts;
    }
    let texts: &'static [String] =
        make_line_texts_for(agent, AutopilotView::Cruise, LineEnd::CruiseUntil(at)).leak();
    made.push((agent, at, texts));
    texts
}

/// The line's words for `view` ending `end`, widest first: with the
/// Autopilot and an armed timer's time (`⤓18:00`, the chip's own mark), with
/// the Autopilot, without it, then on the short version rung (a release has
/// none). The version is the running build's own signature
/// (`menu::version_signature`), so one build reads the same here as in the
/// menu bar.
fn make_line_texts(view: AutopilotView, end: LineEnd) -> Vec<String> {
    make_line_texts_for(None, view, end)
}

fn line_texts_for(rows_kind: usize, view: AutopilotView, end: LineEnd) -> &'static [String] {
    static AGY_TEXTS: [OnceLock<Vec<String>>; 6] = [const { OnceLock::new() }; 6];
    static MUSE_TEXTS: [OnceLock<Vec<String>>; 6] = [const { OnceLock::new() }; 6];
    let (agent, table) = match rows_kind {
        4 => ("agy", &AGY_TEXTS),
        1 => ("muse", &MUSE_TEXTS),
        _ => return line_texts(view, end),
    };
    let end = match (view, end) {
        (AutopilotView::Cruise, LineEnd::CruiseUntil(at)) => {
            return timed_line_texts(Some(agent), at);
        }
        (AutopilotView::Land, _) => end,
        _ => LineEnd::Mode,
    };
    let slot = match (view, end) {
        (AutopilotView::Land, LineEnd::Landed) => &table[5],
        (AutopilotView::Cruise, _) => &table[0],
        (AutopilotView::Land, LineEnd::Mode | LineEnd::CruiseUntil(_)) => &table[1],
        (AutopilotView::Off, _) => &table[2],
        (AutopilotView::MasterOff | AutopilotView::AgentOff(_), _) => &table[3],
        (AutopilotView::Unhosted, _) => &table[4],
    };
    slot.get_or_init(|| make_line_texts_for(Some(agent), view, end))
}

fn make_line_texts_for(agent: Option<&str>, view: AutopilotView, end: LineEnd) -> Vec<String> {
    use crate::claude_lights::ChipFace;
    use crate::menu::{VersionFacts, VersionRung, version_signature};
    let facts = VersionFacts::this_build();
    let mut texts = Vec::with_capacity(4);
    let tag = agent.map_or_else(String::new, |a| format!(" \u{b7} {a}"));
    if let Some(full) = version_signature(facts, VersionRung::Full) {
        let head = format!("enhanced by aterm harness {full}{tag}");
        let (glyph, word) = end.words(view);
        let mode = format!("{head} \u{b7} autopilot {glyph} {word}");
        if let LineEnd::CruiseUntil(at) = end {
            let mark = ChipFace::CruiseUntil
                .word()
                .unwrap_or(AutopilotView::Land.glyph());
            texts.push(format!("{mode} {mark}{}", crate::autopilot_menu::when(at)));
        }
        texts.push(mode);
        texts.push(head);
    }
    if let Some(short) = version_signature(facts, VersionRung::Short) {
        texts.push(format!("enhanced by aterm harness {short}{tag}"));
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
    matches!(place.rows, 1 | 3 | 4) && place.top + place.rows <= pane.rows && pane.cols >= 2
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
    /// The trail, `[start, end)`, beside the card's first rows.
    trail: Option<(usize, usize)>,
    /// The kitty's left column beside the card's first rows.
    kitty_col: Option<usize>,
    /// How many of the card's rows the kitty stands on
    /// (`CardScene::kitty_rows`, §10.6): three — the height of Claude's
    /// mascot — where the card's third row, its working directory's, is
    /// blank across the trail, the kitty and its margin; else two.
    kitty_rows: usize,
    /// The box's AIR, over it and under it (`CardScene::pad_above`,
    /// `pad_below`): the pane row over the banner row, and the one under the
    /// last boxed row, read blank across the box's columns `0..=cols` — the
    /// border stands half a row into each such row, clear of the card's ink.
    air: (bool, bool),
    line: Option<LinePlace<'t>>,
}

/// Lay the card at `place` out on the pane (`cells`), the kitty `kc` cells
/// wide (`harness_card::kitty_cols`; `0`: no kitty), the line one of `texts`
/// (widest first), noting every cell test it makes in `seen` (cleared first;
/// [`Seen`]). The module header has the rules; `None` when the card does not
/// sit whole in the pane. A cell is read once, and no further along a row
/// than the decision it serves needs: the line's run as far as its widest
/// words inside the border reach, the kitty's as far as decides where it
/// sits, the air's no further than the border's column.
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
    let logical_end = ends.iter().copied().max().unwrap_or(0);
    let mut card_end = logical_end;
    for k in 0..place.rows {
        // The compact place merges agy's third and fourth text extents in
        // ends[2]. A fourth row matters at that maximum too; only a trailing
        // continuation there can extend the card's overall text extent.
        let end = ends.get(k).copied().unwrap_or(logical_end);
        if seen.wide(cells, top + k, end) {
            card_end = card_end.max(end + 1);
            if let Some(end) = ends.get_mut(k) {
                *end += 1;
            }
        }
    }
    // Keep the existing clipping fallback for already oversized text. A new
    // wide extension needs a clear border column after its continuation;
    // without one, leave the vendor's card undecorated.
    if card_end > logical_end && card_end > max_border {
        return None;
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
        .chain([row0_end, card_end])
        .max()
        .unwrap_or(0);
    // The kitty and its trail, right of the first two rows' text: the trail
    // from two cells past it, then the kitty, then its margin of stars and
    // the border — every cell blank on both rows, all inside the pane.
    let mut kitty = None;
    if kc > 0 && rows >= 2 {
        let row1_end = if place.rows == 1 {
            line_end.unwrap_or(ends[0])
        } else {
            ends[1]
        };
        let start = row0_end.max(row1_end) + 2;
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
    // The tall kitty (§10.6): where the third row — the working directory's —
    // is blank under the trail, the kitty and its margin, read once from the
    // trail's start and no further than the margin's end, the kitty stands
    // the card's three rows tall, its body (and the trail beside it) reaching
    // into that row; a long path there keeps it two rows tall.
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
    let cols = kitty.map_or(text_end, |(_, _, cols)| cols).min(max_border);
    // The air: the border stands half a row clear of the box over a pane row
    // blank across the columns its edge and corners cross (`0..=cols`), each
    // read once from column 0 and no further than the border's column — a
    // glyph right of the box costs nothing. None over the pane's first row
    // (its padding and chrome are not the card's) nor under its last. The
    // row under a box of the card's own rows (no line under them; three for
    // Claude Code's card, four for agy's) is the one the line was tried on
    // wherever one of the line's texts fits between the text column and the
    // pane's last: read there, it held a glyph where the line would go, and
    // is not read again. Where none fits, no line test read it, and it is
    // read for air as the row over the box is.
    let span = cols + 1;
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

/// `v` as a `u16` (a cell index or a pixel size), saturating.
fn u16_of(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// The scene the emitter draws for a card laid out on `pane`, in WINDOW
/// pixels: grid cell `(0, 0)` of the pane at `(pad + col_off·cw, grid_top +
/// (strip + row_off)·ch)`, its rows tagged with their frame rows, the left
/// border in the window's padding for a pane at the window's left edge, the
/// top and bottom borders half a row into the pane rows over and under the
/// box where the layout read them blank ([`Layout::air`]: a row each, both
/// inside the clip), and the pane's rect as the clip.
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
        palette: if view == AutopilotView::Land {
            CardPalette::Land
        } else {
            CardPalette::Cruise
        },
        dark,
        ground,
        serious,
        coat,
        eye,
    }
}

/// Whether a pane's ground is dark: the card's light is added there, and
/// inked on a light one (`harness_card::dark_ground`, the line's own test).
fn dark_ground(blank: RenderCell) -> bool {
    aterm_effects::harness_card::dark_ground(pack(blank.bg))
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
        line_texts_for(read.place.rows, key.view, key.end),
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
    let sample_row = read.place.top + read.place.rows.saturating_sub(1).min(2);
    let gray = scratch
        .cells
        .get(pane.frame_row(sample_row))
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
        pack(ground.bg),
        animate,
        &mut line.ink,
    );
    line.settled = still;
    // The `✦` leads the line in its first letter's ink: added light at full
    // coverage on dark; on light opaque ink, as the letters are (text, held
    // to AA by `line_ink_into`), never a see-through tint of it.
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
        alpha: u8::MAX,
    };
}

/// Why a card on a pane is not decorated this frame (card engine §3.8; the
/// dev rows' `card-bare reason=`, §8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bare {
    /// Not wrapped: the card switch off, an Autopilot that carries no card,
    /// or no live claim of this host's (module header: GATES).
    Unsupervised,
    /// The current holder image is no card drawer with a token: a shell
    /// (Claude stopped or gone), another command, an untracked image.
    NoDrawer,
    /// No card placed on the pane's rows (none, cut, narrow, redesigned).
    NoCard,
    /// The frame's fence is another holder's (`RenderInput::holder`).
    Token,
    /// The card is another reader's than the drawer's.
    Kind,
    /// The card's rows are not all in one span the drawer laid, or the
    /// alternate screen is not wholly its.
    Fence,
    /// The drawer's mark is tainted: what lay under its cursor at its cut,
    /// or a row motion the grid could not carry.
    Tainted,
    /// The card's banner fails the build policy.
    Build,
    /// The re-show bound holds this show back ([`RESHOW_MS`]).
    Deferred,
}

impl Bare {
    /// The word the dev row prints (`card-bare reason=`).
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

    /// Whether this hide is an IDENTITY OR FENCE hide, which bounds the
    /// next show of the same card (§3.9: the drawer left, its token, kind,
    /// build or fence refused the card). A viewport hide (scroll-back, a tab
    /// switch, a menu: the pane is not gathered, or its card is only cut)
    /// and an unwrapped one are not.
    pub(crate) const fn bounds_reshow(self) -> bool {
        matches!(
            self,
            Self::NoDrawer | Self::Token | Self::Kind | Self::Fence | Self::Tainted | Self::Build
        )
    }
}

/// What one card pass did with one pane's card, for the dev rows
/// (`SessionTimeline::note_card_frame`, run only under the dev card probe).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardOutcome {
    /// Decorated for drawer `token`, its card's banner at absolute row `row`;
    /// `armed` names why an episode was armed this frame (`new`, `view`).
    Decorated {
        token: u64,
        row: i64,
        armed: Option<&'static str>,
    },
    /// Left bare, the current drawer's token (`0` with none), why, and the
    /// absolute row of the banner of the card left bare where one was read.
    Bare(u64, Bare, Option<i64>),
}

/// The frame's fence as one pane's frame source carries it, read under the
/// extraction lock with its cells (`RenderInput::holder`, `base_y`,
/// `engine_alt`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct FenceFrame {
    pub(crate) holder: aterm_core::terminal::HolderView,
    /// The absolute row of the pane's top visible line.
    pub(crate) base_y: i64,
    /// The frame shows the alternate screen.
    pub(crate) alt: bool,
}

impl FenceFrame {
    /// `scratch`'s fence: the pane's frame source.
    pub(crate) fn of(scratch: &RenderInput) -> Self {
        Self {
            holder: scratch.holder,
            base_y: scratch.base_y,
            alt: scratch.engine_alt,
        }
    }
}

/// THE GATE (card engine §3.8, I3): whether `read`, a card placed on a pane
/// at a live viewport, is the current drawer's own — checked cheapest
/// first: the frame's fence names the drawer's token; the card is the
/// drawer's reader's; its rows lie in one span the drawer laid (main), or
/// the alternate screen is wholly the drawer's (alt); and its banner passes
/// the build policy. Integer compares only.
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

/// The gate's first check: the frame's fence names the drawer's token (a
/// zero on either side is no drawer's).
fn token_holds(gate: &crate::holder_image::CardGate, fence: &FenceFrame) -> Result<(), Bare> {
    if gate.token == 0 || fence.holder.token != gate.token {
        return Err(Bare::Token);
    }
    Ok(())
}

/// THE FENCE (§3.8 step 4), one rule for every row the gate holds to the
/// drawer: on main, `rows` rows from pane row `top` lie in one span the
/// drawer laid, its mark untainted; on alt, the alternate screen is wholly
/// the drawer's.
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

/// The gate's verdict on one pane's card this frame: the card read off the
/// pane's cached text and the number of the CURRENT rebuild of that text
/// (`claude_footer::PaneTextCache::launch_card`), or why it stays bare.
pub(crate) type Verdict = Result<(Arc<CardRead>, u64), Bare>;

/// One pane's gate this frame (card engine §3.8): the current drawer's gate
/// as the splice read it under its one timeline lock
/// (`SessionTimeline::card_gate`), the frame's fence, and the verdict,
/// computed at most once ([`Self::verdict`]) and handed to both the card
/// ([`card_leaf`]) and the footer's kept card
/// (`SessionTimeline::note_claude_card`), so the footer keeps no
/// predecessor's card.
#[derive(Debug)]
pub(crate) struct GateCheck {
    gate: Option<crate::holder_image::CardGate>,
    fence: FenceFrame,
    verdict: Option<Verdict>,
    /// The top row on the pane of the card the verdict refused (the dev
    /// rows' [`Self::refused_row`]).
    refused_top: Option<usize>,
}

impl GateCheck {
    pub(crate) fn new(gate: Option<crate::holder_image::CardGate>, fence: FenceFrame) -> Self {
        Self {
            gate,
            fence,
            verdict: None,
            refused_top: None,
        }
    }

    /// The absolute row of the banner of the card the verdict refused, once
    /// it was made: the dev rows' `card-bare row=` (§8's judge).
    pub(crate) fn refused_row(&self) -> Option<i64> {
        self.refused_top.map(|top| {
            self.fence
                .base_y
                .saturating_add(i64::try_from(top).unwrap_or(i64::MAX))
        })
    }

    /// The drawer's token, or `0` with no drawer.
    pub(crate) fn token(&self) -> u64 {
        self.gate.map_or(0, |g| g.token)
    }

    /// The verdict on leaf `leaf_index`'s card, `session`'s: made the first
    /// time it is asked, from the reading the pane's text cache already
    /// holds or takes once per rebuild, and kept for the rest of the frame.
    /// No drawer reads no card.
    pub(crate) fn verdict(
        &mut self,
        texts: &crate::claude_footer::PaneTextCache,
        leaf_index: usize,
        session: u64,
    ) -> &Verdict {
        let (gate, fence, refused) = (self.gate, self.fence, &mut self.refused_top);
        self.verdict.get_or_insert_with(|| {
            let gate = gate.ok_or(Bare::NoDrawer)?;
            // Cheapest first: a fence of another holder's reads no card.
            token_holds(&gate, &fence)?;
            let (read, text_gen) = texts.launch_card(leaf_index, session).ok_or(Bare::NoCard)?;
            if let Err(why) = admit(&gate, &fence, &read) {
                *refused = Some(read.place.top);
                return Err(why);
            }
            Ok((read, text_gen))
        })
    }

    /// Whether the footer's launch card reading (`footer::launch_card_read`)
    /// is the drawer's own, so the footer's facts may keep it
    /// (`SessionTimeline::note_claude_card`): read by `program`'s reader,
    /// its card's banner at pane row `banner` is the card the verdict
    /// admitted ([`Self::verdict`]: that card, that reader), and the live
    /// status row that named its model (`status`, agy's and Muse Code's
    /// footers) lies where the drawer drew ([`fence_holds`], its token and
    /// its reader). A reading of neither row, or by no drawer's reader, is
    /// not. Integers, and the verdict this frame shares with the card.
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
        if let Some(row) = status {
            let Some(gate) = self.gate else {
                return false;
            };
            if gate.kind != kind
                || token_holds(&gate, &self.fence).is_err()
                || fence_holds(&self.fence, row, 1).is_err()
            {
                return false;
            }
        }
        banner.is_none_or(|top| {
            matches!(
                self.verdict(texts, leaf_index, session),
                Ok((read, _)) if read.place.top == top && read.kind == kind
            )
        })
    }
}

/// The footer's splice found a terminal pane at a live viewport whose
/// holder is a card drawer or whose footer facts are held: gather what the
/// card pass needs of it — the session's effective Autopilot `view` and
/// whether this host supervises it (`supervised`), both read ONCE this frame
/// by the caller ([`App::session_autopilot`]), and, where the card is
/// wrapped (`card_on` is the `[claude_footer] card` switch) and the gate
/// admits it ([`GateCheck::verdict`], card engine §3.8), the card read off
/// the pane's cached text (`texts`) and the drawer's token. The checks run
/// cheapest first: wrapped, then the gate's. A free function over the text
/// cache alone, with no `App`: it can reach no session, so it takes no lock
/// (the PTY reader holds the timeline's while Claude streams; the gate was
/// read under the splice's one lock) — a second one cannot be put back here
/// without the compiler saying so — and nothing is copied: the reading is
/// the cache's.
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
    let wrapped = card_on && view.carries() && supervised;
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
        end: LineEnd::Mode,
        card,
        bare,
        token,
        row_base: check.fence.base_y,
        bare_row: if wrapped { check.refused_row() } else { None },
        drawerless: false,
    }
}

/// A pane on the glass whose holder is no drawer and whose footer facts are
/// not held (Claude stopped by Ctrl-Z, or gone), where this window still
/// keeps a card for its session: no card is read and no Autopilot asked,
/// but the card pass learns its card left by IDENTITY, which bounds its
/// next show (§3.9, [`RESHOW_MS`]).
pub(crate) fn drawerless_leaf(leaf_index: usize, session: u64, pane: PaneGeom) -> CardLeaf {
    CardLeaf {
        leaf_index,
        session,
        pane,
        blank: RenderCell::default(),
        view: AutopilotView::Off,
        end: LineEnd::Mode,
        card: None,
        bare: Some(Bare::NoDrawer),
        token: 0,
        row_base: 0,
        bare_row: None,
        drawerless: true,
    }
}

impl CardLeaf {
    /// Whether this leaf read leaf `index`'s card (not a hide alone): its
    /// pane's text stays cached (`claude_footer::PaneTextCache::retain_active`).
    pub(crate) fn reads(&self, index: usize) -> bool {
        self.leaf_index == index && !self.drawerless
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
    /// card on window `wid`'s frame — its two fx layers pushed and recorded
    /// for [`strip`] — and make each its line, which the footer's
    /// write then takes ([`WindowCards::lines`]). Episodes are armed here (a
    /// new process's card, an Autopilot that moved), the cards of closed
    /// sessions are forgotten, and every card not decorated this frame stops
    /// keying the repaint. `leaves` is handed back to the window
    /// ([`WindowCards::take_leaves`]), empty.
    ///
    /// A card at rest costs two layers handed on, its kept contents by
    /// pointer (module header: WHAT A FRAME COSTS): the clock is read only
    /// to arm an episode or draw one of its frames, and
    /// the motion policy, the window's pixels and the kitty's look only
    /// where a card is decorated.
    pub(crate) fn compose_claude_cards(&mut self, wid: WindowId, mut leaves: Vec<CardLeaf>) {
        let Some(focused) = self.windows.get(&wid).map(|ws| ws.focused) else {
            return;
        };
        // The dev rows' probe (`holder_image::DEV_ROWS` builds run with
        // `ATERM_DEV_CARD_PROBE=1`): each leaf's outcome is noted on its
        // session's timeline after the pass, and nowhere else.
        let probe = crate::holder_image::DEV_ROWS && self.session_factory.card_probe;
        // What the card is drawn with, read before the window is borrowed —
        // and only where one is decorated.
        let look = leaves
            .iter()
            .any(|leaf| leaf.card.is_some())
            .then(|| self.card_look(wid, focused));
        // How each decorated Land or Cruise card's line ends: the task's
        // face, or the armed timer's time, read off the glass the frame
        // already reads (a map lookup or two a card, and none for a card
        // under neither: §10.2.3, item 2; §10.6).
        for leaf in leaves.iter_mut().filter(|leaf| {
            leaf.card.is_some() && matches!(leaf.view, AutopilotView::Land | AutopilotView::Cruise)
        }) {
            let glass = self.chip_glass(leaf.session);
            leaf.end = LineEnd::of(leaf.view, glass.task).timed(leaf.view, glass.land_at);
        }
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
        let mut outcomes = std::mem::take(&mut cards.outcomes);
        let WindowCards {
            entries,
            painted,
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
            let Some(((read, token, text_gen), look)) = leaf.card.as_ref().zip(look) else {
                // Not decorated this frame: follow its Autopilot only into a
                // mode that keeps the card bare (Off, the master switch), so a
                // move back into Cruise or Land is a change that arms. A
                // Cruise or Land frame drawn bare is no such move: Off
                // releases the session's claim, and the frame between the word
                // going back to Cruise and the new worker's claim (tens of ms)
                // is drawn bare — recording Cruise there would leave the
                // decorated frame after it no change to arm on. The same keeps
                // a card scrolled back, or refused by its gate, from arming
                // anything when it shows again under the mode it had. A hide
                // by identity or fence bounds the card's next show
                // ([`RESHOW_MS`]), and a show held back is dropped: there is
                // no card to show.
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
                    ));
                }
                continue;
            };
            let same = |e: &CardEntry| e.identity == *token;
            // THE RE-SHOW BOUND: the same card shown again after its identity
            // or its fence hid it waits out [`RESHOW_MS`] from its previous
            // show's start, its wake armed for that instant ([`CardEntry::due`]).
            // Read on a show's start only: a card on show reads no clock here.
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
                end: leaf.end,
                px: look.px,
                serious: look.serious,
                kitty: look.kitty,
            };
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
                if probe {
                    let row = leaf.row_base.saturating_add(read.place.top as i64);
                    outcomes.push((
                        leaf.session,
                        CardOutcome::Bare(*token, Bare::NoCard, Some(row)),
                    ));
                }
                continue;
            };
            let mut armed = None;
            let at = match at {
                Some(at) if same(&entries[at]) => {
                    let entry = &mut entries[at];
                    if !entry.was_shown {
                        // A show starts: the re-show bound's anchor.
                        entry.shown_since = clock.get();
                        entry.gate_hid = false;
                    }
                    if entry.view != leaf.view {
                        entry.arm(clock.get());
                        armed = Some("view");
                    }
                    at
                }
                // First decorated for this drawer: a new card, a new episode,
                // never held back.
                found => {
                    let now = clock.get();
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
                        laid: None,
                        laid_gen: 0,
                        seen: Seen::default(),
                        line: CardLine::default(),
                    };
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
            // A card at rest is replayed: its settled frame's own contents
            // put on the glass at the scene's origin, no clock read, nothing
            // built; an episode's frame is drawn on the clock.
            let (frame, ticked) = match entry.card.replay(&scene, look.animate, &entry.frames) {
                Some(frame) => (frame, None),
                None => {
                    let now = clock.get();
                    let frame = entry
                        .card
                        .draw(now, &scene, look.animate, &mut entry.frames);
                    (frame, Some(now))
                }
            };
            // The two layers: the border and the trail in window pixels under
            // the glyph ink, the kitty, its rim and the stars in the grid's
            // (every one lies right of the card's text, inside the pane),
            // both clipped to the pane.
            if !frame.is_empty() {
                let (cx0, cy0, cx1, cy1) = scene.clip;
                let grid = (pixel(look.px.pad), pixel(look.px.grid_top));
                append_card_frame(
                    painted,
                    scratch,
                    entry.session,
                    frame,
                    PxRect::new(cx0, cy0, cx1, cy1),
                    grid,
                );
            }
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
            // The air above is only what the pane has over the box.
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
                    ticked,
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
                ));
            }
        }
        cards.leaves = leaves;
        if probe {
            cards.composed += 1;
            // The dev probe's own lock, one a pane judged: a dev seam's
            // cost, counted apart from the splice's one lock a pane.
            for (session, outcome) in outcomes.drain(..) {
                if let Some(s) = pool.get(session) {
                    #[cfg(test)]
                    crate::work_counts::card_probe_locked();
                    s.ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .note_card_frame(outcome, cards.composed);
                }
            }
        }
        cards.outcomes = outcomes;
    }

    /// Queue the harness card's arts at window `wid`'s cell height for the
    /// GPU's residency ahead of the card's first frame (design §4.3, eager
    /// residency): when a card drawer's image is published for a session
    /// the window shows ([`Self::prefetch_card_arts_for`]), and on a cell
    /// metric change while one is in front (`App::sync_cell_pixel_size`).
    /// The first admitted ask builds the arts for that cell height; this
    /// publication or metric path can ask before drawing, while a draw that
    /// gets there first may build them itself (`CardArts::for_cell`). The
    /// window holds its admitted arts ([`WindowCards::arts`]) for the next
    /// card. Unsupported heights and a switched-off card queue nothing.
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

    /// [`Self::prefetch_card_arts`] for every window whose tab in front
    /// shows `session`, when the session's published holder image is a card
    /// drawer ([`Self::card_drawer_published`]). The status sweep calls it
    /// for each session whose PTY reader published an image since the last
    /// sweep (the edged inbox, card engine §3.5).
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

    /// Whether `session`'s last published holder image is a card drawer
    /// (Claude Code, agy, Muse Code) or atpkg's shim about to `exec` one
    /// (`holder_image::HolderImage::drawer`, `launcher_for`): one timeline
    /// lock, taken only on an edge or a metric change.
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
        let mut h = mix(u64::from(wrapped) | view.code() << 1 | u64::from(serious) << 8);
        // A show the re-show bound holds back keys the repaint once it is
        // due, so the frame its wake asks for goes through ([`RESHOW_MS`]):
        // the clock is read only while one is held back.
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
        // A show the re-show bound held back, due by now where no frame can
        // draw it (its tab behind another, its window off screen): let go,
        // like an episode run out — the splice that shows its window again
        // shows it.
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
            if !crate::messages_host::band_on_screen(ws) {
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

    /// The `card` verb ([`crate::card_verb`]): the constellation pin read,
    /// or set (a debug or test build) and every card on show re-armed so
    /// each draws its next frame from it; the line says how many.
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
