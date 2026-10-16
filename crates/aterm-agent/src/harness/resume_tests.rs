// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// This tab's conversation, and a sibling tab's in the same directory.
const OURS: &str = "5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
const SIBLING: &str = "0a0b0c0d-1111-4222-8333-444455556666";

/// A private `<claude dir>` with a `sessions/` directory, removed on drop.
struct ClaudeDir(aterm_tempfile::TempDir);

impl ClaudeDir {
    fn new(tag: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-resume-{tag}-"))
            .tempdir()
            .expect("sessions dir");
        std::fs::create_dir_all(root.path().join("sessions")).expect("sessions dir");
        Self(root)
    }

    /// Claude Code's record for `pid`, in the shape of `upgrade_tests.rs`' measured
    /// `SESSION_9162`, filed as `<file>.json`.
    fn record(&self, file: u32, pid: u32, session: &str, started: u64, status_at_ms: u64) {
        let text = format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/Users/_owner/aterm","startedAt":{},"procStart":"{}","version":"2.1.281","kind":"interactive","entrypoint":"cli","status":"idle","statusUpdatedAt":{status_at_ms}}}"#,
            started * 1000,
            crate::harness::footer::lstart_utc(started),
        );
        std::fs::write(
            self.0.path().join("sessions").join(format!("{file}.json")),
            text,
        )
        .expect("write record");
    }
}

/// The resume line the window's footer read gives for `pid` in `dir`
/// ([`crate::harness::footer::read_pid`], which `aterm-gui`'s resolver calls).
fn line_of(dir: &ClaudeDir, pid: u32, started: Option<u64>, argv: &[String]) -> Option<String> {
    use crate::harness::footer;
    let offset = |_: Option<&str>, _: i64| None;
    footer::read_pid(
        dir.0.path(),
        pid,
        started,
        |_| (footer::LaunchFacts::default(), argv.to_vec()),
        &mut footer::TailCache::default(),
        &mut footer::FooterCache::default(),
        &footer::ReadClock {
            read_at: std::time::SystemTime::now(),
            now: 0,
            offset_at: &offset,
        },
        None,
    )
    .and_then(|facts| facts.resume)
}

/// Two live Claude Code processes in one directory, the sibling's conversation the newer (the one
/// `claude --continue` would resume): each pid's command names its own conversation, flags carried.
/// Negative controls: no record, a reused pid, a record filed under another pid, a record of
/// another shape, and no `procStart` where the kernel start is known give no command.
#[test]
fn two_records_in_one_directory_each_pid_resumes_its_own_conversation() {
    let dir = ClaudeDir::new("two");
    let (ours_pid, ours_start) = (46_976, 1_790_194_435);
    let (sibling_pid, sibling_start) = (89_092, 1_790_199_000);
    dir.record(ours_pid, ours_pid, OURS, ours_start, 1_790_200_000_000);
    // The sibling spoke last: `--continue` from this directory picks it.
    dir.record(
        sibling_pid,
        sibling_pid,
        SIBLING,
        sibling_start,
        1_790_212_772_957,
    );
    let launch = argv(&[
        "claude",
        "--model",
        "opus",
        "--effort",
        "high",
        "fix the bug",
    ]);

    let ours = line_of(&dir, ours_pid, Some(ours_start), &launch).expect("our record");
    assert_eq!(ours, format!("claude --model opus --resume {OURS}"));
    assert!(!ours.contains(SIBLING), "{ours}");
    assert!(!ours.contains("--continue"), "{ours}");
    let sibling = line_of(&dir, sibling_pid, Some(sibling_start), &argv(&["claude"]))
        .expect("the sibling's record");
    assert_eq!(sibling, format!("claude --resume {SIBLING}"));
    // Without a known start the pid alone decides.
    assert_eq!(
        line_of(&dir, ours_pid, None, &launch).as_deref(),
        Some(ours.as_str())
    );

    // No record for the pid: no command.
    assert_eq!(line_of(&dir, 12_345, None, &launch), None);
    // A reused pid: the record's start is another process's.
    assert_eq!(line_of(&dir, ours_pid, Some(ours_start + 1), &launch), None);
    // A record filed under one pid that names another.
    dir.record(777, sibling_pid, SIBLING, sibling_start, 1);
    assert_eq!(line_of(&dir, 777, None, &launch), None);
    // A record of another shape.
    std::fs::write(dir.0.path().join("sessions/778.json"), r#"{"pid":778}"#).expect("write");
    assert_eq!(line_of(&dir, 778, None, &launch), None);
    // A record with no `procStart`: where the kernel's start is known nothing ties it to this
    // process, so no line to type.
    std::fs::write(
        dir.0.path().join("sessions/779.json"),
        format!(r#"{{"pid":779,"sessionId":"{OURS}","cwd":"/Users/_owner/aterm"}}"#),
    )
    .expect("write");
    let offset = |_: Option<&str>, _: i64| None;
    let facts = crate::harness::footer::read_pid(
        dir.0.path(),
        779,
        Some(ours_start),
        |_| {
            (
                crate::harness::footer::LaunchFacts::default(),
                launch.clone(),
            )
        },
        &mut crate::harness::footer::TailCache::default(),
        &mut crate::harness::footer::FooterCache::default(),
        &crate::harness::footer::ReadClock {
            read_at: std::time::SystemTime::now(),
            now: 0,
            offset_at: &offset,
        },
        None,
    )
    .expect("the footer reads the record");
    assert_eq!(facts.resume, None, "no procStart, a known start: no line");
    assert_eq!(
        line_of(&dir, 779, None, &launch),
        Some(format!("claude --model opus --resume {OURS}"))
    );
}

/// The line carries what the relaunch carries ([`upgrade::rewrite_argv`]): known flags verbatim,
/// quoted only where a shell would read them; the resume family, `--effort` and positionals
/// dropped. An unknown flag leaves the bare line; a launch not resumable in place, or an id that
/// is not one, gets none. A script launch reads its flags after the script.
#[test]
fn the_line_carries_the_launch_flags_as_the_relaunch_does() {
    let line = command(
        &argv(&[
            "/Users/_owner/.local/bin/claude",
            "--continue",
            "--add-dir",
            "/tmp/a dir",
            "--append-system-prompt",
            "be 'brief'",
            "--dangerously-skip-permissions",
        ]),
        OURS,
    )
    .expect("a line");
    assert_eq!(
        line,
        format!(
            "claude --add-dir '/tmp/a dir' --append-system-prompt 'be '\\''brief'\\''' \
             --dangerously-skip-permissions --resume {OURS}"
        )
    );
    assert!(!line.contains("--continue"), "{line}");
    assert_eq!(
        command(&argv(&["claude", "--brand-new-flag"]), OURS),
        Some(format!("claude --resume {OURS}")),
        "an unknown flag: the conversation alone"
    );
    for refused in [&["claude", "--worktree"][..], &["claude", "-p", "hi"]] {
        assert_eq!(command(&argv(refused), OURS), None, "{refused:?}");
    }
    assert_eq!(command(&argv(&["claude"]), "not an id; rm -rf ~"), None);
    assert_eq!(
        command(&argv(&["claude", "--model", "a\u{7}b"]), OURS),
        None,
        "a control character is never printed as a command"
    );
    assert_eq!(
        command(
            &argv(&[
                "node",
                "/opt/lib/node_modules/.bin/claude",
                "--model",
                "opus"
            ]),
            OURS
        ),
        Some(format!("claude --model opus --resume {OURS}"))
    );
    assert_eq!(bare_of(&line), Some(format!("claude --resume {OURS}")));
    assert_eq!(bare_of("claude --continue"), None);
}

/// [`never_cut`] keeps a named resume line whole, else its permission-mode form, else its bare
/// form, else drops the clause; text naming no line renders untouched. Negative control: the same
/// byte-cap renderer given the text directly cuts the id.
#[test]
fn a_named_resume_line_is_kept_whole_bare_or_not_at_all() {
    let cap = |n: usize| {
        move |s: &str| {
            if s.len() <= n {
                s.to_string()
            } else {
                let mut end = n - '…'.len_utf8();
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}…", &s[..end])
            }
        }
    };
    let line = format!("claude --dangerously-skip-permissions --model opus --resume {OURS}");
    let bare = format!("claude --resume {OURS}");
    let text =
        format!("memory critical: restart it{THEN_RESUME_WITH}{line}: Memory usage critical");
    // Room for all of it: untouched.
    assert_eq!(never_cut(&text, cap(500)), text);
    // Room for the id but not the tail: the whole line, the tail cut.
    let out = never_cut(&text, cap(text.len() - 5));
    assert!(out.contains(&line), "{out}");
    // Room for its permission mode, not its other flags: the mode alone, the last flag a line
    // gives up.
    let mode = format!("claude --dangerously-skip-permissions --resume {OURS}");
    let room = "memory critical: restart it, then resume with ".len() + mode.len() + 3;
    let out = never_cut(&text, cap(room));
    assert!(out.contains(&mode), "{out}");
    // Room for the bare form only.
    let room = "memory critical: restart it, then resume with ".len() + bare.len() + 3;
    let out = never_cut(&text, cap(room));
    assert!(
        out.contains(&bare) && !out.contains("--dangerously"),
        "{out}"
    );
    // Negative control: the renderer alone cuts the command's id.
    let cut = cap(room)(&text);
    assert!(
        cut.contains("resume with claude --") && !cut.contains(OURS),
        "{cut}"
    );
    // Room for no command: the clause goes, the rest is rendered.
    let out = never_cut(&text, cap(40));
    assert!(
        !out.contains("--resume") && !out.contains("resume with"),
        "{out}"
    );
    assert!(
        out.starts_with("memory critical: restart it: Memory"),
        "{out}"
    );
    // A text that names no line (or a `--resume` with no id) is the
    // renderer's alone.
    for plain in [
        "memory critical: restart it: Memory usage critical".to_string(),
        format!("x{THEN_RESUME_WITH}claude --resume not-an-id: y"),
    ] {
        assert_eq!(never_cut(&plain, cap(20)), cap(20)(&plain));
    }
}

/// A Codex's thread.
const THREAD: &str = "019a0b1c-2d3e-7f40-8a5b-6c7d8e9f0a1b";

/// A CODEX'S LINE ([`codex_command`]) is `codex resume <kept flags>
/// <thread>`, each word quoted as Claude Code's line quotes one, and steps
/// down as Claude Code's does ([`shorter`]): its approval and sandbox flags
/// alone ([`mode_of`]), then `codex resume <thread>` ([`bare_of`]). A word
/// that needs quotes reads back whole. NEGATIVE CONTROLS: a thread that is
/// not one, and a launch the rewrite refuses, give no line; a line that is
/// all permission mode steps straight to its bare form; text that is not
/// exactly such a line has no shorter form.
#[test]
fn a_codex_line_steps_down_to_its_mode_and_its_bare_form() {
    let launch = argv(&[
        "/opt/codex",
        "-m",
        "gpt-5-codex",
        "-c",
        "model_reasoning_effort=\"high\"",
        "-s",
        "read-only",
        "-a",
        "never",
        "hello",
    ]);
    let line = codex_command(&launch, THREAD, None).expect("a line");
    assert_eq!(
        line,
        format!(
            "codex resume -m gpt-5-codex -c 'model_reasoning_effort=\"high\"' -s read-only -a \
             never {THREAD}"
        )
    );
    let mode = format!("codex resume -s read-only -a never {THREAD}");
    let bare = format!("codex resume {THREAD}");
    assert_eq!(mode_of(&line), Some(mode.clone()));
    assert_eq!(bare_of(&line), Some(bare.clone()));
    assert_eq!(shorter(&line), [mode, bare.clone()]);
    let bypass = codex_command(
        &argv(&["codex", "--dangerously-bypass-approvals-and-sandbox"]),
        THREAD,
        None,
    )
    .expect("a line");
    assert_eq!(
        mode_of(&bypass).as_ref(),
        Some(&bypass),
        "its mode is all of it"
    );
    assert_eq!(shorter(&bypass), std::slice::from_ref(&bare));
    assert_eq!(shorter(&bare), Vec::<String>::new());
    // Negative controls.
    assert_eq!(codex_command(&argv(&["codex"]), "not-a-thread", None), None);
    assert_eq!(
        codex_command(&argv(&["codex", "--worktree"]), THREAD, None),
        None
    );
    assert_eq!(
        codex_command(&argv(&["codex", "--unknown"]), THREAD, None),
        None
    );
    for text in [
        format!("codex resume {THREAD} trailing"),
        format!("codex resume --nope {THREAD}"),
        format!("codex fork {THREAD}"),
    ] {
        assert_eq!(
            (bare_of(&text), shorter(&text)),
            (None, Vec::new()),
            "{text}"
        );
    }
}

/// A CODEX'S LINE A REASON NAMES IS NEVER CUT ([`never_cut`]), as Claude
/// Code's is not: whole, else its permission mode alone, else `codex resume
/// <thread>`, else its clause dropped. A flag's value that is itself a
/// thread id ends no line before the thread does. NEGATIVE CONTROL: the
/// renderer alone names the command and cuts its thread away.
#[test]
fn a_named_codex_line_is_kept_whole_bare_or_not_at_all() {
    let cap = |n: usize| {
        move |s: &str| {
            if s.len() <= n {
                s.to_string()
            } else {
                format!("{}…", &s[..n - '…'.len_utf8()])
            }
        }
    };
    let launch = argv(&[
        "codex",
        "--profile",
        "0a0b0c0d-1111-4222-8333-444455556666",
        "-s",
        "read-only",
        "-c",
        "model_reasoning_effort=high",
    ]);
    let line = codex_command(&launch, THREAD, None).expect("a line");
    let mode = format!("codex resume -s read-only {THREAD}");
    let bare = format!("codex resume {THREAD}");
    let text =
        format!("memory critical: restart it{THEN_RESUME_WITH}{line}: Memory usage critical");
    let head = "memory critical: restart it, then resume with ".len();
    assert_eq!(never_cut(&text, cap(500)), text);
    let out = never_cut(&text, cap(text.len() - 5));
    assert!(out.contains(&line), "{out}");
    let out = never_cut(&text, cap(head + mode.len() + 3));
    assert!(out.contains(&mode), "{out}");
    let out = never_cut(&text, cap(head + bare.len() + 3));
    assert!(out.contains(&bare) && !out.contains("read-only"), "{out}");
    let out = never_cut(&text, cap(40));
    assert!(
        !out.contains("codex") && !out.contains("resume with"),
        "{out}"
    );
    assert!(
        out.starts_with("memory critical: restart it: Memory"),
        "{out}"
    );
    // Negative control: cut by the renderer alone, the command is named and
    // its thread is not.
    let cut = cap(head + bare.len() + 3)(&text);
    assert!(
        cut.contains("resume with codex resume") && !cut.contains(THREAD),
        "{cut}"
    );
}

/// A CODEX STARTED UNDER `CODEX_HOME=<dir>` IS TOLD A LINE THAT CARRIES IT
/// ([`codex_command`]), as its relaunch line does: `CODEX_HOME=<dir> codex
/// resume <kept flags> <thread>`, the value quoted where a shell needs it and
/// the name never (a quoted `NAME=` is a command). Its shorter forms keep it
/// ([`shorter`]: the permission mode alone, then the bare form, each still
/// behind the home), and a reason names it whole or not at all
/// ([`never_cut`]). NEGATIVE CONTROLS: a Codex started with none is told no
/// home; a home with a control character gives no line; a line whose home is
/// not this one's spelling is no line of [`codex_command`]'s.
#[test]
fn a_codex_line_carries_the_codex_home_its_tui_was_started_with() {
    let launch = argv(&["codex", "-m", "gpt-5-codex", "-s", "read-only"]);
    let home = "/Users//x/my codex";
    let line = codex_command(&launch, THREAD, Some(home)).expect("a line");
    assert_eq!(
        line,
        format!("CODEX_HOME='/Users//x/my codex' codex resume -m gpt-5-codex -s read-only {THREAD}")
    );
    // Read back by a shell's rules: one assignment, then the command.
    let words = crate::harness::upgrade_drive::last_command(&line);
    assert_eq!(words[0], format!("CODEX_HOME={home}"));
    assert_eq!(words[1..3], ["codex", "resume"]);
    let mode = format!("CODEX_HOME='/Users//x/my codex' codex resume -s read-only {THREAD}");
    let bare = format!("CODEX_HOME='/Users//x/my codex' codex resume {THREAD}");
    assert_eq!(mode_of(&line), Some(mode.clone()));
    assert_eq!(bare_of(&line), Some(bare.clone()));
    assert_eq!(shorter(&line), [mode.clone(), bare.clone()]);
    // A plain path needs no quotes.
    assert_eq!(
        codex_command(&argv(&["codex"]), THREAD, Some("/alt/home")),
        Some(format!("CODEX_HOME=/alt/home codex resume {THREAD}"))
    );
    // A reason keeps the line whole, steps it down behind its home, or drops
    // the clause.
    let text =
        format!("memory critical: restart it{THEN_RESUME_WITH}{line}: Memory usage critical");
    let head = "memory critical: restart it, then resume with ".len();
    let cap = |n: usize| {
        move |s: &str| {
            if s.len() <= n {
                s.to_string()
            } else {
                format!("{}…", &s[..n - '…'.len_utf8()])
            }
        }
    };
    assert_eq!(never_cut(&text, cap(500)), text);
    let out = never_cut(&text, cap(head + mode.len() + 3));
    assert!(out.contains(&mode), "{out}");
    let out = never_cut(&text, cap(head + bare.len() + 3));
    assert!(out.contains(&bare) && !out.contains("read-only"), "{out}");
    let out = never_cut(&text, cap(head + 10));
    assert!(
        !out.contains("CODEX_HOME") && !out.contains("resume with"),
        "{out}"
    );
    // Negative controls.
    assert_eq!(
        codex_command(&launch, THREAD, None),
        Some(format!("codex resume -m gpt-5-codex -s read-only {THREAD}"))
    );
    assert_eq!(codex_command(&launch, THREAD, Some("/a\nb")), None);
    let respelled = line.replace(
        "CODEX_HOME='/Users//x/my codex'",
        "CODEX_HOME=\"/Users//x/my codex\"",
    );
    assert_eq!(
        (bare_of(&respelled), shorter(&respelled)),
        (None, Vec::new())
    );
}
