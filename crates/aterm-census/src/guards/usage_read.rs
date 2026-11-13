// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `usage-read-on-timer`: the rule that nothing polls a provider. A walk over the shipped
//! call graph, by name, from every timer path must reach no provider usage read and no sign-in
//! except through a door that holds an account in use: the row a launch starts on, a live
//! session's own turn end, wall or footer, a person's command, and for a sign-in a signed-out row
//! a session here runs on or one due login this Mac uses (design 2026-11-05 §4.15). A door is a
//! function that means one of those; the walk does not enter it, and a timer's own body naming one
//! is a finding, since a wait is no use.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::graph::{attribute, reach};
use super::{Finding, Rule, SourceFile, Tree, is_word};

const SCOPE: &[&str] = &[
    "crates/aterm-agent/src/",
    "crates/aterm-gui/src/",
    "crates/aterm/src/",
];

/// Token constructors and their evidence doors: no timer may mint provider-read or sign-in
/// authority. The word [`read_bodies`] records for a spawn of `accounts refresh`, which `/usage`
/// follows.
const SPAWNED_READ: &str = "accounts refresh";

const READS: &[&str] = &[
    SPAWNED_READ,
    "launch_read_use",
    "session_read_use",
    "wall_read_use",
    "asked_read_use",
    "launch_use",
    "footer_use",
    "host_use",
    "session_sign_in",
    "renewal_sign_in",
    "asked_sign_in",
    "selected_sign_in",
    "launch_sign_in",
    "granted_sign_in",
];

/// The functions that make a read for an account they hold in use. The walk does not enter them.
const IN_USE: &[(&str, &str)] = &[
    ("launch_env_for", "one launch and its shared read allowance"),
    ("run_agy", "a person's command for the active agy home"),
    (
        "run_kimi_refresh_with",
        "a person's command for the active Kimi row",
    ),
    (
        "prepare_quota_wall",
        "a running Codex session's weekly wall",
    ),
    ("read_launch_rows", "the row a launch starts on"),
    (
        "pooled_starter_held",
        "one worker invocation and its retained read allowance",
    ),
    ("codex_cold_live", "the row a cold relaunch starts on"),
    (
        "cold_launch_who_capturing",
        "the retained row a cold relaunch starts on",
    ),
    ("rotate_worker", "the agy a launch starts"),
    (
        "rotate_worker_at_wall",
        "a running agy stopped at its own wall",
    ),
    ("usage_probe", "a live Claude Code session's turn end"),
    ("probe_step", "a live Claude Code session's turn end"),
    (
        "agy_subscription_with_store_in_dir",
        "a running agy's first footer",
    ),
    ("muse_subscription_for", "a running Muse's footer"),
    (
        "upkeep_sign_in",
        "a signed-out row a session here runs on, or one due login this Mac uses",
    ),
    (
        "selection_sign_in",
        "a person's saved selection of one account",
    ),
    (
        "approved_sign_in",
        "a root's approval of one account for this Mac's first custody",
    ),
    (
        "custody_sign_in",
        "a root's current decision for one account in this Mac's custody, not yet answered here",
    ),
];

/// A name declared this many times or more says nothing about which one a call reaches.
const AMBIGUOUS_AT: usize = 3;

/// A line that waits a set time in a body is that function being a timer's.
const TIMED_WAITS: &[&str] = &[
    "sleep(",
    "recv_timeout(",
    "wait_timeout(",
    "park_timeout(",
    "recv_deadline(",
];

pub(super) const USAGE_READ_ON_TIMER: Rule = Rule {
    id: "usage-read-on-timer",
    why: "a timer that reads a provider's usage for, or signs in to, an account nobody is using \
          is central polling, which is ruled out: it can trigger botting protections",
    scope: SCOPE,
    check: usage_read_on_timer,
};

/// Whether `func`'s name says a timer drives it, it is the window's upkeep, or it is the
/// supervisor's wait, whose timed wait sits behind `wait` and `park`.
fn timer_named(func: &str) -> bool {
    func == "upkeep_live"
        || func == "wait_for_next"
        || func == "tick"
        || func.starts_with("tick_")
        || func.ends_with("_tick")
        || func.ends_with("_timer")
        || func.ends_with("_poll")
}

/// Whether a walk starts at `func`. A timer's name is a timer however many share it (every
/// `tick`). A timed wait is only as good as the name it sits under: bodies merge by name, so one
/// `run` of many that sleeps says nothing about the others.
fn is_timer(func: &str, body: &Body<'_>) -> bool {
    timer_named(func) || (body.timed && body.declared < AMBIGUOUS_AT)
}

fn words(code: &str) -> impl Iterator<Item = &str> {
    code.split(|c: char| !(c.is_ascii() && is_word(c as u8)))
        .filter(|word| !word.is_empty())
}

/// What the walk reads of one shipped function: the words of its body, each with its first spot.
#[derive(Default)]
struct Body<'a> {
    words: BTreeMap<&'a str, (&'a str, usize)>,
    declared: usize,
    timed: bool,
}

fn read_bodies<'a>(files: &[&'a SourceFile]) -> BTreeMap<&'a str, Body<'a>> {
    // Each file's bodies are gathered in hash maps, one lookup a word, and joined to the sorted
    // ones as the file ends: a word keeps its first spot, since it joins after every spot of an
    // earlier file and with its own first line in this one.
    #[derive(Default)]
    struct Gathered<'a> {
        words: HashMap<&'a str, usize>,
        declared: usize,
        timed: bool,
    }
    let mut bodies: BTreeMap<&str, Body<'_>> = BTreeMap::new();
    let mut gathered: HashMap<&str, Gathered<'_>> = HashMap::new();
    for file in files {
        let shipped = file.shipped_flags();
        let spawns = file.lexed().text.contains("\"accounts\"");
        for line in attribute(file) {
            let Some(func) = line.func.filter(|_| !line.in_test) else {
                continue;
            };
            if !shipped.get(line.line.no - 1).copied().unwrap_or(true) {
                continue;
            }
            let body = gathered.entry(func).or_default();
            let code = line.line.code;
            if super::graph::fn_name(code) == Some(func) {
                body.declared += 1;
            }
            // Every timed wait opens a call.
            if !body.timed && code.contains('(') {
                body.timed = TIMED_WAITS.iter().any(|wait| code.contains(wait));
            }
            for word in words(code) {
                body.words.entry(word).or_insert(line.line.no);
            }
            // A read another process makes for it: a spawn of the verb that opens a quota tab.
            let text = line.line.text;
            if spawns && text.contains("\"accounts\"") && text.contains("\"refresh\"") {
                body.words.entry(SPAWNED_READ).or_insert(line.line.no);
            }
        }
        for (func, found) in gathered.drain() {
            let body = bodies.entry(func).or_default();
            body.declared += found.declared;
            body.timed |= found.timed;
            for (word, no) in found.words {
                body.words.entry(word).or_insert((file.rel.as_str(), no));
            }
        }
    }
    bodies
}

fn usage_read_on_timer(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let files: Vec<&SourceFile> = tree
        .corpus
        .under(SCOPE)
        .filter(|file| !tree.corpus.test_code().holds(&file.rel))
        .collect();
    let bodies = read_bodies(&files);
    let doors: BTreeSet<&str> = IN_USE.iter().map(|(door, _)| *door).collect();
    let callees: HashSet<&str> = bodies
        .iter()
        .filter(|(_, callee)| callee.declared < AMBIGUOUS_AT)
        .map(|(func, _)| *func)
        .collect();
    let edges: BTreeMap<&str, Vec<&str>> = bodies
        .iter()
        .filter(|(func, _)| !doors.contains(*func))
        .map(|(func, body)| {
            let next = body
                .words
                .keys()
                .copied()
                .filter(|word| word != func && callees.contains(word))
                .collect();
            (*func, next)
        })
        .collect();
    let starts = bodies
        .iter()
        .filter(|(func, body)| !doors.contains(*func) && is_timer(func, body))
        .map(|(func, _)| *func);
    for (func, body) in bodies
        .iter()
        .filter(|(func, body)| !doors.contains(*func) && is_timer(func, body))
    {
        for door in &doors {
            let Some(&(path, line)) = body.words.get(door) else {
                continue;
            };
            out.push(Finding {
                rule: USAGE_READ_ON_TIMER.id,
                path: path.to_string(),
                line,
                detail: format!(
                    "`{func}` waits on a timer and calls `{door}`: a wait does not hold an account \
                     in use"
                ),
            });
        }
    }
    let from = reach(starts, &edges);
    for func in from.keys() {
        if doors.contains(func) {
            continue;
        }
        let Some(body) = bodies.get(func) else {
            continue;
        };
        for read in READS {
            let Some(&(path, line)) = body.words.get(read) else {
                continue;
            };
            let mut chain = vec![*func];
            let mut at = *func;
            while let Some(Some(prev)) = from.get(at) {
                chain.push(prev);
                at = prev;
            }
            chain.reverse();
            out.push(Finding {
                rule: USAGE_READ_ON_TIMER.id,
                path: path.to_string(),
                line,
                detail: format!(
                    "`{read}` is reached from a timer by {}, through no door that holds the \
                     account in use",
                    chain.join(" -> ")
                ),
            });
        }
    }
}

#[cfg(test)]
#[path = "usage_read_tests.rs"]
mod tests;
