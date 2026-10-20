// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::supervise::ScratchDir;

const DAY: u64 = 24 * 3_600;

/// The unix second now, the clock file mtimes are set against.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs()
}

/// Writes `<root>/drive/<name>` holding `body`, last written `age` seconds before `now`.
fn file(root: &Path, name: &str, body: &str, now: u64, age: u64) {
    let drive = root.join("drive");
    std::fs::create_dir_all(&drive).expect("drive");
    let path = drive.join(name);
    std::fs::write(&path, body).expect("write");
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(now - age);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|f| f.set_modified(at))
        .expect("mtime");
}

/// A tab's group: its ledger and journal (and whatever `extra` names), `age` old.
fn group(root: &Path, stem: &str, extra: &[(&str, &str)], now: u64, age: u64) {
    file(root, &format!("{stem}.jsonl"), "{}\n", now, age);
    file(root, &format!("{stem}.journal.jsonl"), "{}\n", now, age);
    for (suffix, body) in extra {
        file(root, &format!("{stem}{suffix}"), body, now, age);
    }
}

/// The tabs whose ledger is still there.
fn left(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join("drive"))
        .expect("drive")
        .filter_map(|e| e.ok()?.file_name().to_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

/// A drive directory of seven tabs and three names that are no tab's group:
/// - `s-a1` live (named), the oldest, its note naming `s-b2`;
/// - `s-b2`, only that note names it;
/// - `s-c3` written a day ago;
/// - `s-d4` (its goal hold), `s-e5`, `s-f6` and `s-0a`, nothing naming them, 20, 15, 10 and 8
///   days old;
/// - `self.jsonl`, a cut's `s-0a.jsonl.tmp`, and a DIRECTORY `s-99.jsonl`.
///
/// Fifteen group files in all: the directory is no file, counts for nothing and keeps its tab.
fn drive(name: &str, now: u64) -> ScratchDir {
    let dir = ScratchDir::new("drive-retention", name);
    group(&dir, "s-a1", &[(".earlier-tabs", "s-b2\n")], now, 30 * DAY);
    group(&dir, "s-b2", &[], now, 29 * DAY);
    group(&dir, "s-c3", &[], now, DAY);
    file(&dir, "s-d4.jsonl", "{}\n", now, 20 * DAY);
    file(&dir, "s-d4.goal-hold.json", "{}", now, 20 * DAY);
    group(&dir, "s-e5", &[], now, 15 * DAY);
    group(&dir, "s-f6", &[], now, 10 * DAY);
    group(&dir, "s-0a", &[], now, 8 * DAY);
    file(&dir, "self.jsonl", "{}\n", now, 40 * DAY);
    file(&dir, "s-0a.jsonl.tmp", "{}\n", now, 40 * DAY);
    std::fs::create_dir_all(dir.join("drive/s-99.jsonl")).expect("a directory");
    dir
}

fn live() -> Option<BTreeSet<String>> {
    Some(["s-a1".to_string()].into())
}

/// THE DRIVE DIRECTORY KEEPS ITS BUDGET AND EVERY TAB SOMETHING NAMES (relaunch-10 of
/// ATERM-KI-1016-1, the review's KR-5): over the budget, the oldest groups nothing names go,
/// oldest first, until the groups are within it — never a named tab, an earlier tab a kept tab's
/// note names, a group written this week, a name that is no tab's group, or a directory.
/// NEGATIVE CONTROLS: unnamed, the live tab and the tab its note named go first; with no note,
/// the noted tab goes; within the budget nothing is read or unlinked; what names tabs unread
/// unlinks nothing.
#[test]
fn the_drive_directory_keeps_its_budget_and_every_tab_something_names() {
    let now = now();
    let budget = |files: usize| Budget {
        files,
        bytes: 64 << 20,
    };
    let all = left(&drive("all", now));

    // Over by six: the three oldest unnamed tabs (six files), and no more.
    let dir = drive("over", now);
    assert_eq!(prune(&dir, live, now, budget(10)), ["s-d4", "s-e5", "s-f6"]);
    let kept: Vec<String> = all
        .iter()
        .filter(|n| !["s-d4", "s-e5", "s-f6"].iter().any(|s| n.starts_with(*s)))
        .cloned()
        .collect();
    assert_eq!(left(&dir), kept);
    assert!(kept.contains(&"self.jsonl".to_string()));
    assert!(kept.contains(&"s-0a.jsonl.tmp".to_string()));
    assert!(kept.contains(&"s-99.jsonl".to_string()));
    assert!(
        kept.contains(&"s-b2.jsonl".to_string()),
        "named by a kept note"
    );
    assert!(
        kept.contains(&"s-c3.jsonl".to_string()),
        "written this week"
    );

    // Over by everything: what is protected stays, whatever the budget.
    let dir = drive("all-over", now);
    assert_eq!(
        prune(&dir, live, now, budget(0)),
        ["s-d4", "s-e5", "s-f6", "s-0a"]
    );
    let after = left(&dir);
    assert!(!after.contains(&"s-0a.jsonl".to_string()), "{after:?}");
    assert!(after.contains(&"s-0a.jsonl.tmp".to_string()), "{after:?}");

    // NEGATIVE CONTROL: nothing names the live tab — it and its noted tab, the oldest, go first.
    let dir = drive("unnamed", now);
    assert_eq!(
        prune(&dir, || Some(BTreeSet::new()), now, budget(10)),
        ["s-a1", "s-b2"]
    );
    // NEGATIVE CONTROL: no note — the tab only it named goes first.
    let dir = drive("unnoted", now);
    std::fs::remove_file(dir.join("drive/s-a1.earlier-tabs")).expect("the note");
    assert_eq!(prune(&dir, live, now, budget(10)), ["s-b2", "s-d4"]);

    // Within the budget: nothing is asked, nothing goes.
    let dir = drive("within", now);
    let asked = std::cell::Cell::new(false);
    let named = || {
        asked.set(true);
        live()
    };
    assert!(prune(&dir, named, now, budget(15)).is_empty());
    assert!(!asked.get(), "the usual pass is one listing");
    assert_eq!(left(&dir), all);
    // What names tabs could not be read: nothing goes.
    assert!(prune(&dir, || None, now, budget(0)).is_empty());
    assert_eq!(left(&dir), all);

    // A complete small listing still prunes through the production collector.
    let dir = drive("bounded-positive", now);
    assert_eq!(
        prune_with_limit(&dir, live, now, budget(10), all.len()),
        ["s-d4", "s-e5", "s-f6"]
    );
    // A partial group census or naming census proves no group unreferenced.
    let dir = drive("bounded-refusal", now);
    assert!(prune_with_limit(&dir, live, now, budget(0), 1).is_empty());
    assert_eq!(left(&dir), all);
    let naming = dir.join("records");
    std::fs::create_dir(&naming).expect("records");
    for name in ["a", "b"] {
        std::fs::write(naming.join(name), "s-d4\n").expect("naming record");
    }
    assert_eq!(
        tabs_named_with_limit(&naming, |_| true, 2),
        Some(["s-d4".to_string()].into())
    );
    let named = || tabs_named_with_limit(&naming, |_| true, 1);
    assert_eq!(named(), None);
    assert!(prune(&dir, named, now, budget(0)).is_empty());
    assert_eq!(left(&dir), all, "INCOMPLETE_NAMING_MUST_KEEP_DRIVE_GROUPS");
    // Oversized naming files and earlier-tab notes are unknown, never prefixes.
    let oversized = std::fs::File::create(naming.join("large")).expect("large record");
    oversized
        .set_len(MAX_NAMING_BYTES + 1)
        .expect("sparse record");
    assert_eq!(tabs_named_in(&naming, |_| true), None);
    let note = std::fs::File::create(dir.join("drive/s-a1.earlier-tabs")).expect("note");
    note.set_len(MAX_NAMING_BYTES + 1).expect("sparse note");
    assert!(prune(&dir, live, now, budget(0)).is_empty());
    assert_eq!(left(&dir), all);

    // THE BYTES COUNT TOO: within the file count, over the bytes, the oldest unnamed goes.
    let dir = drive("bytes", now);
    file(&dir, "s-0a.journal.jsonl", &"x".repeat(4_096), now, 8 * DAY);
    assert_eq!(
        prune(
            &dir,
            live,
            now,
            Budget {
                files: 100,
                bytes: 4_096
            }
        ),
        ["s-d4", "s-e5", "s-f6", "s-0a"]
    );
}

/// A TAB ID IS READ WHEREVER IT STANDS ([`tab_ids_in`]): a TOML string, a record key, a JSON
/// field, a run of hex cut short; `s-` with no hex is none. [`tabs_named_in`] reads the files
/// whose names it is asked for, and a directory that is not there names none.
#[test]
fn every_tab_id_a_record_or_layout_holds_is_read() {
    let mut ids = BTreeSet::new();
    tab_ids_in(
        "sid = \"s-0123abcd\"\nkey codex-s-beef\n{\"tab\":\"s-DEAD\"} s- s-xyz s-abcz",
        &mut ids,
    );
    assert_eq!(
        ids,
        ["s-0123abcd", "s-DEAD", "s-abc", "s-beef"]
            .map(str::to_string)
            .into()
    );

    let dir = ScratchDir::new("drive-retention", "named-in");
    std::fs::write(dir.join("session.toml"), "sid = \"s-1\"\n").expect("layout");
    std::fs::write(dir.join("ledger.jsonl"), "{\"tab\":\"s-2\"}\n").expect("ledger");
    std::fs::create_dir_all(dir.join("s-3.toml")).expect("a directory");
    let toml = |name: &str| name.ends_with(".toml");
    assert_eq!(tabs_named_in(&dir, toml), Some(["s-1".to_string()].into()));
    assert_eq!(
        tabs_named_in(&dir, |_| true),
        Some(["s-1".to_string(), "s-2".to_string()].into())
    );
    assert_eq!(
        tabs_named_in(&dir.join("missing"), toml),
        Some(BTreeSet::new())
    );
    #[cfg(unix)]
    {
        // A wanted naming link cannot prove which tabs are unnamed, even if its
        // target can be read now. Directories above remain deliberately ignored.
        std::os::unix::fs::symlink(dir.join("session.toml"), dir.join("alias.toml"))
            .expect("a layout alias");
        assert_eq!(
            tabs_named_in(&dir, toml),
            None,
            "NAMING_ALIAS_MUST_KEEP_DRIVE_GROUPS"
        );
    }
}

/// One registry listing names its own entries, not every shared-state writer. Captured
/// pruning below is given this fixture's complete protected set. Empty or missing roster
/// admission, an unread directory and an overfull listing all refuse that set.
#[test]
fn a_registry_lists_its_entries_and_refuses_unknown_or_partial_lists() {
    let dir = ScratchDir::new("drive-retention", "registry");
    let graph = dir.join("graph");
    std::fs::create_dir_all(&graph).expect("graph");
    // This instance's tab, another instance's, and two names that are no tab.
    for name in ["s-a1", "s-b2", "notes", "s-xyz"] {
        std::fs::write(graph.join(name), "sock = \"/x\"\n").expect("entry");
    }
    let ours = ["s-a1".to_string()];
    assert_eq!(
        registry_tabs(&graph, &ours),
        Some(["s-a1".to_string(), "s-b2".to_string()].into()),
        "another instance's live tab is named"
    );
    assert_eq!(
        registry_tabs(&graph, &[]),
        None,
        "an empty roster vouches for nothing"
    );
    assert_eq!(
        registry_tabs(&graph, &["s-a1".to_string(), "s-c3".to_string()]),
        None,
        "a roster tab the registry lacks: the registry is not this instance's"
    );
    assert_eq!(registry_tabs(&dir.join("missing"), &ours), None);
    assert_eq!(
        registry_tabs_with_limit(&graph, &ours, 2),
        None,
        "a partial listing"
    );
    assert!(registry_tabs_with_limit(&graph, &ours, 4).is_some());

    // This owned fixture supplies every protected tab explicitly through its listing.
    // Removing an entry permits that fixture's unreferenced group to go.
    let now = now();
    for stem in ["s-a1", "s-b2", "s-c3"] {
        group(&dir, stem, &[], now, 30 * DAY);
    }
    let none = Budget { files: 0, bytes: 0 };
    assert_eq!(
        prune(&dir, || registry_tabs(&graph, &ours), now, none),
        ["s-c3"]
    );
    assert!(
        prune(&dir, || registry_tabs(&graph, &[]), now, none).is_empty(),
        "NEGATIVE CONTROL: an unvouched registry unlinks nothing"
    );
    std::fs::remove_file(graph.join("s-b2")).expect("retire");
    assert_eq!(
        prune(&dir, || registry_tabs(&graph, &ours), now, none),
        ["s-b2"]
    );
}
