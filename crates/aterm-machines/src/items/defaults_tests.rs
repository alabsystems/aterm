// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::model::{HostFacts, Mode, Outcome};
use crate::port::{Fake, Ran};

pub(crate) fn user(name: &str, uid: u32) -> User {
    User {
        name: name.into(),
        uid,
        gid: 20,
        home: format!("/Users/{name}"),
        shell: "/bin/zsh".into(),
        admin: uid == 501,
    }
}

pub(crate) fn ctx(users: &[User]) -> Ctx {
    Ctx {
        facts: HostFacts {
            os_version: "27.0".into(),
            os_build: "26A428".into(),
            arch: "arm64".into(),
            ..HostFacts::default()
        },
        users: users.to_vec(),
        console: None,
        host_name: "lab-07".into(),
        cache_dir: "/c".into(),
        disrupt: true,
    }
}

/// A document as `defaults export` prints it.
pub(crate) fn export(body: &str) -> Ran {
    Ran::ok(&format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n{body}\n</plist>\n"
    ))
}

/// What the 27.0 VM's `defaults export` printed for a domain that does not exist.
pub(crate) fn missing() -> Ran {
    export("<dict/>")
}

const VM_ADLIB: &str = "<dict>\n\t<key>CKDPIDSyncState</key>\n\t<integer>1</integer>\n\t<key>CKSubscribedAccountsScrubbed</key>\n\t<true/>\n\t<key>allowApplePersonalizedAdvertising</key>\n\t<false/>\n\t<key>allowIdentifierForAdvertising</key>\n\t<true/>\n\t<key>personalizedAdsMigrated</key>\n\t<true/>\n</dict>";

fn run(
    items: &[Box<dyn Item>],
    sys: &Fake,
    ctx: &Ctx,
    mode: Mode,
) -> Vec<crate::model::ItemReport> {
    engine::run(items, mode, sys, ctx, &mut NoJournal)
}

fn one(pref: Pref) -> Vec<Box<dyn Item>> {
    vec![Box::new(pref)]
}

fn row(text: &str) -> Vec<Row> {
    #[derive(Deserialize)]
    struct Doc {
        defaults: Vec<Row>,
    }
    aterm_toml::from_str::<Doc>(text).unwrap().defaults
}

#[test]
fn a_missing_domain_and_a_missing_key_read_absent_and_a_present_key_reads_its_value() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    sys.on(
        "admin$ /usr/bin/defaults export com.example.none -",
        missing(),
    );
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.AdLib -",
        export(VM_ADLIB),
    );
    let none = Target::user("com.example.none", "k", &admin);
    assert_eq!(none.read(&sys), Read::Absent);
    let ads = Target::user(
        "com.apple.AdLib",
        "allowApplePersonalizedAdvertising",
        &admin,
    );
    assert_eq!(ads.read(&sys), Read::Value(PValue::Bool(false)));
    let other = Target::user("com.apple.AdLib", "noSuchKey", &admin);
    assert_eq!(other.read(&sys), Read::Absent);
}

#[test]
fn a_failed_or_garbled_export_is_unusable_never_absent() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.dock -",
        Ran::exit(1, "Error: Need a path to write to"),
    );
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.finder -",
        Ran::ok("<plist><dict><key>x</key>"),
    );
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.list -",
        export("<array/>"),
    );
    for (domain, needle) in [
        ("com.apple.dock", "exit 1: Error: Need a path"),
        ("com.apple.finder", "plist: at byte"),
        ("com.apple.list", "not a dictionary"),
        ("com.apple.unscripted", "no reply scripted"),
    ] {
        match Target::user(domain, "k", &admin).read(&sys) {
            Read::Unusable(why) => assert!(why.contains(needle), "{domain}: {why}"),
            other => panic!("{domain}: {other:?}"),
        }
    }
    let pref = Pref::new(
        "defaults",
        "x".into(),
        "x".into(),
        Target::user("com.apple.dock", "autohide", &admin),
        Want::Value(PValue::Bool(true)),
    );
    let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Unknown);
    assert!(!sys.calls().iter().any(|c| c.contains(" write ")));
}

#[test]
fn each_type_is_written_with_the_flag_or_fragment_the_vm_accepted() {
    let admin = user("admin", 501);
    let t = Target::user("com.example", "k", &admin);
    let line = |v: PValue| t.write_cmd(&v).display();
    assert_eq!(
        line(PValue::Bool(false)),
        "admin$ /usr/bin/defaults write com.example k -bool false"
    );
    assert_eq!(
        line(PValue::Int(30_064_771_129)),
        "admin$ /usr/bin/defaults write com.example k -int 30064771129"
    );
    assert_eq!(
        line(PValue::Int(-3)),
        "admin$ /usr/bin/defaults write com.example k '<integer>-3</integer>'"
    );
    assert_eq!(
        line(PValue::Str("Contacts Only".into())),
        "admin$ /usr/bin/defaults write com.example k -string 'Contacts Only'"
    );
    assert_eq!(
        line(PValue::Real(0.1)),
        "admin$ /usr/bin/defaults write com.example k '<real>0.10000000000000001</real>'"
    );
    assert_eq!(
        line(PValue::Date("2026-10-16T22:40:34Z".into())),
        "admin$ /usr/bin/defaults write com.example k '<date>2026-10-16T22:40:34Z</date>'"
    );
    assert_eq!(
        line(PValue::Array(vec![PValue::Int(1)])),
        "admin$ /usr/bin/defaults write com.example k '<array><integer>1</integer></array>'"
    );
    let host = Target::host("-g", "k", &admin);
    assert_eq!(
        host.delete_cmd().display(),
        "admin$ /usr/bin/defaults -currentHost delete -g k"
    );
    assert_eq!(
        host.export_cmd().display(),
        "admin$ /usr/bin/defaults -currentHost export -g -"
    );
    let sys = Target::system("com.apple.SoftwareUpdate", "AutomaticDownload");
    assert_eq!(
        sys.write_cmd(&PValue::Bool(true)).display(),
        "/usr/bin/defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload -bool true"
    );
    assert_eq!(
        Target::system("/Library/Preferences/com.apple.Bluetooth.plist", "k").domain,
        "/Library/Preferences/com.apple.Bluetooth"
    );
}

#[test]
fn a_change_is_written_as_the_user_and_counts_only_once_it_reads_back() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    let export_key = "admin$ /usr/bin/defaults export com.apple.desktopservices -";
    sys.on(export_key, missing());
    sys.on(
        export_key,
        export("<dict>\n\t<key>DSDontWriteNetworkStores</key>\n\t<true/>\n</dict>"),
    );
    sys.on(
        "admin$ /usr/bin/defaults write com.apple.desktopservices DSDontWriteNetworkStores -bool true",
        Ran::ok(""),
    );
    let pref = Pref::new(
        "defaults",
        "d".into(),
        "d".into(),
        Target::user(
            "com.apple.desktopservices",
            "DSDontWriteNetworkStores",
            &admin,
        ),
        Want::Value(PValue::Bool(true)),
    );
    let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert_eq!(rows[0].from.as_deref(), Some("absent"));
    assert_eq!(rows[0].to.as_deref(), Some("true"));
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(
            "admin$ /usr/bin/defaults delete com.apple.desktopservices DSDontWriteNetworkStores  # it was not set before this run"
        )
    );
    assert_eq!(rows[0].effect, Effect::Login);
}

#[test]
fn a_write_that_does_not_read_back_fails_with_what_it_reads() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.AdLib -",
        export(VM_ADLIB),
    );
    sys.on(
        "admin$ /usr/bin/defaults write com.apple.AdLib allowApplePersonalizedAdvertising -bool true",
        Ran::ok(""),
    );
    let pref = Pref::new(
        "defaults",
        "ads".into(),
        "ads".into(),
        Target::user(
            "com.apple.AdLib",
            "allowApplePersonalizedAdvertising",
            &admin,
        ),
        Want::Value(PValue::Bool(true)),
    );
    let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert_eq!(
        rows[0].why.as_deref(),
        Some("applied, but it still reads false")
    );
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(
            "admin$ /usr/bin/defaults write com.apple.AdLib allowApplePersonalizedAdvertising -bool false  # restores the value before this run"
        )
    );
}

#[test]
fn a_boolean_held_as_the_integer_macos_reads_for_it_is_left_alone() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.x -",
        export("<dict>\n\t<key>k</key>\n\t<integer>0</integer>\n</dict>"),
    );
    let pref = Pref::new(
        "defaults",
        "x".into(),
        "x".into(),
        Target::user("com.apple.x", "k", &admin),
        Want::Value(PValue::Bool(false)),
    );
    let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Ok);
    assert_eq!(sys.calls().len(), 1);
}

#[test]
fn an_unset_want_deletes_the_key_and_a_key_already_gone_is_not_an_error() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    let key = "admin$ /usr/bin/defaults export com.apple.sharingd -";
    sys.on(
        key,
        export("<dict>\n\t<key>DiscoverableMode</key>\n\t<string>Off</string>\n</dict>"),
    );
    sys.on(key, missing());
    sys.on(
        "admin$ /usr/bin/defaults delete com.apple.sharingd DiscoverableMode",
        Ran::exit(
            1,
            "Error: Could not find key 'DiscoverableMode' in domain 'com.apple.sharingd'.\nDefaults have not been changed.",
        ),
    );
    let pref = Pref::new(
        "defaults",
        "a".into(),
        "a".into(),
        Target::user("com.apple.sharingd", "DiscoverableMode", &admin),
        Want::Unset,
    );
    let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert_eq!(rows[0].from.as_deref(), Some("\"Off\""));
    assert_eq!(rows[0].to.as_deref(), Some("absent"));
    assert_eq!(
        rows[0].revert.as_deref(),
        Some(
            "admin$ /usr/bin/defaults write com.apple.sharingd DiscoverableMode -string Off  # restores the value before this run"
        )
    );
}

#[test]
fn a_system_write_runs_as_root_and_puts_the_files_owner_and_mode_back() {
    let sys = Fake::root();
    let path = "/Library/Application Support/CrashReporter/DiagnosticMessagesHistory.plist";
    sys.stat_of(
        path,
        Stat {
            uid: 0,
            gid: 80,
            mode: 0o664,
            dir: false,
            symlink: false,
            len: 156,
        },
    );
    let export_key = "/usr/bin/defaults export '/Library/Application Support/CrashReporter/DiagnosticMessagesHistory' -";
    let before = "<dict>\n\t<key>LastCleanupCalled</key>\n\t<date>2026-10-16T23:02:33Z</date>\n\t<key>LastFullSubmissionCalled</key>\n\t<date>2026-10-16T22:41:38Z</date>\n</dict>";
    sys.on(export_key, export(before));
    sys.on(
        export_key,
        export("<dict>\n\t<key>AutoSubmit</key>\n\t<false/>\n</dict>"),
    );
    sys.on(
        "/usr/bin/defaults write '/Library/Application Support/CrashReporter/DiagnosticMessagesHistory' AutoSubmit -bool false",
        Ran::ok(""),
    );
    let pref = Pref::new(
        "consumer",
        "consumer.analytics.auto-submit".into(),
        "t".into(),
        Target::system(
            "/Library/Application Support/CrashReporter/DiagnosticMessagesHistory",
            "AutoSubmit",
        ),
        Want::Value(PValue::Bool(false)),
    );
    let rows = run(&one(pref), &sys, &ctx(&[]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    let calls = sys.calls();
    let write = calls.iter().position(|c| c.contains(" write ")).unwrap();
    let own = calls
        .iter()
        .position(|c| c == &format!("own 0:80 664 {path}"))
        .unwrap();
    assert!(write < own, "{calls:?}");
    assert_eq!(rows[0].effect, Effect::Now);
}

fn dir(uid: u32, gid: u32, mode: u32) -> Stat {
    Stat {
        uid,
        gid,
        mode,
        dir: true,
        symlink: false,
        len: 0,
    }
}

#[test]
fn an_empty_export_of_a_system_domain_whose_file_exists_is_read_from_the_file() {
    let path = "/Library/Preferences/com.apple.apsd.plist";
    let export_key = "/usr/bin/defaults export /Library/Preferences/com.apple.apsd -";
    let plutil = format!("/usr/bin/plutil -convert xml1 -o - {path}");
    let target = Target::system("com.apple.apsd", "k");

    let sys = Fake::root();
    sys.file(path, b"bplist00");
    sys.on(export_key, missing());
    sys.on(
        &plutil,
        Ran::exit(
            1,
            "/Library/Preferences/com.apple.apsd.plist: (The file \u{201c}com.apple.apsd.plist\u{201d} couldn\u{2019}t be opened because you don\u{2019}t have permission to view it.)",
        ),
    );
    match target.read(&sys) {
        Read::Unusable(why) => assert!(why.contains("permission to view it"), "{why}"),
        other => panic!("an unreadable domain read {other:?}"),
    }
    let pref = Pref::new(
        "defaults",
        "x".into(),
        "x".into(),
        target.clone(),
        Want::Unset,
    );
    assert!(matches!(
        pref.probe(&sys, &ctx(&[])),
        Probe::Unusable { .. }
    ));

    let sys = Fake::root();
    sys.file(path, b"bplist00");
    sys.on(export_key, missing());
    sys.on(&plutil, missing());
    assert_eq!(target.read(&sys), Read::Absent);

    let sys = Fake::root();
    sys.file(path, b"bplist00");
    sys.on(export_key, missing());
    sys.on(
        &plutil,
        export("<dict>\n\t<key>k</key>\n\t<true/>\n</dict>"),
    );
    match target.read(&sys) {
        Read::Unusable(why) => assert!(why.contains("holds 1 keys"), "{why}"),
        other => panic!("{other:?}"),
    }

    let sys = Fake::root();
    sys.on(export_key, missing());
    assert_eq!(target.read(&sys), Read::Absent);
    assert_eq!(sys.calls(), [export_key]);
}

#[test]
fn a_key_whose_value_this_crate_cannot_hold_is_unusable_and_its_neighbours_read() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.ncprefs -",
        export("<dict>\n\t<key>big</key>\n\t<integer>9223372036854775815</integer>\n\t<key>k</key>\n\t<integer>3</integer>\n</dict>"),
    );
    assert_eq!(
        Target::user("com.apple.ncprefs", "k", &admin).read(&sys),
        Read::Value(PValue::Int(3))
    );
    match Target::user("com.apple.ncprefs", "big", &admin).read(&sys) {
        Read::Unusable(why) => assert!(why.contains("64-bit signed range"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_system_plist_below_a_directory_others_control_is_neither_read_nor_written() {
    for (dir_path, stat, needle) in [
        ("/Users//Shared", dir(0, 0, 0o1777), "writable by everyone"),
        ("/Users//alice", dir(501, 20, 0o755), "owned by uid 501"),
        ("/opt/x", dir(0, 20, 0o775), "writable by group 20"),
        (
            "/var",
            Stat {
                symlink: true,
                dir: false,
                ..dir(0, 0, 0o755)
            },
            "symbolic link",
        ),
    ] {
        let sys = Fake::root();
        sys.stat_of(dir_path, stat);
        let target = Target::system(&format!("{dir_path}/com.example"), "k");
        match target.read(&sys) {
            Read::Unusable(why) => assert!(why.contains(needle), "{dir_path}: {why}"),
            other => panic!("{dir_path}: {other:?}"),
        }
        assert!(sys.calls().is_empty(), "{dir_path}: {:?}", sys.calls());
    }
    let sys = Fake::root();
    sys.stat_of(
        "/Library/Application Support/CrashReporter",
        dir(0, 80, 0o775),
    );
    assert!(
        check_system_path(
            &sys,
            Path::new("/Library/Application Support/CrashReporter/DiagnosticMessagesHistory.plist")
        )
        .is_ok()
    );
    assert!(check_system_path(&sys, Path::new("/Library/Preferences/../../etc/x.plist")).is_err());
}

#[test]
fn a_system_plist_that_is_a_link_is_neither_read_nor_written_through() {
    let sys = Fake::root();
    let path = "/Library/Preferences/com.example.plist";
    sys.stat_of(
        path,
        Stat {
            uid: 501,
            gid: 80,
            mode: 0o755,
            dir: false,
            symlink: true,
            len: 20,
        },
    );
    sys.on(
        "/usr/bin/defaults export /Library/Preferences/com.example -",
        export("<dict>\n\t<key>k</key>\n\t<false/>\n</dict>"),
    );
    let pref = Pref::new(
        "defaults",
        "x".into(),
        "x".into(),
        Target::system("com.example", "k"),
        Want::Value(PValue::Bool(true)),
    );
    assert!(
        matches!(pref.probe(&sys, &ctx(&[])), Probe::Unusable { why } if why.contains("not a regular file"))
    );
    let err = pref.apply(&sys, &ctx(&[])).unwrap_err();
    assert!(err.starts_with("not written: "), "{err}");
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
}

fn sharingd(admin: &User) -> Pref {
    Pref::new(
        "consumer",
        "consumer.airdrop@admin".into(),
        "airdrop".into(),
        Target::user("com.apple.sharingd", "DiscoverableMode", admin),
        Want::Value(PValue::Str("Off".into())),
    )
    .restart("sharingd")
}

/// A login session the [`Fake`] runs ([`Fake::simulate`]): `defaults` reads and writes
/// one store, `launchctl print` answers as `session` says, and `pgrep`, `killall` and
/// `sleep` act on its processes, each started again at once under a new pid when it is
/// killed, as launchd does.
pub(crate) struct Session {
    prefs: Vec<(String, String, PValue)>,
    running: Vec<(String, u32)>,
    next_pid: u32,
    /// What `launchctl print gui/<uid>` answers.
    session: Ran,
    /// What happened, in order: `write <domain> <key>`, `kill <process>`.
    pub(crate) events: Vec<String>,
}

impl Session {
    /// A session running `processes`, with no preference set.
    pub(crate) fn running(processes: &[&str]) -> Self {
        Self {
            prefs: Vec::new(),
            running: processes
                .iter()
                .zip(400..)
                .map(|(name, pid)| ((*name).to_owned(), pid))
                .collect(),
            next_pid: 1000,
            session: Ran::ok("gui/501 = {\n"),
            events: Vec::new(),
        }
    }

    /// How many times `process` was killed.
    pub(crate) fn kills(&self, process: &str) -> usize {
        let event = format!("kill {process}");
        self.events.iter().filter(|e| **e == event).count()
    }

    fn answer(&mut self, fake: &Fake, cmd: &Cmd) -> Option<Ran> {
        let args: Vec<String> = cmd
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match (cmd.program.to_str()?, args.as_slice()) {
            (DEFAULTS, ["export", domain, "-"]) => {
                let entries = self
                    .prefs
                    .iter()
                    .filter(|(d, _, _)| d == domain)
                    .map(|(_, key, value)| (key.clone(), value.clone()))
                    .collect();
                Some(Ran::ok(&plist::document(&PValue::Dict(entries))))
            }
            (DEFAULTS, ["write", domain, key, value @ ..]) => {
                let value = match value {
                    ["-bool", b] => PValue::Bool(*b == "true"),
                    ["-int", i] => PValue::Int(i.parse().ok()?),
                    ["-string", text] => PValue::Str((*text).into()),
                    [fragment] => plist::parse(fragment.as_bytes()).ok()?,
                    _ => return None,
                };
                self.events.push(format!("write {domain} {key}"));
                self.prefs.retain(|(d, k, _)| !(d == domain && k == key));
                self.prefs.push(((*domain).into(), (*key).into(), value));
                Some(Ran::ok(""))
            }
            ("/usr/bin/pgrep", ["-u", _, "-x", process]) => {
                let pids: String = self
                    .running
                    .iter()
                    .filter(|(name, _)| name == process)
                    .map(|(_, pid)| format!("{pid}\n"))
                    .collect();
                Some(if pids.is_empty() {
                    Ran {
                        status: Some(1),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    }
                } else {
                    Ran::ok(&pids)
                })
            }
            (LAUNCHCTL, ["print", _]) => Some(self.session.clone()),
            (KILLALL, ["-u", _, process]) => {
                if !self.running.iter().any(|(name, _)| name == process) {
                    return Some(Ran::exit(
                        1,
                        "No matching processes belonging to you were found",
                    ));
                }
                for (name, pid) in &mut self.running {
                    if name == process {
                        fake.exited(*pid);
                        *pid = self.next_pid;
                        self.next_pid += 1;
                        fake.alive(*pid);
                    }
                }
                self.events.push(format!("kill {process}"));
                Some(Ran::ok(""))
            }
            ("/bin/sleep", ["1"]) => Some(Ran::ok("")),
            _ => None,
        }
    }
}

/// `session`, run by a fake acting as root over it.
pub(crate) fn session_of(session: Session) -> (Fake, Arc<Mutex<Session>>) {
    let sys = Fake::root();
    for (_, pid) in &session.running {
        sys.alive(*pid);
    }
    let session = Arc::new(Mutex::new(session));
    let model = Arc::clone(&session);
    sys.simulate(move |fake, cmd| model.lock().unwrap().answer(fake, cmd));
    (sys, session)
}

#[test]
fn a_change_relaunches_its_process_in_the_users_session_after_the_write() {
    let admin = user("admin", 501);
    let (sys, session) = session_of(Session::running(&["sharingd"]));
    let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert_eq!(rows[0].effect, Effect::Relaunch);
    assert_eq!(
        session.lock().unwrap().events,
        ["write com.apple.sharingd DiscoverableMode", "kill sharingd"]
    );
    let calls = sys.calls();
    let kill = calls.iter().position(|c| c.contains("killall")).unwrap();
    let read_back = calls
        .iter()
        .rposition(|c| c.contains("export com.apple.sharingd"))
        .unwrap();
    assert!(kill < read_back, "{calls:?}");
    assert!(
        rows[0]
            .revert
            .as_deref()
            .unwrap()
            .ends_with("; then relaunch sharingd")
    );
}

#[test]
fn without_a_login_session_nothing_is_relaunched() {
    let admin = user("admin", 501);
    let mut nobody = Session::running(&["sharingd"]);
    nobody.session = Ran::exit(112, "Bad request.\nCould not find domain for user gui: 501");
    let (sys, session) = session_of(nobody);
    let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert!(!sys.calls().iter().any(|c| c.contains("killall")));
    assert!(!sys.calls().iter().any(|c| c.contains("/bin/sleep")));
    assert_eq!(session.lock().unwrap().kills("sharingd"), 0);
}

#[test]
fn a_process_that_is_not_running_needs_no_relaunch() {
    let admin = user("admin", 501);
    let (sys, _session) = session_of(Session::running(&[]));
    let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.contains("killall") || c.contains("launchctl"))
    );
}

#[test]
fn a_session_check_that_fails_for_another_reason_is_a_failure_not_no_session() {
    let admin = user("admin", 501);
    let mut broken = Session::running(&["sharingd"]);
    broken.session = Ran::exit(64, "Unrecognized target specifier.");
    let (sys, _session) = session_of(broken);
    let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed);
    let why = rows[0].why.as_deref().unwrap();
    assert!(
        why.starts_with("applied, but not activated: ") && why.contains("exit 64"),
        "{why}"
    );
}

#[test]
fn rows_one_process_reads_relaunch_it_once_after_the_last() {
    let admin = user("admin", 501);
    let keys = ["autohide", "autohide-delay", "orientation"];
    let text: String = keys
        .iter()
        .map(|key| {
            format!(
                "[[defaults]]\ndomain = \"com.example.bar\"\nkey = \"{key}\"\nvalue = true\n\
                 scope = \"user\"\nrestart = \"SystemUIServer\"\n"
            )
        })
        .collect();
    let c = ctx(std::slice::from_ref(&admin));
    let shared = expand_rows(&row(&text), &c).unwrap();
    let own: Vec<Box<dyn Item>> = keys
        .iter()
        .map(|key| {
            Box::new(
                Pref::new(
                    "defaults",
                    format!("defaults.com.example.bar.{key}@admin"),
                    (*key).into(),
                    Target::user("com.example.bar", key, &admin),
                    Want::Value(PValue::Bool(true)),
                )
                .restart("SystemUIServer"),
            ) as Box<dyn Item>
        })
        .collect();
    for (items, relaunches) in [(shared, 1), (own, keys.len())] {
        let (sys, session) = session_of(Session::running(&["SystemUIServer"]));
        let mut journal = Lines::default();
        let rows = engine::run(&items, Mode::Apply, &sys, &c, &mut journal);
        assert!(
            rows.iter().all(|row| row.outcome == Outcome::Changed),
            "{rows:?}"
        );
        let session = session.lock().unwrap();
        assert_eq!(
            session.kills("SystemUIServer"),
            relaunches,
            "{:?}",
            session.events
        );
        let relaunch_lines = journal
            .0
            .iter()
            .filter(|id| id.contains("relaunch"))
            .count();
        assert_eq!(relaunch_lines, relaunches, "{:?}", journal.0);
        if relaunches == 1 {
            let last_write = session
                .events
                .iter()
                .rposition(|e| e.starts_with("write "))
                .unwrap();
            let kill = session
                .events
                .iter()
                .position(|e| e.starts_with("kill "))
                .unwrap();
            assert!(last_write < kill, "{:?}", session.events);
            let calls = sys.calls();
            let killed = calls.iter().position(|c| c.contains("killall")).unwrap();
            let read_backs = calls[killed..]
                .iter()
                .filter(|c| c.contains("export com.example.bar"))
                .count();
            assert_eq!(read_backs, 2 + keys.len(), "{calls:#?}");
        }
    }
}

/// The ids a journal was handed, in order.
#[derive(Default)]
struct Lines(Vec<String>);

impl aterm_converge::Journal for Lines {
    fn before_change(
        &mut self,
        id: &str,
        _from: &str,
        _to: &str,
        _revert: Option<&str>,
    ) -> Result<(), String> {
        self.0.push(id.into());
        Ok(())
    }
}

#[test]
fn a_failed_write_relaunches_nothing() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.sharingd -",
        missing(),
    );
    sys.on(
        "admin$ /usr/bin/defaults write com.apple.sharingd DiscoverableMode -string Off",
        Ran::exit(1, "Could not write domain"),
    );
    let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert!(
        rows[0]
            .why
            .as_deref()
            .unwrap()
            .contains("Could not write domain")
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.contains("launchctl") || c.contains("killall"))
    );
}

#[test]
fn a_relaunch_of_the_console_users_process_waits_while_the_mac_is_in_use() {
    let admin = user("admin", 501);
    let sys = Fake::root();
    sys.on(
        "admin$ /usr/bin/defaults export com.apple.sharingd -",
        missing(),
    );
    let mut busy = ctx(std::slice::from_ref(&admin));
    busy.disrupt = false;
    busy.console = Some(admin.clone());
    let rows = run(&one(sharingd(&admin)), &sys, &busy, Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Deferred);
    assert!(rows[0].why.as_deref().unwrap().contains("at the console"));
    assert!(!sys.calls().iter().any(|c| c.contains(" write ")));
    let mut other = busy.clone();
    other.console = Some(user("bob", 502));
    let rows = run(&one(sharingd(&admin)), &sys, &other, Mode::Plan);
    assert_eq!(rows[0].outcome, Outcome::Planned);
}

#[test]
fn user_rows_expand_per_user_with_their_name_filled_and_system_rows_once() {
    let rows = row(r#"
[[defaults]]
domain = "com.apple.dock"
key = "autohide"
value = true
scope = "user"
restart = "Dock"

[[defaults]]
domain = "com.example.{user}"
key = "home"
value = { path = "/Users/{user}/x", n = [1, 2.5] }
scope = "host"

[[defaults]]
domain = "com.apple.loginwindow"
key = "GuestEnabled"
value = false
scope = "system"

[[defaults]]
domain = "com.example"
key = "when"
value = 2026-10-16T22:40:34Z
scope = "user"
"#);
    let users = [user("alice", 501), user("bob", 502)];
    let items = expand_rows(&rows, &ctx(&users)).unwrap();
    let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
    assert_eq!(
        ids,
        [
            "defaults.com.apple.dock.autohide@alice",
            "defaults.com.apple.dock.autohide@bob",
            "defaults.byhost.com.example.alice.home@alice",
            "defaults.byhost.com.example.bob.home@bob",
            "defaults.system.com.apple.loginwindow.GuestEnabled",
            "defaults.com.example.when@alice",
            "defaults.com.example.when@bob",
        ]
    );
    assert_eq!(items[0].effect(), Effect::Relaunch);
    assert_eq!(items[4].effect(), Effect::Now);
    let sys = Fake::root();
    sys.on(
        "bob$ /usr/bin/defaults -currentHost export com.example.bob -",
        missing(),
    );
    let probe = items[3].probe(&sys, &ctx(&users));
    assert_eq!(
        probe,
        Probe::Change {
            from: "absent".into(),
            to: "{n = [1, 2.5], path = \"/Users//bob/x\"}".into(),
        }
    );
    assert_eq!(
        items[4].title(),
        "/Library/Preferences/com.apple.loginwindow GuestEnabled = false (system)"
    );
}

#[test]
fn rows_that_cannot_be_applied_as_written_are_refused() {
    let users = [user("alice", 501)];
    for (text, needle) in [
        (
            "[[defaults]]\ndomain = \"x\"\nkey = \"k\"\nvalue = 1\nscope = \"system\"\nrestart = \"Dock\"\n",
            "a system row has no user",
        ),
        (
            "[[defaults]]\ndomain = \"x\"\nkey = \"k\"\nvalue = 1\nscope = \"user\"\nrestart = \"-9\"\n",
            "not a process name",
        ),
        (
            "[[defaults]]\ndomain = \"/Users//x\"\nkey = \"k\"\nvalue = 1\nscope = \"host\"\n",
            "a name, not a path",
        ),
        (
            "[[defaults]]\ndomain = \"x\"\nkey = \"k\"\nvalue = 2026-10-16T22:40:34-07:00\nscope = \"user\"\n",
            "UTC to the second",
        ),
        (
            "[[defaults]]\ndomain = \"\"\nkey = \"k\"\nvalue = 1\nscope = \"user\"\n",
            "needs a domain and a key",
        ),
        (
            "[[defaults]]\ndomain = \"../LaunchDaemons/x\"\nkey = \"k\"\nvalue = 1\nscope = \"system\"\n",
            "without `..`",
        ),
    ] {
        let err = expand_rows(&row(text), &ctx(&users)).err().unwrap();
        assert!(err.contains(needle), "{text}: {err}");
        assert!(err.starts_with("[[defaults]] row 1"), "{err}");
    }
}

#[test]
fn an_unknown_field_in_a_row_is_refused_rather_than_ignored() {
    #[derive(Deserialize, Debug)]
    struct Doc {
        #[expect(dead_code, reason = "only the parse result is checked")]
        defaults: Vec<Row>,
    }
    let err = aterm_toml::from_str::<Doc>(
        "[[defaults]]\ndomain = \"x\"\nkey = \"k\"\nvalue = 1\nscope = \"user\"\nrestrat = \"Dock\"\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("restrat"), "{err}");
}

#[test]
fn an_unmet_item_is_blocked_and_never_applies() {
    let item = Unmet {
        family: "setup",
        id: "setup.x@a".into(),
        title: "x".into(),
        why: "the host's macOS version was not read".into(),
    };
    let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
    let sys = Fake::root();
    let rows = run(&items, &sys, &ctx(&[]), Mode::Apply);
    assert_eq!(rows[0].outcome, Outcome::Blocked);
    assert!(sys.calls().is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn the_live_tool_reads_a_missing_system_domain_as_absent_and_a_real_one_as_known() {
    // Read only: `defaults export` of system domains, as whoever runs the tests.
    if !Path::new(DEFAULTS).exists() {
        aterm_uds::host::could_not_run(format!("no {DEFAULTS} here (macOS ships it)"));
    }
    let live = crate::port::Live::new();
    let none = Target::system("com.aterm.machines.no-such-domain", "Key");
    assert_eq!(none.read(&live), Read::Absent);
    let languages = Target::system(".GlobalPreferences", "AppleLanguages");
    let read = languages.read(&live);
    assert!(!matches!(read, Read::Unusable(_)), "{read:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn the_live_tool_reads_a_system_domain_it_may_not_read_as_unusable_not_absent() {
    let path = Path::new("/Library/Preferences/com.apple.apsd.plist");
    let live = crate::port::Live::new();
    let denied = matches!(
        std::fs::File::open(path),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied
    );
    if !Path::new(DEFAULTS).exists() {
        aterm_uds::host::could_not_run(format!("no {DEFAULTS} here (macOS ships it)"));
    }
    aterm_uds::host::could_not_run_as_root("a root-only (0600) system plist");
    if !denied {
        aterm_uds::host::could_not_run(format!(
            "{} is not refused to this user here, so no domain stands in as unreadable",
            path.display()
        ));
    }
    let read = Target::system("com.apple.apsd", "Anything").read(&live);
    assert!(matches!(read, Read::Unusable(_)), "{read:?}");
}

#[test]
fn a_container_domain_is_withheld_unless_full_disk_access_is_known_held() {
    for unknown in [None, Some(false)] {
        let why = withheld("com.apple.notificationcenterui", unknown).unwrap();
        assert!(
            why.contains("Notification Center") && why.contains("Full Disk Access"),
            "{why}"
        );
    }
    assert_eq!(withheld("com.apple.notificationcenterui", Some(true)), None);
    for open in ["com.apple.WindowManager", "com.apple.dock", "-g"] {
        assert_eq!(withheld(open, None), None, "{open}");
        assert!(!contained(open), "{open}");
    }
    assert!(contained("com.apple.notificationcenterui"));
    for path in [
        "/Users//alice/Library/Group Containers/ABCDE12345.group.example/Library/Preferences/ABCDE12345.group.example",
        "/Users//alice/Library/group containers/ABCDE12345.group.example/Library/Preferences/ABCDE12345.group.example",
    ] {
        assert!(contained(path), "{path}");
        assert!(withheld(path, None).is_some(), "{path}");
    }
}

#[test]
fn a_raw_row_on_another_apps_container_is_read_and_written_only_with_the_grant() {
    const NC_PATH: &str = "/Users//alice/Library/Containers/com.apple.notificationcenterui/Data/Library/Preferences/com.apple.notificationcenterui";
    const OTHER_PATH: &str = "/Users//alice/library/containers/com.example.Sandboxed/Data/Library/Preferences/com.example.Sandboxed";
    const GROUP_PATH: &str = "/Users//alice/Library/group containers/ABCDE12345.group.example/Library/Preferences/ABCDE12345.group.example";
    let rows = row(r#"
[[defaults]]
domain = "com.apple.notificationcenterui"
key = "widgets"
value = { instances = [] }
scope = "user"

[[defaults]]
domain = "/Users/{user}/Library/Containers/com.apple.notificationcenterui/Data/Library/Preferences/com.apple.notificationcenterui"
key = "widgets"
value = { instances = [] }
scope = "user"

[[defaults]]
domain = "/Users/{user}/library/containers/com.example.Sandboxed/Data/Library/Preferences/com.example.Sandboxed"
key = "Shown"
value = false
scope = "user"

[[defaults]]
domain = "/Users/{user}/Library/group containers/ABCDE12345.group.example/Library/Preferences/ABCDE12345.group.example"
key = "Shown"
value = false
scope = "user"

[[defaults]]
domain = "com.apple.NotificationCenterUI"
key = "widgets"
value = { instances = [] }
scope = "user"

[[defaults]]
domain = "com.apple.dock"
key = "autohide"
value = true
scope = "user"

[[defaults]]
domain = "/Users/{user}/Library/Preferences/com.apple.dock"
key = "autohide"
value = true
scope = "user"
"#);
    let alice = user("alice", 501);
    let export = |domain: &str| {
        let shown = if domain.contains(' ') {
            format!("'{domain}'")
        } else {
            domain.to_owned()
        };
        format!("alice$ /usr/bin/defaults export {shown} -")
    };
    let contained = [
        "com.apple.notificationcenterui",
        NC_PATH,
        OTHER_PATH,
        GROUP_PATH,
        "com.apple.NotificationCenterUI",
    ];
    let open = [
        "com.apple.dock",
        "/Users//alice/Library/Preferences/com.apple.dock",
    ];
    for full_disk_access in [None, Some(false), Some(true)] {
        let mut context = ctx(std::slice::from_ref(&alice));
        context.facts.full_disk_access = full_disk_access;
        let items = expand_rows(&rows, &context).unwrap();
        assert_eq!(items.len(), 7);
        assert_eq!(
            items[0].id(),
            "defaults.com.apple.notificationcenterui.widgets@alice"
        );
        for (item, domain) in items.iter().zip(contained.into_iter().chain(open)) {
            let sys = Fake::root();
            sys.on(&export(domain), missing());
            let probe = item.probe(&sys, &context);
            let exported = sys.calls().contains(&export(domain));
            if full_disk_access == Some(true) || open.contains(&domain) {
                assert!(matches!(probe, Probe::Change { .. }), "{domain}: {probe:?}");
                assert!(exported, "{domain}: {:?}", sys.calls());
            } else {
                let Probe::Blocked { why } = probe else {
                    panic!("{full_disk_access:?} {domain}: {probe:?}");
                };
                assert!(why.contains("Full Disk Access"), "{why}");
                assert!(item.apply(&sys, &context).is_err());
                assert!(sys.calls().is_empty(), "{domain}: {:?}", sys.calls());
            }
        }
    }
}

#[test]
fn a_user_domain_that_climbs_with_dotdot_is_refused_whatever_the_grant() {
    let alice = user("alice", 501);
    for domain in [
        "/Users/{user}/Library/Preferences/../Containers/com.apple.notificationcenterui/Data/Library/Preferences/com.apple.notificationcenterui",
        "/Users/{user}/Library/Application Support/../Containers/com.example.Sandboxed/Data/Library/Preferences/com.example.Sandboxed",
        "/Users/{user}/Library/Preferences/../../{user}/Library/Preferences/com.apple.dock",
    ] {
        let rows = row(&format!(
            "[[defaults]]\ndomain = \"{domain}\"\nkey = \"widgets\"\nvalue = 1\nscope = \"user\"\n"
        ));
        for full_disk_access in [None, Some(false), Some(true)] {
            let mut context = ctx(std::slice::from_ref(&alice));
            context.facts.full_disk_access = full_disk_access;
            let err = expand_rows(&rows, &context)
                .err()
                .unwrap_or_else(|| panic!("{full_disk_access:?}: {domain} expanded"));
            assert!(err.contains("without `..`"), "{err}");
        }
    }
}

#[test]
fn a_system_row_is_gated_on_the_file_it_resolves_to() {
    let rows = row(r#"
[[defaults]]
domain = "/private/var/root/Library/Containers/com.example.Sandboxed/Data/Library/Preferences/com.example.Sandboxed"
key = "Shown"
value = false
scope = "system"

[[defaults]]
domain = "com.apple.notificationcenterui"
key = "widgets"
value = false
scope = "system"
"#);
    for full_disk_access in [None, Some(false), Some(true)] {
        let mut context = ctx(&[]);
        context.facts.full_disk_access = full_disk_access;
        let items = expand_rows(&rows, &context).unwrap();
        assert_eq!(items.len(), 2);
        for (n, item) in items.iter().enumerate() {
            let sys = Fake::root();
            let probe = item.probe(&sys, &context);
            let gated =
                matches!(&probe, Probe::Blocked { why } if why.contains("Full Disk Access"));
            if n == 0 && full_disk_access != Some(true) {
                assert!(gated, "{full_disk_access:?}: {probe:?}");
                assert!(item.apply(&sys, &context).is_err());
                assert!(sys.calls().is_empty(), "{:?}", sys.calls());
            } else {
                assert!(!gated, "{full_disk_access:?} row {n}: {probe:?}");
                assert!(!sys.calls().is_empty(), "{full_disk_access:?} row {n}");
            }
        }
    }
}
