// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::host_path::{abs, json};
#[cfg(unix)]
use std::io::Write as _;

/// What a test runs before a registry read, with the file's path.
type RegistryHook = Box<dyn FnMut(&Path)>;

thread_local! {
    /// Runs before each `sessions/<pid>.json` read on this thread
    /// ([`session_of_pid`]), so a test can land a rewrite mid-read.
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
    /// Transcript lookups ([`transcript_path`]) made on this thread.
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
/// afterwards. Returns the facts and the registry open count.
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

    // An old conversation row cannot become this process's model merely
    // because the unchanged tail is served from memory on a later tick.
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
    // The rewrite is told apart by its ctime alone: let the coarse clock some
    // filesystems stamp it with (a Linux jiffy) tick past the append's.
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

    // A process restart on the identical transcript path must move the
    // floor even before the new process has written a row of its own.
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
/// (an append included). A predecessor's row and a subagent's do not move
/// them; with no such row they are the launch directory's.
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
    // The tail is served from the cache; the repository is read live.
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
    // A measured one (home prefix anonymized): a dot folds as a slash does.
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

/// A `/model` command whose result this build cannot read (or none at all)
/// leaves model and effort unknown; the tag quoted anywhere but at the start
/// of a person's row is text.
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
    // Negative control: the same tail without the command row.
    assert_eq!(
        tail_facts(format!("{answer}\n").as_bytes(), start)
            .model
            .shown(),
        Some("Opus 5")
    );
    // The tag quoted anywhere but at the start of a person's row is text.
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

/// Model results in every build's spelling show the vendor's name
/// (`(default)`, `(1M context)` dropped); anything not plainly a name is
/// no model.
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
        // The icon in the theme's fast-mode colour; the classifier strips
        // the colour first.
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
    // Hostile or unreadable: never a model.
    for text in [
        "Set model to `Opus\u{7}5`",
        "Set model to `Opus\n5`",
        "Set model to `A very long name that no model of anyone's has ever carried at all`",
        "Set model to ``Opus``",
        "Set model to Opus 5.5",
    ] {
        assert_eq!(result(text).model, Said::Unread, "{text:?}");
    }
    // Not a model result at all: says nothing about the model.
    for text in [
        "Cloud session couldn't switch to `Opus 5.5`",
        "Model switch to X was blocked by a PreModelSwitch hook",
        "Current model: `Opus 5.5`",
        "Kept Fast mode ON \u{00B7} model set to `Opus 5.5`",
        "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Kept Fast mode ON \u{00B7} model set to `Opus 5.5`",
        "Fast mode ON",
        // `/fast on` for a model that is already fast: no `model set to`.
        "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} $30/$150 per Mtok",
        "Fast mode OFF",
    ] {
        assert_eq!(result(text).model, Said::Unsaid, "{text:?}");
    }
}

/// A `/fast on` that moved the model: coloured `↯` icon, the phrase and
/// the new model.
const FAST_ON_COLOURED: &str = "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok";

/// A `/model` picker that changed nothing (`type:system` rows with
/// `Kept model as`) decides nothing. Negative control: the same command
/// as a `type:user` choice row with no readable result voids both.
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
    // Negative control: a choice row with no readable result under it.
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

/// A transcript row of an answer by `model`, stamped `at`, with its
/// `effort` when the model has one.
fn answer_at(model: &str, at: &str, effort: Option<&str>) -> String {
    let effort = effort.map_or(String::new(), |e| format!(r#","effort":"{e}""#));
    format!(r#"{{"type":"assistant","timestamp":"{at}"{effort},"message":{{"model":"{model}"}}}}"#)
}

/// The `type:user` row Claude writes for the local command `/<name>`.
fn command_at(name: &str, at: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"<command-name>/{name}</command-name>\n            <command-message>{name}</command-message>\n            <command-args></command-args>"}}}}"#
    )
}

/// The `type:user` result row of a local command, saying `text`.
fn result_at(at: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":{}}}}}"#,
        aterm_json::to_string(&Value::String(format!(
            "<local-command-stdout>{text}</local-command-stdout>"
        )))
        .unwrap()
    )
}

/// `rows`, one per line.
fn body(rows: &[&str]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

/// [`chose_model_since`]: every `/model` command row is a choice whatever its
/// result says (a kept or unreadable model still sets Claude's main-loop
/// override), and so is a result that moved the model with no command row.
/// Negative controls: a `Kept model` alone, an answer, another command, a
/// `/model` before the floor and the tag quoted in a person's text pin nothing.
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

/// A model result without `with … effort` keeps the earlier effort only if
/// it was read for the new model. Negative controls: a switch to that same
/// model keeps it, and an explicit clause names it.
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
    // Opus 5.5 at xhigh, then Haiku 4.5 — which has
    // no effort levels — and then a Haiku answer, which names none.
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
    // Negative control: a switch to the model the effort was read for.
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Opus 5.5 (default)");
    assert_eq!(facts(&[&opus, &c, &r]), is("Opus 5.5", Some("xhigh")));
    // Through two switches: Sonnet 5 ran between, so a switch back to
    // Opus 5.5 does not bring Opus 5.5's xhigh back.
    let [c1, r1] = switch("2026-09-28T15:02:00.000Z", "Sonnet 5");
    let [c2, r2] = switch("2026-09-28T15:02:30.000Z", "Opus 5.5");
    assert_eq!(facts(&[&opus, &c1, &r1, &c2, &r2]), is("Opus 5.5", None));
    // An `/effort` after the switch is the new model's own.
    let [e1, e2] = effort_high("2026-09-28T15:02:10.000Z");
    assert_eq!(
        facts(&[&opus, &c1, &r1, &e1, &e2]),
        is("Sonnet 5", Some("high"))
    );
    // An `/effort` before the switch was set on the model then running:
    // kept only when that is the model switched to.
    let [e1, e2] = effort_high("2026-09-28T15:01:30.000Z");
    assert_eq!(facts(&[&opus, &e1, &e2, &c1, &r1]), is("Sonnet 5", None));
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Opus 5.5");
    assert_eq!(
        facts(&[&opus, &e1, &e2, &c, &r]),
        is("Opus 5.5", Some("high"))
    );
    // A result that names the effort names it, whatever ran before.
    let clause = result_at(
        "2026-09-28T15:02:00.000Z",
        "Set model to `Sonnet 5` for this session only with `high` effort",
    );
    let c = command_at("model", "2026-09-28T15:02:00.000Z");
    assert_eq!(facts(&[&opus, &c, &clause]), is("Sonnet 5", Some("high")));
    // With no older model named, the launch model decides
    // (`facts_for_entry_at`).
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Sonnet 5");
    let alone = tail_facts(body(&[&c, &r]).as_bytes(), start);
    assert_eq!(
        (alone.effort, alone.effort_for.as_deref()),
        (Said::Unsaid, Some("Sonnet 5"))
    );
}

/// A command pairs only with the result directly under it: a `/fast` move
/// does not settle an older unreadable `/model`, so the effort is unknown.
/// Negative control: the move alone keeps the effort.
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
    // 3a: the unreadable choice alone voids both.
    assert_eq!(
        facts(&[&opus, &model_cmd, &unread]),
        (Said::Unread, Said::Unread)
    );
    // 3b: a `/fast` move after it names the model,
    // and the effort stays unknown.
    assert_eq!(
        facts(&[&opus, &model_cmd, &unread, &fast_cmd, &fast]),
        (Said::Is("Opus 5.5".into()), Said::Unread)
    );
    // 3c: a `/model` command with no result under it, then the move: the
    // move's result is its own command's, not the `/model` one's.
    assert_eq!(
        facts(&[&opus, &model_cmd, &fast_cmd, &fast]),
        (Said::Is("Opus 5.5".into()), Said::Unread)
    );
    // Negative control: the move alone keeps the effort.
    assert_eq!(
        facts(&[&opus, &fast_cmd, &fast]),
        (Said::Is("Opus 5.5".into()), Said::Is("xhigh".into()))
    );
    // …only when it was read for the model moved to; a `/fast` that moved
    // nothing decides nothing, whatever its command row.
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

/// [`scan`] reads back to the floor: a row stamped in the floor's own second is
/// the process's, and the first row stamped before it ends the scan as a
/// predecessor's, so no row above it is read whatever its stamp (the file is in
/// append order), and the scan says it met the floor (the read's `resumed`).
#[test]
fn the_floor_second_is_the_processs_and_the_first_row_before_it_ends_the_scan() {
    let floor = crate::harness::upgrade_models::parse_utc("2026-09-28T15:00:00Z");
    let at_floor = answer_at("claude-opus-5-5", "2026-09-28T15:00:00.900Z", Some("high"));
    let before = answer_at("claude-fable-5-1", "2026-09-28T14:59:59.900Z", None);
    let above = answer_at("claude-sonnet-5", "2026-09-28T15:10:00.000Z", Some("low"));
    let read = |rows: &[&str]| {
        let (facts, floored) = scan(body(rows).as_bytes(), floor);
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

    /// The resolver's next reads happen at `stamp`.
    fn at(&self, stamp: &str) {
        self.clock
            .set(crate::harness::upgrade_models::parse_utc(stamp).unwrap());
    }

    /// The registry names `session` for the process — the same pid,
    /// `procStart` and `startedAt` whatever the session.
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
/// 5. Negative controls: launched on Sonnet 5 the effort stays open for the
/// card, and an `/effort` read before the switch stands.
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
    // With no flag, the card may speak for the launch model — only when it
    // names the model switched to ([`a_launch_card_fills_only_open_facts`]).
    let open = proc.read(&LaunchFacts::default(), &mut TailCache::default());
    assert!(open.effort_open);
    assert_eq!(open.effort_for.as_deref(), Some("Sonnet 5"));
    // Negative control: launched on Sonnet 5 the switch is to itself; an
    // earlier `/effort` stands.
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
/// effort stands only for that model. A resumed conversation whose newest
/// answer names no plain model shows none; one with no answer restores
/// nothing and pins nothing. The model-only walk (`Resume`, `Clear`, a
/// choice, `--model`) is `tests/conformance_footer_model.rs`'s.
#[cfg(unix)]
#[test]
fn a_resume_carries_the_model_only_where_the_process_pinned_it() {
    const A: &str = "00000000-0000-4000-8000-0000000000a1";
    const B: &str = "00000000-0000-4000-8000-0000000000b1";
    const R: &str = "00000000-0000-4000-8000-0000000000c1";
    let proc = Proc::new("resume", "2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    // The resumed conversation's answer, written by an earlier process.
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
    // Unpinned: the resume restored Fable 5.1 — Opus 5.5's xhigh is not
    // Fable 5.1's.
    let (before, after, _) = run(&[&opus], &LaunchFacts::default(), &[&old]);
    assert_eq!(shown(&before), is("Opus 5.5", Some("xhigh")));
    assert_eq!(
        shown(&after),
        is("Fable 5.1", None),
        "never the pre-resume model"
    );
    assert!(!after.model_open && !after.effort_open);
    // Unpinned, the same model: the effort was read for it, and stands.
    let fable = answer_at("claude-fable-5-1", "2026-09-28T15:01:00.000Z", Some("low"));
    let (_, after, _) = run(&[&fable], &LaunchFacts::default(), &[&old]);
    assert_eq!(shown(&after), is("Fable 5.1", Some("low")));
    // A resumed conversation whose newest answer names no plain model:
    // no model, never an older answer's.
    let odd = answer_at("claude opus", "2026-09-27T10:00:03.000Z", Some("high"));
    let (_, after, _) = run(&[&opus], &LaunchFacts::default(), &[&old, &odd]);
    assert_eq!(shown(&after), (None, None));
    // A resumed conversation with no answer at all (a choice there, never
    // answered): Claude restores nothing, the process keeps its model, and
    // nothing is pinned; the next `/resume` restores B's.
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
    // Pinned by the launch without a real id: an alias, an environment pin.
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
/// Negative controls: an environment pin restores nothing; a conversation
/// with no answer pins nothing. `--resume` with and without `--model` is
/// `tests/conformance_footer_model.rs`'s.
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
    // `claude --continue`, then `/resume Y`: pinned.
    let continued = launch_facts(&argv(&["claude", "--continue"]));
    let mut cache = TailCache::default();
    assert_eq!(model(&proc.read(&continued, &mut cache)), fable);
    proc.register(Y);
    assert_eq!(model(&proc.read(&continued, &mut cache)), fable, "pinned");
    // A first sight the launch does not name shows no model, yet the pin
    // stands: `/resume Y` never shows Sonnet 5.
    for launch in [LaunchFacts::default(), resumed.clone()] {
        proc.register(Z);
        let mut cache = TailCache::default();
        assert_eq!(model(&proc.read(&launch, &mut cache)), None, "{launch:?}");
        proc.register(Y);
        assert_eq!(model(&proc.read(&launch, &mut cache)), None, "{launch:?}");
    }
    // Negative control: pinned by the environment, nothing is restored.
    let env = launch_facts_of(
        &argv(&["claude", "--resume", X]),
        &argv(&["ANTHROPIC_MODEL=claude-opus-5-5"]),
    );
    proc.register(X);
    let mut cache = TailCache::default();
    assert_eq!(model(&proc.read(&env, &mut cache)), None);
    proc.register(Y);
    assert_eq!(model(&proc.read(&env, &mut cache)), None);
    // Negative control: no answer to restore, so the next `/resume`
    // restores Y's.
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
/// The floor-only judge, a choice's pin and an answered `/clear` are
/// `tests/conformance_footer_model.rs`'s (`FloorOnly`, `ResumeLate`).
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
    // S (last seen at 15:25), `/resume D`, `/resume B`.
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
    // `/clear` into C, then `/resume S` — back into the session this
    // process began: Claude restores S's Opus 5.5 and pins it.
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
/// Negative control: born after that moment, a branch of copies is no
/// resume.
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
    // X is made now, by another tab.
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
/// `/resume`. Negative control: without `own_rows` the head's first stamp
/// is the copied one.
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
    // The branch, begun after the process was last seen in S: the copy,
    // then the memory-mode row as the vendor writes it.
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

    // S seen, `/branch` to F, then `/resume B`.
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
/// Negative controls: an oversized non-answer, and an oversized answer of
/// the process's own since the floor, are passed over.
#[cfg(unix)]
#[test]
fn an_oversized_newest_answer_is_never_passed_over() {
    const S: &str = "00000000-0000-4000-8000-0000000000f1";
    const A: &str = "00000000-0000-4000-8000-0000000000f2";
    let proc = Proc::new("oversized", "2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    let sonnet_old = answer_at("claude-sonnet-5", "2026-09-26T10:00:00.000Z", Some("high"));
    // Claude's own key order: `message` first, `type` and `timestamp`
    // after it.
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
    // 300 KB: past a row's limit, inside one read window; 1.2 MB:
    // across three windows.
    for bytes in [300 * 1024, 1200 * 1024] {
        let big = big_answer("claude-fable-5-1", "2026-09-27T10:05:00.000Z", bytes);
        assert_eq!(after_resume(&[&sonnet_old, &big]), None, "R4 at {bytes}");
        let tool = big_result("2026-09-27T10:05:00.000Z", bytes);
        assert_eq!(
            after_resume(&[&sonnet_old, &tool]).as_deref(),
            Some("Sonnet 5"),
            "a tool result at {bytes}"
        );
        // Restored Sonnet 5 runs on: its own answer, since the floor.
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

/// What pins the model at launch: any `--model` or an environment model pin;
/// only a real id is shown. An empty or other variable pins nothing. A
/// launch resume is recorded, not a pin ([`TailCache`] decides).
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
    // The model shown is the last `--model`, as Claude's parser takes it,
    // in either spelling.
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
    // The Claude home the environment named, raw, for the resume line
    // (`resume::home_env`): set and empty is named too; none, none.
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

/// `/effort` results name the effort at once (top-effort mode as `xhigh`);
/// `auto`, an unknown level or an unreadable result make it unknown.
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
    // Negative control: without the command, the answer's effort stands.
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
/// Negative controls: the plain window misses the answer; a new floor or a
/// shrunk file are read afresh.
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
    // Control: the plain tail window does not see it.
    assert_eq!(
        read_tail_facts(&path, Some(start)).unwrap().model,
        Said::Unsaid
    );
    // A read afresh looks back past the huge row.
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
    // A new floor (an in-place exec after every row) is never carried.
    assert_eq!(read(&mut cache, start + 6 * 3600), TailFacts::default());
    // A file rewritten shorter is read afresh.
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

/// The floor is the kernel's start, else the registry's `procStart` (the
/// path where the kernel cannot be asked); with neither, no model.
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
    // Negative control: a floor older than the answer reads it.
    entry(Some(start - 3 * 86_400));
    assert_eq!(model(None).as_deref(), Some("Opus 5"));
    // An image exec'd in place (same pid, same kernel start) registers
    // its own `startedAt`: the floor moves with it.
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
    // The process's `--model` names the model while the transcript since
    // the floor names none, and closes it to the launch card.
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
    // With no start known at all, the launch flag is still the process's own.
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

/// `/clear` gives the same process a new session id and empty transcript;
/// the model and effort chosen in the left session stand, never `--model`
/// or the card. Negative control: a reader keeping nothing shows the flag's
/// model. A new floor inherits nothing.
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
    // Each read at the wall-clock time it names.
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
    // An unread `/effort`, then `/clear`: the left session is re-read at
    // the switch, so the effort is not lost.
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
    // Negative control: nothing kept of the process: the launch flag.
    assert_eq!(
        read_at(&mut TailCache::default(), "2026-09-28T15:04:30Z")
            .model
            .as_deref(),
        Some("Opus 5.5")
    );
    // A registry read that lands mid-write clears the tail, not the process.
    cache.clear();
    assert_eq!(
        read_at(&mut cache, "2026-09-28T15:05:00Z").model.as_deref(),
        Some("Fable 5.1")
    );
    // An image exec'd in place registers a new `startedAt`: a new floor,
    // a new process for the footer — its own `--model`, nothing carried.
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
    let _ = std::fs::remove_dir_all(&root);
}

/// A scratch `$HOME` whose repositories sit under `place` (a path below the
/// home): `place/repo/.git/HEAD` names `branch`. Returns `(root, home, repo)`.
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

/// A repository inside a sync provider's folder shows no branch rather than
/// open a file whose system dialog would park the shared footer thread. The
/// same layout in an ordinary (or merely protected) folder is read.
#[test]
fn read_git_head_opens_nothing_under_a_file_provider_domain() {
    for place in [
        "Library/CloudStorage/x",
        "Library/Mobile Documents/com~apple~CloudDocs",
    ] {
        let (root, home, repo) = repo_under("provider", place, "main");
        assert_eq!(
            read_git_head_in(Some(&home), &repo),
            GitHeadRead::Absent,
            "{place}"
        );
        // a subdirectory of the repository climbs to the same `.git`
        std::fs::create_dir_all(repo.join("src")).unwrap();
        assert_eq!(
            read_git_head_in(Some(&home), &repo.join("src")),
            GitHeadRead::Absent,
            "{place}/src"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
    for place in ["Documents", "Library/Application Support", "src"] {
        let (root, home, repo) = repo_under("ordinary", place, "main");
        assert_eq!(
            read_git_head_in(Some(&home), &repo),
            GitHeadRead::Found("main".into()),
            "{place}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A worktree outside a provider domain can point its `gitdir:` into one; the
/// `HEAD` there is not opened either. A symlink into one is caught too. (An
/// ordinary worktree file is followed: [`git_head_follows_a_worktree_file_and_climbs_to_the_top`].)
#[cfg(unix)]
#[test]
fn a_worktree_pointing_into_a_provider_domain_is_absent() {
    let (root, home, provider_repo) =
        repo_under("wt-provider", "Library/CloudStorage/x", "elsewhere");
    let wt = home.join("src").join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", provider_repo.join(".git").display()),
    )
    .unwrap();
    assert_eq!(read_git_head_in(Some(&home), &wt), GitHeadRead::Absent);
    // a symlink from an ordinary folder into the provider domain
    let link = home.join("src").join("link");
    std::os::unix::fs::symlink(&provider_repo, &link).unwrap();
    assert_eq!(read_git_head_in(Some(&home), &link), GitHeadRead::Absent);
    let _ = std::fs::remove_dir_all(&root);
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
    assert_eq!(read_git_head(&repo), GitHeadRead::Found("main".into()));
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

/// The account stands after the model: the process's two facts, then the
/// directory's two, a limit wall still first and the tokens still last.
/// Negative control: with no account there is no such mark, and the other
/// five stand in that order.
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
        Segment {
            mark: ACCOUNT_MARK,
            text: "work-2".into()
        },
        "the label, whole"
    );
    // With the model alone, and with nothing else at all.
    let bare = FooterFacts {
        model: Some("Opus 5.5".into()),
        account: Some("work-2".into()),
        ..FooterFacts::default()
    };
    assert_eq!(marks(&bare), [MODEL_MARK, ACCOUNT_MARK]);
    // Negative control: no account, no mark; the other five stand.
    assert_eq!(
        marks(&plain),
        [WALL_MARK, MODEL_MARK, PATH_MARK, BRANCH_MARK, USAGE_MARK]
    );
}

/// End to end over a temp Claude directory: the fold reads the transcript
/// this process's sessions file names, only what was appended, sums
/// subagents and withholds tokens until caught up. Tokens are the
/// conversation's (pre-start rows count); model and effort keep the process
/// floor. A switch and a torn sessions file are the tests below.
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
    // The process's one transcript tail, as the window keeps it.
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

    // A subagent still behind names no tokens — a prefix of a session is
    // not its usage — and the next reads carry on until caught up.
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
    // A kernel start the caller knows (the files carry no `procStart`).
    let started = Some(1_700_000_000);
    // Each process's one transcript tail, as the window keeps them; the
    // tab's one fold.
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

    // `/clear`: the same process names a session with no transcript yet.
    name(4242, "def-2");
    assert_eq!(usage_of(4242, &mut cache), None, "after /clear");
    name(4242, "abc-1");
    assert_eq!(usage_of(4242, &mut cache), Some(shown.clone()), "back");

    // A new process in the same tab, its session not written yet.
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
    // A kernel start the caller knows: the transcript's rows speak for
    // the process, so the tail is read as well as the fold.
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

    // The slug misses: the scan of `projects/` finds the file, once.
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
/// ahead; an unplaceable reset shows nothing. (A response served since takes
/// it down: [`a_fold_still_behind_shows_no_wall_it_has_not_caught_up_to`].)
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

/// Every known pill opens a mode row whatever follows — a narrow pane's
/// gaps, cut items and hints wrapped away included; a cycle hint cut
/// mid-word, a parenthesis that is neither the hint nor its word-cut, a
/// pill-less row, deeper-indented tool output and a glyph paired with
/// another pill's indicator do not.
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
        // 45 and 48 columns, `← for agents` behind the cut hint.
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to  \u{00B7}",
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to  \u{00B7} \u{2190}\u{2026}",
        // 37: the hint wrapped away; the separator past the wide line's gap.
        "  \u{23F5}\u{23F5} bypass permissions on  \u{00B7} \u{2190} fo\u{2026}",
        // 26: auto's second wrapped line is the wider one.
        "  \u{23F5}\u{23F5} auto mode on      \u{00B7}",
        // 51: the whole hint, and a separator the row's edge cut.
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7}",
        // 50, 58 and 66, busy: live status cut short.
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to\u{2026}",
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interru\u{2026}",
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interrupt \u{00B7} \u{2190} f\u{2026}",
        // 31, busy, the separator in the truncating text itself.
        "  \u{23F5}\u{23F5} bypass permissions on \u{00B7}\u{2026}",
        // Cut items after live status (`← 1 agent` cut to its arrow).
        "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} \u{2190}\u{2026}",
        "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} \u{2190} \u{2026}",
        "  \u{23F8} manual mode on \u{00B7} 1 shell \u{00B7} ?\u{2026}",
        // A closed parenthesis that is no hint.
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

/// Narrow-pane mode rows (pill text wrapped by word at `columns - 4`, first
/// line only), transcribed from the renderer's code: each is still found as
/// the mode row.
#[test]
fn a_narrow_pane_cuts_the_hint_between_words_and_the_row_is_still_the_mode_row() {
    let cases: [(&str, &str); 5] = [
        // 39 and 42 columns.
        (
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab",
            "mode.bypass",
        ),
        (
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to",
            "mode.bypass",
        ),
        // 30 columns, as the grid holds it: padded to the pane's width.
        ("  \u{23F5}\u{23F5} auto mode on (shift+tab  ", "mode.auto"),
        // 32 columns: an unexpected mode's pill, its hint cut.
        ("  \u{23F8} plan mode on (shift+tab to", "mode.plan"),
        // 30 columns: the hint wrapped away whole, a blank left behind.
        ("  \u{23F5}\u{23F5} bypass permissions on ", "mode.bypass"),
    ];
    for (row, indicator) in cases {
        assert!(is_mode_row(row), "{row:?}");
        assert_eq!(pill_indicator(row), Some(indicator), "{row:?}");
    }
}

/// `procStart` is `ps -o lstart=` in UTC; the guard compares it to the
/// kernel's start time rendered the same way.
#[test]
fn a_reused_pid_is_refused_by_its_start_time() {
    // 1790194435 = Wed Sep 23 2026 20:13:55 UTC.
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

/// The one resolver (`upgrade_drive::claude_home_dir`), as Claude Code
/// resolves `CLAUDE_CONFIG_DIR ?? join(homedir(), ".claude")`: an empty
/// override is set (`??` keeps it) and names the directory the process runs
/// in; a relative one is under it; neither is read without that directory.
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

/// A pill glyph with an indicator this build does not know is the
/// vendor's drift, and is told apart from rows that are no pill at all.
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
    // A narrow pane's cut of a known pill is geometry, not drift.
    assert!(is_cut_pill("  \u{23F5}\u{23F5} bypass permissi"));
    assert!(is_cut_pill("  \u{23F5}\u{23F5} bypass permissions o"));
    assert!(!is_cut_pill(renamed));
}

/// The rule's cells from screen text: `─` is a plain rule glyph, anything
/// else is ink (the host also checks colour).
fn coverable(row: &str, width: usize) -> Vec<bool> {
    let mut out: Vec<bool> = row.chars().map(|c| c == '\u{2500}').collect();
    out.resize(width, false);
    out
}

/// Every recorded Claude Code screen in `aterm-phase`'s fixtures with a
/// composer has a writable bottom rule of plain glyphs with room for the
/// footer at its width, and the mode row under it; a screen with a box up
/// has none.
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

/// The run is the widest of plain glyphs, rightmost of equals, bounded by
/// ink; writes leave [`RULE_LEAD`] glyphs left and one right.
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

/// The line [`set_facts`] made, read back from where its glyphs are drawn:
/// a space for each pitch of room between two glyphs, at the line's own
/// pitch (its cells over its pitches, `pitches`).
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

/// The facts are set as [`set_facts`] says: letters and words [`FACT_PITCH`]
/// apart, every two facts [`GAP`] pitches apart, the line a whole number of
/// cells (as loose as its glyphs need), its first and last glyphs half a pitch
/// in from its ends, every glyph in a cell of its own, in order, within a
/// nudge's reach. Control: one cell per character would take more cells than
/// the line does.
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
    // A long path and branch, with few spaces among their glyphs: the line
    // is as loose as one cell a glyph needs, the facts as far apart.
    let long = FooterFacts {
        path: Some("~/Developer/aterm-typo-20261014/crates/aterm-gui".into()),
        branch: Some("ui/footer-spacing-iosevka-20261014".into()),
        ..facts.clone()
    };
    check(
        &long,
        "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/Developer/aterm-typo-20261014/crates/aterm-gui   \u{2387} ui/footer-spacing-iosevka-20261014",
    );
    // Wide characters take two cells each, and stay in order.
    let wide = FooterFacts {
        branch: Some("\u{6F22}\u{5B57}".into()),
        ..facts.clone()
    };
    let set = check(
        &wide,
        "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} \u{6F22}\u{5B57}",
    );
    assert_eq!(set.glyphs.iter().filter(|g| g.cols == 2).count(), 2);
    // A path no nudge reaches at FACT_PITCH: set looser, evenly.
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
        let set = set_facts(&[Segment {
            mark: ACCOUNT_MARK,
            text: email.into(),
        }]);
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
        let mut account = [Segment {
            mark: ACCOUNT_MARK,
            text: email.into(),
        }];
        assert!(shorten_subscription_account(&mut account, 8));
        assert_eq!(account[0].text, expected);
        assert_eq!(text_width(&account[0].text), 8);
    }
}

/// The order of giving way ([`fit_rule`]): a light's transient title first
/// (whole, then short, then none), then the path is cut to `…/<dir>`, the
/// branch goes, the path, the effort, then the chips; the model last. A light's
/// reason outranks the branch and the path, never the model and effort.
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
    // With `Σ` and a limit wall: `Σ` narrows to the working time and
    // goes first; the wall outlasts every other fact, standing alone
    // last; a rule too narrow even for it keeps the model, never an
    // empty rule while one fits.
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
    // With a light drawn and titled: the title gives way first, then the
    // facts, then the chips; the model outlasts them all.
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
    // A title with leaner forms (Autopilot's, design §9.2.1) gives way
    // through each of them, widest first, before its short form — and all of
    // them before any fact; a form of width 0 is none and is skipped.
    // NEGATIVE CONTROL: the same steps with no leaner form go whole → short.
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
    // A face whose explanation is no longer than the menu's line has no form
    // with both clicks (`claude_lights::lean_titles`): v1's words, then the
    // click alone.
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
    // At 80 columns (room 76) a hovered light takes nothing from the
    // typical facts: the title gives way instead.
    let owners = FooterFacts {
        path: Some("~/aterm".into()),
        ..facts.clone()
    };
    let fit = fit_rule(&owners, rule_room(&(0..80)), lights);
    assert_eq!(fit.segments, segments(&owners), "{fit:?}");
    assert!(fit.chips && fit.title != TitleFit::Full, "{fit:?}");
    // A reason: the whole title goes, then the path's head; the reason
    // then takes branch and path room, never model or effort, else it
    // goes and the facts return.
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
    // At 80 columns typical facts give the reason the branch's and
    // the path's room; a hovered title of the same width does not.
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
    // Chips narrower than the model still draw where the model cannot.
    let dot = Some(LightsWidth {
        chips: 6,
        title: 0,
        lean: [0; LEAN_FORMS],
        short: 0,
        reason: false,
    });
    let fit = fit_rule(&facts, 9, dot);
    assert!(fit.chips && fit.segments.is_empty(), "{fit:?}");
    // `chips_room` is exactly the room the chips have beside the model.
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

/// What a fit shows, as `<mark> <text>` each.
fn rung_texts(fit: &RuleFit) -> Vec<String> {
    fit.segments
        .iter()
        .map(|s| format!("{} {}", s.mark, s.text))
        .collect()
}

/// Every distinct run of facts a rule shows as it narrows a cell at a
/// time, from the whole facts' width to nothing, with no lights.
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

/// The facts `a_narrow_rule_gives_way_title_first_and_the_model_last`
/// walks: 44 cells whole.
fn narrow_rule_facts() -> FooterFacts {
    FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/src/github.com/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    }
}

/// [`narrow_rule_facts`] with that test's tokens, runtime and limit wall.
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

/// [`narrow_rule_walled`] with no limit wall: the tokens alone.
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

/// The account costs only the tokens: at every width the wall, model, path and
/// branch shown are those of the same facts with no account, and the label is
/// whole or absent. Negative control: with no account every rung is the one
/// `a_narrow_rule_gives_way_title_first_and_the_model_last` pins.
#[test]
fn the_account_costs_only_the_tokens() {
    let plain = narrow_rule_walled();
    let named = |label: &str, facts: &FooterFacts| FooterFacts {
        account: Some(label.into()),
        ..facts.clone()
    };
    let facts = named("work-2", &plain);
    let walk = walk_down(&facts);
    // The first rung to lose a segment loses the tokens, and nothing else.
    let fewer = walk
        .iter()
        .find(|rung| rung.len() < walk[0].len())
        .expect("a narrower rung");
    assert_eq!(fewer.as_slice(), &walk[0][..5], "{fewer:?}");
    let bare = narrow_rule_facts();

    // The law, at every width, for a short label, one of a runtime's
    // width and one wider than the rule's whole room at 80 columns.
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
                        // Beside a model it would not have fitted.
                        if let Some(at) = kept.iter().position(|s| s.mark == MODEL_MARK) {
                            let mut with = kept.clone();
                            with.insert(
                                at + 1,
                                Segment {
                                    mark: ACCOUNT_MARK,
                                    text: label.into(),
                                },
                            );
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

    // Negative control: the named walk is the unnamed one (pinned in
    // `a_narrow_rule_gives_way_title_first_and_the_model_last`) with the
    // account's rungs among it.
    let unnamed_of = |walk: &[Vec<String>]| -> Vec<Vec<String>> {
        walk.iter()
            .filter(|rung| !rung.iter().any(|s| s.starts_with(ACCOUNT_MARK)))
            .cloned()
            .collect()
    };
    let pinned = walk_down(&plain);
    // Rounded to whole cells, an account rung and the next one without it can
    // take the same width, and the earlier shows: the named walk without its
    // account rungs is the pinned walk with at most such rungs missing.
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

#[test]
fn subscription_chip_budget_keeps_account_and_weekly_beside_real_gui_faces() {
    for email in ["owner@example.com", "使用者與工作帳號在公司@example.com"] {
        let facts = FooterFacts {
            subscription: Some(super::super::profile::SubscriptionDisplay {
                email: Some(email.into()),
                plan: Some("Max 5x".into()),
                weekly: Some("weekly 29% left".into()),
            }),
            ..narrow_rule_facts()
        };
        for columns in [80, 100, 120] {
            let room = rule_room(&(0..columns));
            let budget = chips_room(&facts, room);
            // The GUI's faces at rest: `∞` (1 cell) and `⤓ land` (6); then
            // `∞ ⏸ plan` and its wider light faces (3, 8, 10 and 14 were
            // measured while a `⚙` chip, since removed, stood beside them, so
            // they cover wider rows). The last case exercises a block that
            // consumes its whole advertised budget.
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

/// A light's reason and a live upgrade's note, short, each take the account's
/// room where the two do not fit together, never the limit wall's or the model
/// and effort's. Negative controls: a hovered title of the same widths takes
/// nothing, and with no account the reason and the note stand at those widths.
#[test]
fn a_reason_and_the_upgrade_note_take_the_accounts_room() {
    let plain = narrow_rule_walled();
    let facts = FooterFacts {
        account: Some("work-2".into()),
        ..plain.clone()
    };
    let whole = segments(&facts);
    // What a reason never takes the room of: the wall and the model.
    let top: Vec<Segment> = whole
        .iter()
        .filter(|s| s.mark == WALL_MARK || s.mark == MODEL_MARK)
        .cloned()
        .collect();
    assert_eq!(top.len(), 2);
    let named = |fit: &RuleFit| fit.segments.iter().any(|s| s.mark == ACCOUNT_MARK);
    // The fit with the account laid in after the model.
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
    // Rooms where the reason is shown: with the account, and without it.
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
    // Negative control: a hovered title takes nothing from the facts.
    let hover = reason.map(|l| LightsWidth { reason: false, ..l });
    for room in 0..=240 {
        let fit = fit_rule(&facts, room, hover);
        assert!(
            fit.title == TitleFit::None || fit.segments == whole,
            "{room}: {fit:?}"
        );
    }
    // Negative control: with no account, the reason stands at each room it
    // took the account's at.
    for &room in &instead {
        let fit = fit_rule(&plain, room, reason);
        assert_ne!(fit.title, TitleFit::None, "{room}: {fit:?}");
    }

    // The upgrade's note, the same way.
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
    // Negative control: with no account, the note stands at each of them.
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

/// The launch card is read only under a live REPL. Negative controls: a
/// relaunch before the new REPL is up (`Press Ctrl-C again to exit`), a
/// half-drawn screen, and a box over the composer.
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
    // The phase reader alone would take the predecessor's card there.
    assert!(
        aterm_phase::launch_card(&fixture("claude-2.1.283-inline-relaunch-before-repl.txt"))
            .is_some()
    );
    // Default mode draws a pill too (`⏸ manual mode on`), so the card is
    // read under it.
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
    // The pill-less row also proves a live REPL: its hint slot at rest,
    // with a draft, mid-turn.
    let bypass = fixture("claude-2.1.283-footer-bypass-idle.txt");
    let at = mode_row(&bypass).expect("the measured mode row");
    for (bare, live) in [
        ("  ? for shortcuts", true),
        ("  esc to clear", true),
        ("  esc to interrupt \u{00B7} \u{2190} for agents", true),
        // Negative controls: the dead box, a completion list, shell mode.
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

/// A kept card fills only what nothing newer decided, and only from the
/// running build's card.
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
    // After an effort-less switch the card's effort counts only when the
    // card names the model switched to.
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
}

/// Found under the composer's bottom rule only — a quoted copy of the row
/// in the conversation above the frame is not the footer.
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

/// The Bash tool's directory is the newest main-thread row's `cwd`;
/// subagent rows, rows without one and relative values are passed over.
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

/// [`shell_cwds`] reads only live sessions launched in the reported
/// directory; other launches, dead pids and the launch directory add
/// nothing.
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
    // Nothing registered yet: nothing learned.
    assert!(shell_cwds(&claude, &launch).is_empty());
    // This process stands in for a live Claude Code whose Bash tool moved.
    session(me, "a-live", &launch, &launch.join("nested"));
    assert_eq!(shell_cwds(&claude, &launch), [launch.join("nested")]);
    // Another launch directory is another session's business.
    assert!(shell_cwds(&claude, &root.join("elsewhere")).is_empty());
    // Negative control: a pid no process holds is a leftover file.
    session(999_999_990, "b-dead", &launch, &root.join("dead"));
    assert_eq!(shell_cwds(&claude, &launch), [launch.join("nested")]);
    // A Bash tool still in the launch directory adds nothing new.
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
    // A failed read keeps no excerpts.
    std::fs::remove_file(&path).unwrap();
    assert!(cache.read_since(&path, 4242, Some(start), start).is_none());
    assert!(cache.excerpts().is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// A COLD READ STOPS AT ITS BOUND: a prompt more than [`MAX_BACK_BYTES`]
/// above the end is not looked for, and the survey names none (`-`), while
/// the newest words and the call in flight are still read. NEGATIVE CONTROL:
/// the same prompt under less than the bound is found.
#[cfg(unix)]
#[test]
fn a_cold_back_read_past_its_bound_names_no_prompt() {
    let root = cache_test_dir("excerpts-cold");
    let start = crate::harness::upgrade_models::parse_utc("2026-10-15T10:00:00Z").unwrap();
    let at = "2026-10-15T10:01:00Z";
    // One tool result of about 100 KiB: a long run's filler.
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

/// THE ACT'S LABEL NEVER TAKES A FACT'S ROOM (design §9.2.1, §9.2.5,
/// §9.2.7): a transient title with no short form — the act ticker's `✓
/// approved` beside the Autopilot chip — is drawn whole or not at all, at
/// every room from wide to none, beside the facts exactly as the same room
/// draws them with no title: it gives way before any of them. Over the
/// owner's typical facts, whose rungs differ by less than the label, that is
/// beside the whole facts, down to its own width and no narrower. NEGATIVE
/// CONTROL: a hover's title of the same width, which has a short form, is
/// drawn short beside the whole facts at a room the label is not drawn in.
#[test]
fn the_acts_label_is_drawn_whole_or_not_at_all_never_at_a_facts_cost() {
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    // `✓ approved` and its two-cell gap; the Autopilot chip, `∞`.
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
        // The facts alone, in the same room, are what the label leaves.
        let bare = fit_rule(&facts, room, label.map(|l| LightsWidth { title: 0, ..l }));
        assert_eq!(fit.segments, bare.segments, "{room}: no fact gave way");
    }
    let least = *drawn.last().expect("drawn in a wide rule");
    assert_eq!(
        least,
        fit_rule(&facts, 90, label).width(label),
        "drawn down to its own width, and no narrower"
    );
    // NEGATIVE CONTROL: a title with a short form is drawn short there.
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

/// The Together run's Claude Code working directory (2026-10-16): outside
/// HOME, so the facts carry it whole, and a 120-column split pane cuts it to
/// `…/proj-claude`.
const SPLIT_PATH: &str = "/private/tmp/claude-502/-Users-person-aterm/\
    dd11ed0c-05cd-46e8-bf09-5b2786e48b6a/scratchpad/together/run-dir/proj-claude";

/// A TITLE STANDS BESIDE THE FACTS AS THEY FIT WITHOUT IT (design §9.2.7;
/// the Together run's D2): in a split pane whose long working directory is
/// cut to `…/proj-claude`, ninety cells of the rule are free, and the act's
/// label and the hovered Autopilot title are drawn there — the label whole,
/// the title in the widest form that fits. At every room from a wide rule to
/// none: the fit never passes the rule's ends; the facts and the chips are
/// exactly what the same room draws with no title (a title never makes a
/// fact give way); and the title is the widest of its forms that fits
/// beside them, or none. Where nothing but the chips fits, or not even they,
/// the rule drops the title cleanly: no short form of the label, nothing
/// past the room.
///
/// NEGATIVE CONTROL (the fix mutated out, the title paired with the WHOLE
/// facts only, as before): at 120 columns neither is drawn, and the walk
/// meets no room where a title stands beside a fallen rung — the asserts
/// marked RED fail.
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
    // `∞ ⏸ manual`: the Autopilot chip and Claude's mode light.
    let chips = "\u{221e} \u{23f8} manual".chars().count();
    let label = LightsWidth {
        chips,
        title: "\u{2713} approved".chars().count() + 2,
        lean: [0; LEAN_FORMS],
        short: 0,
        reason: false,
    };
    // Cruise's hovered title with no act taken (§9.2.1): the whole, v1's
    // words (no trail to give up first), the phrase with both clicks, with
    // the click alone, and the mode phrase.
    let whole_title = "autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land \u{b7} \
                       right-click: more";
    let v1 = "autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land";
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
        // RED without the fix.
        assert!(
            beside_a_fallen_rung,
            "{lights:?}: a title stands beside a rung the room cut"
        );
    }
    // The Together run's pane, 120 columns: the path cut, the label and the
    // title drawn beside it. RED without the fix.
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
        cut(&hovered) && hovered.title == TitleFit::Lean(1),
        "v1's words beside `…/proj-claude`: {hovered:?}"
    );
    // Where the label does not fit beside what fits, it goes cleanly: the
    // chips stand with the facts and no title. In no room, nothing at all.
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

/// THE STEP TAP'S SIDE OF ONE REGISTRY READ (session log Phase 3B, S1 as
/// built): the lookup says whether the transcript's path is CERTAIN — the
/// project's own file, or the one file a scan finds — and a read that found
/// none tells an armed tap so, so the file Claude Code writes next is bound
/// from its first byte. NEGATIVE CONTROLS: two scanned candidates are not
/// certain; a tap that never saw the file absent binds it at its length.
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
    // Armed while no file exists: the tap waits, then binds from byte 0.
    let mut waited = FooterCache::default();
    waited.set_floor(0, None);
    assert!(read(&mut waited).is_empty() && !waited.steps_bound());
    append_to(&transcript, &served_row("m1", two_pm - 60));
    let evs = read(&mut waited);
    assert_eq!(evs.first(), Some(&StepEvent::Bind { len: 0 }), "{evs:?}");
    assert!(waited.steps_bound());
    // NEGATIVE CONTROL: armed over the file as it stands, its rows history.
    let len = std::fs::metadata(&transcript).unwrap().len();
    let mut over = FooterCache::default();
    over.set_floor(0, None);
    assert_eq!(read(&mut over), vec![StepEvent::Bind { len }]);
    std::fs::remove_dir_all(root).unwrap();
}
