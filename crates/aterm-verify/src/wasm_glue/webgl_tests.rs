// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The table is sorted and has no name twice, so the glue's lookup and
/// the import object it writes are one entry per import; and every body
/// is a function expression taking exactly its signature's arguments.
#[test]
fn each_import_is_listed_once_with_a_body_of_its_arity() {
    for pair in IMPORTS.windows(2) {
        assert!(
            pair[0].name < pair[1].name,
            "{} / {}",
            pair[0].name,
            pair[1].name
        );
    }
    for g in IMPORTS {
        let args: Vec<String> = (0..g.params.len()).map(|i| format!("arg{i}")).collect();
        assert!(
            g.body
                .starts_with(&format!("function({}) {{", args.join(", "))),
            "{}: {}",
            g.name,
            g.body.lines().next().unwrap_or_default()
        );
        assert!(g.results.len() <= 1, "{}", g.name);
    }
}
