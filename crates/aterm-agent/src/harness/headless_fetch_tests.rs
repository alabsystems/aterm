// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's own fetch of its headless browser, against a stand-in downloader and a small
//! archive built here: nothing reaches the network and no browser is run. An archive held to its
//! pin is placed where the finder reads it; one off its pin (size, sha256, cut short, no
//! download, no executable) or a stop installs nothing and leaves no scratch; a stop ends a
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
}

impl StandIn {
    fn serving(bytes: Vec<u8>) -> StandIn {
        StandIn {
            bytes,
            cut: None,
            fail: None,
            calls: 0,
        }
    }
}

impl Fetch for StandIn {
    fn fetch(
        &mut self,
        url: &str,
        dest: &Path,
        bytes: u64,
        stopped: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        self.calls += 1;
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
/// nothing, no scratch is left, and each says why. CONTROL: the pinned bytes install
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
        assert!(names(&root).is_empty(), "{what}: {:?}", names(&root));
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
