// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every corpus case the two engines do not agree on, with its triage, and
//! the answer aterm must give on it instead.
//!
//! Each divergence the oracle finds is one of three things:
//!
//! - **an aterm bug**: fixed in aterm with a regression test beside the fix,
//!   and the case stays in the corpus (the `Regression:` comments in
//!   `corpus.rs`). Those found so far: a refused SGR 38/48/58 colour
//!   re-dispatched its tail, DECSEL/DECSED erased to the default background,
//!   RI kept the pending wrap at the top margin and left a one-row screen
//!   standing, and a `?47`/`?1047` switch left the incoming buffer's BCE
//!   background stale. The pinned fold models each of those behaviours, so
//!   each case is held to agree with it byte by byte.
//! - **an astream-term gap** ([`Triage::Astream`]): the fold lacks an xterm
//!   behaviour, and an astream branch (the entry names it) adds it. The aterm
//!   pin is not moved by the change that finds the gap; whoever lands that
//!   branch on astream's main bumps it. Until then the case must still
//!   diverge, AND aterm's screen after its last byte must be the entry's
//!   [`Want`]: xterm's answer, written out, and the answer the fold on that
//!   branch gives. So an excluded case still checks aterm while the fold
//!   cannot: a regression to the fold's wrong answer fails the written
//!   answer, and the engines agreeing while the fold's [`Caps`] probe has not
//!   moved is read the same way, never as a stale entry. Once the pin carries
//!   the fix (the probe flips) the entry, written answer and all, must be
//!   deleted, and the case is held to agree byte by byte. None is open at
//!   this pin: the thirty the oracle's first run found (VT/FF, erases at a
//!   pending wrap, ED 3, BCE on a scroll, ICH/DCH/ECH/IL/DL, IND/NEL/RI, the
//!   alternate-screen rules, the DECSC pen and UTF-8 maximal subparts) came
//!   with astream's `fix/term-oracle-divergences` (astream main 4e91e82) and
//!   were deleted in the bump that pinned it.
//! - **a deliberate difference** ([`Triage::ByDesign`]): the engines differ on
//!   purpose; the entry says why, against xterm, and its written answer pins
//!   aterm's side. It must keep diverging while its condition holds.
//!
//! `the_corpus_agrees_or_is_excluded` fails on a stale entry, on a case that
//! diverges unexcused, and on aterm missing an entry's written answer.

use super::caps::Caps;
use super::engines::{Ink, Snap};

pub enum Triage {
    /// The pinned fold lacks the behaviour; astream's `branch` adds it.
    /// `fixed` reads the capability off the fold: false while the case must
    /// diverge, true once the pin carries the fix (and the entry must go).
    #[expect(
        dead_code,
        reason = "no astream-term gap is open at this pin; the next one the oracle \
                  finds is an entry with this triage, and this expect goes with it"
    )]
    Astream {
        branch: &'static str,
        fixed: fn(&Caps) -> bool,
    },
    /// The engines differ by design; `diverges` says when the difference is
    /// visible with this fold.
    ByDesign { diverges: fn(&Caps) -> bool },
}

/// aterm's screen after an excluded case's last byte, written out by hand
/// from xterm's source (the entry's `why` cites it), in the oracle's
/// projection ([`Snap`]).
pub struct Want {
    /// Each row's text, trailing blanks trimmed; rows past the list are blank.
    pub rows: &'static [&'static str],
    /// `(row, col)`, 0-based; `col == cols` while a wrap is pending.
    pub cursor: (u16, u16),
    /// The alternate screen is up.
    pub alt: bool,
    /// Every run of cells whose rendition is not [`Ink::PLAIN`], as
    /// `(row, first col, past the last col, ink)`; every other cell is plain.
    pub inks: &'static [(u16, u16, u16, Ink)],
}

impl Want {
    /// The first way `snap` misses this answer, in words; `None` when it
    /// gives it exactly.
    pub fn missed_by(&self, snap: &Snap) -> Option<String> {
        for r in 0..snap.rows {
            let want = self.rows.get(usize::from(r)).copied().unwrap_or("");
            let got = snap.row_text(r);
            if got != want {
                return Some(format!("row {r} is {got:?}, not {want:?}"));
            }
        }
        if self.rows.len() > usize::from(snap.rows) {
            return Some(format!("the answer has {} rows", self.rows.len()));
        }
        if snap.cursor != self.cursor {
            return Some(format!("cursor {:?}, not {:?}", snap.cursor, self.cursor));
        }
        if snap.alt_screen != self.alt {
            return Some(format!(
                "alternate screen {}, not {}",
                snap.alt_screen, self.alt
            ));
        }
        for r in 0..snap.rows {
            for c in 0..snap.cols {
                let want = self
                    .inks
                    .iter()
                    .find(|&&(row, from, to, _)| row == r && (from..to).contains(&c))
                    .map_or(Ink::PLAIN, |&(.., ink)| ink);
                let got = snap.cells[usize::from(r) * usize::from(snap.cols) + usize::from(c)].ink;
                if got != want {
                    return Some(format!("cell ({r},{c}) is {got:?}, not {want:?}"));
                }
            }
        }
        None
    }
}

pub struct Exclusion {
    /// The corpus case it covers.
    pub case: &'static str,
    pub triage: Triage,
    /// What aterm must show after the case's last byte.
    pub want: Want,
    /// What differs, and why, citing xterm.
    pub why: &'static str,
}

/// A [`Want`] with no rendition but the default pen on the main screen.
const fn text(rows: &'static [&'static str], cursor: (u16, u16)) -> Want {
    Want {
        rows,
        cursor,
        alt: false,
        inks: &[],
    }
}

pub const EXCLUSIONS: &[Exclusion] = &[
    Exclusion {
        case: "stray_1049l_with_a_saved_slot",
        triage: Triage::ByDesign { diverges: |_| true },
        want: text(&["", "", "", "        X"], (3, 9)),
        why: "a ?1049l with the main screen up: xterm's 1049 reset is \
              FromAlternate; CursorRestore (charproc.c srm_OPT_ALTBUF_CURSOR) and \
              only the first half needs the alternate screen, so xterm (and \
              Ghostty, and the fold) restore the main DECSC slot. aterm moves \
              nothing, as Alacritty and WezTerm do: its orphaned-screen recovery \
              (a prompt mark after a full-screen app died) restores that slot \
              without consuming it, and a later `tput rmcup` or a script's \
              unconditional rmcup would jump the cursor onto live output \
              (aterm-core handler_dec.rs `exit_alternate_screen`, \
              `a_rmcup_after_the_orphaned_screen_recovery_keeps_the_cursor`). \
              The generator never sends a ?1049l on the main screen.",
    },
    Exclusion {
        case: "stray_1049l_with_nothing_saved",
        triage: Triage::ByDesign { diverges: |_| true },
        want: Want {
            rows: &["        X"],
            cursor: (0, 9),
            alt: false,
            inks: &[(0, 8, 9, Ink::fg(1).bold())],
        },
        why: "as stray_1049l_with_a_saved_slot, with no slot saved: xterm's \
              CursorRestore of the zeroed SavedCursor homes the cursor and resets \
              the rendition; aterm moves nothing and keeps the pen",
    },
    Exclusion {
        case: "sixel_dcs_is_an_image",
        triage: Triage::ByDesign { diverges: |_| true },
        want: text(&["a", "b"], (1, 1)),
        why: "aterm built with `sixel` (as aterm-conformance builds it; DA1 \
              advertises 4) decodes `DCS q` and places the image, moving the \
              cursor below it as xterm's sixel support does (graphics_sixel.c); \
              the fold consumes every DCS payload unpainted. Images are outside \
              the fold's subset, so the generator never sends one.",
    },
    Exclusion {
        case: "osc133_a_leaves_an_orphaned_alt_screen",
        triage: Triage::ByDesign { diverges: |_| true },
        want: text(&[], (0, 0)),
        why: "aterm reads a shell prompt mark (OSC 133/633 `A`) on the alternate \
              screen as the full-screen app having died without its ?1049l (a \
              SIGKILLed pager; conhost never sends it for a client it outlived), \
              archives the screen and leaves it (aterm-core handler_osc_shell.rs \
              `prompt_leaves_orphaned_alt_screen`). xterm ignores OSC 133; so does \
              the fold. The recovery is the product's, behind the nonce gate when \
              the host demands one.",
    },
    Exclusion {
        case: "a_1049_visit_leaves_no_buffer_behind",
        triage: Triage::ByDesign {
            diverges: |c| c.alt_persists,
        },
        want: Want {
            rows: &[],
            cursor: (0, 0),
            alt: true,
            inks: &[],
        },
        why: "xterm keeps one alternate buffer for every mode, so a ?47h after a \
              ?1049h ... ?1049l visit shows that visit's last frame (charproc.c: \
              the 1049 reset is FromAlternate, which only swaps). aterm frees the \
              buffer when a 1049 visit ends and starts each 1049 visit from a fresh \
              one: its alternate-screen archive keeps the rows a full-screen app \
              scrolled off and never re-shows a finished app's screen (aterm-core \
              `host_leave_of_a_mode_47_screen_keeps_the_cursor_where_the_app_had_it` \
              pins the dead-screen half). Modern apps enter with ?1049, whose clear \
              makes the two agree again; only a 47/1047 entry right after a 1049 \
              visit shows the difference. Visible once the fold keeps a persistent \
              buffer (alt_persists).",
    },
    Exclusion {
        case: "ris_keeps_the_alternate_buffer",
        triage: Triage::ByDesign {
            diverges: |c| c.ris_keeps_alt,
        },
        want: Want {
            rows: &[],
            cursor: (0, 0),
            alt: true,
            inks: &[],
        },
        why: "xterm's full reset (charproc.c `ReallyReset`) runs \
              `FromAlternate(xw, True)`, which on the main screen does nothing, \
              and `ClearScreen` of the main buffer, so the alternate buffer \
              survives a RIS and a later ?47h shows `X` (the fold does the same \
              once it keeps it, ris_keeps_alt). aterm's RIS is a whole reset: it \
              drops the alternate buffer and both DECSC slots (aterm-core \
              reset.rs, `*alt_grid = None; cursor_save.reset()`), as it frees \
              the buffer after a 1049 visit (`a_1049_visit_leaves_no_buffer_behind`), \
              so a reset terminal never re-shows an earlier program's screen. \
              The generator enters with ?1049h after a RIS.",
    },
    Exclusion {
        case: "ris_keeps_the_alternate_slot",
        triage: Triage::ByDesign {
            diverges: |c| c.ris_keeps_alt,
        },
        want: Want {
            rows: &["Z"],
            cursor: (0, 1),
            alt: true,
            inks: &[],
        },
        why: "as ris_keeps_the_alternate_buffer, for the alternate screen's own \
              DECSC slot: xterm's reset writes only the main one (`CursorSave` \
              with `whichBuf == 0`), so `sc[1]` survives and the DECRC after the \
              next ?1049h restores (1,4), where xterm puts `Z`. aterm's RIS \
              empties the slot, and the restore of an empty slot homes. The \
              generator restores on the alternate screen after a RIS only once \
              a save there wrote the slot again.",
    },
];

/// The exclusion covering `case`, if any.
pub fn find(case: &str) -> Option<&'static Exclusion> {
    EXCLUSIONS.iter().find(|e| e.case == case)
}
