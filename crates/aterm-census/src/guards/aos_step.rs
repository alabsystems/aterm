// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aos-step-clock: the Aos step code reads no clock and draws no randomness.

use super::rules::{
    ClockShape, UseLeaf, first_hit, test_path, type_alias, use_leaves, use_statements,
};
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
    why: "an Aos task is replayed from a checkpoint over its recorded inputs \
          (docs/DESIGN-aos-hosted-execution-2026-10-21.md, M0-14), so a clock read, a sleep or a \
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
    let test_only: std::collections::BTreeSet<String> = AOS_STEP
        .iter()
        .flat_map(|root| tree.corpus.test_only(root))
        .collect();
    let mut needles: Vec<&str> = AOS_READS.iter().map(|r| r.0).collect();
    needles.extend(AOS_READS.iter().flat_map(|r| r.0.split("::")));
    needles.extend(RANDOM_MODULES);
    needles.extend(["type ", WALL_READER_MODULE]);
    for file in tree.corpus.under(AOS_STEP) {
        if test_path(&file.rel) || test_only.contains(&file.rel) || !file.mentions(&needles) {
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
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    fn findings(files: &[(&str, &str)]) -> Vec<String> {
        run(&AOS_STEP_CLOCK, files)
            .iter()
            .map(|f| format!("{}:{}", f.path, f.line))
            .collect()
    }

    #[test]
    fn a_planted_clock_or_random_read_in_the_step_code_is_found_and_test_code_is_not() {
        let planted = "use aterm_time::Instant;\n\
                       fn a() -> Instant { Instant::now() }\n\
                       fn b() { let w = std::time::SystemTime::now(); }\n\
                       fn c(t: Instant) -> u128 { t.elapsed().as_nanos() }\n\
                       fn d() -> u64 { aterm_time::unix_millis().unwrap_or(0) }\n\
                       fn e() { std::thread::sleep(GAP); }\n\
                       fn f(b: &mut [u8]) { getrandom::getrandom(b).ok(); }\n\
                       fn g() -> u32 { rand::random::<u32>() }\n\
                       fn h() { let m: HashMap<u8, u8> = HashMap::new(); }\n\
                       fn i(now: Instant, epoch: Instant) -> Duration { now.duration_since(epoch) }\n\
                       // Instant::now() is what this file must never read\n\
                       const SAID: &str = \"SystemTime::now\";\n\
                       fn j(self) -> u8 { self.rand }\n\
                       #[cfg(test)]\n\
                       mod tests {\n    fn t() { let _ = std::time::Instant::now(); }\n}\n\
                       #[cfg(any(test, debug_assertions))]\n\
                       fn seam() { let _ = std::time::Instant::now(); }\n";
        let lib = "#[cfg(test)]\nmod rig;\nmod live;\n";
        let found = findings(&[
            ("crates/aterm-aos/src/publish.rs", planted),
            ("crates/aterm-effects/src/aos_momentum.rs", planted),
            (
                "crates/aterm-effects/src/cursor_momentum/snapshot.rs",
                planted,
            ),
            (
                "crates/aterm-effects/src/aos_momentum/host_tests.rs",
                planted,
            ),
            (
                "crates/aterm-effects/src/cursor_momentum/test_fixtures.rs",
                planted,
            ),
            ("crates/aterm-aos/src/lib.rs", lib),
            ("crates/aterm-aos/src/rig.rs", planted),
            ("crates/aterm-aos/tests/toy.rs", planted),
            ("crates/aterm-effects/src/cursor_fx.rs", planted),
            ("crates/aterm-gui/src/aos_verb.rs", planted),
        ]);
        let lines = [2, 3, 4, 5, 6, 7, 8, 9, 19];
        let mut expected: Vec<String> = Vec::new();
        for file in [
            "crates/aterm-aos/src/publish.rs",
            "crates/aterm-effects/src/aos_momentum.rs",
            "crates/aterm-effects/src/cursor_momentum/snapshot.rs",
        ] {
            expected.extend(lines.iter().map(|l| format!("{file}:{l}")));
        }
        assert_eq!(found, expected);
    }

    #[test]
    fn a_clock_read_planted_in_the_m4_plan_lane_evaluator_or_publication_is_found() {
        let planted = "use aterm_time::Instant;\n\
                       fn a() -> Instant { Instant::now() }\n\
                       fn b(t: Instant) -> u128 { t.elapsed().as_nanos() }\n\
                       fn c() { std::thread::sleep(GAP); }\n\
                       fn d() -> u32 { rand::random::<u32>() }\n\
                       fn e(now: Instant, epoch: Instant) -> Duration { now.duration_since(epoch) }\n";
        let parent = "#[cfg(any(test, debug_assertions, feature = \"aos-candidates\"))]\n\
                      pub mod bundle;\n\
                      pub mod lane;\n\
                      #[cfg(test)]\n\
                      mod lane_tests;\n\
                      #[cfg(any(test, debug_assertions, feature = \"aos-candidates\"))]\n\
                      pub mod m4;\n\
                      #[cfg(test)]\n\
                      mod m4_tests;\n\
                      #[cfg(any(test, debug_assertions, feature = \"aos-candidates\"))]\n\
                      pub mod plan;\n\
                      #[cfg(any(test, debug_assertions, feature = \"aos-candidates\"))]\n\
                      pub mod publish;\n\
                      #[cfg(test)]\n\
                      mod publish_tests;\n\
                      #[cfg(test)]\n\
                      mod rig;\n";
        let dir = "crates/aterm-effects/src/aos_momentum";
        let shipped = ["lane", "m4", "plan", "publish"];
        let tests = ["lane_tests", "m4_tests", "publish_tests", "rig"];
        let mut files: Vec<(String, &str)> = vec![(format!("{dir}.rs"), parent)];
        files.extend(
            shipped
                .iter()
                .chain(tests.iter())
                .map(|m| (format!("{dir}/{m}.rs"), planted)),
        );
        let files: Vec<(&str, &str)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let found = findings(&files);
        let expected: Vec<String> = shipped
            .iter()
            .flat_map(|m| (2..=5).map(move |l| format!("{dir}/{m}.rs:{l}")))
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn a_clock_read_in_a_same_crate_helper_the_momentum_step_calls_is_found() {
        let planted = "fn ink(now: Instant) -> u32 { light(now) }\n\
                       fn light(_: Instant) -> u32 { let t = Instant::now(); 0 }\n";
        let glow = "mod light_body;\nmod raster;\n#[cfg(test)]\nmod tests;\n\
                    fn clip() { let _ = std::time::SystemTime::now(); }\n";
        let tests_mod = "mod tint;\nfn t() { let _ = Instant::now(); }\n";
        let found = findings(&[
            ("crates/aterm-effects/src/cursor_glow.rs", glow),
            (
                "crates/aterm-effects/src/cursor_glow/light_body.rs",
                planted,
            ),
            ("crates/aterm-effects/src/cursor_glow/raster.rs", planted),
            ("crates/aterm-effects/src/effect_util.rs", planted),
            ("crates/aterm-effects/src/color_math.rs", planted),
            ("crates/aterm-effects/src/rainbow_kitty/ribbon.rs", planted),
            (
                "crates/aterm-effects/src/cursor_glow/tests/mod.rs",
                tests_mod,
            ),
            (
                "crates/aterm-effects/src/cursor_glow/tests/tint.rs",
                planted,
            ),
            ("crates/aterm-effects/src/rainbow_kitty/meteor.rs", planted),
            ("crates/aterm-effects/src/cursor_comet.rs", planted),
        ]);
        assert_eq!(
            found,
            [
                "crates/aterm-effects/src/color_math.rs:2",
                "crates/aterm-effects/src/cursor_glow.rs:5",
                "crates/aterm-effects/src/cursor_glow/light_body.rs:2",
                "crates/aterm-effects/src/cursor_glow/raster.rs:2",
                "crates/aterm-effects/src/effect_util.rs:2",
                "crates/aterm-effects/src/rainbow_kitty/ribbon.rs:2",
            ]
        );
    }

    #[test]
    fn a_finding_names_the_read_and_what_it_is() {
        let found = run(
            &AOS_STEP_CLOCK,
            &[(
                "crates/aterm-aos/src/replay.rs",
                "fn a() { let t = Instant::now(); }\nfn b() { aterm_uds::rand::fill(&mut x); }\n",
            )],
        );
        let details: Vec<&str> = found.iter().map(|f| f.detail.as_str()).collect();
        assert_eq!(
            details,
            [
                "`Instant::now`: the monotonic clock, in Aos step code: time and randomness enter \
                 only as recorded inputs (M0-14)",
                "`rand::`: a random draw through the `rand` module, in Aos step code: time and \
                 randomness enter only as recorded inputs (M0-14)",
            ]
        );
        assert!(found.iter().all(|f| f.rule == "aos-step-clock"));
    }

    #[test]
    fn an_import_that_hides_a_clock_or_randomness_is_found_where_it_binds() {
        let src = "use std::thread::sleep;\n\
                   use std::time::Instant as Mono;\n\
                   use std::thread::*;\n\
                   use aterm_time::unix_millis;\n\
                   use aterm_time::*;\n\
                   use rand::Rng;\n\
                   use aterm_uds::rand;\n\
                   use std::collections::hash_map::RandomState;\n\
                   use std::collections::HashMap as Map;\n\
                   use std::{\n    thread::{self, sleep_until},\n    time::Duration,\n};\n\
                   type Clock = std::time::SystemTime;\n\
                   use std::time::{Duration, Instant};\n\
                   use aterm_time::Instant;\n\
                   use std::collections::{BTreeMap, HashMap, VecDeque};\n\
                   use std::time::Instant as _;\n\
                   use core::fmt;\n";
        let found = run(&AOS_STEP_CLOCK, &[("crates/aterm-aos/src/log.rs", src)]);
        assert_eq!(
            spots(&found)
                .iter()
                .map(|s| s.trim_start_matches("crates/aterm-aos/src/log.rs:"))
                .collect::<Vec<_>>(),
            ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "14"],
            "{found:#?}"
        );
    }

    #[test]
    fn a_randomness_module_is_named_only_as_a_path() {
        assert!(random_path("let x = rand::random::<u8>();").is_some());
        assert!(random_path("aterm_uds::rand::fill(&mut b)?;").is_some());
        assert!(random_path("let r = rand :: thread_rng();").is_some());
        assert!(random_path("let r = self.rand;").is_none());
        assert!(random_path("let r = x.rand();").is_none());
        assert!(random_path("let rand = 3; let operand = rand + 1;").is_none());
        assert!(random_path("let brand::x = 1;").is_none());
    }
}
