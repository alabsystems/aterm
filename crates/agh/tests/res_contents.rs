// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `contents()` over agh-fake: a file's bytes and a directory's entries.

// Every test routes through the canonical config's dev profile, a private token file (plan §1,
// rule 12).
#![cfg(unix)]

use agh::page::{Consistency, End};
use agh::{Github, Kind, Sub};
use agh_fake::{FakeGithub, FakeOrigin, Fault, Match, RepoSpec};

use crate::common;

/// The repository: public, the dev bot holding pull.
const APP: &str = "fixture-dev/app";

struct World {
    _root: aterm_tempfile::TempDir,
    fake: FakeGithub,
    gh: Github,
}

impl World {
    fn new(label: &str) -> World {
        let root = common::scratch(label);
        let fake = FakeGithub::start();
        common::canonical_tokens(root.path(), &fake, true, false);
        fake.add_repo(APP, RepoSpec::public().pull("fixture-dev-bot"));
        let router = common::router(&common::canonical_config(root.path()), root.path());
        let gh = common::github(router, &fake);
        World {
            _root: root,
            fake,
            gh,
        }
    }

    /// The api paths seen under `/repos/{APP}/contents`.
    fn contents_paths(&self) -> Vec<String> {
        let prefix = format!("/repos/{APP}/contents");
        self.fake
            .seen()
            .into_iter()
            .filter(|s| s.origin == FakeOrigin::Api && s.path_and_query.starts_with(&prefix))
            .map(|s| s.path_and_query)
            .collect()
    }
}

#[test]
fn raw_reads_a_file_s_bytes_and_refuses_a_directory() {
    let w = World::new("ct-raw");
    w.fake.put_file(APP, "main", "README.md", b"# app\n");
    w.fake.put_file(APP, "main", "docs/guide.md", b"guide");
    let contents = w.gh.contents(&common::slug(APP));
    assert_eq!(
        contents.raw(None, "README.md", 1024).expect("the file"),
        b"# app\n"
    );
    let dir = contents.raw(None, "docs", 1024).expect_err("a directory");
    assert_eq!(dir.kind, Kind::Invalid);
    assert_eq!(dir.repo.as_deref(), Some(APP));
    let over = contents.raw(None, "README.md", 3).expect_err("too large");
    assert_eq!(over.kind, Kind::Invalid);
    let missing = contents.raw(None, "nothing.md", 1024).expect_err("absent");
    assert_eq!(missing.kind, Kind::NotFound);
    w.fake.assert_clean();
}

#[test]
fn list_sorts_by_name_and_refuses_a_file() {
    let w = World::new("ct-list");
    for path in ["zeta.txt", "alpha.txt", "src/lib.rs", "Makefile"] {
        w.fake.put_file(APP, "main", path, path.as_bytes());
    }
    let contents = w.gh.contents(&common::slug(APP));
    let listing = contents.list(None, "", false).expect("the root");
    let names: Vec<&str> = listing.items.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Makefile", "alpha.txt", "src", "zeta.txt"]);
    let kinds: Vec<&str> = listing.items.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["file", "file", "dir", "file"]);
    assert_eq!(listing.end, End::Complete);
    assert_eq!(listing.pages, 1);
    assert_eq!(listing.consistency, Consistency::Snapshot);
    assert_eq!(w.contents_paths(), [format!("/repos/{APP}/contents")]);

    let file = contents.list(None, "alpha.txt", false).expect_err("a file");
    assert_eq!(file.kind, Kind::Invalid);
    w.fake.assert_clean();
}

#[test]
fn a_thousand_entries_is_the_directory_cap_unless_partial() {
    let w = World::new("ct-cap");
    for i in 0..1000 {
        w.fake.put_file(APP, "main", &format!("many/f{i:04}"), b"x");
    }
    let contents = w.gh.contents(&common::slug(APP));
    let capped = contents.list(None, "many", false).expect_err("capped");
    assert_eq!(capped.kind, Kind::Incomplete);
    assert_eq!(capped.sub, Sub::DirectoryCap);
    let partial = contents.list(None, "many", true).expect("partial");
    assert_eq!(partial.items.len(), 1000);
    assert_eq!(partial.end, End::Capped { pages: 1 });
    w.fake.assert_clean();
}

#[test]
fn the_ref_is_honoured_and_a_spaced_path_is_encoded() {
    let w = World::new("ct-ref");
    w.fake.put_file(APP, "main", "notes/a b.txt", b"on main");
    w.fake.put_file(APP, "v1", "notes/a b.txt", b"at v1");
    let contents = w.gh.contents(&common::slug(APP));
    assert_eq!(
        contents
            .raw(Some("v1"), "notes/a b.txt", 64)
            .expect("at v1"),
        b"at v1"
    );
    assert_eq!(
        contents.raw(None, "notes/a b.txt", 64).expect("on main"),
        b"on main"
    );
    assert_eq!(
        w.contents_paths(),
        [
            format!("/repos/{APP}/contents/notes/a%20b.txt?ref=v1"),
            format!("/repos/{APP}/contents/notes/a%20b.txt"),
        ]
    );
    let unknown = contents
        .raw(Some("no-such-ref"), "notes/a b.txt", 64)
        .expect_err("an unknown ref");
    assert_eq!(unknown.kind, Kind::NotFound);
    w.fake.assert_clean();
}

#[test]
fn a_file_labelled_raw_json_is_bytes_and_only_application_json_is_a_directory() {
    let w = World::new("ct-media");
    w.fake.put_file(APP, "main", "README", b"Hello World!\n");
    // GitHub labels a raw file's bytes with the vendor type Accept raw asked for.
    let labelled = |media: &str| Fault::Status {
        status: 200,
        body: "Hello World!\n".to_string(),
        headers: vec![("Content-Type".to_string(), media.to_string())],
    };
    let at = format!("/repos/{APP}/contents/README");
    w.fake.fault(
        Match::api(&at),
        labelled("application/vnd.github.raw+json; charset=utf-8"),
    );
    let contents = w.gh.contents(&common::slug(APP));
    assert_eq!(
        contents.raw(None, "README", 1024).expect("the file"),
        b"Hello World!\n"
    );
    w.fake
        .fault(Match::api(&at), labelled("Application/JSON; charset=utf-8"));
    let dir = contents.raw(None, "README", 1024).expect_err("a directory");
    assert_eq!(dir.kind, Kind::Invalid);
    w.fake.assert_clean();
}

#[test]
fn a_dot_segment_is_usage_before_any_request() {
    let w = World::new("ct-dots");
    w.fake.put_file(APP, "main", "README.md", b"# app\n");
    let contents = w.gh.contents(&common::slug(APP));
    for path in [
        "../../../fixture-dev/secret/contents/x",
        "docs/./guide.md",
        "..",
    ] {
        let raw = contents.raw(None, path, 1024).expect_err("a dot-segment");
        assert_eq!(raw.kind, Kind::Usage, "{path}: {}", raw.why);
        let list = contents.list(None, path, false).expect_err("a dot-segment");
        assert_eq!(list.kind, Kind::Usage, "{path}: {}", list.why);
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    w.fake.assert_clean();
}

#[test]
fn a_small_max_holds_the_file_and_never_a_failed_answer() {
    let w = World::new("ct-small-max");
    w.fake.put_file(APP, "main", "README.md", b"# app\n");
    let contents = w.gh.contents(&common::slug(APP));
    assert_eq!(
        contents.raw(None, "README.md", 6).expect("exactly the max"),
        b"# app\n"
    );
    let over = contents
        .raw(None, "README.md", 5)
        .expect_err("one byte over");
    assert_eq!(over.kind, Kind::Invalid);
    let missing = contents.raw(None, "nothing.md", 1).expect_err("absent");
    assert_eq!(missing.kind, Kind::NotFound);
    w.fake.assert_clean();
}

#[test]
fn a_submodule_is_invalid_to_cat_and_to_ls_and_names_its_type() {
    let w = World::new("ct-submodule");
    // GitHub answers a submodule's object under either Accept type.
    let object = Fault::Status {
        status: 200,
        body: r#"{"type":"submodule","name":"sub","path":"sub","sha":"s1","submodule_git_url":"https://github.com/fixture-dev/other.git"}"#.to_string(),
        headers: vec![(
            "Content-Type".to_string(),
            "application/json; charset=utf-8".to_string(),
        )],
    };
    let at = format!("/repos/{APP}/contents/sub");
    w.fake.fault_always(Match::api(&at), object);
    let contents = w.gh.contents(&common::slug(APP));
    let cat = contents.raw(None, "sub", 1024).expect_err("a submodule");
    assert_eq!(cat.kind, Kind::Invalid);
    assert!(cat.why.contains("submodule"), "{}", cat.why);
    let ls = contents.list(None, "sub", false).expect_err("a submodule");
    assert_eq!(ls.kind, Kind::Invalid);
    assert!(ls.why.contains("submodule"), "{}", ls.why);
    w.fake.assert_clean();
}
