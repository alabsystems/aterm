// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The terminal face's loop, headless (design §5.4).

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use amacs_term::client::Target;
use amacs_term::tui::{Close, Options, Outcome, Tui};
use aterm_core::terminal::Terminal;
use aterm_tty::probe;
use aterm_tty::signals::Signals;
use aterm_tty::tty::{MemPeer, MemTty, Mode, Modes, Readiness, Size, Tty};

const SIZE: Size = Size { cols: 80, rows: 24 };

const RESTORE: &[u8] = b"\x1b[?2026l\x1b[m\x1b[0 q\x1b[?25h";

fn tui(options: Options) -> Tui {
    // SAFETY: a test process holding no untrusted input; the launcher-mint guard lets a test, and
    // only a test, mint below the launchers.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    Tui::new(authority.grant(aterm_cap::Tier::Trusted), options)
}

fn target(path: &Path) -> Target {
    Target {
        path: path.to_path_buf(),
        line: None,
    }
}

fn file(name: &str, bytes: &[u8]) -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("the file to edit");
    let path = std::fs::canonicalize(&path).expect("realpath");
    (dir, path)
}

fn aterm_replies() -> Vec<u8> {
    let mut term = Terminal::new(SIZE.rows, SIZE.cols);
    term.process(probe::QUERY);
    term.take_response().expect("aterm answers the probe")
}

fn terminal(size: Size) -> (MemTty, MemPeer) {
    let (tty, peer) = MemTty::pair(size);
    peer.send(&aterm_replies());
    (tty, peer)
}

fn shown(output: &[u8], size: Size) -> Terminal {
    let mut term = Terminal::new(size.rows, size.cols);
    term.process(output);
    term
}

fn last_frame(output: &[u8], size: Size) -> Terminal {
    let end = output
        .windows(RESTORE.len())
        .rposition(|window| window == RESTORE)
        .unwrap_or(output.len());
    shown(output.get(..end).unwrap_or(output), size)
}

fn row(term: &Terminal, row: u16) -> String {
    term.row_text(usize::from(row))
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

fn modeline_and_echo(term: &Terminal, size: Size) -> (u16, String, String) {
    let modeline = (0..size.rows)
        .rev()
        .find(|at| row(term, *at).starts_with("-UU-:"))
        .expect("a modeline");
    let mut echo = String::new();
    for at in modeline.saturating_add(1)..size.rows {
        let text = term.row_text(usize::from(at)).unwrap_or_default();
        echo.push_str(&format!("{text:<width$}", width = usize::from(size.cols)));
    }
    (modeline, row(term, modeline), echo.trim_end().to_string())
}

fn reversed(term: &Terminal, row: u16, col: u16) -> bool {
    term.grid()
        .cell(row, col)
        .is_some_and(|cell| cell.flags().0 & (1 << 5) != 0)
}

fn edit(path: &Path, options: Options, keys: &[&[u8]]) -> (Outcome, Vec<u8>, MemPeer) {
    let (mut tty, peer) = terminal(SIZE);
    for key in keys {
        peer.send(key);
    }
    let outcome = tui(options).edit(&mut tty, &[target(path)]);
    let output = peer.take_output();
    (outcome, output, peer)
}

fn done() -> Outcome {
    Outcome {
        status: 0,
        said: Vec::new(),
    }
}

fn clears(output: &[u8]) -> usize {
    output
        .windows(4)
        .filter(|window| *window == b"\x1b[2J")
        .count()
}

#[test]
fn type_save_quit_round_trip() {
    let (_dir, path) = file("COMMIT_EDITMSG", b"world\n");
    let (outcome, output, peer) = edit(
        &path,
        Options {
            for_word: Some("git".to_string()),
            ..Options::default()
        },
        &[b"hello ", b"\x18\x13", b"\x18\x03"],
    );
    assert_eq!(outcome, done());
    assert_eq!(std::fs::read(&path).expect("read"), b"hello world\n");
    assert_eq!(peer.modes(), Modes::NONE, "every mode put back");
    assert!(peer.script_done());
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("\x1b[>5u"), "kitty flags 1|4 pushed");
    assert!(
        text.ends_with("\x1b[<u\x1b[?1049l"),
        "{:?}",
        &text[text.len().saturating_sub(40)..]
    );

    let term = last_frame(&output, SIZE);
    assert_eq!(row(&term, 0), "hello world");
    let (at, modeline, echo) = modeline_and_echo(&term, SIZE);
    let full =
        "-UU-:--- COMMIT_EDITMSG  (GitCommit) L1 C6  [for git · C-c C-c done · C-c C-k abort]";
    assert_eq!(modeline, full.chars().take(80).collect::<String>());
    assert!(
        reversed(&term, at, 0) && reversed(&term, at, 79),
        "a reverse-video modeline"
    );
    assert_eq!(echo, format!("Wrote {}", path.display()));
    let summary = term.grid().cell(0, 0).expect("a cell");
    assert!(summary.flags().0 & 1 != 0, "the summary line is bold");
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (0, 6), "the cursor is the caret");
}

#[test]
fn isearch_and_kill_yank_in_terminal_profile() {
    let (_dir, path) = file("notes.txt", b"one two three\n");
    let (mut tty, peer) = terminal(SIZE);
    peer.send(b"\x13");
    peer.send(b"two");
    let mut tui = tui(Options::default());
    let mut buffer = tui.open(&target(&path)).expect("opens");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Hangup);
    let term = shown(&peer.take_output(), SIZE);
    assert_eq!(modeline_and_echo(&term, SIZE).2, "I-search: two");
    assert!(reversed(&term, 0, 4) && reversed(&term, 0, 6));
    assert!(!reversed(&term, 0, 3) && !reversed(&term, 0, 7));

    for key in [
        &b"\r"[..],
        b"\x17",
        b"\x05",
        b"\x19",
        b"\x18\x13",
        b"\x18\x03",
    ] {
        peer.send(key);
    }
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Closed);
    tui.finish(&mut tty).expect("put back");
    assert_eq!(std::fs::read(&path).expect("read"), b"one  threetwo\n");
    assert_eq!(peer.modes(), Modes::NONE);
}

#[test]
fn dirty_quit_prompts_and_n_discards() {
    let (_dir, path) = file("notes.txt", b"kept\n");
    let (mut tty, peer) = terminal(SIZE);
    peer.send(b"x");
    peer.send(b"\x18\x03");
    let mut tui = tui(Options::default());
    let mut buffer = tui.open(&target(&path)).expect("opens");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Hangup);
    let mut term = shown(&peer.take_output(), SIZE);
    let question = format!("Save {}? (y, n, q)", path.display());
    let (at, modeline, echo) = modeline_and_echo(&term, SIZE);
    assert_eq!(echo, question);
    assert!(modeline.starts_with("-UU-:**- notes.txt"), "{modeline}");
    let rows = question.len().div_ceil(80);
    assert_eq!(usize::from(SIZE.rows - at - 1), rows, "{question}");
    let cursor = term.cursor();
    assert_eq!(
        (usize::from(cursor.row), usize::from(cursor.col)),
        (23, question.len() + 1 - (rows - 1) * 80),
        "the cursor waits after the question"
    );

    peer.send(b"z");
    peer.send(b"q");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Hangup);
    term.process(&peer.take_output());
    assert_eq!(modeline_and_echo(&term, SIZE).2, "Quit");
    assert_eq!(
        row(&term, 0),
        "xkept",
        "q went back to the buffer, typing nothing"
    );

    peer.send(b"\x18\x03");
    peer.send(b"n");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Closed);
    tui.finish(&mut tty).expect("put back");
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"kept\n",
        "n saves nothing"
    );
    assert_eq!(peer.modes(), Modes::NONE);

    let (outcome, _, _) = edit(&path, Options::default(), &[b"y", b"\x18\x03", b"y"]);
    assert_eq!(outcome, done());
    assert_eq!(std::fs::read(&path).expect("read"), b"ykept\n");
}

#[test]
fn resize_repaints_to_new_size() {
    let (_dir, path) = file("notes.txt", b"alpha\nbeta\n");
    let big = Size {
        cols: 100,
        rows: 30,
    };
    let (mut tty, peer) = terminal(SIZE);
    peer.resize_after(Duration::from_millis(5), big);
    peer.send(b"\x18\x03");
    let outcome = tui(Options::default()).edit(&mut tty, &[target(&path)]);
    assert_eq!(outcome, done());
    let output = peer.take_output();
    assert_eq!(
        clears(&output),
        2,
        "the first frame and the resized one are drawn whole"
    );
    let term = last_frame(&output, big);
    assert_eq!(row(&term, 0), "alpha");
    assert_eq!(row(&term, 1), "beta");
    assert!(
        row(&term, 28).starts_with("-UU-:--- notes.txt  (Plain) L1 C0"),
        "{}",
        row(&term, 28)
    );
    assert!(reversed(&term, 28, 99), "the modeline spans the new width");
    assert!(
        row(&term, 22).is_empty(),
        "the old modeline row is buffer now"
    );
}

#[test]
fn save_refuses_when_disk_changed() {
    let (_dir, path) = file("notes.txt", b"base\n");
    let (mut tty, peer) = terminal(SIZE);
    let mut tui = tui(Options::default());
    let mut buffer = tui.open(&target(&path)).expect("opens");
    std::fs::write(&path, b"theirs\n").expect("an edit from elsewhere");
    peer.send(b"ours ");
    peer.send(b"\x18\x13");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Hangup);
    let term = shown(&peer.take_output(), SIZE);
    assert_eq!(
        modeline_and_echo(&term, SIZE).2,
        "notes.txt changed on disk; C-x C-s again to overwrite, M-x revert-buffer to reload"
    );
    assert_eq!(std::fs::read(&path).expect("read"), b"theirs\n");

    peer.send(b"\x06");
    peer.send(b"\x18\x13");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Hangup);
    assert_eq!(std::fs::read(&path).expect("read"), b"theirs\n");

    peer.send(b"\x18\x13");
    peer.send(b"\x18\x03");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Closed);
    tui.finish(&mut tty).expect("put back");
    assert_eq!(std::fs::read(&path).expect("read"), b"ours base\n");

    let (mut tty, peer) = terminal(SIZE);
    let mut buffer = tui.open(&target(&path)).expect("opens");
    std::fs::write(&path, b"theirs again\n").expect("an edit from elsewhere");
    for key in [&b"mine "[..], b"\x1bx", b"revert-buffer\r"] {
        peer.send(key);
    }
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Hangup);
    let term = shown(&peer.take_output(), SIZE);
    assert_eq!(row(&term, 0), "theirs again");
    let (_, modeline, echo) = modeline_and_echo(&term, SIZE);
    assert_eq!(echo, format!("Reverted {}", path.display()));
    assert!(modeline.starts_with("-UU-:--- "), "{modeline}");
    peer.send(b"\x18\x03");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Closed);
    tui.finish(&mut tty).expect("put back");
    assert_eq!(std::fs::read(&path).expect("read"), b"theirs again\n");
}

/// A SAVE THE FILESYSTEM REFUSES SAYS WHY IN THE SAVE WORDS aterm says too.
#[cfg(unix)]
#[test]
fn a_refused_save_says_why_in_the_save_words() {
    use amacs_files::io::{AtomicSaveStage, SaveCause, SaveTouch};
    use amacs_files::save_words::{Failure, Speaker, words};
    use std::io::ErrorKind;
    use std::os::unix::fs::PermissionsExt as _;

    let mode = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("permissions");
    };
    let (_dir, path) = file("notes.txt", b"base\n");
    let folder = path.parent().expect("a folder").to_path_buf();
    let lock = amacs_files::host::save_lock_of(&path).expect("a save lock");
    let said = |stage, kind, touch| {
        format!(
            "Could not save notes.txt: {}",
            words(
                Speaker::Amacs,
                &path,
                &Failure::Write {
                    stage,
                    cause: Some(SaveCause { kind, touch }),
                },
            )
        )
    };
    let save = |before: &dyn Fn(), after: &dyn Fn()| {
        let (mut tty, peer) = terminal(SIZE);
        let mut tui = tui(Options::default());
        let mut buffer = tui.open(&target(&path)).expect("opens");
        before();
        peer.send(b"x");
        peer.send(b"\x18\x13");
        assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Hangup);
        after();
        let echo = modeline_and_echo(&shown(&peer.take_output(), SIZE), SIZE).2;
        tui.finish(&mut tty).expect("put back");
        echo
    };
    let denied = |stage, touch| said(stage, ErrorKind::PermissionDenied, touch);
    type Case<'a> = (&'a str, &'a dyn Fn(), &'a dyn Fn(), String);
    let cases: [Case<'_>; 4] = [
        (
            "a folder this user cannot write, no save lock yet",
            &|| mode(&folder, 0o500),
            &|| mode(&folder, 0o700),
            denied(AtomicSaveStage::Preflight, SaveTouch::Folder),
        ),
        (
            "a file this user cannot read",
            &|| mode(&path, 0o200),
            &|| mode(&path, 0o600),
            denied(AtomicSaveStage::Preflight, SaveTouch::Read),
        ),
        (
            "a save lock this user cannot open",
            &|| {
                std::fs::write(&lock, b"").expect("a save lock");
                mode(&lock, 0o400);
            },
            &|| mode(&lock, 0o600),
            denied(AtomicSaveStage::Preflight, SaveTouch::Lock),
        ),
        (
            "a folder this user cannot write, the save lock made",
            &|| mode(&folder, 0o500),
            &|| mode(&folder, 0o700),
            denied(AtomicSaveStage::CreateTemporary, SaveTouch::Folder),
        ),
    ];
    for (case, before, after, want) in cases {
        let echo = save(before, after);
        assert_eq!(echo, want, "{case}");
        assert!(
            !echo.contains("os error") && !echo.contains("Preflight"),
            "{case}: {echo}"
        );
        assert_eq!(std::fs::read(&path).expect("read"), b"base\n", "{case}");
    }
    assert!(
        denied(AtomicSaveStage::Preflight, SaveTouch::Lock).contains("amacs can"),
        "the face names itself"
    );
    let held = amacs_files::host::HeldAdvisoryLock::lock(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&lock)
            .expect("the save lock"),
    )
    .expect("hold the save lock");
    let echo = save(&|| {}, &|| {});
    drop(held);
    assert_eq!(
        echo,
        said(
            AtomicSaveStage::Preflight,
            ErrorKind::WouldBlock,
            SaveTouch::Lock
        ),
        "a save lock another writer holds"
    );
    assert!(echo.contains("try again in a moment"), "{echo}");
    assert_eq!(std::fs::read(&path).expect("read"), b"base\n");
}

/// AN OPEN THE FILE HOST REFUSES SAYS WHY IN THE SAVE WORDS.
#[cfg(unix)]
#[test]
fn a_refused_open_says_why_in_the_save_words() {
    use amacs_files::host::DocumentHostError;
    use amacs_files::io::AtomicSaveStage;
    use amacs_files::save_words::{Failure, Speaker, words};
    use std::os::unix::ffi::OsStrExt as _;

    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let folder = std::fs::canonicalize(dir.path()).expect("realpath");
    let refused = |path: &Path| {
        tui(Options::default())
            .open(&target(path))
            .err()
            .unwrap_or_else(|| panic!("{} opened", path.display()))
    };
    let said = |path: &Path, error: DocumentHostError| {
        format!(
            "{}: {}",
            path.display(),
            words(Speaker::Amacs, path, &Failure::Open(&error))
        )
    };
    let link = folder.join("notes.md");
    std::os::unix::fs::symlink(folder.join("nowhere.md"), &link).expect("a link to nothing");
    assert_eq!(
        refused(&link),
        said(
            &link,
            DocumentHostError::SymlinkComponent {
                path: link.clone(),
                dangling: true,
            }
        )
    );
    let unnamed = folder.join(std::ffi::OsStr::from_bytes(b"notes-\xff.md"));
    assert_eq!(
        refused(&unnamed),
        said(&unnamed, DocumentHostError::InvalidEncoding)
    );
    let missing = folder.join("no such folder");
    let lost = missing.join("notes.md");
    assert_eq!(
        refused(&lost),
        said(
            &lost,
            DocumentHostError::Io {
                stage: AtomicSaveStage::Preflight,
                message: format!("{} does not exist", missing.display()),
                kind: Some(std::io::ErrorKind::NotFound),
            }
        )
    );
}

#[test]
fn kill_writes_osc52_when_enabled() {
    let (_dir, path) = file("notes.txt", b"copy me\n");
    let keys: [&[u8]; 3] = [b"\x0b", b"\x18\x03", b"n"];
    let (outcome, output, _) = edit(&path, Options::default(), &keys);
    assert_eq!(outcome, done());
    let osc = b"\x1b]52;c;Y29weSBtZQ==\x07";
    assert!(
        output.windows(osc.len()).any(|window| window == osc),
        "the kill's OSC 52 write"
    );

    let (outcome, output, _) = edit(
        &path,
        Options {
            osc52: false,
            ..Options::default()
        },
        &keys,
    );
    assert_eq!(outcome, done());
    assert!(
        !output.windows(5).any(|window| window == b"\x1b]52;"),
        "no clipboard write with osc52 off"
    );
    assert_eq!(std::fs::read(&path).expect("read"), b"copy me\n");
}

#[test]
fn bom_and_crlf_survive_edit_and_save() {
    let (_dir, path) = file("crlf.txt", b"\xef\xbb\xbfline one\r\nline two\r\n");
    let (outcome, output, _) = edit(&path, Options::default(), &[b"X", b"\x18\x13", b"\x18\x03"]);
    assert_eq!(outcome, done());
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"\xef\xbb\xbfXline one\r\nline two\r\n"
    );
    let term = last_frame(&output, SIZE);
    assert_eq!(row(&term, 0), "Xline one");
    assert_eq!(row(&term, 1), "line two");
}

#[test]
fn a_press_sets_point_and_the_wheel_scrolls() {
    let lines: String = (0..100).map(|n| format!("line {n}\n")).collect();
    let (_dir, path) = file("notes.txt", lines.as_bytes());
    let (mut tty, peer) = terminal(SIZE);
    peer.send(b"\x1b[<0;4;3M\x1b[<0;4;3m");
    peer.send(b"X");
    peer.send(b"\x1b[<65;1;1M");
    let mut tui = tui(Options::default());
    let mut buffer = tui.open(&target(&path)).expect("opens");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Hangup);
    let term = shown(&peer.take_output(), SIZE);
    assert_eq!(row(&term, 0), "line 3", "scrolled three lines");
    assert_eq!(row(&term, 21), "line 24");
    let (_, modeline, _) = modeline_and_echo(&term, SIZE);
    assert!(
        modeline.contains(" L3 C4"),
        "point stayed after the X: {modeline}"
    );
    peer.send(b"\x18\x13\x18\x03");
    assert_eq!(tui.run(&mut tty, &mut buffer).expect("runs"), Close::Closed);
    tui.finish(&mut tty).expect("put back");
    let saved = std::fs::read_to_string(&path).expect("read");
    assert_eq!(saved.lines().nth(2), Some("linXe 2"));
}

#[test]
fn files_in_turn_share_the_kill_ring_and_nothing_else() {
    let (_dir, first) = file("a.txt", b"aaaa\n");
    let second = first.with_file_name("b.txt");
    std::fs::write(&second, b"bbbb\n").expect("the second file");
    let (mut tty, peer) = terminal(SIZE);
    for key in [
        &b"x"[..],
        b"\x0b",
        b"\x19",
        b"\x00",
        b"\x18\x13",
        b"\x18\x03",
        b"\x1by",
        b"\x15\x00",
        b"\x19",
        b"\x18u",
        b"\x18u",
        b"z",
        b"\x18\x13",
        b"\x18\x03",
    ] {
        peer.send(key);
    }
    let outcome = tui(Options::default()).edit(&mut tty, &[target(&first), target(&second)]);
    assert_eq!(outcome, done());
    assert_eq!(std::fs::read(&first).expect("read"), b"xaaaa\n");
    assert_eq!(
        std::fs::read(&second).expect("read"),
        b"zbbbb\n",
        "the second file's undo and pop-mark stayed in it"
    );
    assert!(peer.script_done());
    assert_eq!(peer.modes(), Modes::NONE);
}

#[test]
fn finish_saves_and_abort_ends_the_run() {
    let (_dir, first) = file("a.txt", b"aaaa\n");
    let second = first.with_file_name("b.txt");
    std::fs::write(&second, b"bbbb\n").expect("the second file");
    let unopenable = first.with_file_name("gone").join("c.txt");
    let (mut tty, peer) = terminal(SIZE);
    for key in [&b"x"[..], b"\x03\x03", b"y", b"\x03\x0b"] {
        peer.send(key);
    }
    let outcome = tui(Options::default()).edit(
        &mut tty,
        &[target(&first), target(&second), target(&unopenable)],
    );
    assert_eq!(
        outcome,
        Outcome {
            status: 1,
            said: Vec::new(),
        },
        "an abort is status 1 and the third file is never opened"
    );
    assert_eq!(
        std::fs::read(&first).expect("read"),
        b"xaaaa\n",
        "C-c C-c saved"
    );
    assert_eq!(
        std::fs::read(&second).expect("read"),
        b"bbbb\n",
        "C-c C-k saved nothing"
    );
    assert!(peer.script_done());
    assert_eq!(peer.modes(), Modes::NONE);
}

struct ResizedWhileStopped {
    tty: MemTty,
    size: Size,
    stopped: Option<Size>,
}

impl Tty for ResizedWhileStopped {
    fn read(&mut self, buf: &mut [u8], deadline: Option<Instant>) -> io::Result<Readiness> {
        self.tty.read(buf, deadline)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.tty.write_all(bytes)
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn now(&self) -> Instant {
        self.tty.now()
    }

    fn take_signals(&mut self) -> io::Result<Signals> {
        self.tty.take_signals()
    }

    fn enable(&mut self, mode: Mode) -> io::Result<()> {
        self.tty.enable(mode)
    }

    fn disable(&mut self, mode: Mode) -> io::Result<()> {
        self.tty.disable(mode)
    }

    fn suspend(&mut self) -> io::Result<()> {
        self.tty.suspend()?;
        if let Some(size) = self.stopped.take() {
            self.size = size;
        }
        Ok(())
    }
}

#[test]
fn a_stop_redraws_once_at_the_size_it_continues_at() {
    let (_dir, path) = file("notes.txt", b"alpha\nbeta\n");
    let small = Size { cols: 60, rows: 20 };
    let (tty, peer) = terminal(SIZE);
    let mut tty = ResizedWhileStopped {
        tty,
        size: SIZE,
        stopped: Some(small),
    };
    peer.send(b"\x1a");
    peer.signal_after(Duration::from_millis(5), Signals::TSTP);
    peer.send_after(Duration::from_millis(5), b"\x18\x03");
    let outcome = tui(Options::default()).edit(&mut tty, &[target(&path)]);
    assert_eq!(outcome, done());
    assert_eq!(peer.suspensions(), 2, "C-z and the SIGTSTP each stopped it");
    let output = peer.take_output();
    assert_eq!(
        clears(&output),
        3,
        "the first frame, then one whole frame after each stop"
    );
    let term = last_frame(&output, small);
    assert_eq!(row(&term, 0), "alpha");
    assert_eq!(row(&term, 1), "beta");
    assert!(
        row(&term, 18).starts_with("-UU-:--- notes.txt  (Plain) L1 C0"),
        "the modeline is on the new size's second-last row: {:?}",
        row(&term, 18)
    );
    assert!(reversed(&term, 18, 59), "the modeline spans the new width");
    assert_eq!(peer.modes(), Modes::NONE);
}
