// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn s(list: &[&str]) -> Vec<String> {
    list.iter().map(|d| (*d).to_owned()).collect()
}

/// Only a local drive directory survives, in one spelling: what an OSC 7
/// report, the restore manifest and a hand edit can each hand the list.
#[test]
fn only_local_drive_directories_are_kept_in_one_spelling() {
    assert_eq!(normalize(r"C:\Users\me").as_deref(), Some(r"C:\Users\me"));
    assert_eq!(normalize(r"c:\Users\me\").as_deref(), Some(r"C:\Users\me"));
    assert_eq!(normalize("C:/Users//me").as_deref(), Some(r"C:\Users\me"));
    assert_eq!(normalize(r"C:\Users\\me").as_deref(), Some(r"C:\Users\me"));
    assert_eq!(normalize(r"C:\").as_deref(), Some(r"C:\"));
    assert_eq!(normalize("D:").as_deref(), None, "drive-relative");
    assert_eq!(normalize(r"\\?\C:\src").as_deref(), Some(r"C:\src"));
    assert_eq!(
        normalize(r"C:\Program Files\x").as_deref(),
        Some(r"C:\Program Files\x")
    );
    assert_eq!(
        normalize(r"C:\Users\ü\Café").as_deref(),
        Some(r"C:\Users\ü\Café")
    );
    for refused in [
        "",
        r"\\server\share\dir",
        r"\\wsl$\Ubuntu\home\me",
        r"\\wsl.localhost\Ubuntu\home",
        r"\\?\UNC\server\share",
        "//server/share",
        "/home/me/proj",
        "/c/Users//me",
        r"Users\me",
        r"C:x",
        r"C:\a\..\b",
        r"C:\a\.\b",
        r"C:\a.\b",
        r"C:\a \b",
        r"C:\file.txt:stream",
        "C:\\a\"b",
        "C:\\a\nb",
        r"C:\a?b",
        r"C:\a*b",
        r"C:\a|b",
    ] {
        assert_eq!(normalize(refused), None, "{refused:?} must not be kept");
    }
    let long = format!(r"C:\{}", "a".repeat(MAX_PATH_CHARS));
    assert_eq!(normalize(&long), None);
}

/// Newest first, one row per directory whatever its case, capped; a kept
/// directory keeps its spelling and junk never enters.
#[test]
fn merge_moves_each_note_to_the_front_once_and_caps() {
    let list = s(&[r"C:\a", r"C:\b", r"C:\c"]);
    assert_eq!(
        merge(&list, &s(&[r"C:\c", r"C:\d"]), KEEP),
        s(&[r"C:\d", r"C:\c", r"C:\a", r"C:\b"])
    );
    assert_eq!(
        merge(&list, &s(&[r"c:\B"]), KEEP),
        s(&[r"C:\b", r"C:\a", r"C:\c"])
    );
    assert_eq!(
        merge(&[], &s(&[r"C:\x", r"C:\y", r"C:\x"]), KEEP),
        s(&[r"C:\x", r"C:\y"])
    );
    assert_eq!(
        merge(
            &s(&[r"C:\a", "/home/me", r"c:\A\"]),
            &s(&[r"\\wsl$\u\home"]),
            KEEP
        ),
        s(&[r"C:\a"])
    );
    let many: Vec<String> = (0..20).map(|i| format!(r"C:\d{i}")).collect();
    let merged = merge(&[], &many, KEEP);
    assert_eq!(merged.len(), KEEP);
    assert_eq!(merged[0], r"C:\d19");
    assert_eq!(merged[KEEP - 1], format!(r"C:\d{}", 20 - KEEP));
}

/// The rows shown are the newest that exist now, at most `max`; removed
/// rows leave the list.
#[test]
fn shown_rows_are_the_newest_existing_and_removed_rows_leave() {
    let list = s(&[r"C:\gone", r"C:\a", r"C:\b", r"C:\c"]);
    let exists = |dir: &str| dir != r"C:\gone";
    assert_eq!(shown(&list, exists, 2), s(&[r"C:\a", r"C:\b"]));
    assert_eq!(shown(&list, |_| false, SHOWN), Vec::<String>::new());
    assert_eq!(
        without(&list, &s(&[r"c:\A", r"C:\c"])),
        s(&[r"C:\gone", r"C:\b"])
    );
}

/// The file round-trips, and a hand-edited file is held to the same rules.
#[test]
fn the_file_round_trips_and_a_hand_edit_is_held_to_the_rules() {
    let list = s(&[r"C:\Users\me\src", r"D:\", r"C:\Program Files"]);
    assert_eq!(parse(&render(&list)), list);
    assert_eq!(
        parse("# note\r\n\r\nC:\\a\r\n\\\\server\\x\r\nc:\\a\\\r\nC:\\b   \r\n"),
        s(&[r"C:\a", r"C:\b"])
    );
}

/// A row reads as the directory's name, lengthened only where two rows
/// would read the same; a drive root reads as itself.
#[test]
fn titles_are_leaf_names_lengthened_only_to_tell_rows_apart() {
    assert_eq!(
        titles(&s(&[
            r"C:\Users\me\aterm",
            r"C:\a\src",
            r"D:\b\src",
            r"E:\"
        ])),
        s(&["aterm", r"a\src", r"b\src", r"E:\"])
    );
    assert_eq!(
        titles(&s(&[r"C:\x\src", r"D:\x\src"])),
        s(&[r"C:\x\src", r"D:\x\src"])
    );
    assert_eq!(
        titles(&s(&[r"C:\p\Src", r"C:\q\src"])),
        s(&[r"p\Src", r"q\src"])
    );
}

/// The row's arguments parse back, through the system's own argv splitter
/// (the one the MSVCRT rules describe), to the verb and the directory —
/// a drive root and a path with spaces included.
#[test]
fn row_arguments_parse_back_to_the_directory() {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn CommandLineToArgvW(line: *const u16, argc: *mut i32) -> *mut *mut u16;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }
    for dir in [r"C:\", r"C:\Program Files\x y", r"D:\a&b%c", r"C:\Users\ü"] {
        let line = format!("aterm-gui.exe {}", row_arguments(dir));
        let wide = crate::win_ffi::wide(&line);
        let mut argc = 0i32;
        // SAFETY: `wide` is a NUL-terminated UTF-16 line that outlives the
        // call; the returned block is read within its `argc` entries, each a
        // NUL-terminated string, and freed once with LocalFree as documented.
        let argv: Vec<String> = unsafe {
            let block = CommandLineToArgvW(wide.as_ptr(), &mut argc);
            assert!(!block.is_null());
            let args = (0..usize::try_from(argc).unwrap())
                .map(|i| {
                    let arg = *block.add(i);
                    let len = (0..).take_while(|&n| *arg.add(n) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(arg, len))
                })
                .collect();
            LocalFree(block.cast());
            args
        };
        assert_eq!(argv, ["aterm-gui.exe", "new-window", "-d", dir], "{line}");
    }
}

/// End to end on the headless app: the shell's own OSC 7 and OSC 133 bytes
/// reach the worker's channel as the directories commands ran in, each
/// once, oldest first.
#[test]
fn the_sampler_hands_the_worker_where_commands_ran() {
    const HWND: isize = 0x7001;
    let notes = take_notes().expect("this is the test binary's one taker");
    let app = crate::App::headless_for_test();
    let feed = |bytes: &[u8]| crate::feed_session_for_test(&app, 0, bytes);
    feed(b"\x1b]7;file:///C:/Users//me\x07\x1b]133;A\x07PS> \x1b]133;B\x07");
    app.sample_recent_dirs(HWND, true);
    assert!(notes.try_recv().is_err(), "a prompt alone is not a use");
    feed(b"ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
    app.sample_recent_dirs(HWND, true);
    assert!(notes.try_recv().is_err(), "the next sample is not due yet");
    app.sample_recent_dirs(HWND, false);
    assert_eq!(notes.try_recv().ok(), Some(s(&[r"C:\Users\me"])));
    feed(
        b"\x1b]7;file:///C:/src\x07\x1b]133;A\x07PS> \x1b]133;B\x07make\r\n\x1b]133;C\x07\
          \x1b]133;D;0\x07\x1b]7;file:///C:/src/b\x07\x1b]133;A\x07PS> \x1b]133;B\x07\
          vim\r\n\x1b]133;C\x07",
    );
    app.sample_recent_dirs(HWND, true);
    app.sample_recent_dirs(HWND, false);
    assert_eq!(notes.try_recv().ok(), Some(s(&[r"C:\src", r"C:\src\b"])));
    app.sample_recent_dirs(HWND, true);
    app.sample_recent_dirs(HWND, false);
    assert!(notes.try_recv().is_err());
    stop_recording();
    feed(b"\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07pwd\r\n\x1b]133;C\x07");
    app.sample_recent_dirs(HWND, true);
    app.sample_recent_dirs(HWND, false);
    assert!(notes.try_recv().is_err());
}

fn block(id: u64, state: BlockState, cwd: Option<&str>, at: u64) -> OutputBlock {
    let mut block = OutputBlock::new(id, id * 10, 0);
    block.state = state;
    block.working_directory = cwd.map(Into::into);
    block.command_exec_start_time_ms = Some(at);
    block
}

/// Only blocks that ran a command count, newest-first input comes out
/// oldest first in native spelling, a later read starts past the last
/// block that ran — and a prompt nothing ran in yet is read again.
#[test]
fn fresh_dirs_reads_what_ran_since_the_last_read() {
    let blocks = [
        block(1, BlockState::Complete, Some("/C:/a"), 100),
        block(2, BlockState::PromptOnly, Some("/C:/skip"), 200),
        block(3, BlockState::Complete, Some("/C:/b"), 300),
        block(4, BlockState::Executing, Some("/C:/c"), 400),
        block(5, BlockState::EnteringCommand, Some("/C:/typing"), 500),
    ];
    let (dirs, through) = fresh_dirs(blocks.iter().rev(), None, NOTES_PER_SESSION);
    assert_eq!(
        dirs,
        vec![
            (100, r"C:\a".to_owned()),
            (300, r"C:\b".to_owned()),
            (400, r"C:\c".to_owned())
        ]
    );
    assert_eq!(through, Some(4), "block 5 has run nothing yet");
    let (dirs, again) = fresh_dirs(blocks.iter().rev(), through, NOTES_PER_SESSION);
    assert!(dirs.is_empty());
    assert_eq!(again, Some(4));
    let mut later = blocks.to_vec();
    later[4].state = BlockState::Executing;
    let (dirs, through) = fresh_dirs(later.iter().rev(), Some(4), NOTES_PER_SESSION);
    assert_eq!(dirs, vec![(500, r"C:\typing".to_owned())]);
    assert_eq!(through, Some(5));
    let (dirs, _) = fresh_dirs(blocks.iter().rev(), None, 2);
    assert_eq!(
        dirs,
        vec![(300, r"C:\b".to_owned()), (400, r"C:\c".to_owned())]
    );
    let bare = [block(7, BlockState::Complete, None, 700)];
    assert_eq!(
        fresh_dirs(bare.iter().rev(), None, NOTES_PER_SESSION),
        (Vec::new(), Some(7))
    );
}
