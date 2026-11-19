// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::io;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::elevated::as_elevated;
use super::super::{
    ANOTHER_LINK, ELEVATED, ELEVATED_PARENT, JudgedDir, OPEN_MODE, OTHERS_WRITE, PrivateDir,
    PrivateFileError, PrivateLock, check_owned_dir, check_private_dir, create_private_dir,
    ensure_private_dir, judge_private_dir, new_file_descriptor, open_private, open_private_rw,
    read_owned, read_owned_into, read_private, read_private_into, remove_private,
    set_dir_mode_nofollow, write_private, write_private_flushed, write_private_new,
};
use super::DIR_OTHERS_WRITE;
use crate::acl::{Descriptor, NotOwnerOnly, Parts, Sid};

fn me() -> Sid {
    Sid::token_user().expect("this token's user")
}

/// A scratch folder under the temporary folder, and a private folder made in it.
pub(in crate::private_file) fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
    assert!(
        !crate::elevation::is_elevated(),
        "these tests take this process as unelevated wherever they do not say otherwise"
    );
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-private-win-")
        .tempdir()
        .expect("scratch folder");
    let dir = root.path().join("state");
    create_private_dir(&dir).expect("a private folder");
    (root, dir)
}

/// The owner and access list of `path`, as descriptor text, read by its name.
fn sddl_of(path: &Path) -> String {
    Descriptor::of_path(path, Parts::OWNER | Parts::DACL)
        .and_then(|descriptor| descriptor.to_sddl(Parts::OWNER | Parts::DACL))
        .expect("the descriptor reads")
}

/// The access list's flags (`P`, `AI`) and its entries, from descriptor text.
fn dacl_parts(sddl: &str) -> (String, Vec<String>) {
    let dacl = &sddl[sddl.find("D:").expect("a DACL") + 2..];
    let open = dacl.find('(').unwrap_or(dacl.len());
    let entries = dacl[open..]
        .split_inclusive(')')
        .map(str::to_owned)
        .collect();
    (dacl[..open].to_owned(), entries)
}

/// Assert `path` is owned by the user under a protected list of exactly the user and SYSTEM,
/// with `flags` on each entry (`""` for a file, `"OICI"` for a folder).
pub(in crate::private_file) fn assert_owner_only(path: &Path, flags: &str) {
    let me = me();
    let sddl = sddl_of(path);
    assert!(
        sddl.starts_with(&format!("O:{me}D:")),
        "{}: owned by the user: {sddl}",
        path.display()
    );
    let (list, entries) = dacl_parts(&sddl);
    assert!(list.contains('P'), "{}: protected: {sddl}", path.display());
    assert_eq!(
        entries,
        vec![
            format!("(A;{flags};FA;;;{me})"),
            format!("(A;{flags};FA;;;SY)")
        ],
        "{}: exactly the user and SYSTEM: {sddl}",
        path.display()
    );
}

/// `path` opened to rewrite its descriptor.
pub(in crate::private_file) fn open_to_rewrite(path: &Path) -> std::fs::File {
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(
            crate::acl::FILE_FLAG_OPEN_REPARSE_POINT | crate::acl::FILE_FLAG_BACKUP_SEMANTICS,
        )
        .open(path)
        .expect("open to rewrite its access list")
}

/// Make the junction `link` to the folder `target` (`mklink /J`, which needs no privilege).
pub(in crate::private_file) fn junction(link: &Path, target: &Path) {
    let status = crate::spawnfd::command("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null())
        .status()
        .expect("run mklink");
    assert!(status.success(), "mklink /J {link:?} {target:?}");
}

fn not_private(result: Result<Vec<u8>, PrivateFileError>) -> &'static str {
    match result {
        Err(PrivateFileError::NotPrivate(why)) => why,
        other => panic!("not refused as not private: {other:?}"),
    }
}

/// THE LAW: every file this module makes is the token user's under a PROTECTED access list of
/// exactly the user and SYSTEM, full access, nothing passed on (`O:<user>D:P(A;;FA;;;<user>)
/// (A;;FA;;;SY)`): one `write_private` wrote, one `write_private_flushed` and
/// `write_private_new` wrote, one `open_private_rw` and a lock made; and the private folder is
/// the same list passed on (`OICI`). Each reads back through the rule.
#[test]
fn a_file_write_private_made_has_exactly_the_user_and_system_under_a_protected_list() {
    let (_root, dir) = scratch();
    assert_owner_only(&dir, "OICI");

    write_private(&dir, "key", b"secret").expect("write");
    assert_owner_only(&dir.join("key"), "");
    assert_eq!(read_private(&dir, "key", 64).unwrap(), b"secret");

    write_private(&dir, "key", b"again").expect("replace");
    assert_owner_only(&dir.join("key"), "");
    assert_eq!(read_private(&dir, "key", 64).unwrap(), b"again");

    write_private_flushed(&dir, "flushed", b"f", aterm_durable::Flush::Durable).expect("flushed");
    assert_owner_only(&dir.join("flushed"), "");

    assert!(write_private_new(&dir, "new", b"n", |_| Ok(())).expect("new"));
    assert!(
        !write_private_new(&dir, "new", b"m", |_| Ok(())).expect("not over one there"),
        "a name that exists is left"
    );
    assert_owner_only(&dir.join("new"), "");
    assert_eq!(read_private(&dir, "new", 8).unwrap(), b"n");

    drop(open_private_rw(&dir, "rw").expect("made read-write"));
    assert_owner_only(&dir.join("rw"), "");

    drop(PrivateLock::take(&dir, ".lock", Duration::ZERO, "held").expect("lock"));
    assert_owner_only(&dir.join(".lock"), "");

    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|name| !name.contains(".tmp.")),
        "no temporary is left: {names:?}"
    );
}

/// NEGATIVE CONTROL: an allow entry for Everyone added to a file the rule made, or to its
/// folder, is refused; the folder's is narrowed back by `ensure_private_dir`, the file's never.
#[test]
fn an_added_everyone_entry_is_refused() {
    let (_root, dir) = scratch();
    write_private(&dir, "key", b"k").unwrap();
    let me = me();
    let open =
        Descriptor::from_sddl(&format!("D:P(A;;FA;;;{me})(A;;FA;;;SY)(A;;FR;;;WD)")).unwrap();
    open.apply(
        open_to_rewrite(&dir.join("key")),
        Parts::DACL | Parts::PROTECTED_DACL,
    )
    .unwrap();
    assert!(
        sddl_of(&dir.join("key")).contains(";;;WD)"),
        "the control took"
    );
    assert_eq!(
        not_private(read_private(&dir, "key", 8)),
        NotOwnerOnly::Grants(Sid::parse("WD").unwrap()).why()
    );

    write_private(&dir, "other", b"o").unwrap();
    let open_dir = Descriptor::from_sddl(&format!(
        "D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICI;FR;;;WD)"
    ))
    .unwrap();
    open_dir
        .apply(open_to_rewrite(&dir), Parts::DACL | Parts::PROTECTED_DACL)
        .unwrap();
    let refused = check_private_dir(&dir).expect_err("a folder Everyone may read");
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    assert!(matches!(
        read_private(&dir, "other", 8),
        Err(PrivateFileError::NotPrivate(_))
    ));
    assert_eq!(
        judge_private_dir(&dir).unwrap(),
        PrivateDir::Open(OPEN_MODE)
    );
    assert!(JudgedDir::private(&dir).is_err());
    check_owned_dir(&dir).expect("still the user's own folder");

    assert_eq!(
        ensure_private_dir(&dir).unwrap(),
        Some(OPEN_MODE),
        "narrowed"
    );
    assert_owner_only(&dir, "OICI");
    assert_eq!(read_private(&dir, "other", 8).unwrap(), b"o");
}

/// NEGATIVE CONTROL: an inherited entry is refused, even the user's own under a protected list;
/// and so is a list that takes entries from its folder.
#[test]
fn an_inherited_entry_and_an_unprotected_list_are_refused() {
    let (_root, dir) = scratch();
    let me = me();

    write_private(&dir, "inherited", b"i").unwrap();
    let inherited = Descriptor::from_sddl(&format!("D:P(A;ID;FA;;;{me})(A;ID;FA;;;SY)")).unwrap();
    // The kernel's own write, which keeps an entry's flags as given (the access-control
    // library's would drop an inherited entry it was handed).
    const DACL: u32 = 0x0000_0004;
    const PROTECTED_DACL: u32 = 0x8000_0000;
    let file = open_to_rewrite(&dir.join("inherited"));
    // SAFETY: `file` is a live handle with WRITE_DAC and `inherited` a live descriptor.
    let set = unsafe {
        crate::win::ffi::SetKernelObjectSecurity(
            file.as_raw_handle() as crate::win::ffi::RawHandle,
            DACL | PROTECTED_DACL,
            inherited.as_ptr(),
        )
    };
    assert_ne!(set, 0, "{}", io::Error::last_os_error());
    drop(file);
    let sddl = sddl_of(&dir.join("inherited"));
    let (list, entries) = dacl_parts(&sddl);
    assert!(
        list.contains('P') && entries.iter().all(|entry| entry.contains(";ID;")),
        "the control took: {sddl}"
    );
    assert_eq!(
        not_private(read_private(&dir, "inherited", 8)),
        NotOwnerOnly::Inherited.why()
    );

    write_private(&dir, "unprotected", b"u").unwrap();
    let own = Descriptor::from_sddl(&format!("D:(A;;FA;;;{me})(A;;FA;;;SY)")).unwrap();
    const UNPROTECTED_DACL: u32 = 0x2000_0000;
    let file = open_to_rewrite(&dir.join("unprotected"));
    // SAFETY: as above.
    let set = unsafe {
        crate::win::ffi::SetKernelObjectSecurity(
            file.as_raw_handle() as crate::win::ffi::RawHandle,
            DACL | UNPROTECTED_DACL,
            own.as_ptr(),
        )
    };
    assert_ne!(set, 0, "{}", io::Error::last_os_error());
    drop(file);
    let sddl = sddl_of(&dir.join("unprotected"));
    assert!(
        !dacl_parts(&sddl).0.contains('P'),
        "the control took: {sddl}"
    );
    assert_eq!(
        not_private(read_private(&dir, "unprotected", 8)),
        NotOwnerOnly::Unprotected.why()
    );
}

/// NEGATIVE CONTROL, ANOTHER OWNER: this unelevated user can give nothing another owner (only
/// an account in its token marked as an owner, which is itself alone, or a holder of the restore
/// privilege may: `ERROR_INVALID_OWNER`), so the control is Windows' own folder and a program
/// in it, which `TrustedInstaller` owns, judged read-only: the folder is another user's to every
/// call here, and the file is refused by the owner clause. The descriptor-level controls are in
/// `acl::tests`.
#[test]
fn another_owners_folder_and_file_are_refused() {
    let (_root, dir) = scratch();
    write_private(&dir, "key", b"k").unwrap();
    let ba = Descriptor::from_sddl("O:BA").unwrap();
    let refused = ba
        .apply(open_owner_writer(&dir.join("key")), Parts::OWNER)
        .expect_err("an unelevated user gives a file no other owner");
    assert_eq!(
        refused.raw_os_error(),
        Some(1307),
        "ERROR_INVALID_OWNER: {refused}"
    );

    let windows = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    let me = me();
    let owner = Descriptor::of_path(&windows, Parts::OWNER)
        .unwrap()
        .owner()
        .unwrap()
        .expect("an owner");
    assert_ne!(owner, me, "the control: {owner} owns {}", windows.display());
    let refused = check_private_dir(&windows).expect_err("Windows' folder");
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    assert!(refused.to_string().contains("another user's"), "{refused}");
    assert!(judge_private_dir(&windows).is_err());
    assert!(check_owned_dir(&windows).is_err());
    assert!(matches!(
        open_private(&windows, "win.ini"),
        Err(
            PrivateFileError::NotPrivate("the directory is another user's")
                | PrivateFileError::Missing
        )
    ));
    let program = std::fs::File::open(windows.join("System32").join("cmd.exe")).unwrap();
    assert!(matches!(
        crate::acl::owner_only(&program, &me).unwrap(),
        Err(NotOwnerOnly::Owner(Some(_)))
    ));
}

/// `path` opened to rewrite its owner.
fn open_owner_writer(path: &Path) -> std::fs::File {
    const WRITE_OWNER: u32 = 0x0008_0000;
    std::fs::OpenOptions::new()
        .access_mode(WRITE_OWNER)
        .custom_flags(crate::acl::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .expect("open to rewrite its owner")
}

/// NEGATIVE CONTROL: a junction is never followed: a private folder that is one, and a name in
/// a private folder that is one, are each refused, though what they lead to is private.
#[test]
fn a_junction_is_refused_never_followed() {
    let (root, dir) = scratch();
    write_private(&dir, "key", b"k").unwrap();
    let link = root.path().join("link");
    junction(&link, &dir);
    assert!(
        std::fs::metadata(link.join("key")).is_ok(),
        "the junction leads there"
    );

    let refused = check_private_dir(&link).expect_err("a junction for a folder");
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    assert!(matches!(
        read_private(&link, "key", 8),
        Err(PrivateFileError::Symlink)
    ));
    assert!(judge_private_dir(&link).is_err());
    assert!(write_private(&link, "key", b"x").is_err());
    assert_eq!(
        read_private(&dir, "key", 8).unwrap(),
        b"k",
        "nothing written through it"
    );

    let inner = dir.join("inner");
    junction(&inner, &dir);
    assert!(matches!(
        open_private(&dir, "inner"),
        Err(PrivateFileError::Symlink)
    ));
    assert!(matches!(
        open_private_rw(&dir, "inner"),
        Err(PrivateFileError::Symlink)
    ));
    // The open's own refusal (a write open of a folder link), or, had it opened, no plain file.
    let refused = PrivateLock::take(&dir, "inner", Duration::ZERO, "held")
        .err()
        .map(|e| e.kind());
    assert!(
        matches!(
            refused,
            Some(io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidData)
        ),
        "a lock is never a junction: {refused:?}"
    );
}

/// NEGATIVE CONTROL: a second hard link to a private file is another copy of it: refused until
/// it goes.
#[test]
fn a_second_hard_link_is_refused() {
    let (_root, dir) = scratch();
    write_private(&dir, "key", b"k").unwrap();
    std::fs::hard_link(dir.join("key"), dir.join("copy")).unwrap();
    assert_eq!(not_private(read_private(&dir, "key", 8)), ANOTHER_LINK);
    assert!(matches!(
        open_private_rw(&dir, "key"),
        Err(PrivateFileError::NotPrivate(ANOTHER_LINK))
    ));
    std::fs::remove_file(dir.join("copy")).unwrap();
    assert_eq!(read_private(&dir, "key", 8).unwrap(), b"k");
}

/// A folder made as any other (inheriting its parent's list) is no private folder until
/// `ensure_private_dir` replaces its list; one that is missing is made private at once.
#[test]
fn an_ordinary_folder_is_refused_until_ensured_and_a_missing_one_is_made_private() {
    let (root, _dir) = scratch();
    let plain = root.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert!(check_private_dir(&plain).is_err());
    assert!(write_private(&plain, "key", b"k").is_err());
    assert_eq!(
        judge_private_dir(&plain).unwrap(),
        PrivateDir::Open(OPEN_MODE)
    );
    assert_eq!(ensure_private_dir(&plain).unwrap(), Some(OPEN_MODE));
    assert_owner_only(&plain, "OICI");
    assert_eq!(ensure_private_dir(&plain).unwrap(), None, "already private");

    let deep = root.path().join("a").join("b").join("c");
    assert_eq!(judge_private_dir(&deep).unwrap(), PrivateDir::Missing);
    assert_eq!(ensure_private_dir(&deep).unwrap(), None, "made private");
    assert_owner_only(&deep, "OICI");
    write_private(&deep, "key", b"d").unwrap();
    assert_eq!(read_private(&deep, "key", 8).unwrap(), b"d");
}

/// AN ELEVATED PROCESS KEEPS PRIVATE FILES THROUGH A HELD FOLDER OF THE USER'S: run as elevated,
/// each maker writes, replaces, links, opens to write, locks and removes in a private folder,
/// and what it makes is the user's under the private list (never Administrators', a full
/// token's default owner); a missing private folder is made in a folder of the user's that is
/// there. It makes no missing folder above one, nor one in another account's folder (Windows'
/// own, `TrustedInstaller`'s): `ELEVATED_PARENT`, naming the folder, and nothing is made. And
/// `new_file_descriptor`, whose callers make their files by path, still refuses it (`ELEVATED`).
/// NEGATIVE CONTROLS (2026-11-18): `ensure_private_dir` making the parents elevated too (its
/// `!elevated()` guard dropped): the folders above are made and the refusal does not come
/// (`refused elevated: ()`); the parent's owner unread (`make_private_dir`'s `is_users` clause
/// dropped): the make in Windows' folder is the system's own refusal (`Access is denied`), not
/// `ELEVATED_PARENT`.
#[test]
fn an_elevated_process_keeps_private_files_through_a_held_folder_of_the_users() {
    let (root, dir) = scratch();
    write_private(&dir, "gone", b"g").unwrap();
    let made = root.path().join("made");
    let deep = root.path().join("missing").join("inner");
    let windows = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    let theirs = windows.join("aterm-private-win-elevated");
    assert!(!theirs.exists(), "the control's name is free");
    let parent_refused = |result: io::Result<()>, at: &Path| {
        let e = result.expect_err("refused elevated");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        let text = e.to_string();
        assert!(text.contains(ELEVATED_PARENT), "{text}");
        assert!(text.contains(&at.display().to_string()), "{text}");
    };
    as_elevated(|| {
        write_private(&dir, "key", b"k").expect("written elevated");
        write_private(&dir, "key", b"again").expect("replaced elevated");
        write_private_flushed(&dir, "flushed", b"f", aterm_durable::Flush::Durable)
            .expect("flushed elevated");
        assert!(write_private_new(&dir, "new", b"n", |_| Ok(())).expect("linked elevated"));
        drop(open_private_rw(&dir, "rw").expect("opened to write elevated"));
        drop(PrivateLock::take(&dir, ".lock", Duration::ZERO, "held").expect("locked elevated"));
        assert!(remove_private(&dir, "gone").expect("removed elevated"));
        assert_eq!(
            ensure_private_dir(&made).expect("made in the user's folder"),
            None
        );
        write_private(&made, "key", b"m").expect("written in it");

        parent_refused(ensure_private_dir(&deep).map(drop), &deep);
        parent_refused(create_private_dir(&deep), &deep);
        parent_refused(ensure_private_dir(&theirs).map(drop), &theirs);
        parent_refused(create_private_dir(&theirs), &theirs);
        let refused = new_file_descriptor().expect_err("a maker by path");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(refused.to_string(), ELEVATED);
        assert_eq!(read_private(&dir, "key", 8).unwrap(), b"again", "it reads");
    });
    for path in [
        dir.join("key"),
        dir.join("flushed"),
        dir.join("new"),
        dir.join("rw"),
        dir.join(".lock"),
        made.join("key"),
    ] {
        assert_owner_only(&path, "");
    }
    assert_owner_only(&made, "OICI");
    assert!(!dir.join("gone").exists(), "removed");
    assert!(
        !root.path().join("missing").exists() && !theirs.exists(),
        "nothing made above or elsewhere"
    );
    new_file_descriptor().expect("unelevated, a maker by path has its descriptor");
}

/// THE LAW, A HELD FOLDER: a folder renamed after its judgement, with a junction to another
/// private folder put at its name, sends nothing there: a judged folder's reads, and a write's
/// temporary, rename and removal, all stay on the folder the judgement held.
/// NEGATIVE CONTROL (2026-11-18): `open_in` judging the folder again by its path (as the arm
/// did before its folders were held): the judged folder's read fails `Symlink`.
#[test]
fn a_folder_renamed_after_its_judgement_sends_nothing_elsewhere() {
    let (root, dir) = scratch();
    let other = root.path().join("other");
    let retired = root.path().join("retired");
    create_private_dir(&other).unwrap();
    write_private(&dir, "row", b"before").unwrap();
    write_private(&other, "row", b"unrelated").unwrap();

    let judged = JudgedDir::judge(&dir).unwrap();
    let directory = super::super::PrivateDirectory::open(&dir).unwrap();
    std::fs::rename(&dir, &retired).expect("a held folder renames");
    junction(&dir, &other);
    assert_eq!(
        read_private(&dir, "row", 16).ok(),
        None,
        "the name is a link"
    );
    assert_eq!(
        judged.read("row", 16).unwrap(),
        b"before",
        "read from the folder judged"
    );
    let view = directory.view().unwrap();
    view.write_entry_landed("row", b"after", || Ok(()))
        .expect("published in the held folder")
        .expect("noted");
    view.write("second", b"2").unwrap();
    view.remove("second").unwrap();
    assert_eq!(view.read("row", 16).unwrap(), b"after");
    assert_eq!(read_private(&retired, "row", 16).unwrap(), b"after");
    assert!(!retired.join("second").exists());
    assert_eq!(read_private(&other, "row", 16).unwrap(), b"unrelated");
    let names: Vec<String> = std::fs::read_dir(&other)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["row"], "nothing reached the other folder");
    assert!(directory.is_at(&retired).unwrap());
    assert!(
        directory.is_at(&dir).is_err(),
        "a junction is no folder of its own"
    );
    std::fs::remove_dir(&dir).expect("the junction goes");
}

/// THE LAW, A FOLDER SWAPPED IN THE WRITE: the writer `write_private` and its kin share
/// (`write_in`) runs its `check` after its temporary is written and before its rename, and a
/// folder renamed then and replaced by a junction to another private folder still takes the
/// rename: the bytes land in the folder judged, never through the junction, and no temporary is
/// left in either.
/// NEGATIVE CONTROL (2026-11-18): `write_in` renaming its temporary by the file's path
/// (`std::fs::rename`, as the arm did before its folders were held): `published in the held
/// folder` fails, the temporary not found behind the junction.
#[test]
fn a_folder_swapped_during_a_write_takes_the_write_in_the_folder_judged() {
    let (root, dir) = scratch();
    let other = root.path().join("other");
    let retired = root.path().join("retired");
    create_private_dir(&other).unwrap();
    write_private(&other, "row", b"unrelated").unwrap();
    write_private(&dir, "row", b"before").unwrap();
    let folder = super::private_folder(&dir).unwrap();
    let flushing = super::Flushing {
        flush: aterm_durable::Flush::Durable,
        diagnose: true,
    };
    super::write_in(&folder, &dir.join("row"), "row", b"after", flushing, || {
        std::fs::rename(&dir, &retired)?;
        junction(&dir, &other);
        Ok(())
    })
    .expect("published in the held folder")
    .expect("noted");
    drop(folder);
    assert_eq!(read_private(&retired, "row", 16).unwrap(), b"after");
    assert_eq!(read_private(&other, "row", 16).unwrap(), b"unrelated");
    for folder in [&retired, &other] {
        let names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["row"], "no temporary left in {}", folder.display());
    }
    std::fs::remove_dir(&dir).expect("the junction goes");
}

/// A lock file is judged private on its handle, as a private file is: one another account may
/// read, or that has a second link, takes no lock (a mandatory lock on another's file is a way
/// to stop it being written).
/// NEGATIVE CONTROL (2026-11-18): a read-write opening judged as a plain file only, as the lock's
/// was before (`judge_opened` skipping the owner-only and link clauses for it): the lock another
/// account may read is taken (`another account may read it` fails).
#[test]
fn a_lock_file_that_is_not_private_takes_no_lock() {
    let (_root, dir) = scratch();
    drop(PrivateLock::take(&dir, "open.lock", Duration::ZERO, "held").unwrap());
    let me = me();
    Descriptor::from_sddl(&format!("D:P(A;;FA;;;{me})(A;;FA;;;SY)(A;;FR;;;WD)"))
        .unwrap()
        .apply(
            open_to_rewrite(&dir.join("open.lock")),
            Parts::DACL | Parts::PROTECTED_DACL,
        )
        .unwrap();
    let refused = PrivateLock::take(&dir, "open.lock", Duration::ZERO, "held")
        .err()
        .expect("another account may read it");
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");

    drop(PrivateLock::take(&dir, "linked.lock", Duration::ZERO, "held").unwrap());
    std::fs::hard_link(dir.join("linked.lock"), dir.join("copy")).unwrap();
    let refused = PrivateLock::take(&dir, "linked.lock", Duration::ZERO, "held")
        .err()
        .expect("a second link");
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    std::fs::remove_file(dir.join("copy")).unwrap();
    drop(PrivateLock::take(&dir, "linked.lock", Duration::ZERO, "held").expect("one link again"));
}

/// `file`'s access list replaced with `sddl`'s (the kernel's own write, which keeps each entry's
/// flags as given) and `protected` or not.
fn set_list(path: &Path, sddl: &str, protected: bool) {
    const DACL: u32 = 0x0000_0004;
    const PROTECTED_DACL: u32 = 0x8000_0000;
    const UNPROTECTED_DACL: u32 = 0x2000_0000;
    let list = Descriptor::from_sddl(sddl).unwrap();
    let file = open_to_rewrite(path);
    let how = DACL
        | if protected {
            PROTECTED_DACL
        } else {
            UNPROTECTED_DACL
        };
    // SAFETY: `file` is a live handle with WRITE_DAC and `list` a live descriptor.
    let set = unsafe {
        crate::win::ffi::SetKernelObjectSecurity(
            file.as_raw_handle() as crate::win::ffi::RawHandle,
            how,
            list.as_ptr(),
        )
    };
    assert_ne!(set, 0, "{}", io::Error::last_os_error());
}

/// READ_OWNED'S RULE: a file and folder the user owns may hold inherited entries, take entries
/// from their folder, and let other accounts read (`loose`), but no entry may let an account
/// other than the user, SYSTEM and Administrators write: `OTHERS_WRITE`, the folder's
/// `DIR_OTHERS_WRITE`. A strict private file reads as not loose; a file made as any other in the
/// user's ordinary folder reads; another account's file and folder, a junction and a second
/// link are refused as by the strict rule. The identity is the handle's (`file_id`).
/// NEGATIVE CONTROL (2026-11-18): `owned_rule` taking any right of another account as a read
/// (its `READ_RIGHTS` clause dropped): the file Everyone may write reads.
#[test]
fn read_owned_lets_others_read_but_never_write() {
    let (root, dir) = scratch();
    let me = me();
    let mut buf = [0u8; 64];

    write_private(&dir, "strict", b"s").unwrap();
    let read = read_owned_into(&dir, "strict", 64, &mut buf).unwrap();
    assert_eq!((&buf[..read.len], read.loose), (&b"s"[..], false));
    let id = aterm_dirfd::file_id(std::fs::File::open(dir.join("strict")).unwrap()).unwrap();
    assert_eq!(
        (read.dev, read.ino),
        (id.dev, id.ino),
        "the handle's identity"
    );

    write_private(&dir, "readable", b"r").unwrap();
    set_list(
        &dir.join("readable"),
        &format!("D:(A;;FA;;;{me})(A;ID;FA;;;SY)(A;;FR;;;WD)(A;;FA;;;BA)"),
        false,
    );
    let read = read_owned_into(&dir, "readable", 64, &mut buf).unwrap();
    assert_eq!((&buf[..read.len], read.loose), (&b"r"[..], true));
    assert_eq!(read_owned(&dir, "readable", 64).unwrap(), b"r");
    assert!(
        matches!(
            read_private(&dir, "readable", 64),
            Err(PrivateFileError::NotPrivate(_))
        ),
        "the strict rule refuses it"
    );

    write_private(&dir, "writable", b"w").unwrap();
    set_list(
        &dir.join("writable"),
        &format!("D:P(A;;FA;;;{me})(A;;FA;;;SY)(A;;0x00000002;;;WD)"),
        true,
    );
    assert!(matches!(
        read_owned(&dir, "writable", 64),
        Err(PrivateFileError::NotPrivate(OTHERS_WRITE))
    ));

    // A file made as any other in an ordinary folder of the user's (the profile's inherited list).
    let plain = root.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    std::fs::write(plain.join("legacy"), b"l").unwrap();
    let read = read_owned_into(&plain, "legacy", 64, &mut buf).unwrap();
    assert_eq!(&buf[..read.len], b"l");
    assert!(read_private(&plain, "legacy", 64).is_err());

    // The folder: another account may read it, then write in it.
    let shared = root.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::write(shared.join("row"), b"x").unwrap();
    set_list(
        &shared,
        &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;;FR;;;WD)"),
        true,
    );
    assert!(read_owned_into(&shared, "row", 64, &mut buf).unwrap().loose);
    set_list(
        &shared,
        &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;;0x00000006;;;WD)"),
        true,
    );
    assert!(matches!(
        read_owned(&shared, "row", 64),
        Err(PrivateFileError::NotPrivate(DIR_OTHERS_WRITE))
    ));
    // An entry passed on only to what the folder holds grants the folder nothing.
    set_list(
        &shared,
        &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICIIO;FA;;;WD)"),
        true,
    );
    assert!(!read_owned_into(&shared, "row", 64, &mut buf).unwrap().loose);

    std::fs::hard_link(dir.join("strict"), dir.join("copy")).unwrap();
    assert!(matches!(
        read_owned(&dir, "strict", 64),
        Err(PrivateFileError::NotPrivate(ANOTHER_LINK))
    ));
    let windows = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    assert!(matches!(
        read_owned(&windows, "win.ini", 64),
        Err(PrivateFileError::NotPrivate(_) | PrivateFileError::Missing)
    ));
    let link = root.path().join("link");
    junction(&link, &dir);
    assert!(matches!(
        read_owned(&link, "readable", 64),
        Err(PrivateFileError::Symlink)
    ));
    std::fs::remove_dir(&link).unwrap();
}

/// READ_PRIVATE_INTO reads into the caller's buffer and answers the file's identity off its
/// handle: the same file's, and a replacement's another.
/// NEGATIVE CONTROL (2026-11-18): the identity's file id answered as 0: the first comparison
/// with `file_id` fails (and `read_owned_into`'s in the test of its rule).
#[test]
fn read_private_into_answers_the_files_identity() {
    let (_root, dir) = scratch();
    write_private(&dir, "key", b"secret").unwrap();
    let mut buf = [0u8; 16];
    let first = read_private_into(&dir, "key", 16, &mut buf).unwrap();
    assert_eq!(&buf[..first.len], b"secret");
    let id = aterm_dirfd::file_id(std::fs::File::open(dir.join("key")).unwrap()).unwrap();
    assert_eq!((first.dev, first.ino), (id.dev, id.ino));
    assert_eq!(read_private_into(&dir, "key", 16, &mut buf).unwrap(), first);
    write_private(&dir, "key", b"other").unwrap();
    let second = read_private_into(&dir, "key", 16, &mut buf).unwrap();
    assert_eq!(&buf[..second.len], b"other");
    assert_eq!(second.dev, first.dev);
    assert_ne!(second.ino, first.ino, "a replacement is another file");
    assert!(matches!(
        read_private_into(&dir, "key", 3, &mut buf),
        Err(PrivateFileError::Malformed(_))
    ));
}

/// SET_DIR_MODE_NOFOLLOW narrows an ordinary folder of the user's to the private list on a
/// handle of the folder itself, leaves a private one as it is, never widens one, and never
/// follows a junction: one is refused and its target left as it was.
/// NEGATIVE CONTROL (2026-11-18): the folder opened by a path that follows links
/// (`open_parent`'s open): the junction's target is narrowed and the refusal does not come
/// (`a junction: ()`).
#[test]
fn set_dir_mode_nofollow_narrows_a_folder_as_itself() {
    let (root, dir) = scratch();
    let plain = root.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let inherited = sddl_of(&plain);
    assert!(!dacl_parts(&inherited).0.contains('P'), "{inherited}");

    let link = root.path().join("link");
    junction(&link, &plain);
    let refused = set_dir_mode_nofollow(&link, 0o700).expect_err("a junction");
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied, "{refused}");
    assert_eq!(sddl_of(&plain), inherited, "its target is left as it was");
    std::fs::remove_dir(&link).unwrap();

    let widened = set_dir_mode_nofollow(&plain, 0o755).expect_err("a mode for others");
    assert_eq!(widened.kind(), io::ErrorKind::Unsupported, "{widened}");
    assert_eq!(sddl_of(&plain), inherited);

    set_dir_mode_nofollow(&plain, 0o700).unwrap();
    assert_owner_only(&plain, "OICI");
    let private = sddl_of(&dir);
    set_dir_mode_nofollow(&dir, 0o700).unwrap();
    assert_eq!(sddl_of(&dir), private, "a private folder is left as it is");

    write_private(&dir, "file", b"f").unwrap();
    let refused = set_dir_mode_nofollow(&dir.join("file"), 0o700).expect_err("a file");
    assert_eq!(refused.kind(), io::ErrorKind::NotADirectory, "{refused}");
    let missing = set_dir_mode_nofollow(&root.path().join("missing"), 0o700).unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound, "{missing}");
}

/// A name a Windows path would read as more or other than one file in its folder is refused
/// before anything is opened.
#[test]
fn a_name_windows_reads_as_another_file_is_refused() {
    let (_root, dir) = scratch();
    for name in [
        "a\\b",
        "a/b",
        "a:b",
        "C:x",
        "NUL",
        "nul.txt",
        "Con",
        "conin$",
        "CONOUT$.x",
        "COM1",
        "lpt9.log",
        "COM¹",
        "key.",
        "key ",
        "a*b",
        "a?b",
        "a|b",
        "a\"b",
        "a<b",
        "a>b",
        "a\tb",
        "",
        ".",
        "..",
    ] {
        let refused = write_private(&dir, name, b"x").expect_err(name);
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "{name:?}");
        assert!(
            matches!(
                read_private(&dir, name, 8),
                Err(PrivateFileError::Malformed(_))
            ),
            "{name:?}"
        );
    }
    for name in ["key", ".lock", "COM10", "console", "nul2", "a.b.c", "état"] {
        write_private(&dir, name, b"x").unwrap_or_else(|e| panic!("{name:?}: {e}"));
    }
}

/// A held lock is held against a second taker, and a waiter takes it once it is let go.
#[test]
fn a_private_lock_excludes_and_a_waiter_takes_it_once_let_go() {
    let (root, dir) = scratch();
    let first = PrivateLock::take(&dir, ".lock", Duration::ZERO, "busy").unwrap();
    assert_eq!(
        PrivateLock::take(&dir, ".lock", Duration::ZERO, "busy")
            .err()
            .map(|e| e.kind()),
        Some(io::ErrorKind::WouldBlock)
    );
    let (told, heard) = std::sync::mpsc::channel();
    let waiter = {
        let dir = dir.clone();
        std::thread::spawn(move || {
            PrivateLock::wait_for(&dir, ".lock", move || told.send(()).unwrap())
        })
    };
    if let Err(error) = heard.recv_timeout(Duration::from_secs(60)) {
        let kept = root.keep();
        panic!(
            "the waiter did not find the lock held: {error}; retained {}",
            kept.display()
        );
    }
    drop(first);
    if aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(10),
        || waiter.is_finished().then_some(()),
    )
    .is_none()
    {
        let kept = root.keep();
        panic!(
            "the released lock waiter did not finish within the hang detector; retained {}",
            kept.display()
        );
    }
    let held = waiter.join().unwrap().expect("taken once let go");
    assert!(PrivateLock::take(&dir, ".lock", Duration::ZERO, "busy").is_err());
    drop(held);
}

/// The reads and removal round-trip on a private folder; a folder judged once serves its files.
#[test]
fn a_judged_folder_serves_its_files_and_removal_answers_whether_one_was_there() {
    let (_root, dir) = scratch();
    write_private(&dir, "a", b"alpha").unwrap();
    write_private(&dir, "b", b"beta").unwrap();
    let judged = JudgedDir::judge(&dir).unwrap();
    assert!(judged.is_private() && !judged.is_missing());
    assert_eq!(judged.read("a", 16).unwrap(), b"alpha");
    assert_eq!(judged.read_head("b", 2).unwrap(), b"be");
    assert!(matches!(
        judged.read("c", 16),
        Err(PrivateFileError::Missing)
    ));
    assert!(matches!(
        read_private(&dir, "a", 2),
        Err(PrivateFileError::Malformed(_))
    ));
    assert!(remove_private(&dir, "a").unwrap());
    assert!(!remove_private(&dir, "a").unwrap());
    assert!(matches!(
        read_private(&dir, "a", 16),
        Err(PrivateFileError::Missing)
    ));
}

/// A cloud placeholder is a reparse point here whatever this thread's placeholder mode shows: a
/// private file and a private folder that a sync client (as OneDrive does) made placeholders of,
/// in a scratch folder registered as a sync root, are refused as a link is (`Symlink`, a folder a
/// reparse point), never read as the plain file and folder the process's default mode shows
/// them as; once the root is let go, they read as before.
/// NEGATIVE CONTROL (2026-11-16): `exposed` answering a guard that leaves the thread's mode as it
/// is (`aterm_dirfd::seam::leave_placeholders_disguised` on the test thread): `a placeholder file:
/// read Ok([107])`.
#[test]
fn a_cloud_placeholder_is_refused_as_a_reparse_point() {
    assert!(
        !crate::elevation::is_elevated(),
        "these tests take this process as unelevated wherever they do not say otherwise"
    );
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-private-win-")
        .tempdir()
        .expect("scratch folder");
    let sync = root.path().join("sync");
    std::fs::create_dir(&sync).expect("the sync root's folder");
    let (state, folder) = (sync.join("state"), sync.join("folder"));
    create_private_dir(&state).expect("a private folder");
    create_private_dir(&folder).expect("a private folder");
    write_private(&state, "key", b"k").expect("written");
    let synced = aterm_dirfd::seam::SyncRoot::register(&sync).expect("a sync root");
    synced
        .convert(&state.join("key"))
        .expect("the file a placeholder");
    synced.convert(&folder).expect("the folder a placeholder");
    assert_eq!(
        aterm_dirfd::seam::thread_placeholder_mode(),
        0,
        "the test thread runs in the process's default mode"
    );
    let read = read_private(&state, "key", 8);
    assert!(
        matches!(read, Err(PrivateFileError::Symlink)),
        "a placeholder file: read {read:?}"
    );
    assert!(matches!(
        open_private_rw(&state, "key"),
        Err(PrivateFileError::Symlink)
    ));
    assert!(PrivateLock::take(&state, "key", Duration::ZERO, "held").is_err());
    let refused = check_private_dir(&folder).expect_err("a placeholder folder");
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    assert!(judge_private_dir(&folder).is_err());
    assert!(check_owned_dir(&folder).is_err());
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), 0);

    drop(synced);
    assert_eq!(read_private(&state, "key", 8).expect("plain again"), b"k");
    check_private_dir(&folder).expect("a plain private folder again");
}

/// KEPT PARSES ON WINDOWS ARE KEPT BY THEIR BYTES: no file is held open, every read opens and
/// judges the file and reads its bytes, equal bytes at the same path reuse their parse (the
/// same bytes in a replacement too), and a same-length write in place through a handle still
/// open is read and parsed again. A file no longer private is refused and forgotten; the bound
/// evicts the first kept.
/// NEGATIVE CONTROL (2026-11-18): the parse found by its path alone (the byte comparison
/// dropped): the write in place hands the old parse back (`first`, not `again`).
#[test]
fn a_kept_parse_is_kept_by_its_bytes_and_reads_a_write_in_place() {
    use std::io::Write as _;
    let (_root, dir) = scratch();
    write_private(&dir, "book", b"first").unwrap();
    let kept = super::super::KeptParse::new(1);
    let parses = std::cell::Cell::new(0);
    let parse = |bytes: &[u8]| {
        parses.set(parses.get() + 1);
        bytes.to_vec()
    };
    assert_eq!(kept.read(&dir, "book", 64, parse).unwrap(), b"first");
    assert_eq!(kept.read(&dir, "book", 64, parse).unwrap(), b"first");
    assert_eq!(parses.get(), 1, "equal bytes are parsed once");
    write_private(&dir, "book", b"first").unwrap();
    assert_eq!(kept.read(&dir, "book", 64, parse).unwrap(), b"first");
    assert_eq!(
        parses.get(),
        1,
        "equal bytes in a replacement reuse their parse"
    );
    drop(
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(dir.join("book"))
            .expect("opened alone: no handle holds the file"),
    );

    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("book"))
        .unwrap();
    writer.write_all(b"again").unwrap();
    assert_eq!(
        kept.read(&dir, "book", 64, parse).unwrap(),
        b"again",
        "a same-length write in place, its handle still open, is read again"
    );
    assert_eq!(parses.get(), 2);
    drop(writer);
    assert!(matches!(
        kept.read(&dir, "book", 4, parse),
        Err(PrivateFileError::Malformed(_))
    ));

    write_private(&dir, "other", b"other").unwrap();
    assert_eq!(kept.read(&dir, "other", 64, parse).unwrap(), b"other");
    assert_eq!(parses.get(), 3);
    assert_eq!(kept.read(&dir, "book", 64, parse).unwrap(), b"again");
    assert_eq!(parses.get(), 4, "the evicted file is parsed again");

    let me = me();
    Descriptor::from_sddl(&format!("D:P(A;;FA;;;{me})(A;;FA;;;SY)(A;;FR;;;WD)"))
        .unwrap()
        .apply(
            open_to_rewrite(&dir.join("book")),
            Parts::DACL | Parts::PROTECTED_DACL,
        )
        .unwrap();
    assert!(matches!(
        kept.read(&dir, "book", 64, parse),
        Err(PrivateFileError::NotPrivate(_))
    ));
    assert_eq!(parses.get(), 4, "a refused file is not parsed");
}
