// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dated token intervals from one provider conversation in one local terminal's agent run.

use std::sync::Arc;

use crate::explain::{IdMap, IdSet};
use crate::model::{Sample, TOKENS_CAP_PER_S, TOKENS_HOLD_MS};
use crate::pane_cpu::{BUCKET_NS, BUCKETS};
use crate::pane_history::Minute;

const FRESH_NS: u64 = TOKENS_HOLD_MS as u64 * 1_000_000;

/// Which run owns these counters. A retained terminal can change any of these between reads.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Owner {
    /// Stable launch nonce of the terminal.
    pub launch: String,
    /// Foreground custody generation, advanced even when the shell itself stays alive.
    pub generation: u64,
    /// Foreground process group.
    pub pgid: i32,
    /// Provider-qualified process and conversation identifier.
    pub conversation: String,
    /// Provider's process start or image token, when recorded.
    pub started: Option<u64>,
    /// Transcript floor under the provider's cumulative counts, when recorded.
    pub floor: Option<u64>,
}

/// Cumulative conversation counters from a complete source observation. Each direction is independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counters {
    /// End of the observation on the shared monotonic clock.
    pub at_ns: u64,
    /// When the provider tap collected the observation on the same clock.
    pub collected_ns: u64,
    /// Input tokens including cache reads when the provider counts them.
    pub input: Option<u64>,
    /// Output tokens when the provider counted them.
    pub output: Option<u64>,
}

/// A pane with an identified provider conversation. An unread or incomplete fold has no counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// Local terminal pane key.
    pub pane: u64,
    /// Identity of the producing run.
    pub owner: Owner,
    /// Dated whole counters, if measured.
    pub counters: Option<Counters>,
}

/// Average tokens per second between two comparable source observations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// End of the source interval on the monotonic clock.
    pub at_ns: u64,
    /// Monotonic time of collection.
    pub collected_ns: u64,
    /// Time covered by these two provider observations.
    pub window_ns: u64,
    /// Measured input rate.
    pub input_per_s: Option<f64>,
    /// Measured output rate.
    pub output_per_s: Option<f64>,
}

/// One current agent run's retained readings.
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    /// Identity to compare with the live terminal before showing these readings.
    pub owner: Owner,
    /// Last measured interval, including after its freshness expires.
    pub latest: Option<Reading>,
    last: Option<Counters>,
    high_water: Option<u64>,
    minute: Minute<Reading>,
}

impl History {
    /// Latest rate while its source still stands and its measurement is fresh.
    #[must_use]
    pub fn latest_at(&self, at_ns: u64) -> Option<Reading> {
        let end = self.last?.at_ns;
        self.latest.filter(|r| {
            r.collected_ns <= at_ns
                && r.at_ns == end
                && at_ns.checked_sub(end).is_some_and(|age| age <= FRESH_NS)
        })
    }

    /// Last twelve closed five-second intervals, oldest first. A broad fold has no location within its span.
    #[must_use]
    pub fn buckets(&self, at_ns: u64) -> [Option<Reading>; BUCKETS] {
        self.minute.buckets(at_ns)
    }

    fn observe(&mut self, sample: Option<Counters>, now: u64) {
        let newest = sample.filter(|r| {
            r.at_ns <= r.collected_ns && r.collected_ns <= now && now - r.at_ns <= FRESH_NS
        });
        if self.last == newest {
            return;
        }
        if let Some(new) = newest {
            self.minute.record(new.at_ns, None);
            if self.high_water.is_some_and(|at| at >= new.at_ns) {
                self.last = None;
                return;
            }
            self.high_water = Some(new.at_ns);
        } else {
            self.minute.record(now, None);
        }
        let old = std::mem::replace(&mut self.last, newest);
        let (Some(old), Some(new)) = (old, newest) else {
            return;
        };
        let Some(window_ns) = new
            .at_ns
            .checked_sub(old.at_ns)
            .filter(|n| *n > 0 && *n <= FRESH_NS)
        else {
            return;
        };
        if old.input.zip(new.input).is_some_and(|(a, b)| b < a)
            || old.output.zip(new.output).is_some_and(|(a, b)| b < a)
        {
            return;
        }
        let dt = window_ns as f64 / 1e9;
        let rate = |before: Option<u64>, after: Option<u64>| {
            Some(after?.checked_sub(before?)? as f64 / dt)
        };
        let input_per_s = rate(old.input, new.input);
        let output_per_s = rate(old.output, new.output).filter(|rate| *rate <= TOKENS_CAP_PER_S);
        if input_per_s.is_none() && output_per_s.is_none() {
            return;
        }
        let r = Reading {
            at_ns: new.at_ns,
            collected_ns: new.collected_ns,
            window_ns,
            input_per_s,
            output_per_s,
        };
        self.latest = Some(r);
        if old.at_ns / BUCKET_NS == (new.at_ns - 1) / BUCKET_NS {
            self.minute.record(new.at_ns, Some(r));
        }
    }
}

/// Local histories keyed by the producing pane.
pub type Panes = IdMap<u64, History>;

pub(crate) fn absorb(map: &mut Arc<Panes>, s: &Sample) {
    if s.machine_only {
        return;
    }
    let panes = Arc::make_mut(map);
    let mut present = IdSet::default();
    let mut conversations = std::collections::HashSet::new();
    let mut shared = std::collections::HashSet::new();
    for obs in &s.pane_token_observations {
        if !conversations.insert(&obs.owner.conversation) {
            shared.insert(&obs.owner.conversation);
        }
    }
    for obs in &s.pane_token_observations {
        present.insert(obs.pane);
        let new_history = || History {
            owner: obs.owner.clone(),
            latest: None,
            last: None,
            high_water: None,
            minute: Minute::default(),
        };
        let h = panes.entry(obs.pane).or_insert_with(new_history);
        if h.owner != obs.owner {
            *h = new_history();
        }
        h.observe(
            obs.counters
                .filter(|_| !shared.contains(&obs.owner.conversation)),
            s.at_ns,
        );
    }
    panes.retain(|key, _| present.contains(key));
}

#[cfg(test)]
#[path = "pane_tokens_tests.rs"]
mod tests;
