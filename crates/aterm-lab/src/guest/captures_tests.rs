// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Each line through the schedule, each milestone as `(at, key)`.
fn run(schedule: &mut Schedule, lines: &[(Source, &str)]) -> Vec<(u64, String)> {
    lines
        .iter()
        .flat_map(|(source, line)| schedule.see(*source, line))
        .map(|m| (m.at_ms, m.key))
        .collect()
}

/// A first launch with a person: the records as the lane's watches write them, interleaved
/// in the order they came.
fn first_launch() -> Vec<(Source, &'static str)> {
    vec![
        (Source::Census, "census 1000 every=250 windows=2"),
        (Source::Census, "base 1000 10 300 25 1512x33 Window Server"),
        (Source::Census, "base 1000 11 301 0 800x600 Finder"),
        (Source::Watch, "at 1000 absent"),
        (
            Source::Procs,
            "procs 1000 every=50 exe=/Applications/aterm.app/Contents/MacOS/aterm",
        ),
        (Source::Procs, "up 1500 700 start=1490"),
        (Source::Census, "new 2100 40 700 0 900x644 aterm"),
        (Source::Setup, "setup 2200 -"),
        (
            Source::Watch,
            r#"at 2300 {"profile":"default","state":"pending","done":0,"running":1}"#,
        ),
        (Source::Setup, "setup 3200 pending:dock,desktop"),
        (
            Source::Watch,
            r#"at 3400 {"profile":"default","state":"pending","done":1,"running":1}"#,
        ),
        (
            Source::Watch,
            r#"at 3500 {"profile":"default","state":"pending","done":1,"running":2}"#,
        ),
        (
            Source::Watch,
            r#"at 3600 {"profile":"default","state":"pending","done":3,"running":4}"#,
        ),
        (Source::Census, "new 3700 41 120 0 420x210 nehelper"),
        (
            Source::Setup,
            "setup 4200 pending:dock,fda,lan;unread:capslock",
        ),
        (Source::Census, "new 4800 42 130 0 700x600 System Settings"),
        (Source::Census, "new 5000 43 130 8 400x200 System Settings"),
        (Source::Census, "gone 9000 41"),
        (Source::Census, "gone 12000 43"),
        (
            Source::Setup,
            "setup 13200 pending:fda;needs:fda;unread:dock",
        ),
        (Source::Census, "gone 15000 42"),
        (Source::Procs, "down 15100 700"),
        (Source::Procs, "up 15600 701 start=15590"),
        (Source::Setup, "setup 18200 pending"),
        (Source::Census, "new 19000 44 140 0 400x300 SecurityAgent"),
        (Source::Census, "gone 25000 44"),
        (Source::Setup, "setup 26200 pending:fda"),
        (
            Source::Watch,
            r#"at 27000 {"profile":"default","state":"pending","done":7,"running":8}"#,
        ),
        (Source::Setup, "setup 30200 done"),
    ]
}

#[test]
fn each_milestone_of_a_first_launch_is_named_once_in_the_order_it_came() {
    let mut schedule = Schedule::default();
    assert_eq!(
        run(&mut schedule, &first_launch()),
        [
            (2300, "stage-workstation"),
            (3500, "stage-spotlight"),
            (3600, "stage-permissions"),
            (3700, "before-nehelper"),
            (4200, "needs-fda"),
            (4200, "needs-lan"),
            (4800, "before-system-settings"),
            (9000, "after-nehelper"),
            (13200, "cleared-lan"),
            (15000, "after-system-settings"),
            (15600, "relaunch"),
            (18200, "cleared-fda"),
            (19000, "before-securityagent"),
            (25000, "after-securityagent"),
            (26200, "needs-fda-2"),
            (27000, "stage-secrets"),
            (30200, "cleared-fda-2"),
        ]
        .map(|(at, key)| (at, key.to_owned()))
    );
}

/// The negative control: lines that are no milestone name none, so the schedule above is
/// not one that names every line.
#[test]
fn lines_that_are_no_milestone_name_none_and_the_same_lines_with_milestones_do() {
    let mut schedule = Schedule::default();
    let quiet = [
        // The app's own windows, the desktop's, and a window there before the census began.
        (Source::Census, "new 2100 40 700 0 900x644 aterm"),
        (Source::Census, "new 2150 45 90 0 1512x33 Dock"),
        (Source::Census, "base 1000 46 130 0 700x600 System Settings"),
        (Source::Census, "gone 2200 45"),
        // A window of an owner already on screen.
        (Source::Census, "new 2300 47 130 0 300x100 System Settings"),
        // The same stage looked at twice, the record's other changes, and no stage.
        (
            Source::Watch,
            r#"at 2400 {"state":"pending","done":0,"running":1}"#,
        ),
        (Source::Watch, r#"at 2500 {"state":"pending","done":0}"#),
        (Source::Watch, "at 2600 absent"),
        (Source::Watch, "at 2700 not json"),
        // The first launch's own process, and a process going.
        (Source::Procs, "up 1500 700 start=1490"),
        (Source::Procs, "down 2800 700"),
        // Settings the run applies by itself, and a reading that repeats.
        (Source::Setup, "setup 3000 pending:dock,desktop,capslock"),
        (Source::Setup, "setup 3100 pending:dock,desktop,capslock"),
        (Source::Setup, "setup 3200 pending"),
        // Torn and unknown lines.
        (Source::Setup, "setup"),
        (Source::Census, "new x 47"),
        (Source::Watch, "killed 2900 700 running=1 sent=yes"),
    ];
    let named = run(&mut schedule, &quiet);
    assert_eq!(
        named,
        [(2400, "stage-workstation".to_owned())],
        "only the first look at a running stage is one"
    );
    // The same lines with one milestone each among them do name it: the control can fail.
    let mut schedule = Schedule::default();
    let mut louder = quiet.to_vec();
    louder.push((Source::Census, "new 3300 48 120 0 420x210 nehelper"));
    louder.push((Source::Procs, "up 3400 702 start=3390"));
    louder.push((Source::Setup, "setup 3500 pending:dock,repos,sync-peers"));
    assert_eq!(
        run(&mut schedule, &louder)
            .into_iter()
            .map(|(_, key)| key)
            .collect::<Vec<_>>(),
        [
            "stage-workstation",
            "before-nehelper",
            "relaunch",
            "needs-repos",
            "needs-sync-peers"
        ]
    );
}

#[test]
fn a_window_there_at_the_census_start_ends_its_stage_when_it_goes() {
    let mut schedule = Schedule::default();
    let lines = [
        (Source::Census, "base 1000 46 130 0 700x600 System Settings"),
        (Source::Census, "gone 4000 46"),
        (Source::Census, "new 5000 49 130 0 700x600 System Settings"),
    ];
    assert_eq!(
        run(&mut schedule, &lines),
        [
            (4000, "after-system-settings".to_owned()),
            (5000, "before-system-settings".to_owned())
        ]
    );
}

#[test]
fn only_a_relaunch_is_asked_until_the_app_answers() {
    let mut schedule = Schedule::default();
    let mut all = Vec::new();
    for (source, line) in first_launch() {
        all.extend(schedule.see(source, line));
    }
    let waiting: Vec<&str> = all
        .iter()
        .filter(|m| m.until_answered)
        .map(|m| m.key.as_str())
        .collect();
    assert_eq!(waiting, ["relaunch"]);
}

/// One look's milestones are one frame: each its own line, all naming the one file; a failed
/// capture is a failure line for each, naming no file.
#[test]
fn every_milestone_of_one_capture_names_its_one_frame() {
    let batch: Vec<Milestone> = [(5492, "stage-permissions-2"), (6835, "before-nehelper")]
        .into_iter()
        .map(|(at_ms, key)| Milestone {
            at_ms,
            key: key.to_owned(),
            why: format!("{key} came"),
            until_answered: false,
        })
        .collect();
    assert_eq!(
        record_lines(&batch, Ok(("04-stage-permissions-2.png", "1248x964")), 8669),
        [
            (
                "capture",
                "stage-permissions-2 04-stage-permissions-2.png 1248x964 trigger=5492 \
                 begun=8669 stage-permissions-2 came"
            ),
            (
                "capture",
                "before-nehelper 04-stage-permissions-2.png 1248x964 trigger=6835 \
                 begun=8669 before-nehelper came"
            ),
        ]
        .map(|(kind, rest)| (kind, rest.to_owned()))
    );
    assert_eq!(
        record_lines(&batch, Err("aterm ctl image:\n  refused"), 8669),
        [
            (
                "capture-error",
                "stage-permissions-2 trigger=5492 aterm ctl image: refused"
            ),
            (
                "capture-error",
                "before-nehelper trigger=6835 aterm ctl image: refused"
            ),
        ]
        .map(|(kind, rest)| (kind, rest.to_owned()))
    );
    assert!(record_lines(&[], Ok(("05-x.png", "1x1")), 1).is_empty());
}

#[test]
fn a_key_is_a_file_names_word() {
    assert_eq!(slug("System Settings"), "system-settings");
    assert_eq!(
        slug("Install Command Line Developer Tools"),
        "install-command-line-developer-tools"
    );
    assert_eq!(slug("sync-peers"), "sync-peers");
    assert_eq!(slug("  a/b  "), "a-b");
}

#[test]
fn only_an_ok_reply_with_a_size_and_a_path_is_a_frame() {
    assert_eq!(
        image_reply("OK 1800 1288 /Users//admin/Library/aterm/images/nux-01.png\n"),
        Some((
            "1800x1288".to_owned(),
            PathBuf::from("/Users//admin/Library/aterm/images/nux-01.png")
        ))
    );
    assert_eq!(image_reply("OK 0 0 /x.png\n"), None, "nothing captured");
    assert_eq!(image_reply("ERR image: no window to capture\n"), None);
    assert_eq!(image_reply("OK 10 10 relative.png\n"), None);
}

#[test]
fn a_follow_reads_whole_lines_once_and_again_from_the_start_of_a_fresh_file() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("record.txt");
    std::fs::write(&path, "at 1 a\nat 2 b").expect("write");
    let mut follow = Follow::new(&path.to_string_lossy());
    assert_eq!(follow.lines(), ["at 1 a"]);
    assert!(follow.lines().is_empty(), "a line is read once");
    std::fs::write(&path, "at 1 a\nat 2 b\nat 3 c\n").expect("write");
    assert_eq!(
        follow.lines(),
        ["at 2 b", "at 3 c"],
        "the torn line, once whole"
    );
    std::fs::write(&path, "at 9 z\n").expect("write");
    assert_eq!(
        follow.lines(),
        ["at 9 z"],
        "a file made fresh is read from its start"
    );
}
