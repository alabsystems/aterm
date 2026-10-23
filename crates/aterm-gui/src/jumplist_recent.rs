// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Taskbar jump list Recent directories list and UI-thread command-directory sampler.
#![cfg(windows)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_types::{BlockState, OutputBlock};

/// Maximum number of recent directories persisted on disk.
pub(crate) const KEEP: usize = 12;
/// Maximum number of recent directories shown in the jump list.
pub(crate) const SHOWN: usize = 6;
/// The sampler's resting cadence.
const SAMPLE_EVERY: Duration = Duration::from_secs(30);
/// At most this many directories from one session per sample, so a session
/// whose whole history is new (a restored one) cannot flood the list.
const NOTES_PER_SESSION: usize = 8;
/// The longest path kept: a jump list row is not the place for more, and the
/// file stays a few KB.
const MAX_PATH_CHARS: usize = 1024;
/// Filename in `aterm`'s state directory.
pub(crate) const FILE_NAME: &str = "jumplist-recent.txt";

/// Normalizes `dir` to a canonical local-drive path (`X:\…`), or returns `None`.
pub(crate) fn normalize(dir: &str) -> Option<String> {
    if dir.is_empty()
        || dir.chars().count() > MAX_PATH_CHARS
        || dir
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '<' | '>' | '|' | '*'))
    {
        return None;
    }
    let unified = dir.replace('/', "\\");
    let path = match unified.strip_prefix(r"\\?\") {
        Some(rest)
            if rest
                .get(..4)
                .is_some_and(|p| p.eq_ignore_ascii_case(r"UNC\")) =>
        {
            return None;
        }
        Some(rest) => rest,
        None => unified.as_str(),
    };
    if path.contains('?') {
        return None;
    }
    let bytes = path.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return None;
    }
    let mut out = String::with_capacity(path.len());
    out.push(char::from(bytes[0].to_ascii_uppercase()));
    out.push_str(":\\");
    let mut first = true;
    for part in path[3..].split('\\').filter(|part| !part.is_empty()) {
        if part == "." || part == ".." || part.ends_with(['.', ' ']) || part.contains(':') {
            return None;
        }
        if !first {
            out.push('\\');
        }
        out.push_str(part);
        first = false;
    }
    Some(out)
}

/// Returns whether `a` and `b` name the same directory case-insensitively.
pub(crate) fn same_dir(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// Merges `notes` (oldest first) to the front of `list`, deduplicated by [`same_dir`] and capped at `keep`.
pub(crate) fn merge(list: &[String], notes: &[String], keep: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(list.len() + notes.len());
    for dir in list.iter().filter_map(|dir| normalize(dir)) {
        if !out.iter().any(|kept| same_dir(kept, &dir)) {
            out.push(dir);
        }
    }
    for note in notes.iter().filter_map(|note| normalize(note)) {
        let dir = match out.iter().position(|kept| same_dir(kept, &note)) {
            Some(at) => out.remove(at),
            None => note,
        };
        out.insert(0, dir);
    }
    out.truncate(keep);
    out
}

/// Returns `list` without the directories in `gone`.
pub(crate) fn without(list: &[String], gone: &[String]) -> Vec<String> {
    list.iter()
        .filter(|dir| !gone.iter().any(|g| same_dir(g, dir)))
        .cloned()
        .collect()
}

/// Returns the first `max` directories of `list` for which `is_dir` holds.
pub(crate) fn shown(list: &[String], is_dir: impl Fn(&str) -> bool, max: usize) -> Vec<String> {
    list.iter()
        .filter(|dir| is_dir(dir))
        .take(max)
        .cloned()
        .collect()
}

/// Parses the recent-directories state file into normalized, deduplicated entries capped at [`KEEP`].
pub(crate) fn parse(text: &str) -> Vec<String> {
    let lines: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    merge(&lines, &[], KEEP)
}

/// Serializes `list` as the recent-directories state file text.
pub(crate) fn render(list: &[String]) -> String {
    let mut text = String::from(
        "# aterm: working directories for the taskbar jump list's Recent rows, newest first\n",
    );
    for dir in list {
        text.push_str(dir);
        text.push('\n');
    }
    text
}

/// Computes disambiguated short display titles for each directory in `dirs`.
pub(crate) fn titles(dirs: &[String]) -> Vec<String> {
    let parts: Vec<Vec<&str>> = dirs
        .iter()
        .map(|dir| dir.split('\\').filter(|part| !part.is_empty()).collect())
        .collect();
    let mut depth = vec![1usize; dirs.len()];
    let title = |i: usize, depth: usize| -> String {
        let components = &parts[i];
        if depth >= components.len() {
            dirs[i].clone()
        } else {
            components[components.len() - depth..].join("\\")
        }
    };
    loop {
        let current: Vec<String> = (0..dirs.len()).map(|i| title(i, depth[i])).collect();
        let mut grew = false;
        for i in 0..dirs.len() {
            let clashes = (0..dirs.len()).any(|j| j != i && same_dir(&current[i], &current[j]));
            if clashes && depth[i] < parts[i].len() {
                depth[i] += 1;
                grew = true;
            }
        }
        if !grew {
            return current;
        }
    }
}

/// Formats the CLI arguments (`new-window -d "<dir>"`) for a jump list row.
pub(crate) fn row_arguments(dir: &str) -> String {
    let tail = if dir.ends_with('\\') { "\\" } else { "" };
    format!("new-window -d \"{dir}{tail}\"")
}

/// Collects up to `cap` command-executed directories from newest-first `blocks` newer than `through`, returned oldest-first with the updated watermark.
pub(crate) fn fresh_dirs<'a>(
    blocks: impl Iterator<Item = &'a OutputBlock>,
    through: Option<u64>,
    cap: usize,
) -> (Vec<(u64, String)>, Option<u64>) {
    let mut newest = through;
    let mut dirs = Vec::new();
    for block in blocks {
        if through.is_some_and(|through| block.id <= through) {
            break;
        }
        if !matches!(block.state, BlockState::Executing | BlockState::Complete) {
            continue;
        }
        newest = newest.max(Some(block.id));
        if dirs.len() == cap {
            break;
        }
        if let Some(dir) = block.working_directory.as_deref() {
            let at = block
                .command_exec_start_time_ms
                .or(block.prompt_time_ms)
                .unwrap_or(0);
            dirs.push((at, crate::cwd_native::native_path(dir).into_owned()));
        }
    }
    dirs.reverse();
    (dirs, newest)
}

/// Notes from the UI-thread sampler to the jump list's worker thread: batches
/// of directories, oldest first.
struct Mailbox {
    tx: Sender<Vec<String>>,
    rx: Mutex<Option<Receiver<Vec<String>>>>,
}

static MAILBOX: OnceLock<Mailbox> = OnceLock::new();

/// Set by the worker once it holds the receiver, so the sampler does nothing
/// at all in a process whose worker never started (the console image, a test
/// binary) and the channel cannot fill with notes nobody reads.
static RECORDING: AtomicBool = AtomicBool::new(false);

fn mailbox() -> &'static Mailbox {
    MAILBOX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        Mailbox {
            tx,
            rx: Mutex::new(Some(rx)),
        }
    })
}

/// Takes the worker's receiver once per process and enables sampling.
pub(crate) fn take_notes() -> Option<Receiver<Vec<String>>> {
    let rx = mailbox().rx.lock().ok()?.take()?;
    RECORDING.store(true, Ordering::Release);
    Some(rx)
}

/// Stops UI-thread recent-directory sampling.
pub(crate) fn stop_recording() {
    RECORDING.store(false, Ordering::Release);
}

#[derive(Clone, Copy)]
struct Seen {
    /// The session's `started_command_seq` at the last read.
    started: u64,
    /// The newest block a read consumed ([`fresh_dirs`]).
    through: Option<u64>,
}

#[derive(Default)]
struct Sampler {
    next_at: Option<Instant>,
    seen: HashMap<u64, Seen>,
    /// Each window's focus at its last frame, so the frame that first sees a
    /// window unfocused samples at once.
    focused: HashMap<isize, bool>,
}

thread_local! {
    static SAMPLER: RefCell<Sampler> = RefCell::new(Sampler::default());
}

/// Removes a closing window's focus state from the sampler.
pub(crate) fn forget_window(hwnd: isize) {
    SAMPLER.with(|s| {
        s.borrow_mut().focused.remove(&hwnd);
    });
}

impl crate::App {
    /// Samples newly executed command directories across sessions on cadence or focus loss.
    pub(crate) fn sample_recent_dirs(&self, hwnd: isize, focused: bool) {
        if !RECORDING.load(Ordering::Acquire) {
            return;
        }
        SAMPLER.with(|s| {
            let mut s = s.borrow_mut();
            let left = match s.focused.get(&hwnd) {
                Some(&was) if was == focused => false,
                _ => {
                    let was = s.focused.insert(hwnd, focused);
                    was == Some(true) && !focused
                }
            };
            let now = Instant::now();
            if !left && s.next_at.is_some_and(|at| now < at) {
                return;
            }
            s.next_at = Some(now + SAMPLE_EVERY);
            let mut notes: Vec<(u64, String)> = Vec::new();
            for session in self.pool.iter() {
                let Ok(term) = session.term.try_lock() else {
                    // contended (or poisoned): the next sample reads it
                    continue;
                };
                let started = term.started_command_seq();
                let seen = s.seen.get(&session.id).copied();
                if seen.is_some_and(|seen| seen.started == started) {
                    continue;
                }
                let (dirs, through) = fresh_dirs(
                    term.all_blocks().rev(),
                    seen.and_then(|seen| seen.through),
                    NOTES_PER_SESSION,
                );
                drop(term);
                s.seen.insert(session.id, Seen { started, through });
                notes.extend(dirs);
            }
            s.seen.retain(|id, _| self.pool.get(*id).is_some());
            if notes.is_empty() {
                return;
            }
            notes.sort_by_key(|(at, _)| *at);
            let batch = notes.into_iter().map(|(_, dir)| dir).collect();
            let _ = mailbox().tx.send(batch);
        });
    }
}

#[cfg(test)]
mod tests {
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
            // SAFETY: `wide` is a NUL-terminated UTF-16 string and the returned argv block is freed once with `LocalFree`.
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
}
