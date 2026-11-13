// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

#[test]
fn settings_section_parser_accepts_every_visible_label_and_stable_path() {
    for route in crate::native_settings::SettingsRoute::ALL {
        assert_eq!(
            parse_settings_section_route(route.label()),
            Some(route),
            "visible label {:?}",
            route.label()
        );
        assert_eq!(
            parse_settings_section_route(route.path()),
            Some(route),
            "stable path {:?}",
            route.path()
        );
    }
}

#[test]
fn settings_field_wire_reply_preserves_success_and_failure_framing() {
    assert_eq!(
        settings_field_wire_reply(Ok("saved: copy_on_select = true".to_string())),
        "OK saved: copy_on_select = true\n"
    );
    assert_eq!(
        settings_field_wire_reply(Err(
            "publication unverified for copy_on_select; reload aterm.toml".to_string()
        )),
        "ERR publication unverified for copy_on_select; reload aterm.toml\n",
        "a non-success completion must never acquire an OK prefix"
    );
}

fn native_image_metadata() -> crate::control::ImageFrameMetadata {
    crate::control::ImageFrameMetadata {
        frame_kind: "native",
        phase: "presented",
        window: 3,
        view: Some(42),
        generation: Some(7),
        config_revision: Some(11),
        update_revision: Some(13),
        document_seq: Some(17),
        presentation_revision: Some(19),
        paint_revision: Some(21),
        capture_serial: 23,
        width: 800,
        height: 600,
        pixel_fingerprint: 0x1111_2222,
        compiled_fingerprint: Some(0x1234_abcd),
        raster_fingerprint: 0x3333_4444,
        raster_model_fingerprint: 0x5555_6666,
        raster_geometry: 0x7777_8888,
        overlay_fingerprint: 0x9999_aaaa,
        theme_fingerprint: 0xbbbb_cccc,
        leaves: vec![crate::control::ImageLeafFrameMetadata {
            kind: "native",
            view: 42,
            session: None,
            focused: true,
            width: 800,
            height: 600,
            snapshot_seq: None,
            instance: Some(5),
            generation: Some(7),
            geometry: Some(9),
            config_revision: Some(11),
            update_revision: Some(13),
            document_seq: Some(17),
            presentation_revision: Some(19),
            paint_revision: Some(21),
            compiled_fingerprint: Some(0x1234_abcd),
            raster_fingerprint: Some(0x3333_4444),
        }],
    }
}

#[test]
fn image_reply_base64_appends_without_a_second_buffer() {
    let metadata = image_metadata_fields(None);
    for len in 0..=257 {
        let png = (0..len)
            .map(|index| (index as u8).wrapping_mul(37).wrapping_add(11))
            .collect::<Vec<_>>();
        let encoded = aterm_codec::base64::encode(&png).expect("bounded test input");
        for want_metadata in [false, true] {
            let reply = image_bytes_reply(7, 5, &png, want_metadata, None)
                .expect("a short PNG fits a byte reply");
            let (head, reserved) = if want_metadata {
                (
                    format!("OK 2\nimage-meta {metadata}\n"),
                    encoded.len()
                        + IMAGE_BYTE_ROW_OVERHEAD
                        + metadata.len()
                        + "image-meta \n".len(),
                )
            } else {
                (
                    "OK 1\n".to_string(),
                    encoded.len() + IMAGE_BYTE_ROW_OVERHEAD,
                )
            };
            assert_eq!(
                reply,
                format!("{head}7 5 {len} {encoded}\n"),
                "base64 parity at input length {len}",
            );
            assert_eq!(
                reply.capacity(),
                reserved,
                "the reply must not reallocate at input length {len} (meta={want_metadata})",
            );
        }
    }
}

#[test]
fn image_bytes_target_bypasses_filesystem_confinement() {
    let budgets = crate::control::ArtifactBudgets::shipping();
    let mut permit = budgets
        .try_reserve_in_memory()
        .expect("a fresh budget has room");
    let (_target, display_path) = prepare_image_target(
        true,
        false,
        "nested/paths-are-invalid-for-file-output.png",
        std::path::Path::new("missing-socket-directory"),
        &budgets,
        &mut permit,
    )
    .expect("inline output needs no filesystem target");
    assert_eq!(display_path, None);
}

#[test]
fn image_meta_is_an_opt_in_flag_and_preserves_legacy_names() {
    assert_eq!(
        parse_image_options("plain --meta --bytes shot.png"),
        ImageOptions {
            want_bytes: true,
            want_metadata: true,
            rest: "plain shot.png".to_string(),
        }
    );
    assert_eq!(
        parse_image_options("meta"),
        ImageOptions {
            want_bytes: false,
            want_metadata: false,
            rest: "meta".to_string(),
        },
        "only --meta is reserved; an existing file literally named meta still works",
    );
}

#[test]
fn image_meta_protocol_is_additive_and_keeps_legacy_replies_exact() {
    let metadata = native_image_metadata();
    assert_eq!(
        image_file_reply(800, 600, "/tmp/legacy.png", false, Some(&metadata)),
        "OK 800 600 /tmp/legacy.png\n",
    );
    let file = image_file_reply(800, 600, "/tmp/frame.png", true, Some(&metadata));
    assert!(file.starts_with("OK 800 600 image-meta image-meta-version=3 frame-kind=native "));
    for field in [
        "image-meta-version=3",
        "frame-phase=presented",
        "view=42",
        "generation=7",
        "config-revision=11",
        "update-revision=13",
        "document-seq=17",
        "presentation-revision=19",
        "capture-serial=23",
        "dimensions=800x600",
        "pixel-fingerprint=0000000011112222",
        "compiled-fingerprint=000000001234abcd",
        "leaf-count=1",
        "leaves=v42:native:s-:f1:",
        "path=\"/tmp/frame.png\"",
    ] {
        assert!(file.contains(field), "missing {field}: {file}");
    }

    let png = [0x89, b'P', b'N', b'G'];
    assert_eq!(
        image_bytes_reply(2, 3, &png, false, Some(&metadata)).unwrap(),
        "OK 1\n2 3 4 iVBORw==\n",
    );
    let bytes = image_bytes_reply(2, 3, &png, true, Some(&metadata)).unwrap();
    let lines = bytes.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "OK 2");
    assert!(lines[1].starts_with("image-meta image-meta-version=3 frame-kind=native "));
    assert!(lines[1].contains("capture-serial=23"));
    assert_eq!(lines[2], "2 3 4 iVBORw==");
    assert!(image_byte_reply_png_fits(MAX_IMAGE_BYTE_REPLY_PNG_BYTES));
    assert!(
        !image_byte_reply_png_fits(MAX_IMAGE_BYTE_REPLY_PNG_BYTES.saturating_add(1)),
        "the server never emits a row its shipping client must reject",
    );

    let unavailable = image_file_reply(4, 5, "/tmp/unknown.png", true, None);
    assert!(unavailable.contains(
        "image-meta-version=3 frame-kind=unknown frame-phase=unknown identity=unavailable"
    ));
}

#[test]
fn video_args_defaults_values_and_clamps() {
    let a = parse_video_args("").expect("defaults parse");
    assert_eq!(a.secs, 3.0);
    assert!(!a.full_res && !a.keys && !a.pace);
    assert_eq!(a.fps, None);
    assert_eq!(a.budget_bytes, aterm_gpu::video_tap::DEFAULT_BUDGET);
    let a = parse_video_args("5 full keys pace fps=10 budget=1024").expect("parses");
    assert_eq!(a.secs, 5.0);
    assert!(a.full_res && a.keys && a.pace);
    assert_eq!(a.fps, Some(10));
    assert_eq!(a.budget_bytes, 1024 << 20);
    assert_eq!(parse_video_args("0.1").unwrap().secs, 0.5);
    assert_eq!(parse_video_args("999").unwrap().secs, 60.0);
    assert_eq!(parse_video_args("fps=0").unwrap().fps, Some(1));
    assert_eq!(parse_video_args("fps=500").unwrap().fps, Some(120));
    assert_eq!(parse_video_args("budget=8").unwrap().budget_bytes, 64 << 20);
    assert_eq!(
        parse_video_args("budget=999999").unwrap().budget_bytes,
        4096 << 20
    );
}

#[test]
fn video_args_rejects_unknown_and_malformed() {
    for bad in [
        "bogus",
        "fps=abc",
        "fps=",
        "budget=abc",
        "budget=",
        "fps=-1",
    ] {
        let err = parse_video_args(bad).expect_err(bad);
        assert!(err.starts_with("ERR video:"), "{bad}: {err}");
    }
}

#[test]
fn capture_timeout_reports_err_only_when_cancellation_wins() {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = crate::control::CaptureCancellation::new();
    let mut cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel.clone()));
    let cancelled =
        recv_capture_reply::<(u32, u32)>(&rx, &mut cancel_on_drop, Duration::ZERO, "image")
            .expect_err("live request is cancelled at timeout");
    assert!(cancelled.contains("publication cancelled"));
    drop(tx);

    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = crate::control::CaptureCancellation::new();
    assert!(cancel.authorize_commit(), "worker wins before timeout");
    let cancel_on_drop = CancelCaptureRequestOnDrop(Some(cancel));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let mut cancel_on_drop = cancel_on_drop;
        done_tx
            .send(recv_capture_reply(
                &rx,
                &mut cancel_on_drop,
                Duration::ZERO,
                "image",
            ))
            .unwrap();
    });
    tx.send(Ok(crate::control::Retained::plain((7_u32, 9_u32))))
        .unwrap();
    let reply = done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("committed result completes the pending waiter")
        .expect("commit winner is awaited instead of receiving a false timeout ERR");
    assert_eq!(reply.value, (7, 9));
    waiter.join().unwrap();
}

#[test]
fn video_timeout_awaits_commit_winner_instead_of_returning_err() {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = crate::VideoCancellation::new();
    let mut cancel_on_drop = CancelVideoRequestOnDrop(Some(cancel.clone()));
    let cancelled = recv_video_reply(&rx, &mut cancel_on_drop, Duration::ZERO)
        .expect_err("live recording is cancelled at timeout");
    assert!(cancelled.contains("publication cancelled"));
    drop(tx);

    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = crate::VideoCancellation::new();
    assert!(cancel.authorize_commit(), "encoder wins before timeout");
    let cancel_on_drop = CancelVideoRequestOnDrop(Some(cancel));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let mut cancel_on_drop = cancel_on_drop;
        done_tx
            .send(recv_video_reply(&rx, &mut cancel_on_drop, Duration::ZERO))
            .unwrap();
    });
    tx.send(crate::control::Retained::plain("OK video\n".to_string()))
        .unwrap();
    let reply = done_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("committed video result completes the pending waiter")
        .expect("commit winner is awaited instead of receiving a false timeout ERR");
    assert_eq!(reply.value, "OK video\n");
    waiter.join().unwrap();
}

fn video_test_dir(tag: &str) -> std::path::PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "aterm-vf-{tag}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

fn instance_root(sock_dir: &std::path::Path, instance: &str) -> std::path::PathBuf {
    sock_dir.join(super::control_auth::VIDEO_DIR).join(instance)
}

fn write_fake_recording(
    sock_dir: &std::path::Path,
    instance: &str,
    rec: &str,
    frames: &[(u64, u64)],
) {
    let dir = instance_root(sock_dir, instance).join(rec);
    std::fs::create_dir_all(&dir).unwrap();
    let mut lines = String::new();
    for (i, (delta, t_us)) in frames.iter().enumerate() {
        if i > 0 {
            lines.push_str(",\n");
        }
        let n = i + 1;
        std::fs::write(dir.join(format!("frame_{n:04}.png")), b"png").unwrap();
        lines.push_str(&format!(
            "    {{\"n\":{n},\"seq\":{n},\"t_us\":{t_us},\"fp\":0,\"delta\":{delta},\"file\":\"frame_{n:04}.png\"}}"
        ));
    }
    std::fs::write(
        dir.join("index.json"),
        format!("{{\"frames\":[\n{lines}\n]}}"),
    )
    .unwrap();
    std::fs::write(
        dir.join(super::control_auth::VIDEO_PUBLISHED_FILE),
        b"aterm-video-published-v1\n",
    )
    .unwrap();
}

#[test]
fn video_frame_row_formatter_preserves_wire_shape() {
    let temp = aterm_tempfile::tempdir().expect("temporary frame directory");
    let recording =
        crate::pinned_dir::PinnedDir::open_resolved(temp.path()).expect("pinned frame directory");
    let name = std::ffi::OsStr::new("frame_0001.png");
    let identity = recording
        .write_new_private(name, b"png")
        .expect("private frame")
        .into_identity()
        .expect("frame identity");
    let path = recording.path().join(name);
    let rows = [PinnedVideoFrame {
        candidate: VideoFrameCandidate {
            delta: u64::MAX,
            n: u64::MAX,
            seq: u64::MAX,
            t_us: u64::MAX,
            file: "frame_0001.png".to_string(),
        },
        path: path.clone(),
        identity,
    }];

    let output = format_video_frame_rows(&rows);
    assert_eq!(
        output,
        format!(
            "OK 1\nframe n={} delta={} t_us={} seq={} {}\n",
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            path.display()
        )
    );
}

#[test]
fn video_frames_ranks_by_delta_picks_newest_and_isolates_instance() {
    const INSTANCE: &str = "p101-current";
    const SIBLING: &str = "p202-sibling";
    let tmp = video_test_dir("rank");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(5, 10), (1, 20)]);
    std::fs::create_dir_all(instance_root(&tmp, INSTANCE).join("rec-1000000002-000")).unwrap();
    write_fake_recording(
        &tmp,
        INSTANCE,
        "rec-1000000001-000",
        &[(3, 0), (40, 16), (7, 33), (2, 50)],
    );
    write_fake_recording(&tmp, SIBLING, "rec-9999999999-000", &[(999, 99)]);

    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        "count=2".split_whitespace(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(out.starts_with("OK 2\n"), "framed header: {out:?}");
    let body: Vec<&str> = out.lines().skip(1).collect();
    assert!(
        body[0].contains("delta=40") && body[0].contains("n=2"),
        "top: {out:?}"
    );
    assert!(
        body[1].contains("delta=7") && body[1].contains("n=3"),
        "second: {out:?}"
    );
    assert!(
        !out.contains("delta=5") && !out.contains("delta=999"),
        "ignored the older recording and live sibling: {out:?}"
    );
    assert!(!out.contains(SIBLING), "no sibling path disclosed: {out:?}");

    let all = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(all.starts_with("OK 4\n"), "all frames: {all:?}");

    assert!(
        video_frames_for_instance(
            &tmp,
            INSTANCE,
            "count=0".split_whitespace(),
            &crate::control::ArtifactBudgets::shipping()
        )
        .starts_with("ERR ")
    );
    assert!(
        video_frames_for_instance(
            &tmp,
            INSTANCE,
            "bogus".split_whitespace(),
            &crate::control::ArtifactBudgets::shipping()
        )
        .starts_with("ERR ")
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn video_frames_backfills_past_missing_and_symlinked_high_ranks() {
    use std::os::unix::fs::symlink;

    const INSTANCE: &str = "p707-backfill";
    const RECORDING: &str = "rec-1000000000-000";
    let tmp = video_test_dir("backfill");
    let _ = std::fs::remove_dir_all(&tmp);
    write_fake_recording(
        &tmp,
        INSTANCE,
        RECORDING,
        &[(100, 10), (90, 20), (80, 30), (70, 40)],
    );
    let recording = instance_root(&tmp, INSTANCE).join(RECORDING);
    let missing = recording.join("frame_0001.png");
    std::fs::remove_file(&missing).unwrap();
    let linked = recording.join("frame_0002.png");
    std::fs::remove_file(&linked).unwrap();
    symlink("frame_0003.png", &linked).unwrap();
    assert!(
        !missing.exists(),
        "the highest-ranked frame is genuinely absent"
    );
    assert!(
        std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the second rank is genuinely an untrusted symlink"
    );

    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        "count=2".split_whitespace(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(out.starts_with("OK 2\n"), "backfill result: {out:?}");
    let body = out.lines().skip(1).collect::<Vec<_>>();
    assert_eq!(body.len(), 2, "backfill rows: {out:?}");
    assert!(
        body[0].contains("delta=80") && body[0].contains("n=3"),
        "first usable rank: {out:?}"
    );
    assert!(
        body[1].contains("delta=70") && body[1].contains("n=4"),
        "second usable rank: {out:?}"
    );
    assert!(!out.contains("delta=100") && !out.contains("delta=90"));

    drop(body);
    drop(out);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn video_frame_reply_handles_stay_bounded() {
    if !super::control_auth::enter_low_nofile_test_child(128) {
        return;
    }

    const INSTANCE: &str = "p606-bounded";
    let temp = aterm_tempfile::tempdir().expect("temporary video root");
    let frames = (0..64).map(|n| (n, n)).collect::<Vec<_>>();
    write_fake_recording(temp.path(), INSTANCE, "rec-1000000000-000", &frames);
    let socket_depth = std::fs::canonicalize(temp.path())
        .unwrap()
        .components()
        .count();
    let units_per_reply = socket_depth
        .saturating_add(2)
        .saturating_add(crate::pinned_dir::PINNED_DIR_OPERATION_DESCRIPTOR_UNITS);
    let expected = super::super::ARTIFACT_HANDOFF_LIMIT
        .min(super::super::ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT / units_per_reply);
    assert!(expected > 0, "the fixture path must fit one admitted reply");

    let budgets = crate::control::ArtifactBudgets::shipping();
    let mut replies = Vec::new();
    while replies.len() < super::super::ARTIFACT_HANDOFF_LIMIT {
        let mut retained =
            match super::confined_video_reader::read(temp.path(), INSTANCE, 64, &budgets) {
                Ok(Some(retained)) => retained,
                Err(error) if error == super::super::ARTIFACT_HANDOFF_BUSY => break,
                result => panic!("unexpected confined read result: {result:?}"),
            };
        assert!(retained.value.starts_with("OK 64\n"), "{}", retained.value);
        retained
            .retention
            .as_mut()
            .expect("frame reply owns publication retention")
            .prepare_write()
            .expect("closed frame identities revalidate at the wire edge");
        replies.push(retained);
    }
    assert_eq!(
        replies.len(),
        expected,
        "admission must spend the exact canonical-depth charge"
    );
    assert!(
        matches!(
            super::confined_video_reader::read(temp.path(), INSTANCE, 64, &budgets),
            Err(error) if error == super::super::ARTIFACT_HANDOFF_BUSY
        ),
        "one more read must fail before doing descriptor-heavy work"
    );
}

#[cfg(unix)]
#[test]
fn video_frames_revalidates_selected_handles_at_wire_edge() {
    const INSTANCE: &str = "p303-wire";
    let tmp = video_test_dir("wire-revalidate");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(9, 10)]);

    let mut reply = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(reply.starts_with("OK 1\n"));
    let recording = instance_root(&tmp, INSTANCE).join("rec-1000000000-000");
    let frame = recording.join("frame_0001.png");
    std::fs::rename(&frame, recording.join("frame_moved.png")).unwrap();
    std::fs::write(&frame, b"replacement").unwrap();

    let error = reply
        .prepare_retention_for_test()
        .expect_err("a replacement after formatting cannot receive the queued OK");
    assert!(error.contains("video frame identity changed"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn video_frames_wire_guard_survives_later_recording_retention() {
    let tmp = video_test_dir("wire-retention");
    let _ = std::fs::remove_dir_all(&tmp);
    super::control_auth::ensure_private_dir(&tmp).unwrap();

    let mut oldest = super::control_auth::confine_video_dir(&tmp).unwrap();
    let oldest_path = oldest.path().to_path_buf();
    let frame = oldest
        .write_sealed_frame(std::ffi::OsStr::new("frame_0001.png"), b"png")
        .unwrap();
    let index = oldest
        .write_new_private(
            std::ffi::OsStr::new("index.json"),
            br#"{"frames":[{"n":1,"seq":1,"t_us":10,"fp":0,"delta":9,"file":"frame_0001.png"}]}"#,
        )
        .unwrap();
    oldest
        .publish(std::slice::from_ref(&frame), &index)
        .unwrap();
    drop(oldest.publish_marker().unwrap());
    drop(index);
    drop(frame);
    drop(oldest);
    super::control_auth::wait_for_artifact_cleanup_for_test(&oldest_path);

    let mut reply = video_frames_for_instance(
        &tmp,
        super::control_auth::process_instance_id(),
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(reply.starts_with("OK 1\n"), "{reply:?}");
    assert!(reply.contains(oldest_path.to_string_lossy().as_ref()));

    for _ in 0..11 {
        let mut later = super::control_auth::confine_video_dir(&tmp).unwrap();
        let later_index = later
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        later.publish(&[], &later_index).unwrap();
        drop(later.publish_marker().unwrap());
        drop(later_index);
        drop(later);
    }

    assert!(
        oldest_path.join("index.json").is_file(),
        "sibling retention cannot quarantine a recording named by a queued frames reply"
    );
    reply
        .prepare_retention_for_test()
        .expect("the queued frame paths still validate at the wire edge");
    drop(reply);
    super::control_auth::wait_for_artifact_cleanup_for_test(&oldest_path);

    let mut next = super::control_auth::confine_video_dir(&tmp).unwrap();
    let next_path = next.path().to_path_buf();
    let next_index = next
        .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
        .unwrap();
    next.publish(&[], &next_index).unwrap();
    drop(next.publish_marker().unwrap());
    drop(next_index);
    drop(next);
    super::control_auth::wait_for_artifact_cleanup_for_test(&next_path);
    assert!(
        !oldest_path.exists(),
        "after the wire guard drops, the oldest recording becomes eligible"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn video_frames_release_sweep_stays_on_pinned_root_after_replacement() {
    const INSTANCE: &str = "p304-pinned-release";
    const RECORDINGS: usize = 12;
    let tmp = video_test_dir("pinned-release");
    let _ = std::fs::remove_dir_all(&tmp);
    for sequence in 0..RECORDINGS {
        write_fake_recording(
            &tmp,
            INSTANCE,
            &format!("rec-{sequence:020}-000"),
            &[(sequence as u64 + 1, 10)],
        );
    }

    let mut reply = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(reply.starts_with("OK 1\n"), "{reply:?}");
    reply
        .prepare_retention_for_test()
        .expect("reader identities validate before namespace replacement");

    let original = instance_root(&tmp, INSTANCE);
    let selected_key = original.join(format!("rec-{:020}-000", RECORDINGS - 1));
    let moved = original.with_file_name(format!("{INSTANCE}-original"));
    std::fs::rename(&original, &moved).unwrap();
    for sequence in 100..(100 + RECORDINGS) {
        write_fake_recording(
            &tmp,
            INSTANCE,
            &format!("rec-{sequence:020}-000"),
            &[(999, 99)],
        );
    }
    let replacement = instance_root(&tmp, INSTANCE);
    std::fs::write(replacement.join("sentinel"), b"replacement").unwrap();

    drop(reply);
    super::control_auth::wait_for_artifact_cleanup_for_test(&selected_key);

    let recording_count = |root: &std::path::Path| {
        std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("rec-"))
            .count()
    };
    assert!(
        recording_count(&moved) < RECORDINGS,
        "last-reader retention makes progress in the exact namespace that was read"
    );
    assert_eq!(
        recording_count(&replacement),
        RECORDINGS,
        "the replacement installed at the old lexical path is never swept"
    );
    assert_eq!(
        std::fs::read(replacement.join("sentinel")).unwrap(),
        b"replacement"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn failed_video_frames_read_before_arm_does_not_run_retention() {
    use super::confined_video_reader::{ReadStage, read_for_test};

    const INSTANCE: &str = "p305-failed-prearm";
    const RECORDINGS: usize = 12;
    let tmp = video_test_dir("failed-prearm");
    let _ = std::fs::remove_dir_all(&tmp);
    for sequence in 0..RECORDINGS {
        write_fake_recording(
            &tmp,
            INSTANCE,
            &format!("rec-{sequence:020}-000"),
            &[(sequence as u64 + 1, 10)],
        );
    }

    let newest = instance_root(&tmp, INSTANCE).join(format!("rec-{:020}-000", RECORDINGS - 1));
    let frame = newest.join("frame_0001.png");
    let mut replaced = false;
    let result = read_for_test(&tmp, INSTANCE, VIDEO_FRAMES_DEFAULT, |stage| {
        if replaced || stage != ReadStage::FramePinned {
            return;
        }
        replaced = true;
        std::fs::rename(&frame, newest.join("frame.original")).unwrap();
        std::fs::write(&frame, b"replacement").unwrap();
    });

    assert!(replaced, "the post-lease, pre-arm frame hook was reached");
    match result {
        Err(error) => assert_eq!(error, "recording namespace changed during read"),
        Ok(_) => panic!("the replaced frame identity must fail the reader"),
    }
    let remaining = std::fs::read_dir(instance_root(&tmp, INSTANCE))
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("rec-"))
        .count();
    assert_eq!(
        remaining, RECORDINGS,
        "dropping an unarmed failed-read lease performs no retention sweep"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn video_frames_arm_revalidates_the_selected_recording_identity() {
    use super::confined_video_reader::{ReadStage, read_for_test};

    const INSTANCE: &str = "p306-arm-identity";
    const RECORDINGS: usize = 12;
    let tmp = video_test_dir("arm-identity");
    let _ = std::fs::remove_dir_all(&tmp);
    for sequence in 0..RECORDINGS {
        write_fake_recording(
            &tmp,
            INSTANCE,
            &format!("rec-{sequence:020}-000"),
            &[(sequence as u64 + 1, 10)],
        );
    }

    let root = instance_root(&tmp, INSTANCE);
    let newest_name = format!("rec-{:020}-000", RECORDINGS - 1);
    let newest = root.join(&newest_name);
    let original = root.join(".selected-original");
    let oldest = root.join("rec-00000000000000000000-000");
    let mut replaced = false;
    let result = read_for_test(&tmp, INSTANCE, VIDEO_FRAMES_DEFAULT, |stage| {
        if replaced || stage != ReadStage::SelectionValidated {
            return;
        }
        replaced = true;
        std::fs::rename(&newest, &original).unwrap();
        write_fake_recording(&tmp, INSTANCE, &newest_name, &[(999, 99)]);
        std::fs::write(newest.join("sentinel"), b"replacement").unwrap();
    });

    assert!(replaced, "the final pre-Arm hook was reached");
    assert_eq!(
        result,
        Err("recording namespace changed while arming retention"),
        "Arm itself rejects a recording replaced after selection validation"
    );
    assert!(
        oldest.is_dir(),
        "a rejected Arm cannot request retention on final lease drop"
    );
    assert_eq!(
        std::fs::read(newest.join("sentinel")).unwrap(),
        b"replacement",
        "the replacement at the old lexical path is untouched"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn video_frames_errors_when_no_recording() {
    const INSTANCE: &str = "p303-empty";
    let tmp = video_test_dir("empty");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(instance_root(&tmp, INSTANCE)).unwrap();
    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(out.starts_with("ERR video frames:"), "{out:?}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn video_frames_before_the_first_recording_says_there_is_none() {
    const INSTANCE: &str = "p305-fresh";
    const NONE_YET: &str =
        "ERR video frames: no finished recording found (run `video <seconds>` first)\n";
    let tmp = video_test_dir("fresh");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert_eq!(out, NONE_YET, "no `video/` yet");

    std::fs::create_dir_all(instance_root(&tmp, "p306-sibling")).unwrap();
    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert_eq!(
        out, NONE_YET,
        "`video/` exists, this launch's namespace does not"
    );

    let not_a_dir = video_test_dir("fresh-file");
    let _ = std::fs::remove_dir_all(&not_a_dir);
    std::fs::create_dir_all(&not_a_dir).unwrap();
    std::fs::write(not_a_dir.join(super::control_auth::VIDEO_DIR), b"not a dir").unwrap();
    let out = video_frames_for_instance(
        &not_a_dir,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(
        out.contains("recording namespace is not confined"),
        "a `video` that is not a directory is refused: {out:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&not_a_dir);
}

#[test]
fn video_frames_caps_index_and_falls_back_to_valid_prior_recording() {
    const INSTANCE: &str = "p404-capped";
    let tmp = video_test_dir("cap");
    let _ = std::fs::remove_dir_all(&tmp);
    write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(17, 10)]);
    let oversized = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
    std::fs::create_dir_all(&oversized).unwrap();
    std::fs::write(
        oversized.join("index.json"),
        vec![b' '; VIDEO_INDEX_MAX_BYTES + 1],
    )
    .unwrap();
    std::fs::write(
        oversized.join(super::control_auth::VIDEO_PUBLISHED_FILE),
        b"published",
    )
    .unwrap();

    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(
        out.starts_with("OK 1\n") && out.contains("delta=17"),
        "oversized newest index is skipped for the valid prior recording: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn video_frames_skips_newest_index_until_publication_marker_exists() {
    const INSTANCE: &str = "p405-marker";
    let tmp = video_test_dir("marker");
    let _ = std::fs::remove_dir_all(&tmp);
    write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(17, 10)]);

    let queued = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
    std::fs::create_dir_all(&queued).unwrap();
    std::fs::write(queued.join("frame_0001.png"), b"queued").unwrap();
    std::fs::write(
        queued.join("index.json"),
        br#"{"frames":[{"n":1,"seq":1,"t_us":20,"fp":0,"delta":999,"file":"frame_0001.png"}]}"#,
    )
    .unwrap();
    assert!(
        !queued
            .join(super::control_auth::VIDEO_PUBLISHED_FILE)
            .exists(),
        "fixture is a fully encoded reply still queued before wire publication"
    );

    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(
        out.starts_with("OK 1\n") && out.contains("delta=17"),
        "the reader falls back to the newest marker-visible recording: {out:?}"
    );
    assert!(
        !out.contains("delta=999") && !out.contains(queued.to_string_lossy().as_ref()),
        "an index alone never makes a queued recording discoverable: {out:?}"
    );

    drop(out);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn video_frames_bounds_recording_namespace_scan() {
    const INSTANCE: &str = "p505-bounded";
    let tmp = video_test_dir("scan");
    let root = instance_root(&tmp, INSTANCE);
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..=VIDEO_RECORDING_SCAN_MAX {
        std::fs::create_dir(root.join(format!("junk-{index:04}"))).unwrap();
    }

    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(
        out.contains("recording namespace has too many entries"),
        "bounded scan fails closed: {out:?}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn video_frames_rejects_traversal_and_non_frame_manifest_names() {
    const INSTANCE: &str = "p606-names";
    let tmp = video_test_dir("names");
    let rec = instance_root(&tmp, INSTANCE).join("rec-1000000000-000");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&rec).unwrap();
    std::fs::write(rec.join("frame_0001.png"), b"png").unwrap();
    std::fs::write(
        rec.join("index.json"),
        br#"{"frames":[
                {"n":1,"seq":1,"t_us":1,"delta":10,"file":"frame_0001.png"},
                {"n":2,"seq":2,"t_us":2,"delta":999,"file":"../../escape.png"},
                {"n":3,"seq":3,"t_us":3,"delta":998,"file":"/absolute.png"},
                {"n":4,"seq":4,"t_us":4,"delta":997,"file":"notes.txt"}
            ]}"#,
    )
    .unwrap();
    std::fs::write(
        rec.join(super::control_auth::VIDEO_PUBLISHED_FILE),
        b"published",
    )
    .unwrap();

    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(out.starts_with("OK 1\n"), "only one safe row: {out:?}");
    assert!(
        out.contains("frame_0001.png"),
        "safe frame retained: {out:?}"
    );
    for forbidden in ["escape.png", "absolute.png", "notes.txt"] {
        assert!(!out.contains(forbidden), "{forbidden} rejected: {out:?}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[test]
fn video_frames_rejects_linked_roots_recordings_indexes_and_frames() {
    use std::os::unix::fs::symlink;

    const INSTANCE: &str = "p707-links";

    let linked_root = video_test_dir("linked-root");
    let outside_root = video_test_dir("outside-root");
    let _ = std::fs::remove_dir_all(&linked_root);
    let _ = std::fs::remove_dir_all(&outside_root);
    std::fs::create_dir_all(outside_root.join(INSTANCE)).unwrap();
    std::fs::create_dir_all(&linked_root).unwrap();
    symlink(
        &outside_root,
        linked_root.join(super::control_auth::VIDEO_DIR),
    )
    .unwrap();
    let out = video_frames_for_instance(
        &linked_root,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(
        out.contains("recording namespace is not confined"),
        "linked root refused: {out:?}"
    );

    let tmp = video_test_dir("linked-entries");
    let outside = video_test_dir("linked-outside");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&outside);
    write_fake_recording(&tmp, INSTANCE, "rec-1000000000-000", &[(7, 10)]);
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("index.json"),
        br#"{"frames":[{"n":1,"seq":1,"t_us":1,"delta":999,"file":"frame_0001.png"}]}"#,
    )
    .unwrap();
    std::fs::write(
        outside.join(super::control_auth::VIDEO_PUBLISHED_FILE),
        b"published",
    )
    .unwrap();
    symlink(
        &outside,
        instance_root(&tmp, INSTANCE).join("rec-1000000002-000"),
    )
    .unwrap();
    let linked_index = instance_root(&tmp, INSTANCE).join("rec-1000000001-000");
    std::fs::create_dir(&linked_index).unwrap();
    symlink(outside.join("index.json"), linked_index.join("index.json")).unwrap();
    std::fs::write(
        linked_index.join(super::control_auth::VIDEO_PUBLISHED_FILE),
        b"published",
    )
    .unwrap();
    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert!(
        out.starts_with("OK 1\n") && out.contains("delta=7"),
        "linked recording and index fall back safely: {out:?}"
    );

    let linked_frame = instance_root(&tmp, INSTANCE).join("rec-1000000003-000");
    std::fs::create_dir(&linked_frame).unwrap();
    std::fs::write(outside.join("frame.png"), b"png").unwrap();
    symlink(
        outside.join("frame.png"),
        linked_frame.join("frame_0001.png"),
    )
    .unwrap();
    std::fs::write(
        linked_frame.join("index.json"),
        br#"{"frames":[{"n":1,"seq":1,"t_us":1,"delta":500,"file":"frame_0001.png"}]}"#,
    )
    .unwrap();
    std::fs::write(
        linked_frame.join(super::control_auth::VIDEO_PUBLISHED_FILE),
        b"published",
    )
    .unwrap();
    let out = video_frames_for_instance(
        &tmp,
        INSTANCE,
        std::iter::empty(),
        &crate::control::ArtifactBudgets::shipping(),
    );
    assert_eq!(
        out, "OK 0\n",
        "a linked frame is never returned even from a valid completion index"
    );

    for dir in [linked_root, outside_root, tmp, outside] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(unix)]
#[test]
fn video_frames_handle_anchors_fail_closed_on_concurrent_swaps() {
    use std::os::unix::fs::symlink;

    use super::confined_video_reader::{ReadStage, read_for_test};

    const INSTANCE: &str = "p808-race";
    const RECORDING: &str = "rec-1000000000-000";

    for stage in [
        ReadStage::NamespacePinned,
        ReadStage::RecordingPinned,
        ReadStage::IndexPinned,
        ReadStage::FramePinned,
    ] {
        let tag = match stage {
            ReadStage::SocketPathResolved => unreachable!("covered by the ancestor test"),
            ReadStage::NamespacePinned => "namespace",
            ReadStage::RecordingPinned => "recording",
            ReadStage::IndexPinned => "index",
            ReadStage::FramePinned => "frame",
            ReadStage::SelectionValidated => {
                unreachable!("covered by the final Arm race test")
            }
        };
        let tmp = video_test_dir(&format!("race-{tag}"));
        let moved = video_test_dir(&format!("race-{tag}-moved"));
        let outside = video_test_dir(&format!("race-{tag}-outside"));
        for dir in [&tmp, &moved, &outside] {
            let _ = std::fs::remove_dir_all(dir);
            let _ = std::fs::remove_file(dir);
        }
        write_fake_recording(&tmp, INSTANCE, RECORDING, &[(17, 10)]);
        write_fake_recording(&outside, INSTANCE, RECORDING, &[(999, 99)]);

        let recording = instance_root(&tmp, INSTANCE).join(RECORDING);
        let outside_recording = instance_root(&outside, INSTANCE).join(RECORDING);
        let mut swapped = false;
        let result = read_for_test(&tmp, INSTANCE, VIDEO_FRAMES_DEFAULT, |seen| {
            if swapped || seen != stage {
                return;
            }
            swapped = true;
            match stage {
                ReadStage::SocketPathResolved => unreachable!("covered by the ancestor test"),
                ReadStage::NamespacePinned => {
                    std::fs::rename(&tmp, &moved).unwrap();
                    symlink(&outside, &tmp).unwrap();
                }
                ReadStage::RecordingPinned => {
                    std::fs::rename(&recording, recording.with_extension("original")).unwrap();
                    symlink(&outside_recording, &recording).unwrap();
                }
                ReadStage::IndexPinned => {
                    let index = recording.join("index.json");
                    std::fs::rename(&index, recording.join("index.original")).unwrap();
                    symlink(outside_recording.join("index.json"), index).unwrap();
                }
                ReadStage::FramePinned => {
                    let frame = recording.join("frame_0001.png");
                    std::fs::rename(&frame, recording.join("frame.original")).unwrap();
                    symlink(outside_recording.join("frame_0001.png"), frame).unwrap();
                }
                ReadStage::SelectionValidated => {
                    unreachable!("covered by the final Arm race test")
                }
            }
        });

        assert!(swapped, "{stage:?} hook was reached");
        assert_eq!(
            result,
            Err("recording namespace changed during read"),
            "{stage:?} replacement is never emitted"
        );
        let debug = format!("{result:?}");
        assert!(
            !debug.contains("999") && !debug.contains(&outside.display().to_string()),
            "{stage:?} never discloses attacker data/path: {debug}"
        );

        let _ = std::fs::remove_file(&tmp);
        for dir in [tmp, moved, outside] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(unix)]
#[test]
fn video_frames_rejects_intermediate_ancestor_swap_after_resolution() {
    use std::os::unix::fs::symlink;

    use super::confined_video_reader::{ReadStage, read_for_test};

    const INSTANCE: &str = "p909-ancestor";
    const RECORDING: &str = "rec-1000000000-000";

    let outer = video_test_dir("ancestor-race");
    let outside = video_test_dir("ancestor-race-outside");
    let ancestor = outer.join("controlled");
    let moved = outer.join("controlled-original");
    let socket = ancestor.join("socket");
    let outside_socket = outside.join("socket");
    for dir in [&outer, &outside] {
        let _ = std::fs::remove_dir_all(dir);
    }
    write_fake_recording(&socket, INSTANCE, RECORDING, &[(17, 10)]);
    write_fake_recording(&outside_socket, INSTANCE, RECORDING, &[(999, 99)]);

    let mut swapped = false;
    let result = read_for_test(&socket, INSTANCE, VIDEO_FRAMES_DEFAULT, |seen| {
        if swapped || seen != ReadStage::SocketPathResolved {
            return;
        }
        swapped = true;
        std::fs::rename(&ancestor, &moved).unwrap();
        symlink(&outside, &ancestor).unwrap();
    });

    assert!(swapped, "post-canonicalization hook was reached");
    assert_eq!(
        result,
        Err("recording namespace is not confined"),
        "a swapped intermediate ancestor is rejected"
    );
    assert!(
        !format!("{result:?}").contains("999"),
        "attacker index was not read"
    );

    let _ = std::fs::remove_file(&ancestor);
    for dir in [outer, outside] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(unix)]
#[test]
fn video_frames_rejects_socket_directory_replacement_after_pin() {
    use super::confined_video_reader::{ReadStage, read_for_test};

    const INSTANCE: &str = "p910-socket-replacement";
    const RECORDING: &str = "rec-1000000000-000";

    let socket = video_test_dir("socket-replacement");
    let moved = video_test_dir("socket-replacement-original");
    let attacker = video_test_dir("socket-replacement-attacker");
    for dir in [&socket, &moved, &attacker] {
        let _ = std::fs::remove_dir_all(dir);
    }
    write_fake_recording(&socket, INSTANCE, RECORDING, &[(17, 10)]);
    write_fake_recording(&attacker, INSTANCE, RECORDING, &[(999, 99)]);

    let mut swapped = false;
    let result = read_for_test(&socket, INSTANCE, VIDEO_FRAMES_DEFAULT, |stage| {
        if swapped || stage != ReadStage::SocketPathResolved {
            return;
        }
        swapped = true;
        std::fs::rename(&socket, &moved).unwrap();
        std::fs::rename(&attacker, &socket).unwrap();
    });

    assert!(swapped, "post-pin hook was reached");
    assert_eq!(result, Err("recording namespace is not confined"));
    assert!(
        !format!("{result:?}").contains("999"),
        "the replacement recording was not inspected"
    );

    for dir in [socket, moved, attacker] {
        let _ = std::fs::remove_dir_all(dir);
    }
}
