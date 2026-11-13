// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::RunOut;

struct Tags {
    local: bool,
    on_origin: bool,
}

impl GitRunner for Tags {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        let (status, stdout) = match args.first().copied() {
            Some("rev-parse") => (i32::from(!self.local), String::new()),
            Some("ls-remote") if self.on_origin => {
                (0, format!("{:040x}\t{}\n", 1, args[args.len() - 1]))
            }
            Some("ls-remote") => (0, String::new()),
            _ => panic!("tag_free asks only rev-parse and ls-remote, not {args:?}"),
        };
        Ok(RunOut {
            status,
            stdout: stdout.into_bytes(),
            stderr: vec![],
        })
    }
}

#[test]
fn a_taken_tag_refuses_the_cut() {
    for (local, on_origin) in [(true, true), (true, false), (false, true)] {
        tag_gate(
            &Tags { local, on_origin },
            Some(&fixture_origin::FixtureOrigin(&Tags { local, on_origin })),
            "0.97.0",
            true,
        )
        .expect_err("a taken tag is refused");
    }
    let free = Tags {
        local: false,
        on_origin: false,
    };
    tag_gate(
        &free,
        Some(&fixture_origin::FixtureOrigin(&free)),
        "0.97.0",
        true,
    )
    .expect("a free tag passes");
}

#[test]
fn a_signing_rehearsal_neither_claims_nor_queries_tags() {
    struct NoTagQueries;

    impl GitRunner for NoTagQueries {
        fn git(&self, args: &[&str]) -> Result<RunOut> {
            panic!("a non-publishing rehearsal must not query tags: {args:?}");
        }
    }

    tag_gate(
        &NoTagQueries,
        Some(&fixture_origin::FixtureOrigin(&NoTagQueries)),
        "0.97.0",
        false,
    )
    .unwrap();
}
