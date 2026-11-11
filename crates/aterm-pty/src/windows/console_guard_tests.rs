// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn only_a_flag_and_its_arguments_make_a_client() {
    assert_eq!(client_args(&os(&[])), None);
    assert_eq!(client_args(&os(&["--window"])), None);
    assert_eq!(client_args(&os(&["ctl", FLAG])), None);
    assert_eq!(client_args(&os(&["ctl", LEAVE_FLAG])), None);
    assert_eq!(
        client_args(&os(&[FLAG, "120", "30"])),
        Some(Client::Guard {
            cols: 120,
            rows: 30
        })
    );
    assert_eq!(
        client_args(&os(&[LEAVE_FLAG])),
        Some(Client::LeaveAltScreen(None))
    );
    assert_eq!(
        client_args(&os(&[LEAVE_FLAG, "4", "54", "50531b5b4b"])),
        Some(Client::LeaveAltScreen(Some(Carry {
            row: 4,
            col: 54,
            text: "PS\x1b[K".to_string()
        })))
    );
    for bad in [
        &[FLAG][..],
        &[FLAG, "120"],
        &[FLAG, "0", "30"],
        &[FLAG, "+120", "30"],
        &[FLAG, "x", "30"],
        &[FLAG, "1", "2", "3"],
        &[LEAVE_FLAG, "now"],
        &[LEAVE_FLAG, "4", "54"],
        &[LEAVE_FLAG, "4", "54", ""],
        &[LEAVE_FLAG, "4", "54", "5"],
        &[LEAVE_FLAG, "4", "54", "zz"],
        &[LEAVE_FLAG, "4", "54", "+a"],
        &[LEAVE_FLAG, "4", "54", "ff"],
        &[LEAVE_FLAG, "+4", "54", "50"],
        &[LEAVE_FLAG, "-1", "54", "50"],
        &[LEAVE_FLAG, "4", "54", "50", "more"],
    ] {
        assert_eq!(client_args(&os(bad)), Some(Client::Malformed), "{bad:?}");
    }
}

#[test]
fn a_carry_round_trips_through_the_leave_arguments() {
    let text = "\x1b[0m\x1b[r\x1b[30;1H\n\n\x1b[1;1H\x1b[J\x1b[1;1H\x1b[38;2;1;2;3mPS ✳ C:\\> \"q\"\x1b[0m";
    let args = leave_args(Some(((7, 12), text)));
    let argv: Vec<OsString> = args.split(' ').map(OsString::from).collect();
    assert_eq!(
        client_args(&argv),
        Some(Client::LeaveAltScreen(Some(Carry {
            row: 7,
            col: 12,
            text: text.to_string()
        })))
    );
    assert_eq!(leave_args(None), LEAVE_FLAG);
    assert_eq!(leave_args(Some(((0, 0), ""))), LEAVE_FLAG);
    let long = "x".repeat(CARRY_HEX_MAX / 2 + 1);
    assert_eq!(leave_args(Some(((0, 0), &long))), LEAVE_FLAG);
    let longest = "x".repeat(CARRY_HEX_MAX / 2);
    assert_ne!(leave_args(Some(((0, 0), &longest))), LEAVE_FLAG);
}

#[test]
fn a_carry_applies_only_to_a_raw_reader_at_its_own_cursor() {
    let carry = Carry {
        row: 0,
        col: 54,
        text: "PS> ".to_string(),
    };
    assert!(carry_applies(&carry, Some((0, 54)), Some(false)));
    assert!(!carry_applies(&carry, Some((0, 54)), Some(true)), "cmd.exe");
    assert!(!carry_applies(&carry, Some((0, 55)), Some(false)), "moved");
    assert!(!carry_applies(&carry, Some((1, 54)), Some(false)), "moved");
    assert!(!carry_applies(&carry, None, Some(false)));
    assert!(!carry_applies(&carry, Some((0, 54)), None));
}

#[test]
fn the_client_command_lines_quote_the_image() {
    let exe = Path::new(r"C:\Program Files\aterm\aterm.exe");
    let text = |line: Vec<u16>| {
        assert_eq!(line.last(), Some(&0), "NUL-terminated");
        String::from_utf16_lossy(&line[..line.len() - 1])
    };
    assert_eq!(
        text(command_line(exe, 100, 28)),
        format!("\"C:\\Program Files\\aterm\\aterm.exe\" {FLAG} 100 28")
    );
    assert_eq!(
        text(client_command_line(exe, LEAVE_FLAG)),
        format!("\"C:\\Program Files\\aterm\\aterm.exe\" {LEAVE_FLAG}")
    );
    assert_eq!(
        text(client_command_line(exe, &leave_args(Some(((4, 54), "PS"))))),
        format!("\"C:\\Program Files\\aterm\\aterm.exe\" {LEAVE_FLAG} 4 54 5053")
    );
    assert_eq!(
        client_args(&os(&[FLAG, "100", "28"])),
        Some(Client::Guard {
            cols: 100,
            rows: 28
        })
    );
}

#[test]
fn the_leave_resets_the_app_modes_and_ends_with_the_switch() {
    assert!(LEAVE_SEQUENCE.ends_with("\x1b[?1049l"));
    for reset in [
        "\x1b[?1l",
        "\x1b>",
        "\x1b[?1000l",
        "\x1b[?1002l",
        "\x1b[?1003l",
        "\x1b[?1006l",
        "\x1b[?2048l",
        "\x1b[?2031l",
    ] {
        assert!(LEAVE_SEQUENCE.contains(reset), "{reset:?}");
    }
    assert_eq!(LEAVE_SEQUENCE.matches("\x1b[?1049").count(), 1);
    assert!(!LEAVE_SEQUENCE.contains("\x1b[?2004"), "the shell's paste");
}

#[test]
fn the_record_has_the_input_record_layout() {
    let record = ffi::INPUT_RECORD {
        EventType: ffi::MENU_EVENT,
        Event: [0; 4],
    };
    assert_eq!(std::mem::size_of::<ffi::INPUT_RECORD>(), 20);
    let base = std::ptr::addr_of!(record) as usize;
    assert_eq!(std::ptr::addr_of!(record.Event) as usize - base, 4);
    assert_eq!(ffi::MENU_EVENT, 8);
}

#[test]
fn an_empty_job_runs_no_posix_runtime() {
    // SAFETY: NULL attributes and name, a fresh anonymous job, owned below.
    let job = Handle::new(unsafe { ffi::CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) });
    assert_ne!(job.raw(), 0);
    assert!(pids_in(job.raw()).is_empty());
    assert!(!tab_runs_a_posix_runtime(
        &pids_in(job.raw()),
        0,
        || panic!("no root, no tree"),
        |_| None,
        runs_a_posix_runtime
    ));
}

#[test]
fn a_runtime_program_outside_the_job_is_found_below_the_shell() {
    let tree = vec![(4, 0), (10, 4), (11, 10), (12, 11), (13, 12), (20, 4)];
    let runtime = |pid: u32| pid == 12;
    assert!(tab_runs_a_posix_runtime(
        &[10],
        10,
        || tree.clone(),
        |_| None,
        runtime
    ));
    assert!(
        !tab_runs_a_posix_runtime(&[10], 10, Vec::new, |_| None, runtime),
        "the job alone holds no runtime program"
    );
    assert!(tab_runs_a_posix_runtime(
        &[10, 12],
        10,
        || panic!("the job answered"),
        |_| None,
        runtime
    ));
    assert!(
        !tab_runs_a_posix_runtime(&[20], 20, || tree.clone(), |_| None, runtime),
        "another tree"
    );
    assert!(tab_runs_a_posix_runtime(
        &[],
        11,
        || tree.clone(),
        |_| None,
        runtime
    ));
    assert!(!tab_runs_a_posix_runtime(
        &[],
        0,
        || panic!("no root"),
        |_| None,
        runtime
    ));
}

#[test]
fn the_walk_below_a_process_takes_each_once_and_ends_on_a_cycle() {
    let tree = [(10, 4), (11, 10), (12, 11), (14, 10), (20, 4), (0, 0)];
    let mut got = below(10, &tree, |_| None, Unread::AsRecorded);
    got.sort_unstable();
    assert_eq!(got, [11, 12, 14]);
    assert_eq!(
        below(12, &tree, |_| None, Unread::AsRecorded),
        Vec::<u32>::new()
    );
    let cycle = [(5, 6), (6, 5)];
    assert_eq!(below(5, &cycle, |_| None, Unread::AsRecorded), [6]);
    let mut got = below(10, &tree, |_| Some(7), Unread::LeftOut);
    got.sort_unstable();
    assert_eq!(got, [11, 12, 14], "equal starts are one tick, not a reuse");
}

#[test]
fn the_walk_leaves_out_a_process_older_than_its_recorded_parent() {
    let tree = [(10, 4), (30, 10), (31, 30), (11, 10)];
    let born = |pid| match pid {
        10 => Some(100),
        30 => Some(50),
        31 => Some(60),
        11 => Some(120),
        _ => None,
    };
    assert_eq!(below(10, &tree, born, Unread::AsRecorded), [11]);
    assert_eq!(below(10, &tree, born, Unread::LeftOut), [11]);
    let runtime = |pid: u32| pid == 30;
    assert!(!tab_runs_a_posix_runtime(
        &[10],
        10,
        || tree.to_vec(),
        born,
        runtime
    ));
    let unreadable = |pid| (pid != 30).then_some(100);
    let mut got = below(10, &tree, unreadable, Unread::AsRecorded);
    got.sort_unstable();
    assert_eq!(got, [11, 30, 31]);
}

#[test]
fn the_strict_walk_leaves_out_what_it_cannot_vouch_for() {
    let tree = [(10, 4), (30, 10), (31, 30), (11, 10), (12, 11)];
    let unreadable = |pid| (pid != 30).then_some(100);
    let mut got = below(10, &tree, unreadable, Unread::LeftOut);
    got.sort_unstable();
    assert_eq!(got, [11, 12]);
    let no_root = |pid| (pid != 10).then_some(100);
    assert_eq!(
        below(10, &tree, no_root, Unread::LeftOut),
        Vec::<u32>::new()
    );
    let mut got = below(10, &tree, no_root, Unread::AsRecorded);
    got.sort_unstable();
    assert_eq!(got, [11, 12, 30, 31]);
}

#[test]
fn this_process_started_after_its_parent() {
    // SAFETY: plain FFI.
    let me = unsafe { ffi::GetCurrentProcessId() };
    let mine = created_at(me).expect("this process's start reads");
    let parent = process_tree()
        .into_iter()
        .find_map(|(pid, ppid)| (pid == me).then_some(ppid))
        .expect("this process is in the snapshot");
    if let Some(theirs) = created_at(parent) {
        assert!(theirs <= mine, "parent {theirs} after child {mine}");
    }
}

#[test]
fn the_process_tree_holds_this_process() {
    // SAFETY: plain FFI.
    let me = unsafe { ffi::GetCurrentProcessId() };
    assert!(process_tree().iter().any(|&(pid, _)| pid == me));
}

#[test]
fn a_process_image_path_reads_back() {
    // SAFETY: plain FFI.
    let me = unsafe { ffi::GetCurrentProcessId() };
    let image = image_of(me).expect("our own image path");
    assert_eq!(
        image.file_name(),
        std::env::current_exe().unwrap().file_name()
    );
}
