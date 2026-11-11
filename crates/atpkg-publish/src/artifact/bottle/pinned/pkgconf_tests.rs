// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::empty_search;

#[test]
fn implicit_search_is_empty_only_when_every_field_is_present_once_and_empty() {
    let empty = "DefaultSearchPaths: \nSystemIncludePaths: (null)\nSystemLibraryPaths: \n";
    assert!(empty_search(empty));
    assert!(!empty_search(""));
    assert!(!empty_search(&empty.replace(
        "DefaultSearchPaths: ",
        "DefaultSearchPaths: /opt/homebrew/lib/pkgconfig"
    )));
    assert!(!empty_search(&empty.replace(
        "SystemIncludePaths: (null)",
        "SystemIncludePaths: /usr/include"
    )));
    assert!(!empty_search(&format!("{empty}DefaultSearchPaths: \n")));
}
