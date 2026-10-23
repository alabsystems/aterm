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

/// One-line usage synopsis.
pub const SYNOPSIS: &str = "usage: amacs [--wait | --no-wait] [-nw | --tty] [+LINE[:COL]] FILE...";

/// Help text for `amacs --help` and `aterm edit --help`.
pub const HELP: &str = "\
amacs - aterm's editor (`aterm edit` is the same command)

USAGE:
    amacs [--wait | --no-wait] [-nw | --tty] [+LINE[:COL]] FILE...

In a tab of an aterm window, each FILE opens in a fresh Editor tab beside this
one, and amacs waits, printing nothing, until that tab is closed; then the next
FILE opens. That makes it an $EDITOR for `git commit`, `crontab -e` and an
agent's editor key: close the tab when you are done, and the file on disk is
the answer. A FILE that does not exist yet is created by its first save.

OPTIONS:
    +LINE[:COL]   put the caret on LINE (and column COL) of the FILE after it
    --wait        open the window view and wait even when stdin or stdout is
                  not a terminal
    --no-wait     open the window view and return at once
    -nw, --tty    edit in this terminal instead of a window tab
    -h, --help    print this help
    --            every argument after it is a FILE

OUTSIDE A WINDOW (over ssh, in tmux or screen, with -nw, or when the window
will not take a FILE), amacs edits in the terminal itself, with the same
Emacs keys: C-x C-s saves, C-x C-c closes the file (asking first when it has
unsaved changes), C-c C-c saves and closes, C-c C-k closes without saving,
C-g cancels, C-z suspends. A save refuses when the file changed on disk since
it was read: C-x C-s again overwrites it, M-x revert-buffer reloads it.

EXIT STATUS:
    0    every file's view was closed
    1    the window refused a file or did not answer, a file could not be
         opened, or an edit was abandoned (C-c C-k)
    2    a usage error, or no terminal (stdin or stdout is not a tty)
         without --wait or --no-wait, or no terminal to edit in
    75   the aterm serving the edit went away and no successor took the wait
    130  interrupted (C-c): the view stays open in the window
";

/// File to edit and its initial caret position.
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
mod tests {
    use std::io;

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
    /// byte written.
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
    /// the realpath.
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
    /// next file only, `--` making every later word a file.
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
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let name = std::ffi::OsStr::from_bytes(b"/usr/bin/gi\xff\xc3t");
            assert_eq!(for_word(Some(Path::new(name))).as_deref(), Some("git"));
        }
    }

    /// The client's word alphabet is the engine's: for every byte, a `for=`
    /// word of it formats exactly when [`is_for_word_byte`] keeps it, so a
    /// word [`for_word`] builds is never why a request does not send.
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
