// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a Bash read reaches past the words it names: every file below a tree a reader descends,
//! where a link below it leads, and what git prints from its object store or index
//! (`docs/measured/classifier-tree-reach-2026-11-05.md`).

use super::*;
#[cfg(unix)]
use crate::supervise::policy::{LiveProbe, PolicyProbe};

/// Each of `files` (relative to `root`) planted with a line holding `token`, its folders made.
#[cfg(unix)]
fn plant(root: &Path, files: &[&str]) {
    for f in files {
        let path = root.join(f);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a folder");
        std::fs::write(&path, "token\n").expect("a file");
    }
}

/// A Bash box's context run in the real folder `cwd`, the default rules under `home`, under a
/// bash startup proven to leave every shell glob off a leading dot (zsh's reading refuses find's
/// `{}` before any read is weighed).
#[cfg(unix)]
fn standing_in(cwd: &Path, home: &Path) -> ApprovalCtx {
    ApprovalCtx {
        secrets: default_secrets(Some(home)),
        home: Some(home.to_path_buf()),
        cwd: cwd.to_path_buf(),
        shell: Ok(ShellStartup::default()),
        ..ctx()
    }
}

/// The decision on `line` drawn in a folder box under `c`.
#[cfg(unix)]
fn decided(line: &str, c: &ApprovalCtx) -> Decision {
    on_screen(&folder_box_with(line), c)
}

/// Every reader that descends a tree, each over `t` (`twin` the other tree a comparison takes).
#[cfg(unix)]
fn descending(t: &str) -> Vec<String> {
    [
        "grep -r token {t}",
        "grep -R token {t}",
        "grep --recursive token {t}",
        "grep -d recurse token {t}",
        "rg token {t}",
        "find {t} -exec cat {} +",
        "diff -r {t} twin",
        "git diff --no-index {t} twin",
        "git grep --no-index token -- {t}",
    ]
    .iter()
    .map(|line| line.replace("{t}", t))
    .collect()
}

/// A reader that descends a tree opens every file below it, so a file a basename or component
/// rule names anywhere below the tree is a read of a secret though no word names it and no
/// home-rooted rule lies above it: the tree is walked on the disk (MEASURED 2026-11-05 on macOS
/// 27.0: `grep -r`, `grep -R`, `diff -r` and `find … -exec cat {} +` printed `a/.ssh/id_rsa`,
/// `a/.env` and `a/src/k.pem`, and rg did with `--hidden`). A plain `diff` compares one level,
/// so a secret deeper than that is no read of it; a tree holding none, or nothing at its path,
/// is read.
#[cfg(unix)]
#[test]
fn a_tree_a_reader_descends_is_walked_for_every_secret_rule() {
    let dir = crate::supervise::ScratchDir::new("approval-reach", "walk");
    let root = crate::supervise::test_canonical(&dir);
    plant(&root, &["clean/x.rs", "clean/src/y.rs", "twin/x.rs"]);
    plant(&root, &["comp/x.rs", "comp/deep/.ssh/config"]);
    plant(&root, &["base/x.rs", "base/src/k.pem"]);
    plant(&root, &["top/x.rs", "top/.env"]);
    let c = standing_in(&root, Path::new(owner_dirs().0));
    for (tree, met) in [("comp", ".ssh"), ("base", "k.pem"), ("top", ".env")] {
        for line in descending(tree) {
            let r = reason(&decided(&line, &c)).to_string();
            assert!(
                r.contains("a read of a secret") && r.contains(met),
                "{line}: {r}"
            );
        }
    }
    let r = reason(&decided("diff top twin", &c)).to_string();
    assert!(
        r.contains("a read of a secret") && r.contains(".env"),
        "{r}"
    );
    // NEGATIVE CONTROL: a tree holding no secret, nothing at the path, and a secret below the
    // one level a plain diff compares.
    for line in descending("clean")
        .into_iter()
        .chain(descending("gone"))
        .chain(["diff comp twin".to_string(), "diff base twin".to_string()])
    {
        let d = decided(&line, &c);
        assert_eq!(approved(&d), Some(RULE_READ_ONLY), "{line}: {d:?}");
    }
}

/// A glob a reader descends from is walked from the folder its literal components name, any
/// depth below: `grep -r token sr?` in a folder whose `src` holds a key reads it, though no name
/// the glob may expand to is a secret's.
#[cfg(unix)]
#[test]
fn a_glob_a_reader_descends_is_walked_from_its_literal_folder() {
    let dir = crate::supervise::ScratchDir::new("approval-reach", "glob");
    let root = crate::supervise::test_canonical(&dir);
    plant(
        &root,
        &[
            "base/x.rs",
            "base/src/k.pem",
            "clean/x.rs",
            "clean/src/y.rs",
        ],
    );
    let home = Path::new(owner_dirs().0);
    let line = "grep -r token sr?";
    let r = reason(&decided(line, &standing_in(&root.join("base"), home))).to_string();
    assert!(r.contains("k.pem"), "{r}");
    // NEGATIVE CONTROL
    let d = decided(line, &standing_in(&root.join("clean"), home));
    assert_eq!(approved(&d), Some(RULE_READ_ONLY), "{d:?}");
}

/// A link below a tree is followed, as some reader follows it (MEASURED 2026-11-05 on macOS
/// 27.0: `grep -rS token d` printed `d/notes.txt:token rsa` through `d/notes.txt ->
/// ../a/.ssh/id_rsa`, and `find d -exec cat {} +` printed it too): a file link whose target a
/// basename rule names, a folder link into a component rule's folder, and a link into a
/// home-rooted rule's file are each a read of a secret. A link to a plain file, a link that
/// names nothing, and a link back up the tree are read.
#[cfg(unix)]
#[test]
fn a_link_below_a_tree_is_followed_where_some_reader_follows_it() {
    use std::os::unix::fs::symlink;
    let dir = crate::supervise::ScratchDir::new("approval-reach", "link");
    let root = crate::supervise::test_canonical(&dir);
    plant(
        &root,
        &[
            "vault/id_rsa",
            "vault/.ssh/config",
            "vault/plain.txt",
            "home/.claude.json",
            "flink/x.rs",
            "dlink/x.rs",
            "ulink/x.rs",
            "plink/x.rs",
            "dangle/x.rs",
            "cycle/x.rs",
        ],
    );
    let link = |target: &str, at: &str| symlink(target, root.join(at)).expect("a link");
    link("../vault/id_rsa", "flink/notes.txt");
    link("../vault/.ssh", "dlink/keys");
    link("../home/.claude.json", "ulink/c.json");
    link("../vault/plain.txt", "plink/p.txt");
    link("../nowhere", "dangle/n.txt");
    link(".", "cycle/self");
    let c = standing_in(&root, &root.join("home"));
    for (tree, met) in [
        ("flink", "id_rsa"),
        ("dlink", ".ssh"),
        ("ulink", ".claude.json"),
    ] {
        for line in [
            format!("grep -r token {tree}"),
            format!("find {tree} -exec cat {{}} +"),
        ] {
            let r = reason(&decided(&line, &c)).to_string();
            assert!(
                r.contains("a read of a secret") && r.contains(met),
                "{line}: {r}"
            );
        }
    }
    // NEGATIVE CONTROL
    for tree in ["plink", "dangle", "cycle"] {
        let line = format!("grep -r token {tree}");
        let d = decided(&line, &c);
        assert_eq!(approved(&d), Some(RULE_READ_ONLY), "{line}: {d:?}");
    }
}

/// A walk that is cut proves nothing: a folder below the tree that cannot be listed leaves the
/// tree unproven, and the same tree, listed, is read.
#[cfg(unix)]
#[test]
fn a_tree_whose_walk_is_cut_is_unproven() {
    use std::os::unix::fs::PermissionsExt;
    let dir = crate::supervise::ScratchDir::new("approval-reach", "cut");
    let root = crate::supervise::test_canonical(&dir);
    plant(&root, &["t/x.rs", "t/locked/y.rs"]);
    let locked = root.join("t/locked");
    let mode = |m: u32| {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(m)).expect("a mode");
    };
    let c = standing_in(&root, Path::new(owner_dirs().0));
    mode(0o000);
    let d = decided("grep -r token t", &c);
    mode(0o755);
    assert!(reason(&d).contains("cannot be listed"), "{d:?}");
    // NEGATIVE CONTROL
    let d = decided("grep -r token t", &c);
    assert_eq!(approved(&d), Some(RULE_READ_ONLY), "{d:?}");
}

/// A git read that prints a file git reads from its object store or index, whose path no word
/// bounds, is unproven: a walk of the disk lists none of it (MEASURED 2026-11-05 with git 2.56.0
/// in a repository that tracked `a/.ssh/id_rsa` and `.env`: `git log -p`, `git show`, `git diff
/// HEAD~2`, `git diff`, `git cat-file -p HEAD~1:a/.ssh/id_rsa` and `git grep token HEAD~1` each
/// printed a line of one, as did `git status -v`, `git show --stat <blob>` and `git for-each-ref
/// --format='%(raw)'` over a tag naming a blob). A names-only format, a commit's own fields, a
/// `REV:path` held to the rules, and a grep of the worktree's paths are read.
#[test]
fn a_git_read_of_its_object_store_is_refused_where_no_word_bounds_its_files() {
    let (_, proj) = owner_dirs();
    for line in [
        "git log -p",
        "git log --patch -1",
        "git log -U3",
        "git log --stat -p",
        "git log -1 -ps",
        "git log --cc",
        "git show",
        "git show HEAD^",
        "git show 1a2b3c4d",
        "git show --stat 1a2b3c4d",
        "git show --oneline HEAD",
        "git diff",
        "git diff HEAD^",
        "git diff --cached",
        "git diff --check",
        "git cat-file -p 1a2b3c4d",
        "git cat-file blob 1a2b3c4d",
        "git cat-file --batch",
        "git cat-file --batch-co",
        "git grep --cached token",
        "git grep --cach token",
        "git grep -e token HEAD -- src",
        "git grep token HEAD:src",
        "git stash list -p",
        "git status -v",
        "git status --verb",
    ] {
        escalates_in(&[(proj, line)], "cannot list");
    }
    // A ref format's `%(…)` is refused before this as a word that may expand to any name, so its
    // `raw` atom is judged here by [`object_read`] alone.
    let judged = |line: &str| {
        let seg = &crate::supervise::classify::command_words(line).expect(line)[0];
        object_read(seg, 0)
    };
    for line in [
        "git for-each-ref --format='%(raw)'",
        "git tag -l --format='%(*raw)'",
    ] {
        assert!(
            judged(line).is_some_and(|w| w.contains("cannot list")),
            "{line}"
        );
    }
    for line in [
        "git for-each-ref --format='%(refname)'",
        "git for-each-ref --format='%(raw:size)'",
    ] {
        assert_eq!(judged(line), None, "{line}");
    }
    // NEGATIVE CONTROL
    reads_in(&[
        (proj, "git log --oneline"),
        (proj, "git log --stat -3"),
        (proj, "git log -S token --oneline"),
        (proj, "git show --stat"),
        (proj, "git show --stat HEAD^"),
        (proj, "git show -s --format=%B HEAD^"),
        (proj, "git show HEAD:README.md"),
        (proj, "git diff --stat"),
        (proj, "git diff --name-only HEAD^2"),
        (proj, "git cat-file -t HEAD"),
        (proj, "git cat-file -p HEAD:README.md"),
        (proj, "git cat-file --batch-check"),
        (proj, "git grep -n TODO -- src"),
        (proj, "git stash list"),
        (proj, "git status -s"),
        (proj, "git status --porcelain=v2"),
        (proj, "git branch -v"),
    ]);
}

/// A path git reads from its repository's top (`REV:path`, `:path`, a pathspec's `top` magic)
/// is read from every directory above where git runs, as the top is one of them (MEASURED
/// 2026-11-05 with git 2.56.0: `git show HEAD:x`, run in a repository's subdirectory, printed the
/// top's `x`): in `~/proj`, `HEAD:.claude.json` may be the home's. Led by `./`, the path is read
/// from where git runs.
#[test]
fn a_path_git_reads_from_its_top_is_read_from_every_directory_above() {
    let (_, proj) = owner_dirs();
    escalates_in(
        &[
            (proj, "git show HEAD:.claude.json"),
            (proj, "git cat-file -p HEAD:.config/gh/hosts.yml"),
            (proj, "git show :.secrets/t"),
            (proj, "git log --oneline -- ':(top).claude.json'"),
        ],
        "which git reads from its repository's top",
    );
    // NEGATIVE CONTROL
    reads_in(&[
        (proj, "git show HEAD:README.md"),
        (proj, "git show HEAD:./.claude.json"),
        (proj, "git log --oneline -- ':(top)src'"),
    ]);
}

/// A pathspec's long magic is no part of the path it names (`:(icase)src` names `src`, read as a
/// tree, never a component holding `(`), and a git grep word git reads as its pattern is no
/// pathspec, so `git grep ':/x' src` reads `src` alone. With no `--`, a word past the pattern
/// that names nothing on the disk may be a revision, whose files git reads from its object store
/// (MEASURED 2026-11-05 with git 2.56.0: `git grep token src2`, `src2` a branch and no path,
/// printed the branch's `a/.ssh/id_rsa`), and `:/x` names one by a commit's message. A git told
/// to read its pathspecs literally or in any case reads none as this check reads it.
#[cfg(unix)]
#[test]
fn a_pathspec_magic_and_a_git_grep_pattern_name_no_path() {
    let dir = crate::supervise::ScratchDir::new("approval-reach", "magic");
    let root = crate::supervise::test_canonical(&dir);
    plant(&root, &["src/x.rs"]);
    let c = standing_in(&root, Path::new(owner_dirs().0));
    for line in [
        "git grep token -- ':(icase)src'",
        "git grep token -- ':(glob)src/*.rs'",
        "git grep token -- ':(literal)src'",
        "git log --oneline -- ':(icase)src'",
        "git diff --stat -- ':(glob)src/*.rs'",
        "git grep ':/x' src",
        "git grep -e ':/x' src",
        "git grep -n token src",
    ] {
        let d = decided(line, &c);
        assert_eq!(approved(&d), Some(RULE_READ_ONLY), "{line}: {d:?}");
    }
    for (line, why) in [
        ("git grep token ':/x'", "as a revision"),
        ("git grep token HEAD^", "as a revision"),
        ("git grep -n token gone", "as a revision"),
        ("git grep token 'HEAD^{/fix*}'", "as a revision"),
        ("git grep token -- ':(top)src'", "cannot place"),
        (
            "git --literal-pathspecs grep token -- ':(icase)src'",
            "otherwise than this check reads it",
        ),
        (
            "git --icase-pathspecs log --oneline -- src",
            "otherwise than this check reads it",
        ),
    ] {
        let r = reason(&decided(line, &c)).to_string();
        assert!(r.contains(why), "{line}: {r}");
    }
}

/// [`git_tree_path`] reads the path a git word names past what leads it, and whether git reads it
/// from its repository's top.
#[test]
fn a_git_word_names_its_path_past_its_revision_or_magic() {
    for (word, read) in [
        ("HEAD:a/b", Some(("a/b", true))),
        ("HEAD~2:./a", Some(("./a", false))),
        ("v1:../a", Some(("../a", false))),
        (":a", Some(("a", true))),
        (":0:a", Some(("a", true))),
        (":2:a", Some(("a", true))),
        (":(icase)src", Some(("src", false))),
        (":(glob,top)src/*", Some(("src/*", true))),
        (":(icase", Some(("(icase", false))),
        (":/src", Some(("src", true))),
        (":!/src", Some(("src", true))),
        (":!src", Some(("src", false))),
        (":^:src", Some(("src", false))),
        ("src", None),
    ] {
        assert_eq!(git_tree_path(word), read, "{word}");
    }
}

/// The advisory word looks at no disk, so a tree a reader descends is unproven there; `aterm
/// drive classify`, asked from a real folder, walks it as the harness does.
#[cfg(unix)]
#[test]
fn the_advisory_word_lists_no_tree_and_the_classify_verb_walks_one() {
    let dir = crate::supervise::ScratchDir::new("approval-reach", "word");
    let root = crate::supervise::test_canonical(&dir);
    plant(&root, &["clean/x.rs", "top/.env"]);
    let home = Path::new(owner_dirs().0);
    let r = readings_read_only_at(&["grep -r token /etc".to_string()], &[], Some(home))
        .expect_err("unlisted");
    assert!(r.contains("does not list"), "{r}");
    let walk = |root: &[String], below: usize, secrets: &[SecretRule]| {
        LiveProbe.below_tree(root, below, secrets, Some(home))
    };
    let verb = |line: &str| command_read_only_at(line, &[], Some(home), Some(&root), &walk);
    let r = verb("grep -r token top").expect_err("a secret below");
    assert!(r.contains(".env"), "{r}");
    // NEGATIVE CONTROL
    assert_eq!(verb("grep -r token clean"), Ok(()));
    assert_eq!(verb("cat clean/x.rs"), Ok(()));
}
