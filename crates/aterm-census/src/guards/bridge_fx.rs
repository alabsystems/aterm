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
#[path = "bridge_fx_tests.rs"]
mod tests;
