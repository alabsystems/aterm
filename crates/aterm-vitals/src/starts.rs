// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process starts and exits between two readings (`docs/DESIGN-stats-deck-flow-2026-11-02.md`
//! §8.2): from the kernel's resource coalitions on macOS, which cover every process of every
//! user and read unprivileged, and from `/proc/stat`'s `processes` on Linux. Windows reads no
//! start counter without ETW, so it gives none.
//!
//! With `C_t` the coalitions answering at read `t` and `N_{t−1}` the newest id listed at the read
//! before (the kernel numbers coalitions in order and never reuses an id):
//!
//! ```text
//! starts_t = Σ_{c ∈ C_t ∩ C_{t−1}} Δs_c + Σ_{c ∈ C_t \ C_{t−1}, c > N_{t−1}} s_c
//! exits_t  = Σ_{c ∈ C_t ∩ C_{t−1}} Δe_c + Σ_{new} e_c + Σ_{c ∈ C_{t−1} \ C_t} (s_c − e_c)
//! ```
//!
//! A coalition gone from the answers took its live members with it. No count spans a truncated
//! list (either read's), a read in which no coalition answered beside one in which some did (it
//! would close every coalition at once, or open every one whole), a coalition answering now at or
//! below `N_{t−1}` that did not answer then (it has no base), or a counter that ran backwards; and
//! no rate spans a sleep or a gap (the model's `rate_dt`).

/// One resource coalition's task counters, as one read gave them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoalTasks {
    /// Its id.
    pub id: u64,
    /// Tasks that joined it since it was made: a spawn's, a fork's, and an exec's new task each.
    pub started: u64,
    /// Tasks of it that have exited.
    pub exited: u64,
}

/// One read of every resource coalition's task counters, its rows ascending by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalTaskRead {
    newest: Option<u64>,
    truncated: bool,
    rows: Vec<CoalTasks>,
}

impl CoalTaskRead {
    /// The read whose list named `newest` as its highest id (answered or not), was cut at its
    /// buffer where `truncated`, and whose answering coalitions gave `rows`: ordered by id, a
    /// second row for one id dropped.
    #[must_use]
    pub fn of(
        newest: Option<u64>,
        truncated: bool,
        rows: impl IntoIterator<Item = CoalTasks>,
    ) -> Self {
        let mut rows: Vec<CoalTasks> = rows.into_iter().collect();
        rows.sort_by_key(|r| r.id);
        rows.dedup_by_key(|r| r.id);
        Self {
            newest,
            truncated,
            rows,
        }
    }

    /// The answering coalitions' counters, ascending by id.
    #[must_use]
    pub fn rows(&self) -> &[CoalTasks] {
        &self.rows
    }

    /// Coalition `id`'s counters, where it answered.
    #[must_use]
    pub fn get(&self, id: u64) -> Option<CoalTasks> {
        self.rows
            .binary_search_by_key(&id, |r| r.id)
            .ok()
            .map(|i| self.rows[i])
    }
}

/// What one reading read of the machine's process starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartCount {
    /// macOS: every answering resource coalition's task counters, and the id of aterm's own
    /// (where its self-check read it).
    Coalitions {
        /// The read.
        read: CoalTaskRead,
        /// aterm's own resource coalition.
        own: Option<u64>,
    },
    /// Linux: every `fork` and `clone` since boot, a thread's included (`/proc/stat`'s
    /// `processes`).
    Forks(u64),
}

/// Starts and exits counted between two reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// Processes started (Linux: forks and clones, threads included).
    pub starts: u64,
    /// Processes ended; `None` where the counter says no exits (Linux).
    pub exits: Option<u64>,
    /// Starts in aterm's own coalition; `None` off macOS, or where it answered at one read only.
    pub own_starts: Option<u64>,
}

/// The starts and exits from read `then` to read `now` (the module's formula); `None` across a
/// truncated list, an empty coalition read beside an answered one, a coalition without a base, a
/// counter that ran backwards, or two reads of different kinds.
#[must_use]
pub fn moved(then: &StartCount, now: &StartCount) -> Option<Moved> {
    match (then, now) {
        (StartCount::Forks(a), StartCount::Forks(b)) => Some(Moved {
            starts: b.checked_sub(*a)?,
            exits: None,
            own_starts: None,
        }),
        (StartCount::Coalitions { read: a, own: was }, StartCount::Coalitions { read: b, own }) => {
            let (starts, exits) = coalition_moves(a, b)?;
            let own_starts = own
                .filter(|id| was == &Some(*id))
                .and_then(|id| b.get(id)?.started.checked_sub(a.get(id)?.started));
            Some(Moved {
                starts,
                exits: Some(exits),
                own_starts,
            })
        }
        _ => None,
    }
}

/// `(starts, exits)` from coalition read `then` to `now` (the module's formula).
#[must_use]
pub fn coalition_moves(then: &CoalTaskRead, now: &CoalTaskRead) -> Option<(u64, u64)> {
    if then.truncated || now.truncated || then.rows.is_empty() != now.rows.is_empty() {
        return None;
    }
    let (mut starts, mut exits) = (0u64, 0u64);
    let (mut was, mut is) = (then.rows.iter().peekable(), now.rows.iter().peekable());
    loop {
        let (a, b) = (was.peek().copied(), is.peek().copied());
        let gone = match (a, b) {
            (None, None) => return Some((starts, exits)),
            (Some(a), Some(b)) if a.id == b.id => {
                starts = starts.checked_add(b.started.checked_sub(a.started)?)?;
                exits = exits.checked_add(b.exited.checked_sub(a.exited)?)?;
                was.next();
                is.next();
                continue;
            }
            (Some(a), Some(b)) => (a.id < b.id).then_some(a),
            (Some(a), None) => Some(a),
            (None, Some(_)) => None,
        };
        if let Some(a) = gone {
            exits = exits.checked_add(a.started.checked_sub(a.exited)?)?;
            was.next();
        } else if let Some(b) = b {
            if then.newest.is_some_and(|n| b.id <= n) {
                return None;
            }
            starts = starts.checked_add(b.started)?;
            exits = exits.checked_add(b.exited)?;
            is.next();
        }
    }
}

/// Starts and exits a second over one reading's interval.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TaskFlow {
    /// Processes started a second (Linux: forks and clones, threads included).
    pub starts_ps: f64,
    /// Processes ended a second; `None` on Linux.
    pub exits_ps: Option<f64>,
    /// Starts a second in aterm's own resource coalition (its tabs' processes and their
    /// children, unless launchd placed one elsewhere); `None` off macOS, or without a base.
    pub aterm_starts_ps: Option<f64>,
}

impl TaskFlow {
    /// [`Moved`] over `dt_s` seconds; `None` for an interval that is not positive.
    #[must_use]
    pub fn of(moved: Moved, dt_s: f64) -> Option<Self> {
        (dt_s.is_finite() && dt_s > 0.0).then(|| Self {
            starts_ps: moved.starts as f64 / dt_s,
            exits_ps: moved.exits.map(|n| n as f64 / dt_s),
            aterm_starts_ps: moved.own_starts.map(|n| n as f64 / dt_s),
        })
    }
}

#[cfg(test)]
#[path = "starts_tests.rs"]
mod tests;
