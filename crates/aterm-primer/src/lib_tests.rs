// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// THE LAND END'S WORDS ARE ONE SOURCE ([`LAND_CHECK_ANSWER`]): the primer composes them, and
/// the supervise-agent skill, a file no constant reaches, carries them as written.
/// Negative control: the skill's sentence reworded.
#[test]
fn the_land_ends_words_are_the_primers_and_the_skills() {
    assert!(PRIMER_BODY.contains(LAND_CHECK_ANSWER));
    assert!(
        SUPERVISE_SKILL_BODY.contains(LAND_CHECK_ANSWER),
        "the supervise-agent skill says how a Land worker's session ends"
    );
}

/// WHAT A MISSING `checked=` MEANS IS ONE SOURCE ([`aterm_types::control_verbs::NO_CHECKED_RECORD`], the
/// post-landing review, 2026-11-01): the help and the manual compose it, and the
/// supervise-agent skill, a file no constant reaches, carries it as written, its lines
/// wrapped where the file wraps them. Negative control: the skill's clause reworded.
#[test]
fn the_missing_checked_words_are_the_helps_and_the_skills() {
    let words = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words(SUPERVISE_SKILL_BODY).contains(&words(aterm_types::control_verbs::NO_CHECKED_RECORD)),
        "the supervise-agent skill says what a missing checked= means"
    );
}

fn auto_prime(home: &Path) -> AutoPrime {
    auto_prime_with(home, &Roots::default())
}

fn agents_report(home: &Path, args: &[String]) -> (String, i32) {
    agents_report_with(home, args, &Roots::default())
}

const V1_BLOCK: &str = "<!-- aterm primer v1 — managed by `aterm agents`; `aterm agents remove` uninstalls -->\n\
    ## aterm\n\
    If the environment has `TERM_PROGRAM=aterm` or `ATERM_CHILD=1`, this terminal is aterm.\n\
    <!-- /aterm primer -->\n";

const V2_BLOCK: &str = r"<!-- aterm primer v2 — managed by `aterm agents`; `aterm agents remove` uninstalls -->
## aterm
If the environment has `TERM_PROGRAM=aterm` or `ATERM_CHILD=1`, this terminal is aterm — an
AI-native terminal whose sessions are introspectable and drivable: agents and humans can read
the live screen, send input, and await real transitions, concurrently. Run `aterm help` for the
agent operating brief and `aterm help introspection` for the `aterm ctl` control verbs (`aterm`
is already on PATH inside aterm sessions). aterm deliberately STRIPS `CLAUDE*`, `ANTHROPIC_*`,
`COPILOT_*`, `CODEX_*`, `CURSOR_*`, and `AI_*` env vars from the shells it spawns — `aterm help`
explains why. If neither variable is set, you are not inside aterm; ignore this section.
<!-- /aterm primer -->
";

#[test]
fn a_newer_block_is_left_as_is() {
    let block = generic();
    let ours = marker_version(&block).expect("our marker names its version");
    assert_eq!(marker_version(MARK_BEGIN), Some(ours));
    let newer_marker = MARK_BEGIN.replacen(&format!("v{ours} "), &format!("v{} ", ours + 1), 1);
    let newer = format!("mine\n\n{newer_marker}\nA later body.\n{MARK_END}\n\nafter\n");
    assert_eq!(block_state(&newer, &block).unwrap(), BlockState::Newer);
    assert_eq!(
        upsert_block(&newer, &block).unwrap(),
        None,
        "nothing written"
    );

    let older_marker = MARK_BEGIN.replacen(&format!("v{ours} "), &format!("v{} ", ours - 1), 1);
    let older = format!("{older_marker}\nAn earlier body.\n{MARK_END}\n");
    assert_eq!(block_state(&older, &block).unwrap(), BlockState::Stale);
    assert!(upsert_block(&older, &block).unwrap().is_some());
    let edited = block.replacen("## aterm", "## aterm (edited)", 1);
    assert_eq!(block_state(&edited, &block).unwrap(), BlockState::Stale);
    assert!(upsert_block(&edited, &block).unwrap().is_some());
    assert_eq!(marker_version("<!-- aterm primer vX —"), None);
    assert_eq!(marker_version("no marker"), None);
}
const V3_BLOCK: &str = r"<!-- aterm primer v3 — managed by `aterm agents`; `aterm agents remove` uninstalls -->
## aterm
If the environment has `TERM_PROGRAM=aterm` or `ATERM_CHILD=1`, this terminal is aterm — an
AI-native terminal whose sessions are introspectable and drivable: agents and humans can read
the live screen, send input, and await real transitions, concurrently. Run `aterm help` for the
agent operating brief and `aterm help introspection` for the `aterm ctl` control verbs (`aterm`
is already on PATH inside aterm sessions). First moves: `aterm ctl windows` and `aterm ctl ls`
list every window and session; read a peer's `status` (detail= names the program it is running)
before typing into it. aterm deliberately STRIPS `CLAUDE*`, `ANTHROPIC_*`, `COPILOT_*`,
`CODEX_*`, `CURSOR_*`, and `AI_*` env vars from the shells it spawns — `aterm help` explains
why. If neither variable is set, you are not inside aterm; ignore this section.
<!-- /aterm primer -->
";

fn generic() -> String {
    primer_block(None)
}

/// The block a file `readers` share carries ([`Readers::block`]): each one's own part, in
/// registry order.
fn shared_by(readers: &[&str]) -> String {
    let parts: Vec<&str> = AGENT_FILES
        .iter()
        .filter(|a| readers.contains(&a.name))
        .filter_map(|a| a.addendum)
        .collect();
    block_with(&parts)
}

fn temp_litter(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("aterm-tmp"))
        .collect()
}

fn backups(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("settings.json.bak-"))
        .collect();
    names.sort();
    names
}

#[test]
fn upsert_is_idempotent_and_remove_round_trips_to_the_original() {
    let block = generic();
    let original = "# My rules\n\nBe nice.\n";
    let installed = upsert_block(original, &block).unwrap().unwrap();
    assert_eq!(installed, format!("{original}\n{block}"));
    assert_eq!(
        upsert_block(&installed, &block).unwrap(),
        None,
        "second install is a no-op"
    );
    assert_eq!(remove_block(&installed).unwrap().unwrap(), original);
    assert_eq!(remove_block(&block).unwrap().unwrap(), "");
    assert_eq!(remove_block(original).unwrap(), None);
}

#[test]
fn unterminated_block_fails_closed() {
    let block = generic();
    let corrupt = "x\n<!-- aterm primer v1 -->\nno end marker\n";
    assert!(block_state(corrupt, &block).is_err());
    assert!(upsert_block(corrupt, &block).is_err());
    assert!(remove_block(corrupt).is_err());
    let stray = format!("x\n{MARK_END}\ny\n");
    assert_eq!(block_state(&stray, &block).unwrap(), BlockState::Absent);
}

#[test]
fn a_failed_atomic_write_leaves_the_original_intact() {
    let home = aterm_tempfile::tempdir().unwrap();
    let target = home.path().join("CLAUDE.md");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "keep").unwrap();
    write_atomically(&target, b"new").expect_err("a directory in the way fails");
    assert_eq!(
        std::fs::read_to_string(target.join("keep.txt")).unwrap(),
        "keep"
    );
    assert_eq!(temp_litter(home.path()), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_symlinked_context_file_is_written_through_to_its_target() {
    let home = aterm_tempfile::tempdir().unwrap();
    let store = home.path().join("dotfiles");
    let cfg = home.path().join(".claude");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    let real = store.join("claude-context.md");
    std::fs::write(&real, "# mine\n").unwrap();
    let link = cfg.join("CLAUDE.md");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    upsert_primer_file(&link, &generic(), Room::Anywhere).unwrap();
    assert!(
        std::fs::symlink_metadata(&link).unwrap().is_symlink(),
        "the link is still a link, not a regular file that shadows it"
    );
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        format!("# mine\n\n{}", generic())
    );
    for d in [&store, &cfg] {
        assert_eq!(temp_litter(d), Vec::<String>::new(), "{d:?}");
    }

    std::fs::remove_file(&real).unwrap();
    let skill = cfg.join("skills/drive-aterm/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    let skill_target = store.join("drive-aterm.md");
    std::os::unix::fs::symlink(&skill_target, &skill).unwrap();
    let refused = upsert_primer_file(&link, &generic(), Room::Anywhere).unwrap_err();
    assert_eq!(
        install_skill_file(&skill, DRIVE_SKILL_BODY, Room::Anywhere).unwrap_err(),
        refused
    );
    for (l, target) in [(&link, &real), (&skill, &skill_target)] {
        assert!(std::fs::symlink_metadata(l).unwrap().is_symlink(), "{l:?}");
        assert!(!target.exists(), "{target:?}");
    }
    let status_row_ends = |path: &str, refused: &str| {
        let (status, _) = agents_report(home.path(), &["status".to_string()]);
        let row = status.lines().find(|l| l.contains(path)).unwrap();
        assert!(row.ends_with(&format!("ERROR: {refused}")), "{status}");
    };
    status_row_ends("~/.claude/CLAUDE.md", &refused);
    status_row_ends("~/.claude/skills/drive-aterm/SKILL.md", &refused);

    std::fs::remove_dir_all(cfg.join("skills")).unwrap();
    let skills_target = store.join("claude-skills");
    std::os::unix::fs::symlink(&skills_target, cfg.join("skills")).unwrap();
    let refused = install_skill_file(&skill, DRIVE_SKILL_BODY, Room::Anywhere).unwrap_err();
    status_row_ends("~/.claude/skills/drive-aterm/SKILL.md", &refused);
    assert!(
        std::fs::symlink_metadata(cfg.join("skills"))
            .unwrap()
            .is_symlink()
    );
    assert!(
        !skills_target.exists(),
        "nothing made where the link points"
    );
}

#[cfg(unix)]
#[test]
fn the_temp_file_is_never_wider_than_the_context_file_it_becomes() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = aterm_tempfile::tempdir().unwrap();
    let dir = home.path();
    let mode_of = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

    let tmp = dir.join(".CLAUDE.md.aterm-tmp-probe");
    drop(create_private_temp(&tmp).unwrap());
    assert_eq!(mode_of(&tmp) & 0o077, 0, "the temp file is born private");
    assert_eq!(
        create_private_temp(&tmp).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );

    let fresh = dir.join("CLAUDE.md");
    write_atomically(&fresh, b"primed\n").unwrap();
    assert_eq!(mode_of(&fresh) & 0o077, 0, "a new context file is private");
    let existing = dir.join("AGENTS.md");
    std::fs::write(&existing, "old\n").unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
    let recording = aterm_durable::seam::Recording::start();
    write_atomically(&existing, b"primed\n").unwrap();
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "an atomic write is only ordered ahead of its rename"
    );
    assert_eq!(mode_of(&existing), 0o644);
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "primed\n");
}

#[test]
fn install_touches_only_detected_agents_unless_forced() {
    let home = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let (out, code) = agents_report(home.path(), &["install".to_string()]);
    assert_eq!(code, 0, "skips are not failures:\n{out}");
    assert_eq!(
        std::fs::read_to_string(home.path().join(".claude/CLAUDE.md")).unwrap(),
        primer_block(Some("claude"))
    );
    assert!(
        !home.path().join(".codex").exists(),
        "a bare install must not create an undetected agent's dir"
    );

    let (out, code) = agents_report(home.path(), &["install".to_string(), "codex".to_string()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        std::fs::read_to_string(home.path().join(".codex/AGENTS.md")).unwrap(),
        primer_block(Some("codex"))
    );
}

/// THE RETIRED GEMINI CLI IS REFUSED BY NAME, agy named: its context file is agy's row,
/// which agy loads, and its custom-command file is no longer written.
#[test]
fn the_retired_gemini_cli_is_refused_by_name_and_its_file_is_agys() {
    let home = aterm_tempfile::tempdir().unwrap();
    for retired in ["gemini", "Gemini", "gemini-cli"] {
        for verb in ["install", "remove", "primer"] {
            let (out, code) = agents_report(home.path(), &[verb.to_string(), retired.to_string()]);
            assert_ne!(code, 0, "{verb} {retired}: {out}");
            assert!(out.contains("agy replaces it"), "{verb} {retired}: {out}");
        }
    }
    assert!(!home.path().join(".gemini").exists(), "nothing written");
    let (out, code) = agents_report(home.path(), &["install".to_string(), "agy".to_string()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        std::fs::read_to_string(home.path().join(".gemini/GEMINI.md")).unwrap(),
        generic(),
        "agy's row primes the file agy loads"
    );
    assert!(
        !home.path().join(".gemini/commands").exists(),
        "no Gemini CLI command file"
    );
    assert!(skills_for("agy").is_empty() && skills_for("gemini").is_empty());
}

/// THE GEMINI CLI COMMAND AN EARLIER ATERM WROTE IS DELETED by the automatic pass, `install`
/// and `remove`, each naming it once and then quiet; a file the user wrote there (no marker
/// line) is never touched, and one that cannot be read is an error, not a silent skip.
#[test]
fn the_gemini_cli_command_an_earlier_aterm_wrote_is_deleted_and_the_users_is_kept() {
    // What every earlier version wrote there (088c0cb4d1^ `GEMINI_FABRIC_BODY`).
    let ours = concat!(
        "# aterm-fabric — MANAGED FILE, rewritten by `aterm agents` on every install/update.\n",
        "description = \"The aterm fabric: read this session's inbox, post to a peer or a human, and understand hold=/trust=/fabric=.\"\n",
        "prompt = '''\n",
        include_str!("../assets/aterm-fabric-body.md"),
        "'''\n",
    );
    let theirs = "description = \"my fabric notes\"\nprompt = '''\nmine\n'''\n";
    let rel = ".gemini/commands/aterm-fabric.toml";
    assert_eq!(
        retired_skills_for("agy")
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        [rel]
    );
    assert!(
        AGENT_FILES
            .iter()
            .filter(|a| a.name != "agy")
            .all(|a| retired_skills_for(a.name).all(|r| r.path != rel)),
        "no other row sweeps Gemini CLI's command"
    );
    let primed = |home: &Path| {
        std::fs::create_dir_all(home.join(".gemini/commands")).unwrap();
        std::fs::write(home.join(".gemini/GEMINI.md"), generic()).unwrap();
        home.join(rel)
    };
    let status = |home: &Path| agents_report(home, &["status".to_string()]).0;
    let retired_row = |out: &str| {
        out.lines()
            .filter(|l| l.starts_with("  retired") && l.contains("aterm-fabric.toml"))
            .count()
    };

    // The automatic pass: deleted, listed once, then idempotent.
    let home = aterm_tempfile::tempdir().unwrap();
    let file = primed(home.path());
    std::fs::write(&file, ours).unwrap();
    assert_eq!(retired_row(&status(home.path())), 1, "status names it");
    let first = auto_prime(home.path());
    let agy = first.agents.iter().find(|a| a.agent == "agy").unwrap();
    assert_eq!(agy.outcome, Outcome::Updated, "{agy:?}");
    assert_eq!(
        agy.wrote,
        ["~/.gemini/commands/aterm-fabric.toml (Gemini CLI's command, removed)"]
    );
    assert!(!file.exists());
    assert_eq!(
        retired_row(&status(home.path())),
        0,
        "{}",
        status(home.path())
    );
    let second = auto_prime(home.path());
    assert!(!second.changed(), "{second:?}");
    assert_eq!(
        second
            .agents
            .iter()
            .find(|a| a.agent == "agy")
            .unwrap()
            .outcome,
        Outcome::Unchanged
    );

    // install and remove: one row each, the file gone, exit 0; a second run prints none.
    for verb in ["install", "remove"] {
        let home = aterm_tempfile::tempdir().unwrap();
        let file = primed(home.path());
        std::fs::write(&file, ours).unwrap();
        let (out, code) = agents_report(home.path(), &[verb.to_string()]);
        assert_eq!(code, 0, "{verb}: {out}");
        assert_eq!(retired_row(&out), 1, "{verb}: {out}");
        assert!(
            out.contains("removed (Gemini CLI's command"),
            "{verb}: {out}"
        );
        assert!(!file.exists(), "{verb}");
        let (again, code) = agents_report(home.path(), &[verb.to_string()]);
        assert_eq!((code, retired_row(&again)), (0, 0), "{verb}: {again}");
        let (named, code) = agents_report(home.path(), &[verb.to_string(), "agy".to_string()]);
        assert_eq!((code, retired_row(&named)), (0, 0), "{verb} agy: {named}");
    }

    // The user's own file there is never touched and never named.
    let home = aterm_tempfile::tempdir().unwrap();
    let file = primed(home.path());
    std::fs::write(&file, theirs).unwrap();
    assert_eq!(retired_row(&status(home.path())), 0);
    let pass = auto_prime(home.path());
    assert!(!pass.changed(), "{pass:?}");
    for verb in ["install", "remove"] {
        let (out, code) = agents_report(home.path(), &[verb.to_string()]);
        assert_eq!((code, retired_row(&out)), (0, 0), "{verb}: {out}");
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), theirs);

    // A path that cannot be read is an error row.
    let home = aterm_tempfile::tempdir().unwrap();
    let file = primed(home.path());
    std::fs::create_dir_all(&file).unwrap();
    let pass = auto_prime(home.path());
    let agy = pass.agents.iter().find(|a| a.agent == "agy").unwrap();
    assert!(
        matches!(&agy.outcome, Outcome::Error(e) if e.starts_with("retired skill: ")),
        "{agy:?}"
    );
    let (out, code) = agents_report(home.path(), &["install".to_string()]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("  retired") && l.contains("ERROR: ")),
        "{out}"
    );
    assert!(file.is_dir(), "nothing deleted");
}

/// The `  skill` rows of an `aterm agents` report, as (path shown, verdict).
fn skill_rows(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| l.strip_prefix("  skill"))
        .filter_map(|rest| {
            let (path, verdict) = rest.trim_start().split_once(' ')?;
            Some((path.to_string(), verdict.trim_start().to_string()))
        })
        .collect()
}

/// CODEX GETS EVERY ATERM SKILL CLAUDE CODE GETS, WHERE CODEX READS ONE: `install codex`
/// writes each, Claude Code's body, as `<codex home>/skills/<name>/SKILL.md`, the codex home
/// `~/.codex` or the `$CODEX_HOME` that moves it; a second install reports every one current;
/// `remove` deletes the marked ones and keeps the copy whose marker line the user deleted.
/// The window's pass writes them in `~/.codex` whatever `$CODEX_HOME` says, and `remove`
/// under `$CODEX_HOME` deletes aterm's in both homes. NEGATIVE CONTROL: with `$CODEX_HOME`
/// set, `install` puts nothing in `~/.codex`, and with it unset, nothing in the folder it
/// would name; a pass that follows `$CODEX_HOME` writes there and not in `~/.codex`.
#[test]
fn codex_gets_every_aterm_skill_where_codex_reads_skills() {
    let names = |agent: &str| -> Vec<&str> {
        skills_for(agent)
            .iter()
            .map(|s| doc_label(s.path))
            .collect()
    };
    assert_eq!(names("codex"), names("claude"));
    assert_eq!(
        names("claude"),
        [
            "drive-aterm",
            "supervise-agent",
            "rust-in-aterm",
            "aterm-themes",
            "aterm-fabric",
            "system-state",
            "open-in-amacs",
        ]
    );
    let n = skills_for("codex").len();
    for (s, c) in skills_for("codex").iter().zip(skills_for("claude")) {
        assert_eq!(
            s.path,
            format!(".codex/skills/{}/SKILL.md", doc_label(s.path))
        );
        assert_eq!(s.body, c.body, "{}", s.path);
    }
    let elsewhere = aterm_tempfile::tempdir().unwrap();
    let codex_home = elsewhere.path().join("codex-home");
    let installed = |home: &Path, roots: &Roots| {
        skills_for("codex")
            .iter()
            .filter(|s| {
                std::fs::read_to_string(home_join(home, roots, s.path)).ok()
                    == Some(s.body.to_string())
            })
            .count()
    };
    for moved in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let roots = Roots {
            codex: moved.then(|| codex_home.clone()),
            ..Roots::default()
        };
        let run = |verb: &str| {
            agents_report_with(
                home.path(),
                &[verb.to_string(), "codex".to_string()],
                &roots,
            )
        };
        let (out, code) = run("install");
        assert_eq!(code, 0, "{out}");
        assert_eq!(installed(home.path(), &roots), n, "{out}");
        let rows = skill_rows(&out);
        let shown: Vec<String> = skills_for("codex")
            .iter()
            .map(|s| display_path(home.path(), &roots, s.path))
            .collect();
        assert_eq!(
            rows.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            shown
        );
        assert!(rows.iter().all(|(_, v)| v == "installed"), "{out}");
        if moved {
            assert!(
                shown
                    .iter()
                    .all(|p| p.starts_with(&*codex_home.to_string_lossy()))
            );
            assert!(!home.path().join(".codex").exists(), "nothing in ~/.codex");
        } else {
            assert!(shown.iter().all(|p| p.starts_with("~/.codex/skills/")));
            assert!(
                !codex_home.exists(),
                "nothing where $CODEX_HOME would point"
            );
        }

        let (again, code) = run("install");
        assert_eq!(code, 0, "{again}");
        let rows = skill_rows(&again);
        assert_eq!(rows.len(), n, "{again}");
        assert!(
            rows.iter().all(|(_, v)| v == "already installed"),
            "{again}"
        );

        let themes = home_join(home.path(), &roots, ".codex/skills/aterm-themes/SKILL.md");
        let forked: String = THEMES_SKILL_BODY
            .lines()
            .filter(|l| !l.starts_with(SKILL_MARK_PREFIX))
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(&themes, &forked).unwrap();
        let (out, code) = run("remove");
        assert_eq!(code, 0, "{out}");
        assert_eq!(installed(home.path(), &roots), 0, "{out}");
        assert_eq!(std::fs::read_to_string(&themes).unwrap(), forked);
        let kept: Vec<String> = skill_rows(&out)
            .into_iter()
            .filter(|(_, v)| v.starts_with("kept"))
            .map(|(p, _)| doc_label(&p.replace(std::path::MAIN_SEPARATOR, "/")).to_string())
            .collect();
        assert_eq!(kept, ["aterm-themes"], "{out}");
        std::fs::remove_file(&themes).unwrap();

        if moved {
            std::fs::create_dir_all(&codex_home).unwrap();
            let window = roots.window();
            assert_eq!((window.xdg, window.codex), (None, None));
            let wrote_by = |roots: &Roots| -> Vec<String> {
                let pass = auto_prime_with(home.path(), roots);
                assert!(
                    pass.agents.iter().all(|a| a.agent == "codex"),
                    "{}",
                    pass.summary
                );
                pass.agents.into_iter().flat_map(|a| a.wrote).collect()
            };
            assert!(
                wrote_by(&roots.window()).is_empty(),
                "no ~/.codex, no codex"
            );
            std::fs::create_dir_all(home.path().join(".codex")).unwrap();
            let wrote = wrote_by(&roots.window());
            assert_eq!(wrote.len(), n + 1, "{wrote:?}");
            assert!(
                wrote.iter().all(|p| p.starts_with("~/.codex/")),
                "{wrote:?}"
            );
            assert_eq!(installed(home.path(), &roots.window()), n);
            assert_eq!(installed(home.path(), &roots), 0, "nothing in $CODEX_HOME");

            let wrote = wrote_by(&roots);
            assert_eq!(wrote.len(), n + 1, "{wrote:?}");
            assert!(wrote.iter().all(|p| !p.starts_with("~/")), "{wrote:?}");
            assert_eq!(installed(home.path(), &roots), n);

            let (out, code) = run("remove");
            assert_eq!(code, 0, "{out}");
            assert_eq!(installed(home.path(), &roots), 0, "{out}");
            assert_eq!(installed(home.path(), &roots.window()), 0, "{out}");
            for agents_md in [
                home.path().join(".codex/AGENTS.md"),
                codex_home.join("AGENTS.md"),
            ] {
                let left = std::fs::read_to_string(&agents_md).unwrap_or_default();
                assert!(
                    !left.contains(MARK_PREFIX),
                    "{}: {out}",
                    agents_md.display()
                );
            }
            let removed: Vec<&str> = skill_rows(&out)
                .into_iter()
                .filter(|(_, v)| v == "removed")
                .map(|(p, _)| {
                    if p.starts_with("~/") {
                        "~"
                    } else {
                        "$CODEX_HOME"
                    }
                })
                .collect();
            assert_eq!(
                removed,
                [vec!["$CODEX_HOME"; n], vec!["~"; n]].concat(),
                "{out}"
            );
        }
    }
}

/// THE CODEX CUSTOM PROMPT AN EARLIER ATERM WROTE IS DELETED by the automatic pass,
/// `install` and `remove`: Codex reads no custom prompt now, and every row naming it says it
/// was Codex's, not Gemini CLI's. While `$CODEX_HOME` moves `.codex`, the copy in `~/.codex`
/// (where every earlier aterm wrote it) goes, and so does one in the folder it names.
/// NEGATIVE CONTROL: a file at either place without the marker line is the user's, kept and
/// never named.
#[test]
fn the_codex_prompt_an_earlier_aterm_wrote_is_deleted_and_the_users_is_kept() {
    let rel = ".codex/prompts/aterm-fabric.md";
    assert_eq!(
        retired_skills_for("codex")
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        [rel]
    );
    // What every earlier version wrote there began with the fabric body's marker line.
    let ours = include_str!("../assets/aterm-fabric-body.md");
    let theirs = "# my own fabric notes\n";
    let primed = |home: &Path, text: &str| {
        std::fs::create_dir_all(home.join(".codex/prompts")).unwrap();
        std::fs::write(home.join(rel), text).unwrap();
        home.join(rel)
    };
    // A row names a place under the home as `~/…`, and one a variable moved as this host
    // joins its segments.
    let joined = Path::new("prompts").join("aterm-fabric.md");
    let joined = joined.display().to_string();
    let retired_rows = |out: &str| -> Vec<String> {
        out.lines()
            .filter(|l| {
                l.starts_with("  retired")
                    && (l.contains("prompts/aterm-fabric.md") || l.contains(&joined))
            })
            .map(str::to_string)
            .collect()
    };
    let named_as_codex =
        |row: &str| row.contains("Codex's custom prompt") && !row.contains("Gemini");

    let home = aterm_tempfile::tempdir().unwrap();
    let file = primed(home.path(), ours);
    let status = agents_report(home.path(), &["status".to_string()]).0;
    let rows = retired_rows(&status);
    assert!(rows.len() == 1 && named_as_codex(&rows[0]), "{status}");
    let pass = auto_prime(home.path());
    let codex = pass.agents.iter().find(|a| a.agent == "codex").unwrap();
    assert!(
        codex.wrote.contains(&format!(
            "~/{rel} (Codex's custom prompt, which Codex no longer reads, removed)"
        )),
        "{codex:?}"
    );
    assert!(!file.exists());
    assert!(!auto_prime(home.path()).changed());

    for verb in ["install", "remove"] {
        let home = aterm_tempfile::tempdir().unwrap();
        let file = primed(home.path(), ours);
        let (out, code) = agents_report(home.path(), &[verb.to_string()]);
        assert_eq!(code, 0, "{verb}: {out}");
        let rows = retired_rows(&out);
        assert!(rows.len() == 1 && named_as_codex(&rows[0]), "{verb}: {out}");
        assert!(!file.exists(), "{verb}");

        let home = aterm_tempfile::tempdir().unwrap();
        let file = primed(home.path(), theirs);
        let (out, code) = agents_report(home.path(), &[verb.to_string()]);
        assert_eq!((code, retired_rows(&out).len()), (0, 0), "{verb}: {out}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), theirs, "{verb}");
    }
    let home = aterm_tempfile::tempdir().unwrap();
    let file = primed(home.path(), theirs);
    assert!(retired_rows(&agents_report(home.path(), &["status".to_string()]).0).is_empty());
    let pass = auto_prime(home.path());
    assert!(
        pass.agents
            .iter()
            .all(|a| a.wrote.iter().all(|w| !w.contains(rel))),
        "{pass:?}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), theirs);

    // No earlier aterm followed `$CODEX_HOME`: while it moves `.codex`, the prompt is
    // looked for in `~/.codex` too, and in the folder it names (a home copied there).
    let elsewhere = aterm_tempfile::tempdir().unwrap();
    let roots = Roots {
        codex: Some(elsewhere.path().to_path_buf()),
        ..Roots::default()
    };
    let moved = |home: &Path, text: &str| {
        let file = home_join(home, &roots, rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        file
    };
    for verb in ["install", "remove"] {
        let home = aterm_tempfile::tempdir().unwrap();
        let files = [primed(home.path(), ours), moved(home.path(), ours)];
        let report =
            |verb: &str| agents_report_with(home.path(), &[verb.into(), "codex".into()], &roots).0;
        let rows = retired_rows(&report("status"));
        assert_eq!(rows.len(), 2, "{rows:?}");
        let out = report(verb);
        let rows = retired_rows(&out);
        assert!(
            rows.len() == 2 && rows.iter().all(|r| named_as_codex(r)),
            "{out}"
        );
        assert!(rows[1].contains(&format!("~/{rel}")), "{out}");
        assert!(files.iter().all(|f| !f.exists()), "{verb}: {out}");

        let home = aterm_tempfile::tempdir().unwrap();
        let files = [primed(home.path(), theirs), moved(home.path(), theirs)];
        let out = agents_report_with(home.path(), &[verb.into(), "codex".into()], &roots).0;
        assert!(retired_rows(&out).is_empty(), "{verb}: {out}");
        assert!(files.iter().all(|f| f.exists()), "{verb}: {out}");
    }
    let home = aterm_tempfile::tempdir().unwrap();
    let file = primed(home.path(), ours);
    std::fs::create_dir_all(elsewhere.path()).unwrap();
    let pass = auto_prime_with(home.path(), &roots);
    let codex = pass.agents.iter().find(|a| a.agent == "codex").unwrap();
    assert!(
        codex
            .wrote
            .iter()
            .any(|w| w.starts_with(&format!("~/{rel} "))),
        "{codex:?}"
    );
    assert!(!file.exists());
}

/// Why codex's skill loader (`codex-rs/skills/src/parser.rs`) would refuse `body` at `path`,
/// or `None`; also the folder rule both Claude Code and codex follow, a description that
/// is a plain one-line YAML scalar, and the 1024 characters codex shows the model of one.
fn skill_refusal(path: &str, body: &str) -> Option<String> {
    let mut lines = body.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Some("no `---` frontmatter".to_string());
    }
    let head: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "---").collect();
    if head.is_empty() || !body.lines().skip(1).any(|l| l.trim() == "---") {
        return Some("no closing `---`".to_string());
    }
    let field = |key: &str| -> Option<&str> {
        head.iter()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .map(str::trim)
    };
    let name = field("name").unwrap_or_default();
    let folder = path.rsplit('/').nth(1).unwrap_or_default();
    if name.is_empty() || name.chars().count() > 64 {
        return Some(format!("name `{name}`: empty or over 64 characters"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Some(format!(
            "name `{name}`: not lowercase letters, digits and hyphens"
        ));
    }
    if name != folder {
        return Some(format!("name `{name}` is not its folder `{folder}`"));
    }
    let description = field("description").unwrap_or_default();
    if description.is_empty() {
        return Some("no description".to_string());
    }
    if description.chars().count() > 1024 {
        return Some("description over 1024 characters".to_string());
    }
    let indicator = description.starts_with([
        '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@',
        '`',
    ]);
    if indicator || description.contains(": ") || description.contains(" #") {
        return Some("description is not a plain YAML scalar".to_string());
    }
    None
}

/// EVERY SKILL.md ATERM WRITES PASSES CODEX'S FRONTMATTER CHECK: a `---` head with a closing
/// `---`, a `name:` of at most 64 lowercase letters, digits and hyphens that names its
/// folder, and a non-empty `description:` that is a plain one-line YAML scalar of at most
/// 1024 characters. NEGATIVE CONTROL: the check refuses a body with no head, a 65-character
/// name, a name its folder does not carry, an empty description and one with `: ` in it.
#[test]
fn every_skill_md_passes_codexs_frontmatter_check() {
    let mut checked = 0;
    for s in AGENT_FILES.iter().flat_map(|a| skills_for(a.name)) {
        if s.path.ends_with("/SKILL.md") {
            assert_eq!(skill_refusal(s.path, s.body), None, "{}", s.path);
            checked += 1;
        }
    }
    assert_eq!(
        checked,
        skills_for("claude").len() + skills_for("codex").len() + 1,
        "claude's, codex's and muse's one"
    );
    let path = ".codex/skills/x-y/SKILL.md";
    let long = "n".repeat(65);
    for refused in [
        "<!-- aterm skill v1 -->\nbody\n".to_string(),
        format!("---\nname: {long}\ndescription: d\n---\n"),
        "---\nname: x-z\ndescription: d\n---\n".to_string(),
        "---\nname: x-y\ndescription:\n---\n".to_string(),
        "---\nname: x-y\ndescription: Use for this: that\n---\n".to_string(),
    ] {
        assert!(skill_refusal(path, &refused).is_some(), "{refused}");
    }
    assert_eq!(
        skill_refusal(path, "---\nname: x-y\ndescription: d\n---\n"),
        None
    );
}

/// The `skills/<name>/SKILL.md` rows in `rows` that one skills folder linked into two homes
/// would make one file, but whose bodies differ.
fn split_bodies<'s>(rows: &[(&'s str, &'s str)]) -> Vec<&'s str> {
    let skill = |path: &'s str| path.rsplit_once("/skills/").map(|(_, rest)| rest);
    let mut split = Vec::new();
    for (i, &(path, body)) in rows.iter().enumerate() {
        let Some(name) = skill(path) else { continue };
        if rows[..i]
            .iter()
            .any(|&(p, b)| skill(p) == Some(name) && b != body)
        {
            split.push(path);
        }
    }
    split
}

/// A SKILL TWO AGENTS CAN SHARE IS ONE BODY: every agent's `skills/<name>/SKILL.md` for one
/// `<name>` is byte for byte the same, so a skills folder linked into two homes holds a file
/// each row reads as current, and the fabric skill every agent gets carries Codex's sandbox
/// note. NEGATIVE CONTROL: the check names a codex fabric skill that adds a line.
#[test]
fn a_skill_two_agents_can_share_is_one_body() {
    let rows: Vec<(&str, &str)> = AGENT_FILES
        .iter()
        .flat_map(|a| skills_for(a.name))
        .map(|s| (s.path, s.body))
        .collect();
    assert_eq!(split_bodies(&rows), Vec::<&str>::new());
    let note = "Codex tool shells can have different sandbox permissions";
    for agent in ["claude", "codex", "muse"] {
        let fabric = skills_for(agent)
            .iter()
            .find(|s| doc_label(s.path) == "aterm-fabric")
            .unwrap();
        assert!(fabric.body.contains(note), "{}", fabric.path);
    }
    let forked = format!("{FABRIC_SKILL_BODY}\nCodex only.\n");
    let control = [
        (".claude/skills/aterm-fabric/SKILL.md", FABRIC_SKILL_BODY),
        (".codex/skills/aterm-fabric/SKILL.md", forked.as_str()),
        (".codex/skills/drive-aterm/SKILL.md", DRIVE_SKILL_BODY),
    ];
    assert_eq!(
        split_bodies(&control),
        [".codex/skills/aterm-fabric/SKILL.md"]
    );
}

/// A SKILLS FOLDER TWO AGENTS SHARE SETTLES AND OUTLIVES ONE AGENT'S REMOVE: with
/// `~/.codex/skills` linked to `~/.claude/skills`, the first pass writes each skill once,
/// the second writes nothing, a second `install` finds every file current, and `remove codex`
/// (or `remove claude`) keeps each file the other agent reads, naming it. NEGATIVE CONTROL:
/// `remove claude codex` deletes every one.
#[cfg(unix)]
#[test]
fn a_skills_folder_two_agents_share_settles_and_outlives_one_agents_remove() {
    let home = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude/skills")).unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    std::os::unix::fs::symlink("../.claude/skills", home.path().join(".codex/skills")).unwrap();
    let current = || {
        skills_for("claude")
            .iter()
            .filter(|s| {
                std::fs::read_to_string(home.path().join(s.path)).ok() == Some(s.body.to_string())
            })
            .count()
    };
    let n = skills_for("claude").len();

    let first = auto_prime(home.path());
    let wrote: Vec<&str> = first
        .agents
        .iter()
        .flat_map(|a| a.wrote.iter().map(String::as_str))
        .filter(|w| w.contains("/skills/"))
        .collect();
    assert_eq!(wrote.len(), n, "each skill written once: {wrote:?}");
    assert_eq!(current(), n);
    let second = auto_prime(home.path());
    assert!(!second.changed(), "{}", second.summary);

    let (out, code) = agents_report(home.path(), &["install".to_string()]);
    assert_eq!(code, 0, "{out}");
    let rows = skill_rows(&out);
    assert_eq!(rows.len(), 2 * n, "{out}");
    assert!(rows.iter().all(|(_, v)| v == "already installed"), "{out}");

    for (agent, other) in [("codex", "claude"), ("claude", "codex")] {
        let (out, code) = agents_report(home.path(), &["remove".into(), agent.into()]);
        assert_eq!(code, 0, "{out}");
        let rows = skill_rows(&out);
        assert_eq!(rows.len(), n, "{out}");
        let kept = format!("kept — {other} reads it too");
        assert!(rows.iter().all(|(_, v)| v.starts_with(&kept)), "{out}");
        assert_eq!(current(), n, "{agent}: {out}");
    }

    let (out, code) = agents_report(
        home.path(),
        &["remove".into(), "claude".into(), "codex".into()],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(current(), 0, "{out}");
    let verdicts: Vec<String> = skill_rows(&out).into_iter().map(|(_, v)| v).collect();
    assert_eq!(
        verdicts,
        [vec!["removed"; n], vec!["nothing to remove"; n]].concat(),
        "{out}"
    );
}

#[test]
fn a_save_between_read_and_write_of_a_context_file_stands() {
    let home = aterm_tempfile::tempdir().unwrap();
    let dir = home.path().join(".gemini");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("GEMINI.md");
    let user = "# Gemini rules\n";
    let run = |sub: &str, mut save: Box<dyn FnMut(&Path)>| {
        let mut once = true;
        let (out, code) = saving_after_read(
            move |path| {
                if std::mem::take(&mut once) {
                    save(path);
                }
            },
            || agents_report(home.path(), &[sub.to_string(), "agy".to_string()]),
        );
        assert_eq!(code, 0, "{out}");
        std::fs::read_to_string(&file).unwrap()
    };
    let created = run(
        "install",
        Box::new(move |path| std::fs::write(path, user).unwrap()),
    );
    assert_eq!(created, format!("{user}\n{}", generic()));
    let removed = run(
        "remove",
        Box::new(|path| {
            let text = std::fs::read_to_string(path).unwrap() + "Saved over it.\n";
            std::fs::write(path.with_extension("md.swp"), text).unwrap();
            std::fs::rename(path.with_extension("md.swp"), path).unwrap();
        }),
    );
    assert_eq!(removed, format!("{user}\nSaved over it.\n"));
}

fn saving_after_read<R>(save: impl FnMut(&Path) + 'static, run: impl FnOnce() -> R) -> R {
    SAVE_AFTER_READ.set(Some(Box::new(save)));
    let ran = run();
    SAVE_AFTER_READ.set(None);
    ran
}

#[test]
fn agents_usage_errors_exit_2_and_primer_prints_the_block() {
    let home = aterm_tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        agents_report(home.path(), &args)
    };
    let cases: [(&[&str], i32); 8] = [
        (&["frobnicate"], 2),
        (&["install", "copilot"], 2),
        (&["remove", "copilot"], 2),
        (&["primer", "copilot"], 2),
        (&["primer", "claude", "codex"], 2),
        (&["-h"], 0),
        (&["--help"], 0),
        (&["help"], 0),
    ];
    for (args, code) in cases {
        assert_eq!(run(args).1, code, "{args:?}");
    }
    assert_eq!(run(&["primer"]), (generic(), 0));
    assert_eq!(run(&["primer", "codex"]), (primer_block(Some("codex")), 0));
}

fn first_pass_files(agent: &str) -> Vec<String> {
    let a = AGENT_FILES
        .iter()
        .find(|a| a.name == agent)
        .expect("a registry agent");
    std::iter::once(format!("~/{}", a.file))
        .chain(skills_for(agent).iter().map(|s| format!("~/{}", s.path)))
        .collect()
}

#[test]
fn auto_prime_primes_detected_agents_only_and_is_idempotent() {
    let home = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();

    let first = auto_prime(home.path());
    let rows: Vec<(&str, Outcome)> = first
        .agents
        .iter()
        .map(|a| (a.agent, a.outcome.clone()))
        .collect();
    assert_eq!(
        rows,
        [
            ("claude", Outcome::Installed),
            ("codex", Outcome::Installed)
        ]
    );
    assert_eq!(first.agents[0].wrote, first_pass_files("claude"));
    assert_eq!(first.agents[1].wrote, first_pass_files("codex"));
    assert_eq!(
        std::fs::read_to_string(home.path().join(".claude/CLAUDE.md")).unwrap(),
        primer_block(Some("claude"))
    );
    for s in skills_for("claude") {
        assert_eq!(
            std::fs::read_to_string(home.path().join(s.path)).unwrap(),
            s.body
        );
    }
    assert!(!home.path().join(".gemini").exists());
    assert!(!home.path().join(".config").exists());

    let second = auto_prime(home.path());
    assert!(
        second
            .agents
            .iter()
            .all(|a| a.outcome == Outcome::Unchanged),
        "{:?}",
        second.agents
    );
    assert!(!second.changed());
}

#[test]
fn auto_prime_upgrades_every_older_block_in_place() {
    let v5_block = format!(
        "<!-- aterm primer v5 — managed by `aterm agents`; `aterm agents remove` uninstalls -->\n{PRIMER_BODY}\n{MARK_END}\n"
    );
    let v40_block = "<!-- aterm primer v40 — managed by `aterm agents`; `aterm agents remove` uninstalls -->\n\
Every `aterm ctl` verb here answers `Operation not permitted (os error 1)`: the control\n\
socket is refused by this sandbox — nothing to configure.\n\
<!-- /aterm primer -->\n";
    for (version, stale) in [
        ("v1", V1_BLOCK),
        ("v2", V2_BLOCK),
        ("v3", V3_BLOCK),
        ("v5", v5_block.as_str()),
        ("v40", v40_block),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let file = home.path().join(".codex/AGENTS.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, format!("mine\n\n{stale}\ntheirs\n")).unwrap();

        let pass = auto_prime(home.path());
        assert_eq!(
            pass.agents[0].outcome,
            Outcome::Updated,
            "{version}: {}",
            pass.summary
        );
        assert_eq!(pass.agents[0].wrote, first_pass_files("codex"), "{version}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("mine\n\n{}\ntheirs\n", primer_block(Some("codex"))),
            "{version}"
        );
        assert_eq!(
            auto_prime(home.path()).agents[0].outcome,
            Outcome::Unchanged,
            "{version}"
        );
    }
}

#[test]
fn auto_prime_replaces_completion_rules_in_v24_and_supervise_v3() {
    const OLD_PRIMER: &str = "\
<!-- aterm primer v24 — managed by `aterm agents`; `aterm agents remove` uninstalls -->
## aterm
A finished job's final report ends with `ATERM-TASK-DONE` on a line by itself, even when it also answers a person, which stops the continuations.
<!-- /aterm primer -->
";
    const OLD_SKILL: &str = "\
---
name: supervise-agent
description: Supervise a worker.
---
<!-- aterm skill v3 — MANAGED FILE, written by `aterm agents install`. -->
A worker's report ends with `ATERM-TASK-DONE` on a line by itself.
";
    let home = aterm_tempfile::tempdir().unwrap();
    let primer = home.path().join(".claude/CLAUDE.md");
    let skill = home.path().join(".claude/skills/supervise-agent/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&primer, format!("mine\n\n{OLD_PRIMER}\ntheirs\n")).unwrap();
    std::fs::write(&skill, OLD_SKILL).unwrap();

    let pass = auto_prime(home.path());
    assert_eq!(pass.agents[0].outcome, Outcome::Updated);
    assert_eq!(
        std::fs::read_to_string(&primer).unwrap(),
        format!("mine\n\n{}\ntheirs\n", primer_block(Some("claude")))
    );
    assert_eq!(
        std::fs::read_to_string(&skill).unwrap(),
        SUPERVISE_SKILL_BODY
    );
    assert_eq!(
        auto_prime(home.path()).agents[0].outcome,
        Outcome::Unchanged
    );
}

fn shell_prime_pending(home: &Path) -> PrimePending {
    auto_prime_pending_with(home, &Roots::default())
}

/// `shell_prime_pending` reads what the primer pass would write, writing nothing: before a
/// pass every file of a detected agent is due, after it none (so `aterm init`'s read-back
/// settles), and a block a newer aterm wrote or a skill the person wrote is never due, as the
/// pass leaves both. Negative control: an older block is due again.
#[test]
fn shell_prime_pending_reads_what_the_pass_would_write() {
    let home = aterm_tempfile::tempdir().unwrap();
    let none = shell_prime_pending(home.path());
    assert!(none.detected.is_empty() && none.due.is_empty(), "{none:?}");
    assert!(none.looked.contains(&"~/.claude".to_owned()), "{none:?}");

    std::fs::create_dir(home.path().join(".claude")).unwrap();
    let before = shell_prime_pending(home.path());
    assert_eq!(before.detected, ["claude"]);
    assert!(
        before
            .due
            .contains(&"~/.claude/CLAUDE.md (absent)".to_owned()),
        "{before:?}"
    );
    assert_eq!(
        before.due.len(),
        1 + skills_for("claude").len(),
        "{before:?}"
    );
    assert!(before.errors.is_empty(), "{before:?}");
    assert_eq!(
        std::fs::read_dir(home.path().join(".claude"))
            .unwrap()
            .count(),
        0,
        "reading writes nothing"
    );

    assert!(auto_prime(home.path()).changed());
    let after = shell_prime_pending(home.path());
    assert!(after.due.is_empty() && after.errors.is_empty(), "{after:?}");

    let claude_md = home.path().join(".claude/CLAUDE.md");
    std::fs::write(&claude_md, V1_BLOCK).unwrap();
    assert_eq!(
        shell_prime_pending(home.path()).due,
        ["~/.claude/CLAUDE.md (stale)"]
    );

    let newer = primer_block(Some("claude")).replacen("aterm primer v", "aterm primer v9", 1);
    assert!(is_newer(&newer, &primer_block(Some("claude"))));
    std::fs::write(&claude_md, &newer).unwrap();
    let skill = home.path().join(".claude/skills/drive-aterm/SKILL.md");
    std::fs::write(&skill, "---\nname: drive-aterm\n---\nmy own notes\n").unwrap();
    let left = shell_prime_pending(home.path());
    assert!(left.due.is_empty() && left.errors.is_empty(), "{left:?}");
    assert!(
        !auto_prime(home.path()).changed(),
        "the pass leaves both too"
    );
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), newer);
}

/// A skill folder of one agent's that links to another's not yet made is no error: the pass
/// writes the first agent's skills, which fills the link, so the probe reads each shared file
/// once and settles after the pass. Negative control: a link no agent's write fills is an
/// error.
#[cfg(unix)]
#[test]
fn shell_prime_pending_reads_a_shared_skill_folder_once() {
    let home = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".claude")).unwrap();
    std::fs::create_dir(home.path().join(".codex")).unwrap();
    std::os::unix::fs::symlink("../.claude/skills", home.path().join(".codex/skills")).unwrap();
    let before = shell_prime_pending(home.path());
    assert_eq!(before.detected, ["claude", "codex"]);
    assert!(before.errors.is_empty(), "{before:?}");
    assert!(
        !before
            .due
            .iter()
            .any(|due| due.starts_with("~/.codex/skills/")),
        "a skill codex shares with claude is claude's to write: {before:?}"
    );
    auto_prime(home.path());
    let after = shell_prime_pending(home.path());
    assert!(after.due.is_empty() && after.errors.is_empty(), "{after:?}");

    let skill = home.path().join(".claude/skills/drive-aterm/SKILL.md");
    std::fs::remove_file(&skill).unwrap();
    std::os::unix::fs::symlink(home.path().join("gone/SKILL.md"), &skill).unwrap();
    let dangling = shell_prime_pending(home.path());
    assert_eq!(dangling.errors.len(), 1, "{dangling:?}");
    assert!(
        dangling.errors[0].starts_with("claude: ~/.claude/skills/drive-aterm/SKILL.md: "),
        "{dangling:?}"
    );
}

#[test]
fn auto_prime_never_overwrites_a_foreign_skill() {
    let home = aterm_tempfile::tempdir().unwrap();
    let skill = home.path().join(".claude/skills/drive-aterm/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    let theirs = "---\nname: drive-aterm\n---\nmy own notes\n";
    std::fs::write(&skill, theirs).unwrap();

    let first = auto_prime(home.path());
    assert_eq!(first.agents[0].outcome, Outcome::Installed);
    assert_eq!(std::fs::read_to_string(&skill).unwrap(), theirs);
    let second = auto_prime(home.path());
    assert_eq!(second.agents[0].outcome, Outcome::SkippedForeign);
    assert!(!second.changed());
    assert_eq!(std::fs::read_to_string(&skill).unwrap(), theirs);

    let latin1: &[u8] = b"---\nname: drive-aterm\ndescription: my caf\xe9 notes\n---\n";
    std::fs::write(&skill, latin1).unwrap();
    let third = auto_prime(home.path());
    assert_eq!(third.agents[0].outcome, Outcome::SkippedForeign);
    assert_eq!(std::fs::read(&skill).unwrap(), latin1);

    std::fs::remove_file(&skill).unwrap();
    std::fs::create_dir(&skill).unwrap();
    let dir_note = skill.join("notes.md");
    std::fs::write(&dir_note, theirs).unwrap();
    let dir_err = auto_prime(home.path());
    assert!(
        matches!(dir_err.agents[0].outcome, Outcome::Error(_)),
        "{:?}",
        dir_err.agents[0]
    );
    assert_eq!(std::fs::read_to_string(&dir_note).unwrap(), theirs);
    std::fs::remove_dir_all(&skill).unwrap();
    std::fs::write(&skill, latin1).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&skill, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read(&skill).is_err();
        let locked = auto_prime(home.path());
        std::fs::set_permissions(&skill, std::fs::Permissions::from_mode(0o600)).unwrap();
        if unreadable {
            assert!(
                matches!(locked.agents[0].outcome, Outcome::Error(_)),
                "{:?}",
                locked.agents[0]
            );
        } else {
            assert_eq!(locked.agents[0].outcome, Outcome::SkippedForeign);
        }
        assert_eq!(std::fs::read(&skill).unwrap(), latin1);
    }
}

#[test]
fn a_corrupt_block_is_an_error_row_that_stops_nothing_else() {
    let home = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let corrupt = "x\n<!-- aterm primer v1 -->\nno end marker\n";
    let claude_md = home.path().join(".claude/CLAUDE.md");
    std::fs::write(&claude_md, corrupt).unwrap();

    let pass = auto_prime(home.path());
    let row = &pass.agents[0];
    assert!(matches!(row.outcome, Outcome::Error(_)), "{row:?}");
    assert_eq!(
        row.wrote,
        first_pass_files("claude")[1..].to_vec(),
        "every claude doc, and not the primer file it refused"
    );
    assert!(pass.changed(), "an error row that wrote: {}", pass.summary);
    assert_eq!(pass.errors().count(), 1);
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), corrupt);

    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    let second = auto_prime(home.path());
    assert!(matches!(second.agents[0].outcome, Outcome::Error(_)));
    assert!(second.agents[0].wrote.is_empty(), "{:?}", second.agents[0]);
    assert_eq!(second.agents[1].outcome, Outcome::Installed);
    assert!(
        !auto_prime(home.path()).changed(),
        "an error row that wrote nothing is no change"
    );
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), corrupt);
}

#[test]
fn an_agent_folder_taken_mid_pass_is_never_made_again() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    let moved = home.path().join("moved");
    let (from, to) = (claude.clone(), moved.clone());
    let mut first = true;
    let pass = saving_after_read(
        move |_| {
            if std::mem::take(&mut first) {
                std::fs::rename(&from, &to).unwrap();
            }
        },
        || auto_prime_with(home.path(), &Roots::default()),
    );
    assert!(!claude.exists(), "the taken folder is not made again");
    assert!(
        std::fs::read_dir(&moved).unwrap().next().is_none(),
        "and nothing landed where it went"
    );
    let rows: Vec<&str> = pass.agents.iter().map(|a| a.agent).collect();
    assert_eq!(rows, ["claude", "codex"]);
    assert!(
        matches!(pass.agents[0].outcome, Outcome::Error(_)),
        "{:?}",
        pass.agents[0]
    );
    assert!(pass.agents[0].wrote.is_empty(), "{:?}", pass.agents[0]);
    assert_eq!(pass.agents[1].outcome, Outcome::Installed);
    assert!(home.path().join(".codex/AGENTS.md").is_file());

    let (_, code) = agents_report(home.path(), &["install".into(), "claude".into()]);
    assert_eq!(code, 0);
    assert!(
        claude.join("CLAUDE.md").is_file(),
        "a named install makes it"
    );
}

#[cfg(unix)]
fn counted_pass(home: &Path) -> (AutoPrime, usize) {
    let reads = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = std::rc::Rc::clone(&reads);
    let pass = saving_after_read(
        move |path| {
            if path.extension() == Some("md".as_ref()) {
                count.set(count.get() + 1);
            }
        },
        || auto_prime(home),
    );
    (pass, reads.get())
}

#[cfg(unix)]
#[test]
fn agents_that_share_one_context_file_get_one_block() {
    use std::os::unix::fs::MetadataExt as _;
    let home = aterm_tempfile::tempdir().unwrap();
    let claude_md = home.path().join(".claude/CLAUDE.md");
    let codex_md = home.path().join(".codex/AGENTS.md");
    std::fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
    std::fs::create_dir_all(codex_md.parent().unwrap()).unwrap();
    let left = format!(
        "# mine\n\n{}",
        marked(&primer_block(Some("codex")), ROW_BY_ROW_VERSION)
    );
    std::fs::write(&claude_md, left).unwrap();
    std::os::unix::fs::symlink(&claude_md, &codex_md).unwrap();
    let shared = format!("# mine\n\n{}", shared_by(&["claude", "codex"]));

    let (first, reads) = counted_pass(home.path());
    assert_eq!(reads, 1, "one upsert of the shared file: {}", first.summary);
    let rows: Vec<(&str, Outcome)> = first
        .agents
        .iter()
        .map(|a| (a.agent, a.outcome.clone()))
        .collect();
    assert_eq!(
        rows,
        [("claude", Outcome::Updated), ("codex", Outcome::Updated)]
    );
    assert_eq!(first.agents[0].wrote, first_pass_files("claude"));
    assert_eq!(
        first.agents[1].wrote,
        first_pass_files("codex")[1..].to_vec(),
        "the shared file is named once"
    );
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), shared);
    assert!(std::fs::symlink_metadata(&codex_md).unwrap().is_symlink());
    let inode = std::fs::metadata(&claude_md).unwrap().ino();

    let (second, _) = counted_pass(home.path());
    assert!(
        second
            .agents
            .iter()
            .all(|a| a.outcome == Outcome::Unchanged),
        "{:?}",
        second.agents
    );
    assert!(!second.changed());
    assert_eq!(std::fs::metadata(&claude_md).unwrap().ino(), inode);

    let (status, _) = agents_report(home.path(), &["status".to_string()]);
    for (agent, other) in [("claude", "codex"), ("codex", "claude")] {
        let row = status.lines().find(|l| l.starts_with(agent)).unwrap();
        assert_eq!(row.split_whitespace().nth(2), Some("installed"), "{status}");
        assert!(row.contains(other), "the row names the sharing: {status}");
    }
    assert!(
        status_line_with(home.path(), &Roots::default())
            .starts_with("claude installed, codex installed,")
    );

    let (out, code) = agents_report(home.path(), &["install".into(), "codex".into()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), shared);
    assert_eq!(std::fs::metadata(&claude_md).unwrap().ino(), inode, "{out}");

    let (out, code) = agents_report(home.path(), &["remove".into()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), "# mine\n");
    let verdict = |agent: &str| {
        out.lines()
            .find(|l| l.starts_with(agent))
            .and_then(|l| l.split_whitespace().nth(2))
    };
    assert_eq!(verdict("claude"), Some("removed"), "{out}");
    assert_eq!(verdict("codex"), Some("removed"), "{out}");

    std::fs::remove_file(&codex_md).unwrap();
    std::fs::write(&codex_md, "# codex\n").unwrap();
    let (apart, reads) = counted_pass(home.path());
    assert_eq!(reads, 2, "{}", apart.summary);
    assert_eq!(
        std::fs::read_to_string(&claude_md).unwrap(),
        format!("# mine\n\n{}", primer_block(Some("claude")))
    );
    assert_eq!(
        std::fs::read_to_string(&codex_md).unwrap(),
        format!("# codex\n\n{}", primer_block(Some("codex")))
    );
}

/// The marker version of the last aterm that primed row by row (0.113.0):
/// each detected agent's own block, in registry order, into its context
/// file, so a file Codex shares with Claude Code was left holding Codex's.
#[cfg(unix)]
const ROW_BY_ROW_VERSION: u32 = 33;

/// `block` as an aterm that marks its blocks `v{version}` writes it.
#[cfg(unix)]
fn marked(block: &str, version: u32) -> String {
    let ours = marker_version(MARK_BEGIN).expect("our marker names its version");
    block.replacen(&format!("v{ours} "), &format!("v{version} "), 1)
}

/// A file two agents share keeps its block while an aterm that primed row
/// by row still runs beside this one (each window is its own process,
/// updated on its own): the block's marker is newer than that aterm's, so
/// its pass reads it `newer` and writes nothing. At an equal version its
/// Codex row read the shared block `stale` and wrote Codex's own block over
/// Claude Code's file, and the two builds rewrote it in turn for as long as
/// both ran. NEGATIVE CONTROL: that
/// aterm still primes a file Codex alone reads, where nothing newer is, and
/// this pass then reads its block `stale` and rewrites it.
#[cfg(unix)]
#[test]
fn a_row_by_row_aterm_leaves_a_shared_files_block() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude_md = home.path().join(".claude/CLAUDE.md");
    let codex_md = home.path().join(".codex/AGENTS.md");
    std::fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
    std::fs::create_dir_all(codex_md.parent().unwrap()).unwrap();
    std::fs::write(&claude_md, "# mine\n").unwrap();
    std::os::unix::fs::symlink(&claude_md, &codex_md).unwrap();
    auto_prime(home.path());
    let shared = format!("# mine\n\n{}", shared_by(&["claude", "codex"]));
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), shared);

    // That aterm's pass: every detected agent's own block, row by row.
    let row_by_row = || -> Vec<(&str, Result<PrimerWrite, String>)> {
        AGENT_FILES
            .iter()
            .filter(|a| detected(home.path(), &Roots::default(), a))
            .map(|a| {
                let dir = home_join(home.path(), &Roots::default(), a.dir);
                let block = marked(&block_for(a), ROW_BY_ROW_VERSION);
                let file = home_join(home.path(), &Roots::default(), a.file);
                (a.name, upsert_primer_file(&file, &block, Room::Below(&dir)))
            })
            .collect()
    };
    assert_eq!(
        row_by_row(),
        [
            ("claude", Ok(PrimerWrite::Current)),
            ("codex", Ok(PrimerWrite::Current))
        ]
    );
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), shared);
    let pass = auto_prime(home.path());
    assert!(!pass.changed(), "{}", pass.summary);

    std::fs::remove_file(&codex_md).unwrap();
    std::fs::write(&codex_md, "# codex\n").unwrap();
    assert_eq!(
        row_by_row(),
        [
            ("claude", Ok(PrimerWrite::Current)),
            ("codex", Ok(PrimerWrite::Appended))
        ]
    );
    let pass = auto_prime(home.path());
    let rows: Vec<(&str, Outcome)> = pass
        .agents
        .iter()
        .map(|a| (a.agent, a.outcome.clone()))
        .collect();
    assert_eq!(
        rows,
        [("claude", Outcome::Updated), ("codex", Outcome::Updated)]
    );
    assert_eq!(
        std::fs::read_to_string(&claude_md).unwrap(),
        format!("# mine\n\n{}", primer_block(Some("claude")))
    );
    assert_eq!(
        std::fs::read_to_string(&codex_md).unwrap(),
        format!("# codex\n\n{}", primer_block(Some("codex")))
    );
}

#[cfg(unix)]
#[test]
fn a_file_not_made_yet_is_one_file_through_a_linked_folder() {
    let home = aterm_tempfile::tempdir().unwrap();
    let opencode = home.path().join(".config/opencode");
    std::fs::create_dir_all(&opencode).unwrap();
    std::os::unix::fs::symlink(&opencode, home.path().join(".codex")).unwrap();

    let (first, reads) = counted_pass(home.path());
    assert_eq!(reads, 1, "{}", first.summary);
    let rows: Vec<(&str, Outcome)> = first
        .agents
        .iter()
        .map(|a| (a.agent, a.outcome.clone()))
        .collect();
    assert_eq!(
        rows,
        [
            ("codex", Outcome::Installed),
            ("opencode", Outcome::Installed)
        ]
    );
    assert_eq!(
        std::fs::read_to_string(opencode.join("AGENTS.md")).unwrap(),
        shared_by(&["codex", "opencode"])
    );
    assert!(!counted_pass(home.path()).0.changed());
}

#[cfg(unix)]
#[test]
fn a_link_made_before_its_file_is_that_file_from_the_first_pass() {
    for (link, target) in [
        (".codex/AGENTS.md", ".claude/CLAUDE.md"),
        (".claude/CLAUDE.md", ".codex/AGENTS.md"),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        for dir in [".claude", ".codex"] {
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        std::os::unix::fs::symlink(home.path().join(target), home.path().join(link)).unwrap();

        let (first, reads) = counted_pass(home.path());
        assert_eq!(reads, 1, "{link}: {}", first.summary);
        let rows: Vec<(&str, Outcome)> = first
            .agents
            .iter()
            .map(|a| (a.agent, a.outcome.clone()))
            .collect();
        assert_eq!(
            rows,
            [
                ("claude", Outcome::Installed),
                ("codex", Outcome::Installed)
            ],
            "{link}"
        );
        assert_eq!(first.agents[0].wrote[0], format!("~/{target}"));
        assert_eq!(
            first.agents[1].wrote,
            first_pass_files("codex")[1..].to_vec()
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(target)).unwrap(),
            shared_by(&["claude", "codex"]),
            "{link}"
        );
        assert!(!counted_pass(home.path()).0.changed(), "{link}");
        assert!(
            status_line_with(home.path(), &Roots::default())
                .starts_with("claude installed, codex installed,"),
            "{link}"
        );
    }

    let home = aterm_tempfile::tempdir().unwrap();
    let claude_md = home.path().join(".claude/CLAUDE.md");
    let codex_md = home.path().join(".codex/AGENTS.md");
    std::fs::create_dir_all(claude_md.parent().unwrap()).unwrap();
    std::fs::create_dir_all(codex_md.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&codex_md, &claude_md).unwrap();
    std::os::unix::fs::symlink(&claude_md, &codex_md).unwrap();
    let looped = auto_prime(home.path());
    assert!(
        looped
            .agents
            .iter()
            .all(|a| matches!(a.outcome, Outcome::Error(_))),
        "{:?}",
        looped.agents
    );
    for md in [&claude_md, &codex_md] {
        assert!(std::fs::symlink_metadata(md).unwrap().is_symlink());
    }
}

#[cfg(unix)]
#[test]
fn an_install_through_a_folder_link_made_before_its_folder_makes_one_file() {
    let verdict = |out: &str, agent: &str| {
        out.lines()
            .find(|l| l.starts_with(agent))
            .and_then(|l| l.split_whitespace().nth(2))
            .map(str::to_string)
    };
    for names in [["codex", "opencode"], ["opencode", "codex"]] {
        let home = aterm_tempfile::tempdir().unwrap();
        let opencode = home.path().join(".config/opencode");
        std::os::unix::fs::symlink(&opencode, home.path().join(".codex")).unwrap();
        let install: Vec<String> = std::iter::once("install")
            .chain(names)
            .map(String::from)
            .collect();

        let (out, code) = agents_report(home.path(), &install);
        assert_eq!(code, 0, "{out}");
        for agent in names {
            assert_eq!(verdict(&out, agent).as_deref(), Some("installed"), "{out}");
        }
        assert_eq!(
            std::fs::read_to_string(opencode.join("AGENTS.md")).unwrap(),
            shared_by(&["codex", "opencode"]),
            "{out}"
        );
        let (again, _) = agents_report(home.path(), &install);
        for agent in names {
            assert_eq!(
                verdict(&again, agent).as_deref(),
                Some("already"),
                "{again}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_named_install_shares_the_file_a_folder_link_waits_for() {
    let install = ["install".to_string(), "codex".to_string()];
    for linked in ["opencode", "muse"] {
        let home = aterm_tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(home.path().join(".config")).unwrap();
        std::os::unix::fs::symlink(&codex, home.path().join(".config").join(linked)).unwrap();

        let (out, code) = agents_report(home.path(), &install);
        assert_eq!(code, 0, "{linked}: {out}");
        assert_eq!(
            std::fs::read_to_string(codex.join("AGENTS.md")).unwrap(),
            shared_by(&["codex", linked]),
            "{linked}: {out}"
        );
        let (status, _) = agents_report(home.path(), &["status".to_string()]);
        for agent in ["codex", linked] {
            let row = status.lines().find(|l| l.starts_with(agent)).unwrap();
            assert_eq!(row.split_whitespace().nth(2), Some("installed"), "{status}");
        }
    }

    let home = aterm_tempfile::tempdir().unwrap();
    let opencode = home.path().join(".config/opencode");
    std::os::unix::fs::symlink(&opencode, home.path().join(".codex")).unwrap();
    let (out, code) = agents_report(home.path(), &install);
    assert_eq!(code, 1, "{out}");
    assert!(!opencode.exists(), "{out}");

    let home = aterm_tempfile::tempdir().unwrap();
    let (out, code) = agents_report(home.path(), &install);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        std::fs::read_to_string(home.path().join(".codex/AGENTS.md")).unwrap(),
        primer_block(Some("codex"))
    );
}

#[cfg(unix)]
#[test]
fn status_reads_a_link_another_reader_makes_live_as_the_file_not_made_yet() {
    let home = aterm_tempfile::tempdir().unwrap();
    for dir in [".claude", ".codex"] {
        std::fs::create_dir_all(home.path().join(dir)).unwrap();
    }
    let codex_md = home.path().join(".codex/AGENTS.md");
    std::os::unix::fs::symlink(home.path().join(".claude/CLAUDE.md"), &codex_md).unwrap();

    let (status, _) = agents_report(home.path(), &["status".to_string()]);
    for agent in ["claude", "codex"] {
        let row = status.lines().find(|l| l.starts_with(agent)).unwrap();
        assert!(row.contains("absent (no context file yet)"), "{status}");
    }
    let line = status_line_with(home.path(), &Roots::default());
    assert!(
        line.starts_with("claude absent ")
            && line.contains(", codex absent ")
            && !line.contains("ERROR"),
        "{line}"
    );
    let (out, code) = agents_report(home.path(), &["install".to_string()]);
    assert_eq!(code, 0, "{out}");

    std::fs::remove_file(&codex_md).unwrap();
    std::os::unix::fs::symlink(home.path().join("dotfiles/AGENTS.md"), &codex_md).unwrap();
    let line = status_line_with(home.path(), &Roots::default());
    assert!(
        line.contains("codex ERROR: a symlink whose target does not exist"),
        "{line}"
    );
    let (out, code) = agents_report(home.path(), &["install".to_string()]);
    assert_eq!(code, 1, "{out}");
}

#[cfg(unix)]
#[test]
fn a_link_spelled_in_another_case_is_its_file_where_case_is_ignored() {
    for (link, spelled, made, other) in [
        (
            ".codex/AGENTS.md",
            ".claude/Claude.md",
            ".claude/CLAUDE.md",
            "claude",
        ),
        (
            ".claude/CLAUDE.md",
            ".codex/agents.md",
            ".codex/AGENTS.md",
            "codex",
        ),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        for dir in [".claude", ".codex"] {
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        std::os::unix::fs::symlink(home.path().join(spelled), home.path().join(link)).unwrap();
        let ignores_case = home.path().join(".CLAUDE").is_dir();

        let (first, reads) = counted_pass(home.path());
        let rows: Vec<(&str, Outcome)> = first
            .agents
            .iter()
            .map(|a| (a.agent, a.outcome.clone()))
            .collect();
        let made_md = std::fs::read_to_string(home.path().join(made)).unwrap();
        if ignores_case {
            assert_eq!(reads, 1, "{link}: {}", first.summary);
            assert_eq!(
                rows,
                [
                    ("claude", Outcome::Installed),
                    ("codex", Outcome::Installed)
                ],
                "{link}"
            );
            assert_eq!(made_md, shared_by(&["claude", "codex"]), "{link}");
            assert!(!counted_pass(home.path()).0.changed(), "{link}");
            assert!(
                status_line_with(home.path(), &Roots::default())
                    .starts_with("claude installed, codex installed,"),
                "{link}"
            );
        } else {
            let refused = rows
                .iter()
                .find(|(a, _)| *a != other)
                .is_some_and(|(_, o)| matches!(o, Outcome::Error(_)));
            assert!(refused, "{link}: {rows:?}");
            assert_eq!(made_md, primer_block(Some(other)), "{link}");
            assert!(
                std::fs::symlink_metadata(home.path().join(link))
                    .unwrap()
                    .is_symlink()
            );
        }
    }
}

#[test]
fn relocated_config_drives_detection_install_and_remove_in_one_root() {
    let home = aterm_tempfile::tempdir().unwrap();
    let config = aterm_tempfile::tempdir().unwrap();
    let roots = Roots {
        xdg: Some(config.path().to_path_buf()),
        ..Roots::default()
    };
    let default_file = home.path().join(".config/opencode/AGENTS.md");
    std::fs::create_dir_all(default_file.parent().unwrap()).unwrap();
    let default_user = "# This is the inactive default config\n";
    std::fs::write(&default_file, default_user).unwrap();
    let absent = auto_prime_with(home.path(), &roots);
    assert!(absent.agents.is_empty(), "{}", absent.summary);

    let active_file = config.path().join("opencode/AGENTS.md");
    std::fs::create_dir_all(active_file.parent().unwrap()).unwrap();
    let active_user = "# The active OpenCode instructions\n";
    std::fs::write(&active_file, active_user).unwrap();
    let installed = auto_prime_with(home.path(), &roots);
    let rows: Vec<(&str, Outcome)> = installed
        .agents
        .iter()
        .map(|a| (a.agent, a.outcome.clone()))
        .collect();
    assert_eq!(rows, [("opencode", Outcome::Installed)]);
    assert_eq!(
        std::fs::read_to_string(&active_file).unwrap(),
        format!("{active_user}\n{}", generic())
    );
    for skill in skills_for("opencode") {
        let path = home_join(home.path(), &roots, skill.path);
        assert_eq!(std::fs::read_to_string(path).unwrap(), skill.body);
    }

    let (report, code) = agents_report_with(home.path(), &["remove".into()], &roots);
    assert_eq!(code, 0, "{report}");
    assert_eq!(std::fs::read_to_string(&active_file).unwrap(), active_user);
    for skill in skills_for("opencode") {
        assert!(!home_join(home.path(), &roots, skill.path).exists());
    }
    assert_eq!(
        std::fs::read_to_string(&default_file).unwrap(),
        default_user
    );
}

/// EACH VARIABLE MOVES ONLY ITS OWN ROWS: `$XDG_CONFIG_HOME` a `.config/` path, `$CODEX_HOME`
/// `.codex` and every path in it, and neither one any other path, which shows as `~/…`.
/// NEGATIVE CONTROL: with neither set, every path lands under the home, and a name that only
/// begins with `.codex` stays there with both set.
#[test]
fn each_variable_moves_only_its_own_rows() {
    let home = Path::new("/h");
    let roots = Roots {
        xdg: Some(PathBuf::from("/x/cfg")),
        codex: Some(PathBuf::from("/x/codex")),
    };
    let unset = Roots::default();
    let cases = [
        (".config/opencode/AGENTS.md", "/x/cfg/opencode/AGENTS.md"),
        (".codex", "/x/codex"),
        (".codex/AGENTS.md", "/x/codex/AGENTS.md"),
        (
            ".codex/skills/drive-aterm/SKILL.md",
            "/x/codex/skills/drive-aterm/SKILL.md",
        ),
        (".claude/CLAUDE.md", "/h/.claude/CLAUDE.md"),
        (".codexrc/AGENTS.md", "/h/.codexrc/AGENTS.md"),
    ];
    for (rel, moved) in cases {
        assert_eq!(home_join(home, &roots, rel), PathBuf::from(moved), "{rel}");
        // A moved path shows its variable's folder as given, then each segment as this
        // host joins it (with a backslash on Windows).
        let shown = ["/x/cfg", "/x/codex"]
            .into_iter()
            .find_map(|root| Some((root, moved.strip_prefix(root)?)))
            .map_or_else(
                || format!("~/{rel}"),
                |(root, rest)| {
                    rest.split('/')
                        .filter(|seg| !seg.is_empty())
                        .fold(root.to_string(), |shown, seg| {
                            format!("{shown}{}{seg}", std::path::MAIN_SEPARATOR)
                        })
                },
            );
        assert_eq!(display_path(home, &roots, rel), shown, "{rel}");
        assert_eq!(home_join(home, &unset, rel), home.join(rel), "{rel}");
        assert_eq!(display_path(home, &unset, rel), format!("~/{rel}"), "{rel}");
    }
}

#[test]
fn every_bundled_doc_is_marked_and_parses_for_its_agent() {
    for s in AGENT_FILES.iter().flat_map(|a| skills_for(a.name)) {
        assert!(
            s.body
                .lines()
                .any(|l| l.trim_start().starts_with(SKILL_MARK_PREFIX)),
            "{} lost its `{SKILL_MARK_PREFIX}` marker line",
            s.path
        );
        if !s.path.contains("/skills/") {
            continue;
        }
        let mut lines = s.body.lines();
        assert_eq!(lines.next(), Some("---"), "{} needs frontmatter", s.path);
        let head: Vec<&str> = lines.take_while(|l| *l != "---").collect();
        assert!(
            head.iter().any(|l| l.starts_with("name:")),
            "{} frontmatter needs name:",
            s.path
        );
        let description = head.iter().find_map(|l| l.strip_prefix("description:"));
        assert!(
            description.is_some_and(|d| !d.contains(": ")),
            "{} needs a description: that is plain YAML: {description:?}",
            s.path
        );
    }
    let body = include_str!("../assets/aterm-fabric-body.md");
    assert!(body.starts_with(SKILL_MARK_PREFIX));
    assert!(!body.contains("'''") && !body.contains('\\'));
    for agent in ["claude", "codex", "muse", "opencode"] {
        let fabric = skills_for(agent)
            .iter()
            .find(|s| doc_label(s.path) == "aterm-fabric")
            .unwrap();
        let head: Vec<&str> = fabric.body.lines().take(4).collect();
        if agent == "opencode" {
            assert!(
                fabric.body.starts_with("---\ndescription: "),
                "{}",
                fabric.path
            );
            assert!(
                !head[..3].iter().any(|l| l.starts_with("name:")),
                "{}",
                fabric.path
            );
        } else {
            assert!(
                fabric.body.starts_with("---\nname: aterm-fabric\n"),
                "{}",
                fabric.path
            );
            assert!(
                head.iter().any(|l| l.starts_with("description:")),
                "{}",
                fabric.path
            );
        }
    }
}

#[test]
fn skill_state_distinguishes_ours_from_the_users() {
    let body = "---\nname: x\n---\n<!-- aterm skill v1 -->\nbody\n";
    let older = "---\nname: x\n---\n<!-- aterm skill v0 -->\nold body\n";
    assert_eq!(skill_state(older, body), SkillState::Stale);
    let theirs = "---\nname: x\n---\nmy own notes\n";
    assert_eq!(skill_state(theirs, body), SkillState::Foreign);
}

#[test]
fn the_primer_and_the_skills_teach_served_verbs() {
    let skills = AGENT_FILES
        .iter()
        .flat_map(|a| skills_for(a.name))
        .map(|f| f.body);
    let parts = AGENT_FILES.iter().filter_map(|a| a.addendum);
    for text in std::iter::once(PRIMER_BODY).chain(parts).chain(skills) {
        for (command, verb) in taught_ctl_verbs(text) {
            assert!(
                aterm_types::control_verbs::spec(&verb).is_some()
                    || aterm_ctl::CLIENT_HELP_VERBS.contains(&verb.as_str()),
                "`aterm ctl {command}` teaches `{verb}`, which nothing serves"
            );
        }
    }
}

/// EACH PROGRAM IS TAUGHT THE WAIT ITS SHELL CAN DO: Claude Code's block runs `await task` and
/// `amacs --wait` as a background Bash, whose exit wakes its turn, and keeps the `rm` box its
/// harness answers; codex-alab's shell tool wakes no turn, so Codex's block bounds a
/// foreground wait with `timeout=` and re-arms it with each worker's rev. In every block a
/// line that waits in the background, or names the `rm` box, names Claude Code, and only
/// Claude Code's block and a file it shares carry those lines. Each part sits in the `## aterm`
/// section, after the body and ahead of the Rust note, in registry order where a file two
/// agents share carries both.
#[test]
fn each_program_is_taught_the_wait_its_shell_can_do() {
    let claude_line = |line: &str| {
        line.contains("background Bash")
            || line.contains("in the background")
            || line.contains("${VAR:?}")
    };
    let shared = shared_by(&["claude", "codex"]);
    let blocks = AGENT_FILES
        .iter()
        .map(|a| (a.name, primer_block(Some(a.name))))
        .chain([("generic", primer_block(None)), ("shared", shared.clone())]);
    for (name, block) in blocks {
        let lines: Vec<&str> = block.lines().filter(|l| claude_line(l)).collect();
        for line in &lines {
            assert!(line.starts_with("In Claude Code,"), "{name}: {line}");
        }
        assert_eq!(
            lines.len(),
            if matches!(name, "claude" | "shared") {
                2
            } else {
                0
            },
            "{name}'s Claude Code lines: {lines:?}"
        );
    }
    for block in [primer_block(Some("codex")), shared.clone()] {
        let bounded = taught_ctl_verbs(&block).into_iter().any(|(command, _)| {
            command.starts_with("await task ")
                && command.contains(" since=")
                && command.contains(" timeout=")
        });
        assert!(bounded, "no bounded `await task` in {block}");
        assert!(block.contains("`@<sid>:<rev>`"), "{block}");
    }
    let at = |block: &str, text: &str| {
        block
            .find(text)
            .unwrap_or_else(|| panic!("{text:?} is not in {block}"))
    };
    for (block, parts) in [
        (primer_block(Some("claude")), &[CLAUDE_ADDENDUM][..]),
        (primer_block(Some("codex")), &[CODEX_ADDENDUM][..]),
        (shared, &[CLAUDE_ADDENDUM, CODEX_ADDENDUM][..]),
    ] {
        let mut after = at(&block, PRIMER_BODY) + PRIMER_BODY.len();
        for part in parts {
            let start = at(&block, part);
            assert!(
                start >= after,
                "{part:?} follows the body and the part before it"
            );
            after = start + part.len();
        }
        assert!(
            after <= at(&block, RUST_NOTE),
            "an agent's part is in the `## aterm` section, ahead of `## Rust:`: {block}"
        );
    }
}

#[test]
fn nothing_aterm_installs_names_the_retired_token() {
    let names = |text: &str| text.contains("ATERM-TASK-DONE");
    for agent in AGENT_FILES {
        assert!(
            !names(&primer_block(Some(agent.name))),
            "{}'s primer block names the token",
            agent.name
        );
        for skill in skills_for(agent.name) {
            assert!(!names(skill.body), "{} names the token", skill.path);
        }
    }
    assert!(names(
        "A finished job's final report ends with `ATERM-TASK-DONE` on a line by itself."
    ));
}

/// `taught_ctl_verbs` scans across line breaks, skips global flags and session/placeholder
/// tokens (including `'…'`), and ignores punctuation-only placeholder tokens.
/// NEGATIVE CONTROL: omitting `'…'` or the empty-verb guard pushes `""` for `` `aterm ctl …` ``
/// and `` `aterm ctl ...` `` and drops `"ls"` in `` `aterm ctl … ls` ``.
#[test]
fn a_taught_command_is_read_to_its_verb_across_a_line_break() {
    let verbs = |text: &str| -> Vec<String> {
        taught_ctl_verbs(text).into_iter().map(|(_, v)| v).collect()
    };
    assert_eq!(
        verbs("hold it, `aterm ctl\n  @s-a lease acquire holder=<you>` (renew it)"),
        ["lease"]
    );
    assert_eq!(
        verbs("`aterm ctl --sock \"$SOCK\" ls` and `aterm ctl \"@$SID\" text trim`"),
        ["ls", "text"]
    );
    assert_eq!(
        verbs("every step is an `aterm ctl` verb; `aterm ctl --help` needs no session"),
        Vec::<String>::new()
    );
    assert_eq!(
        verbs(
            "placeholders `aterm ctl …` and `aterm ctl ...` name no verb, while `aterm ctl … ls` names ls"
        ),
        ["ls"]
    );
}

fn code_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut prose = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            spans.push(line.split(" #").next().unwrap_or(line).trim().to_string());
        } else {
            prose.push_str(line);
            prose.push(' ');
        }
    }
    let prose = prose.split_whitespace().collect::<Vec<_>>().join(" ");
    spans.extend(prose.split('`').skip(1).step_by(2).map(str::to_string));
    spans
}

fn placeholder(w: &str) -> bool {
    w.starts_with('<') || w.contains('…') || w == "-"
}

fn node_id(w: &str) -> bool {
    w == "-"
        || w.strip_prefix(['r', 't', 'p']).is_some_and(|n| {
            !n.is_empty() && (n.bytes().all(|b| b.is_ascii_digit()) || placeholder(n))
        })
        || w == "<node>"
}

fn misnamed_log_words(span: &str) -> Vec<String> {
    use aterm_slog::{Key, Kind, Lvl};
    let out_word = |w: &str| aterm_slog::vocab::OUT_WORDS.contains(&w);
    let mut bad = Vec::new();
    for tok in span.split_whitespace() {
        if let Some(v) = tok.strip_prefix("out=") {
            let v = v.trim_end_matches([',', '.', ';', ')']);
            // A bare `out=` names the key, not a word.
            for alt in v.split('|').filter(|a| !a.is_empty() && !placeholder(a)) {
                if !out_word(alt) {
                    bad.push(format!("out={alt} in `{span}`"));
                }
            }
        }
    }
    let words: Vec<&str> = span.split_whitespace().collect();
    let start =
        usize::from(words.first().is_some_and(|w| {
            w.bytes().all(|b| b.is_ascii_digit()) || *w == "<seq>" || *w == "<n>"
        }));
    if let [op, node, kind, rest @ ..] = &words[start.min(words.len())..]
        && ["open", "close", "mark", "amend", "gap"].contains(op)
        && node_id(node)
    {
        if !placeholder(kind) && Kind::from_word(kind) == Kind::Unknown {
            bad.push(format!("kind {kind} in `{span}`"));
        }
        for kv in rest {
            let Some((k, _)) = kv.split_once('=') else {
                continue;
            };
            let header = ["par", "by", "src", "conf", "t"].contains(&k);
            if !header && !placeholder(k) && Key::from_word(k).is_none() {
                bad.push(format!("key {k}= in `{span}`"));
            }
        }
        return bad;
    }
    for part in span.split(" · ") {
        let w: Vec<&str> = part.split_whitespace().collect();
        match (w.first(), w.get(1)) {
            (Some(n), Some(next)) if n.starts_with('t') && node_id(n) && *n != "-" => {
                if *next != "open" && !out_word(next) && !placeholder(next) {
                    bad.push(format!("turn state {next} in `{span}`"));
                }
            }
            (Some(n), Some(next))
                if n.starts_with('p')
                    && node_id(n)
                    && Kind::from_word(next).node_level() != Some(Lvl::Step) =>
            {
                bad.push(format!("step kind {next} in `{span}`"));
            }
            _ => {}
        }
    }
    bad
}

#[test]
fn every_log_word_a_skill_names_is_the_logs() {
    let skills = [DRIVE_SKILL_BODY, SUPERVISE_SKILL_BODY];
    let mut examples = 0;
    for body in skills {
        for span in code_spans(body) {
            let bad = misnamed_log_words(&span);
            assert!(bad.is_empty(), "a skill names words the log lacks: {bad:?}");
            examples += usize::from(span.contains("out=") || span.contains(" · "));
        }
    }
    assert!(examples >= 3, "the skills show no log or brief example");
    for planted in [
        "`close p<n> approval out=finished by=ctl`",
        "`mark - finished by=ctl`",
        "`close t3 turn finished=1`",
        "`t17 finished 4m10s`",
        "`t17 open 4m · p204 finished bash`",
    ] {
        let text = format!("{DRIVE_SKILL_BODY}\nplanted: {planted}\n");
        let bad: Vec<String> = code_spans(&text)
            .iter()
            .flat_map(|s| misnamed_log_words(s))
            .collect();
        assert!(!bad.is_empty(), "{planted} passed");
    }
}

/// THE LOG'S VERBS' HELP NAMES THE LOG'S WORDS (6G-10): every code span
/// of the help for the verbs that read the session log or its views
/// (`log`, `brief`, `timeline`, `exits`, and `subscribe`, whose `events`
/// are the timeline's rows) names only the log's kinds, keys and outs.
#[test]
fn every_log_word_the_log_verbs_help_names_is_the_logs() {
    let help = |verb: &str| {
        aterm_types::control_verbs::spec(verb)
            .unwrap_or_else(|| panic!("no verb {verb}"))
            .help_line()
    };
    let misnamed = |text: &str| -> Vec<String> {
        code_spans(text)
            .iter()
            .flat_map(|s| misnamed_log_words(s))
            .collect()
    };
    let mut examples = 0;
    for verb in ["log", "brief", "timeline", "exits", "subscribe"] {
        let text = help(verb);
        let bad = misnamed(&text);
        assert!(
            bad.is_empty(),
            "help {verb} names words the log lacks: {bad:?}"
        );
        examples += code_spans(&text)
            .iter()
            .filter(|s| s.starts_with("mark ") || s.contains("out="))
            .count();
    }
    assert!(examples >= 3, "the help shows no log record");
    for planted in ["`mark - humann`", "`close t3 turn out=finished`"] {
        let text = format!("{} {planted}", help("timeline"));
        assert!(
            !misnamed(&text).is_empty(),
            "NEGATIVE CONTROL: {planted} passed"
        );
    }
}

/// THE BRIEF'S HELP NAMES ITS COUNT LINES AND WHAT IT LEAVES OUT BY ITS PRODUCER'S WORDS (fix
/// J1-2, ruling 3): `help brief` shows every count line as
/// `aterm_slog::brief::count_line_forms` prints it, and every word `more=` names a left-out
/// item by (`aterm_slog::brief::MORE_WORDS`) as a `<word>:<n>` span.
#[test]
fn the_brief_help_names_its_count_lines_and_every_more_word() {
    let help = aterm_types::control_verbs::spec("brief")
        .expect("the brief verb")
        .help_line();
    let unnamed = |help: &str, words: &[&str], forms: &[String]| -> Vec<String> {
        let spans = code_spans(help);
        words
            .iter()
            .map(|w| format!("{w}:<n>"))
            .chain(forms.iter().cloned())
            .filter(|item| !spans.contains(item))
            .collect()
    };
    let forms = aterm_slog::brief::count_line_forms();
    let gone = unnamed(&help, &aterm_slog::brief::MORE_WORDS, &forms);
    assert!(gone.is_empty(), "help brief leaves out {gone:?}: {help}");
    let mut words = aterm_slog::brief::MORE_WORDS.to_vec();
    words.push("stepz");
    let mut planted = forms.to_vec();
    planted.push("stepz <n>".to_string());
    assert_eq!(
        unnamed(&help, &words, &planted),
        vec!["stepz:<n>".to_string(), "stepz <n>".to_string()],
        "NEGATIVE CONTROL: a word or a count line the help lacks passed"
    );
}

const SETTINGS_WITH_ATERM_HOOKS: &str = r#"{
  "model": "opus",
  "env": {"SECRET_OF_THE_HUMAN": "never-touched"},
  "statusLine": {"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' statusline StatusLine usage-hud # aterm-harness", "refreshInterval": 60},
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "/opt/aterm/aterm link hook run session-start --state /s"}]}
    ],
    "Stop": [
      {"hooks": [{"type": "command", "command": "/opt/aterm/aterm-link hook run stop --report-to @s-1", "timeout": 600}]},
      {"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/their-own-stop-hook"}]},
      {"hooks": [{"type": "command", "command": "mytool hook run x"}]}
    ],
    "Notification": [
      {"hooks": [{"type": "command", "command": "'/Users//h/Library/Application Support/aterm/pkg/bin/aterm' link hook run notification"}]}
    ],
    "PermissionRequest": [
      {"hooks": [{"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' decide PermissionRequest rm-approve # aterm-harness"}]}
    ],
    "PreToolUse": [
      {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/local/bin/their-lint"}]},
      {"hooks": [{"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' decide PreToolUse rm-approve # aterm-harness"}]}
    ]
  },
  "permissions": {"allow": ["Bash(ls:*)"]}
}
"#;

#[test]
fn only_an_aterm_executable_running_the_hook_verb_or_the_harness_mark_is_aterms() {
    for ours in [
        "/Applications/aterm.app/Contents/MacOS/aterm link hook run stop --state /Users//h/.local/state/aterm-link --wake-budget 6/1 --timeout 15",
        "aterm link hook run notification",
        "/opt/aterm/aterm-link hook run stop --report-to @s-1",
        "'/Users//h/Library/Application Support/aterm/pkg/bin/aterm' link hook run stop",
        "'/tmp/it'\\''s/aterm-link' hook run stop",
        "/Users//h/Library/Application Support/aterm/pkg/bin/aterm link hook run stop --state /Users//h/.local/state/aterm-link",
        "/Users//h/My Tools/aterm hook run stop",
        "/Applications/aterm.app/Contents/MacOS/aterm-cli link hook run stop",
        "aterm-cli link hook run notification",
        "sh '/s/plugin/hooks/bridge.sh' statusline StatusLine usage-hud # aterm-harness  ",
    ] {
        assert!(is_aterm_hook_command(ours), "aterm's: {ours}");
    }
    for theirs in [
        "mytool hook run x",
        "/usr/local/bin/mytool link hook run stop",
        "/opt/aterm/aterm-foo link hook run stop",
        "/opt/aterm/aterm link hook runner",
        "/opt/aterm/aterm ctl ls",
        "echo aterm link hook run stop",
        "'/unterminated/aterm link hook run stop",
        "echo /Users//h/Library/Application Support/aterm/pkg/bin/aterm link hook run stop",
        "/usr/bin/tee /x | /Users//h/App Support/aterm link hook run stop",
        "/usr/bin/env FOO=$(/x/aterm link hook run stop)",
        "/Users//h/bin/my aterm link hook run stop",
        "Library/Application Support/aterm link hook run stop",
        "/Users//h/Application Support/aterm link hook runner",
        "sh x.sh # aterm-harness-but-not",
        "sh x.sh #aterm-harness",
        "# aterm-harness",
    ] {
        assert!(!is_aterm_hook_command(theirs), "the owner's: {theirs}");
    }
}

#[test]
fn the_top_effort_key_is_the_vendors_spelling() {
    assert_eq!(CLAUDE_TOP_EFFORT_KEY.as_bytes(), b"ultra\x63ode");
}

#[test]
fn claude_defaults_create_only_where_claude_code_is() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    assert_eq!(
        add_claude_defaults_in(home.path()),
        Ok(DefaultsWrite::Nothing)
    );
    assert!(!claude.exists());

    std::fs::create_dir_all(&claude).unwrap();
    assert_eq!(
        add_claude_defaults_in(home.path()),
        Ok(DefaultsWrite::Added {
            keys: CLAUDE_DEFAULTS.map(|(key, _)| key).to_vec(),
            backup: None,
        })
    );
    let text = std::fs::read_to_string(claude.join("settings.json")).unwrap();
    let v = settings_json::Json::parse(&text).unwrap();
    for (key, value) in CLAUDE_DEFAULTS {
        let got = match key.split_once('.') {
            Some((parent, child)) => v.get(parent).and_then(|o| o.get(child)),
            None => v.get(key),
        };
        assert_eq!(got, Some(&value.json()), "{key}: {text}");
    }
}

#[test]
fn claude_defaults_add_only_what_is_unset_and_not_held_back() {
    use settings_json::Json;
    const TOP: &str = CLAUDE_TOP_EFFORT_KEY;
    const RATE: &str = "feedbackSurveyRate";
    const SWITCH: &str = CLAUDE_SURVEY_SWITCH;
    const DRAFTS: &str = CLAUDE_DRAFTS_KEY;
    const WINDOW: &str = CLAUDE_COMPACT_WINDOW_KEY;
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let efforts = format!(r#""{TOP}": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0"#);
    let cases: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "every key set by hand, to any value",
            format!(
                r#"{{"{TOP}": false, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
                        "feedbackDrafts": null, "autoCompactWindow": 300000,
                        "env": {{"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "0"}}}}"#
            ),
            vec![],
        ),
        (
            "no env and no drafts key",
            format!(r#"{{"model": "m", {efforts}}}"#),
            vec![SWITCH, DRAFTS, WINDOW],
        ),
        (
            "an env of the person's own",
            format!(r#"{{{efforts}, "feedbackDrafts": "quiet", "env": {{"B": "2", "A": "1"}}}}"#),
            vec![SWITCH, WINDOW],
        ),
        (
            "a hand-set probability holds the switch back",
            r#"{"feedbackSurveyRate": 0.3}"#.to_string(),
            vec![TOP, "effortLevel", DRAFTS, WINDOW],
        ),
        (
            "a zero probability holds nothing back",
            r#"{"feedbackSurveyRate": 0.0}"#.to_string(),
            vec![TOP, "effortLevel", SWITCH, DRAFTS, WINDOW],
        ),
        (
            "a hand-set effort holds the effort defaults back",
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "medium"}}}"#.to_string(),
            vec![RATE, SWITCH, DRAFTS, WINDOW],
        ),
        (
            "a hand-set xhigh holds nothing back",
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "xhigh"}}}"#.to_string(),
            vec![TOP, "effortLevel", RATE, SWITCH, DRAFTS, WINDOW],
        ),
    ];
    let flat = |text: &str| -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (k, v) in Json::parse(text).unwrap().as_object().unwrap() {
            match (k.as_str(), v.as_object()) {
                ("env", Some(env)) => {
                    out.extend(env.iter().map(|(m, v)| (format!("env.{m}"), v.render())))
                }
                _ => out.push((k.clone(), v.render())),
            }
        }
        out
    };
    for (case, text, want) in cases {
        std::fs::write(&settings, &text).unwrap();
        let done = add_claude_defaults_in(home.path());
        let after = std::fs::read_to_string(&settings).unwrap();
        if want.is_empty() {
            assert_eq!(done, Ok(DefaultsWrite::Nothing), "{case}");
            assert_eq!(after, text, "{case}: not a byte touched");
            continue;
        }
        let Ok(DefaultsWrite::Added { keys, .. }) = done else {
            panic!("{case}: {done:?}");
        };
        assert_eq!(keys, want, "{case}");
        let (before, now) = (flat(&text), flat(&after));
        assert_eq!(now[..before.len()], before[..], "{case}: {after}");
    }

    let kept = backups(&claude);
    for bad in [
        "{ not json",
        "[]",
        r#"{"env": "FOO=1"}"#,
        r#"{"env": null}"#,
    ] {
        std::fs::write(&settings, bad).unwrap();
        let done = add_claude_defaults_in(home.path());
        assert!(done.is_err(), "{bad}: {done:?}");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), bad);
    }
    assert_eq!(backups(&claude), kept, "a refused file leaves no backup");
}

#[test]
fn compact_state_reads_are_bounded_before_defaults_are_chosen() {
    let home = aterm_tempfile::tempdir().unwrap();
    let state = home.path().join(".claude.json");
    assert_eq!(claude_compact_offers_kept_in(home.path()), Ok(Vec::new()));
    let kept = r#"{"autoCompactOffer":{"account":{"model":{"resolved":{"choice":"keep"}}}}}"#;
    std::fs::write(&state, kept).unwrap();
    assert_eq!(
        claude_compact_offers_kept_in(home.path()),
        Ok(vec!["model".to_string()])
    );
    std::fs::write(&state, [0xff]).unwrap();
    assert!(claude_compact_offers_kept_in(home.path()).is_err());
    let at_limit = format!("{}{{}}", " ".repeat(2 * 1024 * 1024 - 2));
    std::fs::write(&state, &at_limit).unwrap();
    assert_eq!(claude_compact_offers_kept_in(home.path()), Ok(Vec::new()));
    std::fs::write(&state, format!("{at_limit} ")).unwrap();
    assert!(claude_compact_offers_kept_in(home.path()).is_err());
}

#[cfg(unix)]
#[test]
fn compact_state_accepts_regular_symlinks_and_refuses_open_nonregular_files() {
    use std::os::fd::OwnedFd;
    use std::os::unix::{fs::symlink, net::UnixStream};
    let home = aterm_tempfile::tempdir().unwrap();
    let target = home.path().join("state-target");
    std::fs::write(&target, "{}").unwrap();
    symlink(&target, home.path().join(".claude.json")).unwrap();
    assert_eq!(claude_compact_offers_kept_in(home.path()), Ok(Vec::new()));
    let (stream, _peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    let file = std::fs::File::from(OwnedFd::from(stream));
    assert_eq!(
        read_compact_state(file).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn an_oversized_compact_state_holds_only_its_default_back() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir(&claude).unwrap();
    let settings = claude.join("settings.json");
    let state = home.path().join(".claude.json");
    std::fs::write(&state, "{}").unwrap();
    let DefaultsWrite::Added { keys, .. } = add_claude_defaults_in(home.path()).unwrap() else {
        panic!("the ordinary state must admit the defaults");
    };
    assert!(keys.contains(&CLAUDE_COMPACT_WINDOW_KEY));
    std::fs::write(&settings, "{}").unwrap();
    std::fs::write(&state, format!("{}{{}}", " ".repeat(2 * 1024 * 1024 - 1))).unwrap();
    let DefaultsWrite::Added { keys, .. } = add_claude_defaults_in(home.path()).unwrap() else {
        panic!("other unset defaults must still be added");
    };
    assert!(!keys.contains(&CLAUDE_COMPACT_WINDOW_KEY));
    let after = settings_json::Json::parse(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(after.get(CLAUDE_COMPACT_WINDOW_KEY).is_none());
    assert!(after.get(CLAUDE_TOP_EFFORT_KEY).is_some());
}

#[test]
fn claude_defaults_add_the_compact_window_only_where_unset_and_no_offer_was_kept() {
    const TOP: &str = CLAUDE_TOP_EFFORT_KEY;
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let rest = format!(
        "  \"{TOP}\": true,\n  \"effortLevel\": \"xhigh\",\n  \"feedbackSurveyRate\": 0,\n  \
         \"env\": {{\n    \"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY\": \"1\"\n  }},\n  \
         \"feedbackDrafts\": \"quiet\""
    );
    let run = |text: &str| {
        std::fs::write(&settings, text).unwrap();
        let done = add_claude_defaults_in(home.path());
        (done, std::fs::read_to_string(&settings).unwrap())
    };
    let added = |done: &Result<DefaultsWrite, String>| match done {
        Ok(DefaultsWrite::Added { keys, .. }) => keys.clone(),
        _ => Vec::new(),
    };

    let (done, after) = run(&format!("{{\n{rest}\n}}\n"));
    assert_eq!(added(&done), vec![CLAUDE_COMPACT_WINDOW_KEY]);
    assert_eq!(
        after,
        format!("{{\n{rest},\n  \"autoCompactWindow\": 400000\n}}\n")
    );

    for mine in ["300000", "1000000", "100000", "400000", "\"auto\""] {
        let text = format!("{{\n{rest},\n  \"autoCompactWindow\": {mine}\n}}\n");
        let (done, after) = run(&text);
        assert_eq!(done, Ok(DefaultsWrite::Nothing), "{mine}");
        assert_eq!(after, text, "{mine}");
    }

    for mine in ["400000", "1000000", "300000", "\"auto\""] {
        let model = format!(
            "  \"modelSettings\": {{\n    \"claude-opus-5-5\": {{\n      \
             \"autoCompactWindow\": {mine}\n    }}\n  }}"
        );
        let (done, after) = run(&format!("{{\n{model},\n{rest}\n}}\n"));
        assert_eq!(added(&done), vec![CLAUDE_COMPACT_WINDOW_KEY], "{mine}");
        assert_eq!(
            after,
            format!("{{\n{model},\n{rest},\n  \"autoCompactWindow\": 400000\n}}\n"),
            "{mine}"
        );
    }

    let state = home.path().join(".claude.json");
    let offer = |choice: &str| {
        format!(
            r#"{{"eligibleLoggedAt": 1, "resolved": {{"choice": "{choice}", "tokens": 1000000}}}}"#
        )
    };
    let kept = format!(
        r#"{{"numStartups": 3, "autoCompactOffer": {{
                "acct-a": {{"claude-opus-5-5": {keep}, "claude-sonnet-5": {accept}}},
                "acct-b": {{"claude-opus-5-5": {keep}, "claude-fable-5-1": {{"escCount": 1}},
                            "claude-haiku-5": 7}}}}}}"#,
        keep = offer("keep"),
        accept = offer("accept"),
    );
    assert_eq!(
        compact_offers_kept(&kept),
        Ok(vec!["claude-opus-5-5".to_string()])
    );
    let accepted = format!(
        r#"{{"autoCompactOffer": {{"acct-a": {{"claude-opus-5-5": {}}}}}}}"#,
        offer("accept")
    );
    for none in [accepted.as_str(), "{}", r#"{"autoCompactOffer": []}"#] {
        assert_eq!(compact_offers_kept(none), Ok(Vec::new()), "{none}");
    }
    assert!(compact_offers_kept("{").is_err());
    assert_eq!(
        claude_compact_offers_kept_in(home.path()),
        Ok(Vec::new()),
        "no file"
    );

    let unset = format!("{{\n{rest}\n}}\n");
    std::fs::write(&state, &kept).unwrap();
    let (done, after) = run(&unset);
    assert_eq!(done, Ok(DefaultsWrite::Nothing));
    assert_eq!(after, unset);
    let (done, _) = run("{}");
    assert_eq!(
        added(&done),
        vec![
            TOP,
            "effortLevel",
            "feedbackSurveyRate",
            CLAUDE_SURVEY_SWITCH,
            CLAUDE_DRAFTS_KEY
        ]
    );
    std::fs::write(&state, "{\"autoCompactOffer\": ").unwrap();
    assert_eq!(run(&unset), (Ok(DefaultsWrite::Nothing), unset.clone()));
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
    assert!(claude_compact_offers_kept_in(home.path()).is_err());
    assert_eq!(run(&unset), (Ok(DefaultsWrite::Nothing), unset.clone()));
    std::fs::remove_dir(&state).unwrap();
    std::fs::write(&state, &accepted).unwrap();
    let (done, _) = run(&unset);
    assert_eq!(added(&done), vec![CLAUDE_COMPACT_WINDOW_KEY]);
}

#[cfg(unix)]
#[test]
fn claude_defaults_never_create_over_a_dangling_settings_link() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let target = home.path().join("dotfiles").join("claude-settings.json");
    std::os::unix::fs::symlink(&target, &settings).unwrap();

    let refused = add_claude_defaults_in(home.path());
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(remove_aterm_hooks_in(home.path()), Ok(HookRemoval::Nothing));
    assert_eq!(
        std::fs::read_link(&settings).unwrap(),
        target,
        "the link is still that link"
    );
    assert!(!target.exists(), "nothing is created at the link's target");
    assert_eq!(
        std::fs::read_dir(&claude).unwrap().count(),
        1,
        "no backup and no temp file beside the link"
    );
}

#[cfg(unix)]
#[test]
fn a_settings_write_that_fails_leaves_no_backup_behind() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let store = home.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let real = store.join("settings.json");
    std::fs::write(&real, SETTINGS_WITH_ATERM_HOOKS).unwrap();
    std::os::unix::fs::symlink(&real, claude.join("settings.json")).unwrap();

    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
    let defaults = add_claude_defaults_in(home.path());
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(defaults.is_err(), "{defaults:?}");
    assert_eq!(backups(&claude), Vec::<String>::new());
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        SETTINGS_WITH_ATERM_HOOKS
    );
}

#[test]
fn a_save_between_read_and_write_of_the_settings_stands() {
    fn theirs(n: usize) -> String {
        format!("{{\"model\": \"opus\", \"theirs\": {n}}}\n")
    }
    fn save(settings: &Path, n: usize) {
        if n.is_multiple_of(2) {
            let swap = settings.with_extension("json.swp");
            std::fs::write(&swap, theirs(n)).unwrap();
            std::fs::rename(&swap, settings).unwrap();
        } else {
            std::fs::write(settings, theirs(n)).unwrap();
        }
    }
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    for saves in [REWRITE_TRIES - 1, REWRITE_TRIES] {
        std::fs::write(&settings, "{\"model\": \"opus\"}\n").unwrap();
        for b in backups(&claude) {
            std::fs::remove_file(claude.join(b)).unwrap();
        }
        let mut n = 0;
        let wrote = saving_after_read(
            move |path| {
                if n < saves {
                    save(path, n);
                }
                n += 1;
            },
            || add_claude_defaults(&settings, false),
        );
        let after = std::fs::read_to_string(&settings).unwrap();
        let last = theirs(saves - 1);
        if saves < REWRITE_TRIES {
            let Ok(DefaultsWrite::Added {
                backup: Some(backup),
                ..
            }) = wrote
            else {
                panic!("{wrote:?}")
            };
            let doc = settings_json::Json::parse(&after).unwrap();
            let theirs = settings_json::Json::Number((saves - 1).to_string());
            assert_eq!(doc.get("theirs"), Some(&theirs), "{after}");
            assert!(doc.get("effortLevel").is_some(), "{after}");
            assert_eq!(std::fs::read_to_string(&backup).unwrap(), last);
            assert_eq!(backups(&claude).len(), 1);
        } else {
            assert!(wrote.is_err(), "{wrote:?}");
            assert_eq!(after, last);
            assert_eq!(backups(&claude), Vec::<String>::new());
        }
    }
}

#[test]
fn the_pass_removes_the_hook_entries_aterm_wrote_and_leaves_the_foreign_ones() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    std::fs::write(&settings, SETTINGS_WITH_ATERM_HOOKS).unwrap();

    let pass = auto_prime(home.path());
    let after = std::fs::read_to_string(&settings).unwrap();
    for gone in [
        "link hook run",
        "aterm-link",
        "aterm-harness",
        "statusLine",
        "SessionStart",
        "Notification",
        "PermissionRequest",
    ] {
        assert!(!after.contains(gone), "`{gone}` is gone: {after}");
    }
    for kept in [
        "/usr/local/bin/their-own-stop-hook",
        "/usr/local/bin/their-lint",
        "\"mytool hook run x\"",
        "\"model\": \"opus\"",
        "never-touched",
        "Bash(ls:*)",
    ] {
        assert!(after.contains(kept), "{kept} stays: {after}");
    }
    let at = |k: &str| after.find(&format!("\"{k}\"")).unwrap();
    assert!(
        at("model") < at("env")
            && at("env") < at("hooks")
            && at("hooks") < at("permissions")
            && at("Stop") < at("PreToolUse"),
        "the order the owner wrote stays: {after}"
    );
    let first = backups(&claude);
    assert_eq!(first.len(), 1, "one backup beside the file: {first:?}");
    assert_eq!(
        std::fs::read_to_string(claude.join(&first[0])).unwrap(),
        SETTINGS_WITH_ATERM_HOOKS,
        "the backup is the previous bytes"
    );
    assert!(
        pass.agents[0]
            .wrote
            .iter()
            .any(|w| w.starts_with("~/.claude/settings.json ")),
        "{:?}",
        pass.agents[0].wrote
    );

    let again = auto_prime(home.path());
    assert!(
        again.agents[0]
            .wrote
            .iter()
            .all(|w| !w.contains("settings.json")),
        "{:?}",
        again.agents[0].wrote
    );
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), after);
    assert_eq!(backups(&claude), first);
}

#[cfg(unix)]
#[test]
fn the_settings_backup_keeps_the_settings_files_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = aterm_tempfile::tempdir().unwrap();
    let settings = home.path().join("settings.json");
    std::fs::write(&settings, "{}\n").unwrap();
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o640)).unwrap();
    let mode_of = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let recording = aterm_durable::seam::Recording::start();
    let first = write_backup(&settings, b"{}\n").unwrap();
    let second = write_backup(&settings, b"{}\n").unwrap();
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered],
        "each backup is only ordered ahead of the rewrite"
    );
    assert_ne!(first, second);
    assert_eq!((mode_of(&first), mode_of(&second)), (0o640, 0o640));
}

#[test]
fn a_backup_whose_bytes_fail_to_go_down_is_not_left_behind() {
    let home = aterm_tempfile::tempdir().unwrap();
    let settings = home.path().join("settings.json");
    std::fs::write(&settings, SETTINGS_WITH_ATERM_HOOKS).unwrap();
    let failed = write_backup_with(&settings, |file| {
        file.write_all(&SETTINGS_WITH_ATERM_HOOKS.as_bytes()[..16])?;
        Err(std::io::Error::other("injected: no space left on device"))
    });
    assert!(failed.is_err(), "{failed:?}");
    assert_eq!(backups(home.path()), Vec::<String>::new());
}

#[test]
fn the_harness_statusline_is_given_back_from_the_installers_byte_copy() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    std::fs::write(&settings, SETTINGS_WITH_ATERM_HOOKS).unwrap();
    std::fs::write(
        claude.join("settings.json.aterm-harness.orig"),
        r#"{"model": "opus", "statusLine": {"type": "command", "command": "~/bin/my-footer", "padding": 0}}"#,
    )
    .unwrap();
    remove_aterm_hooks(&settings).unwrap();
    let after = std::fs::read_to_string(&settings).unwrap();
    assert!(after.contains("~/bin/my-footer"), "{after}");
    assert!(!after.contains("aterm-harness"), "{after}");
    let at = |k: &str| after.find(&format!("\"{k}\"")).unwrap();
    assert!(
        at("env") < at("statusLine") && at("statusLine") < at("hooks"),
        "the restored value keeps the slot's position: {after}"
    );
}

#[test]
fn the_statusline_install_displaced_wins_over_a_stale_byte_copy() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let state = home.path().join("harness state/claude-harness");
    std::fs::create_dir_all(state.join("plugin/hooks")).unwrap();
    let settings = claude.join("settings.json");
    let fixture = SETTINGS_WITH_ATERM_HOOKS.replace(
        "/s/harness/claude-harness",
        &state.display().to_string().replace('\\', "\\\\"),
    );
    let orig = claude.join("settings.json.aterm-harness.orig");
    let stale =
        r#"{"statusLine": {"type": "command", "command": "~/bin/old-footer", "padding": 2}}"#;
    let user = state.join("statusline.user");
    let restore = || {
        remove_aterm_hooks(&settings).unwrap();
        let after = std::fs::read_to_string(&settings).unwrap();
        settings_json::Json::parse(&after)
            .unwrap()
            .get("statusLine")
            .cloned()
    };

    std::fs::write(&settings, &fixture).unwrap();
    std::fs::write(&orig, stale).unwrap();
    std::fs::write(&user, "~/bin/new-footer --tz \"UTC\"\n").unwrap();
    let line = restore().expect("restored");
    assert_eq!(
        line.get("command").and_then(settings_json::Json::as_str),
        Some("~/bin/new-footer --tz \"UTC\"")
    );
    assert!(line.get("padding").is_none(), "{line:?}");

    std::fs::write(&settings, &fixture).unwrap();
    std::fs::write(&user, "~/bin/old-footer\n").unwrap();
    let line = restore().expect("restored");
    assert!(line.get("padding").is_some(), "{line:?}");

    std::fs::write(&settings, &fixture).unwrap();
    std::fs::remove_file(&user).unwrap();
    assert_eq!(restore(), None);

    std::fs::write(&settings, &fixture).unwrap();
    std::fs::write(&user, "sh '/s/bridge.sh' # aterm-harness\n").unwrap();
    assert_eq!(restore(), None);
    assert_eq!(
        remove_aterm_hooks(&settings),
        Ok(HookRemoval::Nothing),
        "a harness-marked statusline.user is not restored, so the second pass is idempotent"
    );
}

#[test]
fn strip_marked_hooks_keeps_pre_existing_empty_groups_and_events() {
    let home = aterm_tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    std::fs::write(
        &settings,
        r#"{
  "statusLine": {"type": "command", "command": "sh '/s/bridge.sh' # aterm-harness"},
  "hooks": {
    "Stop": [
      {"hooks": [{"type": "command", "command": "aterm link hook run stop"}]},
      {"matcher": "keep-empty-group", "hooks": []}
    ],
    "UserPromptSubmit": []
  }
}
"#,
    )
    .unwrap();
    let removed = remove_aterm_hooks(&settings).unwrap();
    assert!(matches!(removed, HookRemoval::Removed { removed: 2, .. }));
    let after = std::fs::read_to_string(&settings).unwrap();
    assert!(!after.contains("aterm link hook run"), "{after}");
    assert!(!after.contains("statusLine"), "{after}");
    assert!(after.contains("keep-empty-group"), "{after}");
    assert!(after.contains("\"UserPromptSubmit\": []"), "{after}");

    std::fs::write(
        &settings,
        r#"{"statusLine": {"type": "command", "command": "sh '/s/bridge.sh' # aterm-harness"}, "hooks": {}}"#,
    )
    .unwrap();
    remove_aterm_hooks(&settings).unwrap();
    let after_empty_top = std::fs::read_to_string(&settings).unwrap();
    assert!(
        after_empty_top.contains("\"hooks\": {}"),
        "{after_empty_top}"
    );
    assert_eq!(remove_aterm_hooks(&settings), Ok(HookRemoval::Nothing));
}

/// `read_skill` and `restore_harness_statusline` refuse non-regular files (such as `/dev/null`
/// or FIFOs) through `AsRead::open` rather than performing unbounded blocking `std::fs::read`.
/// NEGATIVE CONTROL: calling `std::fs::read(Path::new("/dev/null"))` in `read_skill` classifies
/// `/dev/null` as `SkillState::Foreign` instead of returning an `"unreadable: ..."` error.
#[cfg(unix)]
#[test]
fn skill_and_statusline_readers_refuse_non_regular_files() {
    let err = read_skill(Path::new("/dev/null"), "body").expect_err("device file refused");
    assert!(err.contains("not a regular file"), "{err}");
    assert!(read_bounded_bytes(Path::new("/dev/null")).is_err());
}
