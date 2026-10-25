// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::host_path::{abs, json};
#[cfg(unix)]
use std::io::Write as _;

type RegistryHook = Box<dyn FnMut(&Path)>;

thread_local! {
    static REGISTRY_READ: std::cell::RefCell<Option<RegistryHook>> =
        const { std::cell::RefCell::new(None) };
}

/// The test seam [`session_of_pid`] calls before it reads.
pub(super) fn before_registry_read(path: &Path) {
    REGISTRY_READ.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

thread_local! {
    static TRANSCRIPT_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The test seam [`transcript_path`] calls on every lookup.
pub(super) fn on_transcript_lookup() {
    TRANSCRIPT_LOOKUPS.with(|n| n.set(n.get() + 1));
}

/// One footer read of `pid` as the window's resolver makes it
/// ([`read_pid`]), over a per-process `tail` kept across reads and the
/// tab's `fold`, with no launch facts or argv.
#[cfg(unix)]
fn folded(
    claude: &Path,
    pid: u32,
    started: Option<u64>,
    tail: &mut TailCache,
    fold: &mut FooterCache,
    now: i64,
    offset_at: &dyn Fn(Option<&str>, i64) -> Option<i64>,
) -> Option<FooterFacts> {
    let clock = ReadClock {
        read_at: SystemTime::now(),
        now,
        offset_at,
    };
    read_pid(
        claude,
        pid,
        started,
        |_| (LaunchFacts::default(), Vec::new()),
        tail,
        fold,
        &clock,
        None,
        None,
    )
}

/// One footer read ([`folded`]) while the registry file is rewritten: the
/// first read finds it whole, later ones cut mid-write; it is whole again
/// afterwards.
#[cfg(unix)]
fn read_torn_after_first(
    claude: &Path,
    pid: u32,
    started: Option<u64>,
    tail: &mut TailCache,
    cache: &mut FooterCache,
    now: i64,
    offset_at: &dyn Fn(Option<&str>, i64) -> Option<i64>,
) -> (Option<FooterFacts>, usize) {
    let file = claude.join(format!("sessions/{pid}.json"));
    let whole = std::fs::read_to_string(&file).unwrap();
    let reads = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let (seen, torn, half) = (
        std::rc::Rc::clone(&reads),
        file.clone(),
        whole[..whole.len() / 2].to_owned(),
    );
    REGISTRY_READ.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |path: &Path| {
            if path == torn {
                seen.set(seen.get() + 1);
                if seen.get() > 1 {
                    std::fs::write(&torn, &half).unwrap();
                }
            }
        }));
    });
    let facts = folded(claude, pid, started, tail, cache, now, offset_at);
    REGISTRY_READ.with(|hook| *hook.borrow_mut() = None);
    std::fs::write(&file, &whole).unwrap();
    (facts, reads.get())
}

#[cfg(unix)]
fn cache_test_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "aterm-footer-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[cfg(unix)]
fn assistant_row(model: &str, timestamp: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{timestamp}","effort":"high","message":{{"model":"{model}"}}}}"#
    )
}

#[cfg(unix)]
#[test]
fn tail_cache_reuses_only_one_process_and_one_unchanged_file() {
    let root = cache_test_dir("tail-cache");
    let path = root.join("session.jsonl");
    let before = "2026-09-27T11:00:00Z";
    let during = "2026-09-27T13:00:00Z";
    let after = "2026-09-27T15:00:00Z";
    let start = crate::harness::upgrade_models::parse_utc("2026-09-27T12:00:00Z").unwrap();
    let old = format!("{}\n", assistant_row("claude-opus-5", before));
    std::fs::write(&path, &old).unwrap();
    let mut cache = TailCache::default();
    let read =
        |cache: &mut TailCache, started, since| cache.read(&path, 4242, started, since).unwrap();

    assert_eq!(read(&mut cache, Some(start), start), TailFacts::default());
    assert_eq!(read(&mut cache, Some(start), start), TailFacts::default());
    assert_eq!(cache.reads, 1, "unchanged tail is read once");

    let newer = format!("{}\n", assistant_row("claude-opus-6", during));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(newer.as_bytes())
        .unwrap();
    assert_eq!(
        read(&mut cache, Some(start), start).model.shown(),
        Some("Opus 6")
    );
    assert_eq!(cache.reads, 2, "append invalidates the tail");

    let whole = format!("{old}{}\n", assistant_row("claude-opus-7", during));
    assert_eq!(whole.len(), old.len() + newer.len());
    let old_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, &whole).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old_modified))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        old_modified
    );
    assert_eq!(
        read(&mut cache, Some(start), start).model.shown(),
        Some("Opus 7")
    );
    assert_eq!(
        cache.reads, 3,
        "same-size, same-mtime rewrite invalidates the tail"
    );

    let replacement = root.join("replacement.jsonl");
    std::fs::write(
        &replacement,
        format!("{old}{}\n", assistant_row("claude-opus-8", during)),
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old_modified))
        .unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(
        read(&mut cache, Some(start), start).model.shown(),
        Some("Opus 8")
    );
    assert_eq!(
        cache.reads, 4,
        "same-size, same-mtime replacement invalidates"
    );

    let new_start = crate::harness::upgrade_models::parse_utc("2026-09-27T14:00:00Z").unwrap();
    assert_eq!(
        read(&mut cache, Some(new_start), new_start),
        TailFacts::default()
    );
    assert_eq!(cache.reads, 5, "process start changes the key");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("{}\n", assistant_row("claude-opus-9", after)).as_bytes())
        .unwrap();
    assert_eq!(
        read(&mut cache, Some(new_start), new_start).model.shown(),
        Some("Opus 9")
    );
    assert_eq!(cache.reads, 6);
    assert_eq!(cache.appends, 2, "both appends were read as appends");

    std::fs::remove_file(&path).unwrap();
    assert!(
        cache
            .read(&path, 4242, Some(new_start), new_start)
            .is_none()
    );
    assert!(cache.entry.is_none(), "failed opens must never be cached");
    std::fs::write(
        &path,
        format!("{}\n", assistant_row("claude-opus-8", after)),
    )
    .unwrap();
    assert_eq!(
        read(&mut cache, Some(new_start), new_start).model.shown(),
        Some("Opus 8")
    );
    assert_eq!(cache.reads, 7, "recovered file is read anew");
    assert_eq!(cache.appends, 2, "a recreated file is no append");
    std::fs::remove_dir_all(root).unwrap();
}

/// The path and the branch follow the Bash tool: the newest main-thread
/// `cwd` since the process started, read from the same tail as the model
/// (an append included).
#[cfg(unix)]
#[test]
fn the_path_and_branch_follow_the_bash_tools_directory() {
    let root = cache_test_dir("bash-cwd");
    let cwd = root.join("work");
    let wt = root.join("wt");
    let elsewhere = root.join("elsewhere");
    for (dir, branch) in [(&cwd, "main"), (&wt, "feature"), (&elsewhere, "old")] {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
    }
    let claude = root.join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let project = claude.join("projects").join(project_slug(&cwd));
    std::fs::create_dir_all(&project).unwrap();
    let started = crate::harness::upgrade_models::parse_utc("2026-09-27T12:00:00Z").unwrap();
    std::fs::write(
        claude.join("sessions/4242.json"),
        format!(
            r#"{{"pid":4242,"sessionId":"cwd-demo","cwd":"{}","procStart":"{}"}}"#,
            cwd.display(),
            lstart_utc(started)
        ),
    )
    .unwrap();
    let stamped = |dir: &Path, at: &str, sidechain: bool| {
        format!(
            r#"{{"type":"user","isSidechain":{sidechain},"cwd":"{}","timestamp":"{at}"}}"#,
            dir.display()
        )
    };
    let transcript = project.join("cwd-demo.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n{}\n",
            stamped(&elsewhere, "2026-09-27T11:00:00Z", false),
            assistant_row("claude-opus-5", "2026-09-27T13:00:00Z")
        ),
    )
    .unwrap();
    let entry = session_of_pid(&claude, 4242, Some(started)).unwrap();
    let read = |cache: &mut TailCache| {
        facts_for_entry_at(
            &claude,
            4242,
            Some(started),
            &entry,
            &LaunchFacts::default(),
            cache,
            SystemTime::now(),
        )
    };
    let home = aterm_types::dirs::home_dir();
    let shown = |dir: &Path| home_path(dir, home.as_deref());
    let mut cache = TailCache::default();
    let first = read(&mut cache);
    assert_eq!(first.path, shown(&cwd), "a predecessor's cwd is not ours");
    assert_eq!(first.branch.as_deref(), Some("main"));
    std::fs::write(cwd.join(".git/HEAD"), "ref: refs/heads/topic\n").unwrap();
    assert_eq!(read(&mut cache).branch.as_deref(), Some("topic"));
    assert_eq!(cache.reads, 1, "git re-read while the transcript is not");

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    std::io::Write::write_all(
        &mut file,
        format!(
            "{}\n{}\n",
            stamped(&wt, "2026-09-27T13:01:00Z", false),
            stamped(&elsewhere, "2026-09-27T13:02:00Z", true)
        )
        .as_bytes(),
    )
    .unwrap();
    let moved = read(&mut cache);
    assert_eq!(cache.appends, 1, "read from the append");
    assert_eq!(
        moved.path,
        shown(&wt),
        "a subagent's cwd is not the session's"
    );
    assert_eq!(moved.branch.as_deref(), Some("feature"));
    assert_eq!(moved.model, first.model);
    assert_eq!(
        read(&mut TailCache::default()).path,
        shown(&wt),
        "a fresh read agrees"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn model_ids_read_as_the_vendor_spells_them() {
    assert_eq!(model_display("claude-opus-5-5"), "Opus 5.5");
    assert_eq!(model_display("claude-opus-5"), "Opus 5");
    assert_eq!(model_display("claude-fable-5-1"), "Fable 5.1");
    assert_eq!(model_display("claude-sonnet-5"), "Sonnet 5");
    assert_eq!(model_display("claude-haiku-4-5-20251001"), "Haiku 4.5");
    assert_eq!(model_display("claude-opus-5-5[1m]"), "Opus 5.5");
    assert_eq!(model_display("gpt-5-codex"), "gpt-5-codex");
}

#[test]
fn a_registry_entry_must_name_the_pid_it_is_filed_under() {
    let text =
        r#"{"pid":7214,"sessionId":"28b6a7bd-7021","cwd":"/Users//ana/aterm","status":"busy"}"#;
    assert_eq!(
        parse_session_entry(text, 7214, None),
        Some(SessionEntry {
            session_id: "28b6a7bd-7021".into(),
            cwd: PathBuf::from("/Users//ana/aterm"),
            version: None,
            proc_start: None,
            started_at: None,
        })
    );
    assert_eq!(
        parse_session_entry(text, 9999, None),
        None,
        "a stale row left by a reused pid is not this process's"
    );
    let hostile = r#"{"pid":7,"sessionId":"../../etc/passwd","cwd":"/"}"#;
    assert_eq!(
        parse_session_entry(hostile, 7, None),
        None,
        "the id becomes a file name"
    );
}

#[test]
fn the_slug_matches_claude_codes_project_directories() {
    assert_eq!(project_slug(Path::new("/home/dev/proj")), "-home-dev-proj");
    assert_eq!(project_slug(Path::new("/tmp/a.b c")), "-tmp-a-b-c");
    assert_eq!(
        project_slug(Path::new(
            "/home/me/repo/.claude/worktrees/kitty-motion-wave"
        )),
        "-home-me-repo--claude-worktrees-kitty-motion-wave"
    );
}

#[test]
fn the_tail_takes_the_newest_main_thread_model_and_effort() {
    let body = concat!(
        r#"{"type":"assistant","effort":"high","message":{"model":"claude-opus-5"}}"#,
        "\n",
        r#"{"type":"assistant","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"effort":"low","message":{"model":"claude-haiku-4-5"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#,
        "\n",
        r#"{"type":"user","effort":"xhigh"}"#,
        "\n",
    );
    let facts = tail_facts(body.as_bytes(), None);
    assert_eq!(facts.model.shown(), Some("Opus 5.5"));
    assert_eq!(facts.effort.shown(), Some("xhigh"));
    assert_eq!(
        tail_facts(b"", None),
        TailFacts::default(),
        "a fresh session knows neither"
    );
}

#[test]
fn an_unreadable_model_choice_voids_model_and_effort() {
    let answer = r#"{"type":"assistant","timestamp":"2026-09-24T04:13:30.000Z","effort":"xhigh","message":{"model":"claude-opus-5"}}"#;
    let command = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}"#;
    let unread = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<local-command-stdout>Model switch to X was blocked by a PreModelSwitch hook</local-command-stdout>"}}"#;
    let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
    let void = TailFacts {
        model: Said::Unread,
        effort: Said::Unread,
        ..TailFacts::default()
    };
    assert_eq!(
        tail_facts(format!("{answer}\n{command}\n").as_bytes(), start),
        void
    );
    assert_eq!(
        tail_facts(format!("{answer}\n{command}\n{unread}\n").as_bytes(), start),
        void,
        "a result this build does not read is no model"
    );
    assert_eq!(
        tail_facts(format!("{answer}\n").as_bytes(), start)
            .model
            .shown(),
        Some("Opus 5")
    );
    for quoted in [
        r#"{"type":"assistant","timestamp":"2026-09-24T04:13:31.000Z","message":{"model":"<synthetic>","content":"<command-name>/model</command-name>"}}"#,
        r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":[{"type":"tool_result","content":"<command-name>/model</command-name>"}]}}"#,
        r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"why does <command-name>/model</command-name> exist"}}"#,
        r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"it printed <local-command-stdout>Set model to `Haiku 4.5`</local-command-stdout>"}}"#,
        r#"{"type":"assistant","timestamp":"2026-09-24T04:13:31.000Z","message":{"model":"<synthetic>","content":"<local-command-stdout>Set model to `Haiku 4.5`</local-command-stdout>"}}"#,
    ] {
        assert_eq!(
            tail_facts(format!("{answer}\n{quoted}\n").as_bytes(), start)
                .model
                .shown(),
            Some("Opus 5"),
            "{quoted}"
        );
    }
}

#[test]
fn an_unstamped_row_is_not_attributed() {
    let unstamped =
        r#"{"type":"assistant","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
    let stamped = r#"{"type":"assistant","timestamp":"2026-09-24T04:15:32.000Z","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
    let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
    assert_eq!(
        tail_facts(format!("{unstamped}\n").as_bytes(), start),
        TailFacts::default()
    );
    assert_eq!(
        tail_facts(format!("{stamped}\n").as_bytes(), start)
            .model
            .shown(),
        Some("Opus 5.5")
    );
}

#[test]
fn a_model_result_display_is_the_vendors_name() {
    let result = |text: &str| {
        let row = format!(
            r#"{{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{{"content":{}}}}}"#,
            aterm_json::to_string(&Value::String(format!(
                "<local-command-stdout>{text}</local-command-stdout>"
            )))
            .unwrap()
        );
        tail_facts(format!("{row}\n").as_bytes(), None)
    };
    for (text, model) in [
        (
            "Set model to `Opus 5.5 (default)` and saved as your default for new sessions",
            "Opus 5.5",
        ),
        (
            "Set model to `Opus 5 (1M context) (default)` for this session only",
            "Opus 5",
        ),
        ("Set model to `Fable 5.1`", "Fable 5.1"),
        ("Set model to `Fable 5.1 (1M context)`", "Fable 5.1"),
        (
            "Set model to \u{1b}[1mclaude-fable-5-1\u{1b}[22m and saved as your default",
            "Fable 5.1",
        ),
        (
            "\u{21AF} Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok",
            "Opus 5.5",
        ),
        (FAST_ON_COLOURED, "Opus 5.5"),
    ] {
        assert_eq!(result(text).model.shown(), Some(model), "{text:?}");
    }
    let with_effort = result(&format!(
        "Set model to `Opus 5.5` and saved as your default for new sessions with `{CLAUDE_TOP_EFFORT_KEY}` effort ({CLAUDE_TOP_EFFORT_KEY} applies to this session only)",
    ));
    assert_eq!(
        (with_effort.model.shown(), with_effort.effort.shown()),
        (Some("Opus 5.5"), Some("xhigh"))
    );
    for text in [
        "Set model to `Opus\u{7}5`",
        "Set model to `Opus\n5`",
        "Set model to `A very long name that no model of anyone's has ever carried at all`",
        "Set model to ``Opus``",
        "Set model to Opus 5.5",
    ] {
        assert_eq!(result(text).model, Said::Unread, "{text:?}");
    }
    for text in [
        "Cloud session couldn't switch to `Opus 5.5`",
        "Model switch to X was blocked by a PreModelSwitch hook",
        "Current model: `Opus 5.5`",
        "Kept Fast mode ON \u{00B7} model set to `Opus 5.5`",
        "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Kept Fast mode ON \u{00B7} model set to `Opus 5.5`",
        "Fast mode ON",
        "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} $30/$150 per Mtok",
        "Fast mode OFF",
    ] {
        assert_eq!(result(text).model, Said::Unsaid, "{text:?}");
    }
}

const FAST_ON_COLOURED: &str = "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok";

#[test]
fn a_kept_model_leaves_the_facts_as_they_were() {
    const ANSWER: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000030","isSidechain":false,"type":"assistant","effort":"xhigh","message":{"model":"claude-fable-5-1","role":"assistant","content":[{"type":"text","text":"Done."}]},"uuid":"00000000-0000-4000-8000-000000000031","timestamp":"2026-09-22T17:05:50.000Z","userType":"external","entrypoint":"cli","cwd":"/Users//person/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
    const KEPT_COMMAND: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000031","isSidechain":false,"type":"system","subtype":"local_command","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>","level":"info","timestamp":"2026-09-22T17:06:02.248Z","uuid":"00000000-0000-4000-8000-000000000032","isMeta":false,"userType":"external","entrypoint":"cli","cwd":"/Users//person/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
    const KEPT_RESULT: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000032","isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout>Kept model as `Fable 5.1`</local-command-stdout>","level":"info","timestamp":"2026-09-22T17:06:02.248Z","uuid":"00000000-0000-4000-8000-000000000033","isMeta":false,"commandRun":{"command":"model","args":""},"userType":"external","entrypoint":"cli","cwd":"/Users//person/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
    let start = crate::harness::upgrade_models::parse_utc("2026-09-22T17:00:00Z");
    let before = tail_facts(format!("{ANSWER}\n").as_bytes(), start);
    let kept = tail_facts(
        format!("{ANSWER}\n{KEPT_COMMAND}\n{KEPT_RESULT}\n").as_bytes(),
        start,
    );
    assert_eq!(kept, before, "a kept model changes nothing");
    assert_eq!(
        (kept.model.shown(), kept.effort.shown()),
        (Some("Fable 5.1"), Some("xhigh"))
    );
    let choice = KEPT_COMMAND.replace(
        r#""type":"system","subtype":"local_command","content":"#,
        r#""type":"user","message":{"role":"user","content":"#,
    );
    let choice = choice.replacen(r#"","level""#, r#""},"level""#, 1);
    let voided = tail_facts(format!("{ANSWER}\n{choice}\n").as_bytes(), start);
    assert_eq!(
        (voided.model, voided.effort),
        (Said::Unread, Said::Unread),
        "{choice}"
    );
}

fn answer_at(model: &str, at: &str, effort: Option<&str>) -> String {
    let effort = effort.map_or(String::new(), |e| format!(r#","effort":"{e}""#));
    format!(r#"{{"type":"assistant","timestamp":"{at}"{effort},"message":{{"model":"{model}"}}}}"#)
}

fn command_at(name: &str, at: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"<command-name>/{name}</command-name>\n            <command-message>{name}</command-message>\n            <command-args></command-args>"}}}}"#
    )
}

fn result_at(at: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":{}}}}}"#,
        aterm_json::to_string(&Value::String(format!(
            "<local-command-stdout>{text}</local-command-stdout>"
        )))
        .unwrap()
    )
}

fn body(rows: &[&str]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

#[test]
fn a_model_command_row_pins_whatever_its_result_says() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-footer-chose-")
        .tempdir()
        .unwrap();
    let path = dir.path().join("session.jsonl");
    let floor = crate::harness::upgrade_models::parse_utc("2026-09-24T04:00:00Z").unwrap();
    let (old, at) = ("2026-09-24T03:00:00.000Z", "2026-09-24T04:13:31.000Z");
    let chose = |rows: &[&str]| {
        std::fs::write(&path, body(rows)).unwrap();
        chose_model_since(&path, floor)
    };
    let command = command_at("model", at);
    let kept = result_at(at, "Kept model as `Fable 5.1`");
    let blocked = result_at(at, "Model switch to X was blocked by a PreModelSwitch hook");
    let set = result_at(at, "Set model to `Fable 5.1` for this session only");
    let fast = result_at(
        at,
        "\u{21AF} Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok",
    );
    assert!(chose(&[&command]), "the command row alone");
    assert!(chose(&[&command, &kept]), "a model kept");
    assert!(
        chose(&[&command, &blocked]),
        "a result this build cannot read"
    );
    assert!(chose(&[&set]), "a `Set model` result");
    assert!(chose(&[&fast]), "a `/fast on` that moved the model");

    let answer = answer_at("claude-opus-5", at, None);
    let quoted = format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"content":"why does <command-name>/model</command-name> exist"}}}}"#
    );
    assert!(!chose(&[&kept]), "a kept model, no command: nothing moved");
    assert!(!chose(&[&answer]), "an answer");
    assert!(!chose(&[&command_at("effort", at)]), "another command");
    assert!(!chose(&[&command_at("model", old)]), "before the floor");
    assert!(
        !chose(&[&command_at("model", old), &answer]),
        "before the floor, under a row"
    );
    assert!(!chose(&[&quoted]), "the tag quoted in text");
}

#[test]
fn a_switch_that_names_no_effort_keeps_it_only_for_the_same_model() {
    let start = crate::harness::upgrade_models::parse_utc("2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    let switch = |at: &str, to: &str| {
        [
            command_at("model", at),
            result_at(at, &format!("Set model to `{to}` for this session only")),
        ]
    };
    let effort_high = |at: &str| {
        [
            command_at("effort", at),
            result_at(
                at,
                "Set effort level to high (this session only): Deeper reasoning",
            ),
        ]
    };
    let facts = |rows: &[&str]| {
        let facts = tail_facts(body(rows).as_bytes(), start);
        (facts.model.shown().map(str::to_owned), facts.effort)
    };
    let is = |model: &str, effort: Option<&str>| {
        (
            Some(model.to_owned()),
            effort.map_or(Said::Unread, |e| Said::Is(e.to_owned())),
        )
    };
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Haiku 4.5");
    let haiku = answer_at(
        "claude-haiku-4-5-20251001",
        "2026-09-28T15:03:00.000Z",
        None,
    );
    assert_eq!(facts(&[&opus, &c, &r]), is("Haiku 4.5", None));
    assert_eq!(
        facts(&[&opus, &c, &r, &haiku]),
        is("Haiku 4.5", None),
        "never Haiku 4.5 xhigh, after any number of Haiku answers"
    );
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Opus 5.5 (default)");
    assert_eq!(facts(&[&opus, &c, &r]), is("Opus 5.5", Some("xhigh")));
    let [c1, r1] = switch("2026-09-28T15:02:00.000Z", "Sonnet 5");
    let [c2, r2] = switch("2026-09-28T15:02:30.000Z", "Opus 5.5");
    assert_eq!(facts(&[&opus, &c1, &r1, &c2, &r2]), is("Opus 5.5", None));
    let [e1, e2] = effort_high("2026-09-28T15:02:10.000Z");
    assert_eq!(
        facts(&[&opus, &c1, &r1, &e1, &e2]),
        is("Sonnet 5", Some("high"))
    );
    let [e1, e2] = effort_high("2026-09-28T15:01:30.000Z");
    assert_eq!(facts(&[&opus, &e1, &e2, &c1, &r1]), is("Sonnet 5", None));
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Opus 5.5");
    assert_eq!(
        facts(&[&opus, &e1, &e2, &c, &r]),
        is("Opus 5.5", Some("high"))
    );
    let clause = result_at(
        "2026-09-28T15:02:00.000Z",
        "Set model to `Sonnet 5` for this session only with `high` effort",
    );
    let c = command_at("model", "2026-09-28T15:02:00.000Z");
    assert_eq!(facts(&[&opus, &c, &clause]), is("Sonnet 5", Some("high")));
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Sonnet 5");
    let alone = tail_facts(body(&[&c, &r]).as_bytes(), start);
    assert_eq!(
        (alone.effort, alone.effort_for.as_deref()),
        (Said::Unsaid, Some("Sonnet 5"))
    );
}

#[test]
fn a_command_pairs_only_with_the_result_directly_under_it() {
    let start = crate::harness::upgrade_models::parse_utc("2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    let model_cmd = command_at("model", "2026-09-28T15:02:00.000Z");
    let unread = result_at("2026-09-28T15:02:00.000Z", "Set model to Fable 5.1");
    let fast_cmd = command_at("fast", "2026-09-28T15:02:30.000Z");
    let fast = result_at("2026-09-28T15:02:30.000Z", FAST_ON_COLOURED);
    let facts = |rows: &[&str]| {
        let facts = tail_facts(body(rows).as_bytes(), start);
        (facts.model, facts.effort)
    };
    assert_eq!(
        facts(&[&opus, &model_cmd, &unread]),
        (Said::Unread, Said::Unread)
    );
    assert_eq!(
        facts(&[&opus, &model_cmd, &unread, &fast_cmd, &fast]),
        (Said::Is("Opus 5.5".into()), Said::Unread)
    );
    assert_eq!(
        facts(&[&opus, &model_cmd, &fast_cmd, &fast]),
        (Said::Is("Opus 5.5".into()), Said::Unread)
    );
    assert_eq!(
        facts(&[&opus, &fast_cmd, &fast]),
        (Said::Is("Opus 5.5".into()), Said::Is("xhigh".into()))
    );
    let fable = answer_at("claude-fable-5-1", "2026-09-28T15:01:00.000Z", Some("high"));
    assert_eq!(
        facts(&[&fable, &fast_cmd, &fast]),
        (Said::Is("Opus 5.5".into()), Said::Unread)
    );
    let off = result_at("2026-09-28T15:02:30.000Z", "Fast mode OFF");
    assert_eq!(
        facts(&[&fable, &fast_cmd, &off]),
        (Said::Is("Fable 5.1".into()), Said::Is("high".into()))
    );
}

#[test]
fn the_floor_second_is_the_processs_and_the_first_row_before_it_ends_the_scan() {
    let floor = crate::harness::upgrade_models::parse_utc("2026-09-28T15:00:00Z");
    let at_floor = answer_at("claude-opus-5-5", "2026-09-28T15:00:00.900Z", Some("high"));
    let before = answer_at("claude-fable-5-1", "2026-09-28T14:59:59.900Z", None);
    let above = answer_at("claude-sonnet-5", "2026-09-28T15:10:00.000Z", Some("low"));
    let read = |rows: &[&str]| {
        let (facts, floored) = scan(&rows_of(body(rows).as_bytes()).collect::<Vec<_>>(), floor);
        (facts.model, facts.effort, floored)
    };
    assert_eq!(
        read(&[&at_floor]),
        (Said::Is("Opus 5.5".into()), Said::Is("high".into()), false)
    );
    assert_eq!(read(&[&above, &before]), (Said::Unsaid, Said::Unsaid, true));
}

/// One Claude Code process on disk for the resolver's tests: registry
/// file, transcripts, and the read clock (its start until [`Self::at`]).
#[cfg(unix)]
struct Proc {
    _root: aterm_tempfile::TempDir,
    claude: PathBuf,
    project: PathBuf,
    cwd: PathBuf,
    start: u64,
    clock: std::cell::Cell<u64>,
}

#[cfg(unix)]
impl Proc {
    fn new(tag: &str, start: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-footer-{tag}-"))
            .tempdir()
            .unwrap();
        let cwd = root.path().join("work");
        let claude = root.path().join("claude");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        let start = crate::harness::upgrade_models::parse_utc(start).unwrap();
        Self {
            _root: root,
            claude,
            project,
            cwd,
            start,
            clock: std::cell::Cell::new(start),
        }
    }

    fn at(&self, stamp: &str) {
        self.clock
            .set(crate::harness::upgrade_models::parse_utc(stamp).unwrap());
    }

    fn register(&self, session: &str) {
        std::fs::write(
            self.claude.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{session}","cwd":"{}","procStart":"{}","startedAt":{},"version":"2.1.283"}}"#,
                self.cwd.display(),
                lstart_utc(self.start),
                self.start * 1000 + 684
            ),
        )
        .unwrap();
    }

    fn write(&self, session: &str, rows: &[&str]) {
        std::fs::write(self.project.join(format!("{session}.jsonl")), body(rows)).unwrap();
    }

    fn read(&self, launch: &LaunchFacts, cache: &mut TailCache) -> FooterFacts {
        let entry = session_of_pid(&self.claude, 4242, Some(self.start)).expect("registry");
        facts_for_entry_at(
            &self.claude,
            4242,
            Some(self.start),
            &entry,
            launch,
            cache,
            UNIX_EPOCH + std::time::Duration::from_secs(self.clock.get()),
        )
    }
}

/// Launched with `--model` Haiku 4.5 and switched to Sonnet 5 with no
/// effort named: the footer shows no effort until one is named for Sonnet
/// 5.
#[cfg(unix)]
#[test]
fn a_switch_from_the_launch_model_names_no_effort_until_one_is_read() {
    const S: &str = "00000000-0000-4000-8000-000000000051";
    let proc = Proc::new("launch-switch", "2026-09-28T15:00:00Z");
    proc.register(S);
    let argv = |model: &str| launch_facts(&["claude".into(), "--model".into(), model.to_owned()]);
    let haiku = argv("claude-haiku-4-5-20251001");
    let cmd = command_at("model", "2026-09-28T15:01:00.000Z");
    let sonnet = result_at(
        "2026-09-28T15:01:00.000Z",
        "Set model to `Sonnet 5` for this session only",
    );
    proc.write(S, &[&cmd, &sonnet]);
    let facts = proc.read(&haiku, &mut TailCache::default());
    assert_eq!(
        (facts.model.as_deref(), facts.effort.as_deref()),
        (Some("Sonnet 5"), None)
    );
    assert!(
        !facts.effort_open,
        "Haiku 4.5's launch card may not fill it"
    );
    let open = proc.read(&LaunchFacts::default(), &mut TailCache::default());
    assert!(open.effort_open);
    assert_eq!(open.effort_for.as_deref(), Some("Sonnet 5"));
    let sonnet_flag = argv("claude-sonnet-5");
    let same = proc.read(&sonnet_flag, &mut TailCache::default());
    assert!(same.effort_open && same.effort.is_none());
    let before = [
        command_at("effort", "2026-09-28T15:00:30.000Z"),
        result_at(
            "2026-09-28T15:00:30.000Z",
            "Set effort level to low (this session only): Quick",
        ),
    ];
    proc.write(S, &[&before[0], &before[1], &cmd, &sonnet]);
    assert_eq!(
        proc.read(&sonnet_flag, &mut TailCache::default())
            .effort
            .as_deref(),
        Some("low")
    );
    assert_eq!(
        proc.read(&haiku, &mut TailCache::default()).effort,
        None,
        "set on Haiku 4.5, not Sonnet 5's"
    );
}

/// An in-REPL `/resume` into an older conversation restores its model
/// unless the launch pinned it (an alias, an environment pin); unpinned, the
/// effort stands only for that model.
#[cfg(unix)]
#[test]
fn a_resume_carries_the_model_only_where_the_process_pinned_it() {
    const A: &str = "00000000-0000-4000-8000-0000000000a1";
    const B: &str = "00000000-0000-4000-8000-0000000000b1";
    const R: &str = "00000000-0000-4000-8000-0000000000c1";
    let proc = Proc::new("resume", "2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    let old = answer_at("claude-fable-5-1", "2026-09-27T10:00:00.000Z", Some("high"));
    let shown = |f: &FooterFacts| (f.model.clone(), f.effort.clone());
    let is = |m: &str, e: Option<&str>| (Some(m.to_owned()), e.map(str::to_owned));
    let run = |first: &[&str], launch: &LaunchFacts, into: &[&str]| {
        proc.register(A);
        proc.write(A, first);
        proc.write(R, into);
        let mut cache = TailCache::default();
        let before = proc.read(launch, &mut cache);
        proc.register(R);
        (before, proc.read(launch, &mut cache), cache)
    };
    let (before, after, _) = run(&[&opus], &LaunchFacts::default(), &[&old]);
    assert_eq!(shown(&before), is("Opus 5.5", Some("xhigh")));
    assert_eq!(
        shown(&after),
        is("Fable 5.1", None),
        "never the pre-resume model"
    );
    assert!(!after.model_open && !after.effort_open);
    let fable = answer_at("claude-fable-5-1", "2026-09-28T15:01:00.000Z", Some("low"));
    let (_, after, _) = run(&[&fable], &LaunchFacts::default(), &[&old]);
    assert_eq!(shown(&after), is("Fable 5.1", Some("low")));
    let odd = answer_at("claude opus", "2026-09-27T10:00:03.000Z", Some("high"));
    let (_, after, _) = run(&[&opus], &LaunchFacts::default(), &[&old, &odd]);
    assert_eq!(shown(&after), (None, None));
    let chose_there = [
        command_at("model", "2026-09-27T10:00:00.000Z"),
        result_at(
            "2026-09-27T10:00:00.000Z",
            "Set model to `Fable 5.1` for this session only",
        ),
    ];
    let (_, after, mut cache) = run(
        &[&opus],
        &LaunchFacts::default(),
        &[&chose_there[0], &chose_there[1]],
    );
    assert_eq!(shown(&after), is("Opus 5.5", Some("xhigh")));
    let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
    proc.write(B, &[&sonnet_old]);
    proc.register(B);
    assert_eq!(
        proc.read(&LaunchFacts::default(), &mut cache)
            .model
            .as_deref(),
        Some("Sonnet 5"),
        "no answer, no restore, no pin"
    );
    let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    for launch in [
        launch_facts(&argv(&["claude", "--model", "opus"])),
        launch_facts_of(
            &argv(&["claude"]),
            &argv(&["ANTHROPIC_MODEL=claude-opus-5-5"]),
        ),
    ] {
        let (_, after, _) = run(&[&opus], &launch, &[&old]);
        assert_eq!(shown(&after), is("Opus 5.5", Some("xhigh")), "{launch:?}");
    }
}

/// A launch resume restores and pins from the start: the restored model is
/// named at once, its effort open for its card alone; `--continue` pins as
/// `--resume` does; an untied first sight shows nothing but keeps the pin.
#[cfg(unix)]
#[test]
fn a_launch_resume_pins_the_model_from_the_start() {
    const X: &str = "00000000-0000-4000-8000-0000000000e1";
    const Y: &str = "00000000-0000-4000-8000-0000000000e2";
    const Z: &str = "00000000-0000-4000-8000-0000000000e3";
    let proc = Proc::new("launch-resume", "2026-09-28T15:00:00Z");
    let fable_old = answer_at("claude-fable-5-1", "2026-09-27T10:00:00.000Z", Some("high"));
    let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
    let prompt_old = r#"{"type":"user","timestamp":"2026-09-27T09:59:00.000Z","message":{"role":"user","content":"hi"}}"#;
    proc.write(X, &[prompt_old, &fable_old]);
    proc.write(Y, &[&sonnet_old]);
    proc.write(Z, &[prompt_old, &fable_old]);
    let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    let model = |f: &FooterFacts| f.model.clone();
    let fable = Some("Fable 5.1".to_owned());
    let resumed = launch_facts(&argv(&["claude", "--resume", X]));
    proc.register(X);
    let first = proc.read(&resumed, &mut TailCache::default());
    assert_eq!(model(&first), fable, "the restored model, at once");
    assert!(!first.model_open);
    assert!(
        first.effort.is_none() && first.effort_open,
        "the effort open for Fable 5.1's card alone"
    );
    assert_eq!(first.effort_for, fable);
    let continued = launch_facts(&argv(&["claude", "--continue"]));
    let mut cache = TailCache::default();
    assert_eq!(model(&proc.read(&continued, &mut cache)), fable);
    proc.register(Y);
    assert_eq!(model(&proc.read(&continued, &mut cache)), fable, "pinned");
    for launch in [LaunchFacts::default(), resumed.clone()] {
        proc.register(Z);
        let mut cache = TailCache::default();
        assert_eq!(model(&proc.read(&launch, &mut cache)), None, "{launch:?}");
        proc.register(Y);
        assert_eq!(model(&proc.read(&launch, &mut cache)), None, "{launch:?}");
    }
    let env = launch_facts_of(
        &argv(&["claude", "--resume", X]),
        &argv(&["ANTHROPIC_MODEL=claude-opus-5-5"]),
    );
    proc.register(X);
    let mut cache = TailCache::default();
    assert_eq!(model(&proc.read(&env, &mut cache)), None);
    proc.register(Y);
    assert_eq!(model(&proc.read(&env, &mut cache)), None);
    proc.write(X, &[prompt_old]);
    proc.register(X);
    let mut cache = TailCache::default();
    assert_eq!(model(&proc.read(&resumed, &mut cache)), None);
    proc.register(Y);
    assert_eq!(
        model(&proc.read(&resumed, &mut cache)).as_deref(),
        Some("Sonnet 5")
    );
}

/// A `/resume` is told from a `/clear` by when the switch came: a
/// conversation whose own first row predates the last sighting existed
/// before the switch (even one another tab began after this process
/// started), so its model is restored — its effort not the other tab's —
/// and pinned; a resume back into the process's own session pins too.
#[cfg(unix)]
#[test]
fn a_resume_into_a_conversation_begun_since_the_start_pins_it() {
    const S: &str = "00000000-0000-4000-8000-0000000001a1";
    const D: &str = "00000000-0000-4000-8000-0000000001a2";
    const B: &str = "00000000-0000-4000-8000-0000000001a3";
    const C: &str = "00000000-0000-4000-8000-0000000001a4";
    let proc = Proc::new("late-resume", "2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    let d_prompt = r#"{"type":"user","timestamp":"2026-09-28T15:20:00.000Z","message":{"role":"user","content":"hi"}}"#;
    let d_answer = answer_at("claude-fable-5-1", "2026-09-28T15:20:30.000Z", Some("low"));
    let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
    proc.write(D, &[d_prompt, &d_answer]);
    proc.write(B, &[&sonnet_old]);
    let launch = LaunchFacts::default();
    let model = |f: &FooterFacts| f.model.clone();
    let shown = |f: &FooterFacts| (f.model.clone(), f.effort.clone());
    let is = |m: &str, e: Option<&str>| (Some(m.to_owned()), e.map(str::to_owned));
    proc.register(S);
    proc.write(S, &[&opus]);
    let mut cache = TailCache::default();
    proc.at("2026-09-28T15:25:00Z");
    assert_eq!(
        shown(&proc.read(&launch, &mut cache)),
        is("Opus 5.5", Some("xhigh"))
    );
    proc.register(D);
    proc.at("2026-09-28T15:30:00Z");
    assert_eq!(
        shown(&proc.read(&launch, &mut cache)),
        is("Fable 5.1", None),
        "restored — not Opus 5.5's xhigh, nor the other tab's `low`"
    );
    proc.register(B);
    proc.at("2026-09-28T15:31:00Z");
    assert_eq!(
        model(&proc.read(&launch, &mut cache)).as_deref(),
        Some("Fable 5.1"),
        "the restore pinned it"
    );
    proc.register(S);
    proc.write(C, &[]);
    let mut cache = TailCache::default();
    proc.at("2026-09-28T15:25:00Z");
    proc.read(&launch, &mut cache);
    let then = [
        (C, "2026-09-28T15:26:00Z"),
        (S, "2026-09-28T15:30:00Z"),
        (B, "2026-09-28T15:31:00Z"),
    ];
    for (session, at) in then {
        proc.register(session);
        proc.at(at);
        assert_eq!(
            model(&proc.read(&launch, &mut cache)).as_deref(),
            Some("Opus 5.5"),
            "{session}: a resume into the process's own session pins too"
        );
    }
}

/// A conversation whose file was born before the last sighting existed
/// before the switch whatever its rows say, so it restores and pins.
#[cfg(unix)]
#[test]
fn a_conversation_born_before_the_switch_is_resumed() {
    const S: &str = "00000000-0000-4000-8000-0000000001b1";
    const X: &str = "00000000-0000-4000-8000-0000000001b2";
    const B: &str = "00000000-0000-4000-8000-0000000001b3";
    let now = aterm_time::unix_secs().unwrap();
    let stamp = |secs: u64| {
        crate::harness::usage::rfc3339_utc(i64::try_from(secs).unwrap()).replace('Z', ".000Z")
    };
    let proc = Proc::new("born-before", &stamp(now - 3600));
    let opus = answer_at("claude-opus-5-5", &stamp(now - 1800), Some("xhigh"));
    let copied = format!(
        r#"{{"type":"assistant","timestamp":"{}","effort":"high","message":{{"model":"claude-fable-5-1"}},"forkedFrom":{{"sessionId":"elsewhere","messageUuid":"u"}}}}"#,
        stamp(now - 1200)
    );
    let sonnet_old = answer_at("claude-sonnet-5", &stamp(now - 7 * 86_400), Some("high"));
    proc.register(S);
    proc.write(S, &[&opus]);
    proc.write(B, &[&sonnet_old]);
    proc.write(X, &[&copied]);
    let launch = LaunchFacts::default();
    let run = |seen_in_s: u64| {
        proc.register(S);
        let mut cache = TailCache::default();
        proc.clock.set(seen_in_s);
        proc.read(&launch, &mut cache);
        [X, B]
            .into_iter()
            .zip(1..)
            .map(|(session, n)| {
                proc.register(session);
                proc.clock.set(seen_in_s + 10 * n);
                proc.read(&launch, &mut cache).model
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        run(now + 10),
        [Some("Fable 5.1".into()), Some("Fable 5.1".into())],
        "born before the switch: resumed, restored and pinned"
    );
    assert_eq!(
        run(now - 10),
        [Some("Opus 5.5".into()), Some("Sonnet 5".into())],
        "born after the process was last seen in S: its rows decide"
    );
}

/// A `/branch`'s copied `memory-mode` row (original stamp, no `forkedFrom`)
/// is not the branch's own first row, so a fresh branch is not read as a
/// `/resume`.
#[cfg(unix)]
#[test]
fn a_branchs_copied_memory_mode_row_is_not_its_own() {
    const S: &str = "00000000-0000-4000-8000-0000000001c1";
    const F: &str = "00000000-0000-4000-8000-0000000001c2";
    const B: &str = "00000000-0000-4000-8000-0000000001c3";
    let now = aterm_time::unix_secs().unwrap();
    let stamp = |secs: u64| {
        crate::harness::usage::rfc3339_utc(i64::try_from(secs).unwrap()).replace('Z', ".000Z")
    };
    let memory_mode = |session: &str, at: u64| {
        format!(
            r#"{{"type":"memory-mode","mode":"off","afterUuid":null,"timestamp":"{}","sessionId":"{session}"}}"#,
            stamp(at)
        )
    };
    let proc = Proc::new("branch-memory", &stamp(now - 3600));
    let opus = answer_at("claude-opus-5-5", &stamp(now - 1800), Some("xhigh"));
    let copied = format!(
        r#"{{"type":"assistant","timestamp":"{}","effort":"xhigh","message":{{"model":"claude-opus-5-5"}},"forkedFrom":{{"sessionId":"{S}","messageUuid":"u"}}}}"#,
        stamp(now - 1800)
    );
    let sonnet_old = answer_at("claude-sonnet-5", &stamp(now - 7 * 86_400), Some("high"));
    proc.write(S, &[&memory_mode(S, now - 1900), &opus]);
    proc.write(B, &[&sonnet_old]);
    proc.write(F, &[&copied, &memory_mode(F, now - 1900)]);
    let path = |session: &str| proc.project.join(format!("{session}.jsonl"));
    let head =
        |session: &str, own: bool| head_stamp(&mut open_regular(&path(session)).unwrap(), own);
    assert_eq!(head(F, true), None, "no message row of its own yet");
    assert_eq!(
        head(F, false),
        Some(now - 1800),
        "control: the head's first stamp is the copy's"
    );

    let launch = LaunchFacts::default();
    let mut cache = TailCache::default();
    proc.register(S);
    proc.clock.set(now - 10);
    assert_eq!(
        proc.read(&launch, &mut cache).model.as_deref(),
        Some("Opus 5.5")
    );
    proc.register(F);
    proc.clock.set(now + 5);
    assert_eq!(
        proc.read(&launch, &mut cache).model.as_deref(),
        Some("Opus 5.5"),
        "the branch runs on the process's model"
    );
    proc.register(B);
    proc.clock.set(now + 15);
    assert_eq!(
        proc.read(&launch, &mut cache).model.as_deref(),
        Some("Sonnet 5"),
        "the fork pinned nothing: `/resume B` restores B's model, as Claude does"
    );
}

/// A resumed conversation's newest answer past [`MAX_LINE_BYTES`] makes the
/// model unknown, never an older answer's, however windows cut the row.
#[cfg(unix)]
#[test]
fn an_oversized_newest_answer_is_never_passed_over() {
    const S: &str = "00000000-0000-4000-8000-0000000000f1";
    const A: &str = "00000000-0000-4000-8000-0000000000f2";
    let proc = Proc::new("oversized", "2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
    let big_answer = |model: &str, at: &str, bytes: usize| {
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"message":{{"model":"{model}","role":"assistant","content":[{{"type":"tool_use","name":"Write","input":{{"content":"{}"}}}}]}},"type":"assistant","uuid":"u","timestamp":"{at}","effort":"high"}}"#,
            "x".repeat(bytes)
        )
    };
    let big_result = |at: &str, bytes: usize| {
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","content":"{}"}}]}},"type":"user","uuid":"u","timestamp":"{at}"}}"#,
            "x".repeat(bytes)
        )
    };
    let after_resume = |a: &[&str]| {
        proc.register(S);
        proc.write(S, &[&opus]);
        proc.write(A, a);
        let mut cache = TailCache::default();
        proc.read(&LaunchFacts::default(), &mut cache);
        proc.register(A);
        proc.read(&LaunchFacts::default(), &mut cache).model
    };
    for bytes in [300 * 1024, 1200 * 1024] {
        let big = big_answer("claude-fable-5-1", "2026-09-27T10:05:00.000Z", bytes);
        assert_eq!(after_resume(&[&sonnet_old, &big]), None, "R4 at {bytes}");
        let tool = big_result("2026-09-27T10:05:00.000Z", bytes);
        assert_eq!(
            after_resume(&[&sonnet_old, &tool]).as_deref(),
            Some("Sonnet 5"),
            "a tool result at {bytes}"
        );
        let own = big_answer("claude-sonnet-5", "2026-09-28T15:06:00.000Z", bytes);
        assert_eq!(
            after_resume(&[&sonnet_old, &own]).as_deref(),
            Some("Sonnet 5"),
            "the process's own answer at {bytes}"
        );
    }
}

/// [`rows_back`] meets every row once, newest first, an oversized one in
/// its place by its end, however windows fall across it.
#[cfg(unix)]
#[test]
fn rows_back_meets_an_oversized_row_once_in_its_place() {
    let dir = cache_test_dir("rows-back");
    let path = dir.join("t.jsonl");
    let huge = format!("{{\"h\":\"{}\",\"end\":1}}", "a".repeat(1200 * 1024));
    let big = format!("{{\"b\":\"{}\",\"end\":2}}", "b".repeat(300 * 1024));
    std::fs::write(
        &path,
        body(&["{\"n\":1}", &huge, "{\"n\":2}", &big, "{\"n\":3}"]),
    )
    .unwrap();
    let mut met = Vec::new();
    let whole = rows_back(&path, |row| {
        met.push(match row {
            Row::Whole(raw) => String::from_utf8_lossy(raw).into_owned(),
            Row::Oversized(part) => {
                let text = String::from_utf8_lossy(part);
                let end = text
                    .trim_end()
                    .rsplit(',')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                format!("oversized {end}")
            }
        });
        std::ops::ControlFlow::Continue(())
    });
    assert_eq!(whole, Some(true));
    assert_eq!(
        met,
        [
            "{\"n\":3}",
            "oversized \"end\":2}",
            "{\"n\":2}",
            "oversized \"end\":1}",
            "{\"n\":1}"
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_launch_pins_the_model_by_flag_or_environment() {
    let argv = |args: &[&str]| args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    let of = |args: &[&str], env: &[&str]| launch_facts_of(&argv(args), &argv(env));
    assert_eq!(
        of(&["claude", "--model", "claude-opus-5-5"], &[]),
        LaunchFacts {
            model: Some("Opus 5.5".into()),
            pinned: true,
            resume: None,
            config: None,
        }
    );
    assert_eq!(
        of(&["claude", "--model", "opus"], &[]),
        LaunchFacts {
            model: None,
            pinned: true,
            resume: None,
            config: None,
        }
    );
    for env in [
        "ANTHROPIC_MODEL=claude-opus-5-5",
        "ANTHROPIC_DEFAULT_OPUS_MODEL=claude-opus-5-5",
        "ANTHROPIC_DEFAULT_FABLE_MODEL=x",
    ] {
        assert!(of(&["claude"], &[env]).pinned, "{env}");
    }
    for env in [
        "ANTHROPIC_MODEL=",
        "ANTHROPIC_API_KEY=k",
        "ANTHROPIC_DEFAULT_OPUS=x",
    ] {
        assert!(!of(&["claude"], &[env]).pinned, "{env}");
    }
    assert_eq!(
        of(&["claude", "--resume", "x"], &[]),
        LaunchFacts {
            model: None,
            pinned: false,
            resume: Some(LaunchResume::Session("x".into())),
            config: None,
        }
    );
    assert_eq!(
        of(&["claude", "--continue", "--model", "claude-opus-5-5"], &[]),
        LaunchFacts {
            model: Some("Opus 5.5".into()),
            pinned: true,
            resume: Some(LaunchResume::Unnamed),
            config: None,
        }
    );
    assert_eq!(
        of(&["claude", "--resume", "x", "--fork-session"], &[]).resume,
        Some(LaunchResume::Unnamed)
    );
    assert_eq!(
        of(&["claude", "--model=claude-fable-5-1[1m]"], &[])
            .model
            .as_deref(),
        Some("Fable 5.1")
    );
    assert_eq!(
        of(
            &[
                "claude",
                "--model",
                "claude-opus-5",
                "--resume",
                "x",
                "--model",
                "claude-opus-5-5"
            ],
            &[]
        )
        .model
        .as_deref(),
        Some("Opus 5.5")
    );
    assert_eq!(
        of(&["claude"], &["CLAUDE_CONFIG_DIR=/w/.claude"])
            .config
            .as_deref(),
        Some("/w/.claude")
    );
    assert_eq!(
        of(&["claude"], &["CLAUDE_CONFIG_DIR="]).config.as_deref(),
        Some("")
    );
    assert_eq!(of(&["claude"], &["HOME=/w"]).config, None);
}

#[test]
fn an_effort_result_names_the_effort_at_once() {
    let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z");
    let answer = r#"{"type":"assistant","timestamp":"2026-09-24T04:13:30.000Z","effort":"high","message":{"model":"claude-opus-5-5"}}"#;
    let command = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args></command-args>"}}"#;
    let result = |text: &str| {
        format!(
            r#"{{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{{"content":{}}}}}"#,
            aterm_json::to_string(&Value::String(format!(
                "<local-command-stdout>{text}</local-command-stdout>"
            )))
            .unwrap()
        )
    };
    let effort = |text: Option<&str>| {
        let mut body = format!("{answer}\n{command}\n");
        if let Some(text) = text {
            body.push_str(&result(text));
            body.push('\n');
        }
        tail_facts(body.as_bytes(), start)
    };
    let top = format!(
        "Set effort level to {CLAUDE_TOP_EFFORT_KEY} (this session only): xhigh + dynamic workflow orchestration"
    );
    for (text, level) in [
        (
            "Set effort level to high (saved as your default for new sessions): Deeper reasoning",
            "high",
        ),
        (top.as_str(), "xhigh"),
        (
            "Current effort level: max (Maximum reasoning; this session only)",
            "max",
        ),
        ("Set effort level to \u{1b}[1mlow\u{1b}[22m: Quick", "low"),
    ] {
        let facts = effort(Some(text));
        assert_eq!(facts.effort.shown(), Some(level), "{text:?}");
        assert_eq!(
            facts.model.shown(),
            Some("Opus 5.5"),
            "an effort leaves the model"
        );
    }
    for text in [
        "Effort level set to auto for this session",
        "Set effort level to turbo: new",
    ] {
        assert_eq!(effort(Some(text)).effort, Said::Unread, "{text:?}");
    }
    assert_eq!(
        effort(None).effort,
        Said::Unread,
        "an unreadable result is no effort"
    );
    assert_eq!(
        tail_facts(format!("{answer}\n").as_bytes(), start)
            .effort
            .shown(),
        Some("high")
    );
}

/// The carry and backward read: a named model survives appends larger than
/// the tail window, a choice in an append is read at once, and a fresh read
/// looks back past a huge row for the model and the Bash tool's directory.
#[cfg(unix)]
#[test]
fn an_answer_past_the_tail_window_is_carried_for_the_same_process() {
    let root = cache_test_dir("carry");
    let path = root.join("session.jsonl");
    let start = crate::harness::upgrade_models::parse_utc("2026-09-28T15:04:08Z").unwrap();
    let append = |text: &str| {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    };
    let image = format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-09-28T15:18:13.903Z\",\"message\":{{\"content\":[{{\"type\":\"image\",\"source\":\"{}\"}}]}}}}\n",
        "A".repeat(600 * 1024)
    );
    std::fs::write(
        &path,
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-28T15:05:00Z\",\"cwd\":\"/w/tree\",\"message\":{\"model\":\"claude-opus-5-5\"}}\n",
    )
    .unwrap();
    let mut cache = TailCache::default();
    let read = |cache: &mut TailCache, since| cache.read(&path, 4242, Some(start), since).unwrap();
    assert_eq!(read(&mut cache, start).model.shown(), Some("Opus 5.5"));
    append(&image);
    assert_eq!(
        read(&mut cache, start).model.shown(),
        Some("Opus 5.5"),
        "the answer is 600 KiB above the tail now: carried"
    );
    assert_eq!(cache.appends, 1, "read as an append");
    assert_eq!(
        read_tail_facts(&path, Some(start)).unwrap().model,
        Said::Unsaid
    );
    let fresh = TailCache::default()
        .read_since(&path, 4242, Some(start), start)
        .unwrap();
    assert_eq!(fresh.facts.model.shown(), Some("Opus 5.5"));
    assert_eq!(fresh.cwd, Some(PathBuf::from("/w/tree")));
    append(
        r#"{"type":"user","timestamp":"2026-09-28T15:19:00.000Z","message":{"content":"<local-command-stdout>Set model to `Fable 5.1`</local-command-stdout>"}}
"#,
    );
    append(&image);
    assert_eq!(
        read(&mut cache, start).model.shown(),
        Some("Fable 5.1"),
        "every appended byte is scanned"
    );
    assert_eq!(cache.appends, 2);
    assert_eq!(read(&mut cache, start + 6 * 3600), TailFacts::default());
    assert_eq!(read(&mut cache, start).model.shown(), Some("Fable 5.1"));
    std::fs::write(
        &path,
        format!(
            "{}\n",
            assistant_row("claude-opus-5", "2026-09-28T16:01:00Z")
        ),
    )
    .unwrap();
    assert_eq!(read(&mut cache, start).model.shown(), Some("Opus 5"));
    assert_eq!(cache.appends, 2, "a shrunk file is no append");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn facts_floor_at_the_kernel_else_the_registry_start() {
    let root = std::env::temp_dir().join(format!("aterm-footer-floor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cwd = root.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude = root.join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let project = claude.join("projects").join(project_slug(&cwd));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("0b5e7a11-f100.jsonl"),
        concat!(
            r#"{"type":"assistant","timestamp":"2026-09-22T16:44:57.000Z","effort":"high","message":{"model":"claude-opus-5"}}"#,
            "\n"
        ),
    )
    .unwrap();
    let start = crate::harness::upgrade_models::parse_utc("2026-09-24T04:13:26Z").unwrap();
    let entry = |proc_start: Option<u64>| {
        let stamp = proc_start.map_or(String::new(), |s| {
            format!(r#","procStart":"{}""#, lstart_utc(s))
        });
        let text = format!(
            r#"{{"pid":4242,"sessionId":"0b5e7a11-f100","cwd":"{}"{stamp}}}"#,
            json(&cwd)
        );
        std::fs::write(claude.join("sessions/4242.json"), text).unwrap();
    };
    let read = |started: Option<u64>| {
        let entry = session_of_pid(&claude, 4242, started)?;
        Some(facts_for_entry_at(
            &claude,
            4242,
            started,
            &entry,
            &LaunchFacts::default(),
            &mut TailCache::default(),
            SystemTime::now(),
        ))
    };
    let model = |started: Option<u64>| read(started).and_then(|f| f.model);
    entry(Some(start));
    assert_eq!(model(Some(start)), None, "the kernel's start floors it");
    assert_eq!(model(None), None, "the registry's start floors it");
    entry(None);
    assert_eq!(model(None), None, "no start known: no model");
    entry(Some(start - 3 * 86_400));
    assert_eq!(model(None).as_deref(), Some("Opus 5"));
    let old_start = start - 3 * 86_400;
    std::fs::write(
        claude.join("sessions/4242.json"),
        format!(
            r#"{{"pid":4242,"sessionId":"0b5e7a11-f100","cwd":"{}","procStart":"{}","startedAt":{}}}"#,
            json(&cwd),
            lstart_utc(old_start),
            start * 1000 + 250
        ),
    )
    .unwrap();
    assert_eq!(
        model(Some(old_start)),
        None,
        "the pre-exec image's answer does not speak for the new one"
    );
    let entry = session_of_pid(&claude, 4242, Some(old_start)).unwrap();
    let launch = launch_facts(&["claude".into(), "--model".into(), "claude-opus-5-5".into()]);
    let facts = facts_for_entry_at(
        &claude,
        4242,
        Some(old_start),
        &entry,
        &launch,
        &mut TailCache::default(),
        SystemTime::now(),
    );
    assert_eq!(facts.model.as_deref(), Some("Opus 5.5"));
    assert!(!facts.model_open && facts.effort_open);
    assert_eq!(
        facts.owner,
        Some(FactsOwner {
            floor: Some(start),
            session_id: "0b5e7a11-f100".into(),
        }),
        "the floor is the image's"
    );
    let unfloored = SessionEntry {
        proc_start: None,
        started_at: None,
        ..entry.clone()
    };
    let facts = facts_for_entry_at(
        &claude,
        4242,
        None,
        &unfloored,
        &launch,
        &mut TailCache::default(),
        SystemTime::now(),
    );
    assert_eq!(facts.model.as_deref(), Some("Opus 5.5"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_clear_keeps_the_model_the_process_chose() {
    let root = std::env::temp_dir().join(format!("aterm-footer-clear-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cwd = root.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let claude = root.join("claude");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let project = claude.join("projects").join(project_slug(&cwd));
    std::fs::create_dir_all(&project).unwrap();
    let start = crate::harness::upgrade_models::parse_utc("2026-09-28T15:04:08Z").unwrap();
    let registry = |session: &str, image: u64| {
        std::fs::write(
            claude.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{session}","cwd":"{}","procStart":"{}","startedAt":{},"version":"2.1.283"}}"#,
                json(&cwd),
                lstart_utc(start),
                image * 1000 + 684
            ),
        )
        .unwrap();
    };
    let transcript = |session: &str, rows: &[&str]| {
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(project.join(format!("{session}.jsonl")), body).unwrap();
    };
    let launch = launch_facts(&["claude".into(), "--model".into(), "claude-opus-5-5".into()]);
    let read_at = |cache: &mut TailCache, at: &str| {
        let entry = session_of_pid(&claude, 4242, Some(start)).expect("the registry");
        let at = crate::harness::upgrade_models::parse_utc(at).unwrap();
        facts_for_entry_at(
            &claude,
            4242,
            Some(start),
            &entry,
            &launch,
            cache,
            UNIX_EPOCH + std::time::Duration::from_secs(at),
        )
    };
    const A: &str = "00000000-0000-4000-8000-00000000000a";
    const B: &str = "00000000-0000-4000-8000-00000000000b";
    const C: &str = "00000000-0000-4000-8000-00000000000c";
    const CHOICE: [&str; 2] = [
        r#"{"type":"user","timestamp":"2026-09-28T15:04:17.798Z","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}"#,
        r#"{"type":"user","timestamp":"2026-09-28T15:04:17.798Z","message":{"role":"user","content":"<local-command-stdout>Set model to `Fable 5.1` for this session only</local-command-stdout>"}}"#,
    ];
    const EFFORT: &str = r#"{"type":"user","timestamp":"2026-09-28T15:04:28.429Z","message":{"role":"user","content":"<local-command-stdout>Set effort level to high (this session only): Deeper reasoning</local-command-stdout>"}}"#;
    registry(A, start);
    transcript(A, &CHOICE);
    let mut cache = TailCache::default();
    let facts = read_at(&mut cache, "2026-09-28T15:04:20Z");
    assert_eq!(
        (facts.model.as_deref(), facts.effort.as_deref()),
        (Some("Fable 5.1"), None)
    );
    transcript(A, &[CHOICE[0], CHOICE[1], EFFORT]);
    registry(B, start);
    let facts = read_at(&mut cache, "2026-09-28T15:04:30Z");
    assert_eq!(
        (facts.model.as_deref(), facts.effort.as_deref()),
        (Some("Fable 5.1"), Some("high")),
        "the process runs what it chose, not its `--model`"
    );
    assert!(
        !facts.model_open && !facts.effort_open,
        "the card may not fill it"
    );
    assert_eq!(facts.owner.map(|o| o.session_id).as_deref(), Some(B));
    assert_eq!(
        read_at(&mut TailCache::default(), "2026-09-28T15:04:30Z")
            .model
            .as_deref(),
        Some("Opus 5.5")
    );
    cache.clear();
    assert_eq!(
        read_at(&mut cache, "2026-09-28T15:05:00Z").model.as_deref(),
        Some("Fable 5.1")
    );
    registry(C, start + 3600);
    let facts = read_at(&mut cache, "2026-09-28T16:05:00Z");
    assert_eq!(facts.model.as_deref(), Some("Opus 5.5"));
    assert!(facts.effort_open);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_head_names_its_branch_or_a_short_commit() {
    assert_eq!(
        branch_of_head("ref: refs/heads/main\n").as_deref(),
        Some("main")
    );
    assert_eq!(
        branch_of_head("ref: refs/heads/feat/footer\n").as_deref(),
        Some("feat/footer")
    );
    assert_eq!(
        branch_of_head("4a2573a96c0ffee4a2573a96c0ffee4a2573a96c\n").as_deref(),
        Some("4a2573a")
    );
    assert_eq!(branch_of_head("garbage"), None);
}

#[test]
fn git_head_follows_a_worktree_file_and_climbs_to_the_top() {
    let root = std::env::temp_dir().join(format!("aterm-footer-git-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let main = root.join("repo");
    std::fs::create_dir_all(main.join(".git")).unwrap();
    std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(main.join("crates/deep")).unwrap();
    assert_eq!(git_head(&main.join("crates/deep")), Some("main".to_owned()));
    let wt_git = root.join("gitdirs/wt");
    std::fs::create_dir_all(&wt_git).unwrap();
    std::fs::write(wt_git.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
    let wt = root.join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
    assert_eq!(git_head(&wt), Some("feature".to_owned()));
    assert_eq!(
        read_git_head(&main.join("crates/deep")),
        GitHeadRead::Found("main".into(), main.clone())
    );
    std::fs::create_dir_all(wt.join("src")).unwrap();
    assert_eq!(
        read_git_head(&wt.join("src")),
        GitHeadRead::Found("feature".into(), wt.clone())
    );
    let _ = std::fs::remove_dir_all(&root);
}

fn repo_under(tag: &str, place: &str, branch: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("aterm-footer-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let repo = home.join(place).join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(
        repo.join(".git/HEAD"),
        format!("ref: refs/heads/{branch}\n"),
    )
    .unwrap();
    (root, home, repo)
}

#[test]
fn read_git_head_opens_nothing_under_a_file_provider_domain() {
    for place in [
        "Library/CloudStorage/x",
        "Library/Mobile Documents/com~apple~CloudDocs",
        "Library/Containers/x",
        "Library/Group Containers/group.x",
    ] {
        let (root, home, repo) = repo_under("provider", place, "main");
        assert_eq!(
            read_git_head_in(Some(&home), &repo),
            GitHeadRead::Absent,
            "{place}"
        );
        std::fs::create_dir_all(repo.join("src")).unwrap();
        assert_eq!(
            read_git_head_in(Some(&home), &repo.join("src")),
            GitHeadRead::Absent,
            "{place}/src"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    for place in [
        "Documents",
        "Library/Application Support",
        "Library/Caches/y",
        "src",
    ] {
        let (root, home, repo) = repo_under("ordinary", place, "main");
        assert_eq!(
            read_git_head_in(Some(&home), &repo),
            GitHeadRead::Found("main".into(), repo.clone()),
            "{place}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A worktree outside a provider domain or app container can point its
/// `gitdir:` into one; the `HEAD` there is not opened either.
#[cfg(unix)]
#[test]
fn a_worktree_pointing_into_a_provider_domain_is_absent() {
    for place in [
        "Library/CloudStorage/x",
        "Library/Containers/x",
        "Library/Group Containers/group.x",
    ] {
        let (root, home, provider_repo) = repo_under("wt-provider", place, "elsewhere");
        let wt = home.join("src").join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", provider_repo.join(".git").display()),
        )
        .unwrap();
        assert_eq!(
            read_git_head_in(Some(&home), &wt),
            GitHeadRead::Absent,
            "{place}"
        );
        let link = home.join("src").join("link");
        std::os::unix::fs::symlink(&provider_repo, &link).unwrap();
        assert_eq!(
            read_git_head_in(Some(&home), &link),
            GitHeadRead::Absent,
            "{place} symlink"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A kernel refusal is its own answer, not "no repository": `EPERM` (what a
/// macOS folder privacy consent returns) is denied, while `EACCES` (a
/// directory stripped of its search bit) and a missing file stay `Absent`, so
/// only a consent refusal raises the host's consent attention.
#[cfg(unix)]
#[test]
fn only_an_eperm_refusal_reads_as_denied() {
    assert!(is_eperm(&std::io::Error::from_raw_os_error(libc::EPERM)));
    assert!(!is_eperm(&std::io::Error::from_raw_os_error(libc::EACCES)));
    assert!(!is_eperm(&std::io::Error::from_raw_os_error(libc::ENOENT)));

    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("aterm-footer-eacces-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    assert_eq!(
        read_git_head(&repo),
        GitHeadRead::Found("main".into(), repo.clone())
    );
    std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let locked = read_git_head(&repo);
    std::fs::set_permissions(repo.join(".git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        locked,
        GitHeadRead::Absent,
        "an EACCES permission bit is not a privacy-consent refusal"
    );
}

#[test]
fn a_long_path_keeps_its_last_directory() {
    assert_eq!(
        elide_path("~/src/github.com/aterm").as_deref(),
        Some("\u{2026}/aterm")
    );
    assert_eq!(elide_path("/opt/work/").as_deref(), Some("\u{2026}/work"));
    assert_eq!(elide_path("~/aterm"), None, "no shorter");
    assert_eq!(elide_path("~"), None);
    assert_eq!(elide_path("/"), None);
}

#[test]
fn the_path_shows_home_as_a_tilde() {
    let home = Path::new("/Users//ana");
    let path = |cwd: &str| home_path(Path::new(cwd), Some(home));
    assert_eq!(path("/Users//ana/aterm").as_deref(), Some("~/aterm"));
    assert_eq!(path("/Users//ana/src/aterm").as_deref(), Some("~/src/aterm"));
    assert_eq!(path("/Users//ana").as_deref(), Some("~"));
    assert_eq!(path("/Users//anabel/x").as_deref(), Some("/Users//anabel/x"));
    assert_eq!(path("/opt/work").as_deref(), Some("/opt/work"));
    assert_eq!(
        home_path(Path::new("/w"), None).as_deref(),
        Some("/w"),
        "no home: the path whole"
    );
    assert_eq!(
        home_path(Path::new("/opt/work"), Some(Path::new("/"))).as_deref(),
        Some("/opt/work"),
        "a home of / abbreviates nothing"
    );
    assert_eq!(path("/Users//ana/a\nb"), None);
    assert_eq!(path("/Users//ana/a\u{1b}[2J"), None);
}

#[test]
fn the_account_stands_after_the_model_and_before_the_path() {
    use crate::harness::session_usage::{ModelTokens, UsageFacts, Wall};
    let plain = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        usage: UsageFacts::of(
            (
                vec![ModelTokens {
                    label: "opus".into(),
                    input: 1_200_000,
                    output: 40_000,
                }],
                crate::harness::session_usage::More::default(),
            ),
            Some(Wall {
                label: "5h",
                resets: "3pm".into(),
            }),
            None,
        ),
        ..FooterFacts::default()
    };
    let facts = FooterFacts {
        account: Some("work-2".into()),
        ..plain.clone()
    };
    let marks =
        |facts: &FooterFacts| -> Vec<char> { segments(facts).iter().map(|s| s.mark).collect() };
    assert_eq!(
        marks(&facts),
        [
            WALL_MARK,
            MODEL_MARK,
            ACCOUNT_MARK,
            PATH_MARK,
            BRANCH_MARK,
            USAGE_MARK
        ]
    );
    assert_eq!(
        segments(&facts)[2],
        Segment::plain(ACCOUNT_MARK, "work-2"),
        "the label, whole"
    );
    let bare = FooterFacts {
        model: Some("Opus 5.5".into()),
        account: Some("work-2".into()),
        ..FooterFacts::default()
    };
    assert_eq!(marks(&bare), [MODEL_MARK, ACCOUNT_MARK]);
    assert_eq!(
        marks(&plain),
        [WALL_MARK, MODEL_MARK, PATH_MARK, BRANCH_MARK, USAGE_MARK]
    );
}

/// End to end over a temp Claude directory: the fold reads the transcript
/// this process's sessions file names, only what was appended, sums
/// subagents and withholds tokens until caught up.
#[cfg(unix)]
#[test]
fn the_folded_facts_are_this_sessions_transcript() {
    use crate::harness::session_usage::ModelTokens;
    let root = cache_test_dir("folded");
    let claude = root.join("claude");
    let project = claude.join("projects").join("-w-repo");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let started = crate::harness::upgrade_models::parse_utc("2026-09-27T12:00:00Z").unwrap();
    let name_session = |id: &str| {
        std::fs::write(
            claude.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{id}","cwd":"/w/repo","procStart":"{}"}}"#,
                lstart_utc(started)
            ),
        )
        .unwrap();
    };
    name_session("abc-1");
    let row = |id: &str, model: &str, output: u64, ts: &str| {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"effort\":\"xhigh\",\"message\":{{\"id\":\"{id}\",\"model\":\"{model}\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":{output},\"cache_read_input_tokens\":990}}}}}}\n"
        )
    };
    let append = |path: &Path, text: &str| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .unwrap();
    };
    let before = "2026-09-27T11:00:00Z";
    let during = "2026-09-27T13:00:00Z";
    let mine = project.join("abc-1.jsonl");
    std::fs::write(&mine, row("m0", "claude-opus-5-5", 2, before)).unwrap();
    append(&mine, &row("m1", "claude-opus-5-5", 40_000, during));
    std::fs::write(
        project.join("zzz-9.jsonl"),
        row("z1", "claude-opus-5-5", 9_999_999, during),
    )
    .unwrap();
    let place = |_: Option<&str>, _: i64| None;
    let mut cache = FooterCache::default();
    let tail = std::cell::RefCell::new(TailCache::default());
    let read = |cache: &mut FooterCache| {
        folded(
            &claude,
            4242,
            Some(started),
            &mut tail.borrow_mut(),
            cache,
            0,
            &place,
        )
        .expect("a session")
    };
    let facts = read(&mut cache);
    assert_eq!(facts.model.as_deref(), Some("Opus 5.5"));
    assert_eq!(
        facts.usage.expect("tokens").models,
        vec![ModelTokens {
            label: "opus".into(),
            input: 2_000,
            output: 40_002
        }],
        "this session's whole conversation, not the neighbour's"
    );
    let usage = cache.usage.as_ref().expect("folding");
    assert!(!usage.moved(), "just read");
    let bytes = usage.bytes_read();
    let more = row("m2", "claude-haiku-4-5", 5, during);
    append(&mine, &more);
    let facts = read(&mut cache);
    assert_eq!(
        cache.usage.as_ref().expect("folding").bytes_read() - bytes,
        more.len() as u64 + 1,
        "the appended row and the boundary byte"
    );
    assert_eq!(facts.model.as_deref(), Some("Haiku 4.5"));
    assert_eq!(facts.usage.expect("usage").models.len(), 2);

    let subs = project.join("abc-1").join("subagents");
    std::fs::create_dir_all(&subs).unwrap();
    let many: String = (0..40)
        .map(|i| row(&format!("s{i}"), "claude-haiku-4-5", 1, during))
        .collect();
    append(&subs.join("agent-a.jsonl"), &many);
    let mut short = FooterCache {
        budget: std::fs::metadata(&mine).unwrap().len() + 64,
        ..FooterCache::default()
    };
    assert_eq!(read(&mut short).usage, None, "a prefix is not shown");
    assert_eq!(
        (short.traffic(), short.traffic_generation()),
        (None, None),
        "nor graphed"
    );
    let mut reads = 1;
    let caught = loop {
        reads += 1;
        if let Some(usage) = read(&mut short).usage {
            break usage;
        }
        assert!(reads < 100, "it catches up");
    };
    assert_eq!(
        caught
            .models
            .iter()
            .find(|m| m.label == "haiku")
            .map(|m| m.output),
        Some(5 + 40),
        "the whole session once caught up"
    );
    let traffic = short.traffic().expect("graphed once caught up");
    assert_eq!(traffic.width_s, 2 * crate::harness::usage::SERIES_BASE_S);
    assert_eq!(traffic.totals, (43 * 1_000, 2 + 40_000 + 5 + 40));
    let graphed = traffic
        .buckets
        .iter()
        .fold((0, 0), |(i, o), &(bi, bo)| (i + bi, o + bo));
    assert_eq!(graphed, traffic.totals, "every row was stamped");
    assert_eq!((traffic.unread, traffic.runtime_s), (0, None));
    let generation = short.traffic_generation();
    read(&mut short);
    assert_eq!(short.traffic_generation(), generation);
    append(&mine, &row("m3", "claude-haiku-4-5", 1, during));
    read(&mut short);
    assert_ne!(short.traffic_generation(), generation);
    assert_eq!(
        short.dirs(),
        Some(&FactDirs {
            cwd: PathBuf::from("/w/repo"),
            repo_root: None
        })
    );

    std::fs::remove_dir_all(root).unwrap();
}

/// The fold is one conversation of one process: a `/clear` before its first
/// prompt or a new process in the tab shows no previous Σ or wall (a torn
/// read: [`a_torn_read_of_the_same_session_keeps_its_fold`],
/// [`a_torn_read_of_another_processs_file_shows_nothing`]).
#[cfg(unix)]
#[test]
fn a_new_conversation_or_process_never_shows_the_last_ones_usage() {
    let root = cache_test_dir("carry");
    let (claude, transcript, two_pm) = walled_session(&root);
    append_to(&transcript, &served_row("m0", two_pm - 600));
    let utc = |_: Option<&str>, _: i64| Some(0);
    let now = two_pm + 60;
    let started = Some(1_700_000_000);
    let tails = std::cell::RefCell::new(std::collections::HashMap::<u32, TailCache>::new());
    let usage_of = |pid: u32, cache: &mut FooterCache| {
        folded(
            &claude,
            pid,
            started,
            tails.borrow_mut().entry(pid).or_default(),
            cache,
            now,
            &utc,
        )
        .expect("a session")
        .usage
    };
    let name = |pid: u32, id: &str| {
        std::fs::write(
            claude.join(format!("sessions/{pid}.json")),
            format!(r#"{{"pid":{pid},"sessionId":"{id}","cwd":"/w/repo"}}"#),
        )
        .unwrap();
    };
    let mut cache = FooterCache::default();
    let shown = usage_of(4242, &mut cache).expect("control");
    assert!(shown.wall.is_some() && !shown.models.is_empty(), "control");

    name(4242, "def-2");
    assert_eq!(usage_of(4242, &mut cache), None, "after /clear");
    name(4242, "abc-1");
    assert_eq!(usage_of(4242, &mut cache), Some(shown.clone()), "back");

    name(5555, "ghi-3");
    assert_eq!(usage_of(5555, &mut cache), None, "a new process");
    std::fs::remove_dir_all(root).unwrap();
}

/// `/resume` (or `/clear`) into another conversation reads model and Σ from
/// it together: one footer read parses the sessions file once, so a rewrite
/// landing mid-read cannot bring back the previous Σ and wall.
#[cfg(unix)]
#[test]
fn a_resume_to_another_session_never_pairs_its_model_with_the_old_usage() {
    use crate::harness::session_usage::ModelTokens;
    let root = cache_test_dir("resume-torn");
    let claude = root.join("claude");
    let project = claude.join("projects").join("-w-repo");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let started = crate::harness::upgrade_models::parse_utc("2026-09-27T12:00:00Z").unwrap();
    let name_session = |id: &str| {
        std::fs::write(
            claude.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{id}","cwd":"/w/repo","procStart":"{}"}}"#,
                lstart_utc(started)
            ),
        )
        .unwrap();
    };
    let row = |id: &str, model: &str, output: u64| {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-09-27T13:00:00Z\",\"message\":{{\"id\":\"{id}\",\"model\":\"{model}\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":{output}}}}}}}\n"
        )
    };
    std::fs::write(
        project.join("abc-1.jsonl"),
        row("a1", "claude-opus-5-5", 40_000),
    )
    .unwrap();
    std::fs::write(
        project.join("def-2.jsonl"),
        row("d1", "claude-haiku-4-5", 7),
    )
    .unwrap();
    let place = |_: Option<&str>, _: i64| None;
    let mut tail = TailCache::default();
    let mut cache = FooterCache::default();
    name_session("abc-1");
    let facts = folded(
        &claude,
        4242,
        Some(started),
        &mut tail,
        &mut cache,
        0,
        &place,
    )
    .expect("control");
    assert_eq!(facts.model.as_deref(), Some("Opus 5.5"), "control");
    assert!(facts.usage.is_some(), "control");

    name_session("def-2");
    let (facts, reads) = read_torn_after_first(
        &claude,
        4242,
        Some(started),
        &mut tail,
        &mut cache,
        0,
        &place,
    );
    let facts = facts.expect("the read's one parse was whole");
    assert_eq!(facts.model.as_deref(), Some("Haiku 4.5"), "the resumed one");
    assert_eq!(
        facts.usage.map(|u| u.models),
        Some(vec![ModelTokens {
            label: "haiku".into(),
            input: 10,
            output: 7
        }]),
        "the Σ of the conversation the model is from"
    );
    assert_eq!(reads, 1, "one footer read parses the sessions file once");
    std::fs::remove_dir_all(root).unwrap();
}

/// A mid-write sessions file keeps the fold of the session last named,
/// with or without a kernel start time; a torn only parse yields no facts
/// and the next good read folds nothing again.
#[cfg(unix)]
#[test]
fn a_torn_read_of_the_same_session_keeps_its_fold() {
    for started in [None, Some(1_700_000_000)] {
        let root = cache_test_dir("same-torn");
        let (claude, transcript, two_pm) = walled_session(&root);
        append_to(&transcript, &served_row("m0", two_pm - 600));
        let utc = |_: Option<&str>, _: i64| Some(0);
        let now = two_pm + 60;
        let mut cache = FooterCache::default();
        let tail = std::cell::RefCell::new(TailCache::default());
        let read = |cache: &mut FooterCache| {
            folded(
                &claude,
                4242,
                started,
                &mut tail.borrow_mut(),
                cache,
                now,
                &utc,
            )
        };
        let shown = read(&mut cache).and_then(|f| f.usage).expect("control");
        assert!(shown.wall.is_some(), "control");

        let (facts, reads) = read_torn_after_first(
            &claude,
            4242,
            started,
            &mut tail.borrow_mut(),
            &mut cache,
            now,
            &utc,
        );
        assert_eq!(
            facts.expect("the read's one parse was whole").usage,
            Some(shown.clone()),
            "started {started:?}: Σ and the wall stay on the glass"
        );
        assert_eq!(reads, 1, "one footer read parses the sessions file once");

        let sessions = claude.join("sessions/4242.json");
        let whole = std::fs::read_to_string(&sessions).unwrap();
        let held = cache.usage.as_ref().expect("folding").bytes_read();
        std::fs::write(&sessions, &whole[..whole.len() / 2]).unwrap();
        assert_eq!(read(&mut cache), None, "no facts from a torn parse");
        std::fs::write(&sessions, &whole).unwrap();
        assert_eq!(read(&mut cache).and_then(|f| f.usage), Some(shown));
        assert_eq!(
            cache.usage.as_ref().expect("the fold is kept").bytes_read(),
            held,
            "nothing refolded after a torn read"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A torn read of another process's sessions file (a new Claude Code in the
/// tab) shows nothing of the old fold, nor does its next good read before
/// its first prompt.
#[cfg(unix)]
#[test]
fn a_torn_read_of_another_processs_file_shows_nothing() {
    let root = cache_test_dir("other-torn");
    let (claude, transcript, two_pm) = walled_session(&root);
    append_to(&transcript, &served_row("m0", two_pm - 600));
    let utc = |_: Option<&str>, _: i64| Some(0);
    let now = two_pm + 60;
    let started = Some(1_700_000_000);
    let mut cache = FooterCache::default();
    let tails = std::cell::RefCell::new(std::collections::HashMap::<u32, TailCache>::new());
    let usage_of = |pid: u32, cache: &mut FooterCache| {
        folded(
            &claude,
            pid,
            started,
            tails.borrow_mut().entry(pid).or_default(),
            cache,
            now,
            &utc,
        )
        .map(|f| f.usage)
    };
    assert!(
        usage_of(4242, &mut cache).flatten().is_some(),
        "control: the tab's cache holds 4242's fold"
    );
    let other = claude.join("sessions/5555.json");
    let whole = r#"{"pid":5555,"sessionId":"ghi-3","cwd":"/w/repo"}"#;
    std::fs::write(&other, &whole[..whole.len() / 2]).unwrap();
    assert_eq!(usage_of(5555, &mut cache), None, "its only parse torn");
    std::fs::write(&other, whole).unwrap();
    let (facts, reads) = read_torn_after_first(
        &claude,
        5555,
        started,
        tails.borrow_mut().entry(5555).or_default(),
        &mut cache,
        now,
        &utc,
    );
    assert_eq!(
        facts.expect("the read's one parse was whole").usage,
        None,
        "rewritten inside the read"
    );
    assert_eq!(reads, 1, "one footer read parses the sessions file once");
    assert_eq!(usage_of(5555, &mut cache), Some(None), "its next good read");
    std::fs::remove_dir_all(root).unwrap();
}

/// One footer read ([`read_pid`]) looks its transcript up once
/// ([`transcript_path`]), for both the slug's directory and a scanned one.
#[cfg(unix)]
#[test]
fn one_footer_read_looks_its_transcript_up_once() {
    let root = cache_test_dir("one-lookup");
    let (claude, transcript, two_pm) = walled_session(&root);
    append_to(&transcript, &served_row("m0", two_pm - 600));
    let utc = |_: Option<&str>, _: i64| Some(0);
    let started = Some(1_700_000_000);
    let read = |claude: &Path| {
        TRANSCRIPT_LOOKUPS.with(|n| n.set(0));
        let facts = folded(
            claude,
            4242,
            started,
            &mut TailCache::default(),
            &mut FooterCache::default(),
            two_pm + 60,
            &utc,
        )
        .expect("a session");
        (facts, TRANSCRIPT_LOOKUPS.with(std::cell::Cell::get))
    };
    let (facts, lookups) = read(&claude);
    assert_eq!(facts.model.as_deref(), Some("Opus 5.5"), "the tail read it");
    assert!(facts.usage.is_some(), "the fold read it");
    assert_eq!(lookups, 1, "one lookup, the slug's own directory");

    let projects = claude.join("projects");
    std::fs::rename(projects.join("-w-repo"), projects.join("-w-repo-moved")).unwrap();
    let (facts, lookups) = read(&claude);
    assert_eq!(
        facts.model.as_deref(),
        Some("Opus 5.5"),
        "found by the scan"
    );
    assert!(
        facts.usage.is_some(),
        "the fold read the file the scan found"
    );
    assert_eq!(lookups, 1, "one lookup, one scan of every project");
    std::fs::remove_dir_all(root).unwrap();
}

/// A Claude directory where process 4242 runs `abc-1`, whose transcript has
/// one session-limit notice at 14:00 UTC: the directory, transcript, 14:00.
#[cfg(unix)]
fn walled_session(root: &Path) -> (PathBuf, PathBuf, i64) {
    let claude = root.join("claude");
    let project = claude.join("projects").join("-w-repo");
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        claude.join("sessions/4242.json"),
        r#"{"pid":4242,"sessionId":"abc-1","cwd":"/w/repo"}"#,
    )
    .unwrap();
    let two_pm = 1_790_000_000 - 1_790_000_000 % 86_400 + 14 * 3600;
    let transcript = project.join("abc-1.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{{\"id\":\"n1\",\"model\":\"<synthetic>\",\"content\":[{{\"type\":\"text\",\"text\":\"You've hit your session limit \u{00b7} resets 3pm (UTC)\"}}],\"usage\":{{\"input_tokens\":0,\"output_tokens\":0}}}}}}\n",
            crate::harness::usage::rfc3339_utc(two_pm)
        ),
    )
    .unwrap();
    (claude, transcript, two_pm)
}

/// A response the API served, written at `at`, with its newline.
#[cfg(unix)]
fn served_row(id: &str, at: i64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-opus-5-5\",\"usage\":{{\"input_tokens\":100,\"output_tokens\":50}}}}}}\n",
        crate::harness::usage::rfc3339_utc(at)
    )
}

/// The wall segment's text for process 4242 at `now` (UTC). No start is
/// known, so no transcript row speaks for the process ([`floor_of`]).
#[cfg(unix)]
fn wall_at(claude: &Path, cache: &mut FooterCache, now: i64) -> Option<String> {
    let utc = |_: Option<&str>, _: i64| Some(0);
    folded(
        claude,
        4242,
        None,
        &mut TailCache::default(),
        cache,
        now,
        &utc,
    )
    .and_then(|f| segments(&f).into_iter().find(|s| s.mark == WALL_MARK))
    .map(|s| s.text)
}

#[cfg(unix)]
fn append_to(path: &Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .unwrap();
}

/// The limit is the account's: a background agent served after the notice
/// takes the wall down; one served before it does not.
#[cfg(unix)]
#[test]
fn a_response_a_subagent_was_served_since_takes_the_wall_down() {
    let root = cache_test_dir("wall-sub");
    let (claude, transcript, two_pm) = walled_session(&root);
    let agent = transcript
        .with_extension("")
        .join("subagents/workflows/wf1/agent-a1.jsonl");
    std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
    append_to(&agent, &served_row("w0", two_pm - 60));
    let mut cache = FooterCache::default();
    assert!(
        wall_at(&claude, &mut cache, two_pm + 300).is_some(),
        "served before the notice: the wall stands"
    );
    append_to(&agent, &served_row("w1", two_pm + 600));
    assert_eq!(
        wall_at(&claude, &mut cache, two_pm + 700),
        None,
        "a workflow's agent served since"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// A fold still behind shows no wall it has not caught up to; a wall from a
/// caught-up read is kept while a later read is behind.
#[cfg(unix)]
#[test]
fn a_fold_still_behind_shows_no_wall_it_has_not_caught_up_to() {
    let root = cache_test_dir("wall-behind");
    let (claude, transcript, two_pm) = walled_session(&root);
    let notice_len = std::fs::metadata(&transcript).unwrap().len();
    append_to(&transcript, &served_row("m1", two_pm + 600));
    let mut cache = FooterCache {
        budget: notice_len + 8,
        ..FooterCache::default()
    };
    assert_eq!(
        wall_at(&claude, &mut cache, two_pm + 700),
        None,
        "a prefix holding the notice but not the response that lifted it"
    );
    for _ in 0..8 {
        assert_eq!(wall_at(&claude, &mut cache, two_pm + 700), None);
    }
    assert!(cache.refreshed.is_some_and(|r| r.caught_up), "caught up");

    let root2 = cache_test_dir("wall-kept");
    let (claude, transcript, two_pm) = walled_session(&root2);
    let mut cache = FooterCache::default();
    assert!(
        wall_at(&claude, &mut cache, two_pm + 60).is_some(),
        "control"
    );
    cache.budget = 64;
    let filler: String = (0..20)
        .map(|i| served_row(&format!("x{i}"), two_pm - 3600))
        .collect();
    append_to(&transcript, &filler);
    assert!(
        wall_at(&claude, &mut cache, two_pm + 120).is_some(),
        "behind: the caught-up read's wall stands"
    );
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(root2).unwrap();
}

/// End to end: the wall is this session's notice, shown while its reset is
/// ahead; an unplaceable reset shows nothing.
#[cfg(unix)]
#[test]
fn the_limit_wall_is_read_from_the_transcripts_row() {
    let root = cache_test_dir("wall");
    let (claude, transcript, two_pm) = walled_session(&root);
    let day0 = two_pm - 14 * 3600;
    let mut cache = FooterCache::default();
    assert_eq!(
        wall_at(&claude, &mut cache, two_pm + 60),
        Some(super::super::session_usage::wall_text(
            &super::super::session_usage::Wall {
                label: "5h",
                resets: "3pm".into(),
            }
        )),
        "the vendor's reset, its zone left off"
    );
    assert!(wall_at(&claude, &mut cache, day0 + 15 * 3600 - 1).is_some());
    assert_eq!(
        wall_at(&claude, &mut cache, day0 + 15 * 3600),
        None,
        "reset: gone"
    );
    assert_eq!(
        wall_at(
            &claude,
            &mut FooterCache::default(),
            day0 + 86_400 + 11 * 3600
        ),
        None,
        "read the next morning: the row is day 0's, its 3pm has passed"
    );
    let unplaced = std::fs::read_to_string(&transcript)
        .unwrap()
        .replace("3pm (UTC)", "when it does");
    std::fs::write(&transcript, unplaced).unwrap();
    assert_eq!(
        wall_at(&claude, &mut FooterCache::default(), two_pm + 60),
        None,
        "a reset that cannot be placed: nothing, never a guess"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn is_mode_row_takes_known_pills_and_their_cuts_and_nothing_else() {
    for row in [
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents",
        "  \u{23F5}\u{23F5} bypass permissions on \u{00B7} 1 monitor \u{00B7} \u{2190} for agents \u{00B7} \u{2193} to manage",
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interrupt \u{00B7} \u{2190} for agents        /rc active",
        "  \u{23F8} plan mode on (shift+tab to cycle)",
        "  \u{23F8} manual mode on (shift+tab to cycle)",
        "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)",
        "  \u{23F5}\u{23F5} don't ask on",
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to  \u{00B7}",
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to  \u{00B7} \u{2190}\u{2026}",
        "  \u{23F5}\u{23F5} bypass permissions on  \u{00B7} \u{2190} fo\u{2026}",
        "  \u{23F5}\u{23F5} auto mode on      \u{00B7}",
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7}",
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to\u{2026}",
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interru\u{2026}",
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interrupt \u{00B7} \u{2190} f\u{2026}",
        "  \u{23F5}\u{23F5} bypass permissions on \u{00B7}\u{2026}",
        "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} \u{2190}\u{2026}",
        "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} \u{2190} \u{2026}",
        "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} ?\u{2026}",
        "  \u{23F5}\u{23F5} bypass permissions on (beta)",
    ] {
        assert!(is_mode_row(row), "{row:?}");
    }
    for row in [
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cy",
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to go",
        "  \u{23F5}\u{23F5} bypass permissions on ((shift+tab",
        "  ? for shortcuts",
        "  esc to clear",
        "  ! for shell mode",
        "\u{276F} Try \"refactor gpu_matches_cpu.rs\"",
        "  bypass permissions on",
        "    \u{23F5}\u{23F5} bypass permissions on",
        "  \u{23F8} auto mode on",
    ] {
        assert!(!is_mode_row(row), "{row:?}");
    }
}

#[test]
fn a_narrow_pane_cuts_the_hint_between_words_and_the_row_is_still_the_mode_row() {
    let cases: [(&str, &str); 5] = [
        (
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab",
            "mode.bypass",
        ),
        (
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to",
            "mode.bypass",
        ),
        ("  \u{23F5}\u{23F5} auto mode on (shift+tab  ", "mode.auto"),
        ("  \u{23F8} plan mode on (shift+tab to", "mode.plan"),
        ("  \u{23F5}\u{23F5} bypass permissions on ", "mode.bypass"),
    ];
    for (row, indicator) in cases {
        assert!(is_mode_row(row), "{row:?}");
        assert_eq!(pill_indicator(row), Some(indicator), "{row:?}");
    }
}

#[test]
fn a_reused_pid_is_refused_by_its_start_time() {
    assert_eq!(lstart_utc(1_790_194_435), "Wed Sep 23 20:13:55 2026");
    assert_eq!(lstart_utc(0), "Thu Jan 1 00:00:00 1970");
    let text = r#"{"pid":9162,"sessionId":"03396a15-856e","cwd":"/w","procStart":"Thu Sep  3 20:13:55 2026"}"#;
    let sep3 = 1_790_194_435 - 20 * 86_400;
    assert!(
        parse_session_entry(text, 9162, Some(sep3)).is_some(),
        "`ps` pads a one-digit day; the compare squashes it"
    );
    assert_eq!(
        parse_session_entry(text, 9162, Some(sep3 + 1)),
        None,
        "a different process that holds the same pid"
    );
    assert!(
        parse_session_entry(text, 9162, None).is_some(),
        "no kernel answer: the pid match stands alone"
    );
    assert_eq!(
        parse_session_entry(text, 9162, None).and_then(|e| e.proc_start),
        Some(sep3),
        "the registry's start is kept as the transcript floor"
    );
}

#[test]
fn the_claude_dir_is_the_processs_own() {
    let (work, cfg, ana) = (abs("/work"), abs("/cfg/claude"), abs("/Users//ana"));
    let home = Some(ana.as_str());
    let runs_in = || Some(PathBuf::from(&work));
    assert_eq!(
        claude_dir_of(Some(&cfg), home, runs_in),
        Some(PathBuf::from(&cfg))
    );
    assert_eq!(
        claude_dir_of(Some(""), home, runs_in),
        Some(PathBuf::from(&work)),
        "an empty override is the directory it runs in"
    );
    assert_eq!(
        claude_dir_of(Some("rel/dir"), home, runs_in),
        Some(Path::new(&work).join("rel/dir"))
    );
    assert_eq!(
        claude_dir_of(Some("rel/dir"), home, || None),
        None,
        "its directory not read"
    );
    assert_eq!(
        claude_dir_of(None, home, || None),
        Some(Path::new(&ana).join(".claude"))
    );
    assert_eq!(claude_dir_of(None, None, || None), None);
}

/// A FIFO where `HEAD` should be must not park the reader thread.
#[cfg(unix)]
#[test]
fn a_fifo_is_not_read() {
    let dir = std::env::temp_dir().join(format!("aterm-footer-fifo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fifo = dir.join("HEAD");
    let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated path we own; mkfifo only creates a node.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert_eq!(read_small(&fifo, MAX_GIT_FILE_BYTES), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_renamed_pill_still_opens_with_a_pill_glyph() {
    let renamed = "  \u{23F5}\u{23F5} turbo mode on (shift+tab to cycle)";
    assert!(opens_with_pill_glyph(renamed));
    assert_eq!(pill_indicator(renamed), None);
    assert!(opens_with_pill_glyph("  \u{23F8} manual mode on"));
    assert!(!opens_with_pill_glyph("  ? for shortcuts"));
    assert!(!opens_with_pill_glyph(
        "    \u{23F5}\u{23F5} indented tool output"
    ));
    assert!(is_cut_pill("  \u{23F5}\u{23F5} bypass permissi"));
    assert!(is_cut_pill("  \u{23F5}\u{23F5} bypass permissions o"));
    assert!(!is_cut_pill(renamed));
}

fn coverable(row: &str, width: usize) -> Vec<bool> {
    let mut out: Vec<bool> = row.chars().map(|c| c == '\u{2500}').collect();
    out.resize(width, false);
    out
}

#[test]
fn every_recorded_screen_has_a_rule_the_footer_can_write() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    let mut ruled = 0;
    for entry in std::fs::read_dir(&dir).expect("the phase fixtures") {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
        let Some(bottom) = aterm_phase::phase::composer_bottom(&rows) else {
            continue;
        };
        let rule = &rows[bottom];
        let width = rule.chars().count();
        let run = rule_run(&coverable(rule, width))
            .unwrap_or_else(|| panic!("{}: no plain glyph in {rule:?}", path.display()));
        let room = rule_room(&run);
        if width >= 80 {
            assert_eq!(
                fit_rule(&facts, room, None).segments,
                segments(&facts),
                "{}: the whole footer fits a {width}-column rule",
                path.display()
            );
        }
        if let Some(mode) = mode_row(&rows) {
            assert!(mode > bottom && is_mode_row(&rows[mode]));
        }
        ruled += 1;
    }
    assert!(ruled >= 10, "the corpus must exercise the rule: {ruled}");
}

#[test]
fn the_rule_run_is_the_widest_plain_run() {
    let row = |s: &str| s.chars().map(|c| c == '-').collect::<Vec<_>>();
    assert_eq!(rule_run(&row("--------")), Some(0..8));
    assert_eq!(rule_run(&row("---x------")), Some(4..10));
    assert_eq!(
        rule_run(&row("----x----")),
        Some(5..9),
        "the rightmost of equals"
    );
    assert_eq!(rule_run(&row("xxxx")), None);
    assert_eq!(rule_run(&[]), None);
    assert_eq!(rule_room(&(0..3)), 0, "too narrow for any write");
}

fn read_set(set: &SetFacts, pitches: f32) -> String {
    let pitch = set.cells as f32 / pitches;
    let at = |g: &SetGlyph| {
        g.cell as f32 + g.cols as f32 / 2.0 + f32::from(g.nudge) / f32::from(NUDGES_PER_CELL)
    };
    let mut out = String::new();
    for (i, g) in set.glyphs.iter().enumerate() {
        if let Some(before) = i.checked_sub(1).map(|b| &set.glyphs[b]) {
            let room = (at(g) - at(before)) / pitch - (before.cols + g.cols) as f32 / 2.0;
            out.extend(std::iter::repeat_n(' ', room.round() as usize));
        }
        out.push(g.ch);
    }
    out
}

#[test]
fn the_facts_are_set_as_a_line_of_type() {
    let step = 1.0 / f32::from(NUDGES_PER_CELL);
    let at = |g: &SetGlyph| {
        g.cell as f32 + g.cols as f32 / 2.0 + f32::from(g.nudge) / f32::from(NUDGES_PER_CELL)
    };
    let check = |facts: &FooterFacts, reading: &str| {
        let segments = segments(facts);
        let set = set_facts(&segments);
        let pitches = reading
            .chars()
            .map(aterm_grapheme::char_width)
            .sum::<usize>() as f32;
        assert_eq!(read_set(&set, pitches), reading);
        let pitch = set.cells as f32 / pitches;
        let ink = set.glyphs.iter().map(|g| g.cols).sum::<usize>() as f32;
        let tight = FACT_PITCH.max(ink / pitches);
        assert!(
            (tight..tight + 1.0 / pitches).contains(&pitch),
            "{reading}: {pitch}"
        );
        let (first, last) = (set.glyphs[0], set.glyphs[set.glyphs.len() - 1]);
        assert!((at(&first) - first.cols as f32 * pitch / 2.0).abs() <= step);
        assert!((set.cells as f32 - at(&last) - last.cols as f32 * pitch / 2.0).abs() <= step);
        for pair in set.glyphs.windows(2) {
            assert!(pair[1].cell >= pair[0].cell + pair[0].cols, "{pair:?}");
        }
        assert!(last.cell + last.cols <= set.cells);
        assert!(
            set.cells < reading.chars().count(),
            "CONTROL: {} cells, {} characters",
            set.cells,
            reading.chars().count()
        );
        set
    };
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    check(
        &facts,
        "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main",
    );
    let long = FooterFacts {
        path: Some("~/Developer/aterm-typo-20261014/crates/aterm-gui".into()),
        branch: Some("ui/footer-spacing-iosevka-20261014".into()),
        ..facts.clone()
    };
    check(
        &long,
        "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/Developer/aterm-typo-20261014/crates/aterm-gui   \u{2387} ui/footer-spacing-iosevka-20261014",
    );
    let wide = FooterFacts {
        branch: Some("\u{6F22}\u{5B57}".into()),
        ..facts.clone()
    };
    let set = check(
        &wide,
        "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} \u{6F22}\u{5B57}",
    );
    assert_eq!(set.glyphs.iter().filter(|g| g.cols == 2).count(), 2);
    let deep = FooterFacts {
        path: Some(format!("~/{}", "deep/".repeat(30))),
        ..facts
    };
    let set = set_facts(&segments(&deep));
    let d = set
        .glyphs
        .iter()
        .position(|g| g.ch == 'd')
        .expect("the path");
    let pitches: Vec<f32> = set.glyphs[d..d + 40]
        .windows(2)
        .map(|p| at(&p[1]) - at(&p[0]))
        .collect();
    let (lo, hi) = pitches
        .iter()
        .fold((f32::MAX, 0.0f32), |(lo, hi), &p| (lo.min(p), hi.max(p)));
    assert!(
        lo > FACT_PITCH && hi - lo <= 2.0 * step,
        "looser, evenly: {lo}..{hi}"
    );
    assert!(set_facts(&[]).glyphs.is_empty());
}

#[test]
fn native_footer_text_preserves_combining_and_joined_graphemes_when_set_and_elided() {
    for email in [
        "jose\u{0301}@example.com",
        "x\u{0301}\u{0323}@example.com",
        "使用者👩‍💻@example.com",
    ] {
        let set = set_facts(&[Segment::plain(ACCOUNT_MARK, email)]);
        let mut actual = String::new();
        for glyph in set.glyphs.iter().filter(|glyph| !glyph.mark) {
            assert!(glyph.cell + glyph.cols <= set.cells);
            if let Some((_, cluster)) = set.clusters.iter().find(|(col, _)| *col == glyph.cell) {
                actual.push_str(cluster);
            } else {
                actual.push(glyph.ch);
                if let Some((_, marks)) = set.combining.iter().find(|(col, _)| *col == glyph.cell) {
                    actual.extend(marks.iter());
                }
            }
        }
        assert_eq!(actual, email);
        if email.contains('👩') {
            assert!(
                set.glyphs.iter().all(|glyph| glyph.nudge == 0),
                "full-size color emoji must leave adjacent text at native pitch"
            );
            let emoji = set.glyphs.iter().find(|glyph| glyph.ch == '👩').unwrap();
            let at = set.glyphs.iter().find(|glyph| glyph.ch == '@').unwrap();
            assert!(at.cell >= emoji.cell + emoji.cols);
        }
    }
    for (email, expected) in [
        ("123456e\u{0301}x@example.com", "123456e\u{0301}…"),
        ("12345👩‍💻x@example.com", "12345👩‍💻…"),
    ] {
        let mut account = [Segment::plain(ACCOUNT_MARK, email)];
        assert!(shorten_subscription_account(&mut account, 8));
        assert_eq!(account[0].text, expected);
        assert_eq!(text_width(&account[0].text), 8);
    }
}

#[test]
fn a_narrow_rule_gives_way_title_first_and_the_model_last() {
    let facts = narrow_rule_facts();
    let texts = rung_texts;
    assert_eq!(
        walk_down(&facts),
        [
            vec![
                "\u{25C6} Opus 5.5 xhigh",
                "\u{2302} ~/src/github.com/aterm",
                "\u{2387} main"
            ],
            vec![
                "\u{25C6} Opus 5.5 xhigh",
                "\u{2302} \u{2026}/aterm",
                "\u{2387} main"
            ],
            vec!["\u{25C6} Opus 5.5 xhigh", "\u{2302} \u{2026}/aterm"],
            vec!["\u{25C6} Opus 5.5 xhigh"],
            vec!["\u{25C6} Opus 5.5"],
            Vec::<&str>::new(),
        ]
        .map(|v| v.into_iter().map(str::to_owned).collect::<Vec<_>>())
    );
    let walled = narrow_rule_walled();
    let model = "\u{25C6} Opus 5.5 xhigh";
    let path = "\u{2302} ~/src/github.com/aterm";
    let short = "\u{2302} \u{2026}/aterm";
    let branch = "\u{2387} main";
    let wall = "\u{29D7} 7d \u{267B} Oct 1 at 9am";
    assert_eq!(
        walk_down(&walled),
        [
            vec![
                wall,
                model,
                path,
                branch,
                "\u{03A3} 7h52m \u{2191}48M \u{2193}310k"
            ],
            vec![wall, model, path, branch, "\u{03A3} 7h52m"],
            vec![wall, model, path, branch],
            vec![wall, model, short, branch],
            vec![wall, model, short],
            vec![wall, model],
            vec![wall, "\u{25C6} Opus 5.5"],
            vec![wall],
            vec![model],
            vec!["\u{25C6} Opus 5.5"],
            Vec::<&str>::new(),
        ]
        .map(|v| v.into_iter().map(str::to_owned).collect::<Vec<_>>())
    );
    let lights = Some(LightsWidth {
        chips: 13,
        title: 35,
        lean: [0; LEAN_FORMS],
        short: 20,
        reason: false,
    });
    let steps: Vec<(usize, TitleFit, bool)> = (0..=120)
        .rev()
        .map(|room| {
            let fit = fit_rule(&facts, room, lights);
            assert!(fit.width(lights) <= room, "{room}: {fit:?}");
            (fit.segments.len(), fit.title, fit.chips)
        })
        .fold(Vec::new(), |mut acc, step| {
            if acc.last() != Some(&step) {
                acc.push(step);
            }
            acc
        });
    assert_eq!(
        steps,
        [
            (3, TitleFit::Full, true),
            (3, TitleFit::Short, true),
            (3, TitleFit::None, true),
            (2, TitleFit::None, true),
            (1, TitleFit::None, true),
            (1, TitleFit::None, false),
            (0, TitleFit::None, false),
        ]
    );
    let leaner = lights.map(|l| LightsWidth {
        lean: [32, 29, 26, 23],
        ..l
    });
    let steps: Vec<(usize, TitleFit, bool)> = (0..=120)
        .rev()
        .map(|room| {
            let fit = fit_rule(&facts, room, leaner);
            assert!(fit.width(leaner) <= room, "{room}: {fit:?}");
            (fit.segments.len(), fit.title, fit.chips)
        })
        .fold(Vec::new(), |mut acc, step| {
            if acc.last() != Some(&step) {
                acc.push(step);
            }
            acc
        });
    assert_eq!(
        steps[..7],
        [
            (3, TitleFit::Full, true),
            (3, TitleFit::Lean(0), true),
            (3, TitleFit::Lean(1), true),
            (3, TitleFit::Lean(2), true),
            (3, TitleFit::Lean(3), true),
            (3, TitleFit::Short, true),
            (3, TitleFit::None, true),
        ]
    );
    let titles = |lean: [usize; LEAN_FORMS]| -> Vec<TitleFit> {
        let lights = lights.map(|l| LightsWidth { lean, ..l });
        (0..=120)
            .rev()
            .map(|room| fit_rule(&facts, room, lights).title)
            .fold(Vec::new(), |mut acc, title| {
                if acc.last() != Some(&title) {
                    acc.push(title);
                }
                acc
            })
    };
    assert_eq!(
        titles([0, 25, 0, 0]),
        [
            TitleFit::Full,
            TitleFit::Lean(1),
            TitleFit::Short,
            TitleFit::None
        ]
    );
    assert_eq!(
        titles([0, 25, 0, 22]),
        [
            TitleFit::Full,
            TitleFit::Lean(1),
            TitleFit::Lean(3),
            TitleFit::Short,
            TitleFit::None
        ]
    );
    let owners = FooterFacts {
        path: Some("~/aterm".into()),
        ..facts.clone()
    };
    let fit = fit_rule(&owners, rule_room(&(0..80)), lights);
    assert_eq!(fit.segments, segments(&owners), "{fit:?}");
    assert!(fit.chips && fit.title != TitleFit::Full, "{fit:?}");
    let reason = lights.map(|l| LightsWidth {
        reason: true,
        title: 50,
        short: 35,
        ..l
    });
    let steps: Vec<(Vec<String>, TitleFit, bool)> = (0..=130)
        .rev()
        .map(|room| {
            let fit = fit_rule(&facts, room, reason);
            assert!(fit.width(reason) <= room, "{room}: {fit:?}");
            (texts(&fit), fit.title, fit.chips)
        })
        .fold(Vec::new(), |mut acc, step| {
            if acc.last() != Some(&step) {
                acc.push(step);
            }
            acc
        });
    let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let (model_effort, cut, branch, path) = (
        "\u{25C6} Opus 5.5 xhigh",
        "\u{2302} \u{2026}/aterm",
        "\u{2387} main",
        "\u{2302} ~/src/github.com/aterm",
    );
    assert_eq!(
        steps,
        [
            (owned(&[model_effort, path, branch]), TitleFit::Full, true),
            (owned(&[model_effort, path, branch]), TitleFit::Short, true),
            (owned(&[model_effort, cut, branch]), TitleFit::Short, true),
            (owned(&[model_effort, cut]), TitleFit::Short, true),
            (owned(&[model_effort]), TitleFit::Short, true),
            (owned(&[model_effort, path, branch]), TitleFit::None, true),
            (owned(&[model_effort, cut, branch]), TitleFit::None, true),
            (owned(&[model_effort, cut]), TitleFit::None, true),
            (owned(&[model_effort]), TitleFit::None, true),
            (owned(&["\u{25C6} Opus 5.5"]), TitleFit::None, true),
            (owned(&["\u{25C6} Opus 5.5"]), TitleFit::None, false),
            (Vec::new(), TitleFit::None, false),
        ]
    );
    let stop = Some(LightsWidth {
        chips: 22,
        title: 47,
        lean: [0; LEAN_FORMS],
        short: 30,
        reason: true,
    });
    let fit = fit_rule(&owners, rule_room(&(0..80)), stop);
    assert_eq!(
        (texts(&fit), fit.title),
        (owned(&[model_effort]), TitleFit::Short),
        "{fit:?}"
    );
    let hover = stop.map(|l| LightsWidth { reason: false, ..l });
    let fit = fit_rule(&owners, rule_room(&(0..80)), hover);
    assert_eq!(
        (fit.segments, fit.title),
        (segments(&owners), TitleFit::None)
    );
    let dot = Some(LightsWidth {
        chips: 6,
        title: 0,
        lean: [0; LEAN_FORMS],
        short: 0,
        reason: false,
    });
    let fit = fit_rule(&facts, 9, dot);
    assert!(fit.chips && fit.segments.is_empty(), "{fit:?}");
    for room in 0..120 {
        let chips = chips_room(&facts, room);
        for w in [chips, chips + 1] {
            let lights = Some(LightsWidth {
                chips: w,
                title: 0,
                lean: [0; LEAN_FORMS],
                short: 0,
                reason: false,
            });
            let fit = fit_rule(&facts, room, lights);
            let model_fits = !fit_rule(&facts, room, None).segments.is_empty();
            if w == chips && w > 0 {
                assert!(fit.chips, "{room}: {w} chips fit");
            }
            if w == chips + 1 && model_fits {
                assert!(
                    !fit.chips || fit.segments.is_empty(),
                    "{room}: {w} is past the room"
                );
            }
        }
    }
}

fn rung_texts(fit: &RuleFit) -> Vec<String> {
    fit.segments
        .iter()
        .map(|s| format!("{} {}", s.mark, s.text))
        .collect()
}

fn walk_down(facts: &FooterFacts) -> Vec<Vec<String>> {
    let full = segments_width(&segments(facts)) + 2;
    let mut seen: Vec<Vec<String>> = Vec::new();
    for room in (0..=full).rev() {
        let fit = fit_rule(facts, room, None);
        assert!(fit.width(None) <= room, "{room}: {fit:?}");
        let texts = rung_texts(&fit);
        if seen.last() != Some(&texts) {
            seen.push(texts);
        }
    }
    seen
}

fn narrow_rule_facts() -> FooterFacts {
    FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/src/github.com/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    }
}

fn narrow_rule_walled() -> FooterFacts {
    use crate::harness::session_usage::{ModelTokens, More, UsageFacts, Wall};
    FooterFacts {
        usage: UsageFacts::of(
            (
                vec![ModelTokens {
                    label: "opus".into(),
                    input: 48_000_000,
                    output: 310_000,
                }],
                More::default(),
            ),
            Some(Wall {
                label: "7d",
                resets: "Oct 1 at 9am".into(),
            }),
            None,
        )
        .map(|u| u.with_runtime(28_320_000)),
        ..narrow_rule_facts()
    }
}

fn narrow_rule_tokens() -> FooterFacts {
    let walled = narrow_rule_walled();
    FooterFacts {
        usage: walled.usage.map(|mut usage| {
            usage.wall = None;
            usage
        }),
        ..walled
    }
}

#[test]
fn the_account_costs_only_the_tokens() {
    let plain = narrow_rule_walled();
    let named = |label: &str, facts: &FooterFacts| FooterFacts {
        account: Some(label.into()),
        ..facts.clone()
    };
    let facts = named("work-2", &plain);
    let walk = walk_down(&facts);
    let fewer = walk
        .iter()
        .find(|rung| rung.len() < walk[0].len())
        .expect("a narrower rung");
    assert_eq!(fewer.as_slice(), &walk[0][..5], "{fewer:?}");
    let bare = narrow_rule_facts();

    let strip = |segments: &[Segment], marks: &[char]| -> Vec<Segment> {
        segments
            .iter()
            .filter(|s| !marks.contains(&s.mark))
            .cloned()
            .collect()
    };
    for label in ["a", "work-2", "a-label-of-twenty-six-cells"] {
        for unnamed in [&plain, &bare, &narrow_rule_tokens()] {
            let facts = named(label, unnamed);
            let mut shown = 0;
            for room in 0..=160 {
                let fit = fit_rule(&facts, room, None);
                let base = fit_rule(unnamed, room, None);
                let kept = strip(&base.segments, &[USAGE_MARK]);
                match fit.segments.iter().find(|s| s.mark == ACCOUNT_MARK) {
                    Some(s) => {
                        assert_eq!(s.text, label, "{room}: {fit:?}");
                        assert_eq!(
                            strip(&fit.segments, &[USAGE_MARK, ACCOUNT_MARK]),
                            kept,
                            "{label} {room}: it took the tokens' room alone"
                        );
                        shown += 1;
                    }
                    None => {
                        assert_eq!(fit, base, "{label} {room}");
                        if let Some(at) = kept.iter().position(|s| s.mark == MODEL_MARK) {
                            let mut with = kept.clone();
                            with.insert(at + 1, Segment::plain(ACCOUNT_MARK, label));
                            let with = RuleFit {
                                segments: with,
                                ..fit.clone()
                            };
                            assert!(with.width(None) > room, "{label} {room}: {fit:?}");
                        }
                    }
                }
            }
            assert!(shown > 0, "{label}: a wide rule shows it");
        }
    }

    let unnamed_of = |walk: &[Vec<String>]| -> Vec<Vec<String>> {
        walk.iter()
            .filter(|rung| !rung.iter().any(|s| s.starts_with(ACCOUNT_MARK)))
            .cloned()
            .collect()
    };
    let pinned = walk_down(&plain);
    let mut rest = pinned[1..].iter();
    for rung in unnamed_of(&walk) {
        assert!(rest.any(|r| *r == rung), "{rung:?} out of order: {walk:?}");
    }
    assert_eq!(
        unnamed_of(&walk_down(&named("work-2", &bare))),
        walk_down(&bare)
    );
}

#[test]
fn subscription_account_and_weekly_outlast_workspace_details_without_overflow() {
    let facts = FooterFacts {
        subscription: Some(super::super::profile::SubscriptionDisplay {
            email: Some("owner@example.com".into()),
            plan: Some("Max 5x".into()),
            weekly: Some("weekly 29% left".into()),
            ..Default::default()
        }),
        ..narrow_rule_facts()
    };
    let whole = segments(&facts);
    assert!(
        whole
            .iter()
            .any(|s| s.mark == PLAN_MARK && s.text == "Max 5x")
    );
    let short = fit_rule(&facts, 45, None);
    assert!(
        short
            .segments
            .iter()
            .any(|s| s.mark == ACCOUNT_MARK && s.text == "owner@example.com")
    );
    assert!(
        short
            .segments
            .iter()
            .any(|s| s.mark == WEEKLY_MARK && s.text == "weekly 29% left")
    );
    assert!(
        !short
            .segments
            .iter()
            .any(|s| s.mark == PATH_MARK || s.mark == BRANCH_MARK)
    );
    for room in 0..=180 {
        let fit = fit_rule(&facts, room, None);
        assert!(fit.width(None) <= room, "{room}: {fit:?}");
    }
    let missing = FooterFacts {
        subscription: Some(super::super::profile::SubscriptionDisplay::default()),
        ..FooterFacts::default()
    };
    let shown = segments(&missing);
    assert!(shown.iter().any(|s| s.text == "account unavailable"));
    assert!(shown.iter().any(|s| s.text == "weekly unavailable"));
}

/// A WEEKLY FIGURE THE FOOTER HAS NOT GOT SAYS WHY: a few words per reason where the read
/// told one, `weekly unavailable` where none was told (a refused profile read), and a
/// standing 7d wall over every reason. Each reason, like an aged figure, stays beside the
/// account and the controls on an 80-column pane, in its narrow form (`weekly ?`, an aged
/// figure's `61.5% left · 59m`) where the controls take the room; and on any rule where
/// `weekly unavailable` beside the shortest account fits, the week stays.
#[test]
fn a_missing_weekly_figure_names_its_reason_beside_the_controls() {
    use super::super::profile::{SubscriptionDisplay, WeeklyGap};
    use super::super::tap::Refusal;
    let with = |gap: Option<WeeklyGap>, weekly: Option<&str>| FooterFacts {
        subscription: Some(SubscriptionDisplay {
            email: Some("owner@example.com".into()),
            plan: Some("Max 20x".into()),
            weekly: weekly.map(str::to_owned),
            weekly_gap: gap,
            ..Default::default()
        }),
        ..narrow_rule_facts()
    };
    let weekly = |facts: &FooterFacts| {
        segments(facts)
            .into_iter()
            .find(|s| s.mark == WEEKLY_MARK)
            .map(|s| s.text)
    };
    for (gap, words) in [
        (WeeklyGap::TapOff, "weekly ? \u{b7} tap off"),
        (
            WeeklyGap::Unoffered(Refusal::Home),
            "weekly ? \u{b7} untapped",
        ),
        (WeeklyGap::Untapped, "weekly ? \u{b7} until relaunch"),
        (WeeklyGap::Unrestarted, "weekly ? \u{b7} untapped"),
        (WeeklyGap::NoReply, "weekly ? \u{b7} no reply yet"),
        (WeeklyGap::Stale, "weekly ? \u{b7} no reply 1h"),
    ] {
        assert_eq!(weekly(&with(Some(gap), None)).as_deref(), Some(words));
    }
    assert_eq!(
        weekly(&with(None, None)).as_deref(),
        Some("weekly unavailable"),
        "no reason told"
    );
    let walled = FooterFacts {
        subscription: with(Some(WeeklyGap::NoReply), None).subscription,
        ..narrow_rule_walled()
    };
    assert_eq!(weekly(&walled).as_deref(), Some("weekly 0% left"));
    let aged = "weekly 61.5% left \u{b7} 59m ago";
    let room = rule_room(&(0..80));
    for (facts, narrow) in [
        (with(Some(WeeklyGap::NoReply), None), "weekly ?"),
        (with(Some(WeeklyGap::Untapped), None), "weekly ?"),
        (with(None, Some(aged)), "61.5% left \u{b7} 59m"),
    ] {
        let words = weekly(&facts).expect("a weekly segment");
        let mut pair = [
            Segment::plain(ACCOUNT_MARK, "owner@example.com"),
            Segment::plain(WEEKLY_MARK, narrow),
        ];
        shorten_subscription_account(&mut pair, 8);
        let reserved = rule_width(&pair, TitleFit::None, false, None);
        assert_eq!(
            chips_room(&facts, room),
            room - (reserved + RULE_SEP + 2),
            "{words}: the controls get the room its narrow form frees"
        );
        for chips in [1, 3, 6, chips_room(&facts, room)] {
            let lights = Some(LightsWidth {
                chips,
                title: 50,
                lean: [0; LEAN_FORMS],
                short: 20,
                reason: true,
            });
            let fit = fit_rule(&facts, room, lights);
            assert!(fit.width(lights) <= room, "{chips}: {fit:?}");
            assert!(fit.chips, "{chips}: controls stay available");
            assert!(
                fit.segments
                    .iter()
                    .any(|s| s.mark == WEEKLY_MARK && (s.text == words || s.text == narrow)),
                "{chips}: {words} stays: {fit:?}"
            );
        }
        let mut old_floor = [
            Segment::plain(ACCOUNT_MARK, "owner@example.com"),
            Segment::plain(WEEKLY_MARK, "weekly unavailable"),
        ];
        shorten_subscription_account(&mut old_floor, 8);
        let floor = rule_width(&old_floor, TitleFit::None, false, None);
        for room in 0..=180 {
            let fit = fit_rule(&facts, room, None);
            assert!(fit.width(None) <= room, "{room}: {fit:?}");
            if room >= floor {
                assert!(
                    fit.segments
                        .iter()
                        .any(|s| s.mark == WEEKLY_MARK && (s.text == words || s.text == narrow)),
                    "{room}: the old floor held the week, and still does: {fit:?}"
                );
            }
        }
    }
}

#[test]
fn subscription_chip_budget_keeps_account_and_weekly_beside_real_gui_faces() {
    for email in ["owner@example.com", "使用者與工作帳號在公司@example.com"] {
        let facts = FooterFacts {
            subscription: Some(super::super::profile::SubscriptionDisplay {
                email: Some(email.into()),
                plan: Some("Max 5x".into()),
                weekly: Some("weekly 29% left".into()),
                ..Default::default()
            }),
            ..narrow_rule_facts()
        };
        for columns in [80, 100, 120] {
            let room = rule_room(&(0..columns));
            let budget = chips_room(&facts, room);
            for chips in [1, 3, 6, 8, 10, 14, budget] {
                assert!(chips <= budget, "{columns}: {chips} > {budget}");
                for reason in [false, true] {
                    let lights = Some(LightsWidth {
                        chips,
                        title: 50,
                        lean: [0; LEAN_FORMS],
                        short: 20,
                        reason,
                    });
                    let fit = fit_rule(&facts, room, lights);
                    assert!(fit.width(lights) <= room, "{columns}/{chips}: {fit:?}");
                    assert!(fit.chips, "{columns}/{chips}: controls stay available");
                    assert!(
                        fit.segments.iter().any(|s| s.mark == ACCOUNT_MARK),
                        "{columns}/{chips}: selected account remains visible: {fit:?}"
                    );
                    assert!(
                        fit.segments
                            .iter()
                            .any(|s| s.mark == WEEKLY_MARK && s.text == "weekly 29% left"),
                        "{columns}/{chips}: weekly quota remains visible: {fit:?}"
                    );
                    if chips == budget {
                        let account = fit
                            .segments
                            .iter()
                            .find(|s| s.mark == ACCOUNT_MARK)
                            .unwrap();
                        assert!(text_width(&account.text) <= 8, "{fit:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn subscription_keeps_a_standing_limit_and_controls_before_secondary_facts() {
    let facts = FooterFacts {
        subscription: Some(super::super::profile::SubscriptionDisplay {
            email: Some("owner@example.com".into()),
            plan: Some("Max 5x".into()),
            weekly: Some("weekly 0% left".into()),
            ..Default::default()
        }),
        ..narrow_rule_walled()
    };
    let wall: Vec<_> = segments(&facts)
        .into_iter()
        .filter(|s| s.mark == WALL_MARK)
        .collect();
    assert_eq!(wall.len(), 1);
    for chips in [1, 3, 8, 10, 14] {
        let lights = Some(LightsWidth {
            chips,
            title: 0,
            lean: [0; LEAN_FORMS],
            short: 0,
            reason: true,
        });
        let room = rule_width(&wall, TitleFit::None, true, lights);
        let fit = fit_rule(&facts, room, lights);
        assert!(fit.chips, "{room}/{chips}: controls remain usable");
        assert_eq!(
            fit.segments, wall,
            "{room}/{chips}: the limit is actionable"
        );
        assert!(fit.width(lights) <= room);
    }
}

#[test]
fn subscription_control_glyph_outlasts_the_model_when_essentials_cannot_fit() {
    let facts = FooterFacts {
        subscription: Some(super::super::profile::SubscriptionDisplay::default()),
        ..narrow_rule_facts()
    };
    for chips in [1, 3, 8, 10, 14] {
        let lights = Some(LightsWidth {
            chips,
            title: 0,
            lean: [0; LEAN_FORMS],
            short: 0,
            reason: true,
        });
        let room = chips + 2;
        assert!(chips <= chips_room(&facts, room));
        let fit = fit_rule(&facts, room, lights);
        assert!(fit.chips, "{room}/{chips}: model must not hide controls");
        assert!(fit.segments.is_empty());
        assert!(fit.width(lights) <= room);
    }
}

#[test]
fn a_reason_and_the_upgrade_note_take_the_accounts_room() {
    let plain = narrow_rule_walled();
    let facts = FooterFacts {
        account: Some("work-2".into()),
        ..plain.clone()
    };
    let whole = segments(&facts);
    let top: Vec<Segment> = whole
        .iter()
        .filter(|s| s.mark == WALL_MARK || s.mark == MODEL_MARK)
        .cloned()
        .collect();
    assert_eq!(top.len(), 2);
    let named = |fit: &RuleFit| fit.segments.iter().any(|s| s.mark == ACCOUNT_MARK);
    let with_account = |fit: &RuleFit| {
        let mut fit = fit.clone();
        let at = fit.segments.iter().position(|s| s.mark == MODEL_MARK);
        fit.segments
            .insert(at.expect("the model") + 1, whole[2].clone());
        fit
    };
    assert_eq!(whole[2].mark, ACCOUNT_MARK);
    let reason = Some(LightsWidth {
        chips: 13,
        title: 50,
        lean: [0; LEAN_FORMS],
        short: 35,
        reason: true,
    });
    let (mut beside, mut instead): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    for room in (0..=240).rev() {
        let fit = fit_rule(&facts, room, reason);
        assert!(fit.width(reason) <= room, "{room}: {fit:?}");
        if fit.title == TitleFit::None {
            continue;
        }
        assert!(
            top.iter().all(|s| fit.segments.contains(s)),
            "{room}: a reason never takes the wall's or the model's room: {fit:?}"
        );
        if named(&fit) {
            beside.push(room);
        } else {
            assert_eq!(fit.title, TitleFit::Short, "{room}: {fit:?}");
            assert!(
                with_account(&fit).width(reason) > room,
                "{room}: only where the two do not fit together: {fit:?}"
            );
            instead.push(room);
        }
    }
    assert!(!instead.is_empty(), "the reason takes the account's room");
    assert!(
        instead.iter().max() < beside.iter().max(),
        "a wide rule shows both: {instead:?} {beside:?}"
    );
    let hover = reason.map(|l| LightsWidth { reason: false, ..l });
    for room in 0..=240 {
        let fit = fit_rule(&facts, room, hover);
        assert!(
            fit.title == TitleFit::None || fit.segments == whole,
            "{room}: {fit:?}"
        );
    }
    for &room in &instead {
        let fit = fit_rule(&plain, room, reason);
        assert_ne!(fit.title, TitleFit::None, "{room}: {fit:?}");
    }

    let note = UpgradeWords {
        whole: "Claude Code 2.1.286 is ready \u{b7} waits while you use this tab".into(),
        short: "2.1.286 waits while you use this tab".into(),
    };
    let noted = FooterFacts {
        upgrade: Some(note.clone()),
        ..facts.clone()
    };
    let (mut beside, mut instead): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    for room in (0..=240).rev() {
        let fit = fit_rule(&noted, room, None);
        assert!(fit.width(None) <= room, "{room}: {fit:?}");
        let Some(at) = fit.segments.iter().position(|s| s.mark == UPGRADE_MARK) else {
            continue;
        };
        assert_eq!(at, 0, "{room}: the note leads: {fit:?}");
        assert!(
            top.iter().all(|s| fit.segments.contains(s)),
            "{room}: the note never takes the wall's or the model's room: {fit:?}"
        );
        if named(&fit) {
            beside.push(room);
        } else {
            assert_eq!(fit.segments[0].text, note.short, "{room}: {fit:?}");
            assert!(
                with_account(&fit).width(None) > room,
                "{room}: only where the two do not fit together: {fit:?}"
            );
            instead.push(room);
        }
    }
    assert!(!instead.is_empty(), "the note takes the account's room");
    let wide = fit_rule(&noted, 240, None);
    assert_eq!(
        (wide.segments[0].mark, wide.segments[0].text.as_str()),
        (UPGRADE_MARK, note.whole.as_str()),
        "a wide rule leads with the whole note"
    );
    assert!(
        instead.iter().max() < beside.iter().max(),
        "a wide rule shows both: {instead:?} {beside:?}"
    );
    let unnamed = FooterFacts {
        upgrade: Some(note),
        ..plain
    };
    for &room in &instead {
        let fit = fit_rule(&unnamed, room, None);
        assert_eq!(
            fit.segments.first().map(|s| s.mark),
            Some(UPGRADE_MARK),
            "{room}: {fit:?}"
        );
    }
}

#[test]
fn the_launch_card_is_read_only_under_a_live_repl() {
    let fixture = |name: &str| -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-phase/src/fixtures")
            .join(name);
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect()
    };
    let card = launch_card(&fixture("claude-2.1.283-footer-bypass-idle.txt")).unwrap();
    assert_eq!(
        (
            card.version.as_str(),
            card.model.as_str(),
            card.effort.as_deref()
        ),
        ("2.1.283", "Opus 5.5", Some("xhigh"))
    );
    let relaunched = launch_card(&fixture("claude-2.1.283-inline-relaunch-repl-ready.txt"));
    assert_eq!(relaunched.map(|c| c.model).as_deref(), Some("Haiku 4.5"));
    for dead in [
        "claude-2.1.283-inline-relaunch-before-repl.txt",
        "claude-2.1.283-inline-relaunch-repl-half-drawn.txt",
        "claude-2.1.283-launch-repl-half-drawn.txt",
        "claude-2.1.283-effort-switch.txt",
    ] {
        assert_eq!(launch_card(&fixture(dead)), None, "{dead}");
    }
    assert!(
        aterm_phase::launch_card(&fixture("claude-2.1.283-inline-relaunch-before-repl.txt"))
            .is_some()
    );
    let manual = fixture("claude-2.1.283-footer-manual.txt");
    assert_eq!(
        live_repl_row(&manual),
        mode_row(&manual),
        "default mode's row is a mode row"
    );
    assert_eq!(
        launch_card(&manual).map(|c| c.model).as_deref(),
        Some("Opus 5.5")
    );
    let bypass = fixture("claude-2.1.283-footer-bypass-idle.txt");
    let at = mode_row(&bypass).expect("the measured mode row");
    for (bare, live) in [
        ("  ? for shortcuts", true),
        ("  esc to clear", true),
        ("  esc to interrupt \u{00B7} \u{2190} for agents", true),
        ("  Press Ctrl-C again to exit", false),
        ("  /clear          Clear conversation history", false),
        ("  ! for shell mode", false),
    ] {
        let mut rows = bypass.clone();
        rows[at] = bare.to_owned();
        assert_eq!(live_repl_row(&rows).is_some(), live, "{bare:?}");
        assert_eq!(
            launch_card(&rows).map(|c| c.model),
            live.then(|| "Opus 5.5".to_owned()),
            "{bare:?}"
        );
        assert_eq!(mode_row(&rows), None, "{bare:?} is no mode row");
    }
}

#[test]
fn the_context_indicator_has_gone_only_from_an_empty_slot() {
    let rule = "─".repeat(120);
    let screen = |caret: &str, under: &[String]| -> Vec<String> {
        let mut rows = vec![
            "⏺ Hi! I am the local lab stub, not a model. (reply 1)".to_string(),
            String::new(),
            "✻ Brewed for 0s · done 12:48 AM".to_string(),
            String::new(),
            rule.clone(),
            caret.to_string(),
            rule.clone(),
        ];
        rows.extend(under.iter().cloned());
        rows.extend([String::new(), String::new()]);
        rows
    };
    let slot = |left: &str, end: &str| aterm_phase::prompt::fixtures::right_aligned(left, end);
    let rest = "  ⏸ manual mode on · ? for shortcuts";
    for (what, rows) in [
        ("after /compact", screen("❯", &[slot(rest, "")])),
        (
            "an older build's pill",
            screen(
                "❯",
                &["  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string()],
            ),
        ),
        (
            "an older build's hint",
            screen("❯", &["  ? for shortcuts".to_string()]),
        ),
    ] {
        assert!(context_gone(&rows), "{what}: gone");
    }
    let mut above = screen("❯", &["  ? for shortcuts".to_string()]);
    above[3] = format!("{:>120}", "7% until auto-compact");
    let mut boxed = aterm_phase::prompt::fixtures::bash_one_row();
    let last = boxed.len() - 1;
    boxed[last] = slot(rest, "");
    for (what, rows) in [
        (
            "at rest",
            screen("❯", &[slot(rest, "0% until auto-compact")]),
        ),
        (
            "beside a draft",
            screen(
                "❯ /",
                &[slot("  ⏸ manual mode on", "0% until auto-compact")],
            ),
        ),
        (
            "through a turn",
            screen(
                "❯",
                &[slot(
                    "  ⏸ manual mode on · esc to interrupt",
                    "0% until auto-compact",
                )],
            ),
        ),
        ("above the composer", above),
        (
            "the effort notice",
            screen("❯ h", &[slot("  ⏸ manual mode on", "◐ medium · /effort")]),
        ),
        (
            "the / menu",
            screen(
                "❯ /",
                &[
                    "  ❯ /add-dir                    Add a new working directory".to_string(),
                    "    /autocompact                Set how full the context gets before auto-summarizing".to_string(),
                ],
            ),
        ),
        (
            "the @ menu",
            screen(
                "❯ @",
                &[
                    "    * claude (agent) – Catch-all for any task that doesn't fit a more specific age…".to_string(),
                    "    * Explore (agent) – Fast read-only search agent for locating code. Use it to fi…".to_string(),
                ],
            ),
        ),
        (
            "the shortcuts",
            screen(
                "❯",
                &[
                    "  ! for shell mode        double tap esc to clear input      ctrl + shift + _ to undo".to_string(),
                    "  / for commands          shift + tab to auto-accept edits   ctrl + z to suspend".to_string(),
                ],
            ),
        ),
        ("a box above the composer", boxed),
        (
            "no composer",
            aterm_phase::prompt::fixtures::rows(&["⏺ Done.", "", rest]),
        ),
    ] {
        assert!(!context_gone(&rows), "{what}: no sign it has gone");
    }
}

/// The gone rule on recorded screens of Claude Code 2.1.310 (`aterm_phase::phase::context_fixtures`):
/// only an empty slot after `/compact` is the indicator gone, at 120 columns and at 44; the `/`
/// and `@` menus, the shortcuts, the effort notice in the slot and shell mode never are, nor the
/// indicator a narrow pane moves after one space or onto a row of its own. One screen is
/// CONSTRUCTED, not recorded (no notice was seen there at 44 columns): the 44-column screen with
/// the indicator's own row swapped for the effort notice, which is the slot taken.
#[test]
fn the_context_indicator_has_gone_on_recorded_screens() {
    use aterm_phase::phase::context_fixtures as f;
    let screen = aterm_phase::prompt::fixtures::screen;
    for (what, text) in [
        ("after /compact", f::COMPACTED),
        ("after /compact at 44 columns", f::COMPACTED_44COL),
    ] {
        assert!(context_gone(&screen(text)), "{what}: gone");
    }
    let mut notice_under = screen(f::MODE_ROW_44COL);
    let at = notice_under
        .iter()
        .rposition(|row| row.ends_with("7% until auto-compact"))
        .expect("the indicator's own row");
    notice_under[at] = format!("{:>42}", "◐ medium · /effort");
    for (what, rows) in [
        ("the indicator", screen(f::MODE_ROW_7)),
        ("the indicator after one space", screen(f::MODE_ROW_60COL)),
        (
            "the indicator on a row of its own",
            screen(f::MODE_ROW_44COL),
        ),
        ("the / menu", screen(f::SLASH_MENU)),
        ("the @ menu", screen(f::AT_MENU)),
        ("the shortcuts", screen(f::SHORTCUTS)),
        ("the effort notice", screen(f::COMPACTED_EFFORT)),
        ("shell mode", screen(f::SHELL_MODE)),
        ("a notice on a row of its own (constructed)", notice_under),
    ] {
        assert!(!context_gone(&rows), "{what}: no sign it has gone");
    }
}

#[test]
fn a_launch_card_fills_only_open_facts() {
    let card = aterm_phase::LaunchCard {
        version: "2.1.283".into(),
        model: "Opus 5.5".into(),
        effort: Some("xhigh".into()),
    };
    let open = FooterFacts {
        path: Some("~/aterm".into()),
        version: Some("2.1.283".into()),
        model_open: true,
        effort_open: true,
        ..FooterFacts::default()
    };
    let filled = open.filled_from(Some(&card));
    assert_eq!(
        (filled.model.as_deref(), filled.effort.as_deref()),
        (Some("Opus 5.5"), Some("xhigh"))
    );
    let chosen = FooterFacts {
        model: Some("Fable 5.1".into()),
        model_open: false,
        effort_open: false,
        ..open.clone()
    };
    assert_eq!(chosen.filled_from(Some(&card)), chosen, "a choice stands");
    let voided = FooterFacts {
        model_open: false,
        effort_open: false,
        ..open.clone()
    };
    assert_eq!(
        voided.filled_from(Some(&card)),
        voided,
        "unknown stays unknown"
    );
    let other_build = FooterFacts {
        version: Some("2.1.284".into()),
        ..open.clone()
    };
    assert_eq!(other_build.filled_from(Some(&card)), other_build);
    assert_eq!(open.filled_from(None), open);
    let switched = |to: &str| FooterFacts {
        model: Some(to.into()),
        model_open: false,
        effort_for: Some(to.into()),
        ..open.clone()
    };
    assert_eq!(
        switched("Opus 5.5")
            .filled_from(Some(&card))
            .effort
            .as_deref(),
        Some("xhigh"),
        "the card is the model switched to"
    );
    let away = switched("Sonnet 5");
    assert_eq!(
        away.filled_from(Some(&card)),
        away,
        "the card's xhigh was Opus 5.5's, not Sonnet 5's"
    );
    let open_fallback = FooterFacts {
        model: Some("Gemini 4 Argon".into()),
        effort: Some("high".into()),
        model_open: true,
        effort_open: true,
        ..open
    };
    assert_eq!(
        open_fallback.filled_from(None),
        open_fallback,
        "fallback model/effort stands when no screen card is admitted"
    );
    let card_override = open_fallback.filled_from(Some(&card));
    assert_eq!(
        (
            card_override.model.as_deref(),
            card_override.effort.as_deref()
        ),
        (Some("Opus 5.5"), Some("xhigh")),
        "an admitted screen card overrides an open fallback model/effort"
    );
}

#[test]
fn the_mode_row_is_found_under_the_composer_and_nowhere_else() {
    let rule = "\u{2500}".repeat(40);
    let quoted = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
    let screen: Vec<String> = vec![
        quoted.into(),
        String::new(),
        rule.clone(),
        "\u{276F} ".into(),
        rule.clone(),
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents".into(),
    ];
    assert_eq!(mode_row(&screen), Some(5));
    let no_frame: Vec<String> = vec![quoted.into(), "$ ".into()];
    assert_eq!(mode_row(&no_frame), None, "no composer, no footer");
    let with_statusline: Vec<String> = vec![
        rule.clone(),
        "\u{276F} ".into(),
        rule,
        "  my statusline".into(),
        "  \u{23F5}\u{23F5} auto mode on".into(),
    ];
    assert_eq!(
        mode_row(&with_statusline),
        Some(4),
        "a statusLine row may sit between"
    );
}

#[test]
fn the_last_cwd_is_the_newest_main_thread_rows() {
    let body = [
        format!(r#"{{"type":"user","cwd":"{}"}}"#, abs("/w")),
        format!(r#"{{"type":"assistant","cwd":"{}"}}"#, abs("/w/nested")),
        format!(
            r#"{{"type":"assistant","cwd":"{}","isSidechain":true}}"#,
            abs("/w/agent")
        ),
        r#"{"type":"summary","summary":"no directory here"}"#.to_owned(),
        r#"{"type":"user","cwd":"relative"}"#.to_owned(),
    ]
    .join("\n");
    assert_eq!(
        last_cwd(body.as_bytes()),
        Some(PathBuf::from(abs("/w/nested")))
    );
    assert_eq!(last_cwd(br#"{"type":"user"}"#), None);
    assert_eq!(last_cwd(b""), None);
}

#[test]
fn shell_cwds_reads_the_live_sessions_launched_in_the_directory() {
    let root = std::env::temp_dir().join(format!("aterm-footer-shell-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let claude = root.join("claude");
    let launch = root.join("w");
    std::fs::create_dir_all(claude.join("sessions")).expect("sessions");
    let session = |pid: u32, id: &str, cwd: &Path, last: &Path| {
        std::fs::write(
            claude.join("sessions").join(format!("{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{id}","cwd":"{}"}}"#,
                json(cwd)
            ),
        )
        .expect("session file");
        let project = claude.join("projects").join(project_slug(cwd));
        std::fs::create_dir_all(&project).expect("project dir");
        std::fs::write(
            project.join(format!("{id}.jsonl")),
            format!(
                "{}\n{}\n",
                format_args!(r#"{{"type":"user","cwd":"{}"}}"#, json(cwd)),
                format_args!(r#"{{"type":"assistant","cwd":"{}"}}"#, json(last)),
            ),
        )
        .expect("transcript");
    };
    let me = std::process::id();
    assert!(shell_cwds(&claude, &launch).is_empty());
    session(me, "a-live", &launch, &launch.join("nested"));
    assert_eq!(shell_cwds(&claude, &launch), [launch.join("nested")]);
    assert!(shell_cwds(&claude, &root.join("elsewhere")).is_empty());
    session(999_999_990, "b-dead", &launch, &root.join("dead"));
    assert_eq!(shell_cwds(&claude, &launch), [launch.join("nested")]);
    session(me, "a-live", &launch, &launch);
    assert!(shell_cwds(&claude, &launch).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// One transcript row stamped `at`, for the survey's excerpt reads.
#[cfg(unix)]
fn excerpt_row(kind: &str, at: &str, message: &str) -> String {
    format!(r#"{{"type":"{kind}","timestamp":"{at}","message":{message}}}"#)
}

/// THE SURVEY'S EXCERPTS ride the tail read: a fresh read finds the prompt,
/// the newest words and the call in flight; appends move them (the call's
/// result ends it, a new prompt goes first) and are read as appends, not
/// afresh.
#[cfg(unix)]
#[test]
fn the_excerpts_ride_the_tail_read_and_its_appends() {
    let root = cache_test_dir("excerpts");
    let path = root.join("session.jsonl");
    let start = crate::harness::upgrade_models::parse_utc("2026-10-15T10:00:00Z").unwrap();
    let at = "2026-10-15T10:01:00Z";
    let append = |text: String| {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    };
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            excerpt_row("user", at, r#"{"content":"wire the survey"}"#),
            excerpt_row(
                "assistant",
                at,
                r#"{"id":"m1","model":"claude-opus-5-5","content":[{"type":"text","text":"Running the tests."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"targo test"}}]}"#
            ),
        ),
    )
    .unwrap();
    let mut cache = TailCache::default();
    assert!(cache.excerpts().is_none(), "nothing read yet");
    cache.read_since(&path, 4242, Some(start), start).unwrap();
    let e = cache.excerpts().cloned().expect("the excerpts");
    assert_eq!(e.prompts.len(), 1);
    assert_eq!(e.prompts[0].text, "wire the survey");
    assert_eq!(e.said.as_deref(), Some("Running the tests."));
    assert_eq!(e.doing.as_deref(), Some("Bash"));

    append(format!(
        "{}\n",
        excerpt_row(
            "user",
            at,
            r#"{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}"#
        )
    ));
    cache.read_since(&path, 4242, Some(start), start).unwrap();
    assert_eq!(cache.appends, 1, "read as an append");
    let e = cache.excerpts().cloned().expect("the excerpts");
    assert_eq!(e.doing, None, "the result answered the call");
    assert_eq!(e.said.as_deref(), Some("Running the tests."), "carried");

    append(format!(
        "{}\n",
        excerpt_row("user", at, r#"{"content":"now the client"}"#)
    ));
    cache.read_since(&path, 4242, Some(start), start).unwrap();
    assert_eq!(cache.appends, 2);
    let prompts: Vec<String> = cache
        .excerpts()
        .expect("the excerpts")
        .prompts
        .iter()
        .map(|p| p.text.clone())
        .collect();
    assert_eq!(prompts, ["now the client", "wire the survey"]);
    std::fs::remove_file(&path).unwrap();
    assert!(cache.read_since(&path, 4242, Some(start), start).is_none());
    assert!(cache.excerpts().is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// A COLD READ STOPS AT ITS BOUND: a prompt more than [`MAX_BACK_BYTES`]
/// above the end is not looked for, and the survey names none (`-`), while
/// the newest words and the call in flight are still read.
#[cfg(unix)]
#[test]
fn a_cold_back_read_past_its_bound_names_no_prompt() {
    let root = cache_test_dir("excerpts-cold");
    let start = crate::harness::upgrade_models::parse_utc("2026-10-15T10:00:00Z").unwrap();
    let at = "2026-10-15T10:01:00Z";
    let filler = |n: usize| {
        excerpt_row(
            "user",
            at,
            &format!(
                r#"{{"content":[{{"type":"tool_result","tool_use_id":"f{n}","content":"{}"}}]}}"#,
                "x".repeat(100 * 1024)
            ),
        )
    };
    let transcript = |rows: usize| {
        let mut body = excerpt_row("user", at, r#"{"content":"the only prompt"}"#);
        body.push('\n');
        for n in 0..rows {
            body.push_str(&filler(n));
            body.push('\n');
        }
        body.push_str(&excerpt_row(
            "assistant",
            at,
            r#"{"id":"m1","model":"claude-opus-5-5","content":[{"type":"text","text":"still at it"},{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}"#,
        ));
        body.push('\n');
        body
    };
    let read = |name: &str, rows: usize| {
        let path = root.join(name);
        let body = transcript(rows);
        std::fs::write(&path, &body).unwrap();
        let mut cache = TailCache::default();
        cache.read_since(&path, 4242, Some(start), start).unwrap();
        (
            body.len() as u64,
            cache.excerpts().cloned().expect("the excerpts"),
        )
    };
    let (len, past) = read("past.jsonl", 170);
    assert!(len > MAX_BACK_BYTES + TAIL_BYTES, "{len}");
    assert!(past.prompts.is_empty(), "past the bound: {past:?}");
    assert_eq!(past.said.as_deref(), Some("still at it"));
    assert_eq!(past.doing.as_deref(), Some("Bash"));
    let (len, under) = read("under.jsonl", 100);
    assert!(len < MAX_BACK_BYTES, "{len}");
    assert_eq!(under.prompts.len(), 1, "{under:?}");
    assert_eq!(under.prompts[0].text, "the only prompt");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_acts_label_is_drawn_whole_or_not_at_all_never_at_a_facts_cost() {
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    let label = Some(LightsWidth {
        chips: 1,
        title: "\u{2713} approved".chars().count() + 2,
        lean: [0; LEAN_FORMS],
        short: 0,
        reason: false,
    });
    let whole = segments(&facts);
    let mut drawn = Vec::new();
    for room in (0..=90).rev() {
        let fit = fit_rule(&facts, room, label);
        assert!(fit.width(label) <= room, "{room}: {fit:?}");
        assert_ne!(fit.title, TitleFit::Short, "{room}: no short form");
        if fit.title == TitleFit::Full {
            assert_eq!(fit.segments, whole, "{room}: beside the whole facts");
            drawn.push(room);
        }
        let bare = fit_rule(&facts, room, label.map(|l| LightsWidth { title: 0, ..l }));
        assert_eq!(fit.segments, bare.segments, "{room}: no fact gave way");
    }
    let least = *drawn.last().expect("drawn in a wide rule");
    assert_eq!(
        least,
        fit_rule(&facts, 90, label).width(label),
        "drawn down to its own width, and no narrower"
    );
    let hover = label.map(|l| LightsWidth { short: 5, ..l });
    let fit = fit_rule(&facts, least - 1, hover);
    assert_eq!(
        (fit.title, fit.segments == whole),
        (TitleFit::Short, true),
        "{fit:?}"
    );
    let fit = fit_rule(&facts, least - 1, label);
    assert_eq!(
        (fit.title, fit.segments == whole),
        (TitleFit::None, true),
        "{fit:?}"
    );
}

const SPLIT_PATH: &str = "/private/tmp/claude-502/-Users-person-aterm/\
    dd11ed0c-05cd-46e8-bf09-5b2786e48b6a/scratchpad/together/run-dir/proj-claude";

#[test]
fn a_title_stands_beside_the_facts_as_they_fit_without_it() {
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some(SPLIT_PATH.into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    let whole = segments(&facts);
    let chips = "\u{221e} \u{23f8} manual".chars().count();
    let label = LightsWidth {
        chips,
        title: "\u{2713} approved".chars().count() + 2,
        lean: [0; LEAN_FORMS],
        short: 0,
        reason: false,
    };
    let sentence =
        crate::supervise::mode_words::mode_words(crate::supervise::Autopilot::Cruise).chip;
    let whole_title = format!(
        "autopilot: cruise \u{2014} {sentence} \u{b7} click: land \u{b7} right-click: more"
    );
    let v1 = format!("autopilot: cruise \u{2014} {sentence} \u{b7} click: land");
    let clicks = "autopilot: cruise \u{b7} click: land \u{b7} right-click: more";
    let click = "autopilot: cruise \u{b7} click: land";
    let hover = LightsWidth {
        chips,
        title: whole_title.chars().count() + 2,
        lean: [
            0,
            v1.chars().count() + 2,
            clicks.chars().count() + 2,
            click.chars().count() + 2,
        ],
        short: "autopilot: cruise".chars().count() + 2,
        reason: false,
    };
    let untitled = |l: LightsWidth| LightsWidth {
        title: 0,
        lean: [0; LEAN_FORMS],
        short: 0,
        ..l
    };
    let forms = [
        TitleFit::Full,
        TitleFit::Lean(0),
        TitleFit::Lean(1),
        TitleFit::Lean(2),
        TitleFit::Lean(3),
        TitleFit::Short,
    ];
    for lights in [label, hover] {
        let mut beside_a_fallen_rung = false;
        for room in 0..=320 {
            let fit = fit_rule(&facts, room, Some(lights));
            assert!(
                fit.width(Some(lights)) <= room,
                "{room}: never past the rule's ends: {fit:?}"
            );
            let alone = fit_rule(&facts, room, Some(untitled(lights)));
            assert_eq!(
                (&fit.segments, fit.chips),
                (&alone.segments, alone.chips),
                "{room}: no fact gave way for the title"
            );
            let widest = forms
                .into_iter()
                .filter(|form| match form {
                    TitleFit::Full => true,
                    TitleFit::Lean(n) => lights.lean[usize::from(*n)] > 0,
                    TitleFit::Short => lights.short > 0,
                    TitleFit::None => false,
                })
                .find(|form| {
                    alone.chips && rule_width(&alone.segments, *form, true, Some(lights)) <= room
                })
                .unwrap_or(TitleFit::None);
            assert_eq!(fit.title, widest, "{room}: the widest form that fits");
            if lights.short == 0 {
                assert_ne!(fit.title, TitleFit::Short, "{room}: the label has none");
            }
            if fit.title != TitleFit::None && fit.segments != whole {
                beside_a_fallen_rung = true;
            }
        }
        assert!(
            beside_a_fallen_rung,
            "{lights:?}: a title stands beside a rung the room cut"
        );
    }
    let room = rule_room(&(0..120));
    let cut = |fit: &RuleFit| {
        fit.segments
            .iter()
            .any(|s| s.mark == PATH_MARK && s.text == "\u{2026}/proj-claude")
    };
    let lit = fit_rule(&facts, room, Some(label));
    assert!(
        cut(&lit) && lit.title == TitleFit::Full,
        "the label beside `…/proj-claude`: {lit:?}"
    );
    let hovered = fit_rule(&facts, room, Some(hover));
    assert!(
        cut(&hovered) && hovered.title == TitleFit::Lean(2),
        "the phrase and its clicks beside `…/proj-claude`: {hovered:?}"
    );
    assert!(
        (0..=320).any(|room| {
            let fit = fit_rule(&facts, room, Some(label));
            fit.chips && fit.title == TitleFit::None
        }),
        "the label gives way before the chips"
    );
    assert_eq!(
        fit_rule(&facts, 0, Some(label)),
        RuleFit {
            segments: Vec::new(),
            title: TitleFit::None,
            chips: false
        }
    );
}

/// THE STEP TAP'S SIDE OF ONE REGISTRY READ (S1 as
/// built): the lookup says whether the transcript's path is CERTAIN — the
/// project's own file, or the one file a scan finds — and a read that found
/// none tells an armed tap so, so the file Claude Code writes next is bound
/// from its first byte.
#[cfg(unix)]
#[test]
fn a_footer_read_tells_the_step_tap_what_it_found() {
    use super::super::steps::StepEvent;
    let root = cache_test_dir("tap-found");
    let (claude, transcript, two_pm) = walled_session(&root);
    let entry = session_of_pid(&claude, 4242, None).expect("the entry");
    assert_eq!(
        transcript_lookup(&claude, &entry),
        Some((transcript.clone(), true)),
        "the project's own file"
    );
    let projects = claude.join("projects");
    std::fs::rename(projects.join("-w-repo"), projects.join("-w-moved")).unwrap();
    let moved = projects.join("-w-moved/abc-1.jsonl");
    assert_eq!(
        transcript_lookup(&claude, &entry),
        Some((moved.clone(), true)),
        "the one file a scan finds"
    );
    std::fs::create_dir_all(projects.join("-w-other")).unwrap();
    std::fs::copy(&moved, projects.join("-w-other/abc-1.jsonl")).unwrap();
    assert!(
        transcript_lookup(&claude, &entry).is_some_and(|(_, certain)| !certain),
        "NEGATIVE CONTROL: two candidates"
    );
    std::fs::remove_dir_all(&projects).unwrap();
    std::fs::create_dir_all(projects.join("-w-repo")).unwrap();
    assert_eq!(transcript_lookup(&claude, &entry), None, "none yet");

    let utc = |_: Option<&str>, _: i64| Some(0);
    let read = |cache: &mut FooterCache| {
        let _ = folded(
            &claude,
            4242,
            Some(1_700_000_000),
            &mut TailCache::default(),
            cache,
            two_pm + 60,
            &utc,
        );
        cache
            .take_steps()
            .steps
            .into_iter()
            .map(|s| s.ev)
            .collect::<Vec<_>>()
    };
    let mut waited = FooterCache::default();
    waited.set_floor(0, None);
    assert!(read(&mut waited).is_empty() && !waited.steps_bound());
    append_to(&transcript, &served_row("m1", two_pm - 60));
    let evs = read(&mut waited);
    assert_eq!(evs.first(), Some(&StepEvent::Bind { len: 0 }), "{evs:?}");
    assert!(waited.steps_bound());
    let len = std::fs::metadata(&transcript).unwrap().len();
    let mut over = FooterCache::default();
    over.set_floor(0, None);
    assert_eq!(read(&mut over), vec![StepEvent::Bind { len }]);
    std::fs::remove_dir_all(root).unwrap();
}

fn under_rule(rest: &[&str]) -> (Vec<String>, usize) {
    let rule = "─".repeat(90);
    let mut rows = vec![
        "⏺ Both are running.".to_owned(),
        "  ⏺ main".to_owned(),
        String::new(),
        rule.clone(),
        "❯ ".to_owned(),
        rule,
    ];
    rows.extend(rest.iter().map(|r| (*r).to_owned()));
    let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the frame");
    assert_eq!(bottom, 5);
    (rows, bottom)
}

fn kinds(
    panel: &aterm_phase::agents_panel::AgentsPanel,
) -> Vec<(u16, aterm_phase::agents_panel::RowKind)> {
    panel.rows().iter().map(|r| (r.row, r.kind)).collect()
}

#[test]
fn the_agents_panel_is_found_under_the_footer() {
    use aterm_phase::agents_panel::RowKind::{Agent, Main, More, Workflow};
    let (rows, bottom) = under_rule(&[
        "  ⏵⏵ auto mode on · 1 shell · ← for agents · ↓ to manage                                                                      /rc active",
        "",
        "  ⏺ main",
        "  ◯ general-purpose  Searching the report's consumers in tests                                                  4m 28s · ↓ 115.4k tokens",
    ]);
    let panel = agents_panel_under(&rows, bottom).expect("the 2026-09-12 panel");
    assert_eq!(kinds(&panel), [(8, Main), (9, Agent)]);

    let fixture = |name: &str| -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-phase/src/fixtures")
            .join(name);
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    };
    for name in [
        "wait_bg.out",
        "wait_bg2.out",
        "claude-goal-active-suggestion.txt",
    ] {
        let rows = fixture(name);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect(name);
        let panel = agents_panel_under(&rows, bottom).expect(name);
        let top = usize::from(panel.top().unwrap());
        assert_eq!(kinds(&panel).len(), 1, "{name}");
        assert_eq!(panel.rows()[0].kind, Workflow, "{name}");
        assert!(top > bottom + 1 && top <= bottom + 3, "{name}");
    }

    let shape = |slot: &str| {
        under_rule(&[
            "  ⏵⏵ bypass permissions on · 14 shells · ← for agents · ↓ to manage",
            "",
            "  ⏺ main                                                    ↑ 2 more",
            "  ◯ general-purpose  Searching the report's consumers   4m 28s · ↓ 115.4k tokens",
            "❯ ◯ Explore  Reading the footer readers   12s · ↑ 900 tokens",
            "  └ ◯ Explore  Reading one file   1m 0s · ↓ 1.0k tokens",
            slot,
            "  ◯ nightly-sweep  ▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  1/4 · 3m02s · ↓ 12.4k tokens",
            "  ⧉  aterm Wrapper Brief · canvas",
            "  ◯ general-purpose  After the artifact bar   4m 28s · ↓ 115.4k tokens",
        ])
    };
    let (rows, bottom) = shape("  ↓ 3 more");
    let panel = agents_panel_under(&rows, bottom).unwrap();
    assert_eq!(
        kinds(&panel),
        [
            (8, Main),
            (9, Agent),
            (10, Agent),
            (11, Agent),
            (12, More),
            (13, Workflow)
        ]
    );
    assert!(panel.rows()[2].pointer.is_some());
    assert_eq!(panel.rows()[3].depth, 1);
    let (rows, bottom) = shape(" ");
    let panel = agents_panel_under(&rows, bottom).unwrap();
    assert_eq!(
        kinds(&panel),
        [
            (8, Main),
            (9, Agent),
            (10, Agent),
            (11, Agent),
            (13, Workflow)
        ]
    );
    let many: Vec<String> = (0..20)
        .map(|n| format!("  ◯ worker-{n}  Working   {n}s · ↓ 1.0k tokens"))
        .collect();
    let mut rest = vec!["  ? for shortcuts", "", "  ⏺ main"];
    rest.extend(many.iter().map(String::as_str));
    let (rows, bottom) = under_rule(&rest);
    let panel = agents_panel_under(&rows, bottom).unwrap();
    assert_eq!(
        panel.rows().len(),
        aterm_phase::agents_panel::PANEL_ROWS_MAX
    );
}

#[test]
fn the_agents_panel_is_read_nowhere_else() {
    let main = "  ⏺ main";
    let agent = "  ◯ general-purpose  Searching   4m 28s · ↓ 115.4k tokens";
    for rest in [
        vec!["", main, agent],
        vec!["  ? for shortcuts", "", agent],
        vec!["  ? for shortcuts", "", "", main, agent],
        vec!["  ? for shortcuts", ""],
        vec!["  ? for shortcuts"],
        vec![
            "  /agents        Manage agent configurations",
            "  /artifacts     Open an artifact",
            "  /bashes        List shells",
            "  /clear         Clear the conversation",
            "  /compact       Compact the conversation",
            main,
            agent,
        ],
        vec![main, agent],
    ] {
        let (rows, bottom) = under_rule(&rest);
        assert_eq!(agents_panel_under(&rows, bottom), None, "{rest:#?}");
    }
    let (rows, bottom) = under_rule(&[
        "  ⏵⏵ auto mode on · 3 shells · 2 monitors · ← for agents",
        "  ↓ to manage                                     /rc active",
        "",
        main,
        agent,
    ]);
    assert_eq!(
        agents_panel_under(&rows, bottom).map(|p| p.rows().len()),
        Some(2)
    );
}

#[test]
fn the_panel_is_found_under_a_manual_or_plan_mode_footer() {
    let main = "  ⏺ main";
    let agent = "  ◯ general-purpose  Searching   4m 28s · ↓ 115.4k tokens";
    for footer in [
        "  ⏸ manual mode on · ? for shortcuts · ← for agents",
        "  ⏸ plan mode on (shift+tab to cycle) · ← for agents",
        "  ⏸ manual mode on · ? for shortcuts · ← for agents  ◉ xhigh · /effort",
        "  ⏸ manual mo",
    ] {
        assert!(
            aterm_phase::agents_panel::parse_row(footer, aterm_phase::agents_panel::one_cell)
                .is_some(),
            "the precondition: {footer:?} reads as a panel row"
        );
        let (rows, bottom) = under_rule(&[footer, "", main, agent]);
        let panel = agents_panel_under(&rows, bottom).unwrap_or_else(|| panic!("{footer:?}"));
        assert_eq!(panel.rows().len(), 2, "{footer:?}");
    }
    let (rows, bottom) = under_rule(&[main, agent]);
    assert_eq!(agents_panel_under(&rows, bottom), None);
    let paused = "  ⏸ nightly-sweep  paused · usage limit resets at 3pm";
    let (rows, bottom) = under_rule(&["  ⏵⏵ auto mode on · ← for agents", "", main, paused]);
    assert_eq!(
        agents_panel_under(&rows, bottom).map(|p| kinds(&p)),
        Some(vec![
            (8, aterm_phase::agents_panel::RowKind::Main),
            (9, aterm_phase::agents_panel::RowKind::Workflow)
        ])
    );
}

#[test]
fn a_cut_pill_is_still_read_without_joining_it() {
    for row in [
        "  ⏵⏵ bypass permissi",
        "  ⏵⏵ bypass permissions on",
        "  ⏵",
        "  ⏸ plan mo",
    ] {
        assert!(is_cut_pill(row), "{row}");
    }
    for row in [
        "  ⏵⏵ bypass permissions onward",
        "  ⏵⏵ bypass perm1",
        "  ",
        "⏵⏵ bypass",
        "  ⏸ auto mode",
    ] {
        assert!(!is_cut_pill(row), "{row}");
    }
}

#[test]
fn the_bypass_and_manual_pills_are_found_to_relabel() {
    let (rows, bottom) =
        under_rule(&["  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents"]);
    let r = relabel_under(&rows, bottom).expect("the bypass pill");
    assert_eq!(
        (r.row, r.start, r.words, r.end, r.label),
        (bottom + 1, 2, 5, 26, YOLO_ON)
    );
    assert_eq!(r.run_end, rows[bottom + 1].chars().count());
    assert_eq!((r.shift(), r.label_end()), (17, 9));

    let row = "  ⏵⏵ bypass permissions on · 14 shells · ← for agents · ↓ to manage                    /rc active";
    let (rows, bottom) = under_rule(&[row]);
    let r = relabel_under(&rows, bottom).unwrap();
    let shells = row
        .find("14 shells")
        .map(|b| row[..b].chars().count())
        .unwrap();
    assert_eq!(
        r.run_end,
        row.find("manage")
            .map(|b| row[..b].chars().count() + 6)
            .unwrap()
    );
    let at = shells - r.shift();
    assert_eq!(
        r.grid_col(at),
        shells,
        "the glass's `14 shells` is Claude's"
    );
    for col in r.start..r.label_end() {
        assert_eq!(r.grid_col(col), col, "the label is over Claude's pill");
    }
    for col in r.run_end - r.shift()..r.run_end {
        assert_eq!(
            r.grid_col(col),
            r.start,
            "a blank the run left: the pill's cell"
        );
    }
    let notice = row.find("/rc").map(|b| row[..b].chars().count()).unwrap();
    assert_eq!(r.grid_col(notice), notice, "the right-hand notice stays");
    assert_eq!(r.grid_col(0), 0);

    let (rows, bottom) = under_rule(&["  ⏸ manual mode on · ? for shortcuts · ← for agents"]);
    let r = relabel_under(&rows, bottom).expect("the manual pill");
    assert_eq!((r.start, r.end, r.label, r.shift()), (2, 18, YOLO_OFF, 8));
    let (rows, bottom) = under_rule(&["  ⏵⏵ bypass permissions on"]);
    let r = relabel_under(&rows, bottom).unwrap();
    assert_eq!(r.run_end, r.end);
    assert_eq!(r.grid_col(r.label_end()), r.start);
}

#[test]
fn no_other_pill_or_row_is_relabelled() {
    for footer in [
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
        "  ⏸ plan mode on (shift+tab to cycle)",
        "  ⏵⏵ accept edits on · 1 shell",
        "  ⏵⏵ bypass permissi",
        "  ⏵⏵ bypass permissions only",
        "  ? for shortcuts · ⏵⏵ bypass permissions on",
        "  esc to interrupt",
        "? for shortcuts                                         Gemini 4 Argon · medium",
    ] {
        let (rows, bottom) = under_rule(&[footer]);
        assert_eq!(relabel_under(&rows, bottom), None, "{footer}");
    }
    let (mut rows, bottom) = under_rule(&["  ⏵⏵ auto mode on", "", "  ⏵⏵ bypass permissions on"]);
    rows[0] = "  ⏵⏵ bypass permissions on".into();
    assert_eq!(relabel_under(&rows, bottom), None);
}

#[test]
fn the_measured_2_1_304_panel_and_pill_are_found() {
    use aterm_phase::agents_panel::RowKind::{Agent, Main};
    let fixture = |name: &str| -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-phase/src/fixtures")
            .join(format!("claude-2.1.304-{name}.txt"));
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect()
    };
    for (name, pointed) in [
        ("agents-panel", None),
        ("agents-panel-later", None),
        ("agents-panel-main-selected", Some(0)),
        ("agents-panel-agent-selected", Some(1)),
    ] {
        let rows = fixture(name);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect(name);
        let panel = agents_panel_under(&rows, bottom).expect(name);
        let b = u16::try_from(bottom).unwrap();
        assert_eq!(kinds(&panel), [(b + 3, Main), (b + 4, Agent)], "{name}");
        let at = panel.rows().iter().position(|r| r.pointer.is_some());
        assert_eq!(at, pointed, "{name}");
        let r = relabel_under(&rows, bottom);
        assert_eq!(
            r.is_some(),
            pointed.is_none(),
            "{name}: the pill row or a hint"
        );
    }
    let rows = fixture("footer-bypass-idle-144col");
    let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
    let r = relabel_under(&rows, bottom).expect("the measured bypass pill");
    assert_eq!((r.start, r.end, r.label), (2, 26, YOLO_ON));
    assert_eq!(r.run_end, rows[bottom + 1].trim_end().chars().count());
    assert_eq!(
        agents_panel_under(&rows, bottom),
        None,
        "no agents, no panel"
    );
}

fn measured_2_1_304(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-phase/src/fixtures")
        .join(format!("claude-2.1.304-{name}.txt"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .skip(1)
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_pill_after_vim_inserts_indicator_is_read_as_its_mode() {
    for name in [
        "top-vim-insert-120col",
        "top-vim-normal-typed-120col",
        "top-refused-needs-workflows-vim-insert-120col",
    ] {
        let rows = measured_2_1_304(name);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect(name);
        let at = mode_row(&rows).expect(name);
        assert_eq!(at, bottom + 1, "{name}: the mode row under the rule");
        let row = &rows[at];
        assert!(row.starts_with("  -- INSERT -- \u{23F5}\u{23F5}"), "{name}");
        assert!(is_mode_row(row) && vim_insert(row), "{name}: {row:?}");
        assert_eq!(pill_indicator(row), Some("mode.auto"), "{name}");
        assert!(opens_with_pill_glyph(row), "{name}");
        assert_eq!(live_repl_row(&rows), Some(at), "{name}");
        let screen = crate::harness::lights::read_screen(&rows).expect(name);
        assert_eq!(
            (screen.mode, screen.pill, screen.vim_insert),
            (Some(crate::harness::lights::Mode::Auto), true, true),
            "{name}: the lights"
        );
    }
    let rows = measured_2_1_304("top-vim-normal-120col");
    let at = mode_row(&rows).expect("NORMAL's mode row");
    assert!(!vim_insert(&rows[at]), "CONTROL: NORMAL draws no indicator");
    assert_eq!(pill_indicator(&rows[at]), Some("mode.auto"));
    let screen = crate::harness::lights::read_screen(&rows).unwrap();
    assert_eq!(
        (screen.mode, screen.vim_insert),
        (Some(crate::harness::lights::Mode::Auto), false)
    );
    for (row, pill) in [
        ("  -- VISUAL -- \u{23F5}\u{23F5} auto mode on", false),
        ("  -- INSERT --\u{23F5}\u{23F5} auto mode on", false),
        ("   -- INSERT -- \u{23F5}\u{23F5} auto mode on", false),
        ("  \u{23F5}\u{23F5} auto mode on -- INSERT -- ", true),
    ] {
        assert!(!vim_insert(row), "{row:?}");
        assert_eq!(pill_indicator(row).is_some(), pill, "{row:?}");
        assert_eq!(opens_with_pill_glyph(row), pill, "{row:?}");
    }
    assert!(is_cut_pill("  -- INSERT -- \u{23F5}\u{23F5} bypass permi"));
    assert!(!is_cut_pill("  -- INSERT -- "));
}

#[test]
fn the_yolo_relabel_maps_the_pill_after_vim_inserts_indicator() {
    let row = "  -- INSERT -- \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{B7} \u{2190} for agents";
    let (rows, bottom) = under_rule(&[row]);
    let r = relabel_under(&rows, bottom).expect("the bypass pill after the indicator");
    assert_eq!(
        (r.row, r.start, r.words, r.end, r.label),
        (bottom + 1, 15, 18, 39, YOLO_ON)
    );
    assert_eq!(r.run_end, row.chars().count());
    assert_eq!((r.shift(), r.label_end()), (17, 22));
    for col in 0..r.label_end() {
        assert_eq!(r.grid_col(col), col, "the indicator and the label");
    }
    let agents = col_of_chars(row, "\u{2190} for agents");
    assert_eq!(
        r.grid_col(agents - r.shift()),
        agents,
        "the glass's `← for agents` is Claude's"
    );
    for col in r.run_end - r.shift()..r.run_end {
        assert_eq!(r.grid_col(col), r.start, "a blank the run left");
    }
    let (rows, bottom) =
        under_rule(&["  -- INSERT -- \u{23F8} manual mode on \u{B7} ? for shortcuts"]);
    let r = relabel_under(&rows, bottom).expect("the manual pill after the indicator");
    assert_eq!((r.start, r.end, r.label, r.shift()), (15, 31, YOLO_OFF, 8));
    let measured = measured_2_1_304("top-vim-insert-120col");
    let bottom = aterm_phase::phase::composer_bottom(&measured).unwrap();
    assert_eq!(
        relabel_under(&measured, bottom),
        None,
        "CONTROL: auto stays"
    );
    let (rows, bottom) = under_rule(&[&row.replacen("-- INSERT -- ", "", 1)]);
    let r = relabel_under(&rows, bottom).expect("CONTROL: the plain row");
    assert_eq!((r.start, r.words, r.end), (2, 5, 26));
}

fn col_of_chars(row: &str, needle: &str) -> usize {
    row[..row.find(needle).expect("the needle")].chars().count()
}

fn capital_key() -> String {
    let mut chars = CLAUDE_TOP_EFFORT_KEY.chars();
    let first = chars.next().expect("a key");
    format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
}

fn effort_after_answer(text: Option<&str>) -> TailFacts {
    let start = crate::harness::upgrade_models::parse_utc("2026-10-19T10:00:00Z");
    let answer = answer_at("claude-opus-5-5", "2026-10-19T10:00:10.000Z", Some("high"));
    let command = command_at("effort", "2026-10-19T10:00:20.000Z");
    let mut rows = vec![answer, command];
    if let Some(text) = text {
        rows.push(result_at("2026-10-19T10:00:20.000Z", text));
    }
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    tail_facts(body(&rows).as_bytes(), start)
}

#[test]
fn a_top_effort_off_result_keeps_the_effort_and_names_the_key() {
    let key = capital_key();
    let facts = effort_after_answer(Some(&format!("{key} off. Effort stays xhigh.")));
    assert_eq!(facts.effort, Said::Is("xhigh".into()));
    assert_eq!(facts.top, Some(false));
    assert_eq!(facts.model.shown(), Some("Opus 5.5"), "the model stands");
    let bare = effort_after_answer(None);
    assert_eq!(bare.effort, Said::Unread);
    assert_eq!(bare.top, None);
}

#[test]
fn a_top_effort_on_result_keeps_the_effort() {
    let key = capital_key();
    let on = |stays: &str| {
        effort_after_answer(Some(&format!(
            "{key} on (this session only): dynamic workflows on every task. Effort stays {stays}."
        )))
    };
    let facts = on("xhigh");
    assert_eq!(
        (facts.effort, facts.top),
        (Said::Is("xhigh".into()), Some(true))
    );
    let auto = on("auto");
    assert_eq!((auto.effort, auto.top), (Said::Unread, Some(true)));
    let lower = effort_after_answer(Some(&format!(
        "{CLAUDE_TOP_EFFORT_KEY} off. Effort stays max."
    )));
    assert_eq!(
        (lower.effort, lower.top),
        (Said::Is("max".into()), Some(false))
    );
    let other = effort_after_answer(Some("Workspace off. Effort stays max."));
    assert_eq!((other.effort, other.top), (Said::Unread, None));
}

#[test]
fn a_level_result_suffix_names_the_key() {
    let key = capital_key();
    let set = effort_after_answer(Some(&format!(
        "Set effort level to high (this session only): Comprehensive implementation \u{b7} {key} off"
    )));
    assert_eq!(
        (set.effort, set.top),
        (Said::Is("high".into()), Some(false))
    );
    let current = effort_after_answer(Some(&format!(
        "Current effort level: xhigh (Extended reasoning with thorough analysis) \u{b7} {key} on"
    )));
    assert_eq!(
        (current.effort, current.top),
        (Said::Is("xhigh".into()), Some(true))
    );
    let auto = effort_after_answer(Some(&format!("Effort level set to auto \u{b7} {key} off")));
    assert_eq!((auto.effort, auto.top), (Said::Unread, Some(false)));
    let plain = effort_after_answer(Some("Set effort level to low (this session only): Quick"));
    assert_eq!((plain.effort, plain.top), (Said::Is("low".into()), None));
    for tail in [
        "Fast mode off".to_owned(),
        format!("{key} online"),
        format!("{key}s off"),
    ] {
        let said = effort_after_answer(Some(&format!(
            "Set effort level to low (this session only): Quick \u{b7} {tail}"
        )));
        assert_eq!(said.top, None, "{tail:?}");
    }
}

#[test]
fn a_refused_key_answers_its_command_and_decides_nothing() {
    let key = capital_key();
    for refusal in [
        format!(
            "{key} needs dynamic workflows enabled (see /config). Valid options are: low, medium, high, xhigh, max, auto"
        ),
        format!(
            "{key} isn't available on claude-haiku-4-5. Valid options are: low, medium, high, auto"
        ),
    ] {
        let facts = effort_after_answer(Some(&refusal));
        assert_eq!(facts.effort, Said::Is("high".into()), "{refusal:?}");
        assert_eq!(facts.top, None, "{refusal:?}");
    }
    let unknown = effort_after_answer(Some("Effort is now something new"));
    assert_eq!((unknown.effort, unknown.top), (Said::Unread, None));
}

/// The key's state rides over newer rows that settle model and effort: an
/// answer after `/effort <key> off` decides both, and the process still
/// reads the key off (its carry and its own facts keep it).
#[cfg(unix)]
#[test]
fn the_key_state_rides_over_a_settled_newer_read() {
    const S: &str = "00000000-0000-4000-8000-000000001019";
    let proc = Proc::new("top-rides", "2026-10-19T10:00:00Z");
    proc.register(S);
    let key = capital_key();
    let rows = [
        answer_at("claude-opus-5-5", "2026-10-19T10:00:10.000Z", Some("xhigh")),
        command_at("effort", "2026-10-19T10:00:20.000Z"),
        result_at(
            "2026-10-19T10:00:20.000Z",
            &format!("{key} off. Effort stays xhigh."),
        ),
    ];
    let mut cache = TailCache::default();
    let launch = LaunchFacts::default();
    proc.write(S, &[&rows[0], &rows[1], &rows[2]]);
    proc.at("2026-10-19T10:00:30Z");
    assert_eq!(proc.read(&launch, &mut cache).top_effort, Some(false));
    let newer = answer_at("claude-opus-5-5", "2026-10-19T10:01:00.000Z", Some("xhigh"));
    proc.write(S, &[&rows[0], &rows[1], &rows[2], &newer]);
    proc.at("2026-10-19T10:01:10Z");
    let facts = proc.read(&launch, &mut cache);
    assert_eq!(facts.effort.as_deref(), Some("xhigh"));
    assert_eq!(facts.top_effort, Some(false), "rides over the settled read");
    let entry = session_of_pid(&proc.claude, 4242, Some(proc.start)).expect("registry");
    let other = facts_for_entry_at(
        &proc.claude,
        4343,
        Some(proc.start),
        &entry,
        &launch,
        &mut cache,
        UNIX_EPOCH + std::time::Duration::from_secs(proc.start + 80),
    );
    assert_eq!(other.effort.as_deref(), Some("xhigh"));
    assert_eq!(other.top_effort, None, "a new process drops it");
}

#[test]
fn drawn_cells_follow_the_glyphs_nudges() {
    let glyph = |cell: usize, cols: usize, nudge: i8| SetGlyph {
        ch: 'a',
        cell,
        cols,
        nudge,
        mark: false,
        tail: false,
        quiet: false,
    };
    let set = |glyphs: Vec<SetGlyph>| SetFacts {
        cells: 12,
        glyphs,
        ..SetFacts::default()
    };
    let span = |from: u16, to: u16| FactSpan {
        kind: FactKind::Path,
        cells: (from, to),
        restores: FactMask::default(),
    };
    let quarter = NUDGES_PER_CELL / 4;
    let right = set(vec![glyph(4, 1, 3 * quarter), glyph(5, 1, 3 * quarter)]);
    assert_eq!(drawn_cells(&right, &span(4, 6)), 5..7);
    let left = set(vec![glyph(4, 1, -3 * quarter), glyph(5, 1, -3 * quarter)]);
    assert_eq!(drawn_cells(&left, &span(4, 6)), 3..5);
    let wide = set(vec![glyph(4, 2, 3 * quarter)]);
    assert_eq!(drawn_cells(&wide, &span(4, 6)), 5..7);
    let half = set(vec![glyph(4, 1, 2 * quarter)]);
    assert_eq!(drawn_cells(&half, &span(4, 5)), 4..5, "4.5..5.5: cell 4");
    let still = set(vec![glyph(4, 1, 0), glyph(5, 1, 0)]);
    assert_eq!(drawn_cells(&still, &span(4, 6)), 4..6);
    assert_eq!(drawn_cells(&still, &span(8, 10)), 0..0);
}

#[test]
fn fact_spans_split_model_and_effort_by_their_glyphs() {
    let facts = FooterFacts {
        account: Some("work-2".into()),
        ..narrow_rule_tokens()
    };
    let segs = segments(&facts);
    let set = set_facts(&segs);
    let spans = fact_spans(&set, &segs);
    let kinds: Vec<FactKind> = spans.iter().flatten().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        [
            FactKind::Model,
            FactKind::Effort,
            FactKind::Path,
            FactKind::Branch,
            FactKind::Usage
        ]
    );
    let span = |kind: FactKind| {
        spans
            .iter()
            .flatten()
            .find(|s| s.kind == kind)
            .copied()
            .expect("a span")
    };
    let glyphs_in = |span: FactSpan| -> String {
        set.glyphs
            .iter()
            .filter(|g| {
                g.cell >= usize::from(span.cells.0) && g.cell + g.cols <= usize::from(span.cells.1)
            })
            .map(|g| g.ch)
            .collect()
    };
    let effort = span(FactKind::Effort);
    assert_eq!(effort.cells.1 - effort.cells.0, 5, "{effort:?}");
    assert_eq!(glyphs_in(effort), "xhigh");
    assert_eq!(glyphs_in(span(FactKind::Model)), "\u{25C6}Opus5.5");
    assert_eq!(glyphs_in(span(FactKind::Branch)), "\u{2387}main");
    assert!(spans.iter().flatten().all(|s| s.restores.is_empty()));
    let cells: Vec<(u16, u16)> = spans.iter().flatten().map(|s| s.cells).collect();
    assert!(cells.windows(2).all(|w| w[0].1 <= w[1].0), "{cells:?}");
    assert!(usize::from(cells.last().unwrap().1) <= set.cells);
    let alone = model_alone(&facts).expect("a model");
    let alone_spans = fact_spans(&set_facts(std::slice::from_ref(&alone)), &[alone]);
    assert_eq!(
        alone_spans
            .iter()
            .flatten()
            .map(|s| s.kind)
            .collect::<Vec<_>>(),
        [FactKind::Model]
    );
    let effort_only = FooterFacts {
        effort: Some("xhigh".into()),
        ..FooterFacts::default()
    };
    let segs = segments(&effort_only);
    let only = fact_spans(&set_facts(&segs), &segs);
    let only: Vec<FactSpan> = only.iter().flatten().copied().collect();
    assert_eq!(only.len(), 1);
    let cells = u16::try_from(set_facts(&segs).cells).unwrap();
    assert_eq!(
        (only[0].kind, only[0].cells),
        (FactKind::Effort, (0, cells))
    );
    let both = FooterFacts {
        off: FactMask::MODEL.union(FactMask::EFFORT),
        ..narrow_rule_facts()
    };
    let segs = segments(&both);
    assert_eq!(
        (segs[0].text.as_str(), segs[0].split),
        ("Opus 5.5 off xhigh off", Some("Opus 5.5 off ".len()))
    );
    let stubs = fact_spans(&set_facts(&segs), &segs);
    let (model, effort) = (stubs[0].expect("its model"), stubs[1].expect("its effort"));
    assert_eq!(
        (model.kind, model.restores, effort.kind, effort.restores),
        (
            FactKind::Model,
            FactMask::MODEL,
            FactKind::Effort,
            FactMask::EFFORT
        )
    );
    assert!(model.cells.1 <= effort.cells.0, "{model:?} {effort:?}");
}

#[test]
fn the_context_fact_is_a_span_its_stub_restores_and_it_goes_first() {
    let facts = FooterFacts {
        context: Some("ctx 84% left".into()),
        ..narrow_rule_tokens()
    };
    let segs = segments(&facts);
    let set = set_facts(&segs);
    let spans = fact_spans(&set, &segs);
    let kinds: Vec<FactKind> = spans.iter().flatten().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        [
            FactKind::Model,
            FactKind::Effort,
            FactKind::Path,
            FactKind::Branch,
            FactKind::Context,
            FactKind::Usage
        ]
    );
    let context = spans
        .iter()
        .flatten()
        .find(|s| s.kind == FactKind::Context)
        .copied()
        .expect("the context's span");
    let glyphs: String = set
        .glyphs
        .iter()
        .filter(|g| {
            g.cell >= usize::from(context.cells.0)
                && g.cell + g.cols <= usize::from(context.cells.1)
        })
        .map(|g| g.ch)
        .collect();
    assert_eq!(glyphs, "\u{25D0}ctx84%left");
    assert!(context.restores.is_empty(), "shown, no stub");
    let hidden = FooterFacts {
        off: FactMask::CONTEXT,
        ..facts.clone()
    };
    let segs = segments(&hidden);
    let stub = segs
        .iter()
        .find(|s| s.mark == CONTEXT_MARK)
        .expect("the context's stub");
    assert_eq!(
        (stub.text.as_str(), stub.quiet),
        ("ctx 84% left off", (true, false))
    );
    let spans = fact_spans(&set_facts(&segs), &segs);
    let stub = spans
        .iter()
        .flatten()
        .find(|s| s.kind == FactKind::Context)
        .expect("the stub's span");
    assert_eq!(stub.restores, FactMask::CONTEXT, "a stub that restores it");
    let subscribed = FooterFacts {
        subscription: Some(crate::harness::profile::SubscriptionDisplay {
            email: Some("person@example.com".into()),
            plan: Some("Max".into()),
            weekly: Some("weekly 29% left".into()),
            ..Default::default()
        }),
        ..facts.clone()
    };
    let ladder = facts_ladder(&subscribed);
    let without: Vec<Segment> = ladder[0]
        .iter()
        .filter(|s| s.mark != CONTEXT_MARK)
        .cloned()
        .collect();
    assert!(ladder[0].iter().any(|s| s.mark == CONTEXT_MARK));
    assert_eq!(ladder[1], without, "the context goes first");
    let none = FooterFacts {
        context: None,
        ..subscribed
    };
    let segs = segments(&none);
    assert!(
        fact_spans(&set_facts(&segs), &segs)
            .iter()
            .flatten()
            .all(|s| s.kind != FactKind::Context)
    );
    let ladder = facts_ladder(&none);
    assert!(
        ladder[1].len() < ladder[0].len() && ladder[1].iter().all(|s| s.mark != USAGE_MARK),
        "CONTROL: the usage goes first: {:?}",
        ladder[1]
    );
}

fn fit_sweep(facts: &FooterFacts) -> String {
    let lights = [
        None,
        Some(LightsWidth {
            chips: 4,
            title: 22,
            lean: [18, 14, 0, 0],
            short: 9,
            reason: false,
        }),
        Some(LightsWidth {
            chips: 4,
            title: 22,
            lean: [0; LEAN_FORMS],
            short: 9,
            reason: true,
        }),
    ];
    let mut out = String::new();
    for (i, lights) in lights.iter().enumerate() {
        let shown = |room: usize| {
            let fit = fit_rule(facts, room, *lights);
            format!(
                "{}|{:?}|{}",
                rung_texts(&fit).join("/"),
                fit.title,
                fit.chips
            )
        };
        let mut from = 20;
        let mut last = shown(20);
        for room in 21..=201 {
            let now = (room <= 200).then(|| shown(room));
            if now.as_ref() != Some(&last) {
                out.push_str(&format!("{i} {from}..={} {last}\n", room - 1));
                if let Some(now) = now {
                    (from, last) = (room, now);
                }
            }
        }
    }
    out
}

#[test]
fn a_hidden_fact_reads_off_in_rule_gray_and_gives_way_first() {
    let shown = |facts: &FooterFacts| -> Vec<String> {
        segments(facts)
            .iter()
            .map(|s| format!("{} {}", s.mark, s.text))
            .collect()
    };
    let hide = |off: FactMask| FooterFacts {
        off,
        ..narrow_rule_facts()
    };
    assert_eq!(
        shown(&hide(FactMask::MODEL)),
        [
            "\u{25C6} Opus 5.5 off xhigh",
            "\u{2302} ~/src/github.com/aterm",
            "\u{2387} main"
        ]
    );
    assert_eq!(
        shown(&hide(FactMask::EFFORT))[0],
        "\u{25C6} Opus 5.5 xhigh off"
    );
    assert_eq!(
        shown(&hide(FactMask::MODEL.union(FactMask::EFFORT)))[0],
        "\u{25C6} Opus 5.5 off xhigh off"
    );
    assert_eq!(
        shown(&hide(FactMask::PATH.union(FactMask::BRANCH)))[1..],
        ["\u{2302} ~/src/github.com/aterm off", "\u{2387} main off"]
    );
    let quiet = |off: FactMask| -> (String, String) {
        let set = set_facts(&segments(&hide(off)));
        let pick = |q: bool| {
            set.glyphs
                .iter()
                .filter(|g| !g.mark && g.quiet == q)
                .map(|g| g.ch)
                .collect()
        };
        (pick(true), pick(false))
    };
    assert_eq!(
        quiet(FactMask::MODEL),
        (
            "Opus5.5off".into(),
            "xhigh~/src/github.com/atermmain".into()
        )
    );
    assert_eq!(
        quiet(FactMask::EFFORT),
        (
            "xhighoff".into(),
            "Opus5.5~/src/github.com/atermmain".into()
        )
    );
    assert_eq!(
        quiet(FactMask::BRANCH),
        (
            "mainoff".into(),
            "Opus5.5xhigh~/src/github.com/aterm".into()
        )
    );
    let no_branch = FooterFacts {
        branch: None,
        off: FactMask::BRANCH,
        ..narrow_rule_facts()
    };
    assert!(!shown(&no_branch).iter().any(|s| s.contains(OFF_STUB)));
    let tokens = FooterFacts {
        off: FactMask::PATH,
        ..narrow_rule_tokens()
    };
    let walk = walk_down(&tokens);
    assert!(
        walk[0].contains(&"\u{2302} ~/src/github.com/aterm off".to_owned()),
        "{walk:?}"
    );
    assert_eq!(
        walk[1],
        [
            "\u{25C6} Opus 5.5 xhigh",
            "\u{2387} main",
            "\u{03A3} 7h52m \u{2191}48M \u{2193}310k"
        ],
        "{walk:?}"
    );
    assert!(walk[2..].iter().flatten().all(|s| !s.contains(OFF_STUB)));
    let no_model = hide(FactMask::MODEL);
    assert_eq!(model_alone(&no_model), None);
    assert_eq!(walk_down(&no_model).last().map(Vec::len), Some(0));
    let facts = FooterFacts {
        account: Some("work-2".into()),
        ..narrow_rule_walled()
    };
    let golden: String = include_str!("testdata/footer-fit-sweep.txt")
        .lines()
        .skip(1)
        .map(|l| format!("{l}\n"))
        .collect();
    let swept = fit_sweep(&facts);
    assert!(swept == golden, "the sweep moved:\n{swept}");
    assert_eq!(plain_ladder(&facts), bare_ladder(&facts));
    assert_ne!(
        plain_ladder(&hide(FactMask::PATH)),
        bare_ladder(&hide(FactMask::PATH))
    );
}

#[test]
fn top_rule_tag_finds_the_key_alone() {
    let fixtures = [
        include_str!("../../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-61col.txt"),
        include_str!(
            "../../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-narrow-26col.txt"
        ),
        include_str!(
            "../../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-narrow-34col.txt"
        ),
        include_str!("../../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-80col.txt"),
        include_str!(
            "../../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-144col.txt"
        ),
    ];
    assert_eq!(
        CLAUDE_TOP_EFFORT_KEY.len(),
        "workspace".len(),
        "the stand-in"
    );
    for text in fixtures {
        let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
        let (top, bottom) = aterm_phase::phase::composer_rules(&rows).expect("a composer");
        let stand_in = &rows[top];
        assert_eq!(top_rule_tag(stand_in), None, "{stand_in:?}");
        assert_eq!(top_rule_tag(&rows[bottom]), None, "a plain rule");
        let row = stand_in.replace(" workspace ", &format!(" {CLAUDE_TOP_EFFORT_KEY} "));
        let at = row
            .chars()
            .collect::<Vec<_>>()
            .windows(CLAUDE_TOP_EFFORT_KEY.len())
            .position(|w| w.iter().copied().eq(CLAUDE_TOP_EFFORT_KEY.chars()))
            .expect("the key");
        assert_eq!(
            top_rule_tag(&row),
            Some((at, at + CLAUDE_TOP_EFFORT_KEY.len())),
            "{row:?}"
        );
        let path = stand_in.replace(" workspace ", &format!(" /{CLAUDE_TOP_EFFORT_KEY} "));
        assert_eq!(top_rule_tag(&path), None, "{path:?}");
    }
    let rule = "\u{2500}".repeat(40);
    let beside = format!("{rule} fast mode  {CLAUDE_TOP_EFFORT_KEY} \u{2500}");
    assert_eq!(
        top_rule_tag(&beside),
        Some((52, 61)),
        "beside another label"
    );
    let inside = format!("{rule} {CLAUDE_TOP_EFFORT_KEY} {rule}");
    assert_eq!(top_rule_tag(&inside), None, "not the rule's end");
    let not_a_rule = format!("  {CLAUDE_TOP_EFFORT_KEY} \u{2500}");
    assert_eq!(top_rule_tag(&not_a_rule), None);
}

#[test]
fn top_effort_answer_reads_the_newest_answer_only() {
    let key = capital_key();
    let rule = "\u{2500}".repeat(40);
    let screen = |above: &[String]| -> Vec<String> {
        let mut rows: Vec<String> = above.to_vec();
        rows.push(String::new());
        rows.push(rule.clone());
        rows.push("\u{276F} ".into());
        rows.push(rule.clone());
        rows
    };
    let answer = |text: &str| format!("  \u{23BF}  {text}");
    let echo = format!("\u{276F} /effort {CLAUDE_TOP_EFFORT_KEY} off");
    let read = |rows: &[String]| {
        let (top, _) = aterm_phase::phase::composer_rules(rows).expect("a composer");
        top_effort_answer(rows, top)
    };
    let off = answer(&format!("{key} off. Effort stays xhigh."));
    assert_eq!(
        read(&screen(&[echo.clone(), off.clone()])),
        Some(TopAnswer::Off)
    );
    let on = answer(&format!(
        "{key} on (this session only): dynamic workflows on every task. Effort stays xhigh."
    ));
    assert_eq!(
        read(&screen(&[echo.clone(), on.clone()])),
        Some(TopAnswer::On)
    );
    let wrapped = [
        echo.clone(),
        answer(&format!("{key} off. Effort")),
        "     stays xhigh.".into(),
    ];
    assert_eq!(read(&screen(&wrapped)), Some(TopAnswer::Off));
    let needs = answer(&format!(
        "{key} needs dynamic workflows enabled (see /config)."
    ));
    assert_eq!(
        read(&screen(&[echo.clone(), needs])),
        Some(TopAnswer::NeedsWorkflows)
    );
    let unavailable = answer(&format!("{key} isn't available on claude-haiku-4-5."));
    assert_eq!(
        read(&screen(&[echo.clone(), unavailable])),
        Some(TopAnswer::Unavailable)
    );
    assert_eq!(
        read(&screen(&[
            echo.clone(),
            on.clone(),
            echo.clone(),
            off.clone()
        ])),
        Some(TopAnswer::Off)
    );
    assert_eq!(
        read(&screen(&[off.clone(), echo.clone()])),
        None,
        "under an echo"
    );
    let level = answer("Set effort level to high (this session only): Deep");
    assert_eq!(
        read(&screen(&[on.clone(), level])),
        None,
        "the newest is another"
    );
    let mut far = vec![echo.clone(), off.clone()];
    far.extend(std::iter::repeat_n("  plain transcript".to_owned(), 8));
    assert_eq!(read(&screen(&far)), None, "out of reach");
}

fn measured(text: &str, cut: Option<usize>) -> Vec<String> {
    let key = capital_key();
    let mut rows: Vec<String> = text
        .lines()
        .skip(1)
        .map(|l| {
            l.replace("Workspace", &key)
                .replace("workspace", CLAUDE_TOP_EFFORT_KEY)
        })
        .collect();
    if let Some(cut) = cut {
        let width = rows[cut..]
            .iter()
            .find(|r| aterm_phase::phase::is_rule(r))
            .map_or(40, |r| r.chars().count());
        let rule = "\u{2500}".repeat(width);
        rows.truncate(cut);
        rows.extend([String::new(), rule.clone(), "\u{276F} ".into(), rule]);
    }
    rows
}

#[test]
fn top_effort_reply_reads_every_measured_answer() {
    use aterm_phase::phase::composer_rules;
    let fx = |name: &str| -> &'static str {
        match name {
            "on-26" => {
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.304-top-on-26col.txt")
            }
            "on-34" => {
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.304-top-on-34col.txt")
            }
            "on-61" => {
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.304-top-on-61col.txt")
            }
            "on-80" => {
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.304-top-on-80col.txt")
            }
            "on-120" => {
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.304-top-on-120col.txt")
            }
            "off-120" => {
                include_str!("../../../aterm-phase/src/fixtures/claude-2.1.304-top-off-120col.txt")
            }
            "unavailable" => include_str!(
                "../../../aterm-phase/src/fixtures/claude-2.1.304-top-refused-unavailable-120col.txt"
            ),
            "needs" => include_str!(
                "../../../aterm-phase/src/fixtures/claude-2.1.304-top-refused-needs-workflows-120col.txt"
            ),
            _ => unreachable!("{name}"),
        }
    };
    let read = |rows: &[String]| {
        let (top, _) = composer_rules(rows).expect("a composer");
        top_effort_reply(rows, top)
    };
    let said = |answer: TopAnswer| TopReply {
        answer,
        model: None,
    };
    for name in ["on-26", "on-34", "on-61", "on-80", "on-120"] {
        assert_eq!(
            read(&measured(fx(name), None)),
            Some(said(TopAnswer::On)),
            "{name}"
        );
    }
    assert_eq!(
        read(&measured(fx("off-120"), None)),
        Some(said(TopAnswer::Off)),
        "far above the fullscreen composer"
    );
    assert_eq!(
        read(&measured(fx("needs"), None)),
        Some(said(TopAnswer::NeedsWorkflows))
    );
    let unavailable = Some(TopReply {
        answer: TopAnswer::Unavailable,
        model: Some("claude-sonnet-4-5".into()),
    });
    assert_eq!(read(&measured(fx("unavailable"), None)), unavailable);
    for (name, cut) in [("on-34", 22), ("on-80", 26)] {
        let rows = measured(fx(name), Some(cut));
        assert_eq!(read(&rows), unavailable, "{name} cut at {cut}");
    }
    let stand_in: Vec<String> = fx("on-34").lines().skip(1).map(str::to_owned).collect();
    let (top, _) = composer_rules(&stand_in).expect("a composer");
    assert_eq!(top_effort_reply(&stand_in, top), None, "no key's head");
    let model = measured(fx("on-26"), Some(26));
    assert_eq!(read(&model), None, "a /model answer");
}

#[test]
fn no_anchor_spells_the_key() {
    for id in [
        "effort.top_on",
        "effort.top_off",
        "effort.stays",
        "effort.top_needs",
        "effort.top_unavailable",
        "effort.key_on",
        "effort.key_off",
        "effort.hint_tail",
    ] {
        let text = aterm_phase::anchor(id);
        assert!(
            !text.to_ascii_lowercase().contains(CLAUDE_TOP_EFFORT_KEY),
            "{id}: {text:?}"
        );
        assert!(!text.is_empty(), "{id}");
    }
}

#[test]
fn the_effort_split_survives_clusters_and_marks() {
    for head in ["Opu\u{301}s 5.5 ", "\u{1F469}\u{200D}\u{1F4BB} 5.5 "] {
        let text = format!("{head}xhigh");
        let set = set_facts(&[Segment {
            mark: MODEL_MARK,
            text: text.clone(),
            split: Some(head.len()),
            quiet: (true, false),
        }]);
        let spelled = |pick: &dyn Fn(&SetGlyph) -> bool| -> String {
            set.glyphs
                .iter()
                .filter(|g| !g.mark && pick(g))
                .map(|g| g.ch)
                .collect()
        };
        assert_eq!(spelled(&|g| g.tail), "xhigh", "{text:?}: the effort");
        assert_eq!(spelled(&|g| g.tail && g.quiet), "", "{text:?}: shown");
        let lead: String = head.chars().filter(|c| !c.is_whitespace()).collect();
        let lead: String = aterm_grapheme::split_graphemes(&lead)
            .map(|c| c.text.chars().next().unwrap_or(' '))
            .collect();
        assert_eq!(spelled(&|g| !g.tail && g.quiet), lead, "{text:?}: the stub");
        let mark = set.glyphs.first().expect("the mark");
        assert!(
            mark.mark && !mark.tail && !mark.quiet,
            "{text:?}: CONTROL, the mark"
        );
    }
}

#[test]
fn agents_panel_under_reads_agy_active_subagents_and_skips_background_tasks() {
    let rule = "─".repeat(80);
    let rows = vec![
        "● Spawned 3 subagents (ctrl+o to expand)".to_owned(),
        String::new(),
        rule.clone(),
        "> ".to_owned(),
        rule.clone(),
        "  ● [17:50:42] sleep 30 running".to_owned(),
        "  ● Agent(self: Cursor Trail Rendering Engineer)  Reading file · 4m36s".to_owned(),
        "  ● Agent(self: System Monitoring Network Stats Engineer)  Running command · 4m39s".to_owned(),
        rule,
        "? for shortcuts                                      Gemini 4 Argon · high · 2 subagent(s)".to_owned(),
    ];
    let panel = agents_panel_under(&rows, 4).expect("agy subagents panel");
    assert_eq!(panel.rows().len(), 2);
    assert_eq!(panel.rows()[0].row, 6);
    assert_eq!(
        panel.rows()[0].name,
        aterm_phase::agents_panel::Span {
            start: 4,
            end: 4 + "Cursor Trail Rendering Engineer".len() as u16,
        }
    );
    assert_eq!(panel.rows()[1].row, 7);
    assert_eq!(
        panel.rows()[1].name,
        aterm_phase::agents_panel::Span {
            start: 4,
            end: 4 + "System Monitoring Network Stats Engineer".len() as u16,
        }
    );
}
