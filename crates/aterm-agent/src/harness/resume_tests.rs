// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

const OURS: &str = "5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
const SIBLING: &str = "0a0b0c0d-1111-4222-8333-444455556666";

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

#[test]
fn two_records_in_one_directory_each_pid_resumes_its_own_conversation() {
    let dir = ClaudeDir::new("two");
    let (ours_pid, ours_start) = (46_976, 1_790_194_435);
    let (sibling_pid, sibling_start) = (89_092, 1_790_199_000);
    dir.record(ours_pid, ours_pid, OURS, ours_start, 1_790_200_000_000);
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
    assert_eq!(
        line_of(&dir, ours_pid, None, &launch).as_deref(),
        Some(ours.as_str())
    );

    assert_eq!(line_of(&dir, 12_345, None, &launch), None);
    assert_eq!(line_of(&dir, ours_pid, Some(ours_start + 1), &launch), None);
    dir.record(777, sibling_pid, SIBLING, sibling_start, 1);
    assert_eq!(line_of(&dir, 777, None, &launch), None);
    std::fs::write(dir.0.path().join("sessions/778.json"), r#"{"pid":778}"#).expect("write");
    assert_eq!(line_of(&dir, 778, None, &launch), None);
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
    assert_eq!(never_cut(&text, cap(500)), text);
    let out = never_cut(&text, cap(text.len() - 5));
    assert!(out.contains(&line), "{out}");
    let mode = format!("claude --dangerously-skip-permissions --resume {OURS}");
    let room = "memory critical: restart it, then resume with ".len() + mode.len() + 3;
    let out = never_cut(&text, cap(room));
    assert!(out.contains(&mode), "{out}");
    let room = "memory critical: restart it, then resume with ".len() + bare.len() + 3;
    let out = never_cut(&text, cap(room));
    assert!(
        out.contains(&bare) && !out.contains("--dangerously"),
        "{out}"
    );
    let cut = cap(room)(&text);
    assert!(
        cut.contains("resume with claude --") && !cut.contains(OURS),
        "{cut}"
    );
    let out = never_cut(&text, cap(40));
    assert!(
        !out.contains("--resume") && !out.contains("resume with"),
        "{out}"
    );
    assert!(
        out.starts_with("memory critical: restart it: Memory"),
        "{out}"
    );
    for plain in [
        "memory critical: restart it: Memory usage critical".to_string(),
        format!("x{THEN_RESUME_WITH}claude --resume not-an-id: y"),
    ] {
        assert_eq!(never_cut(&plain, cap(20)), cap(20)(&plain));
    }
}

/// Verifies that [`shorter`] and [`bare_of`] preserve any leading environment prefix produced by [`with_env`].
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
        let text = format!("Claude Code froze{THEN_RESUME_WITH}{homed}.");
        for cap in (20..text.len() + 10).step_by(7) {
            let out = never_cut(&text, |t| t.chars().take(cap).collect());
            assert!(
                !out.contains(" --resume ") || out.contains(head),
                "{cap}: {out}"
            );
        }
    }
    assert_eq!(with_env(&[], &line).as_deref(), Some(line.as_str()));
    assert_eq!(with_env(&[("home", "/o")], &line), None);
    assert_eq!(with_env(&[("HOME", "/o\nx")], &line), None);
}

/// Verifies that [`crate::harness::footer::read_pid`] prefixes the resume line with `HOME` or
/// `CLAUDE_CONFIG_DIR` when the session runs outside the user's default `.claude` directory.
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
    assert_eq!(
        read(Some(&other), None),
        Some(bare.clone()),
        "the user's own"
    );
    assert_eq!(read(None, None), Some(bare));
    assert!(home_env(Path::new("/u/me"), None, Some(Path::new("/u/me/.claude"))).is_empty());
}
