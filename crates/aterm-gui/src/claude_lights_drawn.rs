// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE AUTOPILOT CHIP AS DRAWN (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §12.5.3, slice 5b): `aterm ctl chrome`'s `autopilot` lines, one per
//! supervised pane in every window, so a driver reads the chip as the person
//! sees it instead of inferring it from a screenshot.
//!
//! `face=` is the chip's face NOW: [`ChipFace::of`] over the session's view
//! ([`App::session_autopilot`]) and its glass ([`App::chip_glass`]), the
//! function and the glass the frame draws from, for every supervised pane,
//! in front or not. `drawn=` is the chip's cells AS THE LAST FRAME DREW THEM,
//! read from the block that frame kept home ([`WindowLights::fitted`]), and
//! only for a pane that frame drew: a fitted entry the frame asked for
//! (`used`), in the window's front tab, whose chip reached the glass (its
//! hits) and stayed on it (a band spliced after the footer that claims the
//! chip's row hides it, as every other reader of the hits rules). A pane in
//! a tab behind has no drawn cells, or for one frame stale ones, and says
//! `drawn=- why=not-drawn`, never older cells. `spoken=` is what the drawn
//! chip's accessibility node says (`App::claude_chips_a11y`'s label), `-`
//! where no chip is drawn, and `description=` its description (the W4 title
//! slice: why Land holds, a harness's need, the pause, the newest act;
//! `App::autopilot_spoken`), `-` where it has none or no chip is drawn.
//! `window=` tells apart the lines of a session co-viewed in two windows.
//!
//! Composed only when `chrome` asks: the frame path gains nothing (no field,
//! no copy, no count), and the block is read where the frame left it.

use aterm_core::terminal::RenderCell;

use super::{Chip, ChipFace, SessionLights, WindowLights};
use crate::App;
use crate::autopilot::AutopilotView;

/// What the last frame drew of one pane's Autopilot chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Drawn {
    /// The chip's cells as the frame wrote them, a wide glyph's right half
    /// (its continuation cell) left out.
    Cells(String),
    /// The last frame did not draw the pane's composer rule, or a band it
    /// spliced after the footer covers the chip's row: a tab behind, a pane
    /// a zoom hides, a pane scrolled back, a screen with no rule, the link
    /// caption over the rule.
    NoRule,
    /// The last frame drew the pane's rule, and the chip did not fit in it:
    /// too narrow a rule, or one with no run of plain rule glyphs to write
    /// into at all.
    NoRoom,
    /// The last frame fitted the pane's rule, and no harness host drew a
    /// chip there: the view that frame noted was `unhosted`.
    NoHost,
    /// The program has no composer footer to draw a chip in
    /// ([`crate::claude_footer::drawn_for`]: Gemini CLI, codex-alab, an agent
    /// started under a script runtime's name): `face=-` too.
    NoFooter,
}

impl Drawn {
    /// Its `why=` word, `None` for drawn cells.
    pub(crate) const fn why(&self) -> Option<&'static str> {
        match self {
            Drawn::Cells(_) => None,
            Drawn::NoRule => Some("not-drawn"),
            Drawn::NoRoom => Some("no-room"),
            Drawn::NoHost => Some("no-host"),
            Drawn::NoFooter => Some("no-footer"),
        }
    }
}

/// The face's `face=` word: one per [`ChipFace`], none of them a glyph.
pub(crate) const fn face_word(face: ChipFace) -> &'static str {
    match face {
        ChipFace::Cruise => "cruise",
        ChipFace::CruiseUntil => "cruise-until",
        ChipFace::Land => "land",
        ChipFace::Landed => "landed",
        ChipFace::Theirs => "theirs",
        ChipFace::Off => "off",
        ChipFace::MasterOff => "master-off",
        ChipFace::AgentOff(_) => "agent-off",
        ChipFace::Unhosted => "unhosted",
    }
}

/// The text of a chip's cells: each cell's character, a wide glyph's
/// continuation cell left out.
fn cells_text(cells: &[RenderCell]) -> String {
    cells.iter().filter(|c| !c.wide).map(|c| c.ch).collect()
}

/// One `autopilot` line of `chrome` (§12.5.3): `autopilot sid=<sid>
/// window=<id> program=<name> face=<word|-> drawn="<cells>"|drawn=-
/// why=<word> spoken="<label>"|spoken=- description="<words>"|description=-`.
/// `window=` is the window's logical id, as `ls` prints it. The quoted
/// values are quoted as the `statusmenu` lines' are. `spoken` is the drawn
/// chip's node's label and its description, `None` where no chip is drawn.
pub(crate) fn drawn_chip_line(
    sid: &str,
    window: u64,
    program: aterm_phase::Program,
    face: Option<ChipFace>,
    drawn: &Drawn,
    spoken: Option<(&str, Option<&str>)>,
) -> String {
    let face = face.map_or("-", face_word);
    let mut line = format!(
        "autopilot sid={sid} window={window} program={} face={face}",
        program.name()
    );
    match drawn {
        Drawn::Cells(cells) => line.push_str(&format!(" drawn={cells:?}")),
        other => {
            line.push_str(" drawn=-");
            if let Some(why) = other.why() {
                line.push_str(" why=");
                line.push_str(why);
            }
        }
    }
    match spoken {
        Some((spoken, description)) => {
            line.push_str(&format!(" spoken={spoken:?}"));
            match description {
                Some(description) => line.push_str(&format!(" description={description:?}")),
                None => line.push_str(" description=-"),
            }
        }
        None => line.push_str(" spoken=- description=-"),
    }
    line
}

impl WindowLights {
    /// What the last frame of this window drew of `session`'s Autopilot chip
    /// in leaf `pane` of its front tab (the leaf index the frame's plan gave
    /// it, [`super::Fitted::pane`]): its cells where the frame asked for the
    /// pane's block (`used`), the block holds the chip, and the chip reached
    /// the glass (a hit of it) on a row no band claimed after (`covered`,
    /// the window's chrome register: [`App::chrome_owns_terminal_row`]); else
    /// why not.
    pub(crate) fn drawn_chip(
        &self,
        session: u64,
        pane: usize,
        covered: impl Fn(usize) -> bool,
    ) -> Drawn {
        let Some(fitted) = self
            .fitted
            .iter()
            .find(|f| f.session == session && f.pane == pane && f.used)
        else {
            // No block asked for: the frame found the rule on the glass with
            // no run in it for the chips (`footers`, room 0), or no rule.
            return if self.footers.iter().any(|(s, _)| *s == session) {
                Drawn::NoRoom
            } else {
                Drawn::NoRule
            };
        };
        // The chip's painted cells, and whether any is left on the glass: a
        // band that later claims the row hides the chip (`LightHit::term_row`).
        let (painted, seen) = self
            .hits
            .iter()
            .filter(|h| h.session == session && h.chip == Some(Chip::Autopilot))
            .fold((false, false), |(_, seen), h| {
                (true, seen || !covered(h.term_row))
            });
        if painted && !seen {
            return Drawn::NoRule;
        }
        if seen
            && let Some(block) = fitted.block.as_ref().filter(|_| fitted.drawn)
            && let Some((span, _)) = block.cols.iter().find(|(_, c)| *c == Chip::Autopilot)
            && let Some(cells) = block.lights.get(span.clone())
        {
            return Drawn::Cells(cells_text(cells));
        }
        // The frame noted the session's view before it fitted the block
        // (`App::note_claude_autopilot`): no chip, for want of room or of a
        // host.
        if self
            .sessions
            .get(&session)
            .is_some_and(SessionLights::harness_chips)
        {
            Drawn::NoRoom
        } else {
            Drawn::NoHost
        }
    }
}

impl App {
    /// `chrome`'s `autopilot` lines (§12.5.3): one per supervised pane — a
    /// session whose agent the host supervises or watches
    /// ([`App::session_agent`]) — in every window, its tabs in order and each
    /// tab's panes in their layout's order. Composed only for a `chrome`
    /// call: for each drawn chip what its accessibility node says
    /// ([`App::autopilot_spoken`]: a wall read only where a face's clock, a
    /// pause or a hold is told, for its day).
    pub(crate) fn drawn_chip_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (&wid, ws) in &self.windows {
            // Only the front tab is painted, and only the leaves of its plan:
            // a pane of a tab behind, or one a zoom hides, is in none of them.
            let plan = ws
                .tab_set
                .active_id()
                .and_then(|tab| self.visible_leaf_plan_for_tab(wid, tab));
            for tab in ws.tab_set.tabs() {
                for view in tab.root.leaves() {
                    let Some(session) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                    else {
                        continue;
                    };
                    let Some(program) = self.session_agent(session) else {
                        continue;
                    };
                    let Some(entry) = self.pool.get(session) else {
                        continue;
                    };
                    let sid = entry.ctx.self_id.as_str();
                    let published = entry
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .agent()
                        .program
                        .clone();
                    if !crate::claude_footer::drawn_for(published.as_deref()) {
                        lines.push(drawn_chip_line(
                            sid,
                            wid.0,
                            program,
                            None,
                            &Drawn::NoFooter,
                            None,
                        ));
                        continue;
                    }
                    let (_, view_now, _) = self.session_autopilot(session);
                    let glass = self.chip_glass(session);
                    let face = ChipFace::of(view_now, &glass);
                    let drawn = plan
                        .as_ref()
                        .and_then(|plan| plan.leaves.iter().position(|l| l.view == view))
                        .map_or(Drawn::NoRule, |pane| {
                            ws.claude_lights.drawn_chip(session, pane, |row| {
                                self.chrome_owns_terminal_row(wid, row)
                            })
                        });
                    // What the drawn chip's accessibility node says
                    // (`App::claude_chips_a11y`): the face of the view the
                    // frame noted, over the glass now. No chip, no node.
                    let spoken = matches!(drawn, Drawn::Cells(_)).then(|| {
                        let noted = ws
                            .claude_lights
                            .sessions
                            .get(&session)
                            .and_then(|s| s.autopilot)
                            .unwrap_or(AutopilotView::Unhosted);
                        self.autopilot_spoken(session, noted, &glass)
                    });
                    lines.push(drawn_chip_line(
                        sid,
                        wid.0,
                        program,
                        Some(face),
                        &drawn,
                        spoken
                            .as_ref()
                            .map(|(label, description)| (label.as_str(), description.as_deref())),
                    ));
                }
            }
        }
        lines
    }
}

#[cfg(all(test, unix))]
#[path = "claude_lights_drawn_tests.rs"]
mod tests;
