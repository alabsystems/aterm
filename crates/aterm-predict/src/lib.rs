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
//! declined with the pixels in flight kept), never noted as well. A glyph typed
//! where the known line's cursor stands inside its text, and a Backspace there,
//! are ops of a chain too: they move the text right of the cursor (a SHIFT),
//! and a frame paints the moved text where it lands ([`GhostCell::Moved`], which
//! a host paints as the cell it came from, in its own style).
//!
//! Home, End, Ctrl-A, Ctrl-E and Delete are LEARNED keys of the same lane: placed
//! only on a line started at the very cell the shell's authenticated OSC 133 `B`
//! names as its buffer's start ([`KeySample::input_start`], so the line's start
//! and end are the buffer's), and shown only once the far end's own echoes, two
//! of each key's canonical effect, have taught its binding in that editor
//! context ([`EditorContext`]); until then each is a dark probe.
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
//!   echo, and at none an earlier op's echo shows part-way (an erase caught
//!   before its step back); a motion shows only once a motion of its epoch has
//!   echoed exactly (THE CLASS LADDER: a cooked tty echoes `^[[D`), and so does a
//!   shift, once a shift of its kind has echoed moving a glyph (overwrite mode
//!   replaces where an insert shifts, and a tail of spaces moves invisibly); the
//!   class proofs go with the glyph proof; the chain expires whole. A chain is
//!   credited whole or not at all, an edit op only with no later key still
//!   echoing (an Enter's carriage return makes a cursor by itself), and a chain
//!   no state matches waits only with the cursor inside its text. While a chain
//!   is in flight on a line not proven at the end of the row, the row past every
//!   state of it holds what it held when the chain's base was verified, or less
//!   (a right prompt cleared): text appearing or moving there may be the
//!   buffer's own; and the press that starts a chain holds the standing proof to
//!   its row first, whatever is in flight, since the chain then holds only that
//!   row. A run typed on a cursor an owed echo may still move (a STALE ANCHOR) is
//!   credited by no later key's echo, pending or cancelled; a run typed inside
//!   the known text proves nothing; and a blank row is no evidence of the line's
//!   end while an arrow we declined, or a key we did not model, may still move
//!   the far end's cursor.
//! * **Positional evidence, bounded.** A cursor advance is an echo only as far
//!   as the run's echoes reach; a park in the last column only at the end of
//!   the line; an echo whose cell comes back opaque (a zero-width mark riding in
//!   from the buffer's hidden tail) is no append; and a space, whose echo is a
//!   blank any output can step over, proves no line by itself.
//! * **Only rows in logical order.** A glyph that makes its row reorder under
//!   UAX #9 (a right-to-left letter, an Arabic digit), and any key typed on a row
//!   that holds one, is declined: such a row is drawn in another order, and a
//!   key's echo there lands where the reorder puts it, which no column says.
//! * **A shift shows only once one has echoed telling itself apart.** A glyph
//!   or Backspace typed inside the known text moves the text right of the
//!   cursor; readline's overwrite mode replaces and blanks in place instead.
//!   While a shift's kind is unproven, every retirement asks the IN-PLACE
//!   WORLDS (the chain's keys folded with that kind edited in place, over the
//!   cells the chain knows) what they would show: a kind is proven only where
//!   every such world is refuted by the screen; a screen one of them shows
//!   with another buffer leaves the line unknown, and one it shows with fewer
//!   keys read credits nothing. Judged op by op, a burst whose intermediate
//!   state no frame showed proved what overwrite mode's echo shows too. And a
//!   Backspace cancels our own glyph only where no shift of an unproven kind
//!   is in flight: overwrite mode's glyph and rubout are no pair that cancels.
//! * **A line edge only at an authenticated prompt, and only as learned.** Home,
//!   End, Ctrl-A, Ctrl-E and Delete are placed only on a line started at the
//!   cell the shell's nonce-signed `B` mark names as its buffer's start, that
//!   mark still there: anywhere else (a remote shell, a line started after a
//!   Tab completion) the line's start is a lower bound, not the buffer's. And no
//!   binding is assumed: each key is a dark probe of its canonical effect until
//!   two credited echoes of it in the editor context teach it; a divergence or
//!   an expiry it heads unlearns it, and two ban it. End at the end of the text
//!   is declined: an autosuggestion is accepted there.
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
        /// (`proof_echo`), and so is the proof a carried epoch's guesses display
        /// on, while one is pending (`carried_proof`). While a state chain is in
        /// flight on such a line, the row past every state of it is held to the
        /// snapshot its base was verified with instead, a cell of it allowed to
        /// clear (`far_tail_moved`), or the chain flushes. Off: the tail is
        /// compared only as a guess retires, so an
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
        /// owe in flight and no later key for the same glyph pending or owed
        /// typed (`attributable`). Off: it is credited as any other, and once the
        /// owed echo (an erase we did not place) stepped the cursor back under
        /// the run, the echo of the guess after it proved the line a cell behind.
        StaleAnchor,
        /// A cursor advance is an echo only as far as the echoes of the run can
        /// take it (`echoed_past`): past the guess, and no further than the cell
        /// after the newest guess pending on the cursor's row. Off: any cursor
        /// past the guess, so a frame caught after a far end drew an
        /// autosuggestion beside an echo, its cursor not yet stepped back over
        /// it, retired the guess under the suggestion's first glyph, and the run
        /// typed next, anchored on that cursor a cell right of where it echoes,
        /// was credited with the next key's echo.
        CursorReach,
        /// The cursor standing on a guess in the last column is that guess's echo
        /// PARKED in the deferred-wrap slot only for a run at the end of the line
        /// (`echoed_past`, `tail_line_end`): a run typed mid-line pushes the
        /// line's own text right, and once its last glyph reaches the last column
        /// the cursor stands on it exactly as a park leaves it. Off: any run, and
        /// a guess there was credited with the shifted tail before its key was
        /// read.
        ParkedAtTheEnd,
        /// A state chain retires to the state the screen matches only when every
        /// op up to it is attributable (`retire_chain`): one that is not says the
        /// screen may show that state by another key's output, and with it every
        /// state before it. Off: front first, each asked as it is credited, so the
        /// motions before an unattributable glyph were credited, and the link
        /// measured on them, before the far end had read either.
        ChainAttributedWhole,
        /// An echo whose own cell reads opaque where its run's press saw a plain
        /// cell is no append (`appended`), and a proof standing on one whose cell
        /// turns opaque is voided (`reconcile_seen`): a zero-width mark the far
        /// end's buffer held after its cursor attached itself to the inserted
        /// glyph, which was therefore typed before text that no cell shows. Off:
        /// only the cells right of an echo are read, a run typed before such a
        /// mark proved as appends, and each later glyph, shown plain, settled
        /// with the mark on it.
        MarkedEcho,
        /// A state chain no state of which the screen matches waits (the
        /// transit wait, `in_transit`) only with the real cursor inside the text
        /// its echoes write, from `lo` to the furthest end any state reaches. Off:
        /// anywhere on the chain's row, so a password prompt printed on the next
        /// row and a reverse index back up left the chain waiting and displayed,
        /// and a glyph typed at that prompt joined it on glass.
        TransitReach,
        /// An edit op (a motion, an erase), whose state differs from the one
        /// before it only by the cursor and by cells left blank, is credited only
        /// while no echo owed to a key typed AFTER it is in flight
        /// (`attributable`): such a key's echo (an Enter's carriage return, a
        /// declined arrow's motion) can put the cursor on the op's state by
        /// itself. Off: a Backspace that erased a space, carried across an Enter
        /// on a far end that had stopped echoing, was credited when the Enter's
        /// carriage return alone put the cursor on its cell.
        EditOwedAfter,
        /// A credited SPACE never proves an epoch that is not proven yet
        /// (`retire_echoed`); on a proven line it carries the proof on. Its echo
        /// is a cursor advance over a blank, which any output that moves the
        /// cursor makes, a password prompt's own trailing space among them. Off:
        /// a space typed while `Password: ` was being drawn, on the cell of its
        /// trailing space, was credited with it, proved the line at the prompt's
        /// end, and the password typed there was painted.
        SpaceNeverProves,
        /// A glyph typed inside the known text (a mid-line insert, which shifts
        /// the text right of the cursor) displays in Adaptive only once a
        /// mid-line insert of the same epoch has echoed exactly (`op_visible`,
        /// THE CLASS LADDER): readline's overwrite mode replaces the glyph under
        /// the cursor instead. Off: it displays on the glyph proof alone.
        InsertShiftProof,
        /// A Backspace inside the known text (a mid-line erase, which shifts the
        /// text right of the cursor left) displays in Adaptive only once a
        /// mid-line erase of the same epoch has echoed exactly (`op_visible`):
        /// overwrite mode's rubout blanks the glyph before the cursor instead.
        /// Off: it displays on the glyph proof alone.
        EraseShiftProof,
        /// A shift tells its class apart only where no IN-PLACE WORLD shows
        /// the screen (`in_place_worlds`): the chain's ops folded with each
        /// unproven shift kind edited in place, as readline's overwrite mode
        /// replaces and rubs out, over the cells the chain knows (the rest
        /// match anything). A retirement whose screen some in-place world also
        /// shows, at another count of keys or with another buffer, leaves the
        /// line unknown: the proof goes and the chain is flushed
        /// (`retire_chain`); and a shift kind is proven only once every world
        /// that edits it in place is refuted. Off: any credited shift proves
        /// its class and its state becomes the line. A tail of spaces moved one
        /// cell is the same screen as one left in place, and so is a lone `c`
        /// moved onto a cell where a suggestion's `c` already stood: overwrite
        /// mode's echo matched the insert's state, the next insert was shown
        /// shifted while overwrite mode replaced, and a line one cell longer
        /// than the buffer took a `→` the editor beeped at (the two-hundred-seed
        /// sweep, seed index 21). Judged op by op against the fold's own
        /// states, which no frame showed, a rubout then a glyph typed in
        /// overwrite mode (`a c`, then `aoc`) proved both shift kinds, since
        /// each op alone differs from its edit in place while the two together
        /// leave the very screen of the two shifts.
        ShiftProofDistinguishes,
        /// A glyph typed inside the text that a chain in flight cannot place (a
        /// wide glyph, the margin, the cap) is owed and FLUSHES the chain
        /// (`arm_in_chain`): its echo moves the tail the chain's states paint
        /// over, and a frame caught between the pieces of that echo would show
        /// the state's pixels over a tail already moving. Off: it is declined as
        /// a glyph at the end of the line is, the pixels in flight kept; the far
        /// end reads it after every op in flight, so each op's frames settle
        /// first, and the safety theorem finds no counterexample (a fail-closed
        /// rule, its `rule_clause` `None`).
        MidLineDeclineFlushes,
        /// A glyph is placed only on a row the engine's UAX #9 reorder leaves in
        /// logical order: one that would make a row reorder (a right-to-left
        /// letter, an Arabic digit, an embedding: `aterm_bidi::has_bidi`) is
        /// declined, and so is every key whose press row already holds one
        /// (`reorders`, `row_reorders`). A right-to-left run is drawn reversed
        /// with the spaces and numbers between its letters, so a key's echo
        /// there may land on another column and move its neighbours. Off: such
        /// a glyph is placed as any narrow glyph, and after `א` proved its line
        /// (a lone right-to-left letter keeps the logical order) the space and
        /// two letters typed next were shown `א אא` while the row settled to
        /// `אא א` (the two-hundred-seed sweep, seed index 125, case 509).
        RightToLeftRows,
        /// A state chain holding a shift of an unproven kind retires at the
        /// smallest state the screen matches only when no in-place world
        /// (`in_place_worlds`) shows that screen with fewer keys read; one that
        /// does is flushed with nothing credited (`retire_chain`). The smallest
        /// match counts the keys an editor of the chain's own kind has read,
        /// and overwrite mode's screen after `k` keys can be the shifts' state
        /// after more: `c` echoed, `←`, `b`, `→`, Backspace typed, and the far
        /// end, having read `←b`, showing `b` (replaced) with the cursor after
        /// it, which is the chain's state after all four. Off: all four were
        /// credited, `→` and Backspace before either was read (a sample of
        /// nothing in the smoothed RTT), and the line rebased on a state the
        /// far end leaves once they land (found by review).
        InPlaceReadsFewer,
        /// Our own trailing glyph and a Backspace after it cancel
        /// (`predict_backspace`, `predict_edit`) only while no op in flight is a
        /// shift of a kind unproven for its epoch (`pair_cancels`): in
        /// overwrite mode a glyph typed inside the text REPLACES the glyph
        /// under the cursor and the rubout after it blanks or deletes it, which
        /// is no pair that cancels. The Backspace is then an op of the chain
        /// (an erase, dark while its kind is unproven), or, where it cannot be
        /// placed, an erase we did not model. Off: it popped the glyph, and the
        /// chain's next `←` showed the line with `b` still on it while overwrite
        /// mode settled to `a` (the review's shrink of seed index 152, case 65,
        /// with `VacateRefusal` off).
        CancelsOnlyKnownShifts,
        /// A glyph-lane run started inside the known line's text (`run_inside`:
        /// a motion of a chain left the line's cursor there, and the edit lane
        /// could not place the glyph as a shift) proves nothing (`appended`):
        /// it is a mid-line insert by knowledge, whatever the row shows. Off:
        /// it proved as the tail guard reads it, and a run typed on a SPACE of
        /// the line reads as typed over nothing: readline writes the glyph
        /// before the tail it shifts, the frame between showed the row right of
        /// it as the press saw it, and the glyph typed next was painted over
        /// the tail about to move (the eight-hundred-seed sweep, seed index
        /// 361).
        InsideRunsNeverProve,
        /// A standing proof stands only while its echo's own cell still holds
        /// the glyph it echoed (THE STANDING GLYPH, `holds_its_glyph`): read at
        /// every reconcile before anything retires, at a press that continues
        /// the proof on the live cursor, and for a carried proof while a
        /// carried guess is pending; not while a chain is in flight, whose
        /// states own the line's cells. A cell that no longer holds it says the
        /// line was erased or redrawn by output nobody typed for. Off: `i` and
        /// a space typed, `i` echoed and proving, then the space's echo, a
        /// password prompt printed on the same row, and a screen clear that
        /// redrew `$ ` landed in one burst, the cursor brought back onto the
        /// space's cell; the redraw's trailing space then credited it, the proof
        /// carried on, and `8`, typed at the password read, was painted (the
        /// two-thousand-seed sweep, seed index 1121, case 571).
        StandingGlyph,
        /// A Backspace that cancels our own trailing glyph owes the erase's echo
        /// too, typed as a SPACE (`predict_backspace`): part-way, as `\b \b`,
        /// it is a blank on the cancelled cell with the cursor past it, a
        /// space's echo exactly. Off: `n` typed and cancelled, a space typed on
        /// the cancelled cell, and the frame caught after the erase's blank
        /// and before its step back credited the space before the far end read
        /// it (the two-thousand-seed sweep, seed index 1200, case 44).
        CancelOwesItsBlank,
        /// A state chain retires at a state the screen matches only where the
        /// echo of no op before it, caught PART-WAY, shows that screen too
        /// (`part_way_before`): zle's erase writes a blank on the cell it
        /// vacates before it steps back, and readline's insert inside the text
        /// writes its tail before it steps back, and a frame between can be a
        /// later state of the chain. Off: `n` echoed, a Backspace and a space
        /// typed after it, and zle's erase caught after its blank, the cursor
        /// past it: the chain's state after both keys, so the space was
        /// credited before the far end read it, and the space typed next was
        /// credited with that one's echo (the two-thousand-seed sweep, seed
        /// index 1200, case 44, once the cancelled pair's blank was owed). A
        /// line edge's echo moves the cursor over the text a cell at a time
        /// (End reprints the line from the cursor), so caught part-way its
        /// cursor is anywhere between where it started and where it lands: a
        /// later `←`'s state matched there was credited unread, and the End
        /// typed after it with the first one's output (the three-hundred-seed
        /// sweep of increment 4, seed index 266, case 427).
        PartWayEchoes,
        /// A line-edge key (Home, End, Ctrl-A, Ctrl-E) or Delete is placed only
        /// on a WITNESSED line (`witnessed`): one the predictor started at the
        /// very cell an authenticated OSC 133 `B` names as the buffer's start
        /// (`EditLine::fresh`), that `B` still where the line starts. Then the
        /// line's `lo` is the buffer's start and its text the whole buffer, so
        /// Home's state, the cursor on `lo`, is the far end's. Off: any
        /// authenticated `B` on the line's row will do, so a line the
        /// predictor started after a Tab completion, mid-buffer, took Home to
        /// its own start while the far end went to the buffer's.
        WitnessFreshPrompt,
        /// THE FIRST KEY (`witness_of`): a run started at the `B`'s cell is
        /// witnessed only when its press is the first key typed at that
        /// prompt (`prompt_gate`, open since a line terminator pressed at
        /// another prompt). The `B` vouches for the buffer it found empty;
        /// any later key may have left text the screen draws as blanks (a
        /// literal tab, spaces) right of a cursor brought back to the start
        /// by a key this crate does not model, or the shell may have redrawn
        /// the prompt narrower and left the `B` behind (zsh's `reset-prompt`
        /// sends none). Off: any run started at the `B`'s cell, and after
        /// two spaces typed, Home at the start not modelled (the line gone)
        /// and `c` typed there, End was shown after `c` while the far end
        /// went past the spaces (the review of increment 4).
        WitnessFirstKey,
        /// A line-edge key or Delete is LEARNED before it is shown
        /// (`EditorBindings`): an unlearned one is armed as a dark PROBE of its
        /// canonical effect, and only two credited echoes of that effect, in
        /// the same editor context, make it shown; a divergence it heads
        /// unlearns it, and two ban it. No binding is assumed: zsh without its
        /// integration leaves Home unbound and inserts `~` for Delete, and zsh's
        /// vi insert mode inserts Ctrl-A. Off: every such key is shown on the
        /// glyph proof as its canonical effect, and an unbound Home was shown
        /// moving the cursor the far end left where it was.
        LearnedBindings,
        /// End or Ctrl-E with the cursor already at the end of the text the
        /// line knows is DECLINED WITH ITS PIXELS KEPT (`predict_line_edge`):
        /// there zsh-autosuggestions and fish ACCEPT the suggestion drawn after
        /// the buffer, which no state of the chain names. Off: it is armed as
        /// a line end that moves nothing, and the glyph typed after it was
        /// shown at the end of the line while the far end put it after the
        /// suggestion it accepted.
        EndRefusal,
        /// A glyph-lane guess whose own cell reads OPAQUE AND BLANK before its
        /// echo is flushed (`reconcile_seen`): the far end drew there what a
        /// plain ghost cannot show, a zero-width mark its buffer holds after
        /// the cursor (which the guess's echo then takes on), or the right half
        /// of a wide glyph. Off: the guess waited there as over a blank, and a
        /// line editor whose forced wrap carried the mark its buffer held after
        /// the cursor onto the next row's first cell, under the next guess, had
        /// that guess painted plain while it settled marked (the
        /// three-hundred-seed sweep of increment 4, seed index 139, a break on
        /// main before it).
        OpaqueHead,
        /// DARK BEHIND A WAIT: a glyph-lane guess waiting on a cell that shows
        /// its glyph with the real cursor ON it, a glyph the run's press did not
        /// see there (`reconcile_cells`, `held_wait`), shows nothing typed after
        /// it until it retires (`visible_count`, Adaptive). A suggestion drawn
        /// after the press, or a tail a mid-line echo shifted under it, puts the
        /// glyph there while the far end goes on editing; so does the guess's
        /// own echo with the cursor brought back over it by output nobody typed
        /// for, after which the far end may echo nothing more. Off: `sad`
        /// typed, ` Password: ` printed after it on the same row and a reverse
        /// index back onto `d` left `d` waiting there, and the secret typed next
        /// was shown after it (the review of increment 4's sweep, seed index
        /// 1909, case 109, a break on main before it). The flush that would also
        /// close it costs every run typed ahead along an autosuggestion its
        /// display; holding only what follows costs those keys one echo.
        DarkBehindAWait,
        /// THE FLOOR: a standing proof is voided when the row under the
        /// proof's line, which the run's first press read, shows text where it
        /// read a blank (`floor_moved`). Text drawn under the line is output of
        /// something other than the line editor's appends, and the cursor it
        /// leaves there can be brought back to the very cell the proof names.
        /// Off: ` sie` echoed, `\r\nPassword: ` printed on the next row and a
        /// reverse index back up onto the cell after `e`, where the proof
        /// stood, and the secret typed there was shown (the review of
        /// increment 4's sweep, seed index 1206, case 434, a break on main
        /// before it).
        FloorBelow,
        /// THE CREDIT TAKEN BACK: a turn that finds the real cursor back on the
        /// newest glyph-lane credit's cell, or before it on its row, with nothing
        /// of ours moving it there (`last_credit`), owes that key's echo again
        /// and voids the proof: an echo never steps back over itself. Off: an
        /// autosuggestion drawn part-way, its first glyph the typed one with the
        /// cursor past it, stood for the key's echo, and the guess typed next on
        /// the cursor stepped back was credited with that key's real echo (the
        /// review of increment 4's sweep, seed indices 784 and 2115, breaks on
        /// main before it).
        CreditTakenBack,
        /// A press places its glyph by the engine's own deferred-wrap flag
        /// (`KeySample::wrap_pending`, `parked`) where the host hands it: the
        /// cursor on a filled last column is PARKED there only after the
        /// terminal wrote that column; a line editor moving its cursor onto its
        /// own text there (overwrite mode, `→` over the buffer) leaves it
        /// unparked, and the next glyph replaces that cell. Off: the last
        /// reconcile's reading, a filled last column under the cursor, which put
        /// `i` on the next row while overwrite mode wrote it over the `d` the
        /// cursor stood on, and every guess after it a cell right of its echo
        /// (the review of increment 4's sweep, seed index 1841, case 453).
        EngineWrapFlag,
        /// A run anchored on the cell of a guess a Backspace cancelled
        /// (`cancelled_at`) carries a STALE ANCHOR when that guess did
        /// (`cancelled_stale`, `arm_glyph`), and no shift is anchored there
        /// (`arm_inside`): the cancelled guess was placed from a cursor an owed
        /// echo could move, so its cell is no better. Off: the cancelled cell
        /// was trusted as a fresh anchor. Over ssh (no integration), `d`
        /// echoed, `←` and Delete pressed (Delete owed, not modelled there) and
        /// `o` guessed on the live cursor; the far end's `\b`, ` \b`, `o` put
        /// `o` a cell left of its guess, which stayed pending; a Backspace
        /// cancelled it, `aac` was typed on its cell, and the second `a`'s
        /// echo credited the first guess, so `ac` stood a cell right of where
        /// they echoed (the review of increment 5's sweep, seed index 3942,
        /// case 141, a break on main before it).
        StaleCancel,
        /// The echo of an erase or a Delete (zle's and readline's redraw: the
        /// tail rewritten, a blank where the text ended, then the step back)
        /// and of an insert inside the text (readline's: the glyph and the
        /// tail, then the step back) is PART-WAY with the cursor anywhere on
        /// its step back (`part_way_before`): strictly right of the cursor its
        /// state leaves, up to the end of the text it rewrote. Off: only with
        /// the cursor at that end. Split after its first `\b`, a Delete's
        /// `d`, ` `, `\b\b` left the cursor on End's state, and Delete and End
        /// were credited (End unread); the rest of the step back put the
        /// cursor on `lo`, Home's state, and Home, pressed meanwhile, was
        /// credited with Delete's output (ATTRIBUTION, the review of
        /// increment 5's sweep, seed index 2748, case 172, a break on main
        /// before it).
        PartWayStepBack,
        /// A state chain no state of which the screen matches waits in
        /// transit (`in_transit`) only where every BLANK cell some state
        /// compares is one a state holds blank (a space of its text, a cell
        /// it vacated) or one an echo of the chain's own ops writes: at or
        /// right of where an insert, an erase or a Delete starts writing. No
        /// echo of the chain's keys blanks text left of that, and no motion's
        /// blanks any. Off: every blank cell was explained, so zsh's
        /// `reset-prompt` redrawing the prompt narrower (`\r$ od\e[K`, the
        /// line's text moved left of `lo`, its old cells blank and the cursor
        /// left inside them) kept a `←` in flight waiting and painted `od` a
        /// second time over the blanks (FRAME FIDELITY, the review of
        /// increment 5's sweep, seed index 2482, case 215, a break on main
        /// before it).
        TransitBlanks,
        /// A coordinate move (`observe_coordinates`: rows moved under the
        /// screen) keeps THE FIRST KEY's gate as it stood, where a host reset
        /// shuts it: a scroll lets no key past the predictor. Off: it shuts it
        /// as the reset it runs does, and a shell on the bottom row, whose
        /// Enter's own output scrolls the screen before the next prompt's
        /// first key, never had a line witnessed, so Home, End, Ctrl-A, Ctrl-E
        /// and Delete were never placed there (`tests/editing_win.rs`, which
        /// counts the lines witnessed with it on and off). It keeps no safety
        /// clause: off, less is shown.
        ScrollKeepsFirstKey,
        /// THE SCREEN AS IT WAS: a head guess whose cell shows another glyph,
        /// the very one its run's press read there, with the real cursor still
        /// on it, is not echoed yet and waits (a glyph typed onto an
        /// autosuggestion it diverges from, the review of phase 0). Off: any
        /// other glyph on the head's cell flushes, as before that review, so
        /// a glyph typed over text (`sudo ` after Home or Ctrl-A, a typo
        /// retyped inside the line) goes at the first frame after its press.
        /// It keeps no safety clause: off, less is shown. It was the one
        /// change of what the glyph lane shows since the plan that no rule
        /// switched off: with it, every rule off shows on the measurement's
        /// corpus what the predictor before the plan showed. A cross-check
        /// driving the predictor at 3d4c211fbb through the same world printed
        /// every metric of every `Baseline` run of `tests/editing_win.rs`
        /// exactly, the churn (which it did not count) and the cost (the press
        /// still reads the row) aside (`docs/DESIGN-predictive-editing-2026-10-20.md`
        /// §2).
        ScreenAsItWas,
    }
}

impl Rule {
    /// This rule's bit in the predictor's disabled set. Sixty-four rules fit,
    /// and the assertion below refuses to build a sixty-fifth.
    #[cfg(any(test, debug_assertions))]
    const fn bit(self) -> u64 {
        1 << self as u32
    }
}

// A rule past the sixty-fourth would shift its bit out of the disabled set, and
// its negative control would switch off nothing.
#[cfg(any(test, debug_assertions))]
const _: () = assert!(Rule::ALL.len() <= 64);

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
///
/// It also carries what the session's line editor was LEARNED to do with Home,
/// End, Ctrl-A, Ctrl-E and Delete (`EditorBindings`): learned from its own
/// echoes, in its own editor context, and as much the session's as its link.
/// Carried here, the swap that keeps the one keeps the other, and a session
/// nothing is known about starts with neither.
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
    bindings: EditorBindings,
}

/// Which line editor a line-edge key reaches, as far as its bindings go: the
/// session, the shell integration's capability nonce in use (a new shell, or a
/// re-key, is a new one), DECCKM (Home is `ESC O H` under it, `ESC [ H` without,
/// and an editor binds the two apart) and whether the keyboard mode sends a
/// control chord as an escape sequence (Ctrl-A is `CSI 97;5u` under a Kitty
/// mode that reports functional keys, `CSI 27;5;97~` under modifyOtherKeys
/// level 2). Learned bindings (`EditorBindings`) hold for one context; a key
/// pressed in another starts them afresh.
///
/// Compared, never read: the nonce is kept as a digest, so the context carries
/// no capability and its `Debug` prints none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorContext {
    session: u64,
    nonce: u64,
    cursor_keys: bool,
    escaped_chords: bool,
}

impl EditorContext {
    /// The context of a press: the `session` the keys go to, the shell
    /// integration nonce in use (`None` with no integration, where no
    /// line-edge key is placed anyway), DECCKM (`cursor_keys_application`)
    /// and whether a control chord goes out as an escape sequence
    /// (`escaped_chords`, [`KeySample::escaped_chords`]).
    #[must_use]
    pub fn new(
        session: u64,
        nonce: Option<&[u8; 32]>,
        cursor_keys_application: bool,
        escaped_chords: bool,
    ) -> Self {
        // FNV-1a over the nonce: equality is all a context is asked, and two
        // nonces folding alike is a 2^-64 chance of keeping bindings a new
        // shell must relearn, never of showing anything unlearned.
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

/// A key whose binding the predictor LEARNS rather than assumes (an
/// [`EditKey`] past the arrows and Backspace): what each does is the line
/// editor's own business, measured to differ (zsh without its integration
/// leaves Home and End unbound and inserts `~` for Delete; zsh's vi insert
/// mode inserts Ctrl-A and Ctrl-E as glyphs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Learned {
    Home,
    End,
    CtrlA,
    CtrlE,
    Delete,
}

impl Learned {
    /// Every learned key, in the order of `EditorBindings::keys`.
    const ALL: [Self; 5] = [
        Self::Home,
        Self::End,
        Self::CtrlA,
        Self::CtrlE,
        Self::Delete,
    ];

    /// The learned key `key` is, if it is one.
    fn of(key: EditKey) -> Option<Self> {
        match key {
            EditKey::Home => Some(Self::Home),
            EditKey::End => Some(Self::End),
            EditKey::CtrlA => Some(Self::CtrlA),
            EditKey::CtrlE => Some(Self::CtrlE),
            EditKey::Delete => Some(Self::Delete),
            EditKey::Left | EditKey::Right | EditKey::Backspace => None,
        }
    }

    /// The edit key this is.
    fn key(self) -> EditKey {
        match self {
            Self::Home => EditKey::Home,
            Self::End => EditKey::End,
            Self::CtrlA => EditKey::CtrlA,
            Self::CtrlE => EditKey::CtrlE,
            Self::Delete => EditKey::Delete,
        }
    }

    /// Its slot in `EditorBindings::keys`.
    fn index(self) -> usize {
        Self::ALL.iter().position(|&k| k == self).unwrap_or(0)
    }

    /// The op its canonical binding is: beginning-of-line, end-of-line or
    /// delete-char.
    fn op(self) -> Op {
        match self {
            Self::Home | Self::CtrlA => Op::LineStart(self),
            Self::End | Self::CtrlE => Op::LineEnd(self),
            Self::Delete => Op::Delete,
        }
    }

    /// A control chord, which a keyboard mode that escapes chords sends as
    /// another key: `CSI 97;5u` under a Kitty mode that reports functional
    /// keys, `CSI 27;5;97~` under modifyOtherKeys level 2 (R-KITTY).
    fn chord(self) -> bool {
        matches!(self, Self::CtrlA | Self::CtrlE)
    }
}

/// What one learned key's echoes have shown in an editor context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Binding {
    /// Credited echoes of its canonical effect since it was last contradicted,
    /// up to [`BINDING_LEARNED`].
    seen: u8,
    /// Divergences it headed, up to [`BINDING_BANNED`].
    contradictions: u8,
}

/// Echoes of a key's canonical effect that make its binding learned.
const BINDING_LEARNED: u8 = 2;
/// Contradictions that ban a key for its editor context.
const BINDING_BANNED: u8 = 2;

/// THE LEARNED BINDINGS of one editor context ([`EditorContext`]), carried with
/// the session in its [`LinkEstimate`]. A key is LEARNED once two credited
/// echoes of its canonical effect, each from a state that effect changes,
/// agreed with no divergence it headed between; it is BANNED once two
/// divergences it headed contradicted it, and is then input the predictor does
/// not model for the rest of the context. No binding is assumed: an echo is the
/// only proof, as for every other key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EditorBindings {
    /// The context these hold for; `None` before any line-edge key.
    context: Option<EditorContext>,
    keys: [Binding; 5],
}

impl EditorBindings {
    fn get(&self, key: Learned) -> Binding {
        self.keys.get(key.index()).copied().unwrap_or_default()
    }

    fn learned(&self, key: Learned) -> bool {
        self.get(key).seen >= BINDING_LEARNED
    }

    fn banned(&self, key: Learned) -> bool {
        self.get(key).contradictions >= BINDING_BANNED
    }
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
    /// What this cell of a state chain's overlay shows ([`GhostCell`]): a typed
    /// glyph, text the chain's shift moved here, or a cell an erase vacated.
    /// Every guess of the glyph lane, and every pending op, is `Typed`.
    ghost: GhostCell,
    /// A PROBE: a line-edge key or Delete armed before its binding is learned
    /// in this editor context (`EditorBindings`), as a question to the far end
    /// whose answer, its echo, teaches the binding. Never displayed, and nor is
    /// anything typed after it, in any mode: it is a binding nobody has seen.
    probe: bool,
}

/// What a cell of a state chain's overlay shows ([`Prediction::ghost`]), so a
/// host can paint it as what it is.
///
/// A SHIFT (a glyph typed, or a Backspace, inside the text) moves the text
/// right of the cursor by one cell. That text is certain; only its place is
/// speculative: it is the very glyph the screen shows at another cell, and a
/// host may paint it as THAT cell, in its own colours and attributes, rather
/// than dimmed like a typed glyph whose echo has not come (aterm-core's
/// `RenderInput::paint_prediction_span`). Every cell's glyph is
/// [`Prediction::ch`] whatever its kind, so a host that paints every cell as a
/// typed ghost (the web host) still shows the state's text exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhostCell {
    /// A glyph a key typed, whose echo has not come: painted dimmed.
    Typed,
    /// The glyph the screen shows at logical column `from` of the same row,
    /// moved here by a shift: the predictor names `from` only where its last
    /// read of the row saw this very glyph there.
    Moved {
        /// The column the moved glyph stands on now.
        from: u16,
    },
    /// A cell an erase vacated, past the state's text: the terminal's blank.
    Vacated,
}

/// What one pending key does to the line it is typed on (`Prediction::op`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// A glyph typed at the cursor: at the end of the text, every guess of the
    /// glyph lane and a glyph a state chain appends; inside it, a glyph a chain
    /// inserts, shifting the text right of the cursor one cell right.
    Insert,
    /// `←`: the cursor one cell left, no cell changed.
    Left,
    /// `→`: the cursor one cell right, no cell changed.
    Right,
    /// `⌫`: the glyph before the cursor erased, the cursor on its cell; inside
    /// the text, the text right of the cursor shifted one cell left.
    Erase,
    /// Home or Ctrl-A as beginning-of-line: the cursor on the line's `lo`, no
    /// cell changed. Placed only on a witnessed line, whose `lo` is the
    /// buffer's start.
    LineStart(Learned),
    /// End or Ctrl-E as end-of-line: the cursor at the end of the text, no cell
    /// changed. Placed only on a witnessed line, whose text is the whole
    /// buffer.
    LineEnd(Learned),
    /// Delete as delete-char: the glyph under the cursor erased, the text right
    /// of it one cell left, the cursor where it was.
    Delete,
}

impl Op {
    /// The learned key this op was armed for, if it is one's.
    fn learned(self) -> Option<Learned> {
        match self {
            Self::LineStart(k) | Self::LineEnd(k) => Some(k),
            Self::Delete => Some(Learned::Delete),
            Self::Insert | Self::Left | Self::Right | Self::Erase => None,
        }
    }

    /// An op that erases a glyph and vacates the cell the text's end leaves
    /// (a Backspace, Delete): the cells past the end must read blank
    /// ([`Rule::VacateRefusal`]).
    fn erases(self) -> bool {
        matches!(self, Self::Erase | Self::Delete)
    }
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
    /// The glyph proof and a mid-line insert of the same epoch echoed exactly,
    /// moving a glyph: overwrite mode replaces the glyph under the cursor.
    InsertShift,
    /// The glyph proof and a mid-line erase of the same epoch echoed exactly,
    /// moving a glyph: overwrite mode's rubout blanks the glyph before the
    /// cursor and moves nothing.
    EraseShift,
    /// The glyph proof and the key's binding learned in this editor context
    /// (`Prediction::probe` clear): Home, End, Ctrl-A, Ctrl-E as the line's
    /// edges, on a witnessed line.
    LineEdge,
    /// As [`Self::LineEdge`], for Delete as delete-char.
    DeleteFwd,
}

impl OpClass {
    /// This class's bit in `Predictor::proven` (none for the glyph class, which
    /// the glyph proof alone admits, nor for the learned classes, which their
    /// bindings admit).
    const fn proven_bit(self) -> u8 {
        match self {
            Self::Glyph | Self::LineEdge | Self::DeleteFwd => 0,
            Self::Motion => PROVEN_MOTION,
            Self::InsertShift => PROVEN_INSERT_SHIFT,
            Self::EraseShift => PROVEN_ERASE_SHIFT,
        }
    }
}

/// `Predictor::proven`'s bit for the motion class.
const PROVEN_MOTION: u8 = 1;
/// `Predictor::proven`'s bit for the mid-line insert class.
const PROVEN_INSERT_SHIFT: u8 = 2;
/// `Predictor::proven`'s bit for the mid-line erase class.
const PROVEN_ERASE_SHIFT: u8 = 4;

/// The `ch` of an op that writes no glyph of its own (a motion, an erase): NUL,
/// which no typed owe names and no host paints (an engaged chain's overlay is its
/// ghost cells, never its ops).
const EDIT_OP_CH: char = '\0';

/// The source column of a glyph a key typed, in `Predictor::fold_cells`: no
/// column of the line's verified text (`compose_chain`).
const NO_SOURCE: u16 = u16::MAX;

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
            ghost: GhostCell::Typed,
            probe: false,
        }
    }

    /// [`Self::test_at`] for a state chain's cell of kind `ghost`: a host's
    /// painter tests build the moved and vacated cells of a shift.
    #[must_use]
    pub fn test_ghost(row: u16, col: u16, ch: char, ghost: GhostCell) -> Self {
        Self {
            ghost,
            ..Self::test_at(row, col, ch)
        }
    }

    /// What this cell of a state chain's overlay shows ([`GhostCell`]):
    /// `Typed` for every guess of the glyph lane.
    #[must_use]
    pub fn ghost(&self) -> GhostCell {
        self.ghost
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
            Op::LineStart(k) | Op::LineEnd(k) => Some(k.key()),
            Op::Delete => Some(EditKey::Delete),
        }
    }

    /// Whether this pending op is a PROBE (a line-edge key or Delete whose
    /// binding is not learned yet: never displayed). A read-only test seam.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn probe_for_test(&self) -> bool {
        self.probe
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

/// Where the shell in front says its line editor's buffer starts
/// ([`KeySample::input_start`]): the visible cell of its authenticated OSC 133
/// `B`, and which prompt that is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputStart {
    /// The `B`'s visible `(row, col)`.
    pub cell: (u16, u16),
    /// The prompt it belongs to (aterm-core's block id): every prompt has its
    /// own, so a `B` the engine keeps while the shell redraws its prompt is
    /// told from the next prompt's at the same cell, and the prompt a line
    /// terminator was pressed at ([`Predictor::note_line_submit_at`]) from the
    /// one that follows it.
    pub prompt: u64,
}

/// THE FIRST KEY's gate (`Predictor::prompt_gate`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PromptGate {
    /// A key was handed over since the last line terminator, or the predictor
    /// does not know what was typed (a fresh or reset predictor, a terminator
    /// pressed where no authenticated prompt was in front).
    #[default]
    Shut,
    /// No key since a line terminator pressed at the prompt `after`: the next
    /// key is the first typed at whatever prompt follows, if any does. One
    /// shown the same prompt's `B` meets a buffer the terminator did not
    /// clear (an Enter the line editor kept for itself).
    Open { after: u64 },
}

/// What the host read under the terminal hold of one keypress, for
/// [`Predictor::predict_glyph`] and [`Predictor::predict_edit`].
#[derive(Clone, Copy, Debug, Default)]
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
    /// The row UNDER the cursor row, one [`Seen`] per column, read under the same
    /// hold, wherever `row` is read and the cursor row is not the last: THE FLOOR
    /// a run's proof stands on (`Rule::FloorBelow`). Text drawn there where the
    /// press read a blank is no append of the line's, and the cursor it leaves
    /// can be brought back up onto the proof's cell. `None` from a host that
    /// reads no such row, or on the last row.
    pub below: Option<&'r [Seen]>,
    /// The engine's own deferred-wrap flag at the press (aterm-core's
    /// `Grid::pending_wrap`): the cursor is PARKED after filling the last
    /// column, and the next glyph lands at column 0 of the next row. `None` from a
    /// host that cannot read it, which leaves the predictor's own reading of the
    /// last reconcile (`deferred_wrap`: a filled last column under the cursor).
    /// That reading is no flag: a line editor that moves the cursor onto text
    /// in the last column (overwrite mode, `→` over its buffer) leaves it on a
    /// filled cell it has not wrapped from ([`Rule::EngineWrapFlag`]).
    pub wrap_pending: Option<bool>,
    /// Where the line editor's buffer starts, as the shell's AUTHENTICATED
    /// OSC 133 `B` mark of the prompt in front names it, and which prompt that
    /// is (aterm-core's `Terminal::input_region_start`: the capability-nonce
    /// gate on and its nonce in use, a `B` that says it stands where the
    /// buffer starts, between that `B` and its `C`, on the main screen at the
    /// live bottom), or `None`. The one far-end word on the buffer's start a
    /// terminal gets: Home, End, Ctrl-A, Ctrl-E and Delete are placed only on a
    /// line the predictor started at that very cell with the FIRST key typed
    /// at that prompt (`EditLine::fresh`, THE FIRST KEY). A host with no signed
    /// marks (a remote shell, the web) hands `None`, and those keys stay input
    /// the predictor does not model.
    pub input_start: Option<InputStart>,
    /// The editor context the keys reach ([`EditorContext`]): learned bindings
    /// hold for one.
    pub context: EditorContext,
    /// The keyboard mode in force sends a control chord as an escape sequence
    /// rather than its C0 byte (aterm-core's `control_chords_escaped`): a Kitty
    /// mode that reports functional keys (`DISAMBIGUATE_ESC_CODES` or
    /// `REPORT_ALL_KEYS_AS_ESC`: Ctrl-A is `CSI 97;5u`) or xterm's
    /// modifyOtherKeys at level 2 (`CSI 27;5;97~`). Ctrl-A and Ctrl-E then go
    /// out as another key, which the predictor does not model (R-KITTY).
    pub escaped_chords: bool,
}

/// A line-editing key the host hands [`Predictor::predict_edit`]: bare, with no
/// modifier, sent as the bytes below. The keypad's arrows are these keys too
/// under a legacy encoder, which sends them alike; a Kitty mode that reports
/// the keypad or the lock keys sends another key (`CSI 57417 u`,
/// `ESC [ 1 ; 65 D`), and a modified arrow is a word motion or something else
/// again: the host hands none of those.
///
/// Home, End, Ctrl-A, Ctrl-E and Delete are LEARNED keys: what each does is the
/// line editor's own binding, learned from its echoes in each editor context
/// before it is ever shown, and each is placed only on a line started at the
/// shell's authenticated buffer start ([`KeySample::input_start`]). The main
/// block's keys only: the keypad's Home or End under DECKPAM is another key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKey {
    /// `←` (`ESC [ D`, or `ESC O D` under DECCKM).
    Left,
    /// `→` (`ESC [ C`, or `ESC O C` under DECCKM).
    Right,
    /// Backspace (`DEL` or `BS`, as the host already classifies it).
    Backspace,
    /// Home (`ESC [ H`, or `ESC O H` under DECCKM), learned as
    /// beginning-of-line.
    Home,
    /// End (`ESC [ F`, or `ESC O F` under DECCKM), learned as end-of-line.
    End,
    /// Ctrl-A (`SOH`), learned as beginning-of-line.
    CtrlA,
    /// Ctrl-E (`ENQ`), learned as end-of-line.
    CtrlE,
    /// Delete (`ESC [ 3 ~`), learned as delete-char.
    Delete,
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
    /// Every glyph that built the line proved it at the END OF THE ROW
    /// (`tail_line_end` of its run): its press saw nothing from its cell to the
    /// margin, so nothing right of the line can be the buffer's own. Otherwise
    /// the line proved beside text its echoes left in place (a right prompt),
    /// and only that text staying in place says the buffer ends at `hi`: a chain
    /// in flight holds the row past every state of it to the snapshot its base
    /// was verified with (`far_tail_moved`), as the standing tail holds the
    /// glyph lane's.
    end_known: bool,
    /// THE WITNESS (R-WITNESS): the prompt whose authenticated OSC 133 `B` the
    /// line was started at, `None` for any other line. Started by the FIRST
    /// KEY typed at that prompt (`Predictor::prompt_gate`), at the very cell
    /// the `B` named as its buffer's start ([`KeySample::input_start`]), whose
    /// echo proved as an append over a blank or kept tail. The `B` said the
    /// buffer starts there and was empty when the mark was sent, and nothing
    /// was typed since: so `lo` is the buffer's start, not a lower bound, and,
    /// every key since having been one the predictor placed and verified (any
    /// other ends the epoch and drops the line), the text is the whole buffer:
    /// Home's state and End's are the far end's. Kept by every append and every
    /// state a chain retires to.
    fresh: Option<u64>,
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

/// What the IN-PLACE WORLDS make of a state chain's retirement at `S_j`
/// (`Predictor::in_place_worlds`).
#[derive(Clone, Copy, Debug, Default)]
struct Worlds {
    /// Some in-place world shows the screen with fewer keys read than `j`
    /// ([`Rule::InPlaceReadsFewer`]).
    fewer: bool,
    /// Some in-place world shows the screen other than as `S_j` does: at
    /// another count of keys, with another buffer, or past an edge whose
    /// outcome the predictor does not know ([`Rule::ShiftProofDistinguishes`]).
    other: bool,
    /// The shift kinds ([`PROVEN_INSERT_SHIFT`], [`PROVEN_ERASE_SHIFT`]) the
    /// retirement proves: every world that edits that kind in place shows
    /// another screen at every count of keys.
    proves: u8,
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

/// Whether `ch` makes a row the engine's UAX #9 reorder draws in another order
/// (a right-to-left letter, an Arabic digit, an embedding or isolate), by the
/// engine's own classification ([`Rule::RightToLeftRows`]).
fn reorders(ch: char) -> bool {
    aterm_bidi::has_bidi(&[ch])
}

/// Whether a row read as `cells` holds a character that makes it reorder, by
/// each cell's plain answer (an opaque cell's included).
fn row_reorders(cells: &[Seen]) -> bool {
    cells.iter().any(|s| s.legacy().is_some_and(reorders))
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
    /// Shifts armed: glyphs and erases inside the known text.
    pub shifts_armed: u64,
    /// Credited shifts that proved their class ([`Rule::InsertShiftProof`],
    /// [`Rule::EraseShiftProof`]), the text they moved holding a glyph.
    pub shifts_proven: u64,
    /// Edit keys and shifts refused, and chains flushed, for an opaque cell on
    /// the line (R-OPAQUE: a wide glyph's half, marks, a reordered row), and
    /// glyph-lane guesses flushed for an opaque blank under the head
    /// ([`Rule::OpaqueHead`]).
    pub opaque_refusals: u64,
    /// Glyphs typed inside the text a chain could not place, which flushed it
    /// ([`Rule::MidLineDeclineFlushes`]).
    pub mid_line_flushes: u64,
    /// Credited shifts whose screen an edit in place would also show, with
    /// their class unproven: the line left unknown ([`Rule::ShiftProofDistinguishes`]).
    pub ambiguous_shifts: u64,
    /// Keys declined for a row a right-to-left character reorders, typed or
    /// already on it ([`Rule::RightToLeftRows`]).
    pub bidi_refusals: u64,
    /// Known lines started at the shell's authenticated buffer start
    /// (`EditLine::fresh`): where a line-edge key may be placed.
    pub witnessed_lines: u64,
    /// Line-edge keys and Deletes armed as ops of a chain, probes included.
    pub line_edge_armed: u64,
    /// Of those, PROBES: armed before their binding was learned.
    pub probes_armed: u64,
    /// Bindings that became learned: a key's second credited echo of its
    /// canonical effect ([`Rule::LearnedBindings`]).
    pub bindings_learned: u64,
    /// Contradictions: a divergence or an expiry a line-edge key or Delete
    /// headed, which unlearns its binding.
    pub bindings_unlearned: u64,
    /// Ctrl-A and Ctrl-E refused under a keyboard mode that sends them as an
    /// escape sequence (R-KITTY: Kitty's `CSI u`, modifyOtherKeys level 2).
    pub kitty_refusals: u64,
    /// Turns a guess waited on with the cursor on a glyph its press did not see,
    /// and what was typed after it held dark ([`Rule::DarkBehindAWait`]).
    pub held_waits: u64,
    /// Credits taken back for the cursor found on or before them
    /// ([`Rule::CreditTakenBack`]).
    pub credits_taken_back: u64,
    /// Proofs voided for text drawn under their line where the run's press saw
    /// a blank ([`Rule::FloorBelow`]).
    pub floor_voids: u64,
    /// THE COST AND THE CHURN, which the measurement of what predictive editing
    /// buys (`tests/editing_win.rs`) counts rather than times: the four below
    /// decide nothing.
    ///
    /// Ops applied by THE FOLD (`fold_cells`): every state of every chain the
    /// predictor computed, to match it, to compare an in-place world, to wait
    /// in transit or to paint it, and every op a press walked to find where its
    /// key applies (`placeable`, by lengths and the cursor alone). A frame's
    /// share is what it costs over the glyph lane's, and a press's what the
    /// edit lane adds to it.
    pub ops_folded: u64,
    /// Flushes that dropped something pending (`flush`): a guess or an op the
    /// predictor stopped tracking, owed.
    pub flushes: u64,
    /// Confirmation epochs ended with their pixels kept (`end_epoch`): a line
    /// terminator, a printable glyph declined, an arrow declined
    /// (`decline_motion`). A key the predictor does not model ends its epoch
    /// by a flush (`flushes`, when anything was pending).
    pub epoch_ends: u64,
    /// Expiries (`expire`): each glyph-lane guess that outlived the window,
    /// owed or not, and each state chain expired whole.
    pub expiries: u64,
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
    FloorVoid,
    CreditTakenBack,
    Flush,
    EpochEnd,
    Expiry,
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
    /// The cell of the standing proof's echo (`proof_echo`) read opaque when it
    /// proved, which an echo may only where its press saw that cell opaque too
    /// (a right-to-left row's blanks, `echo_unmarked`). THE STANDING MARK: a
    /// proof whose echo cell reads opaque now and did not then is voided at the
    /// next reconcile or press ([`Rule::MarkedEcho`]), since a far end that
    /// sends the glyph and the zero-width mark it then carries apart lets a
    /// frame prove between the two. `false` whenever `confirmed_epoch` is.
    proof_opaque: bool,
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
    /// A stale guess's cell anchors a stale run (`cancelled_stale`).
    cancelled_at: Option<(u16, u16)>,
    /// The guess a Backspace cancelled at `cancelled_at` carried a STALE ANCHOR
    /// (`Prediction::stale`): its cell was placed from a live cursor an owed
    /// echo may have moved, so the cell the next run anchors on is that stale
    /// too ([`Rule::StaleCancel`]). Read only while `cancelled_at` is `Some`,
    /// and set with it.
    cancelled_stale: bool,
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
    disabled: u64,
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
    /// The op classes this epoch has watched echo exactly ([`PROVEN_MOTION`],
    /// [`PROVEN_INSERT_SHIFT`], [`PROVEN_ERASE_SHIFT`]), beside the glyph proof
    /// (`confirmed_epoch`), which every class needs first.
    /// Cleared with the glyph proof (`void_proof`): a cursor the program moved, a
    /// tail that moved or a boundary may put a different program behind the same
    /// line, a cooked `cat` after the shell that proved `←`.
    proven: u8,
    /// `proven` as it stood when the epoch whose ops are carried
    /// (`grandfathered_epoch`) ended: the carried ops keep displaying exactly as
    /// they did.
    carried_proven: u8,
    /// The standing proof of the carried epoch as it stood when that epoch ended
    /// (`proof_echo`, `proof_line_end`, `proof_opaque`): THE CARRIED STANDING
    /// TAIL. The carried guesses display on that proof, so it is held to what
    /// held it while it stood: at every reconcile the row right of its echo
    /// must still read as its run's snapshot (unless it was earned at the end of
    /// the line) and its echo's cell no more opaque than when it proved, or the
    /// carry ends (`grandfathered_epoch := None`) and the carried guesses go
    /// dark, tracked. Ending the epoch voided the proof, and with it the checks:
    /// a far end that writes a mid-line glyph before the tail it shifts proved
    /// on the frame between, a glyph typed then, an arrow declined with its
    /// pixels kept (or an Enter) carried it, and once the redraw landed it stood
    /// on glass over the tail that had moved (FRAME FIDELITY, the
    /// eight-hundred-seed sweep's seed index 361). A new run cannot start while
    /// carried guesses are pending, so the run's snapshot (`tail`) is still the
    /// carried run's.
    carried_proof: Option<(Prediction, bool, bool)>,
    /// The edit lane is switched off (`set_editing(false)`): every edit key is
    /// routed as before it existed. The kill switch, and a measurement's baseline.
    editing_off: bool,
    /// The host's last reconcile could say which cells are opaque
    /// ([`Predictor::reconcile_seen`]). A chain compares whole states, and a host
    /// that reads a wide glyph's half as text cannot tell it from a state's cell,
    /// so behind a legacy [`Predictor::reconcile`] no edit key is armed.
    edit_capable: bool,
    /// The cells `[lo, lo + seen.len())` of the chain's row as the last
    /// reconcile of a chain read them, out to the margin, or as the press that
    /// started the chain read them: what the ghosts are painted against, where a
    /// moved glyph is found ([`GhostCell::Moved`]), and the row a credited
    /// retirement re-bases the standing proof on (`rebase_proof`).
    seen: Vec<Seen>,
    /// Scratch for folding a chain's states: one state's text.
    fold_text: Vec<char>,
    /// Scratch for folding the displayed state with where each glyph came from
    /// (`compose_chain`): the glyph, and the column of the line's verified
    /// text it is, or `NO_SOURCE` for a glyph a key typed.
    fold_cells: Vec<(char, u16)>,
    /// The cells an engaged chain's overlay paints: its displayed state where it
    /// differs from the screen. Resident, so [`Predictor::overlay`] keeps
    /// returning a borrow.
    ghosts: Vec<Prediction>,
    /// The cursor of the displayed state of an engaged chain, as the last
    /// [`Predictor::overlay`] composed it ([`Predictor::overlay_cursor`]).
    ghost_cursor: Option<(u16, u16)>,
    /// Scratch for the transit wait (`in_transit`): one mark per cell read.
    ghost_marks: Vec<u8>,
    /// Scratch for an in-place world (`in_place_worlds`): one cell per column
    /// from the chain's `lo`, `None` where the world does not know it.
    world_cells: Vec<Option<char>>,
    /// The glyph-lane run in flight was anchored on the live cursor while an
    /// owed echo was in flight (`owed_at` inside the window at its first press):
    /// its guesses carry `Prediction::stale`. Every guess extending it does.
    run_stale: bool,
    /// The glyph-lane run in flight was started INSIDE the known line's text:
    /// its first press found the line's cursor left of the text's end (a motion
    /// of a chain retired there), so it is a mid-line insert by what the
    /// predictor knows, not by what the tail guard infers, and none of its
    /// echoes proves anything (`appended`). The inference alone missed a run
    /// typed on a SPACE of the line: a typed space reads blank, the cell under
    /// the cursor looked like nothing typed over, and the echo of a far end
    /// that writes the glyph before the tail it shifts proved the line as an
    /// append (FRAME FIDELITY, the eight-hundred-seed sweep's seed index 361).
    run_inside: bool,
    /// The glyph-lane run in flight was started by THE FIRST KEY of a prompt
    /// (`prompt_gate`), pressed at the very cell the shell's authenticated OSC
    /// 133 `B` named as its buffer's start ([`KeySample::input_start`]): that
    /// start. A line the run's guess there starts is WITNESSED
    /// (`EditLine::fresh`). `None` for a run started anywhere else, by any
    /// later key, or by a press that handed no start.
    run_witness: Option<InputStart>,
    /// DARK BEHIND A WAIT ([`Rule::DarkBehindAWait`]): the guess (its `seq`)
    /// the last reconcile turn left waiting on a cell that shows its glyph with
    /// the real cursor on it, a glyph its run's press did not see there. Adaptive
    /// shows nothing typed after it (`visible_count`) until a turn finds it
    /// gone or no longer so. Set and cleared by each turn of the glyph lane.
    held_wait: Option<u64>,
    /// THE CREDIT TAKEN BACK ([`Rule::CreditTakenBack`]): the cell and press time
    /// of the newest glyph-lane guess credited with an echo, while nothing of
    /// ours has moved the cursor back since (a chain started, a flush or an
    /// epoch boundary clears it). A turn that finds the real cursor on that
    /// cell or before it on its row takes the credit back (`credit_taken_back`).
    last_credit: Option<(u16, u16, Instant)>,
    /// THE FIRST KEY (R-WITNESS): whether the next key the host hands over is
    /// the first one typed at a prompt the shell drew after a line terminator,
    /// and the prompt that terminator was pressed at. A `B` vouches for where
    /// the buffer starts and that it was empty when the mark was sent; only
    /// the first key typed after it meets that buffer. Every key the host
    /// hands over shuts the gate, whatever it is (a glyph, an edit key, one
    /// not modelled), and so does a reset (a mode switched off hands the
    /// predictor no key); only a line terminator pressed at an authenticated
    /// prompt opens it ([`Predictor::note_line_submit_at`]). Without it a run
    /// typed at the `B`'s cell later in the prompt was taken for the buffer's
    /// start: one typed after an unmodelled key (a Tab that inserted a literal
    /// tab, drawn as blanks, then a word motion back to the start), one typed
    /// at a `B` zsh's `reset-prompt` left stale when it redrew the prompt
    /// narrower, or one typed ahead of the prompt (the review of increment 4).
    prompt_gate: PromptGate,
    /// THE LEARNED BINDINGS of the editor context the line-edge keys last
    /// reached ([`EditorBindings`]): the session's, carried in its
    /// [`LinkEstimate`].
    bindings: EditorBindings,
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
    /// THE FLOOR ([`Rule::FloorBelow`]): the row under the line of the run in
    /// flight, as its first press read it ([`KeySample::below`]): its index
    /// and its cells. A standing proof on that line is voided once a cell the
    /// press read blank shows text (`floor_moved`). `None` when the press read
    /// no such row, or its guess wrapped onto it.
    floor: Option<u16>,
    /// The cells of `floor`. Resident, refilled in place.
    floor_cells: Vec<Seen>,
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
    /// it voided), or the newest key whose echo moves the far end's cursor
    /// where the live cursor does not show it was pressed (an arrow declined, a
    /// key not modelled). Its echo's trailing output, the tail redraw or the
    /// motion, may still be on its way for one glitch window, and until then a
    /// blank row from the cursor is no evidence that the line ends there
    /// (`tail_line_end`).
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

    /// Count `n` ops through THE FOLD for [`PredictEvents::ops_folded`];
    /// nothing in a release build. Called once per loop that folds, after it,
    /// since the loop borrows the ops it folds.
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
            // Keys typed under `Off` reach no predictor (`prompt_gate`).
            self.prompt_gate = PromptGate::Shut;
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
        let sample = KeySample {
            cursor,
            coords,
            grid,
            ..KeySample::default()
        };
        self.arm_glyph(ch, &sample, now)
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
        self.arm_glyph(ch, sample, now)
    }

    /// [`predict_glyph`](Self::predict_glyph) and the predict entry points that
    /// wrap it: `sample.row` is the press's cursor row, if the host read one.
    fn arm_glyph(&mut self, ch: char, sample: &KeySample<'_>, now: Instant) -> bool {
        let (cursor, coords, grid, cells) = (sample.cursor, sample.coords, sample.grid, sample.row);
        // Every key shuts THE FIRST KEY's gate, armed or not: asked once, here.
        let first_key = self.first_key();
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
        // R-BIDI: a glyph that makes its row reorder, or one typed on a row
        // that holds such a glyph already, echoes where UAX #9 draws it, which
        // the logical column does not say ([`Rule::RightToLeftRows`]). Only a
        // run's first press reads the row; a glyph typed after it that would
        // make the row reorder is refused here, and what the far end draws
        // there meanwhile is unprompted output.
        if self.rule(Rule::RightToLeftRows) && (reorders(ch) || cells.is_some_and(row_reorders)) {
            self.count(Event::BidiRefusal);
            return self.decline(ch, now);
        }
        // The known line's cursor is inside its text (a motion retired there):
        // a glyph typed now is a MID-LINE insert, whose echo shifts the text right
        // of it. Where the edit lane can place it, it is the first op of a state
        // chain whose states shift that text with it (`arm_inside`), shown once
        // a mid-line insert of the epoch has echoed exactly. Where it cannot, the
        // proof covers the cell the cursor stands on, not an append there, so it
        // goes, with the line it named; the glyph is tracked dark by the glyph
        // lane, and its run proves nothing (`run_inside`), which the tail guard
        // alone could not promise: typed on a space of the line, it reads as
        // typed over nothing. Kept, the proof painted the glyph over the cell
        // under the cursor without shifting the tail (`ls --la` shown for
        // `ls - -la`).
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
        // over and the mark it was earned without (`standing_at_press`).
        if anchor.is_none() {
            self.standing_at_press(cells, cursor, cancelled.is_some());
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
            None if self.parked(sample.wrap_pending) && cursor.1.saturating_add(1) == cols => {
                (cursor.0, cols)
            }
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
            // …and the row under it, the floor its proof will stand on.
            self.take_floor(cursor, (row, col), sample.below);
            // A STALE ANCHOR: an echo owed now may still move the cursor this run
            // is placed from (`Prediction::stale`).
            let window = self.glitch_window();
            // …and so may one owed when the guess a Backspace just cancelled was
            // placed, if that guess was stale: its cell is no better than the
            // cursor it was placed from ([`Rule::StaleCancel`]). The owed echo
            // (a Delete or an arrow over ssh, which nothing models there) stepped
            // the far end's cursor back under that guess before its glyph
            // echoed, so the glyph landed a cell left of its cell and the guess
            // stayed pending there; the Backspace cancelling it erases that
            // glyph, and the cursor returns a cell left of the cancelled cell.
            // Anchored there as fresh, `aac` typed next was placed a cell right
            // of its echo, and the second `a`'s echo credited the first guess
            // (FIDELITY, the review of increment 5's sweep, seed index 3942,
            // case 141). Stale, that credit is refused while a later key for the
            // same glyph is pending (`attributable`), and the run proves nothing
            // on another key's echo.
            let stale_cancel =
                cancelled.is_some() && self.cancelled_stale && self.rule(Rule::StaleCancel);
            self.run_stale = stale_cancel
                || self
                    .owed_at
                    .is_some_and(|t| now.saturating_duration_since(t) <= window);
            // …and one typed inside the known text is mid-line, by knowledge.
            self.run_inside = inside;
            // …and one typed at the very cell the shell's authenticated `B`
            // names as its buffer's start, the first key typed at that prompt,
            // starts the buffer: a line it proves there is WITNESSED
            // (`EditLine::fresh`, `witness_of`).
            self.run_witness = self.witness_of(sample.input_start, first_key, (row, col));
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
            ghost: GhostCell::Typed,
            probe: false,
        });
        self.next_seq = self.next_seq.wrapping_add(1);
        // A Backspace now erases this op first (see `declined_tail`).
        self.declined_tail = 0;
    }

    /// Register a Backspace. We cancel only our OWN trailing prediction of the current
    /// epoch (the common "type then immediately fix" case), and only where the pair
    /// cancels (no shift of an unproven kind in flight, `pair_cancels`). Returns
    /// `true` if a guess was cancelled.
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
        let _ = self.first_key();
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
        // That owe is dated by THIS press, the Backspace's, not by the cancelled
        // glyph's: what it guards is the cancelled glyph standing on screen with
        // the cursor past it, which lasts until the erase's echo takes it off,
        // and that echo is due a window after the Backspace. Dated by the glyph,
        // it lapsed while the erase was still on time, and a glyph typed again on
        // the cancelled cell was credited with the old echo still standing there
        // (ATTRIBUTION, sweep seed index 455).
        //
        // Only a typed glyph pops: an edit op of a state chain (a motion, an
        // erase) is not erased by a Backspace, which erases text at the cursor
        // that op left (`predict_edit` arms that erase where it can). And only
        // where the pair cancels (`pair_cancels`): beside a shift of a kind not
        // yet proven, the far end may be overwrite mode, whose glyph and rubout
        // inside the text leave a blank where a glyph stood.
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
            // …and so is the erase's echo, as a space: part-way through `\b \b`
            // it is a blank on the cancelled cell with the cursor past it, which
            // is a space's echo exactly ([`Rule::CancelOwesItsBlank`]). Owed typed,
            // it refuses credit to a later space alone, until the erase is due.
            if p.ch != ' ' && self.rule(Rule::CancelOwesItsBlank) {
                self.owe_glyph(p.seq, ' ', now, now);
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
    ///   [`predict_backspace`](Self::predict_backspace) always has, where the
    ///   pair cancels (`pair_cancels`: no shift of an unproven kind in flight);
    ///   beside one it is an erase of the chain, armed as any Backspace is;
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
    /// Home, End, Ctrl-A, Ctrl-E and Delete are LEARNED keys
    /// (`predict_line_edge`): placed only on a line started at the shell's
    /// authenticated buffer start ([`KeySample::input_start`]), and shown only
    /// once the far end's echoes have taught the predictor their binding.
    ///
    /// The host never also calls [`note_unmodelled_input`](Self::note_unmodelled_input)
    /// for the key: every outcome has accounted for it.
    pub fn predict_edit(
        &mut self,
        key: EditKey,
        sample: &KeySample<'_>,
        now: Instant,
    ) -> EditOutcome {
        // An edit key is a key: THE FIRST KEY's gate shuts, armed or not.
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
        // A glyph held on its own cell has not echoed. Starting a state
        // chain on it could match that glyph plus Left at this same cursor,
        // crediting both before either key was read. Account for this edit
        // as unmodelled input; a resolved head leaves navigation unchanged.
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
        // Our own trailing glyph first: the pair's echoes cancel, whatever lane,
        // where they cancel at all (`pair_cancels`); beside a shift of a kind
        // not yet proven the Backspace is an op of the chain, an erase.
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
        // A chain would start (`start_chain`, which re-snapshots the row): the
        // standing proof it needs is held to its tail and its mark first,
        // against the row this press read, whether or not a glyph-lane run is
        // still in flight. Once a chain is engaged its reconciles hold only the
        // row past its states (`far_tail_moved`), against the snapshot this
        // press takes: skipped here, a run's glyph echoed before the tail it
        // shifts proved on the frame between, an arrow pressed after the
        // redraw re-snapshotted the moved row, and the run's next glyph stood
        // on glass over the moved tail.
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
            // At the end of the text the erase changes the last cell alone; inside
            // it, it SHIFTS the text right of the cursor one cell left, which the
            // shift's own class proof must admit (`op_visible`).
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
                // A learned key takes `predict_line_edge` above and never comes
                // here; were one to, it is input this lane does not model.
                _ => {
                    self.note_unmodelled_input(now);
                    EditOutcome::Declined
                }
            };
        };
        if !self.engaged()
            && let Some(cells) = sample.row
        {
            // A chain starts: nothing the last chain's reconcile read describes
            // this one's line; the row this press read does (`start_chain`).
            self.start_chain(cells, &e);
        }
        self.chain = true;
        // The op anchors where the chain's last state leaves the cursor, which a
        // cancelled glyph's cell is when nothing else is pending.
        self.cancelled_at = None;
        self.push_op(e.row, e.cursor, EDIT_OP_CH, op, class, now);
        self.count(Event::EditArmed);
        if class == OpClass::EraseShift {
            self.count(Event::ShiftArmed);
        }
        EditOutcome::Armed
    }

    /// A LEARNED KEY (Home, End, Ctrl-A, Ctrl-E, Delete) pressed
    /// (`predict_edit`): an op of a state chain where it can be placed, else
    /// input this crate does not model, as it was before the lane knew it.
    ///
    /// Placed only on a WITNESSED line (R-WITNESS, `witnessed`): the known
    /// line was started at the very cell the shell's authenticated `B` names
    /// as its buffer's start, and that `B` still stands where the line starts.
    /// Then `lo` is the buffer's start and the text the whole buffer, so the
    /// canonical effects are exact states: beginning-of-line puts the cursor on
    /// `lo`, end-of-line at the text's end, delete-char erases the glyph under
    /// the cursor and moves the text right of it one cell left. Anywhere else
    /// (a remote shell, whose marks never carry the nonce; a line started after
    /// a Tab completion, mid-buffer; a program a command started) none of them
    /// is a state the predictor can name.
    ///
    /// Shown only once LEARNED in the editor context (R-BIND,
    /// [`Rule::LearnedBindings`]): until then the op is a PROBE, armed and
    /// reconciled like any op but never displayed, nor anything after it, and
    /// its credited echo is one observation of the canonical binding
    /// (`credit_binding`); two make it learned. A divergence or an expiry a
    /// learned key's op heads unlearns it (`contradict_head`), and two ban it
    /// for the context: it is then input this crate does not model. A probe is
    /// armed only where its effect changes the state, so its echo can tell the
    /// canonical binding from a key bound to nothing.
    ///
    /// * Ctrl-A and Ctrl-E under a keyboard mode that escapes control chords
    ///   (a Kitty mode that reports functional keys: `CSI 97;5u`; xterm's
    ///   modifyOtherKeys at level 2: `CSI 27;5;97~`) go out as another key
    ///   (R-KITTY): not modelled.
    /// * End or Ctrl-E with the cursor at the end of the text is DECLINED WITH
    ///   ITS PIXELS KEPT ([`Rule::EndRefusal`]): an autosuggestion is accepted
    ///   there, which no state names, and whatever the far end makes of it,
    ///   it reads it after every key in flight and writes only after them.
    /// * Home or Ctrl-A at `lo`, and Delete at the end of the text, change
    ///   nothing canonically and teach nothing: not modelled, as before.
    /// * Delete is an erase (R-VACATE): the cells from the text's end to one
    ///   past every state must read blank.
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
        // Not learned in this context: a question to the far end, dark.
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

    /// R-WITNESS: whether the press `sample` may place a learned key on the
    /// chain ending at `e`: an authenticated `B` on the line's row, and
    /// ([`Rule::WitnessFreshPrompt`]) the line started at that very cell by
    /// that very prompt's first key (`EditLine::fresh`), its `lo` the `B`'s
    /// column. A `B` the line was not started at says the line may hold only
    /// part of the buffer: one started after a key this crate does not model
    /// (a Tab completion) begins wherever that key left the cursor. And a line
    /// witnessed at another prompt is no line of this one's buffer.
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

    /// THE FIRST KEY: what the gate says of the key being handed over now
    /// (`prompt_gate`), which it shuts. Every key entry point asks it once,
    /// before anything else, so a key refused, declined or not modelled
    /// shuts it as surely as one armed.
    fn first_key(&mut self) -> PromptGate {
        std::mem::take(&mut self.prompt_gate)
    }

    /// R-WITNESS at a run's first press: the start a run whose first guess is
    /// placed on `cell` witnesses, if any. The `B` must name that very cell,
    /// and ([`Rule::WitnessFirstKey`]) the press must be the FIRST KEY typed
    /// at its prompt (`gate`, as `first_key` read it for this press): open
    /// since a line terminator pressed at another prompt. The `B` vouches for
    /// the buffer it found empty, which only the first key typed after it
    /// meets. A run started at its cell by any later key may have text right
    /// of it the screen does not show (a literal tab drawn as blanks, spaces
    /// typed and stepped back over with a key this crate does not model), or
    /// stand at a `B` the shell left behind when it redrew the prompt (zsh's
    /// `reset-prompt` sends no new one).
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

    /// The learned keys are pressed in `context`: bindings learned in another
    /// hold nothing here, so a change starts them afresh. Ops of a learned key
    /// armed under the old context are flushed first (owed, as any flush owes):
    /// their echoes are the old editor's answers, no evidence of the new one's.
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

    /// A credited op of a chain was `key`'s: one echo of its canonical binding
    /// in the context its bindings hold for. The second makes it learned; a
    /// banned key learns nothing.
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

    /// The chain is flushed for a divergence, or expires: if its head is a
    /// learned key's op, the screen shows no state of it, which CONTRADICTS
    /// that key's canonical binding (a key bound to nothing waits out its
    /// window, a glyph inserted for it diverges). It is unlearned, and a
    /// second contradiction bans it for the context. Output nobody typed for
    /// can contradict a right binding too, which costs display alone.
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

    /// THE STANDING MARK and THE STANDING TAIL at a press that is about to
    /// re-snapshot the row (`arm_glyph` with nothing of its epoch to extend, a
    /// run's tail; `predict_edit` and `arm_inside` starting a chain, its base):
    /// it compares its own row (`cells`, read at `cursor`) with the snapshot
    /// first. Once an echo's glyph proved before its tail redraw landed, the row
    /// this press reads is the moved one, and a snapshot of it would let every
    /// later comparison pass. A glyph-lane run anchored on a cancelled cell,
    /// inside its pair's transit (`skip_tail`), skips the tail, which the next
    /// reconcile reads (no chain is in flight to stop it); a press that starts
    /// a chain never skips it, since the chain's reconciles hold only the row
    /// past its states, against the snapshot that press takes.
    ///
    /// THE STANDING MARK is read at or away from the end of the line: a mark
    /// that attached itself to the proof's echo since it proved (`proof_opaque`)
    /// says the line was typed before it; and so is THE STANDING GLYPH: an echo
    /// cell that no longer holds its glyph says the line was erased or redrawn
    /// (`holds_its_glyph`). Each voids the proof.
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

    /// A state chain starts on the known line at `e`, from a press that read
    /// the row (`cells`). What the press saw from `lo` to the margin is what the
    /// chain's first frames paint against (`seen`: a host may compose a frame
    /// before any reconcile of the chain reads the row), and it is the snapshot
    /// the chain is held to (`tail`, `tail_cells`): the row past every state of
    /// it may not move while the chain is in flight (`far_tail_moved`), and the
    /// standing proof re-reads it once the chain drains. The standing proof was
    /// held to the old snapshot at this press (`standing_at_press`), so the row
    /// right of its echo is the same in both.
    fn start_chain(&mut self, cells: &[Seen], e: &ChainEnd) {
        // The chain moves the cursor by keys of ours: no credit is taken back
        // for where it leaves it.
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

    /// A glyph typed with the known line's cursor inside its text and nothing
    /// pending (`arm_glyph`): a MID-LINE INSERT, armed as the first op of a
    /// state chain whose states shift the text right of the cursor one cell
    /// right with it (`fold_op`, class `InsertShift`). Placed where the chain
    /// lane places any key (`placeable`: the glyph proof stands, the cursor is
    /// where the line's last verified state left it, or a cancelled glyph's
    /// cell its pair returns it to), with the press's row read and no opaque
    /// cell on the line (R-OPAQUE), the text's end short of the last column once
    /// it moves (R-ROW), after the standing proof was held to that row
    /// (`standing_at_press`). `None` when it cannot be placed: the caller tracks
    /// the glyph as before, the proof voided and its run proving nothing.
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
        // A cancelled guess's cell anchors a shift only where that guess was
        // placed fresh: a stale one's cell may be a cell right of where its
        // erase returns the cursor ([`Rule::StaleCancel`]), and the glyph lane
        // then tracks the press as a stale run.
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

    /// An arrow that cannot be placed (`predict_edit`): DECLINED WITH ITS PIXELS
    /// KEPT. The far end reads it after every key in flight, and in the family no
    /// motion erases text, so every guess in flight still echoes where it is
    /// painted; flushing them, as every unmodelled key did, blinked out a
    /// typed-ahead tail at each arrow. What the arrow does is not ours to place
    /// (readline beeps at an edge, zsh does nothing, an autosuggestion is
    /// accepted), so it is OWED ([`Rule::EditOpsOwe`]), the epoch ends, and
    /// nothing is armed until the queue drains and the live cursor re-anchors.
    ///
    /// Its echo moves the far end's cursor, into the text it stood at the end
    /// of: for a window a blank row from the live cursor is no evidence that the
    /// line ends there (`moved_tail_at`). A run typed on the live cursor before
    /// the arrow landed read a blank row, took itself to be at the end of the
    /// line, and proved without comparing its tail, while the far end inserted
    /// it before the line's last glyph; the glyph typed next was painted over
    /// that glyph, which its echo moved one cell on (FRAME FIDELITY, found by
    /// review).
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
    ///
    /// That walk folds every pending op, up to [`EDIT_CHAIN_MAX`] of them, at
    /// each press that asks it (an edit key; a glyph typed while a chain is in
    /// flight, or inside the text), and is counted with the fold's ops
    /// ([`PredictEvents::ops_folded`]), one an op walked, as far as it went
    /// before a refusal stopped it.
    fn placeable(&mut self, cursor: (u16, u16)) -> Option<ChainEnd> {
        let (end, walked) = self.chain_end(cursor);
        self.folded(walked);
        end
    }

    /// [`placeable`](Self::placeable)'s walk: the chain's end, if it can be
    /// placed, and how many pending ops the walk applied.
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

    /// The pending op `p`, typed where `end` leaves the cursor, applied to it by
    /// its length and the cursor (`chain_end`); `None` where it cannot apply.
    fn end_step(end: &mut ChainEnd, p: &Prediction, grid_cols: u16) -> Option<()> {
        match p.op {
            // R-ROW: the text's end never reaches the last column (the
            // deferred wrap's slot), so the line never leaves its row. A
            // glyph inside the text pushes its end one cell on, as an append
            // does.
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

    /// Whether the press's row lets an edit key be armed at `end`: read at all
    /// (a host that hands no row arms none); nothing on it that makes it reorder
    /// ([`Rule::RightToLeftRows`]); no opaque cell from `lo` to one past the
    /// furthest end (R-OPAQUE: a wide glyph's half, marks or a reordered row is
    /// nothing a state can name); and for an erase, every cell
    /// from the text's end to one past the furthest end blank, or our own text
    /// there not yet erased on screen ([`Rule::VacateRefusal`]): anything else
    /// there (an autosuggestion, the buffer's unseen tail) is what the editor
    /// redraws over, or pulls into, the cell the erase vacates.
    fn press_row_clear(&mut self, row: Option<&[Seen]>, end: &ChainEnd, erase: bool) -> bool {
        let Some(cells) = row else {
            return false;
        };
        // R-BIDI: anywhere on the row, a glyph that makes it reorder, which can
        // move the line's own cells when the row's levels change.
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

    /// A glyph typed while a state chain is in flight (`arm_glyph`): an insert
    /// where the chain's last state leaves its cursor. At the end of the text it
    /// is an append (the glyph class); inside it, a MID-LINE insert whose state
    /// shifts the text right of the cursor one cell right (`InsertShift`, shown
    /// once a mid-line insert of the epoch has echoed exactly).
    ///
    /// A glyph inside the text that the chain cannot place (a wide glyph, the
    /// text's end reaching the last column, the chain's cap) still moves that
    /// text when it echoes, after every op in flight: it is owed and the chain
    /// flushed, so no frame caught between the pieces of that echo shows the
    /// state's pixels over a tail already moving ([`Rule::MidLineDeclineFlushes`]). One at the
    /// end of the text, or one no chain can place (a lost anchor), is declined
    /// as the glyph lane declines one: it inserts after every key in flight, so
    /// their pixels stay.
    fn arm_in_chain(&mut self, ch: char, cursor: (u16, u16), cols: u16, now: Instant) -> bool {
        if ch.is_control() {
            return self.decline(ch, now);
        }
        let end = self.placeable(cursor);
        // R-BIDI: a glyph that makes the row reorder is placed nowhere.
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
        // Started at the cell the shell's authenticated `B` named when this
        // run's first key was pressed: the buffer's own start (R-WITNESS). A
        // line started by a later guess of the run, its first retired without
        // credit, is not.
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

    /// Apply op `p` to the state `(text, cursor)` of a line starting at `lo`:
    /// THE FOLD, the one definition every state of a chain comes from (the
    /// safety theorem's PROVENANCE oracle implements it again, independently).
    ///
    /// * a glyph at the cursor: inserted there, the text right of it one cell
    ///   on (at the end of the text, nothing moves), the cursor one on;
    /// * `←`: the cursor one back; `→`: one on;
    /// * an erase: the glyph before the cursor removed, the text right of the
    ///   cursor one cell back, the cursor on the erased glyph's cell;
    /// * a line start (Home, Ctrl-A): the cursor on `lo`; a line end (End,
    ///   Ctrl-E): the cursor at the end of the text;
    /// * a delete: the glyph under the cursor removed, the text right of it one
    ///   cell back, the cursor where it was.
    fn fold_op(text: &mut Vec<char>, cursor: &mut u16, lo: u16, p: &Prediction) {
        Self::fold_cells(text, cursor, lo, p, |ch| ch);
    }

    /// [`fold_op`](Self::fold_op) over cells of any kind: `typed` makes the cell
    /// a typed glyph puts in the text. `compose_chain` folds glyphs with where
    /// each came from; every other fold folds glyphs alone. One body, so the
    /// states a chain matches and the state it paints cannot disagree.
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

    /// One past the last cell of a state's text starting at `lo`.
    fn text_end<T>(lo: u16, text: &[T]) -> u16 {
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
                Op::Erase | Op::Delete => hi.saturating_sub(1),
                Op::Left | Op::Right | Op::LineStart(_) | Op::LineEnd(_) => hi,
            };
            far = far.max(hi);
        }
        far
    }

    /// Reconcile a STATE CHAIN against the screen (`reconcile_seen`).
    ///
    /// 1. Read its row from `lo` to the margin, into `seen` (the cells past
    ///    every state are where a moved glyph's source may stand, and what a
    ///    credited retirement re-bases the standing proof on). An opaque cell
    ///    from `lo` to one past the furthest text end any state reaches flushes
    ///    (R-OPAQUE). With an erase pending, so does anything but a blank in
    ///    that last cell, the SENTINEL no state compares
    ///    ([`Rule::VacateRefusal`]): a split erase echo may still show the
    ///    chain's own vacated text, which step 6 waits through, but nothing past
    ///    every state is the chain's. On a line not proven at the end of the
    ///    row, the row past every state must hold what the chain's snapshot
    ///    holds there, or less (`far_tail_moved`), or the chain flushes.
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
    /// 6. No state matches: if every compared cell holds what some state
    ///    comparing it holds, or is a blank an op of the chain may be writing
    ///    ([`Rule::TransitBlanks`]), and the cursor is on the line's row inside
    ///    the text the chain's echoes write ([`Rule::TransitReach`]), the
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
        for c in lo..self.grid_cols {
            let seen = observe(row, c);
            if c < span_end && matches!(seen, Seen::Opaque(_)) {
                self.count(Event::OpaqueRefusal);
                self.flush();
                return;
            }
            self.seen.push(seen);
        }
        // A split echo may still hold this chain's own vacated text. The
        // sentinel past every state is never ours, even while no state matches.
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
        // A state an earlier op's echo shows part-way is no evidence of the keys
        // after that op (`part_way_before`, [`Rule::PartWayEchoes`]): the match
        // is no match, and the screen waits or flushes as one no state shows.
        let matched = self
            .match_chain(cursor)
            .filter(|&j| !(self.rule(Rule::PartWayEchoes) && self.part_way_before(j, cursor)));
        let Some(j) = matched else {
            if self.in_transit(cursor) {
                self.count(Event::TransitWait);
            } else {
                // A divergence: a learned key at the head was not its binding.
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

    /// THE STANDING TAIL of a chain in flight on a line not proven at the end
    /// of the row (`EditLine::end_known`): whether a cell of the row past every
    /// state of the chain (from `far`, the furthest text end any state reaches,
    /// to the margin) holds a glyph its snapshot (`tail`, taken where the
    /// chain's base was verified: its press, `start_chain`, or a credited
    /// retirement, `rebase_proof`) did not, or a glyph other than the one it
    /// held there. A cell that CLEARED (a right prompt the editor hides before
    /// the line reaches it, a suggestion it drops) has moved nothing.
    ///
    /// Such a line ends at `hi` only because the text its echoes left in place
    /// stayed in place. A glyph that appears or moves past every state is the
    /// far end drawing there, and it may be the buffer's own tail, drawn late:
    /// an editor that writes a glyph before the tail it shifts proved such a
    /// line on the frame between, and the chain's states, built on a line that
    /// ends where the buffer does not, were shown with that tail standing still
    /// while every shift moved it. The glyph lane's STANDING TAIL holds the row
    /// right of the proof's echo the same way; while a chain is in flight its
    /// states own the cells up to `far`, so only the row past them is held.
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

    /// Whether the screen (`seen`, the real `cursor`) is what the echo of an op
    /// before the `j`-th shows PART-WAY ([`Rule::PartWayEchoes`]): an echo that
    /// writes forward and then steps the cursor back. readline's insert inside
    /// the text writes the glyph and the tail it shifts, the cursor then at the
    /// text's end, before it steps back; zle's erase (and a cooked tty's,
    /// `\b \b`) writes the tail, then a blank on the cell it vacates, the cursor
    /// then at the old end, before it steps back. A Delete's echo (delete-char
    /// redrawn as zle and readline without DCH draw it) is the same: the tail
    /// rewritten from the cursor, a blank where the text ended, the cursor at the
    /// old end before it steps back. And a line edge's echo (Home, End) moves the
    /// cursor over the text a cell at a time, reprinting it or stepping back over
    /// it, the text as it was: part-way, the cursor is on any cell strictly
    /// between where it started and where it lands. Caught there, the screen can
    /// be a later state of the chain: a Backspace at the end and a space typed
    /// after it, whose state is a blank with the cursor past it, the erase's
    /// own echo before its step back. Retired at that state, the space was
    /// credited before the far end read it, and the space typed next was
    /// credited with its echo. Such a match says nothing of the keys after the
    /// op caught part-way: it waits for the step back, or flushes.
    ///
    /// The step back is a `\b` a cell, and a read can end after any of them: an
    /// erase's, a Delete's and an insert's echo is part-way with the cursor
    /// anywhere strictly right of the cursor its state leaves, up to the end of
    /// the text it rewrote ([`Rule::PartWayStepBack`]). A Delete's step back
    /// passes End's state (the text's end, one cell short of where it started)
    /// and, at `lo`, ends on Home's: caught at End's, Delete and End were
    /// credited with End unread, and the rest of the step back credited the
    /// Home pressed meanwhile.
    ///
    /// Cost: the chain's states before `j` folded once, each compared with the
    /// screen only where its part-way cursor is the real one.
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
            // A line edge's echo moves the cursor over the text it leaves as it
            // is, a cell at a time where the far end reprints it or steps back
            // over it: part-way, the cursor stands on any cell strictly between
            // where it started and where it lands.
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
            // The echo rewrites up to `part` and then steps back to `c`, a `\b`
            // at a time, in as many reads as the link splits it into: part-way,
            // the cursor is anywhere on that step back
            // ([`Rule::PartWayStepBack`]; off: at `part` alone). A Delete's
            // step back passes End's state on its way, and the rest of it Home's.
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

    /// Retire the first `j` ops of the chain, front first, onto the line
    /// (`reconcile_chain`, step 3). `false` when one was not attributable and the
    /// chain was flushed.
    ///
    /// Every one of the `j` is asked BEFORE any is credited
    /// ([`Rule::ChainAttributedWhole`]): the screen matched `S_j` as a whole, and
    /// an op among them that may be standing on another key's output (an echo
    /// owed from before it) says the screen may have reached `S_j` by that
    /// output, so no op before it is shown to have been read either. Asked one by
    /// one as each was credited, the arrows ahead of a re-typed glyph were
    /// credited (a sample of nothing, the motion class proven) on a cooked tty's
    /// echo of the cancelled glyph, before the tty had read either arrow, which
    /// it would echo as `^[[D` (ATTRIBUTION, sweep seed index 65). Each is asked
    /// under the window as it stood before this turn: a turn's evidence is judged
    /// by what was known before it, not by the samples it is about to record.
    ///
    /// With a shift of an unproven kind in flight the far end may be an editor
    /// that edits that kind IN PLACE (readline's overwrite mode), and the
    /// screen is then a state of that editor, not of the chain: the in-place
    /// worlds (`in_place_worlds`, the real `cursor`) are asked first. One that
    /// shows the screen with fewer keys read says `S_j` credits keys no far end
    /// may have read: nothing is credited, and the chain is flushed
    /// ([`Rule::InPlaceReadsFewer`]). One that shows it otherwise (with more
    /// keys, or another buffer) says the line is no longer known: the keys are
    /// credited, every world having read them, and then the chain and the
    /// proof go ([`Rule::ShiftProofDistinguishes`]). A shift kind is proven only
    /// where every world that edits it in place is refuted.
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
            // `flush` owes every op in flight, the `j` with the rest.
            self.count(Event::AmbiguousShift);
            self.flush();
            return false;
        }
        let ambiguous = worlds.other && distinguishes;
        let Some(mut line) = self.line.take() else {
            return false;
        };
        let mut credited_now = None;
        // The learned keys this retirement credits: each one echo of its
        // canonical binding, counted once the screen is known to be a state of
        // the chain and not an in-place world's (`credit_binding`).
        let mut learned = [0u8; Learned::ALL.len()];
        for _ in 0..j {
            let Some(p) = self.preds.pop_front() else {
                break;
            };
            // A shift proves its kind only where no in-place world shows this
            // screen ([`Rule::ShiftProofDistinguishes`]; off: any credited
            // shift does).
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
            // Its echoes came, and were its own; but the screen it matched is
            // also one an editor that edits in place shows, where the buffer
            // may be a cell shorter or hold another glyph under the cursor, so
            // the state folded from it is no known line. The line goes with
            // the proof that named its cursor (a glyph typed there is no
            // append), and the ops folded onto it with them.
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

    /// THE IN-PLACE WORLDS of a retirement at `S_j` (`retire_chain`), the real
    /// cursor `cursor` and the row the reconcile read (`seen`): what an editor
    /// that edits a shift kind IN PLACE would show after each count of the
    /// chain's keys, for every set of the kinds not yet proven for an op's
    /// epoch. Asked only while a shift of such a kind is in flight: every
    /// other op is the same in every world.
    ///
    /// A shift's state is the chain's own fold. Overwrite mode, which the
    /// family holds, REPLACES the glyph under the cursor where an insert
    /// shifts the text from it right, and its rubout BLANKS the glyph before
    /// the cursor where an erase shifts the text left; at the end of the
    /// buffer both act as the shifts do (`world_step`). A world starts from
    /// the line as verified (its text, then the cells the chain verified blank
    /// to `vacated`, and nothing known past them) and folds each op in turn;
    /// at each count `m` of keys it SHOWS the screen when the real cursor is
    /// its cursor and every cell it knows holds what the reconcile read there.
    /// Where its buffer and cursor are the chain's own `S_m` (no op it edits
    /// in place has told it apart yet) at `m ≥ j`, what it shows is the
    /// chain's own evidence, which the smallest match already reads.
    ///
    /// Judging each shift alone against the state before it, which no frame
    /// showed in a burst, was too little: a rubout and a glyph typed after it
    /// inside the text differ each from its edit in place, and together leave
    /// the very screen of the two edits in place (`a c`, then `aoc`), so both
    /// kinds were proven on overwrite mode's echo. A world folds every op from
    /// the verified line, so only what the screen shows is evidence.
    ///
    /// * `fewer`: a world shows the screen at some `m < j` (where the chain's own
    ///   `S_m` did not): the far end may have read fewer keys than `S_j` credits.
    /// * `other`: a world shows it at some `m` with a state other than `S_m`,
    ///   or meets an edge whose outcome it does not know (`←` or an erase at
    ///   `lo`, a buffer that would reach the last column): the line may be
    ///   another than `S_j`.
    /// * `proves`: a kind every world that edits it in place fails to show at
    ///   every count: a world showing the screen with the chain's own state is
    ///   no other line, but it is no evidence of the kind either.
    ///
    /// Cost: at most three worlds of `n` ops over the row from `lo`, beside
    /// the chain's own fold, only at a retirement with an unproven shift in
    /// flight.
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
        // Per world (`flip`): whether it showed the screen or met an edge.
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
            // `S_m`, the chain's own state, beside it.
            s_text.clear();
            s_text.extend_from_slice(&line.text);
            let mut s_cursor = line.cursor;
            for (m, p) in self.preds.iter().enumerate().map(|(i, p)| (i + 1, p)) {
                Self::fold_op(&mut s_text, &mut s_cursor, lo, p);
                let stepped = Self::world_step(&mut cells, &mut len, &mut cur, p, flip);
                // The chain's own state and the world's: two ops applied.
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
                // Its own state is the chain's evidence only from `j` on, where the
                // chain's state matched too; before `j` the chain's did not, and a
                // world that shows the screen there reads fewer keys.
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

    /// Op `p` in an in-place world (`in_place_worlds`) over its `cells` from
    /// `lo`, its buffer of `len` cells and its cursor `cur`, with the shift
    /// kinds in `flip` edited in place as readline's overwrite mode edits them:
    /// a glyph inside the buffer replaces the one under the cursor, a rubout
    /// inside it blanks the glyph before the cursor, both as the shifts do at
    /// the buffer's end; `→` at the end is the beep that moves nothing.
    /// `false` where the outcome is not known: `←` or an erase at `lo` (the
    /// buffer may begin left of it), a buffer that would reach the last
    /// column (R-ROW).
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
            // A learned key acts as its binding does in every world: only on
            // a witnessed line, whose start is the buffer's.
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

    /// The shift kinds proven for ops of `epoch`: this epoch's (`proven`), or
    /// the carried epoch's as it ended (`carried_proven`), else none.
    fn proven_for(&self, epoch: u64) -> u8 {
        if epoch == self.epoch {
            self.proven
        } else if Some(epoch) == self.grandfathered_epoch {
            self.carried_proven
        } else {
            0
        }
    }

    /// The op classes proven for op `p`'s epoch (`proven_for`).
    fn class_proven(&self, p: &Prediction) -> u8 {
        self.proven_for(p.epoch) & p.class.proven_bit()
    }

    /// Whether our own trailing glyph and a Backspace after it CANCEL, the
    /// pair's echoes leaving the line as it was ([`Rule::CancelsOnlyKnownShifts`]):
    /// so they do in every editor of the family, at the end of the text and
    /// inside it alike, unless a shift of a kind unproven for its epoch is in
    /// flight. Overwrite mode's glyph inside the text replaces the one under
    /// the cursor and its rubout then blanks (or, at the buffer's end, erases)
    /// it, so `b` typed over and erased is gone; and once such a shift has
    /// made the far end's buffer another than the chain's, a glyph the chain
    /// appends may be one overwrite mode replaces with. A glyph-lane run holds
    /// no shift, so it always cancels.
    fn pair_cancels(&self) -> bool {
        !self.rule(Rule::CancelsOnlyKnownShifts)
            || !self.preds.iter().any(|p| {
                matches!(p.class, OpClass::InsertShift | OpClass::EraseShift)
                    && self.class_proven(p) == 0
            })
    }

    /// The credited op `p` of a chain left the cursor at `cell`. A glyph of the
    /// current epoch proves it, as a glyph-lane append does. A motion or an
    /// erase never earns the glyph proof ([`Rule::MotionsNeverProve`]): an
    /// unechoed line never shows a glyph because of an arrow's echo. Where the
    /// glyph proof stands it moves the proof's cell to the cursor, and a motion
    /// proves its class, and so does a shift whose kind no in-place world shows
    /// (`proves`, `in_place_worlds`, [`Rule::ShiftProofDistinguishes`]):
    /// overwrite mode, which moves nothing, echoes the same screen as a shift of
    /// spaces.
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
            // A learned key proves its binding (`credit_binding`), no class.
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
        // The line's end may have moved: a run started there is judged afresh.
        self.line_end_at = None;
    }

    /// The chain retired to the state the whole screen showed, crediting `p`
    /// (the newest op of this epoch it credited) with the glyph proof standing:
    /// the standing proof now rests on that whole line, not on the echo of the
    /// run that built it, whose row a shift moves. Re-based, it reads as the
    /// line's last glyph (or its first cell, for a line erased to nothing), its
    /// tail the row from `lo` to the margin as this reconcile read it (`seen`),
    /// and it is at the end of the line exactly when the line was proven there
    /// (`EditLine::end_known`).
    ///
    /// Left on the run's echo, a proof beside a right prompt was voided by the
    /// first shift that moved the row right of that echo, which is every one:
    /// the shift's own state, which the screen matched whole, is the evidence a
    /// re-read of the old snapshot would refuse. Re-based, the row past the
    /// line is held to what this match saw there, by the chain's own check
    /// while one is in flight (`far_tail_moved`) and by the standing tail once
    /// it drains.
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
        let cell = (line.row, line.cursor);
        self.fold_text = text;
        self.folded(self.preds.len());
        if !null {
            return;
        }
        if !self.rule(Rule::NetNullUncredited) {
            // The rule off: credited, as if their echoes had come.
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
                // As if exact: the shifts among them prove their class too.
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

    /// `reconcile_chain`, step 6: whether the screen is part-way through an echo
    /// of the chain. Every cell some state compares holds what one of the states
    /// comparing it holds there, or is a blank an echo of the chain's ops can
    /// write, at or right of the leftmost cell an insert, an erase or a Delete
    /// starts writing at ([`Rule::TransitBlanks`]), and the real cursor is on
    /// the line's row, inside the text the chain's echoes write: from `lo` to
    /// the furthest end any state reaches ([`Rule::TransitReach`]).
    ///
    /// The cursor anywhere on the row was too much. A far end that prints a
    /// password prompt on the next row while a chain is in flight, then moves
    /// up a row (a reverse index), leaves the chain's row untouched and the
    /// cursor on it, ten cells right of every state: waited on, the chain kept
    /// displaying, and a glyph typed at that prompt joined the chain and was
    /// painted (SECRECY, the eight-hundred-seed sweep's seed index 230). No echo of
    /// the chain's own keys passes the cursor right of every state's text, or
    /// left of the text it verified; a frame catching a redisplay that does
    /// (through the prompt, out to a right prompt) flushes, which costs display
    /// alone.
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
        // A blank explains itself only where an echo of the chain's own ops can
        // write one (`Rule::TransitBlanks`; off: every blank cell): at or right
        // of the cell an insert, an erase or a Delete starts writing at (an
        // ICH's blank, a tail rewritten from there, the cell an erase vacates).
        // Left of that the chain's echoes write nothing, and a motion's write
        // no blank; a blank there that no state holds blank is text that moved
        // or went, which no state of this chain shows part-way: zsh's
        // `reset-prompt` redrawing the prompt narrower (`\r$ od\e[K`) moves the
        // line left of `lo`, blanks its old cells and leaves the cursor among
        // them, and with every blank explained the chain waited and painted its
        // text a second time over them.
        let blanks = self.rule(Rule::TransitBlanks);
        // Per cell: 0 compared by no state yet, 1 compared and unexplained, 2
        // explained by a state that compares it (a blank one holds blank: a
        // space of its text, a cell it vacated), or a blank the rule off
        // excuses.
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
        // The leftmost cell any op's echo writes at.
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
                    // Past this state's text, inside what it vacated: blank.
                    None => seen == Some(Seen::Blank),
                };
                *mark = if held { 2 } else { 1 };
            }
        }
        // …and the blanks the chain's own echoes may be writing.
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

    /// The cells an engaged chain's overlay paints (`overlay`): the state its
    /// visible ops leave, from `lo` to the furthest text end any of those states
    /// reaches, wherever it differs from the row as the chain last read it
    /// (`seen`): the chain's last reconcile, or, before any, the press that
    /// started it (`start_chain`), each out to the margin. So a frame composed
    /// after the key, before any output, paints only what the state changes
    /// against what that press saw: its text, and a blank on each cell it
    /// vacated; a moved glyph already standing where the state puts it (a
    /// suggestion's same glyph past the line) is left as it stands. Its cursor
    /// goes to `ghost_cursor`, unless the chain is carried across a terminator
    /// and [`Rule::CarryCursor`] is off.
    fn compose_chain(&mut self) {
        self.ghosts.clear();
        let visible = self.visible_count();
        let Some(line) = self.line.as_ref().filter(|_| visible > 0) else {
            return;
        };
        let (row, lo) = (line.row, line.lo);
        // Each glyph of the displayed state with where it came from: the column
        // of the line's verified text it is, or `NO_SOURCE` for a typed glyph.
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
        // What the screen shows at column `at` as the chain last read it (its
        // press read the row to the margin, and every reconcile does). Past what
        // was read, which no state reaches (R-ROW keeps the text short of the
        // last column), the line's verified base answers.
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
                // A glyph of the verified text a shift moved here is the very
                // glyph the screen shows at its source column, if this read saw
                // it there: a host may paint it as that cell.
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
    ///
    /// The prompt it was pressed at is unknown here, so THE FIRST KEY's gate
    /// stays shut (`prompt_gate`): no line at the prompt that follows is
    /// witnessed. A host that knows it calls
    /// [`note_line_submit_at`](Self::note_line_submit_at).
    pub fn note_line_submit(&mut self, now: Instant) {
        self.note_line_submit_at(None, now);
    }

    /// [`note_line_submit`](Self::note_line_submit), pressed at the
    /// authenticated prompt `prompt` (aterm-core's `Terminal::prompt_in_front`,
    /// read under the press's hold), or where none was in front (`None`).
    ///
    /// THE FIRST KEY (`prompt_gate`): a terminator pressed at a prompt opens
    /// the gate, so the next key, if it is typed at ANOTHER prompt's `B`, meets
    /// the buffer that `B` found empty, and a run it starts there is witnessed
    /// (`witness_of`). One pressed at no authenticated prompt (a command runs:
    /// the keys typed ahead go to the next prompt's buffer, or to the command)
    /// leaves it shut, and so does any key after it.
    pub fn note_line_submit_at(&mut self, prompt: Option<u64>, now: Instant) {
        self.prompt_gate = prompt.map_or(PromptGate::Shut, |after| PromptGate::Open { after });
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
    /// `now` is the press: its echo is owed from then (see `owed_at`). Until it
    /// has had a window to land, a blank row from the live cursor is no
    /// evidence of the line's end (`moved_tail_at`): Home, a word motion or a
    /// completion moves the far end's cursor where the live one does not show,
    /// as a declined arrow's does (`decline_motion`).
    pub fn note_unmodelled_input(&mut self, now: Instant) {
        // A key: THE FIRST KEY's gate shuts (`prompt_gate`).
        let _ = self.first_key();
        // The input's own echo is owed, and so is every guess `flush` retires:
        // their keystrokes reached the program.
        self.owe(now);
        self.moved_tail(now);
        // `flush` ends the epoch with the guesses: nothing is left to carry.
        self.flush();
    }

    /// End the confirmation epoch at a line terminator (see
    /// [`note_line_submit`](Self::note_line_submit)).
    fn end_epoch(&mut self) {
        self.count(Event::EpochEnd);
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
            // …and on the glyph proof it stands on, held to its tail and its
            // mark as it was (THE CARRIED STANDING TAIL, `carried_proof`).
            self.carried_proof = self
                .proof_echo
                .map(|p| (p, self.proof_line_end, self.proof_opaque));
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.last_credit = None;
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
        // A glyph-lane echo says how its cell read (`retire_echoed`); a chain's
        // line holds no opaque cell (R-OPAQUE).
        self.proof_opaque = false;
    }

    /// Forget the standing proof: the epoch is unproven until an echo proves it
    /// again (`confirmed_epoch`, `proof_cell`, `proof_echo`, `proof_line_end`
    /// and `proof_opaque` move together).
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
        self.proof_opaque = false;
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

    /// Whether the terminal is PARKED in the deferred-wrap slot at a press: the
    /// engine's own flag where the host hands it ([`Rule::EngineWrapFlag`]),
    /// else the last reconcile's reading (`deferred_wrap`).
    fn parked(&self, flag: Option<bool>) -> bool {
        match flag {
            Some(pending) if self.rule(Rule::EngineWrapFlag) => pending,
            _ => self.deferred_wrap,
        }
    }

    /// THE FLOOR of the run whose first guess is placed at `at` from the press
    /// at `cursor` ([`Rule::FloorBelow`]): the row under the cursor row as the
    /// press read it (`below`), when the guess is placed on the cursor's row.
    fn take_floor(&mut self, cursor: (u16, u16), at: (u16, u16), below: Option<&[Seen]>) {
        self.floor = None;
        self.floor_cells.clear();
        let Some(cells) = below.filter(|_| at.0 == cursor.0) else {
            return;
        };
        self.floor_cells.extend_from_slice(cells);
        self.floor = cursor.0.checked_add(1);
    }

    /// Whether the floor moved under the standing proof ([`Rule::FloorBelow`]):
    /// the proof's echo is on the row above `floor`, the real `cursor` is above
    /// the floor too (on it, the line wrapped there and the editor writes it),
    /// and a cell the run's press read blank shows text other than the glyph a
    /// guess in flight names there (a glyph typed ahead wraps onto the floor, and
    /// its own echo is no prompt; a `Password: ` printed over the cells it names
    /// is). A cell that cleared, or text that changed, moves nothing: a
    /// completion list zle leaves under the line, redrawn or cleared.
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

    /// The guess typed at `born` echoed somewhere other than an append: its tail
    /// redraw may still be on its way (`moved_tail_at`).
    fn moved_tail(&mut self, born: Instant) {
        self.moved_tail_at = Some(self.moved_tail_at.map_or(born, |t| t.max(born)));
    }

    /// The cell after `p`, where an echo of `p` leaves the cursor: the next
    /// column, or `p`'s own in the last column, where the terminal parks.
    fn cell_after(&self, p: &Prediction) -> (u16, u16) {
        self.after(p.row, p.col)
    }

    /// The cell after `(row, col)` as an echo written there leaves the cursor
    /// (`cell_after`).
    fn after(&self, row: u16, col: u16) -> (u16, u16) {
        let next = col.saturating_add(1);
        let parked = self.grid_cols > 0 && next >= self.grid_cols;
        (row, if parked { col } else { next })
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
    /// frame can tell: the run was not started inside the known line's text
    /// (`run_inside`), its own cell, read as `own`, came back as plain as its
    /// press saw it (`echo_unmarked`), and the run is at the end of the line
    /// (`tail_line_end`) or the row right of `p` is clean (`tail_clean`).
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

    /// Whether `seen`, read at the cell of `p`'s echo, is no more opaque than
    /// the run's press saw that cell ([`Rule::MarkedEcho`]): plain, or opaque
    /// where the press saw it opaque too (the blanks of a right-to-left row,
    /// `Opaque(None)`, are typed over like any blank).
    ///
    /// A ZERO-WIDTH TAIL. A combining mark occupies no cell of its own: it rides
    /// on the cell before it. When the far end's buffer holds one after its
    /// cursor (a motion that steps by code point stopped between a glyph and its
    /// mark), a glyph typed there is inserted BEFORE the mark, which then
    /// attaches to the inserted glyph instead. Every cell right of the echo
    /// still reads as the press saw it, and a run typed there from a row that
    /// reads blank to the margin is at the end of the line by every cell, so
    /// the tail guard proved it: `e` was then shown plain where it settled to
    /// `é` (FRAME FIDELITY, sweep seed index 573). The echo's own cell is the one
    /// place the hidden tail shows, and it shows there as the opaque cell no
    /// typed narrow glyph makes by itself. A row that becomes right-to-left
    /// under the run reads opaque too, and proves only from the next run's
    /// press, which sees it opaque: display deferred, never a wrong cell.
    fn echo_unmarked(&self, p: &Prediction, seen: Seen) -> bool {
        !self.rule(Rule::MarkedEcho)
            || !matches!(seen, Seen::Opaque(_))
            || matches!(self.tail_at(p.row, p.col), Some(Seen::Opaque(_)))
    }

    /// THE STANDING GLYPH ([`Rule::StandingGlyph`]): whether `seen`, read at
    /// the cell of the echo `p` a proof rests on, still holds the glyph it
    /// echoed (a space, a blank), by its plain answer: an opaque cell is the
    /// standing mark's question (`echo_unmarked`).
    ///
    /// The proof names the cell after its echo, where the cursor stands while
    /// nothing but our own keys has moved it. Output nobody typed for can erase
    /// or redraw the line and bring the cursor back to that very cell, and no
    /// check of the row right of the echo sees it when the proof was earned at
    /// the end of the line: the echo's own cell does. A password prompt printed
    /// on the same row and a screen clear that redrew `$ ` put the cursor on a
    /// pending space's cell, the redraw's trailing space credited it, and the
    /// proof `i` had earned, its `i` now a `$`, carried on to paint the secret
    /// typed next.
    fn holds_its_glyph(p: &Prediction, seen: Seen) -> bool {
        seen.legacy() == Seen::of_cell(p.ch, false).legacy()
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
    /// the same physical row. Both are retired as at a host [`reset`](Self::reset),
    /// with one difference: THE FIRST KEY's gate (`prompt_gate`), which a reset
    /// shuts, stands as it stood ([`Rule::ScrollKeepsFirstKey`]).
    fn observe_coordinates(&mut self, coords: u64) {
        if self.coords.is_some_and(|seen| seen != coords) {
            // Rows moved under the screen: every guess goes, as at a host reset.
            // THE FIRST KEY's gate (`prompt_gate`) stays as it stood: a reset
            // shuts it because the host may have let keys past the predictor
            // (a mode switched off hands none over), and a scroll lets none
            // past, so the next key handed over is still the first typed since
            // the terminator that opened it, and the prompt it names is the
            // engine's, whatever row it now stands on. Shut here, no line was
            // ever witnessed at a shell on the bottom row, where the Enter's own
            // output scrolls the screen before the next prompt's first key
            // (measured: `tests/editing_win.rs`'s `shut-at-scroll` runs, the rule
            // off, witness 11 of 300 lines at 1 to 160 ms, the ones typed before
            // the screen filled, where its `full` runs witness 282).
            let gate = self.prompt_gate;
            self.reset();
            if self.rule(Rule::ScrollKeepsFirstKey) {
                self.prompt_gate = gate;
            }
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
        // The cell of the echo the standing proof rests on, read once a turn
        // while it stands, before anything retires: THE STANDING GLYPH and THE
        // STANDING MARK below both read it.
        let proof_seen = self
            .proof_echo
            .filter(|_| self.confirmed_epoch)
            .map(|p| observe(p.row, p.col));
        // THE STANDING GLYPH (`holds_its_glyph`): the proof as it stood before
        // this turn stands only while its echo's cell holds its glyph, read
        // before anything retires, so a burst that redraws the line and moves
        // the cursor over the next guess in one frame is read as what it is.
        // Not while a chain is in flight: its states own the line's cells.
        if !self.engaged()
            && self.rule(Rule::StandingGlyph)
            && let (Some(p), Some(seen)) = (self.proof_echo, proof_seen)
            && !Self::holds_its_glyph(&p, seen)
        {
            self.moved_tail(p.born);
            self.void_proof();
            self.count(Event::StandingVoid);
        }
        // The echo the standing proof rested on before this turn (`proof_echo`).
        let proof_before = self.proof_echo.map(|p| p.seq);

        // A state chain retires by whole states, its glyphs included.
        let chain = self.engaged();
        if chain {
            self.reconcile_chain(real_cursor, now, &mut observe, &mut oldest_confirmed);
        }
        // Each turn says afresh which guess, if any, holds what follows it dark.
        self.held_wait = None;
        // Retire confirmed leading predictions; a different glyph at the head ⇒ the
        // program diverged from our guess ⇒ flush. A still-blank head ⇒ not echoed
        // yet ⇒ stop and wait for the next burst.
        while let Some(p) = self.preds.front().copied().filter(|_| !chain) {
            let own = observe(p.row, p.col);
            // THE OPAQUE HEAD ([`Rule::OpaqueHead`]): the cell the head guess
            // is typed into reads opaque and blank before its echo. The far end
            // drew something there no plain ghost shows: a zero-width mark its
            // buffer holds after the cursor, which a forced wrap carried onto
            // the next row's first cell (the guess's echo takes it on), or a
            // wide glyph's right half. Waited on as a blank, the guess was
            // painted plain over a cell that settled marked.
            if own == Seen::Opaque(None) && self.rule(Rule::OpaqueHead) {
                self.count(Event::OpaqueRefusal);
                self.flush();
                break;
            }
            match own.legacy() {
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
                        // DARK BEHIND A WAIT ([`Rule::DarkBehindAWait`]): the real
                        // cursor ON the guess's own cell, which shows its glyph
                        // though the run's press saw no such glyph there. A
                        // suggestion drawn after the press, or a tail a mid-line
                        // echo shifted under it, puts it there as the far end
                        // goes on editing; so does the guess's own echo with the
                        // cursor brought back over it by output nobody typed for
                        // (a password prompt printed after it and a reverse
                        // index), after which the far end echoes nothing. The
                        // guess waits as before; what is typed after it shows
                        // only once it retires.
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
                // Another glyph, but the very one the run's press saw in that cell,
                // with the cursor still on it: the screen as it was, not yet echoed
                // (a glyph typed onto an autosuggestion it diverges from: `x` over
                // the `o` of `echo hello`). Flushed, the guess went at the first
                // frame after its press, and the proof with it, so every suggestion
                // the typist left cost the keys after it their display
                // ([`Rule::ScreenAsItWas`]).
                Some(c)
                    if self.rule(Rule::ScreenAsItWas)
                        && real_cursor == Some((p.row, p.col))
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
        if let Some(sample) = oldest_confirmed {
            self.record_echo_regime(sample);
        }

        // THE STANDING MARK (`proof_opaque`): a proof, at the end of the line or
        // not, stands only while its echo's own cell reads no more opaque than
        // when it proved: the cell read before this turn retired anything, when
        // this turn left the proof where it was (a proof this turn earned was
        // read by its own retirement).
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

        // THE CREDIT TAKEN BACK (`last_credit`): the real cursor back on the
        // newest credited glyph's cell, or before it on its row, with nothing
        // of ours moving it there, says what was credited was no echo: an echo
        // never steps back over itself. An autosuggestion drawn part-way (its
        // first glyph, the typed one, with the cursor past it before the step
        // back) is that, and the key's own echo is still to land, where the guess
        // typed next would be credited with it. The key's echo is owed again
        // and the proof goes. A park in the last column leaves the cursor on its
        // guess as its echo (`echoed_past`).
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

        // THE FLOOR (`floor`): a proof, earned this turn or before, stands only
        // while the row under its line shows no text where the run's press read
        // a blank. Read whatever retired this turn: a password prompt printed on
        // the next row and a reverse index back onto the proof's cell land in the
        // same burst as the echoes that earned it.
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

        // THE STANDING TAIL (`proof_echo`): a proof earned before this turn stands
        // only while the row right of its echo still reads as the snapshot. One
        // row scan per turn while proven, none when this turn's own retirement
        // just compared that row, and none for a proof at the end of the line,
        // which has no tail to move (`proof_line_end`). While a state chain is in
        // flight its states own the line, and its own check holds the row past
        // them (`far_tail_moved`): a shift in flight moves the row right of the
        // echo, as the far end is meant to.
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

        // THE CARRIED STANDING TAIL (`carried_proof`): guesses carried past the
        // end of their epoch display on the proof it ended with, so that proof is
        // held to its mark and its tail for as long as one of them is pending.
        // Broken, the carry ends and they go dark, still tracked.
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
    /// differs from the screen the chain's last reconcile read (a typed glyph, or
    /// a blank where an erase vacated a cell), or, at a cell no reconcile of the
    /// chain has read yet, from the line's verified base; with its cursor in
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

    /// [`overlay`](Self::overlay) and [`overlay_cursor`](Self::overlay_cursor) in
    /// one borrow: the cells to paint and, for a state chain, its cursor. For a
    /// host that paints from the borrowed cells (no copy) and needs the cursor
    /// in the same breath.
    pub fn overlay_frame(&mut self, now: Instant) -> (&[Prediction], Option<(u16, u16)>) {
        let _ = self.overlay(now);
        let cursor = self.ghost_cursor;
        let cells = if self.engaged() {
            &self.ghosts[..]
        } else {
            // `overlay` left the queue contiguous, and its visible run a prefix.
            let visible = self.visible_count();
            self.preds.as_slices().0.get(..visible).unwrap_or(&[])
        };
        (cells, cursor)
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
                self.count(Event::Expiry);
                self.count(Event::ExpiryOwe);
                // No echo of the head's state in its window: a learned key
                // bound to nothing (a beep) is contradicted here.
                self.contradict_head();
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
            self.count(Event::Expiry);
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
    ///
    /// In every mode a PROBE (`Prediction::probe`: a learned key whose binding
    /// is not learned yet) ends the run: `Always` shows every guess, but a
    /// probe is a question to the far end, not a guess, and the state after it
    /// is built on a binding nobody has seen.
    fn visible_count(&self) -> usize {
        match self.mode {
            PredictMode::Off => 0,
            PredictMode::Always => self.preds.iter().take_while(|p| !p.probe).count(),
            PredictMode::Adaptive if !self.adaptive_slow => 0,
            PredictMode::Adaptive => {
                let shown = self.preds.iter().take_while(|p| self.op_visible(p)).count();
                // Nothing after a guess held waiting (DARK BEHIND A WAIT).
                self.held_wait
                    .and_then(|s| self.preds.iter().position(|p| p.seq == s))
                    .map_or(shown, |i| shown.min(i + 1))
            }
        }
    }

    /// Whether Adaptive may show op `p`: of the current epoch once its glyph
    /// proof stands, or of the carried epoch, and in either case once its class
    /// is proven there (THE CLASS LADDER, [`Rule::MotionClassProof`],
    /// [`Rule::InsertShiftProof`], [`Rule::EraseShiftProof`]). A motion shows
    /// only after a motion of its epoch has echoed exactly: a cooked tty never
    /// echoes one exactly, so it never shows one. A shift shows only after a
    /// shift of its kind has echoed exactly, moving a glyph: overwrite mode
    /// never moves one, so it never shows one. A learned key shows only once
    /// its binding is learned (`Prediction::probe` clear, [`Rule::LearnedBindings`]).
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
        // What is typed meanwhile may reach no predictor (a mode switched off
        // hands over no key, and the host resets it): THE FIRST KEY's gate
        // shuts until a line terminator opens it.
        self.prompt_gate = PromptGate::Shut;
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
        // …and its line editor's bindings are unknown too: learned from its own
        // echoes, they leave with the session (`take_link_estimate`).
        self.bindings = EditorBindings::default();
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
            bindings: self.bindings,
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
        self.bindings = est.bindings;
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
    ///
    /// And any later column was too much ([`Rule::CursorReach`]): the echoes of
    /// the run take the cursor no further than the cell after its newest guess
    /// on that row (`within_reach`). Further right, something else moved it: an
    /// autosuggestion the far end drew beside the echo, the frame caught before
    /// its cursor stepped back. The guess under the suggestion's first glyph was
    /// retired on it, and the run typed next was anchored on that cursor, a cell
    /// right of where it echoes (FIDELITY, sweep seed index 490). Held instead,
    /// the head is flushed by the cursor check, owed, and the run typed next is
    /// a STALE ANCHOR (`Prediction::stale`), credited by no echo while that owe
    /// is in flight.
    ///
    /// Parked ON `p` counts only for a run at the end of the line
    /// ([`Rule::ParkedAtTheEnd`], `tail_line_end`). A run typed mid-line pushes
    /// the line's own text right, and once the line's last glyph reaches the
    /// last column the cursor stands on it exactly as a park would leave it,
    /// before the key typed there was read (ATTRIBUTION, sweep seed index 605:
    /// three ICH echoes pushed a `d` there under a `d` guess). Only at the end of
    /// the line is there no tail to push.
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

    /// Whether column `c` of row `r` is as far as the echoes of our own keys can
    /// take the cursor: at or before the cell after the newest guess pending on
    /// that row, or after the cancelled pair's cell (`cancelled_pair`), whose
    /// glyph's echo lands before its erase's and leaves the screen a cell ahead
    /// for that long ([`Rule::CursorReach`]; off, any column is).
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
    /// later key for the same glyph is pending or owed typed. Credited, `c`
    /// typed twice after such a Backspace proved the line on the second `c`'s
    /// echo, a cell behind, and the third `c` was painted where the second echoed
    /// (FIDELITY, seed 76 of the eighty-seed sweep of the edit lane's world).
    ///
    /// A later key is as much a later key once a Backspace has cancelled its
    /// guess: the cancelled glyph's echo still lands (it is a typed owe, with the
    /// erase's echo after it), and lands in the stale guess's cell exactly as a
    /// pending one's would. Asking only the pending guesses, `n`, `n` typed
    /// before an erase landed, the second cancelled and `i` typed, credited the
    /// first `n` with the cancelled one's echo, and `i` was painted a cell right
    /// of where it echoes (FIDELITY, sweep seed index 151).
    ///
    /// An EDIT OP (a motion, an erase) asks about the keys after it too, in
    /// every run ([`Rule::EditOwedAfter`]): its state differs from the one
    /// before it by the cursor and by cells left blank, and a later key's echo
    /// makes both by itself (an Enter's carriage return stands the cursor at the
    /// line's start; a declined arrow steps it). So it is refused credit while
    /// an owe newer than any it was armed under is in flight. A Backspace that
    /// erased the space a line began with, carried across an Enter to a far end
    /// that had stopped echoing, was credited when the carriage return alone put
    /// the cursor on its cell (ATTRIBUTION, the eight-hundred-seed sweep's seed index
    /// 508); a glyph's state names its glyph, which no other key's echo writes.
    fn attributable(&self, p: &Prediction, now: Instant) -> bool {
        let window = self.glitch_window();
        let live = |t: Instant| now.saturating_duration_since(t) <= window;
        if p.owed.is_some_and(live) {
            return false;
        }
        // An edit op's evidence is a cursor and blanks, which a LATER key's echo
        // can make on its own ([`Rule::EditOwedAfter`]): an owe newer than any the
        // op was armed under is a key typed after it, still echoing.
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
    ///
    /// `own` is what the reconcile read at `p`'s cell: the echo's own cell, which
    /// says whether a zero-width tail rode in on it (`echo_unmarked`).
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
            // The newest credit of the glyph lane, which a cursor brought back
            // onto it takes back (THE CREDIT TAKEN BACK, `last_credit`).
            self.last_credit = Some((p.row, p.col, p.born));
            // A guess from BEFORE the last submit measures the link, but must not
            // arm the new line's display gate (password safety).
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
                // password typed there was painted (SECRECY, the eight-hundred-seed
                // sweep's seed index 796). It measures the link, and on a line a
                // glyph has proven it carries the proof on.
                if p.ch == ' ' && !self.confirmed_epoch && self.rule(Rule::SpaceNeverProves) {
                    self.ended_line_at(&p);
                    return;
                }
                if self.tail_decorated(&p) {
                    self.count(Event::DecoratedProof);
                }
                self.ended_line_at(&p);
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
        // Not credited: an echo no key is credited with moved the cursor past
        // `p`, and `p`'s own is still to land. The proof of its epoch named a cell
        // at or before `p`'s, where the cursor stands once nothing else moves it;
        // that no longer holds, and an erase in flight (a Backspace that popped our
        // guess) puts the cursor back on that very cell, where the next glyph was
        // painted and `p`'s echo then landed (FIDELITY, seed 43 of the sweep).
        if p.epoch == self.epoch && self.rule(Rule::UncreditedVoids) {
            self.void_proof();
        }
        let appended = self.appended((&p, own), observe, clean_row, cursor);
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
        if !self.preds.is_empty() {
            self.count(Event::Flush);
        }
        self.owed_at = self.owed_with_pending();
        self.preds.clear();
        self.last_credit = None;
        self.chain = false; // …a chain with them, and its line with the proof below
        // Folded into `owed_at` above: the guesses they could refuse are gone,
        // and what is armed next anchors on a cursor these echoes still move.
        self.glyph_owes.clear();
        self.void_proof();
        self.seen.clear();
        // Nothing flushed is displayed: a host asking for the cursor before its
        // next `overlay` (a web host after `set_predictive_echo("off")` or a
        // resize, both a `reset`) must not get the last chain's.
        self.ghost_cursor = None;
        self.anchor_lost = false; // nothing left to be anchored to
        self.grandfathered_epoch = None; // …and nothing left to carry pixels for
        self.carried_proven = 0;
        self.carried_proof = None;
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

    /// `ch` typed at `cursor` on a `grid` whose cursor row the press reads
    /// blank from the cursor to the margin: a host's press at the END OF THE
    /// LINE (`predict_glyph`). Only such a run reads a cursor standing on its
    /// guess in the last column as that guess's echo, parked there
    /// (`Rule::ParkedAtTheEnd`); a run whose press carried no row is not known
    /// to be at the end of anything.
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
        // The screen still shows the echo the standing proof rests on: an echo
        // erases nothing before it (THE STANDING GLYPH reads that cell).
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
        assert!(at_the_end(&mut p, 'a', (0, 79), (80, 24), now));
        assert!(at_the_end(&mut p, 'b', (0, 79), (80, 24), now));
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
        assert!(at_the_end(&mut p, 'a', (5, 79), (80, 24), now));
        assert!(at_the_end(&mut p, 'b', (5, 79), (80, 24), now)); // wraps to (6,0)
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
            // echo did, and its last glyph is the one that echo wrote (`Pas`), so
            // only the generation can tell the lines apart (a prompt with another
            // glyph there leaves the proof's echo cell without it: THE STANDING
            // GLYPH voids the proof without the generation's help).
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
            ..KeySample::default()
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
        // The standing proof's echo cell (THE STANDING GLYPH and MARK, once a
        // turn), four guess cells, one tail scan (columns 3..12), the
        // deferred-wrap probe reads nothing (the cursor is not at the margin).
        assert_eq!(reads, 1 + 4 + 9, "observe calls for a four-guess burst");
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

    /// THE SCREEN AS IT WAS ([`Rule::ScreenAsItWas`]). `s` typed at the start of
    /// `du` (after Ctrl-A, `sudo ` in front): its press read `d` on its cell,
    /// and the frame before its echo still shows `d` there with the cursor on
    /// it. That is the screen as the press saw it, not a divergence: the guess
    /// waits, and its echo (`sdu`, the cursor past `s`) retires it. Control:
    /// with the rule off the `d` flushes it at that first frame, as the
    /// predictor did before the review of phase 0, and its echo finds nothing
    /// pending.
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
                p.overlay(unread).len(),
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

    /// A row holding right-to-left text is one the frame draws in another
    /// order, and a key's echo there lands where UAX #9 puts it: a digit typed
    /// after `אב ` joins their run and is drawn left of them. So a glyph typed
    /// on such a row is declined, owed, and nothing of it shows (R-BIDI); and
    /// so is a right-to-left glyph typed on a row that holds none yet, whose
    /// echo starts such a run. Control: with the rule off, the row reads
    /// opaque on every written cell, its blanks included (`Opaque(None)`), and
    /// a guess typed on one, with a right prompt further along that its echo
    /// leaves in place, proves as on a left-to-right row.
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
                // A right-to-left glyph on a left-to-right row starts a run.
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
            ..KeySample::default()
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
    /// `x`, `x` are typed then. The erase lands, the two echo at (0, 2) and
    /// (0, 3). With the rule they anchor on the cancelled cell, where they echo,
    /// and their own echoes prove the line. Without it (`CancelTransit` off: the
    /// shipped code) they anchor on the live cursor a cell right, the second
    /// `x`'s echo stands in the first's cell with the cursor past it, that credit
    /// proves the line a cell behind, and the second `x` is painted at (0, 4),
    /// where it does not echo. (Typed as two spaces, the shipped case, the
    /// credit now proves nothing: a space proves no line by itself,
    /// `Rule::SpaceNeverProves`.)
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
    /// as the plain observe did, but for an opaque BLANK under the head guess,
    /// which flushes it ([`Rule::OpaqueHead`]: a mark or a wide glyph's half is
    /// there, which a plain ghost cannot show). With that rule off the two
    /// hosts act alike on every screen; with it on, on the screen where the
    /// head guess `b` stands on an opaque blank, the opaque host has flushed.
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
            ..KeySample::default()
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
    /// MID-LINE insert, whose echo shifts the text right of it: never an append
    /// over the cell under the cursor. It is the first op of a state chain
    /// whose state shifts the tail (a SHIFT), tracked dark until a mid-line
    /// insert of the epoch has echoed exactly; its echo proves the class, and
    /// the next glyph typed inside the text shows at once, the tail moved with
    /// it (the moved glyph named as the cell it came from). With the edit lane
    /// off the glyph lane tracks it dark, and the line it shifts is no longer
    /// known.
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

        // A shift the lane cannot place (an opaque cell on the line, R-OPAQUE)
        // is the glyph lane's, dark, and the line it shifts is no longer known.
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
    /// here the proof is voided while `←` is in flight, and `←`'s echo then
    /// lands. Control: a motion's credit that re-proves the epoch.
    ///
    /// No public path voids the proof under a chain in flight without flushing
    /// it or ending the epoch any more: the right prompt cleared beside the line
    /// while `←` is in flight, which once voided it through the standing tail,
    /// moves nothing past the chain's states (`far_tail_moved`), so the test
    /// voids it itself. The rule is the defence of the line against any that
    /// does.
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

    /// A host may compose a frame right after an edit key, before any reconcile
    /// read the chain's line (the web host does when no output came): the cells
    /// the state leaves as the verified line shows them are not painted, only
    /// what it changes (here the cell an erase vacates; a motion, none).
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

    /// `overlay_frame` is `overlay` and `overlay_cursor` in one borrow, for the
    /// glyph lane and for a chain alike.
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

    /// Nothing flushed is displayed, its cursor included: a `reset` (a web host's
    /// switch to `off`, a resize) or any other flush clears the chain's cursor at
    /// once, so a host that asks before its next `overlay` draws the engine's.
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

    // SHIFTS: glyphs and erases inside the known text.

    /// `text` typed one glyph a round trip at a `$ ` prompt with a right prompt
    /// standing at column 8: a slow line proven BESIDE the right prompt its
    /// echoes left in place, not at the end of the row.
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

    /// A Backspace where a motion left the cursor inside the text is a mid-line
    /// ERASE: its state shifts the text right of the cursor one cell left and
    /// vacates the text's last cell. Tracked dark until a mid-line erase of the
    /// epoch has echoed exactly (overwrite mode's rubout moves nothing); its
    /// echo proves the class, and the next shows at once, the moved glyph named
    /// as the cell it came from. Control: with the class proof off, the first
    /// shows on the glyph proof alone.
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

    /// The first mid-line insert of an epoch is the proof of its class, so its
    /// own frame stays dark. Control: with the class proof off it shows at once,
    /// the tail moved, on the glyph proof alone (where overwrite mode would
    /// replace `b` instead).
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

    /// A shift proves its class only by moving a glyph. `a` and two spaces
    /// proven, the cursor on the first space, and a space typed: the tail it
    /// moves is spaces, which read the same moved or not, and overwrite mode,
    /// which moves nothing, echoes the very screen an insert does. Credited, it
    /// proves no class. Control: any credited shift proves its class.
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
            // Overwrite mode leaves the same screen with a buffer one space
            // shorter: the line is no longer known, nor the cursor's cell.
            assert_eq!(p.edit_line_for_test().is_some(), !rule_on);
            assert_eq!(p.confirmed_epoch, !rule_on);
            assert_eq!(p.events_for_test().ambiguous_shifts, u64::from(rule_on));
        }
    }

    /// A lone glyph a shift moves lands on the cell past the line, which may
    /// hold whatever the far end drew there: a suggestion's glyph that is the
    /// very glyph moved, beside which overwrite mode's echo (`x` replacing `b`)
    /// is the insert's state exactly. `ab`, `←`, `x`: credited, it proves no
    /// class and leaves the line unknown. Landed on a cell the chain verified
    /// blank (vacated by an erase earlier in the chain), the same shift proves
    /// it: no edit in place puts `b` there. Control: with the rule off, the
    /// first proves and its state is the line.
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

        // `abc`, then Backspace, `←` and `x` in one chain: `b` moves onto the
        // cell the erase vacated, which the chain verified blank.
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

    /// A glyph typed inside the text that the chain in flight cannot place (a
    /// wide glyph) still moves the text when it echoes, after the chain's ops:
    /// owed, and the chain flushed (fail closed: no frame between the pieces of
    /// that echo shows the state over a moving tail). Control: declined as a
    /// glyph at the end of the line is, the chain's pixels kept.
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

    /// A shift whose text would reach the last column (the deferred wrap's
    /// slot) is not placed (R-ROW): the glyph lane tracks it, dark.
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

    /// THE STANDING TAIL of a chain in flight on a line proven beside its right
    /// prompt: the row past every state may clear (the editor hides its right
    /// prompt as the line grows toward it), but a glyph appearing there is the
    /// far end drawing past the line, which may be the buffer's own tail drawn
    /// late: the chain flushes. Control: the row past it is never read.
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

    /// A credited shift on a line proven beside its right prompt moves the row
    /// right of the run's last echo, which is the shift's own exactly-matched
    /// state: the standing proof is RE-BASED on the line the screen showed
    /// (`rebase_proof`), and stands. Held to the old snapshot it was voided by
    /// the first shift. Re-based, a glyph that then appears past the line still
    /// voids it.
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

    /// An edit key that would start a chain on the live cursor holds the
    /// standing proof to the row it read first, as a glyph's press does: the
    /// row right of the proof's echo moved since the last reconcile (a tail
    /// redrawn late), so the proof goes and the arrow is declined. Control:
    /// with the standing tail off it is armed on a line that does not end
    /// where the predictor thinks.
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

    // REVIEW OF INCREMENT 3: the in-place worlds, the pair that cancels, the
    // standing tail under a run in flight, a line end behind a declined arrow,
    // and the guards no test held.

    /// THE IN-PLACE WORLDS of a burst. `abc` proven, `←` echoed onto `c`, then
    /// a Backspace and `o` typed inside the text before either echoes.
    /// Overwrite mode blanks `b` (`a c`, which no state matches: the chain
    /// waits) and then writes `o` there: `aoc`, the very screen of the two
    /// shifts. Each shift alone differs from its edit in place, but no frame
    /// showed the state between them, and the world that edits both in place
    /// shows `aoc` too: neither kind is proven, and `n` typed next stays dark.
    /// That world's buffer is `aoc` as well, so the line stands. Control: with
    /// the rule off both kinds were proven, and `n` was shown shifted (`aonc`)
    /// while overwrite mode settles to `aon`.
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

    /// An in-place world that shows the screen with FEWER keys read. `ab`
    /// proven, `←` echoed onto `b`, then `x`, `→` and a Backspace typed before
    /// any is read. Overwrite mode, having read `x` alone, has replaced `b`:
    /// `ax`, the cursor after it, which is the chain's state after all three.
    /// Nothing is credited, and the chain goes. Control: all three were
    /// credited, `→` and the Backspace before the far end read either.
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

    /// A Backspace after a glyph typed inside the text, whose kind is not yet
    /// proven, cancels nothing: overwrite mode's glyph replaced `b`, and the
    /// rubout after it erases it (`a`), where the shifts' pair leaves `ab`.
    /// The Backspace is an op of the chain, an erase, dark like the glyph
    /// before it, and the `←` typed next waits behind both. Control: popped
    /// as a pair that cancels, `←` showed `ab` with the cursor on `a` while
    /// overwrite mode settles to `a`, once the transit wait also took the
    /// blank where `b` stood for an echo part-way: [`Rule::TransitBlanks`]
    /// alone flushes that chain at this frame (the `←` in flight writes no
    /// blank), so the control switches both off.
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

    /// THE STANDING TAIL at an edit press with a glyph-lane run in flight.
    /// `ab cd` typed and proven, the cursor taken onto the space by a key not
    /// modelled; `e` typed there and `c` typed ahead. Readline writes `e`
    /// before the tail it shifts: the frame between shows `abecd`, the row
    /// right of `e` as the press saw it, and `e` proves. The redraw lands
    /// (`abe cd`) and `←` is pressed before the next frame. The chain it would
    /// start re-snapshots the row, and its reconciles hold only the row past
    /// its states, so the press holds the proof to its tail first, finds it
    /// moved, and declines the arrow: nothing shows. Control: with the
    /// standing tail off, `c` was shown over the moved space (`abeccd`) while
    /// the screen settles to `abec cd`.
    #[test]
    fn an_arrow_with_a_run_in_flight_holds_the_standing_tail() {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            let (screen, at) = type_after(&mut p, "$ ", "ab cd", t0());
            assert!(p.adaptive_slow && p.confirmed_epoch, "fixture");
            if !rule_on {
                p.disable_rule_for_test(Rule::StandingTail);
            }
            // Alt-B and `←`, say: the cursor onto the space, the line unknown.
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

    /// A blank row read while an arrow we declined is still on its way is no
    /// evidence of the line's end. `b` typed; `←` declined while it is in
    /// flight, and `d` after it; `b`'s echo lands, and `b`, `n`, `s` are typed
    /// on the live cursor after it, over a blank row. The far end reads `←` and
    /// `d` first, and inserts the run before the line's `b`: `dbnsb`. Taken to
    /// be at the end of the line, the run proved without comparing its tail,
    /// and the glyph typed next proved again over the `b` its echo pushes on,
    /// and was painted where that `b` then settles (found by review). The row right of the run holds a `b` its press did not
    /// see: it proves nothing.
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

    /// A glyph-lane run started inside the known text proves nothing.
    /// `abcdefg i` proven, `←` twice onto the space: `o` typed there is inside
    /// the text, but its shift would bring the line to the last column
    /// (R-ROW), so the glyph lane tracks it. The space reads blank, so the run
    /// looks typed over nothing; readline writes `o` before the tail it
    /// shifts, and the frame between shows `abcdefgoi` with the row right of
    /// `o` as the press saw it. Control: proved there, the space typed next
    /// was painted over `i`, which settles a cell on.
    #[test]
    fn a_run_started_inside_the_text_proves_nothing() {
        for rule_on in [true, false] {
            let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abcdefg i");
            if !rule_on {
                p.disable_rule_for_test(Rule::InsideRunsNeverProve);
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

    /// A rubout of a glyph whose tail is all spaces leaves the screen an erase
    /// in place leaves too: `cab ` proven, `←` onto the space, a Backspace;
    /// overwrite mode blanks `b` (`ca  `), which is also the state of the
    /// shift (`ca ` and its vacated cell). Credited, it proves no kind and
    /// leaves the line unknown: the buffer may be a cell longer. Control: with
    /// the rule off, its state was taken as the line, a cell shorter than
    /// overwrite mode's buffer.
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

    /// A glyph typed inside the text holds the standing proof to its row
    /// before the chain it starts re-snapshots it (`arm_inside`): `abc` proven
    /// beside a right prompt, `←` twice, then `x` pressed over a row where a
    /// glyph has appeared past the line (`z`, which may be the buffer's own
    /// tail drawn late). The proof goes, and no chain is armed on it.
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

    /// An edit key pressed on a row that holds a right-to-left character
    /// anywhere (here past the line, where a right prompt may hold one) is
    /// declined (`press_row_clear`), and a right-to-left glyph typed while a
    /// chain is in flight is placed nowhere (`arm_in_chain`).
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

    /// THE STANDING GLYPH. `i` and a space typed, `i` echoed and proving.
    /// Then, in one burst, the space's echo, a password prompt printed on the
    /// same row and a screen clear that redrew `$`: the cursor is back on the
    /// space's cell, and `i`'s cell holds `$`. `8` is typed (at the password
    /// read), and the redraw's trailing space then lands: the space is credited
    /// by the cursor passing it. The proof `i` earned no longer stands, since
    /// its echo's cell no longer holds `i`, and a space proves no line by
    /// itself: `8` stays dark. Control: the proof stood, the space carried it
    /// on, and `8` was painted.
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

    /// A cancelled glyph's erase owes its echo as a space. `n` typed and
    /// cancelled by a Backspace, a space typed on the cancelled cell; the far
    /// end echoes `n` and erases it (`\b \b`), and a frame falls after the
    /// erase's blank and before its step back: a blank on the cancelled cell
    /// with the cursor past it, a space's echo exactly, though the far end has
    /// not read the space. It is not credited. Control: it was.
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

    /// An erase caught PART-WAY is no later state. `n` echoed and proving its
    /// line, a Backspace at its end and a space after it: a chain whose state
    /// after both keys is a blank with the cursor past it. zle's erase
    /// (`\b \b`) caught after its blank and before its step back shows exactly
    /// that, the space not read. Nothing retires until the step back lands,
    /// and then the erase alone. Control: both were credited on the erase's
    /// part-way echo.
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

    // THE REVIEW OF INCREMENT 4: the glyph lane's breaks its sweep found on main.

    /// A frame of two rows: `top` on row 0, `under` on row 1.
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

    /// `text` typed from `(0, 0)` one glyph a round trip, each press handing the
    /// row under it as `below`, each echo through `reconcile_seen` with that
    /// row shown under the line. Returns when the last echo landed.
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

    /// THE FLOOR. `ab` typed and proving, the row under it blank at the run's
    /// press; a prompt printed on that row and the cursor brought back onto the
    /// proof's cell: the proof is void, and nothing typed there shows. Text the
    /// row held at the press (a completion list under the line) moves nothing.
    /// Control: with the rule off the proof stands.
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

    /// THE FLOOR UNDER A WRAPPED GUESS. `abcdefghij` proving on a 12-column
    /// row, `k` `l` `m` typed ahead in one run: `m` wraps onto the floor at
    /// `(1, 0)`. A frame with the cursor still on the line and `m` there is
    /// that guess's own echo landing early, and moves nothing; a frame with
    /// `P` there (a `Password: ` printed over the cells the guesses name) is a
    /// prompt, and the proof is void. Control: with the rule off it stands.
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

    /// DARK BEHIND A WAIT. `ab` proving, `c` and `d` typed ahead; a frame shows
    /// `c` on its own cell with the cursor ON it, a glyph its press saw no trace
    /// of (a suggestion drawn after the press, a tail a mid-line echo shifted,
    /// or its own echo with the cursor brought back over it). `c` waits as
    /// before, and `d` does not show until `c` retires. Control: with the rule
    /// off `d` shows.
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

        // A motion may not turn the unresolved glyph into a chain state.
        // S0 is `ab` at 2; S2, `c` then Left, is `abc` at 2: the held
        // screen matches S2 although it is no evidence either key was read.
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
        // A genuine cursor advance resolves the same wait. Normal navigation
        // on the now-known line remains eligible.
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

    /// THE CREDIT TAKEN BACK. `a` typed; a suggestion beginning with `a` drawn
    /// part-way (its `a` with the cursor past it) credits it; the step back
    /// puts the cursor on that cell: the credit is taken back, `a`'s echo owed
    /// again, and the `a` typed next there is not credited with it. Control:
    /// with the rule off it is.
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

    /// THE ENGINE'S WRAP FLAG. The cursor on a filled last column reads as the
    /// deferred-wrap park to the last reconcile, but the engine says the
    /// terminal wrapped nothing (a line editor moved its cursor onto its own
    /// text there): the next glyph replaces that cell, and is placed on it. The
    /// flag set, it goes to the next row. Control: with the rule off the
    /// reading wins, and the glyph goes to the next row.
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
            // A key the lane declines, which tells the predictor the grid.
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

    // THE LEARNED KEYS: Home, End, Ctrl-A, Ctrl-E and Delete, at a prompt whose
    // shell signs its marks.

    /// The editor context of the learned keys' tests: one session, one nonce,
    /// DECCKM off, control chords sent as C0 bytes.
    fn context() -> EditorContext {
        EditorContext::new(1, Some(&[7; 32]), false, false)
    }

    /// The prompt (its block id) whose `B` the learned keys' tests type at;
    /// the line terminator before it is pressed at `PROMPT - 1`.
    const PROMPT: u64 = 7;

    /// The `B` of [`PROMPT`] after its `$ `, at `(0, 2)`.
    const B: InputStart = InputStart {
        cell: (0, 2),
        prompt: PROMPT,
    };

    /// [`sample`] at a prompt whose shell signs its marks: the authenticated
    /// `B` after the `$ ` prompt, at `(0, 2)`, in [`context`].
    fn signed(row: &[Seen], cursor: u16) -> KeySample<'_> {
        KeySample {
            input_start: Some(B),
            context: context(),
            ..sample(row, cursor)
        }
    }

    /// A line terminator pressed at the prompt before [`PROMPT`], at `at`: THE
    /// FIRST KEY's gate opens. Returns when its owed echo is past due, where
    /// the next prompt's first key may prove.
    fn submitted(p: &mut Predictor, at: Instant) -> Instant {
        p.note_line_submit_at(Some(PROMPT - 1), at);
        at + p.glitch_window_for_test() + MS(1)
    }

    /// [`type_after`] at a signed prompt: each press carries the `B`.
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

    /// [`edited_line`] at a signed prompt: the known line `text` from `(0, 2)`,
    /// started at the shell's authenticated buffer start by the prompt's first
    /// key, so WITNESSED.
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

    /// Press edit `key` at the signed prompt, the cursor at `(0, cursor)` over
    /// row 0 showing `screen`.
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

    /// `key` pressed at the signed prompt over `before` with the cursor at
    /// `from`, and its canonical echo a round trip later: `after` with the
    /// cursor at `to`. Returns when the echo landed.
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

    /// Home and End taught on a witnessed line `screen` whose text runs from
    /// column 2 to `end`, the cursor at its end: each pressed twice and echoed
    /// as beginning-of-line and end-of-line, which learns both.
    fn learn_home_and_end(p: &mut Predictor, screen: &str, end: u16, at: Instant) -> Instant {
        let mut at = at;
        for _ in 0..2 {
            at = echoed_edge(p, EditKey::Home, (screen, end), (screen, 2), at);
            at = echoed_edge(p, EditKey::End, (screen, 2), (screen, end), at);
        }
        at
    }

    /// LEARNED BINDINGS. On a witnessed line, Home is armed before anything
    /// says what the far end binds it to: as a PROBE, tracked and reconciled
    /// like any op but never shown. Its echo, the cursor on the line's start,
    /// is one observation of beginning-of-line; End's, the cursor at the end,
    /// one of end-of-line. A second each makes them learned, and Home then
    /// shows at once: the cursor and no cell. Control: with the rule off the
    /// first Home shows on the glyph proof alone.
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

    /// A probe is never shown, in any mode, and nor is anything typed after it:
    /// `Always` shows every guess, but a probe is a question to the far end.
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

    /// R-WITNESS. A learned key is placed only on a line started at the very
    /// cell the shell's authenticated `B` names as its buffer's start, the `B`
    /// still there: a line typed at a prompt that signs nothing, a press that
    /// hands no start, or a `B` elsewhere on the row leave it input this crate
    /// does not model (owed, the epoch ended). Control: with the rule off any
    /// `B` on the line's row will do.
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

    /// The prompt a line is witnessed at, if it is.
    fn witness(p: &Predictor) -> Option<u64> {
        p.line.as_ref().and_then(|l| l.fresh)
    }

    /// THE FIRST KEY. A run started at the `B`'s cell is witnessed only when
    /// its press is the first key typed at that prompt: the gate a line
    /// terminator pressed at ANOTHER prompt opened, and nothing since shut.
    /// Each case types `c` at the `B` with the gate shut, and its line is not
    /// witnessed: a key not modelled before it (a Tab that inserted a literal
    /// tab, brought back to the start by a word motion), a line terminator
    /// pressed at the same prompt (an Enter the editor kept), one pressed at
    /// no authenticated prompt or with none named (`note_line_submit`), a
    /// reset, and a mode switched. Control: with `Rule::WitnessFirstKey` off
    /// each is witnessed; and with the gate open, the first key is.
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

    /// THE FIRST KEY across a scroll. A shell on the screen's bottom row
    /// scrolls it with every Enter's own output, so the coordinate generation
    /// moves between the terminator and the next prompt's first key; no key
    /// went past the predictor meanwhile, and that key, typed at the new `B`,
    /// still starts a witnessed line. Shut by the move, no line was witnessed
    /// at a shell on the bottom row (the measurement of what predictive editing
    /// buys, `tests/editing_win.rs`, found 11 of its 300 lines witnessed at 1
    /// to 160 ms, all typed before its screen filled). Controls: a host reset at the same
    /// point still shuts the gate (it may have let keys past), and so does the
    /// move with [`Rule::ScrollKeepsFirstKey`] off.
    #[test]
    fn a_scroll_after_a_line_terminator_keeps_the_first_key_s_gate() {
        for (host_reset, rule_on) in [(false, true), (true, true), (false, false)] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::ScrollKeepsFirstKey);
            }
            let at = t0();
            // The screen before the Enter's output: another generation.
            p.reconcile_seen(Some((0, 2)), G + 1, false, at, seen_cells("$ "));
            let at = submitted(&mut p, at);
            if host_reset {
                p.reset();
            }
            // The Enter's output scrolled the screen: the new prompt's `$ ` at
            // generation `G`, which the next frame and the presses read.
            p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("$ "));
            let _ = type_signed(&mut p, "$ ", "cd", at);
            assert_eq!(
                witness(&p),
                (!host_reset && rule_on).then_some(PROMPT),
                "host reset {host_reset}, rule on {rule_on}"
            );
        }
    }

    /// THE FIRST KEY against a `B` the shell left behind: zsh's `reset-prompt`
    /// (an async prompt segment resolving) redraws the prompt narrower and
    /// sends no new `B`, so the host still hands the old cell. `ab` typed at a
    /// `$$$ ` prompt (witnessed), Home and End learned, the prompt redrawn as
    /// `$ ab`, and `h` typed at the old `B`'s cell, mid-buffer: its line is no
    /// witnessed one, and Home is not placed. Control: with the rule off the
    /// line is taken for the buffer's start, and Home is shown on the stale
    /// `B` while zle goes to column 2 (the review of increment 4's case).
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
            // The redraw, two cells narrower: the buffer from column 2.
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

    /// A learned key whose op heads a chain the screen shows no state of is
    /// CONTRADICTED: here Home, learned, then bound to nothing (no echo), so
    /// its op waits out its window and expires. It is unlearned, and the next
    /// Home is a probe again (dark); a second contradiction BANS it for the
    /// editor context, and Home is then input this crate does not model.
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
            // The line went with the expiry, owing its key; the next prompt's,
            // typed once that echo is past due, is witnessed.
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

    /// Bindings hold for one editor context: pressed in another (another
    /// nonce: a new shell, or a re-key; DECCKM, under which Home is `ESC O H`;
    /// a keyboard mode that escapes control chords), Home is a probe again.
    /// They travel with the session in its link estimate, and a session
    /// nothing is known about has none: `reset_session` itself forgets them.
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
            // Control: the same press in the learned context shows.
            let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
            let at = learn_home_and_end(&mut p, &screen, 5, at);
            let _ = press_signed(&mut p, EditKey::Home, &screen, 5, at);
            assert!(
                frame(&mut p, &screen, 5, at).1.is_some(),
                "{label}: control"
            );
        }

        // An unknown session: `reset_session` alone, nothing restored.
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

    /// A learned key pressed in a new editor context first FLUSHES the ops of
    /// learned keys armed under the old one (owed): their echoes are the old
    /// editor's answers. Home armed as a probe, then End pressed under another
    /// nonce: the Home op is gone (owed from its press) before End is weighed,
    /// and End, its line flushed with it, is not modelled.
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

    /// A learned key whose op heads a chain the screen DIVERGES from is
    /// contradicted as one that expires is: Home, learned, then echoed as a
    /// `~` inserted at the end (zsh without its integration reads `ESC [ 3 ~`
    /// so). It is unlearned (the next Home a dark probe), counted.
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

    /// R-KITTY. Under a Kitty mode that reports functional keys, Ctrl-A and
    /// Ctrl-E go out as `CSI u`: input this crate does not model (owed, the
    /// epoch ended), counted. Home goes out as `ESC [ H` there too, and is
    /// placed. Control: the same Ctrl-A without the mode is armed.
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

    /// R-END. End with the cursor at the end of the text the line knows is
    /// DECLINED WITH ITS PIXELS KEPT: an autosuggestion is accepted there. `c`
    /// typed and shown, then End: still shown, the epoch ended and the anchor
    /// lost. Control: with the rule off End is armed there, a line end that
    /// moves nothing.
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

    /// `abcde` witnessed, the cursor walked onto `b`, and Delete learned there
    /// from two echoes of delete-char (`b`, then `c`, gone, the cursor kept):
    /// the line `ade`, the cursor on `d`.
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

    /// Delete, learned as delete-char, erases the glyph under the cursor and
    /// SHIFTS the text right of it one cell left, the cursor where it was: on
    /// `ade` with the cursor on `d`, it shows `e` moved onto `d`'s cell (painted
    /// as the cell it came from) and the last cell vacated, the cursor kept.
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

    /// Delete is an ERASE (R-VACATE): it pulls the text right of the cursor
    /// left, so the cell the line's end vacates must read blank, at the press
    /// and at every reconcile while it is in flight. A cell past the end of
    /// `ade` holding text (a suggestion, a buffer tail the line does not know)
    /// refuses it at the press; drawn there while it is in flight, it flushes
    /// it. Controls: the blank row arms it, and a blank frame keeps it.
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

    /// Where its canonical effect changes nothing, a learned key teaches
    /// nothing and is placed nowhere: Home at the line's start and Delete at
    /// the end of its text stay input this crate does not model.
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

    // THE REVIEW OF INCREMENT 5: the breaks its sweep found on main.

    /// THE STALE CANCEL. A key not modelled (over ssh, Delete) is owed, and `o`
    /// is guessed on the live cursor after `xd`: a stale anchor. The owed echo
    /// stepped the far end's cursor back and erased `d`, so `o` echoes a cell
    /// left of its guess, which stays pending on the blank cell after it. A
    /// window later a Backspace cancels that guess, and `aa` is typed on its
    /// cell. The Backspace's echo puts the cursor back a cell left of that
    /// cell, so the first `a`'s echo lands a cell left of its guess, and the
    /// second's lands in it: credited, the line was proven a cell behind and
    /// the second `a` was shown where nothing of it lands. The cancelled cell
    /// of a stale guess anchors a stale run, which a later key for the same
    /// glyph refuses credit. Control: with the rule off, the first `a` is
    /// credited and the second shown.
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
            // `\b`, ` \b` and `o`: `o` a cell left of its guess, the cursor on it.
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
            // `\b \b`, `a`, `a`: `xaa`, the cursor after them.
            let (shown, _) = frame(&mut p, "xaa", 3, at + RTT);
            let credited = p.take_credited_for_test();
            assert_eq!(
                (credited.len(), shown.is_empty()),
                if rule_on { (0, true) } else { (1, false) },
                "rule on {rule_on}: {shown:?}"
            );
        }
    }

    /// THE STEP BACK. On `ade`, Delete learned and the cursor on `d`, Delete
    /// and End pressed: Delete's echo (`e`, ` `, `\b\b`) caught after its
    /// first `\b` leaves `ae` with the cursor at its end, End's state. That is
    /// Delete's echo part-way, its step back not done: nothing is credited, and
    /// the rest of it retires Delete alone. Control: with the rule off, Delete
    /// and End were both credited there, End with no output of its own.
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

    /// THE TRANSIT BLANKS. `ab` proven at a wide prompt (`$$$ `), the motion
    /// class proven by a `←` echoed, and a second `←` pressed and shown. zsh's
    /// `reset-prompt` then redraws the prompt narrower, `\r$ ab\e[K`: the
    /// line's text now stands left of its `lo`, its old cells blank, the
    /// cursor on `lo`, and no state matches. No echo of a motion writes a
    /// blank: the screen is no part-way echo of the chain, which is flushed.
    /// Control: with the rule off every blank was explained, and the chain
    /// waited, painting `ab` a second time over the blanks.
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
