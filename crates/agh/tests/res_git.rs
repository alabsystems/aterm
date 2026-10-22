// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `git()` over agh-fake: commits, history, comparisons, refs and trees.

// Every test routes through the canonical config's dev profile, a private token file (plan §1,
// rule 12).
#![cfg(unix)]

use agh::github::CompareResult;
use agh::page::{End, ListOpts};
use agh::{Github, Kind};
use agh_fake::{CommitSpec, CompareSpec, FakeGithub, FakeOrigin, Fault, Match, RepoSpec};

use crate::common;

/// The repository with a history.
const APP: &str = "fixture-dev/app";
/// A repository with files and refs but no history.
const TREE: &str = "fixture-dev/tree";

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
        fake.add_repo(TREE, RepoSpec::public().pull("fixture-dev-bot"));
        let router = common::router(&common::canonical_config(root.path()), root.path());
        let gh = common::github(router, &fake);
        World {
            _root: root,
            fake,
            gh,
        }
    }

    /// Five commits on APP, `c1` oldest; `dev` at `c3`, the tag `v1` at `c2`.
    fn history(&self) {
        let commits = [
            ("c1", "2026-01-01T00:00:00Z", "fixture-dev-bot", "README.md"),
            (
                "c2",
                "2026-02-01T00:00:00Z",
                "fixture-ext-bot",
                "src/lib.rs",
            ),
            (
                "c3",
                "2026-03-01T00:00:00Z",
                "fixture-dev-bot",
                "src/main.rs",
            ),
            (
                "c4",
                "2026-04-01T00:00:00Z",
                "fixture-ext-bot",
                "docs/guide.md",
            ),
            (
                "c5",
                "2026-05-01T00:00:00Z",
                "fixture-dev-bot",
                "src/lib.rs",
            ),
        ];
        for (sha, date, login, path) in commits {
            self.fake.add_commit(
                APP,
                CommitSpec {
                    sha: sha.to_string(),
                    message: format!("{sha} subject\n\nbody"),
                    author_login: Some(login.to_string()),
                    author_name: format!("{login} name"),
                    date: date.to_string(),
                    paths: vec![path.to_string()],
                },
            );
        }
        self.fake.set_branch(APP, "dev", "c3");
        self.fake.set_tag(APP, "v1", "c2");
    }

    /// The api requests seen, as `METHOD path`.
    fn api_requests(&self) -> Vec<String> {
        self.fake
            .seen()
            .into_iter()
            .filter(|s| s.origin == FakeOrigin::Api)
            .map(|s| format!("{} {}", s.method, s.path_and_query))
            .collect()
    }
}

fn shas(items: &[agh::model::CommitObject]) -> Vec<&str> {
    items.iter().map(|c| c.sha.as_str()).collect()
}

#[test]
fn resolve_names_the_ref_s_commit_and_an_unknown_ref_is_not_found() {
    let w = World::new("git-resolve");
    w.history();
    let git = w.gh.git(&common::slug(APP));
    let dev = git.resolve("dev").expect("dev");
    assert_eq!(dev.sha, "c3");
    assert_eq!(dev.subject(), "c3 subject");
    assert_eq!(dev.date(), Some("2026-03-01T00:00:00Z"));
    assert_eq!(dev.author_name(), Some("fixture-dev-bot"));
    assert_eq!(git.resolve("v1").expect("v1").sha, "c2");
    let unknown = git.resolve("no-such-ref").expect_err("unknown");
    assert_eq!(unknown.kind, Kind::NotFound);
    assert!(unknown.why.contains("no-such-ref"), "{}", unknown.why);
    w.fake.assert_clean();
}

#[test]
fn log_is_newest_first_and_honours_its_limit_and_filters() {
    let w = World::new("git-log");
    w.history();
    let git = w.gh.git(&common::slug(APP));
    let all = git.log(None, None, None, None, 30).expect("the log");
    assert_eq!(shas(&all.items), ["c5", "c4", "c3", "c2", "c1"]);
    assert_eq!(all.end, End::Complete);

    w.fake.clear_seen();
    let two = git.log(None, None, None, None, 2).expect("two");
    assert_eq!(shas(&two.items), ["c5", "c4"]);
    assert_eq!(two.end, End::Limited { at: 2 });
    assert_eq!(
        w.api_requests(),
        [format!("GET /repos/{APP}/commits?per_page=2")]
    );

    let from_dev = git.log(Some("dev"), None, None, None, 30).expect("dev");
    assert_eq!(shas(&from_dev.items), ["c3", "c2", "c1"]);
    let src = git.log(None, Some("src"), None, None, 30).expect("src");
    assert_eq!(shas(&src.items), ["c5", "c3", "c2"]);
    let since = git
        .log(None, None, Some("2026-03-01T00:00:00Z"), None, 30)
        .expect("since");
    assert_eq!(shas(&since.items), ["c5", "c4", "c3"]);
    let by = git
        .log(None, None, None, Some("fixture-ext-bot"), 30)
        .expect("by");
    assert_eq!(shas(&by.items), ["c4", "c2"]);
    w.fake.assert_clean();
}

#[test]
fn compare_reads_the_fields_and_no_common_ancestor_is_no_merge_base() {
    let w = World::new("git-compare");
    w.history();
    w.fake.set_compare(
        APP,
        "c2",
        "c5",
        CompareSpec::Ok {
            status: "ahead".to_string(),
            ahead_by: 3,
            behind_by: 0,
            total_commits: 3,
            merge_base: "c2".to_string(),
        },
    );
    w.fake
        .set_compare(APP, "v1", "orphan", CompareSpec::NoCommonAncestor);
    let git = w.gh.git(&common::slug(APP));
    let CompareResult::Compared(compared) = git.compare("c2", "c5").expect("compared") else {
        panic!("not compared");
    };
    assert_eq!(compared.status, "ahead");
    assert_eq!((compared.ahead_by, compared.behind_by), (3, 0));
    assert_eq!(compared.total_commits, 3);
    assert_eq!(shas(&compared.commits), ["c3", "c4", "c5"]);
    assert_eq!(compared.files.len(), 3);
    assert!(!compared.capped_commits());
    assert!(!compared.capped_files());
    assert_eq!(
        compared
            .merge_base_commit
            .as_ref()
            .map(|base| base.sha.as_str()),
        Some("c2")
    );
    assert_eq!(
        git.compare("v1", "orphan").expect("unrelated"),
        CompareResult::NoMergeBase {
            base: "v1".into(),
            head: "orphan".into()
        }
    );
    let absent = git
        .compare("c1", "nowhere")
        .expect_err("no such comparison");
    assert_eq!(absent.kind, Kind::NotFound);
    w.fake.assert_clean();
}

#[test]
fn branches_and_tags_are_sorted_by_name_across_pages() {
    let w = World::new("git-refs");
    w.history();
    for name in ["release", "alpha", "zz-top", "beta"] {
        w.fake.set_branch(APP, name, "c1");
    }
    for name in ["v3", "v10", "a-tag", "v2"] {
        w.fake.set_tag(APP, name, "c4");
    }
    let git = w.gh.git(&common::slug(APP));
    let opts = ListOpts::STANDARD.with_per_page(2);
    let branches = git.branches(&opts).expect("branches");
    let names: Vec<&str> = branches.items.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta", "dev", "release", "zz-top"]);
    assert_eq!(branches.pages, 3);
    let tags = git.tags(&opts).expect("tags");
    let names: Vec<&str> = tags.items.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["a-tag", "v1", "v10", "v2", "v3"]);
    assert_eq!(tags.pages, 3);
    let limited = git.tags(&opts.with_limit(Some(2))).expect("two tags");
    let names: Vec<&str> = limited.items.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["a-tag", "v1"]);
    assert_eq!(limited.end, End::Limited { at: 2 });
    w.fake.assert_clean();
}

#[test]
fn tree_reads_by_branch_tag_and_sha_and_the_default_costs_one_request() {
    let w = World::new("git-tree");
    w.fake.set_branch(TREE, "main", "s1");
    w.fake.set_tag(TREE, "v1", "s1");
    w.fake.put_file(TREE, "main", "README.md", b"readme");
    w.fake.put_file(TREE, "main", "src/lib.rs", b"lib");
    let git = w.gh.git(&common::slug(TREE));
    let paths = |tree: &agh::model::TreeObject| {
        tree.tree
            .iter()
            .map(|e| format!("{} {}", e.kind, e.path))
            .collect::<Vec<_>>()
    };
    let recursive = ["blob README.md", "tree src", "blob src/lib.rs"];
    for reference in ["main", "v1", "s1"] {
        let tree = git.tree(Some(reference), true).expect("the tree");
        assert_eq!(paths(&tree), recursive, "{reference}");
        assert!(!tree.truncated);
        assert!(tree.tree.iter().all(|e| e.raw.get("path").is_some()));
    }
    let top = git.tree(Some("main"), false).expect("the top");
    assert_eq!(paths(&top), ["blob README.md", "tree src"]);

    w.fake.clear_seen();
    let default = git.tree(None, true).expect("the default branch's tree");
    assert_eq!(paths(&default), recursive);
    assert_eq!(
        w.api_requests(),
        [
            format!("GET /repos/{TREE}"),
            format!("GET /repos/{TREE}/git/trees/main?recursive=1"),
        ]
    );

    w.fake.set_tree_truncated(TREE, true);
    assert!(git.tree(Some("main"), true).expect("truncated").truncated);
    let unknown = git.tree(Some("nothing"), true).expect_err("unknown");
    assert_eq!(unknown.kind, Kind::NotFound);
    w.fake.assert_clean();
}

#[test]
fn a_limit_cut_of_a_capped_walk_stays_capped() {
    let w = World::new("git-cap-limit");
    w.history();
    for name in ["release", "alpha", "zz-top", "beta"] {
        w.fake.set_branch(APP, name, "c1");
    }
    let git = w.gh.git(&common::slug(APP));
    let opts = ListOpts::STANDARD
        .with_per_page(2)
        .with_max_pages(1)
        .with_partial(true)
        .with_limit(Some(1));
    let branches = git.branches(&opts).expect("a partial listing");
    assert_eq!(branches.items.len(), 1);
    assert_eq!(branches.end, End::Capped { pages: 1 });
    w.fake.assert_clean();
}

#[test]
fn a_tree_ref_with_a_dot_segment_is_usage_before_any_request() {
    let w = World::new("git-tree-dots");
    w.fake.set_branch(TREE, "main", "s1");
    let git = w.gh.git(&common::slug(TREE));
    for reference in [
        "..",
        "main/../../../fixture-dev/app/git/trees/main",
        "./main",
    ] {
        let error = git.tree(Some(reference), true).expect_err("a dot-segment");
        assert_eq!(error.kind, Kind::Usage, "{reference}: {}", error.why);
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    w.fake.assert_clean();
}

#[test]
fn a_compare_side_with_a_dot_segment_is_usage_before_any_request() {
    let w = World::new("git-compare-dots");
    let git = w.gh.git(&common::slug(APP));
    for (base, head) in [
        ("x/../../../../user", "main"),
        ("main", ".."),
        ("./main", "c5"),
    ] {
        let error = git.compare(base, head).expect_err("a dot-segment");
        assert_eq!(error.kind, Kind::Usage, "{base}...{head}: {}", error.why);
        assert_eq!(error.repo.as_deref(), Some(APP));
    }
    assert!(w.fake.seen().is_empty(), "{:?}", w.fake.seen());
    w.fake.assert_clean();
}

#[test]
fn a_commit_whose_author_is_an_empty_object_still_reads() {
    // GitHub types a commit's `author` and `committer` as `simple-user` or `{}`.
    let w = World::new("git-empty-author");
    let commit = r#"{"sha":"e1","commit":{"message":"e1 subject","author":{"name":"Someone","date":"2026-06-01T00:00:00Z"}},"author":{},"committer":{},"html_url":""}"#;
    let answer = |body: String| Fault::Status {
        status: 200,
        body,
        headers: Vec::new(),
    };
    w.fake.fault_always(
        Match::api(&format!("/repos/{APP}/commits")),
        answer(format!("[{commit}]")),
    );
    w.fake.fault_always(
        Match::api(&format!("/repos/{APP}/compare/")),
        answer(format!(
            r#"{{"status":"ahead","ahead_by":1,"behind_by":0,"total_commits":1,"commits":[{commit}],"files":[]}}"#
        )),
    );
    let git = w.gh.git(&common::slug(APP));

    let resolved = git.resolve("main").expect("rev");
    assert_eq!(resolved.sha, "e1");
    assert_eq!(resolved.author_name(), Some("Someone"));

    let log = git.log(None, None, None, None, 30).expect("log");
    assert_eq!(shas(&log.items), ["e1"]);

    let CompareResult::Compared(compared) = git.compare("c1", "e1").expect("compare") else {
        panic!("not compared");
    };
    assert_eq!(shas(&compared.commits), ["e1"]);
    assert_eq!(compared.commits[0].author_name(), Some("Someone"));
    w.fake.assert_clean();
}
