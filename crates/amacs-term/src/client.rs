// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native editor face client (`amacs FILE` in an aterm tab).

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use amacs::protocol::edit::{AwaitKey, EditOk, EditOutcome, EditRequest, EditResult};
use aterm_ctl::edit::{AwaitErr, EditErr};

use crate::decide::{self, Face, Handoff};

/// Exit status when the server disappeared with no successor ([`aterm_ctl::EXIT_REPLACED`]).
pub const EXIT_REPLACED: u8 = aterm_ctl::EXIT_REPLACED;

/// The usage line printed after a usage error.
pub const SYNOPSIS: &str =
    "usage: amacs [--wait | --no-wait] [-nw | --tty] [--secret=HEADER,...] [+LINE[:COL]] FILE...";

/// Help text for `amacs --help`, `aterm edit --help` and `aterm amacs --help`.
pub const HELP: &str = "\
amacs - aterm's editor (`aterm edit` is the same command)

USAGE:
    amacs [--wait | --no-wait] [-nw | --tty] [--secret=HEADER,...] [+LINE[:COL]] FILE...

In an aterm tab, each FILE opens in a new Editor tab to the right of this one,
and amacs waits, printing nothing, until that tab closes; then the next FILE
opens. That makes it an $EDITOR for `git commit`, `crontab -e` or an agent's
editor key: close the tab when done, and the file on disk is the answer. A
FILE that does not exist yet is created by its first save.

A directory opens as a read-only listing, with directories first, including
hidden entries. RET or f opens the entry at point, n/p or the arrows move, ^
opens the parent, g refreshes, and q closes. These keys work in both
the Editor tab and the terminal editor.

OPTIONS:
    +LINE[:COL]   put the caret on LINE (and column COL) of the next FILE
    --wait        in an aterm tab, use the Editor tab and wait even when stdin
                  or stdout is not a terminal
    --no-wait     in an aterm tab, hand each FILE to an Editor tab and return
                  at once (in the terminal neither flag changes anything)
    -nw, --tty    edit in this terminal instead of an Editor tab
    --secret=HEADER,...
                  for a private grid (`aterm db`'s revealed secrets): the
                  columns, by header, whose cells the Editor tab's
                  accessibility tree and status never read out (`--secret=`
                  names none; without it such a grid hides every cell)
    -h, --help    print this help
    --            every later argument is a FILE

TABLES: in an Editor tab, a .csv or .tsv FILE opens as a grid. The arrows,
Tab, Home and End move between cells; typing, Return or a double-click opens a
cell; Return keeps the cell and Escape drops it. In an open cell Cmd-Z undoes
your last typing and Cmd-Shift-Z redoes it; with no cell open, Cmd-Z undoes the
last cell you kept. Cmd-C or M-w copies a cell's value, Cmd-X, C-w or C-k cuts
it, C-y puts the last copy or cut in a cell, and Cmd-V pastes the clipboard.
C-o (Add Row) adds an empty row below and moves to it; C-S-Backspace (Delete
Row) deletes the row; Cmd-Z takes either back. A click in the panel under the
grid puts the caret there. The first column stays at the left once the grid
scrolls sideways, and a sideways swipe or the arrow marks show hidden columns.
A value with a comma, a quote or a line break is quoted for you, and every
byte outside the cells and rows you change stays as it was. Cmd-E (C-c C-t)
shows the text and back. A file over 4 MiB opens as text. The terminal editor
shows tables as text.

IN THE TERMINAL (outside an aterm tab, over ssh, in tmux or screen, with -nw,
or once the window refuses a FILE or cannot be reached), amacs edits in this
terminal with Emacs keys: C-x C-s saves, C-x C-c closes the file (asking first
when it has unsaved changes), C-c C-c saves and closes, C-c C-k closes without
saving, C-g cancels, C-z suspends. A save refuses when the file changed on disk
since it was read: C-x C-s again overwrites it, M-x revert-buffer reloads it.
On Windows there is no terminal editor yet: in these cases amacs says so and
exits 1.

EXIT STATUS:
    0    every FILE was closed (with --no-wait: handed to the window)
    1    a FILE could not be opened, the window did not answer or failed the
         wait, the edit was abandoned (C-c C-k), the terminal hung up or
         failed, or (on Windows) the edit needed the terminal
    2    a usage error; in an aterm tab (not in tmux or screen) without -nw,
         stdin or stdout is not a terminal and neither --wait nor --no-wait
         was given; or no terminal to edit in
    75   the aterm serving the edit went away and no successor took the wait
    130  interrupted (C-c) while waiting; the Editor tab stays open
";

/// File or directory to open and its initial caret position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    /// 1-based line and optional column.
    pub line: Option<(u32, Option<u32>)>,
}

/// Parsed `amacs` edit command-line arguments.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Args {
    /// Whether `-nw` or `--tty` was passed.
    pub tty: bool,
    pub wait: bool,
    pub no_wait: bool,
    /// `--secret=HEADER,...`: the private grid's columns no reader of the window gets
    /// ([`EditRequest::secret`]).
    pub secret: Option<Vec<String>>,
    pub targets: Vec<Target>,
}

/// Parsed `amacs` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Edit(Args),
}

/// Parses `amacs` command-line arguments after the program name.
///
/// # Errors
/// Returns the usage error message without the `amacs: ` prefix.
pub fn parse_args(args: &[OsString]) -> Result<Command, String> {
    let mut parsed = Args::default();
    let mut position: Option<(u32, Option<u32>)> = None;
    let mut files_only = false;
    for arg in args {
        let text = arg.to_string_lossy();
        if !files_only {
            match text.as_ref() {
                "-h" | "--help" => return Ok(Command::Help),
                "-nw" | "--tty" => {
                    parsed.tty = true;
                    continue;
                }
                "--wait" => {
                    parsed.wait = true;
                    continue;
                }
                "--no-wait" => {
                    parsed.no_wait = true;
                    continue;
                }
                "--" => {
                    files_only = true;
                    continue;
                }
                _ => {}
            }
            if let Some(headers) = text.strip_prefix("--secret=") {
                if parsed.secret.is_some() {
                    return Err("--secret is given twice".to_string());
                }
                let headers: Vec<String> = if headers.is_empty() {
                    Vec::new()
                } else {
                    headers.split(',').map(str::to_string).collect()
                };
                // The wire's own reading decides which lists it carries.
                let probe = EditRequest {
                    uri: "file:///".to_string(),
                    line: None,
                    create: false,
                    for_word: None,
                    wait: true,
                    read_only: false,
                    secret: Some(headers.clone()),
                };
                if probe.format().is_err() {
                    return Err(format!("bad {text}"));
                }
                parsed.secret = Some(headers);
                continue;
            }
            if let Some(spec) = text.strip_prefix('+') {
                position =
                    Some(parse_position(spec).ok_or_else(|| format!("bad position {text}"))?);
                continue;
            }
            if text.starts_with('-') {
                return Err(format!("unknown option {text}"));
            }
        }
        parsed.targets.push(Target {
            path: PathBuf::from(arg),
            line: position.take(),
        });
    }
    if parsed.wait && parsed.no_wait {
        return Err("--wait and --no-wait ask for opposite things".to_string());
    }
    if let Some((line, col)) = position {
        let col = col.map(|c| format!(":{c}")).unwrap_or_default();
        return Err(format!("+{line}{col} names no FILE after it"));
    }
    if parsed.targets.is_empty() {
        return Err("no FILE to edit".to_string());
    }
    Ok(Command::Edit(parsed))
}

/// `LINE` or `LINE:COL`, both 1-based decimal.
fn parse_position(spec: &str) -> Option<(u32, Option<u32>)> {
    let positive = |text: &str| {
        text.parse::<u32>()
            .ok()
            .filter(|n| *n > 0 && text.bytes().all(|b| b.is_ascii_digit()))
    };
    match spec.split_once(':') {
        Some((line, col)) => Some((positive(line)?, Some(positive(col)?))),
        None => Some((positive(spec)?, None)),
    }
}

/// Resolves `path`'s realpath as a `file://` URI and reports whether the file is missing.
///
/// # Errors
/// Why the path names no file the window could be handed (for example no such
/// directory, a dangling symbolic link, or a path that is not UTF-8), worded for
/// a person.
pub fn target_uri(path: &Path) -> Result<(String, bool), String> {
    let (real, uri) = amacs_files::host::open_path(path)
        .map_err(|e| format!("{}: {}", path.display(), crate::tui::host_error(path, &e)))?;
    let missing = std::fs::symlink_metadata(&real).is_err();
    Ok((uri, missing))
}

/// Extracts the `[A-Za-z0-9._-]` `for=` token (up to 32 bytes) from `program`'s file name.
#[must_use]
pub fn for_word(program: Option<&Path>) -> Option<String> {
    let name = program?.file_name()?.as_encoded_bytes();
    let suffix = std::env::consts::EXE_SUFFIX.as_bytes();
    let name = match name.strip_suffix(suffix) {
        Some(stem) if !suffix.is_empty() => stem,
        _ => name,
    };
    let word: String = name
        .iter()
        .copied()
        .filter(|&byte| is_for_word_byte(byte))
        .take(32)
        .map(char::from)
        .collect();
    (!word.is_empty()).then_some(word)
}

trust_contract_fn! {
    /// Returns whether `byte` is a valid `for=` wire word byte (`[A-Za-z0-9._-]`).
    #[must_use]
    pub fn is_for_word_byte(byte: u8) -> bool
    clauses [ensures result == ((byte >= 48 && byte <= 57)
        || ((byte >= 65 && byte <= 90)
        || ((byte >= 97 && byte <= 122)
        || (byte == 46 || (byte == 95 || byte == 45))))) by is_for_word_byte_is_the_class]
    {
        matches!(byte, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'.' | b'_' | b'-')
    }
}

verify_island! {
    clean {
        theorem is_for_word_byte_is_the_class (b : UInt8) :
            trust_import_amacs_term__client__is_for_word_byte b =
              ((decide (48 <= b) && decide (b <= 57))
                || ((decide (65 <= b) && decide (b <= 90))
                || ((decide (97 <= b) && decide (b <= 122))
                || (decide (b = 46) || (decide (b = 95) || decide (b = 45)))))) := by
          simp only [trust_import_amacs_term__client__is_for_word_byte]
    }
}

/// Returns the executable path of this process's parent.
#[must_use]
pub fn parent_program() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        let parent = i32::try_from(std::os::unix::process::parent_id()).ok()?;
        aterm_uds::process::read_process_path(parent)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Window backend that opens and awaits editor views.
pub trait Window {
    /// Requests a fresh Editor view of the file.
    ///
    /// # Errors
    /// Returns why no view opened.
    fn edit(&mut self, request: &EditRequest) -> Result<EditOk, EditErr>;
    /// Waits until the opened view closes.
    ///
    /// # Errors
    /// Returns why the wait ended without an edit outcome.
    fn await_edit(&mut self, opened: &EditOk) -> Result<EditOutcome, AwaitErr>;
}

/// Window hosting session `sid` over its control socket.
pub struct SessionWindow {
    pub sid: String,
}

impl Window for SessionWindow {
    fn edit(&mut self, request: &EditRequest) -> Result<EditOk, EditErr> {
        aterm_ctl::edit::edit(&self.sid, request)
    }

    fn await_edit(&mut self, opened: &EditOk) -> Result<EditOutcome, AwaitErr> {
        aterm_ctl::edit::await_edit(&self.sid, AwaitKey::Ticket(opened.ticket), &opened.uri)
    }
}

/// Outcome of a native editor client run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    /// All files were handed over and (unless `--no-wait`) their views closed.
    Done,
    /// The window declined the file at `from`, falling back to the terminal for remaining files.
    Terminal { why: String, from: usize },
    /// Exit immediately with this status code.
    Exit(u8),
}

/// Maps an [`EditErr`] to its [`Handoff`] classification.
#[must_use]
pub const fn handoff_of(error: &EditErr) -> Handoff {
    match error {
        EditErr::Refused(_) => Handoff::Refused,
        EditErr::Unreachable(_) | EditErr::Unsendable(_) => Handoff::Unreachable,
        EditErr::TimedOut(_) | EditErr::NoAnswer(_) | EditErr::Unreadable(_) => Handoff::TimedOut,
    }
}

/// Hands each file in `args` to `window` and awaits completion unless `--no-wait` is set.
pub fn edit_files(
    args: &Args,
    for_word: Option<&str>,
    window: &mut dyn Window,
    tty: &mut dyn Write,
) -> Ending {
    for (from, target) in args.targets.iter().enumerate() {
        let (uri, missing) = match target_uri(&target.path) {
            Ok(named) => named,
            Err(why) => {
                say(tty, &why);
                return Ending::Exit(1);
            }
        };
        let request = EditRequest {
            uri,
            line: target.line,
            create: missing,
            for_word: for_word.map(str::to_string),
            wait: !args.no_wait,
            read_only: false,
            secret: args.secret.clone(),
        };
        let opened = match window.edit(&request) {
            Ok(opened) => opened,
            Err(error) => {
                let why = format!(
                    "{}: the aterm window did not open it ({error})",
                    target.path.display()
                );
                return match decide::after_handoff(handoff_of(&error)) {
                    Face::Terminal => Ending::Terminal { why, from },
                    Face::Exit(code, line) => {
                        say(tty, &why);
                        say(tty, line.trim_start_matches("amacs: "));
                        Ending::Exit(code)
                    }
                    Face::Native => Ending::Exit(1),
                };
            }
        };
        if args.no_wait {
            continue;
        }
        let end = match window.await_edit(&opened) {
            Ok(outcome) => match outcome.result {
                EditResult::Finished => decide::WAIT_FINISHED,
                EditResult::Closed => decide::WAIT_CLOSED,
                EditResult::Aborted => decide::WAIT_ABORTED,
            },
            Err(error) => {
                say(tty, &format!("{}: {error}", target.path.display()));
                match error {
                    AwaitErr::Replaced(_) => decide::WAIT_REPLACED,
                    AwaitErr::Refused(_) | AwaitErr::Unreadable(_) | AwaitErr::Unsendable(_) => {
                        decide::WAIT_FAILED
                    }
                }
            }
        };
        let status = decide::wait_status(end);
        if status != 0 {
            return Ending::Exit(status);
        }
    }
    Ending::Done
}

/// Writes an `amacs: …` diagnostic line to `tty`.
pub(crate) fn say(tty: &mut dyn Write, line: &str) {
    let _ = writeln!(tty, "amacs: {line}");
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
