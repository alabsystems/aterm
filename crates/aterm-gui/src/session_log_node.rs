// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE INSTANCE'S NODE LOG, host side (session log 5J-2, ruling T27; design
//! §2.1 "node", §2.6 "Per instance", §6's instance row): one
//! [`aterm_slog::NodeLog`] per process, what the instance itself did.
//!
//! Three producers, each a typed call:
//! - **`start prev_end=`.** Written when the node log is first touched
//!   ([`install`], at `main_entry`) as `prev_end=-`; the recovery census
//!   (`recovery_census::record`'s thread) amends it with how the run before
//!   this launch ended once it has read the ledger ([`NodeLogHost::note_prev_end`]:
//!   `amend n start prev_end=<word>`, a caught signal's number in `code=`). A
//!   launch that never reads the ledger (plain headless, dev) keeps `-`; a lab
//!   desk reads its private root's own (Phase 6 step 6C-4,
//!   `recovery_census::cold_census`).
//! - **`link state=`.** Every stored transition of the fabric link, through
//!   `aterm_gui_fabric::on_link_moved` (that crate has no aterm-slog
//!   dependency), installed by [`install`] before any bridge can attach.
//! - **`config key=`.** Every schema key whose authored value moved in a
//!   published config snapshot (`App::publish_native_config_snapshot`, the one
//!   funnel every admitted snapshot passes): the schema's static NAME
//!   ([`changed_config_keys`]), never a value and never a name a person chose.
//!   A map member is named by the schema row that holds it; a changed key the
//!   schema does not know, or one past [`CONFIG_KEYS_MAX`], is only counted
//!   (`config n=`).
//!
//! THE LEAF LOCK. Each host holds its log under one `Mutex`, and nothing is
//! ever taken under it: a write appends one record, and a read (`log n`,
//! [`cmd_node_log`]) copies what its reply shows and renders after the lock
//! is gone.
//! It is taken under the fabric's link locks (the hook runs under
//! `LINK.generation`, and in a report or a delivery's evidence under
//! `LINK.link` too), so the ORDER is `LINK.log_transition` →
//! `LINK.generation` → `LINK.link` → [`NodeLogHost`]'s lock, and under no
//! other lock of this crate's.
//!
//! Local, additive and RAM only (T27): never journaled (D3), never carried
//! (each process writes its own `start`) and never on the bus (D4): no
//! record it takes is bus class.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{LazyLock, Mutex};

use aterm_gui_fabric::{LinkMove, LinkSaid};
use aterm_slog::{Actor, Conf, Fields, Key, Kind, Mark, NodeLog, Src};
// The recovery census, which reads the ledger, is unix's (`lib.rs`'s `mod
// recovery_census`).
#[cfg(any(unix, test))]
use aterm_update::recovery_ledger::EndClass;

use crate::session_log::{Clock, LogCopy, LogQuery, NODE_LOG_USAGE, render_log_copy};

/// The most `config key=` marks one published snapshot writes; the rest of
/// its changed keys are counted in one `config n=` mark.
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
    /// A node log of epoch `ep` reading `clock`, its start written
    /// (`prev_end=-`: the ledger is not read yet).
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

    /// The previous run's end, read off the recovery ledger: `amend n start
    /// prev_end=<word>` (a signal's number in `code=`).
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

    /// A stored fabric link transition: `mark n link state=<word> gen=<n>`,
    /// attributed to the bridge when it is the bridge's word or deed, else
    /// to aterm.
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

    /// The config keys a published snapshot moved: one `mark n config
    /// key=<name>` per name, then `config n=<count>` for those not named.
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

    /// Every live record as its line, at the content tier (everything it
    /// holds): what the tests read.
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
/// Exhaustive: a new class does not compile until it has a word.
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

/// The schema NAMES of the keys whose authored values differ between two
/// semantic value maps (`ConfigSnapshot::semantic_values`), and how many
/// changed keys no schema row names. Only the deepest changed paths count (a
/// table's whole-table entry moves with any child). A path the schema does
/// not hold is named by its nearest ancestor the schema does (a map's member
/// by its map), so a name a person chose never reaches a record: every name
/// returned is a schema row's static key.
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

/// This process's node-log epoch, from its existing launch identity. A
/// restart must not deterministically reuse an epoch just because both
/// starts fell in one wall second; this log is never carried.
fn process_epoch() -> u32 {
    instance_epoch(crate::control_auth::process_instance_id())
}

/// A bounded fingerprint of the process identity (a launch nonce, with the
/// existing high-resolution fallback), not a timestamp or an authority token.
/// As with any finite u32 epoch, collisions remain possible.
fn instance_epoch(instance: &str) -> u32 {
    let hash = aterm_hash::fnv1a_64(instance.as_bytes()).to_le_bytes();
    u32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]])
}

/// THE instance's node log.
static NODE_LOG: LazyLock<NodeLogHost> =
    LazyLock::new(|| NodeLogHost::started(process_epoch(), Clock::default()));

/// The instance's node log (its start written on first touch).
pub(crate) fn node_log() -> &'static NodeLogHost {
    &NODE_LOG
}

/// Start the node log and hear the fabric link: called once at `main_entry`,
/// before any bridge can attach. Idempotent.
pub(crate) fn install() {
    let _ = node_log();
    aterm_gui_fabric::on_link_moved(heard_link);
}

/// Whether a `log` request names the node log: a bare `n` among its words
/// (`n=<count>` is a count). Such a request is the instance's, Owner-only like
/// `brief all`, and answered with no terminal (`control`'s dispatch).
pub(crate) fn names_node_log(rest: &str) -> bool {
    rest.split_whitespace().any(|w| w == "n")
}

/// `log n [since=<seq> ep=<epoch>] [n=<count>] [kinds=<k>[,...]] [--json]`
/// (session log 5J-3): `host`'s records as `log` prints a session's, under
/// the same header. A node subtree, `lvl=` or `class=turn` names nothing
/// here: a usage error.
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
