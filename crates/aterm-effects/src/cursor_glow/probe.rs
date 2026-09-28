// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ROW PROBES — the host's reads of the rows the light lands on (the witness
//! rows, the ribbon rows, the print anchor) and the trust each one carries.

use super::*;

/// What the host told us about ONE row flanking the probed cursor row
/// ([`CursorGlow::observe_neighbor_rows`]). The stars' landing gate reads
/// v2's occupancy bitsets, fed by the same call; the seam's CONTENT
/// witnesses read the retained glyphs this state guards — the soft-wrapped
/// caret, the carried and the lifted word ([`CursorGlow::soft_wrapped_caret`],
/// [`CursorGlow::carried_word_left`], [`CursorGlow::lifted_word`]) and the
/// minibuffer's run-end glyph ([`CursorGlow::probe_glyph_before`]). Only a
/// [`NbrProbe::Probed`] row has bytes to read; anything else is unknown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NbrProbe {
    /// The host supplied no neighbor capture with this probe (a host that
    /// only wires `observe_row`): the row's content is UNKNOWN, and no
    /// content witness gives a verdict on it.
    Unprobed,
    /// The host handed no row: the grid edge, or — from the GUI — a frame v2
    /// did not own, whose capture skipped the flanking rows. No bytes, so no
    /// content witness gives a verdict on it either. (The stars' gate reads
    /// v2's occupancy, which answers an off-grid row by construction.)
    OffGrid,
    /// Captured: the chars ride the matching `row_above_*`/`row_below_*`
    /// double buffer, rotated in lockstep with `row_cur`/`row_prev` — the
    /// only state whose bytes a witness reads.
    Probed,
}

/// Which detector families may consume one row probe — the repair for the
/// alt-screen `no-row-probe` retire class: on `less` /-search typing, `vi`
/// insert mode, and a per-key-echo TUI whose concurrent streamer writes at
/// another row via cursor save/restore (ESC 7/ESC 8), the repaint-blink epoch
/// never advances, so the host's old alt-screen probe gate
/// (`probe_ok = !alt || blink_recent`) WITHHELD the row probe entirely and
/// every honest typed keystroke's admission retired `no-row-probe` — zero
/// confirms, zero spawns, zero ink. The probe's CONTENT is exactly as
/// authentic in those shapes as anywhere else; the only thing unsound there
/// is the kill/poof INFERENCE ("kill hint + row shrink = erase"), because a
/// plain alt-screen app runs REGION SCROLLS that slide content through the
/// cursor row (Ctrl-U pages, it does not kill — the adversarial review that
/// installed the old gate). So the host now always captures and marks the
/// trust class instead of withholding the probe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProbeTrust {
    /// Primary screen, or an alt-screen app inside the repaint-blink window
    /// (a DECTCEM hide within a DEC-2026 synchronized update — Claude Code's
    /// per-keystroke repaint bracket): every consumer may read it, the
    /// kill/poof detector included.
    Full,
    /// Alt screen without repaint-blink evidence (less, vi, the ESC 7/ESC 8
    /// streamer shape): EXACT content proofs only — the typed/delete confirm
    /// and the unowned-steady anchor attestation, which compare content and
    /// fail closed on any difference. Every kill/poof branch must refuse it,
    /// or a Ctrl-U page scroll paired with a fresh kill hint puffs phantom
    /// smoke off rows the program merely scrolled.
    ContentOnly,
}

/// Metadata for one cursor-row content probe ([`CursorGlow::observe_row`]):
/// which row was sampled, where the caret sat, the row's FILL (one past the
/// last non-blank column), when, and its [`ProbeTrust`] class. The chars
/// themselves ride the double buffer (`row_cur`/`row_prev`).
#[derive(Clone, Copy)]
pub(super) struct RowProbe {
    pub(super) row: u16,
    /// The caret column at capture (kept for row identity/diagnostics; the
    /// vanished span itself comes from the prefix/suffix diff, which needs no
    /// caret cooperation from TUIs that rewrite rows wholesale).
    pub(super) caret: u16,
    pub(super) fill: u16,
    pub(super) at: Instant,
    /// What the host told us about the row ABOVE `row` (see [`NbrProbe`]).
    /// Rides the probe metadata so the neighbor buffers can never be read
    /// against the wrong probe generation — the meta and its buffers rotate
    /// as one unit in `poof_scan`.
    pub(super) above: NbrProbe,
    /// What the host told us about the row BELOW `row`.
    pub(super) below: NbrProbe,
    /// Which detector families may read this probe (see [`ProbeTrust`]).
    /// Rides the metadata and rotates with it, so a kill branch can never
    /// pair a full-trust `cur` against a content-only `prev` unnoticed.
    pub(super) trust: ProbeTrust,
}

/// One sampled grid row for Rainbow Kitty's CONTENT WITNESS
/// ([`CursorGlow::observe_ribbon_row`], [`CursorGlow::capture_ribbon_row`]):
/// the row, and its per-column chars
/// in the row probe's own convention. Slots are resident and reused, so the
/// steady frame allocates nothing. A host with its terminal lock can fill the
/// slot directly through [`CursorGlow::capture_ribbon_row`].
#[derive(Default)]
pub(super) struct WitnessRowBuf {
    pub(super) row: u16,
    pub(super) cols: Vec<char>,
}

impl CursorGlow {
    /// DROP the row probe (both buffers + metas + every poof proof). Called when
    /// the probed terminal is no longer the same content stream — a tab/pane
    /// switch re-pointing `ws.term`, or a scroll fence trip — so the diff can
    /// never compare two different terminals (or pre/post-scroll content) into
    /// a phantom poof. Capacity is retained: zero steady-state allocation.
    pub fn drop_row_probe(&mut self) {
        self.recent_typed_run = None;
        self.park_source_intact = None;
        self.row_cur.clear();
        self.row_prev.clear();
        self.row_cur_meta = None;
        self.row_prev_meta = None;
        self.clear_neighbor_rows();
        self.kill_hint = None;
        self.bs_poof_hint = None;
        self.bs_baseline = None;
    }

    /// Clear the content witnesses' neighbor captures (both generations,
    /// [`NbrProbe`]). Called
    /// wherever the row probe itself is dropped — the neighbor buffers are
    /// meta-gated (unreadable once the metas are `None`), so this is capacity-
    /// retaining hygiene keeping their lifecycle byte-identical to
    /// `row_cur`/`row_prev`'s.
    pub(super) fn clear_neighbor_rows(&mut self) {
        self.row_above_cur.clear();
        self.row_above_prev.clear();
        self.row_below_cur.clear();
        self.row_below_prev.clear();
    }

    /// HOST OBSERVATION: where the terminal's most recent PTY print run ended
    /// (`(row, col)` active-grid, `seq` bumped per print action), sampled
    /// under the same terminal lock as the cursor. `None` (an unwired host, a
    /// scrolled-back viewport) leaves the previous sample — the anchored lane
    /// then idles because its `seq` never advances. Feeds
    /// [`Self::echo_anchor_pass`] only; never a license by itself. A new
    /// sample handed over HERE carries no glyph: the run's last cell is
    /// unknown until [`Self::observe_print_anchor_glyph`] says otherwise, so
    /// a host or harness that never names it keeps the lane it always had.
    pub fn observe_print_anchor(&mut self, anchor: Option<(u16, u16, u64)>) {
        if let Some(sample) = anchor {
            self.print_anchor = Some(sample);
            self.print_anchor_glyph = None;
        }
    }

    /// [`Self::observe_print_anchor`] with the glyph at the run's LAST cell
    /// (`Terminal::print_anchor_glyph`, read under the SAME lock hold as the
    /// anchor, in the row probe's per-column convention: `' '` blank, `'\0'`
    /// a wide continuation). THE MINIBUFFER'S WITNESS: the visible-parked
    /// arm of [`Self::echo_anchor_pass`] lays a key's echo only where the
    /// run ends on that key's own glyph, and the caret row's flanking probes
    /// cannot see a status row two or more rows below the caret — zsh's
    /// Ctrl-R under a WRAPPED history match draws it under the match's last
    /// line. `glyph` rides only with a `Some` anchor; a `None` anchor keeps
    /// the previous sample and its glyph together.
    pub fn observe_print_anchor_glyph(
        &mut self,
        anchor: Option<(u16, u16, u64)>,
        glyph: Option<char>,
    ) {
        self.observe_print_anchor(anchor);
        if anchor.is_some() {
            self.print_anchor_glyph = glyph;
        }
    }

    /// HOST OBSERVATION: whether DECTCEM draws the caret this frame hands to
    /// [`Self::tick`]. The single-pane window hands over a hidden caret too,
    /// and says so here every frame; a host that hands over only drawn
    /// carets need never call it. Feeds the minibuffer gate of
    /// [`Self::echo_anchor_pass`] only: its law is zle's VISIBLE caret, and
    /// a hidden-caret TUI's echo on another row keeps the anchored lane it
    /// has on every other host.
    pub fn observe_caret_drawn(&mut self, drawn: bool) {
        self.caret_hidden = !drawn;
    }

    /// PER-FRAME ROW PROBE: hand the engine the cursor row's content as
    /// captured under the host's terminal lock — `cols` is per-COLUMN (the
    /// resolved lead char at its column, `'\0'` at wide continuations, `' '`
    /// for blanks, so column math survives CJK/emoji; see
    /// `Terminal::row_cols_into`). Copied into an internal double buffer
    /// (clear + extend, `mem::swap` rotation in `poof_scan`) — zero
    /// steady-state allocation. Call it IMMEDIATELY before [`Self::tick`];
    /// skipping a frame (scrolled back, split pane unwired, headless) simply
    /// leaves the previous probe in place and the poof detector idle.
    pub fn observe_row(&mut self, row: u16, caret: u16, cols: &[char], now: Instant) {
        self.observe_row_with_trust(row, caret, cols, now, ProbeTrust::Full);
    }

    /// [`Self::observe_row`] with an explicit [`ProbeTrust`] class.
    /// `ContentOnly` is the alt-screen-without-repaint-blink capture (`less`
    /// /-search typing, `vi` insert mode, the ESC 7/ESC 8 streamer TUI): it
    /// feeds the EXACT content proofs — the typed/delete confirm and the
    /// unowned-steady anchor attestation — while every kill/poof branch
    /// refuses it. Before this seam existed the host simply withheld the
    /// probe on those screens and every honest typed keystroke retired
    /// `no-row-probe`; the fix makes the probe AVAILABLE without widening
    /// the kill license one bit.
    pub fn observe_row_with_trust(
        &mut self,
        row: u16,
        caret: u16,
        cols: &[char],
        now: Instant,
        trust: ProbeTrust,
    ) {
        self.unsettle();
        self.row_cur.clear();
        self.row_cur.extend_from_slice(cols);
        // FILL = one past the last non-blank column. A wide continuation
        // ('\0') counts as filled — its lead glyph is content.
        let fill = cols
            .iter()
            .rposition(|&c| c != ' ')
            .map_or(0, |i| i + 1)
            .min(u16::MAX as usize) as u16;
        self.row_cur_meta = Some(RowProbe {
            row,
            caret,
            fill,
            at: now,
            // Neighbor knowledge arrives separately (`observe_neighbor_rows`,
            // called right after this by hosts that wire it); until it does,
            // the flanking rows are UNKNOWN and no content witness reads
            // them ([`NbrProbe`]).
            above: NbrProbe::Unprobed,
            below: NbrProbe::Unprobed,
            trust,
        });
        // SEAM POINT 13 (§5.4): the same probe, handed to v2's glyph gate.
        if self.v2.engaged() {
            self.v2_probe_row(i32::from(row), cols);
        }
    }

    /// The host's row probe → v2's glyph probe (§5.4's `probed_cell_glyph
    /// == Some(false)` gate): every char reduces to its
    /// [`rk::stardust::CellInk`] — a blank, a glyph (a wide glyph's `'\0'`
    /// continuation counts as ink exactly as the v1 fill counts it), or a
    /// LIGHT horizontal rule, the one glyph whose ink the sky band can never
    /// touch and which a bordered TUI prompt (Claude Code's input box) puts
    /// in every column of the row above the line being typed. The class view
    /// is a resident scratch refilled from the chars v1 just copied — no
    /// second grid scan, no allocation past the first row.
    pub(super) fn v2_probe_row(&mut self, row: i32, cols: &[char]) {
        self.v2_probe_scratch.clear();
        self.v2_probe_scratch
            .extend(cols.iter().map(|&c| rk::stardust::cell_ink(c)));
        self.v2
            .probe_mut()
            .probe_row_ink(row, &self.v2_probe_scratch);
    }

    /// **THE ROWS THE CONTENT WITNESS WANTS** (2026-09-12, the abandoned
    /// band; `rk::witness`): the distinct grid rows Rainbow Kitty's resident
    /// ribbon occupies — and, since 2026-09-21 (the band follows its text),
    /// the row above and below each, so the follow pass can see where a
    /// run's text went when a bottom-anchored box grew a row — then a
    /// WAITING KEY's source row and the rows one above and below it
    /// ([`rk::Engine::ribbon_rows_for`]), written into `out` (at most
    /// `out.len()` — size it [`CURSOR_WITNESS_ROWS`]); returns how many. The
    /// host captures the caret's row first (it rides the row probe the host
    /// already holds, so it costs no second grid read), then exactly these
    /// rows under its terminal lock, beside the row probe, and hands each to
    /// [`Self::observe_ribbon_row`] or [`Self::capture_ribbon_row`] before
    /// the tick. The host's [`CURSOR_WITNESS_ROWS`] slots hold all of it:
    /// the bands' [`rk::witness::WITNESS_ROWS`], the waiting key's
    /// [`rk::witness::ARMING_ROWS`] and the caret's row, so no row of the
    /// list is ever dropped for want of a slot. A caller with a shorter `out`
    /// gets the bands' rows first — a waiting key never evicts a band's row.
    /// `0` for every style but rainbow kitty — the other nine sample nothing
    /// and pay nothing.
    ///
    /// The source is needed even after the key's previous ribbon has faded,
    /// and its neighbours with it: the witness arms that key's record on
    /// this frame's samples, and a copy of its glyph standing beside it
    /// already is a twin only if that row is seen
    /// (`tests/erased_under_its_twin.rs`). The list handed out is recorded
    /// (`witness_asked`): the follow pass counts as withheld only a row of it
    /// the host did not deliver.
    pub fn ribbon_rows(&self, out: &mut [u16]) -> usize {
        if !self.v2.engaged() || out.is_empty() {
            return 0;
        }
        // A waiting key needs its source even after its previous ribbon has
        // faded. The same bounded row-sampling seam serves every host. A
        // delivered insert waiting for its echo (a paste, a Tab completion)
        // is a waiting key too: its whole width is armed on the frame it
        // echoes, on the row the hand was on when it was delivered.
        let source = self
            .held_park
            .filter(|p| p.cross_row)
            .map(|p| p.row)
            .or_else(|| {
                (self.type_hint.slots.iter().any(Option::is_some)
                    || !self.type_press_ring.is_empty())
                .then_some(self.last)
                .flatten()
                .map(|(row, _)| row)
            })
            .or_else(|| self.insert.armed.and_then(|licence| licence.row));
        let n = self.v2.ribbon_rows_for(source, out);
        let mut asked = [0u16; RIBBON_LIST_ROWS];
        let kept = n.min(asked.len());
        asked[..kept].copy_from_slice(&out[..kept]);
        self.witness_asked.set((asked, kept));
        n
    }

    /// **ONE SAMPLED ROW FOR THE CONTENT WITNESS**: `cols` is `row`'s
    /// per-column chars in the row probe's convention (the lead glyph at
    /// its column, `'\0'` at a wide continuation, `' '` for a blank — see
    /// `Terminal::row_cols_into`), captured under the host's terminal lock
    /// AFTER the PTY batch was applied. Copied into a resident slot (a
    /// clear and an extend: zero steady-state allocation); a row given twice
    /// in one frame replaces its earlier sample; past
    /// [`CURSOR_WITNESS_ROWS`] rows the sample is dropped. Read by
    /// the engine's witness right after this frame's [`Self::tick`] and
    /// never after: the tick takes the count to zero. Inert for every style
    /// but rainbow kitty.
    pub fn observe_ribbon_row(&mut self, row: u16, cols: &[char]) {
        let Some(slot) = self.witness_row_slot(row) else {
            return;
        };
        slot.cols.clear();
        slot.cols.extend_from_slice(cols);
    }

    /// Fill a content-witness row directly into its resident engine slot.
    /// The host invokes `fill` while holding the same terminal lock that
    /// captured this frame's caret and glyphs. It must preserve the ordinary
    /// per-column convention, including wide continuations (`'\0'`) and
    /// implicit blank tails. This avoids copying a far row from a temporary
    /// host buffer into the slot while that terminal lock is held. The
    /// callback is not invoked when Rainbow Kitty is disengaged or all
    /// witness slots are already occupied.
    pub fn capture_ribbon_row(&mut self, row: u16, fill: impl FnOnce(&mut Vec<char>)) {
        let Some(slot) = self.witness_row_slot(row) else {
            return;
        };
        slot.cols.clear();
        fill(&mut slot.cols);
    }

    pub(super) fn witness_row_slot(&mut self, row: u16) -> Option<&mut WitnessRowBuf> {
        if !self.v2.engaged() {
            return None;
        }
        let n = self.witness_rows_n;
        let slot = match self.witness_rows[..n.min(self.witness_rows.len())]
            .iter()
            .position(|s| s.row == row)
        {
            Some(i) => i,
            None => {
                if n >= CURSOR_WITNESS_ROWS {
                    return None;
                }
                if self.witness_rows.len() == n {
                    self.witness_rows.push(WitnessRowBuf::default());
                }
                self.witness_rows_n = n + 1;
                n
            }
        };
        let slot = &mut self.witness_rows[slot];
        slot.row = row;
        Some(slot)
    }

    /// Rainbow Kitty's laid ribbon, read-only — `Some` only while v2 owns
    /// the frame. What the abandoned-band conformance tests and the
    /// paint-conformance bind read the RESIDENT cells from (per row, per
    /// column); `trail status`'s `ribbon_segments=` stays the LIT count.
    #[must_use]
    pub fn v2_ribbon(&self) -> Option<&rk::ribbon::Ribbon> {
        self.v2.engaged().then(|| self.v2.ribbon())
    }

    /// STAR-LANDING NEIGHBOR PROBE: hand the engine the content of the two
    /// rows FLANKING the probed cursor row, captured under the SAME terminal
    /// lock as [`Self::observe_row`]'s row (same per-column char convention).
    /// The displaced rainbow ribbon stars paint in the pixel bands of the rows
    /// above/below the swept row, so the TEXT-FIRST gate must prove the
    /// LANDING cell blank — the spark's own cell says nothing about them.
    ///
    /// `None` = the row does not exist (grid edge): the landing band is
    /// window padding the effects box clips into, provably glyph-free, so the
    /// displaced placement stays licensed. Call it immediately AFTER
    /// `observe_row` (it no-ops when no fresh probe arrived this frame);
    /// skipping it entirely leaves the neighbors UNKNOWN and displaced stars
    /// fall back to the in-cell placement — never a guess over someone
    /// else's glyphs. The slices feed v2's occupancy bitsets directly, and
    /// are copied into the same rotate-on-`poof_scan` double buffering as
    /// the row probe for the seam's content witnesses ([`NbrProbe`]): zero
    /// steady-state allocation. The bitsets are written only while v2 is
    /// engaged; the copy is made whatever its state. So a row the host did
    /// not capture must come as `None` (the GUI's capture answers `false`
    /// for both rows on a frame v2 does not own), never as an empty slice,
    /// which the witnesses would take for a real blank row.
    pub fn observe_neighbor_rows(&mut self, above: Option<&[char]>, below: Option<&[char]>) {
        let Some(meta) = self.row_cur_meta.as_mut() else {
            return;
        };
        let probed_row = i32::from(meta.row);
        self.row_above_cur.clear();
        self.row_below_cur.clear();
        meta.above = match above {
            Some(cols) => {
                self.row_above_cur.extend_from_slice(cols);
                NbrProbe::Probed
            }
            None => NbrProbe::OffGrid,
        };
        meta.below = match below {
            Some(cols) => {
                self.row_below_cur.extend_from_slice(cols);
                NbrProbe::Probed
            }
            None => NbrProbe::OffGrid,
        };
        // SEAM POINT 13 (§5.4): the flanking rows feed v2's sky-band gate —
        // a star is born over row−1 only where this says it is blank. An
        // off-grid neighbour needs no write: the engine answers `Some(false)`
        // for any row outside the grid by construction.
        if self.v2.engaged() {
            if let Some(cols) = above {
                self.v2_probe_row(probed_row - 1, cols);
            }
            if let Some(cols) = below {
                self.v2_probe_row(probed_row + 1, cols);
            }
        }
    }
}
