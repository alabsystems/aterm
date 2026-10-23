// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CRASH RESTORE'S SOAK.

use crate::recovery_ledger::{EndClass, KeeperAtLaunch, Restores, Row};

/// How much soak counts as enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    /// Days the keeper link has been on without a break, up to now.
    pub min_days: u64,
    /// Launches that recovered sessions after an unexpected end.
    pub min_kills: u32,
    /// Of the sessions those launches recovered, the share that came back exactly, in thousandths.
    pub min_exact_permille: u32,
}

/// The bar keeper P4 waits on.
pub const P4: Bar = Bar {
    min_days: 14,
    min_kills: 3,
    min_exact_permille: 900,
};

/// Seconds in a day.
const DAY: u64 = 86_400;

/// One judged criterion: its name, whether the evidence meets the bar, and what it read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Criterion {
    /// `keeper-on`, `real-kills`, `exact`, `none-blank` or `no-miss`.
    pub name: &'static str,
    /// Whether the evidence meets the bar.
    pub met: bool,
    /// What the evidence says, for a person.
    pub detail: String,
}

/// A kill the keeper should have brought back and did not ([`Soak::misses`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Miss {
    /// When the launch that recovered nothing wrote its row (Unix seconds).
    pub at: u64,
    /// The pid of the run that was killed.
    pub killed_pid: u32,
}

/// What the ledger says about the crash restore's soak.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Soak {
    /// Rows read.
    pub rows: usize,
    /// The oldest row of the current keeper-on streak, when the newest row's link is on.
    pub streak_since: Option<u64>,
    /// Launches in that streak whose keeper answered at boot (`keeper=on`).
    pub streak_on: u32,
    /// See [`Self::streak_on`].
    pub streak_armed: u32,
    /// See [`Self::streak_on`].
    pub streak_absent: u32,
    /// Launches that recovered sessions after an unexpected end.
    pub kills_recovered: u32,
    /// Those recoveries summed ([`Restores`]: rungs, losses, words).
    pub restores: Restores,
    /// Of [`Self::kills_recovered`], how many lost bytes in flight, and how many lost scrollback.
    pub kills_in_flight: u32,
    /// See [`Self::kills_in_flight`].
    pub kills_history: u32,
    /// Launches that recovered sessions after a run that ended on purpose.
    pub other_recoveries: u32,
    /// Launches that recovered sessions after a run whose end the ledger cannot say.
    pub unclassified_recoveries: u32,
    /// Kills the keeper should have brought back and did not.
    pub misses: Vec<Miss>,
    /// The judged criteria, in the order the module doc lists them.
    pub criteria: Vec<Criterion>,
}

impl Soak {
    /// Whether every criterion is met.
    #[must_use]
    pub fn met(&self) -> bool {
        self.criteria.iter().all(|c| c.met)
    }

    /// The report, one line each.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .criteria
            .iter()
            .map(|c| {
                format!(
                    "{:<11} {} — {}",
                    c.name,
                    if c.met { "MET    " } else { "NOT MET" },
                    c.detail
                )
            })
            .collect();
        let r = &self.restores;
        let whys = if r.whys.is_empty() {
            "none".to_string()
        } else {
            r.whys
                .iter()
                .map(|(word, n)| format!("{word} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(format!(
            "losses      reported — of {} recoveries after a kill: {} lost bytes in flight ({} \
             bytes), {} lost scrollback ({} lines); {} sessions overflowed a tail; why not \
             exact: {whys}",
            self.kills_recovered,
            self.kills_in_flight,
            r.in_flight_lost,
            self.kills_history,
            r.history_lost,
            r.tail_overflows,
        ));
        if self.other_recoveries > 0 || self.unclassified_recoveries > 0 {
            lines.push(format!(
                "other       reported — {} recoveries after a run that ended on purpose (a quit, \
                 a handoff), {} after a run whose end the ledger cannot say (no evidence left, \
                 still running, no earlier row); neither counted as kills",
                self.other_recoveries, self.unclassified_recoveries
            ));
        }
        lines.push(
            "not judged  the real-Claude lab's read of Claude Code after a restore that was not \
             exact (Q1): a lab run, not a ledger row (read on 2.1.304; read it again on the \
             Claude Code P4 ships against)"
                .to_string(),
        );
        let unmet = self.criteria.iter().filter(|c| !c.met).count();
        lines.push(if unmet == 0 {
            format!("verdict     MET ({} criteria)", self.criteria.len())
        } else {
            format!(
                "verdict     NOT MET ({unmet} of {} criteria short)",
                self.criteria.len()
            )
        });
        lines
    }
}

/// Whether a row's link counts toward the streak: on, armed or absent (the link was on,
/// whatever answered it).
fn streak_link(row: &Row) -> Option<KeeperAtLaunch> {
    row.keeper.filter(|k| k.link_on())
}

/// `n` of `d` in thousandths, rounded down; 0 of 0 is 0.
fn permille(n: u32, d: u32) -> u32 {
    if d == 0 {
        return 0;
    }
    let p = u64::from(n).saturating_mul(1000) / u64::from(d);
    u32::try_from(p).unwrap_or(u32::MAX)
}

/// `p` thousandths as a percentage with one decimal (`93.7%`).
fn percent(p: u32) -> String {
    format!("{}.{}%", p / 10, p % 10)
}

/// Fold `rows` into the soak, judged against `bar` at `now`.
#[must_use]
pub fn judge(rows: &[Row], now: u64, bar: &Bar) -> Soak {
    let mut streak_since = None;
    let (mut on, mut armed, mut absent) = (0u32, 0u32, 0u32);
    for row in rows.iter().rev() {
        let Some(link) = streak_link(row) else {
            break;
        };
        streak_since = Some(row.at);
        match link {
            KeeperAtLaunch::On => on = on.saturating_add(1),
            KeeperAtLaunch::Armed => armed = armed.saturating_add(1),
            KeeperAtLaunch::Absent => absent = absent.saturating_add(1),
            KeeperAtLaunch::Off => {}
        }
    }

    let mut restores = Restores::default();
    let mut kills_recovered = 0u32;
    let mut kills_in_flight = 0u32;
    let mut kills_history = 0u32;
    let mut other_recoveries = 0u32;
    let mut unclassified_recoveries = 0u32;
    let mut misses = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let killed = row.prev.class.is_unexpected();
        match (&row.restores, killed) {
            (Some(r), true) => {
                // A recovery that refused every offer brought nothing back: its refusals
                if r.recovered() > 0 {
                    kills_recovered = kills_recovered.saturating_add(1);
                    kills_in_flight =
                        kills_in_flight.saturating_add(u32::from(r.in_flight_lost > 0));
                    kills_history = kills_history.saturating_add(u32::from(r.history_lost > 0));
                }
                add(&mut restores, r);
            }
            (Some(r), false) if r.recovered() > 0 => {
                if matches!(row.prev.class, EndClass::Clean | EndClass::Handoff) {
                    other_recoveries = other_recoveries.saturating_add(1);
                } else {
                    unclassified_recoveries = unclassified_recoveries.saturating_add(1);
                }
            }
            (None, true) if row.keeper == Some(KeeperAtLaunch::On) => {
                // The killed run's own row, when the ledger still holds it: was its keeper
                let Some(killed_pid) = row.prev.pid else {
                    continue;
                };
                let held = rows[..i]
                    .iter()
                    .rev()
                    .find(|r| r.pid == killed_pid)
                    .and_then(|r| r.keeper)
                    .is_some_and(KeeperAtLaunch::link_on);
                if held {
                    misses.push(Miss {
                        at: row.at,
                        killed_pid,
                    });
                }
            }
            _ => {}
        }
    }

    let mut criteria = Vec::with_capacity(5);
    let days = streak_since.map_or(0, |since| now.saturating_sub(since) / DAY);
    criteria.push(Criterion {
        name: "keeper-on",
        met: streak_since.is_some() && days >= bar.min_days,
        detail: match streak_since {
            None => format!(
                "{} (need {} days on)",
                match rows.last().map(|r| r.keeper) {
                    None => "no launch recorded yet",
                    Some(None) =>
                        "the newest launch's row does not say its keeper link (a build \
                                   before `keeper=` wrote it)",
                    Some(Some(_)) => "the newest launch had the keeper link off",
                },
                bar.min_days
            ),
            Some(since) => format!(
                "{days} of {} days on without a break, since {} ({} launches: {on} with a keeper \
                 answering, {armed} update successors, {absent} with none answering)",
                bar.min_days,
                aterm_types::rfc3339::format_rfc3339(since),
                on.saturating_add(armed).saturating_add(absent),
            ),
        },
    });
    criteria.push(Criterion {
        name: "real-kills",
        met: kills_recovered >= bar.min_kills,
        detail: format!(
            "{kills_recovered} of {} launches recovered sessions after a run that ended \
             unexpectedly",
            bar.min_kills
        ),
    });
    let sessions = restores.recovered().saturating_add(restores.refused);
    let exact = permille(restores.exact, sessions);
    criteria.push(Criterion {
        name: "exact",
        met: sessions > 0 && exact >= bar.min_exact_permille,
        detail: if sessions == 0 {
            "no session recovered after a kill yet".to_string()
        } else {
            format!(
                "{} of {sessions} sessions came back exactly ({}, floor {}); {} with a loss \
                 counted, {} refused",
                restores.exact,
                percent(exact),
                percent(bar.min_exact_permille),
                restores.screen,
                restores.refused,
            )
        },
    });
    criteria.push(Criterion {
        name: "none-blank",
        met: restores.meta == 0,
        detail: format!("{} of {sessions} sessions came back blank", restores.meta),
    });
    criteria.push(Criterion {
        name: "no-miss",
        met: misses.is_empty() && restores.refused == 0,
        detail: if misses.is_empty() && restores.refused == 0 {
            "no kill the keeper held went unrecovered, and no session it offered after one was \
             refused"
                .to_string()
        } else {
            let first = misses.first().map_or_else(String::new, |first| {
                format!(
                    " (first: pid {} before the launch of {})",
                    first.killed_pid,
                    aterm_types::rfc3339::format_rfc3339(first.at),
                )
            });
            format!(
                "{} kills of a run whose keeper link was on recovered nothing, though a keeper \
                 answered the next launch{first}; {} sessions a keeper offered after a kill were \
                 refused",
                misses.len(),
                restores.refused,
            )
        },
    });

    Soak {
        rows: rows.len(),
        streak_since,
        streak_on: on,
        streak_armed: armed,
        streak_absent: absent,
        kills_recovered,
        restores,
        kills_in_flight,
        kills_history,
        other_recoveries,
        unclassified_recoveries,
        misses,
        criteria,
    }
}

/// Add one recovery's [`Restores`] to a running sum (the `restore_why` words by word, in
/// the order each was first seen).
fn add(sum: &mut Restores, r: &Restores) {
    sum.exact = sum.exact.saturating_add(r.exact);
    sum.screen = sum.screen.saturating_add(r.screen);
    sum.meta = sum.meta.saturating_add(r.meta);
    sum.in_flight_lost = sum.in_flight_lost.saturating_add(r.in_flight_lost);
    sum.tail_overflows = sum.tail_overflows.saturating_add(r.tail_overflows);
    sum.marks_lost = sum.marks_lost.saturating_add(r.marks_lost);
    sum.resizes_unjournaled = sum
        .resizes_unjournaled
        .saturating_add(r.resizes_unjournaled);
    sum.history_lost = sum.history_lost.saturating_add(r.history_lost);
    sum.history_short = sum.history_short.saturating_add(r.history_short);
    sum.refused = sum.refused.saturating_add(r.refused);
    for (word, n) in &r.whys {
        match sum.whys.iter_mut().find(|(w, _)| w == word) {
            Some((_, m)) => *m = m.saturating_add(*n),
            None => sum.whys.push((word.clone(), *n)),
        }
    }
}

#[cfg(test)]
#[path = "restore_soak_tests.rs"]
mod tests;
