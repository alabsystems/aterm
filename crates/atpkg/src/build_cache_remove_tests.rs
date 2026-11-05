// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(target_os = "macos")]
use crate::build_cache::tests::{DENY_DELETE, acl, holds_ace};
#[cfg(unix)]
use crate::build_cache::tests::{mode, mode_of};

/// Clears this thread's remover seams when dropped, so a failed test leaves none set.
#[cfg(unix)]
struct Seams;

#[cfg(unix)]
impl Drop for Seams {
    fn drop(&mut self) {
        seams::clear();
    }
}

/// `path`'s inode.
#[cfg(unix)]
fn ino(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path).unwrap().ino()
}

/// The exact ordered ACL entries printed by the same native reader as [`holds_ace`].
#[cfg(target_os = "macos")]
fn acl_entries(path: &Path) -> Vec<String> {
    let out = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/ls")))
        .arg("-led")
        .arg(path)
        .output()
        .expect("ls -led");
    assert!(out.status.success(), "ls -led: {}", out.status);
    String::from_utf8(out.stdout)
        .expect("ACL text")
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect()
}

/// A tree that is not there is gone already: whole, nothing freed.
#[test]
fn a_missing_tree_is_gone_already() {
    let dir = crate::testdir::dir("build-cache-remove-missing");
    assert_eq!(tree(&dir.join("absent")), Left::default());
    assert!(tree(&dir.join("absent")).whole());
}

#[cfg(unix)]
#[test]
fn revocation_after_a_file_went_keeps_the_exact_byte_count_and_stops_the_walk() {
    use std::cell::Cell;
    use std::os::unix::fs::MetadataExt as _;
    use std::rc::Rc;
    let directory = crate::testdir::dir("build-cache-remove-revoked");
    let top = directory.join("top");
    let first = top.join("first");
    std::fs::create_dir_all(&first).unwrap();
    let file = first.join("unit");
    std::fs::write(&file, vec![7u8; 64 << 10]).unwrap();
    let blocks = std::fs::symlink_metadata(&file).unwrap().blocks() * 512;
    assert!(blocks > 0);
    let stopped = Rc::new(Cell::new(false));
    let stop = Rc::clone(&stopped);
    let clear = Seams;
    seams::on(seams::At::Listed, move |name| {
        if name == "first" {
            stop.set(true);
        }
    });
    let observed_stop = Cell::new(false);
    let result = tree_while(&top, &|| {
        assert!(
            !observed_stop.get(),
            "a revoked pass must not ask permission again"
        );
        if stopped.get() {
            observed_stop.set(true);
            false
        } else {
            true
        }
    });
    assert_eq!(result, Err(Revoked { freed: blocks }));
    assert!(observed_stop.get());
    assert!(!file.exists());
    assert!(first.is_dir());
    drop(clear);
    assert!(tree_while(&top, &|| true).unwrap().whole());
    assert!(!top.exists());
}

/// WHAT WENT IS COUNTED ONCE, BY ITS LAST NAME: a file with two names in the tree frees its
/// blocks once, and one with a name outside it frees nothing (it is still there); nothing
/// stayed. NEGATIVE CONTROL: the outside name still reads the file's bytes.
#[cfg(unix)]
#[test]
fn freed_bytes_count_each_file_once_and_none_still_named_outside() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = crate::testdir::dir("build-cache-remove-freed");
    let top = dir.join("top");
    std::fs::create_dir_all(top.join("deps")).unwrap();
    let body = vec![7u8; 64 * 1024];
    std::fs::write(top.join("deps/one"), &body).unwrap();
    std::fs::hard_link(top.join("deps/one"), top.join("one")).unwrap();
    std::fs::write(top.join("deps/kept"), &body).unwrap();
    let outside = dir.join("outside");
    std::fs::hard_link(top.join("deps/kept"), &outside).unwrap();
    let blocks = std::fs::symlink_metadata(top.join("deps/one"))
        .unwrap()
        .blocks()
        * 512;
    assert!(blocks >= 64 * 1024, "{blocks}");

    let left = tree(&top);
    assert_eq!(
        left,
        Left {
            stayed: 0,
            freed: blocks
        }
    );
    assert!(!top.exists());
    assert_eq!(std::fs::read(&outside).unwrap(), body);
}

/// WHAT STAYED IS NAMED AS IT STANDS: the entries with nothing of them left to walk, depth first
/// in name order, the first few alone, each under the stage, with how many more there are, and
/// the words name each of them and none of what they hold; a stage that is a lone folder names
/// itself.
#[cfg(unix)]
#[test]
fn what_stayed_is_named_under_its_stage_first_few_alone() {
    let dir = crate::testdir::dir("build-cache-remove-named");
    let stage = dir.join(".cargo-build.removing");
    for rel in ["tmp/b/d", "tmp/a", "debug/deps"] {
        std::fs::create_dir_all(stage.join(rel)).unwrap();
    }
    std::fs::write(stage.join("debug/deps/x"), b"never read").unwrap();
    std::os::unix::fs::symlink("/nowhere", stage.join("tmp/b/link")).unwrap();
    assert_eq!(
        stayed(&stage, NAMED),
        [
            stage.join("debug/deps/x"),
            stage.join("tmp/a"),
            stage.join("tmp/b/d")
        ]
    );
    let four = named(&stage, 4);
    assert_eq!(
        four.first,
        [
            (PathBuf::from("debug/deps/x"), Why::StillRefusing),
            (PathBuf::from("tmp/a"), Why::StillRefusing),
            (PathBuf::from("tmp/b/d"), Why::StillRefusing)
        ]
    );
    assert_eq!((four.count, four.more()), (4, 1));
    let words = stayed_words(&stage, 4);
    assert_eq!(words, four.words(&stage));
    let mut at = 0;
    for (p, _) in &four.first {
        let found = words[at..]
            .find(&*p.to_string_lossy())
            .unwrap_or_else(|| panic!("{} named in order: {words}", p.display()));
        at += found;
    }
    assert!(!words.contains("never read"), "{words}");

    let lone = dir.join("lone");
    std::fs::create_dir(&lone).unwrap();
    let one = named(&lone, 1);
    assert_eq!(
        (one.more(), one.first),
        (0, vec![(lone.clone(), Why::StillRefusing)])
    );
}

/// A PATH LONGER THAN THE SYSTEM'S LIMIT GOES: a tree whose deepest file's path is longer than
/// any path call takes (`PATH_MAX`, 1024 bytes on macOS, 4096 on Linux) is removed whole, every
/// entry named in the folder held open. NEGATIVE CONTROL: the deepest file's path refuses a path
/// call (`symlink_metadata`), as it refused a remover walking by path.
#[cfg(unix)]
#[test]
fn a_tree_deeper_than_any_path_goes_whole() {
    use aterm_dirfd::{Mode, OFlags};
    let dir = crate::testdir::dir("build-cache-remove-deep");
    let top = dir.join("top");
    std::fs::create_dir(&top).unwrap();
    let name = "n".repeat(200);
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = aterm_dirfd::openat(aterm_dirfd::CWD, &top, flags, Mode::empty()).unwrap();
    let mut deep = top.clone();
    for _ in 0..24 {
        aterm_dirfd::mkdirat(&fd, &name, Mode::RWXU).unwrap();
        fd = aterm_dirfd::openat(&fd, &name, flags, Mode::empty()).unwrap();
        deep.push(&name);
    }
    let file = aterm_dirfd::openat(
        &fd,
        "f",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .unwrap();
    drop((file, fd));
    deep.push("f");
    assert!(deep.as_os_str().len() > 4096, "{}", deep.as_os_str().len());
    assert!(
        std::fs::symlink_metadata(&deep).is_err(),
        "a path call reaches the deepest file"
    );

    let left = tree(&top);
    assert_eq!(left.stayed, 0, "{left:?}");
    assert!(!top.exists());
}

/// A FOLDER SWAPPED FOR A LINK MID-WALK LEADS NOWHERE OUTSIDE: as the walk is about to open a
/// folder it read as a folder, the folder is moved out of the stage and a link takes its name,
/// to a folder outside or to the very folder moved out (the one the walk read, so the folder it
/// would open through the link is the one it judged). The walk opens nothing through the link:
/// the link goes as itself, the stage goes whole, and the folder moved out and the one outside
/// keep what they hold. NEGATIVE CONTROL: a path lookup at that moment (`read_dir` of the
/// folder's path, as a remover walking by path lists it) lists the file the link leads to.
#[cfg(unix)]
#[test]
fn a_folder_swapped_for_a_link_mid_walk_leads_nowhere_outside() {
    let dir = crate::testdir::dir("build-cache-remove-swap");
    for to_moved in [false, true] {
        let case = dir.join(if to_moved { "to-moved" } else { "to-outside" });
        let stage = case.join("stage");
        let swap = stage.join("a/swap");
        std::fs::create_dir_all(&swap).unwrap();
        std::fs::write(swap.join("inner"), b"inner").unwrap();
        let victim = case.join("outside/victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("precious"), b"precious").unwrap();
        let moved = case.join("outside/moved");
        let _seams = Seams;
        let listed = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let to = if to_moved {
            moved.clone()
        } else {
            victim.clone()
        };
        let (seen, at, from) = (std::rc::Rc::clone(&listed), swap.clone(), moved.clone());
        seams::on(seams::At::Opening, move |name| {
            if name == "swap" && !from.exists() {
                std::fs::rename(&at, &from).unwrap();
                std::os::unix::fs::symlink(&to, &at).unwrap();
                seen.borrow_mut().extend(
                    std::fs::read_dir(&at)
                        .unwrap()
                        .flatten()
                        .map(|e| e.file_name()),
                );
            }
        });

        let left = tree(&stage);
        let led_to = if to_moved { "inner" } else { "precious" };
        assert_eq!(*listed.borrow(), [led_to], "the path leads outside");
        assert_eq!(left.stayed, 0, "{left:?}");
        assert!(!stage.exists());
        assert_eq!(std::fs::read(victim.join("precious")).unwrap(), b"precious");
        assert_eq!(
            std::fs::read(moved.join("inner")).unwrap(),
            b"inner",
            "the folder moved out was emptied through the link"
        );
    }
}

/// A FOLDER SWAPPED FOR ANOTHER MID-WALK IS JUDGED AGAIN: as the walk is about to open a folder
/// it read as this user's, the folder is moved out and another user's folder (the remover's
/// seam) takes its name. What opens is not what was read, so the name is read again and left as
/// it is: that folder keeps its file and stays, counted once, and the folder moved out keeps its
/// own. NEGATIVE CONTROL: unmarked, the folder swapped in is this user's and goes with the stage.
#[cfg(unix)]
#[test]
fn a_folder_swapped_for_another_mid_walk_is_judged_again() {
    let dir = crate::testdir::dir("build-cache-remove-reswap");
    let run = |case: &str, theirs_marked: bool| {
        let case = dir.join(case);
        let stage = case.join("stage");
        let swap = stage.join("a/swap");
        std::fs::create_dir_all(&swap).unwrap();
        std::fs::write(swap.join("inner"), b"inner").unwrap();
        let theirs = case.join("theirs");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("held"), b"held").unwrap();
        let moved = case.join("moved");
        let _seams = Seams;
        if theirs_marked {
            seams::other_user(ino(&theirs));
        }
        let (at, from, them) = (swap.clone(), moved.clone(), theirs);
        seams::on(seams::At::Opening, move |name| {
            if name == "swap" && !from.exists() {
                std::fs::rename(&at, &from).unwrap();
                std::fs::rename(&them, &at).unwrap();
            }
        });
        let left = tree(&stage);
        assert_eq!(std::fs::read(moved.join("inner")).unwrap(), b"inner");
        (left, stage)
    };

    let (left, stage) = run("control", false);
    assert!(left.whole() && !stage.exists(), "{left:?}");

    let (left, stage) = run("marked", true);
    assert_eq!(left.stayed, 1, "{left:?}");
    assert_eq!(std::fs::read(stage.join("a/swap/held")).unwrap(), b"held");
}

/// THE FOLDER HOLDING THE TREE IS NEVER CHANGED: `top` in a folder at 0o500 (on macOS also
/// denying `delete_child` by ACL) is emptied, and then stays, counted once: the folder keeps its
/// mode (and its ACL), and `top` stays in it. NEGATIVE CONTROL: the folder refuses the removal of
/// an empty folder beside `top`, by path, before the walk.
#[cfg(unix)]
#[test]
fn the_folder_holding_the_tree_is_never_changed() {
    let dir = crate::testdir::dir("build-cache-remove-holder");
    let holder = dir.join("holder");
    let top = holder.join("top");
    std::fs::create_dir_all(top.join("deps")).unwrap();
    std::fs::create_dir(holder.join("empty")).unwrap();
    std::fs::write(top.join("deps/x"), b"x").unwrap();
    #[cfg(target_os = "macos")]
    acl(&holder, &["group:everyone deny delete_child"]);
    mode(&holder, 0o500);
    assert!(std::fs::remove_dir(holder.join("empty")).is_err());

    let left = tree(&top);
    assert_eq!(left.stayed, 1, "{left:?}");
    assert_eq!(mode_of(&holder), 0o500, "the holder was chmodded");
    #[cfg(target_os = "macos")]
    assert!(
        holds_ace(&holder, "deny delete_child"),
        "the holder's ACL was cleared"
    );
    assert!(top.is_dir() && !top.join("deps").exists());
    assert!(holder.join("empty").is_dir());
    mode(&holder, 0o700);
}

/// ANOTHER USER'S ENTRY IS LEFT AS IT IS: a folder (at 0o500, on macOS denying its removal by
/// ACL, holding a file) and a file, each read as another user's (the remover's seam), are never
/// entered, changed or removed: each stays, counted once, the folder keeping its mode, its ACL and
/// its file, the file its bytes; this user's entries beside them go. NEGATIVE CONTROL: unmarked,
/// the same tree goes whole.
#[cfg(unix)]
#[test]
fn another_users_entries_are_left_as_they_are() {
    let dir = crate::testdir::dir("build-cache-remove-theirs");
    let lay = |stage: &Path| {
        let theirs = stage.join("theirs");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("f"), b"f").unwrap();
        std::fs::write(stage.join("their-file"), b"theirs").unwrap();
        std::fs::create_dir_all(stage.join("mine/deps")).unwrap();
        std::fs::write(stage.join("mine/deps/x"), b"x").unwrap();
        #[cfg(target_os = "macos")]
        acl(&theirs, &[DENY_DELETE]);
        mode(&theirs, 0o500);
    };
    let control = dir.join("control");
    lay(&control);
    assert!(tree(&control).whole());
    assert!(!control.exists());

    let stage = dir.join("stage");
    lay(&stage);
    let _seams = Seams;
    seams::other_user(ino(&stage.join("theirs")));
    seams::other_user(ino(&stage.join("their-file")));
    let left = tree(&stage);
    assert_eq!(left.stayed, 2, "{left:?}");
    assert_eq!(mode_of(&stage.join("theirs")), 0o500);
    #[cfg(target_os = "macos")]
    assert!(holds_ace(&stage.join("theirs"), DENY_DELETE));
    assert_eq!(std::fs::read(stage.join("theirs/f")).unwrap(), b"f");
    assert_eq!(std::fs::read(stage.join("their-file")).unwrap(), b"theirs");
    assert!(!stage.join("mine").exists());
}

/// ANOTHER VOLUME IS NEVER ENTERED: a folder read as on another volume (the remover's seam: a
/// volume mounted inside the stage) is neither entered nor removed, its file stays, and it is
/// counted once; the entries beside it go. NEGATIVE CONTROL: unmarked, the same tree goes whole.
#[cfg(unix)]
#[test]
fn another_volume_is_never_entered() {
    let dir = crate::testdir::dir("build-cache-remove-volume");
    let lay = |stage: &Path| {
        std::fs::create_dir_all(stage.join("mount")).unwrap();
        std::fs::write(stage.join("mount/f"), b"f").unwrap();
        std::fs::write(stage.join("beside"), b"b").unwrap();
    };
    let control = dir.join("control");
    lay(&control);
    assert!(tree(&control).whole());

    let stage = dir.join("stage");
    lay(&stage);
    let _seams = Seams;
    seams::other_volume(ino(&stage.join("mount")));
    let left = tree(&stage);
    assert_eq!(left.stayed, 1, "{left:?}");
    assert_eq!(std::fs::read(stage.join("mount/f")).unwrap(), b"f");
    assert!(!stage.join("beside").exists());
}

/// A FOLDER DENYING ITS SEARCH IS CLEARED AND EMPTIED: a folder whose ACL denies `search` lists
/// what it holds but lets nothing in it be read or removed; the walk clears that folder's ACL
/// and the tree goes whole. NEGATIVE CONTROL: before the walk, its file cannot be read by path.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_denying_its_search_is_cleared_and_emptied() {
    let dir = crate::testdir::dir("build-cache-remove-search");
    let stage = dir.join("stage");
    let shut = stage.join("shut");
    std::fs::create_dir_all(&shut).unwrap();
    std::fs::write(shut.join("f"), b"f").unwrap();
    std::fs::write(stage.join("g"), b"g").unwrap();
    acl(&shut, &["group:everyone deny search"]);
    assert!(std::fs::symlink_metadata(shut.join("f")).is_err());

    let left = tree(&stage);
    assert_eq!(left.stayed, 0, "{left:?}");
    assert!(!stage.exists());
}

/// A FOLDER WHOSE MODE DENIES ITS OWNER SEARCH IS GIVEN IT AND EMPTIED: a folder at 0o600 opens
/// and lists but lets nothing in it be read, its custody included; the walk gives its owner
/// search through the folder held open before it reads that custody there, is not revoked, and
/// the tree goes whole. NEGATIVE CONTROL: before the walk, its file cannot be read by path.
#[cfg(unix)]
#[test]
fn a_folder_whose_mode_denies_its_search_is_given_it_and_emptied() {
    let dir = crate::testdir::dir("build-cache-remove-unsearched");
    let stage = dir.join("stage");
    let shut = stage.join("shut");
    std::fs::create_dir_all(&shut).unwrap();
    std::fs::write(shut.join("f"), b"f").unwrap();
    std::fs::write(stage.join("g"), b"g").unwrap();
    mode(&shut, 0o600);
    assert!(std::fs::symlink_metadata(shut.join("f")).is_err());

    let left = tree_while(&stage, &|| true).expect("revoked by a search it denied");
    assert_eq!(left.stayed, 0, "{left:?}");
    assert!(!stage.exists());
}

/// A KEEP THAT DENIES ITS SEARCH STILL STOPS THE WALK: a folder in the tree retained by the
/// keep-only API ([`crate::build_cache::registered::retain`]), then made by `deny` to deny its
/// owner search, hides its custody marker. Its complete name-only scan finds custody and
/// revokes before changing that folder: its mode, ACL, contents and marker stay unchanged.
/// The keep is withdrawn by hand
/// at the end so the scratch goes. NEGATIVE CONTROL: before the walk, the marker cannot be read
/// by path.
#[cfg(unix)]
fn a_keep_denying_its_search_stops_the_walk(
    prefix: &str,
    deny: fn(&Path),
    after: u32,
    allow: fn(&Path),
) {
    use crate::build_cache::registered::{MARKER, retain};
    use std::os::unix::fs::MetadataExt as _;
    let dir = crate::testdir::dir(prefix);
    let home = dir.canonicalize().unwrap();
    let stage = home.join("stage");
    let kept = stage.join("kept");
    std::fs::create_dir_all(kept.join("deps")).unwrap();
    std::fs::write(kept.join("deps/precious"), b"retained output").unwrap();
    std::fs::write(stage.join("beside"), b"b").unwrap();
    mode(&kept, 0o700);
    assert_eq!(retain(&home, &kept).unwrap(), kept);
    let marker = std::fs::read(kept.join(MARKER)).unwrap();
    deny(&kept);
    assert!(std::fs::read(kept.join(MARKER)).is_err());
    let before = std::fs::symlink_metadata(&kept).unwrap();

    let walked = tree_while(&stage, &|| true);
    assert!(walked.is_err(), "a keep was removed: {walked:?}");
    assert_eq!(mode_of(&kept), after);
    let now = std::fs::symlink_metadata(&kept).unwrap();
    assert_eq!(
        (now.ctime(), now.ctime_nsec()),
        (before.ctime(), before.ctime_nsec())
    );
    assert!(std::fs::read(kept.join(MARKER)).is_err());
    allow(&kept);
    assert_eq!(
        std::fs::read(kept.join("deps/precious")).unwrap(),
        b"retained output"
    );
    assert_eq!(std::fs::read(kept.join(MARKER)).unwrap(), marker);
    std::fs::remove_file(kept.join(MARKER)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_keep_whose_mode_denies_its_search_stops_the_walk() {
    a_keep_denying_its_search_stops_the_walk(
        "build-cache-remove-keep-mode",
        |kept| mode(kept, 0o400),
        0o400,
        |kept| mode(kept, 0o700),
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_keep_whose_acl_denies_its_search_stops_the_walk() {
    a_keep_denying_its_search_stops_the_walk(
        "build-cache-remove-keep-acl",
        |kept| acl(kept, &["group:everyone deny search"]),
        0o700,
        |kept| {
            let status =
                aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/chmod")))
                    .arg("-N")
                    .arg(kept)
                    .status()
                    .expect("chmod -N");
            assert!(status.success(), "chmod -N {}: {status}", kept.display());
        },
    );
}

/// A complete fresh-directory scan must leave the later traversal's cursor at the start.
/// Enough names to cross the directory reader's buffer are all removed after search repair.
#[cfg(target_os = "macos")]
#[test]
fn a_search_repair_scan_preserves_every_entry_for_the_removal() {
    let dir = crate::testdir::dir("build-cache-search-cursor");
    let stage = dir.join("stage");
    let shut = stage.join("shut");
    std::fs::create_dir_all(&shut).unwrap();
    for index in 0..1024 {
        let name = format!("{index:04}-{}", "x".repeat(96));
        std::fs::write(shut.join(name), b"x").unwrap();
    }
    acl(&shut, &["group:everyone deny search"]);
    assert!(tree(&stage).whole());
    assert!(!stage.exists());
}

/// Even an invalid, unreadable, or differently cased custody name defeats absence proof.
/// No permission or content is changed merely to discover which declaration it might be.
#[cfg(target_os = "macos")]
#[test]
fn hidden_custody_preserves_permissions_and_contents() {
    use crate::build_cache::registered::MARKER;
    for kind in ["invalid", "unreadable", "uppercase", "symlink"] {
        let dir = crate::testdir::dir("build-cache-hidden-custody");
        let stage = dir.join("stage");
        let shut = stage.join("shut");
        std::fs::create_dir_all(&shut).unwrap();
        std::fs::write(shut.join("precious"), b"retained bytes").unwrap();
        let marker = shut.join(if kind == "uppercase" {
            MARKER.to_ascii_uppercase()
        } else {
            MARKER.into()
        });
        if kind == "symlink" {
            std::os::unix::fs::symlink("precious", &marker).unwrap();
        } else {
            std::fs::write(&marker, b"unknown custody").unwrap();
        }
        if kind == "unreadable" {
            acl(&marker, &["group:everyone deny read"]);
        }
        acl(&shut, &["group:everyone deny search"]);
        let before = snapshot(&shut);
        let left = tree(&stage);
        assert!(!left.whole(), "{kind}");
        assert_eq!(snapshot(&shut), before, "{kind}");
        assert_eq!(
            named(&stage, left.stayed).first,
            [(stage.clone(), Why::PermissionRevoked)],
            "{kind}"
        );
        assert!(cleared(&shut));
        if kind == "unreadable" {
            assert!(cleared(&marker));
        }
        assert_eq!(
            std::fs::read(shut.join("precious")).unwrap(),
            b"retained bytes"
        );
    }
}

/// A declaration or a lost ancestor lease between inspection and repair must stop the lift.
#[cfg(target_os = "macos")]
#[test]
fn search_repair_rechecks_custody_and_parent_permission() {
    use crate::build_cache::registered::MARKER;
    use std::cell::Cell;
    use std::rc::Rc;
    for late_marker in [false, true] {
        let dir = crate::testdir::dir("build-cache-search-recheck");
        let stage = dir.join("stage");
        let shut = stage.join("shut");
        std::fs::create_dir_all(&shut).unwrap();
        std::fs::write(shut.join("precious"), b"retained bytes").unwrap();
        acl(&shut, &["group:everyone deny search"]);
        let permitted = Rc::new(Cell::new(true));
        let held = Rc::clone(&permitted);
        let target = shut.clone();
        let _seams = Seams;
        seams::on(seams::At::CustodyChecked, move |name| {
            if name != "shut" {
                return;
            }
            if late_marker {
                assert!(cleared(&target));
                std::fs::write(target.join(MARKER), b"late custody").unwrap();
                acl(&target, &["group:everyone deny search"]);
            } else {
                held.set(false);
            }
        });
        assert!(tree_while(&stage, &|| permitted.get()).is_err());
        assert!(holds_ace(&shut, "deny search"));
        assert!(cleared(&shut));
        assert_eq!(
            std::fs::read(shut.join("precious")).unwrap(),
            b"retained bytes"
        );
    }
}

/// The pinned directory may have moved outside the tree during inspection. Its old name
/// must still identify it before permission repair changes anything through the descriptor.
#[cfg(target_os = "macos")]
#[test]
fn search_repair_does_not_change_a_directory_replaced_after_inspection() {
    let dir = crate::testdir::dir("build-cache-search-replaced");
    let stage = dir.join("stage");
    let shut = stage.join("shut");
    let moved = dir.join("moved");
    std::fs::create_dir_all(&shut).unwrap();
    std::fs::write(shut.join("precious"), b"retained bytes").unwrap();
    acl(&shut, &["group:everyone deny search"]);
    let original = ino(&shut);
    let (from, to) = (shut.clone(), moved.clone());
    let _seams = Seams;
    seams::on(seams::At::CustodyChecked, move |name| {
        if name == "shut" && !to.exists() {
            std::fs::rename(&from, &to).unwrap();
            std::fs::create_dir(&from).unwrap();
        }
    });
    assert!(!tree(&stage).whole());
    assert_eq!(ino(&moved), original);
    assert!(holds_ace(&moved, "deny search"));
    assert!(shut.is_dir());
    assert!(cleared(&moved));
    assert_eq!(
        std::fs::read(moved.join("precious")).unwrap(),
        b"retained bytes"
    );
}

/// A readable custody refusal remains opaque in forecasts and in plain removal diagnostics.
#[cfg(unix)]
#[test]
fn custody_forecasts_and_plain_revocation_never_enumerate_the_retained_subtree() {
    use crate::build_cache::registered::{MARKER, retain};
    for retained in [false, true] {
        let dir = crate::testdir::dir("build-cache-custody-forecast");
        let home = dir.canonicalize().unwrap();
        let stage = home.join("stage");
        std::fs::create_dir_all(stage.join("precious/subtree")).unwrap();
        mode(&stage, 0o700);
        std::fs::write(stage.join("precious/subtree/value"), b"retained bytes").unwrap();
        if retained {
            retain(&home, &stage).unwrap();
        } else {
            std::fs::write(stage.join(MARKER), b"unknown custody").unwrap();
        }
        let before = snapshot(&stage);
        let forecast = forecast(&stage, NAMED);
        assert!(!forecast.whole && forecast.count == 1);
        assert_eq!(forecast.first, [(PathBuf::new(), Why::PermissionRevoked)]);
        assert_eq!(snapshot(&stage), before);
        let left = tree(&stage);
        assert_eq!(left.stayed, 1);
        assert_eq!(
            named(&stage, left.stayed).first,
            [(stage.clone(), Why::PermissionRevoked)]
        );
        assert_eq!(snapshot(&stage), before);
        std::fs::remove_file(stage.join(MARKER)).unwrap();
    }
}

/// UNKNOWN CUSTODY NEVER WIDENS A KEEP'S ACL: a nested retained folder at 0755 holds both
/// an everyone search denial (or a list denial preventing its open) and an unrelated other
/// user's list denial. The actual walk refuses with the exact ordered ACL, mode, marker and
/// payload intact. NEGATIVE CONTROL: after the owner clears the ACL and withdraws the keep,
/// this same tree goes whole. Held file handles read the evidence while the denial still stands.
#[cfg(target_os = "macos")]
#[test]
fn an_acl_hiding_a_keeps_custody_never_discards_other_access_restrictions() {
    use std::io::Read as _;

    use crate::build_cache::registered::{MARKER, retain};

    for denial in ["search", "list"] {
        let dir = crate::testdir::dir(&format!("build-cache-keep-private-{denial}"));
        let home = dir.canonicalize().unwrap();
        let stage = home.join("stage");
        let kept = stage.join("kept");
        let payload = kept.join("deps/precious");
        std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
        std::fs::write(&payload, b"private retained output").unwrap();
        mode(&kept, 0o755);
        assert_eq!(retain(&home, &kept).unwrap(), kept);
        let marker = kept.join(MARKER);
        let marker_before = std::fs::read(&marker).unwrap();
        let mut marker_file = std::fs::File::open(&marker).unwrap();
        let mut payload_file = std::fs::File::open(&payload).unwrap();
        let deny = format!("group:everyone deny {denial}");
        let other = if crate::platform::our_uid() == 0 {
            "user:nobody deny list"
        } else {
            "user:root deny list"
        };
        // Add the unrelated restriction while this owner can still open the
        // directory; the final list denial also prevents chmod opening it.
        acl(&kept, &[other, &deny]);
        let before = acl_entries(&kept);
        assert!(holds_ace(&kept, &deny));
        assert!(holds_ace(&kept, other));
        assert_eq!(std::fs::File::open(&kept).is_ok(), denial == "search");

        let walked = tree_while(&stage, &|| true);
        assert!(
            !matches!(&walked, Ok(left) if left.whole()),
            "unknown custody was removed: {walked:?}"
        );
        assert_eq!(mode_of(&kept), 0o755);
        assert_eq!(acl_entries(&kept), before, "{denial}: ACL changed");
        let mut marker_after = Vec::new();
        marker_file.read_to_end(&mut marker_after).unwrap();
        assert_eq!(marker_after, marker_before);
        let mut payload_after = Vec::new();
        payload_file.read_to_end(&mut payload_after).unwrap();
        assert_eq!(payload_after, b"private retained output");

        let status =
            aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/chmod")))
                .arg("-N")
                .arg(&kept)
                .status()
                .expect("chmod -N");
        assert!(status.success(), "chmod -N: {status}");
        std::fs::remove_file(marker).unwrap();
        assert!(tree_while(&stage, &|| true).unwrap().whole());
        assert!(!stage.exists());
    }
}

/// A TREE A CUSTODY READ STOPS IS NEVER WHOLE: a plain removal ([`tree`]) of a stage holding a
/// custody marker that cannot be read, in a folder of it or at the stage itself, stops where it
/// reads that marker, and reports the stage as standing (at least one entry stayed), so no
/// caller books it removed; the folder and its file stay. The marker is withdrawn by hand at the
/// end so the scratch goes. NEGATIVE CONTROL: the same stage without the marker goes whole.
#[cfg(unix)]
#[test]
fn a_tree_a_custody_read_stops_is_never_whole() {
    use crate::build_cache::registered::MARKER;
    let dir = crate::testdir::dir("build-cache-remove-custody");
    let lay = |stage: &Path| {
        std::fs::create_dir_all(stage.join("held")).unwrap();
        std::fs::write(stage.join("held/f"), b"f").unwrap();
        std::fs::write(stage.join("beside"), b"b").unwrap();
    };
    let control = dir.join("control");
    lay(&control);
    assert!(tree(&control).whole());
    assert!(!control.exists());

    for at in ["held", ""] {
        let stage = dir.join("stage");
        lay(&stage);
        let marker = stage.join(at).join(MARKER);
        std::fs::write(&marker, b"unreadable custody").unwrap();
        let left = tree(&stage);
        assert!(
            !left.whole(),
            "a stopped walk read as whole at {at:?}: {left:?}"
        );
        assert!(left.stayed >= 1, "{left:?}");
        assert_eq!(std::fs::read(stage.join("held/f")).unwrap(), b"f");
        assert_eq!(std::fs::read(&marker).unwrap(), b"unreadable custody");
        std::fs::remove_file(&marker).unwrap();
        assert!(tree(&stage).whole());
        assert!(!stage.exists());
    }
}

/// A FOLDER IS LET SEARCHED ONLY WHILE EVERY FOLDER ABOVE IT ALLOWS: a folder at 0o600 whose
/// holding folder gains a custody marker just as the walk holds it open (the remover's seam: a
/// late keep above it) is never changed: the walk revokes, the folder keeps its mode and its
/// file. NEGATIVE CONTROL: before the walk, its file cannot be read by path.
#[cfg(unix)]
#[test]
fn a_folder_is_let_searched_only_while_every_folder_above_it_allows() {
    let dir = crate::testdir::dir("build-cache-remove-late-keep");
    let stage = dir.join("stage");
    let outer = stage.join("outer");
    let shut = outer.join("shut");
    std::fs::create_dir_all(&shut).unwrap();
    std::fs::write(shut.join("f"), b"f").unwrap();
    mode(&shut, 0o600);
    assert!(std::fs::symlink_metadata(shut.join("f")).is_err());
    let marker = outer.join(crate::build_cache::registered::MARKER);
    let late = marker.clone();
    let _seams = Seams;
    seams::on(seams::At::Held, move |name| {
        if name == "shut" {
            std::fs::write(&late, b"late custody").unwrap();
        }
    });

    let walked = tree_while(&stage, &|| true);
    assert!(walked.is_err(), "a late keep was passed over: {walked:?}");
    assert_eq!(mode_of(&shut), 0o600, "lifted under a keep");
    mode(&shut, 0o700);
    assert_eq!(std::fs::read(shut.join("f")).unwrap(), b"f");
    std::fs::remove_file(&marker).unwrap();
}

/// A KEEP AS DEEP AS THE WALK GOES IS NEVER MOVED: a folder retained by the keep-only API
/// ([`crate::build_cache::registered::retain`]) exactly [`sys::DEPTH`] folders deep in a stage,
/// as deep as the walk holds folders before it moves one up to walk it from under the stage, is
/// held and its custody read before it would move: the pass is revoked, nothing was moved up,
/// and the keep stands at its own path with its marker and what it holds. The keep is withdrawn
/// by hand at the end so the scratch goes. NEGATIVE CONTROL: without the keep, the same chain
/// goes whole.
#[cfg(unix)]
#[test]
fn a_keep_as_deep_as_the_walk_goes_is_never_moved() {
    use crate::build_cache::registered::{MARKER, retain};
    let dir = crate::testdir::dir("build-cache-remove-deep-keep");
    let home = dir.canonicalize().unwrap();
    let stage = home.join("stage");
    let kept = chain(&stage, sys::DEPTH - 1).join("kept");
    assert_eq!(
        kept.strip_prefix(&stage).unwrap().components().count(),
        sys::DEPTH
    );
    std::fs::create_dir_all(kept.join("deps")).unwrap();
    std::fs::write(kept.join("deps/precious"), b"retained output").unwrap();
    mode(&kept, 0o700);
    assert_eq!(retain(&home, &kept).unwrap(), kept);
    let marker = std::fs::read(kept.join(MARKER)).unwrap();

    let walked = tree_while(&stage, &|| true);
    assert!(walked.is_err(), "a keep was passed over: {walked:?}");
    let up: Vec<_> = std::fs::read_dir(&stage)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with(sys::UP))
        .collect();
    assert!(up.is_empty(), "the keep was moved up: {up:?}");
    assert_eq!(
        std::fs::read(kept.join("deps/precious")).unwrap(),
        b"retained output"
    );
    assert_eq!(std::fs::read(kept.join(MARKER)).unwrap(), marker);
    std::fs::remove_file(kept.join(MARKER)).unwrap();
    assert!(tree_while(&stage, &|| true).unwrap().whole());
    assert!(!stage.exists());
}

/// A KEEP SWAPPED IN AT A DEEP NAME IS NEVER MOVED UP UNREAD: a plain folder `x` exactly
/// [`sys::DEPTH`] folders deep in a stage is held and its custody read, which passes; then it is
/// renamed aside and a folder retained by the keep-only API
/// ([`crate::build_cache::registered::retain`]) renamed in at `x`. Swapped in once `x` was held
/// (the remover's `Held` seam), the keep is found by `x`'s second read before any rename, read
/// anew, its custody read and the pass revoked, and nothing was ever about to move it (the
/// `Moving` seam never finds it at `x`). Swapped in at the instant before the rename (the
/// `Moving` seam), the keep moves, is found under the stage to be another folder than the one
/// held, is put back at `x`, and the pass revoked. Either way nothing stands moved up under the
/// stage, and the keep stands at `x` with its marker and what it holds. The keep is withdrawn by
/// hand at the end so the scratch goes. NEGATIVE CONTROL: the swap happened (the folder first
/// held stands aside, and the keep's inode is at `x`).
#[cfg(unix)]
#[test]
fn a_keep_swapped_in_at_a_deep_name_is_never_moved_up_unread() {
    use crate::build_cache::registered::{MARKER, retain};
    use std::cell::Cell;
    use std::rc::Rc;
    for at in [seams::At::Held, seams::At::Moving] {
        let dir = crate::testdir::dir("build-cache-remove-deep-swap");
        let home = dir.canonicalize().unwrap();
        let stage = home.join("stage");
        let deep = chain(&stage, sys::DEPTH - 1).join("x");
        std::fs::create_dir(&deep).unwrap();
        assert_eq!(
            deep.strip_prefix(&stage).unwrap().components().count(),
            sys::DEPTH
        );
        let first = ino(&deep);
        let kept = home.join("kept");
        std::fs::create_dir_all(kept.join("deps")).unwrap();
        std::fs::write(kept.join("deps/precious"), b"retained output").unwrap();
        mode(&kept, 0o700);
        assert_eq!(retain(&home, &kept).unwrap(), kept);
        let marker = std::fs::read(kept.join(MARKER)).unwrap();
        let keep = ino(&kept);
        let aside = home.join("x-aside");
        let _seams = Seams;
        let swapped = Rc::new(Cell::new(false));
        let (once, from, to, out) = (
            Rc::clone(&swapped),
            kept.clone(),
            deep.clone(),
            aside.clone(),
        );
        seams::on(at, move |name| {
            if name == "x" && !once.replace(true) {
                std::fs::rename(&to, &out).unwrap();
                std::fs::rename(&from, &to).unwrap();
            }
        });
        let about = Rc::new(Cell::new(false));
        if at == seams::At::Held {
            let (seen, deep) = (Rc::clone(&about), deep.clone());
            seams::on(seams::At::Moving, move |name| {
                if name == "x" && ino(&deep) == keep {
                    seen.set(true);
                }
            });
        }

        let walked = tree_while(&stage, &|| true);
        seams::clear();
        assert!(swapped.get(), "{at:?}: never swapped");
        assert!(
            walked.is_err(),
            "{at:?}: a keep was passed over: {walked:?}"
        );
        assert!(!about.get(), "{at:?}: the keep was about to be moved up");
        let up: Vec<_> = std::fs::read_dir(&stage)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().starts_with(sys::UP))
            .collect();
        assert!(up.is_empty(), "{at:?}: moved up: {up:?}");
        assert_eq!(ino(&aside), first, "{at:?}");
        assert_eq!(ino(&deep), keep, "{at:?}: the keep left its name");
        assert_eq!(
            std::fs::read(deep.join("deps/precious")).unwrap(),
            b"retained output"
        );
        assert_eq!(std::fs::read(deep.join(MARKER)).unwrap(), marker);
        assert!(
            !stage.join(MARKER).exists(),
            "{at:?}: no escaped move needs a temporary top fence"
        );
        std::fs::remove_file(deep.join(MARKER)).unwrap();
        assert!(tree_while(&stage, &|| true).unwrap().whole());
        assert!(!stage.exists());
    }
}

/// A KEEP PUBLISHED AS A DEEP FOLDER IS RENAMED UP KEEPS IT WHERE IT STOOD: a folder `x` exactly
/// [`sys::DEPTH`] folders deep in a stage is held and its custody read, which passes; then, at
/// the instant before it is renamed up (the remover's `Moving` seam), a custody marker is
/// published on a folder holding it (`kept`, ten folders down) or in `x` itself. The rename
/// acts by name all the same, but custody is read again through every folder held just after
/// it: the keep is found, `x` is put back at its own path, and the pass revoked. Nothing stands
/// moved up under the stage, `x` and what it holds stand where they stood, and a later pass
/// stops at the keep too. The marker is withdrawn by hand at the end so the scratch goes.
/// NEGATIVE CONTROL: the seam fired (the marker was published at the instant before the
/// rename), and with no marker published the same chain goes whole.
#[cfg(unix)]
#[test]
fn a_keep_published_as_a_deep_folder_is_renamed_up_keeps_it_where_it_stood() {
    use crate::build_cache::registered::MARKER;
    use std::cell::Cell;
    use std::rc::Rc;
    let lay = |stage: &Path| {
        let kept = chain(stage, 10);
        let deep = chain(&kept, sys::DEPTH - 11).join("x");
        std::fs::create_dir(&deep).unwrap();
        assert_eq!(
            deep.strip_prefix(stage).unwrap().components().count(),
            sys::DEPTH
        );
        std::fs::write(deep.join("precious"), b"under the keep").unwrap();
        (kept, deep)
    };
    let dir = crate::testdir::dir("build-cache-remove-deep-late-keep");
    let control = dir.join("control");
    lay(&control);
    assert!(tree_while(&control, &|| true).unwrap().whole());
    assert!(!control.exists());

    for on_x in [false, true] {
        let stage = dir.join("stage");
        let (kept, deep) = lay(&stage);
        let held = ino(&deep);
        let marker = if on_x {
            deep.join(MARKER)
        } else {
            kept.join(MARKER)
        };
        let _seams = Seams;
        let published = Rc::new(Cell::new(false));
        let (once, late) = (Rc::clone(&published), marker.clone());
        seams::on(seams::At::Moving, move |name| {
            if name == "x" && !once.replace(true) {
                std::fs::write(&late, b"keep published now").unwrap();
            }
        });
        let walked = tree_while(&stage, &|| true);
        seams::clear();
        assert!(published.get(), "on x {on_x}: the seam never fired");
        assert!(walked.is_err(), "on x {on_x}: a keep was passed over");
        for pass in 0..2 {
            let up: Vec<_> = std::fs::read_dir(&stage)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| name.to_string_lossy().starts_with(sys::UP))
                .collect();
            assert!(up.is_empty(), "on x {on_x}, pass {pass}: moved up: {up:?}");
            assert_eq!(
                ino(&deep),
                held,
                "on x {on_x}, pass {pass}: x left its path"
            );
            assert_eq!(
                std::fs::read(deep.join("precious")).unwrap(),
                b"under the keep"
            );
            assert!(marker.is_file());
            let again = tree_while(&stage, &|| true);
            assert!(again.is_err(), "on x {on_x}, pass {pass}: {again:?}");
        }
        std::fs::remove_file(&marker).unwrap();
        assert!(tree_while(&stage, &|| true).unwrap().whole());
        assert!(!stage.exists());
    }
}

/// A KEEP PUBLISHED AS A DEEP FOLDER IS RENAMED UP, ITS NAME TAKEN MEANWHILE, STAYS UNDER THE
/// KEEP: as above, a custody marker is published on a folder holding `x` (`kept`, ten folders
/// down) or in `x` itself at the instant before `x` is renamed up, and at the instant after the
/// rename (the remover's `Moved` seam) another folder is made at `x`'s old name, so `x` cannot
/// be put back there. It is put back beside it, in the folder it left, under a name of its own:
/// nothing stands moved up under the stage, where a later pass would read no custody but its own
/// and the stage's, `x` and what it holds stand under `kept`, and later passes stop at the keep
/// whatever order they list the stage in. The marker is withdrawn by hand at the end so the
/// scratch goes. NEGATIVE CONTROL: both seams fired, and the folder made at `x` is the one that
/// stands there.
#[cfg(unix)]
#[test]
fn a_keep_published_as_a_deep_folder_is_renamed_up_and_its_name_taken_stays_under_the_keep() {
    use crate::build_cache::registered::MARKER;
    use std::cell::Cell;
    use std::rc::Rc;
    let dir = crate::testdir::dir("build-cache-remove-deep-late-keep-taken");
    for on_x in [false, true] {
        let stage = dir.join("stage");
        let kept = chain(&stage, 10);
        let deep = chain(&kept, sys::DEPTH - 11).join("x");
        std::fs::create_dir(&deep).unwrap();
        assert_eq!(
            deep.strip_prefix(&stage).unwrap().components().count(),
            sys::DEPTH
        );
        std::fs::write(deep.join("precious"), b"under the keep").unwrap();
        let held = ino(&deep);
        let late = if on_x { &deep } else { &kept }.join(MARKER);
        let _seams = Seams;
        let published = Rc::new(Cell::new(false));
        let once = Rc::clone(&published);
        seams::on(seams::At::Moving, move |name| {
            if name == "x" && !once.replace(true) {
                std::fs::write(&late, b"keep published now").unwrap();
            }
        });
        let made = Rc::new(Cell::new(0));
        let (taken, at) = (Rc::clone(&made), deep.clone());
        seams::on(seams::At::Moved, move |name| {
            if name == "x" && taken.get() == 0 {
                std::fs::create_dir(&at).unwrap();
                taken.set(ino(&at));
            }
        });
        let walked = tree_while(&stage, &|| true);
        seams::clear();
        assert!(published.get(), "on x {on_x}: the keep was never published");
        assert_ne!(made.get(), 0, "on x {on_x}: x's name was never taken");
        assert!(walked.is_err(), "on x {on_x}: a keep was passed over");
        assert_eq!(ino(&deep), made.get(), "on x {on_x}: the folder made at x");
        let beside = || {
            let found: Vec<_> = std::fs::read_dir(deep.parent().unwrap())
                .unwrap()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| ino(path) == held)
                .collect();
            found
        };
        for pass in 0..2 {
            let up: Vec<_> = std::fs::read_dir(&stage)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| name.to_string_lossy().starts_with(sys::UP))
                .collect();
            assert!(up.is_empty(), "on x {on_x}, pass {pass}: moved up: {up:?}");
            let found = beside();
            let [x] = found.as_slice() else {
                panic!("on x {on_x}, pass {pass}: x stands nowhere beside its name: {found:?}");
            };
            assert_eq!(
                std::fs::read(x.join("precious")).unwrap(),
                b"under the keep",
                "on x {on_x}, pass {pass}"
            );
            assert!(if on_x { x } else { &kept }.join(MARKER).is_file());
            let again = tree_while(&stage, &|| true);
            assert!(again.is_err(), "on x {on_x}, pass {pass}: {again:?}");
        }
        let marker = if on_x {
            beside().remove(0)
        } else {
            kept.clone()
        }
        .join(MARKER);
        std::fs::remove_file(&marker).unwrap();
        assert!(tree_while(&stage, &|| true).unwrap().whole());
        assert!(!stage.exists());
    }
}

/// All nine restore destinations can be taken. The pre-rename keep at the held top then
/// survives the stopped pass and protects the escaped inode on a SECOND independent pass.
/// Its original ancestor, replacement names, bytes, mode and ACL remain untouched. The
/// same deep tree without the collision/late keep still goes whole.
#[cfg(unix)]
#[test]
fn a_deep_move_with_every_restore_name_taken_is_durably_kept_across_passes() {
    use crate::build_cache::registered::MARKER;
    use std::cell::Cell;
    use std::rc::Rc;

    let dir = crate::testdir::dir("build-cache-deep-putback-exhausted");
    let lay = |top: &Path| {
        let kept = chain(top, 10);
        let deep = chain(&kept, sys::DEPTH - 11).join("x");
        std::fs::create_dir(&deep).unwrap();
        mode(&deep, 0o755);
        std::fs::write(deep.join("precious"), b"no path out of custody").unwrap();
        (kept, deep)
    };
    let control = dir.join("control");
    lay(&control);
    assert!(tree_while(&control, &|| true).unwrap().whole());
    assert!(!control.exists());

    let top = dir.join("stage");
    let (kept, deep) = lay(&top);
    let inode = ino(&deep);
    #[cfg(target_os = "macos")]
    acl(
        &deep,
        &[if crate::platform::our_uid() == 0 {
            "user:nobody deny list"
        } else {
            "user:root deny list"
        }],
    );
    #[cfg(target_os = "macos")]
    let before_acl = acl_entries(&deep);
    let before_mode = mode_of(&deep);
    let late = kept.join(MARKER);
    let published = Rc::new(Cell::new(false));
    let once = Rc::clone(&published);
    let _seams = Seams;
    seams::on(seams::At::Moving, move |name| {
        if name == "x" && !once.replace(true) {
            std::fs::write(&late, b"ancestor keep").unwrap();
        }
    });
    let occupied = Rc::new(Cell::new(false));
    let made = Rc::clone(&occupied);
    let at = deep.clone();
    seams::on(seams::At::Moved, move |name| {
        if name == "x" && !made.replace(true) {
            std::fs::create_dir(&at).unwrap();
            std::fs::write(at.join("replacement"), b"not ours to overwrite").unwrap();
            for n in 2..=9 {
                let beside = at.parent().unwrap().join(format!("{}-{n}", sys::UP));
                std::fs::create_dir(&beside).unwrap();
                std::fs::write(beside.join("replacement"), b"not ours to overwrite").unwrap();
            }
        }
    });
    assert!(tree_while(&top, &|| true).is_err());
    seams::clear();
    assert!(published.get() && occupied.get());
    let model = aterm_models_atpkg::atpkg_deep_removal_fence_model();
    let mut state = model.init_state();
    for step in ["Arm", "Move", "RestoreFailedOrCrash"] {
        assert!(model.fire(step, &mut state));
    }
    let escaped = top.join(format!("{}-1", sys::UP));
    assert_eq!(ino(&escaped), inode);
    assert_eq!(i64::from(escaped.is_dir()), state["escaped"]);
    let fence = std::fs::read(top.join(MARKER)).unwrap();
    assert!(!crate::build_cache::registered::retention::allows_tree(
        &top
    ));
    assert!(
        tree_while(&top, &|| true).is_err(),
        "second pass must retain"
    );
    assert!(!model.fire("LaterPass", &mut state));
    assert_eq!(i64::from(!escaped.exists()), state["removed"]);
    assert_eq!(std::fs::read(top.join(MARKER)).unwrap(), fence);
    assert_eq!(ino(&escaped), inode);
    assert_eq!(mode_of(&escaped), before_mode);
    #[cfg(target_os = "macos")]
    assert_eq!(acl_entries(&escaped), before_acl);
    assert_eq!(
        std::fs::read(escaped.join("precious")).unwrap(),
        b"no path out of custody"
    );
    assert_eq!(std::fs::read(kept.join(MARKER)).unwrap(), b"ancestor keep");
    for at in std::iter::once(deep.clone())
        .chain((2..=9).map(|n| deep.parent().unwrap().join(format!("{}-{n}", sys::UP))))
    {
        assert_eq!(
            std::fs::read(at.join("replacement")).unwrap(),
            b"not ours to overwrite"
        );
    }
    let sibling = dir.join("ordinary");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(sibling.join("unit"), b"ordinary eligible output").unwrap();
    assert!(tree_while(&sibling, &|| true).unwrap().whole());
}

/// A busy publication lock is not a durable keep proof. The walk must stop BEFORE moving
/// the original inode; it cannot rename first and merely report that fencing failed.
#[cfg(unix)]
#[test]
fn a_deep_move_never_starts_without_its_durable_fence() {
    use crate::build_cache::registered::{MARKER, retention};
    use std::cell::Cell;
    use std::rc::Rc;

    let dir = crate::testdir::dir("build-cache-deep-fence-busy");
    let top = dir.join("stage");
    let deep = chain(&top, sys::DEPTH - 1).join("x");
    std::fs::create_dir(&deep).unwrap();
    std::fs::write(deep.join("precious"), b"never moved without a fence").unwrap();
    let inode = ino(&deep);
    let root = aterm_dirfd::openat(
        aterm_dirfd::CWD,
        &top,
        aterm_dirfd::OFlags::RDONLY
            | aterm_dirfd::OFlags::DIRECTORY
            | aterm_dirfd::OFlags::NOFOLLOW
            | aterm_dirfd::OFlags::CLOEXEC,
        aterm_dirfd::Mode::empty(),
    )
    .unwrap();
    let held = retention::publication_lock(&root).unwrap();
    let moved = Rc::new(Cell::new(false));
    let seen = Rc::clone(&moved);
    let _seams = Seams;
    seams::on(seams::At::Moving, move |_| seen.set(true));
    assert!(tree_while(&top, &|| true).is_err());
    seams::clear();
    assert!(!moved.get());
    assert_eq!(ino(&deep), inode);
    assert_eq!(
        std::fs::read(deep.join("precious")).unwrap(),
        b"never moved without a fence"
    );
    assert!(!top.join(MARKER).exists(), "busy was not a published keep");
    drop(held);
    assert!(tree_while(&top, &|| true).unwrap().whole());
}

/// An ordinary unmarked top may come from umask 002. Sealing that held directory before
/// fence publication preserves normal deep cleanup; existing unknown custody is unchanged.
#[cfg(unix)]
#[test]
fn an_unmarked_group_writable_top_still_allows_normally_eligible_deep_cleanup() {
    use crate::build_cache::registered::MARKER;

    let dir = crate::testdir::dir("build-cache-deep-fence-group-mode");
    for top_mode in [0o775, 0o777] {
        let top = dir.join(format!("top-{top_mode:o}"));
        let deep = chain(&top, sys::DEPTH + 3);
        std::fs::write(deep.join("unit"), b"ordinary deep output").unwrap();
        mode(&top, top_mode);
        assert!(tree_while(&top, &|| true).unwrap().whole());
        assert!(!top.exists());

        let top = dir.join(format!("kept-{top_mode:o}"));
        let deep = chain(&top, sys::DEPTH + 3);
        std::fs::write(deep.join("precious"), b"unknown custody stays").unwrap();
        std::fs::write(top.join(MARKER), b"unknown keep").unwrap();
        mode(&top, top_mode);
        assert!(tree_while(&top, &|| true).is_err());
        assert_eq!(mode_of(&top), top_mode);
        assert_eq!(std::fs::read(top.join(MARKER)).unwrap(), b"unknown keep");
        assert_eq!(
            std::fs::read(deep.join("precious")).unwrap(),
            b"unknown custody stays"
        );
    }
}

/// The real keep API, called while a deep rename is admitted, must revoke the owning
/// pass's provisional exemption. The in-flight rename is put back; the acknowledged
/// explicit marker and every later pass keep the payload.
#[cfg(unix)]
#[test]
fn an_explicit_keep_at_the_deep_move_boundary_revokes_the_live_pass_exemption() {
    use crate::build_cache::registered::{MARKER, retain};
    use std::cell::Cell;
    use std::rc::Rc;

    let dir = crate::testdir::dir("build-cache-deep-fence-explicit-keep");
    let home = dir.canonicalize().unwrap();
    let top = home.join("ordinary.noindex");
    let deep = chain(&top, sys::DEPTH - 1).join("x");
    std::fs::create_dir(&deep).unwrap();
    mode(&top, 0o700);
    std::fs::write(deep.join("precious"), b"explicitly kept").unwrap();
    let inode = ino(&deep);
    let published = Rc::new(Cell::new(false));
    let kept = Rc::clone(&published);
    let at = top.clone();
    let _seams = Seams;
    seams::on(seams::At::Moving, move |name| {
        if name == "x" && !kept.replace(true) {
            retain(&home, &at).unwrap();
        }
    });
    assert!(tree_while(&top, &|| true).is_err());
    seams::clear();
    assert!(published.get());
    let marker = std::fs::read(top.join(MARKER)).unwrap();
    let receipt: aterm_json::Value = aterm_json::from_slice(&marker).unwrap();
    assert_eq!(receipt["retained"].as_bool(), Some(true));
    assert!(receipt["removal_fence"].is_null());
    assert_eq!(ino(&deep), inode);
    assert_eq!(
        std::fs::read(deep.join("precious")).unwrap(),
        b"explicitly kept"
    );
    assert!(tree_while(&top, &|| true).is_err());
    assert_eq!(std::fs::read(top.join(MARKER)).unwrap(), marker);
    assert_eq!(ino(&deep), inode);
}

/// A restored second move does not release the top fence while the first moved folder is
/// still being walked. It has already been popped from the queue, so queue emptiness alone
/// is no proof that no escaped piece remains. The next pass must retain that first inode.
#[cfg(unix)]
#[test]
fn a_successful_nested_restore_keeps_the_fence_for_the_already_popped_outer_move() {
    use crate::build_cache::registered::MARKER;
    use std::cell::Cell;
    use std::rc::Rc;

    let dir = crate::testdir::dir("build-cache-deep-fence-nested-restore");
    let lay = |top: &Path| {
        let outer = chain(top, sys::DEPTH - 1).join("x");
        std::fs::create_dir(&outer).unwrap();
        let inner = chain(&outer, sys::DEPTH - 2).join("y");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(inner.join("precious"), b"nested outstanding output").unwrap();
        (outer, inner)
    };
    let ordinary = dir.join("ordinary");
    lay(&ordinary);
    assert!(tree_while(&ordinary, &|| true).unwrap().whole());

    let top = dir.join("stage");
    let (outer, inner) = lay(&top);
    let outer_inode = ino(&outer);
    let inner_inode = ino(&inner);
    let relative = inner.strip_prefix(&outer).unwrap().to_path_buf();
    let escaped = top.join(format!("{}-1", sys::UP));
    let restored = escaped.join(relative);
    let marker = restored.parent().unwrap().join(MARKER);
    let published = Rc::new(Cell::new(false));
    let wrote = Rc::clone(&published);
    let _seams = Seams;
    seams::on(seams::At::Moving, move |name| {
        if name == "y" && !wrote.replace(true) {
            std::fs::write(&marker, b"nested ancestor keep").unwrap();
        }
    });
    assert!(tree_while(&top, &|| true).is_err());
    seams::clear();
    assert!(published.get());
    assert_eq!(ino(&escaped), outer_inode);
    assert_eq!(ino(&restored), inner_inode);
    assert!(!top.join(format!("{}-2", sys::UP)).exists());
    let fence = std::fs::read(top.join(MARKER)).unwrap();
    assert!(tree_while(&top, &|| true).is_err());
    assert_eq!(std::fs::read(top.join(MARKER)).unwrap(), fence);
    assert_eq!(ino(&escaped), outer_inode);
    assert_eq!(ino(&restored), inner_inode);
    assert_eq!(
        std::fs::read(restored.join("precious")).unwrap(),
        b"nested outstanding output"
    );
}

/// An ACL-repair retry does not forget a moved subtree left by the first walk. Ordinary
/// hardlinks still go on that retry even when enumeration omits the old moved name. A changed
/// or vanished candidate followed by caller revocation, or a missing/replaced old moved name,
/// keeps the old inode, bytes and replacements fenced for SECOND independent passes.
#[cfg(target_os = "macos")]
#[test]
fn an_acl_repair_retry_preserves_the_fence_for_an_earlier_escaped_move() {
    use crate::build_cache::registered::MARKER;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    for (vanished, lost, nested) in [
        (None, None, false),
        (Some(false), None, false),
        (Some(true), None, false),
        (None, Some(false), false),
        (None, Some(true), false),
        (None, None, true),
    ] {
        let dir = crate::testdir::dir("build-cache-deep-fence-acl-retry");
        let top = dir.join("top");
        let outer = chain(&top, sys::DEPTH - 1).join("outer");
        std::fs::create_dir_all(outer.join("deps")).unwrap();
        std::fs::write(outer.join("lib.rlib"), b"ordinary deep linked output").unwrap();
        std::fs::hard_link(outer.join("lib.rlib"), outer.join("deps/lib.rlib")).unwrap();
        acl(&outer.join("lib.rlib"), &[DENY_DELETE]);
        for file in [outer.join("lib.rlib"), outer.join("deps/lib.rlib")] {
            assert!(std::fs::remove_file(file).is_err());
        }
        let original = ino(&outer);
        let starts = Rc::new(Cell::new(0));
        let escaped = Rc::new(RefCell::new(None::<PathBuf>));
        let candidate = Rc::new(RefCell::new(None::<PathBuf>));
        let kept = Rc::new(RefCell::new(None::<PathBuf>));
        let permitted = Rc::new(Cell::new(true));
        let revoked = Rc::new(Cell::new(false));
        let _seams = Seams;
        let (count, root, up, next, out) = (
            Rc::clone(&starts),
            top.clone(),
            Rc::clone(&escaped),
            Rc::clone(&candidate),
            Rc::clone(&kept),
        );
        seams::on(seams::At::Starting, move |_| {
            count.set(count.get() + 1);
            if count.get() != 2 {
                return;
            }
            assert!(root.join(MARKER).is_file());
            let moved: Vec<_> = std::fs::read_dir(&root)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(sys::UP)
                })
                .collect();
            let [moved] = moved.as_slice() else {
                panic!("first walk did not leave exactly one escaped subtree: {moved:?}");
            };
            assert_eq!(ino(moved), original);
            seams::omit(moved.file_name().unwrap());
            *up.borrow_mut() = Some(moved.clone());
            if nested {
                std::fs::write(moved.join("precious"), b"keep the escaped subtree").unwrap();
                std::fs::create_dir(root.join("holder")).unwrap();
            }
            if let Some(replaced) = lost {
                std::fs::write(moved.join("precious"), b"keep the escaped subtree").unwrap();
                let aside = moved.with_file_name("earlier-held");
                std::fs::rename(moved, &aside).unwrap();
                if replaced {
                    std::fs::create_dir(moved).unwrap();
                    std::fs::write(moved.join("replacement"), b"keep the replacement").unwrap();
                }
                *up.borrow_mut() = Some(aside.clone());
                *out.borrow_mut() = Some(aside);
                return;
            }
            if vanished.is_some() {
                let path = chain(moved, sys::DEPTH - 2).join("candidate");
                std::fs::create_dir(&path).unwrap();
                std::fs::write(path.join("precious"), b"keep the escaped subtree").unwrap();
                *next.borrow_mut() = Some(path);
            }
        });
        let (next, out, old, root) = (
            Rc::clone(&candidate),
            Rc::clone(&kept),
            Rc::clone(&escaped),
            top.clone(),
        );
        seams::on(seams::At::Held, move |name| {
            if nested && name == "holder" && out.borrow().is_none() {
                let path = old.borrow().clone().unwrap();
                let aside = root.join("holder/earlier-held");
                std::fs::rename(&path, &aside).unwrap();
                *old.borrow_mut() = Some(aside.clone());
                *out.borrow_mut() = Some(aside);
                return;
            }
            if name != "candidate" || out.borrow().is_some() {
                return;
            }
            let path = next.borrow().clone().unwrap();
            let aside = path.with_file_name("candidate-held");
            std::fs::rename(&path, &aside).unwrap();
            if vanished == Some(false) {
                std::fs::create_dir(&path).unwrap();
            }
            *out.borrow_mut() = Some(aside);
        });
        let (allows, stopped) = (Rc::clone(&permitted), Rc::clone(&revoked));
        seams::on(seams::At::MoveSkipped, move |name| {
            if name == "candidate" {
                stopped.set(true);
                allows.set(false);
            }
        });
        let result = tree_while(&top, &|| permitted.get());
        let omitted = seams::omitted();
        seams::clear();
        assert_eq!(starts.get(), 2, "the real ACL-repair retry must run");
        if lost.is_none() && !nested {
            assert!(omitted > 0, "the cursor never omitted the old moved name");
        }
        if vanished.is_none() && lost.is_none() && !nested {
            assert!(result.unwrap().whole());
            assert!(!top.exists());
            continue;
        }
        if vanished.is_some() {
            assert!(revoked.get(), "candidate never changed before its rename");
        }
        assert!(result.is_err());
        let escaped = escaped.borrow().clone().unwrap();
        let kept = kept.borrow().clone().unwrap();
        let marker = std::fs::read(top.join(MARKER)).unwrap();
        for _ in 0..2 {
            assert_eq!(ino(&escaped), original);
            assert_eq!(std::fs::read(top.join(MARKER)).unwrap(), marker);
            assert_eq!(
                std::fs::read(kept.join("precious")).unwrap(),
                b"keep the escaped subtree"
            );
            assert!(tree_while(&top, &|| true).is_err());
        }
        if lost == Some(true) {
            let replacement = std::fs::read_dir(&top)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.join("replacement").exists())
                .unwrap();
            assert_eq!(
                std::fs::read(replacement.join("replacement")).unwrap(),
                b"keep the replacement"
            );
        }
    }
}

/// A file and link denying removal are repaired without following the link. A folder denying
/// list but permitting search first proves its custody through a search-only handle. The
/// tree goes whole and the link target keeps its ACL.
#[cfg(target_os = "macos")]
#[test]
fn an_entry_denying_its_own_reading_and_removal_goes() {
    let dir = crate::testdir::dir("build-cache-remove-unread");
    let stage = dir.join("stage");
    std::fs::create_dir_all(stage.join("unlisted")).unwrap();
    std::fs::write(stage.join("unread"), b"u").unwrap();
    let target = dir.join("target");
    std::fs::write(&target, b"t").unwrap();
    acl(&target, &[DENY_DELETE]);
    std::os::unix::fs::symlink(&target, stage.join("link")).unwrap();
    acl(&stage.join("unread"), &["group:everyone deny read,delete"]);
    acl(
        &stage.join("unlisted"),
        &["group:everyone deny list,delete"],
    );
    acl(&stage.join("link"), &[DENY_DELETE]);
    assert!(std::fs::remove_file(stage.join("unread")).is_err());
    assert!(std::fs::remove_dir(stage.join("unlisted")).is_err());
    assert!(std::fs::remove_file(stage.join("link")).is_err());

    let left = tree(&stage);
    assert_eq!(left.stayed, 0, "{left:?}");
    assert!(!stage.exists());
    assert!(
        holds_ace(&target, DENY_DELETE),
        "the link's target was cleared"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"t");
}

/// A FILE NAMED OUTSIDE THE TREE TOO IS NEVER CHANGED: a file outside the stage whose ACL denies
/// its removal, hard-linked into the stage (as cargo links what it builds into a target folder),
/// keeps its ACL, which is the file's wherever it is named: its name in the stage stays, counted
/// once, and the entry beside it goes. The scratch's own drop clears the ACL
/// ([`crate::testdir`]). NEGATIVE CONTROL: the name in the stage refuses its removal before the
/// walk.
#[cfg(target_os = "macos")]
#[test]
fn a_file_named_outside_the_tree_too_is_never_changed() {
    let dir = crate::testdir::dir("build-cache-remove-shared");
    let stage = dir.join("stage");
    std::fs::create_dir_all(stage.join("deps")).unwrap();
    std::fs::write(stage.join("deps/beside"), b"b").unwrap();
    let outside = dir.join("outside");
    std::fs::write(&outside, b"o").unwrap();
    std::fs::hard_link(&outside, stage.join("deps/linked")).unwrap();
    acl(&outside, &[DENY_DELETE]);
    assert!(std::fs::remove_file(stage.join("deps/linked")).is_err());

    let left = tree(&stage);
    assert_eq!(left.stayed, 1, "{left:?}");
    assert!(
        holds_ace(&outside, DENY_DELETE),
        "the file outside was changed"
    );
    assert_eq!(stayed(&stage, 10), [stage.join("deps/linked")]);
}

/// A chain of `levels` folders named `d` under `top`, a file in every tenth: the deepest folder.
#[cfg(unix)]
fn chain(top: &Path, levels: usize) -> PathBuf {
    let mut deep = top.to_path_buf();
    for level in 0..levels {
        deep.push("d");
        std::fs::create_dir_all(&deep).unwrap();
        if level % 10 == 0 {
            std::fs::write(deep.join("f"), b"f").unwrap();
        }
    }
    deep
}

/// A FOLDER SWAPPED BEFORE ITS CHANGE IS JUDGED AGAIN, NEVER CHANGED BY ITS NAME: a folder at
/// 0o000 refuses its opening, and right then (the remover's seam) it is replaced by a second
/// name of a file outside the stage at 0o600. The walk reads the name again before it changes
/// anything by it, finds another entry, and judges that: the file's name in the stage goes, the
/// file keeps its mode, and the stage goes whole. NEGATIVE CONTROL: the swap happened (the
/// outside file has two names right after it).
#[cfg(unix)]
#[test]
fn a_folder_swapped_before_its_change_is_judged_again_never_changed_by_its_name() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = crate::testdir::dir("build-cache-remove-preswap");
    let stage = dir.join("stage");
    let sealed = stage.join("sealed");
    std::fs::create_dir_all(&sealed).unwrap();
    mode(&sealed, 0o000);
    let outside = dir.join("outside");
    std::fs::write(&outside, b"o").unwrap();
    mode(&outside, 0o600);
    let _seams = Seams;
    let links = std::rc::Rc::new(std::cell::Cell::new(0));
    let (seen, at, to) = (std::rc::Rc::clone(&links), sealed.clone(), outside.clone());
    seams::on(seams::At::Refused, move |name| {
        if name == "sealed" && seen.get() == 0 {
            std::fs::remove_dir(&at).unwrap();
            std::fs::hard_link(&to, &at).unwrap();
            seen.set(std::fs::symlink_metadata(&to).unwrap().nlink());
        }
    });

    let left = tree(&stage);
    assert_eq!(links.get(), 2, "the swap happened");
    assert!(left.whole() && !stage.exists(), "{left:?}");
    assert_eq!(mode_of(&outside), 0o600, "the file outside was chmodded");
    assert_eq!(std::fs::read(&outside).unwrap(), b"o");
}

/// A FILE SWAPPED BEFORE ITS ACL IS CLEARED KEEPS ITS ACL: a file whose ACL denies its removal
/// refuses it, and right then (the remover's seam) it is replaced by a second name of a file
/// outside the stage whose ACL denies its reading. The walk reads the name again, finds another
/// entry, and judges that: its name in the stage goes, the file outside keeps its ACL, and the
/// stage goes whole. NEGATIVE CONTROL: right after the swap the name in the stage reads as the
/// file outside, holding its ACL.
#[cfg(target_os = "macos")]
#[test]
fn a_file_swapped_before_its_acl_is_cleared_keeps_its_acl() {
    const DENY_READ: &str = "group:everyone deny read";
    let dir = crate::testdir::dir("build-cache-remove-aclswap");
    let stage = dir.join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let f = stage.join("f");
    std::fs::write(&f, b"f").unwrap();
    acl(&f, &[DENY_DELETE]);
    let outside = dir.join("outside");
    std::fs::write(&outside, b"o").unwrap();
    acl(&outside, &[DENY_READ]);
    let _seams = Seams;
    let swapped = std::rc::Rc::new(std::cell::Cell::new(false));
    let (seen, at, to) = (std::rc::Rc::clone(&swapped), f.clone(), outside.clone());
    seams::on(seams::At::Refused, move |name| {
        if name == "f" && !seen.get() {
            let cleared =
                aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/chmod")))
                    .arg("-N")
                    .arg(&at)
                    .status()
                    .unwrap();
            assert!(cleared.success());
            std::fs::remove_file(&at).unwrap();
            std::fs::hard_link(&to, &at).unwrap();
            seen.set(ino(&at) == ino(&to) && holds_ace(&at, DENY_READ));
        }
    });

    let left = tree(&stage);
    assert!(swapped.get(), "the swap happened");
    assert!(left.whole() && !stage.exists(), "{left:?}");
    assert!(
        holds_ace(&outside, DENY_READ),
        "the file outside lost its ACL"
    );
}

/// The remaining name-based ACL repair must act on a raced link itself, never its target.
/// Replacing the file after the final identity read reaches the actual no-follow boundary.
#[cfg(target_os = "macos")]
#[test]
fn file_acl_repair_does_not_follow_a_link_after_its_identity_check() {
    use std::cell::Cell;
    use std::rc::Rc;
    let dir = crate::testdir::dir("build-cache-file-acl-nofollow");
    let stage = dir.join("stage");
    std::fs::create_dir(&stage).unwrap();
    let file = stage.join("file");
    std::fs::write(&file, b"old").unwrap();
    acl(&file, &[DENY_DELETE]);
    let outside = dir.join("outside");
    std::fs::write(&outside, b"outside").unwrap();
    acl(&outside, &["group:everyone deny read"]);
    let swapped = Rc::new(Cell::new(false));
    let (seen, from, to) = (Rc::clone(&swapped), file.clone(), outside.clone());
    let _seams = Seams;
    seams::on(seams::At::Changing, move |name| {
        if name == "file" && !seen.replace(true) {
            assert!(cleared(&from));
            std::fs::remove_file(&from).unwrap();
            std::os::unix::fs::symlink(&to, &from).unwrap();
        }
    });
    assert!(tree(&stage).whole());
    assert!(swapped.get());
    assert!(holds_ace(&outside, "deny read"));
    assert!(cleared(&outside));
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
}

/// An uninspectable directory keeps its permissions even inside an owned parent. Securing
/// the parent does not establish that the child's hidden custody is absent.
#[cfg(unix)]
#[test]
fn an_uninspectable_child_is_not_repaired_by_name() {
    let dir = crate::testdir::dir("build-cache-remove-locked");
    let stage = dir.join("stage");
    let open = stage.join("open");
    std::fs::create_dir_all(open.join("sealed")).unwrap();
    std::fs::write(open.join("sealed/f"), b"f").unwrap();
    mode(&open.join("sealed"), 0o000);
    mode(&open, 0o777);
    assert_eq!(mode_of(&open), 0o777);
    let _seams = Seams;
    let modes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (seen, at) = (std::rc::Rc::clone(&modes), open.clone());
    seams::on(seams::At::Changing, move |name| {
        if name == "sealed" {
            seen.borrow_mut().push(mode_of(&at));
        }
    });

    let left = tree(&stage);
    assert!(
        modes.borrow().is_empty(),
        "uninspectable directory changed by name"
    );
    assert!(!left.whole() && stage.exists(), "{left:?}");
    assert_eq!(mode_of(&open.join("sealed")), 0o000);
    mode(&open.join("sealed"), 0o700);
    assert_eq!(std::fs::read(open.join("sealed/f")).unwrap(), b"f");
}

/// AN ENTRY WHOSE ATTRIBUTES CANNOT BE READ IS LEFT AS IT IS, AND ITS REMEDY NAMED: a file and a
/// folder (holding a file) whose own ACLs deny the reading of their attributes cannot be judged
/// (whose they are, on which volume, under how many names), so each stays, counted once for that
/// reason and named with it, keeping its ACL (it still refuses that read), and the entry beside
/// them goes. The words a pass's readers print name, for each, the `chmod -N` that clears it; run
/// as they say, the next pass takes the stage whole. NEGATIVE CONTROL: before the walk, each
/// refuses a read of its attributes by path.
#[cfg(target_os = "macos")]
#[test]
fn an_entry_whose_attributes_cannot_be_read_is_left_as_it_is() {
    const DENY_READATTR: &str = "group:everyone deny readattr";
    let dir = crate::testdir::dir("build-cache-remove-readattr");
    let stage = dir.join("stage");
    std::fs::create_dir_all(stage.join("folder")).unwrap();
    std::fs::write(stage.join("folder/inner"), b"i").unwrap();
    std::fs::write(stage.join("file"), b"f").unwrap();
    std::fs::write(stage.join("beside"), b"b").unwrap();
    let unread = [stage.join("file"), stage.join("folder")];
    let _clear = Clear(unread.to_vec());
    for at in &unread {
        acl(at, &[DENY_READATTR]);
        assert!(std::fs::symlink_metadata(at).is_err(), "{}", at.display());
    }
    let _seams = Seams;

    let left = tree(&stage);
    assert_eq!(left.stayed, 2, "{left:?}");
    assert_eq!(seams::judged(), [Why::AttributesUnreadable; 2]);
    assert!(!stage.join("beside").exists());
    for at in &unread {
        assert!(
            std::fs::symlink_metadata(at).is_err(),
            "{} lost its ACL",
            at.display()
        );
    }
    assert_eq!(stayed(&stage, 10), unread);
    assert_eq!(
        named(&stage, left.stayed).first,
        [
            (PathBuf::from("file"), Why::AttributesUnreadable),
            (PathBuf::from("folder"), Why::AttributesUnreadable)
        ]
    );
    let words = stayed_words(&stage, left.stayed);
    for at in &unread {
        let remedy = format!(
            "chmod -N {}",
            crate::platform::sh_quote_str(&at.to_string_lossy())
        );
        assert!(words.contains(&remedy), "{remedy} in {words}");
        assert!(cleared(at), "{remedy}");
    }
    assert!(tree(&stage).whole() && !stage.exists());
}

/// The soft descriptor limit [`a_walk_bounds_two_descriptors_per_active_folder`]'s stand-in runs under.
#[cfg(unix)]
const FD_LIMIT: &str = "ATPKG_REMOVE_FD_LIMIT";

/// A WALK BOUNDS TWO DESCRIPTORS PER ACTIVE FOLDER: its iterator and owned retention identity,
/// plus a fixed allowance for root/holder descriptors and the test runner. A chain twice the
/// active depth goes whole under that real limit; keeping descriptors for folders already
/// left would exhaust it. The test binary runs itself as the stand-in under the limit.
/// NEGATIVE CONTROL: the stand-in's limit is in force (opening that many more descriptors than
/// it allows is refused there).
#[cfg(unix)]
#[test]
fn a_walk_bounds_two_descriptors_per_active_folder() {
    let limit = 2 * sys::DEPTH + 24;
    if let Some(dir) = std::env::var_os(FD_LIMIT) {
        let mut rlimit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `rlimit` is a writable `struct rlimit` the call fills.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut rlimit) },
            0
        );
        rlimit.rlim_cur = libc::rlim_t::try_from(limit).unwrap();
        // SAFETY: `rlimit` is a valid `struct rlimit` the call reads.
        let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const rlimit) };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        let top = PathBuf::from(dir).join("top");
        let mut held = Vec::new();
        let refused = (0..limit).any(|_| match std::fs::File::open(&top) {
            Ok(f) => {
                held.push(f);
                false
            }
            Err(_) => true,
        });
        assert!(refused, "the limit is not in force");
        drop(held);
        let left = tree(&top);
        assert!(left.whole(), "{left:?}");
        assert!(!top.exists());
        return;
    }
    for deny_list in [false, true] {
        if deny_list && !cfg!(target_os = "macos") {
            continue;
        }
        let dir = crate::testdir::dir("build-cache-remove-fds");
        chain(&dir.join("top"), 2 * sys::DEPTH + 3);
        #[cfg(target_os = "macos")]
        if deny_list {
            let mut at = dir.join("top");
            for _ in 0..2 * sys::DEPTH + 3 {
                at.push("d");
                acl(&at, &["group:everyone deny list"]);
            }
        }
        let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "build_cache::remove::tests::a_walk_bounds_two_descriptors_per_active_folder",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(FD_LIMIT, &*dir)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .expect("the stand-in");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "deny_list={deny_list}\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!dir.join("top").exists());
    }
}

/// A FILE EVERY NAME OF WHICH IS IN THE TREE GOES WHOLE: a file whose ACL denies its removal,
/// with a second name in another folder of the stage (as cargo links what it builds into the
/// profile folder), has no name outside, so once the walk has met both its ACL is cleared and
/// the stage goes whole. NEGATIVE CONTROL: each name refuses its removal before the walk.
#[cfg(target_os = "macos")]
#[test]
fn a_file_every_name_of_which_is_in_the_tree_goes_whole() {
    let dir = crate::testdir::dir("build-cache-remove-twonames");
    let stage = dir.join("stage");
    std::fs::create_dir_all(stage.join("deps")).unwrap();
    std::fs::write(stage.join("deps/lib.rlib"), b"r").unwrap();
    std::fs::hard_link(stage.join("deps/lib.rlib"), stage.join("lib.rlib")).unwrap();
    acl(&stage.join("lib.rlib"), &[DENY_DELETE]);
    for name in ["deps/lib.rlib", "lib.rlib"] {
        assert!(std::fs::remove_file(stage.join(name)).is_err(), "{name}");
    }

    let left = tree(&stage);
    assert!(left.whole() && !stage.exists(), "{left:?}");
}

/// WHAT STAYED IS NAMED AS THE WALK LEFT IT: a folder read as another user's (the remover's
/// seam), holding three files, stays whole and is counted once, so its readers name the folder
/// itself, never what it holds, and never more entries than stayed. NEGATIVE CONTROL: the files
/// in it are there to be named.
#[cfg(unix)]
#[test]
fn what_stayed_is_named_as_the_walk_left_it() {
    let dir = crate::testdir::dir("build-cache-remove-theirs-named");
    let stage = dir.join("stage");
    let theirs = stage.join("theirs");
    std::fs::create_dir_all(&theirs).unwrap();
    for name in ["a", "b", "c"] {
        std::fs::write(theirs.join(name), name).unwrap();
    }
    std::fs::write(stage.join("mine"), b"m").unwrap();
    let _seams = Seams;
    seams::other_user(ino(&theirs));

    let left = tree(&stage);
    assert_eq!(left.stayed, 1, "{left:?}");
    assert_eq!(std::fs::read_dir(&theirs).unwrap().count(), 3);
    assert_eq!(stayed(&stage, 10), std::slice::from_ref(&theirs));
    let named = named(&stage, left.stayed);
    assert_eq!(
        (named.first, named.count),
        (vec![(PathBuf::from("theirs"), Why::AnotherUser)], 1)
    );
    assert_eq!(super::named(&stage, 0).first, Vec::<(PathBuf, Why)>::new());
}

/// A VOLUME MOUNTED AT THE TREE IS NEVER ENTERED: a tree read as on another volume than the
/// folder holding it (the remover's seam, every entry of it marked, as a volume mounted at the
/// tree reads) stays whole, counted once, nothing in it removed or changed. NEGATIVE CONTROL:
/// unmarked, the same tree goes whole.
#[cfg(unix)]
#[test]
fn a_volume_mounted_at_the_tree_is_never_entered() {
    let dir = crate::testdir::dir("build-cache-remove-mounted");
    let lay = |top: &Path| {
        std::fs::create_dir_all(top.join("deps")).unwrap();
        std::fs::write(top.join("deps/x"), b"x").unwrap();
        mode(&top.join("deps"), 0o500);
    };
    let control = dir.join("control");
    lay(&control);
    assert!(tree(&control).whole() && !control.exists());

    let top = dir.join("top");
    lay(&top);
    let _seams = Seams;
    for at in [top.clone(), top.join("deps"), top.join("deps/x")] {
        seams::other_volume(ino(&at));
    }
    let left = tree(&top);
    assert_eq!(
        left,
        Left {
            stayed: 1,
            freed: 0
        }
    );
    assert_eq!(std::fs::read(top.join("deps/x")).unwrap(), b"x");
    assert_eq!(
        mode_of(&top.join("deps")),
        0o500,
        "the mounted volume was chmodded"
    );
    assert_eq!(stayed(&top, 10), std::slice::from_ref(&top));
}

/// A TREE IN A FOLDER DENYING ITS READING GOES WHOLE: the folder holding the tree at 0o300 (its
/// owner may search and write it, not list it) is searched alone, as a removal of the tree asks
/// no more of it, and the tree goes whole; the folder keeps its mode. NEGATIVE CONTROL: the
/// folder refuses its listing before the walk.
#[cfg(unix)]
#[test]
fn a_tree_in_a_folder_denying_its_reading_goes_whole() {
    /// Gives the holder its owner's bits back when dropped.
    struct Reopen(PathBuf);
    impl Drop for Reopen {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let dir = crate::testdir::dir("build-cache-remove-unlisted-holder");
    let holder = dir.join("holder");
    let top = holder.join("top");
    std::fs::create_dir_all(top.join("deps")).unwrap();
    std::fs::write(top.join("deps/x"), b"x").unwrap();
    let _reopen = Reopen(holder.clone());
    mode(&holder, 0o300);
    assert!(std::fs::read_dir(&holder).is_err(), "the holder lists");

    let left = tree(&top);
    assert!(left.whole(), "{left:?}");
    assert!(std::fs::symlink_metadata(&top).is_err());
    assert_eq!(mode_of(&holder), 0o300);
}

/// A FOLDER IS LISTED AGAIN UNTIL IT HOLDS NOTHING: the stage, each time its listing has been
/// read to its end, twice gains an entry it did not list (the remover's seam: a listing read as
/// the folder empties may skip one), and it is listed again and goes whole. NEGATIVE CONTROL:
/// the entries came after each listing (two calls).
#[cfg(unix)]
#[test]
fn a_folder_is_listed_again_until_it_holds_nothing() {
    let dir = crate::testdir::dir("build-cache-remove-sweeps");
    let stage = dir.join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("a"), b"a").unwrap();
    let _seams = Seams;
    let late = std::rc::Rc::new(std::cell::Cell::new(0));
    let (made, at) = (std::rc::Rc::clone(&late), stage.clone());
    seams::on(seams::At::Listed, move |name| {
        if name == "stage" && made.get() < 2 {
            made.set(made.get() + 1);
            std::fs::write(at.join(format!("late-{}", made.get())), b"l").unwrap();
        }
    });

    let left = tree(&stage);
    assert_eq!(late.get(), 2);
    assert!(left.whole() && !stage.exists(), "{left:?}");
}

/// A FOLDER DENYING THE REMOVAL OF WHAT IT HOLDS IS CLEARED: a folder inside the stage whose
/// ACL denies `delete_child`, holding a file, has its ACL cleared through the folder held open
/// and goes whole with the stage. NEGATIVE CONTROL: its file refuses its removal before the
/// walk.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_denying_the_removal_of_what_it_holds_is_cleared() {
    let dir = crate::testdir::dir("build-cache-remove-delete-child");
    let stage = dir.join("stage");
    let inner = stage.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("f"), b"f").unwrap();
    acl(&inner, &["group:everyone deny delete_child"]);
    assert!(std::fs::remove_file(inner.join("f")).is_err());

    let left = tree(&stage);
    assert!(left.whole() && !stage.exists(), "{left:?}");
}

/// A TREE DENYING ITS OWN REMOVAL GOES: `top` itself carrying `deny delete` (its holder never
/// changed, so never by its name there) has its ACL cleared through the folder held open once it
/// is emptied, and goes whole; the folder holding it keeps its mode. NEGATIVE CONTROL:
/// `top` refuses its removal once emptied by path.
#[cfg(target_os = "macos")]
#[test]
fn a_tree_denying_its_own_removal_goes() {
    let dir = crate::testdir::dir("build-cache-remove-top-acl");
    let holder = dir.join("holder");
    let (top, control) = (holder.join("top"), holder.join("control"));
    for at in [&top, &control] {
        std::fs::create_dir_all(at).unwrap();
        std::fs::write(at.join("f"), b"f").unwrap();
        acl(at, &[DENY_DELETE]);
    }
    std::fs::remove_file(control.join("f")).unwrap();
    assert!(std::fs::remove_dir(&control).is_err());
    let holder_mode = mode_of(&holder);

    let left = tree(&top);
    assert!(left.whole(), "{left:?}");
    assert!(std::fs::symlink_metadata(&top).is_err());
    assert_eq!(mode_of(&holder), holder_mode);
}

/// A chain of `levels` folders each named `name` under `top`, made by descriptor (no path call
/// reaches past `PATH_MAX`), with the file `f` in the deepest: its inode, and its name under `top`.
#[cfg(unix)]
fn deep_file(top: &Path, name: &str, levels: usize) -> (u64, PathBuf) {
    use aterm_dirfd::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = aterm_dirfd::openat(aterm_dirfd::CWD, top, flags, Mode::empty()).unwrap();
    let mut rel = PathBuf::new();
    for _ in 0..levels {
        aterm_dirfd::mkdirat(&fd, name, Mode::RWXU).unwrap();
        fd = aterm_dirfd::openat(&fd, name, flags, Mode::empty()).unwrap();
        rel.push(name);
    }
    let file = aterm_dirfd::openat(
        &fd,
        "f",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .unwrap();
    let ino = aterm_dirfd::fstat(&file).unwrap().st_ino;
    rel.push("f");
    (ino, rel)
}

/// WHAT STAYED PAST ANY PATH IS NAMED AS THE WALK LEFT IT: in a tree deeper than any path call
/// takes (`PATH_MAX`), the deepest file, read as another user's (the remover's seam), stays alone
/// of the tree, counted once for that reason; its readers name that file, by its name under the
/// stage, with the reason the removal gave, walking by descriptor as the removal walks.
/// NEGATIVE CONTROL: a walk by path (as the readers walked) cannot read past `PATH_MAX`, so it
/// names the first folder too deep to read instead.
#[cfg(unix)]
#[test]
fn what_stayed_past_any_path_is_named_as_the_walk_left_it() {
    let dir = crate::testdir::dir("build-cache-remove-deep-named");
    let top = dir.join("top");
    std::fs::create_dir(&top).unwrap();
    let (ino, rel) = deep_file(&top, &"n".repeat(200), 24);
    assert!(top.join(&rel).as_os_str().len() > 4096);
    assert!(
        std::fs::symlink_metadata(top.join(&rel)).is_err(),
        "a path call reaches the deepest file"
    );
    let _seams = Seams;
    seams::other_user(ino);

    let left = tree(&top);
    assert_eq!(left.stayed, 1, "{left:?}");
    assert_eq!(seams::judged(), [Why::AnotherUser]);
    let named = named(&top, left.stayed);
    assert_eq!(named.first, [(rel.clone(), Why::AnotherUser)]);
    assert_eq!(stayed(&top, 10), [top.join(&rel)]);
    assert!(
        named.words(&top).contains(&*rel.to_string_lossy()),
        "{}",
        named.words(&top)
    );
}

/// Clears the ACL of each of its paths (`chmod -N`) when dropped, before the scratch goes: an
/// entry denying the reading of its attributes, which no remover acts on.
#[cfg(target_os = "macos")]
struct Clear(Vec<PathBuf>);

#[cfg(target_os = "macos")]
impl Drop for Clear {
    fn drop(&mut self) {
        for at in &self.0 {
            let _ =
                aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/chmod")))
                    .arg("-N")
                    .arg(at)
                    .status();
        }
    }
}

/// `chmod -N` of `at`, as a person runs it: whether it cleared.
#[cfg(target_os = "macos")]
fn cleared(at: &Path) -> bool {
    aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/chmod")))
        .arg("-N")
        .arg(at)
        .status()
        .is_ok_and(|s| s.success())
}

/// What `top` holds, every entry reachable by path, as a path walk reads it: each entry's type,
/// mode, owner, names, size and change and modification times to the nanosecond (a change of a
/// mode, an ACL, a flag or a name moves the change time), a file's bytes, and what refuses to be
/// read or listed.
#[cfg(unix)]
fn snapshot(top: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt as _;
    let mut out = Vec::new();
    let mut stack = vec![top.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(m) = std::fs::symlink_metadata(&at) else {
            out.push(format!("{} unreadable", at.display()));
            continue;
        };
        out.push(format!(
            "{} {:o} {} {} {} {}.{} {}.{}",
            at.display(),
            m.mode(),
            m.uid(),
            m.nlink(),
            m.len(),
            m.ctime(),
            m.ctime_nsec(),
            m.mtime(),
            m.mtime_nsec()
        ));
        if m.is_file() {
            out.push(format!("{} {:?}", at.display(), std::fs::read(&at).ok()));
        }
        if m.is_dir() {
            match std::fs::read_dir(&at) {
                Ok(listing) => {
                    let mut children: Vec<PathBuf> = listing.flatten().map(|e| e.path()).collect();
                    children.sort();
                    stack.extend(children.into_iter().rev());
                }
                Err(_) => out.push(format!("{} unlisted", at.display())),
            }
        }
    }
    out
}

/// THE FORECAST CHANGES NOTHING, AND FORECASTS WHAT THE JUDGE LEAVES: a stage holding a folder
/// read as another user's (at 0o500, on macOS denying its removal by ACL), a folder read as on
/// another volume (the remover's seams), a folder at 0o000, a folder at 0o777, a link, a second
/// name of a file outside, and on macOS a file whose ACL denies reading its attributes, a folder
/// denying its search and a file denying its removal, is forecast: what will stay is the other
/// user's, the other volume's and (on macOS) the unreadable one, each named with its reason in
/// name order; owned folders whose contents could not be read make the forecast incomplete.
/// Every entry's mode, owner, names, times and bytes are the same after it, and each ACL is
/// still there. NEGATIVE CONTROL: actually changing the sealed folder's permission bits is
/// visible in the snapshot, without relying on a filesystem timestamp tick.
#[cfg(unix)]
#[test]
fn a_forecast_changes_nothing() {
    let dir = crate::testdir::dir("build-cache-remove-forecast");
    let stage = dir.join("stage");
    for at in ["theirs", "mount", "sealed", "open", "mine/deps"] {
        std::fs::create_dir_all(stage.join(at)).unwrap();
    }
    for at in ["theirs/f", "mount/f", "sealed/f", "open/x", "mine/deps/x"] {
        std::fs::write(stage.join(at), at).unwrap();
    }
    let outside = dir.join("outside");
    std::fs::write(&outside, b"o").unwrap();
    std::fs::hard_link(&outside, stage.join("linked")).unwrap();
    std::os::unix::fs::symlink(&outside, stage.join("link")).unwrap();
    #[cfg(target_os = "macos")]
    let _clear = {
        std::fs::create_dir_all(stage.join("shut")).unwrap();
        std::fs::write(stage.join("shut/f"), b"f").unwrap();
        std::fs::write(stage.join("unread"), b"u").unwrap();
        std::fs::write(stage.join("held"), b"h").unwrap();
        acl(&stage.join("theirs"), &[DENY_DELETE]);
        acl(&stage.join("held"), &[DENY_DELETE]);
        acl(&outside, &[DENY_DELETE]);
        acl(&stage.join("shut"), &["group:everyone deny search"]);
        acl(&stage.join("unread"), &["group:everyone deny readattr"]);
        Clear(vec![stage.join("unread")])
    };
    mode(&stage.join("theirs"), 0o500);
    mode(&stage.join("sealed"), 0o000);
    mode(&stage.join("open"), 0o777);
    let _seams = Seams;
    seams::other_user(ino(&stage.join("theirs")));
    seams::other_volume(ino(&stage.join("mount")));
    let before = snapshot(&dir);

    let forecast = forecast(&stage, 10);
    #[cfg_attr(
        not(target_os = "macos"),
        expect(unused_mut, reason = "macOS adds the unreadable entry")
    )]
    let mut stays = vec![
        (PathBuf::from("mount"), Why::AnotherVolume),
        (PathBuf::from("theirs"), Why::AnotherUser),
    ];
    #[cfg(target_os = "macos")]
    stays.push((PathBuf::from("unread"), Why::AttributesUnreadable));
    assert_eq!(forecast.first, stays, "{forecast:?}");
    assert_eq!(forecast.count, stays.len());
    assert!(
        !forecast.whole && forecast.walked > stays.len(),
        "{forecast:?}"
    );
    assert_eq!(snapshot(&dir), before, "the forecast changed the tree");
    #[cfg(target_os = "macos")]
    {
        for (at, ace) in [
            ("stage/theirs", DENY_DELETE),
            ("stage/held", DENY_DELETE),
            ("outside", DENY_DELETE),
            ("stage/shut", "deny search"),
        ] {
            assert!(holds_ace(&dir.join(at), ace), "{at} lost {ace}");
        }
        assert!(std::fs::symlink_metadata(stage.join("unread")).is_err());
    }

    mode(&stage.join("sealed"), 0o700);
    assert_ne!(
        snapshot(&dir),
        before,
        "the changed permission bits are seen"
    );
}

/// A sealed owned directory is not proof about the children a forecast cannot read. Its
/// foreign-owned child is hidden from the read-only pass; incomplete is the only truthful
/// answer. Removal also preserves the parent when neither search nor listing can prove that
/// its hidden custody is absent.
#[cfg(unix)]
#[test]
fn a_forecast_of_unread_children_is_incomplete_and_preserves_the_hidden_foreign_entry() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = aterm_tempfile::tempdir()
        .expect("private unread forecast world")
        .keep();
    eprintln!("retained unread forecast fixture: {}", dir.display());
    let stage = dir.join("stage");
    let sealed = stage.join("sealed");
    let foreign = sealed.join("foreign");
    std::fs::create_dir_all(&sealed).unwrap();
    std::fs::write(&foreign, b"hidden foreign bytes").unwrap();
    let foreign_inode = ino(&foreign);
    mode(&sealed, 0o000);
    let state = |path: &Path| {
        let meta = std::fs::symlink_metadata(path).unwrap();
        (
            meta.mode(),
            meta.uid(),
            meta.ctime(),
            meta.ctime_nsec(),
            meta.mtime(),
            meta.mtime_nsec(),
        )
    };
    let before = (state(&stage), state(&sealed));
    let _clear = Seams;
    seams::other_user(foreign_inode);

    let looked = forecast(&stage, NAMED);
    assert!(
        !looked.whole,
        "unread children were called a complete forecast: {looked:?}"
    );
    assert_eq!(looked.count, 0, "no unseen child's ownership was guessed");
    assert!(looked.first.is_empty());
    assert_eq!(
        (state(&stage), state(&sealed)),
        before,
        "forecast changed modes or ctimes"
    );

    let left = tree(&stage);
    assert_eq!(left.stayed, 1, "the hidden foreign data stays: {left:?}");
    let kept = named(&stage, left.stayed);
    assert_eq!(mode_of(&sealed), 0o000);
    assert_eq!(kept.first, [(stage.clone(), Why::PermissionRevoked)]);
    mode(&sealed, 0o700);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"hidden foreign bytes");
}

/// A FORECAST IS BOUNDED BY ITS VISITS: in a stage of a folder of 300 files and, after it in name
/// order, a file read as another user's (the remover's seam), a walk allowed 50 visits judges no
/// more than 50 entries, names nothing it did not reach, and says it was cut short, as a dry plan
/// of a stage of millions says it; allowed [`VISITS`], the same walk reaches every entry and
/// names the other user's file. NEGATIVE CONTROL: unbounded, the small walk reaches it too.
#[cfg(unix)]
#[test]
fn a_forecast_is_bounded_by_its_visits() {
    let dir = crate::testdir::dir("build-cache-remove-visits");
    let stage = dir.join("stage");
    std::fs::create_dir_all(stage.join("a")).unwrap();
    std::fs::create_dir_all(stage.join("z")).unwrap();
    for i in 0..300 {
        std::fs::write(stage.join(format!("a/f{i:03}")), b"").unwrap();
    }
    std::fs::write(stage.join("z/theirs"), b"t").unwrap();
    let _seams = Seams;
    seams::other_user(ino(&stage.join("z/theirs")));

    let cut = sys::look(&stage, NAMED, false, 50);
    assert_eq!(
        (cut.count, cut.walked, cut.whole, cut.first.len()),
        (0, 50, false, 0),
        "{cut:?}"
    );
    assert!(
        cut.words(&stage)
            .is_some_and(|w| w.contains(&cut.walked.to_string())),
        "{:?}",
        cut.words(&stage)
    );
    let left = sys::look(&stage, NAMED, true, 50);
    assert_eq!((left.walked, left.whole), (50, false), "{left:?}");

    let whole = forecast(&stage, NAMED);
    assert_eq!(
        (whole.count, whole.walked, whole.whole),
        (1, 304, true),
        "{whole:?}"
    );
    assert_eq!(whole.first, [(PathBuf::from("z/theirs"), Why::AnotherUser)]);
}

/// EACH ENTRY THAT STAYS IS NAMED WITH THE REASON THE REMOVAL JUDGED: of a stage holding a file
/// read as another user's and a folder read as on another volume (the remover's seams), and on
/// macOS a file whose ACL denies reading its attributes, a file flagged `uchg` (which the remover
/// never clears) and a file denying its removal with a second name outside the stage, the
/// readers name every entry that stayed with the reason the removal counted it for, one each.
/// NEGATIVE CONTROL: each entry beside them of this user's goes.
#[cfg(unix)]
#[test]
fn each_entry_that_stays_is_named_with_the_reason_the_removal_judged() {
    let dir = crate::testdir::dir("build-cache-remove-reasons");
    let stage = dir.join("stage");
    std::fs::create_dir_all(stage.join("mount")).unwrap();
    std::fs::create_dir_all(stage.join("mine")).unwrap();
    std::fs::write(stage.join("mount/f"), b"f").unwrap();
    std::fs::write(stage.join("theirs"), b"t").unwrap();
    std::fs::write(stage.join("mine/x"), b"x").unwrap();
    #[cfg_attr(
        not(target_os = "macos"),
        expect(unused_mut, reason = "macOS adds the entries its ACLs and flags keep")
    )]
    let mut want = vec![
        (PathBuf::from("mount"), Why::AnotherVolume),
        (PathBuf::from("theirs"), Why::AnotherUser),
    ];
    #[cfg(target_os = "macos")]
    let (_clear, _unflag) = {
        /// Clears the flag when dropped.
        struct Unflag(PathBuf);
        impl Drop for Unflag {
            fn drop(&mut self) {
                let _ = crate::platform::set_file_flags(&self.0, 0);
            }
        }
        let outside = dir.join("outside");
        std::fs::write(&outside, b"o").unwrap();
        std::fs::hard_link(&outside, stage.join("linked")).unwrap();
        acl(&outside, &[DENY_DELETE]);
        std::fs::write(stage.join("unread"), b"u").unwrap();
        acl(&stage.join("unread"), &["group:everyone deny readattr"]);
        std::fs::write(stage.join("flagged"), b"g").unwrap();
        crate::platform::set_file_flags(&stage.join("flagged"), libc::UF_IMMUTABLE).unwrap();
        want.extend([
            (PathBuf::from("flagged"), Why::StillRefusing),
            (PathBuf::from("linked"), Why::NameOutsideTree),
            (PathBuf::from("unread"), Why::AttributesUnreadable),
        ]);
        want.sort_by(|a, b| a.0.cmp(&b.0));
        (
            Clear(vec![stage.join("unread")]),
            Unflag(stage.join("flagged")),
        )
    };
    let _seams = Seams;
    seams::other_user(ino(&stage.join("theirs")));
    seams::other_volume(ino(&stage.join("mount")));

    let left = tree(&stage);
    assert_eq!(left.stayed, want.len(), "{left:?}");
    assert!(!stage.join("mine").exists());
    let named = sys::look(&stage, 10, true, VISITS);
    assert_eq!(named.first, want);
    let mut judged = seams::judged();
    let mut reasons: Vec<Why> = want.iter().map(|(_, why)| *why).collect();
    let key = |why: &Why| format!("{why:?}");
    judged.sort_by_key(key);
    reasons.sort_by_key(key);
    assert_eq!(judged, reasons, "the removal's reasons");
    assert_eq!(
        super::named(&stage, left.stayed).first,
        want[..NAMED.min(want.len())]
    );
}

/// `path`'s identity as an identified removal is given it: its volume and inode.
#[cfg(unix)]
fn identity_of(path: &Path) -> super::super::registered::Identity {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).unwrap();
    super::super::registered::Identity {
        device: meta.dev(),
        inode: meta.ino(),
    }
}

/// A TOP A REMOVAL DOES NOT ENTER IS NAMED ITSELF, WITH THE REMOVAL'S REASON: a tree holding a
/// folder and a file, given to an identified removal as another folder's (its sibling's
/// identity), or held, as that removal is told, by another folder than the one holding it,
/// stays whole and is never entered, counted once as not the folder it was given; a plain tree
/// whose holder lets nothing in it be looked up (0o600: read, no search) stays whole too, as
/// one it cannot reach. Each time the removal's readers name the tree itself with the removal's
/// reason, never an entry inside it, and the forecast of that removal says the same before it
/// runs. NEGATIVE CONTROL: the same trees, given as themselves and opened up, go whole.
#[cfg(unix)]
#[test]
fn a_top_the_removal_does_not_enter_is_named_itself_with_its_reason() {
    let dir = crate::testdir::dir("build-cache-remove-unentered");
    let (top, sibling) = (dir.join("top"), dir.join("sibling"));
    for at in [&top, &sibling] {
        std::fs::create_dir_all(at.join("inner")).unwrap();
        std::fs::write(at.join("inner/f"), b"f").unwrap();
    }
    let _seams = Seams;
    let named_itself = |at: &Path, left: Left, why: Why| {
        assert_eq!(
            left,
            Left {
                stayed: 1,
                freed: 0
            },
            "{}",
            at.display()
        );
        assert_eq!(seams::judged(), [why], "the removal's reason");
        let said = named(at, left.stayed);
        assert_eq!(said.first, [(at.to_path_buf(), why)]);
        let words = said.words(at);
        assert!(words.contains(&why.words(at)), "{words}");
        assert!(!words.contains("inner"), "named inside: {words}");
        seams::clear();
    };
    let (held, own, other) = (identity_of(&dir), identity_of(&top), identity_of(&sibling));

    let replaced = forecast_tree_identified(&top, &held, &other, &[], NAMED);
    assert_eq!(
        (replaced.count, replaced.first),
        (1, vec![(PathBuf::new(), Why::NotIdentified)])
    );
    let left = tree_identified(&top, &held, &other, &[], &|| true).unwrap();
    named_itself(&top, left, Why::NotIdentified);
    let moved = forecast_tree_identified(&top, &other, &own, &[], NAMED);
    assert_eq!(moved.first, [(PathBuf::new(), Why::NotIdentified)]);
    let left = tree_identified(&top, &other, &own, &[], &|| true).unwrap();
    named_itself(&top, left, Why::NotIdentified);
    assert!(top.join("inner/f").is_file(), "the removal entered it");

    let unsearched = dir.join("unsearched");
    let shut = unsearched.join("top");
    std::fs::create_dir_all(shut.join("inner")).unwrap();
    std::fs::write(shut.join("inner/f"), b"f").unwrap();
    /// Gives the holder its owner's bits back when dropped.
    struct Reopen(PathBuf);
    impl Drop for Reopen {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let reopen = Reopen(unsearched.clone());
    mode(&unsearched, 0o600);
    assert_eq!(
        forecast(&shut, NAMED).first,
        [(PathBuf::new(), Why::Unreachable)]
    );
    let left = tree(&shut);
    named_itself(&shut, left, Why::Unreachable);
    drop(reopen);
    assert!(shut.join("inner/f").is_file(), "the removal entered it");

    assert!(
        tree_identified(&top, &held, &own, &[], &|| true)
            .unwrap()
            .whole()
    );
    assert!(tree(&shut).whole());
    assert!(!top.exists() && !shut.exists());
}

/// A STAGE TAKEN ONLY ONCE EMPTY NAMES WHAT IT HOLDS AS NEVER GIVEN: a stage whose payload an
/// identified removal took whole, holding a folder it was never given, is left by the removal
/// that takes it only once empty, which never enters that folder: counted once, the stage, for
/// what it holds, and its readers name the stage with that reason, never the entry inside the
/// folder; the forecast of the two removals, made before them, says the same. When the payload
/// itself keeps an entry (read as another user's), the stage is never tried, and its readers,
/// told what stayed under the stage, name the payload's entry under it, never the folder beside.
/// NEGATIVE CONTROL: the folder beside sorts first, so a walk of what stands names its entry.
#[cfg(unix)]
#[test]
fn a_stage_taken_only_once_empty_names_what_it_holds_as_never_given() {
    let dir = crate::testdir::dir("build-cache-remove-emptied");
    let stage = dir.join("stage");
    let payload = stage.join("cache.noindex");
    std::fs::create_dir_all(payload.join("debug")).unwrap();
    std::fs::write(payload.join("debug/x"), b"x").unwrap();
    std::fs::create_dir_all(stage.join("a-foreign/sub")).unwrap();
    std::fs::write(stage.join("a-foreign/sub/y"), b"y").unwrap();
    // A registered pending stage is private regardless of ambient umask. In particular a
    // guest's 0002 umask must not create a group-writable holder that correctly fails identity.
    mode(&stage, 0o700);
    assert_eq!(mode_of(&stage), 0o700);
    let (held, staged, root) = (
        identity_of(&dir),
        identity_of(&stage),
        identity_of(&payload),
    );
    let _seams = Seams;
    seams::other_user(ino(&payload.join("debug/x")));

    let kept = forecast_tree_identified(&payload, &staged, &root, &[], NAMED);
    assert_eq!(kept.first, [(PathBuf::from("debug/x"), Why::AnotherUser)]);
    let left = tree_identified(&payload, &staged, &root, &[], &|| true).unwrap();
    assert_eq!(left.stayed, 1);
    named_as(&payload, &stage);
    let said = named(&stage, left.stayed);
    assert_eq!(
        said.first,
        [(PathBuf::from("cache.noindex/debug/x"), Why::AnotherUser)]
    );
    assert!(!said.words(&stage).contains("a-foreign"), "{said:?}");
    assert_eq!(
        sys::look(&stage, NAMED, true, VISITS).first[0],
        (PathBuf::from("a-foreign/sub/y"), Why::StillRefusing),
        "the walk of what stands names the folder beside first"
    );
    seams::clear();

    assert!(
        forecast_tree_identified(&payload, &staged, &root, &[], NAMED)
            .first
            .is_empty()
    );
    let alone = forecast_empty_identified(&stage, &staged, &["cache.noindex"], NAMED);
    assert_eq!(
        (alone.count, alone.first),
        (1, vec![(PathBuf::new(), Why::HoldsOthers)])
    );
    assert!(
        tree_identified(&payload, &staged, &root, &[], &|| true)
            .unwrap()
            .whole()
    );
    let left = empty_identified(&stage, &staged, &|| true).unwrap();
    assert_eq!(left.stayed, 1, "{left:?}");
    assert_eq!(seams::judged(), [Why::HoldsOthers]);
    let said = named(&stage, left.stayed);
    assert_eq!(said.first, [(stage.clone(), Why::HoldsOthers)]);
    assert!(!said.words(&stage).contains("sub"), "{said:?}");
    assert!(
        stage.join("a-foreign/sub/y").is_file(),
        "the removal entered it"
    );
    assert!(!payload.exists());
    let _ = held;
}

/// A PROTECTED NAME STOPS THE FORECAST AS IT STOPS THE REMOVAL: a payload holding `Cargo.toml`
/// at its root, forecast by an identified removal's rules with that name protected, says its
/// removal stops before anything goes and forecasts nothing past it; the removal itself is
/// revoked with nothing gone. NEGATIVE CONTROL: with no name protected, nothing stops it, and the
/// removal takes it whole.
#[cfg(unix)]
#[test]
fn a_protected_name_stops_the_forecast_as_it_stops_the_removal() {
    let dir = crate::testdir::dir("build-cache-remove-protected");
    let top = dir.join("top");
    std::fs::create_dir_all(top.join("debug")).unwrap();
    std::fs::write(top.join("debug/x"), b"x").unwrap();
    std::fs::write(top.join("Cargo.toml"), b"[workspace]\n").unwrap();
    let (held, root) = (identity_of(&dir), identity_of(&top));
    let protected = ["Cargo.toml"];

    let stopped = forecast_tree_identified(&top, &held, &root, &protected, NAMED);
    assert_eq!(
        stopped.stops,
        Some(PathBuf::from("Cargo.toml")),
        "{stopped:?}"
    );
    assert_eq!(stopped.count, 0);
    let words = stopped.words(&top).unwrap();
    assert!(
        words.contains("stops before anything goes: it holds Cargo.toml"),
        "{words}"
    );
    assert_eq!(
        tree_identified(&top, &held, &root, &protected, &|| true),
        Err(Revoked { freed: 0 })
    );
    assert!(top.join("debug/x").is_file() && top.join("Cargo.toml").is_file());

    let free = forecast_tree_identified(&top, &held, &root, &[], NAMED);
    assert_eq!((&free.stops, free.count, free.whole), (&None, 0, true));
    assert_eq!(free.words(&top), None);
    assert!(
        tree_identified(&top, &held, &root, &[], &|| true)
            .unwrap()
            .whole()
    );
}

/// A PROTECTED ROOT DENYING ITS SEARCH IS LEFT AS IT STANDS: an identified removal, with
/// `Cargo.toml` protected, of a payload holding it whose root lacks its owner's search (0o600),
/// and on macOS of one whose ACL denies `search` beside another entry, is revoked with nothing
/// gone, and the root keeps its mode and its whole ACL: the names that make it someone's are
/// read in it before anything of it changes, and a root that denies its search cannot show them
/// absent. NEGATIVE CONTROL: with no name protected, the same root at 0o600 is let searched and
/// goes whole.
#[cfg(unix)]
#[test]
fn a_protected_root_denying_its_search_is_left_as_it_stands() {
    let dir = crate::testdir::dir("build-cache-remove-protected-shut");
    let lay = |top: &Path| {
        std::fs::create_dir_all(top.join("debug")).unwrap();
        std::fs::write(top.join("debug/x"), b"x").unwrap();
        std::fs::write(top.join("Cargo.toml"), b"[workspace]\n").unwrap();
    };
    let held = identity_of(&dir);
    let protected = ["Cargo.toml"];

    let shut = dir.join("shut");
    lay(&shut);
    let root = identity_of(&shut);
    mode(&shut, 0o600);
    let walked = tree_identified(&shut, &held, &root, &protected, &|| true);
    let after = mode_of(&shut);
    mode(&shut, 0o700);
    assert_eq!(walked, Err(Revoked { freed: 0 }));
    assert_eq!(after, 0o600, "the protected root was let searched");
    assert!(shut.join("debug/x").is_file() && shut.join("Cargo.toml").is_file());

    #[cfg(target_os = "macos")]
    {
        let denied = dir.join("denied");
        lay(&denied);
        let root = identity_of(&denied);
        acl(
            &denied,
            &["group:everyone deny search", "group:staff allow read"],
        );
        assert!(holds_ace(&denied, "group:staff allow list"));
        let walked = tree_identified(&denied, &held, &root, &protected, &|| true);
        let kept = holds_ace(&denied, "group:everyone deny search")
            && holds_ace(&denied, "group:staff allow list");
        let status =
            aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/chmod")))
                .arg("-N")
                .arg(&denied)
                .status()
                .expect("chmod -N");
        assert!(status.success(), "chmod -N {}: {status}", denied.display());
        assert_eq!(walked, Err(Revoked { freed: 0 }));
        assert!(kept, "the protected root's ACL was cleared");
        assert!(denied.join("debug/x").is_file() && denied.join("Cargo.toml").is_file());
    }

    mode(&shut, 0o600);
    assert!(
        tree_identified(&shut, &held, &root, &[], &|| true)
            .unwrap()
            .whole()
    );
    assert!(!shut.exists());
}

/// A TOP THAT WILL NOT OPEN IS FORECAST TO STAY, AS IT STAYS: a stage of this user's at 0o000
/// holding a folder and a file has unproved custody and is forecast incomplete, then remains
/// unchanged by removal and is named as one opaque retained root. A folder at 0o000 below the
/// stage is incomplete in the read-only forecast because its contents could not be read; the
/// actual removal also keeps it unchanged because hidden custody is unproved.
/// NEGATIVE CONTROL: opened up, the sealed stage is forecast to go, and goes whole.
#[cfg(unix)]
#[test]
fn a_top_that_will_not_open_is_forecast_to_stay_as_it_stays() {
    let dir = crate::testdir::dir("build-cache-remove-sealed-top");
    let (stage, below) = (dir.join("stage"), dir.join("below"));
    for at in [&stage, &below] {
        std::fs::create_dir_all(at.join("d")).unwrap();
        std::fs::write(at.join("d/f"), b"f").unwrap();
    }
    /// Gives the stage its owner's bits back when dropped.
    struct Reopen(PathBuf);
    impl Drop for Reopen {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let _reopen = Reopen(stage.clone());
    mode(&stage, 0o000);
    mode(&below.join("d"), 0o000);
    let _seams = Seams;

    let sealed = forecast(&stage, NAMED);
    assert_eq!(
        (sealed.count, sealed.first, sealed.whole),
        (1, vec![(PathBuf::new(), Why::PermissionRevoked)], false)
    );
    let left = tree(&stage);
    assert_eq!(
        left,
        Left {
            stayed: 1,
            freed: 0
        }
    );
    assert!(seams::judged().is_empty());
    assert_eq!(
        named(&stage, 1).first,
        [(stage.clone(), Why::PermissionRevoked)]
    );
    seams::clear();

    let opened = forecast(&below, NAMED);
    assert_eq!((opened.count, opened.whole), (0, false), "{opened:?}");
    assert!(!tree(&below).whole() && below.exists());
    assert_eq!(mode_of(&below.join("d")), 0o000);
    mode(&below.join("d"), 0o700);
    assert!(tree(&below).whole() && !below.exists());

    mode(&stage, 0o700);
    assert_eq!(forecast(&stage, NAMED).count, 0);
    assert!(tree(&stage).whole() && !stage.exists());
}

/// A REMOVAL SAYS IT IS STILL GOING, ENTRY BY ENTRY, where it goes entry by entry: a tree of a
/// million entries takes minutes on a swapping machine. Off Unix it is one call, so one tick.
#[test]
fn a_removal_ticks_for_each_entry() {
    const ENTRIES: usize = 8;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let top = scratch.path().join("top");
    std::fs::create_dir(&top).unwrap();
    for i in 0..ENTRIES {
        std::fs::write(top.join(i.to_string()), b"x").unwrap();
    }
    let before = aterm_uds::progress::ticks_here();
    assert!(tree(&top).whole());
    let ticks = aterm_uds::progress::ticks_here() - before;
    assert!(!top.exists());
    assert!(
        ticks >= if cfg!(unix) { ENTRIES } else { 1 },
        "{ticks} ticks"
    );
}
