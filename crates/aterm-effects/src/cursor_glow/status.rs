// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE ENGINE REPORTS — the admission log, the block-fill verdict, the
//! `trail status` row and the host's read-only accessors and drains.

use super::*;

/// Which spawn-seam verdict one [`AdmissionRecord`] captures.
///
/// TWO PHASES, because the license asks ONE question. The proof era's five
/// (`armed`/`confirmed`/`retired`/`tombstone`/`extended`) described a candidate
/// LIFECYCLE that no longer exists; a move is now simply licensed by a fresh
/// key hint or it is not.
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

/// One entry of the ADMISSION DIAGNOSIS RING ([`CursorGlow::admission_log`]):
/// a bounded record of one spawn-seam verdict — licensed / declined — with the
/// declining reason and the move's endpoints.
///
/// This is DIAGNOSTIC STATE, not admission state: the ring is written beside
/// the decision the engine already made and is never read back by any
/// admission, morphology, or rendering path. It exists because the rainbow-
/// trail blackout was diagnosed with `ATERM_TRACE_SPAWN` stderr archaeology;
/// the `trail` control verb reads this ring so a user report is ONE COMMAND.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionRecord {
    /// Monotonic per-engine sequence (1-based); gaps mean overwritten entries.
    pub seq: u64,
    pub phase: AdmissionPhase,
    /// Why this move was declined — one of
    /// [`CursorGlow::DECLINE_NO_FRESH_HINT`] (no license term was fresh),
    /// [`CursorGlow::DECLINE_NO_CREDITS`] (a multi-cell coalesce outran the
    /// press CREDIT budget), [`CursorGlow::DECLINE_OFF_SHAPE`] (licensed,
    /// but the classifier's shape gates laid nothing), or
    /// [`CursorGlow::DECLINE_PROGRAM_ROW`] (the anchored-echo lane refused a
    /// row branded as program output) — or `"licensed"` on a licensed row.
    pub reason: &'static str,
    /// The move's clock.
    pub at: Instant,
    pub origin: (u16, u16),
    pub target: (u16, u16),
    /// Alternate-screen bit at the verdict.
    pub alternate_screen: bool,
    /// WHICH licence class admitted a licensed row: `"key"` for every
    /// press-hint class (typed, quench, nav, Return, newline, gesture),
    /// `"inflight"` for a move licensed by the in-flight pool alone (no
    /// stamp fresh — the stalled batch), `"insert"` for a delivered insert's
    /// sweep, `"rewrite"` for the program's retract of that span, and
    /// `"none"` on a decline — so the one measurement that matters after a
    /// drop, did it light, reads off `ctl trail`.
    pub licence: &'static str,
}

impl AdmissionRecord {
    /// Ring token: a licensed row admitted by a press-hint class.
    pub const LICENCE_KEY: &'static str = "key";
    /// Ring token: a licensed row admitted by the delivered-insert licence.
    pub const LICENCE_INSERT: &'static str = "insert";
    /// Ring token: a licensed row admitted by the IN-FLIGHT pool alone — no
    /// stamp fresh, the unpaid presses paying for the echo's cells (a batch
    /// under the share rule, or one press for its own +1).
    pub const LICENCE_IN_FLIGHT: &'static str = "inflight";
    /// Ring token: a licensed row that was the program's rewrite of a
    /// delivered insert's span (the ribbon retracted to the new caret).
    pub const LICENCE_REWRITE: &'static str = "rewrite";
    /// Ring token: a declined row admitted nothing.
    pub const LICENCE_NONE: &'static str = "none";

    /// One diagnostic line, the `trail` verb's row shape:
    /// `admission seq= phase= reason= age_ms= origin=r,c target=r,c alt=0|1
    /// licence=key|inflight|insert|rewrite|none`. `age_ms` is relative to `now`, so
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

/// The fixed-size drop-oldest ring behind [`CursorGlow::admission_log`].
/// Resident in the engine (no allocation on the hot path — one slot write per
/// observed move).
#[derive(Default)]
pub(super) struct AdmissionLog {
    pub(super) records: [Option<AdmissionRecord>; ADMISSION_LOG_CAP],
    /// Next slot to write (== oldest entry once the ring has wrapped).
    pub(super) head: usize,
    /// Total records ever pushed; stamps each record's `seq`.
    pub(super) seq: u64,
    /// CUMULATIVE tally, which the 32-slot ring alone cannot answer: a burst
    /// of fast typing overwrites its own history long before anyone reads it,
    /// so "did ANY move ever get licensed on this window" would otherwise be
    /// unanswerable a second later.
    pub(super) tally: AdmissionTally,
}

/// The CUMULATIVE license scoreboard behind [`CursorGlow::admission_tally`]
/// — the `trail status` verb's headline numbers, and the one reading that
/// survives the ring wrapping.
///
/// A dark ribbon has exactly two honest explanations and these numbers separate
/// them: `licensed == 0` with `declined > 0` means every observed move was
/// program output nobody's fingers asked for (read `last_decline_reason`),
/// while `licensed > 0` over a dark screen means the license is fine and the
/// failure is downstream — morphology, tuning, or the compositor.
///
/// EVERY OBSERVED MOVE IS COUNTED EXACTLY ONCE: `licensed + declined` is the
/// number of cursor deltas the spawn seam has judged.
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

/// WHICH cursor-body effect owns the BLOCK CURSOR's fill — the one channel
/// through which a style can take the caret away from the terminal entirely.
///
/// THE SENSOR GAP THIS CLOSES. A body effect's fill leaves its tick as
/// [`aterm_render::RenderInput::cursor_fill_override`], and `draw_cursor` (with
/// its GPU twin) applies that override INSTEAD of `frame_cursor(input)`, by
/// design. So while one of these owners is live, the block cursor is NOT the
/// terminal's cursor colour — it is whatever the owner painted. Twice now
/// (`d602f8cd` the rainbow, then the phaser one style over) a body that bloomed
/// from a hard-coded base ate OSC 12 and the configured `cursor_color`, and
/// BOTH times the misdiagnosis was the same: `trail status` reported
/// `glow_active`/`pet_active`/`cat_active` — every one of them quiet, because
/// none of them is the gate that was open — while the style owning the caret
/// was live the whole time. This names the owner so the next person can ask
/// instead of guess.
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
    /// body wear (`beam`, `lumen`, `sparkle`, trail packs).
    BeamRod,
    /// The typing-momentum glow's warm tint (`crate::cursor_momentum`) —
    /// LOWEST precedence: every style body above it owns the caret when it
    /// is lit; the tint paints only a caret nobody else is painting.
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
            // Both take the host's resolved cursor colour as the base their
            // spectrum/beam hue tints. Both had to be TAUGHT to (the two
            // fixes this sensor exists because of).
            // The momentum tint is the cursor's own colour, warmed: at rest
            // the caret IS the user's / OSC-12 colour.
            Self::Rainbow | Self::Phaser | Self::Momentum => BlockFillBase::CursorColor,
            // Built from the resolved TRAIL colour, which itself follows the
            // cursor colour unless the user pinned `cursor_trail_color` — or,
            // for the laser, at all: lightning is electric yellow ON PURPOSE
            // and is exempt from the OSC-12 rewire.
            Self::Comet | Self::Bolt | Self::BeamRod => BlockFillBase::TrailColor,
            // The style IS its colour: a fire cursor is orange because it is
            // fire, a water cursor is aqua because it is water. Neither takes
            // a base at all — deliberate, not a leak.
            Self::Forge | Self::Droplet => BlockFillBase::StyleIdentity,
        }
    }
}

/// The provenance of a [`BlockFillOwner`]'s body colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockFillBase {
    /// The terminal's resolved cursor colour (OSC 12, else the configured
    /// theme `cursor_color`, else the live OSC 10 foreground). A caret under
    /// this owner IS the user's cursor colour, tinted by the effect.
    CursorColor,
    /// The resolved trail colour (`GlowConfig::color`) — the cursor colour
    /// too, unless an explicit `cursor_trail_color` (or the laser's exemption)
    /// replaced it.
    TrailColor,
    /// The style's own identity colour. The cursor colour DELIBERATELY does
    /// not reach the caret here.
    StyleIdentity,
    /// THE CARET IS A WHITE LIGHT THE RAINBOW PASSES THROUGH. The user pinned
    /// no `cursor_color`, so the owner was handed no base and built from the
    /// theme-polar white (`cursor_rainbow::BASE_DARK_THEME`). Distinct from
    /// [`Self::CursorColor`] on purpose: that arm is the sensor's whole
    /// reason to exist — "is the pinned colour reaching the caret?" — and
    /// reporting it for an unpinned caret made the row say `base=50fa7b`
    /// beside a white pixel, which is the sensor lying about the one thing
    /// it measures.
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

/// ONE reading of the block-cursor fill override: who owns it, what colour
/// went to the renderer, and what colour that owner was TOLD to build from.
///
/// THE TWO COLOURS ARE THE POINT. `base` is what the host handed the owner;
/// `fill` is what the owner actually painted. A body that ignores its base —
/// the exact defect fixed twice now — shows up as the two disagreeing
/// wildly: `block_fill_base=ff0000` beside `block_fill_rgb=f8e1e1` is a
/// near-white caret under a red cursor colour, printed in one row, with no
/// screen capture required to see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockFill {
    /// The effect that set `cursor_fill_override` this frame.
    pub owner: BlockFillOwner,
    /// The colour `0x00RRGGBB` that override carried — literally the caret's
    /// body on the glass (the renderer floors it for glyph contrast).
    pub fill: u32,
    /// The base colour `0x00RRGGBB` the HOST handed this owner to build from:
    /// the resolved cursor colour or the resolved trail colour, per
    /// [`BlockFillOwner::base_from`]. `None` for the owners that take no base
    /// because the style IS its colour ([`BlockFillBase::StyleIdentity`]).
    pub base: Option<u32>,
}

impl BlockFill {
    /// WHERE this fill's body colour ACTUALLY came from — read off the
    /// resolved fill, not off the owner's declaration.
    ///
    /// [`BlockFillOwner::base_from`] says what an owner TAKES when handed a
    /// base. It cannot say whether one was handed. A `CursorColor` owner
    /// whose user pinned no `cursor_color` is handed `None` and builds from
    /// the theme-polar white, and the sensor's whole job is to report that
    /// difference: printing `cursor_color` there put the theme's green in the
    /// row beside a white pixel, which is the sensor lying about the one thing
    /// it measures. Every other owner's provenance is the owner's own.
    #[must_use]
    pub const fn base_from(self) -> BlockFillBase {
        match (self.owner.base_from(), self.base) {
            (BlockFillBase::CursorColor, None) => BlockFillBase::White,
            (from, _) => from,
        }
    }
}

/// ONE READING of the cursor-trail / glow engine's live truth, as the
/// `trail status` control verb prints it: what the style resolves to, every
/// gate between the config knob and the glass, the cumulative admission
/// scoreboard, and what light is actually alive right now.
///
/// WHY IT EXISTS. aterm could introspect the matrix rain (`rain status`) and
/// the tone of typing (`tone status`) but not the cursor trail, so the standing
/// owner report — *"I don't see the rainbow cursor trails"* — could only be
/// investigated by recording video and squinting at hue histograms. Every
/// field here is one link in the chain a dark ribbon could break at, in the
/// order the frame path walks them, so the FIRST field that reads wrong names
/// the bug.
///
/// A pure record with a pure [`Self::line`]: the host fills it, nothing here
/// touches the clock or the engine, and it is built ONLY when asked (the
/// `rain status` cheapness rule — an unasked status costs nothing at all).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailStatus<'a> {
    /// The `cursor_trail_style` config string VERBATIM, before resolution —
    /// so a typo that silently fell back to the default is visible as itself.
    pub style_raw: &'a str,
    /// What that string resolved to.
    pub style: GlowStyle,
    /// `cursor_trail` master knob.
    pub config_enabled: bool,
    /// The resolved `GlowConfig::enabled` this frame — config AND serious mode.
    pub effective: bool,
    /// Whether the window the reading came from is focused AS THE CURSOR-EFFECT
    /// SEAM RESOLVES IT — the same fold the frame path feeds the motion policy,
    /// so an in-flight `video` recording's focus pin shows here, and so does a
    /// live typed wake (a window being driven through the control socket, or a
    /// handoff-adopted window that never saw an OS focus event, is focused for
    /// the purpose of the trail it just earned).
    pub focused: bool,
    /// The resolved motion policy: `full` or `reduced`. Reduced ⇒ the aurora
    /// runs at amplitude 0 and NOTHING draws, whatever the rest of the row says.
    pub motion_stage: &'static str,
    /// The `motion` config mode (`auto` / `full` / `reduced`).
    pub motion_mode: &'static str,
    /// The soft load-shed envelope in 0..1 (1.0 = not shedding).
    pub shed: f32,
    /// The live effect intensity after every multiply AND after the [`Self::effective`]
    /// enable gate — the one number that is 0 exactly when the aurora is provably
    /// dark. The enable gate belongs INSIDE it: `enabled = false` is the engine's
    /// sole gate (the `!cfg.enabled` teardown emits no geometry whatever the
    /// amplitude says), so a row that multiplied only the policy and the shed
    /// envelope reported a bright `intensity` for frames that were never drawn.
    /// Fillers must fold it; `effective == false` ⇒ `intensity == 0.0`.
    pub intensity: f32,
    /// Whether the KEY SEAM is open — [`CursorGlow::sound_seam_open`], read
    /// off the engine rather than re-derived from the config, so this row and
    /// the gate `cue_keystroke_shifted` tests cannot drift.
    ///
    /// It sits beside `shed` and `intensity` on purpose: `shed=0.00
    /// intensity=0.00 sound_seam=true` is a shed or motion-reduced window
    /// whose typing is STILL HEARD (a key-time click costs no GPU), and
    /// `sound_seam=false` beside `focused=false` is the one dark case that is
    /// supposed to be silent.
    pub sound_seam: bool,
    /// The ribbon's dark-theme PRESENTATION as the raw spelling resolves it:
    /// `"underline"` (the explicit post-v0.43 highlighter-plus-strip hybrid) or
    /// `"tall"` (the default smooth v0.43-shaped body, selected by ordinary
    /// rainbow spellings and the explicit `… tall` aliases).
    pub ribbon_look: &'static str,
    /// The cumulative admission scoreboard.
    pub tally: AdmissionTally,
    /// Moves the spawn seam admitted ([`CursorGlow::spawns`]).
    pub spawns: u64,
    /// Live TYPING sparks ([`CursorGlow::ribbon_segments`]).
    pub ribbon_segments: usize,
    /// Distinct hue bands among them ([`CursorGlow::ribbon_hue_bands`]).
    pub ribbon_hue_bands: usize,
    /// **THE FACT BESIDE THE CLAIM** ([`CursorGlow::ribbon_drawn`]): ribbon
    /// boundaries whose own composite reads as band ink on the glass.
    /// [`Self::ribbon_segments`] is floored at the arc's DIMMEST stop and is
    /// therefore a bound on what may be claimed; this is what is there.
    pub ribbon_drawn: usize,
    /// Milliseconds left in a falling curtain
    /// ([`CursorGlow::curtain_left_ms`]), `None` when none is falling — why a
    /// band that is drawn is under the claim floor.
    pub ribbon_curtain_ms: Option<u32>,
    /// **THE ONE FIELD** (`docs/design/RAINBOW-TRAIL-ONE-STORY.md` §2.1) — the
    /// spectrum position the caret is ABOUT TO LAY
    /// ([`CursorGlow::rainbow_field`]), `0` red … `1` violet.
    ///
    /// It is on the row because the field is the one thing every layer of the
    /// mark now reads, so it is the one number that says which rainbow is being
    /// drawn. A reader who sees the ribbon, the wake and the caret disagreeing
    /// on screen can check here whether the field moved or a layer stopped
    /// reading it.
    pub field: f32,
    /// Live sparks of every kind ([`CursorGlow::live_sparks`]).
    pub sparks: usize,
    /// The canonical typing-momentum metric ([`CursorGlow::typing_momentum`]).
    pub momentum: f32,
    /// Its eased display spine ([`CursorGlow::momentum_display`]).
    pub momentum_display: f32,
    /// The TYPING-MOMENTUM GLOW's own value ([`crate::cursor_momentum::MomentumGlow::value`],
    /// 0..=1) — not the cat's `momentum` above. Added after a capture read
    /// `block_fill=momentum` beside `momentum=0.00` and could not explain
    /// itself: `momentum=` is CursorGlow's cat momentum, and the warm caret
    /// is this engine's.
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
    /// WHO OWNS THE BLOCK CURSOR right now, the colour they painted it, and the
    /// colour they were handed to build from — `None` when nobody does (the
    /// caret is the terminal's own cursor, so OSC 12 / `cursor_color` reach it
    /// directly).
    ///
    /// The field the three `*_active` gates above could not stand in for. Those
    /// report the ribbon, the pet and the cat; NONE of them reports the one
    /// channel that replaces the caret's colour outright
    /// ([`aterm_render::RenderInput::cursor_fill_override`]). A style can own
    /// the caret — and paint it a colour the terminal never asked for — while
    /// every gate this row printed reads quiet, which is exactly how the same
    /// defect was misdiagnosed twice (an audit, then a maintainer) before
    /// `d602f8cd` found it in the rainbow block and the phaser turned out to
    /// have it too.
    ///
    /// Reported from the LAST cursor-effect tick — the gates the last presented
    /// frame actually applied — not a parallel re-derivation, for the same
    /// reason [`Self::focused`] is the frame path's own fold.
    pub block_fill: Option<BlockFill>,
    /// **FLOW** — `flow=` / `combo=` / `combo_best=`, the engine's own
    /// counter as [`CursorGlow::flow_status`] reads it.
    ///
    /// The one field on this row that is about the PERSON rather than the
    /// pixels: `combo=` is keys typed at [`rk::spine::FLOW_KEY_DISP`] with no
    /// delete, `flow=` is the open ramp over the last
    /// [`rk::spine::FLOW_OPEN_EASE_KEYS`] before
    /// [`rk::spine::FLOW_ENTRY_KEYS`], and `combo_best=` is the window's
    /// high-water mark. An agent reading the socket sees `flow=1.00` and
    /// knows the human is mid-flow — the reason the counter is on the wire at
    /// all.
    ///
    /// Zero on every style but Rainbow Kitty v2 (the spine that prices it is
    /// v2's), and zero the instant the run breaks.
    pub flow: rk::Flow,
    /// THE DELIVERED-INSERT ROWS — `inserts_delivered=` /
    /// `inserts_lit=` / `inserts_retracted=` / `last_insert_cells=`: how
    /// many inserts the host reported delivered, how many the seam laid as
    /// one sweep, how many placeholder rewrites it retracted, and the last
    /// insert's priced width ([`CursorGlow::insert_tally`]). The reading
    /// that says whether a drop reached the engine at all.
    pub inserts: InsertTally,
    /// THE IN-FLIGHT ROWS — `inflight_licensed=` /
    /// `inflight_forgotten=` / `credits=` / `swallowed_no_echo=`: batches the
    /// in-flight pool alone licensed, forgets an observed edge made, the live
    /// pool, and the presses the host never banked because the tty would not
    /// echo them ([`CursorGlow::in_flight_tally`]).
    pub in_flight: InFlightTally,
}

impl TrailStatus<'_> {
    /// Whether a multi-hue rainbow is on glass right now: at least two ribbon
    /// cells in at least two distinct hues. One cell is a glow under the
    /// caret; four cells all one hue is a monochrome bar. Two bands is the
    /// weakest thing a human would still call a rainbow trail, so this is the
    /// verb's headline verdict and the bar any fix has to clear.
    #[must_use]
    pub fn ribbon_active(&self) -> bool {
        // A FALLING CURTAIN IS STILL A RIBBON (2026-09-14). The claim clause
        // reads `ribbon_segments`, which is floored at the arc's dimmest
        // stop; from about +99 ms of every 0.24 s curtain every boundary is
        // under that floor while the band is still plainly on the glass —
        // measured at 1065 quads, alpha 102 down to 9 — and the verb answered
        // `ribbon_active=false` for the last half of every fall. Under a
        // curtain the whole band is on ONE envelope, so hue diversity is not
        // in question: two boundaries that READ AS INK are the rainbow
        // leaving, and `ribbon_drawn` reaching 0 at the end of the fall is
        // the honest edge.
        (self.ribbon_segments >= 2 && self.ribbon_hue_bands >= 2)
            || (self.ribbon_curtain_ms.is_some() && self.ribbon_drawn >= 2)
    }

    /// The wire tail after `OK ` — one line, `key=value` pairs in a fixed
    /// order, the shape `rain status` established and `tone` followed.
    #[must_use]
    pub fn line(&self) -> String {
        // The sensor's one arithmetic contract, guarded where the row is built
        // rather than trusted at each fill site: a disabled engine draws
        // nothing, so it must not advertise light.
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
             park_returns={} park_flushed={}",
            self.style_raw,
            self.style.label(),
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
            // THE HAND'S OWN ROW: the climb, the run, and the best the
            // window has held. Beside the two momentum numbers because they
            // are the same story — what the spine is doing, and what the
            // person did to it.
            self.flow.heat,
            self.flow.combo,
            self.flow.best,
            self.glow_active,
            self.pet_active,
            self.cat_active,
            // `none` across the four when the caret belongs to the terminal: a
            // reader who sees `block_fill=none` knows the cursor colour is
            // reaching the glass without a capture to prove it.
            self.block_fill.map_or("none", |b| b.owner.label()),
            self.block_fill
                .map_or_else(|| "none".to_string(), |b| format!("{:06x}", b.fill)),
            // A base of `none` beside a named owner is not a hole in the row —
            // it is the style-identity owners saying they were handed nothing
            // to bloom from, which `block_fill_base_from` then names.
            self.block_fill
                .and_then(|b| b.base)
                .map_or_else(|| "none".to_string(), |base| format!("{base:06x}")),
            // …and the provenance is read off the RESOLVED fill, not the
            // owner's declaration: a `CursorColor` owner handed no base built
            // from white, and the row must say so ([`BlockFill::base_from`]).
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
        )
    }

    /// [`Self::line`] plus Rainbow Kitty v2's rows (`RAINBOW-KITTY-V2.md`
    /// §17.2, seam point 12) — `v2_quads=` `v2_halos=` `v2_stars=`
    /// `v2_meteors=` `v2_bridged=` `ribbon_retired=` — appended ONLY while
    /// v2 owns the frame (`Some` from [`CursorGlow::v2_status`]). Additive
    /// rows at the tail; nothing in front of them moves, so every existing
    /// reader of [`Self::line`] parses this unchanged. `v2_bridged=` is the
    /// honest signal for a repaired late echo: the admission
    /// ring keeps scoring the late move `declined no-fresh-hint`, because
    /// the gate is untouched, while the engine's ledger relights its cell
    /// on the next licensed echo and counts it here. `ribbon_retired=` is
    /// the window's cumulative count of ribbon cells taken off by CONTENT
    /// ([`rk::Status::retired`]: the witness saw the glyph under them
    /// replaced, or go) — the number that says a
    /// band went out because its text moved or went, as against expiring.
    /// `ribbon_followed=` (2026-09-21) is its twin: the cells the follow
    /// pass carried to another row WITH their text ([`rk::Status::followed`]
    /// — a bottom-anchored composer growing a row without a scroll), so
    /// "the band went with its text" reads apart from "its text moved out
    /// from under it". `ribbon_follow_missed=` (2026-09-23) rides last: the
    /// cells whose text ARRIVED on a neighbouring row that the follow pass
    /// still did not carry ([`rk::Status::follow_missed`]) — each one left
    /// to the content witness where it stood instead of going with its
    /// text, the owner's *"not simply abruptly vanish"*; `0` is the healthy
    /// reading.
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
    /// **SEAM POINT 12** (§17.2): v2's `trail status` counters — `Some` only
    /// while v2 owns the frame; hand it to [`TrailStatus::line_v2`].
    #[must_use]
    pub fn v2_status(&self) -> Option<rk::Status> {
        self.v2.engaged().then(|| self.v2.status())
    }

    /// Whether the KEY SEAM is open: would a keypress landing on this
    /// engine's window right now record a click cue?
    ///
    /// The first reader of `sound_live` outside the engine, minted because
    /// `aterm ctl tone` must be able to refute "sound is broken" on its own
    /// and `aterm ctl trail status` must be able to say "the light is off AND
    /// the keys are heard" in one row. Both read THIS value rather than
    /// re-deriving it from the config, so the two verbs cannot drift from
    /// each other or from the gate `cue_keystroke_shifted` actually tests on
    /// its first line.
    ///
    /// It answers for the ENGINE only: the host's own sound gates
    /// (`trail_sounds`, `trail_sound_volume`, the resize-quiet window, a dead
    /// audio worker) sit downstream and are reported separately.
    #[must_use]
    pub const fn sound_seam_open(&self) -> bool {
        self.sound_live
    }

    /// **SEAM POINT 12's SIBLING — THE FLOW SEAM.** `trail status`'s
    /// `flow=` / `combo=` / `combo_best=` ([`rk::Flow`]), and the number an
    /// agent holding its turn reads off the control socket.
    ///
    /// The engine's OWN counter, read out — never a second one kept here
    /// (the flow law is one counter, exactly as the momentum law is one
    /// metric). Pure and free: three fields off the spine, no clock, no
    /// engine walk, so it costs nothing unasked like every other reading on
    /// that row. `Flow::default()` — `flow=0.00 combo=0 combo_best=0` — for
    /// every style but Rainbow Kitty v2, whose spine is the one that prices
    /// it.
    #[must_use]
    pub fn flow_status(&self) -> rk::Flow {
        if self.v2.engaged() {
            self.v2.spine().flow()
        } else {
            rk::Flow::default()
        }
    }

    /// Whether the row probe the last tick consumed carried a readable
    /// capture of its row above and its row below — `(above, below)`, each
    /// `true` only for [`NbrProbe::Probed`], the one state whose bytes the
    /// seam's content witnesses read; `None` before any probe. This is
    /// observability for the host's lifecycle tests, not a licence.
    #[must_use]
    pub fn neighbor_rows_probed(&self) -> Option<(bool, bool)> {
        self.row_prev_meta
            .map(|m| (m.above == NbrProbe::Probed, m.below == NbrProbe::Probed))
    }

    /// The last admission-diagnosis records, OLDEST → NEWEST (at most
    /// [`ADMISSION_LOG_CAP`]). The read face of the ring the `trail` control
    /// verb serves; see [`AdmissionRecord`] for what one entry carries.
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
    /// Ring reason: the anchored-echo lane refused a PROGRAM row — one branded
    /// by an earlier keyless end-advance (see [`AnchorRow`]), or one advancing
    /// while a different row's licensed echo held the stamp window
    /// ([`Self::last_anchor_sweep`]) — so its advance, though inside a typed
    /// stamp's freshness window, consumed nothing. Also a status row under a
    /// parked caret whose print run ends on a glyph no live press typed
    /// (zsh's i-search minibuffer and its fake cursor `_`), and a typed
    /// re-anchor of the visible caret whose one laid cell — its landing, or
    /// a soft-wrapped caret's origin; a lifted word lays none and is never
    /// refused — holds a glyph no live press typed (zle walking the caret
    /// along the search's match row): nothing is laid there, and the
    /// presses are forgotten.
    pub const DECLINE_PROGRAM_ROW: &'static str = "program-row";
    /// Ring reason: a hidden→visible relocation the bounded ConPTY hide-bridge
    /// REFUSED while a licence term was still fresh — a key was pressed and
    /// the move it produced never reached [`Self::spawn`] at all, the one
    /// drop path `aterm ctl trail` could otherwise not see (no licensed
    /// row, no declined row, nothing). Only a REFUSAL UNDER A
    /// FRESH LICENCE is logged: a hidden-cursor build log relocating its caret
    /// every frame must not spam the ring at frame rate.
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

    /// How many live TYPING sparks the ribbon is currently made of — the
    /// number of cells a `rainbow kitty` band spans right now, and the field
    /// `trail status` reports as `ribbon_segments`. Jump/comet sparks are
    /// excluded: they are a leap's streak, not the typed band the config docs
    /// promise. `0` while nothing was typed recently.
    #[must_use]
    pub fn ribbon_segments(&self) -> usize {
        // §17.2 (D14): while v2 owns the frame the ribbon is ITS cells — a
        // status row that counted v1's sparks read `ribbon_active=false` over
        // a lit v2 ribbon, and the paint-conformance bind refused the glass.
        if self.v2.engaged() {
            return self.v2.ribbon_segments();
        }
        self.sparks.iter().filter(|spark| spark.typing).count()
    }

    /// **WHAT IS ON THE GLASS**, beside [`Self::ribbon_segments`]'s CLAIM:
    /// the ribbon boundaries whose own composite reads as band ink
    /// (`rk::ribbon::reads_as_ink`), the field `trail status` reports as
    /// `ribbon_drawn=`. `ribbon_segments` is floored at
    /// `rk::ribbon::STATUS_LIT_COV`, which is that same census floor solved
    /// once for the arc's DIMMEST stop — a bound on what may be claimed, not
    /// a fact about the band, and reported as one it said the band was gone
    /// for the last half of every curtain while a thousand quads were still
    /// compositing. `0` for every style but rainbow kitty.
    #[must_use]
    pub fn ribbon_drawn(&self) -> usize {
        if self.v2.engaged() {
            return self.v2.ribbon_drawn();
        }
        0
    }

    /// Milliseconds left in a falling curtain (`rk::ribbon::CURTAIN_S`), or
    /// `None` when none is — `trail status`'s `ribbon_curtain_ms=`, so a row
    /// whose claim is under the floor says WHY.
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        if self.v2.engaged() {
            return self.v2.curtain_left_ms(now);
        }
        None
    }

    /// Distinct twentieth-of-the-arc bands in the visible classic ribbon.
    /// This is derived from the same geometry as rendering, so `trail status`
    /// cannot report diversity that exists only in the retired phase lattice.
    #[must_use]
    pub fn ribbon_hue_bands(&self) -> usize {
        if self.v2.engaged() {
            return self.v2.ribbon_hue_bands();
        }
        0
    }

    /// Total live sparks (ribbon + jump/comet cells) — the whole per-cell
    /// light population, so `trail status` can say whether a dark ribbon is
    /// "no light at all" or "light, but not the typed band".
    #[must_use]
    pub fn live_sparks(&self) -> usize {
        self.sparks.len()
    }

    /// The EASED momentum spine the ribbon's width / wave / brightness all
    /// read (`rainbow.disp`) — the display twin of [`Self::typing_momentum`],
    /// which is the raw metric. Reported beside it because they differ during
    /// the attack and the long release, and a "why is the ribbon thin" answer
    /// needs to name which one is low.
    #[must_use]
    pub fn momentum_display(&self) -> f32 {
        // SEAM POINT 6 (§17.2, D14): the eased spine is v2's while it owns
        // the frame — one integrator, one follower (§19.1). Nothing else
        // displays momentum: the other nine styles have no ribbon to widen.
        if self.v2.engaged() {
            return self.v2.momentum_display();
        }
        0.0
    }

    /// Does the window-pixel `(x, y)` sit over a probed GLYPH cell of the
    /// cursor's OWN row? Reads the PREV probe slot (this frame's presented
    /// truth at emit time — see [`Self::probed_cell_glyph`]) and answers
    /// `false` off-row, left of the grid, or past the captured width. The
    /// text-first predicate the own-row flying transients (fire embers, pack
    /// dots) key their dimming on — each site keeps its own dim factor.
    /// Deliberately narrower than `probed_cell_glyph`: the neighbor-row probes
    /// are NOT consulted, so an unprobed neighbor row flies at full brightness
    /// as it always has.
    pub(super) fn own_row_glyph_at_px(&self, geom: Geom, x: f32, y: f32) -> bool {
        let prow = ((y - geom.origin_y as f32) / geom.ch as f32).floor() as i32;
        let pcol = ((x - geom.origin_x as f32) / geom.cw as f32).floor() as i32;
        pcol >= 0
            && self.row_prev_meta.is_some_and(|m| {
                i32::from(m.row) == prow
                    && self.row_prev.get(pcol as usize).is_some_and(|&g| g != ' ')
            })
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

    /// The rolling spectrum hue in turns `0..1` — the hue the NEXT beam segment
    /// will be laid in (phaser advances it 2× per move; see `hue_step`). Public
    /// so the host can hand the SAME sweep phase to
    /// [`crate::cursor_phaser::CursorPhaser`] (read it after [`Self::tick`]),
    /// keeping the emitter cursor and the streak one continuous piece of light.
    #[must_use]
    pub fn beam_hue(&self) -> f32 {
        self.hue
    }

    /// The rainbow kitty family's shared phase-ring clock (`0..1024`), sampled
    /// after [`Self::tick`]. Hosts should hand this exact value to
    /// [`crate::cursor_rainbow::CursorRainbow::tick_with_family_phase`] so the
    /// caret, its halo, and the ribbon under that cell resolve one spectrum
    /// position instead of running visually adjacent independent clocks. The
    /// value is rendering-authoritative only while this engine is running
    /// [`GlowStyle::RainbowKitty`].
    #[must_use]
    pub fn rainbow_phase(&self) -> f32 {
        // SEAM POINT 7 (§17.2, D14): the family's shared phase ring is v2's
        // spine clock while it owns the frame — the `0..1024` ring the caret
        // and the companion read. At rest the ring is at its origin.
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
        // SEAM POINT 4 (§17.2, D14, C2): the caret's own field position is
        // v2's while it owns the frame — the ribbon head, the caret and a
        // star's halo on that cell read this one number. It is the SEATED
        // stop (`v2_stop`), held across a cell death under a parked caret;
        // the engine's live field only until the first seat. When v2 does
        // not own the frame (another style, or a dark tick) there is no
        // field: the arc's origin.
        if self.v2.engaged() {
            return self.v2_stop.map_or_else(|| self.v2.field(), |(t, _)| t);
        }
        0.0
    }

    /// The colour of a SEATED caret stop (`v2_stop`), spelled as
    /// `rk::ribbon::Ribbon::head_rgb` spells the live one: the authored stop
    /// at the REFLECTED walk (D4, `tri`) on a dark theme, the body's own
    /// leading ink on that stop on a light one. The two agree to the byte on
    /// every seat frame (`the_caret_settles_on_its_own_stop_without_a_pop`
    /// pins the landing frame).
    pub(super) fn v2_caret_stop_rgb(t: f32, cfg: &rk::Config) -> u32 {
        let arc = crate::spectrum::spectrum(rk::ribbon::walk_arc(t));
        if cfg.dark_theme {
            arc
        } else {
            rk::ribbon::light_role(cfg).ink(arc)
        }
    }

    /// Source RGB selected by the typing ribbon HEAD's BODY, before additive
    /// coverage premultiplication. This is the colour a companion cursor should
    /// inherit to meet the laid ribbon without a palette seam.
    ///
    /// All presentations and both theme arms resolve [`Self::rainbow_field`]
    /// once. Dark themes return the authored spectrum colour; light themes apply
    /// the leading-ink recipe to that same field position. A non-rainbow config,
    /// or an engine that has not observed a cursor yet, returns `None`.
    #[must_use]
    pub fn rainbow_head_rgb(&self, cfg: &GlowConfig) -> Option<u32> {
        // SEAM POINT 5 (§17.2, D14): the head's body colour is v2's while it
        // owns the frame, READ OFF THE SEATED STOP so a cell death under a
        // parked caret cannot move it. `Config::from_glow` is a seven-field
        // copy, not a resolution.
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
        // A rainbow kitty tick v2 does not own (the trail dark, or nothing
        // presented yet): the caret keeps the arc's origin stop, exactly the
        // colour an empty field resolved to before the seam.
        self.last?;
        let field = self.rainbow_field();
        if !cfg.dark_theme {
            return Some(InkRole::Leading.ink(rainbow_thing_of(field)));
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
    // The five `swap_*` methods below hand the host THIS frame's stream by
    // moving the buffer instead of copying it, exactly like the `out` quad
    // scratch the host already passes into [`Self::tick`]. A hot rainbow ribbon
    // at retina metrics carries 6k-14k `GlowQuad` (16 B each — see the pinned
    // bounds in `tests/cursor_bench.rs`), so the copy path rewrites ~100-230 KB
    // per presented frame on top of the identical bytes the emitter just wrote.
    //
    // The invariant that makes the move safe is the same one the sibling
    // channels in `EffectsPipeline::apply` rely on: [`Self::tick`] clears every
    // one of these scratches at entry, so the buffer handed BACK by the swap —
    // which holds the PREVIOUS frame's content until the next `tick` — is never
    // read. A host that reads a stream twice within one frame must therefore
    // stay on the borrowing accessor above; swap exactly once, at the splice.

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
