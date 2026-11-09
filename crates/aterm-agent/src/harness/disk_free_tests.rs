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
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
                .expect("private cache folders");
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&dir).expect("cache folder");
        std::fs::write(dir.join("blob"), vec![7u8; len]).expect("cache file");
        dir
    }
}

#[cfg(unix)]
fn short_home() -> Home {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("ipc-")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("short private socket fixture");
    Home {
        path: std::fs::canonicalize(tmp.path()).expect("resolved short home"),
        _tmp: tmp,
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
fn managed_custody_keeps_roots_descendants_and_containing_directories() {
    for name in atpkg::build_cache::registered::CUSTODY_MARKERS {
        let home = Home::new();
        let mounts = whole();
        let w = world(&home, &nobody, &mounts);
        let outer = home.cache(".cache/tool", 1024);
        let managed = home.cache(".cache/tool/nested/managed", 2048);
        std::fs::write(managed.join(name), br#"{"schema":999}"#).unwrap();
        for path in [&outer, &managed, &managed.join("blob")] {
            let (outcomes, rows) = free(std::slice::from_ref(path), false, &w);
            assert!(outcomes[0].refused());
            assert!(
                !rows
                    .iter()
                    .any(|row| row["action"] == Value::from("removing"))
            );
        }
        assert!(outer.join("blob").is_file());
        assert_eq!(
            std::fs::read(managed.join("blob")).unwrap(),
            vec![7u8; 2048]
        );
    }
}

#[cfg(unix)]
#[test]
fn registered_custody_without_an_embedded_marker_and_unknown_records_still_refuse() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let parent = home.cache(".cache/tool", 1024);
    let target = parent.join("registered");
    let owner = home.path.join("source");
    aterm_uds::private_file::ensure_private_dir(&owner).unwrap();
    std::fs::write(owner.join("Cargo.toml"), b"[workspace]\n").unwrap();
    assert!(atpkg::build_cache::registered::create_checked(&home.path, &target, &owner).unwrap());
    std::fs::write(target.join("payload"), b"keep").unwrap();
    std::fs::remove_file(target.join(atpkg::build_cache::registered::MARKER)).unwrap();
    for path in [&parent, &target, &target.join("payload")] {
        let (outcomes, _) = free(std::slice::from_ref(path), false, &w);
        assert!(outcomes[0].refused());
    }
    assert_eq!(std::fs::read(target.join("payload")).unwrap(), b"keep");
    let unrelated = home.cache(".npm/unrelated", 512);
    std::fs::write(
        home.path
            .join(atpkg::build_cache::registered::INDEX)
            .join("unknown.json"),
        br#"{"schema":999}"#,
    )
    .unwrap();
    let (outcomes, _) = free(std::slice::from_ref(&unrelated), false, &w);
    assert!(outcomes[0].refused());
    assert!(unrelated.join("blob").is_file());
}

#[test]
fn custody_created_by_the_journal_is_rechecked_before_any_removal() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let path = home.cache(".cache/tool", 4096);
    let outcomes = run(std::slice::from_ref(&path), false, &w, &mut |row| {
        let row: Value = aterm_json::from_str(row).unwrap();
        if row["action"] == Value::from("removing") {
            std::fs::write(
                path.join(atpkg::build_cache::registered::package::COORDINATOR),
                b"",
            )
            .unwrap();
        }
        true
    });
    assert!(outcomes[0].refused());
    assert_eq!(std::fs::read(path.join("blob")).unwrap(), vec![7u8; 4096]);
}

#[cfg(unix)]
#[test]
fn a_cache_with_a_bound_unix_socket_is_kept_whole() {
    use std::os::unix::fs::FileTypeExt as _;
    use std::os::unix::net::{UnixListener, UnixStream};

    let home = short_home();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let dir = home.cache(".cache/tool", 4096);
    let socket = dir.join("s");
    let listener = UnixListener::bind(&socket).expect("scratch socket");
    let (outcomes, rows) = free(std::slice::from_ref(&dir), false, &w);
    assert!(
        matches!(outcomes[0].verdict, Verdict::Refused(_)),
        "{outcomes:?}; cache exists={}",
        dir.exists()
    );
    assert_eq!(
        std::fs::read(dir.join("blob")).expect("kept cache"),
        vec![7u8; 4096]
    );
    assert!(
        std::fs::symlink_metadata(&socket)
            .expect("kept socket")
            .file_type()
            .is_socket()
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["action"], Value::from("refused"));
    let _client = UnixStream::connect(&socket).expect("socket still reachable");
    let _server = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("socket still accepts");
}

#[cfg(unix)]
#[test]
fn a_cache_with_a_fifo_is_kept_whole() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::FileTypeExt as _;

    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let dir = home.cache(".cache/tool", 4096);
    let fifo = dir.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("path");
    // SAFETY: the NUL-terminated path remains live and names an owned scratch entry.
    let created = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
    assert_eq!(created, 0, "{}", std::io::Error::last_os_error());
    let (outcomes, rows) = free(std::slice::from_ref(&dir), false, &w);
    assert!(
        matches!(outcomes[0].verdict, Verdict::Refused(_)),
        "{outcomes:?}"
    );
    assert_eq!(
        std::fs::read(dir.join("blob")).expect("kept cache"),
        vec![7u8; 4096]
    );
    assert!(
        std::fs::symlink_metadata(fifo)
            .expect("kept FIFO")
            .file_type()
            .is_fifo()
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["action"], Value::from("refused"));
}

#[cfg(unix)]
#[test]
fn a_socket_added_after_admission_is_kept_and_partial_allocation_is_actual() {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};

    let home = short_home();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let control = home.cache(".cache/control", 4096);
    let admitted = judge(&control, &w).expect("ordinary cache");
    let mut boundaries = 0;
    let removed = super::super::target::remove_unmanaged_tree(&control, admitted.id, &mut || {
        boundaries += 1;
        Ok(())
    })
    .expect("clean control");
    assert_eq!(removed.units, [control]);
    assert!(boundaries > 0);
    for inject_at in 1..=boundaries {
        let dir = home.cache(&format!(".cache/tool-{inject_at}"), 4096);
        let blob = dir.join("blob");
        let allocation = std::fs::symlink_metadata(&blob).expect("blob").blocks() * 512;
        let admitted = judge(&dir, &w).expect("ordinary cache");
        let mut calls = 0;
        let mut listener = None;
        let mut fixture_removed_payload = false;
        let removed = super::super::target::remove_unmanaged_tree(&dir, admitted.id, &mut || {
            calls += 1;
            if calls == inject_at {
                match std::fs::remove_file(&blob) {
                    Ok(()) => fixture_removed_payload = true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => panic!("replace fixture payload: {error}"),
                }
                listener = Some(UnixListener::bind(&blob).expect("replacement socket"));
            }
            Ok(())
        })
        .expect("removal reports what it retained");
        assert!(dir.is_dir(), "boundary {inject_at}");
        assert!(removed.units.is_empty());
        assert!(!removed.trouble.is_empty());
        assert_eq!(
            removed.bytes,
            if fixture_removed_payload {
                0
            } else {
                allocation
            }
        );
        assert!(
            std::fs::symlink_metadata(&blob)
                .expect("kept socket")
                .file_type()
                .is_socket()
        );
        let _client = UnixStream::connect(&blob).expect("socket still reachable");
        let _server = aterm_uds::accept(
            listener.as_ref().expect("injection ran"),
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("still accepts");
    }
}

#[cfg(unix)]
#[test]
fn regular_cache_file_allocation_needs_no_payload_read_permission() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let dir = home.cache(".cache/tool", 64 << 10);
    let blob = dir.join("blob");
    let bytes = std::fs::symlink_metadata(&blob).expect("blob").blocks() * 512;
    assert!(bytes > 0);
    std::fs::set_permissions(&blob, std::fs::Permissions::from_mode(0o000)).expect("no read bits");
    let (outcomes, rows) = free(std::slice::from_ref(&dir), false, &w);
    assert_eq!(
        outcomes[0].verdict,
        Verdict::Removed {
            bytes,
            partial: false
        }
    );
    assert!(!dir.exists());
    assert_eq!(
        rows.last().expect("completion")["bytes"],
        Value::from(bytes)
    );
}

#[cfg(unix)]
#[test]
fn a_nested_link_is_removed_without_reading_or_deleting_its_protected_target() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let candidate = home.cache(".cache/tool", 4096);
    let outside = home.cache("outside-protected", 2048);
    let marker = outside.join(atpkg::build_cache::registered::MARKER);
    std::fs::write(&marker, b"unsupported managed policy").unwrap();
    std::os::unix::fs::symlink(&outside, candidate.join("link")).unwrap();
    let (outcomes, _) = free(std::slice::from_ref(&candidate), false, &w);
    assert!(matches!(outcomes[0].verdict, Verdict::Removed { .. }));
    assert!(!candidate.exists());
    assert_eq!(
        std::fs::read(outside.join("blob")).unwrap(),
        vec![7u8; 2048]
    );
    assert_eq!(
        std::fs::read(marker).unwrap(),
        b"unsupported managed policy"
    );
}

#[cfg(unix)]
#[test]
fn revoked_index_permission_reports_actual_partial_bytes_and_keeps_remaining_payload() {
    use std::os::unix::fs::MetadataExt as _;
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let path = home.cache(".cache/tool", 64 << 10);
    let second = path.join("second");
    std::fs::write(&second, vec![9u8; 32 << 10]).unwrap();
    let files = [path.join("blob"), second];
    let blocks: Vec<_> = files
        .iter()
        .map(|file| std::fs::metadata(file).unwrap().blocks() * 512)
        .collect();
    let admitted = judge(&path, &w).unwrap();
    let clearance = atpkg::build_cache::registered::ordinary_clearance(&home.path, &path).unwrap();
    let mut permit = || {
        if files.iter().any(|file| !file.exists()) {
            let index = home.path.join(atpkg::build_cache::registered::INDEX);
            std::fs::create_dir_all(&index).unwrap();
            std::fs::write(index.join("new.json"), br#"{"schema":999}"#).unwrap();
        }
        if clearance.current() {
            Ok(())
        } else {
            Err("registry changed".to_owned())
        }
    };
    let removed = super::super::target::remove_unmanaged_tree(&path, admitted.id, &mut permit);
    let verdict = record_removal(&admitted, removed, &w, &mut |_| true, "fixture removal");
    let Verdict::Failed { freed, .. } = verdict else {
        panic!("{verdict:?}");
    };
    let expected: u64 = files
        .iter()
        .zip(blocks)
        .filter(|(file, _)| !file.exists())
        .map(|(_, bytes)| bytes)
        .sum();
    assert!(freed > 0);
    assert_eq!(freed, expected);
    assert_eq!(files.iter().filter(|file| file.exists()).count(), 1);
    for (file, byte, length) in [(&files[0], 7u8, 64 << 10), (&files[1], 9u8, 32 << 10)] {
        if file.exists() {
            assert_eq!(std::fs::read(file).unwrap(), vec![byte; length]);
        }
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
fn a_process_use_that_appears_after_intent_keeps_the_cache() {
    use std::cell::Cell;

    for how in ["cwd", "exe", "map", "fd"] {
        let home = Home::new();
        let mounts = whole();
        let dir = home.cache(".cache/tool", 4096);
        let after_intent = Cell::new(false);
        let processes = |dirs: &[PathBuf], _: &[u32]| {
            assert_eq!(dirs, std::slice::from_ref(&dir));
            let mut scan = PathScan::default();
            if after_intent.get() {
                scan.users.push(PathUse {
                    pid: 4242,
                    how,
                    path: dir.join("blob"),
                });
            }
            Ok(scan)
        };
        let w = world(&home, &processes, &mounts);
        let mut rows = Vec::new();
        let outcomes = run(std::slice::from_ref(&dir), false, &w, &mut |line| {
            let row = aterm_json::from_str::<Value>(line).expect("row");
            if row["action"].as_str() == Some("removing") {
                after_intent.set(true);
            }
            rows.push(row);
            true
        });
        assert!(
            refusal(&outcomes[0]).contains("4242"),
            "{how}: {outcomes:?}"
        );
        assert_eq!(
            std::fs::read(dir.join("blob")).expect("kept cache"),
            vec![7u8; 4096]
        );
        let actions: Vec<_> = rows
            .iter()
            .map(|row| row["action"].as_str().expect("action"))
            .collect();
        assert_eq!(actions, ["removing", "refused"], "{how}");
        assert_eq!(outcomes[0].bytes(), 0);
    }
}

#[test]
fn post_intent_process_discovery_must_still_rule_out_every_user() {
    use std::cell::Cell;

    for state in ["unavailable", "unreadable", "clear"] {
        let home = Home::new();
        let mounts = whole();
        let dir = home.cache(".cache/tool", 4096);
        let after_intent = Cell::new(false);
        let rechecked = Cell::new(false);
        let processes = |_: &[PathBuf], _: &[u32]| {
            if !after_intent.get() {
                return Ok(PathScan::default());
            }
            rechecked.set(true);
            match state {
                "unavailable" => Err(std::io::Error::other("late process scan unavailable")),
                "unreadable" => Ok(PathScan {
                    unreadable: vec![(4242, None)],
                    ..PathScan::default()
                }),
                _ => Ok(PathScan::default()),
            }
        };
        let w = world(&home, &processes, &mounts);
        let outcomes = run(std::slice::from_ref(&dir), false, &w, &mut |line| {
            let row = aterm_json::from_str::<Value>(line).expect("row");
            if row["action"].as_str() == Some("removing") {
                after_intent.set(true);
            }
            true
        });
        assert!(rechecked.get(), "a fresh scan followed the intent: {state}");
        if state == "clear" {
            assert!(matches!(outcomes[0].verdict, Verdict::Removed { .. }));
            assert!(!dir.exists());
        } else {
            assert!(
                matches!(outcomes[0].verdict, Verdict::Refused(_)),
                "{state}: {outcomes:?}"
            );
            assert_eq!(
                std::fs::read(dir.join("blob")).expect("kept cache"),
                vec![7u8; 4096]
            );
        }
    }
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
fn cleanup_intent_starts_a_separate_record_after_an_interrupted_journal_row() {
    use crate::supervise::journal::Journal;

    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let cache = home.cache(".cache/tool", 4096);
    let ledger = home.path.join("disk.jsonl");
    let interrupted = b"{\"interrupted\":";
    std::fs::write(&ledger, interrupted).expect("interrupted earlier write");
    let mut journal = Journal::open(Some(&ledger), Some(w.sid), &mut Vec::new());
    let outcomes = run(std::slice::from_ref(&cache), false, &w, &mut |row| {
        journal.append_raw(row, &mut Vec::new())
    });
    assert!(
        matches!(outcomes[0].verdict, Verdict::Removed { .. }),
        "{outcomes:?}"
    );
    assert!(!cache.exists());
    let bytes = std::fs::read(&ledger).expect("ledger");
    assert!(
        bytes.starts_with(interrupted),
        "prior evidence stays intact"
    );
    let actions: Vec<_> = bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| aterm_json::from_slice::<Value>(line).ok())
        .filter(|row| row["kind"] == Value::from(KIND))
        .map(|row| row["action"].as_str().expect("action").to_owned())
        .collect();
    assert_eq!(
        actions,
        ["removing", "removed"],
        "every accepted intent is its own JSON record"
    );
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

#[test]
fn an_unrecorded_completion_fails_the_command_but_keeps_actual_removal_facts() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let dir = home.cache(".cache/tool", 64 << 10);
    let bytes = judge(&dir, &w).expect("eligible").bytes;
    assert!(bytes > 0);
    let mut attempted = Vec::new();
    let mut recorded = Vec::new();
    let outcomes = run(std::slice::from_ref(&dir), false, &w, &mut |line| {
        let row = aterm_json::from_str::<Value>(line).expect("journal row");
        let accepted = row["action"].as_str() == Some("removing");
        attempted.push(row.clone());
        if accepted {
            recorded.push(row);
        }
        accepted
    });
    assert!(!dir.exists(), "the physical removal completed");
    assert_eq!(outcomes[0].verdict, Verdict::Unrecorded { bytes });
    assert_eq!(attempted.len(), 2);
    assert_eq!(attempted[0]["action"], Value::from("removing"));
    assert_eq!(attempted[1]["action"], Value::from("removed"));
    assert_eq!(recorded.len(), 1, "only the intent reached the ledger");
    assert!(outcomes[0].refused(), "the command must exit nonzero");
    assert_eq!(
        Summary::from_outcomes(&outcomes),
        Summary {
            successful: 0,
            refused: 1,
            bytes,
            bytes_partial: false
        }
    );
    let json = outcomes[0].to_json(false, w.grant());
    assert_eq!(json["removed"], Value::from(true));
    assert_eq!(json["bytes"], Value::from(bytes));
    assert_eq!(json["bytes_partial"], Value::from(false));
    assert!(json["refused"].as_str().is_some_and(|why| !why.is_empty()));
}

#[test]
fn a_partial_removal_keeps_both_the_removal_and_completion_record_failure() {
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let dir = home.cache(".cache/tool", 64 << 10);
    let admitted = judge(&dir, &w).expect("eligible");
    for journalled in [false, true] {
        let done = super::super::Removed {
            bytes: 7,
            trouble: vec!["retained child".to_owned()],
            ..super::super::Removed::default()
        };
        let mut attempted = Vec::new();
        let verdict = record_removal(
            &admitted,
            Ok(done),
            &w,
            &mut |line| {
                attempted.push(aterm_json::from_str::<Value>(line).expect("row"));
                journalled
            },
            "fixture removal",
        );
        let Verdict::Failed { freed, why } = verdict else {
            panic!("{verdict:?}");
        };
        assert_eq!(freed, 7);
        assert!(why.contains("retained child"));
        assert_eq!(why.contains(UNRECORDED_COMPLETION), !journalled);
        assert_eq!(attempted.len(), 1);
        assert_eq!(attempted[0]["action"], Value::from("failed"));
        assert_eq!(attempted[0]["bytes"], Value::from(7u64));
    }
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
    let o = Outcome {
        path: PathBuf::from("/h/.npm/x"),
        verdict: Verdict::Removed {
            bytes: 2048,
            partial: true,
        },
    };
    assert_eq!(o.to_json(false, true)["removed"], Value::from(true));
    assert_eq!(o.to_json(false, true)["bytes"], Value::from(2048u64));
    assert_eq!(o.to_json(false, true)["bytes_partial"], Value::from(true));
}

#[test]
fn summary_keeps_partial_counts_and_failed_removal_allocation() {
    let outcome = |verdict| Outcome {
        path: PathBuf::from("cache"),
        verdict,
    };
    let outcomes = [
        outcome(Verdict::Removed {
            bytes: 7,
            partial: false,
        }),
        outcome(Verdict::Would {
            bytes: 11,
            partial: true,
        }),
        outcome(Verdict::Failed {
            freed: 13,
            why: "retained part".to_owned(),
        }),
        outcome(Verdict::Refused("held".to_owned())),
    ];
    assert_eq!(
        Summary::from_outcomes(&outcomes),
        Summary {
            successful: 2,
            refused: 2,
            bytes: 31,
            bytes_partial: true,
        }
    );
}

#[test]
fn summary_marks_a_saturated_total_as_a_lower_bound() {
    let outcomes = [
        Outcome {
            path: PathBuf::from("cache-a"),
            verdict: Verdict::Removed {
                bytes: u64::MAX,
                partial: false,
            },
        },
        Outcome {
            path: PathBuf::from("cache-b"),
            verdict: Verdict::Failed {
                freed: 1,
                why: "retained part".to_owned(),
            },
        },
    ];
    let summary = Summary::from_outcomes(&outcomes);
    assert_eq!(summary.bytes, u64::MAX);
    assert!(summary.bytes_partial);
    assert_eq!((summary.successful, summary.refused), (1, 1));
    assert_eq!(Summary::from_outcomes(&[]), Summary::default());
    let exact = Summary::from_outcomes(&outcomes[..1]);
    assert_eq!(exact.bytes, u64::MAX);
    assert!(
        !exact.bytes_partial,
        "the largest exact count remains exact"
    );
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

/// WHAT A WORKER REMOVED IS ITS OWN ROWS' BYTES: another session's rows, a refusal and an intent
/// count for nothing, so the figure can differ from the volume's rise.
#[test]
fn removed_by_adds_a_sessions_own_removals() {
    let at = |sid: &str, action: &str, bytes: Option<u64>| {
        row(1, sid, Path::new("/c/x"), action, bytes, "words")
    };
    let rows = vec![
        at("s-a", "removing", Some(1000)),
        at("s-a", "removed", Some(10)),
        at("s-a", "failed", Some(5)),
        at("s-a", "refused", Some(70)),
        at("s-b", "removed", Some(100)),
        "not a row".to_string(),
    ];
    assert_eq!(removed_by(&rows, "s-a"), 15);
    assert_eq!(removed_by(&rows, "s-b"), 100);
    assert_eq!(removed_by(&rows, "s-c"), 0);
}
