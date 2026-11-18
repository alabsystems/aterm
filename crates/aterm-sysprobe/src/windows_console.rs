// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Console observations retained with the process census they qualify.

use std::collections::{BTreeMap, BTreeSet};

use super::{MAX_CONSOLE_READS, tree_adopting};
use crate::ProcessReading;

/// One original process generation and its observed console host generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsoleFact {
    /// The process's `(pid, creation FILETIME)` from the census.
    pub process: (u32, u64),
    /// Its console host's `(pid, creation FILETIME)`, or a positively observed detached console.
    pub host: Option<(u32, u64)>,
}

/// Bounded console evidence from one census; absence is never a detached-console observation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsoleFacts {
    /// Requested roots bound to their original census generations.
    pub roots: Vec<(u32, u64)>,
    /// Each queried process at most once: roots first, then candidates in census order.
    pub rows: Vec<ConsoleFact>,
    /// A required generation, native observation or bounded query was unavailable.
    pub incomplete: bool,
}

/// A root's tree and adopted console orphans from retained evidence alone.
#[derive(Debug)]
pub struct ConsoleTree<'a> {
    /// Readable known members, each once, including an adopted orphan's descendants.
    pub members: Vec<&'a ProcessReading>,
    /// The known members may omit work: caps, unread identities or console evidence remain.
    pub incomplete: bool,
}

/// Collect console facts once for every requested root and eligible orphan, sharing one cap.
/// `read` returns the opened process's creation FILETIME and its observed host generation;
/// an unread or changed generation is `Err(())`; a positively detached console keeps the
/// observed creation time and returns no host.
/// No process is queried twice, including a root which is itself an orphan.
#[must_use]
pub fn collect_console_facts(
    procs: &[ProcessReading],
    roots: &[u32],
    mut read: impl FnMut(&ProcessReading) -> Result<(u64, Option<(u32, u64)>), ()>,
) -> ConsoleFacts {
    let mut facts = ConsoleFacts::default();
    let mut by_pid = BTreeMap::new();
    for row in procs {
        if by_pid.insert(row.pid, row).is_some() {
            facts.incomplete = true;
        }
    }
    let roots: BTreeSet<_> = roots.iter().copied().collect();
    let mut since = None;
    for root in &roots {
        match by_pid
            .get(root)
            .and_then(|row| row.start)
            .filter(|start| *start > 0)
        {
            Some(start) if *root > 0 => {
                facts.roots.push((*root, start));
                since = Some(since.map_or(start, |old: u64| old.min(start)));
            }
            _ => facts.incomplete = true,
        }
    }
    let Some(since) = since else {
        return facts;
    };
    let orphan = |row: &ProcessReading| {
        row.pid != 0 && (row.ppid == row.pid || !by_pid.contains_key(&row.ppid))
    };
    let candidates: Vec<_> = procs
        .iter()
        .filter(|row| {
            orphan(row)
                && row.start.is_none_or(|start| start >= since)
                && (!roots.contains(&row.pid)
                    || facts.roots.iter().any(|&(pid, start)| {
                        pid != row.pid && row.start.is_none_or(|born| born >= start)
                    }))
        })
        .collect();
    if candidates.is_empty() {
        return facts;
    }
    let mut seen = BTreeSet::new();
    let mut attempted = 0;
    // Roots first: an orphan's own-console membership depends on its root's observation.
    for row in roots
        .iter()
        .filter_map(|pid| by_pid.get(pid).copied())
        .chain(candidates)
    {
        if !seen.insert(row.pid) {
            continue;
        }
        let Some(start) = row.start.filter(|start| *start > 0) else {
            facts.incomplete = true;
            continue;
        };
        if attempted == MAX_CONSOLE_READS {
            facts.incomplete = true;
            continue;
        }
        attempted += 1;
        let host = match read(row) {
            Ok((created, Some((pid, born))))
                if created == start
                    && born > 0
                    && by_pid
                        .get(&pid)
                        .is_some_and(|host| host.start == Some(born)) =>
            {
                Some((pid, born))
            }
            Ok((created, None)) if created == start => None,
            Ok(_) | Err(()) => {
                facts.incomplete = true;
                continue;
            }
        };
        facts.rows.push(ConsoleFact {
            process: (row.pid, start),
            host,
        });
    }
    facts
}

impl ConsoleFacts {
    /// Read a root's membership without a native fallback. A retained fact must still match
    /// both census generations. `procs` must be the same immutable census supplied to the
    /// collector: these facts do not carry ordinary descendant edges across sweeps.
    /// Detached/windowed orphans which no console can identify
    /// remain outside the known tree; this policy does not establish all historical work.
    #[must_use]
    pub fn tree<'a>(&self, procs: &'a [ProcessReading], root: u32) -> ConsoleTree<'a> {
        let mut incomplete = self.incomplete;
        let mut roots = procs.iter().filter(|row| row.pid == root);
        let current = roots
            .next()
            .and_then(|row| row.start)
            .map(|start| (root, start));
        if current.is_none_or(|key| !self.roots.contains(&key)) || roots.next().is_some() {
            return ConsoleTree {
                members: Vec::new(),
                incomplete: true,
            };
        }
        let members = tree_adopting(procs, root, |row| {
            let Some(start) = row.start else {
                incomplete = true;
                return None;
            };
            let mut matches = self
                .rows
                .iter()
                .filter(|fact| fact.process == (row.pid, start));
            let Some(fact) = matches.next() else {
                incomplete = true;
                return None;
            };
            if matches.next().is_some() {
                incomplete = true;
                return None;
            }
            match fact.host {
                Some((pid, start)) => {
                    let mut hosts = procs
                        .iter()
                        .filter(|host| host.pid == pid && host.start == Some(start));
                    if hosts.next().is_none() || hosts.next().is_some() {
                        incomplete = true;
                        None
                    } else {
                        Some(pid)
                    }
                }
                None => None,
            }
        });
        incomplete |= members.is_empty()
            || members.len() >= crate::MAX_GROUP_PIDS
            || members.iter().any(|row| row.start.is_none());
        ConsoleTree {
            members,
            incomplete,
        }
    }
}

#[cfg(test)]
#[path = "windows_console_tests.rs"]
mod tests;
