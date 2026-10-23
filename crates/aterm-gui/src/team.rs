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
mod tests {
    use super::*;

    fn row(local: u64, sid: &str, name: &str, from: Option<&str>) -> Row {
        Row {
            local,
            sid: sid.to_string(),
            name: name.to_string(),
            from: from.map(str::to_string),
            task: None,
        }
    }

    #[test]
    fn workers_name_their_manager_and_a_manager_counts_them() {
        let rows = [
            row(1, "s-aa", "lab-sweep", None),
            row(2, "s-bb", "bench", Some("s-aa")),
            row(3, "s-cc", "fmt", Some("s-aa")),
            row(4, "s-dd", "lint", Some("s-aa")),
            row(5, "s-ee", "remote", Some("s-1e91ffff")),
            row(6, "s-ff", "self", Some("s-ff")),
            row(7, "s-00", "ctl", None),
        ];
        let teams = table(&rows);
        let manager = &teams[&1];
        assert_eq!((manager.workers, manager.manager.as_ref()), (3, None));
        assert_eq!(&*manager.workers_mark, "\u{21c9}3");
        assert_eq!(
            manager.members,
            [2, 3, 4],
            "its workers, in the order they were made"
        );
        for worker in [2, 3, 4] {
            let boss = teams[&worker].manager.as_ref().expect("a worker");
            assert_eq!((boss.local, &*boss.label), (Some(1), "lab-sweep"));
            assert_eq!(teams[&worker].workers, 0);
        }
        let remote = teams[&5]
            .manager
            .as_ref()
            .expect("a worker of a sid not here");
        assert_eq!((remote.local, &*remote.label), (None, "s-1e91"));
        assert!(!teams.contains_key(&6), "a row naming itself is no team");
        assert!(!teams.contains_key(&7), "from=ctl names no chip");
        assert_eq!(
            teams[&2].strip(),
            crate::tab_bar::TabTeam {
                worker: true,
                workers: 0
            }
        );
    }

    #[test]
    fn a_managers_workers_are_listed_in_the_order_they_were_made() {
        let rows = [
            row(4, "s-dd", "lint", Some("s-aa")),
            row(1, "s-aa", "lab-sweep", None),
            row(3, "s-cc", "fmt", Some("s-aa")),
            row(2, "s-bb", "bench", Some("s-aa")),
        ];
        assert_eq!(table(&rows)[&1].members, [2, 3, 4]);
    }

    #[test]
    fn a_managers_name_is_sanitized() {
        let rows = [
            row(
                1,
                "s-aa",
                "evil\u{202e}\u{1b}[31m\u{25c6}role-that-is-far-too-long",
                None,
            ),
            row(2, "s-bb", "w", Some("s-aa")),
        ];
        let label = table(&rows)[&2].manager.clone().expect("a worker").label;
        assert!(!label.contains('\u{202e}') && !label.contains('\u{1b}'));
        assert!(!label.contains('\u{25c6}'));
        assert!(label.chars().count() <= LABEL_CELLS, "{label:?}");
    }

    #[test]
    fn the_term_moves_with_what_the_chip_says() {
        let rows = |name: &str| {
            table(&[
                row(1, "s-aa", name, None),
                row(2, "s-bb", "w", Some("s-aa")),
            ])
        };
        assert_eq!(rows("a")[&2].term(), rows("a")[&2].term());
        assert_ne!(rows("a")[&2].term(), rows("b")[&2].term());
        assert_ne!(Team::default().term(), 0);
    }
}
