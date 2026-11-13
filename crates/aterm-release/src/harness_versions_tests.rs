// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::testing::{Git, git, init};

const THREE: &str = "\
//! A reader.

/// Major component.
pub const HARNESS_VERSION_MAJOR: u16 = 0;

/// Minor component.
pub const HARNESS_VERSION_MINOR: u16 = 1;

/// Text.
pub const HARNESS_VERSION: &str = \"0.1\";

/// Structured.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(
    HARNESS_VERSION_MAJOR,
    HARNESS_VERSION_MINOR,
    HARNESS_VERSION,
);

fn read() {}
";

fn one(minor: u16) -> String {
    format!(
        "//! A reader.\n\n/// The version.\n{}\n\nfn read() {{}}\n",
        version_line((0, minor))
    )
}

#[test]
fn both_shapes_declare_their_version_and_a_third_part_is_none() {
    assert_eq!(declared(THREE), Some((0, 1)));
    assert_eq!(declared(&one(4)), Some((0, 4)));
    assert_eq!(declared(&one(12)), Some((0, 12)));
    assert_eq!(declared("fn read() {}"), None);
    let lying = one(4).replace("\"0.4\"", "\"0.5\"");
    assert_eq!(declared(&lying), None, "the text must be the numbers");
    let third = one(4).replace("\"0.4\"", "\"0.4.0\"");
    assert_eq!(declared(&third), None, "never a third part");
    assert_eq!(declared(&one(4).replace("0, 4,", "+0, 4,")), None);
    assert_eq!(
        declared(&THREE.replace("MAJOR: u16 = 0;", "MAJOR: u16 = +0;")),
        None
    );
    for harness in CliHarness::ALL {
        let real = std::fs::read_to_string(
            aterm_tempfile::manifest_dir()
                .join("../..")
                .join(harness.version_file()),
        )
        .unwrap();
        assert_eq!(
            declared(&real),
            Some(harness.version().tuple()),
            "{harness:?}: the gate reads what the build compiles"
        );
    }
}

#[test]
fn a_rewrite_collapses_the_old_shape_into_one_line() {
    let new = rewritten(THREE, (0, 2)).unwrap();
    assert_eq!(declared(&new), Some((0, 2)));
    assert!(!new.contains("HARNESS_VERSION_MAJOR"), "{new}");
    assert!(!new.contains("HARNESS_VERSION:"), "{new}");
    assert!(new.contains(&version_line((0, 2))), "{new}");
    assert!(new.ends_with("fn read() {}\n"), "{new}");
    let again = rewritten(&one(4), (0, 5)).unwrap();
    assert_eq!(again, one(5));
}

#[test]
fn only_a_moved_harness_under_an_unchanged_version_owes() {
    let texts = |minor: u16| move |_: CliHarness| Ok::<_, Error>(one(minor));
    let before = |minor: u16| move |_: CliHarness| Ok::<_, Error>(Some(one(minor)));
    let none = owed(
        &[
            "README.md".into(),
            "crates/aterm-agent/src/harness/footer.rs".into(),
            "crates/aterm-agent/src/harness/codex_alab.rs".into(),
        ],
        before(4),
        texts(4),
    )
    .unwrap();
    assert!(
        none.is_empty(),
        "no CLI harness source moved; shared infrastructure does not lockstep-bump harnesses"
    );
    for (harness, sample) in [
        (
            CliHarness::ClaudeCode,
            "crates/aterm-agent/src/harness/cli_shared_claude.rs",
        ),
        (
            CliHarness::Agy,
            "crates/aterm-agent/src/harness/upgrade_agy.rs",
        ),
        (
            CliHarness::Grok,
            "crates/aterm-agent/src/harness/grok_signin.rs",
        ),
        (
            CliHarness::Kimi,
            "crates/aterm-agent/src/harness/cli_shared_kimi.rs",
        ),
        (
            CliHarness::Muse,
            "crates/aterm-agent/src/harness/upgrade_muse.rs",
        ),
    ] {
        let single = owed(&[sample.into()], before(4), texts(4)).unwrap();
        assert_eq!(
            single.iter().map(|o| o.harness).collect::<Vec<_>>(),
            vec![harness],
            "{sample} moves only {harness:?}"
        );
        assert_eq!((single[0].at, single[0].to), ((0, 4), (0, 5)));
    }
    let reader = owed(
        &["crates/aterm-phase/src/muse.rs".into()],
        before(4),
        texts(4),
    )
    .unwrap();
    assert_eq!(reader.len(), 1);
    assert_eq!(reader[0].harness, CliHarness::Muse);
    assert!(reader[0].line().contains("crates/aterm-phase/src/muse.rs"));
    assert!(reader[0].line().contains(&version_line((0, 5))));
    let bumped = owed(
        &[
            "crates/aterm-phase/src/muse.rs".into(),
            "crates/aterm-agent/src/harness/upgrade_muse.rs".into(),
        ],
        before(4),
        texts(5),
    )
    .unwrap();
    assert!(
        bumped.is_empty(),
        "a window's one bump covers every change in it"
    );
    let born = owed(
        &["crates/aterm-phase/src/kimi.rs".into()],
        |_| Ok(None),
        texts(1),
    )
    .unwrap();
    assert!(born.is_empty(), "a harness new in the window owes nothing");
}

/// A scratch history: a release tag, then changes to two harnesses (`claude` and `muse`) and a
/// non-harness file (`footer.rs`), leaving `agy`, `grok` and `kimi` untouched.
fn history(dir: &Path, bump_too: bool) -> (String, String) {
    init(dir, &[("user.name", "t"), ("user.email", "t@example.com")]);
    let run = |args: &[&str]| {
        let out = git().arg("-C").arg(dir).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    for h in CliHarness::ALL {
        let path = dir.join(h.version_file());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            if h == CliHarness::Muse {
                THREE.to_string()
            } else {
                one(1)
            },
        )
        .unwrap();
    }
    let footer = dir.join("crates/aterm-agent/src/harness/footer.rs");
    let claude_src = dir.join("crates/aterm-agent/src/harness/cli_shared_claude.rs");
    let muse_src = dir.join("crates/aterm-agent/src/harness/upgrade_muse.rs");
    std::fs::create_dir_all(footer.parent().unwrap()).unwrap();
    std::fs::write(&footer, "fn a() {}\n").unwrap();
    std::fs::write(&claude_src, "fn c() {}\n").unwrap();
    std::fs::write(&muse_src, "fn m() {}\n").unwrap();
    std::fs::write(
        dir.join("CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - x\n",
    )
    .unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-qm", "v0.1.0"]);
    let released = run(&["rev-parse", "HEAD"]);
    std::fs::write(&footer, "fn a() { b() }\n").unwrap();
    std::fs::write(&claude_src, "fn c() { b() }\n").unwrap();
    std::fs::write(&muse_src, "fn m() { b() }\n").unwrap();
    if bump_too {
        for h in [CliHarness::ClaudeCode, CliHarness::Muse] {
            std::fs::write(dir.join(h.version_file()), one(2)).unwrap();
        }
    }
    run(&["commit", "-qam", "lane"]);
    (released, run(&["rev-parse", "HEAD"]))
}

#[test]
fn the_gate_refuses_a_moved_lane_and_passes_its_bump() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let (released, head) = history(tmp.path(), false);
    let git = Git::new(tmp.path());
    let refused = gate(&git, ("0.1.0", &released), &head)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("since v0.1.0"), "{refused}");
    for h in [CliHarness::ClaudeCode, CliHarness::Muse] {
        assert!(refused.contains(h.version_file()), "{h:?}: {refused}");
    }
    for h in [CliHarness::Agy, CliHarness::Grok, CliHarness::Kimi] {
        assert!(
            !refused.contains(h.version_file()),
            "{h:?} did not move: {refused}"
        );
    }
    assert!(refused.contains(&version_line((0, 2))), "{refused}");
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let (released, head) = history(tmp.path(), true);
    let git = Git::new(tmp.path());
    let passed = gate(&git, ("0.1.0", &released), &head).unwrap();
    assert!(
        passed.contains("claude 0.2")
            && passed.contains("muse 0.2")
            && passed.contains("agy 0.1")
            && passed.contains("grok 0.1")
            && passed.contains("kimi 0.1"),
        "{passed}"
    );
}

/// T-R10. `SHIP HARNESS-BUMP WRITES THE VERSION FILES AND NOTHING ELSE`: no changelog line and
/// no entry file; the release derives the announcement from the version files, naming exactly
/// the raised harnesses in canonical order, and a second run owes and writes nothing.
/// NEGATIVE CONTROL: before the bump commit nothing rose, so nothing is announced; a harness
/// that moves later in the same window joins the one derived line.
#[test]
fn harness_bump_writes_what_the_gate_asks_for() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let (released, head) = history(tmp.path(), false);
    let log_before = "## [Unreleased]\n\n### Changed\n\n- A note.\n\n## [0.1.0] - x\n";
    std::fs::write(tmp.path().join("CHANGELOG.md"), log_before).unwrap();
    let git = Git::new(tmp.path());
    assert_eq!(
        announcement(&git, Some(("0.1.0", &released)), &head).unwrap(),
        None,
        "nothing rose before the bump"
    );
    let owed = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
    assert_eq!(
        owed.iter().map(|o| o.harness).collect::<Vec<_>>(),
        vec![CliHarness::ClaudeCode, CliHarness::Muse]
    );
    for h in [CliHarness::ClaudeCode, CliHarness::Muse] {
        let text = std::fs::read_to_string(tmp.path().join(h.version_file())).unwrap();
        assert_eq!(declared(&text), Some((0, 2)), "{h:?}");
        assert!(text.contains(&version_line((0, 2))), "{h:?}: one line");
    }
    for h in [CliHarness::Agy, CliHarness::Grok, CliHarness::Kimi] {
        let text = std::fs::read_to_string(tmp.path().join(h.version_file())).unwrap();
        assert_eq!(declared(&text), Some((0, 1)), "{h:?} stays at 0.1");
    }
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("CHANGELOG.md")).unwrap(),
        log_before,
        "the bump writes no changelog line"
    );
    assert!(
        !tmp.path().join(crate::changelog::FRAGMENT_DIR).exists(),
        "the bump writes no entry file"
    );
    assert_eq!(
        written(&owed),
        vec![
            CliHarness::ClaudeCode.version_file().to_string(),
            CliHarness::Muse.version_file().to_string(),
        ],
        "the files a bump commit carries"
    );
    let again = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
    assert!(again.is_empty(), "a second run owes nothing");
    assert!(written(&again).is_empty());
    let run = |args: &[&str]| {
        let out = git_cmd(tmp.path(), args);
        assert!(out.status.success(), "{args:?}");
    };
    run(&["commit", "-qam", "bump"]);
    let head = crate::ledger::rev_parse(&git, "HEAD").unwrap();
    assert!(gate(&git, ("0.1.0", &released), &head).is_ok());
    let line = announcement(&git, Some(("0.1.0", &released)), &head)
        .unwrap()
        .unwrap();
    assert_eq!(
        line,
        format!(
            "{}0.1.0: `alab claude harness 0.2`, `alab muse harness 0.2`.** Each harness's \
             minor moves once per release in which it changed.",
            crate::changelog::HARNESS_ENTRY_HEAD
        )
    );
    assert!(
        !line.contains("codex harness"),
        "Codex versions its own binary"
    );
    notes_gate(&format!("## [0.2.0] - d\n\n{line}\n"), "0.2.0").unwrap();
    assert_eq!(
        announcement(&git, None, &head).unwrap(),
        None,
        "no release precedes"
    );

    let agy_src = tmp
        .path()
        .join("crates/aterm-agent/src/harness/upgrade_agy.rs");
    std::fs::write(&agy_src, "fn agy() {}\n").unwrap();
    run(&["add", "crates/aterm-agent/src/harness/upgrade_agy.rs"]);
    let third = bump(tmp.path(), &git, ("0.1.0", &released)).unwrap();
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].harness, CliHarness::Agy);
    run(&["commit", "-qam", "agy moves later in the window"]);
    let head = crate::ledger::rev_parse(&git, "HEAD").unwrap();
    let line = announcement(&git, Some(("0.1.0", &released)), &head)
        .unwrap()
        .unwrap();
    assert_eq!(
        line.matches(crate::changelog::HARNESS_ENTRY_HEAD).count(),
        1
    );
    assert!(
        line.contains(
            "0.1.0: `alab claude harness 0.2`, `alab agy harness 0.2`, `alab muse harness 0.2`.**"
        ),
        "one line, every raised harness in canonical order: {line}"
    );
}

#[test]
fn announcement_matches_versions_in_release_objects() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let (released, _) = history(tmp.path(), true);
    let run = |args: &[&str]| {
        let out = git_cmd(tmp.path(), args);
        assert!(out.status.success(), "{args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let git = Git::new(tmp.path());
    let path = tmp.path().join("CHANGELOG.md");
    for entry in [
        "- Other release work.",
        "- **Harness versions since v0.1.0: `alab claude harness 0.2`.**",
        "- **Harness versions since v0.1.0: `alab claude harness 0.2`, `alab muse harness 0.2`, `alab agy harness 0.1`.**",
        "- **Harness versions since v0.1.0: `alab claude harness 0.9`, `alab muse harness 0.2`.**",
    ] {
        // A recut ships its rolled section as it stands, so its announcement is judged.
        std::fs::write(
            &path,
            format!("## [Unreleased]\n\n## [0.2.0] - d\n\n{entry}\n\n## [0.1.0]\n"),
        )
        .unwrap();
        run(&["commit", "-qam", "candidate notes"]);
        assert!(gate(&git, ("0.1.0", &released), "HEAD").is_ok());
        // Syntax alone cannot discover these planted version defects.
        notes_gate(&format!("## [0.2.0] - d\n\n{entry}\n"), "0.2.0").unwrap();
        assert!(
            notes_gate_at(&git, ("0.1.0", &released), "HEAD", "0.2.0").is_err(),
            "accepted {entry}"
        );
        // A fresh window's hand-written line is no announcement: the roll drops it.
        std::fs::write(&path, format!("## [Unreleased]\n\n{entry}\n\n## [0.1.0]\n")).unwrap();
        run(&["commit", "-qam", "unreleased notes"]);
        notes_gate_at(&git, ("0.1.0", &released), "HEAD", "0.2.0").unwrap();
    }
    // NEGATIVE CONTROL: the derived announcement, exactly the versions that moved, passes,
    // even though the preceding release's prose did not announce its shipped versions.
    let derived = announcement(&git, Some(("0.1.0", &released)), "HEAD")
        .unwrap()
        .unwrap();
    std::fs::write(
        &path,
        format!("## [Unreleased]\n\n## [0.2.0] - d\n\n{derived}\n\n## [0.1.0]\n"),
    )
    .unwrap();
    run(&["commit", "-qam", "derived notes"]);
    notes_gate_at(&git, ("0.1.0", &released), "HEAD", "0.2.0").unwrap();
}

/// Real v0.191.0 announces Claude 0.29 since v0.189.0; intervening
/// v0.190.0 still declared 0.28. The wider, accurate window is not stale.
#[test]
fn an_unchanged_intervening_release_does_not_stale_an_announcement() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let (released, _) = history(tmp.path(), true);
    let run = |args: &[&str]| {
        let out = git_cmd(tmp.path(), args);
        assert!(out.status.success(), "{args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    run(&["tag", "v0.1.0", &released]);
    run(&["checkout", "-qb", "intervening", &released]);
    std::fs::write(tmp.path().join("unrelated"), "unrelated release change").unwrap();
    run(&["add", "unrelated"]);
    run(&[
        "commit",
        "-qm",
        "intervening public release leaves harness versions alone",
    ]);
    let public = run(&["rev-parse", "HEAD"]);
    run(&["tag", "v0.2.0"]);
    for h in [CliHarness::ClaudeCode, CliHarness::Muse] {
        std::fs::write(tmp.path().join(h.version_file()), one(2)).unwrap();
    }
    let git = Git::new(tmp.path());
    for announcement in [
        "- **Harness versions since v0.1.0: `alab claude harness 0.2`, `alab muse harness 0.2`.**",
        "- `alab claude harness 0.2` and `alab muse harness 0.2` record the changes.",
    ] {
        std::fs::write(
            tmp.path().join("CHANGELOG.md"),
            format!(
                "## [Unreleased]\n\n## [0.3.0] - d\n\n{announcement}\n\n## [0.2.0]\n\n\
                 - Unrelated public change.\n"
            ),
        )
        .unwrap();
        run(&["commit", "-qam", "accurate historical announcement"]);
        notes_gate_at(&git, ("0.2.0", &public), "HEAD", "0.3.0").unwrap();
    }
    // NEGATIVE CONTROL: the same history still owes both actual increases.
    std::fs::write(
        tmp.path().join("CHANGELOG.md"),
        "## [Unreleased]\n\n## [0.3.0] - d\n\n\
         - **Harness versions since v0.1.0: `alab claude harness 0.2`.**\n",
    )
    .unwrap();
    run(&["commit", "-qam", "missing muse announcement"]);
    assert!(
        notes_gate_at(&git, ("0.2.0", &public), "HEAD", "0.3.0")
            .unwrap_err()
            .to_string()
            .contains("require")
    );

    // Real 5013ffcb59e / 8c367204137 / abe0ab58db7 / ad235556523:
    // an older label must not require re-announcing an intervening release's
    // already-shipped bump. The actual public predecessor still governs this cut.
    std::fs::write(
        tmp.path().join(CliHarness::ClaudeCode.version_file()),
        one(1),
    )
    .unwrap();
    run(&["commit", "-qam", "next public release ships muse only"]);
    let muse_public = run(&["rev-parse", "HEAD"]);
    std::fs::write(
        tmp.path().join(CliHarness::ClaudeCode.version_file()),
        one(2),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("CHANGELOG.md"),
        "## [Unreleased]\n\n## [0.4.0] - d\n\n\
         - **Harness versions since v0.1.0: `alab claude harness 0.2`.**\n",
    )
    .unwrap();
    run(&["commit", "-qam", "claude changes after muse shipped"]);
    notes_gate_at(&git, ("0.3.0", &muse_public), "HEAD", "0.4.0").unwrap();
    // NEGATIVE CONTROL: repeating that shipped bump in the new announcement
    // is still wrong; it must contain exactly this window's actual increases.
    std::fs::write(
        tmp.path().join("CHANGELOG.md"),
        "## [Unreleased]\n\n## [0.4.0] - d\n\n- **Harness versions since v0.1.0: `alab claude harness 0.2`, `alab muse harness 0.2`.**\n",
    ).unwrap();
    run(&[
        "commit",
        "-qam",
        "reannounce the already-shipped muse version",
    ]);
    assert!(notes_gate_at(&git, ("0.3.0", &muse_public), "HEAD", "0.4.0").is_err());
}

#[test]
#[cfg(unix)]
fn applying_a_bump_uses_native_release_objects_and_stops_before_untrusted_writes() {
    struct LocalOnly<'a>(&'a dyn GitRunner);
    impl GitRunner for LocalOnly<'_> {
        fn git(&self, args: &[&str]) -> Result<crate::ledger::RunOut> {
            assert!(
                !args
                    .iter()
                    .any(|arg| matches!(*arg, "fetch" | "push" | "ls-remote")),
                "harness application must use its typed origin: {args:?}"
            );
            self.0.git(args)
        }
    }
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let source = tmp.path().join("source");
    let bare = tmp.path().join("remote.git");
    let (released, _) = history(&source, false);
    let command = |args: &[&str]| {
        let out = git_cmd(&source, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    command(&["tag", "-a", "v0.1.0", "-m", "release", &released]);
    command(&["tag", "-a", "v0.3.0", "-m", "future release", &released]);
    command(&["init", "-q", "--bare", bare.to_str().unwrap()]);
    command(&["push", "-q", bare.to_str().unwrap(), "refs/tags/v0.1.0"]);
    command(&["push", "-q", bare.to_str().unwrap(), "refs/tags/v0.3.0"]);
    command(&[
        "config",
        "remote.origin.url",
        "https://github.com/fixture/app.git",
    ]);
    std::fs::write(source.join("Cargo.toml"), "[workspace.package]\nversion = \"0.2.0\"\nrepository = \"https://github.com/fixture/app\"\n").unwrap();
    // The real append-only ledger retains pre-three-component labels,
    // repeated claims and future windows. They are not comparison floors.
    let ledger = "1 0.61\n2 0.1.0\n3 0.1.0\n4 0.2.0\n5 0.3.0\n6 1.20261109.1\n";
    std::fs::write(source.join(crate::ledger::LEDGER_FILE), ledger).unwrap();
    let origin = crate::github_remote::claim_fixture(&source, &bare);
    assert_eq!(
        origin.get("refs/heads/main").unwrap(),
        None,
        "only the public release tag is needed"
    );
    let local = Git::new(&source);
    let (release, owed) = apply_with(&source, &LocalOnly(&local), &origin)
        .unwrap()
        .unwrap();
    assert_eq!(release, "0.1.0");
    assert_eq!(
        std::fs::read_to_string(source.join(crate::ledger::LEDGER_FILE)).unwrap(),
        ledger,
        "selecting a public baseline never rewrites historical release identity"
    );
    assert_eq!(
        owed.iter().map(|row| row.harness).collect::<Vec<_>>(),
        [CliHarness::ClaudeCode, CliHarness::Muse]
    );
    for harness in [CliHarness::ClaudeCode, CliHarness::Muse] {
        assert_eq!(
            declared(&std::fs::read_to_string(source.join(harness.version_file())).unwrap()),
            Some((0, 2))
        );
    }
    assert!(
        apply_with(&source, &LocalOnly(&local), &origin)
            .unwrap()
            .unwrap()
            .1
            .is_empty(),
        "one release window never bumps twice"
    );
    let paths = [
        CliHarness::ClaudeCode.version_file(),
        CliHarness::Muse.version_file(),
        "CHANGELOG.md",
    ];
    let before = paths.map(|path| std::fs::read(source.join(path)).unwrap());
    command(&[
        "config",
        "remote.origin.url",
        "https://github.com/foreign/app.git",
    ]);
    assert!(apply_with(&source, &LocalOnly(&local), &origin).is_err());
    assert_eq!(
        before,
        paths.map(|path| std::fs::read(source.join(path)).unwrap())
    );
    command(&[
        "config",
        "remote.origin.url",
        "https://github.com/fixture/app.git",
    ]);
    let tree = command(&["rev-parse", &format!("{released}^{{tree}}")]);
    command(&["tag", "-a", "noncommit", "-m", "not a release", &tree]);
    command(&["push", "-q", bare.to_str().unwrap(), "refs/tags/noncommit"]);
    let token = command(&["rev-parse", "refs/tags/noncommit"]);
    let out = git()
        .arg("--git-dir")
        .arg(&bare)
        .args(["update-ref", "refs/tags/v0.1.0", &token])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        crate::gates::tag_commits(&origin.tag_catalog().unwrap()).get("v0.1.0"),
        Some(&tree)
    );
    let error = apply_with(&source, &LocalOnly(&local), &origin).unwrap_err();
    assert!(
        error.to_string().contains("not an imported commit"),
        "{error}"
    );
    assert_eq!(
        before,
        paths.map(|path| std::fs::read(source.join(path)).unwrap()),
        "a noncommit baseline cannot write harness versions or notes"
    );
}

/// A LEDGER NONE OF WHOSE RELEASES SHIPPED IS REFUSED BY THIS GATE, BY NAME: its error says
/// `harness versions:`, never another gate's name. CONTROL: a tag for the newest earlier
/// release is the window's start.
#[test]
fn a_ledger_none_of_whose_releases_shipped_is_refused_by_this_gate() {
    let ledger = "# seed\n1 0.1.0\n2 0.2.0\n";
    let refused = previous_release(ledger, "0.3.0", "")
        .unwrap_err()
        .to_string();
    let unshipped = crate::gates::predecessor_release(ledger, "0.3.0", &|_| false)
        .expect_err("no earlier version shipped")
        .to_string();
    assert_eq!(refused, format!("harness versions: {unshipped}"));
    assert_eq!(
        previous_release(ledger, "0.3.0", "abc123\trefs/tags/v0.2.0\n").unwrap(),
        Some(("0.2.0".to_owned(), "abc123".to_owned())),
        "CONTROL"
    );
}

#[test]
fn bump_uses_the_latest_shipped_release_before_and_after_the_version_bump() {
    let ledger = "# seed\n1 0.1.0\n2 0.2.0\n3 0.3.0\n";
    let tags = "abc123\trefs/tags/v0.1.0\ndef456\trefs/tags/v0.2.0\n";
    let expected = Some(("0.2.0".to_owned(), "def456".to_owned()));
    assert_eq!(bump_base_release(ledger, "0.2.0", tags).unwrap(), expected);
    assert_eq!(bump_base_release(ledger, "0.3.0", tags).unwrap(), expected);
    assert_eq!(
        previous_release(ledger, "0.2.0", tags).unwrap(),
        Some(("0.1.0".to_owned(), "abc123".to_owned())),
        "the release gate still compares a candidate against its predecessor"
    );
}

#[test]
fn the_last_daily_counter_is_inspectable_without_allocating_a_successor() {
    let ledger = "# seed\n1 1.20261109.1\n2 1.20261109.999999999\n3 1.20261110.1\n";
    let tags = "old\trefs/tags/v1.20261109.1\nlast\trefs/tags/v1.20261109.999999999\nfuture\trefs/tags/v1.20261110.1\n";
    assert_eq!(
        bump_base_release(ledger, "1.20261109.999999999", tags).unwrap(),
        Some(("1.20261109.999999999".into(), "last".into()))
    );
    assert_eq!(
        bump_base_release(ledger, "1.20261109.1", tags).unwrap(),
        Some(("1.20261109.1".into(), "old".into()))
    );
}

fn git_cmd(dir: &Path, args: &[&str]) -> std::process::Output {
    git().arg("-C").arg(dir).args(args).output().unwrap()
}
