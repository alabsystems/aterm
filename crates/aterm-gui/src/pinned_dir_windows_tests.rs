// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{
    PinnedDir, imp::rename_payload, imp::test_is_direct_child, imp::test_link_count,
    windows_component_is_safe,
};

/// The `FILE_RENAME_INFO` payload carries a NUL terminator inside the request bytes at every
/// destination length.
#[test]
fn rename_payload_nul_terminates_its_destination_at_every_length() {
    let mut zero_slack_seen = 0usize;
    for extra in 0..4usize {
        let destination = std::path::Path::new(r"\\?\C:\aterm-rename-payload")
            .join("x".repeat(extra + 1))
            .join("artifact.png");
        let payload = rename_payload(&destination, true).expect("payload builds");
        let expected = {
            use std::os::windows::ffi::OsStrExt as _;
            destination.as_os_str().encode_wide().collect::<Vec<_>>()
        };

        assert_eq!(
            payload.file_name_length() as usize,
            expected.len() * 2,
            "FileNameLength states the NAME's bytes, excluding the terminator"
        );

        let units = payload.file_name_units();
        assert!(
            units.len() > expected.len(),
            "the request bytes must hold one more unit than the name itself \
             (destination {} units, request {} units)",
            expected.len(),
            units.len()
        );
        assert_eq!(&units[..expected.len()], &expected[..], "name round-trips");
        assert_eq!(
            units[expected.len()],
            0,
            "the unit after the name must be the NUL the kernel stops on"
        );

        if (std::mem::offset_of!(
            aterm_win32::Win32::Storage::FileSystem::FILE_RENAME_INFO,
            FileName
        ) + expected.len() * 2)
            % std::mem::size_of::<usize>()
            == 0
        {
            zero_slack_seen += 1;
        }
    }
    assert!(
        zero_slack_seen > 0,
        "the sweep must cover the length where allocation padding supplies no zero byte"
    );
}

fn unique_dir(stem: &str) -> std::path::PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!(
            "aterm-pinned-{stem}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
}

#[test]
fn retained_windows_handles_validate_publish_and_delete_exact_children() {
    let root = unique_dir("windows-lifecycle");
    std::fs::create_dir_all(&root).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let index = pinned
        .write_new_private(std::ffi::OsStr::new("index.json"), b"whole")
        .unwrap();
    pinned.sync().unwrap();
    pinned.validate_path_identity().unwrap();
    index.validate_path_identity().unwrap();
    let (bytes, read) = pinned
        .read_private(std::ffi::OsStr::new("index.json"), 32)
        .unwrap();
    assert_eq!(bytes, b"whole");
    read.validate_path_identity().unwrap();
    drop(index);
    std::fs::remove_file(root.join("index.json"))
        .expect("a retained read guard permits deletion, as an open Unix fd does");
    assert!(
        read.validate_path_identity().is_err(),
        "a removed artifact must fail the read guard's identity revalidation"
    );
    drop(read);

    let child = pinned
        .create_child(std::ffi::OsStr::new("recording"))
        .unwrap();
    let frame = child
        .write_new_private(std::ffi::OsStr::new("frame.png"), b"png")
        .unwrap();
    frame.validate_path_identity().unwrap();
    drop(frame);
    pinned
        .remove_child_tree_exact(std::ffi::OsStr::new("recording"), &child)
        .unwrap();
    drop(child);
    assert!(!root.join("recording").exists());
    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn windows_device_stream_and_normalization_aliases_are_rejected() {
    for unsafe_name in [
        "CON",
        "con.txt",
        "CONIN$",
        "conout$.log",
        "COM0",
        "COM1",
        "COM¹",
        "LPT².txt",
        "name:stream",
        "trailing.",
        "trailing ",
    ] {
        assert!(
            !windows_component_is_safe(std::ffi::OsStr::new(unsafe_name)),
            "{unsafe_name:?} must be rejected before CreateFileW"
        );
    }
    for safe_name in ["capture.png", "complete.json", "company.txt", "lptx"] {
        assert!(
            windows_component_is_safe(std::ffi::OsStr::new(safe_name)),
            "{safe_name:?} is an ordinary filename"
        );
    }
}

#[test]
fn windows_hardlink_target_is_rejected_without_mutating_victim() {
    let root = unique_dir("windows-hardlink");
    std::fs::create_dir_all(&root).unwrap();
    let victim = root.join("victim");
    std::fs::write(&victim, b"keep-me").unwrap();
    std::fs::hard_link(&victim, root.join("shot.png")).unwrap();
    let links_before = test_link_count(&victim).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();

    assert!(
        pinned
            .write_private(std::ffi::OsStr::new("shot.png"), b"replace")
            .is_err(),
        "a multiply-linked final component must fail closed"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep-me");
    assert_eq!(
        test_link_count(&victim).unwrap(),
        links_before,
        "neither hardlink name is removed or replaced"
    );

    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn windows_guards_deny_delete_until_exact_handles_drop() {
    let root = unique_dir("windows-deny-delete");
    let moved = root.with_extension("moved");
    std::fs::create_dir_all(&root).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let file_path = root.join("shot.png");
    let renamed_path = root.join("renamed.png");
    let file = pinned
        .write_new_private(std::ffi::OsStr::new("shot.png"), b"png")
        .unwrap();

    assert!(
        std::fs::remove_file(&file_path).is_err(),
        "a live PinnedFile denies ordinary deletion"
    );
    assert!(
        std::fs::rename(&file_path, &renamed_path).is_err(),
        "a live PinnedFile denies ordinary rename"
    );
    drop(file);
    std::fs::rename(&file_path, &renamed_path)
        .expect("ordinary file rename succeeds after guard drop");
    std::fs::rename(&renamed_path, &file_path).unwrap();

    assert!(
        std::fs::rename(&root, &moved).is_err(),
        "a live PinnedDir denies ancestor rename"
    );
    drop(pinned);
    std::fs::rename(&root, &moved)
        .expect("ancestor rename succeeds after every directory guard drops");

    let _ = std::fs::remove_dir_all(moved);
}

/// The Windows posture of a directory reached through `child`, stated in
/// code: it SHARES delete, so another opener may rename it (and every
/// retained chain through it then fails closed), but NTFS refuses to
/// rename any directory above a live handle, so its ancestors stay put.
#[test]
fn windows_child_pins_share_delete_but_pin_every_ancestor() {
    let root = unique_dir("windows-child-posture");
    std::fs::create_dir_all(root.join("video").join("recording")).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let video = pinned.child(std::ffi::OsStr::new("video")).unwrap();
    let recording = video.child(std::ffi::OsStr::new("recording")).unwrap();

    assert!(
        std::fs::rename(root.join("video"), root.join("video-moved")).is_err(),
        "a live pin beneath `video` keeps it in place, though `video` shares delete"
    );
    std::fs::rename(
        root.join("video").join("recording"),
        root.join("video").join("recording-moved"),
    )
    .expect("a `child` pin shares delete, so the pinned leaf itself may be renamed");
    assert!(
        recording.validate_path_identity().is_err(),
        "the chain notices its leaf moved and fails closed"
    );
    video
        .validate_path_identity()
        .expect("the renamed leaf's parent chain is intact");

    drop(recording);
    drop(video);
    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
}

/// A pathname open inside a `child` directory is trusted only once its
/// handle is proven to sit in the retained directory — by the path the
/// file was actually opened through, so a hard link placed inside cannot
/// vouch for the same file reached from outside.
#[test]
fn windows_direct_child_proof_follows_the_link_that_was_opened() {
    let root = unique_dir("windows-direct-child");
    let inside = root.join("inside");
    let outside = root.join("outside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("victim"), b"keep-me").unwrap();
    std::fs::hard_link(outside.join("victim"), inside.join("victim")).unwrap();

    test_is_direct_child(&inside, &inside.join("victim"))
        .expect("the link inside the retained directory is its direct child");
    assert!(
        test_is_direct_child(&inside, &outside.join("victim")).is_err(),
        "the same file opened through the outside link is not"
    );
    assert!(
        test_is_direct_child(&inside, &root).is_err(),
        "nor is the retained directory's own parent"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// A directory junction at `link`: unlike `symlink_dir` it needs no
/// privilege or Developer Mode, so the tests that use it run on every
/// host, but not under sshd's Redirection Guard, which follows no
/// junction this user made: there a refusal through it is the guard's,
/// not the code's under test, so they end COULD NOT RUN.
pub(crate) fn junction(link: &std::path::Path, target: &std::path::Path) {
    aterm_uds::host::could_not_run_under_redirection_guard(
        "the test plants a junction this user makes where the code under test goes",
    );
    let plain = |path: &std::path::Path| {
        let text = path.to_str().expect("test paths are UTF-8");
        text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
    };
    let status = aterm_uds::spawnfd::command("cmd")
        .args(["/C", "mklink", "/J", &plain(link), &plain(target)])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J {link:?} {target:?}");
}

fn sorted_names(directory: &std::path::Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// A recording is a `child` directory, which shares delete,
/// and between two frame writes nothing is open inside it — so another
/// process can rename it aside and put a junction in its place.
#[test]
fn windows_swapped_recording_is_neither_written_nor_read_through_the_swap() {
    use std::ffi::OsStr;

    let root = unique_dir("windows-swapped-recording");
    let outside = unique_dir("windows-swapped-recording-outside");
    let named = root.join("rec");
    let moved = root.join("rec-moved");
    std::fs::create_dir_all(&named).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let recording = pinned.child(OsStr::new("rec")).unwrap();
    let member = |name: &str, bytes: &[u8]| {
        recording
            .write_new_private_deferred_dir_sync_authorized(OsStr::new(name), bytes, || true)
            .and_then(super::PinnedFile::into_identity)
    };
    member("frame_0001.png", b"one").expect("an unswapped recording takes its frame");

    std::fs::rename(&named, &moved)
        .expect("nothing is open inside the recording between two frames");
    junction(&named, &outside);
    std::fs::write(outside.join("planted.png"), b"planted").unwrap();

    assert!(
        member("frame_0002.png", b"two").is_err(),
        "a batch member refuses a directory that is not the one it retains"
    );
    assert!(
        recording
            .write_new_private(OsStr::new("index.json"), b"{}")
            .is_err(),
        "a fully validated publication refuses the swapped chain"
    );
    assert!(
        recording
            .write_private(OsStr::new("frame_0001.png"), b"replaced")
            .is_err(),
        "so does a replacing one"
    );
    assert!(
        recording
            .pin_private_file_at_retained(OsStr::new("planted.png"))
            .is_err(),
        "a file planted in the junction's target is not the recording's"
    );
    assert!(
        recording
            .private_file_identity_at_retained(OsStr::new("planted.png"))
            .is_err(),
        "nor may it vouch for a frame seal"
    );
    assert!(
        recording
            .remove_file_if_exists(OsStr::new("planted.png"))
            .is_err(),
        "nor be removed through the swap"
    );
    assert_eq!(
        sorted_names(&outside),
        ["planted.png"],
        "nothing was written into, removed from, or left in the junction's target"
    );
    assert_eq!(
        std::fs::read(outside.join("planted.png")).unwrap(),
        b"planted"
    );

    std::fs::remove_dir(&named).unwrap();
    junction(&named, &moved);
    member("frame_0003.png", b"three").expect("the retained directory takes its frame");
    assert!(recording.validate_path_identity().is_err());
    assert_eq!(sorted_names(&moved), ["frame_0001.png", "frame_0003.png"]);
    assert_eq!(sorted_names(&outside), ["planted.png"]);

    std::fs::remove_dir(&named).unwrap();
    drop(recording);
    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

/// A retention tombstone renamed under a junction'd parent name is never
/// cleared through the junction: the clear opens every entry where the
/// retained handle IS and proves it a direct child before removing it.
#[test]
fn windows_renamed_recording_clears_only_what_it_retains() {
    use std::ffi::OsStr;

    let root = unique_dir("windows-renamed-clear");
    let outside = unique_dir("windows-renamed-clear-outside");
    std::fs::create_dir_all(root.join("rec")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("rec").join("frame_0001.png"), b"one").unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let recording = pinned.child(OsStr::new("rec")).unwrap();

    std::fs::rename(root.join("rec"), root.join("rec-moved")).unwrap();
    junction(&root.join("rec"), &outside);

    let mut budget = 16;
    let cleared = recording.clear_contents_with_budget(&mut budget);
    cleared
        .result
        .expect("the clear follows the retained handle to its new name");
    assert_eq!(cleared.removed, 1);
    assert!(sorted_names(&root.join("rec-moved")).is_empty());
    assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
    assert!(
        pinned
            .remove_empty_child_exact(OsStr::new("rec"), &recording)
            .is_err(),
        "the junction on the old name is not the retained recording"
    );
    assert!(root.join("rec-moved").is_dir());
    assert_eq!(sorted_names(&outside), ["keep.txt"]);

    std::fs::remove_dir(root.join("rec")).unwrap();
    drop(recording);
    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn windows_reparse_directory_is_never_followed_during_cleanup() {
    let root = unique_dir("windows-reparse");
    let outside = unique_dir("windows-reparse-outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
    junction(&root.join("recording"), &outside);
    let pinned = PinnedDir::open(&root).unwrap();
    assert!(
        pinned
            .remove_child_tree(std::ffi::OsStr::new("recording"))
            .is_err(),
        "a junction is not opened as a recording directory"
    );
    assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
    drop(pinned);
    let _ = std::fs::remove_dir(root.join("recording"));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}
