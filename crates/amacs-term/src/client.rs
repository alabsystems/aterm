// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native face's client (design §6.1, §6.4, §6.5; slice N2): `amacs FILE`
//! in an aterm tab hands each file to the window hosting the tab and waits,
//! SILENTLY, until its Editor view is gone.
//!
//! Nothing is written to the caller's terminal on success: Claude Code's
//! redraw counts its own lines, and a line from its `$EDITOR` would shift
//! them. Only a failure says anything, on stderr.
//!
//! Each file is realpathed (a missing file's parent, plus its name), so the
//! window, whose working directory is not the caller's, is handed an absolute
//! name, and encoded with the engine's one file-URI codec
//! ([`amacs::protocol::uri::path_to_file_uri`]), the spelling the server
//! decodes. Several files are edited one after another; `+LINE[:COL]` puts
//! the caret of the file after it. `for=` names the program the edit is for
//! (its header row shows it, N3): the parent process's file name.
//!
//! An interrupt (C-c in the caller's terminal) ends the client by SIGINT's
//! default action, which the shell reports as 130. Nothing is sent: the view
//! and the person's typing stay, and the server orphans the ticket once its
//! waiter has been gone a few seconds (design D-9).
//!
//! The window is a [`Window`] (the control socket, or a test's stand-in) and
//! the terminal a plain writer, so a run is checked byte for byte.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use amacs::protocol::edit::{AwaitKey, EditOk, EditOutcome, EditRequest, EditResult};
use aterm_ctl::edit::{AwaitErr, EditErr};

use crate::decide::{self, Face, Handoff};

/// The status of a run whose server went away with no successor to take the
/// wait: [`aterm_ctl::EXIT_REPLACED`].
pub const EXIT_REPLACED: u8 = aterm_ctl::EXIT_REPLACED;

/// The one-line synopsis a usage error repeats.
pub const SYNOPSIS: &str = "usage: amacs [--wait | --no-wait] [-nw | --tty] [+LINE[:COL]] FILE...";

/// `amacs --help` (and `aterm edit --help`).
pub const HELP: &str = "\
amacs - aterm's editor (`aterm edit` is the same command)

USAGE:
    amacs [--wait | --no-wait] [-nw | --tty] [+LINE[:COL]] FILE...

In a tab of an aterm window, each FILE opens in a fresh Editor tab beside this
one, and amacs waits, printing nothing, until that tab is closed; then the next
FILE opens. That makes it an $EDITOR for `git commit`, `crontab -e` and an
agent's editor key: close the tab when you are done, and the file on disk is
the answer. A FILE that does not exist yet is not opened in a window yet (the
window refuses to create one).

OPTIONS:
    +LINE[:COL]   put the caret on LINE (and column COL) of the FILE after it
    --wait        open the window view and wait even when stdin or stdout is
                  not a terminal
    --no-wait     open the window view and return at once
    -nw, --tty    edit in this terminal instead of a window tab
    -h, --help    print this help
    --            every argument after it is a FILE

OUTSIDE A WINDOW (over ssh, in tmux or screen, with -nw), amacs edits in the
terminal itself. That editor is not built yet: amacs says so and exits 1.

EXIT STATUS:
    0    every file's view was closed
    1    the window refused a file or did not answer, or (for now) the edit
         needed the terminal editor
    2    a usage error, or no terminal (stdin or stdout is not a tty)
         without --wait or --no-wait
    75   the aterm serving the edit went away and no successor took the wait
    130  interrupted (C-c): the view stays open in the window
";

/// One file to edit, and where its caret lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    /// A 1-based line and, optionally, column.
    pub line: Option<(u32, Option<u32>)>,
}

/// A command line that edits.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Args {
    /// `-nw` / `--tty`.
    pub tty: bool,
    pub wait: bool,
    pub no_wait: bool,
    pub targets: Vec<Target>,
}

/// What a command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Edit(Args),
}

/// Read `amacs`'s arguments (argv after the program name).
///
/// # Errors
///
/// The usage error to print, without the `amacs: ` lead.
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

/// `path`'s realpath as a `file://` URI, and whether the file is missing.
/// A missing file is named by its directory's realpath plus its own name, so
/// a symlinked directory resolves either way.
///
/// # Errors
///
/// Why the path names no file the window could be handed (no such directory,
/// a path that is not UTF-8), worded for a person.
pub fn target_uri(path: &Path) -> Result<(String, bool), String> {
    let (real, missing) = match std::fs::canonicalize(path) {
        Ok(real) => (real, false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let name = path
                .file_name()
                .ok_or_else(|| format!("{}: {e}", path.display()))?;
            let dir = match path.parent() {
                Some(dir) if !dir.as_os_str().is_empty() => dir,
                _ => Path::new("."),
            };
            let dir = std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            (dir.join(name), true)
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let uri = amacs::protocol::uri::path_to_file_uri(&real)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((uri, missing))
}

/// The `for=` word for the program at `program`: its file name (less a
/// Windows `.exe`), kept to `[A-Za-z0-9._-]` and 32 characters; `None` when
/// nothing is left.
///
/// The name is read as its own bytes, never through a lossy conversion: a
/// word byte is ASCII, and every byte of a name that is not ASCII (on Windows,
/// of its WTF-8) is `0x80` or above, so the word is exactly the name's ASCII
/// word characters, whatever else the name holds.
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
    /// A byte the `for=` wire word may hold, `[A-Za-z0-9._-]`: the alphabet
    /// the engine's request parser checks, so a word built from these bytes
    /// is never the reason a window refuses the edit. Proved for every byte.
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

// Spec `for-word-byte` (SPECS.md): for every byte, the class is exactly
// `0-9`, `A-Z`, `a-z`, `.`, `_`, `-` (ASCII 48-57, 65-90, 97-122, 46, 95,
// 45). Kernel-checked against the compiled body.
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

/// The executable of this process's parent, the program the edit is for.
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

/// The window an edit is handed to.
pub trait Window {
    /// Ask for a fresh Editor view of the file.
    ///
    /// # Errors
    ///
    /// Why no view opened.
    fn edit(&mut self, request: &EditRequest) -> Result<EditOk, EditErr>;
    /// Wait until the opened view is gone.
    ///
    /// # Errors
    ///
    /// Why the wait ended without the edit's outcome.
    fn await_edit(&mut self, opened: &EditOk) -> Result<EditOutcome, AwaitErr>;
}

/// The window hosting session `sid`, over its control socket.
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

/// How the native face's run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    /// Every file was handed over and, unless `--no-wait`, its view closed.
    Done,
    /// The window would not take a file (why): the rest is the terminal's.
    Terminal(String),
    /// Stop with this status; anything to say is already said.
    Exit(u8),
}

/// The handoff row an `edit` failure is ([`decide::after_handoff`]).
#[must_use]
pub fn handoff_of(error: &EditErr) -> Handoff {
    match error {
        EditErr::Refused(_) => Handoff::Refused,
        EditErr::Unreachable(_) | EditErr::Unsendable(_) => Handoff::Unreachable,
        EditErr::TimedOut(_) | EditErr::NoAnswer(_) | EditErr::Unreadable(_) => Handoff::TimedOut,
    }
}

/// Hand each of `args`' files to `window` in turn and, unless `--no-wait`,
/// wait for its view to close. `tty` is where the caller sees this process
/// write (its stderr): only a failure writes there.
pub fn edit_files(
    args: &Args,
    for_word: Option<&str>,
    window: &mut dyn Window,
    tty: &mut dyn Write,
) -> Ending {
    for target in &args.targets {
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
            // `--no-wait` says so on the wire, so the server does not hold the
            // caller's session for an edit nothing waits on (design §6.7).
            wait: !args.no_wait,
        };
        let opened = match window.edit(&request) {
            Ok(opened) => opened,
            Err(error) => {
                let why = format!(
                    "{}: the aterm window did not open it ({error})",
                    target.path.display()
                );
                return match decide::after_handoff(handoff_of(&error)) {
                    Face::Terminal => Ending::Terminal(why),
                    Face::Exit(code, line) => {
                        say(tty, &why);
                        say(tty, line.trim_start_matches("amacs: "));
                        Ending::Exit(code)
                    }
                    // An error never opens a view.
                    Face::Native => Ending::Exit(1),
                };
            }
        };
        // The request said `wait=0`: the view opened holding nothing, and
        // nobody waits on its ticket.
        if args.no_wait {
            continue;
        }
        let end = match window.await_edit(&opened) {
            Ok(outcome) => match outcome.result {
                EditResult::Finished => decide::WAIT_FINISHED,
                EditResult::Closed => decide::WAIT_CLOSED,
                // The person said abort; there is nothing to add.
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

/// One `amacs: …` line on `tty`; a terminal that cannot take it has nobody
/// to tell.
pub(crate) fn say(tty: &mut dyn Write, line: &str) {
    let _ = writeln!(tty, "amacs: {line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the caller's terminal received from a run: every byte the client
    /// wrote to it.
    #[derive(Default)]
    struct MemTty(Vec<u8>);

    impl Write for MemTty {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A window that opens every view and answers each wait with the next of
    /// `outcomes`, recording what it was asked.
    struct StandIn {
        asked: Vec<EditRequest>,
        waited: Vec<u64>,
        outcomes: Vec<Result<EditOutcome, AwaitErr>>,
    }

    impl Window for StandIn {
        fn edit(&mut self, request: &EditRequest) -> Result<EditOk, EditErr> {
            self.asked.push(request.clone());
            Ok(EditOk {
                ticket: amacs::protocol::edit::Ticket(self.asked.len() as u64),
                view: 40,
                doc: 3,
                uri: request.uri.clone(),
            })
        }
        fn await_edit(&mut self, opened: &EditOk) -> Result<EditOutcome, AwaitErr> {
            self.waited.push(opened.ticket.0);
            self.outcomes.remove(0)
        }
    }

    fn closed(saved: bool) -> Result<EditOutcome, AwaitErr> {
        Ok(EditOutcome {
            result: EditResult::Closed,
            saved,
            seq: 1,
        })
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// THE CALLER'S TERMINAL SEES NOTHING when the edits succeed: two files,
    /// each opened, waited on and closed, one after another, and not one
    /// byte written. NEGATIVE CONTROL: a wait whose server went away says so
    /// there and ends 75, so the sink does see what the client writes.
    #[test]
    fn success_writes_zero_bytes_to_the_tty() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let first = dir.path().join("COMMIT_EDITMSG");
        let second = dir.path().join("notes.md");
        std::fs::write(&first, "x\n").expect("first file");
        std::fs::write(&second, "y\n").expect("second file");
        let args = Args {
            targets: vec![
                Target {
                    path: first.clone(),
                    line: Some((1, None)),
                },
                Target {
                    path: second,
                    line: None,
                },
            ],
            ..Args::default()
        };
        let mut window = StandIn {
            asked: Vec::new(),
            waited: Vec::new(),
            outcomes: vec![closed(true), closed(false)],
        };
        let mut tty = MemTty::default();
        let ending = edit_files(&args, Some("git"), &mut window, &mut tty);
        assert_eq!(ending, Ending::Done);
        assert!(
            tty.0.is_empty(),
            "wrote {:?}",
            String::from_utf8_lossy(&tty.0)
        );
        assert_eq!(window.waited, [1, 2], "one after another");
        assert_eq!(window.asked[0].line, Some((1, None)));
        assert_eq!(window.asked[0].for_word.as_deref(), Some("git"));
        assert!(!window.asked[0].create);
        let (uri, _) = target_uri(&first).expect("uri");
        assert_eq!(window.asked[0].uri, uri);

        let mut window = StandIn {
            asked: Vec::new(),
            waited: Vec::new(),
            outcomes: vec![Err(AwaitErr::Replaced("connection refused".to_string()))],
        };
        let ending = edit_files(&args, None, &mut window, &mut tty);
        assert_eq!(ending, Ending::Exit(EXIT_REPLACED));
        let said = String::from_utf8_lossy(&tty.0);
        assert!(
            said.starts_with("amacs: ") && said.contains("went away"),
            "{said}"
        );
        assert_eq!(window.asked.len(), 1, "the second file is never opened");
    }

    /// `--no-wait` hands every file over saying `wait=0` and waits on none;
    /// an abort ends the run 1, saying nothing (the person asked for it).
    #[test]
    fn no_wait_never_waits_and_an_abort_is_exit_1() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a\n").expect("file");
        let target = Target {
            path: file,
            line: None,
        };
        let mut args = Args {
            no_wait: true,
            targets: vec![target.clone(), target],
            ..Args::default()
        };
        let mut window = StandIn {
            asked: Vec::new(),
            waited: Vec::new(),
            outcomes: Vec::new(),
        };
        let mut tty = MemTty::default();
        assert_eq!(edit_files(&args, None, &mut window, &mut tty), Ending::Done);
        assert_eq!((window.asked.len(), window.waited.len()), (2, 0));
        assert!(
            window.asked.iter().all(|request| !request.wait),
            "each request says `wait=0`"
        );
        assert!(
            window.asked[0].format().expect("wire").ends_with(" wait=0"),
            "and the wire carries it"
        );

        args.no_wait = false;
        let mut window = StandIn {
            asked: Vec::new(),
            waited: Vec::new(),
            outcomes: vec![Ok(EditOutcome {
                result: EditResult::Aborted,
                saved: false,
                seq: 0,
            })],
        };
        assert_eq!(
            edit_files(&args, None, &mut window, &mut tty),
            Ending::Exit(1)
        );
        assert_eq!(window.asked.len(), 1);
        assert!(window.asked[0].wait, "a waiting request holds its caller");
        assert!(tty.0.is_empty());

        // A wait the window refused ends the run 1 too, saying why.
        let mut window = StandIn {
            asked: Vec::new(),
            waited: Vec::new(),
            outcomes: vec![Err(AwaitErr::Refused("ERR no such ticket".to_string()))],
        };
        assert_eq!(
            edit_files(&args, None, &mut window, &mut tty),
            Ending::Exit(1)
        );
        let said = String::from_utf8_lossy(&tty.0);
        assert!(said.ends_with("ERR no such ticket\n"), "{said}");

        // So does a wait whose session cannot be framed: nothing was sent.
        let mut window = StandIn {
            asked: Vec::new(),
            waited: Vec::new(),
            outcomes: vec![Err(AwaitErr::Unsendable(
                "expected an s-<hex> session id".to_string(),
            ))],
        };
        let mut tty = MemTty::default();
        assert_eq!(
            edit_files(&args, None, &mut window, &mut tty),
            Ending::Exit(1)
        );
        let said = String::from_utf8_lossy(&tty.0);
        assert!(
            said.ends_with("the request cannot be sent (expected an s-<hex> session id)\n"),
            "{said}"
        );
    }

    /// The file the window is handed is the realpath's, percent-encoded into
    /// one wire token: a symlinked directory and a symlinked file resolve to
    /// what they name, spaces and `#` are escaped, and the URI decodes back to
    /// the realpath. A missing file is its directory's realpath plus its
    /// name, and says it is missing (`create=1`). NEGATIVE CONTROL: a missing
    /// directory names nothing.
    #[cfg(unix)]
    #[test]
    fn realpath_and_percent_encoding_of_symlinked_and_spaced_paths() {
        use amacs::protocol::uri::{file_uri_to_path, path_to_file_uri};

        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let real_dir = dir.path().join("with space");
        std::fs::create_dir(&real_dir).expect("dir");
        let real_file = real_dir.join("COMMIT EDITMSG#1");
        std::fs::write(&real_file, "x\n").expect("file");
        let link_dir = dir.path().join("link dir");
        std::os::unix::fs::symlink(&real_dir, &link_dir).expect("dir symlink");
        let link_file = dir.path().join("alias");
        std::os::unix::fs::symlink(&real_file, &link_file).expect("file symlink");
        let canonical = std::fs::canonicalize(&real_file).expect("realpath");

        for named in [
            real_file.clone(),
            link_dir.join("COMMIT EDITMSG#1"),
            link_file,
        ] {
            let (uri, missing) = target_uri(&named).expect("named");
            assert!(!missing, "{}", named.display());
            assert_eq!(uri, path_to_file_uri(&canonical).expect("uri"));
            assert!(uri.contains("with%20space/COMMIT%20EDITMSG%231"), "{uri}");
            assert!(!uri.chars().any(char::is_whitespace), "{uri}");
            assert_eq!(file_uri_to_path(&uri).expect("decodes"), canonical);
        }

        let (uri, missing) = target_uri(&link_dir.join("new file.md")).expect("missing file");
        assert!(missing);
        assert_eq!(
            file_uri_to_path(&uri).expect("decodes"),
            std::fs::canonicalize(&real_dir)
                .expect("realpath")
                .join("new file.md")
        );
        assert!(uri.ends_with("/with%20space/new%20file.md"), "{uri}");

        let nowhere = dir.path().join("no such dir").join("x.md");
        let why = target_uri(&nowhere).expect_err("no directory");
        assert!(why.contains("no such dir"), "{why}");
    }

    /// The command line: flags anywhere before `--`, `+LINE[:COL]` for the
    /// next file only, `--` making every later word a file. Usage errors:
    /// an unknown flag, a bad or dangling position, both waits, no file.
    #[test]
    fn the_command_line_reads_flags_positions_and_files() {
        assert_eq!(parse_args(&os(&["a", "--help"])), Ok(Command::Help));
        assert_eq!(parse_args(&os(&["-h"])), Ok(Command::Help));
        let Ok(Command::Edit(args)) = parse_args(&os(&[
            "--wait", "+12:4", "a b", "c", "-nw", "--", "-x", "+3",
        ])) else {
            panic!("an edit");
        };
        assert!(args.wait && args.tty && !args.no_wait);
        assert_eq!(
            args.targets,
            [
                Target {
                    path: PathBuf::from("a b"),
                    line: Some((12, Some(4))),
                },
                Target {
                    path: PathBuf::from("c"),
                    line: None,
                },
                Target {
                    path: PathBuf::from("-x"),
                    line: None,
                },
                Target {
                    path: PathBuf::from("+3"),
                    line: None,
                },
            ]
        );
        for bad in [
            &["--aterm-front-door-routing-probe"][..],
            &["-x", "a"],
            &["+0", "a"],
            &["+3:0", "a"],
            &["+x", "a"],
            &["a", "+3"],
            &["--wait", "--no-wait", "a"],
            &[],
            &["--tty"],
        ] {
            assert!(parse_args(&os(bad)).is_err(), "{bad:?}");
        }
    }

    /// `for=` is the parent's file name kept to the wire's word.
    #[test]
    fn for_word_is_the_parents_file_name_as_a_wire_word() {
        assert_eq!(
            for_word(Some(Path::new("/usr/bin/git"))).as_deref(),
            Some("git")
        );
        assert_eq!(
            for_word(Some(Path::new("/opt/Claude Code/claude+x"))).as_deref(),
            Some("claudex")
        );
        let long = format!("/bin/{}", "a".repeat(40));
        assert_eq!(for_word(Some(Path::new(&long))).map(|w| w.len()), Some(32));
        assert_eq!(for_word(Some(Path::new("/bin/+++"))), None);
        assert_eq!(for_word(None), None);
        // A name that is not UTF-8 keeps its ASCII word bytes and drops the
        // rest, as a lossy reading of it would have.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let name = std::ffi::OsStr::from_bytes(b"/usr/bin/gi\xff\xc3t");
            assert_eq!(for_word(Some(Path::new(name))).as_deref(), Some("git"));
        }
    }

    /// The client's word alphabet is the engine's: for every byte, a `for=`
    /// word of it formats exactly when [`is_for_word_byte`] keeps it, so a
    /// word [`for_word`] builds is never why a request does not send. The
    /// proved contract fixes the class; this ties it to the parser in
    /// another crate, which no contract here can reach.
    #[test]
    fn the_word_byte_class_is_the_engines_for_word_alphabet() {
        let request = |word: String| EditRequest {
            uri: "file:///tmp/a".to_string(),
            line: None,
            create: false,
            for_word: Some(word),
            wait: true,
        };
        for byte in 0..=u8::MAX {
            let sent = request(char::from(byte).to_string()).format().is_ok();
            assert_eq!(sent, is_for_word_byte(byte), "byte {byte:#04x}");
        }
        let long = for_word(Some(Path::new(&format!("/bin/{}", "a.b_c-9".repeat(9)))));
        assert!(request(long.expect("a word")).format().is_ok());
    }

    /// The client's face after a failed `edit` agrees with the control
    /// client's own reading of the failure: the terminal exactly when it
    /// says the terminal.
    #[test]
    fn a_failed_handoff_takes_the_face_the_control_client_names() {
        let all = [
            EditErr::Refused("ERR scope".to_string()),
            EditErr::TimedOut("ERR timeout ticket=1".to_string()),
            EditErr::Unreachable(io::ErrorKind::NotFound.into()),
            EditErr::NoAnswer(io::ErrorKind::TimedOut.into()),
            EditErr::Unreadable("OK".to_string()),
            EditErr::Unsendable("uri is empty".to_string()),
        ];
        for error in all {
            let face = decide::after_handoff(handoff_of(&error));
            assert_eq!(
                face == Face::Terminal,
                error.falls_back_to_terminal(),
                "{error:?}: {face:?}"
            );
        }
    }
}
