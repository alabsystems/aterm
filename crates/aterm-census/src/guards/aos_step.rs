// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aos-step-clock: the Aos step code reads no clock and draws no randomness
//! (requirement M0-14; docs/DESIGN-aos-hosted-execution-2026-10-21.md §2 and §4.3).
//!
//! An Aos task is replayed. The substrate (`crates/aterm-aos/src/`) and the task it hosts
//! (`aterm_effects::aos_momentum`, over the engine in `cursor_momentum` and its `snapshot` and
//! `admission` modules, and the aterm-effects helpers that engine's step calls: see
//! [`AOS_STEP`]) run the same step from a checkpoint over the recorded inputs, and must
//! land on the same state and observation digests. Time enters only as a recorded input (a
//! signed offset from the epoch the host declared at attach), and the task draws no
//! randomness. A clock read, a sleep or a random draw in that code is therefore an input no
//! log holds: the replay answers it from the real clock or a fresh seed, the same trace stops
//! reproducing its digests, and nothing else says so (the step still compiles and the live
//! path still works). aterm-aos's own `clippy.toml` bans the std clock methods by path;
//! aterm-effects has no such file, and no lint sees a clock imported under another name, a
//! sleep imported bare or a randomness crate named by its module.
//!
//! What it names, on the CODE view of shipped lines (so a comment, a string or a test-only
//! item naming one is no finding), in [`ClockShape`]s:
//! * the clocks: `Instant::now`, `SystemTime::now`, an `.elapsed()` (which reads
//!   `Instant::now` itself), and the wall readers `aterm_time::{since_epoch, unix_secs,
//!   unix_millis}` and `aterm_types::shell_types::current_time_ms`;
//! * the sleeps: `thread::sleep`, `sleep_until`, `sleep_ms`, `park_timeout`;
//! * the randomness: `getrandom` (the crate `crates/aterm-getrandom` builds), a `rand` module
//!   named as a path's head (`rand::random()`, `aterm_uds::rand::fill(..)`), `thread_rng`,
//!   `OsRng`, `fastrand`, and a per-process random hash key — `RandomState`,
//!   `aterm_hash::{KeyedHashMap, KeyedState}`, and std's `HashMap`/`HashSet` built by `new` or
//!   `with_capacity` (which hash under `RandomState`, so their iteration order differs between
//!   the live run and its replay).
//!
//! Each import is read as the names it binds ([`use_leaves`]), as supervise-fx and agh-fx read
//! one: a clock or sleep imported whole (`use std::thread::sleep;`, then `sleep(d)`), a head of
//! one renamed (`Instant as Mono`, `std::thread as t`, `HashMap as Map`), its module globbed in
//! (`std::thread::*`, `aterm_time::*`), a wall reader imported to be passed bare
//! (`use aterm_time::unix_millis;`), or any path through a randomness crate or module
//! (`use rand::Rng;`, `use aterm_uds::rand;`) is named at the import. So is a `type` alias of
//! one ([`type_alias`]). An import of a clock's type under its own name
//! (`use aterm_time::Instant;`) is no finding: the lines then name `Instant::now` by its path.
//!
//! Test code is not read: a file whose path names a test ([`test_path`]: `host_tests.rs`,
//! `test_fixtures.rs`) and a file its parent declares under a test `cfg`
//! ([`super::Corpus::test_only`]). A `#[cfg(any(test, debug_assertions))]` seam is read: it
//! ships in every debug build.
//!
//! LEXICAL, like every census rule, and scoped by file, not by call graph. It reads every
//! shipped line of the module trees in [`AOS_STEP`] (so it also forbids a clock in code of
//! those modules the step never reaches), and nothing outside them. So it does not see:
//! * a clock read inside a helper of another crate that the step calls: `aterm_render`
//!   (`premul_rgb`, `over_premul`, `ribbon_beam`), `aterm_time` (`Instant`'s arithmetic),
//!   `aterm_digest` (`Sha256`) and `astream_wire` (the frame and step codec); the engine is
//!   handed `now` by its host for that reason;
//! * a helper in another module of aterm-effects that the step comes to call after the list
//!   in [`AOS_STEP`] was derived (it is derived by hand from the step's imports and calls, not
//!   computed), until that module is added there;
//! * a macro that expands to a read, a qualified path (`<Instant>::now()`), a `HashMap` built
//!   by `default()` or `from_iter`, or a randomness crate reached under a name its manifest
//!   renamed.

use super::rules::{
    ClockShape, UseLeaf, first_hit, test_path, type_alias, use_leaves, use_statements,
};
use super::{Bound, Finding, Rule, Tree, hits};

/// The Aos step code, as module trees (each a file and the directory of its child modules):
/// aterm-aos's library source; in aterm-effects the momentum task and the engine it hosts; and
/// the same-crate modules that engine's shipped step calls into, derived from its imports and
/// calls (2026-10-22):
/// * `cursor_glow`: `Geom` and its effects-box clip (`cursor_glow.rs`); `light_floor_at`,
///   `light_alpha_keeping`, `light_ink_over_floor` and `push_ink_ellipse_shaped`
///   (`cursor_glow/light_body.rs`); `light_ink_bold` (`cursor_glow/raster.rs`);
/// * `effect_util`: `mix_rgb`, `push_fx_ramp`;
/// * `color_math`: `light_charge_in_hue` and the relative-luminance table it and the light
///   body read;
/// * `rainbow_kitty/ribbon`: `seen`, which `light_ink_over_floor` asks.
const AOS_STEP: &[&str] = &[
    "crates/aterm-aos/src/",
    "crates/aterm-effects/src/aos_momentum",
    "crates/aterm-effects/src/cursor_momentum",
    "crates/aterm-effects/src/cursor_glow",
    "crates/aterm-effects/src/effect_util",
    "crates/aterm-effects/src/color_math",
    "crates/aterm-effects/src/rainbow_kitty/ribbon",
];

/// Each clock read, sleep or random draw the step code may not make, its shape, and what it
/// is.
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

/// The modules and crates whose every item is randomness: a path through one, imported or
/// named as a path's head, is a draw the log does not hold.
const RANDOM_MODULES: &[&str] = &["rand", "getrandom", "aterm_getrandom", "fastrand"];

/// The module whose glob import binds the wall readers bare.
const WALL_READER_MODULE: &str = "aterm_time";

/// What a finding says after the needle and what it is.
const ENTERS_AS_INPUT: &str =
    "in Aos step code: time and randomness enter only as recorded inputs (M0-14)";

pub(super) const AOS_STEP_CLOCK: Rule = Rule {
    id: "aos-step-clock",
    why: "an Aos task is replayed from a checkpoint over its recorded inputs \
          (docs/DESIGN-aos-hosted-execution-2026-10-21.md, M0-14), so a clock read, a sleep or a \
          random draw in the step code (crates/aterm-aos/src, and aterm-effects' aos_momentum \
          and cursor_momentum with the helpers their step calls: cursor_glow, effect_util, \
          color_math and rainbow_kitty/ribbon) is an input no log holds and a replay answers \
          it from the real clock or a fresh seed: take the time from the recorded input (a \
          signed offset from the attach epoch) or from the caller's `now`, draw no randomness, \
          and key maps deterministically (a `BTreeMap`, never a `RandomState`-keyed map)",
    scope: AOS_STEP,
    check: aos_step_clock,
};

fn aos_step_clock(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_only: std::collections::BTreeSet<String> = AOS_STEP
        .iter()
        .flat_map(|root| tree.corpus.test_only(root))
        .collect();
    let mut needles: Vec<&str> = AOS_READS.iter().map(|r| r.0).collect();
    // Each segment too: an import that binds a clock under a name the line view cannot see
    // names a segment of it (`use std::thread::{self, sleep};` names `thread`).
    needles.extend(AOS_READS.iter().flat_map(|r| r.0.split("::")));
    needles.extend(RANDOM_MODULES);
    needles.extend(["type ", WALL_READER_MODULE]);
    for file in tree.corpus.under(AOS_STEP) {
        if test_path(&file.rel) || test_only.contains(&file.rel) || !file.mentions(&needles) {
            continue;
        }
        // Imports are read whole, as the names they bind; only their spans are blanked in the
        // view the lines are read on, so an import is named once, at its first line.
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

/// A randomness module named as a path's head on one line of code (`rand::random()`,
/// `aterm_uds::rand::fill(&mut b)`, `getrandom::getrandom(..)`), never a field or a method of
/// that name (`self.rand`, `x.rand()`), nor a local named `rand`.
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

/// The finding for one imported name (or `type` alias) that binds a clock read, a sleep or a
/// random draw where the lines cannot see it, if any. `as _` binds nothing to call, and a head
/// imported under its own name (`use std::time::Instant;`) keeps the path a line is read for.
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
            // The whole of it: a path read bare from here on (`sleep(d)`), a one-word name
            // that is itself the door (`RandomState`), or a wall reader passed as a fn item.
            true
        } else {
            // A head of it renamed: `Instant as Mono`, then `Mono::now()`.
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

    /// Each planted read in a shipped file of the step code, with what the guard says.
    fn findings(files: &[(&str, &str)]) -> Vec<String> {
        run(&AOS_STEP_CLOCK, files)
            .iter()
            .map(|f| format!("{}:{}", f.path, f.line))
            .collect()
    }

    /// The negative control of M0-14: a clock read, a sleep and a random draw planted in each
    /// file of the step code is found, line by line; the same text in a comment, a string, a
    /// test-only item, a test file or a module its parent declares test-only is not, and
    /// neither is a time the caller handed in.
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
            // Test code, by name and by its parent's declaration.
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
            // Outside the step code: the host reads the clock and hands `now` in.
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

    /// The aterm-effects helpers the momentum step calls are step code too: a clock read in a
    /// shipped line of each (the light body's ink solve and ellipse, the aurora's `Geom` and
    /// raster, the effects quads, the colour math, the ribbon's `seen`) is found. The aurora's
    /// own tests, which `cursor_glow.rs` declares under `#[cfg(test)]`, are not read, and
    /// neither is a module of the crate the step does not call.
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
            // The aurora's tests, test-only by the parent's declaration.
            (
                "crates/aterm-effects/src/cursor_glow/tests/mod.rs",
                tests_mod,
            ),
            (
                "crates/aterm-effects/src/cursor_glow/tests/tint.rs",
                planted,
            ),
            // Modules of the same crate the momentum step does not call.
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

    /// What each finding says names the read and what it is.
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

    /// A clock, a sleep or a random draw bound under a name the lines cannot see is named at
    /// the import; an import under the clock type's own name, `as _` and a plain module are
    /// not.
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

    /// The randomness modules are named as a path's head, never as a field, a method or a
    /// local of that name.
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
