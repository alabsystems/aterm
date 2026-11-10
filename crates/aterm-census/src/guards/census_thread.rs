// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! census-thread: the census starts its threads through `memo::threads`, which carries a memo
//! unit's scope onto each.

use super::{Bound, Finding, Rule, Tree, hits};

const CENSUS: &[&str] = &["crates/aterm-census/src/"];

/// The wrapper, the one file that starts threads itself.
const WRAPPER: &str = "crates/aterm-census/src/memo.rs";

/// What starts a thread, or imports what does.
const STARTS: &[&str] = &[
    "thread::scope",
    "thread::spawn",
    "thread::Builder",
    "thread::{",
    "spawn_scoped",
];

pub(super) const CENSUS_THREAD: Rule = Rule {
    id: "census-thread",
    why: "a census memo unit's reads are its recorded questions only on the threads that carry \
          its scope, and a thread the census starts any other way carries none: its reads go \
          unrecorded, and a reused result could rest on a file that changed since. Start it \
          with `memo::threads` (`Threads::spawn`, `Threads::spawn_named`)",
    scope: CENSUS,
    check: census_thread,
};

fn census_thread(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let test_code = tree.corpus.test_code();
    for file in tree.corpus.under(CENSUS) {
        if file.rel == WRAPPER || test_code.holds(&file.rel) || !file.mentions(STARTS) {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some(start) = STARTS
                .iter()
                .find(|start| hits(line.code, start, Bound::Sub).next().is_some())
            {
                out.push(file.finding(
                    CENSUS_THREAD.id,
                    line.no,
                    format!("starts a thread by `{start}`, outside `memo::threads`"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guards::tests::{run, spots};

    #[test]
    fn a_census_thread_outside_the_wrapper_is_a_finding() {
        let src = "fn a() {\n    std::thread::scope(|s| { s.spawn(|| ()); });\n}\n\
                   fn b() { let _ = std::thread::Builder::new(); }\n\
                   use std::thread::{scope, spawn};\n\
                   fn c() { let n = std::thread::available_parallelism(); }\n\
                   fn d() { crate::memo::threads(|s| { s.spawn(|| ()); }); }\n\
                   // std::thread::spawn in prose\n\
                   #[cfg(test)]\nmod tests {\n    fn t() { std::thread::spawn(|| ()); }\n}\n";
        let found = run(
            &CENSUS_THREAD,
            &[
                ("crates/aterm-census/src/x.rs", src),
                ("crates/aterm-census/src/memo.rs", src),
                ("crates/aterm-gui/src/x.rs", src),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-census/src/x.rs:2",
                "crates/aterm-census/src/x.rs:4",
                "crates/aterm-census/src/x.rs:5",
            ],
            "CONTROL: the wrapper, another crate, `available_parallelism`, `memo::threads`, prose \
             and test code are no finding: {found:?}"
        );
    }
}
