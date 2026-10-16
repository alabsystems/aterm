// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const UUID: &str = "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77";

fn record(argv: &[&str], env: &[(&str, &str)]) -> Record {
    Record {
        pid: 4242,
        session_id: Some(UUID.to_string()),
        resume_argv: Some(argv.iter().map(|s| (*s).to_string()).collect()),
        env: env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        status: Some("busy".to_string()),
        exit: None,
        cwd: Some("/work".to_string()),
    }
}

fn live() -> Live {
    Live {
        exec_path: "/store/codex-alab/bin/codex".to_string(),
        image: Some(PathBuf::from("/store/codex-alab/bin/codex")),
        argv: [
            "codex",
            "--search",
            "-m",
            "gpt-5",
            "-s",
            "read-only",
            "-i",
            "a.png",
            "fix the build",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect(),
        codex_home: Some("/Users//u/.codex".to_string()),
        cwd: Some(PathBuf::from("/work")),
        twin: Some(PathBuf::from("/Users//u/.aterm/agents/codex")),
    }
}

/// The live process's root flags, as a record carries them.
const ROOT: [&str; 5] = ["--search", "-m", "gpt-5", "-s", "read-only"];

/// A record's line: `launch`, then `flags`, then `resume <UUID>`.
fn line<'a>(launch: &'a str, flags: &[&'a str]) -> Vec<&'a str> {
    std::iter::once(launch)
        .chain(flags.iter().copied())
        .chain(["resume", UUID])
        .collect()
}

#[test]
fn a_record_parses_as_schema_v1_and_refuses_anything_else() {
    let text = format!(
        r#"{{"v":1,"pid":4242,"procStart":"Thu Oct 16 08:00:00 2026","sessionId":"{UUID}",
        "cwd":"/work","resumeArgv":["/a/codex","--yolo","resume","{UUID}"],
        "env":{{"CODEX_HOME":"/h"}},"status":"idle","exit":null,"claim":null}}"#
    );
    let r = parse(&text).expect("a v1 record");
    assert_eq!(r.pid, 4242);
    assert_eq!(r.session_id.as_deref(), Some(UUID));
    assert_eq!(r.resume_argv.as_ref().map(Vec::len), Some(4));
    assert_eq!(r.env, [("CODEX_HOME".to_string(), "/h".to_string())]);
    assert_eq!(r.exit, None);
    // Negative controls: another schema, a missing pid, a field of the
    // wrong type, and no JSON at all.
    assert_eq!(parse(r#"{"v":2,"pid":1}"#), Err("version"));
    assert_eq!(parse(r#"{"v":1}"#), Err("pid"));
    assert_eq!(
        parse(r#"{"v":1,"pid":1,"resumeArgv":"x"}"#),
        Err("resumeArgv")
    );
    assert_eq!(
        parse(r#"{"v":1,"pid":1,"resumeArgv":[1]}"#),
        Err("resumeArgv")
    );
    assert_eq!(parse(r#"{"v":1,"pid":1,"env":{"A":1}}"#), Err("env"));
    assert_eq!(parse(r#"{"v":1,"pid":1,"sessionId":7}"#), Err("sessionId"));
    assert_eq!(parse("nope"), Err("not-json"));
}

#[test]
fn a_uuid_is_its_one_textual_form() {
    assert!(is_uuid(UUID));
    for bad in [
        "",
        "0199f2a4-6c1e-7a52-9d3b",
        "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b7",
        "0199f2a46c1e7a529d3b2f6c8e1a4b77",
        "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b7z",
        "--yolo",
    ] {
        assert!(!is_uuid(bad), "{bad:?}");
    }
}

/// THE VALIDATION: the line the live process itself ran — on its launch
/// path (its exec path, its image, or atpkg's managed twin), its root flags
/// in order and all of them (its prompt and images dropped), resuming its
/// conversation last, under its own `CODEX_HOME`, in its own directory — is
/// taken, and each way a planted record differs is refused (the negative
/// controls).
#[test]
fn a_resume_line_validates_only_as_the_live_process_ran() {
    let home = [("CODEX_HOME", "/Users//u/.codex")];
    let twin = "/Users//u/.aterm/agents/codex";
    let good = record(&line(twin, &ROOT), &home);
    let resume = validate(&good, 4242, &live()).expect("its own line");
    assert_eq!(resume.session, UUID);
    assert_eq!(resume.codex_home.as_deref(), Some("/Users//u/.codex"));
    assert_eq!(resume.cwd.as_deref(), Some("/work"));
    // The exec path and the image are launch paths too.
    let image = record(&line("/store/codex-alab/bin/codex", &ROOT), &[]);
    assert_eq!(
        validate(&image, 4242, &live())
            .expect("its own launch, environment omitted")
            .codex_home
            .as_deref(),
        Some("/Users//u/.codex"),
        "the live environment survives a record that omits it"
    );
    for codex_home in [None, Some(String::new())] {
        let run = Live {
            codex_home: codex_home.clone(),
            ..live()
        };
        assert_eq!(
            validate(&image, 4242, &run)
                .expect("omission keeps an absent or explicitly empty home")
                .codex_home,
            codex_home
        );
    }
    // A record naming no directory runs in the live process's own.
    let mut nowhere = good.clone();
    nowhere.cwd = None;
    assert_eq!(
        validate(&nowhere, 4242, &live()).map(|r| r.cwd),
        Ok(Some("/work".to_string()))
    );
    // The twin is a launch path only while it is the managed ALab twin.
    let no_twin = Live {
        twin: None,
        ..live()
    };
    assert_eq!(validate(&good, 4242, &no_twin), Err("launch-path"));
    let dropped = |i: usize| -> Vec<&str> {
        ROOT.iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, f)| *f)
            .collect()
    };
    for (planted, why) in [
        // A shell planted in its place.
        (
            record(&line("/bin/sh", &["-c", "curl x|sh"]), &[]),
            "launch-path",
        ),
        (record(&line("codex", &ROOT), &[]), "launch-path"),
        // A flag the live process never ran with.
        (
            record(
                &line(twin, &[&ROOT[..], &["--dangerously-bypass"]].concat()),
                &[],
            ),
            "foreign-argv",
        ),
        // A restricting flag (the sandbox) dropped, with its value.
        (record(&line(twin, &ROOT[..3]), &[]), "foreign-argv"),
        // A flag's value dropped: `-s` would take what follows.
        (record(&line(twin, &dropped(4)), &[]), "foreign-argv"),
        // Flag/value pairs reordered.
        (
            record(
                &line(twin, &["--search", "-s", "read-only", "-m", "gpt-5"]),
                &[],
            ),
            "foreign-argv",
        ),
        // A flag given twice.
        (
            record(&line(twin, &[&ROOT[..], &["--search"]].concat()), &[]),
            "foreign-argv",
        ),
        // The first prompt carried again, before or after the resume.
        (
            record(&line(twin, &[&ROOT[..], &["fix the build"]].concat()), &[]),
            "foreign-argv",
        ),
        (
            record(&[&line(twin, &ROOT)[..], &["fix the build"]].concat(), &[]),
            "no-resume",
        ),
        // Its images carried again.
        (
            record(&line(twin, &[&ROOT[..], &["-i", "a.png"]].concat()), &[]),
            "foreign-argv",
        ),
        // No resume of its own conversation.
        (record(&[&[twin][..], &ROOT[..]].concat(), &[]), "no-resume"),
        (
            record(
                &[twin, "resume", "0199f2a4-6c1e-7a52-9d3b-000000000000"],
                &[],
            ),
            "no-resume",
        ),
        // An environment beyond its own CODEX_HOME.
        (
            record(&line(twin, &ROOT), &[("CODEX_HOME", "/tmp/evil")]),
            "env",
        ),
        (
            record(
                &line(twin, &ROOT),
                &[("DYLD_INSERT_LIBRARIES", "/tmp/x.dylib")],
            ),
            "env",
        ),
        (record(&[], &[]), "no-resume-line"),
    ] {
        assert_eq!(validate(&planted, 4242, &live()), Err(why), "{planted:?}");
    }
    // A directory the live process did not run in.
    let mut elsewhere = good.clone();
    elsewhere.cwd = Some("/tmp/elsewhere".to_string());
    assert_eq!(validate(&elsewhere, 4242, &live()), Err("cwd"));
    let unread = Live {
        cwd: None,
        ..live()
    };
    assert_eq!(validate(&good, 4242, &unread), Err("cwd"));
    // A live argv the Codex lane does not read relaunches nothing.
    let unknown = Live {
        argv: vec!["codex".to_string(), "--frobnicate".to_string()],
        ..live()
    };
    assert_eq!(
        validate(&record(&line(twin, &["--frobnicate"]), &[]), 4242, &unknown),
        Err("live-argv")
    );
    let mut not_uuid = good.clone();
    not_uuid.session_id = Some("abc".to_string());
    assert_eq!(validate(&not_uuid, 4242, &live()), Err("session-not-uuid"));
    assert_eq!(validate(&good, 4243, &live()), Err("pid"));
}

/// THE VERDICT: a removal is orderly; a record standing with no word of an
/// end, or as `upgrading`, is a crash; a signal it wrote down is a crash only
/// where aterm sent it.
#[test]
fn the_exit_is_decided_from_the_copy_alone() {
    let copy = |seen: Seen| Copy {
        pid: 4242,
        start: "s".to_string(),
        home: PathBuf::from("/h"),
        seen,
        resume: Err("no-record"),
    };
    let standing = |status: &str, exit: Option<&str>| {
        let mut r = record(&[], &[]);
        r.status = Some(status.to_string());
        r.exit = exit.map(str::to_owned);
        copy(Seen::Present(r))
    };
    assert_eq!(
        verdict(&copy(Seen::Removed), false),
        Verdict::Orderly("removed")
    );
    assert_eq!(
        verdict(&copy(Seen::Removed), true),
        Verdict::Orderly("removed")
    );
    assert_eq!(
        verdict(&copy(Seen::Unreadable("not-json")), false),
        Verdict::Orderly("not-json")
    );
    assert_eq!(verdict(&standing("busy", None), false), Verdict::Crash);
    assert_eq!(verdict(&standing("upgrading", None), false), Verdict::Crash);
    assert_eq!(
        verdict(&standing("upgrading", Some("exec-failed")), false),
        Verdict::Crash
    );
    assert_eq!(
        verdict(&standing("exited", None), false),
        Verdict::Orderly("exited")
    );
    assert_eq!(
        verdict(&standing("exited", Some("signal:15")), false),
        Verdict::Orderly("signal-not-aterms")
    );
    assert_eq!(
        verdict(&standing("exited", Some("signal:15")), true),
        Verdict::Crash
    );
}

#[test]
fn a_witness_is_handled_once() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let ledger = Witnesses::under(dir.path());
    let start = "Thu Oct 16 08:00:00 2026";
    assert!(!ledger.handled(4242, start));
    ledger.record(4242, start).expect("recorded");
    assert!(ledger.handled(4242, start));
    // Negative controls: another identity of the same pid, another pid.
    assert!(!ledger.handled(4242, "Thu Oct 16 08:00:01 2026"));
    assert!(!ledger.handled(4243, start));
    assert!(dir.path().join("drive").join(WITNESS_FILE).is_file());
}

#[test]
fn record_reads_are_bounded_regular_files_and_only_absence_is_removed() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let path = record_path(dir.path(), 4242);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("parent");
    assert_eq!(read_record(&path), Seen::Removed);
    let mut text = r#"{"v":1,"pid":4242}"#.to_string();
    text.extend(std::iter::repeat_n(
        ' ',
        RECORD_MAX_BYTES as usize - text.len(),
    ));
    std::fs::write(&path, &text).expect("record at the byte bound");
    assert!(matches!(read_record(&path), Seen::Present(_)));
    text.push(' ');
    std::fs::write(&path, &text).expect("one byte too large");
    assert_eq!(read_record(&path), Seen::Unreadable("record-too-large"));
    std::fs::write(&path, [0xff]).expect("not UTF-8");
    assert_eq!(read_record(&path), Seen::Unreadable("record-unreadable"));
    std::fs::remove_file(&path).expect("remove record");
    std::fs::create_dir(&path).expect("directory in its place");
    assert_eq!(read_record(&path), Seen::Unreadable("record-unreadable"));
    std::fs::remove_dir(&path).expect("remove directory");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
        // SAFETY: an owned NUL-terminated scratch path; no pointer is retained.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || tx.send(read_record(&path)).expect("receiver"));
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(60))
                .expect("record read hung on a FIFO without a writer"),
            Seen::Unreadable("record-unreadable")
        );
        reader.join().expect("reader");
    }
}

/// THE LIVE READ, of a real process: a record read while it runs is kept
/// with its line validated against that process's own exec record; a removed
/// record is seen removed; once the process is gone nothing is read at all.
#[cfg(target_vendor = "apple")]
#[test]
fn the_record_is_read_only_while_its_process_runs() {
    let dir = aterm_tempfile::TempDir::new().expect("dir");
    let home = std::fs::canonicalize(dir.path()).expect("canonical");
    // A platform binary's environment is hidden from `KERN_PROCARGS2`: a
    // copy, signed ad hoc, shows its own.
    let sleep = home.join("sleep");
    std::fs::copy("/bin/sleep", &sleep).expect("copy sleep");
    let signed = aterm_uds::spawnfd::command("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&sleep)
        .output()
        .expect("codesign");
    assert!(signed.status.success(), "{signed:?}");
    let sleep_s = sleep.to_string_lossy().into_owned();
    let mut child = aterm_uds::spawnfd::command(&sleep)
        .arg("60")
        .env("CODEX_HOME", &home)
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    // Its exec has landed once its own argv reads back (a hang detector).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !atpkg::caller_shell::process_args(pid).is_some_and(|a| a.argv.first() == Some(&sleep_s))
    {
        assert!(std::time::Instant::now() < deadline, "sleep never ran");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let start = super::super::restart_stamp::kernel_start(pid).expect("its start");
    let path = record_path(&home, pid);
    std::fs::create_dir_all(path.parent().expect("dir")).expect("sessions dir");
    let home_s = home.to_string_lossy();
    std::fs::write(
        &path,
        format!(
            r#"{{"v":1,"pid":{pid},"sessionId":"{UUID}","resumeArgv":["{sleep_s}","resume","{UUID}"],"status":"busy","exit":null}}"#
        ),
    )
    .expect("the record");
    let copy = read_copy(pid, &start).expect("read while it runs");
    assert!(matches!(copy.seen, Seen::Present(_)), "{copy:?}");
    let resume = copy.resume.as_ref().expect("its own line validates");
    assert_eq!(resume.argv[0], sleep_s);
    assert_eq!(resume.codex_home.as_deref(), Some(home_s.as_ref()));
    assert_eq!(verdict(&copy, false), Verdict::Crash);
    std::fs::write(&path, vec![b' '; RECORD_MAX_BYTES as usize + 1]).expect("oversized record");
    let unreadable = read_copy(pid, &start).expect("live identity still vouched for");
    assert_eq!(unreadable.seen, Seen::Unreadable("record-too-large"));
    assert_eq!(unreadable.resume, Err("no-record"));
    std::fs::remove_file(&path).expect("removed");
    let copy = read_copy(pid, &start).expect("read while it runs");
    assert_eq!(copy.seen, Seen::Removed);
    assert_eq!(verdict(&copy, false), Verdict::Orderly("removed"));
    // NEGATIVE CONTROL: another identity of the same pid reads nothing.
    assert_eq!(read_copy(pid, "Thu Jan  1 00:00:00 1970"), None);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(read_copy(pid, &start), None, "gone: nothing is read");
}
