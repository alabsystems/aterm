// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

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
