// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side instance node log ([`aterm_slog::NodeLog`]) under its leaf lock.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{LazyLock, Mutex};

use aterm_gui_fabric::{LinkMove, LinkSaid};
use aterm_slog::{Actor, Conf, Fields, Key, Kind, Mark, NodeLog, Src};
#[cfg(any(unix, test))]
use aterm_update::recovery_ledger::EndClass;

use crate::session_log::{Clock, LogCopy, LogQuery, NODE_LOG_USAGE, render_log_copy};

/// The most `config key=` marks one published snapshot writes.
pub(crate) const CONFIG_KEYS_MAX: usize = 16;

/// One instance's node log under its leaf lock.
pub(crate) struct NodeLogHost {
    inner: Mutex<Inner>,
}

struct Inner {
    log: NodeLog,
    clock: Clock,
}

impl NodeLogHost {
    /// A node log of epoch `ep` reading `clock`, its start written.
    pub(crate) fn started(ep: u32, clock: Clock) -> NodeLogHost {
        let mut log = NodeLog::new(ep);
        let f = Fields::new().word(Key::PrevEnd, "-").unwrap_or_default();
        if let Err(e) = log.start(f, clock.now()) {
            aterm_log::warn!("node log: the start was refused: {e:?}");
        }
        NodeLogHost {
            inner: Mutex::new(Inner { log, clock }),
        }
    }

    /// Run `f` on the log under the leaf lock (taken last, nothing under it).
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut inner)
    }

    /// The previous run's end, read off the recovery ledger.
    #[cfg(any(unix, test))]
    pub(crate) fn note_prev_end(&self, class: EndClass) {
        let (word, code) = end_word(class);
        let f = Fields::new()
            .word(Key::PrevEnd, word)
            .and_then(|f| match code {
                Some(n) => f.i(Key::Code, i64::from(n)),
                None => Ok(f),
            });
        let Ok(f) = f else {
            return;
        };
        self.with(|i| {
            let at = i.clock.now();
            if let Err(e) = i.log.amend_start(f, at) {
                aterm_log::warn!("node log: prev_end refused: {e:?}");
            }
        });
    }

    /// A stored fabric link transition.
    pub(crate) fn note_link(&self, mv: LinkMove) {
        let (by, conf) = match mv.said {
            LinkSaid::Report => (Actor::Bridge, Conf::Exact),
            LinkSaid::Evidence => (Actor::Bridge, Conf::Heuristic),
            LinkSaid::Attached | LinkSaid::Lost => (Actor::Aterm, Conf::Strong),
        };
        let Ok(f) = Fields::new()
            .word(Key::State, mv.state)
            .and_then(|f| f.id(Key::Gen, &mv.generation.to_string()))
        else {
            return;
        };
        let m = Mark::new(Kind::Link, Src::Fabric, conf).by(by).fields(f);
        self.mark(m);
    }

    /// The config keys a published snapshot moved.
    pub(crate) fn note_config(&self, names: &BTreeSet<&'static str>, unnamed: usize) {
        let named = names.len().min(CONFIG_KEYS_MAX);
        for name in names.iter().take(named) {
            if let Ok(f) = Fields::new().id(Key::CfgKey, name) {
                self.mark(config_mark(f));
            }
        }
        let rest = unnamed + (names.len() - named);
        if rest > 0
            && let Ok(f) = Fields::new().u(Key::N, rest as u64)
        {
            self.mark(config_mark(f));
        }
    }

    fn mark(&self, m: Mark) {
        self.with(|i| {
            let at = i.clock.now();
            if let Err(e) = i.log.mark(m, at) {
                aterm_log::warn!("node log: a mark was refused: {e:?}");
            }
        });
    }

    /// What a `log n` reply shows, copied under the lock (rendered after it).
    fn copied(&self, q: &LogQuery) -> LogCopy {
        self.with(|i| LogCopy::copied_from_node(&i.log, q))
    }

    /// The last seq written (tests read past it).
    #[cfg(test)]
    pub(crate) fn last_seq(&self) -> u64 {
        self.with(|i| i.log.last_seq())
    }

    /// Every live record as its line, at the content tier (everything it holds).
    #[cfg(test)]
    pub(crate) fn lines(&self) -> Vec<String> {
        self.with(|i| {
            i.log
                .window()
                .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Content).unwrap_or_default())
                .collect()
        })
    }
}

fn config_mark(f: Fields) -> Mark {
    Mark::new(Kind::Config, Src::Aterm, Conf::Exact)
        .by(Actor::Aterm)
        .fields(f)
}

/// The `prev_end=` word of a ledger end class, and a signal's number.
#[cfg(any(unix, test))]
const fn end_word(class: EndClass) -> (&'static str, Option<u8>) {
    match class {
        EndClass::Clean => ("clean", None),
        EndClass::Handoff => ("handoff", None),
        EndClass::Killed => ("killed", None),
        EndClass::Signal(n) => ("signal", Some(n)),
        EndClass::Panic => ("panic", None),
        EndClass::Running => ("running", None),
        EndClass::Unknown => ("unknown", None),
        EndClass::None => ("none", None),
    }
}

/// Schema names of changed keys between `before` and `after`, plus the unnamed count.
pub(crate) fn changed_config_keys(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> (BTreeSet<&'static str>, usize) {
    let changed: Vec<&str> = before
        .iter()
        .filter(|(k, v)| after.get(*k) != Some(*v))
        .map(|(k, _)| k.as_str())
        .chain(
            after
                .keys()
                .filter(|k| !before.contains_key(*k))
                .map(String::as_str),
        )
        .collect();
    let is_parent = |p: &str| {
        changed
            .iter()
            .any(|q| q.len() > p.len() && q.starts_with(p) && q.as_bytes()[p.len()] == b'.')
    };
    let mut names = BTreeSet::new();
    let mut unnamed = 0;
    for path in changed.iter().copied().filter(|p| !is_parent(p)) {
        match schema_name(path) {
            Some(name) => {
                names.insert(name);
            }
            None => unnamed += 1,
        }
    }
    (names, unnamed)
}

/// The schema row's static key for `path` or its nearest ancestor.
fn schema_name(path: &str) -> Option<&'static str> {
    let mut at = path;
    loop {
        if let Some(entry) = crate::native_config_language::config_schema_entry(at) {
            return Some(entry.key);
        }
        at = &at[..at.rfind('.')?];
    }
}

/// This process's node-log epoch, from its existing launch identity.
fn process_epoch() -> u32 {
    instance_epoch(crate::control_auth::process_instance_id())
}

/// Bounded 32-bit FNV-1a fingerprint of the process instance id.
const fn instance_epoch(instance: &str) -> u32 {
    let hash = aterm_hash::fnv1a_64(instance.as_bytes()).to_le_bytes();
    u32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]])
}

/// The instance's node log.
static NODE_LOG: LazyLock<NodeLogHost> =
    LazyLock::new(|| NodeLogHost::started(process_epoch(), Clock::default()));

/// The instance's node log (its start written on first touch).
pub(crate) fn node_log() -> &'static NodeLogHost {
    &NODE_LOG
}

/// Start the node log and hear the fabric link.
pub(crate) fn install() {
    let _ = node_log();
    aterm_gui_fabric::on_link_moved(heard_link);
}

/// Whether a `log` request names the node log.
pub(crate) fn names_node_log(rest: &str) -> bool {
    rest.split_whitespace().any(|w| w == "n")
}

/// `log n [since=<seq> ep=<epoch>] [n=<count>] [kinds=<k>[,...]] [--json]`.
pub(crate) fn cmd_node_log(host: &NodeLogHost, rest: &str) -> String {
    let rest = rest
        .split_whitespace()
        .filter(|w| *w != "n")
        .collect::<Vec<_>>()
        .join(" ");
    let q = match LogQuery::parse(&rest) {
        Ok(q) if q.node == aterm_slog::Node::SESSION && q.lvl.is_none() && !q.turn_class => q,
        Ok(_) => return NODE_LOG_USAGE.to_string(),
        Err(usage) => return usage.to_string(),
    };
    render_log_copy(&host.copied(&q), &q)
}

/// The fabric's hook ([`install`]).
fn heard_link(mv: LinkMove) {
    node_log().note_link(mv);
}

#[cfg(test)]
#[path = "session_log_node_tests.rs"]
mod tests;
