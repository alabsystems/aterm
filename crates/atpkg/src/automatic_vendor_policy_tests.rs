// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn an_automatic_vendor_pass_reads_current_policy_before_fetching() {
    use crate::vendor_direct::lane::world::{self, Fake, native_exe};

    let layout = world::layout("automatic-vendor-policy");
    let config_dir = aterm_tempfile::tempdir().unwrap();
    let config = config_dir.path().join("aterm.toml");
    let checkout = config_dir.path().join("development");
    std::fs::create_dir(&checkout).unwrap();
    std::fs::write(checkout.join("owner-file"), b"keep this checkout").unwrap();
    let fetcher = Fake::default();
    let spec = crate::vendor_direct::spec("claude").unwrap();
    let trust = world::trust();
    let mut seams = world_seams(&trust);
    seams.config = Some(config.clone());

    fetcher.publish_claude("2.1.281", &native_exe("claude 2.1.281"));
    assert!(update_vendor(
        &layout,
        &fetcher,
        spec,
        VendorDoor::Person,
        &seams
    ));
    let original = crate::active_builds(&layout);
    fetcher.publish_claude("2.1.282", &native_exe("claude 2.1.282"));
    let reads = fetcher.gets.borrow().len();
    let downloads = fetcher.downloads.borrow().len();

    for text in [
        "[packages]\nexclude = [\"claude\"]\n".to_owned(),
        "[packages]\nenabled = false\n".to_owned(),
        "[packages]\nexclude = 5\n".to_owned(),
        format!("[packages.links]\nclaude = {:?}\n", checkout),
    ] {
        std::fs::write(&config, text).unwrap();
        assert!(update_vendor(
            &layout,
            &fetcher,
            spec,
            VendorDoor::Window,
            &seams
        ));
        assert_eq!(crate::active_builds(&layout), original);
        assert_eq!(fetcher.gets.borrow().len(), reads, "no provider read");
        assert_eq!(fetcher.downloads.borrow().len(), downloads, "no transfer");
    }
    assert_eq!(
        std::fs::read(checkout.join("owner-file")).unwrap(),
        b"keep this checkout"
    );

    std::fs::remove_file(&config).unwrap();
    std::fs::create_dir(&config).unwrap();
    assert!(update_vendor(
        &layout,
        &fetcher,
        spec,
        VendorDoor::Window,
        &seams
    ));
    assert_eq!(crate::active_builds(&layout), original);
    assert_eq!(fetcher.gets.borrow().len(), reads);
    std::fs::remove_dir(&config).unwrap();

    std::fs::write(&config, "[packages]\nexclude = [\"claude\"]\n").unwrap();
    assert!(update_vendor(
        &layout,
        &fetcher,
        spec,
        VendorDoor::Person,
        &seams
    ));
    assert_ne!(
        crate::active_builds(&layout),
        original,
        "explicit requests still run"
    );

    fetcher.publish_claude("2.1.283", &native_exe("claude 2.1.283"));
    let before = crate::active_builds(&layout);
    std::fs::write(&config, "[packages]\nenabled = true\n").unwrap();
    assert!(update_vendor(
        &layout,
        &fetcher,
        spec,
        VendorDoor::Window,
        &seams
    ));
    assert_ne!(
        crate::active_builds(&layout),
        before,
        "a fresh policy read resumes updates"
    );
}
