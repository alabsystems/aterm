// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A one-file tar.zst holding `bin/tool` with `body`, and its SHA-256.
fn archive(body: &[u8]) -> (Vec<u8>, String) {
    let tar = crate::extract::fixtures::tar_bytes(&[("bin/tool", b'0', "", body, 0o755)]);
    let zst = crate::extract::fixtures::zstd_bytes(&tar);
    let digest = ring::digest::digest(&ring::digest::SHA256, &zst);
    (zst, aterm_codec::hex::encode_lower(digest.as_ref()))
}

/// A tar.zst row signing `sha256` over `zst`'s size.
fn row(zst: &[u8], sha256: &str) -> Artifact {
    Artifact {
        sha256: sha256.to_string(),
        size: zst.len() as u64,
        payload: String::new(),
        tree_root: String::new(),
        ..crate::vendor::testkit::row()
    }
}

/// A fetch that writes `bytes` into `<dl>.part` in a few slices, as curl does, and renames
/// it onto `dl`.
fn dribbled(bytes: &[u8]) -> impl FnOnce(&Path) -> Result<(), String> + '_ {
    move |dl: &Path| {
        use std::io::Write as _;
        let mut part = dl.as_os_str().to_os_string();
        part.push(".part");
        let mut f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        for slice in bytes.chunks(bytes.len().div_ceil(5).max(1)) {
            f.write_all(slice).map_err(|e| e.to_string())?;
            f.flush().map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(f);
        std::fs::rename(&part, dl).map_err(|e| e.to_string())
    }
}

#[test]
fn a_download_that_hashes_to_its_signed_digest_is_staged_as_it_arrives() {
    let dir = crate::testdir::dir("atpkg-arriving-staged-");
    let (zst, digest) = archive(b"tool body");
    let artifact = row(&zst, &digest);
    let build_dir = dir.join("store/tool/7");
    let dl = dir.join("staging/tool/tool-7.tar.zst");
    std::fs::create_dir_all(dl.parent().unwrap()).unwrap();
    let (fetched, staged) = fetch_staging(&artifact, &dl, &build_dir, dribbled(&zst));
    assert_eq!(fetched, Ok(()));
    let staged = staged.expect("the bytes read hash to the signed digest: the stage stands");
    let scratch = staged.dir().to_path_buf();
    assert_eq!(
        std::fs::read(scratch.join("bin/tool")).unwrap(),
        b"tool body",
        "the tree was laid while the bytes arrived"
    );
    drop(staged);
    assert!(!scratch.exists(), "an unused stage removes its scratch");
}

#[test]
fn a_download_that_misses_its_signed_digest_hands_back_no_stage_and_no_scratch() {
    let dir = crate::testdir::dir("atpkg-arriving-mismatch-");
    let (zst, _) = archive(b"tool body");
    // The row signs other bytes: everything read must be thrown away.
    let (_, other) = archive(b"another body");
    let artifact = row(&zst, &other);
    let build_dir = dir.join("store/tool/7");
    let dl = dir.join("staging/tool/tool-7.tar.zst");
    std::fs::create_dir_all(dl.parent().unwrap()).unwrap();
    let (fetched, staged) = fetch_staging(&artifact, &dl, &build_dir, dribbled(&zst));
    assert_eq!(fetched, Ok(()), "the fetch itself succeeded");
    assert!(
        staged.is_none(),
        "a streamed digest mismatch stages nothing"
    );
    let left: Vec<_> = std::fs::read_dir(dir.join("store/tool"))
        .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "no scratch is left behind: {left:?}");
    assert!(
        crate::install::check_signed_digest(&artifact, &dl).is_err(),
        "the file the caller falls back to is refused by the digest gate as before"
    );
}

#[test]
fn a_partial_left_over_from_an_earlier_run_is_not_read_while_the_fetch_runs() {
    let dir = crate::testdir::dir("atpkg-arriving-leftover-");
    let (zst, digest) = archive(b"tool body");
    let artifact = row(&zst, &digest);
    let build_dir = dir.join("store/tool/7");
    let dl = dir.join("staging/tool/tool-7.tar.zst");
    std::fs::create_dir_all(dl.parent().unwrap()).unwrap();
    // A LAN peer's partial a killed run left: as long as the archive, other bytes.
    let leftover: Vec<u8> = zst.iter().map(|b| b ^ 0x5a).collect();
    let peer_part = crate::peers::arrival_paths(&dl)[1].clone();
    std::fs::write(&peer_part, &leftover).unwrap();
    let (fetched, staged) = fetch_staging(&artifact, &dl, &build_dir, dribbled(&zst));
    assert_eq!(fetched, Ok(()));
    assert!(
        staged.is_some(),
        "the stage read the fetch's bytes, not the leftover's"
    );
}

#[test]
fn a_fetch_that_panics_does_not_strand_the_staging_thread() {
    // With the download writing a partial and then panicking, the staging thread must be
    // released, not left waiting on an end that never comes: the whole call returns (by
    // re-raising the panic) instead of hanging the scope's join forever.
    let dir = crate::testdir::dir("atpkg-arriving-panic-");
    let (zst, digest) = archive(b"tool body");
    let artifact = row(&zst, &digest);
    let build_dir = dir.join("store/tool/7");
    let dl = dir.join("staging/tool/tool-7.tar.zst");
    std::fs::create_dir_all(dl.parent().unwrap()).unwrap();
    let (artifact, dl2, build_dir2, half) = (artifact.clone(), dl.clone(), build_dir, zst.clone());
    let ran = std::thread::Builder::new()
        .name("atpkg-arriving-panic-probe".into())
        .spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fetch_staging(
                    &artifact,
                    &dl2,
                    &build_dir2,
                    |dl: &Path| -> Result<(), String> {
                        let mut part = dl.as_os_str().to_os_string();
                        part.push(".part");
                        let _ = std::fs::write(&part, &half[..half.len() / 2]);
                        panic!("the download blew up");
                    },
                )
            }))
            .is_err()
        })
        .unwrap();
    let began = std::time::Instant::now();
    while !ran.is_finished() {
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "fetch_staging hung after its fetch panicked instead of re-raising"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(ran.join().unwrap(), "the fetch's panic reached the caller");
}

#[test]
fn a_failed_fetch_hands_back_its_error_and_no_stage() {
    let dir = crate::testdir::dir("atpkg-arriving-failed-");
    let (zst, digest) = archive(b"tool body");
    let artifact = row(&zst, &digest);
    let build_dir = dir.join("store/tool/7");
    let dl = dir.join("staging/tool/tool-7.tar.zst");
    std::fs::create_dir_all(dl.parent().unwrap()).unwrap();
    let half = zst.len() / 2;
    let (fetched, staged) = fetch_staging(&artifact, &dl, &build_dir, |dl: &Path| {
        let mut part = dl.as_os_str().to_os_string();
        part.push(".part");
        std::fs::write(&part, &zst[..half]).map_err(|e| e.to_string())?;
        Err::<(), _>("the link dropped".to_string())
    });
    assert_eq!(fetched, Err("the link dropped".to_string()));
    assert!(staged.is_none());
}
