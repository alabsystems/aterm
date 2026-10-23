// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admission log, block-fill verdict, and live `trail status` reporting.

use super::*;

/// Spawn-seam verdict captured by an [`AdmissionRecord`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionPhase {
    /// [`CursorGlow::move_licensed`] admitted the move.
    Licensed,
    /// The move was refused at the license seam.
    Declined,
}

impl AdmissionPhase {
    /// Wire token printed by the `trail` control verb.
    #[inline]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Licensed => "licensed",
            Self::Declined => "declined",
        }
    }
}

/// One entry of the admission diagnosis ring ([`CursorGlow::admission_log`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionRecord {
    /// Monotonic per-engine sequence (1-based).
    pub seq: u64,
    pub phase: AdmissionPhase,
    /// Decline reason token (`-` when licensed).
    pub reason: &'static str,
    /// Timestamp of the move.
    pub at: Instant,
    pub origin: (u16, u16),
    pub target: (u16, u16),
    /// Alternate-screen bit at the verdict.
    pub alternate_screen: bool,
    /// Licence class token that admitted the move.
    pub licence: &'static str,
}

impl AdmissionRecord {
    /// Ring token: admitted by a press-hint class.
    pub const LICENCE_KEY: &'static str = "key";
    /// Ring token: admitted by the delivered-insert licence.
    pub const LICENCE_INSERT: &'static str = "insert";
    /// Ring token: admitted by the in-flight pool.
    pub const LICENCE_IN_FLIGHT: &'static str = "inflight";
    /// Ring token: admitted as a program rewrite of a just-typed row.
    pub const LICENCE_REWRITE: &'static str = "rewrite";
    /// Ring token: recalled keylessly after the program printed on its row.
    pub const LICENCE_RECALL: &'static str = "recall";
    /// Ring token: lit by the classic motion wake.
    pub const LICENCE_MOTION: &'static str = "motion";
    /// Ring token: declined row.
    pub const LICENCE_NONE: &'static str = "none";

    /// Format one diagnostic line in the `trail` verb's row shape.
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

/// Capacity of the admission diagnosis ring.
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
    /// Cumulative admission tally.
    pub(super) tally: AdmissionTally,
}

/// Cumulative licence scoreboard behind [`CursorGlow::admission_tally`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdmissionTally {
    /// Moves the spawn seam licensed.
    pub licensed: u64,
    /// Moves the spawn seam declined.
    pub declined: u64,
    /// Most recent decline reason token, if any.
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

/// Which cursor-body effect owns the block cursor's fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockFillOwner {
    /// [`crate::cursor_rainbow`]: the `rainbow kitty` block.
    Rainbow,
    /// The `fire` style's forge fill ([`CursorGlow::forge_fill`]).
    Forge,
    /// [`crate::cursor_phaser`]: the `phaser` style's emitter block.
    Phaser,
    /// The `laser` style's lightning bolt fill.
    Bolt,
    /// [`crate::cursor_comet`]: the `comet` style's frosted nucleus.
    Comet,
    /// [`crate::cursor_droplet`]: the `water` style's liquid block.
    Droplet,
    /// [`crate::cursor_beam`]: the emitter block for styles without a bespoke body.
    BeamRod,
    /// The typing-momentum glow's warm tint (`crate::cursor_momentum`).
    Momentum,
}

impl BlockFillOwner {
    /// Wire token for the `trail status` row.
    #[inline]
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

    /// Provenance of this owner's body colour.
    #[inline]
    #[must_use]
    pub const fn base_from(self) -> BlockFillBase {
        match self {
            Self::Rainbow | Self::Phaser | Self::Momentum => BlockFillBase::CursorColor,
            Self::Comet | Self::Bolt | Self::BeamRod => BlockFillBase::TrailColor,
            Self::Forge | Self::Droplet => BlockFillBase::StyleIdentity,
        }
    }
}

/// Provenance of a [`BlockFillOwner`]'s body colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockFillBase {
    /// The terminal's resolved cursor colour.
    CursorColor,
    /// The resolved trail colour (`GlowConfig::color`).
    TrailColor,
    /// The style's own identity colour.
    StyleIdentity,
    /// Theme-polar fallback when no `cursor_color` was pinned.
    White,
}

impl BlockFillBase {
    /// Wire token for the `trail status` row.
    #[inline]
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

/// Block-cursor fill override state for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockFill {
    /// The effect that set `cursor_fill_override` this frame.
    pub owner: BlockFillOwner,
    /// Override colour `0x00RRGGBB` sent to the renderer.
    pub fill: u32,
    /// Base colour `0x00RRGGBB` handed to this owner, if any.
    pub base: Option<u32>,
}

impl BlockFill {
    /// Provenance of this fill's body colour.
    #[inline]
    #[must_use]
    pub const fn base_from(self) -> BlockFillBase {
        match (self.owner.base_from(), self.base) {
            (BlockFillBase::CursorColor, None) => BlockFillBase::White,
            (from, _) => from,
        }
    }
}

/// Live state snapshot of the cursor-trail engine for `trail status`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailStatus<'a> {
    /// Raw `cursor_trail_style` config string before resolution.
    pub style_raw: &'a str,
    /// Resolved [`GlowStyle`], or `None` when disabled.
    pub style: Option<GlowStyle>,
    /// Whether the raw style spelling was unrecognized.
    pub unknown: bool,
    /// Whether the configured style is enabled.
    pub config_enabled: bool,
    /// Effective enabled state for this frame after serious-mode gating.
    pub effective: bool,
    /// Whether the window is focused.
    pub focused: bool,
    /// Resolved motion policy (`full` or `reduced`).
    pub motion_stage: &'static str,
    /// Configured `motion` mode (`auto`, `full`, or `reduced`).
    pub motion_mode: &'static str,
    /// Soft load-shed envelope in `0.0..=1.0`.
    pub shed: f32,
    /// Live effect intensity after multipliers and load-shedding.
    pub intensity: f32,
    /// Whether the key seam is open ([`CursorGlow::sound_seam_open`]).
    pub sound_seam: bool,
    /// Dark-theme ribbon presentation token.
    pub ribbon_look: &'static str,
    /// Cumulative admission scoreboard.
    pub tally: AdmissionTally,
    /// Moves the spawn seam admitted ([`CursorGlow::spawns`]).
    pub spawns: u64,
    /// Live typing sparks ([`CursorGlow::ribbon_segments`]).
    pub ribbon_segments: usize,
    /// Distinct hue bands among live ribbon segments ([`CursorGlow::ribbon_hue_bands`]).
    pub ribbon_hue_bands: usize,
    /// Ribbon boundaries whose composite reads as band ink ([`CursorGlow::ribbon_drawn`]).
    pub ribbon_drawn: usize,
    /// Milliseconds remaining in a falling curtain ([`CursorGlow::curtain_left_ms`]).
    pub ribbon_curtain_ms: Option<u32>,
    /// Caret spectrum position in `0.0..=1.0` ([`CursorGlow::rainbow_field`]).
    pub field: f32,
    /// Total live sparks ([`CursorGlow::live_sparks`]).
    pub sparks: usize,
    /// Canonical typing-momentum metric ([`CursorGlow::typing_momentum`]).
    pub momentum: f32,
    /// Eased display spine ([`CursorGlow::momentum_display`]).
    pub momentum_display: f32,
    /// Typing-momentum glow value.
    pub momentum_glow: f32,
    /// Whether any live light is active ([`CursorGlow::is_active`]).
    pub glow_active: bool,
    /// Whether the full-body pet companion is drawn and animating.
    pub pet_active: bool,
    /// Resident brain's current action token.
    pub pet_action: &'a str,
    /// Earned affection/contentment in `0.0..=1.0`.
    pub pet_content: f32,
    /// Queued clicks/strokes waiting for consumption.
    pub pet_pending: u8,
    /// Located console attention focus, reason, anchor, and pose.
    pub pet_focus: &'a str,
    pub pet_reason: &'a str,
    pub pet_anchor: Option<u64>,
    pub pet_event_seq: u64,
    pub pet_pose: &'a str,
    /// Last drawn resident body `(x0, x1, y0, y1)` in frame pixels.
    pub pet_body: Option<(i32, i32, i32, i32)>,
    /// Whether the flying cursor cat is animating.
    pub cat_active: bool,
    /// Active block-cursor fill override, if any.
    pub block_fill: Option<BlockFill>,
    /// Flow state from [`CursorGlow::flow_status`].
    pub flow: rk::Flow,
    /// Delivered-insert counters from [`CursorGlow::insert_tally`].
    pub inserts: InsertTally,
    /// In-flight licence counters from [`CursorGlow::in_flight_tally`].
    pub in_flight: InFlightTally,
    /// Opaque comet cell count and peak alpha on the last tick.
    pub comet_cells: usize,
    pub comet_alpha_max: u8,
}

impl TrailStatus<'_> {
    /// Whether a multi-hue rainbow ribbon is active on glass.
    #[inline]
    #[must_use]
    pub const fn ribbon_active(&self) -> bool {
        (self.ribbon_segments >= 2 && self.ribbon_hue_bands >= 2)
            || (self.ribbon_curtain_ms.is_some() && self.ribbon_drawn >= 2)
    }

    /// Resolved style label token.
    #[inline]
    #[must_use]
    pub const fn resolved_label(&self) -> &'static str {
        match self.style {
            Some(style) => style.label(),
            None => "off",
        }
    }

    /// Format the `trail status` wire tail.
    #[must_use]
    pub fn line(&self) -> String {
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
            self.flow.heat,
            self.flow.combo,
            self.flow.best,
            self.glow_active,
            self.pet_active,
            self.cat_active,
            self.block_fill.map_or("none", |b| b.owner.label()),
            self.block_fill
                .map_or_else(|| "none".to_string(), |b| format!("{:06x}", b.fill)),
            self.block_fill
                .and_then(|b| b.base)
                .map_or_else(|| "none".to_string(), |base| format!("{base:06x}")),
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
            if self.unknown { "unknown" } else { "none" },
            self.comet_cells,
            self.comet_alpha_max,
        )
    }

    /// Format [`Self::line`] plus rainbow kitty v2 status rows.
    #[must_use]
    pub fn line_v2(&self, v2: Option<rk::Status>) -> String {
        let mut line = self.line();
        if let Some(s) = v2 {
            use std::fmt::Write as _;
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
    /// Rainbow kitty v2 `trail status` counters when v2 owns the frame.
    #[inline]
    #[must_use]
    pub fn v2_status(&self) -> Option<rk::Status> {
        self.v2.engaged().then(|| self.v2.status())
    }

    /// Whether a keypress on this engine's window would play a key sound.
    #[inline]
    #[must_use]
    pub const fn sound_seam_open(&self) -> bool {
        self.sound_live
    }

    /// Current flow status (`flow=`, `combo=`, `combo_best=`).
    #[inline]
    #[must_use]
    pub fn flow_status(&self) -> rk::Flow {
        if self.v2.engaged() {
            self.v2.spine().flow()
        } else {
            rk::Flow::default()
        }
    }

    /// Whether the last tick's row probe captured the rows above and below the cursor.
    #[inline]
    #[must_use]
    pub fn neighbor_rows_probed(&self) -> Option<(bool, bool)> {
        self.row_prev_meta
            .map(|m| (m.above == NbrProbe::Probed, m.below == NbrProbe::Probed))
    }

    /// Recent admission-diagnosis records, oldest to newest.
    #[inline]
    pub fn admission_log(&self) -> impl Iterator<Item = &AdmissionRecord> {
        self.admission_log.iter()
    }

    /// Cumulative admission scoreboard ([`AdmissionTally`]).
    #[inline]
    #[must_use]
    pub const fn admission_tally(&self) -> AdmissionTally {
        self.admission_log.tally
    }

    /// Moves the spawn seam has licensed over this engine's life.
    #[inline]
    #[must_use]
    pub const fn spawns(&self) -> u64 {
        self.spawns
    }

    /// Row-band moves replayed into this engine.
    #[inline]
    #[must_use]
    pub const fn band_moves(&self) -> u64 {
        self.coord_band_moves
    }

    /// Ring reason: no licence term was fresh.
    pub const DECLINE_NO_FRESH_HINT: &'static str = "no-fresh-hint";
    /// Ring reason: multi-cell coalesce outran the press credit.
    pub const DECLINE_NO_CREDITS: &'static str = "no-credits";
    /// Ring reason: style shape declined to emit.
    pub const DECLINE_OFF_SHAPE: &'static str = "off-shape";
    /// Ring reason: anchored-echo lane refused a program-branded row.
    pub const DECLINE_PROGRAM_ROW: &'static str = "program-row";
    /// Ring reason: hidden-to-visible relocation refused by the ConPTY hide bridge.
    pub const DECLINE_HIDDEN_RELOCATION: &'static str = "hidden-relocation";

    /// Record a licensed verdict in the diagnosis ring under the named licence
    /// class (`AdmissionRecord::LICENCE_*`).
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

    /// Record a declined verdict in the diagnosis ring.
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

    /// Count of live typing sparks in the ribbon.
    #[inline]
    #[must_use]
    pub fn ribbon_segments(&self) -> usize {
        if self.v2.engaged() {
            return self.v2.ribbon_segments();
        }
        self.sparks.iter().filter(|spark| spark.typing).count()
    }

    /// Count of ribbon boundaries whose composite reads as band ink.
    #[inline]
    #[must_use]
    pub const fn ribbon_drawn(&self) -> usize {
        if self.v2.engaged() {
            return self.v2.ribbon_drawn();
        }
        0
    }

    /// Milliseconds remaining in a falling curtain, if any.
    #[inline]
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        if self.v2.engaged() {
            return self.v2.curtain_left_ms(now);
        }
        None
    }

    /// Count of distinct hue bands among lit ribbon boundaries.
    #[inline]
    #[must_use]
    pub fn ribbon_hue_bands(&self) -> usize {
        if self.v2.engaged() {
            return self.v2.ribbon_hue_bands();
        }
        0
    }

    /// Total live sparks across ribbon, jump/comet cells, and classic wake.
    #[inline]
    #[must_use]
    pub const fn live_sparks(&self) -> usize {
        self.sparks.len() + self.classic.live_sparks()
    }

    /// Eased momentum display value.
    #[inline]
    #[must_use]
    pub const fn momentum_display(&self) -> f32 {
        if self.v2.engaged() {
            return self.v2.momentum_display();
        }
        0.0
    }

    /// Probed text spans for the cursor's row and its flanking rows.
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
            if let Some(first) = cols.iter().position(|&g| g != ' ')
                && let Some(rel_last) = cols[first..].iter().rposition(|&g| g != ' ')
            {
                *slot = Some((r, first as i32, (first + rel_last) as i32 + 1));
            }
        }
        spans
    }

    /// This frame's radial light halos.
    #[inline]
    pub const fn halos(&self) -> &[RainHalo] {
        self.halo_out.as_slice()
    }

    /// This frame's under-ink flame quads.
    #[inline]
    pub const fn under_quads(&self) -> &[GlowQuad] {
        self.under_out.as_slice()
    }

    /// Mutable under-ink quad sink for caret body effects.
    #[inline]
    pub(crate) const fn under_sink(&mut self) -> &mut Vec<GlowQuad> {
        &mut self.under_out
    }

    /// This frame's per-pixel fire patches.
    #[inline]
    pub const fn patches(&self) -> &[FirePatch] {
        self.patch_out.as_slice()
    }

    /// This frame's charred-ink foreground overrides.
    #[inline]
    pub const fn charred(&self) -> &[CharFg] {
        self.char_out.as_slice()
    }

    /// Rolling spectrum hue in turns `0.0..1.0`.
    #[inline]
    #[must_use]
    pub const fn beam_hue(&self) -> f32 {
        self.hue
    }

    /// Shared rainbow phase-ring clock (`0.0..1024.0`).
    #[inline]
    #[must_use]
    pub const fn rainbow_phase(&self) -> f32 {
        // While v2 owns the frame the shared phase ring is v2's spine clock; at
        // rest the ring is at its origin.
        if self.v2.engaged() {
            return self.v2.phase();
        }
        0.0
    }

    /// Caret spectrum position in `0.0..=1.0`.
    #[inline]
    pub fn rainbow_field(&self) -> f32 {
        if self.v2.engaged() {
            return self.v2_stop.map_or_else(|| self.v2.field(), |(t, _)| t);
        }
        0.0
    }

    /// Colour of a seated caret stop (`v2_stop`).
    #[inline]
    pub(super) fn v2_caret_stop_rgb(t: f32, cfg: &rk::Config) -> u32 {
        rk::ribbon::caret_ink(crate::spectrum::spectrum(rk::ribbon::walk_arc(t)), cfg)
    }

    /// Source RGB selected by the typing ribbon head's body.
    #[inline]
    #[must_use]
    pub fn rainbow_head_rgb(&self, cfg: &GlowConfig) -> Option<u32> {
        // While v2 owns the frame the head colour is read off the seated stop,
        // so a cell death under a parked caret cannot move it.
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

    /// This frame's fire contrast-halo cell strengths.
    #[inline]
    pub const fn halo_cells(&self) -> &[FireHaloCell] {
        self.fire_halo_out.as_slice()
    }

    /// Swap this frame's radial light halos into `dst` (see [`Self::halos`]).
    #[inline]
    pub const fn swap_halos(&mut self, dst: &mut Vec<RainHalo>) {
        std::mem::swap(dst, &mut self.halo_out);
    }

    /// Swap this frame's under-ink flame quads into `dst` (see [`Self::under_quads`]).
    #[inline]
    pub const fn swap_under_quads(&mut self, dst: &mut Vec<GlowQuad>) {
        std::mem::swap(dst, &mut self.under_out);
    }

    /// Swap this frame's per-pixel fire patches into `dst` (see [`Self::patches`]).
    #[inline]
    pub const fn swap_patches(&mut self, dst: &mut Vec<FirePatch>) {
        std::mem::swap(dst, &mut self.patch_out);
    }

    /// Swap this frame's charred-ink overrides into `dst` (see [`Self::charred`]).
    #[inline]
    pub const fn swap_charred(&mut self, dst: &mut Vec<CharFg>) {
        std::mem::swap(dst, &mut self.char_out);
    }

    /// Swap this frame's fire contrast-halo cells into `dst` (see [`Self::halo_cells`]).
    #[inline]
    pub const fn swap_halo_cells(&mut self, dst: &mut Vec<FireHaloCell>) {
        std::mem::swap(dst, &mut self.fire_halo_out);
    }
}

/// Probed text lines of one frame ([`CursorGlow::probed_text_spans`]).
#[derive(Clone, Copy, Default)]
pub(crate) struct TextSpans {
    rows: [Option<(i32, i32, i32)>; 3],
    read: [Option<i32>; 3],
}

impl TextSpans {
    /// Whether grid cell `(row, col)` lies on a probed line of text.
    #[inline]
    pub(super) const fn covers_cell(&self, row: i32, col: i32) -> bool {
        let mut i = 0;
        while i < self.rows.len() {
            if let Some((r, first, end)) = self.rows[i]
                && r == row
                && col >= first
                && col < end
            {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Each probed line of text as `(row, first, end)`: the column span from
    /// its first glyph to one past its last.
    #[inline]
    pub(super) fn lines(&self) -> impl Iterator<Item = (i32, i32, i32)> + '_ {
        self.rows.iter().flatten().copied()
    }

    /// Whether the probe read grid row `row` this frame, so a cell of it off
    /// every line ([`Self::covers_cell`]) is known blank rather than unknown.
    #[inline]
    pub(super) const fn reads_row(&self, row: i32) -> bool {
        matches!(self.read[0], Some(r) if r == row)
            || matches!(self.read[1], Some(r) if r == row)
            || matches!(self.read[2], Some(r) if r == row)
    }
}

/// Per-row text spans and cell colours read from the frame ([`CursorGlow::observe_frame_text`]).
#[derive(Default)]
pub(crate) struct FrameText {
    pub(super) at: Option<Instant>,
    pub(super) lo: i32,
    pub(super) rows: Vec<Option<(i32, i32)>>,
    /// Row-major cell `(ink, ground)` pairs (`0x00RRGGBB`) for each read row.
    pub(super) pairs: Vec<Option<(Option<u32>, u32)>>,
    pub(super) cols: usize,
}

impl FrameText {
    /// The ink (`None` for a blank cell) and ground of cell `(row, col)` in
    /// the frame's own cells, or `None` for a cell the frame did not read.
    #[inline]
    pub(crate) const fn pair(&self, row: i32, col: i32) -> Option<(Option<u32>, u32)> {
        let i = row - self.lo;
        if i < 0 || i as usize >= self.rows.len() || col < 0 || col as usize >= self.cols {
            return None;
        }
        let idx = i as usize * self.cols + col as usize;
        let pairs = self.pairs.as_slice();
        if idx < pairs.len() { pairs[idx] } else { None }
    }
}

/// Combined text view over the row probe and [`FrameText`].
#[derive(Clone, Copy)]
pub(crate) struct TextView<'a> {
    pub(crate) spans: TextSpans,
    frame: &'a [Option<(i32, i32)>],
    lo: i32,
}

impl<'a> TextView<'a> {
    /// The probe's lines and the frame's rows `frame` (`None` for a frame
    /// that handed none).
    #[inline]
    pub(crate) const fn new(spans: TextSpans, frame: Option<&'a FrameText>) -> Self {
        match frame {
            Some(f) => Self {
                spans,
                frame: f.rows.as_slice(),
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
    #[inline]
    const fn frame_row(&self, row: i32) -> Option<Option<(i32, i32)>> {
        let i = row - self.lo;
        if i >= 0 && (i as usize) < self.frame.len() {
            Some(self.frame[i as usize])
        } else {
            None
        }
    }

    /// Whether the frame says what row `row` holds (the probe or the cells).
    #[inline]
    pub(crate) const fn knows_row(&self, row: i32) -> bool {
        self.spans.reads_row(row) || self.frame_row(row).is_some()
    }

    /// Text span of `row` from [`FrameText`] when not covered by the row probe.
    #[inline]
    pub(crate) const fn frame_line(&self, row: i32) -> Option<Option<(i32, i32)>> {
        if self.spans.reads_row(row) {
            return None;
        }
        self.frame_row(row)
    }

    /// Whether grid cell `(row, col)` lies on a line of text the frame shows.
    #[inline]
    pub(crate) const fn covers_cell(&self, row: i32, col: i32) -> bool {
        if self.spans.reads_row(row) {
            return self.spans.covers_cell(row, col);
        }
        match self.frame_row(row) {
            Some(Some((first, end))) => col >= first && col < end,
            _ => false,
        }
    }

    /// Whether grid row `row` was read and contains no glyphs.
    #[inline]
    pub(crate) const fn row_is_blank(&self, row: i32) -> bool {
        if self.spans.reads_row(row) {
            let mut i = 0;
            while i < self.spans.rows.len() {
                if let Some((r, _, _)) = self.spans.rows[i]
                    && r == row
                {
                    return false;
                }
                i += 1;
            }
            true
        } else {
            matches!(self.frame_row(row), Some(None))
        }
    }

    /// Whether the dark streak treats cell `(row, col)` as text.
    #[inline]
    pub(crate) const fn over_text(&self, row: i32, col: i32) -> bool {
        !self.knows_row(row) || self.covers_cell(row, col)
    }
}
