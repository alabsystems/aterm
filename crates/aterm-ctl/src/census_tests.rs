// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

const LAUNCHD: i32 = 4_100;

struct Row {
    listed: Listed,
    exe: Option<Image>,
    argv: Option<Vec<String>>,
    lineage: Option<Lineage>,
    ran: Option<bool>,
    /// The image the kernel names once its words were read: an exec while they were.
    execs_to: Option<&'static str>,
    stdin_terminal: Option<bool>,
}

/// A process table with the reads the census makes of it, recording which pids it was asked
/// the arguments and the standard input of.
#[derive(Default)]
struct Fake {
    rows: Vec<Row>,
    launchd: Option<i32>,
    unreadable: bool,
    /// A parent's own child list does not read either.
    children_unread: bool,
    argv_asked: RefCell<BTreeSet<u32>>,
    stdin_asked: RefCell<BTreeSet<u32>>,
}

impl Fake {
    fn with(rows: Vec<Row>) -> Self {
        Self {
            rows,
            launchd: Some(LAUNCHD),
            ..Self::default()
        }
    }

    fn row(&self, pid: u32) -> Option<&Row> {
        self.rows.iter().find(|r| r.listed.pid == pid)
    }
}

impl Kernel for Fake {
    fn listed(&self) -> Option<Vec<Listed>> {
        (!self.unreadable).then(|| self.rows.iter().map(|r| r.listed).collect())
    }
    fn children(&self, parent: u32) -> Option<Vec<u32>> {
        (!self.children_unread).then(|| {
            self.rows
                .iter()
                .filter(|r| r.listed.ppid == Some(parent))
                .map(|r| r.listed.pid)
                .collect()
        })
    }
    fn exe(&self, pid: u32) -> Option<Image> {
        let row = self.row(pid)?;
        match row.execs_to {
            Some(path) if self.argv_asked.borrow().contains(&pid) => Some(Image {
                path: PathBuf::from(path),
                deleted: false,
            }),
            _ => row.exe.clone(),
        }
    }
    fn ran(&self, pid: u32) -> Option<bool> {
        self.row(pid)?.ran
    }
    fn argv(&self, pid: u32) -> Option<Vec<OsString>> {
        self.argv_asked.borrow_mut().insert(pid);
        let words = self.row(pid)?.argv.as_ref()?;
        Some(words.iter().map(OsString::from).collect())
    }
    fn lineage(&self, pid: u32) -> Option<Lineage> {
        self.row(pid)?.lineage
    }
    fn launchd_version(&self) -> Option<i32> {
        self.launchd
    }
    fn stdin_terminal(&self, pid: u32) -> Option<bool> {
        self.stdin_asked.borrow_mut().insert(pid);
        self.row(pid)?.stdin_terminal
    }
}

const NOW: u64 = 1_790_000_000;

fn proc(pid: u32, ppid: u32, age_s: u64, exe: &str, argv: &[&str]) -> Row {
    Row {
        listed: Listed {
            pid,
            ppid: Some(ppid),
            birth: Some(ProcessBirth {
                seconds: NOW - age_s,
                microseconds: u64::from(pid),
            }),
            age_s: Some(age_s),
        },
        exe: Some(Image {
            path: PathBuf::from(exe),
            deleted: false,
        }),
        argv: Some(argv.iter().map(ToString::to_string).collect()),
        lineage: None,
        ran: Some(true),
        execs_to: None,
        stdin_terminal: Some(true),
    }
}

fn detached(mut row: Row) -> Row {
    row.stdin_terminal = Some(false);
    row
}

fn under_launchd(mut row: Row, launcher_version: Option<i32>) -> Row {
    row.listed.ppid = Some(1);
    row.lineage = launcher_version.map(|launcher_version| Lineage {
        identity: aterm_uds::process::Identity {
            pid: i32::try_from(row.listed.pid).expect("pid"),
            birth: row.listed.birth.expect("a birth").into(),
            idversion: Some(90_000),
        },
        uniqueid: u64::from(row.listed.pid),
        parent: 1,
        pgid: row.listed.pid,
        launcher_version: Some(launcher_version),
        tty: None,
    });
    row
}

/// `row` with a lineage that keeps no launcher version, as Linux's.
fn unversioned(mut row: Row) -> Row {
    if let Some(lineage) = row.lineage.as_mut() {
        lineage.launcher_version = None;
    }
    row
}

const ATERM: &str = "/opt/aterm/bin/aterm";
const BUNDLE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

fn verbs(word: &str) -> Option<Verb> {
    match word {
        "link" | "ctl" | "doctor" | "pkg" | "update" | "-h" | "--help" | "-V" | "--version"
        | "--completions" => Some(Verb::Program),
        "new-window" | "new-tab" => Some(Verb::Window),
        _ if word.starts_with("--completions=") => Some(Verb::Program),
        _ => None,
    }
}

/// The front door's argv0 aliases, as aterm-cli's table names them.
fn alias(name: &str, first: &str) -> Option<Verb> {
    match name {
        "aterm-gui" | "aterm-windowed" if first == "__tcc-open" => None,
        "aterm-gui" | "aterm-windowed" => Some(Verb::Window),
        "aterm-ctl" | "aterm-link" | "aterm-askpass" | "atpkg" | "abrew" | "amacs" => {
            Some(Verb::Program)
        }
        _ => None,
    }
}

const DOOR: FrontDoor<'static> = FrontDoor {
    alias: &alias,
    verb: &verbs,
};

fn table() -> Vec<Row> {
    vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        proc(7641, 1, 128_929, BUNDLE, &[BUNDLE, "--window"]),
        proc(
            67548,
            50,
            1_031_523,
            "/w/before/aterm",
            &["./aterm", "--headless"],
        ),
        proc(
            19235,
            50,
            341_206,
            "/w/target/debug/deps/aterm_pty-8a8f4252995814f9",
            &[
                "/w/target/debug/deps/aterm_pty-8a8f4252995814f9",
                "--headless",
            ],
        ),
        proc(300, 50, 300, ATERM, &["aterm"]),
        proc(
            301,
            50,
            2,
            ATERM,
            &["aterm", "ctl", "--sock", "/x", "windows"],
        ),
        proc(302, 50, 9, ATERM, &["aterm", "--session", "--headless"]),
    ]
}

fn census(k: &Fake, published: &[u32]) -> Census {
    let published = published.iter().copied().collect();
    census_of(k, 100, &published, "/run/aterm".into(), DOOR).expect("census")
}

#[test]
fn it_names_the_unreached_instance_and_ignores_the_reached_one() {
    let k = Fake::with(table());
    let c = census(&k, &[7641]);
    assert_eq!(c.instances, 2, "{c:?}");
    assert_eq!(
        c.unreached,
        vec![Unreached {
            pid: 67548,
            age: "11d 22h".to_string(),
            command: "./aterm --headless".to_string(),
            socketless: false,
        }]
    );
    let c = census(&k, &[7641, 67548]);
    assert_eq!((c.instances, c.unreached.len()), (2, 0));
    let programs: Vec<(u32, &str)> = c
        .programs
        .iter()
        .map(|p| (p.pid, p.name.as_str()))
        .collect();
    assert_eq!(
        programs,
        [(300, "aterm"), (302, "aterm"), (301, "aterm ctl")],
        "the sessions and the client are programs, oldest first"
    );
}

#[test]
fn an_aterm_program_is_known_by_its_executable_not_its_name() {
    let mut unnamed = proc(503, 50, 40, ATERM, &["aterm", "--headless"]);
    unnamed.exe = None;
    let k = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        proc(500, 50, 40, "/bin/bash", &["aterm", "--headless"]),
        proc(501, 50, 40, ATERM, &["x", "--headless"]),
        proc(
            502,
            50,
            40,
            "/w/target/debug/deps/aterm_pty-8a8f4252995814f9",
            &["aterm", "--headless"],
        ),
        unnamed,
    ]);
    let c = census(&k, &[]);
    assert_eq!(c.instances, 1, "{c:?}");
    let unreached: Vec<u32> = c.unreached.iter().map(|u| u.pid).collect();
    assert_eq!(unreached, [501]);
    assert!(c.programs.is_empty(), "{c:?}");
    assert_eq!(
        *k.argv_asked.borrow(),
        BTreeSet::from([501]),
        "only a proven aterm's arguments are read"
    );
    assert!(!is_aterm(Path::new("/bin/zsh")));
    assert!(!is_aterm(Path::new("/x/atermx")));
    assert!(is_aterm(Path::new("/repo/target/debug/aterm-gui")));
}

#[test]
fn a_held_program_s_arguments_are_never_read() {
    let mut held = proc(900, 50, 30, ATERM, &["aterm", "--headless"]);
    held.ran = Some(false);
    let k = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        held,
        proc(901, 50, 30, ATERM, &["aterm", "--headless"]),
    ]);
    let c = census(&k, &[]);
    assert!(
        !k.argv_asked.borrow().contains(&900),
        "a held program's arguments were read: {c:?}"
    );
    assert_eq!(c.instances, 1, "{c:?}");
    let names: Vec<(u32, &str)> = c
        .programs
        .iter()
        .map(|p| (p.pid, p.name.as_str()))
        .collect();
    assert_eq!(names, [(900, "aterm")], "{c:?}");
}

/// An instance's children are its child processes the census proves aterm by executable,
/// each with its words, read only for those and never for a held one: a child running
/// another program with an aterm's words, another process's aterm child and a child whose
/// executable is unnamed are not its, and no row is a child of pid 0 or of launchd.
/// NEGATIVE CONTROL: take any aterm row whatever its parent, and the other process's child
/// joins; drop the `parent > 1` guard, and launchd's aterm child is one.
#[test]
fn an_instance_s_children_are_its_aterm_programs() {
    let serve = ["aterm", "link", "serve", "--fleet", "f"];
    let mut held = proc(4003, 4000, 10, ATERM, &serve);
    held.ran = Some(false);
    let mut unnamed = proc(4005, 4000, 10, ATERM, &serve);
    unnamed.exe = None;
    let k = Fake::with(vec![
        proc(4000, 50, 60, ATERM, &[ATERM, "--headless"]),
        proc(4001, 4000, 10, ATERM, &serve),
        proc(4002, 4000, 10, "/bin/bash", &serve),
        held,
        proc(4004, 77, 10, ATERM, &serve),
        unnamed,
        proc(4006, 1, 10, ATERM, &serve),
        proc(4007, 0, 10, ATERM, &serve),
    ]);
    assert_eq!(
        children_of(&k, 4000).expect("its children"),
        [
            Child {
                pid: 4001,
                words: Some(serve.map(String::from).to_vec()),
            },
            Child {
                pid: 4003,
                words: None,
            },
        ]
    );
    assert_eq!(
        *k.argv_asked.borrow(),
        BTreeSet::from([4001]),
        "only an unheld aterm child's arguments are read"
    );
    assert_eq!(children_of(&k, 1), Some(Vec::new()), "no child of launchd");
    assert_eq!(children_of(&k, 0), Some(Vec::new()), "no child of pid 0");
}

/// An instance's children are read for it alone, never out of the whole process table: the
/// kernel can hold a `KERN_PROC_ALL` read past its bound while another process is mid-exec, and
/// a busy machine's doctor then read every instance as bridgeless (`aterm fabric --json` named
/// `bridge_pids: []` beside a live bridge). With the table unreadable, the bridge child is still
/// found; with its own list unreadable, the answer is none read, never an empty list.
/// NEGATIVE CONTROL: take the children out of `listed()` again, and the table's miss reads as
/// `None` here.
#[test]
fn an_instance_s_children_do_not_wait_on_the_whole_table() {
    let serve = ["aterm", "link", "serve", "--fleet", "f"];
    let mut k = Fake::with(vec![
        proc(4000, 50, 60, ATERM, &[ATERM, "--headless"]),
        proc(4001, 4000, 1, ATERM, &serve),
    ]);
    k.unreadable = true;
    assert_eq!(
        children_of(&k, 4000),
        Some(vec![Child {
            pid: 4001,
            words: Some(serve.map(String::from).to_vec()),
        }])
    );
    k.children_unread = true;
    assert_eq!(children_of(&k, 4000), None);
}

/// An aterm's words are read only once its one thread has run (`thread_ran`, the guard of
/// every read of another process's arguments, since such a read of a program at its first
/// instruction can hang): one at its first instruction for less than the held judgment's
/// 20 s, and one whose thread list does not read, keep their words unread and are aterm
/// programs named by their executable. NEGATIVE CONTROL: guard the read with the held
/// judgment instead, and both have their words read.
#[test]
fn an_aterm_s_words_are_read_only_once_its_thread_ran() {
    let mut starting = proc(900, 50, 5, ATERM, &["aterm", "--headless"]);
    starting.ran = Some(false);
    let mut unread = proc(902, 50, 5, ATERM, &["aterm", "--headless"]);
    unread.ran = None;
    let k = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        starting,
        proc(901, 50, 30, ATERM, &["aterm", "--headless"]),
        unread,
    ]);
    let c = census(&k, &[]);
    assert_eq!(
        *k.argv_asked.borrow(),
        BTreeSet::from([901]),
        "only a program whose thread ran has its words read: {c:?}"
    );
    assert_eq!(c.instances, 1, "{c:?}");
    let names: Vec<(u32, &str)> = c
        .programs
        .iter()
        .map(|p| (p.pid, p.name.as_str()))
        .collect();
    assert_eq!(names, [(900, "aterm"), (902, "aterm")], "{c:?}");
}

/// A process proven aterm that execs another program while its words are read is no aterm:
/// the words are kept only while the kernel names the same image after the read, so another
/// program's words neither name a row nor make a child. NEGATIVE CONTROL: keep the words
/// without reading the image again, and the shell is an aterm program and the instance's
/// child.
#[test]
fn a_process_that_execs_another_program_as_its_words_are_read_is_no_aterm() {
    let table = || {
        let mut execs = proc(4001, 4000, 10, ATERM, &["-zsh"]);
        execs.execs_to = Some("/bin/zsh");
        Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            proc(4000, 50, 60, ATERM, &[ATERM, "--headless"]),
            execs,
        ])
    };
    let k = table();
    let children = children_of(&k, 4000).expect("its children");
    assert!(children.is_empty(), "{children:?}");
    let c = census(&table(), &[]);
    assert_eq!(c.instances, 1, "{c:?}");
    assert!(c.programs.is_empty(), "{c:?}");
}

#[test]
fn verbs_are_counted_by_their_words() {
    let k = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        proc(
            600,
            50,
            10,
            ATERM,
            &[ATERM, "link", "broker", "/r/b.sock", "/r/b.log"],
        ),
        proc(
            601,
            50,
            10,
            "/w/target/debug/aterm-link",
            &["/w/target/debug/aterm-link", "broker", "/r/b.sock"],
        ),
        proc(
            602,
            50,
            10,
            ATERM,
            &["aterm", "link", "serve", "--fleet", "f"],
        ),
        proc(
            603,
            50,
            10,
            ATERM,
            &["aterm", "ctl", "--sock", "x", "await"],
        ),
        proc(604, 50, 10, ATERM, &["aterm", "frobnicate"]),
        proc(605, 50, 10, ATERM, &["aterm-ctl", "status"]),
        proc(606, 50, 10, ATERM, &["aterm", "link", "Broker;rm"]),
        proc(
            607,
            50,
            10,
            "/opt/bin/aterm-dev build",
            &["/opt/bin/aterm-dev build"],
        ),
        proc(
            608,
            50,
            10,
            "/w/target/debug/aterm-lab",
            &["aterm-lab", "instance"],
        ),
        proc(609, 50, 10, ATERM, &["aterm", "--completions=\u{1b}]0;x"]),
        proc(610, 50, 10, ATERM, &["aterm---version", "--version"]),
        proc(611, 50, 10, BUNDLE, &["atpkg", "update"]),
        proc(
            612,
            50,
            10,
            BUNDLE,
            &["/opt/MacOS/aterm-askpass", "Password:"],
        ),
        detached(proc(613, 50, 10, BUNDLE, &["abrew", "--real", "install"])),
        detached(proc(
            614,
            50,
            10,
            "/w/target/debug/aterm-verify",
            &["/w/target/debug/aterm-verify", "--no-windows"],
        )),
    ]);
    let c = census(&k, &[]);
    let names: BTreeMap<u32, &str> = c
        .programs
        .iter()
        .map(|p| (p.pid, p.name.as_str()))
        .collect();
    let said: Vec<&str> = names.values().copied().collect();
    assert!(!said.contains(&"aterm frobnicate"), "{names:?}");
    assert!(!said.contains(&"aterm doctor"), "{names:?}");
    assert_eq!(
        names,
        BTreeMap::from([
            (600, "aterm link broker"),
            (601, "aterm link broker"),
            (602, "aterm link serve"),
            (603, "aterm ctl"),
            (604, "aterm"),
            (605, "aterm ctl status"),
            (606, "aterm link"),
            (607, "aterm?"),
            (608, "aterm-lab"),
            (609, "aterm"),
            (610, "aterm"),
            (611, "atpkg"),
            (612, "aterm-askpass"),
            (613, "abrew"),
            (614, "aterm-verify"),
        ]),
        "a flag the front door answers names no verb, and an alias is named by itself"
    );
    assert_eq!(c.instances, 0, "{c:?}");
}

#[test]
fn an_adopted_program_is_said_and_a_launchd_started_one_is_not() {
    let broker = |pid| {
        proc(
            pid,
            4_000,
            20,
            ATERM,
            &[ATERM, "link", "broker", "/r/b.sock"],
        )
    };
    let k = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        under_launchd(broker(700), Some(31)),
        under_launchd(broker(701), Some(LAUNCHD)),
        under_launchd(broker(702), None),
        broker(703),
        unversioned(under_launchd(broker(705), Some(31))),
        under_launchd(
            proc(704, 4_000, 80, BUNDLE, &[BUNDLE, "pkg", "update"]),
            Some(77),
        ),
    ]);
    let c = census(&k, &[]);
    let by_pid: BTreeMap<u32, &Program> = c.programs.iter().map(|p| (p.pid, p)).collect();
    assert!(by_pid[&700].adopted, "adopted: {c:?}");
    assert!(!by_pid[&701].adopted, "launchd started it: {c:?}");
    assert!(!by_pid[&702].adopted, "unjudged: {c:?}");
    assert!(!by_pid[&703].adopted, "its launcher lives: {c:?}");
    assert!(
        !by_pid[&705].adopted,
        "a lineage that keeps no launcher version (Linux's) is unjudged: {c:?}"
    );
    assert!(
        by_pid[&704].adopted && by_pid[&704].name == "aterm pkg update",
        "{c:?}"
    );
    // A watcher runs under the tether's own name; one an earlier build left, under its path.
    let watcher = |pid, exe, arg0| {
        let argv = [arg0, "__tether", "500", "/usr/bin/log", "log", "stream"];
        under_launchd(proc(pid, 4_000, 20, exe, &argv), Some(31))
    };
    let c = census(
        &Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            watcher(706, BUNDLE, aterm_uds::tether::NAME),
            watcher(
                707,
                "/w/target/debug/aterm-gui",
                "/w/target/debug/aterm-gui",
            ),
        ]),
        &[],
    );
    assert_eq!(c.instances, 0, "a watcher is no window: {c:?}");
    assert!(
        c.programs
            .iter()
            .all(|p| p.name == "aterm tether" && !p.adopted)
            && c.programs.len() == 2,
        "a tether's watcher is launchd's by design: {c:?}"
    );
    let holder = |pid, verb| {
        let argv = [BUNDLE, verb, "500", "77", "/l/1-500-0.claim", "{}", "-"];
        under_launchd(proc(pid, 4_000, 20, BUNDLE, &argv), Some(31))
    };
    let c = census(
        &Fake::with(vec![
            proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
            holder(708, "__build_hold"),
            holder(709, "__build_holder"),
            proc(
                710,
                50,
                20,
                BUNDLE,
                &[
                    "atpkg",
                    "__build_admit",
                    "--shim",
                    "targo",
                    "-",
                    "/s/targo",
                    "test",
                ],
            ),
        ]),
        &[],
    );
    let by_pid: BTreeMap<u32, &Program> = c.programs.iter().map(|p| (p.pid, p)).collect();
    assert!(
        by_pid[&708].name == "aterm build hold" && !by_pid[&708].adopted,
        "a build's claim holder is launchd's by design: {c:?}"
    );
    assert!(by_pid[&709].adopted, "another verb is an orphan: {c:?}");
    assert_eq!(by_pid[&710].name, "atpkg build-admit", "{c:?}");
    let mut unread = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        under_launchd(broker(700), Some(31)),
    ]);
    unread.launchd = None;
    assert!(
        !census(&unread, &[]).programs[0].adopted,
        "launchd's version unread proves nothing"
    );
}

#[test]
fn a_build_and_a_deleted_binary_are_said() {
    let broker = |pid, exe: &str| proc(pid, 50, 20, exe, &[exe, "link", "broker"]);
    let mut gone = broker(801, "/w/gone/target/debug/aterm");
    gone.exe = Some(Image {
        path: PathBuf::from("/w/gone/target/debug/aterm"),
        deleted: true,
    });
    let k = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        broker(800, "/w/wt/target/debug/aterm"),
        gone,
        broker(802, BUNDLE),
        broker(803, "/w/wt/target.noindex/release/aterm-link"),
    ]);
    let c = census(&k, &[]);
    let by_pid: BTreeMap<u32, &Program> = c.programs.iter().map(|p| (p.pid, p)).collect();
    assert!(by_pid[&800].build && !by_pid[&800].deleted, "{c:?}");
    assert!(by_pid[&801].build && by_pid[&801].deleted, "{c:?}");
    assert!(!by_pid[&802].build && !by_pid[&802].deleted, "{c:?}");
    assert!(by_pid[&803].build, "{c:?}");
    assert!(
        !in_build_dir(Path::new("/target")),
        "the file itself is no directory"
    );
}

/// Linux's ` (deleted)` mark comes off the `exe` link, so the process keeps its file name.
#[cfg(unix)]
#[test]
fn linux_s_deleted_mark_is_taken_off_the_executable() {
    assert_eq!(
        unmarked(Path::new("/w/wt/target/debug/aterm (deleted)")),
        Some(PathBuf::from("/w/wt/target/debug/aterm"))
    );
    assert_eq!(unmarked(Path::new("/w/wt/target/debug/aterm")), None);
    let image = unmarked(Path::new("/w/aterm-link (deleted)")).expect("marked");
    assert!(is_aterm(&image), "{}", image.display());
    assert_eq!(exe_word(&image), "aterm-link");
}

#[test]
fn a_census_without_its_own_row_is_not_taken() {
    let k = Fake::with(table());
    census_of(&k, 99_999, &BTreeSet::new(), String::new(), DOOR).expect_err("no own row");
    let mut unreadable = Fake::with(table());
    unreadable.unreadable = true;
    census_of(&unreadable, 100, &BTreeSet::new(), String::new(), DOOR).expect_err("no table");
}

#[test]
fn window_mode_is_read_from_the_launch_flags() {
    let routed = |exe: &str, argv: &[&str], stdin_terminal: Option<bool>| {
        let words: Vec<String> = argv.iter().map(ToString::to_string).collect();
        window_args(Path::new(exe), &words, || stdin_terminal, DOOR).is_some()
    };
    let is_window = |exe: &str, argv: &[&str]| routed(exe, argv, Some(true));
    let started_alone = |exe: &str, argv: &[&str]| routed(exe, argv, Some(false));
    assert!(is_window(
        ATERM,
        &["aterm", "--headless", "--columns", "80"]
    ));
    assert!(is_window(ATERM, &["aterm", "--diagnose"]));
    assert!(is_window("/w/aterm-gui", &["aterm-gui"]));
    assert!(is_window(ATERM, &["aterm-gui"]), "the alias name");
    assert!(
        is_window(ATERM, &["aterm-windowed"]),
        "the windowed image's name"
    );
    assert!(is_window(ATERM, &["aterm", "new-window"]), "a window verb");
    assert!(is_window(ATERM, &["aterm-gui", "new-tab", "-d", "/w"]));
    assert!(started_alone(BUNDLE, &[BUNDLE]));
    assert!(
        started_alone(BUNDLE, &[BUNDLE, "--no-control-sock"]),
        "a window's flags keep it a window"
    );
    assert!(
        !is_window(BUNDLE, &["aterm"]),
        "a bare `aterm` typed on a terminal is a session, whatever its executable"
    );
    assert!(
        !routed(ATERM, &["aterm"], None),
        "an unread standard input proves nothing"
    );
    for answered in [
        &["aterm", "--version"][..],
        &["aterm", "-V"],
        &["aterm", "--help"],
        &["aterm", "-h"],
        &["aterm", "--completions", "zsh"],
        &["aterm", "--completions=zsh"],
    ] {
        assert!(
            !started_alone(ATERM, answered),
            "the front door answers {answered:?} before it routes a window"
        );
    }
    assert!(!started_alone(ATERM, &["aterm", "--session"]));
    assert!(
        !started_alone(ATERM, &["aterm-cli"]),
        "the forced session's name"
    );
    let spaced = "/Users//a/My Apps/aterm.app/Contents/MacOS/aterm";
    assert!(
        started_alone(spaced, &[spaced]),
        "a bundle path with a space"
    );
    assert!(!is_window(
        ATERM,
        &["aterm", "-e", "sh", "-c", "aterm --headless"]
    ));
    assert!(!is_window(ATERM, &["aterm", "--", "--window"]));
    assert!(!started_alone(
        ATERM,
        &["aterm", "pkg", "doctor", "--window"]
    ));
    assert!(!is_window(
        "/w/aterm-link",
        &["aterm-link", "serve", "--fleet", "f"]
    ));
    let support = "/Users//a/Library/Application Support/aterm/bin/aterm";
    assert!(is_window(support, &[support, "--headless"]));
    assert!(is_window(ATERM, &["/usr/local/bin/aterm", "--headless"]));
    for alias in [
        &["atpkg", "--json", "list"][..],
        &["abrew", "--real", "install", "x"],
        &["/Applications/aterm.app/Contents/MacOS/aterm-askpass"],
        &["amacs"],
        &["aterm-ctl", "new-window"],
    ] {
        assert!(
            !started_alone(BUNDLE, alias),
            "the front door hands {alias:?} to the alias's own program"
        );
    }
    assert!(
        !is_window(BUNDLE, &["aterm-gui", "__tcc-open", "/x"]),
        "the window alias's open probe is answered at the front door"
    );
    assert!(
        started_alone(BUNDLE, &["renamed"]),
        "an argv[0] no alias names is the front door's own"
    );
    let verify = "/w/target/debug/aterm-verify";
    assert!(
        !started_alone(verify, &[verify, "--no-windows"]),
        "another aterm image is its own program, never the front door"
    );

    let socketless = |argv: &[&str]| {
        let words: Vec<String> = argv.iter().map(ToString::to_string).collect();
        socket_off(&window_args(Path::new(ATERM), &words, || Some(true), DOOR).expect("a window"))
    };
    assert!(socketless(&["aterm", "--headless", "--no-control-sock"]));
    assert!(socketless(&[
        "aterm",
        "--headless",
        "--control-sock",
        "off"
    ]));
    assert!(socketless(&["aterm", "--headless", "--control-sock", "0"]));
    assert!(!socketless(&[
        "aterm",
        "--headless",
        "--control-sock",
        "/tmp/a.sock"
    ]));
    assert!(!socketless(&["aterm", "--headless"]));
}

#[test]
fn a_window_is_counted_by_the_front_door_s_route() {
    let k = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        proc(1001, 50, 50, ATERM, &["aterm", "new-window", "-d", "/w"]),
        detached(proc(1002, 1, 40, BUNDLE, &[BUNDLE, "--no-control-sock"])),
        proc(1003, 50, 30, BUNDLE, &["aterm"]),
        proc(1004, 50, 20, ATERM, &["aterm", "--headless"]),
    ]);
    let c = census(&k, &[1001, 1004]);
    assert_eq!(c.instances, 3, "{c:?}");
    let unreached: Vec<(u32, bool)> = c.unreached.iter().map(|u| (u.pid, u.socketless)).collect();
    assert_eq!(unreached, [(1002, true)], "{c:?}");
    let programs: Vec<(u32, &str)> = c
        .programs
        .iter()
        .map(|p| (p.pid, p.name.as_str()))
        .collect();
    assert_eq!(programs, [(1003, "aterm")], "{c:?}");
    assert_eq!(
        *k.stdin_asked.borrow(),
        BTreeSet::from([1002, 1003]),
        "standard input is read only where no word or flag routes"
    );
}

#[test]
fn an_unreached_instance_s_words_are_its_escaped_launch_flags() {
    let k = Fake::with(vec![
        proc(100, 50, 60, ATERM, &[ATERM, "doctor"]),
        proc(
            1100,
            50,
            20,
            ATERM,
            &[
                "aterm",
                "--headless",
                "--working-directory",
                "/w/\u{1b}]52;c;eA==\u{7}\r\n",
                "-e",
                "sh",
                "-c",
                "LOGIN=secret run",
            ],
        ),
    ]);
    let c = census(&k, &[]);
    let said = &c.unreached[0].command;
    assert_eq!(
        said, "aterm --headless --working-directory /w/\\u{1b}]52;c;eA==\\u{7}\\r\\n -e …",
        "{c:?}"
    );
    assert!(!said.chars().any(char::is_control), "{said:?}");
    assert!(!said.contains("secret"), "{said:?}");
}

#[test]
fn a_missing_directory_publishes_nothing_and_an_unreadable_one_is_not_measured() {
    let targets = vec![
        (7641, "/r/aterm-7641.sock".to_string()),
        (0, "/x".to_string()),
    ];
    let found = crate::DirOutcome::Found {
        path: PathBuf::from("/r"),
        n: 2,
    };
    let (dir, published) = published_in(found, targets).expect("found");
    assert_eq!((dir.as_str(), published), ("/r", BTreeSet::from([7641])));

    let missing = crate::DirOutcome::Missing {
        path: PathBuf::from("/scratch/run/aterm"),
        why: "resolved from $XDG_RUNTIME_DIR".to_string(),
    };
    let (dir, published) = published_in(missing, Vec::new()).expect("missing is an answer");
    assert!(published.is_empty());
    let c = census_of(&Fake::with(table()), 100, &published, dir, DOOR).expect("census");
    let pids: Vec<u32> = c.unreached.iter().map(|u| u.pid).collect();
    assert_eq!(
        pids,
        [67548, 7641],
        "every instance is unreached from there"
    );

    let unreadable = crate::DirOutcome::Unreadable {
        path: PathBuf::from("/r"),
        err: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };
    published_in(unreadable, Vec::new()).expect_err("not measured");
    published_in(crate::DirOutcome::Unresolvable, Vec::new()).expect_err("not measured");
}

#[test]
fn a_published_foreign_instance_counts_as_reached() {
    let targets = vec![
        (7641, "/r/aterm-7641.sock".to_string()),
        (67548, "/var/folders/xy/T/at.Q1/a.sock".to_string()),
    ];
    let found = crate::DirOutcome::Found {
        path: PathBuf::from("/r"),
        n: 2,
    };
    let (dir, published) = published_in(found, targets).expect("found");
    assert!(published.contains(&67548), "the graph entry publishes it");
    let c = census_of(&Fake::with(table()), 100, &published, dir, DOOR).expect("census");
    assert_eq!((c.instances, c.unreached.len()), (2, 0));
}

#[test]
fn ages_are_words_and_the_oldest_comes_first() {
    assert_eq!(age_words(11 * 86_400 + 22 * 3_600 + 5), "11d 22h");
    assert_eq!(age_words(3 * 3_600 + 5 * 60), "3h 05m");
    assert_eq!(age_words(4 * 60 + 12), "4m 12s");
    assert_eq!(age_words(42), "42s");

    let mut t = table();
    t.push(proc(
        68000,
        50,
        5,
        ATERM,
        &["aterm", "--headless", "--no-control-sock"],
    ));
    let c = census(&Fake::with(t), &[7641]);
    let pids: Vec<u32> = c.unreached.iter().map(|u| u.pid).collect();
    assert_eq!(pids, [67548, 68000]);
    assert!(c.unreached[1].socketless);
    assert_eq!(c.unreached[1].age, "5s");
}

#[test]
fn a_process_born_as_the_table_is_read_has_an_age() {
    let clock = std::cell::Cell::new(NOW);
    let read = |pid: u32| {
        clock.set(clock.get() + 1);
        Listed {
            pid,
            ppid: Some(1),
            birth: Some(ProcessBirth {
                seconds: clock.get(),
                microseconds: 0,
            }),
            age_s: None,
        }
    };
    let rows = read_then_aged(vec![7, 8], read, || Some(clock.get()));
    let ages: Vec<Option<u64>> = rows.iter().map(|r| r.age_s).collect();
    assert_eq!(ages, [Some(1), Some(0)], "{rows:?}");
}

/// Of the link brokers launchd adopted, the one serving the fabric's socket is supervised;
/// any other is unsupervised and takes the token the leftover view offers it. A broker whose
/// launcher lives, and an adopted program that is no broker, are neither. NEGATIVE CONTROL:
/// drop the serving pid's check, and the fabric's own broker reads unsupervised.
#[test]
fn the_supervised_broker_is_not_unsupervised() {
    let program = |pid: u32, name: &str, adopted: bool| Program {
        pid,
        name: name.to_string(),
        age: "3h 05m".to_string(),
        adopted,
        build: false,
        deleted: false,
        unsupervised: false,
        token: None,
    };
    let mut c = Census {
        dir: "/run/aterm".to_string(),
        instances: 1,
        unreached: Vec::new(),
        programs: vec![
            program(600, BROKER, true),
            program(601, BROKER, true),
            program(602, BROKER, false),
            program(603, "aterm link serve", true),
        ],
    };
    let asked = RefCell::new(Vec::new());
    supervise(&mut c, Some(600), &|pid| {
        asked.borrow_mut().push(pid);
        Some(format!("{pid}@1792098840004410:331"))
    });
    let marked: Vec<(u32, bool, Option<&str>)> = c
        .programs
        .iter()
        .map(|p| (p.pid, p.unsupervised, p.token.as_deref()))
        .collect();
    assert_eq!(
        marked,
        [
            (600, false, None),
            (601, true, Some("601@1792098840004410:331")),
            (602, false, None),
            (603, false, None),
        ]
    );
    assert_eq!(
        *asked.borrow(),
        [601],
        "only an unsupervised broker is read"
    );

    for p in &mut c.programs {
        p.unsupervised = false;
        p.token = None;
    }
    supervise(&mut c, None, &|_| None);
    let unsupervised: Vec<u32> = c
        .programs
        .iter()
        .filter(|p| p.unsupervised)
        .map(|p| p.pid)
        .collect();
    assert_eq!(unsupervised, [600, 601], "no broker serves the fabric");
    assert!(c.programs.iter().all(|p| p.token.is_none()));
}

/// Live: the shipped table lists this process with its real parent, and the kernel names
/// its executable, a test binary that is not aterm.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_live_table_lists_this_process() {
    let me = std::process::id();
    let listed = LiveKernel.listed().expect("the table reads");
    let row = listed
        .iter()
        .find(|r| r.pid == me)
        .expect("this process is listed");
    assert_eq!(row.ppid, Some(std::os::unix::process::parent_id()));
    assert!(row.birth.is_some() && row.age_s.is_some(), "{row:?}");
    assert!(
        LiveKernel.stdin_terminal(me).is_some(),
        "this process's standard input reads"
    );
    let image = LiveKernel
        .exe(me)
        .expect("this process's executable is named");
    assert!(!image.deleted, "{image:?}");
    assert!(!is_aterm(&image.path), "{}", image.path.display());
    assert_eq!(LiveKernel.ran(me), Some(true), "this process runs");
}
