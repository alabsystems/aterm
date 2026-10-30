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

/// Opaque host identities of the terminal view that produced an admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionSource {
    pub view: u64,
    pub session: u64,
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
    /// The owner bound when the verdict was appended, unknown for unbound hosts.
    pub source: Option<AdmissionSource>,
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
    #[inline]
    #[must_use]
    pub fn line(&self, now: Instant) -> String {
        format!(
            "admission seq={} phase={} reason={} age_ms={:.0} origin={},{} target={},{} alt={} licence={} source={} source_view={} source_session={}",
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
            if self.source.is_some() {
                "bound"
            } else {
                "unknown"
            },
            std::fmt::from_fn(|f| match self.source {
                Some(s) => write!(f, "{}", s.view),
                None => f.write_str("-"),
            }),
            std::fmt::from_fn(|f| match self.source {
                Some(s) => write!(f, "{}", s.session),
                None => f.write_str("-"),
            }),
        )
    }
}

/// Capacity of the admission diagnosis ring.
pub const ADMISSION_LOG_CAP: usize = 32;

/// The fixed-size drop-oldest ring behind [`CursorGlow::admission_log`],
/// resident in the engine (one slot write per observed move).
pub(super) struct AdmissionLog {
    pub(super) records: [Option<AdmissionRecord>; ADMISSION_LOG_CAP],
    /// Next slot to write (== oldest entry once the ring has wrapped).
    pub(super) head: usize,
    /// Total records ever pushed; stamps each record's `seq`.
    pub(super) seq: u64,
    /// Cumulative admission tally.
    pub(super) tally: AdmissionTally,
}

impl Default for AdmissionLog {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Cumulative licence scoreboard behind [`CursorGlow::admission_tally`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionTally {
    /// Moves the spawn seam licensed.
    pub licensed: u64,
    /// Moves the spawn seam declined.
    pub declined: u64,
    /// Most recent decline reason token, if any.
    pub last_decline_reason: Option<&'static str>,
}

impl Default for AdmissionTally {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl AdmissionTally {
    /// Zeroed admission tally.
    pub const EMPTY: Self = Self {
        licensed: 0,
        declined: 0,
        last_decline_reason: None,
    };
}

impl AdmissionLog {
    pub(super) const EMPTY: Self = Self {
        records: [None; ADMISSION_LOG_CAP],
        head: 0,
        seq: 0,
        tally: AdmissionTally::EMPTY,
    };

    #[inline]
    const fn str_eq(a: &str, b: &str) -> bool {
        let (ab, bb) = (a.as_bytes(), b.as_bytes());
        if ab.len() != bb.len() {
            return false;
        }
        let mut i = 0;
        while i < ab.len() {
            if ab[i] != bb[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    #[inline]
    pub(super) const fn push(&mut self, mut record: AdmissionRecord) {
        self.seq += 1;
        record.seq = self.seq;
        match record.phase {
            AdmissionPhase::Licensed => self.tally.licensed += 1,
            AdmissionPhase::Declined => {
                self.tally.declined += 1;
                self.tally.last_decline_reason = Some(record.reason);
            }
        }
        let head_preservable = match &self.records[self.head] {
            Some(old) => {
                matches!(old.phase, AdmissionPhase::Licensed)
                    || !Self::str_eq(old.reason, CursorGlow::DECLINE_NO_FRESH_HINT)
            }
            None => false,
        };
        if matches!(record.phase, AdmissionPhase::Declined)
            && Self::str_eq(record.reason, CursorGlow::DECLINE_NO_FRESH_HINT)
            && head_preservable
        {
            const {
                assert!(ADMISSION_LOG_CAP.is_power_of_two());
            }
            const MASK: usize = ADMISSION_LOG_CAP - 1;
            let mut offset = None;
            let mut off = 1;
            while off < ADMISSION_LOG_CAP {
                let idx = (self.head + off) & MASK;
                if let Some(r) = &self.records[idx]
                    && matches!(r.phase, AdmissionPhase::Declined)
                    && Self::str_eq(r.reason, CursorGlow::DECLINE_NO_FRESH_HINT)
                {
                    offset = Some(off);
                    break;
                }
                off += 1;
            }
            if let Some(offset) = offset {
                let mut step = offset;
                while step < ADMISSION_LOG_CAP - 1 {
                    let dst = (self.head + step) & MASK;
                    let src = (self.head + step + 1) & MASK;
                    self.records[dst] = self.records[src];
                    step += 1;
                }
                let newest = (self.head + ADMISSION_LOG_CAP - 1) & MASK;
                self.records[newest] = Some(record);
                return;
            }
        }
        self.records[self.head] = Some(record);
        self.head = (self.head + 1) & (ADMISSION_LOG_CAP - 1);
    }

    /// Entries oldest → newest.
    #[inline]
    pub(super) fn iter(&self) -> impl Iterator<Item = &AdmissionRecord> {
        self.records[self.head..]
            .iter()
            .chain(&self.records[..self.head])
            .flatten()
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
    /// Whether the resident brain is in deep sleep: past its breath window, drawn at full strength
    /// (no fade under way) and asking for no frame.
    pub pet_deep: bool,
    /// Last drawn resident body `(x0, x1, y0, y1)` in frame pixels.
    pub pet_body: Option<(i32, i32, i32, i32)>,
    /// The flying head's drawn body `(x0, x1, y0, y1)` in frame pixels
    /// ([`CursorGlow::head_body_window_box`]).
    pub head_body: Option<(i32, i32, i32, i32)>,
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
    #[inline]
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
             pet_action={} pet_content={:.3} pet_pending={} pet_body={} head_body={} \
             pet_focus={} pet_reason={} pet_anchor={} pet_event_seq={} pet_pose={} pet_deep={} \
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
            std::fmt::from_fn(|f| match self.ribbon_curtain_ms {
                Some(ms) => write!(f, "{ms}"),
                None => f.write_str("none"),
            }),
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
            match self.block_fill {
                Some(b) => b.owner.label(),
                None => "none",
            },
            std::fmt::from_fn(|f| match self.block_fill {
                Some(b) => write!(f, "{:06x}", b.fill),
                None => f.write_str("none"),
            }),
            std::fmt::from_fn(|f| match self.block_fill {
                Some(BlockFill {
                    base: Some(base), ..
                }) => write!(f, "{base:06x}"),
                _ => f.write_str("none"),
            }),
            match self.block_fill {
                Some(b) => b.base_from().label(),
                None => "none",
            },
            self.pet_action,
            self.pet_content,
            self.pet_pending,
            std::fmt::from_fn(|f| match self.pet_body {
                Some((x0, x1, y0, y1)) => write!(f, "{x0},{x1},{y0},{y1}"),
                None => f.write_str("none"),
            }),
            std::fmt::from_fn(|f| match self.head_body {
                Some((x0, x1, y0, y1)) => write!(f, "{x0},{x1},{y0},{y1}"),
                None => f.write_str("none"),
            }),
            self.pet_focus,
            self.pet_reason,
            std::fmt::from_fn(|f| match self.pet_anchor {
                Some(id) => write!(f, "{id}"),
                None => f.write_str("none"),
            }),
            self.pet_event_seq,
            self.pet_pose,
            self.pet_deep,
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
    #[inline]
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
    /// Bind diagnostic provenance for future verdicts without changing retained history or ink.
    #[inline]
    pub const fn set_admission_source(&mut self, source: Option<AdmissionSource>) {
        self.admission_source = source;
    }

    /// Rainbow kitty v2 `trail status` counters when v2 owns the frame.
    #[inline]
    #[must_use]
    pub const fn v2_status(&self) -> Option<rk::Status> {
        if self.v2.engaged() {
            Some(self.v2.status())
        } else {
            None
        }
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
    pub const fn flow_status(&self) -> rk::Flow {
        if self.v2.engaged() {
            self.v2.spine().flow()
        } else {
            rk::Flow::EMPTY
        }
    }

    /// Whether the last tick's row probe captured the rows above and below the cursor.
    #[inline]
    #[must_use]
    pub const fn neighbor_rows_probed(&self) -> Option<(bool, bool)> {
        match self.row_prev_meta {
            Some(m) => Some((
                matches!(m.above, NbrProbe::Probed),
                matches!(m.below, NbrProbe::Probed),
            )),
            None => None,
        }
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
    #[inline]
    pub(super) const fn log_licensed_as(
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
            source: self.admission_source,
        });
    }

    /// Record a declined verdict in the diagnosis ring.
    #[inline]
    pub(super) const fn log_decline(
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
            source: self.admission_source,
        });
    }

    /// Count of live typing sparks in the ribbon.
    #[inline]
    #[must_use]
    pub const fn ribbon_segments(&self) -> usize {
        if self.v2.engaged() {
            return self.v2.ribbon_segments();
        }
        let sparks = self.sparks.as_slice();
        let mut count = 0;
        let mut i = 0;
        while i < sparks.len() {
            if sparks[i].typing {
                count += 1;
            }
            i += 1;
        }
        count
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
    pub const fn ribbon_hue_bands(&self) -> usize {
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
    #[inline]
    pub(super) const fn probed_text_spans(&self) -> TextSpans {
        self.probed_spans
    }

    /// Recompute [`Self::probed_spans`] after rotating or lifting the previous row probe.
    #[inline]
    pub(super) const fn refresh_probed_text_spans(&mut self) {
        let mut spans = TextSpans::EMPTY;
        let Some(m) = self.row_prev_meta.as_ref() else {
            self.probed_spans = spans;
            return;
        };
        let row = m.row as i32;
        let rows: [(i32, Option<&[char]>); 3] = [
            (
                row - 1,
                if matches!(m.above, NbrProbe::Probed) {
                    Some(self.row_above_prev.as_slice())
                } else {
                    None
                },
            ),
            (row, Some(self.row_prev.as_slice())),
            (
                row + 1,
                if matches!(m.below, NbrProbe::Probed) {
                    Some(self.row_below_prev.as_slice())
                } else {
                    None
                },
            ),
        ];
        let mut i = 0;
        while i < 3 {
            let (r, cols) = rows[i];
            if let Some(cols) = cols {
                spans.read[i] = Some(r);
                let mut first = 0;
                while first < cols.len() && cols[first] == ' ' {
                    first += 1;
                }
                if first < cols.len() {
                    let mut end = cols.len();
                    while end > first && cols[end - 1] == ' ' {
                        end -= 1;
                    }
                    spans.rows[i] = Some((r, first as i32, end as i32));
                }
            }
            i += 1;
        }
        self.probed_spans = spans;
    }

    /// This frame's radial light halos.
    #[inline]
    #[must_use]
    pub const fn halos(&self) -> &[RainHalo] {
        self.halo_out.as_slice()
    }

    /// This frame's under-ink flame quads.
    #[inline]
    #[must_use]
    pub const fn under_quads(&self) -> &[GlowQuad] {
        self.under_out.as_slice()
    }

    /// The ramps laid among this frame's under-ink quads ([`Self::under_quads`]).
    #[inline]
    pub const fn under_ramps(&self) -> &GlowRamps {
        &self.under_ramps
    }

    /// The ramps laid among the last tick's `out` quads.
    #[inline]
    pub const fn out_ramps(&self) -> &GlowRamps {
        &self.out_ramps
    }

    /// Mutable under-ink quad sink for caret body effects.
    #[inline]
    pub(crate) const fn under_sink(&mut self) -> &mut Vec<GlowQuad> {
        &mut self.under_out
    }

    /// `quads` and `ramps` as a beam's sink: its runs laid as ramps where
    /// they pay, under a test's cost where one is set ([`Self::rows_cost`]).
    #[inline]
    pub(crate) fn ramp_sink<'a>(
        &self,
        quads: &'a mut Vec<GlowQuad>,
        ramps: &'a mut GlowRamps,
    ) -> RampSink<'a> {
        rows_sink(quads, ramps, self.rows_cost())
    }

    /// A test's cost for the beams' runs in place of the measured one
    /// ([`Self::set_glow_ramps`], [`Self::set_ramp_cost`]).
    #[inline]
    pub(crate) const fn rows_cost(&self) -> Option<RampCost> {
        #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
        {
            self.rows_cost
        }
        #[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
        {
            None
        }
    }

    /// A test's oracle: lay every ramp row as the quad it stands for
    /// (`on: false`), the stream every ramp reader must draw alike; `on`
    /// lays the runs that pay under the measured cost.
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    #[inline]
    pub const fn set_glow_ramps(&mut self, on: bool) {
        self.rows_cost = if on { None } else { Some(RampCost::NEVER) };
    }

    /// A test's cost for the beams' runs ([`RampCost::ALWAYS`]: every run a
    /// ramp, however short), so a law reaches every producer's ramps.
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    #[inline]
    pub const fn set_ramp_cost(&mut self, cost: RampCost) {
        self.rows_cost = Some(cost);
    }

    /// At a tick's end, the streams folded into their quads where a test
    /// reads them so ([`Self::fold_glow_ramps`]).
    #[inline]
    pub(super) fn fold_for_tests(&mut self, out: &mut Vec<GlowQuad>) {
        #[cfg(any(test, debug_assertions))]
        if self.fold_rows.unwrap_or(cfg!(test)) {
            self.out_ramps.flatten_into(out);
            self.ink_ledger
                .fold_rails(&self.under_ramps, self.under_out.len());
            self.under_ramps.flatten_into(&mut self.under_out);
        }
        #[cfg(not(any(test, debug_assertions)))]
        let _ = out;
    }

    /// A test's read of the streams as quads: the ramps laid as the shipped
    /// path lays them, then folded at the tick's end into the quads they
    /// stand for, so [`Self::under_quads`] and the tick's `out` hold the
    /// whole streams. This crate's own unit tests read them so unless they
    /// say otherwise; every other build lays them unfolded.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn fold_glow_ramps(&mut self, on: bool) {
        self.fold_rows = Some(on);
    }

    /// A test's read of this frame's whole under-ink stream:
    /// [`Self::under_quads`] with every lit ramp row the quad it stands for,
    /// in draw order, however the tick laid it.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub fn under_laid(&self) -> Vec<GlowQuad> {
        let mut all = Vec::new();
        self.under_ramps.expand_into(&self.under_out, &mut all);
        all
    }

    /// A test's read of the last tick's whole `out` stream: `out` with every
    /// lit row of [`Self::out_ramps`] the quad it stands for, in draw order.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub fn out_laid(&self, out: &[GlowQuad]) -> Vec<GlowQuad> {
        let mut all = Vec::new();
        self.out_ramps.expand_into(out, &mut all);
        all
    }

    /// This frame's per-pixel fire patches.
    #[inline]
    #[must_use]
    pub const fn patches(&self) -> &[FirePatch] {
        self.patch_out.as_slice()
    }

    /// This frame's charred-ink foreground overrides.
    #[inline]
    #[must_use]
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
    #[must_use]
    pub fn rainbow_field(&self) -> f32 {
        if self.v2.engaged() {
            return match self.v2_stop {
                Some((t, _)) => t,
                None => self.v2.field(),
            };
        }
        0.0
    }

    /// Colour of a seated caret stop (`v2_stop`).
    #[inline]
    pub(super) const fn v2_caret_stop_rgb(t: f32, cfg: &rk::Config) -> u32 {
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
    #[must_use]
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

    /// Swap this frame's under-ink ramps into `dst` (see [`Self::under_ramps`]).
    #[inline]
    pub const fn swap_under_ramps(&mut self, dst: &mut GlowRamps) {
        std::mem::swap(dst, &mut self.under_ramps);
    }

    /// Swap the last tick's `out` ramps into `dst` (see [`Self::out_ramps`]).
    #[inline]
    pub const fn swap_out_ramps(&mut self, dst: &mut GlowRamps) {
        std::mem::swap(dst, &mut self.out_ramps);
    }

    /// Drop the last tick's `out` ramps, with the quads they lie among.
    #[inline]
    pub fn clear_out_ramps(&mut self) {
        self.out_ramps.clear();
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
#[derive(Clone, Copy)]
pub(crate) struct TextSpans {
    rows: [Option<(i32, i32, i32)>; 3],
    read: [Option<i32>; 3],
}

impl Default for TextSpans {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl TextSpans {
    pub(crate) const EMPTY: Self = Self {
        rows: [None; 3],
        read: [None; 3],
    };

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

    /// Whether any column in `c0..=c1` on `row` lies on a probed line of text.
    #[inline]
    pub(super) const fn covers_col_range(&self, row: i32, (c0, c1): (i32, i32)) -> bool {
        if c0 > c1 {
            return false;
        }
        let mut i = 0;
        while i < self.rows.len() {
            if let Some((r, first, end)) = self.rows[i]
                && r == row
            {
                return c1 >= first && c0 < end;
            }
            i += 1;
        }
        false
    }

    /// Probed lines packed from slot 0 with no `None` gaps.
    #[inline]
    pub(super) const fn packed_rows(&self) -> [Option<(i32, i32, i32)>; 3] {
        let mut out = [None; 3];
        let mut dst = 0;
        let mut src = 0;
        while src < 3 {
            if let Some(line) = self.rows[src] {
                out[dst] = Some(line);
                dst += 1;
            }
            src += 1;
        }
        out
    }

    /// A test's probe: `self` with `row` read as blank, in the first free read slot.
    #[cfg(test)]
    #[inline]
    pub(crate) const fn with_blank_row(mut self, row: i32) -> Self {
        let mut i = 0;
        while i < self.read.len() {
            if self.read[i].is_none() {
                self.read[i] = Some(row);
                return self;
            }
            i += 1;
        }
        self
    }

    /// A test's probe of up to three lines `(row, first, end)`, each row read.
    #[cfg(test)]
    #[inline]
    pub(crate) const fn of_lines(lines: &[(i32, i32, i32)]) -> Self {
        let mut s = Self::EMPTY;
        let limit = if lines.len() < 3 { lines.len() } else { 3 };
        let mut i = 0;
        while i < limit {
            let line = lines[i];
            s.rows[i] = Some(line);
            s.read[i] = Some(line.0);
            i += 1;
        }
        s
    }

    /// Column span `(first, end)` for probed `row`, if present.
    #[inline]
    pub(super) const fn line_at(&self, row: i32) -> Option<(i32, i32)> {
        let mut i = 0;
        while i < self.rows.len() {
            if let Some((r, first, end)) = self.rows[i]
                && r == row
            {
                return Some((first, end));
            }
            i += 1;
        }
        None
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
pub(crate) struct FrameText {
    pub(super) at: Option<Instant>,
    pub(super) lo: i32,
    pub(super) rows: Vec<Option<(i32, i32)>>,
    /// Row-major cell `(ink, ground)` pairs (`0x00RRGGBB`) for each read row.
    pub(super) pairs: Vec<Option<(Option<u32>, u32)>>,
    /// Row-major, alongside [`Self::pairs`]: whether the cell's glyph keeps
    /// under the x-height ([`super::emit::is_short_glyph`], and no combining
    /// mark stands over it).
    pub(super) short: Vec<bool>,
    /// Row-major, alongside [`Self::pairs`]: the cell's glyph as the face's
    /// [`aterm_render::GlyphInk`] indexes it ([`aterm_render::GlyphInk::index`]).
    pub(super) glyph: Vec<u8>,
    pub(super) cols: usize,
    /// The window column of `pairs`' first column (a split pane's offset).
    pub(super) col0: i32,
    /// The pane whose cells the frame handed, in window cells: its rows
    /// `0..1` and its columns `2..3`, end exclusive (`None`: no frame handed
    /// any). A split's focused pane hands its own cells, so its edge lies
    /// inside the window's grid, where the GUI clips the glow to the pane.
    pub(super) pane: Option<[i32; 4]>,
}

impl Default for FrameText {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl FrameText {
    pub(crate) const EMPTY: Self = Self {
        at: None,
        lo: 0,
        rows: Vec::new(),
        pairs: Vec::new(),
        short: Vec::new(),
        glyph: Vec::new(),
        cols: 0,
        col0: 0,
        pane: None,
    };

    /// The ink (`None` for a blank cell) and ground of cell `(row, col)` in
    /// the frame's own cells, or `None` for a cell the frame did not read.
    #[inline]
    pub(crate) const fn pair(&self, row: i32, col: i32) -> Option<(Option<u32>, u32)> {
        let (i, col) = (row - self.lo, col - self.col0);
        if i < 0 || i as usize >= self.rows.len() || col < 0 || col as usize >= self.cols {
            return None;
        }
        let idx = i as usize * self.cols + col as usize;
        let pairs = self.pairs.as_slice();
        if idx < pairs.len() { pairs[idx] } else { None }
    }

    /// Whether window cell `(row, col)` lies outside the pane the frame
    /// handed ([`Self::pane`]); `false` when no frame handed one.
    #[inline]
    pub(crate) const fn past_pane(&self, row: i32, col: i32) -> bool {
        match self.pane {
            Some([r0, r1, c0, c1]) => row < r0 || row >= r1 || col < c0 || col >= c1,
            None => false,
        }
    }

    /// Cell `(row, col)`'s glyph as [`aterm_render::GlyphInk::index`] names
    /// it; [`aterm_render::GlyphInk::OTHER`] for a cell the frame did not read.
    #[inline]
    pub(crate) const fn glyph(&self, row: i32, col: i32) -> u8 {
        let (i, col) = (row - self.lo, col - self.col0);
        if i < 0 || i as usize >= self.rows.len() || col < 0 || col as usize >= self.cols {
            return aterm_render::GlyphInk::OTHER;
        }
        let idx = i as usize * self.cols + col as usize;
        let glyph = self.glyph.as_slice();
        if idx < glyph.len() {
            glyph[idx]
        } else {
            aterm_render::GlyphInk::OTHER
        }
    }

    /// Whether cell `(row, col)`'s glyph keeps under the x-height
    /// ([`super::emit::is_short_glyph`]); `false` for a cell the frame did not
    /// read.
    #[inline]
    pub(crate) const fn short(&self, row: i32, col: i32) -> bool {
        let (i, col) = (row - self.lo, col - self.col0);
        if i < 0 || i as usize >= self.rows.len() || col < 0 || col as usize >= self.cols {
            return false;
        }
        let idx = i as usize * self.cols + col as usize;
        let short = self.short.as_slice();
        idx < short.len() && short[idx]
    }
}

/// Combined text view over the row probe and [`FrameText`].
#[derive(Clone, Copy)]
pub(crate) struct TextView<'a> {
    pub(crate) spans: TextSpans,
    frame: &'a [Option<(i32, i32)>],
    lo: i32,
    /// The frame's cells, whose grounds [`Self::grounds_read_as_page`] reads.
    cells: Option<&'a FrameText>,
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
                cells: Some(f),
            },
            None => Self {
                spans,
                frame: &[],
                lo: 0,
                cells: None,
            },
        }
    }

    /// Whether every cell of row `row` from column `c0` to `c1` the frame
    /// read lies on the page's own ground `page`
    /// ([`super::emit::reads_as_page`]): no selection or powerline bar laid
    /// there. A cell the frame did not read counts as the page's.
    #[inline]
    pub(crate) const fn grounds_read_as_page(
        &self,
        row: i32,
        (c0, c1): (i32, i32),
        page: u32,
    ) -> bool {
        let Some(cells) = self.cells else {
            return true;
        };
        let mut c = c0;
        while c <= c1 {
            if let Some((_, ground)) = cells.pair(row, c)
                && !super::emit::reads_as_page(ground, page)
            {
                return false;
            }
            c += 1;
        }
        true
    }

    /// Whether window cell `(row, col)` lies outside the pane the frame
    /// handed ([`FrameText::past_pane`]); `false` with no frame, or a frame
    /// that handed none.
    #[inline]
    pub(crate) const fn past_pane(&self, row: i32, col: i32) -> bool {
        match self.cells {
            Some(cells) => cells.past_pane(row, col),
            None => false,
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

    /// Whether any column in `c0..=c1` on `row` lies on a line of text the frame shows.
    #[inline]
    pub(crate) const fn covers_col_range(&self, row: i32, (c0, c1): (i32, i32)) -> bool {
        if c0 > c1 {
            return false;
        }
        if self.spans.reads_row(row) {
            return self.spans.covers_col_range(row, (c0, c1));
        }
        match self.frame_row(row) {
            Some(Some((first, end))) => c1 >= first && c0 < end,
            _ => false,
        }
    }

    /// Whether window pixel `(x, y)` lies in a cell on a line of text the frame shows.
    #[inline]
    pub(crate) const fn covers_px(
        &self,
        (x, y): (f32, f32),
        (ox, oy): (f32, f32),
        (inv_cw, inv_ch): (f32, f32),
    ) -> bool {
        let row = ((y - oy) * inv_ch).floor() as i32;
        let col = ((x - ox) * inv_cw).floor() as i32;
        self.covers_cell(row, col)
    }

    /// Whether grid row `row` was read and contains no glyphs.
    #[inline]
    pub(crate) const fn row_is_blank(&self, row: i32) -> bool {
        if self.spans.reads_row(row) {
            self.spans.line_at(row).is_none()
        } else {
            matches!(self.frame_row(row), Some(None))
        }
    }

    /// Whether grid row `row` was read and contains at least one glyph.
    #[inline]
    pub(crate) const fn row_has_text(&self, row: i32) -> bool {
        if self.spans.reads_row(row) {
            self.spans.line_at(row).is_some()
        } else {
            matches!(self.frame_row(row), Some(Some(_)))
        }
    }

    /// Whether the dark streak treats cell `(row, col)` as text.
    #[inline]
    pub(crate) const fn over_text(&self, row: i32, col: i32) -> bool {
        if self.spans.reads_row(row) {
            return self.spans.covers_cell(row, col);
        }
        match self.frame_row(row) {
            Some(Some((first, end))) => col >= first && col < end,
            Some(None) => false,
            None => true,
        }
    }

    /// Whether the dark streak treats any column in `c0..=c1` on `row` as text.
    #[inline]
    pub(crate) const fn over_text_range(&self, row: i32, (c0, c1): (i32, i32)) -> bool {
        if c0 > c1 {
            return false;
        }
        if self.spans.reads_row(row) {
            return self.spans.covers_col_range(row, (c0, c1));
        }
        match self.frame_row(row) {
            Some(Some((first, end))) => c1 >= first && c0 < end,
            Some(None) => false,
            None => true,
        }
    }
}

/// `quads` and `ramps` as a beam's sink ([`CursorGlow::ramp_sink`]), every
/// row laid as its quad where `as_quads` (a test's oracle).
#[cfg(test)]
#[inline]
pub(crate) fn glow_sink<'a>(
    quads: &'a mut Vec<GlowQuad>,
    ramps: &'a mut GlowRamps,
    as_quads: bool,
) -> RampSink<'a> {
    rows_sink(
        quads,
        ramps,
        if as_quads {
            Some(RampCost::NEVER)
        } else {
            None
        },
    )
}

/// `quads` and `ramps` as a beam's sink, its runs laid as ramps where they
/// pay under a test's `cost` where one is set, else the measured one.
#[inline]
pub(crate) fn rows_sink<'a>(
    quads: &'a mut Vec<GlowQuad>,
    ramps: &'a mut GlowRamps,
    cost: Option<RampCost>,
) -> RampSink<'a> {
    let sink = RampSink::new(quads, ramps);
    match cost {
        Some(cost) => sink.costed(cost),
        None => sink,
    }
}
