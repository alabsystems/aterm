// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, export, user};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, FileId, LetsGo, Ran, Stat};

const ENV: &str = "alice$ CARGO_HOME=/Users//alice/.cargo RUSTUP_HOME=/Users//alice/.rustup /Users//alice/.cargo/bin/rustup";
const STABLE: &str =
    "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/components";
const PINNED: &str =
    "/Users//alice/.rustup/toolchains/1.97.1-aarch64-apple-darwin/lib/rustlib/components";
const WITH_DOCS: &[u8] =
    b"cargo-aarch64-apple-darwin\nrust-docs-aarch64-apple-darwin\nrustc-aarch64-apple-darwin\n";
const WITHOUT: &[u8] = b"cargo-aarch64-apple-darwin\nrustc-aarch64-apple-darwin\n";
const STABLE_HTML: &str =
    "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html";
const STABLE_ASIDE: &str =
    "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/share/doc/rust/html.noindex";

fn context() -> Ctx {
    ctx(&[user("alice", 501)])
}

#[test]
fn indexing_status_requires_a_successful_unambiguous_reading() {
    let status = "/usr/bin/mdutil -s /System/Volumes/Data";
    for (output, expected) in [
        ("/System/Volumes/Data:\n\tIndexing enabled.\n", Some(true)),
        ("/System/Volumes/Data:\n\tIndexing disabled.\n", Some(false)),
        ("Spotlight server is disabled.", None),
        ("Indexing enabled.\nIndexing disabled.", None),
        ("", None),
    ] {
        let sys = Fake::root();
        sys.on(status, Ran::ok(output));
        assert_eq!(
            spotlight_scope::indexing(&Port(&sys)).ok(),
            expected,
            "{output}"
        );
    }
    let sys = Fake::root();
    sys.on(
        status,
        Ran {
            status: Some(1),
            stdout: b"/System/Volumes/Data:\n\tIndexing disabled.\n".to_vec(),
            stderr: Vec::new(),
        },
    );
    assert!(spotlight_scope::indexing(&Port(&sys)).is_err());
}

const ALICE: &str = "/Users//alice";
const RECEIPT_WRITE: &str = "write /Library/Application Support/aterm/spotlight-scope 644";

fn folder() -> Stat {
    Stat {
        uid: 501,
        gid: 20,
        mode: 0o755,
        dir: true,
        symlink: false,
        len: 0,
    }
}

/// The data volume's device number in the fake, and the automounter's.
const VOLUME: u64 = 16_777_229;
const AUTOFS: u64 = 889_192_451;

/// A folder of alice's made at `path` on the data volume, its inode `ino`: one the scope and the
/// root LAN broker read alike, as macOS answers `lstat` for it.
pub(crate) fn made_on_volume(sys: &Fake, path: &str, ino: u64) {
    sys.file(path, b"");
    on_volume(sys, path, ino);
}

/// A folder on the data volume with inode `ino`.
fn on_volume(sys: &Fake, path: &str, ino: u64) {
    sys.stat_of(path, folder()).file_id_of(
        Path::new(path),
        FileId {
            dev: VOLUME,
            ino,
            born_ns: None,
        },
    );
}

/// A Mac's folders as the scope lists them.
/// - Alice's home, on the data volume: the folders Spotlight keeps, three it should leave out
///   (one with a space in its name), and what is never a candidate (a dot-folder, a `.noindex`
///   folder, a file, a link).
/// - Her Library: Caches and Mail, which it should leave out, and the synced drives'
///   CloudStorage and Mobile Documents, which it keeps.
/// - The data volume's top level: Applications, Users and Volumes, which it keeps; Library and
///   opt, firmlinked at the system volume's root; MobileSoftwareUpdate, which the root does not
///   show; and the automounter's `home` on another device.
/// - /Users//Shared.
pub(crate) fn home(sys: &Fake) {
    sys.file_id_of(
        Path::new(ALICE),
        FileId {
            dev: VOLUME,
            ino: 1000,
            born_ns: None,
        },
    );
    for (ino, name) in (3000..).zip([
        "Documents",
        "Desktop",
        "Library",
        "Applications",
        "Developer",
        "Downloads",
        "My Projects",
        ".cargo",
        "build.noindex",
        "Library/Caches",
        "Library/Mail",
        "Library/CloudStorage",
        "Library/Mobile Documents",
    ]) {
        made_on_volume(sys, &format!("{ALICE}/{name}"), ino);
    }
    sys.file(&format!("{ALICE}/notes.txt"), b"x")
        .link(&format!("{ALICE}/Linked"), "/Volumes/Work");
    on_volume(sys, DATA_VOLUME, 2);
    for (ino, name, firmlink) in [
        (10, "Applications", true),
        (11, "Library", true),
        (12, "Users", true),
        (13, "Volumes", true),
        (14, "opt", true),
        (16, "MobileSoftwareUpdate", false),
    ] {
        let path = format!("{DATA_VOLUME}/{name}");
        sys.file(&path, b"");
        on_volume(sys, &path, ino);
        if firmlink {
            on_volume(sys, &format!("/{name}"), ino);
        }
    }
    let autofs = format!("{DATA_VOLUME}/home");
    sys.file(&autofs, b"")
        .stat_of(&autofs, folder())
        .file_id_of(
            Path::new(&autofs),
            FileId {
                dev: AUTOFS,
                ino: 2,
                born_ns: None,
            },
        );
    on_volume(sys, spotlight_scope::SHARED, 20);
}

/// How the fake `plutil` treats an insert.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Config {
    /// It adds the path.
    Writes,
    /// It exits 0 and adds nothing.
    Deaf,
    /// It adds the path, and `mds` rewrites the configuration before each edit lands, adding
    /// an entry of its own each time ([`MDS_ADDED`] and its count), so no two rewrites are the
    /// same bytes.
    Rewritten,
    /// As [`Self::Rewritten`], at the first edit only.
    RewrittenOnce,
}

/// The entry `mds` adds to the configuration as it rewrites it under an edit ([`Config`]),
/// numbered from 1.
const MDS_ADDED: &str = "/Volumes/Added By mds";

/// What the fake configuration file holds where the key is absent, and where it is no array.
const NO_LIST: &[u8] = b"\0absent";
const NO_ARRAY: &[u8] = b"\0string";

/// The excluded folders the fake configuration at `path` lists, in order (`None`: the file
/// or its list is absent, or the key holds no array).
fn list_at(sys: &Fake, path: &str) -> Option<Vec<String>> {
    let bytes = sys.written(path)?;
    if bytes.starts_with(b"\0") {
        return None;
    }
    Some(
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect(),
    )
}

/// The excluded folders the live fake configuration lists ([`spotlight`]).
pub(crate) fn config_list(sys: &Fake) -> Option<Vec<String>> {
    list_at(sys, VOLUME_CONFIG)
}

fn put(sys: &Fake, path: &str, list: &[String]) {
    let text: String = list.iter().map(|entry| format!("{entry}\n")).collect();
    sys.file(path, text.as_bytes());
}

/// The data volume's Spotlight as root drives it: `mdutil -s` says indexing is enabled, the
/// configuration is a file holding `excluded` (`None`: it has no list yet), `plutil` reads,
/// extends and lints whichever file it is given (the live one or a copy), and
/// `killall -9 mds` succeeds.
pub(crate) fn spotlight(sys: &Fake, excluded: Option<&[&str]>, config: Config) {
    match excluded {
        Some(paths) => {
            let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
            put(sys, VOLUME_CONFIG, &paths);
        }
        None => {
            sys.file(VOLUME_CONFIG, NO_LIST);
        }
    }
    sys.simulate(move |sys, cmd| {
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match (cmd.program.to_str()?, args.as_slice()) {
            (MDUTIL, ["-s", DATA_VOLUME]) => {
                Some(Ran::ok("/System/Volumes/Data:\n\tIndexing enabled.\n"))
            }
            (PLUTIL, ["-extract", EXCLUSIONS, "xml1", "-o", "-", file]) => {
                Some(match list_at(sys, file) {
                    Some(paths) => {
                        let entries: String = paths
                            .iter()
                            .map(|path| format!("<string>{path}</string>"))
                            .collect();
                        export(&format!("<array>{entries}</array>"))
                    }
                    None if sys.written(file).as_deref() == Some(NO_ARRAY) => {
                        export("<string>not a list</string>")
                    }
                    None => Ran::exit(1, "No value at that key path"),
                })
            }
            (PLUTIL, ["-insert", EXCLUSIONS, "-array", file]) => {
                Some(if sys.written(file).as_deref() == Some(NO_LIST) {
                    put(sys, file, &[]);
                    Ran::ok("")
                } else {
                    Ran::exit(1, "Value already exists at key path")
                })
            }
            (PLUTIL, ["-insert", "Exclusions.0", "-string", path, file]) => {
                let mut list = list_at(sys, file)?;
                if config != Config::Deaf {
                    list.insert(0, (*path).to_owned());
                    put(sys, file, &list);
                }
                Some(Ran::ok(""))
            }
            (PLUTIL, ["-lint", _]) => {
                if matches!(config, Config::Rewritten | Config::RewrittenOnce) {
                    let mut live = config_list(sys)?;
                    let rewrites = live
                        .iter()
                        .filter(|entry| entry.starts_with(MDS_ADDED))
                        .count();
                    if config == Config::Rewritten || rewrites == 0 {
                        live.push(format!("{MDS_ADDED} {}", rewrites + 1));
                        put(sys, VOLUME_CONFIG, &live);
                    }
                }
                Some(Ran::ok(""))
            }
            (KILLALL, ["-9", "mds"]) => Some(Ran::ok("")),
            _ => None,
        }
    });
}

/// Alice at the console, which is whose home the root step scopes.
fn at_console() -> Ctx {
    let mut ctx = context();
    ctx.console = Some(user("alice", 501));
    ctx
}

fn scope_rows(sys: &Fake, mode: Mode) -> Vec<crate::model::ItemReport> {
    let items: Vec<Box<dyn Item>> = vec![Box::new(Scope)];
    engine::run(&items, mode, sys, &at_console(), &mut NoJournal)
}

fn count(sys: &Fake, call: &str) -> usize {
    sys.calls().iter().filter(|c| c.as_str() == call).count()
}

fn sorted(sys: &Fake) -> Vec<String> {
    let mut list = config_list(sys).unwrap();
    list.sort();
    list
}

const KILL: &str = "/usr/bin/killall -9 mds";
/// Every folder [`home`]'s Mac should leave out, as the receipt lists them.
pub(crate) const FULL: &[u8] = b"/Library\n\
    /System/Volumes/Data/MobileSoftwareUpdate\n\
    /Users//Shared\n\
    /Users//alice/Developer\n\
    /Users//alice/Downloads\n\
    /Users//alice/Library/Caches\n\
    /Users//alice/Library/Mail\n\
    /Users//alice/My Projects\n\
    /opt\n";

/// [`FULL`]'s folders beyond alice's home's top level: a receipt under which every folder owed
/// is one the root LAN broker takes.
pub(crate) const BEYOND_THE_HOME: &[u8] = b"/Library\n\
    /System/Volumes/Data/MobileSoftwareUpdate\n\
    /Users//Shared\n\
    /Users//alice/Library/Caches\n\
    /Users//alice/Library/Mail\n\
    /opt\n";

/// The receipt lines of alice's own top-level folders [`home`] holds for the root LAN broker to
/// exclude, the only folders of [`FULL`] it takes.
const AT_HOME: &[u8] =
    b"/Users//alice/Developer\n/Users//alice/Downloads\n/Users//alice/My Projects\n";

/// [`FULL`]'s folders, one a line.
pub(crate) fn full() -> Vec<String> {
    String::from_utf8(FULL.to_vec())
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Root keeps Spotlight to the owner's list: it adds every folder beside Applications,
/// Documents, Desktop and the synced drives to the excluded folders (the home's, its
/// Library's, the data volume's top level, spelled at the root where firmlinked, and
/// /Users//Shared), keeps the entry a person put there, restarts mds once, and leaves a receipt
/// naming what it excluded; a second run changes nothing, and a new Library folder is a change
/// again. The live configuration is only ever replaced by a rename of a checked copy with its
/// owner and mode, never edited where it stands.
/// NEGATIVE CONTROLS: no kept folder, dot-folder, `.noindex` folder, file, link, ~/Library
/// itself or mount point ever reaches the configuration, and no run turns indexing off with
/// `mdutil -i`.
#[test]
fn setup_keeps_spotlight_to_the_owners_list_and_excludes_the_rest() {
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&["/Volumes/Backup"]), Config::Writes);
    sys.stat_of(
        VOLUME_CONFIG,
        Stat {
            uid: 0,
            gid: 89,
            mode: 0o640,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    let planned = scope_rows(&sys, Mode::Plan);
    assert_eq!(
        planned[0].to.as_deref(),
        Some("excluded; Applications, Documents, Desktop and synced drives stay indexed"),
        "{planned:?}"
    );
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(
        rows[0].from.as_deref(),
        Some(
            "indexed: 3 home folders (Developer, Downloads, My Projects), 2 Library folders, \
             4 data-volume folders"
        )
    );
    assert_eq!(
        rows[0].to.as_deref(),
        Some(
            "setup has excluded each of 9 folders beside Applications, Documents, Desktop and \
             synced drives"
        )
    );
    let mut expected = full();
    expected.push("/Volumes/Backup".to_owned());
    expected.sort();
    assert_eq!(sorted(&sys), expected);
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(FULL));
    let home = Path::new(ALICE);
    let mut never: Vec<PathBuf> = spotlight_scope::kept(home)
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    never.extend(
        [
            "/Users//alice/Library",
            "/Users//alice/.cargo",
            "/Users//alice/build.noindex",
            "/Users//alice/notes.txt",
            "/Users//alice/Linked",
            "/Users",
            "/Volumes",
            "/System/Volumes/Data/home",
            "/System/Volumes/Data/opt",
            "/System/Volumes/Data/Library",
        ]
        .map(PathBuf::from),
    );
    for path in &never {
        assert!(
            !sorted(&sys).contains(&path.display().to_string()),
            "{}",
            path.display()
        );
    }
    let calls = sys.calls();
    let at = |start: &str| calls.iter().position(|c| c.starts_with(start)).unwrap();
    let landed = |c: &String| c.starts_with("rename ") && c.ends_with(VOLUME_CONFIG);
    assert_eq!(calls.iter().filter(|c| landed(c)).count(), 1, "{calls:?}");
    let rename = calls.iter().position(landed).unwrap();
    assert!(at("/usr/bin/plutil -lint") < rename, "{calls:?}");
    assert!(at("own 0:89 640") < rename, "{calls:?}");
    assert!(
        rename < at(KILL) && at(KILL) < at(RECEIPT_WRITE),
        "{calls:?}"
    );
    assert_eq!(count(&sys, KILL), 1);
    // No editing command ever named the live file.
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("/usr/bin/plutil -insert") && c.ends_with(VOLUME_CONFIG)),
        "{calls:?}"
    );

    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(count(&sys, KILL), 1);
    assert_eq!(count(&sys, RECEIPT_WRITE), 1);

    let new = format!("{ALICE}/Library/Containers");
    sys.file(&new, b"").stat_of(&new, folder());
    let rows = scope_rows(&sys, Mode::Plan);
    assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
    assert_eq!(rows[0].from.as_deref(), Some("indexed: 1 Library folder"));
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(count(&sys, KILL), 2);
    assert_eq!(config_list(&sys).unwrap().len(), expected.len() + 1);
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.starts_with("/usr/bin/mdutil -i")),
        "{:?}",
        sys.calls()
    );
}

/// A folder the person took out of the excluded folders stays out: an ordinary account and
/// root both read the receipt, which still names it, so nothing is owed, nothing is added
/// back and mds is not restarted, even when a new folder makes root edit the list again.
/// NEGATIVE CONTROL: the new folder of the same run is added, and so was Downloads the first
/// time, before any receipt named it.
#[test]
fn a_folder_the_person_made_searchable_again_is_never_excluded_again() {
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&[]), Config::Writes);
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    assert!(sorted(&sys).contains(&"/Users//alice/Downloads".to_owned()));

    let mut list = config_list(&sys).unwrap();
    list.retain(|path| path != "/Users//alice/Downloads");
    put(&sys, VOLUME_CONFIG, &list);
    assert_eq!(scope_rows(&sys, Mode::Plan)[0].outcome, Outcome::Ok);
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Ok);
    assert_eq!(count(&sys, KILL), 1);

    let new = format!("{ALICE}/Scratch");
    sys.file(&new, b"").stat_of(&new, folder());
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    let mut expected = full();
    expected.retain(|path| path != "/Users//alice/Downloads");
    expected.push("/Users//alice/Scratch".to_owned());
    expected.sort();
    assert_eq!(sorted(&sys), expected);
    // The receipt keeps naming it, which is the record of the person's choice.
    let receipt = String::from_utf8(sys.written(SCOPE_RECEIPT).unwrap()).unwrap();
    assert!(receipt.contains("/Users//alice/Downloads\n"), "{receipt}");
}

/// A home on another volume keeps its own folders, which the data volume's configuration does
/// not govern, while root still leaves out the data volume's folders: the probe plans a change
/// and the apply excludes the volume's folders alone.
/// NEGATIVE CONTROL: the same Mac with the home on the volume excludes its folders too
/// ([`setup_keeps_spotlight_to_the_owners_list_and_excludes_the_rest`]), and nothing of the
/// home reaches the configuration here.
#[test]
fn a_home_off_the_data_volume_keeps_its_folders_and_root_still_scopes_the_volume() {
    let sys = Fake::root();
    home(&sys);
    sys.stat_of(ALICE, folder()).file_id_of(
        Path::new(ALICE),
        FileId {
            dev: AUTOFS,
            ino: 7,
            born_ns: None,
        },
    );
    spotlight(&sys, Some(&[]), Config::Writes);
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(
        rows[0].from.as_deref(),
        Some("indexed: 4 data-volume folders")
    );
    assert_eq!(
        sorted(&sys),
        [
            "/Library",
            "/System/Volumes/Data/MobileSoftwareUpdate",
            "/Users//Shared",
            "/opt"
        ]
    );
    assert!(
        !sorted(&sys).iter().any(|path| path.starts_with(ALICE)),
        "{:?}",
        sorted(&sys)
    );
    assert_eq!(count(&sys, KILL), 1);
}

/// A receipt that is gone while the configuration already holds every folder is written
/// again with no edit and no restart of mds.
/// NEGATIVE CONTROL: the first run of the same world renames a copy in and kills mds once.
#[test]
fn a_lost_receipt_is_rewritten_without_restarting_the_indexer() {
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&[]), Config::Writes);
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    let renames = |sys: &Fake| {
        sys.calls()
            .iter()
            .filter(|c| c.starts_with("rename ") && c.ends_with(VOLUME_CONFIG))
            .count()
    };
    assert_eq!((count(&sys, KILL), renames(&sys)), (1, 1));

    sys.remove(Path::new(SCOPE_RECEIPT)).unwrap();
    assert_eq!(scope_rows(&sys, Mode::Plan)[0].outcome, Outcome::Planned);
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    assert_eq!((count(&sys, KILL), renames(&sys)), (1, 1));
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(FULL));
    assert_eq!(scope_rows(&sys, Mode::Plan)[0].outcome, Outcome::Ok);
}

/// A configuration with no list yet gets one. An edit that cannot be made leaves the live
/// configuration byte for byte as it was, mds running and no receipt, and records the
/// folders it could not exclude: a copy whose inserts did not take, a key that holds no
/// array, and a configuration that is not there. A later apply that succeeds removes that
/// record.
/// NEGATIVE CONTROL: the same world with a `plutil` that writes ends changed, with no record.
#[test]
fn setup_makes_a_missing_list_and_a_failed_edit_leaves_the_configuration_alone() {
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, None, Config::Writes);
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    assert_eq!(config_list(&sys).map(|list| list.len()), Some(full().len()));
    assert!(sys.written(SCOPE_REFUSED).is_none());

    let failed = |seed: &dyn Fn(&Fake)| {
        let sys = Fake::root();
        home(&sys);
        seed(&sys);
        let before = sys.written(VOLUME_CONFIG);
        let rows = scope_rows(&sys, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
        assert_eq!(count(&sys, KILL), 0);
        assert!(sys.written(SCOPE_RECEIPT).is_none());
        assert_eq!(sys.written(SCOPE_REFUSED).as_deref(), Some(FULL));
        (sys, before)
    };
    let (deaf, before) = failed(&|sys| spotlight(sys, Some(&["/Volumes/Backup"]), Config::Deaf));
    assert_eq!(deaf.written(VOLUME_CONFIG), before);
    let (no_array, before) = failed(&|sys| {
        spotlight(sys, Some(&[]), Config::Writes);
        sys.file(VOLUME_CONFIG, NO_ARRAY);
    });
    assert_eq!(no_array.written(VOLUME_CONFIG), before);
    let (absent, _) = failed(&|sys| {
        spotlight(sys, Some(&[]), Config::Writes);
        sys.remove(Path::new(VOLUME_CONFIG)).unwrap();
    });
    assert!(absent.written(VOLUME_CONFIG).is_none());

    // The same Mac once its configuration takes the edit.
    spotlight(&deaf, Some(&["/Volumes/Backup"]), Config::Writes);
    assert_eq!(scope_rows(&deaf, Mode::Apply)[0].outcome, Outcome::Changed);
    assert!(deaf.written(SCOPE_REFUSED).is_none());
    assert_eq!(deaf.written(SCOPE_RECEIPT).as_deref(), Some(FULL));
}

/// A configuration `mds` rewrote while the copy was edited is edited again from a fresh copy,
/// [`EDIT_TRIES`] times in all; one `mds` rewrote under each is left as `mds` left it, with mds
/// running and no receipt, and is no refusal: no record names the folders, so the next run
/// excludes them, and the home is still owed its scope.
/// NEGATIVE CONTROLS: an exclusion with no retry lints once; a rewrite recorded as a refusal
/// names every folder in `spotlight-scope.refused`, which takes them off the automatic ask.
#[test]
fn a_configuration_mds_rewrote_under_the_edit_is_no_refusal() {
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&["/Volumes/Backup"]), Config::Rewritten);
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert!(
        rows[0]
            .why
            .as_deref()
            .is_some_and(|why| why.contains("changed while it was being edited")),
        "{rows:?}"
    );
    let lints = sys
        .calls()
        .iter()
        .filter(|call| call.starts_with("/usr/bin/plutil -lint "))
        .count();
    assert_eq!(lints, EDIT_TRIES, "{:?}", sys.calls());
    assert_eq!(
        config_list(&sys).unwrap(),
        [
            "/Volumes/Backup",
            "/Volumes/Added By mds 1",
            "/Volumes/Added By mds 2",
            "/Volumes/Added By mds 3"
        ]
    );
    assert_eq!(count(&sys, KILL), 0);
    assert!(sys.written(SCOPE_RECEIPT).is_none());
    assert!(sys.written(SCOPE_REFUSED).is_none(), "{:?}", sys.calls());

    spotlight(&sys, None, Config::Writes);
    sys.file(VOLUME_CONFIG, b"/Volumes/Backup\n/Volumes/Added By mds\n");
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(FULL));
}

/// A configuration `mds` rewrote once under the password route's edit is edited again within
/// the run from a fresh copy, which keeps `mds`'s entry: the step that asked the person's
/// password excludes every folder, restarts `mds` once and writes its receipt, with no refusal.
/// NEGATIVE CONTROL: an exclusion with one try (`EDIT_TRIES` 1) fails the step on the rewrite.
#[test]
fn a_configuration_mds_rewrote_once_is_edited_again_within_the_run() {
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&["/Volumes/Backup"]), Config::RewrittenOnce);
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    let mut expected = full();
    expected.push("/Volumes/Backup".to_owned());
    expected.push("/Volumes/Added By mds 1".to_owned());
    expected.sort();
    assert_eq!(sorted(&sys), expected);
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(FULL));
    assert_eq!(count(&sys, KILL), 1);
    assert!(sys.written(SCOPE_REFUSED).is_none());
}

/// Every change `sys` made to Spotlight's configuration or a file (all but the configuration's
/// reads) lies inside a hold of root's lock ([`SCOPE_LOCK`]), and there was one.
fn changed_under_the_lock(sys: &Fake) {
    let calls = sys.calls();
    let (hold, go) = (format!("hold {SCOPE_LOCK}"), format!("let go {SCOPE_LOCK}"));
    let mut held = false;
    let mut changes = 0;
    for call in &calls {
        if *call == hold {
            held = true;
        } else if *call == go {
            held = false;
        } else if edits_one(call) && !call.starts_with("/usr/bin/plutil -extract ") {
            assert!(held, "{call} outside root's lock: {calls:?}");
            changes += 1;
        }
    }
    assert!(changes > 0, "{calls:?}");
}

/// Every root writer of Spotlight's configuration and of the files beside it holds root's one
/// lock ([`SCOPE_LOCK`]) from before its first change to after its last: while another holder
/// has it, the password route's step fails, naming nothing refused, the page's Keep a folder
/// out… fails, and the LAN broker answers busy, each editing nothing; once it is let go, each
/// goes ahead, every change it makes under its own hold.
/// NEGATIVE CONTROLS: the step's, the keep-out's and the broker's hold each taken out edits
/// Spotlight under another's lock; the broker's let go as soon as it is taken edits outside it.
#[test]
fn every_root_writer_of_spotlight_holds_its_one_lock() {
    use crate::port::System as _;
    const ARCHIVE: &str = "/Users//alice/Documents/Archive";
    let hold = |sys: &Fake| {
        sys.hold_root_lock(
            Path::new(SCOPE_LOCK),
            std::time::Duration::ZERO,
            "the other writer holds it",
        )
        .expect("the other writer's hold")
    };
    let mac = || {
        let sys = Fake::root();
        home(&sys);
        spotlight(&sys, Some(&[]), Config::Writes);
        sys
    };

    let sys = mac();
    let other = hold(&sys);
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert!(edits(&sys).is_empty(), "{:?}", sys.calls());
    assert!(sys.written(SCOPE_REFUSED).is_none(), "nothing was refused");
    drop(other);
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    changed_under_the_lock(&sys);

    let sys = mac();
    let other = hold(&sys);
    let rows = keep_out_rows(&sys, ARCHIVE);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    // Its probe reads the configuration, which is no edit.
    assert!(
        edits(&sys)
            .iter()
            .all(|call| call.starts_with("/usr/bin/plutil -extract ")),
        "{:?}",
        sys.calls()
    );
    assert_eq!(config_list(&sys).unwrap(), Vec::<String>::new());
    drop(other);
    assert_eq!(keep_out_rows(&sys, ARCHIVE)[0].outcome, Outcome::Changed);
    changed_under_the_lock(&sys);

    let sys = Arc::new(broker_mac());
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    let other = hold(&sys);
    assert_eq!(duty.apply(&plan), ScopeAnswer::Busy);
    assert!(edits(&sys).is_empty(), "{:?}", sys.calls());
    drop(other);
    assert_eq!(duty.apply(&plan), ScopeAnswer::Excluded(3));
    changed_under_the_lock(&sys);
}

/// The LAN broker reads the receipt under root's lock as it applies, and adds no folder the
/// receipt names since its plan: the password route's run in between excluded it, and the
/// person then took it back, which the broker leaves as they chose. The receipt keeps the
/// password route's line beside the broker's.
/// NEGATIVE CONTROL: an apply of the whole plan excludes the folder the person took back.
#[test]
fn the_broker_adds_no_folder_the_receipt_names_since_its_plan() {
    let sys = Arc::new(broker_mac());
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    assert_eq!(
        plan.folders,
        owed_paths(&["Developer", "Downloads", "My Projects"])
    );
    sys.file(SCOPE_RECEIPT, b"/Users//alice/Developer\n");
    assert_eq!(duty.apply(&plan), ScopeAnswer::Excluded(2));
    assert!(
        !config_list(&sys)
            .unwrap()
            .iter()
            .any(|path| path == "/Users//alice/Developer"),
        "{:?}",
        config_list(&sys)
    );
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(AT_HOME));
}

/// The password route's step and the page's Keep a folder out… wait for another holder of root's
/// lock, and go ahead once it lets go while they wait; the LAN broker only tries it, so it
/// answers busy and edits nothing while the same holder has it.
/// NEGATIVE CONTROLS: `hold_scope(sys, Duration::ZERO)` in `Scope::apply` or in `KeepOut::apply`
/// fails that step; the broker's hold taken with `LOCK_WAIT` excludes the plan.
#[test]
fn the_password_route_and_keep_out_wait_for_another_holder_and_the_broker_does_not() {
    const ARCHIVE: &str = "/Users//alice/Documents/Archive";
    let lock = Path::new(SCOPE_LOCK);
    let waited = format!("waited {SCOPE_LOCK}");
    let mac = || {
        let sys = Fake::root();
        home(&sys);
        spotlight(&sys, Some(&[]), Config::Writes);
        sys.held_elsewhere(lock, LetsGo::WhileWaited, |_| {});
        sys
    };

    let sys = mac();
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert!(sys.calls().contains(&waited), "{:?}", sys.calls());
    changed_under_the_lock(&sys);

    let sys = mac();
    let rows = keep_out_rows(&sys, ARCHIVE);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert!(sys.calls().contains(&waited), "{:?}", sys.calls());
    assert_eq!(config_list(&sys).unwrap(), [ARCHIVE]);

    let sys = Arc::new(broker_mac());
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    sys.held_elsewhere(lock, LetsGo::WhileWaited, |_| {});
    assert_eq!(duty.apply(&plan), ScopeAnswer::Busy);
    assert!(edits(&sys).is_empty(), "{:?}", sys.calls());
    assert!(!sys.calls().contains(&waited), "{:?}", sys.calls());
}

/// Each root writer reads root's receipt under its own hold of root's lock, after the last
/// write of the holder before it: a folder that holder noted (the person may have taken it back
/// since) is added again by neither the password route's step, which waited for it, nor the
/// broker's apply, which took the lock as it was let go; and the receipt keeps that line.
/// NEGATIVE CONTROLS: the receipt read above `hold_scope` in `Scope::apply`
/// (`listed_as_root`) or in the broker's apply (`broker_lines`) excludes Developer.
#[test]
fn each_writer_reads_the_receipt_under_its_hold() {
    let developer = "/Users//alice/Developer";
    let noted_developer = |sys: &Fake| {
        sys.file(SCOPE_RECEIPT, b"/Users//alice/Developer\n");
    };
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&[]), Config::Writes);
    sys.held_elsewhere(Path::new(SCOPE_LOCK), LetsGo::WhileWaited, noted_developer);
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    assert!(
        !config_list(&sys)
            .unwrap()
            .iter()
            .any(|path| path == developer),
        "{:?}",
        config_list(&sys)
    );
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(FULL));

    let sys = Arc::new(broker_mac());
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    sys.held_elsewhere(
        Path::new(SCOPE_LOCK),
        LetsGo::AtTheNextTake,
        noted_developer,
    );
    assert_eq!(duty.apply(&plan), ScopeAnswer::Excluded(2));
    assert!(
        !config_list(&sys)
            .unwrap()
            .iter()
            .any(|path| path == developer),
        "{:?}",
        config_list(&sys)
    );
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(AT_HOME));
}

/// The broker's half of the receipt and a home's share are judged again over the receipt read
/// under root's lock: where alice's share had room for Developer's line alone at the plan, a
/// password route's run between the plan and the apply that noted one more line of hers leaves
/// Spotlight as it is, with no edit.
/// NEGATIVE CONTROL: an apply that takes the plan less what the receipt names, unjudged,
/// excludes the planned folder past the home's share.
#[test]
fn the_brokers_bounds_hold_over_the_receipt_read_under_its_lock() {
    let sys = Arc::new(broker_mac());
    let developer = "/Users//alice/Developer\n".len();
    let share = spotlight_scope::HOME_BYTES - developer;
    let mut lines = String::new();
    let mut at = 0;
    while share - lines.len() > 300 {
        lines.push_str(&format!("{ALICE}/gone-{at:02}-{}\n", "g".repeat(200)));
        at += 1;
    }
    let room = share - lines.len();
    lines.push_str(&format!("{ALICE}/{}\n", "f".repeat(room - ALICE.len() - 2)));
    assert_eq!(lines.len(), share);
    sys.file(SCOPE_RECEIPT, lines.as_bytes());
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    assert_eq!(
        plan.folders,
        owed_paths(&["Developer"]),
        "room for one line of the share"
    );
    sys.held_elsewhere(
        Path::new(SCOPE_LOCK),
        LetsGo::AtTheNextTake,
        move |sys: &Fake| {
            sys.file(
                SCOPE_RECEIPT,
                format!("{lines}{ALICE}/gone-last\n").as_bytes(),
            );
        },
    );
    let answer = duty.apply(&plan);
    assert!(
        matches!(&answer, ScopeAnswer::Unchanged(why) if why.contains("full")),
        "{answer:?}"
    );
    assert!(edits(&sys).is_empty(), "{:?}", sys.calls());
}

/// The broker records its own lines of the receipt beside it, and only it: after its apply the
/// record names the folders it added, and a password route's run after it, which writes the
/// rest of the scope to the receipt, leaves the record as it was.
/// NEGATIVE CONTROL: the password route writing the record as its receipt names the whole
/// scope as the broker's.
#[test]
fn the_broker_records_its_own_lines_and_the_password_route_never_does() {
    let sys = Arc::new(broker_mac());
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    assert_eq!(duty.apply(&plan), ScopeAnswer::Excluded(3));
    assert_eq!(sys.written(SCOPE_BROKERED).as_deref(), Some(AT_HOME));
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(FULL));
    assert_eq!(sys.written(SCOPE_BROKERED).as_deref(), Some(AT_HOME));
}

/// The broker's record keeps only its lines the receipt still names: a line the receipt lacks (a
/// run that stopped before its receipt, the folder since removed) is dropped as it next applies,
/// so a line the password route later notes there is never counted as the broker's. Its line
/// the receipt names, of another home, is kept.
/// NEGATIVE CONTROL: the record written as all it named and the folders added keeps the lines
/// the receipt lacks.
#[test]
fn the_brokers_record_drops_lines_the_receipt_no_longer_names() {
    let sys = Arc::new(broker_mac());
    sys.file(SCOPE_RECEIPT, b"/Users//bob/Kept\n").file(
        SCOPE_BROKERED,
        b"/Users//alice/Gone\n/Users//bob/Gone\n/Users//bob/Kept\n",
    );
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    assert_eq!(duty.apply(&plan), ScopeAnswer::Excluded(3));
    let text = |path: &str| {
        sys.written(path)
            .map(|bytes| String::from_utf8(bytes).unwrap())
    };
    let ours = format!("{}/Users//bob/Kept\n", String::from_utf8_lossy(AT_HOME));
    assert_eq!(text(SCOPE_BROKERED).as_deref(), Some(ours.as_str()));
    assert_eq!(text(SCOPE_RECEIPT).as_deref(), Some(ours.as_str()));
}

/// The broker writes its record before the receipt, so a run that stops between them leaves no
/// receipt line its record lacks, which every reader would count as the password route's and so
/// let the broker's own lines pass their half: a record that cannot be written leaves the
/// receipt as it was, and the apply fails; a receipt that cannot be written after it leaves the
/// record naming lines the receipt lacks, which no reader counts.
/// NEGATIVE CONTROL: the receipt written before the record names the broker's folders where the
/// record failed, and leaves the record unwritten where the receipt failed.
#[test]
fn the_brokers_record_is_written_before_the_receipt() {
    let sys = Arc::new(broker_mac());
    sys.write_fails(SCOPE_BROKERED, io::ErrorKind::StorageFull);
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    let answer = duty.apply(&plan);
    assert!(matches!(answer, ScopeAnswer::Failed(_)), "{answer:?}");
    assert!(sys.written(SCOPE_RECEIPT).is_none(), "{:?}", sys.calls());

    let sys = Arc::new(broker_mac());
    sys.write_fails(SCOPE_RECEIPT, io::ErrorKind::StorageFull);
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    let answer = duty.apply(&plan);
    assert!(matches!(answer, ScopeAnswer::Failed(_)), "{answer:?}");
    assert_eq!(sys.written(SCOPE_BROKERED).as_deref(), Some(AT_HOME));
    assert!(sys.written(SCOPE_RECEIPT).is_none(), "{:?}", sys.calls());
}

/// A lock the broker could not take for a cause that passes (a full disk making its folder, no
/// file descriptor free in the broker or in the system) is busy, which the client asks again on
/// the clock; one it could not take for a cause that stands (a file there not root's) leaves
/// Spotlight as it is. Neither edits anything or is remembered.
/// NEGATIVE CONTROLS: every lock error but `WouldBlock` read as a cause that stands answers the
/// full disk Unchanged; `passing` without its `EMFILE | ENFILE` arm answers no descriptor free
/// Unchanged.
#[test]
fn a_lock_not_taken_for_a_passing_cause_is_busy_and_not_remembered() {
    let lock = Path::new(SCOPE_LOCK);
    for (why, fails, passes) in [
        (
            "a full disk",
            &(|sys: &Fake| {
                sys.exclusive_fails(lock, io::ErrorKind::StorageFull);
            }) as &dyn Fn(&Fake),
            true,
        ),
        (
            "no descriptor free in the broker",
            &|sys: &Fake| {
                sys.exclusive_fails_os(lock, libc::EMFILE);
            },
            true,
        ),
        (
            "no descriptor free in the system",
            &|sys: &Fake| {
                sys.exclusive_fails_os(lock, libc::ENFILE);
            },
            true,
        ),
        (
            "a file there not root's",
            &|sys: &Fake| {
                sys.exclusive_fails(lock, io::ErrorKind::PermissionDenied);
            },
            false,
        ),
    ] {
        let sys = Arc::new(broker_mac());
        let duty = duty_over(&sys);
        let plan = duty.plan(501).unwrap();
        fails(&sys);
        let answer = duty.apply(&plan);
        if passes {
            assert_eq!(answer, ScopeAnswer::Busy, "{why}");
        } else {
            assert!(
                matches!(answer, ScopeAnswer::Unchanged(_)),
                "{why}: {answer:?}"
            );
        }
        assert!(edits(&sys).is_empty(), "{why}: {:?}", sys.calls());
        assert_eq!(duty.plan(501), Ok(plan), "{why}");
    }
}

/// The password route's reading of the home holds no lock: every look into alice's home, the
/// step's probe and its apply alike, finds root's lock free, so a reading that hangs holds no
/// other root writer.
/// NEGATIVE CONTROL: `hold_scope` taken above `wanted` in `Scope::apply` finds it held.
#[test]
fn the_password_routes_reading_of_the_home_holds_no_lock() {
    use crate::port::System as _;
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&[]), Config::Writes);
    let free = Arc::new(Mutex::new(Vec::new()));
    {
        let free = Arc::clone(&free);
        sys.on_stat_read(move |sys, path| {
            if path.starts_with(ALICE) {
                let held =
                    sys.hold_root_lock(Path::new(SCOPE_LOCK), Duration::ZERO, "the reading's");
                free.lock().unwrap().push(held.is_ok());
            }
        });
    }
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    let free = free.lock().unwrap();
    assert!(!free.is_empty(), "the home was read");
    assert!(free.iter().all(|free| *free), "{free:?}");
}

/// A reading of a home ends before its result is handed on: a request that hears it finds no
/// reading held, so the next request reads the home rather than answer busy.
/// NEGATIVE CONTROL: a result handed on before the reading's end is heard with it still held.
#[test]
fn a_reading_of_a_home_ends_before_its_result_is_heard() {
    let reading = Arc::new(AtomicBool::new(true));
    let mut heard = None;
    read_then_end(
        Arc::clone(&reading),
        || "the home's folders",
        |read| heard = Some((read, reading.load(Ordering::Acquire))),
    );
    assert_eq!(heard, Some(("the home's folders", false)));
}

/// A volume whose indexing is already off is left off, and nothing is excluded or restarted;
/// off macOS and with no one at the console there is nothing to scope.
/// NEGATIVE CONTROL: the first test's enabled volume runs `plutil` and `killall`.
#[test]
fn a_disabled_volume_is_left_as_it_is() {
    let sys = Fake::root();
    home(&sys);
    sys.on(
        "/usr/bin/mdutil -s /System/Volumes/Data",
        Ran::ok("/System/Volumes/Data:\n\tIndexing disabled.\n"),
    );
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(sys.calls(), ["/usr/bin/mdutil -s /System/Volumes/Data"]);

    assert!(matches!(
        Scope.probe(&sys, &context()),
        Probe::NotApplicable { .. }
    ));
    let mut linux = Fake::root();
    linux.linux = true;
    assert!(matches!(
        Scope.probe(&linux, &at_console()),
        Probe::NotApplicable { .. }
    ));
    assert!(Scope.apply(&linux, &at_console()).is_err());
    assert!(linux.calls().is_empty());
}

/// An ordinary account reads only the receipt (the configuration is root's): the automatic ask
/// owes the step while a folder of its home is indexed and no receipt or refusal names any
/// folder of that home, and only at the console. Once one does, a folder the home or its
/// Library gains later asks no password by itself; the person's own setup still has it to
/// exclude.
/// NEGATIVE CONTROLS: a receipt that scoped the home owes no automatic ask; one that is not root's own
/// file, an unread indexing state, a disabled volume, another account at the console, root
/// and Linux owe nothing either, and no reading runs `plutil`; a refusal that is not root's own
/// file says nothing, so the never-scoped home is still owed.
#[test]
fn an_ordinary_account_is_owed_the_scope_by_the_receipt() {
    let console = |uid| Stat {
        uid,
        gid: 0,
        mode: 0o600,
        dir: false,
        symlink: false,
        len: 0,
    };
    let theirs = Stat {
        uid: 501,
        gid: 20,
        mode: 0o644,
        dir: false,
        symlink: false,
        len: 0,
    };
    let account = || {
        let mut sys = Fake::root();
        sys.euid = 501;
        home(&sys);
        sys.logged_in(&user("alice", 501))
            .stat_of("/dev/console", console(501));
        sys.on(
            "/usr/bin/mdutil -s /System/Volumes/Data",
            Ran::ok("/System/Volumes/Data:\n\tIndexing enabled.\n"),
        );
        sys
    };
    let sys = account();
    assert!(automatic_scope_owed(&sys));
    assert!(scope_owed(&sys));
    // Every folder but ~/Developer, made since setup scoped the home.
    let but_developer: Vec<u8> = full()
        .iter()
        .filter(|line| *line != "/Users//alice/Developer")
        .flat_map(|line| format!("{line}\n").into_bytes())
        .collect();
    sys.file(SCOPE_RECEIPT, &but_developer);
    assert!(!automatic_scope_owed(&sys));
    assert!(scope_owed(&sys));
    sys.file(SCOPE_RECEIPT, FULL);
    assert!(!automatic_scope_owed(&sys));
    assert!(!scope_owed(&sys));
    let library_new = format!("{ALICE}/Library/Containers");
    sys.file(&library_new, b"").stat_of(&library_new, folder());
    assert!(
        scope_owed(&sys) && !automatic_scope_owed(&sys),
        "a new Library folder is work and asks no password"
    );
    let home_new = format!("{ALICE}/Checkout");
    sys.file(&home_new, b"").stat_of(&home_new, folder());
    assert!(
        scope_owed(&sys) && !automatic_scope_owed(&sys),
        "a new home folder is work and asks no password"
    );
    assert!(
        !sys.calls().iter().any(|call| call.contains("plutil")),
        "{:?}",
        sys.calls()
    );

    let refused = account();
    refused.file(SCOPE_REFUSED, &but_developer);
    assert!(!automatic_scope_owed(&refused));
    assert!(scope_owed(&refused));
    refused.file(SCOPE_REFUSED, FULL);
    assert!(!automatic_scope_owed(&refused));
    assert!(!scope_owed(&refused));
    let new = format!("{ALICE}/Scratch");
    refused.file(&new, b"").stat_of(&new, folder());
    assert!(!automatic_scope_owed(&refused));
    assert!(scope_owed(&refused));
    let forged = account();
    forged
        .file(SCOPE_REFUSED, FULL)
        .stat_of(SCOPE_REFUSED, theirs);
    assert!(automatic_scope_owed(&forged));

    let foreign = account();
    foreign
        .file(SCOPE_RECEIPT, b"")
        .stat_of(SCOPE_RECEIPT, theirs);
    assert!(!automatic_scope_owed(&foreign));

    let unread = account();
    unread.on(
        "/usr/bin/mdutil -s /System/Volumes/Data",
        Ran::ok("Spotlight server is disabled."),
    );
    // The scripted replies answer in order: the enabled one first, then the unrecognized one.
    assert!(automatic_scope_owed(&unread));
    assert!(!automatic_scope_owed(&unread));

    let disabled = account();
    disabled.on(
        "/usr/bin/mdutil -s /System/Volumes/Data",
        Ran::ok("/System/Volumes/Data:\n\tIndexing disabled.\n"),
    );
    assert!(automatic_scope_owed(&disabled));
    assert!(!automatic_scope_owed(&disabled));

    let elsewhere = account();
    elsewhere.stat_of("/dev/console", console(502));
    assert!(!automatic_scope_owed(&elsewhere));

    let root = Fake::root();
    home(&root);
    root.logged_in(&user("alice", 501))
        .stat_of("/dev/console", console(0));
    assert!(!automatic_scope_owed(&root));

    let mut linux = account();
    linux.linux = true;
    assert!(!automatic_scope_owed(&linux));
}

fn run(sys: &Fake, mode: Mode) -> Vec<crate::model::ItemReport> {
    let alice = user("alice", 501);
    let rustup = Rustup {
        user: alice,
        cargo_home: "/Users//alice/.cargo".into(),
        rustup_home: "/Users//alice/.rustup".into(),
    };
    let items: Vec<Box<dyn Item>> = vec![
        Box::new(Profile(rustup.clone())),
        Box::new(NoDocs(rustup, LastReading::default())),
    ];
    engine::run(&items, mode, sys, &context(), &mut NoJournal)
}

/// A Mac whose rustup reports `profile` first, with stable's `rust-docs` HTML in place when
/// its components record lists them.
fn mac(profile: &str, stable: &[u8]) -> Fake {
    let sys = Fake::root();
    if stable == WITH_DOCS {
        sys.create_dir_all(Path::new(&format!("{STABLE_HTML}/std")), 0o755)
            .unwrap();
        sys.file(&format!("{STABLE_HTML}/std/index.html"), b"<html>");
    }
    sys.file("/Users//alice/.cargo/bin/rustup", b"")
        .on(&format!("{ENV} show profile"), Ran::ok(profile))
        .on(
            &format!("{ENV} toolchain list"),
            Ran::ok("stable-aarch64-apple-darwin\n1.97.1-aarch64-apple-darwin (active, default)\ntrust\n"),
        )
        .file(STABLE, stable)
        .file(PINNED, WITHOUT)
        .link(
            "/Users//alice/.rustup/toolchains/trust",
            "/Users//alice/Library/Application Support/aterm/pkg/rustup/trust",
        );
    sys
}

#[test]
fn a_default_profile_and_stables_docs_are_planned_and_nothing_runs() {
    let sys = mac("default\n", WITH_DOCS);
    let rows = run(&sys, Mode::Plan);
    assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
    assert_eq!(rows[1].outcome, Outcome::Planned, "{rows:?}");
    assert_eq!(
        rows[1].from.as_deref(),
        Some("rust-docs installed in stable-aarch64-apple-darwin")
    );
    assert_eq!(Reading::of(&rows[1]), Some(Reading::DocsInstalled));
    assert!(
        !sys.calls().iter().any(|c| c.contains(" set ")
            || c.contains(" remove ")
            || c.starts_with("rename ")
            || c.starts_with("symlink ")
            || c.starts_with("detach ")),
        "{:?}",
        sys.calls()
    );
}

/// Apply sets the profile, and keeps stable's docs out of Spotlight at once: the HTML is
/// renamed `html.noindex` with a link to it in its place, and rustup's removal of the
/// component, then of the HTML, starts in the background; no removal runs in the pass. Both
/// rows read back done at once.
#[test]
fn apply_sets_the_profile_and_the_docs_aside_at_once_and_removes_them_in_the_background() {
    let sys = mac("default\n", WITH_DOCS);
    let set = format!("{ENV} set profile minimal");
    sys.on(&format!("{ENV} show profile"), Ran::ok("minimal\n"))
        .on(&set, Ran::ok(""));
    let rows = run(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(rows[1].outcome, Outcome::Changed, "{rows:?}");
    let calls = sys.calls();
    assert_eq!(calls.iter().filter(|c| **c == set).count(), 1, "{calls:?}");
    let rename = format!("rename {STABLE_HTML} {STABLE_ASIDE}");
    let link = format!("symlink {STABLE_HTML} -> html.noindex");
    let removal = format!(
        "detach alice$ CARGO_HOME=/Users//alice/.cargo RUSTUP_HOME=/Users//alice/.rustup \
         /bin/sh -c {} sh /Users//alice/.cargo/bin/rustup stable-aarch64-apple-darwin \
         {STABLE_ASIDE} >> /dev/null",
        crate::port::shell_word(REMOVE_IN_BACKGROUND)
    );
    let at = |want: &str| calls.iter().position(|c| c == want);
    let (rename, link, removal) = (at(&rename), at(&link), at(&removal));
    assert!(
        rename.is_some() && rename < link && link < removal,
        "set aside, linked, then the removal in the background: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with(ENV) && c.contains(" component remove ")),
        "no removal runs in the pass: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("--toolchain trust") || c.contains("1.97.1")),
        "{calls:?}"
    );
    assert_eq!(
        sys.read_file(Path::new(&format!("{STABLE_ASIDE}/std/index.html")))
            .unwrap(),
        b"<html>"
    );
    // Read again, the docs set aside are kept out and nothing runs again.
    let again = run(&sys, Mode::Apply);
    assert_eq!(again[1].outcome, Outcome::Ok, "{again:?}");
    assert_eq!(Reading::of(&again[1]), None);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|c| c.starts_with("detach "))
            .count(),
        1,
        "{:?}",
        sys.calls()
    );
}

/// An aside an earlier run left is not this run's to remove: the row fails, says where it
/// is, and moves nothing.
#[test]
fn an_aside_already_there_fails_the_row_and_moves_nothing() {
    let sys = mac("minimal\n", WITH_DOCS);
    sys.create_dir_all(Path::new(STABLE_ASIDE), 0o755).unwrap();
    let rows = run(&sys, Mode::Apply);
    assert_eq!(rows[1].outcome, Outcome::Failed, "{rows:?}");
    assert!(
        rows[1]
            .why
            .as_deref()
            .is_some_and(|why| why.contains(STABLE_ASIDE)),
        "{rows:?}"
    );
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("rename ")
            || c.starts_with("symlink ")
            || c.starts_with("detach ")
            || c.contains(" component remove ")),
        "{:?}",
        sys.calls()
    );
}

#[test]
fn a_minimal_mac_is_ok_and_one_without_rustup_has_nothing_to_do() {
    let sys = mac("minimal\n", WITHOUT);
    let rows = run(&sys, Mode::Apply);
    assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
    let bare = Fake::root();
    let rows = run(&bare, Mode::Apply);
    assert!(
        rows.iter().all(|r| r.outcome == Outcome::NotApplicable),
        "{rows:?}"
    );
    assert!(bare.calls().is_empty(), "{:?}", bare.calls());
}

const MDFIND: &str =
    "alice$ /usr/bin/mdfind -onlyin /Users//alice -0 'kMDItemFSName == \"CACHEDIR.TAG\"'";
const TARGET: &str = "/Users//alice/work/demo/target";
/// The cache tag cargo writes at the top of a build folder.
const CARGO_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n# For information about cache directory tags see https://bford.info/cachedir/\n";

/// A scratch atpkg prefix: the item reads its store and rustup views on disk, by the
/// layout contract (`aterm_toolchain::layout`), never through the fake.
struct Pkg(aterm_tempfile::TempDir);

impl Pkg {
    fn new() -> Self {
        Self(aterm_tempfile::tempdir().unwrap())
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    /// `rustup/<name>` or `store/trust/<n>`, with its Rust sysroot.
    fn plain(&self, at: &str) -> String {
        let dir = self.path().join(at);
        std::fs::create_dir_all(dir.join("lib/rustlib")).unwrap();
        dir.display().to_string()
    }

    /// `<at>.noindex` with `<at>` a link to it, as atpkg lays one aside.
    fn aside(&self, at: &str) {
        let dir = self.path().join(at);
        let name = dir.file_name().unwrap().to_str().unwrap().to_owned();
        std::fs::create_dir_all(
            dir.with_file_name(format!("{name}.noindex"))
                .join("lib/rustlib"),
        )
        .unwrap();
        std::os::unix::fs::symlink(format!("{name}.noindex"), &dir).unwrap();
    }

    /// `store/trust/current`, a link to build `n`.
    fn current(&self, n: &str) {
        std::os::unix::fs::symlink(n, self.path().join("store/trust/current")).unwrap();
    }

    fn items(&self) -> Vec<Box<dyn Item>> {
        vec![Box::new(BuildNoindex::new(
            user("alice", 501),
            "/Users//alice".into(),
            self.path().to_path_buf(),
            "/Users//alice/.cargo".into(),
            Projects::Noted,
        ))]
    }
}

/// A Mac with aterm's caches made and `found`, each a folder holding cargo's cache tag,
/// with what Spotlight's search for them answers.
fn with_build_folders(found: &[&str]) -> (Fake, Ran) {
    let sys = Fake::root();
    for name in [".ubd/cargo-build.noindex", "tooling.noindex"] {
        sys.create_dir_all(
            &Path::new("/Users//alice/Library/Caches/aterm").join(name),
            0o755,
        )
        .unwrap();
    }
    let mut reply = String::new();
    for folder in found {
        sys.create_dir_all(Path::new(&format!("{folder}/debug")), 0o755)
            .unwrap();
        sys.file(&format!("{folder}/CACHEDIR.TAG"), CARGO_TAG);
        reply.push_str(&format!("{folder}/CACHEDIR.TAG\0"));
    }
    sys.file("/usr/bin/mdfind", b"");
    (sys, Ran::ok(&reply))
}

/// [`with_build_folders`], whose search finds them.
fn spotlit(found: &[&str]) -> Fake {
    let (sys, found) = with_build_folders(found);
    sys.on(MDFIND, found);
    sys
}

/// A Mac whose caches the item makes are `made`, with one project `target` of the
/// person's that Spotlight indexes.
fn with_a_spotlit_project(made: bool) -> Fake {
    let sys = Fake::root();
    sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
        .unwrap();
    if made {
        for name in [".ubd/cargo-build.noindex", "tooling.noindex"] {
            sys.create_dir_all(
                &Path::new("/Users//alice/Library/Caches/aterm").join(name),
                0o755,
            )
            .unwrap();
        }
    }
    sys.create_dir_all(Path::new(&format!("{TARGET}/debug")), 0o755)
        .unwrap();
    sys.file(&format!("{TARGET}/CACHEDIR.TAG"), CARGO_TAG);
    sys.file("/usr/bin/mdfind", b"")
        .on(MDFIND, Ran::ok(&format!("{TARGET}/CACHEDIR.TAG\0")));
    sys
}

/// The item on a Mac with no atpkg prefix.
fn alice_builds() -> Vec<Box<dyn Item>> {
    cargo_home_builds("/Users//alice/.cargo")
}

/// [`alice_builds`] with `CARGO_HOME` at `cargo_home`.
fn cargo_home_builds(cargo_home: &str) -> Vec<Box<dyn Item>> {
    searching(cargo_home, Projects::Noted)
}

/// The item on a Mac with no atpkg prefix, `CARGO_HOME` at `cargo_home`, searching for the
/// person's project build folders as `projects` says.
fn searching(cargo_home: &str, projects: Projects) -> Vec<Box<dyn Item>> {
    vec![Box::new(BuildNoindex::new(
        user("alice", 501),
        "/Users//alice".into(),
        "/Users//alice/Library/Application Support/aterm/pkg".into(),
        cargo_home.into(),
        projects,
    ))]
}

/// No rename and no link: what the item never does to a folder.
fn moved_nothing(sys: &Fake) {
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.starts_with("rename ") || c.contains("/bin/ln")),
        "{:#?}",
        sys.calls()
    );
}

/// The Rust in Spotlight row these rows read as.
fn row_of(rows: &[crate::model::ItemReport]) -> aterm_types::macos_setup::Observation {
    let refs: Vec<&crate::model::ItemReport> = rows.iter().collect();
    crate::init_observe::observation(aterm_types::macos_setup::Setting::Spotlight, &refs)
}

/// The item makes aterm's two caches and nothing else: the Trust toolchain build in use,
/// left plain in the store, is atpkg's move to make at its next update, so the plan
/// names it in its line and promises only the caches, and the build stays as it is.
#[test]
fn build_noindex_makes_its_caches_and_only_notes_the_toolchain_build_in_use() {
    let sys = Fake::root();
    sys.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(""));
    sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
        .unwrap();
    let pkg = Pkg::new();
    let build = pkg.plain("store/trust/15698");
    pkg.current("15698");
    let items = pkg.items();
    let planned = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(planned[0].outcome, Outcome::Planned, "{planned:?}");
    assert_eq!(
        Reading::of(&planned[0]),
        Some(Reading::BuildsIndexed {
            projects: 0,
            toolchain: true,
        })
    );
    assert!(
        planned[0]
            .from
            .as_deref()
            .is_some_and(|f| f.contains("cargo-build.noindex absent") && f.contains(&build)),
        "{planned:?}"
    );
    assert_eq!(
        planned[0].to.as_deref(),
        Some("cargo-build.noindex in .ubd, and tooling.noindex"),
        "the plan promises only what apply makes"
    );
    let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(rows[0].stood_aside, None);
    assert!(
        std::fs::symlink_metadata(&build).is_ok_and(|m| m.is_dir()),
        "the build stays where atpkg left it"
    );
    moved_nothing(&sys);
    assert!(
        sys.stat(Path::new(
            "/Users//alice/Library/Caches/aterm/.ubd/cargo-build.noindex"
        ))
        .is_ok_and(|s| s.dir)
    );
}

/// A Mac with the Trust toolchain atpkg installed: the build in use, left plain in the
/// store, is aterm's own move still to come, a quiet note on an in-place row and never
/// Needs you, on every pass ("no user required"), and the stage finishes. A `<n>` that
/// is a link to `<n>.noindex`, as atpkg now installs one, is kept out already and noted
/// by no one.
#[test]
fn an_atpkg_toolchain_build_is_a_note_and_never_needs_you() {
    let sys = with_a_spotlit_project(true);
    let pkg = Pkg::new();
    let build = pkg.plain("store/trust/15698");
    pkg.current("15698");
    let items = pkg.items();
    for mode in [Mode::Plan, Mode::Apply, Mode::Apply, Mode::Plan] {
        let rows = engine::run(&items, mode, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{mode:?}: {rows:?}");
        assert_eq!(rows[0].stood_aside, None);
        assert_eq!(
            Reading::of(&rows[0]),
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: true,
            })
        );
        let read = row_of(&rows);
        assert_eq!(
            read.standing,
            aterm_types::macos_setup::Standing::InPlace,
            "{mode:?}: {read:?}"
        );
        assert!(
            read.note.as_deref().is_some_and(|note| note.starts_with(
                "aterm moves the Rust toolchain it is using out of Spotlight at its next \
                 update; Spotlight indexes 1 of your project build folders"
            )),
            "{read:?}"
        );
        assert!(
            read.detail.as_deref().is_some_and(|d| d.contains(&build)),
            "the build stands behind the note: {read:?}"
        );
    }
    moved_nothing(&sys);

    // atpkg's layout: `<n>.noindex`, with `<n>` a link to it.
    let linked = spotlit(&[]);
    let pkg = Pkg::new();
    pkg.aside("store/trust/15698");
    pkg.current("15698");
    let rows = engine::run(
        &pkg.items(),
        Mode::Apply,
        &linked,
        &context(),
        &mut NoJournal,
    );
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(Reading::of(&rows[0]), None, "nothing to note");
    assert_eq!(row_of(&rows).note, None);
}

/// A run nobody asked for ([`Projects::Unread`]) puts no question of macOS's on the screen:
/// it neither asks Spotlight for the person's build folders nor reads one, so a project
/// `target` in Documents, whose first read can raise macOS's question about that folder, is
/// never reached, and the row still reads aterm's caches as in place. CONTROL: the row that
/// notes them searches, reads that folder's cache tag and notes it.
#[test]
fn a_run_nobody_asked_for_reads_no_folder_of_the_persons() {
    let documents = "/Users//alice/Documents/demo/target";
    for (projects, reads) in [(Projects::Unread, false), (Projects::Noted, true)] {
        let sys = spotlit(&[documents]);
        let items = searching("/Users//alice/.cargo", projects);
        let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{projects:?}: {rows:?}");
        let searched = sys.calls().iter().any(|call| call == MDFIND);
        assert_eq!(searched, reads, "{projects:?}: {:?}", sys.calls());
        let noted = Reading::of(&rows[0]).is_some_and(|reading| {
            reading
                == Reading::KeptOut {
                    projects: 1,
                    toolchain: false,
                }
        });
        assert_eq!(noted, reads, "{projects:?}: {rows:?}");
        assert_eq!(
            rows[0]
                .to
                .as_deref()
                .is_some_and(|to| to.contains(documents)),
            reads,
            "{projects:?}: {rows:?}"
        );
    }
}

/// The person's indexed project `target` is a quiet note with the folder named in its detail,
/// never a change, and is never renamed or linked whatever stands beside it.
#[test]
fn a_spotlit_project_folder_of_the_persons_is_a_note_and_never_touched() {
    let sys = with_a_spotlit_project(true);
    let items = alice_builds();
    for mode in [Mode::Plan, Mode::Apply] {
        let rows = engine::run(&items, mode, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{mode:?}: {rows:?}");
        assert_eq!(
            Reading::of(&rows[0]),
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: false,
            })
        );
        let line = format!(
            "Spotlight indexes these project build folders of yours, which aterm leaves as they are: {TARGET}"
        );
        assert!(
            rows[0].to.as_deref().is_some_and(|to| to.ends_with(&line)),
            "{rows:?}"
        );
        let read = row_of(&rows);
        assert_eq!(
            read.standing,
            aterm_types::macos_setup::Standing::InPlace,
            "{read:?}"
        );
        assert_eq!(
            read.note.as_deref(),
            Some(crate::init_observe::projects_note(1).as_str())
        );
        assert!(
            read.detail.as_deref().is_some_and(|d| d.ends_with(&line)),
            "the folder stands behind the note: {read:?}"
        );
    }
    // A `.noindex` beside it changes nothing: it is still theirs, still indexed.
    let kept = format!("{TARGET}/retained");
    sys.file(&kept, b"existing build bytes");
    sys.create_dir_all(Path::new(&format!("{TARGET}.noindex")), 0o755)
        .unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(
        sys.read_file(Path::new(&kept)).unwrap(),
        b"existing build bytes"
    );
    assert!(sys.stat(Path::new(TARGET)).unwrap().dir);
    moved_nothing(&sys);

    // Spotlight not answering costs the note, never aterm's own caches.
    let census = |reply| {
        let sys = Fake::root();
        sys.create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
            .unwrap();
        sys.file("/usr/bin/mdfind", b"").on(MDFIND, reply);
        sys
    };
    let incomplete = census(Ran::ok(""));
    incomplete.refuse_complete_stdout(MDFIND, "complete capture unavailable");
    let missing = Fake::root();
    missing
        .create_dir_all(Path::new("/Users//alice/Library/Caches"), 0o755)
        .unwrap();
    for unread in [
        census(Ran::exit(1, "census failed")),
        census(Ran::ok("/Users//alice/work/demo/target/CACHEDIR.TAG")),
        incomplete,
        missing,
    ] {
        let items = alice_builds();
        let rows = engine::run(&items, Mode::Apply, &unread, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(
            Reading::of(&rows[0]),
            None,
            "made and read back in place, with no project counted"
        );
        assert!(
            unread
                .stat(Path::new(
                    "/Users//alice/Library/Caches/aterm/.ubd/cargo-build.noindex"
                ))
                .is_ok_and(|s| s.dir)
        );
    }
}

/// Spotlight is asked for build folders by `BUILD_MARKER`, and a search that fails (here, over
/// the complete capture) stands its age like one that found.
#[test]
fn spotlight_is_asked_for_build_folders_and_a_failed_search_stands_its_age() {
    let sys = with_a_spotlit_project(true);
    let items = alice_builds();
    let searches = |sys: &Fake| sys.calls().iter().filter(|c| c.contains("mdfind")).count();
    let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(
        sys.calls().iter().filter(|c| c.as_str() == MDFIND).count(),
        1,
        "{:#?}",
        sys.calls()
    );
    assert_eq!(
        Reading::of(&rows[0]),
        Some(Reading::KeptOut {
            projects: 1,
            toolchain: false,
        })
    );

    let mut big = String::new();
    let mut n = 0;
    while big.len() <= aterm_uds::deadline::OUTPUT_KEEP {
        big.push_str(&format!("/Users//alice/work/p{n:06}/target/CACHEDIR.TAG\0"));
        n += 1;
    }
    let mut over = Fake::root();
    for name in [".ubd/cargo-build.noindex", "tooling.noindex"] {
        over.create_dir_all(
            &Path::new("/Users//alice/Library/Caches/aterm").join(name),
            0o755,
        )
        .unwrap();
    }
    over.file("/usr/bin/mdfind", b"").on(MDFIND, Ran::ok(&big));
    let items = alice_builds();
    for _ in 0..3 {
        let rows = engine::run(&items, Mode::Plan, &over, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        assert_eq!(Reading::of(&rows[0]), None, "the note goes, nothing else");
    }
    assert_eq!(searches(&over), 1, "a failed search stands its age too");
    over.now += INVENTORY_AGE;
    engine::run(&items, Mode::Plan, &over, &context(), &mut NoJournal);
    assert_eq!(searches(&over), 2);
}

/// One search of the home folder stands for every probe for [`INVENTORY_AGE`]: a run's
/// plan, apply and read-back, and the page's reading, ask Spotlight once.
#[test]
fn spotlight_is_searched_once_per_inventory_age() {
    let mut sys = with_a_spotlit_project(true);
    let items = alice_builds();
    let searches = |sys: &Fake| sys.calls().iter().filter(|c| c.contains("mdfind")).count();
    engine::run(&items, Mode::Apply, &sys, &context(), &mut NoJournal);
    engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(searches(&sys), 1, "{:#?}", sys.calls());
    sys.now += INVENTORY_AGE - 1;
    engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(searches(&sys), 1);
    sys.now += 1;
    let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(
        searches(&sys),
        2,
        "an inventory past its age is taken again"
    );
    assert_eq!(
        Reading::of(&rows[0]),
        Some(Reading::KeptOut {
            projects: 1,
            toolchain: false,
        })
    );
    // A clock that went back reads again.
    sys.now -= 10;
    engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
    assert_eq!(searches(&sys), 3);
}

/// Spotlight's build folders are the folders cargo tagged, whatever their names (a
/// `CARGO_TARGET_DIR` or `--target-dir` names its own), each counted once: a build
/// folder inside one found (a build script's own under its `OUT_DIR`) is part of it.
/// NEGATIVE CONTROL: a folder another tool tagged is no build folder of cargo's, and
/// a cargo build folder inside it still counts.
#[test]
fn a_cargo_build_folder_counts_once_whatever_its_name() {
    let read = |sys: &Fake| {
        let rows = engine::run(&alice_builds(), Mode::Plan, sys, &context(), &mut NoJournal);
        Reading::of(&rows[0])
    };
    let projects = |projects| {
        Some(Reading::KeptOut {
            projects,
            toolchain: false,
        })
    };
    assert_eq!(read(&spotlit(&["/Users//alice/brtgt"])), projects(1));
    let nested = format!("{TARGET}/debug/build/pmt-1/out/target");
    assert_eq!(read(&spotlit(&[TARGET, &nested])), projects(1));
    let beside = "/Users//alice/work/demo/target-main";
    assert_eq!(read(&spotlit(&[TARGET, &nested, beside])), projects(2));
    let other_tool = "/Users//alice/.cache/pip";
    let pip = |sys: &Fake| {
        sys.file(
            &format!("{other_tool}/CACHEDIR.TAG"),
            b"Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by pip.\n",
        );
    };
    let sys = spotlit(&[other_tool]);
    pip(&sys);
    assert_eq!(read(&sys), None, "nothing of cargo's to note");
    let inside = format!("{other_tool}/target");
    let sys = spotlit(&[other_tool, &inside]);
    pip(&sys);
    assert_eq!(read(&sys), projects(1));
}

/// Cargo tags its download caches in its home (`registry`, `git`) exactly as it tags a
/// build folder. NEGATIVE CONTROL: with a visible `CARGO_HOME`, which Spotlight indexes,
/// they are cargo's own and never counted as the person's project build folders; a
/// project's beside them still is. A cargo home that holds the home folder leaves out
/// only those caches.
#[test]
fn cargos_download_caches_are_no_project_build_folders() {
    let read = |items: Vec<Box<dyn Item>>, sys: &Fake| {
        let rows = engine::run(&items, Mode::Plan, sys, &context(), &mut NoJournal);
        Reading::of(&rows[0])
    };
    let caches = ["/Users//alice/cargo/registry", "/Users//alice/cargo/git"];
    let sys = spotlit(&caches);
    assert_eq!(
        read(cargo_home_builds("/Users//alice/cargo"), &sys),
        None,
        "nothing of the person's to note"
    );
    let sys = spotlit(&[caches[0], caches[1], TARGET]);
    assert_eq!(
        read(cargo_home_builds("/Users//alice/cargo"), &sys),
        Some(Reading::KeptOut {
            projects: 1,
            toolchain: false,
        })
    );
    let caches = ["/Users//alice/registry", "/Users//alice/git"];
    let sys = spotlit(&[caches[0], caches[1], TARGET]);
    assert_eq!(
        read(cargo_home_builds("/Users//alice"), &sys),
        Some(Reading::KeptOut {
            projects: 1,
            toolchain: false,
        })
    );
}

/// System State reads inside the app, whose environment a launch from the Dock gives
/// it names no `CARGO_HOME`: a visible cargo home a shell's startup file names
/// (`~/cargo`) is still cargo's, by the files cargo keeps at its top, whichever it
/// wrote (`.package-cache`, `.global-cache`), and its download caches are no project
/// build folders. So are those of `~/.cargo` linked to a visible folder, which
/// Spotlight reports by the folder's own path. NEGATIVE CONTROLS: a `registry` and a
/// `git` cargo tagged beside neither (a target folder of that name) are build folders
/// like any other, and a project's beside the caches always counts.
#[test]
fn a_cargo_home_the_environment_does_not_name_is_still_cargos() {
    let read = |sys: &Fake| {
        let rows = engine::run(&alice_builds(), Mode::Plan, sys, &context(), &mut NoJournal);
        Reading::of(&rows[0])
    };
    let projects = |projects| {
        Some(Reading::KeptOut {
            projects,
            toolchain: false,
        })
    };
    let visible = ["/Users//alice/cargo/registry", "/Users//alice/cargo/git"];
    for mark in [".package-cache", ".global-cache"] {
        let sys = spotlit(&[visible[0], visible[1], TARGET]);
        sys.file(&format!("/Users//alice/cargo/{mark}"), b"");
        assert_eq!(read(&sys), projects(1), "{mark}");
    }
    let sys = spotlit(&[visible[0], visible[1], TARGET]);
    assert_eq!(read(&sys), projects(3), "nothing of cargo's beside them");
    let real = [
        "/Users//alice/dev/cargo/registry",
        "/Users//alice/dev/cargo/git",
    ];
    let sys = spotlit(&[real[0], real[1], TARGET]);
    sys.link("/Users//alice/.cargo", "/Users//alice/dev/cargo");
    assert_eq!(read(&sys), projects(1), "~/.cargo is a link to them");
    let sys = spotlit(&[real[0], real[1], TARGET]);
    assert_eq!(
        read(&sys),
        projects(3),
        "no link, nothing of cargo's beside them"
    );
}

/// Check again ([`search_again`]) never waits on a search under way, which holds the
/// inventory's lock for as long as Spotlight takes (a minute, or more): the window that
/// presses it stays live. The search under way began before it, so the next reading
/// searches anew. The wait here is a hang detector: the stand-in search is held on a
/// channel until the forget has returned.
#[test]
fn check_again_never_waits_on_a_search_under_way() {
    use std::sync::mpsc;
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    let home = Path::new("/Users//alice");
    let inventory = Arc::new(Inventory::default());
    let (started, searching) = mpsc::channel();
    let (release, held) = mpsc::channel::<()>();
    let search = {
        let inventory = Arc::clone(&inventory);
        std::thread::spawn(move || {
            inventory.read(home, 0, || {
                started.send(()).unwrap();
                held.recv().unwrap();
                Ok(BTreeSet::new())
            })
        })
    };
    searching.recv_timeout(HANG).expect("the search began");
    let (forgot, forgetting) = mpsc::channel();
    let forget = {
        let inventory = Arc::clone(&inventory);
        std::thread::spawn(move || {
            inventory.forget();
            forgot.send(()).unwrap();
        })
    };
    let returned = forgetting.recv_timeout(HANG);
    release.send(()).unwrap();
    search.join().unwrap().unwrap();
    forget.join().unwrap();
    returned.expect("Check again waited on the search under way");
    let searched = std::cell::Cell::new(false);
    inventory
        .read(home, 0, || {
            searched.set(true);
            Ok(BTreeSet::new())
        })
        .unwrap();
    assert!(searched.get(), "a search begun before Check again is stale");
}

/// The build `store/trust/current` names is the one in use, which atpkg never moves
/// until an update makes another current, and the rustup view laid from it moves with
/// the next update too: one note says so, never "older". A plain build no longer in
/// use, atpkg's to move at its next pass, goes unsaid. With nothing plain left, the row
/// says the toolchain is kept out, with no note.
#[test]
fn only_the_toolchain_in_use_is_noted() {
    let sys = spotlit(&[]);
    let read = |pkg: &Pkg| {
        let rows = engine::run(&pkg.items(), Mode::Plan, &sys, &context(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        (row_of(&rows), rows[0].to.clone().unwrap_or_default())
    };
    let pkg = Pkg::new();
    let older = pkg.plain("store/trust/15600");
    let build = pkg.plain("store/trust/15698");
    pkg.current("15698");
    let view = pkg.plain("rustup/trust");
    let (row, line) = read(&pkg);
    assert_eq!(row.note.as_deref(), Some(TOOLCHAIN_NOTE), "{row:?}");
    assert!(
        line.contains(&build) && line.contains(&view) && !line.contains(&older),
        "{line}"
    );
    assert!(line.contains(&format!("{TOOLCHAIN_NOTE}: ")), "{line}");

    // The build in use laid aside, its view still plain: the view is the note.
    let pkg = Pkg::new();
    pkg.plain("store/trust/15600");
    pkg.aside("store/trust/15698");
    pkg.current("15698");
    let view = pkg.plain("rustup/trust");
    let (row, line) = read(&pkg);
    assert_eq!(row.note.as_deref(), Some(TOOLCHAIN_NOTE), "{row:?}");
    assert!(line.contains(&view), "{line}");

    // Both laid aside: the older plain build is no note, and no "kept out" is claimed.
    let pkg = Pkg::new();
    pkg.plain("store/trust/15600");
    pkg.aside("store/trust/15698");
    pkg.current("15698");
    pkg.aside("rustup/trust");
    let (row, line) = read(&pkg);
    assert_eq!(row.note, None, "{row:?}");
    assert_eq!(
        line,
        format!("cargo-build.noindex and tooling.noindex are .noindex; {QUIET_LINE}")
    );

    // Nothing plain: kept out, no note.
    std::fs::remove_dir_all(pkg.path().join("store/trust/15600")).unwrap();
    let (row, line) = read(&pkg);
    assert_eq!(row.note, None, "{row:?}");
    assert_eq!(
        line,
        format!(
            "cargo-build.noindex, tooling.noindex, and the Trust toolchain are .noindex; \
             {QUIET_LINE}"
        )
    );
}

/// Check again searches Spotlight anew ([`search_again`]): a search kept from minutes
/// ago, found or failed, would leave the row's note as it was.
#[test]
fn check_again_searches_spotlight_anew() {
    let (sys, found) = with_build_folders(&[TARGET]);
    sys.on(MDFIND, Ran::exit(1, "Spotlight is rebuilding its index"))
        .on(MDFIND, found);
    let item = BuildNoindex {
        inventory: SHARED.get_or_init(Arc::default).clone(),
        ..BuildNoindex::new(
            user("alice", 501),
            "/Users//alice".into(),
            "/Users//alice/Library/Application Support/aterm/pkg".into(),
            "/Users//alice/.cargo".into(),
            Projects::Noted,
        )
    };
    let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
    let read = || {
        let rows = engine::run(&items, Mode::Plan, &sys, &context(), &mut NoJournal);
        Reading::of(&rows[0])
    };
    let searches = || sys.calls().iter().filter(|c| c.as_str() == MDFIND).count();
    assert_eq!(read(), None, "the search failed");
    assert_eq!((read(), searches()), (None, 1), "and stands its age");
    search_again();
    assert_eq!(
        (read(), searches()),
        (
            Some(Reading::KeptOut {
                projects: 1,
                toolchain: false,
            }),
            2
        )
    );
}

/// A folder of root's that neither group nor others may write: `/` and `/Users` as macOS lays
/// them.
fn roots(mode: u32) -> Stat {
    Stat {
        uid: 0,
        gid: 0,
        mode,
        dir: true,
        symlink: false,
        len: 0,
    }
}

/// The console's device, owned by `uid` (the account logged in at it).
fn console_of(uid: u32) -> Stat {
    Stat {
        uid,
        gid: 0,
        mode: 0o600,
        dir: false,
        symlink: false,
        len: 0,
    }
}

/// Alice's home folder as `uid` owns it with `mode`.
fn home_folder(uid: u32, mode: u32) -> Stat {
    Stat {
        uid,
        gid: 20,
        mode,
        dir: true,
        symlink: false,
        len: 0,
    }
}

/// The Mac the root LAN broker serves alice (501) on, as root reads it: alice at the console,
/// her home under root's `/Users` as directory services name it, holding [`home`]'s folders,
/// and Spotlight indexing the data volume with nothing excluded yet.
fn broker_mac() -> Fake {
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&[]), Config::Writes);
    sys.stat_of("/", roots(0o755))
        .stat_of("/Users", roots(0o755))
        .stat_of(ALICE, home_folder(501, 0o750))
        .stat_of("/dev/console", console_of(501))
        .home_of_uid(501, ALICE);
    sys
}

/// The duty over `sys`, as `aterm lan serve` makes it.
fn duty_over(sys: &Arc<Fake>) -> Arc<dyn aterm_uds::lan::ScopeDuty> {
    broker_duty(Arc::clone(sys) as Arc<dyn System + Send + Sync>)
}

/// What [`home`]'s folders hold for the broker to exclude.
fn owed_paths(names: &[&str]) -> BTreeSet<PathBuf> {
    names
        .iter()
        .map(|name| PathBuf::from(format!("{ALICE}/{name}")))
        .collect()
}

/// Every call that edits Spotlight's configuration, restarts `mds` or writes a file.
fn edits(sys: &Fake) -> Vec<String> {
    sys.calls()
        .into_iter()
        .filter(|call| edits_one(call))
        .collect()
}

/// Whether `call` is one [`edits`] keeps.
fn edits_one(call: &str) -> bool {
    call.contains("plutil")
        || call.contains("killall")
        || call.starts_with("write ")
        || call.starts_with("rename ")
        || call.starts_with("remove ")
}

/// The broker plans for the console account alone and only over a home that account cannot
/// steer root away from: each row is refused before anything is read past the home, and edits
/// nothing. The account's own top-level folders the scope leaves out are planned, a link among
/// them never, and nothing of the rest of the scope ([`FULL`]: ~/Library's folders, the data
/// volume's top level, /Users//Shared), which is the password route's.
/// NEGATIVE CONTROLS: the console comparison, the sticky check, the ancestors' write-bit check
/// and the plain-path check each removed lets the row they guard plan; the home read for the
/// scope's whole `wanted` and the plan's top-level filter dropped plans ~/Library's folders and
/// the data volume's top level.
#[test]
fn the_broker_scopes_only_the_console_accounts_own_plain_home() {
    let refused = |why: &str, uid: u32, seed: &dyn Fn(&Fake)| {
        let sys = Arc::new(broker_mac());
        seed(&sys);
        let planned = duty_over(&sys).plan(uid);
        assert!(
            matches!(&planned, Err(ScopeAnswer::Refused(words)) if !words.contains('/')),
            "{why}: {planned:?}"
        );
        assert!(edits(&sys).is_empty(), "{why}: {:?}", sys.calls());
    };
    refused("root", 0, &|sys| {
        sys.stat_of("/dev/console", console_of(0))
            .home_of_uid(0, "/var/root");
    });
    // Bob's own home holds to every other rule, so only the console's owner keeps him out.
    refused("an account not at the console", 502, &|sys| {
        sys.stat_of("/Users//bob", home_folder(502, 0o750))
            .home_of_uid(502, "/Users//bob");
    });
    refused("no home from directory services", 503, &|sys| {
        sys.stat_of("/dev/console", console_of(503));
    });
    refused("a home that is a link", 501, &|sys| {
        sys.stat_of(
            ALICE,
            Stat {
                symlink: true,
                dir: false,
                ..home_folder(501, 0o755)
            },
        );
    });
    refused("a home another account owns", 501, &|sys| {
        sys.stat_of(ALICE, home_folder(502, 0o750));
    });
    refused("a home with the sticky bit", 501, &|sys| {
        sys.stat_of(ALICE, home_folder(501, 0o1750));
    });
    refused("a group-writable /Users", 501, &|sys| {
        sys.stat_of("/Users", roots(0o775));
    });
    refused("a /Users that is not root's", 501, &|sys| {
        sys.stat_of("/Users", home_folder(501, 0o755));
    });
    // Every folder its spelling names is in place, so only the spelling refuses it.
    refused("a home spelled through ..", 501, &|sys| {
        sys.stat_of("/Users/..", roots(0o755))
            .stat_of("/Users/../tmp", roots(0o755))
            .stat_of("/Users/../tmp/x", home_folder(501, 0o750))
            .home_of_uid(501, "/Users/../tmp/x");
    });
    refused("a doubled /", 501, &|sys| {
        sys.home_of_uid(501, "/Users//alice");
    });
    refused("a trailing /", 501, &|sys| {
        sys.home_of_uid(501, "/Users//alice/");
    });
    refused("a relative home", 501, &|sys| {
        sys.home_of_uid(501, "Users/alice");
    });

    let sys = Arc::new(broker_mac());
    let plan = duty_over(&sys).plan(501).unwrap();
    assert_eq!(
        plan,
        aterm_uds::lan::ScopePlan {
            uid: 501,
            folders: owed_paths(&["Developer", "Downloads", "My Projects"]),
        },
        "a link, a file, a dot-folder and a .noindex folder are never planned"
    );
    assert!(edits(&sys).is_empty(), "{:?}", sys.calls());
}

/// An apply excludes the plan's folders and adds them to root's receipt; a broker that finds
/// the receipt, or its record of its own lines, is not root's own regular file, or the receipt
/// past its bound, leaves Spotlight as it is, and the broker neither writes nor removes the
/// password route's record of what it failed to exclude.
/// NEGATIVE CONTROLS: a receipt or record read leniently (`unwrap_or_default`) plans over the
/// foreign file; the password route's record kept (`Route::Password`) removes it on success and
/// writes it on failure.
#[test]
fn the_broker_reads_the_receipt_strictly_and_never_touches_the_refusal() {
    let theirs = Stat {
        uid: 501,
        gid: 20,
        mode: 0o644,
        dir: false,
        symlink: false,
        len: 0,
    };
    for (why, seed) in [
        (
            "a receipt of the account's",
            &(|sys: &Fake| {
                sys.file(SCOPE_RECEIPT, b"").stat_of(SCOPE_RECEIPT, theirs);
            }) as &dyn Fn(&Fake),
        ),
        ("a receipt past its bound", &|sys: &Fake| {
            sys.file(SCOPE_RECEIPT, &vec![b'x'; spotlight_scope::RECEIPT_MAX + 1]);
        }),
        (
            "a record of the broker's lines of the account's",
            &|sys: &Fake| {
                sys.file(SCOPE_BROKERED, b"")
                    .stat_of(SCOPE_BROKERED, theirs);
            },
        ),
    ] {
        let sys = Arc::new(broker_mac());
        seed(&sys);
        assert!(
            matches!(duty_over(&sys).plan(501), Err(ScopeAnswer::Unchanged(_))),
            "{why}"
        );
        assert!(edits(&sys).is_empty(), "{why}: {:?}", sys.calls());
    }

    let sys = Arc::new(broker_mac());
    sys.file(SCOPE_REFUSED, b"/Users//alice/Developer\n");
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    assert_eq!(duty.apply(&plan), ScopeAnswer::Excluded(3));
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(AT_HOME));
    assert_eq!(count(&sys, KILL), 1);
    assert_eq!(
        sys.written(SCOPE_REFUSED).as_deref(),
        Some(&b"/Users//alice/Developer\n"[..]),
        "the broker leaves the password route's record as it is"
    );
    assert!(
        duty.plan(501).unwrap().folders.is_empty(),
        "nothing is owed"
    );

    let deaf = Arc::new(broker_mac());
    spotlight(&deaf, Some(&[]), Config::Deaf);
    let duty = duty_over(&deaf);
    let plan = duty.plan(501).unwrap();
    assert!(matches!(duty.apply(&plan), ScopeAnswer::Failed(words) if !words.contains('/')));
    assert!(deaf.written(SCOPE_REFUSED).is_none(), "{:?}", deaf.calls());
    assert!(deaf.written(SCOPE_RECEIPT).is_none());
}

/// A configuration `mds` rewrote under each of the broker's tries is busy, never a failure: the
/// plan is not remembered, so the client's next ask plans the same folders and edits again, and
/// no folder is written to the receipt or to the password route's record. One `mds` rewrote
/// once is edited again within the apply, and excluded.
/// NEGATIVE CONTROLS: the broker's `Changed` arm setting `failed` as `Failed` does answers
/// `Failed`, and the next plan is refused; an exclusion with one try answers the rewrite once
/// busy.
#[test]
fn a_configuration_mds_rewrote_under_the_brokers_edit_is_busy_and_tried_again() {
    let sys = Arc::new(broker_mac());
    spotlight(&sys, Some(&[]), Config::Rewritten);
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    assert_eq!(duty.apply(&plan), ScopeAnswer::Busy, "{:?}", sys.calls());
    assert!(sys.written(SCOPE_RECEIPT).is_none());
    assert!(sys.written(SCOPE_REFUSED).is_none());
    assert_eq!(count(&sys, KILL), 0, "mds was not restarted");
    let tried = sys.calls().len();
    assert_eq!(
        duty.plan(501).unwrap(),
        plan,
        "the same folders are planned again"
    );
    assert_eq!(duty.apply(&plan), ScopeAnswer::Busy);
    assert!(
        sys.calls()[tried..]
            .iter()
            .any(|call| call.contains("plutil")),
        "the next apply edits again: {:?}",
        &sys.calls()[tried..]
    );

    let once = Arc::new(broker_mac());
    spotlight(&once, Some(&[]), Config::RewrittenOnce);
    let duty = duty_over(&once);
    let plan = duty.plan(501).unwrap();
    assert_eq!(
        duty.apply(&plan),
        ScopeAnswer::Excluded(3),
        "{:?}",
        once.calls()
    );
    assert_eq!(count(&once, KILL), 1);
    assert_eq!(once.written(SCOPE_RECEIPT).as_deref(), Some(AT_HOME));
}

/// The same account's same folders whose apply failed are not tried again for a day: the next
/// plan of them answers that Spotlight stays as it is, and runs no edit. New folders are a new
/// plan; a day on, by the duty's clock, the same folders are planned again.
/// NEGATIVE CONTROLS: a broker that forgets the failure plans the same folders again; one that
/// remembers it with no age refuses them a day on.
#[test]
fn a_failed_plan_is_not_tried_again() {
    let sys = Arc::new(broker_mac());
    spotlight(&sys, Some(&[]), Config::Deaf);
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    assert!(matches!(duty.apply(&plan), ScopeAnswer::Failed(_)));
    let tried = sys.calls().len();
    assert!(
        matches!(duty.plan(501), Err(ScopeAnswer::Unchanged(words)) if words.contains("failed")),
        "{:?}",
        sys.calls()
    );
    assert!(
        sys.calls()[tried..]
            .iter()
            .all(|call| !call.contains("plutil") && !call.contains("killall")),
        "{:?}",
        &sys.calls()[tried..]
    );
    sys.advance(spotlight_scope::LEFT_KEPT.as_secs() - 1);
    assert!(matches!(duty.plan(501), Err(ScopeAnswer::Unchanged(_))));
    sys.advance(1);
    assert_eq!(
        duty.plan(501),
        Ok(plan),
        "a day on, the same folders are tried again"
    );
    made_on_volume(&sys, &format!("{ALICE}/Scratch"), 4000);
    assert_eq!(
        duty.plan(501).unwrap().folders,
        owed_paths(&["Developer", "Downloads", "My Projects", "Scratch"])
    );
}

/// A failed plan dated after the duty's clock (the clock set back since it failed) counts for
/// nothing: the same folders are planned again at once rather than kept out by a failure whose
/// age cannot be read.
/// NEGATIVE CONTROLS: the memory without its `*at <= now` guard overflows; one that reads the
/// failure's age as `now.saturating_sub(*at)` with no guard refuses the plan.
#[test]
fn a_failed_plan_dated_after_the_clock_is_tried_again() {
    let sys = Arc::new(broker_mac());
    spotlight(&sys, Some(&[]), Config::Deaf);
    let duty = duty_over(&sys);
    let plan = duty.plan(501).unwrap();
    assert!(matches!(duty.apply(&plan), ScopeAnswer::Failed(_)));
    assert!(matches!(duty.plan(501), Err(ScopeAnswer::Unchanged(_))));
    sys.set_back(1);
    assert_eq!(duty.plan(501), Ok(plan));
}

/// One request excludes at most [`spotlight_scope::SCOPE_BATCH`] folders, in path order, and the
/// next takes the rest; a home whose receipt lines are at [`spotlight_scope::HOME_LINES`], or
/// past it, gains none; a folder whose line would take the receipt past its bound is left out, so of a receipt
/// 40 bytes short of it only Developer's 23-byte line is planned, and of one 20 bytes short none
/// is; a volume Spotlight does not index plans nothing.
/// NEGATIVE CONTROLS: a plan with no batch bound takes all 70 at once; a line bound met only
/// at its count (`==`) plans Developer past it; a plan that leaves Spotlight as it is where any
/// owed folder would pass a bound plans nothing 40 bytes short.
#[test]
fn a_plan_is_bounded_by_its_batch_the_homes_lines_and_the_receipt() {
    let sys = Arc::new(broker_mac());
    for at in 0..67 {
        made_on_volume(&sys, &format!("{ALICE}/made-{at:02}"), 4000 + at);
    }
    let duty = duty_over(&sys);
    let first = duty.plan(501).unwrap();
    assert_eq!(first.folders.len(), spotlight_scope::SCOPE_BATCH);
    assert!(first.folders.contains(Path::new("/Users//alice/Developer")));
    assert_eq!(duty.apply(&first), ScopeAnswer::Excluded(64));
    let rest = duty.plan(501).unwrap();
    assert_eq!(rest.folders.len(), 6, "{:?}", rest.folders);
    assert!(rest.folders.is_disjoint(&first.folders));

    // Lines of alice's top level with names of one or two characters, so the home's byte share
    // has room for Developer's line and only the line bound stops it, at it and past it (lines
    // the password route wrote).
    const NAMES: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    for count in [spotlight_scope::HOME_LINES, spotlight_scope::HOME_LINES + 1] {
        let full = Arc::new(broker_mac());
        let lines: String = (0..count)
            .map(|at| {
                let name: String = if at < NAMES.len() {
                    char::from(NAMES[at]).into()
                } else {
                    let at = at - NAMES.len();
                    [NAMES[at / NAMES.len()], NAMES[at % NAMES.len()]]
                        .map(char::from)
                        .iter()
                        .collect()
                };
                format!("{ALICE}/{name}\n")
            })
            .collect();
        assert!(lines.len() + "/Users//alice/Developer\n".len() <= spotlight_scope::HOME_BYTES);
        full.file(SCOPE_RECEIPT, lines.as_bytes());
        assert!(
            matches!(
                duty_over(&full).plan(501),
                Err(ScopeAnswer::Unchanged(words)) if words.contains("full")
            ),
            "{count} lines"
        );
    }

    // A receipt `short` bytes short of its bound, of lines none of them alice's.
    let near = |short: usize| {
        let near = Arc::new(broker_mac());
        let mut lines = String::new();
        while lines.len() + 1100 < spotlight_scope::RECEIPT_MAX - short {
            lines.push_str(&format!(
                "/Volumes/{:04}-{}\n",
                lines.len(),
                "v".repeat(1000)
            ));
        }
        let room = spotlight_scope::RECEIPT_MAX - short - lines.len();
        lines.push_str(&format!("/{}\n", "p".repeat(room - 2)));
        assert_eq!(lines.len(), spotlight_scope::RECEIPT_MAX - short);
        near.file(SCOPE_RECEIPT, lines.as_bytes());
        near
    };
    assert_eq!(
        duty_over(&near(40)).plan(501).map(|plan| plan.folders),
        Ok(owed_paths(&["Developer"]))
    );
    assert!(matches!(
        duty_over(&near(20)).plan(501),
        Err(ScopeAnswer::Unchanged(words)) if words.contains("bound")
    ));

    let off = Arc::new(broker_mac());
    off.simulate(|_, cmd| {
        (cmd.program.to_str()? == MDUTIL)
            .then(|| Ran::ok("/System/Volumes/Data:\n\tIndexing disabled.\n"))
    });
    assert!(duty_over(&off).plan(501).unwrap().folders.is_empty());
}

/// A home whose listing passes [`spotlight_scope::LISTING_MAX`] names is left as it is: root
/// reads no listing without bound for an account.
/// NEGATIVE CONTROL: a listing read without its bound plans the home's folders.
#[test]
fn a_home_past_its_listing_bound_is_left_as_it_is() {
    let sys = Arc::new(broker_mac());
    for at in 0..spotlight_scope::LISTING_MAX {
        sys.file(&format!("{ALICE}/.dot-{at}"), b"");
    }
    assert!(matches!(
        duty_over(&sys).plan(501),
        Err(ScopeAnswer::Unchanged(words)) if words.contains("names")
    ));
}

/// The asker waits while a maker is at work and nothing is noted, asks for what was noted
/// (taking the note, so a folder noted during the ask is the next ask's), and ends only once
/// every maker has ended with nothing noted left, a folder noted as the last maker ended among
/// what it asks for.
/// NEGATIVE CONTROL: an asker that ends whenever nothing is noted ends while a maker works.
#[test]
fn the_asker_waits_while_a_maker_works_and_ends_once_none_does() {
    let mut made = Made {
        noted: false,
        makers: 1,
    };
    assert_eq!(next(&mut made), Next::Wait);
    made.noted = true;
    assert_eq!(next(&mut made), Next::Ask);
    assert!(!made.noted, "the ask took the note");
    assert_eq!(next(&mut made), Next::Wait);
    made.makers = 0;
    made.noted = true;
    assert_eq!(next(&mut made), Next::Ask);
    assert_eq!(next(&mut made), Next::End);
}

/// A folder published as a top-level folder of the console account's own home is noted for
/// the asker, which asks the broker once for every folder noted since its last ask began and
/// ends once every maker has; one published elsewhere, or by an account away from the console,
/// is noted for no ask.
/// NEGATIVE CONTROL: an asker that ends with its makers before asking leaves the noted folders
/// unasked.
#[test]
fn a_folder_made_in_the_console_home_is_asked_for_once_its_makers_end() {
    let account = || {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.logged_in(&user("alice", 501))
            .stat_of("/dev/console", console_of(501));
        sys
    };
    let asks = |sys: &Fake| sys.calls().iter().filter(|c| *c == "scope home").count();
    // Every folder is noted, and its maker ended, before the asker starts, so one ask is
    // all a correct asker makes.
    let asked_for = |sys: &Fake, made: &[&str]| {
        let folders = MadeFolders::new();
        let maker = folders.maker();
        for made in made {
            folders.made(sys, Path::new(made));
        }
        drop(maker);
        std::thread::scope(|scope| {
            scope.spawn(|| folders.ask_while_made(sys));
        });
        asks(sys)
    };
    let sys = account();
    assert_eq!(
        asked_for(&sys, &["/Users//alice/new", "/Users//alice/other"]),
        1,
        "{:?}",
        sys.calls()
    );
    let deeper = account();
    assert_eq!(asked_for(&deeper, &["/Users//alice/Developer/new"]), 0);
    let away = account();
    away.stat_of("/dev/console", console_of(502));
    assert_eq!(asked_for(&away, &["/Users//alice/new"]), 0);
}

/// The password route never takes root's receipt past its bound: where the folders it would
/// add pass it, it fails before Spotlight is edited, leaving the configuration and the receipt
/// as they were; and root's own receipt past its bound is no empty one, so the root step fails
/// rather than writes a receipt of this home alone over the other homes' lines.
/// NEGATIVE CONTROLS: an exclusion that writes the receipt whatever its size edits Spotlight and
/// writes a receipt no reader reads; root reading its own over-bound receipt as none rewrites
/// it with this home's folders alone.
#[test]
fn the_password_route_keeps_the_receipt_within_its_bound() {
    let near = Fake::root();
    home(&near);
    spotlight(&near, Some(&[]), Config::Writes);
    let mut lines = String::new();
    while lines.len() + 1100 < spotlight_scope::RECEIPT_MAX - 40 {
        lines.push_str(&format!(
            "/Volumes/{:04}-{}\n",
            lines.len(),
            "v".repeat(1000)
        ));
    }
    let room = spotlight_scope::RECEIPT_MAX - 40 - lines.len();
    lines.push_str(&format!("/{}\n", "p".repeat(room - 2)));
    near.file(SCOPE_RECEIPT, lines.as_bytes());
    let rows = scope_rows(&near, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed, "{rows:?}");
    assert!(
        rows[0].why.as_deref().unwrap_or("").contains("bound"),
        "{rows:?}"
    );
    assert_eq!(
        config_list(&near).as_deref(),
        Some(&[][..]),
        "{:?}",
        near.calls()
    );
    assert_eq!(
        near.written(SCOPE_RECEIPT).as_deref(),
        Some(lines.as_bytes())
    );
    assert_eq!(count(&near, KILL), 0);

    let over = Fake::root();
    home(&over);
    spotlight(&over, Some(&[]), Config::Writes);
    let past = vec![b'/'; spotlight_scope::RECEIPT_MAX + 1];
    over.file(SCOPE_RECEIPT, &past);
    let rows = scope_rows(&over, Mode::Apply);
    assert!(rows[0].outcome.is_failure(), "{rows:?}");
    assert_eq!(over.written(SCOPE_RECEIPT).as_deref(), Some(&past[..]));
    assert_eq!(config_list(&over).as_deref(), Some(&[][..]));
}

/// The broker's reading of a home runs on a thread of its own that a request waits
/// [`READ_DEADLINE`] for: a read the home's filesystem holds (a mount made after the table was
/// read) answers busy within the deadline, which the client asks again on the clock, and while
/// that reading still runs no other starts, so the next request answers busy at once; once it
/// ends, a request reads the home again.
/// NEGATIVE CONTROL: a request that waits for its reading with no deadline answers only once the
/// filesystem does, past the test's minute.
#[test]
fn a_home_reading_the_filesystem_holds_ends_at_its_deadline() {
    let sys = Arc::new(broker_mac());
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = Mutex::new(held);
    let blocked = Arc::new(std::sync::atomic::AtomicBool::new(true));
    {
        let blocked = Arc::clone(&blocked);
        sys.on_stat_read(move |_, path| {
            if path == Path::new("/Users//alice/Developer")
                && blocked.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                // Held until the test releases it, or for the hang detector's minute.
                let _ = held
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(60));
            }
        });
    }
    let duty = broker_duty_within(
        Arc::clone(&sys) as Arc<dyn System + Send + Sync>,
        std::time::Duration::from_millis(200),
    );
    let began = std::time::Instant::now();
    assert_eq!(
        duty.plan(501),
        Err(ScopeAnswer::Busy),
        "a held reading ends at its deadline"
    );
    assert!(began.elapsed() < std::time::Duration::from_secs(30));
    assert_eq!(
        duty.plan(501),
        Err(ScopeAnswer::Busy),
        "no second reading starts beside the held one"
    );
    assert!(edits(&sys).is_empty(), "{:?}", sys.calls());
    release.send(()).unwrap();
    // A hang detector: the released reading must end, and the next request read the home.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let planned = loop {
        match duty.plan(501) {
            Err(ScopeAnswer::Busy) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            planned => break planned,
        }
    };
    assert_eq!(
        planned.map(|plan| plan.folders),
        Ok(owed_paths(&["Developer", "Downloads", "My Projects"]))
    );
}

/// `sync.spotlight`'s ask keeps the folders the broker left as they are, or failed to exclude,
/// in this account's state folder with the time, so its readings for a day ask nothing for them
/// (not applicable), and the automatic ask owes the password the home they leave never scoped
/// whatever the record's age; a day on, the broker is asked for them again, as it is where the
/// record's time is after the clock's. An ask that excluded every folder owed removes the
/// record. A transient answer keeps nothing.
/// NEGATIVE CONTROLS: an ask that keeps no record leaves the next reading asking the broker for
/// the same folders; a record honoured at any age asks nothing a day on, or with the clock set
/// back.
#[test]
fn the_folders_the_broker_left_are_kept_for_the_next_reading() {
    let state = "/Users//alice/Library/Application Support/aterm/state";
    let account = |answer: ScopeAnswer| {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.account_state = Some(PathBuf::from(state));
        home(&sys);
        spotlight(&sys, Some(&[]), Config::Writes);
        sys.logged_in(&user("alice", 501))
            .stat_of("/", roots(0o755))
            .stat_of("/Users", roots(0o755))
            .stat_of(ALICE, home_folder(501, 0o750))
            .stat_of("/dev/console", console_of(501))
            .on_scope_home(move |_| answer.clone());
        sys
    };
    let record = PathBuf::from(state).join(spotlight_scope::BROKER_LEFT);
    let record = record.to_string_lossy();
    let day = spotlight_scope::LEFT_KEPT.as_secs();
    let left = |pass: &spotlight_scope::Pass| {
        matches!(
            pass,
            spotlight_scope::Pass::Left(_) | spotlight_scope::Pass::Failed(_)
        )
    };
    for answer in [
        ScopeAnswer::Unchanged("its last edit of these folders failed".into()),
        ScopeAnswer::Failed("TCC denied the edit".into()),
    ] {
        let sys = account(answer.clone());
        let pass = exclude_by_broker(&sys);
        assert!(left(&pass), "{answer:?}: {pass:?}");
        let text = format!("at {}\n{}", sys.now, String::from_utf8_lossy(AT_HOME));
        assert_eq!(
            sys.written(&record).as_deref(),
            Some(text.as_bytes()),
            "{answer:?}"
        );
        assert_eq!(
            spotlight_scope::broker_would_take(&Port(&sys)),
            Some(Err(Leave::Stands(spotlight_scope::LEFT.to_owned()))),
            "{answer:?}"
        );
        let asks = count(&sys, "scope home");
        assert_eq!(exclude_by_broker(&sys), spotlight_scope::Pass::Nothing);
        sys.advance(day - 1);
        assert_eq!(exclude_by_broker(&sys), spotlight_scope::Pass::Nothing);
        assert_eq!(
            count(&sys, "scope home"),
            asks,
            "{answer:?}: the left folders ask nothing for a day"
        );
        sys.advance(1);
        assert_eq!(
            spotlight_scope::broker_would_take(&Port(&sys)),
            Some(Err(Leave::Stands(spotlight_scope::LEFT.to_owned()))),
            "{answer:?}: the automatic ask reads them left still"
        );
        assert!(left(&exclude_by_broker(&sys)), "{answer:?}");
        assert_eq!(
            count(&sys, "scope home"),
            asks + 1,
            "{answer:?}: a day on, the broker is asked again"
        );
    }
    let ahead = account(ScopeAnswer::Unchanged("still full".into()));
    let owed = owed_paths(&["Developer", "Downloads", "My Projects"]);
    let text = spotlight_scope::broker_left_text(&owed, ahead.now + 60);
    ahead.file(&record, text.as_bytes());
    assert!(left(&exclude_by_broker(&ahead)));
    assert_eq!(
        count(&ahead, "scope home"),
        1,
        "a record dated after the clock's time is asked again"
    );

    let busy = account(ScopeAnswer::Busy);
    assert_eq!(exclude_by_broker(&busy), spotlight_scope::Pass::Busy);
    assert!(busy.written(&record).is_none());

    let done = account(ScopeAnswer::Excluded(3));
    done.file(&record, b"/Users//alice/Gone\n");
    done.on_scope_home(|sys| {
        sys.file(SCOPE_RECEIPT, FULL);
        ScopeAnswer::Excluded(3)
    });
    assert_eq!(exclude_by_broker(&done), spotlight_scope::Pass::Done(3));
    assert!(done.written(&record).is_none(), "{:?}", done.calls());
}

/// The person's keep-out of `folder`, as `--only spotlight-keep-out --keep-out <folder>` runs it.
fn keep_out_rows(sys: &Fake, folder: &str) -> Vec<crate::model::ItemReport> {
    let items = person_items(
        aterm_types::macos_setup::root_only::Step::SpotlightKeepOut,
        Some(Path::new(folder)),
    )
    .expect("a keep-out's items");
    engine::run(&items, Mode::Apply, sys, &at_console(), &mut NoJournal)
}

/// A FOLDER KEPT OUT FROM THE PAGE IS THE PERSON'S AND THE SCOPE STEP NEVER TAKES IT BACK
/// (SD-96): Keep a folder out… of ~/Documents/Archive adds it to the data volume's excluded
/// folders by the scope step's own checked edit (one `mds` restart) and records it in
/// `spotlight-scope.yours`, never on setup's receipt; setup's scope step then applies its own
/// folders and leaves the person's entry and record as they are; the same keep-out again
/// changes nothing; and a keep-out never reads the volume's indexing, so one pressed while the
/// person's pause holds is kept out all the same.
/// CONTROL: a folder the rule refuses (~/Library, ~/Documents itself, the home, ~/code/x, a path
/// on another volume, one with `..`) changes nothing: no configuration edit, no record, no
/// restart.
#[test]
fn a_folder_kept_out_from_the_page_is_the_persons_and_the_scope_step_never_takes_it_back() {
    const ARCHIVE: &str = "/Users//alice/Documents/Archive";
    const YOURS_WRITE: &str = "write /Library/Application Support/aterm/spotlight-scope.yours 644";
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, Some(&[]), Config::Writes);
    let rows = keep_out_rows(&sys, ARCHIVE);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(config_list(&sys).unwrap(), [ARCHIVE]);
    assert_eq!(
        sys.written(spotlight_scope::YOURS).as_deref(),
        Some(format!("{ARCHIVE}\n").as_bytes())
    );
    assert!(sys.written(SCOPE_RECEIPT).is_none(), "not setup's receipt");
    assert_eq!(count(&sys, KILL), 1);
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.starts_with("/usr/bin/mdutil")),
        "a keep-out reads no indexing: a pause does not hold it back"
    );

    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert!(
        config_list(&sys)
            .unwrap()
            .iter()
            .any(|path| path == ARCHIVE),
        "setup's scope keeps the person's entry"
    );
    assert_eq!(
        sys.written(spotlight_scope::YOURS).as_deref(),
        Some(format!("{ARCHIVE}\n").as_bytes()),
        "and their record"
    );
    let receipt = String::from_utf8(sys.written(SCOPE_RECEIPT).expect("the receipt")).unwrap();
    assert!(!receipt.contains(ARCHIVE), "{receipt}");

    let rows = keep_out_rows(&sys, ARCHIVE);
    assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
    assert_eq!(count(&sys, KILL), 2, "only the scope's own restart since");
    assert_eq!(count(&sys, YOURS_WRITE), 1);

    for refused in [
        "/Users//alice/Library",
        "/Users//alice/Documents",
        ALICE,
        "/Users//alice/code/x",
        "/Volumes/X/a",
        "/Users//alice/Documents/../x",
    ] {
        let sys = Fake::root();
        home(&sys);
        spotlight(&sys, Some(&[]), Config::Writes);
        let rows = keep_out_rows(&sys, refused);
        assert_ne!(
            rows[0].outcome,
            Outcome::Changed,
            "CONTROL {refused}: {rows:?}"
        );
        assert_eq!(
            config_list(&sys).unwrap(),
            Vec::<String>::new(),
            "CONTROL {refused}"
        );
        assert!(
            sys.written(spotlight_scope::YOURS).is_none(),
            "CONTROL {refused}"
        );
        assert_eq!(count(&sys, KILL), 0, "CONTROL {refused}");
    }
}

/// The person's Pause, Resume and Rebuild run Spotlight's own tool on the data volume as root,
/// read back by `mdutil -s`: Pause turns indexing off (`-i off`), Resume on (`-i on`), Rebuild
/// erases the index (`-E`) as a journaled refresh of indexing that reads on before and after.
/// CONTROL: a pause of a volume already off runs no `mdutil -i`, and a rebuild of a volume whose
/// indexing is off runs no `-E` and does not finish.
#[test]
fn the_persons_spotlight_steps_run_mdutil_on_the_data_volume() {
    use aterm_types::macos_setup::root_only::Step;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let mac = |on: bool| {
        let sys = Fake::root();
        let state = Arc::new(AtomicBool::new(on));
        sys.simulate(move |_, cmd| {
            let args: Vec<String> = cmd
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match (cmd.program.to_str()?, args.as_slice()) {
                (MDUTIL, ["-s", DATA_VOLUME]) => Some(Ran::ok(if state.load(Ordering::SeqCst) {
                    "/System/Volumes/Data:\n\tIndexing enabled.\n"
                } else {
                    "/System/Volumes/Data:\n\tIndexing disabled.\n"
                })),
                (MDUTIL, ["-i", word, DATA_VOLUME]) => {
                    state.store(*word == "on", Ordering::SeqCst);
                    Some(Ran::ok(""))
                }
                (MDUTIL, ["-E", DATA_VOLUME]) => Some(Ran::ok("")),
                _ => None,
            }
        });
        sys
    };
    let run = |sys: &Fake, step: Step| {
        let items = person_items(step, None).expect("the step's items");
        engine::run(&items, Mode::Apply, sys, &at_console(), &mut NoJournal)
    };
    for (on, step, call, outcome) in [
        (
            true,
            Step::SpotlightPause,
            "/usr/bin/mdutil -i off /System/Volumes/Data",
            Outcome::Changed,
        ),
        (
            false,
            Step::SpotlightResume,
            "/usr/bin/mdutil -i on /System/Volumes/Data",
            Outcome::Changed,
        ),
        (
            true,
            Step::SpotlightRebuild,
            "/usr/bin/mdutil -E /System/Volumes/Data",
            Outcome::Ok,
        ),
    ] {
        let sys = mac(on);
        let rows = run(&sys, step);
        assert_eq!(rows[0].outcome, outcome, "{step:?}: {rows:?}");
        assert_eq!(count(&sys, call), 1, "{step:?}: {:?}", sys.calls());
    }
    let off = mac(false);
    let rows = run(&off, Step::SpotlightPause);
    assert_eq!(rows[0].outcome, Outcome::Ok, "CONTROL: {rows:?}");
    assert!(
        !off.calls().iter().any(|call| call.contains("mdutil -i")),
        "CONTROL"
    );
    let rows = run(&off, Step::SpotlightRebuild);
    assert!(
        !matches!(rows[0].outcome, Outcome::Ok | Outcome::Changed),
        "CONTROL: {rows:?}"
    );
    assert!(
        !off.calls().iter().any(|call| call.contains("mdutil -E")),
        "CONTROL"
    );
}
