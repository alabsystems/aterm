// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

/// The injected clock: the second the first pass runs.
const NOW: u64 = 1_800_000_000;

/// The wall second the injected boot began.
const BOOTED: u64 = NOW - 3600;

/// The injected readings at wall second `wall`, in one boot that has run since [`BOOTED`].
fn at(wall: u64) -> Now {
    Now {
        wall,
        boot: "b0".to_owned(),
        since_boot: wall - BOOTED,
    }
}

/// [`pass`] at the readings [`at`] `wall`.
fn pass_at(
    root: &Path,
    life: &dyn Fn(&str) -> TabLife,
    known_live: &[String],
    groups: &dyn Fn() -> Option<Vec<BTreeSet<String>>>,
    wall: u64,
) -> Pass {
    pass(root, life, known_live, groups, &at(wall))
}

/// A session id of the legacy form, numbered.
fn sid(n: u32) -> String {
    format!("s-{n:020x}")
}

/// A state root with a private drive folder.
fn root() -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let drive = dir.path().join("drive");
    std::fs::create_dir(&drive).unwrap();
    std::fs::set_permissions(&drive, std::fs::Permissions::from_mode(0o700)).unwrap();
    (dir, drive)
}

/// Write `name` in `drive` and set its time to unix second `at`.
fn file_at(drive: &Path, name: &str, body: &str, at: u64) {
    let path = drive.join(name);
    std::fs::write(&path, body).unwrap();
    touch(&path, at);
}

fn touch(path: &Path, at: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(at))
        .unwrap();
}

/// Every file a session's stem leaves, each last written at `at`.
fn session_files(drive: &Path, stem: &str, at: u64) {
    for kind in [
        ".jsonl",
        ".journal.jsonl",
        ".jsonl.lock",
        ".journal.jsonl.lock",
    ] {
        file_at(drive, &format!("{stem}{kind}"), "{}\n", at);
    }
}

fn names(drive: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(drive)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// `life` from the stems gone and those unknown; every other stem is live.
fn life_of(gone: &[String], unknown: &[String]) -> impl Fn(&str) -> TabLife {
    let (gone, unknown) = (gone.to_vec(), unknown.to_vec());
    move |tab: &str| {
        if gone.iter().any(|t| t == tab) {
            TabLife::Gone
        } else if unknown.iter().any(|t| t == tab) {
            TabLife::Unknown
        } else {
            TabLife::Live
        }
    }
}

fn no_groups() -> Option<Vec<BTreeSet<String>>> {
    Some(Vec::new())
}

/// A session gone from the window's list is marked at the first pass that sees it gone, with
/// that second in a marker of mode 0600, and its ledger, journal, both locks and note go
/// [`STATE_KEEP_S`] after it; a live session's files stay.
/// NEGATIVE CONTROLS: a pass one second short of the keep collects nothing (a rule on the files'
/// own age, ten days here, collects at the first pass, before the session had been gone at all).
#[test]
fn a_gone_session_is_marked_then_collected_a_keep_after() {
    let (dir, drive) = root();
    let (gone, live) = (sid(1), sid(2));
    let old = NOW - 10 * 24 * 3600;
    session_files(&drive, &gone, old);
    file_at(&drive, &format!("{gone}.earlier-tabs"), "", old);
    file_at(&drive, &format!("{live}.jsonl"), "{}\n", old);
    let life = life_of(std::slice::from_ref(&gone), &[]);
    let pass_when = |now| pass_at(dir.path(), &life, &[], &no_groups, now);
    let first = pass_when(NOW);
    assert_eq!(first.marked, [gone.clone()]);
    assert!(first.collected.is_empty());
    let marker = drive.join(format!("{gone}{LEFT}"));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), at(NOW).text());
    let mode = std::fs::metadata(&marker).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(pass_when(NOW + STATE_KEEP_S - 1), Pass::default());
    let due = pass_when(NOW + STATE_KEEP_S);
    assert_eq!(due.collected, [gone.clone()]);
    assert_eq!(names(&drive), [format!("{live}.jsonl")]);
    // A later pass finds nothing of it to mark again.
    assert_eq!(pass_when(NOW + STATE_KEEP_S + 1), Pass::default());
}

/// A sighting that is not of a gone session removes the marker, so the wait starts again from
/// the next sighting of its departure; so does a write to its files after the marker.
/// NEGATIVE CONTROLS: a marker kept through a live or unknown sighting would let the first
/// marker's second collect at `NOW + STATE_KEEP_S`, and one kept past a write would collect a
/// session that came back.
#[test]
fn a_session_seen_again_or_written_since_starts_its_wait_again() {
    for seen in [TabLife::Live, TabLife::Unknown] {
        let (dir, drive) = root();
        let stem = sid(7);
        session_files(&drive, &stem, NOW - 60);
        let gone = life_of(std::slice::from_ref(&stem), &[]);
        let seen_life = move |_: &str| seen;
        assert_eq!(
            pass_at(dir.path(), &gone, &[], &no_groups, NOW).marked,
            [stem.clone()]
        );
        assert_eq!(
            pass_at(dir.path(), &seen_life, &[], &no_groups, NOW + 10).unmarked,
            [stem.clone()]
        );
        let later = NOW + 20;
        assert_eq!(
            pass_at(dir.path(), &gone, &[], &no_groups, later).marked,
            [stem.clone()]
        );
        let at_first_keep = pass_at(dir.path(), &gone, &[], &no_groups, NOW + STATE_KEEP_S);
        assert!(at_first_keep.collected.is_empty(), "{seen:?}");
        assert_eq!(
            pass_at(dir.path(), &gone, &[], &no_groups, later + STATE_KEEP_S).collected,
            [stem.clone()]
        );
    }
    let (dir, drive) = root();
    let stem = sid(8);
    session_files(&drive, &stem, NOW - 60);
    let gone = life_of(std::slice::from_ref(&stem), &[]);
    assert_eq!(
        pass_at(dir.path(), &gone, &[], &no_groups, NOW).marked,
        [stem.clone()]
    );
    touch(&drive.join(format!("{stem}.journal.jsonl")), NOW + 1);
    let rewritten = NOW + STATE_KEEP_S;
    let written = pass_at(dir.path(), &gone, &[], &no_groups, rewritten);
    assert_eq!(written.marked, [stem.clone()]);
    assert!(written.collected.is_empty());
    assert!(drive.join(format!("{stem}.jsonl")).is_file());
    let marker = drive.join(format!("{stem}{LEFT}"));
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        at(rewritten).text()
    );
    // A marker that holds no readings, or readings after this pass's, is written again now.
    for left in ["soon".to_owned(), at(NOW + 99).text()] {
        std::fs::write(&marker, left).unwrap();
        assert_eq!(
            pass_at(dir.path(), &gone, &[], &no_groups, NOW + 2).marked,
            [stem.clone()]
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            at(NOW + 2).text()
        );
    }
}

/// A marker's keep is counted on the wall clock and, within the boot that wrote it, on the clock
/// that counts through sleep as well: a wall clock set forward ages no marker. Across a boot the
/// wall clock alone decides. A marker of the wall second alone (the earlier format), or one whose
/// boot clock is ahead of this pass's in the same boot, is read as written now.
/// NEGATIVE CONTROLS: a wall-clock-only age collects at a wall keep while the boot clock shows a
/// second less; the earlier format's second, thirty days old, would collect at the first pass.
#[test]
fn the_keep_is_counted_through_sleep_within_a_boot_and_on_the_wall_across_one() {
    let (dir, drive) = root();
    let stem = sid(9);
    session_files(&drive, &stem, NOW - 60);
    let gone = life_of(std::slice::from_ref(&stem), &[]);
    let run = |now: &Now| pass(dir.path(), &gone, &[], &no_groups, now);
    let marker = drive.join(format!("{stem}{LEFT}"));
    assert_eq!(run(&at(NOW)).marked, [stem.clone()]);
    let set_forward = Now {
        since_boot: at(NOW).since_boot + STATE_KEEP_S - 1,
        ..at(NOW + STATE_KEEP_S)
    };
    assert_eq!(run(&set_forward), Pass::default());
    let set_back = Now {
        wall: NOW + STATE_KEEP_S - 1,
        ..at(NOW + STATE_KEEP_S)
    };
    assert_eq!(run(&set_back), Pass::default());
    let rebooted = |wall| Now {
        wall,
        boot: "b1".to_owned(),
        since_boot: 5,
    };
    assert_eq!(run(&rebooted(NOW + STATE_KEEP_S - 1)), Pass::default());
    assert_eq!(run(&rebooted(NOW + STATE_KEEP_S)).collected, [stem.clone()]);

    session_files(&drive, &stem, NOW - 60);
    std::fs::write(&marker, (NOW - 30 * 24 * 3600).to_string()).unwrap();
    assert_eq!(run(&at(NOW)).marked, [stem.clone()]);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), at(NOW).text());
    assert_eq!(run(&at(NOW + STATE_KEEP_S - 1)), Pass::default());
    let ahead = Now {
        since_boot: at(NOW).since_boot - 1,
        ..at(NOW + 1)
    };
    assert_eq!(run(&ahead).marked, [stem.clone()]);
    assert_eq!(run(&at(NOW + 1 + STATE_KEEP_S)).collected, [stem.clone()]);
}

/// A departed stem that a session not gone may still read stays: one an earlier-tabs note of a
/// live tab names, one named in turn by that stem's own note, and one a group pairs with a live
/// or an unknown tab; one no reader reaches goes. Where the groups cannot be read, nothing goes.
/// NEGATIVE CONTROL: a collector without the needed set removes all five departed stems.
#[test]
fn a_departed_stem_a_session_not_gone_may_read_is_kept() {
    let (dir, drive) = root();
    let [live, noted, chained, paired, paired_unknown, unknown, lone] =
        [1, 2, 3, 4, 5, 6, 7].map(sid);
    let departed = [&noted, &chained, &paired, &paired_unknown, &lone].map(String::clone);
    for stem in &departed {
        session_files(&drive, stem, NOW - 60);
    }
    file_at(
        &drive,
        &format!("{live}.earlier-tabs"),
        &format!("{noted}\n"),
        NOW - 60,
    );
    file_at(
        &drive,
        &format!("{noted}.earlier-tabs"),
        &format!("{chained}\n"),
        NOW - 60,
    );
    let life = life_of(&departed, std::slice::from_ref(&unknown));
    let groups = || {
        Some(vec![
            BTreeSet::from([live.clone(), paired.clone()]),
            BTreeSet::from([unknown.clone(), paired_unknown.clone()]),
        ])
    };
    let unread = || -> Option<Vec<BTreeSet<String>>> { None };
    let known = [live.clone()];
    assert_eq!(
        pass_at(dir.path(), &life, &known, &groups, NOW)
            .marked
            .len(),
        5
    );
    let later = NOW + STATE_KEEP_S;
    assert!(
        pass_at(dir.path(), &life, &known, &unread, later)
            .collected
            .is_empty()
    );
    assert_eq!(
        pass_at(dir.path(), &life, &known, &groups, later).collected,
        [lone.clone()]
    );
    for stem in [&noted, &chained, &paired, &paired_unknown] {
        assert!(drive.join(format!("{stem}.jsonl")).is_file(), "{stem}");
    }
    assert!(!drive.join(format!("{lone}.jsonl")).exists());
}

/// A stem with an entry that is no regular file is neither marked nor collected, and a name no
/// session holds (`self`, any other word) is never read as one.
/// NEGATIVE CONTROL: the same gone stem with regular files is marked.
#[test]
fn a_stem_with_a_link_or_no_session_name_is_left_alone() {
    let (dir, drive) = root();
    let (linked, plain) = (sid(1), sid(2));
    let target = dir.path().join("target");
    std::fs::write(&target, "kept\n").unwrap();
    std::os::unix::fs::symlink(&target, drive.join(format!("{linked}.journal.jsonl"))).unwrap();
    file_at(&drive, &format!("{linked}.jsonl"), "{}\n", NOW - 60);
    file_at(&drive, &format!("{plain}.jsonl"), "{}\n", NOW - 60);
    for other in [
        "self.jsonl",
        "self.journal.jsonl",
        "notes.jsonl",
        "x.goal-hold.json",
    ] {
        file_at(&drive, other, "{}\n", NOW - 60);
    }
    let everything_gone = |_: &str| TabLife::Gone;
    let first = pass_at(dir.path(), &everything_gone, &[], &no_groups, NOW);
    assert_eq!(first.marked, [plain.clone()]);
    let later = pass_at(
        dir.path(),
        &everything_gone,
        &[],
        &no_groups,
        NOW + STATE_KEEP_S,
    );
    assert_eq!(later.collected, [plain]);
    assert_eq!(
        names(&drive),
        [
            "notes.jsonl".to_owned(),
            format!("{linked}.journal.jsonl"),
            format!("{linked}.jsonl"),
            "self.journal.jsonl".to_owned(),
            "self.jsonl".to_owned(),
            "x.goal-hold.json".to_owned(),
        ]
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept\n");
}

/// A record's tabs and the tabs the upgrade ledger pairs with its session are one group; a
/// ledger row of no session (`-`) or of no session id pairs nothing, and a restart stamp beside
/// the records is no record. A ledger that cannot be read, a file under a record's name that
/// this build does not read as one (another build's), and a `*.json` that is no JSON give no
/// groups at all.
/// NEGATIVE CONTROLS: a group of the record alone misses the ledger's tab; a reader that skips
/// a record it cannot read reads groups without that record's tabs.
#[test]
fn groups_pair_a_records_tabs_with_the_ledgers_tabs_of_its_session() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let upgrade = state.join("upgrade");
    std::fs::create_dir_all(&upgrade).unwrap();
    let [now_tab, earlier, ended, ledgered, other, stray] = [1, 2, 3, 4, 5, 6].map(sid);
    let st = St {
        tab: now_tab.clone(),
        tabs: vec![earlier.clone()],
        tabs_until: vec![(ended.clone(), 5)],
        ..St::default()
    };
    std::fs::write(upgrade.join("conv-a.json"), st.to_json()).unwrap();
    let rows = [
        aterm_json::json!({"t": 1, "tab": ledgered, "session": "conv-a"}),
        aterm_json::json!({"t": 2, "tab": other, "session": "conv-b"}),
        aterm_json::json!({"t": 3, "tab": stray, "session": "-"}),
        aterm_json::json!({"t": 4, "tab": "self", "session": "conv-b"}),
    ];
    let ledger: String = rows.iter().map(|row| format!("{row}\n")).collect();
    std::fs::write(upgrade.join("ledger.jsonl"), ledger).unwrap();
    std::fs::write(
        upgrade.join(format!("asked-{now_tab}.json")),
        aterm_json::json!({"at": 5}).to_string(),
    )
    .unwrap();
    let paired = groups(&state).unwrap();
    assert_eq!(
        paired,
        [
            BTreeSet::from([now_tab, earlier, ended, ledgered]),
            BTreeSet::from([other]),
        ]
    );
    for (name, text) in [
        ("0123abcd.json", r#"{"phase": "from-a-later-build"}"#),
        ("notes.json", "{"),
    ] {
        std::fs::write(upgrade.join(name), text).unwrap();
        assert_eq!(groups(&state), None, "{name}");
        std::fs::remove_file(upgrade.join(name)).unwrap();
        assert!(groups(&state).is_some(), "{name}");
    }
    std::fs::remove_file(upgrade.join("ledger.jsonl")).unwrap();
    std::fs::create_dir(upgrade.join("ledger.jsonl")).unwrap();
    assert_eq!(groups(&state), None);
    assert_eq!(groups(&dir.path().join("absent")), Some(Vec::new()));
}

/// A dead holder's release-line record whose lease lapsed, at `old`.
fn lapsed_record(state: &Path, name: &str, old: u64) {
    let registry = state.join(crate::release_progress::DIRECTORY);
    std::fs::create_dir_all(&registry).unwrap();
    let pid = std::process::id();
    let (boot, start) = crate::release_progress::identity(pid).unwrap();
    let record = aterm_json::json!({"holder": {"pid": pid, "boot": boot, "start": start + 1},
        "at": old, "rows": []});
    let path = registry.join(name);
    std::fs::write(&path, record.to_string()).unwrap();
    touch(&path, old);
}

fn opts(dir: &Path) -> Opts {
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        account_roster: None,
        sock: None,
        token: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: Some(dir.join("aterm")),
        ask: false,
        usage_tap: None,
        instances: super::super::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        tell: super::super::Tell::Off,
        press: None,
        timings: crate::harness::Timings::FAST,
    }
}

/// The window's own sweep collects, an hour after the sweep that armed its gate and then at
/// most once an hour; a dry run, a background sweep and a sweep that knows no live tabs (the
/// CLI's) collect nothing and leave the gate as it is.
/// NEGATIVE CONTROLS: the lapsed record each refused sweep leaves is removed by the first due
/// one, and one written inside the hour waits for the hour's end.
#[test]
fn only_the_windows_own_sweep_collects_and_at_most_hourly() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let old = NOW - 10 * 24 * 3600;
    lapsed_record(&opts.state, "a.json", old);
    let record = opts
        .state
        .join(crate::release_progress::DIRECTORY)
        .join("a.json");
    let tabs = [LiveTab {
        sid: sid(1),
        fgpgid: None,
    }];
    let gate = AtomicU64::new(NOW - COLLECT_EVERY_S);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let background = Opts {
        background: true,
        ..opts.clone()
    };
    sweep(&dry, Some(&tabs), NOW, &gate);
    sweep(&background, Some(&tabs), NOW, &gate);
    sweep(&opts, None, NOW, &gate);
    assert!(record.is_file());
    assert_eq!(gate.load(Ordering::SeqCst), NOW - COLLECT_EVERY_S);
    sweep(&opts, Some(&tabs), NOW, &gate);
    assert!(!record.exists());
    lapsed_record(&opts.state, "a.json", old);
    sweep(&opts, Some(&tabs), NOW + COLLECT_EVERY_S - 1, &gate);
    assert!(record.is_file());
    sweep(&opts, Some(&tabs), NOW + COLLECT_EVERY_S, &gate);
    assert!(!record.exists());
}

/// The gate's first sweep arms it and collects nothing; then it admits one collection an hour,
/// and one of two at the same second. A clock that went back arms it again.
/// NEGATIVE CONTROL: a gate that admits its first sweep collects while a starting window's tabs
/// may not yet stand in the graph.
#[test]
fn the_collection_gate_admits_one_an_hour_after_it_is_armed() {
    let gate = AtomicU64::new(0);
    assert!(!due(&gate, NOW));
    assert!(!due(&gate, NOW + COLLECT_EVERY_S - 1));
    assert!(due(&gate, NOW + COLLECT_EVERY_S));
    assert!(!due(&gate, NOW + COLLECT_EVERY_S));
    assert!(!due(&gate, NOW));
    assert!(!due(&gate, NOW + COLLECT_EVERY_S - 1));
    assert!(due(&gate, NOW + COLLECT_EVERY_S));
}

/// A live tab's earlier-tabs note that is there but does not read whole (a dangling link, bytes
/// that are not UTF-8) may name any departed stem, so that pass collects nothing.
/// NEGATIVE CONTROLS: a reader that takes such a note for an empty one collects both departed
/// stems; the note rewritten to name one keeps that one and collects the other.
#[test]
fn a_note_that_does_not_read_whole_collects_nothing() {
    for unread in ["dangling link", "not UTF-8"] {
        let (dir, drive) = root();
        let [live, noted, lone] = [1, 2, 3].map(sid);
        for stem in [&noted, &lone] {
            session_files(&drive, stem, NOW - 60);
        }
        let departed = [noted.clone(), lone.clone()];
        let life = life_of(&departed, &[]);
        let known = [live.clone()];
        let first = pass_at(dir.path(), &life, &known, &no_groups, NOW);
        assert_eq!(first.marked.len(), 2);
        let note = drive.join(format!("{live}.earlier-tabs"));
        if unread == "dangling link" {
            std::os::unix::fs::symlink(dir.path().join("absent"), &note).unwrap();
        } else {
            std::fs::write(&note, [0xff, 0xfe, b'\n']).unwrap();
        }
        let later = NOW + STATE_KEEP_S;
        let refused = pass_at(dir.path(), &life, &known, &no_groups, later);
        assert!(refused.collected.is_empty(), "{unread}");
        std::fs::remove_file(&note).unwrap();
        std::fs::write(&note, format!("{noted}\n")).unwrap();
        let read = pass_at(dir.path(), &life, &known, &no_groups, later);
        assert_eq!(read.collected, [lone.clone()], "{unread}");
        assert!(drive.join(format!("{noted}.jsonl")).is_file());
    }
}

/// A graph folder of `tabs` named `name` under `dir`.
fn graph_of(dir: &Path, name: &str, tabs: &[&String]) -> PathBuf {
    let graph = dir.join(name);
    std::fs::create_dir(&graph).unwrap();
    for tab in tabs {
        std::fs::write(graph.join(tab.as_str()), "sock x\n").unwrap();
    }
    graph
}

/// A record, under the state root `state`, of this process at a start it did not have: on this
/// machine and in this pid namespace, a process this kernel proves ended, publishing in
/// `graphs`.
fn ended_record(state: &Path, graphs: &[&Path]) -> PathBuf {
    ended_record_in(state, graphs, aterm_uds::process::pid_namespace())
}

/// [`ended_record`] in the pid namespace `namespace`.
fn ended_record_in(state: &Path, graphs: &[&Path], namespace: Option<String>) -> PathBuf {
    let pid = std::process::id();
    let (boot, start) = crate::release_progress::identity(pid).unwrap();
    let graphs: Vec<&str> = graphs.iter().map(|g| g.to_str().unwrap()).collect();
    let host = aterm_uds::process::host_id().unwrap_or_else(|| {
        aterm_spec::host::could_not_run("this host names no machine for a process")
    });
    let body = aterm_json::json!({"pid": pid, "boot": boot, "start": start + 1,
        "host": host, "namespace": namespace, "graphs": graphs});
    let path = graph_records::drive_folder(state).join(format!("{pid}-{}.json", start + 1));
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

/// [`ended_record`] in the format written before records named a place: no host, no namespace.
fn placeless_record(state: &Path, graphs: &[&Path]) -> PathBuf {
    let pid = std::process::id();
    let (boot, start) = crate::release_progress::identity(pid).unwrap();
    let graphs: Vec<&str> = graphs.iter().map(|g| g.to_str().unwrap()).collect();
    let body = aterm_json::json!({"pid": pid, "boot": boot, "start": start + 1, "graphs": graphs});
    let path = graph_records::drive_folder(state).join(format!("{pid}-{}.json", start + 1));
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

/// Set the modification time of the record at `path` to wall second `at`.
fn written_at(path: &Path, at: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| {
            file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(at))
        })
        .unwrap();
}

/// A tab this window's graph does not hold is live where another running window of the same
/// state root publishes it in a graph of its own (that window stood in for by this test
/// process's record), while a tab only a window that ended published goes a keep after it was
/// first seen gone, and that window's record is removed.
/// NEGATIVE CONTROL: the single-graph read (this window's graph alone) marks the other window's
/// tab and collects it a keep later.
#[test]
fn a_tab_another_running_window_publishes_is_kept() {
    let (dir, drive) = root();
    let [mine, theirs, ended] = [1, 2, 3].map(sid);
    for stem in [&theirs, &ended] {
        session_files(&drive, stem, NOW - 60);
    }
    let own = graph_of(dir.path(), "own", &[&mine]);
    let other = graph_of(dir.path(), "other", &[&theirs]);
    let gone_window = graph_of(dir.path(), "gone", &[&ended]);
    graph_records::record(
        &graph_records::drive_folder(dir.path()),
        std::slice::from_ref(&other),
    )
    .unwrap();
    let dead = ended_record(dir.path(), &[&gone_window]);
    let known = [mine.clone()];
    let own_life = registry_life(Some(&own), &known);
    let across = |wall| collect_across(dir.path(), &own_life, &known, &no_groups, &at(wall));
    assert_eq!(across(NOW).marked, [ended.clone()]);
    assert!(!dead.exists());
    assert_eq!(across(NOW + STATE_KEEP_S).collected, [ended]);
    assert!(drive.join(format!("{theirs}.jsonl")).is_file());

    let single = |wall| pass_at(dir.path(), &own_life, &known, &no_groups, wall);
    assert_eq!(single(NOW + STATE_KEEP_S).marked, [theirs.clone()]);
    assert_eq!(single(NOW + 2 * STATE_KEEP_S).collected, [theirs.clone()]);
    assert!(!drive.join(format!("{theirs}.jsonl")).exists());
}

/// Where a running window's graph cannot be read for a tab, or a record written a minute before
/// the pass cannot be read, every tab this window's graph does not hold is unknown: nothing is
/// collected, and the marker goes so the wait starts again.
/// NEGATIVE CONTROL: once both read again, the same departed stem is marked and collected a
/// keep later.
#[test]
fn an_unreadable_record_or_recorded_graph_collects_nothing() {
    let (dir, drive) = root();
    let [mine, departed] = [1, 2].map(sid);
    session_files(&drive, &departed, NOW - 60);
    let own = graph_of(dir.path(), "own", &[&mine]);
    let other = graph_of(dir.path(), "other", &[]);
    graph_records::record(
        &graph_records::drive_folder(dir.path()),
        std::slice::from_ref(&other),
    )
    .unwrap();
    let known = [mine.clone()];
    let own_life = registry_life(Some(&own), &known);
    let across = |wall| collect_across(dir.path(), &own_life, &known, &no_groups, &at(wall));
    assert_eq!(across(NOW).marked, [departed.clone()]);

    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unread_graph = across(NOW + STATE_KEEP_S);
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(unread_graph.collected.is_empty());
    assert_eq!(unread_graph.unmarked, [departed.clone()]);
    let marked_again = NOW + STATE_KEEP_S + 1;
    assert_eq!(across(marked_again).marked, [departed.clone()]);

    let garbled = graph_records::drive_folder(dir.path()).join("7-8.json");
    std::fs::write(&garbled, "{").unwrap();
    written_at(&garbled, marked_again + STATE_KEEP_S - 60);
    let unread_record = across(marked_again + STATE_KEEP_S);
    assert!(unread_record.collected.is_empty());
    assert_eq!(unread_record.unmarked, [departed.clone()]);
    std::fs::remove_file(&garbled).unwrap();

    let read_again = marked_again + STATE_KEEP_S + 1;
    assert_eq!(across(read_again).marked, [departed.clone()]);
    assert_eq!(across(read_again + STATE_KEEP_S).collected, [departed]);
}

/// A running window's record from another pid namespace (a container that mounts this home),
/// written a minute before the pass, whose pid this kernel would read as ended (here, at
/// another start), is neither removed nor read as ended: every tab is unknown, so the tab it
/// publishes is not marked, though this window's graph lacks it.
/// NEGATIVE CONTROL: the same record in this process's namespace is removed as ended, and the
/// tab only it published is marked and collected a keep later.
#[test]
fn a_record_of_another_pid_namespace_is_kept_and_collects_nothing() {
    let (dir, drive) = root();
    let [mine, theirs] = [1, 2].map(sid);
    session_files(&drive, &theirs, NOW - 60);
    let own = graph_of(dir.path(), "own", &[&mine]);
    let other = graph_of(dir.path(), "other", &[&theirs]);
    std::fs::create_dir_all(graph_records::drive_folder(dir.path())).unwrap();
    let ours = aterm_uds::process::pid_namespace();
    let elsewhere = format!("{}9", ours.clone().unwrap_or_default());
    let contained = ended_record_in(dir.path(), &[&other], Some(elsewhere));
    written_at(&contained, NOW - 60);
    let known = [mine.clone()];
    let own_life = registry_life(Some(&own), &known);
    let across = |wall| collect_across(dir.path(), &own_life, &known, &no_groups, &at(wall));
    assert_eq!(across(NOW), Pass::default());
    assert!(contained.is_file(), "a running window's record was removed");
    std::fs::remove_file(&contained).unwrap();
    let ended = ended_record_in(dir.path(), &[&other], ours);
    assert_eq!(across(NOW).marked, [theirs.clone()]);
    assert!(!ended.exists());
    assert_eq!(across(NOW + STATE_KEEP_S).collected, [theirs]);
}

/// A record that cannot be told (another pid namespace's, one that names no place, one this
/// process cannot read, one that is no JSON) and that no window refreshed for [`STATE_KEEP_S`] is
/// abandoned: the pass removes it and collects again, the tab only it published marked then and
/// collected a keep later.
/// NEGATIVE CONTROL: one second short of that week the same record stays and the pass marks
/// nothing, as for a running window's.
#[test]
fn a_record_that_cannot_be_told_unwritten_a_week_is_removed_and_collection_resumes() {
    let ours = aterm_uds::process::pid_namespace();
    let elsewhere = format!("{}9", ours.clone().unwrap_or_default());
    let kinds: [(&str, &dyn Fn(&Path, &Path, u64) -> PathBuf); 4] = [
        ("another namespace", &|state, graph, at| {
            let path = ended_record_in(state, &[graph], Some(elsewhere.clone()));
            written_at(&path, at);
            path
        }),
        ("no place", &|state, graph, at| {
            let path = placeless_record(state, &[graph]);
            written_at(&path, at);
            path
        }),
        ("unreadable", &|state, graph, at| {
            let path = ended_record_in(state, &[graph], ours.clone());
            written_at(&path, at);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            path
        }),
        ("no json", &|state, _, at| {
            let path = graph_records::drive_folder(state).join("7-8.json");
            std::fs::write(&path, "{").unwrap();
            written_at(&path, at);
            path
        }),
    ];
    for (what, make) in kinds {
        let (dir, drive) = root();
        let [mine, theirs] = [1, 2].map(sid);
        session_files(&drive, &theirs, NOW - 60);
        let own = graph_of(dir.path(), "own", &[&mine]);
        let other = graph_of(dir.path(), "other", &[&theirs]);
        std::fs::create_dir_all(graph_records::drive_folder(dir.path())).unwrap();
        let record = make(dir.path(), &other, NOW - STATE_KEEP_S + 1);
        let known = [mine.clone()];
        let own_life = registry_life(Some(&own), &known);
        let across = |wall| collect_across(dir.path(), &own_life, &known, &no_groups, &at(wall));
        assert_eq!(across(NOW), Pass::default(), "{what}");
        assert!(record.is_file(), "{what}: a fresh record was removed");
        assert_eq!(across(NOW + 1).marked, [theirs.clone()], "{what}");
        assert!(!record.exists(), "{what}: an abandoned record stayed");
        assert_eq!(across(NOW + 1 + STATE_KEEP_S).collected, [theirs], "{what}");
    }
}
