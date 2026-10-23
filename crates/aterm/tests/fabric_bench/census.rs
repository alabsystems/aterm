// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Log record census for fabric benchmarks.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use astream_broker::store::{COMMIT_SUBJECT, WILL_SUBJECT, is_hidden_subject};

/// One record of the log, as the census needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rec {
    /// Its offset.
    pub seq: u64,
    /// Its idempotency key: `(producer_id, producer_seq)`.
    pub producer: (u64, u64),
    pub subject: String,
    /// The fabric body's `text=`, decoded (empty for a record with none).
    pub text: String,
    /// The body's head line (a presence row's `state=`, `inc=`, `pid=` …).
    pub head: String,
    /// The consumer-group commit it carries: `(group, upto)`.
    pub commit: Option<(String, u64)>,
}

impl Rec {
    /// A pure commit record, or a record that carries a commit beside its output.
    #[must_use]
    pub fn commits(&self) -> bool {
        self.commit.is_some() || self.subject == COMMIT_SUBJECT
    }
}

/// Every record of the stopped broker's log at `path`, in offset order.
/// # Panics
/// The log does not open — the broker still holds it, or it is not a log.
#[must_use]
pub fn read_log(path: &Path) -> Vec<Rec> {
    let log = astream_broker::BrokerLog::open(path)
        .unwrap_or_else(|e| panic!("open the stopped broker's log {}: {e}", path.display()));
    log.read_from(log.base())
        .iter()
        .map(|r| {
            let (body, _) = aterm_link::body::Body::decode(&r.body);
            Rec {
                seq: r.seq.0,
                producer: (r.producer_id, r.producer_seq),
                subject: r.subject.clone(),
                text: body.text,
                head: head_line(&r.body),
                commit: r.commit.as_ref().map(|c| (c.group.clone(), c.upto)),
            }
        })
        .collect()
}

/// A body's first line, lossily decoded: where a presence row keeps its tokens.
#[must_use]
pub fn head_line(body: &[u8]) -> String {
    let head = body.split(|b| *b == b'\n').next().unwrap_or_default();
    String::from_utf8_lossy(head).into_owned()
}

/// The value of the `key=value` token in `head`, if it has one.
#[must_use]
pub fn token<'a>(head: &'a str, key: &str) -> Option<&'a str> {
    head.split_whitespace()
        .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
}

/// A subject with its identities taken out.
#[must_use]
pub fn class_of(subject: &str) -> String {
    subject
        .split('/')
        .map(|seg| {
            if seg.starts_with("n-") {
                "<node>"
            } else if seg.starts_with("s-") {
                "<sid>"
            } else if seg.starts_with("h-") || seg.starts_with("a-") {
                "<principal>"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The records of one window, counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Census {
    /// Every record, hidden or not.
    pub total: usize,
    /// Records a subscriber or a `Fetch` would never see (`/a/commit`, `/a/will`, `/a/bind`).
    pub hidden: usize,
    /// Records that commit a consumer group ([`Rec::commits`]).
    pub commits: usize,
    /// Every kind ([`class_of`]) and how many of it.
    pub by_class: BTreeMap<String, usize>,
}

/// The records with offsets in `lo..=hi`.
pub fn window(recs: &[Rec], lo: u64, hi: u64) -> impl Iterator<Item = &Rec> {
    recs.iter().filter(move |r| (lo..=hi).contains(&r.seq))
}

/// The census of `recs` — a [`window`], perhaps filtered.
#[must_use]
pub fn census<'a>(recs: impl IntoIterator<Item = &'a Rec>) -> Census {
    let mut c = Census::default();
    for r in recs {
        c.total += 1;
        c.hidden += usize::from(is_hidden_subject(&r.subject));
        c.commits += usize::from(r.commits());
        *c.by_class.entry(class_of(&r.subject)).or_default() += 1;
    }
    c
}

/// The first record at or after `from` that commits `group` up to `upto` or past it.
#[must_use]
pub fn covering_commit<'a>(recs: &'a [Rec], group: &str, upto: u64, from: u64) -> Option<&'a Rec> {
    recs.iter().filter(|r| r.seq >= from).find(|r| {
        r.commit
            .as_ref()
            .is_some_and(|(g, u)| g == group && *u >= upto)
    })
}

/// The offsets of every record that is a FIRED WILL.
#[must_use]
pub fn will_firings(recs: &[Rec]) -> BTreeSet<u64> {
    let mut registered: HashSet<(u64, u64)> = HashSet::new();
    let mut fired = BTreeSet::new();
    for r in recs {
        if r.subject == WILL_SUBJECT {
            registered.insert(r.producer);
        } else if registered.contains(&r.producer) {
            fired.insert(r.seq);
        }
    }
    fired
}

/// Whether the record right after offset `seq` is `group`'s commit covering it.
/// # Errors
/// What sits at `seq + 1` instead.
pub fn next_is_its_commit(recs: &[Rec], group: &str, seq: u64) -> Result<(), String> {
    let next = recs.iter().find(|r| r.seq == seq + 1);
    match next {
        Some(r)
            if r.commit
                .as_ref()
                .is_some_and(|(g, u)| g == group && *u >= seq) =>
        {
            Ok(())
        }
        Some(r) => Err(format!(
            "offset {} after the delivery at {seq} is `{}` (commit {:?}), not {group}'s commit \
             covering {seq}",
            r.seq, r.subject, r.commit
        )),
        None => Err(format!("no record follows the delivery at {seq}")),
    }
}

/// Whether `group`'s commit covering offset `upto` lands BEFORE the record at `fence`.
/// # Errors
/// No commit covers `upto`, or the one that does lands after `fence`.
pub fn lands_before(recs: &[Rec], group: &str, upto: u64, fence: u64) -> Result<(), String> {
    match covering_commit(recs, group, upto, upto) {
        Some(r) if r.seq < fence => Ok(()),
        Some(r) => Err(format!(
            "{group}'s commit covering {upto} is at offset {} (through {:?}), after the fence's \
             record at {fence}",
            r.seq,
            r.commit.as_ref().map(|(_, u)| *u)
        )),
        None => Err(format!("no commit of {group} covers offset {upto}")),
    }
}

/// The census as JSON: totals, and per message when `messages` is non-zero.
#[must_use]
pub fn to_json(c: &Census, messages: usize, window: (u64, u64)) -> aterm_json::Value {
    let per = |n: usize| {
        if messages == 0 {
            aterm_json::Value::Null
        } else {
            aterm_json::json!(crate::stats::round3(n as f64 / messages as f64))
        }
    };
    let mut classes = aterm_json::Map::new();
    for (class, n) in &c.by_class {
        classes.insert(
            class.clone(),
            aterm_json::json!({"records": n, "per_message": per(*n)}),
        );
    }
    aterm_json::json!({
        "window": {"first_offset": window.0, "last_offset": window.1},
        "messages": messages,
        "records": c.total,
        "records_per_message": per(c.total),
        "hidden": c.hidden,
        "commit_records": c.commits,
        "commit_records_per_message": per(c.commits),
        "by_class": aterm_json::Value::Object(classes),
    })
}

/// The census as markdown rows: one per kind, then the totals.
#[must_use]
pub fn rows(c: &Census, messages: usize) -> Vec<String> {
    let per = |n: usize| {
        if messages == 0 {
            "–".to_string()
        } else {
            format!("{:.3}", n as f64 / messages as f64)
        }
    };
    let mut out = vec![
        "| record kind | records | per message |".to_string(),
        "|---|---:|---:|".to_string(),
    ];
    for (class, n) in &c.by_class {
        out.push(format!("| `{class}` | {n} | {} |", per(*n)));
    }
    out.push(format!(
        "| **all records** | {} | {} |",
        c.total,
        per(c.total)
    ));
    out.push(format!(
        "| of which commit records | {} | {} |",
        c.commits,
        per(c.commits)
    ));
    out
}
