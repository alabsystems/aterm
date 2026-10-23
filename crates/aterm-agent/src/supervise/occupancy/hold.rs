// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLD'S CLOCK (`docs/DESIGN-harness-occupancy-2026-10-20.md` §3.1):
//! how long a session has waited on its own background work, and whether the agent said so.

/// What the loop saw, in the order it saw it ([`HoldClock::step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldEvent {
    /// A read of the agent's own background work with its turn over: the break began at `since_ms`
    /// (the loop's first read of it since the last read that was no break), this read is at
    /// `now_ms`, and `settled` once the break has stood the loop's settle.
    Background {
        /// When the break began.
        since_ms: i64,
        /// When this read was made.
        now_ms: i64,
        /// Whether the break has stood the settle.
        settled: bool,
    },
    /// The harness typed a line into the session at a break (the upgrade's notice, its release):
    /// the turn that answers it is the harness's own, and so is the point that ends it.
    HarnessLine,
    /// The agent's last words at a settled background read after a harness line: whether a line of
    /// them starts `WAITING:` ([`waiting_declared`]).
    AnswerTail {
        /// A line starts `WAITING:`.
        waiting_line: bool,
    },
    /// A read of a live turn (its spinner or busy footer, no break).
    LiveTurn {
        /// The harness typed what this turn answers.
        ours: bool,
    },
    /// A point the loop decided: `acts` when its action types, escalates or restarts (not nothing,
    /// not a wait), `background` when the screen still shows the agent's own background work.
    DecidedPoint {
        /// The point's action types, escalates or restarts.
        acts: bool,
        /// The screen shows background work.
        background: bool,
    },
}

/// Why a hold closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Close {
    /// A decided point that acted, or that found no background work.
    Point,
    /// A live turn the harness did not type.
    Foreign,
    /// The ledger's open hold was not this loop's to carry: nothing shows a loop watched it until
    /// just before this one began — its row is older than the carry's gap, a row of another rule
    /// followed it, Off was lifted since, or the agent's process began after it (P3) — so it may
    /// have ended unseen.
    Stale,
}

impl Close {
    /// The word the journal and the ledger spell it with.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Close::Point => "point",
            Close::Foreign => "foreign",
            Close::Stale => "stale",
        }
    }
}

/// What a step changed: each said once ([`HoldClock::step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldChange {
    /// A hold opened at `since_ms`, or was carried from the ledger (`seeded`), declared already
    /// where its row said so.
    Opened {
        /// When the hold began.
        since_ms: i64,
        /// Carried from a loop before this one ([`HoldClock::seed`]).
        seeded: bool,
        /// Declared already.
        declared: bool,
    },
    /// The open hold was declared, having held `held_ms`.
    Declared {
        /// How long it had held when declared.
        held_ms: i64,
    },
    /// The hold closed, having held `held_ms` at the last read that saw it.
    Closed {
        /// Why it closed.
        why: Close,
        /// How long it held, to the last background read.
        held_ms: i64,
    },
}

/// A line the harness typed and its answer, as far as the clock has seen it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Answer {
    /// No line of the harness's is being answered.
    #[default]
    None,
    /// Typed; its live turn not read yet.
    Owed,
    /// Its live turn read.
    Running,
}

/// THE HOLD'S CLOCK ([module docs](self)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoldClock {
    /// When the open hold began; `None` with none open.
    since_ms: Option<i64>,
    /// The open hold was declared.
    declared: bool,
    /// The last read that saw the open hold.
    seen_ms: i64,
    /// A live turn has been read since the last break or point: the next live read is no edge.
    live: bool,
    /// The harness's own line and its answer.
    answer: Answer,
}

impl HoldClock {
    /// One event.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OccupancyDoor",
            action = "IdleRead",
            project = "conformance_occupancy::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OccupancyDoor",
            action = "HoldCloses",
            project = "conformance_occupancy::project"
        )
    )]
    pub fn step(&mut self, event: HoldEvent) -> Option<HoldChange> {
        match event {
            HoldEvent::Background {
                since_ms,
                now_ms,
                settled,
            } => {
                self.live = false;
                if self.since_ms.is_some() {
                    if super::fault::armed(super::fault::Fault::RestartOnIdle) {
                        self.since_ms = Some(since_ms);
                    }
                    self.seen_ms = self.seen_ms.max(now_ms);
                    return None;
                }
                if !settled {
                    return None;
                }
                self.since_ms = Some(since_ms);
                self.declared = false;
                self.seen_ms = now_ms;
                Some(HoldChange::Opened {
                    since_ms,
                    seeded: false,
                    declared: false,
                })
            }
            HoldEvent::HarnessLine => {
                self.answer = Answer::Owed;
                None
            }
            HoldEvent::AnswerTail { waiting_line } => {
                if self.answer == Answer::None {
                    return None;
                }
                if !waiting_line {
                    // The answer's first break read nothing declared; a line its turn has not been
                    // read answering may still be answered.
                    if self.answer == Answer::Running {
                        self.answer = Answer::None;
                    }
                    return None;
                }
                self.answer = Answer::None;
                let since = self.since_ms?;
                if std::mem::replace(&mut self.declared, true) {
                    return None;
                }
                Some(HoldChange::Declared {
                    held_ms: self.seen_ms - since,
                })
            }
            HoldEvent::LiveTurn { ours } => {
                if std::mem::replace(&mut self.live, true) {
                    return None;
                }
                if ours && !super::fault::armed(super::fault::Fault::CloseOnOurTurn) {
                    if self.answer == Answer::Owed {
                        self.answer = Answer::Running;
                    }
                    return None;
                }
                self.answer = Answer::None;
                self.close(Close::Foreign)
            }
            HoldEvent::DecidedPoint { acts, background } => {
                self.live = false;
                if std::mem::take(&mut self.answer) != Answer::None
                    && !super::fault::armed(super::fault::Fault::CloseAtTheAnswer)
                {
                    return None;
                }
                if acts || !background || super::fault::armed(super::fault::Fault::CloseAtAnyPoint)
                {
                    self.close(Close::Point)
                } else {
                    None
                }
            }
        }
    }

    /// The hold a loop before this one left open, carried: open since `since_ms` (declared where
    /// its row said so), last seen at `now_ms`.
    pub const fn seed(&mut self, since_ms: i64, declared: bool, now_ms: i64) -> Option<HoldChange> {
        if self.since_ms.is_some() {
            return None;
        }
        self.since_ms = Some(since_ms);
        self.declared = declared;
        self.seen_ms = now_ms;
        Some(HoldChange::Opened {
            since_ms,
            seeded: true,
            declared,
        })
    }

    /// How long the open hold has held at `now_ms`; `None` with none open.
    #[must_use]
    pub fn held_ms(&self, now_ms: i64) -> Option<i64> {
        self.since_ms.map(|since| now_ms - since)
    }

    /// How long the open hold had held at the last read that saw it.
    #[must_use]
    pub fn seen_held_ms(&self) -> Option<i64> {
        self.held_ms(self.seen_ms)
    }

    /// When the open hold began.
    #[must_use]
    pub const fn since_ms(&self) -> Option<i64> {
        self.since_ms
    }

    /// The open hold was declared.
    #[must_use]
    pub const fn declared(&self) -> bool {
        self.declared
    }

    /// Whether the loop should read the agent's last words at this settled break and say what they
    /// declare ([`HoldEvent::AnswerTail`]): a hold is open and a line the harness typed is being
    /// answered.
    #[must_use]
    pub fn wants_tail(&self) -> bool {
        self.since_ms.is_some() && self.answer != Answer::None
    }

    fn close(&mut self, why: Close) -> Option<HoldChange> {
        let since = self.since_ms.take()?;
        self.declared = false;
        Some(HoldChange::Closed {
            why,
            held_ms: self.seen_ms - since,
        })
    }
}

/// THE `WAITING:` READER: whether some line of the agent's last words (`said_tail`, a line a row,
/// its marker and indentation stripped) starts `WAITING:` — exactly, in capitals, with its colon,
/// at the line's start.
#[must_use]
pub fn waiting_declared(said_tail: &str) -> bool {
    said_tail
        .lines()
        .any(|line| line.trim_start().starts_with("WAITING:"))
}

/// A HOLD A LOOP BEFORE THIS ONE LEFT OPEN, as its last `fact-hold@v1` row says it
/// ([`seed_row_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedRow {
    /// The row's own stamp, unix ms: the last word of the loop that watched the hold, and what an
    /// agent process launched after it never saw.
    pub row_ms: i64,
    /// How long the hold had held at the row.
    pub held_ms: i64,
    /// The hold was declared.
    pub declared: bool,
    /// A row of the session's under another rule (no fact's) follows it: the row is not the last
    /// word of the loop that booked it, or a loop that did not watch the hold wrote since.
    pub followed: bool,
}

impl SeedRow {
    /// When the hold began, unix ms.
    #[must_use]
    pub const fn since_wall_ms(&self) -> i64 {
        self.row_ms - self.held_ms
    }
}

/// The reason a `fact-hold@v1` row carries: `hold=open held_ms=<n>[ declared]`, `hold=declared
/// held_ms=<n>`, or `hold=closed why=<word> held_ms=<n>`.
#[must_use]
pub fn row_reason(change: &RowSaid) -> String {
    match *change {
        RowSaid::Open { held_ms, declared } => {
            let declared = if declared { " declared" } else { "" };
            format!("hold=open held_ms={held_ms}{declared}")
        }
        RowSaid::Declared { held_ms } => format!("hold=declared held_ms={held_ms}"),
        RowSaid::Closed { why, held_ms } => {
            format!("hold=closed why={} held_ms={held_ms}", why.word())
        }
    }
}

/// What a `fact-hold@v1` row says ([`row_reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowSaid {
    /// The hold is open, having held `held_ms`.
    Open {
        /// How long it had held at the row.
        held_ms: i64,
        /// Declared already.
        declared: bool,
    },
    /// The open hold was declared.
    Declared {
        /// How long it had held at the row.
        held_ms: i64,
    },
    /// The hold closed.
    Closed {
        /// Why.
        why: Close,
        /// How long it held.
        held_ms: i64,
    },
}

/// THE OPEN HOLD THE LEDGER TEXT `body` LEAVES SESSION `sid` (its rows alone, by `"sid":`): its
/// last `fact-hold@v1` row, where that row says the hold is open or declared, and whether a row of
/// the session's under a rule that is no fact's (`fact-…`) follows it ([`SeedRow::followed`]).
#[must_use]
pub fn seed_row_in(body: &str, sid: Option<&str>) -> Option<SeedRow> {
    let rule = super::super::facts::FactKind::Hold.rule();
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<SeedRow> = None;
    for line in body.lines() {
        if (last.is_none() && !line.contains(rule))
            || want.as_deref().is_some_and(|sid| !line.contains(sid))
        {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        if text("rule_id") != Some(rule) {
            if let Some(open) = &mut last
                && !text("rule_id").is_some_and(crate::supervise::approvals::is_fact_rule)
            {
                open.followed = true;
            }
            continue;
        }
        let (Some(row_ms), Some(reason)) = (
            v.get("ts").and_then(aterm_json::Value::as_i64),
            text("reason"),
        ) else {
            continue;
        };
        let Some(said) = row_said(reason) else {
            continue;
        };
        last = match said {
            RowSaid::Open { held_ms, declared } => Some(SeedRow {
                row_ms,
                held_ms,
                declared,
                followed: false,
            }),
            RowSaid::Declared { held_ms } => Some(SeedRow {
                row_ms,
                held_ms,
                declared: true,
                followed: false,
            }),
            RowSaid::Closed { .. } => None,
        };
    }
    last
}

/// EVERY ROW OF THE HOLD'S CLOCK in the ledger text `body` for session `sid` (its rows alone,
/// by `"sid":`), oldest first, each with its stamp (unix ms): the `fact-hold@v1` rows
/// [`row_reason`] wrote — the door's own `typed` and `skipped` rows under the same rule carry
/// an episode, not a hold, and are passed over, as is a row this build cannot read. What
/// `occupancy check --replay` rebuilds the session's holds from ([`super::replay::spans_of`]).
#[must_use]
pub fn hold_rows_in(body: &str, sid: Option<&str>) -> Vec<(i64, RowSaid)> {
    let rule = super::super::facts::FactKind::Hold.rule();
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut rows = Vec::new();
    for line in body.lines() {
        if !line.contains(rule) || want.as_deref().is_some_and(|sid| !line.contains(sid)) {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want || text("rule_id") != Some(rule) {
            continue;
        }
        let (Some(row_ms), Some(said)) = (
            v.get("ts").and_then(aterm_json::Value::as_i64),
            text("reason").and_then(row_said),
        ) else {
            continue;
        };
        rows.push((row_ms, said));
    }
    rows
}

/// A `fact-hold@v1` row's reason read back ([`row_reason`]); `None` for one this build did not
/// write.
fn row_said(reason: &str) -> Option<RowSaid> {
    let mut words = reason.split(' ');
    let head = words.next()?;
    let mut held_ms = None;
    let mut why = None;
    let mut declared = false;
    for word in words {
        if let Some(n) = word.strip_prefix("held_ms=") {
            held_ms = Some(n.parse::<i64>().ok()?);
        } else if let Some(w) = word.strip_prefix("why=") {
            why = Some(match w {
                "point" => Close::Point,
                "foreign" => Close::Foreign,
                "stale" => Close::Stale,
                _ => return None,
            });
        } else if word == "declared" {
            declared = true;
        } else {
            return None;
        }
    }
    let held_ms = held_ms.filter(|n| *n >= 0)?;
    match head {
        "hold=open" => Some(RowSaid::Open { held_ms, declared }),
        "hold=declared" => Some(RowSaid::Declared { held_ms }),
        "hold=closed" => Some(RowSaid::Closed { why: why?, held_ms }),
        _ => None,
    }
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
