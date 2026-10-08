// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ROW PROBES — the host's reads of the rows the light lands on (the witness
//! rows, the ribbon rows, the print anchor) and the trust each one carries.

use super::*;

/// What the host said about one row flanking the probed cursor row
/// ([`CursorGlow::observe_neighbor_rows`]). The stars' landing gate reads v2's
/// occupancy bitsets from the same call; the seam's content witnesses read the
/// retained glyphs this guards: the soft-wrapped caret, the carried and lifted
/// word ([`CursorGlow::soft_wrapped_caret`], [`CursorGlow::carried_word_left`],
/// [`CursorGlow::lifted_word`]) and the minibuffer's run-end glyph
/// ([`CursorGlow::probe_glyph_before`]). Only a [`NbrProbe::Probed`] row has
/// bytes; anything else is unknown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NbrProbe {
    /// No neighbour capture with this probe (a host wiring only `observe_row`):
    /// the row is unknown and no content witness judges it.
    Unprobed,
    /// No row handed: the grid edge, or (from the GUI) a frame v2 did not own,
    /// whose capture skipped the flanking rows. No witness judges it. (The
    /// stars' gate reads v2's occupancy, which answers off-grid rows itself.)
    OffGrid,
    /// Captured: the chars ride the matching `row_above_*`/`row_below_*` double
    /// buffer, rotated with `row_cur`/`row_prev`.
    Probed,
}

/// Which detector families may consume one row probe. On the alt screen
/// without repaint-blink evidence (`less` /-search typing, `vi` insert mode, a
/// per-key-echo TUI whose streamer writes elsewhere via ESC 7/ESC 8) the
/// probe's content is as authentic as anywhere, but the kill/poof inference
/// ("kill hint + row shrink = erase") is not, because alt-screen apps run
/// region scrolls through the cursor row (Ctrl-U pages rather than kills). So
/// the host always captures and marks the trust class rather than withholding
/// the probe, which would retire every honest keystroke `no-row-probe`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProbeTrust {
    /// Primary screen, or an alt-screen app inside the repaint-blink window (a
    /// DECTCEM hide inside a DEC-2026 update: Claude Code's per-keystroke
    /// bracket): every consumer may read it, kill/poof included.
    Full,
    /// Alt screen without repaint-blink evidence (less, vi, the ESC 7/ESC 8
    /// streamer): exact content proofs only (the typed/delete confirm and the
    /// unowned-steady anchor attestation, which fail closed on any difference).
    /// Every kill/poof branch refuses it, so a Ctrl-U page scroll with a fresh
    /// kill hint puffs no smoke.
    ContentOnly,
}

/// Metadata for one cursor-row content probe ([`CursorGlow::observe_row`]): the
/// sampled row, the caret, the row's fill (one past the last non-blank column),
/// when, and its [`ProbeTrust`] class. The chars ride the double buffer
/// (`row_cur`/`row_prev`).
#[derive(Clone, Copy)]
pub(super) struct RowProbe {
    pub(super) row: u16,
    /// The caret column at capture (row identity and diagnostics; the vanished
    /// span comes from the prefix/suffix diff, which needs no caret cooperation
    /// from TUIs that rewrite rows wholesale).
    pub(super) caret: u16,
    pub(super) fill: u16,
    pub(super) at: Instant,
    /// What the host said about the row above `row` (see [`NbrProbe`]). Rides
    /// the probe metadata, so the neighbour buffers are never read against the
    /// wrong generation; meta and buffers rotate together in `poof_scan`.
    pub(super) above: NbrProbe,
    /// What the host told us about the row BELOW `row`.
    pub(super) below: NbrProbe,
    /// Which detector families may read this probe (see [`ProbeTrust`]). Rotates
    /// with the metadata, so a kill branch cannot pair a full-trust `cur` with a
    /// content-only `prev` unnoticed.
    pub(super) trust: ProbeTrust,
}

/// One sampled grid row for Rainbow Kitty's content witness
/// ([`CursorGlow::observe_ribbon_row`], [`CursorGlow::capture_ribbon_row`]): the
/// row and its per-column chars in the row probe's convention. Slots are
/// resident and reused, so a steady frame allocates nothing.
#[derive(Default)]
pub(super) struct WitnessRowBuf {
    pub(super) row: u16,
    pub(super) cols: Vec<char>,
}

impl CursorGlow {
    /// Drop the row probe (both buffers, metas and every poof proof) when the
    /// probed terminal is no longer the same content stream (a tab/pane switch,
    /// a scroll fence), so the diff never compares two streams into a phantom
    /// poof. Capacity is kept.
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

    /// Clear the content witnesses' neighbour captures (both generations,
    /// [`NbrProbe`]) wherever the row probe is dropped. The buffers are
    /// meta-gated, so this keeps their lifecycle in step with
    /// `row_cur`/`row_prev`, retaining capacity.
    pub(super) fn clear_neighbor_rows(&mut self) {
        self.row_above_cur.clear();
        self.row_above_prev.clear();
        self.row_below_cur.clear();
        self.row_below_prev.clear();
    }

    /// Host observation: where the terminal's latest PTY print run ended
    /// (`(row, col)` active-grid, `seq` bumped per print action), sampled under
    /// the same lock as the cursor. `None` (an unwired host, a scrolled-back
    /// viewport) keeps the previous sample, so the anchored lane idles. Feeds
    /// [`Self::echo_anchor_pass`] only; never a licence by itself. A sample
    /// handed here carries no glyph (the run's last cell is unknown until
    /// [`Self::observe_print_anchor_glyph`] says otherwise).
    pub fn observe_print_anchor(&mut self, anchor: Option<(u16, u16, u64)>) {
        if let Some(sample) = anchor {
            self.print_anchor = Some(sample);
            self.print_anchor_glyph = None;
        }
    }

    /// [`Self::observe_print_anchor`] with the glyph at the run's last cell
    /// (`Terminal::print_anchor_glyph`, read under the same lock hold, in the row
    /// probe's convention: `' '` blank, `'\0'` a wide continuation). The
    /// minibuffer's witness: the visible-parked arm of
    /// [`Self::echo_anchor_pass`] lays a key's echo only where the run ends on
    /// that key's glyph, and the caret row's flanking probes cannot see a status
    /// row two or more rows below (zsh's Ctrl-R under a wrapped match). `glyph`
    /// rides only with a `Some` anchor; `None` keeps the previous sample and
    /// glyph together.
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

    /// Host observation: whether DECTCEM draws the caret handed to
    /// [`Self::tick`] this frame. The single-pane window hands over hidden carets
    /// too and says so each frame; a host handing over only drawn carets need not
    /// call it. Feeds only the minibuffer gate of [`Self::echo_anchor_pass`],
    /// whose law is about a visible caret.
    pub fn observe_caret_drawn(&mut self, drawn: bool) {
        self.caret_hidden = !drawn;
    }

    /// Per-frame row probe: the cursor row's content captured under the host's
    /// terminal lock, `cols` per column (lead char at its column, `'\0'` at wide
    /// continuations, `' '` for blanks; see `Terminal::row_cols_into`). Copied
    /// into an internal double buffer (rotated in `poof_scan`) with no
    /// steady-state allocation. Call it immediately before [`Self::tick`];
    /// skipping a frame leaves the previous probe and the poof detector idle.
    pub fn observe_row(&mut self, row: u16, caret: u16, cols: &[char], now: Instant) {
        self.observe_row_with_trust(row, caret, cols, now, ProbeTrust::Full);
    }

    /// [`Self::observe_row`] with an explicit [`ProbeTrust`] class.
    /// `ContentOnly` (alt screen without repaint blink: `less` /-search, `vi`
    /// insert mode, the ESC 7/ESC 8 streamer) feeds the exact content proofs,
    /// the typed/delete confirm and the unowned-steady anchor attestation,
    /// while every kill/poof branch refuses it.
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
            // Neighbour knowledge arrives separately (`observe_neighbor_rows`,
            // right after this); until then the flanking rows are unknown
            // ([`NbrProbe`]).
            above: NbrProbe::Unprobed,
            below: NbrProbe::Unprobed,
            trust,
        });
        // The same probe feeds v2's glyph gate (§5.4).
        if self.v2.engaged() {
            self.v2_probe_row(i32::from(row), cols);
        }
    }

    /// The host's row probe to v2's glyph probe (§5.4's `probed_cell_glyph ==
    /// Some(false)` gate): each char reduces to an [`rk::stardust::CellInk`]: a
    /// blank, a glyph (a wide `'\0'` continuation counts as ink), or a light
    /// horizontal rule, whose ink the sky band never touches and which a
    /// bordered TUI prompt (Claude Code's input box) puts across the row above
    /// the typed line. Refilled from the chars just copied, with no second scan
    /// and no allocation past the first row.
    pub(super) fn v2_probe_row(&mut self, row: i32, cols: &[char]) {
        self.v2_probe_scratch.clear();
        self.v2_probe_scratch
            .extend(cols.iter().map(|&c| rk::stardust::cell_ink(c)));
        self.v2
            .probe_mut()
            .probe_row_ink(row, &self.v2_probe_scratch);
    }

    /// The rows the content witness wants (`rk::witness`): the distinct rows
    /// Rainbow Kitty's ribbon occupies and the rows above and below each (so the
    /// follow pass sees where a run's text went), then a waiting key's source
    /// row and its neighbours ([`rk::Engine::ribbon_rows_for`]), written into
    /// `out` (size it [`CURSOR_WITNESS_ROWS`]); returns how many. The host
    /// captures the caret's row first (from the row probe it already holds),
    /// then these rows under its terminal lock, and hands each to
    /// [`Self::observe_ribbon_row`] or [`Self::capture_ribbon_row`] before the
    /// tick. [`CURSOR_WITNESS_ROWS`] holds the bands'
    /// [`rk::witness::WITNESS_ROWS`], the key's [`rk::witness::ARMING_ROWS`] and
    /// the caret's row; with a shorter `out` the bands' rows come first. `0` for
    /// every other style.
    ///
    /// The source and its neighbours are needed even after the key's previous
    /// ribbon faded: the witness arms that key's record on this frame's samples,
    /// and a copy of its glyph beside it is a twin only if that row is seen
    /// (`tests/erased_under_its_twin.rs`). The list is recorded (`witness_asked`)
    /// so the follow pass counts only its undelivered rows as withheld.
    pub fn ribbon_rows(&self, out: &mut [u16]) -> usize {
        if !self.v2.engaged() || out.is_empty() {
            return 0;
        }
        // A waiting key needs its source even after its previous ribbon faded.
        // A delivered insert awaiting its echo (a paste, a Tab completion) is a
        // waiting key too: its whole width is armed on its echo frame, on the
        // row the hand was on at delivery.
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

    /// One sampled row for the content witness: `row`'s per-column chars in the
    /// row probe's convention (`Terminal::row_cols_into`), captured under the
    /// host's lock after the PTY batch was applied. Copied into a resident slot
    /// (no steady-state allocation); a row given twice in a frame replaces the
    /// earlier sample; past [`CURSOR_WITNESS_ROWS`] rows it is dropped. Read by
    /// the witness right after this frame's [`Self::tick`], which takes the
    /// count to zero. Inert for other styles.
    pub fn observe_ribbon_row(&mut self, row: u16, cols: &[char]) {
        let Some(slot) = self.witness_row_slot(row) else {
            return;
        };
        slot.cols.clear();
        slot.cols.extend_from_slice(cols);
    }

    /// Fill a content-witness row directly into its resident slot. The host calls
    /// `fill` under the same terminal lock that captured this frame's caret and
    /// glyphs, keeping the per-column convention (wide continuations `'\0'`,
    /// implicit blank tails), which avoids an extra copy under the lock. `fill`
    /// is not called when Rainbow Kitty is disengaged or all slots are full.
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

    /// Rainbow Kitty's laid ribbon, read-only, `Some` only while v2 owns the
    /// frame. Conformance tests read the resident cells from it; `trail
    /// status`'s `ribbon_segments=` stays the lit count.
    #[must_use]
    pub fn v2_ribbon(&self) -> Option<&rk::ribbon::Ribbon> {
        self.v2.engaged().then(|| self.v2.ribbon())
    }

    /// Star-landing neighbour probe: the two rows flanking the probed cursor row,
    /// captured under the same lock as [`Self::observe_row`]'s row (same
    /// convention). Displaced ribbon stars paint in the rows above and below the
    /// swept row, so the text-first gate must prove the landing cell blank.
    ///
    /// `None` means the row does not exist (grid edge): the landing band is
    /// window padding, glyph-free, so displaced placement stays licensed. Call
    /// right after `observe_row` (a no-op without a fresh probe); never calling
    /// it leaves the neighbours unknown and displaced stars fall back in-cell.
    /// The slices feed v2's occupancy bitsets (only while engaged) and are
    /// copied into the probe's double buffer for the content witnesses
    /// ([`NbrProbe`]) with no steady-state allocation. An uncaptured row must
    /// come as `None`, never an empty slice, which would read as a blank row.
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
        // The flanking rows feed v2's sky-band gate (§5.4): a star is born over
        // row−1 only where this says it is blank. An off-grid neighbour needs
        // no write; the engine answers `Some(false)` for it.
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
