// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CODEX ROLLOUT TAP (session log 5G-3; plan amendment A1, step 6D-3b;
//! ruling T15): the reader that hands a codex-alab's own rollout to the
//! Codex step fold ([`super::codex_steps`]), so the session log takes its
//! turns, tools, tokens and usage wall as it takes a Claude Code
//! transcript's. The host keeps one per session on its own thread
//! (`aterm-gui`'s `codex_rollout_tap`); this is the part that touches files.
//!
//! THE GATE. A tap is made for one THREAD: the `sessionId` of the record a
//! codex-alab's proven self-claim names (the host's own copy of that record,
//! read while the claim's identity was live), under the `$CODEX_HOME` the
//! record was read under. It binds that thread's rollout
//! (`sessions/<y>/<m>/<d>/rollout-<stamp>-<thread>.jsonl`) only when the
//! rollout's own first row is the thread's `session_meta` and names a build
//! [`CODEX_MEASURED_BUILDS`] holds, the whole string matched ([`gate`]), and
//! the build the claimant's image runs NOW is one it holds too (the host
//! reads it at every arm, `super::codex_alab::alab_build`): a thread resumed
//! or re-exec'd onto another build appends to the same rollout and never
//! restates its `session_meta`. Anything else binds nothing and says why
//! ([`Refusal`]): no rollout yet, a first row not written whole yet, a first
//! row of another thread, a build not measured — a vendor Codex's release
//! among them, which no proven claim names in the first place. A refusal of
//! the thread's own first row, or of the running build, is final for the
//! tap: a thread's `session_meta` is written once, and a bound rollout whose
//! writer moved onto a build nobody measured says `off` once
//! ([`CodexStepFold::stop`]) and is read no more.
//!
//! THE READ ([`RolloutTap::read`]). Each read goes on from where the last
//! stopped, at most `cap` bytes (`footer::FOLD_BUDGET` on the host), each
//! complete line pushed with its end; a torn last line is read again next
//! time, and a line no read from its start could finish (longer than the
//! cap) is skipped from where the read stopped, counted lost once at its
//! newline. The fold is fed from the rollout's FIRST byte (a copy is known
//! only from the first row). Rows ending at or below the seed — the
//! watermark the run's join holds, on this very file — or older than the
//! floor (the latest of the run's floor, its open and the process's start,
//! [`RolloutTap::arm`]) are history: folded, never queued. A file replaced
//! at its path (another identity) or cut below where the read stood is bound
//! again, fed from its first byte, and is history up to its length then
//! (ruling S1), unless it is the very file the watermark names; so is a file
//! at the path the watermark names under another identity, whichever tap
//! binds it.
//!
//! Nothing is kept of a row but what the fold keeps (ids, a tool's bounded
//! name, class words, numbers: ruling S6); the gate reads the first row
//! through the same key-only projection, so the base instructions it holds
//! are validated, never materialized.

use std::path::{Path, PathBuf};

use aterm_json::Value;
use aterm_phase::anchors::anchor_text;

use super::codex_rollout::{
    RolloutRead, Streamed, first_line_of, is_rollout_of, is_thread_id, rollouts_by_write,
    stream_open_ends, thread_created_ms,
};
use super::codex_steps::{CODEX_MEASURED_BUILDS, CodexStepFold};
use super::steps::{StepBatch, StepMark};

const SESSION_META: &str = anchor_text("rollout.session_meta");
const KEY_ID: &str = anchor_text("rollout.id");
const KEY_CLI_VERSION: &str = anchor_text("rollout.cli_version");

/// Why a tap bound nothing (module docs), in one word ([`Self::word`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No rollout of the thread under the home (yet).
    NoRollout,
    /// The rollout's first row is not written whole yet, or runs past the
    /// read's bound.
    NoMeta,
    /// The rollout's first row is no `session_meta` of the thread (final).
    OtherThread,
    /// The build its `session_meta` names is not one
    /// [`CODEX_MEASURED_BUILDS`] holds (final).
    Unmeasured,
}

impl Refusal {
    /// The refusal's word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Refusal::NoRollout => "no-rollout",
            Refusal::NoMeta => "no-meta",
            Refusal::OtherThread => "other-thread",
            Refusal::Unmeasured => "unmeasured",
        }
    }

    /// Whether the refusal stands for the tap's life (module docs).
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Refusal::OtherThread | Refusal::Unmeasured)
    }
}

/// THE GATE (pure, module docs): the build the rollout's first row names,
/// when that row is `thread`'s own `session_meta` and the build is one of
/// `measured`, the whole string matched.
///
/// # Errors
/// [`Refusal::OtherThread`] for a first row that is no `session_meta` of
/// `thread`, [`Refusal::Unmeasured`] for a build `measured` does not hold
/// (or none named).
pub fn gate(first_row: &str, thread: &str, measured: &[&str]) -> Result<String, Refusal> {
    let v = super::transcript::codex_steps(first_row.trim()).map_err(|_| Refusal::OtherThread)?;
    let payload = v.get("payload");
    let word = |key: &str| payload.and_then(|p| p.get(key)).and_then(Value::as_str);
    if v.get("type").and_then(Value::as_str) != Some(SESSION_META) || word(KEY_ID) != Some(thread) {
        return Err(Refusal::OtherThread);
    }
    match word(KEY_CLI_VERSION) {
        Some(build) if measured.contains(&build) => Ok(build.to_string()),
        _ => Err(Refusal::Unmeasured),
    }
}

/// The rollout of `thread` under `home`: for a UUIDv7 thread, looked for in
/// the day folders around its creation (a rollout sits in the folder of the
/// LOCAL day its thread started, so the UTC day and each one beside it),
/// else among every rollout.
#[must_use]
pub fn rollout_path(home: &Path, thread: &str) -> Option<PathBuf> {
    let named = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| is_rollout_of(n, thread))
    };
    let Some(secs) = thread_created_ms(thread).map(|ms| ms / 1000) else {
        return rollouts_by_write(home)
            .into_iter()
            .map(|(p, _)| p)
            .find(|p| named(p));
    };
    [0_i64, -86_400, 86_400].into_iter().find_map(|d| {
        let day = aterm_types::rfc3339::format_rfc3339(secs.saturating_add_signed(d));
        let dir = home
            .join("sessions")
            .join(day.get(0..4)?)
            .join(day.get(5..7)?)
            .join(day.get(8..10)?);
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| named(p))
    })
}

/// The rollout a tap bound: its path, its identity, and where the next
/// read starts (the end of the last complete line read, or, inside a line
/// longer than a read's cap, where the last read stopped).
#[derive(Clone, Debug)]
struct Bound {
    path: PathBuf,
    id: Option<(u64, u64)>,
    pos: u64,
    /// Where the line being skipped began, while `pos` is inside one no
    /// read from its start could finish.
    skip: Option<u64>,
}

/// THE TAP of one codex-alab thread's rollout (module docs).
#[derive(Debug)]
pub struct RolloutTap {
    thread: String,
    home: PathBuf,
    floor_ms: u64,
    given: Option<StepMark>,
    /// The build the claimant's image runs, as the last arm read it.
    running: Option<String>,
    bound: Option<Bound>,
    /// It bound a rollout before: a file bound again (one replaced at its
    /// path, or cut) is history up to its length, unless it is the very
    /// file the run's watermark names.
    rebind: bool,
    fold: CodexStepFold,
    refused: Option<Refusal>,
}

impl RolloutTap {
    /// A tap of `thread`'s rollout under `home`, unarmed; `None` for a
    /// thread that is no thread id (Codex prints and takes back no other).
    #[must_use]
    pub fn new(thread: &str, home: &Path) -> Option<RolloutTap> {
        is_thread_id(thread).then(|| RolloutTap {
            thread: thread.to_string(),
            home: home.to_path_buf(),
            floor_ms: 0,
            given: None,
            running: None,
            bound: None,
            rebind: false,
            fold: CodexStepFold::new(0),
            refused: None,
        })
    }

    /// The thread it taps.
    #[must_use]
    pub fn thread(&self) -> &str {
        &self.thread
    }

    /// Arm it with the run's floor (wall ms; it only rises), the watermark
    /// the run's join holds (`given`: a bind of that very file seeds there)
    /// and the build the claimant's image runs now (`running`, its package's
    /// version whole; `None` when it is no ALab build's or cannot be read):
    /// the next read reads only while that build is measured (module docs).
    pub fn arm(&mut self, floor_ms: u64, given: Option<StepMark>, running: Option<&str>) {
        self.floor_ms = self.floor_ms.max(floor_ms);
        self.fold.raise_floor(self.floor_ms);
        self.given = given;
        self.running = running.map(str::to_string);
    }

    /// Whether it bound a rollout.
    #[must_use]
    pub fn is_bound(&self) -> bool {
        self.bound.is_some()
    }

    /// Why the last read bound nothing, while it binds nothing.
    #[must_use]
    pub fn refused(&self) -> Option<Refusal> {
        self.refused
    }

    /// Everything the fold queued since the last take.
    pub fn take(&mut self) -> StepBatch {
        self.fold.take()
    }

    /// READ ON (module docs): bind the thread's rollout when none is bound
    /// (or the bound one was replaced or cut), then fold at most `cap`
    /// bytes past where the last read stopped. `None` when nothing could be
    /// read: refused ([`Self::refused`]), or the file not opened.
    pub fn read(&mut self, cap: u64) -> Option<RolloutRead> {
        if self.refused.is_some_and(Refusal::is_final) {
            return None;
        }
        // The build writing the rollout now: one nobody measured binds
        // nothing, and a bound rollout says `off` once, read no more.
        let Some(running) = self
            .running
            .as_deref()
            .filter(|b| CODEX_MEASURED_BUILDS.contains(b))
        else {
            if let Some(b) = self.bound.take() {
                self.fold.stop(b.pos);
            }
            self.refused = Some(Refusal::Unmeasured);
            return None;
        };
        self.fold.set_writer(running);
        // The bound file, still the one bound and no shorter than the read;
        // else (none bound, or replaced at its path, or cut) bound again.
        let held = self.bound.as_ref().and_then(|b| {
            let file = super::footer::open_regular(&b.path)?;
            let meta = file.metadata().ok()?;
            (super::session_usage::open_identity(&file, &meta) == b.id && meta.len() >= b.pos)
                .then_some(file)
        });
        let file = match held {
            Some(file) => file,
            None => match self.bind() {
                Ok(file) => {
                    self.refused = None;
                    file
                }
                Err(why) => {
                    self.bound = None;
                    self.refused = Some(why);
                    return None;
                }
            },
        };
        let b = self.bound.as_mut()?;
        let fold = &mut self.fold;
        let from = b.pos;
        let mut last = None;
        let read = stream_open_ends(file, from, b.skip, cap, |_, end, line| {
            match line {
                Streamed::Line(Some(text)) => fold.push(text.as_bytes(), end),
                Streamed::Line(None) | Streamed::Long => fold.lost_line(end),
            }
            last = Some(end);
            std::ops::ControlFlow::Continue(())
        });
        let stopped = from.saturating_add(read.read);
        match (last, b.skip) {
            // A line ended: the next read starts past it.
            (Some(end), _) => {
                b.pos = end;
                b.skip = None;
            }
            // Still inside the line being skipped.
            (None, Some(_)) => b.pos = stopped,
            // The cap ran out inside a line that began where this read did:
            // no read from its start can finish it, so it is skipped from
            // here on, and counted lost at its newline.
            (None, None) if read.read >= cap && stopped > from => {
                b.skip = Some(from);
                b.pos = stopped;
            }
            (None, None) => {}
        }
        Some(read)
    }

    /// Find the thread's rollout, hold its first row to the [`gate`], and
    /// bind the fold to it at the build it names. The seed: the run's
    /// watermark on this very file; else the file's length when the file
    /// replaced one read before (ruling S1: history up to where it stands)
    /// — one this tap bound, or the one at the path the watermark names,
    /// whichever tap read it; else none (a file no watermark names: the
    /// floor makes what it held before the process history). The file
    /// opened, for the read.
    fn bind(&mut self) -> Result<std::fs::File, Refusal> {
        let path = rollout_path(&self.home, &self.thread).ok_or(Refusal::NoRollout)?;
        let file = super::footer::open_regular(&path).ok_or(Refusal::NoRollout)?;
        let meta = file.metadata().map_err(|_| Refusal::NoRollout)?;
        let id = super::session_usage::open_identity(&file, &meta);
        let first = first_line_of(&file).ok_or(Refusal::NoMeta)?;
        let build = gate(&first, &self.thread, CODEX_MEASURED_BUILDS)?;
        let fp = super::session_usage::path_fp(&path);
        let seed = match self.given.filter(|m| m.fp == fp) {
            Some(m) if m.id == id => m.off,
            Some(_) => meta.len(),
            None if self.rebind => meta.len(),
            None => 0,
        };
        self.fold.bind_build(fp, id, seed, Some(&build));
        self.bound = Some(Bound {
            path,
            id,
            pos: 0,
            skip: None,
        });
        self.rebind = true;
        Ok(file)
    }
}

#[cfg(test)]
#[path = "codex_tap_tests.rs"]
mod tests;
