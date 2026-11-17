// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::cell::Cell;
use std::io;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::{
    ANOTHER_LINK, ELEVATED, JudgedDir, OPEN_MODE, PrivateDir, PrivateFileError, PrivateLock,
    check_owned_dir, check_private_dir, create_private_dir, ensure_private_dir, judge_private_dir,
    open_private, open_private_rw, read_private, remove_private, write_private,
    write_private_flushed, write_private_new,
};
use crate::acl::{Descriptor, NotOwnerOnly, Parts, Sid};

thread_local! {
    /// Whether this thread's calls take this process as elevated (`None`: as it is).
    pub(super) static ELEVATED_HERE: Cell<Option<bool>> = const { Cell::new(None) };
}

/// `run` with this thread's calls taking this process as elevated.
fn as_elevated<R>(run: impl FnOnce() -> R) -> R {
    struct Back;
    impl Drop for Back {
        fn drop(&mut self) {
            ELEVATED_HERE.set(None);
        }
    }
    ELEVATED_HERE.set(Some(true));
    let _back = Back;
    run()
}

fn me() -> Sid {
    Sid::token_user().expect("this token's user")
}

/// A scratch folder under the temporary folder, and a private folder made in it.
fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
    assert!(
        !crate::elevation::is_elevated(),
        "these tests make private files, which an elevated process may not"
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
fn assert_owner_only(path: &Path, flags: &str) {
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
fn open_to_rewrite(path: &Path) -> std::fs::File {
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
fn junction(link: &Path, target: &Path) {
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

/// An elevated process makes and changes nothing here, and still reads.
/// NEGATIVE CONTROL: each maker, and removal, run as elevated is refused with `ELEVATED`, and
/// nothing is made or removed.
#[test]
fn an_elevated_process_makes_nothing_and_still_reads() {
    let (root, dir) = scratch();
    write_private(&dir, "key", b"k").unwrap();
    let missing = root.path().join("missing");
    let refused = |result: io::Result<()>| {
        let e = result.expect_err("refused elevated");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        assert_eq!(e.to_string(), ELEVATED);
    };
    as_elevated(|| {
        refused(write_private(&dir, "made", b"x"));
        refused(write_private_flushed(
            &dir,
            "made",
            b"x",
            aterm_durable::Flush::Durable,
        ));
        refused(write_private_new(&dir, "made", b"x", |_| Ok(())).map(drop));
        refused(create_private_dir(&missing));
        refused(ensure_private_dir(&missing).map(drop));
        refused(PrivateLock::take(&dir, ".lock", Duration::ZERO, "held").map(drop));
        assert!(matches!(
            open_private_rw(&dir, "made"),
            Err(PrivateFileError::Io(e)) if e.to_string() == ELEVATED
        ));
        refused(remove_private(&dir, "key").map(drop));
        assert_eq!(read_private(&dir, "key", 8).unwrap(), b"k", "it reads");
    });
    assert!(!missing.exists() && !dir.join("made").exists() && !dir.join(".lock").exists());
    assert_eq!(
        read_private(&dir, "key", 8).unwrap(),
        b"k",
        "nothing removed"
    );
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
    let (_root, dir) = scratch();
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
    heard
        .recv_timeout(Duration::from_secs(60))
        .expect("the waiter found the lock held");
    drop(first);
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
        "these tests make private files, which an elevated process may not"
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
