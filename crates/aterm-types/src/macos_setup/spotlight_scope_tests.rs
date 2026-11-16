// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

const ALICE: &str = "/Users//alice";
const ENABLED: &str = "/System/Volumes/Data:\n\tIndexing enabled.\n";

/// Both of alice's folders Spotlight should leave out, as a receipt lists them.
const FULL: &[u8] = b"/Users//alice/Developer\n/Users//alice/Downloads\n";

/// A machine held in memory: each path's metadata, each root-owned file's bytes, and what
/// `mdutil -s` prints.
struct Mac {
    macos: bool,
    euid: u32,
    home: Option<PathBuf>,
    entries: RefCell<BTreeMap<PathBuf, Entry>>,
    files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    status: RefCell<Result<Vec<u8>, String>>,
}

const FOLDER: Entry = Entry {
    uid: 501,
    dir: true,
    symlink: false,
};

impl Mac {
    /// Alice (501) at the console of a Mac whose data volume is indexed, her home holding
    /// Documents, Desktop, Library, Applications, Developer and Downloads, a dot-folder, a
    /// `.noindex` folder, a link to a folder and a plain file.
    fn alice() -> Self {
        let mac = Self {
            macos: true,
            euid: 501,
            home: Some(PathBuf::from(ALICE)),
            entries: RefCell::default(),
            files: RefCell::default(),
            status: RefCell::new(Ok(ENABLED.as_bytes().to_vec())),
        };
        mac.put(CONSOLE, file_of(501));
        mac.put(ALICE, FOLDER);
        for name in [
            "Documents",
            "Desktop",
            "Library",
            "Applications",
            "Developer",
            "Downloads",
            ".cargo",
            "Cache.noindex",
        ] {
            mac.put(&format!("{ALICE}/{name}"), FOLDER);
        }
        mac.put(
            &format!("{ALICE}/Linked"),
            Entry {
                symlink: true,
                ..FOLDER
            },
        );
        mac.put(&format!("{ALICE}/notes.txt"), file_of(501));
        mac
    }

    fn put(&self, path: &str, entry: Entry) {
        self.entries.borrow_mut().insert(PathBuf::from(path), entry);
    }

    /// A file at `path` owned by `uid`, holding `bytes`.
    fn file(&self, path: &str, uid: u32, bytes: &[u8]) {
        self.put(path, file_of(uid));
        self.files
            .borrow_mut()
            .insert(PathBuf::from(path), bytes.to_vec());
    }

    fn prints(&self, status: Result<&str, &str>) {
        *self.status.borrow_mut() = status
            .map(|text| text.as_bytes().to_vec())
            .map_err(str::to_owned);
    }
}

fn file_of(uid: u32) -> Entry {
    Entry {
        uid,
        dir: false,
        symlink: false,
    }
}

impl Reads for Mac {
    fn macos(&self) -> bool {
        self.macos
    }

    fn euid(&self) -> u32 {
        self.euid
    }

    fn home(&self) -> Option<PathBuf> {
        self.home.clone()
    }

    fn entry(&self, path: &Path) -> io::Result<Entry> {
        self.entries
            .borrow()
            .get(path)
            .copied()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    fn names(&self, dir: &Path) -> io::Result<Vec<String>> {
        if !self.entries.borrow().contains_key(dir) {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(self
            .entries
            .borrow()
            .keys()
            .filter(|path| path.parent() == Some(dir))
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect())
    }

    /// Root's own file only, as the port's rule says.
    fn root_file(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        if self.entry(path)?.uid != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let bytes = self
            .files
            .borrow()
            .get(path)
            .cloned()
            .ok_or(io::ErrorKind::NotFound)?;
        if bytes.len() > max {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(bytes)
    }

    fn volume_status(&self) -> Result<Vec<u8>, String> {
        self.status.borrow().clone()
    }
}

fn paths(names: &[&str]) -> BTreeSet<PathBuf> {
    names
        .iter()
        .map(|name| PathBuf::from(format!("{ALICE}/{name}")))
        .collect()
}

/// The indexing state is read only from exactly one recognized line of a run that succeeded.
/// NEGATIVE CONTROL: both lines, neither line, another sentence and a failed run are errors,
/// never "disabled".
#[test]
fn indexing_requires_one_recognized_state() {
    let mac = Mac::alice();
    assert_eq!(indexing(&mac), Ok(true));
    mac.prints(Ok("/System/Volumes/Data:\n\tIndexing disabled.\n"));
    assert_eq!(indexing(&mac), Ok(false));
    for unread in [
        Ok("Spotlight server is disabled."),
        Ok("Indexing enabled.\nIndexing disabled."),
        Ok(""),
        Err("Spotlight status: exit 1"),
    ] {
        mac.prints(unread);
        assert!(indexing(&mac).is_err(), "{unread:?}");
    }
}

/// The folders to leave out are the home's plain top-level folders other than the kept four.
/// NEGATIVE CONTROL: a dot-folder, a `.noindex` folder, a link, a plain file and a name with a
/// control character are not among them, and a home that cannot be listed is an error.
#[test]
fn wanted_names_the_plain_top_level_folders_outside_the_kept_four() {
    let mac = Mac::alice();
    mac.put(&format!("{ALICE}/Bad\nName"), FOLDER);
    assert_eq!(
        wanted(&mac, Path::new(ALICE)),
        Ok(paths(&["Developer", "Downloads"]))
    );
    assert!(wanted(&mac, Path::new("/Users//nobody")).is_err());
}

/// A receipt lists one path a line; no file lists none.
/// NEGATIVE CONTROL: a file there that is not root's is an error, never an empty list, as is
/// one that is not UTF-8.
#[test]
fn listed_reads_roots_file_and_refuses_another_owners() {
    let mac = Mac::alice();
    assert_eq!(listed(&mac, RECEIPT), Ok(BTreeSet::new()));
    mac.file(
        RECEIPT,
        0,
        b"/Users//alice/Developer\n\n/Users//alice/Downloads\n",
    );
    assert_eq!(
        listed(&mac, RECEIPT),
        Ok(paths(&["Developer", "Downloads"]))
    );
    mac.file(RECEIPT, 501, FULL);
    assert!(listed(&mac, RECEIPT).is_err());
    mac.file(RECEIPT, 0, b"\xff\n");
    assert!(listed(&mac, RECEIPT).is_err());
}

/// The state goes by the receipt alone.
/// NEGATIVE CONTROL: a volume that is not indexed is `Disabled` whatever the receipt says; an
/// ordinary account's receipt that is not root's is an error, while root reads it as none.
#[test]
fn state_goes_by_the_receipt() {
    let mac = Mac::alice();
    let home = Path::new(ALICE);
    assert_eq!(
        state(&mac, home),
        Ok(State::Indexed(paths(&["Developer", "Downloads"])))
    );
    mac.file(RECEIPT, 0, b"/Users//alice/Developer\n");
    assert_eq!(state(&mac, home), Ok(State::Indexed(paths(&["Downloads"]))));
    mac.file(RECEIPT, 0, FULL);
    assert_eq!(state(&mac, home), Ok(State::Scoped(2)));

    mac.file(RECEIPT, 501, FULL);
    assert!(state(&mac, home).is_err());
    let root = Mac {
        euid: 0,
        ..Mac::alice()
    };
    root.file(RECEIPT, 501, FULL);
    assert_eq!(
        state(&root, home),
        Ok(State::Indexed(paths(&["Developer", "Downloads"])))
    );

    mac.prints(Ok("Indexing disabled."));
    assert_eq!(state(&mac, home), Ok(State::Disabled));
}

/// The account at the console is owed the step while a folder of its home is on no receipt.
/// NEGATIVE CONTROLS: a receipt naming every folder owes nothing; nor does one that is not
/// root's own file, an unread indexing state, a volume that is not indexed, another account at
/// the console, no console owner, no home, root, or a system that is not macOS.
#[test]
fn only_the_console_account_with_an_unlisted_folder_is_owed() {
    let mac = Mac::alice();
    assert!(owed(&mac));
    mac.file(RECEIPT, 0, b"/Users//alice/Developer\n");
    assert!(owed(&mac));
    mac.file(RECEIPT, 0, FULL);
    assert!(!owed(&mac));

    let foreign = Mac::alice();
    foreign.file(RECEIPT, 501, b"");
    assert!(!owed(&foreign));

    let unread = Mac::alice();
    unread.prints(Ok("Spotlight server is disabled."));
    assert!(!owed(&unread));
    unread.prints(Err("Spotlight status: timed out"));
    assert!(!owed(&unread));

    let disabled = Mac::alice();
    disabled.prints(Ok("Indexing disabled."));
    assert!(!owed(&disabled));

    let elsewhere = Mac::alice();
    elsewhere.put(CONSOLE, file_of(502));
    assert!(!owed(&elsewhere));
    elsewhere.entries.borrow_mut().remove(Path::new(CONSOLE));
    assert!(!owed(&elsewhere));

    let homeless = Mac {
        home: None,
        ..Mac::alice()
    };
    assert!(!owed(&homeless));

    let root = Mac {
        euid: 0,
        ..Mac::alice()
    };
    root.put(CONSOLE, file_of(0));
    assert!(!owed(&root));

    let linux = Mac {
        macos: false,
        ..Mac::alice()
    };
    assert!(!owed(&linux));
}

/// Folders an apply already failed to exclude are not owed again.
/// NEGATIVE CONTROLS: a refusal naming only some of the indexed folders is still owed, as is
/// one that is not root's own file, and a new folder in the home is owed again.
#[test]
fn a_refusal_naming_every_indexed_folder_is_not_owed() {
    let mac = Mac::alice();
    mac.file(REFUSED, 0, b"/Users//alice/Developer\n");
    assert!(owed(&mac));
    mac.file(REFUSED, 0, FULL);
    assert!(!owed(&mac));
    mac.put(&format!("{ALICE}/Scratch"), FOLDER);
    assert!(owed(&mac));

    let forged = Mac::alice();
    forged.file(REFUSED, 501, FULL);
    assert!(owed(&forged));
}

/// This Mac's file reading holds the opened file to the owner asked for.
/// NEGATIVE CONTROL: the same file read for another owner, made group-writable, reached
/// through a link, or longer than the bound is refused; the test's own file is read.
#[cfg(unix)]
#[test]
fn the_live_file_reading_admits_only_the_owners_bounded_regular_file() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("spotlight-scope-live");
    let path = scratch.path().join("receipt");
    std::fs::write(&path, FULL).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let me = Live.euid();
    assert_eq!(read_owned(&path, me, RECEIPT_MAX).unwrap(), FULL);
    assert_eq!(Live.entry(&path).unwrap(), file_of(me));
    assert_eq!(
        read_owned(&path, me.wrapping_add(1), RECEIPT_MAX)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        read_owned(&path, me, FULL.len() - 1).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let link = scratch.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(read_owned(&link, me, RECEIPT_MAX).is_err());
    assert!(Live.entry(&link).unwrap().symlink);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert_eq!(
        read_owned(&path, me, RECEIPT_MAX).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        Live.names(scratch.path()).unwrap().len(),
        2,
        "the folder lists the file and the link"
    );
}

/// A new checkout is visible work, but cannot reopen a completed password episode.
#[test]
fn completed_scope_needs_an_explicit_extension() {
    let mac = Mac::alice();
    assert!(automatic_owed(&mac));
    mac.file(RECEIPT, 0, FULL);
    assert!(!automatic_owed(&mac));
    mac.put(&format!("{ALICE}/NewCheckout"), FOLDER);
    assert!(owed(&mac), "the new folder remains visible as work");
    assert!(
        !automatic_owed(&mac),
        "a restart must not request the password again"
    );
    mac.file(RECEIPT, 501, FULL);
    assert!(
        !automatic_owed(&mac),
        "an untrusted receipt never admits a password ask"
    );
}
