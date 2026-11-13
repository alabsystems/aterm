// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Metrics, emit_gpu, text_field, wire};

#[test]
fn gpu_time_and_frame_shape_are_published_and_reset() {
    let m = Metrics::new();
    let ctx = wire::ReadCtx::inert(&m, 0);
    let text = || wire::render_text(|s| emit_gpu(&ctx, s));
    let json = || {
        aterm_json::from_str::<aterm_json::Value>(&wire::render_json(|s| emit_gpu(&ctx, s)))
            .expect("one JSON object")
    };
    let empty_source = if aterm_gpu::gpu_time::SUPPORTED {
        "unobserved"
    } else {
        "unsupported"
    };
    let empty = text();
    assert_eq!(text_field(&empty, "gpu_time_source"), Some(empty_source));
    assert_eq!(text_field(&empty, "instances_by_stream"), Some("none"));

    let mut shape = aterm_gpu::FrameShape {
        passes: 3,
        instance_bytes: 512,
        build_ns: 2_000_000,
        stage_ns: 3_000_000,
        ablation: aterm_gpu::UnormAblation::Thin,
        route: aterm_gpu::UnormRoute::Fetch,
        fetch_probe: Some(aterm_gpu::FetchProbe::Passed),
        fx_instances: 6,
        fx_upload_bytes: 1_216,
        fx_refused: 2,
        ..aterm_gpu::FrameShape::default()
    };
    let glyph = aterm_gpu::FrameShape::STREAMS
        .iter()
        .position(|&name| name == "glyph")
        .expect("a glyph stream");
    shape.instances[glyph] = 8;
    m.window.gpu.note_frame(shape, 5_000_000);
    m.window.gpu.note_time(aterm_gpu::gpu_time::GpuFrameTime {
        serial: 1,
        gpu_ns: 7_000_000,
        parts: 2,
    });
    let published = text();
    for field in [
        "n_gpu_time=1",
        "max_gpu_time_ms=7.00",
        "gpu_time_parts_last=2",
        "n_gpu_encode=1",
        "gpu_encode_last_ms=5.00",
        "passes_last=3",
        "instances_by_stream=glyph:8",
        "instance_bytes_last=512",
        "gpu_build_last_ms=2.00",
        "gpu_stage_last_ms=3.00",
        "gpu_unorm_ablation=thin",
        "gpu_unorm_route=fetch",
        "gpu_fetch_probe=passed",
        "fx_instances_last=6",
        "fx_upload_bytes_last=1216",
        "fx_refused_last=2",
    ] {
        assert!(
            published.split_whitespace().any(|token| token == field),
            "{field}: {published}"
        );
    }
    let object = json();
    assert_eq!(object["n_gpu_time"].as_u64(), Some(1));
    assert_eq!(object["instances_by_stream"]["glyph"].as_u64(), Some(8));
    assert_eq!(object["gpu_unorm_ablation"].as_str(), Some("thin"));
    assert_eq!(object["gpu_unorm_route"].as_str(), Some("fetch"));
    assert_eq!(object["gpu_fetch_probe"].as_str(), Some("passed"));
    assert_eq!(object["fx_instances_last"].as_u64(), Some(6));
    assert_eq!(object["fx_upload_bytes_last"].as_u64(), Some(1_216));
    assert_eq!(object["fx_refused_last"].as_u64(), Some(2));
    assert_eq!(
        object["gpu_time_source"].as_str(),
        Some(if aterm_gpu::gpu_time::SUPPORTED {
            "device"
        } else {
            "unsupported"
        })
    );

    m.reset();
    let object = json();
    assert_eq!(object["n_gpu_time"].as_u64(), Some(0));
    assert_eq!(object["n_gpu_encode"].as_u64(), Some(0));
    assert_eq!(object["passes_last"].as_u64(), Some(0));
    assert_eq!(
        object["instances_by_stream"]
            .as_object()
            .map(|streams| streams.len()),
        Some(0)
    );
    assert_eq!(object["gpu_unorm_ablation"].as_str(), Some("off"));
    assert_eq!(object["gpu_unorm_route"].as_str(), Some("split"));
    assert_eq!(object["gpu_fetch_probe"].as_str(), Some("pending"));
    assert_eq!(object["gpu_time_source"].as_str(), Some(empty_source));
    assert_eq!(object["fx_instances_last"].as_u64(), Some(0));
    assert_eq!(object["fx_upload_bytes_last"].as_u64(), Some(0));
    assert_eq!(object["fx_refused_last"].as_u64(), Some(0));
}

#[test]
fn fx_counters_count_since_reset() {
    use aterm_core::render::{FxContent, fx_layer_builds, fx_payload_copies};
    let m = Metrics::new();
    let process = wire::ReadCtx {
        metrics: &m,
        now_ns: 0,
        fragments: wire::Fragments::Process,
    };
    let count = |text: &str, key: &str| -> u64 {
        text_field(text, key)
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("{key} missing from {text}"))
    };
    let _sealed = FxContent::new(Vec::new(), Vec::new());
    let before = (fx_layer_builds(), fx_payload_copies());
    assert!(before.0 >= 1, "a seal is booked on aterm-core's total");
    m.reset();
    let text = wire::render_text(|s| emit_gpu(&process, s));
    assert!(
        count(&text, "fx_layer_builds") <= fx_layer_builds() - before.0,
        "a reset re-bases the seals: {text}"
    );
    assert!(
        count(&text, "fx_payload_copies") <= fx_payload_copies() - before.1,
        "a reset re-bases the copies: {text}"
    );

    let content = FxContent::new(Vec::new(), Vec::new());
    let _copy = content.clone();
    let text = wire::render_text(|s| emit_gpu(&process, s));
    let (builds, copies) = (
        count(&text, "fx_layer_builds"),
        count(&text, "fx_payload_copies"),
    );
    assert!(
        builds >= 1 && copies >= 1,
        "builds {builds}, copies {copies}"
    );
    let inert = wire::ReadCtx::inert(&m, 0);
    let held = wire::render_text(|s| emit_gpu(&inert, s));
    assert_eq!(
        (
            count(&held, "fx_layer_builds"),
            count(&held, "fx_payload_copies")
        ),
        (builds, copies),
        "an inert reply reads the ledger alone"
    );
    let object =
        aterm_json::from_str::<aterm_json::Value>(&wire::render_json(|s| emit_gpu(&process, s)))
            .expect("one JSON object");
    for (key, floor) in [("fx_layer_builds", builds), ("fx_payload_copies", copies)] {
        let read = object[key].as_u64().expect(key);
        assert!(read >= floor, "{key}: {read} < {floor}");
    }
}

#[test]
fn a_stale_fx_sample_never_lowers_the_reset_base() {
    use super::instrument::{Baselined, Windowed as _};
    let m = Metrics::new();
    let gpu = &m.window.gpu;
    let at_reset = u64::MAX >> 1;
    gpu.fx_layer_builds.raise_total(at_reset);
    gpu.fx_payload_copies.raise_total(at_reset);
    gpu.sample_fx_totals();
    m.window.reset();
    gpu.fx_layer_builds.raise_total(at_reset + 1);
    gpu.fx_payload_copies.raise_total(at_reset + 1);
    assert_eq!(
        (
            gpu.fx_layer_builds.since_reset(),
            gpu.fx_payload_copies.since_reset()
        ),
        (1, 1),
        "a stale sample moved the base"
    );

    let stale = aterm_core::render::fx_layer_builds();
    let plain = Baselined::new();
    plain.set_total(at_reset);
    plain.set_total(stale);
    plain.reset();
    plain.set_total(at_reset + 1);
    assert_eq!(plain.since_reset(), at_reset + 1 - stale);
}
