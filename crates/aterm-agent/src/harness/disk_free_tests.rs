// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm harness disk free`: every fence, on cache roots under a scratch home only.

use super::*;

use aterm_sysprobe::volumes::{Mount, Table};
use aterm_uds::holders::{PathScan, PathUse};

/// A scratch home, resolved, so the paths given are their own real paths.
struct Home {
    _tmp: aterm_tempfile::TempDir,
    path: PathBuf,
}

impl Home {
    fn new() -> Home {
        let tmp = aterm_tempfile::TempDir::new().expect("scratch home");
        let path = std::fs::canonicalize(tmp.path()).expect("resolved scratch home");
        Home { _tmp: tmp, path }
    }

    /// A cache folder `rel` under the home, holding one file of `len` bytes.
    fn cache(&self, rel: &str, len: usize) -> PathBuf {
        let dir = self.path.join(rel);
        std::fs::create_dir_all(&dir).expect("cache folder");
        std::fs::write(dir.join("blob"), vec![7u8; len]).expect("cache file");
        dir
    }
}

fn nobody(_: &[PathBuf], _: &[u32]) -> std::io::Result<PathScan> {
    Ok(PathScan::default())
}

fn whole() -> Table {
    Table {
        complete: true,
        ..Table::default()
    }
}

fn on() -> Config {
    Config {
        auto_free_gib: 10,
        ..Config::default()
    }
}

fn world<'a>(home: &'a Home, processes: discover::Processes<'a>, mounts: &'a Table) -> World<'a> {
    World {
        home: &home.path,
        processes,
        mounts: Some(mounts),
        config: on(),
        now: 1_761_350_400,
        sid: "s-worker",
    }
}

/// Run over `paths`, every journal write landing in the returned rows.
fn free(paths: &[PathBuf], dry_run: bool, w: &World<'_>) -> (Vec<Outcome>, Vec<Value>) {
    let mut rows = Vec::new();
    let outcomes = run(paths, dry_run, w, &mut |r| {
        rows.push(aterm_json::from_str::<Value>(r).expect("a row is one JSON object"));
        true
    });
    (outcomes, rows)
}

fn refusal(o: &Outcome) -> &str {
    match &o.verdict {
        Verdict::Refused(why) => why,
        other => panic!("{} was not refused: {other:?}", o.path.display()),
    }
}

#[test]
fn a_cache_under_each_root_is_removed_and_journalled_before_and_after() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    for root in ROOTS {
        let dir = home.cache(&format!("{}/some-tool", root.rel), 64 * 1024);
        let (outcomes, rows) = free(std::slice::from_ref(&dir), false, &w);
        let [o] = outcomes.as_slice() else {
            panic!("one outcome per path: {outcomes:?}");
        };
        let Verdict::Removed { bytes, partial } = o.verdict else {
            panic!("{} was not removed: {:?}", root.rel, o.verdict);
        };
        assert!(bytes >= 64 * 1024, "{}: counted {bytes}", root.rel);
        assert!(!partial);
        assert!(!dir.exists(), "{} is gone", dir.display());
        assert!(dir.parent().expect("root").is_dir(), "the root stays");
        let actions: Vec<&str> = rows
            .iter()
            .map(|r| r["action"].as_str().expect("action"))
            .collect();
        assert_eq!(actions, ["removing", "removed"], "{}", root.rel);
        for r in &rows {
            assert_eq!(r["kind"], Value::from(KIND));
            assert_eq!(r["sid"], Value::from("s-worker"));
            assert_eq!(r["path"], Value::from(dir.display().to_string()));
            assert_eq!(r["bytes"], Value::from(bytes));
        }
    }
}

#[test]
fn a_file_under_a_root_is_removed_and_its_blocks_counted() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let dir = home.cache(".npm/_cacache", 8 * 1024);
    let file = dir.join("blob");
    let (outcomes, _) = free(std::slice::from_ref(&file), false, &w);
    assert!(
        matches!(outcomes[0].verdict, Verdict::Removed { bytes, .. } if bytes >= 8 * 1024),
        "{:?}",
        outcomes[0]
    );
    assert!(!file.exists());
    assert!(dir.is_dir(), "only the file named goes");
}

#[test]
fn a_link_and_a_path_through_a_link_are_refused_and_nothing_behind_them_goes() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let far = home.cache("far/sub", 1024);
    let cache = home.path.join(".cache");
    std::fs::create_dir_all(&cache).expect("root");
    let link = cache.join("tool");
    atpkg::platform::atomic_symlink(&home.path.join("far"), &link).expect("a link");
    let (outcomes, rows) = free(&[link.clone(), link.join("sub")], false, &w);
    assert!(
        refusal(&outcomes[0]).contains("symbolic link"),
        "{outcomes:?}"
    );
    assert!(
        refusal(&outcomes[1]).contains("its real path is"),
        "{outcomes:?}"
    );
    assert!(far.join("blob").is_file(), "what the link reaches stays");
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("the link stays")
            .file_type()
            .is_symlink()
    );
    let actions: Vec<&str> = rows
        .iter()
        .map(|r| r["action"].as_str().expect("action"))
        .collect();
    assert_eq!(
        actions,
        ["refused", "refused"],
        "no intent row for a refusal"
    );
}

#[test]
fn a_root_reached_through_a_link_or_resolving_outside_home_is_refused() {
    let home = Home::new();
    let outside = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let elsewhere = outside.cache("npm/tool", 1024);
    atpkg::platform::atomic_symlink(&outside.path.join("npm"), &home.path.join(".npm"))
        .expect("a linked root");
    let (outcomes, _) = free(&[home.path.join(".npm/tool"), elsewhere.clone()], false, &w);
    assert!(
        refusal(&outcomes[0]).contains("outside the home folder"),
        "{outcomes:?}"
    );
    assert!(
        refusal(&outcomes[1]).contains("outside the home folder"),
        "{outcomes:?}"
    );
    assert!(elsewhere.join("blob").is_file());

    let inside = home.cache("real-registry/index", 1024);
    std::fs::create_dir_all(home.path.join(".cargo")).expect(".cargo");
    atpkg::platform::atomic_symlink(
        &home.path.join("real-registry"),
        &home.path.join(".cargo/registry"),
    )
    .expect("a root linked inside home");
    let (outcomes, _) = free(&[home.path.join(".cargo/registry/index")], false, &w);
    assert!(
        refusal(&outcomes[0]).contains("reached through a link"),
        "{outcomes:?}"
    );
    assert!(inside.join("blob").is_file());
    let (outcomes, _) = free(std::slice::from_ref(&inside), false, &w);
    assert!(
        matches!(outcomes[0].verdict, Verdict::Removed { .. }),
        "the root's real path is the root: {outcomes:?}"
    );
}

#[test]
fn a_root_itself_a_path_outside_every_root_and_a_relative_path_are_refused() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let root = home.cache(".cargo/git", 1024);
    let doc = home.cache("Documents/notes", 1024);
    let (outcomes, rows) = free(
        &[
            root.clone(),
            doc.clone(),
            home.path.join("Library"),
            PathBuf::from(".cache/tool"),
        ],
        false,
        &w,
    );
    assert!(refusal(&outcomes[0]).contains("cache root"), "{outcomes:?}");
    assert!(refusal(&outcomes[0]).contains("itself"), "{outcomes:?}");
    assert!(refusal(&outcomes[1]).contains("none of the cache roots"));
    assert!(refusal(&outcomes[1]).contains("~/.cargo/registry"));
    assert!(refusal(&outcomes[2]).contains("none of the cache roots"));
    assert!(refusal(&outcomes[3]).contains("not an absolute path"));
    assert!(root.join("blob").is_file());
    assert!(doc.join("blob").is_file());
    assert_eq!(rows.len(), 4, "each refusal is journalled");
    assert!(outcomes.iter().all(Outcome::refused));
}

#[test]
fn the_children_a_root_keeps_stay() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let kept = [
        home.cache("Library/Caches/com.apple.foo", 1024),
        home.cache("Library/Caches/Com.Apple.Bar", 1024),
        home.cache("Library/Caches/aterm/x", 1024),
        home.cache("Library/Caches/com.aterm.aterm", 1024),
        home.cache("Library/Caches/.hidden", 1024),
        home.cache(".cache/aterm/x", 1024),
    ];
    let (outcomes, _) = free(&kept, false, &w);
    for (o, dir) in outcomes.iter().zip(&kept) {
        assert!(refusal(o).contains("it stays"), "{o:?}");
        assert!(dir.join("blob").is_file(), "{} stays", dir.display());
    }
    assert!(refusal(&outcomes[0]).contains("macOS's own"));
    assert!(refusal(&outcomes[2]).contains("aterm's own"));
    let other = home.cache("Library/Caches/Homebrew", 1024);
    let (outcomes, _) = free(std::slice::from_ref(&other), false, &w);
    assert!(
        matches!(outcomes[0].verdict, Verdict::Removed { .. }),
        "another tool's cache goes: {outcomes:?}"
    );
}

#[test]
fn a_path_a_process_uses_or_cannot_rule_out_stays() {
    let home = Home::new();
    let mounts = whole();
    let dir = home.cache(".cache/pip", 1024);
    let held = dir.join("blob");
    let holds = |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        assert_eq!(
            dirs,
            std::slice::from_ref(&dir),
            "the scan reads the path itself"
        );
        Ok(PathScan {
            users: vec![PathUse {
                pid: 4242,
                how: "fd",
                path: held.clone(),
            }],
            ..PathScan::default()
        })
    };
    let w = world(&home, &holds, &mounts);
    let (outcomes, _) = free(std::slice::from_ref(&dir), false, &w);
    assert!(
        refusal(&outcomes[0]).contains("process 4242"),
        "{outcomes:?}"
    );
    assert!(held.is_file());

    let unlisted = |_: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        Err(std::io::Error::other("no process list"))
    };
    let w = world(&home, &unlisted, &mounts);
    let (outcomes, _) = free(std::slice::from_ref(&dir), false, &w);
    assert!(
        refusal(&outcomes[0]).contains("could not be listed"),
        "{outcomes:?}"
    );
    assert!(held.is_file());
}

#[test]
fn a_mount_under_it_or_a_mount_table_not_read_whole_refuses() {
    let home = Home::new();
    let dir = home.cache(".cache/vm", 1024);
    let mut mounted = whole();
    mounted.mounts.push(Mount {
        on: dir.join("disk"),
        from: "/dev/disk9s1".to_owned(),
        fstype: "apfs".to_owned(),
        local: true,
        automounted: false,
        removable: true,
        read_only: false,
        container: None,
    });
    let w = world(&home, &nobody, &mounted);
    let (outcomes, _) = free(std::slice::from_ref(&dir), false, &w);
    assert!(
        refusal(&outcomes[0]).contains("mounted inside it"),
        "{outcomes:?}"
    );
    assert!(dir.join("blob").is_file());

    let cut = Table {
        complete: false,
        ..Table::default()
    };
    let w = world(&home, &nobody, &cut);
    let (outcomes, _) = free(std::slice::from_ref(&dir), false, &w);
    assert!(
        refusal(&outcomes[0]).contains("not read whole"),
        "{outcomes:?}"
    );
    let w = World {
        mounts: None,
        ..world(&home, &nobody, &cut)
    };
    let (outcomes, _) = free(std::slice::from_ref(&dir), false, &w);
    assert!(
        refusal(&outcomes[0]).contains("not read whole"),
        "{outcomes:?}"
    );
    assert!(dir.join("blob").is_file());
}

#[test]
fn with_the_grant_off_everything_is_refused_but_a_dry_run_still_says_what_would_go() {
    let home = Home::new();
    let mounts = whole();
    let dir = home.cache(".cargo/registry/cache", 32 * 1024);
    let kept = home.cache("Library/Caches/com.apple.x", 1024);
    let w = World {
        config: Config {
            auto_free_gib: 0,
            ..Config::default()
        },
        ..world(&home, &nobody, &mounts)
    };
    assert!(!w.grant());
    let (outcomes, rows) = free(std::slice::from_ref(&dir), false, &w);
    assert_eq!(refusal(&outcomes[0]), GRANT_OFF);
    assert!(dir.join("blob").is_file());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["action"], Value::from("refused"));

    let (outcomes, rows) = free(&[dir.clone(), kept.clone()], true, &w);
    assert!(
        matches!(outcomes[0].verdict, Verdict::Would { bytes, .. } if bytes >= 32 * 1024),
        "{outcomes:?}"
    );
    assert!(!outcomes[0].refused());
    assert!(refusal(&outcomes[1]).contains("it stays"));
    assert!(rows.is_empty(), "a dry run journals nothing");
    assert!(dir.join("blob").is_file(), "a dry run removes nothing");
    assert!(kept.join("blob").is_file());
    let json = outcomes[0].to_json(true, w.grant());
    assert_eq!(json["removed"], Value::from(false));
    assert_eq!(json["refused"], Value::Null);
    assert_eq!(json["dry_run"], Value::from(true));
    assert_eq!(json["grant"], Value::from(false));
    assert!(outcomes[0].line().starts_with("would remove "));
}

#[test]
fn a_journal_that_cannot_write_refuses_the_removal() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let dir = home.cache(".npm/_cacache", 1024);
    let mut tried = Vec::new();
    let outcomes = run(std::slice::from_ref(&dir), false, &w, &mut |r| {
        tried.push(r.to_owned());
        false
    });
    assert_eq!(refusal(&outcomes[0]), UNJOURNALLED);
    assert!(dir.join("blob").is_file(), "nothing unjournalled goes");
    assert!(tried[0].contains("\"removing\""), "{tried:?}");
}

#[cfg(unix)]
#[test]
fn a_removal_that_fails_part_way_journals_what_it_released() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let dir = home.cache(".cache/tool", 16 * 1024);
    let locked = home.cache(".cache/tool/locked", 4 * 1024);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let (outcomes, rows) = free(std::slice::from_ref(&dir), false, &w);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let Verdict::Failed { freed, .. } = &outcomes[0].verdict else {
        panic!("{outcomes:?}");
    };
    assert!(outcomes[0].refused());
    let actions: Vec<&str> = rows
        .iter()
        .map(|r| r["action"].as_str().expect("action"))
        .collect();
    assert_eq!(actions, ["removing", "failed"]);
    assert_eq!(rows[1]["bytes"], Value::from(*freed));
    assert!(locked.join("blob").is_file(), "what could not go stays");
}

#[test]
fn the_json_object_names_each_field() {
    let o = Outcome {
        path: PathBuf::from("/h/.npm/x"),
        verdict: Verdict::Refused("why".to_owned()),
    };
    let json = o.to_json(false, true);
    assert_eq!(json["path"], Value::from("/h/.npm/x"));
    assert_eq!(json["removed"], Value::from(false));
    assert_eq!(json["bytes"], Value::from(0u64));
    assert_eq!(json["refused"], Value::from("why"));
    assert_eq!(o.line(), "refused /h/.npm/x: why");
    let o = Outcome {
        path: PathBuf::from("/h/.npm/x"),
        verdict: Verdict::Removed {
            bytes: 2048,
            partial: true,
        },
    };
    assert_eq!(o.line(), "removed /h/.npm/x (at least 2.0 KiB)");
    assert_eq!(o.to_json(false, true)["removed"], Value::from(true));
}

/// `--dry-run` with no path lists what the roots hold that may go, largest first, removing and
/// journalling nothing. CONTROL: a child a root keeps by its name (macOS's own, aterm's) is not
/// listed, and nothing listed is gone.
#[test]
fn a_survey_lists_what_the_roots_hold_that_may_go_largest_first_and_removes_nothing() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let pip = home.cache("Library/Caches/pip", 64 * 1024);
    let npm = home.cache(".npm/_cacache", 512 * 1024);
    let apple = home.cache("Library/Caches/com.apple.Safari", 1024 * 1024);
    let ours = home.cache(".cache/aterm", 1024 * 1024);
    let listed = survey(&w);
    let paths: Vec<&Path> = listed.iter().map(|o| o.path.as_path()).collect();
    assert_eq!(
        paths,
        [
            npm.parent()
                .expect("the .npm root")
                .join("_cacache")
                .as_path(),
            pip.as_path()
        ]
    );
    assert!(
        listed
            .iter()
            .all(|o| matches!(o.verdict, Verdict::Would { .. }))
    );
    for kept in [&pip, &npm, &apple, &ours] {
        assert!(kept.exists(), "{} stays", kept.display());
    }
}
