// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const VERSION: &str = "0.5.0";
const BUILD: u64 = 500;

fn fixture(label: &str) -> (aterm_tempfile::TempDir, CutCtx) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-artifact-proof-{label}-"))
        .tempdir()
        .unwrap();
    let root = dir.path().to_path_buf();
    let dist = root.join("dist");
    let app = bundle::staged_app_path(&dist, BUILD);
    fs::create_dir_all(app.join("Contents")).unwrap();
    fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<plist><dict><key>CFBundleShortVersionString</key><string>{VERSION}</string>\
             <key>CFBundleVersion</key><string>{BUILD}</string></dict></plist>"
        ),
    )
    .unwrap();
    let commit = "a".repeat(40);
    fs::write(
        dist.join(format!("aterm-{VERSION}-build.txt")),
        format!(
            "version={VERSION}\nbuild={BUILD}\ncommit={}\n",
            &commit[..12]
        ),
    )
    .unwrap();
    let dmg = dist.join(channel::dmg_asset_name(VERSION));
    let zip = dist.join(channel::zip_asset_name(VERSION));
    fs::write(&dmg, b"the lean dmg bytes").unwrap();
    fs::write(&zip, b"the updater zip bytes").unwrap();
    let dmg_sha = dmg::sha256_file(&dmg).unwrap();
    let zip_sha = dmg::sha256_file(&zip).unwrap();
    stage_manifest(
        &dist,
        &manifest_out::ManifestInputs {
            version: VERSION,
            build_number: BUILD,
            commit: &commit,
            dmg_name: &channel::dmg_asset_name(VERSION),
            dmg_sha256: &dmg_sha,
            zip_name: &channel::zip_asset_name(VERSION),
            zip_sha256: &zip_sha,
            repo_slug: "channel/repo",
            min_os: "11.0",
            team_id: "",
            pub_date: "2026-09-23T00:00:00Z",
            min_build: None,
            changelog: "- a change",
        },
        None,
        None,
        None,
    )
    .unwrap();
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::write(app.join("Contents/MacOS/aterm"), b"test executable").unwrap();
    crate::service_qualification::fixture(
        &app,
        BUILD,
        &commit,
        &dist.join(manifest_out::MANIFEST_ASSET),
    );
    let ctx = CutCtx {
        credentials: None,
        apple: sign::AppleTier::Inactive,
        repo: root.clone(),
        tree: root,
        dist: dist.clone(),
        journal_path: dist.join("cut-state.toml"),
        slug: "owner/repo".to_string(),
        version: VERSION.to_string(),
        tag: format!("v{VERSION}"),
        build: BUILD,
        commit,
        min_build: None,
        artifact: Some("aa".repeat(32)),
        macho_sha256: Some("bb".repeat(32)),
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        verify_pubkey: None,
        signature_machine_id: None,
        attribution: None,
        roster: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        kind: CutKind::DryRun,
        lease: None,
        fence: None,
        journal: None,
        adopted: false,
        linux: None,
        windows: None,
    };
    (dir, ctx)
}

#[test]
fn the_publication_steps_reprove_bytes_and_run_nothing() {
    let (_fixture, mut ctx) = fixture("bytes");
    let manifest = prove_artifacts_on_disk(&ctx).expect("the bytes are the manifest's");
    assert_eq!(manifest.build_number, BUILD);
    assert!(ctx.stable_dmg_path().is_file() && ctx.dmg_sha256_path().is_file());

    let error = step_selfcheck(&mut ctx)
        .expect_err("the behavioural pass must run the binary")
        .to_string();
    assert!(error.contains("spawn aterm --diagnose"), "{error}");
}

#[test]
fn a_byte_that_changes_after_the_selfcheck_is_refused_by_digest() {
    let (_fixture, ctx) = fixture("dmg-flip");
    prove_artifacts_on_disk(&ctx).expect("the untouched bytes pass");
    let mut dmg = fs::read(ctx.dmg_path()).unwrap();
    dmg[0] ^= 1;
    fs::write(ctx.dmg_path(), &dmg).unwrap();
    let error = prove_artifacts_on_disk(&ctx)
        .expect_err("a flipped DMG byte must be refused")
        .to_string();
    assert!(error.contains("DMG sha256"), "{error}");

    let (_fixture, ctx) = fixture("zip-flip");
    let mut zip = fs::read(ctx.zip_path()).unwrap();
    zip[0] ^= 1;
    fs::write(ctx.zip_path(), &zip).unwrap();
    let error = prove_artifacts_on_disk(&ctx)
        .expect_err("a flipped zip byte must be refused")
        .to_string();
    assert!(error.contains("zip sha256"), "{error}");
}
