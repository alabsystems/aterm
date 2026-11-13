// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`owner_asks`] and [`rule_tool`] over Claude Code settings chains.

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

fn found(got: Result<Vec<OwnerAsk>, String>) -> Vec<(String, String)> {
    got.expect("the chain reads")
        .into_iter()
        .filter(|a| matches!(a.kind, AskKind::Rule { bare: true, .. }))
        .map(|a| (a.text, a.source))
        .collect()
}

fn bare_tools(got: Result<Vec<OwnerAsk>, String>) -> Vec<String> {
    got.expect("reads")
        .into_iter()
        .filter_map(|a| match a.kind {
            AskKind::Rule { tool, bare: true } => Some(tool),
            _ => None,
        })
        .collect()
}

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
    let by_home = WorkerEnv::from_env(vec![format!(
        "{}={}",
        crate::harness::accounts::HOMEDIR_ENV,
        s.display()
    )])
    .launched_with(&[
        "claude",
        r#"--settings={"permissions":{"ask":["NotebookEdit"]}}"#,
    ]);
    std::fs::rename(&claude, s.join(".claude")).expect("move the Claude directory");
    let tools = bare_tools(owner_asks(&by_home, &proj));
    assert_eq!(tools, ["NotebookEdit", "Edit", "Read", "Write"]);
    let other = s.join("other");
    std::fs::create_dir_all(&other).expect("other");
    let elsewhere =
        WorkerEnv::hermetic(None).with(&format!("CLAUDE_CONFIG_DIR={}", s.join("none").display()));
    assert!(found(owner_asks(&elsewhere, &other)).is_empty());
}

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
    write(&gitdir.join("commondir"), &"x".repeat(5000));
    assert!(
        owner_asks(&worker, &sub)
            .expect_err("oversized commondir")
            .contains("larger than 4096 bytes")
    );
}

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

#[test]
fn the_chain_is_read_where_claude_code_reads_it() {
    let s = ScratchDir::new("claude-settings", "where");
    let proj = s.join("proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let relative = WorkerEnv::hermetic(None).with("CLAUDE_CONFIG_DIR=cc");
    let user = proj.join("cc").join("settings.json");
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
    let after_ddash = WorkerEnv::hermetic(None).with(&dir).launched_with(&[
        "claude",
        "--",
        "--project-config-root",
        "../root",
        r#"--settings={"permissions":{"ask":["Bash"]}}"#,
    ]);
    assert_eq!(tools(&after_ddash, &proj), ["Grep", "Read"]);
}

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
    let named = |p: &str| {
        p.split('/')
            .fold(managed.clone(), |at, part| at.join(part))
            .display()
            .to_string()
    };
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
    #[cfg_attr(
        not(unix),
        expect(unused_mut, reason = "the link is made on unix alone")
    )]
    let mut fenced = vec![provider.clone()];
    #[cfg(unix)]
    {
        let linked = home.join("src/.claude/settings.local.json");
        std::os::unix::fs::symlink(&provider, &linked).expect("a link into the domain");
        fenced.push(linked);
        let out_link = proj.join(".claude/settings.local.json");
        std::os::unix::fs::symlink(&ordinary, &out_link).expect("a link out of the domain");
        fenced.push(out_link);
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

const PROVIDER_STAGE: &str = "ATERM_CLAUDE_SETTINGS_PROVIDER_PROJ";

const PROVIDER_STAGE_DONE: &str = "ATERM_CLAUDE_SETTINGS_PROVIDER_DONE";

#[test]
fn the_approval_lane_reads_its_settings_in_a_file_provider_domain_of_this_home() {
    if let (Some(proj), Some(done)) = (
        std::env::var_os(PROVIDER_STAGE).map(PathBuf::from),
        std::env::var_os(PROVIDER_STAGE_DONE).map(PathBuf::from),
    ) {
        provider_home_stage(&proj, &done);
        return;
    }
    let s = ScratchDir::new("claude-settings", "provider-lane");
    let base = std::fs::canonicalize(&*s).expect("canonical");
    let home = base.join("home");
    let proj = home.join("Library/CloudStorage/SyncProvider-acct/proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let done = base.join("done");
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args([
                "supervise::policy::claude_settings::tests::the_approval_lane_reads_its_settings_in_a_file_provider_domain_of_this_home",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env(PROVIDER_STAGE, &proj)
            .env(PROVIDER_STAGE_DONE, &done),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("the stage within the hang detector: {e}"));
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the stage failed:\n{said}");
    assert!(done.exists(), "the stage ran no check:\n{said}");
}

fn provider_home_stage(proj: &Path, done: &Path) {
    let home = aterm_types::dirs::home_dir().expect("the stage's HOME");
    assert!(
        atpkg::protected::under_file_provider_domain(&home, proj),
        "{} is in a file-provider domain of {}",
        proj.display(),
        home.display()
    );
    let worker = WorkerEnv::hermetic(None).with(&format!("HOME={}", home.display()));
    assert_eq!(worker.git_settings_env(proj), Ok(()), "nothing there");
    assert!(found(owner_asks(&worker, proj)).is_empty(), "nothing there");
    let local = proj.join(".claude/settings.local.json");
    write(
        &local,
        r#"{"permissions":{"allow":["Bash(ls:*)"],"ask":["Write"]}}"#,
    );
    assert_eq!(worker.git_settings_env(proj), Ok(()), "a quiet local file");
    assert_eq!(
        found(owner_asks(&worker, proj)),
        [("Write".to_string(), local.display().to_string())]
    );
    write(&local, r#"{"env":{"GIT_DIR":"elsewhere"}}"#);
    let why = worker.git_settings_env(proj).expect_err("an env it names");
    assert!(
        why.contains("gets `GIT_DIR` from Claude Code's settings"),
        "{why}"
    );
    write(done, "ran");
}

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

/// WHAT THE AGENT'S SETTINGS MAKE OF `/login` (the post-landing review, 2026-11-02): a
/// `forceLoginMethod` that names a method in any document of the chain, the managed file, the
/// project's, the user's under its own `CLAUDE_CONFIG_DIR` or the server-managed copy beside it,
/// pins the sign-in (`/login` opens the browser at once); a gateway is a pin of its own; a
/// document there that cannot be read, a managed preference domain included, leaves aterm unable
/// to tell; a chain with none of them draws the picker. Negative control: [`login_pin`] reading
/// no source (main's guard, which read no pin) answers the picker for every case.
#[test]
fn the_chains_pinned_sign_in_method_is_read_wherever_claude_code_reads_it() {
    use super::super::login::LoginPin;
    let s = ScratchDir::new("claude-settings", "login-pin");
    let claude = s.join("claude");
    let proj = s.join("proj");
    let managed = s.join("managed");
    let prefs = s.join("prefs");
    std::fs::create_dir_all(&proj).expect("proj");
    std::fs::create_dir_all(&prefs).expect("prefs");
    let worker = WorkerEnv::hermetic(None)
        .with(&format!("CLAUDE_CONFIG_DIR={}", claude.display()))
        .with("USER=me");
    let pin = || login_pin_under(&worker, &proj, Some(&managed), &prefs, None);
    assert_eq!(pin(), LoginPin::Picker, "nothing written yet");
    let user = claude.join("settings.json");
    write(&user, r#"{"model":"opus","forceLoginMethod":""}"#);
    assert_eq!(pin(), LoginPin::Picker, "an empty method pins nothing");
    for (at, text, want) in [
        (
            &user,
            r#"{"forceLoginMethod":"claudeai"}"#,
            LoginPin::Method,
        ),
        (&user, r#"{"forceLoginMethod":"console"}"#, LoginPin::Method),
        (
            &user,
            r#"{"forceLoginMethod":"gateway"}"#,
            LoginPin::Gateway,
        ),
        (
            &user,
            r#"{"forceLoginGatewayUrl":"https://gw.example.test"}"#,
            LoginPin::Gateway,
        ),
        (
            &proj.join(".claude/settings.local.json"),
            r#"{"forceLoginMethod":"console"}"#,
            LoginPin::Method,
        ),
        (
            &managed.join("managed-settings.json"),
            r#"{"forceLoginMethod":"claudeai","forceLoginOrgUUID":"x"}"#,
            LoginPin::Method,
        ),
        (
            &claude.join("remote-settings.json"),
            r#"{"settings":{"forceLoginMethod":"claudeai"}}"#,
            LoginPin::Method,
        ),
        (&user, "{not json", LoginPin::Unread),
    ] {
        write(at, text);
        assert_eq!(pin(), want, "{} = {text}", at.display());
        std::fs::remove_file(at).expect("rm");
        assert_eq!(pin(), LoginPin::Picker, "{} gone", at.display());
    }
    write(&prefs.join("me").join(MANAGED_PREFERENCES_FILE), "plist");
    assert_eq!(
        pin(),
        LoginPin::Unread,
        "a managed preference domain is unread"
    );
    write(&user, r#"{"forceLoginMethod":"claudeai"}"#);
    assert_eq!(pin(), LoginPin::Method, "a method read beats one unread");
    assert_eq!(login_pin(&[]), LoginPin::Picker);
}

/// A PIN THE STATIC DOCUMENTS CANNOT SHOW IS READ ON THE SAFE SIDE (the post-landing review,
/// 2026-11-02): a managed document that names a policy helper (whose output, which may pin the
/// method, aterm never runs) reads unread unless a static document pins a method; a managed folder
/// or a server-managed copy the agent's environment names in place of Claude Code's own is read
/// too (2.1.316 compiles both overrides out; a build that honours them pins there); and an agent
/// started on a cloud session (`--cloud`, `--remote`, `--teleport`, `--attach-serve`), whose
/// `/login` is claude.ai's alone, is pinned whatever its settings say. CONTROL: none of them
/// draws the picker; a flag past `--` is no flag. Negative controls: [`login_pin`] without the
/// helper keys, [`login_pin_under`] without its environment's paths, or without the cloud flags,
/// reads [`LoginPin::Picker`] where Claude Code's `/login` opens the browser at once.
#[test]
fn a_pin_the_static_documents_cannot_show_is_read_on_the_safe_side() {
    use super::super::login::LoginPin;
    let s = ScratchDir::new("claude-settings", "login-pin-hidden");
    let claude = s.join("claude");
    let proj = s.join("proj");
    let managed = s.join("managed");
    let prefs = s.join("prefs");
    let other = s.join("other-managed");
    let copy = s.join("copy.json");
    std::fs::create_dir_all(&proj).expect("proj");
    std::fs::create_dir_all(&prefs).expect("prefs");
    let base = WorkerEnv::hermetic(None)
        .with(&format!("CLAUDE_CONFIG_DIR={}", claude.display()))
        .with("USER=me");
    let pin = |worker: &WorkerEnv| login_pin_under(worker, &proj, Some(&managed), &prefs, None);
    assert_eq!(pin(&base), LoginPin::Picker, "CONTROL: nothing written");
    let file = managed.join("managed-settings.json");
    for helper in [
        r#"{"policyHelper":{"path":"/usr/local/bin/corp-claude-policy"}}"#,
        r#"{"policyHelpers":{"macos":{"path":"/usr/local/bin/corp-claude-policy"}}}"#,
    ] {
        write(&file, helper);
        assert_eq!(pin(&base), LoginPin::Unread, "{helper}");
    }
    write(&file, r#"{"policyHelper":null}"#);
    assert_eq!(pin(&base), LoginPin::Picker, "CONTROL: a helper unset");
    write(
        &file,
        r#"{"policyHelper":{"path":"/x"},"forceLoginMethod":"console"}"#,
    );
    assert_eq!(
        pin(&base),
        LoginPin::Method,
        "a static method beside a helper"
    );
    std::fs::remove_file(&file).expect("rm");
    let named = base.clone().with(&format!(
        "CLAUDE_CODE_MANAGED_SETTINGS_PATH={}",
        other.display()
    ));
    assert_eq!(
        pin(&named),
        LoginPin::Picker,
        "CONTROL: the named folder empty"
    );
    write(
        &other.join("managed-settings.d").join("10-login.json"),
        r#"{"forceLoginMethod":"claudeai"}"#,
    );
    assert_eq!(
        pin(&named),
        LoginPin::Method,
        "a drop-in of the named folder"
    );
    assert_eq!(
        pin(&base),
        LoginPin::Picker,
        "CONTROL: a folder nothing names"
    );
    let remote = base.clone().with(&format!(
        "CLAUDE_CODE_REMOTE_SETTINGS_PATH={}",
        copy.display()
    ));
    assert_eq!(pin(&remote), LoginPin::Picker, "CONTROL: no copy there");
    write(&copy, r#"{"settings":{"forceLoginMethod":"claudeai"}}"#);
    assert_eq!(
        pin(&remote),
        LoginPin::Method,
        "the named server-managed copy"
    );
    let relative = base
        .clone()
        .with("CLAUDE_CODE_REMOTE_SETTINGS_PATH=copy.json");
    assert_eq!(pin(&relative), LoginPin::Unread, "a path it cannot place");
    for argv in [
        &["claude", "--cloud", "fix the build"][..],
        &["claude", "--remote=abc"],
        &["claude", "--teleport"],
        &["claude", "--attach-serve", "s1"],
    ] {
        assert_eq!(
            pin(&base.clone().launched_with(argv)),
            LoginPin::Method,
            "{argv:?}"
        );
    }
    for argv in [
        &["claude", "--", "--cloud"][..],
        &["claude", "--remote-control"],
        &["claude", "--resume"],
    ] {
        assert_eq!(
            pin(&base.clone().launched_with(argv)),
            LoginPin::Picker,
            "CONTROL: {argv:?}"
        );
    }
}

/// THE DEFAULT PERMISSION MODE IS THE HIGHEST DOCUMENT'S THAT NAMES ONE: with nothing written it
/// is unset; the user's settings name it; the project's outrank the user's, the project's local
/// ones the project's, the command line's `--settings` those, the managed file all of them, and
/// a managed drop-in the managed file. Each answer names the document that won. NEGATIVE
/// CONTROL: a document below the winner that cannot be parsed changes nothing, and the same
/// document above it makes the answer unread.
#[test]
fn the_default_permission_mode_is_read_in_claude_codes_precedence() {
    use crate::harness::lights::Mode;
    let s = ScratchDir::new("claude-settings", "default-mode");
    let claude = s.join("claude");
    let proj = s.join("proj");
    let managed = s.join("managed");
    let prefs = s.join("prefs");
    std::fs::create_dir_all(&proj).expect("proj");
    std::fs::create_dir_all(&prefs).expect("prefs");
    let line = s.join("line.json");
    let worker = WorkerEnv::hermetic(None)
        .with(&format!("CLAUDE_CONFIG_DIR={}", claude.display()))
        .with("USER=me")
        .launched_with(&["claude", "--settings", &line.display().to_string()]);
    let read = || permission_default_under(&worker, &proj, Some(&managed), &prefs, None);
    let named = |mode: Mode, at: &Path| Resolved::Mode {
        mode,
        source: at.display().to_string(),
    };
    let doc = |mode: &str| format!(r#"{{"permissions":{{"defaultMode":"{mode}"}}}}"#);
    assert_eq!(read().mode, Resolved::Unset, "nothing written");
    assert_eq!(read().bypass_disabled, None);
    let user = claude.join("settings.json");
    write(
        &user,
        r#"{"model":"opus","permissions":{"allow":["Read"]}}"#,
    );
    assert_eq!(read().mode, Resolved::Unset, "permissions with no default");
    let project = proj.join(".claude/settings.json");
    let local = proj.join(".claude/settings.local.json");
    let file = managed.join("managed-settings.json");
    let drop_in = managed.join("managed-settings.d").join("10-a.json");
    for (at, word, mode) in [
        (&user, "plan", Mode::Plan),
        (&project, "acceptEdits", Mode::AcceptEdits),
        (&local, "dontAsk", Mode::DontAsk),
        (&line, "auto", Mode::Auto),
        (&file, "default", Mode::Manual),
        (&drop_in, "bypassPermissions", Mode::Bypass),
    ] {
        write(at, &doc(word));
        assert_eq!(read().mode, named(mode, at), "{}", at.display());
    }
    std::fs::remove_file(&drop_in).expect("the drop-in");
    std::fs::remove_file(&file).expect("the managed file");
    assert_eq!(read().mode, named(Mode::Auto, &line), "the line's again");
    write(&user, "{ not json");
    assert_eq!(
        read().mode,
        named(Mode::Auto, &line),
        "control: an unparsed document below the winner says nothing"
    );
    write(&file, "{ not json");
    assert!(
        matches!(&read().mode, Resolved::Unread(why) if why.contains("managed-settings.json")),
        "an unparsed document above it is not read past: {:?}",
        read().mode
    );
}

/// WHAT THE RESOLVER WILL NOT GUESS, AND THE BYPASS SWITCH: a mode word this build does not
/// know, a `defaultMode` that is no string, one nested where no document of this shape keeps it,
/// a policy helper and a managed preference domain each read as unread, never as unset. `manual`
/// is `default`. `permissions.disableBypassPermissionsMode: "disable"` in any readable document
/// names that document, whatever names the mode; any other value of it does not.
#[test]
fn the_default_permission_mode_is_unread_wherever_it_cannot_be_told() {
    use crate::harness::lights::Mode;
    let source = |name: &str, text: &str| Source {
        name: name.to_string(),
        text: Ok(Some(text.to_string())),
    };
    let mode = |text: &str| permission_default(&[source("a.json", text)]).mode;
    assert_eq!(
        mode(r#"{"permissions":{"defaultMode":"manual"}}"#),
        Resolved::Mode {
            mode: Mode::Manual,
            source: "a.json".into()
        }
    );
    assert_eq!(
        mode(r#"{"permissions":{"defaultMode":null}}"#),
        Resolved::Unset
    );
    for text in [
        r#"{"permissions":{"defaultMode":"turbo"}}"#,
        r#"{"permissions":{"defaultMode":7}}"#,
        r#"{"policy":{"permissions":{"defaultMode":"plan"}}}"#,
        r#"{"policyHelper":"/usr/local/bin/policy"}"#,
    ] {
        assert!(
            matches!(mode(text), Resolved::Unread(why) if why.contains("a.json")),
            "{text}"
        );
    }
    let prefs = Source {
        name: "com.anthropic.claudecode.plist".into(),
        text: Err("are a managed preference domain this reader does not parse".into()),
    };
    let user = source("user.json", r#"{"permissions":{"defaultMode":"plan"}}"#);
    assert!(matches!(
        permission_default(&[prefs.clone(), user.clone()]).mode,
        Resolved::Unread(_)
    ));
    assert!(
        matches!(
            permission_default(&[user.clone(), prefs]).mode,
            Resolved::Mode {
                mode: Mode::Plan,
                ..
            }
        ),
        "control: unread below the winner"
    );
    let off = source(
        "managed.json",
        r#"{"permissions":{"disableBypassPermissionsMode":"disable"}}"#,
    );
    let got = permission_default(&[user.clone(), off]);
    assert_eq!(got.bypass_disabled.as_deref(), Some("managed.json"));
    assert!(matches!(got.mode, Resolved::Mode { .. }));
    let on = source(
        "managed.json",
        r#"{"permissions":{"disableBypassPermissionsMode":"enable"}}"#,
    );
    assert_eq!(permission_default(&[user, on]).bypass_disabled, None);
}
