// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A name's one assignment is read bare, quoted or exported; a comment, a line with no `=`
/// and another name are skipped. Negative controls: a name assigned twice, or nowhere.
#[test]
fn a_name_reads_its_one_assignment_and_nothing_runs() {
    let text = "# GH_TOKEN=commented\nOTHER=x\nnot an assignment\nexport GH_TOKEN = 'abc_1'\n";
    assert_eq!(assigned(text, "GH_TOKEN"), Ok("abc_1"));
    assert_eq!(assigned("GH_TOKEN=\"$(run)\"", "GH_TOKEN"), Ok("$(run)"));
    assert_eq!(assigned("GH_TOKEN=bare", "GH_TOKEN"), Ok("bare"));
    assert_eq!(
        assigned("GH_TOKEN=a\nGH_TOKEN=b\n", "GH_TOKEN"),
        Err(Unassigned::Repeated)
    );
    assert_eq!(assigned("OTHER=a\n", "GH_TOKEN"), Err(Unassigned::Missing));
    assert!(assignable("GH_TOKEN") && assignable("_x9"));
    for refused in ["", "9X", "GH-TOKEN", "GH TOKEN"] {
        assert!(!assignable(refused), "{refused}");
    }
}
