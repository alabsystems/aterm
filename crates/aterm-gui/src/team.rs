// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO DRIVES (`docs/DESIGN-owner-surfaces-2026-10-16.md` §5, S9): the team
//! a session is in — the manager its task reports to (the record's `from=`)
//! and how many workers name it — as the Claude rule's Team chip
//! (`crate::claude_lights`), the strip's marks (`tab_bar::TabTeam`) and the
//! tab's hover read it.
//!
//! It is the one team table: the tab menu's AGENT rows (a worker's Go to
//! its manager, a manager's Workers rows) and the long hold's entry read it
//! too, so no reader walks the task records for teams again.
//!
//! A RECORD: no need, no colour, no wake of its own. The table is built whole
//! at change rate (`App::rebuild_teams`: a task moved, a role or a title
//! moved, a tab retired, and `App::refresh_presence_all_windows` — a
//! restore, a successor's commit, a config adopt; never a focus change,
//! which moves nothing it holds) from one [`Row`] per pooled
//! session, and every frame reads it without a lock. Pure, so it is tested
//! off macOS.

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
    /// What a worker's chip names it by, raw: its tab's label as every
    /// Agents surface names it (`App::tab_label_of_session`).
    pub(crate) name: String,
    /// Its task's `from=` where that names a session: `None` with no task
    /// record, and for `from=ctl` (a task the instance's own client started,
    /// which no chip names).
    pub(crate) from: Option<String>,
    /// Its task's state word and when it entered it, for a worker's hover.
    pub(crate) task: Option<(&'static str, std::time::Instant)>,
}

/// A worker's manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Manager {
    /// Its local id, where it is a session of this instance.
    pub(crate) local: Option<u64>,
    /// Its name as the chip and the hover print it: its [`Row::name`], or its
    /// short sid where it is not here, held to the presence token rule.
    pub(crate) label: Arc<str>,
}

/// One session's place in its team.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Team {
    /// The manager its task reports to, where it is a worker.
    pub(crate) manager: Option<Manager>,
    /// How many sessions' tasks name it.
    pub(crate) workers: u16,
    /// Those sessions' local ids, in the order they were made (ascending:
    /// the pool is a map, so its own order is none): what its tab menu's
    /// Workers rows list (`App::agent_menu_facts`), each read off its own
    /// [`Team::task`].
    pub(crate) members: Vec<u64>,
    /// `⇉<workers>`, composed once a build (empty with none): the manager's
    /// chip prints it.
    pub(crate) workers_mark: Arc<str>,
    /// A worker's task state word and when it entered it (`task working 4m`
    /// on the chip's hover).
    pub(crate) task: Option<(&'static str, std::time::Instant)>,
}

impl Team {
    /// The strip's marks for this team.
    #[must_use]
    pub(crate) fn strip(&self) -> crate::tab_bar::TabTeam {
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
/// A worker's manager is the row whose sid its `from=` names, else the
/// short sid it names (a manager not here); a manager's count is the rows
/// whose `from=` names it. A row naming itself is neither.
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

    /// A manager with three workers here, one worker of a manager not here,
    /// a task the instance's own client started, and a row naming itself.
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

    /// A manager's workers are listed in the order they were made, whatever
    /// order the pool (a map) hands the rows over in.
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

    /// A manager's name is held to the presence token rule: no control byte,
    /// no bidi override, no presence glyph, at most [`LABEL_CELLS`] cells.
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

    /// The footer's term moves with the chip's words and only with them.
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
