// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::items::defaults::tests::{ctx, export, user};
use crate::model::{Mode, Outcome};
use crate::port::{Fake, Ran, Stat};

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
            stdout: b"Indexing disabled.".to_vec(),
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

/// Alice's home as its top level lists: the folders Spotlight keeps, three it should leave
/// out (one with a space in its name), and what is never a candidate (a dot-folder, a
/// `.noindex` folder, a file, a link).
pub(crate) fn home(sys: &Fake) {
    for name in [
        "Documents",
        "Desktop",
        "Library",
        "Applications",
        "Developer",
        "Downloads",
        "My Projects",
        ".cargo",
        "build.noindex",
    ] {
        let path = format!("{ALICE}/{name}");
        sys.file(&path, b"").stat_of(&path, folder());
    }
    sys.file(&format!("{ALICE}/notes.txt"), b"x")
        .link(&format!("{ALICE}/Linked"), "/Volumes/Work");
}

/// How the fake `plutil` treats an insert.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Config {
    /// It adds the path.
    Writes,
    /// It exits 0 and adds nothing.
    Deaf,
    /// It adds the path, and `mds` rewrites the configuration before the edit lands.
    Rewritten,
}

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
                if config == Config::Rewritten {
                    let mut live = config_list(sys)?;
                    live.push("/Volumes/Added By mds".to_owned());
                    put(sys, VOLUME_CONFIG, &live);
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
const FULL: &[u8] = b"/Users//alice/Developer\n/Users//alice/Downloads\n/Users//alice/My Projects\n";

/// Root adds each home folder but Documents, Desktop, Library and Applications to the
/// excluded folders, keeps the entry a person put there, restarts mds once, and leaves a
/// receipt naming what it excluded; a second run changes nothing. The live configuration is
/// only ever replaced by a rename of a checked copy with its owner and mode, never edited
/// where it stands.
/// NEGATIVE CONTROLS: a dot-folder, a `.noindex` folder, a file and a link are never added; a
/// new folder is a change again.
#[test]
fn setup_excludes_the_home_folders_spotlight_need_not_index() {
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
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(
        rows[0].from.as_deref(),
        Some("indexed: Developer, Downloads, My Projects")
    );
    assert_eq!(
        sorted(&sys),
        [
            "/Users//alice/Developer",
            "/Users//alice/Downloads",
            "/Users//alice/My Projects",
            "/Volumes/Backup",
        ]
    );
    assert_eq!(sys.written(SCOPE_RECEIPT).as_deref(), Some(FULL));
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

    let new = format!("{ALICE}/Scratch");
    sys.file(&new, b"").stat_of(&new, folder());
    let rows = scope_rows(&sys, Mode::Plan);
    assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
    assert_eq!(rows[0].from.as_deref(), Some("indexed: Scratch"));
    let rows = scope_rows(&sys, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(count(&sys, KILL), 2);
    assert_eq!(config_list(&sys).unwrap().len(), 5);
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
    assert_eq!(
        sorted(&sys),
        [
            "/Users//alice/Developer",
            "/Users//alice/My Projects",
            "/Users//alice/Scratch",
        ]
    );
    // The receipt keeps naming it, which is the record of the person's choice.
    let receipt = String::from_utf8(sys.written(SCOPE_RECEIPT).unwrap()).unwrap();
    assert!(receipt.contains("/Users//alice/Downloads\n"), "{receipt}");
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
/// array, a configuration that is not there, and one mds rewrote while the copy was edited.
/// A later apply that succeeds removes that record.
/// NEGATIVE CONTROL: the same world with a `plutil` that writes ends changed, with no record.
#[test]
fn setup_makes_a_missing_list_and_a_failed_edit_leaves_the_configuration_alone() {
    let sys = Fake::root();
    home(&sys);
    spotlight(&sys, None, Config::Writes);
    assert_eq!(scope_rows(&sys, Mode::Apply)[0].outcome, Outcome::Changed);
    assert_eq!(config_list(&sys).map(|list| list.len()), Some(3));
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
    let (raced, _) = failed(&|sys| spotlight(sys, Some(&["/Volumes/Backup"]), Config::Rewritten));
    assert_eq!(
        config_list(&raced).unwrap(),
        ["/Volumes/Backup", "/Volumes/Added By mds"]
    );

    // The same Mac once its configuration takes the edit.
    spotlight(&deaf, Some(&["/Volumes/Backup"]), Config::Writes);
    assert_eq!(scope_rows(&deaf, Mode::Apply)[0].outcome, Outcome::Changed);
    assert!(deaf.written(SCOPE_REFUSED).is_none());
    assert_eq!(deaf.written(SCOPE_RECEIPT).as_deref(), Some(FULL));
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
/// folder of that home, and only at the console. Once one does, a folder the home gains later
/// asks no password by itself; the person's own setup still has it to exclude.
/// NEGATIVE CONTROLS: a receipt naming any folder owes nothing; one that is not root's own
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
    sys.file(SCOPE_RECEIPT, b"/Users//alice/Developer\n");
    assert!(!automatic_scope_owed(&sys));
    assert!(scope_owed(&sys));
    sys.file(SCOPE_RECEIPT, FULL);
    assert!(!automatic_scope_owed(&sys));
    assert!(!scope_owed(&sys));
    assert!(
        !sys.calls().iter().any(|call| call.contains("plutil")),
        "{:?}",
        sys.calls()
    );

    let refused = account();
    refused.file(SCOPE_REFUSED, b"/Users//alice/Developer\n");
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
        Ran::ok("Indexing disabled."),
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
