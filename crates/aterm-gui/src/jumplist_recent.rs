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
#[path = "jumplist_recent_tests.rs"]
mod tests;
