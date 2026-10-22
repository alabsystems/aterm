// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Repository names, `-R` arguments, CONTENT references and the RFC 3986 encoders.

use std::collections::HashSet;

use agh::path::{file_path, has_dot_segment, query, ref_path, segment};
use agh::{ContentRef, Kind, RepoArg, Slug};

fn refused(result: agh::error::Result<Slug>, names: &str) {
    let error = result.expect_err("refused");
    assert_eq!(error.kind, Kind::Usage);
    assert!(
        error.why.contains(names),
        "{} should name {names}",
        error.why
    );
}

#[test]
fn slug_new_limits() {
    assert!(Slug::new("a", "b").is_ok());
    assert!(Slug::new(&"o".repeat(39), &"r".repeat(100)).is_ok());
    assert!(Slug::new("fixture-dev", "a.b_c-d").is_ok());
    refused(Slug::new("", "r"), "\"\"");
    refused(Slug::new(&"o".repeat(40), "r"), &"o".repeat(40));
    refused(Slug::new("-lead", "r"), "-lead");
    refused(Slug::new("under_score", "r"), "under_score");
    refused(Slug::new("o", ""), "\"\"");
    refused(Slug::new("o", &"r".repeat(101)), &"r".repeat(101));
    refused(Slug::new("o", "."), "\".\"");
    refused(Slug::new("o", ".."), "\"..\"");
    refused(Slug::new("o", "sp ace"), "sp ace");
}

#[test]
fn slug_parse() {
    let slug = Slug::parse("Fixture-Dev/Packs").expect("parses");
    assert_eq!((slug.owner(), slug.repo()), ("Fixture-Dev", "Packs"));
    refused(Slug::parse("a/b/c"), "a/b/c");
    refused(Slug::parse("nothing"), "nothing");
}

#[test]
fn slugs_compare_without_case_and_show_with_it() {
    let upper = Slug::parse("Fixture-Dev/Packs").expect("parses");
    let lower = Slug::parse("fixture-dev/packs").expect("parses");
    assert_eq!(upper, lower);
    assert_eq!(upper.cmp(&lower), std::cmp::Ordering::Equal);
    let set: HashSet<Slug> = [upper.clone(), lower].into_iter().collect();
    assert_eq!(set.len(), 1);
    assert_eq!(upper.to_string(), "Fixture-Dev/Packs");
    assert_eq!(upper.key(), "fixture-dev/packs");
    assert_eq!(upper.owner_key(), "fixture-dev");
}

#[test]
fn slugs_order_as_their_keys_do() {
    // The `/` between the halves orders as the key's own byte: `a-b/c` before `a/bc`, as
    // `-` (0x2d) is before `/` (0x2f), and a longer owner after its prefix.
    let texts = ["a/bc", "A-B/c", "ab/c", "a/b", "Zz/a", "a/B.c", "a-b/C"];
    let mut slugs: Vec<Slug> = texts
        .iter()
        .map(|t| Slug::parse(t).expect("parses"))
        .collect();
    slugs.sort();
    let mut keys: Vec<String> = texts.iter().map(|t| t.to_ascii_lowercase()).collect();
    keys.sort();
    let sorted: Vec<String> = slugs.iter().map(Slug::key).collect();
    assert_eq!(sorted, keys);
    let set: HashSet<Slug> = slugs.into_iter().collect();
    assert_eq!(set.len(), texts.len() - 1, "a-b/C and A-B/c are one");
}

#[test]
fn repo_arg_parse() {
    assert_eq!(
        RepoArg::parse("@channel").expect("a target"),
        RepoArg::Target("channel".into())
    );
    for bad in ["@Bad", "."] {
        let error = RepoArg::parse(bad).expect_err(bad);
        assert_eq!(error.kind, Kind::Usage, "{bad}");
    }
}

fn content(text: &str, with_repo: bool) -> ContentRef {
    ContentRef::parse(text, with_repo).expect(text)
}

#[test]
fn content_ref_with_a_repository() {
    let slug = |text: &str| Some(RepoArg::Literal(Slug::parse(text).expect("slug")));
    let c = content("o/r@v1:src/lib.rs", true);
    assert_eq!(c.repo, slug("o/r"));
    assert_eq!(c.reference.as_deref(), Some("v1"));
    assert_eq!(c.path.as_deref(), Some("src/lib.rs"));
    let c = content("o/r:", true);
    assert_eq!((c.reference, c.path.as_deref()), (None, Some("")));
    let c = content("o/r", true);
    assert_eq!((c.reference, c.path), (None, None));
    assert_eq!(content("o/r:/x", true).path.as_deref(), Some("x"));
    let error = ContentRef::parse("o/r@:x", true).expect_err("an empty ref");
    assert_eq!(error.kind, Kind::Usage);
    let c = content("@chan@main:README.md", true);
    assert_eq!(c.repo, Some(RepoArg::Target("chan".into())));
    assert_eq!(c.reference.as_deref(), Some("main"));
    assert_eq!(c.path.as_deref(), Some("README.md"));
}

#[test]
fn content_ref_without_a_repository() {
    let c = content("@v1:x", false);
    assert_eq!(
        (c.repo, c.reference.as_deref(), c.path.as_deref()),
        (None, Some("v1"), Some("x"))
    );
    let c = content(":x", false);
    assert_eq!((c.reference, c.path.as_deref()), (None, Some("x")));
    assert_eq!(content("", false), ContentRef::default());
}

#[test]
fn encoders() {
    assert_eq!(segment("a b+c/d"), "a%20b%2Bc%2Fd");
    assert_eq!(file_path("/a//b c/"), "a/b%20c");
    assert_eq!(ref_path("heads/feat x"), "heads/feat%20x");
    let pairs = vec![
        ("q".to_string(), "a b".to_string()),
        ("per_page".to_string(), "100".to_string()),
    ];
    assert_eq!(query(&pairs), "q=a%20b&per_page=100");
    assert_eq!(query(&[]), "");
    assert_eq!(segment("AZaz09-._~"), "AZaz09-._~");
    assert_eq!(segment("é"), "%C3%A9");
    assert_eq!(segment("\u{1}\u{7f}\u{ff}"), "%01%7F%C3%BF");
}

#[test]
fn content_ref_refuses_dot_segments() {
    for bad in [
        "o/r:../../../alabsystems/aterm/contents/README.md",
        "o/r:src/./lib.rs",
        "o/r:.",
        "o/r@..:x",
        "o/r@heads/../main:x",
    ] {
        let error = ContentRef::parse(bad, true).expect_err(bad);
        assert_eq!(error.kind, Kind::Usage, "{bad}");
        assert!(error.why.contains(bad), "{bad}: {}", error.why);
    }
    let error = ContentRef::parse(":a/../b", false).expect_err("a dot-segment without -R");
    assert_eq!(error.kind, Kind::Usage);
    let c = content("o/r@v1.2:.github/x..y/.z", true);
    assert_eq!(c.reference.as_deref(), Some("v1.2"));
    assert_eq!(c.path.as_deref(), Some(".github/x..y/.z"));
}

#[test]
fn has_dot_segment_reads_whole_parts() {
    for dotted in [".", "..", "a/..", "../a", "a/./b", "/./"] {
        assert!(has_dot_segment(dotted), "{dotted}");
    }
    for plain in ["", "a", ".a", "a..", "...", "a/.b/c.", "v1.2.3"] {
        assert!(!has_dot_segment(plain), "{plain}");
    }
}
