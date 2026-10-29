// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pane as an Aos task: a terminal checkpoint and the output, resizes and live witnesses after it, replayed under `pane.faithful-replay`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use core::fmt;
use core::time::Duration;
use std::sync::Arc;

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::{EffectDivergence, EffectPort, EffectRequest};
use aterm_aos::replay::Run;
use aterm_aos::task::{Code, EntryRefusal, PausedClass, Schema, StepRefusal};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactId, ArtifactKind, Digest, TaskIdentity};
use aterm_core::config::TerminalConfig;
use aterm_core::grid::{MAX_GRID_COLS, MAX_GRID_ROWS, ResizeLaw, ResizePolicy};
use aterm_core::terminal::{
    CHECKPOINT_CANON_VERSION, CONFIG_CANON_VERSION, CanonFault, ClockReading, CutKind, HolderCut,
    HostOp, PANE_CARRY_CANON_VERSION, POLICY_CANON_VERSION, PaneStepCarry, ShellIntegrationNonce,
    Terminal, TerminalCheckpoint, are_canonical_hyperlink_schemes, policy_canonical,
    policy_from_canonical,
};

pub mod trace;

/// The task's name.
pub const TASK_NAME: &str = "aterm.pane.screen";
/// The schema's name.
pub const SCHEMA_NAME: &str = "pane.checkpoint+output";
/// The contract's name.
pub const CONTRACT_NAME: &str = "pane.faithful-replay";
/// [`PaneCode::ThisBuild`]'s artifact name.
pub const THIS_BUILD_NAME: &str = "pane.this-build";
/// [`PaneCode::DemoClockSkew`]'s artifact name.
pub const DEMO_CLOCK_SKEW_NAME: &str = "pane.demo-clock-skew";
/// The law that every live witness is the replayed screen.
pub const WITNESS_MATCHES: &str = "WitnessMatches";
/// The law that every slice's replies are the live engine's.
pub const REPLIES_MATCH: &str = "RepliesMatch";
/// The law that every slice carries the clock it was parsed at.
pub const CLOCK_RECORDED: &str = "ClockRecorded";
/// The step law a resize to no grid breaks.
pub const RESIZE_GEOMETRY: &str = "pane.resize-geometry";
/// The step law a slice that ends inside a sequence no checkpoint carries breaks.
pub const UNCARRIED_PARTIAL: &str = "pane.uncarried-partial";
/// The history lines a pane state keeps past its screen, as a Rewind keyframe does.
pub const PANE_HISTORY_LINES: usize = 2048;
/// The effect target a slice's replies go out through: the pty.
pub const REPLY_TARGET: u32 = 1;
/// The one operation on [`REPLY_TARGET`]: write the bytes.
pub const REPLY_WRITE: u16 = 1;
/// The domain of a screen's digest ([`screen_digest`]).
pub const SCREEN_DOMAIN: &[u8] = b"aterm-aos-pane-screen-v1\0";
/// How far [`PaneCode::DemoClockSkew`] moves each clock it parses at.
#[cfg(any(test, debug_assertions))]
pub const DEMO_SKEW: Duration = Duration::from_secs(3600);

/// The digest a live witness names a screen by: its `checkpoint_visible()`'s canonical bytes.
#[must_use]
pub fn screen_digest(seen: &TerminalCheckpoint) -> Digest {
    Digest::of(SCREEN_DOMAIN, &[&seen.canonical()])
}

/// The target this build runs on, as `arch-os[-env]`.
#[must_use]
pub fn target_name() -> String {
    let env = if cfg!(target_env = "gnu") {
        "-gnu"
    } else if cfg!(target_env = "musl") {
        "-musl"
    } else if cfg!(target_env = "msvc") {
        "-msvc"
    } else {
        ""
    };
    format!("{}-{}{env}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The task artifact `aterm.pane.screen` v1.
#[must_use]
pub fn task_artifact() -> ArtifactId {
    let mut e = Enc::new();
    e.str("the screen history of one pane: aterm_core::terminal::Terminal fed its recorded output");
    ArtifactId::new(ArtifactKind::Task, TASK_NAME, 1, e.as_bytes())
}

/// The schema artifact `pane.checkpoint+output` v1: the canonical checkpoint and config versions, and the history a state keeps.
#[must_use]
pub fn schema_artifact() -> ArtifactId {
    let mut e = Enc::new();
    e.u32(CHECKPOINT_CANON_VERSION);
    e.u32(CONFIG_CANON_VERSION);
    e.u32(PANE_CARRY_CANON_VERSION);
    e.u32(POLICY_CANON_VERSION);
    e.u64(PANE_HISTORY_LINES as u64);
    e.str(
        "state: canonical TerminalCheckpoint, canonical PaneStepCarry, canonical TerminalConfig, \
         resize policy, host resync, host alternate scroll, cell_px, canonical security Policy, \
         hyperlink schemes, kitty_media_admitted; inputs Bytes|Resize|Witness|CellPx|Host; \
         observations Parsed|Resized|Witnessed|CellPx|Host; replies are effects",
    );
    ArtifactId::new(ArtifactKind::Schema, SCHEMA_NAME, 1, e.as_bytes())
}

/// The profile artifact: the target and the engine's resize law.
#[must_use]
pub fn profile_artifact() -> ArtifactId {
    let target = target_name();
    let mut e = Enc::new();
    e.str(&target);
    e.u64(ResizeLaw::default().header_value());
    ArtifactId::new(
        ArtifactKind::Profile,
        &format!("{target}/aterm-core"),
        1,
        e.as_bytes(),
    )
}

/// The five artifacts a pane task runs under, with `code` running it.
#[must_use]
pub fn pane_identity(code: &PrebuiltPane) -> TaskIdentity {
    TaskIdentity {
        task: task_artifact(),
        code: code.id.clone(),
        contract: FaithfulReplay::artifact(),
        schema: schema_artifact(),
        profile: profile_artifact(),
    }
}

/// What a replay of the pane runs under beside its checkpoint: the session's config, its resize policy and whether its host resyncs an orphaned alternate screen.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneEnv {
    /// The session's terminal config.
    pub config: TerminalConfig,
    /// The policy every recorded resize applies under.
    pub policy: ResizePolicy,
    /// The live host forwards `?1049l` to exit an orphaned alternate screen.
    pub host_resyncs_alt_screen: bool,
    /// The live host's alternate-scroll default (DEC 1007).
    pub host_alternate_scroll: bool,
    /// The host-measured cell box `(width_px, height_px)`, when installed.
    pub cell_px: Option<(u16, u16)>,
    /// The security policy engine document installed by the host, when any.
    pub security_policy: Option<aterm_core::policy::Policy>,
    /// Host-authorized extra OSC 8 URI schemes (lowercased).
    pub hyperlink_schemes: Vec<String>,
    /// Whether non-direct Kitty graphics media (`t=f`, `t=t`, `t=s`) were admitted by the live host.
    pub kitty_media_admitted: bool,
}

/// The task's state: a checkpoint of the engine (history kept to [`PANE_HISTORY_LINES`], the shell nonce carried), its per-slice transient carry, and what it replays under.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneState {
    checkpoint: TerminalCheckpoint,
    carry: PaneStepCarry,
    env: Arc<PaneEnv>,
}

impl PaneState {
    /// The state `checkpoint` (a Rewind keyframe) and `env` name, with pure baseline step carry.
    #[must_use]
    pub fn new(checkpoint: TerminalCheckpoint, env: PaneEnv) -> Self {
        let carry = PaneStepCarry::initial(&checkpoint, &env.config, env.security_policy.as_ref());
        PaneState {
            checkpoint,
            carry,
            env: Arc::new(env),
        }
    }

    /// The state `checkpoint`, explicit `carry` and `env` name.
    #[must_use]
    pub fn with_carry(checkpoint: TerminalCheckpoint, carry: PaneStepCarry, env: PaneEnv) -> Self {
        PaneState {
            checkpoint,
            carry,
            env: Arc::new(env),
        }
    }

    /// Project a live engine onto `schema`'s clock origin (`checkpoint` + [`PaneStepCarry`]), or `None` inside an uncarried partial sequence.
    #[must_use]
    pub fn from_live(term: &Terminal, schema: &PaneSchema, env: PaneEnv) -> Option<Self> {
        let checkpoint = project(term)?;
        let carry = PaneStepCarry::capture(term, schema.origin.monotonic, PANE_HISTORY_LINES);
        Some(Self::with_carry(checkpoint, carry, env))
    }

    /// The checkpoint.
    #[must_use]
    pub const fn checkpoint(&self) -> &TerminalCheckpoint {
        &self.checkpoint
    }

    /// The per-slice transient carry.
    #[must_use]
    pub const fn carry(&self) -> &PaneStepCarry {
        &self.carry
    }

    /// What it replays under.
    #[must_use]
    pub fn env(&self) -> &PaneEnv {
        &self.env
    }

    /// The engine the checkpoint restores, configured as the live one was.
    #[must_use]
    pub fn engine(&self) -> Terminal {
        self.engine_at(ClockReading::now())
    }

    /// The engine the checkpoint restores on `schema`'s clock origin, configured as the live one was.
    #[must_use]
    pub fn engine_on(&self, schema: &PaneSchema) -> Terminal {
        self.engine_at(schema.origin)
    }

    fn engine_at(&self, origin: ClockReading) -> Terminal {
        let mut term = Terminal::replay_from_checkpoint(&self.checkpoint);
        if let Some(nonce) = self.checkpoint.shell_integration_nonce {
            term.authorize_shell_integration(nonce.0);
        }
        term.set_resize_law(ResizeLaw::default());
        term.set_alt_archive_enabled(false);
        term.set_console_host_posture(self.env.policy);
        term.set_host_resyncs_alt_screen(self.env.host_resyncs_alt_screen);
        term.set_host_alternate_scroll_default(self.env.host_alternate_scroll);
        if let Some((w, h)) = self.env.cell_px {
            term.set_cell_pixel_size(w, h);
        }
        term.apply_config_keeping_modes(&self.env.config);
        if let Some(policy) = &self.env.security_policy {
            term.apply_policy_engine(aterm_core::policy::engine::PolicyEngine::new(
                policy.clone(),
            ));
        }
        for scheme in &self.env.hyperlink_schemes {
            let _ = term.authorize_hyperlink_scheme(scheme);
        }
        if self.env.kitty_media_admitted {
            term.set_kitty_file_resolver(|_, _| None);
        }
        term.apply_pane_step_carry(&self.carry, origin.monotonic);
        let _ = term.take_response();
        term
    }
}

/// The projection a pane state keeps of `term`: history bounded to [`PANE_HISTORY_LINES`], the shell nonce and revision carried, or `None` inside a sequence no checkpoint carries.
#[must_use]
pub fn project(term: &Terminal) -> Option<TerminalCheckpoint> {
    let mut checkpoint = term.checkpoint_pane_step(PANE_HISTORY_LINES)?;
    checkpoint.shell_integration_nonce = term
        .shell_integration_nonce_in_use()
        .map(ShellIntegrationNonce);
    checkpoint.shell_integration_rev = term.shell_integration_rev().map(str::to_owned);
    Some(checkpoint)
}

/// The clock a slice was parsed at: its monotonic offset from the schema's origin and its wall time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneClock {
    /// Nanoseconds after the schema's origin.
    pub offset_ns: u64,
    /// Wall-clock epoch milliseconds, when the live engine had them.
    pub wall_ms: Option<u64>,
}

/// One input of the pane task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneInput {
    /// Output the live engine parsed at `clock` (`None` when the recording dropped it).
    Bytes {
        /// The bytes.
        bytes: Arc<[u8]>,
        /// When the live engine parsed them.
        clock: Option<PaneClock>,
    },
    /// A resize the live engine applied.
    Resize {
        /// Rows after it.
        rows: u16,
        /// Columns after it.
        cols: u16,
    },
    /// The live screen at this point ([`screen_digest`] of its `checkpoint_visible()`).
    Witness {
        /// Its digest.
        screen: Digest,
    },
    /// A host-measured cell box `(width, height)` in pixels applied by the live host.
    CellPx {
        /// Cell width in pixels.
        width: u16,
        /// Cell height in pixels.
        height: u16,
    },
    /// A post-seed host-side mutation applied by the live host outside `process_at` (or observed via `program_probe` during an unsigned OSC 7).
    Host {
        /// The host mutation.
        op: HostOp,
        /// Clock reading when time-sensitive (`LeaveAlternateScreen`).
        clock: Option<PaneClock>,
    },
}

/// What a step's replies met at the effect frontier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyMatch {
    /// They are the recorded ones.
    Matched,
    /// They are not ([`EffectDivergence`]).
    Diverged(EffectDivergence),
}

/// What one step makes visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneObservation {
    /// A slice was parsed.
    Parsed {
        /// Its input carried its clock.
        clocked: bool,
        /// The digest of the replies it made ([`EffectRequest::payload_digest`]).
        reply: Digest,
        /// What the replies met.
        replies: ReplyMatch,
    },
    /// The engine was resized.
    Resized {
        /// Rows after it.
        rows: u16,
        /// Columns after it.
        cols: u16,
    },
    /// A witness was compared.
    Witnessed {
        /// The live screen's digest.
        live: Digest,
        /// The replayed screen's, or `None` inside a sequence no checkpoint carries.
        replayed: Option<Digest>,
    },
    /// The host cell-pixel box was updated.
    CellPx {
        /// Cell width in pixels after it.
        width: u16,
        /// Cell height in pixels after it.
        height: u16,
    },
    /// A host-side mutation was applied.
    Host {
        /// The mutation applied.
        op: HostOp,
        /// Whether its input carried a clock.
        clocked: bool,
    },
}

/// The task ABI over a clock origin the host read: every slice's clock is an offset from it.
#[derive(Clone, Debug)]
pub struct PaneSchema {
    id: ArtifactId,
    origin: ClockReading,
}

impl PaneSchema {
    /// The schema whose clock offsets count from `origin`.
    #[must_use]
    pub fn new(origin: ClockReading) -> Self {
        PaneSchema {
            id: schema_artifact(),
            origin,
        }
    }

    /// The clock reading `clock` names on this schema's origin.
    #[must_use]
    pub fn reading(&self, clock: PaneClock) -> ClockReading {
        let mut out =
            advance_monotonic_saturating(self.origin, Duration::from_nanos(clock.offset_ns));
        out.wall_ms = clock.wall_ms;
        out
    }

    /// The reading a slice whose clock the recording dropped parses at: the origin, with no wall time.
    #[must_use]
    pub fn unclocked(&self) -> ClockReading {
        ClockReading {
            monotonic: self.origin.monotonic,
            wall_ms: None,
        }
    }
}

fn advance_monotonic_saturating(mut reading: ClockReading, delta: Duration) -> ClockReading {
    if let Some(sum) = reading.monotonic.checked_add(delta) {
        reading.monotonic = sum;
        return reading;
    }
    for shift in (0..64).rev() {
        if let Some(next) = reading
            .monotonic
            .checked_add(Duration::from_secs(1u64 << shift))
        {
            reading.monotonic = next;
        }
    }
    for shift in (0..30).rev() {
        if let Some(next) = reading
            .monotonic
            .checked_add(Duration::from_nanos(1u64 << shift))
        {
            reading.monotonic = next;
        }
    }
    reading
}

const fn canon_fault(f: &CanonFault) -> WireFault {
    WireFault::NonCanonical { field: f.field }
}

fn encode_policy(e: &mut Enc, p: ResizePolicy) {
    match p {
        ResizePolicy::Native => e.u8(0),
        ResizePolicy::ConPty => e.u8(1),
        other => {
            e.u8(0xFF);
            e.str(&format!("{other:?}"));
        }
    }
}

fn decode_policy(d: &mut Dec<'_>) -> Result<ResizePolicy, WireFault> {
    match d.u8("state.policy")? {
        0 => Ok(ResizePolicy::Native),
        1 => Ok(ResizePolicy::ConPty),
        tag => Err(WireFault::BadTag {
            field: "state.policy",
            tag,
        }),
    }
}

fn encode_host_op(e: &mut Enc, op: &HostOp) {
    match op {
        HostOp::ForegroundJob {
            active,
            probed,
            restore_cwd,
        } => {
            e.u8(0);
            e.bool(*active);
            e.bool(*probed);
            e.bool(*restore_cwd);
        }
        HostOp::HolderCut(HolderCut {
            to,
            kind,
            from_alive,
        }) => {
            e.u8(1);
            e.opt(*to, Enc::u64);
            e.u8(match kind {
                CutKind::Group => 0,
                CutKind::Exec => 1,
            });
            e.bool(*from_alive);
        }
        HostOp::HolderRetire(token) => {
            e.u8(2);
            e.u64(*token);
        }
        HostOp::LeaveAlternateScreen => e.u8(3),
        HostOp::ExpectHostAltLeave => e.u8(4),
        HostOp::ClearScrollback => e.u8(5),
        HostOp::Reset => e.u8(6),
        HostOp::AuthorizeShell(nonce) => {
            e.u8(7);
            e.raw(nonce);
        }
        HostOp::AuthorizeShellOnFirstMark(nonce) => {
            e.u8(8);
            e.raw(nonce);
        }
        HostOp::AuthorizeShellRekey(nonce) => {
            e.u8(9);
            e.raw(nonce);
        }
        HostOp::WithdrawShellRekey(nonce) => {
            e.u8(10);
            e.raw(nonce);
        }
        HostOp::SettleShellRekey(nonce) => {
            e.u8(11);
            e.raw(nonce);
        }
        HostOp::AdvanceKittyAnimations => e.u8(12),
    }
}

fn decode_nonce(d: &mut Dec<'_>, field: &'static str) -> Result<[u8; 32], WireFault> {
    let bytes = d.raw(field, 32)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

fn decode_host_op(d: &mut Dec<'_>) -> Result<HostOp, WireFault> {
    match d.u8("input.host.op")? {
        0 => {
            let active = d.bool("input.host.active")?;
            let probed = d.bool("input.host.probed")?;
            let restore_cwd = d.bool("input.host.restore_cwd")?;
            if (active && probed) || (restore_cwd && (active || probed)) {
                return Err(WireFault::NonCanonical {
                    field: "input.host.foreground_job",
                });
            }
            Ok(HostOp::ForegroundJob {
                active,
                probed,
                restore_cwd,
            })
        }
        1 => Ok(HostOp::HolderCut(HolderCut {
            to: d.opt("input.host.to", |d| d.u64("input.host.to"))?,
            kind: match d.u8("input.host.kind")? {
                0 => CutKind::Group,
                1 => CutKind::Exec,
                tag => {
                    return Err(WireFault::BadTag {
                        field: "input.host.kind",
                        tag,
                    });
                }
            },
            from_alive: d.bool("input.host.from_alive")?,
        })),
        2 => Ok(HostOp::HolderRetire(d.u64("input.host.token")?)),
        3 => Ok(HostOp::LeaveAlternateScreen),
        4 => Ok(HostOp::ExpectHostAltLeave),
        5 => Ok(HostOp::ClearScrollback),
        6 => Ok(HostOp::Reset),
        7 => Ok(HostOp::AuthorizeShell(decode_nonce(d, "input.host.nonce")?)),
        8 => Ok(HostOp::AuthorizeShellOnFirstMark(decode_nonce(
            d,
            "input.host.nonce",
        )?)),
        9 => Ok(HostOp::AuthorizeShellRekey(decode_nonce(
            d,
            "input.host.nonce",
        )?)),
        10 => Ok(HostOp::WithdrawShellRekey(decode_nonce(
            d,
            "input.host.nonce",
        )?)),
        11 => Ok(HostOp::SettleShellRekey(decode_nonce(
            d,
            "input.host.nonce",
        )?)),
        12 => Ok(HostOp::AdvanceKittyAnimations),
        tag => Err(WireFault::BadTag {
            field: "input.host.op",
            tag,
        }),
    }
}

fn encode_divergence(e: &mut Enc, d: &EffectDivergence) {
    match *d {
        EffectDivergence::Inserted { position } => {
            e.u8(0);
            e.u64(position);
        }
        EffectDivergence::TargetDiverged {
            position,
            recorded,
            asked,
        } => {
            e.u8(1);
            e.u64(position);
            e.u32(recorded);
            e.u32(asked);
        }
        EffectDivergence::OpDiverged {
            position,
            recorded,
            asked,
        } => {
            e.u8(2);
            e.u64(position);
            e.u16(recorded);
            e.u16(asked);
        }
        EffectDivergence::PayloadDiverged {
            position,
            recorded,
            asked,
        } => {
            e.u8(3);
            e.u64(position);
            e.digest(&recorded);
            e.digest(&asked);
        }
        EffectDivergence::Unreached {
            position,
            remaining,
        } => {
            e.u8(4);
            e.u64(position);
            e.u64(remaining);
        }
        EffectDivergence::Undeclared { target } => {
            e.u8(5);
            e.u32(target);
        }
    }
}

impl Schema for PaneSchema {
    type State = PaneState;
    type Input = PaneInput;
    type Observation = PaneObservation;

    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn encode_state(&self, s: &PaneState, e: &mut Enc) {
        e.bytes(&s.checkpoint.canonical());
        e.bytes(&s.carry.canonical());
        let PaneEnv {
            config,
            policy,
            host_resyncs_alt_screen,
            host_alternate_scroll,
            cell_px,
            security_policy,
            hyperlink_schemes,
            kitty_media_admitted,
        } = &*s.env;
        e.bytes(&config.canonical());
        encode_policy(e, *policy);
        e.bool(*host_resyncs_alt_screen);
        e.bool(*host_alternate_scroll);
        e.opt(*cell_px, |e, (w, h)| {
            e.u16(w);
            e.u16(h);
        });
        e.opt(security_policy.as_ref(), |e, p| {
            e.bytes(&policy_canonical(p));
        });
        e.u64(u64::try_from(hyperlink_schemes.len()).unwrap_or(u64::MAX));
        for scheme in hyperlink_schemes {
            e.str(scheme);
        }
        e.bool(*kitty_media_admitted);
    }

    fn decode_state(&self, d: &mut Dec<'_>) -> Result<PaneState, WireFault> {
        let checkpoint = TerminalCheckpoint::from_canonical(d.bytes("state.checkpoint")?)
            .map_err(|f| canon_fault(&f))?;
        let carry =
            PaneStepCarry::from_canonical(d.bytes("state.carry")?).map_err(|f| canon_fault(&f))?;
        let config = TerminalConfig::from_canonical(d.bytes("state.config")?)
            .map_err(|f| canon_fault(&f))?;
        let policy = decode_policy(d)?;
        let host_resyncs_alt_screen = d.bool("state.host_resyncs_alt_screen")?;
        let host_alternate_scroll = d.bool("state.host_alternate_scroll")?;
        let cell_px = d.opt("state.cell_px", |d| {
            let w = d.u16("state.cell_px.width")?;
            let h = d.u16("state.cell_px.height")?;
            if w == 0 || h == 0 {
                return Err(WireFault::NonCanonical {
                    field: "state.cell_px",
                });
            }
            Ok((w, h))
        })?;
        let security_policy = d.opt("state.security_policy", |d| {
            policy_from_canonical(d.bytes("state.security_policy")?).map_err(|f| canon_fault(&f))
        })?;
        if !carry.matches_policy(security_policy.as_ref()) {
            return Err(WireFault::NonCanonical {
                field: "state.carry.policy_buckets",
            });
        }
        if carry.inactive_handback.is_some() != checkpoint.alt_cursor.is_some() {
            return Err(WireFault::NonCanonical {
                field: "state.carry.inactive_handback",
            });
        }
        if carry.awaiting_shell_nonce.is_some() && checkpoint.shell_integration_nonce.is_some() {
            return Err(WireFault::NonCanonical {
                field: "state.carry.awaiting_shell_nonce",
            });
        }
        if carry.dcs_query.is_some() && (checkpoint.parser.is_some() || checkpoint.parser_ground) {
            return Err(WireFault::NonCanonical {
                field: "state.carry.dcs_query",
            });
        }
        let n_schemes = d.count("state.hyperlink_schemes", 4)?;
        let mut hyperlink_schemes = Vec::with_capacity(n_schemes);
        for _ in 0..n_schemes {
            hyperlink_schemes.push(d.str("state.hyperlink_schemes.item")?.to_owned());
        }
        if !are_canonical_hyperlink_schemes(&hyperlink_schemes) {
            return Err(WireFault::NonCanonical {
                field: "state.hyperlink_schemes",
            });
        }
        let kitty_media_admitted = d.bool("state.kitty_media_admitted")?;
        Ok(PaneState::with_carry(
            checkpoint,
            carry,
            PaneEnv {
                config,
                policy,
                host_resyncs_alt_screen,
                host_alternate_scroll,
                cell_px,
                security_policy,
                hyperlink_schemes,
                kitty_media_admitted,
            },
        ))
    }

    fn encode_input(&self, i: &PaneInput, e: &mut Enc) {
        match i {
            PaneInput::Bytes { bytes, clock } => {
                e.u8(0);
                e.bytes(bytes);
                e.opt(clock.as_ref(), |e, c| {
                    e.u64(c.offset_ns);
                    e.opt(c.wall_ms, Enc::u64);
                });
            }
            PaneInput::Resize { rows, cols } => {
                e.u8(1);
                e.u16(*rows);
                e.u16(*cols);
            }
            PaneInput::Witness { screen } => {
                e.u8(2);
                e.digest(screen);
            }
            PaneInput::CellPx { width, height } => {
                e.u8(3);
                e.u16(*width);
                e.u16(*height);
            }
            PaneInput::Host { op, clock } => {
                e.u8(4);
                encode_host_op(e, op);
                e.opt(clock.as_ref(), |e, c| {
                    e.u64(c.offset_ns);
                    e.opt(c.wall_ms, Enc::u64);
                });
            }
        }
    }

    fn decode_input(&self, d: &mut Dec<'_>) -> Result<PaneInput, WireFault> {
        match d.u8("input.kind")? {
            0 => Ok(PaneInput::Bytes {
                bytes: Arc::from(d.bytes("input.bytes")?),
                clock: d.opt("input.clock", |d| {
                    Ok(PaneClock {
                        offset_ns: d.u64("input.clock.offset_ns")?,
                        wall_ms: d.opt("input.clock.wall_ms", |d| d.u64("input.clock.wall_ms"))?,
                    })
                })?,
            }),
            1 => Ok(PaneInput::Resize {
                rows: d.u16("input.rows")?,
                cols: d.u16("input.cols")?,
            }),
            2 => Ok(PaneInput::Witness {
                screen: d.digest("input.screen")?,
            }),
            3 => Ok(PaneInput::CellPx {
                width: d.u16("input.width")?,
                height: d.u16("input.height")?,
            }),
            4 => Ok(PaneInput::Host {
                op: decode_host_op(d)?,
                clock: d.opt("input.host.clock", |d| {
                    Ok(PaneClock {
                        offset_ns: d.u64("input.host.clock.offset_ns")?,
                        wall_ms: d.opt("input.host.clock.wall_ms", |d| {
                            d.u64("input.host.clock.wall_ms")
                        })?,
                    })
                })?,
            }),
            tag => Err(WireFault::BadTag {
                field: "input.kind",
                tag,
            }),
        }
    }

    fn encode_observation(&self, o: &PaneObservation, e: &mut Enc) {
        match o {
            PaneObservation::Parsed {
                clocked,
                reply,
                replies,
            } => {
                e.u8(0);
                e.bool(*clocked);
                e.digest(reply);
                match replies {
                    ReplyMatch::Matched => e.u8(0),
                    ReplyMatch::Diverged(d) => {
                        e.u8(1);
                        encode_divergence(e, d);
                    }
                }
            }
            PaneObservation::Resized { rows, cols } => {
                e.u8(1);
                e.u16(*rows);
                e.u16(*cols);
            }
            PaneObservation::Witnessed { live, replayed } => {
                e.u8(2);
                e.digest(live);
                e.opt(replayed.as_ref(), Enc::digest);
            }
            PaneObservation::CellPx { width, height } => {
                e.u8(3);
                e.u16(*width);
                e.u16(*height);
            }
            PaneObservation::Host { op, clocked } => {
                e.u8(4);
                encode_host_op(e, op);
                e.bool(*clocked);
            }
        }
    }

    fn paused_class(&self, _i: &PaneInput) -> PausedClass {
        PausedClass::Queue
    }
}

/// The step functions compiled into this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneCode {
    /// `Terminal::replay_from_checkpoint`, then `process_at` at the recorded clock: the engine this build ships.
    ThisBuild,
    /// **An intentionally introduced demonstration fault, not a defect of shipped code**: it parses every slice an hour past its recorded clock.
    #[cfg(any(test, debug_assertions))]
    DemoClockSkew,
}

impl PaneCode {
    /// Every implementation in this build.
    pub const ALL: &'static [PaneCode] = &[
        PaneCode::ThisBuild,
        #[cfg(any(test, debug_assertions))]
        PaneCode::DemoClockSkew,
    ];

    /// The code artifact's name.
    #[must_use]
    pub const fn artifact_name(self) -> &'static str {
        match self {
            PaneCode::ThisBuild => THIS_BUILD_NAME,
            #[cfg(any(test, debug_assertions))]
            PaneCode::DemoClockSkew => DEMO_CLOCK_SKEW_NAME,
        }
    }

    /// The short name a verb takes (`this-build`, `demo-clock-skew`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            PaneCode::ThisBuild => "this-build",
            #[cfg(any(test, debug_assertions))]
            PaneCode::DemoClockSkew => "demo-clock-skew",
        }
    }

    const fn note(self) -> &'static str {
        match self {
            PaneCode::ThisBuild => {
                "Terminal::replay_from_checkpoint of the state, configured as the live engine, then \
                 process_at the recorded clock; replies go out as effects"
            }
            #[cfg(any(test, debug_assertions))]
            PaneCode::DemoClockSkew => {
                "an intentionally introduced demonstration fault, not a defect of shipped code: \
                 process_at an hour past the recorded clock"
            }
        }
    }

    /// Its artifact under `build`.
    #[must_use]
    pub fn artifact(self, build: &str) -> ArtifactId {
        let mut e = Enc::new();
        e.str(self.artifact_name());
        e.str(build);
        e.str(self.note());
        ArtifactId::new(ArtifactKind::Code, self.artifact_name(), 1, e.as_bytes())
    }

    /// The reading a slice clocked `clock` parses at under this code.
    fn reading(self, schema: &PaneSchema, clock: Option<PaneClock>) -> ClockReading {
        let reading = clock.map_or_else(|| schema.unclocked(), |c| schema.reading(c));
        match self {
            PaneCode::ThisBuild => reading,
            #[cfg(any(test, debug_assertions))]
            PaneCode::DemoClockSkew => {
                let skew = u64::try_from(DEMO_SKEW.as_millis()).unwrap_or(u64::MAX);
                let mut skewed = advance_monotonic_saturating(reading, DEMO_SKEW);
                skewed.wall_ms = reading.wall_ms.map(|ms| ms.saturating_add(skew));
                skewed
            }
        }
    }
}

/// A [`PaneCode`] with its artifact under one build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrebuiltPane {
    /// The implementation.
    pub code: PaneCode,
    /// Its artifact.
    pub id: ArtifactId,
}

impl PrebuiltPane {
    /// `code` under `build`.
    #[must_use]
    pub fn new(code: PaneCode, build: &str) -> Self {
        PrebuiltPane {
            code,
            id: code.artifact(build),
        }
    }
}

const fn refusal(law: &'static str, detail: String) -> StepRefusal {
    StepRefusal { law, detail }
}

impl Code<PaneSchema> for PrebuiltPane {
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn entry(&self, _schema: &PaneSchema, state: &PaneState) -> Result<(), EntryRefusal> {
        let c = &state.checkpoint;
        if c.rows == 0 || c.cols == 0 || c.rows > MAX_GRID_ROWS || c.cols > MAX_GRID_COLS {
            return Err(EntryRefusal {
                law: RESIZE_GEOMETRY,
                detail: format!("{}x{}", c.rows, c.cols),
            });
        }
        Ok(())
    }

    fn step(
        &self,
        schema: &PaneSchema,
        state: &mut PaneState,
        input: &PaneInput,
        port: &mut dyn EffectPort,
    ) -> Result<PaneObservation, StepRefusal> {
        match input {
            PaneInput::Bytes { bytes, clock } => {
                let mut term = state.engine_on(schema);
                term.process_at(bytes, self.code.reading(schema, *clock));
                let request = EffectRequest {
                    target: REPLY_TARGET,
                    op: REPLY_WRITE,
                    payload: term.take_response().unwrap_or_default(),
                };
                let reply = request.payload_digest();
                let replies = match port.submit(request) {
                    Ok(_) => ReplyMatch::Matched,
                    Err(d) => ReplyMatch::Diverged(d),
                };
                state.checkpoint = project(&term).ok_or_else(|| {
                    refusal(
                        UNCARRIED_PARTIAL,
                        format!(
                            "the slice ends inside {}, which no checkpoint carries",
                            term.partial_sequence_uncarried().unwrap_or("a sequence")
                        ),
                    )
                })?;
                state.carry =
                    PaneStepCarry::capture(&term, schema.origin.monotonic, PANE_HISTORY_LINES);
                Ok(PaneObservation::Parsed {
                    clocked: clock.is_some(),
                    reply,
                    replies,
                })
            }
            PaneInput::Resize { rows, cols } => {
                if *rows == 0 || *cols == 0 || *rows > MAX_GRID_ROWS || *cols > MAX_GRID_COLS {
                    return Err(refusal(RESIZE_GEOMETRY, format!("{rows}x{cols}")));
                }
                let mut term = state.engine_on(schema);
                term.resize_with_policy(*rows, *cols, state.env.policy);
                let _ = term.take_reports();
                state.checkpoint = project(&term).ok_or_else(|| {
                    refusal(
                        UNCARRIED_PARTIAL,
                        "a resize inside a partial sequence".into(),
                    )
                })?;
                state.carry =
                    PaneStepCarry::capture(&term, schema.origin.monotonic, PANE_HISTORY_LINES);
                Ok(PaneObservation::Resized {
                    rows: *rows,
                    cols: *cols,
                })
            }
            PaneInput::Witness { screen } => {
                let replayed = state
                    .engine_on(schema)
                    .checkpoint_visible()
                    .map(|seen| screen_digest(&seen));
                Ok(PaneObservation::Witnessed {
                    live: *screen,
                    replayed,
                })
            }
            PaneInput::CellPx { width, height } => {
                if *width == 0 || *height == 0 {
                    return Err(refusal(RESIZE_GEOMETRY, format!("{width}x{height}px")));
                }
                let mut term = state.engine_on(schema);
                term.set_cell_pixel_size(*width, *height);
                let _ = term.take_reports();
                let checkpoint = project(&term).ok_or_else(|| {
                    refusal(
                        UNCARRIED_PARTIAL,
                        "a cell-px update inside a partial sequence".into(),
                    )
                })?;
                let carry =
                    PaneStepCarry::capture(&term, schema.origin.monotonic, PANE_HISTORY_LINES);
                Arc::make_mut(&mut state.env).cell_px = Some((*width, *height));
                state.checkpoint = checkpoint;
                state.carry = carry;
                Ok(PaneObservation::CellPx {
                    width: *width,
                    height: *height,
                })
            }
            PaneInput::Host { op, clock } => {
                let mut term = state.engine_on(schema);
                term.apply_host_op_at(*op, self.code.reading(schema, *clock));
                state.checkpoint = project(&term).ok_or_else(|| {
                    refusal(
                        UNCARRIED_PARTIAL,
                        "a host mutation inside a partial sequence".into(),
                    )
                })?;
                state.carry =
                    PaneStepCarry::capture(&term, schema.origin.monotonic, PANE_HISTORY_LINES);
                Ok(PaneObservation::Host {
                    op: *op,
                    clocked: clock.is_some(),
                })
            }
        }
    }
}

/// `pane.faithful-replay` v1: every live witness is the replayed screen ([`WITNESS_MATCHES`]), every slice's replies are the live engine's ([`REPLIES_MATCH`]), and every slice carries its clock ([`CLOCK_RECORDED`]).
#[derive(Clone, Debug)]
pub struct FaithfulReplay {
    id: ArtifactId,
}

impl Default for FaithfulReplay {
    fn default() -> Self {
        Self::new()
    }
}

impl FaithfulReplay {
    /// The contract.
    #[must_use]
    pub fn new() -> Self {
        FaithfulReplay {
            id: Self::artifact(),
        }
    }

    /// Its artifact: the descriptor names the laws.
    #[must_use]
    pub fn artifact() -> ArtifactId {
        let mut e = Enc::new();
        for law in [WITNESS_MATCHES, REPLIES_MATCH, CLOCK_RECORDED] {
            e.str(law);
        }
        e.str("full equality of the canonical visible checkpoint");
        ArtifactId::new(ArtifactKind::Contract, CONTRACT_NAME, 1, e.as_bytes())
    }
}

const fn fail(cursor: u64, law: &'static str, detail: String) -> Verdict {
    Verdict::Fail {
        cursor,
        law,
        detail,
    }
}

const fn kind(i: &PaneInput) -> &'static str {
    match i {
        PaneInput::Bytes { .. } => "bytes",
        PaneInput::Resize { .. } => "resize",
        PaneInput::Witness { .. } => "witness",
        PaneInput::CellPx { .. } => "cell_px",
        PaneInput::Host { .. } => "host",
    }
}

impl Contract<PaneSchema> for FaithfulReplay {
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn judge(&self, _schema: &PaneSchema, inputs: &[PaneInput], run: &Run<PaneSchema>) -> Verdict {
        for ((cursor, obs), input) in run.observations.iter().zip(inputs) {
            let cursor = *cursor;
            match (input, obs) {
                (
                    PaneInput::Bytes { .. },
                    PaneObservation::Parsed {
                        clocked, replies, ..
                    },
                ) => {
                    if !clocked {
                        return fail(
                            cursor,
                            CLOCK_RECORDED,
                            "the slice carries no clock; it was parsed at the origin".into(),
                        );
                    }
                    if let ReplyMatch::Diverged(d) = replies {
                        return fail(cursor, REPLIES_MATCH, d.to_string());
                    }
                }
                (PaneInput::Resize { .. }, PaneObservation::Resized { .. })
                | (PaneInput::CellPx { .. }, PaneObservation::CellPx { .. }) => {}
                (
                    PaneInput::Host { op: expected, .. },
                    PaneObservation::Host { op: seen, clocked },
                ) if expected == seen => {
                    if !clocked {
                        match expected {
                            HostOp::LeaveAlternateScreen => {
                                return fail(
                                    cursor,
                                    CLOCK_RECORDED,
                                    "the host alternate-screen leave carries no clock; it ran at the origin"
                                        .into(),
                                );
                            }
                            HostOp::AdvanceKittyAnimations => {
                                return fail(
                                    cursor,
                                    CLOCK_RECORDED,
                                    "the Kitty animation step carries no clock; it ran at the origin"
                                        .into(),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                (PaneInput::Witness { .. }, PaneObservation::Witnessed { live, replayed }) => {
                    if *replayed != Some(*live) {
                        return fail(
                            cursor,
                            WITNESS_MATCHES,
                            format!(
                                "the live screen is {}, the replay's {}",
                                live.short(),
                                replayed.map_or_else(
                                    || "inside a partial sequence".into(),
                                    |d| d.short()
                                )
                            ),
                        );
                    }
                }
                (input, obs) => {
                    return fail(
                        cursor,
                        WITNESS_MATCHES,
                        format!("a {} input observed {obs:?}", kind(input)),
                    );
                }
            }
        }
        if let Some((cursor, r)) = &run.refused {
            return fail(*cursor, WITNESS_MATCHES, format!("the replay stopped: {r}"));
        }
        if run.observations.len() != inputs.len() {
            let cursor = run.observations.get(inputs.len()).map_or_else(
                || {
                    run.observations
                        .last()
                        .map_or(0, |(c, _)| c.saturating_add(1))
                },
                |(c, _)| *c,
            );
            return fail(
                cursor,
                WITNESS_MATCHES,
                format!(
                    "the run observed {} of {} inputs",
                    run.observations.len(),
                    inputs.len()
                ),
            );
        }
        Verdict::Pass
    }
}

/// How many inputs of each kind a run's inputs hold, and how many witnesses it matched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneCounts {
    /// Slices.
    pub bytes: usize,
    /// Resizes.
    pub resizes: usize,
    /// Witnesses.
    pub witnesses: usize,
    /// Witnesses whose replayed screen was the live one.
    pub matched: usize,
}

impl PaneCounts {
    /// The counts over `inputs` and the witnesses `run` matched.
    #[must_use]
    pub fn of(inputs: &[PaneInput], run: &Run<PaneSchema>) -> Self {
        let mut c = PaneCounts::default();
        for i in inputs {
            let slot = match i {
                PaneInput::Bytes { .. } => &mut c.bytes,
                PaneInput::Resize { .. } => &mut c.resizes,
                PaneInput::Witness { .. } => &mut c.witnesses,
                PaneInput::CellPx { .. } | PaneInput::Host { .. } => continue,
            };
            *slot = slot.saturating_add(1);
        }
        c.matched = run
            .observations
            .iter()
            .filter(|(_, o)| {
                matches!(o, PaneObservation::Witnessed { live, replayed } if *replayed == Some(*live))
            })
            .count();
        c
    }
}

impl fmt::Display for PaneCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "slices={} resizes={} witnesses={} matched={}",
            self.bytes, self.resizes, self.witnesses, self.matched
        )
    }
}

#[cfg(test)]
mod tests;
