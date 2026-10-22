// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS CARD on the glass (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §3): while the aterm harness carries a Claude Code session — its Autopilot
//! Cruise or Land, and the window's host holding the session's live
//! supervisor claim — Claude Code's launch card wears an animated rainbow
//! border, a pixel-art aterm kitty riding a rainbow trail with stars, and the
//! line `✦ enhanced by aterm harness <version> · autopilot ∞ cruise` (an armed
//! timer's time after it, `∞ cruise ⤓18:00`; Land: `· autopilot ⤓ land`, in a
//! sunset; a landing's end, `· autopilot ✓ landed` — [`LineEnd`]). Under Off
//! the card wears all of it in white — the moonbow, white → pearl → silver →
//! steel (`CardPalette::Off`), and the line `✦ enhanced by aterm harness
//! <version> · autopilot ○ off` — with no claim, since Off gives its claim
//! back ([`AutopilotView::wears_glass`]); the master switch off, an agent's
//! own switch off and a window with no host show the vendor's card bare.
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
//!   effective Autopilot Cruise or Land AND its live claim this host's, or
//!   its Autopilot Off, which needs none ([`AutopilotView::wears_glass`],
//!   read through [`App::session_autopilot`]; the hosted loop asks for the
//!   claim as its first request, `aterm_agent`'s `claim_at_naming`); and THE GATE
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
//! * THE LAYOUT ([`layout`]). The box holds the card's rows — three, or the
//!   four of the NARROW card Claude Code draws in a narrow pane with the key
//!   on or a non-default effort (MEASURED at 50 columns, and down to 33 with
//!   the wand, 31 without: the banner alone on its row, the mascot beside
//!   the model, the billing and the working directory) — and the line's row
//!   under them when that row is blank
//!   across the line's span (the line goes there at the card's text column);
//!   otherwise the line goes on the banner row two cells past the banner's
//!   end, if those cells are blank, and the box keeps its rows; otherwise
//!   there is no line. A narrow pane drops ` · autopilot …` first, then
//!   steps the version down (`menu::VersionRung::Short`; a release has
//!   none), then the version itself: the SHORT RUNGS keep the hero word and
//!   the chip's glyph and word (`aterm harness · ∞ cruise`, `aterm · ∞
//!   cruise`, `aterm ∞`), so the bold `aterm` shows down to the narrowest
//!   card the reader places (31 columns); an armed timer's ` ⤓18:00` goes
//!   before ` · autopilot …` does. The kitty
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
//! * AN INTRO, THEN THE FLOW (owner, 2026-10-19: "Make the rainbow at the
//!   banner continue to shimmer and flow continuously, which is also aligned
//!   with how the claude alien shimmers"). An intro (`harness_card::EPISODE_MS`)
//!   is armed when a card is first decorated for a drawer (its snapshot
//!   token, which also seeds its stars), when the session's Autopilot
//!   changes (Cruise, Land or
//!   Off), and when the window regains focus after [`RETURN_MS`] or more
//!   away ([`App::claude_glass_focus`]: a shorter absence is a blip the
//!   window server deals as another process starts, not a person back);
//!   the kitty rides in on the first only, and once the card has been on the
//!   glass at rest it plays every later one in its seat (`harness_card`'s
//!   `Ride`). A card is the session's, not the leaf's: a pane that moves in
//!   the plan (zoomed, a sibling split or closed to its left) keeps its card
//!   and its episode.
//!   While the intro plays, each frame is due a `FRAME_MS` after the last one
//!   drawn — an instant no other wake can push on — and is judged on every
//!   wake (`App::claude_cards_due`, beside the band's motion). After it the
//!   card FLOWS on: one frame a step of Claude's ripple
//!   (`aterm_effects::ripple::FLOW_STEP_MS`, 128 ms), due at the step's
//!   start plus `harness_card::FLOW_SLACK_MS` — a fixed instant — the hue
//!   travelling round the box and the glints running out from the mascot's
//!   wand, PHASE-LOCKED to Claude's own AS SEEN where its `✦` flares
//!   (`aterm_phase::wand_flare`, `HarnessCard::lock_glint`: a ring's birth,
//!   a step's start, put `harness_card::FLARE_STEP_LEAD_MS` before the
//!   frame that showed the flare; taken whole while no glint shows, else
//!   slewed a quarter of the error and at most a step a flare, so Claude's
//!   repaint jitter is never chased), and the steps to each other repaint
//!   of its shimmer by the same rules ([`mascot_ink`],
//!   `HarnessCard::lock_step`), so Claude's repaints, about one a step,
//!   land inside the step's slack and their presents draw the card's
//!   steps. Free-running is the normal case: only Claude's fullscreen
//!   renderer with the key on shimmers its mascot and flares its wand (the
//!   inline renderer and the plain mascot are static, MEASURED), and a lock
//!   lost — Claude blurred, its card remounted, the key off — leaves the grid
//!   running where the last lock put it, Claude's clock being its process's
//!   own and steady across all three. It flows while the window is focused
//!   (or lost focus less than [`RETURN_MS`] ago: a blip leaves it flowing),
//!   motion is allowed (Reduce Motion, `motion = "reduced"` and the load-shed
//!   latch settle it), the window is on screen and serious mode is off; a
//!   gate closing settles it on its next frame and a gate reopening resumes
//!   it from the settled colours, the repaint key's term moving with the
//!   motion verdict so the resume comes on the next wake
//!   ([`App::claude_card_term`]). A settled card is static: the term is
//!   constant and no wake is armed ([`App::claude_card_deadline`],
//!   `metrics::DeadlineOwner::ClaudeCard`); nor is one while the window's tab
//!   in front does not show the card's session (Settings, where no splice
//!   draws or hides the card). Reduced motion and an unfocused window get
//!   the settled frame from the start; serious mode drops the kitty, trail
//!   and stars.
//! * ATERM STANDS OUT (owner, 2026-10-19: "emphasize aterm in the banner").
//!   The line keeps its words; its [`HERO_WORD`] is set bold and wears its
//!   own whole run of the palette, on dark a step brighter than the line and
//!   flaring harder as a sweep or a glint passes
//!   (`harness_card::LineAt::hero`, [`hero_cells`]).
//! * THE HARNESS BANNER over Claude Code's update notice (owner, 2026-10-19:
//!   "just remove ' Updated to latest. Got 4 features, 43 bugfixes, and 16
//!   other changes.' and replace it alab.systems URL and we will customize
//!   that banner"; [`NOTICE`] is its one place). Under a decorated card —
//!   every gate the card takes, Off's white card included — the notice Claude
//!   draws on a new build's first launch (`aterm_phase::update_notice_scan`,
//!   read by its shape and kept, as the card's reading is, while no row it
//!   read changed) is hidden on the glass: its summary's first row shows
//!   `✦ alab.systems`, inked by the line's own ink ([`ink_words`]: the
//!   palette, the flow and the glints), and the summary's wrap and the
//!   details row with its code.claude.com link are blanked to the notice's
//!   end ([`make_notice`]). Written in the line's undoable write, so it is
//!   glass only. A link-modifier press on the banner's words opens
//!   [`NOTICE`]'s link (behind `is_safe_url`), a press on the hidden rows
//!   opens nothing, and under mouse tracking a plain press on any of it in
//!   the focused pane is swallowed, so Claude never opens the link it no
//!   longer shows (in an unfocused pane it is the focus click, which
//!   reports nothing) ([`App::claude_notice_at`]; the grid's link readers
//!   see the banner).
//!   A narrow pane's wraps are the notice's too: at 44 columns the summary's
//!   second row and both rows of the wrapped details row are blanked, and
//!   under the narrow card down to 31 columns the summary's three rows and
//!   the details link Claude breaks hard at the pane's edge. When the
//!   conversation scrolls the card's top off a fullscreen pane, the card
//!   decorated whole on an earlier frame is known again by its rows left at
//!   the screen's top (`aterm_phase::launch_card_scrolled`): the box leaves
//!   the glass but the notice under those rows keeps the banner, settled;
//!   with no row of the card left the notice shows bare, so a reply quoting
//!   it is never hidden. While a find bar is up, and wherever the banner
//!   would not fit or the vendor's notice is not whole (a reworded row, a
//!   row broken inside a word), the vendor's notice shows bare; so does the
//!   notice of a card whose banner wraps (32 columns and fewer with the
//!   wand, 30 and fewer without: no card is placed) and of a scrolled card
//!   this window never saw whole.
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
//!   extremely smooth and zero latency!"). A held card is REPLAYED: its
//!   settled frame is kept beside it (`harness_card::CardFrames`) and put
//!   on the glass from there — the same two content `Arc`s, which the
//!   renderer compares by pointer and builds nothing for — while the card
//!   stays still on a scene of the same shape (an inline scroll under it is
//!   the same frame at a new origin), never drawn again, with no clock read;
//!   a FLOWING card is drawn once a step and replayed between steps
//!   (`HarnessCard::replay_step`: one clock read, to find the step, and the
//!   step's same two `Arc`s), so a keystroke's present or streamed output
//!   between two steps draws nothing. An intro's frames refill a ring of
//!   contents in place when their prior contents are not retained
//!   elsewhere. Arts are built at the first admitted ask for a cell height,
//!   which may be a draw; publication and metric changes can queue them
//!   ahead ([`App::prefetch_card_arts`]). Its reading is kept while no row
//!   it reads changes — the card's three (the narrow card's four and the
//!   blank row under them), and below them only a row that now draws a
//!   banner ([`CardReading::holds`]); its layout while the cells the layout
//!   read say what they said — known without a look while the pane's text is
//!   the same rebuild, else by testing again, once per rebuild, exactly the
//!   cells the layout read and no others ([`Seen`]: the layout reads each
//!   cell once, and no further along a row than its decision needs — the
//!   rows over and under the box no further than the border's column — so a
//!   wide pane costs it no more than a narrow one; laid anew while a find
//!   bar is up, whose tint writes cells no revision moves) — and its line —
//!   cells, ink and star — while what it shows holds (for a flowing card,
//!   within a step). A card laid anew (it moved, the pane resized) notes what
//!   its layout read as it reads it, and reads no cell again. So a keystroke
//!   in the composer, a spinner or streamed output under the card, a chrome
//!   row's refill and a split's compose, each a rebuild of the pane's text,
//!   read no card and lay none out. Nothing is allocated for it once its
//!   buffers are grown, the window's repaint key and wake judges read no
//!   clock for a held card, the splice takes no lock for it (the gate
//!   comes from the footer's own timeline lock, one a pane a frame, facts or
//!   none), and a window with no card on
//!   its glass reads no clock, no motion policy and no geometry at all. The
//!   clock is read where an intro is live or the card flows — its frames,
//!   the arm that starts it, the step between two — and nowhere else,
//!   through [`card_now`] (`work_counts`, asserted by `claude_card_tests`'
//!   steady-state tests); a card hidden while it moves stops asking once it
//!   wants nothing more ([`App::claude_cards_due`]).

use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_core::render::{
    DecoBlend, DecoGlyph, FxLayer, GlowQuad, InkCell, PxRect, RainHalo, RenderInput, WordDecoration,
};
use aterm_core::terminal::RenderCell;
use aterm_effects::harness_card::{
    CardArts, CardFrames, CardPalette, CardScene, FRAME_MS, HarnessCard, LineAt, kitty_cols,
};
use aterm_phase::{CardPlace, NoticePlace};

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
/// THE HERO WORD of the card's line (owner, 2026-10-19: "emphasize aterm in
/// the banner"): every rung of the line names it ([`make_line_texts_for`]),
/// and its cells are set bold and wear their own run of the palette
/// ([`hero_cells`], `harness_card::LineAt::hero`).
pub(crate) const HERO_WORD: &str = "aterm";
/// Tags the flow's step in the repaint key's card term
/// ([`App::claude_card_term`]), apart from the intro's frame index.
const FLOW_TAG: u64 = 0xF10F_0000_0000_0000;
/// Tags the motion verdict in the card term ([`App::claude_card_term`]).
const MOTION_TAG: u64 = 0xA417_0000_0000_0000;

/// A `✦` that draws nothing: a line's or a banner's before it is made.
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

/// What the harness banner over Claude Code's update notice says, and where
/// a press on it goes ([`NOTICE`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HarnessNotice {
    /// The banner's words, drawn after a `✦` where the vendor's summary was.
    pub(crate) text: &'static str,
    /// What a link-modifier press on them opens (behind `is_safe_url`).
    pub(crate) url: &'static str,
}

/// THE HARNESS BANNER over Claude Code's update notice (owner, 2026-10-19:
/// "add the alab.systems URL and replace 'code.claude.com/docs/en/changelog'
/// with my URL", then "in fact, just remove ' Updated to latest. Got 4
/// features, 43 bugfixes, and 16 other changes.' and replace it alab.systems
/// URL and we will customize that banner", and "alab is always upgrading
/// the version of claude code"): the ONE place its words and its link live.
/// Customizing the banner is editing this const; a `[claude_footer]` key
/// can come when the owner asks for one.
pub(crate) const NOTICE: HarnessNotice = HarnessNotice {
    text: aterm_types::identity::SITE,
    url: aterm_types::identity::SITE_URL,
};

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

/// THE RETURN BOUND (ms): a focus regain is a person BACK at the window —
/// every card on show plays its intro again (§3), every stuck dot on show
/// breathes its course again — only after the window was away at least
/// this long ([`App::claude_glass_focus`]). A person who looks away and
/// back is away longer (a ⌘-Tab there and back is slower than this); the
/// focus the window server takes from the window for a moment as another
/// process starts — a freshly built test binary's first run, a lab's
/// instance — comes back far sooner: 18 to 65 ms, MEASURED on a real
/// window (2026-10-21: eight blips in 12 minutes on a desk where agents
/// build and test, each as a new `aterm-gui` test binary first ran). An
/// intro replayed on such a blip is 6 s of frames a [`FRAME_MS`] apart
/// that Claude's repaints cannot carry: about 140 of the card's own wakes,
/// where the flow it interrupted paid none. Until this long has passed a
/// loss is not known to be a person leaving, so a card on show moves on as
/// a focused window's does ([`moves_focused`]): a blip shorter than this
/// leaves its flow running, its colours and glints where they were, and a
/// window that stays away settles on its first frame past the bound. Both
/// edges are timed as the loop handles them (a focus event carries no
/// instant of its own), so a main-thread turn this long between them can
/// judge one wrongly: an intro played, or one not played, and no more.
pub(crate) const RETURN_MS: u64 = 250;

/// Whether a window's cards move as a focused window's do (the focus
/// [`App::card_animates`] is asked with): `focused`, or away less than
/// [`RETURN_MS`] — `away` the note [`App::claude_glass_focus`] took on the
/// loss. Reads the clock only while such a note stands, and the splice lets
/// a note that ran out go ([`App::compose_claude_cards`]): a gain with no
/// note is a return, as one past the bound is.
fn moves_focused(focused: bool, away: Option<Instant>) -> bool {
    focused
        || away.is_some_and(|since| {
            card_now().saturating_duration_since(since) < Duration::from_millis(RETURN_MS)
        })
}
/// What the card reads off a pane's rows: where the newest launch card sits
/// ([`CardPlace`]) and the build its banner names. Read at most once per
/// rebuild of the pane's text (`claude_footer::PaneTextCache::launch_card`),
/// only when a card is wanted there, and not at all where the rebuild
/// changed no row the last reading read ([`CardReading::holds`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardRead {
    pub(crate) place: CardPlace,
    pub(crate) version: Box<str>,
    /// `version` as a build, parsed once when the card is read: the gate's
    /// build policy compares integers per frame (card engine §3.8). `None`
    /// where it is no version `atpkg` spells.
    pub(crate) build: Option<atpkg::vendor_direct::Version>,
    /// The card's words where it was read whole ([`Self::whole`];
    /// `aterm_phase::launch_card_words`), its rows' joined by `\n`: what the
    /// next reading HOLDS the card by on rows the reader does not place it
    /// on, or knows it by once its top has scrolled off ([`read_card`]).
    words: Box<str>,
    /// The place the card was read WHOLE at, whose rows [`Self::words`]
    /// holds: `place` itself but on a scrolled card ([`Self::cut`]). A
    /// scrolled card is known again by the whole card's last rows, so the
    /// conversation scrolling it back down a row (a menu under the composer
    /// closing, the composer shrinking) keeps it as surely as scrolling it
    /// on, not only the rows the last frame showed.
    whole: CardPlace,
    /// Which scan read it ([`CardReading::rows_kind`]): `3` Claude Code
    /// (its three-row card and its narrow four-row one alike), `4` agy, `1`
    /// Muse Code.
    pub(crate) kind: usize,
    /// The card SCROLLED: its top cut by the screen's top edge, `place` the
    /// rows of it left there (`aterm_phase::launch_card_scrolled`). Such a
    /// card wears no glass of its own — what is left of it is not the card
    /// the box was laid around — but the update notice under it is anchored
    /// on it: the banner stays over the notice until the card's last row
    /// leaves ([`card_leaf`]).
    pub(crate) cut: bool,
}

impl CardRead {
    /// The card's words, row by row (the first `.1` of `.0`).
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
    /// asked here. A SCROLLED card (`CardRead::cut`) holds while its rows
    /// left and the row under them stand and no banner draws
    /// (`aterm_phase::launch_card_scrolled_unmoved`). Never for a reading
    /// the reader could not take.
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
                // A scrolled card: while its rows left and the blank row
                // under them stand, and no banner draws anywhere.
                Some(read) if read.cut => {
                    aterm_phase::launch_card_scrolled_unmoved(rows, &read.place, changed)
                }
                _ => aterm_phase::launch_card_unmoved(rows, banner, changed),
            },
        })
        .unwrap_or(false)
    }
}

/// The launch card on `rows` and the build it names, behind the reader's
/// panic fence (`reader_guard`): no card when none is placed (cut by the
/// screen's bottom, its banner wrapped, redesigned — the card goes bare,
/// never wrong) or its banner's version does not parse, and nothing known
/// when the reader panicked. A card equal to `before`'s — the last
/// reading's, where it had one — IS that card, shared: a rebuild that moved
/// the card nowhere allocates nothing for it.
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
/// (scrolled, cleared, overwritten, the program's screen gone, the other
/// layout) or a newer card shows under it. Where the conversation SCROLLED
/// it up past the screen's top, its last rows still there and no banner on
/// the screen, the reading is the card SCROLLED ([`CardRead::cut`]: by
/// `aterm_phase::launch_card_scrolled` against the whole card's place and
/// words, so a scroll back down is known as a scroll on is; its build the
/// one it was placed with), handed on rebuild after rebuild until its last
/// row leaves. A pane whose grid moved (a resize, another pane
/// region) starts from no reading, and holds nothing.
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
            // Its top scrolled off: known by the whole card's last rows at
            // the screen's top — scrolled on or back — the same card, its
            // build the one it was placed with.
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
    /// Cruise, Land or Off arms an episode.
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
    /// Claude Code's update notice under the card (`aterm_phase::NoticePlace`,
    /// the pane cache's scan): `Some` only under a decorated Claude Code
    /// card — so the banner takes every gate the card takes.
    notice: Option<NoticePlace>,
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
    /// The flow step the ink was made for (`None`: not a flow frame's): it
    /// stands while the card is replayed within that step.
    step: Option<u64>,
    frame_row: usize,
    /// The frame column of the line's first word: the write's key.
    col: usize,
    /// The hero word's cells, offsets into [`Self::cells`] ([`hero_cells`]).
    hero: Range<u16>,
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
            col: 0,
            hero: 0..0,
            cells: Vec::new(),
            ground: RenderCell::default(),
            ink: Vec::new(),
            star: NO_STAR,
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

/// One row of a banner's glass ([`NoticeGlass`]): its cells, written at
/// frame column `col` of frame row `frame_row` — window grid row `term_row`,
/// what the chrome's register of claimed rows is asked by.
#[derive(Clone, Debug, Default)]
struct NoticeRow {
    frame_row: usize,
    term_row: usize,
    col: usize,
    cells: Vec<RenderCell>,
}

/// What a banner's cells were made for ([`NoticeGlass::key`]): the notice's
/// place, the pane, the cell its words are drawn in, the pane's blank and
/// the ink's polarity and palette.
#[derive(Clone, Copy, Debug, PartialEq)]
struct NoticeKey {
    place: NoticePlace,
    pane: PaneGeom,
    base: RenderCell,
    ground: RenderCell,
    dark: bool,
    palette: CardPalette,
}

/// THE BANNER over one card's update notice ([`NOTICE`]), kept on its card
/// ([`CardEntry::notice`]) and rebuilt only when what it shows moves, as the
/// line is: the summary's first row becomes `✦ alab.systems` at the notice's
/// column, every other row of the notice — the summary's wrap and the
/// details row with its code.claude.com link — is blanked, each row to the
/// notice's end; written by the footer's splice in the same undoable write
/// as the line (`claude_footer::write_edits`, borrowed), its ink and its
/// star put on the glass, and its hits recorded, only where the write took
/// ([`ink_lines`]).
#[derive(Clone, Debug)]
struct NoticeGlass {
    /// Drawn this frame.
    on: bool,
    key: Option<NoticeKey>,
    /// The ink is the settled gradient ([`CardLine::settled`]).
    settled: bool,
    /// The flow step the ink was made for ([`CardLine::step`]).
    step: Option<u64>,
    /// The rows, the banner's first; the first [`Self::live`] are this
    /// banner's (the rest are spare buffers).
    rows: Vec<NoticeRow>,
    live: usize,
    ground: RenderCell,
    /// The frame columns of the banner's `✦` through its words' end: what a
    /// press opens [`NOTICE`]'s link on.
    link: Range<usize>,
    ink: Vec<InkCell>,
    star: WordDecoration,
}

impl Default for NoticeGlass {
    fn default() -> Self {
        Self {
            on: false,
            key: None,
            settled: false,
            step: None,
            rows: Vec::new(),
            live: 0,
            ground: RenderCell::default(),
            link: 0..0,
            ink: Vec::new(),
            star: NO_STAR,
        }
    }
}

impl NoticeGlass {
    /// This banner's rows.
    fn rows(&self) -> &[NoticeRow] {
        &self.rows[..self.live]
    }
}

/// Where a press or the pointer finds a banner on the glass
/// ([`App::claude_notice_at`]): the cells of one written row, and the link
/// they open — [`NOTICE`]'s on the banner's `✦` and words, none on every
/// other cell it wrote (the hidden summary's and details' rows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NoticeHit {
    session: u64,
    frame_row: usize,
    term_row: usize,
    cols: Range<usize>,
    url: Option<&'static str>,
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
    /// The card's frames: its settled or flow frame, replayed while it
    /// rests or between two of the flow's steps, the ring its intro's frames
    /// are built in, and its cell height's arts.
    frames: CardFrames,
    /// Claude's wand was flaring on the last frame that looked
    /// ([`aterm_phase::wand_flare`]): a flare's rising edge locks the
    /// ripple (`HarnessCard::lock_glint`).
    flare: bool,
    /// The wand's cell and Claude's mascot's ink around it on the last
    /// flowing frame that looked ([`mascot_ink`]); `None` after a frame
    /// that did not (a held card's replay). A change under the same wand is
    /// Claude repainting its shimmer, which locks the flow's steps
    /// (`HarnessCard::lock_step`).
    shimmer: Option<((usize, usize), u64)>,
    /// The last layout and scene, and what they were made from: kept while
    /// the key holds and the cells the layout read do (`None` inside: the
    /// card did not fit).
    laid: Option<(LaidKey, Option<(Layout<'static>, CardScene)>)>,
    /// The card was drawn whole for this process since it was last found
    /// not to fit: a scrolled card's notice keeps the banner on its word
    /// ([`CardRead::cut`]) — under whatever Autopilot the pane shows now,
    /// and after a find bar, which keeps no layout (`laid`).
    whole: bool,
    /// The rebuild of the pane's text (`claude_footer::PaneTextCache`) the
    /// kept layout's cells were last read in: the same rebuild, the same
    /// cells, and nothing is read to keep it.
    laid_gen: u64,
    /// The cells the kept layout read and what it found there ([`Seen`]):
    /// tested again once per rebuild of the pane's text.
    seen: Seen,
    /// The line under the card.
    line: CardLine,
    /// The banner over Claude Code's update notice under the card.
    notice: NoticeGlass,
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
    /// intro plays, a [`FRAME_MS`] after the last frame drawn — ANCHORED
    /// there, never later (`HarnessCard::deadline` is a frame ahead of
    /// whatever instant it is asked at, so a stream of other wakes, each
    /// asking anew, would push the frame on for as long as the stream lasts);
    /// while it flows, the next step's own instant, which no wake moves; the
    /// frame owed once the intro runs out, `now`. `None` when settled, not on
    /// show, or once asked until a tick draws it.
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

    /// Whether this card on show may want a frame — known without a clock:
    /// shown, not yet asked (nor found wanting nothing more, `asked` too),
    /// and not at rest (`HarnessCard::at_rest`); or a show the re-show bound
    /// holds back, not yet asked for.
    fn live(&self) -> bool {
        !self.asked && (self.deferred.is_some() || self.shown && !self.card.at_rest())
    }
}

/// What one splice appended to the frame — the cards' fx layers, each as
/// pushed, the agents panels' (`crate::claude_panel`) light, and the lines'
/// and panels' ink and stars — so the next frame (and a capture's re-splice
/// of the same scratch) takes exactly that back ([`strip`]). Each stream's
/// part is kept in the stream's own order.
#[derive(Debug, Default)]
pub(crate) struct CardPainted {
    layers: Vec<FxLayer>,
    /// The panels' slabs (`RenderInput::glow_under`).
    under: Vec<GlowQuad>,
    /// The panels' dot halos (`RenderInput::glow_halo`).
    halo: Vec<RainHalo>,
    ink: Vec<InkCell>,
    decos: Vec<WordDecoration>,
}

impl CardPainted {
    fn is_empty(&self) -> bool {
        self.layers.is_empty()
            && self.under.is_empty()
            && self.halo.is_empty()
            && self.ink.is_empty()
            && self.decos.is_empty()
    }

    fn clear(&mut self) {
        self.layers.clear();
        self.under.clear();
        self.halo.clear();
        self.ink.clear();
        self.decos.clear();
    }
}

/// Put one agents panel's look on the glass, recorded for [`strip`]: its
/// dot halos after the halo stream's last, its slabs after the under-glyph
/// stream's last, and its ink only on cells no other producer inked (theirs
/// keep their cells; the stream stays sorted by `(row, col)` and unique, as
/// [`ink_lines`] keeps it). `ink` is sorted and unique.
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
    spare_quads: Vec<GlowQuad>,
    spare_halos: Vec<RainHalo>,
    spare_ink: Vec<InkCell>,
    spare_decos: Vec<WordDecoration>,
    /// The agents panels on this window's glass (`crate::claude_panel`),
    /// their light and ink recorded in [`Self::painted`] beside the cards'.
    panels: crate::claude_panel::WindowPanels,
    /// Where the banners the last write took sit on the glass
    /// ([`App::claude_notice_at`]), recorded by [`ink_lines`].
    notice_hits: Vec<NoticeHit>,
    /// When the window last lost focus with a card or a breathing panel on
    /// show ([`App::claude_glass_focus`]); `None` while focused, or when it
    /// went away with nothing on show to play again.
    away: Option<Instant>,
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

    /// Drop the layers of id `id` from the record alone, leaving them on the
    /// frame: a record kept elsewhere, as a negative control's.
    #[cfg(test)]
    pub(crate) fn forget_layers(&mut self, id: u64) {
        self.painted.layers.retain(|l| l.id != id);
    }

    /// Put another painter's fx layer on the glass in the cards' own record
    /// (a top rule's mark, `claude_footer`'s `mark_layer`): appended to the
    /// frame's layers and to [`CardPainted`] together, so every [`strip`]
    /// takes it back by the same identity law as a card's
    /// ([`withdraw_card_layers`]: its id, slot and space and the very
    /// content, wherever it now lies) — the capture's one-pane refill
    /// included, which runs no footer undo. A layer that clips to nothing or
    /// draws nothing is not put on.
    pub(crate) fn paint_layer(&mut self, scratch: &mut RenderInput, layer: FxLayer) {
        if layer.clip.is_empty() || layer.content.items().is_empty() {
            return;
        }
        self.painted.layers.push(layer.clone());
        scratch.fx_layers.push(layer);
    }

    /// No card is on show: the splice returned before its card pass (the
    /// footer's `tab_status` gate), so none keys the repaint or arms a wake
    /// — and no banner is on the glass to be pressed.
    pub(crate) fn hide_all(&mut self) {
        for entry in &mut self.entries {
            entry.was_shown = entry.shown;
            entry.shown = false;
            entry.line.on = false;
            entry.notice.on = false;
        }
        self.panels.hide_all();
        self.notice_hits.clear();
    }

    /// The agents panels' state and the record their light joins (one
    /// [`strip`] takes the cards' and the panels' back).
    pub(crate) fn panels_and_painted(
        &mut self,
    ) -> (&mut crate::claude_panel::WindowPanels, &mut CardPainted) {
        (&mut self.panels, &mut self.painted)
    }

    /// The agents panels' state.
    pub(crate) fn panels(&self) -> &crate::claude_panel::WindowPanels {
        &self.panels
    }

    /// The agents panels' state, to change.
    pub(crate) fn panels_mut(&mut self) -> &mut crate::claude_panel::WindowPanels {
        &mut self.panels
    }

    /// The ripple clock of a card on show here that flows at `now`
    /// (`HarnessCard::ripple_origin`): the agents panels' crest steps on it
    /// (`claude_panel::PanelGrid`), so the window draws one frame a step for
    /// both. The clock is read only when a card on show is not at rest.
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

    /// The banners' rows this frame draws, as the footer's write takes them
    /// (`(frame_row, col, cells, ground)`, the cells borrowed).
    pub(crate) fn notice_edits(
        &self,
    ) -> impl Iterator<Item = (usize, usize, &[RenderCell], RenderCell)> {
        self.entries
            .iter()
            .map(|e| &e.notice)
            .filter(|n| n.on)
            .flat_map(|n| {
                n.rows()
                    .iter()
                    .map(move |r| (r.frame_row, r.col, r.cells.as_slice(), n.ground))
            })
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
    painted.clear();
}

/// Cut the lines' ink and star and the agents panels' light the last splice
/// put on the glass out of the frame cells `rect`: the tab menus
/// (`App::splice_tab_menu`, the Autopilot menu), topmost chrome painted
/// after the card, hang from the first grid row, where a fresh card sits,
/// and no ink, star or light of ours may show on them. The card's layers are
/// cut there by the occluder the menu's own scrub pushes
/// (`app_render::scrub_overlay_card`), which the renderers subtract from
/// every layer. A panel's light quad over the rectangle keeps only its parts
/// either side of it ([`cut_quad`]); an inked cell, a star or a dot's halo
/// inside it goes. The record is cut the same way, so [`strip`] still takes
/// back exactly what is on the glass. `cw` and `pad` are the window's cell
/// width and left padding: `glow_under` and `glow_halo` are window pixels,
/// and a quad's `row` is its frame row.
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
    // A panel's dot halo under the menu goes whole: it is a cell or three
    // wide, and the menu's edge through it would leave a cut light.
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

/// A halo's [`EntryKey`]: its quad, its colour, its centre, its radii and
/// its mode.
fn halo_key(h: &RainHalo) -> EntryKey {
    let [a, b, c] = h.damage_words();
    [a, b, c]
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

/// Take back one occurrence of each recorded layer, retaining other
/// producers even when they carry the same id: a layer is the record's
/// where it carries the record's id, slot and space and the very content
/// the record holds (`Arc::ptr_eq`), WHEREVER it now lies. A capture
/// re-extracts its scratch keeping the host's layers (`cell_frame_into`),
/// and the tab strip's prepend then moves them a row down
/// (`RenderInput::prepend_host_rows`), so by the next splice a layer this
/// one put on can sit a row below where the record says; matched by place,
/// it stayed on the glass and a copy drifted down the frame each capture.
/// The common tail, where nothing moved it, needs no scan.
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

/// Put each written line's ink and star on the glass, recorded for [`strip`]:
/// the ink only on cells no other producer inked (theirs keep their cells;
/// the stream stays sorted by `(row, col)` and unique, the renderers'
/// lockstep walk — so an unstable sort, which allocates nothing, is the
/// stable one), the star after the stream's last decoration. `took` says
/// which of this frame's lines ([`WindowCards::lines`]) and banner rows
/// ([`WindowCards::notice_edits`]) the footer's write put on the frame, by
/// where each starts. So with each banner: its ink and its star where its
/// first row's write took, and its hits ([`NoticeHit`]) on every row's that
/// did — the link on the `✦` and its words, nothing on the rest.
pub(crate) fn ink_lines(
    cards: &mut WindowCards,
    scratch: &mut RenderInput,
    took: impl Fn((usize, usize)) -> bool,
) {
    let WindowCards {
        entries,
        painted,
        notice_hits,
        ..
    } = cards;
    notice_hits.clear();
    let before = scratch.ink.len();
    let words = entries
        .iter()
        .map(|e| &e.line)
        .filter(|l| l.on && took(l.at()))
        .map(|l| (l.ink.as_slice(), l.star))
        .chain(entries.iter().map(|e| &e.notice).filter_map(|n| {
            let banner = n.rows().first()?;
            (n.on && took((banner.frame_row, banner.col))).then_some((n.ink.as_slice(), n.star))
        }));
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
    for entry in entries.iter().filter(|e| e.notice.on) {
        let notice = &entry.notice;
        for (k, row) in notice.rows().iter().enumerate() {
            if !took((row.frame_row, row.col)) {
                continue;
            }
            let hit = |cols: Range<usize>, url| NoticeHit {
                session: entry.session,
                frame_row: row.frame_row,
                term_row: row.term_row,
                cols,
                url,
            };
            if k == 0 {
                notice_hits.push(hit(notice.link.clone(), Some(NOTICE.url)));
            }
            notice_hits.push(hit(row.col..row.col + row.cells.len(), None));
        }
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

/// Set (or, with `None`, clear) this thread's test clock ([`card_now`]): the
/// agents panel's tests step it as the card's do.
#[cfg(test)]
pub(crate) fn set_test_now(now: Option<Instant>) {
    TEST_NOW.with(|t| t.set(now));
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
pub(crate) fn pack(rgb: [u8; 3]) -> u32 {
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

/// The end whose words a slot of [`line_texts`] keeps for `view`: a landing is
/// told under Land alone (its words are the face's), and every other end is
/// the view's own words — a timer's time among them, which follows Cruise's
/// words alone (§10.6, [`timed_line_texts`]). So a slot's words are the same
/// whichever end first asked for them: Land's never wear a timer's time
/// because a timed end was the first Land line asked for.
const fn untimed_end(view: AutopilotView, end: LineEnd) -> LineEnd {
    match (view, end) {
        (AutopilotView::Land, LineEnd::Landed) => LineEnd::Landed,
        _ => LineEnd::Mode,
    }
}

/// The line's words for `view` and how it ends ([`make_line_texts`]), made
/// once per pair: the build is the process's, so they never change while it
/// runs, and a frame formats nothing — they are looked up only where the
/// card is laid out, when its key moves or the cells its layout read do.
fn line_texts(view: AutopilotView, end: LineEnd) -> &'static [String] {
    static TEXTS: [OnceLock<Vec<String>>; 6] = [const { OnceLock::new() }; 6];
    let end = match (view, end) {
        (AutopilotView::Cruise, LineEnd::CruiseUntil(at)) => return timed_line_texts(None, at),
        _ => untimed_end(view, end),
    };
    let slot = match (view, end) {
        (AutopilotView::Land, LineEnd::Landed) => &TEXTS[5],
        (AutopilotView::Cruise, _) => &TEXTS[0],
        (AutopilotView::Land, _) => &TEXTS[1],
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
/// none); then, with no version, the hero word with the Autopilot's glyph
/// and word (`aterm harness · ∞ cruise`, `aterm · ∞ cruise`) and with its
/// glyph alone (`aterm ∞`). The version is the running build's own
/// signature (`menu::version_signature`), so one build reads the same here
/// as in the menu bar.
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
        _ => untimed_end(view, end),
    };
    let slot = match (view, end) {
        (AutopilotView::Land, LineEnd::Landed) => &table[5],
        (AutopilotView::Cruise, _) => &table[0],
        (AutopilotView::Land, _) => &table[1],
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
    let mut texts = Vec::with_capacity(7);
    let tag = agent.map_or_else(String::new, |a| format!(" \u{b7} {a}"));
    if let Some(full) = version_signature(facts, VersionRung::Full) {
        let head = format!("enhanced by {HERO_WORD} harness {full}{tag}");
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
        texts.push(format!("enhanced by {HERO_WORD} harness {short}{tag}"));
    }
    // THE SHORT RUNGS (owner, 2026-10-19: "emphasize aterm in the banner"):
    // where no version fits — a narrow pane's card, 31 to 50 columns — the
    // line keeps its hero word and the Autopilot's own glyph and word, the
    // chip's, then the glyph alone, so the bold `aterm` shows at every width
    // the card is decorated at.
    let (glyph, word) = end.words(view);
    texts.push(format!("{HERO_WORD} harness{tag} \u{b7} {glyph} {word}"));
    texts.push(format!("{HERO_WORD} \u{b7} {glyph} {word}"));
    texts.push(format!("{HERO_WORD} {glyph}"));
    texts
}

/// How many cells `text` takes (`aterm_grapheme::char_width`, a control or
/// zero-width char taking none).
fn text_cells(text: &str) -> usize {
    text.chars().map(aterm_grapheme::char_width).sum()
}

/// The cells of a line's words `text` that spell its [`HERO_WORD`] — its
/// first whole word so spelled — as offsets into the line's cells; `None`
/// where no word is.
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
    /// (`CardScene::cols`): the first past the card's text (or the kitty's
    /// margin) — never a column a glyph of the card's stands in. Where the
    /// text fills the pane's last column that is the pane's width, one past
    /// the pane, where the scene's clip cuts the right side away: the box
    /// is open at the pane's edge.
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
    // The pane's last column: what aterm adds (the line, the kitty) ends
    // before it, so a border drawn in it stays inside the pane.
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
    // Text that fills the pane's last column keeps its box, the right side
    // past the pane (`Layout::cols`). A new wide extension needs a clear
    // border column after its continuation; without one, leave the vendor's
    // card undecorated.
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
    // The border's column, clear of every glyph of the card: where the text
    // fills the pane's last column, the one past the pane (`Layout::cols`),
    // never the last column over the text's last glyph (measured at 33 and
    // 31 columns, where Claude's version and model rows end at the edge).
    let cols = kitty.map_or(text_end, |(_, _, cols)| cols).min(pane.cols);
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
    // The box's columns inside the pane: a right side past it reads nothing.
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
        palette: view.card_palette().unwrap_or_default(),
        dark,
        ground,
        serious,
        coat,
        eye,
        wand: place.wand().map(|(col, row)| (u16_of(col), u16_of(row))),
    }
}

/// Whether a pane's ground is dark: the card's light is added there, and
/// inked on a light one (`harness_card::dark_ground`, the line's own test;
/// the composer's border reads it too).
pub(crate) fn dark_ground(blank: RenderCell) -> bool {
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
        line_texts_for(read.kind, key.view, key.end),
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

/// How this frame put a card on the glass, as the ink of its words reads it
/// ([`ink_words`]): the clock for the ink (`None`: the card is still, or
/// may not move — the settled gradient, and no clock), whether it is
/// still, the flow step on the glass, and whether the card was replayed
/// rather than drawn. `frame` is `None` for a held card's replay (still,
/// and no clock read), else the clock the frame read, whether it drew the
/// card (`false`: a flow step replayed), and the flow step on the glass
/// (`harness_card::KeptFrame::step`).
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

    /// Whether ink made `settled`, or for flow step `step`, still shows what
    /// this frame would make: the settled gradient while the card stays
    /// still, a flow step's while the card is replayed within it.
    fn keeps(&self, settled: bool, step: Option<u64>) -> bool {
        (settled && self.still) || (self.replayed && self.step.is_some() && step == self.step)
    }
}

/// Claude's MASCOT'S INK on the frame `scratch` for the card at `place` on
/// `pane`: a fingerprint of each cell's glyph and colour across the
/// mascot's three rows, from column 0 to its last glyph's (a cell past a
/// trimmed row reads blank) — what Claude's shimmer repaints, a frame of
/// its ripple at a time. `None` where no mascot was read
/// ([`CardPlace::wand`]). The plain mascot
/// and the inline renderer's are static, and a frame that repainted nothing
/// there reads the same.
fn mascot_ink(scratch: &RenderInput, pane: PaneGeom, place: &CardPlace) -> Option<u64> {
    let (_, wand_row) = place.wand()?;
    let mut ink = 0xcbf2_9ce4_8422_2325_u64;
    for row in wand_row..wand_row + 3 {
        let cells = scratch.cells.get(pane.frame_row(row));
        for col in 0..place.mascot_end {
            let (ch, [r, g, b]) = cells
                .and_then(|cells| cells.get(pane.frame_col(col)))
                .map_or((' ', [0; 3]), |cell| (cell.ch, cell.fg));
            let word = u64::from(u32::from(ch))
                ^ (u64::from(r) << 32 | u64::from(g) << 40 | u64::from(b) << 48);
            ink = (ink ^ word).wrapping_mul(0x0100_0000_01b3);
        }
    }
    Some(ink)
}

/// The cell a card's words are drawn in on `pane` ([`make_line`],
/// [`make_notice`]): the footer's value colour — the card's own gray, its
/// working directory's, lifted toward the pane's ink — on the pane's blank
/// `ground`.
fn words_base(
    read: &CardRead,
    pane: PaneGeom,
    ground: RenderCell,
    scratch: &RenderInput,
) -> RenderCell {
    // Claude Code's working directory is its card's last row on every
    // layout (the wide card's third, the narrow card's fourth, a scrolled
    // card's last); agy's and muse's keep their third row, or their only.
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

/// `text`'s cells in `base`, appended to `out`: a wide char and its spacer,
/// a zero-width one none.
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

/// Ink `len` cells of words on frame row `frame_row` from two cells past
/// `star_col` with the card's line ink (`HarnessCard::line_ink_into`: the
/// card's palette, its intro's sweep and its flow, its glints running out
/// from Claude's wand, `hero` its own run) into `ink` (cleared first), and
/// make the `✦` that leads them in the first letter's colour: the one way
/// the line and the banner over Claude's notice are inked, so each palette,
/// the flow and the glints reach both alike.
#[allow(
    clippy::too_many_arguments,
    reason = "the words' place, their card, the pane, the frame's clock and motion"
)]
fn ink_words(
    card: &HarnessCard,
    read: &CardRead,
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
    let dark = dark_ground(ground);
    // The ripple's origin in the words' grid: the wand's frame cell.
    let wand = read
        .place
        .wand()
        .map(|(col, row)| (u16_of(pane.frame_col(col)), u16_of(pane.frame_row(row))));
    let at = LineAt {
        row: u16_of(frame_row),
        col: u16_of(star_col + LINE_LEAD),
        len: u16_of(len),
        hero,
        wand,
    };
    card.line_ink_into(inked.now, &at, pack(ground.bg), animate, ink);
    // The `✦` leads the words in their first letter's ink: added light at
    // full coverage on dark; on light opaque ink, as the letters are (text,
    // held to AA by `line_ink_into`), never a see-through tint of it.
    let star_rgb = ink.first().map_or(pack(ground.fg), |c| pack(c.color));
    WordDecoration {
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
    }
}

/// Make `line` the line laid at `place` on `pane` for this frame: its cells
/// (in the footer's value colour, [`words_base`]; the [`HERO_WORD`]'s bold),
/// its ink and its star ([`ink_words`]). What is already there is kept where
/// it shows the same: the cells while their key holds, the ink while the
/// card also stays still, or while a flowing card is replayed within the
/// step the ink was made for ([`InkFrame`], whose docs say what `frame` is).
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
        line.key = Some(key);
    }
    line.star = ink_words(
        card,
        read,
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

/// Make `notice` the banner over Claude Code's update notice at `place`
/// under the card `read` on `pane` for this frame ([`NoticeGlass`]): the
/// summary's first row the `✦` and [`NOTICE`]'s words at the notice's
/// column — in the line's cell ([`words_base`]), inked as the line is
/// ([`ink_words`]) — every row of the notice blanked to its end. Kept where
/// it shows the same, as the line is ([`make_line`]). Refused — the vendor's
/// notice bare — where the banner's words would reach past the notice's own
/// end or the pane's last column, or the notice is not inside the pane.
#[allow(
    clippy::too_many_arguments,
    reason = "the notice's place, its card, the frame's cells and the card's clock and motion"
)]
fn make_notice(
    notice: &mut NoticeGlass,
    card: &HarnessCard,
    place: &NoticePlace,
    read: &CardRead,
    pane: PaneGeom,
    ground: RenderCell,
    scratch: &RenderInput,
    palette: CardPalette,
    frame: Option<(Instant, bool, Option<u64>)>,
    animate: bool,
) {
    let words = text_cells(NOTICE.text);
    if place.text_col + LINE_LEAD + words > place.end.min(pane.cols.saturating_sub(1))
        || place.rows == 0
        || place.top + place.rows > pane.rows
    {
        return;
    }
    let key = NoticeKey {
        place: *place,
        pane,
        base: words_base(read, pane, ground, scratch),
        ground,
        dark: dark_ground(ground),
        palette,
    };
    notice.on = true;
    let inked = InkFrame::of(card, frame, animate);
    if notice.key == Some(key) && inked.keeps(notice.settled, notice.step) {
        return;
    }
    let star_col = pane.frame_col(place.text_col);
    if notice.key != Some(key) {
        let width = place.end - place.text_col;
        if notice.rows.len() < place.rows {
            notice.rows.resize_with(place.rows, NoticeRow::default);
        }
        notice.live = place.rows;
        for (k, row) in notice.rows[..place.rows].iter_mut().enumerate() {
            row.frame_row = pane.frame_row(place.top + k);
            row.term_row = pane.row_off + place.top + k;
            row.col = star_col;
            row.cells.clear();
            if k == 0 {
                // Under the `✦` and its space, the pane's blank.
                row.cells.resize(LINE_LEAD, ground);
                push_words(NOTICE.text, key.base, &mut row.cells);
            }
            row.cells.resize(width, ground);
        }
        notice.ground = ground;
        notice.link = star_col..star_col + LINE_LEAD + words;
        notice.key = Some(key);
    }
    notice.star = ink_words(
        card,
        read,
        pane,
        (pane.frame_row(place.top), star_col, words),
        0..0,
        ground,
        inked,
        animate,
        &mut notice.ink,
    );
    notice.settled = inked.still;
    notice.step = inked.step;
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
    // Only under Claude Code's card — three rows, four on a narrow pane, or
    // what is left of either scrolled up past the screen's top — is a
    // notice Claude's.
    let notice = card
        .as_ref()
        .filter(|(read, _, _)| read.kind == 3)
        .and_then(|(read, _, _)| texts.update_notice(leaf_index, session, &read.place, pane.cols));
    CardLeaf {
        leaf_index,
        session,
        pane,
        blank,
        view,
        end: LineEnd::Mode,
        card,
        notice,
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
        notice: None,
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
    /// The banner over a Claude Code update notice under window pixel
    /// `(x, y)` of `wid`, as the last frame wrote it ([`NoticeHit`]): its
    /// session, and the link a link-modifier press there opens — [`NOTICE`]'s
    /// on the banner's `✦` and words, none on the rest of what it hid (the
    /// summary's wrap, the details row's code.claude.com link). `None` off
    /// every banner, and on a row window chrome took since. No banner on the
    /// glass costs a look at an empty list.
    pub(crate) fn claude_notice_at(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(u64, Option<&'static str>)> {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.claude_card.notice_hits.is_empty())
        {
            return None;
        }
        // Laid by the footer's splice with the harness's glass: on the grid
        // its lists were laid on, or nowhere.
        let (row, col) = self.glass_cell_at(wid, x, y)?;
        let ws = self.windows.get(&wid)?;
        let hit = ws
            .claude_card
            .notice_hits
            .iter()
            .find(|hit| hit.frame_row == row && hit.cols.contains(&col))?;
        (!self.chrome_owns_terminal_row(wid, hit.term_row)).then_some((hit.session, hit.url))
    }

    /// Whether window `wid`'s cards may move — the motion verdict for the
    /// harness card (`motion`, OS Reduce Motion, the load-shed latch, and
    /// focus or a recording of the window: [`App::motion_focus`]): what each
    /// card is ticked with (`Look::animate`) and the repaint key folds
    /// ([`App::claude_card_term`]). Reads no clock.
    pub(crate) fn card_animates(&self, wid: WindowId, focused: bool) -> bool {
        let motion = crate::motion::MotionEffect::HarnessCard;
        self.effect_policy(motion, self.motion_focus(wid, focused))
            .animate(motion)
    }

    /// What window `wid` draws a decorated card with this frame ([`Look`]).
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
        let Some((focused, away)) = self
            .windows
            .get(&wid)
            .map(|ws| (ws.focused, ws.claude_card.away))
        else {
            return;
        };
        // The dev rows' probe (`holder_image::DEV_ROWS` builds run with
        // `ATERM_DEV_CARD_PROBE=1`): each leaf's outcome is noted on its
        // session's timeline after the pass, and nowhere else.
        let probe = crate::holder_image::DEV_ROWS && self.session_factory.card_probe;
        // What the card is drawn with, read before the window is borrowed —
        // and only where one is decorated: a window away less than
        // `RETURN_MS` moves on as a focused one (`moves_focused`), and one
        // whose note ran out lets it go below.
        let mut lapsed = false;
        let look = leaves.iter().any(|leaf| leaf.card.is_some()).then(|| {
            let focus = moves_focused(focused, away);
            lapsed = !focus && away.is_some();
            self.card_look(wid, focus)
        });
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
        if lapsed {
            cards.away = None;
        }
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
                // view that is not Cruise or Land (the master switch, an
                // agent's switch, no host; Off where the card switch, the
                // gate or the viewport kept it bare), so a move back into
                // Cruise or Land is a change that arms. A Cruise or Land frame
                // drawn bare is no such move: Off releases the session's
                // claim, and the frame between the word going back to Cruise
                // and the new worker's claim (tens of ms) is drawn bare —
                // recording Cruise there would leave the decorated frame
                // after it no change to arm on (the white Off card's entry
                // says Off). The same keeps a card scrolled back, or refused
                // by its gate, from arming anything when it shows again under
                // the mode it had. A hide by identity or fence bounds the
                // card's next show ([`RESHOW_MS`]), and a show held back is
                // dropped: there is no card to show.
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
            // A SCROLLED card (`CardRead::cut`): its top is off the screen,
            // and what is left wears no glass — but its notice keeps the
            // banner, from the card's own entry (drawn whole before it
            // scrolled: same process), in the palette of the Autopilot the
            // pane shows now — a mode moved while the card is scrolled, or a
            // find bar opened and closed over it, keeps it — SETTLED: the
            // entry is not on show, so it keys no repaint and arms no wake
            // for a flow nobody sees. A card first seen scrolled has no
            // entry, and its notice stays bare.
            if read.cut {
                if let Some(entry) = at.map(|at| &mut entries[at])
                    && same(entry)
                    && entry.whole
                    && let Some(place) = leaf.notice.as_ref().filter(|_| !find_up)
                {
                    let palette = leaf.view.card_palette().unwrap_or_default();
                    make_notice(
                        &mut entry.notice,
                        &entry.card,
                        place,
                        read,
                        leaf.pane,
                        leaf.blank,
                        scratch,
                        palette,
                        None,
                        false,
                    );
                }
                continue;
            }
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
                        entry.whole = false;
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
                // never held back — on the ripple of a card already flowing
                // here, if one is (`HarnessCard::adopt_ripple`): one frame a
                // step for both until a lock moves either to its own Claude's.
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
                        whole: false,
                        laid_gen: 0,
                        seen: Seen::default(),
                        line: CardLine::default(),
                        notice: NoticeGlass::default(),
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
            entry.whole = true;
            // A held card is replayed: its settled frame's own contents put
            // on the glass at the scene's origin, no clock read, nothing
            // built. Any other reads the clock: a flowing card between two
            // steps is replayed from the step it keeps, the same contents;
            // an intro's frame and a new step are drawn.
            let (glass, frame) = match entry.card.replay(&scene, look.animate, &entry.frames) {
                Some(kept) => {
                    entry.shimmer = None;
                    (kept, None)
                }
                None => {
                    let now = clock.get();
                    // Claude's own ripple, read off the cell its wand is
                    // drawn in on a frame that read the clock anyway: a
                    // flare's rising edge locks the card's glints to it, and
                    // any other repaint of its shimmer (its mascot's ink
                    // changed under the same wand) the flow's steps — each
                    // AS SEEN, on this frame. Only Claude Code's mascot
                    // ripples on Claude's clock; any other program's card
                    // free-runs.
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
            // The two layers: the border and the trail in window pixels under
            // the glyph ink, the kitty, its rim and the stars in the grid's
            // (every one lies right of the card's text, inside the pane),
            // both clipped to the pane.
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
                    frame
                        .map(|(now, drew)| (now, drew, entry.frames.kept().and_then(|k| k.step()))),
                    look.animate,
                );
            }
            // The banner over Claude Code's update notice: never while a find
            // bar is up — its matches lie in the text the banner hides, so
            // the vendor's notice stays bare to show them.
            if let Some(place) = leaf.notice.as_ref().filter(|_| !find_up) {
                make_notice(
                    &mut entry.notice,
                    &entry.card,
                    place,
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
    /// leaf `index` of window `wid`, `ws`, showing `session`), folded into
    /// `claude_footer_fp` beside the chips' — `view` and `supervised` the
    /// session's Autopilot as the footer's key read it, once: whether the
    /// harness wraps it (the switch, and [`AutopilotView::wears_glass`]: its
    /// Autopilot and, but for Off, the claim — [`card_leaf`]'s gate), which
    /// Autopilot and, wrapped, whether serious mode strips it to its border
    /// (a toggle on a settled card moves nothing else), and for a card on
    /// show its place, and, not serious, whether it may move (the motion
    /// verdict, [`App::card_animates`]: a gate reopening — Reduce Motion
    /// off, the load-shed latch let go — moves the key, so the flow resumes
    /// on the next wake rather than on an unrelated one; asked only for a
    /// card on show, so a window with none reads no motion policy), plus,
    /// while its intro plays, the intro's frame clock (one step a
    /// [`FRAME_MS`]) and, while it flows, the flow's step. Reads the clock
    /// only for a card that is not at rest (`HarnessCard::at_rest`), or on
    /// a window away less than [`RETURN_MS`] ago ([`moves_focused`]); takes
    /// no lock.
    ///
    /// The term names the frame the card WANTS, never the one it last drew:
    /// the window's key is read before its splice ticks the card, so a term
    /// that folded the drawn frame (its fingerprint) would move on the frame
    /// itself, and every later wake — the pointer's, another pane's output,
    /// a 120 Hz display's — would find the key moved and draw again, an
    /// intro then costing a frame a wake instead of one a [`FRAME_MS`], and
    /// the flow one a wake instead of one a step. For the same reason the
    /// frame owed when the intro runs out keys as the flow's step does: the
    /// intro's frame index goes, the key moves once, and the frame that
    /// draws the step leaves it where the next wake within the step reads it.
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
            // The motion verdict, asked only for a card on show: a window with
            // no card on its glass reads no motion policy.
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
    /// frame a card on glass wants ([`wanted_frame`]) — while an intro
    /// plays, a frame after the last one drawn (an instant later wakes do
    /// not move); while a card flows, its next step's instant; `now` once
    /// for the frame it owes as the intro runs out — each asked once
    /// ([`Self::claude_cards_due`]) and not again until a frame draws it;
    /// and `None` once every card is held, so an idle window arms
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

    /// The windows on screen whose card's frame is due at `now` — an intro's
    /// frame, a flow step or the frame owed as the intro runs out — each
    /// frame asked for once: one that
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
    /// (`HarnessCard::deadline`: an intro run out or a flow, past its settle
    /// grace with no frame drawn): it is marked asked here, so it stops being
    /// live ([`CardEntry::live`]) — a window minimized or hidden while it moves,
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

    /// Window `wid`'s focus edge, for what its glass plays on a return: a
    /// LOSS notes when the window went away, where a card or a breathing
    /// agents panel is on show (else nothing, and no clock is read); a GAIN
    /// after at least [`RETURN_MS`] away, or with no such note, is a RETURN:
    /// every card on show plays an episode again (§3) and every stuck dot on
    /// show breathes its course again ([`Self::rearm_claude_cards`],
    /// [`Self::rearm_claude_panels`]). A gain sooner than that is a blip
    /// (another process starting took the window server's focus for a
    /// moment): nothing plays again, and a card, which moved on through the
    /// blip as a focused window's ([`moves_focused`]), flows on from where
    /// it was, its steps riding Claude's repaints.
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
