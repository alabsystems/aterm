// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

#[test]
fn the_bridge_reads_its_clocks_through_its_door() {
    let bridge = "use std::time::{Duration, Instant, SystemTime};\n\
                  fn a(&self) { let t = Instant::now(); }\n\
                  fn b(&self) { let w = SystemTime::now(); }\n\
                  fn c(&self, t: Instant) -> Duration { t.elapsed() }\n\
                  fn d(&self, t: Instant) -> Duration { self.fx.now().saturating_duration_since(t) }\n\
                  // Instant::now() is the clock this file must not read\n\
                  const SAID: &str = \"SystemTime::now\";\n\
                  fn e(&self) { let t = std::time::Instant::now(); }\n\
                  fn g(&self) -> u64 { crate::now_ms() }\n\
                  fn h(&self) -> u64 { aterm_time::unix_millis() }\n\
                  fn i(&self) { std::thread::sleep(POLL); }\n\
                  fn j(&self) { let _ = self.mailbox.take(wait); }\n\
                  fn k(&self, m: &Mirror, now: Instant) -> u64 { m.now_ms(now) }\n\
                  fn l(&self) { self.fx.sleep(POLL); self.fx.take(&self.mailbox, wait); }\n\
                  #[cfg(test)]\n\
                  mod tests {\n    fn f() { let t = std::time::Instant::now(); }\n}\n";
    let door = "fn now(&self) -> Instant { Instant::now() }\n";
    let found = run(
        &BRIDGE_FX,
        &[
            ("crates/aterm-link/src/bridge.rs", bridge),
            ("crates/aterm-link/src/bridge_mirror.rs", "fn x() {}\n"),
            (
                "crates/aterm-link/src/pipeline.rs",
                "fn y(t: Instant) { t.elapsed(); }\n",
            ),
            ("crates/aterm-link/src/fx.rs", door),
            ("crates/aterm-link/src/notify.rs", door),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-link/src/bridge.rs:2",
            "crates/aterm-link/src/bridge.rs:3",
            "crates/aterm-link/src/bridge.rs:4",
            "crates/aterm-link/src/bridge.rs:8",
            "crates/aterm-link/src/bridge.rs:9",
            "crates/aterm-link/src/bridge.rs:10",
            "crates/aterm-link/src/bridge.rs:11",
            "crates/aterm-link/src/bridge.rs:12",
            "crates/aterm-link/src/pipeline.rs:1",
        ]
    );
}
