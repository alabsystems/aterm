// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn actual_keeper_start_survives_scheduling_delay() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let socket = scratch.path().join("start.sock");
    let mut server = crate::server::Server::bind(crate::server::ServeConfig {
        socket: socket.clone(),
        identity: Identity::same_uid(),
        relaunch: crate::server::Relauncher::Unavailable,
        version: "scheduled-start".into(),
        test_drop_custody: false,
    })
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let worker = std::thread::spawn(move || {
        while !done.load(Ordering::SeqCst) {
            server.step(10).unwrap();
        }
    });
    let accepted = wait_for_start(
        &socket,
        &Identity::same_uid(),
        "start-test",
        std::time::Instant::now() - Duration::from_secs(11),
    );
    let expired = wait_for_start(
        &socket,
        &Identity::same_uid(),
        "start-test",
        std::time::Instant::now() - Duration::from_secs(61),
    );
    stop.store(true, Ordering::SeqCst);
    worker.join().unwrap();
    assert!(accepted);
    assert!(!expired);
}

#[cfg(unix)]
#[test]
fn only_a_job_told_to_relaunch_may_reopen_the_app() {
    assert_eq!(
        serve_argv("/s", "designated", true),
        [
            "keeper",
            "serve",
            "--sock",
            "/s",
            "--identity",
            "designated"
        ]
    );
    assert_eq!(
        serve_argv("/s", "uid", false),
        [
            "keeper",
            "serve",
            "--sock",
            "/s",
            "--identity",
            "uid",
            "--no-relaunch"
        ]
    );
    let parsed = parse(
        &serve_argv("/s", "uid", false)[1..]
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>(),
    )
    .expect("the verb reads its own argv");
    assert!(parsed.no_relaunch);
}

#[cfg(unix)]
#[test]
fn a_dial_error_is_read_as_what_it_is() {
    use std::io::{Error, ErrorKind};
    let refused = Error::new(
        ErrorKind::PermissionDenied,
        crate::client::NotThisBuild("OSStatus -67050".into()),
    );
    assert_eq!(NoKeeper::of(&refused), NoKeeper::NotOurs);
    let eacces = Error::from(ErrorKind::PermissionDenied);
    assert!(matches!(NoKeeper::of(&eacces), NoKeeper::Failed(_)));
    assert_eq!(
        NoKeeper::of(&Error::from(ErrorKind::NotFound)),
        NoKeeper::Nothing
    );
    assert_eq!(
        NoKeeper::of(&Error::from(ErrorKind::ConnectionRefused)),
        NoKeeper::Nothing
    );
    assert_eq!(
        NoKeeper::of(&Error::other("timed out")),
        NoKeeper::Failed("timed out".into())
    );
}

#[cfg(unix)]
fn keeper_fields(s: &str) -> Vec<&str> {
    s.split([' ', ';'])
        .take_while(|t| !t.starts_with('('))
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(unix)]
#[test]
fn stop_says_stopped_only_when_the_keeper_ended() {
    let birth = crate::wire::Birth {
        seconds: 1,
        micros: 2,
    };
    let reused = crate::wire::Birth {
        seconds: 3,
        micros: 0,
    };
    let short = Duration::from_millis(50);
    assert!(!await_end(7, birth, short, |_| Some(birth)));
    assert!(await_end(7, birth, short, |_| None));
    assert!(await_end(7, birth, short, |_| Some(reused)));
    assert_eq!(
        keeper_fields(&still_running_line("L", 7)),
        ["keeper=running", "label=L", "pid=7"]
    );
    assert_eq!(
        stop_absent_line("L", None),
        ("job=absent label=L".to_string(), true)
    );
    let (with_pid, ok) = stop_absent_line("L", Some((std::path::Path::new("/p/k.sock"), Some(42))));
    assert!(!ok);
    assert_eq!(
        keeper_fields(&with_pid),
        [
            "keeper=running",
            "label=L",
            "job=absent",
            "pid=42",
            "socket=/p/k.sock"
        ]
    );
    let (no_pid, ok) = stop_absent_line("L", Some((std::path::Path::new("/p/k.sock"), None)));
    assert!(!ok);
    assert_eq!(
        keeper_fields(&no_pid),
        [
            "keeper=running",
            "label=L",
            "job=absent",
            "socket=/p/k.sock"
        ]
    );
    assert_eq!(
        status_pid("keeper=running\npid=4242 version=0.70.0\nsocket=/p/k.sock"),
        Some(4242)
    );
    assert_eq!(status_pid("keeper=running\nsocket=/p/k.sock"), None);
}

#[cfg(unix)]
#[test]
fn start_says_running_only_when_this_builds_keeper_answers() {
    use crate::job::JobState;
    const REMEDY: &str = "aterm keeper stop, then aterm keeper start";
    let check = |got: Option<(String, bool)>,
                 want_fields: &[&str],
                 want_ok: bool,
                 want_remedy: bool,
                 want_tail: Option<&str>| {
        let (text, ok) = got.expect("decision line");
        assert_eq!(ok, want_ok, "{text}");
        assert_eq!(keeper_fields(&text), want_fields, "{text}");
        assert_eq!(text.contains(REMEDY), want_remedy, "{text}");
        if let Some(tail) = want_tail {
            assert!(text.contains(tail), "{text} missing {tail:?}");
        }
    };
    let sock = std::path::Path::new("/p/k.sock");
    let log = std::path::Path::new("/p/k.sock.log");
    let say = |job: JobState, answer: Result<String, NoKeeper>, log| {
        start_without_submit("L", sock, true, &job, &answer, log)
    };
    let ours = || Ok("keeper=running".to_string());
    assert_eq!(say(JobState::Absent, Err(NoKeeper::Nothing), None), None);
    assert_eq!(
        say(JobState::Absent, Err(NoKeeper::Failed("x".into())), None),
        None
    );
    check(
        say(JobState::Loaded { pid: Some(7) }, ours(), None),
        &["keeper=running", "label=L", "pid=7", "socket=/p/k.sock"],
        true,
        false,
        None,
    );
    check(
        say(JobState::Loaded { pid: None }, ours(), None),
        &[
            "keeper=running",
            "label=L",
            "job=loaded",
            "socket=/p/k.sock",
        ],
        true,
        false,
        None,
    );
    check(
        start_without_submit(
            "L",
            sock,
            false,
            &JobState::Loaded { pid: Some(7) },
            &ours(),
            None,
        ),
        &[
            "keeper=running",
            "label=L",
            "job=running",
            "pid=7",
            "socket=/p/k.sock",
        ],
        true,
        false,
        None,
    );
    check(
        say(
            JobState::Loaded { pid: None },
            Err(NoKeeper::Nothing),
            Some(log),
        ),
        &["keeper=absent", "label=L", "job=loaded", "socket=/p/k.sock"],
        false,
        false,
        Some("/p/k.sock.log"),
    );
    check(
        say(
            JobState::Loaded { pid: Some(7) },
            Err(NoKeeper::Failed("timed out".into())),
            None,
        ),
        &[
            "keeper=absent",
            "label=L",
            "job=running",
            "pid=7",
            "socket=/p/k.sock",
        ],
        false,
        false,
        Some("timed out"),
    );
    check(
        say(
            JobState::Loaded { pid: Some(7) },
            Err(NoKeeper::NotOurs),
            None,
        ),
        &["keeper=refused", "label=L", "pid=7", "socket=/p/k.sock"],
        false,
        true,
        None,
    );
    check(
        start_without_submit(
            "L",
            sock,
            false,
            &JobState::Loaded { pid: Some(7) },
            &Err(NoKeeper::NotOurs),
            None,
        ),
        &[
            "keeper=refused",
            "label=L",
            "job=running",
            "pid=7",
            "socket=/p/k.sock",
        ],
        false,
        false,
        None,
    );
    check(
        say(JobState::Loaded { pid: None }, Err(NoKeeper::NotOurs), None),
        &[
            "keeper=refused",
            "label=L",
            "job=loaded",
            "socket=/p/k.sock",
        ],
        false,
        false,
        None,
    );
    check(
        say(JobState::Absent, Err(NoKeeper::NotOurs), None),
        &[
            "keeper=refused",
            "label=L",
            "job=absent",
            "socket=/p/k.sock",
        ],
        false,
        false,
        None,
    );
    check(
        say(JobState::Absent, ours(), None),
        &[
            "keeper=running",
            "label=L",
            "job=absent",
            "socket=/p/k.sock",
        ],
        true,
        false,
        None,
    );
    check(
        say(
            JobState::Absent,
            Ok("keeper=running\npid=42 version=0.70.0\nsocket=/p/k.sock".to_string()),
            None,
        ),
        &[
            "keeper=running",
            "label=L",
            "job=absent",
            "pid=42",
            "socket=/p/k.sock",
        ],
        true,
        false,
        None,
    );
}

#[cfg(unix)]
#[test]
fn status_tells_a_refused_keeper_from_an_absent_one() {
    const REMEDY: &str = "aterm keeper stop, then aterm keeper start";
    let sock = std::path::Path::new("/p/k.sock");
    let repl = status_miss_line(sock, &NoKeeper::NotOurs, true);
    assert_eq!(keeper_fields(&repl), ["keeper=refused", "socket=/p/k.sock"]);
    assert!(repl.contains(REMEDY), "{repl}");
    let norepl = status_miss_line(sock, &NoKeeper::NotOurs, false);
    assert_eq!(
        keeper_fields(&norepl),
        ["keeper=refused", "socket=/p/k.sock"]
    );
    assert!(!norepl.contains(REMEDY), "{norepl}");
    assert_eq!(
        status_miss_line(sock, &NoKeeper::Nothing, true),
        "keeper=absent socket=/p/k.sock"
    );
    let failed = status_miss_line(sock, &NoKeeper::Failed("timed out".into()), false);
    assert_eq!(
        keeper_fields(&failed),
        ["keeper=absent", "socket=/p/k.sock"]
    );
    assert!(failed.contains("(timed out)"), "{failed}");
}

#[test]
fn start_and_stop_touch_only_this_builds_label_or_a_test_one() {
    let own = "com.aterm.aterm.keeper.dev.0123abcd";
    assert!(label_is_ours(own, own));
    assert!(label_is_ours("com.aterm.aterm.keeper.test.42.e2e", own));
    assert!(
        !label_is_ours(crate::job::INSTALLED_LABEL, own),
        "a dev build never touches the installed app's keeper"
    );
    assert!(!label_is_ours("com.aterm.aterm.keeper.test", own));
    assert!(!label_is_ours("com.aterm.aterm.keeper.testx", own));
    assert!(!label_is_ours("com.apple.Finder", own));
}

#[test]
fn the_options_parse() {
    let args: Vec<OsString> = [
        "start",
        "--label",
        "com.aterm.aterm.keeper.test.1.x",
        "--identity",
        "uid",
        "--sock",
        "/p/k.sock",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    let a = parse(&args).expect("parse");
    assert_eq!(a.verb, "start");
    assert_eq!(a.label.as_deref(), Some("com.aterm.aterm.keeper.test.1.x"));
    assert_eq!(a.identity, IdentityPolicy::SameUid);
    assert_eq!(a.sock, Some(PathBuf::from("/p/k.sock")));
    let serve: Vec<OsString> = ["serve", "--no-relaunch"]
        .iter()
        .map(OsString::from)
        .collect();
    let parsed_serve = parse(&serve).expect("parse");
    assert!(parsed_serve.no_relaunch && !parsed_serve.help);

    let help_after_verb: Vec<OsString> = ["status", "--help"].iter().map(OsString::from).collect();
    assert!(parse(&help_after_verb).expect("status --help").help);

    let sock_help_value: Vec<OsString> = ["start", "--sock", "--help"]
        .iter()
        .map(OsString::from)
        .collect();
    let parsed_sock = parse(&sock_help_value).expect("--sock consumes its value");
    assert!(!parsed_sock.help);
    assert_eq!(parsed_sock.sock, Some(PathBuf::from("--help")));

    let empty_sock: Vec<OsString> = ["start", "--sock", ""].iter().map(OsString::from).collect();
    assert!(parse(&empty_sock).is_err());

    let unknown_verb_help: Vec<OsString> = ["bogus", "--help"].iter().map(OsString::from).collect();
    assert!(parse(&unknown_verb_help).is_err());
}

#[cfg(unix)]
#[test]
fn fresh_log_replaces_only_our_regular_files_and_resets_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = crate::test_scratch_dir("fresh-log");
    std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700)).expect("0700");

    let log = scratch.path().join("k.sock.log");
    std::fs::write(&log, b"stale").expect("seed log");
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).expect("0644");
    fresh_log(&log).expect("replace regular log");
    assert!(std::fs::read(&log).expect("read fresh").is_empty());
    let mode = std::fs::symlink_metadata(&log)
        .expect("stat fresh")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    let keep = scratch.path().join("keep.txt");
    std::fs::write(&keep, b"keep").expect("seed target");
    let link = scratch.path().join("link.log");
    std::os::unix::fs::symlink(&keep, &link).expect("symlink");
    let err = fresh_log(&link).expect_err("symlink log must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    assert_eq!(std::fs::read(&keep).expect("target untouched"), b"keep");
    assert!(std::fs::symlink_metadata(&link).is_ok());

    let subdir = scratch.path().join("dir.log");
    std::fs::create_dir(&subdir).expect("subdir");
    let err = fresh_log(&subdir).expect_err("directory log must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
}
