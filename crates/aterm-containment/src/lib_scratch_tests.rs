// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

/// NEGATIVE CONTROL: even an oversized multi-byte UTF-8 tag truncates cleanly on a character
/// boundary and preserves the `.noindex` Spotlight-exclusion suffix.
#[test]
fn test_scratch_dir_truncates_utf8_tags_and_preserves_noindex_suffix() {
    let scratch = crate::test_scratch_dir("oversized-containment-tag-ééé-with-utf8-tail");
    let name = scratch
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .expect("utf-8 scratch dir name");
    assert!(name.starts_with("at-cnt-"), "unexpected prefix: {name}");
    assert!(name.ends_with(".noindex"), "missing .noindex: {name}");
}
