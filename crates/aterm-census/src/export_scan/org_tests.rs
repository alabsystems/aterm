// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

const ORG: &str = "privorg";

fn rewrite(rel: &str, text: &str) -> String {
    let names = ["ay".to_string(), "aterm".to_string(), "aterm-x".to_string()];
    let bytes = text.as_bytes();
    Rewrite::new(ORG, &names)
        .file(rel, bytes, bytes.len() as u64)
        .map_or_else(
            || text.to_string(),
            |b| String::from_utf8(b).expect("utf-8"),
        )
}

#[test]
fn it_rewrites_a_registry_repo_in_any_case_and_keeps_the_names_case() {
    assert_eq!(
        rewrite("a.md", "see github.com/PrivOrg/Aterm and privorg/ay.\r\n"),
        "see github.com/alabsystems/Aterm and alabsystems/ay.\r\n"
    );
    assert_eq!(rewrite("a.md", "privorg/aterm-x"), "alabsystems/aterm-x");
    assert_eq!(rewrite("a.md", "${2:-privorg/ay}"), "${2:-alabsystems/ay}");
}

#[test]
fn it_leaves_what_the_engines_pattern_does_not_match() {
    for text in [
        "gh-privorg/ay",
        "xprivorg/ay",
        "privorg/aterm-y",
        "privorg/ayx",
        "privorg/other",
    ] {
        assert_eq!(rewrite("a.md", text), text);
    }
}

#[test]
fn it_leaves_pins_lockfiles_and_text_that_is_not_utf8() {
    let pin = "dep = { git = \"https://github.com/privorg/ay\" }\n# privorg/ay\n";
    assert_eq!(
        rewrite("Cargo.toml", pin),
        "dep = { git = \"https://github.com/privorg/ay\" }\n# alabsystems/ay\n"
    );
    assert_eq!(rewrite("Cargo.lock", "privorg/ay"), "privorg/ay");
    assert_eq!(rewrite("package-lock.json", "privorg/ay"), "privorg/ay");
    assert_eq!(rewrite("a.md", "privorg/ay"), "alabsystems/ay");
    let names = ["ay".to_string()];
    let latin1 = b"privorg/ay \xff";
    assert_eq!(Rewrite::new(ORG, &names).file("a.md", latin1, 12), None);
    assert_eq!(
        Rewrite::new(ORG, &names).file("a.md", b"privorg/ay", BIG + 1),
        None
    );
}

#[test]
fn lines_split_as_pythons_splitlines() {
    assert_eq!(
        split_lines("a\r\nb\rc\u{2028}d\u{b}e"),
        ["a\r\n", "b\r", "c\u{2028}", "d\u{b}", "e"]
    );
    assert_eq!(suffix(".lock"), "");
    assert_eq!(suffix("x.lock"), ".lock");
}
