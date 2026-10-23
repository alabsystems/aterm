// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The tar writer, proven by the client: every archive it writes extracts through
//! `atpkg::extract_tar_zst` under the client's own caps, to a tree whose `tree_root` is
//! the stage's, with hard links kept as one file; and the signed `disk_installed` covers
//! a small-file tree whose structure outweighs its content.

use std::path::Path;

use atpkg_publish::artifact::tarwrite::{self, RawFrames, ZstdCli};

use super::fixture::{put, put_exe, scratch, set_mode};

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
    let raw = &RawFrames::WHOLE;
    let pa = tarwrite::write_with(&a, &tmp.path().join("a.tar.zst"), raw).expect("pack a");
    let pb = tarwrite::write_with(&b, &tmp.path().join("b.tar.zst"), raw).expect("pack b");
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

#[test]
#[cfg(unix)]
fn the_hosts_compressor_is_the_zstd_cli_and_packs_what_raw_frames_pack() {
    use atpkg_publish::artifact::tools::{HostTools, Tools as _};

    if let Err(why) = tarwrite::compressor_on_path() {
        aterm_uds::host::could_not_run(format!(
            "{why}: {}",
            aterm_uds::host::tool("zstd").remedy()
        ));
    }
    let tmp = scratch("tar-real-cli");
    let root = tmp.path().join("stage");
    stage(&root);
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
    assert_eq!(
        host.sha256, default.sha256,
        "the CLI's bytes are the same twice"
    );
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

#[test]
#[cfg(unix)]
fn a_compressor_that_fails_or_writes_another_stream_fails_the_pack() {
    use aterm_zstd::frame::{self, Checksum};

    let tmp = scratch("tar-compressor");
    let root = tmp.path().join("stage");
    put(&root.join("bin/tool"), b"tool bytes\n");
    let right = tmp.path().join("right.zst");
    tarwrite::write_with(&root, &right, &RawFrames::WHOLE).expect("the archive's frames");
    let frame_of = |name: &str, checksum| {
        let mut zst = Vec::new();
        frame::write_raw(&mut zst, b"hello", checksum).expect("frame");
        let path = tmp.path().join(name);
        put(&path, &zst);
        path
    };
    let other = frame_of("other.zst", Checksum::Xxh64);
    let unchecked = frame_of("unchecked.zst", Checksum::Omitted);
    let stand_in = |name: &str, body: String| {
        put_exe(
            &tmp.path().join(name),
            format!("#!/bin/sh\n{body}").as_bytes(),
        )
    };
    for (compressor, want) in [
        (tmp.path().join("missing-compressor"), "did not start"),
        (
            stand_in("fails", "echo 'zstd: boom' >&2\nexit 3\n".into()),
            "failed (exit status: 3): zstd: boom",
        ),
        (
            stand_in(
                "other",
                format!("cat > /dev/null\ncat '{}'\n", other.display()),
            ),
            "decodes to 5 bytes",
        ),
        (
            stand_in(
                "unchecked",
                format!("cat > /dev/null\ncat '{}'\n", unchecked.display()),
            ),
            "no content checksum",
        ),
    ] {
        let asset = tmp.path().join("x.tar.zst");
        let cli = ZstdCli {
            program: compressor.clone().into(),
        };
        let err = tarwrite::write_with(&root, &asset, &cli).expect_err("a stand-in's stream");
        assert!(
            err.to_string().contains(want),
            "{}: {err}",
            compressor.display()
        );
    }
    let argv = tmp.path().join("argv");
    let control = stand_in(
        "right",
        format!(
            "echo \"$@\" > '{}'\ncat > /dev/null\ncat '{}'\n",
            argv.display(),
            right.display()
        ),
    );
    let asset = tmp.path().join("cli.tar.zst");
    let cli = ZstdCli {
        program: control.into(),
    };
    let packed = tarwrite::write_with(&root, &asset, &cli).expect("the archive's own frames");
    assert_eq!(
        std::fs::read_to_string(&argv).expect("the stand-in ran"),
        "-q -19 --single-thread -c\n"
    );
    extract(&asset, &tmp.path().join("out"), packed.disk_installed);
}

#[test]
#[cfg(unix)]
fn the_compressor_probe_finds_only_an_executable_zstd() {
    use std::ffi::OsStr;

    for path in [None, Some(OsStr::new(""))] {
        let e = tarwrite::compressor_in(path).expect_err("no PATH, no zstd");
        assert!(e.contains("no `zstd` on this builder's PATH"), "{e}");
    }
    let tmp = scratch("tar-probe");
    let dir = tmp.path().join("bin");
    put(&dir.join("zstd"), b"#!/bin/sh\n");
    tarwrite::compressor_in(Some(dir.as_os_str())).expect_err("not executable");
    set_mode(&dir.join("zstd"), 0o755);
    let joined = std::env::join_paths([tmp.path().join("none"), dir.clone()]).expect("join");
    assert_eq!(
        tarwrite::compressor_in(Some(&joined)).expect("an executable zstd"),
        dir.join("zstd")
    );
}

#[test]
#[cfg(unix)]
fn a_compressor_loud_on_stderr_before_it_reads_still_packs() {
    const CHILD: &str = "ATERM_PUBLISH_LOUD_COMPRESSOR_CHILD";
    const TEST: &str =
        "artifact::tarwrite::a_compressor_loud_on_stderr_before_it_reads_still_packs";
    let Some(path) = std::env::var_os(CHILD) else {
        let tmp = scratch("tar-loud");
        let mut command =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        command.args(["--exact", TEST, "--nocapture"]);
        command.env(CHILD, tmp.path());
        let output = aterm_uds::deadline::output_until(
            &mut command,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("the owned pack child hung or could not run");
        assert!(
            output.status.success(),
            "pack child: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(tmp.path().join("packed")).expect("the exact test ran"),
            b"packed\n"
        );
        return;
    };
    let tmp = std::path::PathBuf::from(path);
    let root = tmp.join("stage");
    put(&root.join("share/blob"), &vec![7u8; 1 << 20]);
    let frames = tmp.join("frames.zst");
    tarwrite::write_with(&root, &frames, &RawFrames::WHOLE).expect("the archive's frames");
    let loud = put_exe(
        &tmp.join("loud"),
        format!(
            "#!/bin/sh\nhead -c 1048576 /dev/zero >&2\ncat > /dev/null\ncat '{}'\n",
            frames.display()
        )
        .as_bytes(),
    );
    let asset = tmp.join("loud.tar.zst");
    let cli = ZstdCli {
        program: loud.into(),
    };
    let packed = tarwrite::write_with(&root, &asset, &cli).expect("the pack");
    extract(&asset, &tmp.join("out"), packed.disk_installed);
    std::fs::write(tmp.join("packed"), b"packed\n").expect("case completed");
}
