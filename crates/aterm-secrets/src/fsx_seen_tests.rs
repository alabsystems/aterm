// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::path::PathBuf;

/// A file looked at whole beside the tree, a directory passed through (looked at by identity),
/// and a tree whose `home` is a successor home its walk leaves out:
///
/// ```text
/// file
/// pass/x
/// tree/a
/// tree/link -> a
/// tree/sub/b
/// tree/home/c
/// ```
struct Fixture {
    root: aterm_tempfile::TempDir,
    file: PathBuf,
    pass: PathBuf,
    tree: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let at = |name: &str| root.path().join(name);
        let (file, pass, tree) = (at("file"), at("pass"), at("tree"));
        let home = tree.join("home");
        for dir in [&pass, &tree, &tree.join("sub"), &home] {
            std::fs::create_dir(dir).unwrap();
        }
        write_owned_in_place(&file, b"file");
        write_owned_in_place(pass.join("x"), b"x");
        write_owned_in_place(tree.join("a"), b"alpha");
        std::os::unix::fs::symlink("a", tree.join("link")).unwrap();
        write_owned_in_place(tree.join("sub").join("b"), b"bravo");
        write_owned_in_place(home.join("c"), b"charlie");
        Self {
            root,
            file,
            pass,
            tree,
            home,
        }
    }

    /// The paths a key would hold of this fixture: the file and the tree whole, the directory
    /// passed through by its identity.
    fn listed(&self) -> Vec<(PathBuf, Look)> {
        let mut paths = vec![
            (self.file.clone(), Look::Whole),
            (self.pass.clone(), Look::Identity),
        ];
        paths.extend(
            tree_paths(&self.tree, std::slice::from_ref(&self.home))
                .expect("the tree lists")
                .into_iter()
                .map(|path| (path, Look::Whole)),
        );
        paths
    }
}

/// `after` as a filesystem whose clock ticks coarsely shows it where every change since `before`
/// fell within one tick of `before`'s look: each path looked at whole keeps the content and
/// inode-change times `before` showed of it. `utimensat` sets no inode-change time, so a test on a
/// fine clock cannot make a `chmod` keep it; this view is how such a filesystem reports it.
fn in_the_tick_of(after: &SeenSet, before: &SeenSet) -> SeenSet {
    SeenSet(
        after
            .0
            .iter()
            .zip(&before.0)
            .map(|(now, was)| match (now, was) {
                (
                    (look, Some(SeenAs::Whole { seen, gid })),
                    (_, Some(SeenAs::Whole { seen: was, .. })),
                ) => (
                    *look,
                    Some(SeenAs::Whole {
                        seen: Seen {
                            stamp: FileStamp {
                                mtime_ns: was.stamp.mtime_ns,
                                ctime_ns: was.stamp.ctime_ns,
                                ..seen.stamp
                            },
                            ..*seen
                        },
                        gid: *gid,
                    }),
                ),
                (now, _) => *now,
            })
            .collect(),
    )
}

/// Give `path` another group (`chown`), the first of those tried other than its own that this
/// user may give it, and name it; `None` where it may give none of them (`everyone`, `staff` and
/// `admin` on macOS; elsewhere this user may be in none).
fn chgrp(path: &Path) -> Option<u32> {
    let gid = std::fs::symlink_metadata(path).unwrap().gid();
    [12, 20, 80]
        .into_iter()
        .filter(|other| *other != gid)
        .find(|other| std::os::unix::fs::chown(path, None, Some(*other)).is_ok())
}

/// One change to a [`Fixture`], `make` answering whether it could be made; and whether of the
/// times it moves only the inode-change time, which a coarse clock can keep within one tick
/// ([`in_the_tick_of`]).
struct Change {
    what: &'static str,
    in_a_tick: bool,
    make: fn(&Fixture) -> bool,
}

/// A key's stamps ([`seen_set`] of a [`tree_paths`] listing and of single paths), taken by the
/// list as it stood before the change, since a confirmation lists nothing again, move for each of
/// these: a file renamed over, appended to or rewritten in place at the same length, its mode,
/// group (where this user is in another group tried) or link count changed, an entry made in a
/// listed directory or at the tree's root, an entry removed, and a symlink swapped in for a file.
/// A `chmod`, `chown` and link move them even where a coarse clock keeps every time within one
/// tick ([`in_the_tick_of`]). Reading every file, listing every directory and walking the tree
/// again move nothing, and taking them opens no file.
///
/// NEGATIVE CONTROLS (2026-11-14, each run alone): with `SeenAs::of` taking a whole look's mode
/// as 0, `left: ["a chmod (within one tick)"]`; with a whole look's group left out (`gid: 0`),
/// `left: ["a chown (within one tick)"]`; with a directory looked at whole taken by its identity
/// (`Look::Whole if meta.is_dir() => Self::Identity { .. }`), `left: ["an entry made in a listed
/// directory", "an entry made at the tree's root"]`.
#[test]
fn a_seen_set_moves_for_every_change_and_for_no_read() {
    let changes = [
        Change {
            what: "a file renamed over",
            in_a_tick: false,
            make: |at| {
                let beside = at.tree.join("a.new");
                write_owned_in_place(&beside, b"other");
                std::fs::rename(&beside, at.tree.join("a")).unwrap();
                true
            },
        },
        Change {
            what: "an append",
            in_a_tick: false,
            make: |at| {
                let mut file = File::options()
                    .append(true)
                    .open(at.tree.join("sub").join("b"))
                    .unwrap();
                file.write_all(b"!").unwrap();
                true
            },
        },
        Change {
            what: "a rewrite in place at the same length",
            in_a_tick: false,
            make: |at| {
                let file = File::options().write(true).open(&at.file).unwrap();
                file.write_all_at(b"FILE", 0).unwrap();
                // A time no other change gives it, so the case holds on any clock.
                file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30))
                    .unwrap();
                true
            },
        },
        Change {
            what: "a chmod",
            in_a_tick: true,
            make: |at| {
                std::fs::set_permissions(&at.file, std::fs::Permissions::from_mode(0o644)).unwrap();
                true
            },
        },
        Change {
            what: "a chown",
            in_a_tick: true,
            make: |at| chgrp(&at.tree.join("a")).is_some(),
        },
        Change {
            what: "a link added",
            in_a_tick: true,
            make: |at| {
                std::fs::hard_link(&at.file, at.pass.join("file.link")).unwrap();
                true
            },
        },
        Change {
            what: "an entry made in a listed directory",
            in_a_tick: false,
            make: |at| {
                write_owned_in_place(at.tree.join("sub").join("new"), b"new");
                true
            },
        },
        Change {
            what: "an entry made at the tree's root",
            in_a_tick: false,
            make: |at| {
                std::fs::create_dir(at.tree.join("made")).unwrap();
                true
            },
        },
        Change {
            what: "an entry removed",
            in_a_tick: false,
            make: |at| {
                std::fs::remove_file(at.tree.join("sub").join("b")).unwrap();
                true
            },
        },
        Change {
            what: "a symlink swapped in for a file",
            in_a_tick: false,
            make: |at| {
                let beside = at.tree.join("a.link");
                std::os::unix::fs::symlink("sub/b", &beside).unwrap();
                std::fs::rename(&beside, at.tree.join("a")).unwrap();
                true
            },
        },
    ];
    let mut unmoved = Vec::new();
    for Change {
        what,
        in_a_tick,
        make,
    } in changes
    {
        let at = Fixture::new();
        let listed = at.listed();
        let before = seen_set(&listed).expect("the paths are examined");
        if !make(&at) {
            // `chown`: this user is in no other group tried.
            continue;
        }
        let after = seen_set(&listed).expect("the paths are examined");
        if after == before {
            unmoved.push(what.to_owned());
        } else if in_a_tick && in_the_tick_of(&after, &before) == before {
            unmoved.push(format!("{what} (within one tick)"));
        }
    }
    assert_eq!(unmoved, Vec::<String>::new(), "moved nothing");

    let at = Fixture::new();
    let listed = at.listed();
    let opened = reads_under(at.root.path());
    let before = seen_set(&listed).expect("the paths are examined");
    for (path, _) in &listed {
        let meta = std::fs::symlink_metadata(path).unwrap();
        if meta.is_dir() {
            assert!(std::fs::read_dir(path).unwrap().count() > 0);
        } else if meta.file_type().is_symlink() {
            std::fs::read_link(path).unwrap();
        } else {
            std::fs::read(path).unwrap();
        }
    }
    assert_eq!(
        tree_paths(&at.tree, std::slice::from_ref(&at.home)),
        Some(
            listed
                .iter()
                .skip(2)
                .map(|(path, _)| path.clone())
                .collect()
        ),
        "a walk of the unchanged tree lists it as before"
    );
    assert_eq!(
        seen_set(&listed),
        Some(before),
        "reads and listings move nothing"
    );
    assert_eq!(
        reads_under(at.root.path()),
        opened,
        "stamping and walking open no file"
    );
}

/// A directory looked at by its identity, as a key looks at one a reader only passes through,
/// stands while entries come and go in it, and moves for a new mode and for another directory or
/// a symlink in its place.
///
/// NEGATIVE CONTROL (2026-11-14): with `SeenAs::of` taking an identity look as a whole one
/// (`Self::Whole { seen: seen_of(meta), gid: meta.gid() }`), `left: ["moved: entries made and
/// removed in it"]`.
#[test]
fn an_identity_look_moves_for_a_swap_and_not_for_churn() {
    let at = Fixture::new();
    let look = || seen_set(&[(at.pass.clone(), Look::Identity)]).expect("examined");
    let mut wrong = Vec::new();
    let before = look();
    write_owned_in_place(at.pass.join("y"), b"y");
    std::fs::remove_file(at.pass.join("x")).unwrap();
    if look() != before {
        wrong.push("moved: entries made and removed in it");
    }
    // A mode the umask did not give it.
    let mode = if std::fs::symlink_metadata(&at.pass).unwrap().mode() & 0o777 == 0o700 {
        0o711
    } else {
        0o700
    };
    std::fs::set_permissions(&at.pass, std::fs::Permissions::from_mode(mode)).unwrap();
    if look() == before {
        wrong.push("unmoved: its mode changed");
    }
    let before = look();
    let aside = at.root.path().join("aside");
    std::fs::rename(&at.pass, &aside).unwrap();
    std::fs::create_dir(&at.pass).unwrap();
    std::fs::set_permissions(&at.pass, std::fs::Permissions::from_mode(mode)).unwrap();
    if look() == before {
        wrong.push("unmoved: another directory in its place");
    }
    let before = look();
    std::fs::remove_dir(&at.pass).unwrap();
    std::os::unix::fs::symlink(&aside, &at.pass).unwrap();
    if look() == before {
        wrong.push("unmoved: a symlink in its place");
    }
    assert_eq!(wrong, Vec::<&str>::new());
}

/// A directory looked at privately, as a key looks at one a reader holds to the owner-only rule
/// before it reads in it, stands while entries come and go in it, and moves for an access-list
/// grant (no mode bit or time it keeps shows one) and back once the grant is taken away, and for a
/// mode that opens it.
///
/// NEGATIVE CONTROL (2026-11-15): with `SeenAs::of` taking a private look's verdict as
/// `Verdict::Private` whatever the directory's (`Verdict::of(path).map(|_| Verdict::Private)?`),
/// which leaves it what an identity look takes, `left: ["unmoved: an access list granted"]`.
#[cfg(target_os = "macos")]
#[test]
fn a_private_look_moves_for_an_access_list_and_not_for_churn() {
    let at = Fixture::new();
    std::fs::set_permissions(&at.pass, std::fs::Permissions::from_mode(0o700)).unwrap();
    let look = || seen_set(&[(at.pass.clone(), Look::Private)]).expect("examined");
    let acl = |words: &[&str]| {
        let chmod = aterm_uds::deadline::stand_in(Path::new("/bin/chmod"));
        let ran = aterm_uds::spawnfd::command(chmod)
            .args(words)
            .arg(&at.pass)
            .status()
            .expect("run chmod");
        assert!(ran.success(), "CONTROL: chmod {words:?}");
    };
    let mut wrong = Vec::new();
    let before = look();
    write_owned_in_place(at.pass.join("y"), b"y");
    std::fs::remove_file(at.pass.join("x")).unwrap();
    if look() != before {
        wrong.push("moved: entries made and removed in it");
    }
    acl(&["+a", "everyone allow list,search"]);
    assert_eq!(
        std::fs::symlink_metadata(&at.pass).unwrap().mode() & 0o777,
        0o700,
        "CONTROL: a grant moves no mode bit"
    );
    if look() == before {
        wrong.push("unmoved: an access list granted");
    }
    acl(&["-N"]);
    if look() != before {
        wrong.push("moved: the grant taken away");
    }
    std::fs::set_permissions(&at.pass, std::fs::Permissions::from_mode(0o755)).unwrap();
    if look() == before {
        wrong.push("unmoved: a mode that opens it");
    }
    assert_eq!(wrong, Vec::<&str>::new());
}

/// A tree's listing holds its root and every entry under it, sorted, a symlink among them as an
/// entry and never walked, and leaves out what `stop` names with all it holds; a missing root or
/// a file there lists the root alone, and a symlinked root lists nothing (`None`), since a reader
/// may follow it where the walk does not.
///
/// NEGATIVE CONTROLS (2026-11-14, each run alone): with `stop` ignored, the listing holds the
/// successor home (`the tree's listing: Some([... ".../tree/home", ".../tree/home/c", ...])`);
/// with an entry's type taken through a link (`std::fs::metadata(&path)`), it holds what the
/// symlinked directory leads to (`... ".../tree/linkdir", ".../tree/linkdir/b", ...`); with the
/// root looked at through a link (`std::fs::metadata(root).map(Some)`, as `read_dir` follows
/// one), `left: ["a symlinked root is listed", "a missing root is not the list alone"]`, the
/// second as that look answers a missing root with an error.
#[test]
fn a_tree_lists_all_it_holds_but_what_stop_names_and_never_through_a_link() {
    let at = Fixture::new();
    std::os::unix::fs::symlink("sub", at.tree.join("linkdir")).unwrap();
    let mut wrong = Vec::new();
    let listed = tree_paths(&at.tree, std::slice::from_ref(&at.home));
    let expected: Vec<PathBuf> = ["", "a", "link", "linkdir", "sub", "sub/b"]
        .into_iter()
        .map(|name| {
            if name.is_empty() {
                at.tree.clone()
            } else {
                at.tree.join(name)
            }
        })
        .collect();
    if listed.as_ref() != Some(&expected) {
        wrong.push(format!("the tree's listing: {listed:#?}"));
    }
    let linked = at.root.path().join("linked");
    std::os::unix::fs::symlink(&at.tree, &linked).unwrap();
    if tree_paths(&linked, &[]).is_some() {
        wrong.push("a symlinked root is listed".to_owned());
    }
    let missing = at.root.path().join("missing");
    if tree_paths(&missing, &[]) != Some(vec![missing.clone()]) {
        wrong.push("a missing root is not the list alone".to_owned());
    }
    if tree_paths(&at.file, &[]) != Some(vec![at.file.clone()]) {
        wrong.push("a file at the root is not the list alone".to_owned());
    }
    assert_eq!(wrong, Vec::<String>::new());
}
