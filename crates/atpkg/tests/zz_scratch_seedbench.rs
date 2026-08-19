// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! TEMPORARY adjudication probe — deleted immediately after the measurement.
//! Times the REAL client extractor at seed scale.

#[test]
#[ignore = "manual probe"]
fn probe_extract_speed() {
    let archive = std::env::var("PROBE_ARCHIVE").expect("PROBE_ARCHIVE");
    let dest = std::env::var("PROBE_DEST").expect("PROBE_DEST");
    let dest = std::path::PathBuf::from(dest);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).unwrap();
    let t0 = std::time::Instant::now();
    atpkg::extract::extract_tar_zst(
        std::path::Path::new(&archive),
        &dest,
        64 * 1024 * 1024 * 1024,
        1_000_000,
    )
    .unwrap();
    let dt = t0.elapsed();
    println!("PROBE extract_tar_zst took {:?}", dt);
    let t1 = std::time::Instant::now();
    let root = atpkg::tree::tree_root(&dest).unwrap();
    println!("PROBE tree_root took {:?} ({root})", t1.elapsed());
    println!("PROBE total {:?}", t0.elapsed());
}
