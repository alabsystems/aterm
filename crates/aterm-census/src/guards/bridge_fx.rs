// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Census guard ensuring the fabric bridge reads its clocks and parks through `Fx`.

use super::{Bound, Finding, Rule, Tree, has};

/// The bridge's own files: its loop and handlers, the mirror's half, and the
/// pipelined sends.
const BRIDGE: &[&str] = &[
    "crates/aterm-link/src/bridge.rs",
    "crates/aterm-link/src/bridge_mirror.rs",
    "crates/aterm-link/src/pipeline.rs",
];

/// Each clock read and park the door replaces, and what it is.
const CLOCKS: &[(&str, &str)] = &[
    ("Instant::now", "the monotonic clock"),
    ("SystemTime::now", "the wall clock"),
    (
        ".elapsed()",
        "the monotonic clock (`elapsed` reads `Instant::now`)",
    ),
    ("now_ms()", "the wall clock (`crate::now_ms`)"),
    ("unix_millis(", "the wall clock (`aterm_time::unix_millis`)"),
    (
        "thread::sleep(",
        "a real park (the door's is `self.fx.sleep`)",
    ),
    (
        "mailbox.take(",
        "the mailbox's real wait (the door's is `self.fx.take`)",
    ),
];

pub(super) const BRIDGE_FX: Rule = Rule {
    id: "bridge-fx",
    why: "aterm-link's deterministic simulation runs the real bridge behind its doors \
          (src/fx.rs): a clock read past `self.fx` is the real clock inside a simulated world, \
          so a seed stops replaying and a deadline the scheduler moved is judged by wall time; \
          read `self.fx.now()` / `self.fx.wall_ms()`, or take `now` from the caller, and measure \
          a span as `fx.now().saturating_duration_since(started)`; park through `self.fx.take` or \
          `self.fx.sleep`",
    scope: BRIDGE,
    check: bridge_fx,
};

fn bridge_fx(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let needles: Vec<&str> = CLOCKS.iter().map(|c| c.0).collect();
    for file in tree.corpus.under(BRIDGE) {
        if !file.mentions(&needles) {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some((_, what)) = CLOCKS
                .iter()
                .find(|(needle, _)| has(line.code, needle, Bound::Sub))
            {
                out.push(file.finding(
                    BRIDGE_FX.id,
                    line.no,
                    format!("reads {what} past the bridge's `Fx`"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
}
