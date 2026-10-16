// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`owner_asks`] over settings chains written in scratch directories (never
//! the owner's files), and [`rule_tool`] over the rule spellings Claude Code
//! takes.

use super::*;
use crate::supervise::ScratchDir;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

fn asks(rules: &[&str]) -> String {
    let list: Vec<String> = rules.iter().map(|r| format!("{r:?}")).collect();
    format!(r#"{{"permissions":{{"ask":[{}]}}}}"#, list.join(","))
}

/// `(rule as written, its settings document)` of each bare rule found.
fn found(got: Result<Vec<OwnerAsk>, String>) -> Vec<(String, String)> {
    got.expect("the chain reads")
        .into_iter()
        .filter(|a| matches!(a.kind, AskKind::Rule { bare: true, .. }))
        .map(|a| (a.text, a.source))
        .collect()
}

/// The tool of each bare rule `got` holds, in order.
fn bare_tools(got: Result<Vec<OwnerAsk>, String>) -> Vec<String> {
    got.expect("reads")
        .into_iter()
        .filter_map(|a| match a.kind {
            AskKind::Rule { tool, bare: true } => Some(tool),
            _ => None,
        })
        .collect()
}

/// Claude Code reads a rule as its bare tool with no parentheses, `()` or
/// `(*)`, and `Tool(content)` otherwise as the tool with a specifier. A
/// malformed rule names no tool.
#[test]
fn a_rule_names_its_tool_bare_or_with_a_specifier() {
    for (rule, named) in [
        ("Write", Some(("Write", true))),
        ("Edit()", Some(("Edit", true))),
        ("Read(*)", Some(("Read", true))),
        ("Edit(src/**)", Some(("Edit", false))),
        ("Read(~/x/**)", Some(("Read", false))),
        ("Bash(touch:*)", Some(("Bash", false))),
        ("Bash(sed:*)", Some(("Bash", false))),
        ("Write(**)", Some(("Write", false))),
        ("Write(x", None),
        ("Write)(", None),
        ("(*)", None),
        ("", None),
    ] {
        assert_eq!(rule_tool(rule), named, "{rule:?}");
    }
}

/// The agent's own chain: the user's file in its Claude Code directory, the
/// project's two files, the main worktree's local one and a `--settings`; a
/// rule with a specifier is no bare rule. Negative control: the same files
/// elsewhere hold nothing for it.
#[test]
fn the_agents_own_settings_chain_is_read_for_bare_ask_rules() {
    let s = ScratchDir::new("claude-settings", "chain");
    let claude = s.join("claude");
    let proj = s.join("proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let worker = WorkerEnv::hermetic(None).with(&format!("CLAUDE_CONFIG_DIR={}", claude.display()));
    assert!(
        found(owner_asks(&worker, &proj)).is_empty(),
        "nothing written yet"
    );

    let user = claude.join("settings.json");
    let project = proj.join(".claude/settings.json");
    let local = proj.join(".claude/settings.local.json");
    write(&user, &asks(&["Write", "Edit(src/**)"]));
    write(
        &project,
        r#"{"permissions":{"allow":["Edit"],"ask":["Read(*)","Bash(git push:*)"]}}"#,
    );
    write(&local, &asks(&["Edit()"]));
    let name = |p: &Path| p.display().to_string();
    assert_eq!(
        found(owner_asks(&worker, &proj)),
        [
            ("Edit()".to_string(), name(&local)),
            ("Read(*)".to_string(), name(&project)),
            ("Write".to_string(), name(&user)),
        ]
    );
    let specified: Vec<(String, String)> = owner_asks(&worker, &proj)
        .expect("reads")
        .into_iter()
        .filter(|a| matches!(a.kind, AskKind::Rule { bare: false, .. }))
        .map(|a| (a.text, a.source))
        .collect();
    assert_eq!(
        specified,
        [
            ("Bash(git push:*)".to_string(), name(&project)),
            ("Edit(src/**)".to_string(), name(&user)),
        ]
    );
    // The same Claude Code directory by `$HOME`, and a `--settings` given
    // inline: its rules join the chain.
    let by_home = WorkerEnv::from_env(vec![format!("HOME={}", s.display())]).launched_with(&[
        "claude",
        r#"--settings={"permissions":{"ask":["NotebookEdit"]}}"#,
    ]);
    std::fs::rename(&claude, s.join(".claude")).expect("move the Claude directory");
    let tools = bare_tools(owner_asks(&by_home, &proj));
    assert_eq!(tools, ["NotebookEdit", "Edit", "Read", "Write"]);
    // Negative control: an agent under another Claude Code directory, launched
    // elsewhere, has none of these.
    let other = s.join("other");
    std::fs::create_dir_all(&other).expect("other");
    let elsewhere =
        WorkerEnv::hermetic(None).with(&format!("CLAUDE_CONFIG_DIR={}", s.join("none").display()));
    assert!(found(owner_asks(&elsewhere, &other)).is_empty());
}

/// An agent in a linked worktree is read for the main worktree's
/// `.claude/settings.local.json` as well as its own.
#[test]
fn a_linked_worktree_reads_the_main_worktrees_local_settings() {
    let s = ScratchDir::new("claude-settings", "worktree");
    let main = s.join("main");
    let wt = s.join("wt");
    let gitdir = main.join(".git/worktrees/wt");
    std::fs::create_dir_all(&gitdir).expect("gitdir");
    write(&gitdir.join("commondir"), "../..\n");
    write(&wt.join(".git"), &format!("gitdir: {}\n", gitdir.display()));
    let sub = wt.join("crates/x");
    std::fs::create_dir_all(&sub).expect("sub");
    let worker = WorkerEnv::hermetic(None);
    assert!(found(owner_asks(&worker, &sub)).is_empty());
    let local = main.join(".claude/settings.local.json");
    write(&local, &asks(&["Write"]));
    assert_eq!(
        found(owner_asks(&worker, &sub)),
        [("Write".to_string(), local.display().to_string())]
    );
}

/// A document that cannot be read whole, does not parse, or holds a malformed
/// ask list is an error naming it. Negative control: no `permissions` asks
/// nothing.
#[test]
fn a_settings_file_that_cannot_be_read_is_reported_not_skipped() {
    let s = ScratchDir::new("claude-settings", "unread");
    let proj = s.join("proj");
    let project = proj.join(".claude/settings.json");
    let worker = WorkerEnv::hermetic(None);
    let padded = format!(
        "{{\"permissions\":{{\"ask\":[\"Write\"]}},\"pad\":\"{}\"}}",
        "x".repeat(2 << 20)
    );
    for (body, says) in [
        ("{ not json", "do not parse"),
        (r#"{"permissions":{"ask":"Write"}}"#, "is no list"),
        (r#"{"permissions":{"ask":[["Write"]]}}"#, "is no string"),
        (padded.as_str(), "are larger than"),
    ] {
        write(&project, body);
        let why = owner_asks(&worker, &proj).expect_err(body);
        assert!(
            why.contains(says) && why.contains(&project.display().to_string()),
            "{why}"
        );
    }
    std::fs::remove_file(&project).expect("rm");
    std::fs::create_dir(&project).expect("a directory where the file goes");
    assert!(
        owner_asks(&worker, &proj)
            .expect_err("a directory")
            .contains("cannot be read as a regular file")
    );
    std::fs::remove_dir(&project).expect("rmdir");
    write(&project, r#"{"env":{"X":"1"}}"#);
    assert!(found(owner_asks(&worker, &proj)).is_empty());
}

/// Each PreToolUse entry that names a hook is read with its matcher; an empty
/// `hooks` list and another event hold nothing; a field of the wrong type
/// makes what the settings ask unknown, naming the document.
#[test]
fn the_chains_pre_tool_use_hooks_are_read_with_their_matchers() {
    let s = ScratchDir::new("claude-settings", "hooks");
    let proj = s.join("proj");
    let project = proj.join(".claude/settings.json");
    let worker = WorkerEnv::hermetic(None);
    let run = r#"[{"type":"command","command":"exit 0"}]"#;
    write(
        &project,
        &format!(
            r#"{{"hooks":{{"PreToolUse":[{{"matcher":"Bash","hooks":{run}}},{{"hooks":{run}}},{{"matcher":"Edit|Write","hooks":[]}}],"PostToolUse":[{{"matcher":"*","hooks":{run}}}]}}}}"#
        ),
    );
    let hooks: Vec<(Option<String>, String)> = owner_asks(&worker, &proj)
        .expect("reads")
        .into_iter()
        .filter_map(|a| match a.kind {
            AskKind::Hook { matcher } => Some((matcher, a.text)),
            AskKind::Rule { .. } => None,
        })
        .collect();
    assert_eq!(
        hooks,
        [
            (Some("Bash".to_string()), "Bash".to_string()),
            (None, "(none)".to_string()),
        ]
    );
    for (body, says) in [
        (r#"{"hooks":[]}"#, "hold a `hooks` that is no object"),
        (
            r#"{"hooks":{"PreToolUse":{"matcher":"Bash"}}}"#,
            "hold a `hooks.PreToolUse` that is no list",
        ),
        (
            r#"{"hooks":{"PreToolUse":[{"matcher":["Bash"],"hooks":[]}]}}"#,
            "hold a PreToolUse matcher that is no string",
        ),
    ] {
        write(&project, body);
        let why = owner_asks(&worker, &proj).expect_err(body);
        assert!(
            why.contains(says) && why.contains(&project.display().to_string()),
            "{why}"
        );
    }
}

/// None, empty and `*` match every tool; names alone match the tools they
/// name; anything else is a regex, taken to match. Only a bare rule asks every
/// call.
#[test]
fn a_hook_matcher_matches_bash_as_claude_code_matches_it() {
    for (matcher, matches) in [
        (None, true),
        (Some(""), true),
        (Some("*"), true),
        (Some("Bash"), true),
        (Some("Edit|Bash"), true),
        (Some("Write, Bash"), true),
        (Some(" Bash |Read"), true),
        (Some("Edit|Write"), false),
        (Some("MultiEdit"), false),
        (Some("bash"), false),
        (Some("Bas"), false),
        (Some(".*"), true),
        (Some("mcp__.*"), true),
        (Some("^Ba"), true),
    ] {
        assert_eq!(hook_may_match(matcher, "Bash"), matches, "{matcher:?}");
    }
    let ask = |kind: AskKind| OwnerAsk {
        kind,
        text: String::new(),
        source: String::new(),
    };
    let rule = |tool: &str, bare: bool| {
        ask(AskKind::Rule {
            tool: tool.to_string(),
            bare,
        })
    };
    assert!(rule("Bash", true).asks_every("Bash") && rule("Bash", true).may_ask("Bash"));
    assert!(!rule("Bash", false).asks_every("Bash") && rule("Bash", false).may_ask("Bash"));
    assert!(!rule("Edit", true).may_ask("Bash"));
    let hook = ask(AskKind::Hook {
        matcher: Some("*".to_string()),
    });
    assert!(hook.may_ask("Bash") && !hook.asks_every("Bash"));
}

/// An empty or blank settings file reads as `{}` and a leading byte-order mark
/// is stripped, never `do not parse` (which would hold every file box).
/// Negative control: text that is not JSON still does not parse.
#[test]
fn an_empty_or_marked_settings_file_reads_as_claude_code_reads_it() {
    let s = ScratchDir::new("claude-settings", "empty");
    let proj = s.join("proj");
    let project = proj.join(".claude/settings.json");
    let worker = WorkerEnv::hermetic(None);
    for body in ["", " \n\t\r\n", "\u{feff}", "\u{feff}  \n"] {
        write(&project, body);
        assert!(found(owner_asks(&worker, &proj)).is_empty(), "{body:?}");
    }
    write(&project, &format!("\u{feff}{}", asks(&["Write"])));
    assert_eq!(
        found(owner_asks(&worker, &proj)),
        [("Write".to_string(), project.display().to_string())]
    );
    for body in ["{ not json", "\u{feff}{ not json"] {
        write(&project, body);
        let why = owner_asks(&worker, &proj).expect_err(body);
        assert!(why.contains("do not parse"), "{why}");
    }
}

/// A relative `CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_USE_COWORK_PLUGINS` and
/// `--project-config-root` move the documents as Claude Code moves them.
/// Negative controls: a false cowork flag keeps `settings.json`; without the
/// root flag the launch directory's files are the project's.
#[test]
fn the_chain_is_read_where_claude_code_reads_it() {
    let s = ScratchDir::new("claude-settings", "where");
    let proj = s.join("proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let relative = WorkerEnv::hermetic(None).with("CLAUDE_CONFIG_DIR=cc");
    let user = proj.join("cc/settings.json");
    write(&user, &asks(&["Write"]));
    assert_eq!(
        found(owner_asks(&relative, &proj)),
        [("Write".to_string(), user.display().to_string())]
    );

    let claude = s.join("claude");
    let dir = format!("CLAUDE_CONFIG_DIR={}", claude.display());
    let cowork = claude.join("cowork_settings.json");
    write(&claude.join("settings.json"), &asks(&["Read"]));
    write(&cowork, &asks(&["Edit"]));
    let tools = |w: &WorkerEnv, launch: &Path| -> Vec<String> { bare_tools(owner_asks(w, launch)) };
    let on = WorkerEnv::hermetic(None)
        .with(&dir)
        .with("CLAUDE_CODE_USE_COWORK_PLUGINS= Yes");
    assert_eq!(tools(&on, &s), ["Edit"]);
    let off = WorkerEnv::hermetic(None)
        .with(&dir)
        .with("CLAUDE_CODE_USE_COWORK_PLUGINS=0");
    assert_eq!(tools(&off, &s), ["Read"]);

    let root = s.join("root");
    write(
        &root.join(".claude/settings.json"),
        &asks(&["NotebookEdit"]),
    );
    write(&root.join(".claude/settings.local.json"), &asks(&["Glob"]));
    write(&proj.join(".claude/settings.json"), &asks(&["Grep"]));
    let plain = WorkerEnv::hermetic(None).with(&dir);
    assert_eq!(tools(&plain, &proj), ["Grep", "Read"]);
    for argv in [
        vec![
            "claude".to_string(),
            "--project-config-root".to_string(),
            "../root".to_string(),
        ],
        vec![
            "claude".to_string(),
            format!("--project-config-root={}", root.display()),
        ],
    ] {
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let rooted = WorkerEnv::hermetic(None).with(&dir).launched_with(&argv);
        assert_eq!(
            tools(&rooted, &proj),
            ["Glob", "NotebookEdit", "Read"],
            "{argv:?}"
        );
    }
}

/// The managed file and its sorted `*.json` drop-ins open the chain. Negative
/// control: with no managed directory there are none.
#[test]
fn the_managed_file_and_its_drop_ins_open_the_chain() {
    let s = ScratchDir::new("claude-settings", "managed");
    let managed = s.join("managed");
    let proj = s.join("proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let worker = WorkerEnv::hermetic(None);
    let there = |managed: Option<&Path>| -> Vec<String> {
        sources_under(&worker, &proj, managed, None)
            .into_iter()
            .filter(|s| !matches!(s.text, Ok(None)))
            .map(|s| s.name)
            .collect()
    };
    assert!(there(Some(&managed)).is_empty(), "nothing there yet");
    write(&managed.join("managed-settings.json"), "{}");
    write(&managed.join("managed-settings.d/20-b.json"), "{}");
    write(&managed.join("managed-settings.d/10-a.json"), "{}");
    write(&managed.join("managed-settings.d/notes.txt"), "x");
    let named = |p: &str| managed.join(p).display().to_string();
    assert_eq!(
        there(Some(&managed)),
        [
            named("managed-settings.json"),
            named("managed-settings.d/10-a.json"),
            named("managed-settings.d/20-b.json"),
        ]
    );
    assert!(there(None).is_empty(), "no managed directory");
}

/// Each managed preference domain that is there (the account's, by `USER`
/// else `LOGNAME`, then the device's) is reported unopened. Negative controls:
/// none there, none reported; another account's is not this worker's.
#[test]
fn a_managed_preference_domain_is_a_document_this_reader_cannot_see() {
    let s = ScratchDir::new("claude-settings", "prefs");
    let prefs = s.join("prefs");
    std::fs::create_dir_all(&prefs).expect("prefs");
    let worker = |kv: &str| WorkerEnv::hermetic(None).with(kv);
    let reported = |w: &WorkerEnv| -> Vec<String> {
        managed_preferences(w, &prefs)
            .into_iter()
            .map(|s| {
                assert!(s.text.is_err(), "{}", s.name);
                s.name
            })
            .collect()
    };
    assert!(reported(&worker("USER=owner")).is_empty());
    let mine = prefs.join("owner").join(MANAGED_PREFERENCES_FILE);
    write(&mine, "plist");
    write(&prefs.join("other").join(MANAGED_PREFERENCES_FILE), "plist");
    assert_eq!(
        reported(&worker("USER=owner")),
        [mine.display().to_string()]
    );
    assert_eq!(
        reported(&worker("LOGNAME=owner")),
        [mine.display().to_string()]
    );
    assert!(reported(&worker("USER=nobody")).is_empty());
    let device = prefs.join(MANAGED_PREFERENCES_FILE);
    write(&device, "plist");
    assert_eq!(
        reported(&worker("USER=owner")),
        [mine.display().to_string(), device.display().to_string()]
    );
}

/// The route's fence reports a settings document in a file-provider domain
/// unopened, through a symlink too; the same file elsewhere is read. Negative
/// control: unfenced, it is read.
#[test]
fn a_settings_file_under_a_file_provider_domain_is_reported_never_opened() {
    let s = ScratchDir::new("claude-settings", "provider");
    let home = std::fs::canonicalize(&*s).expect("canonical").join("home");
    let proj = home.join("Library/CloudStorage/x");
    let provider = proj.join(".claude/settings.json");
    let ordinary = home.join("src/.claude/settings.json");
    for file in [&provider, &ordinary] {
        write(file, "{}");
    }
    let mut fenced = vec![provider.clone()];
    #[cfg(unix)]
    {
        let linked = home.join("src/.claude/settings.local.json");
        std::os::unix::fs::symlink(&provider, &linked).expect("a link into the domain");
        fenced.push(linked);
    }
    for fenced in &fenced {
        assert!(
            read_settings_in(Some(&home), fenced)
                .expect_err("a file-provider domain")
                .contains("file-provider domain"),
            "{}",
            fenced.display()
        );
    }
    assert_eq!(
        read_settings_in(Some(&home), &ordinary),
        Ok(Some("{}".to_string()))
    );
    assert_eq!(
        read_settings_in(None, &provider),
        Ok(Some("{}".to_string()))
    );
    let worker = WorkerEnv::hermetic(None).with(&format!("HOME={}", home.display()));
    let text = |fence: Option<&Path>| {
        sources_under(&worker, &proj, None, fence)
            .into_iter()
            .find(|s| s.name == provider.display().to_string())
            .map(|s| s.text)
    };
    assert!(text(Some(&home)).expect("listed").is_err());
    assert_eq!(text(None), Some(Ok(Some("{}".to_string()))));
}

/// The route's fence covers the git files the chain opens to find a main
/// worktree: fenced, neither is opened and the main worktree's local settings
/// are reported. Negative control: unfenced, both are read.
#[test]
fn the_routes_fence_opens_no_git_file_in_a_file_provider_domain() {
    let s = ScratchDir::new("claude-settings", "provider-git");
    let base = std::fs::canonicalize(&*s).expect("canonical");
    let home = base.join("home");
    let cloud = home.join("Library/CloudStorage/SyncProvider-acct");
    let main = base.join("main");
    std::fs::create_dir_all(main.join(".git")).expect("the main worktree");
    let local = main.join(".claude/settings.local.json");
    write(&local, &asks(&["Write"]));
    let worker = WorkerEnv::hermetic(None).with(&format!("HOME={}", home.display()));
    let main_local = "the main worktree's .claude/settings.local.json";
    // A git directory in the domain, for a worktree outside it (its
    // `commondir` leading back out), and a worktree in the domain whose git
    // directory is outside it.
    let outside = base.join("wt");
    let gitdir = cloud.join("wtgit");
    write(
        &gitdir.join("commondir"),
        &format!("{}\n", main.join(".git").display()),
    );
    write(
        &outside.join(".git"),
        &format!("gitdir: {}\n", gitdir.display()),
    );
    let inside = cloud.join("wt");
    let home_gitdir = main.join(".git/worktrees/wt");
    write(&home_gitdir.join("commondir"), "../..\n");
    write(
        &inside.join(".git"),
        &format!("gitdir: {}\n", home_gitdir.display()),
    );
    for wt in [&outside, &inside] {
        let unfenced = sources_under(&worker, wt, None, None);
        assert!(
            unfenced
                .iter()
                .any(|s| s.name == local.display().to_string() && s.text.is_ok()),
            "{}: {unfenced:?}",
            wt.display()
        );
        let fenced = sources_under(&worker, wt, None, Some(&home));
        assert!(
            fenced.iter().any(|s| s.name == main_local
                && s.text
                    .as_ref()
                    .is_err_and(|why| why.contains("file-provider domain"))),
            "{}: {fenced:?}",
            wt.display()
        );
        assert!(
            !fenced.iter().any(|s| s.name == local.display().to_string()),
            "{}: {fenced:?}",
            wt.display()
        );
    }
}

/// A settings document not there inside a file-provider domain sets nothing,
/// as anywhere else.
#[test]
fn a_settings_file_not_there_in_a_file_provider_domain_sets_nothing() {
    let s = ScratchDir::new("claude-settings", "provider-none");
    let home = std::fs::canonicalize(&*s).expect("canonical").join("home");
    let proj = home.join("Library/CloudStorage/SyncProvider-acct/proj");
    std::fs::create_dir_all(&proj).expect("proj");
    for missing in [
        proj.join(".claude/settings.local.json"),
        home.join("Library/Mobile Documents/com~apple~CloudDocs/x/.claude/settings.json"),
    ] {
        assert_eq!(
            read_settings_in(Some(&home), &missing),
            Ok(None),
            "{}",
            missing.display()
        );
    }
}

/// The variable naming, in [`provider_home_stage`]'s child run, a project
/// inside a file-provider domain of its scratch `HOME`.
const PROVIDER_STAGE: &str = "ATERM_CLAUDE_SETTINGS_PROVIDER_PROJ";

/// Where [`provider_home_stage`] says it ran every check (a child that a
/// filter ran nothing in exits 0 too).
const PROVIDER_STAGE_DONE: &str = "ATERM_CLAUDE_SETTINGS_PROVIDER_DONE";

/// The approval lane opens settings in a file-provider domain of this
/// process's home: none there sets nothing, a local file's ask rules and `env`
/// are read. Runs in a child whose `HOME` is a scratch home
/// ([`provider_home_stage`]).
#[test]
fn the_approval_lane_reads_its_settings_in_a_file_provider_domain_of_this_home() {
    let s = ScratchDir::new("claude-settings", "provider-lane");
    let base = std::fs::canonicalize(&*s).expect("canonical");
    let home = base.join("home");
    let proj = home.join("Library/CloudStorage/SyncProvider-acct/proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let done = base.join("done");
    let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args([
            "supervise::policy::claude_settings::tests::provider_home_stage",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env(PROVIDER_STAGE, &proj)
        .env(PROVIDER_STAGE_DONE, &done)
        .output()
        .expect("the stage runs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the stage failed:\n{said}");
    assert!(done.exists(), "the stage ran no check:\n{said}");
}

/// The child half of
/// [`the_approval_lane_reads_its_settings_in_a_file_provider_domain_of_this_home`],
/// run when [`PROVIDER_STAGE`] is set; otherwise it does nothing.
#[test]
fn provider_home_stage() {
    let (Some(proj), Some(done)) = (
        std::env::var_os(PROVIDER_STAGE).map(PathBuf::from),
        std::env::var_os(PROVIDER_STAGE_DONE).map(PathBuf::from),
    ) else {
        return;
    };
    let home = aterm_types::dirs::home_dir().expect("the stage's HOME");
    assert!(
        atpkg::protected::under_file_provider_domain(&home, &proj),
        "{} is in a file-provider domain of {}",
        proj.display(),
        home.display()
    );
    let worker = WorkerEnv::hermetic(None).with(&format!("HOME={}", home.display()));
    assert_eq!(worker.settings_env(&proj), Ok(()), "nothing there");
    assert!(
        found(owner_asks(&worker, &proj)).is_empty(),
        "nothing there"
    );
    let local = proj.join(".claude/settings.local.json");
    write(
        &local,
        r#"{"permissions":{"allow":["Bash(ls:*)"],"ask":["Write"]}}"#,
    );
    assert_eq!(worker.settings_env(&proj), Ok(()), "a quiet local file");
    assert_eq!(
        found(owner_asks(&worker, &proj)),
        [("Write".to_string(), local.display().to_string())]
    );
    write(&local, r#"{"env":{"GIT_DIR":"elsewhere"}}"#);
    let why = worker.settings_env(&proj).expect_err("an env it names");
    assert!(
        why.contains("gets `GIT_DIR` from Claude Code's settings"),
        "{why}"
    );
    write(&done, "ran");
}

/// A relative `launch` has every document named from it reported, never read
/// against this process's directory. Negative control: what it names
/// absolutely is read.
#[test]
fn an_agent_whose_directory_is_not_known_has_its_project_reported() {
    let s = ScratchDir::new("claude-settings", "blind");
    let team = s.join("team.json");
    write(&team, "{}");
    let claude = s.join("claude");
    write(&claude.join("settings.json"), "{}");
    let absolute = WorkerEnv::hermetic(None)
        .with(&format!("CLAUDE_CONFIG_DIR={}", claude.display()))
        .launched_with(&["claude", "--settings", &team.display().to_string()]);
    for launch in ["", "proj"] {
        let got = sources_under(&absolute, Path::new(launch), None, None);
        let text = |name: &str| {
            got.iter()
                .find(|s| s.name == name)
                .map(|s| s.text.clone())
                .unwrap_or_else(|| panic!("{name} in {got:?}"))
        };
        assert!(text("the project's settings").is_err(), "{launch:?}");
        assert_eq!(
            text(&team.display().to_string()),
            Ok(Some("{}".to_string()))
        );
        assert_eq!(
            text(&claude.join("settings.json").display().to_string()),
            Ok(Some("{}".to_string()))
        );
        let relative = WorkerEnv::hermetic(None)
            .with("CLAUDE_CONFIG_DIR=cc")
            .launched_with(&["claude", "--settings", "team.json"]);
        let got = sources_under(&relative, Path::new(launch), None, None);
        for name in ["--settings team.json", "the user's settings.json"] {
            assert!(
                got.iter().any(|s| s.name == name && s.text.is_err()),
                "{name} in {got:?}"
            );
        }
    }
}
