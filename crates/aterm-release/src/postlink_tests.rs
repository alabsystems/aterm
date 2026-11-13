// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The same linked product reports each new identity after signing. A product-byte
/// mutation changes U; stamping without signing leaves a signature that cannot verify.
#[test]
fn two_stamps_execute_with_equal_content_but_product_changes_do_not() {
    use aterm_uds::deadline::{FIRST_EXEC_HANG, KeptUntilRan, prints};
    use aterm_uds::spawnfd::{Stdio, command};
    let dir = aterm_tempfile::tempdir().unwrap();
    let project = dir.path().join("fixture");
    std::fs::create_dir_all(project.join("src")).unwrap();
    let root = aterm_tempfile::manifest_dir()
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(project.join("Cargo.toml"), format!(
        "[package]\nname = \"stamp-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\naterm-stamp = {{ path = {:?} }}\n[workspace]\n", root.join("aterm-stamp"))).unwrap();
    std::fs::write(
        project.join("src/main.rs"),
        r#"
#[used]
#[unsafe(link_section = "__DATA,__aterm_stamp")]
static STAMP: [u8; aterm_stamp::LEN] = aterm_stamp::UNSTAMPED_RECORD;
fn main() {
    aterm_stamp::install(&STAMP, aterm_stamp::Stamp::UNSTAMPED);
    println!("identity {}", aterm_stamp::current().build_id());
}
"#,
    )
    .unwrap();
    let target = project.join("target.noindex");
    let output = command(crate::gates::trust_stage2_bin().unwrap().join("targo"))
        .args(["--unverified", "build", "--offline", "--target-dir"])
        .arg(&target)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = std::fs::read(target.join("debug/stamp-fixture")).unwrap();
    let expected = normalized(&original).unwrap();
    let linked = layout(&original).unwrap();
    for end in [0, 3, 31, 32, linked.stamp.end - 1, linked.signature.start] {
        assert!(normalized(&original[..end]).is_err());
    }
    let mut bad_command = original.clone();
    bad_command[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(normalized(&bad_command).is_err());
    let fat = |slice: &[u8]| {
        let mut fat = vec![0; 32];
        fat[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]);
        for (at, value) in [
            (4, 1),
            (8, u32_at(slice, 4).unwrap()),
            (12, u32_at(slice, 8).unwrap()),
            (16, 32),
            (20, slice.len() as u32),
        ] {
            fat[at..at + 4].copy_from_slice(&value.to_be_bytes());
        }
        fat.extend_from_slice(slice);
        fat
    };
    let fat_content = normalized(&fat(&original)).unwrap();
    let mut bad_fat = fat(&original);
    bad_fat[16..20].fill(0);
    assert!(normalized(&bad_fat).is_err());
    let store = dir.path().join("store");
    let kept = keep_unstamped(&target.join("debug/stamp-fixture"), &store).unwrap();
    assert_eq!(std::fs::read(&kept).unwrap(), original);
    assert_eq!(
        keep_unstamped(&target.join("debug/stamp-fixture"), &store).unwrap(),
        kept
    );
    let mut bundle_content = None;
    for (version, commit, build) in [("1.2.0", "111111111111", 7), ("1.3.0", "222222222222", 8)] {
        let copy = dir.path().join(version);
        let app = copy.join("aterm.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        let bin = app.join("Contents/MacOS/aterm");
        let thin = copy.join("fixture");
        std::fs::copy(target.join("debug/stamp-fixture"), &thin).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), format!("<plist><dict><key>CFBundleShortVersionString</key><string>{version}</string><key>CFBundleVersion</key><string>{build}</string><key>ATermGitCommit</key><string>{commit}</string></dict></plist>")).unwrap();
        let resource = app.join("Contents/Resources/Help.html");
        std::fs::write(&resource, "product help").unwrap();
        let identity = aterm_stamp::Stamp {
            app_version: version,
            git_commit: commit,
            git_commit_full: format!("{commit}{commit}{commit}0000").leak(),
            build_number: build.to_string().leak(),
            build_time: crate::bundle::epoch_to_rfc3339(build).leak(),
            release_build: true,
            ..aterm_stamp::Stamp::UNSTAMPED
        };
        let guest_root = dir.path().join(format!("q4-{version}"));
        let guest_app = crate::profile_pilot::runtime_app(
            root.parent().unwrap(),
            &guest_root,
            &target.join("debug/stamp-fixture"),
            build,
            &identity,
        )
        .unwrap();
        let guest_image = std::fs::read(guest_app.join("Contents/MacOS/aterm")).unwrap();
        assert_eq!(normalized(&guest_image).unwrap(), expected);
        assert_eq!(
            guest_image[layout(&guest_image).unwrap().stamp],
            aterm_stamp::encode(&identity).unwrap()
        );
        assert_eq!(
            std::fs::read(target.join("debug/stamp-fixture")).unwrap(),
            original,
            "Q4 stamping must leave the measured raw image unchanged"
        );
        // NEGATIVE CONTROLS: an old arm's input cannot be overwritten, and
        // an unstamped identity cannot silently change release behavior.
        assert!(
            crate::profile_pilot::runtime_app(
                root.parent().unwrap(),
                &guest_root,
                &target.join("debug/stamp-fixture"),
                build,
                &identity,
            )
            .is_err()
        );
        assert!(
            crate::profile_pilot::runtime_app(
                root.parent().unwrap(),
                &dir.path().join("unstamped-q4"),
                &target.join("debug/stamp-fixture"),
                build,
                &aterm_stamp::Stamp::UNSTAMPED,
            )
            .is_err()
        );
        stamp(&thin, &aterm_stamp::encode(&identity).unwrap()).unwrap();
        std::fs::copy(&thin, &bin).unwrap();
        let unsigned = command(aterm_uds::os_path!("codesign"))
            .args(["--verify", "--strict"])
            .arg(&bin)
            .output()
            .unwrap();
        assert!(
            !unsigned.status.success(),
            "patched page must invalidate the old signature"
        );
        assert!(
            crate::artifact::content_digest(&app).is_err(),
            "U evidence refuses an invalid signature"
        );
        resign(&thin).unwrap();
        assert!(
            keep_unstamped(&thin, &store).is_err(),
            "a stamped slice cannot enter the raw store"
        );
        std::fs::copy(&thin, &bin).unwrap();
        assert_eq!(normalized(&std::fs::read(&bin).unwrap()).unwrap(), expected);
        assert_eq!(
            normalized(&fat(&std::fs::read(&bin).unwrap())).unwrap(),
            fat_content
        );
        let content = crate::artifact::content_digest(&app).unwrap();
        if let Some(expected) = &bundle_content {
            assert_eq!(&content, expected);
        }
        bundle_content = Some(content.clone());
        let source = crate::artifact::Identity {
            version: version.into(),
            build,
            commit: format!("{commit}{commit}{commit}0000"),
        };
        let compilers = crate::artifact::Compilers {
            trustc: "trust".into(),
            rustc: "stable".into(),
            inputs: "seal".into(),
        };
        let lookup = crate::artifact::key(&source, &compilers);
        let address = crate::artifact::stamped_key(&content, &lookup, None);
        assert_ne!(
            address,
            crate::artifact::stamped_key(&"ff".repeat(32), &lookup, None)
        );
        assert_ne!(
            address,
            crate::artifact::stamped_key(&content, &lookup, Some("1..2")),
            "the stamp address includes the index hint"
        );
        let complete = crate::artifact::Complete {
            schema: crate::artifact::SCHEMA.into(),
            key: address.clone(),
            version: source.version,
            build: source.build,
            commit: source.commit,
            trustc: compilers.trustc,
            rustc: compilers.rustc,
            inputs: compilers.inputs,
            snapshot: "snapshot".into(),
            archs: "arm64".into(),
            compiler_line: "Trust".into(),
            macho_sha256: crate::artifact::macho_sha256(&app).unwrap(),
            bundle_digest: crate::artifact::bundle_digest(&app).unwrap(),
            content: Some(content.clone()),
            slices: Vec::new(),
            index_hint: None,
        };
        let entry = store.join(&address);
        std::fs::create_dir_all(&entry).unwrap();
        crate::artifact::copy_bundle(&complete, &app, &complete.app(&store)).unwrap();
        std::fs::write(
            entry.join(crate::artifact::COMPLETE_FILE),
            aterm_toml::to_string(&complete).unwrap(),
        )
        .unwrap();
        std::fs::create_dir_all(store.join("identity")).unwrap();
        std::fs::write(store.join("identity").join(&lookup), &address).unwrap();
        assert_eq!(
            crate::artifact::read(&store, &lookup).unwrap(),
            Some(complete.clone())
        );
        assert_eq!(
            crate::artifact::read(&store, &address).unwrap(),
            Some(complete.clone())
        );
        let plist_path = app.join("Contents/Info.plist");
        let plist = std::fs::read_to_string(&plist_path).unwrap();
        std::fs::write(
            &plist_path,
            crate::bundle::set_plist_string(&plist, "CFBundleVersion", "999").unwrap(),
        )
        .unwrap();
        let mut wrong_plist = complete.clone();
        wrong_plist.bundle_digest = crate::artifact::bundle_digest(&app).unwrap();
        assert_eq!(crate::artifact::content_digest(&app).unwrap(), content);
        assert!(
            crate::artifact::prove(&wrong_plist, &app).is_err(),
            "equal U and matching hashes cannot hide the wrong plist identity"
        );
        std::fs::write(&plist_path, plist).unwrap();
        let mut wrong_stamp = complete;
        wrong_stamp.commit = "ff".repeat(20);
        assert!(
            crate::artifact::prove(&wrong_stamp, &app).is_err(),
            "equal U does not prove the reservation's stamp"
        );
        let wrong = "ab".repeat(32);
        std::fs::write(store.join("identity").join(&wrong), &address).unwrap();
        assert!(
            crate::artifact::read(&store, &wrong).is_err(),
            "the index cannot substitute another reservation"
        );
        std::fs::write(&resource, "changed product help").unwrap();
        assert_ne!(crate::artifact::content_digest(&app).unwrap(), content);
        let mut custody = KeptUntilRan::new(copy);
        let mut child = command(&bin).stdout(Stdio::piped()).spawn().unwrap();
        prints(
            &mut child,
            &format!("identity {version}+g{commit}"),
            FIRST_EXEC_HANG,
        );
        custody.ran();
        assert!(child.wait().unwrap().success());
    }
    let mut changed = original.clone();
    let section = layout(&changed).unwrap().stamp;
    // A byte immediately before the dedicated identity section remains product content.
    changed[section.start - 1] ^= 1;
    assert_ne!(normalized(&changed).unwrap(), expected);
    std::fs::write(&kept, &changed).unwrap();
    resign(&kept).unwrap();
    assert!(
        keep_unstamped(&target.join("debug/stamp-fixture"), &store).is_err(),
        "the content address detects changed signed product bytes"
    );
    std::fs::write(&kept, &original).unwrap();
    let unused = store.join("unstamped").join("aa".repeat(32));
    std::fs::write(&unused, "obsolete raw slice").unwrap();
    prune_unstamped(&store, std::slice::from_ref(&kept)).unwrap();
    assert!(kept.exists() && !unused.exists());
    assert!(stamp(&project.join("absent"), &aterm_stamp::UNSTAMPED_RECORD).is_err());
    // A source hop in the product, rather than its tests, changes the linked content too.
    let source = project.join("src/main.rs");
    let text = std::fs::read_to_string(&source)
        .unwrap()
        .replace("identity {}", "changed identity {}");
    std::fs::write(source, text).unwrap();
    let output = command(crate::gates::trust_stage2_bin().unwrap().join("targo"))
        .args(["--unverified", "build", "--offline", "--target-dir"])
        .arg(&target)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_ne!(
        normalized(&std::fs::read(target.join("debug/stamp-fixture")).unwrap()).unwrap(),
        expected
    );
}
