// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native window's side of the effects driver (`docs/DESIGN-host-boundary-2026-08-30.md`).

use super::{
    CompanionOwner, ContentScrollDelta, ContentScrollState, CursorFx, CursorFxInput,
    EffectsPipeline, GlowConfig, MatrixRain, RainConfig, RainFrame, TrailConfig, WordDecorations,
    content_scroll_decision,
};
use crate::companion::{CompanionDuty, PetFacts, PetTick};
use crate::cursor_fx::CursorFxFrame;
use crate::cursor_glow::{
    CursorCatMotionPulse, CursorGlow, DeliveredClass, Geom, InsertWidth, ProbeTrust,
};
use crate::output_streak::{OutputStreak, StreakConfig, StreakCue, StreakFrame};
use crate::pane::{
    PanePlace, translate_decos_into_pane, translate_free_into_pane, translate_ink_into_pane,
    translate_nova_into_pane, translate_rain_into_pane,
};
use crate::word_decorations::{CompanionOnGlass, DecoConfig, EffectGeom, SelView};
use aterm_core::grid::LineSize;
use aterm_core::render::RenderInput;
use aterm_core::terminal::RenderCell;
use aterm_lexicon::Lexicon;
use aterm_render::GlowQuad;
use aterm_render::sprites::{FreeSprite, FreeZ};
use aterm_time::Instant;

/// What one content-scroll snapshot did to the cursor family's coordinates
/// ([`EffectsPipeline::sync_scroll`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollChange {
    /// Whole-plane rows the content moved up (translated, not reset).
    pub translated_rows: u16,
    /// Row-band moves replayed oldest first (an inline viewport's line).
    pub band_moves: u8,
    /// Non-uniform or ambiguous motion: the retained light was curtained.
    pub invalidated: bool,
}

impl ScrollChange {
    #[inline]
    #[must_use]
    pub const fn changed(self) -> bool {
        self.translated_rows > 0 || self.invalidated || self.band_moves > 0
    }

    /// A whole-plane translation and nothing else.
    #[inline]
    #[must_use]
    pub const fn translated_only(self) -> bool {
        self.translated_rows > 0 && !self.invalidated && self.band_moves == 0
    }
}

/// What the torn-space retirement does with Rainbow Kitty's band, which is about the grid's cells
/// rather than the window's pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TornBand {
    /// The coordinate space itself was replaced (the alternate screen, a tab or terminal switch, a
    /// re-grid).
    Curtain,
    /// A history viewport is on glass over an unchanged live grid.
    Keep,
}

/// One delivery receipt: the insert half arms on every window that reads the session's receipt.
#[derive(Clone, Copy, Debug)]
pub struct Delivery {
    /// When the sink accepted the bytes.
    pub at: Instant,
    /// When the key or paste was dispatched at the input seam.
    pub dispatched_at: Instant,
    pub insert: Option<InsertWidth>,
    pub key: Option<DeliveredClass>,
}

/// The cursor row the host captured under its terminal lock, with the class of detector that may
/// read it.
#[derive(Clone, Copy, Debug)]
pub struct RowProbe<'a> {
    pub row: u16,
    pub caret: u16,
    pub trust: ProbeTrust,
    pub cols: &'a [char],
    /// The flanking rows the star landings and the content witnesses read.
    pub flanks: Option<Flanks<'a>>,
}

/// The rows `(above, below)` flanking a probed cursor row.
pub type Flanks<'a> = (Option<&'a [char]>, Option<&'a [char]>);

/// One frame of the cursor family, as the host resolved it.
pub struct CursorStep<'a> {
    /// The family's frame input (clock, caret, geometry, the policy folds).
    pub input: CursorFxInput,
    /// The resolved glow config, already folded for motion, audibility and the live shape's head
    /// anchor ([`crate::cursor_fx::head_dx_for`]).
    pub glow: &'a GlowConfig,
    /// The comet's config, already gated by the motion policy.
    pub trail: TrailConfig,
    /// The motion policy lets the family animate (`CursorFx::begin`'s fresh-ink reduced arm).
    pub animate: bool,
    pub row_probe: Option<RowProbe<'a>>,
    /// The cells of the frame about to be presented (window rows), whose text tells the dark streak
    /// where it crosses air ([`crate::cursor_glow::CursorGlow::observe_frame_text`]).
    pub frame_cells: Option<&'a [Vec<RenderCell>]>,
    /// The window cell `(row, col)` of `frame_cells`' first cell: `(0, 0)`
    /// for the window's own cells, a split's offset for its focused pane's.
    pub frame_cells_at: (u16, u16),
    /// Keystrokes the host batched since its last step, replayed at this frame's instant after the
    /// history fence (a page's straggler keys; a native window notes each key at its press and
    /// passes `0`).
    pub pending_keys: u32,
    /// The echo anchor `(row, col, seq)` and the glyph its run ended on.
    pub print_anchor: Option<(u16, u16, u64)>,
    pub print_anchor_glyph: Option<char>,
    /// DECTCEM draws the caret this frame hands over.
    pub caret_drawn: bool,
}

/// What [`EffectsPipeline::cursor_step`] produced beyond the family's frame.
#[derive(Clone, Copy, Debug)]
pub struct CursorStepOut {
    pub frame: CursorFxFrame,
    /// The ribbon's echo-correlated forward pulse this tick — the flying head's momentum source.
    pub motion_pulse: Option<CursorCatMotionPulse>,
}

/// Which sprite channel a host companion draws into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasInk {
    Window,
    Pane,
}

/// The flying head's seat for a frame ([`EffectsPipeline::route_flying_head`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FlyingSeat {
    /// Whole rows lifted above the caret's row, for the host's
    /// [`crate::word_decorations::KittyCursorLayout::lift`].
    pub lift: f32,
    /// The target seat `(x0, x1, y0, y1)` in grid pixels, right and bottom exclusive.
    pub px: Option<(i32, i32, i32, i32)>,
}

/// A snapshot rescan inside a word pass.
#[derive(Clone, Copy, Debug)]
pub struct WordRescan {
    pub birth: Instant,
    pub cursor: Option<(u16, u16)>,
}

/// One grid's word pass — a single-pane present, a single-pane capture.
pub struct WordGrid<'a> {
    pub now: Instant,
    pub lexicon: &'a Lexicon,
    /// The tick config, the host's motion policy folded into `reduced_motion`.
    pub cfg: &'a DecoConfig,
    pub geom: EffectGeom,
    pub cells: &'a [Vec<RenderCell>],
    pub line_sizes: &'a [LineSize],
    /// The snapshot's row origin, for the kitty-command flash.
    pub base_y: i64,
    pub default_bg: u32,
    pub epoch: u64,
    pub rescan: Option<WordRescan>,
    /// Where the frame's one companion stands, so the word under it yields.
    pub companion_at: Option<CompanionOnGlass>,
    pub sel: SelView<'a>,
    /// The window's focus, which gates the idle-life one-shots.
    pub focused: bool,
}

/// What a grid's word pass produced.
#[derive(Clone, Copy, Debug, Default)]
pub struct WordGridOut {
    pub fp: u64,
    /// Curse sites the primed birth frame saw (a capture's first sight of an unseen damaged frame).
    pub primed_wince_hits: u8,
}

/// One visible pane of a composed frame's word pass.
pub struct WordPane<'a> {
    pub session: u64,
    pub focused: bool,
    pub place: PanePlace,
    pub grid: Option<PaneGrid<'a>>,
}

/// The snapshot a composed pane's word pass reads, extracted under the frame's authorization hold.
pub struct PaneGrid<'a> {
    pub cells: &'a [Vec<RenderCell>],
    pub line_sizes: &'a [LineSize],
    pub base_y: i64,
    pub default_bg: u32,
    pub epoch: u64,
    pub alt: bool,
    pub display_offset: i32,
    pub cursor: (u16, u16),
    pub sel: &'a aterm_core::selection::TextSelection,
}

/// The frame-wide inputs of a composed word pass.
pub struct WordPanes<'a> {
    pub now: Instant,
    pub lexicon: &'a Lexicon,
    pub cfg: &'a DecoConfig,
    pub tick_cfg: &'a DecoConfig,
    pub load_shed: bool,
    /// The un-woken focus a pane's presentability takes.
    pub word_focus: bool,
    /// The window's focus, which gates the idle-life one-shots.
    pub window_focused: bool,
    /// The frame's one companion, in the focused pane's pixel space.
    pub companion_at: Option<CompanionOnGlass>,
}

/// The pane a composed word pass hands its host's drains after the pane's tick.
#[derive(Clone, Copy, Debug)]
pub struct PaneDrain {
    pub session: u64,
    pub cols: u16,
}

/// What a composed word pass produced.
#[derive(Clone, Copy, Debug, Default)]
pub struct WordPanesOut {
    pub fp: u64,
    /// The focused pane's placement when it got past suspension — where the host's typed dog may
    /// draw (it draws on no suspended pane).
    pub focus_place: Option<PanePlace>,
    /// Some pane rescanned its snapshot this frame.
    pub rescanned: bool,
}

/// The output-echo facts prism wake's licence reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreakEcho {
    /// The latest submitted turn boundary.
    pub boundary_at: Option<Instant>,
    /// The latest keystroke the sink accepted.
    pub accepted_at: Option<Instant>,
    /// Input is hot: an echo frame, not arrival.
    pub input_hot: bool,
}

/// Prism wake on one grid.
pub struct StreakGrid<'a> {
    pub now: Instant,
    pub cfg: StreakConfig,
    /// The engine's seed, used when it is first built.
    pub seed: u64,
    pub echo: StreakEcho,
    pub cells: &'a [Vec<RenderCell>],
    pub cursor: (usize, usize),
    pub viewport: (usize, usize),
    pub content_seq: u64,
    pub geom: Geom,
}

/// Prism wake on one pane of a composed frame.
pub struct StreakPane<'a> {
    pub session: u64,
    pub slot: usize,
    pub place: PanePlace,
    pub echo: StreakEcho,
    pub snapshot: &'a RenderInput,
    pub content_seq: u64,
    /// This pane's episode may sound (the focused pane of a present).
    pub sound: bool,
}

/// The frame-wide prism wake knobs of a composed frame.
#[derive(Clone, Copy, Debug)]
pub struct StreakKnobs {
    pub now: Instant,
    pub intensity: f32,
    pub tail: u16,
    pub max_streaks: u8,
    pub idle_secs: f32,
}

/// Where a composed pane's prism wake light lands.
pub enum NovaInto<'a> {
    Window,
    Staged(&'a mut Vec<GlowQuad>),
}

impl EffectsPipeline {
    /// The pipeline a native window owns.
    #[inline]
    #[must_use]
    pub fn for_window() -> Self {
        Self {
            companion: CompanionOwner::dressed_by_host(),
            ..Self::new()
        }
    }

    /// The cursor family (aurora, comet, cadence, block bodies, scratch).
    #[inline]
    #[must_use]
    pub const fn cursor(&self) -> &CursorFx {
        &self.cursor
    }

    #[inline]
    pub const fn cursor_mut(&mut self) -> &mut CursorFx {
        &mut self.cursor
    }

    /// The word engine (sparkle words, peeking cats, the sprite atlas).
    #[inline]
    #[must_use]
    pub const fn decos(&self) -> &WordDecorations {
        &self.decos
    }

    #[inline]
    pub const fn decos_mut(&mut self) -> &mut WordDecorations {
        &mut self.decos
    }

    /// The resident pet's one driver.
    #[inline]
    #[must_use]
    pub const fn companion(&self) -> &CompanionOwner {
        &self.companion
    }

    #[inline]
    pub const fn companion_mut(&mut self) -> &mut CompanionOwner {
        &mut self.companion
    }

    /// Which cat the companions wear ([`crate::companion_look::LookPolicy`]).
    #[inline]
    #[must_use]
    pub const fn look(&self) -> &crate::companion_look::LookPolicy {
        &self.look
    }

    #[inline]
    pub const fn look_mut(&mut self) -> &mut crate::companion_look::LookPolicy {
        &mut self.look
    }

    /// The companions beside the resident pet ([`crate::companions`]).
    #[inline]
    #[must_use]
    pub const fn companions(&self) -> &crate::companions::Companions {
        &self.companions
    }

    #[inline]
    pub const fn companions_mut(&mut self) -> &mut crate::companions::Companions {
        &mut self.companions
    }

    /// The companions and the cursor family together, for a host step that feeds one from the other
    /// in one breath (the song's sync reads the glow's style while it drives the flying head).
    #[inline]
    pub const fn companions_with_cursor(
        &mut self,
    ) -> (&mut crate::companions::Companions, &mut CursorFx) {
        (&mut self.companions, &mut self.cursor)
    }

    /// Serious Mode's drain, the engines' half.
    #[inline]
    pub fn drain_serious(&mut self) {
        self.cursor.glow.reset();
        self.cursor.retire_bodies();
        self.companion.drain();
        self.companions.drain_serious();
        self.decos.hard_reset();
        self.flying_sky_row = None;
        self.rain = None;
        self.cursor.glow_scratch.clear();
        self.cursor.glow.clear_out_ramps();
        self.cursor.trail_scratch.clear();
        self.clear_word_scratch();
        self.rain_scratch.clear();
        self.rain_add_scratch.clear();
    }

    /// The frame's free sprites (peeking cats, their gaze dots, companions).
    #[inline]
    #[must_use]
    pub const fn free_sprites(&self) -> &[FreeSprite] {
        self.free_scratch.as_slice()
    }

    /// The frame's additive nova light (the supernovae, then prism wake).
    #[inline]
    #[must_use]
    pub const fn nova(&self) -> &[GlowQuad] {
        self.nova_scratch.as_slice()
    }

    /// The rain driver's session-keyed edges.
    #[inline]
    #[must_use]
    pub const fn rain_latches(&self) -> &crate::matrix_rain::RainLatches {
        &self.rain_latches
    }

    /// The owner the cursor family's retained light belongs to: `(terminal identity, alternate
    /// screen)`, `None` before the first frame.
    #[inline]
    #[must_use]
    pub const fn owner_space(&self) -> Option<(u64, bool)> {
        self.cursor_owner_space
    }

    /// Forget the owner, so the next fence baselines silently.
    #[doc(hidden)]
    #[inline]
    pub const fn forget_owner_space(&mut self) {
        self.cursor_owner_space = None;
    }

    /// The single grid's prism wake engine slot, for host tests that stage a scripted state.
    #[doc(hidden)]
    #[inline]
    pub const fn streak_slot(&mut self) -> &mut Option<Box<OutputStreak>> {
        &mut self.streak
    }

    /// The composed frame's prism wake engines, for host tests that stage a scripted state.
    #[doc(hidden)]
    #[inline]
    pub const fn streak_panes_mut(
        &mut self,
    ) -> &mut std::collections::BTreeMap<(u64, usize), OutputStreak> {
        &mut self.streak_panes
    }

    /// Apply the terminal's cumulative content-scroll clock to the cursor family, the one scroll
    /// law both hosts run.
    #[inline]
    pub fn sync_scroll(&mut self, current: ContentScrollState, now: Instant) -> ScrollChange {
        let decision = content_scroll_decision(self.cursor_scroll_state, current);
        self.cursor_scroll_state = Some(current);
        match decision {
            ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged => ScrollChange::default(),
            ContentScrollDelta::Translate(translated_rows) => {
                self.cursor.scroll_translate(translated_rows);
                self.flying_sky_row = match self.flying_sky_row {
                    Some(r) => r.checked_sub(translated_rows),
                    None => None,
                };
                ScrollChange {
                    translated_rows,
                    ..ScrollChange::default()
                }
            }
            ContentScrollDelta::Bands { first_seq, count } => {
                self.cursor.scroll_bands(&current, first_seq, count);
                if let Some(mut row) = self.flying_sky_row {
                    let mut next = Some(row);
                    let count_u64 = u64::from(count);
                    let mut i = 0;
                    while i < count_u64 {
                        let m = current.band(first_seq + i);
                        match crate::cursor_glow::band_row(row, m.top, m.bottom, m.delta) {
                            Some(r) => row = r,
                            None => {
                                next = None;
                                break;
                            }
                        }
                        next = Some(row);
                        i += 1;
                    }
                    self.flying_sky_row = next;
                }
                ScrollChange {
                    band_moves: count,
                    ..ScrollChange::default()
                }
            }
            ContentScrollDelta::Invalidate => {
                self.cursor.scroll_invalidate(now);
                self.flying_sky_row = None;
                ScrollChange {
                    invalidated: true,
                    ..ScrollChange::default()
                }
            }
        }
    }

    /// The last content-scroll snapshot the cursor family consumed — the baseline a host's witness
    /// capture compares its rows against.
    #[inline]
    #[must_use]
    pub const fn scroll_baseline(&self) -> Option<ContentScrollState> {
        self.cursor_scroll_state
    }

    /// Re-baseline the scroll clock at `state` (a reflow the host performed is not PTY output).
    #[inline]
    pub const fn set_scroll_baseline(&mut self, state: Option<ContentScrollState>) {
        self.cursor_scroll_state = state;
    }

    /// The front terminal was replaced under this surface (tab switch, pane-focus move, session
    /// migration), so every coordinate, probe and stream baseline belongs to the old grid.
    #[inline]
    pub fn front_replaced(&mut self, now: Instant) {
        self.cursor.glow.drop_row_probe();
        self.cursor.glow.clear_blink();
        self.cursor.trail.clear_blink();
        self.cursor.glow.curtain(now);
        self.cursor.trail.reset();
        self.decos.rebase_kitty_cursor_placement();
        self.flying_sky_row = None;
        self.companion.resight_owner();
        self.cursor_scroll_state = None;
        self.cursor_owner_space = None;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_grid_replaced();
        }
        if let Some(streak) = self.streak.as_deref_mut() {
            streak.rebase();
        }
    }

    /// The engines' half of the torn-space law.
    #[inline]
    pub fn retire_torn(&mut self, band: TornBand, now: Instant) {
        self.retire_torn_marks(band, now);
        self.companion.retire_coordinate_space();
    }

    /// [`Self::retire_torn`] for a surface replaced under the resident (a different focused view).
    #[inline]
    pub fn resight_torn(&mut self, now: Instant) {
        self.retire_torn_marks(TornBand::Curtain, now);
        self.companion.resight_coordinate_space();
    }

    /// [`Self::retire_torn`] for the same surface re-gridded (resize, font step, DPI change,
    /// divider drag, zoom that keeps focus).
    #[inline]
    pub fn regrid_torn(&mut self, now: Instant) {
        self.retire_torn_marks(TornBand::Curtain, now);
        self.companion.rebase_geometry();
    }

    /// Every cursor-anchored mark but the resident's: the three torn seams
    /// differ only in what the pet does.
    #[inline]
    fn retire_torn_marks(&mut self, band: TornBand, now: Instant) {
        match band {
            TornBand::Curtain => self.cursor.glow.curtain(now),
            TornBand::Keep => {
                if !self.cursor.glow.v2_owns_frame() {
                    self.cursor.glow.reset();
                }
            }
        }
        self.cursor.retire_body_geometry();
        self.decos.rebase_kitty_cursor_placement();
        self.flying_sky_row = None;
        self.free_scratch.clear();
    }

    /// The owner fence, keyed `(terminal identity, alternate screen)`: the first observation
    /// baselines silently.
    #[inline]
    pub fn fence_owner(&mut self, current: (u64, bool), now: Instant) -> bool {
        let changed = matches!(
            self.cursor_owner_space,
            Some(previous) if previous != current
        );
        self.cursor_owner_space = Some(current);
        if changed {
            self.resight_torn(now);
            self.cursor_scroll_state = None;
            self.companion.resight_owner();
        }
        changed
    }

    /// The cursor family's frame ([`step_cursor_family`]) on this pipeline's engines, with the
    /// host's resolved glow config.
    #[inline]
    pub fn cursor_step(
        &mut self,
        step: &CursorStep<'_>,
        deliveries: impl IntoIterator<Item = Delivery>,
    ) -> CursorStepOut {
        step_cursor_family(&mut self.cursor, step, deliveries)
    }

    /// Hand a single-grid frame the cursor family's streams.
    #[inline]
    pub fn publish_cursor(&mut self, input: &mut RenderInput) {
        std::mem::swap(&mut input.cursor_glow_add, &mut self.cursor.glow_scratch);
        self.cursor
            .glow
            .swap_out_ramps(&mut input.cursor_glow_add_ramps);
        self.cursor.glow.swap_halos(&mut input.glow_halo);
        self.cursor.glow.swap_patches(&mut input.fire_patch);
        self.cursor.glow.swap_under_quads(&mut input.glow_under);
        self.cursor
            .glow
            .swap_under_ramps(&mut input.glow_under_ramps);
        self.cursor.glow.swap_charred(&mut input.char_fg);
        self.cursor.glow.swap_halo_cells(&mut input.fire_halo);
        input.cursor_trail.clone_from(&self.cursor.trail_scratch);
        input.bloom_gate = aterm_core::render::BloomGate::of(self.cursor.bloom_held);
    }

    /// Open a frame's word channels.
    #[inline]
    pub fn open_words(&mut self) {
        self.clear_word_scratch();
        self.cursor.glow.note_word_icons(std::iter::empty());
        self.decos.begin_host_frame();
    }

    /// The word pass's icons from `from` on in the window's sprites, to the
    /// next frame's sky and ribbon ([`crate::cursor_glow::CursorGlow::note_word_icons`]).
    #[inline]
    pub(super) fn note_word_icons(&mut self, from: usize) {
        let icons = self
            .free_scratch
            .get(from..)
            .unwrap_or_default()
            .iter()
            .filter(|s| s.z == FreeZ::UnderText && !s.is_blank());
        self.cursor.glow.note_word_icon_sprites(icons);
    }

    /// Lift the trail's light off the icons the frame's word pass named
    /// ([`crate::cursor_glow::CursorGlow::shade_word_icons`]), in the streams
    /// `input` carries, by the icons' drawn rows in their art: every host
    /// hands it the trail before its word pass.
    #[inline]
    pub fn shade_word_icons(&mut self, input: &mut RenderInput) {
        #[cfg(test)]
        if self.word_icons_unshaded {
            return;
        }
        self.cursor.glow.shade_word_icons((
            (&mut input.cursor_glow_add, &mut input.cursor_glow_add_ramps),
            (&mut input.glow_under, &mut input.glow_under_ramps),
            &mut input.glow_halo,
            &mut input.fire_patch,
        ));
    }

    #[inline]
    fn clear_word_scratch(&mut self) {
        self.deco_scratch.clear();
        self.ink_scratch.clear();
        self.free_scratch.clear();
        self.nova_scratch.clear();
    }

    /// A suspended word pass (suppressed alternate screen, load-shed latch).
    #[inline]
    pub fn suspend_words(&mut self, now: Instant) {
        self.decos.freeze(now);
        self.clear_word_scratch();
    }

    /// One grid's word pass.
    #[inline]
    pub fn word_grid(&mut self, g: &WordGrid<'_>) -> WordGridOut {
        self.decos.thaw(g.now);
        let (rows, cols) = (usize::from(g.geom.rows), usize::from(g.geom.cols));
        let mut prime = None;
        if let Some(r) = g.rescan {
            self.decos.set_scan_base_y(Some(g.base_y));
            self.decos.rescan_from_cells_with_geom_at_cursor(
                g.cells,
                g.line_sizes,
                rows,
                cols,
                g.lexicon,
                g.cfg,
                g.epoch,
                r.birth,
                g.geom,
                g.default_bg,
                r.cursor,
            );
            if r.birth < g.now {
                prime = Some(r.birth);
            }
        }
        let mut primed_wince_hits = 0;
        if let Some(birth) = prime {
            self.decos.begin_host_frame();
            self.tick_words(birth, g);
            primed_wince_hits = crate::sound_policy::drain_curse_bonk_cues(
                &mut self.decos,
                self.glow_cfg.style,
                crate::trail_sound::SoundVoice::Style,
                g.geom.cols,
                None,
                false,
                |_| {},
            )
            .wince_hits;
        }
        self.decos.begin_host_frame();
        let fp = self.tick_words(g.now, g);
        // The tick clears the sprite list before it draws (a primed tick's sprites with it), so
        // every sprite in it is this tick's; counting from the length before the tick would skip
        // a primed frame's icons, and the sky would draw its stars on them.
        self.note_word_icons(0);
        WordGridOut {
            fp,
            primed_wince_hits,
        }
    }

    #[inline]
    fn tick_words(&mut self, now: Instant, g: &WordGrid<'_>) -> u64 {
        self.decos.tick(
            now,
            g.cfg,
            g.geom,
            g.companion_at,
            Some(g.sel),
            g.focused,
            &mut self.deco_scratch,
            &mut self.ink_scratch,
            &mut self.free_scratch,
            &mut self.nova_scratch,
        )
    }

    /// This tick's positioned peek landings reach the pet as fractional cells (the cues clear at
    /// the next tick's start, so forward them promptly).
    #[inline]
    pub fn forward_peek_cues(&mut self, now: Instant, cell: (u16, u16)) {
        self.companion
            .note_peeks(now, self.decos.drain_peek_cues(), cell);
    }

    /// A composed (split) frame's word pass.
    pub fn word_panes<'p>(
        &mut self,
        f: &WordPanes<'_>,
        panes: impl IntoIterator<Item = WordPane<'p>>,
        mut drained: impl FnMut(PaneDrain, &mut WordDecorations),
    ) -> WordPanesOut {
        let mut out = WordPanesOut::default();
        let from = self.free_scratch.len();
        for (index, pane) in panes.into_iter().enumerate() {
            let place = pane.place;
            let px_origin = (
                i32::from(place.col_off) * place.cell_w as i32,
                i32::from(place.row_off) * place.cell_h as i32,
            );
            self.decos.bind_pane(pane.session, px_origin);
            let Some(grid) = pane.grid else {
                continue;
            };
            let geom = EffectGeom {
                cell_w: place.cell_w as u16,
                cell_h: place.cell_h as u16,
                rows: place.rows,
                cols: place.cols,
            };
            let suspended = f.load_shed || (grid.alt && f.cfg.suppress_in_alt_screen);
            let rescan = !suspended && self.decos.needs_rescan(grid.epoch);
            self.decos
                .set_presentable(f.now, f.word_focus && !suspended);
            if suspended {
                self.decos.freeze(f.now);
                continue;
            }
            self.decos.thaw(f.now);
            if pane.focused {
                out.focus_place = Some(place);
            }
            if rescan {
                self.decos.set_scan_base_y(Some(grid.base_y));
                self.decos.rescan_from_cells_with_geom_at_cursor(
                    grid.cells,
                    grid.line_sizes,
                    usize::from(place.rows),
                    usize::from(place.cols),
                    f.lexicon,
                    f.tick_cfg,
                    grid.epoch,
                    f.now,
                    geom,
                    grid.default_bg,
                    if grid.display_offset == 0 {
                        Some(grid.cursor)
                    } else {
                        None
                    },
                );
                out.rescanned = true;
            }
            let pane_fp = self.decos.tick(
                f.now,
                f.tick_cfg,
                geom,
                if pane.focused { f.companion_at } else { None },
                Some(SelView {
                    sel: grid.sel,
                    display_offset: grid.display_offset,
                }),
                f.window_focused,
                &mut self.pane_deco,
                &mut self.pane_ink,
                &mut self.pane_free,
                &mut self.pane_nova,
            );
            out.fp ^= pane_fp.rotate_left((index % 64) as u32);
            drained(
                PaneDrain {
                    session: pane.session,
                    cols: place.cols,
                },
                &mut self.decos,
            );
            if pane.focused {
                self.forward_peek_cues(f.now, (geom.cell_w, geom.cell_h));
            } else {
                let _ = self.decos.drain_peek_cues();
            }
            translate_decos_into_pane(&mut self.pane_deco, place);
            translate_ink_into_pane(&mut self.pane_ink, place);
            translate_free_into_pane(&mut self.pane_free, place);
            translate_nova_into_pane(&mut self.pane_nova, place);
            self.deco_scratch.extend_from_slice(&self.pane_deco);
            self.ink_scratch.extend_from_slice(&self.pane_ink);
            self.free_scratch.extend_from_slice(&self.pane_free);
            self.nova_scratch.extend_from_slice(&self.pane_nova);
        }
        if self.ink_scratch.len() > 1 {
            self.ink_scratch.sort_unstable_by_key(|c| (c.row, c.col));
        }
        self.note_word_icons(from);
        out
    }

    /// Drop the parked episodes of every pane this window stopped showing.
    #[inline]
    pub fn retain_word_panes(&mut self, keep: impl Fn(u64) -> bool) {
        self.decos.retain_panes(keep);
    }

    /// Place the pane staging sprites (a companion the host drew for the focused pane) into the
    /// window grid and append them to its channel.
    #[inline]
    pub fn place_pane_sprites(&mut self, place: PanePlace) {
        translate_free_into_pane(&mut self.pane_free, place);
        self.free_scratch.extend_from_slice(&self.pane_free);
    }

    /// Clear the pane staging sprites before a host companion draws into it.
    #[inline]
    pub fn clear_pane_sprites(&mut self) {
        self.pane_free.clear();
    }

    /// The resident pet's emission ([`CompanionOwner::emit_on`]), the palette sampled over `cells`
    /// (the snapshot the pet stands on).
    #[inline]
    pub fn draw_pet(
        &mut self,
        ink: CanvasInk,
        pet: &crate::companion::CompanionFrame,
        cells: &[Vec<RenderCell>],
        fallback: crate::companion::ContrastFallback,
    ) -> (u64, crate::kitty_pet::SyncLookOutcome) {
        let (free, window) = match ink {
            CanvasInk::Window => (&mut self.free_scratch, None),
            CanvasInk::Pane => (&mut self.pane_free, Some(&mut self.free_scratch)),
        };
        self.companion
            .emit_on(pet, cells, fallback, &mut self.decos, free, window)
    }

    /// The flying head's route (`RAINBOW-KITTY-V2.md` §7.2, D13).
    #[inline]
    pub fn route_flying_head(
        &mut self,
        duty: CompanionDuty,
        cells: &[Vec<RenderCell>],
        now: Instant,
        geom: EffectGeom,
        cat_frame: &mut crate::kitty_cursor::CatFrame,
        animate: bool,
    ) -> FlyingSeat {
        use crate::rainbow_kitty::companion::{
            self as companion, Body, BodyImpulse, CompanionAdmission, Reaction, SeatQuery,
        };
        if self.cursor.glow.v2_status().is_none() {
            self.flying_sky_row = None;
            return FlyingSeat::default();
        }
        let body = companion::body_for(CompanionAdmission {
            duty,
            body_claimed: false,
        });
        let impulse = match self.cursor.glow.take_companion_impulse() {
            Some(impulse) => companion::impulse_for(body, impulse),
            None => BodyImpulse::Ignore,
        };
        let cat = &mut self.companions.cursor_cat;
        match impulse {
            BodyImpulse::Fly(flight) if !flight.dir.is_vertical() => {
                self.decos.rebase_kitty_cursor_placement();
                self.flying_sky_row = None;
                if animate {
                    cat.on_meteor(flight, now, cat_frame);
                }
            }
            BodyImpulse::React(Reaction::Land) if matches!(body, Body::Flying { .. }) => {
                self.decos.rebase_kitty_cursor_placement();
                self.flying_sky_row = None;
            }
            BodyImpulse::React(Reaction::Delight) if matches!(body, Body::Flying { .. }) => {
                if cat.on_delight(now, 1) && animate {
                    cat.refresh_reaction_frame(now, cat_frame);
                }
            }
            BodyImpulse::Fly(_)
            | BodyImpulse::Perk { .. }
            | BodyImpulse::React(Reaction::Delight | Reaction::Wince | Reaction::Land)
            | BodyImpulse::Ignore => {}
        }
        let Body::Flying { cell } = body else {
            self.flying_sky_row = None;
            return FlyingSeat::default();
        };
        let Some(rest) =
            self.decos
                .kitty_cursor_footprint(crate::word_decorations::KittyCursorLayout {
                    geom,
                    cursor: cell,
                    look: cat_frame.render_look(),
                    bob: cat_frame.bob,
                    lift: 0.0,
                })
        else {
            self.flying_sky_row = None;
            return FlyingSeat::default();
        };
        let ink = |row: u16, col: u16| companion::cell_ink(cells, row, col, geom.cols);
        let seat = companion::placement(
            cell,
            &SeatQuery {
                geom: Geom {
                    cw: usize::from(geom.cell_w),
                    ch: usize::from(geom.cell_h),
                    rows: usize::from(geom.rows),
                    cols: usize::from(geom.cols),
                    origin_x: 0,
                    origin_y: 0,
                    win_w: 0,
                    win_h: 0,
                    head: 0,
                },
                rest,
                lead_px: WordDecorations::kitty_cursor_lead_px(geom.cell_w),
                held_sky: self.flying_sky_row == Some(cell.0),
            },
            ink,
        );
        self.flying_sky_row = if seat.lift_rows > 0 && !seat.over_ink {
            Some(cell.0)
        } else {
            None
        };
        // A lifted seat's top is the footprint's own, which stops at the grid top as the drawn head does.
        let seat_y = if seat.lift_rows == 0 {
            seat.y
        } else {
            match self
                .decos
                .kitty_cursor_footprint(crate::word_decorations::KittyCursorLayout {
                    geom,
                    cursor: cell,
                    look: cat_frame.render_look(),
                    bob: cat_frame.bob,
                    lift: f32::from(seat.lift_rows),
                }) {
                Some(lifted) => lifted.y,
                None => seat.y.max(0),
            }
        };
        let dx = seat.x.saturating_sub(rest.x);
        if dx != 0 && geom.cell_w > 0 {
            let facing_left = self.companions.cursor_cat.placement_frame(now).facing_left;
            let lead_cells = dx as f32 / f32::from(geom.cell_w);
            cat_frame.pose.lead += if facing_left { -lead_cells } else { lead_cells };
        }
        FlyingSeat {
            lift: f32::from(seat.lift_rows),
            px: Some((
                seat.x,
                seat.x + i32::from(seat.w),
                seat_y,
                seat_y + i32::from(seat.h),
            )),
        }
    }

    /// The flying head at its resolved placement.
    #[inline]
    pub fn draw_flying_head(
        &mut self,
        ink: CanvasInk,
        frame: crate::word_decorations::KittyCursorFrame,
        placement: crate::word_decorations::KittyCursorPlacement,
    ) -> Option<u64> {
        let free = match ink {
            CanvasInk::Window => &mut self.free_scratch,
            CanvasInk::Pane => &mut self.pane_free,
        };
        self.decos.kitty_cursor_at_placement(frame, placement, free)
    }

    /// The typed dog's visit.
    #[inline]
    pub fn draw_dog(
        &mut self,
        ink: CanvasInk,
        frame: crate::word_decorations::DogCameoFrame,
    ) -> Option<u64> {
        let free = match ink {
            CanvasInk::Window => &mut self.free_scratch,
            CanvasInk::Pane => &mut self.pane_free,
        };
        self.decos.dog_cameo(frame, free)
    }

    /// Install the window's word channels.
    #[inline]
    pub fn publish_words(&mut self, input: &mut RenderInput) {
        input.word_decorations.clone_from(&self.deco_scratch);
        input.ink.clone_from(&self.ink_scratch);
        self.sprite_layers
            .publish(&self.free_scratch, &mut input.fx_layers);
        input.nova_add.clone_from(&self.nova_scratch);
        self.shade_word_icons(input);
        self.veil_pet_letters(input);
    }

    /// The page veil under the letters the pet lies beneath, by the body its
    /// tick drew this frame, into the published under stream
    /// ([`crate::cursor_glow::CursorGlow::veil_pet_letters`]): every host
    /// calls it after its pet's tick and the trail's publish.
    #[inline]
    pub fn veil_pet_letters(&mut self, input: &mut RenderInput) {
        #[cfg(test)]
        if self.pet_veil_stale {
            return;
        }
        let _ = self.cursor.glow.veil_pet_letters(
            (&mut input.glow_under, &mut input.glow_under_ramps),
            &self.free_scratch,
        );
    }

    /// The pet's level rules and senses before its tick ([`CompanionOwner::prepare`]), reading the
    /// word engine's ink.
    #[inline]
    pub fn pet_prepare(&mut self, facts: PetFacts<'_>) -> PetTick {
        self.companion.prepare(facts, &mut self.decos)
    }

    /// The resident's offer seam (`rainbow_kitty::companion`, D13).
    #[inline]
    pub fn offer_pet_v2(
        &mut self,
        pet_frame: &crate::kitty_pet::PetFrame,
        geom: Geom,
        pane_off: (u16, u16),
        now: Instant,
    ) {
        // The body the sky draws no star on: the box `PetOnGlass` reads, in
        // window pixels.
        let (on_glass, body) = if self.cursor.glow.v2_status().is_some() && pet_frame.alpha > 0 {
            let mut frame = *pet_frame;
            frame.row += f32::from(pane_off.0);
            frame.col += f32::from(pane_off.1);
            match crate::rainbow_kitty::companion::PetOnGlass::of_with_body_px(&frame, geom) {
                Some((on_glass, body)) => (Some(on_glass), Some(body)),
                None => (None, None),
            }
        } else {
            (None, None)
        };
        self.cursor.glow.note_pet_body(body);
        let offer = self.cursor.glow.pet_offer(geom, on_glass);
        if let Some(star) = self.companion.note_v2_offer(&offer) {
            self.cursor.glow.catch_star(star, now);
        }
    }

    /// The rain engine, when one is built.
    #[inline]
    #[must_use]
    pub const fn rain(&self) -> Option<&MatrixRain> {
        match &self.rain {
            Some(rain) => Some(&**rain),
            None => None,
        }
    }

    /// The rain engine, built with `cfg` unless one exists.
    #[inline]
    pub fn build_rain(&mut self, cfg: RainConfig) -> &mut MatrixRain {
        rain_engine(&mut self.rain, || cfg)
    }

    /// The rain engine, for the host's notes into it (a config reload, the agent-output and focus
    /// notes).
    #[inline]
    pub const fn rain_mut(&mut self) -> Option<&mut MatrixRain> {
        match &mut self.rain {
            Some(rain) => Some(&mut **rain),
            None => None,
        }
    }

    /// A layout re-grid: the rain keeps its engine (so opening a split does not kill the window's
    /// rain) and rebaselines only its coordinate-space state (occupancy bitset, material bank), so
    /// stale emission does not leak.
    #[inline]
    pub fn regrid_rain(&mut self) {
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_grid_replaced();
        }
        self.rain_scratch.clear();
        self.rain_add_scratch.clear();
    }

    /// The shell's Execute rising edge for `session` — noted every frame, with or without an
    /// engine.
    #[inline]
    pub const fn note_rain_shell(&mut self, session: u64, executing: bool) -> bool {
        self.rain_latches.note_shell_executing(session, executing)
    }

    /// The rain driver's frame on a native window.
    #[inline]
    pub fn rain_frame(
        &mut self,
        build: Option<impl FnOnce() -> RainConfig>,
        f: &RainFrame<'_>,
    ) -> u64 {
        let cmd_seq = f.cmd_done.map(|(seq, _)| seq);
        let Some(build) = build else {
            self.rain_latches.baseline_completion(f.session, cmd_seq);
            return 0;
        };
        if f.suspended && self.rain.is_none() {
            self.rain_latches.baseline_completion(f.session, cmd_seq);
            self.rain_scratch.clear();
            self.rain_add_scratch.clear();
            return 0;
        }
        rain_engine(&mut self.rain, build).host_frame(
            f,
            &mut self.rain_latches,
            &mut self.rain_scratch,
            &mut self.rain_add_scratch,
        )
    }

    /// Place a composed frame's pane-local rain into the window grid, clipped at the pane's
    /// interior edges.
    #[inline]
    pub fn place_rain(&mut self, place: PanePlace) {
        translate_rain_into_pane(
            &mut self.rain_scratch,
            &mut self.rain_add_scratch,
            place.row_off,
            place.col_off,
            place.rows,
            place.cols,
            place.cell_w,
            place.cell_h,
        );
    }

    /// The frame's rain channels are empty.
    #[inline]
    #[must_use]
    pub const fn rain_empty(&self) -> bool {
        self.rain_scratch.is_empty() && self.rain_add_scratch.is_empty()
    }

    /// Install the rain channels into a frame: the cells as fx layers under every other Linear
    /// layer ([`crate::matrix_rain::RainLayers`]), so a rain-free frame is byte-identical to no
    /// rain.
    #[inline]
    pub fn publish_rain(&mut self, input: &mut RenderInput) {
        match self.rain.as_deref() {
            Some(rain) => {
                self.rain_layers
                    .publish(&self.rain_scratch, rain.baker(), &mut input.fx_layers);
            }
            None => self.rain_layers.withdraw(&mut input.fx_layers),
        }
        input.rain_add.clone_from(&self.rain_add_scratch);
    }

    /// Take the rain out of a frame: its layers and its halos (a host that
    /// will not draw the rain this frame).
    #[inline]
    pub fn withdraw_rain(&mut self, input: &mut RenderInput) {
        self.rain_layers.withdraw(&mut input.fx_layers);
        input.rain_add.clear();
    }

    /// Publish `sprites` as the window's companion layers and `cells`, drawn from
    /// `tiles`, as its rain rows, through the window's own publishers: a test's way
    /// to stand a frame's effect layers up without running the effects.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn publish_for_test(
        &mut self,
        sprites: &[FreeSprite],
        (cells, tiles): (
            &[crate::matrix_rain::RainCell],
            &crate::matrix_rain::RainBaker,
        ),
        input: &mut RenderInput,
    ) {
        self.sprite_layers.publish(sprites, &mut input.fx_layers);
        self.rain_layers.publish(cells, tiles, &mut input.fx_layers);
    }

    /// Take the companions' sprite layers out of a frame (the word cats, the
    /// pet, the kitty cursor, the flying head and its notes, the dog, the
    /// nuke): a host that clears the window's effect channels.
    #[inline]
    pub fn withdraw_sprites(&mut self, input: &mut RenderInput) {
        self.sprite_layers.withdraw(&mut input.fx_layers);
    }

    /// The single grid's output-streak engine, when one is built.
    #[inline]
    #[must_use]
    pub const fn streak(&self) -> Option<&OutputStreak> {
        match &self.streak {
            Some(streak) => Some(&**streak),
            None => None,
        }
    }

    /// Prism wake on one grid, appended to the window's nova channel after the word pass (the
    /// decorations own its clear).
    #[inline]
    pub fn streak_grid(&mut self, s: Option<&StreakGrid<'_>>) -> Option<(StreakFrame, bool)> {
        let Some(s) = s else {
            self.streak = None;
            return None;
        };
        let engine = match &mut self.streak {
            Some(engine) => &mut **engine,
            None => self.streak.insert(Box::new(OutputStreak::new(s.seed))),
        };
        if let Some(boundary_at) = s.echo.boundary_at {
            engine.note_turn_boundary(boundary_at);
        }
        if let Some(accepted_at) = s.echo.accepted_at {
            engine.note_keystroke(accepted_at);
        }
        engine.note_output_cells(
            s.cells,
            s.cursor,
            s.viewport,
            s.content_seq,
            s.now,
            s.echo.input_hot,
        );
        let frame = engine.tick(s.now, s.geom, &s.cfg, &mut self.nova_scratch);
        Some((frame, engine.is_active()))
    }

    /// The composed frame's per-pane engines, keyed `(session, slot)`.
    #[inline]
    pub fn streak_panes(&self) -> impl Iterator<Item = &OutputStreak> {
        self.streak_panes.values()
    }

    /// Drop the engines of panes this frame no longer shows.
    #[inline]
    pub fn retain_streak_panes(&mut self, mut keep: impl FnMut(u64, usize) -> bool) {
        self.streak_panes
            .retain(|&(session, slot), _| keep(session, slot));
    }

    /// The inert policy on a composed frame: every pane engine goes.
    #[inline]
    pub fn clear_streak_panes(&mut self) {
        self.streak_panes.clear();
    }

    /// Drop one pane's engine (its snapshot is gone this frame).
    #[inline]
    pub fn drop_streak_pane(&mut self, session: u64, slot: usize) {
        self.streak_panes.remove(&(session, slot));
    }

    /// Prism wake on one pane of a composed frame: a history viewport or the alternate screen drops
    /// the pane's engine.
    #[inline]
    pub fn streak_pane(
        &mut self,
        pane: &StreakPane<'_>,
        k: StreakKnobs,
        into: NovaInto<'_>,
    ) -> (u64, Option<StreakCue>) {
        let snapshot = pane.snapshot;
        if snapshot.engine_alt || snapshot.display_offset != 0 {
            self.streak_panes.remove(&(pane.session, pane.slot));
            return (0, None);
        }
        let place = pane.place;
        let win_w = usize::from(place.cols) * place.cell_w as usize;
        let win_h = usize::from(place.rows) * place.cell_h as usize;
        let geom = Geom {
            cw: place.cell_w as usize,
            ch: place.cell_h as usize,
            rows: usize::from(place.rows),
            cols: usize::from(place.cols),
            origin_x: 0,
            origin_y: 0,
            win_w: if win_w < usize::from(u16::MAX) {
                win_w as u16
            } else {
                u16::MAX
            },
            win_h: if win_h < usize::from(u16::MAX) {
                win_h as u16
            } else {
                u16::MAX
            },
            head: 0,
        };
        let cfg = StreakConfig {
            enabled: true,
            intensity: k.intensity,
            tail: k.tail,
            max_streaks: k.max_streaks,
            idle_secs: k.idle_secs,
            dark_theme: aterm_render::theme_is_dark(snapshot.default_bg),
            theme_bg: snapshot.default_bg,
            theme_fg: snapshot.default_fg,
            theme_cursor: snapshot.cursor_color,
            sound: pane.sound,
        };
        let engine = self
            .streak_panes
            .entry((pane.session, pane.slot))
            .or_insert_with(|| {
                OutputStreak::new(crate::genome::mix(
                    pane.session ^ crate::genome::mix(pane.slot as u64) ^ 0x5052_4953_4D5F_5057,
                ))
            });
        if let Some(boundary_at) = pane.echo.boundary_at {
            engine.note_turn_boundary(boundary_at);
        }
        if let Some(accepted_at) = pane.echo.accepted_at {
            engine.note_keystroke(accepted_at);
        }
        engine.note_output_cells(
            &snapshot.cells,
            (snapshot.cursor_row, snapshot.cursor_col),
            (usize::from(place.rows), usize::from(place.cols)),
            pane.content_seq,
            k.now,
            pane.echo.input_hot,
        );
        self.pane_nova.clear();
        let frame = engine.tick(k.now, geom, &cfg, &mut self.pane_nova);
        translate_nova_into_pane(&mut self.pane_nova, place);
        match into {
            NovaInto::Window => self.nova_scratch.extend_from_slice(&self.pane_nova),
            NovaInto::Staged(out) => out.extend_from_slice(&self.pane_nova),
        }
        (frame.fp, if pane.sound { frame.cue } else { None })
    }
}

/// The cursor family's frame, the prelude every host runs around [`CursorFx::tick`].
#[inline]
pub(super) fn step_cursor_family(
    cursor: &mut CursorFx,
    step: &CursorStep<'_>,
    deliveries: impl IntoIterator<Item = Delivery>,
) -> CursorStepOut {
    let now = step.input.now;
    let live = step.input.live_viewport;
    cursor.begin(live, step.animate);
    let mut k = 0;
    while k < step.pending_keys {
        cursor.cadence.on_keystroke(now);
        cursor
            .momentum
            .on_key(now, crate::cursor_momentum::MOMENTUM_GLOW_TAU_S);
        if live {
            cursor.glow.note_typed_cells(now, 1);
        }
        k += 1;
    }
    if live && step.pending_keys > 0 {
        cursor.trail.note_typed_batch(now, step.pending_keys);
    }
    if let Some(probe) = step.row_probe {
        cursor
            .glow
            .observe_row_with_trust(probe.row, probe.caret, probe.cols, now, probe.trust);
        if let Some((above, below)) = probe.flanks {
            cursor.glow.observe_neighbor_rows(above, below);
        }
    }
    let rainbow = matches!(step.glow.style, crate::cursor_glow::GlowStyle::RainbowKitty);
    const INSERT_HINT_FRESH_DUR: std::time::Duration = std::time::Duration::from_nanos(
        (CursorGlow::INSERT_HINT_FRESH as f64 * 1_000_000_000.0).round() as u64,
    );
    for d in deliveries {
        if now.saturating_duration_since(d.at) > INSERT_HINT_FRESH_DUR {
            continue;
        }
        apply_delivery(cursor, rainbow, d);
    }
    cursor
        .glow
        .observe_print_anchor_glyph(step.print_anchor, step.print_anchor_glyph);
    cursor.glow.observe_caret_drawn(step.caret_drawn);
    if let Some(cells) = step.frame_cells {
        cursor
            .glow
            .observe_frame_text_at(step.input.cur, cells, step.frame_cells_at, now);
    }
    let frame = cursor.tick(&step.input, step.glow, step.trail);
    CursorStepOut {
        frame,
        motion_pulse: cursor.glow.take_cursor_cat_motion_pulse(),
    }
}

/// The rain engine's one construction: built on first need, never for an off
/// config.
#[inline]
fn rain_engine(
    rain: &mut Option<Box<MatrixRain>>,
    cfg: impl FnOnce() -> RainConfig,
) -> &mut MatrixRain {
    match rain {
        Some(engine) => engine,
        None => rain.insert(Box::new(MatrixRain::new(cfg()))),
    }
}

/// One receipt arms its licences at its delivery instant: the insert half
/// (Rainbow Kitty only) bounded at the dispatch; the key half re-stamps the
/// revoked licence as a pure stamp, and the classic trail takes the twins it
/// stamps at dispatch (typed stamp, generic move, nav).
#[inline]
fn apply_delivery(cursor: &mut CursorFx, rainbow: bool, d: Delivery) {
    if rainbow && let Some(width) = d.insert {
        cursor
            .glow
            .note_insert_delivered_from(d.dispatched_at, d.at, width);
    }
    if let Some(class) = d.key {
        cursor.glow.note_delivered(d.at, class);
        match class {
            DeliveredClass::Typed => cursor.trail.note_typed(d.at),
            DeliveredClass::Return | DeliveredClass::Gesture => {
                cursor.trail.note_move_delivered(d.at);
            }
            DeliveredClass::Nav => cursor.trail.note_motion_delivered(d.at),
            DeliveredClass::Newline | DeliveredClass::Erase | DeliveredClass::Kill { .. } => {}
        }
    }
}

#[cfg(test)]
mod flying_head_tests {
    use super::*;
    use crate::cat_baker::EyesFrame;
    use crate::cursor_glow::GlowStyle;
    use crate::kitty_cursor::CatFrame;
    use crate::kitty_registry::KittyLook;
    use crate::rainbow_kitty::CompanionImpulse;
    use crate::word_decorations::KittyCursorLayout;
    use std::time::Duration;

    const GEOM: EffectGeom = EffectGeom {
        cell_w: 8,
        cell_h: 16,
        rows: 24,
        cols: 120,
    };
    const FROM: (u16, u16) = (3, 0);
    const LANDING: (u16, u16) = (3, 40);

    const fn glow_geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 24,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 960,
            win_h: 384,
            head: 0,
        }
    }

    const fn rainbow_kitty() -> GlowConfig {
        GlowConfig {
            classic_mono: false,
            ribbon_tall: true,
            ribbon_flat: false,
            enabled: true,
            dark_theme: true,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
            style: GlowStyle::RainbowKitty,
            color: 0x0050_FA7B,
            accent: 0x007A_A2F7,
            duration: Duration::from_millis(260),
            length: 18,
            intensity: 1.0,
            audible: true,
            radius: 0.6,
            ring: true,
            beam: false,
            head_dx: 0.5,
        }
    }

    /// A window with the flying head on glass at `FROM` (a collection hello
    /// presents it), its placement follower at rest there, and v2 engaged;
    /// then a licensed jump to `to`, the meteor, observed at `t1`.
    fn fly(t0: Instant, t1: Instant, to: (u16, u16)) -> EffectsPipeline {
        let mut fx = EffectsPipeline::for_window();
        let cfg = rainbow_kitty();
        let mut out = Vec::new();
        fx.cursor_mut()
            .glow
            .tick(Some(FROM), t0, &cfg, glow_geom(), &mut out);
        let cat = &mut fx.companions_mut().cursor_cat;
        cat.on_collect(t0, KittyLook::for_app("claude"));
        cat.set_collection_presentable(t0, true);
        let frame = fx.companions_mut().cursor_cat.frame(t0);
        let placement_frame = fx.companions().cursor_cat.placement_frame(t0);
        fx.decos_mut()
            .resolve_kitty_cursor_placement(layout(FROM, &frame), placement_frame)
            .expect("the head is placed at its launch");
        fx.cursor_mut().glow.note_navigation(t1);
        fx.cursor_mut()
            .glow
            .tick(Some(to), t1, &cfg, glow_geom(), &mut out);
        fx
    }

    const fn layout(cursor: (u16, u16), frame: &CatFrame) -> KittyCursorLayout {
        KittyCursorLayout {
            geom: GEOM,
            cursor,
            look: frame.render_look(),
            bob: frame.bob,
            lift: 0.0,
        }
    }

    fn blank() -> Vec<Vec<RenderCell>> {
        vec![vec![RenderCell::default(); 120]; 24]
    }

    /// §7.2(a): on the teleport frame the placement snaps to the landing, the spine is floored
    /// at `disp ≥ 0.97` from a cooled head, the whip reads `lead == −0.30` at `t₀`, the eyes
    /// squint through the flight and 200 ms past arrival, and the squash lands on `t₀ + T`.
    #[test]
    fn the_flying_head_is_at_the_landing_on_frame_zero_and_squashes_on_arrival() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(1_500);
        let Some(CompanionImpulse::Meteor {
            t0: spawn,
            t_flight,
            ..
        }) = fly(t0, t1, LANDING)
            .cursor_mut()
            .glow
            .take_companion_impulse()
        else {
            panic!("a 40-cell licensed jump mints a meteor");
        };
        let mut fx = fly(t0, t1, LANDING);
        let mut frame = fx.companions_mut().cursor_cat.frame(t1);
        let before = frame.pose;
        let cooled = fx.companions().cursor_cat.spine();
        assert!(cooled < 0.97, "precondition: the spine cooled to {cooled}");
        fx.route_flying_head(
            CompanionDuty::FlyingHead { cell: LANDING },
            &blank(),
            t1,
            GEOM,
            &mut frame,
            true,
        );
        let rest = fx
            .decos()
            .kitty_cursor_footprint(layout(LANDING, &frame))
            .expect("a rest at the landing");
        let placement_frame = fx.companions().cursor_cat.placement_frame(t1);
        let placed = fx
            .decos_mut()
            .resolve_kitty_cursor_placement(layout(LANDING, &frame), placement_frame)
            .expect("the head is placed");
        assert_eq!(
            (placed.sample.x, placed.sample.y),
            (rest.x, rest.y),
            "frame zero is at the landing, not gliding from the launch"
        );
        let cat = &fx.companions().cursor_cat;
        assert!(
            cat.spine() >= 0.97,
            "spine floored: {} (was {cooled})",
            cat.spine()
        );
        assert_eq!(spawn, t1, "the meteor is minted on the echo tick");
        assert!(
            (frame.pose.lead - (-0.30)).abs() < 1e-4,
            "the whip at t0: lead {} (before the meteor: {})",
            frame.pose.lead,
            before.lead
        );
        assert_eq!(frame.pose.eyes, EyesFrame::Happy, "the squint at t0");
        assert_eq!(cat.land_at(), Some(spawn + t_flight));
        let land = spawn + t_flight;
        let early = fx
            .companions_mut()
            .cursor_cat
            .frame(land - Duration::from_millis(1));
        let late = fx
            .companions_mut()
            .cursor_cat
            .frame(land + Duration::from_millis(16));
        assert!(
            late.pose.scale_y < early.pose.scale_y - 0.1,
            "the squash lands on the arrival edge: {} then {}",
            early.pose.scale_y,
            late.pose.scale_y
        );
        assert_eq!(late.pose.eyes, EyesFrame::Happy, "the squint past landing");
        let after = fx
            .companions_mut()
            .cursor_cat
            .frame(land + Duration::from_millis(210));
        assert_ne!(
            after.pose.eyes,
            EyesFrame::Happy,
            "the squint ends 200 ms past the arrival edge (spine {})",
            fx.companions().cursor_cat.spine()
        );

        let mut still = fly(t0, t1, LANDING);
        let mut frame = still.companions_mut().cursor_cat.frame(t1);
        let unwhipped = frame.pose;
        still.route_flying_head(
            CompanionDuty::FlyingHead { cell: LANDING },
            &blank(),
            t1,
            GEOM,
            &mut frame,
            false,
        );
        assert_eq!(frame.pose, unwhipped, "no beat for a still head");
        assert!(frame.pose.lead > 0.0, "no whip: {}", frame.pose.lead);
        assert_ne!(
            still.companions().cursor_cat.land_at(),
            Some(land),
            "no squash booked"
        );

        let mut reduced = EffectsPipeline::for_window();
        let cfg = rainbow_kitty();
        let mut out = Vec::new();
        reduced.cursor_mut().glow.set_reduced_motion(true);
        reduced
            .cursor_mut()
            .glow
            .tick(Some(FROM), t0, &cfg, glow_geom(), &mut out);
        reduced
            .companions_mut()
            .cursor_cat
            .on_collect(t0, KittyLook::for_app("claude"));
        reduced
            .companions_mut()
            .cursor_cat
            .set_collection_presentable(t0, true);
        let launch_frame = reduced.companions_mut().cursor_cat.frame(t0);
        let launch_pf = reduced.companions().cursor_cat.placement_frame(t0);
        reduced
            .decos_mut()
            .resolve_kitty_cursor_placement(layout(FROM, &launch_frame), launch_pf)
            .expect("the head is placed at its launch under reduced motion");
        reduced.flying_sky_row = Some(FROM.0);
        reduced.cursor_mut().glow.note_navigation(t1);
        reduced
            .cursor_mut()
            .glow
            .tick(Some(LANDING), t1, &cfg, glow_geom(), &mut out);
        let mut land_frame = reduced.companions_mut().cursor_cat.frame(t1);
        reduced.route_flying_head(
            CompanionDuty::FlyingHead { cell: LANDING },
            &blank(),
            t1,
            GEOM,
            &mut land_frame,
            false,
        );
        assert_eq!(
            reduced.flying_sky_row, None,
            "reduced-motion Land impulse must clear sky hysteresis"
        );
        let land_pf = reduced.companions().cursor_cat.placement_frame(t1);
        let placed_reduced = reduced
            .decos_mut()
            .resolve_kitty_cursor_placement(layout(LANDING, &land_frame), land_pf)
            .expect("the head is placed at its landing under reduced motion");
        assert_eq!(
            (placed_reduced.sample.x, placed_reduced.sample.y),
            (rest.x, rest.y),
            "reduced-motion Land impulse must teleport to the landing on frame zero"
        );

        let down = (FROM.0 + 12, FROM.1);
        let Some(CompanionImpulse::Meteor { dir, .. }) =
            fly(t0, t1, down).cursor_mut().glow.take_companion_impulse()
        else {
            panic!("a 12-row licensed jump mints a meteor");
        };
        assert!(dir.is_vertical(), "precondition: {dir:?}");
        let mut vertical = fly(t0, t1, down);
        let mut frame = vertical.companions_mut().cursor_cat.frame(t1);
        let unmoved = frame.pose;
        vertical.route_flying_head(
            CompanionDuty::FlyingHead { cell: down },
            &blank(),
            t1,
            GEOM,
            &mut frame,
            true,
        );
        assert_eq!(frame.pose, unmoved, "no beat for a vertical flight");
        assert_eq!(vertical.companions().cursor_cat.land_at(), None);

        let mut sky_fx = fly(t0, t1, (5, 78));
        sky_fx.flying_sky_row = Some(5);
        let mut frame = sky_fx.companions_mut().cursor_cat.frame(t1);
        sky_fx.route_flying_head(
            CompanionDuty::FlyingHead { cell: (5, 20) },
            &blank(),
            t1,
            GEOM,
            &mut frame,
            true,
        );
        assert_eq!(
            sky_fx.flying_sky_row, None,
            "a horizontal meteor teleport must clear stale same-row sky hysteresis"
        );
        sky_fx.flying_sky_row = Some(5);
        sky_fx.front_replaced(t1);
        assert_eq!(sky_fx.flying_sky_row, None);

        let t_post_hold = t0 + Duration::from_millis(2_900);
        sky_fx
            .companions_mut()
            .cursor_cat
            .on_key(t_post_hold - Duration::from_millis(20), true);
        let mut delight_frame = sky_fx.companions_mut().cursor_cat.frame(t_post_hold);
        assert_eq!(
            delight_frame.reaction,
            crate::kitty_cursor::CatReaction::Cruise
        );
        sky_fx
            .cursor_mut()
            .glow
            .earn_hero(t_post_hold, FROM.0, FROM.1);
        sky_fx.route_flying_head(
            CompanionDuty::FlyingHead { cell: FROM },
            &blank(),
            t_post_hold,
            GEOM,
            &mut delight_frame,
            true,
        );
        assert_eq!(
            delight_frame.reaction,
            crate::kitty_cursor::CatReaction::Delight,
            "route_flying_head must refresh cat_frame.reaction on frame 0 of Delight"
        );
        assert_eq!(
            delight_frame.pose.eyes,
            EyesFrame::Happy,
            "route_flying_head must refresh cat_frame.pose.eyes on frame 0 of Delight"
        );
    }
}
