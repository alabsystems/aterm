// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::test_scratch_dir;

/// NEGATIVE CONTROL: non-ASCII tags over the byte budget must truncate on a valid UTF-8 char
/// boundary without panicking and still produce a `.noindex` scratch directory.
#[test]
fn test_scratch_dir_truncates_utf8_tags_and_preserves_noindex_suffix() {
    let dir = test_scratch_dir("éééééééééé-handover");
    let name = dir
        .path()
        .file_name()
        .and_then(|os| os.to_str())
        .expect("UTF-8 scratch directory name");
    assert!(
        name.starts_with("acaf-éééééé-"),
        "unexpected prefix: {name}"
    );
    assert!(
        name.ends_with(".noindex"),
        "missing .noindex suffix: {name}"
    );
}
