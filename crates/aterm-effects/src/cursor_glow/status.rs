// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the engine reports: the admission log, the block-fill verdict, the
//! `trail status` row, and the host's read-only accessors and drains.

use super::*;

/// Which spawn-seam verdict one [`AdmissionRecord`] captures: a move is either
/// licensed by a fresh key hint or declined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionPhase {
    /// [`CursorGlow::move_licensed`] said yes and the classifier ran.
    Licensed,
    /// The move was refused at the license seam; `reason` names which term
    /// failed.
    Declined,
}

impl AdmissionPhase {
    /// The wire token the `trail` control verb prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Licensed => "licensed",
            Self::Declined => "declined",
        }
    }
}

/// One entry of the admission diagnosis ring ([`CursorGlow::admission_log`]):
/// a bounded record of one spawn-seam verdict (licensed or declined) with the
/// declining reason and the move's endpoints.
///
/// Diagnostic state only: written beside the decision the engine already made
/// and never read by any admission, morphology or rendering path. The `trail`
/// control verb reads this ring, so a user report is one command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionRecord {
    /// Monotonic per-engine sequence (1-based); gaps mean overwritten entries.
    pub seq: u64,
    pub phase: AdmissionPhase,
    /// Why this move was declined: [`CursorGlow::DECLINE_NO_FRESH_HINT`] (no
    /// license term was fresh), [`CursorGlow::DECLINE_NO_CREDITS`] (a multi-cell
    /// coalesce outran the press credit budget),
    /// [`CursorGlow::DECLINE_OFF_SHAPE`] (licensed, but the classifier's shape
    /// gates laid nothing), or [`CursorGlow::DECLINE_PROGRAM_ROW`] (the
    /// anchored-echo lane refused a row branded as program output); or
    /// `"licensed"` on a licensed row.
    pub reason: &'static str,
    /// The move's clock.
    pub at: Instant,
    pub origin: (u16, u16),
    pub target: (u16, u16),
    /// Alternate-screen bit at the verdict.
    pub alternate_screen: bool,
    /// Which licence class admitted a licensed row: `"key"` for every
    /// press-hint class (typed, quench, nav, Return, newline, gesture),
    /// `"inflight"` for a move licensed by the in-flight pool alone (no
    /// stamp fresh — the stalled batch), `"insert"` for a delivered insert's
    /// sweep, `"rewrite"` for the program's retract of that span,
    /// `"recall"` for a typed lay the caret came to rest behind, and
    /// `"none"` on a decline — so the one measurement that matters after a
    /// drop, did it light, reads off `ctl trail`.
    pub licence: &'static str,
}

impl AdmissionRecord {
    /// Ring token: a licensed row admitted by a press-hint class.
    pub const LICENCE_KEY: &'static str = "key";
    /// Ring token: a licensed row admitted by the delivered-insert licence.
    pub const LICENCE_INSERT: &'static str = "insert";
    /// Ring token: a licensed row admitted by the in-flight pool alone, the
    /// unpaid presses paying for the echo's cells (a batch under the share rule,
    /// or one press for its own +1).
    pub const LICENCE_IN_FLIGHT: &'static str = "inflight";
    /// Ring token: a licensed row that was the program's rewrite of a
    /// delivered insert's span (the ribbon retracted to the new caret).
    pub const LICENCE_REWRITE: &'static str = "rewrite";
    /// Ring token: a typed lay recalled because the program then printed on
    /// another row and the caret came to rest on or left of the lay with no
    /// key behind the move (`CursorGlow`'s torn lay).
    pub const LICENCE_RECALL: &'static str = "recall";
    /// Ring token: a move the classic wake (Classic, either colour face) lit. That
    /// engine spawns on every observed cursor move with no key behind it, so
    /// its rows are never declined.
    pub const LICENCE_MOTION: &'static str = "motion";
    /// Ring token: a declined row admitted nothing.
    pub const LICENCE_NONE: &'static str = "none";

    /// One diagnostic line, the `trail` verb's row shape:
    /// `admission seq= phase= reason= age_ms= origin=r,c target=r,c alt=0|1
    /// licence=key|inflight|insert|rewrite|recall|motion|none`. `age_ms` is relative to `now`, so
    /// the reader sees "how long ago", not an unanchored instant. `licence=`
    /// is the LAST token: every existing reader greps the tokens before it.
    #[must_use]
    pub fn line(&self, now: Instant) -> String {
        format!(
            "admission seq={} phase={} reason={} age_ms={:.0} origin={},{} target={},{} alt={} licence={}",
            self.seq,
            self.phase.as_str(),
            self.reason,
            now.saturating_duration_since(self.at).as_secs_f64() * 1e3,
            self.origin.0,
            self.origin.1,
            self.target.0,
            self.target.1,
            u8::from(self.alternate_screen),
            self.licence,
        )
    }
}

/// Capacity of the admission diagnosis ring — small and fixed: the verb's
/// purpose is "what did the last few keystrokes decide", not history.
pub const ADMISSION_LOG_CAP: usize = 32;

/// The fixed-size drop-oldest ring behind [`CursorGlow::admission_log`],
/// resident in the engine (one slot write per observed move).
#[derive(Default)]
pub(super) struct AdmissionLog {
    pub(super) records: [Option<AdmissionRecord>; ADMISSION_LOG_CAP],
    /// Next slot to write (== oldest entry once the ring has wrapped).
    pub(super) head: usize,
    /// Total records ever pushed; stamps each record's `seq`.
    pub(super) seq: u64,
    /// Cumulative tally: fast typing overwrites the 32-slot ring quickly, so
    /// "was any move ever licensed on this window" needs a counter.
    pub(super) tally: AdmissionTally,
}

/// The cumulative license scoreboard behind [`CursorGlow::admission_tally`]:
/// the `trail status` headline numbers, which survive the ring wrapping.
///
/// They separate the two explanations of a dark ribbon: `licensed == 0` with
/// `declined > 0` means every observed move was program output (read
/// `last_decline_reason`); `licensed > 0` over a dark screen means the failure
/// is downstream (morphology, tuning, or the compositor).
///
/// Every observed move is counted once: `licensed + declined` is the number of
/// cursor deltas the spawn seam has judged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdmissionTally {
    /// Moves a fresh key hint LICENSED.
    pub licensed: u64,
    /// Moves the spawn seam DECLINED (any reason).
    pub declined: u64,
    /// The most recent decline reason token, or `None` while nothing has ever
    /// declined.
    pub last_decline_reason: Option<&'static str>,
}

impl AdmissionLog {
    pub(super) fn push(&mut self, mut record: AdmissionRecord) {
        self.seq += 1;
        record.seq = self.seq;
        match record.phase {
            AdmissionPhase::Licensed => self.tally.licensed += 1,
            AdmissionPhase::Declined => {
                self.tally.declined += 1;
                self.tally.last_decline_reason = Some(record.reason);
            }
        }
        if record.phase == AdmissionPhase::Declined
            && record.reason == CursorGlow::DECLINE_NO_FRESH_HINT
            && self.records[self.head].as_ref().is_some_and(|old| {
                old.phase == AdmissionPhase::Licensed
                    || old.reason != CursorGlow::DECLINE_NO_FRESH_HINT
            })
            && let Some(offset) = (1..ADMISSION_LOG_CAP).find(|&off| {
                let idx = (self.head + off) % ADMISSION_LOG_CAP;
                self.records[idx].as_ref().is_some_and(|r| {
                    r.phase == AdmissionPhase::Declined
                        && r.reason == CursorGlow::DECLINE_NO_FRESH_HINT
                })
            })
        {
            for step in offset..(ADMISSION_LOG_CAP - 1) {
                let dst = (self.head + step) % ADMISSION_LOG_CAP;
                let src = (self.head + step + 1) % ADMISSION_LOG_CAP;
                self.records[dst] = self.records[src];
            }
            let newest = (self.head + ADMISSION_LOG_CAP - 1) % ADMISSION_LOG_CAP;
            self.records[newest] = Some(record);
            return;
        }
        self.records[self.head] = Some(record);
        self.head = (self.head + 1) % ADMISSION_LOG_CAP;
    }

    /// Entries oldest → newest.
    pub(super) fn iter(&self) -> impl Iterator<Item = &AdmissionRecord> {
        self.records[self.head..]
            .iter()
            .chain(&self.records[..self.head])
            .filter_map(Option::as_ref)
    }
}

/// Which cursor-body effect owns the block cursor's fill: the one channel
/// through which a style replaces the caret's colour entirely.
///
/// A body effect's fill leaves its tick as
/// [`aterm_render::RenderInput::cursor_fill_override`], which `draw_cursor`
/// (and its GPU twin) applies instead of `frame_cursor(input)`. While an owner
/// is live the block cursor is whatever it painted, not the terminal's cursor
/// colour, and none of `glow_active`/`pet_active`/`cat_active` reports that.
/// This names the owner so a caret ignoring OSC 12 or `cursor_color` can be
/// traced from `trail status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockFillOwner {
    /// [`crate::cursor_rainbow`] — the `rainbow kitty` block (the SHIPPED
    /// DEFAULT style's body, so this is the owner most readings will name).
    Rainbow,
    /// The `fire` style's FORGE fill ([`CursorGlow::forge_fill`]).
    Forge,
    /// [`crate::cursor_phaser`] — the `phaser` style's emitter block.
    Phaser,
    /// The `laser` style's lightning BOLT fill (host-computed from the storm
    /// colour and the blaze).
    Bolt,
    /// [`crate::cursor_comet`] — the `comet` style's frosted nucleus.
    Comet,
    /// [`crate::cursor_droplet`] — the `water` style's liquid block.
    Droplet,
    /// [`crate::cursor_beam`] — the emitter block the styles with no bespoke
    /// body wear (`beam`, `lumen`, `sparkle`).
    BeamRod,
    /// The typing-momentum glow's warm tint (`crate::cursor_momentum`), lowest
    /// precedence: it paints only a caret no style body is painting.
    Momentum,
}

impl BlockFillOwner {
    /// The wire token for the `trail status` row.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rainbow => "rainbow",
            Self::Forge => "forge",
            Self::Phaser => "phaser",
            Self::Bolt => "bolt",
            Self::Comet => "comet",
            Self::Droplet => "droplet",
            Self::BeamRod => "beamrod",
            Self::Momentum => "momentum",
        }
    }

    /// WHERE this owner's body colour comes from — the field that answers
    /// "should OSC 12 be reaching my caret right now?" without a capture.
    #[must_use]
    pub const fn base_from(self) -> BlockFillBase {
        match self {
            // Both tint the host's resolved cursor colour. The momentum tint is
            // the cursor's own colour warmed: at rest the caret is the user's /
            // OSC-12 colour.
            Self::Rainbow | Self::Phaser | Self::Momentum => BlockFillBase::CursorColor,
            // Built from the resolved trail colour: an explicit
            // `cursor_trail_color`, else the one colour law
            // (`cursor_fx::automatic_glow_color`) — laser storm violet always,
            // beam/comet/sparkle their canonical hue until a cursor colour is
            // pinned, every other style the live cursor colour.
            Self::Comet | Self::Bolt | Self::BeamRod => BlockFillBase::TrailColor,
            // The style is its colour (a fire cursor is orange, a water cursor
            // aqua): no base at all, by design.
            Self::Forge | Self::Droplet => BlockFillBase::StyleIdentity,
        }
    }
}

/// The provenance of a [`BlockFillOwner`]'s body colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockFillBase {
    /// The terminal's resolved cursor colour (OSC 12, else the theme's
    /// `cursor_color`, else the live OSC 10 foreground), tinted by the effect.
    CursorColor,
    /// The resolved trail colour (`GlowConfig::color`): an explicit
    /// `cursor_trail_color`, else `cursor_fx::automatic_glow_color` — the
    /// style's canonical hue (laser always; beam, comet and sparkle until a
    /// cursor colour is pinned) or the live cursor colour.
    TrailColor,
    /// The style's own identity colour. The cursor colour DELIBERATELY does
    /// not reach the caret here.
    StyleIdentity,
    /// No `cursor_color` was pinned, so the owner built from the theme-polar
    /// white (`cursor_rainbow::BASE_DARK_THEME`), a white light the rainbow
    /// passes through. Distinct from [`Self::CursorColor`] so the row never
    /// reports a pinned-colour base beside a white pixel.
    White,
}

impl BlockFillBase {
    /// The wire token for the `trail status` row.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CursorColor => "cursor_color",
            Self::TrailColor => "trail_color",
            Self::StyleIdentity => "style_identity",
            Self::White => "white",
        }
    }
}

/// One reading of the block-cursor fill override: who owns it, the colour sent
/// to the renderer, and the colour that owner was told to build from.
///
/// `base` is what the host handed the owner; `fill` is what the owner painted.
/// A body ignoring its base shows as the two disagreeing (for example
/// `block_fill_base=ff0000` beside `block_fill_rgb=f8e1e1`, a near-white caret
/// under a red cursor colour) in one row, without a screen capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockFill {
    /// The effect that set `cursor_fill_override` this frame.
    pub owner: BlockFillOwner,
    /// The colour `0x00RRGGBB` that override carried — literally the caret's
    /// body on the glass, already held to the page floor
    /// (`cursor_fx::floor_block_fill`); the renderer's `minimum_contrast`
    /// floor, off by default, can still lift it.
    pub fill: u32,
    /// The base colour `0x00RRGGBB` the host handed this owner: the resolved
    /// cursor or trail colour, per [`BlockFillOwner::base_from`]. `None` for
    /// owners whose style is their colour ([`BlockFillBase::StyleIdentity`]).
    pub base: Option<u32>,
}

impl BlockFill {
    /// Where this fill's body colour actually came from, read off the resolved
    /// fill rather than the owner's declaration. [`BlockFillOwner::base_from`]
    /// says what an owner takes when handed a base, not whether one was handed:
    /// a `CursorColor` owner with no pinned `cursor_color` builds from the
    /// theme-polar white, and this reports that.
    #[must_use]
    pub const fn base_from(self) -> BlockFillBase {
        match (self.owner.base_from(), self.base) {
            (BlockFillBase::CursorColor, None) => BlockFillBase::White,
            (from, _) => from,
        }
    }
}

/// One reading of the cursor-trail / glow engine's live state, as the
/// `trail status` control verb prints it: what the style resolves to, every
/// gate between the config knob and the glass, the cumulative admission
/// scoreboard, and what light is alive now. Fields follow the order the frame
/// path walks them, so the first field that reads wrong names the break.
///
/// A pure record with a pure [`Self::line`]: the host fills it, nothing here
/// touches the clock or engine, and it is built only when asked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailStatus<'a> {
    /// The `cursor_trail_style` config string VERBATIM, before resolution —
    /// so a typo that silently fell back to the default is visible as itself.
    pub style_raw: &'a str,
    /// What that string resolved to: `None` when it draws no trail (`off`),
    /// printed `resolved=off`, never the placeholder enum a disabled
    /// `GlowConfig` carries. A spelling naming nothing drawable resolves to
    /// the default style it falls back to, and [`Self::unknown`] says so.
    pub style: Option<GlowStyle>,
    /// The spelling was not recognised, so the default style draws in its
    /// place: the row's `issue=unknown` (`issue=none` when it was honoured).
    pub unknown: bool,
    /// A trail is drawn: the configured style is not Off.
    pub config_enabled: bool,
    /// The resolved `GlowConfig::enabled` this frame — config AND serious mode.
    pub effective: bool,
    /// Whether the window is focused as the cursor-effect seam resolves it (the
    /// same fold the frame path feeds the motion policy), so a `video`
    /// recording's focus pin shows here, as does a live typed wake (a window
    /// driven through the control socket, or a handoff-adopted window that never
    /// saw an OS focus event).
    pub focused: bool,
    /// The resolved motion policy: `full` or `reduced`. Reduced ⇒ the aurora
    /// runs at amplitude 0 and NOTHING draws, whatever the rest of the row says.
    pub motion_stage: &'static str,
    /// The `motion` config mode (`auto` / `full` / `reduced`).
    pub motion_mode: &'static str,
    /// The soft load-shed envelope in 0..1 (1.0 = not shedding).
    pub shed: f32,
    /// The live effect intensity after every multiply and after the
    /// [`Self::effective`] enable gate: 0 exactly when the aurora is dark.
    /// `enabled = false` is the engine's sole gate (the teardown emits no
    /// geometry whatever the amplitude), so fillers must fold it:
    /// `effective == false` ⇒ `intensity == 0.0`.
    pub intensity: f32,
    /// Whether the key seam is open ([`CursorGlow::sound_seam_open`], read off
    /// the engine so this row and `cue_keystroke_shifted`'s gate cannot drift).
    /// `shed=0.00 intensity=0.00 sound_seam=true` is a shed or motion-reduced
    /// window whose typing is still heard; `sound_seam=false` with
    /// `focused=false` is the dark case that is meant to be silent.
    pub sound_seam: bool,
    /// The ribbon's dark-theme presentation as the raw spelling resolves it:
    /// `"underline"` (the explicit highlighter-plus-strip hybrid) or `"tall"`
    /// (the default smooth body, from ordinary rainbow spellings and the
    /// explicit `… tall` aliases); `"none"` when the resolved style is not
    /// rainbow kitty, which draws no ribbon.
    pub ribbon_look: &'static str,
    /// The cumulative admission scoreboard.
    pub tally: AdmissionTally,
    /// Moves the spawn seam admitted ([`CursorGlow::spawns`]).
    pub spawns: u64,
    /// Live TYPING sparks ([`CursorGlow::ribbon_segments`]).
    pub ribbon_segments: usize,
    /// Distinct hue bands among them ([`CursorGlow::ribbon_hue_bands`]).
    pub ribbon_hue_bands: usize,
    /// Ribbon boundaries whose own composite reads as band ink
    /// ([`CursorGlow::ribbon_drawn`]). [`Self::ribbon_segments`] is floored at
    /// the arc's dimmest stop, a bound on what may be claimed; this is what is
    /// there.
    pub ribbon_drawn: usize,
    /// Milliseconds left in a falling curtain ([`CursorGlow::curtain_left_ms`]),
    /// `None` when none is falling: why a drawn band is under the claim floor.
    pub ribbon_curtain_ms: Option<u32>,
    /// The one field (`docs/design/RAINBOW-KITTY-V2.md` §2.3, C2): the
    /// spectrum position the caret is about to lay
    /// ([`CursorGlow::rainbow_field`]), `0` red … `1` violet. Every layer of the
    /// mark reads it, so if ribbon, wake and caret disagree on screen this shows
    /// whether the field moved or a layer stopped reading it.
    pub field: f32,
    /// Live sparks of every kind ([`CursorGlow::live_sparks`]).
    pub sparks: usize,
    /// The canonical typing-momentum metric ([`CursorGlow::typing_momentum`]).
    pub momentum: f32,
    /// Its eased display spine ([`CursorGlow::momentum_display`]).
    pub momentum_display: f32,
    /// The typing-momentum glow's own value
    /// ([`crate::cursor_momentum::MomentumGlow::value`], 0..=1), distinct from
    /// the cat's `momentum` above; the warm caret (`block_fill=momentum`) is
    /// this engine's.
    pub momentum_glow: f32,
    /// [`CursorGlow::is_active`] — any live light at all.
    pub glow_active: bool,
    /// Whether the full-body pet companion is drawn and animating.
    pub pet_active: bool,
    /// The resident brain's current action, lowercase; `none` without a drawn body.
    pub pet_action: &'a str,
    /// Earned affection/contentment, independent of visibility, in 0..=1.
    pub pet_content: f32,
    /// Queued clicks/strokes waiting for the brain's existing consumption gate.
    pub pet_pending: u8,
    /// Located console attention, its cause, source event and command anchor.
    pub pet_focus: &'a str,
    pub pet_reason: &'a str,
    pub pet_anchor: Option<u64>,
    pub pet_event_seq: u64,
    pub pet_pose: &'a str,
    /// Last drawn resident body `(x0, x1, y0, y1)` in frame pixels, exclusive ends.
    pub pet_body: Option<(i32, i32, i32, i32)>,
    /// Whether the flying cursor cat is animating.
    pub cat_active: bool,
    /// Who owns the block cursor now, the colour they painted it, and the colour
    /// they were handed; `None` when the caret is the terminal's own cursor (so
    /// OSC 12 / `cursor_color` reach it directly).
    ///
    /// The `*_active` gates report the ribbon, the pet and the cat, not the one
    /// channel that replaces the caret's colour
    /// ([`aterm_render::RenderInput::cursor_fill_override`]). Reported from the
    /// last cursor-effect tick, the gates the last presented frame applied, like
    /// [`Self::focused`].
    pub block_fill: Option<BlockFill>,
    /// Flow: `flow=` / `combo=` / `combo_best=`, the engine's own counter as
    /// [`CursorGlow::flow_status`] reads it.
    ///
    /// `combo=` is keys typed at [`rk::spine::FLOW_KEY_DISP`] with no delete,
    /// `flow=` the open ramp over the last [`rk::spine::FLOW_OPEN_EASE_KEYS`]
    /// before [`rk::spine::FLOW_ENTRY_KEYS`], and `combo_best=` the window's
    /// high-water mark. An agent seeing `flow=1.00` knows the human is mid-flow.
    ///
    /// Zero on every style but rainbow kitty v2, and zero the instant the run
    /// breaks.
    pub flow: rk::Flow,
    /// Delivered-insert rows (`inserts_delivered=` / `inserts_lit=` /
    /// `inserts_retracted=` / `last_insert_cells=`): inserts the host reported
    /// delivered, those laid as one sweep, placeholder rewrites retracted, and
    /// the last insert's priced width ([`CursorGlow::insert_tally`]).
    pub inserts: InsertTally,
    /// In-flight rows (`inflight_licensed=` / `inflight_forgotten=` / `credits=`
    /// / `swallowed_no_echo=`): batches the in-flight pool alone licensed,
    /// forgets from an observed edge, the live pool, and presses the host never
    /// banked because the tty would not echo them
    /// ([`CursorGlow::in_flight_tally`]).
    pub in_flight: InFlightTally,
    /// The comet's opaque cell bed on the last tick
    /// (`crate::cursor_fx::CursorFx::trail_scratch`): how many cells it laid,
    /// and the brightest one's coverage byte (`0..=255`, after the shed
    /// envelope, the brightness dial and the bed's legibility share
    /// (`crate::cursor_fx::bed_legible_share`) — what the renderer blends at). The
    /// bed is no aurora light, so `glow_active=false` beside `comet_cells>0`
    /// is a bed still fading on the glass.
    pub comet_cells: usize,
    pub comet_alpha_max: u8,
}

impl TrailStatus<'_> {
    /// Whether a multi-hue rainbow is on glass: at least two ribbon cells in at
    /// least two distinct hues (one cell is a glow under the caret; one hue is a
    /// monochrome bar). The verb's headline verdict.
    #[must_use]
    pub fn ribbon_active(&self) -> bool {
        // A falling curtain is still a ribbon: `ribbon_segments` is floored at
        // the arc's dimmest stop, so late in a curtain every boundary is under
        // it while the band is still visible. Under a curtain the whole band is
        // on one envelope, so two boundaries that read as ink are enough, and
        // `ribbon_drawn` reaching 0 marks the end.
        (self.ribbon_segments >= 2 && self.ribbon_hue_bands >= 2)
            || (self.ribbon_curtain_ms.is_some() && self.ribbon_drawn >= 2)
    }

    /// The `resolved=` token: the resolved style's label (a fallback's is the
    /// default it draws; `issue=` says why), else `off`.
    #[must_use]
    pub fn resolved_label(&self) -> &'static str {
        self.style.map_or("off", GlowStyle::label)
    }

    /// The wire tail after `OK ` — one line, `key=value` pairs in a fixed
    /// order, the shape `rain status` established and `tone` followed.
    #[must_use]
    pub fn line(&self) -> String {
        // A disabled engine draws nothing, so it must not advertise light.
        debug_assert!(
            self.effective || self.intensity == 0.0,
            "a disabled trail reported intensity {}",
            self.intensity
        );
        format!(
            "trail style={:?} resolved={} config_enabled={} effective={} focused={} \
             motion={} motion_stage={} shed={:.2} intensity={:.2} sound_seam={} \
             licensed={} declined={} last_decline_reason={} spawns={} \
             ribbon_active={} ribbon_look={} ribbon_segments={} ribbon_hue_bands={} \
             ribbon_drawn={} ribbon_curtain_ms={} \
             field={:.3} sparks={} \
             momentum={:.2} momentum_display={:.2} momentum_glow={:.2} \
             flow={:.2} combo={} combo_best={} \
             glow_active={} pet_active={} cat_active={} \
             block_fill={} block_fill_rgb={} block_fill_base={} block_fill_base_from={} \
             pet_action={} pet_content={:.3} pet_pending={} pet_body={} \
             pet_focus={} pet_reason={} pet_anchor={} pet_event_seq={} pet_pose={} \
             inserts_delivered={} inserts_lit={} inserts_retracted={} last_insert_cells={} \
             inflight_licensed={} inflight_forgotten={} credits={} swallowed_no_echo={} \
             park_returns={} park_flushed={} \
             issue={} comet_cells={} comet_alpha_max={}",
            self.style_raw,
            self.resolved_label(),
            self.config_enabled,
            self.effective,
            self.focused,
            self.motion_mode,
            self.motion_stage,
            self.shed,
            self.intensity,
            self.sound_seam,
            self.tally.licensed,
            self.tally.declined,
            self.tally.last_decline_reason.unwrap_or("none"),
            self.spawns,
            self.ribbon_active(),
            self.ribbon_look,
            self.ribbon_segments,
            self.ribbon_hue_bands,
            self.ribbon_drawn,
            self.ribbon_curtain_ms
                .map_or_else(|| "none".to_string(), |ms| ms.to_string()),
            self.field,
            self.sparks,
            self.momentum,
            self.momentum_display,
            self.momentum_glow,
            // The hand's own row: the climb, the run and the window's best,
            // beside the momentum numbers.
            self.flow.heat,
            self.flow.combo,
            self.flow.best,
            self.glow_active,
            self.pet_active,
            self.cat_active,
            // `none` across the four when the caret belongs to the terminal, so
            // `block_fill=none` shows the cursor colour reaches the glass.
            self.block_fill.map_or("none", |b| b.owner.label()),
            self.block_fill
                .map_or_else(|| "none".to_string(), |b| format!("{:06x}", b.fill)),
            // A `none` base beside a named owner means a style-identity owner
            // with nothing to bloom from, which `block_fill_base_from` names.
            self.block_fill
                .and_then(|b| b.base)
                .map_or_else(|| "none".to_string(), |base| format!("{base:06x}")),
            // Provenance read off the resolved fill: a `CursorColor` owner with
            // no base built from white ([`BlockFill::base_from`]).
            self.block_fill.map_or("none", |b| b.base_from().label()),
            self.pet_action,
            self.pet_content,
            self.pet_pending,
            self.pet_body.map_or_else(
                || "none".to_string(),
                |(x0, x1, y0, y1)| format!("{x0},{x1},{y0},{y1}"),
            ),
            self.pet_focus,
            self.pet_reason,
            self.pet_anchor
                .map_or_else(|| "none".to_string(), |id| id.to_string()),
            self.pet_event_seq,
            self.pet_pose,
            self.inserts.delivered,
            self.inserts.lit,
            self.inserts.retracted,
            self.inserts.last_cells,
            self.in_flight.licensed,
            self.in_flight.forgotten,
            self.in_flight.credits,
            self.in_flight.swallowed_no_echo,
            self.in_flight.park_returns,
            self.in_flight.park_flushed,
            // Appended after the established prefix, so every existing reader
            // still parses it unchanged.
            if self.unknown { "unknown" } else { "none" },
            self.comet_cells,
            self.comet_alpha_max,
        )
    }

    /// [`Self::line`] plus rainbow kitty v2's rows
    /// (`docs/design/RAINBOW-KITTY-V2.md` §17.2), appended only while v2 owns
    /// the frame (`Some` from [`CursorGlow::v2_status`]); earlier tokens do not
    /// move.
    ///
    /// * `v2_quads=` `v2_halos=` `v2_stars=` `v2_meteors=`;
    /// * `v2_bridged=`: late echoes the engine relit on the next licensed echo
    ///   (the admission ring still scores the late move `declined
    ///   no-fresh-hint`);
    /// * `ribbon_retired=`: ribbon cells taken off by content
    ///   ([`rk::Status::retired`]: the glyph under them was replaced or went);
    /// * `ribbon_followed=`: cells the follow pass carried to another row with
    ///   their text ([`rk::Status::followed`]);
    /// * `ribbon_follow_missed=`: cells whose text arrived on a neighbouring row
    ///   that the follow pass did not carry ([`rk::Status::follow_missed`]); `0`
    ///   is healthy.
    #[must_use]
    pub fn line_v2(&self, v2: Option<rk::Status>) -> String {
        let mut line = self.line();
        if let Some(s) = v2 {
            use std::fmt::Write as _;
            // `String::write_fmt` is infallible; the `Result` is a trait
            // formality.
            let _ = write!(
                line,
                " v2_quads={} v2_halos={} v2_stars={} v2_meteors={} v2_bridged={} ribbon_retired={} ribbon_followed={} ribbon_follow_missed={}",
                s.quads,
                s.halos,
                s.stars,
                s.meteors,
                s.bridged,
                s.retired,
                s.followed,
                s.follow_missed
            );
        }
        line
    }
}

impl CursorGlow {
    /// V2's `trail status` counters, `Some` only while v2 owns the frame; hand
    /// it to [`TrailStatus::line_v2`].
    #[must_use]
    pub fn v2_status(&self) -> Option<rk::Status> {
        self.v2.engaged().then(|| self.v2.status())
    }

    /// Whether the key seam is open: would a keypress on this engine's window
    /// now record a click cue?
    ///
    /// `aterm ctl tone` and `aterm ctl trail status` both read this rather than
    /// re-deriving it from the config, so they cannot drift from each other or
    /// from the gate `cue_keystroke_shifted` tests first. Engine only: the host's
    /// sound gates (`trail_sounds`, `trail_sound_volume`, the resize-quiet
    /// window, a dead audio worker) are downstream and reported separately.
    #[must_use]
    pub const fn sound_seam_open(&self) -> bool {
        self.sound_live
    }

    /// The flow seam: `trail status`'s `flow=` / `combo=` / `combo_best=`
    /// ([`rk::Flow`]), which an agent holding its turn reads off the control
    /// socket. The engine's own counter read out, never a second one. Free: three
    /// fields off the spine. `Flow::default()` for every style but rainbow kitty
    /// v2.
    #[must_use]
    pub fn flow_status(&self) -> rk::Flow {
        if self.v2.engaged() {
            self.v2.spine().flow()
        } else {
            rk::Flow::default()
        }
    }

    /// Whether the last tick's row probe carried a readable capture of the rows
    /// above and below: `(above, below)`, each `true` only for
    /// [`NbrProbe::Probed`] (the state whose bytes the content witnesses read);
    /// `None` before any probe. Observability for lifecycle tests, not a
    /// licence.
    #[must_use]
    pub fn neighbor_rows_probed(&self) -> Option<(bool, bool)> {
        self.row_prev_meta
            .map(|m| (m.above == NbrProbe::Probed, m.below == NbrProbe::Probed))
    }

    /// The last admission-diagnosis records, oldest → newest (at most
    /// [`ADMISSION_LOG_CAP`]); the ring the `trail` verb serves.
    pub fn admission_log(&self) -> impl Iterator<Item = &AdmissionRecord> {
        self.admission_log.iter()
    }

    /// The CUMULATIVE admission scoreboard — what the 32-slot ring above has
    /// already forgotten. See [`AdmissionTally`]; served by `trail status`.
    #[must_use]
    pub fn admission_tally(&self) -> AdmissionTally {
        self.admission_log.tally
    }

    /// Moves [`Self::spawn`] has licensed over this engine's life — see
    /// [`Self::spawns`].
    #[must_use]
    pub fn spawns(&self) -> u64 {
        self.spawns
    }

    /// Row-band moves the host has replayed into this engine
    /// ([`Self::note_band_move`]) — the `band_moves=` column of `trail
    /// status`, one per band, for the engine's life.
    #[must_use]
    pub fn band_moves(&self) -> u64 {
        self.coord_band_moves
    }

    /// Ring reason: no license term was fresh — the move was program output
    /// nobody's fingers asked for.
    pub const DECLINE_NO_FRESH_HINT: &'static str = "no-fresh-hint";
    /// Ring reason: the move WAS licensed, but a multi-cell coalesce outran the
    /// press CREDIT budget, so the swept cells were not laid as typing.
    pub const DECLINE_NO_CREDITS: &'static str = "no-credits";
    /// Ring reason: the move WAS licensed and classified, but the style's shape
    /// gates minted no geometry from it.
    pub const DECLINE_OFF_SHAPE: &'static str = "off-shape";
    /// Ring reason: the anchored-echo lane refused a program row, one branded by
    /// an earlier keyless end-advance (see [`AnchorRow`]) or advancing while a
    /// different row's licensed echo held the stamp window
    /// ([`Self::last_anchor_sweep`]), so its advance consumed nothing. Also: a
    /// status row under a parked caret whose print run ends on a glyph no live
    /// press typed (zsh's i-search minibuffer and its fake cursor `_`), and a
    /// typed re-anchor of the visible caret whose one laid cell holds a glyph no
    /// live press typed (zle walking the caret along the search match row).
    /// Nothing is laid there, and the presses are forgotten.
    pub const DECLINE_PROGRAM_ROW: &'static str = "program-row";
    /// Ring reason: a hidden→visible relocation the ConPTY hide bridge refused
    /// while a licence term was still fresh, so the key's move never reached
    /// [`Self::spawn`]. Logged only under a fresh licence, so a hidden-cursor
    /// build log relocating its caret every frame does not spam the ring.
    pub const DECLINE_HIDDEN_RELOCATION: &'static str = "hidden-relocation";

    /// Record a LICENSED verdict in the diagnosis ring under the named
    /// licence class (`AdmissionRecord::LICENCE_*`).
    pub(super) fn log_licensed_as(
        &mut self,
        at: Instant,
        origin: (u16, u16),
        target: (u16, u16),
        licence: &'static str,
    ) {
        self.admission_log.push(AdmissionRecord {
            seq: 0,
            phase: AdmissionPhase::Licensed,
            reason: "licensed",
            at,
            origin,
            target,
            alternate_screen: self.ctx_alt,
            licence,
        });
    }

    /// Record a DECLINED verdict in the diagnosis ring.
    pub(super) fn log_decline(
        &mut self,
        at: Instant,
        origin: (u16, u16),
        target: (u16, u16),
        reason: &'static str,
    ) {
        self.admission_log.push(AdmissionRecord {
            seq: 0,
            phase: AdmissionPhase::Declined,
            reason,
            at,
            origin,
            target,
            alternate_screen: self.ctx_alt,
            licence: AdmissionRecord::LICENCE_NONE,
        });
    }

    /// How many live typing sparks the ribbon is made of: the cells a
    /// `rainbow kitty` band spans now, reported as `ribbon_segments`. Jump/comet
    /// sparks are excluded (a leap's streak, not the typed band). `0` while
    /// nothing was typed recently.
    #[must_use]
    pub fn ribbon_segments(&self) -> usize {
        // While v2 owns the frame the ribbon is its cells.
        if self.v2.engaged() {
            return self.v2.ribbon_segments();
        }
        self.sparks.iter().filter(|spark| spark.typing).count()
    }

    /// What is on the glass, beside [`Self::ribbon_segments`]'s claim: ribbon
    /// boundaries whose own composite reads as band ink
    /// (`rk::ribbon::reads_as_ink`), reported as `ribbon_drawn=`.
    /// `ribbon_segments` is floored at `rk::ribbon::STATUS_LIT_COV` (that census
    /// floor solved for the arc's dimmest stop), a bound on the claim. `0` for
    /// every style but rainbow kitty.
    #[must_use]
    pub fn ribbon_drawn(&self) -> usize {
        if self.v2.engaged() {
            return self.v2.ribbon_drawn();
        }
        0
    }

    /// Milliseconds left in a falling curtain (`rk::ribbon::CURTAIN_S`), or `None`:
    /// `trail status`'s `ribbon_curtain_ms=`.
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        if self.v2.engaged() {
            return self.v2.curtain_left_ms(now);
        }
        None
    }

    /// Distinct twentieth-of-the-arc bands in the visible classic ribbon, derived
    /// from the same geometry as rendering.
    #[must_use]
    pub fn ribbon_hue_bands(&self) -> usize {
        if self.v2.engaged() {
            return self.v2.ribbon_hue_bands();
        }
        0
    }

    /// Total live sparks (ribbon + jump/comet cells, and the classic wake's
    /// swept cells), so `trail status` can tell "no light at all" from "light,
    /// but not the typed band". The two engines never both hold sparks: any
    /// tick of another style resets the classic one, and the classic branch
    /// settles this one before it draws.
    #[must_use]
    pub fn live_sparks(&self) -> usize {
        self.sparks.len() + self.classic.live_sparks()
    }

    /// The eased momentum spine the ribbon's width, wave and brightness read
    /// (`rainbow.disp`), the display twin of the raw [`Self::typing_momentum`].
    /// Both are reported because they differ during attack and release.
    #[must_use]
    pub fn momentum_display(&self) -> f32 {
        // While v2 owns the frame the eased spine is v2's; no other style
        // displays momentum.
        if self.v2.engaged() {
            return self.v2.momentum_display();
        }
        0.0
    }

    /// The text lines the host's row probe shows this frame: the cursor's row
    /// and each probed row flanking it ([`Self::observe_neighbor_rows`]), as
    /// the column span from the row's first glyph to one past its last. Reads
    /// the prev probe slot (this frame's presented truth at emit time); a row
    /// the host did not capture, or a blank one, has no span. The particle law
    /// keys on it ([`TextSpans::covers`]).
    pub(super) fn probed_text_spans(&self) -> TextSpans {
        let mut spans = TextSpans::default();
        let Some(m) = self.row_prev_meta.as_ref() else {
            return spans;
        };
        let row = i32::from(m.row);
        let rows: [(i32, Option<&[char]>); 3] = [
            (
                row - 1,
                (m.above == NbrProbe::Probed).then_some(&self.row_above_prev[..]),
            ),
            (row, Some(&self.row_prev[..])),
            (
                row + 1,
                (m.below == NbrProbe::Probed).then_some(&self.row_below_prev[..]),
            ),
        ];
        for ((slot, read), (r, cols)) in spans.rows.iter_mut().zip(&mut spans.read).zip(rows) {
            let Some(cols) = cols else { continue };
            *read = Some(r);
            let first = cols.iter().position(|&g| g != ' ');
            let last = cols.iter().rposition(|&g| g != ' ');
            if let (Some(first), Some(last)) = (first, last) {
                *slot = Some((r, first as i32, last as i32 + 1));
            }
        }
        spans
    }

    /// This frame's radial light (see `halo_out`), for the host to splice
    /// into `RenderInput.glow_halo` right beside the `GlowQuad` scratch.
    pub fn halos(&self) -> &[RainHalo] {
        &self.halo_out
    }

    /// This frame's under-ink flame body (see `under_out`), for
    /// `RenderInput.glow_under`.
    pub fn under_quads(&self) -> &[GlowQuad] {
        &self.under_out
    }

    /// The under-ink stream the caret's bodies draw their light-ground ink
    /// into after this frame's tick (`CursorFx::tick`: the comet's coma, the
    /// light rod, the typing glow), so every host's `glow_under` carries them
    /// with no host change. Each body bounds its own few quads; their
    /// fingerprints move the frame's.
    pub(crate) fn under_sink(&mut self) -> &mut Vec<GlowQuad> {
        &mut self.under_out
    }

    /// This frame's per-pixel fire (see `patch_out`), for
    /// `RenderInput.fire_patch`.
    pub fn patches(&self) -> &[FirePatch] {
        &self.patch_out
    }

    /// This frame's charred-ink overrides (see `char_out`), for
    /// `RenderInput.char_fg`.
    pub fn charred(&self) -> &[CharFg] {
        &self.char_out
    }

    /// The rolling spectrum hue in turns `0..1`, the hue the next beam segment
    /// will be laid in (phaser advances it 2× per move). Read after
    /// [`Self::tick`] and hand it to [`crate::cursor_phaser::CursorPhaser`] so
    /// the emitter cursor and the streak are one piece of light.
    #[must_use]
    pub fn beam_hue(&self) -> f32 {
        self.hue
    }

    /// The rainbow kitty family's shared phase-ring clock (`0..1024`), sampled
    /// after [`Self::tick`]. Hand it to
    /// [`crate::cursor_rainbow::CursorRainbow::tick_with_family_phase`] so the
    /// caret, its halo and the ribbon under that cell resolve one spectrum
    /// position. Authoritative only while this engine runs
    /// [`GlowStyle::RainbowKitty`].
    #[must_use]
    pub fn rainbow_phase(&self) -> f32 {
        // While v2 owns the frame the shared phase ring is v2's spine clock; at
        // rest the ring is at its origin.
        if self.v2.engaged() {
            return self.v2.phase();
        }
        0.0
    }

    /// The caret's position in the classic field. When its cell owns typing
    /// light, this resolves that exact cell; otherwise a live ribbon supplies
    /// its geometric head. With no live typing light, the next mark starts at
    /// red. No phase latch or forecast is needed because the visible field is
    /// derived wholly from current mark geometry.
    pub fn rainbow_field(&self) -> f32 {
        // While v2 owns the frame the caret's field position is v2's seated stop
        // (`v2_stop`), held across a cell death under a parked caret, falling
        // back to the engine's live field before the first seat. Without v2
        // there is no field: the arc's origin.
        if self.v2.engaged() {
            return self.v2_stop.map_or_else(|| self.v2.field(), |(t, _)| t);
        }
        0.0
    }

    /// The colour of a seated caret stop (`v2_stop`), spelled as
    /// `rk::ribbon::Ribbon::head_rgb` spells the live one, through the same
    /// `rk::ribbon::caret_ink`: the authored stop at the reflected walk on
    /// dark, that stop's caret ink on the page on light. The two agree to the
    /// byte on every seat frame
    /// (`the_caret_settles_on_its_own_stop_without_a_pop`).
    pub(super) fn v2_caret_stop_rgb(t: f32, cfg: &rk::Config) -> u32 {
        rk::ribbon::caret_ink(crate::spectrum::spectrum(rk::ribbon::walk_arc(t)), cfg)
    }

    /// Source RGB selected by the typing ribbon HEAD's BODY, before additive
    /// coverage premultiplication. This is the colour a companion cursor should
    /// inherit to meet the laid ribbon without a palette seam.
    ///
    /// All presentations and both theme arms resolve [`Self::rainbow_field`]
    /// once. Dark themes return the authored spectrum colour; light themes the
    /// caret's ink on the page at that same field position. A non-rainbow config,
    /// or an engine that has not observed a cursor yet, returns `None`.
    #[must_use]
    pub fn rainbow_head_rgb(&self, cfg: &GlowConfig) -> Option<u32> {
        // While v2 owns the frame the head colour is read off the seated stop,
        // so a cell death under a parked caret cannot move it.
        // `Config::from_glow` is a plain field copy.
        if self.v2.engaged() {
            let v2_cfg = rk::Config::from_glow(cfg, self.reduced_motion);
            return match self.v2_stop {
                Some((t, _)) => Some(Self::v2_caret_stop_rgb(t, &v2_cfg)),
                None => self.v2.head_rgb(&v2_cfg),
            };
        }
        if !matches!(cfg.style, GlowStyle::RainbowKitty) {
            return None;
        }
        // A rainbow kitty tick v2 does not own (trail dark, nothing presented
        // yet): the caret keeps the arc's origin stop.
        self.last?;
        let field = self.rainbow_field();
        if !cfg.dark_theme {
            return Some(rk::ribbon::paper_caret_ink(
                rainbow_thing_of(field),
                cfg.theme_bg,
            ));
        }
        Some(rainbow_thing_of(field))
    }

    /// This frame's fire contrast-halo strengths (see `fire_halo_out`), for
    /// `RenderInput.fire_halo`.
    pub fn halo_cells(&self) -> &[FireHaloCell] {
        &self.fire_halo_out
    }

    // ---- O(1) stream handoff -------------------------------------------
    //
    // The `swap_*` methods hand the host this frame's stream by moving the
    // buffer rather than copying it (a hot ribbon at retina metrics carries
    // 6k-14k `GlowQuad`s of 16 B; see `benches/cursor_bench.rs`). [`Self::tick`]
    // clears every scratch at entry, so the buffer handed back (holding the
    // previous frame's content) is never read. A host reading a stream twice in
    // one frame must use the borrowing accessor; swap exactly once, at the
    // splice.

    /// Move this frame's radial light into `dst` (see [`Self::halos`]).
    ///
    /// `dst`'s previous contents come back in exchange and are cleared by the
    /// next [`Self::tick`]; never read them off the engine.
    pub fn swap_halos(&mut self, dst: &mut Vec<RainHalo>) {
        std::mem::swap(dst, &mut self.halo_out);
    }

    /// Move this frame's under-ink flame body into `dst` (see
    /// [`Self::under_quads`]). Same previous-frame caveat as [`Self::swap_halos`].
    pub fn swap_under_quads(&mut self, dst: &mut Vec<GlowQuad>) {
        std::mem::swap(dst, &mut self.under_out);
    }

    /// Move this frame's per-pixel fire into `dst` (see [`Self::patches`]).
    /// Same previous-frame caveat as [`Self::swap_halos`].
    pub fn swap_patches(&mut self, dst: &mut Vec<FirePatch>) {
        std::mem::swap(dst, &mut self.patch_out);
    }

    /// Move this frame's charred-ink overrides into `dst` (see
    /// [`Self::charred`]). Same previous-frame caveat as [`Self::swap_halos`].
    pub fn swap_charred(&mut self, dst: &mut Vec<CharFg>) {
        std::mem::swap(dst, &mut self.char_out);
    }

    /// Move this frame's fire contrast-halo strengths into `dst` (see
    /// [`Self::halo_cells`]). Same previous-frame caveat as [`Self::swap_halos`].
    pub fn swap_halo_cells(&mut self, dst: &mut Vec<FireHaloCell>) {
        std::mem::swap(dst, &mut self.fire_halo_out);
    }
}

/// The probed text lines of one frame ([`CursorGlow::probed_text_spans`]): up
/// to three rows, each `(row, first column, one past the last)`, and which
/// rows the probe read at all (blank or not).
#[derive(Clone, Copy, Default)]
pub(crate) struct TextSpans {
    rows: [Option<(i32, i32, i32)>; 3],
    read: [Option<i32>; 3],
}

impl TextSpans {
    /// Whether grid cell `(row, col)` lies on a probed line of text: a
    /// glyph, or a blank between two of a line's glyphs (a mark in the gap
    /// between words reads as punctuation, a hyphen or a middle dot, as
    /// surely as one over a letter reads as noise).
    pub(super) fn covers_cell(&self, row: i32, col: i32) -> bool {
        self.rows
            .iter()
            .flatten()
            .any(|&(r, first, end)| r == row && (first..end).contains(&col))
    }

    /// Each probed line of text as `(row, first, end)`: the column span from
    /// its first glyph to one past its last.
    pub(super) fn lines(&self) -> impl Iterator<Item = (i32, i32, i32)> + '_ {
        self.rows.iter().flatten().copied()
    }

    /// Whether the probe read grid row `row` this frame, so a cell of it off
    /// every line ([`Self::covers_cell`]) is known blank rather than unknown.
    pub(super) fn reads_row(&self, row: i32) -> bool {
        self.read.contains(&Some(row))
    }
}

/// The text of the rows the frame presents where the streak may run, read
/// from the frame's own cells ([`CursorGlow::observe_frame_text`]): rows
/// `lo..lo + rows.len()`, each the column span from its first glyph to one
/// past its last (`None` for a blank row), for the frame stamped `at`.
#[derive(Default)]
pub(crate) struct FrameText {
    pub(super) at: Option<Instant>,
    pub(super) lo: i32,
    pub(super) rows: Vec<Option<(i32, i32)>>,
    /// Each read row's cells' ink and ground (`0x00RRGGBB`; no ink for a
    /// blank cell; `None` for a column the row does not carry), `cols` a
    /// row, row-major as `rows`: what a letter there is drawn in and on.
    pub(super) pairs: Vec<Option<(Option<u32>, u32)>>,
    pub(super) cols: usize,
}

impl FrameText {
    /// The ink (`None` for a blank cell) and ground of cell `(row, col)` in
    /// the frame's own cells, or `None` for a cell the frame did not read.
    pub(crate) fn pair(&self, row: i32, col: i32) -> Option<(Option<u32>, u32)> {
        let i = row - self.lo;
        if i < 0 || i as usize >= self.rows.len() || col < 0 || col as usize >= self.cols {
            return None;
        }
        self.pairs
            .get(i as usize * self.cols + col as usize)
            .copied()
            .flatten()
    }
}

/// What a frame shows of text where the streak runs: the row probe's lines,
/// and for a row the probe did not read, that row's text in the frame's own
/// cells ([`FrameText`]). A row neither one read is unknown.
#[derive(Clone, Copy)]
pub(crate) struct TextView<'a> {
    pub(crate) spans: TextSpans,
    frame: &'a [Option<(i32, i32)>],
    lo: i32,
}

impl<'a> TextView<'a> {
    /// The probe's lines and the frame's rows `frame` (`None` for a frame
    /// that handed none).
    pub(crate) fn new(spans: TextSpans, frame: Option<&'a FrameText>) -> Self {
        match frame {
            Some(f) => Self {
                spans,
                frame: &f.rows,
                lo: f.lo,
            },
            None => Self {
                spans,
                frame: &[],
                lo: 0,
            },
        }
    }

    /// The frame's own read of `row`: `Some(span)` for a row it read
    /// (`span` `None` when blank), `None` for a row it did not.
    fn frame_row(&self, row: i32) -> Option<Option<(i32, i32)>> {
        let i = row - self.lo;
        (i >= 0)
            .then(|| self.frame.get(i as usize).copied())
            .flatten()
    }

    /// Whether the frame says what row `row` holds (the probe or the cells).
    pub(crate) fn knows_row(&self, row: i32) -> bool {
        self.spans.reads_row(row) || self.frame_row(row).is_some()
    }

    /// The text line of a row the probe did not read but the frame's cells
    /// did: `Some(Some((first, end)))` for a row with glyphs, `Some(None)`
    /// for a blank one, `None` where neither read it.
    pub(crate) fn frame_line(&self, row: i32) -> Option<Option<(i32, i32)>> {
        if self.spans.reads_row(row) {
            return None;
        }
        self.frame_row(row)
    }

    /// Whether grid cell `(row, col)` lies on a line of text the frame
    /// shows (a glyph, or a blank between two of a line's glyphs).
    pub(crate) fn covers_cell(&self, row: i32, col: i32) -> bool {
        if self.spans.reads_row(row) {
            return self.spans.covers_cell(row, col);
        }
        self.frame_row(row)
            .flatten()
            .is_some_and(|(first, end)| (first..end).contains(&col))
    }

    /// Whether the frame shows grid row `row` blank: read (by the probe or
    /// the frame's own cells) and holding no glyph. A row nobody read is
    /// not known blank.
    pub(crate) fn row_is_blank(&self, row: i32) -> bool {
        if self.spans.reads_row(row) {
            !self.spans.lines().any(|line| line.0 == row)
        } else {
            self.frame_row(row) == Some(None)
        }
    }

    /// Whether the dark streak treats cell `(row, col)` as text: a cell on
    /// a line of text, or one on a row nobody read (so the full beam lands
    /// only where the frame showed air).
    pub(crate) fn over_text(&self, row: i32, col: i32) -> bool {
        !self.knows_row(row) || self.covers_cell(row, col)
    }
}
