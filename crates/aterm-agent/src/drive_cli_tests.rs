// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn os(v: &[&str]) -> Vec<std::ffi::OsString> {
    v.iter().map(std::ffi::OsString::from).collect()
}

/// [`classify_verb_at`] under this test's `HOME`, asked from a folder no secret rule names: a
/// checkout may sit under one (`~/Library/Caches`), where a relative path is a read of a secret.
fn classified(args: &[String]) -> Result<Reply, String> {
    classify_verb_at(
        args,
        crate::supervise::policy::approval::advisory_home().as_deref(),
        Some(std::path::Path::new("/var/empty")),
    )
}

#[test]
fn classify_parses_the_command_and_exits_on_the_verdict() {
    let r = classified(&args(&["git status --short && git pull | tail"])).expect("ok");
    assert_eq!(
        r,
        Reply {
            text: "not-read-only git pull\n".to_string(),
            code: 1
        }
    );
    let r = classified(&args(&["git", "log", "-3"])).expect("joined words");
    assert_eq!(
        r,
        Reply {
            text: "read-only\n".to_string(),
            code: 0
        }
    );
    let r = classified(&args(&["git", "log", "--oneline"])).expect("a flag in the command");
    assert_eq!(r.code, 0, "{r:?}");
    let sub = parse_sub("classify", &args(&["git", "log", "--oneline", "-5"])).expect("parses");
    assert_eq!(sub.rest, args(&["git", "log", "--oneline", "-5"]));
    let sub = parse_sub("classify", &args(&["--", "--weird", "x"])).expect("-- ends the flags");
    assert_eq!(sub.rest, args(&["--weird", "x"]));
    let sub = parse_sub("classify", &args(&["@foo", "bar"])).expect("no sid for classify");
    assert_eq!((sub.sid, sub.rest), (None, args(&["@foo", "bar"])));
    let sub = parse_sub("answer", &args(&["@s-1", "--", "--weird"])).expect("parses");
    assert_eq!(
        (sub.sid.as_deref(), sub.rest),
        (Some("@s-1"), args(&["--weird"]))
    );
    let r = classified(&args(&["python3 tools/audit.py"])).expect("ok");
    assert_eq!(r.code, 1);
    let r = classified(&args(&[
        "--allow-python",
        "tools/*.py",
        "python3 tools/audit.py",
    ]))
    .expect("ok");
    assert_eq!(r.code, 0, "{r:?}");
    assert!(classified(&args(&[])).is_err(), "no command");
    let o = parse(vec!["classify".into(), "ls -la".into()]).expect("parses");
    assert_eq!(o.cmd, args(&["classify", "ls -la"]));
    assert_eq!(run(&o).expect("pure verb needs no host").code, 0);
}

#[test]
fn classify_refuses_a_read_of_a_secret_under_the_default_list() {
    let home = Some(std::path::Path::new("/Users/_owner"));
    for line in [
        "cat ~/.ssh/id_rsa",
        "head -3 .env",
        "awk 'NR<3' ~/.aws/credentials",
        "cat /Users/_owner/.claude.json",
        "grep -m 1 key ~/.config/gh/hosts.yml",
        "git show HEAD:.env",
    ] {
        let r = classify_verb_at(&args(&[line]), home, None).expect("ok");
        assert_eq!(r.code, 1, "{line}: {r:?}");
        assert!(
            r.text.starts_with("not-read-only a read of a secret"),
            "{line}: {r:?}"
        );
    }
    // Asked from no directory, the verb cannot place the tree a reader descends.
    let r = classify_verb_at(&args(&["grep -rn credentials src/"]), home, None).expect("ok");
    assert_eq!(r.code, 1, "{r:?}");
    assert!(r.text.contains("cannot list"), "{r:?}");
    // NEGATIVE CONTROLS
    for line in [
        "grep -m 1 credentials src/",
        "wc -l src/credentials.rs",
        "ls -la ~/.ssh",
        "cat README.md",
    ] {
        let r = classify_verb_at(&args(&[line]), home, None).expect("ok");
        assert_eq!(r, Reply::text("read-only\n".to_string()), "{line}");
    }
    let unknown_home =
        classify_verb_at(&args(&["cat /Users/_owner/.claude.json"]), None, None).expect("ok");
    assert_eq!(unknown_home.code, 0, "{unknown_home:?}");
    // The classifier's own refusal still comes first.
    let r = classify_verb_at(&args(&["cat ~/.ssh/id_rsa > out"]), home, None).expect("ok");
    assert!(!r.text.contains("a read of a secret"), "{r:?}");
    assert_eq!(r.code, 1);
}

/// `classify` reads a relative path from the directory it is asked from, where the asking
/// agent's line runs, and from a directory it cannot place when it has none.
#[test]
fn classify_reads_a_relative_path_where_it_is_asked() {
    let home = Some(std::path::Path::new("/Users/_owner"));
    let proj = Some(std::path::Path::new("/Users/_owner/proj"));
    for (line, cwd) in [
        ("cat ../.claude.json", proj),
        ("cd .. && cat .config/gh/hosts.yml", proj),
        ("cat .claude.json", None),
    ] {
        let r = classify_verb_at(&args(&[line]), home, cwd).expect("ok");
        assert_eq!(r.code, 1, "{line}: {r:?}");
        assert!(
            r.text.starts_with("not-read-only a read of a secret"),
            "{line}: {r:?}"
        );
    }
    // NEGATIVE CONTROLS
    for (line, cwd) in [("cat .claude.json", proj), ("cat ../README.md", None)] {
        let r = classify_verb_at(&args(&[line]), home, cwd).expect("ok");
        assert_eq!(r, Reply::text("read-only\n".to_string()), "{line}");
    }
}

/// `classify` resolves the secret list's home roots under the home the advisory word reads
/// (`approval::advisory_home`: `HOME`, else Windows' `USERPROFILE`), and holds a line to the
/// proof gaps the advisory word holds it to, so the two agree on every line.
#[test]
fn classify_reads_the_shell_home_and_the_proof_gaps() {
    let home =
        crate::supervise::policy::approval::advisory_home().expect("the test runs with a home");
    for line in [
        "cat ~/.config/gh/hosts.yml".to_string(),
        // Quoted: a Windows home's `\` is the shell's escape outside quotes.
        format!("cat '{}/Library/Preferences/x.plist'", home.display()),
        "cd ~ && cat .claude.json".to_string(),
        "cat ~/.config/g?/hosts.yml".to_string(),
        "grep -r token ~".to_string(),
    ] {
        let r = classified(&args(&[&line])).expect("ok");
        assert_eq!(r.code, 1, "{line}: {r:?}");
        assert!(
            r.text.starts_with("not-read-only a read of a secret"),
            "{line}: {r:?}"
        );
    }
    for (line, why) in [
        ("FOO=1 cat README.md", "assigns a variable"),
        ("env cat README.md", "runs env"),
        (
            "for f in a; do cat README.md; done",
            "assigns a for loop's variable",
        ),
        ("sort src/*.rs", "an unquoted glob `*`"),
        ("cat a\\  b", "not read as drawn"),
        ("cat $(<files.txt)", "builds at run time"),
        ("echo $GITHUB_TOKEN", "may be a secret"),
    ] {
        let r = classified(&args(&[line])).expect("ok");
        assert_eq!(r.code, 1, "{line}: {r:?}");
        assert!(r.text.contains(why), "{line}: {r:?}");
    }
    // A relative path is read from where the line runs.
    let library = home.join("Library").join("Caches");
    let r = classify_verb_at(&args(&["cat README.md"]), Some(&home), Some(&library)).expect("ok");
    assert_eq!(r.code, 1, "{r:?}");
    assert!(
        r.text.starts_with("not-read-only a read of a secret"),
        "{r:?}"
    );
    // NEGATIVE CONTROLS
    for line in [
        "cat ~/.config/notes.txt",
        "ls src/*.rs | sort",
        "cat README.md",
    ] {
        let r = classified(&args(&[line])).expect("ok");
        assert_eq!(r, Reply::text("read-only\n".to_string()), "{line}");
    }
}

#[test]
fn phase_ends_with_the_context_left() {
    let rule = "─".repeat(120);
    let indicator = format!("{}1% until auto-compact", " ".repeat(97));
    let body = [
        "⏺ Done.",
        "",
        "✻ Cogitated for 4s · done 2:41 PM",
        "",
        "● How is Claude doing this session? (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
        &indicator,
        &rule,
        "❯",
        &rule,
        "  ? for shortcuts",
    ];
    let turn = |rows: Vec<String>| supervise::Turn {
        phase: supervise::worker_phase(&rows),
        screen: supervise::screen::Screen {
            rows,
            ..supervise::screen::Screen::default()
        },
        timed_out: false,
    };
    let claude = aterm_phase::Program::Claude.reader();
    let rows: Vec<String> = body.iter().map(|r| r.to_string()).collect();
    let r = phase_reply(&turn(rows.clone()), claude, &[]);
    assert_eq!(
        (r.text.as_str(), r.code),
        ("idle\nsurvey 0\ncontext 1%\n", 0)
    );
    let no_survey: Vec<String> = rows
        .iter()
        .filter(|r| !r.contains("How is Claude doing") && !r.contains("0: Dismiss"))
        .cloned()
        .collect();
    assert_eq!(
        phase_reply(&turn(no_survey.clone()), claude, &[]).text,
        "idle\ncontext 1%\n"
    );
    let plain: Vec<String> = no_survey
        .into_iter()
        .filter(|r| !r.contains("until auto-compact"))
        .collect();
    assert_eq!(phase_reply(&turn(plain), claude, &[]).text, "idle\n");
}

#[test]
fn a_verb_followed_by_help_asks_for_help_and_never_acts() {
    for verb in ["classify", "phase", "answer", "ledger", "tape", "replay"] {
        for flag in ["--help", "-h"] {
            let o = parse(os(&[verb, flag])).expect("parses");
            assert_eq!(o.cmd, args(&["help"]), "`{verb} {flag}` must ask for help");
            let o = parse(os(&[verb, flag, "more"])).expect("parses");
            assert_eq!(o.cmd, args(&["help"]), "`{verb} {flag} more`");
        }
    }
    let o = parse(os(&["--socket", "/s.sock", "answer", "--help"])).expect("parses");
    assert_eq!(
        (o.socket.as_deref(), o.cmd),
        (Some("/s.sock"), args(&["help"]))
    );
    let o = parse(os(&["answer", "@s-1", "--help"])).expect("parses");
    assert_eq!(o.cmd, args(&["answer", "@s-1", "--help"]));
    let o = parse(os(&["phase", "--help"])).expect("parses");
    let text = match run(&o) {
        Ok(r) => r.text,
        Err(e) => panic!("help must not fail: {e}"),
    };
    assert!(text.contains("aterm drive"), "{text}");
}

#[test]
fn the_one_option_is_the_socket() {
    let o = parse(os(&["--sock", "/run/a.sock", "phase", "@s-1"])).expect("parses");
    assert_eq!(o.socket.as_deref(), Some("/run/a.sock"));
    assert_eq!(o.cmd, args(&["phase", "@s-1"]));
    for gone in ["--dial", "--remote", "--timeout", "--idle", "--ready"] {
        let err = parse(os(&[gone, "x", "phase"])).expect_err(gone);
        assert!(
            err.starts_with(&format!("unknown option '{gone}'")),
            "{err}"
        );
    }
    let err = parse(os(&["--socket"])).expect_err("no path");
    assert_eq!(err, "--socket needs a PATH");
}

#[test]
fn a_removed_verb_is_refused_before_any_host() {
    for verb in [
        "prompt",
        "read",
        "shot",
        "await",
        "await-turn",
        "supervise",
        "watch",
        "task",
        "report",
    ] {
        let o = Opts {
            socket: Some("/nonexistent/aterm.sock".to_string()),
            cmd: args(&[verb, "@s-1", "x"]),
        };
        let err = run(&o).expect_err(verb);
        assert!(
            err.starts_with(&format!("`{verb}` is not an `aterm drive` command; use ")),
            "{err}"
        );
        assert!(gone_to(verb).is_some());
    }
    for word in [
        "classify", "phase", "answer", "ledger", "tape", "replay", "bogus",
    ] {
        assert_eq!(gone_to(word), None, "{word}");
    }
    let err = run(&Opts {
        socket: Some("/nonexistent/aterm.sock".to_string()),
        cmd: args(&["bogus"]),
    })
    .expect_err("not a verb");
    assert!(err.starts_with("unknown command 'bogus'"), "{err}");
}

#[test]
fn the_ledgers_flags_belong_to_it() {
    let sub = parse_sub(
        "ledger",
        &args(&[
            "@s-1",
            "--journal",
            "j.jsonl",
            "--format",
            "md",
            "--out",
            "out.md",
            "--since",
            "1789344000000",
        ]),
    )
    .expect("parses");
    assert!(no_positionals("ledger", &sub).is_ok());
    assert_eq!(
        (
            sub.sid.as_deref(),
            sub.journal.as_deref(),
            sub.format,
            sub.out.as_deref(),
            sub.since_ms
        ),
        (
            Some("@s-1"),
            Some(std::path::Path::new("j.jsonl")),
            Some(LedgerFormat::Md),
            Some(std::path::Path::new("out.md")),
            Some(1_789_344_000_000)
        )
    );
    for (word, want) in [
        ("text", LedgerFormat::Text),
        ("md", LedgerFormat::Md),
        ("markdown", LedgerFormat::Md),
        ("html", LedgerFormat::Html),
    ] {
        let sub = parse_sub("ledger", &args(&["--format", word])).expect(word);
        assert_eq!(sub.format, Some(want), "{word}");
    }
    for bad in ["", "HTML", "pdf", "json"] {
        let err = parse_sub("ledger", &args(&["--format", bad])).expect_err(bad);
        assert_eq!(err, "ledger: --format needs text, md or html", "{bad}");
    }
    let err = parse_sub("ledger", &args(&["--format"])).expect_err("no value");
    assert!(err.contains("--format needs"), "{err}");
    let sub = parse_sub("ledger", &args(&["--since", "2026-09-14T10:30:00Z"])).expect("a time");
    assert_eq!(sub.since_ms, Some(1_789_381_800_000));
    for bad in ["7730:1291", "yesterday", "2026-13-01"] {
        let err = parse_sub("ledger", &args(&["--since", bad])).expect_err(bad);
        assert!(
            err.starts_with("ledger: --since needs a time:"),
            "{bad}: {err}"
        );
    }
    for words in [&["--source", "log"][..], &["--source=log"][..]] {
        let sub = parse_sub("ledger", &args(words)).expect("the one source");
        assert_eq!(sub, parse_sub("ledger", &[]).expect("none"), "{words:?}");
    }
    for bad in [
        &["--source", "clock"][..],
        &["--source="][..],
        &["--source"][..],
    ] {
        let err = parse_sub("ledger", &args(bad)).expect_err("no source");
        assert!(err.starts_with("ledger: --source needs"), "{bad:?}: {err}");
    }
    let sub = parse_sub("answer", &args(&["@s-1", "--box", "q7", "2"])).expect("parses");
    assert_eq!(
        (sub.box_token.as_deref(), sub.rest),
        (Some("q7"), args(&["2"]))
    );
    for (verb, flag, says) in [
        ("phase", "--journal", "--journal is ledger's"),
        ("answer", "--since", "--since is ledger's"),
        ("phase", "--format", "--format is ledger's"),
        ("answer", "--out", "--out is ledger's"),
        ("phase", "--source", "--source is ledger's"),
        ("phase", "--box", "--box is answer's"),
        ("ledger", "--box", "--box is answer's"),
    ] {
        let err = parse_sub(verb, &args(&[flag, "x"])).expect_err(flag);
        assert!(
            err.starts_with(&format!("{verb}: {says}")),
            "{verb} {flag}: {err}"
        );
    }
    for gone in ["--final", "--messages", "--mail", "--max-rows", "--report"] {
        let err = parse_sub("ledger", &args(&[gone])).expect_err(gone);
        assert!(err.contains(&format!("unknown option '{gone}'")), "{err}");
    }
}

/// THE REMOVED SOURCE IS A USAGE ERROR: `ledger --source anchor`, either spelling, is
/// refused before any host with the day it was removed and what to do instead;
/// `--source log` reads as no flag; NEGATIVE CONTROL: the anchor parsed as a source again
/// is caught here.
#[test]
fn a_removed_source_is_a_usage_error_naming_its_date() {
    for words in [
        &["@s-1", "--source", "anchor"][..],
        &["@s-1", "--source=anchor"][..],
    ] {
        let err = parse_sub("ledger", &args(words)).expect_err("removed");
        assert_eq!(
            err,
            format!(
                "ledger: --source anchor was removed on {ANCHOR_REMOVED}: the session log \
                 places every time; leave the flag out"
            ),
            "{words:?}"
        );
    }
    let err = ledger_verb(
        &Opts {
            socket: Some("/nonexistent/aterm.sock".to_string()),
            cmd: args(&["ledger"]),
        },
        &args(&["@s-1", "--source", "anchor"]),
    )
    .expect_err("refused before any host");
    assert!(err.contains("was removed on"), "{err}");
    assert!(
        DRIVE_HELP.contains(&format!("was removed on {ANCHOR_REMOVED}.")),
        "the help names the same day"
    );
    let sub = parse_sub("ledger", &args(&["@s-1", "--source", "log"])).expect("parses");
    assert_eq!(
        ledger_opts(&sub, "@s-1".to_string()),
        ledger_opts(
            &parse_sub("ledger", &args(&["@s-1"])).expect("parses"),
            "@s-1".to_string()
        )
    );
}

#[test]
fn the_ledger_takes_its_worker_from_the_journal_when_none_is_named() {
    let dir = crate::supervise::ScratchDir::new("drive-ledger", "journal");
    let path = dir.join("journal.jsonl");
    let line = |sid: &str| {
        crate::supervise::journal::JournalRecord::of_line(1, Some(sid), "TIMEOUT", None).to_json()
    };
    std::fs::write(&path, format!("{}\n{}\n", line("s-work"), line("@s-work"))).expect("write");
    assert_eq!(journal_sid(Some(&path)).as_deref(), Some("s-work"));
    std::fs::write(&path, format!("{}\n{}\n", line("s-work"), line("s-other"))).expect("write");
    assert_eq!(journal_sid(Some(&path)), None);
    assert_eq!(journal_sid(None), None);
    assert_eq!(journal_sid(Some(&dir.join("nothing.jsonl"))), None);
    let err = ledger_verb(
        &Opts {
            socket: None,
            cmd: vec!["ledger".to_string()],
        },
        &args(&["--journal", &path.to_string_lossy()]),
    )
    .expect_err("no worker");
    assert!(err.starts_with("ledger needs the worker:"), "{err}");
}

/// `tape ls` lists the hosted store newest first, a session's alone with
/// `@sid` (the kept ones beside it), and says an empty store; `tape
/// forget` removes the tapes named — a FILE, or a session's every tape
/// but its kept ones — and refuses anything that is no tape in the store
/// before a tape goes.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
#[test]
fn tape_ls_lists_the_store_and_forget_trims_it() {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::time::{Duration, SystemTime};
    let guard = crate::supervise::ScratchDir::new("drive-tape", "store");
    let state = guard.canonicalize().expect("owned canonical fixture");
    let store = store::store_under(&state);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(state.join("drive"))
        .expect("private drive");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&store)
        .expect("private store");
    let now = SystemTime::now();
    assert!(
        store_ls(&store, &[], now)
            .expect("lists")
            .starts_with("no hosted tapes in "),
        "an empty store says so"
    );
    let put = |dir: &str, name: &str, ago: u64| {
        let path = store.join(dir).join(name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path.parent().expect("dir"))
            .expect("private session directory");
        use std::io::Write as _;
        store::create(&path)
            .expect("exclusive tape")
            .write_all(b"tape")
            .expect("write");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(now - Duration::from_secs(ago))
            .expect("mtime");
        path
    };
    let a = put("s-1", "a.atape", 30);
    let b = put("s-2", "b.atape", 7200);
    let k = put(store::KEPT, "k.atape", 4 * 86_400);
    let all = store_ls(&store, &[], now).expect("lists");
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines.len(), 3, "{all}");
    assert!(
        lines[0].contains("30s ago") && lines[0].ends_with("a.atape"),
        "{all}"
    );
    assert!(
        lines[1].contains("2h ago") && lines[1].ends_with("b.atape"),
        "{all}"
    );
    assert!(
        lines[2].contains("4d ago") && lines[2].ends_with("k.atape  kept"),
        "{all}"
    );
    let one = store_ls(&store, &args(&["@s-2"]), now).expect("lists");
    assert_eq!(one.lines().count(), 2, "s-2's and the kept one: {one}");
    assert!(
        store_ls(&store, &args(&["s-2"]), now).is_err(),
        "a session is @sid"
    );

    let outside = state.join("elsewhere.atape");
    std::fs::write(&outside, b"tape").expect("write");
    let err = store_forget(
        &store,
        &args(&[a.to_str().expect("utf-8"), "/nonexistent.atape"]),
    )
    .expect_err("refused");
    assert!(err.contains("no tape"), "{err}");
    assert!(a.exists(), "nothing went before the refusal");
    assert!(store_forget(&store, &args(&[outside.to_str().expect("utf-8")])).is_err());
    assert!(store_forget(&store, &[]).is_err());
    let err =
        store_forget(&store, &args(&["@s-12", a.to_str().expect("utf-8")])).expect_err("refused");
    assert!(err.contains("@s-12 has no tape"), "{err}");
    assert!(a.exists(), "nothing went before the refusal");
    let gone =
        store_forget(&store, &args(&["@s-1", b.to_str().expect("utf-8")])).expect("forgotten");
    assert_eq!(gone.lines().count(), 2, "{gone}");
    assert!(!a.exists() && !b.exists());
    assert!(k.exists(), "a kept tape goes only by name");
    assert!(outside.exists());
}

#[test]
fn tape_reads_only_its_file_and_refuses_with_two() {
    use crate::supervise::tape::codec::{End, Item};
    use crate::supervise::tape::frame::TapeWriter;
    let dir = crate::supervise::ScratchDir::new("drive-tape", "verb");
    let path = dir.join("run.atape");
    {
        let file = store::create(&path).expect("made");
        let mut w = TapeWriter::start(
            Box::new(file),
            "run.atape",
            "atape 1\ntape=t\n",
            Box::new(std::io::sink()),
        );
        w.append(&Item::End(End::Returned {
            how: "exit 0".to_string(),
        }));
    }
    let file = path.to_string_lossy().into_owned();
    for (line, says) in [
        (vec!["cat", file.as_str()], "end: returned exit 0"),
        (
            vec!["cat", file.as_str(), "--text"],
            "atape-text 1\nheader\n",
        ),
        (vec!["show", file.as_str(), "--decisions"], "no decisions"),
        (vec!["stats", file.as_str()], "end returned exit 0"),
    ] {
        let reply = tape_verb(&args(&line));
        assert_eq!(reply.code, 0, "{line:?}");
        assert!(reply.text.contains(says), "{line:?}: {}", reply.text);
    }
    for line in [
        vec![],
        vec!["play", file.as_str()],
        vec!["cat"],
        vec!["show", file.as_str()],
        vec!["cat", file.as_str(), "--decisions"],
        vec!["cat", file.as_str(), file.as_str()],
        vec!["stats", "/nonexistent/run.atape"],
    ] {
        assert_eq!(tape_verb(&args(&line)).code, 2, "{line:?}");
    }
    std::fs::write(dir.join("not.atape"), b"hello").expect("write");
    let not = dir.join("not.atape").to_string_lossy().into_owned();
    assert_eq!(tape_verb(&args(&["cat", &not])).code, 2, "no tape");
}

#[test]
fn replay_reads_only_its_file_and_refuses_with_two() {
    use crate::supervise::tape::codec::{End, Item};
    use crate::supervise::tape::frame::TapeWriter;
    let dir = crate::supervise::ScratchDir::new("drive-replay", "verb");
    let path = dir.join("run.atape");
    {
        let file = store::create(&path).expect("made");
        let mut w = TapeWriter::start(
            Box::new(file),
            "run.atape",
            "not a header\n",
            Box::new(std::io::sink()),
        );
        w.append(&Item::End(End::Returned {
            how: "exit 0".to_string(),
        }));
    }
    let file = path.to_string_lossy().into_owned();
    for line in [
        vec![],
        vec!["--until", "door=3"],
        vec![file.as_str(), "--until"],
        vec![file.as_str(), "--until", "3"],
        vec![file.as_str(), "--until", "door=x"],
        vec![file.as_str(), "--fast"],
        vec![file.as_str(), file.as_str()],
        vec!["/nonexistent/run.atape"],
        vec![file.as_str()],
    ] {
        assert_eq!(replay_verb(&args(&line)).code, 2, "{line:?}");
    }
    for (until, door) in [
        ("door=7", 7),
        ("door=#7", 7),
        ("door=#0", 0),
        ("door=12", 12),
    ] {
        let ReplayArgs {
            file: named,
            options,
            why,
            ..
        } = replay_args(&args(&["--until", until, file.as_str()])).expect("parses");
        assert_eq!(named, path, "{until}");
        assert!(why.is_none(), "{until}");
        assert_eq!(options.until, Some(door), "{until}");
        assert_eq!(options.base, None, "{until}");
        let err = replay_reply(&args(&[file.as_str(), "--until", until]))
            .expect_err("no loop to rebuild");
        assert!(err.contains("not a tape header"), "{until}: {err}");
    }
    let ReplayArgs { options, why, .. } = replay_args(&args(&[file.as_str()])).expect("parses");
    assert_eq!(options.until, None, "no --until, no stop");
    assert!(why.is_none(), "no --why, a replay");
    for bad in ["door=#", "door=-1", "door=#x", "#7", "door= 7"] {
        assert!(
            replay_args(&args(&[file.as_str(), "--until", bad])).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn why_is_an_option_of_replay_and_refuses_with_two() {
    use crate::supervise::tape::codec::{End, Item};
    use crate::supervise::tape::frame::TapeWriter;
    let dir = crate::supervise::ScratchDir::new("drive-replay", "why");
    let path = dir.join("run.atape");
    {
        let file = store::create(&path).expect("made");
        let mut w = TapeWriter::start(
            Box::new(file),
            "run.atape",
            "not a header\n",
            Box::new(std::io::sink()),
        );
        w.append(&Item::End(End::Returned {
            how: "exit 0".to_string(),
        }));
    }
    let file = path.to_string_lossy().into_owned();
    for line in [
        vec![file.as_str(), "--why"],
        vec![file.as_str(), "--at", "seq=1"],
        vec![file.as_str(), "--format", "md"],
        vec![file.as_str(), "--why", "--at"],
        vec![file.as_str(), "--why", "--at", "seq=x"],
        vec![file.as_str(), "--why", "--at", "seq=1", "--format"],
        vec![file.as_str(), "--why", "--at", "seq=1", "--format", "pdf"],
        vec![file.as_str(), "--why", "--at", "seq=1", "--until", "door=3"],
        vec!["--why", "--at", "seq=1"],
        vec!["/nonexistent/run.atape", "--why", "--at", "seq=1"],
        vec![file.as_str(), "--why", "--at", "seq=1"],
    ] {
        let reply = replay_verb(&args(&line));
        assert_eq!(reply.code, 2, "{line:?}");
        assert!(reply.text.is_empty(), "{line:?}");
    }
    let ReplayArgs { why, options, .. } = replay_args(&args(&[
        "--why",
        "--at",
        "door=#17",
        file.as_str(),
        "--format",
        "html",
    ]))
    .expect("parses");
    assert_eq!(
        why,
        Some((explain::At::Door(17), explain::Format::Html)),
        "the point and the rendering"
    );
    assert_eq!(options.until, None);
    let ReplayArgs { why, .. } =
        replay_args(&args(&[file.as_str(), "--why", "--at", "seq=3"])).expect("parses");
    assert_eq!(why, Some((explain::At::Seq(3), explain::Format::Text)));
    let err =
        replay_reply(&args(&[file.as_str(), "--why", "--at", "door=3"])).expect_err("no decision");
    assert!(err.contains("no decisions"), "{err}");
    let err = run(&Opts {
        socket: None,
        cmd: args(&["why", file.as_str(), "--at", "seq=1"]),
    })
    .expect_err("no `why` verb");
    assert!(err.contains("unknown command 'why'"), "{err}");
}

#[test]
fn counterfactuals_are_options_of_replay_and_refuse_with_two() {
    use crate::supervise::tape::codec::{End, Item};
    use crate::supervise::tape::frame::TapeWriter;
    let dir = crate::supervise::ScratchDir::new("drive-replay", "counterfactual");
    let path = dir.join("run.atape");
    {
        let file = store::create(&path).expect("made");
        let mut w = TapeWriter::start(
            Box::new(file),
            "run.atape",
            "not a header\n",
            Box::new(std::io::sink()),
        );
        w.append(&Item::End(End::Returned {
            how: "exit 0".to_string(),
        }));
    }
    let file = path.to_string_lossy().into_owned();
    let policy = dir.join("aterm.toml");
    std::fs::write(&policy, "[harness]\napprove = \"none\"\n").expect("written");
    let policy = policy.to_string_lossy().into_owned();
    let typo = dir.join("typo.toml");
    std::fs::write(&typo, "[harness]\napprove = \"saf\"\n").expect("written");
    let typo = typo.to_string_lossy().into_owned();
    let absent = dir.join("absent.toml");
    let absent = absent.to_string_lossy().into_owned();
    let len =
        u64::try_from(b"[harness]\napprove = \"none\"\n".len()).expect("small policy fixture");
    let (_, exact) = candidate_file_with_limit(&policy, len).expect("exact policy limit");
    assert_eq!(exact.approve, supervise::config::Approve::None);
    let over = candidate_file_with_limit(&policy, len - 1).expect_err("one byte over");
    assert!(over.contains("exceeds"), "{over}");
    let directory = dir.to_string_lossy().into_owned();
    let kind = candidate_file(&directory).expect_err("a directory is not policy input");
    assert!(kind.contains("not a readable regular file"), "{kind}");
    let non_utf8 = dir.join("non-utf8.toml");
    std::fs::write(&non_utf8, b"[harness]\n# \xff\n").expect("owned non-UTF8 policy");
    assert!(
        candidate_file(&non_utf8.to_string_lossy())
            .expect_err("the policy bytes must be UTF-8")
            .contains("not UTF-8")
    );
    let controls = dir.join("controls.toml");
    std::fs::write(
        &controls,
        "[harness]\n\"\\u001b]52;c;YQ==\\u0007\" = true\n",
    )
    .expect("owned policy with a control-bearing unknown key");
    let refusal = candidate_file(&controls.to_string_lossy()).expect_err("unknown policy key");
    assert!(!refusal.chars().any(char::is_control), "{refusal:?}");
    for line in [
        vec![file.as_str(), "--set"],
        vec![file.as_str(), "--set", "approve"],
        vec![file.as_str(), "--set", "=safe"],
        vec![file.as_str(), "--policy"],
        vec![file.as_str(), "--policy", absent.as_str()],
        vec![
            file.as_str(),
            "--policy",
            policy.as_str(),
            "--policy",
            policy.as_str(),
        ],
        vec![file.as_str(), file.as_str(), "--set", "approve=safe"],
        vec![file.as_str(), "--shadow", "--until", "door=3"],
        vec![
            file.as_str(),
            "--why",
            "--at",
            "seq=1",
            "--set",
            "approve=safe",
        ],
        vec![file.as_str(), "--why", "--at", "seq=1", "--shadow"],
        vec![file.as_str(), "--set", "approve=safe"],
        vec![file.as_str(), "--shadow"],
    ] {
        let reply = replay_verb(&args(&line));
        assert_eq!(reply.code, 2, "{line:?}");
        assert!(reply.text.is_empty(), "{line:?}");
    }
    let reply = replay_verb(&args(&[file.as_str(), file.as_str(), "--shadow"]));
    assert_eq!(reply.code, 2, "{}", reply.text);
    assert_eq!(
        reply.text.matches("not shadowed: ").count(),
        2,
        "{}",
        reply.text
    );
    let ReplayArgs {
        options, shadow, ..
    } = replay_args(&args(&[
        file.as_str(),
        "--set",
        "harness.approve=safe",
        "--set",
        "continue=false",
        "--until",
        "door=9",
    ]))
    .expect("parses");
    assert!(!shadow);
    assert_eq!(options.until, Some(9));
    let candidate = options.candidate.expect("a counterfactual");
    assert_eq!(
        candidate.sets,
        [
            ("harness.approve".to_string(), "safe".to_string()),
            ("continue".to_string(), "false".to_string())
        ]
    );
    assert!(candidate.file.is_none());
    let ReplayArgs {
        more,
        options,
        shadow,
        ..
    } = replay_args(&args(&[
        "--shadow",
        file.as_str(),
        file.as_str(),
        "--policy",
        policy.as_str(),
    ]))
    .expect("parses");
    assert!(shadow);
    assert_eq!(more.len(), 1, "two tapes shadowed");
    let (named, read) = options
        .candidate
        .expect("a candidate")
        .file
        .expect("the policy file");
    assert_eq!(named, policy);
    assert_eq!(read.approve, supervise::config::Approve::None);
    for line in [
        vec![file.as_str(), "--policy", typo.as_str()],
        vec![file.as_str(), "--shadow", "--policy", typo.as_str()],
    ] {
        let err = replay_args(&args(&line)).expect_err("a policy the reader refuses");
        assert!(
            err.contains("harness.approve") && err.contains("saf"),
            "{err}"
        );
    }
    let ReplayArgs { options, .. } = replay_args(&args(&[file.as_str()])).expect("parses");
    assert!(options.candidate.is_none(), "no candidate, a strict replay");
    let err = run(&Opts {
        socket: None,
        cmd: args(&["shadow", file.as_str()]),
    })
    .expect_err("no `shadow` verb");
    assert!(err.contains("unknown command 'shadow'"), "{err}");
}

#[test]
fn a_corpus_check_is_an_option_of_replay() {
    let dir = crate::supervise::ScratchDir::new("drive-replay", "check");
    std::fs::create_dir_all(dir.join("empty")).expect("made");
    std::fs::create_dir_all(dir.join("corpus").join("deep")).expect("made");
    let bad = dir.join("corpus").join("deep").join("bad.atape.txt");
    std::fs::write(&bad, "atape-text 1\nnot a record\n").expect("written");
    let corpus = dir.join("corpus").to_string_lossy().into_owned();
    let empty = dir.join("empty").to_string_lossy().into_owned();
    for line in [
        vec!["--check"],
        vec!["--check", corpus.as_str(), "--shadow"],
        vec!["--check", corpus.as_str(), "--until", "door=3"],
        vec!["--check", corpus.as_str(), "--why", "--at", "seq=1"],
        vec!["--check", empty.as_str()],
        vec!["--check", "/nonexistent/corpus"],
    ] {
        let reply = replay_verb(&args(&line));
        assert_eq!(reply.code, 2, "{line:?}");
        assert!(reply.text.is_empty(), "{line:?}");
    }
    let reply = replay_verb(&args(&["--check", corpus.as_str()]));
    assert_eq!(reply.code, 2, "{}", reply.text);
    assert!(
        reply.text.starts_with("NOT REPLAYED ") && reply.text.contains("bad.atape.txt"),
        "{}",
        reply.text
    );
    assert!(
        reply.text.ends_with("1 tape(s): 1 NOT REPLAYED\n"),
        "{}",
        reply.text
    );
    let ReplayArgs {
        file,
        options,
        check,
        ..
    } = replay_args(&args(&[
        "--check",
        corpus.as_str(),
        "--set",
        "approve=safe",
    ]))
    .expect("parses");
    assert!(check);
    assert_eq!(file, PathBuf::from(&corpus));
    assert_eq!(
        options.candidate.expect("a candidate").sets,
        [("approve".to_string(), "safe".to_string())]
    );
    let err = run(&Opts {
        socket: None,
        cmd: args(&["check", corpus.as_str()]),
    })
    .expect_err("no `check` verb");
    assert!(err.contains("unknown command 'check'"), "{err}");
}

/// A replay reads its tape as the `tape` verbs do: a regular file at
/// most the read budget long — one byte over is refused, never replayed
/// as a shorter tape — and a directory or a FIFO refused at once (a FIFO
/// with no writer would park a blocking open for ever: the one-minute
/// wait below is the hang detector), each exit 2.
#[cfg(unix)]
#[test]
fn replay_reads_its_tape_bounded_and_regular() {
    use crate::supervise::tape::codec::{End, Item};
    use crate::supervise::tape::frame::TapeWriter;
    let dir = crate::supervise::ScratchDir::new("drive-replay", "bounded");
    let path = dir.join("run.atape");
    {
        let file = store::create(&path).expect("made");
        let mut w = TapeWriter::start(
            Box::new(file),
            "run.atape",
            "not a header\n",
            Box::new(std::io::sink()),
        );
        w.append(&Item::End(End::Returned {
            how: "exit 0".to_string(),
        }));
    }
    let len = std::fs::metadata(&path).expect("the tape").len();
    let file = path.to_string_lossy().into_owned();
    let err = replay_reply_with_limit(&args(&[file.as_str()]), len).expect_err("no header");
    assert!(err.contains("not a tape header"), "read whole: {err}");
    let err = replay_reply_with_limit(&args(&[file.as_str()]), len - 1).expect_err("over");
    assert!(err.contains("exceeds"), "one byte over its budget: {err}");

    let directory = dir.to_string_lossy().into_owned();
    let err = replay_reply(&args(&[directory.as_str()])).expect_err("a directory");
    assert!(err.contains("not a readable regular file"), "{err}");

    use std::os::unix::ffi::OsStrExt as _;
    let fifo = dir.join("run.fifo");
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("a C path");
    // SAFETY: one NUL-terminated, owned scratch path; no pointer is kept.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let fifo = fifo.to_string_lossy().into_owned();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let tape_code = replay_verb(&args(&[fifo.as_str()])).code;
        let policy_refused = candidate_file(&fifo).is_err();
        let _ = tx.send((tape_code, policy_refused));
    });
    let refused = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the tape/policy FIFO admission did not come back: it blocked at the open");
    assert_eq!(refused, (2, true));
}

#[test]
fn a_replay_refusal_shows_the_tapes_control_bytes_as_text() {
    use crate::supervise::tape::codec::{End, Item};
    use crate::supervise::tape::frame::TapeWriter;
    const OSC: &str = "\x1b]52;c;cHduZWQ=\x07\x1b]0;title\x07";
    let dir = crate::supervise::ScratchDir::new("drive-replay", "shown");
    let twice = dir.join("twice.atape");
    {
        let file = store::create(&twice).expect("made");
        let mut w = TapeWriter::start(
            Box::new(file),
            "twice.atape",
            &format!("atape 1\n[{OSC}]\nk\nk\n"),
            Box::new(std::io::sink()),
        );
        w.append(&Item::End(End::Returned {
            how: "exit 0".to_string(),
        }));
    }
    let file = twice.to_string_lossy().into_owned();
    let err = replay_reply(&args(&[file.as_str()])).expect_err("refused");
    assert!(err.contains("said twice"), "{err}");
    assert!(
        err.contains("\\x1b]52;c;"),
        "the sequence said as text: {err}"
    );
    assert!(
        !err.chars().any(|c| c.is_control()),
        "no control byte reaches the terminal: {err:?}"
    );
}

/// A tape reader admits ordinary binary and text files, including a link
/// to one, at the exact input limit; an overflow is refused, never parsed
/// as a shorter tape.
#[cfg(unix)]
#[test]
fn tape_file_reads_refuse_overflow_and_keep_regular_symlink_inputs() {
    use crate::supervise::tape::codec::{End, Item};
    use crate::supervise::tape::frame::TapeWriter;
    use crate::supervise::tape::frame_tests::{Memory, Said};

    let dir = crate::supervise::ScratchDir::new("tape-read", "bounded");
    let end = Item::End(End::Returned {
        how: "exit 0".to_string(),
    });
    let text = crate::supervise::tape::text::render(None, std::slice::from_ref(&end));
    let sink = Memory::default();
    let mut writer = TapeWriter::start(
        Box::new(sink.clone()),
        "memory.atape",
        "atape 1\n",
        Box::new(Said::default()),
    );
    writer.append(&end);
    drop(writer);
    let files = [
        ("text.atape", text.into_bytes()),
        ("binary.atape", sink.bytes()),
    ];
    let mut oversized_admissions = Vec::new();
    for (name, bytes) in &files {
        let file = dir.join(name);
        std::fs::write(&file, bytes).expect("owned tape file");
        let cap = u64::try_from(bytes.len()).expect("small fixture");
        let words = vec![
            "cat".to_string(),
            file.to_string_lossy().into_owned(),
            "--text".to_string(),
        ];
        let (rendered, note) = tape_reply_with_limit(&words, cap).expect("exact-limit tape");
        assert!(rendered.contains("exit 0"));
        assert!(note.is_none());
        let link = dir.join(format!("{name}.link"));
        std::os::unix::fs::symlink(&file, &link).expect("owned regular-file link");
        let linked = vec![
            "cat".to_string(),
            link.to_string_lossy().into_owned(),
            "--text".to_string(),
        ];
        assert_eq!(
            tape_reply_with_limit(&linked, cap).expect("regular-file symlink"),
            (rendered, note)
        );
        if tape_reply_with_limit(&words, cap - 1).is_ok() {
            oversized_admissions.push(*name);
        }
        assert_eq!(std::fs::read(&file).expect("original remains"), *bytes);
    }
    let directory = vec!["cat".to_string(), dir.to_string_lossy().into_owned()];
    assert!(tape_reply_with_limit(&directory, 1024).is_err());
    assert!(
        oversized_admissions.is_empty(),
        "raw tape input over its read limit was admitted: {oversized_admissions:?}"
    );
}
