// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Each answer's frame reads back as the same answer, so a broker and a client of this build
/// agree on every code; a code no broker of this build sends reads as unserved.
#[test]
fn every_answer_survives_its_frame() {
    for answer in [
        ScopeAnswer::Excluded(0),
        ScopeAnswer::Excluded(64),
        ScopeAnswer::Busy,
        ScopeAnswer::Unserved("no duty".into()),
        ScopeAnswer::Refused("not the console's".into()),
        ScopeAnswer::Unchanged("the receipt is full".into()),
        ScopeAnswer::Failed("the edit failed".into()),
    ] {
        let read = match answer.frame() {
            Frame::Scoped { added } => ScopeAnswer::Excluded(added),
            Frame::Error { code, words } => ScopeAnswer::of_error(code, words),
            other => panic!("{other:?}"),
        };
        assert_eq!(answer, read);
    }
    assert!(matches!(
        ScopeAnswer::of_error(libc::ENOENT, "gone".into()),
        ScopeAnswer::Unserved(words) if words.contains("code") && words.contains("gone")
    ));
}
