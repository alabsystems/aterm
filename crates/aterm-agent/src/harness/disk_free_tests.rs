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
        if rel.starts_with(".cargo/") {
            self.cargo_protocol();
        }
        dir
    }

    fn cargo_protocol(&self) {
        for name in [".package-cache-mutate", ".package-cache"] {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            options
                .open(self.path.join(".cargo").join(name))
                .expect("Cargo protocol lock");
        }
    }

    /// [`Home::cache`], the folder `rel`'s first two components name (`.cache/<name>`) tagged a
    /// cache by its tool ([`CACHEDIR_TAG`]), so the pass may take it ([`evidence`]).
    #[cfg(not(windows))]
    fn tagged(&self, rel: &str, len: usize) -> PathBuf {
        let dir = self.cache(rel, len);
        let top: PathBuf = rel.split('/').take(2).collect();
        std::fs::write(
            self.path.join(top).join(CACHEDIR_TAG),
            b"Signature: 8a477f597d28d172789f06886806bc55\n# a test tool's cache\n",
        )
        .expect("a cache tag");
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn cargo_reader_with_no_open_crate_file_prevents_cleanup_until_its_guard_releases() {
    let home = Home::new();
    let dir = home.cache(".cargo/registry/src", 1024);
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let reader =
        aterm_toolchain::cargo_cache::SourceCacheGuard::try_acquire(&home.path.join(".cargo"))
            .unwrap();
    let (busy, rows) = free(std::slice::from_ref(&dir), false, &w);
    assert!(refusal(&busy[0]).contains("Cargo source-cache reclamation custody"));
    assert!(dir.join("blob").is_file());
    assert!(
        !rows
            .iter()
            .any(|row| row["action"] == Value::from("removing"))
    );
    let (preview, _) = free(std::slice::from_ref(&dir), true, &w);
    assert!(refusal(&preview[0]).contains("Cargo source-cache reclamation custody"));
    drop(reader);
    let mut held_during_removal = false;
    let removed = run(std::slice::from_ref(&dir), false, &w, &mut |row| {
        let row: Value = aterm_json::from_str(row).unwrap();
        if row["action"] == Value::from("removing") {
            held_during_removal = aterm_toolchain::cargo_cache::SourceCacheGuard::try_acquire(
                &home.path.join(".cargo"),
            )
            .is_err();
        }
        true
    });
    assert!(
        held_during_removal,
        "intent cannot precede real reader exclusion"
    );
    assert!(matches!(&removed[0].verdict, Verdict::Removed { .. }));
    assert!(!dir.exists());
    drop(
        aterm_toolchain::cargo_cache::SourceCacheGuard::try_acquire(&home.path.join(".cargo"))
            .unwrap(),
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn canonical_cargo_targets_keep_their_locks_through_home_and_registry_aliases() {
    for overlap in [false, true] {
        let home = Home::new();
        // Initialize the real existing Cargo lock protocol without an alias.
        home.cache(".cargo/git/checkouts", 0);
        let target = home.cache(
            if overlap {
                ".cache/real-registry/src"
            } else {
                "real-registry/src"
            },
            1024,
        );
        std::os::unix::fs::symlink(target.parent().unwrap(), home.path.join(".cargo/registry"))
            .unwrap();
        let alias_parent = aterm_tempfile::tempdir().unwrap();
        let alias = alias_parent.path().join("home");
        std::os::unix::fs::symlink(&home.path, &alias).unwrap();
        let mounts = whole();
        let mut w = world(&home, &nobody, &mounts);
        w.home = &alias;
        let admitted = judge(&target, &w).unwrap();
        assert_eq!(
            admitted.cargo_home.as_deref(),
            Some(home.path.join(".cargo").as_path())
        );
        let reader =
            aterm_toolchain::cargo_cache::SourceCacheGuard::try_acquire(&home.path.join(".cargo"))
                .unwrap();
        let (outcomes, _) = free(std::slice::from_ref(&target), false, &w);
        assert!(refusal(&outcomes[0]).contains("Cargo source-cache reclamation custody"));
        assert!(target.join("blob").is_file());
        drop(reader);
        let (outcomes, _) = free(std::slice::from_ref(&target), false, &w);
        assert!(matches!(&outcomes[0].verdict, Verdict::Removed { .. }));
        assert!(!target.exists());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_earlier_cache_root_does_not_admit_an_aliased_cargo_root_or_its_ancestor() {
    let home = Home::new();
    home.cache(".cargo/git/checkouts", 0);
    let source = home.cache(".cache/container/registry/src", 1024);
    let registry = source.parent().unwrap();
    std::os::unix::fs::symlink(registry, home.path.join(".cargo/registry")).unwrap();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    for target in [registry.to_path_buf(), home.path.join(".cache/container")] {
        let (outcomes, rows) = free(&[target], false, &w);
        assert!(refusal(&outcomes[0]).contains("contains a Cargo cache root"));
        assert!(
            !rows
                .iter()
                .any(|row| row["action"] == Value::from("removing"))
        );
        assert!(source.join("blob").is_file());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_automatic_pass_keeps_cargo_readers_before_staging_an_aliased_cache() {
    let home = Home::new();
    home.cache(".cargo/git/checkouts", 0);
    let cache = home.tagged(".cache/compiler-inputs", BIG);
    std::os::unix::fs::symlink(home.path.join(".cache"), home.path.join(".cargo/registry"))
        .unwrap();
    let mounts = whole();
    let now = clock() + 9 * DAY;
    let reader =
        aterm_toolchain::cargo_cache::SourceCacheGuard::try_acquire(&home.path.join(".cargo"))
            .unwrap();
    let (busy, rows) = swept(&at(&home, &nobody, &mounts, now), &in_place(), &mut || {
        SHORT
    });
    assert!(gone(&busy).is_empty());
    assert!(cache.join("blob").is_file());
    assert!(
        !rows
            .iter()
            .any(|row| row["action"] == Value::from("removing"))
    );
    assert!(busy.outcomes.iter().any(|outcome| {
        outcome.path == cache
            && matches!(&outcome.verdict, Verdict::Refused(why) if why.contains("Cargo source-cache reclamation custody"))
    }));
    drop(reader);
    let (done, _) = swept(
        &at(&home, &nobody, &mounts, now + 7200),
        &in_place(),
        &mut || SHORT,
    );
    assert_eq!(gone(&done), [cache.clone()]);
    assert!(!cache.exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn finishing_an_aliased_staged_cache_keeps_cargo_custody_through_completion() {
    let home = Home::new();
    home.cache(".cargo/git/checkouts", 0);
    let cache = home.cache(".cache/old-inputs", 1024);
    std::os::unix::fs::symlink(home.path.join(".cache"), home.path.join(".cargo/registry"))
        .unwrap();
    let id = aterm_sysprobe::volumes::file_id(&cache, false).unwrap();
    let staged = cache
        .parent()
        .unwrap()
        .join(staged_name(&cache, id).unwrap());
    std::fs::rename(&cache, &staged).unwrap();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let reader =
        aterm_toolchain::cargo_cache::SourceCacheGuard::try_acquire(&home.path.join(".cargo"))
            .unwrap();
    let busy = finish_staged(&w, &mut |_| true);
    assert!(refusal(&busy[0]).contains("Cargo source-cache reclamation custody"));
    assert!(staged.join("blob").is_file());
    drop(reader);
    let mut held = false;
    let done = finish_staged(&w, &mut |line| {
        let row: Value = aterm_json::from_str(line).unwrap();
        if row["action"] == Value::from("removed") {
            held = aterm_toolchain::cargo_cache::SourceCacheGuard::try_acquire(
                &home.path.join(".cargo"),
            )
            .is_err();
        }
        true
    });
    assert!(held, "Cargo custody must cover the completion journal");
    assert!(matches!(done[0].verdict, Verdict::Removed { .. }));
    assert!(!staged.exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_staged_whole_cargo_alias_with_a_broken_original_name_is_not_reclaimed() {
    let home = Home::new();
    home.cache(".cargo/git/checkouts", 0);
    let cache = home.cache(".cache/registry", 1024);
    std::os::unix::fs::symlink(&cache, home.path.join(".cargo/registry")).unwrap();
    let id = aterm_sysprobe::volumes::file_id(&cache, false).unwrap();
    let staged = cache
        .parent()
        .unwrap()
        .join(staged_name(&cache, id).unwrap());
    std::fs::rename(&cache, &staged).unwrap();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);
    let outcomes = finish_staged(&w, &mut |_| true);
    assert!(refusal(&outcomes[0]).contains("Cargo cache root cannot be resolved"));
    assert!(staged.join("blob").is_file());
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
    let verdict = record_removal(
        &admitted,
        &admitted.path,
        removed,
        &w,
        &mut |_| true,
        "fixture removal",
    );
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
    home.cargo_protocol();
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
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    assert!(
        matches!(outcomes[0].verdict, Verdict::Removed { .. }),
        "the root's real path is the root: {outcomes:?}"
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        assert!(refusal(&outcomes[0]).contains("Cargo source-cache reclamation custody"));
        assert!(inside.join("blob").is_file());
    }
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
    let dir = home.cache(".cache/tool", 32 * 1024);
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
            &admitted.path,
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

/// One day, in seconds.
#[cfg(not(windows))]
const DAY: i64 = 86_400;

/// The free bytes the pass's tests reclaim up to, and a free figure short of it.
#[cfg(not(windows))]
const GOAL: u64 = 100 << 30;
#[cfg(not(windows))]
const SHORT: Option<u64> = Some(1 << 30);

/// What a folder the pass may take holds: over its floor.
#[cfg(not(windows))]
const BIG: usize = 2 << 20;

/// A wall clock no test reaches: [`PASS_WALL`] is a budget of the product's, and a test
/// process stalled past it would be cut and fail on what it removed.
#[cfg(not(windows))]
const NO_WALL: Duration = Duration::from_secs(3600);

/// The hang detector of a wait for something that must happen.
#[cfg(not(windows))]
const HANG: Duration = Duration::from_secs(60);

/// [`world`] at the clock `now`.
#[cfg(not(windows))]
fn at<'a>(
    home: &'a Home,
    processes: discover::Processes<'a>,
    mounts: &'a Table,
    now: i64,
) -> World<'a> {
    World {
        now,
        ..world(home, processes, mounts)
    }
}

/// A dater for a pass that must date nothing off the look's thread: it fails the test.
#[cfg(not(windows))]
fn no_dater(path: &Path, _: &Reading) -> Dated {
    panic!("{} was dated through the guard", path.display())
}

/// The reads of a pass that dates its guarded children through `dater`, under no wall clock.
#[cfg(not(windows))]
fn dating(dater: Dater<'_>) -> Reads<'_> {
    Reads {
        dater,
        wall: NO_WALL,
        wait: HANG,
    }
}

/// The reads of a pass over `.cache` alone, whose children are read in place.
#[cfg(not(windows))]
fn in_place() -> Reads<'static> {
    dating(&no_dater)
}

/// One pass over `w` up to [`GOAL`], free space read through `measure`, with every row it
/// journalled.
#[cfg(not(windows))]
fn swept(
    w: &World<'_>,
    reads: &Reads<'_>,
    measure: &mut dyn FnMut() -> Option<u64>,
) -> (Passed, Vec<Value>) {
    let mut rows = Vec::new();
    let passed = pass(w, GOAL, reads, measure, &mut |r| {
        rows.push(aterm_json::from_str::<Value>(r).expect("a row is one JSON object"));
        true
    });
    (passed, rows)
}

/// The paths a pass removed, in the order it removed them.
#[cfg(not(windows))]
fn gone(passed: &Passed) -> Vec<PathBuf> {
    passed
        .outcomes
        .iter()
        .filter(|o| matches!(o.verdict, Verdict::Removed { .. }))
        .map(|o| o.path.clone())
        .collect()
}

/// Each row's `(action, path)`.
#[cfg(not(windows))]
fn acts(rows: &[Value]) -> Vec<(String, PathBuf)> {
    rows.iter()
        .map(|r| {
            (
                r["action"].as_str().expect("action").to_owned(),
                PathBuf::from(r["path"].as_str().expect("path")),
            )
        })
        .collect()
}

/// Unix seconds now: the `T` the dated fixtures are written at.
#[cfg(not(windows))]
fn clock() -> i64 {
    i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("the clock fits")
}

/// Set the modification time of the file or folder at `path` to `at` (unix seconds).
#[cfg(not(windows))]
fn stamp(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .expect("set the modification time");
}

/// The modification time of the entry at `path`, in unix seconds.
#[cfg(unix)]
fn modified(path: &Path) -> i64 {
    let at = std::fs::symlink_metadata(path)
        .and_then(|meta| meta.modified())
        .expect("a modification time");
    i64::try_from(
        at.duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs(),
    )
    .expect("fits")
}

/// A PATH'S LAST USE IS THE NEWEST WRITE OR CHANGE UNDER IT, read in the walk that counts its
/// bytes: a file nested three folders down, written later than everything above it, dates the
/// whole tree, and a file child is dated by itself. A tree whose every modification time was set
/// back a month, as `cp -a`, `rsync -a` and `tar` leave an unpacked tree, is still dated today,
/// by the change time setting those times left, and the pass, three days on, keeps it. NEGATIVE
/// CONTROL: the fixture's modification times do read a month old, so dating by modification
/// time alone (drop the change time from `target::written`) reads the unpacked tree a month old,
/// fails the assertion on its date, and the pass removes it.
#[cfg(unix)]
#[test]
fn judge_dates_a_path_by_the_newest_write_or_change_under_it() {
    let t = clock();
    let home = Home::new();
    let mounts = whole();
    let w = world(&home, &nobody, &mounts);

    let deep = home.tagged(".cache/tool/a/b", 4096);
    stamp(&deep.join("blob"), t + 3 * DAY);
    let tool = home.path.join(".cache/tool");
    let admitted = judge(&tool, &w).expect("an ordinary cache");
    assert_eq!(admitted.last_use, Some(t + 3 * DAY), "{admitted:?}");
    assert!(!admitted.partial);
    let file = judge(&deep.join("blob"), &w).expect("a file under a root");
    assert_eq!(file.last_use, Some(t + 3 * DAY), "{file:?}");

    let sub = home.tagged(".cache/unpacked/sub", BIG);
    let unpacked = home.path.join(".cache/unpacked");
    for entry in [sub.join("blob"), sub.clone(), unpacked.clone()] {
        stamp(&entry, t - 30 * DAY);
        assert_eq!(modified(&entry), t - 30 * DAY, "{}", entry.display());
    }
    let admitted = judge(&unpacked, &w).expect("an ordinary cache");
    assert!(
        admitted.last_use > Some(t - DAY) && admitted.last_use < Some(t + DAY),
        "set back a month, changed within the day: {admitted:?}"
    );

    let w = at(&home, &nobody, &mounts, t + 3 * DAY);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert!(passed.outcomes.is_empty(), "{passed:?}");
    assert!(rows.is_empty(), "{rows:?}");
    assert!(
        sub.join("blob").is_file(),
        "unpacked three days ago: it stays"
    );
    assert!(deep.join("blob").is_file(), "written today: it stays");
}

/// THE PASS KEEPS WHAT IS IN USE, BY ITS DATE OR BY A PROCESS: of three `.cache` folders nothing
/// else distinguishes, the one written a day ago and the one a process holds a file under stay,
/// with no row naming either, and the one unused for nine days goes. The window is
/// `[disk] idle_build_days`: under a one-day window a folder written a day ago goes, and with
/// the key below 1 there is no window, so a folder nine days unused stays and free space is not
/// measured. NEGATIVE CONTROL: an hour later, with no process under it, the held folder goes
/// too (so the scan kept it, not its date), and the written one still stays (so its date kept
/// it, not the scan); dropping the window from `pass` removes the written folder in the first
/// pass, and a pass that reads a constant seven days keeps the one-day folder.
#[cfg(not(windows))]
#[test]
fn the_pass_keeps_a_cache_folder_written_within_its_window_or_that_a_process_uses() {
    let t = clock();
    let home = Home::new();
    let mounts = whole();
    let written = home.tagged(".cache/written", BIG);
    stamp(&written.join("blob"), t + 8 * DAY);
    let used = home.tagged(".cache/used", BIG);
    let idle = home.tagged(".cache/idle", BIG);
    let holds = |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        let mut scan = PathScan::default();
        if dirs.contains(&used) {
            scan.users.push(PathUse {
                pid: 4242,
                how: "fd",
                path: used.join("blob"),
            });
        }
        Ok(scan)
    };
    let now = t + 9 * DAY;
    let w = at(&home, &holds, &mounts, now);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [idle.clone()], "{passed:?}");
    assert!(!idle.exists(), "unused for nine days: it goes");
    assert!(
        written.join("blob").is_file(),
        "written a day ago: it stays"
    );
    assert!(used.join("blob").is_file(), "a process uses it: it stays");
    assert_eq!(
        acts(&rows),
        [
            ("removing".to_owned(), idle.clone()),
            ("removed".to_owned(), idle.clone())
        ],
        "no row names a folder that stays"
    );
    assert_eq!(passed.cut, None);

    let w = at(&home, &nobody, &mounts, now + 3600);
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [used.clone()], "{passed:?}");
    assert!(!used.exists(), "no process uses it now: it goes");
    assert!(written.join("blob").is_file(), "still within its window");

    let home = Home::new();
    let recent = home.tagged(".cache/recent", BIG);
    stamp(&recent.join("blob"), t + 8 * DAY);
    let one_day = World {
        config: Config {
            idle_build_days: 1,
            ..on()
        },
        ..at(&home, &nobody, &mounts, now)
    };
    let (passed, _) = swept(&one_day, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [recent], "a one-day window: {passed:?}");

    let home = Home::new();
    let old = home.tagged(".cache/old", BIG);
    let no_window = World {
        config: Config {
            idle_build_days: 0,
            ..on()
        },
        ..at(&home, &nobody, &mounts, now)
    };
    let mut measured = 0;
    let (passed, rows) = swept(&no_window, &in_place(), &mut || {
        measured += 1;
        SHORT
    });
    assert_eq!(passed, Passed::default());
    assert!(rows.is_empty() && measured == 0);
    assert!(old.join("blob").is_file(), "no window: it stays");
}

/// THE LEDGER HOLDS EACH REMOVAL BEFORE IT STARTS: for each outside cache the pass takes, its
/// `disk-free` `removing` row, carrying the bytes counted, is written while the folder is still
/// whole, and its `removed` row once it is gone. NEGATIVE CONTROL: over a second home, a ledger
/// that cannot write the `removing` row leaves both folders (so the removal waits on the row);
/// writing the row after the removal fails the closure's own look at the folder.
#[cfg(not(windows))]
#[test]
fn the_pass_journals_each_outside_cache_before_it_removes_it() {
    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let first = home.tagged(".cache/first", BIG);
    let second = home.tagged(".cache/second", BIG);
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let mut rows = Vec::new();
    let passed = pass(&w, GOAL, &in_place(), &mut || SHORT, &mut |line| {
        let row = aterm_json::from_str::<Value>(line).expect("a row is one JSON object");
        let path = PathBuf::from(row["path"].as_str().expect("path"));
        match row["action"].as_str() {
            Some("removing") => assert!(
                path.join("blob").is_file(),
                "{} is whole when its intent is written",
                path.display()
            ),
            Some("removed") => assert!(!path.exists(), "{} is gone", path.display()),
            other => panic!("an unexpected row: {other:?}"),
        }
        rows.push(row);
        true
    });
    assert_eq!(
        acts(&rows),
        [
            ("removing".to_owned(), first.clone()),
            ("removed".to_owned(), first.clone()),
            ("removing".to_owned(), second.clone()),
            ("removed".to_owned(), second.clone()),
        ]
    );
    for row in &rows {
        assert_eq!(row["kind"], Value::from(KIND));
        assert_eq!(row["sid"], Value::from("s-worker"));
        assert!(
            row["bytes"].as_u64().is_some_and(|bytes| bytes >= 2 << 20),
            "{row:?}"
        );
    }
    assert_eq!(gone(&passed), [first, second]);
    assert!(passed.bytes >= 4 << 20, "{passed:?}");

    let home = Home::new();
    let first = home.tagged(".cache/first", BIG);
    let second = home.tagged(".cache/second", BIG);
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let mut tried = Vec::new();
    let passed = pass(&w, GOAL, &in_place(), &mut || SHORT, &mut |line| {
        tried.push(line.to_owned());
        !line.contains("\"removing\"")
    });
    assert!(first.join("blob").is_file() && second.join("blob").is_file());
    assert!(tried.iter().any(|line| line.contains("\"removing\"")));
    assert!(gone(&passed).is_empty(), "{passed:?}");
    assert_eq!(passed.bytes, 0);
    assert_eq!(passed.outcomes.len(), 2, "{passed:?}");
    assert!(
        passed
            .outcomes
            .iter()
            .all(|o| o.verdict == Verdict::Refused(UNJOURNALLED.to_owned())),
        "{passed:?}"
    );
}

/// A FOLDER WRITTEN AFTER THE PASS CHOSE IT STAYS: both folders are nine days old when the pass
/// dates them; a file is written into the second while the first is being removed, and the
/// judgment its `removing` row would be written from dates it again. NEGATIVE CONTROL: without
/// that second date check (remove on the first read's date) the second folder goes, with the
/// file just written into it.
#[cfg(not(windows))]
#[test]
fn a_folder_written_after_it_was_chosen_is_kept() {
    let t = clock();
    let home = Home::new();
    let mounts = whole();
    let first = home.tagged(".cache/first", BIG);
    let second = home.tagged(".cache/second", BIG);
    let now = t + 9 * DAY;
    let w = at(&home, &nobody, &mounts, now);
    let mut rows = Vec::new();
    let passed = pass(&w, GOAL, &in_place(), &mut || SHORT, &mut |line| {
        let row = aterm_json::from_str::<Value>(line).expect("a row is one JSON object");
        if row["action"].as_str() == Some("removed") {
            let new = second.join("new");
            std::fs::write(&new, b"written while the pass ran").expect("a late write");
            stamp(&new, now);
        }
        rows.push(row);
        true
    });
    assert_eq!(gone(&passed), [first.clone()], "{passed:?}");
    assert!(second.join("blob").is_file() && second.join("new").is_file());
    assert_eq!(
        acts(&rows),
        [
            ("removing".to_owned(), first.clone()),
            ("removed".to_owned(), first)
        ],
        "no intent row names the folder written since"
    );
}

/// The three folders of the order test under `home`, each tagged a cache: `zz-old` and
/// trust-cg's store written at `t`, `aa-new` a day later and twice the size.
#[cfg(not(windows))]
fn ordered(home: &Home, t: i64) -> [PathBuf; 3] {
    let old = home.tagged(".cache/zz-old", BIG);
    let new = home.tagged(".cache/aa-new", 2 * BIG);
    stamp(&new.join("blob"), t + DAY);
    let store = home.tagged(".cache/trust-cg", BIG);
    [old, new, store]
}

/// LEAST RECENTLY USED FIRST, TRUST'S STORE NEVER, AND NO FURTHER THAN THE TARGET: with free
/// space back at the target once the oldest folder is gone, only it goes; with free space short
/// throughout, the order is the oldest, then the newer (which is the larger and sorts first by
/// name), and trust-cg's store stays, though it is tagged and as old as the oldest: its proof
/// certificates are Trust's to evict. Where the free figure does not move (a snapshot holds the
/// bytes), the pass stops once what it removed covers the 3 MiB it first measured short, and
/// says so, and the folder after them stays. NEGATIVE CONTROL: a free figure that cannot be read removes nothing and says the
/// pass was cut; ordering largest first or by name takes `aa-new` first; dropping trust-cg from
/// `PASS_KEPT` takes the store; and the verb, given the store, removes it, so every fence admits
/// it and it is the pass's own rule that keeps it.
#[cfg(not(windows))]
#[test]
fn the_pass_takes_the_least_recently_written_first_keeps_trusts_store_and_stops_at_its_target() {
    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let [old, new, store] = ordered(&home, t);
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);

    let (passed, rows) = swept(&w, &in_place(), &mut || None);
    assert!(passed.outcomes.is_empty() && rows.is_empty(), "{passed:?}");
    assert_eq!(passed.cut.as_deref(), Some(CUT_UNMEASURED));
    assert!(old.exists() && new.exists() && store.exists());

    let (passed, _) = swept(&w, &in_place(), &mut || {
        if old.exists() { SHORT } else { Some(GOAL) }
    });
    assert_eq!(gone(&passed), [old.clone()], "{passed:?}");
    assert_eq!(passed.cut, None, "it stopped at its target");
    assert!(new.join("blob").is_file() && store.join("blob").is_file());

    let home = Home::new();
    let [old, new, store] = ordered(&home, t);
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [old, new], "{passed:?}");
    assert_eq!(passed.cut, None, "it reached every folder");
    assert!(store.join("blob").is_file(), "Trust's store stays");
    let (outcomes, _) = free(std::slice::from_ref(&store), false, &w);
    assert!(
        matches!(outcomes[0].verdict, Verdict::Removed { .. }),
        "{outcomes:?}"
    );

    let home = Home::new();
    let [old, new, store] = ordered(&home, t);
    let later = home.tagged(".cache/later", BIG);
    stamp(&later.join("blob"), t + 2 * DAY);
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let (passed, _) = swept(&w, &in_place(), &mut || Some(GOAL - (3 << 20)));
    assert_eq!(gone(&passed), [old, new], "{passed:?}");
    assert_eq!(passed.cut.as_deref(), Some(CUT_COUNTED));
    assert!(later.join("blob").is_file(), "covered: it stays");
    assert!(store.join("blob").is_file(), "it stays");
}

/// THE PASS REACHES TWO ROOTS, AND NOT EVERYTHING UNDER THEM: nine days unused and over the
/// floor, a child of `~/.npm`, one of `~/.cargo/registry`, aterm's own folder and a campaign
/// scope's folder under `~/.cache` all stay, as do a lock folder too small to move the free
/// figure and a folder with a mount point under it, while an ordinary `.cache` child beside
/// them goes and no row names a folder that stays. NEGATIVE CONTROL: the verb, given the `.npm`
/// child and the campaign scopes' folder, removes each, so every fence admits them and it is
/// the pass's own reach that left them; and an hour later, under a mount table that holds no
/// mount, the pass takes the folder the mount kept.
#[cfg(not(windows))]
#[test]
fn the_pass_reaches_only_the_roots_and_folders_it_may() {
    let t = clock();
    let home = Home::new();
    let mounts = whole();
    let npm = home.cache(".npm/x", BIG);
    let registry = home.cache(".cargo/registry/x", BIG);
    let ours = home.tagged(".cache/aterm", BIG);
    let scope = home.tagged(".cache/alab-release-scope/s1", BIG);
    std::fs::write(scope.join("receipt.json"), b"{}").expect("a receipt");
    let slot = home.tagged(".cache/build-slot", 4096);
    let ordinary = home.tagged(".cache/ordinary", BIG);
    let vm = home.tagged(".cache/vm", BIG);
    let mut mounted = whole();
    mounted.mounts.push(Mount {
        on: vm.join("disk"),
        from: "/dev/disk9s1".to_owned(),
        fstype: "apfs".to_owned(),
        local: true,
        automounted: false,
        removable: true,
        read_only: false,
        container: None,
    });
    let w = at(&home, &nobody, &mounted, t + 9 * DAY);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [ordinary.clone()], "{passed:?}");
    assert_eq!(
        acts(&rows),
        [
            ("removing".to_owned(), ordinary.clone()),
            ("removed".to_owned(), ordinary.clone())
        ]
    );
    for kept in [&npm, &registry, &ours, &scope, &slot, &vm] {
        assert!(kept.join("blob").is_file(), "{} stays", kept.display());
    }
    assert!(scope.join("receipt.json").is_file());

    let scopes = home.path.join(".cache/alab-release-scope");
    let (outcomes, _) = free(&[npm.clone(), scopes.clone()], false, &w);
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o.verdict, Verdict::Removed { .. })),
        "{outcomes:?}"
    );
    assert!(!npm.exists() && !scopes.exists());

    let w = at(&home, &nobody, &mounts, t + 9 * DAY + 3600);
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [vm], "no mount under it now: {passed:?}");
}

/// THE PATH FENCES COME BEFORE ANY READ UNDER A CHILD: a child that is a link, a child of a root
/// that is a link, and any child while the mount table is unread are each refused by `read` in
/// `judge`'s own words, and a read whose time is up answers with a partial walk. A pass over a
/// home whose `.cache` is a link removes nothing, writes nothing and measures nothing. A pass
/// whose mount table was not read whole, or not read at all, reads no folder and measures
/// nothing, and says it was cut: it could not look, so it has not run out. NEGATIVE CONTROL:
/// dating a child before `placed` (the walk alone) answers each of these with a size and a date
/// instead of the refusal, and the pass removes the folder the linked root reaches; the same
/// home under a table read whole loses its folder; a walk that ignores its deadline reads the
/// folder whole.
#[cfg(not(windows))]
#[test]
fn read_refuses_before_it_reads_under_a_path() {
    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let reading = |mounts: Option<&Table>| Reading {
        home: home.path.clone(),
        mounts: mounts.cloned(),
        until: Instant::now() + NO_WALL,
        wait: HANG,
    };
    let far = home.cache("far", BIG);
    let cache = home.path.join(".cache");
    std::fs::create_dir_all(&cache).expect("root");
    let link = cache.join("tool");
    atpkg::platform::atomic_symlink(&far, &link).expect("a link");
    let refused = read(&link, &reading(Some(&mounts))).expect_err("a link is refused");
    assert!(refused.contains("symbolic link"), "{refused}");
    assert_eq!(Err(refused), judge(&link, &w).map(|_| ()));

    let plain = home.tagged(".cache/plain", BIG);
    let refused = read(&plain, &reading(None)).expect_err("no mount table, no read");
    assert!(refused.contains("not read whole"), "{refused}");
    let dated = read(&plain, &reading(Some(&mounts))).expect("an ordinary child is read");
    assert!(
        dated.bytes >= 2 << 20 && dated.newest.is_some(),
        "{dated:?}"
    );
    assert!(!dated.partial, "{dated:?}");
    let out_of_time = Reading {
        until: Instant::now(),
        ..reading(Some(&mounts))
    };
    let cut_short = read(&plain, &out_of_time).expect("its place admits it");
    assert!(cut_short.partial, "{cut_short:?}");

    let unread = Table {
        complete: false,
        ..Table::default()
    };
    for table in [Some(&unread), None] {
        let blind = World {
            mounts: table,
            ..at(&home, &nobody, &mounts, t + 9 * DAY)
        };
        let mut measured = 0;
        let (passed, rows) = swept(&blind, &in_place(), &mut || {
            measured += 1;
            SHORT
        });
        assert_eq!(passed.cut.as_deref(), Some(CUT_MOUNTS), "{passed:?}");
        assert!(passed.outcomes.is_empty() && rows.is_empty(), "{passed:?}");
        assert_eq!(measured, 0, "nothing is measured");
        assert!(plain.join("blob").is_file());
    }
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [plain], "{passed:?}");

    let linked = Home::new();
    let behind = linked.tagged("real-cache/tool", BIG);
    atpkg::platform::atomic_symlink(&linked.path.join("real-cache"), &linked.path.join(".cache"))
        .expect("a linked root");
    let w = at(&linked, &nobody, &mounts, t + 9 * DAY);
    let through = linked.path.join(".cache/tool");
    let refused = read(
        &through,
        &Reading {
            home: linked.path.clone(),
            mounts: Some(mounts.clone()),
            until: Instant::now() + NO_WALL,
            wait: HANG,
        },
    )
    .expect_err("a child of a linked root is refused");
    assert!(refused.contains("reached through a link"), "{refused}");
    assert_eq!(Err(refused), judge(&through, &w).map(|_| ()));
    let mut measured = 0;
    let (passed, rows) = swept(&w, &in_place(), &mut || {
        measured += 1;
        SHORT
    });
    assert_eq!(passed, Passed::default());
    assert!(rows.is_empty(), "{rows:?}");
    assert_eq!(measured, 0, "nothing to read, nothing measured");
    assert!(behind.join("blob").is_file(), "what the root reaches stays");
}

/// Wait, under a one-minute hang detector, for the read of `path` to have returned.
#[cfg(not(windows))]
fn returned(path: &Path) {
    let hang = Instant::now() + HANG;
    while locked(&HELD).contains_key(path) {
        assert!(
            Instant::now() < hang,
            "the read of {} never returned",
            path.display()
        );
        aterm_uds::deadline::poll_yield();
    }
}

/// A FIRST READ THAT DOES NOT RETURN IS SKIPPED, SAID ONCE AND NOT MADE AGAIN WHILE IT IS
/// PENDING: a read blocked as a privacy dialog blocks one times out after its wait, and while
/// it is pending a second read of the same folder is not started and a pass dates no child of
/// its root (a `.cache` child beside it still goes). Such a pass says it was cut while the
/// pending read's own walk has time left, since a later look can read the root; once that
/// walk's time is up only a person can end the wait, and the pass is not cut. The answer of a
/// read the look stopped waiting for is kept: the next pass takes it and does not read that
/// folder again. A pass whose dater times out journals one `refused` row for that child,
/// removes nothing of that root, dates no further guarded child and says it was cut.
/// NEGATIVE CONTROL: once the blocked read is let go, the same call answers with its reading
/// and a pass reads that root again (so it was the pending read that held it); without the
/// kept answer that pass dates both children; a later read of the folder that blocks again
/// owes no second row.
#[cfg(not(windows))]
#[test]
fn a_read_that_does_not_return_is_skipped_said_once_and_not_made_again_while_it_is_pending() {
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let first = home.cache("Library/Caches/go-build", BIG);
    let second = home.cache("Library/Caches/pip", BIG);
    let beside = home.tagged(".cache/beside", BIG);
    let nothing = CacheUse {
        bytes: 0,
        partial: false,
        newest: None,
    };
    let reads_made = Arc::new(AtomicUsize::new(0));
    let blocked = |made: &Arc<AtomicUsize>| {
        let made = Arc::clone(made);
        let (let_go, held) = std::sync::mpsc::channel::<()>();
        let (began, begun) = std::sync::mpsc::channel::<()>();
        let look = move |_: &Path| {
            made.fetch_add(1, Ordering::SeqCst);
            let _ = began.send(());
            let _ = held.recv();
            Ok::<CacheUse, String>(nothing)
        };
        (let_go, begun, look)
    };
    let (let_go, begun, look) = blocked(&reads_made);
    let wait = Duration::from_millis(1);
    let walking = Instant::now() + NO_WALL;
    assert_eq!(
        date_off_thread(&first, wait, walking, look),
        Dated::TimedOut { said: false }
    );
    // The read's thread may not have run within the wait: its count is read once it began.
    begun.recv_timeout(HANG).expect("the blocked read began");
    let (_unused, _never, look) = blocked(&reads_made);
    assert_eq!(
        date_off_thread(&first, wait, walking, look),
        Dated::StillHeld
    );
    assert_eq!(reads_made.load(Ordering::SeqCst), 1, "one read, pending");

    let dated = Cell::new(0);
    let counting = |_: &Path, _: &Reading| {
        dated.set(dated.get() + 1);
        Dated::At(Err("a test's dater reads nothing".to_owned()))
    };
    let counted = dating(&counting);
    let now = t + 9 * DAY;
    let w = at(&home, &nobody, &mounts, now);
    let (passed, _) = swept(&w, &counted, &mut || SHORT);
    assert_eq!(dated.get(), 0, "no child of a root with a read pending");
    assert_eq!(gone(&passed), [beside], "the other root is still read");
    assert_eq!(
        passed.cut.as_deref(),
        Some(CUT_PENDING),
        "the root was not read, and a later look can read it"
    );
    assert!(first.join("blob").is_file() && second.join("blob").is_file());

    let_go.send(()).expect("the blocked read is waiting");
    returned(&first);
    assert_eq!(
        date_off_thread(&first, HANG, walking, move |_: &Path| Ok(nothing)),
        Dated::At(Ok(nothing))
    );
    let (passed, rows) = swept(&w, &counted, &mut || SHORT);
    assert_eq!(
        dated.get(),
        1,
        "the answer the first look did not wait for stands for `first`; only `second` is dated"
    );
    assert_eq!(passed.cut, None, "{passed:?}");
    assert!(passed.outcomes.is_empty(), "{passed:?}");
    assert_eq!(acts(&rows), [("refused".to_owned(), second.clone())]);
    let (let_go, _begun, look) = blocked(&reads_made);
    assert_eq!(
        date_off_thread(&first, wait, walking, look),
        Dated::TimedOut { said: true },
        "said once a process"
    );
    let_go.send(()).expect("the blocked read is waiting");
    returned(&first);

    let home = Home::new();
    let first = home.cache("Library/Caches/go-build", BIG);
    let (let_go, _begun, look) = blocked(&reads_made);
    assert_eq!(
        date_off_thread(&first, wait, Instant::now(), look),
        Dated::TimedOut { said: false }
    );
    let w = at(&home, &nobody, &mounts, now);
    let (passed, rows) = swept(&w, &counted, &mut || SHORT);
    assert_eq!(dated.get(), 1, "its root is still not read");
    assert_eq!(
        passed.cut, None,
        "a read held past its own walk's time is out of reach, not unread"
    );
    assert!(passed.outcomes.is_empty() && rows.is_empty(), "{passed:?}");
    let_go.send(()).expect("the blocked read is waiting");
    returned(&first);

    let home = Home::new();
    let first = home.cache("Library/Caches/go-build", BIG);
    let second = home.cache("Library/Caches/pip", BIG);
    let dated = Cell::new(0);
    let timing_out = |_: &Path, _: &Reading| {
        dated.set(dated.get() + 1);
        Dated::TimedOut { said: false }
    };
    let w = at(&home, &nobody, &mounts, now);
    let (passed, rows) = swept(&w, &dating(&timing_out), &mut || SHORT);
    assert_eq!(dated.get(), 1, "no guarded child is dated after a wait");
    assert_eq!(passed.held, [first.clone()]);
    assert_eq!(
        passed.cut.as_deref(),
        Some(CUT_HELD),
        "the folders after it were not read"
    );
    assert!(passed.outcomes.is_empty(), "{passed:?}");
    assert_eq!(acts(&rows), [("refused".to_owned(), first.clone())]);
    assert_eq!(rows[0]["text"], Value::from(HELD_READ));
    assert!(first.join("blob").is_file() && second.join("blob").is_file());
}

/// A GUARDED FOLDER A PATH FENCE REFUSES IS LEFT WITH ONE `refused` ROW, through the dater the
/// product ships: a link under `~/Library/Caches` is refused by its read, on the read's own
/// thread, and the pass writes one row with the fence's words; an hour later, read again, it
/// owes no second row. A link under `~/.cache`, which is read in place, is left with no row,
/// and an ordinary `Library/Caches` folder beside them is dated by the shipped dater and goes.
/// NEGATIVE CONTROL: without the once-a-process memo the second pass writes the row again;
/// with the row unwritten the first pass's rows are the ordinary folder's two alone.
#[cfg(not(windows))]
#[test]
fn a_guarded_folder_a_fence_refuses_is_said_once_through_the_shipped_dater() {
    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let far = home.cache("far", BIG);
    let ordinary = home.cache("Library/Caches/Homebrew", BIG);
    let guarded = home.path.join("Library/Caches/Yarn");
    atpkg::platform::atomic_symlink(&far, &guarded).expect("a link under the guarded root");
    std::fs::create_dir_all(home.path.join(".cache")).expect("root");
    let plain = home.path.join(".cache/linked");
    atpkg::platform::atomic_symlink(&far, &plain).expect("a link under `.cache`");
    let shipped = dating(&date_within);
    let now = t + 9 * DAY;
    let w = at(&home, &nobody, &mounts, now);
    let (passed, rows) = swept(&w, &shipped, &mut || SHORT);
    assert_eq!(gone(&passed), [ordinary.clone()], "{passed:?}");
    assert_eq!(passed.cut, None, "{passed:?}");
    assert_eq!(
        acts(&rows),
        [
            ("removing".to_owned(), ordinary.clone()),
            ("removed".to_owned(), ordinary),
            ("refused".to_owned(), guarded.clone()),
        ]
    );
    assert!(
        rows[2]["text"]
            .as_str()
            .is_some_and(|why| why.contains("symbolic link")),
        "{rows:?}"
    );
    assert_eq!(passed.refused.len(), 1, "{passed:?}");

    let w = at(&home, &nobody, &mounts, now + 3600);
    let (passed, rows) = swept(&w, &shipped, &mut || SHORT);
    assert!(passed.refused.is_empty() && rows.is_empty(), "{passed:?}");
    assert!(far.join("blob").is_file(), "what the links reach stays");
}

/// A PARTIAL WALK, OR A FOLDER NO TIME WAS READ OF, STAYS: a guarded folder whose read came
/// back cut short is kept though the newest time it saw is nine days old, since what the walk
/// did not reach may be newer, and so is one whose read saw no time at all; neither is named
/// in a row, and neither cuts the pass. NEGATIVE CONTROL: the same folder, an hour on, read
/// whole with that date, goes; taking a partial walk at its word removes it in the first pass.
#[cfg(not(windows))]
#[test]
fn a_partial_walk_or_an_undated_folder_stays() {
    use std::cell::Cell;

    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let tool = home.cache("Library/Caches/node-gyp", BIG);
    let answer = Cell::new(CacheUse {
        bytes: 2 << 20,
        partial: true,
        newest: Some(t),
    });
    let answering = |_: &Path, _: &Reading| Dated::At(Ok(answer.get()));
    let reads = dating(&answering);
    let now = t + 9 * DAY;
    for (hour, used) in [
        (0, answer.get()),
        (
            1,
            CacheUse {
                bytes: 2 << 20,
                partial: false,
                newest: None,
            },
        ),
    ] {
        answer.set(used);
        let w = at(&home, &nobody, &mounts, now + hour * 3600);
        let (passed, rows) = swept(&w, &reads, &mut || SHORT);
        assert!(passed.outcomes.is_empty() && rows.is_empty(), "{passed:?}");
        assert_eq!(passed.cut, None, "{passed:?}");
        assert!(tool.join("blob").is_file(), "{used:?}: it stays");
    }
    answer.set(CacheUse {
        bytes: 2 << 20,
        partial: false,
        newest: Some(t),
    });
    let w = at(&home, &nobody, &mounts, now + 2 * 3600);
    let (passed, _) = swept(&w, &reads, &mut || SHORT);
    assert_eq!(gone(&passed), [tool], "whole, nine days old: {passed:?}");
}

/// A FOLDER A PROCESS TAKES UP AFTER IT WAS CHOSEN IS REFUSED ON RECORD: the pass's scan finds
/// nobody under the folder, its `removing` row is written, and the scan the removal makes just
/// before it starts finds a process holding a file there. The folder stays whole, and the
/// ledger holds its `removing` row, then a `refused` row with the process named. NEGATIVE
/// CONTROL: without the pass's own `refused` row the intent would stand alone, read as a
/// removal that never finished; without the removal's second scan the folder goes.
#[cfg(not(windows))]
#[test]
fn a_folder_a_process_takes_up_after_it_was_chosen_is_refused_on_record() {
    use std::cell::Cell;

    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let folder = home.tagged(".cache/folder", BIG);
    let scans = Cell::new(0);
    let taken_up = |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        scans.set(scans.get() + 1);
        let mut scan = PathScan::default();
        if scans.get() > 1 {
            scan.users.push(PathUse {
                pid: 4242,
                how: "fd",
                path: dirs[0].join("blob"),
            });
        }
        Ok(scan)
    };
    let w = at(&home, &taken_up, &mounts, t + 9 * DAY);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(scans.get(), 2, "the pass's scan, then the removal's own");
    assert!(folder.join("blob").is_file(), "in use now: it stays");
    assert_eq!(
        acts(&rows),
        [
            ("removing".to_owned(), folder.clone()),
            ("refused".to_owned(), folder.clone())
        ]
    );
    assert!(
        rows[1]["text"]
            .as_str()
            .is_some_and(|why| why.contains("process 4242")),
        "{rows:?}"
    );
    assert_eq!(passed.bytes, 0);
    assert_eq!(passed.outcomes.len(), 1, "{passed:?}");
    assert!(passed.outcomes[0].refused(), "{passed:?}");
}

/// THE PASS'S WORK IS BOUNDED. Its dating stops at its wall clock: with none, an old folder
/// stays and the pass says it was cut. A folder read fresh is not read again until its window
/// could have passed: dated once nine days on, not at all the day after, and again once its
/// last write is seven days old, when it goes. One scan of the running processes answers for
/// every folder a pass chose: three a process uses stay on that one scan, with no judgment and
/// no row, and an hour later, nobody under them, they go on one scan more for each. And it
/// judges at most eight folders it then keeps: of nine that hold a managed cache's marker,
/// eight are judged, none goes, no row is written, and the pass says it was cut; the next
/// pass, the same second, reads only the ninth, since a folder judged and kept is not read
/// again for an hour. NEGATIVE CONTROL: with a wall clock the old folder goes; without the
/// fresh memo the fresh folder is dated at every look; a scan for each judgment makes the
/// three folders' pass scan three times; without the bound the first pass over the nine is not
/// cut, and without the kept memo the second chooses all nine again.
#[cfg(not(windows))]
#[test]
fn the_pass_bounds_its_work() {
    use std::cell::{Cell, RefCell};

    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let old = home.tagged(".cache/old", BIG);
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let none = Reads {
        wall: Duration::ZERO,
        ..in_place()
    };
    let (passed, rows) = swept(&w, &none, &mut || SHORT);
    assert_eq!(passed.cut.as_deref(), Some(CUT_WALL), "{passed:?}");
    assert!(rows.is_empty() && old.join("blob").is_file());
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [old]);

    let home = Home::new();
    let tool = home.cache("Library/Caches/node-gyp", BIG);
    stamp(&tool.join("blob"), t + 8 * DAY);
    let dated = Cell::new(0);
    let counting = |path: &Path, r: &Reading| {
        dated.set(dated.get() + 1);
        Dated::At(read(path, r))
    };
    let counted = dating(&counting);
    for (day, dates, there) in [(9, 1, true), (10, 1, true), (16, 2, false)] {
        let w = at(&home, &nobody, &mounts, t + day * DAY);
        let (passed, _) = swept(&w, &counted, &mut || SHORT);
        assert_eq!(dated.get(), dates, "day {day}");
        assert_eq!(tool.exists(), there, "day {day}: {passed:?}");
    }

    let home = Home::new();
    let used: Vec<PathBuf> = (1..=3)
        .map(|i| home.tagged(&format!(".cache/used-{i}"), BIG))
        .collect();
    let scans = Cell::new(0);
    let all_used = |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        scans.set(scans.get() + 1);
        Ok(PathScan {
            users: dirs
                .iter()
                .map(|dir| PathUse {
                    pid: 4242,
                    how: "cwd",
                    path: dir.clone(),
                })
                .collect(),
            ..PathScan::default()
        })
    };
    let w = at(&home, &all_used, &mounts, t + 9 * DAY);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(scans.get(), 1, "one scan answers for all three");
    assert!(passed.outcomes.is_empty() && rows.is_empty(), "{passed:?}");
    assert_eq!(passed.cut, None, "a folder in use is no judgment kept");
    assert!(used.iter().all(|f| f.join("blob").is_file()));
    let none_used = |dirs: &[PathBuf], skip: &[u32]| -> std::io::Result<PathScan> {
        scans.set(scans.get() + 1);
        nobody(dirs, skip)
    };
    let w = at(&home, &none_used, &mounts, t + 9 * DAY + 3600);
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed).len(), 3, "{passed:?}");
    assert_eq!(scans.get(), 1 + 1 + 3, "the pass's, then one a removal");

    let home = Home::new();
    let marker = atpkg::build_cache::registered::CUSTODY_MARKERS[0];
    let managed: Vec<PathBuf> = (1..=9)
        .map(|i| {
            let folder = home.tagged(&format!(".cache/managed-{i}"), BIG);
            std::fs::write(folder.join(marker), br#"{"schema":999}"#).expect("a marker");
            folder
        })
        .collect();
    let chosen = RefCell::new(Vec::new());
    let counting = |dirs: &[PathBuf], skip: &[u32]| -> std::io::Result<PathScan> {
        chosen.borrow_mut().push(dirs.len());
        nobody(dirs, skip)
    };
    let w = at(&home, &counting, &mounts, t + 9 * DAY);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert!(passed.outcomes.is_empty() && rows.is_empty(), "{passed:?}");
    assert_eq!(passed.cut.as_deref(), Some(CUT_KEPT), "{passed:?}");
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert!(passed.outcomes.is_empty() && rows.is_empty(), "{passed:?}");
    assert_eq!(passed.cut, None, "the ninth is kept: {passed:?}");
    assert_eq!(
        *chosen.borrow(),
        [9, 1],
        "the eight judged and kept are not read again the same second"
    );
    assert!(managed.iter().all(|f| f.join("blob").is_file()));
}

/// THE UNGUARDED ROOT IS READ FIRST: with two folders under `~/.cache` and one under
/// `~/Library/Caches`, the guarded folder's dater runs after both `.cache` folders were read,
/// which the free figure measured before each read counts, so no guarded wait spends the
/// pass's wall clock before `.cache`. NEGATIVE CONTROL: with `Library/Caches` listed first the
/// dater runs on the pass's first measure.
#[cfg(not(windows))]
#[test]
fn the_pass_reads_the_unguarded_root_before_the_guarded_one() {
    use std::cell::Cell;

    let t = clock();
    let mounts = whole();
    let home = Home::new();
    let guarded = home.cache("Library/Caches/ccache", BIG);
    let a = home.tagged(".cache/a", BIG);
    let b = home.tagged(".cache/b", BIG);
    let measured = Cell::new(0);
    let dated_at = Cell::new(0);
    let dater = |path: &Path, r: &Reading| {
        dated_at.set(measured.get());
        Dated::At(read(path, r))
    };
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let (passed, _) = swept(&w, &dating(&dater), &mut || {
        measured.set(measured.get() + 1);
        SHORT
    });
    assert_eq!(dated_at.get(), 3, "after both `.cache` reads");
    assert_eq!(gone(&passed).len(), 3, "{passed:?}");
    assert!(!guarded.exists() && !a.exists() && !b.exists());
}

/// THE GRANT IS THE PASS'S OFF SWITCH: with `[disk] auto_free_gib = 0` an old folder stays,
/// nothing is journalled and free space is not measured. NEGATIVE CONTROL: the same tree with
/// the grant on loses the folder.
#[cfg(not(windows))]
#[test]
fn the_pass_removes_nothing_while_the_grant_is_off() {
    let t = clock();
    let home = Home::new();
    let mounts = whole();
    let old = home.tagged(".cache/old", BIG);
    let on = at(&home, &nobody, &mounts, t + 9 * DAY);
    let off = World {
        config: Config {
            auto_free_gib: 0,
            ..Config::default()
        },
        ..at(&home, &nobody, &mounts, t + 9 * DAY)
    };
    let mut measured = 0;
    let (passed, rows) = swept(&off, &in_place(), &mut || {
        measured += 1;
        SHORT
    });
    assert_eq!(passed, Passed::default());
    assert!(rows.is_empty() && measured == 0);
    assert!(old.join("blob").is_file());

    let (passed, _) = swept(&on, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [old.clone()]);
    assert!(!old.exists());
}

/// THE PASS TAKES PROVEN CACHES ALONE: of idle `.cache` folders over the floor, a build tool's
/// own cache by its name (`pip`) and a folder its tool tagged (`CACHEDIR.TAG`) go, while a folder
/// with no evidence (`notes`, holding a token file, as `huggingface` does), a tagged folder of a
/// name the pass keeps (`huggingface`, `aterm-machines`, `codex-alab`), a folder whose tag is a
/// link, one whose tag is too short to hold the signature and a hidden tagged folder all stay,
/// and no row names any of them. A guarded root's child is taken by its name alone:
/// `Library/Caches/Homebrew` goes, while `Library/Caches/notes`, tagged, is never dated. NEGATIVE
/// CONTROL: the verb, given `notes` and the tagged `huggingface`, removes each, so every fence
/// admits them and it is the pass's evidence rule that kept them; and `notes`, tagged afterwards,
/// goes on the next pass.
#[cfg(not(windows))]
#[test]
fn the_pass_takes_only_folders_proven_caches_and_keeps_what_no_tool_makes_again() {
    let t = clock();
    let home = Home::new();
    let mounts = whole();
    let pip = home.cache(".cache/pip", BIG);
    let tagged = home.tagged(".cache/some-tool", BIG);
    let notes = home.cache(".cache/notes", BIG);
    std::fs::write(notes.join("token"), b"hf_not_made_again").expect("a token");
    let hf = home.tagged(".cache/huggingface", BIG);
    let machines = home.tagged(".cache/aterm-machines", BIG);
    let codex = home.tagged(".cache/codex-alab", BIG);
    let linked_tag = home.cache(".cache/linked-tag", BIG);
    let real_tag = home.path.join("elsewhere.tag");
    std::fs::write(&real_tag, [b'x'; 64]).expect("a tag elsewhere");
    atpkg::platform::atomic_symlink(&real_tag, &linked_tag.join(CACHEDIR_TAG)).expect("a link");
    let stub = home.cache(".cache/stub-tag", BIG);
    std::fs::write(stub.join(CACHEDIR_TAG), b"Sig").expect("a short tag");
    let hidden = home.tagged(".cache/.hidden", BIG);
    let brew = home.cache("Library/Caches/Homebrew", BIG);
    let guarded_notes = home.tagged("Library/Caches/notes", BIG);
    let shipped = dating(&date_within);
    let now = t + 9 * DAY;
    let w = at(&home, &nobody, &mounts, now);
    let (passed, rows) = swept(&w, &shipped, &mut || SHORT);
    let mut went = gone(&passed);
    went.sort();
    let mut want = vec![pip.clone(), tagged.clone(), brew.clone()];
    want.sort();
    assert_eq!(went, want, "{passed:?}");
    for kept in [
        &notes,
        &hf,
        &machines,
        &codex,
        &linked_tag,
        &stub,
        &hidden,
        &guarded_notes,
    ] {
        assert!(kept.join("blob").is_file(), "{} stays", kept.display());
        assert!(
            acts(&rows).iter().all(|(_, path)| path != kept),
            "no row names {}: {rows:?}",
            kept.display()
        );
    }
    assert!(notes.join("token").is_file());

    let (outcomes, _) = free(&[notes.clone(), hf.clone()], false, &w);
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o.verdict, Verdict::Removed { .. })),
        "{outcomes:?}"
    );

    let home = Home::new();
    let notes = home.cache(".cache/notes", BIG);
    let w = at(&home, &nobody, &mounts, now);
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert!(gone(&passed).is_empty(), "{passed:?}");
    std::fs::write(
        notes.join(CACHEDIR_TAG),
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .expect("a cache tag");
    let w = at(&home, &nobody, &mounts, now + 2 * 3600);
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [notes], "tagged now: {passed:?}");
}

/// A FILE A TOOL READS IS IN USE: a pip cache whose files were last written nine days ago but
/// one of which was read yesterday stays, since a read time the volume records is a use.
/// NEGATIVE CONTROL: the same cache with that read time set back nine days goes.
#[cfg(unix)]
#[test]
fn a_cache_read_within_its_window_stays() {
    let t = clock();
    let home = Home::new();
    let mounts = whole();
    let pip = home.cache(".cache/pip", BIG);
    let read = |at: i64| {
        let when = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
        std::fs::File::options()
            .write(true)
            .open(pip.join("blob"))
            .and_then(|f| f.set_times(std::fs::FileTimes::new().set_accessed(when)))
            .expect("set the read time");
    };
    read(t + 8 * DAY);
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert!(passed.outcomes.is_empty() && rows.is_empty(), "{passed:?}");
    assert!(pip.join("blob").is_file(), "read yesterday: it stays");

    // Not read again until its window from that read could have passed.
    read(t);
    let w = at(&home, &nobody, &mounts, t + 16 * DAY);
    let (passed, _) = swept(&w, &in_place(), &mut || SHORT);
    assert_eq!(gone(&passed), [pip], "{passed:?}");
}

/// A REMOVAL CUT SHORT LEAVES NO PART OF A CACHE WHERE ITS TOOL LOOKS, AND THE NEXT PASS
/// FINISHES IT: the pass sets a pip cache aside before it removes it, so when a folder in it
/// cannot be emptied the pass fails having removed part of it, and `~/.cache/pip` is gone whole
/// while what is left sits under one `.aterm-removing-<ino>-pip` name. Once the folder can be
/// emptied, the next pass removes what is left, its `removing` row first. A folder that only
/// carries the stage's prefix, its name not its own inode, stays with one `refused` row.
/// NEGATIVE CONTROL: removing in place (the verb's way) leaves the part that failed at
/// `~/.cache/pip` itself, where pip would find half its cache.
#[cfg(unix)]
#[test]
fn a_removal_cut_short_leaves_no_part_where_the_tool_looks_and_the_next_pass_finishes_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let t = clock();
    let home = Home::new();
    let mounts = whole();
    let pip = home.cache(".cache/pip", BIG);
    let stuck = pip.join("stuck");
    std::fs::create_dir(&stuck).expect("a folder");
    std::fs::write(stuck.join("wheel"), vec![1u8; 4096]).expect("a file");
    let foreign = home.cache(".cache/.aterm-removing-1-other", BIG);
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o500)).expect("read-only");
    let w = at(&home, &nobody, &mounts, t + 9 * DAY);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o700)).ok();
    assert!(
        passed
            .outcomes
            .iter()
            .any(|o| o.path == pip && matches!(o.verdict, Verdict::Failed { .. })),
        "{passed:?}"
    );
    assert!(!pip.exists(), "no part of it where pip looks");
    let staged: Vec<PathBuf> = std::fs::read_dir(home.path.join(".cache"))
        .expect("the root")
        .map(|e| e.expect("an entry").path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(STAGE) && n.ends_with("-pip"))
        })
        .collect();
    assert_eq!(staged.len(), 1, "{staged:?}");
    assert!(staged[0].join("stuck/wheel").is_file());
    std::fs::set_permissions(
        staged[0].join("stuck"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("writable again");
    assert!(
        acts(&rows).contains(&("refused".to_owned(), foreign.clone())),
        "{rows:?}"
    );
    assert!(
        foreign.join("blob").is_file(),
        "not one this pass set aside"
    );

    let w = at(&home, &nobody, &mounts, t + 9 * DAY + 3600);
    let (passed, rows) = swept(&w, &in_place(), &mut || SHORT);
    assert!(!staged[0].exists(), "the next pass finished it: {passed:?}");
    let acted = acts(&rows);
    let removing = acted
        .iter()
        .position(|a| *a == ("removing".to_owned(), staged[0].clone()));
    let removed = acted
        .iter()
        .position(|a| *a == ("removed".to_owned(), staged[0].clone()));
    assert!(
        removing.is_some() && removing < removed,
        "intent first: {rows:?}"
    );
    assert!(foreign.join("blob").is_file());

    let home = Home::new();
    let pip = home.cache(".cache/pip", BIG);
    let stuck = pip.join("stuck");
    std::fs::create_dir(&stuck).expect("a folder");
    std::fs::write(stuck.join("wheel"), vec![1u8; 4096]).expect("a file");
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o500)).expect("read-only");
    let w = world(&home, &nobody, &mounts);
    let (outcomes, _) = free(std::slice::from_ref(&pip), false, &w);
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o700)).ok();
    assert!(
        matches!(outcomes[0].verdict, Verdict::Failed { .. }),
        "{outcomes:?}"
    );
    assert!(
        stuck.join("wheel").is_file(),
        "in place, the part that failed stays where pip looks"
    );
}
