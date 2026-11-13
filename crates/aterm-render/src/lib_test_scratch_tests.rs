// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// NEGATIVE CONTROL: multi-byte UTF-8 tags straddling byte 24 must truncate at a valid char
/// boundary without panicking and preserve the `.noindex` Spotlight-exclusion suffix.
#[test]
fn test_scratch_dir_truncates_utf8_tags_and_preserves_noindex_suffix() {
    let dir = crate::test_scratch_dir("aaaaaaaaaaaaaaaaaaaaaaaé-extra-tail");
    let name = dir
        .path()
        .file_name()
        .and_then(|os| os.to_str())
        .expect("scratch dir name is UTF-8");
    assert!(name.starts_with("aterm-render-aaaaaaaaaaaaaaaaaaaaaaa-"));
    assert!(name.ends_with(".noindex"));
}
