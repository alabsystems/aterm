// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Metrics, emit_present_glass, wire};
use aterm_gpu::present_glass::{GlassReport, GlassSample, PresentTag};

fn report(sample: GlassSample, tag: PresentTag, successor: PresentTag) -> GlassReport {
    GlassReport {
        sample,
        tag,
        successor,
        deck_late_ns: None,
    }
}

#[test]
fn every_ablation_mode_reads_back_as_itself() {
    use aterm_gpu::UnormAblation::{Drop, Off, Thin};
    for mode in [Off, Drop, Thin] {
        assert_eq!(super::ablation_of(super::ablation_code(mode)), Some(mode));
    }
    assert_eq!(super::ablation_of(3), None);
    let m = super::Metrics::new();
    m.window.gpu.unorm_ablation.set(3);
    assert_eq!(m.window.gpu.unorm_ablation_name(), "unknown");
}

#[test]
fn every_unorm_route_reads_back_as_itself() {
    use aterm_gpu::UnormRoute::{Fetch, Split};
    for route in [Split, Fetch] {
        assert_eq!(super::route_of(super::route_code(route)), Some(route));
    }
    assert_eq!(super::route_of(2), None);
    let m = super::Metrics::new();
    m.window.gpu.unorm_route.set(2);
    assert_eq!(m.window.gpu.unorm_route_name(), "unknown");
}

#[test]
fn every_fetch_verdict_publishes_its_own_name() {
    use aterm_gpu::{FetchProbe, FetchStage};
    let failed = FetchProbe::Failed {
        stage: FetchStage::InPass,
        channel: 1,
        want: 2,
        got: 3,
    };
    for verdict in [
        FetchProbe::Unsupported,
        FetchProbe::Passed,
        failed,
        FetchProbe::Refused,
    ] {
        assert_eq!(
            super::probe_name(u64::from(super::probe_code(Some(verdict)))),
            verdict.name()
        );
    }
    assert_eq!(
        super::probe_name(u64::from(super::probe_code(None))),
        "pending"
    );
    assert_eq!(super::probe_name(5), "unknown");
}

#[test]
fn glass_samples_book_on_glass_and_skipped_apart() {
    let m = Metrics::new();
    let ctx = wire::ReadCtx::inert(&m, 0);
    let text = wire::render_text(|s| emit_present_glass(&ctx, s));
    assert!(
        text.ends_with(" present_glass_shown_by=none present_glass_skipped_by=none"),
        "{text}"
    );
    let json = wire::render_json(|s| emit_present_glass(&ctx, s));
    assert!(
        json.ends_with(",\"present_glass_skipped_by\":{}}"),
        "{json}"
    );

    let glass = &m.window.glass;
    glass.note(report(
        GlassSample::OnGlass { ns: 7_000_000 },
        PresentTag::Spaced,
        PresentTag::Spaced,
    ));
    glass.note(report(
        GlassSample::Skipped,
        PresentTag::Spaced,
        PresentTag::HotBurst,
    ));
    assert_eq!(glass.latency.copy_out().n(), 1, "a skip is not a duration");
    assert_eq!(glass.latency.max(), 7_000_000);
    assert_eq!(glass.skipped.get(), 1);
    let text = wire::render_text(|s| emit_present_glass(&ctx, s));
    assert!(text.contains(" n_present_glass=1 "), "{text}");
    assert!(text.contains(" max_present_glass_ms=7.00 "), "{text}");
    assert!(text.contains(" present_glass_skipped=1 "), "{text}");
    assert!(text.contains(" present_glass_shown_by=spaced:1 "), "{text}");
    assert!(
        text.ends_with(" present_glass_skipped_by=hot_burst:1"),
        "{text}"
    );
    let json = wire::render_json(|s| emit_present_glass(&ctx, s));
    assert!(json.contains("\"n_present_glass\":1,"), "{json}");
    assert!(json.contains("\"present_glass_skipped\":1,"), "{json}");
    assert!(
        json.contains("\"present_glass_shown_by\":{\"spaced\":1}"),
        "{json}"
    );
    assert!(
        json.ends_with("\"present_glass_skipped_by\":{\"hot_burst\":1}}"),
        "{json}"
    );
}
