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
        let nonce = nonce.map_or(0, |bytes| aterm_hash::fnv1a_64(bytes));
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
        if self.grid_cols != cols {
            self.deferred_wrap = false;
        }
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
        if self.grid_cols != cols {
            self.deferred_wrap = false;
        }
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
                if *cur > *len || len.checked_add(2).is_none_or(|n| n > cells.len()) {
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
#[path = "lib_tests.rs"]
mod tests;
