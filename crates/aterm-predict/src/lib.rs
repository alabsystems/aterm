// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-predict` — predictive local echo, modelled on mosh: one state machine
//! shared by `aterm-gui` and the web embedders (`aterm-wasm`, `aterm-gpu-web`).
//!
//! A typed glyph normally appears only once the program echoes it through the
//! PTY. Locally that is ~1 ms; over ssh, a loaded box or a heavy TUI it is the
//! dominant input lag. This crate paints the typed character at the cursor at
//! once — a *prediction* — and RECONCILES it against the real output, retiring
//! confirmed guesses and flushing wrong ones.
//!
//! It is PURE and CLOCK-INJECTED (every entry point takes `now: Instant`), so it
//! is testable without sleeping or a PTY. `Instant` is `std::time::Instant` on
//! native and `aterm_time::Instant` on wasm32. The host wires five seams:
//!   1. [`Predictor::predict_glyph`] (or [`Predictor::predict_char_in_grid`], or the
//!      height-blind [`Predictor::predict_char`], neither of which carries the
//!      cursor row) / [`Predictor::predict_backspace`] on a keypress; a run's first
//!      glyph ([`Predictor::wants_key_row`]) hands over the cursor row as
//!      [`Seen`] cells, read under the hold that read the cursor,
//!   2. [`Predictor::note_line_submit`] on a press that writes a line terminator
//!      and nothing else (Enter, Ctrl+J, Ctrl+M; text ending in one, see
//!      [`is_line_submit`]), and [`Predictor::note_unmodelled_input`] on EVERY
//!      other byte-writing press (Tab, arrows, control chords, Alt chords, an IME
//!      commit, a glyph or Backspace the host refuses to guess at its no-echo
//!      gate) and on a paste: input whose echo this crate does not model, each
//!      with the press's clock,
//!   3. [`Predictor::reconcile_seen`] (or [`Predictor::reconcile`], which cannot
//!      say which cells are opaque) after child output is applied to the grid,
//!   4. [`Predictor::overlay`] when composing a frame (the glyphs to paint), with
//!      [`Predictor::overlay_cursor`], the cursor of a state chain's frame (see
//!      below), and
//!   5. [`Predictor::reset`] when the host changes the coordinate space itself
//!      (resize, pane swap): every pending guess sits at an ABSOLUTE row/col.
//!
//! A bare `←`, `→` or Backspace goes to [`Predictor::predict_edit`] instead of
//! the notes, with the press's cursor row: THE EDIT LANE (switched off with
//! [`Predictor::set_editing`]). On a line this epoch has watched echo and prove
//! (the known line), the key is armed as an op of a STATE CHAIN, whose states
//! are the line folded through each op; the chain retires by whole states, and a
//! frame paints the state its visible ops leave, every cell or none, with its
//! cursor. A key the lane cannot place is accounted for there (an arrow is
//! declined with the pixels in flight kept), never noted as well.
//!
//! The terminal moving rows under those coordinates — output scrolling the
//! screen, a region scroll, an inserted or deleted line — is not a seam the host
//! can forget: `predict_char*` and `reconcile` take the host's content-coordinate
//! generation (`coords`), and a change resets the predictor by itself.
//!
//! A host with tabs owns one more seam: the learned echo estimate is a property of the
//! SESSION, not of the window, so it is SWAPPED across a front change
//! ([`Predictor::take_link_estimate`] / [`Predictor::restore_link_estimate`]) rather
//! than thrown away. [`Predictor::reset_session`] remains the "no estimate known" path.
//!
//! ## Safety (why this never corrupts the screen)
//! * **Adaptive display.** Predictions are always *tracked* (to measure echo RTT),
//!   but in the default `Adaptive` mode they are only *shown* after consecutive slow
//!   confirmations establish a stable high-latency link AND at least one prediction
//!   has been confirmed this epoch. One delayed scheduler turn cannot enable pixels.
//!   The gate is HYSTERETIC (open at `DISPLAY_OPEN_MS`, close only after
//!   `FAST_SAMPLES_TO_HIDE` decisive fast samples with a fast *smoothed* RTT and an
//!   empty pending set): a single-sample gate flaps under ordinary jitter, and every
//!   flap erases a ghost already on glass.
//! * **Alt-screen gate.** In the alternate screen (vim/less/htop) the app owns the
//!   cursor and does not line-echo; [`Predictor::reconcile`] flushes and predicting
//!   is refused.
//! * **No unechoed flash (Adaptive).** Adaptive display requires a *confirmed* echo
//!   in the CURRENT epoch, so a password prompt (a line that never echoes) never
//!   displays a predicted character. An epoch is the run of keystrokes since the last
//!   thing this crate could not model, which is mosh's "become tentative" rule, and
//!   it ends at each of them:
//!   - any byte-writing press that is not a predicted glyph: a line terminator
//!     ([`Predictor::note_line_submit`]: Enter, but equally Shift+Enter, Ctrl+J,
//!     Ctrl+M) or anything else ([`Predictor::note_unmodelled_input`]: a paste, an
//!     arrow, a kill chord) — a line submitted by any of them must not carry its
//!     proof to the password typed ahead of the prompt that follows;
//!   - a Backspace with nothing of ours to cancel, and a glyph we decline;
//!   - the rows moving (the `coords` generation): on a terminal scrolled to the
//!     bottom the next prompt reuses the cursor's physical row, so a row check alone
//!     would hand it the proof of the line that just scrolled away, however that line
//!     ended (a `read -n1` answer ends one with no keystroke at all);
//!   - the cursor not standing where the last confirmed echo left it when a guess
//!     starts from it (`proof_cell`): the program moved it — a new row, a redraw,
//!     a prompt printed on the same line (`read -n1 -p 'Proceed? '; read -s -p
//!     ' Password: '`, over ssh as well, where no tty verdict reaches the host).
//!
//!   Guesses carry the epoch they were armed in, so a guess retiring AFTER the
//!   boundary cannot re-arm the new epoch's gate. `Always` is the explicit power-user
//!   opt-in and does NOT carry this guarantee: it can briefly show an unechoed glyph
//!   until the glitch-window expiry. A host that can read the PTY's line discipline
//!   must therefore refuse to arm a guess for a press the tty will not echo
//!   (canonical mode with ECHO clear: `sudo`, `read -s`, a passphrase) in EVERY
//!   mode; only a remote prompt behind a raw-mode link is left to the epoch.
//! * **No ghost of erased text.** Only a line terminator lets the guesses in
//!   flight keep their pixels past the boundary, because it is the one input that
//!   cannot erase or displace them. Every other unmodelled input retires them, in
//!   every mode: a backward erase (Ctrl+H, Ctrl+W, a remapped kill, vi's `x`) that
//!   lands on typed-ahead text leaves the real cursor on a guess over a blank
//!   cell, which reconciliation cannot tell from "not echoed yet".
//! * **One guess per cell.** Nothing is armed behind a line terminator while the
//!   line's own guesses are still in flight: the real cursor stands on the oldest
//!   of them, so a live-cursor anchor would put the new line's glyph on a cell the
//!   old line's echo is about to fill (`ls⏎cd` painted as `cd`).
//! * **Only attributable proof.** A matching glyph is an echo only with the real
//!   cursor past it (a glyph already on screen, an autosuggestion the typed glyph
//!   matches, is not), and it proves a guess only when no
//!   other keystroke's echo could be standing in its cell. Every keystroke this
//!   crate stops tracking — a flushed guess, an expired guess, a terminator,
//!   unmodelled input (including a key the host refused to guess at a no-echo
//!   gate), a declined glyph, an erase, a guess retired by a match that was not
//!   its own — is OWED:
//!   its echo is still on its way. While an echo owed to a keystroke typed
//!   BEFORE a guess is younger than the glitch window, a match retires that
//!   guess's pixels but neither proves the epoch nor measures the link, and
//!   leaves the guess's own echo owed in turn: as its own glyph (a TYPED owe,
//!   which refuses credit only to a later guess for the same glyph), unless it
//!   was typed mid-line; and the epoch's proof is voided, since that echo may
//!   still take the cell the proof names. A PTY carries no `caused_by`
//!   linking output to the keystroke it answers; this bound is what makes
//!   positional retirement exact without one, for every echo that arrives
//!   within its window. The owed echoes travel with the session's
//!   [`LinkEstimate`]: they are in flight on its PTY whether or not it is in
//!   front.
//!
//! * **Only an append proves.** A matching glyph proves its epoch only if it was
//!   typed at the end of the line (whatever the far end then draws beside it: an
//!   autosuggestion appearing, changing or clearing), or the row right of it is
//!   what the press saw there (a right prompt the echo left in place) or blank:
//!   the echo of a key typed mid-line shifts the text right of it, and a proof
//!   from it would paint the next glyph over the tail that moved (the TAIL
//!   GUARD, `appended`). Such a proof stands only while that row still reads as
//!   the press saw it (THE STANDING TAIL), for a far end that writes the glyph
//!   before it redraws the tail.
//!
//! * **Edits show only as states the screen will reach.** A state chain is
//!   placed only on the known line, which holds only verified text, while the
//!   glyph proof stands; it retires at the SMALLEST state the whole screen
//!   matches (text, vacated cells blank, cursor), so no key is credited before its
//!   echo; a motion shows only once a motion of its epoch has echoed exactly
//!   (THE CLASS LADDER: a cooked tty echoes `^[[D`), and the class proof goes
//!   with the glyph proof; the chain expires whole. A run typed on a cursor an
//!   owed echo may still move (a STALE ANCHOR) is credited by no later key's echo.
//!
//!   These, and the guarantees above, are the SAFETY THEOREM checked by
//!   `tests/reconciled_view_safety.rs` over random interleavings against a real
//!   engine, with the ghosts painted by the painter the GUI uses
//!   (`aterm_core::render::RenderInput::paint_prediction_ghost`): the displayed
//!   view differs from the authoritative one only at live guesses armed in the
//!   current coordinate space, a key typed at a visible no-echo prompt never
//!   shows, a displayed guess is where its echo lands, every displayed frame is
//!   the screen its newest guess's echo settles to (every cell and the cursor),
//!   nothing older than the glitch window is ever on glass, a guess is credited
//!   with its echo only once its own key's output has landed, a displayed state
//!   chain is its keys folded over what the screen showed, painted whole with its
//!   cursor, and once the output settles nothing is left on glass.
//! * **Self-healing.** Any prediction unconfirmed for the glitch window
//!   (RTT-relative — floor 250 ms, ceiling 2 s) expires, owing its echo and
//!   voiding its epoch's proof (the late echo lands on the proven cell),
//!   and any divergence (the app drew a different glyph, or the cursor jumped) flushes
//!   the whole set — so a wrong guess is corrected within one output burst.

use aterm_time::Instant;
use std::collections::VecDeque;
use std::time::Duration;

/// Hard cap on pending unconfirmed predictions so stalled echoes across line
/// submits cannot grow the queue unboundedly.
const MAX_PREDICTIONS: usize = 256;
/// Hard cap on typed owes (`Predictor::glyph_owes`); past it the oldest folds
/// into the untyped owe, which only ever refuses more credit.
const MAX_GLYPH_OWES: usize = 64;
/// Hard cap on a state chain's ops (R-CAP): a reconcile folds every state of the
/// chain and compares each with the line's cells, so a chain of `n` ops on a line
/// of `len` cells costs `n · len` comparisons a frame. A key past it is declined
/// as one the chain cannot place.
const EDIT_CHAIN_MAX: usize = 64;

/// How aggressively to DISPLAY predictions. Tracking happens regardless; this only
/// gates what is painted. Parsed from the `predictive_echo` config string.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PredictMode {
    /// Never predict (feature off). The default, so a default-constructed predictor
    /// is inert until config opts in.
    #[default]
    Off,
    /// Show predictions only when the measured echo RTT is high enough to benefit
    /// (and an echo has been confirmed this epoch). The recommended setting.
    Adaptive,
    /// Always show predictions immediately (power users / high-latency links / demos).
    Always,
}

/// One rule of the predictor that a NEGATIVE CONTROL may switch off
/// ([`Predictor::disable_rule_for_test`], a debug-only seam): the safety theorem
/// (`tests/reconciled_view_safety.rs`) runs the same checker over a predictor with
/// one rule off and must report the clause that rule keeps, or the rule asserts
/// nothing. Every rule is in force in a release build, where the seam does not
/// exist.
///
/// Each variant's doc says what the predictor does with the rule switched off.
/// For the rules found by the theorem's stronger clauses that is the code as it
/// was before the fix, so a measurement can run the predictor as it shipped.
///
/// [`Rule::ALL`] is derived from this declaration (the `rules!` macro), so a rule
/// added here is in it without a second list to keep in step: the theorem's
/// `every_rule_has_a_negative_control` walks it, and the exhaustive match that
/// names each rule's clause makes the new rule say what it keeps.
macro_rules! rules {
    ($(#[$meta:meta])* pub enum Rule { $($(#[$vmeta:meta])* $variant:ident,)* }) => {
        $(#[$meta])*
        pub enum Rule {
            $($(#[$vmeta])* $variant,)*
        }

        impl Rule {
            /// Every rule, in declaration order (see the type's docs).
            #[cfg(any(test, debug_assertions))]
            pub const ALL: &'static [Rule] = &[$(Rule::$variant,)*];
        }
    };
}

rules! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Rule {
        /// An expired guess is OWED and voids the proof of its epoch (`expire`). Off:
        /// expiry pops the guess and nothing else, and the epoch keeps a proof whose
        /// cell the expired key's late echo is about to take.
        ExpiryOwes,
        /// A credited echo proves its epoch only as an APPEND (`appended`): typed in
        /// a run at the end of the line (`tail_line_end`), or over a CLEAN TAIL, the
        /// row right of its cell blank or unchanged since the press (`tail_clean`).
        /// Off: any credited echo proves, including one that shifted text right of
        /// it.
        TailGuard,
        /// A guess retired without credit owes its OWN GLYPH, not any echo (a typed
        /// owe). Off: it owes any echo to every guess behind it, the cascade under
        /// which a line typed faster than the glitch window never proves.
        TypedOwes,
        /// A typed owe blocks credit to a later guess for the SAME glyph, while its
        /// window runs. Off: a typed owe blocks nothing.
        TypedOwesSameGlyph,
        /// A typed owe is taken only over a clean tail; over any other it is an owe
        /// of any echo. Off: a typed owe is taken whatever the tail holds.
        TypedOwesCleanTail,
        /// A Backspace that cancels our own trailing guess accounts for the pair's
        /// TRANSIT (`predict_backspace`): the cancelled glyph's echo lands before
        /// the erase's, the screen a cell ahead (or parked at the margin) between
        /// them, so the next guess with nothing of its line pending anchors on the
        /// cancelled cell, not on that live cursor. Off: it anchors on the live
        /// cursor, a cell right of where it echoes, where the next key's echo can be
        /// credited to it, or wrapped by a park the erase then undoes.
        CancelTransit,
        /// A matching glyph is a guess's echo only once the real cursor has moved
        /// past it, or is parked on it in the last column: the echo's own cursor
        /// advance (`echoed_past`). It retires the head guess only then, and only
        /// then excuses a pending guess's cell from the tail comparison
        /// (`tail_clean`). Off: the glyph alone does both, so a run typed along an
        /// autosuggestion, whose cells already show the typed glyphs, was retired,
        /// measured (a sample of a few milliseconds per guess) and credited before
        /// the far end had read a key of it.
        CursorAdvance,
        /// A standing proof that is not at the end of the line holds only while
        /// the row right of the echo it rests on still reads as the run's tail
        /// snapshot: checked at every reconcile, and at a press that continues the
        /// proof on the live cursor before that press takes a new snapshot
        /// (`proof_echo`). Off: the tail is compared only as a guess retires, so an
        /// echo that writes its glyph before it redraws the tail it shifts proved
        /// on the frame between the two, and the next glyph was painted over the
        /// tail about to move.
        StandingTail,
        /// A Backspace that cancels our own trailing guess owes the cancelled
        /// glyph's echo, typed as that glyph (`predict_backspace`): it still lands,
        /// on the cell the next guess anchors on. Off: the pair owes nothing, and a
        /// guess typed next for the same glyph was credited with the cancelled
        /// key's echo.
        CancelOwes,
        /// A guess of the current epoch retired WITHOUT credit voids the epoch's
        /// proof (`retire_echoed`): an echo no key is credited with has moved the
        /// cursor, and the retired key's own echo is still to land. Off: the proof
        /// stood, and once an erase in flight put the cursor back on its cell, the
        /// next glyph was painted there, where the retired key's echo then landed.
        UncreditedVoids,
        /// A state chain retires at the SMALLEST state the screen matches
        /// (`match_chain`): a screen showing `S_j` has seen at least `j` keys'
        /// echoes, and no fewer, whatever later state it also equals. Off: the
        /// largest matching state, which credits `←→` the moment the screen still
        /// shows the state before them, before either key was read.
        SmallestMatch,
        /// A NET-NULL tail (the screen matches `S_j*` and `S_j* = S_n`, as after
        /// `←→` typed in one burst) retires without credit, owing every key of it
        /// (`reconcile_chain`): the screen it matches says nothing about whether
        /// those keys were read. Off: it is credited, measuring the link with a
        /// sample of keys whose echo has not come.
        NetNullUncredited,
        /// A state's VACATED cells, the cells an earlier state of the chain held as
        /// text and it does not, must read blank for the state to match
        /// (`state_matches`). Off: only its text and cursor are compared, so an
        /// erase whose echo arrives in pieces (`\b`, then ` \b`) matched at the
        /// first piece with the erased glyph still on screen.
        VacatedBlankInMatch,
        /// A state matches only as a whole: its text, its vacated cells and its
        /// cursor (`state_matches`). Off: the cursor alone, the evidence the glyph
        /// lane takes for one cell, so a line the program redrew with other text
        /// under the same cursor retired the chain onto text the screen never held.
        WholeStateMatch,
        /// A motion (`←`, `→`) displays in Adaptive only once a motion of the same
        /// epoch has echoed exactly (`op_visible`, the class proof): a cooked tty
        /// echoes `^[[D` for `←`. Off: a motion displays on the glyph proof alone.
        MotionClassProof,
        /// The class proofs go with the glyph proof (`void_proof`): a boundary, a
        /// cursor the program moved, a tail that moved. Off: a motion proven at the
        /// shell stays proven when Enter starts a cooked program on the same line.
        ProofsEndWithTheEpoch,
        /// A credited motion or erase moves the glyph proof's cell but never
        /// earns the glyph proof itself (`retire_op`): only an echoed glyph
        /// proves a line echoes. Off: a credited motion re-proves an epoch whose
        /// glyph proof was voided while it was in flight.
        MotionsNeverProve,
        /// An edit key stays inside the text it knows (`predict_edit`): `←` only
        /// right of `lo`, `→` only left of the text's end, `⌫` only right of `lo`.
        /// At an edge readline beeps and zsh does nothing. Off: the edge key is
        /// armed as if the text went on.
        TextEdges,
        /// An edit op the predictor stops tracking OWES its key's echo, like a
        /// glyph (`decline_motion`, `flush`): a declined arrow's beep or reprint,
        /// a flushed motion's cursor move. Off: a declined arrow owes nothing, and
        /// a flush owes only the newest glyph of the chain.
        EditOpsOwe,
        /// A state chain whose head outlives the glitch window is flushed whole
        /// (`expire`): every later state is built on the head's effect, so no
        /// head-only expiry can apply to it. Off: a chain never expires, and a
        /// chain no echo answers stays on glass past the window.
        StateExpiryFlushes,
        /// A chain carried across a line terminator keeps its predicted cursor
        /// with its pixels (`overlay_cursor`). Off: a carried chain hands the host
        /// no cursor, and the host draws its glyph-lane cursor one past the newest
        /// ghost, a key behind a chain that ends in a motion.
        CarryCursor,
        /// An erase is armed only where the cells from the text's end to one past
        /// the furthest end any state reaches read blank (`vacate_clear`), at the
        /// press and at every reconcile: anything there (an autosuggestion, an
        /// unseen tail) is what the editor pulls into, or redraws over, the cell
        /// the erase vacates. Off: an erase is armed beside anything.
        VacateRefusal,
        /// A guess of a run anchored on the live cursor while an owed echo was in
        /// flight (a STALE ANCHOR, `Prediction::stale`) is credited only with no
        /// owe in flight and no later guess for the same glyph pending
        /// (`attributable`). Off: it is credited as any other, and once the owed
        /// echo (an erase we did not place) stepped the cursor back under the
        /// run, the echo of the guess after it proved the line a cell behind.
        StaleAnchor,
    }
}

impl Rule {
    /// This rule's bit in the predictor's disabled set.
    #[cfg(any(test, debug_assertions))]
    const fn bit(self) -> u32 {
        1 << self as u32
    }
}

impl PredictMode {
    /// Parse the config string (case-insensitive); unknown ⇒ `Off` (fail safe).
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

/// The session's learned echo latency and custody of echoes still in flight,
/// and nothing about where any guess was on screen.
///
/// It also carries when the predictor last stopped tracking a keystroke whose
/// echo may still be on its way (see `Predictor::owed_at`). That echo is in flight
/// on the session's PTY whether or not the session is in front, so it leaves and
/// comes back with the session: a tab switched away and back inside one glitch
/// window must not forget it, or its late echo proves a guess typed after the
/// switch for the same glyph (the proof one keystroke behind, and the next glyph
/// painted where that guess's own echo lands).
///
/// The estimate is a property of the SESSION — the PTY and the link behind it — not
/// of the window that happens to be showing it. A tabbed host must therefore SWAP it:
/// [`Predictor::take_link_estimate`] when a session goes to the background,
/// [`Predictor::restore_link_estimate`] when it comes forward again. Clearing it
/// instead ([`Predictor::reset_session`], the "no estimate known" path) makes every
/// front change re-earn the display latch: `SLOW_SAMPLES_TO_DISPLAY` unpredicted
/// characters after each tab switch.
///
/// OPAQUE by construction: the fields are private, so a host can store one and hand
/// it back but cannot forge a "this link is slow" verdict no measurement earned.
/// [`Default`] is "nothing measured yet" — what a brand-new session starts with, and
/// exactly what `reset_session` leaves behind.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LinkEstimate {
    srtt: Option<f32>,
    slow_samples: u8,
    fast_samples: u8,
    adaptive_slow: bool,
    expiry_backoff: u8,
    // These belong to the PTY too: returning before its old output arrives must
    // not credit that output as a new guess's own echo. No screen state travels.
    owed_at: Option<Instant>,
}

impl LinkEstimate {
    /// Whether the link was measured: an RTT or a sample counted. A test seam that
    /// reads without forging: the fields stay private, and echo debt (`owed_at`) is
    /// custody, not a measurement.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn has_sample(&self) -> bool {
        self.srtt.is_some() || self.slow_samples > 0 || self.fast_samples > 0
    }
}

/// One predicted glyph at an absolute grid cell, awaiting confirmation from output.
#[derive(Clone, Copy, Debug)]
pub struct Prediction {
    /// Grid row the guess was typed on (active-grid coords).
    pub row: u16,
    /// Grid column the guess occupies.
    pub col: u16,
    /// The predicted glyph.
    pub ch: char,
    /// When predicted — drives the unconfirmed-glitch timeout (the glitch window).
    born: Instant,
    /// The confirmation epoch (see `Predictor::epoch`) this guess was armed in. A
    /// guess armed BEFORE an Enter must never arm the NEXT line's display gate when
    /// its echo finally lands, or a password prompt inherits the submitted command's
    /// confirmation — the exact leak ending the epoch exists to prevent.
    epoch: u64,
    /// When the newest keystroke typed BEFORE this guess whose echo is owed was
    /// typed (see `Predictor::owed_at`): the snapshot taken when the guess was
    /// armed, raised when a guess ahead of it retires without credit. Until one
    /// glitch window has passed since then, that echo may be what stands in this
    /// guess's cell (see `Predictor::attributable`).
    ///
    /// Per guess, not the predictor's newest owe, because only a keystroke typed
    /// EARLIER can stand in this guess's cell. A terminator or a declined glyph
    /// typed after it echoes after it, so the guess is asked only about what came
    /// before: the line's tail still measures the link while its terminator's echo
    /// is owed, and a carried guess with an echo owed from before it is still
    /// refused credit (the newest owe, the terminator's, cannot tell those apart).
    owed: Option<Instant>,
    /// Press order among guesses (`Predictor::next_seq`): a typed owe asks only
    /// about guesses typed after the one that owes it.
    seq: u64,
    /// What the key does to the line: a typed glyph (every guess of the glyph
    /// lane), or an edit op of a state chain (see `Predictor::line`). The glyph
    /// lane and the chains share one queue, so every key that reached the PTY is
    /// held by exactly one pending op, typed owe or owe of any echo.
    op: Op,
    /// The proof the op needs before Adaptive shows it, fixed when it is armed
    /// from the state before it (`Predictor::op_visible`).
    class: OpClass,
    /// The guess belongs to a run anchored on the live cursor while an owed echo
    /// was in flight (`Predictor::run_stale`): a STALE ANCHOR. That echo may still
    /// move the cursor the run was placed from, and an erase we did not place
    /// moves it BACK: every guess of the run then echoes a cell left of where it
    /// is placed, and a guess's cell receives the echo of the key AFTER it. A
    /// typed owe asks only about keys typed before a guess; this asks about the
    /// ones after (`Predictor::attributable`).
    stale: bool,
}

/// What one pending key does to the line it is typed on (`Prediction::op`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// A glyph typed at the end of the text: every guess of the glyph lane, and
    /// a glyph typed in a state chain once its motions have brought the cursor
    /// back to the end.
    Insert,
    /// `←`: the cursor one cell left, no cell changed.
    Left,
    /// `→`: the cursor one cell right, no cell changed.
    Right,
    /// `⌫` at the end of the text: the last glyph erased, the cursor on its cell.
    Erase,
}

/// The proof an op needs before Adaptive shows it (`Predictor::op_visible`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpClass {
    /// The glyph proof alone (`confirmed_epoch`): a glyph typed at the end of the
    /// line, and an erase at the end, which every line editor in the family and a
    /// cooked tty (ECHOE) echo alike.
    Glyph,
    /// The glyph proof and a motion of the same epoch echoed exactly (`proven`):
    /// a cooked tty echoes `^[[D` for `←`, which only that proof tells apart.
    Motion,
}

/// `Predictor::proven`'s bit for the motion class.
const PROVEN_MOTION: u8 = 1;

/// The `ch` of an op that writes no glyph of its own (a motion, an erase): NUL,
/// which no typed owe names and no host paints (an engaged chain's overlay is its
/// ghost cells, never its ops).
const EDIT_OP_CH: char = '\0';

impl Prediction {
    /// Host-test constructor (`born` is private, stamped `now`): `aterm-gui`'s
    /// ghost-painter tests build guesses without a [`Predictor`]. Not for
    /// production; real guesses come only from [`Predictor::predict_char`].
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
        }
    }

    /// This guess's press order among the predictor's guesses: what
    /// [`Predictor::take_credited_for_test`] names it by. A read-only test seam.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn seq_for_test(&self) -> u64 {
        self.seq
    }

    /// The edit key this pending op was armed for, or `None` for a typed glyph.
    /// A read-only test seam: the safety theorem follows each op back to its key.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn edit_key_for_test(&self) -> Option<EditKey> {
        match self.op {
            Op::Insert => None,
            Op::Left => Some(EditKey::Left),
            Op::Right => Some(EditKey::Right),
            Op::Erase => Some(EditKey::Backspace),
        }
    }
}

/// What the host read at one cell, as the predictor needs it: the answer of an
/// `observe` closure ([`Predictor::reconcile_seen`]) and each cell of the row a
/// key-time sample carries ([`KeySample::row`]).
///
/// A glyph alone is not enough to say whether a row changed: a wide glyph's two
/// halves, a grapheme cluster, combining marks, a row the frame draws in another
/// order (BiDi) or at another size (DEC double width) all put something on a cell
/// that its glyph and column do not describe. Those cells are [`Seen::Opaque`]:
/// the predictor compares them only for equality, never reads them as text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// Nothing drawn: an empty cell or a space.
    Blank,
    /// One narrow glyph, drawn at its own logical column.
    Glyph(char),
    /// A cell no glyph and column describe (see the type's docs). It carries
    /// what a plain glyph observe answers there, the cell's `char` or `None` for
    /// a blank, so the glyph lane reads it exactly as it read the plain answer.
    Opaque(Option<char>),
}

impl Seen {
    /// The cell holding `ch` (`' '` for an empty cell), opaque or not, as a host
    /// projects it: a space is [`Seen::Blank`].
    #[must_use]
    pub fn of_cell(ch: char, opaque: bool) -> Self {
        let legacy = (ch != ' ').then_some(ch);
        match legacy {
            _ if opaque => Self::Opaque(legacy),
            Some(ch) => Self::Glyph(ch),
            None => Self::Blank,
        }
    }

    /// What a plain glyph observe answered at this cell: its glyph, or `None` for
    /// a blank. The glyph lane reads every cell through this.
    #[must_use]
    pub fn legacy(self) -> Option<char> {
        match self {
            Self::Blank => None,
            Self::Glyph(ch) => Some(ch),
            Self::Opaque(legacy) => legacy,
        }
    }
}

/// What the host read under the terminal hold of one keypress, for
/// [`Predictor::predict_glyph`].
#[derive(Clone, Copy, Debug)]
pub struct KeySample<'r> {
    /// The live cursor `(row, col)`.
    pub cursor: (u16, u16),
    /// The host's content-coordinate generation (see [`Predictor::reconcile`]).
    pub coords: u64,
    /// `(cols, rows)`: a guess past the right margin continues at column 0 of
    /// the next row, and one off the bottom row is declined.
    pub grid: (u16, u16),
    /// The cursor row, one [`Seen`] per logical column, read under the same hold
    /// as the cursor. Wanted for a glyph only when [`Predictor::wants_key_row`]
    /// says so (the first glyph of a run), and for every edit key
    /// ([`Predictor::predict_edit`]); `None` otherwise, and from a host that
    /// cannot read it, in which case only a blank tail proves the run (see
    /// `tail_clean`) and no edit key is armed.
    pub row: Option<&'r [Seen]>,
}

/// A line-editing key the host hands [`Predictor::predict_edit`]: bare, with no
/// modifier, from the main block (a keypad arrow under DECKPAM sends another
/// sequence, and a modified arrow is a word motion or something else again).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKey {
    /// `←` (`ESC [ D`, or `ESC O D` under DECCKM).
    Left,
    /// `→` (`ESC [ C`, or `ESC O C` under DECCKM).
    Right,
    /// Backspace (`DEL` or `BS`, as the host already classifies it).
    Backspace,
}

/// What [`Predictor::predict_edit`] did with an edit key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// An op was armed for it: the key is tracked, as a pending glyph is.
    Armed,
    /// A Backspace cancelled our own trailing glyph (the pair's echoes cancel).
    Cancelled,
    /// Nothing was armed. The key has been accounted for already (owed, and the
    /// epoch ended or the guesses retired as its kind requires); the host must not
    /// also call [`Predictor::note_unmodelled_input`] for it.
    Declined,
}

/// The single-row line a STATE CHAIN predicts on (`Predictor::line`): the text
/// this epoch has watched echo there, as last verified against the screen. It is
/// `S_0`, the state before the oldest pending op; every pending op folds onto it
/// (`Predictor::fold_op`).
///
/// Only verified text enters it: a glyph whose echo was credited and proved its
/// epoch as an append (`Predictor::extend_line`), and a state the whole screen
/// matched (`Predictor::reconcile_chain`). So it holds narrow plain glyphs, and
/// nothing right of its end is the editor's text: an echo that shifted text there
/// would not have proved.
#[derive(Clone, Debug)]
struct EditLine {
    row: u16,
    /// Where this line's verified text begins: a LOWER BOUND on the editor's
    /// buffer start. No op crosses it; text left of it may be prompt or older
    /// buffer, and a motion into it is not ours to place.
    lo: u16,
    /// The epoch the line belongs to. After a terminator it only carries pixels.
    epoch: u64,
    text: Vec<char>,
    cursor: u16,
    /// One past the furthest text end the chain's verified states have reached
    /// (`V_0`): the cells from the text's end up to it were text once and are
    /// verified blank since. A state's vacated cells start where its text ends
    /// and run to the furthest end any state before it reached.
    vacated: u16,
}

impl EditLine {
    /// One past the last cell of the text.
    fn hi(&self) -> u16 {
        self.lo
            .saturating_add(u16::try_from(self.text.len()).unwrap_or(u16::MAX))
    }
}

/// The end of a state chain as [`Predictor::placeable`] folds it: where the next
/// edit key applies.
#[derive(Clone, Copy, Debug)]
struct ChainEnd {
    row: u16,
    lo: u16,
    hi: u16,
    cursor: u16,
    /// One past the furthest text end of any state, the base's included (`V_n`).
    vacated: u16,
}

/// Whether `seen` is the cell a state shows holding `ch` (a typed space reads
/// blank: the cursor tells it from an empty cell). An opaque cell is never a
/// state's cell.
fn cell_holds(seen: Seen, ch: char) -> bool {
    match seen {
        Seen::Blank => ch == ' ',
        Seen::Glyph(g) => g == ch,
        Seen::Opaque(_) => false,
    }
}

/// Counts of the predictor's phase-0 decisions, for the safety theorem's
/// non-vacuity floors: each must be reached on its committed seed. Debug builds
/// only ([`Predictor::events_for_test`]).
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredictEvents {
    /// Expired guesses that owed their echo ([`Rule::ExpiryOwes`]).
    pub expiry_owes: u64,
    /// Credited echoes of the current epoch that proved nothing because the row
    /// right of them was not clean ([`Rule::TailGuard`]).
    pub tail_refusals: u64,
    /// Credited echoes of the current epoch that proved it over a tail holding
    /// text the press saw there (a right prompt, a suggestion the glyph matched).
    pub decorated_proofs: u64,
    /// Guesses retired without credit that owed only their own glyph
    /// ([`Rule::TypedOwes`]).
    pub typed_owes: u64,
    /// Standing proofs voided because the row right of the echo they rest on
    /// moved after that echo proved ([`Rule::StandingTail`]).
    pub standing_voids: u64,
    /// Reconcile turns that held the head guess on a cell showing its glyph
    /// before the cursor had moved past it ([`Rule::CursorAdvance`]).
    pub advance_waits: u64,
    /// Edit ops armed by [`Predictor::predict_edit`] (motions and erases).
    pub edit_ops_armed: u64,
    /// Credited motions that proved their class ([`Rule::MotionClassProof`]).
    pub motions_proven: u64,
    /// Net-null tails retired without credit ([`Rule::NetNullUncredited`]).
    pub net_null_retired: u64,
    /// Reconcile turns of a state chain that matched no state and waited, every
    /// compared cell blank or some state's (the transit wait).
    pub transit_waits: u64,
    /// Arrows declined with their pixels kept (decline-motion).
    pub decline_motions: u64,
    /// Erases refused for a cell past the text's end that was not blank
    /// ([`Rule::VacateRefusal`]), at the press or at a reconcile.
    pub vacate_refusals: u64,
}

/// One event [`PredictEvents`] counts.
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
}

/// Floor of the unconfirmed-guess expiry window: below this a normal output burst has
/// not necessarily arrived yet even on a local link.
const GLITCH_FLOOR_MS: u64 = 250;
/// Ceiling of the expiry window. The window is RTT-relative (see
/// [`Predictor::glitch_window`]) so a slow link can actually keep a guess alive long
/// enough to be confirmed; this bounds how long a *wrong* guess (raw-mode key,
/// swallowed input) can sit on glass before it self-heals.
const GLITCH_CEILING_MS: u64 = 2000;
/// The expiry window as a multiple of the smoothed RTT. A fixed 250 ms window is
/// shorter than the echo on the links the feature targets, so nothing would ever
/// confirm and the adaptive latch could never open. Three RTTs leaves room for one
/// retransmit-scale outlier.
const GLITCH_SRTT_MULT: f32 = 3.0;
/// How many times consecutive timeouts may DOUBLE the glitch floor while probing a
/// cold link (250 → 500 → 1000 → 2000 ms). Bounded so an endlessly non-echoing
/// context — a password prompt, a raw-mode key — cannot walk the window upward
/// without limit, and reset by the first real confirmation ([`Predictor::record_rtt`]).
const EXPIRY_BACKOFF_MAX: u8 = 3;
/// `Adaptive` OPENS the display gate on samples at or above this latency. Well clear
/// of aterm's own local-echo distribution (single-digit ms), so ordinary scheduler
/// noise cannot be mistaken for a link that speculation can help.
const DISPLAY_OPEN_MS: f32 = 20.0;
/// `Adaptive` may CLOSE the display gate only on samples below this latency. The gap
/// to [`DISPLAY_OPEN_MS`] is the hysteresis band: samples inside it are evidence for
/// neither regime and leave the latch (and both streaks) untouched, so a link jittering
/// across one threshold cannot flap the gate — and every flap erases a ghost that is
/// already on glass.
const DISPLAY_CLOSE_MS: f32 = 8.0;
/// A single delayed event-loop turn is not evidence of a slow link. Require two
/// consecutive slow samples before Adaptive may paint.
const SLOW_SAMPLES_TO_DISPLAY: u8 = 2;
/// …and symmetric-but-stricter evidence to stop painting: three consecutive decisive
/// fast samples. Closing is the destructive direction (it erases in-flight pixels), so
/// it also requires the smoothed RTT to agree and the pending set to be empty.
const FAST_SAMPLES_TO_HIDE: u8 = 3;
/// EWMA weight for a freshly-confirmed echo sample into the smoothed RTT.
const SRTT_ALPHA: f32 = 0.3;

/// Compute the bounded unconfirmed-prediction glitch expiry window in
/// milliseconds from an optional smoothed RTT in microseconds (`srtt_us`) and
/// consecutive expiry backoff count (`expiry_backoff`).
///
/// Matches thepredictor's adaptive window:
/// - floor: `250 ms << min(expiry_backoff, 3)` (capped at `2000 ms`)
/// - RTT target: `3 * srtt_ms` when `srtt_us` is present
/// - clamped to `[floor_ms, 2000 ms]`.
#[must_use]
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

/// Fixed-point EWMA SRTT step in microseconds (`alpha = 3/10`, matching
/// `SRTT_ALPHA = 0.3`): `0.7 * prev_srtt_us + 0.3 * sample_us`.
#[must_use]
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

/// Encode a predicted overlay ghost cell `(row, col, ch, epoch)` into a
/// 16-byte little-endian wire record (`row: u16, col: u16, ch: u32, epoch: u64`).
#[must_use]
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

/// Decode a 16-byte little-endian predicted ghost cell wire record produced by
/// [`encode_predicted_ghost_wire_le`], rejecting invalid Unicode scalar values.
#[must_use]
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

/// Whether `bytes` written to the program are a line terminator and nothing that
/// could erase or move what was typed before them: UTF-8 text that ends in CR or LF
/// and holds no other control character (no BS, no ^U/^W, no ESC prefix, no DEL).
/// The class [`Predictor::note_line_submit`] is for, for a host that has the bytes
/// rather than the key (a remapped key sequence, controller input); any other
/// byte-writing input is [`Predictor::note_unmodelled_input`].
#[must_use]
pub fn is_line_submit(bytes: &[u8]) -> bool {
    let is_terminator = |c: char| matches!(c, '\r' | '\n');
    std::str::from_utf8(bytes).is_ok_and(|text| {
        text.ends_with(is_terminator) && text.chars().all(|c| is_terminator(c) || !c.is_control())
    })
}

/// The speculative-echo state machine for one terminal pane. See the crate docs.
#[derive(Default)]
pub struct Predictor {
    mode: PredictMode,
    /// Pending predictions in type order (oldest = `front`). A guess at the right
    /// margin continues at column 0 of the NEXT row when the host supplies the grid
    /// height ([`Predictor::predict_char_in_grid`]); an unexpected row change still
    /// flushes them.
    preds: VecDeque<Prediction>,

    /// Smoothed echo round-trip estimate in ms (EWMA of confirmed predictions). The
    /// link property `Adaptive` consults. Coordinate-only [`reset`](Self::reset)
    /// preserves it; [`reset_session`](Self::reset_session) clears it so a slow
    /// remote pane can never make a newly focused local pane speculate visibly.
    srtt: Option<f32>,
    /// Consecutive expiry turns with no confirmation, doubling the glitch floor as a
    /// COLD-LINK probe (see [`Predictor::glitch_window`]). Deliberately NOT an input to
    /// `srtt` or to the display regime: a timeout is the absence of an echo, not a slow
    /// echo (see `expire`). Reset by the first real confirmation.
    expiry_backoff: u8,
    /// Consecutive raw echo samples at or above [`DISPLAY_OPEN_MS`]. This filters
    /// isolated scheduler tails that would otherwise seed the EWMA above the visual
    /// threshold after one sample.
    slow_samples: u8,
    /// Consecutive raw echo samples below [`DISPLAY_CLOSE_MS`] — the closing streak.
    /// Separate from `slow_samples` because the two thresholds differ (hysteresis):
    /// a sample inside the band advances neither.
    fast_samples: u8,
    /// Stable Adaptive display classification. Consecutive slow samples open it;
    /// closing needs a sustained fast streak, a fast smoothed RTT, and nothing in
    /// flight — so returning from a remote foreground process to a local shell still
    /// closes it promptly, but ordinary jitter cannot blink a ghost off the glass.
    adaptive_slow: bool,
    /// Have we confirmed ≥1 prediction in the CURRENT epoch, for the cursor the
    /// next guess extends (`proof_cell`)? Gates display so an unechoed context never
    /// shows a guess. Cleared whenever the epoch ends (see the crate docs for every
    /// boundary) and whenever a guess starts from a cursor the proof does not cover.
    confirmed_epoch: bool,
    /// The cell right after the last echo this epoch CONFIRMED: where the real
    /// cursor stands when nothing but our own keystrokes has moved it since. `Some`
    /// exactly while `confirmed_epoch` holds. A guess anchored on a live cursor
    /// anywhere else starts unproven, because the program moved the cursor without
    /// us: a new row, a redraw, or a prompt printed on the same line —
    /// `read -n1 -p 'Proceed? '; read -s -p ' Password: '`, over ssh too, where no
    /// tty verdict reaches the host — so the proof never covers the secret typed
    /// at the cursor that prompt left behind.
    proof_cell: Option<(u16, u16)>,
    /// The guess whose echo the standing proof rests on (`Some` exactly while
    /// `confirmed_epoch` holds): THE STANDING TAIL re-reads the row right of it
    /// against the run's snapshot at every reconcile, and at a press that
    /// continues the proof on the live cursor before that press re-snapshots the
    /// row (`tail_clean`, [`Rule::StandingTail`]). Not for a proof at the end of
    /// the line (`proof_line_end`): there is no tail to move.
    ///
    /// Comparing only as each guess retires missed the echo that writes its glyph
    /// FIRST and redraws the tail it shifts after (`e`, then ` cd`, then the
    /// cursor stepped back, as a line editor without ICH, or one that rewrites
    /// the tail, sends it). A frame between the two shows `abecd` with the cursor
    /// past `e`: the row right of `e` is what the press saw there, so the echo
    /// proves, and once the redraw lands (`abe cd`) the next glyph, anchored on
    /// that proof, was painted over the shifted space (`abeicd`, settling to
    /// `abei cd`). Re-read at the frame after the redraw, the tail has moved and
    /// the proof is voided before anything is painted on it; and a press that
    /// comes first compares its own row with the snapshot, since the snapshot it
    /// takes next would be of the moved row.
    proof_echo: Option<Prediction>,
    /// The standing proof was earned at the END OF THE LINE (`tail_line_end` of
    /// the run its echo belongs to): the far end's line ends at `proof_cell`, so
    /// whatever stands right of it is text the far end drew that is not the line
    /// (an autosuggestion, a right prompt), and a run started on `proof_cell`
    /// inherits the end of the line whatever its snapshot shows. `false`
    /// whenever `confirmed_epoch` is.
    proof_line_end: bool,
    /// Monotonic id of the CURRENT confirmation epoch, bumped by
    /// [`end_epoch`](Self::end_epoch). Guesses are tagged with it so a boundary can
    /// end the epoch for FUTURE guesses without discarding the pixels of in-flight
    /// ones: a stale guess still reconciles (and still feeds the RTT estimate) but
    /// can neither anchor a new guess nor arm the new epoch's gate.
    epoch: u64,
    /// The most recent grid width seen by `predict_char*`. Only [`reconcile`](Self::reconcile)
    /// reads it, for the two margin questions: a guess that continues at column 0 of
    /// the next row is confirmed against a real cursor parked in the DEFERRED-WRAP slot
    /// (last column of the previous row), which is where a terminal leaves it after
    /// echoing the final column — and whether the cursor it is looking at is in that
    /// slot at all (`deferred_wrap`).
    grid_cols: u16,
    /// The terminal is PARKED in the deferred-wrap slot: at the last [`reconcile`](Self::reconcile)
    /// the real cursor sat on the final column of a row whose cell was already FILLED
    /// (by a glyph, or by the right half of a wide one).
    /// That parking is indistinguishable by cursor position alone from a cursor sitting
    /// on a free last column, and the difference decides where the next keystroke
    /// echoes: a parked terminal wraps it to column 0 of the next row, so anchoring on
    /// the raw cursor would paint the guess ON TOP of the glyph just echoed. Recorded
    /// at `reconcile` because `predict_char_in_grid` has no grid to consult.
    deferred_wrap: bool,
    /// A character we could not model was declined while guesses were pending. Those
    /// guesses are anchored to its LEFT and remain valid, but everything typed after it
    /// would be predicted one or more columns too far left (the real echo will insert
    /// the declined glyph). So we stop extending until the set drains and the live
    /// cursor — which the real echo moves correctly — can re-anchor us.
    anchor_lost: bool,
    /// The epoch whose still-pending guesses keep displaying after it ended. Set when
    /// an epoch that had CONFIRMED an echo ends at a line terminator
    /// ([`note_line_submit`](Self::note_line_submit)), so the carried pixels are
    /// always characters the user has already watched that line echo back — never
    /// a secret — and nothing since could have erased them. An unconfirmed epoch
    /// ending leaves it alone: its own guesses stay dark, and the confirmed epoch's
    /// guesses ahead of them keep their pixels. Any other unmodelled input retires
    /// the carried guesses with the rest.
    grandfathered_epoch: Option<u64>,
    /// The host's content-coordinate generation at the last observation (`None`
    /// until the first, and again after a [`reset`](Self::reset)). See
    /// [`observe_coordinates`](Self::observe_coordinates).
    coords: Option<u64>,
    /// When the newest keystroke whose echo this predictor stopped tracking was
    /// typed: a guess dropped by a flush, a press it never guessed (a line
    /// terminator, unmodelled input, a declined glyph, a Backspace with nothing of
    /// ours to cancel), or a guess retired by a match that may not have been its
    /// own echo. Until one glitch window has passed since then, that echo may still
    /// be on its way, and a glyph matching a guess typed after it may be THAT echo
    /// rather than the guess's own (see `attributable`). Each guess takes a
    /// snapshot of it when armed (`Prediction::owed`).
    ///
    /// A guess retired because its own glyph and cursor advance appeared is the
    /// only evidence positional reconciliation has; aterm's PTY carries no
    /// `caused_by` saying which keystroke an output byte answers (astream-term's
    /// exact retirement rides on one). This is the bound that makes the
    /// positional evidence exact anyway: it is credited to a guess only when no
    /// other keystroke's echo could be standing in its cell.
    ///
    /// Every owe is dated by the press it belongs to: the notes that owe one
    /// take the press's clock, because a stamp taken at the host's next clocked
    /// call would date it by however long the host stayed idle (a split pane
    /// reconciles only while something is in flight), and an owe dated at the
    /// next keystroke falls inside that keystroke's own echo, so the line it
    /// starts could never prove itself.
    owed_at: Option<Instant>,
    /// The terminal draws East Asian Ambiguous glyphs two cells wide
    /// (`ambiguous_width = "wide"`): the width the grid writes them with, so the
    /// gate measures with it too ([`set_ambiguous_width_double`](Self::set_ambiguous_width_double)).
    ambiguous_width_double: bool,
    /// How many printable glyphs were declined since the newest guess was armed
    /// (since the last flush or line terminator): the input a Backspace erases
    /// FIRST. A Backspace that erases one of them cancels its echo, and the
    /// guesses to its left still echo exactly where they are painted, so it
    /// retires none of them — `ls⏎c⌫` keeps the submitted line's tail on glass
    /// although `c` was never guessed. Arming a guess zeroes it: the declined
    /// glyphs are then behind that guess, and a Backspace that reaches them has
    /// erased something we no longer place.
    declined_tail: u16,
    /// The next guess's press order (`Prediction::seq`).
    next_seq: u64,
    /// The cell of the guess a Backspace just cancelled: where the next guess
    /// anchors if it finds nothing of its line pending, instead of the live
    /// cursor, which the cancelled glyph's echo may have moved or parked before
    /// the erase's echo puts it back (see `predict_backspace`). Taken by the next
    /// guess, whatever it anchors on; cleared by every flush and epoch boundary.
    cancelled_at: Option<(u16, u16)>,
    /// The newest guess a Backspace cancelled, `(row, col, ch, next_seq)`: its
    /// pair is still writing and erasing that cell (the glyph's echo, then the
    /// erase's) until a guess typed after it (`seq ≥ next_seq`) retires. Our own
    /// keys' output, accounted for, so the tail comparison lets that cell hold
    /// that glyph (`tail_clean`): a proof is not voided, nor an append refused,
    /// because the cancelled glyph showed beside it for a frame. Cleared by
    /// every flush and epoch boundary.
    cancelled_pair: Option<(u16, u16, char, u64)>,
    /// TYPED OWES: `(seq, ch, born)` of each guess retired without credit over
    /// a clean tail, oldest first. Its own echo is still on its way, and what it
    /// puts on screen is known: `ch`, at the cell after the match that retired
    /// it, which is the next guess's. So it can stand only in a later guess FOR
    /// THE SAME GLYPH; any other guess there sees a different glyph and flushes.
    /// It refuses credit to exactly those, while its window runs
    /// (`attributable`), instead of refusing it to every guess behind it.
    ///
    /// THE CASCADE THIS BREAKS: owed as any echo, each uncredited retirement
    /// raised the owe of every guess behind it to its own press, so the next
    /// match was uncredited too, and on a line typed faster than the glitch
    /// window nothing was ever credited after one owed key (an Enter, an
    /// arrow): on a 400 ms link the whole line stayed dark. Typed, the first
    /// guess for a glyph not owed in the window proves the line.
    ///
    /// Taken only over a CLEAN TAIL (`tail_clean`): a guess retired mid-line has
    /// an echo that shifts the text right of it, and a shifted tail glyph can
    /// stand in a later guess's cell whatever glyph the retired guess was. Bounded
    /// (`MAX_GLYPH_OWES`): past the cap, and at every flush, the typed owes fold
    /// into `owed_at`, the owe of any echo, which only ever refuses more.
    glyph_owes: VecDeque<(u64, char, Instant)>,
    /// The rules a negative control switched off ([`Rule`]), one bit each. Not in
    /// a release build: there every rule is in force.
    #[cfg(any(test, debug_assertions))]
    disabled: u32,
    /// THE KNOWN LINE (K1) the edit keys predict on: the text this epoch has
    /// watched echo, verified, with its cursor (see [`EditLine`]). Created and
    /// extended by a credited glyph that proved its epoch as an append
    /// (`extend_line`), moved by every state a chain retires to, and dropped
    /// wherever its text could stop being the screen's without the predictor
    /// seeing it: a flush, a boundary (unless a carried chain still needs it as
    /// its base), the proof voided with no chain in flight, a glyph typed
    /// mid-text. So it exists only while the glyph proof does, or while a chain
    /// it is the base of is in flight; an edit key needs both it and the proof.
    ///
    /// R-OWED is a consequence, not a rule: the text grows only from credited
    /// echoes, so it exists only once no owe was within the window at a credit,
    /// and every owe site after that within an epoch drops it (a flush, an
    /// expiry, an uncredited retirement) or ends the epoch.
    line: Option<EditLine>,
    /// A STATE CHAIN is in flight: an edit key was armed, and until the queue
    /// drains every pending op reconciles by state (`reconcile_chain`) and every
    /// glyph typed meanwhile joins the chain (`arm_in_chain`). Read through
    /// `engaged`, which also asks the queue: every path that empties the queue
    /// ends the chain.
    chain: bool,
    /// The op classes this epoch has watched echo exactly ([`PROVEN_MOTION`]),
    /// beside the glyph proof (`confirmed_epoch`), which every class needs first.
    /// Cleared with the glyph proof (`void_proof`): a cursor the program moved, a
    /// tail that moved or a boundary may put a different program behind the same
    /// line, a cooked `cat` after the shell that proved `←`.
    proven: u8,
    /// `proven` as it stood when the epoch whose ops are carried
    /// (`grandfathered_epoch`) ended: the carried ops keep displaying exactly as
    /// they did.
    carried_proven: u8,
    /// The edit lane is switched off (`set_editing(false)`): every edit key is
    /// routed as before it existed. The kill switch, and a measurement's baseline.
    editing_off: bool,
    /// The host's last reconcile could say which cells are opaque
    /// ([`Predictor::reconcile_seen`]). A chain compares whole states, and a host
    /// that reads a wide glyph's half as text cannot tell it from a state's cell,
    /// so behind a legacy [`Predictor::reconcile`] no edit key is armed.
    edit_capable: bool,
    /// The cells `[lo, lo + seen.len())` of the chain's row as the last
    /// reconcile of a chain read them: what the ghosts are painted against.
    seen: Vec<Seen>,
    /// Scratch for folding a chain's states: one state's text.
    fold_text: Vec<char>,
    /// The cells an engaged chain's overlay paints: its displayed state where it
    /// differs from the screen. Resident, so [`Predictor::overlay`] keeps
    /// returning a borrow.
    ghosts: Vec<Prediction>,
    /// The cursor of the displayed state of an engaged chain, as the last
    /// [`Predictor::overlay`] composed it ([`Predictor::overlay_cursor`]).
    ghost_cursor: Option<(u16, u16)>,
    /// Scratch for the transit wait (`in_transit`): one mark per cell read.
    ghost_marks: Vec<u8>,
    /// The glyph-lane run in flight was anchored on the live cursor while an
    /// owed echo was in flight (`owed_at` inside the window at its first press):
    /// its guesses carry `Prediction::stale`. Every guess extending it does.
    run_stale: bool,
    /// Where the newest run of guesses anchored on the live cursor starts its
    /// TAIL: `(row, col)` of the cursor cell at that press, whose contents and
    /// those of the cells right of it are `tail_cells`. `None` when that press
    /// carried no row ([`KeySample::row`]), or its guess wrapped to the next row.
    ///
    /// THE TAIL GUARD (`tail_clean`) compares the row right of a confirmed guess
    /// with it. A guess is an APPEND: its echo writes one cell and moves nothing.
    /// The echo of a key typed mid-line instead shifts the text right of it, and a
    /// guess anchored on that echo paints over a tail that is about to move: `ab
    /// cd`, three Lefts onto the space, then `e`, `i` typed, showed `abeicd` until
    /// the screen settled to `abei cd`. Its echo proved the epoch, because
    /// positional evidence sees the glyph and the cursor advance and nothing else.
    /// The snapshot, not a blank row, is the reference, so a right prompt (or a
    /// suggestion the typed glyph matches) that the echo leaves in place still
    /// lets the line prove: a blank-only rule would darken every zsh line with an
    /// RPROMPT, which proves today. Taken at the PRESS, under the hold that read
    /// the cursor, because the first frame after an unproven press is often the
    /// echo's own: no reconcile before it sees the row unechoed.
    tail: Option<(u16, u16)>,
    /// The cells of `tail`, one per column from its start. Resident, so a run's
    /// first press refills it in place.
    tail_cells: Vec<Seen>,
    /// The run in flight was typed at the END OF THE LINE: it started on the cell
    /// of a proof earned there (`proof_line_end`) or of a run's append retired
    /// there (`line_end_at`), or, continuing no proof, its first press saw nothing
    /// from the cursor cell to the margin with no moved tail in flight
    /// (`moved_tail_at`). Every guess of such a run is an append, whatever the far
    /// end draws right of it afterwards, so its credited echoes prove without a
    /// tail comparison (see `tail_clean`).
    ///
    /// A blank row is evidence only when nothing typed before the press can still
    /// draw the line's own text there. A press that continues a proof not at the
    /// end of the line inherits that verdict, never its snapshot's: the proof's
    /// echo may have written its glyph with the tail it shifts still to come
    /// (`o` typed over the last `o` of `abo`: the row right of the cursor reads
    /// blank until the displaced `o` lands there). And a blank snapshot within a
    /// window of a moved tail is no evidence either, for the same reason.
    ///
    /// Comparing that tail anyway voided the proof each time the far end drew
    /// text right of the cursor that is not the line: an autosuggestion the echo
    /// brought up, changed or cleared, two dark keys per change on fish,
    /// zsh-autosuggestions, PSReadLine and ble.sh, where the shipped predictor
    /// painted them where they echo. And a guess typed onto such a suggestion's
    /// first glyph (diverging from it) was typed over "other text", which a run
    /// not known to be at the end of the line must treat as mid-line. The line
    /// end is what tells them apart: no tail can move behind it.
    ///
    /// One row is all it sees. A wrapped line whose first row ends in spaces
    /// looks blank from a cursor on one of them; an insert there pushes text on
    /// the next row, which no tail comparison on this row would see either.
    tail_line_end: bool,
    /// The cell after the newest guess of a run at the end of the line that
    /// retired as an append, credited or not: where the far end's line ends,
    /// while nothing but our own appends has moved the cursor. A run started
    /// there is at the end of the line (`tail_line_end`) without a proof: the
    /// first keys after a submit are retired without credit while its echo is
    /// owed, and an autosuggestion drawn beside them must not cost the line its
    /// end. Cleared by every flush, epoch boundary and expiry.
    line_end_at: Option<(u16, u16)>,
    /// When the newest guess whose echo moved the text right of it was typed (a
    /// retirement the tail guard refused, credited or not, or a standing proof
    /// it voided). Its echo's trailing output, the tail redraw, may still be on
    /// its way for one glitch window, and until then a blank row from the cursor
    /// is no evidence that the line ends there (`tail_line_end`).
    moved_tail_at: Option<Instant>,
    /// The phase-0 decision counts the safety theorem's floors read
    /// ([`PredictEvents`]). Debug builds only.
    #[cfg(any(test, debug_assertions))]
    events: PredictEvents,
    /// The press order (`Prediction::seq`) of each guess credited with its echo
    /// since the last [`Predictor::take_credited_for_test`], newest last, the
    /// oldest dropped past `MAX_PREDICTIONS` (a debug host that never takes them
    /// keeps a bounded list): the safety theorem's ATTRIBUTION clause holds each
    /// one to its own key's echo. Debug builds only.
    #[cfg(any(test, debug_assertions))]
    credited: VecDeque<u64>,
    /// The last reconcile of an engaged chain matched a state, so the chain's
    /// base (`line`) is what the screen showed then: what the safety theorem's
    /// PROVENANCE oracle takes its base from. Debug builds only.
    #[cfg(any(test, debug_assertions))]
    chain_matched: bool,
}

impl Predictor {
    /// A predictor starting in `mode` (a default-constructed one starts `Off`).
    pub fn new(mode: PredictMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// Switch one rule off: the negative-control seam of the safety theorem (see
    /// [`Rule`]). Debug builds only; the switch survives every reset, because it
    /// describes the predictor under test, not its state.
    #[cfg(any(test, debug_assertions))]
    pub fn disable_rule_for_test(&mut self, rule: Rule) {
        self.disabled |= rule.bit();
    }

    /// How often each phase-0 decision was taken (see [`PredictEvents`]). A
    /// read-only test seam for the safety theorem's non-vacuity floors.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn events_for_test(&self) -> PredictEvents {
        self.events
    }

    /// Take the press order of every guess credited with its echo since the
    /// last take, oldest first: each one measured the link and may have proved
    /// its epoch. A test seam for the safety theorem's ATTRIBUTION clause, which
    /// follows each back to its key ([`Prediction::seq_for_test`]).
    #[cfg(any(test, debug_assertions))]
    pub fn take_credited_for_test(&mut self) -> Vec<u64> {
        self.credited.drain(..).collect()
    }

    /// Count `event` for [`PredictEvents`]; nothing in a release build.
    fn count(&mut self, event: Event) {
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
            };
            *n = n.saturating_add(1);
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            let _ = event;
        }
    }

    /// Whether `rule` is in force: always, outside a negative control.
    fn rule(&self, rule: Rule) -> bool {
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

    /// The glitch window as it stands: how long an unconfirmed guess may live and
    /// how long an owed echo keeps a guess from earning credit. A read-only test
    /// seam: the safety theorem scopes its link model by the predictor's own
    /// window, so an echo it calls late is one this predictor would also have
    /// given up on.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn glitch_window_for_test(&self) -> Duration {
        self.glitch_window()
    }

    /// The display mode in force: a host that keeps no mode of its own asks it
    /// before paying for a row read the predictor would not use (`Off`).
    #[must_use]
    pub fn mode(&self) -> PredictMode {
        self.mode
    }

    /// Apply a (possibly changed) display mode; a real change flushes in-flight
    /// guesses so a mode flip never leaves an orphaned overlay.
    pub fn set_mode(&mut self, mode: PredictMode) {
        if mode != self.mode {
            self.mode = mode;
            self.flush();
        }
    }

    /// Measure glyphs as the terminal writes them: East Asian Ambiguous ones (`é`, `°`,
    /// `→`) two cells wide when `wide`, so they are declined like CJK rather than
    /// placed one cell where the grid draws two. A real change flushes in-flight
    /// guesses, which were placed under the old width.
    pub fn set_ambiguous_width_double(&mut self, wide: bool) {
        if wide != self.ambiguous_width_double {
            self.ambiguous_width_double = wide;
            self.flush();
        }
    }

    /// The cells the terminal writes `ch` with, under the ambiguous-width policy
    /// in force ([`set_ambiguous_width_double`](Self::set_ambiguous_width_double)).
    fn cells_of(&self, ch: char) -> usize {
        if self.ambiguous_width_double {
            aterm_grapheme::char_width_cjk(ch)
        } else {
            aterm_grapheme::char_width(ch)
        }
    }

    /// Register a printable character the user just typed (and that the host wrote to
    /// the PTY), on a host that does not know its grid HEIGHT. Identical to
    /// [`predict_char_in_grid`](Self::predict_char_in_grid) with no wrap lane: a guess
    /// that would cross the right margin is declined. Prefer the grid-aware entry
    /// point: long command lines are where type-ahead matters most.
    pub fn predict_char(
        &mut self,
        ch: char,
        cursor: (u16, u16),
        coords: u64,
        cols: u16,
        now: Instant,
    ) -> bool {
        // `rows = 0` ⇒ no row is available to wrap INTO, so the margin declines.
        self.predict_char_in_grid(ch, cursor, coords, (cols, 0), now)
    }

    /// Register a printable character the user just typed. `cursor` is the live real
    /// cursor (the anchor when nothing is pending); while typing ahead, each new glyph
    /// extends from the previous prediction. `coords` is the host's content-coordinate
    /// generation, read with the cursor (see [`reconcile`](Self::reconcile)). `grid`
    /// is `(cols, rows)`: at the right margin the guess continues at column 0 of the
    /// next row, and only a guess that would fall off the BOTTOM of the grid is
    /// declined. Returns `true` if a guess was added.
    ///
    /// Refusing a PRINTABLE glyph never flushes: the guesses to the LEFT of it are
    /// still exactly what the program will echo (flushing them would erase the word
    /// typed so far). It does end the epoch (the glyph is input we did not model) and
    /// cost the anchor for everything typed AFTER it (see `anchor_lost`). A control
    /// character is refused as [`note_unmodelled_input`](Self::note_unmodelled_input):
    /// it may erase them.
    pub fn predict_char_in_grid(
        &mut self,
        ch: char,
        cursor: (u16, u16),
        coords: u64,
        grid: (u16, u16),
        now: Instant,
    ) -> bool {
        self.arm_glyph(ch, cursor, coords, grid, None, now)
    }

    /// Whether the next glyph would be guessed on the LIVE cursor (nothing is
    /// pending to extend or to wait behind), so its press should carry the cursor
    /// row ([`KeySample::row`]): the reference the tail guard compares that run's
    /// echoes with. Type-ahead costs this one check and no row read.
    ///
    /// A host that reads the row only when this says so loses nothing: a run that
    /// starts on the live cursor without it (rows moved under the guesses between
    /// this call and the press) proves only over a blank tail.
    ///
    /// It does not ask the display mode. A host asks before its press applies the
    /// mode it is about to predict in (aterm-gui asks before the terminal hold,
    /// and calls `set_mode` after it), and a predictor still `Off` from a window
    /// that never predicted, or from before `predictive_echo` was switched on,
    /// would answer no for the very press that starts its first run, which then
    /// proved only over a blank tail: every line with a right prompt stayed dark
    /// until its typed-ahead queue drained. So the mode is the host's to ask,
    /// before it reads anything: aterm-gui asks only for a glyph it is about to
    /// predict in a mode other than `Off`, and a host that keeps no mode of its
    /// own asks [`mode`](Self::mode).
    #[must_use]
    pub fn wants_key_row(&self) -> bool {
        self.preds.is_empty()
    }

    /// Register a printable character the user just typed, with what the host
    /// read under the press's terminal hold ([`KeySample`]). Identical to
    /// [`predict_char_in_grid`](Self::predict_char_in_grid), and a run started on
    /// the live cursor also takes the cursor row right of it as its tail (see
    /// `tail`), so an echo that leaves a right prompt in place still proves the
    /// line. Returns `true` if a guess was added.
    pub fn predict_glyph(&mut self, ch: char, sample: &KeySample<'_>, now: Instant) -> bool {
        self.arm_glyph(
            ch,
            sample.cursor,
            sample.coords,
            sample.grid,
            sample.row,
            now,
        )
    }

    /// [`predict_glyph`](Self::predict_glyph) and the predict entry points that
    /// wrap it: `cells` is the press's cursor row, if the host read one.
    fn arm_glyph(
        &mut self,
        ch: char,
        cursor: (u16, u16),
        coords: u64,
        grid: (u16, u16),
        cells: Option<&[Seen]>,
        now: Instant,
    ) -> bool {
        if self.mode == PredictMode::Off {
            return false;
        }
        let (cols, rows) = grid;
        if cols == 0 {
            return false; // degenerate geometry: no cell can hold a guess
        }
        self.observe_coordinates(coords);
        self.grid_cols = cols;
        // An idle host may not reconcile until the next press.
        self.observe_cancelled_cursor(Some(cursor), now);
        self.settle_chain();
        // A drained set re-anchors on the live cursor, which the real echo has moved
        // past whatever we declined, so the block on extending lifts here.
        if self.preds.is_empty() {
            self.anchor_lost = false;
        }
        // A glyph typed while a state chain is in flight joins it: it lands where
        // the chain's last state leaves the cursor, which no glyph-lane anchor
        // knows (a motion moves no cell a guess could extend from).
        if self.engaged() {
            return self.arm_in_chain(ch, cursor, cols, now);
        }
        // The known line's cursor is inside its text (a motion retired there):
        // a glyph typed now is a MID-LINE insert, whose echo shifts the text right
        // of it. The proof covers the cell the cursor stands on, not an append
        // there, so it goes, with the line it named; the glyph is tracked dark
        // by the glyph lane, whose tail guard then refuses it any proof (typed
        // over other text). Kept, the proof painted the glyph over the cell
        // under the cursor without shifting the tail (`ls --la` shown for
        // `ls - -la`).
        if self.preds.is_empty()
            && self
                .line
                .as_ref()
                .is_some_and(|l| l.epoch == self.epoch && l.cursor < l.hi())
        {
            self.void_proof();
        }
        // Only single-width, self-echoing printables — width is the criterion, not
        // ASCII-ness: `é`/`ü`/`ñ` occupy exactly one cell and echo like any other
        // letter, while control (ESC sequences), zero-width (combining marks, which
        // modify the cell to their LEFT) and wide (CJK, emoji) glyphs have echo
        // geometry we cannot place. Refused glyphs are left to the real output.
        let width = self.cells_of(ch);
        if ch.is_control() || width != 1 || self.anchor_lost || self.preds.len() >= MAX_PREDICTIONS
        {
            return self.decline(ch, now);
        }
        // Anchor: extend the last pending guess OF THIS EPOCH, else the live cursor.
        // A pre-submit guess still in flight belongs to the previous logical line and
        // must not anchor the new one.
        let anchor = self
            .preds
            .iter()
            .rev()
            .find(|p| p.epoch == self.epoch)
            .copied();
        // …and while such a guess IS in flight, the live cursor is no anchor either.
        // Reconciliation holds the real cursor ON the oldest unechoed guess, so a
        // glyph typed after a line terminator but before the line's echo would be
        // armed on the very cell a carried guess occupies: in `Always` both paint
        // there (the new line's text over the old line's tail, `ls⏎cd` showing
        // `cd`), and in every mode the carried guess's own echo then CONFIRMS the
        // new one by coincidence (`ls⏎ls`), arming the new line's display gate with
        // a proof its own keystrokes never earned and painting what follows on the
        // submitted line. Where this glyph lands is decided by the terminator's echo,
        // which nothing here models, so it is declined like any glyph we cannot
        // place; the carried guesses keep their pixels, and the drained set
        // re-anchors on the live cursor (`anchor_lost`).
        //
        // `reconciled_view_safety.rs` is the theorem this keeps true: no two
        // displayed guesses share a cell, and a displayed guess is where its echo
        // lands.
        if anchor.is_none() && !self.preds.is_empty() {
            return self.decline(ch, now);
        }
        // With nothing of the line pending, a guess a Backspace just cancelled
        // left its cell as the anchor, not the live cursor its echo may have moved
        // or parked (`cancelled_at`).
        let cancelled = self.cancelled_at.take().filter(|_| anchor.is_none());
        let live = cancelled.unwrap_or(cursor);
        // A guess only displays (Adaptive) after an echo is confirmed for the cursor
        // it extends. Anchored on the live cursor, that is the cell the last
        // confirmed echo left it in (`proof_cell`); a cursor anywhere else was moved
        // by the program — a new row, a prompt printed on the same line — and what
        // is typed there has proven nothing, so a no-echo prompt never inherits the
        // confirmation and never shows an unechoed glyph.
        if anchor.is_none() && self.proof_cell != Some(live) {
            self.void_proof();
        }
        // …and one it does cover still stands only on the tail it was earned
        // over (THE STANDING TAIL, `proof_echo`). This press is about to
        // re-snapshot the row, so it compares its own row with the snapshot
        // first: once an echo's glyph proved before its tail redraw landed, the
        // row this press reads is the moved one, and a snapshot of it would let
        // every later comparison pass. (A guess anchored on a cancelled cell is
        // in the cancelled pair's transit, which the next reconcile reads.)
        if anchor.is_none()
            && cancelled.is_none()
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
        // A run that continues a proof takes that proof's verdict on the end of
        // the line (`proof_line_end`), whatever its own row shows; one that starts
        // where a run's append left the line's end inherits it (`line_end_at`);
        // any other is judged by its snapshot (`take_tail`).
        let inherits_line_end = if self.confirmed_epoch {
            Some(self.proof_line_end)
        } else if self.line_end_at == Some(live) {
            Some(true)
        } else {
            None
        };
        let (row, col) = match anchor {
            Some(p) => (p.row, p.col.saturating_add(1)),
            // The cancelled guess's cell is a real cell, already placed (wrapped
            // or not) when that guess was armed.
            None if cancelled.is_some() => live,
            // The live cursor is a free cell ONLY if the terminal is not parked in the
            // deferred-wrap slot (see `deferred_wrap`): there the cell under the cursor
            // is already echoed content and the next glyph lands on the next row. Hand
            // the shared wrap lane below an out-of-range column so the wrap — and the
            // bottom-edge decline — is decided in exactly one place.
            None if self.deferred_wrap && cursor.1.saturating_add(1) == cols => (cursor.0, cols),
            None => cursor,
        };
        let (row, col) = if col >= cols {
            // The line wraps. The next glyph lands at column 0 of the following row —
            // that is what the program will echo, so predicting it is no more
            // speculative than any other guess. Only the bottom edge is unmodellable
            // (the grid scrolls, moving every pending guess), and there we decline
            // WITHOUT flushing: the guesses to the left are still correct.
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
            // A STALE ANCHOR: an echo owed now may still move the cursor this run
            // is placed from (`Prediction::stale`).
            let window = self.glitch_window();
            self.run_stale = self
                .owed_at
                .is_some_and(|t| now.saturating_duration_since(t) <= window);
        }
        self.push_op(row, col, ch, Op::Insert, OpClass::Glyph, now);
        if let Some(p) = self.preds.back_mut() {
            p.stale = self.run_stale;
        }
        true
    }

    /// Arm one op at `(row, col)`: the newest pending key, owing nothing of its
    /// own yet, and owed every keystroke owed so far (each was typed before it).
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
        });
        self.next_seq = self.next_seq.wrapping_add(1);
        // A Backspace now erases this op first (see `declined_tail`).
        self.declined_tail = 0;
    }

    /// Register a Backspace. We cancel only our OWN trailing prediction of the current
    /// epoch (the common "type then immediately fix" case). Returns `true` if a guess
    /// was cancelled.
    ///
    /// With nothing of ours to cancel, the Backspace erases content the program
    /// already echoed (or a guess carried across a line terminator, which we can no
    /// longer tell apart from what the terminator wrote): input we did not model,
    /// handled as [`note_unmodelled_input`](Self::note_unmodelled_input). The real
    /// cursor will step LEFT once that erase echoes, so the live cursor is stale and
    /// a glyph typed meanwhile is guessed one cell right of where it lands — and
    /// after the echo the cursor sits exactly on that guess (erase -1, glyph +1), so
    /// reconciliation cannot catch it. Ending the epoch keeps that guess dark until
    /// an echo is confirmed again.
    ///
    /// One more thing it can erase is ours to account for: a glyph we DECLINED
    /// since the newest guess (`declined_tail`). That erase cancels the declined
    /// glyph's echo and touches nothing to its left, so every guess in flight keeps
    /// its pixels; only the pair's echoes are owed.
    pub fn predict_backspace(&mut self, now: Instant) -> bool {
        if self.mode == PredictMode::Off {
            return false;
        }
        // Only OUR trailing guess on the CURRENT line: a guess left in flight by the
        // previous line's submit is not what this Backspace erases. (A declined
        // glyph after it would have ended its epoch, so nothing declined is newer
        // than a guess popped here.)
        //
        // The glyph's echo and the erase's echo cancel, leaving the cursor where
        // the guesses before it say, so the pair is owed nothing; but they arrive
        // one after the other, and between them the screen runs a cell ahead: the
        // cancelled glyph drawn, the cursor past it, or, when it filled the last
        // column, the terminal parked in the deferred-wrap slot with the cursor
        // where it was. So the next guess that finds nothing of its line pending
        // anchors on the CANCELLED CELL (`cancelled_at`), where the cursor returns
        // once the erase lands, not on the live cursor. Anchored there it echoes
        // where it is painted; anchored on the live cursor it was placed a cell
        // right, the next key's echo then stood in its cell (a space confirms by
        // the cursor advancing past it) and its credit proved the epoch a cell
        // behind, or, parked, it matched the proof's cell and was wrapped to the
        // next row while it echoes in the last column (the theorem's FIDELITY
        // clause, on seeds 13 and 39 of the sweep).
        //
        // The cancelled glyph's echo is still on its way, to that very cell, so
        // it is OWED, typed as its glyph (`glyph_owes`): a guess typed next for
        // the same glyph, anchored there, sees it land in its cell with the
        // cursor past, and was credited with the cancelled key's echo (a short
        // sample, a proof earned on another key's output: the theorem's
        // ATTRIBUTION clause, five ops). Any other glyph there sees a different
        // glyph and flushes. Typed, it refuses nothing else, and the model, which
        // gives typed owes no projection, still holds no owe beside a proof.
        //
        // Only a typed glyph pops: an edit op of a state chain (a motion, an
        // erase) is not erased by a Backspace, which erases text at the cursor
        // that op left (`predict_edit` arms that erase where it can).
        if let Some(p) = self
            .preds
            .back()
            .copied()
            .filter(|p| p.epoch == self.epoch && p.op == Op::Insert)
        {
            self.preds.pop_back();
            if self.rule(Rule::CancelTransit) {
                self.cancelled_at = Some((p.row, p.col));
            }
            if self.rule(Rule::CancelOwes) {
                self.owe_glyph(p.seq, p.ch, p.born, now);
            }
            self.cancelled_pair = Some((p.row, p.col, p.ch, self.next_seq));
            self.settle_chain();
            return true;
        }
        // The erase's echo, and whatever it redraws, is output no guess accounts for.
        self.owe(now);
        if let Some(rest) = self.declined_tail.checked_sub(1) {
            // It erases the newest declined glyph, typed after every guess in
            // flight: they still echo where they are painted. The epoch already
            // ended at the decline, and the anchor stays lost until the set drains.
            self.declined_tail = rest;
            return false;
        }
        self.flush();
        false
    }

    /// Switch the edit lane on or off ([`predict_edit`](Self::predict_edit)):
    /// off, every edit key is routed as it was before the lane existed (a
    /// Backspace through [`predict_backspace`](Self::predict_backspace), an arrow
    /// as [`note_unmodelled_input`](Self::note_unmodelled_input)). On by default.
    /// The kill switch, and a measurement's baseline. A real change flushes.
    pub fn set_editing(&mut self, on: bool) {
        if on == self.editing_off {
            self.editing_off = !on;
            self.flush();
        }
    }

    /// Register a line-editing key the user just pressed (and that the host wrote
    /// to the PTY), with what the host read under the press's terminal hold, the
    /// cursor row included ([`KeySample::row`]): `←`, `→`, or Backspace.
    ///
    /// THE EDIT LANE. A key whose effect on the line is known is armed as an op
    /// of a STATE CHAIN on the known line (`line`): `←` and `→` move the cursor
    /// inside the text, a Backspace at the end of the text erases its last glyph.
    /// The chain's states are the line folded through each op in turn
    /// (`fold_op`); a reconcile retires the ops up to the smallest state the
    /// screen matches (`reconcile_chain`), and a frame paints the state its
    /// visible ops leave (`overlay`, `overlay_cursor`). Only an echo proves
    /// anything: an op is placed only on a line this epoch has watched echo and
    /// proved (`placeable`), and a motion shows only once a motion of the same
    /// epoch has echoed exactly (`op_visible`).
    ///
    /// Every key it cannot place still costs only display:
    /// * a Backspace that finds our own trailing glyph pending cancels it, as
    ///   [`predict_backspace`](Self::predict_backspace) always has;
    /// * an arrow that cannot be placed (no line, the anchor lost, a carried line,
    ///   the cursor not where the line's last verified state left it, at an edge
    ///   of the text) is DECLINED WITH ITS PIXELS KEPT (`decline_motion`): it is
    ///   owed, the epoch ends, and nothing is armed until the queue drains, but
    ///   every guess in flight still echoes where it is painted, since the far
    ///   end reads the arrow after them and no motion erases text;
    /// * a Backspace that cannot be placed is
    ///   [`predict_backspace`](Self::predict_backspace)'s (owed, and the guesses
    ///   retired unless it erases a glyph we declined).
    ///
    /// The host never also calls [`note_unmodelled_input`](Self::note_unmodelled_input)
    /// for the key: every outcome has accounted for it.
    pub fn predict_edit(
        &mut self,
        key: EditKey,
        sample: &KeySample<'_>,
        now: Instant,
    ) -> EditOutcome {
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
        if self.editing_off {
            return match key {
                EditKey::Backspace if self.predict_backspace(now) => EditOutcome::Cancelled,
                EditKey::Backspace => EditOutcome::Declined,
                EditKey::Left | EditKey::Right => {
                    self.note_unmodelled_input(now);
                    EditOutcome::Declined
                }
            };
        }
        // Our own trailing glyph first: the pair's echoes cancel, whatever lane.
        if key == EditKey::Backspace
            && self
                .preds
                .back()
                .is_some_and(|p| p.epoch == self.epoch && p.op == Op::Insert)
        {
            let _popped = self.predict_backspace(now);
            return EditOutcome::Cancelled;
        }
        let end = self
            .placeable(sample.cursor)
            .filter(|e| self.press_row_clear(sample.row, e, key == EditKey::Backspace));
        let edges = self.rule(Rule::TextEdges);
        let armed = end.and_then(|e| match key {
            EditKey::Left if e.cursor > e.lo || !edges => Some((e, Op::Left, OpClass::Motion)),
            EditKey::Right if e.cursor < e.hi || !edges => Some((e, Op::Right, OpClass::Motion)),
            // Only at the end of the text: an erase inside it shifts the text
            // right of the cursor, which this lane does not move.
            EditKey::Backspace if e.cursor > e.lo && e.cursor == e.hi => {
                Some((e, Op::Erase, OpClass::Glyph))
            }
            _ => None,
        });
        let Some((e, op, class)) = armed.filter(|_| self.preds.len() < EDIT_CHAIN_MAX) else {
            return match key {
                EditKey::Left | EditKey::Right => self.decline_motion(now),
                EditKey::Backspace if self.predict_backspace(now) => EditOutcome::Cancelled,
                EditKey::Backspace => EditOutcome::Declined,
            };
        };
        if !self.engaged() {
            // A chain starts: nothing the last chain's reconcile read describes
            // this one's line (`compose_chain`).
            self.seen.clear();
        }
        self.chain = true;
        // The op anchors where the chain's last state leaves the cursor, which a
        // cancelled glyph's cell is when nothing else is pending.
        self.cancelled_at = None;
        self.push_op(e.row, e.cursor, EDIT_OP_CH, op, class, now);
        self.count(Event::EditArmed);
        EditOutcome::Armed
    }

    /// An arrow that cannot be placed (`predict_edit`): DECLINED WITH ITS PIXELS
    /// KEPT. The far end reads it after every key in flight, and in the family no
    /// motion erases text, so every guess in flight still echoes where it is
    /// painted; flushing them, as every unmodelled key did, blinked out a
    /// typed-ahead tail at each arrow. What the arrow does is not ours to place
    /// (readline beeps at an edge, zsh does nothing, an autosuggestion is
    /// accepted), so it is OWED ([`Rule::EditOpsOwe`]), the epoch ends, and
    /// nothing is armed until the queue drains and the live cursor re-anchors.
    fn decline_motion(&mut self, now: Instant) -> EditOutcome {
        if self.rule(Rule::EditOpsOwe) {
            self.owe(now);
        }
        self.end_epoch();
        self.anchor_lost = true;
        self.count(Event::DeclineMotion);
        EditOutcome::Declined
    }

    /// A state chain is in flight (see `chain`).
    fn engaged(&self) -> bool {
        self.chain && !self.preds.is_empty()
    }

    /// The queue drained: a chain that was in flight is over, and its line
    /// survives only if the glyph proof of this epoch still covers it; a fresh
    /// chain on it starts with no vacated cells (they were verified blank, and a
    /// later chain's states never held them as text).
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

    /// Where the next edit key applies, if it can be placed at all (R-LANE): the
    /// edit lane is on and the host says which cells are opaque; the anchor is
    /// not lost; this epoch's glyph proof stands, with the known line it covers;
    /// every pending op is of this epoch, on the line's row, each typed where
    /// the ops before it left the cursor (a glyph-lane guess that wrapped, or
    /// reached the last column, is not); and with nothing pending, the live
    /// cursor is where the line's last verified state left it (the program has
    /// not moved it since). The pending ops fold onto the line by their lengths
    /// and the cursor alone.
    fn placeable(&self, cursor: (u16, u16)) -> Option<ChainEnd> {
        if self.editing_off || !self.edit_capable || self.anchor_lost || !self.confirmed_epoch {
            return None;
        }
        let l = self.line.as_ref().filter(|l| l.epoch == self.epoch)?;
        if self.preds.is_empty() && cursor != (l.row, l.cursor) {
            return None;
        }
        let mut end = ChainEnd {
            row: l.row,
            lo: l.lo,
            hi: l.hi(),
            cursor: l.cursor,
            vacated: l.vacated.max(l.hi()),
        };
        for p in &self.preds {
            if p.epoch != self.epoch || p.row != end.row || p.col != end.cursor {
                return None;
            }
            match p.op {
                // R-ROW: a glyph never reaches the last column (the deferred
                // wrap's slot), so the line never leaves its row.
                Op::Insert
                    if end.cursor == end.hi
                        && end
                            .hi
                            .checked_add(2)
                            .is_some_and(|after| after <= self.grid_cols) =>
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
            }
            end.vacated = end.vacated.max(end.hi);
        }
        Some(end)
    }

    /// Whether the press's row lets an edit key be armed at `end`: read at all
    /// (a host that hands no row arms none); no opaque cell from `lo` to one
    /// past the furthest end (R-OPAQUE: a wide glyph's half, marks or a
    /// reordered row is nothing a state can name); and for an erase, every cell
    /// from the text's end to one past the furthest end blank, or our own text
    /// there not yet erased on screen ([`Rule::VacateRefusal`]): anything else
    /// there (an autosuggestion, the buffer's unseen tail) is what the editor
    /// redraws over, or pulls into, the cell the erase vacates.
    fn press_row_clear(&mut self, row: Option<&[Seen]>, end: &ChainEnd, erase: bool) -> bool {
        let Some(cells) = row else {
            return false;
        };
        let at = |c: u16| cells.get(usize::from(c)).copied().unwrap_or(Seen::Blank);
        let span_end = end.vacated.saturating_add(1).min(self.grid_cols);
        if (end.lo..span_end).any(|c| matches!(at(c), Seen::Opaque(_))) {
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

    /// Whether `seen` at column `c` is text the chain has held there: the line's
    /// verified text, or a pending glyph's, on screen until an erase in flight
    /// lands.
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

    /// A glyph typed while a state chain is in flight (`arm_glyph`): an insert at
    /// the end of the chain's last state, where its cursor stands. Inside the
    /// text it would be a MID-LINE insert, which shifts the text right of it, and
    /// this lane moves no cell: the pixels of the state the chain shows would
    /// stand over a tail about to move, so it is owed and the chain flushed. A
    /// glyph the chain cannot place for its width, the margin (R-ROW) or a lost
    /// anchor is declined as the glyph lane declines one: it inserts after every
    /// key in flight, so their pixels stay.
    fn arm_in_chain(&mut self, ch: char, cursor: (u16, u16), cols: u16, now: Instant) -> bool {
        if ch.is_control() {
            return self.decline(ch, now);
        }
        let end = self.placeable(cursor);
        match end {
            Some(e) if e.cursor < e.hi => {
                self.owe(now);
                self.flush();
                false
            }
            Some(e)
                if self.cells_of(ch) == 1
                    && e.hi.checked_add(2).is_some_and(|after| after <= cols)
                    && self.preds.len() < EDIT_CHAIN_MAX =>
            {
                self.cancelled_at = None;
                self.push_op(e.row, e.cursor, ch, Op::Insert, OpClass::Glyph, now);
                true
            }
            _ => self.decline(ch, now),
        }
    }

    /// `p`, a glyph of the current epoch typed as an append, was credited with
    /// its echo and proved its epoch (`retire_echoed`): its cell is verified
    /// text at the end of the line, which extends the known line, or starts it
    /// there when the line it would extend is not the one it continues. A glyph
    /// at the last column ends the line instead (R-ROW: a state chain never
    /// reaches the deferred wrap's slot).
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
        if let Some(l) = self
            .line
            .as_mut()
            .filter(|l| l.epoch == epoch && l.row == p.row && l.cursor == p.col && l.hi() == p.col)
        {
            l.text.push(p.ch);
            l.cursor = l.cursor.saturating_add(1);
            l.vacated = l.vacated.max(l.cursor);
            return;
        }
        let mut text = self.line.take().map(|l| l.text).unwrap_or_default();
        text.clear();
        text.push(p.ch);
        let next = p.col.saturating_add(1);
        self.line = Some(EditLine {
            row: p.row,
            lo: p.col,
            epoch,
            text,
            cursor: next,
            vacated: next,
        });
    }

    /// Apply op `p` to the state `(text, cursor)` of a line starting at `lo`:
    /// THE FOLD, the one definition every state of a chain comes from (the
    /// safety theorem's PROVENANCE oracle implements it again, independently).
    ///
    /// * a glyph at the cursor: inserted there, the cursor one on;
    /// * `←`: the cursor one back; `→`: one on;
    /// * an erase: the glyph before the cursor removed, the cursor on its cell.
    fn fold_op(text: &mut Vec<char>, cursor: &mut u16, lo: u16, p: &Prediction) {
        let at = usize::from(cursor.saturating_sub(lo));
        match p.op {
            Op::Insert => {
                if at <= text.len() {
                    text.insert(at, p.ch);
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
        }
    }

    /// One past the last cell of a state's text starting at `lo`.
    fn text_end(lo: u16, text: &[char]) -> u16 {
        lo.saturating_add(u16::try_from(text.len()).unwrap_or(u16::MAX))
    }

    /// The furthest text end any state of the chain on `line` reaches, the
    /// base's vacated cells included, by lengths alone.
    fn chain_far_end(&self, line: &EditLine) -> u16 {
        let mut hi = line.hi();
        let mut far = line.vacated.max(hi);
        for p in &self.preds {
            hi = match p.op {
                Op::Insert => hi.saturating_add(1),
                Op::Erase => hi.saturating_sub(1),
                Op::Left | Op::Right => hi,
            };
            far = far.max(hi);
        }
        far
    }

    /// Reconcile a STATE CHAIN against the screen (`reconcile_seen`).
    ///
    /// 1. Read its row from `lo` to one past the furthest text end any state
    ///    reaches, into `seen`. An opaque cell there flushes (R-OPAQUE). With
    ///    an erase pending, so does anything but a blank in that last cell, the
    ///    SENTINEL no state compares ([`Rule::VacateRefusal`]): a split erase
    ///    echo may still show the chain's own vacated text, which step 6 waits
    ///    through, but nothing past every state is the chain's.
    /// 2. Find the SMALLEST state `S_j`, `S_0` the line itself included, that
    ///    the screen matches as a whole (`state_matches`): its text, its
    ///    vacated cells blank, and the real cursor where it leaves it
    ///    ([`Rule::SmallestMatch`]). If the far end is the editor the chain
    ///    models, the screen after it has read `m` keys is `S_m`, so a match at
    ///    `j` means at least `j` keys were read, whatever later state the screen
    ///    also equals: retiring at the smallest never retires an op whose echo
    ///    has not come.
    /// 3. Retire the ops up to it, front first (`retire_chain`): credited, each
    ///    measures the link and moves the proof's cell to the cursor its state
    ///    leaves; an op not attributable (an echo owed from before it may be what
    ///    the screen shows) flushes the chain, whose base is then no longer the
    ///    screen's for certain. The line becomes `S_j`.
    /// 4. A NET-NULL tail (`S_j = S_n`, as after `←→` or a glyph and its erase
    ///    in one burst) retires without credit, owing each key
    ///    ([`Rule::NetNullUncredited`]): nothing on glass changes, and left
    ///    pending it would wait for a state the screen never leaves and expire.
    /// 5. With an erase still pending, the cells from the line's end to one past
    ///    the furthest end must be blank ([`Rule::VacateRefusal`]).
    /// 6. No state matches: if every compared cell is blank or holds what some
    ///    state comparing it holds, and the cursor is on the line's row, the
    ///    screen is part-way through an echo (an ICH before its glyph, a tail
    ///    rewrite, a cancelled pair) and the chain WAITS, as long as its head's
    ///    window lets it; anything else is a divergence, and flushes.
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
            // A chain with no base cannot be placed anywhere.
            self.flush();
            return;
        };
        let span_end = far.saturating_add(1).min(self.grid_cols);
        self.seen.clear();
        for c in lo..span_end {
            let seen = observe(row, c);
            if matches!(seen, Seen::Opaque(_)) {
                self.flush();
                return;
            }
            self.seen.push(seen);
        }
        // A split echo may still hold this chain's own vacated text. The
        // sentinel past every state is never ours, even while no state matches.
        if self.preds.iter().any(|p| p.op == Op::Erase)
            && self.rule(Rule::VacateRefusal)
            && self.seen.get(usize::from(far.saturating_sub(lo))) != Some(&Seen::Blank)
        {
            self.count(Event::VacateRefusal);
            self.flush();
            return;
        }
        let Some(j) = self.match_chain(cursor) else {
            if self.in_transit(cursor) {
                self.count(Event::TransitWait);
            } else {
                self.flush();
            }
            return;
        };
        if !self.retire_chain(j, now, oldest) {
            return;
        }
        #[cfg(any(test, debug_assertions))]
        {
            self.chain_matched = true;
        }
        self.retire_net_null(now, oldest);
        let erase_pending = self.preds.iter().any(|p| p.op == Op::Erase);
        let hi = self.line.as_ref().map_or(lo, EditLine::hi);
        let clear = self
            .seen
            .get(usize::from(hi.saturating_sub(lo))..)
            .is_none_or(|rest| rest.iter().all(|&s| s == Seen::Blank));
        if erase_pending && self.rule(Rule::VacateRefusal) && !clear {
            self.count(Event::VacateRefusal);
            self.flush();
        }
    }

    /// Whether the screen `seen` (read from `lo`) and the real `cursor` show the
    /// state `(text, state_cursor)` of a chain on `row`, whose vacated cells run
    /// to `vacated`: the cursor where the state leaves it, and, as a WHOLE
    /// ([`Rule::WholeStateMatch`]), every cell of its text holding that text and
    /// every vacated cell blank ([`Rule::VacatedBlankInMatch`]). Cells past
    /// `vacated` are no state's: a right prompt, an autosuggestion, never compared.
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

    /// The state of the chain the screen matches (`reconcile_chain`, step 2):
    /// the smallest, or with [`Rule::SmallestMatch`] off the largest.
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
        for j in 0..=self.preds.len() {
            if let Some(p) = j.checked_sub(1).and_then(|i| self.preds.get(i)) {
                Self::fold_op(&mut text, &mut c, lo, p);
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
        found
    }

    /// Retire the first `j` ops of the chain, front first, onto the line
    /// (`reconcile_chain`, step 3). `false` when one was not attributable and the
    /// chain was flushed.
    fn retire_chain(&mut self, j: usize, now: Instant, oldest: &mut Option<Duration>) -> bool {
        let Some(mut line) = self.line.take() else {
            return false;
        };
        for _ in 0..j {
            let Some(p) = self.preds.pop_front() else {
                break;
            };
            Self::fold_op(&mut line.text, &mut line.cursor, line.lo, &p);
            line.vacated = line.vacated.max(line.hi());
            if self
                .cancelled_pair
                .is_some_and(|(_, _, _, after)| p.seq >= after)
            {
                self.cancelled_pair = None;
            }
            if !self.attributable(&p, now) {
                // The screen may show another key's echo where this op's state
                // is, and this op's own is still to land: the base is no longer
                // the screen's for certain. Its key is owed with every other.
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
            self.credit_op(&p, (line.row, line.cursor));
        }
        self.line = Some(line);
        true
    }

    /// The credited op `p` of a chain left the cursor at `cell`. A glyph of the
    /// current epoch proves it, as a glyph-lane append does. A motion or an
    /// erase never earns the glyph proof ([`Rule::MotionsNeverProve`]): an
    /// unechoed line never shows a glyph because of an arrow's echo. Where the
    /// glyph proof stands it moves the proof's cell to the cursor, and a motion
    /// proves its class.
    fn credit_op(&mut self, p: &Prediction, cell: (u16, u16)) {
        if p.epoch != self.epoch {
            return;
        }
        match p.op {
            Op::Insert => {
                self.confirm(*p);
                self.proof_cell = Some(cell);
            }
            Op::Left | Op::Right | Op::Erase => {
                if !self.confirmed_epoch {
                    if self.rule(Rule::MotionsNeverProve) {
                        return;
                    }
                    self.confirm(*p);
                }
                self.proof_cell = Some(cell);
                if p.class == OpClass::Motion && self.proven & PROVEN_MOTION == 0 {
                    self.proven |= PROVEN_MOTION;
                    self.count(Event::MotionProven);
                }
            }
        }
        // The line's end may have moved: a run started there is judged afresh.
        self.line_end_at = None;
    }

    /// `reconcile_chain`, step 4: the ops left pending when the matched state is
    /// also the last state retire, without credit and owing each key, and the
    /// glyph proof goes (an owe beside a proof is no proof): the screen shows
    /// their state whether or not the far end has read them.
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
        self.fold_text = text;
        if !null {
            return;
        }
        if !self.rule(Rule::NetNullUncredited) {
            // The rule off: credited, as if their echoes had come.
            let cell = (line.row, line.cursor);
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
                self.credit_op(&p, cell);
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

    /// `reconcile_chain`, step 6: whether the screen is part-way through an echo
    /// of the chain. Every cell some state compares is blank or holds what one of
    /// the states comparing it holds there, and the real cursor is on the line's
    /// row (readline's redisplay may pass it through the prompt).
    fn in_transit(&mut self, cursor: Option<(u16, u16)>) -> bool {
        let Some(line) = self.line.as_ref() else {
            return false;
        };
        if cursor.map(|(r, _)| r) != Some(line.row) {
            return false;
        }
        let lo = line.lo;
        // Per cell: 0 compared by no state yet, 1 compared and unexplained, 2
        // blank or explained by a state that compares it.
        let mut marks = std::mem::take(&mut self.ghost_marks);
        marks.clear();
        marks.extend(
            self.seen
                .iter()
                .map(|&s| if s == Seen::Blank { 2 } else { 0 }),
        );
        let mut text = std::mem::take(&mut self.fold_text);
        text.clear();
        text.extend_from_slice(&line.text);
        let mut c = line.cursor;
        let mut vacated = line.vacated.max(line.hi());
        for j in 0..=self.preds.len() {
            if let Some(p) = j.checked_sub(1).and_then(|i| self.preds.get(i)) {
                Self::fold_op(&mut text, &mut c, lo, p);
                vacated = vacated.max(Self::text_end(lo, &text));
            }
            for (i, mark) in marks
                .iter_mut()
                .enumerate()
                .take(usize::from(vacated.saturating_sub(lo)))
            {
                if *mark == 2 {
                    continue;
                }
                let held = text
                    .get(i)
                    .is_some_and(|&ch| self.seen.get(i).is_some_and(|&s| cell_holds(s, ch)));
                *mark = if held { 2 } else { 1 };
            }
        }
        let transit = !marks.contains(&1);
        self.fold_text = text;
        self.ghost_marks = marks;
        transit
    }

    /// The cells an engaged chain's overlay paints (`overlay`): the state its
    /// visible ops leave, from `lo` to the furthest text end any of those states
    /// reaches, wherever it differs from the screen the last reconcile read (a
    /// cell it did not read counts as different): its text, and a blank on each
    /// cell it vacated. Its cursor goes to `ghost_cursor`, unless the chain is
    /// carried across a terminator and [`Rule::CarryCursor`] is off.
    fn compose_chain(&mut self) {
        self.ghosts.clear();
        let visible = self.visible_count();
        let Some(line) = self.line.as_ref().filter(|_| visible > 0) else {
            return;
        };
        let (row, lo) = (line.row, line.lo);
        let mut text = std::mem::take(&mut self.fold_text);
        text.clear();
        text.extend_from_slice(&line.text);
        let mut c = line.cursor;
        let mut far = line.hi();
        let mut newest = None;
        for p in self.preds.iter().take(visible) {
            Self::fold_op(&mut text, &mut c, lo, p);
            far = far.max(Self::text_end(lo, &text));
            newest = Some(*p);
        }
        if let Some(newest) = newest {
            for col in lo..far {
                let i = usize::from(col - lo);
                let want = text.get(i).copied();
                let shown = match (want, self.seen.get(i)) {
                    (Some(ch), Some(&s)) => cell_holds(s, ch),
                    (None, Some(&s)) => s == Seen::Blank,
                    (_, None) => false,
                };
                if !shown {
                    self.ghosts.push(Prediction {
                        row,
                        col,
                        ch: want.unwrap_or(' '),
                        op: Op::Insert,
                        class: OpClass::Glyph,
                        ..newest
                    });
                }
            }
            let carried = newest.epoch != self.epoch;
            if !carried || self.rule(Rule::CarryCursor) {
                self.ghost_cursor = Some((row, c));
            }
        }
        self.fold_text = text;
    }

    /// The known line `(row, lo, end of text, cursor)`: the base a state chain
    /// folds onto. A read-only test seam for the safety theorem's PROVENANCE
    /// oracle.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn edit_line_for_test(&self) -> Option<(u16, u16, u16, u16)> {
        self.line.as_ref().map(|l| (l.row, l.lo, l.hi(), l.cursor))
    }

    /// Whether the last reconcile retired a state chain to a state the whole
    /// screen matched (its line is then what the screen showed): where the
    /// safety theorem's PROVENANCE oracle takes its base. A read-only test seam.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn chain_matched_for_test(&self) -> bool {
        self.chain_matched
    }

    /// Whether a state chain is in flight, so the overlay is its displayed state
    /// rather than the glyph lane's guesses. A read-only test seam.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn engaged_for_test(&self) -> bool {
        self.engaged()
    }

    /// How many leading pending ops pass the display gate. A read-only test seam.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn visible_ops_for_test(&self) -> usize {
        self.visible_count()
    }

    /// A line terminator reached the program, and nothing else: Enter (CR, and
    /// Shift+Enter's LF), Ctrl+J, Ctrl+M, or text ending in one ([`is_line_submit`]).
    /// Ends the confirmation epoch, so nothing typed after it displays until an echo
    /// is confirmed again — mosh's "become tentative".
    ///
    /// LOAD-BEARING for the no-unechoed-flash guarantee: the proof is bound to the
    /// cell the last echo left the cursor in (`proof_cell`), and a password typed
    /// AHEAD of the prompt that follows a submit is typed at that very cell until the
    /// remote answers. Which keys submit is the program's business (readline accepts
    /// a line on CR, LF and Ctrl+O alike), so every other press we did not model ends
    /// the epoch too ([`note_unmodelled_input`](Self::note_unmodelled_input)).
    ///
    /// The one boundary that keeps the guesses in flight on glass: a terminator
    /// inserts nothing before them and erases none of them, so each still echoes
    /// exactly where it is painted, and flushing them would blink out the tail of the
    /// command line at the moment of commit. A proven epoch's guesses keep their
    /// pixels until they retire through [`reconcile`](Self::reconcile) or expire at
    /// the glitch window; a stale guess confirming later cannot arm the new epoch
    /// (see `Prediction::epoch`).
    ///
    /// `now` is the press: the terminator's echo is owed from then (see `owed_at`).
    pub fn note_line_submit(&mut self, now: Instant) {
        // The terminator's own echo (and what the program prints for it — a
        // prompt's default answer, a command's first glyph) is output no guess
        // accounts for.
        self.owe(now);
        self.end_epoch();
        // A Backspace after it erases on the NEW line, never a glyph declined on
        // the line it submitted.
        self.declined_tail = 0;
    }

    /// Input this crate does not model reached the program, other than a line
    /// terminator ([`note_line_submit`](Self::note_line_submit)): any byte-writing
    /// press that is not a predicted glyph (Tab, an arrow, a control or Alt chord, a
    /// remapped key sequence, an IME commit) or a paste. Ends the confirmation epoch
    /// and retires every guess in flight, in every mode.
    ///
    /// Retired, not carried, because we cannot say where their echo lands any more:
    /// the input may erase them. A backward erase (Ctrl+H, Ctrl+W, a `[key_sequences]`
    /// kill, vi's `x`) that removes exactly the typed-ahead text leaves the real
    /// cursor on one of its guesses over a blank cell, indistinguishable from "not
    /// echoed yet", and a guess left there would stay on glass as a ghost of the
    /// erased text for the whole glitch window. Failing closed costs one round trip
    /// of a typed-ahead tail after a Tab or an arrow; the echo then draws it.
    ///
    /// It is also the seam for a press the host REFUSED to hand over as a guess:
    /// a glyph or Backspace at a no-echo gate (the alternate screen, an
    /// app-owned keyboard mode, a tty with ECHO clear, as the host sampled it at
    /// the key). The program may still echo that key — it can leave the alternate
    /// screen before reading it — so it is owed like any other input we did not
    /// place, and anything stale from before the gate goes with the flush.
    ///
    /// `now` is the press: its echo is owed from then (see `owed_at`).
    pub fn note_unmodelled_input(&mut self, now: Instant) {
        // The input's own echo is owed, and so is every guess `flush` retires:
        // their keystrokes reached the program.
        self.owe(now);
        // `flush` ends the epoch with the guesses: nothing is left to carry.
        self.flush();
    }

    /// End the confirmation epoch at a line terminator (see
    /// [`note_line_submit`](Self::note_line_submit)).
    fn end_epoch(&mut self) {
        // Ending the epoch closes the Adaptive gate, which would blank the in-flight
        // guesses just as surely as flushing them. Guesses from an epoch that PROVED it
        // echoes keep their pixels until they retire or expire — the user has already
        // watched that line echo, so nothing unechoed is exposed by carrying them. An
        // unproven epoch ending leaves an earlier proven epoch's carry in place: its
        // guesses are the oldest, and its own stay dark behind them.
        if self.confirmed_epoch {
            self.grandfathered_epoch = Some(self.epoch);
            // A carried chain keeps displaying exactly as it did: its motions on
            // the motion proof the line earned.
            self.carried_proven = self.proven;
        }
        self.epoch = self.epoch.wrapping_add(1);
        // A chain in flight keeps its line as its base (`void_proof`); nothing is
        // armed on it again, since it is of the epoch that just ended.
        self.void_proof();
        self.anchor_lost = false; // the new epoch re-anchors on the live cursor
        self.cancelled_at = None; // …which whatever ended the epoch has moved
        self.line_end_at = None; // …and the line it ended may not end there any more
        self.cancelled_pair = None; // …and its pair's transit is no tail of this line
    }

    /// The program echoed `p`. A guess of the CURRENT epoch arms its display gate and
    /// moves the proof to the cell after it — where the terminal leaves the cursor,
    /// or the deferred-wrap slot it parks in at the right margin. A guess from an
    /// earlier epoch measured the link but proves nothing about this one.
    fn confirm(&mut self, p: Prediction) {
        if p.epoch != self.epoch {
            return;
        }
        self.confirmed_epoch = true;
        self.proof_cell = Some(self.cell_after(&p));
        self.proof_echo = Some(p);
        // `p` is of the run in flight: only one run is ever pending (a guess with
        // nothing of its epoch to extend waits for the set to drain).
        self.proof_line_end = self.tail_line_end;
    }

    /// Forget the standing proof: the epoch is unproven until an echo proves it
    /// again (`confirmed_epoch`, `proof_cell`, `proof_echo` and `proof_line_end`
    /// move together).
    ///
    /// The class proofs go with it (`proven`, [`Rule::ProofsEndWithTheEpoch`]):
    /// whatever voided the glyph proof (a boundary, a cursor the program moved, a
    /// tail that moved) may have put another program behind the same line, and
    /// the cooked `cat` an Enter starts echoes `^[[D` where the shell moved the
    /// cursor. So does the known line, which only an echo this proof covers
    /// verified, unless a state chain in flight still needs it as its base; that
    /// chain's ops stay tracked, dark, and nothing new is placed on the line
    /// (`placeable` asks for the proof).
    fn void_proof(&mut self) {
        self.confirmed_epoch = false;
        self.proof_cell = None;
        self.proof_echo = None;
        self.proof_line_end = false;
        if self.rule(Rule::ProofsEndWithTheEpoch) {
            self.proven = 0;
        }
        if !self.engaged() {
            self.line = None;
        }
    }

    /// Start a run's TAIL (see `tail`): the cursor row from the live cursor the
    /// guess at `at` is anchored on, from the press's `cells`, up to the grid's
    /// `cols`. The cursor's own cell is in it, because what a guess is typed over
    /// decides whether it is an append. A press with no row, or a guess that
    /// wrapped off the cursor's row (the deferred-wrap slot), leaves no snapshot,
    /// and that run proves only over a blank tail. Whether the run is at the end
    /// of the line (`tail_line_end`) is what it `inherits` from the proof or the
    /// append it starts on, if anything, else what the snapshot shows at `now`.
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
        let Some(cells) = cells.filter(|_| at == cursor) else {
            return;
        };
        let from = cursor.1;
        self.tail_cells.extend(
            (from..cols).map(|c| cells.get(usize::from(c)).copied().unwrap_or(Seen::Blank)),
        );
        self.tail = Some((cursor.0, from));
        // Nothing from the cursor cell to the margin, and no moved tail still on
        // its way to draw there: the end of the line. A blank is what a plain
        // observe answers as blank, opaque or not (the blanks of a row the frame
        // reorders are opaque).
        if inherits.is_none() {
            let window = self.glitch_window();
            let tail_in_flight = self
                .moved_tail_at
                .is_some_and(|t| now.saturating_duration_since(t) <= window);
            self.tail_line_end =
                !tail_in_flight && self.tail_cells.iter().all(|s| s.legacy().is_none());
        }
    }

    /// The guess typed at `born` echoed somewhere other than an append: its tail
    /// redraw may still be on its way (`moved_tail_at`).
    fn moved_tail(&mut self, born: Instant) {
        self.moved_tail_at = Some(self.moved_tail_at.map_or(born, |t| t.max(born)));
    }

    /// The cell after `p`, where an echo of `p` leaves the cursor: the next
    /// column, or `p`'s own in the last column, where the terminal parks.
    fn cell_after(&self, p: &Prediction) -> (u16, u16) {
        let next = p.col.saturating_add(1);
        let parked = self.grid_cols > 0 && next >= self.grid_cols;
        (p.row, if parked { p.col } else { next })
    }

    /// What the tail snapshot says stood at `(row, col)` when the run started, if
    /// it covers that cell (see `tail`).
    fn tail_at(&self, row: u16, col: u16) -> Option<Seen> {
        let (_, from) = self.tail.filter(|&(r, from)| r == row && from <= col)?;
        Some(
            self.tail_cells
                .get(usize::from(col.saturating_sub(from)))
                .copied()
                .unwrap_or(Seen::Blank),
        )
    }

    /// Whether the row right of the echoed guess `p` is CLEAN, what a run of
    /// appends leaves there (THE TAIL GUARD; see `tail`): every cell holds what
    /// the run's press saw there, or, where no snapshot covers it, nothing. A
    /// cell a pending guess names may also hold that guess's own glyph, echoed in
    /// the same burst: with the real `cursor` past it (`echoed_past`), since a
    /// glyph the cursor has not passed is not that guess's echo but text in its
    /// cell (a shifted tail glyph that happens to be the same letter, typed over
    /// itself: `o` typed over the last `o` of a line by a far end that writes the
    /// glyph before the tail, the second `o` typed, and the displaced `o` stood in
    /// its cell and passed for its echo). Called with `p` already retired, for a
    /// run not at the end of the line (`tail_line_end`, which proves without a
    /// comparison); up to `grid_cols` observe calls.
    ///
    /// …and every guess of the run, `p` and those still pending, was typed over
    /// a cell that held nothing or that very glyph (a suggestion the typed glyph
    /// matches). A guess typed over other text is mid-line, whatever the row
    /// shows now: once enough inserts have pushed the tail off the row's right
    /// edge (onto the next row of a wrapped line), the row right of an echo
    /// reads blank again, and the comparison alone would pass it.
    ///
    /// `clean_row` carries the verdict across one reconcile: once a guess's tail
    /// is clean, so is every later guess's on the same row in the same turn (its
    /// tail is a suffix of the cells just compared, against the same snapshot,
    /// with the same pending guesses exempt under the same cursor, and its own
    /// cell was compared as a pending guess's), so a retired burst costs one row
    /// scan, not one per guess.
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
        // Typed over `held` as glyph `ch`: nothing there, or that glyph. Nothing
        // is what a plain observe answers as blank, opaque or not: on a row the
        // frame reorders (BiDi) every written cell is opaque, the blanks between
        // a right-to-left prompt and its right prompt included, and a guess typed
        // on one of them is an append like any other.
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
            // The cell a cancelled pair is still writing and erasing may hold
            // the cancelled glyph (`cancelled_pair`).
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

    /// Whether `p`, of the run in flight, was typed as an APPEND as far as this
    /// frame can tell: the run is at the end of the line (`tail_line_end`), or
    /// the row right of `p` is clean (`tail_clean`).
    fn appended(
        &self,
        p: &Prediction,
        observe: &mut impl FnMut(u16, u16) -> Seen,
        clean_row: &mut Option<u16>,
        cursor: Option<(u16, u16)>,
    ) -> bool {
        self.tail_line_end || self.tail_clean(p, observe, clean_row, cursor)
    }

    /// Whether the press saw text at or right of `p` that a clean tail kept: a
    /// right prompt, a suggestion the typed glyph matched. Counted, for the
    /// theorem's floor that such lines still prove.
    fn tail_decorated(&self, p: &Prediction) -> bool {
        (p.col..self.grid_cols)
            .any(|c| self.tail_at(p.row, c).is_some_and(|s| s.legacy().is_some()))
    }

    /// Refuse to place a typed glyph: it is input we did not model, and everything
    /// typed after it would be placed relative to an echo we cannot predict. A
    /// printable glyph (wide, zero-width) only inserts, so the guesses to its left
    /// still echo where they are painted and keep their pixels, as across a line
    /// terminator. A control character can erase them (BS, ^U, ^W reach a line
    /// editor however the host labelled the key), so it retires them like any other
    /// unmodelled input.
    fn decline(&mut self, ch: char, now: Instant) -> bool {
        // Declined is not unsent: the program still echoes it, somewhere we did
        // not guess.
        self.owe(now);
        if ch.is_control() {
            self.flush();
            return false;
        }
        self.end_epoch();
        self.anchor_lost = true;
        // Newest input, so the next Backspace erases it (see `declined_tail`).
        self.declined_tail = self.declined_tail.saturating_add(1);
        false
    }

    /// Adopt the host's content-coordinate generation. A change means the terminal
    /// moved rows (output scrolled the screen, a region scroll, an inserted or
    /// deleted line, a reset): every pending guess now names a cell holding some
    /// other line, where an identical glyph would confirm an echo that never happened,
    /// and the line under the cursor is not the line whose echo was proven, even on
    /// the same physical row. Both are retired, exactly as a host [`reset`](Self::reset).
    fn observe_coordinates(&mut self, coords: u64) {
        if self.coords.is_some_and(|seen| seen != coords) {
            self.reset();
        }
        self.coords = Some(coords);
    }

    /// A cancelled pair can leave the real cursor on its cell, one cell past
    /// it, or still parked before a wrapped first-cell glyph. With no earlier
    /// guesses pending, any other observation cannot belong to that pair.
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
            // The pair may still produce output after this unrelated move.
            // Its cell and proof are lost; do not credit that output as a new echo.
            self.owe(now);
            self.flush();
        }
    }

    /// Reconcile pending predictions against fresh grid state after child output was
    /// applied. `real_cursor` is the cursor now, `alt_screen` whether the alternate
    /// screen is active, and `observe(row,col)` reads the real glyph at a cell (its
    /// `char`, or `None` if blank/space/unreadable). Confirms leading predictions the
    /// program echoed (feeding the RTT estimate and arming the epoch's display gate),
    /// and flushes on any divergence.
    ///
    /// `coords` is the host's content-coordinate generation, read under the same
    /// terminal hold as the cursor and the cells: any value that changes exactly when
    /// the terminal moves rows (aterm-core's `ContentScrollState::coordinate_generation`).
    /// A change resets the predictor before anything is compared (see
    /// `observe_coordinates`); every host hands one, so none can forget the scroll.
    ///
    /// [`reconcile_seen`](Self::reconcile_seen) with every glyph read as
    /// [`Seen::Glyph`] and every `None` as [`Seen::Blank`]: a host that cannot say
    /// which cells are opaque.
    ///
    /// Such a host runs no state chain: a chain compares whole states, and a wide
    /// glyph's half read as a glyph or a blank is a cell it cannot tell from a
    /// state's. So a chain in flight is flushed, and no edit key is armed until a
    /// reconcile says which cells are opaque (`edit_capable`).
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

    /// [`reconcile`](Self::reconcile), with `observe` answering what the host read
    /// at a cell as a [`Seen`]. The glyph lane reads each answer through
    /// [`Seen::legacy`], so an opaque cell is the plain glyph it always was; the
    /// tail guard (`tail_clean`) compares whole answers, so a cell that changed
    /// under the same glyph (a wide glyph's half, combining marks) is a change.
    ///
    /// A state chain in flight reconciles by state (`reconcile_chain`): the cells
    /// of its line, which an opaque cell among them flushes.
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

    /// The body of both reconcile entry points.
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
        // The alternate screen owns the cursor and does not line-echo: never predict.
        if alt_screen {
            self.flush();
            // An app-drawn cursor says nothing about a pending line wrap, and the
            // observation would be stale by the time the primary screen returns.
            self.deferred_wrap = false;
            return;
        }

        // Classify latency once per independent output/reconcile TURN, not once per
        // character. One delayed scheduler callback can retire a whole type-ahead
        // burst; counting every retired glyph as separate slow evidence would let
        // that single tail satisfy the two-confirmation latch by itself.
        //
        // The turn's sample is the OLDEST retired guess — the latency the user
        // waited for the first unechoed glyph. The newest is always the smallest
        // sample, so the faster the user typed, the faster it would read.
        let mut oldest_confirmed: Option<Duration> = None;
        // The row whose tail this turn already found clean (see `tail_clean`).
        let mut clean_row: Option<u16> = None;
        // The echo the standing proof rested on before this turn (`proof_echo`).
        let proof_before = self.proof_echo.map(|p| p.seq);

        // A state chain retires by whole states, its glyphs included.
        let chain = self.engaged();
        if chain {
            self.reconcile_chain(real_cursor, now, &mut observe, &mut oldest_confirmed);
        }
        // Retire confirmed leading predictions; a different glyph at the head ⇒ the
        // program diverged from our guess ⇒ flush. A still-blank head ⇒ not echoed
        // yet ⇒ stop and wait for the next burst.
        while let Some(p) = self.preds.front().copied().filter(|_| !chain) {
            match observe(p.row, p.col).legacy() {
                // The glyph is there, but an echo also moves the cursor past it
                // (`echoed_past`). A cell that showed the glyph before the key was
                // even read (an autosuggestion the typed glyph matches) is not its
                // echo: retired on it, a run typed along a suggestion was measured
                // at a few milliseconds a guess, collapsing the smoothed RTT and
                // with it the window every owe is judged by, so an owed echo on
                // time for the link lapsed and was credited to a later guess for
                // the same glyph, one cell behind; and it was credited with no
                // echo in flight custody at all. Held, the guess stays pending on
                // the cell that already shows it, which the cursor check below
                // accepts (the real cursor stands on it), until its echo moves the
                // cursor.
                Some(c) if c == p.ch => {
                    if self.rule(Rule::CursorAdvance) && !self.echoed_past(real_cursor, &p) {
                        self.count(Event::AdvanceWait);
                        break;
                    }
                    self.retire_echoed(
                        p,
                        now,
                        &mut oldest_confirmed,
                        &mut observe,
                        &mut clean_row,
                        real_cursor,
                    );
                }
                // Another glyph, but the very one the run's press saw in that cell,
                // with the cursor still on it: the screen as it was, not yet echoed
                // (a glyph typed onto an autosuggestion it diverges from: `x` over
                // the `o` of `echo hello`). Flushed, the guess went at the first
                // frame after its press, and the proof with it, so every suggestion
                // the typist left cost the keys after it their display.
                Some(c)
                    if real_cursor == Some((p.row, p.col))
                        && self.tail_at(p.row, p.col).and_then(Seen::legacy) == Some(c) =>
                {
                    break;
                }
                Some(_) => {
                    self.flush();
                    break;
                }
                None => {
                    // An echoed SPACE is indistinguishable from a blank cell by glyph
                    // (observe filters ' ' → None), so confirm it instead by the real
                    // cursor having advanced past it; otherwise it would wedge the queue
                    // and the cursor-consistency check below would flush all type-ahead.
                    if p.ch == ' '
                        && matches!(real_cursor, Some((rr, rc)) if rr == p.row && rc > p.col)
                    {
                        self.retire_echoed(
                            p,
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
        if let Some(sample) = oldest_confirmed {
            self.record_echo_regime(sample);
        }

        // THE STANDING TAIL (`proof_echo`): a proof earned before this turn stands
        // only while the row right of its echo still reads as the snapshot. One
        // row scan per turn while proven, none when this turn's own retirement
        // just compared that row, and none for a proof at the end of the line,
        // which has no tail to move (`proof_line_end`).
        if self.confirmed_epoch
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

        // Cursor-consistency: if anything is still pending, the real cursor must sit
        // exactly at the first unconfirmed prediction (the program echoed up to it).
        // Anything else (a row change, completion rewrite, reflow) ⇒ flush. (A
        // state chain's cursor was compared with its states above.)
        if let Some(&p) = self.preds.front().filter(|_| !self.engaged()) {
            match real_cursor {
                Some(rc) if rc == (p.row, p.col) => {}
                // A guess that continued past the right margin is legitimately AHEAD of
                // the real cursor: a terminal defers the wrap, so after echoing the
                // final column the cursor is parked THERE (last column, previous row)
                // and only moves to (row, 0) when the next glyph arrives.
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

        // Latch the deferred-wrap parking for the NEXT keystroke (see `deferred_wrap`).
        // Computed last, from this turn's real cursor, so a flush above cannot leave a
        // stale reading behind: the observation describes the SCREEN, not our guesses.
        // A blank (or space) final cell reads as "not parked" — the terminal has not
        // filled that column yet, so the next glyph really does land in it — unless
        // it is the CONTINUATION of a wide glyph whose lead fills the column before
        // it: `observe` reads that right half as blank, yet the column is filled and
        // the terminal is parked on it, so the next glyph wraps exactly as after a
        // narrow one. Read as free, the next guess went onto the glyph's right half
        // (which the painter refuses) and every guess after it one cell short, the
        // first of them on the cell where the refused one echoes.
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

    /// The predictions to OVERLAY this frame. Expires stale unconfirmed guesses first
    /// (self-healing), then applies the display gate — see
    /// `visible_count` — which yields the leading run of guesses
    /// this frame may paint: `Off` ⇒ none; `Always` ⇒ all pending; `Adaptive` ⇒ all
    /// pending once an echo is confirmed this epoch and consecutive slow samples
    /// established a stable high-latency link (plus any guess carried across a submit
    /// by a line that had already confirmed).
    ///
    /// A state chain in flight is painted as its displayed state: the state the
    /// leading run of visible ops leaves, at every cell of its line where it
    /// differs from the screen the last reconcile read (a typed glyph, or a blank
    /// where an erase vacated a cell), with its cursor in
    /// [`overlay_cursor`](Self::overlay_cursor). A host paints those cells all or
    /// none: a part of a state is a frame no program draws.
    pub fn overlay(&mut self, now: Instant) -> &[Prediction] {
        self.expire(now);
        self.ghost_cursor = None;
        if self.engaged() {
            self.compose_chain();
            return &self.ghosts;
        }
        // `visible_count` is a leading run of `preds`, so this is always `Some`.
        let visible = self.visible_count();
        self.preds.make_contiguous().get(..visible).unwrap_or(&[])
    }

    /// The cursor of the frame [`overlay`](Self::overlay) last composed, when it
    /// is a state chain's: the cursor its displayed state leaves (a motion moves
    /// the cursor and no cell, so the cursor is all a pure motion paints). `None`
    /// for the glyph lane, whose host draws its cursor one past the newest ghost
    /// it painted, and when nothing of a chain is displayed.
    ///
    /// The host draws it only when it painted every cell of that overlay, and
    /// counts it as a displayed prediction (so the frame after it is retired
    /// erases it) even when it painted no cell.
    #[must_use]
    pub fn overlay_cursor(&self) -> Option<(u16, u16)> {
        self.ghost_cursor
    }

    /// Retire the guesses whose glitch window has elapsed — [`Self::overlay`]'s
    /// expiry without composing a frame — and say whether anything was dropped.
    ///
    /// The host's deadline sweep calls this when it wakes for
    /// [`Self::next_deadline`]: leaving expiry to a redraw that is then suppressed
    /// would leave a PAST deadline that the event loop re-folds at turn rate. After
    /// this call `next_deadline()` is `None` or in the future. A dropped guess that
    /// was on glass still needs its erase frame, so the host repaints on `true`.
    pub fn expire_due(&mut self, now: Instant) -> bool {
        let before = self.preds.len();
        self.expire(now);
        self.preds.len() != before
    }

    /// Retire guesses the program has not echoed within the glitch window.
    ///
    /// Only the EXPIRED head(s) go: the younger tail has not earned its timeout.
    ///
    /// An expired guess's key was SENT, and nothing says its echo will not come.
    /// If it comes, it comes late, and it lands where the real cursor stands: on
    /// the cell the expired guess named, ahead of anything typed since. So the
    /// expiry OWES that echo, like every other keystroke this crate stops
    /// tracking, and it VOIDS the proof of the expired guess's epoch. The proof
    /// names the cell after the last confirmed echo, which is exactly the expired
    /// guess's cell; kept, it painted the next glyph there, one cell before the
    /// cell it echoes in once the expired key's echo has taken it (the safety
    /// theorem's FIDELITY clause, judged by this window, in thirteen ops). The
    /// guesses still behind the expired one stay tracked, dark until an echo
    /// proves their epoch again: each was placed after a key whose echo is now
    /// either late or never, and nothing positional can tell those apart.
    ///
    /// A state chain expires WHOLE ([`Rule::StateExpiryFlushes`]): every state
    /// after its head is built on the head's effect, so once the head has waited
    /// a window every op is flushed, owed, as a divergence flushes them.
    fn expire(&mut self, now: Instant) {
        let window = self.glitch_window();
        if self.engaged() {
            let due = self
                .preds
                .front()
                .is_some_and(|head| now.saturating_duration_since(head.born) > window);
            if due && self.rule(Rule::StateExpiryFlushes) {
                self.count(Event::ExpiryOwe);
                self.flush();
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
            timed_out = true;
            // Its echo, late or never, moves the cursor where no guess says: the
            // line's end is no longer known (`line_end_at`).
            self.line_end_at = None;
            if owes {
                // Typed before every guess still pending, so owed to each of them.
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
            // A timeout WIDENS the probe window and NOTHING else. It must not reach
            // `record_rtt` or `record_echo_regime`:
            // (a) DIVERGENCE: `window` is `GLITCH_SRTT_MULT * srtt`, so feeding it back
            //     gives `srtt' = 1.6*srtt`, with no fixed point below the ceiling.
            // (b) SAFETY: `window >= GLITCH_FLOOR_MS > DISPLAY_OPEN_MS`, so every
            //     timeout would vote "slow", and two unechoed password keystrokes
            //     would latch `adaptive_slow` and put a ghost on screen.
            // A cold link slower than the floor still bootstraps: the window widens
            // until an echo fits, and THAT confirmation opens the gate.
            self.expiry_backoff = self
                .expiry_backoff
                .saturating_add(1)
                .min(EXPIRY_BACKOFF_MAX);
        }
    }

    /// How long an unconfirmed guess may live. RTT-relative, because a fixed window is
    /// the wrong unit: 250 ms is generous on a 2 ms LAN link and shorter than a single
    /// echo on the 300 ms satellite/transpacific links this feature is FOR. Floor and
    /// ceiling bound the two failure modes (expiring inside one local output burst; a
    /// wrong guess sitting on glass).
    ///
    /// The `expiry_backoff` term is the COLD-LINK probe: with no confirmation yet
    /// there is no `srtt`, so a link slower than the floor could never get an echo in
    /// under the window and could never learn anything. Doubling the floor per
    /// consecutive timeout walks it 250 → 500 → 1000 → 2000 ms until an echo fits;
    /// any confirmation resets it (see [`Self::record_rtt`]). It is bounded, and
    /// unlike feeding timeouts into `srtt` it cannot diverge or vote on the regime.
    fn glitch_window(&self) -> Duration {
        let probe = (GLITCH_FLOOR_MS << self.expiry_backoff.min(EXPIRY_BACKOFF_MAX)) as f32;
        let floor = probe.min(GLITCH_CEILING_MS as f32);
        let ms = self.srtt.map_or(floor, |s| {
            (s * GLITCH_SRTT_MULT).clamp(floor, GLITCH_CEILING_MS as f32)
        });
        Duration::from_millis(ms as u64)
    }

    /// How many LEADING pending guesses pass the display gate — shared by
    /// [`overlay`](Self::overlay) and [`is_displaying`](Self::is_displaying). `Off` ⇒
    /// none; `Always` ⇒ all; `Adaptive` ⇒ all once an echo is confirmed this epoch and
    /// consecutive slow echo samples established that prediction can help.
    ///
    /// A COUNT rather than a bool because the submit boundary can leave guesses from a
    /// confirmed line in flight while the new epoch is still unconfirmed. Those keep
    /// their pixels (they are always at the FRONT, being older) while the new line's
    /// guesses stay dark — the password guarantee applies per guess, not per frame.
    ///
    /// In Adaptive it is the leading run of ops `op_visible` admits: an op whose
    /// class is unproven blocks everything after it, which is placed relative to
    /// its effect. With every op of the glyph class this is the run of the
    /// current epoch's guesses once it is proven, and of the carried epoch's.
    fn visible_count(&self) -> usize {
        match self.mode {
            PredictMode::Off => 0,
            PredictMode::Always => self.preds.len(),
            PredictMode::Adaptive if !self.adaptive_slow => 0,
            PredictMode::Adaptive => self.preds.iter().take_while(|p| self.op_visible(p)).count(),
        }
    }

    /// Whether Adaptive may show op `p`: of the current epoch once its glyph
    /// proof stands, or of the carried epoch, and in either case once its class
    /// is proven there (THE CLASS LADDER, [`Rule::MotionClassProof`]). A motion
    /// shows only after a motion of its epoch has echoed exactly: a cooked tty
    /// never echoes one exactly, so it never shows one.
    fn op_visible(&self, p: &Prediction) -> bool {
        let class_ok = |proven: u8| match p.class {
            OpClass::Glyph => true,
            OpClass::Motion => !self.rule(Rule::MotionClassProof) || proven & PROVEN_MOTION != 0,
        };
        (p.epoch == self.epoch && self.confirmed_epoch && class_ok(self.proven))
            || (Some(p.epoch) == self.grandfathered_epoch && class_ok(self.carried_proven))
    }

    /// NOTHING pending — the render paths' per-frame idle guard: with no guesses
    /// in flight (and no ghost on glass, which the caller tracks) the host skips
    /// its whole predict block, so an idle predictor costs nothing per frame.
    /// Every flush leaves `preds` empty, so the guard cannot strand a deadline or
    /// an unpainted erase.
    pub fn idle(&self) -> bool {
        self.preds.is_empty()
    }

    /// Half-open `(row_start, col_start, row_end, col_end)` hull of pending
    /// single-cell guesses. A presentation consumer can reserve their space
    /// without calling `overlay`, which expires guesses and changes state.
    /// Includes currently unshown guesses because reconciliation later in the
    /// same frame can admit them. Empty after the ordinary expiry/reset path.
    ///
    /// While a state chain is in flight the hull also covers its line from `lo`
    /// to one past the furthest text end any state reaches: every cell a state
    /// may paint, the vacated ones included.
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
                // Widened from `u16`: neither sum can saturate.
                (row, col, row.saturating_add(1), col.saturating_add(1))
            })
            .chain(span)
            .reduce(|(r0, c0, r1, c1), (s0, d0, s1, d1)| {
                (r0.min(s0), c0.min(d0), r1.max(s1), c1.max(d1))
            })
    }

    /// Every pending guess, oldest first, without expiring any: the leading run
    /// [`overlay`](Self::overlay) displays is a prefix of these.
    ///
    /// A read-only test seam, so a property test can follow each guess back to
    /// the keystroke that armed it (the safety theorem in
    /// `tests/reconciled_view_safety.rs`). Guesses never move once armed and
    /// leave only from the front (echo, expiry), the back (a Backspace popping our
    /// own) or all at once (a flush), which is what lets a caller keep that map.
    #[cfg(any(test, debug_assertions))]
    pub fn pending(&self) -> impl ExactSizeIterator<Item = &Prediction> + '_ {
        self.preds.iter()
    }

    /// Whether predictions are actually being DISPLAYED (not merely tracked), so
    /// Adaptive on a fast local link (where nothing shows) forces no repaint per
    /// keystroke. Returns true while the oldest guess is past its expiry window so
    /// [`overlay`](Self::overlay)'s flush still gets one cleanup frame (an erase only
    /// if that guess passed the display gate; hidden Adaptive state changes no pixels).
    pub fn is_displaying(&self, now: Instant) -> bool {
        match self.preds.front() {
            None => false,
            // The same window `expire` uses: these two disagreeing would either strand
            // an erase (host thinks nothing changed) or burn a frame per repaint while
            // the guess is still legitimately in flight.
            Some(oldest) => {
                now.saturating_duration_since(oldest.born) > self.glitch_window()
                    || self.visible_count() > 0
            }
        }
    }

    /// The instant the oldest pending guess self-expires (the glitch timeout). The host
    /// arms a `WaitUntil` on this so a stale prediction is cleaned up (and, if it was
    /// visible, repainted away) via [`overlay`](Self::overlay)'s expiry flush even when
    /// no further input or output arrives.
    pub fn next_deadline(&self) -> Option<Instant> {
        let window = self.glitch_window();
        self.preds.front().map(|p| p.born + window)
    }

    /// Drop all in-flight predictions (the host changed the coordinate space: resize,
    /// font zoom, pane ⇄ split). The terminal moving rows itself reaches here through
    /// the `coords` generation — a scrolled-in cell holding the same glyph would
    /// otherwise confirm an echo that never happened. The RTT estimate is retained;
    /// the confirmation epoch is forgotten so the next line re-confirms first.
    pub fn reset(&mut self) {
        self.flush();
        // Re-adopt the generation at the next observation: a host reset is itself
        // the coordinate change, and must not be counted twice.
        self.coords = None;
        // The coordinate space changed, so the last cursor/cell reading describes a
        // screen that no longer exists — re-latch it from the next real reconcile.
        self.deferred_wrap = false;
    }

    /// Drop all predictor state when the host switches to a terminal session it has
    /// NO estimate for. Unlike [`reset`](Self::reset), this also forgets the learned
    /// echo RTT: latency belongs to the PTY/link that produced it, not to the window
    /// containing whichever pane happens to be focused next.
    ///
    /// A host that can keep what it learned per session should swap instead
    /// ([`take_link_estimate`](Self::take_link_estimate) /
    /// [`restore_link_estimate`](Self::restore_link_estimate); see [`LinkEstimate`]).
    pub fn reset_session(&mut self) {
        self.reset();
        // A session we know nothing about owes us nothing: the echoes in flight
        // belong to the PTY we just left. (A swap keeps them with that session's
        // estimate instead; see `LinkEstimate`.)
        self.owed_at = None;
        self.srtt = None;
        self.slow_samples = 0;
        self.fast_samples = 0;
        self.adaptive_slow = false;
        // The cold-link probe is the same measurement as the RTT, one window wide: a
        // widened window that outlives its link makes an unknown pane wait up to the
        // ceiling before a wrong guess self-heals. Unknown link ⇒ probe from the floor.
        self.expiry_backoff = 0;
    }

    /// Take the learned link estimate OUT, leaving the predictor exactly as
    /// [`reset_session`](Self::reset_session) would (no guesses, no epoch, no
    /// estimate). The returned [`LinkEstimate`] is the OUTGOING session's property:
    /// park it in a per-session map and hand it back with
    /// [`restore_link_estimate`](Self::restore_link_estimate) when that session is in
    /// front again, instead of throwing away a measurement the link still satisfies.
    pub fn take_link_estimate(&mut self) -> LinkEstimate {
        let est = LinkEstimate {
            srtt: self.srtt,
            slow_samples: self.slow_samples,
            fast_samples: self.fast_samples,
            adaptive_slow: self.adaptive_slow,
            expiry_backoff: self.expiry_backoff,
            // Taking retires all pending guesses; their already-written keys
            // still owe output, just as a normal flush does.
            owed_at: self.owed_with_pending(),
        };
        // Route the clearing through `reset_session` itself so "what a take leaves
        // behind" and "what an unknown session starts with" can never drift apart.
        self.reset_session();
        est
    }

    /// Install the estimate of the session now in front. `LinkEstimate::default()` is
    /// the never-measured session and is therefore equivalent to
    /// [`reset_session`](Self::reset_session).
    ///
    /// The incoming session's LINK is known; its SCREEN is not — pending guesses and
    /// the confirmation epoch belong to the pane we just left, so they go. That is
    /// what keeps the no-unechoed-flash guarantee intact across a swap: a restored
    /// `adaptive_slow` only decides that speculation is WORTH showing on this link,
    /// never that the line now under the cursor has echoed. The epoch is still earned
    /// per line, on this line.
    pub fn restore_link_estimate(&mut self, est: LinkEstimate) {
        self.reset();
        // Retire the outgoing pixels, then install only the incoming session's
        // output custody: its own earlier keys may still echo after its return (an
        // A -> B -> A switch inside one glitch window gets A's back; a
        // never-measured session owes nothing).
        self.srtt = est.srtt;
        self.slow_samples = est.slow_samples;
        self.fast_samples = est.fast_samples;
        self.adaptive_slow = est.adaptive_slow;
        self.expiry_backoff = est.expiry_backoff;
        self.owed_at = est.owed_at;
    }

    /// Debt after retiring the pending guesses, without changing any screen or
    /// estimate state. Both a flush and a parked session own the same echoes. The
    /// typed owes fold in as owes of any echo: conservative, and what the session
    /// carries away in its [`LinkEstimate`].
    ///
    /// Every pending op is a keystroke that reached the program, an edit op as
    /// much as a glyph ([`Rule::EditOpsOwe`]): a flushed `→`'s echo (readline's
    /// older form reprints the cell it steps over) lands like any other.
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

    /// The guess `seq`, typed at `at`, retired without credit over a clean tail:
    /// its echo is owed as `ch` (see `glyph_owes`). Entries older than the window's
    /// ceiling can refuse nothing any more and go first; past the cap, the oldest
    /// folds into the owe of any echo, raised for every guess still pending (each
    /// was typed after it).
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

    /// A keystroke typed at `at` is no longer tracked by any guess: its echo is
    /// owed (see `owed_at`).
    fn owe(&mut self, at: Instant) {
        self.owed_at = Some(self.owed_at.map_or(at, |t| t.max(at)));
    }

    /// Whether the real cursor stands where an echo of `p` leaves it: past `p` on
    /// its row; parked ON `p` in the last column, where the terminal defers the
    /// wrap; or on a later row the echoes of `p`'s run reach (a burst retired in
    /// one turn that wrapped onto it: a guess of the run is on that row) or, when
    /// the run reaches the last column of `p`'s row, at the start of the next row
    /// (a far end that wraps itself). The other half of an echo's evidence,
    /// beside the glyph (see `reconcile_seen`).
    ///
    /// Any later row was too much: a submit's output moves the cursor down a row
    /// or two, and a type-ahead guess whose cell already showed its glyph (an
    /// autosuggestion an earlier echo drew), its own key never echoed (the far
    /// end had switched echo off), was then credited with a sample of nothing.
    fn echoed_past(&self, cursor: Option<(u16, u16)>, p: &Prediction) -> bool {
        let last_col =
            |g: &Prediction| self.grid_cols > 0 && g.col.saturating_add(1) == self.grid_cols;
        cursor.is_some_and(|(r, c)| {
            (r == p.row && c > p.col)
                || ((r, c) == (p.row, p.col) && last_col(p))
                || (r > p.row
                    && (self.preds.iter().any(|g| g.row == r)
                        || ((r, c) == (p.row.saturating_add(1), 0)
                            && (last_col(p)
                                || self.preds.iter().any(|g| g.row == p.row && last_col(g))))))
        })
    }

    /// Whether the glyph (and cursor advance) that just matched `p` can be credited
    /// to `p`'s own keystroke: no keystroke typed before it whose echo is owed can
    /// still be in flight, so no other echo can be standing in its cell.
    ///
    /// THE DEFECT THIS CLOSES: a guess flushed while its keystroke was still on its
    /// way, then the same glyph typed again at the live cursor. The first
    /// keystroke's echo lands in the second guess's cell, the cursor advances past
    /// it exactly as the second guess's own echo would, and positional
    /// reconciliation confirmed it: arming the new epoch's display gate with a
    /// proof whose cell is one keystroke behind, so the NEXT glyph was painted
    /// where the second keystroke's echo then landed (`n`, `n`, ` ` shown as
    /// `n ` and drawn as `nn `). The same holds for any untracked keystroke
    /// whose output can repeat a typed glyph (a Right arrow re-printing the cell it
    /// steps over, a completion, a paste).
    ///
    /// Asked of every guess, of any epoch, about the keystrokes owed from BEFORE it
    /// (`Prediction::owed`): only those can echo ahead of it into its cell. One
    /// owed AFTER it (the terminator that submitted its line, a glyph declined
    /// behind it) echoes after it and leaves its evidence alone, so a carried tail
    /// still measures the link while the terminator's echo is in flight. A carried
    /// guess with an echo owed from before it is asked like any other: matched by
    /// that echo, it would feed the estimate a sample of a few milliseconds that
    /// belongs to another key (and enough of those close the Adaptive gate).
    ///
    /// The bound is the glitch window, the predictor's one notion of how long an
    /// echo may take: past it a guess is given up as unechoed, and so is an owed
    /// echo. An echo later than its window can still stand in a later guess's cell,
    /// exactly as a late echo of an expired guess can; without a causal link from
    /// output to keystroke nothing positional can tell them apart.
    ///
    /// A TYPED owe (`glyph_owes`) refuses credit only to a guess typed after the
    /// one that owes it, for the same glyph: no other glyph can be its echo.
    ///
    /// A guess with a STALE ANCHOR (`Prediction::stale`, [`Rule::StaleAnchor`])
    /// asks about the keys typed after it too: the run may echo a cell left of
    /// where it is placed (an erase we did not place stepped the cursor back
    /// under it), and then the next key's echo stands in its cell, glyph and
    /// cursor advance alike. So it is refused credit while any owed echo is in
    /// flight (a declined key typed after it is owed, not tracked) and while a
    /// later guess for the same glyph is pending. Credited, `c` typed twice after
    /// such a Backspace proved the line on the second `c`'s echo, a cell behind,
    /// and the third `c` was painted where the second echoed (FIDELITY, seed 76
    /// of the eighty-seed sweep of the edit lane's world).
    fn attributable(&self, p: &Prediction, now: Instant) -> bool {
        let window = self.glitch_window();
        let live = |t: Instant| now.saturating_duration_since(t) <= window;
        if p.owed.is_some_and(live) {
            return false;
        }
        if p.stale
            && self.rule(Rule::StaleAnchor)
            && (self.owed_at.is_some_and(live)
                || self
                    .preds
                    .iter()
                    .any(|g| g.seq > p.seq && g.op == Op::Insert && g.ch == p.ch))
        {
            return false;
        }
        !(self.rule(Rule::TypedOwesSameGlyph)
            && self
                .glyph_owes
                .iter()
                .any(|&(seq, ch, at)| seq < p.seq && ch == p.ch && live(at)))
    }

    /// Retire the head guess `p`, whose glyph the program has drawn. An attributable
    /// echo measures the link and proves the epoch; one that might be another
    /// keystroke's retires the pixels (the cell already shows that glyph) and
    /// proves and measures nothing.
    ///
    /// …and leaves `p`'s own echo OWED: if the glyph was an earlier keystroke's,
    /// `p`'s is still on its way and lands in the cell of the guess behind it. Not
    /// owing it shifted the whole queue one cell while the earlier owe's window ran
    /// out under it: `n` flushed, then `n`, `n`, `c` typed — the flushed `n`'s echo
    /// retired the first new guess, the first new guess's echo then matched the
    /// second just after the flushed key's window closed, proving the epoch one
    /// cell behind, and `c` was painted where the second `n` echoed, with every
    /// echo inside 250 ms of its own key.
    ///
    /// An attributable echo of the CURRENT epoch proves it only as an append (THE
    /// TAIL GUARD, `appended`): at the end of the line, or over a clean tail. One
    /// whose echo moved the text right of it was typed mid-line: the sample still
    /// measures the link, but the proof is VOIDED, not merely withheld, since
    /// every guess behind it was placed as an append on a line that is not
    /// appending, and a proof standing from earlier in the epoch would keep
    /// painting them over the tail that moved.
    ///
    /// An unattributable match of an append owes `p`'s echo TYPED, as `p.ch`
    /// (`glyph_owes`): it lands in the next guess's cell, so only a later guess
    /// for the same glyph can be fooled by it. Otherwise `p` was typed mid-line
    /// and its echo shifts text whose glyphs could stand anywhere behind it, so it
    /// is owed as any echo, to every guess behind it.
    fn retire_echoed(
        &mut self,
        p: Prediction,
        now: Instant,
        oldest: &mut Option<Duration>,
        observe: &mut impl FnMut(u16, u16) -> Seen,
        clean_row: &mut Option<u16>,
        cursor: Option<(u16, u16)>,
    ) {
        self.preds.pop_front();
        // A guess typed after the cancelled pair echoed: the pair's erase, sent
        // before it, has landed (`cancelled_pair`).
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
            // A guess from BEFORE the last submit measures the link, but must not
            // arm the new line's display gate (password safety).
            if p.epoch != self.epoch {
                return;
            }
            if !self.rule(Rule::TailGuard) || self.appended(&p, observe, clean_row, cursor) {
                if self.tail_decorated(&p) {
                    self.count(Event::DecoratedProof);
                }
                self.ended_line_at(&p);
                self.confirm(p);
                self.extend_line(&p);
            } else {
                self.moved_tail(p.born);
                self.void_proof();
                self.count(Event::TailRefusal);
            }
            return;
        }
        // Not credited: an echo no key is credited with moved the cursor past
        // `p`, and `p`'s own is still to land. The proof of its epoch named a cell
        // at or before `p`'s, where the cursor stands once nothing else moves it;
        // that no longer holds, and an erase in flight (a Backspace that popped our
        // guess) puts the cursor back on that very cell, where the next glyph was
        // painted and `p`'s echo then landed (FIDELITY, seed 43 of the sweep).
        if p.epoch == self.epoch && self.rule(Rule::UncreditedVoids) {
            self.void_proof();
        }
        let appended = self.appended(&p, observe, clean_row, cursor);
        if appended {
            self.ended_line_at(&p);
        } else {
            self.moved_tail(p.born);
        }
        if self.rule(Rule::TypedOwes) && (!self.rule(Rule::TypedOwesCleanTail) || appended) {
            self.owe_glyph(p.seq, p.ch, p.born, now);
        } else {
            // Typed before every guess still pending and before anything armed
            // later, so it is owed to all of them.
            self.owe(p.born);
            for later in &mut self.preds {
                later.owed = later.owed.max(Some(p.born));
            }
        }
    }

    /// `p`, of the run in flight, retired as an append: in a run of the current
    /// epoch at the end of the line, the line now ends after it (`line_end_at`).
    fn ended_line_at(&mut self, p: &Prediction) {
        if self.tail_line_end && p.epoch == self.epoch {
            self.line_end_at = Some(self.cell_after(p));
        }
    }

    /// Clear pending predictions and end the confirmation epoch (display re-arms only
    /// after a fresh confirmation on the next line).
    ///
    /// The retired guesses' keystrokes reached the program all the same, so their
    /// echoes are owed (see `owed_at`), dated by the newest of them.
    fn flush(&mut self) {
        self.owed_at = self.owed_with_pending();
        self.preds.clear();
        self.chain = false; // …a chain with them, and its line with the proof below
        // Folded into `owed_at` above: the guesses they could refuse are gone,
        // and what is armed next anchors on a cursor these echoes still move.
        self.glyph_owes.clear();
        self.void_proof();
        self.seen.clear();
        self.anchor_lost = false; // nothing left to be anchored to
        self.grandfathered_epoch = None; // …and nothing left to carry pixels for
        self.carried_proven = 0;
        self.declined_tail = 0; // …nor anything a Backspace could still be cancelling
        self.cancelled_at = None; // …nor a cancelled cell the next guess could trust
        self.line_end_at = None; // …nor a line end the flushed keys may have moved
        self.cancelled_pair = None; // …nor a cancelled pair in transit to excuse
    }

    /// Fold a confirmed echo latency into the smoothed RTT estimate.
    fn record_rtt(&mut self, rtt: Duration) {
        let ms = rtt.as_secs_f32() * 1000.0;
        self.srtt = Some(match self.srtt {
            None => ms,
            Some(s) => s * (1.0 - SRTT_ALPHA) + ms * SRTT_ALPHA,
        });
        // A real echo landed, so the cold-link probe has served its purpose: retire the
        // backoff and let `srtt` alone size the window from here. Resetting here rather
        // than in `expire` is what makes the probe converge — silence widens it, and
        // exactly one confirmation collapses it.
        self.expiry_backoff = 0;
    }

    /// Fold one independent output turn into the stable visual classification.
    ///
    /// A LATCH, not a comparator: asymmetric thresholds separated by a dead band,
    /// because each flip erases whatever is mid-flight. Stability is worth more here
    /// than reaction speed in either direction.
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
            // Three conditions, all required, because closing is the destructive
            // direction: a sustained streak (not one lucky turn), agreement from the
            // SMOOTHED estimate (a single raw sample is exactly the jitter we are
            // filtering), and an EMPTY pending set — a gate flip with guesses in flight
            // erases pixels that are already on glass, which is the blink itself.
            let smoothed_fast = self.srtt.is_none_or(|s| s < DISPLAY_CLOSE_MS);
            if self.fast_samples >= FAST_SAMPLES_TO_HIDE && smoothed_fast && self.preds.is_empty() {
                self.adaptive_slow = false;
            }
        }
        // Samples inside [DISPLAY_CLOSE_MS, DISPLAY_OPEN_MS) are evidence for neither
        // regime: they leave the latch AND both streaks alone, so "consecutive" counts
        // decisive turns rather than being reset by every ambiguous one (a link
        // oscillating around one threshold would otherwise never reach either latch).
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

    /// The content-coordinate generation of a screen whose rows never move.
    const G: u64 = 0;

    /// A tiny fake grid for `observe`: maps (row,col) → glyph.
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

    /// THE SWEEP CONSUMES ITS DEADLINE (`docs/EFFECTS-AND-WAKE-FOLLOWUPS-2026-08-24.md`
    /// item 7). One `expire_due` at a wake past the deadline, with no `overlay`
    /// (a suppressed redraw), leaves `next_deadline()` `None` — never the past
    /// instant a peek-only sweep left for the event loop to re-fold every turn.
    /// Control: before the deadline it drops nothing and the deadline stands.
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

    /// One complete type→echo turn on row 0: arm `ch` at `col` at `at`, then confirm it
    /// `rtt` later with the cursor advanced. Returns the confirmation instant. Only
    /// valid with an empty pending set (it anchors on the passed cursor).
    fn echo_turn(p: &mut Predictor, ch: char, col: u16, at: Instant, rtt: Duration) -> Instant {
        assert!(p.predict_char(ch, (0, col), G, 80, at));
        let done = at + rtt;
        p.reconcile(Some((0, col + 1)), G, false, done, cell(&[((0, col), ch)]));
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
        assert!(p.predict_char('a', (0, 0), G, 80, now)); // pred 'a' at (0,0)
        assert!(!p.idle());
        // One 60 ms sample retires the prediction but does not trust a possibly
        // isolated scheduler tail enough to paint the next one.
        let first = now + Duration::from_millis(60);
        p.reconcile(Some((0, 1)), G, false, first, cell(&[((0, 0), 'a')]));
        assert!(p.idle(), "confirmed prediction is retired");
        assert!(p.predict_char('b', (0, 1), G, 80, first));
        assert!(p.overlay(first).is_empty(), "one slow sample stays hidden");

        // A second consecutive slow echo establishes the remote-latency regime.
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
        // Both typed before any echo: cursor is still (0,0) for the 2nd, but 'b'
        // must extend past 'a', not land on top of it.
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

        // Establish the ordinary local regime, then inject one 60 ms callback tail.
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

        // The next fast confirmation clears the candidate streak as well as keeping
        // the display latch closed.
        let local_again = tail + Duration::from_millis(1);
        p.reconcile(Some((0, 4)), G, false, local_again, cell(&[((0, 3), 'd')]));
        assert!(p.predict_char('e', (0, 4), G, 80, local_again));
        assert!(p.overlay(local_again).is_empty());
    }

    #[test]
    fn adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak() {
        // Leaving ssh for a local foreground process must still stop painting — but
        // NOT on one fast sample. A single-sample close makes the steady state on any
        // jittery link "off with occasional flashes", and each flip erases whatever is
        // mid-flight, so the user watches their own typing blink out and return.
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

        // Two more fast turns: the raw streak is now long enough, but the SMOOTHED
        // estimate is still remote-sized, so the latch holds.
        at = echo_turn(&mut p, 'd', 3, at, fast);
        at = echo_turn(&mut p, 'e', 4, at, fast);
        assert!(p.srtt.is_some_and(|s| s >= DISPLAY_CLOSE_MS));
        assert!(
            p.adaptive_slow,
            "a raw streak alone must not close the gate"
        );

        // Keep echoing locally: once the EWMA itself agrees, the gate closes.
        for col in 5..14 {
            at = echo_turn(&mut p, 'x', col, at, fast);
        }
        assert!(!p.adaptive_slow, "a settled local link stops painting");
        assert!(p.predict_char('z', (0, 14), G, 80, at));
        assert!(p.overlay(at).is_empty());
    }

    #[test]
    fn adaptive_gate_does_not_flap_across_a_single_fast_sample() {
        // Hysteresis: one fast dip in a slow link changes nothing (a single-sample
        // close would erase a ghost on every dip).
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(30);

        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow, "control: the slow link opened the gate");

        at = echo_turn(&mut p, 'c', 2, at, Duration::from_millis(5)); // one jitter dip
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
        // Closing is the destructive direction: it un-paints pixels that are already on
        // glass. Even a fully-settled fast streak must wait for the pending set to
        // drain, or the very turn that decides "prediction is not needed" is the turn
        // that blinks out the guess the user is currently looking at.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let fast = Duration::from_millis(1);

        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow);

        // Every turn confirms one guess while the user is one glyph ahead, so the
        // pending set is never empty at classification time.
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

        // Drained: the same evidence now closes it.
        at += fast;
        p.reconcile(Some((0, col + 1)), G, false, at, cell(&[((0, col), 'x')]));
        assert!(p.idle());
        assert!(!p.adaptive_slow);
    }

    #[test]
    fn burst_is_classified_by_the_oldest_retired_guess() {
        // REGRESSION (P3): `preds` is in type order, so the LAST guess retired in a
        // turn is the newest and always the smallest sample. Classifying by the minimum
        // meant a five-glyph burst echoed in one 45 ms turn reported 5 ms — the faster
        // the user typed, the more certain the shutoff. The user waited 45 ms for the
        // FIRST unechoed glyph; that is the latency prediction exists to hide.
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
        // Under-threshold Adaptive guesses remain tracked for reconciliation but are
        // never pixels, so crossing the deadline is an invisible cleanup, not an erase.
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
        // `is_displaying` deliberately returns true for one cleanup frame once any
        // pending guess expires, even if the display gate stayed closed. The visual
        // contract is the overlay trace: empty before the deadline and still empty
        // while expiry removes the hidden bookkeeping state.
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
        // The program echoed a DIFFERENT glyph (e.g. a completion/masked char).
        p.reconcile(Some((0, 1)), G, false, now, cell(&[((0, 0), '*')]));
        assert!(p.idle(), "a wrong guess flushes the whole prediction set");
    }

    #[test]
    fn alt_screen_flushes_predictions() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        assert!(!p.idle());
        // Entering vim/less (alternate screen) drops predictions.
        p.reconcile(Some((10, 4)), G, true, now, cell(&[]));
        assert!(p.idle());
    }

    #[test]
    fn expired_prediction_clears_next_deadline() {
        // REGRESSION (scrolled-back busy loop): a prediction in flight when the
        // user scrolls into history must still self-heal. `overlay`'s expiry flush
        // is the only thing that disarms `next_deadline()`; if it never runs, the
        // deadline stays pinned at a past instant and the host's WaitUntil-driven
        // repaint spins at 100% CPU. After the glitch window, overlay must flush
        // AND next_deadline() must become None (the wake self-disarms).
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        assert!(
            p.next_deadline().is_some(),
            "a pending guess arms a deadline"
        );
        let late = now + Duration::from_millis(GLITCH_FLOOR_MS + 50);
        let _ = p.overlay(late); // the same flush the scrolled-back branch now runs
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
        // The legacy 4-argument seam has no grid HEIGHT, so it cannot know whether a
        // wrap lands on a real row; it still declines. What it must not do is flush:
        // the guesses to the LEFT of the margin are exactly what the program will echo.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('a', (0, 78), G, 80, now)); // col 78 ok
        assert!(p.predict_char('b', (0, 78), G, 80, now)); // extends to col 79 (last column)
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
        // The program echoes the next glyph at column 0 of the following row;
        // guessing that is no more speculative than any other guess.
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
        // A terminal DEFERS the wrap: after echoing the final column the cursor is
        // still parked there and only moves to (row+1, 0) when the next glyph arrives.
        // The strict cursor-consistency check read that as a divergence and flushed
        // every wrapped guess one output burst after it was armed.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), G, (80, 24), now));
        assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now));
        // The echo of 'a' lands; the cursor stays in the deferred-wrap slot (0,79).
        p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 79), 'a')]));
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 1, "'a' retires, the wrapped 'b' survives");
        assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (1, 0, 'b'));
    }

    #[test]
    fn bottom_row_declines_without_flushing() {
        // The bottom edge is genuinely unmodellable — the grid SCROLLS, moving every
        // pending guess — so we decline there. The pending set stays: those cells are
        // above the wrap and unaffected.
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
        // Width, not ASCII-ness, is the criterion. `é` occupies one cell and echoes
        // like any letter; CJK/emoji take two and combining marks alter the cell to
        // their left, so their echo geometry is not ours to place.
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
        // Under `ambiguous_width = "wide"` the grid draws `é` and `°` two cells
        // wide: declined like CJK, never placed one cell where the echo takes two.
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
        // A character we cannot model says nothing about the guesses anchored to its
        // left — but it does cost the ANCHOR, so we stop extending until the set
        // drains and the live cursor (which the real echo moves) re-anchors us.
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
        // The real echo lands and drains the set: the live cursor is authoritative
        // again, so prediction resumes.
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
        // Adaptive confirms two guesses on row 0 (stable slow link), then the user reaches a
        // PASSWORD prompt on a NEW row where typing never echoes. The new line must
        // start a fresh epoch so the unechoed secret is never displayed.
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
        p.predict_char('s', (1, 9), G, 80, t2); // row 1 != epoch row 0 → fresh epoch
        assert!(
            p.overlay(t2).is_empty(),
            "an unechoed char on a new line must NOT display (no password leak)"
        );
    }

    #[test]
    fn submit_does_not_blank_the_tail_of_the_command_line() {
        // Enter must not flush: on a slow link the tail of the command line would
        // vanish at the instant of commit and reappear one RTT later.
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
        // The password guarantee, pinned against the P5 change: keeping in-flight
        // guesses across the boundary means one can CONFIRM after it. That confirmation
        // measures the link (legitimately) but must not arm the new line's display
        // gate, or the secret typed at the prompt below would show.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let r = 23u16; // bottom row, reused across both logical lines
        let slow = Duration::from_millis(60);
        // Establish a slow link on line 1 so the RTT latch is open.
        assert!(p.predict_char('a', (r, 0), G, 80, now));
        let t1 = now + slow;
        p.reconcile(Some((r, 1)), G, false, t1, cell(&[((r, 0), 'a')]));
        assert!(p.predict_char('b', (r, 1), G, 80, t1));
        let t2 = t1 + slow;
        p.reconcile(Some((r, 2)), G, false, t2, cell(&[((r, 1), 'b')]));
        assert!(p.adaptive_slow, "control: the link is classified slow");

        // Type one more glyph, hit Enter while it is still unechoed, then let its echo
        // land AFTER the boundary.
        assert!(p.predict_char('c', (r, 2), G, 80, t2));
        p.note_line_submit(t2);
        let t3 = t2 + slow;
        p.reconcile(Some((r, 3)), G, false, t3, cell(&[((r, 2), 'c')]));
        assert!(p.idle(), "the stale guess retired normally");
        assert!(
            !p.confirmed_epoch,
            "a pre-submit confirmation must not arm the new line"
        );

        // The password prompt lands on the SAME physical row and never echoes.
        assert!(p.predict_char('s', (r, 9), G, 80, t3));
        assert!(
            p.overlay(t3).is_empty(),
            "an unechoed secret on the new line must never display"
        );
    }

    #[test]
    fn slow_link_expiry_window_grows_until_the_echo_can_confirm() {
        // On a link slower than the 250 ms floor, a timeout WIDENS the probe window
        // until the echo fits inside it — but is NOT itself evidence about the link:
        // opening the gate still takes `SLOW_SAMPLES_TO_DISPLAY` genuine slow echoes.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let rtt = Duration::from_millis(400);

        // First keystroke on a cold 400 ms link: nothing confirms it in the floor
        // window, so it expires — recording a LOWER BOUND on the link's latency.
        assert!(p.predict_char('a', (0, 0), G, 80, now));
        let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
        assert!(p.idle(), "the unconfirmed guess still self-heals");
        assert!(
            p.glitch_window() > Duration::from_millis(GLITCH_FLOOR_MS),
            "a timeout widens the window instead of repeating forever"
        );

        // Second keystroke: the widened window now outlasts the real 400 ms echo, so
        // the guess is still in flight when its echo lands and CONFIRMS.
        let armed = now + Duration::from_millis(500);
        assert!(p.predict_char('b', (0, 0), G, 80, armed));
        let _ = p.overlay(armed + Duration::from_millis(300)); // a frame mid-flight
        assert!(!p.idle(), "the guess outlives the old fixed window");
        p.reconcile(Some((0, 1)), G, false, armed + rtt, cell(&[((0, 0), 'b')]));
        assert!(p.confirmed_epoch, "the echo finally reaches a live guess");
        assert!(
            !p.adaptive_slow,
            "ONE confirmation is not a regime: silence must never substitute for the \
             second slow sample (that is what painted a ghost at a password prompt)"
        );
        // The confirmation also collapses the probe back to the floor — the estimate is
        // now doing the sizing, so the backoff must not keep the window inflated.
        assert_eq!(
            p.expiry_backoff, 0,
            "a real echo retires the cold-link probe"
        );

        // A SECOND genuine slow confirmation is what opens the gate.
        let mut at = armed + rtt;
        assert!(p.predict_char('c', (0, 1), G, 80, at));
        at += rtt;
        p.reconcile(Some((0, 2)), G, false, at, cell(&[((0, 1), 'c')]));
        assert!(
            p.adaptive_slow,
            "two real slow echoes classify the link — the honest evidence path"
        );

        // …and that is the whole point: the next keystroke is painted immediately.
        assert!(p.predict_char('d', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "a 400 ms link finally predicts");
    }

    /// REGRESSION (P1 review): an endlessly non-echoing context — a password prompt,
    /// a raw-mode key — must never accumulate "slow" evidence. The real cursor does
    /// not move there, so `reconcile`'s consistency check matches and every guess
    /// reaches the expiry path; an earlier fix counted each of those as a slow echo
    /// sample and latched speculation ON after two keystrokes, on a LOCAL link.
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
        // The tail is YOUNGER than the head and has not earned its timeout.
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

        // Adaptive: an expiry voids the proof the line earned. The expired key's
        // late echo lands on the proven cell, so a glyph typed there now echoes
        // one cell further on.
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
        // 'c''s echo comes after all, late, into 'd''s cell: a different glyph, so
        // 'd' is flushed, and nothing proves the epoch until an echo of a guess
        // typed after it does.
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

    /// The `ExpiryOwes` negative control is the code as it shipped before the
    /// fix: the expiry pops the guess, owes nothing and keeps the proof, so the
    /// next glyph paints on the expired guess's cell.
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
        // Learn a useful RTT on a remote pane, then reuse the same Predictor for a
        // newly focused local pane. The first fast echo there may confirm the line,
        // but it must not inherit the remote pane's display eligibility.
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
        // The line reached the right margin and the echo caught up, so the pending set
        // is empty and the next keystroke anchors on the LIVE cursor — which the
        // terminal has parked in the deferred-wrap slot, ON TOP of the glyph it just
        // echoed. Guessing there painted a wrong character over a correct one on every
        // command line long enough to wrap (the case the wrap lane exists for), and
        // then flushed the whole set a burst later when the real cell diverged.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), G, (80, 24), now));
        p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 79), 'a')]));
        assert!(p.idle(), "control: the echo retired the guess");

        assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now));
        let shown = p.overlay(now);
        assert_eq!(
            (shown[0].row, shown[0].col, shown[0].ch),
            (1, 0, 'b'),
            "a parked cursor wraps the next glyph to the following row"
        );
    }

    #[test]
    fn a_wide_glyph_filling_the_last_two_columns_parks_the_cursor() {
        // `日` echoed into columns 78-79 leaves the terminal parked on its right half,
        // a cell `observe` reads as blank. Read as a free last column, the next guess
        // went onto that continuation (the painter refuses it) and the one after to
        // (1, 0): the cell where the refused glyph really echoes. Under the wide
        // ambiguous policy `é` is such a glyph too; under the narrow one it fills
        // column 78 alone and column 79 is still free.
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
        // A config reload mid-typing switches `ambiguous_width`: the guesses in flight
        // were measured with the old width, and the terminal now writes the line's
        // ambiguous glyphs with the new one, so they go — and are owed, so their
        // echoes prove nothing later. Setting the width it already has is no change.
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
        // The complement, and the reason the parking is read from the CELL rather than
        // from the cursor column: same position, empty cell, nothing parked. The
        // program will put the next glyph exactly there, so wrapping it would leave a
        // hole at the margin and shift the whole tail of the line one cell.
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
        // There is no row to wrap INTO, and the parked cell is echoed content that is
        // not ours to overwrite — so the only honest answer is to decline. (The
        // anchor-extended twin of this edge is `bottom_row_declines_without_flushing`.)
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (23, 79), G, (80, 24), now));
        p.reconcile(Some((23, 79)), G, false, now, cell(&[((23, 79), 'a')]));
        assert!(
            !p.predict_char_in_grid('b', (23, 79), G, (80, 24), now),
            "no row 24 to wrap into, and the slot holds the echoed 'a'"
        );
        assert!(p.idle(), "declining paints nothing and erases nothing");
    }

    #[test]
    fn a_wrap_the_terminal_did_not_defer_keeps_the_wrapped_guess() {
        // Not every wrap is parked: a program that emits the next glyph in the same
        // burst (or a host sampling the cursor after it) reports (row+1, 0) directly.
        // The wrapped guess sits exactly there, so the PLAIN cursor-consistency arm
        // must accept it — the deferred-wrap tolerance is an addition, not a
        // replacement, and a wrapped guess must not depend on the parking to survive.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (0, 79), G, (80, 24), now));
        assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now)); // wraps to (1,0)
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
        // Guesses carry ABSOLUTE rows, so a scroll between arming and echo slides the
        // cell every one of them is waiting on. The cursor scrolls WITH the text, so
        // the deferred-wrap tolerance must keep insisting on the exact previous row:
        // a one-row-off match would hold a wrapped guess against a cell that now
        // belongs to a different line.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char_in_grid('a', (5, 79), G, (80, 24), now));
        assert!(p.predict_char_in_grid('b', (5, 79), G, (80, 24), now)); // wraps to (6,0)
        p.reconcile(Some((5, 79)), G, false, now, cell(&[((5, 79), 'a')]));
        assert_eq!(p.overlay(now).len(), 1, "control: the park keeps 'b' alive");

        // One line of output scrolls the grid: the typed line — and the cursor parked
        // at its margin — are a row higher, and row 6 is still unwritten.
        p.reconcile(Some((4, 79)), G, false, now, cell(&[((4, 79), 'a')]));
        assert!(p.idle(), "a stale absolute row is retired, not tolerated");
    }

    #[test]
    fn a_scroll_retires_guesses_instead_of_confirming_them_against_moved_cells() {
        // The confirmation loop compares GLYPHS at absolute rows and runs BEFORE the
        // cursor-consistency check, so a scroll that slides another line's identical
        // character under a pending guess confirms an echo that never happened: a bogus
        // RTT sample, and (Adaptive) a display gate armed by a line that never echoed.
        // The guesses are not wrong, their COORDINATES are, and nothing in this crate
        // can re-derive them — but the host's generation says they moved.
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
        // One line of output scrolled the screen before the reconcile ran.
        p.reconcile(Some((5, 1)), G + 1, false, now, cell(&[((5, 0), 'a')]));
        assert!(p.idle(), "a scrolled guess is retired, not re-aimed");
        assert!(
            !p.confirmed_epoch,
            "…so a moved cell can never arm the display gate"
        );
    }

    /// The line the cursor is on after a scroll is a different line, even on the same
    /// physical row (the bottom row of a terminal in use). However the previous line
    /// ended — Enter, a key we never saw, or the program itself (`read -n1` takes its
    /// answer without one) — the prompt that scrolled in must re-prove its echo.
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
            // The program ends the line itself and prints a password prompt; at the
            // bottom of the screen that scrolls, and the prompt takes the same row.
            // Three columns wide, it leaves the cursor in the very cell the last
            // echo did, so only the generation can tell the lines apart.
            p.reconcile(Some((r, 3)), coords_after, false, at, cell(&[]));
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

    /// Shift+Enter, Ctrl+J, Ctrl+M: a line terminator in any spelling
    /// (`note_line_submit`). What is typed after it — a password typed ahead of the
    /// prompt the submit produces — must not inherit the proof; what was typed before
    /// it keeps its pixels, across any number of terminators.
    #[test]
    fn a_line_terminator_makes_what_follows_tentative() {
        let slow = Duration::from_millis(60);
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "control: the proven line paints");

        p.note_line_submit(at); // the submit
        p.note_line_submit(at); // and another terminator before the echo
        // Typed ahead of the prompt, while 'c' is in flight: the live cursor still
        // stands on 'c', so this glyph has no cell of its own until the
        // terminator's echo decides one. Declined, not armed on top of 'c'.
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
        // A confirmed echo in the new epoch is what re-arms the display, once the
        // echoes the boundary left in flight (the terminators, the declined 'h')
        // are past their window and cannot be standing in the new guess's cell.
        at += p.glitch_window() + Duration::from_millis(1);
        at = echo_turn(&mut p, 'x', 3, at, slow);
        assert!(p.predict_char('y', (0, 4), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "a re-proven line paints again");
    }

    /// `ls⏎cd` typed ahead on a slow link in `Always`. Reconciliation holds the real
    /// cursor on the oldest unechoed guess, so a live-cursor anchor put 'c' on the
    /// very cell 'l' was painted on: the new line painted over the old line's tail
    /// (two guesses on one cell; `reconciled_view_safety.rs` found it in three ops).
    #[test]
    fn nothing_is_armed_behind_a_terminator_while_its_line_is_in_flight() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(p.predict_char('l', (0, 2), G, 80, now));
        assert!(p.predict_char('s', (0, 2), G, 80, now));
        p.note_line_submit(now);
        // The real cursor still stands on 'l': nothing has echoed.
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
        // Once the line has echoed and drained, the live cursor anchors again.
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

    /// `ls⏎ls` typed ahead in Adaptive: the second "ls" was armed on the first
    /// one's cells, so the first line's echo CONFIRMED it, proving the new line
    /// at the cell after "ls" on the submitted row. The next glyph was then painted
    /// there, though it echoes on the prompt row that follows.
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
        // The line echoes; the terminator's echo has not arrived yet.
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

    /// A guess flushed while its keystroke is still on its way, then the same glyph
    /// typed at the new live cursor: the FIRST keystroke's echo lands in the second
    /// guess's cell and the cursor steps past it, exactly as the second's own echo
    /// would. Crediting it proved the epoch one keystroke behind, and the space
    /// typed next was painted where the second 'n' then echoed
    /// (`reconciled_view_safety.rs`'s fidelity clause, shrunk to fifteen ops).
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
        // 'n' is typed; before its echo the program moves the cursor to a new row.
        assert!(p.predict_char('n', (0, 2), G, 80, at));
        at += Duration::from_millis(10);
        p.reconcile(Some((1, 2)), G, false, at, cell(&screen));
        assert!(
            p.idle(),
            "the cursor left the guess: flushed, its echo still owed"
        );
        // 'n' again at the new cursor; then the FIRST 'n' echoes there.
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
        // The second 'n' echoes; the space guess diverges and goes.
        at += slow;
        let second_echo = [((0, 0), 'a'), ((0, 1), 'b'), ((1, 2), 'n'), ((1, 3), 'n')];
        p.reconcile(Some((1, 4)), G, false, at, cell(&second_echo));
        assert!(p.idle());
        // Control: once nothing owed can still be in flight, an echo proves its
        // guess again and the next glyph paints.
        at += p.glitch_window() + Duration::from_millis(1);
        assert!(p.predict_char('q', (1, 5), G, 80, at));
        at += slow;
        let q_echo = [((1, 2), 'n'), ((1, 3), 'n'), ((1, 5), 'q')];
        p.reconcile(Some((1, 6)), G, false, at, cell(&q_echo));
        assert!(p.confirmed_epoch, "an attributable echo proves the epoch");
        assert!(p.predict_char('r', (1, 6), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1);
    }

    /// A slow (60 ms), proven line "ab" on row 0 with the cursor at (0, 2), and the
    /// instant its last echo landed. The glitch window is the 250 ms floor.
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

    /// `cells` after the fixture line "ab".
    fn after_ab(more: &[((u16, u16), char)]) -> Vec<((u16, u16), char)> {
        AB.iter().chain(more).copied().collect()
    }

    /// The press that owes an echo, and the glyph that echo puts at the cursor, for
    /// each way a keystroke stops being tracked with nothing of ours in flight.
    /// After it the same glyph is typed at the cursor, and the owed output lands
    /// in that guess's cell first: glyph and cursor advance match, so only the owe
    /// keeps the match from proving the epoch, and the glyph typed next from being
    /// painted where the guess's own echo then lands. Deleting any one of these
    /// owes fails its case.
    #[test]
    fn every_untracked_press_owes_its_echo() {
        // A `[y/N]` prompt answers Enter with its default, printed at the cursor.
        let submit: fn(&mut Predictor, Instant) = |p, at| p.note_line_submit(at);
        // A completion (Tab), or a Right arrow re-printing the cell it steps over.
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

    /// A declined glyph is still sent. `a日n`: the wide glyph is declined, and so is
    /// the `n` typed behind it (`anchor_lost`). Once `a` and `日` have echoed, `n`
    /// is typed again at the live cursor, and the FIRST `n`'s echo lands there.
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

    /// A Backspace with nothing of ours to cancel erases echoed text: the live
    /// cursor is one cell right of where the next glyphs echo. `n`, `n` typed
    /// before the erase echoes are guessed at (0, 2) and (0, 3); they echo at
    /// (0, 1) and (0, 2), so the SECOND `n`'s echo matches the first guess.
    /// Credited, it proved the epoch and painted the second guess at (0, 3), a
    /// ghost of a key that had already echoed one cell to its left.
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

    /// An echo owed to a flushed key retires the first guess typed after it
    /// without credit — and then that guess's OWN echo is owed: it lands one cell
    /// on, in the next guess's cell. Not owing it, the second match came just
    /// after the flushed key's window closed and was credited, proving the epoch
    /// one cell behind with every echo inside 250 ms of its own key
    /// (`reconciled_view_safety.rs`'s fidelity clause, in review).
    #[test]
    fn a_guess_retired_without_credit_owes_its_own_echo() {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        // K = 'n' is typed and painted; 10 ms later a frame catches a status-line
        // redraw with the cursor away from it: flushed, owed.
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
        // The cursor is back; 190 ms after K the user types "nnc".
        let typed = k + Duration::from_millis(190);
        for ch in ['n', 'n', 'c'] {
            assert!(p.predict_char(ch, (0, 2), G, 80, typed));
        }
        // 230 ms after K, K's echo lands in the first new guess's cell: no credit.
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
        // 260 ms after K (its window has closed), 70 ms after it was typed, the
        // first new guess's own echo lands in the second's cell.
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
        // The second 'n' echoes where 'c' waits: it diverges and goes.
        let third = after_ab(&[((0, 2), 'n'), ((0, 3), 'n'), ((0, 4), 'n')]);
        let at = at + Duration::from_millis(30);
        p.reconcile(Some((0, 5)), G, false, at, cell(&third));
        assert!(p.idle());
    }

    /// An echo owed from before a CARRIED guess (one typed before the line's
    /// terminator) may stand in its cell like any other's: matched by it, the
    /// carried guess fed the estimate a 5 ms sample belonging to another key.
    /// Control: a carried guess with nothing owed from before it still measures
    /// the link while its own terminator's echo is owed, since that echo lands
    /// after it.
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

        // Control: past every window, a carried tail measures as it always did.
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

    /// An owe is dated by its PRESS. Dated by the host's next clocked call
    /// instead, an Enter followed by an idle minute in a split pane (which makes no
    /// predictor call while nothing is in flight) was owed from the next keystroke,
    /// whose own echo then fell inside the window: that line could never prove
    /// itself, and its glyphs stayed dark on the slow link they exist for.
    #[test]
    fn an_owe_is_dated_by_its_press() {
        let rtt = Duration::from_millis(100);
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), rtt);
        at = echo_turn(&mut p, 'b', 1, at, rtt);
        p.note_line_submit(at); // Enter, nothing in flight; no other call follows
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

    /// `ls⏎c⌫` on a slow link: `c` is declined (nothing is armed behind a submit in
    /// flight), and the Backspace erases `c` — not the submitted line's tail, which
    /// still echoes exactly where it is painted. Likewise a Backspace after a
    /// declined wide glyph erases that glyph and leaves the guesses before it.
    /// A second Backspace has nothing declined left to cancel and fails closed.
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
        // A Backspace after a terminator erases on the new line, never a glyph
        // declined before it.
        assert!(!p.predict_char('日', (0, 2), G, 80, at));
        p.note_line_submit(at);
        assert!(!p.predict_backspace(at));
        assert!(p.idle(), "fail closed: the erase may reach the carried 'x'");
    }

    /// Tab, an arrow, a paste, an IME commit: input we did not model that is not a
    /// line terminator (`note_unmodelled_input`). What follows is tentative, and
    /// the guesses in flight stop painting in every mode, since the input may have
    /// erased or displaced them; their echo draws them one round trip later.
    #[test]
    fn a_key_we_did_not_model_retires_the_guesses_in_flight() {
        let slow = Duration::from_millis(60);
        for mode in [PredictMode::Adaptive, PredictMode::Always] {
            let mut p = Predictor::new(mode);
            let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
            at = echo_turn(&mut p, 'b', 1, at, slow);
            assert!(p.predict_char('c', (0, 2), G, 80, at));
            assert_eq!(p.overlay(at).len(), 1, "{mode:?}: control: 'c' paints");

            p.note_unmodelled_input(at); // Tab
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
                // The echo of 'c' lands; the guess anchored on the stale cursor
                // diverges.
                at += slow;
                p.reconcile(Some((0, 3)), G, false, at, cell(&[((0, 2), 'c')]));
                assert!(p.idle());
                // Re-proven once the Tab's (and the dropped guesses') echoes are
                // past their window (see `attributable`).
                at += p.glitch_window() + Duration::from_millis(1);
                at = echo_turn(&mut p, 'x', 3, at, slow);
                assert!(p.predict_char('y', (0, 4), G, 80, at));
                assert_eq!(p.overlay(at).len(), 1, "a re-proven line paints again");
            }
        }
    }

    /// An erase we did not model may land on a guess carried across a line
    /// terminator the program did not take as a submit (a composer that inserts
    /// the newline): the Backspace may be erasing the 'c' we are still painting,
    /// so the carried pixels stop rather than become a stale ghost.
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

    /// A backward erase that is not a Backspace of our own — Ctrl+W, Ctrl+H, a
    /// remapped kill, a BS an embedder handed over as a glyph — landing on the
    /// typed-ahead text. Once it echoes, the real cursor sits on one of the erased
    /// guesses over a blank cell, which reconciliation cannot tell from "not echoed
    /// yet": a guess left there is a ghost of the erased text for the whole glitch
    /// window, in any mode.
    #[test]
    fn an_erase_landing_on_typed_ahead_text_leaves_no_ghost() {
        type Erase = fn(&mut Predictor, Instant);
        type Cells = &'static [((u16, u16), char)];
        let killed: Cells = &[((0, 0), 'l'), ((0, 1), 's')];
        let one_erased: Cells = &[((0, 0), 'l'), ((0, 1), 's'), ((0, 3), 'f'), ((0, 4), 'o')];
        // (label, the erase, the cells its echo leaves, the cursor column after it)
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
                // The program echoes "foo", then the erase.
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

    /// The byte rule for hosts that have the bytes rather than the key: a line
    /// terminator ends the text, and nothing in it could erase or move what was
    /// typed before.
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
        // The link RTT is a property of the SESSION, not of the window. The host calls
        // its front-change path on every tab switch, so CLEARING the estimate there
        // made a tab-heavy ssh workflow re-earn the display latch after each switch —
        // the first SLOW_SAMPLES_TO_DISPLAY characters of every line unpredicted, for a
        // link that never changed.
        let mut p = Predictor::new(PredictMode::Adaptive);
        let now = t0();
        let slow = Duration::from_millis(60);
        let fast = Duration::from_millis(1);
        let mut at = echo_turn(&mut p, 'a', 0, now, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "control: the slow link paints");

        // The ssh session goes to the background; its estimate leaves with it.
        let ssh = p.take_link_estimate();
        assert!(
            p.idle(),
            "the outgoing pane's guesses do not follow the window"
        );
        // The local pane in front now speculates on its OWN evidence only.
        at = echo_turn(&mut p, 'x', 0, at, fast);
        assert!(p.predict_char('y', (0, 1), G, 80, at));
        assert!(p.overlay(at).is_empty(), "a fast local link stays dark");
        let local = p.take_link_estimate();

        // Back to the ssh session: ONE confirmed echo on the new line is enough,
        // because the LINK never stopped being slow — only the line has to re-prove
        // itself (the no-unechoed-flash guarantee is per line, and still earned here).
        p.restore_link_estimate(ssh);
        // The parked 'c' was still in flight. Its old output cannot prove the
        // returning line until that debt's window has passed.
        at += p.glitch_window() + Duration::from_millis(1);
        at = echo_turn(&mut p, 'a', 0, at, slow);
        assert!(p.predict_char('b', (0, 1), G, 80, at));
        assert_eq!(
            p.overlay(at).len(),
            1,
            "a restored slow-link estimate paints the second keystroke"
        );

        // …and the local session's own estimate comes back just as measured.
        p.restore_link_estimate(local);
        at = echo_turn(&mut p, 'x', 0, at, fast);
        assert!(p.predict_char('y', (0, 1), G, 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "restoring a fast session's estimate must not paint either"
        );
    }

    /// A parked session still owns output its already-written keys may produce.
    /// Returning to it keeps the measured link, but an old echo is not a proof of
    /// a new guess for the same glyph, even when both arrive inside the deadline.
    #[test]
    fn a_returning_sessions_old_echo_cannot_prove_its_new_guess() {
        let slow = Duration::from_millis(60);
        let start = t0();

        // Debt-free control: a known slow link needs only this line's own echo
        // after a normal round trip, without waiting or relearning the link.
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
                // Its first 'n' is already written but has not echoed when this
                // session goes behind another pane. Taking drops its pixels.
                assert!(p.predict_char('n', (0, 2), G, 80, at));
                assert_eq!(p.overlay(at).len(), 1);
            } else {
                // The same custody applies to unmodelled input: a Right arrow,
                // for example, may reprint an existing 'n'.
                p.note_unmodelled_input(at);
            }
            let parked = p.take_link_estimate();
            assert!(p.idle());
            assert!(p.overlay(at).is_empty(), "outgoing pixels do not travel");

            // The intervening pane gets only its own estimate. This is a
            // different session, so its own echo can confirm normally.
            at = echo_turn(&mut p, 'x', 0, at, Duration::from_millis(1));
            assert!(p.confirmed_epoch);
            let _other = p.take_link_estimate();
            p.restore_link_estimate(parked);
            assert!(p.adaptive_slow, "the returning link is still known slow");
            assert!(!p.confirmed_epoch, "a restored line must prove itself");

            // The returning session's cursor has not moved yet. Type 'n'; the
            // earlier key's output then puts 'n' in its cell and advances past
            // it. This is inside the shortest 250 ms window, not a late echo.
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

            // A later real echo diverges at the space's guessed cell. Once the
            // owed window passes, a new attributable echo proves the line again.
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

    /// REGRESSION (adversarial review): `restore_link_estimate` must RESET before it
    /// installs, and that line must be pinned by a test that fails without it.
    ///
    /// The sibling test drives the swap through `take_link_estimate` first — which
    /// already resets — so deleting `self.reset()` from `restore_link_estimate` left
    /// every test passing while the guarantee it protects was gone. This drives
    /// `restore` DIRECTLY on a pane with a displayed guess, which is what a host does
    /// when it installs a parked estimate into a predictor it did not just take from.
    /// Without the reset, pane A's ghost survives onto pane B's screen.
    #[test]
    fn restoring_an_estimate_clears_the_outgoing_panes_pixels() {
        let mut a = Predictor::new(PredictMode::Always);
        let now = t0();
        assert!(a.predict_char('x', (3, 4), G, 80, now));
        assert_eq!(a.overlay(now).len(), 1, "pane A has a ghost on glass");
        assert!(a.is_displaying(now));

        // Install some OTHER session's link estimate directly, with A's guess live.
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
        // The contrast that makes the swap worth having, and the guard against the two
        // being confused: what the host holds for a session it has never measured is
        // `LinkEstimate::default()`, and restoring THAT must still re-earn the latch.
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
        // The expiry backoff measures the same thing the RTT does — how long THIS link
        // makes a guess wait — so it belongs to the estimate. Left behind, a widened
        // window outlives its link and makes the next pane sit on a wrong guess for up
        // to the ceiling; re-walked from the floor, a slow link re-probes 250 → 2000 ms
        // after every tab switch before it can confirm anything at all.
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
        // `reset_session` is the "nothing known about this link" path, so it must leave
        // NOTHING measured behind. A surviving backoff is a stale window: the new pane
        // would wait up to 2 s before a wrong guess self-heals on a link that never
        // timed out once.
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

    /// A Backspace with nothing of OURS to pop erases a glyph the program already
    /// echoed: the real cursor will step LEFT, so the live cursor is stale until
    /// that erase echoes. A glyph typed meanwhile was guessed one cell right of
    /// where it lands, and after the echo the real cursor sits exactly on that
    /// guess (erase -1, glyph +1), so the consistency check kept it: a duplicate
    /// ghost on glass for the whole glitch window ("ac" + a dim "c").
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
        // The program erases 'b' and echoes 'c' in its place.
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
        // A typed space echoes as a blank cell (observe → None), so it must confirm by
        // the cursor advancing past it, not wedge the queue + flush trailing type-ahead.
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        p.predict_char('a', (0, 0), G, 80, now);
        p.predict_char(' ', (0, 0), G, 80, now); // extends to (0,1)
        p.predict_char('b', (0, 0), G, 80, now); // extends to (0,2)
        // Echo: 'a' at (0,0); the space is blank; the cursor advanced to (0,2).
        p.reconcile(Some((0, 2)), G, false, now, cell(&[((0, 0), 'a')]));
        let shown = p.overlay(now);
        assert_eq!(shown.len(), 1, "'a' and the space retire; only 'b' remains");
        assert_eq!(shown[0].ch, 'b');
    }

    #[test]
    fn pending_queue_caps_at_max_predictions() {
        let mut p = Predictor::new(PredictMode::Always);
        let now = t0();
        // A stalled echo under a held key: one line of type-ahead, wrapping.
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
        // Stalled echoes across line submits cannot grow it either: nothing is armed
        // behind a terminator while the submitted line is still in flight.
        for _ in 0..4 {
            p.note_line_submit(now);
            assert!(!p.predict_char_in_grid('b', (0, 0), G, (80, 24), now));
        }
        assert_eq!(p.preds.len(), MAX_PREDICTIONS);
    }

    /// Row 0 of a 12-column screen as a host projects it: `text` from column 0,
    /// blank after.
    fn seen_row(text: &str) -> Vec<Seen> {
        let mut row: Vec<Seen> = text.chars().map(|c| Seen::of_cell(c, false)).collect();
        row.resize(12, Seen::Blank);
        row
    }

    /// An `observe` over row 0 holding `text` from column 0.
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

    /// A slow (60 ms) proven epoch on row 0 of a 12-column screen; the program
    /// then shows `text` and leaves the cursor at `cursor`, and a key the
    /// predictor does not model (an arrow) ends the epoch. Returns the instant one
    /// glitch window later, when nothing owed can still be in flight.
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

    /// THE TAIL GUARD. `ab cd`, the cursor moved back onto the space, `e` typed:
    /// the echo inserts it and shifts ` cd` right. The match is `e`'s own (glyph,
    /// cursor advance, nothing owed), so it measures the link, but the row right
    /// of it is not what the press saw there, so it proves nothing, and `i` typed
    /// next stays dark: painted at (0, 3) over the shifted space, it showed
    /// `abeicd` until the screen settled to `abei cd`.
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

        // The `TailGuard` negative control is the code as it shipped: the same
        // echo proves, and 'i' paints.
        let mut p = Predictor::new(PredictMode::Adaptive);
        p.disable_rule_for_test(Rule::TailGuard);
        let at = slow_link_then(&mut p, "ab cd");
        let row = seen_row("ab cd");
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
        };
        assert!(p.predict_glyph('e', &key, at));
        let echoed = at + Duration::from_millis(60);
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abe cd"));
        assert!(p.confirmed_epoch, "rule off: the mid-line echo proves");
        assert!(p.predict_char('i', (0, 3), G, 12, echoed));
        assert_eq!(p.overlay(echoed).len(), 1);
    }

    /// A right prompt the echo leaves in place is DECORATION: the row right of
    /// the guess is what the press saw, so the line proves, and the next glyph
    /// paints. Without the press's row (a host that cannot read it) only a blank
    /// tail proves, so the same line stays dark: the guard fails closed.
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

    /// A guess typed over OTHER text is mid-line whatever the row shows when its
    /// echo lands: here the echo replaces the cell in place and the row right of
    /// it is unchanged, which is what a shift also leaves once enough inserts have
    /// pushed the tail off the row's edge (onto the next row of a wrapped line).
    /// Refusing both is the safe side. Typed over the same glyph (a suggestion
    /// the glyph matches), it still proves.
    ///
    /// The same holds of every guess of the run, the ones still pending when an
    /// earlier one is checked included: `e` typed over a blank and `q` over `x`
    /// in one burst, both echoed in place. `q`'s cell shows `q` with the cursor
    /// past it, which excuses it from the comparison as an echo of the same burst,
    /// so only what `q` was typed over refuses the proof.
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
            };
            assert!(p.predict_glyph('e', &key, at));
            assert!(p.predict_glyph('q', &key, at));
            let echoed = at + Duration::from_millis(60);
            p.reconcile_seen(Some((0, 4)), G, false, echoed, seen_cells("abeqd"));
            assert!(p.idle());
            assert_eq!(p.confirmed_epoch, proves, "'q' typed over {under:?}");
        }
    }

    /// A proof standing from earlier in the epoch is VOIDED by an echo whose tail
    /// moved, not merely left standing: the guesses behind that echo were placed
    /// as appends on a line that is not appending. Here text appears right of the
    /// line, past the cell `o` waits on, between the press and the echo of `n`,
    /// on a line whose presses carried no row: not known to be at the end of the
    /// line, it might be the line's own tail (beside a line known to end there,
    /// it proves: `a_suggestion_beside_the_end_of_the_line_darkens_no_key`).
    /// Control: with the guard off, `o` keeps painting.
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

    /// A burst retired in one turn scans its row once: once a guess's tail is
    /// clean, every later guess's on the row is a suffix of it.
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
        // Four guess cells, one tail scan (columns 3..12), the deferred-wrap probe
        // reads nothing (the cursor is not at the margin).
        assert_eq!(reads, 4 + 9, "observe calls for a four-guess burst");
    }

    /// One keystroke at a prompt on row 0 of a 12-column screen: `ch`, typed at
    /// the live cursor `(0, col)` over `screen`, echoed onto `echoed` with the
    /// cursor left at `(0, cursor)`.
    type Keystroke<'a> = (char, u16, &'a str, &'a str, u16);

    /// A typist's `key`, one per round trip: typed at `at` with the press's row
    /// (or none, `with_row`), a frame 10 ms later that still shows its screen, and
    /// its echo 100 ms after the press. Returns whether the guess was on glass at
    /// that frame, and when the next key comes.
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

    /// THE END OF THE LINE. A run whose press saw nothing from the cursor to the
    /// margin is an append whatever the shell draws right of it afterwards: here
    /// `h`'s echo brings up the autosuggestion `o hello`, `x` is typed onto its
    /// first glyph (diverging, so the shell clears it) and `y`'s echo brings up
    /// `zz`, which `z` then follows. Every key after the link is known slow is on
    /// glass where it echoes, as the shipped predictor painted them; the tail
    /// comparison alone refused `h`'s echo (text appeared beside it) and `x`'s
    /// (typed over other text), darkening two keys per change on fish,
    /// zsh-autosuggestions, PSReadLine and ble.sh. `x` waits on the suggestion's
    /// `o` it was typed over, the screen as its press saw it, instead of being
    /// flushed by it; `z`, on a `z` already drawn, waits for its echo's cursor
    /// advance. Control: presses that carry no row do not know where the line
    /// ends, and the suggestion `h` brings up voids the proof.
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
                // `h`'s echo brings up the suggestion beside a line not known to
                // end: refused, and `x`, unproven, is flushed by the suggestion's
                // `o` it was typed over, owing its echo past `z`'s.
                assert_eq!(shown, [false, false, true, false, false, false], "no row");
                assert_eq!(events.tail_refusals, 1, "{events:?}");
            }
        }
    }

    /// THE STANDING TAIL. `ab cd`, the cursor onto the space, `e` typed, and a far
    /// end that writes the glyph before it redraws the tail it shifts (`e`, then
    /// ` cd` and the cursor stepped back), its echo split between two frames. The
    /// first shows `abecd` with the cursor past `e`: the row right of `e` is what
    /// the press saw there, so `e` proves. `i`, typed on that proof, is held to it:
    /// once the redraw lands (`abe cd`) the row right of `e` has moved and the
    /// proof is voided before `i` is painted over the shifted space (`abeicd` on
    /// glass, settling to `abei cd`); typed after the redraw, its own press reads
    /// the moved row. Control: with the rule off, `i` paints in both orders.
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

    /// The pending-guess half of the tail comparison takes a glyph in a guess's
    /// cell for its echo only with the real cursor past it. `abo`, the cursor onto
    /// the last `o`, `o` typed over it and its echo split as above (glyph, then
    /// the displaced `o` one cell on): the glyph alone changes nothing, so `o`
    /// proves; `o` and `z` typed on that proof; then the redraw puts the displaced
    /// `o` in the second `o`'s cell with the cursor on it, not past it. Taken for
    /// that guess's echo it kept the proof, and `z` was painted where the
    /// displaced `o` settles (`aboz` on glass, `abozo` settled).
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

    /// THE CURSOR ADVANCE. Every echo comes 300 ms after its key. With a proven
    /// line, the shell draws the autosuggestion `dice` after the cursor, and
    /// `d`, `i`, `c`, `e` are typed along it: their cells already show their
    /// glyphs. Retired on that, before the far end read one of them, they were
    /// credited and measured the link at a few milliseconds each, and the
    /// smoothed RTT fell from 300 ms toward nothing, taking the window every owe
    /// is judged by with it. Held for the cursor to pass them, they retire on
    /// their own echoes, with their real samples. Control: with the rule off the
    /// first frame retires all four and the window collapses.
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

    /// A Backspace that cancels our own trailing guess OWES the cancelled glyph's
    /// echo, typed: it still lands, on the cell the next guess anchors on. `c`
    /// typed and cancelled, then `c` again: the first `c`'s echo lands in the new
    /// guess's cell with the cursor past it, exactly as its own would, and was
    /// credited with it. Control: with the rule off it is.
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
            // Not credited, it voids the proof (`Rule::UncreditedVoids`): the
            // erase in flight puts the cursor back on that cell.
            assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
        }
    }

    /// A row the frame reorders (right-to-left text) reads opaque on every written
    /// cell, its blanks included: `Opaque(None)`. They are blanks all the same, so
    /// a guess typed on one, with a right prompt further along that its echo
    /// leaves in place, proves like on a left-to-right row.
    #[test]
    fn a_right_to_left_line_proves_over_its_opaque_blanks() {
        let opaque = |text: &str| -> Vec<Seen> {
            let mut row: Vec<Seen> = text.chars().map(|c| Seen::of_cell(c, true)).collect();
            row.resize(12, Seen::Blank);
            row
        };
        let mut p = Predictor::new(PredictMode::Adaptive);
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
        };
        assert!(p.predict_glyph('e', &key, at));
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

    /// A host asks for the row before it applies the display mode (aterm-gui
    /// reads it under the press's hold and calls `set_mode` after), so a
    /// predictor still `Off`, in a window that never predicted, wants it all the
    /// same: otherwise the press that starts its first run carried none, and a
    /// line with a right prompt proved nothing until its typed-ahead queue
    /// drained.
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
        };
        assert!(p.predict_glyph('c', &key, t0()));
        assert!(!p.wants_key_row(), "type-ahead reads no row");
        assert_eq!(p.tail, Some((0, 2)), "the first run took its snapshot");
    }

    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// A slow proven line `ab`, Enter (its echo, a prompt on row 1, owed), `x`
    /// typed on row 1 100 ms later and echoed 60 ms after that: `x`'s own echo,
    /// but the Enter's is owed, so it is retired without credit (`echo_x` is the
    /// screen that echo leaves). Then `second` is typed and echoed 270 ms after
    /// the Enter, past its window, onto a clean row. Returns whether the glyph
    /// typed next paints: the line proved.
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

    /// TYPED OWES. `x` retired without credit owes its own echo as `x`: it lands
    /// in the next guess's cell, so it refuses credit to a later `x` there and to
    /// nothing else. `y`'s echo, past the Enter's window, is credited and proves
    /// the line; owed as any echo (the shipped cascade, `TypedOwes` off) it was
    /// refused, and every guess after it in turn while keys came faster than the
    /// window. A typed owe that refuses nothing (`TypedOwesSameGlyph` off)
    /// credits the second `x` with what may be the first's echo.
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

    /// A guess retired without credit over a MOVED tail is owed as any echo: it
    /// was typed mid-line, and its echo shifts text whose glyphs could stand in
    /// any later guess's cell. Here text shows right of `x` when it retires (and
    /// is gone when `y` echoes), so `y` is refused credit; with
    /// `TypedOwesCleanTail` off the owe is typed and `y` proves the line.
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

    /// The typed owes are bounded: past `MAX_GLYPH_OWES` the oldest folds into
    /// the owe of any echo, raised for every guess still pending; and they
    /// travel with the session as owes of any echo.
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

    /// A cancelled pair may put the cursor on its cell or just beyond it, but an
    /// unrelated move cannot carry that cell's proof to the next key.
    #[test]
    fn a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move() {
        fn proven(cols: u16, start: u16) -> (Predictor, Instant) {
            let mut predictor = Predictor::new(PredictMode::Adaptive);
            let mut at = t0();
            let mut cells = Vec::new();
            for (offset, ch) in [(0, 'a'), (1, 'b')] {
                let col = start + offset;
                assert!(predictor.predict_char_in_grid(ch, (0, col), G, (cols, 6), at));
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

        // Ordinary pair positions are admitted before any adverse observation.
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

        // Before a wrapped cancelled glyph echoes, the real cursor may still be
        // parked on the preceding row's final column.
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

    /// THE CANCELLED PAIR'S TRANSIT. `c` is typed and cancelled by a Backspace
    /// before either echoes; `c`'s echo lands first, the screen a cell ahead, and
    /// two spaces are typed then. The erase lands, the spaces echo at (0, 2) and
    /// (0, 3). With the rule the spaces anchor on the cancelled cell, where they
    /// echo, and their own echoes prove the line. Without it (`CancelTransit`
    /// off: the shipped code) they anchor on the live cursor a cell right, the
    /// second space's echo confirms the first by the cursor advancing past it,
    /// that credit proves the line a cell behind, and the second space is painted
    /// at (0, 4), where it does not echo.
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
            assert!(p.predict_char(' ', (0, 3), G, 80, ahead));
            assert!(p.predict_char(' ', (0, 3), G, 80, ahead));
            let cells: Vec<(u16, u16)> = p.preds.iter().map(|g| (g.row, g.col)).collect();
            (p, ahead, cells)
        };

        let (mut p, ahead, cells) = transit(true);
        assert_eq!(cells, [(0, 2), (0, 3)], "anchored on the cancelled cell");
        let landed = ahead + MS(60);
        p.reconcile(Some((0, 4)), G, false, landed, cell(&AB));
        assert!(p.idle(), "both spaces echoed where they were armed");
        assert!(p.confirmed_epoch, "and their own echoes proved the line");

        let (mut p, ahead, cells) = transit(false);
        assert_eq!(cells, [(0, 3), (0, 4)], "rule off: anchored a cell right");
        let landed = ahead + MS(60);
        p.reconcile(Some((0, 4)), G, false, landed, cell(&AB));
        assert_eq!(
            p.preds.len(),
            1,
            "the first space retired by the cursor advance"
        );
        assert!(
            p.confirmed_epoch,
            "rule off: credited with the second space's echo"
        );
        assert_eq!(p.overlay(landed).len(), 1, "rule off: painted at (0, 4)");
    }

    /// The cancelled glyph filled the LAST column: its echo parks the terminal in
    /// the deferred-wrap slot without moving the cursor, so the proof's cell still
    /// matched the live cursor and the next glyph, anchored there, wrapped to the
    /// next row while it echoes in the last column once the erase un-parks it. With
    /// the rule it anchors on the cancelled cell.
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

    /// `reconcile` is `reconcile_seen` with each glyph a plain `Glyph`; and an
    /// OPAQUE cell is read by the glyph lane as the glyph it carries, so a host
    /// that marks wide or marked cells opaque retires, flushes and wraps exactly
    /// as the plain observe did.
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
        for screen in screens {
            let mut plain = Predictor::new(PredictMode::Always);
            let mut opaque = Predictor::new(PredictMode::Always);
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

    // THE EDIT LANE: state chains on the known line.

    /// The echo round trip of the edit lane's tests: slow enough to open the
    /// Adaptive gate, short of the 250 ms window floor.
    const RTT: Duration = Duration::from_millis(60);

    /// A press's key-time sample on row 0 of a 12-column, 6-row screen showing
    /// `screen`, the cursor at `(0, cursor)`.
    fn sample(row: &[Seen], cursor: u16) -> KeySample<'_> {
        KeySample {
            cursor: (0, cursor),
            coords: G,
            grid: (12, 6),
            row: Some(row),
        }
    }

    /// The column after the last character of `screen`.
    fn end_of(screen: &str) -> u16 {
        u16::try_from(screen.chars().count()).unwrap_or(u16::MAX)
    }

    /// Type `text` after `screen` on row 0, one glyph a round trip: each press
    /// with its row, each echo through `reconcile_seen` (a host that says which
    /// cells are opaque). Returns the screen after it and when its last echo
    /// landed.
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

    /// A slow, proven line at a `$ ` prompt on row 0: `text` (two glyphs or more,
    /// so the Adaptive gate opens) typed and echoed, so the known line holds it
    /// from column 2 with the cursor at its end. Returns the predictor, the
    /// screen and when the last echo landed.
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

    /// Press edit `key` with the cursor at `(0, cursor)` over row 0 showing
    /// `screen`.
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

    /// What a frame shows of the predictor: the overlay's cells `(col, ch)` on
    /// row 0, and its cursor.
    type Shown = (Vec<(u16, char)>, Option<(u16, u16)>);

    /// A frame over row 0 showing `screen` with the cursor at `(0, cursor)`: the
    /// reconcile, then what the overlay shows.
    fn frame(p: &mut Predictor, screen: &str, cursor: u16, at: Instant) -> Shown {
        p.reconcile_seen(Some((0, cursor)), G, false, at, seen_cells(screen));
        let cells = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
        (cells, p.overlay_cursor())
    }

    /// THE CLASS LADDER. `←` on a proven line is armed and tracked, but dark: no
    /// motion of this epoch has echoed, and a cooked tty echoes `^[[D` for it.
    /// Its echo retires it by state (the cursor one back, every cell as it was),
    /// which proves the motion class, and the next `←` shows at once: no cell,
    /// only the cursor. Control: with the class proof off, the first `←` shows.
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

    /// `←→` typed in one burst, the screen still showing the state before them:
    /// it equals the chain's last state too, but a screen showing `S_0` has read
    /// no key. The SMALLEST match retires nothing, and the NET-NULL tail retires
    /// without credit, owing both keys, so nothing on glass changes and nothing
    /// waits for a state the screen never leaves. Controls: the largest match,
    /// and a credited net-null tail, each credit two keys no far end has read.
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

    /// A Backspace at the end of the known text is an ERASE of the glyph class:
    /// it shows on the glyph proof, as the vacated cell painted blank and the
    /// cursor on it. Its echo retires it, the line shortens, and a glyph typed
    /// next is an append on the shortened line, shown at once.
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

    /// `abc`, two Backspaces before either echoes, and their echoes arriving in
    /// pieces: the first leaves `ab` on screen with the cursor on `b`. The
    /// cursor and text are those of `S_2` (`a`), but `b` still stands on the
    /// cell `S_2` vacated, so no state matches: every cell is blank or some
    /// state's, so the chain waits. Control: matching without the vacated cells
    /// retired the chain onto `a` with `b` still on screen.
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

    /// A split erase may wait for its own old cell to clear, but an unrelated
    /// cell past every state is not an erase echo. Keep the ordinary split echo
    /// before testing the same cursor/text with a new nonblank sentinel.
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

    /// An arrow the lane cannot place is DECLINED WITH ITS PIXELS KEPT: here the
    /// host reconciles through the legacy observe, which cannot say which cells
    /// are opaque, so no chain runs behind it. The guesses in flight still echo
    /// where they are painted (the far end reads the arrow after them), so they
    /// stay on glass, carried; the arrow is owed, the epoch ends, and nothing is
    /// armed until the queue drains. One reconcile through `reconcile_seen`
    /// later, the same arrow is armed.
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

    /// At an edge of the known text an arrow is declined: `→` at its end, `←` at
    /// `lo` (readline beeps, zsh does nothing, an autosuggestion is accepted).
    /// Control: with the edges off, both are armed as if the text went on.
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

    /// With the edit lane off every edit key is routed as before it existed: an
    /// arrow is input this crate does not model, a Backspace is
    /// `predict_backspace`'s.
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

    /// A glyph typed where a motion left the cursor inside the text is a
    /// MID-LINE insert, whose echo shifts the text right of it: the proof that
    /// covered the cursor's cell does not cover an append there, so the glyph is
    /// tracked dark and the line it shifts is no longer known.
    #[test]
    fn a_glyph_typed_inside_the_text_is_not_an_append() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        assert!(p.idle() && p.confirmed_epoch);
        let row = seen_row(&screen);
        assert!(p.predict_glyph('x', &sample(&row, 3), at));
        assert!(p.overlay(at).is_empty(), "not painted over `b`");
        assert_eq!(p.edit_line_for_test(), None);
    }

    /// A chain whose line shows an opaque cell is flushed (R-OPAQUE): a wide
    /// glyph's half is nothing a state can name. And a press whose row shows one
    /// arms nothing.
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

    /// An erase beside text past the end of the line (an autosuggestion drawn
    /// after it, a tail the line does not know) is not armed: the editor redraws
    /// over, or pulls into, the cell it vacates. The same text appearing while an
    /// erase is in flight flushes the chain. Control: with the refusal off, the
    /// erase is armed beside it.
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

    /// A chain whose head outlives the glitch window is flushed whole: every
    /// later state is built on the head's effect. Owed, with the newest key's
    /// press. Control: a chain that never expires stays on glass past the window.
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

    /// A chain carried across a line terminator keeps its pixels AND its
    /// predicted cursor: drawn back at the engine's cursor it is a key behind the
    /// line the frame shows. Control: a carried chain that hands no cursor.
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

    /// The motion proof goes with the glyph proof: a line submitted, then a new
    /// line typed and proven on the same row (a cooked program after the shell),
    /// and its first `←` is dark again. Control: a proof that outlives its line.
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
            // Past the window of the terminator's owed echo.
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

    /// A chain matches whole states: a line the program redrew with other text,
    /// the cursor where `←` leaves it, is no state of the chain, so it flushes.
    /// Control: matching the cursor alone retired the chain onto text the screen
    /// never held.
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

    /// A credited motion moves the proof's cell but never earns the glyph proof:
    /// here a right prompt moved while `←` was in flight, voiding the proof (the
    /// standing tail), and `←`'s echo then lands. Control: a motion's credit
    /// that re-proves the epoch.
    #[test]
    fn a_motion_echo_never_proves_its_epoch() {
        for rule_on in [true, false] {
            // Typed with a right prompt standing at column 8.
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
                !p.confirmed_epoch && p.preds.len() == 1,
                "voided, ← in flight"
            );
            p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ ab"));
            assert!(p.idle());
            assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
        }
    }

    /// An edit op the predictor stops tracking owes its key's echo: a declined
    /// arrow, and a flushed chain's motions. Control: neither owes.
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

    /// While a chain is in flight the presentation hull covers its line from
    /// `lo` to one past the furthest text end: the erase's vacated cell included.
    #[test]
    fn pending_bounds_cover_the_chain_s_line() {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        let _ = press(&mut p, EditKey::Backspace, &screen, 5, at);
        assert_eq!(p.pending_bounds(), Some((0, 2, 1, 6)));
    }

    /// The chain cap (R-CAP): past `EDIT_CHAIN_MAX` ops an arrow is declined as
    /// one the chain cannot place.
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

    /// THE KNOWN LINE's lifetime: started by the first credited append that
    /// proves, extended by each after it, kept as a chain's base while the chain
    /// is in flight even once the proof is voided, and dropped where its text
    /// could stop being the screen's unseen: a glyph at the last column, the
    /// program moving the cursor, a terminator with nothing carried.
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

    /// SINGLE CUSTODY: every key that reached the PTY and was not credited with
    /// its echo is held by a pending op, a typed owe, or the owe of any echo
    /// dated at or after its press, through a run of glyphs, arrows, a cancelled
    /// glyph, a declined arrow, a net-null burst and a divergence.
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

    /// A STALE ANCHOR: a Backspace that cancels nothing of ours is owed, and its
    /// echo steps the cursor back; `c`, `c` typed on the live cursor before it
    /// lands are placed a cell right of where they echo. Once the erase and both
    /// echoes land, the first guess's cell holds the SECOND `c`'s echo with the
    /// cursor past it, the Backspace's owe lapsed. Credited, it proved the line a
    /// cell behind; refused (a later guess for the same glyph is pending), it
    /// proves nothing. Control: the rule off credits it.
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

    /// A state chain keeps the terminal's deferred-wrap cell out of its row.
    /// Valid append and ordinary margin refusal precede the widest public grid.
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
}
