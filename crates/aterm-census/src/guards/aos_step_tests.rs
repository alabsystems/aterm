// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
            "crates/aterm-effects/src/cursor_momentum.rs",
            "pub mod snapshot;\n#[cfg(test)]\nmod test_fixtures;\n",
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
    assert_eq!(details.len(), 2, "{details:?}");
    let monotonic = AOS_READS
        .iter()
        .find(|r| r.0 == "Instant::now")
        .expect("the Instant::now row")
        .2;
    assert!(
        details[0].starts_with("`Instant::now`:") && details[0].contains(monotonic),
        "{details:?}"
    );
    assert!(details[1].starts_with("`rand::`:"), "{details:?}");
    assert!(
        details.iter().all(|d| d.ends_with(ENTERS_AS_INPUT)),
        "{details:?}"
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
