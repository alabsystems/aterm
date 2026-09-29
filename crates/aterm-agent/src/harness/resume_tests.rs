// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// This tab's conversation, and a SIBLING tab's in the same directory.
const OURS: &str = "5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
const SIBLING: &str = "0a0b0c0d-1111-4222-8333-444455556666";

/// A private `<claude dir>` with a `sessions/` directory, removed on drop.
struct ClaudeDir(PathBuf);

impl ClaudeDir {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "aterm-resume-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sessions")).expect("sessions dir");
        Self(root)
    }

    /// Claude Code's record for `pid`, in the shape 2.1.280 writes
    /// (`upgrade_tests.rs`' measured `SESSION_9162`), filed as `<file>.json`.
    fn record(&self, file: u32, pid: u32, session: &str, started: u64, status_at_ms: u64) {
        let text = format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/Users/_owner/aterm","startedAt":{},"procStart":"{}","version":"2.1.281","kind":"interactive","entrypoint":"cli","status":"idle","statusUpdatedAt":{status_at_ms}}}"#,
            started * 1000,
            crate::harness::footer::lstart_utc(started),
        );
        std::fs::write(self.0.join("sessions").join(format!("{file}.json")), text)
            .expect("write record");
    }
}

impl Drop for ClaudeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The resume line the WINDOW's footer read gives for `pid` in `dir`
/// ([`crate::harness::footer::read_pid`], the function `aterm-gui`'s
/// resolver calls): one read of the record, `argv` the process's own.
fn line_of(dir: &ClaudeDir, pid: u32, started: Option<u64>, argv: &[String]) -> Option<String> {
    use crate::harness::footer;
    let offset = |_: Option<&str>, _: i64| None;
    footer::read_pid(
        &dir.0,
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
    )
    .and_then(|facts| facts.resume)
}

/// THE BACKLOG'S CASE (2026-09-26): two live Claude Code processes filed
/// under ONE working directory, the sibling's conversation the NEWER — the
/// one `claude --continue` would resume. The command for each pid names that
/// pid's own conversation, with its launch flags carried; never the other's,
/// and never `--continue`. Read as the window reads it ([`line_of`]).
/// NEGATIVE CONTROLS: a pid with no record, a record whose kernel start
/// disagrees (a reused pid), a record filed under another pid's name, a
/// record of another shape, and — the review of 2026-09-28 — a record with
/// NO `procStart` where the kernel start is known all give no command, never
/// a guess.
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
    std::fs::write(dir.0.join("sessions/778.json"), r#"{"pid":778}"#).expect("write");
    assert_eq!(line_of(&dir, 778, None, &launch), None);
    // A record with no `procStart`: the footer shows its facts, but where
    // the kernel's start is known nothing ties the record to THIS process,
    // so no line to type. With no start known the pid alone decides, as
    // above.
    std::fs::write(
        dir.0.join("sessions/779.json"),
        format!(r#"{{"pid":779,"sessionId":"{OURS}","cwd":"/Users/_owner/aterm"}}"#),
    )
    .expect("write");
    let offset = |_: Option<&str>, _: i64| None;
    let facts = crate::harness::footer::read_pid(
        &dir.0,
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
    )
    .expect("the footer reads the record");
    assert_eq!(facts.resume, None, "no procStart, a known start: no line");
    assert_eq!(
        line_of(&dir, 779, None, &launch),
        Some(format!("claude --model opus --resume {OURS}"))
    );
}

/// The line carries what the relaunch carries ([`upgrade::rewrite_argv`]):
/// every known flag verbatim, quoted only where a shell would read it, the
/// resume family and `--effort` dropped, positionals dropped. A flag the
/// rewrite does not know leaves the bare line; a launch that is not
/// resumable in place (`--worktree`, `-p`) gets none; nor does an id that is
/// not one. A script launch reads its flags after the script.
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

/// A resume line a reason names is NEVER CUT by a renderer that cuts
/// (resume-hint review, 2026-09-26): [`never_cut`] keeps it whole, else in
/// its bare form, else drops its clause — and a text naming no line is
/// rendered untouched. The renderer here is a plain byte cap with `…`, the
/// shape of the supervisor's escalation.
///
/// NEGATIVE CONTROL: the same renderer given the text directly cuts the id
/// mid-way — the defect the ladder exists for.
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
    // Room for the bare form only.
    let room = "memory critical: restart it, then resume with ".len() + bare.len() + 3;
    let out = never_cut(&text, cap(room));
    assert!(
        out.contains(&bare) && !out.contains("--dangerously"),
        "{out}"
    );
    // THE NEGATIVE CONTROL: the renderer alone cuts the command — it is
    // named, and its conversation is not.
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
