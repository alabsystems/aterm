// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kimi Code's record of a pending approval, read off the lines Kimi Code 2.5.2 wrote.
//!
//! Every `fixtures/kimi-wire/kimi-2.5.2-*.jsonl` file is MEASURED, copied byte for byte from one
//! session's `agents/main/wire.jsonl` in `aterm-lab kimi kw3` (144x50, 2026-11-07): `head` holds
//! that wire's small opening records in order (the system prompt and tool list left out), each
//! `request-*` the `interaction.request` written while its box stood, and each `resolved-*` the
//! line its answer appended. `request` is `cat README.md` (approved); `request-backspaced` is
//! `git push --force`, 16 backspaces, `cat README.md` and three spaces (rejected), which the box
//! drew as `$ cat README.md`, the same row the honest read draws (aterm-phase's
//! `kimi-2.5.2-box-shell-backspaced-push.txt` and `kimi-2.5.2-box-shell-read.txt`, the screens of
//! these two boxes); `request-write` and `request-edit` are a Write and an Edit of `notes.txt`;
//! `request-question` is an `AskUserQuestion` (measured the same way in `aterm-lab kimi kw2`), an
//! interaction of kind `question` that names no tool.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::fixtures::{
    BACKSPACED, HEAD, REQUEST, REQUEST_BACKSPACED, REQUEST_EDIT, REQUEST_QUESTION, REQUEST_WRITE,
    RESOLVED_APPROVED, RESOLVED_REJECTED,
};
use super::{Bound, Pending, Verdict, Wire, kimi_home_of, lone_surrogates_mended, verdict};

fn folded(lines: &[&str]) -> Wire {
    let mut wire = Wire::default();
    wire.advance(lines.concat().as_bytes());
    wire
}

fn commands(wire: &Wire) -> Vec<(String, Option<String>)> {
    wire.pending()
        .expect("the record reads")
        .iter()
        .map(|p| (p.tool.clone(), p.command.clone()))
        .collect()
}

fn readings(of: &[&str]) -> Vec<String> {
    of.iter().map(|r| (*r).to_string()).collect()
}

fn pending_of(lines: &[&str]) -> Vec<Pending> {
    folded(lines).pending().expect("the record reads").to_vec()
}

#[test]
fn a_box_that_waits_is_its_request_until_the_answer_with_its_id() {
    let open = folded(&[HEAD, REQUEST]);
    assert_eq!(
        commands(&open),
        [("Bash".to_string(), Some("cat README.md".to_string()))]
    );
    assert!(
        commands(&folded(&[HEAD, REQUEST, RESOLVED_APPROVED])).is_empty(),
        "an answer with the request's id closes it"
    );
    assert_eq!(
        commands(&folded(&[HEAD, REQUEST, RESOLVED_REJECTED])).len(),
        1,
        "NEGATIVE CONTROL: an answer to another request leaves this one open"
    );
    assert!(
        commands(&folded(&[HEAD])).is_empty(),
        "no request, nothing open"
    );
}

#[test]
fn a_question_is_no_approval_and_leaves_the_record_readable() {
    assert_eq!(
        commands(&folded(&[HEAD, REQUEST, REQUEST_QUESTION])),
        [("Bash".to_string(), Some("cat README.md".to_string()))],
        "a question names no tool and is not read as an approval"
    );
}

#[test]
fn the_record_holds_the_command_its_backspaces_redraw() {
    let wire = folded(&[HEAD, REQUEST_BACKSPACED]);
    assert_eq!(
        commands(&wire),
        [("Bash".to_string(), Some(BACKSPACED.to_string()))]
    );
    // The box drew `$ cat README.md`, the honest read's row, which the screen's rule passes.
    let drawn = readings(&["cat README.md"]);
    assert_eq!(
        verdict(wire.pending(), &drawn, &[], 0),
        Verdict::NotReadOnly,
        "the command that runs is a push"
    );
    assert_eq!(
        verdict(None, &drawn, &[], 0),
        Verdict::Withheld,
        "NEGATIVE CONTROL: with no record nothing is lent"
    );
}

#[test]
fn every_pending_request_must_pass_and_one_must_be_what_the_box_draws() {
    let drawn = readings(&["cat README.md", "cat README.md Show the readme"]);
    let plain = pending_of(&[HEAD, REQUEST]);
    assert_eq!(verdict(Some(&plain), &drawn, &[], 0), Verdict::ReadOnly);

    // Two calls in parallel, an honest read and the spoof: while the spoof's box stands the
    // read is pending too, and it is what the screen draws.
    let both = pending_of(&[HEAD, REQUEST, REQUEST_BACKSPACED]);
    assert_eq!(both.len(), 2);
    assert_eq!(
        verdict(Some(&both), &drawn, &[], 0),
        Verdict::NotReadOnly,
        "a pending spoof withholds the honest read's verdict"
    );
    let honest_only: Vec<Pending> = both.iter().take(1).cloned().collect();
    assert_eq!(
        verdict(Some(&honest_only), &drawn, &[], 0),
        Verdict::ReadOnly,
        "NEGATIVE CONTROL: without the spoof the read is read-only"
    );

    assert_eq!(
        verdict(Some(&plain), &readings(&["cat  README.md"]), &[], 0),
        Verdict::Withheld,
        "a box drawn other than the record's command binds nothing"
    );
    assert_eq!(
        verdict(Some(&[]), &drawn, &[], 0),
        Verdict::Withheld,
        "nothing pending, nothing bound"
    );
}

#[test]
fn a_write_or_an_edit_pending_is_no_command_to_lend() {
    for (lines, tool) in [
        (&[HEAD, REQUEST_WRITE][..], "Write"),
        (&[HEAD, REQUEST_EDIT][..], "Edit"),
    ] {
        let pending = pending_of(lines);
        assert_eq!(
            pending
                .iter()
                .map(|p| (p.tool.as_str(), p.command.as_deref()))
                .collect::<Vec<_>>(),
            [(tool, None)],
            "a file_io display carries no command"
        );
        let beside: Vec<Pending> = pending_of(&[HEAD, REQUEST])
            .into_iter()
            .chain(pending)
            .collect();
        assert_eq!(
            verdict(Some(&beside), &readings(&["cat README.md"]), &[], 0),
            Verdict::NotReadOnly,
            "{tool} pending beside the read may be the box"
        );
    }
}

#[test]
fn a_command_with_a_control_character_never_passes_even_where_the_rule_would() {
    let line = REQUEST.replace("cat README.md\"", "cat README.md\\t\"");
    let pending = pending_of(&[HEAD, &line]);
    assert_eq!(pending[0].command.as_deref(), Some("cat README.md\t"));
    assert_eq!(
        verdict(Some(&pending), &readings(&["cat README.md\t"]), &[], 0),
        Verdict::NotReadOnly
    );
    let newline = REQUEST.replace("cat README.md\"", "cat README.md\\nls -la\"");
    let pending = pending_of(&[HEAD, &newline]);
    assert_eq!(
        verdict(
            Some(&pending),
            &readings(&["cat README.md\nls -la"]),
            &[],
            0
        ),
        Verdict::ReadOnly,
        "NEGATIVE CONTROL: a newline is a line of the command, not a control character"
    );
}

#[test]
fn a_partial_or_unparsed_line_binds_nothing() {
    let torn = &REQUEST[..REQUEST.len() - 1];
    let mut wire = Wire::default();
    wire.advance(format!("{HEAD}{torn}").as_bytes());
    assert_eq!(wire.pending(), None, "an append in progress");
    let mut whole = Wire::default();
    whole.advance(format!("{HEAD}{REQUEST}").as_bytes());
    assert_eq!(
        whole.pending().map(<[Pending]>::len),
        Some(1),
        "NEGATIVE CONTROL: the same line whole binds"
    );

    let unparsed = REQUEST.replacen('{', "[", 1);
    assert_eq!(folded(&[HEAD, &unparsed]).pending(), None);
    let unnamed = REQUEST.replace("\"toolName\"", "\"tool\"");
    assert_eq!(
        folded(&[HEAD, &unnamed]).pending(),
        None,
        "a request with no tool is not this reader's record"
    );
    let untimed = REQUEST.replace("\"time\":", "\"when\":");
    assert_eq!(
        folded(&[HEAD, &untimed]).pending(),
        None,
        "nor one that does not say when it was asked"
    );
    let unanswerable = RESOLVED_APPROVED.replace("\"id\":", "\"ref\":");
    assert_eq!(
        folded(&[HEAD, REQUEST, &unanswerable]).pending(),
        None,
        "nor an answer that names no request"
    );
}

/// Only a line holding an event type the fold reads, as a JSON string, is parsed, so a line that
/// holds neither leaves the record readable however it is written: here one torn in a longer
/// string that holds the request's type, after it or before it. Control: the same torn line
/// holding the request's type, or the answer's, is parsed and leaves the record unread.
#[test]
fn a_line_holding_no_type_the_fold_reads_is_never_parsed() {
    let torn = |word: &str| format!("{{\"type\":\"{word}\",\"id\":\n");
    for other in ["interaction.requested", "re.interaction.request"] {
        assert_eq!(
            folded(&[HEAD, &torn(other), REQUEST])
                .pending()
                .map(<[Pending]>::len),
            Some(1),
            "a line holding {other:?}, no type the fold reads"
        );
    }
    for word in ["interaction.request", "interaction.resolved"] {
        assert_eq!(
            folded(&[HEAD, &torn(word), REQUEST]).pending(),
            None,
            "NEGATIVE CONTROL: a torn line holding {word:?}"
        );
    }
}

#[test]
fn a_request_asked_before_the_last_input_lends_nothing() {
    let drawn = readings(&["cat README.md"]);
    let plain = pending_of(&[HEAD, REQUEST]);
    let asked = plain[0].asked;
    assert_eq!(asked, 1_794_115_339_426, "the request's own time");
    assert_eq!(
        verdict(Some(&plain), &drawn, &[], asked),
        Verdict::Withheld,
        "input written as it was asked may have answered it"
    );
    assert_eq!(
        verdict(Some(&plain), &drawn, &[], asked - 1),
        Verdict::ReadOnly,
        "NEGATIVE CONTROL: one asked after the last input is the box's"
    );
    let both = pending_of(&[HEAD, REQUEST, REQUEST_BACKSPACED]);
    assert_eq!(
        verdict(Some(&both), &drawn, &[], both[1].asked),
        Verdict::NotReadOnly,
        "a request that lends nothing still fails a box"
    );
}

#[test]
fn a_surrogate_kimi_cut_from_its_pair_reads_as_the_character_node_writes() {
    let cut = REQUEST.replace(
        "\"action\":\"Running: cat README.md\"",
        "\"action\":\"Running: cat README.md \\ud83d\"",
    );
    assert_ne!(cut, REQUEST);
    assert!(
        aterm_json::from_slice::<aterm_json::Value>(cut.trim_end().as_bytes()).is_err(),
        "PRECONDITION: a strict parser refuses the lone half"
    );
    assert_eq!(
        commands(&folded(&[HEAD, &cut])),
        [("Bash".to_string(), Some("cat README.md".to_string()))],
        "the request reads"
    );
    let mended =
        |line: &str| String::from_utf8(lone_surrogates_mended(line.as_bytes()).into_owned());
    assert_eq!(mended(r"a\ud83d").as_deref(), Ok(r"a\ufffd"));
    assert_eq!(mended(r"\ude80\ud83d x").as_deref(), Ok(r"\ufffd\ufffd x"));
    assert_eq!(mended(r"\ud83d\u0041").as_deref(), Ok(r"\ufffd\u0041"));
    assert_eq!(
        mended(r"\ud83d x\ude80").as_deref(),
        Ok(r"\ufffd x\ufffd"),
        "halves apart are no pair"
    );
    for kept in [r"\ud83d\ude80", r"\\ud83d", r"\u00e9 \n \\", r"\ud83"] {
        assert!(
            matches!(
                lone_surrogates_mended(kept.as_bytes()),
                std::borrow::Cow::Borrowed(_)
            ),
            "NEGATIVE CONTROL: {kept} is left as it is"
        );
    }
}

#[test]
fn a_home_is_the_folder_of_the_projects_file_kimi_holds_open() {
    let open = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    // As `lsof -p` named a 2.5.2 Kimi's files in `aterm-lab kimi kr1`, the jail's folders
    // shortened.
    let held = open(&[
        "/p/proj",
        "/s/kimi/bin/kimi",
        "/h/Library/Caches/kimi-code/native/2.5.2/clipboard.darwin-arm64.node",
        "/h/.kimi-code/projects.json",
        "/h/.kimi-code/config.toml",
    ]);
    assert_eq!(
        kimi_home_of(&held).as_deref(),
        Some(Path::new("/h/.kimi-code"))
    );
    assert_eq!(
        kimi_home_of(&open(&["/h/.kimi-code/config.toml"])),
        None,
        "NEGATIVE CONTROL: no projects file held, no home"
    );
    assert_eq!(
        kimi_home_of(&open(&["/h/.kimi-code/projects.json", "/k/projects.json"])),
        None,
        "two homes held name neither"
    );
    assert_eq!(
        kimi_home_of(&open(&[
            "/h/.kimi-code/projects.json",
            "/h/.kimi-code/projects.json"
        ])),
        Some(PathBuf::from("/h/.kimi-code")),
        "one home held twice is one home"
    );
    assert_eq!(
        kimi_home_of(&open(&["projects.json"])),
        None,
        "a relative name"
    );
}

/// A Kimi home holding one session in `proj`, its main wire written with `lines`.
struct Home {
    _dir: aterm_tempfile::TempDir,
    home: std::path::PathBuf,
    proj: std::path::PathBuf,
    session: std::path::PathBuf,
}

impl Home {
    fn new(lines: &[&str]) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("kimi-wire-")
            .tempdir()
            .expect("temp dir");
        let home = dir.path().join("kimi");
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).expect("proj");
        let proj = std::fs::canonicalize(&proj).expect("canonical");
        let session = home.join("sessions/wd_proj_0ff709967ef2/session_32518b39");
        std::fs::create_dir_all(session.join("agents/main")).expect("agents");
        std::fs::write(
            session.join("state.json"),
            format!(
                "{{\"id\":\"session_32518b39\",\"version\":2,\"cwd\":{}}}",
                aterm_json::to_string(&proj.display().to_string()).expect("json")
            ),
        )
        .expect("state");
        let made = Self {
            _dir: dir,
            home,
            proj,
            session,
        };
        made.write("agents/main", lines);
        made
    }

    fn write(&self, agent: &str, lines: &[&str]) {
        let dir = self.session.join(agent);
        std::fs::create_dir_all(&dir).expect("agent dir");
        std::fs::write(dir.join("wire.jsonl"), lines.concat()).expect("wire");
    }

    fn append(&self, agent: &str, line: &str) {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(self.session.join(agent).join("wire.jsonl"))
            .expect("open");
        f.write_all(line.as_bytes()).expect("append");
    }

    fn read(&self, bound: &mut Bound) -> Option<Vec<(String, Option<String>)>> {
        bound
            .read(&self.home, &self.proj, SystemTime::UNIX_EPOCH)
            .map(|p| p.into_iter().map(|p| (p.tool, p.command)).collect())
    }
}

#[test]
fn a_session_in_the_process_folder_binds_its_agents_wires() {
    let home = Home::new(&[HEAD, REQUEST]);
    let mut bound = Bound::default();
    assert_eq!(
        home.read(&mut bound),
        Some(vec![(
            "Bash".to_string(),
            Some("cat README.md".to_string())
        )])
    );
    home.write("agents/sub-1", &[HEAD, REQUEST_BACKSPACED]);
    assert_eq!(
        home.read(&mut bound).map(|p| p.len()),
        Some(2),
        "a subagent's wire under agents/ is read too"
    );

    // A session in the same folder that has asked nothing yet has no agents/ folder.
    let fresh = home.session.with_file_name("session_fresh");
    std::fs::create_dir_all(&fresh).expect("fresh");
    std::fs::copy(home.session.join("state.json"), fresh.join("state.json")).expect("state");
    assert_eq!(
        home.read(&mut bound).map(|p| p.len()),
        Some(2),
        "a session with no agents folder yet holds no wire"
    );

    let elsewhere = home.proj.join("sub");
    std::fs::create_dir_all(&elsewhere).expect("sub");
    assert_eq!(
        Bound::default().read(&home.home, &elsewhere, SystemTime::UNIX_EPOCH),
        None,
        "NEGATIVE CONTROL: a session in another folder binds nothing"
    );
}

#[test]
fn a_wire_older_than_the_asking_process_is_not_read() {
    // Its request asked late (2100-01-01), so only the wire's age can keep it out.
    let late = REQUEST_BACKSPACED.replace("\"time\":1794115361243", "\"time\":4102444800000");
    assert_ne!(late, REQUEST_BACKSPACED);
    let home = Home::new(&[HEAD, &late]);
    let written = std::fs::metadata(home.session.join("agents/main/wire.jsonl"))
        .and_then(|m| m.modified())
        .expect("mtime");
    assert_eq!(
        Bound::default().read(&home.home, &home.proj, written + Duration::from_secs(1)),
        None,
        "a wire not written since the asking process started is an earlier Kimi's"
    );
    assert_eq!(
        Bound::default()
            .read(&home.home, &home.proj, written)
            .map(|p| p.len()),
        Some(1),
        "NEGATIVE CONTROL: a wire written at the start is read"
    );
}

#[test]
fn a_request_an_earlier_kimi_left_open_is_not_this_ones() {
    // Resumed, the session's wire is appended to after this process started, but the request
    // the earlier Kimi died asking stays open in it.
    let home = Home::new(&[HEAD, REQUEST]);
    let asked = pending_of(&[REQUEST])[0].asked;
    let at = |ms: u64| SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
    assert_eq!(
        Bound::default().read(&home.home, &home.proj, at(asked + 1)),
        Some(vec![]),
        "a request asked before the process started is no request of its"
    );
    assert_eq!(
        Bound::default()
            .read(&home.home, &home.proj, at(asked))
            .map(|p| p.len()),
        Some(1),
        "NEGATIVE CONTROL: one asked as it started is"
    );
}

#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_read_leaves_the_record_unread() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = Home::new(&[HEAD, REQUEST]);
    let lock = |dir: &Path, mode: u32| {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    let read = || {
        Bound::default()
            .read(&home.home, &home.proj, SystemTime::UNIX_EPOCH)
            .map(|p| p.len())
    };
    // Another session beside it, working in another folder, with a folder it cannot read.
    let foreign = home.session.with_file_name("session_foreign");
    let theirs = foreign.join("agents/sub-1");
    std::fs::create_dir_all(&theirs).expect("theirs");
    std::fs::write(foreign.join("state.json"), "{\"cwd\":\"/elsewhere\"}").expect("state");
    let ours = home.session.join("agents/sub-1");
    std::fs::create_dir_all(&ours).expect("ours");
    lock(&theirs, 0);
    let refused = std::fs::read_dir(&theirs).is_err();
    let beside = read();
    lock(&ours, 0);
    let hidden = read();
    lock(&ours, 0o755);
    lock(&theirs, 0o755);
    if refused {
        assert_eq!(hidden, None, "the wire it hides may hold the box's request");
    }
    assert_eq!(
        beside,
        Some(1),
        "NEGATIVE CONTROL: another folder's session's unread folder is no part of this record"
    );
    assert_eq!(read(), Some(1), "readable again, the record reads");
}

#[test]
fn each_read_folds_what_was_appended_and_a_rewrite_reads_again() {
    let home = Home::new(&[HEAD]);
    let mut bound = Bound::default();
    assert_eq!(home.read(&mut bound), Some(vec![]));
    let torn = &REQUEST[..REQUEST.len() / 2];
    home.append("agents/main", torn);
    assert_eq!(
        home.read(&mut bound),
        None,
        "an append in progress binds nothing"
    );
    home.append("agents/main", &REQUEST[REQUEST.len() / 2..]);
    assert_eq!(
        home.read(&mut bound).map(|p| p.len()),
        Some(1),
        "the torn line is read whole once its rest lands"
    );
    home.append("agents/main", RESOLVED_APPROVED);
    assert_eq!(home.read(&mut bound), Some(vec![]));

    // Kimi rewrites a wire by an atomic rename: the new file holds the request again.
    let wire = home.session.join("agents/main/wire.jsonl");
    let next = home.session.join("agents/main/wire.jsonl.next");
    let mut rewritten = format!("{HEAD}{REQUEST_BACKSPACED}");
    while rewritten.len() < std::fs::metadata(&wire).expect("len").len() as usize {
        rewritten.push_str(HEAD);
    }
    std::fs::write(&next, &rewritten).expect("next");
    std::fs::rename(&next, &wire).expect("rename");
    assert_eq!(
        home.read(&mut bound),
        Some(vec![("Bash".to_string(), Some(BACKSPACED.to_string()))]),
        "a rewritten wire, no shorter, is read again from its start"
    );
}

#[test]
fn a_wire_cut_short_in_place_is_read_again_from_its_start() {
    let home = Home::new(&[HEAD, REQUEST, RESOLVED_APPROVED]);
    let mut bound = Bound::default();
    assert_eq!(home.read(&mut bound), Some(vec![]));
    // The same file, written shorter: its identity stands, only its length tells.
    home.write("agents/main", &[REQUEST_BACKSPACED]);
    assert_eq!(
        home.read(&mut bound),
        Some(vec![("Bash".to_string(), Some(BACKSPACED.to_string()))]),
        "a file shorter than the offset is read again from 0"
    );
}

#[test]
fn a_request_far_from_the_end_still_binds() {
    let padding: String = std::iter::repeat_n(HEAD, (96 * 1024) / HEAD.len() + 1).collect();
    let home = Home::new(&[HEAD, REQUEST_BACKSPACED, &padding]);
    assert!(padding.len() > 64 * 1024);
    assert_eq!(
        home.read(&mut Bound::default()).map(|p| p.len()),
        Some(1),
        "the whole stream is read, not a tail window"
    );
}
