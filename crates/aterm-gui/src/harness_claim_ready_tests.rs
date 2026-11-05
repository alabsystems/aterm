// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for a credit claim's readiness ([`super::look`]): on a made-up machine (a [`Fake`]
//! probe), Chrome found, installed once or held where a person removed it, the extension found
//! turned on, found turned off or listed, and what is left in a person's words.
//! [`super::put_new`]'s own test writes to a scratch folder.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::*;

/// A made-up machine: its folders, its files, and what atpkg's brew says and is asked.
#[derive(Default)]
struct Fake {
    dirs: RefCell<BTreeSet<PathBuf>>,
    files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    held: Option<String>,
    unwritable: bool,
    installs: Cell<u32>,
    writes: Cell<u32>,
}

impl Fake {
    /// A folder at `path`, and each folder it sits in, as a real disk has them.
    fn dir(&self, path: &Path) {
        self.dirs
            .borrow_mut()
            .extend(path.ancestors().map(Path::to_path_buf));
    }

    fn file(&self, path: &Path, bytes: &[u8]) {
        self.files
            .borrow_mut()
            .insert(path.to_path_buf(), bytes.to_vec());
    }
}

impl Probe for Fake {
    fn is_dir(&self, path: &Path) -> bool {
        self.dirs.borrow().contains(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.files.borrow().contains_key(path)
    }

    fn names(&self, dir: &Path) -> Vec<OsString> {
        self.dirs
            .borrow()
            .iter()
            .filter(|path| path.parent() == Some(dir))
            .filter_map(|path| path.file_name().map(OsString::from))
            .collect()
    }

    fn read(&self, path: &Path, max: usize) -> Result<Option<Vec<u8>>, String> {
        match self.files.borrow().get(path) {
            Some(bytes) if bytes.len() > max => Err(format!("it is larger than {max} bytes")),
            found => Ok(found.cloned()),
        }
    }

    fn put_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, String> {
        if self.unwritable {
            return Err("read-only".to_string());
        }
        self.writes.set(self.writes.get() + 1);
        let mut files = self.files.borrow_mut();
        if files.contains_key(path) {
            return Ok(false);
        }
        files.insert(path.to_path_buf(), bytes.to_vec());
        Ok(true)
    }

    fn held(&self) -> Option<String> {
        self.held.clone()
    }

    fn install(&self) {
        self.installs.set(self.installs.get() + 1);
    }
}

fn places() -> Places {
    Places::mac(
        Path::new("/Users//me"),
        PathBuf::from("/Users//me/Library/Application Support/aterm/pkg/abrew/Applications"),
        Some(Path::new("/Users//me/.local/state/aterm")),
    )
}

/// The extension's folder in Chrome profile `profile`.
fn extension_in(places: &Places, profile: &str) -> PathBuf {
    places
        .chrome
        .join(profile)
        .join("Extensions")
        .join(EXTENSION_ID)
}

/// Settings file `name` of Chrome profile `profile`, naming the extension's settings `entry`.
fn settings_in(fake: &Fake, places: &Places, profile: &str, name: &str, entry: &str) {
    fake.file(
        &places.chrome.join(profile).join(name),
        format!(
            "{{\"extensions\":{{\"settings\":{{\"{EXTENSION_ID}\":{entry},             \"someotherextensionidxxxxxxxxxxx\":{{}}}}}}}}"
        )
        .as_bytes(),
    );
}

/// The extension added to Chrome profile `profile` and turned on there, as a person's Chrome
/// keeps it once they turned it on.
fn added_on(fake: &Fake, places: &Places, profile: &str) {
    fake.dir(&extension_in(places, profile));
    settings_in(fake, places, profile, PREFERENCES[0], r#"{"state":1}"#);
}

/// EVERYTHING IN PLACE IS READY, AND TOUCHES NOTHING: Chrome in any of its three folders and the
/// extension in any profile. CONTROL: each piece missing alone is not ready, and names that piece.
#[test]
fn chrome_and_its_extension_in_place_are_ready_and_nothing_is_written() {
    let at = places();
    for (n, folder) in at.apps.iter().enumerate() {
        let fake = Fake::default();
        fake.dir(&folder.join(CHROME_APP));
        let profile = if n == 0 { "Default" } else { "Profile 2" };
        added_on(&fake, &at, profile);
        let found = look(&at, &fake, &Installs::new(), Instant::now());
        assert_eq!(
            found.chrome,
            Chrome::At(folder.join(CHROME_APP)),
            "{folder:?}"
        );
        assert_eq!(found.extension, Extension::Added);
        assert!(found.ready());
        assert_eq!(found.left(), None);
        assert_eq!(fake.installs.get(), 0, "nothing installed");
        assert_eq!(fake.writes.get(), 0, "nothing written");
    }

    let no_chrome = Fake::default();
    added_on(&no_chrome, &at, "Default");
    let found = look(&at, &no_chrome, &Installs::new(), Instant::now());
    assert!(!found.ready(), "CONTROL: no Chrome");
    assert!(found.left().expect("waits").contains("Google Chrome"));

    let no_extension = Fake::default();
    no_extension.dir(&at.apps[0].join(CHROME_APP));
    no_extension.dir(
        &at.chrome
            .join("Default/Extensions/someotherextensionidxxxxxxxxxxx"),
    );
    let found = look(&at, &no_extension, &Installs::new(), Instant::now());
    assert!(!found.ready(), "CONTROL: another extension is not Claude's");
    assert_eq!(found.extension, Extension::Listed);
}

/// A MISSING CHROME IS INSTALLED ONCE, IN THE BACKGROUND: the first look starts atpkg's brew and
/// later looks wait on it; a failed install is named and not started again before
/// [`INSTALL_AGAIN`]; one that finished but left no app reads as failed too. CONTROL: an hour on,
/// a second install starts.
#[test]
fn a_missing_chrome_is_installed_once_and_tried_again_an_hour_after_a_failure() {
    let at = places();
    let fake = Fake::default();
    let installs = Installs::new();
    let t0 = Instant::now();
    assert_eq!(look(&at, &fake, &installs, t0).chrome, Chrome::Installing);
    assert_eq!(fake.installs.get(), 1);
    assert_eq!(look(&at, &fake, &installs, t0).chrome, Chrome::Installing);
    assert_eq!(fake.installs.get(), 1, "one install at a time");

    installs.ended(Err("the download failed".to_string()), t0);
    let failed = look(&at, &fake, &installs, t0 + INSTALL_AGAIN / 2);
    assert_eq!(
        failed.chrome,
        Chrome::Failed("the download failed".to_string())
    );
    assert!(
        failed
            .left()
            .expect("waits")
            .contains("the download failed"),
        "{:?}",
        failed.left()
    );
    assert_eq!(fake.installs.get(), 1, "not again within the hour");
    assert_eq!(
        look(&at, &fake, &installs, t0 + INSTALL_AGAIN).chrome,
        Chrome::Installing,
        "CONTROL: an hour on, again"
    );
    assert_eq!(fake.installs.get(), 2);

    installs.ended(Ok(()), t0 + INSTALL_AGAIN);
    assert!(
        matches!(
            look(&at, &fake, &installs, t0 + INSTALL_AGAIN).chrome,
            Chrome::Failed(why) if why.contains("no Google Chrome.app")
        ),
        "a clean exit that left no app is no install"
    );
    fake.dir(&at.apps[2].join(CHROME_APP));
    assert_eq!(
        look(&at, &fake, &installs, t0 + INSTALL_AGAIN).chrome,
        Chrome::At(at.apps[2].join(CHROME_APP)),
        "the app in atpkg's brew's folder is found"
    );
    assert_eq!(fake.installs.get(), 2);
}

/// A PERSON'S NO TO THE INSTALL STANDS: where atpkg's brew is held (an exclude, a removal, no
/// unattended installs, brew off), nothing is installed and the words say why. CONTROL: the same
/// machine unheld starts one.
#[test]
fn a_held_install_installs_nothing_and_says_why() {
    let at = places();
    let held = Fake {
        held: Some("`[packages] exclude` in aterm.toml names it".to_string()),
        ..Fake::default()
    };
    let found = look(&at, &held, &Installs::new(), Instant::now());
    assert!(matches!(&found.chrome, Chrome::Held(why) if why.contains("exclude")));
    assert_eq!(held.installs.get(), 0);
    let left = found.left().expect("waits");
    assert!(left.contains("installs none of here"), "{left}");
    assert!(left.contains("`[packages] exclude`"), "{left}");

    let free = Fake::default();
    look(&at, &free, &Installs::new(), Instant::now());
    assert_eq!(free.installs.get(), 1, "CONTROL: unheld, it installs");
}

/// AN EXTENSION CHROME ADDED TURNED OFF IS NOT READY, SO NO TRY IS SPENT ON IT: Chrome adds the
/// extension its external list names turned off until a person turns it on, and a profile whose
/// settings keep it off (a newer Chrome's `disable_reasons` list, an older one's number and
/// `state: 0`), or name it nowhere, leaves the claim waiting, its words asking for it to be turned
/// on and nothing listed again. A profile's settings that do not read are named. CONTROL: the
/// extension on in a second profile, on in `Preferences` where `Secure Preferences` names it not,
/// or on with an empty reason list, is ready; `Secure Preferences` is read first.
#[test]
fn an_extension_chrome_added_turned_off_is_not_ready_and_asks_for_it_to_be_turned_on() {
    let at = places();
    let chrome = |fake: &Fake| fake.dir(&at.apps[0].join(CHROME_APP));
    for off in [
        r#"{"disable_reasons":[8192]}"#,
        r#"{"disable_reasons":8192,"state":0}"#,
        r#"{"state":0}"#,
    ] {
        let fake = Fake::default();
        chrome(&fake);
        fake.dir(&extension_in(&at, "Default"));
        settings_in(&fake, &at, "Default", PREFERENCES[0], off);
        let found = look(&at, &fake, &Installs::new(), Instant::now());
        assert_eq!(found.extension, Extension::Off(None), "{off}");
        assert!(!found.ready(), "{off}");
        assert_eq!(
            found.left().as_deref(),
            Some(
                "In place: Google Chrome; the Claude extension, added to Chrome.\n\
                 Left: turn on the Claude extension in Chrome (chrome://extensions); sign in to \
                 claude.ai in Chrome."
            ),
            "{off}"
        );
        assert_eq!(fake.writes.get(), 0, "nothing listed again: {off}");
    }

    let unnamed = Fake::default();
    chrome(&unnamed);
    unnamed.dir(&extension_in(&at, "Default"));
    assert_eq!(
        look(&at, &unnamed, &Installs::new(), Instant::now()).extension,
        Extension::Off(None),
        "a folder its settings never name is not on"
    );

    let unread = Fake::default();
    chrome(&unread);
    unread.dir(&extension_in(&at, "Default"));
    unread.file(
        &at.chrome.join("Default").join(PREFERENCES[0]),
        b"{not json",
    );
    let found = look(&at, &unread, &Installs::new(), Instant::now());
    assert!(
        matches!(&found.extension, Extension::Off(Some(why)) if why.starts_with("Secure Preferences is not JSON")),
        "{:?}",
        found.extension
    );
    assert!(
        found
            .left()
            .expect("waits")
            .contains("could not read whether it is on"),
        "{:?}",
        found.left()
    );

    let second = Fake::default();
    chrome(&second);
    second.dir(&extension_in(&at, "Default"));
    settings_in(
        &second,
        &at,
        "Default",
        PREFERENCES[0],
        r#"{"disable_reasons":[8192]}"#,
    );
    added_on(&second, &at, "Profile 2");
    assert_eq!(
        look(&at, &second, &Installs::new(), Instant::now()).extension,
        Extension::Added,
        "CONTROL: on in another profile"
    );

    let older = Fake::default();
    chrome(&older);
    older.dir(&extension_in(&at, "Default"));
    older.file(
        &at.chrome.join("Default").join(PREFERENCES[0]),
        br#"{"extensions":{}}"#,
    );
    settings_in(
        &older,
        &at,
        "Default",
        PREFERENCES[1],
        r#"{"disable_reasons":[]}"#,
    );
    let found = look(&at, &older, &Installs::new(), Instant::now());
    assert_eq!(
        found.extension,
        Extension::Added,
        "CONTROL: on in Preferences"
    );
    assert!(found.ready());

    let first = Fake::default();
    chrome(&first);
    first.dir(&extension_in(&at, "Default"));
    settings_in(
        &first,
        &at,
        "Default",
        PREFERENCES[0],
        r#"{"disable_reasons":[8192]}"#,
    );
    settings_in(&first, &at, "Default", PREFERENCES[1], r#"{"state":1}"#);
    assert_eq!(
        look(&at, &first, &Installs::new(), Instant::now()).extension,
        Extension::Off(None),
        "Secure Preferences, read first, keeps it off"
    );
}

/// A CHROME A PERSON REMOVED IS NOT INSTALLED AGAIN, however they removed it: no app in any
/// folder, yet Chrome ran here (its `Local State`) or aterm's install once put one in place (its
/// mark), holds the install, and the words say why. CONTROL: a machine where neither is found
/// installs, and a Chrome in place with its `Local State` is simply found.
#[test]
fn a_chrome_a_person_removed_is_not_installed_again() {
    let at = places();
    let mark = at.installed.clone().expect("a state folder");
    for trace in [at.chrome.join(LOCAL_STATE), mark] {
        let fake = Fake::default();
        fake.file(&trace, b"{}");
        let found = look(&at, &fake, &Installs::new(), Instant::now());
        assert!(
            matches!(&found.chrome, Chrome::Held(why) if why.contains("is gone")),
            "{trace:?}: {:?}",
            found.chrome
        );
        assert_eq!(fake.installs.get(), 0, "{trace:?}");
        let left = found.left().expect("waits");
        assert!(left.contains("installs none of here"), "{left}");
    }

    let never = Fake::default();
    assert_eq!(
        look(&at, &never, &Installs::new(), Instant::now()).chrome,
        Chrome::Installing,
        "CONTROL: never here, it installs"
    );
    assert_eq!(never.installs.get(), 1);

    let kept = Fake::default();
    kept.file(&at.chrome.join(LOCAL_STATE), b"{}");
    kept.dir(&at.apps[1].join(CHROME_APP));
    assert_eq!(
        look(&at, &kept, &Installs::new(), Instant::now()).chrome,
        Chrome::At(at.apps[1].join(CHROME_APP)),
        "CONTROL: in place, found"
    );
}

/// THE EXTENSION IS LISTED FOR CHROME ONCE, AND NOTHING IS EVER REPLACED: a missing listing is
/// written as [`listing_text`], an equivalent one someone wrote (laid out otherwise) counts, and a
/// different file stays as it is and is named. CONTROL: an unwritable folder lists nothing.
#[test]
fn the_extension_is_listed_once_and_a_different_file_is_left_alone() {
    let at = places();
    let fake = Fake::default();
    fake.dir(&at.apps[0].join(CHROME_APP));
    let found = look(&at, &fake, &Installs::new(), Instant::now());
    assert_eq!(found.extension, Extension::Listed);
    assert_eq!(
        fake.files.borrow().get(&at.listing()).map(Vec::as_slice),
        Some(listing_text().as_bytes())
    );
    assert_eq!(fake.writes.get(), 1);
    assert_eq!(
        look(&at, &fake, &Installs::new(), Instant::now()).extension,
        Extension::Listed
    );
    assert_eq!(fake.writes.get(), 1, "read back, not written again");
    let left = found.left().expect("waits for the person");
    assert_eq!(
        left,
        "In place: Google Chrome; the Claude extension, listed for Chrome to add.\n\
         Left: open Chrome and enable the Claude extension it adds; sign in to claude.ai in \
         Chrome."
    );

    let someone = Fake::default();
    someone.file(
        &at.listing(),
        format!("{{\"external_update_url\":\"{UPDATE_URL}\"}}").as_bytes(),
    );
    assert_eq!(
        look(&at, &someone, &Installs::new(), Instant::now()).extension,
        Extension::Listed,
        "the same link, laid out otherwise"
    );
    assert_eq!(someone.writes.get(), 0);

    let other = br#"{"external_crx": "/somewhere/else.crx", "external_version": "1.0"}"#;
    let different = Fake::default();
    different.file(&at.listing(), other);
    let found = look(&at, &different, &Installs::new(), Instant::now());
    assert!(
        matches!(&found.extension, Extension::Unlisted(why) if why.contains("leaves as it is")),
        "{:?}",
        found.extension
    );
    assert_eq!(different.writes.get(), 0, "never written over");
    assert_eq!(
        different
            .files
            .borrow()
            .get(&at.listing())
            .map(Vec::as_slice),
        Some(&other[..])
    );
    assert!(
        found
            .left()
            .expect("waits")
            .contains("https://claude.ai/chrome")
    );

    let unwritable = Fake {
        unwritable: true,
        ..Fake::default()
    };
    assert!(
        matches!(
            look(&at, &unwritable, &Installs::new(), Instant::now()).extension,
            Extension::Unlisted(why) if why.contains("could not be written")
        ),
        "CONTROL"
    );
}

/// THE STAGED WRITE NEVER REPLACES A FILE: [`put_new`] lays a new file whole and owner-only, says
/// `false` where a file already stands and leaves it as it was, and leaves no staged file behind.
#[cfg(unix)]
#[test]
fn put_new_places_a_new_file_whole_and_never_one_over_another() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("External Extensions").join("x.json");
    assert_eq!(put_new(&path, b"first"), Ok(true));
    assert_eq!(std::fs::read(&path).expect("read"), b"first");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(put_new(&path, b"second"), Ok(false), "a file stands there");
    assert_eq!(std::fs::read(&path).expect("read"), b"first", "unchanged");
    let names: Vec<_> = std::fs::read_dir(path.parent().expect("folder"))
        .expect("listing")
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert_eq!(names, [OsString::from("x.json")], "no staged file left");
}
