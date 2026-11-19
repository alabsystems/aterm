// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

const ALICE: &str = "/Users//alice";
const ENABLED: &str = "/System/Volumes/Data:\n\tIndexing enabled.\n";

/// The data volume's device number in the fake, and another volume's.
const VOLUME: u64 = 16_777_229;
const OTHER: u64 = 889_192_451;

/// Where abrew's casks are under alice's home ([`ABREW_CASKS`]), and the folders on the way.
const SUPPORT: &str = "/Users//alice/Library/Application Support";
const PKG: &str = "/Users//alice/Library/Application Support/aterm/pkg";
const CASKS: &str = "/Users//alice/Library/Application Support/aterm/pkg/abrew/Caskroom";

/// The app bundle of a cask abrew installed, which Spotlight must still find by name.
const CHROME: &str = "/Users//alice/Library/Application Support/aterm/pkg/abrew/Caskroom/\
                      google-chrome/157.0.8103.79/Google Chrome.app";

/// The firmlinks list alice's Mac carries: a top-level one, one inside `usr` (which the root
/// shows as another folder), one whose two paths are different folders, and one inside a folder
/// the volume does not hold.
const FIRMLINKED: &[u8] =
    b"/opt\topt\n/usr/local\tusr/local\n/usr/libexec/cups\tusr/libexec/cups\n\
                            /System/Library/Caches\tSystem/Library/Caches\n";

/// Every folder alice's Mac should leave out ([`Mac::alice`]), in the receipt's order.
const ALL: [&str; 18] = [
    "/Library",
    "/System/Volumes/Data/MobileSoftwareUpdate",
    "/System/Volumes/Data/usr",
    "/Users//Shared",
    "/Users//alice/Developer",
    "/Users//alice/Downloads",
    "/Users//alice/Library/Application Support/Steam",
    "/Users//alice/Library/Application Support/aterm/pkg/abrew/Applications",
    "/Users//alice/Library/Application Support/aterm/pkg/abrew/Cellar",
    "/Users//alice/Library/Application Support/aterm/pkg/store",
    "/Users//alice/Library/Application Support/aterm/state",
    "/Users//alice/Library/Caches",
    "/Users//alice/Library/Group Containers",
    "/Users//alice/Library/Mail",
    "/Users//alice/Library/Metadata",
    "/opt",
    "/private",
    "/usr/local",
];

/// How many of [`ALL`] are in alice's Library.
const IN_LIBRARY: usize = 9;

/// [`ALL`]'s folders that are not alice's: the data volume's.
const VOLUME_ONLY: [&str; 7] = [
    "/Library",
    "/System/Volumes/Data/MobileSoftwareUpdate",
    "/System/Volumes/Data/usr",
    "/Users//Shared",
    "/opt",
    "/private",
    "/usr/local",
];

/// The receipt main's home-only scope left: the home's top-level folders alone.
const HOME_ONLY: &[u8] = b"/Users//alice/Developer\n/Users//alice/Downloads\n";

/// A receipt naming every folder of [`ALL`].
fn full() -> Vec<u8> {
    full_but("")
}

/// A home-top folder of [`ALL`] made after setup scoped the home.
const DEVELOPER: &str = "/Users//alice/Developer";

/// A receipt naming every folder of [`ALL`] but `left`: a home scoped once, with a folder made
/// in it since.
fn full_but(left: &str) -> Vec<u8> {
    ALL.iter()
        .filter(|path| **path != left)
        .map(|path| format!("{path}\n"))
        .collect::<String>()
        .into_bytes()
}

/// A machine held in memory: each path's metadata, each root-owned file's bytes, what
/// `mdutil -s` prints, and a log of every folder listed and every path whose metadata was read.
struct Mac {
    macos: bool,
    euid: u32,
    home: Option<PathBuf>,
    entries: RefCell<BTreeMap<PathBuf, Entry>>,
    /// Where `realpath` takes a path; a path not named here is its own.
    resolved: RefCell<BTreeMap<PathBuf, PathBuf>>,
    files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    status: RefCell<Result<Vec<u8>, String>>,
    listed: RefCell<Vec<PathBuf>>,
    looked: RefCell<Vec<PathBuf>>,
}

const FOLDER: Entry = Entry {
    uid: 501,
    dir: true,
    symlink: false,
    id: None,
};

/// A folder on the data volume with inode `ino`.
const fn on_volume(ino: u64) -> Entry {
    Entry {
        uid: 0,
        dir: true,
        symlink: false,
        id: Some((VOLUME, ino)),
    }
}

impl Mac {
    /// Alice (501) at the console of a Mac whose data volume is indexed.
    /// - Her home holds Documents, Desktop, Library, Applications, Developer and Downloads, a
    ///   dot-folder, a `.noindex` folder, a link to a folder, a plain file and `mnt`, a disk
    ///   image mounted from another device.
    /// - Her Library holds Mail, Caches, Group Containers, Metadata, the synced drives'
    ///   CloudStorage (with a sync service's drive in it) and Mobile Documents, a `.noindex`
    ///   folder, a link, `Mounted` on another device, and Application Support: Steam beside
    ///   aterm, whose `state` and `pkg` are beside each other, `pkg` holding `store` and abrew,
    ///   and abrew holding Cellar, Applications (its links) and the Caskroom with Chrome's app.
    /// - The data volume's top level holds Applications, Library, Users, Volumes, opt, private,
    ///   usr and MobileSoftwareUpdate, the automounter's `home` on another device, a dot-folder,
    ///   a link and a file (both on the volume), and `stray`, whose numbers are unread. The system
    ///   volume's root shows Library, opt, private, Applications, Users and Volumes as the same
    ///   folders (firmlinks), another `usr`, and none named MobileSoftwareUpdate; its
    ///   `/usr/local` is the volume's `usr/local` ([`FIRMLINKED`]), its `/usr/libexec/cups`
    ///   another folder than the volume's.
    /// - /Users//Shared is a folder on the volume.
    fn alice() -> Self {
        let mac = Self {
            macos: true,
            euid: 501,
            home: Some(PathBuf::from(ALICE)),
            entries: RefCell::default(),
            resolved: RefCell::default(),
            files: RefCell::default(),
            status: RefCell::new(Ok(ENABLED.as_bytes().to_vec())),
            listed: RefCell::default(),
            looked: RefCell::default(),
        };
        mac.put(CONSOLE, file_of(501));
        mac.put(ALICE, on_volume(1000));
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
        mac.put(&format!("{ALICE}/Linked"), link());
        mac.put(&format!("{ALICE}/notes.txt"), file_of(501));
        for name in [
            "Mail",
            "Caches",
            "Group Containers",
            "Metadata",
            "CloudStorage",
            "Mobile Documents",
            "Index.noindex",
        ] {
            mac.put(&format!("{ALICE}/Library/{name}"), FOLDER);
        }
        mac.put(&format!("{ALICE}/Library/Linked"), link());
        mac.put(&format!("{ALICE}/Library/CloudStorage/SyncService"), FOLDER);
        mac.put(&format!("{ALICE}/Library"), on_volume(1001));
        mac.put(
            &format!("{ALICE}/mnt"),
            Entry {
                id: Some((OTHER, 5)),
                ..FOLDER
            },
        );
        mac.put(
            &format!("{ALICE}/Library/Mounted"),
            Entry {
                id: Some((OTHER, 6)),
                ..FOLDER
            },
        );
        for path in [
            SUPPORT,
            &format!("{SUPPORT}/Steam"),
            &format!("{SUPPORT}/aterm"),
            &format!("{SUPPORT}/aterm/state"),
            PKG,
            &format!("{PKG}/store"),
            &format!("{PKG}/abrew"),
            &format!("{PKG}/abrew/Cellar"),
            &format!("{PKG}/abrew/Applications"),
            CASKS,
            &format!("{CASKS}/google-chrome"),
        ] {
            mac.put(path, FOLDER);
        }
        mac.put(DATA_VOLUME, on_volume(2));
        for (ino, name, firmlink) in [
            (10, "Applications", true),
            (11, "Library", true),
            (12, "Users", true),
            (13, "Volumes", true),
            (14, "opt", true),
            (15, "private", true),
            (16, "MobileSoftwareUpdate", false),
            (17, ".Spotlight-V100", false),
            (19, "usr", false),
        ] {
            mac.put(&format!("{DATA_VOLUME}/{name}"), on_volume(ino));
            if firmlink {
                mac.put(&format!("/{name}"), on_volume(ino));
            }
        }
        mac.put(
            &format!("{DATA_VOLUME}/home"),
            Entry {
                id: Some((OTHER, 2)),
                ..on_volume(0)
            },
        );
        mac.put(&format!("{DATA_VOLUME}/stray"), FOLDER);
        mac.put(
            &format!("{DATA_VOLUME}/tmp"),
            Entry {
                id: Some((VOLUME, 18)),
                ..link()
            },
        );
        mac.put(
            &format!("{DATA_VOLUME}/installer.log"),
            Entry {
                id: Some((VOLUME, 21)),
                ..file_of(0)
            },
        );
        mac.put("/usr", on_volume(50));
        mac.put(&format!("{DATA_VOLUME}/usr/local"), on_volume(40));
        mac.put("/usr/local", on_volume(40));
        mac.put(&format!("{DATA_VOLUME}/usr/libexec/cups"), on_volume(41));
        mac.put("/usr/libexec/cups", on_volume(42));
        mac.file(FIRMLINKS, 0, FIRMLINKED);
        mac.put(SHARED, on_volume(20));
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
        id: None,
    }
}

fn link() -> Entry {
    Entry {
        symlink: true,
        ..FOLDER
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
        self.looked.borrow_mut().push(path.to_owned());
        self.entries
            .borrow()
            .get(path)
            .copied()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(self
            .resolved
            .borrow()
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.to_owned()))
    }

    fn names(&self, dir: &Path) -> io::Result<Vec<String>> {
        self.listed.borrow_mut().push(dir.to_owned());
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
    names.iter().map(PathBuf::from).collect()
}

/// [`ALL`] less the folders main's home-only receipt ([`HOME_ONLY`]) names.
fn beyond_the_home() -> BTreeSet<PathBuf> {
    let home_only = paths(&["/Users//alice/Developer", "/Users//alice/Downloads"]);
    paths(&ALL).difference(&home_only).cloned().collect()
}

/// The indexing state is the data volume's block of a run that succeeded, read by the one
/// `mdutil -s` parser: `mdutil -s -a`'s form, another volume off, reads the data volume alone.
/// NEGATIVE CONTROL: both lines, neither line, another sentence, a block of another volume only,
/// a block whose lines disagree and a failed run are errors, never "disabled".
#[test]
fn indexing_requires_one_recognized_state() {
    let mac = Mac::alice();
    assert_eq!(indexing(&mac), Ok(true));
    mac.prints(Ok("/System/Volumes/Data:\n\tIndexing disabled.\n"));
    assert_eq!(indexing(&mac), Ok(false));
    mac.prints(Ok(
        "/:\n\tIndexing disabled. \n/System/Volumes/Data:\n\tIndexing enabled. \n",
    ));
    assert_eq!(indexing(&mac), Ok(true));
    mac.prints(Ok(
        "/System/Volumes/Data:\n\tIndexing and searching disabled. \n",
    ));
    assert_eq!(indexing(&mac), Ok(false));
    for unread in [
        Ok("Spotlight server is disabled."),
        Ok("Indexing enabled.\nIndexing disabled."),
        Ok(""),
        Ok("/:\n\tIndexing enabled.\n"),
        Ok("/System/Volumes/Data:\n\tIndexing enabled.\n\tIndexing disabled.\n"),
        Ok("/System/Volumes/Data:\n\tError: unknown indexing state.\n"),
        Err("Spotlight status: exit 1"),
    ] {
        mac.prints(unread);
        assert!(indexing(&mac).is_err(), "{unread:?}");
    }
}

/// The scope keeps the owner's list and names everything else on the data volume: the home's
/// plain top-level folders but the kept three and Library, Library's but the synced drives' and
/// Application Support, and down the way to abrew's casks each folder's others; the volume's
/// top-level folders but Applications, Users and Volumes (each firmlinked one spelled at the
/// root, the others under the data volume, with the firmlinks inside those at the root's path),
/// and /Users//Shared.
/// NEGATIVE CONTROLS: none of these is ever named, nor any folder holding an app abrew
/// installed: ~/Library itself, CloudStorage, Mobile Documents, Documents, Desktop, either
/// Applications, the folders on the way to abrew's casks and the casks, Users, Volumes, the
/// automounter's `home`, a home or Library folder on another device, a dot-folder, a `.noindex`
/// folder, a link, a file (at the volume's top too, with its numbers read), a name with a
/// control character, a top-level folder whose numbers are unread, the root's own `usr`, and a
/// firmlink whose two paths are different folders; a home that cannot be listed is an error.
/// Without the firmlinks list `/usr/local` is not named. Main's home-only reading (the four kept
/// names, no descent) of the same Mac names neither ~/Library/Caches nor /opt.
#[test]
fn the_scope_keeps_the_owners_list_and_names_everything_else_on_the_data_volume() {
    let mac = Mac::alice();
    mac.put(&format!("{ALICE}/Bad\nName"), FOLDER);
    let home = Path::new(ALICE);
    let out = wanted(&mac, home).unwrap();
    assert_eq!(out, paths(&ALL));
    for (_, kept) in kept(home) {
        assert!(!out.contains(&kept), "{}", kept.display());
    }
    for path in &out {
        assert!(!Path::new(CHROME).starts_with(path), "{}", path.display());
    }
    for never in [
        "/Users//alice/Library",
        SUPPORT,
        "/Users//alice/Library/Application Support/aterm",
        PKG,
        "/Users//alice/Library/Application Support/aterm/pkg/abrew",
        CASKS,
        "/Users//alice/Library/Application Support/aterm/pkg/abrew/Caskroom/google-chrome",
        "/Users//alice/mnt",
        "/Users//alice/Library/Mounted",
        "/Users//alice/Library/CloudStorage",
        "/Users//alice/Library/CloudStorage/SyncService",
        "/Users//alice/Library/Mobile Documents",
        "/Users//alice/Library/Index.noindex",
        "/Users//alice/Library/Linked",
        "/Users//alice/.cargo",
        "/Users//alice/Cache.noindex",
        "/Users//alice/Linked",
        "/Users//alice/notes.txt",
        "/Users//alice/Bad\nName",
        "/Applications",
        "/Users",
        "/Volumes",
        "/System/Volumes/Data/Users",
        "/System/Volumes/Data/home",
        "/System/Volumes/Data/.Spotlight-V100",
        "/System/Volumes/Data/stray",
        "/System/Volumes/Data/tmp",
        "/System/Volumes/Data/installer.log",
        "/System/Volumes/Data/opt",
        "/System/Volumes/Data/usr/local",
        "/usr",
        "/usr/libexec/cups",
        "/System/Library/Caches",
    ] {
        assert!(!out.contains(Path::new(never)), "{never}");
    }
    assert_eq!(
        kept(home),
        [
            (Part::Applications, PathBuf::from("/Applications")),
            (
                Part::Applications,
                PathBuf::from("/Users//alice/Applications")
            ),
            (Part::Applications, PathBuf::from(CASKS)),
            (Part::Documents, PathBuf::from("/Users//alice/Documents")),
            (Part::Desktop, PathBuf::from("/Users//alice/Desktop")),
            (
                Part::SyncedDrives,
                PathBuf::from("/Users//alice/Library/CloudStorage")
            ),
            (
                Part::SyncedDrives,
                PathBuf::from("/Users//alice/Library/Mobile Documents")
            ),
        ]
    );

    let mains = children(
        &mac,
        home,
        &["Documents", "Desktop", "Library", "Applications"].map(str::to_owned),
    )
    .unwrap();
    assert_eq!(
        mains,
        paths(&["/Users//alice/Developer", "/Users//alice/Downloads"])
    );

    mac.entries.borrow_mut().remove(Path::new(FIRMLINKS));
    let unlinked = wanted(&mac, home).unwrap();
    assert!(!unlinked.contains(Path::new("/usr/local")));
    assert_eq!(unlinked.len(), ALL.len() - 1);
    assert!(!mains.contains(Path::new("/Users//alice/Library/Caches")));
    assert!(!mains.contains(Path::new("/opt")));

    assert!(wanted(&mac, Path::new("/Users//nobody")).is_err());
}

/// One reading lists the home, its Library, the folders on the way to abrew's casks and the data
/// volume's top level and nothing else, and reads the metadata of no path deeper than their
/// entries, the home's ancestors, the firmlinks list and the firmlinks' two paths: nothing under
/// CloudStorage, Mobile Documents, Documents, Desktop or the casks.
/// NEGATIVE CONTROL: the logs are not empty and hold ~/Library and an entry of it, so the
/// recorder works; a home with no Library still reads.
#[test]
fn the_scope_lists_the_home_its_library_and_the_volume_top_and_nothing_inside() {
    let mac = Mac::alice();
    assert!(owed(&mac));
    let listed: BTreeSet<PathBuf> = mac.listed.borrow().iter().cloned().collect();
    let roots = [
        ALICE,
        "/Users//alice/Library",
        SUPPORT,
        "/Users//alice/Library/Application Support/aterm",
        PKG,
        "/Users//alice/Library/Application Support/aterm/pkg/abrew",
        DATA_VOLUME,
    ];
    assert_eq!(listed, paths(&roots), "{:?}", mac.listed.borrow());
    let looked = mac.looked.borrow().clone();
    assert!(looked.contains(&PathBuf::from("/Users//alice/Library/Caches")));
    let named = [
        CONSOLE,
        RECEIPT,
        REFUSED,
        SHARED,
        ALICE,
        DATA_VOLUME,
        FIRMLINKS,
        "/",
        "/usr/local",
        "/System/Volumes/Data/usr/local",
        "/usr/libexec/cups",
        "/System/Volumes/Data/usr/libexec/cups",
        "/System/Volumes/Data/System/Library/Caches",
    ];
    for path in &looked {
        let under = |dir: &str| path.parent() == Some(Path::new(dir));
        assert!(
            named.iter().any(|named| path == Path::new(named))
                || roots.iter().any(|root| under(root))
                || under("/"),
            "{} was read",
            path.display()
        );
        assert!(!path.starts_with(format!("{CASKS}/")), "{}", path.display());
        for guarded in ["CloudStorage", "Mobile Documents"] {
            assert!(
                !path.starts_with(format!("{ALICE}/Library/{guarded}/")),
                "{}",
                path.display()
            );
        }
        for guarded in ["Documents", "Desktop"] {
            assert!(
                !path.starts_with(format!("{ALICE}/{guarded}/")),
                "{}",
                path.display()
            );
        }
    }

    let bare = Mac::alice();
    bare.entries
        .borrow_mut()
        .retain(|path, _| !path.starts_with("/Users//alice/Library"));
    assert_eq!(
        wanted(&bare, Path::new(ALICE)).map(|wanted| wanted.len()),
        Ok(ALL.len() - IN_LIBRARY),
        "a home with no Library leaves none of it out"
    );
}

/// A folder made later, at the home's top level, in ~/Library, on the way to abrew's casks or
/// at the data volume's top level, reads indexed and is work ([`owed`]), but asks no password by
/// itself: it waits for the next root transaction that runs for another reason, or a person's
/// own setup, so agents' checkouts and folders macOS and apps make ask for none.
/// NEGATIVE CONTROL: with the same full receipt, a new drive under CloudStorage, a new cask and
/// a folder inside Documents read scoped, since nothing reads any of them.
#[test]
fn a_new_folder_anywhere_is_work_but_asks_no_password_by_itself() {
    for (new, entry) in [
        ("/Users//alice/NewCheckout", FOLDER),
        ("/Users//alice/Library/NewThing", FOLDER),
        ("/Users//alice/Library/Application Support/NewApp", FOLDER),
        ("/System/Volumes/Data/newtop", on_volume(99)),
    ] {
        let mac = Mac::alice();
        mac.file(RECEIPT, 0, &full());
        assert!(!owed(&mac), "covered before {new}");
        mac.put(new, entry);
        assert!(owed(&mac), "{new} is work");
        assert!(!automatic_owed(&mac), "{new} asks no password");
        assert_eq!(
            state(&mac, Path::new(ALICE)),
            Ok(State::Indexed {
                folders: paths(&[new]),
                asks: false
            })
        );
    }
    for inside in [
        "/Users//alice/Library/CloudStorage/NewDrive",
        "/Users//alice/Library/Application Support/aterm/pkg/abrew/Caskroom/new-cask",
        "/Users//alice/Documents/x",
        "/Users//alice/Desktop/y",
    ] {
        let mac = Mac::alice();
        mac.file(RECEIPT, 0, &full());
        mac.put(inside, FOLDER);
        assert!(!owed(&mac), "{inside}");
        assert_eq!(
            state(&mac, Path::new(ALICE)),
            Ok(State::Scoped(ALL.len())),
            "{inside}"
        );
    }
}

/// A Mac that applied the home-only scope is owed the rest of the owner's list once, by the
/// automatic ask though its home was scoped: its receipt reads indexed for exactly ~/Library's
/// other folders, the volume's top-level folders and /Users//Shared.
/// NEGATIVE CONTROLS: that receipt plus those lines reads scoped; and a refusal naming them
/// owes nothing.
#[test]
fn a_receipt_of_the_home_only_scope_owes_the_rest_of_the_owners_list() {
    let mac = Mac::alice();
    let home = Path::new(ALICE);
    mac.file(RECEIPT, 0, HOME_ONLY);
    assert_eq!(
        state(&mac, home),
        Ok(State::Indexed {
            folders: beyond_the_home(),
            asks: true
        })
    );
    assert!(owed(&mac) && automatic_owed(&mac));

    let mut both = HOME_ONLY.to_vec();
    for path in beyond_the_home() {
        both.extend(format!("{}\n", path.display()).bytes());
    }
    mac.file(RECEIPT, 0, &both);
    assert_eq!(state(&mac, home), Ok(State::Scoped(ALL.len())));
    assert!(!owed(&mac) && !automatic_owed(&mac));

    let refused = Mac::alice();
    refused.file(RECEIPT, 0, HOME_ONLY);
    let rest: String = beyond_the_home()
        .iter()
        .map(|path| format!("{}\n", path.display()))
        .collect();
    refused.file(REFUSED, 0, rest.as_bytes());
    assert!(!owed(&refused) && !automatic_owed(&refused));
    refused.put("/Users//alice/NewCheckout", FOLDER);
    assert!(
        owed(&refused) && !automatic_owed(&refused),
        "CONTROL: a new home folder is work, and the refused rest asks for nothing again"
    );
}

/// A home on another volume keeps its own folders, which the data volume's configuration does
/// not govern, while the data volume's folders are still left out and owed.
/// NEGATIVE CONTROL: the same home on the volume names its folders too, as does one whose
/// numbers are unread (with its mount point, which unread numbers cannot tell apart).
#[test]
fn a_home_off_the_data_volume_keeps_its_folders_and_the_volume_is_still_scoped() {
    let mac = Mac::alice();
    mac.put(
        ALICE,
        Entry {
            id: Some((OTHER, 7)),
            ..on_volume(0)
        },
    );
    assert_eq!(
        state(&mac, Path::new(ALICE)),
        Ok(State::Indexed {
            folders: paths(&VOLUME_ONLY),
            asks: true
        })
    );
    assert!(owed(&mac));
    assert!(!mac.listed.borrow().iter().any(|dir| dir.starts_with(ALICE)));

    assert_eq!(wanted(&Mac::alice(), Path::new(ALICE)), Ok(paths(&ALL)));
    // With the home's numbers unread its mount point cannot be told from a folder either.
    let unread = Mac::alice();
    unread.put(ALICE, FOLDER);
    let mut all = paths(&ALL);
    all.insert(PathBuf::from("/Users//alice/mnt"));
    assert_eq!(wanted(&unread, Path::new(ALICE)), Ok(all));
}

/// The data volume's top-level folder that holds the home is never left out, found by its
/// numbers where the home's recorded path reaches it through a link (`/homes`, a link to
/// `System/Volumes/Data/homes`), where it sits under one firmlinked at the root
/// (`/opt/homes/alice`), and where a link to it spells that folder by its data-volume path.
/// NEGATIVE CONTROLS: for alice at /Users//alice the same `homes` and `/opt` are named; and
/// /Users//Shared is not named where it is a link, even with its numbers on the volume.
#[test]
fn the_folder_holding_the_home_is_kept_however_the_home_is_spelled() {
    let mac = Mac::alice();
    let linked_home = "/homes/alice";
    let real_home = "/System/Volumes/Data/homes/alice";
    mac.put("/homes", link());
    mac.put(&format!("{DATA_VOLUME}/homes"), on_volume(30));
    for home in [linked_home, real_home] {
        mac.put(home, on_volume(31));
        mac.put(&format!("{home}/Developer"), FOLDER);
    }
    mac.resolved
        .borrow_mut()
        .insert(PathBuf::from(linked_home), PathBuf::from(real_home));
    let out = wanted(&mac, Path::new(linked_home)).unwrap();
    assert!(out.contains(Path::new("/homes/alice/Developer")), "{out:?}");
    for never in ["/System/Volumes/Data/homes", "/homes"] {
        assert!(!out.contains(Path::new(never)), "{never}");
    }
    let alices = wanted(&mac, Path::new(ALICE)).unwrap();
    assert!(alices.contains(Path::new("/System/Volumes/Data/homes")));

    let opt_home = "/opt/homes/alice";
    mac.put("/opt/homes", FOLDER);
    mac.put(opt_home, on_volume(32));
    mac.put(&format!("{opt_home}/Developer"), FOLDER);
    let out = wanted(&mac, Path::new(opt_home)).unwrap();
    assert!(
        out.contains(Path::new("/opt/homes/alice/Developer")),
        "{out:?}"
    );
    assert!(!out.contains(Path::new("/opt")), "{out:?}");
    assert!(alices.contains(Path::new("/opt")));

    // A link whose target spells the firmlinked folder by its data-volume path.
    let via_data = "/h/alice";
    let data_spelled = "/System/Volumes/Data/opt/homes/alice";
    mac.put("/h", link());
    mac.put(via_data, on_volume(32));
    mac.put(data_spelled, on_volume(32));
    mac.put(&format!("{DATA_VOLUME}/opt/homes"), FOLDER);
    mac.resolved
        .borrow_mut()
        .insert(PathBuf::from(via_data), PathBuf::from(data_spelled));
    let out = wanted(&mac, Path::new(via_data)).unwrap();
    assert!(!out.contains(Path::new("/opt")), "{out:?}");

    let shared_link = Mac::alice();
    shared_link.put(
        SHARED,
        Entry {
            id: Some((VOLUME, 20)),
            ..link()
        },
    );
    let out = wanted(&shared_link, Path::new(ALICE)).unwrap();
    assert!(!out.contains(Path::new(SHARED)));
    assert_eq!(out.len(), ALL.len() - 1);
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
        Ok(paths(&["/Users//alice/Developer", "/Users//alice/Downloads"]))
    );
    mac.file(RECEIPT, 501, &full());
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
    let indexed = |folders| {
        Ok(State::Indexed {
            folders,
            asks: true,
        })
    };
    assert_eq!(state(&mac, home), indexed(paths(&ALL)));
    mac.file(RECEIPT, 0, b"/Users//alice/Developer\n");
    let mut rest = paths(&ALL);
    rest.remove(Path::new("/Users//alice/Developer"));
    assert_eq!(state(&mac, home), indexed(rest));
    mac.file(RECEIPT, 0, &full());
    assert_eq!(state(&mac, home), Ok(State::Scoped(ALL.len())));

    mac.file(RECEIPT, 501, &full());
    assert!(state(&mac, home).is_err());
    let root = Mac {
        euid: 0,
        ..Mac::alice()
    };
    root.file(RECEIPT, 501, &full());
    assert_eq!(state(&root, home), indexed(paths(&ALL)));

    mac.prints(Ok("/System/Volumes/Data:\n\tIndexing disabled.\n"));
    assert_eq!(state(&mac, home), Ok(State::Disabled));
}

/// The account at the console is owed the automatic ask while a folder of its home is indexed
/// and setup's password never scoped that home, and has work left ([`owed`]) while a folder is
/// on no receipt.
/// NEGATIVE CONTROLS: a receipt that scoped the home and the rest of the volume owes no
/// automatic ask, though the folders it does not name are still work; a receipt that is not
/// root's own file, an unread indexing state, a volume that is not indexed, another account at
/// the console, no console owner, no home, root, and a system that is not macOS owe neither.
#[test]
fn only_the_console_account_whose_home_was_never_scoped_is_owed() {
    let mac = Mac::alice();
    assert!(automatic_owed(&mac) && owed(&mac));
    mac.file(RECEIPT, 0, &full_but(DEVELOPER));
    assert!(!automatic_owed(&mac));
    assert!(owed(&mac));
    mac.file(RECEIPT, 0, &full());
    assert!(!automatic_owed(&mac) && !owed(&mac));

    let foreign = Mac::alice();
    foreign.file(RECEIPT, 501, b"");
    assert!(!automatic_owed(&foreign) && !owed(&foreign));

    let unread = Mac::alice();
    unread.prints(Ok("Spotlight server is disabled."));
    assert!(!automatic_owed(&unread) && !owed(&unread));
    unread.prints(Err("Spotlight status: timed out"));
    assert!(!automatic_owed(&unread) && !owed(&unread));

    let disabled = Mac::alice();
    disabled.prints(Ok("/System/Volumes/Data:\n\tIndexing disabled.\n"));
    assert_eq!(
        indexing(&disabled),
        Ok(false),
        "the disabled arm, not the unread"
    );
    assert!(!automatic_owed(&disabled) && !owed(&disabled));

    let elsewhere = Mac::alice();
    elsewhere.put(CONSOLE, file_of(502));
    assert!(!automatic_owed(&elsewhere) && !owed(&elsewhere));
    elsewhere.entries.borrow_mut().remove(Path::new(CONSOLE));
    assert!(!automatic_owed(&elsewhere) && !owed(&elsewhere));

    let homeless = Mac {
        home: None,
        ..Mac::alice()
    };
    assert!(!automatic_owed(&homeless) && !owed(&homeless));

    let root = Mac {
        euid: 0,
        ..Mac::alice()
    };
    root.put(CONSOLE, file_of(0));
    assert!(!automatic_owed(&root) && !owed(&root));

    let linux = Mac {
        macos: false,
        ..Mac::alice()
    };
    assert!(!automatic_owed(&linux) && !owed(&linux));
}

/// A folder made in a home setup's password already scoped asks no password by itself: the
/// person's own setup still has it to exclude ([`owed`]).
/// NEGATIVE CONTROLS: a receipt naming only another account's folders leaves this home asked
/// for; a home with nothing left to exclude is work for nobody.
#[test]
fn a_new_folder_in_a_scoped_home_is_work_but_asks_no_password() {
    let mac = Mac::alice();
    mac.file(RECEIPT, 0, &full());
    assert!(!automatic_owed(&mac));
    assert!(!owed(&mac));
    mac.put(&format!("{ALICE}/codex-worktree"), FOLDER);
    assert!(!automatic_owed(&mac));
    assert!(owed(&mac));

    let other = Mac::alice();
    other.file(RECEIPT, 0, b"/Users//bob/Developer\n/Users//bob/Downloads\n");
    assert!(automatic_owed(&other));
    assert!(owed(&other));
}

/// Folders an apply already failed to exclude are owed neither by the automatic ask nor, while
/// the refusal names every indexed folder, by the person's own setup.
/// NEGATIVE CONTROLS: a refusal naming only some of the indexed folders is still work; one
/// that is not root's own file says nothing, so the never-scoped home is asked for; a new
/// folder in the home is work again but asks no password by itself.
#[test]
fn a_refusal_scopes_the_home_once() {
    let mac = Mac::alice();
    mac.file(REFUSED, 0, &full_but(DEVELOPER));
    assert!(!automatic_owed(&mac));
    assert!(owed(&mac));
    mac.file(REFUSED, 0, &full());
    assert!(!automatic_owed(&mac));
    assert!(!owed(&mac));
    mac.put(&format!("{ALICE}/Scratch"), FOLDER);
    assert!(!automatic_owed(&mac));
    assert!(owed(&mac));

    let forged = Mac::alice();
    forged.file(REFUSED, 501, &full());
    assert!(automatic_owed(&forged));
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
    std::fs::write(&path, HOME_ONLY).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let me = Live.euid();
    assert_eq!(read_owned(&path, me, RECEIPT_MAX).unwrap(), HOME_ONLY);
    let entry = Live.entry(&path).unwrap();
    assert_eq!(Entry { id: None, ..entry }, file_of(me));
    let scratch_device = Live.entry(scratch.path()).unwrap().id.map(|id| id.0);
    assert!(
        entry.id.is_some() && entry.id.map(|id| id.0) == scratch_device,
        "the live reading carries the device and inode: {entry:?}"
    );
    assert_eq!(
        read_owned(&path, me.wrapping_add(1), RECEIPT_MAX)
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        read_owned(&path, me, HOME_ONLY.len() - 1)
            .unwrap_err()
            .kind(),
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
    mac.file(RECEIPT, 0, &full());
    assert!(!automatic_owed(&mac));
    mac.put(&format!("{ALICE}/NewCheckout"), FOLDER);
    assert!(owed(&mac), "the new folder remains visible as work");
    assert!(
        !automatic_owed(&mac),
        "a restart must not request the password again"
    );
    mac.file(RECEIPT, 501, &full());
    assert!(
        !automatic_owed(&mac),
        "an untrusted receipt never admits a password ask"
    );
}

/// A FOLDER KEPT OUT FROM THE PAGE LIES STRICTLY INSIDE DOCUMENTS, DESKTOP OR A SYNCED DRIVE,
/// by its words alone: those three and a folder deep in one are taken as given.
/// CONTROLS: each kept folder itself, Applications, a folder Apply scope keeps out (in the home,
/// in ~/Library), a path outside the home, one with `..`, one spelled other than its components
/// (an interior `.`, a doubled or trailing `/`), and one inside a folder already the person's are
/// refused, each in its own words.
#[test]
fn a_folder_kept_out_lies_inside_documents_desktop_or_a_synced_drive() {
    let home = Path::new(ALICE);
    let yours: BTreeSet<PathBuf> = [PathBuf::from("/Users//alice/Documents/Old")].into();
    for ok in [
        "/Users//alice/Documents/Archive",
        "/Users//alice/Desktop/Shots/2026",
        "/Users//alice/Library/CloudStorage/GoogleDrive-a/Big",
        "/Users//alice/Library/Mobile Documents/com~apple~CloudDocs/Documents/x",
    ] {
        assert_eq!(keep_out(home, &yours, Path::new(ok)), Ok(PathBuf::from(ok)));
    }
    for (folder, words) in [
        ("/Users//alice/Documents", "Spotlight keeps that folder"),
        (
            "/Users//alice/Library/CloudStorage",
            "Spotlight keeps that folder",
        ),
        ("/Users//alice/Applications/Tool", "apps are found by name"),
        ("/Users//alice/code/x", "Apply scope keeps"),
        ("/Users//alice/Library/Caches", "Apply scope keeps"),
        ("/Users//alice", "Apply scope keeps"),
        ("/Volumes/X/a", "not in the home"),
        ("/Users//alice/Documents/../code", "full path"),
        ("/Users//alice/Documents/./x", "full path"),
        ("/Users//alice//Documents/x", "full path"),
        ("/Users//alice/Documents/x/", "full path"),
        ("Documents/x", "full path"),
        (
            "/Users//alice/Documents/Old/inner",
            "you already kept it out",
        ),
        ("/Users//alice/Documents/Old", "you already kept it out"),
    ] {
        let refused = keep_out(home, &yours, Path::new(folder)).expect_err(folder);
        assert!(refused.contains(words), "{folder}: {refused}");
    }
}

/// A PAUSE IS JUDGED BY THE READING THAT STARTED AFTER IT: a reading begun before the pause
/// still reads Paused by you whatever it says; a later Disabled holds the pause; a later Enabled
/// ends it as On (macOS turned it back on). A resume reads Resuming until a later reading, which
/// ends it and is read as it says. The record's line reads back as itself.
/// CONTROLS: with no record a Disabled reading is Off, never by you, and no reading is Unread.
#[test]
fn a_pause_is_judged_by_the_reading_that_started_after_it() {
    let paused = PauseRecord {
        mark: Mark::Paused,
        at_unix_ms: 1_000,
    };
    let by_you = (
        Pause::ByYou {
            since_unix_ms: 1_000,
        },
        false,
    );
    assert_eq!(pause(Some(&paused), Some((true, 900))), by_you);
    assert_eq!(pause(Some(&paused), Some((false, 1_100))), by_you);
    assert_eq!(pause(Some(&paused), None), by_you);
    assert_eq!(pause(Some(&paused), Some((true, 1_100))), (Pause::On, true));
    let resumed = PauseRecord {
        mark: Mark::Resumed,
        at_unix_ms: 2_000,
    };
    assert_eq!(
        pause(Some(&resumed), Some((false, 1_900))),
        (Pause::Resuming, false)
    );
    assert_eq!(
        pause(Some(&resumed), Some((true, 2_100))),
        (Pause::On, true)
    );
    assert_eq!(
        pause(Some(&resumed), Some((false, 2_100))),
        (Pause::Off, true)
    );
    assert_eq!(
        pause(None, Some((false, 5))),
        (Pause::Off, false),
        "CONTROL"
    );
    assert_eq!(pause(None, None), (Pause::Unread, false), "CONTROL");
    for record in [paused, resumed] {
        assert_eq!(PauseRecord::parse(&record.line()), Some(record));
    }
    assert_eq!(PauseRecord::parse("paused soon"), None);
}
