// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// EACH AGENT FINDS ITS FOLDER ([`work_dir`]): Claude Code's transcript's last `cwd`, else where
/// it started; codex's rollout cwd, else where it started; any other agent's start folder; and,
/// for every agent, the terminal's own where none of those is known. NEGATIVE CONTROL: a relative
/// path is never chosen, whichever input holds it.
#[test]
fn each_agent_finds_its_folder() {
    let tail = format!(
        "{{\"type\":\"user\",\"cwd\":\"{}\"}}\n\
         {{\"type\":\"assistant\",\"cwd\":\"{}\"}}\n\
         {{\"type\":\"assistant\"}}\n",
        at!("/work/first"),
        at!("/work/now"),
    );
    let (rollout, engine) = (
        Path::new(at!("/work/rollout")),
        Path::new(at!("/work/engine")),
    );
    let every = Folders {
        started: Some(at!("/work/started")),
        transcript: Some(&tail),
        rollout: Some(rollout),
        engine: Some(engine),
    };
    let unstarted = Folders {
        started: None,
        transcript: None,
        rollout: None,
        engine: Some(engine),
    };
    let rows: [(Program, Folders<'_>, Option<&str>); 9] = [
        (Program::Claude, every, Some(at!("/work/now"))),
        (
            Program::Claude,
            Folders {
                transcript: None,
                ..every
            },
            Some(at!("/work/started")),
        ),
        (Program::Claude, unstarted, Some(at!("/work/engine"))),
        (Program::Codex, every, Some(at!("/work/rollout"))),
        (
            Program::Codex,
            Folders {
                rollout: None,
                ..every
            },
            Some(at!("/work/started")),
        ),
        (Program::Codex, unstarted, Some(at!("/work/engine"))),
        (Program::Agy, every, Some(at!("/work/started"))),
        (Program::Generic, every, Some(at!("/work/started"))),
        (Program::Muse, unstarted, Some(at!("/work/engine"))),
    ];
    for (agent, at, want) in rows {
        assert_eq!(
            work_dir(agent, at),
            want.map(PathBuf::from),
            "{agent:?} over {at:?}"
        );
    }
    let relative = Folders {
        started: Some("work/started"),
        transcript: Some("{\"cwd\":\"work/now\"}\n"),
        rollout: Some(Path::new("work/rollout")),
        engine: Some(Path::new("work/engine")),
    };
    for agent in [
        Program::Claude,
        Program::Codex,
        Program::Agy,
        Program::Muse,
        Program::Grok,
        Program::Kimi,
        Program::Generic,
    ] {
        assert_eq!(
            work_dir(agent, relative),
            None,
            "CONTROL: {agent:?} chooses no relative path"
        );
    }
}

/// A RECORD'S NAMES ARE ONE SORTED SET ([`changed_names`]): the committed names and the working
/// tree's together, each once, a rename's both paths (the index's `R ` and the working tree's
/// ` R`, an intent-to-add move, alike), no ignored entry, and the count is the set's. NEGATIVE
/// CONTROL: with nothing changed there is no name and the count is 0.
#[test]
fn the_changed_names_are_the_committed_and_the_working_trees_once_each() {
    let committed = "src/lib.rs\0CHANGELOG.md\0";
    let status =
        " M src/lib.rs\0R  new.rs\0old.rs\0 R src/moved.rs\0src/was.rs\0?? notes/\0!! target/\0";
    let (paths, files) = changed_names(committed, status);
    assert_eq!(
        paths,
        [
            "CHANGELOG.md",
            "new.rs",
            "notes/",
            "old.rs",
            "src/lib.rs",
            "src/moved.rs",
            "src/was.rs"
        ]
    );
    assert_eq!(files, 7);
    assert_eq!(
        changed_names("", ""),
        (Vec::new(), 0),
        "CONTROL: nothing changed"
    );
}

/// A record of `common_dir` on `host` naming `paths`.
fn at(host: &str, common_dir: &str, paths: &[&str]) -> WorkRecord {
    WorkRecord {
        host: host.into(),
        common_dir: common_dir.into(),
        paths: paths.iter().map(|p| (*p).to_string()).collect(),
        files: u32::try_from(paths.len()).unwrap_or(u32::MAX),
        ..WorkRecord::default()
    }
}

/// TWO SESSIONS IN ONE REPOSITORY COUNT EACH OTHER ([`overlap`]): two records of one common dir on
/// one host, from two worktrees, that share `src/lib.rs` and `CHANGELOG.md` count 2 each, each
/// naming the other, and a third session there that shares no file is named by neither.
/// NEGATIVE CONTROLS: two records in different repositories count 0, and so do two at one common
/// dir on two hosts.
#[test]
fn two_sessions_in_one_repository_count_each_other() {
    let a = WorkRecord {
        tree: "/repo/wa".into(),
        ..at(
            "m1.local",
            "/repo/.git",
            &["CHANGELOG.md", "src/a.rs", "src/lib.rs"],
        )
    };
    let b = WorkRecord {
        tree: "/repo/wb".into(),
        ..at(
            "m1.local",
            "/repo/.git",
            &["CHANGELOG.md", "src/b.rs", "src/lib.rs"],
        )
    };
    let c = at("m1.local", "/repo/.git", &["docs/c.md"]);
    let from_a = overlap(&a, [("s-b", &b), ("s-c", &c)]);
    let from_b = overlap(&b, [("s-a", &a), ("s-c", &c)]);
    assert_eq!(
        from_a,
        Overlap {
            files: 2,
            peers: vec!["s-b"]
        }
    );
    assert_eq!(
        from_b,
        Overlap {
            files: 2,
            peers: vec!["s-a"]
        }
    );
    let elsewhere = at("m1.local", "/other/.git", &["CHANGELOG.md", "src/lib.rs"]);
    assert_eq!(
        overlap(&a, [("s-x", &elsewhere)]),
        Overlap::default(),
        "CONTROL: another repository counts 0"
    );
    let other_host = at("m2.local", "/repo/.git", &["CHANGELOG.md", "src/lib.rs"]);
    assert_eq!(
        overlap(&a, [("s-y", &other_host)]),
        Overlap::default(),
        "CONTROL: one common dir on two hosts counts 0"
    );
}
