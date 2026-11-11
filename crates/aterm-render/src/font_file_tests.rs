// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture_dir(label: &str) -> aterm_tempfile::TempDir {
    crate::test_scratch_dir(&format!("font-file-{label}"))
}

#[test]
fn exact_limit_is_accepted_and_one_byte_over_is_rejected() {
    let root = fixture_dir("limit");
    let exact = root.path().join("exact.ttf");
    let over = root.path().join("over.ttf");
    std::fs::write(&exact, [0x5a; 64]).expect("write exact-limit fixture");
    std::fs::write(&over, [0x5a; 65]).expect("write over-limit fixture");
    assert_eq!(read_bounded_font_file(&exact, 64).unwrap(), [0x5a; 64]);
    let error = read_bounded_font_file(&over, 64).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

#[test]
fn a_reused_buffer_reads_the_same_bytes_a_fresh_one_does() {
    let root = fixture_dir("reuse");
    let sizes = [4096usize, 17, 900, 1, 8192];
    let paths: Vec<_> = sizes
        .iter()
        .enumerate()
        .map(|(i, &n)| {
            let path = root.path().join(format!("face{i}.ttf"));
            std::fs::write(&path, vec![u8::try_from(i).unwrap(); n]).expect("write fixture");
            path
        })
        .collect();

    let mut buf = Vec::new();
    for path in &paths {
        read_font_file_into(path, &mut buf).expect("reused read");
        assert_eq!(buf, read_font_file(path).expect("fresh read"));
    }
    assert!(buf.capacity() >= *sizes.iter().max().unwrap());
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

#[test]
fn a_refused_path_leaves_no_stale_font_in_a_reused_buffer() {
    let root = fixture_dir("stale");
    let previous = root.path().join("previous.ttf");
    let over = root.path().join("over.ttf");
    let absent = root.path().join("absent.ttf");
    std::fs::write(&previous, [0xa5; 512]).expect("write previous-face fixture");
    std::fs::write(&over, [0x11; 4096]).expect("write over-limit fixture");

    let cases: [(&Path, usize, &str); 3] = [
        (absent.as_path(), 1024, "open_regular"),
        (over.as_path(), 1024, "check_observed_size"),
        (previous.as_path(), usize::MAX, "limit_plus_sentinel"),
    ];

    let mut buf = Vec::new();
    for (path, max_bytes, step) in cases {
        read_bounded_font_file_into(&previous, 1024, &mut buf).expect("previous face reads");
        assert_eq!(buf.len(), 512, "the reused buffer holds the previous face");
        read_bounded_font_file_into(path, max_bytes, &mut buf)
            .expect_err("a refused path must not be admitted");
        assert!(
            buf.is_empty(),
            "{step} refused the path and left {} of the PREVIOUS font's bytes behind",
            buf.len()
        );
    }
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

#[test]
fn a_fresh_read_allocates_no_slack() {
    let root = fixture_dir("exact-capacity");
    let path = root.path().join("face.ttf");
    std::fs::write(&path, vec![0x5a; 100_003]).expect("write fixture");
    let bytes = read_font_file(&path).expect("fresh read");
    assert_eq!(bytes.len(), 100_003);
    assert_eq!(bytes.capacity(), bytes.len());
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

#[test]
fn same_open_handle_supplies_bytes_after_path_replacement() {
    let root = fixture_dir("same-handle");
    let path = root.path().join("face.ttf");
    let moved = root.path().join("opened.ttf");
    std::fs::write(&path, b"original-font-bytes").expect("write original fixture");

    let (mut opened, _, _) = open_regular(&path).expect("open admitted handle");
    std::fs::rename(&path, &moved).expect("retain opened inode under another name");
    std::fs::write(&path, b"replacement-font").expect("replace logical path");

    let mut bytes = Vec::new();
    opened
        .read_to_end(&mut bytes)
        .expect("read admitted handle");
    assert_eq!(bytes, b"original-font-bytes");
    assert_eq!(std::fs::read(&path).unwrap(), b"replacement-font");
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

#[test]
fn maps_in_place_admits_the_system_font_volume_and_nothing_user_writable() {
    #[cfg(target_os = "macos")]
    {
        assert!(maps_in_place(Path::new("/System/Library/Fonts/Menlo.ttc")));
        assert!(maps_in_place(Path::new(
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"
        )));
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        assert!(!maps_in_place(Path::new("/System/Library/Fonts/Menlo.ttc")));
        assert!(!maps_in_place(Path::new(
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"
        )));
    }
    assert!(!maps_in_place(Path::new("/Library/Fonts/Custom.ttf")));
    assert!(!maps_in_place(Path::new(
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
    )));
    assert!(!maps_in_place(Path::new(
        "/System/Library/Fonts/../../../Users//x/y.ttf"
    )));
    assert!(!maps_in_place(Path::new("System/Library/Fonts/Menlo.ttc")));
    #[cfg(windows)]
    {
        assert!(maps_in_place(Path::new("C:\\Windows\\Fonts\\msyh.ttc")));
        assert!(maps_in_place(Path::new("C:\\WINDOWS\\Fonts\\SegUIVar.ttf")));
        assert!(maps_in_place(Path::new("c:/windows/fonts/seguisb.ttf")));
        assert!(!maps_in_place(Path::new("C:\\Windows\\FontsX\\a.ttf")));
        assert!(!maps_in_place(Path::new("C:\\Windows")));
        assert!(!maps_in_place(Path::new("/System/Library/Fonts/Menlo.ttc")));
        assert!(!maps_in_place(Path::new("C:\\font-fixture\\café.ttf")));
        assert!(!maps_in_place(Path::new(
            "C:\\Users\\x\\AppData\\Local\\Microsoft\\Windows\\Fonts\\Font.ttf"
        )));
        assert!(!maps_in_place(Path::new("C:\\Windows\\Temp\\Font.ttf")));
        assert!(!maps_in_place(Path::new(
            "C:\\Windows\\Fonts\\..\\Temp\\Font.ttf"
        )));
    }
    #[cfg(not(windows))]
    assert!(!maps_in_place(Path::new("C:\\Windows\\Fonts\\msyh.ttc")));
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        for user_dir in ["Library/Fonts", ".fonts", ".local/share/fonts"] {
            assert!(
                !maps_in_place(&home.join(user_dir).join("Font.ttf")),
                "~/{user_dir} is user-writable and must be COPIED"
            );
        }
    }
}

#[test]
fn admit_font_file_copies_a_file_off_the_system_font_volume() {
    let root = fixture_dir("admit-policy");
    let outside = root.path().join("outside.ttf");
    std::fs::write(&outside, [0x42; 4099]).expect("write outside fixture");
    assert!(!maps_in_place(&outside));
    let admitted = admit_font_file(&outside).expect("a regular file admits");
    assert!(
        matches!(admitted, crate::font::FaceBytes::Vec(_)),
        "a path off the system font volume must be COPIED, got {admitted:?}"
    );
    assert_eq!(&admitted[..], &read_font_file(&outside).unwrap()[..]);

    let empty = root.path().join("empty.ttf");
    std::fs::write(&empty, []).expect("write empty fixture");
    let admitted = admit_font_file(&empty).expect("an empty regular file still admits");
    assert!(
        admitted.is_empty(),
        "the copy arm carries the empty file as-is"
    );

    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

/// The mapping arm answers the SAME bytes the copy arm does: a file on the
/// admitted system font volume is MAPPED (macOS, Windows; elsewhere nothing
/// is).
#[cfg(any(target_os = "macos", windows))]
#[test]
fn admit_font_file_maps_a_file_on_the_system_font_volume() {
    let candidate = MAPPABLE_FONT_ROOTS
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "ttf" | "otf" | "ttc"))
                && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
        });
    let Some(system_font) = candidate else {
        aterm_spec::host::could_not_run(
            "no font file on the system font volume on this host to map",
        );
    };
    assert!(maps_in_place(&system_font));
    let admitted = admit_font_file(&system_font).expect("a system font admits");
    assert!(
        matches!(admitted, crate::font::FaceBytes::Mapped(_)),
        "{}: a file on the system font volume must be MAPPED, got {admitted:?}",
        system_font.display()
    );
    assert_eq!(
        &admitted[..],
        &read_font_file(&system_font).unwrap()[..],
        "the mapping must present exactly the file's bytes"
    );
    let again = admit_font_file(&system_font).expect("a system font admits twice");
    match (&admitted, &again) {
        (crate::font::FaceBytes::Mapped(a), crate::font::FaceBytes::Mapped(b)) => {
            assert!(a.same_file(b));
            assert!(admitted.same_bytes(&again));
        }
        _ => unreachable!("both admissions were asserted mapped"),
    }
}

/// The Windows stand-in for a read-only volume, checked against the OS: while a view is live,
/// opening the file for writing fails with a sharing violation (the mapping keeps the
/// no-write-share handle), readers still open it, and the refusal lifts with the view.
#[cfg(windows)]
#[test]
fn a_mapped_font_refuses_writers_until_it_is_dropped() {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let root = fixture_dir("win-map-share");
    let path = root.path().join("face.ttf");
    std::fs::write(&path, b"mapped-font-bytes").expect("write fixture");

    let (file, metadata) =
        open_regular_shared(&path, win::FILE_SHARE_READ | win::FILE_SHARE_DELETE)
            .expect("the mapping open succeeds with no writer present");
    let length = check_observed_size(&metadata, MAX_FONT_FILE_BYTES).expect("size");
    let mapped = MappedFontFile::map(&file, &metadata, length).expect("map the fixture");
    drop(file);
    assert_eq!(&mapped[..], b"mapped-font-bytes");

    let writer = std::fs::OpenOptions::new().write(true).open(&path);
    assert_eq!(
        writer.as_ref().err().and_then(io::Error::raw_os_error),
        Some(ERROR_SHARING_VIOLATION),
        "a writer opened the file while its bytes were a live mapping: {writer:?}"
    );
    assert_eq!(
        read_font_file(&path).expect("readers are not refused"),
        b"mapped-font-bytes"
    );

    drop(mapped);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("the write refusal must lift with the mapping");
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

/// The servicing path the mapping keeps open, checked against the OS: the
/// admission grants `FILE_SHARE_DELETE`, so while a view is live the file
/// can still be RENAMED and its name reused (a font replaced while aterm
/// runs), and the view, bound to the file object rather than to its name,
/// still reads the original bytes.
#[cfg(windows)]
#[test]
fn a_mapped_font_can_be_renamed_and_replaced_and_its_view_keeps_the_original() {
    let root = fixture_dir("win-map-rename");
    let path = root.path().join("face.ttf");
    let renamed = root.path().join("face-old.ttf");
    std::fs::write(&path, b"original-font-bytes").expect("write fixture");

    let (file, metadata) =
        open_regular_shared(&path, win::FILE_SHARE_READ | win::FILE_SHARE_DELETE)
            .expect("the mapping open succeeds with no writer present");
    let length = check_observed_size(&metadata, MAX_FONT_FILE_BYTES).expect("size");
    let mapped = MappedFontFile::map(&file, &metadata, length).expect("map the fixture");
    drop(file);

    std::fs::rename(&path, &renamed).expect("a mapped font must still be renameable");
    std::fs::write(&path, b"replacement").expect("and its name reusable");
    assert_eq!(
        &mapped[..],
        b"original-font-bytes",
        "the view must still read the ORIGINAL bytes after its name was replaced"
    );
    assert_eq!(
        read_font_file(&path).expect("the replacement reads"),
        b"replacement"
    );

    drop(mapped);
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

/// A mappable file a writer already holds is not mapped (its bytes could change under the
/// view) nor refused: it is copied through the fully-shared open.
#[cfg(windows)]
#[test]
fn a_mappable_font_a_writer_holds_is_copied_not_refused() {
    let root = fixture_dir("win-map-writer");
    let path = root.path().join("face.ttf");
    std::fs::write(&path, b"held-by-a-writer").expect("write fixture");
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open the writer");

    let (file, metadata, may_map) =
        open_for_admission(&path, |_| true).expect("a held file still admits");
    assert!(!may_map, "a file a writer holds must not be mapped");
    let length = check_observed_size(&metadata, MAX_FONT_FILE_BYTES).expect("size");
    let mut bytes = Vec::new();
    read_admitted_handle(file, length, MAX_FONT_FILE_BYTES, &mut bytes).expect("copy");
    assert_eq!(bytes, b"held-by-a-writer");

    drop(writer);
    let (_, _, may_map) = open_for_admission(&path, |_| true).expect("admits once released");
    assert!(may_map, "with no writer the same file is mappable again");
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

/// A writable view keeps its section alive even after both handles close.
#[cfg(windows)]
#[test]
fn a_writable_view_without_its_handles_is_copied_not_mapped() {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

    struct WritableView(std::ptr::NonNull<std::ffi::c_void>);

    impl Drop for WritableView {
        fn drop(&mut self) {
            // SAFETY: this guard owns the live view returned below and unmaps it once, including when an assertion unwinds.
            unsafe { win::UnmapViewOfFile(self.0.as_ptr()) };
        }
    }

    const PAGE_READWRITE: u32 = 0x04;
    const FILE_MAP_WRITE: u32 = 0x0002;
    let root = fixture_dir("win-map-prior-view");
    let path = root.path().join("face.ttf");
    let expected = b"held-by-a-writable-view";
    std::fs::write(&path, expected).expect("write fixture");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open the writable mapping file");
    // SAFETY: the file is open for reading/writing and is nonempty; an unnamed PAGE_READWRITE section covers its existing contents.
    let raw_section = unsafe {
        win::CreateFileMappingW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            PAGE_READWRITE,
            0,
            0,
            std::ptr::null(),
        )
    };
    assert!(!raw_section.is_null(), "{}", io::Error::last_os_error());
    // SAFETY: CreateFileMappingW returned a live handle, transferred to this guard so a later failure still closes it exactly once.
    let section = unsafe { OwnedHandle::from_raw_handle(raw_section) };
    // SAFETY: the section is live and contains expected.len() bytes.
    let raw_view = unsafe {
        win::MapViewOfFile(
            section.as_raw_handle(),
            FILE_MAP_WRITE,
            0,
            0,
            expected.len(),
        )
    };
    let view = WritableView(
        std::ptr::NonNull::new(raw_view)
            .unwrap_or_else(|| panic!("{}", io::Error::last_os_error())),
    );
    drop(section);
    drop(file);

    let (file, metadata, may_map) =
        open_for_admission(&path, |_| true).expect("a live view still admits");
    assert!(
        !may_map,
        "a writable view without handles must prevent mapping"
    );
    let length = check_observed_size(&metadata, MAX_FONT_FILE_BYTES).expect("size");
    let mut bytes = Vec::new();
    read_admitted_handle(file, length, MAX_FONT_FILE_BYTES, &mut bytes).expect("copy");
    assert_eq!(bytes, expected);

    drop(view);
    let (file, metadata, may_map) =
        open_for_admission(&path, |_| true).expect("admits after unmapping");
    assert!(
        may_map,
        "the same file is mappable after its writable view closes"
    );
    let mapped = MappedFontFile::map(&file, &metadata, length).expect("map the released fixture");
    assert_eq!(&mapped[..], expected);
    drop(mapped);
    drop(file);
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

#[cfg(unix)]
#[test]
fn writerless_fifo_and_links_to_it_return_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let root = fixture_dir("special");
    let fifo = root.path().join("face.fifo");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
    // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let error =
        read_bounded_font_file(&fifo, 64).expect_err("writerless FIFO must fail without blocking");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let to_fifo = root.path().join("fifo-link.ttf");
    std::os::unix::fs::symlink(&fifo, &to_fifo).expect("create link to FIFO");
    let error = read_bounded_font_file(&to_fifo, 64)
        .expect_err("a link to a writerless FIFO must fail without blocking");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let to_device = root.path().join("zero-link.ttf");
    std::os::unix::fs::symlink("/dev/zero", &to_device).expect("create link to device");
    let error =
        read_bounded_font_file(&to_device, 64).expect_err("a link to a device must not be read");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}

/// A font installed as a LINK (Ubuntu's `UbuntuMono-R.ttf`, a stow or
/// home-manager font in `~/.local/share/fonts`) admits as the file it
/// names, through every entry point; a link loop and a dangling link do
/// not; and residency is decided on the file opened, so a copy is a copy.
#[cfg(unix)]
#[test]
fn a_link_to_a_regular_font_admits_as_its_target() {
    let root = fixture_dir("links");
    let store = root.path().join("store");
    std::fs::create_dir_all(&store).expect("create link-target directory");
    let regular = store.join("Face[wght].ttf");
    std::fs::write(&regular, b"font-bytes").expect("write link target");
    let linked = root.path().join("Face-R.ttf");
    std::os::unix::fs::symlink(&regular, &linked).expect("create final link");
    let chained = root.path().join("Face-B.ttf");
    std::os::unix::fs::symlink("Face-R.ttf", &chained).expect("create chained link");

    for path in [&linked, &chained] {
        validate_font_file(path).expect("a link to a regular file validates");
        assert_eq!(read_font_file(path).expect("and reads"), b"font-bytes");
        let mut buf = Vec::new();
        read_font_file_into(path, &mut buf).expect("and reads into a buffer");
        assert_eq!(buf, b"font-bytes");
        let admitted = admit_font_file(path).expect("and admits");
        assert_eq!(&admitted[..], b"font-bytes");
        assert!(matches!(admitted, crate::font::FaceBytes::Vec(_)));
    }

    let a = root.path().join("loop-a.ttf");
    let b = root.path().join("loop-b.ttf");
    std::os::unix::fs::symlink(&b, &a).expect("create loop link a");
    std::os::unix::fs::symlink(&a, &b).expect("create loop link b");
    assert!(read_font_file(&a).is_err(), "a link loop must stay refused");
    let dangling = root.path().join("dangling.ttf");
    std::os::unix::fs::symlink(root.path().join("absent.ttf"), &dangling)
        .expect("create dangling link");
    assert_eq!(
        read_font_file(&dangling).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    std::fs::remove_dir_all(root.path()).expect("remove font-file fixtures");
}
