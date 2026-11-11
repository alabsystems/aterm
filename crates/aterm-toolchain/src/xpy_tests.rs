// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::symlink;

fn checkout(root: &Path) -> PathBuf {
    let repo = root.join("trust");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join(XPY), b"#!/usr/bin/env python3\n").unwrap();
    std::fs::canonicalize(repo).unwrap()
}

/// A real `build/` is the build tree, and so is exactly the sanctioned link, walked where it
/// leads. NEGATIVE CONTROLS: a dangling link, a link to another folder of the checkout, a
/// link to a folder outside it (another volume's place), a `build.noindex` that is itself a
/// link, and a link spelled through another path to `build.noindex` that does not resolve to
/// the sibling are no build tree.
#[test]
fn the_build_tree_is_a_real_build_or_exactly_the_sanctioned_link() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    assert_eq!(build_tree(&repo), None, "no build/: no tree");

    std::fs::create_dir(repo.join(BUILD)).unwrap();
    assert_eq!(build_tree(&repo), Some(repo.join(BUILD)), "a real build/");
    assert!(!sanctioned_build_link(&repo), "a real directory is no link");
    std::fs::remove_dir(repo.join(BUILD)).unwrap();

    std::fs::create_dir(repo.join(BUILD_NOINDEX)).unwrap();
    symlink(BUILD_NOINDEX, repo.join(BUILD)).unwrap();
    assert!(sanctioned_build_link(&repo));
    assert_eq!(build_tree(&repo), Some(repo.join(BUILD_NOINDEX)));
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink(repo.join(BUILD_NOINDEX), repo.join(BUILD)).unwrap();
    assert_eq!(
        build_tree(&repo),
        Some(repo.join(BUILD_NOINDEX)),
        "an absolute spelling of the same link"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();

    symlink("build.missing", repo.join(BUILD)).unwrap();
    assert_eq!(build_tree(&repo), None, "a dangling link");
    std::fs::remove_file(repo.join(BUILD)).unwrap();

    std::fs::create_dir(repo.join("elsewhere")).unwrap();
    symlink("elsewhere", repo.join(BUILD)).unwrap();
    assert_eq!(build_tree(&repo), None, "a link to another folder");
    std::fs::remove_file(repo.join(BUILD)).unwrap();

    let outside = tmp.path().join("shared/build.noindex");
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, repo.join(BUILD)).unwrap();
    assert_eq!(
        build_tree(&repo),
        None,
        "a link to a build.noindex outside the checkout"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();

    std::fs::remove_dir(repo.join(BUILD_NOINDEX)).unwrap();
    symlink(&outside, repo.join(BUILD_NOINDEX)).unwrap();
    symlink(BUILD_NOINDEX, repo.join(BUILD)).unwrap();
    assert!(!sanctioned_build_link(&repo));
    assert_eq!(
        build_tree(&repo),
        None,
        "a build.noindex that is itself a link"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    std::fs::remove_file(repo.join(BUILD_NOINDEX)).unwrap();

    // The kernel takes `..` after following the link before it: `hop/../build.noindex` is
    // `elsewhere/build.noindex`, outside, though its words name the sibling.
    std::fs::create_dir(repo.join(BUILD_NOINDEX)).unwrap();
    std::fs::create_dir_all(tmp.path().join("elsewhere/sub")).unwrap();
    std::fs::create_dir_all(tmp.path().join("elsewhere/build.noindex")).unwrap();
    symlink("../elsewhere/sub", repo.join("hop")).unwrap();
    symlink("hop/../build.noindex", repo.join(BUILD)).unwrap();
    assert!(!sanctioned_build_link(&repo), "through `..`");
    assert_eq!(build_tree(&repo), None, "through `..`");
}

/// A folder inside a build tree, by either name, is held by it; the checkout and its other
/// folders are not. NEGATIVE CONTROL: a `build` or `build.noindex` whose parent holds no
/// `x.py` (or only a link named `x.py`) holds nothing.
#[test]
fn a_build_tree_holds_the_folders_inside_it_by_either_name() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    for name in [BUILD, BUILD_NOINDEX] {
        let tree = repo.join(name);
        let stage = tree.join("aarch64-apple-darwin/stage1-rustc");
        assert_eq!(
            build_holding(&stage),
            Some((tree.as_path(), name)),
            "{name}"
        );
        assert_eq!(build_holding(&tree), Some((tree.as_path(), name)), "{name}");
    }
    assert_eq!(build_holding(&repo), None);
    assert_eq!(build_holding(&repo.join("src/build/x")), None);

    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(build_holding(&plain.join("build/x")), None);
    symlink(repo.join(XPY), plain.join(XPY)).unwrap();
    assert!(!is_checkout(&plain), "a link named x.py");
    assert_eq!(build_holding(&plain.join("build.noindex/x")), None);
}

/// A real `build.noindex` that no sanctioned link leads to is stray: beside a fresh real
/// `build/`, alone, or beside any other link. NEGATIVE CONTROLS: the sanctioned link's own
/// target, a `build.noindex` that is itself a link, and none at all are no stray tree.
#[test]
fn a_build_noindex_no_sanctioned_link_leads_to_is_stray() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    let noindex = repo.join(BUILD_NOINDEX);
    assert_eq!(stray_noindex(&repo), None, "none at all");
    std::fs::create_dir(&noindex).unwrap();
    assert_eq!(stray_noindex(&repo), Some(noindex.clone()), "alone");
    std::fs::create_dir(repo.join(BUILD)).unwrap();
    assert_eq!(
        stray_noindex(&repo),
        Some(noindex.clone()),
        "beside a real build/"
    );
    std::fs::remove_dir(repo.join(BUILD)).unwrap();
    symlink("elsewhere", repo.join(BUILD)).unwrap();
    assert_eq!(
        stray_noindex(&repo),
        Some(noindex.clone()),
        "beside another link"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink(BUILD_NOINDEX, repo.join(BUILD)).unwrap();
    assert_eq!(stray_noindex(&repo), None, "the sanctioned link's target");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    // Links that may lead to it by a spelling not read: through another link inside the
    // checkout, through `..`, and an absolute one by another spelling of the checkout.
    symlink(BUILD_NOINDEX, repo.join("hop")).unwrap();
    symlink("hop", repo.join(BUILD)).unwrap();
    assert!(unread_link(&repo), "through another link");
    assert_eq!(stray_noindex(&repo), None, "through another link");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink("hop/../build.noindex", repo.join(BUILD)).unwrap();
    assert!(unread_link(&repo), "through `..`");
    assert_eq!(stray_noindex(&repo), None, "through `..`");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    std::fs::remove_file(repo.join("hop")).unwrap();
    let spelled = tmp.path().join("trust").join(BUILD_NOINDEX);
    if !spelled.starts_with(&repo) {
        symlink(&spelled, repo.join(BUILD)).unwrap();
        assert!(unread_link(&repo), "another spelling of the checkout");
        assert_eq!(
            stray_noindex(&repo),
            None,
            "another spelling of the checkout"
        );
        std::fs::remove_file(repo.join(BUILD)).unwrap();
    }
    symlink("build.gone", repo.join(BUILD)).unwrap();
    assert!(!unread_link(&repo), "a link to nothing is read");
    assert_eq!(
        stray_noindex(&repo),
        Some(noindex.clone()),
        "beside a link to nothing"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    std::fs::remove_dir(&noindex).unwrap();
    let outside = tmp.path().join("shared");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, &noindex).unwrap();
    assert_eq!(stray_noindex(&repo), None, "a build.noindex that is a link");
    std::fs::remove_file(&noindex).unwrap();

    // On a case-insensitive volume (a Mac's default), `build -> Build.noindex` leads to the
    // folder `build.noindex` names, by a spelling Trust does not sanction: no stray.
    let cased = repo.join("Build.noindex");
    std::fs::create_dir(&cased).unwrap();
    if std::fs::symlink_metadata(&noindex).is_ok() {
        symlink("Build.noindex", repo.join(BUILD)).unwrap();
        assert!(
            !sanctioned_build_link(&repo),
            "Trust sanctions no other spelling"
        );
        assert_eq!(build_tree(&repo), None);
        assert_eq!(stray_noindex(&repo), None, "build leads there");
        assert_eq!(linked_tree(&repo), Some(cased.clone()));
        std::fs::remove_file(repo.join(BUILD)).unwrap();
        assert_eq!(
            stray_noindex(&repo),
            Some(cased),
            "with no link it is stray"
        );
    }
}

/// The folder a `build` link leads to inside the checkout, sanctioned or not, as the volume
/// spells it, by the link's words. NEGATIVE CONTROLS: a real `build/`, a dangling link, a
/// link to a file, one to a folder outside the checkout, one through a link inside it and
/// one through `..` lead nowhere.
#[test]
fn a_build_link_leads_to_the_folder_bootstrap_builds_in() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    assert_eq!(linked_tree(&repo), None, "no build");
    std::fs::create_dir(repo.join(BUILD)).unwrap();
    assert_eq!(linked_tree(&repo), None, "a real build/");
    std::fs::remove_dir(repo.join(BUILD)).unwrap();
    std::fs::create_dir(repo.join("build.local")).unwrap();
    symlink("build.local", repo.join(BUILD)).unwrap();
    assert_eq!(linked_tree(&repo), Some(repo.join("build.local")));
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    std::fs::create_dir(repo.join(BUILD_NOINDEX)).unwrap();
    symlink(BUILD_NOINDEX, repo.join(BUILD)).unwrap();
    assert_eq!(
        linked_tree(&repo),
        Some(repo.join(BUILD_NOINDEX)),
        "sanctioned"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink("build.gone", repo.join(BUILD)).unwrap();
    assert_eq!(linked_tree(&repo), None, "a dangling link");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    std::fs::write(repo.join("file"), b"").unwrap();
    symlink("file", repo.join(BUILD)).unwrap();
    assert_eq!(linked_tree(&repo), None, "a link to a file");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, repo.join(BUILD)).unwrap();
    assert_eq!(linked_tree(&repo), None, "a folder outside the checkout");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink(&outside, repo.join("hop")).unwrap();
    symlink("hop", repo.join(BUILD)).unwrap();
    assert_eq!(linked_tree(&repo), None, "through a link inside it");
    assert!(unread_link(&repo), "through a link inside it");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink("./build.local", repo.join(BUILD)).unwrap();
    assert_eq!(
        linked_tree(&repo),
        Some(repo.join("build.local")),
        "`./` read by its words"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    std::fs::create_dir(repo.join("src")).unwrap();
    symlink("src/../build.local", repo.join(BUILD)).unwrap();
    assert_eq!(linked_tree(&repo), None, "through `..`");
    assert!(unread_link(&repo), "through `..`");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink("..", repo.join(BUILD)).unwrap();
    assert_eq!(linked_tree(&repo), None, "the checkout's parent");
    assert!(unread_link(&repo), "the checkout's parent");
    assert_eq!(named_folder(&repo), None, "the checkout's parent");
}

/// The folder a `build` link names inside the checkout, at any depth, by its words alone,
/// whether or not it is there. NEGATIVE CONTROLS: no link, a real `build/`, an absolute
/// target elsewhere, a `..` and an empty `.` name no folder.
#[test]
fn a_build_link_names_a_folder_inside_the_checkout_by_its_words() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    assert_eq!(named_folder(&repo), None, "no build");
    std::fs::create_dir(repo.join(BUILD)).unwrap();
    assert_eq!(named_folder(&repo), None, "a real build/");
    std::fs::remove_dir(repo.join(BUILD)).unwrap();
    for (words, folder) in [
        ("build.local", Some("build.local")),
        ("out/tree", Some("out/tree")),
        ("./out/./tree/", Some("out/tree")),
        ("out/../tree", None),
        (".", None),
    ] {
        symlink(words, repo.join(BUILD)).unwrap();
        assert_eq!(named_folder(&repo), folder.map(|f| repo.join(f)), "{words}");
        std::fs::remove_file(repo.join(BUILD)).unwrap();
    }
    symlink(repo.join("out/tree"), repo.join(BUILD)).unwrap();
    assert_eq!(
        named_folder(&repo),
        Some(repo.join("out/tree")),
        "the checkout's own absolute spelling"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink(tmp.path().join("elsewhere/tree"), repo.join(BUILD)).unwrap();
    assert_eq!(named_folder(&repo), None, "an absolute target elsewhere");
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    // The checkout spelled one way and the link the other (macOS's `/var` and `/private/var`).
    let spelled = tmp.path().join("trust");
    if spelled != repo {
        symlink(repo.join("out/tree"), repo.join(BUILD)).unwrap();
        assert_eq!(
            named_folder(&spelled),
            Some(spelled.join("out/tree")),
            "its canonical spelling, under another"
        );
        std::fs::remove_file(repo.join(BUILD)).unwrap();
    }
}

/// The folder a `build` link names, with each name as the volume stores it, and only through
/// real folders. NEGATIVE CONTROLS: a folder not there, and one reached through a link on
/// the way, give none.
#[test]
fn a_named_folder_is_spelled_as_the_volume_stores_it_and_only_through_real_folders() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    symlink("out/tree", repo.join(BUILD)).unwrap();
    assert_eq!(named_tree(&repo), None, "not there");
    std::fs::create_dir_all(repo.join("out/tree")).unwrap();
    assert_eq!(named_tree(&repo), Some(repo.join("out/tree")));
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    symlink("Out/Tree", repo.join(BUILD)).unwrap();
    let blind = std::fs::symlink_metadata(repo.join("OUT")).is_ok();
    assert_eq!(
        named_tree(&repo),
        blind.then(|| repo.join("out/tree")),
        "another case, on a case-blind volume: {blind}"
    );
    std::fs::remove_file(repo.join(BUILD)).unwrap();
    std::fs::create_dir_all(tmp.path().join("away/tree")).unwrap();
    symlink(tmp.path().join("away"), repo.join("hop")).unwrap();
    symlink("hop/tree", repo.join(BUILD)).unwrap();
    assert_eq!(named_folder(&repo), Some(repo.join("hop/tree")));
    assert_eq!(named_tree(&repo), None, "a link on the way");
}

/// A tree's name in another case is the tree only where the volume takes it for the same
/// entry: on a Mac's default volume `Build` is `build`; on a case-sensitive one it is
/// another folder. NEGATIVE CONTROLS: other names are no tree's, in any case.
#[test]
fn a_tree_name_in_another_case_is_the_tree_only_where_the_volume_says_so() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    assert_eq!(
        tree_name(&repo, OsStr::new(BUILD)),
        Some(BUILD),
        "as spelled"
    );
    assert_eq!(
        tree_name(&repo, OsStr::new(BUILD_NOINDEX)),
        Some(BUILD_NOINDEX)
    );
    assert_eq!(tree_name(&repo, OsStr::new("build.local")), None);
    assert_eq!(tree_name(&repo, OsStr::new("Builds")), None);
    let upper = repo.join("Build.NOINDEX");
    std::fs::create_dir(&upper).unwrap();
    let blind = std::fs::symlink_metadata(repo.join(BUILD_NOINDEX)).is_ok();
    assert_eq!(
        tree_name(&repo, OsStr::new("Build.NOINDEX")),
        blind.then_some(BUILD_NOINDEX),
        "case-blind volume: {blind}"
    );
    assert_eq!(
        build_holding(&upper.join("aarch64-apple-darwin")),
        blind.then_some((upper.as_path(), BUILD_NOINDEX))
    );
    assert_eq!(tree_name(&repo, OsStr::new("BUILD")), None, "no such entry");

    // Two entries that both answer are one only when they are one, on any volume.
    std::fs::create_dir(repo.join("one")).unwrap();
    std::fs::create_dir(repo.join("two")).unwrap();
    assert!(same_entry(&repo.join("one"), &repo.join("one")));
    assert!(
        !same_entry(&repo.join("one"), &repo.join("two")),
        "two folders"
    );
    assert!(
        !same_entry(&repo.join("one"), &repo.join("none")),
        "one missing"
    );
}

/// A checkout whose clean step defines Trust's link rule supports the layout by hand.
/// NEGATIVE CONTROLS: none, an upstream clean step without it, and a folder by its name.
#[test]
fn only_a_clean_step_with_the_link_rule_supports_the_layout() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    assert!(!supports_noindex_layout(&repo), "none");
    let clean = repo.join(CLEAN_STEP);
    std::fs::create_dir_all(&clean).unwrap();
    assert!(!supports_noindex_layout(&repo), "a folder by its name");
    std::fs::remove_dir(&clean).unwrap();
    std::fs::write(&clean, b"fn rm_rf(path: &Path) {}\n").unwrap();
    assert!(!supports_noindex_layout(&repo), "upstream's");
    std::fs::write(
        &clean,
        b"fn rm_rf_build_dir(out: &Path) {}\nfn is_sanctioned_build_link(out: &Path) -> bool {}\n",
    )
    .unwrap();
    assert!(supports_noindex_layout(&repo));
}

/// Only a checkout that holds Trust's noindex-layout module moves its build tree.
/// NEGATIVE CONTROL: a folder by that name is no module.
#[test]
fn only_a_checkout_with_the_layout_module_moves_its_build_tree() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let repo = checkout(tmp.path());
    assert!(!moves_build_aside(&repo));
    let module = repo.join(NOINDEX_LAYOUT);
    std::fs::create_dir_all(&module).unwrap();
    assert!(!moves_build_aside(&repo), "a folder by the module's name");
    std::fs::remove_dir(&module).unwrap();
    std::fs::write(&module, b"//! moves build/ aside\n").unwrap();
    assert!(moves_build_aside(&repo));
}
