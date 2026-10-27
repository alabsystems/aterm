// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tar writer, proven by the client: every archive it writes extracts through
//! `atpkg::extract_tar_zst` under the client's own caps, to a tree whose `tree_root` is
//! the stage's, with hard links kept as one file; and the signed `disk_installed` covers
//! a small-file tree whose structure outweighs its content.

use std::path::Path;

use atpkg_publish::artifact::tarwrite::{self, RawFrames};

use super::fixture::{put, scratch, set_mode};

fn client_cap(disk_installed: u64) -> u64 {
    disk_installed.saturating_mul(2).max(1 << 20)
}
const MAX_ENTRIES: u64 = 4_000_000;

fn extract(asset: &Path, dest: &Path, disk_installed: u64) {
    std::fs::create_dir_all(dest).expect("mkdir dest");
    set_mode(dest, 0o700);
    atpkg::extract_tar_zst(asset, dest, client_cap(disk_installed), MAX_ENTRIES)
        .unwrap_or_else(|e| panic!("the client refuses what was signed: {e:?}"));
}

#[cfg(unix)]
fn ino(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(p).expect("stat").ino()
}

fn stage(root: &Path) {
    put(&root.join("bin/trustc"), b"\x7fELF compiler bytes");
    set_mode(&root.join("bin/trustc"), 0o700);
    put(&root.join("share/doc/README"), b"read me\n");
    set_mode(&root.join("share/doc/README"), 0o600);
    put(
        &root.join("lib/librustc_driver-0123.so"),
        &vec![7u8; 70_000],
    );
    std::fs::create_dir_all(root.join("lib/rustlib/aarch64-unknown-linux-gnu/lib")).expect("mkdir");
    std::fs::hard_link(
        root.join("lib/librustc_driver-0123.so"),
        root.join("lib/rustlib/aarch64-unknown-linux-gnu/lib/librustc_driver-0123.so"),
    )
    .expect("link");
    std::fs::hard_link(root.join("bin/trustc"), root.join("bin/rustc")).expect("link");
    let deep = format!("lib/rustlib/src/rust/{}/{}", "d".repeat(60), "f".repeat(70));
    put(&root.join(&deep), b"deep\n");
    let long = format!("lib/rustlib/{}", "n".repeat(120));
    put(&root.join(long), b"long\n");
    std::fs::create_dir_all(root.join("libexec")).expect("mkdir empty");
}

#[test]
#[cfg(unix)]
fn what_it_writes_the_client_extracts_to_the_same_tree_root_with_links_kept() {
    let tmp = scratch("tar-roundtrip");
    let root = tmp.path().join("stage");
    stage(&root);
    let asset = tmp.path().join("x.tar.zst");
    let packed = tarwrite::write_with(&root, &asset, &RawFrames::WHOLE).expect("pack");
    assert_eq!(
        packed.tree_root,
        atpkg::tree_root(&root).expect("stage root")
    );
    let out = tmp.path().join("out");
    extract(&asset, &out, packed.disk_installed);
    assert_eq!(
        atpkg::tree_root(&out).expect("extracted root"),
        packed.tree_root,
        "the signed root is the one every client recomputes"
    );
    assert_eq!(ino(&out.join("bin/rustc")), ino(&out.join("bin/trustc")));
    assert_eq!(
        ino(&out.join("lib/librustc_driver-0123.so")),
        ino(&out.join("lib/rustlib/aarch64-unknown-linux-gnu/lib/librustc_driver-0123.so")),
        "a hard-link group ships once"
    );
    assert!(
        packed.stream_bytes < 2 * 70_000,
        "the 70 kB driver is stored once, not per name ({} B stream)",
        packed.stream_bytes
    );
    assert!(out.join("libexec").is_dir(), "an empty directory survives");
    assert_eq!(packed.sha256, atpkg::sha256_file(&asset).expect("hash"));
    assert_eq!(packed.size, std::fs::metadata(&asset).expect("stat").len());
    let framed = tmp.path().join("framed.tar.zst");
    let many = tarwrite::write_with(&root, &framed, &RawFrames { frame_max: 4096 })
        .expect("pack in 4 KiB frames");
    assert!(many.stream_bytes > 8 * 4096, "{} B", many.stream_bytes);
    assert_eq!(many.tree_root, packed.tree_root);
    extract(&framed, &tmp.path().join("framed-out"), many.disk_installed);
    assert_eq!(
        atpkg::tree_root(&tmp.path().join("framed-out")).expect("extracted root"),
        packed.tree_root
    );
}

#[test]
#[cfg(unix)]
fn the_archive_is_deterministic_whatever_the_stage_path_or_its_modes() {
    let tmp = scratch("tar-determinism");
    let (a, b) = (tmp.path().join("one/stage"), tmp.path().join("two/x"));
    stage(&a);
    stage(&b);
    set_mode(&b.join("bin/trustc"), 0o755);
    let pa = tarwrite::write(&a, &tmp.path().join("a.tar.zst")).expect("pack a");
    let pb = tarwrite::write(&b, &tmp.path().join("b.tar.zst")).expect("pack b");
    assert_eq!(pa.sha256, pb.sha256);
    assert_eq!(pa.tree_root, pb.tree_root);
}

#[test]
#[cfg(unix)]
fn a_symlink_or_an_empty_tree_is_refused() {
    let tmp = scratch("tar-refusals");
    let root = tmp.path().join("stage");
    put(&root.join("bin/trustc"), b"x");
    std::os::unix::fs::symlink("trustc", root.join("bin/rustc")).expect("symlink");
    let err = tarwrite::write(&root, &tmp.path().join("l.tar.zst")).expect_err("a link");
    assert!(err.to_string().contains("symlink"), "{err}");
    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(empty.join("bin")).expect("mkdir");
    let err = tarwrite::write(&empty, &tmp.path().join("e.tar.zst")).expect_err("empty");
    assert!(err.to_string().contains("no file"), "{err}");
}

#[test]
fn a_small_file_tree_is_signed_by_its_stream_and_an_ordinary_one_by_its_content() {
    let tmp = scratch("tar-size");
    let small = tmp.path().join("small");
    let dir = small
        .join("share")
        .join("d".repeat(200))
        .join("e".repeat(200));
    std::fs::create_dir_all(&dir).expect("mkdir");
    for i in 0..500 {
        let name = format!("{i:03}{}", "f".repeat(150));
        std::fs::write(dir.join(name), [0u8; 100]).expect("write fixture");
    }
    let packed = tarwrite::write_with(&small, &tmp.path().join("small.tar.zst"), &RawFrames::WHOLE)
        .expect("pack");
    assert_eq!(packed.content_bytes, 50_000);
    assert!(
        packed.stream_bytes - packed.content_bytes > 1 << 20,
        "the fixture's structure no longer outweighs the client's 1 MiB floor ({} B)",
        packed.stream_bytes
    );
    assert!(2 * packed.disk_installed >= packed.stream_bytes);
    extract(
        &packed.asset,
        &tmp.path().join("small-out"),
        packed.disk_installed,
    );

    let big = tmp.path().join("big");
    put(&big.join("share/blob"), &vec![0u8; 400_000]);
    put(&big.join("share/note"), b"x");
    let packed = tarwrite::write_with(&big, &tmp.path().join("big.tar.zst"), &RawFrames::WHOLE)
        .expect("pack");
    assert_eq!(packed.disk_installed, 400_001);
}

/// A seeded xorshift.
fn xorshift() -> impl FnMut() -> u64 {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// At least `len` bytes of eight words, spaces and line breaks, drawn from `next`.
fn words(len: usize, next: &mut impl FnMut() -> u64) -> Vec<u8> {
    let words = [
        "pack", "trust", "seal", "stage", "frame", "block", "tree", "root",
    ];
    let mut text = Vec::new();
    while text.len() < len {
        text.extend_from_slice(words[(next() % 8) as usize].as_bytes());
        text.push(if next().is_multiple_of(9) {
            b'\n'
        } else {
            b' '
        });
    }
    text
}

/// A stage the encoder has work in: the fixture tree, plus a megabyte of words in which a
/// 200 kB stretch of noise comes back 700 kB on, five blocks later.
fn compressible(root: &Path) {
    stage(root);
    let mut next = xorshift();
    let mut text = words(1 << 20, &mut next);
    let noise: Vec<u8> = (0..200_000).map(|_| (next() >> 24) as u8).collect();
    text[100_000..300_000].copy_from_slice(&noise);
    text[800_000..1_000_000].copy_from_slice(&noise);
    put(&root.join("share/words"), &text);
}

#[test]
#[cfg(unix)]
fn the_hosts_compressor_is_aterm_zstds_encoder_and_packs_what_raw_frames_pack() {
    use atpkg_publish::artifact::tools::{HostTools, Tools as _};

    let tmp = scratch("tar-encoder");
    let root = tmp.path().join("stage");
    compressible(&root);
    let raw = tarwrite::write_with(&root, &tmp.path().join("raw.tar.zst"), &RawFrames::WHOLE)
        .expect("the raw-block pack");
    let host = tarwrite::write_with(
        &root,
        &tmp.path().join("host.tar.zst"),
        HostTools.compressor(),
    )
    .expect("the host's compressor packs");
    let default =
        tarwrite::write(&root, &tmp.path().join("default.tar.zst")).expect("the default pack");
    assert_eq!(host.tree_root, raw.tree_root);
    assert_eq!(host.stream_bytes, raw.stream_bytes, "the same tar stream");
    assert_eq!(host.sha256, default.sha256, "the same bytes twice");
    assert!(
        host.size * 4 < host.stream_bytes,
        "compressed: {} B of a {} B stream",
        host.size,
        host.stream_bytes
    );
    assert!(
        raw.size >= raw.stream_bytes,
        "raw frames are not compressed: {} B of a {} B stream",
        raw.size,
        raw.stream_bytes
    );
    let out = tmp.path().join("out");
    extract(&host.asset, &out, host.disk_installed);
    assert_eq!(
        atpkg::tree_root(&out).expect("extracted root"),
        raw.tree_root
    );
}

/// Where the child of [`a_stage_packs_to_the_same_bytes_in_another_process`] reads its
/// stage, writes its pack, and (the control's child) the level its encoder runs at.
const CHILD_STAGE: &str = "ATPKG_PUBLISH_TEST_CHILD_STAGE";
const CHILD_ASSET: &str = "ATPKG_PUBLISH_TEST_CHILD_ASSET";
const CHILD_LEVEL: &str = "ATPKG_PUBLISH_TEST_CHILD_LEVEL";

/// aterm-zstd's encoder at `level`, written as [`tarwrite::Zstd`] writes: the controls'
/// compressor. `fitted` takes the window [`tarwrite::options`] fits to the stream; unfitted
/// it takes the options of a stream past 2^27 bytes, which every pack had before the window
/// was fitted (a 2^27 window and no declared size).
struct Encoded {
    level: u8,
    fitted: bool,
}

impl Encoded {
    /// Every pack's compressor before the window was fitted.
    const UNFITTED: Encoded = Encoded {
        level: tarwrite::LEVEL,
        fitted: false,
    };
}

impl tarwrite::Compressor for Encoded {
    fn compress(
        &self,
        asset: &std::fs::File,
        len: u64,
        tar: &mut dyn FnMut(&mut dyn std::io::Write) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        struct Unflushed<W: std::io::Write>(aterm_zstd::encode::Encoder<W>);
        impl<W: std::io::Write> std::io::Write for Unflushed<W> {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.0.write(data)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let options = tarwrite::options(if self.fitted { len } else { u64::MAX }).level(self.level);
        let mut encoder = Unflushed(aterm_zstd::encode::Encoder::new(
            std::io::BufWriter::with_capacity(1 << 20, asset),
            options,
        ));
        tar(&mut encoder)?;
        encoder.0.finish().map(drop)
    }
}

/// What a frame's header declares (RFC 8878 §3.1.1.1).
#[derive(Debug, PartialEq, Eq)]
struct Declared {
    /// Window_Size: the Window_Descriptor's, or a single segment's Frame_Content_Size.
    window: u64,
    /// Single_Segment_Flag.
    single: bool,
    /// Frame_Content_Size, when the header carries one.
    content_size: Option<u64>,
    /// Content_Checksum_Flag.
    checksum: bool,
}

/// The header of the frame `zst` starts with, read by the RFC's layout (§3.1.1.1).
fn declared(zst: &[u8]) -> Declared {
    assert_eq!(&zst[..4], &0xFD2F_B528u32.to_le_bytes(), "a zstd frame");
    let descriptor = zst[4];
    assert_eq!(descriptor & 0x03, 0, "no dictionary id");
    let single = descriptor & 0x20 != 0;
    let (window_descriptor, at) = if single { (None, 5) } else { (Some(zst[5]), 6) };
    let field = match descriptor >> 6 {
        0 => usize::from(single),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let mut bytes = [0u8; 8];
    bytes[..field].copy_from_slice(&zst[at..at + field]);
    let value = u64::from_le_bytes(bytes);
    let content_size = (field > 0).then_some(if field == 2 { value + 256 } else { value });
    let window = match (window_descriptor, content_size) {
        (Some(w), _) => {
            let base = 1u64 << (10 + (w >> 3));
            base + base / 8 * u64::from(w & 7)
        }
        (None, size) => size.expect("a single segment declares its size"),
    };
    Declared {
        window,
        single,
        content_size,
        checksum: descriptor & 0x04 != 0,
    }
}

/// A pack declares the smallest window that holds its tar stream: within 2^27 bytes its
/// frame (one, with its content checksum) is a single segment whose window is the stream,
/// so a decoder needs no window past the pack's own length. The control: the frame every
/// pack had before, of the same stage, declares a 2^27 window, more than a hundred times
/// the stream, and is a pack the client extracts all the same.
#[test]
#[cfg(unix)]
fn a_pack_declares_no_window_past_its_tar_stream() {
    let tmp = scratch("tar-window");
    let root = tmp.path().join("stage");
    compressible(&root);
    let packed = tarwrite::write(&root, &tmp.path().join("fit.tar.zst")).expect("pack");
    let fit = declared(&std::fs::read(&packed.asset).expect("read the pack"));
    assert_eq!(
        fit,
        Declared {
            window: packed.stream_bytes,
            single: true,
            content_size: Some(packed.stream_bytes),
            checksum: true,
        }
    );
    let before = tarwrite::write_with(
        &root,
        &tmp.path().join("before.tar.zst"),
        &Encoded::UNFITTED,
    )
    .expect("the unfitted pack");
    assert_ne!(before.sha256, packed.sha256, "another frame");
    let unfitted = declared(&std::fs::read(&before.asset).expect("read the unfitted pack"));
    assert_eq!(
        unfitted,
        Declared {
            window: 1 << tarwrite::WINDOW_LOG_MAX,
            single: false,
            content_size: None,
            checksum: true,
        }
    );
    assert!(unfitted.window > 100 * before.stream_bytes, "{unfitted:?}");
    extract(
        &before.asset,
        &tmp.path().join("out"),
        before.disk_installed,
    );
}

/// Past 2^27 bytes (the trust pack's tar is 3.1 GB) a pack is compressed with the options
/// every pack had before the window was fitted, so its frame is the bytes it was: the
/// encoder's output is a function of its options and the stream. The control: a stream of
/// exactly 2^27 bytes, which fits, declares its size and gets other options.
#[test]
fn past_the_widest_window_a_pack_is_compressed_as_before() {
    use aterm_zstd::encode::Options;
    use aterm_zstd::frame::{Checksum, WINDOW_MAX};

    let before = Options::new()
        .level(19)
        .window_log(27)
        .checksum(Checksum::Xxh64);
    for len in [WINDOW_MAX as u64 + 1, 3_102_120_448, u64::MAX] {
        assert_eq!(tarwrite::options(len), before, "{len}");
    }
    assert_ne!(
        tarwrite::options(WINDOW_MAX as u64),
        before,
        "the control: a stream that fits"
    );
    assert_eq!(
        tarwrite::options(WINDOW_MAX as u64),
        before.content_size(WINDOW_MAX as u64),
        "2^27 bytes is a single segment under the same window"
    );
}

/// Within 2^27 bytes a pack declares its length and the encoder works in the smallest power
/// of two that holds it, never under the format's 2^10. The control: at a power of two the
/// next byte takes the next window, so a fixed window, or one a power too wide or too
/// narrow, fails a row.
#[test]
fn within_the_widest_window_a_pack_fits_the_smallest_power_of_two() {
    use aterm_zstd::encode::Options;
    use aterm_zstd::frame::{Checksum, WINDOW_MAX};

    let fitted = |log: u8, len: u64| {
        Options::new()
            .level(19)
            .window_log(log)
            .checksum(Checksum::Xxh64)
            .content_size(len)
    };
    for (len, log) in [
        (0, 10),
        (1, 10),
        (1 << 10, 10),
        ((1 << 10) + 1, 11),
        (1 << 20, 20),
        ((1 << 20) + 1, 21),
        (1_133_056, 21),
        (8 << 20, 23),
        ((8 << 20) + 1, 24),
        (WINDOW_MAX as u64 - 1, 27),
    ] {
        assert_eq!(tarwrite::options(len), fitted(log, len), "{len}");
    }
    assert_ne!(
        tarwrite::options(1 << 20),
        tarwrite::options((1 << 20) + 1).content_size(1 << 20),
        "the control: one byte past 2^20 takes the next window"
    );
}

/// A signed pack is the stage's alone: two other processes (other pids, other address
/// layouts, other hash seeds) pack it to the bytes this one does. The control: a child
/// whose encoder runs one level lower, a process-dependent setting such as a pid-keyed
/// one would be, packs other bytes, and the comparison sees them; at [`tarwrite::LEVEL`]
/// the control's compressor packs this process's bytes, so the level is all it changes.
#[test]
#[cfg(unix)]
fn a_stage_packs_to_the_same_bytes_in_another_process() {
    if let (Some(stage), Some(asset)) =
        (std::env::var_os(CHILD_STAGE), std::env::var_os(CHILD_ASSET))
    {
        let (stage, asset) = (Path::new(&stage), Path::new(&asset));
        match std::env::var(CHILD_LEVEL).ok().and_then(|l| l.parse().ok()) {
            Some(level) => tarwrite::write_with(
                stage,
                asset,
                &Encoded {
                    level,
                    fitted: true,
                },
            ),
            None => tarwrite::write(stage, asset),
        }
        .expect("the child packs");
        return;
    }
    let tmp = scratch("tar-processes");
    let root = tmp.path().join("stage");
    stage(&root);
    put(
        &root.join("share/words"),
        &words(256 << 10, &mut xorshift()),
    );
    let here = tarwrite::write(&root, &tmp.path().join("here.tar.zst")).expect("pack here");
    let same = tarwrite::write_with(
        &root,
        &tmp.path().join("same.tar.zst"),
        &Encoded {
            level: tarwrite::LEVEL,
            fitted: true,
        },
    )
    .expect("the control's compressor at the producer's level");
    assert_eq!(
        same.sha256, here.sha256,
        "the control differs only in its level"
    );
    let this = module_path!()
        .split_once("::")
        .map_or(String::new(), |(_, m)| format!("{m}::"))
        + "a_stage_packs_to_the_same_bytes_in_another_process";
    let exe = std::env::current_exe().expect("this test binary's path");
    let child = |name: &str, level: Option<u8>| {
        let asset = tmp.path().join(name);
        let mut cmd = aterm_uds::spawnfd::command(&exe);
        cmd.args(["--exact", &this, "--test-threads=1"])
            .env(CHILD_STAGE, &root)
            .env(CHILD_ASSET, &asset)
            .env_remove(CHILD_LEVEL);
        if let Some(level) = level {
            cmd.env(CHILD_LEVEL, level.to_string());
        }
        // A hang detector, not a budget: the child packs a few hundred kB.
        let hang = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let out = aterm_uds::deadline::output_until(&mut cmd, hang).expect("the child runs");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{name}: the child did not pack\nstdout: {stdout}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        atpkg::sha256_file(&asset).expect("the child's pack")
    };
    assert_eq!(child("one.tar.zst", None), here.sha256, "another process");
    assert_eq!(child("two.tar.zst", None), here.sha256, "a third process");
    assert_ne!(
        child("lower.tar.zst", Some(tarwrite::LEVEL - 1)),
        here.sha256,
        "the control: a child one level lower packs other bytes"
    );
}

/// The oracle: libzstd's CLI tests and decodes what the producer writes, to the stream the
/// client's own decoder reads, under an 8 MB memory limit (RFC 8878 §3.1.1.1.2's
/// recommended decoder bound), since the pack's tar is within 8 MiB. The control: the
/// frame every pack had before, of the same stage, passes `zstd -t` without a limit and is
/// refused under that one for its 2^27 window. COULD NOT RUN without the CLI
/// ([`aterm_uds::host::TOOLS`]' zstd row), never a pass.
#[test]
#[cfg(unix)]
fn a_pack_decodes_with_the_zstd_cli_to_what_the_client_reads() {
    use std::io::Read as _;

    let zstd = aterm_uds::host::need_tool("zstd");
    let tmp = scratch("tar-oracle");
    let root = tmp.path().join("stage");
    compressible(&root);
    let packed = tarwrite::write(&root, &tmp.path().join("x.tar.zst")).expect("pack");
    assert!(packed.stream_bytes < 8 << 20, "{} B", packed.stream_bytes);
    let mut ours = Vec::new();
    aterm_zstd::Decoder::new(std::fs::File::open(&packed.asset).expect("open"))
        .require_checksum()
        .read_to_end(&mut ours)
        .expect("the client's decoder");
    assert_eq!(ours.len() as u64, packed.stream_bytes);
    let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(60);
    let run = |args: &[&str], frame: &Path| {
        let mut cmd = aterm_uds::spawnfd::command(&zstd);
        cmd.args(args).arg(frame);
        let out = aterm_uds::deadline::output_until(&mut cmd, deadline()).expect("zstd runs");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (tested, why) = run(&["-t", "-q", "--memory=8MB"], &packed.asset);
    assert!(tested, "zstd -t --memory=8MB: {why}");
    // Into a file: a run's captured output keeps only its last MiB.
    let tar = tmp.path().join("x.tar");
    let to = tar.to_str().expect("a UTF-8 scratch path");
    let (decoded, why) = run(&["-d", "-q", "--memory=8MB", "-o", to], &packed.asset);
    assert!(decoded, "zstd -d --memory=8MB: {why}");
    assert!(
        std::fs::read(&tar).expect("the CLI's tar") == ours,
        "the CLI decodes the stream the client reads"
    );

    let before = tarwrite::write_with(
        &root,
        &tmp.path().join("before.tar.zst"),
        &Encoded::UNFITTED,
    )
    .expect("the unfitted pack");
    let (tested, why) = run(&["-t", "-q"], &before.asset);
    assert!(tested, "the control decodes without a limit: {why}");
    let (tested, why) = run(&["-t", "-q", "--memory=8MB"], &before.asset);
    assert!(
        !tested && why.contains("too much memory"),
        "the control: a 2^27 window is refused under 8 MB: {why}"
    );
}

/// A compressor that drains the tar stream, then writes `bytes` as the asset (or fails).
struct StandIn(Option<Vec<u8>>);

impl tarwrite::Compressor for StandIn {
    fn compress(
        &self,
        asset: &std::fs::File,
        _len: u64,
        tar: &mut dyn FnMut(&mut dyn std::io::Write) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        use std::io::Write as _;
        tar(&mut std::io::sink())?;
        match &self.0 {
            Some(bytes) => (&*asset).write_all(bytes),
            None => Err(std::io::Error::other("the stand-in compressor failed")),
        }
    }
}

#[test]
fn a_compressor_that_fails_or_writes_another_stream_fails_the_pack() {
    use aterm_zstd::frame::{self, Checksum};

    let tmp = scratch("tar-compressor");
    let root = tmp.path().join("stage");
    put(&root.join("bin/tool"), b"tool bytes\n");
    let right = tmp.path().join("right.zst");
    tarwrite::write_with(&root, &right, &RawFrames::WHOLE).expect("the archive's frames");
    let frame_of = |checksum| {
        let mut zst = Vec::new();
        frame::write_raw(&mut zst, b"hello", checksum).expect("frame");
        zst
    };
    for (compressor, want) in [
        (StandIn(None), "the stand-in compressor failed"),
        (
            StandIn(Some(frame_of(Checksum::Xxh64))),
            "decodes to 5 bytes",
        ),
        (
            StandIn(Some(frame_of(Checksum::Omitted))),
            "no content checksum",
        ),
    ] {
        let asset = tmp.path().join("x.tar.zst");
        let err =
            tarwrite::write_with(&root, &asset, &compressor).expect_err("a stand-in's stream");
        assert!(err.to_string().contains(want), "{want}: {err}");
    }
    let asset = tmp.path().join("control.tar.zst");
    let control = StandIn(Some(std::fs::read(&right).expect("read the frames")));
    let packed = tarwrite::write_with(&root, &asset, &control).expect("the archive's own frames");
    extract(&asset, &tmp.path().join("out"), packed.disk_installed);
}

/// A compressor that sets `file`'s length to `to` once the stream is sized, then compresses
/// as the producer does.
struct Resizes {
    file: std::path::PathBuf,
    to: Option<u64>,
}

impl tarwrite::Compressor for Resizes {
    fn compress(
        &self,
        asset: &std::fs::File,
        len: u64,
        tar: &mut dyn FnMut(&mut dyn std::io::Write) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        if let Some(to) = self.to {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&self.file)?
                .set_len(to)?;
        }
        tarwrite::Compressor::compress(&tarwrite::Zstd, asset, len, tar)
    }
}

/// The stream is sized from the plan before it is written, so a file that shrinks or grows
/// after its plan read its size is refused by name, before the stream runs past the length
/// the frame declared (the file grows by more than the rest of the stream, so a copy of the
/// whole file would meet the encoder's size refusal first). The control: the same
/// compressor, resizing nothing, packs.
#[test]
fn a_file_that_changes_size_after_its_plan_is_refused() {
    let tmp = scratch("tar-resized");
    let root = tmp.path().join("stage");
    let tool = root.join("bin/tool");
    put(&root.join("share/note"), b"note\n");
    for to in [4_000, 100_000] {
        put(&tool, &[7u8; 5_000]);
        let resizes = Resizes {
            file: tool.clone(),
            to: Some(to),
        };
        let err = tarwrite::write_with(&root, &tmp.path().join("x.tar.zst"), &resizes)
            .expect_err("a file resized mid-pack");
        let want = format!("changed size while it was archived (5000 → {to} bytes)");
        assert!(err.to_string().contains(&want), "{want}: {err}");
    }
    put(&tool, &[7u8; 5_000]);
    let unresized = Resizes {
        file: tool,
        to: None,
    };
    let packed = tarwrite::write_with(&root, &tmp.path().join("ok.tar.zst"), &unresized)
        .expect("the control packs");
    extract(
        &packed.asset,
        &tmp.path().join("out"),
        packed.disk_installed,
    );
}
