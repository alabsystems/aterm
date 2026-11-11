// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A store that holds every shape [`spotlight_exposed_builds`] tells apart.
#[cfg(unix)]
#[test]
fn exposed_builds_are_the_plain_sysroot_folders_and_say_which_is_current() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("atc-layout-exposed-")
        .tempdir()
        .expect("a scratch prefix");
    let prefix = scratch.path();
    let sysroot = |program: &str, name: &str| {
        let tree = prefix.join(STORE_DIR).join(program).join(name);
        std::fs::create_dir_all(tree.join("lib/rustlib/aarch64-apple-darwin/lib")).unwrap();
        std::fs::create_dir_all(tree.join("bin")).unwrap();
        tree
    };
    let trust = prefix.join(STORE_DIR).join("trust");
    sysroot("trust", "9192");
    sysroot("trust", "15698");
    sysroot("trust", "20001.noindex");
    std::os::unix::fs::symlink("20001.noindex", trust.join("20001")).unwrap();
    std::os::unix::fs::symlink(trust.join("15698"), trust.join("current")).unwrap();
    sysroot("trust-mc", "7");
    std::fs::create_dir_all(prefix.join(STORE_DIR).join("claude/31/bin")).unwrap();
    std::fs::write(trust.join("9192.ready"), "ok\n").unwrap();
    std::fs::create_dir_all(trust.join("0812/lib/rustlib")).unwrap();

    let found: Vec<(String, u64, bool)> = spotlight_exposed_builds(prefix)
        .into_iter()
        .map(|b| {
            assert_eq!(b.dir, build_dir(prefix, &b.program, b.build));
            (b.program, b.build, b.active)
        })
        .collect();
    assert_eq!(
        found,
        [
            ("trust".to_owned(), 9192, false),
            ("trust".to_owned(), 15698, true),
            ("trust-mc".to_owned(), 7, false),
        ],
        "a link is moved, a build with no sysroot is not Rust's, and a name the store \
         never writes is not a build"
    );
    assert!(is_linked_aside(&trust.join("20001")));
    assert!(!is_linked_aside(&trust.join("9192")) && !is_linked_aside(&trust.join("current")));
    assert_eq!(
        aside_of(&trust.join("9192")),
        Some(trust.join("9192.noindex"))
    );

    std::fs::remove_file(trust.join("current")).unwrap();
    std::os::unix::fs::symlink("20001", trust.join("current")).unwrap();
    assert!(
        spotlight_exposed_builds(prefix).iter().all(|b| !b.active),
        "current names the moved build"
    );
}

/// The views root holds every shape [`spotlight_exposed_views`] tells apart.
#[cfg(unix)]
#[test]
fn exposed_views_are_the_plain_sysroot_views() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("atc-layout-views-")
        .tempdir()
        .expect("a scratch prefix");
    let prefix = scratch.path();
    assert!(
        spotlight_exposed_views(prefix).is_empty(),
        "no views at all"
    );
    let views = prefix.join(VIEWS_DIR);
    let view = |name: &str| {
        let tree = views.join(name);
        std::fs::create_dir_all(tree.join("lib/rustlib/aarch64-apple-darwin/lib")).unwrap();
        std::fs::create_dir_all(tree.join("bin")).unwrap();
        tree
    };
    view("trust");
    view("trust-dev.noindex");
    std::os::unix::fs::symlink("trust-dev.noindex", views.join("trust-dev")).unwrap();
    view(".trust.old-4242");
    let checkout = prefix.join("checkout/lib/rustlib");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::create_dir_all(views.join("linked")).unwrap();
    std::os::unix::fs::symlink(prefix.join("checkout/lib"), views.join("linked/lib")).unwrap();

    assert_eq!(
        spotlight_exposed_views(prefix),
        [ExposedView {
            name: "trust".to_owned(),
            dir: views.join("trust"),
        }],
        "a link is moved, debris and asides are no views, and a dev-linked view holds \
         no sysroot of its own"
    );
    assert!(is_linked_aside(&views.join("trust-dev")));
}

#[test]
fn a_build_id_is_valid_only_when_nonzero() {
    assert!(!is_valid_build_id(0));
    assert!(is_valid_build_id(9192));
}
