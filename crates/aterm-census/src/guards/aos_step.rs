// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aos-step-clock: the Aos step code reads no clock and draws no randomness.

use super::rules::{ClockShape, UseLeaf, first_hit, type_alias, use_leaves, use_statements};
use super::{Bound, Finding, Rule, Tree, hits};

const AOS_STEP: &[&str] = &[
    "crates/aterm-aos/src/",
    "crates/aterm-aos-pane/src/",
    "crates/aterm-effects/src/aos_momentum",
    "crates/aterm-effects/src/cursor_momentum",
    "crates/aterm-effects/src/cursor_glow",
    "crates/aterm-effects/src/effect_util",
    "crates/aterm-effects/src/color_math",
    "crates/aterm-effects/src/rainbow_kitty/ribbon",
];

const AOS_READS: &[(&str, ClockShape, &str)] = &[
    ("Instant::now", ClockShape::Named, "the monotonic clock"),
    ("SystemTime::now", ClockShape::Named, "the wall clock"),
    (
        "elapsed",
        ClockShape::Method,
        "the monotonic clock (`.elapsed()` reads `Instant::now`)",
    ),
    (
        "since_epoch",
        ClockShape::Reader,
        "the wall clock (`aterm_time::since_epoch`)",
    ),
    (
        "unix_secs",
        ClockShape::Reader,
        "the wall clock (`aterm_time::unix_secs`)",
    ),
    (
        "unix_millis",
        ClockShape::Reader,
        "the wall clock (`aterm_time::unix_millis`)",
    ),
    (
        "current_time_ms",
        ClockShape::Reader,
        "the wall clock (`aterm_types::shell_types::current_time_ms`)",
    ),
    ("thread::sleep", ClockShape::Named, "a sleep"),
    (
        "thread::sleep_until",
        ClockShape::Named,
        "a sleep (`thread::sleep_until`)",
    ),
    (
        "thread::sleep_ms",
        ClockShape::Named,
        "a sleep (`thread::sleep_ms`)",
    ),
    (
        "thread::park_timeout",
        ClockShape::Named,
        "a sleep (`thread::park_timeout`)",
    ),
    (
        "getrandom",
        ClockShape::Named,
        "the system's entropy (`getrandom`)",
    ),
    (
        "aterm_getrandom",
        ClockShape::Named,
        "the system's entropy (`getrandom`)",
    ),
    (
        "thread_rng",
        ClockShape::Named,
        "a random generator (`rand::thread_rng`)",
    ),
    ("OsRng", ClockShape::Named, "the system's entropy (`OsRng`)"),
    (
        "fastrand",
        ClockShape::Named,
        "a random generator (`fastrand`)",
    ),
    (
        "RandomState",
        ClockShape::Named,
        "a per-process random hash key (`RandomState`)",
    ),
    (
        "KeyedHashMap",
        ClockShape::Named,
        "a per-process random hash key (`aterm_hash::KeyedHashMap`)",
    ),
    (
        "KeyedState",
        ClockShape::Named,
        "a per-process random hash key (`aterm_hash::KeyedState`)",
    ),
    (
        "HashMap::new",
        ClockShape::Named,
        "a per-process random hash key (`HashMap::new` hashes under `RandomState`)",
    ),
    (
        "HashMap::with_capacity",
        ClockShape::Named,
        "a per-process random hash key (`HashMap::with_capacity` hashes under `RandomState`)",
    ),
    (
        "HashSet::new",
        ClockShape::Named,
        "a per-process random hash key (`HashSet::new` hashes under `RandomState`)",
    ),
    (
        "HashSet::with_capacity",
        ClockShape::Named,
        "a per-process random hash key (`HashSet::with_capacity` hashes under `RandomState`)",
    ),
];

const RANDOM_MODULES: &[&str] = &["rand", "getrandom", "aterm_getrandom", "fastrand"];

const WALL_READER_MODULE: &str = "aterm_time";

const ENTERS_AS_INPUT: &str =
    "in Aos step code: time and randomness enter only as recorded inputs (M0-14)";

pub(super) const AOS_STEP_CLOCK: Rule = Rule {
    id: "aos-step-clock",
    why: "an Aos task is replayed from a checkpoint over its recorded inputs (M0-14), so a \
          clock read, a sleep or a \
          random draw in the step code (crates/aterm-aos/src, crates/aterm-aos-pane/src, and \
          aterm-effects' aos_momentum and cursor_momentum with the helpers their step calls: \
          cursor_glow, effect_util, color_math and rainbow_kitty/ribbon) is an input no log \
          holds and a replay answers it from the real clock or a fresh seed: take the time \
          from the recorded input (a signed offset from the attach epoch) or from the caller's \
          `now`, draw no randomness, and key maps deterministically (a `BTreeMap`, never a \
          `RandomState`-keyed map)",
    scope: AOS_STEP,
    check: aos_step_clock,
};

fn aos_step_clock(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut needles: Vec<&str> = AOS_READS.iter().map(|r| r.0).collect();
    needles.extend(AOS_READS.iter().flat_map(|r| r.0.split("::")));
    needles.extend(RANDOM_MODULES);
    needles.extend(["type ", WALL_READER_MODULE]);
    for file in tree.corpus.under(AOS_STEP) {
        if tree.corpus.test_code().holds(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        let uses = use_statements(file);
        for line in file.shipped_lines() {
            let code = uses
                .executable
                .get(&line.no)
                .map_or(line.code, String::as_str);
            let found = first_hit(code, AOS_READS.iter(), |_| true)
                .map(|(needle, what)| format!("`{needle}`: {what}, {ENTERS_AS_INPUT}"))
                .or_else(|| random_path(code))
                .or_else(|| type_alias(code).as_ref().and_then(imported));
            if let Some(detail) = found {
                out.push(file.finding(AOS_STEP_CLOCK.id, line.no, detail));
            }
        }
        for (no, stmt) in &uses.statements {
            if let Some(detail) = use_leaves(stmt).iter().find_map(imported) {
                out.push(file.finding(AOS_STEP_CLOCK.id, *no, detail));
            }
        }
    }
}

fn random_path(code: &str) -> Option<String> {
    RANDOM_MODULES.iter().find_map(|module| {
        hits(code, module, Bound::Word)
            .any(|at| {
                let before = code[..at].trim_end_matches([' ', '\t']);
                let after = code[at + module.len()..].trim_start_matches([' ', '\t']);
                !before.ends_with('.') && after.starts_with("::")
            })
            .then(|| {
                format!(
                    "`{module}::`: a random draw through the `{module}` module, {ENTERS_AS_INPUT}"
                )
            })
    })
}

fn imported(leaf: &UseLeaf<'_>) -> Option<String> {
    if leaf.alias == Some("_") {
        return None;
    }
    let shown = leaf.path.join("::");
    let written = leaf
        .alias
        .map_or_else(|| shown.clone(), |a| format!("{shown} as {a}"));
    let binds = |what: &str| format!("imports `{written}`, which binds {what} {ENTERS_AS_INPUT}");
    if let Some(module) = leaf.path.iter().find(|s| RANDOM_MODULES.contains(s)) {
        return Some(binds(&format!("a random draw (the `{module}` module)")));
    }
    if leaf.glob && leaf.path.last() == Some(&WALL_READER_MODULE) {
        return Some(binds("the wall readers bare"));
    }
    AOS_READS.iter().find_map(|&(needle, shape, what)| {
        if matches!(shape, ClockShape::Method) {
            return None;
        }
        let parts: Vec<&str> = needle.split("::").collect();
        let ends = |k: usize| leaf.path.ends_with(&parts[..k]);
        let bound = if leaf.glob {
            parts.len() > 1 && ends(parts.len() - 1)
        } else if ends(parts.len()) {
            true
        } else {
            leaf.alias.is_some()
                && (1..parts.len()).any(|k| ends(k) && leaf.alias != Some(parts[k - 1]))
        };
        bound.then(|| binds(what))
    })
}

#[cfg(test)]
#[path = "aos_step_tests.rs"]
mod tests;
