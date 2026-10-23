// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for which Claude Code version is live and what holds the others.

use super::*;
use crate::harness::disk::tests::Tmp;
use crate::harness::disk::{
    Class, Config, Roots, Survey, Trigger, Witness, report, scan_with, version_dir_of,
};
use aterm_uds::holders::{PathScan, PathUse};

/// A fixed instant; never `SystemTime::now()`.
const NOW: i64 = 1_789_603_200;

/// No process uses anything.
fn nobody(_: &[PathBuf], _: &[u32]) -> std::io::Result<PathScan> {
    Ok(PathScan::default())
}

/// bytes)` each, under `tmp`.
fn versions_of(tmp: &Tmp, files: &[(&str, &[u8])]) -> PathBuf {
    let dir = tmp.path().join("versions");
    std::fs::create_dir_all(&dir).expect("versions");
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes).expect("version");
    }
    dir
}

/// Claude Code's lock folder, through `processes`.
fn survey_of(
    tmp: &Tmp,
    versions: &Path,
    launcher: &Path,
    locks: Option<PathBuf>,
    processes: super::super::discover::Processes<'_>,
) -> Survey {
    let roots = Roots {
        versions_dir: Some(versions.to_path_buf()),
        live_link: Some(launcher.to_path_buf()),
        version_locks: locks,
        version_hashes: Some(tmp.path().join("state").join(HASH_FILE)),
        ..Roots::default()
    };
    scan_with(
        &roots,
        NOW,
        Trigger::OnDemand,
        Config::default(),
        None,
        processes,
    )
}

/// The canonical path of `versions/<name>`.
fn entry(versions: &Path, name: &str) -> PathBuf {
    std::fs::canonicalize(versions.join(name)).expect("an entry")
}

/// every real version then read as not live.
#[test]
fn only_a_target_strictly_inside_versions_names_a_version() {
    let root = PathBuf::from("/v");
    assert_eq!(
        version_dir_of(Path::new("/v/2.1.2/bin/claude"), Some(&root)),
        Some(PathBuf::from("/v/2.1.2"))
    );
    assert_eq!(
        version_dir_of(Path::new("/v/2.1.2"), Some(&root)),
        Some(PathBuf::from("/v/2.1.2"))
    );
    assert_eq!(version_dir_of(Path::new("/v"), Some(&root)), None);
    assert_eq!(
        version_dir_of(Path::new("/home/u/.local/bin/claude.exe"), Some(&root)),
        None
    );
    assert_eq!(version_dir_of(Path::new("/v/2.1.2"), None), None);
}

/// it refuses; a version folder by its walk.
#[test]
fn a_version_file_is_sized_by_its_length_and_a_folder_by_its_walk() {
    let tmp = Tmp::new("versions-size");
    let versions = versions_of(&tmp, &[("2.1.1", &[7u8; 1234])]);
    std::fs::create_dir_all(versions.join("2.1.0")).expect("a folder version");
    std::fs::write(versions.join("2.1.0").join("claude"), [1u8; 100]).expect("its binary");
    let root = std::fs::canonicalize(&versions).expect("root");
    let listed = list(&root);
    let bytes = |name: &str| {
        listed
            .iter()
            .find(|v| v.path == entry(&versions, name))
            .map(|v| (v.bytes, v.bytes_partial))
    };
    assert_eq!(bytes("2.1.1"), Some((1234, false)));
    assert_eq!(bytes("2.1.0"), Some((100, false)));
    assert_eq!(listed.len(), 2, "{listed:?}");
}

/// vendor's to lose, and no row may name it.
#[test]
fn a_link_in_versions_leading_out_is_no_entry() {
    let tmp = Tmp::new("versions-link");
    let versions = versions_of(&tmp, &[("2.1.1", b"x")]);
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::write(elsewhere.join("keep"), b"not a version").expect("keep");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&elsewhere, versions.join("2.1.9")).expect("link");
    #[cfg(windows)]
    atpkg::platform::atomic_symlink(&elsewhere, &versions.join("2.1.9")).expect("junction");
    let root = std::fs::canonicalize(&versions).expect("root");
    let listed = list(&root);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].path, entry(&versions, "2.1.1"));
}

/// none, and a launcher two entries are byte-identical to names neither.
#[test]
fn the_launcher_copy_names_the_one_version_of_its_bytes() {
    let tmp = Tmp::new("versions-copy");
    let versions = versions_of(
        &tmp,
        &[
            ("2.1.1", b"aaaaaaaaaa"),
            ("2.1.2", b"bbbbbbbbbb"),
            ("2.1.3", b"bbbbbbbbbbb"),
        ],
    );
    let root = std::fs::canonicalize(&versions).expect("root");
    let entries = list(&root);
    let launcher = tmp.path().join("claude.copy");
    let mut hashes = Hashes::default();

    std::fs::write(&launcher, b"bbbbbbbbbb").expect("a copy of 2.1.2");
    assert_eq!(
        copy_of(&launcher, &entries, &mut hashes),
        Ok(entry(&versions, "2.1.2"))
    );

    std::fs::write(&launcher, b"bbbbbbbbbc").expect("one byte off");
    let none = copy_of(&launcher, &entries, &mut Hashes::default());
    assert!(
        none.as_ref()
            .is_err_and(|e| e.contains("copy of no version")),
        "{none:?}"
    );

    std::fs::write(versions.join("2.1.4"), b"bbbbbbbbbb").expect("a twin of 2.1.2");
    std::fs::write(&launcher, b"bbbbbbbbbb").expect("a copy of both");
    let both = copy_of(&launcher, &list(&root), &mut Hashes::default());
    assert!(
        both.as_ref()
            .is_err_and(|e| e.contains("copy of 2 versions")),
        "{both:?}"
    );
}

/// write time moves, the bytes are read again.
#[test]
fn a_digest_is_kept_under_the_files_own_stamp() {
    let tmp = Tmp::new("versions-cache");
    let file = tmp.path().join("2.1.2");
    let cache = tmp.path().join("state").join(HASH_FILE);
    std::fs::write(&file, b"first bytes").expect("file");
    let mut hashes = Hashes::load(Some(&cache));
    let (len, first) = hashes.digest(&file).expect("read");
    assert_eq!(len, 11);
    hashes.save();
    assert!(cache.is_file(), "written to the state directory");

    let written = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .expect("mtime");
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .expect("in place");
        f.write_all(b"other bytes").expect("same length");
        f.set_modified(written).expect("write time put back");
    }
    let mut again = Hashes::load(Some(&cache));
    assert_eq!(
        again.digest(&file).expect("from the cache").1,
        first,
        "kept under an unmoved stamp"
    );

    let later = written + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .and_then(|f| f.set_modified(later))
        .expect("write time moved");
    let (_, read) = again.digest(&file).expect("read again");
    assert_ne!(read, first, "a moved stamp reads the bytes again");
}

/// CONTROL: the cache this reader writes back is a hit for the same file.
#[test]
fn a_cache_of_the_old_identity_width_is_a_miss() {
    let tmp = Tmp::new("versions-cache-width");
    let file = tmp.path().join("2.1.2");
    std::fs::write(&file, b"the version's bytes").expect("file");
    let cache = tmp.path().join("state").join(HASH_FILE);
    let open = std::fs::File::open(&file).expect("open");
    let meta = open.metadata().expect("metadata");
    let ns = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| u64::try_from(d.as_nanos()).ok())
    };
    let (dev, ino) = crate::harness::session_usage::open_identity(&open, &meta)
        .expect("the old identity reads here");
    let forged = "0".repeat(64);
    let mut e = Map::new();
    e.insert("len".to_owned(), Value::from(meta.len()));
    let opt = |n: Option<u64>| n.map_or(Value::Null, Value::from);
    e.insert("written_ns".to_owned(), opt(ns(meta.modified())));
    e.insert("created_ns".to_owned(), opt(ns(meta.created())));
    e.insert("dev".to_owned(), Value::from(dev));
    e.insert("ino".to_owned(), Value::from(ino));
    e.insert("sha256".to_owned(), Value::from(forged.clone()));
    let mut files = Map::new();
    files.insert(file.to_string_lossy().into_owned(), Value::Object(e));
    let mut o = Map::new();
    o.insert("schema".to_owned(), Value::from(1u64));
    o.insert("files".to_owned(), Value::Object(files));
    std::fs::create_dir_all(cache.parent().expect("state")).expect("state");
    std::fs::write(
        &cache,
        aterm_json::to_string(&Value::Object(o)).expect("encoded"),
    )
    .expect("an old cache");
    drop(open);

    let mut hashes = Hashes::load(Some(&cache));
    let (_, read) = hashes.digest(&file).expect("read");
    assert_ne!(read, forged, "an old-width entry is no hit");
    hashes.save();

    let written = meta.modified().expect("mtime");
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .expect("in place");
        f.write_all(b"other version bytes").expect("same length");
        f.set_modified(written).expect("write time put back");
    }
    let (_, kept) = Hashes::load(Some(&cache))
        .digest(&file)
        .expect("from the cache");
    assert_eq!(kept, read, "the cache it writes is a hit");
}

/// anyone out. NEGATIVE CONTROL: with none of them, nothing keeps it.
#[test]
fn a_lock_or_a_process_keeps_a_version_that_is_not_live() {
    let tmp = Tmp::new("versions-kept");
    let versions = versions_of(&tmp, &[("2.1.1", b"old"), ("2.1.2", b"live")]);
    let old = VersionDir {
        path: entry(&versions, "2.1.1"),
        ..VersionDir::default()
    };
    let live = entry(&versions, "2.1.2");
    let locks = tmp.path().join("locks");
    std::fs::create_dir_all(&locks).expect("locks");

    assert_eq!(kept(&old, &live, Some(&locks), &nobody), None);

    std::fs::create_dir(locks.join("2.1.1.lock")).expect("a lock");
    let why = kept(&old, &live, Some(&locks), &nobody).unwrap_or_default();
    assert!(why.contains("2.1.1.lock"), "{why}");
    std::fs::remove_dir(locks.join("2.1.1.lock")).expect("unlocked");

    let path = old.path.clone();
    let runs = move |_: &[PathBuf], _: &[u32]| {
        Ok(PathScan {
            users: vec![PathUse {
                pid: 4242,
                how: "exe",
                path: path.clone(),
            }],
            ..PathScan::default()
        })
    };
    let why = kept(&old, &live, Some(&locks), &runs).unwrap_or_default();
    assert!(why.contains("process 4242"), "{why}");

    let unlisted = |_: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        Err(std::io::Error::other("no reader"))
    };
    let why = kept(&old, &live, Some(&locks), &unlisted).unwrap_or_default();
    assert!(why.contains("could not be listed"), "{why}");
}

/// nothing uses, is still free. A lock keeps its version without a scan.
#[test]
fn every_version_is_judged_from_one_process_scan() {
    let tmp = Tmp::new("versions-one-scan");
    let versions = versions_of(
        &tmp,
        &[
            ("2.1.0", b"zero"),
            ("2.1.1", b"one!"),
            ("2.1.2", b"two!"),
            ("2.1.3", b"live"),
        ],
    );
    let live = entry(&versions, "2.1.3");
    let fresh = || -> Vec<VersionDir> {
        ["2.1.0", "2.1.1", "2.1.2", "2.1.3"]
            .into_iter()
            .map(|name| VersionDir {
                path: entry(&versions, name),
                ..VersionDir::default()
            })
            .collect()
    };
    let calls = std::cell::Cell::new(0usize);
    let quiet = |_: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        calls.set(calls.get() + 1);
        Ok(PathScan::default())
    };
    let mut entries = fresh();
    mark_kept(&mut entries, &live, None, &quiet, None);
    assert_eq!(calls.get(), 1, "one scan");
    assert!(entries.iter().all(|v| v.kept.is_none()), "{entries:#?}");

    let uses = [
        (7u32, "exe", entry(&versions, "2.1.0")),
        (7u32, "fd", entry(&versions, "2.1.1")),
    ];
    calls.set(0);
    let busy = |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        calls.set(calls.get() + 1);
        let first = uses
            .iter()
            .find(|(_, _, path)| dirs.iter().any(|d| path.starts_with(d)));
        Ok(PathScan {
            users: first
                .map(|(pid, how, path)| PathUse {
                    pid: *pid,
                    how,
                    path: path.clone(),
                })
                .into_iter()
                .collect(),
            ..PathScan::default()
        })
    };
    let mut entries = fresh();
    mark_kept(&mut entries, &live, None, &busy, None);
    let kept = |name: &str| {
        entries
            .iter()
            .find(|v| v.path == entry(&versions, name))
            .and_then(|v| v.kept.clone())
    };
    assert!(
        kept("2.1.0").is_some_and(|w| w.contains("process 7")),
        "{entries:#?}"
    );
    assert!(
        kept("2.1.1").is_some_and(|w| w.contains("process 7")),
        "{entries:#?}"
    );
    assert_eq!(kept("2.1.2"), None);
    assert_eq!(kept("2.1.3"), None, "the live one is not judged");
    assert_eq!(
        calls.get(),
        3,
        "one scan, then one for each entry no use named"
    );

    let locks = tmp.path().join("locks");
    std::fs::create_dir_all(&locks).expect("locks");
    for name in ["2.1.0", "2.1.1", "2.1.2"] {
        std::fs::write(locks.join(format!("{name}.lock")), b"").expect("a lock");
    }
    calls.set(0);
    let mut entries = fresh();
    mark_kept(&mut entries, &live, Some(&locks), &quiet, None);
    assert_eq!(calls.get(), 0, "no scan");
    assert!(
        entries
            .iter()
            .filter(|v| v.path != live)
            .all(|v| v.kept.as_deref().is_some_and(|w| w.contains(".lock"))),
        "{entries:#?}"
    );
}

/// NEGATIVE CONTROL: the entries nothing uses stay free.
#[cfg(windows)]
#[test]
fn a_use_in_the_kernels_spelling_is_placed_on_its_version() {
    let tmp = Tmp::new("versions-spelling");
    let versions = versions_of(
        &tmp,
        &[
            ("2.1.0", b"zero"),
            ("2.1.1", b"one!"),
            ("2.1.2", b"two!"),
            ("2.1.3", b"live"),
        ],
    );
    let live = entry(&versions, "2.1.3");
    let held = entry(&versions, "2.1.0");
    let kernel = PathBuf::from(
        aterm_sysprobe::paths::plain(&held)
            .to_string_lossy()
            .to_uppercase(),
    );
    assert!(!kernel.starts_with(&held), "two spellings of one path");
    let calls = std::cell::Cell::new(0usize);
    let scan = |dirs: &[PathBuf], _: &[u32]| -> std::io::Result<PathScan> {
        calls.set(calls.get() + 1);
        let asked = dirs
            .iter()
            .any(|d| aterm_sysprobe::paths::is_under(&kernel, d) == Some(true));
        Ok(PathScan {
            users: asked
                .then(|| PathUse {
                    pid: 9,
                    how: "exe",
                    path: kernel.clone(),
                })
                .into_iter()
                .collect(),
            ..PathScan::default()
        })
    };
    let mut entries: Vec<VersionDir> = ["2.1.0", "2.1.1", "2.1.2", "2.1.3"]
        .into_iter()
        .map(|name| VersionDir {
            path: entry(&versions, name),
            ..VersionDir::default()
        })
        .collect();
    let table = aterm_sysprobe::volumes::table();
    mark_kept(&mut entries, &live, None, &scan, Some(&table));
    let kept = |name: &str| {
        entries
            .iter()
            .find(|v| v.path == entry(&versions, name))
            .and_then(|v| v.kept.clone())
    };
    assert!(
        kept("2.1.0").is_some_and(|w| w.contains("process 9")),
        "{entries:#?}"
    );
    assert_eq!(kept("2.1.1"), None);
    assert_eq!(kept("2.1.2"), None);
    assert_eq!(
        calls.get(),
        3,
        "one scan, then one for each entry the use does not name"
    );
}

/// generates short names and the child keeps the spelling.
#[cfg(windows)]
#[test]
fn a_version_used_through_a_short_spelling_keeps_that_version_alone() {
    let tmp = Tmp::new("versions-short");
    let home = tmp.path().join("a home with a long name");
    let versions = home.join("versions");
    for name in ["1.0.0", "1.0.1", "2.0.0"] {
        std::fs::create_dir_all(versions.join(name)).expect("a version folder");
    }
    let root = std::fs::canonicalize(&versions).expect("canonical");
    let live = entry(&versions, "2.0.0");
    let used = entry(&versions, "1.0.0");
    let short = aterm_sysprobe::short_spelling(&used).unwrap_or_else(|| {
        panic!(
            "PREMISE: the volume generates 8.3 short names; none for {}",
            used.display()
        )
    });
    let for_cmd = |p: &Path| {
        let s = p.to_str().expect("a test path is UTF-8");
        PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(s))
    };
    let table = aterm_sysprobe::volumes::table();
    for (spelling, short_spelled) in [(for_cmd(&short), true), (for_cmd(&used), false)] {
        let mut child = aterm_uds::spawnfd::command("cmd")
            .args(["/D", "/Q", "/C", "set /p line="])
            .current_dir(&spelling)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("cmd starts");
        let pid = child.id();
        let ours = |dirs: &[PathBuf], skip: &[u32]| {
            aterm_uds::holders::scan_path_users_of_within(
                &[pid],
                dirs,
                skip,
                aterm_spec::wait::MUST_HAPPEN,
            )
        };
        let mut entries = list(&root);
        mark_kept(&mut entries, &live, None, &ours, Some(&table));
        let reported = aterm_sysprobe::process_cwd(pid).expect("its working directory");
        drop(child.stdin.take());
        let _ = child.wait();
        assert_eq!(
            aterm_sysprobe::paths::short_spelled(&reported).is_some(),
            short_spelled,
            "PREMISE: the child reports the spelling it was started in: {reported:?}"
        );
        let kept = |name: &str| {
            entries
                .iter()
                .find(|v| v.path == entry(&versions, name))
                .map(|v| v.kept.clone())
        };
        assert!(
            kept("1.0.0")
                .flatten()
                .is_some_and(|w| w.starts_with(&format!("process {pid} uses"))),
            "{spelling:?}: {entries:#?}"
        );
        assert_eq!(kept("1.0.1"), Some(None), "{spelling:?}: {entries:#?}");
        assert_eq!(kept("2.0.0"), Some(None), "the live one is not judged");
    }
}

/// is not one has no order, which keeps it ([`newer_than`]).
#[test]
fn versions_are_ordered_by_their_numbers_and_a_non_version_is_kept() {
    assert_eq!(version_number("2.1.286"), Some(vec![2, 1, 286]));
    assert!(version_number("2.1.300") > version_number("2.1.286"));
    assert!(version_number("2.10.0") > version_number("2.9.99"));
    for not in ["", "2.1.", ".2", "2.1.3-beta", "2.1.3.tmp", "latest", "+1"] {
        assert_eq!(version_number(not), None, "{not}");
    }
    let live = Path::new("/v/2.1.286");
    assert!(newer_than("2.1.300", live).is_some());
    assert_eq!(newer_than("2.1.285", live), None);
    assert_eq!(newer_than("2.1.286", live), None);
    assert!(newer_than("2.1.287.download", live).is_some());
}

/// old fallback made every version removable, the live one included).
#[test]
fn a_launcher_copy_names_the_live_version_and_holds_are_kept() {
    let tmp = Tmp::new("versions-scan");
    let versions = versions_of(
        &tmp,
        &[
            ("2.1.0", b"version zero"),
            ("2.1.1", b"version one!"),
            ("2.1.2", b"version two!"),
            ("2.1.3", b"version 3!!!"),
        ],
    );
    let launcher = tmp.path().join("claude.copy");
    std::fs::write(&launcher, b"version two!").expect("the installer's copy");
    let locks = tmp.path().join("locks");
    std::fs::create_dir_all(&locks).expect("locks");
    std::fs::write(locks.join("2.1.0.lock"), b"").expect("a lock");
    let survey = survey_of(&tmp, &versions, &launcher, Some(locks.clone()), &nobody);
    let rep = report(&survey, Config::default());
    let rows = rep.rows_of(Class::ClaudeStaleVersions);
    let row = |name: &str| rows.iter().find(|r| r.path == entry(&versions, name));
    if cfg!(windows) {
        let live = entry(&versions, "2.1.2");
        assert_eq!(survey.live_version.as_ref(), Some(&live));
        assert_eq!(survey.live_copy.as_deref(), Some(launcher.as_path()));
        assert!(row("2.1.2").is_none(), "the live one is no row: {rows:#?}");
        let old = row("2.1.1").expect("the older one");
        assert!(old.removable, "{:?}", old.blocked);
        assert_eq!(old.bytes, 12, "sized as a file");
        assert!(
            matches!(&old.witness, Witness::NotLauncherCopy { launcher: l, live: v }
                if l == &launcher && v == &live),
            "{:?}",
            old.witness
        );
        assert_eq!(old.witness.kind(), "not-launcher-copy");
        let newer = row("2.1.3").expect("the newer one");
        assert!(!newer.removable);
        assert!(
            newer
                .blocked
                .as_deref()
                .is_some_and(|b| b.contains("newer")),
            "{:?}",
            newer.blocked
        );
        let locked = row("2.1.0").expect("the locked one");
        assert!(!locked.removable);
        assert!(
            locked
                .blocked
                .as_deref()
                .is_some_and(|b| b.contains("2.1.0.lock")),
            "{:?}",
            locked.blocked
        );
    } else {
        assert_eq!(survey.live_version, None);
        assert!(rows.iter().all(|r| !r.removable), "{rows:#?}");
        assert!(
            survey
                .live_unnamed
                .as_deref()
                .is_some_and(|w| w.contains("not inside")),
            "{:?}",
            survey.live_unnamed
        );
    }

    std::fs::write(&launcher, b"version two?").expect("one byte off");
    let survey = survey_of(&tmp, &versions, &launcher, Some(locks.clone()), &nobody);
    let rep = report(&survey, Config::default());
    let rows = rep.rows_of(Class::ClaudeStaleVersions);
    assert_eq!(survey.live_version, None);
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| !r.removable), "{rows:#?}");

    std::fs::remove_file(&launcher).expect("no launcher");
    let survey = survey_of(&tmp, &versions, &launcher, Some(locks), &nobody);
    let rep = report(&survey, Config::default());
    assert!(
        rep.rows_of(Class::ClaudeStaleVersions)
            .iter()
            .all(|r| !r.removable)
    );
    assert!(
        rep.notes.iter().any(|n| n.contains("every row is blocked")),
        "{:?}",
        rep.notes
    );
}

/// so a row that kept the link's spelling would remove nothing.
#[cfg(windows)]
#[test]
fn a_versions_folder_reached_through_a_junction_is_read_where_it_resolves() {
    let tmp = Tmp::new("versions-through-junction");
    let real = tmp.path().join("real");
    let versions = real.join("versions");
    std::fs::create_dir_all(&versions).expect("versions");
    std::fs::write(versions.join("2.1.1"), b"version one!").expect("the stale one");
    std::fs::write(versions.join("2.1.2"), b"version two!").expect("the live one");
    let link = tmp.path().join("claude");
    atpkg::platform::atomic_symlink(&real, &link).expect("a junction");
    let launcher = tmp.path().join("claude.copy");
    std::fs::write(&launcher, b"version two!").expect("the installer's copy");

    let survey = survey_of(&tmp, &link.join("versions"), &launcher, None, &nobody);
    let rep = report(&survey, Config::default());
    let rows = rep.rows_of(Class::ClaudeStaleVersions);
    assert_eq!(survey.live_version, Some(entry(&versions, "2.1.2")));
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].path, entry(&versions, "2.1.1"));
    assert!(rows[0].removable, "{:?}", rows[0].blocked);

    crate::harness::disk::remove_tree(&rows[0].path).expect("the resolved path goes");
    assert!(!versions.join("2.1.1").exists());
    assert_eq!(
        std::fs::read(versions.join("2.1.2")).expect("the live one stays"),
        b"version two!"
    );
    assert!(
        std::fs::symlink_metadata(&link).is_ok_and(|m| m.is_symlink()),
        "the junction stays"
    );
}
