// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn staging_refuses_existing_files_and_links_without_changing_either_file() {
    let root = tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("staged");
    fs::write(&source, b"source").unwrap();
    fs::write(&destination, b"keep").unwrap();
    assert_eq!(
        copy_to_new_file(&source, &destination).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        copy_to_new_file(&source, &source).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    #[cfg(unix)]
    {
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert_eq!(
            copy_to_new_file(&source, &link).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }
    assert_eq!(fs::read(&source).unwrap(), b"source");
    assert_eq!(fs::read(&destination).unwrap(), b"keep");
}

/// NEGATIVE CONTROL: `hex_digit` clamps nibbles `>= 16` to `'f'` while mapping `0..=15`
/// bijectively, `unique_name` produces distinct `<prefix><pid>_<16 lowercase hex>_<counter>`
/// strings on consecutive calls, and `encode_temp_seed_le` / `decode_temp_seed_le` roundtrip
/// boundary values without field aliasing.
#[test]
fn unique_name_hex_digit_and_seed_codec_roundtrip() {
    let digits: String = (0..16).map(hex_digit).collect();
    assert_eq!(digits, "0123456789abcdef");
    assert_eq!(hex_digit(16), 'f');
    assert_eq!(hex_digit(u64::MAX), 'f');

    let first = unique_name("pfx_");
    let second = unique_name("pfx_");
    assert_ne!(first, second);
    let pid_prefix = format!("pfx_{}_", std::process::id());
    for candidate in [&first, &second] {
        let rest = candidate
            .strip_prefix(&pid_prefix)
            .expect("unique_name must start with <prefix><pid>_");
        let (hex_part, counter_part) = rest
            .split_once('_')
            .expect("unique_name must have <hex16>_<counter>");
        assert_eq!(hex_part.len(), 16);
        assert!(
            hex_part
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert!(counter_part.parse::<u64>().is_ok());
    }

    for seed in [
        (0u64, 0u32, 0u32),
        (0x0123_4567_89ab_cdef, 0x89ab_cdef, 0xfedc_ba98),
        (u64::MAX, u32::MAX, u32::MAX),
    ] {
        let wire = encode_temp_seed_le(seed.0, seed.1, seed.2);
        assert_eq!(decode_temp_seed_le(wire), seed);
    }
}

#[cfg(unix)]
#[test]
fn a_cache_slots_hash_is_aterm_hashs_fold() {
    let inputs: [&[u8]; 4] = [b"", b"a", b"foobar", &[0, 0xff, 0x80, 0x7f]];
    for bytes in inputs {
        assert_eq!(fnv1a(bytes), aterm_hash::fnv1a_64(bytes), "{bytes:02x?}");
    }
}

/// NEGATIVE CONTROL (2026-11-05): with [`clone_tree`] linking `dst` to `src` instead of cloning
/// it, a write to the clone reaches the source (`left: "changed"`, `right: "first"`).
#[cfg(target_os = "macos")]
#[test]
fn a_cloned_tree_holds_what_its_source_held_and_shares_nothing_after() {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let dir = tempdir().unwrap();
    let (src, dst) = (dir.path().join("src"), dir.path().join("dst"));
    fs::DirBuilder::new().mode(0o700).create(&src).unwrap();
    fs::DirBuilder::new()
        .mode(0o711)
        .create(src.join("sub"))
        .unwrap();
    // Creation modes pass through the runner's umask; seed the exact modes to preserve.
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o711)).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(src.join("sub/file"))
        .unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o640))
        .unwrap();
    io::Write::write_all(&mut file, b"first").unwrap();
    drop(file);

    match clone_tree(&src, &dst) {
        // A scratch volume that cannot clone has nothing to judge here.
        Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
        cloned => cloned.unwrap(),
    }

    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&dst), 0o700);
    assert_eq!(mode(&dst.join("sub")), 0o711);
    assert_eq!(mode(&dst.join("sub/file")), 0o640);
    assert_eq!(fs::read(dst.join("sub/file")).unwrap(), b"first");

    fs::write(dst.join("sub/file"), b"changed").unwrap();
    assert_eq!(
        String::from_utf8(fs::read(src.join("sub/file")).unwrap()).unwrap(),
        "first"
    );
    assert_eq!(
        clone_tree(&src, &dst).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn tempdir_creates_and_cleans_up() {
    let path;
    {
        let dir = tempdir().expect("create tempdir");
        path = dir.path().to_path_buf();
        assert!(path.exists(), "tempdir should exist");
        assert!(path.is_dir(), "tempdir should be a directory");
    }
    assert!(!path.exists(), "tempdir should be cleaned up on drop");
}

#[test]
fn keeping_a_directory_transfers_cleanup_to_the_caller() {
    let parent = tempdir().expect("parent");
    let dir = TempDir::new_in(parent.path()).expect("child");
    fs::write(dir.path().join("artifact"), b"retained").expect("artifact");
    let kept = dir.keep();
    assert_eq!(
        fs::read(kept.join("artifact")).expect("retained"),
        b"retained"
    );
    fs::remove_dir_all(kept).expect("caller cleanup");
    assert_eq!(fs::read_dir(parent.path()).expect("parent").count(), 0);
}

#[test]
fn builder_prefix() {
    let dir = Builder::new()
        .prefix("myprefix_")
        .tempdir()
        .expect("create");
    let name = dir.path().file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("myprefix_"), "name: {name}");
    let inner = Builder::new()
        .prefix("inner_")
        .tempdir_in(dir.path())
        .expect("create in dir");
    assert_eq!(inner.path().parent(), Some(dir.path()));
    let name = inner.path().file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("inner_"), "name: {name}");
}

#[test]
fn a_suffix_stays_in_the_name_and_keeps_directory_cleanup() {
    let root = tempdir().expect("root");
    let parent = root.path().join("inside");
    fs::create_dir(&parent).expect("parent");
    let path;
    {
        let dir = Builder::new()
            .prefix("scratch_")
            .suffix(".noindex")
            .tempdir_in(&parent)
            .expect("a suffixed directory");
        path = dir.path().to_path_buf();
        assert_eq!(path.parent(), Some(parent.as_path()));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("scratch_") && name.ends_with(".noindex"));
        fs::write(path.join("artifact"), b"owned").expect("artifact");
    }
    assert!(!path.exists());
    for suffix in ["/../outside", "/nested/child", "/", ".bad\0"] {
        let error = Builder::new()
            .suffix(suffix)
            .tempdir_in(&parent)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{suffix:?}");
    }
    #[cfg(windows)]
    for suffix in [r"\..\outside", r"\nested\child", r"\", ".", " "] {
        let error = Builder::new()
            .suffix(suffix)
            .tempdir_in(&parent)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{suffix:?}");
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(fs::read_dir(parent).unwrap().count(), 0);
}

#[test]
fn a_prefix_cannot_escape_the_requested_parent() {
    let root = tempdir().expect("root");
    let parent = root.path().join("inside");
    fs::create_dir(&parent).expect("parent");
    let absolute = root.path().join("outside_");
    for prefix in [
        "../outside_",
        "nested/child_",
        "nul\0_",
        absolute.to_str().unwrap(),
    ] {
        let error = Builder::new()
            .prefix(prefix)
            .tempdir_in(&parent)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{prefix:?}");
    }
    #[cfg(windows)]
    for prefix in [r"..\outside_", r"C:outside_"] {
        let error = Builder::new()
            .prefix(prefix)
            .tempdir_in(&parent)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{prefix:?}");
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
    for prefix in ["", ".", "..", "ordinary_"] {
        let dir = Builder::new().prefix(prefix).tempdir_in(&parent).unwrap();
        assert_eq!(dir.path().parent(), Some(parent.as_path()));
    }
}

/// The bodies the stand-in tests lay: fixed texts, so the cache every process of this user
/// shares gains one file for each once, never one a run.
#[cfg(unix)]
const RUNS: &str = "#!/bin/sh\necho \"$0 $*\"\n";
#[cfg(unix)]
const OTHER: &str = "#!/bin/sh\necho other\n";

/// ONE NAME PER STAND-IN'S FILE. Two stand-ins of one body, in two fixtures, run one file (on
/// macOS, where they share it) outside both fixtures, read-only and executable, whose link
/// count is 1: no fixture holds a name of it, so the first fixture going, its directory and
/// all, leaves the second's stand-in running that body as itself (`$0` is its own path).
/// Another body runs a file of its own. NEGATIVE CONTROL: laid by hard link, as the copy this
/// replaced laid them, that file has a name in each fixture besides its own, so its link
/// count is 3 (more while another test links it); and the gone fixture's stand-in does not
/// run.
#[cfg(unix)]
#[test]
fn stand_ins_of_one_body_run_one_file_of_one_name() {
    use std::os::unix::fs::MetadataExt as _;
    let (one, two) = (tempdir().expect("a fixture"), tempdir().expect("another"));
    let a = one.path().join("tool");
    let b = two.path().join("tool");
    let c = two.path().join("other");
    stand_in(&a, RUNS).expect("a");
    stand_in(&b, RUNS).expect("b");
    stand_in(&c, OTHER).expect("c");
    let runs = |p: &Path| fs::canonicalize(p).expect("a stand-in resolves");
    for (p, body) in [(&a, RUNS), (&b, RUNS), (&c, OTHER)] {
        let meta = fs::metadata(p).expect("a stand-in's file");
        assert_eq!(fs::read_to_string(p).expect("read"), body);
        assert_eq!(meta.mode() & 0o7777, 0o555, "{}", p.display());
        assert_eq!(
            meta.nlink(),
            1,
            "{} has a name a fixture removes",
            runs(p).display()
        );
    }
    assert!(
        fs::write(&a, "x").is_err(),
        "a write through a stand-in fails"
    );
    assert_ne!(runs(&a), runs(&c), "another body, another file");
    if cfg!(target_os = "macos") {
        assert_eq!(runs(&a), runs(&b), "one file per body");
        for fixture in [one.path(), two.path()] {
            let fixture = fs::canonicalize(fixture).expect("a fixture resolves");
            assert!(
                !runs(&a).starts_with(&fixture),
                "{} runs from inside {}",
                a.display(),
                fixture.display()
            );
        }
    }
    drop(one);
    let run = |tool: &Path| {
        aterm_uds::deadline::output_within(
            aterm_uds::spawnfd::command(tool).arg("kept"),
            aterm_uds::deadline::FIRST_EXEC_HANG,
        )
    };
    let out = run(&b).expect("the kept fixture's stand-in ran");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{} kept\n", b.display())
    );
    assert!(run(&a).is_err(), "the gone fixture's stand-in ran");
}

/// A stand-in laid again with another body runs that body, by rename; laid again with its
/// own body it is unchanged, and nothing is left beside it. The file of the body it ran
/// before still holds that body (on macOS, where it is shared): no stand-in's file is
/// rewritten. A folder it cannot write gets no stand-in (`PermissionDenied`, the temp's own
/// error), nothing is left in it, and the file of that body is unchanged. NEGATIVE CONTROL:
/// a path naming no file is refused, and nothing is laid.
#[cfg(unix)]
#[test]
fn a_stand_in_laid_again_with_another_body_runs_that_body() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let dir = tempdir().expect("a fixture");
    let tool = dir.path().join("tool");
    stand_in(&tool, RUNS).expect("the first body");
    let ran = fs::canonicalize(&tool).expect("the stand-in resolves");
    stand_in(&tool, OTHER).expect("another body");
    assert_eq!(fs::read_to_string(&tool).expect("read"), OTHER);
    if cfg!(target_os = "macos") {
        assert_eq!(fs::read_to_string(&ran).expect("the file it ran"), RUNS);
    }
    stand_in(&tool, OTHER).expect("laid again, unchanged");
    assert_eq!(fs::read_to_string(&tool).expect("read"), OTHER);
    let refused = stand_in(&dir.path().join(".."), RUNS).map_err(|e| e.kind());
    assert_eq!(refused, Err(io::ErrorKind::InvalidInput));
    let names: Vec<String> = fs::read_dir(dir.path())
        .expect("list")
        .map(|e| e.expect("entry").file_name().into_string().expect("UTF-8"))
        .collect();
    assert_eq!(names, ["tool"]);
    if euid() != 0 {
        let file = fs::canonicalize(&tool).expect("the stand-in resolves");
        let before = fs::metadata(&file).map(|m| (m.ino(), m.mode(), m.mtime_nsec()));
        let shut = tempdir().expect("a fixture it cannot write");
        fs::set_permissions(shut.path(), fs::Permissions::from_mode(0o555)).expect("shut");
        let refused = stand_in(&shut.path().join("tool"), OTHER).map_err(|e| e.kind());
        let left = fs::read_dir(shut.path()).map(Iterator::count);
        fs::set_permissions(shut.path(), fs::Permissions::from_mode(0o700)).expect("open");
        assert_eq!(refused, Err(io::ErrorKind::PermissionDenied));
        assert_eq!(
            left.ok(),
            Some(0),
            "nothing is left in a folder it cannot write"
        );
        assert_eq!(fs::read_to_string(&file).expect("read"), OTHER);
        let after = fs::metadata(&file).map(|m| (m.ino(), m.mode(), m.mtime_nsec()));
        assert_eq!(after.ok(), before.ok(), "{} changed", file.display());
    }
}

/// A FILE THAT NO LONGER HOLDS ITS TEXT IS RUN NO MORE. The memo's file for a text, made
/// writable and written through, is passed over: the next ask gets the next slot, holding the
/// text, and the written-through file is left as it is; that file removed in turn, the next
/// ask gets one holding the text again, which the memo then names. NEGATIVE CONTROL: the
/// memo's file, untouched, is the one given again.
#[cfg(unix)]
#[test]
fn a_stand_in_file_that_no_longer_holds_its_text_is_passed_over() {
    use std::os::unix::fs::PermissionsExt as _;
    let me = euid();
    let tmp = tempdir().expect("scratch");
    let text = "#!/bin/sh\nexit 7\n";
    let tampered = "#!/bin/sh\necho tampered\n";
    let mut shared = BTreeMap::new();
    let first = shared_in(&mut shared, me, tmp.path(), text).expect("a file");
    assert_eq!(
        shared_in(&mut shared, me, tmp.path(), text).as_ref(),
        Some(&first),
        "the same file while it holds the text"
    );
    fs::set_permissions(&first, fs::Permissions::from_mode(0o755)).expect("made writable");
    fs::write(&first, tampered).expect("written through");
    let next = shared_in(&mut shared, me, tmp.path(), text).expect("another file");
    assert_ne!(
        next,
        first,
        "{} is run though it was written",
        first.display()
    );
    assert_eq!(fs::read_to_string(&next).ok().as_deref(), Some(text));
    assert_eq!(fs::read_to_string(&first).ok().as_deref(), Some(tampered));
    fs::remove_file(&next).expect("removed");
    let again = shared_in(&mut shared, me, tmp.path(), text).expect("a file again");
    assert_eq!(fs::read_to_string(&again).ok().as_deref(), Some(text));
    assert_eq!(
        shared.get(text),
        Some(&again),
        "the memo names the file it gave"
    );
}

/// Set, the test binary is [`stand_in_cache_child_if_asked`]: it asks [`cached_in`] for this
/// directory's file holding [`CACHED`] and writes the file it got to `got` there.
#[cfg(unix)]
const CACHE_CHILD: &str = "ATERM_TEMPFILE_STAND_IN_CACHE_CHILD";
/// The text [`the_stand_in_cache_holds_one_read_only_file_per_text`] and its child ask for.
#[cfg(unix)]
const CACHED: &str = "#!/bin/sh\nexit 3\n";
/// The wait for that child, which must end: a one-minute hang detector (aterm-spec's
/// `wait::MUST_HAPPEN`), never a latency budget.
#[cfg(unix)]
const MUST_HAPPEN: std::time::Duration = std::time::Duration::from_secs(60);

/// Run by [`the_stand_in_cache_holds_one_read_only_file_per_text`] in a process of its own.
#[cfg(unix)]
fn stand_in_cache_child_if_asked() -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Some(dir) = std::env::var_os(CACHE_CHILD).map(PathBuf::from) else {
        return false;
    };
    let file = cached_in(euid(), &dir, CACHED).expect("cached");
    fs::write(dir.join("got"), file.as_os_str().as_bytes()).expect("said which file");
    true
}

/// The stand-in cache keeps one file per text, however often it is asked, by this process
/// or another, read-only, in a quiet folder on macOS, and nothing else. NEGATIVE CONTROL: a
/// second text gets a file of its own.
#[cfg(unix)]
#[test]
fn the_stand_in_cache_holds_one_read_only_file_per_text() {
    if stand_in_cache_child_if_asked() {
        return;
    }
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let me = euid();
    let tmp = tempdir().expect("scratch");
    let a = cached_in(me, tmp.path(), CACHED).expect("a cached stand-in");
    let again = cached_in(me, tmp.path(), CACHED).expect("the same text");
    let b = cached_in(me, tmp.path(), "#!/bin/sh\nexit 4\n").expect("another text");
    let child = aterm_uds::deadline::output_within(
        aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args([
                "--exact",
                "tests::the_stand_in_cache_holds_one_read_only_file_per_text",
                "--test-threads=1",
            ])
            .env(CACHE_CHILD, tmp.path()),
        MUST_HAPPEN,
    )
    .expect("the child ran");
    assert!(child.status.success(), "the child failed: {child:?}");
    let theirs = fs::read(tmp.path().join("got"))
        .map(|got| PathBuf::from(std::ffi::OsString::from_vec(got)));
    assert_eq!(
        theirs.ok().as_ref(),
        Some(&a),
        "another process gets the same file"
    );
    assert_eq!(a, again, "one file per text");
    assert_ne!(a, b, "a text of its own gets a file of its own");
    let cache = a.parent().expect("the cache").to_path_buf();
    let held = fs::read_dir(&cache).expect("list the cache").count();
    assert_eq!(
        held,
        2,
        "{} holds a file per text and nothing else",
        cache.display()
    );
    let mode = fs::metadata(&a).map(|m| m.mode() & 0o7777);
    assert_eq!(mode.ok(), Some(0o555), "read-only and executable");
    if cfg!(target_os = "macos") {
        assert!(is_quiet(&cache), "{} is not quiet", cache.display());
    }
}

/// A slot not holding the text, or holding it still writable (its writer has not finished
/// it), is passed over and left as it is: the text goes in the next slot. NEGATIVE CONTROL:
/// the next call finds that slot again, and the passed-over ones still hold what they held.
#[cfg(unix)]
#[test]
fn a_slot_still_being_written_or_holding_another_text_is_passed_over() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let me = euid();
    let tmp = tempdir().expect("scratch");
    let text = "#!/bin/sh\nexit 5\n";
    let cache = quiet_in(tmp.path()).join(format!("{STAND_IN_CACHE}{me}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&cache)
        .expect("the cache");
    let slot = |n: u32| cache.join(format!("{:016x}-{n}", fnv1a(text.as_bytes())));
    let lay = |n: u32, bytes: &str, mode: u32| {
        fs::write(slot(n), bytes).expect("lay a slot");
        fs::set_permissions(slot(n), fs::Permissions::from_mode(mode)).expect("chmod a slot");
    };
    lay(0, "#!/bin/sh\nexit", 0o555);
    lay(1, text, 0o600);
    let got = cached_in(me, tmp.path(), text);
    assert_eq!(got, Some(slot(2)), "the first slot holding the text whole");
    assert_eq!(
        cached_in(me, tmp.path(), text),
        Some(slot(2)),
        "found again"
    );
    assert_eq!(
        fs::read_to_string(slot(0)).ok().as_deref(),
        Some("#!/bin/sh\nexit")
    );
    assert_eq!(fs::read_to_string(slot(1)).ok().as_deref(), Some(text));
}

/// A SLOT HANDED OUT LOOKS JUST USED. macOS's temporary-file cleaners take a file by its
/// access time, which `holds` reading a slot does not move on APFS (the slot is aged with
/// both times equal, which a read there leaves as they are): the slot each call hands out,
/// whether `cached_in` found it (made by an earlier process) or the memo named it, is handed
/// out with its access time made now. NEGATIVE CONTROL: only that: the slot handed out keeps
/// its modification time (it is not written), and a slot passed over keeps its age. That slot
/// is one a writer left mid-write (0600), which `holds` refuses without reading, aged to an
/// access time before its modification time, which a read would move on APFS as under
/// Linux's relatime, so the control fails on a mark of it, not on a filesystem's read policy.
#[cfg(unix)]
#[test]
fn a_slot_handed_out_is_marked_used_and_one_passed_over_is_not() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::time::{Duration, SystemTime};
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    const CLEANED: Duration = Duration::from_secs(3 * 24 * 60 * 60);
    let me = euid();
    let tmp = tempdir().expect("scratch");
    let text = "#!/bin/sh\nexit 8\n";
    let cache = quiet_in(tmp.path()).join(format!("{STAND_IN_CACHE}{me}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&cache)
        .expect("the cache");
    let other = cache.join(format!("{:016x}-0", fnv1a(text.as_bytes())));
    fs::write(&other, "#!/bin/sh\nexit").expect("lay a slot");
    fs::set_permissions(&other, fs::Permissions::from_mode(0o600)).expect("chmod a slot");
    let slot = cached_in(me, tmp.path(), text).expect("the text's slot");
    let unused = SystemTime::now() - CLEANED - DAY;
    let age = |file: &Path, accessed: SystemTime| {
        let times = fs::FileTimes::new()
            .set_accessed(accessed)
            .set_modified(unused);
        fs::File::open(file)
            .and_then(|f| f.set_times(times))
            .expect("aged");
    };
    let since = |file: &Path, of: fn(&fs::Metadata) -> io::Result<SystemTime>| {
        let at = fs::metadata(file).and_then(|m| of(&m)).expect("its times");
        SystemTime::now().duration_since(at).unwrap_or_default()
    };
    let mut shared = BTreeMap::new();
    for named in ["found in the cache", "named by the memo"] {
        age(&slot, unused);
        age(&other, unused - DAY);
        let got = shared_in(&mut shared, me, tmp.path(), text);
        assert_eq!(got.as_ref(), Some(&slot), "{named}");
        assert!(
            since(&slot, fs::Metadata::accessed) < CLEANED,
            "{} handed out ({named}) still looks unused",
            slot.display()
        );
        assert!(
            since(&slot, fs::Metadata::modified) >= CLEANED,
            "{} was written",
            slot.display()
        );
        assert!(
            since(&other, fs::Metadata::accessed) >= CLEANED,
            "{} passed over ({named}) was marked used",
            other.display()
        );
    }
}

/// A cache folder that is not the user's alone is refused and nothing is made in it: one
/// another user owns (the folder this process made, asked for as a user it is not, as a
/// shared `/tmp` can hold another user's folder of this user's name), one others may write,
/// and a symlink to a private folder of the user's own. NEGATIVE CONTROL: the folder others
/// could write, made private again, is used.
#[cfg(unix)]
#[test]
fn a_cache_folder_not_the_users_alone_is_refused() {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let me = euid();
    let text = "#!/bin/sh\nexit 6\n";
    let cache = |tmp: &Path, uid: u32| quiet_in(tmp).join(format!("{STAND_IN_CACHE}{uid}"));
    let held = |dir: &Path| fs::read_dir(dir).map(Iterator::count).ok();
    let private = |dir: &Path| {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .expect("a private folder");
    };

    let theirs = tempdir().expect("scratch");
    let other = me.wrapping_add(1);
    assert_eq!(
        cached_in(other, theirs.path(), text),
        None,
        "another user's"
    );
    assert_eq!(
        held(&cache(theirs.path(), other)),
        Some(0),
        "nothing in theirs"
    );

    let open = tempdir().expect("scratch");
    let folder = cache(open.path(), me);
    private(&folder);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o777)).expect("open it to others");
    assert_eq!(
        cached_in(me, open.path(), text),
        None,
        "one others may write"
    );
    assert_eq!(held(&folder), Some(0), "nothing in one others may write");
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).expect("make it private");
    let used = cached_in(me, open.path(), text);
    assert_eq!(
        used.as_deref().and_then(Path::parent),
        Some(folder.as_path())
    );

    let linked = tempdir().expect("scratch");
    let elsewhere = linked.path().join("elsewhere");
    private(&elsewhere);
    std::os::unix::fs::symlink(&elsewhere, cache(linked.path(), me)).expect("the link");
    assert_eq!(cached_in(me, linked.path(), text), None, "a symlink");
    assert_eq!(held(&elsewhere), Some(0), "nothing through the symlink");
}

/// OFF APPLE A CHILD WRITER REMOVES ONLY WHAT IT MADE. A file another writer makes at the
/// name after the writer looked (here the child's own line does, ahead of its open) is
/// refused by `set -C` and left whole, with `AlreadyExists` (`write_read_only` and the
/// stand-in cache both write through this). A file the child made and then failed to write
/// is removed, with the child's error. NEGATIVE CONTROL: a file the child makes and writes
/// whole is kept, holding the body, of one name, in the mode asked, and no writer leaves
/// the body it staged.
#[cfg(all(unix, not(target_vendor = "apple")))]
#[test]
fn a_child_writer_removes_only_what_it_made() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = tempdir().expect("a fixture");
    let theirs = dir.path().join("theirs");
    let raced = format!("echo theirs >\"$1\"; {WRITES_ITS_INPUT}");
    let refused = written_by(&theirs, b"mine\n", 0o555, &raced);
    assert_eq!(
        refused.map_err(|e| e.kind()),
        Err(io::ErrorKind::AlreadyExists)
    );
    assert_eq!(
        fs::read_to_string(&theirs).ok().as_deref(),
        Some("theirs\n"),
        "another writer's file is left whole"
    );
    let cut = dir.path().join("cut");
    let fails = "set -C; umask 077; { echo made; echo part >&3; exit 1; } 3>\"$1\"";
    let failed = written_by(&cut, b"whole\n", 0o555, fails)
        .expect_err("the child failed")
        .kind();
    assert_ne!(failed, io::ErrorKind::AlreadyExists);
    assert!(
        fs::symlink_metadata(&cut).is_err(),
        "{} is left part-written",
        cut.display()
    );
    let made = dir.path().join("made");
    written_by_a_child(&made, b"whole\n", 0o555).expect("written whole");
    let meta = fs::symlink_metadata(&made).expect("made");
    assert!(meta.is_file() && meta.nlink() == 1, "{meta:?}");
    assert_eq!(meta.mode() & 0o7777, 0o555);
    assert_eq!(fs::read(&made).ok().as_deref(), Some(&b"whole\n"[..]));
    let mut names: Vec<String> = fs::read_dir(dir.path())
        .expect("list")
        .map(|e| e.expect("entry").file_name().into_string().expect("UTF-8"))
        .collect();
    names.sort();
    assert_eq!(names, ["made", "theirs"]);
}

/// OFF APPLE A CHILD WRITER SETS A MODE ONLY ON THE FILE ITS CHILD MADE. A link to a FIFO
/// put at the name after the writer looked (here by the child's own line, ahead of its
/// open) is opened by `set -C`, and the child writes into the FIFO; the writer then refuses
/// the name as no file it made (`AlreadyExists`) and leaves the link, and the FIFO's mode,
/// as they were. NEGATIVE CONTROL: the bytes reached the FIFO, so the child did write
/// through the link.
#[cfg(target_os = "linux")]
#[test]
fn a_child_writers_mode_is_never_set_through_a_link() {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let dir = tempdir().expect("a fixture");
    let mode = |path: &Path| fs::symlink_metadata(path).map(|m| m.mode() & 0o7777).ok();
    let fifo = dir.path().join("fifo");
    let made = aterm_uds::deadline::output_within(
        aterm_uds::spawnfd::command("mkfifo")
            .args(["-m", "600"])
            .arg(&fifo),
        MUST_HAPPEN,
    )
    .expect("mkfifo ran");
    assert!(made.status.success(), "mkfifo: {made:?}");
    assert_eq!(mode(&fifo), Some(0o600));
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    assert!(no_link_no_wait(&mut opts), "a reader waits for no writer");
    let mut reader = opts.open(&fifo).expect("the FIFO's reader");
    let tool = dir.path().join("tool");
    let steered = format!("ln -s fifo \"$1\"; {WRITES_ITS_INPUT}");
    let refused = written_by(&tool, b"body\n", 0o755, &steered);
    let mut got = Vec::new();
    reader.read_to_end(&mut got).expect("what the child wrote");
    assert_eq!(got, b"body\n", "the child wrote through the link");
    assert_eq!(mode(&fifo), Some(0o600), "the FIFO's mode was set");
    assert!(
        fs::symlink_metadata(&tool).is_ok_and(|m| m.file_type().is_symlink()),
        "the link is left"
    );
    assert_eq!(
        refused.map_err(|e| e.kind()),
        Err(io::ErrorKind::AlreadyExists)
    );
}

/// THE QUIET FOLDER'S SHAPE: a directory asked for in a shared root goes in that root's
/// `.ubd` folder, so its path holds `/.ubd/`, and one asked for in a folder already under
/// one goes there as asked (no second `.ubd`). NEGATIVE CONTROL: unwanted (off macOS, or a
/// folder the caller chose) it stays in the folder asked for, and a name that only holds
/// `.ubd` is no quiet component.
#[test]
fn a_shared_roots_directories_go_in_its_quiet_folder() {
    let tmp = Path::new("/tmp");
    let quiet = quiet_place(tmp, true).expect("a shared root is made quiet");
    assert_eq!(quiet, Path::new("/tmp/.ubd"));
    let made = quiet.join("al-sim1_0123456789abcdef_0");
    let sep = std::path::MAIN_SEPARATOR;
    assert!(
        made.to_str()
            .is_some_and(|p| p.contains(&format!("{sep}.ubd{sep}"))),
        "{made:?}"
    );
    assert!(is_quiet(&quiet) && is_quiet(&made));
    assert_eq!(quiet_place(&made, true), None, "no second .ubd");
    assert_eq!(quiet_place(tmp, false), None);
    for loud in ["/tmp", "/tmp/x.ubd/y", "/tmp/.ubdx/y", "/tmp/ubd/y"] {
        assert!(!is_quiet(Path::new(loud)), "{loud}");
    }
    assert!(is_shared_root(tmp) && is_shared_root(&call0(std::env::temp_dir)));
    assert!(!is_shared_root(Path::new("/tmp/x")));
}

/// What the tempdirs land in: on macOS the system temp directory's and `/tmp`'s quiet
/// folders. `/tmp`'s may be one another program made (a `mkdir -p` of scratch under it,
/// `0755`), which serves while it is safe, so its mode is not held here: the `1777` one
/// made in a sticky root gets is `a_quiet_folder_is_used_only_while_it_is_safe`'s. NEGATIVE
/// CONTROL: a parent the caller chose gets its directory directly, and no `.ubd` beside it.
#[cfg(target_os = "macos")]
#[test]
fn tempdirs_in_shared_roots_land_where_fseventsd_drops_their_events() {
    let dir = tempdir().expect("tempdir");
    assert!(is_quiet(dir.path()), "{:?}", dir.path());
    assert!(dir.path().starts_with(call0(std::env::temp_dir)));
    let short = Builder::new()
        .prefix("atq-")
        .tempdir_in("/tmp")
        .expect("in /tmp");
    assert_eq!(short.path().parent(), Some(Path::new("/tmp/.ubd")));
    let chosen = Path::new("/tmp").join(unique_name("atq-chosen-"));
    fs::create_dir(&chosen).expect("a parent of the caller's");
    let inner = TempDir::new_in(&chosen).expect("in the chosen parent");
    assert_eq!(inner.path().parent(), Some(chosen.as_path()));
    drop(inner);
    assert_eq!(fs::read_dir(&chosen).expect("list").count(), 0);
    fs::remove_dir(&chosen).expect("cleanup");
}

/// A heard directory is made outside every quiet folder: `outside_quiet` cuts a path at its
/// first `.ubd` component. NEGATIVE CONTROL: a component that only holds the name
/// (`x.ubd`) cuts nothing, and a builder that is not heard places as before.
#[cfg(unix)]
#[test]
fn a_heard_directory_is_made_outside_every_quiet_folder() {
    assert_eq!(outside_quiet(Path::new("/v/T/.ubd/x/y")), Path::new("/v/T"));
    assert_eq!(outside_quiet(Path::new("/v/T/.ubd/")), Path::new("/v/T"));
    assert_eq!(outside_quiet(Path::new(".ubd/x")), Path::new("."));
    assert_eq!(outside_quiet(Path::new(".ubd")), Path::new("."));
    assert_eq!(outside_quiet(Path::new("")), Path::new(""));
    assert_eq!(
        outside_quiet(Path::new("/v/T/x.ubd/y")),
        Path::new("/v/T/x.ubd/y")
    );
    assert_eq!(outside_quiet(Path::new("/v/T")), Path::new("/v/T"));
    let heard = Builder::new()
        .prefix("ath-")
        .heard()
        .tempdir()
        .expect("heard");
    assert!(!is_quiet(heard.path()), "{:?}", heard.path());
    assert_eq!(
        heard.path().parent(),
        Some(outside_quiet(&call0(std::env::temp_dir)).as_path())
    );
    let in_tmp = Builder::new()
        .prefix("ath-")
        .heard()
        .tempdir_in("/tmp")
        .expect("/tmp");
    assert_eq!(in_tmp.path().parent(), Some(Path::new("/tmp")));
    let placed = Builder::new()
        .prefix("ath-")
        .tempdir_in("/tmp")
        .expect("/tmp");
    assert_eq!(
        is_quiet(placed.path()),
        cfg!(target_os = "macos"),
        "{:?}",
        placed.path()
    );
}

/// A quiet folder is used only while it is safe to: this user's own and closed, or sticky.
/// One this user's `mkdir -p` left (`0755`) is closed, so it serves. A sticky root reached
/// through a link, as `/tmp` is, gets a folder every user may share. NEGATIVE CONTROL: a
/// link in its place, or one anyone may write and not sticky, is refused, so the root
/// itself serves.
#[cfg(target_os = "macos")]
#[test]
fn a_quiet_folder_is_used_only_while_it_is_safe() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let root = tempdir().expect("root");
    let quiet = root.path().join(QUIET);
    assert!(made_quiet(root.path(), &quiet), "made");
    assert_eq!(fs::metadata(&quiet).expect("stat").mode() & 0o7777, 0o700);
    assert!(made_quiet(root.path(), &quiet), "found again");
    let chmod = |mode| fs::set_permissions(&quiet, fs::Permissions::from_mode(mode));
    chmod(0o755).expect("as mkdir -p leaves it");
    assert!(made_quiet(root.path(), &quiet), "found closed");
    chmod(0o777).expect("open it");
    assert!(!made_quiet(root.path(), &quiet), "open and not sticky");
    chmod(0o1777).expect("sticky");
    assert!(made_quiet(root.path(), &quiet), "sticky");
    fs::remove_dir(&quiet).expect("rmdir");
    std::os::unix::fs::symlink(root.path(), &quiet).expect("a link in its place");
    assert!(!made_quiet(root.path(), &quiet), "a link");
    fs::remove_file(&quiet).expect("unlink");
    let shared = root.path().join("shared");
    fs::create_dir(&shared).expect("a shared parent");
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).expect("sticky parent");
    assert!(made_quiet(&shared, &shared.join(QUIET)));
    let mode = fs::metadata(shared.join(QUIET)).expect("stat").mode();
    assert_eq!(mode & 0o7777, 0o1777, "{mode:o}");
    let linked_root = root.path().join("linked");
    fs::create_dir(&linked_root).expect("a second sticky root");
    fs::set_permissions(&linked_root, fs::Permissions::from_mode(0o1777)).expect("sticky");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&linked_root, &link).expect("a link to it, as /tmp is");
    assert!(made_quiet(&link, &link.join(QUIET)));
    let mode = fs::metadata(link.join(QUIET)).expect("stat").mode();
    assert_eq!(mode & 0o7777, 0o1777, "through a link: {mode:o}");
}

#[test]
fn made_in_sets_private_mode_retries_collisions_and_propagates_errors() {
    let root = tempdir().expect("root");
    let fresh = TempDir::new_in(root.path()).expect("fresh");
    assert!(fresh.path().is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let mode = fs::metadata(fresh.path()).expect("stat").mode() & 0o7777;
        assert_eq!(mode, 0o700, "Unix tempdirs are mode 0700: {mode:o}");
    }

    let colliding = root.path().join("taken");
    fs::create_dir(&colliding).expect("taken");
    let mut tries = 0usize;
    let won = TempDir::made_in_with(root.path(), || {
        tries += 1;
        if tries < 5 {
            "taken".to_string()
        } else {
            "free_on_fifth".to_string()
        }
    })
    .expect("fifth attempt succeeds");
    assert_eq!(tries, 5);
    assert_eq!(won.path(), root.path().join("free_on_fifth"));

    let mut exhausted_tries = 0usize;
    let exhausted = TempDir::made_in_with(root.path(), || {
        exhausted_tries += 1;
        "taken".to_string()
    })
    .unwrap_err();
    assert_eq!(exhausted_tries, 5);
    assert_eq!(exhausted.kind(), io::ErrorKind::AlreadyExists);

    let mut err_tries = 0usize;
    let missing = TempDir::made_in_with(&root.path().join("missing_parent"), || {
        err_tries += 1;
        "candidate".to_string()
    })
    .unwrap_err();
    assert_eq!(err_tries, 1, "non-collision errors return immediately");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}

/// On Windows the OS RNG (BCryptGenRandom) succeeds on empty and 8-byte buffers.
#[cfg(windows)]
#[test]
fn windows_os_random_succeeds() {
    let mut empty = [];
    let mut buf = [0u8; 8];
    assert!(
        read_os_random(&mut empty),
        "empty BCryptGenRandom read failed"
    );
    assert!(
        read_os_random(&mut buf),
        "8-byte BCryptGenRandom read failed"
    );
}
