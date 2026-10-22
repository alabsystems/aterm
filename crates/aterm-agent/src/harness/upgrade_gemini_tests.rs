// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The parent's first two words, typed by name (measured, 0.46.0 Homebrew).
const NODE: &str = "/opt/homebrew/opt/node/bin/node";
const SCRIPT: &str = "/opt/homebrew/bin/gemini";
/// A session id as 0.46.0 printed one, and another.
const S1: &str = "11edd4f1-7048-4ff7-8ff2-83cce0095a7c";
const S2: &str = "88409a32-5b1e-4c2a-9d7f-0e1f2a3b4c5d";
/// The measured project, and its `projectHash`.
const ROOT: &str = "/private/tmp/at.u46v/proj";
const HASH: &str = "d944508ee7adf907b4df9f68c97455f0ffc044f6295c471e7e4c8db9cdb4af41";

fn launch(rest: &[&str]) -> Vec<String> {
    let mut out = argv(&[NODE, SCRIPT]);
    out.extend(rest.iter().map(|w| (*w).to_string()));
    out
}

/// The process Gemini CLI runs as is told by its script, under `node`: typed
/// by name (the PATH link), by path (the bundle), npm's `bin/gemini`.
/// NEGATIVE CONTROLS: the child (its first argument a node flag), a Claude
/// Code under node, and a `gemini` script under another runtime are none.
#[test]
fn the_process_is_node_running_gemini_clis_script() {
    let node = std::path::Path::new("/opt/homebrew/Cellar/node/26.3.0/bin/node");
    for (args, want) in [
        (launch(&[]), true),
        (launch(&["--resume", S1]), true),
        (
            argv(&[
                NODE,
                "/opt/homebrew/Cellar/gemini-cli/0.46.0/libexec/lib/node_modules/@google/gemini-cli/bundle/gemini.js",
            ]),
            true,
        ),
        (argv(&[NODE, "/Users//u/.npm-global/bin/gemini"]), true),
        (argv(&[NODE, "--max-old-space-size=18432", SCRIPT]), false),
        (
            argv(&[
                NODE,
                "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            ]),
            false,
        ),
        (argv(&[NODE, "/w/gemini.js"]), false),
        (argv(&[NODE]), false),
    ] {
        assert_eq!(is_gemini_process(Some(node), &args), want, "{args:?}");
    }
    assert!(!is_gemini_process(
        Some(std::path::Path::new("/usr/local/bin/bun")),
        &launch(&[])
    ));
    assert!(!is_gemini_process(None, &launch(&[])));
}

/// The relaunch carries what this run chose and the CLI does not keep with
/// the session (the model, the approval posture, YOLO, the sandbox, the
/// policy and extension lists, the include directories, trust, debug, the
/// screen reader and raw output), drops what names the session or sends a
/// prompt again (`--resume`, `--session-id`, `--session-file`, `-i`, a
/// positional prompt), and resumes the witnessed session as `--resume
/// <uuid>`, every word kept as typed.
/// NEGATIVE CONTROLS: a switch's `false` is never dropped as a positional
/// (the relaunch is never looser), and the launch's session is never the one
/// typed.
#[test]
fn a_relaunch_carries_the_runs_choices_and_drops_what_names_the_session() {
    let old = "00000000-1111-4222-8333-444444444444";
    let args = launch(&[
        "-m",
        "gemini-3.5-flash",
        "--approval-mode=auto_edit",
        "--sandbox",
        "--policy",
        "/w/p.toml",
        "-e",
        "ext1",
        "--include-directories=/w/lib",
        "--skip-trust",
        "-d",
        "--screen-reader",
        "--raw-output",
        "--accept-raw-output-risk",
        "--allowed-mcp-server-names",
        "srv",
        "--resume",
        old,
        "-i",
        "carry on from argv",
        "fix the bug",
    ]);
    assert_eq!(
        rewrite_argv(&args, Some(S1)),
        Ok(argv(&[
            "-m",
            "gemini-3.5-flash",
            "--approval-mode=auto_edit",
            "--sandbox",
            "--policy",
            "/w/p.toml",
            "-e",
            "ext1",
            "--include-directories=/w/lib",
            "--skip-trust",
            "-d",
            "--screen-reader",
            "--raw-output",
            "--accept-raw-output-risk",
            "--allowed-mcp-server-names",
            "srv",
            "--resume",
            S1,
        ]))
    );
    assert_eq!(argv_session(&args).as_deref(), Some(old));
    // A switch takes a bare `true`/`false`: `--yolo false` stays off.
    assert_eq!(
        rewrite_argv(&launch(&["--yolo", "false", "hello"]), Some(S1)),
        Ok(argv(&["--yolo", "false", "--resume", S1]))
    );
    assert_eq!(
        rewrite_argv(&launch(&["-y", "--model", "x"]), None),
        Ok(argv(&["-y", "--model", "x"]))
    );
    // `--resume` alone (the newest) takes no flag after it; a session id
    // given as `--session-id` and an import are replaced alike.
    assert_eq!(
        rewrite_argv(&launch(&["--resume", "--debug"]), Some(S1)),
        Ok(argv(&["--debug", "--resume", S1]))
    );
    for words in [
        &["--session-id", S2][..],
        &["--session-file", "/w/s.json"],
        &["-r=3"],
        &["--", "--model", "x"],
    ] {
        assert_eq!(
            rewrite_argv(&launch(words), Some(S1)),
            Ok(argv(&["--resume", S1])),
            "{words:?}"
        );
    }
    // A positional naming a subcommand, past a first positional, is a prompt.
    assert_eq!(
        rewrite_argv(&launch(&["hello", "mcp"]), None),
        Ok(Vec::new())
    );
}

/// A launch no relaunch brings back is refused with its word, a flag the
/// table does not know refuses, and so does a session that is no full UUID.
#[test]
fn a_launch_no_resume_carries_is_refused() {
    for (words, want) in [
        (&["-w"][..], ArgvRefusal::NotResumable("w".into())),
        (
            &["--worktree", "wt"],
            ArgvRefusal::NotResumable("worktree".into()),
        ),
        (
            &["--fake-responses", "/w/f.json"],
            ArgvRefusal::NotResumable("fake-responses".into()),
        ),
        (&["-p", "hi"], ArgvRefusal::NotResumable("p".into())),
        (&["-o", "json"], ArgvRefusal::NotResumable("o".into())),
        (&["--acp"], ArgvRefusal::NotResumable("acp".into())),
        (
            &["--list-sessions"],
            ArgvRefusal::NotResumable("list-sessions".into()),
        ),
        (&["-v"], ArgvRefusal::NotResumable("v".into())),
        (&["mcp", "list"], ArgvRefusal::NotResumable("mcp".into())),
        (
            &["--approvalMode", "yolo"],
            ArgvRefusal::UnknownFlag("approvalMode".into()),
        ),
        (
            &["--no-sandbox"],
            ArgvRefusal::UnknownFlag("no-sandbox".into()),
        ),
        (&["-sy"], ArgvRefusal::UnknownFlag("sy".into())),
        (&["--model"], ArgvRefusal::UnknownFlag("model".into())),
        (&["--model", "-d"], ArgvRefusal::UnknownFlag("model".into())),
    ] {
        assert_eq!(
            rewrite_argv(&launch(words), Some(S1)),
            Err(want),
            "{words:?}"
        );
    }
    for bad in ["3", "latest", "11edd4f1", "custom_id-1", ""] {
        assert!(rewrite_argv(&launch(&[]), Some(bad)).is_err(), "{bad}");
    }
}

/// A TUI is any launch but a one-shot or a subcommand; a launch the rewrite
/// refuses for a flag it does not know or one that refuses is still a TUI.
#[test]
fn a_tui_is_every_launch_but_a_one_shot() {
    for (words, tui) in [
        (&[][..], true),
        (&["--resume", S1], true),
        (&["hello"], true),
        (&["-w"], true),
        (&["--approvalMode", "yolo"], true),
        (&["--approvalMode", "yolo", "-p", "x"], false),
        (&["-p", "x"], false),
        (&["--version"], false),
        (&["extensions", "list"], false),
        (&["--frob", "skills"], false),
    ] {
        assert_eq!(is_tui_argv(&launch(words)), tui, "{words:?}");
    }
}

/// The session an argv names, by its full UUID only. NEGATIVE CONTROLS: an
/// index, `latest`, a custom id and a resume with no value name none.
#[test]
fn an_argv_names_a_session_by_its_full_uuid_alone() {
    for (words, want) in [
        (&["--resume", S1][..], Some(S1)),
        (&["-r", S1], Some(S1)),
        (&[&format!("--resume={S2}") as &str], Some(S2)),
        (&["--session-id", S2], Some(S2)),
        (&["--resume", "3"], None),
        (&["--resume", "latest"], None),
        (&["--session-id", "mine_1"], None),
        (&["--resume"], None),
        (&["--", "--resume", S1], None),
    ] {
        assert_eq!(argv_session(&launch(words)).as_deref(), want, "{words:?}");
    }
    assert!(is_session_uuid(S1));
    assert!(!is_session_uuid("11edd4f17048-4ff7-8ff2-83cce0095a7c0"));
}

/// The Gemini directory: `GEMINI_CLI_HOME`'s where set (it moves the whole
/// home), else `HOME`'s, else the fallback's; a relative `GEMINI_CLI_HOME`
/// is unmeasured and names none.
#[test]
fn the_gemini_directory_follows_its_cli_home_then_its_home() {
    let fb = std::path::Path::new("/Users//me");
    assert_eq!(
        gemini_dir(Some("/alt"), Some("/h"), fb),
        Some(PathBuf::from("/alt/.gemini"))
    );
    assert_eq!(
        gemini_dir(Some(""), Some("/h"), fb),
        Some(PathBuf::from("/h/.gemini"))
    );
    assert_eq!(gemini_dir(Some("rel"), Some("/h"), fb), None);
    assert_eq!(
        gemini_dir(None, Some("rel"), fb),
        Some(PathBuf::from("/Users//me/.gemini"))
    );
    assert_eq!(project_hash(ROOT), HASH, "the measured project's hash");
    assert!(owns("/private/tmp/at.u46v/proj\n", ROOT));
    assert!(!owns("/private/tmp/at.u46v/proj2", ROOT));
    assert_eq!(
        stamp_ms("2026-10-17T18:21:34.531Z"),
        Some(1_792_261_294_531)
    );
    assert_eq!(stamp_ms("2026-10-17T18:21:34Z"), Some(1_792_261_294_000));
    assert_eq!(stamp_ms("2026-10-17 18:21:34Z"), None);
    assert!(names_session(
        &format!("session-2026-10-17T18-21-{}.jsonl", &S1[..8]),
        S1
    ));
    assert!(!names_session(
        &format!("session-2026-10-17T18-21-{}.jsonl", &S2[..8]),
        S1
    ));
}

// ---------------------------------------------------------------- the records

/// A header line as 0.46.0 writes one.
fn header(session: &str, at: &str) -> String {
    format!(
        r#"{{"sessionId":"{session}","projectHash":"{HASH}","startTime":"{at}","lastUpdated":"{at}","kind":"main"}}"#
    )
}

/// The `$set.messages` a launch writes after its header: the CLI's own
/// `<session_context>`, stamped `at`.
fn context(at: &str, id: &str) -> String {
    format!(
        r#"{{"$set":{{"lastUpdated":"{at}","messages":[{{"id":"{id}","timestamp":"{at}","type":"user","content":[{{"text":"<session_context>\nThis is the Gemini CLI.\n</session_context>"}}]}}]}}}}"#
    )
}

/// One turn: a person's message and the model's reply (a plain string, as a
/// live turn leaves it), each with `$set.lastUpdated`.
fn turn(at: &str, user: &str, reply: &str) -> String {
    format!(
        "{{\"id\":\"{user}\",\"timestamp\":\"{at}\",\"type\":\"user\",\"content\":[{{\"text\":\"remember DELTA-FOUR\"}}]}}\n\
         {{\"$set\":{{\"lastUpdated\":\"{at}\"}}}}\n\
         {{\"id\":\"{reply}\",\"timestamp\":\"{at}\",\"type\":\"gemini\",\"content\":\"ok\"}}\n\
         {{\"$set\":{{\"lastUpdated\":\"{at}\"}}}}"
    )
}

fn lines(parts: &[String]) -> String {
    parts.join("\n") + "\n"
}

/// The turn-holding file of the measured chain A (`session-…T18-21-…`): its
/// launch, one turn, resume 1's rewrite bumping `lastUpdated`, resume 2's
/// switch onto it (no bump: its minute differs), and a later start's summary.
fn chain_a_turns(rewrite_at: Option<&str>) -> String {
    let mut parts = vec![
        header(S1, "2026-10-17T18:21:34.531Z"),
        context("2026-10-17T18:21:34.532Z", "ctx1"),
        turn("2026-10-17T18:22:04.634Z", "u1", "g1"),
        r#"{"$set":{"sessionId":"11edd4f1-7048-4ff7-8ff2-83cce0095a7c"}}"#.to_string(),
    ];
    if let Some(at) = rewrite_at {
        parts.push(format!(
            r#"{{"$set":{{"lastUpdated":"{at}","messages":[{{"id":"ctx1","timestamp":"2026-10-17T18:21:34.532Z","type":"user","content":[{{"text":"<session_context>"}}]}},{{"id":"u1","timestamp":"2026-10-17T18:22:04.634Z","type":"user","content":[{{"text":"remember DELTA-FOUR"}}]}},{{"id":"g1","timestamp":"2026-10-17T18:22:04.659Z","type":"gemini","content":[{{"text":"ok"}}]}}]}}}}"#
        ));
        parts.push(r#"{"$set":{"sessionId":"11edd4f1-7048-4ff7-8ff2-83cce0095a7c"}}"#.to_string());
    }
    parts.push(
        r#"{"$set":{"summary":"Lab stub session","memoryScratchpad":{"version":1}}}"#.to_string(),
    );
    lines(&parts)
}

/// A resume's header-only duplicate, begun at `at`.
fn duplicate(session: &str, at: &str) -> String {
    lines(&[header(session, at), context(at, &format!("ctx-{at}"))])
}

fn file<'a>(name: &'a str, text: &'a str) -> ChatsFile<'a> {
    ChatsFile {
        name,
        text,
        mtime_ms: None,
    }
}

/// THE PRE-CHECK PREDICTS WHAT THE RESUME LOADS (the premeasure's chain A,
/// record for record): after a turn and resume 1 (its rewrite bumps the
/// turn-holding file), the pick holds the turn — SAFE; after resume 2 with no
/// turn between (its duplicate newest), the pick is that empty duplicate —
/// UNSAFE, which typed would bring the session back without its turn. A
/// turn taken into the duplicate then splits the history — UNSAFE. A session
/// that never took a turn is NO-TURNS; no file of it is NO-FILE; a legacy
/// `.json` of it cannot be folded. NEGATIVE CONTROL: a tie on `lastUpdated`
/// keeps the earlier name, as the CLI's sorted pass does.
#[test]
fn the_precheck_folds_the_sessions_files_as_the_cli_does() {
    let turns_after_1 = chain_a_turns(Some("2026-10-17T18:22:40.322Z"));
    let dup1 = duplicate(S1, "2026-10-17T18:22:40.299Z");
    let dup2 = duplicate(S1, "2026-10-17T18:23:41.000Z");
    let n_turns = format!("session-2026-10-17T18-21-{}.jsonl", &S1[..8]);
    let n_dup1 = format!("session-2026-10-17T18-22-{}.jsonl", &S1[..8]);
    let n_dup2 = format!("session-2026-10-17T18-23-{}.jsonl", &S1[..8]);
    let after_1 = [file(&n_turns, &turns_after_1), file(&n_dup1, &dup1)];
    assert_eq!(
        precheck(&after_1, S1),
        Precheck::Safe {
            pick: n_turns.clone(),
            turns: 2
        }
    );
    let after_2 = [
        file(&n_turns, &turns_after_1),
        file(&n_dup1, &dup1),
        file(&n_dup2, &dup2),
    ];
    assert_eq!(
        precheck(&after_2, S1),
        Precheck::Unsafe {
            pick: n_dup2.clone(),
            missing: 2
        },
        "resume 3 would load the empty duplicate"
    );
    let split = lines(&[
        header(S1, "2026-10-17T18:23:41.000Z"),
        context("2026-10-17T18:23:41.000Z", "ctx3"),
        turn("2026-10-17T18:24:00.000Z", "u9", "g9"),
    ]);
    let split_files = [
        file(&n_turns, &turns_after_1),
        file(&n_dup1, &dup1),
        file(&n_dup2, &split),
    ];
    assert_eq!(
        precheck(&split_files, S1),
        Precheck::Unsafe {
            pick: n_dup2.clone(),
            missing: 2
        },
        "a turn in the duplicate splits the history"
    );
    assert_eq!(precheck(&[file(&n_dup1, &dup1)], S1), Precheck::NoTurns);
    assert_eq!(precheck(&[], S1), Precheck::NoFile);
    assert_eq!(
        precheck(&[file(&n_turns, &turns_after_1)], S2),
        Precheck::NoFile,
        "another session's files are not its"
    );
    let legacy = format!("session-2026-10-17T18-21-{}.json", &S1[..8]);
    assert_eq!(
        precheck(&[file(&legacy, "{}"), file(&n_dup1, &dup1)], S1),
        Precheck::Unreadable(legacy.clone())
    );
    // A tie keeps the earlier name: the turn file, dated as the duplicate.
    let tie_turns = lines(&[
        header(S1, "2026-10-17T18:21:34.531Z"),
        context("2026-10-17T18:21:34.532Z", "ctx1"),
        turn("2026-10-17T18:22:04.634Z", "u1", "g1"),
        r#"{"$set":{"lastUpdated":"2026-10-17T18:30:00.000Z"}}"#.to_string(),
    ]);
    let tie_dup = duplicate(S1, "2026-10-17T18:30:00.000Z");
    assert!(matches!(
        precheck(&[file(&n_dup1, &tie_dup), file(&n_turns, &tie_turns)], S1),
        Precheck::Safe { .. }
    ));
}

/// The fold follows the CLI's records: a message written again keeps its
/// place, `$set.messages` replaces them, `$rewindTo` drops that message and
/// every one after it (all of them where it names none), and the
/// `<session_context>` is no turn while it makes the file a session.
#[test]
fn the_fold_follows_the_clis_records() {
    let text = lines(&[
        header(S1, "2026-10-17T18:21:34.531Z"),
        context("2026-10-17T18:21:34.532Z", "ctx1"),
        turn("2026-10-17T18:22:00.000Z", "u1", "g1"),
        turn("2026-10-17T18:23:00.000Z", "u2", "g2"),
        r#"{"$rewindTo":"u2"}"#.to_string(),
        "{\"id\":\"u1\",\"timestamp\":\"2026-10-17T18:24:00.000Z\",\"type\":\"user\",\"content\":\"again\"}".to_string(),
        "{torn".to_string(),
    ]);
    let f = fold(&text);
    assert_eq!(f.session.as_deref(), Some(S1));
    assert_eq!(f.project_hash.as_deref(), Some(HASH));
    assert_eq!(f.turns, ["u1", "g1"]);
    assert!(f.valid);
    let none = lines(&[
        header(S1, "2026-10-17T18:21:34.531Z"),
        r#"{"$rewindTo":"nope"}"#.into(),
    ]);
    let f = fold(&none);
    assert!(f.turns.is_empty() && !f.valid);
}

// ---------------------------------------------------------------- the witness

/// Unix ms of the measured tab's parent start (18:28:18.0 UTC).
const START: u64 = 1_792_261_698_000;

fn at(ms_after_start: u64) -> String {
    let ms = START + ms_after_start;
    format!(
        "{}.{:03}Z",
        aterm_types::rfc3339::format_rfc3339(ms / 1000).trim_end_matches('Z'),
        ms % 1000
    )
}

/// THE START-TIME RULE NAMES THE ONE SESSION BEGUN SINCE THE PROCESS
/// STARTED: a fresh launch's file, a resume's duplicate, a resume whose
/// header was appended to its own minute's file. Anything else names none:
/// no header yet, two (a `/clear` since, another launch since), another
/// Gemini CLI started at the same moment, an id that is no UUID or another
/// project's, an argv naming another session, and another session's file
/// moved since (an in-TUI `/resume` switching onto it: a `$set.sessionId`
/// no stamped write follows, written since — a summary written after it
/// hides nothing; or a turn taken in it since). NEGATIVE CONTROLS: the
/// summary a start writes into an older file, an old switch written before
/// the start or followed by a stamped write, and a `--resume` launch's switch
/// a later start's summary follows (its header with it), move nothing.
#[test]
fn the_start_time_rule_names_one_session_begun_since_the_start() {
    let older = chain_a_turns(None);
    let n_older = "session-2026-10-17T18-21-11edd4f1.jsonl";
    let launch_file = duplicate(S2, &at(900));
    let n_launch = "session-2026-10-17T18-28-88409a32.jsonl";
    // The duplicate of the `--resume` whose switch the older file holds,
    // begun before the start.
    let resumed_dup = duplicate(S1, "2026-10-17T18:25:10.000Z");
    let n_resumed_dup = "session-2026-10-17T18-25-11edd4f1.jsonl";
    // The rule over `files` (those written since the start), each switch's
    // session read over `all` (every file the project holds).
    let rule = |files: &[ChatsFile<'_>], all: &[ChatsFile<'_>]| {
        witness(files, START, HASH, None, false, &|s| {
            resumes_account_for_switches(all, s)
        })
    };
    let w = |files: &[ChatsFile<'_>]| rule(files, files);
    // A fresh launch: its own file, an older one the start wrote a summary
    // into after an old `--resume`'s switch, that resume's duplicate older.
    let summary_since = ChatsFile {
        name: n_older,
        text: &older,
        mtime_ms: Some(START + 920),
    };
    let launch = file(n_launch, &launch_file);
    assert_eq!(
        rule(
            &[summary_since, launch],
            &[summary_since, launch, file(n_resumed_dup, &resumed_dup)]
        ),
        Ok(S2.to_string())
    );
    // The same switch with no resume's header beside it is an in-TUI
    // `/resume`'s, the start's summary written after it (the start's
    // background summary of that older session, `generateAndSaveSummary`).
    assert_eq!(
        w(&[summary_since, launch]),
        Err(NoWitness::AnotherMoved),
        "a summary after a switch hides it"
    );
    assert_eq!(w(&[]), Err(NoWitness::NoHeader));
    // A `/clear` since: a second header.
    let cleared = duplicate("03f274bd-1111-4222-8333-444444444444", &at(60_000));
    assert_eq!(
        w(&[launch, file("session-x-03f274bd.jsonl", &cleared)]),
        Err(NoWitness::TwoHeaders)
    );
    // A resume appended to its own minute's file: one header since.
    let appended = format!("{older}{}", duplicate(S1, &at(800)));
    let none = |_: &str| false;
    assert_eq!(
        witness(
            &[file(n_older, &appended)],
            START,
            HASH,
            Some(S1),
            false,
            &none
        ),
        Ok(S1.to_string())
    );
    assert_eq!(
        witness(
            &[file(n_older, &appended)],
            START,
            HASH,
            Some(S2),
            false,
            &none
        ),
        Err(NoWitness::ArgvDisagrees)
    );
    assert_eq!(
        witness(&[launch], START, HASH, None, true, &none),
        Err(NoWitness::StartedTogether)
    );
    let custom = duplicate("mine_1", &at(900));
    assert_eq!(
        w(&[file("session-x-mine_1.jsonl", &custom)]),
        Err(NoWitness::NotTyped)
    );
    assert_eq!(
        witness(&[launch], START, "0f606e64", None, false, &none),
        Err(NoWitness::NotTyped),
        "another project's header"
    );
    // An in-TUI `/resume` onto the older session: a switch appended since,
    // beyond the one its old resume's header accounts for.
    let switched = format!("{older}{{\"$set\":{{\"sessionId\":\"{S1}\"}}}}\n");
    let switched_since = ChatsFile {
        name: n_older,
        text: &switched,
        mtime_ms: Some(START + 44_000),
    };
    let dup = file(n_resumed_dup, &resumed_dup);
    assert_eq!(
        rule(&[switched_since, launch], &[switched_since, launch, dup]),
        Err(NoWitness::AnotherMoved)
    );
    // The same switch written before the start: an old resume's.
    let switched_before = ChatsFile {
        name: n_older,
        text: &switched,
        mtime_ms: Some(START - 60_000),
    };
    assert_eq!(w(&[switched_before, launch]), Ok(S2.to_string()));
    // A switch a stamped write followed (a resume's rewrite, before the
    // start): that write tells when, and the switch came before it.
    let rewritten = format!(
        "{}{{\"$set\":{{\"lastUpdated\":\"2026-10-17T18:25:10.030Z\"}}}}\n",
        lines(&[
            header(S1, "2026-10-17T18:21:34.531Z"),
            context("2026-10-17T18:21:34.532Z", "ctx1"),
            turn("2026-10-17T18:22:04.634Z", "u1", "g1"),
            format!(r#"{{"$set":{{"sessionId":"{S1}"}}}}"#),
        ])
    );
    let rewritten_since = ChatsFile {
        name: n_older,
        text: &rewritten,
        mtime_ms: Some(START + 920),
    };
    assert_eq!(w(&[rewritten_since, launch]), Ok(S2.to_string()));
    // A turn taken in the older session since.
    let turned = format!("{older}{}\n", turn(&at(30_000), "u7", "g7"));
    assert_eq!(
        w(&[file(n_older, &turned), launch]),
        Err(NoWitness::AnotherMoved)
    );
    // Turns of its own session move nothing.
    let own = format!("{launch_file}{}\n", turn(&at(30_000), "u8", "g8"));
    assert_eq!(w(&[file(n_launch, &own)]), Ok(S2.to_string()));
}

/// A LONE HEADER BEGUN LATER THAN ITS OWN COULD BE NAMES NO SESSION: a
/// process that died before writing its own header, and another launch of
/// the project begun 30 s after its start (gone since, or never seen as
/// started together), is never read as running that launch's session
/// (`late-header`). NEGATIVE CONTROLS: its own header at the window's last
/// millisecond names it, a late one naming the very session its argv names
/// is that session (a loaded machine's own), and a `/clear` after its own
/// header is still two.
#[test]
fn a_lone_header_begun_later_than_its_own_could_be_names_none() {
    let none = |_: &str| false;
    let named = |text: &str, argv: Option<&str>| {
        witness(
            &[file("session-x-88409a32.jsonl", text)],
            START,
            HASH,
            argv,
            false,
            &none,
        )
    };
    let rule = |text: &str| named(text, None);
    assert_eq!(
        named(&duplicate(S2, &at(30_000)), Some(S2)),
        Ok(S2.to_string())
    );
    assert_eq!(
        named(&duplicate(S2, &at(30_000)), Some(S1)),
        Err(NoWitness::LateHeader),
        "late, and another session than its argv names"
    );
    assert_eq!(
        rule(&duplicate(S2, &at(30_000))),
        Err(NoWitness::LateHeader)
    );
    assert_eq!(
        rule(&duplicate(S2, &at(LATE_HEADER_MS + 1))),
        Err(NoWitness::LateHeader)
    );
    assert_eq!(
        rule(&duplicate(S2, &at(LATE_HEADER_MS))),
        Ok(S2.to_string())
    );
    let own_then_clear = format!(
        "{}{}",
        duplicate(S2, &at(900)),
        duplicate("03f274bd-1111-4222-8333-444444444444", &at(60_000))
    );
    assert_eq!(rule(&own_then_clear), Err(NoWitness::TwoHeaders));
}

/// A SWITCH ONTO A SESSION NO STAMPED WRITE FOLLOWS is its file's trailing
/// switch, whatever unstamped records follow it (a start's background
/// summary, the process's own `$set.summary` or `$set.directories`, a
/// `$rewindTo`); a stamped write after it (a message, a `$set.lastUpdated`, a
/// header) ends it. The switches onto a session are all `--resume` launches'
/// only where its headers outnumber them, over its own files alone, every one
/// read and none a legacy `.json`.
#[test]
fn a_trailing_switch_is_read_through_unstamped_records_and_counted_against_headers() {
    let switch = format!(r#"{{"$set":{{"sessionId":"{S1}"}}}}"#);
    let base = [
        header(S1, "2026-10-17T18:21:34.531Z"),
        context("2026-10-17T18:21:34.532Z", "ctx1"),
        turn("2026-10-17T18:22:04.634Z", "u1", "g1"),
        switch.clone(),
    ];
    let with = |tail: &[&str]| {
        let mut parts = base.to_vec();
        parts.extend(tail.iter().map(|t| (*t).to_string()));
        scan(&lines(&parts)).trailing_switch
    };
    assert_eq!(with(&[]).as_deref(), Some(S1));
    for unstamped in [
        r#"{"$set":{"summary":"Lab stub session","memoryScratchpad":{"version":1}}}"#,
        r#"{"$set":{"summary":"its own"}}"#,
        r#"{"$set":{"directories":["/w"]}}"#,
        r#"{"$rewindTo":"u1"}"#,
    ] {
        assert_eq!(with(&[unstamped]).as_deref(), Some(S1), "{unstamped}");
    }
    let message =
        r#"{"id":"u2","timestamp":"2026-10-17T18:30:00.000Z","type":"user","content":"hi"}"#;
    let bump = r#"{"$set":{"lastUpdated":"2026-10-17T18:30:00.000Z"}}"#;
    let appended = header(S1, "2026-10-17T18:30:00.000Z");
    for stamped in [message, bump, appended.as_str()] {
        assert_eq!(with(&[stamped]), None, "{stamped}");
    }
    // Counted against its headers.
    let turns = lines(&base);
    let n_turns = format!("session-2026-10-17T18-21-{}.jsonl", &S1[..8]);
    let dup = duplicate(S1, "2026-10-17T18:25:10.000Z");
    let n_dup = format!("session-2026-10-17T18-25-{}.jsonl", &S1[..8]);
    assert!(resumes_account_for_switches(
        &[file(&n_turns, &turns), file(&n_dup, &dup)],
        S1
    ));
    assert!(
        !resumes_account_for_switches(&[file(&n_turns, &turns)], S1),
        "a switch with no resume's header beside it"
    );
    let twice = format!("{turns}{switch}\n");
    assert!(
        !resumes_account_for_switches(&[file(&n_turns, &twice), file(&n_dup, &dup)], S1),
        "a second switch, one header of a resume"
    );
    assert!(!resumes_account_for_switches(&[], S1));
    let legacy = format!("session-2026-10-17T18-21-{}.json", &S1[..8]);
    assert!(!resumes_account_for_switches(
        &[
            file(&legacy, ""),
            file(&n_turns, &turns),
            file(&n_dup, &dup)
        ],
        S1
    ));
    // Another session's file is not its, whatever it holds.
    let n_other = format!("session-2026-10-17T18-26-{}.jsonl", &S2[..8]);
    let other = duplicate(S1, "2026-10-17T18:26:00.000Z");
    assert!(!resumes_account_for_switches(
        &[file(&n_turns, &turns), file(&n_other, &other)],
        S1
    ));
}

// ---------------------------------------------------------------- the exit

/// The exit table: `0` and `130` clean; `41`, `42`, `44`, `52`-`55` a refused
/// start; `1` and a signal death nothing catches (`137` measured) a crash; a
/// stop signal's status stopped; anything else (the parent ignores TERM, HUP
/// and INT) no witness. The relaunch line's own word reads `1`, `126` and
/// `127` as refusals too.
#[test]
fn the_exit_table_tells_a_crash_from_a_refusal_and_an_end() {
    // The signal rows are Unix's (libc's signals): elsewhere no status is a
    // signal's death or stop, so `137` is an end no witness tells.
    #[cfg(unix)]
    for (status, want) in [
        (137, ExitKind::Crashed),
        (128 + i64::from(libc::SIGSEGV), ExitKind::Crashed),
        (128 + i64::from(libc::SIGTSTP), ExitKind::Stopped),
    ] {
        assert_eq!(exit_kind(status), want, "{status}");
        assert_eq!(line_exit_kind(status), want, "{status}");
    }
    #[cfg(not(unix))]
    assert_eq!(exit_kind(137), ExitKind::Unwitnessed);
    for (status, want) in [
        (0, ExitKind::Clean),
        (130, ExitKind::Clean),
        (1, ExitKind::Crashed),
        (41, ExitKind::Refused),
        (42, ExitKind::Refused),
        (44, ExitKind::Refused),
        (52, ExitKind::Refused),
        (55, ExitKind::Refused),
        (143, ExitKind::Unwitnessed),
        (129, ExitKind::Unwitnessed),
        (2, ExitKind::Unwitnessed),
        (199, ExitKind::Unwitnessed),
    ] {
        assert_eq!(exit_kind(status), want, "{status}");
    }
    for (status, want) in [
        (1, ExitKind::Refused),
        (126, ExitKind::Refused),
        (127, ExitKind::Refused),
        (42, ExitKind::Refused),
        (0, ExitKind::Clean),
    ] {
        assert_eq!(line_exit_kind(status), want, "{status}");
    }
}

/// A WORKER'S OWN RESTART NAMES THE ID IT REFUSED ([`restart_id`]): `42`
/// from a launch under `--session-id <uuid>` (a worker's line: the id, then
/// its attached first prompt), the id whichever way it is written — the
/// caller holds it to the start-time rule. NEGATIVE CONTROLS: any other
/// status (`1` a crash, `0`, `199` the restart's own code, which the parent
/// never relays); `42` from a launch under no `--session-id`, one naming a
/// session to resume (`--resume`, bare or by id, `-r`) or a file to import
/// (`--session-file`), and one whose id is no UUID; and the status alone,
/// which is a refusal ([`exit_kind`]).
#[test]
fn a_workers_own_restart_names_the_id_it_refused() {
    let worker = launch(&[
        "--model",
        "gemini-3.5-flash",
        "--session-id",
        S1,
        "--prompt-interactive=fix it\n(Started by `s-1`)",
    ]);
    assert_eq!(restart_id(42, &worker), Some(S1));
    assert_eq!(
        restart_id(42, &launch(&[&format!("--session-id={S1}")])),
        Some(S1)
    );
    assert_eq!(exit_kind(42), ExitKind::Refused, "the status alone");
    for status in [1, 0, 41, 199] {
        assert_eq!(restart_id(status, &worker), None, "{status}");
    }
    for other in [
        launch(&[]),
        launch(&["--resume", S1]),
        launch(&["--session-id", S1, "--resume"]),
        launch(&["--session-id", S1, "-r", S2]),
        launch(&["--session-id", S1, "--session-file", "/tmp/s.json"]),
        launch(&["--session-id", "my-task"]),
    ] {
        assert_eq!(restart_id(42, &other), None, "{other:?}");
    }
    assert_eq!(ExitKind::Restarted.word(), "restarted");
}

/// A WORKER'S TRUSTED-FOLDERS FILE IS READ OFF A RECORD'S WORDS BY NAME
/// ([`trusted_folders_of_words`]), where absolute: what its relaunch carries
/// while the file stands. NEGATIVE CONTROLS: a record's homes still keep
/// none ([`env_of_words`]); a relative file, and a record naming none, give
/// none.
#[test]
fn a_workers_trusted_folders_file_is_read_off_its_record() {
    let words = env_words(&[
        ("GEMINI_CLI_HOME".to_string(), "/g/home".to_string()),
        (
            TRUSTED_FOLDERS_ENV.to_string(),
            "/state/tasks/s-1-1.trust.json".to_string(),
        ),
    ]);
    assert_eq!(
        trusted_folders_of_words(&words),
        Some("/state/tasks/s-1-1.trust.json")
    );
    assert_eq!(
        env_of_words(&words),
        [("GEMINI_CLI_HOME".to_string(), "/g/home".to_string())]
    );
    assert_eq!(
        trusted_folders_of_words(&[format!("{TRUSTED_FOLDERS_ENV}=tasks/s.json")]),
        None
    );
    assert_eq!(
        trusted_folders_of_words(&["HOME=/Users//alt".to_string()]),
        None
    );
}

// ---------------------------------------------------------------- the line

/// The homes a line carries: `GEMINI_CLI_HOME` where set, `HOME` where it is
/// another than the user's, and a worker's own trusted-folders file where an
/// absolute one is set (its relaunch stays trusted in its folder alone);
/// read back from a record by name alone, which keeps no trusted-folders
/// file (the live worker's, removed with its session). The line a person is
/// told cds to the project and resumes by the full UUID, behind those homes.
/// The told line never names the trusted-folders file. NEGATIVE CONTROLS:
/// the user's own `HOME` is not carried, nor a relative trusted-folders
/// file, and no line is told for an id that is no UUID.
#[test]
fn the_line_carries_the_homes_and_the_person_is_told_where_to_run_it() {
    let user = std::path::Path::new("/Users//me");
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    };
    assert_eq!(
        relaunch_env(env(&[("HOME", "/Users//me"), ("PATH", "/bin")]), user),
        Vec::<(String, String)>::new()
    );
    let both = relaunch_env(
        env(&[("HOME", "/Users//alt"), ("GEMINI_CLI_HOME", "/g/home")]),
        user,
    );
    assert_eq!(
        both,
        [
            ("HOME".to_string(), "/Users//alt".to_string()),
            ("GEMINI_CLI_HOME".to_string(), "/g/home".to_string()),
        ]
    );
    assert_eq!(env_of_words(&env_words(&both)), both);
    let trusted = relaunch_env(
        env(&[
            ("GEMINI_CLI_HOME", "/g/home"),
            (
                "GEMINI_CLI_TRUSTED_FOLDERS_PATH",
                "/state/tasks/s-1-1.trust.json",
            ),
        ]),
        user,
    );
    assert_eq!(
        trusted,
        [
            ("GEMINI_CLI_HOME".to_string(), "/g/home".to_string()),
            (
                TRUSTED_FOLDERS_ENV.to_string(),
                "/state/tasks/s-1-1.trust.json".to_string()
            ),
        ]
    );
    assert_eq!(
        env_of_words(&env_words(&trusted)),
        [("GEMINI_CLI_HOME".to_string(), "/g/home".to_string())],
        "a record keeps no trusted-folders file"
    );
    assert_eq!(
        relaunch_env(
            env(&[("GEMINI_CLI_TRUSTED_FOLDERS_PATH", "tasks/s-1-1.trust.json")]),
            user
        ),
        Vec::<(String, String)>::new(),
        "a relative one"
    );
    // The line a person is told names no worker's trusted-folders file: it
    // goes with the worker's session, and the person's own trust stands.
    assert_eq!(
        told_line(S1, &trusted, "/w/p").as_deref(),
        Some(format!("cd /w/p && GEMINI_CLI_HOME=/g/home gemini --resume {S1}").as_str())
    );
    assert_eq!(
        env_of_words(&["PATH=/x".to_string(), "HOME".to_string()]),
        Vec::<(String, String)>::new()
    );
    assert_eq!(
        told_line(S1, &[], "/w/my proj").as_deref(),
        Some(format!("cd '/w/my proj' && gemini --resume {S1}").as_str())
    );
    assert_eq!(
        told_line(S1, &both, "/w/p").as_deref(),
        Some(
            format!("cd /w/p && HOME=/Users//alt GEMINI_CLI_HOME=/g/home gemini --resume {S1}")
                .as_str()
        )
    );
    // A cd only to one folder: never one that climbs, nor on Windows a
    // drive-relative one; a drive's own root path is one.
    assert_eq!(
        told_line(S1, &[], "/w/../p").as_deref(),
        Some(format!("gemini --resume {S1}").as_str())
    );
    #[cfg(windows)]
    {
        assert_eq!(
            told_line(S1, &[], r"C:\w\p").as_deref(),
            Some(format!(r"cd 'C:\w\p' && gemini --resume {S1}").as_str())
        );
        assert_eq!(
            told_line(S1, &[], "C:p").as_deref(),
            Some(format!("gemini --resume {S1}").as_str())
        );
    }
    assert_eq!(told_line("3", &[], "/w/p"), None);
    assert_eq!(told_line(S1, &[], "/w/\np"), None);
    assert_eq!(
        package_version(r#"{"name":"@google/gemini-cli","version":"0.46.0"}"#).as_deref(),
        Some("0.46.0")
    );
    assert_eq!(package_version(r#"{"name":"other","version":"1"}"#), None);
}
