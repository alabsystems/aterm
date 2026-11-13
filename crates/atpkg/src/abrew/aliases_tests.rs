// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{each, parse_script, script_name};

#[test]
fn script_round_trip_matches_brew_shape() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let script = tmp.path().join("ll");
    std::fs::write(
        &script,
        "#! /bin/bash\n# alias: brew ll\n#:  * `ll` [args...]\n#:    `brew ll` is an alias for `brew list --versions`\nbrew list --versions $*\n\n",
    )
    .unwrap();
    assert_eq!(
        parse_script(&script),
        Some(("ll".to_string(), "list --versions".to_string()))
    );
    std::fs::write(
        tmp.path().join("bang"),
        "#! /bin/bash\n# alias: brew bang\necho hi $*\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("old~"),
        "#! /bin/bash\n# alias: brew old\necho old\n",
    )
    .unwrap();
    assert_eq!(
        each(tmp.path(), &[]),
        vec![
            ("bang".to_string(), "!echo hi".to_string()),
            ("ll".to_string(), "list --versions".to_string()),
        ]
    );
    assert_eq!(script_name("my-cmd@v2"), "my_cmd_v2");
}
