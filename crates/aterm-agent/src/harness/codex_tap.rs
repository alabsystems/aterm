// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Codex rollout tap feeding [`super::codex_steps`] ([`RolloutTap::arm`], [`RolloutTap::read`],
//! [`RolloutTap::reads_at`], [`super::codex_steps::CODEX_MEASURED_BUILDS`], [`gate`], [`Refusal`]).

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::codex_rollout::{
    PAGINATED, RolloutRead, Streamed, first_line_of, is_rollout_of, is_thread_id,
    rollouts_by_write, stream_open_ends, thread_created_ms,
};
use super::codex_steps::{CodexStepFold, log_build};
use super::steps::{StepBatch, StepMark, StepState};

const SESSION_META: &str = aterm_phase::anchor!("rollout.session_meta");
const KEY_ID: &str = aterm_phase::anchor!("rollout.id");
const KEY_CLI_VERSION: &str = aterm_phase::anchor!("rollout.cli_version");
const KEY_HISTORY_MODE: &str = "history_mode";

/// The `history_mode` words of a `session_meta` the fold knows (codex's `ThreadHistoryMode`;
/// none written is `legacy`, its default): a mode it does not know may lay a fork's copy out in
/// rows the fold would read as the thread's own.
const HISTORY_MODES: [&str; 2] = ["legacy", PAGINATED];

/// Declares [`Refusal`], its word table and [`Refusal::ALL`] from one variant list, so a
/// refusal the tap can produce is always among those a reader of the words enumerates.
macro_rules! refusals {
    ($($(#[$doc:meta])* $variant:ident => $word:literal,)*) => {
        /// Why a tap bound nothing, in one word ([`Self::word`]).
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Refusal {
            $($(#[$doc])* $variant,)*
        }

        impl Refusal {
            /// Every refusal, so a reader of the words (`status rollout=`'s help) reads them
            /// from here; the same list as the enum's, so it cannot miss one.
            pub const ALL: &'static [Refusal] = &[$(Refusal::$variant,)*];

            /// The refusal's word.
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self {
                    $(Refusal::$variant => $word,)*
                }
            }
        }
    };
}

refusals! {
    /// No rollout of the thread under the home (yet).
    NoRollout => "no-rollout",
    /// The rollout's first row is not written whole yet, or runs past the read's bound.
    NoMeta => "no-meta",
    /// The rollout's first row is no `session_meta` of the thread (final).
    OtherThread => "other-thread",
    /// Its `session_meta` is of a shape the fold does not know: it names no build the log can
    /// say, or a `history_mode` outside [`HISTORY_MODES`] (final).
    UnknownMeta => "unknown-meta",
}

impl Refusal {
    /// Whether the refusal stands for the tap's life.
    #[must_use]
    pub const fn is_final(self) -> bool {
        match self {
            Refusal::OtherThread | Refusal::UnknownMeta => true,
            Refusal::NoRollout | Refusal::NoMeta => false,
        }
    }
}

/// THE GATE: the first row is `thread`'s own `session_meta`, of a shape the fold knows; the
/// build it names, whatever build that is: one [`super::codex_steps::CODEX_MEASURED_BUILDS`]
/// does not name is read [`StepState::Partial`], as Claude Code's unmeasured builds are.
///
/// # Errors
/// [`Refusal::OtherThread`] for a first row that is no `session_meta` of `thread`,
/// [`Refusal::UnknownMeta`] for one that names no build [`log_build`] can say, or a
/// `history_mode` outside [`HISTORY_MODES`].
pub fn gate(first_row: &str, thread: &str) -> Result<String, Refusal> {
    let v = super::transcript::codex_steps(first_row.trim()).map_err(|_| Refusal::OtherThread)?;
    let payload = v.get("payload");
    let word = |key: &str| payload.and_then(|p| p.get(key)).and_then(Value::as_str);
    if v.get("type").and_then(Value::as_str) != Some(SESSION_META) || word(KEY_ID) != Some(thread) {
        return Err(Refusal::OtherThread);
    }
    let mode_known = payload
        .and_then(|p| p.get(KEY_HISTORY_MODE))
        .is_none_or(|m| m.as_str().is_some_and(|m| HISTORY_MODES.contains(&m)));
    match word(KEY_CLI_VERSION) {
        Some(build) if mode_known && log_build(build).is_some() => Ok(build.to_string()),
        _ => Err(Refusal::UnknownMeta),
    }
}

/// The rollout of `thread` under `home`.
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

/// The rollout a tap bound: its path, its identity, and where the next read starts.
#[derive(Clone, Debug)]
struct Bound {
    path: PathBuf,
    id: Option<(u64, u64)>,
    pos: u64,
    /// Where the line being skipped began.
    skip: Option<u64>,
}

/// Tap over one `codex-alab` thread's rollout.
#[derive(Debug)]
pub struct RolloutTap {
    thread: String,
    home: PathBuf,
    floor_ms: u64,
    given: Option<StepMark>,
    /// The build the claimant's image runs, as the last arm read it.
    running: Option<String>,
    bound: Option<Bound>,
    /// It bound a rollout before.
    rebind: bool,
    fold: CodexStepFold,
    refused: Option<Refusal>,
}

impl RolloutTap {
    /// A tap of `thread`'s rollout under `home`, unarmed; `None` for a thread that is no thread id.
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

    /// Arm the tap with the run's floor, watermark, and running build.
    pub fn arm(&mut self, floor_ms: u64, given: Option<StepMark>, running: Option<&str>) {
        self.floor_ms = self.floor_ms.max(floor_ms);
        self.fold.raise_floor(self.floor_ms);
        self.given = given;
        self.running = running.map(str::to_string);
    }

    /// Whether it bound a rollout.
    #[must_use]
    pub const fn is_bound(&self) -> bool {
        self.bound.is_some()
    }

    /// Why the last read bound nothing, while it binds nothing.
    #[must_use]
    pub const fn refused(&self) -> Option<Refusal> {
        self.refused
    }

    /// The state the bound rollout's rows are read at ([`CodexStepFold::reads_at`]: `Partial`
    /// unless its build and its writer's are both measured), while it binds one.
    #[must_use]
    pub const fn reads_at(&self) -> Option<StepState> {
        if self.bound.is_some() {
            Some(self.fold.reads_at())
        } else {
            None
        }
    }

    /// Everything the fold queued since the last take.
    pub fn take(&mut self) -> StepBatch {
        self.fold.take()
    }

    /// Bind the rollout if needed and fold up to `cap` new bytes ([`Self::refused`]): read on
    /// whatever build its writer runs, at the state that build and the rollout's own hold the
    /// rows to ([`Self::reads_at`]).
    pub fn read(&mut self, cap: u64) -> Option<RolloutRead> {
        if self.refused.is_some_and(Refusal::is_final) {
            return None;
        }
        self.fold.set_writer(self.running.as_deref());
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
            (Some(end), _) => {
                b.pos = end;
                b.skip = None;
            }
            (None, Some(_)) => b.pos = stopped,
            (None, None) if read.read >= cap && stopped > from => {
                b.skip = Some(from);
                b.pos = stopped;
            }
            (None, None) => {}
        }
        Some(read)
    }

    /// Find the thread's rollout, validate its first row via [`gate`], and bind the fold.
    fn bind(&mut self) -> Result<std::fs::File, Refusal> {
        let path = rollout_path(&self.home, &self.thread).ok_or(Refusal::NoRollout)?;
        let file = super::footer::open_regular(&path).ok_or(Refusal::NoRollout)?;
        let meta = file.metadata().map_err(|_| Refusal::NoRollout)?;
        let id = super::session_usage::open_identity(&file, &meta);
        let first = first_line_of(&file).ok_or(Refusal::NoMeta)?;
        let build = gate(&first, &self.thread)?;
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
