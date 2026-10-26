// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's own fetch of its browsers, against a stand-in downloader and small archives
//! built here: nothing reaches the network and no browser is run. An archive held to its pin is
//! placed where the finder reads it (the headless shell's, and the full build's where the window
//! browser's finder reads it); one off its pin (size, sha256, cut short, no download, no
//! executable) installs nothing, leaves no scratch and is kept as the archive's last failure
//! until an install of it succeeds; a stop installs nothing and keeps nothing; a stop ends a
//! running download's process group.

use super::*;

/// The executable inside the test archive, as the mac-arm64 pin lays it.
const SHELL: &str = "chrome-headless-shell-mac-arm64/chrome-headless-shell";

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A stored (uncompressed) zip of `members` (`name`, unix mode, bytes).
fn zip(members: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let le16 = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
    let le32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, mode, data) in members {
        let crc = crc32(data);
        let size = u32::try_from(data.len()).unwrap();
        let name_len = u16::try_from(name.len()).unwrap();
        let offset = u32::try_from(out.len()).unwrap();
        le32(&mut out, 0x0403_4b50);
        for v in [20, 0, 0, 0, 0] {
            le16(&mut out, v);
        }
        le32(&mut out, crc);
        le32(&mut out, size);
        le32(&mut out, size);
        le16(&mut out, name_len);
        le16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        le32(&mut central, 0x0201_4b50);
        le16(&mut central, (3 << 8) | 20);
        for v in [20, 0, 0, 0, 0] {
            le16(&mut central, v);
        }
        le32(&mut central, crc);
        le32(&mut central, size);
        le32(&mut central, size);
        le16(&mut central, name_len);
        for v in [0, 0, 0, 0] {
            le16(&mut central, v);
        }
        le32(&mut central, mode << 16);
        le32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = u32::try_from(out.len()).unwrap();
    let cd_size = u32::try_from(central.len()).unwrap();
    let count = u16::try_from(members.len()).unwrap();
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, count);
    le16(&mut out, count);
    le32(&mut out, cd_size);
    le32(&mut out, cd_offset);
    le16(&mut out, 0);
    out
}

fn sha256(bytes: &[u8]) -> String {
    aterm_digest::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The archive a pin names for `bytes`.
fn archive_of(bytes: &[u8]) -> Archive {
    Archive {
        url: "https://downloads.example.invalid/chrome-headless-shell-mac-arm64.zip".to_owned(),
        sha256: sha256(bytes),
        bytes: u64::try_from(bytes.len()).unwrap(),
        dir: "chrome-headless-shell-0.0.0-mac-arm64-harness".to_owned(),
        binary: PathBuf::from(SHELL),
        within: FETCH_WITHIN,
    }
}

/// The shell's tree: its executable and a library beside it.
fn shell_zip() -> Vec<u8> {
    zip(&[
        (SHELL, 0o100_755, b"not a real browser\n"),
        (
            "chrome-headless-shell-mac-arm64/libsomething.dylib",
            0o100_644,
            b"lib",
        ),
    ])
}

/// The downloader as a stand-in: it writes `bytes` (all of them, or the first `cut` and then
/// fails, as a dropped connection does), or fails before writing anything; it counts its calls.
struct StandIn {
    bytes: Vec<u8>,
    cut: Option<usize>,
    fail: Option<&'static str>,
    calls: usize,
    /// Run while the download runs: what another process does meanwhile.
    during: Option<Box<dyn FnMut()>>,
}

impl StandIn {
    fn serving(bytes: Vec<u8>) -> StandIn {
        StandIn {
            bytes,
            cut: None,
            fail: None,
            calls: 0,
            during: None,
        }
    }
}

impl Fetch for StandIn {
    fn fetch(
        &mut self,
        url: &str,
        dest: &Path,
        bytes: u64,
        within: Duration,
        stopped: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        self.calls += 1;
        assert!(within > Duration::ZERO);
        if stopped() {
            // As curl's run ends on a stop: part of the file written, then killed.
            std::fs::write(dest, &self.bytes[..self.bytes.len() / 2]).expect("download");
            return Err(STOPPED.to_owned());
        }
        assert!(url.starts_with("https://"), "{url}");
        assert!(bytes > 0);
        if let Some(why) = self.fail {
            return Err(why.to_owned());
        }
        if let Some(during) = self.during.as_mut() {
            during();
        }
        let n = self.cut.unwrap_or(self.bytes.len());
        std::fs::write(dest, &self.bytes[..n]).expect("download");
        if self.cut.is_some() {
            return Err("the connection dropped".to_owned());
        }
        Ok(())
    }
}

/// A caller never asked to stop.
fn go() -> bool {
    false
}

/// What `root` holds, by name.
fn names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// An archive held to its pin is placed, executable, where the finder reads the cache, with no
/// scratch left; a second install finds it in place and downloads nothing.
#[test]
fn a_download_held_to_its_pin_is_placed_where_the_finder_reads() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("browser.noindex");
    let bytes = shell_zip();
    let archive = archive_of(&bytes);
    let mut fetch = StandIn::serving(bytes);
    let placed = install(&root, &archive, &mut fetch, &go).expect("installed");
    assert_eq!(placed, root.join(&archive.dir).join(SHELL));
    assert!(is_executable(&placed));
    assert_eq!(
        super::super::headless_oauth::shell_in(&root),
        Some(placed.clone()),
        "the finder reads it"
    );
    assert_eq!(names(&root), [archive.dir.as_str()], "no scratch is left");
    assert_eq!(install(&root, &archive, &mut fetch, &go), Ok(placed));
    assert_eq!(fetch.calls, 1, "in place, nothing was downloaded again");
}

/// NEGATIVE CONTROLS: a download off its pin (one byte more, the same size with another byte, cut
/// short, none at all) or an archive with no executable installs nothing: the finder finds
/// nothing, no scratch is left, each says why, and the cache root keeps that why as the
/// archive's last failure (its one entry). CONTROL: the pinned bytes install
/// (`a_download_held_to_its_pin_is_placed_where_the_finder_reads`).
#[test]
fn a_download_off_its_pin_installs_nothing() {
    let bytes = shell_zip();
    let archive = archive_of(&bytes);
    let mut longer = bytes.clone();
    longer.push(0);
    let mut flipped = bytes.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    let no_shell = zip(&[(SHELL, 0o100_644, b"not executable\n")]);
    let cases: Vec<(&str, StandIn, Archive, &str)> = vec![
        (
            "one byte more",
            StandIn::serving(longer),
            archive.clone(),
            "and the pin is",
        ),
        (
            "another byte",
            StandIn::serving(flipped),
            archive.clone(),
            "hashes to",
        ),
        (
            "cut short",
            StandIn {
                cut: Some(bytes.len() / 2),
                ..StandIn::serving(bytes.clone())
            },
            archive.clone(),
            "the download failed: the connection dropped",
        ),
        (
            "no network",
            StandIn {
                fail: Some("the host could not be resolved (no network?)"),
                ..StandIn::serving(bytes.clone())
            },
            archive.clone(),
            "the download failed: the host could not be resolved",
        ),
        (
            "no executable",
            StandIn::serving(no_shell.clone()),
            archive_of(&no_shell),
            "holds no executable",
        ),
    ];
    for (what, mut fetch, archive, says) in cases {
        let tmp = aterm_tempfile::tempdir().expect("tmp");
        let root = tmp.path().join("browser.noindex");
        let why = install(&root, &archive, &mut fetch, &go).expect_err(what);
        assert!(
            why.contains(says) && why.ends_with("nothing was installed"),
            "{what}: {why}"
        );
        assert_eq!(fetch.calls, 1, "{what}");
        assert_eq!(
            super::super::headless_oauth::shell_in(&root),
            None,
            "{what}: the finder finds nothing"
        );
        assert_eq!(
            names(&root),
            [format!("{FAILED_PREFIX}{}", archive.dir)],
            "{what}: no scratch, only the failure's record"
        );
        let kept = last_failure(&root, &archive).expect("kept");
        assert!(kept.contains(says), "{what}: {kept}");
    }
}

/// A stop installs nothing: asked while the archive downloads, the pinned bytes are not placed
/// and no scratch is left. CONTROL: the same bytes unstopped install
/// (`a_download_held_to_its_pin_is_placed_where_the_finder_reads`).
#[test]
fn a_stopped_download_installs_nothing() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("browser.noindex");
    let bytes = shell_zip();
    let archive = archive_of(&bytes);
    let mut fetch = StandIn::serving(bytes);
    let why = install(&root, &archive, &mut fetch, &|| true).expect_err("stopped");
    assert!(
        why.contains(STOPPED) && why.ends_with("nothing was installed"),
        "{why}"
    );
    assert_eq!(super::super::headless_oauth::shell_in(&root), None);
    assert!(names(&root).is_empty(), "{:?}", names(&root));
    assert_eq!(last_failure(&root, &archive), None, "a stop is no failure");
}

/// A stop reaches a running download: the child (here `/bin/sleep 120`, standing in for curl)
/// is ended with its process group as soon as the stop is seen, and its run says so; its exit
/// status is read where it ends by itself. The one-minute bound is a hang detector, never a
/// latency budget. CONTROL: unstopped, `/usr/bin/true` exits 0 and `/usr/bin/false` 1.
#[cfg(unix)]
#[test]
fn a_stop_ends_a_running_download() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let looks = AtomicUsize::new(0);
    // Stopped once the child has been looked at running, so the kill is what ends it.
    let stopped = || looks.fetch_add(1, Ordering::SeqCst) >= 3;
    let started = Instant::now();
    let mut sleep = aterm_uds::spawnfd::command("/bin/sleep");
    sleep.arg("120");
    let ran = run_stoppable(sleep, Duration::from_secs(600), &stopped);
    assert_eq!(ran, Err(STOPPED.to_owned()));
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "the stop did not end the child"
    );
    for (program, code) in [("/usr/bin/true", 0), ("/usr/bin/false", 1)] {
        let ran = run_stoppable(
            aterm_uds::spawnfd::command(program),
            Duration::from_secs(60),
            &go,
        );
        assert_eq!(ran, Ok(Some(code)), "CONTROL: {program}");
    }
}

/// The harness's archive is the browser lane's pin, one source: its URL, sha256 and size are
/// `aterm_uds::host::chrome`'s, and it unpacks to a directory and executable the finder reads,
/// beside the lane's own directory and never in its place.
#[test]
fn the_harness_s_archive_is_the_shared_pin() {
    use aterm_uds::host::chrome;
    for (os, arch) in [
        ("macos", "aarch64"),
        ("macos", "x86_64"),
        ("linux", "x86_64"),
        ("linux", "aarch64"),
    ] {
        let pin = chrome::pin_for(os, arch).expect("pinned");
        let archive = Archive::pinned(os, arch).expect("an archive");
        assert_eq!(archive.url, chrome::archive_url(pin.platform));
        assert_eq!(archive.sha256, pin.sha256);
        assert_eq!(archive.bytes, pin.bytes);
        let lane = chrome::pinned_dir(Path::new(""), pin.platform);
        assert_eq!(archive.dir, format!("{}{DIR_SUFFIX}", lane.display()));
        assert!(archive.dir.starts_with("chrome-headless-shell-"));
        // The finder reads this layout.
        let tmp = aterm_tempfile::tempdir().expect("tmp");
        let shell = tmp.path().join(&archive.dir).join(&archive.binary);
        std::fs::create_dir_all(shell.parent().unwrap()).unwrap();
        std::fs::write(&shell, "").unwrap();
        assert_eq!(
            super::super::headless_oauth::shell_in(tmp.path()),
            Some(shell),
            "{os}-{arch}"
        );
    }
    assert_eq!(Archive::pinned("windows", "x86_64"), None);
}

/// The executable inside the test's full build, as the mac-arm64 pin lays it.
const WINDOW_APP: &str =
    "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing";

/// A full build's tree: the app's executable, a framework link within the tree, and a helper.
fn window_zip() -> Vec<u8> {
    zip(&[
        (WINDOW_APP, 0o100_755, b"not a real browser\n"),
        (
            "chrome-mac-arm64/Google Chrome for Testing.app/Contents/Info.plist",
            0o100_644,
            b"<plist/>",
        ),
        (
            "chrome-mac-arm64/Google Chrome for Testing.app/Contents/Frameworks/F.framework/\
             Versions/Current",
            0o120_777,
            b"1.0",
        ),
    ])
}

/// The full build a pin names for `bytes`, placed as the harness places the window pin.
fn window_archive_of(bytes: &[u8]) -> Archive {
    Archive {
        url: "https://downloads.example.invalid/chrome-mac-arm64.zip".to_owned(),
        dir: "chrome-0.0.0-mac-arm64-harness".to_owned(),
        binary: PathBuf::from(WINDOW_APP),
        within: WINDOW_FETCH_WITHIN,
        ..archive_of(bytes)
    }
}

/// The window browser's finder over the cache `root` alone: no `$ATERM_BROWSER`, no
/// applications folder, nothing on `PATH`.
fn window_in(root: &Path) -> Option<PathBuf> {
    super::super::browser_window::find_window_browser_in(&|_| None, &[root.to_path_buf()], &[], &[])
}

/// THE WINDOW BUILD HELD TO ITS PIN IS WHAT THE WINDOW FINDER FINDS: before the fetch the route
/// is `Fetchable` (none found, no failure kept); installed, the full build's executable is the
/// window browser the finder returns from the cache root, the route `Found`, and no scratch or
/// record is left. NEGATIVE CONTROL: off its pin (one byte changed) it installs nothing, the
/// finder finds nothing, and the failure is kept, so the route is `Unreachable` naming it; a
/// later install held to the pin succeeds and removes that record.
#[test]
fn a_window_build_held_to_its_pin_is_found_and_one_off_it_is_kept_as_unreachable() {
    use super::super::browser_window::{WindowRoute, window_route_in};
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("browser.noindex");
    let bytes = window_zip();
    let archive = window_archive_of(&bytes);
    assert_eq!(
        window_route_in(None, window_in(&root), Some(&archive), Some(&root), true),
        WindowRoute::Fetchable
    );

    let mut flipped = bytes.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    let mut off = StandIn::serving(flipped);
    let why = install(&root, &archive, &mut off, &go).expect_err("off its pin");
    assert!(
        why.contains("hashes to") && why.ends_with("nothing was installed"),
        "{why}"
    );
    assert_eq!(window_in(&root), None, "nothing was installed");
    let route = window_route_in(None, window_in(&root), Some(&archive), Some(&root), true);
    assert!(
        matches!(&route, WindowRoute::Unreachable(kept) if kept.contains("hashes to")),
        "{route:?}"
    );
    assert!(!route.reachable());

    let mut held = StandIn::serving(bytes);
    let placed = install(&root, &archive, &mut held, &go).expect("held to its pin");
    assert_eq!(placed, root.join(&archive.dir).join(WINDOW_APP));
    assert_eq!(
        window_in(&root),
        Some(placed.clone()),
        "the finder finds it"
    );
    assert_eq!(
        window_route_in(None, window_in(&root), Some(&archive), Some(&root), true),
        WindowRoute::Found(placed)
    );
    assert_eq!(last_failure(&root, &archive), None);
    assert_eq!(names(&root), [archive.dir.as_str()], "no scratch or record");
}

/// The window route with nothing to fetch: no pin for the machine, or no cache root, is
/// `Unreachable` and says which; a window browser found is `Found` whatever was kept.
#[test]
fn the_window_route_names_why_none_would_be_fetched() {
    use super::super::browser_window::{WindowRoute, window_route_in};
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let archive = window_archive_of(&window_zip());
    let route = window_route_in(None, None, None, Some(tmp.path()), true);
    assert!(
        matches!(&route, WindowRoute::Unreachable(why) if why.contains("covers no")),
        "{route:?}"
    );
    let route = window_route_in(None, None, Some(&archive), None, true);
    assert!(
        matches!(&route, WindowRoute::Unreachable(why) if why.contains("no cache")),
        "{route:?}"
    );
    std::fs::write(
        tmp.path().join(format!("{FAILED_PREFIX}{}", archive.dir)),
        "the host could not be resolved (no network?)\n",
    )
    .unwrap();
    let found = PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium");
    assert_eq!(
        window_route_in(
            None,
            Some(found.clone()),
            Some(&archive),
            Some(tmp.path()),
            true
        ),
        WindowRoute::Found(found)
    );
    assert!(window_route_in(None, Some(PathBuf::new()), None, None, false).reachable());
}

/// The harness's window archive is the shared full-build pin, one source: its URL, sha256 and
/// size are `aterm_uds::host::chrome`'s window pin's, apart from the headless shell's, and it
/// unpacks to a versioned directory and executable the window browser's finder reads.
#[test]
fn the_harness_s_window_archive_is_the_shared_window_pin() {
    use aterm_uds::host::chrome;
    for (os, arch) in [
        ("macos", "aarch64"),
        ("macos", "x86_64"),
        ("linux", "x86_64"),
        ("linux", "aarch64"),
    ] {
        let pin = chrome::window_pin_for(os, arch).expect("pinned");
        let archive = Archive::window(os, arch).expect("an archive");
        assert_eq!(archive.url, chrome::window_archive_url(pin.platform));
        assert_eq!(archive.sha256, pin.sha256);
        assert_eq!(archive.bytes, pin.bytes);
        assert_eq!(archive.within, WINDOW_FETCH_WITHIN);
        assert_ne!(Archive::pinned(os, arch), Some(archive.clone()));
        assert_eq!(
            archive.dir,
            format!(
                "chrome-{}-{}{DIR_SUFFIX}",
                chrome::PIN_VERSION,
                pin.platform
            )
        );
        let tmp = aterm_tempfile::tempdir().expect("tmp");
        let exe = tmp.path().join(&archive.dir).join(&archive.binary);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        assert_eq!(window_in(tmp.path()), Some(exe), "{os}-{arch}");
        assert_eq!(
            super::super::headless_oauth::shell_in(tmp.path()),
            None,
            "{os}-{arch}: never read as the headless shell"
        );
    }
    assert_eq!(Archive::window("windows", "x86_64"), None);
}

/// THE ROUTE READS THE DESK FIRST AND THE DOWNLOADER BEFORE IT SAYS `Fetchable`: a session with
/// no desk to open a window on (a Linux session with no display, a host that is neither macOS
/// nor Linux) is `Unreachable` with `accounts browser`'s own refusal, even with a window browser
/// found, since `accounts browser` refuses on it before it looks for one; with no
/// `/usr/bin/curl` none would be fetched, so that is `Unreachable` naming it. CONTROL: a desk and
/// curl make the same inputs `Found` and `Fetchable`. The desk is a Linux session's `DISPLAY` or
/// `WAYLAND_DISPLAY` (an empty one is none); macOS needs neither.
#[test]
fn the_window_route_reads_the_desk_and_the_downloader() {
    use super::super::browser_window::{WindowRoute, no_window_desk_in, window_route_in};
    use std::ffi::OsString;
    let none = |_: &str| None;
    let display = |name: &str| (name == "DISPLAY").then(|| OsString::from(":0"));
    let wayland = |name: &str| (name == "WAYLAND_DISPLAY").then(|| OsString::from("wayland-0"));
    let empty = |name: &str| (name == "DISPLAY").then(OsString::new);
    let headless_linux = no_window_desk_in(true, true, &none);
    assert!(
        headless_linux.is_some_and(|why| why.contains("no display")),
        "{headless_linux:?}"
    );
    assert_eq!(no_window_desk_in(true, true, &empty), headless_linux);
    assert_eq!(no_window_desk_in(true, true, &display), None);
    assert_eq!(no_window_desk_in(true, true, &wayland), None);
    assert_eq!(no_window_desk_in(true, false, &none), None, "macOS");
    assert!(
        no_window_desk_in(false, false, &none).is_some_and(|why| why.contains("macOS and Linux")),
    );

    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let archive = window_archive_of(&window_zip());
    let found = PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium");
    for (desk, curl) in [(headless_linux, true), (headless_linux, false)] {
        let why = desk.expect("no desk");
        assert_eq!(
            window_route_in(
                desk,
                Some(found.clone()),
                Some(&archive),
                Some(tmp.path()),
                curl
            ),
            WindowRoute::Unreachable(why.to_owned()),
            "one found is not opened where no window opens"
        );
        assert_eq!(
            window_route_in(desk, None, Some(&archive), Some(tmp.path()), curl),
            WindowRoute::Unreachable(why.to_owned())
        );
    }
    let route = window_route_in(None, None, Some(&archive), Some(tmp.path()), false);
    assert!(
        matches!(&route, WindowRoute::Unreachable(why) if why.contains(CURL)),
        "{route:?}"
    );
    assert_eq!(
        window_route_in(
            None,
            Some(found.clone()),
            Some(&archive),
            Some(tmp.path()),
            false
        ),
        WindowRoute::Found(found),
        "CONTROL: one installed needs no downloader"
    );
    assert_eq!(
        window_route_in(None, None, Some(&archive), Some(tmp.path()), true),
        WindowRoute::Fetchable,
        "CONTROL"
    );
    if cfg!(target_os = "macos") {
        assert!(curl_present(), "macOS ships /usr/bin/curl");
    }
}

/// A FAILURE IS READ FOR AN HOUR: a failed fetch's record keeps the route `Unreachable` while it
/// is fresh, and one written longer than `FAILURE_KEPT` ago is not read, so the route is
/// `Fetchable` again and `accounts browser` leads the need once more; a later failure writes it
/// anew. NEGATIVE CONTROL: the same record, fresh, is read.
#[test]
fn a_kept_failure_is_read_for_an_hour() {
    use super::super::browser_window::{WindowRoute, window_route_in};
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("browser.noindex");
    let bytes = window_zip();
    let archive = window_archive_of(&bytes);
    let mut failing = StandIn {
        fail: Some("the host could not be resolved (no network?)"),
        ..StandIn::serving(bytes)
    };
    install(&root, &archive, &mut failing, &go).expect_err("no network");
    let now = SystemTime::now();
    assert!(
        last_failure_at(&root, &archive, now).is_some(),
        "fresh: read"
    );
    assert!(
        last_failure_at(
            &root,
            &archive,
            now + FAILURE_KEPT - Duration::from_secs(60)
        )
        .is_some(),
        "within the hour: read"
    );
    assert_eq!(
        last_failure_at(
            &root,
            &archive,
            now + FAILURE_KEPT + Duration::from_secs(60)
        ),
        None,
        "past the hour: not read"
    );
    let record = root.join(format!("{FAILED_PREFIX}{}", archive.dir));
    std::fs::File::options()
        .write(true)
        .open(&record)
        .and_then(|f| f.set_modified(now - FAILURE_KEPT - Duration::from_secs(60)))
        .expect("aged");
    assert_eq!(last_failure(&root, &archive), None);
    assert_eq!(
        window_route_in(None, None, Some(&archive), Some(&root), true),
        WindowRoute::Fetchable,
        "an old failure keeps nothing out"
    );
    install(&root, &archive, &mut failing, &go).expect_err("no network again");
    assert!(
        last_failure(&root, &archive).is_some_and(|why| why.contains("could not be resolved")),
        "a later failure is written anew"
    );
    assert!(!window_route_in(None, None, Some(&archive), Some(&root), true).reachable());
}

/// A TREE ANOTHER FETCH PLACED MEANWHILE IS NEVER MOVED: two `accounts browser` runs fetch before
/// either takes the sign-in lock, so the one that finishes second finds the first's tree in place
/// (a browser may already run from it) and returns it, its own tree going with its scratch.
/// CONTROL: a directory left without the executable (a copy cut short) is set aside and replaced.
#[test]
fn a_tree_placed_meanwhile_is_never_moved() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("browser.noindex");
    let bytes = window_zip();
    let archive = window_archive_of(&bytes);
    let dest = root.join(&archive.dir);
    let marker = dest.join("placed-by-the-other-fetch");
    let mut racing = StandIn::serving(bytes.clone());
    let (other_dest, other_marker) = (dest.clone(), marker.clone());
    let binary = archive.binary.clone();
    racing.during = Some(Box::new(move || {
        let exe = other_dest.join(&binary);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "the other fetch's browser\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(&other_marker, "").unwrap();
    }));
    let placed = install(&root, &archive, &mut racing, &go).expect("in place");
    assert_eq!(placed, dest.join(&archive.binary));
    assert!(marker.exists(), "the other fetch's tree was moved");
    assert_eq!(
        std::fs::read_to_string(&placed).unwrap(),
        "the other fetch's browser\n"
    );
    assert_eq!(names(&root), [archive.dir.as_str()], "no scratch or record");

    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("browser.noindex");
    let dest = root.join(&archive.dir);
    std::fs::create_dir_all(dest.join("chrome-mac-arm64")).unwrap();
    std::fs::write(dest.join("cut-short"), "").unwrap();
    let mut held = StandIn::serving(bytes);
    let placed = install(&root, &archive, &mut held, &go).expect("replaced");
    assert!(is_executable(&placed));
    assert!(
        !dest.join("cut-short").exists(),
        "CONTROL: the broken copy was set aside"
    );
    assert_eq!(names(&root), [archive.dir.as_str()], "no scratch or record");
}

/// `fetch_window_browser` FETCHES THE WINDOW BUILD INTO THE CACHE ROOT THE FINDER READS: its
/// plan for a home is this machine's full Chrome for Testing pin (never the headless shell's)
/// and that home's cache root, and the pin's layout is one the window finder returns. Placed,
/// the build is returned only where it is a browser that opens a window; a stop noted as it was
/// put in place leaves it there and opens nothing. NEGATIVE CONTROLS: a headless shell placed
/// by the same path is refused (`window_capable`), and the stop says so and keeps no failure.
#[test]
fn the_window_fetch_places_the_full_build_where_the_window_finder_reads() {
    use super::super::browser_window::{
        STOPPED_IN_PLACE, fetch_window_browser_into, window_fetch_plan,
    };
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let home = tmp.path().join("home");
    match (window_fetch_plan(Some(&home)), Archive::host_window()) {
        (Ok((archive, root)), Some(pinned)) => {
            assert_eq!(archive, pinned);
            assert_ne!(
                Some(archive.clone()),
                Archive::host(),
                "never the headless shell"
            );
            assert_eq!(Some(root), cache_root(Some(&home)));
            let laid = tmp.path().join("laid");
            let exe = laid.join(&archive.dir).join(&archive.binary);
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, "").unwrap();
            assert_eq!(
                window_in(&laid),
                Some(exe),
                "the window finder reads its layout"
            );
        }
        (Err(why), None) => assert!(why.contains("covers no"), "{why}"),
        (plan, pinned) => panic!("{plan:?} for {pinned:?}"),
    }
    assert!(window_fetch_plan(None).is_err() || std::env::var_os("XDG_CACHE_HOME").is_some());

    let root = tmp.path().join("browser.noindex");
    let bytes = window_zip();
    let archive = window_archive_of(&bytes);
    let placed =
        fetch_window_browser_into(&root, &archive, &mut StandIn::serving(bytes.clone()), &go)
            .expect("placed");
    assert_eq!(window_in(&root), Some(placed));

    let shell_bytes = shell_zip();
    let shell = archive_of(&shell_bytes);
    let shell_root = tmp.path().join("shell.noindex");
    let why =
        fetch_window_browser_into(&shell_root, &shell, &mut StandIn::serving(shell_bytes), &go)
            .expect_err("a headless shell opens no window");
    assert!(why.contains("no browser that opens a window"), "{why}");

    let stop_root = tmp.path().join("stopped.noindex");
    let dest = stop_root.join(&archive.dir);
    let in_place = || dest.exists();
    let why = fetch_window_browser_into(
        &stop_root,
        &archive,
        &mut StandIn::serving(bytes),
        &in_place,
    )
    .expect_err("stopped as it was placed");
    assert_eq!(why, STOPPED_IN_PLACE);
    assert!(window_in(&stop_root).is_some(), "it stays for the next run");
    assert_eq!(
        last_failure(&stop_root, &archive),
        None,
        "a stop is no failure"
    );
}
