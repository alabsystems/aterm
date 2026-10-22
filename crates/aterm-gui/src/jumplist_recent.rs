// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The taskbar jump list's **Recent** directories: which working directories
//! the owner actually used, kept newest first in aterm's state folder, and the
//! UI-thread sampler that notices them.
//!
//! **What counts as used**: a directory a shell RAN A COMMAND in. Each shell
//! block carries the cwd its prompt opened in (OSC 7, read by `aterm-core` at
//! OSC 133 A), and a block that reached C ran a command there — so a tab that
//! was opened and never typed into, a `cd` with no command after it, and a
//! cmd.exe tab (prompt-only integration, no C) add nothing. The jump list then
//! offers the newest few as rows that open a new aterm window there.
//!
//! **Cadence**: the sampler rides the per-frame taskbar seam
//! (`App::sync_taskbar_progress`) and does real work at most every
//! [`SAMPLE_EVERY`], plus once the moment a window loses focus — the owner
//! leaving aterm is exactly when the taskbar menu may be opened next. A sample
//! is one `try_lock` per session and an integer compare (`started_command_seq`),
//! and walks blocks only for a session that started a command since the last
//! sample; a contended engine is skipped, never waited on. Everything that can
//! block — the existence check (a mapped network drive can take seconds), the
//! file, the shell's `CommitList` — happens on the jump list's own worker
//! thread (`jumplist_win::install`), which receives the notes over a channel.
//!
//! **What is never kept**: anything that is not a local drive path (UNC and
//! `\\wsl$` shares, a WSL or SSH shell's POSIX cwd, `\\?\UNC\`), a path with a
//! character Windows forbids in one, and anything at all while Windows' own
//! "Show recently opened items in Start, Jump Lists and File Explorer" switch
//! is off (`jumplist_win::recents_tracked`): the worker then deletes the file
//! and shows no Recent category. A private window (a development build under
//! its own `ATERM_STATE_HOME`) runs no worker, so it keeps nothing and leaves
//! the owner's list alone (`jumplist_win::owns_the_jump_list`); a headless
//! instance has no taskbar seam, so it never samples.
#![cfg(windows)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_types::{BlockState, OutputBlock};

/// How many directories the file keeps. More than [`SHOWN`], so a directory
/// that is missing today (an unplugged drive) does not cost the list a row.
pub(crate) const KEEP: usize = 12;
/// How many rows the Recent category shows (Windows Terminal and VS Code show
/// five to ten; the shell's own default slot count is ten, tasks included).
pub(crate) const SHOWN: usize = 6;
/// The sampler's resting cadence.
const SAMPLE_EVERY: Duration = Duration::from_secs(30);
/// At most this many directories from one session per sample, so a session
/// whose whole history is new (a restored one) cannot flood the list.
const NOTES_PER_SESSION: usize = 8;
/// The longest path kept: a jump list row is not the place for more, and the
/// file stays a few KB.
const MAX_PATH_CHARS: usize = 1024;
/// The file's name in aterm's state folder (`aterm_types::dirs::state_dir`,
/// `%LOCALAPPDATA%\aterm`).
pub(crate) const FILE_NAME: &str = "jumplist-recent.txt";

// The list (pure)

/// `dir` as the one spelling the list keeps, or `None` when it is not a local
/// drive directory a jump list row could open: `X:\…` with backslashes, no
/// doubled or trailing separator (but the root's), an upper-case drive letter,
/// and no `.`/`..` component or name Win32 would rewrite (a trailing dot or
/// space). A `\\?\C:\…` verbatim path loses its prefix; a UNC path (`\\server`,
/// `\\wsl$`, `\\?\UNC\`), a POSIX path a WSL or SSH shell reported, a
/// drive-relative `C:x`, an alternate data stream and a character Windows
/// forbids in a path are all refused.
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
    // `?` was let through the forbidden set above only for the verbatim prefix.
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

/// Whether two kept spellings name the same directory. NTFS compares names
/// case-insensitively, so `C:\Src` and `c:\src` are one row.
pub(crate) fn same_dir(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

/// `list` with each of `notes` (oldest first) moved to the front — the newest
/// note ends up first — deduplicated by [`same_dir`], with anything
/// [`normalize`] refuses dropped, cut to `keep`. A directory already kept
/// keeps its spelling: PowerShell reports the case as typed, and a row whose
/// title flipped case with each `cd` would re-commit the list for nothing.
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

/// `list` without the directories in `gone` (the rows the owner removed from
/// the jump list: the shell forbids re-adding them, and they stay out until the
/// owner runs a command there again).
pub(crate) fn without(list: &[String], gone: &[String]) -> Vec<String> {
    list.iter()
        .filter(|dir| !gone.iter().any(|g| same_dir(g, dir)))
        .cloned()
        .collect()
}

/// The rows to offer: the first `max` of `list` that `is_dir` says exist now.
/// `is_dir` is injected so the rule is testable; the worker passes
/// `Path::is_dir`, off the UI thread.
pub(crate) fn shown(list: &[String], is_dir: impl Fn(&str) -> bool, max: usize) -> Vec<String> {
    list.iter()
        .filter(|dir| is_dir(dir))
        .take(max)
        .cloned()
        .collect()
}

/// The file's list: one directory per line, newest first; blank lines and `#`
/// comments skipped, and each line held to [`normalize`] again (the file is
/// the owner's to edit), deduplicated and cut to [`KEEP`].
pub(crate) fn parse(text: &str) -> Vec<String> {
    let lines: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    merge(&lines, &[], KEEP)
}

/// The file's text for `list` ([`parse`]'s inverse).
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

/// Each row's visible title: the directory's own name, lengthened by parent
/// names only where two rows would otherwise read the same (`a\src` and
/// `b\src`), and the whole path for a drive root or once nothing shorter is
/// unique. The full path is always the row's tooltip.
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

/// The front-door arguments of the row for `dir`: `new-window -d "<dir>"`, the
/// verb that always opens a WINDOW (see `jumplist_win`'s module docs). A drive
/// root's trailing backslash is doubled so it does not escape the closing quote
/// under the MSVCRT argv rules (the hazard `explorer_win::verb_command`
/// documents); [`normalize`] leaves no other trailing backslash and no quote.
pub(crate) fn row_arguments(dir: &str) -> String {
    let tail = if dir.ends_with('\\') { "\\" } else { "" };
    format!("new-window -d \"{dir}{tail}\"")
}

// What a session ran (pure)

/// The directories commands ran in, from `blocks` NEWEST FIRST
/// (`Terminal::all_blocks().rev()`), stopping at the first block at or below
/// `through` (the newest block a previous read consumed): each block that
/// reached C (executing or complete) and carries a cwd, at most `cap`, as
/// `(command start ms, native path)` oldest first. Also the new `through`: the
/// newest block that ran, so a prompt nothing has run in yet is read again.
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

// The channel to the worker

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

/// The worker's end of the channel, once per process; the sampler starts
/// recording when it is taken.
pub(crate) fn take_notes() -> Option<Receiver<Vec<String>>> {
    let rx = mailbox().rx.lock().ok()?.take()?;
    RECORDING.store(true, Ordering::Release);
    Some(rx)
}

/// The worker is gone (the shell refused the list): stop sampling.
pub(crate) fn stop_recording() {
    RECORDING.store(false, Ordering::Release);
}

// The sampler (UI thread)

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

/// A closing window's focus record goes with it.
pub(crate) fn forget_window(hwnd: isize) {
    SAMPLER.with(|s| {
        s.borrow_mut().focused.remove(&hwnd);
    });
}

impl crate::App {
    /// One frame of window `hwnd` (focused or not, as the app records it): when
    /// a sample is due — [`SAMPLE_EVERY`] since the last, or this window just
    /// lost focus — read every session's new command blocks and hand the
    /// directories they ran in to the jump list's worker. See the module docs
    /// for the cost; nothing here blocks.
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
                    continue; // contended (or poisoned): the next sample reads it
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
            // Oldest first across every session, by when each command started.
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
        // Case-insensitive dedupe; the kept spelling stays.
        assert_eq!(
            merge(&list, &s(&[r"c:\B"]), KEEP),
            s(&[r"C:\b", r"C:\a", r"C:\c"])
        );
        // The same directory twice in one batch is one row.
        assert_eq!(
            merge(&[], &s(&[r"C:\x", r"C:\y", r"C:\x"]), KEEP),
            s(&[r"C:\x", r"C:\y"])
        );
        // Refused notes and stored junk are dropped; duplicates in the store too.
        assert_eq!(
            merge(
                &s(&[r"C:\a", "/home/me", r"c:\A\"]),
                &s(&[r"\\wsl$\u\home"]),
                KEEP
            ),
            s(&[r"C:\a"])
        );
        // Capped at `keep`, oldest dropped.
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
        // Same leaf and same parent on two drives: the whole path tells them apart.
        assert_eq!(
            titles(&s(&[r"C:\x\src", r"D:\x\src"])),
            s(&[r"C:\x\src", r"D:\x\src"])
        );
        // Case-insensitive clash.
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
    /// once, oldest first. A prompt alone adds nothing, a frame before the
    /// cadence is due reads nothing, and a focus loss samples at once.
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
        // Nothing ran since: the next focus loss sends nothing.
        app.sample_recent_dirs(HWND, true);
        app.sample_recent_dirs(HWND, false);
        assert!(notes.try_recv().is_err());
        stop_recording();
        // Not recording: even a focus loss after a new command reads nothing.
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
        // Nothing new: nothing read, the mark holds.
        let (dirs, again) = fresh_dirs(blocks.iter().rev(), through, NOTES_PER_SESSION);
        assert!(dirs.is_empty());
        assert_eq!(again, Some(4));
        // Block 5's command starts: it is read once.
        let mut later = blocks.to_vec();
        later[4].state = BlockState::Executing;
        let (dirs, through) = fresh_dirs(later.iter().rev(), Some(4), NOTES_PER_SESSION);
        assert_eq!(dirs, vec![(500, r"C:\typing".to_owned())]);
        assert_eq!(through, Some(5));
        // The cap keeps the newest.
        let (dirs, _) = fresh_dirs(blocks.iter().rev(), None, 2);
        assert_eq!(
            dirs,
            vec![(300, r"C:\b".to_owned()), (400, r"C:\c".to_owned())]
        );
        // A block with no reported cwd counts as read but adds no row.
        let bare = [block(7, BlockState::Complete, None, 700)];
        assert_eq!(
            fresh_dirs(bare.iter().rev(), None, NOTES_PER_SESSION),
            (Vec::new(), Some(7))
        );
    }
}
