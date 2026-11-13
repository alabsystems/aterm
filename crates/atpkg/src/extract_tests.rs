// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::fixtures::{ZipMember, gzip_bytes, raw_header_mode, tar_bytes, zip_bytes, zstd_bytes};
use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn root() -> PathBuf {
    PathBuf::from("/store/ay/staging")
}

#[test]
fn vet_entry_admits_in_root_paths_and_refuses_every_escape() {
    use EntryKind::{Directory as D, Hardlink, Other, Regular as F, Symlink};
    let r = root();
    for (raw, kind, at) in [
        ("bin/ay", F, "bin/ay"),
        ("share/doc", D, "share/doc"),
        ("a/./b", F, "a/b"),
        ("lib/foo..bar.so", F, "lib/foo..bar.so"),
    ] {
        assert_eq!(vet_entry(&r, Path::new(raw), kind), Ok(r.join(at)), "{raw}");
    }
    for (raw, kind, why) in [
        ("../etc/passwd", F, ExtractReject::ParentTraversal),
        ("a/../../b", F, ExtractReject::ParentTraversal),
        ("..", D, ExtractReject::ParentTraversal),
        ("/etc/passwd", F, ExtractReject::AbsolutePath),
        ("/", D, ExtractReject::AbsolutePath),
        ("", F, ExtractReject::EmptyPath),
        (".", D, ExtractReject::EmptyPath),
        ("./././", D, ExtractReject::EmptyPath),
        ("bin/ay", Symlink, ExtractReject::DisallowedKind),
        ("bin/ay", Hardlink, ExtractReject::DisallowedKind),
        ("bin/ay", Other, ExtractReject::DisallowedKind),
    ] {
        assert_eq!(
            vet_entry(&r, Path::new(raw), kind),
            Err(why),
            "{raw} {kind:?}"
        );
    }
}

#[test]
fn strip_components_vets_the_full_path_first() {
    let r = root();
    assert_eq!(
        vet_entry_stripped(&r, Path::new("gh_2.80.0/bin/gh"), EntryKind::Regular, 1).unwrap(),
        Some(r.join("bin/gh"))
    );
    assert_eq!(
        vet_entry_stripped(&r, Path::new("gh_2.80.0/"), EntryKind::Directory, 1).unwrap(),
        None
    );
    assert_eq!(
        vet_entry_stripped(&r, Path::new("a/b/c"), EntryKind::Regular, 3).unwrap(),
        None
    );
    assert_eq!(
        vet_symlink(&r, Path::new("./top/bin/link"), Path::new("gh"), 1).unwrap(),
        Some(r.join("top/bin/link"))
    );
    assert_eq!(
        vet_entry_stripped(&r, Path::new("../x/y"), EntryKind::Regular, 1),
        Err(ExtractReject::ParentTraversal),
        "a `..` above the strip depth is not a way in"
    );
    assert_eq!(
        vet_entry_stripped(&r, Path::new("/x/y"), EntryKind::Regular, 1),
        Err(ExtractReject::AbsolutePath)
    );
    assert_eq!(
        vet_entry_stripped(&r, Path::new("x/../y"), EntryKind::Regular, 1),
        Err(ExtractReject::ParentTraversal)
    );
    assert_eq!(
        vet_hardlink_stripped(
            &r,
            Path::new("top/bin/cargo"),
            Path::new("top/bin/targo"),
            1
        )
        .unwrap(),
        Some((r.join("bin/cargo"), r.join("bin/targo")))
    );
    assert_eq!(
        vet_hardlink_stripped(&r, Path::new("top/bin/cargo"), Path::new("targo"), 1),
        Err(ExtractReject::HardlinkTargetMissing)
    );
    assert_eq!(
        vet_hardlink_stripped(&r, Path::new("top"), Path::new("top/bin/targo"), 1).unwrap(),
        None
    );
}

#[test]
fn symlink_vet_resolves_targets_lexically_inside_the_root() {
    let r = root();
    let ok = |raw: &str, target: &str| {
        vet_symlink(&r, Path::new(raw), Path::new(target), 0)
            .unwrap_or_else(|e| panic!("{raw} -> {target}: {e:?}"))
    };
    assert_eq!(
        ok("bin/emacs", "../Emacs.app/Contents/MacOS/Emacs"),
        Some(r.join("bin/emacs"))
    );
    assert_eq!(
        ok("lib/libfoo.dylib", "libfoo.1.dylib"),
        Some(r.join("lib/libfoo.dylib"))
    );
    assert_eq!(ok("a/b/c", "../../x"), Some(r.join("a/b/c")));
    assert_eq!(ok("a/b/c", "./../d/./e"), Some(r.join("a/b/c")));
    assert_eq!(ok("top", "sub/dir"), Some(r.join("top")));
    let bad = |raw: &str, target: &str| {
        vet_symlink(&r, Path::new(raw), Path::new(target), 0).unwrap_err()
    };
    assert_eq!(bad("top", "../outside"), ExtractReject::RootEscape);
    assert_eq!(bad("a/b/c", "../../../x"), ExtractReject::RootEscape);
    assert_eq!(
        bad("a/b/c", "../../../a/b/d"),
        ExtractReject::RootEscape,
        "lexical: no coming back"
    );
    assert_eq!(
        bad("bin", "a/b/c/up/../OUTSIDE"),
        ExtractReject::RootEscape,
        "a non-leading `..` can chain through an earlier link"
    );
    assert_eq!(bad("a/b/c", "../d/../e"), ExtractReject::RootEscape);
    assert_eq!(bad("a/b/c", "./d/.."), ExtractReject::RootEscape);
    assert_eq!(bad("bin/x", "/etc/passwd"), ExtractReject::AbsolutePath);
    assert_eq!(bad("bin/x", ""), ExtractReject::EmptyPath);
    assert_eq!(bad("../x", "y"), ExtractReject::ParentTraversal);
    assert_eq!(bad("/x", "y"), ExtractReject::AbsolutePath);
    assert_eq!(
        vet_symlink(&r, Path::new("top/bin/x"), Path::new("../y"), 1).unwrap(),
        Some(r.join("bin/x"))
    );
    assert_eq!(
        vet_symlink(&r, Path::new("top"), Path::new("y"), 1).unwrap(),
        None
    );
    assert_eq!(
        vet_symlink(&r, Path::new("top/x"), Path::new("../y"), 1),
        Err(ExtractReject::RootEscape)
    );
}

fn dest(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-ex-{label}-"))
}

fn raw_header(name: &str, typeflag: u8, linkname: &str, size: usize) -> [u8; 512] {
    raw_header_mode(name, typeflag, linkname, size, 0o644)
}

fn make_archive(dir: &Path, label: &str, entries: &[(&str, u8, &str, &[u8])]) -> PathBuf {
    let moded: Vec<_> = entries
        .iter()
        .map(|(n, tf, l, c)| (*n, *tf, *l, *c, 0o644u32))
        .collect();
    make_archive_moded(dir, label, &moded)
}

fn fused_and_walked(tree: TreeAccumulator, root: &Path) -> (String, String) {
    tree.require_spelled_back(root)
        .expect("the disk spells back every name the fold records");
    let walked = crate::tree::tree_root_declared(root, &tree.declared_modes()).unwrap();
    (tree.root(), walked)
}

/// [`make_archive`] with a per-entry archive mode. Same bytes for a `0o644` entry.
#[allow(clippy::type_complexity)]
fn make_archive_moded(
    dir: &Path,
    label: &str,
    entries: &[(&str, u8, &str, &[u8], u32)],
) -> PathBuf {
    let path = dir.join(format!("{label}.tar.zst"));
    std::fs::write(&path, zstd_bytes(&tar_bytes(entries))).unwrap();
    path
}

#[test]
fn generic_zstd_extraction_keeps_strip_and_byte_bounds() {
    let d = dest("generic-zstd");
    let archive = make_archive(
        d.as_path(),
        "good",
        &[("release/lib/Std.olean", b'0', "", b"module")],
    );
    let opts = ExtractOptions {
        strip_components: 1,
        ..ExtractOptions::default()
    };
    let stage = d.as_path().join("stage");
    extract_tree(&archive, &stage, Container::TarZst, 4096, opts).unwrap();
    assert_eq!(
        std::fs::read(stage.join("lib/Std.olean")).unwrap(),
        b"module"
    );
    assert!(!stage.join("release").exists());
    assert!(matches!(
        extract_tree(
            &archive,
            &d.as_path().join("small"),
            Container::TarZst,
            1,
            opts
        ),
        Err(ExtractError::TooLarge(_))
    ));
    let bad = make_archive(
        d.as_path(),
        "escape",
        &[("release/../outside", b'0', "", b"escape")],
    );
    assert!(matches!(
        extract_tree(
            &bad,
            &d.as_path().join("bad"),
            Container::TarZst,
            4096,
            opts
        ),
        Err(ExtractError::Rejected(ExtractReject::ParentTraversal, _))
    ));
    assert!(!d.as_path().join("outside").exists());
}

#[cfg(unix)]
#[test]
fn generic_zstd_extraction_keeps_the_requested_link_policy() {
    let d = dest("generic-zstd-links");
    let archive = make_archive(
        d.as_path(),
        "links",
        &[
            ("release/bin/lean", b'0', "", b"compiler"),
            ("release/bin/alias", b'2', "lean", b""),
        ],
    );
    let opts = ExtractOptions {
        strip_components: 1,
        symlinks: SymlinkPolicy::InRoot,
        ..ExtractOptions::default()
    };
    let stage = d.as_path().join("stage");
    extract_tree(&archive, &stage, Container::TarZst, 4096, opts).unwrap();
    assert_eq!(
        std::fs::read_link(stage.join("bin/alias")).unwrap(),
        Path::new("lean")
    );
    assert!(matches!(
        extract_tree(
            &archive,
            &d.as_path().join("refused"),
            Container::TarZst,
            4096,
            ExtractOptions {
                symlinks: SymlinkPolicy::Refuse,
                ..opts
            }
        ),
        Err(ExtractError::Rejected(ExtractReject::DisallowedKind, _))
    ));
}

/// The env that turns [`probe_stages_its_directories_0755_under_an_open_umask_if_asked`] on:
/// the scratch directory it stages under.
#[cfg(unix)]
const OPEN_UMASK_ENV: &str = "ATPKG_EXTRACT_OPEN_UMASK_DIR";
/// That probe's name, as `--exact` wants it.
#[cfg(unix)]
const OPEN_UMASK_PROBE: &str =
    "extract::tests::a_folding_lane_makes_every_directory_0755_whatever_the_umask";

/// The child half of [`a_folding_lane_makes_every_directory_0755_whatever_the_umask`]:
/// under umask `000`, implicit (`share/`, `share/doc/`) and explicit (`bin/`)
/// directories stage at `0755`.
#[cfg(unix)]
fn probe_stages_its_directories_0755_under_an_open_umask_if_asked() -> bool {
    let Some(dir) = std::env::var_os(OPEN_UMASK_ENV) else {
        return false;
    };
    // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail; this
    // process runs this one test (`--exact`), so no other create sees the mask.
    unsafe { libc::umask(0) };
    let d = PathBuf::from(dir);
    let root = d.join("staging");
    std::fs::create_dir_all(&root).unwrap();
    let archive = make_archive_moded(
        &d,
        "implicit",
        &[
            ("share/doc/readme", b'0', "", b"hello", 0o644),
            ("bin/", b'5', "", b"", 0o777),
            ("bin/tool", b'0', "", b"#!/bin/sh\n", 0o755),
        ],
    );
    let tree = extract_tar_zst_tree(
        &archive,
        &root,
        10_000_000,
        10_000,
        ExtractOptions::default(),
        FrameChecksum::IfPresent,
    )
    .unwrap();
    let (fused, walked) = fused_and_walked(tree, &root);
    assert_eq!(fused, walked);
    for dir in ["share", "share/doc", "bin"] {
        let mode = std::fs::metadata(root.join(dir))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o755, "{dir}: staged under umask 000");
    }
    true
}

/// The directories a folding lane makes because a member's path passes through them
/// ([`make_dir_exact`]) get `0755` whatever the umask, like a directory member.
#[cfg(unix)]
#[test]
fn a_folding_lane_makes_every_directory_0755_whatever_the_umask() {
    if probe_stages_its_directories_0755_under_an_open_umask_if_asked() {
        return;
    }
    let d = dest("open-umask");
    let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
        .env(OPEN_UMASK_ENV, d.as_os_str())
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the probe under umask 000: {}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_release_lane_refuses_symlinks_and_escaping_or_dangling_hardlinks() {
    let d = dest("release-links");
    let empty = b"".as_slice();
    for (label, entries, never) in [
        ("symlink", vec![("link", b'2', "bin/real", empty)], "link"),
        (
            "hl-escape",
            vec![
                ("bin/real", b'0', "", b"x".as_slice()),
                ("bin/evil", b'1', "../../etc/passwd", empty),
            ],
            "bin/evil",
        ),
        (
            "hl-abs",
            vec![("bin/evil", b'1', "/etc/passwd", empty)],
            "bin/evil",
        ),
        (
            "hl-forward",
            vec![("bin/evil", b'1', "bin/not-yet", empty)],
            "bin/evil",
        ),
    ] {
        let root = d.join(label);
        let archive = make_archive(&d, label, &entries);
        let err = extract_tar_zst_tree(
            &archive,
            &root,
            10_000_000,
            10_000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
        .unwrap_err();
        assert!(
            matches!(err, ExtractError::Rejected(..)),
            "{label}: {err:?}"
        );
        assert!(
            std::fs::symlink_metadata(root.join(never)).is_err(),
            "{label}: {never} was laid"
        );
    }
}

#[test]
fn every_tar_bomb_defence_refuses_with_its_own_reason() {
    use TooLargeReason as Why;
    type Case<'a> = (
        &'a str,
        &'a [u8],
        u64,
        u64,
        &'a str,
        fn(&ExtractError) -> bool,
    );
    let d = dest("bombs");
    let eoa = |mut tar: Vec<u8>| {
        tar.resize(tar.len() + 1024, 0);
        tar
    };
    let dir_bomb = eoa(raw_header("payload/", b'5', "", 50 << 20).to_vec());
    let link_bomb = eoa(raw_header_mode("lnk", b'2', "target", 50 << 20, 0o777).to_vec());
    let hardlink_body = vec![0u8; 1_000_000];
    let hardlink_bomb = tar_bytes(&[
        ("bin/targo", b'0', "", b"the one binary", 0o644),
        ("bin/cargo", b'1', "bin/targo", &hardlink_body, 0o644),
    ]);
    let longname = 8usize << 20;
    let mut ext_bomb = raw_header("././@LongLink", b'L', "", longname).to_vec();
    ext_bomb.resize(ext_bomb.len() + longname, 0);
    let ext_bomb = eoa(ext_bomb);
    let big = tar_bytes(&[("data", b'0', "", &[b'x'; 4096], 0o644)]);
    let cases: [Case<'_>; 7] = [
        (
            "directory body",
            &dir_bomb,
            10_000_000,
            10_000,
            "payload",
            |e| matches!(e, ExtractError::TooLarge(Why::DirectoryWithBody(m)) if m == Path::new("payload/")),
        ),
        ("symlink body", &link_bomb, 10_000_000, 10, "lnk", |e| {
            matches!(e, ExtractError::TooLarge(Why::SymlinkWithBody(_)))
        }),
        (
            "hardlink body, small cap",
            &hardlink_bomb,
            64 * 1024,
            10_000,
            "bin/cargo",
            |e| matches!(e, ExtractError::TooLarge(Why::HardlinkWithBody(_))),
        ),
        (
            "hardlink body, large cap",
            &hardlink_bomb,
            10_000_000,
            10_000,
            "bin/cargo",
            |e| matches!(e, ExtractError::TooLarge(Why::HardlinkWithBody(_))),
        ),
        ("longname body", &ext_bomb, 64 * 1024, 16, "", |e| {
            matches!(e, ExtractError::TooLarge(Why::StructuralBudget))
        }),
        ("content", &big, 1024, 10_000, "", |e| {
            matches!(e, ExtractError::TooLarge(Why::ContentCap))
        }),
        ("entries", &big, 10_000_000, 0, "", |e| {
            matches!(e, ExtractError::TooLarge(Why::EntryCap))
        }),
    ];
    for (i, (label, tar, cap, entries, never, want)) in cases.into_iter().enumerate() {
        let archive = d.join(format!("{i}.tar.zst"));
        std::fs::write(&archive, zstd_bytes(tar)).unwrap();
        let root = d.join(format!("{i}"));
        let err = extract_tar_zst_tree(
            &archive,
            &root,
            cap,
            entries,
            vendor(0),
            FrameChecksum::IfPresent,
        )
        .unwrap_err();
        assert!(want(&err), "{label}: {err:?}");
        if !never.is_empty() {
            assert!(
                std::fs::symlink_metadata(root.join(never)).is_err(),
                "{label}: {never} was laid"
            );
        }
    }
}

#[test]
fn the_tar_structure_counts_against_the_signed_cap() {
    let d = dest("structure-cap");
    let pax_body_len = 1_040_000usize;
    let mut record = format!("{pax_body_len} comment=").into_bytes();
    record.resize(pax_body_len - 1, b'a');
    record.push(b'\n');
    let mut pax = Vec::new();
    for _ in 0..4 {
        pax.extend_from_slice(&raw_header("PaxHeader/d", b'x', "", pax_body_len));
        pax.extend_from_slice(&record);
        pax.resize(pax.len().div_ceil(512) * 512, 0);
        pax.extend_from_slice(&raw_header("d/", b'5', "", 0));
    }
    pax.resize(pax.len() + 1024, 0);
    let mut dirs = Vec::new();
    for _ in 0..3_000 {
        dirs.extend_from_slice(&raw_header("d/", b'5', "", 0));
    }
    dirs.resize(dirs.len() + 1024, 0);
    for (label, tar) in [("pax", pax), ("dirs", dirs)] {
        let archive = d.join(format!("{label}.tar.zst"));
        std::fs::write(&archive, zstd_bytes(&tar)).unwrap();
        let refused = extract_tar_zst_tree(
            &archive,
            &d.join(format!("{label}-1mib")),
            1 << 20,
            10_000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        );
        assert!(
            matches!(
                refused,
                Err(ExtractError::TooLarge(TooLargeReason::StructureCap))
            ),
            "{label}: {} decompressed bytes and not one of content passed a 1 MiB cap: \
             {:?}",
            tar.len(),
            refused.map(|_| ())
        );
        let pulled = tar.len() as u64;
        for (cap, admitted) in [(pulled, true), (pulled - 1, false)] {
            let got = extract_tar_zst_tree(
                &archive,
                &d.join(format!("{label}-{cap}")),
                cap,
                10_000,
                ExtractOptions::default(),
                FrameChecksum::IfPresent,
            );
            assert_eq!(
                got.is_ok(),
                admitted,
                "{label}: cap {cap} against {pulled} pulled bytes: {:?}",
                got.map(|_| ())
            );
        }
    }
}

#[test]
fn a_small_file_tree_spends_structure_and_content_apart() {
    let d = dest("small-files");
    let (files, size) = (16usize, 760usize);
    let mut tar = Vec::new();
    for i in 0..files {
        tar.extend_from_slice(&raw_header(&format!("share/{i:05}"), b'0', "", size));
        tar.extend(std::iter::repeat_n(b'x', size));
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    let content = (files * size) as u64;
    let structure = tar.len() as u64 - content;
    assert!(
        structure > content,
        "the fixture must carry more structure than content: {structure} vs {content}"
    );
    let archive = d.join("small.tar.zst");
    std::fs::write(&archive, zstd_bytes(&tar)).unwrap();
    let signed_cap = 2 * content;
    assert!(
        content + structure > signed_cap,
        "one shared counter must refuse what two allowances admit: {content} + \
         {structure} vs {signed_cap}"
    );
    let root = d.join("signed-cap");
    let tree = extract_tar_zst_tree(
        &archive,
        &root,
        signed_cap,
        10_000,
        ExtractOptions::default(),
        FrameChecksum::IfPresent,
    );
    assert!(
        tree.is_ok(),
        "{content} B of content and {structure} B of structure, each under the \
         {signed_cap} B cap, must install: {:?}",
        tree.map(|_| ())
    );
    assert_eq!(
        std::fs::read(root.join(format!("share/{:05}", files - 1))).unwrap(),
        vec![b'x'; size]
    );
    let at = |cap: u64| {
        extract_tar_zst_tree(
            &archive,
            &d.join(format!("cap-{cap}")),
            cap,
            10_000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
        .map(|_| ())
    };
    let exact = at(structure);
    assert!(
        exact.is_ok(),
        "cap {structure} = the structure, over the {content} B content: {exact:?}"
    );
    let under = at(structure - 1);
    assert!(
        matches!(
            under,
            Err(ExtractError::TooLarge(TooLargeReason::StructureCap))
        ),
        "cap {} is one byte under the structure: {under:?}",
        structure - 1
    );
}

fn zstd_refusal(err: &ExtractError) -> Option<&aterm_zstd::DecodeError> {
    match err {
        ExtractError::Io { err, .. } => err.get_ref()?.downcast_ref(),
        _ => None,
    }
}

#[cfg(unix)]
#[test]
fn the_write_only_zstd_lane_refuses_a_preexisting_symlink_destination() {
    let d = dest("write-only-existing-link");
    let outside = d.join("outside");
    let stage = d.join("stage");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&stage).unwrap();
    let victim = outside.join("tool");
    std::fs::write(&victim, b"keep outside bytes").unwrap();
    std::os::unix::fs::symlink(&outside, stage.join("bin")).unwrap();
    let archive = make_archive(&d, "escape", &[("bin/tool", b'0', "", b"replacement")]);
    let result = extract_tar_zst(&archive, &stage, 1 << 20, 10);
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep outside bytes");
    assert!(result.is_err(), "an occupied stage must be refused");
}

#[cfg(unix)]
#[test]
fn an_empty_symlink_root_is_not_an_extraction_stage() {
    let d = dest("symlink-root");
    let outside = d.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let stage = d.join("stage");
    std::os::unix::fs::symlink(&outside, &stage).unwrap();
    let archive = make_archive(&d, "escape", &[("tool", b'0', "", b"replacement")]);
    assert!(extract_tar_zst(&archive, &stage, 1 << 20, 10).is_err());
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    assert!(
        extract_tar_zst_tree(
            &archive,
            &stage,
            1 << 20,
            10,
            ExtractOptions::default(),
            FrameChecksum::IfPresent
        )
        .is_err()
    );
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
}

#[test]
fn a_zstd_lane_checks_a_checksum_past_the_tar_end() {
    let d = dest("checksum-past-end");
    let block = aterm_zstd::frame::BLOCK_MAX;
    let body = vec![b'x'; block - 1024];
    let mut tar = raw_header("f", b'0', "", body.len()).to_vec();
    tar.extend_from_slice(&body);
    tar.resize(block + 512, 0);
    tar.resize(tar.len().div_ceil(10_240) * 10_240, 0);
    let mut zst = Vec::new();
    aterm_zstd::frame::write_raw(&mut zst, &tar, aterm_zstd::frame::Checksum::Xxh64).unwrap();
    let good = d.join("good.tar.zst");
    std::fs::write(&good, &zst).unwrap();
    let last = zst.len() - 1;
    zst[last] ^= 1;
    let bad = d.join("bad.tar.zst");
    std::fs::write(&bad, &zst).unwrap();

    extract_tar_zst(&good, &d.join("good"), 1 << 20, 10).unwrap();
    assert_eq!(std::fs::read(d.join("good/f")).unwrap(), body);
    for (label, result) in [
        (
            "write-only",
            extract_tar_zst(&bad, &d.join("bad"), 1 << 20, 10),
        ),
        (
            "folding",
            extract_tar_zst_tree(
                &bad,
                &d.join("bad-tree"),
                1 << 20,
                10,
                ExtractOptions::default(),
                FrameChecksum::IfPresent,
            )
            .map(|_| ()),
        ),
    ] {
        let err = result.unwrap_err();
        assert!(
            matches!(
                zstd_refusal(&err),
                Some(aterm_zstd::DecodeError::Checksum { .. })
            ),
            "{label}: {err:?}"
        );
    }
    let unread = extract_stream(
        zstd_decoder(&bad, FrameChecksum::IfPresent).unwrap(),
        &d.join("unread"),
        1 << 20,
        10,
        false,
        ExtractOptions::default(),
        Trailer::Unread,
    );
    assert!(unread.is_ok(), "{:?}", unread.map(|_| ()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_required_checksum_refuses_a_frame_without_one_before_any_entry() {
    let d = dest("checksum-required");
    let tar = tar_bytes(&[("bin/ay", b'0', "", b"#!/bin/true\n".as_slice(), 0o755)]);
    let mut bare = Vec::new();
    aterm_zstd::frame::write_raw(&mut bare, &tar, aterm_zstd::frame::Checksum::Omitted).unwrap();
    let (bare_zst, checked_zst) = (d.join("bare.tar.zst"), d.join("checked.tar.zst"));
    std::fs::write(&bare_zst, bare).unwrap();
    std::fs::write(&checked_zst, zstd_bytes(&tar)).unwrap();
    let at = |archive: &Path, label: &str, checksum: FrameChecksum| {
        let root = d.join(label);
        let got = extract_tar_zst_tree(
            archive,
            &root,
            1 << 20,
            10,
            ExtractOptions::default(),
            checksum,
        )
        .map(|_| ());
        (root, got)
    };

    let (root, refused) = at(&bare_zst, "refused", FrameChecksum::Required);
    let err = refused.unwrap_err();
    assert_eq!(
        zstd_refusal(&err),
        Some(&aterm_zstd::DecodeError::NoChecksum),
        "{err:?}"
    );
    assert_eq!(
        std::fs::read_dir(&root).map_or(0, Iterator::count),
        0,
        "nothing is laid before the refusal"
    );
    for (archive, label, checksum) in [
        (&bare_zst, "if-present", FrameChecksum::IfPresent),
        (&checked_zst, "checked", FrameChecksum::Required),
    ] {
        let (root, got) = at(archive, label, checksum);
        assert!(got.is_ok(), "{label}: {got:?}");
        assert_eq!(
            std::fs::read(root.join("bin/ay")).unwrap(),
            b"#!/bin/true\n",
            "{label}"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn fused_tree_root_is_byte_identical_to_the_on_disk_walk() {
    let d = dest("fused-parity");
    let root = d.join("staging");
    std::fs::create_dir_all(&root).unwrap();
    let big: Vec<u8> = (0..200_000usize).map(|i| (i % 251) as u8).collect();
    let archive = make_archive_moded(
        &d,
        "parity",
        &[
            ("bin/", b'5', "", b"", 0o755),
            ("bin/targo", b'0', "", b"the one binary", 0o755),
            ("bin/cargo", b'1', "bin/targo", b"", 0o644),
            ("share/doc/readme", b'0', "", b"hello", 0o644),
            ("share/empty", b'0', "", b"", 0o644),
            ("lib/big.so", b'0', "", big.as_slice(), 0o644),
            ("share/doc/readme", b'0', "", b"hello, again", 0o644),
            ("bin/targo", b'0', "", b"the replaced binary", 0o755),
        ],
    );
    let tree = extract_tar_zst_tree(
        &archive,
        &root,
        10_000_000,
        10_000,
        ExtractOptions::default(),
        FrameChecksum::IfPresent,
    )
    .unwrap();
    let (fused, walked) = fused_and_walked(tree, &root);
    assert_eq!(
        fused, walked,
        "the extraction-time root must equal the on-disk walk byte for byte"
    );
    assert_eq!(fused.len(), 64);
    assert_eq!(
        std::fs::read(root.join("bin/cargo")).unwrap(),
        b"the replaced binary",
        "the alias must carry the target's FINAL bytes"
    );
    assert_eq!(
        std::fs::read(root.join("share/doc/readme")).unwrap(),
        b"hello, again"
    );
    assert_eq!(
        std::fs::read(root.join("lib/big.so")).unwrap().len(),
        200_000
    );
    assert!(std::fs::read(root.join("share/empty")).unwrap().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(root.join("bin/cargo")).unwrap().ino(),
            std::fs::metadata(root.join("bin/targo")).unwrap().ino(),
            "the hardlink shape must be present for the parity to mean anything"
        );
        let mode_of = |p: &str| {
            std::fs::metadata(root.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode_of("bin/targo"), 0o755, "the exec mode class");
        assert_eq!(
            mode_of("share/doc/readme"),
            0o644,
            "the non-exec mode class"
        );
    }
}

#[test]
fn a_member_the_filesystem_names_like_another_never_replaces_it_unseen() {
    let d = dest("fs-folded-names");
    for (first, second) in [("bin/tool", "bin/TOOL"), ("caf\u{e9}", "cafe\u{301}")] {
        let entries = [
            (first, b'0', "", b"benign".as_slice(), 0o755),
            (second, b'0', "", b"other!".as_slice(), 0o755),
        ];
        let label = first.len().to_string();
        for (lane, got, root) in in_both_lanes(&d, &label, &entries, vendor(0)) {
            match got {
                Ok(tree) => {
                    let (fused, walked) = fused_and_walked(tree, &root);
                    assert_eq!(fused, walked, "{lane}: the fold must describe the disk");
                }
                Err(ExtractError::Rejected(ExtractReject::Occupied, p)) => {
                    assert_eq!(p, Path::new(second), "{lane}: the second member");
                    assert_eq!(
                        std::fs::read(root.join(first)).unwrap(),
                        b"benign",
                        "{lane}: the first member is left as it was"
                    );
                }
                Err(e) => panic!("{lane}: {e:?}"),
            }
        }
    }
}

#[test]
fn a_directory_the_filesystem_names_like_another_never_merges_unseen() {
    let d = dest("fs-folded-dirs");
    let spellings = [
        ("Dir", "dir"),
        ("caf\u{e9}", "cafe\u{301}"),
        ("a/Sub", "a/sub"),
    ];
    for (pair, (first_dir, folded_dir)) in spellings.into_iter().enumerate() {
        let first = format!("{first_dir}/f");
        let kinds = [
            ("file", b'0', "g", ""),
            ("dir", b'5', "sub/", ""),
            ("symlink", b'2', "l", "f"),
            ("hardlink", b'1', "h", first.as_str()),
        ];
        let kinds = kinds
            .into_iter()
            .filter(|(kind, ..)| cfg!(unix) || *kind != "symlink");
        for (kind, flag, leaf, link) in kinds {
            for (spelled, second_dir) in [("exact", first_dir), ("folded", folded_dir)] {
                let second = format!("{second_dir}/{leaf}");
                let body: &[u8] = if flag == b'0' { b"other!" } else { b"" };
                let entries = [
                    (first.as_str(), b'0', "", b"benign".as_slice(), 0o644),
                    (second.as_str(), flag, link, body, 0o644),
                ];
                let label = format!("{pair}-{kind}-{spelled}");
                for (lane, got, root) in in_both_lanes(&d, &label, &entries, vendor(0)) {
                    match got {
                        Ok(tree) => {
                            let (fused, walked) = fused_and_walked(tree, &root);
                            assert_eq!(fused, walked, "{lane}: the fold must describe the disk");
                        }
                        Err(ExtractError::Rejected(ExtractReject::Occupied, p))
                            if spelled == "folded" =>
                        {
                            assert_eq!(p, Path::new(&second), "{lane}: the second");
                            assert_eq!(
                                std::fs::read(root.join(&first)).unwrap(),
                                b"benign",
                                "{lane}: the first member is left as it was"
                            );
                        }
                        Err(e) => panic!("{lane}: {e:?}"),
                    }
                }
            }
        }
    }
}

#[test]
fn a_name_the_filesystem_stores_under_other_bytes_is_refused() {
    let d = dest("fs-respelled");
    let entries = [
        ("caf\u{e9}/f", b'0', "", b"one".as_slice(), 0o644),
        ("bin/tool", b'0', "", b"two".as_slice(), 0o755),
    ];
    type Respell = fn(&Path);
    let respellings: [(&str, Respell, &str); 2] = [
        (
            "nfd",
            |root| {
                std::fs::rename(root.join("caf\u{e9}"), root.join("cafe\u{301}")).unwrap();
            },
            "caf\u{e9}/f",
        ),
        (
            "vanished",
            |root| std::fs::remove_file(root.join("bin/tool")).unwrap(),
            "bin/tool",
        ),
    ];
    for (how, respell, blamed) in respellings {
        for (lane, got, root) in in_both_lanes(&d, how, &entries, vendor(0)) {
            let tree = got.unwrap_or_else(|e| panic!("{lane}: {e:?}"));
            tree.require_spelled_back(&root)
                .unwrap_or_else(|e| panic!("{lane}: as laid, the names pass: {e:?}"));
            respell(&root);
            match tree.require_spelled_back(&root) {
                Err(ExtractError::Rejected(ExtractReject::Respelled, p)) => {
                    assert_eq!(p, Path::new(blamed), "{lane}: the recorded name");
                }
                other => panic!("{lane}: must be refused, got {other:?}"),
            }
        }
    }
}

#[test]
fn the_fused_root_moves_with_content_and_mode_exactly_as_the_walk_does() {
    fn one_file(d: &Path, label: &str, payload: &[u8], mode: u32) -> String {
        let root = d.join(label);
        std::fs::create_dir_all(&root).unwrap();
        let archive = make_archive_moded(d, label, &[("bin/ay", b'0', "", payload, mode)]);
        let tree = extract_tar_zst_tree(
            &archive,
            &root,
            10_000_000,
            10_000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
        .unwrap();
        let (fused, walked) = fused_and_walked(tree, &root);
        assert_eq!(fused, walked);
        fused
    }
    let d = dest("fused-sensitive");
    let base = one_file(&d, "a", b"payload", 0o644);
    let other_content = one_file(&d, "b", b"payload!", 0o644);
    let other_mode = one_file(&d, "c", b"payload", 0o755);
    assert_eq!(other_mode.len(), 64);
    assert_ne!(base, other_content, "content must move the fused root");
    assert_ne!(base, other_mode, "mode must move the fused root");
}

#[test]
fn a_non_empty_destination_is_refused_rather_than_folded() {
    let d = dest("fused-nonempty");
    let root = d.join("staging");
    std::fs::create_dir_all(&root).unwrap();
    let archive = make_archive(&d, "occupied", &[("bin/ay", b'0', "", b"payload")]);
    let fold = |dest: &Path| {
        extract_tar_zst_tree(
            &archive,
            dest,
            10_000_000,
            10_000,
            ExtractOptions::default(),
            FrameChecksum::IfPresent,
        )
    };
    fold(&root).unwrap();
    assert!(fold(&d.join("absent")).is_ok());
    let err = fold(&root).unwrap_err();
    assert!(
        matches!(&err, ExtractError::Io { err, .. } if err.kind() == io::ErrorKind::AlreadyExists),
        "got {err:?}"
    );
    let planted = d.join("planted");
    std::fs::create_dir_all(&planted).unwrap();
    std::fs::write(planted.join("not-ours"), b"planted").unwrap();
    assert!(fold(&planted).is_err());
}

fn vendor(strip: u32) -> ExtractOptions {
    ExtractOptions {
        strip_components: strip,
        symlinks: SymlinkPolicy::InRoot,
        allow_global_pax: false,
    }
}

#[cfg(unix)]
#[test]
fn external_leaf_links_preserve_targets_without_redirecting_extraction() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let outside = scratch.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("owner"), b"keep").unwrap();
    let target = outside.to_str().unwrap();
    let opts = ExtractOptions {
        symlinks: SymlinkPolicy::ExternalLeaf,
        ..ExtractOptions::default()
    };
    let extract = |entries: &[(&str, u8, &str, &[u8], u32)], label: &str, options| {
        let archive = scratch.path().join(format!("{label}.gz"));
        std::fs::write(&archive, gzip_bytes(&tar_bytes(entries))).unwrap();
        extract_tar_gz_tree(&archive, &scratch.path().join(label), 1 << 20, 20, options)
    };
    let leaf = ("leaf", b'2', target, b"".as_slice(), 0o755);
    for (label, options, reason) in [
        (
            "refused",
            ExtractOptions::default(),
            ExtractReject::DisallowedKind,
        ),
        ("in-root", vendor(0), ExtractReject::AbsolutePath),
    ] {
        assert!(matches!(extract(&[leaf], label, options),
            Err(ExtractError::Rejected(got, _)) if got == reason));
    }
    let tree = extract(&[leaf], "external", opts).unwrap();
    let root = scratch.path().join("external");
    assert_eq!(std::fs::read_link(root.join("leaf")).unwrap(), outside);
    let (fused, walked) = fused_and_walked(tree, &root);
    assert_eq!(fused, walked);
    let relative = ("leaf", b'2', "../outside", b"".as_slice(), 0o755);
    extract(&[relative], "relative", opts).unwrap();
    assert_eq!(
        std::fs::read_link(scratch.path().join("relative/leaf")).unwrap(),
        Path::new("../outside")
    );

    for (label, next, reason) in [
        (
            "on",
            ("leaf", b'0', "", b"overwrite".as_slice(), 0o644),
            ExtractReject::Occupied,
        ),
        (
            "through",
            ("leaf/owner", b'0', "", b"overwrite".as_slice(), 0o644),
            ExtractReject::ThroughSymlink,
        ),
        (
            "link-through",
            ("alias", b'1', "leaf/owner", b"".as_slice(), 0o644),
            ExtractReject::ThroughSymlink,
        ),
        (
            "new-link-through",
            ("leaf/alias", b'2', "owner", b"".as_slice(), 0o644),
            ExtractReject::ThroughSymlink,
        ),
    ] {
        assert!(
            matches!(extract(&[leaf, next], label, opts),
            Err(ExtractError::Rejected(got, _)) if got == reason),
            "{label}"
        );
        assert_eq!(std::fs::read(outside.join("owner")).unwrap(), b"keep");
        assert!(!outside.join("alias").exists());
    }
    for (label, name) in [("traversal", "../outside/alias"), ("absolute", target)] {
        assert!(extract(&[(name, b'2', target, b"", 0o755)], label, opts).is_err());
        assert_eq!(std::fs::read(outside.join("owner")).unwrap(), b"keep");
    }
}

fn global_pax_record(key: &str, value: &str) -> Vec<u8> {
    let payload = format!("{key}={value}\n");
    let mut len = payload.len() + 2;
    loop {
        let record = format!("{len} {payload}");
        if record.len() == len {
            return record.into_bytes();
        }
        len = record.len();
    }
}

#[test]
fn descriptive_global_pax_is_explicit_strict_and_charged_to_the_same_caps() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let opts = ExtractOptions {
        allow_global_pax: true,
        ..ExtractOptions::default()
    };
    let record = global_pax_record("comment", &"a".repeat(800));
    let tar = tar_bytes(&[
        ("pax_global_header", b'g', "", &record, 0o644),
        ("plain", b'0', "", b"", 0o644),
    ]);
    let archive = scratch.path().join("global.zst");
    std::fs::write(&archive, zstd_bytes(&tar)).unwrap();
    let extract = |label: &str, cap, count, options| {
        extract_tar_zst_tree(
            &archive,
            &scratch.path().join(label),
            cap,
            count,
            options,
            FrameChecksum::IfPresent,
        )
    };
    assert!(matches!(
        extract("default", 1 << 20, 2, ExtractOptions::default()),
        Err(ExtractError::Rejected(ExtractReject::DisallowedKind, _))
    ));
    extract("complete", tar.len() as u64, 2, opts).unwrap();
    assert!(scratch.path().join("complete/plain").is_file());
    assert!(!scratch.path().join("complete/pax_global_header").exists());
    assert!(matches!(
        extract("one-byte-less", tar.len() as u64 - 1, 2, opts),
        Err(ExtractError::TooLarge(TooLargeReason::StructureCap))
    ));
    assert!(matches!(
        extract("one-entry-less", 1 << 20, 1, opts),
        Err(ExtractError::TooLarge(TooLargeReason::EntryCap))
    ));

    for (index, body) in [
        global_pax_record("path", "../outside"),
        global_pax_record("linkpath", "/outside"),
        global_pax_record("size", "0"),
        global_pax_record("GNU.sparse.map", "0,1"),
        global_pax_record("hdrcharset", "BINARY"),
        global_pax_record("unknown", "value"),
        b"12 comment=bad\n".to_vec(),
        b"15 comment=bad".to_vec(),
    ]
    .into_iter()
    .enumerate()
    {
        let bytes = tar_bytes(&[
            ("pax_global_header", b'g', "", &body, 0o644),
            ("plain", b'0', "", b"never", 0o644),
        ]);
        std::fs::write(&archive, zstd_bytes(&bytes)).unwrap();
        let label = format!("bad-{index}");
        assert!(extract(&label, 1 << 20, 2, opts).is_err(), "{index}");
        assert!(!scratch.path().join(label).join("plain").exists());
    }
    let mut bytes = raw_header("pax_global_header", b'g', "", record.len()).to_vec();
    bytes.extend_from_slice(&record[..record.len() - 1]);
    std::fs::write(&archive, zstd_bytes(&bytes)).unwrap();
    assert!(extract("truncated", 1 << 20, 2, opts).is_err());
}

#[test]
fn dot_rooted_archives_extract_like_the_tar_that_authored_them() {
    let d = dest("dot-root");
    let tar = tar_bytes(&[
        ("./", b'5', "", b"", 0o755),
        ("./top/", b'5', "", b"", 0o755),
        ("./top/bin/", b'5', "", b"", 0o755),
        ("./top/bin/gh", b'0', "", b"#!/bin/sh\necho gh\n", 0o755),
        ("./top/LICENSE", b'0', "", b"MIT", 0o644),
    ]);
    let gz = d.join("dot.tar.gz");
    std::fs::write(&gz, gzip_bytes(&tar)).unwrap();

    let r0 = d.join("gz0");
    let (root0, walk0) = fused_and_walked(
        extract_tar_gz_tree(&gz, &r0, 10_000_000, 10_000, vendor(0)).unwrap(),
        &r0,
    );
    assert_eq!(
        std::fs::read(r0.join("top/bin/gh")).unwrap(),
        b"#!/bin/sh\necho gh\n"
    );
    assert_eq!(std::fs::read(r0.join("top/LICENSE")).unwrap(), b"MIT");
    assert_eq!(root0, walk0, "fold vs walk");

    let r1 = d.join("gz1");
    extract_tar_gz_tree(&gz, &r1, 10_000_000, 10_000, vendor(1)).unwrap();
    assert!(
        r1.join("top/bin/gh").is_file(),
        "the `.` is the stripped component, as it is for the authoring tar"
    );
    assert!(!r1.join("bin/gh").exists(), "one component too deep");

    let r2 = d.join("gz2");
    extract_tar_gz_tree(&gz, &r2, 10_000_000, 10_000, vendor(2)).unwrap();
    assert!(r2.join("bin/gh").is_file());

    let bad = tar_bytes(&[("./", b'0', "", b"pwned", 0o644)]);
    let bad_gz = d.join("bad.tar.gz");
    std::fs::write(&bad_gz, gzip_bytes(&bad)).unwrap();
    let err =
        extract_tar_gz_tree(&bad_gz, &d.join("gz-bad"), 10_000_000, 10_000, vendor(0)).unwrap_err();
    assert!(
        matches!(&err, ExtractError::Rejected(ExtractReject::EmptyPath, _)),
        "a regular file naming the root is still EmptyPath: {err:?}"
    );
}

/// The tar `entries` laid by the tar.gz lane and — when they hold no hardlink, which
/// zip has not — by the zip lane too, each under its own root in `d`.
#[allow(clippy::type_complexity)]
fn in_both_lanes(
    d: &Path,
    label: &str,
    entries: &[(&str, u8, &str, &[u8], u32)],
    opts: ExtractOptions,
) -> Vec<(String, Result<TreeAccumulator, ExtractError>, PathBuf)> {
    let gz = d.join(format!("{label}.tar.gz"));
    std::fs::write(&gz, gzip_bytes(&tar_bytes(entries))).unwrap();
    let root = d.join(format!("{label}-gz"));
    let got = extract_tar_gz_tree(&gz, &root, 10_000_000, 10_000, opts);
    let mut lanes = vec![(format!("gz {label}"), got, root)];
    if entries.iter().all(|(_, tf, ..)| *tf != b'1') {
        let members: Vec<ZipMember<'_>> = entries
            .iter()
            .map(|(name, tf, link, body, mode)| ZipMember {
                name,
                mode: match tf {
                    b'5' => 0o040_755,
                    b'2' => 0o120_777,
                    _ => 0o100_000 | mode,
                },
                data: if *tf == b'2' { link.as_bytes() } else { body },
                deflate: false,
            })
            .collect();
        let zip = d.join(format!("{label}.zip"));
        std::fs::write(&zip, zip_bytes(&members, false)).unwrap();
        let root = d.join(format!("{label}-zip"));
        let got = extract_zip_tree(&zip, &root, 10_000_000, 10_000, opts);
        lanes.push((format!("zip {label}"), got, root));
    }
    lanes
}

#[test]
fn gzip_and_zip_lanes_refuse_every_slip_shape() {
    let d = dest("lanes-slip");
    for (label, name, tf, link, want) in [
        (
            "dotdot",
            "top/../../OUTSIDE",
            b'0',
            "",
            ExtractReject::ParentTraversal,
        ),
        ("abs", "/OUTSIDE", b'0', "", ExtractReject::AbsolutePath),
        (
            "link-escape",
            "top/bin/x",
            b'2',
            "../../OUTSIDE",
            ExtractReject::RootEscape,
        ),
        (
            "link-abs",
            "top/bin/x",
            b'2',
            "/etc/passwd",
            ExtractReject::AbsolutePath,
        ),
        (
            "link-empty",
            "top/bin/x",
            b'2',
            "",
            ExtractReject::EmptyPath,
        ),
    ] {
        let body: &[u8] = if tf == b'2' { b"" } else { b"pwned" };
        let entries = [
            ("top/ok", b'0', "", b"fine".as_slice(), 0o644),
            (name, tf, link, body, 0o644),
        ];
        for (lane, got, _) in in_both_lanes(&d, label, &entries, vendor(1)) {
            assert!(
                matches!(&got, Err(ExtractError::Rejected(r, _)) if *r == want),
                "{lane}: {:?}",
                got.map(|_| ())
            );
        }
    }
    assert!(
        !d.join("OUTSIDE").exists(),
        "nothing may land outside the roots"
    );
}

/// A symlink can never redirect a later write: a path through an earlier link, a file
/// or hardlink landing on a link, and a link landing on an existing path are each
/// refused, in the tar and zip lanes alike.
#[cfg(unix)]
#[test]
fn writes_through_or_onto_a_symlink_are_refused() {
    type Case = (
        &'static str,
        &'static [(&'static str, u8, &'static str, &'static [u8], u32)],
        ExtractReject,
    );
    let d = dest("through-link");
    let cases: &[Case] = &[
        (
            "through",
            &[
                ("real/", b'5', "", b"", 0o755),
                ("lib", b'2', "real", b"", 0o777),
                ("lib/x", b'0', "", b"through", 0o644),
            ],
            ExtractReject::ThroughSymlink,
        ),
        (
            "onto",
            &[
                ("real", b'0', "", b"r", 0o644),
                ("alias", b'2', "real", b"", 0o777),
                ("alias", b'0', "", b"overwrite", 0o644),
            ],
            ExtractReject::Occupied,
        ),
        (
            "link-over-file",
            &[
                ("real", b'0', "", b"r", 0o644),
                ("real", b'2', "elsewhere", b"", 0o777),
            ],
            ExtractReject::Occupied,
        ),
        (
            "hardlink-onto-link",
            &[
                ("real", b'0', "", b"r", 0o644),
                ("alias", b'2', "real", b"", 0o777),
                ("alias", b'1', "real", b"", 0o644),
            ],
            ExtractReject::Occupied,
        ),
    ];
    for (label, entries, want) in cases {
        for (lane, got, root) in in_both_lanes(&d, label, entries, vendor(0)) {
            assert!(
                matches!(&got, Err(ExtractError::Rejected(r, _)) if r == want),
                "{lane}: {:?}",
                got.map(|_| ())
            );
            if root.join("real").is_file() {
                assert_eq!(std::fs::read(root.join("real")).unwrap(), b"r", "{lane}");
            }
        }
    }
}

/// A hardlink whose target is named through a symlink (`alias -> lnk/f`, `lnk -> real`)
/// is refused by the vet before the link is made: `lstat` and `link` follow every
/// component but the last.
#[cfg(unix)]
#[test]
fn a_hardlink_target_named_through_a_symlink_is_refused_before_the_link_exists() {
    use std::os::unix::fs::MetadataExt;
    let d = dest("hl-through-link");
    let entries: &[(&str, u8, &str, &[u8], u32)] = &[
        ("real/", b'5', "", b"", 0o755),
        ("real/f", b'0', "", b"the one copy", 0o644),
        ("lnk", b'2', "real", b"", 0o777),
        ("alias", b'1', "lnk/f", b"", 0o644),
    ];
    let tar = tar_bytes(entries);
    let gz = d.join("a.tar.gz");
    std::fs::write(&gz, gzip_bytes(&tar)).unwrap();
    let zst = d.join("a.tar.zst");
    std::fs::write(&zst, zstd_bytes(&tar)).unwrap();
    let root_gz = d.join("gz");
    let root_zst = d.join("zst");
    let lanes = [
        (
            "gz",
            extract_tar_gz_tree(&gz, &root_gz, 10_000_000, 10_000, vendor(0)).map(|_| ()),
            &root_gz,
        ),
        (
            "zst",
            extract_tar_zst_tree(
                &zst,
                &root_zst,
                10_000_000,
                10_000,
                vendor(0),
                FrameChecksum::IfPresent,
            )
            .map(|_| ()),
            &root_zst,
        ),
    ];
    for (label, got, root) in lanes {
        let err = got.unwrap_err();
        assert!(
            matches!(
                &err,
                ExtractError::Rejected(ExtractReject::ThroughSymlink, _)
            ),
            "{label}: {err:?}"
        );
        assert!(
            std::fs::symlink_metadata(root.join("alias")).is_err(),
            "{label}: the alias was laid down before the refusal"
        );
        assert_eq!(
            std::fs::metadata(root.join("real/f")).unwrap().nlink(),
            1,
            "{label}: the target must keep its single name"
        );
    }
}

/// Two symlinks that each vet in-root cannot add up to one that leaves it:
/// `a/b/c/up -> ../../..` is the root, and `bin -> a/b/c/up/../OUTSIDE` would walk out
/// of it.
#[cfg(unix)]
#[test]
fn a_chain_of_in_root_symlinks_cannot_escape_the_root() {
    type Entry = (&'static str, u8, &'static str, &'static [u8], u32);
    let d = dest("link-chain");
    std::fs::create_dir_all(d.join("OUTSIDE")).unwrap();
    let dir: Entry = ("a/b/c/", b'5', "", b"", 0o755);
    let up: Entry = ("a/b/c/up", b'2', "../../..", b"", 0o777);
    let bin: Entry = ("bin", b'2', "a/b/c/up/../OUTSIDE", b"", 0o777);
    let file: Entry = ("gh-real", b'0', "", b"x", 0o755);
    let orders: [(&str, [Entry; 4]); 2] = [
        ("up-first", [dir, up, bin, file]),
        ("bin-first", [bin, dir, up, file]),
    ];
    for (label, entries) in &orders {
        for (lane, got, root) in in_both_lanes(&d, label, entries, vendor(0)) {
            let escaped = std::fs::canonicalize(root.join("bin"))
                .is_ok_and(|p| p == std::fs::canonicalize(d.join("OUTSIDE")).unwrap());
            assert!(
                matches!(
                    &got,
                    Err(ExtractError::Rejected(
                        ExtractReject::RootEscape | ExtractReject::ThroughSymlink,
                        _
                    ))
                ),
                "{lane}: {:?} (bin escaped: {escaped})",
                got.map(|_| ())
            );
        }
    }
}

#[test]
fn gzip_and_zip_lanes_enforce_the_same_caps() {
    type Lane = fn(&Path, &Path, u64, u64, ExtractOptions) -> Result<TreeAccumulator, ExtractError>;
    let d = dest("lanes-caps");
    let big = vec![b'x'; 4096];
    let write = |name: &str, bytes: Vec<u8>| {
        let p = d.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let member = |name, mode, data| ZipMember {
        name,
        mode,
        data,
        deflate: true,
    };
    let one_tar = tar_bytes(&[("top/data", b'0', "", &big, 0o644)]);
    let two_tar = tar_bytes(&[("a", b'0', "", b"1", 0o644), ("b", b'0', "", b"2", 0o644)]);
    let lanes: [(&str, Lane, PathBuf, PathBuf); 2] = [
        (
            "gz",
            extract_tar_gz_tree,
            write("big.tar.gz", gzip_bytes(&one_tar)),
            write("two.tar.gz", gzip_bytes(&two_tar)),
        ),
        (
            "zip",
            extract_zip_tree,
            write(
                "big.zip",
                zip_bytes(&[member("top/data", 0o100_644, &big)], false),
            ),
            write(
                "two.zip",
                zip_bytes(
                    &[member("a", 0o100_644, b"1"), member("b", 0o100_644, b"2")],
                    false,
                ),
            ),
        ),
    ];
    for (label, lane, one, two) in lanes {
        let root = |case: &str| d.join(format!("{label}-{case}"));
        assert!(
            matches!(
                lane(&one, &root("cap"), 1024, 10_000, vendor(1)),
                Err(ExtractError::TooLarge(TooLargeReason::ContentCap))
            ),
            "{label}: byte cap"
        );
        assert!(
            matches!(
                lane(&one, &root("strip-cap"), 1024, 10_000, vendor(2)),
                Err(ExtractError::TooLarge(_))
            ),
            "{label}: a stripped-away body is charged"
        );
        std::fs::create_dir_all(root("strip-ok")).unwrap();
        let ok = lane(&one, &root("strip-ok"), 8192, 10_000, vendor(2)).unwrap();
        let (fused, walked) = fused_and_walked(ok, &root("strip-ok"));
        assert_eq!(fused, walked, "{label}: stripped away is not written");
        assert!(
            matches!(
                lane(&two, &root("entries"), 10_000, 1, vendor(0)),
                Err(ExtractError::TooLarge(TooLargeReason::EntryCap))
            ),
            "{label}: entry cap"
        );
    }
    let long = vec![b'a'; 5000];
    let zip = write(
        "longlink.zip",
        zip_bytes(&[member("lnk", 0o120_777, &long)], false),
    );
    let err =
        extract_zip_tree(&zip, &d.join("zip-longlink"), 10_000_000, 10, vendor(0)).unwrap_err();
    assert!(
        matches!(
            err,
            ExtractError::TooLarge(TooLargeReason::LinkTargetTooLong(_))
        ),
        "zip long link: {err:?}"
    );
}

#[test]
fn the_zip_reader_refuses_what_it_does_not_understand_and_reads_zip64() {
    let d = dest("zip-format");
    let members = [
        ZipMember {
            name: "bin/tool",
            mode: 0o100_755,
            data: b"#!/bin/sh\n",
            deflate: true,
        },
        ZipMember {
            name: "README",
            mode: 0o100_644,
            data: b"read me",
            deflate: false,
        },
    ];
    let plain = zip_bytes(&members, false);
    let z64 = zip_bytes(&members, true);
    assert_ne!(plain, z64, "the fixture must really render two encodings");
    let p = d.join("plain.zip");
    std::fs::write(&p, &plain).unwrap();
    let q = d.join("z64.zip");
    std::fs::write(&q, &z64).unwrap();
    let (root_p, walk_p) = fused_and_walked(
        extract_zip_tree(&p, &d.join("plain"), 10_000, 10, vendor(0)).unwrap(),
        &d.join("plain"),
    );
    let root_q = extract_zip_tree(&q, &d.join("z64"), 10_000, 10, vendor(0))
        .unwrap()
        .root();
    assert_eq!(root_p, root_q, "ZIP64 is an encoding, not a different tree");
    assert_eq!(root_p, walk_p);

    let refuse = |label: &str, bytes: &[u8]| {
        let path = d.join(format!("{label}.zip"));
        std::fs::write(&path, bytes).unwrap();
        let err = extract_zip_tree(&path, &d.join(label), 10_000, 10, vendor(0)).unwrap_err();
        assert!(matches!(err, ExtractError::Io { .. }), "{label}: {err:?}");
    };
    let mut enc = plain.clone();
    let cd = enc
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    enc[cd + 8] |= 1;
    refuse("encrypted", &enc);
    let mut meth = plain.clone();
    meth[cd + 10] = 12;
    refuse("method", &meth);
    refuse(
        "backslash",
        &zip_bytes(
            &[ZipMember {
                name: "bin\\tool",
                mode: 0o100_644,
                data: b"x",
                deflate: false,
            }],
            false,
        ),
    );
    refuse("truncated", &plain[..plain.len() - 10]);
    let mut trailing = plain.clone();
    trailing.extend_from_slice(b"garbage");
    refuse("trailing", &trailing);
    let mut lying = plain.clone();
    let readme_cd = lying.windows(6).rposition(|w| w == b"README").unwrap();
    let rec = readme_cd - 46;
    assert_eq!(&lying[rec..rec + 4], &[0x50, 0x4b, 0x01, 0x02]);
    lying[rec + 24..rec + 28].copy_from_slice(&3u32.to_le_bytes());
    refuse("lying-size", &lying);
    refuse("empty", b"");
    let fat = d.join("fat.zip");
    std::fs::write(
        &fat,
        zip_bytes(
            &[ZipMember {
                name: "bin/tool",
                mode: 0,
                data: b"#!/bin/sh\n",
                deflate: false,
            }],
            false,
        ),
    )
    .unwrap();
    let fat_root = d.join("fat");
    extract_zip_tree(&fat, &fat_root, 10_000, 10, vendor(0)).unwrap();
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(fat_root.join("bin/tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o644
    );
}

#[test]
fn stage_file_lays_one_capped_file_and_folds_it() {
    let d = dest("stage-file");
    let dest_path = d.join("bin/claude");
    std::fs::create_dir_all(dest_path.parent().unwrap()).unwrap();
    let payload = b"the binary";
    let w = stage_file(&payload[..], &dest_path, 0o755, 1 << 20).unwrap();
    assert_eq!(w.mode, 0o755, "the raw-binary lane folds mode 0755");
    let mut tree = TreeAccumulator::new();
    tree.record_file(b"bin/claude".to_vec(), w.mode, w.content_sha_hex);
    let (fused, walked) = fused_and_walked(tree, &d);
    assert_eq!(fused, walked);
    let err = stage_file(&payload[..], &d.join("capped"), 0o755, 4).unwrap_err();
    assert!(matches!(err, ExtractError::TooLarge(_)), "capped: {err:?}");
}

/// A disk failure mid-bundle names the step and the path it was on (EACCES from a
/// write-protected stage root stands in for a full disk).
#[cfg(unix)]
#[test]
fn an_io_failure_names_the_operation_and_the_path() {
    let d = dest("io-context");
    let root = d.join("staging");
    std::fs::create_dir_all(&root).unwrap();
    let archive = make_archive(&d, "tool", &[("bin/tool", b'0', "", b"#!/bin/sh\n")]);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
    let got = extract_tar_zst(&archive, &root, 10_000_000, 10_000);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let Err(err) = got else {
        aterm_uds::host::could_not_run(
            "a lay under a mode-0500 root succeeded: root, or a file system that ignores modes",
        );
    };
    let ExtractError::Io { path, .. } = &err else {
        panic!("want Io, got {err:?}");
    };
    assert_eq!(path.as_deref(), Some(root.join("bin").as_path()));
}

/// `write_capped` sets the mode through the handle it holds, not by path: a reader that
/// unlinks `dest` mid-body leaves no path to resolve, yet the write succeeds.
#[cfg(unix)]
#[test]
fn write_capped_sets_the_mode_through_the_open_handle() {
    struct UnlinkMidBody<'a> {
        body: &'a [u8],
        dest: &'a Path,
        done: bool,
    }
    impl Read for UnlinkMidBody<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.done {
                return Ok(0);
            }
            self.done = true;
            let n = self.body.len().min(buf.len());
            buf[..n].copy_from_slice(&self.body[..n]);
            std::fs::remove_file(self.dest)?;
            Ok(n)
        }
    }

    let d = dest("fchmod-handle");
    let path = d.join("lib/rustlib/aarch64-apple-darwin/lib/libstd.rlib");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = b"rlib bytes";
    let budget = Rc::new(Cell::new(TAR_ENTRY_STRUCTURAL_BUDGET));
    let mut buf = vec![0u8; 64 * 1024];
    let mut remaining: u64 = 1 << 20;
    let w = write_capped(
        UnlinkMidBody {
            body,
            dest: &path,
            done: false,
        },
        Sink {
            file: crate::platform::open_create_write(&path, 0o755).unwrap(),
            dest: &path,
            mode: 0o755,
        },
        &mut remaining,
        &budget,
        &mut buf,
        true,
    )
    .unwrap();
    assert_eq!(w.len, body.len() as u64);
    assert_eq!(w.mode, 0o755);
    let mut h = aterm_digest::Sha256::new();
    h.update(body);
    assert_eq!(
        w.content_sha_hex,
        aterm_codec::hex::encode_lower(&h.finalize())
    );
}

fn brotli(content: &[u8]) -> Vec<u8> {
    let mut br = Vec::new();
    aterm_brotli::write_uncompressed(&mut br, content).unwrap();
    br
}

const NPM_JSON: &[u8] = b"{\"name\":\"@xai-official/grok-linux-x64\",\"version\":\"1.0.70\"}";

/// A tar member: name, type flag, link target, content, mode.
type Member<'a> = (&'a str, u8, &'a str, &'a [u8], u32);

/// Stage the npm package `members` as `bin/grok` of `size` bytes.
fn stage_npm(
    label: &str,
    members: &[Member<'_>],
    size: u64,
) -> (crate::testdir::Dir, Result<NpmBinary, ExtractError>) {
    let d = dest(label);
    let archive = d.join("grok.tgz");
    std::fs::write(&archive, gzip_bytes(&tar_bytes(members))).unwrap();
    std::fs::create_dir_all(d.join("tree/bin")).unwrap();
    let got = stage_npm_brotli_binary(&archive, "grok.br", &d.join("tree/bin/grok"), 0o755, size);
    (d, got)
}

/// An npm binary package stages its one `package/bin/grok.br`, decoded, at the vendor's
/// size and 0755, hands back its package.json, and reads and drops the files beside it.
#[test]
fn an_npm_binary_package_stages_its_decoded_binary_and_hands_back_its_manifest() {
    let exe: Vec<u8> = (0..70_000u32).map(|i| (i % 253) as u8).collect();
    let br = brotli(&exe);
    let (d, got) = stage_npm(
        "npm-ok",
        &[
            ("package/bin/grok.br", b'0', "", &br, 0o644),
            ("package/package.json", b'0', "", NPM_JSON, 0o644),
            ("package/README.md", b'0', "", b"# Grok\n", 0o644),
            (
                "package/THIRD_PARTY_NOTICES.md",
                b'0',
                "",
                &[b'n'; 9000],
                0o644,
            ),
        ],
        exe.len() as u64,
    );
    let npm = got.unwrap();
    assert_eq!(npm.package_json, NPM_JSON);
    assert_eq!(npm.written.len, exe.len() as u64);
    assert_eq!(std::fs::read(d.join("tree/bin/grok")).unwrap(), exe);
    let mut h = aterm_digest::Sha256::new();
    h.update(&exe);
    assert_eq!(
        npm.written.content_sha_hex,
        aterm_codec::hex::encode_lower(&h.finalize())
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(d.join("tree/bin/grok"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let laid: Vec<_> = std::fs::read_dir(d.join("tree/bin"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(laid, ["grok"], "nothing beside the binary is written");
}

/// Every member an npm binary package may not carry, and every way its binary or manifest
/// is malformed, is refused: kinds other than a regular file, places other than
/// `package/<name>` and `package/bin/<exe>.br`, a second binary or manifest, a missing one,
/// a stream that does not decode or decodes to another length, a manifest over 4 KiB.
#[test]
fn an_npm_binary_package_refuses_every_other_member_and_length() {
    let exe = b"\x7fELF grok build".to_vec();
    let n = exe.len() as u64;
    let br = brotli(&exe);
    let json = ("package/package.json", b'0', "", NPM_JSON, 0o644);
    let bin = ("package/bin/grok.br", b'0', "", &br[..], 0o644);
    let not_npm = |r: &Result<NpmBinary, ExtractError>, path: &str| matches!(r, Err(ExtractError::Rejected(ExtractReject::NotNpmBinary, p)) if p == Path::new(path));
    let refused_member: Vec<(&str, Vec<Member<'_>>, &str)> = vec![
        (
            "hardlink",
            vec![
                json,
                (
                    "package/bin/grok.br",
                    b'1',
                    "package/package.json",
                    b"",
                    0o644,
                ),
            ],
            "package/bin/grok.br",
        ),
        (
            "symlink",
            vec![
                json,
                ("package/README.md", b'2', "../../x", b"", 0o777),
                bin,
            ],
            "package/README.md",
        ),
        (
            "fifo",
            vec![json, ("package/bin/grok.br", b'6', "", b"", 0o644)],
            "package/bin/grok.br",
        ),
        (
            "dir",
            vec![("package/bin/", b'5', "", b"", 0o755), json, bin],
            "package/bin/",
        ),
        (
            "dot",
            vec![json, ("./package/bin/grok.br", b'0', "", &br, 0o644)],
            "./package/bin/grok.br",
        ),
        (
            "raw",
            vec![json, ("package/bin/grok", b'0', "", &exe, 0o755)],
            "package/bin/grok",
        ),
        (
            "other-root",
            vec![json, bin, ("pkg/README.md", b'0', "", b"x", 0o644)],
            "pkg/README.md",
        ),
        (
            "bare",
            vec![json, bin, ("README.md", b'0', "", b"x", 0o644)],
            "README.md",
        ),
        (
            "deep-bin",
            vec![json, ("package/bin/x/grok.br", b'0', "", &br, 0o644)],
            "package/bin/x/grok.br",
        ),
        (
            "doubled-slash",
            vec![json, ("package//bin/grok.br", b'0', "", &br, 0o644)],
            "package//bin/grok.br",
        ),
        ("second-json", vec![json, bin, json], "package/package.json"),
        ("second-br", vec![json, bin, bin], "package/bin/grok.br"),
        (
            "dotdot",
            vec![json, bin, ("package/..", b'0', "", b"x", 0o644)],
            "package/..",
        ),
    ];
    for (label, members, path) in refused_member {
        let (_d, got) = stage_npm(&format!("npm-{label}"), &members, n);
        assert!(not_npm(&got, path), "{label}: {got:?}");
    }
    for (label, members) in [
        ("no-json", vec![bin]),
        (
            "no-br",
            vec![json, ("package/README.md", b'0', "", b"x", 0o644)],
        ),
        ("empty", vec![]),
    ] {
        let (_d, got) = stage_npm(&format!("npm-{label}"), &members, n);
        assert!(not_npm(&got, ""), "{label}: {got:?}");
        let shown = got.unwrap_err().to_string();
        assert!(shown.contains("must hold one of each"), "{label}: {shown}");
    }
    let mut trailing = br.clone();
    trailing.push(0);
    for (label, stream, size) in [
        ("truncated", br[..br.len() - 1].to_vec(), n),
        ("trailing", trailing, n),
        ("not-brotli", exe.clone(), n),
    ] {
        let (_d, got) = stage_npm(
            &format!("npm-{label}"),
            &[json, ("package/bin/grok.br", b'0', "", &stream, 0o644)],
            size,
        );
        assert!(
            matches!(got, Err(ExtractError::Io { .. })),
            "{label}: {got:?}"
        );
    }
    for size in [n - 1, n + 1, 0] {
        let (_d, got) = stage_npm("npm-size", &[json, bin], size);
        assert!(
            matches!(&got, Err(ExtractError::Rejected(ExtractReject::DecodedSize, p)) if p == Path::new("package/bin/grok.br")),
            "{size}: {got:?}"
        );
    }
    let big = vec![b' '; 4097];
    let (_d, got) = stage_npm(
        "npm-big-json",
        &[("package/package.json", b'0', "", &big, 0o644), bin],
        n,
    );
    assert!(matches!(got, Err(ExtractError::TooLarge(_))), "{got:?}");
    let (_d, got) = stage_npm(
        "npm-4k-json",
        &[("package/package.json", b'0', "", &big[..4096], 0o644), bin],
        n,
    );
    assert_eq!(
        got.unwrap().package_json.len(),
        4096,
        "4 KiB is the bound, not past it"
    );
}

/// Extract `entries` with the writers holding every piece until the walk first flushes
/// ([`super::behind::HOLD_UNTIL_FLUSH`]): a flush the walk skipped would leave a file
/// short, or a replaced file with its earlier bytes laid over its later ones.
#[cfg(unix)]
fn extract_held(
    label: &str,
    entries: &[(&str, u8, &str, &[u8], u32)],
) -> (crate::testdir::Dir, PathBuf, TreeAccumulator) {
    let zst = zstd_bytes(&tar_bytes(entries));
    let d = dest(label);
    let out = d.join("out");
    super::behind::HOLD_UNTIL_FLUSH.with(|hold| hold.set(true));
    let tree = extract_tar_zst_tree_from(
        &zst[..],
        &out,
        1 << 30,
        100,
        ExtractOptions::default(),
        FrameChecksum::IfPresent,
    )
    .unwrap();
    (d, out, tree)
}

/// WRITE BEHIND: a large file whose bytes past its first piece go to the writers lands
/// byte for byte once the walk is done, a hardlink to it included, and folds to the root
/// the disk reads back.
#[cfg(unix)]
#[test]
fn a_large_file_written_behind_is_whole_when_the_walk_is_done() {
    let piece = super::behind::CHUNK;
    let big: Vec<u8> = (0..3 * piece + 4321).map(|i| (i % 253) as u8).collect();
    let (_d, out, tree) = extract_held(
        "behind-whole",
        &[
            ("bin/big", b'0', "", &big, 0o755),
            ("bin/alias", b'1', "bin/big", b"", 0o755),
            ("lib/small", b'0', "", b"small", 0o644),
        ],
    );
    assert_eq!(std::fs::read(out.join("bin/big")).unwrap(), big);
    assert_eq!(std::fs::read(out.join("bin/alias")).unwrap(), big);
    assert_eq!(std::fs::read(out.join("lib/small")).unwrap(), b"small");
    let modes = tree.declared_modes();
    assert_eq!(
        tree.root(),
        crate::tree::tree_root_declared(&out, &modes).unwrap(),
        "what was folded is what the disk holds"
    );
}

/// WRITE BEHIND: a large file laid twice holds its second bytes alone: the first's are
/// all written before the path is opened again.
#[cfg(unix)]
#[test]
fn a_large_file_laid_twice_holds_only_its_second_bytes() {
    let piece = super::behind::CHUNK;
    let first: Vec<u8> = (0..2 * piece + 7).map(|i| (i % 241) as u8).collect();
    let second: Vec<u8> = (0..piece + 5).map(|i| (i % 239) as u8).collect();
    let (_d, out, tree) = extract_held(
        "behind-twice",
        &[
            ("lib/twice", b'0', "", &first, 0o644),
            ("lib/twice", b'0', "", &second, 0o644),
        ],
    );
    assert_eq!(std::fs::read(out.join("lib/twice")).unwrap(), second);
    let modes = tree.declared_modes();
    assert_eq!(
        tree.root(),
        crate::tree::tree_root_declared(&out, &modes).unwrap()
    );
}
