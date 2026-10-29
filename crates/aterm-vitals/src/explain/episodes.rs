// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EPISODES AND THE DECK'S CLAUSE (RD §5.5).

use std::collections::HashMap;

use super::detect::{ActivityId, Candidate, CauseId, Context};
use super::{CoalSplit, WhyFacts};
use crate::model::Snapshot;

/// An open episode closes once its cause has failed this long, ms.
pub const CLOSE_MS: u64 = 30_000;
/// Split ticks further apart than this are not consecutive, ms.
pub const GAP_MS: u64 = super::WINDOW_MAX_NS / 1_000_000;
/// The clause needs its activity to cover this much, millicores.
pub const CLAUSE_MC: i64 = 1000;
/// And this share of the busy CPU outside aterm, per mille.
pub const CLAUSE_PM: i64 = 400;
/// The clause shows once its candidate has held this long, ms.
pub const CLAUSE_IN_MS: u64 = 10_000;
/// It goes once its candidate has failed this long, ms.
pub const CLAUSE_OUT_MS: u64 = 30_000;
/// Or once the facts it rides are older than this, ms (three 5 s ticks).
pub const CLAUSE_STALE_MS: u64 = 15_000;

/// An episode that is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCause {
    /// Its cause.
    pub id: CauseId,
    /// When it opened, unix ms (the second split tick it held on).
    pub opened_unix_ms: i64,
    /// When its work began, by the evidence, unix ms: the earliest over the episode.
    pub since_ms: Option<i64>,
    /// Whether it holds on this tick (an open one that fails stays open for [`CLOSE_MS`]).
    pub holds: bool,
    /// What it covered when it last held, millicores.
    pub covers_mc: i64,
    /// The contexts that held for it when it last held.
    pub contexts: Vec<Context>,
}

#[derive(Clone, Debug)]
struct Episode {
    /// Consecutive ticks it held on, up to 2.
    held: u8,
    /// When it opened, unix ms.
    opened: Option<i64>,
    /// When it began to fail while open, mono ms.
    failing_since: Option<u64>,
    since_ms: Option<i64>,
    covers_mc: i64,
    contexts: Vec<Context>,
}

/// Every cause's episode.
#[derive(Clone, Debug, Default)]
pub struct Episodes {
    episodes: HashMap<CauseId, Episode>,
    last_ms: Option<u64>,
}

impl Episodes {
    /// One split tick at `mono_ms` (wall `unix_ms`) on which `held` hold.
    pub fn step(
        &mut self,
        mono_ms: u64,
        unix_ms: i64,
        held: &[Candidate],
        cut: bool,
    ) -> Vec<OpenCause> {
        if self
            .last_ms
            .is_some_and(|last| mono_ms.saturating_sub(last) > GAP_MS)
        {
            self.episodes.clear();
        }
        self.last_ms = Some(mono_ms);
        let frozen = |id: &CauseId| cut && matches!(id, CauseId::TabWork(_));
        for c in held.iter().filter(|c| c.id.covering() && !frozen(&c.id)) {
            let e = self.episodes.entry(c.id).or_insert(Episode {
                held: 0,
                opened: None,
                failing_since: None,
                since_ms: None,
                covers_mc: 0,
                contexts: Vec::new(),
            });
            e.held = (e.held + 1).min(2);
            e.failing_since = None;
            e.since_ms = match (e.since_ms, c.since_ms) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            e.covers_mc = c.covers_mc;
            e.contexts.clone_from(&c.contexts);
            if e.opened.is_none() && e.held >= 2 {
                e.opened = Some(unix_ms);
            }
        }
        self.episodes.retain(|id, e| {
            if frozen(id) || held.iter().any(|c| c.id == *id) {
                return true;
            }
            e.held = 0;
            e.opened.is_some()
                && mono_ms.saturating_sub(*e.failing_since.get_or_insert(mono_ms)) < CLOSE_MS
        });
        let mut open: Vec<OpenCause> = self
            .episodes
            .iter()
            .filter_map(|(id, e)| {
                e.opened.map(|unix| OpenCause {
                    id: *id,
                    opened_unix_ms: unix,
                    since_ms: e.since_ms,
                    holds: e.held > 0,
                    covers_mc: e.covers_mc,
                    contexts: e.contexts.clone(),
                })
            })
            .collect();
        open.sort_by(|a, b| b.covers_mc.cmp(&a.covers_mc).then(a.id.cmp(&b.id)));
        open
    }
}

/// The Deck's cause clause: `∵ Spotlight, after start-up`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeckCause {
    /// The activity.
    pub activity: ActivityId,
    /// Its people's word: `Spotlight`.
    pub word: &'static str,
    /// Its context, where it names a time: [`Context::Startup`] or [`Context::Wake`].
    pub context: Option<Context>,
}

/// The clause's hold (RD §5.5): in after [`CLAUSE_IN_MS`], out after [`CLAUSE_OUT_MS`].
#[derive(Clone, Debug, Default)]
pub struct ClauseHold {
    shown: Option<DeckCause>,
    /// The candidate, and since when it has held, mono ms.
    candidate: Option<(DeckCause, u64)>,
    /// Since when the shown clause's candidate has failed, mono ms.
    failing_since: Option<u64>,
    last_ms: Option<u64>,
    /// The negative control: no hold either way.
    #[cfg(any(test, debug_assertions))]
    pub(crate) no_hold: bool,
}

impl ClauseHold {
    /// One split tick at `mono_ms`, with the episodes `open` after it over the window `split`.
    pub fn step(
        &mut self,
        mono_ms: u64,
        open: &[OpenCause],
        split: &CoalSplit,
    ) -> Option<DeckCause> {
        if self
            .last_ms
            .is_some_and(|last| mono_ms.saturating_sub(last) > CLAUSE_STALE_MS)
        {
            *self = Self {
                #[cfg(any(test, debug_assertions))]
                no_hold: self.no_hold,
                ..Self::default()
            };
        }
        self.last_ms = Some(mono_ms);
        let now = candidate(open, split);
        self.candidate = match (now, self.candidate) {
            (Some(c), Some((was, since))) if c == was => Some((c, since)),
            (Some(c), _) => Some((c, mono_ms)),
            (None, _) => None,
        };
        #[cfg(any(test, debug_assertions))]
        if self.no_hold {
            self.shown = now;
            return now;
        }
        let ready = self
            .candidate
            .filter(|(_, since)| mono_ms.saturating_sub(*since) >= CLAUSE_IN_MS)
            .map(|(c, _)| c);
        match self.shown {
            Some(shown) if now == Some(shown) => self.failing_since = None,
            Some(_) if ready.is_some() => {
                self.shown = ready;
                self.failing_since = None;
            }
            Some(_) => {
                if mono_ms.saturating_sub(*self.failing_since.get_or_insert(mono_ms))
                    >= CLAUSE_OUT_MS
                {
                    self.shown = None;
                    self.failing_since = None;
                }
            }
            None => self.shown = ready,
        }
        self.shown
    }
}

fn candidate(open: &[OpenCause], split: &CoalSplit) -> Option<DeckCause> {
    let o = open.iter().filter(|o| o.holds && o.id.covering()).fold(
        None,
        |best: Option<&OpenCause>, o| match best {
            Some(b) if b.covers_mc >= o.covers_mc => best,
            _ => Some(o),
        },
    )?;
    let CauseId::Activity(activity) = o.id else {
        return None;
    };
    let outside = i128::from(split.others_mc)
        + i128::from(split.services_mc)
        + i128::from(split.unaccounted_mc.max(0));
    (o.covers_mc >= CLAUSE_MC && i128::from(o.covers_mc) * 1000 >= outside * i128::from(CLAUSE_PM))
        .then(|| DeckCause {
            activity,
            word: activity.activity().word,
            context: o
                .contexts
                .iter()
                .copied()
                .find(|c| matches!(c, Context::Startup | Context::Wake)),
        })
}

/// The clause snapshot `snap` shows: its facts', unless they are older than [`CLAUSE_STALE_MS`].
#[must_use]
pub fn deck_cause(snap: &Snapshot) -> Option<DeckCause> {
    let facts: &WhyFacts = snap.why.as_deref()?;
    let age_ms = snap.at_ns.saturating_sub(facts.at_ns) / 1_000_000;
    facts.clause.filter(|_| age_ms <= CLAUSE_STALE_MS)
}
