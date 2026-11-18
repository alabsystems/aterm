// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_face_is_named_by_its_path_and_nothing_beside_it_is_a_face() {
    let root = Path::new("/h/.aterm/files");
    for (path, name) in [
        ("/h/.aterm/files/ai/accounts.tsv", "ai/accounts"),
        ("/h/.aterm/files/lab/machines.tsv", "lab/machines"),
    ] {
        assert_eq!(name_in(root, Path::new(path)).as_deref(), Some(name));
        assert_eq!(name_of(Path::new(path)).as_deref(), Some(name));
    }
    // Any home's root, judged by the path alone.
    assert_eq!(
        name_of(Path::new("/private/var/t/.aterm/files/ai/links.tsv")).as_deref(),
        Some("ai/links")
    );
    // The negative controls: what a commit leaves beside a face, a face of no kind, a name out of
    // the grammar, another folder, a snapshot, and a path under no root.
    for other in [
        "/h/.aterm/files/ai/accounts.rejected.tsv",
        "/h/.aterm/files/ai/accounts.problems",
        "/h/.aterm/files/ai/accounts",
        "/h/.aterm/files/ai/accounts.json",
        "/h/.aterm/files/Ai/accounts.tsv",
        "/h/.aterm/files/accounts.tsv",
        "/h/.aterm/files/ai/x/accounts.tsv",
        "/h/.aterm/files/.ubd/state.noindex/ai/accounts.snap",
        "/h/.aterm/other/ai/accounts.tsv",
        "/h/elsewhere/ai/accounts.tsv",
    ] {
        assert_eq!(name_in(root, Path::new(other)), None, "{other}");
        assert_eq!(name_of(Path::new(other)), None, "{other}");
    }
}

#[test]
fn a_name_is_a_set_and_a_leaf_in_the_grammar() {
    for good in [
        "ai/accounts",
        "lab/machines",
        "a/b",
        "a1-b/c-9",
        "ai/abcdefghijklmnopqrst",
    ] {
        assert!(well_formed(good), "{good}");
    }
    for bad in [
        "",
        "ai",
        "ai/",
        "/accounts",
        "ai/accounts/x",
        "Ai/accounts",
        "1ai/accounts",
        "ai/-accounts",
        "ai/a.b",
        "ai/abcdefghijklmnopqrstu",
    ] {
        assert!(!well_formed(bad), "{bad:?}");
    }
}
