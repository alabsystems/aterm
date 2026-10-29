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

/// A tree that is not there is gone already: whole, nothing freed.
#[test]
fn a_missing_tree_is_gone_already() {
    let dir = crate::testdir::dir("build-cache-remove-missing");
    assert_eq!(tree(&dir.join("absent")), Left::default());
    assert!(tree(&dir.join("absent")).whole());
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
            PathBuf::from("debug/deps/x"),
            PathBuf::from("tmp/a"),
            PathBuf::from("tmp/b/d")
        ]
    );
    assert_eq!((four.count, four.more()), (4, 1));
    let words = stayed_words(&stage, 4);
    assert_eq!(words, four.words(&stage));
    let mut at = 0;
    for p in &four.first {
        let found = words[at..]
            .find(&*p.to_string_lossy())
            .unwrap_or_else(|| panic!("{} named in order: {words}", p.display()));
        at += found;
    }
    assert!(!words.contains("never read"), "{words}");

    let lone = dir.join("lone");
    std::fs::create_dir(&lone).unwrap();
    let one = named(&lone, 1);
    assert_eq!((one.more(), one.first), (0, vec![lone.clone()]));
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

/// AN ENTRY DENYING ITS OWN READING AND REMOVAL GOES: a file whose ACL denies `read` and
/// `delete`, a folder denying `list` and `delete`, and a link denying `delete` are cleared by
/// name in their folder (no open of the entry, which they refuse, and nothing through the link)
/// and the tree goes whole; the link's target keeps its own ACL. NEGATIVE CONTROL: each refuses
/// its removal before the walk.
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
            let cleared = aterm_uds::spawnfd::command("/bin/chmod")
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

/// AN ENTRY IS CHANGED BY ITS NAME ONLY IN A FOLDER NO ONE ELSE CAN RENAME IN: a folder at 0o777
/// (any user may rename in it) holding a folder at 0o000 is made its owner's alone (0o700, its
/// ACL cleared) before the folder inside is given its owner's bits back by its name (the
/// remover's seam reads the mode right then), and the stage goes whole. NEGATIVE CONTROL: the
/// folder is at 0o777 before the walk.
#[cfg(unix)]
#[test]
fn an_entry_is_changed_by_its_name_only_in_a_folder_no_one_else_can_rename_in() {
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
    assert_eq!(
        *modes.borrow(),
        [0o700],
        "changed by its name in an open folder"
    );
    assert!(left.whole() && !stage.exists(), "{left:?}");
}

/// AN ENTRY WHOSE ATTRIBUTES CANNOT BE READ IS LEFT AS IT IS: a file and a folder (holding a
/// file) whose own ACLs deny the reading of their attributes cannot be judged (whose they are,
/// on which volume, under how many names), so each stays, counted once and named, keeping its
/// ACL (it still refuses that read), and the entry beside them goes. NEGATIVE CONTROL: before
/// the walk, each refuses a read of its attributes by path.
#[cfg(target_os = "macos")]
#[test]
fn an_entry_whose_attributes_cannot_be_read_is_left_as_it_is() {
    const DENY_READATTR: &str = "group:everyone deny readattr";
    /// Clears the two ACLs when dropped, before the scratch goes.
    struct Clear(Vec<PathBuf>);
    impl Drop for Clear {
        fn drop(&mut self) {
            for at in &self.0 {
                let _ = aterm_uds::spawnfd::command("/bin/chmod")
                    .arg("-N")
                    .arg(at)
                    .status();
            }
        }
    }
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

    let left = tree(&stage);
    assert_eq!(left.stayed, 2, "{left:?}");
    assert!(!stage.join("beside").exists());
    for at in &unread {
        assert!(
            std::fs::symlink_metadata(at).is_err(),
            "{} lost its ACL",
            at.display()
        );
    }
    assert_eq!(stayed(&stage, 10), unread);
}

/// The soft descriptor limit [`a_walk_holds_one_descriptor_a_folder`]'s stand-in runs under.
#[cfg(unix)]
const FD_LIMIT: &str = "ATPKG_REMOVE_FD_LIMIT";

/// A WALK HOLDS ONE DESCRIPTOR A FOLDER: in a process whose descriptors are limited to a few
/// more than the folders a walk holds open, a chain twice that deep goes whole (a walk holding
/// two a folder runs out). The test binary runs itself as the stand-in under the limit.
/// NEGATIVE CONTROL: the stand-in's limit is in force (opening that many more descriptors than
/// it allows is refused there).
#[cfg(unix)]
#[test]
fn a_walk_holds_one_descriptor_a_folder() {
    let limit = sys::DEPTH + 24;
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
    let dir = crate::testdir::dir("build-cache-remove-fds");
    chain(&dir.join("top"), 2 * sys::DEPTH + 3);
    let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            "build_cache::remove::tests::a_walk_holds_one_descriptor_a_folder",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(FD_LIMIT, &*dir)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("the stand-in");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!dir.join("top").exists());
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
        (vec![PathBuf::from("theirs")], 1)
    );
    assert_eq!(super::named(&stage, 0).first, Vec::<PathBuf>::new());
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
