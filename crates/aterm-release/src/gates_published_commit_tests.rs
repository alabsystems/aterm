// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::testing::{self, Git};

fn changelog_gate_at(
    git: &dyn GitRunner,
    commit: &str,
    version: &str,
) -> Result<(String, changelog::GateSummary)> {
    let catalog = git_ok(
        git,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/tags",
        ],
    )?
    .stdout_utf8();
    super::changelog_gate_at(git, commit, version, &catalog)
}

struct Repo {
    root: aterm_tempfile::TempDir,
}

impl Repo {
    fn new(label: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-release-published-{label}-"))
            .tempdir()
            .unwrap();
        let repo = Self { root };
        testing::init(
            &repo.work(),
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        repo
    }
    fn work(&self) -> PathBuf {
        self.root.path().join("work")
    }
    fn run(&self, args: &[&str]) -> String {
        let out = testing::git()
            .arg("-C")
            .arg(self.work())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    fn commit(&self, subject: &str, files: &[(&str, &str)]) -> String {
        for (path, body) in files {
            let full = self.work().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, body).unwrap();
        }
        if !files.is_empty() {
            let mut add = vec!["add", "--"];
            add.extend(files.iter().map(|(path, _)| *path));
            self.run(&add);
        }
        self.run(&["commit", "-q", "--allow-empty", "-m", subject]);
        self.run(&["rev-parse", "HEAD"])
    }
    fn publish_origin(&self) {
        let origin = self.root.path().join("origin.git");
        let out = testing::git()
            .args(["clone", "-q", "--bare"])
            .arg(self.work())
            .arg(&origin)
            .output()
            .unwrap();
        assert!(out.status.success());
        testing::config(&origin, &[]);
        self.run(&["remote", "add", "origin", origin.to_str().unwrap()]);
        self.run(&["fetch", "-q", "origin"]);
    }
    fn push(&self) {
        self.run(&["push", "-q", "origin", "main"]);
    }
    fn git(&self) -> Git {
        Git::new(self.work())
    }
}

fn source(commit: &str) -> PublishedSource {
    PublishedSource {
        commit: commit.to_string(),
        verified_at: "2026-09-22T23:37:28+00:00".to_string(),
    }
}

#[test]
fn release_notes_harness_predecessor_is_the_public_object() {
    let repo = Repo::new("harness-note-authority");
    let published = "## [Unreleased]\n\n## [0.194.0] - date\n\n- `alab claude harness 0.31` records the changes.\n";
    repo.commit(
        "public release",
        &[
            ("CHANGELOG.md", published),
            ("RELEASES.ledger", "1794311832 0.194.0\n"),
            (
                "crates/aterm-phase/src/phase.rs",
                &crate::harness_versions::version_line((0, 31)),
            ),
        ],
    );
    repo.run(&["tag", "v0.194.0"]);
    repo.publish_origin();
    for (base, announced, older, expected) in [
        ("0.193.0", "0.32", "0.193.0", Some("anchor")),
        ("0.194.0", "0.31", "0.194.0", Some("require")),
        ("0.194.0", "0.32", "0.194.0", None),
    ] {
        // The candidate's stale copied history claims only 0.30. The
        // immutable public object, not that copy, decides both rules.
        // A recut ships its rolled section as it stands, so its announcement is judged.
        let current = format!(
            "## [Unreleased]\n\n## [0.195.0] - date\n\n- new fix\n\n- **Harness versions since v{base}: `alab claude harness {announced}`.**\n\n## [{older}] - date\n\n- `alab claude harness 0.30` was announced.\n"
        );
        assert!(changelog::gate_section(&current, "0.195.0").is_ok());
        let commit = repo.commit(
            "candidate notes",
            &[
                ("CHANGELOG.md", &current),
                (
                    "crates/aterm-phase/src/phase.rs",
                    &crate::harness_versions::version_line((0, 32)),
                ),
            ],
        );
        let verdict = harness_versions_gate_at(
            &repo.git(),
            &fixture_origin::FixtureOrigin(&repo.git()),
            &commit,
            "0.195.0",
        );
        match expected {
            Some(why) => assert!(verdict.unwrap_err().to_string().contains(why)),
            None => {
                verdict.unwrap();
            }
        }
    }
}

/// A SHIPPED ENTRY A MERGE BRINGS BACK UNDER `[Unreleased]` IS REFUSED BY ITS RELEASE'S TAG,
/// whatever main's own copy of the section says.
/// NEGATIVE CONTROL: before the tag holds it, the same window passes.
#[test]
fn a_shipped_entry_merged_back_under_unreleased_is_refused_by_its_tag() {
    let repo = Repo::new("note-merged-back");
    let bare = "## [Unreleased]\n\n## [0.1.0] - date\n\n- older change\n";
    let entry = "- **Sessions keep their model.** A restart keeps the launch's `--model`.\n";
    let shipped = bare.replace(
        "## [Unreleased]\n\n",
        &format!("## [Unreleased]\n\n## [0.2.0] - date\n\n{entry}\n"),
    );
    let restored = shipped.replace(
        "## [Unreleased]\n\n",
        &format!("## [Unreleased]\n\n{entry}\n"),
    );
    repo.commit("the note ships in 0.2.0", &[("CHANGELOG.md", &shipped)]);
    let tagged = repo.run(&["rev-parse", "HEAD"]);
    repo.commit(
        "a merge brings the shipped note back",
        &[("CHANGELOG.md", &restored)],
    );
    changelog_gate_at(&repo.git(), "HEAD", "0.3.0").unwrap();
    repo.run(&["tag", "v0.2.0", &tagged]);
    let err = changelog_gate_at(&repo.git(), "HEAD", "0.3.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("shipped by v0.2.0"), "{err}");
}

/// THE WINDOW'S STRICT READ REFUSES EVERY ENTRY FILE NO RELEASE SHIPS AS WRITTEN, by its own
/// message: an executable one, a symlink, one over the bound, bytes that are not UTF-8, a
/// root directory that is another case of `changes`, and a file named `changes`. A recut
/// ships its section as it stands and deletes no file, so it reads none of them: a stray
/// file of the next window never blocks the recovery release.
/// NEGATIVE CONTROL: a well-formed file passes the fresh window.
#[cfg(unix)]
#[test]
fn the_window_refuses_odd_entry_files_and_a_recut_reads_none() {
    let fresh = "## [Unreleased]\n\n## [0.1.0] - date\n\n- older change\n";
    let recut = "## [Unreleased]\n\n## [0.2.0] - date\n\n- Shipped.\n\n## [0.1.0] - date\n\n\
                 - older change\n";
    let big = format!("- {}\n", "x".repeat(64 << 10));
    type Lay = fn(&Repo, &str);
    let cases: [(&str, Lay, &str); 6] = [
        (
            "executable",
            |repo, body| {
                repo.commit("x", &[("changes/fixed/x.md", body)]);
                repo.run(&["update-index", "--chmod=+x", "changes/fixed/x.md"]);
                repo.run(&["commit", "-q", "-m", "exec"]);
            },
            "changes/fixed/x.md: is not a regular, non-executable file (git mode 100755",
        ),
        (
            "symlink",
            |repo, _| {
                let at = repo.work().join("changes/fixed");
                fs::create_dir_all(&at).unwrap();
                std::os::unix::fs::symlink("../../elsewhere.md", at.join("x.md")).unwrap();
                repo.run(&["add", "-A"]);
                repo.run(&["commit", "-q", "-m", "link"]);
            },
            "changes/fixed/x.md: is not a regular, non-executable file (git mode 120000",
        ),
        (
            "oversized",
            |repo, _| {
                let big = format!("- {}\n", "x".repeat(64 << 10));
                repo.commit("big", &[("changes/fixed/x.md", &big)]);
            },
            "changes/fixed/x.md: is 65539 bytes, over the 65536",
        ),
        (
            "not-utf8",
            |repo, _| {
                let at = repo.work().join("changes/fixed");
                fs::create_dir_all(&at).unwrap();
                fs::write(at.join("x.md"), b"- \xff\n").unwrap();
                repo.run(&["add", "-A"]);
                repo.run(&["commit", "-q", "-m", "bytes"]);
            },
            "changes/fixed/x.md: is not readable UTF-8 text",
        ),
        (
            "case",
            |repo, body| {
                repo.commit("case", &[("Changes/fixed/x.md", body)]);
            },
            "Changes: changelog entries live in a directory spelled exactly changes/",
        ),
        (
            "blob",
            |repo, body| {
                repo.commit("blob", &[("changes", body)]);
            },
            "changes: changelog entries live in a directory spelled exactly changes/",
        ),
    ];
    assert_eq!(big.len(), 65539);
    for (label, lay, expected) in cases {
        for (changelog, version, refused) in [(fresh, "0.2.0", true), (recut, "0.2.0", false)] {
            let repo = Repo::new(&format!("odd-{label}"));
            repo.commit("notes", &[("CHANGELOG.md", changelog)]);
            lay(&repo, "- **X.** Fine.\n");
            let verdict = changelog_gate_at(&repo.git(), "HEAD", version);
            if refused {
                let err = verdict.unwrap_err().to_string();
                assert!(err.contains(expected), "{label}: {err}");
            } else {
                verdict.unwrap_or_else(|e| panic!("{label}: a recut reads no file: {e}"));
            }
        }
    }
    let repo = Repo::new("odd-control");
    repo.commit(
        "notes",
        &[
            ("CHANGELOG.md", fresh),
            ("changes/fixed/x.md", "- **X.** Fine.\n"),
        ],
    );
    changelog_gate_at(&repo.git(), "HEAD", "0.2.0").unwrap();
}

/// Real 15b0ad50d50 retains the aCaffeine note in Unreleased and under
/// 0.192.0, but that release lacks its change 341231ea9d2. A surviving
/// misplaced copy supplies no shipment evidence.
#[test]
fn shipment_uses_the_tag_even_when_a_merge_changes_older_sections() {
    let repo = Repo::new("note-shipment-evidence");
    let released = "## [Unreleased]\n\n## [0.1.0] - date\n\n- Shipped fix.\n";
    repo.commit("public source", &[("CHANGELOG.md", released)]);
    repo.run(&["tag", "-a", "v0.1.0", "-m", "public release"]);
    repo.run(&["branch", "peer"]);
    let unshipped = "- **aCaffeine resumes after the lid opens.** Wait for the wake to settle.";
    let noted = released.replace(
        "## [Unreleased]\n",
        &format!("## [Unreleased]\n\n{unshipped}\n"),
    );
    repo.commit("wake fix on main", &[("CHANGELOG.md", &noted)]);
    repo.run(&["checkout", "-q", "peer"]);
    repo.commit("peer work", &[("peer", "work")]);
    repo.run(&["merge", "--no-ff", "--no-commit", "-s", "ours", "main"]);
    let copied = noted.replace("- Shipped fix.", &format!("- Shipped fix.\n\n{unshipped}"));
    repo.commit(
        "merge retains a misplaced older copy",
        &[("CHANGELOG.md", &copied)],
    );
    changelog_gate_at(&repo.git(), "HEAD", "0.2.0").unwrap();
    changelog::roll(&copied, "0.2.0", "date").unwrap();

    // NEGATIVE CONTROL: rewriting the candidate's historical copy cannot
    // conceal the original shipped wording still present in Unreleased.
    let concealed = released
        .replace("## [Unreleased]\n", "## [Unreleased]\n\n- Shipped fix.\n")
        .replacen(
            "## [0.1.0] - date\n\n- Shipped fix.",
            "## [0.1.0] - date\n\n- Corrected old description.",
            1,
        );
    repo.commit(
        "rewrite released copy but keep stale candidate",
        &[("CHANGELOG.md", &concealed)],
    );
    // A local tag from an abandoned attempt is not a public shipment.
    super::changelog_gate_at(&repo.git(), "HEAD", "0.2.0", "").unwrap();
    let error = changelog_gate_at(&repo.git(), "HEAD", "0.2.0")
        .unwrap_err()
        .to_string();
    assert!(error.contains("shipped by v0.1.0"), "{error}");
    assert!(
        error.contains("remove this already-shipped copy"),
        "{error}"
    );

    // A recut excludes the next release's Unreleased from its candidate.
    let recut = concealed.replace(
        "## [0.1.0]",
        "## [0.2.0] - date\n\n- Fresh change.\n\n## [0.1.0]",
    );
    repo.commit("recut with later notes", &[("CHANGELOG.md", &recut)]);
    changelog_gate_at(&repo.git(), "HEAD", "0.2.0").unwrap();
}

/// 36306dbeb51 / 028db7a01fe and 0e8a5cabde9 / 6c1b1914da6:
/// a public cut froze the old wording while main corrected the next window.
/// Landing the immutable published body is not a stale merge into that release.
#[test]
fn a_published_body_can_land_after_main_corrects_the_next_window() {
    let repo = Repo::new("published-body-authority");
    let original = "## [Unreleased]\n\n- Original claim.\n\n## [0.1.0] - date\n\n- Older change.\n";
    repo.commit(
        "cut starts before next-window correction",
        &[("CHANGELOG.md", original)],
    );
    repo.run(&["branch", "cut"]);
    let corrected = original.replacen("Original claim.", "Corrected claim.", 1);
    repo.commit(
        "main corrects the next window",
        &[("CHANGELOG.md", &corrected)],
    );
    repo.run(&["checkout", "-q", "cut"]);
    let published = original.replacen(
        "## [Unreleased]\n\n",
        "## [Unreleased]\n\n## [0.2.0] - date\n\n",
        1,
    );
    repo.commit("publish the frozen source", &[("CHANGELOG.md", &published)]);
    repo.run(&["tag", "v0.2.0"]);
    repo.run(&["checkout", "-q", "main"]);
    repo.run(&["merge", "--no-ff", "--no-commit", "-s", "ours", "cut"]);
    let landed = corrected.replace(
        "## [0.1.0]",
        "## [0.2.0] - date\n\n- Original claim.\n\n## [0.1.0]",
    );
    repo.commit(
        "land the published body and retain next-window notes",
        &[("CHANGELOG.md", &landed)],
    );
    changelog_gate_at(&repo.git(), "HEAD", "0.2.0").unwrap();
    changelog_gate_at(&repo.git(), "HEAD", "0.3.0").unwrap();

    // NEGATIVE CONTROL: those words are already shipped if a peer returns
    // them to the next window. The authentic release section cannot bless it.
    let stale = landed.replacen(
        "- Corrected claim.",
        "- Corrected claim.\n\n- Original claim.",
        1,
    );
    repo.commit(
        "peer returns shipped wording to next window",
        &[("CHANGELOG.md", &stale)],
    );
    assert!(
        changelog_gate_at(&repo.git(), "HEAD", "0.3.0")
            .unwrap_err()
            .to_string()
            .contains("shipped by v0.2.0")
    );
}

#[test]
fn peer_pushes_after_the_publish_neither_block_the_cut_nor_leak_into_it() {
    let repo = Repo::new("peer-pushes");
    let published = repo.commit("publish me", &[("src/lib.rs", "v1")]);
    repo.publish_origin();
    repo.commit(
        "feat: a peer's code",
        &[("src/lib.rs", "v2"), ("src/new.rs", "x")],
    );
    let tip = repo.commit("docs: a peer's doc", &[("README", "y")]);
    repo.push();

    let checkout = place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&published),
    )
    .expect("pushes after the publish are not a refusal");
    let tree = repo.root.path().join(".ubd/work-cut.noindex");
    assert_eq!(checkout.tree, tree, "the cut tree sits beside the checkout");
    assert!(checkout.moved);
    assert_eq!(checkout.main_ahead, 2);
    let in_tree = |args: &[&str]| {
        let out = testing::git()
            .arg("-C")
            .arg(&tree)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_eq!(in_tree(&["rev-parse", "HEAD"]), published);
    assert_eq!(
        fs::read_to_string(tree.join("src/lib.rs")).unwrap(),
        "v1",
        "the cut tree is the published tree"
    );
    assert!(!tree.join("src/new.rs").exists());
    assert_eq!(repo.run(&["show", "origin/main:src/lib.rs"]), "v2");
    assert_eq!(repo.run(&["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(repo.run(&["rev-parse", "HEAD"]), tip);
    assert_eq!(
        fs::read_to_string(repo.work().join("src/lib.rs")).unwrap(),
        "v2"
    );

    let again = place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&published),
    )
    .unwrap();
    assert!(!again.moved);
    assert_eq!(again.main_ahead, 2);

    fs::remove_dir_all(&tree).unwrap();
    let recreated = place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&published),
    )
    .unwrap();
    assert!(recreated.moved);
    assert_eq!(in_tree(&["rev-parse", "HEAD"]), published);
}

#[test]
fn a_move_preserves_ignored_work_when_rules_or_tracked_paths_change() {
    let run = |(case, leftovers, replacement): (&str, bool, bool)| {
        let mut failures = Vec::new();
        let repo = Repo::new("leftovers");
        repo.commit(
            "a tool with its own ignore file",
            &[
                (".gitignore", "/target/\n"),
                ("tools/old/.gitignore", "out/\nCargo.lock\nNOTES\n"),
                ("tools/old/src.rs", "x"),
                ("src/lib.rs", "v1"),
            ],
        );
        repo.publish_origin();
        let first = repo.run(&["rev-parse", "HEAD"]);
        let tree = place_published(
            &repo.git(),
            &fixture_origin::FixtureOrigin(&repo.git()),
            &repo.work(),
            &source(&first),
        )
        .unwrap()
        .tree;
        let files = [
            ("tools/old/out/model.tla", "spec"),
            ("tools/old/Cargo.lock", "lock"),
            ("tools/old/NOTES", "personal note"),
        ];
        for (rel, body) in files
            .iter()
            .copied()
            .filter(|_| leftovers)
            .chain([("target/release/aterm", "warm")])
        {
            fs::create_dir_all(tree.join(rel).parent().unwrap()).unwrap();
            fs::write(tree.join(rel), body).unwrap();
        }
        repo.run(&["rm", "-r", "-q", "tools/old"]);
        let mut changed = vec![("src/lib.rs", "v2")];
        if replacement {
            changed.push(("tools/old/NOTES", "new tracked contents"));
        }
        let second = repo.commit("the tool left the tree", &changed);
        repo.push();
        let placed = place_published(
            &repo.git(),
            &fixture_origin::FixtureOrigin(&repo.git()),
            &repo.work(),
            &source(&second),
        );
        if leftovers {
            if placed.is_ok() {
                failures.push(format!(
                    "{case}: admitted a move that conflicts with ignored work"
                ));
            }
            for (rel, body) in files {
                let actual = fs::read_to_string(tree.join(rel));
                if actual.as_deref().ok() != Some(body) {
                    failures.push(format!("{case}: {rel} lost {body:?}: {actual:?}"));
                }
            }
        } else {
            assert!(placed.expect("a clean move succeeds").moved);
            assert!(!tree.join("tools/old").exists());
        }
        assert_eq!(
            fs::read_to_string(tree.join("target/release/aterm")).unwrap(),
            "warm",
            "a still-ignored build stays byte for byte"
        );
        let expected_head = if replacement { &first } else { &second };
        let actual_head = rev_parse(&testing::Git::new(&tree), "HEAD").unwrap();
        if actual_head != *expected_head {
            failures.push(format!(
                "{case}: moved HEAD despite an ignored-file replacement"
            ));
        }
        let observed_source = fs::read_to_string(tree.join("src/lib.rs")).unwrap();
        let expected_source = if replacement { "v1" } else { "v2" };
        if observed_source != expected_source {
            failures.push(format!(
                "{case}: tracked source moved to {observed_source:?}"
            ));
        }
        fs::write(tree.join("NOTES"), "mine").unwrap();
        let third = repo.commit("another", &[("src/lib.rs", "v3")]);
        repo.push();
        let error = place_published(
            &repo.git(),
            &fixture_origin::FixtureOrigin(&repo.git()),
            &repo.work(),
            &source(&third),
        )
        .expect_err("pre-existing work refuses")
        .to_string();
        assert!(error.contains("has changes"), "{error}");
        assert_eq!(fs::read_to_string(tree.join("NOTES")).unwrap(), "mine");
        assert_eq!(
            rev_parse(&testing::Git::new(&tree), "HEAD").unwrap(),
            actual_head
        );
        failures
    };
    let failures: Vec<String> = std::thread::scope(|scope| {
        let running: Vec<_> = [
            ("clean move", false, false),
            ("exposed ignored files", true, false),
            ("ignored note becomes tracked", true, true),
        ]
        .into_iter()
        .map(|each| scope.spawn(move || run(each)))
        .collect();
        running
            .into_iter()
            .flat_map(|ran| ran.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_published_commit_main_does_not_carry_is_refused() {
    let repo = Repo::new("off-main");
    repo.commit("base", &[("a", "1")]);
    repo.publish_origin();
    repo.run(&["checkout", "-q", "-b", "side"]);
    let side = repo.commit("a side commit", &[("a", "2")]);
    repo.run(&["checkout", "-q", "main"]);
    place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&side),
    )
    .expect_err("a commit main does not carry is no release source");
    assert!(
        !repo.root.path().join(".ubd/work-cut.noindex").exists(),
        "no cut tree was made"
    );

    let unknown = "f".repeat(40);
    place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&unknown),
    )
    .expect_err("a commit this repository does not have");
}

#[test]
fn the_operators_work_does_not_block_the_cut_and_a_dirty_cut_tree_refuses() {
    let repo = Repo::new("dirty");
    let first = repo.commit("publish me", &[("a", "1")]);
    repo.publish_origin();
    let second = repo.commit("later", &[("a", "2")]);
    repo.push();
    fs::write(repo.work().join("a"), "uncommitted").unwrap();
    let checkout = place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&first),
    )
    .expect("the operator's uncommitted work is not the cut's business");
    assert_eq!(
        fs::read_to_string(repo.work().join("a")).unwrap(),
        "uncommitted",
        "and it is left exactly as it was"
    );

    fs::write(checkout.tree.join("a"), "edited in the cut tree").unwrap();
    place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&second),
    )
    .expect_err("a cut tree with changes must refuse before it moves");
    assert_eq!(
        fs::read_to_string(checkout.tree.join("a")).unwrap(),
        "edited in the cut tree",
        "the change was not discarded"
    );

    let in_tree = |args: &[&str]| {
        let out = testing::git()
            .arg("-C")
            .arg(&checkout.tree)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    in_tree(&["checkout", "-q", "--", "a"]);
    fs::write(checkout.tree.join("stray.txt"), "left here").unwrap();
    let info = repo.work().join(".git/info");
    fs::create_dir_all(&info).unwrap();
    fs::write(info.join("exclude"), "stray.txt\n").unwrap();
    assert_eq!(
        in_tree(&["status", "--porcelain"]),
        "",
        "porcelain sees nothing"
    );
    let error = place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&second),
    )
    .expect_err("a file only info/exclude hides is a change")
    .to_string();
    assert!(error.contains("stray.txt"), "{error}");
    fs::remove_file(checkout.tree.join("stray.txt")).unwrap();
    place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&second),
    )
    .expect("a clean cut tree moves");
}

#[test]
fn a_foreign_directory_where_the_cut_tree_goes_is_refused_and_left_alone() {
    let repo = Repo::new("foreign");
    let published = repo.commit("publish me", &[("a", "1")]);
    repo.publish_origin();
    let foreign = repo.root.path().join(".ubd/work-cut.noindex");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("keep"), "mine").unwrap();
    place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&published),
    )
    .expect_err("not ours");
    assert_eq!(fs::read_to_string(foreign.join("keep")).unwrap(), "mine");
}

#[test]
fn an_info_attributes_line_that_rewrites_bytes_refuses_the_cut() {
    let repo = Repo::new("info-attrs");
    let published = repo.commit("publish me", &[("a", "1\n")]);
    repo.publish_origin();
    let info = repo.work().join(".git/info");
    fs::create_dir_all(&info).unwrap();
    let cut = repo.root.path().join(".ubd/work-cut.noindex");
    for line in ["* text eol=crlf", "a -text", "[attr]crlfy text eol=crlf"] {
        fs::write(info.join("attributes"), format!("# mine\n{line}\n")).unwrap();
        let error = place_published(
            &repo.git(),
            &fixture_origin::FixtureOrigin(&repo.git()),
            &repo.work(),
            &source(&published),
        )
        .expect_err(line)
        .to_string();
        assert!(error.contains(line), "{error}");
        assert!(!cut.exists(), "{line}: no cut tree was made");
    }
    fs::write(info.join("attributes"), b"#\xff\n* text eol=crlf\n").unwrap();
    assert_eq!(repo.run(&["check-attr", "eol", "--", "a"]), "a: eol: crlf");
    place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&published),
    )
    .expect_err("unreadable attributes cannot establish the checkout's bytes");
    assert!(!cut.exists(), "no cut tree was made after a failed read");

    fs::remove_file(info.join("attributes")).unwrap();
    fs::create_dir(info.join("attributes")).unwrap();
    refuse_rewriting_info_attributes(&repo.git())
        .expect_err("an attributes directory is not a missing attributes file");
    fs::remove_dir(info.join("attributes")).unwrap();
    fs::write(info.join("attributes"), "# mine\n*.png diff=exif\n").unwrap();
    let checkout = place_published(
        &repo.git(),
        &fixture_origin::FixtureOrigin(&repo.git()),
        &repo.work(),
        &source(&published),
    )
    .expect("an attribute that rewrites nothing");
    assert_eq!(fs::read_to_string(checkout.tree.join("a")).unwrap(), "1\n");
}

#[test]
fn the_cut_tree_sits_beside_the_checkout_never_inside_it() {
    assert_eq!(
        cut_tree_path(Path::new("/Users//me/aterm")).unwrap(),
        PathBuf::from("/Users//me/.ubd/aterm-cut.noindex")
    );
    assert!(cut_tree_path(Path::new("/")).is_err());
}

#[test]
fn the_newest_aterm_row_is_the_published_source() {
    let sha = |c: char| c.to_string().repeat(40);
    let text = format!(
        r#"{{"schema":1,"mappings":{{
                "aterm":{{
                    "{}":{{"verified_at":"2026-09-21T19:38:24+00:00"}},
                    "{}":{{"verified_at":"2026-09-22T23:37:28+00:00"}},
                    "short":{{"verified_at":"2026-09-30T00:00:00+00:00"}}
                }},
                "trust":{{"{}":{{"verified_at":"2026-12-31T00:00:00+00:00"}}}}
            }}}}"#,
        sha('a'),
        sha('b'),
        sha('c')
    );
    let newest = newest_published_source(&text).unwrap();
    assert_eq!(
        newest.commit,
        sha('b'),
        "another repo's row and a non-sha key never win"
    );
    let mixed_offsets = format!(
        r#"{{"schema":1,"mappings":{{"aterm":{{
                "{}":{{"verified_at":"2026-09-23T00:15:00Z"}},
                "{}":{{"verified_at":"2026-09-22T23:30:00.25-02:00"}},
                "{}":{{"verified_at":"2026-09-23T00:45:00+00:00"}}
            }}}}}}"#,
        sha('a'),
        sha('d'),
        sha('e')
    );
    assert_eq!(
        newest_published_source(&mixed_offsets).unwrap().commit,
        sha('d'),
        "2026-09-22T23:30:00.25-02:00 (01:30:00.25Z on 09-23) is newer by UTC instant than 00:45Z"
    );
    assert!(newest_published_source(r#"{"mappings":{}}"#).is_err());
    assert!(newest_published_source("not json").is_err());
    // A revoked row is no publication: the newest live row is released. NEGATIVE CONTROL:
    // the same rows unrevoked release the newer one (above, `b`).
    let revoked = |rows: &str| {
        format!(
            r#"{{"mappings":{{"aterm":{{
                    "{}":{{"verified_at":"2026-09-21T19:38:24+00:00"}},
                    "{}":{{"verified_at":"2026-09-22T23:37:28+00:00"}}
                }}}},"revoked":{{"aterm":{{{rows}}}}}}}"#,
            sha('a'),
            sha('b'),
        )
    };
    let one = format!(r#""{}":{{"reason":"withdrawn"}}"#, sha('b'));
    assert_eq!(
        newest_published_source(&revoked(&one)).unwrap().commit,
        sha('a')
    );
    let both = format!(r#"{one},"{}":{{}}"#, sha('a'));
    assert!(
        newest_published_source(&revoked(&both)).is_err(),
        "no live row, no source"
    );
}

#[test]
fn ship_check_judges_the_named_commit_with_the_cuts_own_gates() {
    use aterm_update_core::handoff_policy::SOURCE_PATH;
    let repo = Repo::new("precheck");
    let cargo = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.2.0\"\n";
    let guard = "const PINNED_DESKS: &[(&str, &str)] = &[\n    (\"v0.1.0\", \"history\"),\n];\n\
                 const REQUIRED_DESKS: &[(&str, &str)] = &[\n    (\"v0.1.0\", \"history\"),\n];\n";
    let notes = "# Changelog\n\n## [Unreleased]\n\n- a change worth releasing\n\n\
                 ## [0.1.0]\n\n- the first one\n";
    let fixtures = format!("{HANDOFF_FIXTURE_ROOT}/v0.1.0");
    let desk = format!("{fixtures}/history/parent.toml");
    let candidate = repo.commit(
        "the release candidate",
        &[
            ("Cargo.toml", cargo),
            (crate::ledger::LEDGER_FILE, "# ledger\n1790000000 0.1.0\n"),
            (HANDOFF_FIXTURE_GUARD, guard),
            (&desk, "producer_version = \"0.1.0\"\n"),
            (changelog::CHANGELOG_FILE, notes),
            (SOURCE_PATH, "schema = 1\n"),
        ],
    );
    repo.publish_origin();
    repo.run(&["tag", "v0.1.0"]);
    repo.run(&["push", "-q", "origin", "v0.1.0"]);
    repo.run(&["rm", "-q", "-r", &fixtures]);
    let later = repo.commit("a peer's commit", &[]);
    for (path, body) in [
        (changelog::CHANGELOG_FILE, "# Changelog\n"),
        (SOURCE_PATH, "schema = 1\ncary = \"full\"\n"),
    ] {
        fs::write(repo.work().join(path), body).unwrap();
    }
    let git = repo.git();
    let check = |rev: &str| {
        let own = repo.run(&["rev-parse", rev]);
        precheck_with_judge(
            &git,
            Ok(&fixture_origin::FixtureOrigin(&git)),
            rev,
            Ok(&own),
            |commit| verify_gate_at_with_inputs(&git, commit, || Ok("inputs".into())),
        )
        .expect("a releasable commit")
    };

    let found = check(&candidate);
    assert_eq!(
        (found.commit.as_str(), found.version.as_str()),
        (candidate.as_str(), "0.2.0")
    );
    assert_eq!(found.refused(), ["verify release"]);
    let source = precheck_parts(
        &git,
        Ok(&fixture_origin::FixtureOrigin(&git)),
        &candidate,
        Ok(&candidate),
        None::<fn(&str) -> Result<String>>,
    )
    .unwrap();
    assert_eq!(
        source.exit_code(),
        0,
        "source-only sampling needs no bundle"
    );
    assert_eq!(source.gates.len() + 1, found.gates.len());
    assert!(!source.gates.iter().any(|g| g.name == "verify release"));

    let unavailable = precheck_parts(
        &git,
        Err(Error::new("native source binding unavailable")),
        &candidate,
        Ok(&candidate),
        None::<fn(&str) -> Result<String>>,
    )
    .unwrap();
    assert_eq!(
        unavailable.refused(),
        [
            "changelog",
            "handoff fixtures",
            "harness versions",
            "handoff policy"
        ],
        "native-origin failure stays on the checks that need it"
    );
    assert!(
        unavailable
            .gates
            .iter()
            .filter(|g| g.name == "cutter identity")
            .all(|g| matches!(g.outcome, Outcome::Pass(_)))
    );

    let macho = "ab".repeat(32);
    crate::release::Journal {
        version: "0.2.0".to_string(),
        commit: candidate.clone(),
        artifact: Some(crate::release::Artifact {
            key: "cd".repeat(32),
            app: repo.root.path().join("cache/aterm.app"),
            macho_sha256: macho.clone(),
            build: 1_790_000_100,
        }),
        ..crate::release::Journal::default()
    }
    .save(
        &repo
            .work()
            .join(".git")
            .join(crate::release::RELEASE_DIR)
            .join(crate::release::JOURNAL_FILE),
    )
    .unwrap();
    let ok = |_: &str| Some("ok");
    crate::check_record::record_run(&git, &candidate, &"ef".repeat(32), "inputs", 0, ok);
    let found = check(&candidate);
    assert_eq!(
        found.refused(),
        ["verify release"],
        "another executable's measure record: {}",
        found.transcript()
    );
    crate::check_record::record_run(&git, &candidate, &macho, "inputs", 0, ok);
    let found = check(&candidate);
    assert!(found.refused().is_empty(), "{}", found.transcript());

    let head = check("HEAD");
    assert_eq!(head.commit, later);
    assert_eq!(head.refused(), ["handoff fixtures", "verify release"]);
    crate::check_record::record_run(&git, &candidate, &macho, "inputs", 1, |c| {
        Some(if c == "lint" { "FAIL" } else { "ok" })
    });
    let found = check(&candidate);
    assert_eq!(
        found.refused(),
        ["verify release"],
        "a FAIL record: {}",
        found.transcript()
    );
    assert!(
        found.transcript().contains("FAILED lint"),
        "{}",
        found.transcript()
    );
    let patch = repo.commit(
        "a patch version",
        &[("Cargo.toml", &cargo.replace("0.2.0", "0.2.1"))],
    );
    precheck_with(
        &git,
        Ok(&fixture_origin::FixtureOrigin(&git)),
        &patch,
        Ok(&patch),
    )
    .expect_err("no release version");
    assert!(
        precheck_with(
            &git,
            Ok(&fixture_origin::FixtureOrigin(&git)),
            "no-such-commit",
            Ok(&patch)
        )
        .is_err()
    );
}

#[test]
fn a_cut_keeps_the_handoff_policy_its_predecessor_sealed() {
    use aterm_update_core::handoff_policy::SOURCE_PATH;
    let repo = Repo::new("sealed-policy");
    let knob =
        |range: &str| format!("schema = 1\napplies_to_producers = {range}\ncarry = \"repaint\"\n");
    let ranged = |range: &str| {
        format!(
            "{}\n[[ranges]]\nproducers = {range}\ncarry = \"repaint\"\nreason = \"the \
             rescue\"\n",
            knob(range)
        )
    };
    let retired = |range: &str| {
        format!(
            "\n[[retired]]\nproducers = {range}\nreason = \"every build in range is past its \
             last supported update\"\ndate = \"2026-10-03\"\n"
        )
    };
    let rescue = knob("[1791000000, 1791500000]");
    repo.commit(
        "v0.1.0, which seals a rescue",
        &[
            (
                crate::ledger::LEDGER_FILE,
                "# ledger\n1791000000 0.0.8\n1791500000 0.0.9\n1792000000 0.1.0\n",
            ),
            (SOURCE_PATH, rescue.as_str()),
        ],
    );
    repo.publish_origin();
    repo.run(&["tag", "v0.1.0"]);
    repo.run(&["push", "-q", "origin", "v0.1.0"]);
    let git = repo.git();
    let judge = |policy: &str| {
        let commit = repo.commit(policy, &[(SOURCE_PATH, policy)]);
        handoff_policy_gate_at(&git, &fixture_origin::FixtureOrigin(&git), &commit, "0.2.0")
            .map(|p| p.to_string())
    };

    for kept in [
        ranged("[1791000000, 1791500000]"),
        ranged("[1790000000, 1792000000]"),
    ] {
        judge(&kept).unwrap_or_else(|e| panic!("{kept}: {e}"));
    }
    for dropped in [
        "schema = 1\n".to_string(),
        ranged("[1791000001, 1791500000]"),
        ranged("[1791000000, 1791500000]").replace("repaint", "visible"),
    ] {
        judge(&dropped).expect_err(&dropped);
    }

    let said = judge(&format!(
        "schema = 1\n{}",
        retired("[1791000000, 1791500000]")
    ))
    .expect("a named retirement drops the rescue");
    assert!(said.contains("RETIRES"), "{said}");
    for misnamed in [
        format!("schema = 1\n{}", retired("[1791000000, 1791499999]")),
        format!(
            "{}{}",
            ranged("[1790000000, 1792000000]"),
            retired("[1791000000, 1791500000]")
        ),
        format!(
            "{}{}",
            ranged("[1791000000, 1791500000]"),
            retired("[1791000000, 1791500000]")
        ),
    ] {
        judge(&misnamed).expect_err(&misnamed);
    }

    let other = repo.root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let in_other = |args: &[&str]| {
        let out = testing::git()
            .arg("-C")
            .arg(&other)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let origin = repo.root.path().join("origin.git");
    in_other(&["clone", "-q", origin.to_str().unwrap(), "."]);
    in_other(&["config", "user.name", "Test"]);
    in_other(&["config", "user.email", "test@example.invalid"]);
    in_other(&["config", "commit.gpgsign", "false"]);
    in_other(&["config", "tag.gpgsign", "false"]);
    fs::write(other.join(SOURCE_PATH), knob("[1791000000, 1792000000]")).unwrap();
    in_other(&["commit", "-q", "-a", "-m", "v0.1.0, recut"]);
    in_other(&["tag", "-f", "-a", "-m", "v0.1.0", "v0.1.0"]);
    in_other(&["push", "-q", "--force", "origin", "refs/tags/v0.1.0"]);
    judge(&ranged("[1791000000, 1791500000]")).expect_err("origin's tag, not this clone's");

    let bare = Repo::new("sealed-policy-none");
    bare.commit(
        "v0.97.0, with no policy file",
        &[(
            crate::ledger::LEDGER_FILE,
            "# ledger\n1790563767 0.95.0\n1790585188 0.97.0\n",
        )],
    );
    bare.publish_origin();
    bare.run(&["tag", "v0.97.0"]);
    bare.run(&["push", "-q", "origin", "v0.97.0"]);
    let judge = |policy: &str| {
        let commit = bare.commit(policy, &[(SOURCE_PATH, policy)]);
        handoff_policy_gate_at(
            &bare.git(),
            &fixture_origin::FixtureOrigin(&bare.git()),
            &commit,
            "0.98.0",
        )
        .map(|p| p.to_string())
    };
    judge("schema = 1\n").expect("nothing sealed");
    judge(&ranged("[1790563767, 1790585188]")).expect("names v0.97.0's build");
    for range in [
        "[179000000, 179000000]",
        "[1790563767, 1790563767]",
        "[1790585188, 1890585188]",
    ] {
        judge(&ranged(range)).expect_err(range);
    }
    let said = judge(&format!("schema = 1\n{}", retired("[10, 20]")))
        .expect("an earlier retirement, kept on record");
    assert!(!said.contains("RETIRES"), "{said}");
}

/// A PREDECESSOR NO RELEASE SHIPPED IS REFUSED UNDER THE POLICY GATE'S NAME: with a ledger
/// none of whose earlier versions has its tag on origin, the handoff-policy gate refuses the
/// cut saying so, as `handoff-policy gate:`, never under the fixture gate's name. CONTROL:
/// once origin carries the newest one's tag, the same policy passes.
#[test]
fn a_predecessor_no_release_shipped_is_refused_by_the_policy_gate() {
    use aterm_update_core::handoff_policy::SOURCE_PATH;
    let repo = Repo::new("policy-unshipped");
    repo.commit(
        "two claims, neither shipped",
        &[(
            crate::ledger::LEDGER_FILE,
            "# ledger\n1790563767 0.95.0\n1790585188 0.97.0\n",
        )],
    );
    repo.publish_origin();
    let judge = || {
        let commit = repo.commit("the cut's policy", &[(SOURCE_PATH, "schema = 1\n")]);
        handoff_policy_gate_at(
            &repo.git(),
            &fixture_origin::FixtureOrigin(&repo.git()),
            &commit,
            "0.98.0",
        )
        .map(|p| p.to_string())
    };
    let refused = judge().expect_err("no shipped predecessor").to_string();
    let unshipped = predecessor_release(
        "# ledger\n1790563767 0.95.0\n1790585188 0.97.0\n",
        "0.98.0",
        &|_| false,
    )
    .expect_err("no earlier version shipped")
    .to_string();
    assert_eq!(
        refused,
        format!("handoff-policy gate: {unshipped}"),
        "the gate that asked names itself"
    );
    repo.run(&["tag", "v0.97.0"]);
    repo.run(&["push", "-q", "origin", "v0.97.0"]);
    judge().unwrap_or_else(|e| panic!("CONTROL: v0.97.0 shipped: {e}"));
}

#[test]
fn a_cut_that_drops_the_predecessors_producer_policy_is_refused() {
    use aterm_update_core::handoff_policy::SOURCE_PATH;
    const LEDGER: &str = "# ledger\n1791000000 0.0.8\n1791500000 0.0.9\n1792000000 0.1.0\n";
    const PANICS: &str = "[[ranges]]\nproducers = [1791000000, 1791100000]\ncarry = \
                          \"repaint\"\nreason = \"v0.0.8's capture panics on a wide desk\"\n";
    const GATE: &str = "[[ranges]]\nproducers = [1791500000, 1791900000]\n\
                        park_quiet_gate_at_land = \"relaxed\"\nreason = \"v0.0.9's Land \
                        gate never opens\"\n";
    const HULL: &str = "schema = 1\napplies_to_producers = [1791000000, 1791900000]\ncarry = \
                        \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n";
    let sealed = format!("{HULL}\n{PANICS}\n{GATE}");
    let repo = Repo::new("sealed-ranges");
    repo.commit(
        "v0.1.0, which seals two rescues",
        &[
            (crate::ledger::LEDGER_FILE, LEDGER),
            (SOURCE_PATH, sealed.as_str()),
        ],
    );
    repo.publish_origin();
    repo.run(&["tag", "v0.1.0"]);
    repo.run(&["push", "-q", "origin", "v0.1.0"]);
    let git = repo.git();
    let judge = |policy: &str| {
        let commit = repo.commit(policy, &[(SOURCE_PATH, policy)]);
        handoff_policy_gate_at(&git, &fixture_origin::FixtureOrigin(&git), &commit, "0.2.0")
            .map(|p| p.to_string())
    };

    judge(&sealed).unwrap_or_else(|e| panic!("both kept: {e}"));
    let wider = "schema = 1\napplies_to_producers = [1791000000, 1791900000]\ncarry = \
                 \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n\n[[ranges]]\nproducers \
                 = [1791000000, 1791900000]\ncarry = \"repaint\"\npark_quiet_gate_at_land = \
                 \"relaxed\"\nreason = \"both\"\n";
    judge(wider).unwrap_or_else(|e| panic!("one wider rescue keeps both: {e}"));

    let only_panics = format!(
        "schema = 1\napplies_to_producers = [1791000000, 1791100000]\ncarry = \
         \"repaint\"\n\n{PANICS}"
    );
    judge(&only_panics).expect_err("a dropped rescue");
    let narrowed = format!(
        "schema = 1\napplies_to_producers = [1791000000, 1791800000]\ncarry = \
         \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n\n{PANICS}\n{}",
        GATE.replace("1791900000", "1791800000")
    );
    let weakened = format!(
        "schema = 1\napplies_to_producers = [1791000000, 1791900000]\ncarry = \
         \"visible\"\npark_quiet_gate_at_land = \"relaxed\"\n\n{}\n{GATE}",
        PANICS.replace("repaint", "visible")
    );
    for changed in [narrowed, weakened] {
        judge(&changed).expect_err(&changed);
    }

    let retired = format!(
        "{only_panics}\n[[retired]]\nproducers = [1791500000, 1791900000]\nreason = \"every \
         v0.0.9 install is past its last supported update\"\ndate = \"2026-10-03\"\n"
    );
    let said = judge(&retired).expect("a retired rescue may be dropped");
    assert!(said.contains("RETIRES"), "{said}");
}

#[test]
fn ship_check_answers_only_with_the_commits_own_cutter() {
    let repo = Repo::new("precheck-identity");
    let cargo = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.2.0\"\n";
    let candidate = repo.commit(
        "the release candidate",
        &[
            ("Cargo.toml", cargo),
            ("crates/aterm-release/src/gates.rs", "// the rules\n"),
        ],
    );
    let git = repo.git();
    let found = precheck_with(
        &git,
        Ok(&fixture_origin::FixtureOrigin(&git)),
        &candidate,
        Ok(&candidate),
    )
    .expect("its own");
    assert_eq!(found.gates[0].name, "cutter identity");
    assert!(matches!(found.gates[0].outcome, Outcome::Pass(_)));
    let docs = repo.commit("a doc", &[("docs/NOTE.md", "prose\n")]);
    let not_run = |stamp: std::result::Result<&str, &crate::cutter::NotOwn>| {
        let found = precheck_with(&git, Ok(&fixture_origin::FixtureOrigin(&git)), &docs, stamp)
            .expect("the commit reads");
        assert_eq!(
            found.not_run(),
            ["cutter identity"],
            "{}",
            found.transcript()
        );
        assert_eq!(found.gates.len(), 1, "no cut gate is asked");
        assert_eq!(found.exit_code(), 3);
        match &found.gates[0].outcome {
            Outcome::CouldNotRun(r) => r.clone(),
            other => panic!("{other:?}"),
        }
    };
    not_run(Ok(&candidate));
    not_run(Ok("unknown"));
    let none = crate::cutter::NotOwn {
        why: "a source changed after this cutter was linked".into(),
        fix: "relink it".into(),
    };
    let r = not_run(Err(&none));
    assert!(r.why.contains(&none.why), "{r:?}");
    assert_eq!(r.remedy, none.fix);
}
