// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Renders `aterm ctl chrome` autopilot lines from the last frame's drawn chips.

use aterm_core::terminal::RenderCell;

use super::{Chip, ChipFace, ChipGlass, SessionLights, WindowLights};
use crate::App;
use crate::autopilot::AutopilotView;

/// What the last frame drew of one pane's Autopilot chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Drawn {
    /// The chip's cells as the frame wrote them, excluding wide continuation cells.
    Cells(String),
    /// The last frame did not draw the pane's composer rule, or a later band covered it.
    NoRule,
    /// The last frame drew the pane's rule, but the chip did not fit in it.
    NoRoom,
    /// The last frame fitted the pane's rule, and no harness host drew a chip there.
    NoHost,
    /// The program has no composer footer to draw a chip in.
    NoFooter,
    /// The last frame drew only the pane's harness switch over a footer [`crate::claude_footer::bare_while_off`] leaves bare.
    SwitchOnly,
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
            Drawn::SwitchOnly => Some("switch-only"),
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

fn cells_text(cells: &[RenderCell]) -> String {
    cells.iter().filter(|c| !c.wide).map(|c| c.ch).collect()
}

/// Whether `chrome`'s `autopilot` lines end with the chip's ` title=`: a development build's (a
/// test build too) alone, so a release's lines are main's, byte for byte.
const CHROME_TITLE: bool = cfg!(any(test, debug_assertions));

/// What the chip's node says on one `autopilot` line: its label (`spoken=`), its description and,
/// in a development build, the title its hover reads ([`super::title_parts`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Said<'a> {
    pub(crate) spoken: &'a str,
    pub(crate) description: Option<&'a str>,
    pub(crate) title: Option<&'a str>,
}

/// Formats one `autopilot` line for `aterm ctl chrome`.
pub(crate) fn drawn_chip_line(
    sid: &str,
    window: u64,
    program: aterm_phase::Program,
    face: Option<ChipFace>,
    drawn: &Drawn,
    said: Option<Said<'_>>,
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
    match said {
        Some(said) => {
            line.push_str(&format!(" spoken={:?}", said.spoken));
            match said.description {
                Some(description) => line.push_str(&format!(" description={description:?}")),
                None => line.push_str(" description=-"),
            }
        }
        None => line.push_str(" spoken=- description=-"),
    }
    if CHROME_TITLE {
        match said.and_then(|said| said.title) {
            Some(title) => line.push_str(&format!(" title={title:?}")),
            None => line.push_str(" title=-"),
        }
    }
    line
}

impl WindowLights {
    /// Returns what the last frame of this window drew for `session`'s Autopilot chip in `pane`.
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
            return if self.footers.iter().any(|(s, _)| *s == session) {
                Drawn::NoRoom
            } else if self.border_toggles.iter().any(|b| {
                b.session == session
                    && !covered(b.term_row)
                    && crate::claude_footer::bare_while_off(Some(b.program), Some(b.face))
            }) {
                Drawn::SwitchOnly
            } else {
                Drawn::NoRule
            };
        };
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
    /// Returns the `autopilot` lines for `aterm ctl chrome`, one per supervised pane.
    pub(crate) fn drawn_chip_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (&wid, ws) in &self.windows {
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
                    let spoken = matches!(drawn, Drawn::Cells(_)).then(|| {
                        let built = ws
                            .claude_lights
                            .sessions
                            .get(&session)
                            .map_or((AutopilotView::Unhosted, ChipGlass::default()), |s| {
                                (s.autopilot.unwrap_or(AutopilotView::Unhosted), s.glass)
                            });
                        let (label, description) = self.autopilot_spoken(session, built.0, &glass);
                        let title = CHROME_TITLE.then(|| {
                            super::title_parts(&self.harness_glass, &self.pool, session, &built)
                                .whole
                        });
                        (label, description, title)
                    });
                    lines.push(drawn_chip_line(
                        sid,
                        wid.0,
                        program,
                        Some(face),
                        &drawn,
                        spoken.as_ref().map(|(label, description, title)| Said {
                            spoken: label,
                            description: description.as_deref(),
                            title: title.as_deref(),
                        }),
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
