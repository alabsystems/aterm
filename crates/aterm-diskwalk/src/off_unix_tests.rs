// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Off Unix nothing is walked or looked at: every entry point answers
//! [`Refusal::Unsupported`] for every path, a drive-letter path included, never
//! [`Refusal::Relative`] (which `sys` shows as "refused" where it means "unsupported").

use std::path::Path;
use std::time::{Duration, Instant};

use aterm_sysprobe::volumes::Table;

use crate::refuse::{Deny, Refusal};
use crate::walk::{Policy, RootSpec, Walk, diskwalk_size_full, entry_count_of, list_one_level};

/// The paths a Windows host names: a drive's root, a folder under the home, a verbatim and a UNC
/// spelling, and a relative one.
const PATHS: [&str; 5] = [
    r"C:\",
    r"C:\Users\a\src",
    r"\\?\C:\Users\a\src",
    r"\\server\share\x",
    "src",
];

fn policy() -> Policy {
    let mounts = Table {
        complete: true,
        ..Table::default()
    };
    Policy::new(Deny::for_home(Path::new(r"C:\Users\a"), &[], &[]), mounts)
}

#[test]
fn every_entry_point_answers_unsupported_for_every_path() {
    let policy = policy();
    for path in PATHS {
        let root = RootSpec::new(path);
        let at = Path::new(path);
        let answers = [
            ("Walk::begin", Walk::begin(&root, &policy).err()),
            (
                "list_one_level",
                list_one_level(&root, &policy).map(|_| ()).err(),
            ),
            (
                "entry_count_of",
                entry_count_of(at, &policy).map(|_| ()).err(),
            ),
            (
                "diskwalk_size_full",
                diskwalk_size_full(
                    &root,
                    &policy,
                    Duration::from_secs(60),
                    &mut || -> Instant { unreachable!("a refused root reads no clock") },
                )
                .map(|_| ())
                .err(),
            ),
            (
                "marker_at",
                crate::look::marker_at(at, ".git", &policy)
                    .map(|_| ())
                    .err(),
            ),
            (
                "markers_at",
                crate::look::markers_at(at, [".git", "Cargo.toml"], &policy)
                    .map(|_| ())
                    .err(),
            ),
            (
                "link_target",
                crate::look::link_target(at, "current", &policy)
                    .map(|_| ())
                    .err(),
            ),
        ];
        for (entry, answer) in answers {
            assert_eq!(answer, Some(Refusal::Unsupported), "{entry}({path:?})");
        }
    }
}
