// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let dir = crate::test_scratch_dir("zero-bytes");
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
    let dir = crate::test_scratch_dir("no-wait");
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

    let dir = crate::test_scratch_dir("realpath-uri");
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
    assert_eq!(args.secret, None);
    let Ok(Command::Edit(args)) = parse_args(&os(&["--secret=password,=totp", "a.tsv"])) else {
        panic!("an edit");
    };
    assert_eq!(
        args.secret,
        Some(vec!["password".to_string(), "=totp".to_string()])
    );
    let Ok(Command::Edit(args)) = parse_args(&os(&["--secret=", "a.tsv"])) else {
        panic!("an edit");
    };
    assert_eq!(args.secret, Some(Vec::new()), "`--secret=` names none");
    let Ok(Command::Edit(args)) = parse_args(&os(&[
        "--wait", "+12:4", "a b", "c", "-nw", "--", "-x", "+3",
    ])) else {
        panic!("an edit");
    };
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
        &["--secret=a,a", "a"],
        &["--secret=a", "--secret=b", "a"],
        &["--secret=a b", "a"],
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
        read_only: false,
        secret: None,
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
