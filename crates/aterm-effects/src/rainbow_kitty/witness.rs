// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE CONTENT WITNESS** (2026-09-12, the abandoned band) — the ribbon's
//! one content-aware retirement, and the engine half of a seam the host
//! feeds from its terminal lock.
//!
//! The ribbon is an ABSOLUTE-CELL record of the keys: a cell is `(row, col)`
//! and its clocks, and nothing in the engine knows what glyph is under it.
//! Since `1e3e4a31b` an erase invalidates no host coordinate (D2 — the
//! owner: *"when the screen is refreshed the rainbow disappears"* — a prompt
//! redraw must not wipe the ribbon), so the only things that could take a
//! cell off the glass were its own clocks. A TUI that re-lays its input box
//! (Claude Code, Codex) moves the TEXT and leaves the band where the text
//! was: the owner's *"stray rainbows … cursor trails that sometimes get
//! 'lost' or abandoned in complex CLI tools like codex"*.
//!
//! The witness closes exactly that, and nothing else:
//!
//! * after each glow tick the host hands the engine the live grid rows the
//!   resident ribbon occupies ([`RowSample`], per-column chars in the row
//!   probe's own convention — the lead glyph at its column, `'\0'` at a wide
//!   continuation, `' '` for a blank);
//! * the FIRST time a cell is seen holding a NON-BLANK glyph, the witness
//!   records it — the char, whether it is wide, and which half of a wide
//!   glyph the cell is. Blank → glyph only ARMS: a key whose echo lands a
//!   frame late is never retired, and a cell laid over a blank is left to
//!   its own clocks — unless the glyph landed on an old blank in the very
//!   batch that took the cell's run (2026-09-24,
//!   `Witness::landed_goes_with_its_run`): that glyph is the rewrite's,
//!   and the blank goes with its run (below);
//! * glyph → a DIFFERENT glyph RETIRES the cell on the fast melt
//!   (`Ribbon::retire_cells`, [`super::ribbon::RETIRE_MELT_S`]) — both cells
//!   of a wide glyph as one unit, whichever half changed: light is sitting
//!   under the WRONG letters, the stray the melt was built for;
//! * glyph → BLANK is a different verdict (2026-09-14, the new line's fade —
//!   the owner: *"when I went to a new line the contrail disappeared versus
//!   nicely fading"*). Nothing sits under the light, so it has nothing to
//!   be wrong over; the cell is RELEASED (`Ribbon::release_cells`) and its
//!   run leaves through the swoosh's own retract — drawn into the hand
//!   farthest-first over `RETRACT_DUR_S + RETRACT_FADE_S`, the head last —
//!   exactly as a row the hand left leaves everywhere else. That is Claude
//!   Code's Enter (the composer cleared), a Ctrl-U, a cleared line. **THE
//!   RUN DECIDES THE SPEED**: a typed run is one object, so if any of its
//!   glyphs was REPLACED this walk, every blanked cell of the run melts
//!   with it (a re-laid box that overwrote one letter and blanked the rest
//!   is a re-laid box); and if the run's blanked glyphs are found, in the
//!   same order at the same spacing, on another sampled row or elsewhere on
//!   its own — the text MOVED, an input box re-laid elsewhere with the
//!   light left where the text was — the run melts fast too, the mark
//!   following its text away. Only a run whose text is GONE from every row
//!   the witness can see is released;
//! * **a released record is KEPT** (the fix-up of 2026-09-14). A released
//!   cell is not stamped — it owns its column, unstamped, for the whole
//!   retract — so its record must stand for as long as it is resident, or
//!   the next walk would ARM it over whatever lands there: an Ink frame
//!   split across PTY reads paints the transcript's spinner on the row the
//!   composer left one frame after the clear, and the light would have sat
//!   under those wrong letters for the rest of the 0.64 s. A released
//!   record has exactly one thing left to say: a glyph landing under the
//!   cell is REPLACED text and the cell melts fast — the melt multiplies
//!   into the retract's envelope, monotone (`Ribbon::env_of`) — with its
//!   run (a run-mate still under a blank goes with it, the run being one
//!   object); a blank is still nothing; the same glyph back is D2. It is
//!   never released again, never searched for again, and never counted
//!   again: the walk reports every cell it named on `retire` that an
//!   earlier walk had already released, and `Engine::witness_rows` keeps
//!   `ribbon_retired=` at one count per cell. The never-armed cells of a
//!   released run (its spaces) get a released record of their own, so a
//!   glyph landing on one of them is read the same way;
//! * a record is about ONE CELL — `(row, col, born)`, not a position. A
//!   cell re-laid at the same column is a new birth with a record of its
//!   own, so a retype is never mistaken for an overwrite; and where TWO
//!   cells are resident at one position (one owner per cell holds per
//!   cohort — a key typed back over a cell an abandoned cohort still owns
//!   mints a second) each is judged on its own record and retired by its
//!   own identity: the stale one for the glyph it was laid under, the key's
//!   own never, on the frame it is born or after. A cell the ribbon drops
//!   is forgotten with it;
//! * **a blank goes with its run.** A cell laid over a blank — the space in
//!   `hello world` — has no glyph of its own to witness: blank before, blank
//!   after the row is cleared, it could never be retired by content and
//!   lingered ALONE on the abandoned row for its whole life, a one-cell
//!   stray. When a run is released whole or every witnessed glyph in it is
//!   retired, its never-armed cells go with it on the same clock. A run
//!   released in PART — recorded glyphs still standing — takes a
//!   never-armed cell by the span-plus-tail rule (2026-09-21,
//!   [`Witness::released_span_takes`]): the cell goes when it lies inside
//!   the span of the cells the run lost, or past that span with no standing
//!   glyph of the run beyond it on its side; it stands when a glyph of the
//!   run still stands beyond it. So the interior space of `hello world`
//!   with one glyph blanked stands (glyphs on both sides), the typed
//!   trailing space after a cleared `world` goes with the suffix, and the
//!   space before a cleared suffix stands because `hello` does. If some
//!   letters survive a partial repaint, unchanged spaces stay between them;
//!   changing one glyph must not split the ribbon at every word boundary.
//!   A redraw of the same text retires nothing (D2).
//! * **a torn read is not a cleared line** (2026-09-22 — the owner, on 0.89:
//!   *"There is a rainbow trail gap in 0.89. I'm not sure what is causing it
//!   but it seems like some kind of back cursor movement bug."*, three dark
//!   cells under `TO ` of `INTO `). D2's premise below — one batch, one
//!   sample — does not hold for a frame larger than a PTY read: a
//!   full-region composer redraw over 1 KiB, which the macOS PTY hands over
//!   in 1024-byte reads, presented between two of them, shows the composer
//!   row `CSI 2K`-cleared and written only up to the boundary, the caret
//!   hidden. Every cell right of it reads as glyph → BLANK, and the
//!   identical text comes back 2 to 30 ms later. WHAT WAS AND WAS NOT
//!   WITNESSED: the shape is driven byte for byte at the host seam
//!   (`tests/torn_read_gaps.rs`), which is where every measurement below
//!   comes from; the four real Claude Code recordings beside it
//!   (`tests/fixtures/claude-composer-2026-09-21*.ptylog`) carry no
//!   `CSI 2K` at all and only their startup paint reaches 1 KiB, because
//!   Ink's diff renderer writes changed cells, so they replay as a guard and
//!   not as the reproducer. The owner's own frame — a composer under a
//!   running agent, with a spinner and rules repainting above it — was not
//!   recorded, so the cause of his screenshot is this shape reproduced, not
//!   this shape caught. Three verdicts made that permanent, each
//!   measured at the host seam (`tests/torn_read_gaps.rs`) and each now
//!   answered: a blanked fragment of a run with a recorded glyph still
//!   STANDING in place is released, never searched for — the moved-text
//!   search has no length floor, and `TO` stands in `NEED TO` and `STOP`, so
//!   the owner's `TO` was ruled moved text and melted, which nothing
//!   restores ([`Witness::walk`]); one blanked glyph is never moved text at
//!   all (`Witness::moved`); a restore judges each partial release on its
//!   own cells, lifts it as soon as one of its glyphs is exactly back — the
//!   walk that follows gives the complete frame the verdict it would have
//!   had without the torn one — and a record armed after the release is no
//!   evidence either way (`Witness::restored_runs`); and a released run
//!   whose text still stands takes only the never-armed cells the loss is
//!   around — the span-plus-tail rule of the entry above
//!   ([`Witness::released_span_takes`], which landed first for the same
//!   defect class read from the owner's next report) — so an unrestored
//!   tail no longer combs the text left of it.
//! * **a column still blank when a part is lifted is text that really went**
//!   (2026-09-22, the review round — the owner's class reached through the
//!   back cursor movement he actually named). The lift also demanded that
//!   NO cell of the part be under a blank, and a Backspace or ⌃W erases one
//!   for real: its column is blank on the complete present too, so the part
//!   was never lifted and every standing letter right of the boundary
//!   retracted while its glyph stood — 38 cells dark within 160 ms, then a
//!   29-cell hole for about a second once the hand typed on (85 frames with
//!   real 1024-byte reads under the spinner). The part is lifted on its
//!   returned ink alone now. A column still blank is not evidence against
//!   the rest of the part, and it needs no special case: the walk runs on
//!   that same sample straight after and names it, as a one-glyph run
//!   released to the retract, which is what it is. Measured in
//!   `tests/torn_edit_gaps.rs`; one glyph is never moved text, so this
//!   cannot melt.
//! * **one glyph means one GLYPH** (2026-09-22, the review round). The
//!   length floor counted CELLS, and a wide glyph owns two of them — so one
//!   CJK character cleared the floor and took the melt on the very
//!   coincidence the floor was written against, in the languages where one
//!   glyph really is a word. The continuation half is not a glyph
//!   (`Witness::moved`).
//!
//! **THE BAND FOLLOWS ITS TEXT** (2026-09-21, the owner on v0.90.0 with
//! Claude Code in the window: *"when typing wraps to a new line the
//! previous row's rainbow vanishes suddenly while the new row populates"*).
//! Claude Code's bottom-anchored composer grows by repainting ONE ROW
//! HIGHER without a scroll (measured byte for byte:
//! `docs/measured/claude-code-composer-wrap-bytes-2026-09-21.md`): the
//! text row is rewritten a row up minus the word the wrap moved, the
//! continuation row holds that word beside the caret, and the caret makes
//! a same-row backward move. The witness used to read the old row's
//! glyphs as replaced and melt the whole run in `RETIRE_MELT_S` — the
//! vanish — because the row the text went TO was never sampled and
//! nothing translated cells. So the host now also samples the row above
//! and below every ribbon row (`Engine::ribbon_rows_for`), and a FOLLOW PASS
//! runs at the START of the tick, before the tick's events are replayed
//! ([`Witness::follow_runs`], `Engine::follow_rows`): a run whose armed
//! glyphs are gone from their own row and stand, at their own columns,
//! one row away (nearest first: `−1, +1, −2, +2`) as a block is
//! TRANSLATED there with every clock intact (`Ribbon::translate_run`),
//! and its records re-keyed ([`Witness::translate_cells`]). The walk after
//! the tick then finds the cells under their own glyphs and retires
//! nothing; `ribbon_followed=` counts them beside `ribbon_retired=`. The
//! pass is a pure function of the records and the samples; it allocates
//! nothing past warm-up and is bounded by the runs' own widths.
//!
//! **A TWIN IS WHAT STOOD BESIDE THE LINE — BY TIME** (2026-09-25). A glyph
//! the follow pass finds is evidence of a move only if it ARRIVED there, and
//! the witness knows it did NOT only from what it saw beside the record
//! while the record's own glyph STOOD. So every record ACCUMULATES, per
//! follow offset, on every sampled frame ([`Witness::walk`],
//! [`Seen::observe`]): a copy of its glyph on the row that far away is a
//! [`Seen::twin`] — at once on the arming frame (the copy was there first:
//! `$ cd ..` typed under `$ cd ..`; and a record the shape pass catches up
//! to a new glyph is armed again), and otherwise once it has stood there
//! for [`TWIN_MIN`] (40 ms, wall time, seen on every sampled frame between;
//! [`Seen::pending`] and [`Seen::pend_ms`] keep the clock). A copy on a row
//! a SCROLL brought in came in with the row, so its clock is dated from the
//! last walk before the scroll ([`Witness::translate`] starts it at
//! [`Witness::last_walk`] for the rows at the bottom of the region that
//! scrolled — the grid's, or the focused pane's in a split), and the first
//! look that finds it there makes it a twin once `TWIN_MIN` has passed since
//! then. A record ARRIVED at a move iff the destination holds its glyph and
//! it is not a twin there ([`Seen::arrivals`]); a twin whose glyph stands
//! where the moved text would put it is NEUTRAL: it testifies neither way,
//! and about no row but its own. A record a follow CARRIES, or the shift
//! pass moves along its row, starts over: its evidence was about the rows
//! around where it stood ([`Witness::translate_cells`],
//! [`Witness::shift_cells`]).
//!
//! **WHAT THE WITNESS DID NOT SEE IS NOT EVIDENCE — EITHER WAY** (0.93.0's
//! reading, kept). A destination no sample has shown without the glyph is
//! still one the glyph may have ARRIVED at: a band moved twice on
//! consecutive frames, a paste then a wrap on the next frame, a band
//! carried a moment ago and a torn repaint lasting two frames all follow
//! (`an_unsampled_neighbour_is_no_evidence_either_way`,
//! `tests/moves_are_followed.rs`). Reading an unseen destination as
//! "not arrived" melted those moves under their own text; the copies such a
//! reading would catch are caught by the time rule instead. Two places
//! where the witness knows it has NOT seen what stands there are read
//! fail-closed:
//!
//! * a row a BAND MOVE carried across the record's offset is not clear
//!   until a sample shows it without the glyph ([`Witness::translate_band`]:
//!   vim's `yyp` opens the line under the one just typed and writes the
//!   copy into it in one batch);
//! * a run beside a row the host was asked for and did NOT deliver — the
//!   composed (split or zoomed) host's second, generation-checked read,
//!   dropped under streaming — passes no verdict for one walk
//!   ([`Witness::find_deferred_runs`]), so a line moved onto the withheld
//!   row follows on the next frame instead of melting.
//!
//! The rows the host samples are 0.93.0's — each band and its `±1`, the
//! caret's row — and, in slots of their own after the bands', a WAITING
//! KEY's row and its `±1` (a typed key's, or a delivered paste's), so the
//! frame that arms the key's records sees what was there first
//! (`Engine::ribbon_rows_for`, `CursorGlow::ribbon_rows`). No row two away is
//! named for its own sake (`no_row_two_away_is_named`): each would be a new
//! place for a copy to be taken for the line — fzf re-sorting its list under
//! a cleared query, a streamed row quoting the composer, a flicker two rows
//! up (`tests/copies_are_not_moves.rs`).
//!
//! THE LIMITS, stated as laws so that a change to any is seen:
//!
//! * a copy that stood beside the line for less than [`TWIN_MIN`], or on
//!   one sampled frame, before the line was erased is not told from a torn
//!   repaint, which must follow — the erase carries the band onto the copy,
//!   as 0.93.0 did (`tests/copy_beside_the_line.rs`,
//!   `the_limit_a_copy_that_stood_less_than_twin_min_is_a_move`);
//! * a copy written in the SAME batch as the erase is, glyph for glyph, the
//!   line relocated (fzf re-sorting an item one row away as the query is
//!   cleared, a relocation onto a new last row), which must follow;
//! * a record a follow carried starts over, so a copy standing beside the
//!   row it was carried to is a twin only once the witness has seen it stand
//!   there `TWIN_MIN` — a line moved beside an identical line and killed
//!   sooner lights that line (`tests/copies_are_not_moves.rs`,
//!   `the_limit_a_line_moved_beside_its_twin_and_killed_before_it_was_learned_lights_it`).
//!   Taking the carried record's first look as an arming look would refuse
//!   the mirror image, a real move onto the copy's row right after the
//!   carry, which cannot be told from it (THE LIMIT below);
//! * a TWIN IS STICKY for the record's life: a copy that stood `TWIN_MIN`
//!   beside the line and then went away keeps that offset neutral, and a real
//!   move onto that row later — every record of it such a twin — is refused
//!   and melts (`tests/moves_are_followed.rs`,
//!   `the_limit_a_move_onto_a_row_a_copy_stood_on_for_twin_min_melts`);
//! * **and the other way round, A TORN REPAINT THAT STANDS [`TWIN_MIN`] IS
//!   A COPY** (the chosen trade). A program with no synchronized-update
//!   bracket that draws a line's new row and erases the old one a while
//!   later shows the witness, on the frames between, a copy standing beside
//!   the line — exactly what fzf's late list or a quoted line shows it
//!   before an erase. Once that copy has stood `TWIN_MIN`, seen on two
//!   sampled frames at least that far apart, it is a twin, and the move
//!   melts instead of following. MEASURED at the host seam on tears of
//!   8–300 ms (`tests/moves_are_followed.rs`, the `the_limit_…` laws): a
//!   one-row relocation, a two-row one onto the caret's row, and an unsynced
//!   composer's wrap melt from the first sampled frame 40 ms after the one
//!   that first showed the new row — a 40 ms tear on 8 ms frames (38 ms
//!   follows), 48 ms on 16 ms frames and under the GUI's pacing with or
//!   without the pet (46 ms follows); but after a 200 ms pause, when the
//!   no-pet style's idle frames come 110–130 ms apart and the copy is seen
//!   on one frame only, not until 120 ms. A relocation that comes WITH a
//!   scroll is dated from the last frame before the scroll, so it melts
//!   sooner: from a 32 ms tear on 16 ms frames and under the pet's pacing,
//!   48 ms on 8 ms frames. A relocation two rows away onto a row no band
//!   names is never sampled between and always follows. 0.93.0 followed
//!   every tear, and every copy that stood beside the line with it. What
//!   the trade costs is bounded by who tears: Claude Code and Codex bracket
//!   their frames in `CSI ?2026h … ?2026l` in a terminal that answers their
//!   DECRQM 2026 query, as aterm does (953 brackets in the eight recordings
//!   of `tests/fixtures/*.ptylog`; `docs/measured/claude-code-composer-*`),
//!   and a bracketed frame is presented whole, so their moves never tear;
//!   and the torn reads measured live — a redraw over 1 KiB presented
//!   between two PTY reads, above — last 2 to 30 ms, under the limit.
//!
//! **THE LIMIT.** A line identical to the one above it — or differing from
//! it in one column (`step 2:` under `step 1:`) — that genuinely moves up
//! onto it cannot be told, glyph for glyph, from the same line erased under
//! an identical one: nothing at the destination arrived that was not
//! already standing there, or one glyph did, and one is too little
//! evidence. Both are refused, and such a line's band melts on its row
//! instead of following (`tests/composer_multiline_wrap.rs` states it as a
//! law). A NEAR copy a move carries onto the line's own row leaves fewer
//! than half of its glyphs gone, and the line reads as rewritten in place
//! (`tag it` under `log it` in `tests/moves_are_followed.rs`'s tall
//! prompt). And the twin one row off does not stop the search two rows off
//! (`a_twin_testifies_only_about_its_own_row`): where the row two away is
//! sampled and a copy of the line arrived on it, the band is carried there.
//!
//! D2 holds by construction: the host samples AFTER the PTY batch is
//! applied, so an erase followed by the same text at the same cells inside
//! one batch (every zsh prompt redraw, every Ctrl-L) is seen as the same
//! glyph and touches nothing. Witness records form a fixed-capacity, sorted,
//! resident list ([`WITNESS_CAP`]); ordinary record lookups cost
//! `O(cells · log entries)`. Retirement membership uses sorted scratch, and
//! moved-text checks have their own bound below. The walk allocates nothing
//! past warm-up and runs only while rainbow kitty owns the frame.
//!
//! **It composes with the echo ledger** (`Engine::echo_bridge`, 2026-09-10)
//! by never touching it: a retirement is a clock on a cell already laid,
//! and the ledger is about presses whose cells are not laid yet. A ledger
//! payout lays fresh cells (a new birth) only where no live cell owns the
//! column — a retired cell is [`super::ribbon::Cell::leaving`] and owns
//! nothing — so a payout can lay over a retired cell but never revive it,
//! and the fresh cell arms its own record on the next walk.

use aterm_time::{Duration, Instant};

use crate::cursor_glow::band_row;

use super::ribbon::Cell;

/// Witness records the engine keeps: one per resident ribbon cell, at most
/// this many. Past the cap a cell is simply not witnessed (it keeps its own
/// clocks); the host's widest band at 177 columns across three rows is
/// under it.
pub const WITNESS_CAP: usize = 1024;

/// Rows the BANDS request per frame for the witness
/// ([`super::Engine::ribbon_rows_for`] names them): the rows the resident
/// ribbon occupies and their possible follow destinations. A wrapped
/// paragraph is three; eight band rows are headroom, not a target. A waiting
/// key's arming rows have [`ARMING_ROWS`] slots of their own after these, and
/// the host captures the caret's row in one more
/// ([`crate::cursor_glow::CURSOR_WITNESS_ROWS`]).
pub const WITNESS_ROWS: usize = 8;

/// The slots a WAITING KEY's arming rows take after the bands'
/// [`WITNESS_ROWS`] ([`super::Engine::ribbon_rows_for`]): the row its cell
/// will be laid on, and the rows one above and below it, where the bands'
/// list does not name them already.
pub const ARMING_ROWS: usize = 3;

/// One row of the live grid as the host sampled it this frame — the row
/// probe's own per-column convention (`Terminal::row_cols_into`).
#[derive(Clone, Copy, Debug)]
pub struct RowSample<'a> {
    /// The grid row.
    pub row: u16,
    /// Per-column chars: the lead glyph at its column, `'\0'` at a wide
    /// continuation, `' '` for a blank. Shorter than the grid means the
    /// missing tail is blank.
    pub cols: &'a [char],
}

/// What one column of a sampled row holds, resolved to the GLYPH UNIT it
/// belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    /// The glyph — the lead char for either half of a wide glyph; `' '` for
    /// a blank.
    pub ch: char,
    /// The glyph occupies two cells.
    pub wide: bool,
    /// This column is the CONTINUATION half of a wide glyph (its lead is one
    /// column left).
    pub cont: bool,
}

impl Unit {
    /// A blank column.
    pub const BLANK: Self = Self {
        ch: ' ',
        wide: false,
        cont: false,
    };

    /// True for a column holding no glyph.
    #[must_use]
    pub fn is_blank(self) -> bool {
        self.ch == ' '
    }
}

/// The glyph unit at `col` of a sampled row. A column past the sample's end
/// is blank; a `'\0'` continuation resolves to the lead one column left (a
/// `'\0'` at column 0, or one whose lead is itself blank or `'\0'`, is a
/// degenerate sample and reads as blank).
#[must_use]
pub fn unit_at(cols: &[char], col: u16) -> Unit {
    let i = usize::from(col);
    match cols.get(i) {
        None => Unit::BLANK,
        Some('\0') => {
            let lead = i
                .checked_sub(1)
                .and_then(|j| cols.get(j))
                .copied()
                .unwrap_or(' ');
            if lead == '\0' || lead == ' ' {
                Unit::BLANK
            } else {
                Unit {
                    ch: lead,
                    wide: true,
                    cont: true,
                }
            }
        }
        Some(&c) => Unit {
            ch: c,
            wide: cols.get(i + 1) == Some(&'\0'),
            cont: false,
        },
    }
}

/// One cell of a run as the shift pass reads it: `(col, recorded, gone)`
/// ([`Witness::shift_runs`]).
type ShiftCell = (u16, Option<Unit>, bool);

/// [`Witness::shift_runs`]'s verdict for ONE run, in column order: the
/// `(lo, hi, dc)` of the block an insert pushed right along the row, or
/// `None`.
/// See that method for the law; this is its arithmetic.
fn find_shift(cells: &[ShiftCell], cols: &[char], caret: Option<u16>) -> Option<(u16, u16, i16)> {
    let s = cells.iter().position(|c| c.2)?;
    let (scol, sunit, _) = cells[s];
    let sunit = sunit?;
    let width = u16::try_from(cols.len()).unwrap_or(u16::MAX);
    // AN INSERT: `s`'s own glyph stands `dc` columns right of it, nearest
    // first — the narrowest insert that explains the row.
    let reach = width.min(scol.saturating_add(SHIFT_MAX_COLS));
    for x in scol.saturating_add(1)..reach {
        if unit_at(cols, x) != sunit {
            continue;
        }
        let Ok(dc) = i16::try_from(x - scol) else {
            break;
        };
        if let Some(v) = shift_block(cells, cols, s, dc, caret) {
            return Some(v);
        }
    }
    None
}

/// Verify one candidate shift for [`find_shift`]: every cell from index `b`
/// on stands at `col + dc` under its own glyph (a never-armed cell under a
/// blank) as one run from `b`, and past the first cell that does not, every
/// recorded glyph has left the row. Then extend the block left over the
/// cells the shift also explains, snap the edit point to the caret when it
/// stands inside that extension, and weigh the evidence. Returns
/// `(lo, hi, dc)` in columns.
fn shift_block(
    cells: &[ShiftCell],
    cols: &[char],
    b: usize,
    dc: i16,
    caret: Option<u16>,
) -> Option<(u16, u16, i16)> {
    let at = |col: u16| {
        col.checked_add_signed(dc)
            .map_or(Unit::BLANK, |c| unit_at(cols, c))
    };
    let mut found = 0usize;
    let mut tail = false;
    let mut hi = None;
    // Whether any found glyph's OWN column holds other text now. An insert
    // overwrites every column from its edit point on (with the new text,
    // then the pushed tail); a block whose every column went BLANK is text
    // that was cleared, and a copy of it further along the row proves
    // nothing about where it went.
    let mut overwritten = false;
    for (i, &(col, rec, _)) in cells.iter().enumerate().skip(b) {
        let there = at(col);
        match rec {
            Some(unit) => {
                if !tail && there == unit {
                    found += 1;
                    hi = Some(i);
                    overwritten |= !unit_at(cols, col).is_blank();
                } else if there.is_blank() {
                    tail = true;
                } else {
                    // A glyph of the tail standing on OTHER text: a
                    // rewrite, not a move.
                    return None;
                }
            }
            None => {
                let on_row = col
                    .checked_add_signed(dc)
                    .is_some_and(|c| usize::from(c) < cols.len());
                if !tail && on_row && there.is_blank() {
                    hi = Some(i);
                } else {
                    tail = true;
                }
            }
        }
    }
    let hi = hi?;
    if found == 0 || !overwritten {
        return None;
    }
    // THE EDIT POINT: left over every contiguous cell the shift explains.
    let mut lo = b;
    while lo > 0 {
        let (pcol, prec, _) = cells[lo - 1];
        if pcol.checked_add(1) != Some(cells[lo].0) {
            break;
        }
        let there = at(pcol);
        let explained = match prec {
            Some(unit) => there == unit,
            None => there.is_blank(),
        };
        if !explained {
            break;
        }
        lo -= 1;
    }
    // …and a hand typing at the caret inserts THERE, whatever letters
    // repeat around it.
    let keyed = caret.and_then(|c| (lo..=b).find(|&i| cells[i].0 == c));
    let lo = keyed.unwrap_or(lo);
    // Two found glyphs are a block. One is a block only at the caret, only
    // whole, and only at the END of the line: a single letter found with
    // the rest of the run gone is a rewrite that happens to share it, and
    // one found with more text standing past it is a letter the line
    // already held further on (a Backspace pulling `also see` together
    // lands the caret's `s` three columns short of the `s` of `see`).
    if found < 2 {
        let end = cells[hi]
            .0
            .checked_add_signed(dc)
            .map_or(usize::MAX, usize::from);
        let text_past = cols
            .iter()
            .skip(end.saturating_add(1))
            .any(|&c| c != ' ' && c != '\0');
        if keyed.is_none() || tail || text_past {
            return None;
        }
    }
    Some((cells[lo].0, cells[hi].0, dc))
}

/// One witnessed cell — keyed by the CELL, `(row, col, born)`, not by the
/// position: two cells can be resident at one position with different
/// births, and each carries its own record.
#[derive(Clone, Copy, Debug)]
struct Seen {
    row: u16,
    col: u16,
    /// The birth of the cell this record is about, the third part of the
    /// key: a cell re-laid at the same column is a different cell and the
    /// record does not carry over; a stale cell still resident under a
    /// fresh one is named for retirement by this, so the fresh one is
    /// never taken with it (`retire_cells` stamps by identity — a stamp by
    /// position took a key's own cell with the stale one on its birth
    /// frame).
    born: Instant,
    /// The glyph unit the cell was first seen holding.
    unit: Unit,
    /// Set by the walk for every record whose cell is still resident; a
    /// record the walk did not reach is a cell the ribbon dropped.
    live: bool,
    /// Set once the cell's run was RELEASED — its glyph went and nothing
    /// replaced it — and kept while the cell is resident: it is unstamped
    /// and owns its column for the retract's span, so without a record the
    /// next walk would arm it over whatever lands there. A released record
    /// says one thing more: a glyph landing under the cell is REPLACED text
    /// (the melt, with the cell's run); a blank is nothing; the same glyph
    /// back is D2. It is never released, searched for or counted again.
    released: bool,
    /// The original identity of a released run, eligible for exact redraw
    /// restoration. A freshly armed/retyped identity never inherits this.
    restorable: bool,
    /// Retirement accounting survives a successful redraw restoration.
    counted: bool,
    /// **NOTHING SAYS A GLYPH FOUND THERE DID NOT ARRIVE** (2026-09-25; the
    /// module doc's evidence model). One bit per follow offset
    /// of [`FOLLOW_DRS`]: the positive half of an arrival — the follow pass
    /// counts a glyph found at that offset as ARRIVED only where this bit is
    /// set and [`Seen::twin`] is not ([`Seen::arrivals`]). Set for EVERY
    /// offset when the record is armed: what the witness did not see is not
    /// evidence against a move (0.93.0's reading; a fail-closed reading, one
    /// that refused every move whose destination had not been seen empty
    /// first, misses moves two frames apart, a paste then a wrap, a torn
    /// repaint). A BAND MOVE clears
    /// the offsets that reach across the band's edge
    /// ([`Witness::translate_band`]): the
    /// row there is one the record was never seen beside — vim's `yyp`
    /// opens a line under the one just typed and writes the copy into it —
    /// and a sample that shows it holding something else sets the bit again.
    clear: u8,
    /// **A COPY STOOD BESIDE IT** (2026-09-22, the twin; accumulated, by
    /// time, since 2026-09-25): one bit per follow offset, STICKY, set once
    /// the row that far away was sampled holding THIS unit at THIS column
    /// while the record's own glyph stood — at once on the arming frame (the
    /// glyph was there before the record's own was witnessed: `$ cd ..`
    /// typed under `$ cd ..`); otherwise once the copy has stood there, seen
    /// on every sampled frame between, for [`TWIN_MIN`] ([`Seen::pending`])
    /// — on a row a scroll brought in, counted from the last walk before the
    /// scroll ([`Witness::translate`]), so the first look there can already
    /// make it a twin. Finding the glyph there later is not
    /// evidence that the text MOVED, nor, since it stands where the moved
    /// text would put it, evidence against the move: the follow pass counts
    /// it neither way (2026-09-24).
    twin: u8,
    /// A copy seen beside the standing record after it was armed, not yet
    /// for [`TWIN_MIN`] — a twin in the making, first seen [`Seen::pend_ms`]
    /// into the cell's life. On a row a scroll brought in the copy came in
    /// with the row: [`Witness::translate`] sets this bit and starts the
    /// clock at [`Witness::last_walk`], the last walk before the scroll, and
    /// a row never looked at since stays clear. A sampled frame with
    /// anything else there resets it. A short stand is not enough: a program
    /// that repaints without a synchronized-update bracket can draw the
    /// text's new row before it erases the old (a TORN REPAINT), for as long
    /// as it takes to write the rest, and that move must still follow.
    pending: u8,
    /// When each [`Seen::pending`] copy was first seen, in ms since the
    /// record's cell was born (saturating).
    pend_ms: [u32; 4],
    /// **A VERDICT DEFERRED ONCE** (2026-09-25): the record's glyph
    /// changed on a walk where a row beside its run was asked for but not
    /// delivered, and the walk passed no verdict on the run
    /// ([`Witness::walk`]'s `withheld`). A second change while this is set
    /// is judged, so a host that never delivers the row cannot keep light
    /// under the wrong text. Cleared when the glyph is seen back or the
    /// record is carried.
    deferred: bool,
}

/// **HOW LONG A COPY MUST STAND BESIDE A LINE TO BE ITS TWIN** (2026-09-25):
/// 40 ms, measured, seen on every sampled frame between ([`Seen::pending`]).
/// The unit is WALL TIME, not frames: frames come 8-16 ms apart while a
/// key's light is live and 110-130 ms apart once the no-pet style is idle,
/// so two frames is 16 ms on one host and a quarter of a second on the
/// next. The floor is a TORN REPAINT — a program with no synchronized-update
/// bracket drawing the text's new row before it erases the old, presented on
/// up to three 60 Hz frames (32 ms; `tests/moved_again_without_a_key.rs`) —
/// which follows when the copy's clock starts at its first look; the
/// ceiling is a copy that stood beside the line for three frames before it
/// was erased (48 ms; `tests/copy_beside_the_line.rs`), which must not. A
/// copy on a row a SCROLL brought in is clocked from the last walk before
/// the scroll ([`Witness::translate`]), a frame earlier than its first look,
/// so a torn relocation that comes with a scroll melts from a 32 ms tear on
/// 16 ms frames and under the pet's pacing (48 ms on 8 ms frames).
/// `a_copy_that_appears_beside_a_standing_line_is_a_twin_once_it_has_stood_forty_ms`
/// pins both ends, and `tests/moves_are_followed.rs`'s `the_limit_…` laws
/// and `a_line_relocated_by_a_scroll_follows_until_its_new_row_has_stood_twin_min`
/// the tear lengths the host reads past it.
pub const TWIN_MIN: Duration = Duration::from_millis(40);

/// The follow pass's row offsets, nearest first: one row up, one row down,
/// then two. Bit `k` of [`Seen::clear`], [`Seen::twin`] and
/// [`Seen::pending`] is `FOLLOW_DRS[k]`, and the rows one away from a
/// waiting key's row are among the ones the host samples first
/// (`Engine::ribbon_rows_for`).
pub(super) const FOLLOW_DRS: [i16; 4] = [-1, 1, -2, 2];

/// The sampled rows at every follow offset from `row`, in [`FOLLOW_DRS`]
/// order — `None` where that row was not sampled this frame (or does not
/// exist). Four scans of at most [`WITNESS_ROWS`] samples; the walk reuses
/// the answer for every cell on the same row.
fn neighbours_of<'a>(rows: &[RowSample<'a>], row: u16) -> [Option<&'a [char]>; 4] {
    let mut out = [None; 4];
    for (slot, dr) in out.iter_mut().zip(FOLLOW_DRS) {
        *slot = row
            .checked_add_signed(dr)
            .and_then(|r| rows.iter().find(|s| s.row == r))
            .map(|s| s.cols);
    }
    out
}

/// The follow offsets whose evidence survives a ROW BAND move
/// ([`Witness::translate_band`]) for a record carried from `old` to `new`:
/// offset `dr` keeps its bits only where the neighbour's content moved WITH
/// the record — the row that stood at `old + dr` is at `new + dr` after the
/// move. A neighbour outside the band stood still while the record moved
/// (or moved while the record stood still, outside the band), and a row the
/// band carried in or dropped is not the one that was seen: those offsets
/// start over, NOT clear ([`Seen::clear`]) until a sample shows them.
fn band_keeps(old: u16, new: u16, top: u16, bottom: u16, delta: i16) -> u8 {
    let mut keep = 0u8;
    for (k, dr) in FOLLOW_DRS.into_iter().enumerate() {
        if let (Some(was), Some(now)) = (old.checked_add_signed(dr), new.checked_add_signed(dr))
            && band_row(was, top, bottom, delta) == Some(now)
        {
            keep |= 1 << k;
        }
    }
    keep
}

impl Seen {
    /// **ONE FRAME'S EVIDENCE** (2026-09-25): the record's own glyph STANDS
    /// on its row this frame, at `now`, and `nb` are the rows sampled at its
    /// follow offsets ([`neighbours_of`]). For each sampled one: a different
    /// unit at the
    /// record's column sets [`Seen::clear`] and resets [`Seen::pending`];
    /// the same unit sets [`Seen::twin`] at once when the record is being
    /// ARMED (`at_arm` — the copy was there first), and otherwise once the
    /// copy has stood there [`TWIN_MIN`] since it was first seen
    /// ([`Seen::pending`]) — on a row a scroll brought in, since the last
    /// walk before the scroll ([`Witness::translate`]), so the first look
    /// there can already find it a twin. An unsampled row changes nothing.
    /// A blank record testifies nothing.
    fn observe(&mut self, nb: &[Option<&[char]>; 4], at_arm: bool, now: Instant) {
        if self.unit.is_blank() {
            return;
        }
        let age =
            u32::try_from(now.saturating_duration_since(self.born).as_millis()).unwrap_or(u32::MAX);
        for (k, cols) in nb.iter().enumerate() {
            let Some(cols) = cols else {
                continue;
            };
            let bit = 1u8 << k;
            if unit_at(cols, self.col) != self.unit {
                self.clear |= bit;
                self.pending &= !bit;
            } else if at_arm {
                self.twin |= bit;
            } else if self.pending & bit == 0 {
                self.pending |= bit;
                self.pend_ms[k] = age;
            } else if u128::from(age.saturating_sub(self.pend_ms[k])) >= TWIN_MIN.as_millis() {
                self.twin |= bit;
            }
        }
    }

    /// Start over as a record just armed where nothing was seen: every
    /// offset clear, no twin — the record now stands on a row (or claims a
    /// glyph) its earlier observations were not about, and the frames that
    /// follow learn its new neighbours ([`Seen::observe`]).
    fn forget_evidence(&mut self) {
        self.clear = ALL_OFFSETS;
        self.twin = 0;
        self.pending = 0;
    }

    /// The follow offsets at which a glyph found would have ARRIVED: not
    /// known to have been there before ([`Seen::clear`]), and never a twin
    /// there. A twin testifies about its own row and no other: a copy one
    /// row away says nothing about the row two away in the same direction
    /// (`the_evidence_is_what_each_follow_row_was_seen_holding`,
    /// `a_twin_testifies_only_about_its_own_row`).
    fn arrivals(&self) -> u8 {
        self.clear & !self.twin & ALL_OFFSETS
    }
}

/// Every follow offset's bit ([`FOLLOW_DRS`]).
const ALL_OFFSETS: u8 = 0x0F;

/// A cell whose recorded glyph went BLANK this walk — provisional until its
/// run has decided the clock ([`Witness::walk`]).
#[derive(Clone, Copy, Debug)]
struct Blanked {
    row: u16,
    col: u16,
    born: Instant,
    cohort: u32,
    /// The glyph unit the cell was recorded holding — what the moved-text
    /// search looks for elsewhere.
    unit: Unit,
    /// The record was already released by an earlier walk and the cell is
    /// still under a blank: HELD until its run decides — it melts with a
    /// run-mate that was replaced this walk (a re-verdict, not counted
    /// again) and is otherwise left to the retract it is on. Its glyph is
    /// long gone, so it is never searched for.
    held: bool,
    counted: bool,
}

/// One armed record on a row beside a withheld one
/// ([`Witness::find_deferred_runs`]).
#[derive(Clone, Copy, Debug)]
struct BlindRecord {
    row: u16,
    cohort: u32,
    col: u16,
    /// Its glyph no longer stands under it.
    changed: bool,
    /// …and its verdict was deferred once already ([`Seen::deferred`]).
    deferred: bool,
}

#[derive(Clone, Copy, Debug)]
struct RestoreRun {
    row: u16,
    cohort: u32,
    /// The release the run's cells carry ([`Cell::released_at`]): one
    /// partial release's stamp, or `None` for the cohort's unstamped cells.
    stamp: Option<Instant>,
    exact: bool,
    returned_ink: bool,
}

/// The witness: the sorted `(row, col, born)` list of cells seen holding a
/// glyph.
#[derive(Clone, Debug, Default)]
pub struct Witness {
    /// Sorted by `(row, col, born)`, one record per cell, resident and
    /// reused.
    seen: Vec<Seen>,
    /// `(row, cohort)` of every run a walk RETIRED a cell of — a glyph
    /// replaced, or the run's blanked text found elsewhere — the runs whose
    /// blanked and never-armed cells take the fast melt. Resident scratch,
    /// cleared per walk.
    retired_runs: Vec<(u16, u32)>,
    /// Sorted `(row, cohort)` identities of retired runs that still have an
    /// armed, non-retired cell. Such runs keep their unchanged spaces;
    /// retired runs absent from this list take their spaces with them.
    /// Resident scratch, cleared per walk.
    standing_runs: Vec<(u16, u32)>,
    /// Sorted copy of this walk's retirement identities. The public verdict
    /// keeps its original order; this scratch makes membership logarithmic
    /// while finding surviving runs. Capacity is reused after warm-up.
    retired_cells: Vec<(u16, u16, Instant)>,
    /// `(row, cohort)` of every run a walk RELEASED — blanked, and its text
    /// nowhere the witness can see — the runs whose never-armed cells go to
    /// the swoosh with them when the run's text went WHOLE. Resident
    /// scratch, cleared per walk.
    released_runs: Vec<(u16, u32)>,
    /// Sorted `(row, cohort, col)` of every recorded glyph STILL STANDING
    /// this walk — a record that is not released and whose glyph is under
    /// it now. A released run with a cell on this list lost only PART of
    /// its text, and keeps its never-armed cells outside the span it lost
    /// and short of a glyph still standing on their side
    /// ([`Witness::released_span_takes`], 2026-09-21). Resident scratch,
    /// cleared per walk.
    standing_glyphs: Vec<(u16, u32, u16)>,
    /// The runs the shape pass reads this walk: every run something was
    /// named in, retired or released, each once.
    shape_runs: Vec<(u16, u32)>,
    /// The ids of this walk's fresh releases, marked on their records only
    /// once the shape pass has let them stand.
    fresh_released: Vec<(u16, u16, Instant)>,
    /// The ids this walk named whose records were already counted — the
    /// re-verdicts, tallied against the names that survive the shape pass.
    counted_names: Vec<(u16, u16, Instant)>,
    /// One run's named ids, for the shape pass's retains.
    run_ids: Vec<(u16, u16, Instant)>,
    /// The cells whose glyph went blank this walk, sorted by `(row, cohort,
    /// col)` once the pass is over. Resident scratch, cleared per walk.
    blanked: Vec<Blanked>,
    restore_runs: Vec<RestoreRun>,
    /// The follow pass's runs — every `(row, cohort)` with a resident,
    /// non-leaving cell — sorted and deduped. Resident scratch.
    follow_runs: Vec<(u16, u32)>,
    /// One run's ARMED cells for the follow pass: `(col, unit, gone,
    /// arrivals)` in column order, `gone` when the recorded glyph no longer
    /// stands at the cell, `arrivals` the offsets at which a glyph found
    /// would have ARRIVED ([`Seen::arrivals`]). Resident scratch.
    follow_armed: Vec<(u16, Unit, bool, u8)>,
    /// **THE CELLS THE LAST WALK READ OVER A BLANK** (2026-09-24), sorted
    /// `(row, col, born)`: resident, not leaving, and holding no record —
    /// never armed, because nothing was ever under them (a typed Space). The
    /// one thing that tells a glyph LANDING on an old blank this walk from a
    /// key's own cell first read this walk ([`Witness::walk`]'s landed
    /// arm). Rebuilt by every walk; carried with its cells by a scroll, a
    /// band move and the follow pass. Resident.
    blank_seen: Vec<(u16, u16, Instant)>,
    /// The next walk's [`Witness::blank_seen`], built by this one.
    /// Resident scratch.
    blank_next: Vec<(u16, u16, Instant)>,
    /// This walk's cells from [`Witness::blank_seen`] with a glyph under
    /// them now, `(row, col, born, cohort)`: armed on the walk's first pass
    /// as any blank → glyph is, and judged with their run once its verdict
    /// is in. Resident scratch.
    landed: Vec<(u16, u16, Instant, u32)>,
    /// Every armed record on a row beside a withheld one this walk
    /// ([`Witness::find_deferred_runs`]) — at most one per record. Resident
    /// scratch, reserved at [`WITNESS_CAP`] by [`Witness::new`].
    blind_changed: Vec<BlindRecord>,
    /// The runs this walk passes no verdict on, sorted: a row beside them
    /// was withheld ([`Witness::find_deferred_runs`]). Resident scratch,
    /// reserved at [`WITNESS_CAP`] by [`Witness::new`].
    deferred_runs: Vec<(u16, u32)>,
    /// When the last walk ran — the frame a scroll that arrives before the
    /// next one dates the copies on the rows it brings in from: it starts
    /// their [`Seen::pending`] clocks here ([`Witness::translate`]). `None`
    /// until the first walk.
    last_walk: Option<Instant>,
    /// The shift pass's view of one run ([`Witness::shift_runs`]): every
    /// resident, non-leaving cell as `(col, recorded, gone)` in column
    /// order — `recorded` the live record's unit, `None` for a never-armed
    /// cell (the space in `hello world`), `gone` when the recorded glyph no
    /// longer stands at the cell. Resident scratch.
    shift_cells: Vec<(u16, Option<Unit>, bool)>,
}

/// **A RUN THE SHIFT PASS FOUND ALONG ITS OWN ROW** ([`Witness::shift_runs`],
/// 2026-09-24): the cells of `cohort` on `row` within `lo..=hi` stand under
/// their own glyphs `dc` columns RIGHT on the SAME row — the tail a
/// mid-line insert of `dc` cells pushed along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShiftRun {
    /// The run's row.
    pub row: u16,
    /// The run's cohort.
    pub cohort: u32,
    /// The leftmost column that moves — the insert's edit point: the new
    /// text stands on `lo..lo + dc`.
    pub lo: u16,
    /// The rightmost column that moves.
    pub hi: u16,
    /// Columns to the text: the inserted width, always positive.
    pub dc: i16,
}

/// The widest same-row shift the shift pass searches for: a paste of a
/// full wide line. Past it the tail is left to the walk (the fast melt, as
/// before). A bound on the SEARCH, not on the evidence.
pub const SHIFT_MAX_COLS: u16 = 512;

/// **A RUN THE FOLLOW PASS FOUND ELSEWHERE** ([`Witness::follow_runs`]):
/// the cells of `cohort` on `row` within `lo..=hi` stand under their own
/// glyphs `dr` rows away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowRun {
    /// The run's row now.
    pub row: u16,
    /// The run's cohort.
    pub cohort: u32,
    /// The leftmost column carried: the found block's, grown over its
    /// neutral and never-armed border ([`Witness::follow_runs`]).
    pub lo: u16,
    /// The rightmost column carried, grown the same way.
    pub hi: u16,
    /// Rows to the text: `−1` one row up, `+1` one row down, then `±2`.
    pub dr: i16,
}

/// **WHAT A FOLLOW PASS SAW BESIDE WHAT IT NAMED** ([`Witness::follow_runs`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FollowScore {
    /// **THE MISSED FOLLOWS** (2026-09-23): armed cells whose glyphs are
    /// GONE from their row in a run whose text ARRIVED on a neighbouring
    /// sampled row — two or more records, and no fewer than half of those
    /// neither neutral nor relaid by a wrap, found at their own columns
    /// (the half-gate of [`Witness::follow_runs`]) — that the pass did NOT
    /// name, because the arrivals were not one block. Such a run is the
    /// content witness's to judge next, where it stands: it melts (or, with
    /// glyphs of it still standing, is released) — the abrupt vanish this
    /// counter exists to make visible (`ribbon_follow_missed=`). A
    /// true re-layout (other text a row up), an erase under an identical
    /// line (every record a twin) and a kill under a line sharing only a
    /// prefix (the rest holes) arrive nothing and count nothing.
    pub missed: u32,
}

/// One offset's reading of a run ([`Witness::follow_verdict`]).
#[derive(Clone, Copy, Debug)]
struct FollowVerdict {
    /// Records FOUND: arrived at their own column.
    found: usize,
    /// Records NEUTRAL: their twin stands there, as it did at arm time.
    neutral: usize,
    /// HOLES among the run's relaid suffix ([`Witness::relaid_suffix`]):
    /// records the wrap took ELSEWHERE, out of the half-gate's denominator.
    relaid: usize,
    /// The block's extent, riders included.
    lo: u16,
    hi: u16,
    /// No found record after a hole that followed a found one.
    block: bool,
}

impl Witness {
    /// An empty witness with its capacity reserved once ([`WITNESS_CAP`]):
    /// the records, and the deferral's scratch, which holds at most one
    /// entry per record — a frame with a withheld row can come long after
    /// warm-up, and must not allocate either.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seen: Vec::with_capacity(WITNESS_CAP),
            retired_runs: Vec::new(),
            standing_runs: Vec::new(),
            retired_cells: Vec::new(),
            released_runs: Vec::new(),
            standing_glyphs: Vec::new(),
            shape_runs: Vec::new(),
            fresh_released: Vec::new(),
            counted_names: Vec::new(),
            run_ids: Vec::new(),
            blanked: Vec::new(),
            restore_runs: Vec::new(),
            follow_runs: Vec::new(),
            follow_armed: Vec::new(),
            blank_seen: Vec::new(),
            blank_next: Vec::new(),
            landed: Vec::new(),
            blind_changed: Vec::with_capacity(WITNESS_CAP),
            deferred_runs: Vec::with_capacity(WITNESS_CAP),
            last_walk: None,
            shift_cells: Vec::new(),
        }
    }

    /// **THE FOLLOW PASS'S SEARCH** (2026-09-21, the band follows its text;
    /// see the module doc). For every run `(row, cohort)` with a resident
    /// cell on a row sampled this frame, read its ARMED records — live,
    /// not released, on cells that are not leaving — in column order, and
    /// name the run on `out` when the text moved vertically AS A BLOCK:
    ///
    /// * at least two armed records, and at least two of them — and no
    ///   fewer than half — are GONE from their own row (replaced or blank).
    ///   A run whose glyphs still stand where they were is not followed,
    ///   however many copies of its text the screen holds (identical text
    ///   on two rows translates nothing); a run with a single armed glyph
    ///   is too little evidence for a move;
    /// * on the nearest sampled row `row + dr`, `dr ∈ {−1, +1, −2, +2}`,
    ///   at least two records are FOUND — standing at THEIR OWN COLUMN with
    ///   THEIR OWN unit, and ARRIVED there ([`Seen::arrivals`]): not a copy
    ///   of itself that stood there before ([`Seen::twin`]), nor on a row a
    ///   band move carried beside it unseen since ([`Seen::clear`]) — and no
    ///   fewer than half of the records that are neither NEUTRAL nor a hole
    ///   the wrap RELAID ([`Witness::follow_verdict`],
    ///   [`Witness::relaid_suffix`]). An erase is not a move: a line killed
    ///   under an identical line (`$ cd ..` typed below `$ cd ..`, then
    ///   Ctrl-U) has its glyphs gone and a block of them standing one row
    ///   up, but that block was there before any of them was typed —
    ///   carrying the band onto it lit a previous command no key wrote for
    ///   1.3 s (2026-09-22 review) — so a run of nothing but twins names
    ///   nothing. Nor is a copy that APPEARED beside the line while it stood
    ///   — fzf's list landing after the query's first keys, a completion
    ///   popup — once it has stood there [`TWIN_MIN`] (2026-09-25). The
    ///   found records are one block — a prefix or a suffix of the run (the
    ///   word a composer's wrap moved down off the row's end), never letters
    ///   either side of a HOLE: a rewrite that keeps some letters by
    ///   coincidence is not a block that moved. The first `dr` that
    ///   qualifies wins; a row whose glyphs stand as the run's twins is
    ///   refused and the search goes on (**A TWIN STOPS NOTHING**, 2026-09-25:
    ///   a twin testifies about its own row and no other, so a copy that
    ///   ARRIVED two rows away is carried onto —
    ///   `a_twin_testifies_only_about_its_own_row`).
    ///
    /// **A GLYPH FOUND WITHOUT EVIDENCE OF ARRIVAL IS NEUTRAL** (2026-09-23 —
    /// the owner: *"the existing line rainbow should beautifully flow and
    /// drift and fade away, not simply abruptly vanish"*). The 2026-09-22
    /// rule counted a twin whose glyph stands at the target as NOT FOUND,
    /// and one such record between two found ones broke the block. In a
    /// composer the row above a growing line is the previous line of the
    /// same paragraph, and a glyph under the same glyph at the same column
    /// is ordinary English — the owner's own capture puts `WHY` under
    /// `HEY`, the very `Y` Ink's diff reused. So every growth of the box
    /// after the first refused the follow and the content witness melted
    /// the whole row in `RETIRE_MELT_S`: measured at the host seam
    /// (`tests/composer_growth_follows.rs`, the owner's text at 90 columns)
    /// wraps 2 and 3 followed `+0` and retired `+86`, the row gone at
    /// 140 ms, where the same text with no twin followed `+77` and flowed
    /// for 1.3 s; one changed letter flips it. On glass the lead's take read
    /// `ribbon_followed=54 ribbon_retired=58`. A glyph whose arrival there
    /// the witness has no evidence of proves nothing EITHER way: it is
    /// neither found nor a hole, it leaves the half-gate's denominator, and
    /// it rides with the block — inside it always, and at its edges FLUSH
    /// against it (a lead directly before its first found record, a trail
    /// directly after its last) whether or not its own glyph still reads as
    /// standing on the run's row: in a bottom-anchored composer that glyph
    /// can be the NEW line's first letter, re-laid there by the wrap (the
    /// review of this rule, 2026-09-23 — see [`Witness::follow_verdict`]).
    /// What stays evidence is unchanged: every found record is a glyph that
    /// ARRIVED, a record whose glyph is missing at the target is a hole, and
    /// the gone gate above is the run's own row.
    ///
    /// **THE WORD THE WRAP RELAID IS NOT A HOLE** (2026-09-23, the review of
    /// the half-gate). A composer's wrap carries the line a row up MINUS its
    /// last word, which it lays again at the start of the caret row — the
    /// run's own row. Those records are holes at the target only because
    /// the wrap took them elsewhere, and counted against the line they
    /// refused the follow wherever the moved word outnumbered the line's
    /// arrivals: at 20 columns the owner's `this` (4) under a moved
    /// `confirmatio` (11), at 60 `see` before a 52-letter path — each melted
    /// on the old row in ~0.13 s with `ribbon_follow_missed=` reading `0`.
    /// A trailing suffix whose glyphs stand on the run's own row in order,
    /// at their spacing, the first at the row's first glyph column
    /// ([`Witness::relaid_suffix`]), leaves the denominator where it is a
    /// hole: the half-gate is `2·found ≥ armed − neutral − relaid`. Its
    /// holes are only ever SUBTRACTED — never found, never part of a block
    /// — so a run of nothing but twins or holes still names nothing (the
    /// Ctrl-U laws have `found = 0`).
    ///
    /// `lo..=hi` is the found block's extent, riders included, then GROWN
    /// over a bordering never-armed cell of the run while no record beyond
    /// it on that side was left behind (2026-09-24) — the Space typed at the
    /// end of a full line before the glyph that wraps writes no glyph and
    /// arms no record, and was left on the continuation row. The run's
    /// never-armed blanks inside the extent go with it
    /// (`Ribbon::translate_run`). Returns what the pass SAW beside what it
    /// named ([`FollowScore`]). Pure in the records and the samples,
    /// `O(runs × cells × 4)` to search plus `O(width × cells)` once for a
    /// run that qualifies (an event), and allocation-free past warm-up: the
    /// scratch is resident and `out` is the caller's.
    pub fn follow_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        out: &mut Vec<FollowRun>,
    ) -> FollowScore {
        self.follow_runs_with_short_park(cells, rows, None, out)
    }

    /// A sole arriving glyph is only evidence for the exact short held park
    /// whose source row the host is judging. Ordinary copy and twin searches
    /// keep their two-arrival floor.
    pub(crate) fn follow_runs_with_short_park(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        short_park: Option<((u16, u16), (u16, u16))>,
        out: &mut Vec<FollowRun>,
    ) -> FollowScore {
        let mut score = FollowScore::default();
        out.clear();
        self.follow_runs.clear();
        // The short-park exception belongs to the run immediately behind
        // that park, not every run on its row. Resolve its cohort once before
        // the candidate loop so unrelated runs keep the two-arrival floor.
        let short_owner = short_park.and_then(|(from, to)| {
            if from.0 != to.0 || !(1..=2).contains(&from.1.saturating_sub(to.1)) {
                return None;
            }
            let last_col = from.1.checked_sub(1)?;
            cells
                .iter()
                .find(|cell| !cell.leaving() && cell.row == from.0 && cell.col == last_col)
                .map(|cell| (from.0, cell.cohort))
        });
        for cell in cells {
            if !cell.leaving() && rows.iter().any(|s| s.row == cell.row) {
                self.follow_runs.push((cell.row, cell.cohort));
            }
        }
        self.follow_runs.sort_unstable();
        self.follow_runs.dedup();
        for k in 0..self.follow_runs.len() {
            let (row, cohort) = self.follow_runs[k];
            let Some(own) = rows.iter().find(|s| s.row == row) else {
                continue;
            };
            self.follow_armed.clear();
            for cell in cells {
                if cell.leaving() || cell.row != row || cell.cohort != cohort {
                    continue;
                }
                let Some(i) = self.find(cell.row, cell.col, cell.born) else {
                    continue;
                };
                let seen = self.seen[i];
                if seen.released {
                    continue;
                }
                let gone = unit_at(own.cols, cell.col) != seen.unit;
                self.follow_armed
                    .push((cell.col, seen.unit, gone, seen.arrivals()));
            }
            self.follow_armed.sort_unstable_by_key(|a| a.0);
            self.follow_armed.dedup_by_key(|a| a.0);
            let armed = self.follow_armed.len();
            let gone = self.follow_armed.iter().filter(|a| a.2).count();
            if armed < 2 || gone < 2 || gone * 2 < armed {
                continue;
            }
            let relaid = Self::relaid_suffix(&self.follow_armed, own.cols);
            let mut named = false;
            let mut evidence = false;
            for (k, dr) in FOLLOW_DRS.into_iter().enumerate() {
                let Some(target) = row.checked_add_signed(dr) else {
                    continue;
                };
                let Some(there) = rows.iter().find(|s| s.row == target) else {
                    continue;
                };
                let v = Self::follow_verdict(&self.follow_armed, there.cols, k, relaid);
                // THE HALF-GATE: two or more ARRIVED, and no fewer than
                // half of the records that are neither neutral nor relaid.
                // A single arrival is allowed only with a substantial
                // exact relaid suffix and no competing counted cell.
                let counted = armed.saturating_sub(v.neutral + v.relaid);
                // A narrow wrap can leave just one glyph on the row the
                // composer carries up. One arrival alone is ambiguous, but
                // the same frame relaying at least three exact trailing
                // glyphs to the start of the old row identifies the wrap.
                // No other non-neutral hole may compete with that sole
                // arrival. The ordinary two-arrival law stays unchanged.
                let sole_kept = short_owner == Some((row, cohort))
                    && v.found == 1
                    && v.relaid >= 3
                    && counted == 1;
                let enough = (v.found >= 2 || sole_kept) && v.found * 2 >= counted;
                // THE EVIDENCE WITHOUT THE BLOCK: enough records ARRIVED at
                // this offset to be a move, whether or not they are one
                // block — what [`FollowScore::missed`] counts when no offset
                // names the run.
                evidence |= enough;
                if !v.block || !enough {
                    continue;
                }
                let (mut lo, mut hi) = (v.lo, v.hi);
                // A glyph standing here that did not ARRIVE — a twin, or a
                // row a band move carried beside it unseen: neutral.
                let neutral = |col: u16, unit: Unit, arrivals: u8| {
                    arrivals & (1 << k) == 0 && unit_at(there.cols, col) == unit
                };
                // The extent, riders included, grows over a bordering
                // never-armed cell of the run while no record past it on that
                // side stayed behind (see the doc).
                let is_cell = |col: u16| {
                    cells
                        .iter()
                        .any(|c| !c.leaving() && c.row == row && c.cohort == cohort && c.col == col)
                };
                let armed_at = |col: u16| {
                    self.follow_armed
                        .binary_search_by_key(&col, |a| a.0)
                        .ok()
                        .map(|i| self.follow_armed[i])
                };
                let beyond_hi = self
                    .follow_armed
                    .iter()
                    .any(|a| a.0 > hi && !neutral(a.0, a.1, a.3));
                let beyond_lo = self
                    .follow_armed
                    .iter()
                    .any(|a| a.0 < lo && !neutral(a.0, a.1, a.3));
                while let Some(next) = hi.checked_add(1) {
                    let grows = match armed_at(next) {
                        Some(a) => neutral(a.0, a.1, a.3),
                        None => !beyond_hi && is_cell(next),
                    };
                    if !grows {
                        break;
                    }
                    hi = next;
                }
                while let Some(prev) = lo.checked_sub(1) {
                    let grows = match armed_at(prev) {
                        Some(a) => neutral(a.0, a.1, a.3),
                        None => !beyond_lo && is_cell(prev),
                    };
                    if !grows {
                        break;
                    }
                    lo = prev;
                }
                out.push(FollowRun {
                    row,
                    cohort,
                    lo,
                    hi,
                    dr,
                });
                named = true;
                break;
            }
            if !named && evidence {
                score.missed = score
                    .missed
                    .saturating_add(u32::try_from(gone).unwrap_or(u32::MAX));
            }
        }
        score
    }

    /// **ONE OFFSET'S READING OF A RUN'S ARMED RECORDS** ([`Witness::follow_runs`]),
    /// bit `k` of each record's [`Seen::arrivals`] being that offset's:
    /// every record, in column order, is
    ///
    /// * FOUND — its unit stands at its own column on the target row and
    ///   the witness has evidence it ARRIVED there: that row was seen
    ///   holding something else while the record stood ([`Seen::clear`]),
    ///   and no copy of it stood there ([`Seen::twin`]);
    /// * NEUTRAL — its unit stands there without that evidence: a copy that
    ///   was there when the record was armed, one that appeared beside the
    ///   standing line and stood [`TWIN_MIN`] (fzf's list, a completion
    ///   popup), or a row a band move carried beside it unseen — its
    ///   presence proves nothing either way (2026-09-23; accumulated,
    ///   2026-09-25);
    /// * a HOLE — its unit is not there, twin or not; a hole among the last
    ///   `relaid` records ([`Witness::relaid_suffix`]) is counted apart, for
    ///   the half-gate.
    ///
    /// The found records must be one block — a hole after the first found
    /// record and a found one after that hole break it (`found` still
    /// counts every arrival, for the evidence) — and neutral records RIDE:
    /// inside the block with no special case, and at its edges FLUSH
    /// against it — a lead of neutrals directly before the first found
    /// record, a trail directly after the last — whatever their own row
    /// reads; a neutral cut off from the block by a hole stays.
    ///
    /// **WHATEVER THEIR OWN ROW READS** (2026-09-23, the review of the first
    /// cut, which let an edge neutral ride only while its own glyph was
    /// GONE). A bottom-anchored composer re-lays the caret row with the
    /// moved word, so a kept line's first cell whose letter the moved word
    /// also starts with reads as standing — `HEY` / `HOW` / the moved `HIS`,
    /// ordinary English — and was left on the caret row, where the run it
    /// was split into (standing, its bounds reaching the old line's end)
    /// captured every later key of the new line: measured at 90 columns,
    /// the new row's columns 4..14 re-walked the row above's stops column
    /// for column (`Ribbon::join_cohort`). Leaving one cell behind costs the
    /// whole new line; carrying a glyph that truly stayed costs one cell's
    /// light — the edge rider no longer reads the gone bit.
    fn follow_verdict(
        armed: &[(u16, Unit, bool, u8)],
        there: &[char],
        k: usize,
        relaid: usize,
    ) -> FollowVerdict {
        let mut v = FollowVerdict {
            found: 0,
            neutral: 0,
            relaid: 0,
            lo: u16::MAX,
            hi: 0,
            block: true,
        };
        let relaid_from = armed.len().saturating_sub(relaid);
        let mut after = false;
        let mut lead: Option<u16> = None;
        let mut ext_hi = 0u16;
        for (i, &(col, unit, _, arrivals)) in armed.iter().enumerate() {
            let here = unit_at(there, col) == unit;
            // No evidence the glyph there ARRIVED ([`Seen::arrivals`]): a
            // twin, or a row a band move carried beside it unseen.
            let twin = arrivals & (1 << k) == 0;
            if here && !twin {
                v.found += 1;
                if after {
                    // Not one block — but every arrival still counts for
                    // the evidence [`FollowScore::missed`] reads.
                    v.block = false;
                    continue;
                }
                v.lo = v.lo.min(lead.unwrap_or(col));
                lead = None;
                ext_hi = col;
            } else if here {
                v.neutral += 1;
                if v.found == 0 {
                    lead.get_or_insert(col);
                } else if !after {
                    // Inside the block, or trailing it flush: it rides.
                    ext_hi = col;
                }
            } else {
                if i >= relaid_from {
                    v.relaid += 1;
                }
                if v.found > 0 {
                    after = true;
                } else {
                    lead = None;
                }
            }
        }
        v.hi = ext_hi;
        v
    }

    /// **THE WORD A WRAP RELAID** ([`Witness::follow_runs`], 2026-09-23 —
    /// the review of the half-gate): how many of `armed`'s LAST records
    /// (column order) stand on the run's own row `own` in order and at
    /// their spacing, shifted left as one block so the first of them lands
    /// on the row's first glyph column — the longest such suffix, `0` when
    /// the row holds no glyph or none matches. That is a composer's wrap:
    /// the line's last word moved off the row's end and laid again at the
    /// start of the caret row (`  this confirmatio` → `  confirmation`).
    /// Anchored at the row's first glyph, so a row REWRITTEN with other
    /// text, or with the word anywhere but leading it, relays nothing.
    /// `O(armed)` per candidate start and nearly always one compare each:
    /// only a run past the gone gate is read, an event rather than a frame.
    fn relaid_suffix(armed: &[(u16, Unit, bool, u8)], own: &[char]) -> usize {
        let Some(first) = own
            .iter()
            .position(|&c| c != ' ' && c != '\0')
            .and_then(|i| u16::try_from(i).ok())
        else {
            return 0;
        };
        for (start, &(c0, ..)) in armed.iter().enumerate() {
            let Some(shift) = c0.checked_sub(first).filter(|&s| s > 0) else {
                continue;
            };
            if armed[start..]
                .iter()
                .all(|&(col, unit, ..)| unit_at(own, col - shift) == unit)
            {
                return armed.len() - start;
            }
        }
        0
    }

    /// **THE SHIFT PASS'S SEARCH** (2026-09-24, the band follows its text
    /// ALONG the row — the owner: *"when I did a backward movement, the
    /// rainbow cursor trail fractured with black spaces when I started
    /// typing in the middle of a line"*). A mid-line insert pushes every
    /// glyph from the edit point to the end of the line right by the
    /// inserted width. Read per cell,
    /// every one of those columns is REPLACED text, and the walk melted the
    /// whole tail in [`super::ribbon::RETIRE_MELT_S`] — the band went black
    /// from the caret to the end of the line on the first key typed inside
    /// it, and every later edit cut another dark stretch out of it. The
    /// text did not go anywhere: it moved `dc` columns, and so does its
    /// light now (`Ribbon::shift_run`), every clock, price and `t` intact.
    ///
    /// For every run `(row, cohort)` with a resident cell on a sampled row
    /// whose records are all live (a released run is on its retract and is
    /// left to it) and none of whose cells right of the edit point is
    /// already LEAVING (a Backspace's or a kill's suffix retract — the
    /// erase's own law, below — or a melt), in column order:
    ///
    /// * `s` is the run's FIRST armed record whose glyph is gone from its
    ///   own column; without one nothing moved;
    /// * the shift `dc` is found from the evidence, not searched blind: a
    ///   column right of `s` holding `s`'s own glyph, nearest first — the
    ///   narrowest insert that explains the row;
    /// * the BLOCK — every cell of the run from `dc`'s first moved cell on
    ///   — must stand at `col + dc` under its own glyph (a never-armed cell,
    ///   a space, under a blank), as ONE run from the block's start: the
    ///   first cell that does not is the start of a TAIL, and every
    ///   recorded glyph of the tail must have left the row (its `col + dc`
    ///   blank — the word a composer's wrap moved down, the line's end past
    ///   the grid). A tail glyph standing on OTHER text refuses the shift:
    ///   that is a rewrite, not a move;
    /// * the block is extended LEFT over the cells whose glyph (or blank)
    ///   also stands at `col + dc` — the letters natural text repeats
    ///   (`ll`, the spaces before an insert at a space) — and when the
    ///   caret stands inside that extension, the CARET is the edit point:
    ///   a hand typing at the caret inserts there, whatever the letters
    ///   repeat;
    /// * evidence: at least two of the block's recorded glyphs found — or
    ///   one, when the edit point is the caret's own column, nothing of the
    ///   block left the row and no text stands past it (a key typed one
    ///   cell before the line's last glyph) — and the found glyphs' own
    ///   columns holding text now (an insert overwrites every column from
    ///   its edit point on; a copy of a CLEARED tail further along the row
    ///   is not where it went). The tail that left the row does not weigh
    ///   against the block: a wrap can take most of a line.
    ///
    /// **A DELETE IS NOT FOLLOWED, on purpose.** A mid-line Backspace or
    /// kill pulls the tail left, and [`super::ribbon::Ribbon`]'s erase law
    /// takes the row's suffix from the caret on (its doc records the slide
    /// that was built, measured on glass against the suffix and not taken).
    /// The erase is replayed at the KEY, before its echo moves the text, so
    /// the tail is already leaving by the frame a shift could be read — a
    /// shift pass for deletes would keep the tail lit only when key and echo
    /// share a frame, which is a different law per frame timing. Deletes
    /// keep the one law they have; this pass is the insert's.
    ///
    /// Pure in the records, the samples and the caret; allocation-free past
    /// warm-up. Every engaged frame with samples pays one record lookup per
    /// live cell on a sampled row (`O(cells × log records)`), and stops
    /// there when no armed glyph left its column; only the runs that lost a
    /// glyph are gathered, sorted and searched (`O(cells × width)` each).
    pub fn shift_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        caret: Option<(u16, u16)>,
        out: &mut Vec<ShiftRun>,
    ) {
        out.clear();
        self.follow_runs.clear();
        // Only a run with a cell whose recorded glyph LEFT its column can
        // have been pushed ([`find_shift`] starts from the first such cell),
        // so only those runs are gathered: on a frame where every armed
        // glyph still stands, this loop is the whole pass.
        for cell in cells {
            if cell.leaving() {
                continue;
            }
            let Some(own) = rows.iter().find(|s| s.row == cell.row) else {
                continue;
            };
            let gone = self
                .find(cell.row, cell.col, cell.born)
                .is_some_and(|i| unit_at(own.cols, cell.col) != self.seen[i].unit);
            if gone {
                self.follow_runs.push((cell.row, cell.cohort));
            }
        }
        if self.follow_runs.is_empty() {
            return;
        }
        self.follow_runs.sort_unstable();
        self.follow_runs.dedup();
        for k in 0..self.follow_runs.len() {
            let (row, cohort) = self.follow_runs[k];
            let Some(own) = rows.iter().find(|s| s.row == row).copied() else {
                continue;
            };
            self.shift_cells.clear();
            let mut released = false;
            // The rightmost cell of the run already LEAVING: an erase's
            // suffix retract, a melt. A shift is never read from its edit
            // point rightward over light that is going out.
            let mut leaving_max: Option<u16> = None;
            for cell in cells {
                if cell.row != row || cell.cohort != cohort {
                    continue;
                }
                if cell.leaving() {
                    leaving_max = Some(leaving_max.map_or(cell.col, |m| m.max(cell.col)));
                    continue;
                }
                let rec = match self.find(cell.row, cell.col, cell.born) {
                    Some(i) if self.seen[i].released => {
                        released = true;
                        break;
                    }
                    Some(i) => Some(self.seen[i].unit),
                    None => None,
                };
                let gone = rec.is_some_and(|u| unit_at(own.cols, cell.col) != u);
                self.shift_cells.push((cell.col, rec, gone));
            }
            if released {
                continue;
            }
            self.shift_cells.sort_unstable_by_key(|c| c.0);
            self.shift_cells.dedup_by_key(|c| c.0);
            let caret_col = caret.filter(|c| c.0 == row).map(|c| c.1);
            if let Some((lo, hi, dc)) = find_shift(&self.shift_cells, own.cols, caret_col)
                && leaving_max.is_none_or(|m| m < lo)
            {
                out.push(ShiftRun {
                    row,
                    cohort,
                    lo,
                    hi,
                    dc,
                });
            }
        }
    }

    /// **THE RECORDS FOLLOW THEIR CELLS ALONG THE ROW**
    /// ([`Witness::shift_runs`] → `Ribbon::shift_run`): each identity in
    /// `moved` — a cell's OLD `(row, col, born)` — is re-keyed to
    /// `col + dc`. Re-sorted in place afterwards, as
    /// [`Witness::translate_cells`].
    ///
    /// **A SHIFTED RECORD STARTS OVER**, as a carried one does
    /// ([`Witness::translate_cells`]): what its neighbour rows were seen
    /// holding ([`Seen::clear`], [`Seen::twin`], [`Seen::pending`]) was
    /// about its OLD column, and a deferred verdict ([`Seen::deferred`]) was
    /// about the glyph that stood there — both are forgotten
    /// ([`Seen::forget_evidence`]), and the walks that follow learn the new
    /// column's neighbours.
    pub fn shift_cells(&mut self, moved: &mut [(u16, u16, Instant)], dc: i16) {
        if moved.is_empty() || dc == 0 {
            return;
        }
        moved.sort_unstable();
        // A record standing where a moved one lands, with the same birth, is
        // the left-behind tail's (`Ribbon::shift_run` drops its cell): it
        // goes now, so no two records share one `(row, col, born)` key and
        // `find` never has to pick between them.
        self.seen.retain(|s| {
            moved.binary_search(&(s.row, s.col, s.born)).is_ok()
                || !s
                    .col
                    .checked_add_signed(-dc)
                    .is_some_and(|src| moved.binary_search(&(s.row, src, s.born)).is_ok())
        });
        let mut any = false;
        for s in &mut self.seen {
            if moved.binary_search(&(s.row, s.col, s.born)).is_ok()
                && let Some(target) = s.col.checked_add_signed(dc)
            {
                s.col = target;
                s.forget_evidence();
                s.deferred = false;
                any = true;
            }
        }
        if any {
            self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
        }
        // A never-armed blank the shift carried with its run is still one
        // ([`Witness::blank_seen`], as the vertical carry keeps it).
        let mut blanks = false;
        for b in &mut self.blank_seen {
            if moved.binary_search(b).is_ok()
                && let Some(target) = b.1.checked_add_signed(dc)
            {
                b.1 = target;
                blanks = true;
            }
        }
        if blanks {
            self.blank_seen.sort_unstable();
            self.blank_seen.dedup();
        }
    }

    /// **THE RECORDS FOLLOW THEIR CELLS** ([`Witness::follow_runs`] →
    /// `Ribbon::translate_run`): each identity in `moved` — a cell's OLD
    /// `(row, col, born)` — is re-keyed to `row + dr`; a cell without a
    /// record (never armed) needs nothing but its place on
    /// [`Witness::blank_seen`], carried with it (2026-09-24). The list is re-sorted in place
    /// afterwards: an event, not a frame, and `sort_unstable` allocates
    /// nothing.
    ///
    /// **A CARRIED RECORD STARTS OVER.** Its neighbours on the new row are
    /// not the ones
    /// it was seen beside — the run moved alone, the rows around it did not
    /// — so its evidence is FORGOTTEN: every offset clear, no twin
    /// ([`Seen::forget_evidence`]), exactly a record armed where nothing was
    /// seen, and the walks that follow learn its new neighbours
    /// ([`Witness::walk`]). A second move with no key between — a box
    /// relocated twice, an inline chat box pushed down twice by streamed
    /// rows, two moves on consecutive frames — follows again
    /// (`tests/moved_again_without_a_key.rs`). Started over with NOTHING
    /// clear — a sample of the destination without the glyph required first
    /// — a move on the very next frame, or two rows away where no row is
    /// named, would melt under its own text. A copy standing beside the new
    /// row is a twin once it has stood there [`TWIN_MIN`], and not before:
    /// a kill sooner carries the band onto it (THE LIMITS, the module doc).
    pub fn translate_cells(&mut self, moved: &mut [(u16, u16, Instant)], dr: i16) {
        if moved.is_empty() || dr == 0 {
            return;
        }
        // The records are read against a SORTED copy of the identities, so
        // a record already moved cannot break the search for the next.
        moved.sort_unstable();
        let mut any = false;
        for s in &mut self.seen {
            if moved.binary_search(&(s.row, s.col, s.born)).is_ok()
                && let Some(target) = s.row.checked_add_signed(dr)
            {
                s.row = target;
                s.forget_evidence();
                s.deferred = false;
                any = true;
            }
        }
        if any {
            self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
        }
        // A never-armed blank the pass carried with its run is still one.
        let mut blanks = false;
        for b in &mut self.blank_seen {
            if moved.binary_search(b).is_ok()
                && let Some(target) = b.0.checked_add_signed(dr)
            {
                b.0 = target;
                blanks = true;
            }
        }
        if blanks {
            self.blank_seen.sort_unstable();
        }
    }

    /// Records held.
    #[must_use]
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// True with nothing witnessed.
    #[must_use]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Forget everything (a reset: the cells are gone with the coordinate
    /// space). Capacity is kept.
    pub fn clear(&mut self) {
        self.seen.clear();
        self.blank_seen.clear();
    }

    /// A scroll moved every cell up by `rows` (`Ribbon::translate_scroll`):
    /// the records move with them, and those that left the grid are dropped;
    /// so do the blanks the last walk read ([`Witness::blank_seen`]). Order
    /// is preserved — every row shifts by the same amount.
    ///
    /// **THE EVIDENCE MOVES WITH THEM** (2026-09-25): a scroll moves
    /// the WHOLE screen, so the row that stood `dr` away from a record still
    /// stands `dr` away, holding what it held — what the record's neighbours
    /// were seen holding ([`Seen::clear`], [`Seen::twin`]) is still true of
    /// them. Forgetting it instead would lose a move on the scroll frame's
    /// heels: an inline chat box at the screen's bottom rewritten under each
    /// streamed row, whose text lands one row below where the scroll carried
    /// its band (`tests/moved_again_without_a_key.rs`).
    ///
    /// **…EXCEPT ABOUT THE ROWS IT BROUGHT IN.** The rows scrolled in are
    /// the last `rows` above `bottom` — the row past the region that
    /// scrolled: the grid's last row, or the focused pane's in a split
    /// (`Engine::translate_scroll`). They were past the region's edge: never
    /// sampled, and whatever stands there now came in WITH the scroll. Every
    /// offset of a record that reaches one has its [`Seen::pending`] bit set
    /// and its clock started at the last walk before the scroll
    /// ([`Witness::last_walk`]): a copy of the glyph there has stood since
    /// then at the latest, so the first look that finds it is a twin once
    /// [`TWIN_MIN`] has passed since — a command line typed on the last row,
    /// then a job notice's redraw that scrolls and re-echoes the line on the
    /// new last row, then the original erased 48 ms later
    /// (`tests/copy_beside_the_line.rs`, in a full window and in a split's
    /// top pane). Sooner, it is the line relocated by the scroll and torn —
    /// the original erased a frame or two after — which follows
    /// (`tests/moves_are_followed.rs`). An offset never looked at is still
    /// clear — the line relocated onto the new last row before any frame
    /// sampled it follows (the law's LIMIT). The price is a torn relocation
    /// that comes WITH a scroll: its threshold is dated from the frame before
    /// the scroll, so it melts from a 32 ms tear on 16 ms frames and under the
    /// pet's pacing (48 ms on 8 ms frames), where one without a scroll melts
    /// from 48 ms (40 ms). Rows below `bottom` — another pane's — are not
    /// marked. `bottom` is `0` while the grid is unmeasured, and then nothing
    /// is marked.
    pub fn translate(&mut self, rows: u16, bottom: u16) {
        if rows == 0 {
            return;
        }
        let first_new = bottom.saturating_sub(rows);
        let last_walk = self.last_walk;
        self.seen.retain_mut(|s| {
            if s.row < rows {
                return false;
            }
            s.row -= rows;
            if bottom > 0 {
                for (k, dr) in FOLLOW_DRS.into_iter().enumerate() {
                    if s.row
                        .checked_add_signed(dr)
                        .is_some_and(|r| r >= first_new && r < bottom)
                    {
                        s.pending |= 1 << k;
                        s.pend_ms[k] = last_walk.map_or(0, |t| {
                            u32::try_from(t.saturating_duration_since(s.born).as_millis())
                                .unwrap_or(u32::MAX)
                        });
                    }
                }
            }
            true
        });
        self.blank_seen.retain_mut(|b| {
            if b.0 < rows {
                return false;
            }
            b.0 -= rows;
            true
        });
    }

    /// A ROW BAND moved (`Ribbon::translate_band`, the
    /// [`crate::cursor_glow::band_row`] law): a record outside
    /// `top..=bottom` stands, one inside moves by `delta` with its cell, and
    /// one carried past the band's edge is dropped with it — and so does
    /// each blank the last walk read ([`Witness::blank_seen`]). Rows inside and
    /// outside the band can cross, so the list is re-sorted in place — an
    /// event, not a frame, and `sort_unstable` allocates nothing.
    ///
    /// A band move is NOT uniform, so neighbour evidence survives it only
    /// where the neighbour's content moved with the record ([`band_keeps`],
    /// 2026-09-25): a record inside the band keeps what it saw of rows
    /// inside the band, and forgets the offsets that reach across its edge;
    /// a record outside the band forgets the offsets that reach into it —
    /// vim's `yyp` opens a line under the one just typed, and what was seen
    /// there before is not what stands there now. A forgotten offset is NOT
    /// clear ([`Seen::clear`]): a glyph found there is no arrival
    /// until a sample has shown the row without it, so `yyp`'s copy, put
    /// into the opened line in the same batch, is not where the original
    /// went when `S` clears it — however few frames sampled it between
    /// (`tests/copy_beside_the_line.rs`).
    pub fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.seen
            .retain_mut(|s| match band_row(s.row, top, bottom, delta) {
                Some(row) => {
                    let keep = band_keeps(s.row, row, top, bottom, delta);
                    s.clear &= keep;
                    s.twin &= keep;
                    s.pending &= keep;
                    s.row = row;
                    true
                }
                None => false,
            });
        self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
        self.blank_seen
            .retain_mut(|b| match band_row(b.0, top, bottom, delta) {
                Some(row) => {
                    b.0 = row;
                    true
                }
                None => false,
            });
        self.blank_seen.sort_unstable();
    }

    /// **THE INSERT'S REWRITE RE-LAID `row` LEFT OF `col`** ([`super::Event::Rewrite`],
    /// 2026-09-13): drop the records there — and, since 2026-09-24, the
    /// blanks the last walk read there ([`Witness::blank_seen`]) — so the
    /// next walk ARMS the cells under the placeholder's glyphs instead of
    /// retiring them. Claude
    /// Code's `[Image #1] ` is different text over the insert's first cells,
    /// and the seam promises those cells keep their light under one
    /// continuous ribbon (§27) — judged as an overwrite they melted, and the
    /// next key laid beside a retired cell in a new cohort: the rainbow
    /// severed by a hole the placeholder's width. The suffix's records fall
    /// out of the walk on their own (its cells are leaving); a later re-lay
    /// of the row with other text still retires the re-armed cells. Order
    /// is preserved — `retain` keeps it.
    pub fn forget_left_of(&mut self, row: u16, col: u16) {
        self.seen.retain(|s| !(s.row == row && s.col < col));
        self.blank_seen.retain(|b| !(b.0 == row && b.1 < col));
    }

    /// The record for the cell `(row, col, born)`, if any.
    fn find(&self, row: u16, col: u16, born: Instant) -> Option<usize> {
        self.seen
            .binary_search_by(|s| (s.row, s.col, s.born).cmp(&(row, col, born)))
            .ok()
    }

    /// Arm a record for `cell` holding `unit` — unless the unit is blank
    /// (blank → glyph only arms, later) or the witness is full — and take
    /// its first frame of neighbour evidence from `nb`, the rows sampled at
    /// its follow offsets ([`Seen::observe`] with `at_arm`): a copy of its
    /// glyph standing there already is a TWIN at once; every other offset,
    /// sampled or not, is clear.
    ///
    /// A fresh line's first keys have no band of their own yet to name their
    /// rows: two keys armed in one frame under an identical line with that
    /// line unseen, then erased at once, carry the band onto it
    /// (`tests/erased_under_its_twin.rs`). So the host names a waiting key's
    /// row and its `±1` on that frame, in slots of their own
    /// (`Engine::ribbon_rows_for`): the copy that was there first is seen and
    /// is a twin. What the witness still did not see is no evidence either
    /// way and, as on 0.93.0, does not stop a move.
    fn arm(&mut self, cell: &Cell, unit: Unit, nb: &[Option<&[char]>; 4], now: Instant) {
        if unit.is_blank() {
            return;
        }
        self.insert(cell, unit, false);
        if let Some(i) = self.find(cell.row, cell.col, cell.born) {
            self.seen[i].observe(nb, true, now);
        }
    }

    /// A RELEASED record for a never-armed cell of a run the walk just
    /// released — the space in `hello world`, nothing ever under it: it
    /// went to the swoosh with its run, and a glyph landing on it is read
    /// as replaced text exactly as on its run-mates. Unless the witness is
    /// full, when the cell is simply not witnessed, as ever.
    fn hold_released(&mut self, cell: &Cell) {
        self.insert(cell, Unit::BLANK, true);
    }

    /// Insert the record for `cell` at its sorted place, live; a full
    /// witness ([`WITNESS_CAP`]) takes nothing.
    fn insert(&mut self, cell: &Cell, unit: Unit, released: bool) {
        if self.seen.len() >= WITNESS_CAP {
            return;
        }
        let key = (cell.row, cell.col, cell.born);
        let at = self.seen.partition_point(|s| (s.row, s.col, s.born) < key);
        self.seen.insert(
            at,
            Seen {
                row: cell.row,
                col: cell.col,
                born: cell.born,
                unit,
                live: true,
                released,
                restorable: released,
                counted: released,
                clear: ALL_OFFSETS,
                twin: 0,
                pending: 0,
                pend_ms: [0; 4],
                deferred: false,
            },
        );
    }

    /// Both cells of the unit a record stands for, pushed onto `retire` by
    /// identity. The partner half carries the SAME `born`: a wide glyph's
    /// two cells are laid by one keystroke at one instant (`Ribbon::lay`),
    /// and a cell at the partner's column with another birth is a different
    /// cell, judged on its own record in the same walk.
    fn push_unit(
        retire: &mut Vec<(u16, u16, Instant)>,
        row: u16,
        col: u16,
        born: Instant,
        unit: Unit,
    ) {
        retire.push((row, col, born));
        if unit.wide {
            if unit.cont {
                if let Some(lead) = col.checked_sub(1) {
                    retire.push((row, lead, born));
                }
            } else {
                retire.push((row, col.saturating_add(1), born));
            }
        }
    }

    /// **THE WALK.** Every resident cell that is not already leaving is read
    /// against this frame's samples: a cell on an unsampled row keeps what
    /// the witness knew; a cell first seen over a glyph is armed; a cell
    /// whose recorded glyph has CHANGED has its unit pushed onto `retire`
    /// BY IDENTITY, `(row, col, born)`, and its record dropped (the cell is
    /// stamped and leaving); a cell whose recorded glyph has GONE goes onto
    /// `release` (both lists cleared here, so the caller reads exactly this
    /// walk's verdicts) — unless its run decides otherwise, below — and its
    /// record is KEPT, marked released; a cell re-laid since (a different
    /// `born`) is a different cell and is armed on its own.
    ///
    /// **THE RUN DECIDES THE CLOCK.** A blanked cell's verdict is provisional
    /// until every cell of its run has been read: if any cell of the run was
    /// REPLACED this walk, or — for a run with no recorded glyph still
    /// standing in place (2026-09-22: a run that stands lost a tail, it did
    /// not move) — the run's blanked glyphs are found in order at
    /// their own spacing on any sampled row other than where they were
    /// ([`Witness::moved`] — the text moved, the light did not), the run's
    /// blanked cells go onto `retire` with it; otherwise the run's text is
    /// gone and they go onto `release`. Never-armed cells follow their run
    /// when it is released — only those the loss is around while its text
    /// still stands ([`Witness::released_span_takes`]) — or has no
    /// witnessed glyph left standing; unchanged blanks between surviving
    /// letters stay lit. A
    /// released run's never-armed cells get a released record of their own.
    /// Records whose cells the walk never
    /// reached — dropped by the ribbon — are forgotten. A cell may be pushed
    /// twice (a wide unit's two halves both resident); `Ribbon::retire_cells`
    /// and `Ribbon::release_cells` act on it once.
    ///
    /// **A RELEASED RECORD IS READ AGAIN ON EVERY WALK** while its cell is
    /// resident (unstamped, on the retract): the same glyph back is D2 and
    /// a blank is nothing, but a glyph landing under it is REPLACED text —
    /// the cell goes onto `retire`, and so does every held run-mate still
    /// under a blank. Those re-verdicts name cells an earlier walk already
    /// released and `Engine::witness_rows` already counted, so the walk
    /// RETURNS how many of them it named on `retire` this time — the caller
    /// subtracts them and `ribbon_retired=` stays one count per cell. A
    /// held cell is never released again and never searched for.
    ///
    /// **EVERY STANDING RECORD TAKES THIS FRAME'S NEIGHBOUR EVIDENCE**
    /// (2026-09-25; [`Seen::observe`]): a record whose glyph stands
    /// where it was recorded reads the rows sampled at its follow offsets —
    /// something else there is [`Seen::clear`], a copy of its glyph there a
    /// step toward [`Seen::twin`], timed by `now` — and a record armed this
    /// walk takes its first frame's the same way. That is what the follow
    /// pass at the start of the next tick judges an arrival by. One lookup
    /// of the neighbour samples per row the walk visits; nothing allocates.
    ///
    /// **A RUN BESIDE A WITHHELD ROW WAITS ONE WALK** (2026-09-25):
    /// `withheld` are rows the host was asked for this frame and did not
    /// deliver (`Engine::follow_rows`), and a run with a changed record
    /// beside one of them is passed
    /// over — no verdict, no arming, no evidence — at most once in a row
    /// ([`Witness::find_deferred_runs`]).
    pub fn walk(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        withheld: &[u16],
        now: Instant,
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        retire.clear();
        release.clear();
        self.retired_runs.clear();
        self.released_runs.clear();
        self.standing_glyphs.clear();
        self.blanked.clear();
        self.fresh_released.clear();
        self.counted_names.clear();
        self.blank_next.clear();
        self.landed.clear();
        for s in &mut self.seen {
            s.live = false;
        }
        // The frame a scroll before the next walk dates its rows from.
        self.last_walk = Some(now);
        // The rows sampled at every follow offset from the row last looked
        // up — the evidence each standing record takes this frame
        // ([`Seen::observe`]). Recomputed only when the walk changes row.
        let mut nb_row = None;
        let mut nb = [None; 4];
        self.find_deferred_runs(cells, rows, withheld);
        for cell in cells {
            if !self.deferred_runs.is_empty()
                && self
                    .deferred_runs
                    .binary_search(&(cell.row, cell.cohort))
                    .is_ok()
            {
                if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                    self.seen[i].live = true;
                    if !self.seen[i].released
                        && rows
                            .iter()
                            .find(|s| s.row == cell.row)
                            .is_some_and(|s| unit_at(s.cols, cell.col) != self.seen[i].unit)
                    {
                        self.seen[i].deferred = true;
                    }
                } else if self.was_blank(cell) {
                    // No verdict this walk: it keeps what the witness knew
                    // ([`Witness::blank_seen`]).
                    self.blank_next.push((cell.row, cell.col, cell.born));
                }
                continue;
            }
            if cell.leaving() {
                // A cell a PARTIAL release stamped onto the retract keeps its
                // released record while it is resident, so the identical
                // redraw a frame later can still restore it
                // ([`Witness::restored_runs`], `Ribbon::restore_releases`).
                if cell.released_at.is_some()
                    && let Some(i) = self.find(cell.row, cell.col, cell.born)
                    && self.seen[i].released
                {
                    self.seen[i].live = true;
                }
                continue;
            }
            let Some(sample) = rows.iter().find(|s| s.row == cell.row) else {
                if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                    self.seen[i].live = true;
                } else if self.was_blank(cell) {
                    // Unsampled, it keeps what the witness knew.
                    self.blank_next.push((cell.row, cell.col, cell.born));
                }
                continue;
            };
            let unit = unit_at(sample.cols, cell.col);
            if nb_row != Some(cell.row) {
                nb_row = Some(cell.row);
                nb = neighbours_of(rows, cell.row);
            }
            match self.find(cell.row, cell.col, cell.born) {
                Some(i) => {
                    let seen = self.seen[i];
                    if seen.unit == unit {
                        self.seen[i].live = true;
                        self.seen[i].deferred = false;
                        // A recorded glyph standing where it was recorded:
                        // its run still has text — and what stands beside it
                        // this frame is evidence for the follow pass.
                        if !seen.released && !unit.is_blank() {
                            self.standing_glyphs.push((cell.row, cell.cohort, cell.col));
                            self.seen[i].observe(&nb, false, now);
                        }
                    } else if unit.is_blank() {
                        self.blanked.push(Blanked {
                            row: cell.row,
                            col: cell.col,
                            born: cell.born,
                            cohort: cell.cohort,
                            unit: seen.unit,
                            held: seen.released,
                            counted: seen.counted,
                        });
                    } else {
                        // A different glyph — under a live cell, or under a
                        // released one still retracting: the wrong letters.
                        Self::push_unit(retire, cell.row, cell.col, cell.born, seen.unit);
                        let run = (cell.row, cell.cohort);
                        if !self.retired_runs.contains(&run) {
                            self.retired_runs.push(run);
                        }
                        if seen.counted {
                            self.counted_names.push((cell.row, cell.col, cell.born));
                        }
                    }
                }
                None => {
                    if unit.is_blank() {
                        self.blank_next.push((cell.row, cell.col, cell.born));
                    } else if self.was_blank(cell) {
                        // A glyph LANDED on an old blank: armed as ever,
                        // and judged with its run below.
                        self.landed
                            .push((cell.row, cell.col, cell.born, cell.cohort));
                    }
                    self.arm(cell, unit, &nb, now);
                }
            }
        }
        self.standing_glyphs.sort_unstable();
        self.standing_glyphs.dedup();
        // THE RUN DECIDES: each run with a blanked cell is retired if one of
        // its glyphs was replaced or its blanked text is found elsewhere, and
        // released otherwise. Sorted by run, the FRESH blanks before the HELD
        // ones, so the moved-text search reads each run's newly blanked
        // glyphs in column order, once, and never the held cells' — theirs
        // went on an earlier walk.
        self.blanked
            .sort_unstable_by_key(|b| (b.row, b.cohort, b.held, b.col));
        let mut i = 0;
        while i < self.blanked.len() {
            let run = (self.blanked[i].row, self.blanked[i].cohort);
            let end = i + self.blanked[i..]
                .iter()
                .take_while(|b| (b.row, b.cohort) == run)
                .count();
            let fresh_end = i + self.blanked[i..end].iter().take_while(|b| !b.held).count();
            // **A RUN THAT STILL STANDS HAS NOT MOVED** (2026-09-22 — the
            // owner, on 0.89: *"There is a rainbow trail gap in 0.89. I'm
            // not sure what is causing it but it seems like some kind of
            // back cursor movement bug."*). The moved-text search has no
            // length floor, so a one- or two-glyph fragment (`TO`, `N`,
            // `IN`) is found somewhere on almost any line of English
            // (`NEED TO`, `STOP`). A present that lands between two PTY
            // reads of one Claude Code frame — the frame is over 1 KiB, the
            // macOS PTY hands it over in 1024-byte reads — sees the composer
            // row cleared by `CSI 2K` and rewritten only up to the read
            // boundary, the caret hidden: every cell right of the boundary
            // reads BLANK, and the newest letters of the hand's own run were
            // ruled MOVED TEXT and retired. No restore reaches a
            // retirement and nothing re-lays an interior column, so the
            // identical text back 2 to 30 ms later stood over a permanent
            // black hole inside the live band — the owner's three cells
            // under `TO ` of `INTO `, and every later key only widened it.
            // Text that moved took its WHOLE run with it: a run with a
            // recorded glyph still standing in place
            // ([`Witness::standing_glyphs`], the same record the partial
            // release's span law reads) lost a tail, it did not go
            // elsewhere, so its blanked glyphs are RELEASED — restorable —
            // and never searched for. A replaced glyph still retires its
            // run, and a run with nothing standing is still searched as
            // before.
            let fast = self.retired_runs.contains(&run)
                || (self.standing_glyphs_of(run).is_empty()
                    && Self::moved(&self.blanked[i..fresh_end], rows));
            if fast {
                // The whole run melts: the fresh blanks with their replaced
                // or moved run-mates, and the held cells with them — a
                // re-verdict on each, counted already.
                for b in &self.blanked[i..end] {
                    Self::push_unit(retire, b.row, b.col, b.born, b.unit);
                    if b.counted {
                        self.counted_names.push((b.row, b.col, b.born));
                    }
                }
                if !self.retired_runs.contains(&run) {
                    self.retired_runs.push(run);
                }
            } else {
                // The run's text is gone: the fresh blanks are released and
                // their records kept, marked; the held cells stay on the
                // retract they are on.
                for b in &self.blanked[i..end] {
                    if !b.held {
                        Self::push_unit(release, b.row, b.col, b.born, b.unit);
                        self.fresh_released.push((b.row, b.col, b.born));
                        if b.counted {
                            self.counted_names.push((b.row, b.col, b.born));
                        }
                    }
                    if let Some(k) = self.find(b.row, b.col, b.born) {
                        self.seen[k].live = true;
                    }
                }
                if fresh_end > i {
                    self.released_runs.push(run);
                }
            }
            i = end;
        }
        self.shape_verdicts(cells, rows, now, retire, release);
        self.landed_goes_with_its_run(cells, retire, release);
        // The releases that stood: their records are kept, marked — a glyph
        // landing under one later is REPLACED text, a blank is nothing, the
        // same glyph back is D2 — and counted once.
        for k in 0..self.fresh_released.len() {
            let (row, col, born) = self.fresh_released[k];
            if let Some(i) = self.find(row, col, born) {
                self.seen[i].released = true;
                self.seen[i].counted = true;
            }
        }
        let recounted = self
            .counted_names
            .iter()
            .filter(|id| retire.contains(id) || release.contains(id))
            .count();
        self.find_standing_runs(cells, retire);
        // THE BLANKS GO WITH THEIR RUN: a cell of a named run that holds no
        // record at all — never armed, because nothing was ever under it —
        // goes with its run-mates, on the run's own clock, and a released
        // run's takes a released record so it is read like its run-mates
        // from here on. A cell WITH a record was read above: matched and
        // kept, or changed and named.
        self.released_runs.sort_unstable();
        if !self.retired_runs.is_empty() || !self.released_runs.is_empty() {
            for cell in cells {
                if cell.leaving() || self.find(cell.row, cell.col, cell.born).is_some() {
                    continue;
                }
                let run = (cell.row, cell.cohort);
                if self.retired_runs.binary_search(&run).is_ok() {
                    // **A BLANK THAT IS STILL BLANK, IN A RUN THAT IS STILL
                    // STANDING, IS NOT EVIDENCE**
                    // (2026-09-15, the owner: *"odd logic of drawing a part
                    // of the trail when moving the cursor and editing
                    // text"*). A never-armed cell has no record because
                    // nothing was ever under it — it is the SPACE in
                    // `hello world`. It goes with a RETIRED run because the
                    // run is one object and its letters moved; but where
                    // the space is STILL a space, nothing moved under this
                    // cell and there is nothing for its light to be wrong
                    // over. Retiring it anyway is what punched the owner's
                    // band into word-shaped blocks: one character inserted
                    // mid-line retired eleven cells, five of them spaces at
                    // columns 8, 14, 18, 20 and 25 whose glyphs had not
                    // changed, and the band read
                    // `..######.#####.###.#.-###.####....` — six blocks
                    // with one-cell gaps — for the next 640 ms
                    // (`rbt/c4_edit`, frames 0647 → 0655).
                    //
                    // A run with NOTHING LEFT STANDING still takes its
                    // blanks, and so does a released run whose text went
                    // WHOLE: there the run's text is gone and a space with
                    // nothing around it is the one-cell stray the law was
                    // written for. What is refused is only a hole punched
                    // in a run whose letters are still lit either side of
                    // it.
                    let still_blank = rows
                        .iter()
                        .find(|s| s.row == cell.row)
                        .is_some_and(|s| unit_at(s.cols, cell.col).is_blank());
                    if !still_blank || self.standing_runs.binary_search(&run).is_err() {
                        retire.push((cell.row, cell.col, cell.born));
                    }
                } else if self.released_runs.binary_search(&run).is_ok()
                    && self.released_span_takes(cells, release, run, cell.col)
                {
                    // **A PARTIAL RELEASE TAKES NO SPACE OUTSIDE WHAT IT
                    // LOST** (2026-09-21 — the owner, in Claude Code's
                    // composer: the spaces between typed phrases dark, every
                    // glyph lit). A row sampled mid-rewrite reads ONE
                    // recorded glyph blank for a walk; the run is released
                    // for that cell, and this arm took every never-armed
                    // cell of the run with it — all its spaces — onto the
                    // retract (`Ribbon::release_cells`' partial stamp). A
                    // key typed inside the melt, or the glyph back a walk
                    // later than the melt, then left the letters standing
                    // and the spaces dark: the owner's exact shape. A
                    // released run whose recorded glyphs STILL STAND lost
                    // only part of its text, and its never-armed cells go
                    // only where the loss is around them: inside the span
                    // of the cells it lost, as the retire span law reads
                    // names, or PAST the span with no standing glyph of the
                    // run beyond them on their side — the tail closure
                    // (`released_span_takes`: the review's probe typed
                    // `hello world ` and the program cleared from `w`; the
                    // span alone left the trailing space lit five cells past
                    // `hello `, the one-cell stray this law forbids). So a
                    // suffix an app cleared takes the space inside it and
                    // the space after it (`tests/partial_release.rs`), a
                    // single blanked letter takes none, and the space before
                    // a cleared suffix stands while the letters left of it
                    // do. A run with no recorded glyph standing lost its
                    // text whole, and its spaces go with it as they always
                    // did.
                    release.push((cell.row, cell.col, cell.born));
                    self.hold_released(cell);
                }
            }
        }
        // **WHAT A RUN LOSES, IT LOSES IN ONE PIECE** (2026-09-16, the
        // owner: *"I still am sometimes seeing bugs and gaps"*, and *"when
        // backspacing … the rainbow cursor trail breaks a part"*).
        //
        // Every verdict above is taken PER CELL, by comparing one column's
        // recorded glyph with the one standing there now. A text SHIFT — the
        // mid-line Backspace that pulls the tail left, the insert that pushes
        // it right — changes every column from the edit to the end of the
        // line, so the honest per-cell answer is "replaced" for all of them.
        // But natural text repeats: wherever `old[i + 1] == old[i]` the
        // shifted column holds the SAME glyph it recorded, the comparison
        // answers "unchanged", and that one cell is kept while both its
        // neighbours are named. Measured at the host seam on the owner's own
        // gesture (`c8_bs_mid`: the caret walked back three words into a
        // wrapped composer, then a Backspace run mid-word), the named set
        // came out a COMB — columns 26, 28, 30, 31, 32, 33 and 35 retired,
        // 27, 29 and 34 kept, over `…and I alsosee some a` — and 120 ms
        // later, when the fast melt had taken the named ones, the band on
        // glass was a solid head at 2..25 plus three DETACHED SPECKS: three
        // interior dark runs where the row had had none.
        //
        // `Ribbon::retract_suffix` states the shape light is allowed to
        // leave a row in, and states it as a law: *"CONTIGUOUS at every frame
        // … nothing is ever removed from the MIDDLE"*. The Backspace, the
        // kill and the insert's rewrite all obey it. The content witness is
        // the one path that removes light by NAME, and a name is not a shape:
        // a per-cell verdict can name any subset, and most subsets are holes.
        //
        // So the names are read as a SPAN and not as a set: within one run —
        // one row, one cohort, the object the retirement is already reasoned
        // about as ("the run is one object and its letters moved") — every
        // cell BETWEEN the leftmost and the rightmost named column goes with
        // them. A cell inside that span that compared equal did so by
        // coincidence: the shift moved text over it too, and what is under it
        // now is not what its light was laid for.
        //
        // The span is the tightest closure that removes holes, and that is
        // why it is a span and not a suffix. It swallows only columns the
        // walk has already condemned on BOTH sides, so a change that really
        // is interior stays interior: a program overwriting a wide glyph's
        // two cells still takes exactly those two and leaves its narrow
        // neighbours lit either side
        // ([`a_wide_glyph_s_two_cells_are_retired_as_one_unit`]), and a
        // single stale cell named alone is still named alone
        // ([`a_key_typed_over_an_abandoned_cohorts_cell_keeps_its_own_light`]).
        // A run nothing was named in is untouched. A cell the walk RELEASED
        // (its glyph WENT, rather than changed) is left on its own clock:
        // that verdict has a restore path ([`Witness::restored_runs`]) and
        // this is not the place to revoke it.
        if !retire.is_empty() {
            for i in 0..self.retired_runs.len() {
                let (row, cohort) = self.retired_runs[i];
                let mut lo = u16::MAX;
                let mut hi = 0u16;
                let mut named = false;
                for cell in cells {
                    if cell.row == row
                        && cell.cohort == cohort
                        && retire.contains(&(cell.row, cell.col, cell.born))
                    {
                        named = true;
                        lo = lo.min(cell.col);
                        hi = hi.max(cell.col);
                    }
                }
                if !named || lo >= hi {
                    continue;
                }
                for cell in cells {
                    if cell.leaving()
                        || cell.row != row
                        || cell.cohort != cohort
                        || cell.col < lo
                        || cell.col > hi
                    {
                        continue;
                    }
                    let id = (cell.row, cell.col, cell.born);
                    if !retire.contains(&id) && !release.contains(&id) {
                        retire.push(id);
                    }
                }
            }
        }
        // Tag every original run identity, including its still-visible
        // letters and spaces. A later retype arms an untagged new birth.
        for cell in cells {
            if self
                .released_runs
                .binary_search(&(cell.row, cell.cohort))
                .is_ok()
                && let Some(i) = self.find(cell.row, cell.col, cell.born)
            {
                self.seen[i].restorable = true;
            }
        }
        self.seen.retain(|s| s.live);
        std::mem::swap(&mut self.blank_seen, &mut self.blank_next);
        self.blank_seen.sort_unstable();
        self.blank_seen.dedup();
        recounted
    }

    /// Whether the last walk read `cell` over a blank with no record
    /// ([`Witness::blank_seen`]).
    fn was_blank(&self, cell: &Cell) -> bool {
        self.blank_seen
            .binary_search(&(cell.row, cell.col, cell.born))
            .is_ok()
    }

    /// **A GLYPH THAT LANDED ON A BLANK WITH THE REWRITE THAT TOOK ITS RUN
    /// GOES WITH THE RUN** (2026-09-24 — the owner: *"the spectrum is
    /// smooshed on the next line. I want smooth continuous rainbow"*; the
    /// re-review of the follow landing, *"F1 is still open where a wrap
    /// moves a word at least as long as the line it keeps"*). A cell the
    /// last walk read over a blank ([`Witness::blank_seen`]) that holds a
    /// glyph now was armed on the walk's first pass, as blank → glyph
    /// always is — a key whose echo lands a frame late is never retired.
    /// But if its RUN was retired this walk (a glyph of it replaced), or
    /// released with this cell inside what the release takes
    /// ([`Witness::released_span_takes`]), the glyph under it came with the
    /// rewrite that took the run, not with a key: the cell is the run's
    /// blank, and it leaves on the run's own verdict — with a retired run on
    /// the melt (`retire`, its fresh record dropped) when no standing glyph
    /// of the run lies beyond it ([`Witness::retired_far_side_clear`]), with
    /// a released run on the swoosh (`release`, its record kept and
    /// marked). That is the "blanks go with their run" law below, which the
    /// arm had hidden by giving the cell a record in the same walk.
    ///
    /// Measured at the host seam (`tests/composer_growth_follows.rs`): a
    /// composer wrap that moves a word at least as long as the line it
    /// keeps relays that word over the column of the Space the wrap ate.
    /// That Space's cell, left on the caret row in the old line's run by
    /// the follow split, was armed under the relaid glyph and STOOD while
    /// every other cell of its run melted — and `Ribbon::join_cohort` then
    /// laid the new line into that run from its own anchor: at 60 columns
    /// `see` and a path relaid from `t` 0.000 on the fast leg, the row
    /// above's stops column for column, where the path's first letter had
    /// 0.250. A key's own cell is never on [`Witness::blank_seen`] on the
    /// walk that first reads it, so it is armed exactly as before; so is a
    /// landed glyph whose run keeps its text.
    fn landed_goes_with_its_run(
        &mut self,
        cells: &[Cell],
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) {
        for k in 0..self.landed.len() {
            let (row, col, born, cohort) = self.landed[k];
            let run = (row, cohort);
            if self.retired_runs.contains(&run) {
                if !self.retired_far_side_clear(cells, retire, run, col) {
                    continue;
                }
                retire.push((row, col, born));
                if let Some(i) = self.find(row, col, born) {
                    self.seen[i].live = false;
                }
            } else if self.released_runs.contains(&run)
                && self.released_span_takes(cells, release, run, col)
            {
                // The run's OWN verdict: a released run leaves on the
                // swoosh, and its landed blank rides it — released, its
                // record kept and marked below like its run-mates'.
                release.push((row, col, born));
                self.fresh_released.push((row, col, born));
            }
        }
    }

    /// Whether a landed cell at `col` of the RETIRED `run` is the run's loose
    /// blank rather than a cell between text that still stands (2026-09-24,
    /// the review of [`Witness::landed_goes_with_its_run`]): inside the span
    /// the walk named, or outside it with no standing glyph of the run
    /// beyond it — the rule [`Witness::released_span_takes`] reads for a
    /// released run. `ab cd ef` → `ab*cd eX` names only `f`'s cell; the `*`
    /// on the Space has `ab` standing beyond it and stays out of the melt,
    /// where taking it let the span closure melt `cd e` with it. The span is
    /// read off the cells named so far, never a landed cell's own push, so
    /// one landed cell cannot widen it for another.
    fn retired_far_side_clear(
        &self,
        cells: &[Cell],
        retire: &[(u16, u16, Instant)],
        run: (u16, u32),
        col: u16,
    ) -> bool {
        let mut lo = u16::MAX;
        let mut hi = 0u16;
        for cell in cells {
            if (cell.row, cell.cohort) != run
                || !retire.contains(&(cell.row, cell.col, cell.born))
                || self
                    .landed
                    .iter()
                    .any(|l| (l.0, l.1, l.2) == (cell.row, cell.col, cell.born))
            {
                continue;
            }
            lo = lo.min(cell.col);
            hi = hi.max(cell.col);
        }
        if lo > hi || (lo <= col && col <= hi) {
            return true;
        }
        let standing = self.standing_glyphs_of(run);
        if col < lo {
            standing.first().is_none_or(|s| s.2 > col)
        } else {
            standing.last().is_none_or(|s| s.2 < col)
        }
    }

    /// **A RUN WHOSE NEIGHBOUR WAS WITHHELD IS NOT JUDGED THIS WALK**
    /// (2026-09-25 — the composed host's dropped far-row read). `withheld`
    /// are the rows the host was ASKED for this frame and did not deliver,
    /// inside the grid and the focused pane (`Engine::follow_rows`): the
    /// split or zoomed host reads the rows past the caret's
    /// `±1` in a second, generation-checked lock and drops them when a PTY
    /// chunk landed between. A run on a row next to one of them may have
    /// moved there — the follow pass could not look — and its glyphs read
    /// as REPLACED on its own row: Claude Code's composer at a Shift+Enter,
    /// the first list line moved up past the caret's `−1`
    /// (`tests/moved_again_without_a_key.rs`), melted under its own text.
    /// So a run on a row whose `±1` was withheld, and which the follow pass
    /// COULD carry — two of its armed glyphs gone from under it, and no
    /// fewer than half ([`Witness::follow_runs`]) — is put on
    /// [`Witness::deferred_runs`] and the walk passes no verdict on it: the
    /// next frame's follow pass sees the row. A change the follow pass
    /// could never carry is judged at once, as ever — one interior glyph
    /// rewritten stays the shape pass's to forgive
    /// (`Witness::shape_verdicts`), and stacking it onto the next frame's
    /// change would read the two as one suffix. At most ONCE: a run with a
    /// changed record already deferred ([`Seen::deferred`]) is judged, so a
    /// host that never delivers the row costs a stray one frame, not its
    /// whole life. Nothing is scanned when nothing was withheld; otherwise
    /// `O(cells × log records)` into scratch reserved at [`WITNESS_CAP`] by
    /// [`Witness::new`] — at most one entry per record — so the first
    /// withheld frame, however late, allocates nothing.
    fn find_deferred_runs(&mut self, cells: &[Cell], rows: &[RowSample<'_>], withheld: &[u16]) {
        self.blind_changed.clear();
        self.deferred_runs.clear();
        if withheld.is_empty() {
            return;
        }
        for cell in cells {
            if cell.leaving() {
                continue;
            }
            let blind = [cell.row.checked_sub(1), cell.row.checked_add(1)]
                .into_iter()
                .flatten()
                .any(|r| withheld.contains(&r));
            if !blind {
                continue;
            }
            let Some(sample) = rows.iter().find(|s| s.row == cell.row) else {
                continue;
            };
            let Some(i) = self.find(cell.row, cell.col, cell.born) else {
                continue;
            };
            let seen = self.seen[i];
            if seen.released {
                continue;
            }
            let changed = unit_at(sample.cols, cell.col) != seen.unit;
            self.blind_changed.push(BlindRecord {
                row: cell.row,
                cohort: cell.cohort,
                col: cell.col,
                changed,
                deferred: changed && seen.deferred,
            });
        }
        self.blind_changed
            .sort_unstable_by_key(|b| (b.row, b.cohort, b.col));
        self.blind_changed
            .dedup_by_key(|b| (b.row, b.cohort, b.col));
        let mut k = 0;
        while k < self.blind_changed.len() {
            let run = (self.blind_changed[k].row, self.blind_changed[k].cohort);
            let end = k + self.blind_changed[k..]
                .iter()
                .take_while(|b| (b.row, b.cohort) == run)
                .count();
            let records = &self.blind_changed[k..end];
            let changed = records.iter().filter(|b| b.changed).count();
            // Only a run the follow pass could carry waits for it
            // ([`Witness::follow_runs`]'s first test): two of its glyphs
            // gone, and no fewer than half.
            if changed >= 2 && changed * 2 >= records.len() && !records.iter().any(|b| b.deferred) {
                self.deferred_runs.push(run);
            }
            k = end;
        }
    }

    /// **A RUN LOSES A PREFIX, A SUFFIX, OR ALL OF ITSELF — NEVER ITS MIDDLE**
    /// (2026-09-16 — the owner: *"there is still this gapping issue that
    /// arises in codex, it seems to happen when doing backword and
    /// editing"*, and, on the fix's first cut: *"I'm not convinced that this
    /// is a Codex specific issue … you need to be fixing in general"*).
    ///
    /// [`Ribbon::retract_suffix`] states how light may leave a row —
    /// *"CONTIGUOUS at every frame … nothing is ever removed from the
    /// MIDDLE"* — and every geometric path obeys it. The witness is the one
    /// path that removes light by NAME, and until this pass a name anywhere
    /// in a run was honoured: one interior cell whose glyph changed, or
    /// went, punched a one-cell hole in a band whose letters stood lit either
    /// side of it. The 2026-09-16 span closure below closed the holes
    /// BETWEEN names; a lone name stayed a hole "by design".
    ///
    /// What names a lone interior cell, measured: an app painting a
    /// decoration into a blank cell of its composer and moving it on — Codex
    /// 0.154's ambient particle field, a dot drifting through the space
    /// between two typed words, the cell under a parked caret cleared and
    /// painted again (`tests/codex_particle_replay.rs`, its real bytes: a
    /// one-cell hole exactly on each space a word hop had parked the caret
    /// on, and — the same dot found nowhere else that frame — the whole run
    /// RELEASED and the band falling from 33 lit cells to 9 while the hand
    /// was still typing). A spinner glyph, a spellcheck rewriting one letter,
    /// a TUI drawing its own cursor glyph, a line-number gutter ticking: the
    /// same shape, none of them Codex.
    ///
    /// The law. For a run that is STANDING — some cell of it holds a record
    /// that is neither released nor named this walk — the names are read as
    /// ONE SHAPE against the run's recorded extent (its cells with records;
    /// never-armed spaces are not ends, they go with their run):
    ///
    /// * names that reach the run's first or last recorded cell are a
    ///   prefix, a suffix or the whole, and stand as named — the shift a
    ///   mid-line Backspace or insert makes, the tail an app cleared, the
    ///   line it rewrote;
    /// * names strictly inside that cover MOST of the run's recorded cells
    ///   are a rewrite whose end cell happened to compare equal (`ll`, `ee`
    ///   — natural text repeats) and are EXTENDED to the nearer end, so no
    ///   speck is left standing past them;
    /// * names strictly inside that cover less are NOT EVIDENCE: the light
    ///   stays, and the records take the glyphs standing there now, so the
    ///   walk does not name them again next frame for the same reason. A
    ///   cell that went blank records a blank; a glyph landing on it later
    ///   is another interior change, and stays. **A RECORD CAUGHT UP IS
    ///   ARMED AGAIN** (2026-09-25): what its neighbours were seen holding
    ///   was about the glyph it no longer claims, so its evidence is
    ///   forgotten ([`Seen::forget_evidence`]) — and this walk is the new
    ///   glyph's arming frame ([`Seen::observe`] with `at_arm`): a copy of
    ///   it standing beside the line NOW was there first and is a twin at
    ///   once. A typo fixed in place (Ctrl-T, two vi `r`s) in a line typed
    ///   under the previous command makes it that command again, and a
    ///   Ctrl-U a moment later is an erase under a twin, not a move
    ///   (`a_record_the_shape_pass_caught_up_is_armed_again`,
    ///   `tests/copies_are_not_moves.rs`). Forgotten and learned again only
    ///   by time, the evidence would carry the band onto the previous command
    ///   for [`TWIN_MIN`] after the catch-up. Keeping the OLD glyph's evidence
    ///   instead is wrong the other way: its twins refuse the arrival of the
    ///   glyphs a transpose changed.
    ///
    /// A run with nothing standing — every record released, its text gone,
    /// its light on the retract — is not shaped: a glyph landing under it
    /// is replaced text and the run melts as it always did.
    fn shape_verdicts(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        now: Instant,
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) {
        self.shape_runs.clear();
        self.shape_runs.extend_from_slice(&self.retired_runs);
        self.shape_runs.extend_from_slice(&self.released_runs);
        self.shape_runs.sort_unstable();
        self.shape_runs.dedup();
        for k in 0..self.shape_runs.len() {
            let run = self.shape_runs[k];
            let (row, cohort) = run;
            let mut lo_r = u16::MAX;
            let mut hi_r = 0u16;
            let mut recorded = 0usize;
            let mut lo_n = u16::MAX;
            let mut hi_n = 0u16;
            let mut standing = false;
            self.run_ids.clear();
            for cell in cells {
                if cell.leaving() || cell.row != row || cell.cohort != cohort {
                    continue;
                }
                let id = (cell.row, cell.col, cell.born);
                let Some(i) = self.find(cell.row, cell.col, cell.born) else {
                    continue;
                };
                recorded += 1;
                lo_r = lo_r.min(cell.col);
                hi_r = hi_r.max(cell.col);
                if retire.contains(&id) || release.contains(&id) {
                    self.run_ids.push(id);
                    lo_n = lo_n.min(cell.col);
                    hi_n = hi_n.max(cell.col);
                } else if !self.seen[i].released {
                    standing = true;
                }
            }
            if self.run_ids.is_empty() || !standing || lo_n <= lo_r || hi_n >= hi_r {
                continue;
            }
            let inside = cells
                .iter()
                .filter(|c| {
                    !c.leaving()
                        && c.row == row
                        && c.cohort == cohort
                        && (lo_n..=hi_n).contains(&c.col)
                        && self.find(c.row, c.col, c.born).is_some()
                })
                .count();
            if inside * 2 > recorded {
                // A rewrite of most of the run: extend to the nearer end.
                let (a, b) = if lo_n - lo_r <= hi_r - hi_n {
                    (lo_r, hi_n)
                } else {
                    (lo_n, hi_r)
                };
                let releasing = !self.retired_runs.contains(&run);
                for cell in cells {
                    if cell.leaving()
                        || cell.row != row
                        || cell.cohort != cohort
                        || !(a..=b).contains(&cell.col)
                    {
                        continue;
                    }
                    let id = (cell.row, cell.col, cell.born);
                    if retire.contains(&id) || release.contains(&id) {
                        continue;
                    }
                    if releasing {
                        release.push(id);
                        self.fresh_released.push(id);
                    } else {
                        retire.push(id);
                    }
                }
                continue;
            }
            // Not evidence: the names come off, the records catch up.
            retire.retain(|id| !self.run_ids.contains(id));
            release.retain(|id| !self.run_ids.contains(id));
            self.fresh_released.retain(|id| !self.run_ids.contains(id));
            self.retired_runs.retain(|r| *r != run);
            self.released_runs.retain(|r| *r != run);
            let sample = rows.iter().find(|s| s.row == row);
            let nb = neighbours_of(rows, row);
            for j in 0..self.run_ids.len() {
                let (r, c, born) = self.run_ids[j];
                if let Some(i) = self.find(r, c, born) {
                    self.seen[i].unit = sample.map_or(Unit::BLANK, |s| unit_at(s.cols, c));
                    self.seen[i].live = true;
                    // What its neighbours were seen holding was about the
                    // glyph the record no longer claims: the record is ARMED
                    // AGAIN on the glyph it takes, and this frame is its
                    // arming frame — a copy of the NEW glyph standing beside
                    // it now was there first, a twin at once.
                    self.seen[i].forget_evidence();
                    self.seen[i].observe(&nb, true, now);
                }
            }
        }
    }

    /// Whether the never-armed cell at `col` of the released `run` goes with
    /// the release. A run with NO recorded glyph standing lost its text
    /// whole: yes, as always. A run with glyphs still standing lost only
    /// PART, and takes the cell in two shapes — **the span**: `col` lies
    /// strictly inside the span the run is losing, between two of its cells
    /// this walk named on `release` or that an earlier partial release
    /// stamped onto the retract (`Cell::released_at`); and **the tail**
    /// (2026-09-21, the review's host-seam probe): `col` lies past the span
    /// with no standing recorded glyph of the run beyond it on its side —
    /// right of the span's `hi` with none standing right of `hi`, left of
    /// its `lo` with none standing left of `lo`. Under that rule the
    /// interior space of `hello world` with one glyph blanked stands
    /// (glyphs stand on both sides), the typed trailing space after a
    /// cleared `world` goes with the suffix (nothing stands right of it),
    /// and the space before a cleared suffix stands because `hello` stands
    /// left of it. An event's cost, not a frame's.
    fn released_span_takes(
        &self,
        cells: &[Cell],
        release: &[(u16, u16, Instant)],
        run: (u16, u32),
        col: u16,
    ) -> bool {
        let standing = self.standing_glyphs_of(run);
        if standing.is_empty() {
            return true;
        }
        let mut lo = u16::MAX;
        let mut hi = 0u16;
        for cell in cells {
            if (cell.row, cell.cohort) != run {
                continue;
            }
            let lost = (cell.leaving() && cell.released_at.is_some())
                || release.contains(&(cell.row, cell.col, cell.born));
            if lost {
                lo = lo.min(cell.col);
                hi = hi.max(cell.col);
            }
        }
        if lo > hi {
            return false;
        }
        let inside = lo < col && col < hi;
        // `standing` is column-sorted: its last entry is the rightmost
        // standing glyph, its first the leftmost.
        let tail =
            (col > hi && standing[standing.len() - 1].2 <= hi) || (col < lo && standing[0].2 >= lo);
        inside || tail
    }

    /// The standing recorded glyphs of `run` this walk, column-sorted — a
    /// contiguous slice of the sorted [`Witness::standing_glyphs`].
    fn standing_glyphs_of(&self, run: (u16, u32)) -> &[(u16, u32, u16)] {
        let lo = self
            .standing_glyphs
            .partition_point(|&(r, c, _)| (r, c) < run);
        let hi = self
            .standing_glyphs
            .partition_point(|&(r, c, _)| (r, c) <= run);
        &self.standing_glyphs[lo..hi]
    }

    /// Find the retired runs with something left standing in one pool walk.
    /// Only runs with no surviving armed cell take their unchanged spaces.
    /// Previously each retired run rescanned the pool and each candidate
    /// linearly searched all retirement identities, multiplying the work on
    /// a composer's partial repaint. Both membership checks now use sorted
    /// resident scratch; the verdict's order and cell identities are untouched.
    fn find_standing_runs(&mut self, cells: &[Cell], retire: &[(u16, u16, Instant)]) {
        self.standing_runs.clear();
        self.retired_cells.clear();
        if self.retired_runs.is_empty() {
            return;
        }
        self.retired_runs.sort_unstable();
        self.retired_cells.extend_from_slice(retire);
        self.retired_cells.sort_unstable();
        for cell in cells {
            let run = (cell.row, cell.cohort);
            if !cell.leaving()
                && self.retired_runs.binary_search(&run).is_ok()
                && self.find(cell.row, cell.col, cell.born).is_some()
                && self
                    .retired_cells
                    .binary_search(&(cell.row, cell.col, cell.born))
                    .is_err()
            {
                self.standing_runs.push(run);
            }
        }
        self.standing_runs.sort_unstable();
        self.standing_runs.dedup();
    }

    /// Releases whose complete original cell identities and glyphs are
    /// back, as `(cohort, stamp)`, sorted: `stamp` is the
    /// [`Cell::released_at`] one PARTIAL release laid on its cells, `None`
    /// for the cohort's unstamped cells — a WHOLE release. Each partial
    /// release is judged on its own cells (2026-09-22): one whose text
    /// never came back as it was — an insert's shifted tail — no longer
    /// vetoes a later one whose glyphs are back. The first pass indexes
    /// distinct candidates; the second visits each resident cell once and
    /// checks its record. No per-cell full-pool scan. Missing
    /// samples/records and partial restores cannot authorize recovery.
    /// Whether every released CELL is still resident is the ribbon's to
    /// count (`Ribbon::restore_releases`): a cell laid after the release is
    /// no part of what was released, here or there. A record armed after
    /// the release carries `released_at: None`, so it is read with the
    /// cohort's UNSTAMPED cells, and there it is neutral unless it sits
    /// over a column a released record holds (2026-09-21, the arm below) —
    /// a retype over released text, whose new light the old must not come
    /// back under.
    pub(super) fn restored_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        out: &mut Vec<(u32, Option<Instant>)>,
    ) {
        out.clear();
        self.restore_runs.clear();
        if !self.seen.iter().any(|s| s.restorable) {
            return;
        }
        for cell in cells {
            if !self
                .find(cell.row, cell.col, cell.born)
                .is_some_and(|i| self.seen[i].restorable)
            {
                continue;
            }
            self.restore_runs.push(RestoreRun {
                row: cell.row,
                cohort: cell.cohort,
                stamp: cell.released_at,
                exact: true,
                returned_ink: false,
            });
        }
        self.restore_runs
            .sort_unstable_by_key(|r| (r.cohort, r.stamp));
        self.restore_runs.dedup_by_key(|r| (r.cohort, r.stamp));
        for cell in cells {
            let Ok(i) = self
                .restore_runs
                .binary_search_by_key(&(cell.cohort, cell.released_at), |r| (r.cohort, r.stamp))
            else {
                continue;
            };
            let seen = self
                .find(cell.row, cell.col, cell.born)
                .map(|j| self.seen[j]);
            let sample = rows.iter().find(|r| r.row == cell.row);
            // **A KEY TYPED INSIDE THE MELT IS NOT A VETO** (2026-09-21 —
            // the owner's dark cells at phrase boundaries). A record armed
            // AFTER the release — the cell the hand laid past the stamped
            // ones while the composer's rewrite was still owed, tagged by
            // no release walk — makes no claim on the run's OLD text: it
            // neither authorizes nor refuses, whether its own glyph stands
            // or was rewritten since (a changed glyph is the walk's to
            // retire on its own — the walk runs right after this restore,
            // `Engine::witness_rows` — so it is NEUTRAL here, not a veto;
            // 2026-09-21, the review). Read as a refusal (the tag was
            // required of every record) it made a partial release
            // UNRESTORABLE the moment the next key landed, and the stamped
            // glyph melted under text that was back. What still refuses is
            // a record armed over a column a RELEASED record holds: that is
            // the retype over released text the tag loop names ("a later
            // retype arms an untagged new birth"), and the old light must
            // not come back under the new.
            let over_released = seen.is_some_and(|s| !s.restorable)
                && self
                    .seen
                    .iter()
                    .any(|s| s.row == cell.row && s.col == cell.col && s.released);
            let run = &mut self.restore_runs[i];
            match (seen, sample) {
                // **A PART COMES BACK WITH ITS ROW** (2026-09-22). A PARTIAL
                // release was a verdict on a BLANK — the tail a torn read
                // cut off — and nothing else. Once one glyph it released is
                // back exactly, the torn present is over, and the part is
                // lifted whole:
                // the walk that follows on this same sample then reads its
                // cells as the live cells they were, and gives exactly the
                // verdict the complete frame would have had without the
                // torn one — the same glyph stands (D2), a different one is
                // named and goes through the shape law like any other.
                // Demanding every glyph exactly back held the part hostage
                // to text that comes back MOVED: a torn read under a
                // mid-line insert releases the letters just typed with the
                // tail the insert pushes right, the tail returns one column
                // on, and the letters — back under their own light — were
                // never lifted and retracted out of the middle of the band
                // (the owner's hole under `INTO`, measured at the host seam
                // for 40 frames from ONE torn present). A never-armed cell
                // (a released blank, the part's spaces) constrains nothing.
                //
                // **AND A COLUMN STILL BLANK IS TEXT THAT REALLY WENT**
                // (2026-09-22, the review round). The lift also demanded
                // that no released glyph be under a blank. A Backspace or
                // ⌃W erases one for real, and its column is blank on the
                // COMPLETE present too — so a torn edit's part was never
                // lifted, and the standing text right of the boundary
                // retracted while its glyph stood: 38 cells dark within
                // 160 ms, a 29-cell hole for ~1 s once the hand typed on.
                // The clause is gone. Nothing is lost by dropping it,
                // because the walk runs on this very sample straight after
                // the lift and names a column that is still blank, as a run
                // of its own on its own clock — which is precisely what
                // lifting the part is FOR. A part lifted over a frame that
                // is still incomplete is simply re-released the same frame.
                // Measured in `tests/torn_edit_gaps.rs`.
                (Some(seen), Some(sample)) if run.stamp.is_some() => {
                    let now = unit_at(sample.cols, cell.col);
                    run.exact &= seen.restorable && run.row == cell.row;
                    run.returned_ink |= seen.released && !seen.unit.is_blank() && seen.unit == now;
                }
                (Some(seen), Some(sample)) => {
                    let standing = seen.unit == unit_at(sample.cols, cell.col);
                    run.exact &= if seen.restorable {
                        (!cell.leaving() || cell.released_at.is_some())
                            && run.row == cell.row
                            && standing
                    } else {
                        !over_released
                    };
                    run.returned_ink |= seen.released && !seen.unit.is_blank();
                }
                // **A CELL WITH NO RECORD IS NOT EVIDENCE** (2026-09-15) —
                // the restore path's half of the blank law. A never-armed
                // cell holds no record because nothing was ever under it:
                // the SPACE in `hello world`, and the cell a key laid while
                // the app's repaint still had the column blank. It makes no
                // claim about the run's text, so it can neither authorize a
                // restore nor refuse one — only recorded cells decide. Read
                // as a refusal it made a run with a space in it
                // UNRESTORABLE for good: a composer that clears to the end
                // of the screen and rewrites releases the row, the restore
                // is vetoed by the run's own spaces, and every glyph that
                // lands back under the released light is then read as
                // REPLACED text and retired — one cell at a time, for the
                // rest of the run. Measured 2026-09-15 (`c2_bs`): `witness
                // REPLACED (13,7) was=' ' now='s'`, a one-cell hole that
                // stood 1.42 s of a 16 s take.
                //
                // A cell whose ROW was not sampled is a different thing and
                // still refuses: there the witness genuinely does not know.
                (None, Some(_)) => {}
                _ => run.exact = false,
            }
        }
        out.extend(
            self.restore_runs
                .iter()
                .filter(|r| r.exact && r.returned_ink)
                .map(|r| (r.cohort, r.stamp)),
        );
    }

    /// Clear release custody only after the ribbon accepted restoration.
    /// Already-counted identities stay counted if they are cleared again.
    /// A cell still LEAVING in an accepted cohort belongs to another
    /// partial release the ribbon did not lift (2026-09-22: each part is
    /// restored on its own) and keeps its custody record for that one.
    pub(super) fn restored(&mut self, cohorts: &[u32], cells: &[Cell]) {
        if cohorts.is_empty() {
            return;
        }
        for cell in cells
            .iter()
            .filter(|c| !c.leaving() && cohorts.binary_search(&c.cohort).is_ok())
        {
            if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                // A HELD BLANK GOES BACK TO NEVER-ARMED (2026-09-16). The
                // release gave the run's spaces a released blank record of
                // their own (`hold_released`); un-releasing that record
                // would leave a space with a BLANK record no walk can ever
                // name — blank over blank is D2 — so a later clear of the
                // whole line could never name every cell of the run and
                // `Ribbon::release_cells` would read the text gone whole as
                // gone in part. A blank arms nothing: the record goes.
                if self.seen[i].unit.is_blank() {
                    self.seen.remove(i);
                } else {
                    self.seen[i].released = false;
                    self.seen[i].restorable = false;
                }
            }
        }
    }

    /// **THE MOVED-TEXT SEARCH.** True when a run's blanked glyphs — sorted
    /// by column, `run` — stand in the same order at the same spacing
    /// somewhere else on the sampled rows: on another row at any offset, or
    /// on their own row at another offset. Never-armed cells (a run's
    /// spaces) are not in `run` and so constrain nothing, which is what lets
    /// `hello world` re-laid two rows down match. The one place excluded is
    /// where the glyphs were — blank now by definition. `O(rows × cols ×
    /// glyphs)`, and a run is at most a row wide: an event's cost, not a
    /// frame's, and it allocates nothing.
    ///
    /// **ONE GLYPH IS NOT A WORD** (2026-09-22). A run of ONE blanked glyph
    /// is never ruled moved: a single letter stands somewhere on almost any
    /// sampled row, so its "match" is coincidence, not the text going
    /// elsewhere. Measured at the host seam: the first key of a line, its
    /// run blanked whole by a spinner repaint whose first 1024-byte read
    /// ended before the composer row was written, matched the `z` on the
    /// row the frame was writing and took the melt — the band started one
    /// cell late for the rest of the line. A one-glyph run whose text went
    /// is released, and leaves through the retract.
    ///
    /// GLYPHS, NOT CELLS (2026-09-22, the review round). The floor counted
    /// `run.len()`, which is CELLS, and a wide glyph owns two of them — so
    /// one CJK character cleared the floor and took the melt on the very
    /// coincidence the floor was written against, in the languages where
    /// one glyph really is a word. The continuation half of a wide cell is
    /// not a glyph and is not counted.
    fn moved(run: &[Blanked], rows: &[RowSample<'_>]) -> bool {
        if run.iter().filter(|b| !b.unit.cont).count() < 2 {
            return false;
        }
        let Some(first) = run.first() else {
            return false;
        };
        let (row, c0) = (first.row, usize::from(first.col));
        rows.iter().any(|s| {
            (0..s.cols.len()).any(|d| {
                if s.row == row && d == c0 {
                    return false;
                }
                run.iter().all(|b| {
                    let at = usize::from(b.col) - c0 + d;
                    u16::try_from(at).is_ok_and(|col| unit_at(s.cols, col) == b.unit)
                })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::ribbon::Layer;
    use super::*;

    fn cell(row: u16, col: u16, born: Instant) -> Cell {
        cell_of(row, col, born, 0)
    }

    fn cell_of(row: u16, col: u16, born: Instant, cohort: u32) -> Cell {
        Cell {
            row,
            col,
            cohort,
            t: 0.0,
            born,
            attack_at: born,
            life_s: 1.0,
            cov0: 1.0,
            typing: true,
            retract_at: None,
            retire_at: None,
            birth_disp: 0.5,
            edge_cells: 3.0,
            layer: Layer::Base,
            rearm: None,
            released_at: None,
        }
    }

    fn row(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// The positions a walk named, in the order it named them.
    fn pos(out: &[(u16, u16, Instant)]) -> Vec<(u16, u16)> {
        out.iter().map(|&(r, c, _)| (r, c)).collect()
    }

    #[test]
    fn a_unit_is_the_lead_glyph_for_either_half_of_a_wide_one() {
        let cols = row("a\u{4f60}\0 ");
        assert_eq!(
            unit_at(&cols, 0),
            Unit {
                ch: 'a',
                wide: false,
                cont: false
            }
        );
        assert_eq!(
            unit_at(&cols, 1),
            Unit {
                ch: '\u{4f60}',
                wide: true,
                cont: false
            }
        );
        assert_eq!(
            unit_at(&cols, 2),
            Unit {
                ch: '\u{4f60}',
                wide: true,
                cont: true
            }
        );
        assert!(unit_at(&cols, 3).is_blank(), "a space is blank");
        assert!(unit_at(&cols, 9).is_blank(), "past the sample is blank");
        assert!(
            unit_at(&row("\0x"), 0).is_blank(),
            "a stray continuation is blank"
        );
    }

    /// Six cells of one run on row 5, armed over `abcdef` with row 4
    /// sampled holding `above` at the arming walk.
    fn armed_run(above: &str) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("abcdef"),
                },
                RowSample {
                    row: 4,
                    cols: &row(above),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        assert_eq!(w.len(), 6, "every glyph armed");
        (w, cells)
    }

    /// The follow pass's verdict once row 5 is blank and row 4 reads `now`.
    fn follow_onto(w: &mut Witness, cells: &[Cell], now: &str) -> Vec<FollowRun> {
        follow_onto_with(w, cells, "      ", now)
    }

    /// The follow pass's verdict once row 5 reads `own` and row 4 reads
    /// `now`.
    fn follow_onto_with(w: &mut Witness, cells: &[Cell], own: &str, now: &str) -> Vec<FollowRun> {
        let mut out = Vec::new();
        w.follow_runs(
            cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row(own),
                },
                RowSample {
                    row: 4,
                    cols: &row(now),
                },
            ],
            &mut out,
        );
        out
    }

    /// Row 5's run followed one row up, over `lo..=hi`.
    fn up(lo: u16, hi: u16) -> FollowRun {
        FollowRun {
            row: 5,
            cohort: 7,
            lo,
            hi,
            dr: -1,
        }
    }

    #[test]
    fn a_short_park_cannot_relax_an_unrelated_run() {
        let sample = [
            RowSample {
                row: 5,
                cols: &row("bcdef       "),
            },
            RowSample {
                row: 4,
                cols: &row("a           "),
            },
        ];
        let (mut ordinary, cells) = armed_run("            ");
        let mut out = Vec::new();
        ordinary.follow_runs(&cells, &sample, &mut out);
        assert!(out.is_empty(), "one arrival cannot move an ordinary run");

        let (mut unrelated, cells) = armed_run("            ");
        out.clear();
        unrelated.follow_runs_with_short_park(&cells, &sample, Some(((5, 12), (5, 11))), &mut out);
        assert!(
            out.is_empty(),
            "an unrelated park at col 12 must not relax the run at cols 0..5: {out:?}"
        );

        let (mut owned, cells) = armed_run("            ");
        out.clear();
        owned.follow_runs_with_short_park(&cells, &sample, Some(((5, 6), (5, 5))), &mut out);
        assert_eq!(out, vec![up(0, 0)], "the exact run keeps the narrow wrap");
    }

    /// **A TWIN IS NEUTRAL** (2026-09-23, [`Witness::follow_runs`] — the
    /// owner: *"the existing line rainbow should beautifully flow and drift
    /// and fade away, not simply abruptly vanish"*). A glyph that already
    /// stood a row up when its record was armed proves nothing either way
    /// once the run's text arrives there: it neither counts as found nor
    /// breaks the block. `  c   ` a row up at arm time (the `c` under the
    /// `c`, the owner's own `WHY` under `HEY`) and the whole run arriving
    /// there is ONE block that moved; so are twins at both ends and a
    /// block of four twins between two arrivals.
    ///
    /// RED before 2026-09-23: the twin counted as not found, broke the
    /// block, and the run was refused at every offset — on the composer's
    /// second and later growths the whole row melted in `RETIRE_MELT_S`.
    #[test]
    fn a_twin_inside_the_block_is_neutral_and_the_block_follows() {
        for above in ["  c   ", "a    f", " bcde "] {
            let (mut w, cells) = armed_run(above);
            assert_eq!(
                follow_onto(&mut w, &cells, "abcdef"),
                vec![up(0, 5)],
                "armed under `{above}`: the whole run arrived a row up and follows as one block"
            );
        }
    }

    /// **…BUT A TWIN WHOSE GLYPH IS NOT AT THE TARGET IS A HOLE**
    /// (2026-09-23): the neutral twin is one standing where the run's text
    /// ARRIVED. A twin record whose glyph is missing from the target row is
    /// a hole in the block like any other missing record — `ab def` and
    /// `ab  ef` a row up are letters either side of a hole, not a move.
    #[test]
    fn a_twin_whose_glyph_is_not_at_the_target_is_a_hole() {
        for now in ["ab def", "ab  ef"] {
            let (mut w, cells) = armed_run("  c   ");
            assert_eq!(
                follow_onto(&mut w, &cells, now),
                vec![],
                "`{now}` a row up: a hole in the middle is not a block that moved"
            );
        }
    }

    /// **A MISSED FOLLOW IS COUNTED** (2026-09-23, [`FollowScore::missed`],
    /// `ribbon_follow_missed=`). Row 5's six glyphs gone and `abc  f` a row
    /// up: four of them ARRIVED, but not as one block, so nothing is named
    /// and the content witness will melt the run — the pass says so,
    /// counting the six gone cells. A run it names misses nothing, and a
    /// run whose text arrived nowhere misses nothing either: an erase under
    /// an identical line (every record a twin), a kill under a line sharing
    /// only a prefix (the twins neutral, the rest holes) and other text a
    /// row up.
    #[test]
    fn a_run_whose_text_arrived_but_not_as_one_block_is_counted_missed() {
        let score = |above: &str, own: &str, now: &str| {
            let (mut w, cells) = armed_run(above);
            let mut out = Vec::new();
            let score = w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &row(own),
                    },
                    RowSample {
                        row: 4,
                        cols: &row(now),
                    },
                ],
                &mut out,
            );
            (out.len(), score.missed)
        };
        let blank = "      ";
        for (above, own, now, want, what) in [
            (blank, blank, "abc  f", (0, 6), "arrived, not a block"),
            (blank, blank, "abcdef", (1, 0), "named: nothing missed"),
            ("  c   ", blank, "abcdef", (1, 0), "the twin is neutral"),
            ("abcdef", blank, "abcdef", (0, 0), "an erase is not a move"),
            ("abc   ", blank, "abcxyz", (0, 0), "a prefix twin and holes"),
            (blank, blank, "uvwxyz", (0, 0), "other text arrived nothing"),
            (blank, "abcdef", "abcdef", (0, 0), "nothing left its row"),
        ] {
            assert_eq!(score(above, own, now), want, "{what}");
        }
    }

    /// **AN EDGE TWIN RIDES WITH ITS BLOCK, GONE OR STANDING** (2026-09-23,
    /// RE-PINNED ON PURPOSE by the review of the twin-neutral rule, the
    /// spec's test 3 inverted). A neutral record FLUSH against the found
    /// block — a lead of them directly before its first found record, a
    /// trail directly after its last — rides with the block whether or not
    /// its own glyph still reads as standing on the run's row. The first
    /// cut kept a standing edge twin behind "with its glyph" (`a     ` on
    /// its own row named `1..=5`), and in a bottom-anchored composer that
    /// cannot be told from a NEW line starting with the same letter: Ink
    /// re-lays the caret row with the moved word, so where three rows start
    /// alike (`HEY` / `HOW` / the moved `HIS` — ordinary English) the kept
    /// line's first cell read as standing, stayed on the caret row, and the
    /// run it was split into — standing again, its bounds reaching the old
    /// line's end — captured every later key of the new line: the review
    /// measured row 27's cells 4..14 re-walking row 26's colours column for
    /// column (`t` 2.94…3.22 under 2.94…3.22), a folded jump of 0.194 where
    /// the control's worst is 0.028, at 90 columns and 60 ms keys
    /// (`tests/composer_growth_follows.rs`). Leaving one cell behind costs
    /// the whole new line; carrying a glyph that truly never left costs one
    /// cell's light. Only a FLUSH edge rides: a neutral cut off from the
    /// block by a hole stays, and the half-gate never counts it either way.
    ///
    /// RED before the re-pin: `a     ` on its own row gave `1..=5`, and its
    /// trailing mirror `     f` gave `0..=4` (the trailing half of the old
    /// rule, unpinned until the review: `gone && trail_open` → `trail_open`
    /// survived the whole suite).
    #[test]
    fn an_edge_twin_rides_with_its_block_whether_or_not_its_own_glyph_is_gone() {
        for above in ["a     ", "     f"] {
            for own in [above, "      "] {
                let (mut w, cells) = armed_run(above);
                assert_eq!(
                    follow_onto_with(&mut w, &cells, own, "abcdef"),
                    vec![up(0, 5)],
                    "armed under `{above}`, its row reading `{own}`: the edge twin rides"
                );
            }
        }
        // …only FLUSH: a hole between the twin and the block leaves it.
        for (above, now, want) in [
            ("a     ", "a cdef", up(2, 5)),
            ("     f", "abcd f", up(0, 3)),
        ] {
            for own in [above, "      "] {
                let (mut w, cells) = armed_run(above);
                assert_eq!(
                    follow_onto_with(&mut w, &cells, own, now),
                    vec![want],
                    "armed under `{above}`, `{now}` a row up: a twin past a hole is not the block's"
                );
            }
        }
    }

    /// Six-plus cells of one run on row 5, one per column of `text` (its
    /// blanks laid and never armed, as a typed Space is), armed with row 4
    /// sampled holding `above`.
    fn armed_line_under(text: &str, above: &str) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let n = u16::try_from(text.chars().count()).expect("a short line");
        let cells: Vec<Cell> = (0..n).map(|c| cell_of(5, c, t0, 7)).collect();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row(text),
                },
                RowSample {
                    row: 4,
                    cols: &row(above),
                },
            ],
            &[],
            t0,
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        (w, cells)
    }

    /// The follow pass's names and its missed count once row 5 reads `own`
    /// and row 4 reads `now`.
    fn follow_line(w: &mut Witness, cells: &[Cell], own: &str, now: &str) -> (Vec<FollowRun>, u32) {
        let mut out = Vec::new();
        let score = w.follow_runs(
            cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row(own),
                },
                RowSample {
                    row: 4,
                    cols: &row(now),
                },
            ],
            &mut out,
        );
        (out, score.missed)
    }

    /// **THE WORD THE WRAP RELAID IS NOT A HOLE IN THE LINE** (2026-09-23,
    /// the review of the half-gate). A bottom-anchored composer's wrap
    /// carries the line a row up MINUS its last word, which it lays again
    /// at the start of the caret row: `  this confirmatio` becomes `  this`
    /// a row up and `  confirmation` (the word and the wrap key) on its own
    /// row. The moved word's eleven records are holes a row up only because
    /// the wrap took them ELSEWHERE, and counted against the four that
    /// arrived (`2·4 < 15`) they refused the follow: the review measured
    /// `this` melting on the old row in 134 ms with the row above never lit,
    /// at 20 columns (wrap 5 of the owner's text) and at 60 (`see` before a
    /// 52-letter path), and `ribbon_follow_missed=` still reading `0`. A
    /// trailing suffix of the run whose glyphs stand, in order and at their
    /// spacing, at the start of the run's OWN row — its first at the row's
    /// first glyph column, shifted left — is the wrap's own signature and
    /// leaves the half-gate's denominator where it is a hole.
    ///
    /// The controls: a REWRITE of the row (other text on it) and the same
    /// word NOT leading its row relay nothing, so the same four arrivals are
    /// refused and counted as no evidence; and the relaid word does not
    /// excuse a hole in the kept line — `t is` a row up is still not one
    /// block, now counted missed (the arrivals are evidence once the word
    /// is out of the denominator).
    ///
    /// RED before the review's fix: the first case named nothing.
    #[test]
    fn a_word_the_wrap_relaid_at_its_row_s_start_does_not_count_against_the_line() {
        let line = "  this confirmatio";
        let blank = "";
        // `lo` 0, not 2 (the reconciliation of 2026-09-26): the line's two
        // leading cells are typed Spaces of the same run, never armed, with
        // no record of the run left behind on that side — they go up with
        // the line they lead (the extent's never-armed growth), rather than
        // stay lit on the caret row in front of the relaid word.
        let run = FollowRun {
            row: 5,
            cohort: 7,
            lo: 0,
            hi: 5,
            dr: -1,
        };
        let (mut w, cells) = armed_line_under(line, blank);
        assert_eq!(
            follow_line(&mut w, &cells, "  confirmation", "  this"),
            (vec![run], 0),
            "`this` arrived a row up and `confirmatio` leads the caret row: the line follows"
        );
        for (own, now, want, what) in [
            (
                "  xyzwvutsrqpon",
                "  this",
                (vec![], 0),
                "a rewrite relays nothing",
            ),
            (
                "  xyz confirmatio",
                "  this",
                (vec![], 0),
                "a word not leading its row",
            ),
            (
                "  confirmation",
                "  t is",
                (vec![], 14),
                "a hole in the kept line",
            ),
        ] {
            let (mut w, cells) = armed_line_under(line, blank);
            assert_eq!(follow_line(&mut w, &cells, own, now), want, "{what}");
        }
    }

    /// **THE ONE-BLOCK LAW** ([`Witness::follow_runs`]): the records a
    /// neighbour row matches must be one block — a prefix or a suffix of the
    /// run, never letters either side of a hole. `abc  f` a row up keeps
    /// four of six letters, a block of three of them first — enough by
    /// count — but the `f` past the hole says it is a coincidence: nothing
    /// is named. The positive controls: the prefix `abcd` (the moved-word
    /// shape) and the whole run are named, with their extents.
    #[test]
    fn the_follow_pass_names_only_a_block_never_letters_around_a_hole() {
        let (mut w, cells) = armed_run("      ");
        for holed in ["ab  ef", "abc  f", "a cdef"] {
            assert_eq!(
                follow_onto(&mut w, &cells, holed),
                vec![],
                "`{holed}`: a hole in the middle is not a block that moved"
            );
        }
        let run = |lo, hi| FollowRun {
            row: 5,
            cohort: 7,
            lo,
            hi,
            dr: -1,
        };
        assert_eq!(follow_onto(&mut w, &cells, "abcd  "), vec![run(0, 3)]);
        assert_eq!(follow_onto(&mut w, &cells, "  cdef"), vec![run(2, 5)]);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![run(0, 5)]);
    }

    /// **AN ERASE IS NOT A MOVE** (2026-09-22 review): the same run, armed
    /// while the row above ALREADY held `abcdef` — the previous command
    /// typed again below itself. When the run's own row is blanked (Ctrl-U)
    /// its glyphs are gone and a block of them stands a row up, but that
    /// block never ARRIVED: nothing is named. The control is the run armed
    /// over a blank row above, where the same final screen IS a move.
    ///
    /// **RE-PINNED ON PURPOSE 2026-09-23** (the twin is neutral —
    /// [`Witness::follow_runs`]): a twin on part of the run, `abc` armed
    /// under `abc`, the run's own row blanked and the whole run standing a
    /// row up. The arrived block `def` is found, and the twin prefix — its
    /// own glyphs GONE from the run's row — now rides with the block it
    /// leads: `0..=5`, where it was `3..=5` and the prefix was left on the
    /// old row to melt in `RETIRE_MELT_S` (forensics path D: "the left end
    /// of the row snaps out while the rest flows right"). The prefix is
    /// still no EVIDENCE — the all-twin run above names nothing — and it
    /// rides only flush against the block
    /// ([`an_edge_twin_rides_with_its_block_whether_or_not_its_own_glyph_is_gone`]).
    #[test]
    fn a_block_already_standing_beside_the_run_when_it_was_armed_is_not_followed() {
        let (mut w, cells) = armed_run("abcdef");
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        let (mut w, cells) = armed_run("      ");
        assert_eq!(follow_onto(&mut w, &cells, "abcdef").len(), 1);
        // A twin on part of the run only: the glyphs that ARRIVED are the
        // evidence, and the twins bordering them — standing where the moved
        // text puts them — go with the block: the whole run is carried, not
        // only its arrived tail.
        let (mut w, cells) = armed_run("abc   ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![up(0, 5)],
            "the arrived block is the evidence; the twins bordering it ride along"
        );
    }

    /// **A TWIN INSIDE AN ARRIVED BLOCK IS NEUTRAL, NOT A HOLE** (2026-09-24,
    /// the owner on 0.93.0: Claude Code's second wrapped line): the run was
    /// armed under a line that already held `c` at the same column — the
    /// letter prose shares with the line above a few times a line. That
    /// record did not arrive, but it stands where the moved text puts it: the
    /// run moved whole and is carried whole. RED on `aa71f9319`: `vec![]`, the
    /// twin scored as a hole split the block. The negative control is the
    /// `$ cd ..` erase: every record a twin, nothing arrived, nothing named.
    #[test]
    fn a_twin_inside_an_arrived_block_is_neutral_not_a_hole() {
        let (mut w, cells) = armed_run("  c   ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![FollowRun {
                row: 5,
                cohort: 7,
                lo: 0,
                hi: 5,
                dr: -1,
            }]
        );
        let (mut w, cells) = armed_run("abcdef");
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
    }

    /// **THE HALF IS OF THE RECORDS THAT CAN TESTIFY** (2026-09-24): a run
    /// armed under a line it mostly repeats (`abcd` standing above) moved
    /// when the two letters that differ ARRIVE a row up — two of the two
    /// records that are not neutral twins — and it is carried whole. RED
    /// with the twins left in the denominator: 2 of 6 is under half, and a
    /// composer line typed under a near-copy of itself melted on the caret's
    /// row (`tests/composer_multiline_wrap.rs`). The controls: the same run
    /// when the row above is UNCHANGED (its differing letters never arrived:
    /// the erase of a line under a near-identical one) and when only ONE
    /// letter arrived (too little evidence for a move) — nothing is named.
    #[test]
    fn a_run_under_a_near_copy_of_itself_follows_on_the_letters_that_arrived() {
        let (mut w, cells) = armed_run("abcd  ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![FollowRun {
                row: 5,
                cohort: 7,
                lo: 0,
                hi: 5,
                dr: -1,
            }]
        );
        let (mut w, cells) = armed_run("abcdxy");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdxy"),
            vec![],
            "the row above never changed: nothing arrived"
        );
        let (mut w, cells) = armed_run("abcd  ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcde "),
            vec![],
            "one arrived letter is too little evidence"
        );
    }

    /// Six cells of one run on `row`, born at `t0`, armed over `abcdef` on
    /// the samples `rows` alone, at `t0`.
    fn armed_on(row: u16, rows: &[RowSample<'_>]) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(row, c, t0, 7)).collect();
        walk_quiet(&mut w, &cells, rows);
        assert_eq!(w.len(), 6, "every glyph armed");
        (w, cells)
    }

    /// One walk that names nothing — the run's glyphs stand — so the only
    /// thing it does is take this frame's evidence, at the cells' birth.
    fn walk_quiet(w: &mut Witness, cells: &[Cell], rows: &[RowSample<'_>]) {
        walk_quiet_at(w, cells, rows, 0);
    }

    /// [`walk_quiet`] `ms` after the cells' birth.
    fn walk_quiet_at(w: &mut Witness, cells: &[Cell], rows: &[RowSample<'_>], ms: u64) {
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let now = cells[0].born + Duration::from_millis(ms);
        w.walk(cells, rows, &[], now, &mut out, &mut rel);
        assert!(out.is_empty() && rel.is_empty(), "the glyphs stand");
    }

    /// Every record's evidence, `(clear, twin, pending)`, in column order.
    fn evidence(w: &Witness) -> Vec<(u8, u8, u8)> {
        w.seen
            .iter()
            .map(|s| (s.clear, s.twin, s.pending))
            .collect()
    }

    /// The run on row 5, cohort 7, carried whole one row up.
    fn whole_up() -> Vec<FollowRun> {
        vec![FollowRun {
            row: 5,
            cohort: 7,
            lo: 0,
            hi: 5,
            dr: -1,
        }]
    }

    /// **WHAT THE WITNESS DID NOT SEE IS NO EVIDENCE — EITHER WAY**
    /// (0.93.0's reading). The run is armed while row 4 — one row up, inside
    /// the grid — is NOT SAMPLED. When row 5 is blanked and row 4 reads
    /// `abcdef`, the run is carried there: nothing says those glyphs were
    /// there before, and a move whose destination the witness had not yet
    /// seen empty is still a move — a paste into an empty composer and the
    /// wrap on the next frame, a band moved twice on consecutive frames.
    /// Read as neutral, an unseen row names nothing here, and those moves
    /// melt under their own text. The control is
    /// what the host makes sure of instead — the arming row's `±1` is
    /// sampled on the arming frame (`Engine::ribbon_rows_for`): a copy seen
    /// there then is a twin, and the same erase names nothing
    /// (`tests/erased_under_its_twin.rs`).
    #[test]
    fn an_unsampled_neighbour_is_no_evidence_either_way() {
        let own = row("abcdef");
        let (mut w, cells) = armed_on(5, &[RowSample { row: 5, cols: &own }]);
        assert_eq!(
            evidence(&w),
            vec![(0b1111, 0, 0); 6],
            "nothing seen: every offset clear, no twin"
        );
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            whole_up(),
            "row 4 was never seen: the glyphs found there may have arrived"
        );
        let copy = row("abcdef");
        let (mut w, cells) = armed_on(
            5,
            &[
                RowSample { row: 5, cols: &own },
                RowSample {
                    row: 4,
                    cols: &copy,
                },
            ],
        );
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![],
            "row 4 held the line when it was armed: its erase is no move"
        );
    }

    /// **THE EVIDENCE, OFFSET BY OFFSET** ([`Seen::observe`]; bit `k` is
    /// `FOLLOW_DRS[k]` = `−1, +1, −2, +2`). On the arming frame a sampled
    /// row holding the record's glyph at its column is a TWIN at once; every
    /// other offset, sampled or not, inside the grid or past its edge, is
    /// clear. What ARRIVES ([`Seen::arrivals`]) is clear and not a twin. A
    /// twin testifies only about its own row: the row two away past it, in
    /// the same direction, may still arrive (refused, the arrivals would read
    /// `0b0101`).
    #[test]
    fn the_evidence_is_what_each_follow_row_was_seen_holding() {
        let own = row("abcdef");
        let blank = row("      ");
        let twin = row("abcdef");
        // −1 seen blank, +1 a twin, −2 and +2 not sampled.
        let (w, _) = armed_on(
            5,
            &[
                RowSample { row: 5, cols: &own },
                RowSample {
                    row: 4,
                    cols: &blank,
                },
                RowSample {
                    row: 6,
                    cols: &twin,
                },
            ],
        );
        assert_eq!(evidence(&w), vec![(0b1111, 0b0010, 0); 6]);
        assert!(
            w.seen.iter().all(|s| s.arrivals() == 0b1101),
            "−1, −2 and +2 may arrive; +1 is a twin"
        );
        // Every follow row seen, one of them holding the run shifted by a
        // column (not a twin at any column): all four clear.
        let (w, _) = armed_on(
            2,
            &[
                RowSample { row: 2, cols: &own },
                RowSample {
                    row: 0,
                    cols: &blank,
                },
                RowSample {
                    row: 1,
                    cols: &blank,
                },
                RowSample {
                    row: 3,
                    cols: &row("bcdefa"),
                },
                RowSample {
                    row: 4,
                    cols: &blank,
                },
            ],
        );
        assert_eq!(
            evidence(&w),
            vec![(0b1111, 0, 0); 6],
            "all four seen, no twin"
        );
        // Row 0: nothing above it, nothing sampled below it.
        let (w, _) = armed_on(0, &[RowSample { row: 0, cols: &own }]);
        assert_eq!(evidence(&w), vec![(0b1111, 0, 0); 6]);
    }

    /// **A COPY THAT APPEARS BESIDE A STANDING LINE IS A TWIN ONCE IT HAS
    /// STOOD THERE [`TWIN_MIN`]** (2026-09-25 — fzf's list landing after the
    /// query's first two keys, a completion popup, a copy of the command line
    /// drawn above it; and torn repaints). The run is armed with row
    /// 4 seen blank, and then `abcdef` is drawn on row 4 while the run still
    /// stands. A copy seen on every sampled frame for 40 ms makes every
    /// record a twin, and the erase of row 5 names nothing. Less is a TORN
    /// REPAINT — a program with no synchronized-update bracket drawing the
    /// text's new row before it erases the old, on up to three 60 Hz frames
    /// — and that move is named. The unit is time, not frames: two frames
    /// 16 ms apart are a tear, two frames 120 ms apart (the no-pet style's
    /// idle pacing) are a copy. Counted in frames — two of them — the tear
    /// across two or three frames melts under its text.
    #[test]
    fn a_copy_that_appears_beside_a_standing_line_is_a_twin_once_it_has_stood_forty_ms() {
        let own = row("abcdef");
        let blank = row("      ");
        let copy = row("abcdef");
        let other = row("xyzxyz");
        let frame = |w: &mut Witness, cells: &[Cell], four: &[char], ms: u64| {
            walk_quiet_at(
                w,
                cells,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample { row: 4, cols: four },
                ],
                ms,
            );
        };
        let armed = || {
            armed_on(
                5,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample {
                        row: 4,
                        cols: &blank,
                    },
                ],
            )
        };
        // Three 60 Hz frames of the copy — 32 ms — then the old row erased:
        // the torn repaint, a move.
        let (mut w, cells) = armed();
        for ms in [16, 32, 48] {
            frame(&mut w, &cells, &copy, ms);
        }
        assert_eq!(evidence(&w), vec![(0b1111, 0, 0b0001); 6]);
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            whole_up(),
            "a copy that stood 32 ms: the new row of a torn repaint"
        );
        // A fourth frame, 48 ms after the first: a twin.
        let (mut w, cells) = armed();
        for ms in [16, 32, 48, 64] {
            frame(&mut w, &cells, &copy, ms);
        }
        assert_eq!(evidence(&w), vec![(0b1111, 0b0001, 0b0001); 6]);
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![],
            "the copy stood beside the line: its erase is not a move"
        );
        // Two frames 120 ms apart: a twin.
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 120);
        frame(&mut w, &cells, &copy, 240);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        // Copy, something else, copy: the stand starts over.
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        frame(&mut w, &cells, &other, 48);
        frame(&mut w, &cells, &copy, 80);
        assert_eq!(evidence(&w), vec![(0b1111, 0, 0b0001); 6]);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), whole_up());
        // A frame on which row 4 is NOT sampled does not break the stand:
        // the copy stood on both frames the witness saw.
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        walk_quiet_at(&mut w, &cells, &[RowSample { row: 5, cols: &own }], 32);
        frame(&mut w, &cells, &copy, 64);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        // A twin is sticky: the copy gone again does not undo it.
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        frame(&mut w, &cells, &copy, 64);
        frame(&mut w, &cells, &blank, 80);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
    }

    /// **A CARRIED RECORD STARTS OVER ON ITS NEW ROW AS IF JUST ARMED**
    /// ([`Witness::translate_cells`]). The run is armed on row 5 under a
    /// copy of itself on row 4 — a twin one row up — and its text moves
    /// DOWN to row 6: the follow pass finds the copy one row up neutral and
    /// carries the run down. Its evidence was about the rows around row 5,
    /// which did not move with it, so it is forgotten: every offset clear,
    /// no twin. The text goes back up to row 5 on the very next frame, the
    /// copy still standing on row 4: row 5 is a row the record was never
    /// seen beside, the glyphs arrived there, and the band follows. Kept,
    /// the twin one row up would be about row 5 now — every record neutral
    /// there, the move refused (or carried two rows, onto the copy). And it
    /// learns its new row: a copy that stood on row 7 for [`TWIN_MIN`]
    /// makes the next erase no move.
    #[test]
    fn a_carried_record_starts_over_on_its_new_row_as_if_just_armed() {
        let text = row("abcdef");
        let blank = row("      ");
        let dests = |out: &[FollowRun]| out.iter().map(|f| (f.row, f.dr)).collect::<Vec<_>>();
        let carried_down = || {
            let (mut w, cells) = armed_run("abcdef");
            assert_eq!(
                evidence(&w),
                vec![(0b1111, 0b0001, 0); 6],
                "fixture: a twin one row up"
            );
            let mut out = Vec::new();
            w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &blank,
                    },
                    RowSample {
                        row: 4,
                        cols: &text,
                    },
                    RowSample {
                        row: 6,
                        cols: &text,
                    },
                ],
                &mut out,
            );
            assert_eq!(
                dests(&out),
                vec![(5, 1)],
                "fixture: the run follows its text down, not onto its twin"
            );
            let mut moved: Vec<(u16, u16, Instant)> =
                cells.iter().map(|c| (c.row, c.col, c.born)).collect();
            w.translate_cells(&mut moved, 1);
            let carried: Vec<Cell> = cells
                .iter()
                .map(|c| cell_of(6, c.col, c.born, c.cohort))
                .collect();
            assert_eq!(
                evidence(&w),
                vec![(0b1111, 0, 0); 6],
                "carried: as if just armed"
            );
            (w, carried)
        };
        // Back up on the next frame, the copy still on row 4: it follows.
        let (mut w, carried) = carried_down();
        let mut out = Vec::new();
        w.follow_runs(
            &carried,
            &[
                RowSample {
                    row: 6,
                    cols: &blank,
                },
                RowSample {
                    row: 5,
                    cols: &text,
                },
                RowSample {
                    row: 4,
                    cols: &text,
                },
            ],
            &mut out,
        );
        assert_eq!(dests(&out), vec![(6, -1)], "back up one row");
        // A copy that stood on row 7 for 48 ms: its erase is not a move.
        let (mut w, carried) = carried_down();
        for ms in [16, 64] {
            walk_quiet_at(
                &mut w,
                &carried,
                &[
                    RowSample {
                        row: 6,
                        cols: &text,
                    },
                    RowSample {
                        row: 7,
                        cols: &text,
                    },
                ],
                ms,
            );
        }
        let mut out = Vec::new();
        w.follow_runs(
            &carried,
            &[
                RowSample {
                    row: 6,
                    cols: &blank,
                },
                RowSample {
                    row: 7,
                    cols: &text,
                },
            ],
            &mut out,
        );
        assert_eq!(dests(&out), vec![], "erased beside a twin it learned");
    }

    /// **A SCROLL KEEPS THE EVIDENCE; A BAND MOVE KEEPS WHAT MOVED WITH THE
    /// RECORD** ([`Witness::translate`], [`Witness::translate_band`],
    /// 2026-09-25). A scroll moves every row, so the row `dr` away still
    /// holds what it was seen holding: the bits ride along. A band move
    /// keeps an offset only where the neighbour moved with the record: a
    /// record inside the band forgets the offsets that reach across its
    /// edge, and a record outside it forgets those that reach in — vim's
    /// `yyp` opens a line under the one just typed. RED if the scroll
    /// forgets: an inline chat box at the screen's bottom, pushed by a
    /// streamed row, loses its band on the first push
    /// (`tests/moved_again_without_a_key.rs`); RED if the band move keeps
    /// everything: a stale "seen clear" about a row the band replaced.
    #[test]
    fn a_scroll_keeps_the_evidence_and_a_band_move_keeps_what_moved_with_the_record() {
        let own = row("abcdef");
        let blank = row("      ");
        let everything = |row_: u16| {
            let mut w = Witness::new();
            let t0 = Instant::now();
            let cells: Vec<Cell> = (0..6).map(|c| cell_of(row_, c, t0, 7)).collect();
            walk_quiet(
                &mut w,
                &cells,
                &[
                    RowSample {
                        row: row_,
                        cols: &own,
                    },
                    RowSample {
                        row: row_ - 2,
                        cols: &blank,
                    },
                    RowSample {
                        row: row_ - 1,
                        cols: &blank,
                    },
                    RowSample {
                        row: row_ + 1,
                        cols: &blank,
                    },
                    RowSample {
                        row: row_ + 2,
                        cols: &blank,
                    },
                ],
            );
            assert_eq!(evidence(&w), vec![(0b1111, 0, 0); 6]);
            w
        };
        let mut w = everything(10);
        w.translate(3, 0);
        assert!(w.seen.iter().all(|s| s.row == 7));
        assert_eq!(
            evidence(&w),
            vec![(0b1111, 0, 0); 6],
            "a scroll keeps it all"
        );
        // The band 10..=20 moves down one: the record on row 10 moves to 11
        // with rows 11 and 12 (its +1, +2), while rows 9 and 8 stay put.
        let mut w = everything(10);
        w.translate_band(10, 20, 1);
        assert!(w.seen.iter().all(|s| s.row == 11));
        assert_eq!(
            evidence(&w),
            vec![(0b1010, 0, 0); 6],
            "+1 and +2 moved with it; −1 and −2 are a new line and a row that stood"
        );
        // The band 11..=20 moves down one — vim's `yyp` under row 10: the
        // record on row 10 stands, its +1 and +2 do not.
        let mut w = everything(10);
        w.translate_band(11, 20, 1);
        assert!(w.seen.iter().all(|s| s.row == 10));
        assert_eq!(
            evidence(&w),
            vec![(0b0101, 0, 0); 6],
            "−1 and −2 stood with it"
        );
    }

    /// **A BAND MOVE THAT OPENS A ROW BESIDE A LINE LEAVES THAT ROW UNSEEN
    /// UNTIL A FRAME SHOWS IT** ([`Witness::translate_band`], 2026-09-25).
    /// vim's `yyp` on a line typed on row 10: the band 11..=20
    /// moves down one and the copy is written into the opened row 11 in the
    /// same batch. The record's `+1` and `+2` are not clear, so the copy is
    /// no arrival however few frames sampled it before `S` clears the
    /// original — RED on 0.93.0: the band carried onto the put copy. A frame
    /// that shows row 11 WITHOUT the glyph clears it again, and the line
    /// moved there after that follows.
    #[test]
    fn a_band_move_that_opens_a_row_beside_a_line_leaves_it_unseen_until_a_frame_shows_it() {
        let own = row("abcdef");
        let blank = row("      ");
        let t0 = Instant::now();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(10, c, t0, 7)).collect();
        let armed = || {
            let mut w = Witness::new();
            walk_quiet(
                &mut w,
                &cells,
                &[
                    RowSample {
                        row: 10,
                        cols: &own,
                    },
                    RowSample {
                        row: 11,
                        cols: &blank,
                    },
                ],
            );
            w.translate_band(11, 20, 1);
            w
        };
        let erased_onto_eleven = |w: &mut Witness| {
            let mut out = Vec::new();
            w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 10,
                        cols: &blank,
                    },
                    RowSample {
                        row: 11,
                        cols: &own,
                    },
                ],
                &mut out,
            );
            out.iter().map(|f| f.dr).collect::<Vec<_>>()
        };
        let mut w = armed();
        assert_eq!(evidence(&w), vec![(0b0101, 0, 0); 6]);
        assert_eq!(
            erased_onto_eleven(&mut w),
            Vec::<i16>::new(),
            "`S` after `yyp`"
        );
        let mut w = armed();
        walk_quiet_at(
            &mut w,
            &cells,
            &[
                RowSample {
                    row: 10,
                    cols: &own,
                },
                RowSample {
                    row: 11,
                    cols: &row("}     "),
                },
            ],
            16,
        );
        assert_eq!(
            erased_onto_eleven(&mut w),
            vec![1],
            "seen without it: a move"
        );
    }

    /// **A ROW A SCROLL BRINGS IN DATES ITS COPY FROM THE LAST FRAME BEFORE
    /// THE SCROLL** ([`Witness::translate`], [`Witness::last_walk`],
    /// 2026-09-25). A line typed on the grid's last row (23 of 24) has
    /// nothing below it. A scroll of one row carries it to row 22 and brings
    /// row 23 in — whatever stands there came in WITH the scroll, so it has
    /// stood there since the last walk before it (the arming walk, at 0 ms)
    /// at the latest: the offset's [`Seen::pending`] clock starts there. A
    /// copy of the line there on a look 48 ms on has stood
    /// [`TWIN_MIN`]: a twin, and the original's erase names nothing (a job
    /// notice's redraw that scrolls and re-echoes the command line: RED on
    /// `aa71f9319`, `(10, 90)` at the host seam,
    /// `tests/copy_beside_the_line.rs`). On a look 16 ms on it has not: the
    /// line relocated WITH the scroll, its original erased a frame later — a
    /// torn relocation, which follows (a twin on any first look would melt
    /// it; `tests/moves_are_followed.rs`).
    /// Row 23 blank on the first look is clear, and so is a row 23 never
    /// looked at — the line relocated onto it before any frame sampled it
    /// follows (the law's LIMIT).
    #[test]
    fn a_row_a_scroll_brings_in_dates_its_copy_from_the_last_frame_before_the_scroll() {
        let own = row("abcdef");
        let blank = row("      ");
        let t0 = Instant::now();
        let at = |r: u16| -> Vec<Cell> { (0..6).map(|c| cell_of(r, c, t0, 7)).collect() };
        let scrolled = || {
            let mut w = Witness::new();
            walk_quiet(
                &mut w,
                &at(23),
                &[RowSample {
                    row: 23,
                    cols: &own,
                }],
            );
            w.translate(1, 24);
            // `+1` reaches row 23, the row the scroll brought in; `+2`
            // reaches row 24, past the grid, which no scroll brings in.
            assert!(
                w.seen
                    .iter()
                    .all(|s| s.row == 22 && s.pending == 0b0010 && s.pend_ms == [0; 4])
            );
            w
        };
        let erased = |w: &mut Witness| {
            let mut out = Vec::new();
            w.follow_runs(
                &at(22),
                &[
                    RowSample {
                        row: 22,
                        cols: &blank,
                    },
                    RowSample {
                        row: 23,
                        cols: &own,
                    },
                ],
                &mut out,
            );
            out.iter().map(|f| f.dr).collect::<Vec<_>>()
        };
        let look = |w: &mut Witness, twenty_three: &[char], ms: u64| {
            walk_quiet_at(
                w,
                &at(22),
                &[
                    RowSample {
                        row: 22,
                        cols: &own,
                    },
                    RowSample {
                        row: 23,
                        cols: twenty_three,
                    },
                ],
                ms,
            );
        };
        let mut w = scrolled();
        look(&mut w, &own, 48);
        assert_eq!(
            erased(&mut w),
            Vec::<i16>::new(),
            "the copy came in with the row, 48 ms ago at the latest"
        );
        let mut w = scrolled();
        look(&mut w, &own, 16);
        assert_eq!(erased(&mut w), vec![1], "16 ms: the line relocated, torn");
        let mut w = scrolled();
        look(&mut w, &own, 16);
        look(&mut w, &own, 48);
        assert_eq!(erased(&mut w), Vec::<i16>::new(), "seen again at 48 ms");
        let mut w = scrolled();
        look(&mut w, &blank, 48);
        assert_eq!(erased(&mut w), vec![1], "seen blank first: a move");
        let mut w = scrolled();
        assert_eq!(erased(&mut w), vec![1], "never looked at: a move");
        // An unmeasured grid marks nothing.
        let mut w = Witness::new();
        walk_quiet(
            &mut w,
            &at(23),
            &[RowSample {
                row: 23,
                cols: &own,
            }],
        );
        w.translate(1, 0);
        assert!(w.seen.iter().all(|s| s.pending == 0));
    }

    /// **A RUN BESIDE A WITHHELD ROW WAITS ONE WALK** (2026-09-25;
    /// [`Witness::find_deferred_runs`]). The run on row 5 reads other text
    /// on its own row on a walk where row 4 was asked for and NOT delivered —
    /// the composed host's dropped far read — so it may have moved there
    /// unseen: no verdict, the records kept. On the next walk the host
    /// delivers row 4, and the follow pass carries the run there. A host
    /// that withholds the row again cannot keep the light under the wrong
    /// text: the second walk judges it. With nothing withheld the verdict is
    /// at once, as ever. Without the deferral, the first list line of
    /// Claude Code's composer, moved up past the caret's `−1` at a
    /// Shift+Enter whose far read was dropped, melts
    /// (`tests/moved_again_without_a_key.rs`).
    #[test]
    fn a_run_beside_a_withheld_row_waits_one_walk() {
        let own = row("abcdef");
        let other = row("uvwxyz");
        let blank = row("      ");
        let t0 = Instant::now();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
        let armed = || {
            let mut w = Witness::new();
            walk_quiet(
                &mut w,
                &cells,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample {
                        row: 4,
                        cols: &blank,
                    },
                ],
            );
            w
        };
        let replaced = |w: &mut Witness, withheld: &[u16]| {
            let (mut out, mut rel) = (Vec::new(), Vec::new());
            w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &other,
                }],
                withheld,
                t0 + Duration::from_millis(16),
                &mut out,
                &mut rel,
            );
            out.len()
        };
        let mut w = armed();
        assert_eq!(replaced(&mut w, &[4]), 0, "row 4 withheld: no verdict");
        assert_eq!(w.len(), 6, "the records are kept");
        let mut out = Vec::new();
        w.follow_runs(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &other,
                },
                RowSample { row: 4, cols: &own },
            ],
            &mut out,
        );
        assert_eq!(out.iter().map(|f| f.dr).collect::<Vec<_>>(), vec![-1]);
        let mut w = armed();
        assert_eq!(replaced(&mut w, &[4]), 0);
        assert_eq!(replaced(&mut w, &[4]), 6, "withheld again: judged");
        let mut w = armed();
        assert_eq!(replaced(&mut w, &[]), 6, "nothing withheld: judged at once");
        let mut w = armed();
        assert_eq!(replaced(&mut w, &[9]), 6, "a row not beside it: judged");
    }

    /// **A TWIN TESTIFIES ONLY ABOUT ITS OWN ROW** ([`Witness::follow_runs`],
    /// [`Seen::arrivals`], 2026-09-25). The run was armed under its twin —
    /// row 4 held `abcdef` — with rows 3 and 6 sampled. Its row blanked and
    /// `abcdef` ARRIVING two rows up (row 3) or two rows down (row 7) is a
    /// move: the twin on row 4 is refused, since nothing arrived there, and
    /// says nothing about the rows beyond it; nor does a near copy one row
    /// down (`abcdvw` on row 6 since the arming frame) say anything about
    /// row 7. A search that stopped at a whole text one row away on that
    /// side, or refused a two-row offset past a twin one row away in the
    /// same direction, reads `[]` for the first and fourth screens here, and
    /// melts a command line pushed down two rows under an identical one
    /// (`tests/moves_are_followed.rs`).
    /// Where a copy of the line stands two rows away on a SAMPLED row — the
    /// caret's, or another band's neighbour; no row two away is named for
    /// its own sake (`no_row_two_away_is_named`) — and did not stand there
    /// `TWIN_MIN`, the band is carried onto it: THE LIMIT (module doc). The
    /// negative control: row 3 held `abcdef` too when the run was armed — an
    /// erase under two copies, nothing arrived anywhere, and nothing is
    /// named.
    #[test]
    fn a_twin_testifies_only_about_its_own_row() {
        let own = row("abcdef");
        let blank = row("      ");
        let near = row("abcdvw");
        // (row 4, row 3, row 6) when armed and after; the row the text
        // lands on is blank when armed and holds `abcdef` after, unless it
        // is row 3 and held it already; the dr named.
        for (four, three, six, lands, named) in [
            (&own, &blank, &blank, 3u16, vec![-2]),
            (&blank, &blank, &blank, 3, vec![-2]),
            (&own, &blank, &blank, 7, vec![2]),
            (&own, &blank, &near, 7, vec![2]),
            (&own, &own, &blank, 3, vec![]),
        ] {
            let (mut w, cells) = armed_on(
                5,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample { row: 4, cols: four },
                    RowSample {
                        row: 3,
                        cols: three,
                    },
                    RowSample { row: 6, cols: six },
                    RowSample {
                        row: 7,
                        cols: &blank,
                    },
                ],
            );
            let at = |r: u16, was: &[char]| -> Vec<char> {
                if r == lands {
                    own.clone()
                } else {
                    was.to_vec()
                }
            };
            let (r3, r7) = (at(3, three), at(7, &blank));
            let mut out = Vec::new();
            w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &blank,
                    },
                    RowSample { row: 4, cols: four },
                    RowSample { row: 3, cols: &r3 },
                    RowSample { row: 6, cols: six },
                    RowSample { row: 7, cols: &r7 },
                ],
                &mut out,
            );
            assert_eq!(
                out.iter().map(|f| f.dr).collect::<Vec<_>>(),
                named,
                "row 4 {:?}, row 3 {:?}, row 6 {:?}, lands on {lands}",
                four.iter().collect::<String>(),
                three.iter().collect::<String>(),
                six.iter().collect::<String>()
            );
        }
    }

    /// **A RECORD THE SHAPE PASS CAUGHT UP IS ARMED AGAIN**
    /// (`Witness::shape_verdicts`, 2026-09-25). `abcdef` typed under
    /// the identical `abcdef`: every record a twin one row up. Two interior
    /// glyphs are overwritten (`abXYef`) — too few to be evidence, so the
    /// records catch up to `X`, `Y` — and then restored: the records catch
    /// up to `c`, `d` again. That walk is their new arming frame, and the
    /// `c`, `d` standing one row up were there first: twins at once. The
    /// line is then erased under its twin — an erase, not a move: nothing is
    /// named. With the two records' evidence forgotten and learned again
    /// only by time, `c`, `d` "arrive" one row up, two found of the two that
    /// could testify, and the band is carried onto the previous command
    /// (`tests/copies_are_not_moves.rs`: a glyph caught
    /// up under its twin, a typo fixed in place with Ctrl-T). The control:
    /// the line MOVED
    /// down a row after the same catch-up follows — the twins one row UP
    /// say nothing about a row down.
    #[test]
    fn a_record_the_shape_pass_caught_up_is_armed_again() {
        let own = row("abcdef");
        let blank = row("      ");
        let t0 = Instant::now();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
        let caught_up = || {
            let (mut w, _) = armed_on(
                5,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample { row: 4, cols: &own },
                    RowSample {
                        row: 6,
                        cols: &blank,
                    },
                ],
            );
            for (text, ms) in [("abXYef", 100), ("abcdef", 190)] {
                let (mut out, mut rel) = (Vec::new(), Vec::new());
                w.walk(
                    &cells,
                    &[
                        RowSample {
                            row: 5,
                            cols: &row(text),
                        },
                        RowSample { row: 4, cols: &own },
                        RowSample {
                            row: 6,
                            cols: &blank,
                        },
                    ],
                    &[],
                    t0 + Duration::from_millis(ms),
                    &mut out,
                    &mut rel,
                );
                assert!(
                    out.is_empty() && rel.is_empty(),
                    "an interior rewrite is not evidence: {text}"
                );
            }
            assert!(
                w.seen.iter().all(|s| s.twin & 1 == 1),
                "every record a twin one row up again"
            );
            w
        };
        let follow = |w: &mut Witness, six: &[char]| {
            let mut out = Vec::new();
            w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &blank,
                    },
                    RowSample { row: 4, cols: &own },
                    RowSample { row: 6, cols: six },
                ],
                &mut out,
            );
            out.iter().map(|f| f.dr).collect::<Vec<_>>()
        };
        let mut w = caught_up();
        assert_eq!(
            follow(&mut w, &blank),
            Vec::<i16>::new(),
            "erased under its twin"
        );
        let mut w = caught_up();
        assert_eq!(follow(&mut w, &own), vec![1], "moved down a row");
    }

    /// One run on row 5 laid under every column of `text` (its spaces
    /// never-armed) and armed by one walk.
    fn armed_line(text: &str) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let n = u16::try_from(text.chars().count()).expect("a short line");
        let cells: Vec<Cell> = (0..n).map(|c| cell_of(5, c, t0, 7)).collect();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row(text),
            }],
            &[],
            t0,
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        (w, cells)
    }

    /// The shift pass's verdict once row 5 reads `now`, the caret last seen
    /// at `caret` on it.
    fn shift_onto(w: &mut Witness, cells: &[Cell], now: &str, caret: Option<u16>) -> Vec<ShiftRun> {
        let mut out = Vec::new();
        w.shift_runs(
            cells,
            &[RowSample {
                row: 5,
                cols: &row(now),
            }],
            caret.map(|c| (5, c)),
            &mut out,
        );
        out
    }

    fn shift(lo: u16, hi: u16, dc: i16) -> Vec<ShiftRun> {
        vec![ShiftRun {
            row: 5,
            cohort: 7,
            lo,
            hi,
            dc,
        }]
    }

    /// **AN INSERT PUSHES ITS TAIL, AND THE TAIL IS FOUND WHERE IT WENT**
    /// ([`Witness::shift_runs`], 2026-09-24 — the owner's *"the rainbow
    /// cursor trail fractured with black spaces when I started typing in
    /// the middle of a line"*). One key inside `hello world`, at the caret
    /// and one column on, and a three-cell paste: the block from the edit
    /// point to the line's end is named with its width — the space before
    /// `world` included when the key went in at it, left standing when the
    /// key went in after it.
    #[test]
    fn an_insert_names_the_tail_it_pushed_with_its_width() {
        let (mut w, cells) = armed_line("hello world");
        assert_eq!(
            shift_onto(&mut w, &cells, "hello Xworld", Some(6)),
            shift(6, 10, 1),
            "a key typed at the `w`"
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "helloX world", Some(5)),
            shift(5, 10, 1),
            "a key typed at the space takes the space along"
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "hello XYZworld", Some(6)),
            shift(6, 10, 3),
            "a three-cell insert"
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "hello world", Some(11)),
            vec![],
            "nothing moved, nothing named"
        );
    }

    /// **THE CARET IS THE EDIT POINT WHERE LETTERS REPEAT.** `hello` with an
    /// `l` typed at the first `l`: read column by column only the `o` is
    /// gone, and the two `l`s stand where an `l` stands either way. The
    /// extension over the repeated letters finds the block, and the caret
    /// the key was typed at picks its start — the key's cell goes where the
    /// hand was, and the pushed `llo` keeps its light one column on.
    /// Without the caret the one moved glyph (`o`) is too little to go on.
    #[test]
    fn a_key_typed_among_repeated_letters_inserts_at_the_caret() {
        let (mut w, cells) = armed_line("hello");
        assert_eq!(
            shift_onto(&mut w, &cells, "helllo", Some(2)),
            shift(2, 4, 1)
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "helllo", Some(3)),
            shift(3, 4, 1)
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "helllo", None),
            vec![],
            "one moved glyph and no caret is not evidence"
        );
        // `ab|cd` + `b`: the greedy block would start at the `b` left of
        // the caret; the caret says the key went in at `c`.
        let (mut w, cells) = armed_line("abcd");
        assert_eq!(shift_onto(&mut w, &cells, "abbcd", Some(2)), shift(2, 3, 1));
    }

    /// **ONE GLYPH IS EVIDENCE ONLY AT THE CARET.** A key typed before the
    /// line's last letter pushes a one-glyph tail: named when the caret was
    /// at the edit point, refused when it was not (one coincidental letter
    /// further along the row is not a move).
    #[test]
    fn a_one_glyph_tail_moves_only_under_the_caret() {
        let (mut w, cells) = armed_line("type");
        assert_eq!(shift_onto(&mut w, &cells, "typXe", Some(3)), shift(3, 3, 1));
        assert_eq!(shift_onto(&mut w, &cells, "typXe", None), vec![]);
        assert_eq!(shift_onto(&mut w, &cells, "typXe", Some(1)), vec![]);
        // The Backspace this was measured against (`wrapped_composer_band`'s
        // `c8_bs_mid`): the band's last cell is the caret's `s`, the erase
        // pulled `also see` together, and the `s` of `see` stands three
        // columns on. More text stands past it — the line did not end
        // where a pushed tail would — so it is not an insert.
        let (mut w, cells) = armed_line("I als");
        assert_eq!(shift_onto(&mut w, &cells, "I alee see", Some(4)), vec![]);
    }

    /// **A REWRITE IS NOT A SHIFT.** The negative controls: other text over
    /// the tail with a coincidental copy of its first letter further along;
    /// a tail whose glyphs are found but whose next glyph stands on OTHER
    /// text (not blank — so not a wrap); and a tail CLEARED with a copy of
    /// it standing further left (a ⌃W over a repeated word) — nothing is
    /// named, and the walk melts or releases those cells as it always did.
    #[test]
    fn a_rewrite_or_a_clear_is_never_read_as_an_insert() {
        let (mut w, cells) = armed_line("hello world");
        assert_eq!(
            shift_onto(&mut w, &cells, "hello xyzqw", Some(6)),
            vec![],
            "other text with a `w` in it"
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "hello Xwoqqq", Some(6)),
            vec![],
            "`wo` found, then other text"
        );
        let (mut w, cells) = armed_line("ab cd ab");
        assert_eq!(
            shift_onto(&mut w, &cells, "ab cd   ", Some(8)),
            vec![],
            "a cleared tail is not text that moved"
        );
    }

    /// **A TAIL THE ROW'S END TOOK IS LEFT BEHIND, NOT A VETO.** An insert
    /// that pushes the line's last word past the edge (a composer's wrap
    /// moves it to the next row): the glyphs still on the row are found and
    /// named; the ones whose `col + dc` is blank stay where they are for
    /// the walk to release.
    #[test]
    fn an_insert_whose_tail_left_the_row_moves_what_stayed() {
        let (mut w, cells) = armed_line("hi you there");
        assert_eq!(
            shift_onto(&mut w, &cells, "hi XYZyou      ", Some(3)),
            shift(3, 6, 3),
            "`you ` pushed on, `there` wrapped away"
        );
    }

    /// A block that lands on its own left-behind tail (the wrap case above)
    /// leaves ONE record per `(row, col, born)`: the stale tail's records
    /// under the landing columns go, so `find` never picks between two
    /// records that share a key. The records past the landing (the rest of
    /// the wrapped word) stay for the walk to release.
    #[test]
    fn a_shift_onto_its_own_left_behind_tail_leaves_one_record_per_key() {
        let (mut w, cells) = armed_line("hi you there");
        assert_eq!(
            shift_onto(&mut w, &cells, "hi XYZyou      ", Some(3)),
            shift(3, 6, 3)
        );
        let mut moved: Vec<_> = cells
            .iter()
            .filter(|c| (3..=6).contains(&c.col))
            .map(|c| (c.row, c.col, c.born))
            .collect();
        w.shift_cells(&mut moved, 3);
        let keys: Vec<_> = w.seen.iter().map(|s| (s.row, s.col, s.born)).collect();
        let mut dedup = keys.clone();
        dedup.dedup();
        assert_eq!(keys, dedup, "two records share a key");
        let cols: Vec<u16> = w.seen.iter().map(|s| s.col).collect();
        // Spaces were never armed: `h i`, then `you` at 6..=8, then the
        // wrapped word's `re` past the landing at 10..=11.
        assert_eq!(cols, vec![0, 1, 6, 7, 8, 10, 11]);
        let units: Vec<Unit> = w.seen.iter().map(|s| s.unit).collect();
        let at = |c: u16| units[cols.iter().position(|&x| x == c).expect("a record")];
        assert_eq!(at(6), unit_at(&row("hi you there"), 3), "`y` moved to 6");
        assert_eq!(at(8), unit_at(&row("hi you there"), 5), "`u` moved to 8");
        assert_eq!(at(10), unit_at(&row("hi you there"), 10), "`r` stayed");
    }

    /// The records follow their cells: re-keyed to the new column, so the
    /// next walk finds each cell under its own glyph and names nothing.
    #[test]
    fn shifted_records_find_their_glyphs_on_the_next_walk() {
        let (mut w, mut cells) = armed_line("hello world");
        let found = shift_onto(&mut w, &cells, "hello Xworld", Some(6));
        assert_eq!(found, shift(6, 10, 1));
        let mut moved = Vec::new();
        for c in &mut cells {
            if (6..=10).contains(&c.col) {
                moved.push((c.row, c.col, c.born));
                c.col += 1;
            }
        }
        w.shift_cells(&mut moved, 1);
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hello Xworld"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && rel.is_empty(),
            "retired {:?} released {:?}",
            pos(&out),
            pos(&rel)
        );
    }

    #[test]
    fn blank_to_glyph_only_arms_and_glyph_to_blank_releases() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        // The echo landed a frame late: nothing under the cells yet.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && w.is_empty(),
            "a blank arms nothing and retires nothing"
        );
        // Then the glyphs arrive.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 2, "seen: armed, not retired");
        // The same text again — a prompt redraw that put it back (D2).
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && w.len() == 2,
            "the same glyph touches nothing"
        );
        // Then the row is blank: both go — RELEASED to the swoosh, not
        // retired on the melt: nothing sits under them, and `hi` is nowhere
        // else the witness can see.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "a blank is not the fast melt's verdict");
        assert_eq!(pos(&rel), vec![(5, 0), (5, 1)]);
        assert!(
            rel.iter().all(|&(_, _, born)| born == t0),
            "named by identity: the cells' own births"
        );
        assert_eq!(
            w.len(),
            2,
            "released records are KEPT: the cells are resident and unstamped for the retract"
        );
        // The next frame, still blank: nothing to say, and nothing named
        // again.
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty() && n == 0);
        assert_eq!(w.len(), 2, "…and the records stand");
    }

    /// What one walk named: the positions on `retire`, then on `release`.
    type Named = (Vec<(u16, u16)>, Vec<(u16, u16)>);

    /// One walk of `cells` against one sampled row.
    fn walk_row(w: &mut Witness, cells: &[Cell], r: u16, now: &str) -> Named {
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            cells,
            &[RowSample {
                row: r,
                cols: &row(now),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        (pos(&out), pos(&rel))
    }

    /// **A GLYPH LANDING ON A BLANK WITH THE REWRITE THAT TOOK ITS RUN GOES
    /// WITH THE RUN** (2026-09-24, [`Witness::landed_goes_with_its_run`]).
    /// The run a composer's follow split leaves on the caret row: the Space
    /// the wrap ate (never armed) and the moved word's old cells after it.
    /// The relay lays a long word over all of them in one batch — every
    /// glyph replaced, and a glyph lands on the Space. The Space's cell goes
    /// with its run; its fresh record goes too. RED before, measured: the
    /// walk armed it under the relaid glyph and named only `(5,3)` and
    /// `(5,4)`, and the lone standing cell kept the run joinable.
    #[test]
    fn a_glyph_landing_on_a_blank_with_the_rewrite_that_took_its_run_goes_with_the_run() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cells = [cell(5, 2, t0), cell(5, 3, t0), cell(5, 4, t0)];
        assert_eq!(walk_row(&mut w, &cells, 5, "xx cd"), (vec![], vec![]));
        assert_eq!(w.len(), 2, "premise: the Space is never armed");
        let (out, rel) = walk_row(&mut w, &cells, 5, "xxzqw");
        assert_eq!(out, vec![(5, 3), (5, 4), (5, 2)]);
        assert!(rel.is_empty());
        assert!(w.is_empty(), "no record is left for the melting cells");

        // CONTROL — a key's OWN cell, first read on the walk that retires
        // its run, is armed as ever: it was never read over a blank.
        let t1 = t0 + std::time::Duration::from_millis(60);
        let mut w = Witness::new();
        let word = [cell(5, 3, t0), cell(5, 4, t0)];
        assert_eq!(walk_row(&mut w, &word, 5, "xxxcd"), (vec![], vec![]));
        let keyed = [cell(5, 2, t1), cell(5, 3, t0), cell(5, 4, t0)];
        let (out, _) = walk_row(&mut w, &keyed, 5, "xxzqw");
        assert_eq!(out, vec![(5, 3), (5, 4)], "the key's cell is not named");
        assert_eq!(w.len(), 1, "…and it is armed");

        // CONTROL — a glyph landing on a blank of a run that keeps its text
        // is armed as ever (a late echo, an app filling the space).
        let mut w = Witness::new();
        let line = [
            cell(5, 0, t0),
            cell(5, 1, t0),
            cell(5, 2, t0),
            cell(5, 3, t0),
            cell(5, 4, t0),
        ];
        assert_eq!(walk_row(&mut w, &line, 5, "ab cd"), (vec![], vec![]));
        assert_eq!(walk_row(&mut w, &line, 5, "abxcd"), (vec![], vec![]));
        assert_eq!(w.len(), 5, "the landed glyph is armed with its run");
    }

    /// **…AND WITH THE CLEAR THAT RELEASED IT** (2026-09-24): `ab cd` whose
    /// row is cleared in the batch that writes one glyph on the Space's
    /// column (a spinner painted over the composer the Enter cleared). The
    /// letters are released to the swoosh; the Space's cell, under a glyph
    /// that is not its text, goes with them rather than standing alone —
    /// the one-cell stray. RED before, measured: it was armed under the
    /// glyph and named nowhere.
    ///
    /// It goes on the run's OWN verdict, the swoosh (`release`), never the
    /// melt (2026-09-24, the review of this rule, which first put it on
    /// `retire`): a never-armed cell of a released run is released, as the
    /// "blanks go with their run" law gives it. On the melt it left the
    /// draining band punched through — a one-cell hole for 17 frames at the
    /// host seam (`tests/new_line_fade.rs`).
    #[test]
    fn a_glyph_landing_on_a_released_run_s_blank_goes_with_the_run() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let line = [
            cell(5, 0, t0),
            cell(5, 1, t0),
            cell(5, 2, t0),
            cell(5, 3, t0),
            cell(5, 4, t0),
        ];
        assert_eq!(walk_row(&mut w, &line, 5, "ab cd"), (vec![], vec![]));
        let (out, rel) = walk_row(&mut w, &line, 5, "  x  ");
        assert_eq!(rel, vec![(5, 0), (5, 1), (5, 3), (5, 4), (5, 2)]);
        assert!(out.is_empty(), "nothing melts: {out:?}");
    }

    /// **A GLYPH LANDING ON A BLANK DOES NOT PULL ITS RUN'S STANDING TEXT
    /// INTO THE MELT** (2026-09-24, the review of
    /// [`Witness::landed_goes_with_its_run`]). `ab cd ef` → `ab*cd eX`: a
    /// glyph lands on the Space at column 2 (an app's decoration) in the
    /// walk that retires the run for the rewritten last letter. The landed
    /// cell has `ab` standing on its far side from the run's named cells,
    /// so it is not the run's loose blank — it stays, and the span closure
    /// does not reach back to it through `cd e`. The same with a mid-line
    /// Backspace, `ab cd efgh` → `ab*cd egh `. RED before the fix,
    /// measured by the review: cols 2..7 retired (2..9 for the Backspace),
    /// where only 7 (7..9) changed.
    #[test]
    fn a_glyph_landing_on_a_blank_between_standing_words_stays_out_of_its_run_s_melt() {
        let t0 = Instant::now();
        for (before, after, want) in [
            ("ab cd ef", "ab*cd eX", vec![(5, 7)]),
            ("ab cd efgh", "ab*cd egh ", vec![(5, 7), (5, 8), (5, 9)]),
        ] {
            let mut w = Witness::new();
            let n = u16::try_from(before.len()).expect("short");
            let line: Vec<Cell> = (0..n).map(|c| cell(5, c, t0)).collect();
            assert_eq!(walk_row(&mut w, &line, 5, before), (vec![], vec![]));
            let (mut out, rel) = walk_row(&mut w, &line, 5, after);
            out.sort_unstable();
            assert_eq!(out, want, "{before:?} -> {after:?}");
            assert!(rel.is_empty(), "{before:?} -> {after:?}: {rel:?}");
        }
    }

    /// **THE LAST WALK'S BLANKS RIDE WITH THEIR CELLS** (2026-09-24,
    /// [`Witness::blank_seen`]): a follow pass, a scroll and a band move
    /// each carry a never-armed blank to another row, and the next walk
    /// still knows it was a blank — so the relay's glyph landing on it,
    /// with its run retired, takes it with the run. RED with the list left
    /// behind: the carried cell was armed and stood.
    #[test]
    fn the_last_walk_s_blanks_ride_with_their_cells() {
        let t0 = Instant::now();
        for mover in 0..3 {
            let mut w = Witness::new();
            let mut cells = [cell(5, 2, t0), cell(5, 3, t0), cell(5, 4, t0)];
            assert_eq!(walk_row(&mut w, &cells, 5, "xx cd"), (vec![], vec![]));
            match mover {
                0 => {
                    let mut moved: Vec<_> = cells.iter().map(|c| (c.row, c.col, c.born)).collect();
                    w.translate_cells(&mut moved, -1);
                }
                1 => w.translate(1, 0),
                _ => w.translate_band(0, 10, -1),
            }
            for c in &mut cells {
                c.row = 4;
            }
            let (out, _) = walk_row(&mut w, &cells, 4, "xxzqw");
            assert_eq!(out, vec![(4, 3), (4, 4), (4, 2)], "mover {mover}");
        }
    }

    /// **A RELEASED RECORD IS KEPT, AND READ AGAIN** (the fix-up of
    /// 2026-09-14). `hi` released on one walk; the cells stay resident and
    /// unstamped on the retract. Then the transcript's spinner lands on the
    /// row — `Th` under the cells: different glyphs, REPLACED text — and
    /// both go onto `retire` on that walk, named by their own births, with
    /// the walk reporting both as re-verdicts of cells already released
    /// (so the engine does not count them twice). And the same glyphs back
    /// (`hi` echoed where it was) is D2: nothing. Before the fix-up the
    /// released records were dropped and the next walk ARMED the cells
    /// over the spinner.
    #[test]
    fn a_released_record_is_kept_so_a_glyph_landing_under_it_later_melts_fast_and_counts_once() {
        let t0 = Instant::now();
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        let release = |w: &mut Witness, out: &mut Vec<_>, rel: &mut Vec<_>| {
            let n = w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &row("hi"),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
            assert!(out.is_empty() && rel.is_empty() && n == 0 && w.len() == 2);
            let n = w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &row("  "),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
            assert_eq!(pos(rel), vec![(5, 0), (5, 1)], "released");
            assert!(out.is_empty() && n == 0);
            assert_eq!(w.len(), 2, "the released records are kept");
        };
        // (1) The spinner lands under the released cells.
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        release(&mut w, &mut out, &mut rel);
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("Th"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(
            pos(&out),
            vec![(5, 0), (5, 1)],
            "text under a released cell is REPLACED text: the melt"
        );
        assert!(
            out.iter().all(|&(_, _, born)| born == t0),
            "named by identity"
        );
        assert!(rel.is_empty(), "never released twice");
        assert_eq!(
            n, 2,
            "both are re-verdicts of cells already released and counted"
        );
        assert!(
            w.is_empty(),
            "a retired cell's record goes: it is stamped and leaving"
        );
        // (2) One glyph lands and the other cell stays blank: the run is
        // one object, so the held cell melts with its replaced run-mate —
        // and is a re-verdict too.
        let mut w = Witness::new();
        release(&mut w, &mut out, &mut rel);
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row(" x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        assert_eq!(got, vec![(5, 0), (5, 1)], "the held cell goes with its run");
        assert!(rel.is_empty());
        assert_eq!(n, 2);
        // (3) The same glyphs back where they were: D2, nothing.
        let mut w = Witness::new();
        release(&mut w, &mut out, &mut rel);
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && rel.is_empty() && n == 0,
            "the same glyph touches nothing"
        );
        assert_eq!(w.len(), 2);
        // …and blank again after that: still nothing — a held cell is never
        // released or counted again, and its text is never searched for
        // (`hi` standing on the caret's row would have read as MOVED on a
        // fresh blank; a held one is past that).
        let n = w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("hi"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty() && n == 0);
        assert_eq!(w.len(), 2);
    }

    /// The never-armed cell of a released run — the space in `ab cd` —
    /// takes a released record with its run, so a glyph landing on IT is
    /// read as replaced text too, and the run melts as one.
    #[test]
    fn a_released_run_s_never_armed_cell_is_held_with_it_and_a_glyph_on_it_melts_the_run() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let word: Vec<Cell> = (0..5u16).map(|c| cell_of(5, c, t0, 7)).collect();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("ab cd"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 4, "the four letters, not the blank");
        let n = w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("     "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(rel.len(), 5, "the run released, the blank with it");
        assert_eq!(n, 0);
        assert_eq!(
            w.len(),
            5,
            "…and the blank now holds a released record of its own"
        );
        // A single glyph lands on the space: the whole run melts, five
        // re-verdicts.
        let n = w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("  *  "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, vec![(5, 0), (5, 1), (5, 2), (5, 3), (5, 4)]);
        assert!(rel.is_empty());
        assert_eq!(n, 5, "every cell was counted when released: none again");
        assert!(w.is_empty());
    }

    /// **A RELEASE TAKES NO SPACE FROM BETWEEN STANDING LETTERS**
    /// (2026-09-21, `wrapped_composer_band`'s `c2_bs`). `ab cd ef` with the
    /// last letter blanked: the blanked `f` is released, and so is nothing
    /// between the letters still standing — the spaces at 2 and 5 stay lit
    /// with them. Clear the tail from 4 instead and the space at 5, past
    /// the standing `ab c`, goes with the release (no speck); clear the
    /// whole row and every blank goes (the stray law, as before).
    ///
    /// RED before 2026-09-21: the first walk released columns 2, 5 and 7 —
    /// a comb over a line whose letters were all still on glass.
    #[test]
    fn a_partial_release_takes_no_space_from_between_standing_letters() {
        let t0 = Instant::now();
        let run: Vec<Cell> = (0..8u16).map(|c| cell_of(5, c, t0, 7)).collect();
        let arm = |w: &mut Witness, out: &mut Vec<_>, rel: &mut Vec<_>| {
            w.walk(
                &run,
                &[RowSample {
                    row: 5,
                    cols: &row("ab cd ef"),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
        };
        let released = |text: &str| {
            let mut w = Witness::new();
            let (mut out, mut rel) = (Vec::new(), Vec::new());
            arm(&mut w, &mut out, &mut rel);
            w.walk(
                &run,
                &[RowSample {
                    row: 5,
                    cols: &row(text),
                }],
                &[],
                Instant::now(),
                &mut out,
                &mut rel,
            );
            assert!(
                out.is_empty(),
                "{text:?}: nothing replaced: {:?}",
                pos(&out)
            );
            let mut got: Vec<u16> = pos(&rel).into_iter().map(|(_, c)| c).collect();
            got.sort_unstable();
            got.dedup();
            got
        };
        assert_eq!(
            released("ab cd e "),
            vec![7],
            "the blanked letter alone: the spaces between standing letters stay"
        );
        assert_eq!(
            released("ab c    "),
            vec![4, 5, 6, 7],
            "a cleared tail takes its own space with it"
        );
        assert_eq!(
            released("        "),
            (0..8).collect::<Vec<u16>>(),
            "a run with nothing standing takes every blank"
        );
    }

    /// **ONE GLYPH READ BLANK FOR A WALK TAKES NO SPACE WITH IT**
    /// (2026-09-21 — the owner, in Claude Code's composer: the spaces
    /// between typed phrases dark, every glyph lit). `hello world` as one
    /// run; a row sampled mid-rewrite reads the last letter blank for one
    /// walk — the run's LAST recorded cell, a one-cell suffix, which the
    /// shape pass lets stand as named (a lone INTERIOR blank it already
    /// refuses as evidence). The run's other glyphs still stand, so the
    /// release names that one cell and NOT the never-armed space at 5 — which keeps its light
    /// and its clock (it is never named, so `Ribbon::release_cells` never
    /// stamps it) and takes no released record. The glyph back on the next
    /// walk restores the run by its recorded identities. The control is a
    /// suffix cleared from `w` on: the space inside the lost span still goes
    /// with it (`tests/partial_release.rs`'s law).
    ///
    /// RED before the span rule: `rel=[(5, 10), (5, 5)]`, `w.len() == 11`.
    #[test]
    fn a_partial_release_of_one_glyph_leaves_the_run_s_spaces_standing() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let word: Vec<Cell> = (0..11u16).map(|c| cell_of(5, c, t0, 7)).collect();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 10, "ten letters armed, not the space");
        let n = w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello worl "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing replaced: {:?}", pos(&out));
        assert_eq!(
            pos(&rel),
            vec![(5, 10)],
            "the one blanked glyph is released, and no space with it"
        );
        assert_eq!(n, 0);
        assert_eq!(w.len(), 10, "the space took no released record");
        // A key typed inside the melt, past the stamped cell: its cell is
        // armed on the next walk with no release tag. It is not a veto.
        let t1 = t0 + std::time::Duration::from_millis(50);
        let mut typed: Vec<Cell> = word.clone();
        typed.push(cell_of(5, 11, t1, 7));
        w.walk(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worl x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        assert_eq!(w.len(), 11, "the new key's cell is armed");
        // The glyph back: the run's recorded identities are all standing,
        // and the released record's ink returned — restorable, the untagged
        // new cell notwithstanding.
        let mut restore = Vec::new();
        w.restored_runs(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worldx"),
            }],
            &mut restore,
        );
        assert_eq!(
            restore,
            vec![(7, None)],
            "the run is restorable with a key typed inside the melt"
        );
        // The control: a RETYPE over the released column — a new cell born
        // later at col 10, its glyph the same `d` — is the untagged new
        // birth, and refuses.
        let mut w2 = w.clone();
        let mut retyped: Vec<Cell> = word.clone();
        retyped.push(cell_of(5, 10, t1, 7));
        w2.walk(
            &retyped,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut refused = Vec::new();
        w2.restored_runs(
            &retyped,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &mut refused,
        );
        assert!(
            refused.is_empty(),
            "a retype over the released column does not bring the old light back: {refused:?}"
        );
        w.restored(&[7], &typed);
        let n = w.walk(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worldx"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty(), "the row stands whole");
        assert_eq!(n, 0);
        assert_eq!(w.len(), 11);
        // The control: the suffix cleared from `w` on — the space at 5 is
        // outside the lost span and stands; a suffix cleared from `o` on
        // takes the space inside it.
        let mut w = Witness::new();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello      "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(
            got,
            vec![(5, 6), (5, 7), (5, 8), (5, 9), (5, 10)],
            "a cleared suffix takes its letters; the space before it stands"
        );
        let mut w = Witness::new();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hell       "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(
            got,
            (4..11u16).map(|c| (5, c)).collect::<Vec<_>>(),
            "a suffix cleared across the space takes the space inside it"
        );
    }

    /// **THE TAIL GOES WITH THE SUFFIX** (2026-09-21, the review's host-seam
    /// probe). `hello world ` — twelve keys, the last a typed trailing
    /// space at 11, never armed — and the program clears from `w` on: the
    /// span rule alone released 6..10 and left the space at 11 lit, five
    /// dark cells past `hello ` — the one-cell stray the blank law was
    /// written for. A never-armed cell of a released run with glyphs still
    /// standing goes with the release when it is inside the lost span OR
    /// when no standing recorded glyph of the run lies beyond it on its
    /// side: 11 is right of `hi = 10` with nothing standing right of 10, so
    /// it goes; 5 is left of `lo = 6` with `hello` standing left of 6, so it
    /// stands. The mirror: ` hello world` with `hello` cleared takes the
    /// leading space at 0 (nothing stands left of 1) and leaves the space at
    /// 6 (`world` stands right of 5).
    ///
    /// RED before the tail closure: `rel=[6, 7, 8, 9, 10]` (11 lit), and
    /// `rel=[1, 2, 3, 4, 5]` (0 lit).
    #[test]
    fn a_never_armed_cell_past_a_cleared_suffix_goes_with_it() {
        let t0 = Instant::now();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let mut w = Witness::new();
        let line: Vec<Cell> = (0..12u16).map(|c| cell_of(5, c, t0, 7)).collect();
        w.walk(
            &line,
            &[RowSample {
                row: 5,
                cols: &row("hello world "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 10, "ten letters armed, neither space");
        w.walk(
            &line,
            &[RowSample {
                row: 5,
                cols: &row("hello       "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing replaced: {:?}", pos(&out));
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(
            got,
            (6..12u16).map(|c| (5, c)).collect::<Vec<_>>(),
            "the cleared suffix takes the trailing space after it; the space before it stands"
        );
        // The mirror: a leading never-armed space before a cleared prefix.
        let mut w = Witness::new();
        w.walk(
            &line,
            &[RowSample {
                row: 5,
                cols: &row(" hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 10);
        w.walk(
            &line,
            &[RowSample {
                row: 5,
                cols: &row("       world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing replaced: {:?}", pos(&out));
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(
            got,
            (0..6u16).map(|c| (5, c)).collect::<Vec<_>>(),
            "the cleared prefix takes the leading space before it; the space after it stands"
        );
    }

    /// **A RECORD ARMED AFTER THE RELEASE WHOSE GLYPH CHANGED IS NEUTRAL**
    /// (2026-09-21, the review). `hello world`, one glyph released for a
    /// walk; a key lands on the never-armed column 11 and is armed
    /// (`x`); its glyph is then rewritten differently (`y`) as the old
    /// text returns. The record at 11 was armed after the release and
    /// makes no claim on the run's OLD text — a changed glyph is the
    /// walk's to retire on its own — so it neither authorizes nor refuses:
    /// the restore is ACCEPTED, and the walk after it retires exactly the
    /// rewritten cell. Read as a veto it left the stamped `d` melting under
    /// text that was back.
    ///
    /// RED before: `restore=[]`.
    #[test]
    fn a_record_armed_after_the_release_whose_glyph_changed_is_neutral_to_the_restore() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let word: Vec<Cell> = (0..11u16).map(|c| cell_of(5, c, t0, 7)).collect();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello worl "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(
            pos(&rel),
            vec![(5, 10)],
            "the one blanked glyph is released"
        );
        let t1 = t0 + std::time::Duration::from_millis(50);
        let mut typed: Vec<Cell> = word.clone();
        typed.push(cell_of(5, 11, t1, 7));
        w.walk(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worl x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        assert_eq!(w.len(), 11, "the new key's cell is armed as `x`");
        // The glyph at 11 rewritten to `y`, the `d` back at 10.
        let mut restore = Vec::new();
        w.restored_runs(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worldy"),
            }],
            &mut restore,
        );
        assert_eq!(
            restore,
            vec![(7, None)],
            "a record armed after the release, its glyph changed, is no veto"
        );
        w.restored(&[7], &typed);
        w.walk(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worldy"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(
            pos(&out),
            vec![(5, 11)],
            "the rewritten cell is retired by the walk, on its own"
        );
        assert!(rel.is_empty(), "nothing else leaves: {:?}", pos(&rel));
        assert_eq!(
            w.len(),
            10,
            "the retired cell's record goes with its verdict; the restored `d` keeps its own, the space still has none"
        );
    }

    /// **THE RUN DECIDES THE CLOCK** (2026-09-14, the new line's fade). The
    /// same `hi`, three ways: one glyph replaced and one blanked is a
    /// re-laid box — the whole run melts fast, the blanked cell with it;
    /// both blanked with `hi` standing on another sampled row is a box
    /// re-laid elsewhere — moved, fast; both blanked and `hi` nowhere is a
    /// submit — released.
    #[test]
    fn a_replaced_glyph_or_a_moved_run_melts_fast_and_only_a_vanished_run_is_released() {
        let t0 = Instant::now();
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        let arm = |w: &mut Witness, out: &mut Vec<_>, rel: &mut Vec<_>| {
            w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &row("hi"),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
            assert!(out.is_empty() && rel.is_empty() && w.len() == 2);
        };
        // (1) `h` → `H`, `i` → blank: the run was overwritten.
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        arm(&mut w, &mut out, &mut rel);
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("H "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        assert_eq!(
            got,
            vec![(5, 0), (5, 1)],
            "the blanked `i` melts with its replaced run-mate"
        );
        assert!(rel.is_empty());
        // (2) both blank here, `hi` on the caret's row: the text moved.
        let mut w = Witness::new();
        arm(&mut w, &mut out, &mut rel);
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("hi"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(
            pos(&out),
            vec![(5, 0), (5, 1)],
            "a run found elsewhere moved: fast"
        );
        assert!(rel.is_empty());
        // (3) both blank, the caret's row holds other text: the text went.
        let mut w = Witness::new();
        arm(&mut w, &mut out, &mut rel);
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("> ok"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "nothing replaced it and it is nowhere: not the melt"
        );
        assert_eq!(pos(&rel), vec![(5, 0), (5, 1)], "released to the swoosh");
        // A partial match is not the text: `h` alone elsewhere moves nothing.
        let mut w = Witness::new();
        arm(&mut w, &mut out, &mut rel);
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("h ho"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "`h` and `i` must stand together at their spacing"
        );
        assert_eq!(pos(&rel), vec![(5, 0), (5, 1)]);
    }

    /// **A RUN THAT STILL STANDS HAS NOT MOVED** (2026-09-22). The owner's
    /// hole: a torn read blanks `TO` at the end of a line whose `TO` also
    /// stands earlier on it. The blanked fragment of a run with glyphs
    /// still in place is RELEASED — restorable — never searched for and
    /// melted as moved text.
    #[test]
    fn a_blanked_tail_of_a_standing_run_is_released_though_its_glyphs_stand_elsewhere() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let cells: Vec<Cell> = (0..11u16).map(|c| cell(5, c, t0)).collect();
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab TO cd TO"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab TO cd   "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing melts: {:?}", pos(&out));
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(got, vec![(5, 9), (5, 10)], "the blanked `TO` is released");
    }

    /// **ONE GLYPH IS NOT A WORD** (2026-09-22). A run of one blanked glyph
    /// is never ruled moved text, wherever its letter stands.
    #[test]
    fn one_blanked_glyph_found_elsewhere_is_released_not_moved() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let cells = [cell(5, 0, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("z"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row(" "),
                },
                RowSample {
                    row: 7,
                    cols: &row("zoom"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "a lone `z` elsewhere moves nothing");
        assert_eq!(pos(&rel), vec![(5, 0)]);
    }

    /// **ONE WIDE GLYPH IS ONE GLYPH** (2026-09-22, the review round). The
    /// floor counted cells, and a wide glyph owns two — so one CJK
    /// character standing anywhere else took the melt. It is released.
    #[test]
    fn one_blanked_wide_glyph_found_elsewhere_is_released_not_moved() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("\u{4f60}\0"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("\u{4f60}\0"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "one wide glyph elsewhere moves nothing");
        // Both halves are released. A wide cell is named once per half by
        // each of the run's two records, so the list is deduped here.
        let mut got = pos(&rel);
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, vec![(5, 0), (5, 1)]);
    }

    /// **A PARTIAL RELEASE TAKES ITS OWN SPACES, NOT THE RUN'S** (2026-09-22).
    /// A released tail of a run whose text still stands takes the spaces on
    /// its released side and none inside the standing text; a run with
    /// nothing standing still takes all of them.
    #[test]
    fn a_released_tail_of_a_standing_run_takes_only_its_own_spaces() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        // "ab cd ef gh": spaces at 2, 5 and 8.
        let cells: Vec<Cell> = (0..11u16).map(|c| cell(5, c, t0)).collect();
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd ef gh"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd      "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        let mut got = pos(&rel);
        got.sort_unstable();
        got.dedup();
        assert_eq!(
            got,
            vec![(5, 6), (5, 7), (5, 8), (5, 9), (5, 10)],
            "`ef gh` and the space between them go; the spaces at 2 and 5 stand"
        );
    }

    /// The moved-text search reads the run's OWN row too, at any other
    /// offset — a box re-laid four columns right on the same row is a
    /// re-laid box — and never the place the glyphs were.
    #[test]
    fn a_run_re_laid_elsewhere_on_its_own_row_moved_and_melts_fast() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("    hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(pos(&out), vec![(5, 0), (5, 1)], "moved along its row: fast");
        assert!(rel.is_empty());
    }

    #[test]
    fn a_different_glyph_retires_and_a_relaid_cell_arms_afresh() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(5, 0, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("h"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("H"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(pos(&out), vec![(5, 0)], "h -> H is an overwrite");
        // Re-laid at the same column (a new birth) over the new glyph: not
        // an overwrite of the old record — a fresh arm.
        let later = t0 + std::time::Duration::from_millis(90);
        let relaid = [cell(5, 0, later)];
        w.walk(
            &relaid,
            &[RowSample {
                row: 5,
                cols: &row("x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "the old record must not retire a re-laid cell"
        );
        assert_eq!(w.len(), 1);
        w.walk(
            &relaid,
            &[RowSample {
                row: 5,
                cols: &row("x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_wide_glyph_s_two_cells_are_retired_as_one_unit() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(5, 3, t0), cell(5, 4, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("   \u{4f60}\0"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 2);
        // Overwritten by two narrow glyphs: the lead changes AND the
        // continuation changes; each pushes its unit, so both cells are
        // named (twice — `retire_cells` stamps a cell once).
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("   ab"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, vec![(5, 3), (5, 4)]);
        // And when only the CONTINUATION cell is resident, its change still
        // names the lead too.
        let mut w = Witness::new();
        let cont = [cell(5, 4, t0)];
        w.walk(
            &cont,
            &[RowSample {
                row: 5,
                cols: &row("   \u{4f60}\0"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cont,
            &[RowSample {
                row: 5,
                cols: &row("     "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "blanked, and nowhere else: not the melt");
        assert_eq!(pos(&rel), vec![(5, 4), (5, 3)], "released as one unit");
        assert!(
            rel.iter().all(|&(_, _, born)| born == t0),
            "the partner is named with the SAME birth: one keystroke laid both"
        );
    }

    #[test]
    fn a_stale_cell_under_a_fresh_one_is_named_by_its_own_birth_and_the_fresh_one_never() {
        // One owner per cell holds per COHORT, so a key typed back over a
        // cell an abandoned cohort still owns leaves two cells resident at
        // one position. The stale one (cohort 7, laid under `r`) and the
        // fresh one (cohort 9, laid under the `X` that replaced it) are two
        // records; the walk names the stale cell by its birth and the fresh
        // cell is armed, whichever order the pool holds them in.
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(540);
        let mut out = Vec::new();
        let mut rel = Vec::new();
        for stale_first in [true, false] {
            let mut w = Witness::new();
            let stale = cell_of(5, 8, t0, 7);
            let blank = cell_of(5, 5, t0, 7);
            w.walk(
                &[blank, stale],
                &[RowSample {
                    row: 5,
                    cols: &row("hello world"),
                }],
                &[],
                Instant::now(),
                &mut out,
                &mut rel,
            );
            assert!(
                out.is_empty() && w.len() == 1,
                "the `r` is armed, the blank not"
            );
            let fresh = cell_of(5, 8, t1, 9);
            let pool = if stale_first {
                [blank, stale, fresh]
            } else {
                [fresh, blank, stale]
            };
            w.walk(
                &pool,
                &[RowSample {
                    row: 5,
                    cols: &row("hello woXld"),
                }],
                &[],
                Instant::now(),
                &mut out,
                &mut rel,
            );
            let mut got = out.clone();
            got.sort_unstable();
            assert_eq!(
                got,
                vec![(5, 5, t0), (5, 8, t0)],
                "stale first = {stale_first}: the stale cell and its run's blank, by birth — never the fresh cell"
            );
            assert_eq!(
                w.len(),
                1,
                "stale first = {stale_first}: the fresh cell's record stands alone"
            );
            // The next frame, the same text: the fresh cell is at peace.
            w.walk(
                &[fresh],
                &[RowSample {
                    row: 5,
                    cols: &row("hello woXld"),
                }],
                &[],
                Instant::now(),
                &mut out,
                &mut rel,
            );
            assert!(out.is_empty(), "stale first = {stale_first}");
        }
    }

    #[test]
    fn standing_run_index_matches_the_retirement_oracle() {
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(1);
        let mut cells = [
            cell_of(2, 0, t0, 7),
            cell_of(2, 1, t0, 7), // never-armed space
            cell_of(2, 2, t0, 7),
            cell_of(3, 0, t0, 7),  // same cohort ID, different row
            cell_of(3, 1, t0, 7),  // wide continuation
            cell_of(3, 2, t0, 7),  // never-armed space
            cell_of(5, 0, t0, 9),  // released record
            cell_of(5, 1, t0, 9),  // leaving cell cannot keep its run standing
            cell_of(6, 0, t0, 10), // no record
            cell_of(2, 0, t1, 11), // fresh owner at an old position
        ];
        cells[7].retire_at = Some(t0);
        let mut witness = Witness::new();
        for i in [0, 2, 3, 4, 7, 9] {
            witness.arm(
                &cells[i],
                Unit {
                    ch: if i == 3 || i == 4 { '你' } else { 'a' },
                    wide: i == 3 || i == 4,
                    cont: i == 4,
                },
                &[None; 4],
                t0,
            );
        }
        witness.hold_released(&cells[6]);
        let runs = [(5, 9), (2, 7), (6, 10), (3, 7), (2, 11)];

        // Every subset of the witnessed identities; order is deliberately
        // reversed and duplicates emulate both halves naming a wide unit.
        let armed = [0, 2, 3, 4, 6, 7, 9];
        for mask in 0..(1usize << armed.len()) {
            let mut retire = Vec::new();
            for (bit, &i) in armed.iter().enumerate().rev() {
                if mask & (1 << bit) != 0 {
                    let c = cells[i];
                    retire.push((c.row, c.col, c.born));
                    retire.push((c.row, c.col, c.born));
                }
            }
            let mut expected: Vec<_> = runs
                .iter()
                .copied()
                .filter(|&run| {
                    cells.iter().any(|c| {
                        !c.leaving()
                            && (c.row, c.cohort) == run
                            && witness.find(c.row, c.col, c.born).is_some()
                            && !retire.contains(&(c.row, c.col, c.born))
                    })
                })
                .collect();
            expected.sort_unstable();
            let original_order = retire.clone();
            witness.retired_runs.clear();
            witness.retired_runs.extend_from_slice(&runs);
            witness.find_standing_runs(&cells, &retire);
            assert_eq!(witness.standing_runs, expected, "mask {mask}");
            assert_eq!(retire, original_order, "verdict order must stay unchanged");
        }
        witness.retired_runs.clear();
        witness.find_standing_runs(&cells, &[]);
        assert!(
            witness.standing_runs.is_empty(),
            "no stale run state on an idle walk"
        );
    }

    #[test]
    fn a_blank_in_a_word_goes_with_its_run_and_stays_with_it_on_a_redraw() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        // "ab cd" as one run (cohort 7), and an unrelated run on the same
        // row (cohort 9) three columns on.
        let word: Vec<Cell> = (0..5u16).map(|c| cell_of(5, c, t0, 7)).collect();
        let other: Vec<Cell> = (8..10u16).map(|c| cell_of(5, c, t0, 9)).collect();
        let cells: Vec<Cell> = word.iter().chain(other.iter()).copied().collect();
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd   xy"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        assert_eq!(w.len(), 6, "the four letters and the two, not the blank");
        // The same text again: nothing moves, the blank stays with its word.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd   xy"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        // The word's row cleared under it, the other run's text still there:
        // the blank goes with ITS run; the other run is untouched.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("        xy"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "`ab cd` went and is nowhere: released, not melted"
        );
        let mut got = pos(&rel);
        got.sort_unstable();
        got.dedup();
        assert_eq!(
            got,
            vec![(5, 0), (5, 1), (5, 2), (5, 3), (5, 4)],
            "the blank goes with its run, on the run's own clock"
        );
        assert_eq!(
            w.len(),
            7,
            "the other run's records stand, and the released run's are kept — the blank's too"
        );
    }

    #[test]
    fn an_unsampled_row_keeps_its_records_and_a_dropped_cell_is_forgotten() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(5, 0, t0), cell(7, 0, t0)];
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("a"),
                },
                RowSample {
                    row: 7,
                    cols: &row("b"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 2);
        // Row 7 not sampled this frame: its record stays.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("a"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 2);
        // The ribbon dropped row 7's cell: its record goes with it.
        w.walk(
            &cells[..1],
            &[RowSample {
                row: 5,
                cols: &row("a"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 1);
        // A scroll carries the record with the cell.
        w.translate(5, 0);
        let moved = [cell(0, 0, t0)];
        w.walk(
            &moved,
            &[RowSample {
                row: 0,
                cols: &row("a"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && w.len() == 1,
            "the record followed the scroll"
        );
        w.walk(
            &moved,
            &[RowSample {
                row: 0,
                cols: &row(" "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        assert_eq!(pos(&rel), vec![(0, 0)], "blanked and nowhere: released");
    }

    #[test]
    fn a_band_move_carries_the_records_inside_it_drops_what_leaves_and_keeps_the_list_sorted() {
        // Codex's composer slides rows 3..=5 UP by one under a band above it
        // that stands still: the row-2 record stays at row 2, the row-3
        // record lands beside it on row 2 (the list must re-sort — the walk's
        // binary search reads it), row 5's moves to row 4, and a record on
        // row 3 that would be carried past the band's top edge is dropped
        // with its cell.
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(2, 5, t0), cell(3, 1, t0), cell(5, 0, t0)];
        let glyphs = row("abcdef");
        let samples: Vec<RowSample<'_>> = [2u16, 3, 5]
            .iter()
            .map(|&r| RowSample {
                row: r,
                cols: &glyphs,
            })
            .collect();
        w.walk(&cells, &samples, &[], Instant::now(), &mut out, &mut rel);
        assert_eq!(w.len(), 3);
        w.translate_band(3, 5, -1);
        let moved = [cell(2, 5, t0), cell(2, 1, t0), cell(4, 0, t0)];
        let samples: Vec<RowSample<'_>> = [2u16, 4]
            .iter()
            .map(|&r| RowSample {
                row: r,
                cols: &glyphs,
            })
            .collect();
        w.walk(&moved, &samples, &[], Instant::now(), &mut out, &mut rel);
        assert!(
            out.is_empty() && w.len() == 3,
            "every record followed its cell and none was re-armed"
        );
        // The text under the moved row-3 cell is now blank: found by the
        // binary search on the re-sorted list. Its run's blanked glyphs —
        // `b` at column 1 and `f` four columns on — stand at that spacing on
        // row 4 (`abcdef`, offset 1), so the run MOVED and is retired, in
        // column order.
        let blank = row("a     ");
        w.walk(
            &moved,
            &[
                RowSample {
                    row: 2,
                    cols: &blank,
                },
                RowSample {
                    row: 4,
                    cols: &glyphs,
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(pos(&out), vec![(2, 1), (2, 5)]);
        assert!(rel.is_empty());
        // A band move that carries a record past the top edge drops it.
        let mut w = Witness::new();
        w.walk(
            &[cell(3, 0, t0)],
            &[RowSample {
                row: 3,
                cols: &glyphs,
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.translate_band(3, 5, -1);
        assert!(w.is_empty(), "carried past the band's edge: gone");
    }

    #[test]
    fn the_witness_is_bounded_and_allocates_nothing_past_its_cap() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cap = w.seen.capacity();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells: Vec<Cell> = (0..(WITNESS_CAP as u16 + 40))
            .map(|i| cell(i / 200, i % 200, t0))
            .collect();
        let glyphs: Vec<char> = vec!['x'; 200];
        let samples: Vec<RowSample<'_>> = (0..6)
            .map(|r| RowSample {
                row: r,
                cols: &glyphs,
            })
            .collect();
        w.walk(&cells, &samples, &[], Instant::now(), &mut out, &mut rel);
        assert_eq!(w.len(), WITNESS_CAP);
        assert_eq!(w.seen.capacity(), cap, "no growth past the reserved cap");
    }

    /// **THE DEFERRAL'S SCRATCH IS RESERVED** ([`Witness::find_deferred_runs`],
    /// [`Witness::new`]). A full witness whose every record lies beside a
    /// withheld row — runs of two on the even rows, the odd rows withheld —
    /// and whose every glyph changed: each record is read into
    /// `blind_changed`, each run is deferred, and neither vector grows past
    /// what `new` reserved, so the first withheld frame, however long after
    /// warm-up, allocates nothing. RED with both started empty.
    #[test]
    fn the_deferral_s_scratch_is_reserved_and_never_grows() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let caps = (w.blind_changed.capacity(), w.deferred_runs.capacity());
        assert!(
            caps.0 >= WITNESS_CAP && caps.1 >= WITNESS_CAP,
            "reserved: {caps:?}"
        );
        let cap = u16::try_from(WITNESS_CAP).expect("a small cap");
        let cells: Vec<Cell> = (0..cap)
            .map(|i| cell_of(2 * (i / 200), i % 200, t0, u32::from(i / 2)))
            .collect();
        fn even(cols: &[char]) -> Vec<RowSample<'_>> {
            (0..6).map(|k| RowSample { row: 2 * k, cols }).collect()
        }
        let was: Vec<char> = vec!['x'; 200];
        let changed: Vec<char> = vec!['y'; 200];
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(&cells, &even(&was), &[], t0, &mut out, &mut rel);
        assert_eq!(w.len(), WITNESS_CAP, "fixture: a full witness");
        let odd: Vec<u16> = (0..6).map(|k| 2 * k + 1).collect();
        let later = t0 + Duration::from_millis(16);
        w.walk(&cells, &even(&changed), &odd, later, &mut out, &mut rel);
        assert!(out.is_empty() && rel.is_empty(), "every run waits a walk");
        assert_eq!(w.blind_changed.len(), WITNESS_CAP, "every record read");
        assert_eq!(w.deferred_runs.len(), WITNESS_CAP / 2, "every run deferred");
        assert_eq!(
            (w.blind_changed.capacity(), w.deferred_runs.capacity()),
            caps,
            "no growth past the reserved capacity"
        );
    }
}
