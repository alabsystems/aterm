// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loop keeps the screens a fixture is to be made from ([`Session::screen_capture`]): each
//! screen of Claude Code's reset program, and each form of the account's usage nothing here
//! records, as a `CAPTURED` journal record ([`crate::supervise::journal::Capture`]). No lab can
//! draw these (Claude Code reads the account from claude.ai), so a supervised session that meets
//! one is how it is ever measured (docs/SPEC-claude-accounts-resets-2026-10-02.md §6, "Collected
//! by itself").

use super::*;

/// How many captures of each `what` one loop keeps ([`Session::screen_capture`]): a wall's
/// dialog, its answer and a step's pending lines and confirmation are one or two each, a person
/// moving the dialog's cursor row by row and a warning's figure moving a point at a time are
/// bounded, and the walls with no reset on offer, which come most often, never use up an offer's
/// or an answer's room.
pub(super) const CAPTURES_EACH: usize = 8;

/// The Claude Code version the newest transcript row of `tail` names (each row's `version`).
fn transcript_version(tail: &str) -> Option<String> {
    tail.lines().rev().find_map(|row| {
        let v = aterm_json::from_str::<aterm_json::Value>(row).ok()?;
        let version = v.get("version")?.as_str()?;
        (!version.is_empty()
            && version.len() <= 32
            && version
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
        .then(|| version.to_string())
    })
}

/// A sighting's `what` in a capture of the reset program (`reset-offer`, `reset-answer-spent`).
fn reset_what(sight: aterm_phase::ResetSight) -> String {
    let read = match sight {
        aterm_phase::ResetSight::Answer(line) => match line {
            aterm_phase::ResetLine::Spent { .. } => "-spent",
            aterm_phase::ResetLine::Walled { .. } => "-walled",
            aterm_phase::ResetLine::Lifted { .. } => "-lifted",
            aterm_phase::ResetLine::Kept => "-kept",
            aterm_phase::ResetLine::Login => "-login",
            aterm_phase::ResetLine::Other => "-other",
        },
        _ => "",
    };
    format!("reset-{}{read}", sight.word())
}

impl<C: Ctl> Session<'_, C> {
    /// Keep a capture of a Claude Code screen that shows the reset program
    /// ([`aterm_phase::reset_sighting`]: the usage-limit dialog, with a reset on offer or none, a
    /// confirmation, the chooser, a pending line, an answer) or, where it shows none of it, the
    /// account's usage in a form nothing records ([`aterm_phase::usage_sighting`]: a box of Claude
    /// Code's own naming usage credits, or a model's or money's wall or a `You've used …` warning
    /// no reader puts on record, a reset the host's clock does not place
    /// ([`crate::supervise::limit::parse_reset`]) among them): once per sighting and at most
    /// [`CAPTURES_EACH`] of each `what` a loop, as a `CAPTURED` journal record
    /// ([`crate::supervise::journal::Capture`]) a fixture is made from: the rows from the
    /// sighting's `top` (the box's first row or the rule over it, the command row, the notice's
    /// row) to the screen's foot, the transcript above cut, their e-mail addresses and the
    /// composer's draft masked, the rows under the composer kept as drawn. It reads and records;
    /// it presses nothing, whatever `[harness] limit_reset` says. A look runs it before its box is
    /// decided, so a box the policy answers (`credits-decline@v1`'s Esc) is kept first.
    pub(super) fn screen_capture(
        &mut self,
        screen: &Screen,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) {
        // Only Claude Code draws these screens; a new program names itself here.
        match aterm_phase::identify(self.program.as_deref(), &screen.rows).program() {
            aterm_phase::Program::Claude => {}
            aterm_phase::Program::Codex
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Generic => return,
        }
        let Some((what, key, top)) = aterm_phase::reset_sighting(&screen.rows)
            .map(|seen| (reset_what(seen.sight), seen.key, seen.top))
            .or_else(|| {
                aterm_phase::usage_sighting(&screen.rows, |reset| {
                    crate::supervise::limit::parse_reset(reset).is_some()
                })
                .map(|seen| (seen.what.to_string(), seen.key, seen.top))
            })
        else {
            return;
        };
        let kept = self.captured.iter().filter(|(w, _)| *w == what);
        if kept.clone().any(|(_, held)| *held == key) || kept.count() >= CAPTURES_EACH {
            return;
        }
        let version = opts
            .idle_host
            .as_deref()
            .and_then(|host| host.transcript_tail())
            .as_deref()
            .and_then(transcript_version);
        let capture = crate::supervise::journal::Capture::of_screen(
            screen.seq,
            what.clone(),
            "claude".to_string(),
            version,
            screen.cols.zip(screen.pane_rows),
            screen.first + top,
            screen.rows.get(top..).unwrap_or_default(),
        );
        review.note(&capture.line());
        self.captured.push((what, key));
    }
}
