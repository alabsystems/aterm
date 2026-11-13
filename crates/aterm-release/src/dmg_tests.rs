// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{archive_app, assert_zip_has_no_metadata, command, copy_into_stage};
use std::collections::BTreeSet;
use std::path::Path;

#[test]
fn accelerated_file_digest_binds_every_byte_and_descriptor_size() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("digest");
    for length in [0, 1, 55, 56, 63, 64, 65, 16_383, 16_384, 16_385, 65_537] {
        let mut bytes: Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let expected = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes));
        assert_eq!(
            super::sha256_and_size(&path).unwrap(),
            (expected.clone(), length as u64)
        );
        if let Some(last) = bytes.last_mut() {
            *last ^= 1;
            std::fs::write(&path, &bytes).unwrap();
            assert_ne!(super::sha256_file(&path).unwrap(), expected);
        }
    }
}

/// Credential-free, interleaved comparison of the exact old and new file loops.
#[test]
#[ignore = "measurement: run explicitly with --ignored --nocapture"]
fn measure_cut_digest() {
    use std::io::Read as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("container");
    std::fs::write(&path, vec![0xa5; 64 * 1024 * 1024]).unwrap();
    let mut expected = None;
    for accelerated in [false, true, true, false] {
        let got = crate::cut_timing::measure(
            if accelerated {
                "64MiB ring"
            } else {
                "64MiB scalar"
            },
            || {
                if accelerated {
                    super::sha256_file(&path).unwrap()
                } else {
                    let (mut file, _) = crate::durable::open_regular_file_with_meta(&path).unwrap();
                    let mut hasher = aterm_digest::Sha256::new();
                    let mut buf = [0; 16 * 1024];
                    loop {
                        let n = file.read(&mut buf).unwrap();
                        if n == 0 {
                            break;
                        }
                        hasher.update(&buf[..n]);
                    }
                    aterm_codec::hex::encode_lower(&hasher.finalize())
                }
            },
        );
        if let Some(expected) = &expected {
            assert_eq!(&got, expected);
        }
        expected = Some(got);
    }
    crate::cut_timing::measure("staging process scan", || {
        let processes = crate::bundle::running_processes().unwrap();
        println!("processes={}", processes.len());
        crate::bundle::live_staging_dirs(dir.path(), &processes)
    });
}

fn tagged_app(root: &Path) -> std::path::PathBuf {
    let app = root.join("aterm.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::write(app.join("Contents/MacOS/aterm"), b"#!/bin/sh\n").unwrap();
    std::os::unix::fs::symlink("aterm", app.join("Contents/MacOS/aterm-ctl")).unwrap();
    std::fs::write(app.join("Contents/Info.plist"), b"<plist/>").unwrap();
    for file in ["Contents/MacOS/aterm", "Contents/Info.plist"] {
        let out = command("/usr/bin/xattr")
            .args(["-w", "user.t", "x"])
            .arg(app.join(file))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    app
}

fn attributes(path: &Path) -> String {
    let out = command("/usr/bin/xattr")
        .arg("-lr")
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn attribute_names(path: &Path) -> BTreeSet<String> {
    let out = command("/usr/bin/xattr")
        .arg("-r")
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.rsplit_once(": ").map(|(_, name)| name.to_owned()))
        .collect()
}

#[test]
fn the_updater_zip_and_the_dmg_stage_do_not_copy_source_file_metadata() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-dmg-")
        .tempdir()
        .unwrap();
    let app = tagged_app(dir.path());
    assert!(attributes(&app).contains("user.t"), "the fixture is tagged");

    let zip = dir.path().join("aterm-mac.zip");
    archive_app(&app, &zip).unwrap();
    assert_zip_has_no_metadata(&zip).unwrap();

    let control = dir.path().join("control");
    std::fs::create_dir(&control).unwrap();
    let copied = command("/bin/cp")
        .arg("-R")
        .arg(&app)
        .arg(&control)
        .output()
        .unwrap();
    assert!(copied.status.success(), "{copied:?}");
    assert!(
        attribute_names(&control).contains("user.t"),
        "the ordinary-copy negative control carries the injected source metadata"
    );
    let stage = dir.path().join("stage");
    std::fs::create_dir(&stage).unwrap();
    copy_into_stage(&app, &stage).unwrap();
    // Observe the metadata this fixture put on the source. A live app directory may acquire
    // OS tracking attributes independently of copying, unlike an empty flat-file control.
    assert!(
        !attribute_names(&stage).contains("user.t"),
        "the stage copy carries the injected source metadata: {}",
        attributes(&stage)
    );
    assert_eq!(
        std::fs::read_link(stage.join("aterm.app/Contents/MacOS/aterm-ctl")).unwrap(),
        Path::new("aterm")
    );

    let sequestered = dir.path().join("sequestered.zip");
    let out = command("/usr/bin/ditto")
        .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(&app)
        .arg(&sequestered)
        .output()
        .unwrap();
    assert!(out.status.success());
    let err = assert_zip_has_no_metadata(&sequestered).unwrap_err();
    assert!(err.contains("__MACOSX/"), "{err}");
    let doubled = dir.path().join("doubled.zip");
    let out = command("/usr/bin/ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(&app)
        .arg(&doubled)
        .output()
        .unwrap();
    assert!(out.status.success());
    let err = assert_zip_has_no_metadata(&doubled).unwrap_err();
    assert!(err.contains("._"), "{err}");
}
