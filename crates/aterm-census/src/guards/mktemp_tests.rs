// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::tests::{run, spots};
use super::*;

#[test]
fn a_scratch_directory_comes_from_a_checked_mktemp() {
    let script = "#!/bin/bash\n\
                  T=\"$(cd \"$(mktemp -d \"${TMPDIR:-/tmp}/x.XXXXXX\")\" && pwd -P)\"\n\
                  U=$(cd $(mktemp -d) && pwd)\n\
                  V=$(cd -P \"$(mktemp -d)\" && pwd)\n\
                  W=`cd \"$(mktemp -d)\" && pwd`\n\
                  # T=\"$(cd \"$(mktemp -d)\" && pwd -P)\" is the shape to avoid\n\
                  T=\"$(mktemp -d)\" || exit 1\ncd \"$T\" || exit 1\n";
    let rust = "const SCRIPT: &str = r#\"\nT=\"$(cd \"$(mktemp -d)\" && pwd -P)\"\n\"#;\n\
                // T=\"$(cd \"$(mktemp -d)\")\"\nfn f() { let _ = r#\"$(cd -- \"$(mktemp -d)\")\"#; }\n";
    let found = run(
        &CHECKED_MKTEMP,
        &[
            ("tools/test-x.sh", script),
            ("crates/aterm-verify/tests/t.rs", rust),
            ("crates/aterm-census/src/guards/mktemp.rs", rust),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-verify/tests/t.rs:2",
            "crates/aterm-verify/tests/t.rs:5",
            "tools/test-x.sh:2",
            "tools/test-x.sh:3",
            "tools/test-x.sh:4",
            "tools/test-x.sh:5",
        ]
    );
}
