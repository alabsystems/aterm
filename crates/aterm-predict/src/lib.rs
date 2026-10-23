// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Speculative local-echo state machine (`aterm-predict`), shared by `aterm-gui` and web embedders.

use aterm_time::Instant;
use std::collections::VecDeque;
use std::time::Duration;

const MAX_PREDICTIONS: usize = 256;
const MAX_GLYPH_OWES: usize = 64;
const EDIT_CHAIN_MAX: usize = 64;

/// Controls how aggressively predicted echo glyphs are displayed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PredictMode {
    /// Never display predictions (default).
    #[default]
    Off,
    /// Display predictions only when measured echo RTT exceeds the threshold and the epoch is proven.
    Adaptive,
    /// Always display predictions immediately.
    Always,
}

macro_rules! rules {
    ($(#[$meta:meta])* pub enum Rule { $($(#[$vmeta:meta])* $variant:ident,)* }) => {
        $(#[$meta])*
        pub enum Rule {
            $($(#[$vmeta])* $variant,)*
        }

        impl Rule {
            /// All negative-control rules in declaration order.
            #[cfg(any(test, debug_assertions))]
            pub const ALL: &'static [Rule] = &[$(Rule::$variant,)*];
        }
    };
}

rules! {
    /// Negative-control rule toggleable via [`Predictor::disable_rule_for_test`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Rule {
        ExpiryOwes,
        TailGuard,
        TypedOwes,
        TypedOwesSameGlyph,
        TypedOwesCleanTail,
        CancelTransit,
        CursorAdvance,
        StandingTail,
        CancelOwes,
        UncreditedVoids,
        SmallestMatch,
        NetNullUncredited,
        VacatedBlankInMatch,
        WholeStateMatch,
        MotionClassProof,
        ProofsEndWithTheEpoch,
        MotionsNeverProve,
        TextEdges,
        EditOpsOwe,
        StateExpiryFlushes,
        CarryCursor,
        VacateRefusal,
        StaleAnchor,
        CursorReach,
        ParkedAtTheEnd,
        ChainAttributedWhole,
        MarkedEcho,
        TransitReach,
        EditOwedAfter,
        SpaceNeverProves,
        InsertShiftProof,
        EraseShiftProof,
        ShiftProofDistinguishes,
        MidLineDeclineFlushes,
        RightToLeftRows,
        InPlaceReadsFewer,
        CancelsOnlyKnownShifts,
        InsideRunsNeverProve,
        StandingGlyph,
        CancelOwesItsBlank,
        PartWayEchoes,
        WitnessFreshPrompt,
        WitnessFirstKey,
        LearnedBindings,
        EndRefusal,
        OpaqueHead,
        DarkBehindAWait,
        FloorBelow,
        CreditTakenBack,
        EngineWrapFlag,
        StaleCancel,
        PartWayStepBack,
        TransitBlanks,
        ScrollKeepsFirstKey,
        ScreenAsItWas,
        CleanTailInFlight,
        DarkOverText,
    }
}

impl Rule {
    #[cfg(any(test, debug_assertions))]
    #[inline]
    const fn bit(self) -> u64 {
        1 << self as u32
    }
}

#[cfg(any(test, debug_assertions))]
const _: () = assert!(Rule::ALL.len() <= 64);

impl PredictMode {
    /// Parses a [`PredictMode`] config string (case-insensitive), defaulting to [`Self::Off`].
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        let any = |opts: &[&str]| opts.iter().any(|o| s.eq_ignore_ascii_case(o));
        if any(&["adaptive", "auto", "on", "true"]) {
            Self::Adaptive
        } else if any(&["always", "force"]) {
            Self::Always
        } else {
            Self::Off
        }
    }
}

/// Per-session learned echo RTT estimate and in-flight owed-echo custody.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinkEstimate {
    srtt: Option<f32>,
    slow_samples: u8,
    fast_samples: u8,
    adaptive_slow: bool,
    expiry_backoff: u8,
    owed_at: Option<Instant>,
    bindings: EditorBindings,
}

/// Session, shell-integration nonce, and alt-screen state scoping learned line-editor bindings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorContext {
    session: u64,
    nonce: u64,
    cursor_keys: bool,
    escaped_chords: bool,
}

impl EditorContext {
    /// Constructs an [`EditorContext`] for `session`, optional capability `nonce`, and `alt_screen`.
    #[must_use]
    pub fn new(
        session: u64,
        nonce: Option<&[u8; 32]>,
        cursor_keys_application: bool,
        escaped_chords: bool,
    ) -> Self {
        let nonce = nonce.map_or(0, |bytes| {
            bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
                (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
            })
        });
        Self {
            session,
            nonce,
            cursor_keys: cursor_keys_application,
            escaped_chords,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Learned {
    /// Home (`ESC [ H` or `ESC O H`), learned as beginning-of-line.
    Home,
    /// End (`ESC [ F` or `ESC O F`), learned as end-of-line.
    End,
    /// Ctrl-A (`SOH`), learned as beginning-of-line.
    CtrlA,
    /// Ctrl-E (`ENQ`), learned as end-of-line.
    CtrlE,
    /// Delete (`ESC [ 3 ~`), learned as forward delete-char.
    Delete,
}

impl Learned {
    const ALL: [Self; 5] = [
        Self::Home,
        Self::End,
        Self::CtrlA,
        Self::CtrlE,
        Self::Delete,
    ];

    #[inline]
    const fn of(key: EditKey) -> Option<Self> {
        match key {
            EditKey::Home => Some(Self::Home),
            EditKey::End => Some(Self::End),
            EditKey::CtrlA => Some(Self::CtrlA),
            EditKey::CtrlE => Some(Self::CtrlE),
            EditKey::Delete => Some(Self::Delete),
            EditKey::Left | EditKey::Right | EditKey::Backspace => None,
        }
    }

    #[cfg(any(test, debug_assertions))]
    #[inline]
    const fn key(self) -> EditKey {
        match self {
            Self::Home => EditKey::Home,
            Self::End => EditKey::End,
            Self::CtrlA => EditKey::CtrlA,
            Self::CtrlE => EditKey::CtrlE,
            Self::Delete => EditKey::Delete,
        }
    }

    #[inline]
    fn index(self) -> usize {
        Self::ALL.iter().position(|&k| k == self).unwrap_or(0)
    }

    #[inline]
    const fn op(self) -> Op {
        match self {
            Self::Home | Self::CtrlA => Op::LineStart(self),
            Self::End | Self::CtrlE => Op::LineEnd(self),
            Self::Delete => Op::Delete,
        }
    }

    #[inline]
    const fn chord(self) -> bool {
        matches!(self, Self::CtrlA | Self::CtrlE)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Binding {
    seen: u8,
    contradictions: u8,
}

const BINDING_LEARNED: u8 = 2;
const BINDING_BANNED: u8 = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EditorBindings {
    context: Option<EditorContext>,
    keys: [Binding; 5],
}

impl EditorBindings {
    #[inline]
    fn get(&self, key: Learned) -> Binding {
        self.keys.get(key.index()).copied().unwrap_or_default()
    }

    #[inline]
    fn learned(&self, key: Learned) -> bool {
        self.get(key).seen >= BINDING_LEARNED
    }

    #[inline]
    fn banned(&self, key: Learned) -> bool {
        self.get(key).contradictions >= BINDING_BANNED
    }
}

impl LinkEstimate {
    /// Returns `true` if at least one RTT sample has been recorded.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn has_sample(&self) -> bool {
        self.srtt.is_some() || self.slow_samples > 0 || self.fast_samples > 0
    }
}

/// Predicted glyph at an absolute grid cell awaiting output confirmation.
#[derive(Clone, Copy, Debug)]
pub struct Prediction {
    /// Grid row of the prediction.
    pub row: u16,
    /// Grid column of the prediction.
    pub col: u16,
    /// Predicted character.
    pub ch: char,
    born: Instant,
    epoch: u64,
    owed: Option<Instant>,
    seq: u64,
    op: Op,
    class: OpClass,
    stale: bool,
    ghost: GhostCell,
    probe: bool,
}

/// Visual role of a cell in a state chain's predicted overlay ([`Prediction::ghost`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhostCell {
    /// Newly typed glyph awaiting echo (painted dimmed).
    Typed,
    /// Existing glyph shifted from column `from` on the same row.
    Moved { from: u16 },
    /// Cell vacated by an erase past the end of the line's text.
    Vacated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Insert,
    /// Left arrow (`ESC [ D` or `ESC O D`).
    Left,
    /// Right arrow (`ESC [ C` or `ESC O C`).
    Right,
    Erase,
    LineStart(Learned),
    LineEnd(Learned),
    Delete,
}

impl Op {
    #[inline]
    const fn learned(self) -> Option<Learned> {
        match self {
            Self::LineStart(k) | Self::LineEnd(k) => Some(k),
            Self::Delete => Some(Learned::Delete),
            Self::Insert | Self::Left | Self::Right | Self::Erase => None,
        }
    }

    #[inline]
    const fn erases(self) -> bool {
        matches!(self, Self::Erase | Self::Delete)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpClass {
    Glyph,
    Motion,
    InsertShift,
    EraseShift,
    LineEdge,
    DeleteFwd,
}

impl OpClass {
    #[inline]
    const fn proven_bit(self) -> u8 {
        match self {
            Self::Glyph | Self::LineEdge | Self::DeleteFwd => 0,
            Self::Motion => PROVEN_MOTION,
            Self::InsertShift => PROVEN_INSERT_SHIFT,
            Self::EraseShift => PROVEN_ERASE_SHIFT,
        }
    }
}

const PROVEN_MOTION: u8 = 1;
const PROVEN_INSERT_SHIFT: u8 = 2;
const PROVEN_ERASE_SHIFT: u8 = 4;

const EDIT_OP_CH: char = '\0';

const NO_SOURCE: u16 = u16::MAX;

impl Prediction {
    /// Constructs a [`GhostCell::Typed`] prediction at `(row, col)` for host painter tests.
    pub fn test_at(row: u16, col: u16, ch: char) -> Self {
        Self {
            row,
            col,
            ch,
            born: Instant::now(),
            epoch: 0,
            owed: None,
            seq: 0,
            op: Op::Insert,
            class: OpClass::Glyph,
            stale: false,
            ghost: GhostCell::Typed,
            probe: false,
        }
    }

    /// Constructs a prediction at `(row, col)` with explicit `ghost` role for host painter tests.
    #[must_use]
    pub fn test_ghost(row: u16, col: u16, ch: char, ghost: GhostCell) -> Self {
        Self {
            ghost,
            ..Self::test_at(row, col, ch)
        }
    }

    /// Returns the [`GhostCell`] role of this prediction in the overlay.
    #[must_use]
    #[inline]
    pub const fn ghost(&self) -> GhostCell {
        self.ghost
    }

    /// Returns the press-order sequence number of this prediction.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn seq_for_test(&self) -> u64 {
        self.seq
    }

    /// Returns the [`EditKey`] this pending op was armed for, or `None` for a typed glyph.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn edit_key_for_test(&self) -> Option<EditKey> {
        match self.op {
            Op::Insert => None,
            Op::Left => Some(EditKey::Left),
            Op::Right => Some(EditKey::Right),
            Op::Erase => Some(EditKey::Backspace),
            Op::LineStart(k) | Op::LineEnd(k) => Some(k.key()),
            Op::Delete => Some(EditKey::Delete),
        }
    }

    /// Returns `true` if this pending op is an unlearned dark probe.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn probe_for_test(&self) -> bool {
        self.probe
    }
}

/// Host cell observation passed to [`Predictor::reconcile_seen`] and [`KeySample`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// Empty or space cell.
    Blank,
    /// Narrow glyph drawn at its own logical column.
    Glyph(char),
    /// Non-plain cell (wide continuation, combining marks, or non-normal width) with legacy glyph.
    Opaque(Option<char>),
}

impl Seen {
    /// Classifies a cell holding `ch` (`' '` for empty) and `opaque` flag into a [`Seen`].
    #[must_use]
    #[inline]
    pub const fn of_cell(ch: char, opaque: bool) -> Self {
        let legacy = if ch != ' ' { Some(ch) } else { None };
        match legacy {
            _ if opaque => Self::Opaque(legacy),
            Some(ch) => Self::Glyph(ch),
            None => Self::Blank,
        }
    }

    /// Returns the plain glyph for legacy observation, or `None` for [`Seen::Blank`].
    #[must_use]
    #[inline]
    pub const fn legacy(self) -> Option<char> {
        match self {
            Self::Blank => None,
            Self::Glyph(ch) => Some(ch),
            Self::Opaque(legacy) => legacy,
        }
    }
}

/// Authenticated OSC 133 `B` prompt input-start coordinate and prompt block ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputStart {
    /// Visible `(row, col)` of the OSC 133 `B` mark.
    pub cell: (u16, u16),
    /// Prompt block ID owning this `B` mark.
    pub prompt: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PromptGate {
    #[default]
    Shut,
    Open {
        after: u64,
    },
}

/// Terminal state sampled under a single keypress hold for [`Predictor::predict_glyph`] and [`Predictor::predict_edit`].
#[derive(Clone, Copy, Debug, Default)]
pub struct KeySample<'r> {
    /// Live cursor `(row, col)`.
    pub cursor: (u16, u16),
    /// Host content-coordinate generation.
    pub coords: u64,
    /// Grid dimensions `(cols, rows)`.
    pub grid: (u16, u16),
    /// Observed cells across the cursor row.
    pub row: Option<&'r [Seen]>,
    /// Observed cells across the row immediately below the cursor row.
    pub below: Option<&'r [Seen]>,
    /// Terminal deferred-wrap flag (`Grid::pending_wrap`) at the press.
    pub wrap_pending: Option<bool>,
    /// Authenticated OSC 133 `B` buffer start for the active prompt, if any.
    pub input_start: Option<InputStart>,
    /// Active line-editor context for learned key bindings.
    pub context: EditorContext,
    /// Whether the active keyboard mode encodes control chords as escape sequences.
    pub escaped_chords: bool,
}

/// Bare line-editing key handled by [`Predictor::predict_edit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKey {
    Left,
    Right,
    /// Backspace (`DEL` or `BS`).
    Backspace,
    Home,
    End,
    CtrlA,
    CtrlE,
    Delete,
}

/// Disposition of an [`EditKey`] passed to [`Predictor::predict_edit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// Armed as a tracked pending edit operation.
    Armed,
    /// Cancelled the predictor's own trailing glyph prediction.
    Cancelled,
    /// Not armed; owed and accounted for according to the key's kind.
    Declined,
}

#[derive(Clone, Debug)]
struct EditLine {
    row: u16,
    lo: u16,
    epoch: u64,
    text: Vec<char>,
    cursor: u16,
    vacated: u16,
    end_known: bool,
    fresh: Option<u64>,
}

impl EditLine {
    #[inline]
    fn hi(&self) -> u16 {
        self.lo
            .saturating_add(u16::try_from(self.text.len()).unwrap_or(u16::MAX))
    }
}

#[derive(Clone, Copy, Debug)]
struct ChainEnd {
    row: u16,
    lo: u16,
    hi: u16,
    cursor: u16,
    vacated: u16,
}

#[derive(Clone, Copy, Debug, Default)]
struct Worlds {
    fewer: bool,
    other: bool,
    proves: u8,
}

#[inline]
const fn cell_holds(seen: Seen, ch: char) -> bool {
    match seen {
        Seen::Blank => ch == ' ',
        Seen::Glyph(g) => g == ch,
        Seen::Opaque(_) => false,
    }
}

#[inline]
fn reorders(ch: char) -> bool {
    aterm_bidi::has_bidi(&[ch])
}

#[inline]
fn row_reorders(cells: &[Seen]) -> bool {
    cells.iter().any(|s| s.legacy().is_some_and(reorders))
}

/// Decision counters used by non-vacuity checks and benchmarks.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredictEvents {
    /// Expired guesses that owed their echo ([`Rule::ExpiryOwes`]).
    pub expiry_owes: u64,
    /// Credited echoes that did not prove the epoch due to a non-clean tail ([`Rule::TailGuard`]).
    pub tail_refusals: u64,
    /// Credited echoes that proved the epoch over an unchanged right-hand tail.
    pub decorated_proofs: u64,
    /// Uncredited retired guesses that owed only their own glyph ([`Rule::TypedOwes`]).
    pub typed_owes: u64,
    /// Standing proofs voided when the tail to their right changed ([`Rule::StandingTail`]).
    pub standing_voids: u64,
    /// Reconcile turns waiting for the cursor to advance past a matching glyph ([`Rule::CursorAdvance`]).
    pub advance_waits: u64,
    /// Edit operations armed by [`Predictor::predict_edit`].
    pub edit_ops_armed: u64,
    /// Credited cursor motions that proved the motion class ([`Rule::MotionClassProof`]).
    pub motions_proven: u64,
    /// Net-null edit tails retired without credit ([`Rule::NetNullUncredited`]).
    pub net_null_retired: u64,
    /// Reconcile turns where a state chain waited in transit.
    pub transit_waits: u64,
    /// Arrow keys declined while preserving in-flight pixels.
    pub decline_motions: u64,
    /// Erases refused due to non-blank cells past the text end ([`Rule::VacateRefusal`]).
    pub vacate_refusals: u64,
    /// Mid-line insert or erase shift operations armed.
    pub shifts_armed: u64,
    /// Credited shifts that proved their shift class ([`Rule::InsertShiftProof`], [`Rule::EraseShiftProof`]).
    pub shifts_proven: u64,
    /// Edit keys refused or chains flushed due to an opaque cell on the line.
    pub opaque_refusals: u64,
    /// Unplaceable mid-line glyphs that flushed an active state chain ([`Rule::MidLineDeclineFlushes`]).
    pub mid_line_flushes: u64,
    /// Credited shifts indistinguishable from an in-place edit ([`Rule::ShiftProofDistinguishes`]).
    pub ambiguous_shifts: u64,
    /// Keys declined on rows subject to UAX #9 bidi reordering ([`Rule::RightToLeftRows`]).
    pub bidi_refusals: u64,
    /// Known lines started at an authenticated OSC 133 `B` prompt mark.
    pub witnessed_lines: u64,
    /// Line-edge and Delete keys armed onto a state chain (including probes).
    pub line_edge_armed: u64,
    /// Unlearned line-edge or Delete keys armed as dark probes.
    pub probes_armed: u64,
    /// Editor key bindings learned after two confirmed canonical echoes ([`Rule::LearnedBindings`]).
    pub bindings_learned: u64,
    /// Learned editor key bindings invalidated by divergence or expiry.
    pub bindings_unlearned: u64,
    /// Control-chord edit keys refused under escape-sequence keyboard modes.
    pub kitty_refusals: u64,
    /// Turns where a guess waited on an unexpected glyph under the cursor ([`Rule::DarkBehindAWait`]).
    pub held_waits: u64,
    /// Turns where a guess typed over existing text waited with subsequent guesses held dark ([`Rule::DarkOverText`]).
    pub text_waits: u64,
    /// Glyph credits revoked when the cursor stepped back onto or before them ([`Rule::CreditTakenBack`]).
    pub credits_taken_back: u64,
    /// Standing proofs voided when text appeared on the row below ([`Rule::FloorBelow`]).
    pub floor_voids: u64,
    /// Total chain operations folded during state-chain construction and retirement.
    pub ops_folded: u64,
    /// Flushes that dropped at least one pending prediction or edit op.
    pub flushes: u64,
    /// Confirmation epochs ended while keeping in-flight pixels.
    pub epoch_ends: u64,
    /// Pending guesses or state chains retired by glitch-window expiry.
    pub expiries: u64,
}

#[derive(Clone, Copy)]
enum Event {
    ExpiryOwe,
    TailRefusal,
    DecoratedProof,
    TypedOwe,
    StandingVoid,
    AdvanceWait,
    EditArmed,
    MotionProven,
    NetNull,
    TransitWait,
    DeclineMotion,
    VacateRefusal,
    ShiftArmed,
    ShiftProven,
    OpaqueRefusal,
    MidLineFlush,
    AmbiguousShift,
    BidiRefusal,
    WitnessedLine,
    EdgeArmed,
    ProbeArmed,
    BindingLearned,
    BindingUnlearned,
    KittyRefusal,
    HeldWait,
    TextWait,
    FloorVoid,
    CreditTakenBack,
    Flush,
    EpochEnd,
    Expiry,
}

const GLITCH_FLOOR_MS: u64 = 250;
const GLITCH_CEILING_MS: u64 = 2000;
const GLITCH_SRTT_MULT: f32 = 3.0;
const EXPIRY_BACKOFF_MAX: u8 = 3;
const DISPLAY_OPEN_MS: f32 = 20.0;
const DISPLAY_CLOSE_MS: f32 = 8.0;
const SLOW_SAMPLES_TO_DISPLAY: u8 = 2;
const FAST_SAMPLES_TO_HIDE: u8 = 3;
const SRTT_ALPHA: f32 = 0.3;

/// Computes the bounded glitch-expiry window (ms) from smoothed RTT (`srtt_us`) and `expiry_backoff`.
#[must_use]
#[inline]
pub const fn compute_glitch_window_ms(srtt_us: Option<u32>, expiry_backoff: u8) -> u64 {
    let shift = if expiry_backoff > EXPIRY_BACKOFF_MAX {
        EXPIRY_BACKOFF_MAX
    } else {
        expiry_backoff
    };
    let backed_floor = GLITCH_FLOOR_MS << (shift as u32);
    let floor_ms = if backed_floor > GLITCH_CEILING_MS {
        GLITCH_CEILING_MS
    } else {
        backed_floor
    };
    let target_ms = match srtt_us {
        Some(us) => ((us as u64).saturating_mul(3)) / 1_000,
        None => floor_ms,
    };
    if target_ms < floor_ms {
        floor_ms
    } else if target_ms > GLITCH_CEILING_MS {
        GLITCH_CEILING_MS
    } else {
        target_ms
    }
}

/// Computes one fixed-point EWMA SRTT update step in microseconds (`0.7 * prev + 0.3 * sample`).
#[must_use]
#[inline]
pub const fn ewma_srtt_step_us(prev_srtt_us: Option<u32>, sample_us: u32) -> u32 {
    match prev_srtt_us {
        None => sample_us,
        Some(prev) => {
            let weighted = (prev as u64)
                .saturating_mul(7)
                .saturating_add((sample_us as u64).saturating_mul(3))
                / 10;
            if weighted > (u32::MAX as u64) {
                u32::MAX
            } else {
                weighted as u32
            }
        }
    }
}

/// Encodes a predicted ghost cell `(row, col, ch, epoch)` into a 16-byte little-endian wire record.
#[must_use]
#[inline]
pub const fn encode_predicted_ghost_wire_le(row: u16, col: u16, ch: char, epoch: u64) -> [u8; 16] {
    let r = row.to_le_bytes();
    let c = col.to_le_bytes();
    let g = (ch as u32).to_le_bytes();
    let e = epoch.to_le_bytes();
    [
        r[0], r[1], c[0], c[1], g[0], g[1], g[2], g[3], e[0], e[1], e[2], e[3], e[4], e[5], e[6],
        e[7],
    ]
}

/// Decodes a 16-byte little-endian ghost cell record from [`encode_predicted_ghost_wire_le`].
#[must_use]
#[inline]
pub const fn decode_predicted_ghost_wire_le(wire: &[u8; 16]) -> Option<(u16, u16, char, u64)> {
    let row = u16::from_le_bytes([wire[0], wire[1]]);
    let col = u16::from_le_bytes([wire[2], wire[3]]);
    let raw_ch = u32::from_le_bytes([wire[4], wire[5], wire[6], wire[7]]);
    let Some(ch) = char::from_u32(raw_ch) else {
        return None;
    };
    let epoch = u64::from_le_bytes([
        wire[8], wire[9], wire[10], wire[11], wire[12], wire[13], wire[14], wire[15],
    ]);
    Some((row, col, ch, epoch))
}

/// Returns `true` if `bytes` is a pure line-submit sequence ending in CR or LF with no control edits.
#[must_use]
#[inline]
pub fn is_line_submit(bytes: &[u8]) -> bool {
    let is_terminator = |c: char| matches!(c, '\r' | '\n');
    std::str::from_utf8(bytes).is_ok_and(|text| {
        text.ends_with(is_terminator) && text.chars().all(|c| is_terminator(c) || !c.is_control())
    })
}

/// Speculative local-echo state machine for a single terminal pane.
#[derive(Default)]
pub struct Predictor {
    mode: PredictMode,
    preds: VecDeque<Prediction>,

    srtt: Option<f32>,
    expiry_backoff: u8,
    slow_samples: u8,
    fast_samples: u8,
    adaptive_slow: bool,
    confirmed_epoch: bool,
    proof_cell: Option<(u16, u16)>,
    proof_echo: Option<Prediction>,
    proof_line_end: bool,
    proof_opaque: bool,
    epoch: u64,
    grid_cols: u16,
    deferred_wrap: bool,
    anchor_lost: bool,
    grandfathered_epoch: Option<u64>,
    coords: Option<u64>,
    owed_at: Option<Instant>,
    ambiguous_width_double: bool,
    declined_tail: u16,
    next_seq: u64,
    cancelled_at: Option<(u16, u16)>,
    cancelled_stale: bool,
    cancelled_pair: Option<(u16, u16, char, u64)>,
    glyph_owes: VecDeque<(u64, char, Instant)>,
    #[cfg(any(test, debug_assertions))]
    disabled: u64,
    line: Option<EditLine>,
    chain: bool,
    proven: u8,
    carried_proven: u8,
    carried_proof: Option<(Prediction, bool, bool)>,
    editing_off: bool,
    edit_capable: bool,
    seen: Vec<Seen>,
    fold_text: Vec<char>,
    fold_cells: Vec<(char, u16)>,
    ghosts: Vec<Prediction>,
    ghost_cursor: Option<(u16, u16)>,
    ghost_marks: Vec<u8>,
    world_cells: Vec<Option<char>>,
    run_stale: bool,
    run_inside: bool,
    run_witness: Option<InputStart>,
    held_wait: Option<u64>,
    text_wait: Option<u64>,
    last_credit: Option<(u16, u16, Instant)>,
    prompt_gate: PromptGate,
    bindings: EditorBindings,
    tail: Option<(u16, u16)>,
    tail_cells: Vec<Seen>,
    tail_line_end: bool,
    floor: Option<u16>,
    floor_cells: Vec<Seen>,
    line_end_at: Option<(u16, u16)>,
    moved_tail_at: Option<Instant>,
    hidden_tail_at: Option<(u16, u16)>,
    run_hidden: bool,
    #[cfg(any(test, debug_assertions))]
    events: PredictEvents,
    #[cfg(any(test, debug_assertions))]
    credited: VecDeque<u64>,
    #[cfg(any(test, debug_assertions))]
    chain_matched: bool,
}

impl Predictor {
    /// Constructs a [`Predictor`] starting in `mode`.
    #[inline]
    pub fn new(mode: PredictMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Disables `rule` as a negative control for safety-theorem verification.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn disable_rule_for_test(&mut self, rule: Rule) {
        self.disabled |= rule.bit();
    }

    /// Returns the cumulative [`PredictEvents`] decision counters.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn events_for_test(&self) -> PredictEvents {
        self.events
    }

    /// Drains and returns press-order sequence numbers of guesses credited since the last call.
    #[cfg(any(test, debug_assertions))]
    pub fn take_credited_for_test(&mut self) -> Vec<u64> {
        self.credited.drain(..).collect()
    }

    #[inline]
    const fn count(&mut self, event: Event) {
        #[cfg(any(test, debug_assertions))]
        {
            let n = match event {
                Event::ExpiryOwe => &mut self.events.expiry_owes,
                Event::TailRefusal => &mut self.events.tail_refusals,
                Event::DecoratedProof => &mut self.events.decorated_proofs,
                Event::TypedOwe => &mut self.events.typed_owes,
                Event::StandingVoid => &mut self.events.standing_voids,
                Event::AdvanceWait => &mut self.events.advance_waits,
                Event::EditArmed => &mut self.events.edit_ops_armed,
                Event::MotionProven => &mut self.events.motions_proven,
                Event::NetNull => &mut self.events.net_null_retired,
                Event::TransitWait => &mut self.events.transit_waits,
                Event::DeclineMotion => &mut self.events.decline_motions,
                Event::VacateRefusal => &mut self.events.vacate_refusals,
                Event::ShiftArmed => &mut self.events.shifts_armed,
                Event::ShiftProven => &mut self.events.shifts_proven,
                Event::OpaqueRefusal => &mut self.events.opaque_refusals,
                Event::MidLineFlush => &mut self.events.mid_line_flushes,
                Event::AmbiguousShift => &mut self.events.ambiguous_shifts,
                Event::BidiRefusal => &mut self.events.bidi_refusals,
                Event::WitnessedLine => &mut self.events.witnessed_lines,
                Event::EdgeArmed => &mut self.events.line_edge_armed,
                Event::ProbeArmed => &mut self.events.probes_armed,
                Event::BindingLearned => &mut self.events.bindings_learned,
                Event::BindingUnlearned => &mut self.events.bindings_unlearned,
                Event::KittyRefusal => &mut self.events.kitty_refusals,
                Event::HeldWait => &mut self.events.held_waits,
                Event::TextWait => &mut self.events.text_waits,
                Event::FloorVoid => &mut self.events.floor_voids,
                Event::CreditTakenBack => &mut self.events.credits_taken_back,
                Event::Flush => &mut self.events.flushes,
                Event::EpochEnd => &mut self.events.epoch_ends,
                Event::Expiry => &mut self.events.expiries,
            };
            *n = n.saturating_add(1);
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            let _ = event;
        }
    }

    #[inline]
    fn folded(&mut self, n: usize) {
        #[cfg(any(test, debug_assertions))]
        {
            let n = u64::try_from(n).unwrap_or(u64::MAX);
            self.events.ops_folded = self.events.ops_folded.saturating_add(n);
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            let _ = n;
        }
    }

    #[inline]
    const fn rule(&self, rule: Rule) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.disabled & rule.bit() == 0
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            let _ = rule;
            true
        }
    }

    /// Returns the current unconfirmed-prediction glitch expiry window.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub fn glitch_window_for_test(&self) -> Duration {
        self.glitch_window()
    }

    /// Returns the active [`PredictMode`].
    #[must_use]
    #[inline]
    pub const fn mode(&self) -> PredictMode {
        self.mode
    }

    /// Sets the [`PredictMode`], flushing in-flight predictions if the mode changed.
    pub fn set_mode(&mut self, mode: PredictMode) {
        if mode != self.mode {
            self.mode = mode;
            self.flush();
            self.prompt_gate = PromptGate::Shut;
        }
    }

    /// Configures whether East Asian Ambiguous characters are measured as two cells wide.
    pub fn set_ambiguous_width_double(&mut self, wide: bool) {
        if wide != self.ambiguous_width_double {
            self.ambiguous_width_double = wide;
            self.flush();
        }
    }

    #[inline]
    const fn cells_of(&self, ch: char) -> usize {
        if self.ambiguous_width_double {
            aterm_grapheme::char_width_cjk(ch)
        } else {
            aterm_grapheme::char_width(ch)
        }
    }

    /// Registers a typed printable character when grid row height is unknown.
    #[inline]
    pub fn predict_char(
        &mut self,
        ch: char,
        cursor: (u16, u16),
        coords: u64,
        cols: u16,
        now: Instant,
    ) -> bool {
        self.predict_char_in_grid(ch, cursor, coords, (cols, 0), now)
    }

    /// Registers a typed printable character within a `(cols, rows)` grid.
    pub fn predict_char_in_grid(
        &mut self,
        ch: char,
        cursor: (u16, u16),
        coords: u64,
        grid: (u16, u16),
        now: Instant,
    ) -> bool {
        let sample = KeySample {
            cursor,
            coords,
            grid,
            ..KeySample::default()
        };
        self.arm_glyph(ch, &sample, now)
    }

    /// Returns `true` if the next glyph press needs the cursor row sampled in [`KeySample::row`].
    #[must_use]
    #[inline]
    pub fn wants_key_row(&self) -> bool {
        self.preds.is_empty()
    }

    /// Registers a typed printable character `ch` using the terminal state in `sample`.
    #[inline]
    pub fn predict_glyph(&mut self, ch: char, sample: &KeySample<'_>, now: Instant) -> bool {
        self.arm_glyph(ch, sample, now)
    }

    fn arm_glyph(&mut self, ch: char, sample: &KeySample<'_>, now: Instant) -> bool {
        let (cursor, coords, grid, cells) = (sample.cursor, sample.coords, sample.grid, sample.row);
        let first_key = self.first_key();
        if self.mode == PredictMode::Off {
            return false;
        }
        let (cols, rows) = grid;
        if cols == 0 {
            return false;
        }
        self.observe_coordinates(coords);
        self.grid_cols = cols;
        self.observe_cancelled_cursor(Some(cursor), now);
        self.settle_chain();
        if self.preds.is_empty() {
            self.anchor_lost = false;
        }
        if self.engaged() {
            return self.arm_in_chain(ch, cursor, cols, now);
        }
        if self.rule(Rule::RightToLeftRows) && (reorders(ch) || cells.is_some_and(row_reorders)) {
            self.count(Event::BidiRefusal);
            return self.decline(ch, now);
        }
        let inside = self.preds.is_empty()
            && self
                .line
                .as_ref()
                .is_some_and(|l| l.epoch == self.epoch && l.cursor < l.hi());
        if inside {
            if let Some(armed) = self.arm_inside(ch, cursor, cols, cells, now) {
                return armed;
            }
            self.void_proof();
        }
        let width = self.cells_of(ch);
        if ch.is_control() || width != 1 || self.anchor_lost || self.preds.len() >= MAX_PREDICTIONS
        {
            return self.decline(ch, now);
        }
        let anchor = self
            .preds
            .iter()
            .rev()
            .find(|p| p.epoch == self.epoch)
            .copied();
        if anchor.is_none() && !self.preds.is_empty() {
            return self.decline(ch, now);
        }
        let cancelled = self.cancelled_at.take().filter(|_| anchor.is_none());
        let live = cancelled.unwrap_or(cursor);
        if anchor.is_none() && self.proof_cell != Some(live) {
            self.void_proof();
        }
        if anchor.is_none() {
            self.standing_at_press(cells, cursor, cancelled.is_some());
        }
        let inherits_line_end = if self.confirmed_epoch {
            Some(self.proof_line_end)
        } else if self.line_end_at == Some(live) {
            Some(true)
        } else {
            None
        };
        let (row, col) = match anchor {
            Some(p) => (p.row, p.col.saturating_add(1)),
            None if cancelled.is_some() => live,
            None if self.parked(sample.wrap_pending) && cursor.1.saturating_add(1) == cols => {
                (cursor.0, cols)
            }
            None => cursor,
        };
        let (row, col) = if col >= cols {
            let next = row.saturating_add(1);
            if next >= rows {
                return self.decline(ch, now);
            }
            (next, 0)
        } else {
            (row, col)
        };
        if anchor.is_none() {
            self.take_tail(cursor, (row, col), cells, cols, inherits_line_end, now);
            self.take_floor(cursor, (row, col), sample.below);
            let window = self.glitch_window();
            let stale_cancel =
                cancelled.is_some() && self.cancelled_stale && self.rule(Rule::StaleCancel);
            self.run_stale = stale_cancel
                || self
                    .owed_at
                    .is_some_and(|t| now.saturating_duration_since(t) <= window);
            self.run_inside = inside;
            self.run_witness = self.witness_of(sample.input_start, first_key, (row, col));
        }
        self.push_op(row, col, ch, Op::Insert, OpClass::Glyph, now);
        if let Some(p) = self.preds.back_mut() {
            p.stale = self.run_stale;
        }
        if !self.text_held()
            && self.first_over_text() == self.preds.len().checked_sub(1)
            && let Some(p) = self.preds.back()
        {
            let read = cells
                .filter(|_| row == cursor.0)
                .and_then(|r| r.get(usize::from(col)).copied())
                .or_else(|| self.tail_at(row, col));
            if read.and_then(Seen::legacy).is_some_and(|g| g != ch) {
                self.text_wait = Some(p.seq);
            }
        }
        true
    }

    #[inline]
    fn text_under(&self, row: u16, col: u16, ch: char) -> Option<char> {
        if self.tail_line_end || !self.rule(Rule::DarkOverText) {
            return None;
        }
        self.tail_at(row, col)
            .and_then(Seen::legacy)
            .filter(|&c| c != ch)
    }

    #[inline]
    fn first_over_text(&self) -> Option<usize> {
        self.preds
            .iter()
            .position(|p| self.text_under(p.row, p.col, p.ch).is_some())
    }

    #[inline]
    fn text_held(&self) -> bool {
        self.text_wait
            .is_some_and(|s| self.preds.iter().any(|p| p.seq == s))
    }

    #[inline]
    fn push_op(&mut self, row: u16, col: u16, ch: char, op: Op, class: OpClass, now: Instant) {
        self.preds.push_back(Prediction {
            row,
            col,
            ch,
            born: now,
            epoch: self.epoch,
            owed: self.owed_at,
            seq: self.next_seq,
            op,
            class,
            stale: false,
            ghost: GhostCell::Typed,
            probe: false,
        });
        self.next_seq = self.next_seq.wrapping_add(1);
        self.declined_tail = 0;
    }

    /// Registers a Backspace on the glyph lane, cancelling a trailing prediction of the current epoch if possible.
    pub fn predict_backspace(&mut self, now: Instant) -> bool {
        let _ = self.first_key();
        if self.mode == PredictMode::Off {
            return false;
        }
        if let Some(p) = self
            .preds
            .back()
            .copied()
            .filter(|p| p.epoch == self.epoch && p.op == Op::Insert && self.pair_cancels())
        {
            self.preds.pop_back();
            if self.rule(Rule::CancelTransit) {
                self.cancelled_at = Some((p.row, p.col));
                self.cancelled_stale = p.stale;
            }
            if self.rule(Rule::CancelOwes) {
                self.owe_glyph(p.seq, p.ch, now, now);
            }
            if p.ch != ' ' && self.rule(Rule::CancelOwesItsBlank) {
                self.owe_glyph(p.seq, ' ', now, now);
            }
            self.cancelled_pair = Some((p.row, p.col, p.ch, self.next_seq));
            self.settle_chain();
            return true;
        }
        self.owe(now);
        if let Some(rest) = self.declined_tail.checked_sub(1) {
            self.declined_tail = rest;
            return false;
        }
        self.flush();
        false
    }

    /// Enables or disables the state-chain edit lane ([`Self::predict_edit`]).
    pub fn set_editing(&mut self, on: bool) {
        if on == self.editing_off {
            self.editing_off = !on;
            self.flush();
        }
    }

    /// Registers a line-editing key (`key`) with the terminal state sampled in `sample`.
    pub fn predict_edit(
        &mut self,
        key: EditKey,
        sample: &KeySample<'_>,
        now: Instant,
    ) -> EditOutcome {
        let _ = self.first_key();
        if self.mode == PredictMode::Off {
            return EditOutcome::Declined;
        }
        let (cols, _) = sample.grid;
        if cols == 0 {
            return EditOutcome::Declined;
        }
        self.observe_coordinates(sample.coords);
        self.grid_cols = cols;
        self.observe_cancelled_cursor(Some(sample.cursor), now);
        self.settle_chain();
        if self.preds.is_empty() {
            self.anchor_lost = false;
        }
        if self
            .held_wait
            .is_some_and(|seq| self.preds.iter().any(|p| p.seq == seq))
        {
            self.note_unmodelled_input(now);
            return EditOutcome::Declined;
        }
        if let Some(learned) = Learned::of(key) {
            return self.predict_line_edge(learned, sample, now);
        }
        if self.editing_off {
            return match key {
                EditKey::Backspace if self.predict_backspace(now) => EditOutcome::Cancelled,
                EditKey::Backspace => EditOutcome::Declined,
                _ => {
                    self.note_unmodelled_input(now);
                    EditOutcome::Declined
                }
            };
        }
        if key == EditKey::Backspace
            && self
                .preds
                .back()
                .is_some_and(|p| p.epoch == self.epoch && p.op == Op::Insert)
            && self.pair_cancels()
        {
            let _popped = self.predict_backspace(now);
            return EditOutcome::Cancelled;
        }
        if !self.engaged() {
            self.standing_at_press(sample.row, sample.cursor, false);
        }
        let end = self
            .placeable(sample.cursor)
            .filter(|e| self.press_row_clear(sample.row, e, key == EditKey::Backspace));
        let edges = self.rule(Rule::TextEdges);
        let armed = end.and_then(|e| match key {
            EditKey::Left if e.cursor > e.lo || !edges => Some((e, Op::Left, OpClass::Motion)),
            EditKey::Right if e.cursor < e.hi || !edges => Some((e, Op::Right, OpClass::Motion)),
            EditKey::Backspace if e.cursor > e.lo && e.cursor == e.hi => {
                Some((e, Op::Erase, OpClass::Glyph))
            }
            EditKey::Backspace if e.cursor > e.lo && e.cursor < e.hi => {
                Some((e, Op::Erase, OpClass::EraseShift))
            }
            _ => None,
        });
        let Some((e, op, class)) = armed.filter(|_| self.preds.len() < EDIT_CHAIN_MAX) else {
            return match key {
                EditKey::Left | EditKey::Right => self.decline_motion(now),
                EditKey::Backspace if self.predict_backspace(now) => EditOutcome::Cancelled,
                EditKey::Backspace => EditOutcome::Declined,
                _ => {
                    self.note_unmodelled_input(now);
                    EditOutcome::Declined
                }
            };
        };
        if !self.engaged()
            && let Some(cells) = sample.row
        {
            self.start_chain(cells, &e);
        }
        self.chain = true;
        self.cancelled_at = None;
        self.push_op(e.row, e.cursor, EDIT_OP_CH, op, class, now);
        self.count(Event::EditArmed);
        if class == OpClass::EraseShift {
            self.count(Event::ShiftArmed);
        }
        EditOutcome::Armed
    }

    fn predict_line_edge(
        &mut self,
        key: Learned,
        sample: &KeySample<'_>,
        now: Instant,
    ) -> EditOutcome {
        let unmodelled = |p: &mut Self| {
            p.note_unmodelled_input(now);
            EditOutcome::Declined
        };
        if self.editing_off {
            return unmodelled(self);
        }
        if key.chord() && sample.escaped_chords {
            self.count(Event::KittyRefusal);
            return unmodelled(self);
        }
        self.observe_context(sample.context);
        if self.bindings.banned(key) {
            return unmodelled(self);
        }
        if !self.engaged() {
            self.standing_at_press(sample.row, sample.cursor, false);
        }
        let end = self
            .placeable(sample.cursor)
            .filter(|e| self.witnessed(sample, e))
            .filter(|e| self.press_row_clear(sample.row, e, key == Learned::Delete));
        let op = key.op();
        let class = if op == Op::Delete {
            OpClass::DeleteFwd
        } else {
            OpClass::LineEdge
        };
        let changes = |e: &ChainEnd| match op {
            Op::LineStart(_) => e.cursor > e.lo,
            Op::LineEnd(_) => e.cursor < e.hi || !self.rule(Rule::EndRefusal),
            Op::Delete => e.cursor < e.hi,
            Op::Insert | Op::Left | Op::Right | Op::Erase => false,
        };
        let Some(e) = end.filter(|e| changes(e) && self.preds.len() < EDIT_CHAIN_MAX) else {
            // R-END: at the end of the text an autosuggestion is accepted.
            if end.is_some_and(|e| matches!(op, Op::LineEnd(_)) && e.cursor >= e.hi) {
                return self.decline_motion(now);
            }
            return unmodelled(self);
        };
        if !self.engaged()
            && let Some(cells) = sample.row
        {
            self.start_chain(cells, &e);
        }
        self.chain = true;
        self.cancelled_at = None;
        self.push_op(e.row, e.cursor, EDIT_OP_CH, op, class, now);
        let probe = self.rule(Rule::LearnedBindings) && !self.bindings.learned(key);
        if let Some(p) = self.preds.back_mut() {
            p.probe = probe;
        }
        self.count(Event::EditArmed);
        self.count(Event::EdgeArmed);
        if probe {
            self.count(Event::ProbeArmed);
        }
        EditOutcome::Armed
    }

    fn witnessed(&self, sample: &KeySample<'_>, e: &ChainEnd) -> bool {
        let Some(start) = sample.input_start else {
            return false;
        };
        let Some(line) = self.line.as_ref() else {
            return false;
        };
        let (row, col) = start.cell;
        row == e.row
            && (!self.rule(Rule::WitnessFreshPrompt)
                || (line.fresh == Some(start.prompt) && col == line.lo))
    }

    #[inline]
    fn first_key(&mut self) -> PromptGate {
        std::mem::take(&mut self.prompt_gate)
    }

    #[inline]
    fn witness_of(
        &self,
        start: Option<InputStart>,
        gate: PromptGate,
        cell: (u16, u16),
    ) -> Option<InputStart> {
        let start = start.filter(|s| s.cell == cell)?;
        if !self.rule(Rule::WitnessFirstKey) {
            return Some(start);
        }
        match gate {
            PromptGate::Open { after } if after != start.prompt => Some(start),
            PromptGate::Open { .. } | PromptGate::Shut => None,
        }
    }

    fn observe_context(&mut self, context: EditorContext) {
        if self.bindings.context == Some(context) {
            return;
        }
        if self.preds.iter().any(|p| p.op.learned().is_some()) {
            self.flush();
        }
        self.bindings = EditorBindings {
            context: Some(context),
            ..EditorBindings::default()
        };
    }

    fn credit_binding(&mut self, key: Learned) {
        let Some(b) = self.bindings.keys.get_mut(key.index()) else {
            return;
        };
        if b.contradictions >= BINDING_BANNED || b.seen >= BINDING_LEARNED {
            return;
        }
        b.seen = b.seen.saturating_add(1);
        if b.seen >= BINDING_LEARNED {
            self.count(Event::BindingLearned);
        }
    }

    fn contradict_head(&mut self) {
        let Some(key) = self.preds.front().and_then(|p| p.op.learned()) else {
            return;
        };
        let Some(b) = self.bindings.keys.get_mut(key.index()) else {
            return;
        };
        b.seen = 0;
        b.contradictions = b.contradictions.saturating_add(1).min(BINDING_BANNED);
        self.count(Event::BindingUnlearned);
    }

    fn standing_at_press(&mut self, cells: Option<&[Seen]>, cursor: (u16, u16), skip_tail: bool) {
        if self.confirmed_epoch
            && self.rule(Rule::StandingGlyph)
            && let (Some(cells), Some(p)) = (cells, self.proof_echo)
            && p.row == cursor.0
            && !Self::holds_its_glyph(
                &p,
                cells
                    .get(usize::from(p.col))
                    .copied()
                    .unwrap_or(Seen::Blank),
            )
        {
            self.moved_tail(p.born);
            self.void_proof();
            self.count(Event::StandingVoid);
        }
        if self.confirmed_epoch
            && !self.proof_opaque
            && self.rule(Rule::MarkedEcho)
            && let (Some(cells), Some(p)) = (cells, self.proof_echo)
            && p.row == cursor.0
            && matches!(cells.get(usize::from(p.col)), Some(Seen::Opaque(_)))
        {
            self.moved_tail(p.born);
            self.void_proof();
            self.count(Event::StandingVoid);
        }
        if !skip_tail
            && !self.proof_line_end
            && self.rule(Rule::TailGuard)
            && self.rule(Rule::StandingTail)
            && let (Some(cells), Some(p)) = (cells, self.proof_echo)
        {
            let row = cursor.0;
            let mut observe = |r: u16, c: u16| {
                if r == row {
                    cells.get(usize::from(c)).copied().unwrap_or(Seen::Blank)
                } else {
                    Seen::Blank
                }
            };
            if !self.tail_clean(&p, &mut observe, &mut None, Some(cursor)) {
                self.moved_tail(p.born);
                self.void_proof();
                self.count(Event::StandingVoid);
            }
        }
    }

    fn start_chain(&mut self, cells: &[Seen], e: &ChainEnd) {
        self.last_credit = None;
        self.seen.clear();
        self.seen.extend(
            (e.lo..self.grid_cols)
                .map(|c| cells.get(usize::from(c)).copied().unwrap_or(Seen::Blank)),
        );
        self.tail = Some((e.row, e.lo));
        self.tail_cells.clear();
        self.tail_cells.extend_from_slice(&self.seen);
        self.chain = true;
    }

    fn arm_inside(
        &mut self,
        ch: char,
        cursor: (u16, u16),
        cols: u16,
        cells: Option<&[Seen]>,
        now: Instant,
    ) -> Option<bool> {
        if self.editing_off || ch.is_control() || self.cells_of(ch) != 1 {
            return None;
        }
        let cells = cells?;
        if self.cancelled_at.is_some() && self.cancelled_stale && self.rule(Rule::StaleCancel) {
            return None;
        }
        let live = self.cancelled_at.unwrap_or(cursor);
        if self.proof_cell != Some(live) {
            return None;
        }
        self.standing_at_press(Some(cells), cursor, false);
        let e = self.placeable(live)?;
        let fits = e.hi.checked_add(2).is_some_and(|after| after <= cols);
        if e.cursor >= e.hi || !fits || !self.press_row_clear(Some(cells), &e, false) {
            return None;
        }
        self.start_chain(cells, &e);
        self.cancelled_at = None;
        self.push_op(e.row, e.cursor, ch, Op::Insert, OpClass::InsertShift, now);
        self.count(Event::ShiftArmed);
        Some(true)
    }

    fn decline_motion(&mut self, now: Instant) -> EditOutcome {
        if self.rule(Rule::EditOpsOwe) {
            self.owe(now);
        }
        self.moved_tail(now);
        self.end_epoch();
        self.anchor_lost = true;
        self.count(Event::DeclineMotion);
        EditOutcome::Declined
    }

    #[inline]
    fn engaged(&self) -> bool {
        self.chain && !self.preds.is_empty()
    }

    fn settle_chain(&mut self) {
        if !self.preds.is_empty() {
            return;
        }
        self.chain = false;
        let epoch = self.epoch;
        if self.confirmed_epoch
            && let Some(l) = self.line.as_mut().filter(|l| l.epoch == epoch)
        {
            l.vacated = l.hi();
        } else {
            self.line = None;
        }
    }

    fn placeable(&mut self, cursor: (u16, u16)) -> Option<ChainEnd> {
        let (end, walked) = self.chain_end(cursor);
        self.folded(walked);
        end
    }

    fn chain_end(&self, cursor: (u16, u16)) -> (Option<ChainEnd>, usize) {
        if self.editing_off || !self.edit_capable || self.anchor_lost || !self.confirmed_epoch {
            return (None, 0);
        }
        let Some(l) = self.line.as_ref().filter(|l| l.epoch == self.epoch) else {
            return (None, 0);
        };
        if self.preds.is_empty() && cursor != (l.row, l.cursor) {
            return (None, 0);
        }
        let mut end = ChainEnd {
            row: l.row,
            lo: l.lo,
            hi: l.hi(),
            cursor: l.cursor,
            vacated: l.vacated.max(l.hi()),
        };
        for (walked, p) in (1..).zip(&self.preds) {
            let placed = p.epoch == self.epoch && p.row == end.row && p.col == end.cursor;
            if !placed || Self::end_step(&mut end, p, self.grid_cols).is_none() {
                return (None, walked);
            }
        }
        (Some(end), self.preds.len())
    }

    fn end_step(end: &mut ChainEnd, p: &Prediction, grid_cols: u16) -> Option<()> {
        match p.op {
            Op::Insert
                if end.cursor >= end.lo
                    && end.cursor <= end.hi
                    && end
                        .hi
                        .checked_add(2)
                        .is_some_and(|after| after <= grid_cols) =>
            {
                end.hi = end.hi.saturating_add(1);
                end.cursor = end.cursor.saturating_add(1);
            }
            Op::Insert => return None,
            Op::Left => end.cursor = end.cursor.checked_sub(1)?,
            Op::Right => end.cursor = end.cursor.saturating_add(1),
            Op::Erase => {
                end.hi = end.hi.checked_sub(1)?;
                end.cursor = end.cursor.checked_sub(1)?;
            }
            Op::LineStart(_) => end.cursor = end.lo,
            Op::LineEnd(_) => end.cursor = end.hi,
            Op::Delete => end.hi = end.hi.checked_sub(1).filter(|&hi| hi >= end.cursor)?,
        }
        end.vacated = end.vacated.max(end.hi);
        Some(())
    }

    fn press_row_clear(&mut self, row: Option<&[Seen]>, end: &ChainEnd, erase: bool) -> bool {
        let Some(cells) = row else {
            return false;
        };
        if self.rule(Rule::RightToLeftRows) && row_reorders(cells) {
            self.count(Event::BidiRefusal);
            return false;
        }
        let at = |c: u16| cells.get(usize::from(c)).copied().unwrap_or(Seen::Blank);
        let span_end = end.vacated.saturating_add(1).min(self.grid_cols);
        if (end.lo..span_end).any(|c| matches!(at(c), Seen::Opaque(_))) {
            self.count(Event::OpaqueRefusal);
            return false;
        }
        if !erase || !self.rule(Rule::VacateRefusal) {
            return true;
        }
        let clear = (end.hi..span_end).all(|c| {
            let seen = at(c);
            seen == Seen::Blank || (c < end.vacated && self.chain_text_at(c, seen))
        });
        if !clear {
            self.count(Event::VacateRefusal);
        }
        clear
    }

    fn chain_text_at(&self, c: u16, seen: Seen) -> bool {
        let base = self.line.as_ref().is_some_and(|l| {
            c >= l.lo
                && l.text
                    .get(usize::from(c - l.lo))
                    .is_some_and(|&ch| cell_holds(seen, ch))
        });
        base || self
            .preds
            .iter()
            .any(|p| p.op == Op::Insert && p.col == c && cell_holds(seen, p.ch))
    }

    fn arm_in_chain(&mut self, ch: char, cursor: (u16, u16), cols: u16, now: Instant) -> bool {
        if ch.is_control() {
            return self.decline(ch, now);
        }
        let end = self.placeable(cursor);
        let bidi = self.rule(Rule::RightToLeftRows) && reorders(ch);
        if bidi {
            self.count(Event::BidiRefusal);
        }
        match end {
            Some(e)
                if self.cells_of(ch) == 1
                    && !bidi
                    && e.hi.checked_add(2).is_some_and(|after| after <= cols)
                    && self.preds.len() < EDIT_CHAIN_MAX =>
            {
                self.cancelled_at = None;
                let class = if e.cursor < e.hi {
                    self.count(Event::ShiftArmed);
                    OpClass::InsertShift
                } else {
                    OpClass::Glyph
                };
                self.push_op(e.row, e.cursor, ch, Op::Insert, class, now);
                true
            }
            Some(e) if e.cursor < e.hi && self.rule(Rule::MidLineDeclineFlushes) => {
                self.owe(now);
                self.flush();
                self.count(Event::MidLineFlush);
                false
            }
            _ => self.decline(ch, now),
        }
    }

    fn extend_line(&mut self, p: &Prediction) {
        if self.grid_cols == 0
            || p.col
                .checked_add(2)
                .is_none_or(|after| after > self.grid_cols)
        {
            self.line = None;
            return;
        }
        let epoch = self.epoch;
        let at_end = self.tail_line_end;
        if let Some(l) = self
            .line
            .as_mut()
            .filter(|l| l.epoch == epoch && l.row == p.row && l.cursor == p.col && l.hi() == p.col)
        {
            l.text.push(p.ch);
            l.cursor = l.cursor.saturating_add(1);
            l.vacated = l.vacated.max(l.cursor);
            l.end_known &= at_end;
            return;
        }
        let mut text = self.line.take().map(|l| l.text).unwrap_or_default();
        text.clear();
        text.push(p.ch);
        let next = p.col.saturating_add(1);
        let fresh = self
            .run_witness
            .filter(|s| s.cell == (p.row, p.col))
            .map(|s| s.prompt);
        if fresh.is_some() {
            self.count(Event::WitnessedLine);
        }
        self.line = Some(EditLine {
            row: p.row,
            lo: p.col,
            epoch,
            text,
            cursor: next,
            vacated: next,
            end_known: at_end,
            fresh,
        });
    }

    fn fold_op(text: &mut Vec<char>, cursor: &mut u16, lo: u16, p: &Prediction) {
        Self::fold_cells(text, cursor, lo, p, |ch| ch);
    }

    fn fold_cells<T>(
        text: &mut Vec<T>,
        cursor: &mut u16,
        lo: u16,
        p: &Prediction,
        typed: impl FnOnce(char) -> T,
    ) {
        let at = usize::from(cursor.saturating_sub(lo));
        match p.op {
            Op::Insert => {
                if at <= text.len() {
                    text.insert(at, typed(p.ch));
                }
                *cursor = cursor.saturating_add(1);
            }
            Op::Left => *cursor = cursor.saturating_sub(1),
            Op::Right => *cursor = cursor.saturating_add(1),
            Op::Erase => {
                if let Some(i) = at.checked_sub(1).filter(|&i| i < text.len()) {
                    text.remove(i);
                }
                *cursor = cursor.saturating_sub(1);
            }
            Op::LineStart(_) => *cursor = lo,
            Op::LineEnd(_) => *cursor = Self::text_end(lo, text),
            Op::Delete => {
                if at < text.len() {
                    text.remove(at);
                }
            }
        }
    }

    #[inline]
    fn text_end<T>(lo: u16, text: &[T]) -> u16 {
        lo.saturating_add(u16::try_from(text.len()).unwrap_or(u16::MAX))
    }

    fn chain_far_end(&self, line: &EditLine) -> u16 {
        let mut hi = line.hi();
        let mut far = line.vacated.max(hi);
        for p in &self.preds {
            hi = match p.op {
                Op::Insert => hi.saturating_add(1),
                Op::Erase | Op::Delete => hi.saturating_sub(1),
                Op::Left | Op::Right | Op::LineStart(_) | Op::LineEnd(_) => hi,
            };
            far = far.max(hi);
        }
        far
    }

    fn reconcile_chain(
        &mut self,
        cursor: Option<(u16, u16)>,
        now: Instant,
        observe: &mut impl FnMut(u16, u16) -> Seen,
        oldest: &mut Option<Duration>,
    ) {
        let Some((row, lo, far)) = self
            .line
            .as_ref()
            .map(|l| (l.row, l.lo, self.chain_far_end(l)))
        else {
            self.flush();
            return;
        };
        let span_end = far.saturating_add(1).min(self.grid_cols);
        self.seen.clear();
        for c in lo..self.grid_cols {
            let seen = observe(row, c);
            if c < span_end && matches!(seen, Seen::Opaque(_)) {
                self.count(Event::OpaqueRefusal);
                self.flush();
                return;
            }
            self.seen.push(seen);
        }
        if self.preds.iter().any(|p| p.op.erases())
            && self.rule(Rule::VacateRefusal)
            && self.seen.get(usize::from(far.saturating_sub(lo))) != Some(&Seen::Blank)
        {
            self.count(Event::VacateRefusal);
            self.flush();
            return;
        }
        if self.far_tail_moved(row, far) {
            if let Some(newest) = self.preds.iter().map(|p| p.born).max() {
                self.moved_tail(newest);
            }
            self.count(Event::StandingVoid);
            self.flush();
            return;
        }
        let matched = self
            .match_chain(cursor)
            .filter(|&j| !(self.rule(Rule::PartWayEchoes) && self.part_way_before(j, cursor)));
        let Some(j) = matched else {
            if self.in_transit(cursor) {
                self.count(Event::TransitWait);
            } else {
                self.contradict_head();
                self.flush();
            }
            return;
        };
        if !self.retire_chain(j, now, oldest, cursor) {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        {
            self.chain_matched = true;
        }
        self.retire_net_null(now, oldest);
        let erase_pending = self.preds.iter().any(|p| p.op.erases());
        let hi = self.line.as_ref().map_or(lo, EditLine::hi);
        let clear = self
            .seen
            .get(usize::from(hi.saturating_sub(lo))..usize::from(span_end.saturating_sub(lo)))
            .is_none_or(|rest| rest.iter().all(|&s| s == Seen::Blank));
        if erase_pending && self.rule(Rule::VacateRefusal) && !clear {
            self.count(Event::VacateRefusal);
            self.flush();
        }
    }

    fn far_tail_moved(&self, row: u16, far: u16) -> bool {
        let Some((snap_row, from)) = self.tail else {
            return false;
        };
        if snap_row != row
            || self.line.as_ref().is_none_or(|l| l.end_known)
            || !self.rule(Rule::TailGuard)
            || !self.rule(Rule::StandingTail)
        {
            return false;
        }
        let lo = self.line.as_ref().map_or(from, |l| l.lo);
        (far.max(from)..self.grid_cols).any(|c| {
            let now = self
                .seen
                .get(usize::from(c.saturating_sub(lo)))
                .copied()
                .unwrap_or(Seen::Blank);
            let then = self.tail_at(row, c).unwrap_or(Seen::Blank);
            now != then && now.legacy().is_some()
        })
    }

    fn state_matches(
        &self,
        row: u16,
        lo: u16,
        text: &[char],
        state_cursor: u16,
        vacated: u16,
        cursor: Option<(u16, u16)>,
    ) -> bool {
        if cursor != Some((row, state_cursor)) {
            return false;
        }
        if !self.rule(Rule::WholeStateMatch) {
            return true;
        }
        let text_shown = text
            .iter()
            .enumerate()
            .all(|(i, &ch)| self.seen.get(i).is_some_and(|&s| cell_holds(s, ch)));
        let hi = Self::text_end(lo, text);
        text_shown
            && (!self.rule(Rule::VacatedBlankInMatch)
                || (hi..vacated).all(|c| {
                    self.seen
                        .get(usize::from(c - lo))
                        .is_some_and(|&s| s == Seen::Blank)
                }))
    }

    fn part_way_before(&mut self, j: usize, cursor: Option<(u16, u16)>) -> bool {
        let Some(line) = self.line.as_ref() else {
            return false;
        };
        let (row, lo) = (line.row, line.lo);
        let mut text = std::mem::take(&mut self.fold_text);
        text.clear();
        text.extend_from_slice(&line.text);
        let mut c = line.cursor;
        let mut found = false;
        let mut folds = 0;
        for p in self.preds.iter().take(j.saturating_sub(1)) {
            let hi_before = Self::text_end(lo, &text);
            let inside = c < hi_before;
            let from = c;
            Self::fold_op(&mut text, &mut c, lo, p);
            folds += 1;
            let hi_after = Self::text_end(lo, &text);
            let shown = |seen: &[Seen], text: &[char]| {
                text.iter()
                    .enumerate()
                    .all(|(i, &ch)| seen.get(i).is_some_and(|&s| cell_holds(s, ch)))
            };
            if matches!(p.op, Op::LineStart(_) | Op::LineEnd(_)) {
                let between = cursor
                    .is_some_and(|(r, col)| r == row && col > from.min(c) && col < from.max(c));
                if between && shown(&self.seen, &text) {
                    found = true;
                    break;
                }
                continue;
            }
            let part = match p.op {
                Op::Insert if inside => hi_after,
                Op::Erase | Op::Delete => hi_before,
                Op::Insert | Op::Left | Op::Right | Op::LineStart(_) | Op::LineEnd(_) => continue,
            };
            let step_back = self.rule(Rule::PartWayStepBack);
            let on_the_way = cursor.is_some_and(|(r, col)| {
                r == row
                    && if step_back {
                        col > c && col <= part
                    } else {
                        col == part
                    }
            });
            if part <= c || !on_the_way {
                continue;
            }
            let shown = shown(&self.seen, &text);
            let blanked = !p.op.erases()
                || self.seen.get(usize::from(hi_after.saturating_sub(lo))) == Some(&Seen::Blank);
            if shown && blanked {
                found = true;
                break;
            }
        }
        self.fold_text = text;
        self.folded(folds);
        found
    }

    fn match_chain(&mut self, cursor: Option<(u16, u16)>) -> Option<usize> {
        let line = self.line.as_ref()?;
        let (row, lo) = (line.row, line.lo);
        let mut text = std::mem::take(&mut self.fold_text);
        text.clear();
        text.extend_from_slice(&line.text);
        let mut c = line.cursor;
        let mut vacated = line.vacated.max(line.hi());
        let smallest = self.rule(Rule::SmallestMatch);
        let mut found = None;
        let mut folds = 0;
        for j in 0..=self.preds.len() {
            if let Some(p) = j.checked_sub(1).and_then(|i| self.preds.get(i)) {
                Self::fold_op(&mut text, &mut c, lo, p);
                folds += 1;
                vacated = vacated.max(Self::text_end(lo, &text));
            }
            if self.state_matches(row, lo, &text, c, vacated, cursor) {
                found = Some(j);
                if smallest {
                    break;
                }
            }
        }
        self.fold_text = text;
        self.folded(folds);
        found
    }

    fn retire_chain(
        &mut self,
        j: usize,
        now: Instant,
        oldest: &mut Option<Duration>,
        cursor: Option<(u16, u16)>,
    ) -> bool {
        let whole = self.rule(Rule::ChainAttributedWhole);
        if whole
            && let Some(p) = self
                .preds
                .iter()
                .take(j)
                .find(|p| !self.attributable(p, now))
                .copied()
        {
            self.owe(p.born);
            self.flush();
            return false;
        }
        let distinguishes = self.rule(Rule::ShiftProofDistinguishes);
        let fewer_rule = self.rule(Rule::InPlaceReadsFewer);
        let worlds = if distinguishes || fewer_rule {
            self.in_place_worlds(j, cursor)
        } else {
            Worlds::default()
        };
        if worlds.fewer && fewer_rule {
            self.count(Event::AmbiguousShift);
            self.flush();
            return false;
        }
        let ambiguous = worlds.other && distinguishes;
        let Some(mut line) = self.line.take() else {
            return false;
        };
        let mut credited_now = None;
        let mut learned = [0u8; Learned::ALL.len()];
        for _ in 0..j {
            let Some(p) = self.preds.pop_front() else {
                break;
            };
            let proves = !distinguishes || worlds.proves & p.class.proven_bit() != 0;
            Self::fold_op(&mut line.text, &mut line.cursor, line.lo, &p);
            self.folded(1);
            line.vacated = line.vacated.max(line.hi());
            if self
                .cancelled_pair
                .is_some_and(|(_, _, _, after)| p.seq >= after)
            {
                self.cancelled_pair = None;
            }
            if !whole && !self.attributable(&p, now) {
                self.owe(p.born);
                self.flush();
                return false;
            }
            #[cfg(any(test, debug_assertions))]
            {
                if self.credited.len() >= MAX_PREDICTIONS {
                    self.credited.pop_front();
                }
                self.credited.push_back(p.seq);
            }
            let sample = now.saturating_duration_since(p.born);
            self.record_rtt(sample);
            oldest.get_or_insert(sample);
            self.credit_op(&p, (line.row, line.cursor), proves);
            if let Some(n) = p.op.learned().and_then(|k| learned.get_mut(k.index())) {
                *n = n.saturating_add(1);
            }
            if p.epoch == self.epoch {
                credited_now = Some(p);
            }
        }
        self.line = Some(line);
        if ambiguous {
            self.count(Event::AmbiguousShift);
            self.flush();
            return false;
        }
        for (key, &n) in Learned::ALL.iter().zip(&learned) {
            for _ in 0..n {
                self.credit_binding(*key);
            }
        }
        if let Some(newest) = credited_now
            && self.confirmed_epoch
        {
            self.rebase_proof(&newest);
        }
        true
    }

    fn in_place_worlds(&mut self, j: usize, cursor: Option<(u16, u16)>) -> Worlds {
        const SHIFTS: u8 = PROVEN_INSERT_SHIFT | PROVEN_ERASE_SHIFT;
        let mut out = Worlds::default();
        let mut unproven = 0;
        let mut shift_pending = false;
        for p in &self.preds {
            let open = SHIFTS & !self.proven_for(p.epoch);
            unproven |= open;
            shift_pending |= p.class.proven_bit() & open != 0;
        }
        let Some(line) = self.line.as_ref().filter(|_| shift_pending) else {
            return out;
        };
        let (row, lo) = (line.row, line.lo);
        let vacated = usize::from(line.vacated.max(line.hi()).saturating_sub(lo));
        let width = usize::from(self.grid_cols.saturating_sub(lo));
        let mut s_text = std::mem::take(&mut self.fold_text);
        let mut cells = std::mem::take(&mut self.world_cells);
        let mut standing = [false; 3];
        let mut folds = 0;
        let flips = [PROVEN_INSERT_SHIFT, PROVEN_ERASE_SHIFT, SHIFTS];
        for (k, &flip) in flips.iter().enumerate() {
            if flip & unproven != flip {
                continue;
            }
            cells.clear();
            cells.extend((0..width).map(|i| match line.text.get(i) {
                Some(&ch) => Some(ch),
                None if i < vacated => Some(' '),
                None => None,
            }));
            let mut len = line.text.len();
            let mut cur = usize::from(line.cursor.saturating_sub(lo));
            s_text.clear();
            s_text.extend_from_slice(&line.text);
            let mut s_cursor = line.cursor;
            for (m, p) in self.preds.iter().enumerate().map(|(i, p)| (i + 1, p)) {
                Self::fold_op(&mut s_text, &mut s_cursor, lo, p);
                let stepped = Self::world_step(&mut cells, &mut len, &mut cur, p, flip);
                folds += 2;
                if !stepped {
                    standing[k] = true;
                    out.other = true;
                    out.fewer |= m < j;
                    break;
                }
                let at = lo.saturating_add(u16::try_from(cur).unwrap_or(u16::MAX));
                let shows = cursor == Some((row, at))
                    && cells.iter().enumerate().all(|(i, w)| {
                        w.is_none_or(|ch| self.seen.get(i).is_some_and(|&s| cell_holds(s, ch)))
                    });
                if !shows {
                    continue;
                }
                standing[k] = true;
                let own = m >= j
                    && at == s_cursor
                    && len == s_text.len()
                    && cells.iter().zip(&s_text).all(|(w, &ch)| *w == Some(ch));
                if !own {
                    out.other = true;
                    out.fewer |= m < j;
                    break;
                }
            }
        }
        for kind in [PROVEN_INSERT_SHIFT, PROVEN_ERASE_SHIFT] {
            let refuted = flips
                .iter()
                .zip(standing)
                .all(|(&flip, stood)| flip & kind == 0 || flip & unproven != flip || !stood);
            if unproven & kind != 0 && refuted {
                out.proves |= kind;
            }
        }
        self.fold_text = s_text;
        self.world_cells = cells;
        self.folded(folds);
        out
    }

    fn world_step(
        cells: &mut [Option<char>],
        len: &mut usize,
        cur: &mut usize,
        p: &Prediction,
        flip: u8,
    ) -> bool {
        match p.op {
            Op::Insert if flip & PROVEN_INSERT_SHIFT != 0 && *cur < *len => {
                if let Some(cell) = cells.get_mut(*cur) {
                    *cell = Some(p.ch);
                }
                *cur += 1;
            }
            Op::Insert => {
                // R-ROW: the buffer's end, one cell on, short of the last
                // column, as the chain's own fold keeps it.
                if *cur > *len || *len + 2 > cells.len() {
                    return false;
                }
                if let Some(span) = cells.get_mut(*cur..=*len) {
                    span.rotate_right(1);
                    if let Some(cell) = span.first_mut() {
                        *cell = Some(p.ch);
                    }
                }
                *len += 1;
                *cur += 1;
            }
            Op::Erase if *cur == 0 || *cur > *len => return false,
            Op::Erase if flip & PROVEN_ERASE_SHIFT != 0 && *cur < *len => {
                if let Some(cell) = cells.get_mut(*cur - 1) {
                    *cell = Some(' ');
                }
                *cur -= 1;
            }
            Op::Erase => {
                if let Some(span) = cells.get_mut(*cur - 1..*len) {
                    span.rotate_left(1);
                    if let Some(cell) = span.last_mut() {
                        *cell = Some(' ');
                    }
                }
                *len -= 1;
                *cur -= 1;
            }
            Op::Left if *cur == 0 => return false,
            Op::Left => *cur -= 1,
            Op::Right if *cur < *len => *cur += 1,
            Op::Right => {}
            Op::LineStart(_) => *cur = 0,
            Op::LineEnd(_) => *cur = *len,
            Op::Delete if *cur >= *len => return false,
            Op::Delete => {
                if let Some(span) = cells.get_mut(*cur..*len) {
                    span.rotate_left(1);
                    if let Some(cell) = span.last_mut() {
                        *cell = Some(' ');
                    }
                }
                *len -= 1;
            }
        }
        true
    }

    #[inline]
    fn proven_for(&self, epoch: u64) -> u8 {
        if epoch == self.epoch {
            self.proven
        } else if Some(epoch) == self.grandfathered_epoch {
            self.carried_proven
        } else {
            0
        }
    }

    #[inline]
    fn class_proven(&self, p: &Prediction) -> u8 {
        self.proven_for(p.epoch) & p.class.proven_bit()
    }

    fn pair_cancels(&self) -> bool {
        !self.rule(Rule::CancelsOnlyKnownShifts)
            || !self.preds.iter().any(|p| {
                matches!(p.class, OpClass::InsertShift | OpClass::EraseShift)
                    && self.class_proven(p) == 0
            })
    }

    fn credit_op(&mut self, p: &Prediction, cell: (u16, u16), proves: bool) {
        if p.epoch != self.epoch {
            return;
        }
        match p.op {
            Op::Insert => {
                self.confirm(*p);
                self.proof_cell = Some(cell);
            }
            Op::Left | Op::Right | Op::Erase | Op::LineStart(_) | Op::LineEnd(_) | Op::Delete => {
                if !self.confirmed_epoch {
                    if self.rule(Rule::MotionsNeverProve) {
                        return;
                    }
                    self.confirm(*p);
                }
                self.proof_cell = Some(cell);
            }
        }
        let bit = p.class.proven_bit();
        let proves = match p.class {
            OpClass::Glyph | OpClass::LineEdge | OpClass::DeleteFwd => false,
            OpClass::Motion => true,
            OpClass::InsertShift | OpClass::EraseShift => proves,
        };
        if proves && self.proven & bit == 0 {
            self.proven |= bit;
            self.count(if p.class == OpClass::Motion {
                Event::MotionProven
            } else {
                Event::ShiftProven
            });
        }
        self.line_end_at = None;
    }

    fn rebase_proof(&mut self, p: &Prediction) {
        let Some(line) = self.line.as_ref() else {
            return;
        };
        let (row, lo, hi) = (line.row, line.lo, line.hi());
        let (col, ch) = match line.text.last() {
            Some(&ch) => (hi.saturating_sub(1), ch),
            None => (lo, ' '),
        };
        let end_known = line.end_known;
        self.proof_echo = Some(Prediction {
            row,
            col,
            ch,
            op: Op::Insert,
            class: OpClass::Glyph,
            ghost: GhostCell::Typed,
            probe: false,
            ..*p
        });
        self.proof_line_end = end_known;
        self.proof_opaque = false;
        self.tail = Some((row, lo));
        self.tail_cells.clear();
        self.tail_cells.extend_from_slice(&self.seen);
    }

    fn retire_net_null(&mut self, now: Instant, oldest: &mut Option<Duration>) {
        let Some(line) = self.line.as_ref() else {
            return;
        };
        if self.preds.is_empty() {
            return;
        }
        let lo = line.lo;
        let mut text = std::mem::take(&mut self.fold_text);
        text.clear();
        text.extend_from_slice(&line.text);
        let mut c = line.cursor;
        for p in &self.preds {
            Self::fold_op(&mut text, &mut c, lo, p);
        }
        let null = text == line.text && c == line.cursor;
        let cell = (line.row, line.cursor);
        self.fold_text = text;
        self.folded(self.preds.len());
        if !null {
            return;
        }
        if !self.rule(Rule::NetNullUncredited) {
            while let Some(p) = self.preds.pop_front() {
                #[cfg(any(test, debug_assertions))]
                {
                    if self.credited.len() >= MAX_PREDICTIONS {
                        self.credited.pop_front();
                    }
                    self.credited.push_back(p.seq);
                }
                let sample = now.saturating_duration_since(p.born);
                self.record_rtt(sample);
                oldest.get_or_insert(sample);
                self.credit_op(&p, cell, true);
            }
            return;
        }
        let current = self.preds.iter().any(|p| p.epoch == self.epoch);
        if let Some(newest) = self.preds.iter().map(|p| p.born).max() {
            self.owe(newest);
        }
        self.preds.clear();
        self.count(Event::NetNull);
        if current && self.rule(Rule::UncreditedVoids) {
            self.void_proof();
        }
    }

    fn in_transit(&mut self, cursor: Option<(u16, u16)>) -> bool {
        let Some(line) = self.line.as_ref() else {
            return false;
        };
        let reach = !self.rule(Rule::TransitReach)
            || cursor.is_some_and(|(_, c)| c >= line.lo && c <= self.chain_far_end(line));
        if cursor.map(|(r, _)| r) != Some(line.row) || !reach {
            return false;
        }
        let lo = line.lo;
        let blanks = self.rule(Rule::TransitBlanks);
        let mut marks = std::mem::take(&mut self.ghost_marks);
        marks.clear();
        marks.extend(
            self.seen
                .iter()
                .map(|&s| if s == Seen::Blank && !blanks { 2 } else { 0 }),
        );
        let mut text = std::mem::take(&mut self.fold_text);
        text.clear();
        text.extend_from_slice(&line.text);
        let mut c = line.cursor;
        let mut vacated = line.vacated.max(line.hi());
        let mut writes_from = u16::MAX;
        for j in 0..=self.preds.len() {
            if let Some(p) = j.checked_sub(1).and_then(|i| self.preds.get(i)) {
                let from = c;
                Self::fold_op(&mut text, &mut c, lo, p);
                vacated = vacated.max(Self::text_end(lo, &text));
                if matches!(p.op, Op::Insert | Op::Erase | Op::Delete) {
                    writes_from = writes_from.min(from.min(c));
                }
            }
            for (i, mark) in marks
                .iter_mut()
                .enumerate()
                .take(usize::from(vacated.saturating_sub(lo)))
            {
                if *mark == 2 {
                    continue;
                }
                let seen = self.seen.get(i).copied();
                let held = match text.get(i) {
                    Some(&ch) => seen.is_some_and(|s| cell_holds(s, ch)),
                    None => seen == Some(Seen::Blank),
                };
                *mark = if held { 2 } else { 1 };
            }
        }
        for (i, mark) in marks.iter_mut().enumerate() {
            let col = lo.saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
            if *mark == 1 && col >= writes_from && self.seen.get(i) == Some(&Seen::Blank) {
                *mark = 2;
            }
        }
        let transit = !marks.contains(&1);
        self.fold_text = text;
        self.ghost_marks = marks;
        self.folded(self.preds.len());
        transit
    }

    fn compose_chain(&mut self) {
        self.ghosts.clear();
        let visible = self.visible_count();
        let Some(line) = self.line.as_ref().filter(|_| visible > 0) else {
            return;
        };
        let (row, lo) = (line.row, line.lo);
        let mut cells = std::mem::take(&mut self.fold_cells);
        cells.clear();
        cells.extend(
            line.text
                .iter()
                .enumerate()
                .map(|(i, &ch)| (ch, lo.saturating_add(u16::try_from(i).unwrap_or(u16::MAX)))),
        );
        let mut c = line.cursor;
        let mut far = line.hi();
        let mut newest = None;
        for p in self.preds.iter().take(visible) {
            Self::fold_cells(&mut cells, &mut c, lo, p, |ch| (ch, NO_SOURCE));
            far = far.max(Self::text_end(lo, &cells));
            newest = Some(*p);
        }
        let holds = |at: u16, want: Option<char>| {
            let i = usize::from(at.saturating_sub(lo));
            match (want, self.seen.get(i)) {
                (Some(ch), Some(&s)) => cell_holds(s, ch),
                (None, Some(&s)) => s == Seen::Blank,
                (want, None) => line.text.get(i).copied() == want,
            }
        };
        if let Some(newest) = newest {
            for col in lo..far {
                let i = usize::from(col - lo);
                let want = cells.get(i).copied();
                if holds(col, want.map(|(ch, _)| ch)) {
                    continue;
                }
                let ghost = match want {
                    None => GhostCell::Vacated,
                    Some((ch, from))
                        if from != NO_SOURCE && from != col && holds(from, Some(ch)) =>
                    {
                        GhostCell::Moved { from }
                    }
                    Some(_) => GhostCell::Typed,
                };
                self.ghosts.push(Prediction {
                    row,
                    col,
                    ch: want.map_or(' ', |(ch, _)| ch),
                    op: Op::Insert,
                    class: OpClass::Glyph,
                    ghost,
                    ..newest
                });
            }
            let carried = newest.epoch != self.epoch;
            if !carried || self.rule(Rule::CarryCursor) {
                self.ghost_cursor = Some((row, c));
            }
        }
        self.fold_cells = cells;
        self.folded(visible);
    }

    /// Returns the known edit line `(row, lo, end, cursor)` for tests.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn edit_line_for_test(&self) -> Option<(u16, u16, u16, u16)> {
        self.line.as_ref().map(|l| (l.row, l.lo, l.hi(), l.cursor))
    }

    /// Returns `true` if the last reconcile matched and retired a state chain.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn chain_matched_for_test(&self) -> bool {
        self.chain_matched
    }

    /// Returns `true` if a state chain is currently in flight.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub fn engaged_for_test(&self) -> bool {
        self.engaged()
    }

    /// Returns the number of leading pending operations that pass the display gate.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub fn visible_ops_for_test(&self) -> usize {
        self.visible_count()
    }

    /// Records a line-terminator submission (`Enter`, `Ctrl+J`, `Ctrl+M`), ending the confirmation epoch.
    #[inline]
    pub fn note_line_submit(&mut self, now: Instant) {
        self.note_line_submit_at(None, now);
    }

    /// Records a line-terminator submission at authenticated prompt ID `prompt`.
    pub fn note_line_submit_at(&mut self, prompt: Option<u64>, now: Instant) {
        self.prompt_gate = prompt.map_or(PromptGate::Shut, |after| PromptGate::Open { after });
        self.owe(now);
        self.end_epoch();
        self.declined_tail = 0;
    }

    /// Records unmodelled PTY input (chords, pastes, tabs), flushing pending predictions.
    pub fn note_unmodelled_input(&mut self, now: Instant) {
        let _ = self.first_key();
        self.owe(now);
        self.moved_tail(now);
        self.flush();
    }

    fn end_epoch(&mut self) {
        self.count(Event::EpochEnd);
        if self.confirmed_epoch {
            self.grandfathered_epoch = Some(self.epoch);
            self.carried_proven = self.proven;
            self.carried_proof = self
                .proof_echo
                .map(|p| (p, self.proof_line_end, self.proof_opaque));
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.last_credit = None;
        self.void_proof();
        self.anchor_lost = false;
        self.cancelled_at = None;
        self.line_end_at = None;
        self.cancelled_pair = None;
    }

    const fn confirm(&mut self, p: Prediction) {
        if p.epoch != self.epoch {
            return;
        }
        self.confirmed_epoch = true;
        self.proof_cell = Some(self.cell_after(&p));
        self.proof_echo = Some(p);
        self.proof_line_end = self.tail_line_end;
        self.proof_opaque = false;
    }

    fn void_proof(&mut self) {
        self.confirmed_epoch = false;
        self.proof_cell = None;
        self.proof_echo = None;
        self.proof_line_end = false;
        self.proof_opaque = false;
        if self.rule(Rule::ProofsEndWithTheEpoch) {
            self.proven = 0;
        }
        if !self.engaged() {
            self.line = None;
        }
    }

    fn take_tail(
        &mut self,
        cursor: (u16, u16),
        at: (u16, u16),
        cells: Option<&[Seen]>,
        cols: u16,
        inherits: Option<bool>,
        now: Instant,
    ) {
        self.tail = None;
        self.tail_cells.clear();
        self.tail_line_end = inherits.unwrap_or(false);
        let window = self.glitch_window();
        let tail_in_flight = self
            .moved_tail_at
            .is_some_and(|t| now.saturating_duration_since(t) <= window);
        self.run_hidden = tail_in_flight && self.hidden_tail_at == Some(at);
        let Some(cells) = cells.filter(|_| at == cursor) else {
            return;
        };
        let from = cursor.1;
        self.tail_cells.extend(
            (from..cols).map(|c| cells.get(usize::from(c)).copied().unwrap_or(Seen::Blank)),
        );
        self.tail = Some((cursor.0, from));
        if inherits.is_none() {
            self.tail_line_end =
                !tail_in_flight && self.tail_cells.iter().all(|s| s.legacy().is_none());
        }
    }

    #[inline]
    const fn parked(&self, flag: Option<bool>) -> bool {
        match flag {
            Some(pending) if self.rule(Rule::EngineWrapFlag) => pending,
            _ => self.deferred_wrap,
        }
    }

    fn take_floor(&mut self, cursor: (u16, u16), at: (u16, u16), below: Option<&[Seen]>) {
        self.floor = None;
        self.floor_cells.clear();
        let Some(cells) = below.filter(|_| at.0 == cursor.0) else {
            return;
        };
        self.floor_cells.extend_from_slice(cells);
        self.floor = cursor.0.checked_add(1);
    }

    fn floor_moved(
        &self,
        cursor: Option<(u16, u16)>,
        observe: &mut impl FnMut(u16, u16) -> Seen,
    ) -> bool {
        let (Some(floor), Some(p)) = (self.floor, self.proof_echo) else {
            return false;
        };
        if p.row.checked_add(1) != Some(floor) || cursor.is_some_and(|(r, _)| r >= floor) {
            return false;
        }
        let named = |c: u16, ch: char| {
            self.preds
                .iter()
                .any(|g| (g.row, g.col, g.ch) == (floor, c, ch))
        };
        (0u16..).zip(&self.floor_cells).any(|(c, &was)| {
            was.legacy().is_none() && observe(floor, c).legacy().is_some_and(|ch| !named(c, ch))
        })
    }

    #[inline]
    fn moved_tail(&mut self, born: Instant) {
        self.moved_tail_at = Some(self.moved_tail_at.map_or(born, |t| t.max(born)));
    }

    #[inline]
    const fn cell_after(&self, p: &Prediction) -> (u16, u16) {
        self.after(p.row, p.col)
    }

    #[inline]
    const fn after(&self, row: u16, col: u16) -> (u16, u16) {
        let next = col.saturating_add(1);
        let parked = self.grid_cols > 0 && next >= self.grid_cols;
        (row, if parked { col } else { next })
    }

    #[inline]
    fn tail_at(&self, row: u16, col: u16) -> Option<Seen> {
        let (_, from) = self.tail.filter(|&(r, from)| r == row && from <= col)?;
        Some(
            self.tail_cells
                .get(usize::from(col.saturating_sub(from)))
                .copied()
                .unwrap_or(Seen::Blank),
        )
    }

    fn tail_clean(
        &self,
        p: &Prediction,
        observe: &mut impl FnMut(u16, u16) -> Seen,
        clean_row: &mut Option<u16>,
        cursor: Option<(u16, u16)>,
    ) -> bool {
        if *clean_row == Some(p.row) {
            return true;
        }
        let over_its_own =
            |held: Option<Seen>, ch: char| held.is_none_or(|s| s.legacy().is_none_or(|c| c == ch));
        if !over_its_own(self.tail_at(p.row, p.col), p.ch) {
            return false;
        }
        let advance = self.rule(Rule::CursorAdvance);
        let clean = (p.col.saturating_add(1)..self.grid_cols).all(|c| {
            let seen = observe(p.row, c);
            let held = self.tail_at(p.row, c);
            let expected = held.unwrap_or(Seen::Blank);
            let pair = self.cancelled_pair.is_some_and(|(r, pc, ch, _)| {
                (r, pc) == (p.row, c) && seen.legacy() == Seen::of_cell(ch, false).legacy()
            });
            match self.preds.iter().find(|g| (g.row, g.col) == (p.row, c)) {
                Some(g) => {
                    over_its_own(held, g.ch)
                        && (seen == expected
                            || pair
                            || (seen.legacy() == Seen::of_cell(g.ch, false).legacy()
                                && (!advance || self.echoed_past(cursor, g))))
                }
                None => seen == expected || pair,
            }
        });
        if clean {
            *clean_row = Some(p.row);
        }
        clean
    }

    fn appended(
        &self,
        (p, own): (&Prediction, Seen),
        observe: &mut impl FnMut(u16, u16) -> Seen,
        clean_row: &mut Option<u16>,
        cursor: Option<(u16, u16)>,
    ) -> bool {
        !(self.run_inside && self.rule(Rule::InsideRunsNeverProve))
            && self.echo_unmarked(p, own)
            && (self.tail_line_end || self.tail_clean(p, observe, clean_row, cursor))
    }

    #[inline]
    fn echo_unmarked(&self, p: &Prediction, seen: Seen) -> bool {
        !self.rule(Rule::MarkedEcho)
            || !matches!(seen, Seen::Opaque(_))
            || matches!(self.tail_at(p.row, p.col), Some(Seen::Opaque(_)))
    }

    #[inline]
    fn holds_its_glyph(p: &Prediction, seen: Seen) -> bool {
        seen.legacy() == Seen::of_cell(p.ch, false).legacy()
    }

    fn tail_decorated(&self, p: &Prediction) -> bool {
        (p.col..self.grid_cols)
            .any(|c| self.tail_at(p.row, c).is_some_and(|s| s.legacy().is_some()))
    }

    fn decline(&mut self, ch: char, now: Instant) -> bool {
        self.owe(now);
        if ch.is_control() {
            self.flush();
            return false;
        }
        self.end_epoch();
        self.anchor_lost = true;
        self.declined_tail = self.declined_tail.saturating_add(1);
        false
    }

    fn observe_coordinates(&mut self, coords: u64) {
        if self.coords.is_some_and(|seen| seen != coords) {
            let gate = self.prompt_gate;
            self.reset();
            if self.rule(Rule::ScrollKeepsFirstKey) {
                self.prompt_gate = gate;
            }
        }
        self.coords = Some(coords);
    }

    fn observe_cancelled_cursor(&mut self, cursor: Option<(u16, u16)>, now: Instant) {
        if !self.preds.is_empty() {
            return;
        }
        let Some((row, col)) = self.cancelled_at else {
            return;
        };
        let same = cursor == Some((row, col));
        let ahead = col
            .checked_add(1)
            .filter(|&c| c < self.grid_cols)
            .is_some_and(|c| cursor == Some((row, c)));
        let before_wrap = col == 0
            && self.grid_cols > 0
            && row
                .checked_sub(1)
                .is_some_and(|r| cursor == Some((r, self.grid_cols - 1)));
        if !(same || ahead || before_wrap) {
            self.owe(now);
            self.flush();
        }
    }

    /// Reconciles pending predictions against post-output cursor and plain cell observations.
    pub fn reconcile(
        &mut self,
        real_cursor: Option<(u16, u16)>,
        coords: u64,
        alt_screen: bool,
        now: Instant,
        mut observe: impl FnMut(u16, u16) -> Option<char>,
    ) {
        self.edit_capable = false;
        if self.engaged() {
            self.flush();
        }
        self.reconcile_cells(real_cursor, coords, alt_screen, now, |row, col| {
            observe(row, col).map_or(Seen::Blank, |ch| Seen::of_cell(ch, false))
        });
    }

    /// Reconciles pending predictions against post-output cursor and [`Seen`] cell observations.
    pub fn reconcile_seen(
        &mut self,
        real_cursor: Option<(u16, u16)>,
        coords: u64,
        alt_screen: bool,
        now: Instant,
        observe: impl FnMut(u16, u16) -> Seen,
    ) {
        self.edit_capable = true;
        self.reconcile_cells(real_cursor, coords, alt_screen, now, observe);
    }

    fn reconcile_cells(
        &mut self,
        real_cursor: Option<(u16, u16)>,
        coords: u64,
        alt_screen: bool,
        now: Instant,
        mut observe: impl FnMut(u16, u16) -> Seen,
    ) {
        #[cfg(any(test, debug_assertions))]
        {
            self.chain_matched = false;
        }
        self.observe_coordinates(coords);
        if alt_screen {
            self.flush();
            self.deferred_wrap = false;
            return;
        }

        let mut oldest_confirmed: Option<Duration> = None;
        let mut clean_row: Option<u16> = None;
        let proof_seen = self
            .proof_echo
            .filter(|_| self.confirmed_epoch)
            .map(|p| observe(p.row, p.col));
        if !self.engaged()
            && self.rule(Rule::StandingGlyph)
            && let (Some(p), Some(seen)) = (self.proof_echo, proof_seen)
            && !Self::holds_its_glyph(&p, seen)
        {
            self.moved_tail(p.born);
            self.void_proof();
            self.count(Event::StandingVoid);
        }
        let proof_before = self.proof_echo.map(|p| p.seq);

        let chain = self.engaged();
        if chain {
            self.reconcile_chain(real_cursor, now, &mut observe, &mut oldest_confirmed);
        }
        self.held_wait = None;
        self.text_wait = None;
        while let Some(p) = self.preds.front().copied().filter(|_| !chain) {
            let own = observe(p.row, p.col);
            if own == Seen::Opaque(None) && self.rule(Rule::OpaqueHead) {
                self.count(Event::OpaqueRefusal);
                self.flush();
                break;
            }
            match own.legacy() {
                Some(c) if c == p.ch => {
                    if self.rule(Rule::CursorAdvance) && !self.echoed_past(real_cursor, &p) {
                        self.count(Event::AdvanceWait);
                        if self.rule(Rule::DarkBehindAWait)
                            && real_cursor == Some((p.row, p.col))
                            && self.tail_at(p.row, p.col).and_then(Seen::legacy) != Some(c)
                        {
                            self.held_wait = Some(p.seq);
                            self.count(Event::HeldWait);
                        }
                        break;
                    }
                    self.retire_echoed(
                        (p, own),
                        now,
                        &mut oldest_confirmed,
                        &mut observe,
                        &mut clean_row,
                        real_cursor,
                    );
                }
                Some(c)
                    if self.rule(Rule::ScreenAsItWas)
                        && real_cursor == Some((p.row, p.col))
                        && self.tail_at(p.row, p.col).and_then(Seen::legacy) == Some(c) =>
                {
                    // DARK OVER TEXT ([`Rule::DarkOverText`]): not known to be past
                    // the line's end, `c` may be the line's own text, which this
                    // key's echo shifts right of its glyph. The guess waits off the
                    // glass, and what was typed after it with it: painted, it hid
                    // that text unshifted until the echo (`sudo` over `du -sh`).
                    if self.text_under(p.row, p.col, p.ch).is_some() {
                        self.text_wait = Some(p.seq);
                        self.count(Event::TextWait);
                    }
                    break;
                }
                Some(_) => {
                    self.flush();
                    break;
                }
                None => {
                    // As far as the run's echoes reach, as for any glyph
                    // (`echoed_past`).
                    if p.ch == ' '
                        && matches!(real_cursor, Some((rr, rc))
                            if rr == p.row && rc > p.col && self.within_reach(rr, rc))
                    {
                        self.retire_echoed(
                            (p, own),
                            now,
                            &mut oldest_confirmed,
                            &mut observe,
                            &mut clean_row,
                            real_cursor,
                        );
                    } else {
                        break;
                    }
                }
            }
        }
        if !chain
            && self.text_wait.is_none()
            && let Some(i) = self.first_over_text().filter(|&i| i > 0)
            && let Some(p) = self.preds.get(i).copied()
            && observe(p.row, p.col).legacy().is_some_and(|g| g != p.ch)
        {
            self.text_wait = Some(p.seq);
            self.count(Event::TextWait);
        }
        if let Some(sample) = oldest_confirmed {
            self.record_echo_regime(sample);
        }

        if self.confirmed_epoch
            && !self.proof_opaque
            && self.proof_echo.map(|p| p.seq) == proof_before
            && self.rule(Rule::MarkedEcho)
            && let Some(p) = self.proof_echo
            && matches!(proof_seen, Some(Seen::Opaque(_)))
        {
            self.moved_tail(p.born);
            self.void_proof();
            self.count(Event::StandingVoid);
        }

        if !self.engaged()
            && self.rule(Rule::CreditTakenBack)
            && let Some((r, c, born)) = self.last_credit
            && real_cursor.is_some_and(|(cr, cc)| {
                cr == r && (cc < c || (cc == c && c.saturating_add(1) != self.grid_cols))
            })
        {
            self.last_credit = None;
            self.owe(born);
            self.moved_tail(born);
            self.void_proof();
            self.count(Event::CreditTakenBack);
        }

        if self.confirmed_epoch
            && !self.engaged()
            && self.rule(Rule::FloorBelow)
            && self.floor_moved(real_cursor, &mut observe)
            && let Some(p) = self.proof_echo
        {
            self.moved_tail(p.born);
            self.void_proof();
            self.count(Event::FloorVoid);
        }

        let engaged = self.engaged();
        if self.confirmed_epoch
            && !engaged
            && !self.proof_line_end
            && self.proof_echo.map(|p| p.seq) == proof_before
            && self.rule(Rule::TailGuard)
            && self.rule(Rule::StandingTail)
            && let Some(p) = self.proof_echo
            && !self.tail_clean(&p, &mut observe, &mut None, real_cursor)
        {
            self.moved_tail(p.born);
            self.void_proof();
            self.count(Event::StandingVoid);
        }

        let carrying = !engaged
            && self
                .grandfathered_epoch
                .is_some_and(|e| self.preds.iter().any(|g| g.epoch == e));
        if carrying && let Some((p, line_end, opaque)) = self.carried_proof {
            let own = observe(p.row, p.col);
            let marked = (!opaque && self.rule(Rule::MarkedEcho) && matches!(own, Seen::Opaque(_)))
                || (self.rule(Rule::StandingGlyph) && !Self::holds_its_glyph(&p, own));
            let moved = !line_end
                && self.rule(Rule::TailGuard)
                && self.rule(Rule::StandingTail)
                && !self.tail_clean(&p, &mut observe, &mut None, real_cursor);
            if marked || moved {
                self.moved_tail(p.born);
                self.grandfathered_epoch = None;
                self.carried_proof = None;
                self.count(Event::StandingVoid);
            }
        }

        if let Some(&p) = self.preds.front().filter(|_| !self.engaged()) {
            match real_cursor {
                Some(rc) if rc == (p.row, p.col) => {}
                Some((rr, rc))
                    if p.col == 0
                        && self.grid_cols > 0
                        && rr.saturating_add(1) == p.row
                        && rc.saturating_add(1) == self.grid_cols => {}
                _ => self.flush(),
            }
        }

        self.settle_chain();
        self.observe_cancelled_cursor(real_cursor, now);

        self.deferred_wrap = match real_cursor {
            Some((rr, rc)) if self.grid_cols > 0 && rc.saturating_add(1) == self.grid_cols => {
                observe(rr, rc).legacy().is_some()
                    || rc
                        .checked_sub(1)
                        .and_then(|lead| observe(rr, lead).legacy())
                        .is_some_and(|lead| self.cells_of(lead) == 2)
            }
            _ => false,
        };
    }

    /// Expires stale guesses and returns the slice of predictions to paint on the current frame.
    pub fn overlay(&mut self, now: Instant) -> &[Prediction] {
        self.expire(now);
        self.ghost_cursor = None;
        if self.engaged() {
            self.compose_chain();
            return &self.ghosts;
        }
        let visible = self.visible_count();
        self.preds.make_contiguous().get(..visible).unwrap_or(&[])
    }

    /// Calls [`Self::overlay`] and returns both the overlay predictions and [`Self::overlay_cursor`].
    pub fn overlay_frame(&mut self, now: Instant) -> (&[Prediction], Option<(u16, u16)>) {
        let _ = self.overlay(now);
        let cursor = self.ghost_cursor;
        let cells = if self.engaged() {
            &self.ghosts[..]
        } else {
            let visible = self.visible_count();
            self.preds.as_slices().0.get(..visible).unwrap_or(&[])
        };
        (cells, cursor)
    }

    /// Returns the predicted cursor `(row, col)` when the last [`Self::overlay`] frame came from a state chain.
    #[must_use]
    #[inline]
    pub const fn overlay_cursor(&self) -> Option<(u16, u16)> {
        self.ghost_cursor
    }

    /// Expires guesses past their glitch window without composing a frame; returns `true` if any expired.
    #[inline]
    pub fn expire_due(&mut self, now: Instant) -> bool {
        let before = self.preds.len();
        self.expire(now);
        self.preds.len() != before
    }

    fn expire(&mut self, now: Instant) {
        let window = self.glitch_window();
        if self.engaged() {
            let due = self
                .preds
                .front()
                .is_some_and(|head| now.saturating_duration_since(head.born) > window);
            if due && self.rule(Rule::StateExpiryFlushes) {
                self.count(Event::Expiry);
                self.count(Event::ExpiryOwe);
                self.contradict_head();
                self.flush();
                // SAFETY: timeout window >= GLITCH_FLOOR_MS > DISPLAY_OPEN_MS, so feeding timeouts into RTT/regime would diverge SRTT and let unechoed keys latch adaptive_slow.
                self.expiry_backoff = self
                    .expiry_backoff
                    .saturating_add(1)
                    .min(EXPIRY_BACKOFF_MAX);
            }
            return;
        }
        let owes = self.rule(Rule::ExpiryOwes);
        let mut timed_out = false;
        while let Some(oldest) = self.preds.front().copied() {
            if now.saturating_duration_since(oldest.born) <= window {
                break;
            }
            self.preds.pop_front();
            if self.text_wait == Some(oldest.seq) {
                self.text_wait = self.preds.front().map(|p| p.seq);
            }
            timed_out = true;
            self.count(Event::Expiry);
            self.line_end_at = None;
            if owes {
                self.owe(oldest.born);
                for later in &mut self.preds {
                    later.owed = later.owed.max(Some(oldest.born));
                }
                if oldest.epoch == self.epoch {
                    self.void_proof();
                }
                self.count(Event::ExpiryOwe);
            }
        }
        if timed_out {
            // SAFETY: timeout window >= GLITCH_FLOOR_MS > DISPLAY_OPEN_MS, so feeding timeouts into RTT/regime would diverge SRTT and let unechoed keys latch adaptive_slow.
            self.expiry_backoff = self
                .expiry_backoff
                .saturating_add(1)
                .min(EXPIRY_BACKOFF_MAX);
        }
    }

    #[inline]
    fn glitch_window(&self) -> Duration {
        let probe = (GLITCH_FLOOR_MS << self.expiry_backoff.min(EXPIRY_BACKOFF_MAX)) as f32;
        let floor = probe.min(GLITCH_CEILING_MS as f32);
        let ms = self.srtt.map_or(floor, |s| {
            (s * GLITCH_SRTT_MULT).clamp(floor, GLITCH_CEILING_MS as f32)
        });
        Duration::from_millis(ms as u64)
    }

    fn visible_count(&self) -> usize {
        let shown = match self.mode {
            PredictMode::Off => 0,
            PredictMode::Always => self.preds.iter().take_while(|p| !p.probe).count(),
            PredictMode::Adaptive if !self.adaptive_slow => 0,
            PredictMode::Adaptive => {
                let shown = self.preds.iter().take_while(|p| self.op_visible(p)).count();
                self.held_wait
                    .and_then(|s| self.preds.iter().position(|p| p.seq == s))
                    .map_or(shown, |i| shown.min(i + 1))
            }
        };
        self.text_wait
            .and_then(|s| self.preds.iter().position(|p| p.seq == s))
            .map_or(shown, |i| shown.min(i))
    }

    #[inline]
    fn op_visible(&self, p: &Prediction) -> bool {
        let class_ok = |proven: u8| {
            let rule = match p.class {
                OpClass::Glyph => return true,
                OpClass::LineEdge | OpClass::DeleteFwd => return !p.probe,
                OpClass::Motion => Rule::MotionClassProof,
                OpClass::InsertShift => Rule::InsertShiftProof,
                OpClass::EraseShift => Rule::EraseShiftProof,
            };
            !self.rule(rule) || proven & p.class.proven_bit() != 0
        };
        (p.epoch == self.epoch && self.confirmed_epoch && class_ok(self.proven))
            || (Some(p.epoch) == self.grandfathered_epoch && class_ok(self.carried_proven))
    }

    /// Returns `true` when no predictions or state-chain operations are pending.
    #[inline]
    pub fn idle(&self) -> bool {
        self.preds.is_empty()
    }

    /// Returns the half-open `(row_start, col_start, row_end, col_end)` bounding box of pending guesses.
    #[must_use]
    pub fn pending_bounds(&self) -> Option<(usize, usize, usize, usize)> {
        let span = self.line.as_ref().filter(|_| self.engaged()).map(|l| {
            let row = usize::from(l.row);
            let far = usize::from(self.chain_far_end(l)).saturating_add(1);
            (row, usize::from(l.lo), row.saturating_add(1), far)
        });
        self.preds
            .iter()
            .map(|p| {
                let row = usize::from(p.row);
                let col = usize::from(p.col);
                (row, col, row.saturating_add(1), col.saturating_add(1))
            })
            .chain(span)
            .reduce(|(r0, c0, r1, c1), (s0, d0, s1, d1)| {
                (r0.min(s0), c0.min(d0), r1.max(s1), c1.max(d1))
            })
    }

    /// Returns the press-order sequence number of the newest pending operation.
    #[must_use]
    #[inline]
    pub fn newest_op(&self) -> Option<u64> {
        self.preds.back().map(|p| p.seq)
    }

    /// Returns [`Self::newest_op`] if a new operation was armed since `before`.
    #[must_use]
    #[inline]
    pub fn armed_since(&self, before: Option<u64>) -> Option<u64> {
        self.newest_op()
            .filter(|op| before.is_none_or(|before| *op > before))
    }

    /// Returns `true` if operation `op` is still pending.
    #[must_use]
    #[inline]
    pub fn op_is_pending(&self, op: u64) -> bool {
        self.preds.iter().any(|pending| pending.seq == op)
    }

    /// Returns the press-order sequence number of the newest operation shown in the overlay.
    #[must_use]
    #[inline]
    pub fn shown_through(&self) -> Option<u64> {
        self.visible_count()
            .checked_sub(1)
            .and_then(|newest| self.preds.get(newest))
            .map(|p| p.seq)
    }

    /// Returns an iterator over all pending glyph predictions from oldest to newest.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn pending(&self) -> impl ExactSizeIterator<Item = &Prediction> + '_ {
        self.preds.iter()
    }

    /// Returns `true` when predictions are actively displayed or require an expiry cleanup frame.
    #[inline]
    pub fn is_displaying(&self, now: Instant) -> bool {
        match self.preds.front() {
            None => false,
            Some(oldest) => {
                now.saturating_duration_since(oldest.born) > self.glitch_window()
                    || self.visible_count() > 0
            }
        }
    }

    /// Returns the glitch-expiry instant of the oldest pending prediction, if any.
    #[inline]
    pub fn next_deadline(&self) -> Option<Instant> {
        let window = self.glitch_window();
        self.preds.front().map(|p| p.born + window)
    }

    /// Drops all pending predictions and epoch proof on a coordinate-space change while keeping the RTT estimate.
    pub fn reset(&mut self) {
        self.flush();
        self.prompt_gate = PromptGate::Shut;
        self.coords = None;
        self.deferred_wrap = false;
    }

    /// Resets all predictor state including the learned link RTT estimate.
    pub fn reset_session(&mut self) {
        self.reset();
        self.owed_at = None;
        self.srtt = None;
        self.slow_samples = 0;
        self.fast_samples = 0;
        self.adaptive_slow = false;
        self.expiry_backoff = 0;
        self.bindings = EditorBindings::default();
    }

    /// Extracts the current session's [`LinkEstimate`] and resets the predictor.
    pub fn take_link_estimate(&mut self) -> LinkEstimate {
        let est = LinkEstimate {
            srtt: self.srtt,
            slow_samples: self.slow_samples,
            fast_samples: self.fast_samples,
            adaptive_slow: self.adaptive_slow,
            expiry_backoff: self.expiry_backoff,
            owed_at: self.owed_with_pending(),
            bindings: self.bindings,
        };
        self.reset_session();
        est
    }

    /// Restores a session's [`LinkEstimate`] and clears screen-specific predictions and epoch state.
    pub fn restore_link_estimate(&mut self, est: LinkEstimate) {
        self.reset();
        self.srtt = est.srtt;
        self.slow_samples = est.slow_samples;
        self.fast_samples = est.fast_samples;
        self.adaptive_slow = est.adaptive_slow;
        self.expiry_backoff = est.expiry_backoff;
        self.owed_at = est.owed_at;
        self.bindings = est.bindings;
    }

    fn owed_with_pending(&self) -> Option<Instant> {
        let edit_ops_owe = self.rule(Rule::EditOpsOwe);
        self.preds
            .iter()
            .filter(|p| edit_ops_owe || p.op == Op::Insert)
            .map(|p| p.born)
            .chain(self.glyph_owes.iter().map(|&(_, _, at)| at))
            .chain(self.owed_at)
            .max()
    }

    fn owe_glyph(&mut self, seq: u64, ch: char, at: Instant, now: Instant) {
        let ceiling = Duration::from_millis(GLITCH_CEILING_MS);
        while self
            .glyph_owes
            .front()
            .is_some_and(|&(_, _, t)| now.saturating_duration_since(t) > ceiling)
        {
            self.glyph_owes.pop_front();
        }
        if self.glyph_owes.len() >= MAX_GLYPH_OWES
            && let Some((_, _, oldest)) = self.glyph_owes.pop_front()
        {
            self.owe(oldest);
            for later in &mut self.preds {
                later.owed = later.owed.max(Some(oldest));
            }
        }
        self.glyph_owes.push_back((seq, ch, at));
        self.count(Event::TypedOwe);
    }

    #[inline]
    fn owe(&mut self, at: Instant) {
        self.owed_at = Some(self.owed_at.map_or(at, |t| t.max(at)));
    }

    fn echoed_past(&self, cursor: Option<(u16, u16)>, p: &Prediction) -> bool {
        let last_col =
            |g: &Prediction| self.grid_cols > 0 && g.col.saturating_add(1) == self.grid_cols;
        let parked = self.tail_line_end || !self.rule(Rule::ParkedAtTheEnd);
        cursor.is_some_and(|(r, c)| {
            (r == p.row && c > p.col && self.within_reach(r, c))
                || ((r, c) == (p.row, p.col) && last_col(p) && parked)
                || (r > p.row
                    && ((self.preds.iter().any(|g| g.row == r) && self.within_reach(r, c))
                        || ((r, c) == (p.row.saturating_add(1), 0)
                            && (last_col(p)
                                || self.preds.iter().any(|g| g.row == p.row && last_col(g))))))
        })
    }

    fn within_reach(&self, r: u16, c: u16) -> bool {
        let pair = self
            .cancelled_pair
            .is_some_and(|(pr, pc, _, _)| pr == r && c <= self.after(pr, pc).1);
        !self.rule(Rule::CursorReach)
            || pair
            || self
                .preds
                .iter()
                .any(|g| g.row == r && c <= self.after(g.row, g.col).1)
    }

    fn attributable(&self, p: &Prediction, now: Instant) -> bool {
        let window = self.glitch_window();
        let live = |t: Instant| now.saturating_duration_since(t) <= window;
        if p.owed.is_some_and(live) {
            return false;
        }
        if p.op != Op::Insert
            && self.rule(Rule::EditOwedAfter)
            && self
                .owed_at
                .is_some_and(|t| live(t) && p.owed.is_none_or(|o| t > o))
        {
            return false;
        }
        if p.stale
            && self.rule(Rule::StaleAnchor)
            && (self.owed_at.is_some_and(live)
                || self
                    .preds
                    .iter()
                    .any(|g| g.seq > p.seq && g.op == Op::Insert && g.ch == p.ch)
                || self
                    .glyph_owes
                    .iter()
                    .any(|&(seq, ch, at)| seq > p.seq && ch == p.ch && live(at)))
        {
            return false;
        }
        !(self.rule(Rule::TypedOwesSameGlyph)
            && self
                .glyph_owes
                .iter()
                .any(|&(seq, ch, at)| seq < p.seq && ch == p.ch && live(at)))
    }

    fn retire_echoed(
        &mut self,
        (p, own): (Prediction, Seen),
        now: Instant,
        oldest: &mut Option<Duration>,
        observe: &mut impl FnMut(u16, u16) -> Seen,
        clean_row: &mut Option<u16>,
        cursor: Option<(u16, u16)>,
    ) {
        self.preds.pop_front();
        if self
            .cancelled_pair
            .is_some_and(|(_, _, _, after)| p.seq >= after)
        {
            self.cancelled_pair = None;
        }
        if self.attributable(&p, now) {
            #[cfg(any(test, debug_assertions))]
            {
                if self.credited.len() >= MAX_PREDICTIONS {
                    self.credited.pop_front();
                }
                self.credited.push_back(p.seq);
            }
            let sample = now.saturating_duration_since(p.born);
            self.record_rtt(sample);
            oldest.get_or_insert(sample);
            self.last_credit = Some((p.row, p.col, p.born));
            if p.epoch != self.epoch {
                return;
            }
            if !self.rule(Rule::TailGuard) || self.appended((&p, own), observe, clean_row, cursor) {
                // A space proves no line by itself ([`Rule::SpaceNeverProves`]):
                // its echo, a blank with the cursor past it, is what any output
                // that steps the cursor over a blank draws, a prompt's trailing
                // space among them. Typed while `Password: ` was still being
                // drawn, on the cell of its last space, a space was credited with
                // that space, proved the line where the prompt ends, and the
                // password typed there was painted. It measures the link, and on
                // a line a glyph has proven it carries the proof on.
                if p.ch == ' ' && !self.confirmed_epoch && self.rule(Rule::SpaceNeverProves) {
                    self.retired_as_append(&p);
                    return;
                }
                if self.tail_decorated(&p) {
                    self.count(Event::DecoratedProof);
                }
                self.retired_as_append(&p);
                self.confirm(p);
                self.proof_opaque = matches!(own, Seen::Opaque(_));
                self.extend_line(&p);
            } else {
                self.moved_tail(p.born);
                self.void_proof();
                self.count(Event::TailRefusal);
            }
            return;
        }
        if p.epoch == self.epoch && self.rule(Rule::UncreditedVoids) {
            self.void_proof();
        }
        let appended = self.appended((&p, own), observe, clean_row, cursor);
        if appended {
            self.retired_as_append(&p);
        } else {
            self.moved_tail(p.born);
        }
        if self.rule(Rule::TypedOwes) && (!self.rule(Rule::TypedOwesCleanTail) || appended) {
            self.owe_glyph(p.seq, p.ch, p.born, now);
        } else {
            self.owe(p.born);
            for later in &mut self.preds {
                later.owed = later.owed.max(Some(p.born));
            }
        }
    }

    fn retired_as_append(&mut self, p: &Prediction) {
        if self.tail_line_end {
            if p.epoch == self.epoch {
                self.line_end_at = Some(self.cell_after(p));
            }
            return;
        }
        let over_text = self
            .tail_at(p.row, p.col)
            .is_none_or(|held| held.legacy().is_some());
        if self.rule(Rule::CleanTailInFlight) && (over_text || self.run_hidden) {
            self.moved_tail(p.born);
            self.hidden_tail_at = Some(self.cell_after(p));
            if self.line_end_at == self.hidden_tail_at {
                self.line_end_at = None;
            }
        }
    }

    fn flush(&mut self) {
        if !self.preds.is_empty() {
            self.count(Event::Flush);
        }
        self.owed_at = self.owed_with_pending();
        self.preds.clear();
        self.last_credit = None;
        self.chain = false;
        self.glyph_owes.clear();
        self.void_proof();
        self.seen.clear();
        self.ghost_cursor = None;
        self.anchor_lost = false;
        self.grandfathered_epoch = None;
        self.carried_proven = 0;
        self.carried_proof = None;
        self.declined_tail = 0;
        self.cancelled_at = None;
        self.line_end_at = None;
        self.cancelled_pair = None;
    }

    #[inline]
    fn record_rtt(&mut self, rtt: Duration) {
        let ms = rtt.as_secs_f32() * 1000.0;
        self.srtt = Some(match self.srtt {
            None => ms,
            Some(s) => s * (1.0 - SRTT_ALPHA) + ms * SRTT_ALPHA,
        });
        self.expiry_backoff = 0;
    }

    fn record_echo_regime(&mut self, rtt: Duration) {
        let ms = rtt.as_secs_f32() * 1000.0;
        if ms >= DISPLAY_OPEN_MS {
            self.fast_samples = 0;
            self.slow_samples = self
                .slow_samples
                .saturating_add(1)
                .min(SLOW_SAMPLES_TO_DISPLAY);
            if self.slow_samples >= SLOW_SAMPLES_TO_DISPLAY {
                self.adaptive_slow = true;
            }
        } else if ms < DISPLAY_CLOSE_MS {
            self.slow_samples = 0;
            self.fast_samples = self
                .fast_samples
                .saturating_add(1)
                .min(FAST_SAMPLES_TO_HIDE);
            let smoothed_fast = self.srtt.is_none_or(|s| s < DISPLAY_CLOSE_MS);
            if self.fast_samples >= FAST_SAMPLES_TO_HIDE && smoothed_fast && self.preds.is_empty() {
                self.adaptive_slow = false;
            }
        }
    }
}

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace PredictorQueueCap

        def maxPredictions : Nat := 256

        theorem maxPredictions_eq : maxPredictions = 256 := rfl

        theorem pop_front_step_len (rest : Nat) :
            Nat.succ rest - 1 = rest := rfl

        end PredictorQueueCap
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    const G: u64 = 0;

    fn cell(map: &[((u16, u16), char)]) -> impl Fn(u16, u16) -> Option<char> + '_ {
        move |r, c| {
            map.iter()
                .find(|((rr, cc), _)| *rr == r && *cc == c)
                .map(|(_, ch)| *ch)
        }
    }

    fn t0() -> Instant {
        Instant::now()
    }

    fn at_the_end(
        p: &mut Predictor,
        ch: char,
        cursor: (u16, u16),
        grid: (u16, u16),
        now: Instant,
    ) -> bool {
        let row = vec![Seen::Blank; usize::from(grid.0)];
        let key = KeySample {
            cursor,
            coords: G,
            grid,
            row: Some(&row),
            ..KeySample::default()
        };
        p.predict_glyph(ch, &key, now)
    }

    #[test]
    fn pending_bounds_reserves_wrapped_guesses_without_consuming_them() {
        let mut predictor = Predictor::new(PredictMode::Always);
        let now = t0();
        assert_eq!(predictor.pending_bounds(), None);
        for ch in ['a', 'b', 'c'] {
            assert!(predictor.predict_char_in_grid(ch, (2, 9), G, (10, 10), now));
        }
        let deadline = predictor.next_deadline();
        assert_eq!(predictor.pending_bounds(), Some((2, 0, 4, 10)));
        assert_eq!(predictor.pending_bounds(), Some((2, 0, 4, 10)));
        assert_eq!(predictor.preds.len(), 3);
        assert_eq!(predictor.next_deadline(), deadline);
        predictor.reset();
        assert_eq!(predictor.pending_bounds(), None);
    }

    #[test]
    fn expire_due_consumes_the_deadline_without_a_redraw() {
        let mut p = Predictor::new(PredictMode::Always);
        let born = t0();
        assert!(!p.expire_due(born), "nothing pending, nothing due");
        assert!(p.predict_char('a', (0, 0), G, 80, born));
        let deadline = p.next_deadline().expect("a pending guess arms a deadline");

        let early = born + Duration::from_millis(1);
        assert!(!p.expire_due(early), "a guess inside its window is not due");
        assert_eq!(p.next_deadline(), Some(deadline), "and its deadline stands");

        let late = deadline + Duration::from_millis(1);
        assert!(
            p.next_deadline().is_some_and(|d| d <= late),
            "fixture: the peek's view — a past deadline"
        );
        assert!(p.expire_due(late), "the expired guess is dropped");
        assert!(
            p.next_deadline().is_none_or(|d| d > late),
            "no past deadline survives the sweep: {:?}",
            p.next_deadline()
        );
        assert!(!p.expire_due(late), "a second sweep finds nothing");
    }

    fn echo_turn(p: &mut Predictor, ch: char, col: u16, at: Instant, rtt: Duration) -> Instant {
        assert!(p.predict_char(ch, (0, col), G, 80, at));
        let done = at + rtt;
        let mut screen = vec![((0, col), ch)];
        screen.extend(p.proof_echo.map(|e| ((e.row, e.col), e.ch)));
        p.reconcile(Some((0, col + 1)), G, false, done, cell(&screen));
        done
    }

    #[test]
    fn off_mode_predicts_nothing() {
        let mut p = Predictor::new(PredictMode::Off);
        assert!(!p.predict_char('a', (0, 0), G, 80, t0()));
        assert!(p.idle());
    }

    #[test]
    fn sustained_slow_echo_retires_predictions_and_displays_next() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), G, 80, now));
        assert!(!p.idle());
        let first = now + Duration::from_millis(60);
        p.reconcile(Some((0, 1)), G, false, first, cell(&[((0, 0), 'a')]));
        assert!(p.idle(), "confirmed prediction is retired");
        assert!(p.predict_char('b', (0, 1), G, 80, first));
        assert!(p.overlay(first).is_empty(), "one slow sample stays hidden");

        let second = first + Duration::from_millis(60);
        p.reconcile(Some((0, 2)), G, false, second, cell(&[((0, 1), 'b')]));
        assert!(p.predict_char('c', (0, 2), G, 80, second));
        let shown = p.overlay(second);
        assert_eq!(shown.len(), 1);
        assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (0, 2, 'c'));
    }

    #[test]
    fn types_ahead_extends_from_last_guess_before_echo() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        p.predict_char('b', (0, 0), G, 80, now);
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 2);
        assert_eq!((shown[0].col, shown[0].ch), (0, 'a'));
        assert_eq!((shown[1].col, shown[1].ch), (1, 'b'));
    }

    #[test]
    fn adaptive_isolated_scheduler_tail_does_not_open_gate() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();

        assert!(p.predict_char('a', (0, 0), G, 80, now));
        let fast = now + Duration::from_millis(1);
        p.reconcile(Some((0, 1)), G, false, fast, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('b', (0, 1), G, 80, fast));
        assert!(p.predict_char('c', (0, 1), G, 80, fast));
        let tail = fast + Duration::from_millis(60);
        p.reconcile(
            Some((0, 3)),
            G,
            false,
            tail,
            cell(&[((0, 1), 'b'), ((0, 2), 'c')]),
        );

        assert!(p.predict_char('d', (0, 3), G, 80, tail));
        assert!(
            p.overlay(tail).is_empty(),
            "one delayed scheduler turn must not manufacture speculative pixels, even when it retires a burst"
        );

        let local_again = tail + Duration::from_millis(1);
        p.reconcile(Some((0, 4)), G, false, local_again, cell(&[((0, 3), 'd')]));
        assert!(p.predict_char('e', (0, 4), G, 80, local_again));
        assert!(p.overlay(local_again).is_empty());
    }

    #[test]
    fn adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let fast = Duration::from_millis(1);

        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "sustained remote latency opens");
        p.reconcile(Some((0, 3)), G, false, at + fast, cell(&[((0, 2), 'c')]));
        at += fast;

        at = echo_turn(&mut p, 'd', 3, at, fast);
        at = echo_turn(&mut p, 'e', 4, at, fast);
        assert!(p.srtt.is_some_and(|s| s >= DISPLAY_CLOSE_MS));
        assert!(
            p.adaptive_slow,
            "a raw streak alone must not close the gate"
        );

        for col in 5..14 {
            at = echo_turn(&mut p, 'x', col, at, fast);
        }
        assert!(!p.adaptive_slow, "a settled local link stops painting");
        assert!(p.predict_char('z', (0, 14), G, 80, at));
        assert!(p.overlay(at).is_empty());
    }

    #[test]
    fn adaptive_gate_does_not_flap_across_a_single_fast_sample() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(30);

        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow, "control: the slow link opened the gate");

        at = echo_turn(&mut p, 'c', 2, at, Duration::from_millis(5));
        assert!(p.adaptive_slow, "one fast dip must not blank the overlay");
        at = echo_turn(&mut p, 'd', 3, at, slow);
        assert!(p.predict_char('e', (0, 4), G, 80, at));
        assert_eq!(
            p.overlay(at).len(),
            1,
            "the slow link keeps painting across its own jitter"
        );
    }

    #[test]
    fn adaptive_gate_never_closes_while_a_guess_is_in_flight() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let fast = Duration::from_millis(1);

        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow);

        let mut col = 2u16;
        assert!(p.predict_char('x', (0, col), G, 80, at));
        for _ in 0..14 {
            assert!(
                p.predict_char('x', (0, col + 1), G, 80, at),
                "one glyph ahead"
            );
            at += fast;
            p.reconcile(Some((0, col + 1)), G, false, at, cell(&[((0, col), 'x')]));
            col += 1;
        }
        assert_eq!(p.preds.len(), 1, "fixture: one guess still in flight");
        assert!(
            p.srtt.is_some_and(|s| s < DISPLAY_CLOSE_MS),
            "control: the smoothed estimate is now unambiguously local"
        );
        assert!(
            p.adaptive_slow,
            "a gate flip with pixels in flight is exactly the visible blink"
        );

        at += fast;
        p.reconcile(Some((0, col + 1)), G, false, at, cell(&[((0, col), 'x')]));
        assert!(p.idle());
        assert!(!p.adaptive_slow);
    }

    #[test]
    fn burst_is_classified_by_the_oldest_retired_guess() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let burst = |p: &mut Predictor, start: Instant, base: u16| -> Instant {
            for i in 0..5u16 {
                assert!(p.predict_char(
                    'a',
                    (0, base + i),
                    G,
                    80,
                    start + Duration::from_millis(u64::from(i) * 10)
                ));
            }
            let echoed: Vec<((u16, u16), char)> = (0..5u16).map(|i| ((0, base + i), 'a')).collect();
            let done = start + Duration::from_millis(45);
            p.reconcile(Some((0, base + 5)), G, false, done, cell(&echoed));
            done
        };

        let after = burst(&mut p, now, 0);
        let after = burst(&mut p, after, 5);
        assert!(
            p.adaptive_slow,
            "two 45 ms turns are a slow link no matter how fast the user types"
        );
        assert!(p.predict_char('z', (0, 10), G, 80, after));
        assert_eq!(p.overlay(after).len(), 1);
    }

    #[test]
    fn adaptive_local_tail_expires_without_a_visible_erase() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        let fast = now + Duration::from_millis(1);
        p.reconcile(Some((0, 1)), G, false, fast, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('b', (0, 1), G, 80, fast));
        assert!(!p.is_displaying(fast));
        assert!(
            p.overlay(fast).is_empty(),
            "no speculative pixel was painted"
        );

        let after_deadline = fast + Duration::from_millis(GLITCH_FLOOR_MS + 1);
        assert!(p.overlay(after_deadline).is_empty());
        assert!(p.idle(), "the hidden stale guess still self-heals");
        assert!(
            p.next_deadline().is_none(),
            "invisible expiry disarms its timer"
        );
    }

    #[test]
    fn divergence_flushes_all() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        p.reconcile(Some((0, 1)), G, false, now, cell(&[((0, 0), '*')]));
        assert!(p.idle(), "a wrong guess flushes the whole prediction set");
    }

    #[test]
    fn alt_screen_flushes_predictions() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        assert!(!p.idle());
        p.reconcile(Some((10, 4)), G, true, now, cell(&[]));
        assert!(p.idle());
    }

    #[test]
    fn expired_prediction_clears_next_deadline() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        assert!(
            p.next_deadline().is_some(),
            "a pending guess arms a deadline"
        );
        let late = now + Duration::from_millis(GLITCH_FLOOR_MS + 50);
        let _ = p.overlay(late);
        assert!(p.idle(), "stale guess flushed past the glitch window");
        assert!(
            p.next_deadline().is_none(),
            "after the expiry flush the deadline self-disarms (no permanently-past wake)"
        );
    }

    #[test]
    fn backspace_cancels_pending_prediction() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        p.predict_char('b', (0, 0), G, 80, now);
        assert_eq!(p.overlay(now).len(), 2);
        assert!(p.predict_backspace(now));
        assert_eq!(p.overlay(now).len(), 1, "backspace removes the last guess");
        assert_eq!(p.overlay(now)[0].ch, 'a');
    }

    #[test]
    fn height_blind_hosts_decline_at_the_margin_without_flushing() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 78), G, 80, now));
        assert!(p.predict_char('b', (0, 78), G, 80, now));
        assert!(
            !p.predict_char('c', (0, 78), G, 80, now),
            "col 80 would wrap and no row height was supplied → declined"
        );
        assert_eq!(
            p.overlay(now).len(),
            2,
            "declining the wrap must not erase the two glyphs already on glass"
        );
    }

    #[test]
    fn wraps_to_the_next_row_instead_of_flushing() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), G, (80, 24), now));
        assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now));
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 2);
        assert_eq!((shown[0].row, shown[0].col), (0, 79));
        assert_eq!(
            (shown[1].row, shown[1].col, shown[1].ch),
            (1, 0, 'b'),
            "the wrapped glyph continues on the next row"
        );
    }

    #[test]
    fn wrapped_guess_survives_the_deferred_wrap_cursor() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(at_the_end(&mut p, 'a', (0, 79), (80, 24), now));
        assert!(at_the_end(&mut p, 'b', (0, 79), (80, 24), now));
        p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 79), 'a')]));
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 1, "'a' retires, the wrapped 'b' survives");
        assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (1, 0, 'b'));
    }

    #[test]
    fn bottom_row_declines_without_flushing() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (23, 79), G, (80, 24), now));
        assert!(
            !p.predict_char_in_grid('b', (23, 79), G, (80, 24), now),
            "there is no row 24 to wrap into"
        );
        assert_eq!(p.overlay(now).len(), 1, "the armed glyph keeps its pixels");
    }

    #[test]
    fn wide_glyphs_are_declined_but_narrow_accents_are_predicted() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(
            p.predict_char('é', (0, 0), G, 80, now),
            "single-width accented Latin is ordinary self-echoing input"
        );
        assert!(p.predict_char('ü', (0, 0), G, 80, now));
        assert_eq!(p.overlay(now).len(), 2);
        assert!(
            !p.predict_char('日', (0, 0), G, 80, now),
            "wide/CJK glyphs are left to real echo"
        );
        assert!(
            !p.predict_char('\u{0301}', (0, 0), G, 80, now),
            "a combining mark modifies the cell to its LEFT — not a new cell"
        );
    }

    #[test]
    fn ambiguous_glyphs_are_measured_as_the_terminal_writes_them() {
        let mut p = Predictor::new(PredictMode::Always);
        p.set_ambiguous_width_double(true);
        let now = t0();
        assert!(!p.predict_char('é', (0, 0), G, 80, now));
        assert!(!p.predict_char('°', (0, 0), G, 80, now));
        assert!(
            p.predict_char('a', (0, 0), G, 80, now),
            "ASCII stays narrow"
        );
    }

    #[test]
    fn a_declined_glyph_does_not_erase_the_word_to_its_left() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), G, 80, now));
        assert!(!p.predict_char('日', (0, 0), G, 80, now));
        assert_eq!(p.overlay(now).len(), 1, "'a' keeps its pixels");
        assert!(
            !p.predict_char('b', (0, 0), G, 80, now),
            "we cannot place a glyph past a wide echo we did not model"
        );
        assert_eq!(
            p.overlay(now).len(),
            1,
            "…and declining that changes nothing"
        );
        p.reconcile(Some((0, 3)), G, false, now, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('c', (0, 3), G, 80, now));
        assert_eq!(p.overlay(now)[0].col, 3);
    }

    #[test]
    fn mode_parse_is_fail_safe() {
        assert_eq!(PredictMode::parse("adaptive"), PredictMode::Adaptive);
        assert_eq!(PredictMode::parse("ALWAYS"), PredictMode::Always);
        assert_eq!(PredictMode::parse("off"), PredictMode::Off);
        assert_eq!(PredictMode::parse("nonsense"), PredictMode::Off);
    }

    #[test]
    fn fresh_line_resets_confirmation_no_leak() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        let t1 = now + Duration::from_millis(50);
        p.reconcile(Some((0, 1)), G, false, t1, cell(&[((0, 0), 'a')]));
        p.predict_char('b', (0, 1), G, 80, t1);
        let t2 = t1 + Duration::from_millis(50);
        p.reconcile(Some((0, 2)), G, false, t2, cell(&[((0, 1), 'b')]));
        assert!(
            p.adaptive_slow,
            "control: sustained slow echo opened the latch"
        );
        p.predict_char('s', (1, 9), G, 80, t2);
        assert!(
            p.overlay(t2).is_empty(),
            "an unechoed char on a new line must NOT display (no password leak)"
        );
    }

    #[test]
    fn submit_does_not_blank_the_tail_of_the_command_line() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('l', (0, 0), G, 80, now));
        assert!(p.predict_char('s', (0, 0), G, 80, now));
        assert_eq!(p.overlay(now).len(), 2);
        p.note_line_submit(now);
        assert_eq!(
            p.overlay(now).len(),
            2,
            "the unechoed tail of the submitted line stays on glass"
        );
        assert!(
            p.next_deadline().is_some(),
            "…and still self-heals on its own deadline"
        );
    }

    #[test]
    fn a_stale_guess_confirming_after_submit_cannot_arm_the_new_line() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let r = 23u16;
        let slow = Duration::from_millis(60);
        assert!(p.predict_char('a', (r, 0), G, 80, now));
        let t1 = now + slow;
        p.reconcile(Some((r, 1)), G, false, t1, cell(&[((r, 0), 'a')]));
        assert!(p.predict_char('b', (r, 1), G, 80, t1));
        let t2 = t1 + slow;
        p.reconcile(Some((r, 2)), G, false, t2, cell(&[((r, 1), 'b')]));
        assert!(p.adaptive_slow, "control: the link is classified slow");

        assert!(p.predict_char('c', (r, 2), G, 80, t2));
        p.note_line_submit(t2);
        let t3 = t2 + slow;
        p.reconcile(Some((r, 3)), G, false, t3, cell(&[((r, 2), 'c')]));
        assert!(p.idle(), "the stale guess retired normally");
        assert!(
            !p.confirmed_epoch,
            "a pre-submit confirmation must not arm the new line"
        );

        assert!(p.predict_char('s', (r, 9), G, 80, t3));
        assert!(
            p.overlay(t3).is_empty(),
            "an unechoed secret on the new line must never display"
        );
    }

    #[test]
    fn slow_link_expiry_window_grows_until_the_echo_can_confirm() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let rtt = Duration::from_millis(400);

        assert!(p.predict_char('a', (0, 0), G, 80, now));
        let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
        assert!(p.idle(), "the unconfirmed guess still self-heals");
        assert!(
            p.glitch_window() > Duration::from_millis(GLITCH_FLOOR_MS),
            "a timeout widens the window instead of repeating forever"
        );

        let armed = now + Duration::from_millis(500);
        assert!(p.predict_char('b', (0, 0), G, 80, armed));
        let _ = p.overlay(armed + Duration::from_millis(300));
        assert!(!p.idle(), "the guess outlives the old fixed window");
        p.reconcile(Some((0, 1)), G, false, armed + rtt, cell(&[((0, 0), 'b')]));
        assert!(p.confirmed_epoch, "the echo finally reaches a live guess");
        assert!(
            !p.adaptive_slow,
            "ONE confirmation is not a regime: silence must never substitute for the \
             second slow sample (that is what painted a ghost at a password prompt)"
        );
        assert_eq!(
            p.expiry_backoff, 0,
            "a real echo retires the cold-link probe"
        );

        let mut at = armed + rtt;
        assert!(p.predict_char('c', (0, 1), G, 80, at));
        at += rtt;
        p.reconcile(Some((0, 2)), G, false, at, cell(&[((0, 1), 'c')]));
        assert!(
            p.adaptive_slow,
            "two real slow echoes classify the link — the honest evidence path"
        );

        assert!(p.predict_char('d', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "a 400 ms link finally predicts");
    }

    #[test]
    fn silence_never_opens_the_gate_or_diverges_the_estimate() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut now = t0();
        for _ in 0..12 {
            assert!(p.predict_char('x', (0, 0), G, 80, now));
            now += Duration::from_millis(GLITCH_CEILING_MS + 10);
            assert!(
                p.overlay(now).is_empty(),
                "an unechoed guess is never displayed on a link that never echoed"
            );
        }
        assert!(!p.adaptive_slow, "silence is not evidence of a slow link");
        assert!(!p.confirmed_epoch, "nothing was ever confirmed");
        assert_eq!(p.srtt, None, "timeouts must not seed the RTT estimate");
        assert!(
            p.glitch_window() <= Duration::from_millis(GLITCH_CEILING_MS),
            "the probe is bounded, never divergent"
        );
    }

    #[test]
    fn expiry_retires_only_the_oldest_guess_owes_it_and_voids_the_epoch() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), G, 80, now));
        let later = now + Duration::from_millis(200);
        assert!(p.predict_char('b', (0, 0), G, 80, later));
        let expired = now + Duration::from_millis(GLITCH_FLOOR_MS + 10);
        let shown = p.overlay(expired);
        assert_eq!(shown.len(), 1, "only the expired head is retired");
        assert_eq!(shown[0].ch, 'b');
        assert_eq!(p.owed_at, Some(now), "the expired key's echo is owed");
        assert_eq!(
            p.preds.front().map(|g| g.owed),
            Some(Some(now)),
            "…to the guess typed after it"
        );

        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert_eq!(
            p.overlay(at).len(),
            1,
            "control: the proven line paints 'c'"
        );
        let expired = at + Duration::from_millis(GLITCH_FLOOR_MS + 10);
        assert!(p.overlay(expired).is_empty(), "the stale guess expired");
        assert!(
            !p.confirmed_epoch && p.proof_cell.is_none(),
            "an expiry voids the epoch's proof"
        );
        assert!(p.predict_char('d', (0, 2), G, 80, expired));
        assert!(
            p.overlay(expired).is_empty(),
            "'d' echoes after 'c''s late echo, at (0, 3): not painted at (0, 2)"
        );
        let late = expired + Duration::from_millis(60);
        p.reconcile(
            Some((0, 3)),
            G,
            false,
            late,
            cell(&after_ab(&[((0, 2), 'c')])),
        );
        assert!(p.idle(), "'c' at 'd''s cell: a different glyph, flushed");
        assert!(!p.confirmed_epoch);
    }

    #[test]
    fn without_expiry_owes_an_expiry_keeps_the_proof_as_it_did() {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        p.disable_rule_for_test(Rule::ExpiryOwes);
        let owed = p.owed_at;
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        let expired = at + Duration::from_millis(GLITCH_FLOOR_MS + 10);
        assert!(p.overlay(expired).is_empty());
        assert_eq!(p.owed_at, owed, "nothing owed");
        assert!(p.confirmed_epoch, "the proof kept");
        assert!(p.predict_char('d', (0, 2), G, 80, expired));
        assert_eq!(p.overlay(expired).len(), 1, "'d' painted on 'c''s cell");
    }

    #[test]
    fn session_switch_forgets_slow_link_rtt() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), G, 80, now));
        let slow1 = now + Duration::from_millis(60);
        p.reconcile(Some((0, 1)), G, false, slow1, cell(&[((0, 0), 'a')]));
        assert!(p.predict_char('b', (0, 1), G, 80, slow1));
        let slow2 = slow1 + Duration::from_millis(60);
        p.reconcile(Some((0, 2)), G, false, slow2, cell(&[((0, 1), 'b')]));
        assert!(p.predict_char('c', (0, 2), G, 80, slow2));
        assert_eq!(p.overlay(slow2).len(), 1, "control: slow link displays");

        p.reset_session();
        let local = slow2 + Duration::from_millis(1);
        assert!(p.predict_char('x', (0, 0), G, 80, slow2));
        p.reconcile(Some((0, 1)), G, false, local, cell(&[((0, 0), 'x')]));
        assert!(p.predict_char('y', (0, 1), G, 80, local));
        assert!(
            p.overlay(local).is_empty(),
            "a new session learns its own fast RTT instead of inheriting the old slow link"
        );
    }

    #[test]
    fn the_deferred_wrap_slot_is_not_a_free_cell() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(at_the_end(&mut p, 'a', (0, 79), (80, 24), now));
        p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 79), 'a')]));
        assert!(p.idle(), "control: the echo retired the guess");

        assert!(at_the_end(&mut p, 'b', (0, 79), (80, 24), now));
        let shown = p.overlay(now);
        assert_eq!(
            (shown[0].row, shown[0].col, shown[0].ch),
            (1, 0, 'b'),
            "a parked cursor wraps the next glyph to the following row"
        );
    }

    #[test]
    fn a_wide_glyph_filling_the_last_two_columns_parks_the_cursor() {
        for (glyph, wide_policy, parked) in [
            ('日', false, true),
            ('日', true, true),
            ('é', true, true),
            ('é', false, false),
        ] {
            let mut p = Predictor::new(PredictMode::Always);
            p.set_ambiguous_width_double(wide_policy);
            let now = t0();
            let accepted = p.predict_char_in_grid(glyph, (0, 78), G, (80, 24), now);
            assert_eq!(
                accepted, !parked,
                "{glyph} wide={wide_policy}: placed iff narrow"
            );
            p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 78), glyph)]));
            assert!(
                p.idle(),
                "{glyph} wide={wide_policy}: the echo drained the set"
            );

            assert!(p.predict_char_in_grid('i', (0, 79), G, (80, 24), now));
            let shown = p.overlay(now);
            let want = if parked { (1, 0) } else { (0, 79) };
            assert_eq!(
                (shown[0].row, shown[0].col),
                want,
                "{glyph} wide={wide_policy}: parked={parked}"
            );
        }
    }

    #[test]
    fn changing_the_ambiguous_width_flushes_guesses_placed_under_the_old_one() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), G, 80, now));
        p.set_ambiguous_width_double(false);
        assert_eq!(p.overlay(now).len(), 1, "the same width keeps the guess");
        p.set_ambiguous_width_double(true);
        assert!(p.idle(), "a new width flushes what the old one placed");
        assert!(
            !p.predict_char('é', (0, 1), G, 80, now),
            "and measures with the new one"
        );
    }

    #[test]
    fn a_free_last_column_is_still_predicted_in_place() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 78), G, (80, 24), now));
        p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 78), 'a')]));
        assert!(p.idle());
        assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now));
        let shown = p.overlay(now);
        assert_eq!((shown[0].row, shown[0].col), (0, 79));
    }

    #[test]
    fn a_deferred_wrap_on_the_last_row_declines_without_painting() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(at_the_end(&mut p, 'a', (23, 79), (80, 24), now));
        p.reconcile(Some((23, 79)), G, false, now, cell(&[((23, 79), 'a')]));
        assert!(
            !at_the_end(&mut p, 'b', (23, 79), (80, 24), now),
            "no row 24 to wrap into, and the slot holds the echoed 'a'"
        );
        assert!(p.idle(), "declining paints nothing and erases nothing");
    }

    #[test]
    fn a_wrap_the_terminal_did_not_defer_keeps_the_wrapped_guess() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), G, (80, 24), now));
        assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now));
        p.reconcile(Some((1, 0)), G, false, now, cell(&[((0, 79), 'a')]));
        let shown = p.overlay(now);
        assert_eq!(
            shown.len(),
            1,
            "'a' retires; the wrapped 'b' is no divergence"
        );
        assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (1, 0, 'b'));
    }

    #[test]
    fn a_scrolled_cursor_does_not_pass_for_the_deferred_wrap_slot() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(at_the_end(&mut p, 'a', (5, 79), (80, 24), now));
        assert!(at_the_end(&mut p, 'b', (5, 79), (80, 24), now));
        p.reconcile(Some((5, 79)), G, false, now, cell(&[((5, 79), 'a')]));
        assert_eq!(p.overlay(now).len(), 1, "control: the park keeps 'b' alive");

        p.reconcile(Some((4, 79)), G, false, now, cell(&[((4, 79), 'a')]));
        assert!(p.idle(), "a stale absolute row is retired, not tolerated");
    }

    #[test]
    fn a_scroll_retires_guesses_instead_of_confirming_them_against_moved_cells() {
        let now = t0();
        let mut unaware = Predictor::new(PredictMode::Adaptive);
        assert!(unaware.predict_char_in_grid('a', (5, 0), G, (80, 24), now));
        unaware.reconcile(Some((5, 1)), G, false, now, cell(&[((5, 0), 'a')]));
        assert!(
            unaware.confirmed_epoch,
            "control: a glyph-equal cell really does confirm"
        );

        let mut p = Predictor::new(PredictMode::Adaptive);
        assert!(p.predict_char_in_grid('a', (5, 0), G, (80, 24), now));
        p.reconcile(Some((5, 1)), G + 1, false, now, cell(&[((5, 0), 'a')]));
        assert!(p.idle(), "a scrolled guess is retired, not re-aimed");
        assert!(
            !p.confirmed_epoch,
            "…so a moved cell can never arm the display gate"
        );
    }

    #[test]
    fn a_scroll_ends_the_epoch_on_the_same_physical_row() {
        let slow = Duration::from_millis(60);
        let r = 23u16;
        let leak = |coords_after: u64| {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let mut at = t0();
            for (col, ch) in "yes".chars().enumerate() {
                let col = col as u16;
                assert!(p.predict_char_in_grid(ch, (r, col), G, (80, 24), at));
                at += slow;
                p.reconcile(Some((r, col + 1)), G, false, at, cell(&[((r, col), ch)]));
            }
            assert!(
                p.adaptive_slow && p.confirmed_epoch,
                "control: a proven slow line"
            );
            let prompt = [((r, 0), 'P'), ((r, 1), 'a'), ((r, 2), 's')];
            p.reconcile(Some((r, 3)), coords_after, false, at, cell(&prompt));
            assert!(p.predict_char_in_grid('s', (r, 3), coords_after, (80, 24), at));
            !p.overlay(at).is_empty()
        };
        assert!(
            leak(G),
            "control: without a scroll the row's proof still applies"
        );
        assert!(
            !leak(G + 1),
            "an unechoed glyph on a prompt that scrolled onto the proven row was painted"
        );
    }

    #[test]
    fn a_line_terminator_makes_what_follows_tentative() {
        let slow = Duration::from_millis(60);
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "control: the proven line paints");

        p.note_line_submit(at);
        p.note_line_submit(at);
        assert!(!p.predict_char('h', (0, 2), G, 80, at));
        let shown: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
        assert_eq!(
            shown,
            ['c'],
            "the proven line's 'c' keeps its pixels; nothing typed after the \
             boundary paints"
        );

        at += slow;
        p.reconcile(Some((0, 3)), G, false, at, cell(&[((0, 2), 'c')]));
        assert!(p.idle());
        at += p.glitch_window() + Duration::from_millis(1);
        at = echo_turn(&mut p, 'x', 3, at, slow);
        assert!(p.predict_char('y', (0, 4), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "a re-proven line paints again");
    }

    #[test]
    fn nothing_is_armed_behind_a_terminator_while_its_line_is_in_flight() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('l', (0, 2), G, 80, now));
        assert!(p.predict_char('s', (0, 2), G, 80, now));
        p.note_line_submit(now);
        assert!(!p.predict_char('c', (0, 2), G, 80, now));
        assert!(!p.predict_char('d', (0, 2), G, 80, now));
        let shown: Vec<(u16, u16, char)> = p
            .overlay(now)
            .iter()
            .map(|g| (g.row, g.col, g.ch))
            .collect();
        assert_eq!(
            shown,
            [(0, 2, 'l'), (0, 3, 's')],
            "the submitted line's tail stays on glass, and nothing is painted over it"
        );
        let echoed = now + Duration::from_millis(60);
        p.reconcile(
            Some((1, 2)),
            G,
            false,
            echoed,
            cell(&[((0, 2), 'l'), ((0, 3), 's')]),
        );
        assert!(p.idle(), "the line retired");
        assert!(p.predict_char('x', (1, 2), G, 80, echoed));
    }

    #[test]
    fn the_submitted_lines_echo_cannot_prove_the_next_line() {
        let slow = Duration::from_millis(60);
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('l', (0, 2), G, 80, at));
        assert!(p.predict_char('s', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 2, "control: the proven line paints");
        p.note_line_submit(at);
        assert!(!p.predict_char('l', (0, 2), G, 80, at));
        assert!(!p.predict_char('s', (0, 2), G, 80, at));
        at += slow;
        p.reconcile(
            Some((0, 4)),
            G,
            false,
            at,
            cell(&[((0, 0), 'a'), ((0, 1), 'b'), ((0, 2), 'l'), ((0, 3), 's')]),
        );
        assert!(p.idle());
        assert!(
            !p.confirmed_epoch,
            "the old line's echo proved nothing about the new one"
        );
        assert!(p.predict_char('x', (0, 4), G, 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "'x' echoes on the next prompt row, not after \"ls\" on this one"
        );
    }

    #[test]
    fn an_echo_owed_to_a_flushed_guess_cannot_prove_a_later_one() {
        let slow = Duration::from_millis(60);
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(
            p.adaptive_slow && p.confirmed_epoch,
            "control: a slow, proven line"
        );
        let screen = [((0, 0), 'a'), ((0, 1), 'b')];
        assert!(p.predict_char('n', (0, 2), G, 80, at));
        at += Duration::from_millis(10);
        p.reconcile(Some((1, 2)), G, false, at, cell(&screen));
        assert!(
            p.idle(),
            "the cursor left the guess: flushed, its echo still owed"
        );
        assert!(p.predict_char('n', (1, 2), G, 80, at));
        at += slow;
        let first_echo = [((0, 0), 'a'), ((0, 1), 'b'), ((1, 2), 'n')];
        p.reconcile(Some((1, 3)), G, false, at, cell(&first_echo));
        assert!(p.idle(), "glyph and cursor match, so the pixels retire");
        assert!(
            !p.confirmed_epoch,
            "…but the echo may be the first 'n's: it proves nothing"
        );
        assert!(p.predict_char(' ', (1, 3), G, 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "the space echoes at (1, 4), after the second 'n': not painted at (1, 3)"
        );
        at += slow;
        let second_echo = [((0, 0), 'a'), ((0, 1), 'b'), ((1, 2), 'n'), ((1, 3), 'n')];
        p.reconcile(Some((1, 4)), G, false, at, cell(&second_echo));
        assert!(p.idle());
        at += p.glitch_window() + Duration::from_millis(1);
        assert!(p.predict_char('q', (1, 5), G, 80, at));
        at += slow;
        let q_echo = [((1, 2), 'n'), ((1, 3), 'n'), ((1, 5), 'q')];
        p.reconcile(Some((1, 6)), G, false, at, cell(&q_echo));
        assert!(p.confirmed_epoch, "an attributable echo proves the epoch");
        assert!(p.predict_char('r', (1, 6), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1);
    }

    fn proven_slow_line(mode: PredictMode) -> (Predictor, Instant) {
        let slow = Duration::from_millis(60);
        let mut p = Predictor::new(mode);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(
            p.adaptive_slow && p.confirmed_epoch && p.proof_cell == Some((0, 2)),
            "fixture: a slow, proven line"
        );
        assert_eq!(
            p.glitch_window(),
            Duration::from_millis(GLITCH_FLOOR_MS),
            "fixture: the floor window"
        );
        (p, at)
    }

    const AB: [((u16, u16), char); 2] = [((0, 0), 'a'), ((0, 1), 'b')];

    fn after_ab(more: &[((u16, u16), char)]) -> Vec<((u16, u16), char)> {
        AB.iter().chain(more).copied().collect()
    }

    #[test]
    fn every_untracked_press_owes_its_echo() {
        let submit: fn(&mut Predictor, Instant) = |p, at| p.note_line_submit(at);
        let unmodelled: fn(&mut Predictor, Instant) = |p, at| p.note_unmodelled_input(at);
        for (label, press) in [("terminator", submit), ("unmodelled input", unmodelled)] {
            let (mut p, mut at) = proven_slow_line(PredictMode::Adaptive);
            press(&mut p, at);
            assert!(p.idle());
            at += Duration::from_millis(10);
            assert!(p.predict_char('n', (0, 2), G, 80, at), "{label}");
            at += Duration::from_millis(50);
            let landed = after_ab(&[((0, 2), 'n')]);
            p.reconcile(Some((0, 3)), G, false, at, cell(&landed));
            assert!(
                p.idle(),
                "{label}: glyph and cursor match, so the pixels retire"
            );
            assert!(
                !p.confirmed_epoch,
                "{label}: the 'n' may be the press's output: it proves nothing"
            );
            assert!(p.predict_char('x', (0, 3), G, 80, at));
            assert!(
                p.overlay(at).is_empty(),
                "{label}: 'x' echoes after the typed 'n', at (0, 4): not painted at (0, 3)"
            );
        }
    }

    #[test]
    fn a_declined_glyph_owes_its_echo() {
        let (mut p, mut at) = proven_slow_line(PredictMode::Adaptive);
        assert!(p.predict_char('a', (0, 2), G, 80, at));
        assert!(!p.predict_char('日', (0, 2), G, 80, at), "wide: declined");
        assert!(
            !p.predict_char('n', (0, 2), G, 80, at),
            "behind a declined glyph: declined"
        );
        at += Duration::from_millis(60);
        let wide = after_ab(&[((0, 2), 'a'), ((0, 3), '日')]);
        p.reconcile(Some((0, 5)), G, false, at, cell(&wide));
        assert!(p.idle(), "'a' retired; the first 'n' is still on its way");
        at += Duration::from_millis(10);
        assert!(p.predict_char('n', (0, 5), G, 80, at));
        at += Duration::from_millis(10);
        let landed: Vec<_> = wide.iter().copied().chain([((0, 5), 'n')]).collect();
        p.reconcile(Some((0, 6)), G, false, at, cell(&landed));
        assert!(p.idle());
        assert!(!p.confirmed_epoch, "the first 'n's echo proves nothing");
        assert!(p.predict_char('c', (0, 6), G, 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "'c' echoes at (0, 7), after the second 'n'"
        );
    }

    #[test]
    fn an_erase_we_did_not_cancel_owes_its_echo() {
        let (mut p, mut at) = proven_slow_line(PredictMode::Adaptive);
        assert!(!p.predict_backspace(at), "nothing of ours: it erases 'b'");
        at += Duration::from_millis(10);
        assert!(p.predict_char('n', (0, 2), G, 80, at));
        assert!(p.predict_char('n', (0, 2), G, 80, at));
        at += Duration::from_millis(60);
        let landed = [((0, 0), 'a'), ((0, 1), 'n'), ((0, 2), 'n')];
        p.reconcile(Some((0, 3)), G, false, at, cell(&landed));
        assert_eq!(p.preds.len(), 1, "the first guess's pixels retire");
        assert!(!p.confirmed_epoch, "the match was the second key's echo");
        assert!(
            p.overlay(at).is_empty(),
            "no ghost at (0, 3) of an 'n' that echoed at (0, 2)"
        );
    }

    #[test]
    fn a_guess_retired_without_credit_owes_its_own_echo() {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        assert!(p.predict_char('n', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "control: the proven line paints K");
        let k = at;
        p.reconcile(
            Some((0, 6)),
            G,
            false,
            k + Duration::from_millis(10),
            cell(&AB),
        );
        assert!(p.idle());
        let typed = k + Duration::from_millis(190);
        for ch in ['n', 'n', 'c'] {
            assert!(p.predict_char(ch, (0, 2), G, 80, typed));
        }
        let first = after_ab(&[((0, 2), 'n')]);
        p.reconcile(
            Some((0, 3)),
            G,
            false,
            k + Duration::from_millis(230),
            cell(&first),
        );
        assert_eq!(p.preds.len(), 2);
        assert!(!p.confirmed_epoch);
        let at = k + Duration::from_millis(260);
        let second = after_ab(&[((0, 2), 'n'), ((0, 3), 'n')]);
        p.reconcile(Some((0, 4)), G, false, at, cell(&second));
        assert_eq!(p.preds.len(), 1, "the second guess's pixels retire");
        assert!(
            !p.confirmed_epoch,
            "…but the echo may be the first new guess's: it proves nothing"
        );
        assert!(
            p.overlay(at).is_empty(),
            "'c' echoes at (0, 5), after the second 'n': not painted at (0, 4)"
        );
        let third = after_ab(&[((0, 2), 'n'), ((0, 3), 'n'), ((0, 4), 'n')]);
        let at = at + Duration::from_millis(30);
        p.reconcile(Some((0, 5)), G, false, at, cell(&third));
        assert!(p.idle());
    }

    #[test]
    fn an_owed_echo_neither_proves_nor_measures_a_carried_guess() {
        let rtt = Duration::from_millis(100);
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), rtt);
        at = echo_turn(&mut p, 'b', 1, at, rtt);
        assert!(p.predict_char('n', (0, 2), G, 80, at));
        at += Duration::from_millis(10);
        p.reconcile(Some((1, 0)), G, false, at, cell(&AB));
        assert!(p.idle(), "the cursor jumped off 'n': flushed, owed");
        assert!(p.predict_char('n', (1, 0), G, 80, at));
        p.note_line_submit(at);
        let before = (p.srtt, p.slow_samples, p.fast_samples);
        at += Duration::from_millis(5);
        let landed = after_ab(&[((1, 0), 'n')]);
        p.reconcile(Some((1, 1)), G, false, at, cell(&landed));
        assert!(p.idle(), "the carried guess's pixels retire");
        assert_eq!(
            (p.srtt, p.slow_samples, p.fast_samples),
            before,
            "the first 'n's echo is no sample of the link"
        );

        at += Duration::from_millis(GLITCH_CEILING_MS);
        assert!(p.predict_char('x', (1, 1), G, 80, at));
        p.note_line_submit(at);
        let echoed = at + rtt;
        let tail = [((1, 0), 'n'), ((1, 1), 'x')];
        p.reconcile(Some((1, 2)), G, false, echoed, cell(&tail));
        assert!(p.idle());
        assert_eq!(p.slow_samples, before.1, "a slow sample, counted (capped)");
        assert!(
            p.srtt.is_some_and(|s| (s - 100.0).abs() < 0.01),
            "the tail's 100 ms echo measured: {:?}",
            p.srtt
        );
    }

    #[test]
    fn an_owe_is_dated_by_its_press() {
        let rtt = Duration::from_millis(100);
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), rtt);
        at = echo_turn(&mut p, 'b', 1, at, rtt);
        p.note_line_submit(at);
        at += Duration::from_secs(60);
        assert!(p.predict_char('x', (1, 2), G, 80, at));
        at += rtt;
        p.reconcile(Some((1, 3)), G, false, at, cell(&[((1, 2), 'x')]));
        assert!(
            p.confirmed_epoch,
            "x's own echo, a minute after the Enter's"
        );
        assert!(p.predict_char('y', (1, 3), G, 80, at));
        assert_eq!(
            p.overlay(at).len(),
            1,
            "the new line paints its second glyph"
        );
    }

    #[test]
    fn a_backspace_cancels_the_glyph_we_declined() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('l', (0, 2), G, 80, now));
        assert!(p.predict_char('s', (0, 2), G, 80, now));
        p.note_line_submit(now);
        assert!(!p.predict_char('c', (0, 2), G, 80, now));
        assert!(!p.predict_backspace(now), "no guess of ours was cancelled");
        let shown = |p: &mut Predictor| -> Vec<(u16, u16, char)> {
            p.overlay(now)
                .iter()
                .map(|g| (g.row, g.col, g.ch))
                .collect()
        };
        assert_eq!(
            shown(&mut p),
            [(0, 2, 'l'), (0, 3, 's')],
            "the submitted line's tail stays on glass"
        );
        assert!(!p.predict_backspace(now));
        assert!(
            shown(&mut p).is_empty(),
            "an erase with nothing declined to cancel may reach the carried tail"
        );

        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        assert!(p.predict_char('x', (0, 2), G, 80, at));
        assert!(!p.predict_char('日', (0, 2), G, 80, at));
        assert!(!p.predict_backspace(at));
        assert_eq!(
            p.overlay(at).len(),
            1,
            "'x' (carried by its proven epoch) survives the erase of the wide glyph"
        );
        assert!(!p.predict_char('日', (0, 2), G, 80, at));
        p.note_line_submit(at);
        assert!(!p.predict_backspace(at));
        assert!(p.idle(), "fail closed: the erase may reach the carried 'x'");
    }

    #[test]
    fn a_key_we_did_not_model_retires_the_guesses_in_flight() {
        let slow = Duration::from_millis(60);
        for mode in [PredictMode::Adaptive, PredictMode::Always] {
            let mut p = Predictor::new(mode);
            let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
            at = echo_turn(&mut p, 'b', 1, at, slow);
            assert!(p.predict_char('c', (0, 2), G, 80, at));
            assert_eq!(p.overlay(at).len(), 1, "{mode:?}: control: 'c' paints");

            p.note_unmodelled_input(at);
            assert!(
                p.overlay(at).is_empty(),
                "{mode:?}: 'c' stays on glass past a key that may have erased it"
            );
            if mode == PredictMode::Adaptive {
                assert!(p.predict_char('h', (0, 2), G, 80, at));
                assert!(
                    p.overlay(at).is_empty(),
                    "the glyph typed after the boundary stays dark"
                );
                at += slow;
                p.reconcile(Some((0, 3)), G, false, at, cell(&[((0, 2), 'c')]));
                assert!(p.idle());
                at += p.glitch_window() + Duration::from_millis(1);
                at = echo_turn(&mut p, 'x', 3, at, slow);
                assert!(p.predict_char('y', (0, 4), G, 80, at));
                assert_eq!(p.overlay(at).len(), 1, "a re-proven line paints again");
            }
        }
    }

    #[test]
    fn an_erase_after_a_boundary_darkens_the_carried_guesses() {
        let slow = Duration::from_millis(60);
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        p.note_line_submit(at);
        assert_eq!(p.overlay(at).len(), 1, "control: 'c' is carried");
        assert!(
            !p.predict_backspace(at),
            "'c' is not this epoch's to cancel"
        );
        assert!(p.overlay(at).is_empty(), "the erase may be landing on it");
    }

    #[test]
    fn an_erase_landing_on_typed_ahead_text_leaves_no_ghost() {
        type Erase = fn(&mut Predictor, Instant);
        type Cells = &'static [((u16, u16), char)];
        let killed: Cells = &[((0, 0), 'l'), ((0, 1), 's')];
        let one_erased: Cells = &[((0, 0), 'l'), ((0, 1), 's'), ((0, 3), 'f'), ((0, 4), 'o')];
        let erases: [(&str, Erase, Cells, u16); 3] = [
            (
                "ctrl+w kills the word",
                |p, at| p.note_unmodelled_input(at),
                killed,
                3,
            ),
            (
                "ctrl+h deletes its last glyph",
                |p, at| p.note_unmodelled_input(at),
                one_erased,
                5,
            ),
            (
                "a BS handed over as a glyph",
                |p, at| assert!(!p.predict_char('\u{8}', (0, 3), G, 80, at)),
                one_erased,
                5,
            ),
        ];
        let slow = Duration::from_millis(60);
        for mode in [PredictMode::Adaptive, PredictMode::Always] {
            for (label, erase, left, cursor) in erases {
                let mut p = Predictor::new(mode);
                let mut at = echo_turn(&mut p, 'l', 0, t0(), slow);
                at = echo_turn(&mut p, 's', 1, at, slow);
                at = echo_turn(&mut p, ' ', 2, at, slow);
                for ch in ['f', 'o', 'o'] {
                    assert!(p.predict_char(ch, (0, 3), G, 80, at));
                }
                assert_eq!(
                    p.overlay(at).len(),
                    3,
                    "{mode:?} {label}: control: the typed-ahead word paints"
                );
                erase(&mut p, at);
                at += slow;
                p.reconcile(Some((0, cursor)), G, false, at, cell(left));
                let ghost: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
                assert!(
                    ghost.is_empty(),
                    "{mode:?} {label}: the erased text is still on glass as {ghost:?}"
                );
            }
        }
    }

    #[test]
    fn only_text_ending_in_a_terminator_is_a_line_submit() {
        for yes in [
            &b"\r"[..],
            b"\n",
            b"\r\n",
            b"ls -l\r",
            "caf\u{e9}\n".as_bytes(),
        ] {
            assert!(is_line_submit(yes), "{yes:?}");
        }
        for no in [
            &b""[..],
            b"ls",
            b"\r ",
            b"\x1b\r",
            b"\x15clear\r",
            b"\x08\r",
            b"\x7f\n",
            b"\t\r",
            b"\xff\r",
        ] {
            assert!(!is_line_submit(no), "{no:?}");
        }
    }

    #[test]
    fn a_backgrounded_session_keeps_its_link_estimate() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let fast = Duration::from_millis(1);
        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "control: the slow link paints");

        let ssh = p.take_link_estimate();
        assert!(
            p.idle(),
            "the outgoing pane's guesses do not follow the window"
        );
        at = echo_turn(&mut p, 'x', 0, at, fast);
        assert!(p.predict_char('y', (0, 1), G, 80, at));
        assert!(p.overlay(at).is_empty(), "a fast local link stays dark");
        let local = p.take_link_estimate();

        p.restore_link_estimate(ssh);
        at += p.glitch_window() + Duration::from_millis(1);
        at = echo_turn(&mut p, 'a', 0, at, slow);
        assert!(p.predict_char('b', (0, 1), G, 80, at));
        assert_eq!(
            p.overlay(at).len(),
            1,
            "a restored slow-link estimate paints the second keystroke"
        );

        p.restore_link_estimate(local);
        at = echo_turn(&mut p, 'x', 0, at, fast);
        assert!(p.predict_char('y', (0, 1), G, 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "restoring a fast session's estimate must not paint either"
        );
    }

    #[test]
    fn a_returning_sessions_old_echo_cannot_prove_its_new_guess() {
        let slow = Duration::from_millis(60);
        let start = t0();

        let mut control = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut control, 'a', 0, start, slow);
        at = echo_turn(&mut control, 'b', 1, at, slow);
        let estimate = control.take_link_estimate();
        assert!(control.idle());
        control.restore_link_estimate(estimate);
        at = echo_turn(&mut control, 'q', 0, at, slow);
        assert!(control.predict_char('r', (0, 1), G, 80, at));
        assert_eq!(control.overlay(at).len(), 1, "debt-free return paints");

        let mut verdicts = Vec::new();
        for pending_guess in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let mut at = echo_turn(&mut p, 'a', 0, start, slow);
            at = echo_turn(&mut p, 'b', 1, at, slow);
            assert!(p.adaptive_slow && p.confirmed_epoch);
            if pending_guess {
                assert!(p.predict_char('n', (0, 2), G, 80, at));
                assert_eq!(p.overlay(at).len(), 1);
            } else {
                p.note_unmodelled_input(at);
            }
            let parked = p.take_link_estimate();
            assert!(p.idle());
            assert!(p.overlay(at).is_empty(), "outgoing pixels do not travel");

            at = echo_turn(&mut p, 'x', 0, at, Duration::from_millis(1));
            assert!(p.confirmed_epoch);
            let _other = p.take_link_estimate();
            p.restore_link_estimate(parked);
            assert!(p.adaptive_slow, "the returning link is still known slow");
            assert!(!p.confirmed_epoch, "a restored line must prove itself");

            assert!(p.predict_char('n', (0, 2), G, 80, at));
            assert!(p.overlay(at).is_empty());
            at += slow;
            p.reconcile(
                Some((0, 3)),
                G,
                false,
                at,
                cell(&[((0, 0), 'a'), ((0, 1), 'b'), ((0, 2), 'n')]),
            );
            assert!(p.idle(), "a matching glyph retires the guess's pixels");
            let borrowed_proof = p.confirmed_epoch;
            assert!(p.predict_char(' ', (0, 3), G, 80, at));
            let displayed: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
            verdicts.push((pending_guess, borrowed_proof, displayed));

            at += slow;
            p.reconcile(
                Some((0, 4)),
                G,
                false,
                at,
                cell(&[((0, 0), 'a'), ((0, 1), 'b'), ((0, 2), 'n'), ((0, 3), 'n')]),
            );
            assert!(p.idle());
            at += p.glitch_window() + Duration::from_millis(1);
            at = echo_turn(&mut p, 'q', 4, at, slow);
            assert!(p.predict_char('r', (0, 5), G, 80, at));
            assert_eq!(p.overlay(at).len(), 1, "fresh proof paints again");
        }
        assert_eq!(
            verdicts,
            [(true, false, Vec::new()), (false, false, Vec::new())],
            "a parked session's old output cannot prove a new guess or paint its space"
        );
    }

    #[test]
    fn restoring_an_estimate_clears_the_outgoing_panes_pixels() {
        let mut a = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(a.predict_char('x', (3, 4), G, 80, now));
        assert_eq!(a.overlay(now).len(), 1, "pane A has a ghost on glass");
        assert!(a.is_displaying(now));

        a.restore_link_estimate(LinkEstimate::default());

        assert!(
            a.overlay(now).is_empty(),
            "the outgoing pane's speculative pixels must not survive the swap"
        );
        assert!(!a.is_displaying(now));
        assert!(
            !a.confirmed_epoch,
            "nor may its confirmation license the incoming line"
        );
    }

    #[test]
    fn a_default_estimate_is_the_no_estimate_path() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow, "control: the latch is open");

        p.restore_link_estimate(LinkEstimate::default());
        at = echo_turn(&mut p, 'a', 0, at, slow);
        assert!(p.predict_char('b', (0, 1), G, 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "an unmeasured session re-earns the latch from its own samples"
        );
        at += slow;
        p.reconcile(Some((0, 2)), G, false, at, cell(&[((0, 1), 'b')]));
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "…and opens it on the second sample");
    }

    #[test]
    fn the_cold_link_probe_travels_with_the_session_estimate() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), G, 80, now));
        let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
        assert_eq!(
            p.expiry_backoff, 1,
            "control: the timeout widened the probe"
        );

        let est = p.take_link_estimate();
        assert_eq!(p.expiry_backoff, 0, "an unknown link probes from the floor");
        p.restore_link_estimate(est);
        assert_eq!(
            p.expiry_backoff, 1,
            "…the probing session gets its window back"
        );
    }

    #[test]
    fn session_reset_restarts_the_cold_link_probe() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        assert!(p.predict_char('a', (0, 0), G, 80, now));
        let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
        assert_eq!(
            p.expiry_backoff, 1,
            "control: the timeout widened the probe"
        );
        p.reset_session();
        assert_eq!(p.expiry_backoff, 0);
        assert_eq!(p.glitch_window(), Duration::from_millis(GLITCH_FLOOR_MS));
    }

    #[test]
    fn an_erase_we_did_not_model_leaves_no_ghost_where_nothing_echoes() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let slow = Duration::from_millis(60);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(
            p.adaptive_slow && p.confirmed_epoch,
            "control: a slow, proven line"
        );
        assert!(
            !p.predict_backspace(at),
            "nothing of ours to pop: 'b' already echoed"
        );
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        let wrong_cell = |p: &mut Predictor, at| {
            p.overlay(at)
                .iter()
                .any(|g| (g.row, g.col, g.ch) == (0, 2, 'c'))
        };
        assert!(
            !wrong_cell(&mut p, at),
            "'c' will echo at column 1, not 2: it must not be painted at 2"
        );
        at += slow;
        p.reconcile(
            Some((0, 2)),
            G,
            false,
            at,
            cell(&[((0, 0), 'a'), ((0, 1), 'c')]),
        );
        assert!(
            !wrong_cell(&mut p, at),
            "after the echo a ghost 'c' beside the real one is a stale duplicate"
        );
    }

    #[test]
    fn space_confirms_by_cursor_advance() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        p.predict_char(' ', (0, 0), G, 80, now);
        p.predict_char('b', (0, 0), G, 80, now);
        p.reconcile(Some((0, 2)), G, false, now, cell(&[((0, 0), 'a')]));
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 1, "'a' and the space retire; only 'b' remains");
        assert_eq!(shown[0].ch, 'b');
    }

    #[test]
    fn pending_queue_caps_at_max_predictions() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        for i in 0..MAX_PREDICTIONS {
            assert!(
                p.predict_char_in_grid('a', (0, 0), G, (80, 24), now),
                "prediction {i} below MAX_PREDICTIONS must succeed"
            );
        }
        assert_eq!(p.preds.len(), MAX_PREDICTIONS);
        assert!(
            !p.predict_char_in_grid('z', (0, 0), G, (80, 24), now),
            "prediction past MAX_PREDICTIONS must be declined"
        );
        assert_eq!(p.preds.len(), MAX_PREDICTIONS);
        for _ in 0..4 {
            p.note_line_submit(now);
            assert!(!p.predict_char_in_grid('b', (0, 0), G, (80, 24), now));
        }
        assert_eq!(p.preds.len(), MAX_PREDICTIONS);
    }

    fn seen_row(text: &str) -> Vec<Seen> {
        let mut row: Vec<Seen> = text.chars().map(|c| Seen::of_cell(c, false)).collect();
        row.resize(12, Seen::Blank);
        row
    }

    fn seen_cells(text: &str) -> impl FnMut(u16, u16) -> Seen + '_ {
        move |r, c| {
            let ch = if r == 0 {
                text.chars().nth(usize::from(c))
            } else {
                None
            };
            ch.map_or(Seen::Blank, |ch| Seen::of_cell(ch, false))
        }
    }

    fn slow_link_then(p: &mut Predictor, text: &str) -> Instant {
        let slow = Duration::from_millis(60);
        let mut at = echo_turn(p, 'a', 0, t0(), slow);
        at = echo_turn(p, 'b', 1, at, slow);
        assert!(p.adaptive_slow, "fixture: a slow link");
        p.note_unmodelled_input(at);
        at += Duration::from_millis(10);
        p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells(text));
        at + p.glitch_window() + Duration::from_millis(1)
    }

    #[test]
    fn a_mid_line_echo_proves_nothing() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let at = slow_link_then(&mut p, "ab cd");
        let row = seen_row("ab cd");
        assert!(p.wants_key_row(), "the run starts on the live cursor");
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        assert!(p.predict_glyph('e', &key, at));
        assert!(!p.wants_key_row(), "type-ahead reads no row");
        let echoed = at + Duration::from_millis(60);
        let before = p.events_for_test().tail_refusals;
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abe cd"));
        assert!(p.idle(), "glyph and cursor match: the pixels retire");
        assert_eq!(p.events_for_test().tail_refusals, before + 1);
        assert!(!p.confirmed_epoch, "a shifted tail proves nothing");
        let row = seen_row("abe cd");
        let key = KeySample {
            cursor: (0, 3),
            row: Some(&row),
            ..key
        };
        assert!(p.predict_glyph('i', &key, echoed));
        assert!(
            p.overlay(echoed).is_empty(),
            "'i' is not painted over the shifted space"
        );

        let mut p = Predictor::new(PredictMode::Adaptive);
        p.disable_rule_for_test(Rule::TailGuard);
        let at = slow_link_then(&mut p, "ab cd");
        let row = seen_row("ab cd");
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        assert!(p.predict_glyph('e', &key, at));
        let echoed = at + Duration::from_millis(60);
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abe cd"));
        assert!(p.confirmed_epoch, "rule off: the mid-line echo proves");
        assert!(p.predict_char('i', (0, 3), G, 12, echoed));
        assert_eq!(p.overlay(echoed).len(), 1);
    }

    #[test]
    fn a_right_prompt_the_echo_leaves_in_place_still_proves() {
        let screen = "ab      <r>";
        let echoed_screen = "abc     <r>";
        for with_row in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let at = slow_link_then(&mut p, screen);
            let row = seen_row(screen);
            let key = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (12, 6),
                row: with_row.then_some(row.as_slice()),
                ..KeySample::default()
            };
            assert!(p.predict_glyph('c', &key, at));
            let echoed = at + Duration::from_millis(60);
            let decorated = p.events_for_test().decorated_proofs;
            p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells(echoed_screen));
            assert!(p.idle());
            assert_eq!(p.confirmed_epoch, with_row, "with the row: {with_row}");
            assert_eq!(
                p.events_for_test().decorated_proofs,
                decorated + u64::from(with_row)
            );
            assert!(p.predict_char('d', (0, 3), G, 12, echoed));
            assert_eq!(p.overlay(echoed).len(), usize::from(with_row));
        }
    }

    #[test]
    fn a_run_typed_over_other_text_never_proves() {
        for (under, typed, proves) in [('x', 'e', false), ('e', 'e', true)] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let screen: String = ['a', 'b', under, 'd'].iter().collect();
            let at = slow_link_then(&mut p, &screen);
            let row = seen_row(&screen);
            let key = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            assert!(p.predict_glyph(typed, &key, at));
            let echoed = at + Duration::from_millis(60);
            let after: String = ['a', 'b', typed, 'd'].iter().collect();
            p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells(&after));
            assert!(p.idle());
            assert_eq!(p.confirmed_epoch, proves, "{typed:?} typed over {under:?}");
        }

        for (under, proves) in [('x', false), ('q', true)] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let screen: String = ['a', 'b', ' ', under, 'd'].iter().collect();
            let at = slow_link_then(&mut p, &screen);
            let row = seen_row(&screen);
            let key = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            assert!(p.predict_glyph('e', &key, at));
            assert!(p.predict_glyph('q', &key, at));
            let echoed = at + Duration::from_millis(60);
            p.reconcile_seen(Some((0, 4)), G, false, echoed, seen_cells("abeqd"));
            assert!(p.idle());
            assert_eq!(p.confirmed_epoch, proves, "'q' typed over {under:?}");
        }
    }

    #[test]
    fn a_dirty_tail_voids_a_standing_proof() {
        for guard in [true, false] {
            let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
            if !guard {
                p.disable_rule_for_test(Rule::TailGuard);
            }
            assert!(p.predict_char('n', (0, 2), G, 12, at));
            assert!(p.predict_char('o', (0, 2), G, 12, at));
            assert_eq!(
                p.overlay(at).len(),
                2,
                "control: the proven line paints both"
            );
            let echoed = at + Duration::from_millis(60);
            p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abn  sug"));
            assert_eq!(p.preds.len(), 1, "'o' still waits on its blank cell");
            assert_eq!(p.confirmed_epoch, !guard, "guard {guard}: proof voided");
            assert_eq!(
                p.overlay(echoed).len(),
                usize::from(!guard),
                "guard {guard}"
            );
        }
    }

    #[test]
    fn a_retired_burst_reads_its_tail_once() {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        for ch in ['c', 'd', 'e', 'f'] {
            assert!(p.predict_char(ch, (0, 2), G, 12, at));
        }
        let echoed = at + Duration::from_millis(60);
        let mut reads = 0usize;
        let mut observe = seen_cells("abcdef");
        p.reconcile_seen(Some((0, 6)), G, false, echoed, |r, c| {
            reads += 1;
            observe(r, c)
        });
        assert!(p.idle() && p.confirmed_epoch);
        assert_eq!(reads, 1 + 4 + 9, "observe calls for a four-guess burst");
    }

    type Keystroke<'a> = (char, u16, &'a str, &'a str, u16);

    fn typed_at_the_prompt(
        p: &mut Predictor,
        key: Keystroke<'_>,
        with_row: bool,
        at: Instant,
    ) -> (bool, Instant) {
        let (ch, col, screen, echoed, cursor) = key;
        let row = seen_row(screen);
        let key = KeySample {
            cursor: (0, col),
            coords: G,
            grid: (12, 6),
            row: with_row.then_some(row.as_slice()),
            ..KeySample::default()
        };
        assert!(p.wants_key_row(), "one key per round trip: nothing pending");
        assert!(p.predict_glyph(ch, &key, at), "{ch:?} is guessed");
        let frame = at + MS(10);
        p.reconcile_seen(Some((0, col)), G, false, frame, seen_cells(screen));
        let shown = p.overlay(frame).iter().any(|g| (g.col, g.ch) == (col, ch));
        let landed = at + MS(100);
        p.reconcile_seen(Some((0, cursor)), G, false, landed, seen_cells(echoed));
        assert!(p.idle(), "{ch:?} retired by its echo");
        (shown, landed + MS(5))
    }

    #[test]
    fn a_suggestion_beside_the_end_of_the_line_darkens_no_key() {
        let keys: [Keystroke<'_>; 6] = [
            ('e', 2, "$", "$ e", 3),
            ('c', 3, "$ e", "$ ec", 4),
            ('h', 4, "$ ec", "$ echo hello", 5),
            ('x', 5, "$ echo hello", "$ echx", 6),
            ('y', 6, "$ echx", "$ echxyzz", 7),
            ('z', 7, "$ echxyzz", "$ echxyzz", 8),
        ];
        for with_row in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let mut at = t0();
            let mut shown = Vec::new();
            for key in keys {
                let (on_glass, next) = typed_at_the_prompt(&mut p, key, with_row, at);
                shown.push(on_glass);
                at = next;
            }
            let events = p.events_for_test();
            if with_row {
                assert_eq!(
                    shown,
                    [false, false, true, true, true, true],
                    "two echoes latch the slow link, then every key paints"
                );
                assert_eq!(events.tail_refusals, 0, "{events:?}");
                assert!(
                    events.advance_waits >= 1,
                    "z waited for its echo: {events:?}"
                );
                assert!(p.confirmed_epoch);
            } else {
                assert_eq!(shown, [false, false, true, false, false, false], "no row");
                assert_eq!(events.tail_refusals, 1, "{events:?}");
            }
        }
    }

    #[test]
    fn a_glyph_typed_over_the_text_its_press_saw_waits_for_its_echo() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Always);
            if !rule_on {
                p.disable_rule_for_test(Rule::ScreenAsItWas);
            }
            let at = t0();
            let row = seen_row("$ du");
            let key = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            assert!(p.predict_glyph('s', &key, at));
            let unread = at + MS(5);
            p.reconcile_seen(Some((0, 2)), G, false, unread, seen_cells("$ du"));
            assert_eq!(
                p.pending().len(),
                usize::from(rule_on),
                "rule on {rule_on}: the screen as the press saw it"
            );
            let echoed = unread + MS(60);
            p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("$ sdu"));
            assert!(
                p.idle(),
                "rule on {rule_on}: retired by its echo or flushed"
            );
            assert_eq!(
                p.take_credited_for_test().len(),
                usize::from(rule_on),
                "rule on {rule_on}: credited with its own echo, or gone before it"
            );
        }
    }

    #[test]
    fn a_glyph_waiting_over_the_line_s_text_is_not_painted() {
        for rule_on in [true, false] {
            let fresh = || {
                let mut p = Predictor::new(PredictMode::Always);
                if !rule_on {
                    p.disable_rule_for_test(Rule::DarkOverText);
                }
                p
            };
            let row = seen_row("$ du -sh");
            let key = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            let mut p = fresh();
            let at = t0();
            assert!(p.predict_glyph('s', &key, at));
            assert_eq!(
                p.overlay(at).len(),
                usize::from(!rule_on),
                "rule on {rule_on}: s at its press, before any reconcile"
            );
            let unread = at + MS(5);
            p.reconcile_seen(Some((0, 2)), G, false, unread, seen_cells("$ du -sh"));
            assert_eq!(p.pending().len(), 1, "rule on {rule_on}: s waits");
            assert_eq!(
                p.overlay(unread).len(),
                usize::from(!rule_on),
                "rule on {rule_on}: s over the d of du"
            );
            assert_eq!(p.shown_through(), (!rule_on).then_some(0));
            assert_eq!(p.events_for_test().text_waits, u64::from(rule_on));
            let echoed = unread + MS(60);
            p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("$ sdu -sh"));
            assert!(p.idle(), "rule on {rule_on}: s retired by its echo");

            let mut p = fresh();
            p.note_unmodelled_input(at);
            let typed = at + MS(30);
            assert!(p.predict_glyph('l', &key, typed));
            assert_eq!(
                p.overlay(typed).len(),
                usize::from(!rule_on),
                "rule on {rule_on}: l at its press, before any reconcile"
            );
            let unread = typed + MS(5);
            p.reconcile_seen(Some((0, 2)), G, false, unread, seen_cells("$ du -sh"));
            assert_eq!(
                p.overlay(unread).len(),
                usize::from(!rule_on),
                "rule on {rule_on}: l over the d Ctrl-K is still to erase"
            );
            let erased = at + MS(60);
            p.reconcile_seen(Some((0, 2)), G, false, erased, seen_cells("$"));
            let shown: Vec<(u16, char)> = p.overlay(erased).iter().map(|g| (g.col, g.ch)).collect();
            assert_eq!(
                shown,
                [(2, 'l')],
                "rule on {rule_on}: l over the erased row"
            );
            let echoed = typed + MS(60);
            p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("$ l"));
            assert!(p.idle(), "rule on {rule_on}: l retired by its echo");

            let mut p = fresh();
            let keys: [Keystroke<'_>; 4] = [
                ('e', 2, "$", "$ e", 3),
                ('c', 3, "$ e", "$ ec", 4),
                ('h', 4, "$ ec", "$ echo hello", 5),
                ('x', 5, "$ echo hello", "$ echx", 6),
            ];
            let mut at = t0();
            let mut shown = Vec::new();
            for key in keys {
                let (on_glass, next) = typed_at_the_prompt(&mut p, key, true, at);
                shown.push(on_glass);
                at = next;
            }
            assert_eq!(shown, [true; 4], "rule on {rule_on}: x over the suggestion");
            assert_eq!(p.events_for_test().text_waits, 0);
        }
    }

    #[test]
    fn a_guess_after_the_head_over_the_line_s_text_is_not_painted() {
        for rule_on in [true, false] {
            for (screen, cursor, typed) in
                [("$ ssh host", 2u16, "su"), ("$ git commit", 5, " -C x")]
            {
                let mut p = Predictor::new(PredictMode::Always);
                if !rule_on {
                    p.disable_rule_for_test(Rule::DarkOverText);
                }
                let row = seen_row(screen);
                let key = KeySample {
                    cursor: (0, cursor),
                    coords: G,
                    grid: (12, 6),
                    row: Some(&row),
                    ..KeySample::default()
                };
                let mut at = t0();
                for ch in typed.chars() {
                    assert!(p.predict_glyph(ch, &key, at), "{ch:?} is guessed");
                    at += MS(5);
                }
                let painted = if rule_on { 1 } else { typed.len() };
                let expect: Vec<(u16, char)> =
                    (cursor..).zip(typed.chars()).take(painted).collect();
                let shown = |p: &mut Predictor| -> Vec<(u16, char)> {
                    p.overlay(at).iter().map(|g| (g.col, g.ch)).collect()
                };
                assert_eq!(
                    shown(&mut p),
                    expect,
                    "rule on {rule_on}, {screen:?}: at the presses"
                );
                p.reconcile_seen(Some((0, cursor)), G, false, at, seen_cells(screen));
                assert_eq!(
                    p.pending().len(),
                    typed.len(),
                    "{screen:?}: every guess waits"
                );
                assert_eq!(
                    shown(&mut p),
                    expect,
                    "rule on {rule_on}, {screen:?}: at the reconcile"
                );
                assert_eq!(p.events_for_test().text_waits, u64::from(rule_on));
            }
        }
    }

    #[test]
    fn a_hold_over_the_line_s_text_outlives_its_guess() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Always);
            if !rule_on {
                p.disable_rule_for_test(Rule::DarkOverText);
            }
            let row = seen_row("$ du -sh");
            let key = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            let at = t0();
            for (ch, after) in [('x', 0), ('y', 50), ('z', 100)] {
                assert!(
                    p.predict_glyph(ch, &key, at + MS(after)),
                    "{ch:?} is guessed"
                );
            }
            let shown = |p: &mut Predictor, now| -> Vec<(u16, char)> {
                p.overlay(now).iter().map(|g| (g.col, g.ch)).collect()
            };
            let read = at + MS(110);
            p.reconcile_seen(Some((0, 2)), G, false, read, seen_cells("$ du -sh"));
            let all = [(2, 'x'), (3, 'y'), (4, 'z')];
            let expect = |from: usize| {
                if rule_on {
                    Vec::new()
                } else {
                    all[from..].to_vec()
                }
            };
            assert_eq!(shown(&mut p, read), expect(0), "rule on {rule_on}: x waits");
            assert_eq!(p.events_for_test().text_waits, u64::from(rule_on));
            let x_gone = at + MS(251);
            assert_eq!(
                shown(&mut p, x_gone),
                expect(1),
                "rule on {rule_on}: x expired"
            );
            assert_eq!(p.pending().len(), 2, "x expired, y and z wait");
            let y_gone = at + MS(551);
            assert_eq!(
                shown(&mut p, y_gone),
                expect(2),
                "rule on {rule_on}: y expired"
            );
            assert_eq!(p.pending().len(), 1, "y expired, z waits");
        }
    }

    #[test]
    fn a_glyph_echoed_before_its_tail_moves_proves_nothing_after() {
        for rule_on in [true, false] {
            for i_first in [true, false] {
                let mut p = Predictor::new(PredictMode::Adaptive);
                if !rule_on {
                    p.disable_rule_for_test(Rule::StandingTail);
                }
                let at = slow_link_then(&mut p, "ab cd");
                let row = seen_row("ab cd");
                let key = KeySample {
                    cursor: (0, 2),
                    coords: G,
                    grid: (12, 6),
                    row: Some(&row),
                    ..KeySample::default()
                };
                assert!(p.predict_glyph('e', &key, at));
                let glyph_only = at + MS(60);
                p.reconcile_seen(Some((0, 3)), G, false, glyph_only, seen_cells("abecd"));
                assert!(p.idle() && p.confirmed_epoch, "the glyph alone proves");
                let (press_row, redrawn) = if i_first {
                    ("abecd", glyph_only + MS(5))
                } else {
                    let redrawn = glyph_only + MS(5);
                    p.reconcile_seen(Some((0, 3)), G, false, redrawn, seen_cells("abe cd"));
                    ("abe cd", redrawn)
                };
                let row = seen_row(press_row);
                let key = KeySample {
                    cursor: (0, 3),
                    row: Some(&row),
                    ..key
                };
                assert!(p.predict_glyph('i', &key, redrawn));
                let frame = redrawn + MS(5);
                p.reconcile_seen(Some((0, 3)), G, false, frame, seen_cells("abe cd"));
                assert_eq!(
                    p.overlay(frame).len(),
                    usize::from(!rule_on),
                    "rule on {rule_on}, i typed before the redraw {i_first}"
                );
                if rule_on {
                    assert_eq!(p.events_for_test().standing_voids, 1);
                }
            }
        }
    }

    #[test]
    fn a_shifted_glyph_is_not_a_pending_guess_s_echo() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let at = slow_link_then(&mut p, "abo");
        let row = seen_row("abo");
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        assert!(p.predict_glyph('o', &key, at));
        let glyph_only = at + MS(60);
        p.reconcile_seen(Some((0, 3)), G, false, glyph_only, seen_cells("abo"));
        assert!(p.idle() && p.confirmed_epoch, "the glyph alone proves");
        let key = KeySample {
            cursor: (0, 3),
            ..key
        };
        assert!(p.predict_glyph('o', &key, glyph_only));
        assert!(p.predict_glyph('z', &key, glyph_only));
        assert_eq!(
            p.overlay(glyph_only).len(),
            2,
            "the proven line paints both"
        );
        let redrawn = glyph_only + MS(5);
        p.reconcile_seen(Some((0, 3)), G, false, redrawn, seen_cells("aboo"));
        assert_eq!(p.preds.len(), 2, "the second 'o' waits for its own echo");
        assert!(!p.confirmed_epoch, "the displaced 'o' is not its echo");
        assert!(
            p.overlay(redrawn).is_empty(),
            "'z' is not painted a cell short"
        );
    }

    #[test]
    fn a_glyph_already_on_screen_is_not_its_echo() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::CursorAdvance);
            }
            let slow = MS(300);
            let mut at = t0();
            for (col, ch) in (0u16..).zip(['a', 'b', 'o']) {
                let screen: String = "abo".chars().take(usize::from(col)).collect();
                let row = seen_row(&screen);
                let key = KeySample {
                    cursor: (0, col),
                    coords: G,
                    grid: (12, 6),
                    row: Some(&row),
                    ..KeySample::default()
                };
                assert!(p.predict_glyph(ch, &key, at));
                at += slow;
                let echoed: String = "abo".chars().take(usize::from(col) + 1).collect();
                p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&echoed));
            }
            assert!(
                p.confirmed_epoch && p.adaptive_slow && p.proof_line_end,
                "fixture: a slow line, proven at its end"
            );
            let window = p.glitch_window();
            assert_eq!(window, MS(900), "fixture: three round trips");
            let screen = "abodice";
            let row = seen_row(screen);
            let key = KeySample {
                cursor: (0, 3),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            for ch in ['d', 'i', 'c', 'e'] {
                assert!(p.predict_glyph(ch, &key, at));
            }
            let frame = at + MS(5);
            p.reconcile_seen(Some((0, 3)), G, false, frame, seen_cells(screen));
            if rule_on {
                assert_eq!(p.preds.len(), 4, "nothing echoed yet");
                assert_eq!(p.glitch_window(), window, "and nothing measured");
                assert_eq!(p.overlay(frame).len(), 4, "on glass over the suggestion");
                let echoed = at + slow;
                p.reconcile_seen(Some((0, 7)), G, false, echoed, seen_cells(screen));
                assert!(p.idle() && p.confirmed_epoch, "retired by their echoes");
                assert_eq!(p.glitch_window(), window, "a 300 ms sample, as before");
            } else {
                assert!(p.idle(), "rule off: retired before any echo");
                assert!(
                    p.glitch_window() < MS(300),
                    "rule off: the window collapsed below the round trip: {:?}",
                    p.glitch_window()
                );
            }
        }
    }

    #[test]
    fn a_cancelled_glyph_s_echo_is_owed_to_the_same_glyph_typed_next() {
        for rule_on in [true, false] {
            let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::CancelOwes);
            }
            assert!(p.predict_char('c', (0, 2), G, 80, at));
            assert!(p.predict_backspace(at), "our own guess, cancelled");
            let again = at + MS(20);
            assert!(p.predict_char('c', (0, 2), G, 80, again));
            assert_eq!(
                p.preds.front().map(|g| g.col),
                Some(2),
                "on the cancelled cell"
            );
            let _ = p.take_credited_for_test();
            p.reconcile(
                Some((0, 3)),
                G,
                false,
                at + MS(60),
                cell(&after_ab(&[((0, 2), 'c')])),
            );
            assert!(
                p.idle(),
                "the cell shows the guess's glyph, the cursor past it"
            );
            assert_eq!(
                p.take_credited_for_test().len(),
                usize::from(!rule_on),
                "rule on {rule_on}: credited with the cancelled key's echo"
            );
            assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
        }
    }

    #[test]
    fn a_right_to_left_row_is_never_guessed_on() {
        let opaque = |text: &str| -> Vec<Seen> {
            let mut row: Vec<Seen> = text.chars().map(|c| Seen::of_cell(c, true)).collect();
            row.resize(12, Seen::Blank);
            row
        };
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::RightToLeftRows);
            }
            let at = slow_link_then(&mut p, "");
            let screen = "$ אב    <r>";
            let row = opaque(screen);
            assert_eq!(
                row.get(5),
                Some(&Seen::Opaque(None)),
                "fixture: an opaque blank"
            );
            let key = KeySample {
                cursor: (0, 5),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            assert_eq!(
                p.predict_glyph('e', &key, at),
                !rule_on,
                "rule on {rule_on}"
            );
            if rule_on {
                assert!(p.idle() && p.owed_at == Some(at), "declined, owed");
                assert_eq!(p.events_for_test().bidi_refusals, 1);
                let plain = seen_row("$ ab");
                let key = KeySample {
                    row: Some(&plain),
                    cursor: (0, 4),
                    ..key
                };
                assert!(!p.predict_glyph('\u{5d0}', &key, at));
                assert_eq!(p.events_for_test().bidi_refusals, 2);
                continue;
            }
            let echoed = opaque("$ אב e  <r>");
            let landed = at + MS(60);
            p.reconcile_seen(Some((0, 6)), G, false, landed, |r, c| {
                if r == 0 {
                    echoed.get(usize::from(c)).copied().unwrap_or(Seen::Blank)
                } else {
                    Seen::Blank
                }
            });
            assert!(p.idle());
            assert!(
                p.confirmed_epoch,
                "the right prompt stayed, the line proves"
            );
            assert_eq!(p.events_for_test().tail_refusals, 0);
        }
    }

    #[test]
    fn a_predictor_still_off_wants_the_row_of_the_press_that_starts_it() {
        let mut p = Predictor::default();
        assert!(p.wants_key_row(), "Off, with nothing pending");
        p.set_mode(PredictMode::Adaptive);
        assert!(p.wants_key_row());
        let row = seen_row("ab      <r>");
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        assert!(p.predict_glyph('c', &key, t0()));
        assert!(!p.wants_key_row(), "type-ahead reads no row");
        assert_eq!(p.tail, Some((0, 2)), "the first run took its snapshot");
    }

    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn typed_owe_run(second: char, echo_x: &str, rule_off: Option<Rule>) -> (Predictor, bool) {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        if let Some(rule) = rule_off {
            p.disable_rule_for_test(rule);
        }
        p.note_line_submit(at);
        assert!(p.predict_char('x', (1, 0), G, 12, at + MS(100)));
        let row1 = |text: &str| -> Vec<((u16, u16), char)> {
            (0u16..)
                .zip(text.chars())
                .filter(|&(_, ch)| ch != ' ')
                .map(|(col, ch)| ((1, col), ch))
                .collect()
        };
        p.reconcile(Some((1, 1)), G, false, at + MS(160), cell(&row1(echo_x)));
        assert!(p.idle() && !p.confirmed_epoch, "x retired without credit");
        assert!(p.predict_char(second, (1, 1), G, 12, at + MS(200)));
        let echoed: String = ['x', second].iter().collect();
        p.reconcile(Some((1, 2)), G, false, at + MS(270), cell(&row1(&echoed)));
        assert!(p.idle());
        assert!(p.predict_char('z', (1, 2), G, 12, at + MS(270)));
        let painted = !p.overlay(at + MS(270)).is_empty();
        (p, painted)
    }

    #[test]
    fn an_uncredited_guess_owes_only_its_own_glyph() {
        let (p, proved) = typed_owe_run('y', "x", None);
        assert!(
            proved,
            "a different glyph earns credit past the Enter's window"
        );
        assert_eq!(p.events_for_test().typed_owes, 1, "x owed typed");
        let (_, proved) = typed_owe_run('x', "x", None);
        assert!(!proved, "the same glyph may be x's own echo: no credit");
        let (_, proved) = typed_owe_run('y', "x", Some(Rule::TypedOwes));
        assert!(
            !proved,
            "TypedOwes off: owed as any echo, the cascade refuses y"
        );
        let (_, proved) = typed_owe_run('x', "x", Some(Rule::TypedOwesSameGlyph));
        assert!(proved, "TypedOwesSameGlyph off: the second x is credited");
    }

    #[test]
    fn a_guess_retired_over_a_moved_tail_owes_any_echo() {
        let moved = "x  sug";
        let (p, proved) = typed_owe_run('y', moved, None);
        assert!(!proved, "owed as any echo: y refused credit");
        assert_eq!(
            p.events_for_test().typed_owes,
            1,
            "only y's own retirement is typed"
        );
        let (_, proved) = typed_owe_run('y', moved, Some(Rule::TypedOwesCleanTail));
        assert!(proved, "TypedOwesCleanTail off: typed over a moved tail");
    }

    #[test]
    fn typed_owes_fold_into_any_echo_past_the_cap_and_at_a_take() {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        p.note_line_submit(at);
        let born = |i: u16| at + MS(10 + u64::from(i));
        for i in 0..=64u16 {
            assert!(p.predict_char('x', (1, i), G, 80, born(i)));
        }
        assert!(p.predict_char('y', (1, 65), G, 80, born(65)));
        let echoed: Vec<((u16, u16), char)> = (0..=64u16).map(|i| ((1, i), 'x')).collect();
        p.reconcile(Some((1, 65)), G, false, at + MS(120), cell(&echoed));
        assert_eq!(p.preds.len(), 1, "y still waits");
        assert_eq!(p.glyph_owes.len(), MAX_GLYPH_OWES, "capped");
        assert_eq!(p.owed_at, Some(born(0)), "the oldest folded into any echo");
        assert_eq!(
            p.preds.front().and_then(|g| g.owed),
            Some(born(0)),
            "…raised for y"
        );
        let estimate = p.take_link_estimate();
        assert!(p.glyph_owes.is_empty());
        p.restore_link_estimate(estimate);
        assert_eq!(
            p.owed_at,
            Some(born(65)),
            "the session carries its typed owes and pending guesses as owes of any echo"
        );
    }

    #[test]
    fn a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move() {
        fn proven(cols: u16, start: u16) -> (Predictor, Instant) {
            let mut predictor = Predictor::new(PredictMode::Adaptive);
            let mut at = t0();
            let mut cells = Vec::new();
            for (offset, ch) in [(0, 'a'), (1, 'b')] {
                let col = start + offset;
                assert!(at_the_end(&mut predictor, ch, (0, col), (cols, 6), at));
                cells.push(((0, col), ch));
                at += MS(60);
                predictor.reconcile(
                    Some((0, (col + 1).min(cols - 1))),
                    G,
                    false,
                    at,
                    cell(&cells),
                );
                assert!(predictor.idle(), "ordinary echo retires its guess");
            }
            (predictor, at)
        }

        for ahead in [false, true] {
            let (mut predictor, mut at) = proven(80, 0);
            assert!(predictor.predict_char_in_grid('c', (0, 2), G, (80, 6), at));
            assert_eq!(
                predictor.overlay(at).len(),
                1,
                "the original line is proven"
            );
            assert!(predictor.predict_backspace(at));
            assert!(predictor.idle(), "only our trailing guess was cancelled");
            at += MS(30);
            let cursor = (0, if ahead { 3 } else { 2 });
            let cells = if ahead {
                after_ab(&[((0, 2), 'c')])
            } else {
                AB.to_vec()
            };
            predictor.reconcile(Some(cursor), G, false, at, cell(&cells));
            assert!(predictor.predict_char_in_grid('d', cursor, G, (80, 6), at));
            assert_eq!(predictor.pending_bounds(), Some((0, 2, 1, 3)));
            assert_eq!(
                predictor.overlay(at).len(),
                1,
                "legitimate pair transit retains proof"
            );
        }
        let (mut predictor, mut at) = proven(9, 6);
        assert!(predictor.predict_char_in_grid('c', (0, 8), G, (9, 6), at));
        assert_eq!(predictor.overlay(at).len(), 1);
        assert!(predictor.predict_backspace(at));
        at += MS(30);
        predictor.reconcile(
            Some((0, 8)),
            G,
            false,
            at,
            cell(&[((0, 6), 'a'), ((0, 7), 'b'), ((0, 8), 'c')]),
        );
        assert!(predictor.predict_char_in_grid('d', (0, 8), G, (9, 6), at));
        assert_eq!(
            predictor.pending_bounds(),
            Some((0, 8, 1, 9)),
            "last-column parking stays in its cell"
        );
        assert_eq!(predictor.overlay(at).len(), 1);

        let (mut predictor, at) = proven(9, 7);
        assert!(predictor.predict_char_in_grid('c', (0, 8), G, (9, 6), at));
        assert_eq!(predictor.pending_bounds(), Some((1, 0, 2, 1)));
        assert!(predictor.predict_backspace(at));
        predictor.reconcile(
            Some((0, 8)),
            G,
            false,
            at,
            cell(&[((0, 7), 'a'), ((0, 8), 'b')]),
        );
        assert!(predictor.predict_char_in_grid('d', (0, 8), G, (9, 6), at));
        assert_eq!(
            predictor.pending_bounds(),
            Some((1, 0, 2, 1)),
            "pending wrap remains a legitimate transit position"
        );

        let mut wrong = Vec::new();
        for cursor in [(1, 0), (0, 6)] {
            for reconciled in [false, true] {
                let (mut predictor, mut at) = proven(80, 0);
                assert!(predictor.predict_char_in_grid('c', (0, 2), G, (80, 6), at));
                assert_eq!(predictor.overlay(at).len(), 1);
                assert!(predictor.predict_backspace(at));
                at += MS(30);
                if reconciled {
                    predictor.reconcile(Some(cursor), G, false, at, cell(&AB));
                }
                assert!(predictor.predict_char_in_grid('s', cursor, G, (80, 6), at));
                let bounds = predictor.pending_bounds();
                let visible = predictor.overlay(at).len();
                let expected = Some((
                    usize::from(cursor.0),
                    usize::from(cursor.1),
                    usize::from(cursor.0) + 1,
                    usize::from(cursor.1) + 1,
                ));
                if bounds != expected || visible != 0 {
                    wrong.push((Some(cursor), reconciled, bounds, visible));
                }
            }
        }
        let (mut predictor, at) = proven(80, 0);
        assert!(predictor.predict_char_in_grid('c', (0, 2), G, (80, 6), at));
        assert!(predictor.predict_backspace(at));
        predictor.reconcile(None, G, false, at, cell(&AB));
        assert!(predictor.predict_char_in_grid('s', (0, 2), G, (80, 6), at));
        let visible = predictor.overlay(at).len();
        if visible != 0 {
            wrong.push((None, true, predictor.pending_bounds(), visible));
        }
        assert!(
            wrong.is_empty(),
            "PREDICT_CANCELLED_ANCHOR_SURVIVED_UNRELATED_CURSOR: {wrong:?}"
        );
    }

    #[test]
    fn a_cancelled_glyph_anchors_the_next_on_its_cell() {
        let transit = |rule_on: bool| {
            let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::CancelTransit);
            }
            let owed = p.owed_at;
            assert!(p.predict_char('c', (0, 2), G, 80, at));
            assert!(p.predict_backspace(at), "our own guess, cancelled");
            assert_eq!(p.owed_at, owed, "the cancelled pair owes nothing");
            let ahead = at + MS(30);
            p.reconcile(
                Some((0, 3)),
                G,
                false,
                ahead,
                cell(&after_ab(&[((0, 2), 'c')])),
            );
            assert!(p.predict_char('x', (0, 3), G, 80, ahead));
            assert!(p.predict_char('x', (0, 3), G, 80, ahead));
            let cells: Vec<(u16, u16)> = p.preds.iter().map(|g| (g.row, g.col)).collect();
            (p, ahead, cells)
        };
        let echoed = after_ab(&[((0, 2), 'x'), ((0, 3), 'x')]);

        let (mut p, ahead, cells) = transit(true);
        assert_eq!(cells, [(0, 2), (0, 3)], "anchored on the cancelled cell");
        let landed = ahead + MS(60);
        p.reconcile(Some((0, 4)), G, false, landed, cell(&echoed));
        assert!(p.idle(), "both echoed where they were armed");
        assert!(p.confirmed_epoch, "and their own echoes proved the line");

        let (mut p, ahead, cells) = transit(false);
        assert_eq!(cells, [(0, 3), (0, 4)], "rule off: anchored a cell right");
        let landed = ahead + MS(60);
        p.reconcile(Some((0, 4)), G, false, landed, cell(&echoed));
        assert_eq!(
            p.preds.len(),
            1,
            "the first `x` retired by the second's echo"
        );
        assert!(
            p.confirmed_epoch,
            "rule off: credited with the second `x`'s echo"
        );
        assert_eq!(p.overlay(landed).len(), 1, "rule off: painted at (0, 4)");
    }

    #[test]
    fn a_cancelled_glyph_at_the_margin_does_not_wrap_the_next() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::CancelTransit);
            }
            let slow = MS(60);
            let mut at = echo_turn(&mut p, 'a', 6, t0(), slow);
            at = echo_turn(&mut p, 'b', 7, at, slow);
            let grid = (9, 6);
            assert!(
                p.predict_char_in_grid('c', (0, 8), G, grid, at),
                "the last column"
            );
            assert_eq!(p.overlay(at).len(), 1, "control: the proven line paints c");
            assert!(p.predict_backspace(at));
            at += MS(30);
            p.reconcile(
                Some((0, 8)),
                G,
                false,
                at,
                cell(&[((0, 6), 'a'), ((0, 7), 'b'), ((0, 8), 'c')]),
            );
            assert!(p.deferred_wrap, "fixture: c's echo parked the terminal");
            assert!(p.predict_char_in_grid('d', (0, 8), G, grid, at));
            let cell = p.preds.front().map(|g| (g.row, g.col));
            assert_eq!(
                cell,
                Some(if rule_on { (0, 8) } else { (1, 0) }),
                "rule on {rule_on}"
            );
        }
    }

    #[test]
    fn an_opaque_cell_is_its_glyph_to_the_glyph_lane() {
        assert_eq!(Seen::of_cell(' ', false), Seen::Blank);
        assert_eq!(Seen::of_cell(' ', true), Seen::Opaque(None));
        assert_eq!(Seen::of_cell('日', true).legacy(), Some('日'));
        let screens: [&[((u16, u16), char)]; 3] = [
            &[((0, 0), 'a')],
            &[((0, 0), 'a'), ((0, 1), 'x')],
            &[((0, 0), 'a'), ((0, 1), 'b'), ((0, 2), '日')],
        ];
        let mut head = Predictor::new(PredictMode::Always);
        for ch in ['a', 'b', 'c'] {
            assert!(head.predict_char(ch, (0, 0), G, 4, t0()));
        }
        let first = cell(screens[0]);
        head.reconcile_seen(Some((0, 1)), G, false, t0(), |r, c| {
            first(r, c).map_or(Seen::Opaque(None), |ch| Seen::Opaque(Some(ch)))
        });
        assert!(head.idle(), "`b` on an opaque blank: flushed");
        for screen in screens {
            let mut plain = Predictor::new(PredictMode::Always);
            let mut opaque = Predictor::new(PredictMode::Always);
            opaque.disable_rule_for_test(Rule::OpaqueHead);
            for p in [&mut plain, &mut opaque] {
                for ch in ['a', 'b', 'c'] {
                    assert!(p.predict_char(ch, (0, 0), G, 4, t0()));
                }
            }
            let at = t0();
            plain.reconcile(Some((0, 1)), G, false, at, cell(screen));
            let marked = cell(screen);
            opaque.reconcile_seen(Some((0, 1)), G, false, at, |r, c| match marked(r, c) {
                Some(ch) => Seen::Opaque(Some(ch)),
                None => Seen::Opaque(None),
            });
            let pending = |p: &Predictor| {
                p.preds
                    .iter()
                    .map(|g| (g.row, g.col, g.ch))
                    .collect::<Vec<_>>()
            };
            assert_eq!(pending(&plain), pending(&opaque), "{screen:?}");
            assert_eq!(plain.deferred_wrap, opaque.deferred_wrap, "{screen:?}");
        }
    }

    const RTT: Duration = Duration::from_millis(60);

    fn sample(row: &[Seen], cursor: u16) -> KeySample<'_> {
        KeySample {
            cursor: (0, cursor),
            coords: G,
            grid: (12, 6),
            row: Some(row),
            ..KeySample::default()
        }
    }

    fn end_of(screen: &str) -> u16 {
        u16::try_from(screen.chars().count()).unwrap_or(u16::MAX)
    }

    fn type_after(p: &mut Predictor, screen: &str, text: &str, at: Instant) -> (String, Instant) {
        let mut screen = screen.to_owned();
        let mut at = at;
        for ch in text.chars() {
            let col = end_of(&screen);
            let row = seen_row(&screen);
            assert!(p.predict_glyph(ch, &sample(&row, col), at));
            at += RTT;
            screen.push(ch);
            p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&screen));
        }
        (screen, at)
    }

    fn edited_line(mode: PredictMode, text: &str) -> (Predictor, String, Instant) {
        let mut p = Predictor::new(mode);
        let (screen, at) = type_after(&mut p, "$ ", text, t0());
        assert!(
            p.adaptive_slow && p.confirmed_epoch,
            "fixture: a slow, proven line"
        );
        let hi = end_of(&screen);
        assert_eq!(
            p.edit_line_for_test(),
            Some((0, 2, hi, hi)),
            "fixture: the known line"
        );
        (p, screen, at)
    }

    fn press(
        p: &mut Predictor,
        key: EditKey,
        screen: &str,
        cursor: u16,
        at: Instant,
    ) -> EditOutcome {
        let row = seen_row(screen);
        p.predict_edit(key, &sample(&row, cursor), at)
    }

    type Shown = (Vec<(u16, char)>, Option<(u16, u16)>);

    fn frame(p: &mut Predictor, screen: &str, cursor: u16, at: Instant) -> Shown {
        p.reconcile_seen(Some((0, cursor)), G, false, at, seen_cells(screen));
        let cells = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
        (cells, p.overlay_cursor())
    }

    #[test]
    fn a_first_motion_stays_dark_until_its_echo_proves_the_class() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 4, at),
            EditOutcome::Armed
        );
        assert_eq!(
            frame(&mut p, &screen, 4, at),
            (vec![], None),
            "the motion class is unproven: nothing shows"
        );
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        assert!(p.idle(), "the state the screen shows retires the motion");
        assert_eq!(p.events_for_test().motions_proven, 1);
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 4, 3)));
        assert_eq!(p.proof_cell, Some((0, 3)), "the proof follows the cursor");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 3, at),
            EditOutcome::Armed
        );
        assert_eq!(
            frame(&mut p, &screen, 3, at),
            (vec![], Some((0, 2))),
            "the second `←` shows at once: a cursor and no cell"
        );

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        p.disable_rule_for_test(Rule::MotionClassProof);
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        assert_eq!(frame(&mut p, &screen, 4, at).1, Some((0, 3)), "rule off");
    }

    #[test]
    fn a_burst_the_screen_cannot_tell_apart_retires_at_the_smallest_state() {
        for off in [
            None,
            Some(Rule::SmallestMatch),
            Some(Rule::NetNullUncredited),
        ] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if let Some(rule) = off {
                p.disable_rule_for_test(rule);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let _ = press(&mut p, EditKey::Right, &screen, 4, at);
            let net_null = p.events_for_test().net_null_retired;
            let _ = p.take_credited_for_test();
            let read = at + MS(5);
            p.reconcile_seen(Some((0, 4)), G, false, read, seen_cells(&screen));
            let credited = p.take_credited_for_test();
            assert!(p.idle(), "{off:?}");
            if off.is_none() {
                assert!(credited.is_empty(), "nothing was read: nothing credited");
                assert_eq!(p.events_for_test().net_null_retired, net_null + 1);
                assert_eq!(p.owed_at, Some(at), "both keys owed");
                assert!(!p.confirmed_epoch, "an owe beside a proof is no proof");
            } else {
                assert_eq!(credited.len(), 2, "{off:?}: credited before any echo");
            }
        }
    }

    #[test]
    fn an_erase_at_the_end_paints_its_vacated_cell_blank() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 5, at),
            EditOutcome::Armed
        );
        assert_eq!(
            frame(&mut p, &screen, 5, at),
            (vec![(4, ' ')], Some((0, 4)))
        );
        let at = at + RTT;
        let _ = frame(&mut p, "$ ab", 4, at);
        assert!(p.idle());
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 4, 4)));
        let row = seen_row("$ ab");
        assert!(p.predict_glyph('x', &sample(&row, 4), at));
        assert_eq!(p.overlay(at).len(), 1, "the append after it shows");
    }

    #[test]
    fn a_split_erase_echo_matches_no_state_early() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
            if !rule_on {
                p.disable_rule_for_test(Rule::VacatedBlankInMatch);
            }
            for _ in 0..2 {
                assert_eq!(
                    press(&mut p, EditKey::Backspace, &screen, 5, at),
                    EditOutcome::Armed,
                    "the second erase: `c` on screen is the chain's own text"
                );
            }
            let waits = p.events_for_test().transit_waits;
            p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ ab"));
            if rule_on {
                assert_eq!(p.preds.len(), 2, "no state matches: it waits");
                assert_eq!(p.events_for_test().transit_waits, waits + 1);
                p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ a"));
                assert!(p.idle(), "the second piece lands: `S_2`");
            } else {
                assert!(p.idle(), "rule off: matched with `b` still shown");
            }
        }
    }

    #[test]
    fn an_erase_in_transit_refuses_new_text_past_every_state() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        assert!(
            p.proof_line_end,
            "fixture: the original append proved the end"
        );
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 4, at),
            EditOutcome::Armed
        );
        assert_eq!(
            frame(&mut p, &screen, 4, at),
            (vec![(3, ' ')], Some((0, 3))),
            "ordinary pending erase is visible on its glyph proof"
        );
        let waits = p.events_for_test().transit_waits;
        assert_eq!(
            frame(&mut p, "$ ab", 3, at + RTT),
            (vec![(3, ' ')], Some((0, 3))),
            "the cursor moved first; its own old b is allowed in transit"
        );
        assert!(!p.idle());
        assert_eq!(p.events_for_test().transit_waits, waits + 1);
        assert_eq!(
            frame(&mut p, "$ a", 3, at + RTT + RTT),
            (vec![], None),
            "the cell clears and the ordinary erase retires"
        );
        assert!(p.idle());
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 3, 3)));

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        assert!(p.proof_line_end, "fixture: standing-tail bypass is present");
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 4, at),
            EditOutcome::Armed
        );
        assert_eq!(
            frame(&mut p, &screen, 4, at),
            (vec![(3, ' ')], Some((0, 3)))
        );
        let shown = frame(&mut p, "$ abX", 3, at + RTT);
        assert!(
            p.idle() && !p.confirmed_epoch && shown == (vec![], None),
            "EDIT_ERASE_TRANSIT_ACCEPTED_NEW_TAIL: idle={} proof={} shown={shown:?}",
            p.idle(),
            p.confirmed_epoch
        );
    }

    #[test]
    fn an_arrow_that_cannot_be_placed_keeps_the_guesses_in_flight() {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        assert_eq!(p.edit_line_for_test(), Some((0, 0, 2, 2)), "a known line");
        assert!(p.predict_char('n', (0, 2), G, 12, at));
        assert!(p.predict_char('o', (0, 2), G, 12, at));
        assert_eq!(p.overlay(at).len(), 2);
        let decline = p.events_for_test().decline_motions;
        assert_eq!(
            press(&mut p, EditKey::Left, "ab", 2, at),
            EditOutcome::Declined
        );
        assert_eq!(p.events_for_test().decline_motions, decline + 1);
        assert_eq!(p.overlay(at).len(), 2, "still on glass, carried");
        assert!(p.anchor_lost && p.owed_at == Some(at), "owed, anchor lost");
        assert!(!p.predict_char('x', (0, 2), G, 12, at), "nothing behind it");

        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("ab"));
        assert_eq!(
            press(&mut p, EditKey::Left, "ab", 2, at),
            EditOutcome::Armed
        );
    }

    #[test]
    fn an_arrow_at_an_edge_of_the_text_is_declined() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::TextEdges);
            }
            let right = press(&mut p, EditKey::Right, &screen, 4, at);
            assert_eq!(right == EditOutcome::Armed, !rule_on, "→ at the end");

            let (mut p, screen, mut at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::TextEdges);
            }
            for cursor in [4, 3] {
                assert_eq!(
                    press(&mut p, EditKey::Left, &screen, cursor, at),
                    EditOutcome::Armed
                );
                at += RTT;
                let _ = frame(&mut p, &screen, cursor - 1, at);
                assert!(p.idle());
            }
            let left = press(&mut p, EditKey::Left, &screen, 2, at);
            assert_eq!(left == EditOutcome::Armed, !rule_on, "← at lo");
        }
    }

    #[test]
    fn editing_off_routes_edit_keys_exactly_as_before() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let row = seen_row(&screen);
        let mut q = Predictor::new(PredictMode::Adaptive);
        let _ = type_after(&mut q, "$ ", "ab", t0());
        p.set_editing(false);
        q.flush();
        assert!(p.predict_glyph('c', &sample(&row, 4), at));
        assert!(q.predict_glyph('c', &sample(&row, 4), at));
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 4, at),
            EditOutcome::Cancelled
        );
        assert!(q.predict_backspace(at));
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 4, at),
            EditOutcome::Declined
        );
        q.note_unmodelled_input(at);
        let state = |p: &Predictor| {
            (
                p.preds.len(),
                p.owed_at,
                p.epoch,
                p.confirmed_epoch,
                p.glyph_owes.len(),
            )
        };
        assert_eq!(state(&p), state(&q));
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 4, at),
            EditOutcome::Declined
        );
        assert!(!q.predict_backspace(at));
        assert_eq!(state(&p), state(&q));
    }

    #[test]
    fn a_glyph_typed_inside_the_text_is_not_an_append() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        let mut at = at;
        for cursor in [5, 4] {
            let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
            at += RTT;
            let _ = frame(&mut p, &screen, cursor - 1, at);
        }
        assert!(p.idle() && p.confirmed_epoch);
        let row = seen_row(&screen);
        assert!(p.predict_glyph('x', &sample(&row, 3), at));
        assert!(p.engaged(), "a shift: the first op of a chain");
        assert_eq!(frame(&mut p, &screen, 3, at), (vec![], None), "dark");
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 5, 3)), "still known");
        let at = at + RTT;
        let _ = frame(&mut p, "$ axbc", 4, at);
        assert!(p.idle(), "its echo, the tail shifted, retires it");
        assert_eq!(p.events_for_test().shifts_proven, 1);
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 6, 4)));
        let row = seen_row("$ axbc");
        assert!(p.predict_glyph('y', &sample(&row, 4), at));
        let _ = frame(&mut p, "$ axbc", 4, at);
        let ghosts: Vec<(u16, char, GhostCell)> = p
            .overlay(at)
            .iter()
            .map(|g| (g.col, g.ch, g.ghost()))
            .collect();
        assert_eq!(
            ghosts,
            [
                (4, 'y', GhostCell::Typed),
                (5, 'b', GhostCell::Moved { from: 4 }),
                (6, 'c', GhostCell::Moved { from: 5 })
            ],
            "the next shows at once, `bc` moved"
        );
        assert_eq!(p.overlay_cursor(), Some((0, 5)));

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        let mut row = seen_row(&screen);
        row[4] = Seen::Opaque(None);
        let refusals = p.events_for_test().opaque_refusals;
        assert!(p.predict_glyph('x', &sample(&row, 3), at));
        assert!(!p.engaged());
        assert_eq!(p.events_for_test().opaque_refusals, refusals + 1);
        assert!(p.overlay(at).is_empty(), "not painted over `b`");
        assert_eq!(p.edit_line_for_test(), None);
        let _ = frame(&mut p, "$ axb", 4, at + RTT);
        assert!(p.idle(), "its echo retires it");
        assert!(
            !p.confirmed_epoch,
            "a run typed inside the text proves nothing"
        );
    }

    #[test]
    fn an_opaque_cell_on_the_chain_s_line_flushes_it() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let mut plain = seen_cells(&screen);
        p.reconcile_seen(Some((0, 3)), G, false, at + RTT, |r, c| {
            if (r, c) == (0, 4) {
                Seen::Opaque(Some('日'))
            } else {
                plain(r, c)
            }
        });
        assert!(p.idle() && !p.confirmed_epoch, "flushed");

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let mut row = seen_row(&screen);
        row[4] = Seen::Opaque(None);
        assert_eq!(
            p.predict_edit(EditKey::Left, &sample(&row, 4), at),
            EditOutcome::Declined
        );
    }

    #[test]
    fn an_erase_beside_text_past_the_end_is_not_armed() {
        for rule_on in [true, false] {
            let (mut p, _, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::VacateRefusal);
            }
            let refusals = p.events_for_test().vacate_refusals;
            let out = press(&mut p, EditKey::Backspace, "$ abcd", 4, at);
            assert_eq!(out == EditOutcome::Armed, !rule_on, "rule on {rule_on}");
            assert_eq!(
                p.events_for_test().vacate_refusals,
                refusals + u64::from(rule_on)
            );
        }
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let _ = press(&mut p, EditKey::Right, &screen, 4, at);
        let _ = press(&mut p, EditKey::Backspace, &screen, 4, at);
        p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ abcd"));
        assert!(p.idle(), "a suggestion appeared beside a pending erase");
    }

    #[test]
    fn an_expired_chain_head_takes_the_chain_with_it() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::StateExpiryFlushes);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let _ = press(&mut p, EditKey::Right, &screen, 4, at + MS(100));
            let late = at + p.glitch_window() + MS(1);
            let _ = p.overlay(late);
            assert_eq!(p.idle(), rule_on, "rule on {rule_on}");
            if rule_on {
                assert_eq!(p.owed_at, Some(at + MS(100)));
                assert!(!p.confirmed_epoch);
            }
        }
    }

    #[test]
    fn a_chain_carried_across_enter_keeps_its_cursor() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::CarryCursor);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 3, at);
            let _ = press(&mut p, EditKey::Left, &screen, 3, at);
            assert_eq!(frame(&mut p, &screen, 3, at).1, Some((0, 2)));
            p.note_line_submit(at);
            let carried = frame(&mut p, &screen, 3, at).1;
            assert_eq!(carried, rule_on.then_some((0, 2)), "rule on {rule_on}");
        }
    }

    #[test]
    fn a_motion_proof_does_not_outlive_its_line() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::ProofsEndWithTheEpoch);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 3, at);
            assert_eq!(p.events_for_test().motions_proven, 1);
            p.note_line_submit(at);
            let (screen, at) = type_after(&mut p, "", "cd", at + MS(300));
            assert!(p.confirmed_epoch, "the new line proves");
            assert_eq!(
                press(&mut p, EditKey::Left, &screen, 2, at),
                EditOutcome::Armed
            );
            let shown = frame(&mut p, &screen, 2, at).1;
            assert_eq!(shown.is_some(), !rule_on, "rule on {rule_on}");
        }
    }

    #[test]
    fn a_chain_matches_whole_states_not_its_cursor() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::WholeStateMatch);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ xy"));
            assert!(p.idle());
            let line = p.edit_line_for_test();
            assert_eq!(line.is_some(), !rule_on, "rule on {rule_on}: {line:?}");
        }
    }

    #[test]
    fn a_motion_echo_never_proves_its_epoch() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let mut screen = String::from("$       <r>");
            let mut at = t0();
            for (i, ch) in ['a', 'b'].into_iter().enumerate() {
                let col = 2 + u16::try_from(i).unwrap_or(0);
                let row = seen_row(&screen);
                assert!(p.predict_glyph(ch, &sample(&row, col), at));
                at += RTT;
                screen.replace_range(usize::from(col)..=usize::from(col), &ch.to_string());
                p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&screen));
            }
            assert!(
                p.confirmed_epoch && !p.proof_line_end,
                "fixture: a decorated proof"
            );
            if !rule_on {
                p.disable_rule_for_test(Rule::MotionsNeverProve);
            }
            assert_eq!(
                press(&mut p, EditKey::Left, &screen, 4, at),
                EditOutcome::Armed
            );
            p.reconcile_seen(Some((0, 4)), G, false, at, seen_cells("$ ab"));
            assert!(
                p.confirmed_epoch && p.preds.len() == 1,
                "a right prompt cleared beside the line moves nothing"
            );
            p.void_proof();
            assert!(
                !p.confirmed_epoch && p.preds.len() == 1,
                "voided, ← in flight"
            );
            p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ ab"));
            assert!(p.idle());
            assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
        }
    }

    #[test]
    fn an_edit_op_dropped_is_owed() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::EditOpsOwe);
            }
            let before = p.owed_at;
            let _ = press(&mut p, EditKey::Right, &screen, 4, at + MS(5));
            assert_eq!(p.owed_at == Some(at + MS(5)), rule_on, "the declined →");
            assert_eq!(p.owed_at == before, !rule_on);

            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::EditOpsOwe);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at + MS(5));
            p.reconcile_seen(Some((1, 0)), G, false, at + MS(10), seen_cells(&screen));
            assert!(p.idle(), "the cursor left the line: flushed");
            assert_eq!(p.owed_at == Some(at + MS(5)), rule_on, "the flushed ←");
        }
    }

    #[test]
    fn pending_bounds_cover_the_chain_s_line() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        let _ = press(&mut p, EditKey::Backspace, &screen, 5, at);
        assert_eq!(p.pending_bounds(), Some((0, 2, 1, 6)));
    }

    #[test]
    fn a_chain_is_capped() {
        let (mut p, screen, at) = edited_line(PredictMode::Always, "ab");
        let mut armed = 0;
        for i in 0..EDIT_CHAIN_MAX + 2 {
            let key = if i % 2 == 0 {
                EditKey::Left
            } else {
                EditKey::Right
            };
            if press(&mut p, key, &screen, 4, at) == EditOutcome::Armed {
                armed += 1;
            }
        }
        assert_eq!(armed, EDIT_CHAIN_MAX);
        assert!(p.anchor_lost, "the key past the cap was declined");
    }

    #[test]
    fn the_known_line_lives_with_the_proof_it_came_from() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let (screen, at) = type_after(&mut p, "$ ", "a", t0());
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 3, 3)), "started");
        let (screen, at) = type_after(&mut p, &screen, "bcdefgh", at);
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 10, 10)), "extended");
        let (screen, at) = type_after(&mut p, &screen, "i", at);
        assert_eq!(
            p.edit_line_for_test(),
            Some((0, 2, 11, 11)),
            "up to column 10"
        );
        let _ = type_after(&mut p, &screen, "j", at);
        assert_eq!(p.edit_line_for_test(), None, "a glyph in the last column");

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let row = seen_row(&screen);
        assert!(
            p.predict_glyph('c', &sample(&row, 7), at),
            "the cursor moved"
        );
        assert_eq!(p.edit_line_for_test(), None);

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        p.note_unmodelled_input(at);
        assert_eq!(p.edit_line_for_test(), None, "a flush");

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        p.note_line_submit(at);
        assert_eq!(
            p.edit_line_for_test(),
            None,
            "a terminator, nothing carried"
        );
        let _ = screen;
    }

    #[test]
    fn every_key_is_held_until_its_echo_is_credited() {
        let (mut p, screen, mut at) = edited_line(PredictMode::Adaptive, "ab");
        let mut presses: Vec<(u64, Instant)> = Vec::new();
        let check = |p: &mut Predictor, presses: &mut Vec<(u64, Instant)>, label: &str| {
            let credited = p.take_credited_for_test();
            presses.retain(|(seq, _)| !credited.contains(seq));
            for &(seq, t) in presses.iter() {
                let held = p.preds.iter().any(|g| g.seq == seq)
                    || p.glyph_owes.iter().any(|&(s, _, _)| s == seq)
                    || p.owed_at.is_some_and(|o| o >= t);
                assert!(held, "{label}: key {seq} typed at {t:?} is held by nothing");
            }
        };
        let _ = p.take_credited_for_test();
        let key = |p: &mut Predictor,
                   k: EditKey,
                   cursor: u16,
                   at: Instant,
                   presses: &mut Vec<(u64, Instant)>| {
            let seq = p.next_seq;
            let _ = press(p, k, &screen, cursor, at);
            presses.push((seq, at));
        };
        key(&mut p, EditKey::Left, 4, at, &mut presses);
        key(&mut p, EditKey::Right, 4, at + MS(1), &mut presses);
        check(&mut p, &mut presses, "a chain armed");
        at += MS(5);
        p.reconcile_seen(Some((0, 4)), G, false, at, seen_cells(&screen));
        check(&mut p, &mut presses, "a net-null burst");
        let row = seen_row(&screen);
        let seq = p.next_seq;
        assert!(p.predict_glyph('c', &sample(&row, 4), at));
        presses.push((seq, at));
        assert!(p.predict_backspace(at + MS(1)));
        check(&mut p, &mut presses, "a cancelled glyph");
        key(&mut p, EditKey::Right, 4, at + MS(2), &mut presses);
        check(&mut p, &mut presses, "a declined arrow");
        at += MS(5);
        p.reconcile_seen(Some((3, 0)), G, false, at, seen_cells(&screen));
        check(&mut p, &mut presses, "a divergence");
    }

    #[test]
    fn a_stale_anchor_is_not_credited_by_the_next_key_s_echo() {
        for rule_on in [true, false] {
            let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::StaleAnchor);
            }
            assert!(!p.predict_backspace(at), "nothing of ours: owed");
            let typed = at + MS(140);
            assert!(p.predict_char('c', (0, 2), G, 12, typed));
            assert!(p.predict_char('c', (0, 2), G, 12, typed));
            assert!(p.preds.iter().all(|g| g.stale), "a stale anchor");
            let _ = p.take_credited_for_test();
            let landed = at + MS(300);
            let screen = [((0, 0), 'a'), ((0, 1), 'c'), ((0, 2), 'c')];
            p.reconcile(Some((0, 3)), G, false, landed, cell(&screen));
            let credited = p.take_credited_for_test();
            assert_eq!(
                credited.is_empty(),
                rule_on,
                "rule on {rule_on}: {credited:?}"
            );
            assert_eq!(p.confirmed_epoch, !rule_on);
        }
    }

    #[test]
    fn an_edit_chain_does_not_saturate_its_last_column_guard() {
        fn append_after_a_motion_pair(cols: u16, first: u16) -> (bool, Option<(u16, u16)>) {
            let mut predictor = Predictor::new(PredictMode::Always);
            let born = t0();
            let mut row = vec![Seen::Blank; usize::from(cols)];
            fn key(cells: &[Seen], col: u16, cols: u16) -> KeySample<'_> {
                KeySample {
                    cursor: (0, col),
                    coords: G,
                    grid: (cols, 2),
                    row: Some(cells),
                    ..KeySample::default()
                }
            }
            assert!(predictor.predict_glyph('a', &key(&row, first, cols), born));
            row[usize::from(first)] = Seen::Glyph('a');
            let now = born + RTT;
            let cursor = first + 1;
            predictor.reconcile_seen(Some((0, cursor)), G, false, now, |_, col| {
                row.get(usize::from(col)).copied().unwrap_or(Seen::Blank)
            });
            assert_eq!(
                predictor.edit_line_for_test(),
                Some((0, first, cursor, cursor))
            );
            let sampled = key(&row, cursor, cols);
            assert_eq!(
                predictor.predict_edit(EditKey::Left, &sampled, now),
                EditOutcome::Armed
            );
            assert_eq!(
                predictor.predict_edit(EditKey::Right, &sampled, now),
                EditOutcome::Armed
            );
            let armed = predictor.predict_glyph('b', &sampled, now);
            let _ = predictor.overlay(now);
            (armed, predictor.overlay_cursor())
        }

        assert_eq!(append_after_a_motion_pair(12, 2), (true, Some((0, 4))));
        for cols in [12, u16::MAX - 1] {
            let (armed, cursor) = append_after_a_motion_pair(cols, cols - 2);
            assert!(!armed, "ordinary last-column refusal at width {cols}");
            assert!(cursor.is_none_or(|(_, col)| col < cols));
        }
        let widest = append_after_a_motion_pair(u16::MAX, u16::MAX - 2);
        assert!(
            !widest.0 && widest.1.is_none_or(|(_, col)| col < u16::MAX),
            "EDIT_CHAIN_LAST_COLUMN_SATURATED: {widest:?}"
        );
    }

    #[test]
    fn glitch_window_ewma_and_ghost_wire_codec_round_trip() {
        assert_eq!(compute_glitch_window_ms(None, 0), 250);
        assert_eq!(compute_glitch_window_ms(None, 2), 1000);
        assert_eq!(compute_glitch_window_ms(None, 9), 2000);
        assert_eq!(compute_glitch_window_ms(Some(120_000), 0), 360);
        assert_eq!(compute_glitch_window_ms(Some(900_000), 0), 2000);

        let srtt0 = ewma_srtt_step_us(None, 100_000);
        assert_eq!(srtt0, 100_000);
        let srtt1 = ewma_srtt_step_us(Some(srtt0), 200_000);
        assert_eq!(srtt1, 130_000);

        let wire = encode_predicted_ghost_wire_le(4, 19, 'λ', 42);
        assert_eq!(
            decode_predicted_ghost_wire_le(&wire),
            Some((4, 19, 'λ', 42))
        );
    }

    #[test]
    fn a_chain_composed_before_any_reconcile_paints_only_what_it_changes() {
        let (mut p, screen, at) = edited_line(PredictMode::Always, "abc");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 5, at),
            EditOutcome::Armed
        );
        assert!(p.overlay(at).is_empty(), "a motion changes no cell");
        assert_eq!(p.overlay_cursor(), Some((0, 4)));
        let (mut p, screen, at) = edited_line(PredictMode::Always, "abc");
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 5, at),
            EditOutcome::Armed
        );
        let cells: Vec<(u16, char)> = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
        assert_eq!(cells, [(4, ' ')], "only the vacated cell");
    }

    #[test]
    fn overlay_frame_is_the_overlay_and_its_cursor() {
        let (mut p, screen, at) = edited_line(PredictMode::Always, "ab");
        let row = seen_row(&screen);
        assert!(p.predict_glyph('c', &sample(&row, 4), at));
        let glyph_lane: Vec<(u16, char)> = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
        let (cells, cursor) = p.overlay_frame(at);
        let framed: Vec<(u16, char)> = cells.iter().map(|g| (g.col, g.ch)).collect();
        assert_eq!((framed, cursor), (glyph_lane, None));
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 4, at),
            EditOutcome::Armed
        );
        let chain: Vec<(u16, char)> = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
        let chain_cursor = p.overlay_cursor();
        let (cells, cursor) = p.overlay_frame(at);
        let framed: Vec<(u16, char)> = cells.iter().map(|g| (g.col, g.ch)).collect();
        assert_eq!((framed, cursor), (chain, chain_cursor));
        assert_eq!(cursor, Some((0, 4)), "the chain's cursor on `c`");
    }

    #[test]
    fn newest_op_and_shown_through_name_the_pressed_and_the_displayed_ops() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let row = seen_row(&screen);
        assert_eq!((p.newest_op(), p.shown_through()), (None, None));
        assert!(p.predict_glyph('c', &sample(&row, 4), at));
        let c = p.newest_op().expect("`c` armed an op");
        assert_eq!(p.armed_since(None), Some(c));
        let _ = p.overlay(at);
        assert_eq!(p.shown_through(), Some(c), "the proven line shows `c`");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 5, at),
            EditOutcome::Armed
        );
        let left = p.newest_op().expect("`←` armed an op");
        assert!(left > c, "ops are numbered in press order");
        assert_eq!(p.armed_since(Some(c)), Some(left));
        let _ = p.overlay(at);
        assert_eq!(
            p.shown_through(),
            Some(c),
            "the line's first `←` is dark until a motion of it echoes (THE CLASS LADDER)"
        );
        let (mut q, screen, at) = edited_line(PredictMode::Always, "ab");
        let row = seen_row(&screen);
        assert!(q.predict_glyph('c', &sample(&row, 4), at));
        assert!(q.predict_glyph('d', &sample(&row, 5), at));
        let before = q.newest_op();
        assert_eq!(
            press(&mut q, EditKey::Backspace, &screen, 6, at),
            EditOutcome::Cancelled
        );
        assert!(
            q.newest_op() < before,
            "a cancel pops `d` and arms nothing newer"
        );
        assert_eq!(
            q.armed_since(before),
            None,
            "the cancel armed nothing (naming the newest op would hand `c`'s op to it)"
        );
    }

    #[test]
    fn a_flush_takes_the_chain_s_cursor_with_it() {
        let (mut p, screen, at) = edited_line(PredictMode::Always, "abc");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 5, at),
            EditOutcome::Armed
        );
        let _ = p.overlay(at);
        assert_eq!(
            p.overlay_cursor(),
            Some((0, 4)),
            "control: the chain's cursor"
        );
        p.reset();
        assert_eq!(p.overlay_cursor(), None);
        assert!(p.idle());
    }

    fn decorated_line(text: &str) -> (Predictor, String, Instant) {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut screen = String::from("$       <r>");
        let mut at = t0();
        for (i, ch) in text.chars().enumerate() {
            let col = 2 + u16::try_from(i).unwrap_or(0);
            let row = seen_row(&screen);
            assert!(p.predict_glyph(ch, &sample(&row, col), at));
            at += RTT;
            screen.replace_range(usize::from(col)..=usize::from(col), &ch.to_string());
            p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&screen));
        }
        assert!(
            p.adaptive_slow && p.confirmed_epoch && !p.proof_line_end,
            "fixture: a slow line proven beside its right prompt"
        );
        (p, screen, at)
    }

    #[test]
    fn an_erase_inside_the_text_shifts_its_tail() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
            if !rule_on {
                p.disable_rule_for_test(Rule::EraseShiftProof);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 5, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 4, at);
            assert!(p.idle() && p.confirmed_epoch, "`←` echoed: inside the text");
            assert_eq!(
                press(&mut p, EditKey::Backspace, &screen, 4, at),
                EditOutcome::Armed
            );
            let shown = frame(&mut p, &screen, 4, at);
            if !rule_on {
                assert_eq!(shown, (vec![(3, 'c'), (4, ' ')], Some((0, 3))), "rule off");
                continue;
            }
            assert_eq!(shown, (vec![], None), "dark: no mid-line erase has echoed");
            let at = at + RTT;
            let _ = frame(&mut p, "$ ac", 3, at);
            assert!(p.idle(), "its echo retires it");
            assert_eq!(p.events_for_test().shifts_proven, 1);
            assert_eq!(p.edit_line_for_test(), Some((0, 2, 4, 3)));
            assert_eq!(
                press(&mut p, EditKey::Backspace, "$ ac", 3, at),
                EditOutcome::Armed
            );
            let _ = frame(&mut p, "$ ac", 3, at);
            let ghosts: Vec<(u16, char, GhostCell)> = p
                .overlay(at)
                .iter()
                .map(|g| (g.col, g.ch, g.ghost()))
                .collect();
            assert_eq!(
                ghosts,
                [
                    (2, 'c', GhostCell::Moved { from: 3 }),
                    (3, ' ', GhostCell::Vacated)
                ],
                "the next shows at once"
            );
            assert_eq!(p.overlay_cursor(), Some((0, 2)));
        }
    }

    #[test]
    fn a_first_shift_stays_dark_until_its_echo_proves_the_class() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::InsertShiftProof);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 3, at);
            let row = seen_row(&screen);
            assert!(p.predict_glyph('x', &sample(&row, 3), at));
            let shown = frame(&mut p, &screen, 3, at);
            let want = if rule_on {
                (vec![], None)
            } else {
                (vec![(3, 'x'), (4, 'b')], Some((0, 4)))
            };
            assert_eq!(shown, want, "rule on {rule_on}");
        }
    }

    #[test]
    fn a_shift_of_spaces_proves_nothing() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "a  ");
            if !rule_on {
                p.disable_rule_for_test(Rule::ShiftProofDistinguishes);
            }
            let mut at = at;
            for cursor in [5, 4] {
                let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
                at += RTT;
                let _ = frame(&mut p, &screen, cursor - 1, at);
                assert!(p.idle());
            }
            let row = seen_row(&screen);
            assert!(p.predict_glyph(' ', &sample(&row, 3), at));
            at += RTT;
            let _ = p.take_credited_for_test();
            let _ = frame(&mut p, &screen, 4, at);
            assert!(p.idle(), "the screen shows its state");
            assert_eq!(p.take_credited_for_test().len(), 1, "credited");
            assert_eq!(
                p.events_for_test().shifts_proven,
                u64::from(!rule_on),
                "rule on {rule_on}"
            );
            assert_eq!(p.edit_line_for_test().is_some(), !rule_on);
            assert_eq!(p.confirmed_epoch, !rule_on);
            assert_eq!(p.events_for_test().ambiguous_shifts, u64::from(rule_on));
        }
    }

    #[test]
    fn a_lone_glyph_moved_past_the_line_proves_nothing() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::ShiftProofDistinguishes);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 3, at);
            let row = seen_row(&screen);
            assert!(p.predict_glyph('x', &sample(&row, 3), at));
            let _ = frame(&mut p, "$ axb", 4, at + RTT);
            assert!(p.idle(), "rule on {rule_on}: retired");
            assert_eq!(p.events_for_test().shifts_proven, u64::from(!rule_on));
            assert_eq!(p.events_for_test().ambiguous_shifts, u64::from(rule_on));
            assert_eq!(
                p.edit_line_for_test(),
                (!rule_on).then_some((0, 2, 5, 4)),
                "rule on {rule_on}"
            );
        }

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        for (key, cursor) in [(EditKey::Backspace, 5), (EditKey::Left, 4)] {
            assert_eq!(press(&mut p, key, &screen, cursor, at), EditOutcome::Armed);
        }
        let row = seen_row(&screen);
        assert!(p.predict_glyph('x', &sample(&row, 5), at));
        let _ = frame(&mut p, "$ axb", 4, at + RTT);
        assert!(p.idle(), "the chain's last state, matched whole");
        assert_eq!(p.events_for_test().shifts_proven, 1);
        assert_eq!(p.events_for_test().ambiguous_shifts, 0);
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 5, 4)));
    }

    #[test]
    fn a_glyph_the_chain_cannot_place_inside_the_text_flushes_it() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Always, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::MidLineDeclineFlushes);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let typed = at + MS(5);
            assert!(!p.predict_char_in_grid('日', (0, 4), G, (12, 6), typed));
            assert_eq!(p.idle(), rule_on, "rule on {rule_on}");
            assert_eq!(p.owed_at, Some(typed));
            assert_eq!(p.events_for_test().mid_line_flushes, u64::from(rule_on));
        }
    }

    #[test]
    fn a_shift_into_the_last_column_is_not_placed() {
        let (mut p, screen, at) = edited_line(PredictMode::Always, "abcdefghi");
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 11, 11)));
        let _ = press(&mut p, EditKey::Left, &screen, 11, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 10, at);
        let row = seen_row(&screen);
        assert!(p.predict_glyph('x', &sample(&row, 10), at));
        assert!(!p.engaged(), "the glyph lane's");
        assert_eq!(p.edit_line_for_test(), None);
    }

    #[test]
    fn a_chain_beside_a_right_prompt_holds_the_row_past_it() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = decorated_line("ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::StandingTail);
            }
            assert_eq!(
                press(&mut p, EditKey::Left, &screen, 4, at),
                EditOutcome::Armed
            );
            p.reconcile_seen(Some((0, 4)), G, false, at, seen_cells("$ ab"));
            assert!(!p.idle(), "a right prompt cleared moves nothing");
            p.reconcile_seen(Some((0, 4)), G, false, at, seen_cells("$ ab cd"));
            assert_eq!(p.idle(), rule_on, "rule on {rule_on}");
        }
    }

    #[test]
    fn a_credited_shift_re_bases_the_standing_proof() {
        let (mut p, screen, at) = decorated_line("abc");
        let mut at = at;
        for cursor in [5, 4] {
            let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
            at += RTT;
            let _ = frame(&mut p, &screen, cursor - 1, at);
        }
        assert!(p.idle() && p.confirmed_epoch);
        let row = seen_row(&screen);
        assert!(p.predict_glyph('x', &sample(&row, 3), at));
        let at = at + RTT;
        let _ = frame(&mut p, "$ axbc  <r>", 4, at);
        assert!(p.idle() && p.confirmed_epoch, "credited, proven");
        let _ = frame(&mut p, "$ axbc  <r>", 4, at);
        assert!(
            p.confirmed_epoch,
            "re-based: the shifted row is the snapshot"
        );
        assert_eq!(p.events_for_test().standing_voids, 0);
        let _ = frame(&mut p, "$ axbc z<r>", 4, at);
        assert!(!p.confirmed_epoch, "a glyph past the line voids it");
    }

    #[test]
    fn an_edit_press_holds_the_standing_proof_to_its_row() {
        for rule_on in [true, false] {
            let (mut p, _, at) = decorated_line("ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::StandingTail);
            }
            let out = press(&mut p, EditKey::Left, "$ ab cd <r>", 4, at);
            assert_eq!(out == EditOutcome::Armed, !rule_on, "rule on {rule_on}");
        }
    }

    #[test]
    fn a_burst_overwrite_mode_also_shows_proves_no_shift() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
            if !rule_on {
                p.disable_rule_for_test(Rule::ShiftProofDistinguishes);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 5, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 4, at);
            assert!(p.idle() && p.events_for_test().motions_proven == 1);
            assert_eq!(
                press(&mut p, EditKey::Backspace, &screen, 4, at),
                EditOutcome::Armed
            );
            let row = seen_row(&screen);
            assert!(p.predict_glyph('o', &sample(&row, 4), at));
            let at = at + RTT;
            assert_eq!(
                frame(&mut p, "$ a c", 3, at),
                (vec![], None),
                "the rubout's echo: no state, the chain waits, dark"
            );
            assert!(!p.idle());
            let at = at + MS(5);
            let _ = frame(&mut p, "$ aoc", 4, at);
            assert!(p.idle(), "`S_2`, the screen of both");
            assert_eq!(
                p.events_for_test().shifts_proven,
                if rule_on { 0 } else { 2 },
                "rule on {rule_on}"
            );
            assert_eq!(
                p.edit_line_for_test(),
                Some((0, 2, 5, 4)),
                "the line stands"
            );
            let row = seen_row("$ aoc");
            assert!(p.predict_glyph('n', &sample(&row, 4), at));
            let shown = frame(&mut p, "$ aoc", 4, at);
            assert_eq!(shown.0.is_empty(), rule_on, "rule on {rule_on}: {shown:?}");
        }
    }

    #[test]
    fn a_screen_overwrite_mode_shows_with_fewer_keys_credits_none() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::InPlaceReadsFewer);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 3, at);
            assert!(p.idle());
            let row = seen_row(&screen);
            assert!(p.predict_glyph('x', &sample(&row, 3), at));
            assert_eq!(
                press(&mut p, EditKey::Right, &screen, 3, at),
                EditOutcome::Armed
            );
            assert_eq!(
                press(&mut p, EditKey::Backspace, &screen, 3, at),
                EditOutcome::Armed
            );
            let _ = p.take_credited_for_test();
            let _ = frame(&mut p, "$ ax", 4, at + RTT);
            let credited = p.take_credited_for_test();
            assert!(p.idle());
            assert_eq!(
                credited.len(),
                if rule_on { 0 } else { 3 },
                "rule on {rule_on}: {credited:?}"
            );
        }
    }

    #[test]
    fn a_backspace_cancels_no_glyph_of_an_unproven_shift() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::CancelsOnlyKnownShifts);
                p.disable_rule_for_test(Rule::TransitBlanks);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 4, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 3, at);
            let row = seen_row(&screen);
            assert!(p.predict_glyph('n', &sample(&row, 3), at));
            let out = press(&mut p, EditKey::Backspace, &screen, 3, at);
            let want = if rule_on {
                EditOutcome::Armed
            } else {
                EditOutcome::Cancelled
            };
            assert_eq!(out, want, "rule on {rule_on}");
            assert_eq!(
                press(&mut p, EditKey::Left, &screen, 3, at),
                EditOutcome::Armed
            );
            let shown = frame(&mut p, "$ a", 3, at + RTT);
            assert_eq!(
                shown == (vec![], None),
                rule_on,
                "rule on {rule_on}: {shown:?}"
            );
        }
    }

    #[test]
    fn an_arrow_with_a_run_in_flight_holds_the_standing_tail() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let (screen, at) = type_after(&mut p, "$ ", "ab cd", t0());
            assert!(p.adaptive_slow && p.confirmed_epoch, "fixture");
            if !rule_on {
                p.disable_rule_for_test(Rule::StandingTail);
            }
            p.note_unmodelled_input(at);
            let at = at + MS(600);
            let _ = frame(&mut p, &screen, 4, at);
            let row = seen_row(&screen);
            assert!(p.predict_glyph('e', &sample(&row, 4), at));
            assert!(p.predict_glyph('c', &sample(&row, 4), at));
            let at = at + RTT;
            let _ = frame(&mut p, "$ abecd", 5, at);
            assert!(
                p.confirmed_epoch,
                "fixture: `e` proved on the frame between"
            );
            let voids = p.events_for_test().standing_voids;
            let out = press(&mut p, EditKey::Left, "$ abe cd", 5, at);
            let shown = frame(&mut p, "$ abe cd", 5, at);
            if rule_on {
                assert_eq!(out, EditOutcome::Declined);
                assert_eq!(p.events_for_test().standing_voids, voids + 1);
                assert_eq!(shown, (vec![], None));
            } else {
                assert!(shown.0.contains(&(5, 'c')), "rule off: {shown:?}");
            }
        }
    }

    #[test]
    fn a_blank_row_behind_a_declined_arrow_is_no_line_end() {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let t = t0();
        let empty = seen_row("");
        assert!(p.predict_glyph('b', &sample(&empty, 0), t));
        assert_eq!(
            press(&mut p, EditKey::Left, "", 0, t + MS(10)),
            EditOutcome::Declined
        );
        assert!(!p.predict_glyph('d', &sample(&empty, 0), t + MS(20)));
        let at = t + MS(60);
        p.reconcile_seen(Some((0, 1)), G, false, at, seen_cells("b"));
        assert!(p.idle(), "`b` echoed");
        let row = seen_row("b");
        for ch in ['b', 'n', 's'] {
            assert!(p.predict_glyph(ch, &sample(&row, 1), at));
        }
        p.reconcile_seen(Some((0, 4)), G, false, at + MS(400), seen_cells("dbnsb"));
        assert!(p.idle(), "the run's echoes retire it");
        assert!(!p.confirmed_epoch, "a tail its press did not see: no proof");
        assert!(p.events_for_test().tail_refusals > 0);
    }

    #[test]
    fn an_echo_drawn_over_the_glyph_it_shifts_hides_its_tail() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::CleanTailInFlight);
            }
            let (screen, at) = type_after(&mut p, "", "xi", t0());
            assert!(
                p.adaptive_slow && p.confirmed_epoch,
                "fixture: a slow, proven line"
            );
            let step_back = at;
            p.note_unmodelled_input(step_back);
            let at = step_back + RTT;
            let _ = frame(&mut p, &screen, 1, at);
            assert!(p.predict_glyph('i', &sample(&seen_row(&screen), 1), at));
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 2, at);
            assert!(p.idle(), "the glyph's echo retires it");
            assert!(!p.confirmed_epoch, "uncredited: Ctrl-B's echo is owed");
            assert_eq!(
                p.hidden_tail_at.is_some(),
                rule_on,
                "rule on {rule_on}: typed over its own glyph"
            );
            let at = step_back + p.glitch_window_for_test() + MS(1);
            assert!(p.predict_glyph('a', &sample(&seen_row(&screen), 2), at));
            assert_eq!(
                p.tail_line_end, !rule_on,
                "rule on {rule_on}: the blank row is the line's end"
            );
            let at = at + RTT;
            let _ = frame(&mut p, "xiai", 3, at);
            assert!(p.idle(), "`a`'s echo retires it");
            assert_eq!(
                p.confirmed_epoch, !rule_on,
                "rule on {rule_on}: `a` proves beside the shifted `i`"
            );
            assert!(p.predict_glyph('d', &sample(&seen_row("xiai"), 3), at));
            let (shown, _) = frame(&mut p, "xiai", 3, at);
            assert_eq!(
                shown.is_empty(),
                rule_on,
                "rule on {rule_on}: `d` over the `i`: {shown:?}"
            );
        }
    }

    #[test]
    fn a_run_started_inside_the_text_proves_nothing() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abcdefg i");
            if !rule_on {
                p.disable_rule_for_test(Rule::InsideRunsNeverProve);
                p.disable_rule_for_test(Rule::DarkOverText);
            }
            let mut at = at;
            for cursor in [11, 10] {
                let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
                at += RTT;
                let _ = frame(&mut p, &screen, cursor - 1, at);
                assert!(p.idle());
            }
            let row = seen_row(&screen);
            assert!(p.predict_glyph('o', &sample(&row, 9), at));
            assert!(!p.engaged(), "R-ROW: the glyph lane's");
            let at = at + RTT;
            let _ = frame(&mut p, "$ abcdefgoi", 10, at);
            assert!(p.idle());
            assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
            let row = seen_row("$ abcdefgoi");
            assert!(p.predict_glyph(' ', &sample(&row, 10), at));
            assert_eq!(p.overlay(at).is_empty(), rule_on, "rule on {rule_on}");
        }
    }

    #[test]
    fn an_erase_before_a_tail_of_spaces_proves_nothing() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "cab ");
            if !rule_on {
                p.disable_rule_for_test(Rule::ShiftProofDistinguishes);
            }
            let _ = press(&mut p, EditKey::Left, &screen, 6, at);
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 5, at);
            assert!(p.idle());
            assert_eq!(
                press(&mut p, EditKey::Backspace, &screen, 5, at),
                EditOutcome::Armed
            );
            let _ = frame(&mut p, "$ ca  ", 4, at + RTT);
            assert!(p.idle(), "rule on {rule_on}: retired");
            let events = p.events_for_test();
            assert_eq!(events.shifts_proven, u64::from(!rule_on));
            assert_eq!(events.ambiguous_shifts, u64::from(rule_on));
            assert_eq!(
                p.edit_line_for_test(),
                (!rule_on).then_some((0, 2, 5, 4)),
                "rule on {rule_on}"
            );
        }
    }

    #[test]
    fn a_glyph_typed_inside_the_text_holds_the_standing_proof_to_its_row() {
        let (mut p, screen, at) = decorated_line("abc");
        let mut at = at;
        for cursor in [5, 4] {
            let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
            at += RTT;
            let _ = frame(&mut p, &screen, cursor - 1, at);
            assert!(p.idle());
        }
        assert!(p.confirmed_epoch, "fixture");
        let voids = p.events_for_test().standing_voids;
        let row = seen_row("$ abc z <r>");
        assert!(p.predict_glyph('x', &sample(&row, 3), at));
        assert!(!p.engaged(), "no chain on a tail that moved");
        assert_eq!(p.events_for_test().standing_voids, voids + 1);
        assert!(!p.confirmed_epoch);
    }

    #[test]
    fn a_right_to_left_row_takes_no_edit_key() {
        let (mut p, _, at) = edited_line(PredictMode::Adaptive, "ab");
        let refusals = p.events_for_test().bidi_refusals;
        assert_eq!(
            press(&mut p, EditKey::Left, "$ ab     \u{5d0}", 4, at),
            EditOutcome::Declined
        );
        assert_eq!(p.events_for_test().bidi_refusals, refusals + 1);

        let (mut p, screen, at) = edited_line(PredictMode::Always, "ab");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 4, at),
            EditOutcome::Armed
        );
        let refusals = p.events_for_test().bidi_refusals;
        assert!(!p.predict_char_in_grid('\u{5d0}', (0, 4), G, (12, 6), at));
        assert_eq!(p.events_for_test().bidi_refusals, refusals + 1);
    }

    #[test]
    fn a_proof_whose_echo_was_redrawn_stands_no_more() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::StandingGlyph);
            }
            let t = t0();
            let empty = seen_row("");
            assert!(p.predict_glyph('i', &sample(&empty, 0), t));
            assert!(p.predict_glyph(' ', &sample(&empty, 0), t));
            let at = t + RTT;
            p.reconcile_seen(Some((0, 1)), G, false, at, seen_cells("i"));
            assert!(p.confirmed_epoch, "fixture: `i` proved");
            let at = at + RTT;
            p.reconcile_seen(Some((0, 1)), G, false, at, seen_cells("$"));
            assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
            let row = seen_row("$");
            assert!(p.predict_glyph('8', &sample(&row, 1), at));
            let at = at + MS(5);
            p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("$ "));
            assert_eq!(p.overlay(at).is_empty(), rule_on, "rule on {rule_on}");
        }
    }

    #[test]
    fn a_cancelled_glyph_s_erase_is_no_space_s_echo() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::CancelOwesItsBlank);
            }
            let t = t0();
            let empty = seen_row("");
            assert!(p.predict_glyph('n', &sample(&empty, 0), t));
            assert!(p.predict_backspace(t + MS(5)));
            assert!(p.predict_glyph(' ', &sample(&empty, 0), t + MS(10)));
            let _ = p.take_credited_for_test();
            p.reconcile_seen(Some((0, 1)), G, false, t + MS(40), seen_cells(" "));
            assert!(p.idle(), "rule on {rule_on}: retired");
            let credited = p.take_credited_for_test();
            assert_eq!(
                credited.is_empty(),
                rule_on,
                "rule on {rule_on}: {credited:?}"
            );
        }
    }

    #[test]
    fn an_erase_caught_part_way_is_no_later_state() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::PartWayEchoes);
            }
            let (screen, at) = type_after(&mut p, "", "n", t0());
            assert_eq!(p.edit_line_for_test(), Some((0, 0, 1, 1)), "fixture");
            assert_eq!(
                press(&mut p, EditKey::Backspace, &screen, 1, at),
                EditOutcome::Armed
            );
            assert!(p.predict_glyph(' ', &sample(&seen_row(&screen), 1), at));
            let _ = p.take_credited_for_test();
            p.reconcile_seen(Some((0, 1)), G, false, at + RTT, seen_cells(" "));
            let credited = p.take_credited_for_test();
            assert_eq!(
                credited.len(),
                if rule_on { 0 } else { 2 },
                "rule on {rule_on}"
            );
            if rule_on {
                p.reconcile_seen(Some((0, 0)), G, false, at + RTT, seen_cells(" "));
                assert_eq!(p.take_credited_for_test().len(), 1, "the erase alone");
                assert_eq!(p.preds.len(), 1, "the space still pending");
            }
        }
    }

    fn two_rows<'t>(top: &'t str, under: &'t str) -> impl FnMut(u16, u16) -> Seen + 't {
        move |r, c| {
            let text = match r {
                0 => top,
                1 => under,
                _ => "",
            };
            text.chars()
                .nth(usize::from(c))
                .map_or(Seen::Blank, |ch| Seen::of_cell(ch, false))
        }
    }

    fn typed_over(p: &mut Predictor, text: &str, below: &str, at: Instant) -> Instant {
        let under = seen_row(below);
        let mut at = at;
        for (col, ch) in (0u16..).zip(text.chars()) {
            let before: String = text.chars().take(usize::from(col)).collect();
            let row = seen_row(&before);
            let key = KeySample {
                below: Some(&under),
                ..sample(&row, col)
            };
            assert!(p.predict_glyph(ch, &key, at));
            at += RTT;
            let after: String = text.chars().take(usize::from(col) + 1).collect();
            p.reconcile_seen(Some((0, col + 1)), G, false, at, two_rows(&after, below));
        }
        at
    }

    #[test]
    fn text_drawn_under_the_line_voids_its_proof() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::FloorBelow);
            }
            let at = typed_over(&mut p, "ab", "", t0());
            assert!(p.confirmed_epoch, "fixture: a proven line");
            p.reconcile_seen(Some((0, 2)), G, false, at, two_rows("ab", "Password: "));
            assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
            assert_eq!(p.events_for_test().floor_voids, u64::from(rule_on));
        }
        let mut p = Predictor::new(PredictMode::Adaptive);
        let at = typed_over(&mut p, "ab", "ls ion", t0());
        p.reconcile_seen(Some((0, 2)), G, false, at, two_rows("ab", "ls io"));
        assert!(p.confirmed_epoch, "control: text the press saw, redrawn");
    }

    #[test]
    fn text_over_a_wrapped_guess_s_cell_voids_its_proof() {
        for (under, rule_on, voids) in [("m", true, false), ("P", true, true), ("P", false, false)]
        {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::FloorBelow);
            }
            let line = "abcdefghij";
            let at = typed_over(&mut p, line, "", t0());
            assert!(p.confirmed_epoch, "fixture: a proven line");
            let row = seen_row(line);
            let blank = seen_row("");
            for ch in ['k', 'l', 'm'] {
                let key = KeySample {
                    below: Some(&blank),
                    ..sample(&row, end_of(line))
                };
                assert!(p.predict_glyph(ch, &key, at));
            }
            let wrapped = p.preds.iter().any(|g| (g.row, g.col, g.ch) == (1, 0, 'm'));
            assert!(wrapped, "fixture: `m` wraps onto the floor");
            p.reconcile_seen(Some((0, 10)), G, false, at + MS(5), two_rows(line, under));
            assert_eq!(
                p.confirmed_epoch, !voids,
                "{under:?} under, rule on {rule_on}"
            );
            assert_eq!(p.events_for_test().floor_voids, u64::from(voids));
        }
    }

    #[test]
    fn nothing_shows_behind_a_guess_held_on_its_own_glyph() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::DarkBehindAWait);
            }
            let at = typed_over(&mut p, "ab", "", t0());
            let row = seen_row("ab");
            assert!(p.predict_glyph('c', &sample(&row, 2), at));
            assert!(p.predict_glyph('d', &sample(&row, 2), at));
            let at = at + MS(5);
            p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("abc"));
            let shown: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
            let want: &[char] = if rule_on { &['c'] } else { &['c', 'd'] };
            assert_eq!(shown, want, "rule on {rule_on}");
            let at = at + RTT;
            p.reconcile_seen(Some((0, 3)), G, false, at, seen_cells("abc"));
            let shown: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
            assert_eq!(shown, ['d'], "rule on {rule_on}: `c` retired, `d` shows");
        }

        for arrows in [false, true] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let at = typed_over(&mut p, "ab", "", t0());
            assert!(p.confirmed_epoch, "fixture: an echo-proven line");
            let _ = p.take_credited_for_test();
            let row = seen_row("ab");
            assert!(p.predict_glyph('c', &sample(&row, 2), at));
            let held = at + MS(5);
            p.reconcile_seen(Some((0, 2)), G, false, held, seen_cells("abc"));
            assert!(p.held_wait.is_some(), "fixture: c is held, not credited");
            assert!(p.take_credited_for_test().is_empty());
            if arrows {
                let row = seen_row("abc");
                assert_eq!(
                    p.predict_edit(EditKey::Left, &sample(&row, 2), held + MS(1)),
                    EditOutcome::Declined
                );
                assert!(p.preds.is_empty(), "uncertain input is retired and owed");
                assert_eq!(
                    p.predict_edit(EditKey::Right, &sample(&row, 2), held + MS(2)),
                    EditOutcome::Declined
                );
                p.reconcile_seen(Some((0, 2)), G, false, held + MS(3), seen_cells("abc"));
                assert!(p.take_credited_for_test().is_empty());
            }
            let row = seen_row("abc");
            assert!(p.predict_glyph('x', &sample(&row, 2), held + MS(4)));
            assert!(
                p.overlay(held + MS(4)).iter().all(|g| g.ch != 'x'),
                "arrows {arrows}: nothing new shows on the unresolved head"
            );
        }
        let mut p = Predictor::new(PredictMode::Adaptive);
        let at = typed_over(&mut p, "ab", "", t0());
        let _ = p.take_credited_for_test();
        let row = seen_row("ab");
        assert!(p.predict_glyph('c', &sample(&row, 2), at));
        p.reconcile_seen(Some((0, 2)), G, false, at + MS(5), seen_cells("abc"));
        assert!(p.held_wait.is_some());
        let echoed = at + RTT;
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abc"));
        assert_eq!(p.take_credited_for_test().len(), 1, "c really echoed");
        assert!(p.held_wait.is_none());
        let row = seen_row("abc");
        assert_eq!(
            p.predict_edit(EditKey::Left, &sample(&row, 3), echoed + MS(1)),
            EditOutcome::Armed,
            "a resolved head does not refuse navigation"
        );
    }

    #[test]
    fn a_cursor_back_on_a_credited_cell_takes_the_credit_back() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::CreditTakenBack);
            }
            let t = t0();
            let empty = seen_row("");
            assert!(p.predict_glyph('a', &sample(&empty, 0), t));
            p.reconcile_seen(Some((0, 1)), G, false, t + MS(5), seen_cells("a"));
            assert_eq!(p.take_credited_for_test().len(), 1, "the part-way frame");
            p.reconcile_seen(Some((0, 0)), G, false, t + MS(6), seen_cells("ab"));
            assert_eq!(p.owed_at == Some(t), rule_on, "rule on {rule_on}");
            let row = seen_row("ab");
            assert!(p.predict_glyph('a', &sample(&row, 0), t + MS(7)));
            p.reconcile_seen(Some((0, 1)), G, false, t + MS(60), seen_cells("ab"));
            assert_eq!(
                p.take_credited_for_test().is_empty(),
                rule_on,
                "rule on {rule_on}: the first `a`'s echo credits the second"
            );
        }
    }

    #[test]
    fn a_cursor_on_text_in_the_last_column_is_not_parked() {
        for (rule_on, flag, want) in [
            (true, false, (0, 11)),
            (true, true, (1, 0)),
            (false, false, (1, 0)),
        ] {
            let mut p = Predictor::new(PredictMode::Always);
            if !rule_on {
                p.disable_rule_for_test(Rule::EngineWrapFlag);
            }
            let line = "$ abcdefghij";
            let row = seen_row(line);
            let _ = p.predict_edit(EditKey::Left, &sample(&row, 11), t0());
            p.reconcile_seen(Some((0, 11)), G, false, t0(), seen_cells(line));
            assert!(
                p.deferred_wrap,
                "fixture: a filled last column reads parked"
            );
            let row = seen_row(line);
            let key = KeySample {
                wrap_pending: Some(flag),
                ..sample(&row, 11)
            };
            assert!(p.predict_glyph('x', &key, t0()));
            let placed = p.preds.back().map(|g| (g.row, g.col));
            assert_eq!(placed, Some(want), "rule on {rule_on}, flag {flag}");
        }
    }

    fn context() -> EditorContext {
        EditorContext::new(1, Some(&[7; 32]), false, false)
    }

    const PROMPT: u64 = 7;

    const B: InputStart = InputStart {
        cell: (0, 2),
        prompt: PROMPT,
    };

    fn signed(row: &[Seen], cursor: u16) -> KeySample<'_> {
        KeySample {
            input_start: Some(B),
            context: context(),
            ..sample(row, cursor)
        }
    }

    fn submitted(p: &mut Predictor, at: Instant) -> Instant {
        p.note_line_submit_at(Some(PROMPT - 1), at);
        at + p.glitch_window_for_test() + MS(1)
    }

    fn type_signed(p: &mut Predictor, screen: &str, text: &str, at: Instant) -> (String, Instant) {
        let mut screen = screen.to_owned();
        let mut at = at;
        for ch in text.chars() {
            let col = end_of(&screen);
            let row = seen_row(&screen);
            assert!(p.predict_glyph(ch, &signed(&row, col), at));
            at += RTT;
            screen.push(ch);
            p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&screen));
        }
        (screen, at)
    }

    fn witnessed_line(mode: PredictMode, text: &str) -> (Predictor, String, Instant) {
        let mut p = Predictor::new(mode);
        let at = submitted(&mut p, t0());
        let (screen, at) = type_signed(&mut p, "$ ", text, at);
        assert!(
            p.line
                .as_ref()
                .is_some_and(|l| l.fresh == Some(PROMPT) && l.lo == 2),
            "fixture: a witnessed line"
        );
        (p, screen, at)
    }

    fn press_signed(
        p: &mut Predictor,
        key: EditKey,
        screen: &str,
        cursor: u16,
        at: Instant,
    ) -> EditOutcome {
        let row = seen_row(screen);
        p.predict_edit(key, &signed(&row, cursor), at)
    }

    fn echoed_edge(
        p: &mut Predictor,
        key: EditKey,
        (before, from): (&str, u16),
        (after, to): (&str, u16),
        at: Instant,
    ) -> Instant {
        assert_eq!(press_signed(p, key, before, from, at), EditOutcome::Armed);
        let at = at + RTT;
        let _ = frame(p, after, to, at);
        assert!(p.idle(), "{key:?}'s echo retires it");
        at
    }

    fn learn_home_and_end(p: &mut Predictor, screen: &str, end: u16, at: Instant) -> Instant {
        let mut at = at;
        for _ in 0..2 {
            at = echoed_edge(p, EditKey::Home, (screen, end), (screen, 2), at);
            at = echoed_edge(p, EditKey::End, (screen, 2), (screen, end), at);
        }
        at
    }

    #[test]
    fn an_unlearned_key_is_a_dark_probe_until_two_echoes_teach_it() {
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed
        );
        assert!(p.preds.iter().all(|g| g.probe), "armed as a probe");
        assert_eq!(frame(&mut p, &screen, 5, at), (vec![], None), "dark");
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 2, at);
        assert!(p.idle(), "its echo retires it");
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 5, 2)));
        assert_eq!(
            p.events_for_test().bindings_learned,
            0,
            "one echo is not two"
        );
        let at = echoed_edge(&mut p, EditKey::End, (&screen, 2), (&screen, 5), at);
        let at = echoed_edge(&mut p, EditKey::Home, (&screen, 5), (&screen, 2), at);
        let at = echoed_edge(&mut p, EditKey::End, (&screen, 2), (&screen, 5), at);
        assert_eq!(p.events_for_test().bindings_learned, 2, "Home and End");
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed
        );
        assert_eq!(
            frame(&mut p, &screen, 5, at),
            (vec![], Some((0, 2))),
            "learned: Home shows at once"
        );

        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        p.disable_rule_for_test(Rule::LearnedBindings);
        let _ = press_signed(&mut p, EditKey::Home, &screen, 5, at);
        assert_eq!(frame(&mut p, &screen, 5, at).1, Some((0, 2)), "rule off");
    }

    #[test]
    fn nothing_after_a_probe_is_shown_even_in_always() {
        let (mut p, screen, at) = witnessed_line(PredictMode::Always, "ab");
        assert!(p.predict_glyph('c', &signed(&seen_row(&screen), 4), at));
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 4, at),
            EditOutcome::Armed
        );
        assert!(p.predict_glyph('x', &signed(&seen_row(&screen), 4), at));
        assert_eq!(p.visible_ops_for_test(), 1, "`c` alone, before the probe");
        let shown = frame(&mut p, &screen, 4, at);
        assert_eq!(shown, (vec![(4, 'c')], Some((0, 5))), "the state after `c`");
    }

    #[test]
    fn a_learned_key_is_placed_only_on_a_witnessed_line() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Declined,
            "a line not started at the B"
        );
        assert!(p.idle() && !p.confirmed_epoch && p.owed_at == Some(at));

        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        assert_eq!(
            press(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Declined,
            "a press that hands no start"
        );
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let row = seen_row(&screen);
        let elsewhere = KeySample {
            input_start: Some(InputStart { cell: (0, 3), ..B }),
            ..signed(&row, 5)
        };
        assert_eq!(
            p.predict_edit(EditKey::Home, &elsewhere, at),
            EditOutcome::Declined,
            "a B the line was not started at"
        );
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let row = seen_row(&screen);
        let next_prompt = KeySample {
            input_start: Some(InputStart {
                prompt: PROMPT + 1,
                ..B
            }),
            ..signed(&row, 5)
        };
        assert_eq!(
            p.predict_edit(EditKey::Home, &next_prompt, at),
            EditOutcome::Declined,
            "another prompt's B at the same cell"
        );

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        p.disable_rule_for_test(Rule::WitnessFreshPrompt);
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed,
            "rule off: any B on the row"
        );
    }

    fn witness(p: &Predictor) -> Option<u64> {
        p.line.as_ref().and_then(|l| l.fresh)
    }

    #[test]
    fn only_the_first_key_of_a_prompt_starts_a_witnessed_line() {
        type Shut = fn(&mut Predictor, Instant);
        let cases: [(&str, Shut); 6] = [
            ("a key not modelled", |p, at| {
                p.note_line_submit_at(Some(PROMPT - 1), at);
                p.note_unmodelled_input(at);
            }),
            ("an Enter at the same prompt", |p, at| {
                p.note_line_submit_at(Some(PROMPT), at);
            }),
            ("an Enter at no prompt", |p, at| {
                p.note_line_submit_at(None, at);
            }),
            ("an Enter at a prompt not named", |p, at| {
                p.note_line_submit(at);
            }),
            ("a reset", |p, at| {
                p.note_line_submit_at(Some(PROMPT - 1), at);
                p.reset();
            }),
            ("a mode switched", |p, at| {
                p.note_line_submit_at(Some(PROMPT - 1), at);
                p.set_mode(PredictMode::Always);
            }),
        ];
        for (label, shut) in cases {
            for rule_on in [true, false] {
                let mut p = Predictor::new(PredictMode::Adaptive);
                if !rule_on {
                    p.disable_rule_for_test(Rule::WitnessFirstKey);
                }
                let at = t0();
                shut(&mut p, at);
                let at = at + p.glitch_window_for_test() + MS(1);
                let (_, _) = type_signed(&mut p, "$ ", "cd", at);
                assert_eq!(
                    witness(&p),
                    (!rule_on).then_some(PROMPT),
                    "{label}, rule on {rule_on}"
                );
            }
        }
        let mut p = Predictor::new(PredictMode::Adaptive);
        let at = submitted(&mut p, t0());
        let _ = type_signed(&mut p, "$ ", "cd", at);
        assert_eq!(witness(&p), Some(PROMPT), "control: the first key");
    }

    #[test]
    fn a_scroll_after_a_line_terminator_keeps_the_first_key_s_gate() {
        for (host_reset, rule_on) in [(false, true), (true, true), (false, false)] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::ScrollKeepsFirstKey);
            }
            let at = t0();
            p.reconcile_seen(Some((0, 2)), G + 1, false, at, seen_cells("$ "));
            let at = submitted(&mut p, at);
            if host_reset {
                p.reset();
            }
            p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("$ "));
            let _ = type_signed(&mut p, "$ ", "cd", at);
            assert_eq!(
                witness(&p),
                (!host_reset && rule_on).then_some(PROMPT),
                "host reset {host_reset}, rule on {rule_on}"
            );
        }
    }

    #[test]
    fn a_b_left_behind_by_a_narrower_redraw_witnesses_nothing() {
        for rule_on in [true, false] {
            fn at_stale(row: &[Seen], cursor: u16) -> KeySample<'_> {
                KeySample {
                    input_start: Some(InputStart {
                        cell: (0, 4),
                        prompt: PROMPT,
                    }),
                    context: context(),
                    ..sample(row, cursor)
                }
            }
            let mut p = Predictor::new(PredictMode::Always);
            if !rule_on {
                p.disable_rule_for_test(Rule::WitnessFirstKey);
            }
            let mut at = submitted(&mut p, t0());
            let mut screen = String::from("$$$ ");
            for ch in ['a', 'b'] {
                let col = end_of(&screen);
                assert!(p.predict_glyph(ch, &at_stale(&seen_row(&screen), col), at));
                at += RTT;
                screen.push(ch);
                let _ = frame(&mut p, &screen, col + 1, at);
            }
            assert_eq!(witness(&p), Some(PROMPT), "rule on {rule_on}: control");
            for _ in 0..2 {
                for (key, from, to) in [(EditKey::Home, 6, 4), (EditKey::End, 4, 6)] {
                    let row = seen_row(&screen);
                    assert_eq!(
                        p.predict_edit(key, &at_stale(&row, from), at),
                        EditOutcome::Armed
                    );
                    at += RTT;
                    let _ = frame(&mut p, &screen, to, at);
                }
            }
            assert_eq!(p.events_for_test().bindings_learned, 2);
            let screen = String::from("$ ab");
            at += RTT;
            let _ = frame(&mut p, &screen, 4, at);
            assert!(p.predict_glyph('h', &at_stale(&seen_row(&screen), 4), at));
            at += RTT;
            let screen = String::from("$ abh");
            let _ = frame(&mut p, &screen, 5, at);
            let outcome = p.predict_edit(EditKey::Home, &at_stale(&seen_row(&screen), 5), at);
            if rule_on {
                assert_eq!(witness(&p), None, "the stale B witnesses nothing");
                assert_eq!(outcome, EditOutcome::Declined);
            } else {
                assert_eq!(outcome, EditOutcome::Armed, "rule off");
                assert_eq!(
                    frame(&mut p, &screen, 5, at).1,
                    Some((0, 4)),
                    "rule off: Home on the stale B, zle at column 2"
                );
            }
        }
    }

    #[test]
    fn a_contradicted_binding_is_unlearned_and_a_second_bans_it() {
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let mut at = learn_home_and_end(&mut p, &screen, 5, at);
        for round in 0..2 {
            assert_eq!(
                press_signed(&mut p, EditKey::Home, &screen, 5, at),
                EditOutcome::Armed
            );
            let shown = frame(&mut p, &screen, 5, at).1;
            assert_eq!(shown.is_some(), round == 0, "learned, then a probe");
            at += p.glitch_window_for_test() + MS(1);
            let _ = frame(&mut p, &screen, 5, at);
            assert!(p.idle(), "no echo: it expires");
            assert_eq!(p.events_for_test().bindings_unlearned, round + 1);
            at += p.glitch_window_for_test() + MS(1);
            at = submitted(&mut p, at);
            (_, at) = type_signed(&mut p, "$ ", "abc", at);
        }
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Declined,
            "banned"
        );
        assert!(p.idle() && !p.confirmed_epoch);
    }

    #[test]
    fn bindings_hold_for_their_editor_context_and_travel_with_the_session() {
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let at = learn_home_and_end(&mut p, &screen, 5, at);
        let estimate = p.take_link_estimate();
        p.restore_link_estimate(estimate);
        let at = submitted(&mut p, at);
        let (screen, at) = type_signed(&mut p, "$ ", "abc", at);
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed
        );
        assert!(frame(&mut p, &screen, 5, at).1.is_some(), "carried: shown");

        for (label, other) in [
            (
                "another nonce",
                EditorContext::new(1, Some(&[8; 32]), false, false),
            ),
            ("DECCKM", EditorContext::new(1, Some(&[7; 32]), true, false)),
            (
                "escaped chords",
                EditorContext::new(1, Some(&[7; 32]), false, true),
            ),
        ] {
            assert_ne!(other, context(), "{label}: another context");
            let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
            let at = learn_home_and_end(&mut p, &screen, 5, at);
            let row = seen_row(&screen);
            let key = KeySample {
                context: other,
                ..signed(&row, 5)
            };
            assert_eq!(p.predict_edit(EditKey::Home, &key, at), EditOutcome::Armed);
            assert_eq!(
                frame(&mut p, &screen, 5, at),
                (vec![], None),
                "{label}: a probe"
            );
            let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
            let at = learn_home_and_end(&mut p, &screen, 5, at);
            let _ = press_signed(&mut p, EditKey::Home, &screen, 5, at);
            assert!(
                frame(&mut p, &screen, 5, at).1.is_some(),
                "{label}: control"
            );
        }

        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let at = learn_home_and_end(&mut p, &screen, 5, at);
        p.reset_session();
        let at = submitted(&mut p, at);
        let (screen, at) = type_signed(&mut p, "$ ", "abc", at);
        assert!(
            p.line.as_ref().is_some_and(|l| l.fresh == Some(PROMPT)),
            "control: a witnessed line"
        );
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed
        );
        assert_eq!(
            frame(&mut p, &screen, 5, at),
            (vec![], None),
            "an unknown session: a probe"
        );
    }

    #[test]
    fn a_new_editor_context_flushes_the_old_one_s_ops() {
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed
        );
        let row = seen_row(&screen);
        let other = KeySample {
            context: EditorContext::new(1, Some(&[8; 32]), false, false),
            ..signed(&row, 5)
        };
        let later = at + MS(10);
        let _ = p.predict_edit(EditKey::End, &other, later);
        assert!(
            p.preds
                .iter()
                .all(|g| g.op.learned() != Some(Learned::Home)),
            "the old context's Home is flushed"
        );
        assert!(p.owed_at >= Some(at), "and owed");
    }

    #[test]
    fn a_divergence_a_learned_key_heads_unlearns_it() {
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let at = learn_home_and_end(&mut p, &screen, 5, at);
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed
        );
        assert!(frame(&mut p, &screen, 5, at).1.is_some(), "learned: shown");
        let diverged = format!("{screen}~");
        let at = at + RTT;
        let _ = frame(&mut p, &diverged, 6, at);
        assert!(p.idle(), "no state of the chain: flushed");
        assert_eq!(p.events_for_test().bindings_unlearned, 1, "contradicted");
        let at = at + p.glitch_window_for_test() + MS(1);
        let at = submitted(&mut p, at);
        let (screen, at) = type_signed(&mut p, "$ ", "abc", at);
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed
        );
        assert_eq!(
            frame(&mut p, &screen, 5, at),
            (vec![], None),
            "unlearned: a probe again"
        );
    }

    #[test]
    fn ctrl_a_under_a_kitty_mode_is_not_modelled() {
        for kitty in [true, false] {
            let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
            let row = seen_row(&screen);
            let key = KeySample {
                escaped_chords: kitty,
                ..signed(&row, 5)
            };
            let outcome = p.predict_edit(EditKey::CtrlA, &key, at);
            assert_eq!(outcome == EditOutcome::Armed, !kitty, "kitty {kitty}");
            assert_eq!(p.events_for_test().kitty_refusals, u64::from(kitty));
            if kitty {
                assert!(!p.confirmed_epoch, "not modelled: the epoch ended");
                let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
                let row = seen_row(&screen);
                let home = KeySample {
                    escaped_chords: true,
                    ..signed(&row, 5)
                };
                assert_eq!(p.predict_edit(EditKey::Home, &home, at), EditOutcome::Armed);
            }
        }
    }

    #[test]
    fn end_at_the_end_of_the_text_is_declined_with_its_pixels_kept() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "ab");
            if !rule_on {
                p.disable_rule_for_test(Rule::EndRefusal);
            }
            let at = learn_home_and_end(&mut p, &screen, 4, at);
            assert!(p.predict_glyph('c', &signed(&seen_row(&screen), 4), at));
            assert_eq!(frame(&mut p, &screen, 4, at).0, vec![(4, 'c')]);
            let declines = p.events_for_test().decline_motions;
            let outcome = press_signed(&mut p, EditKey::End, &screen, 4, at);
            if rule_on {
                assert_eq!(outcome, EditOutcome::Declined);
                assert_eq!(p.events_for_test().decline_motions, declines + 1);
                assert!(p.anchor_lost && !p.confirmed_epoch);
                assert_eq!(frame(&mut p, &screen, 4, at).0, vec![(4, 'c')], "kept");
            } else {
                assert_eq!(outcome, EditOutcome::Armed, "rule off");
            }
        }
    }

    fn delete_learned_on_ade() -> (Predictor, Instant) {
        let (mut p, screen, mut at) = witnessed_line(PredictMode::Adaptive, "abcde");
        for cursor in [7, 6, 5, 4] {
            assert_eq!(
                press_signed(&mut p, EditKey::Left, &screen, cursor, at),
                EditOutcome::Armed
            );
            at += RTT;
            let _ = frame(&mut p, &screen, cursor - 1, at);
        }
        at = echoed_edge(&mut p, EditKey::Delete, (&screen, 3), ("$ acde", 3), at);
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 6, 3)), "`b` deleted");
        at = echoed_edge(&mut p, EditKey::Delete, ("$ acde", 3), ("$ ade", 3), at);
        assert_eq!(p.events_for_test().bindings_learned, 1, "Delete learned");
        (p, at)
    }

    #[test]
    fn delete_erases_the_glyph_under_the_cursor_and_shifts_the_tail() {
        let (mut p, at) = delete_learned_on_ade();
        assert_eq!(
            press_signed(&mut p, EditKey::Delete, "$ ade", 3, at),
            EditOutcome::Armed
        );
        let _ = frame(&mut p, "$ ade", 3, at);
        let ghosts: Vec<(u16, char, GhostCell)> = p
            .overlay(at)
            .iter()
            .map(|g| (g.col, g.ch, g.ghost()))
            .collect();
        assert_eq!(
            ghosts,
            [
                (3, 'e', GhostCell::Moved { from: 4 }),
                (4, ' ', GhostCell::Vacated)
            ],
            "`d` deleted: `e` moved onto its cell, the last cell vacated"
        );
        assert_eq!(p.overlay_cursor(), Some((0, 3)));
        let at = at + RTT;
        let _ = frame(&mut p, "$ ae", 3, at);
        assert!(p.idle(), "its echo retires it");
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 4, 3)));
    }

    #[test]
    fn delete_vacates_only_beside_blanks() {
        let (mut p, at) = delete_learned_on_ade();
        assert_eq!(
            press_signed(&mut p, EditKey::Delete, "$ adez", 3, at),
            EditOutcome::Declined,
            "text where the line's end vacates"
        );
        let (mut p, at) = delete_learned_on_ade();
        assert_eq!(
            press_signed(&mut p, EditKey::Delete, "$ ade", 3, at),
            EditOutcome::Armed,
            "control: a blank row"
        );
        let _ = frame(&mut p, "$ ade", 3, at);
        assert!(!p.idle(), "control: a blank frame keeps it");
        let _ = frame(&mut p, "$ adez", 3, at + MS(5));
        assert!(p.idle(), "text drawn one past every state flushes it");
    }

    #[test]
    fn home_at_the_start_and_delete_at_the_end_are_not_modelled() {
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        assert_eq!(
            press_signed(&mut p, EditKey::Delete, &screen, 5, at),
            EditOutcome::Declined
        );
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let at = echoed_edge(&mut p, EditKey::Home, (&screen, 5), (&screen, 2), at);
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 2, at),
            EditOutcome::Declined
        );
    }

    #[test]
    fn a_cancelled_stale_guess_anchors_a_stale_run() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::StaleCancel);
            }
            let (screen, at) = type_after(&mut p, "", "xd", t0());
            assert!(p.adaptive_slow, "fixture: a slow link");
            p.note_unmodelled_input(at);
            assert!(p.predict_glyph('o', &sample(&seen_row(&screen), 2), at));
            assert!(p.preds.iter().all(|g| g.stale), "a stale anchor");
            let at = at + RTT;
            let _ = frame(&mut p, "xo", 2, at);
            assert_eq!(p.preds.len(), 1, "`o` waits on its blank cell");
            let at = at + p.glitch_window_for_test();
            assert!(p.predict_backspace(at), "the Backspace cancels `o`");
            let row = seen_row("xo");
            assert!(p.predict_glyph('a', &sample(&row, 2), at));
            assert!(p.predict_glyph('a', &sample(&row, 2), at));
            assert_eq!(
                p.preds.iter().all(|g| g.stale),
                rule_on,
                "rule on {rule_on}: the run is stale"
            );
            let _ = p.take_credited_for_test();
            let (shown, _) = frame(&mut p, "xaa", 3, at + RTT);
            let credited = p.take_credited_for_test();
            assert_eq!(
                (credited.len(), shown.is_empty()),
                if rule_on { (0, true) } else { (1, false) },
                "rule on {rule_on}: {shown:?}"
            );
        }
    }

    #[test]
    fn a_delete_s_step_back_is_no_later_state() {
        for rule_on in [true, false] {
            let (mut p, at) = delete_learned_on_ade();
            if !rule_on {
                p.disable_rule_for_test(Rule::PartWayStepBack);
            }
            assert_eq!(
                press_signed(&mut p, EditKey::Delete, "$ ade", 3, at),
                EditOutcome::Armed
            );
            assert_eq!(
                press_signed(&mut p, EditKey::End, "$ ade", 3, at),
                EditOutcome::Armed
            );
            let _ = p.take_credited_for_test();
            let at = at + RTT;
            let _ = frame(&mut p, "$ ae", 4, at);
            let credited = p.take_credited_for_test();
            assert_eq!(
                credited.len(),
                if rule_on { 0 } else { 2 },
                "rule on {rule_on}: {credited:?}"
            );
            if rule_on {
                assert_eq!(p.preds.len(), 2, "both wait for the step back");
                let _ = frame(&mut p, "$ ae", 3, at + MS(2));
                assert_eq!(p.take_credited_for_test().len(), 1, "Delete alone");
                assert_eq!(p.preds.len(), 1, "End still pending");
            }
        }
    }

    #[test]
    fn a_narrower_redraw_is_no_echo_in_transit() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::TransitBlanks);
            }
            let (screen, at) = type_after(&mut p, "$$$ ", "ab", t0());
            assert_eq!(p.edit_line_for_test(), Some((0, 4, 6, 6)), "fixture");
            assert_eq!(
                press(&mut p, EditKey::Left, &screen, 6, at),
                EditOutcome::Armed
            );
            let at = at + RTT;
            let _ = frame(&mut p, &screen, 5, at);
            assert!(p.idle(), "fixture: the motion class proven");
            assert_eq!(
                press(&mut p, EditKey::Left, &screen, 5, at),
                EditOutcome::Armed
            );
            assert_eq!(frame(&mut p, &screen, 5, at), (vec![], Some((0, 4))));
            let shown = frame(&mut p, "$ ab", 4, at + MS(5));
            if rule_on {
                assert!(p.idle(), "flushed");
                assert_eq!(shown, (vec![], None));
            } else {
                assert_eq!(shown, (vec![(4, 'a'), (5, 'b')], Some((0, 4))), "rule off");
            }
        }
    }
}
