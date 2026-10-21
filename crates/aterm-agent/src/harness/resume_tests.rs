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

/// A SHORTER FORM NEVER DROPS THE CLAUDE HOME IT CARRIES (ATERM-KI-1015-1
/// LC-4): a told line carries the home its agent ran under in front of it
/// ([`with_env`]: plain where it can be, quoted otherwise), and every form a
/// tight budget steps down to — its permission mode alone, then the
/// conversation alone — keeps it, as [`never_cut`] keeps the whole clause or
/// none. Before, the permission form was none (its first word not `claude`)
/// and the bare form dropped the home: a line that resumes nothing under
/// the tab's shell's home. NEGATIVE CONTROLS: a line with no home gives the
/// forms it always did; a bad name or a control character gives no line.
#[test]
fn a_shorter_form_never_drops_the_home_it_carries() {
    let line = command(
        &argv(&[
            "claude",
            "--permission-mode",
            "plan",
            "--model",
            "claude-opus-5-5",
        ]),
        OURS,
    )
    .expect("a line");
    let plain_forms = shorter(&line);
    assert_eq!(plain_forms.len(), 2, "{plain_forms:?}");
    for (env, head) in [
        (vec![("HOME", "/o")], "HOME=/o "),
        (vec![("HOME", "/o w")], "HOME='/o w' "),
        (
            vec![(crate::harness::accounts::HOME_ENV, "/a b/.claude")],
            "CLAUDE_CONFIG_DIR='/a b/.claude' ",
        ),
    ] {
        let homed = with_env(&env, &line).expect("a homed line");
        assert_eq!(homed, format!("{head}{line}"));
        assert_eq!(
            shorter(&homed),
            plain_forms
                .iter()
                .map(|form| format!("{head}{form}"))
                .collect::<Vec<_>>(),
            "{homed}"
        );
        assert_eq!(
            bare_of(&homed),
            Some(format!("{head}claude --resume {OURS}"))
        );
        // Whatever the budget, the clause is whole, home and all, or gone.
        let text = format!("Claude Code froze{THEN_RESUME_WITH}{homed}.");
        for cap in (20..text.len() + 10).step_by(7) {
            let out = never_cut(&text, |t| t.chars().take(cap).collect());
            assert!(
                !out.contains(" --resume ") || out.contains(head),
                "{cap}: {out}"
            );
        }
    }
    // NEGATIVE CONTROLS.
    assert_eq!(with_env(&[], &line).as_deref(), Some(line.as_str()));
    assert_eq!(with_env(&[("home", "/o")], &line), None);
    assert_eq!(with_env(&[("HOME", "/o\nx")], &line), None);
}

/// THE FOOTER'S RESUME LINE CARRIES THE CLAUDE HOME ITS PROCESS RUNS UNDER
/// (ATERM-KI-1015-1 LC-4; [`crate::harness::footer::read_pid`], [`home_env`]):
/// the frozen-program remedy and the menu-bar row tell the line every other
/// surface tells — `HOME=<it>` for a process whose records are another
/// `HOME`'s `.claude`, its `CLAUDE_CONFIG_DIR` where its environment named
/// one. NEGATIVE CONTROL: records under the user's own `.claude` carry
/// nothing, and a read with no user home known adds nothing.
#[test]
fn the_footers_resume_line_carries_the_home_its_process_runs_under() {
    use crate::harness::footer;
    let root = ClaudeDir::new("footer-home");
    let other = root.0.path().join("other");
    let claude = other.join(".claude");
    std::fs::create_dir_all(claude.join("sessions")).expect("sessions");
    let (pid, started) = (46_977, 1_790_194_435);
    std::fs::write(
        claude.join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{OURS}","cwd":"/w","startedAt":{},"procStart":"{}","version":"2.1.281","kind":"interactive","entrypoint":"cli","status":"idle","statusUpdatedAt":1}}"#,
            started * 1000,
            footer::lstart_utc(started),
        ),
    )
    .expect("record");
    let read = |user_home: Option<&Path>, config: Option<&str>| {
        let offset = |_: Option<&str>, _: i64| None;
        footer::read_pid(
            &claude,
            pid,
            Some(started),
            |_| {
                (
                    footer::LaunchFacts {
                        config: config.map(str::to_string),
                        ..footer::LaunchFacts::default()
                    },
                    argv(&["claude"]),
                )
            },
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            &footer::ReadClock {
                read_at: std::time::SystemTime::now(),
                now: 0,
                offset_at: &offset,
            },
            None,
            user_home,
        )
        .and_then(|facts| facts.resume)
    };
    let bare = format!("claude --resume {OURS}");
    // A value as the line writes it: plain, or in single quotes.
    let word = |value: &str| {
        if value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-'))
        {
            value.to_string()
        } else {
            format!("'{value}'")
        }
    };
    let homedir = other.to_string_lossy().into_owned();
    assert_eq!(
        read(Some(Path::new("/u/me")), None),
        Some(format!(
            "{}={} {bare}",
            super::super::accounts::HOMEDIR_ENV,
            word(&homedir)
        )),
        "another HOME's records"
    );
    let config = claude.to_string_lossy().into_owned();
    assert_eq!(
        read(Some(Path::new("/u/me")), Some(&config)),
        Some(format!("CLAUDE_CONFIG_DIR={} {bare}", word(&config))),
        "its CLAUDE_CONFIG_DIR"
    );
    // NEGATIVE CONTROLS.
    assert_eq!(
        read(Some(&other), None),
        Some(bare.clone()),
        "the user's own"
    );
    assert_eq!(read(None, None), Some(bare));
    assert!(home_env(Path::new("/u/me"), None, Some(Path::new("/u/me/.claude"))).is_empty());
}
