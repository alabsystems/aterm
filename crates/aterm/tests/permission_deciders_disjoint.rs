// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two `rm` permission deciders never both allow the same command.
//!
//! aterm carries two deciders for a Claude Code `PermissionRequest` on a
//! removal: aterm-link's hook (`aterm_link::permission::decide`, installed by
//! default by the primer's HookLane) and the aterm harness's
//! `aterm_agent::harness::rm_policy::evaluate`. Today their allow sets are
//! DISJOINT, and that is what lets both be installed without one ever
//! overriding the other's refusal:
//!
//! * link allows (`Verdict::Guarded`) only a line that needs a `$` in a
//!   removal target — it resolves the variable bound on the same line and
//!   rewrites it to `${NAME:?}`; a literal target is left to the human
//!   (`Verdict::Escalate`);
//! * rm_policy's rule 7 abstains on ANY `$` in an operand, and allows only
//!   literal targets strictly inside the prefix.
//!
//! A change that breaks this — rm_policy learning to resolve variables, or
//! link starting to allow literal targets — must MERGE the two deciders into
//! one policy. Do not delete or weaken this test to make such a change pass.

use std::path::Path;

use aterm_agent::harness::rm_policy::{self, HookEvent, RmDecision, RmPolicy};
use aterm_link::permission::{self, Verdict};

const CWD: &str = "/Users//nobody-aterm-test/proj";

fn link(cmd: &str) -> Verdict {
    permission::decide(permission::DECIDING_MODE, "Bash", cmd, CWD)
}

fn harness(cmd: &str) -> RmDecision {
    rm_policy::evaluate(
        cmd,
        Path::new(CWD),
        HookEvent::PermissionRequest,
        &RmPolicy::default(),
    )
    .decision
}

/// Lines that bind their removal variable on the same line.
const SAME_LINE_BINDINGS: &[&str] = &[
    "S=/tmp/x/build; rm -rf \"$S\"",
    "for d in a b; do rm -r \"$d\"; done",
    "for p in \"build dist\"; do set -- $p; rm -rf $1 $2; done",
];

#[test]
fn link_guards_what_rm_policy_abstains_on() {
    for cmd in SAME_LINE_BINDINGS {
        // Non-vacuity: link must actually allow, or the test proves nothing.
        let guarded = match link(cmd) {
            Verdict::Guarded { command, .. } => command,
            other => panic!("link must guard {cmd:?}, got {other:?}"),
        };
        assert_eq!(
            harness(cmd),
            RmDecision::Abstain,
            "rm_policy must abstain on the original {cmd:?}"
        );
        assert_eq!(
            harness(&guarded),
            RmDecision::Abstain,
            "rm_policy must abstain on the guarded rewrite {guarded:?} of {cmd:?}"
        );
    }
}

/// Literal targets strictly inside the session cwd.
const LITERAL_INSIDE_CWD: &[&str] = &["rm -rf build", "rm target/foo.o", "rm -r ./dist"];

#[test]
fn rm_policy_allows_what_link_escalates() {
    for cmd in LITERAL_INSIDE_CWD {
        // Non-vacuity: rm_policy must actually allow.
        assert_eq!(
            harness(cmd),
            RmDecision::Allow,
            "rm_policy must allow {cmd:?}"
        );
        match link(cmd) {
            Verdict::Escalate { .. } => {}
            other => panic!("link must escalate the literal {cmd:?}, got {other:?}"),
        }
    }
}
