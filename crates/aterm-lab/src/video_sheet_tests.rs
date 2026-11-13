// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn png(w: u32, h: u32, alpha: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = aterm_png::Encoder::new(&mut bytes, w, h);
    encoder.set_color(aterm_png::ColorType::Rgba);
    encoder.set_depth(aterm_png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&[80, 90, 100, alpha].repeat((w * h) as usize))
        .unwrap();
    writer.finish().unwrap();
    bytes
}

fn fixture() -> (aterm_tempfile::TempDir, Options) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let meta = json!({"w":4,"h":2,"device_px":[4,2],"mode":"swapchain-tap","half_res":false,
        "head_truncated":false,"resized_early_stop":false,"fps_cap":null,
        "dropped_frames":0,"ring_skipped":0,"evicted_frames":0,"decimated_frames":0,
        "unlogged_inputs":0,"unlogged_other_window":0,"unlogged_over_cap":0,
        "keys_requested":true,"frames_written":3,"inputs_logged":1,"requested_ms":1000,"covered_us":[10,25]});
    let frames: Vec<_> = [10, 20, 25]
        .into_iter()
        .enumerate()
        .map(|(i, t)| {
            let file = format!("frame_{:04}.png", i + 1);
            std::fs::write(root.join(&file), png(4, 2, 255)).unwrap();
            json!({"n":i+1,"seq":i+1,"t_us":t,"delta":if i<2{0}else{1},"file":file})
        })
        .collect();
    let index = root.join("index.json");
    std::fs::write(
        &index,
        aterm_json::to_vec(&json!({"meta":meta,"frames":frames,"inputs":[{"t_us":15,"ch":"x"}]}))
            .unwrap(),
    )
    .unwrap();
    let dims = root.join("dims.txt");
    std::fs::write(&dims, "OK 2 4 4 2 cell_w=1 cell_h=1 pad=0 pad_top=0 head=0 frame_w=4 frame_h=2 tab_rows=0 offset_x=0 offset_y=0 band_left=0 band_top=0 crop_left=0 crop_top=0").unwrap();
    let options = Options {
        index,
        dims,
        rows: (0, 1),
        cols: (0, 3),
        before_us: 25,
        out: root.join("out"),
        expected: Some("x".into()),
    };
    (dir, options)
}

#[test]
fn every_entry_including_unchanged_and_cutoff_tail_is_atlased() {
    let (_dir, options) = fixture();
    let report = run(&options).unwrap();
    assert_eq!(report["frames_decoded"].as_u64(), Some(3));
    assert_eq!(report["frames_before_cutoff"].as_u64(), Some(2));
    assert_eq!(report["frames_at_or_after_cutoff"].as_u64(), Some(1));
    assert!(report["reasons"].as_array().unwrap().is_empty());
    let rows = report["frames"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["entry"]["delta"].as_u64(), Some(0));
    assert_eq!(rows[1]["entry"]["delta"].as_u64(), Some(0));
    assert_eq!(rows[2]["before_cutoff"].as_bool(), Some(false));
    assert_eq!(rows[2]["tile"].as_u64(), Some(3));
    assert_eq!(report["visual_not_judged"].as_bool(), Some(true));
    assert_eq!(report["manifest_fp_validated"].as_bool(), Some(false));
}

#[test]
fn missing_frames_and_nonzero_or_missing_coverage_flags_are_inconclusive() {
    let (_dir, options) = fixture();
    std::fs::remove_file(options.index.parent().unwrap().join("frame_0002.png")).unwrap();
    let mut index: Value = aterm_json::from_slice(&std::fs::read(&options.index).unwrap()).unwrap();
    let meta = index
        .as_object_mut()
        .unwrap()
        .get_mut("meta")
        .unwrap()
        .as_object_mut()
        .unwrap();
    meta.insert("dropped_frames".into(), json!(1));
    meta.remove("ring_skipped");
    std::fs::write(&options.index, aterm_json::to_vec(&index).unwrap()).unwrap();
    let report = run(&options).unwrap();
    assert_eq!(report["frames_errors"].as_u64(), Some(1));
    assert_eq!(report["frames"].as_array().unwrap().len(), 3);
    assert!(report["frames"][1]["error"].as_str().is_some());
    assert!(report["reasons"].as_array().unwrap().len() >= 3);
}

#[test]
fn wrong_dimensions_and_transparency_are_refused_per_frame() {
    let (_dir, options) = fixture();
    let parent = options.index.parent().unwrap();
    std::fs::write(parent.join("frame_0001.png"), png(5, 2, 255)).unwrap();
    std::fs::write(parent.join("frame_0002.png"), png(4, 2, 128)).unwrap();
    let report = run(&options).unwrap();
    assert_eq!(report["frames_errors"].as_u64(), Some(2));
    assert_eq!(report["frames_decoded"].as_u64(), Some(1));
    assert!(report["frames"][0]["sheet"].is_null());
    assert!(report["frames"][1]["sheet"].is_null());
}

#[test]
fn missing_or_mismatched_native_dimensions_are_inconclusive() {
    for replacement in [None, Some(json!([8, 4]))] {
        let (_dir, options) = fixture();
        let mut index: Value =
            aterm_json::from_slice(&std::fs::read(&options.index).unwrap()).unwrap();
        let meta = index
            .as_object_mut()
            .unwrap()
            .get_mut("meta")
            .unwrap()
            .as_object_mut()
            .unwrap();
        if let Some(size) = replacement {
            meta.insert("device_px".into(), size);
        } else {
            meta.remove("device_px");
        }
        std::fs::write(&options.index, aterm_json::to_vec(&index).unwrap()).unwrap();
        let report = run(&options).unwrap();
        assert_eq!(report["frames_decoded"].as_u64(), Some(3));
        assert_eq!(report["coverage"].as_str(), Some("inconclusive"));
        assert!(report["reasons"].as_array().unwrap().iter().any(|reason| {
            reason
                .as_str()
                .is_some_and(|s| s.contains("meta.device_px"))
        }));
    }
}

#[test]
fn invalid_roi_and_existing_output_are_refused() {
    let (_dir, mut options) = fixture();
    options.cols = (0, 4);
    assert!(run(&options).is_err());
    assert!(!options.out.exists());
    options.cols = (0, 3);
    run(&options).unwrap();
    let before = std::fs::read(options.out.join("coverage.json")).unwrap();
    assert!(run(&options).is_err());
    assert_eq!(
        std::fs::read(options.out.join("coverage.json")).unwrap(),
        before
    );
}

#[cfg(unix)]
#[test]
fn traversal_and_symlinked_frame_names_never_import_a_frame() {
    let (_dir, options) = fixture();
    let parent = options.index.parent().unwrap();
    let mut index: Value = aterm_json::from_slice(&std::fs::read(&options.index).unwrap()).unwrap();
    let frames = index
        .as_object_mut()
        .unwrap()
        .get_mut("frames")
        .unwrap()
        .as_array_mut()
        .unwrap();
    frames[0]
        .as_object_mut()
        .unwrap()
        .insert("file".into(), json!("../outside.png"));
    std::fs::remove_file(parent.join("frame_0002.png")).unwrap();
    std::os::unix::fs::symlink(parent.join("frame_0003.png"), parent.join("frame_0002.png"))
        .unwrap();
    std::fs::write(&options.index, aterm_json::to_vec(&index).unwrap()).unwrap();
    let report = run(&options).unwrap();
    assert_eq!(report["frames_errors"].as_u64(), Some(2));
    assert_eq!(report["frames_decoded"].as_u64(), Some(1));
}

#[test]
fn expected_text_checks_only_the_input_ledger() {
    let (_dir, mut options) = fixture();
    options.expected = Some("x ".into());
    let report = run(&options).unwrap();
    assert!(!report["reasons"].as_array().unwrap().is_empty());
    assert_eq!(report["glyph_prefix_not_judged"].as_bool(), Some(true));
    assert_eq!(report["frames_decoded"].as_u64(), Some(3));
}
