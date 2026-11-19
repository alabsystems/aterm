// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn exe(bundle: &str) -> PathBuf {
    Path::new(bundle).join("Contents/MacOS/aterm")
}

/// Only a bundle's own executable outside the broker's image folder names an asker's bundle: a
/// loose binary, a helper deeper in a bundle, a relative path and the image itself do not.
/// NEGATIVE CONTROL (run): without the image-folder rule the image's own executable is kept.
#[test]
fn an_asker_is_kept_only_for_a_bundles_executable_outside_the_image() {
    assert_eq!(
        bundle_of(&exe("/Applications/aterm.app")),
        Some(PathBuf::from("/Applications/aterm.app"))
    );
    for refused in [
        PathBuf::from("/usr/local/bin/aterm"),
        PathBuf::from("/Applications/aterm.app/Contents/Helpers/aterm"),
        PathBuf::from("/Applications/aterm.app/Contents/MacOS/agh"),
        exe("Applications/aterm.app"),
        Path::new(aterm_uds::lan::IMAGE_EXECUTABLE).to_path_buf(),
    ] {
        assert_eq!(bundle_of(&refused), None, "{}", refused.display());
    }
    let askers = Askers::default();
    assert!(!askers.asked(501, [1; 20], Path::new(aterm_uds::lan::IMAGE_EXECUTABLE)));
    assert!(askers.now().is_empty());
}

/// The askers are bounded: one entry per bundle (the newest ask's), at most two per uid, eight
/// in all, newest first, and an account that asks from many bundles cannot push another's out.
/// NEGATIVE CONTROL (run): without the per-uid bound one uid's asks evict every other uid's.
#[test]
fn the_askers_are_bounded_per_bundle_per_uid_and_in_all() {
    let askers = Askers::default();
    assert!(askers.asked(501, [1; 20], &exe("/Users//a/aterm.app")));
    assert!(askers.asked(501, [2; 20], &exe("/Users//a/aterm.app")));
    assert_eq!(
        askers.now(),
        vec![Asker {
            uid: 501,
            cdhash: [2; 20],
            bundle: PathBuf::from("/Users//a/aterm.app"),
        }]
    );
    for n in 0..20 {
        assert!(askers.asked(502, [3; 20], &exe(&format!("/Users//b/{n}.app"))));
    }
    let kept = askers.now();
    let of = |uid| kept.iter().filter(|asker| asker.uid == uid).count();
    assert_eq!((of(501), of(502)), (1, MOST_PER_UID));
    assert_eq!(kept[0].bundle, PathBuf::from("/Users//b/19.app"));
    assert_eq!(kept[1].bundle, PathBuf::from("/Users//b/18.app"));
    for uid in 600..620 {
        assert!(askers.asked(uid, [4; 20], &exe(&format!("/Users/{uid}/aterm.app"))));
    }
    let kept = askers.now();
    assert_eq!(kept.len(), MOST);
    assert_eq!(kept[0].uid, 619);
}

/// Past a bound, of the entries other than the ask just made, the one of the lowest build the
/// adoption loop authenticated goes (one not yet authenticated the lowest), equal builds oldest
/// first: the ask just made always stays to be authenticated, a later ask never pushes out its
/// uid's newest authenticated build, and another uid's entries are not touched. NEGATIVE CONTROLS
/// (run): evicted oldest first, the newest bundle's ask goes; with the ask just made among those
/// that may go, it goes before it was ever authenticated.
#[test]
fn past_a_bound_the_lowest_authenticated_build_goes_never_the_ask_just_made() {
    let askers = Askers::default();
    let ask = |uid: u32, n: u8, bundle: &str| {
        assert!(askers.asked(uid, [n; 20], &exe(bundle)));
        Asker {
            uid,
            cdhash: [n; 20],
            bundle: PathBuf::from(bundle),
        }
    };
    let b = ask(502, 9, "/Users//b/aterm.app");
    askers.stated(&b, 1);
    let newest = ask(501, 1, "/Users//a/newest.app");
    askers.stated(&newest, 130);
    let older = ask(501, 2, "/Users//a/older.app");
    askers.stated(&older, 115);
    let newer = ask(501, 3, "/Users//a/newer.app");
    let bundles = |askers: &Askers| -> Vec<PathBuf> {
        askers.now().into_iter().map(|asker| asker.bundle).collect()
    };
    assert_eq!(
        bundles(&askers),
        [
            "/Users//a/newer.app",
            "/Users//a/newest.app",
            "/Users//b/aterm.app"
        ]
        .map(PathBuf::from),
        "the ask just made stays, unauthenticated; the lower authenticated build goes"
    );
    askers.stated(&newer, 120);
    let unread = ask(501, 4, "/Users//a/unread.app");
    assert_eq!(
        bundles(&askers),
        [
            "/Users//a/unread.app",
            "/Users//a/newest.app",
            "/Users//b/aterm.app"
        ]
        .map(PathBuf::from),
        "the uid's newest authenticated build stays"
    );
    askers.stated(&unread, 130);
    ask(501, 5, "/Users//a/same.app");
    assert_eq!(
        bundles(&askers),
        [
            "/Users//a/same.app",
            "/Users//a/unread.app",
            "/Users//b/aterm.app"
        ]
        .map(PathBuf::from),
        "of equal builds the oldest goes"
    );
    askers.stated(&newest, 200);
    assert!(
        askers
            .now()
            .iter()
            .all(|asker| asker.bundle != newest.bundle),
        "a build given to an entry that went keeps nothing"
    );
}

/// A forgotten asker goes only while it is still its bundle's entry: a newer ask from that bundle
/// stays. NEGATIVE CONTROL (run): forgetting by bundle drops the newer ask too.
#[test]
fn forgetting_an_asker_leaves_a_newer_ask_of_its_bundle() {
    let askers = Askers::default();
    assert!(askers.asked(501, [1; 20], &exe("/Users//a/aterm.app")));
    let first = askers.now().remove(0);
    assert!(askers.asked(501, [2; 20], &exe("/Users//a/aterm.app")));
    askers.forget(&first);
    assert_eq!(askers.now().len(), 1);
    let second = askers.now().remove(0);
    askers.forget(&second);
    assert!(askers.now().is_empty());
}
