// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO DRIVES (`docs/DESIGN-owner-surfaces-2026-10-16.md` §5, S9).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// The longest name a chip or a hover gives a manager, in grid cells.
pub(crate) const LABEL_CELLS: usize = 16;

/// One pooled session as the table reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    /// Its local id.
    pub(crate) local: u64,
    /// Its global sid.
    pub(crate) sid: String,
    /// What a worker's chip names it by, raw.
    pub(crate) name: String,
    /// Its task's `from=` where that names a session.
    pub(crate) from: Option<String>,
    /// Its task's state word and when it entered it, for a worker's hover.
    pub(crate) task: Option<(&'static str, std::time::Instant)>,
}

/// A worker's manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Manager {
    /// Its local id, where it is a session of this instance.
    pub(crate) local: Option<u64>,
    /// Its name as the chip and the hover print it.
    pub(crate) label: Arc<str>,
}

/// One session's place in its team.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Team {
    /// The manager its task reports to, where it is a worker.
    pub(crate) manager: Option<Manager>,
    /// How many sessions' tasks name it.
    pub(crate) workers: u16,
    /// Those sessions' local ids, in the order they were made.
    pub(crate) members: Vec<u64>,
    /// `⇉<workers>`, composed once a build (empty with none): the manager's chip prints it.
    pub(crate) workers_mark: Arc<str>,
    /// A worker's task state word and when it entered it (`task working 4m` on the chip's hover).
    pub(crate) task: Option<(&'static str, std::time::Instant)>,
}

impl Team {
    /// The strip's marks for this team.
    #[must_use]
    pub(crate) const fn strip(&self) -> crate::tab_bar::TabTeam {
        crate::tab_bar::TabTeam {
            worker: self.manager.is_some(),
            workers: self.workers,
        }
    }

    /// The footer's repaint term: moves when anything the chip says moves.
    #[must_use]
    pub(crate) fn term(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.manager
            .as_ref()
            .map(|m| (m.local, &*m.label))
            .hash(&mut h);
        self.workers.hash(&mut h);
        h.finish() | 1
    }
}

/// THE TABLE: each session that is a worker or a manager, from `rows`.
#[must_use]
pub(crate) fn table(rows: &[Row]) -> HashMap<u64, Team> {
    let by_sid: HashMap<&str, &Row> = rows.iter().map(|row| (row.sid.as_str(), row)).collect();
    let mut workers: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut out: HashMap<u64, Team> = HashMap::new();
    for row in rows {
        let Some(from) = row.from.as_deref().filter(|from| *from != row.sid) else {
            continue;
        };
        let manager = by_sid.get(from).copied();
        if let Some(manager) = manager {
            workers.entry(manager.local).or_default().push(row.local);
        }
        let name = manager.map_or_else(
            || crate::presence::short_sid(from),
            |manager| manager.name.clone(),
        );
        let team = out.entry(row.local).or_default();
        team.manager = Some(Manager {
            local: manager.map(|manager| manager.local),
            label: Arc::from(crate::presence::sanitize_token(&name, LABEL_CELLS)),
        });
        team.task = row.task;
    }
    for (local, mut members) in workers {
        members.sort_unstable();
        let team = out.entry(local).or_default();
        team.workers = u16::try_from(members.len()).unwrap_or(u16::MAX);
        team.workers_mark = Arc::from(format!("\u{21c9}{}", team.workers));
        team.members = members;
    }
    out
}

#[cfg(test)]
#[path = "team_tests.rs"]
mod tests;
