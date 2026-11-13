// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture(run: impl FnOnce(&[PathBuf], &str, u64)) {
    let fake = agh_fake::FakeGithub::start();
    let slug = "cut-fixture/aterm";
    let token = agh_fake::token("cut-assets");
    fake.add_token(&token, "cut-reader");
    fake.add_repo(slug, agh_fake::RepoSpec::public());
    let release = fake.add_release(
        slug,
        agh_fake::ReleaseSpec::published("v0.183.0", "2026-11-08T00:00:00Z"),
    );
    let dir = aterm_tempfile::tempdir().unwrap();
    let files: Vec<_> = (0..10)
        .map(|n| {
            let name = format!("asset-{n}");
            fake.add_asset(
                release,
                agh_fake::AssetSpec::uploaded(&name, b"correct bytes"),
            );
            let file = dir.path().join(name);
            fs::write(&file, b"correct bytes").unwrap();
            file
        })
        .collect();
    let ports = fake.ports();
    let _route = crate::github::Fixture {
        origins: aterm_update_core::github_rest::Origins::loopback(
            ports.api,
            ports.uploads,
            ports.web,
            ports.cdn,
        ),
        token: Some(token),
        spawn: None,
    }
    .enter();
    run(&files, slug, release);
}

/// NEGATIVE CONTROL: a same-sized wrong byte must fail even though the
/// complete immutable-ID inventory remains identical.
#[test]
fn batched_asset_proof_still_checks_every_byte() {
    fixture(|files, slug, release| {
        let before = release_asset_inventory_for_release_id(slug, release).unwrap();
        prove_asset_files(files, &before, slug, release).unwrap();
        fs::write(&files[7], b"corrupt bytes").unwrap();
        let error = prove_asset_files(files, &before, slug, release).unwrap_err();
        assert!(error.to_string().contains("bytes differ"), "{error}");
        assert_eq!(
            before,
            release_asset_inventory_for_release_id(slug, release).unwrap()
        );
    });
}

#[test]
#[ignore = "explicit loopback before/after asset-proof measurement"]
fn cut_asset_proof_abba() {
    fixture(|files, slug, release| {
        for (take, batch) in [false, true, true, false].into_iter().enumerate() {
            let label = format!("asset proof take {take} batch={batch}");
            let _timer = crate::cut_timing::Timer::new(&label);
            let before = release_asset_inventory_for_release_id(slug, release).unwrap();
            if batch {
                prove_asset_files(files, &before, slug, release).unwrap();
            } else {
                for file in files {
                    verify_release_asset_id_matches_local(
                        slug,
                        release,
                        file.file_name().unwrap().to_str().unwrap(),
                        file,
                    )
                    .unwrap();
                }
            }
            assert_eq!(
                before,
                release_asset_inventory_for_release_id(slug, release).unwrap()
            );
        }
    });
}
