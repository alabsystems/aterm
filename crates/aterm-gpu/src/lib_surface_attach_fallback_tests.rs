// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SurfaceAttachFallback as F, surface_attach_fallback};

#[test]
fn rebuild_iff_visual_and_alone() {
    let rows = [
        (true, false, F::RebuildOpaque, "visual, first window"),
        (true, true, F::DeclineWindow, "visual, live GPU windows"),
        (false, true, F::DeclineWindow, "plain, live GPU windows"),
        (false, false, F::CpuRenderer, "plain, first window"),
    ];
    for (visual, others, want, why) in rows {
        let plan = surface_attach_fallback(visual, others);
        assert_eq!(plan, want, "{why}: visual={visual} others={others}");
        assert_eq!(
            plan == F::RebuildOpaque,
            visual && !others,
            "visual={visual} others={others} -> {plan:?}"
        );
        assert_eq!(plan == F::DeclineWindow, others, "others must dominate");
    }
}
