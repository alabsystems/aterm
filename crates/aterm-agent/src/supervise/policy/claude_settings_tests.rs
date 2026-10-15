// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`bare_asks`] over settings chains written in scratch directories (never
//! the owner's files), and [`bare_tool`] over the rule spellings Claude Code
//! 2.1.295's parser takes.

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
fn found(got: Result<Vec<AskRule>, String>) -> Vec<(String, String)> {
    got.expect("the chain reads")
        .into_iter()
        .map(|r| (r.rule, r.source))
        .collect()
}

/// Claude Code 2.1.295 reads a rule as its bare tool with no parentheses, and
/// with `()` or `(*)` (its parser, and driven: `Write(*)` drew the box for a
/// Write into a subdirectory in accept-edits mode, where an unruled one draws
/// none). Anything else carries a specifier, or names no tool.
#[test]
fn a_bare_rule_is_its_tool_with_no_specifier() {
    for (rule, tool) in [
        ("Write", Some("Write")),
        ("Edit()", Some("Edit")),
        ("Read(*)", Some("Read")),
        ("Edit(src/**)", None),
        ("Read(~/x/**)", None),
        ("Bash(touch:*)", None),
        ("Write(**)", None),
        ("Write(x", None),
        ("Write)(", None),
        ("(*)", None),
        ("", None),
    ] {
        assert_eq!(bare_tool(rule), tool, "{rule:?}");
    }
}

/// The agent's OWN chain: the user's `settings.json` in its Claude Code
/// directory (`CLAUDE_CONFIG_DIR`, else `$HOME/.claude`), the project's two
/// files where it was launched, the local one in the repository's main
/// worktree too, and a `--settings` it was launched with. A rule with a
/// specifier is left out, and a file not there sets nothing. Negative
/// control: the same files under a directory the agent does not run under
/// hold nothing for it.
#[test]
fn the_agents_own_settings_chain_is_read_for_bare_ask_rules() {
    let s = ScratchDir::new("claude-settings", "chain");
    let claude = s.join("claude");
    let proj = s.join("proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let worker = WorkerEnv::hermetic(None).with(&format!("CLAUDE_CONFIG_DIR={}", claude.display()));
    assert!(
        found(bare_asks(&worker, &proj)).is_empty(),
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
        found(bare_asks(&worker, &proj)),
        [
            ("Edit()".to_string(), name(&local)),
            ("Read(*)".to_string(), name(&project)),
            ("Write".to_string(), name(&user)),
        ]
    );
    // The same Claude Code directory by `$HOME`, and a `--settings` given
    // inline: its rules join the chain.
    let by_home = WorkerEnv::from_env(vec![format!("HOME={}", s.display())]).launched_with(&[
        "claude",
        r#"--settings={"permissions":{"ask":["NotebookEdit"]}}"#,
    ]);
    std::fs::rename(&claude, s.join(".claude")).expect("move the Claude directory");
    let tools: Vec<String> = bare_asks(&by_home, &proj)
        .expect("reads")
        .into_iter()
        .map(|r| r.tool)
        .collect();
    assert_eq!(tools, ["NotebookEdit", "Edit", "Read", "Write"]);
    // Negative control: an agent under another Claude Code directory, launched
    // elsewhere, has none of these.
    let other = s.join("other");
    std::fs::create_dir_all(&other).expect("other");
    let elsewhere =
        WorkerEnv::hermetic(None).with(&format!("CLAUDE_CONFIG_DIR={}", s.join("none").display()));
    assert!(found(bare_asks(&elsewhere, &other)).is_empty());
}

/// Claude Code 2.1.295 keeps `.claude/settings.local.json` in the repository's
/// main worktree for every subdirectory and linked worktree of it, so an agent
/// launched in a linked worktree (git's layout: a `.git` file naming
/// `<main>/.git/worktrees/<name>`, whose `commondir` leads back to `<main>/.git`)
/// is read for the main worktree's local rules as well as its own.
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
    assert!(found(bare_asks(&worker, &sub)).is_empty());
    let local = main.join(".claude/settings.local.json");
    write(&local, &asks(&["Write"]));
    assert_eq!(
        found(bare_asks(&worker, &sub)),
        [("Write".to_string(), local.display().to_string())]
    );
}

/// A settings document that is there but cannot be read whole, does not
/// parse, or holds an ask list that is no list of strings: what it asks is
/// unknown, and the error names it (its reader fails closed). Negative
/// control: one with no `permissions` at all asks nothing.
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
        let why = bare_asks(&worker, &proj).expect_err(body);
        assert!(
            why.contains(says) && why.contains(&project.display().to_string()),
            "{why}"
        );
    }
    std::fs::remove_file(&project).expect("rm");
    std::fs::create_dir(&project).expect("a directory where the file goes");
    assert!(
        bare_asks(&worker, &proj)
            .expect_err("a directory")
            .contains("cannot be read as a regular file")
    );
    std::fs::remove_dir(&project).expect("rmdir");
    write(&project, r#"{"env":{"X":"1"}}"#);
    assert!(found(bare_asks(&worker, &proj)).is_empty());
}

/// Claude Code's settings loader (2.1.295 and 2.1.299) reads a file that is
/// empty or only white space as `{}`, and strips a leading byte-order mark
/// before it parses: such a file sets nothing, a marked one its rules — never
/// `do not parse`, which held every file box (the review of 2026-10-09: an
/// empty `settings.local.json` held a Write Claude Code ran unasked).
/// Negative control: text that is not JSON still does not parse, marked or not.
#[test]
fn an_empty_or_marked_settings_file_reads_as_claude_code_reads_it() {
    let s = ScratchDir::new("claude-settings", "empty");
    let proj = s.join("proj");
    let project = proj.join(".claude/settings.json");
    let worker = WorkerEnv::hermetic(None);
    for body in ["", " \n\t\r\n", "\u{feff}", "\u{feff}  \n"] {
        write(&project, body);
        assert!(found(bare_asks(&worker, &proj)).is_empty(), "{body:?}");
    }
    write(&project, &format!("\u{feff}{}", asks(&["Write"])));
    assert_eq!(
        found(bare_asks(&worker, &proj)),
        [("Write".to_string(), project.display().to_string())]
    );
    for body in ["{ not json", "\u{feff}{ not json"] {
        write(&project, body);
        let why = bare_asks(&worker, &proj).expect_err(body);
        assert!(why.contains("do not parse"), "{why}");
    }
}

/// The documents read where Claude Code 2.1.299 reads them (its settings-path
/// switch): a RELATIVE `CLAUDE_CONFIG_DIR` against the agent's directory
/// (`path.resolve`), not skipped; `cowork_settings.json` in place of
/// `settings.json` under a true `CLAUDE_CODE_USE_COWORK_PLUGINS`; and the
/// project's two files under the root a `--project-config-root` names, in
/// place of the launch directory's. Negative controls: a false
/// `CLAUDE_CODE_USE_COWORK_PLUGINS` keeps `settings.json`; without the flag
/// the launch directory's files are the project's.
#[test]
fn the_chain_is_read_where_claude_code_reads_it() {
    let s = ScratchDir::new("claude-settings", "where");
    let proj = s.join("proj");
    std::fs::create_dir_all(&proj).expect("proj");
    let relative = WorkerEnv::hermetic(None).with("CLAUDE_CONFIG_DIR=cc");
    let user = proj.join("cc/settings.json");
    write(&user, &asks(&["Write"]));
    assert_eq!(
        found(bare_asks(&relative, &proj)),
        [("Write".to_string(), user.display().to_string())]
    );

    let claude = s.join("claude");
    let dir = format!("CLAUDE_CONFIG_DIR={}", claude.display());
    let cowork = claude.join("cowork_settings.json");
    write(&claude.join("settings.json"), &asks(&["Read"]));
    write(&cowork, &asks(&["Edit"]));
    let tools = |w: &WorkerEnv, launch: &Path| -> Vec<String> {
        bare_asks(w, launch)
            .expect("reads")
            .into_iter()
            .map(|r| r.tool)
            .collect()
    };
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

/// The managed documents open the chain: `managed-settings.json` and the
/// `*.json` drop-ins of `managed-settings.d` beside it, sorted (a file of
/// another kind is none). Claude Code's own directory is [`MANAGED_DIR`]; a
/// scratch tree stands in for it here. Negative control: with no managed
/// directory there are none.
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

/// Claude Code's managed preference domains (2.1.300: the account's, then
/// the device's, under `/Library/Managed Preferences`): each that is there
/// is reported as a document nobody here can read, and none is opened; the
/// account is the worker's `USER`, else its `LOGNAME`. Negative controls:
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

/// THE ROUTE'S FENCE ([`sources_under`] given a home): a settings document
/// that is there inside a file-provider domain of the home (iCloud Drive, a
/// sync provider's folder) is reported WITHOUT being opened, though its text
/// is perfectly good — reached by a symlink from an ordinary folder too —
/// while the same file in an ordinary folder is read. NEGATIVE CONTROL: with
/// no fence (the approval lane's [`sources`], or no home known) it is read.
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

/// THE ROUTE'S FENCE COVERS THE GIT FILES THE CHAIN OPENS to find a linked
/// worktree's main worktree (the `.git` file, its git directory's
/// `commondir`), as footer's fenced git read does: a git directory can lie in
/// a file-provider domain from outside one, and a linked worktree inside one
/// has its `.git` file there. Fenced, neither is opened and the main
/// worktree's local settings are reported, never looked for. NEGATIVE
/// CONTROL: unfenced (the approval lane's [`sources`]), both are read and the
/// main worktree's `settings.local.json` is found. The review of 2026-10-14
/// found f71bc6de9 opening both under the fence.
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

/// A settings document that is NOT THERE inside a file-provider domain sets
/// nothing, as anywhere else: whether it is there is a `stat`, which no
/// consent gates (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §1.5), so the
/// fence has nothing to keep unopened. The review of 2026-10-14 found it
/// reported (`lie in a file-provider domain`), so a project in iCloud Drive or
/// a sync folder with no `.claude` at all had every chain read fail.
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

/// Set in the environment of [`provider_home_stage`]'s run: this test binary
/// is the child of
/// [`the_approval_lane_reads_its_settings_in_a_file_provider_domain_of_this_home`],
/// its `HOME` a scratch home, and this names a project inside a
/// file-provider domain of it.
const PROVIDER_STAGE: &str = "ATERM_CLAUDE_SETTINGS_PROVIDER_PROJ";

/// Where [`provider_home_stage`] says it ran every check (a child that a
/// filter ran nothing in exits 0 too).
const PROVIDER_STAGE_DONE: &str = "ATERM_CLAUDE_SETTINGS_PROVIDER_DONE";

/// THE APPROVAL LANE READS AN AGENT'S SETTINGS IN A FILE-PROVIDER DOMAIN OF
/// THIS PROCESS'S HOME (iCloud Drive, a sync provider's folder): none there
/// sets nothing, and the project's `settings.local.json` is read for its ask
/// rules and its `env`. The
/// review of 2026-10-14 found every chain read there `Err` on f71bc6de9 (the
/// route's fence, moved into the shared reader), so every read-only Bash box
/// and every git read of such a project escalated (module header, "The
/// file-provider fence is the route's"). Run in a child of this test binary
/// whose `HOME` is a scratch home ([`provider_home_stage`]): the project is
/// then in a file-provider domain of THIS process's home, the one the
/// route's fence is against.
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

/// THE STAGE of
/// [`the_approval_lane_reads_its_settings_in_a_file_provider_domain_of_this_home`],
/// when [`PROVIDER_STAGE`] is set (and nothing at all otherwise).
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
    assert!(found(bare_asks(&worker, &proj)).is_empty(), "nothing there");
    let local = proj.join(".claude/settings.local.json");
    write(
        &local,
        r#"{"permissions":{"allow":["Bash(ls:*)"],"ask":["Write"]}}"#,
    );
    assert_eq!(worker.settings_env(&proj), Ok(()), "a quiet local file");
    assert_eq!(
        found(bare_asks(&worker, &proj)),
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

/// An agent whose directory is not known (a relative `launch`) has every
/// document named from it reported, never read against this process's own
/// directory: the project's, a relative `--settings` and a relative Claude
/// Code directory. Negative control: what it names absolutely is read.
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
