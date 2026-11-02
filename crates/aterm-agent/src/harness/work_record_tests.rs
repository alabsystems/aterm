// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// EACH AGENT FINDS ITS FOLDER ([`work_dir`]): Claude Code's transcript's last `cwd`, else where
/// it started; codex's rollout cwd, else where it started; any other agent's start folder; and,
/// for every agent, the terminal's own where none of those is known. NEGATIVE CONTROL: a relative
/// path is never chosen, whichever input holds it.
#[test]
fn each_agent_finds_its_folder() {
    let tail = concat!(
        "{\"type\":\"user\",\"cwd\":\"/work/first\"}\n",
        "{\"type\":\"assistant\",\"cwd\":\"/work/now\"}\n",
        "{\"type\":\"assistant\"}\n",
    );
    let (rollout, engine) = (Path::new("/work/rollout"), Path::new("/work/engine"));
    let every = Folders {
        started: Some("/work/started"),
        transcript: Some(tail),
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
        (Program::Claude, every, Some("/work/now")),
        (
            Program::Claude,
            Folders {
                transcript: None,
                ..every
            },
            Some("/work/started"),
        ),
        (Program::Claude, unstarted, Some("/work/engine")),
        (Program::Codex, every, Some("/work/rollout")),
        (
            Program::Codex,
            Folders {
                rollout: None,
                ..every
            },
            Some("/work/started"),
        ),
        (Program::Codex, unstarted, Some("/work/engine")),
        (Program::Agy, every, Some("/work/started")),
        (Program::Generic, every, Some("/work/started")),
        (Program::Muse, unstarted, Some("/work/engine")),
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
