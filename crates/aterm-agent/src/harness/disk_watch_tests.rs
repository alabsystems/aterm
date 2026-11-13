// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn reported_session_directories_are_actual_or_unknown() {
    assert_eq!(
        reported_cwd("OK /tmp/a%20b"),
        Some(PathBuf::from("/tmp/a b"))
    );
    for bad in [
        "OK ",
        "OK relative",
        "ERR busy",
        "OK /tmp/%ZZ",
        "OK /tmp/%FF",
        "OK /tmp/%00",
    ] {
        assert_eq!(reported_cwd(bad), None, "{bad}");
    }
}
