// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::EnvSnapshot;

struct Repo {
    _dir: aterm_tempfile::TempDir,
    root: std::path::PathBuf,
    env: EnvSnapshot,
}

impl Repo {
    /// A repository whose `base` branch holds `changelog` and `fragments`.
    fn new(changelog: &str, fragments: &[(&str, &str)]) -> Repo {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let env = EnvSnapshot {
            path: std::env::var_os("PATH").unwrap_or_default(),
            ..Default::default()
        };
        let repo = Repo {
            _dir: dir,
            root,
            env,
        };
        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.name", "A"]);
        repo.git(&["config", "user.email", "a@example.invalid"]);
        repo.write(CHANGELOG_FILE, changelog);
        for (path, text) in fragments {
            repo.write(path, text);
        }
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "base"]);
        repo.git(&["branch", "base"]);
        repo
    }

    // The fixture producer and diff selector use the same explicit Git provider.
    fn git(&self, args: &[&str]) {
        let out = crate::changed::git(&self.root, &self.env.path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, path: &str, text: &str) {
        let full = self.root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    fn ctx(&self) -> Ctx {
        Ctx::new(
            self.root.clone(),
            Scope::changed("base", vec![], false),
            self.env.clone(),
            self.root.join("scratch"),
        )
    }

    fn judged(&self) -> Vec<String> {
        judge(&self.ctx()).unwrap().expect("selected")
    }
}

const CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- **Raw one.** Before \
                         fragments.\n\n- **Raw two.** Also.\n\n## [0.2.0] - 2026-11-10\n\n\
                         ### Fixed\n\n- **Shipped.** In 0.2.0.\n";

/// (a) AN ENTRY A BRANCH ADDS OUTSIDE A RELEASED SECTION IS RED, under [Unreleased] or
/// above every section, naming where entries go; one already in the base's file is not.
/// NEGATIVE CONTROL: moving a raw entry to another kind, removing one, and editing the head's
/// prose pass; a change that touches neither the changelog nor changes/ is not judged.
#[test]
fn a_branch_that_adds_an_entry_outside_a_released_section_is_red() {
    let repo = Repo::new(CHANGELOG, &[]);
    repo.write("unrelated.txt", "work");
    assert!(judge(&repo.ctx()).unwrap().is_none());
    for (text, line) in [
        (
            CHANGELOG.replace("- **Raw two.**", "- **New.** Added raw.\n\n- **Raw two.**"),
            9,
        ),
        (format!("- **Stray.**\n\n{CHANGELOG}"), 1),
        (
            CHANGELOG.replace("- **Raw two.** Also.", "- **Raw two.** Reworded."),
            9,
        ),
    ] {
        repo.write(CHANGELOG_FILE, &text);
        let found = repo.judged();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with(&format!("CHANGELOG.md line {line}: an entry outside"))
                && found[0].contains("changes/<kind>/<session>-<slug>.md")
                && found[0].contains("added, changed, deprecated, removed, fixed, security"),
            "{found:?}"
        );
    }
    for text in [
        CHANGELOG.replace(
            "### Fixed\n\n- **Raw one.** Before fragments.\n\n",
            "### Added\n\n- **Raw one.** Before fragments.\n\n### Fixed\n\n",
        ),
        CHANGELOG.replace("- **Raw one.** Before fragments.\n\n", ""),
        CHANGELOG.replace("# Changelog\n", "# Changelog\n\nHead prose, reworded.\n"),
    ] {
        repo.write(CHANGELOG_FILE, &text);
        assert_eq!(repo.judged(), Vec::<String>::new(), "{text}");
    }
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "edit"]);
    assert_eq!(repo.judged(), Vec::<String>::new());
}

/// (b) EVERY FRAGMENT THE BRANCH ADDS OR CHANGES IS JUDGED AS IT SHIPS, naming its line;
/// a fragment the branch did not touch is not.
/// NEGATIVE CONTROL: a well-formed fragment passes.
#[test]
fn a_fragment_the_branch_adds_is_judged_at_its_line() {
    let repo = Repo::new(CHANGELOG, &[("changes/fixed/old.md", "lazy\n")]);
    repo.write("changes/fixed/sa-good.md", "- **Good.** Fine.\n");
    assert_eq!(
        repo.judged(),
        Vec::<String>::new(),
        "untouched old.md is not judged"
    );
    repo.write("changes/fixed/sa-bad.md", "- **Bad.**\nunindented\n");
    repo.write("changes/fixed/sa-case.txt", "- **Case.**\n");
    let found = repo.judged();
    assert!(
        found
            .iter()
            .any(|f| f
                == "changes/fixed/sa-bad.md line 2: indent continuation lines with two spaces"),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|f| f.starts_with("changes/fixed/sa-case.txt: a changelog entry is a file")),
        "{found:?}"
    );
    std::fs::remove_file(repo.root.join("changes/fixed/sa-bad.md")).unwrap();
    std::fs::remove_file(repo.root.join("changes/fixed/sa-case.txt")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = repo.root.join("changes/fixed/sa-good.md");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(repo.judged()[0].contains("not a regular, non-executable file"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_eq!(repo.judged(), Vec::<String>::new());
}

/// (c) A FRAGMENT REPEATING A SHIPPED ENTRY, OR ANOTHER FRAGMENT, IS RED, naming the release
/// or the other file.
/// NEGATIVE CONTROL: the same lead with new words passes.
#[test]
fn a_fragment_that_repeats_a_shipped_entry_or_another_fragment_is_red() {
    let repo = Repo::new(
        CHANGELOG,
        &[("changes/fixed/peer.md", "- **Peer.** Theirs.\n")],
    );
    repo.git(&["tag", "v0.2.0"]);
    repo.write("changes/fixed/sa-again.md", "- **Shipped.** In 0.2.0.\n");
    let found = repo.judged();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("v0.2.0's tag holds this entry"),
        "{found:?}"
    );
    repo.write("changes/fixed/sa-again.md", "- **Peer.** Theirs.\n");
    let found = repo.judged();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("changes/fixed/peer.md holds this entry too"),
        "{found:?}"
    );
    repo.write(
        "changes/fixed/sa-again.md",
        "- **Shipped.** And more since.\n",
    );
    assert_eq!(repo.judged(), Vec::<String>::new());
}

/// (c) A FRAGMENT IS SHIPPED ONLY WHERE ITS RELEASE'S TAG HOLDS IT, as the release judges
/// it: a section main keeps for a version that was abandoned (its tag deleted) shipped
/// nothing, so its notes restored as files pass; a tagged one is red, naming the tag and the
/// stale-tag case. A fragment repeating a raw `[Unreleased]` entry (the drain) is red, naming
/// the line, and whitespace alone makes no new entry.
/// NEGATIVE CONTROL: the abandoned section's entry as a file passes.
#[test]
fn a_fragment_repeats_only_what_a_tag_shipped_or_what_unreleased_holds() {
    let abandoned = CHANGELOG.replace(
        "## [0.2.0]",
        "## [0.3.0] - 2026-11-11\n\n### Fixed\n\n- **Abandoned.** Never shipped.\n\n## [0.2.0]",
    );
    let repo = Repo::new(&abandoned, &[]);
    repo.git(&["tag", "v0.2.0"]);
    repo.write(
        "changes/fixed/sa-restored.md",
        "- **Abandoned.** Never shipped.\n",
    );
    assert_eq!(repo.judged(), Vec::<String>::new(), "v0.3.0 has no tag");
    repo.write(
        "changes/fixed/sa-restored.md",
        "- **Shipped.**  In 0.2.0.\n",
    );
    let found = repo.judged();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("v0.2.0's tag holds this entry")
            && found[0].contains("git tag -d v0.2.0"),
        "{found:?}"
    );
    repo.write(
        "changes/fixed/sa-restored.md",
        "- **Raw one.**\n  Before fragments.\n",
    );
    let found = repo.judged();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("CHANGELOG.md line 7 holds this entry under [Unreleased]"),
        "{found:?}"
    );
}

/// (d) A MERGE ACROSS A RELEASE THAT MOVES AN ENTRY INTO OR OUT OF THE SECTION MAIN ROLLED
/// IS RED: git's line merge leaves a branch's raw entry inside the `## [0.3.0]` the roll
/// inserted above it, and applies a branch's removal of a raw entry inside it; both are
/// clean merges. A released section records what its release shipped, so an entry it gains
/// or loses against the base is named, and so is an edit to a shipped entry.
/// NEGATIVE CONTROL: the merge of a branch that only adds a file passes.
#[test]
fn a_merge_that_moves_an_entry_into_or_out_of_a_released_section_is_red() {
    let base = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- **A1.** One.\n\n\
                - **A2.** Two.\n\n### Fixed\n\n- **F1.** One.\n\n- **F2.** Two.\n\n\
                - **F3.** Three.\n\n- **F4.** Four.\n\n## [0.2.0] - 2026-11-10\n\n\
                ### Fixed\n\n- **Shipped.** In 0.2.0.\n";
    let rolled = base.replacen(
        "## [Unreleased]\n",
        "## [Unreleased]\n\n## [0.3.0] - 2026-11-11\n",
        1,
    );
    let fork = |branch: &dyn Fn(&Repo)| -> Repo {
        let repo = Repo::new(base, &[]);
        repo.git(&["checkout", "-q", "-b", "rolled"]);
        repo.write(CHANGELOG_FILE, &rolled);
        repo.git(&["commit", "-qam", "release: v0.3.0"]);
        repo.git(&["checkout", "-q", "-"]);
        branch(&repo);
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-qm", "branch"]);
        repo.git(&["merge", "-q", "--no-ff", "--no-edit", "rolled"]);
        repo
    };
    let judged = |repo: &Repo| -> Vec<String> {
        let ctx = Ctx::new(
            repo.root.clone(),
            Scope::changed("rolled", vec![], false),
            repo.env.clone(),
            repo.root.join("scratch"),
        );
        judge(&ctx).unwrap().expect("selected")
    };
    let gains = fork(&|repo| {
        repo.write(
            CHANGELOG_FILE,
            &base.replace(
                "- **F4.** Four.\n",
                "- **F4.** Four.\n\n- **X.** The old way.\n",
            ),
        );
    });
    let merged = std::fs::read_to_string(gains.root.join(CHANGELOG_FILE)).unwrap();
    assert!(
        merged.find("## [0.3.0]") < merged.find("- **X.**")
            && merged.find("- **X.**") < merged.find("## [0.2.0]"),
        "git's merge leaves the entry in the rolled section: {merged}"
    );
    let found = judged(&gains);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("[0.3.0] gains an entry") && found[0].contains("changes/<kind>/"),
        "{found:?}"
    );
    let loses = fork(&|repo| {
        repo.write(CHANGELOG_FILE, &base.replace("- **F2.** Two.\n\n", ""));
        repo.write("changes/fixed/s-f2.md", "- **F2.** Two.\n");
    });
    let found = judged(&loses);
    assert!(
        found.iter().any(|f| f.contains("[0.3.0] loses the entry")),
        "{found:?}"
    );
    let file_only = fork(&|repo| {
        repo.write("changes/fixed/s-new.md", "- **New.** From a file.\n");
    });
    assert_eq!(judged(&file_only), Vec::<String>::new());
    let repo = Repo::new(CHANGELOG, &[]);
    repo.write(
        CHANGELOG_FILE,
        &CHANGELOG.replace("In 0.2.0.", "In 0.2.0, corrected."),
    );
    let found = repo.judged();
    assert_eq!(found.len(), 2, "an edit is a loss and a gain: {found:?}");
}

/// (a) TEXT A BRANCH ADDS UNDER `[Unreleased]` THAT THE RELEASE'S ROLL REFUSES IS RED AT ITS
/// LINE: an unknown subsection, prose, an HTML comment that never closes (which would
/// swallow every raw entry after it).
/// NEGATIVE CONTROL: the same prose its base already held is the release's to name.
#[test]
fn text_a_branch_adds_that_the_roll_refuses_is_red() {
    let repo = Repo::new(CHANGELOG, &[]);
    for (text, expected) in [
        (
            CHANGELOG.replace("### Fixed\n", "### Notes\n\n### Fixed\n"),
            "line 5: [Unreleased] has an unknown subsection",
        ),
        (
            CHANGELOG.replace("### Fixed\n\n", "### Fixed\n\nProse.\n\n"),
            "line 7: [Unreleased] holds text that is no entry",
        ),
        (
            CHANGELOG.replace("- **Raw two.**", "<!-- a note\n\n- **Raw two.**"),
            "line 9: an HTML comment that never closes",
        ),
    ] {
        repo.write(CHANGELOG_FILE, &text);
        let found = repo.judged();
        assert!(
            found
                .iter()
                .any(|f| f.starts_with(&format!("CHANGELOG.md {expected}"))),
            "{expected}: {found:?}"
        );
    }
    let prose = CHANGELOG.replace("### Fixed\n\n", "### Fixed\n\nProse.\n\n");
    let repo = Repo::new(&prose, &[]);
    repo.write(
        CHANGELOG_FILE,
        &prose.replace("- **Raw one.** Before fragments.\n\n", ""),
    );
    assert_eq!(repo.judged(), Vec::<String>::new());
}

/// THE RULES SELECT AND NAME A CASE VARIANT OF `changes/` AND A FILE NAMED `changes`, which
/// the release's strict read refuses; and a fragment over the bound is named without being
/// read (bytes that are not even UTF-8 make no failure to run).
/// NEGATIVE CONTROL: a well-formed file under `changes/` passes.
#[test]
fn a_case_variant_or_oversized_entry_file_is_named() {
    let repo = Repo::new(CHANGELOG, &[]);
    repo.write("Changes/fixed/sa-x.md", "- **X.** Fine.\n");
    let found = repo.judged();
    assert!(
        found.iter().any(|f| f.contains("spelled exactly changes/")),
        "{found:?}"
    );
    std::fs::remove_dir_all(repo.root.join("Changes")).unwrap();
    repo.write("changes", "- **X.** Fine.\n");
    let found = repo.judged();
    assert!(
        found
            .iter()
            .any(|f| f.starts_with("changes: ") && f.contains("spelled exactly")),
        "{found:?}"
    );
    std::fs::remove_file(repo.root.join("changes")).unwrap();
    let big = repo.root.join("changes/fixed/sa-big.md");
    std::fs::create_dir_all(big.parent().unwrap()).unwrap();
    std::fs::write(
        &big,
        vec![0xFF; crate::changelog_fragments::MAX_FRAGMENT_BYTES + 1],
    )
    .unwrap();
    let found = repo.judged();
    assert!(
        found
            .iter()
            .any(|f| f.starts_with("changes/fixed/sa-big.md: is ") && f.contains("over the")),
        "{found:?}"
    );
    std::fs::remove_file(&big).unwrap();
    repo.write("changes/fixed/sa-x.md", "- **X.** Fine.\n");
    assert_eq!(repo.judged(), Vec::<String>::new());
}

/// The release owns its window: under `release` nothing here is selected.
#[test]
fn a_release_leaves_the_window_to_its_own_gate() {
    let repo = Repo::new(CHANGELOG, &[]);
    repo.write(CHANGELOG_FILE, &format!("- **Stray.**\n\n{CHANGELOG}"));
    let mut ctx = repo.ctx();
    assert!(selected(&ctx).unwrap());
    ctx.release = true;
    assert!(judge(&ctx).unwrap().is_none());
}
