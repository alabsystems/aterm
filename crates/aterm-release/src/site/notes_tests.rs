// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::site::releases::parse_releases;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    aterm_tempfile::manifest_dir()
        .join("tests/fixtures/site")
        .join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(fixture(name)).unwrap()
}

fn changelog_page() -> String {
    let mut sections = from_changelog(&read("CHANGELOG.md"));
    sections.truncate(LIMIT);
    render_page("alabsystems/aterm", &sections)
}

#[test]
fn the_changelog_page_is_the_reviewed_golden() {
    assert_eq!(changelog_page(), read("releases.golden.html"));
}

#[test]
fn the_release_bodies_page_is_the_reviewed_golden() {
    let releases = parse_releases(&read("releases-fat.json")).unwrap();
    let mut sections = from_releases(&releases);
    sections.truncate(LIMIT);
    assert_eq!(
        sections.iter().map(|s| s.tag.as_str()).collect::<Vec<_>>(),
        ["v0.63.0", "v0.61.0", "v0.60.0"],
        "stable v-tags only: no prerelease, no package index"
    );
    assert_eq!(
        render_page("alabsystems/aterm", &sections),
        read("releases-bodies.golden.html")
    );
}

#[test]
fn twelve_sections_newest_first_and_never_unreleased() {
    let sections = from_changelog(&read("CHANGELOG.md"));
    assert_eq!(sections.first().map(|s| s.tag.as_str()), Some("v0.64.0"));
    assert_eq!(
        sections.get(LIMIT - 1).map(|s| s.tag.as_str()),
        Some("v0.53.0")
    );
    assert!(
        sections
            .iter()
            .all(|s| !s.body.contains("must never appear"))
    );
    assert_eq!(
        sections.iter().find(|s| s.tag == "v0.62.0").unwrap().body,
        ""
    );
}

#[test]
fn the_markdown_subset_renders_and_everything_else_is_text() {
    let html = |md: &str| render_markdown(md);
    assert_eq!(
        html("raw <b>tag</b> & *emph*"),
        "<p>raw &lt;b&gt;tag&lt;/b&gt; &amp; *emph*</p>"
    );
    assert_eq!(
        html("**`k = \"v\"` draws** and `**not bold**`"),
        "<p><strong><code>k = &quot;v&quot;</code> draws</strong> and <code>**not bold**</code></p>"
    );
    assert_eq!(
        html("[the notes](https://example.invalid/a#b) & https://example.invalid/bare"),
        "<p><a href=\"https://example.invalid/a#b\">the notes</a> &amp; https://example.invalid/bare</p>"
    );
    assert_eq!(
        html("1. one\n2. two `c`"),
        "<ol><li>one</li><li>two <code>c</code></li></ol>"
    );
    assert_eq!(
        html("- a\n  continued\n  - nested\n- b"),
        "<ul><li>a continued<ul><li>nested</li></ul></li><li>b</li></ul>"
    );
    assert_eq!(html("> quoted"), "<blockquote><p>quoted</p></blockquote>");
    assert_eq!(
        html("```\n$ a <b>\n```"),
        "<pre><code>$ a &lt;b&gt;</code></pre>"
    );
    assert_eq!(html("### Added ##\n---"), "<h3>Added</h3>\n<hr />");
    assert_eq!(html("#### Deep"), "<h4>Deep</h4>");
}

#[test]
fn the_cutters_asset_legend_is_dropped_only_in_its_exact_shape() {
    let body =
        "**aterm** is the terminal. New here? What each file is:\n\n- `a`\n\n---\n\n### Fixed\n";
    assert_eq!(strip_asset_preamble(body), "### Fixed");
    let notes = "### Fixed\n\n---\n\nmore";
    assert_eq!(strip_asset_preamble(notes), notes);
}
