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
    /// What [`Reads::mount_points`] answers.
    mounts: RefCell<Option<BTreeSet<PathBuf>>>,
    /// What [`Reads::broker_left`] answers: this account's record of the folders the broker
    /// left as they are.
    left: RefCell<Option<Vec<u8>>>,
    /// What [`Reads::now`] answers: no clock unless a test sets one.
    clock: RefCell<Option<u64>>,
}

const FOLDER: Entry = Entry {
    uid: 501,
    dir: true,
    symlink: false,
    id: None,
    mode: 0o750,
};

/// A folder of alice's on the data volume.
const MINE: Entry = Entry {
    id: Some((VOLUME, 3000)),
    ..FOLDER
};

/// A folder of root's on the data volume with inode `ino`, that only root may write.
const fn on_volume(ino: u64) -> Entry {
    Entry {
        uid: 0,
        dir: true,
        symlink: false,
        id: Some((VOLUME, ino)),
        mode: 0o755,
    }
}

/// Alice's home: her own folder on the data volume, with no sticky bit.
const HOME: Entry = Entry {
    uid: 501,
    mode: 0o750,
    ..on_volume(1000)
};

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
    /// - The mount table names `/` alone, and alice has no record of folders the broker left.
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
            mounts: RefCell::new(Some(BTreeSet::from([PathBuf::from("/")]))),
            left: RefCell::default(),
            clock: RefCell::default(),
        };
        mac.put(CONSOLE, file_of(501));
        mac.put(
            "/",
            Entry {
                uid: 0,
                ..on_volume(1)
            },
        );
        mac.put(ALICE, HOME);
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
            mac.put(&format!("{ALICE}/{name}"), MINE);
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
        mode: 0o644,
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

    fn home_of(&self, uid: u32) -> Option<PathBuf> {
        (uid == self.euid).then(|| self.home.clone()).flatten()
    }

    fn mount_points(&self) -> Option<BTreeSet<PathBuf>> {
        self.mounts.borrow().clone()
    }

    fn broker_left(&self) -> Option<Vec<u8>> {
        self.left.borrow().clone()
    }

    fn now(&self) -> Option<u64> {
        *self.clock.borrow()
    }
}

fn paths(names: &[&str]) -> BTreeSet<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

/// Alice's own folders at `names`, under her home.
fn home_paths(names: &[&str]) -> BTreeSet<PathBuf> {
    names
        .iter()
        .map(|name| Path::new(ALICE).join(name))
        .collect()
}

/// A receipt naming every folder of [`ALL`] beyond alice's home's top level: a home whose owed
/// folders are all ones the broker takes.
fn beyond_the_home_receipt() -> Vec<u8> {
    beyond_the_home()
        .iter()
        .map(|path| format!("{}\n", path.display()))
        .collect::<String>()
        .into_bytes()
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

/// The broker's reading is the console account's own home by the broker's rules: a folder the
/// password route failed to exclude is still the broker's to try, and no home is read for root
/// or for an account other than the console's.
/// NEGATIVE CONTROL: reading the refusal as `owed` does leaves the refused folders unowed.
#[test]
fn the_brokers_reading_is_the_console_home_by_the_receipt_alone() {
    let mac = Mac::alice();
    mac.file(REFUSED, 0, HOME_ONLY);
    assert_eq!(
        broker_owed(&mac),
        Some(Ok(home_paths(&["Developer", "Downloads"])))
    );
    let root = Mac {
        euid: 0,
        ..Mac::alice()
    };
    assert_eq!(broker_owed(&root), None);
    let away = Mac::alice();
    away.put(CONSOLE, file_of(502));
    assert_eq!(broker_owed(&away), None);
}

/// The broker is asked only where it would take a folder, and the home is read again after its
/// answer: what it excluded with none left is done, with some left more; a busy broker and an
/// unserved request are ones the clock may finish; a refusal, a Spotlight left as it is and an
/// exclusion of none still owed are definite, and a failed edit fails; each answer comes with
/// the folders it was asked for.
/// NEGATIVE CONTROLS: asking where nothing is owed records an ask on a scoped home; reading the
/// answer without reading the home again calls a partial exclusion done; an unchanged answer
/// read as a transient one asks again for folders the broker left.
#[cfg(unix)]
#[test]
fn the_broker_is_asked_only_where_a_folder_is_owed_and_read_again() {
    use aterm_uds::lan::ScopeAnswer;
    use std::cell::Cell;
    let mac = Mac::alice();
    mac.file(RECEIPT, 0, HOME_ONLY);
    let asked = Cell::new(0);
    let counted = |answer: ScopeAnswer| {
        asked.set(asked.get() + 1);
        answer
    };
    assert_eq!(
        exclude_owed(&mac, &|| counted(ScopeAnswer::Excluded(1))),
        (Pass::Nothing, BTreeSet::new())
    );
    assert_eq!(asked.get(), 0, "a scoped home asks nothing");

    mac.put(&format!("{ALICE}/trust"), MINE);
    mac.put(&format!("{ALICE}/ty"), MINE);
    let one_of_two = || {
        mac.file(
            RECEIPT,
            0,
            b"/Users//alice/Developer\n/Users//alice/Downloads\n/Users//alice/trust\n",
        );
        counted(ScopeAnswer::Excluded(1))
    };
    assert_eq!(
        exclude_owed(&mac, &one_of_two),
        (Pass::More(1), home_paths(&["trust", "ty"]))
    );
    let the_rest = || {
        mac.file(
            RECEIPT,
            0,
            b"/Users//alice/Developer\n/Users//alice/Downloads\n/Users//alice/trust\n\
              /Users//alice/ty\n",
        );
        counted(ScopeAnswer::Excluded(1))
    };
    assert_eq!(
        exclude_owed(&mac, &the_rest),
        (Pass::Done(1), home_paths(&["ty"]))
    );
    assert_eq!(asked.get(), 2);

    mac.put(&format!("{ALICE}/Scratch"), MINE);
    assert!(matches!(
        exclude_owed(&mac, &|| ScopeAnswer::Excluded(0)).0,
        Pass::Left(why) if why.contains("none")
    ));
    for (answer, pass) in [
        (ScopeAnswer::Busy, Pass::Busy),
        (
            ScopeAnswer::Unserved("restarting".into()),
            Pass::Unavailable("restarting".into()),
        ),
        (
            ScopeAnswer::Refused("not the console's".into()),
            Pass::Left("not the console's".into()),
        ),
        (
            ScopeAnswer::Unchanged("its last edit failed".into()),
            Pass::Left("its last edit failed".into()),
        ),
        (
            ScopeAnswer::Failed("the edit failed".into()),
            Pass::Failed("the edit failed".into()),
        ),
    ] {
        assert_eq!(
            exclude_owed(&mac, &|| answer.clone()),
            (pass, home_paths(&["Scratch"]))
        );
    }
}

/// Folders this account's record says the broker left as they are are not asked for again, and
/// owe the password a home they leave never scoped, so a Mac whose broker cannot serve the step
/// asks the password once; a folder made since is a new ask of every folder owed, and the
/// password then waits for its answer. Alice's receipt names every folder beyond her home's top
/// level, so the broker would take every folder owed.
/// NEGATIVE CONTROL: a reading that leaves out the record asks the broker again for the same
/// folders, and owes the password nothing.
#[cfg(unix)]
#[test]
fn folders_the_broker_left_are_not_asked_again_and_owe_the_password() {
    let mac = Mac::alice();
    mac.file(RECEIPT, 0, &beyond_the_home_receipt());
    *mac.left.borrow_mut() =
        Some(broker_left_text(&home_paths(&["Developer", "Downloads"]), 1_000).into_bytes());
    assert_eq!(
        broker_would_take(&mac),
        Some(Err(Leave::Stands(LEFT.to_owned())))
    );
    assert_eq!(
        exclude_owed(&mac, &|| panic!("a left plan asks no broker")),
        (Pass::Nothing, BTreeSet::new())
    );
    assert!(automatic_owed_beside(&mac, || true));
    mac.put(&format!("{ALICE}/trust"), MINE);
    assert_eq!(
        broker_would_take(&mac),
        Some(Ok(home_paths(&["Developer", "Downloads", "trust"])))
    );
    assert!(!automatic_owed_beside(&mac, || true));
}

/// A home setup's password never scoped is owed the password unless a broker of this build
/// serves this Mac and would take every folder the ask owes. The broker takes only the home's
/// own top-level folders, so a home whose scope owes folders in ~/Library or at the data
/// volume's top level (alice's, never scoped) is owed the password with the broker serving; one
/// whose owed folders are all at the top level (her receipt names every folder beyond it) is
/// not, unless the broker refuses the home (a sticky home, a `/Users` others may write) or
/// leaves Spotlight as it is for a cause that stands (a listing past its bound). The broker's
/// reading is read only where the scope is owed.
/// NEGATIVE CONTROLS: counting a broker that would take some folder as taking the home leaves
/// alice's never-scoped home owed nothing; counting every serving broker as taking it leaves the
/// refused and the bounded homes owed nothing.
#[test]
fn a_home_the_broker_would_not_take_is_still_owed_the_password() {
    use std::cell::Cell;
    let asked = Cell::new(0);
    let serves = |answer: bool| {
        let asked = &asked;
        move || {
            asked.set(asked.get() + 1);
            answer
        }
    };
    let never = Mac::alice();
    assert!(automatic_owed_beside(&never, serves(false)));
    assert!(automatic_owed_beside(&never, serves(true)));
    let at_top = || {
        let mac = Mac::alice();
        mac.file(RECEIPT, 0, &beyond_the_home_receipt());
        mac
    };
    let mac = at_top();
    assert!(automatic_owed_beside(&mac, serves(false)));
    assert!(!automatic_owed_beside(&mac, serves(true)));
    let sticky = at_top();
    sticky.put(
        ALICE,
        Entry {
            mode: 0o1750,
            ..HOME
        },
    );
    assert!(automatic_owed_beside(&sticky, serves(true)));
    let shared = at_top();
    shared.put(
        "/Users",
        Entry {
            mode: 0o775,
            ..on_volume(12)
        },
    );
    assert!(automatic_owed_beside(&shared, serves(true)));
    let crowded = at_top();
    for at in 0..LISTING_MAX {
        crowded.put(&format!("{ALICE}/.dot-{at}"), file_of(501));
    }
    assert!(automatic_owed_beside(&crowded, serves(true)));
    assert_eq!(asked.get(), 7);
    let scoped = Mac::alice();
    scoped.file(RECEIPT, 0, &full());
    assert!(!automatic_owed_beside(&scoped, serves(true)));
    assert_eq!(asked.get(), 7, "a home owed nothing reads no broker");
}

/// The broker takes only the console account's own top-level home folders the scope leaves out,
/// by the scope's own rule ([`wanted`] at the home's top level): of alice's, Developer and
/// Downloads, never Documents, Desktop, Applications or Library, a dot-folder, a `*.noindex`
/// folder, a link, a file or a folder on another device; and nothing in ~/Library, at the data
/// volume's top level, in /Users//Shared or behind a firmlink, which are the password route's. Its
/// reading lists the home alone, and a folder whose device numbers are unread is not taken. A
/// home the password never scoped plans those two folders.
/// NEGATIVE CONTROLS: a reading that takes the scope's whole `wanted` takes ~/Library's and the
/// volume's folders; one that keeps no name of the home takes Documents, Desktop, Applications
/// and Library; one held to the scope's own device rule takes the folder whose numbers are
/// unread.
#[test]
fn the_broker_takes_only_the_homes_own_top_level_folders_the_scope_leaves_out() {
    let mac = Mac::alice();
    // Its numbers unread, a folder is no proof of the home's device, though the scope names it.
    mac.put(&format!("{ALICE}/Unread"), FOLDER);
    let read = broker_home_reading(&mac, 501).unwrap();
    assert_eq!(read.home, PathBuf::from(ALICE));
    assert_eq!(read.wanted, home_paths(&["Developer", "Downloads"]));
    assert_eq!(*mac.listed.borrow(), [PathBuf::from(ALICE)]);
    assert!(
        wanted(&mac, Path::new(ALICE))
            .unwrap()
            .contains(Path::new("/Users//alice/Unread"))
    );
    assert_eq!(
        broker_plan(&mac, 501),
        Ok(home_paths(&["Developer", "Downloads"]))
    );
    // Main's home-only receipt names both: the broker owes this home nothing, while the
    // password route still owes it the rest of the scope.
    mac.file(RECEIPT, 0, HOME_ONLY);
    assert_eq!(broker_plan(&mac, 501), Ok(BTreeSet::new()));
    assert!(matches!(
        state(&mac, Path::new(ALICE)),
        Ok(State::Indexed { asks: true, .. })
    ));
}

/// The broker scopes a home only on the data volume, whose configuration governs no other
/// volume's index: a home whose numbers say another device, whose numbers are unread, or that
/// lies inside a filesystem mounted below the root, is refused, and none of them is listed.
/// NEGATIVE CONTROL: a broker that reads the device rule as the password route's (unread
/// numbers are on the volume) lists the home whose numbers are unread.
#[test]
fn the_broker_refuses_a_home_off_the_data_volume() {
    let other = Mac::alice();
    other.put(
        ALICE,
        Entry {
            id: Some((OTHER, 7)),
            ..HOME
        },
    );
    let unread = Mac::alice();
    unread.put(ALICE, Entry { id: None, ..HOME });
    let inside = Mac::alice();
    inside
        .mounts
        .borrow_mut()
        .as_mut()
        .unwrap()
        .insert(PathBuf::from("/Users"));
    for mac in [other, unread, inside] {
        assert!(matches!(
            broker_home_reading(&mac, 501),
            Err(Leave::Refused(why)) if why.contains("data volume")
        ));
        assert!(mac.listed.borrow().is_empty(), "{:?}", mac.listed.borrow());
    }
}

/// While the data volume is not indexed (the person paused indexing from the page, or it is off
/// for any other cause) the broker plans nothing and reads no home, and nothing asks it: a pause
/// is never undone or worked around by an exclusion, as on the password route.
/// NEGATIVE CONTROL: a plan that reads a volume not indexed as indexed plans alice's folders.
#[cfg(unix)]
#[test]
fn a_volume_not_indexed_plans_nothing_and_asks_nothing() {
    let mac = Mac::alice();
    mac.prints(Ok("/System/Volumes/Data:\n\tIndexing disabled.\n"));
    assert_eq!(broker_plan(&mac, 501), Ok(BTreeSet::new()));
    assert!(mac.listed.borrow().is_empty(), "{:?}", mac.listed.borrow());
    assert_eq!(broker_would_take(&mac), Some(Ok(BTreeSet::new())));
    assert_eq!(
        exclude_owed(&mac, &|| panic!("a volume not indexed asks no broker")),
        (Pass::Nothing, BTreeSet::new())
    );
    assert!(!automatic_owed_beside(&mac, || panic!("nothing is owed")));
}

/// Root reads its own receipt strictly: one past its bound is an error to the reading and to
/// the root step, never none, so no write of root's drops the other homes' lines; a receipt
/// that is not root's own file, or none, reads as none, which root's next write replaces.
/// NEGATIVE CONTROL: root reading a receipt it cannot read as none calls the over-bound one
/// empty.
#[test]
fn root_reads_its_own_receipt_strictly_and_anothers_as_none() {
    let root = Mac {
        euid: 0,
        ..Mac::alice()
    };
    assert_eq!(listed_as_root(&root, RECEIPT), Ok(BTreeSet::new()));
    root.file(RECEIPT, 501, HOME_ONLY);
    assert_eq!(listed_as_root(&root, RECEIPT), Ok(BTreeSet::new()));
    root.file(RECEIPT, 0, HOME_ONLY);
    assert_eq!(
        listed_as_root(&root, RECEIPT),
        Ok(home_paths(&["Developer", "Downloads"]))
    );
    root.file(RECEIPT, 0, &vec![b'/'; RECEIPT_MAX + 1]);
    assert!(listed_as_root(&root, RECEIPT).is_err());
    assert!(state(&root, Path::new(ALICE)).is_err());
}

/// The broker reads no folder another filesystem is mounted on: such a top-level folder is left
/// out unread, a home that is a mount of its own is refused, and a mount table that could not
/// be read whole leaves the home unread.
/// NEGATIVE CONTROL: a listing kept whole, or a home not checked against the table, reads the
/// mounted folder or the mounted home.
#[test]
fn the_broker_reads_no_folder_a_filesystem_is_mounted_on() {
    let mac = Mac::alice();
    let mounted = PathBuf::from(format!("{ALICE}/Developer"));
    mac.mounts
        .borrow_mut()
        .as_mut()
        .unwrap()
        .insert(mounted.clone());
    // Any read of the mounted folder's metadata would reach into its filesystem.
    mac.put(&format!("{ALICE}/Developer"), Entry { uid: 0, ..MINE });
    let read = broker_home_reading(&mac, 501).unwrap();
    assert_eq!(read.wanted, home_paths(&["Downloads"]));
    assert!(
        !mac.looked.borrow().contains(&mounted),
        "{:?}",
        mac.looked.borrow()
    );

    let home = Mac::alice();
    home.mounts
        .borrow_mut()
        .as_mut()
        .unwrap()
        .insert(PathBuf::from(ALICE));
    assert!(matches!(
        broker_home_reading(&home, 501),
        Err(Leave::Refused(why)) if why.contains("filesystem of its own")
    ));
    assert!(!home.looked.borrow().contains(&PathBuf::from(ALICE)));

    let unread = Mac::alice();
    *unread.mounts.borrow_mut() = None;
    assert!(matches!(
        broker_home_reading(&unread, 501),
        Err(Leave::Unread(_))
    ));
}

/// A home keeps at most [`HOME_BYTES`] of the receipt: a folder whose line would pass its share
/// is left out while one that fits is taken, and a home whose share no owed folder fits stands;
/// another home's lines are not this home's share.
/// NEGATIVE CONTROL: a plan bounded by lines alone takes the long name past the share.
#[test]
fn a_home_is_bounded_by_its_share_of_the_receipt_bytes() {
    let mac = Mac::alice();
    let mut receipt = String::from_utf8(HOME_ONLY.to_vec()).unwrap();
    let mut at = 0;
    while HOME_BYTES - receipt.len() > 154 {
        receipt.push_str(&format!("{ALICE}/{at:0>40}\n"));
        at += 1;
    }
    // One line more leaves 30 bytes of alice's share: room for `short`'s line, not `long`'s.
    let filler = HOME_BYTES - 30 - receipt.len() - ALICE.len() - 2;
    receipt.push_str(&format!("{ALICE}/{}\n", "f".repeat(filler)));
    assert_eq!(receipt.len(), HOME_BYTES - 30);
    for at in 0..200 {
        receipt.push_str(&format!("/Users//bob/{at:0>60}\n"));
    }
    mac.file(RECEIPT, 0, receipt.as_bytes());
    let long = "l".repeat(60);
    mac.put(&format!("{ALICE}/{long}"), MINE);
    mac.put(&format!("{ALICE}/short"), MINE);
    assert_eq!(broker_plan(&mac, 501), Ok(home_paths(&["short"])));
    mac.file(RECEIPT, 0, format!("{receipt}{ALICE}/short\n").as_bytes());
    assert!(matches!(
        broker_plan(&mac, 501),
        Err(Leave::Stands(why)) if why.contains("share")
    ));
}

/// The broker's own lines of the receipt ([`BROKERED`]) stay within [`BROKER_MAX`]: the rest of
/// its bound is always the password route's, so the lines that route owes a home's ~/Library
/// and the data volume's top level still fit however many homes the broker served first. A
/// folder whose line fits under that half is taken; one whose line would pass it stands, though
/// the whole bound has room. Only the record's lines the receipt names count, so lines a receipt
/// lost take none of the half.
/// NEGATIVE CONTROLS: a half bounded by the whole receipt's lines plans nothing here; one that
/// counts the record whole, its lines the receipt lacks among them, stands at the first fill.
#[test]
fn the_broker_leaves_the_password_route_its_half_of_the_receipt() {
    let mac = Mac::alice();
    let folder = format!("{ALICE}/a-folder-of-alices");
    let line = folder.len() + 1;
    // The broker's own lines of other homes, to `short` bytes short of its half, in the receipt
    // and in its record, which also names lines the receipt lost; the password route's
    // (alice's) lines beside them in the receipt.
    let broker_short_of_half_by = |short: usize| {
        let mut own = String::new();
        let mut at = 0;
        while BROKER_MAX - short - own.len() > 1100 {
            own.push_str(&format!("/Users//bob/{at:04}-{}\n", "b".repeat(1000)));
            at += 1;
        }
        let room = BROKER_MAX - short - own.len();
        own.push_str(&format!("/{}\n", "p".repeat(room - 2)));
        assert_eq!(own.len(), BROKER_MAX - short);
        let receipt = format!("{}{own}", String::from_utf8(HOME_ONLY.to_vec()).unwrap());
        mac.file(RECEIPT, 0, receipt.as_bytes());
        let lost = format!("/Users//dave/{}\n", "d".repeat(8000));
        mac.file(BROKERED, 0, format!("{own}{lost}").as_bytes());
    };
    mac.put(&folder, MINE);
    broker_short_of_half_by(line);
    assert_eq!(
        broker_plan(&mac, 501),
        Ok(home_paths(&["a-folder-of-alices"]))
    );
    broker_short_of_half_by(line - 1);
    const { assert!(BROKER_MAX + 1 < RECEIPT_MAX) };
    assert!(matches!(
        broker_plan(&mac, 501),
        Err(Leave::Stands(why)) if why.contains("password route")
    ));
}

/// The password route's lines, however many, take none of the broker's half: with more than
/// half the receipt's bound of them and no line the broker recorded as its own, a folder the
/// broker takes is planned.
/// NEGATIVE CONTROL: a half that bounds the whole receipt leaves the folder out.
#[test]
fn the_password_routes_lines_past_half_leave_the_broker_its_own_half() {
    let mac = Mac::alice();
    mac.put(&format!("{ALICE}/a-folder-of-alices"), MINE);
    let mut receipt = String::from_utf8(HOME_ONLY.to_vec()).unwrap();
    let mut at = 0;
    while receipt.len() < 150 * 1024 {
        receipt.push_str(&format!(
            "/Users//bob/Library/{at:04}-{}\n",
            "b".repeat(1000)
        ));
        at += 1;
    }
    const { assert!(150 * 1024 > BROKER_MAX) };
    mac.file(RECEIPT, 0, receipt.as_bytes());
    assert_eq!(
        broker_plan(&mac, 501),
        Ok(home_paths(&["a-folder-of-alices"]))
    );
}

/// This account's record of the folders the broker left stands a day for `sync.spotlight`'s ask
/// ([`broker_would_ask`]) and at any age for the automatic ask ([`broker_would_take`]): dated
/// within the last [`LEFT_KEPT`] it asks nothing; a day on, undated, or dated after the clock's
/// time it asks again, while the automatic ask still reads the folders left, so the password
/// stays owed until the broker excludes them. A reader with no clock keeps it at any age.
/// NEGATIVE CONTROLS: a record honoured at any age by the ask asks nothing a day on; the
/// automatic ask reading it by its age owes the password nothing a day on.
#[cfg(unix)]
#[test]
fn a_left_record_stands_a_day_for_the_ask_and_any_age_for_the_password() {
    let mac = Mac::alice();
    mac.file(RECEIPT, 0, &beyond_the_home_receipt());
    let owed = home_paths(&["Developer", "Downloads"]);
    let left = Err(Leave::Stands(LEFT.to_owned()));
    let day = LEFT_KEPT.as_secs();
    *mac.left.borrow_mut() = Some(broker_left_text(&owed, 1_000).into_bytes());
    assert_eq!(broker_would_ask(&mac), Some(left.clone()), "no clock");
    for (now, stands) in [
        (1_000, true),
        (1_000 + day - 1, true),
        (1_000 + day, false),
        (999, false),
    ] {
        *mac.clock.borrow_mut() = Some(now);
        let asks = if stands {
            left.clone()
        } else {
            Ok(owed.clone())
        };
        assert_eq!(broker_would_ask(&mac), Some(asks), "{now}");
        assert_eq!(broker_would_take(&mac), Some(left.clone()), "{now}");
        assert!(automatic_owed_beside(&mac, || true), "{now}");
    }
    *mac.left.borrow_mut() = Some(b"/Users//alice/Developer\n/Users//alice/Downloads\n".to_vec());
    *mac.clock.borrow_mut() = Some(1_000);
    assert_eq!(broker_would_ask(&mac), Some(Ok(owed)), "undated");
    assert_eq!(broker_would_take(&mac), Some(left));
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
