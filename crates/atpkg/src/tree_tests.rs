// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn tmp(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-tree-{label}-"))
}

fn write(p: &Path, content: &[u8]) {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, content).unwrap();
}

#[test]
fn a_long_file_streams_to_the_one_shot_digest() {
    let d = tmp("vec");
    let f = d.join("v");
    let long: Vec<u8> = (0..200_003_u32)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    write(&f, &long);
    assert_eq!(
        file_sha512(&f).unwrap(),
        aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA512, &long).as_ref())
    );
    assert_eq!(
        file_sha256(&f).unwrap(),
        aterm_codec::hex::encode_lower(&Sha256::digest(&long))
    );
    assert!(file_sha512(&d.join("absent")).is_err());
    let _ = std::fs::remove_dir_all(&d);
}

/// [`BulkSha256`] gives the scalar reference's digest (archive digests and tree roots
/// are signed, cross-version contracts): FIPS 180-4's published vectors, then
/// pseudo-random messages split in two at each awkward boundary (empty, one byte, the
/// padding's 55/56, a block's 63/64/65, past a 64 KiB read buffer) and fed in runs of
/// awkward chunk sizes.
#[test]
fn bulk_sha256_is_the_reference_digest() {
    let bulk = |parts: &[&[u8]]| {
        let mut h = BulkSha256::new();
        for p in parts {
            h.update(p);
        }
        h.finalize_hex()
    };
    let million_a = vec![b'a'; 1_000_000];
    let vectors: [(&[u8], &str); 5] = [
        (
            b"",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
        ),
        (
            &million_a,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        ),
    ];
    for (msg, want) in vectors {
        assert_eq!(bulk(&[msg]), want);
        assert_eq!(aterm_codec::hex::encode_lower(&Sha256::digest(msg)), want);
    }

    let mut x = 0x9e37_79b9_7f4a_7c15_u64;
    let data: Vec<u8> = (0..(1 << 20) + 1)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[7]
        })
        .collect();
    let edges = [0, 1, 55, 56, 63, 64, 65, 64 * 1024 + 1, (1 << 20) + 1];
    for len in edges {
        let msg = &data[..len];
        let want = aterm_codec::hex::encode_lower(&Sha256::digest(msg));
        for split in edges.into_iter().filter(|&s| s <= len) {
            let (a, b) = msg.split_at(split);
            assert_eq!(bulk(&[a, b]), want, "len {len}, split at {split}");
        }
        for step in [55, 63, 64, 65] {
            let chunks: Vec<&[u8]> = msg.chunks(step).collect();
            assert_eq!(bulk(&chunks), want, "len {len}, {step}-byte chunks");
        }
    }
}

/// The cross-version byte contract pinned to a literal, computed independently from the
/// documented format (`sort(<relpath> 0x00 <octal mode> 0x00 <hex sha256> 0x0A)`,
/// concatenated, SHA-256): the recompute tests stay green if the primitive, framing or
/// sort changes, and signed manifests embed roots from earlier releases.
#[test]
fn tree_root_matches_the_pinned_cross_version_vector() {
    const GOLDEN: &str = "e44608970d54f90c4d54ad7852b174304b86395b89f59b3fcc04a11ae22e38e0";
    let d = tmp("golden");
    write(&d.join("bin/ay"), b"binary");
    write(&d.join("data"), b"x");
    #[cfg(unix)]
    {
        std::fs::set_permissions(d.join("bin/ay"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(d.join("data"), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            tree_root(&d).unwrap(),
            GOLDEN,
            "tree_root moved: every signed manifest in the wild disagrees with this build"
        );
    }
    let mut modes = DeclaredModes::new();
    modes.insert(b"bin/ay".to_vec(), 0o755);
    modes.insert(b"data".to_vec(), 0o644);
    let declared = tree_root_declared(&d, &modes).unwrap();
    assert_eq!(
        declared, GOLDEN,
        "the declared-mode walk must reproduce the root a Unix producer signed"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// The relpath slot is `/`-separated on every platform, though `Path::join` spells the
/// separator `\` on Windows.
#[test]
fn rel_line_bytes_spell_the_separator_as_a_slash() {
    let rel = Path::new("bin").join("sub").join("claude.exe");
    assert_eq!(rel_line_bytes(&rel), b"bin/sub/claude.exe".to_vec());
    assert_eq!(
        rel_line_bytes(Path::new("claude.exe")),
        b"claude.exe".to_vec()
    );
}

/// A signed one-file row (`bin/claude.exe`, mode `0755`) folds to its signed root from
/// exactly that line, and the same content under a `\` separator and a `0` mode folds
/// to the different digest a client with those spellings produced.
#[test]
fn the_measured_windows_claude_row_folds_from_the_declared_line() {
    const CONTENT: &str = "0e4195524b73eb77efbdf3e2b36de5322a29f0ca575dfd2d9b4f946b1d425469";
    let signed = root_of_entry_lines(vec![entry_line(b"bin/claude.exe", 0o755, CONTENT)]);
    assert_eq!(
        signed,
        "15c4346e4e80077255d03ad4590b17d834b2853c5b5e013cd7bb1fb656219165"
    );
    let former_windows_fold = root_of_entry_lines(vec![entry_line(b"bin\\claude.exe", 0, CONTENT)]);
    assert_eq!(
        former_windows_fold,
        "3a446f3033188e9ec3bdaa0868f30863bc7508d3986290fc803316fa7dcd18b5"
    );
    let d = tmp("claude-row");
    write(&d.join("bin/claude.exe"), b"stand-in");
    let mut modes = DeclaredModes::new();
    modes.insert(b"bin/claude.exe".to_vec(), 0o755);
    let expected = root_of_entry_lines(vec![entry_line(
        b"bin/claude.exe",
        0o755,
        &file_sha256(&d.join("bin/claude.exe")).unwrap(),
    )]);
    #[cfg(unix)]
    std::fs::set_permissions(
        d.join("bin/claude.exe"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(tree_root_declared(&d, &modes).unwrap(), expected);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn tree_root_declared_uses_stored_mode_on_posix_and_declared_otherwise() {
    const GOLDEN: &str = "e44608970d54f90c4d54ad7852b174304b86395b89f59b3fcc04a11ae22e38e0";
    let d = tmp("declared-mode");
    write(&d.join("bin/ay"), b"binary");
    write(&d.join("data"), b"x");
    #[cfg(unix)]
    {
        std::fs::set_permissions(d.join("bin/ay"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(d.join("data"), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let mut exec = DeclaredModes::new();
    exec.insert(b"bin/ay".to_vec(), 0o755);
    exec.insert(b"data".to_vec(), 0o644);
    let mut plain = DeclaredModes::new();
    plain.insert(b"bin/ay".to_vec(), 0o644);
    plain.insert(b"data".to_vec(), 0o644);
    let as_exec = tree_root_declared(&d, &exec).unwrap();
    let as_plain = tree_root_declared(&d, &plain).unwrap();
    assert_eq!(as_exec, GOLDEN);
    if crate::platform::HAS_POSIX_MODES {
        assert_eq!(
            as_plain, GOLDEN,
            "a stored POSIX mode is read back, never overridden by declared modes"
        );
    } else {
        assert_ne!(
            as_exec, as_plain,
            "where the filesystem stores no POSIX mode, the declared mode decides the line"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// A symlink is digested by its raw target bytes, never followed or skipped, and the
/// digest moves exactly when the target string moves; a dangling link is a good line.
#[cfg(unix)]
#[test]
fn a_symlink_is_digested_by_its_target_bytes_and_never_followed() {
    let d = tmp("link");
    write(&d.join("real"), b"x");
    std::os::unix::fs::symlink("real", d.join("link")).unwrap();
    let base = tree_root(&d).unwrap();
    assert_eq!(base, tree_root(&d).unwrap(), "deterministic");
    std::os::unix::fs::symlink("real", d.join("link2")).unwrap();
    let with_twin = tree_root(&d).unwrap();
    assert_ne!(base, with_twin, "an added link must move the root");
    std::fs::remove_file(d.join("link2")).unwrap();
    assert_eq!(tree_root(&d).unwrap(), base);
    std::fs::remove_file(d.join("link")).unwrap();
    std::os::unix::fs::symlink("nowhere", d.join("link")).unwrap();
    let dangling = tree_root(&d).unwrap();
    assert_ne!(base, dangling, "a changed target must move the root");
    std::fs::remove_file(d.join("link")).unwrap();
    std::os::unix::fs::symlink("elsewhere", d.join("link")).unwrap();
    assert_ne!(dangling, tree_root(&d).unwrap());
    write(&d.join("sub/inner"), b"inner");
    std::fs::remove_file(d.join("link")).unwrap();
    std::os::unix::fs::symlink("sub", d.join("link")).unwrap();
    let via_dir = tree_root(&d).unwrap();
    std::fs::remove_file(d.join("link")).unwrap();
    std::os::unix::fs::symlink("sub", d.join("link")).unwrap();
    assert_eq!(via_dir, tree_root(&d).unwrap());
    let _ = std::fs::remove_dir_all(&d);
}

/// The symlink line pinned to a literal computed independently from the documented
/// format, `sort(<relpath> 0x00 symlink 0x00 <hex sha256(target bytes)> 0x0A ...)`
/// concatenated and hashed, over `real` (content `x`, mode 0644) and `link -> real`.
#[cfg(unix)]
#[test]
fn symlink_tree_root_matches_the_pinned_cross_version_vector() {
    let d = tmp("golden-link");
    write(&d.join("real"), b"x");
    std::fs::set_permissions(d.join("real"), std::fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink("real", d.join("link")).unwrap();
    assert_eq!(
        tree_root(&d).unwrap(),
        "9b629b43d3775c6c84f676fd875ce601e5d9c3473b8e71b6470b12d48ee2d952",
        "the symlink line moved: every vendor manifest signed against it disagrees"
    );
    assert_eq!(
        symlink_line(b"link", &symlink_target_sha(b"real")),
        b"link\0symlink\0aa33996d60e89311b4d1a920dae03c6d7fa3ae1956c52662e273aad4683e577f\n"
            .to_vec()
    );
    let _ = std::fs::remove_dir_all(&d);
}
